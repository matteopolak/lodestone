//! Proposal-backed event ordering and failure isolation.

use std::sync::{Arc, Mutex};

use bevy_app::{App, Plugin};
use lodestone_model::{ResourceKey, Vec3};
use lodestone_server::ecs::{
    GameTick, PaperEvent, PaperEventBus, PaperEventKind, PaperEventPriority,
    PaperEventRegistrationError, ServerApp, ServerProposalAction, ServerProposalQueue,
};

fn key(value: &str) -> ResourceKey {
    value.parse().expect("test resource key")
}

struct StagedSpawn {
    listeners: Arc<dyn Fn(&mut PaperEventBus) + Send + Sync>,
}

impl Plugin for StagedSpawn {
    fn build(&self, app: &mut App) {
        let mut events = app.world_mut().resource_mut::<PaperEventBus>();
        (self.listeners)(&mut events);
        app.world_mut().resource_mut::<ServerProposalQueue>().stage(
            ServerProposalAction::SpawnMob {
                entity_type: key("minecraft:pig"),
                pos: Vec3::new(1.0, 2.0, 3.0),
            },
        );
    }
}

#[test]
fn listeners_are_ordered_and_later_listeners_see_mutations_and_cancellation() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let first_seen = Arc::clone(&seen);
    let second_seen = Arc::clone(&seen);
    let monitor_seen = Arc::clone(&seen);
    let server = ServerApp::bootstrap_with(|app| {
        app.add_plugins(StagedSpawn {
            listeners: Arc::new(move |events| {
                let first_seen = Arc::clone(&first_seen);
                let second_seen = Arc::clone(&second_seen);
                let monitor_seen = Arc::clone(&monitor_seen);
                events
                    .register(
                        PaperEventKind::EntitySpawn,
                        PaperEventPriority::Low,
                        "move-spawn",
                        move |event| {
                            let PaperEvent::EntitySpawn { pos, .. } = event else {
                                unreachable!("event kind was filtered at registration");
                            };
                            assert_eq!(*pos, Vec3::new(1.0, 2.0, 3.0));
                            pos.x = 9.0;
                            first_seen.lock().expect("first listener lock").push("first");
                        },
                    )
                    .expect("supported event");
                events
                    .register(
                        PaperEventKind::EntitySpawn,
                        PaperEventPriority::High,
                        "cancel-spawn",
                        move |event| {
                            let PaperEvent::EntitySpawn { pos, .. } = event else {
                                unreachable!("event kind was filtered at registration");
                            };
                            assert_eq!(pos.x, 9.0);
                            event.cancel();
                            second_seen.lock().expect("second listener lock").push("second");
                        },
                    )
                    .expect("supported event");
                events
                    .register(
                        PaperEventKind::EntitySpawn,
                        PaperEventPriority::Monitor,
                        "observe-cancelled",
                        move |event| {
                            assert!(event.is_cancelled());
                            monitor_seen.lock().expect("monitor listener lock").push("monitor");
                        },
                    )
                    .expect("supported event");
            }),
        });
    });
    let mut world = server.into_world();
    world.run_schedule(GameTick);

    let outcome = world
        .resource_mut::<ServerProposalQueue>()
        .take_resolutions()
        .pop()
        .expect("staged proposal resolution")
        .outcome;
    assert!(matches!(outcome, Err(lodestone_server::ecs::ProposalRefusal::Denied)));
    assert_eq!(
        *seen.lock().expect("listener observations lock"),
        vec!["first", "second", "monitor"]
    );
}

#[test]
#[ignore = "Cranelift cannot unwind this nested test panic; run with the LLVM override"]
fn panicking_listener_isolated_and_later_listener_can_replace() {
    let server = ServerApp::bootstrap_with(|app| {
        app.add_plugins(StagedSpawn {
            listeners: Arc::new(|events| {
                events
                    .register(
                        PaperEventKind::EntitySpawn,
                        PaperEventPriority::Lowest,
                        "broken-listener",
                        |_| panic!("listener failure is contained"),
                    )
                    .expect("supported event");
                events
                    .register(
                        PaperEventKind::EntitySpawn,
                        PaperEventPriority::Normal,
                        "replace-after-panic",
                        |event| {
                            let PaperEvent::EntitySpawn { entity_type, .. } = event else {
                                unreachable!("event kind was filtered at registration");
                            };
                            *entity_type = key("minecraft:cow");
                        },
                    )
                    .expect("supported event");
            }),
        });
    });
    let mut world = server.into_world();
    world.run_schedule(GameTick);

    let outcome = world
        .resource_mut::<ServerProposalQueue>()
        .take_resolutions()
        .pop()
        .expect("staged proposal resolution")
        .outcome
        .expect("later listener replacement survives panic");
    assert_eq!(
        outcome,
        ServerProposalAction::SpawnMob {
            entity_type: key("minecraft:cow"),
            pos: Vec3::new(1.0, 2.0, 3.0),
        }
    );
    assert_eq!(
        world.resource_mut::<PaperEventBus>().take_failures(),
        vec![lodestone_server::ecs::PaperEventFailure {
            kind: PaperEventKind::EntitySpawn,
            listener: "broken-listener",
        }]
    );
}

#[test]
fn unsupported_event_registration_fails_explicitly() {
    let mut events = PaperEventBus::default();
    let error = events
        .register(
            PaperEventKind::PlayerInteract,
            PaperEventPriority::Normal,
            "unsupported",
            |_| {},
        )
        .expect_err("unsupported event must not look registered");
    assert_eq!(
        error,
        PaperEventRegistrationError::Unsupported(PaperEventKind::PlayerInteract)
    );
    assert!(!PaperEventKind::PlayerInteract.supported());
}
