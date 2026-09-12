//! Proposal-backed event ordering and failure isolation.

use std::sync::{Arc, Mutex};
use std::thread;

use bevy_app::{App, Plugin};
use lodestone_data::block_states::StateId;
use lodestone_model::{BlockFace, BlockPos, Hand, ResourceKey, Vec3};
use lodestone_server::ecs::{
    GameTick, PaperEvent, PaperEventBus, PaperEventFailureReason, PaperEventKind,
    PaperEventPriority, PaperEventRegistrationError, ServerApp, ServerProposalAction,
    ServerProposalQueue,
};
use uuid::Uuid;

fn key(value: &str) -> ResourceKey {
    value.parse().expect("test resource key")
}

fn state(value: &str) -> StateId {
    StateId::from_state_str(value).expect("test block state")
}

fn assert_census_status(kind: PaperEventKind) {
    match kind {
        PaperEventKind::EntitySpawn
        | PaperEventKind::EntityDespawn
        | PaperEventKind::ResidentBlockChange
        | PaperEventKind::PlayerInteract
        | PaperEventKind::BlockBreak => assert!(kind.supported()),
        PaperEventKind::InventoryClick => assert!(!kind.supported()),
    }
}

struct StagedSpawn {
    listeners: Arc<dyn Fn(&mut PaperEventBus) + Send + Sync>,
    action: Option<ServerProposalAction>,
}

impl Plugin for StagedSpawn {
    fn build(&self, app: &mut App) {
        let mut events = app.world_mut().resource_mut::<PaperEventBus>();
        (self.listeners)(&mut events);
        if let Some(action) = &self.action {
            app.world_mut()
                .resource_mut::<ServerProposalQueue>()
                .stage(action.clone());
        }
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
            action: Some(ServerProposalAction::SpawnMob {
                entity_type: key("minecraft:pig"),
                pos: Vec3::new(1.0, 2.0, 3.0),
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
            action: Some(ServerProposalAction::SpawnMob {
                entity_type: key("minecraft:pig"),
                pos: Vec3::new(1.0, 2.0, 3.0),
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
            reason: PaperEventFailureReason::Panic,
        }]
    );
}

#[test]
fn player_interact_runs_every_priority_and_refuses_monitor_mutation() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let server = ServerApp::bootstrap_with(|app| {
        app.add_plugins(StagedSpawn {
            listeners: Arc::new({
                let order = Arc::clone(&order);
                move |events| {
                    for (priority, name) in [
                        (PaperEventPriority::Lowest, "lowest"),
                        (PaperEventPriority::Low, "low"),
                        (PaperEventPriority::Normal, "normal"),
                        (PaperEventPriority::High, "high"),
                        (PaperEventPriority::Highest, "highest"),
                        (PaperEventPriority::Monitor, "monitor"),
                    ] {
                        let order = Arc::clone(&order);
                        events
                            .register(
                                PaperEventKind::PlayerInteract,
                                priority,
                                name,
                                move |event| {
                                    order.lock().expect("priority order lock").push(name);
                                    let PaperEvent::PlayerInteract { pos, .. } = event else {
                                        unreachable!("event kind was filtered at registration");
                                    };
                                    if priority == PaperEventPriority::High {
                                        pos.x = 8;
                                    } else if priority == PaperEventPriority::Monitor {
                                        assert_eq!(pos.x, 8);
                                        pos.x = 99;
                                    }
                                },
                            )
                            .expect("supported event");
                    }
                }
            }),
            action: Some(ServerProposalAction::PlayerInteract {
                pos: BlockPos::new(1, 64, 3),
                face: BlockFace::North,
                hand: Hand::Main,
                using_secondary_action: false,
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
        .expect("monitor mutation is refused, not the event");
    assert_eq!(
        outcome,
        ServerProposalAction::PlayerInteract {
            pos: BlockPos::new(8, 64, 3),
            face: BlockFace::North,
            hand: Hand::Main,
            using_secondary_action: false,
        }
    );
    assert_eq!(
        *order.lock().expect("priority order lock"),
        vec!["lowest", "low", "normal", "high", "highest", "monitor"]
    );
    assert_eq!(
        world.resource_mut::<PaperEventBus>().take_failures(),
        vec![lodestone_server::ecs::PaperEventFailure {
            kind: PaperEventKind::PlayerInteract,
            listener: "monitor",
            reason: PaperEventFailureReason::MonitorMutation,
        }]
    );
}

#[test]
fn block_break_listener_cancellation_reaches_the_proposal_result() {
    let target = BlockPos::new(4, 64, 6);
    let block = state("minecraft:stone");
    let breaker = Uuid::from_u128(0x729);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server = ServerApp::bootstrap_with(|app| {
        app.add_plugins(StagedSpawn {
            listeners: Arc::new({
                let seen = Arc::clone(&seen);
                move |events| {
                    let seen = Arc::clone(&seen);
                    events
                        .register(
                            PaperEventKind::BlockBreak,
                            PaperEventPriority::Normal,
                            "cancel-break",
                            move |event| {
                                let PaperEvent::BlockBreak {
                                    pos,
                                    state,
                                    breaker: event_breaker,
                                    ..
                                } = event
                                else {
                                    unreachable!("event kind was filtered at registration");
                                };
                                assert_eq!(*pos, target);
                                assert_eq!(*state, block);
                                assert_eq!(*event_breaker, breaker);
                                event.cancel();
                                seen.lock().expect("break listener lock").push("cancel");
                            },
                        )
                        .expect("block break is supported");

                    let seen = Arc::clone(&seen);
                    events
                        .register(
                            PaperEventKind::BlockBreak,
                            PaperEventPriority::Monitor,
                            "observe-break",
                            move |event| {
                                assert!(event.is_cancelled());
                                seen.lock().expect("break monitor lock").push("monitor");
                            },
                        )
                        .expect("block break is supported");
                }
            }),
            action: Some(ServerProposalAction::BlockBreak {
                pos: target,
                state: block,
                breaker,
            }),
        });
    });
    let mut world = server.into_world();
    world.run_schedule(GameTick);

    let outcome = world
        .resource_mut::<ServerProposalQueue>()
        .take_resolutions()
        .pop()
        .expect("staged block-break proposal resolution")
        .outcome;
    assert!(matches!(
        outcome,
        Err(lodestone_server::ecs::ProposalRefusal::Denied)
    ));
    assert_eq!(
        *seen.lock().expect("break listener observations lock"),
        vec!["cancel", "monitor"]
    );
}

#[test]
fn player_interact_handle_reaches_the_production_proposal_queue() {
    let server = ServerApp::bootstrap_with(|app| {
        app.add_plugins(StagedSpawn {
            listeners: Arc::new(|events| {
                events
                    .register(
                        PaperEventKind::PlayerInteract,
                        PaperEventPriority::Normal,
                        "deny-interaction",
                        |event| {
                            let PaperEvent::PlayerInteract { pos, .. } = event else {
                                unreachable!("event kind was filtered at registration");
                            };
                            assert_eq!(*pos, BlockPos::new(4, 65, 6));
                            event.cancel();
                        },
                    )
                    .expect("supported event");
            }),
            action: None,
        });
    });
    let handle = server.proposal_handle();
    let mut world = server.into_world();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let outcome = runtime.block_on(handle.player_interact(
            BlockPos::new(4, 65, 6),
            BlockFace::Up,
            Hand::Off,
            true,
        ));
        sender.send(outcome).expect("proposal result receiver");
    });
    let mut result = None;
    for _ in 0..128 {
        world.run_schedule(GameTick);
        if let Ok(outcome) = receiver.try_recv() {
            result = Some(outcome);
            break;
        }
        thread::yield_now();
    }
    worker.join().expect("proposal worker");
    assert!(matches!(
        result.or_else(|| receiver.try_recv().ok()).expect("proposal result"),
        Err(lodestone_server::ecs::ProposalRefusal::Denied)
    ));
}

#[test]
fn resident_block_change_later_listeners_see_mutation_before_cancellation() {
    let stone = state("minecraft:stone");
    let air = state("minecraft:air");
    let target = BlockPos::new(4, 64, 6);
    let seen = Arc::new(Mutex::new(Vec::new()));
    let server = ServerApp::bootstrap_with(|app| {
        app.add_plugins(StagedSpawn {
            listeners: Arc::new({
                let seen = Arc::clone(&seen);
                move |events| {
                    let replace_seen = Arc::clone(&seen);
                    events
                        .register(
                            PaperEventKind::ResidentBlockChange,
                            PaperEventPriority::Low,
                            "replace-block",
                            move |event| {
                                let PaperEvent::ResidentBlockChange { pos, state, .. } = event
                                else {
                                    unreachable!("event kind was filtered at registration");
                                };
                                assert_eq!(*pos, target);
                                assert_eq!(*state, stone);
                                *state = air;
                                replace_seen
                                    .lock()
                                    .expect("block listener lock")
                                    .push("replace");
                            },
                        )
                        .expect("resident block change is supported");

                    let cancel_seen = Arc::clone(&seen);
                    events
                        .register(
                            PaperEventKind::ResidentBlockChange,
                            PaperEventPriority::High,
                            "cancel-block",
                            move |event| {
                                let PaperEvent::ResidentBlockChange { pos, state, .. } = event
                                else {
                                    unreachable!("event kind was filtered at registration");
                                };
                                assert_eq!(*pos, target);
                                assert_eq!(*state, air);
                                event.cancel();
                                cancel_seen
                                    .lock()
                                    .expect("block listener lock")
                                    .push("cancel");
                            },
                        )
                        .expect("resident block change is supported");

                    let monitor_seen = Arc::clone(&seen);
                    events
                        .register(
                            PaperEventKind::ResidentBlockChange,
                            PaperEventPriority::Monitor,
                            "observe-block",
                            move |event| {
                                let PaperEvent::ResidentBlockChange { pos, state, .. } = event
                                else {
                                    unreachable!("event kind was filtered at registration");
                                };
                                assert_eq!(*pos, target);
                                assert_eq!(*state, air);
                                assert!(event.is_cancelled());
                                monitor_seen
                                    .lock()
                                    .expect("block monitor lock")
                                    .push("monitor");
                            },
                        )
                        .expect("resident block change is supported");
                }
            }),
            action: Some(ServerProposalAction::SetResidentBlock {
                pos: target,
                state: stone,
            }),
        });
    });
    let mut world = server.into_world();
    world.run_schedule(GameTick);

    let outcome = world
        .resource_mut::<ServerProposalQueue>()
        .take_resolutions()
        .pop()
        .expect("staged block proposal resolution")
        .outcome;
    assert!(matches!(
        outcome,
        Err(lodestone_server::ecs::ProposalRefusal::Denied)
    ));
    assert_eq!(
        *seen.lock().expect("block listener observations lock"),
        vec!["replace", "cancel", "monitor"]
    );
}

#[test]
fn entity_despawn_runs_every_priority_and_refuses_monitor_mutation() {
    let order = Arc::new(Mutex::new(Vec::new()));
    let server = ServerApp::bootstrap_with(|app| {
        app.add_plugins(StagedSpawn {
            listeners: Arc::new({
                let order = Arc::clone(&order);
                move |events| {
                    for (priority, name) in [
                        (PaperEventPriority::Lowest, "lowest"),
                        (PaperEventPriority::Low, "low"),
                        (PaperEventPriority::Normal, "normal"),
                        (PaperEventPriority::High, "high"),
                        (PaperEventPriority::Highest, "highest"),
                        (PaperEventPriority::Monitor, "monitor"),
                    ] {
                        let order = Arc::clone(&order);
                        events
                            .register(
                                PaperEventKind::EntityDespawn,
                                priority,
                                name,
                                move |event| {
                                    order.lock().expect("priority order lock").push(name);
                                    let PaperEvent::EntityDespawn { id, .. } = event else {
                                        unreachable!("event kind was filtered at registration");
                                    };
                                    if priority == PaperEventPriority::High {
                                        *id = 72;
                                    } else if priority == PaperEventPriority::Monitor {
                                        assert_eq!(*id, 72);
                                        *id = 99;
                                    }
                                },
                            )
                            .expect("supported event");
                    }
                }
            }),
            action: Some(ServerProposalAction::DespawnMob { id: 7 }),
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
        .expect("monitor mutation is refused, not the event");
    assert_eq!(outcome, ServerProposalAction::DespawnMob { id: 72 });
    assert_eq!(
        *order.lock().expect("priority order lock"),
        vec!["lowest", "low", "normal", "high", "highest", "monitor"]
    );
    assert_eq!(
        world.resource_mut::<PaperEventBus>().take_failures(),
        vec![lodestone_server::ecs::PaperEventFailure {
            kind: PaperEventKind::EntityDespawn,
            listener: "monitor",
            reason: PaperEventFailureReason::MonitorMutation,
        }]
    );
}

#[test]
fn entity_despawn_handle_reaches_the_production_proposal_queue() {
    let server = ServerApp::bootstrap_with(|app| {
        app.add_plugins(StagedSpawn {
            listeners: Arc::new(|events| {
                events
                    .register(
                        PaperEventKind::EntityDespawn,
                        PaperEventPriority::Normal,
                        "deny-despawn",
                        |event| {
                            let PaperEvent::EntityDespawn { id, .. } = event else {
                                unreachable!("event kind was filtered at registration");
                            };
                            assert_eq!(*id, 7);
                            event.cancel();
                        },
                    )
                    .expect("supported event");
            }),
            action: None,
        });
    });
    let handle = server.proposal_handle();
    let mut world = server.into_world();
    let (sender, receiver) = std::sync::mpsc::channel();
    let worker = thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        let outcome = runtime.block_on(handle.despawn_mob(7));
        sender.send(outcome).expect("proposal result receiver");
    });
    let mut result = None;
    for _ in 0..128 {
        world.run_schedule(GameTick);
        if let Ok(outcome) = receiver.try_recv() {
            result = Some(outcome);
            break;
        }
        thread::yield_now();
    }
    worker.join().expect("proposal worker");
    assert!(matches!(
        result.or_else(|| receiver.try_recv().ok()).expect("proposal result"),
        Err(lodestone_server::ecs::ProposalRefusal::Denied)
    ));
}

#[test]
fn unsupported_event_registration_fails_explicitly() {
    for kind in [
        PaperEventKind::EntitySpawn,
        PaperEventKind::EntityDespawn,
        PaperEventKind::ResidentBlockChange,
        PaperEventKind::PlayerInteract,
        PaperEventKind::InventoryClick,
    ] {
        assert_census_status(kind);
    }

    let mut events = PaperEventBus::default();
    let error = events
        .register(
            PaperEventKind::InventoryClick,
            PaperEventPriority::Normal,
            "unsupported",
            |_| {},
        )
        .expect_err("unsupported event must not look registered");
    assert_eq!(
        error,
        PaperEventRegistrationError::Unsupported(PaperEventKind::InventoryClick)
    );
    assert!(!PaperEventKind::InventoryClick.supported());
}
