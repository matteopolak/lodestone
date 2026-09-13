//! Emit one Lodestone-side observation for the Paper conformance scenario.
//!
//! This uses the production server proposal and Paper-shaped event path. The
//! current server producer is a resident block mutation rather than a player
//! packet, so this is an observation of that supported path, not a claim that
//! the Paper/JVM bridge can execute a plugin or a player `BlockBreakEvent`.

use lodestone_data::block_states::StateId;
use lodestone_model::BlockPos;
use lodestone_server::ecs::{
    PaperEvent, PaperEventBus, PaperEventKind, PaperEventPriority, ProposalRefusal,
    ServerApp, ServerProposalAction, ServerProposalQueue,
};

const TARGET: BlockPos = BlockPos::new(1, 64, 0);

fn state(value: &str) -> StateId {
    StateId::from_state_str(value).expect("conformance block state is present")
}

fn observe(with_listener: bool) -> &'static str {
    let stone = state("minecraft:stone");
    let air = state("minecraft:air");
    assert_ne!(stone, air, "the fixed target must change from stone to air");
    let mut server = ServerApp::bootstrap_with(|app| {
        if with_listener {
            app.world_mut()
                .resource_mut::<PaperEventBus>()
                .register(
                    PaperEventKind::ResidentBlockChange,
                    PaperEventPriority::Normal,
                    "block-break-cancel",
                    move |event| {
                        let PaperEvent::ResidentBlockChange { pos, state, .. } = event else {
                            unreachable!("event kind was filtered at registration");
                        };
                        if *pos == TARGET && *state == air {
                            event.cancel();
                        }
                    },
                )
                .expect("resident block change is proposal-backed");
        }
        app.world_mut()
            .resource_mut::<ServerProposalQueue>()
            .stage(ServerProposalAction::SetResidentBlock { pos: TARGET, state: air });
    });

    // The action represents breaking the fixed stone target into air. The
    // proposal path owns the adjudication window and apply decision.
    server.run_game_tick();
    let mut world = server.into_world();
    let outcome = world
        .resource_mut::<ServerProposalQueue>()
        .take_resolutions()
        .pop()
        .expect("conformance proposal resolution")
        .outcome;

    match (with_listener, outcome) {
        (true, Err(ProposalRefusal::Denied)) => "cancelled",
        (false, Ok(ServerProposalAction::SetResidentBlock { pos, state }))
            if pos == TARGET && state == air => "completed",
        (true, other) => panic!("listener-present result was not cancelled: {other:?}"),
        (false, other) => panic!("no-listener result was not completed: {other:?}"),
    }
}

fn main() {
    let listener_present = observe(true);
    let no_listener = observe(false);
    println!(
        "{{\"schema\":1,\"kind\":\"observation\",\"backend\":\"lodestone\",\"scenario\":\"block-break-cancel\",\"status\":\"complete\",\"evidence_kind\":\"external\",\"source\":\"lodestone-server-paper-event-path\",\"observations\":[{{\"id\":\"listener-present\",\"outcome\":\"{listener_present}\"}},{{\"id\":\"no-listener\",\"outcome\":\"{no_listener}\"}}]}}"
    );
}
