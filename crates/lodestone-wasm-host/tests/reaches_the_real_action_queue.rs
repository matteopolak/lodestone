//! Issue #173's gate, and the one that answers `CLAUDE.md`'s island rule: a
//! separately-built `.wasm` loaded from a file changes the state of the **real
//! client `App`**.
//!
//! # Why this composes `lodestone_app::client_app()` and not a `World` of its own
//!
//! `CLAUDE.md`'s *world* species of vacuous test is the one you cannot find by
//! reading the test: both recorded instances "were verified against the one scene
//! that structurally cannot exercise the change". A harness that built its own
//! `App`, added `CorePlugin` and called `run_schedule(GameTick)` would pass with the
//! conductor anchored in a set the shipped client never runs, or on a schedule
//! nothing drives. So the subject here is the composed client `App` itself — the
//! same function the shell calls (`docs/plugin-registration.md`) — and the assertion
//! is on `lodestone_ecs::player::ActionQueue`, the one sanctioned egress a native
//! plugin also writes.
//!
//! # The two controls, and what each rules out
//!
//! | control | rules out |
//! |---|---|
//! | the same `App` **without** `add_plugins(WasmHostPlugin)` | that the actions come from anywhere but the guest — this is the brief's "remove the host's registration call and watch the effect vanish" |
//! | the host loaded but the guest fed a chat it does not answer | that the conductor pushes unconditionally whenever it runs |

mod support;

use lodestone_ecs::events::GameEvent;
use lodestone_ecs::player::ActionQueue;
use lodestone_ecs::{GameTick, app::App};
use lodestone_model::{ClientAction, ClientEvent, Text};
use lodestone_physics::{PlayerState, Vec3d};
use lodestone_wasm_host::{Capability, CapabilitySet, PluginHost, WasmHostPlugin, WasmPlugins};

fn chat(text: &str) -> GameEvent {
    GameEvent(ClientEvent::Chat {
        text: Text::literal(text),
        kind: lodestone_model::event::ChatKind::Chat,
        ack: None,
    })
}

fn responder_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ObserveChat, Capability::ActChat])
}

/// A composed client `App`, optionally with the wasm tier installed.
fn client_app_with_host(install: bool) -> App {
    let mut app = lodestone_app::client_app();
    if install {
        let wasm = support::build_example_plugin(&[]);
        let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
        host.load_file("chat-responder", &wasm, &responder_capabilities())
            .expect("the example plugin must load");
        app.add_plugins(WasmHostPlugin::new(host));
    }
    // After `add_plugins`, for the same reason `Sim` does it in that order: a plugin
    // may install a resource a spawn hook reads.
    lodestone_app::spawn_session(&mut app, PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0));
    app
}

fn action_queue(app: &App) -> Vec<ClientAction> {
    app.world().resource::<ActionQueue>().0.clone()
}

/// Only the actions the wasm tier could have produced. The native `ControllerPlugin`
/// legitimately pushes `Move`/`KeepAliveResponse` into the same queue, and a test
/// that asserted the whole queue would be asserting their behaviour too.
fn chat_actions(app: &App) -> Vec<ClientAction> {
    action_queue(app)
        .into_iter()
        .filter(|a| matches!(a, ClientAction::SendChat { .. } | ClientAction::SendCommand { .. }))
        .collect()
}

#[test]
fn a_runtime_loaded_wasm_plugin_pushes_a_real_client_action_onto_the_real_queue() {
    let mut app = client_app_with_host(true);

    app.world_mut().write_message(chat("hello ping there"));
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        chat_actions(&app),
        vec![ClientAction::SendChat {
            // Authored in the guest crate — a separate compilation unit for a
            // different target triple that this crate does not link — so neither the
            // string nor the count is something the host could have produced on its
            // own.
            text: "pong (chat messages seen: 1)".to_owned(),
        }],
        "the guest's action must reach `ActionQueue` through the conductor"
    );
    assert_eq!(
        app.world().resource::<WasmPlugins>().refused_actions(),
        0,
        "nothing should have been refused: the plugin holds `act:chat`"
    );
}

/// **CONTROL 1 — the registration call removed.** Byte-identical `App` composition
/// and event, minus `add_plugins(WasmHostPlugin)`. If this produced a chat action,
/// the test above would be measuring something else entirely.
#[test]
fn control_without_the_host_plugin_no_chat_action_appears() {
    let mut app = client_app_with_host(false);

    app.world_mut().write_message(chat("hello ping there"));
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        chat_actions(&app),
        Vec::<ClientAction>::new(),
        "with no wasm host registered, nothing may push a chat action"
    );
    assert!(
        app.world().get_resource::<WasmPlugins>().is_none(),
        "control: the host resource must be absent"
    );
}

/// **CONTROL 2 — the conductor runs but the guest declines.** Proves the push is a
/// decision the *guest* made about its input, not something the conductor does every
/// time it is scheduled.
#[test]
fn control_a_chat_the_guest_does_not_answer_produces_no_action() {
    let mut app = client_app_with_host(true);

    app.world_mut().write_message(chat("nothing to see here"));
    app.world_mut().run_schedule(GameTick);
    assert_eq!(chat_actions(&app), Vec::<ClientAction>::new());

    // And the conductor really did run and really did deliver that event: the
    // counter in the next reply is 2, not 1.
    app.world_mut().write_message(chat("ping"));
    app.world_mut().run_schedule(GameTick);
    assert_eq!(
        chat_actions(&app),
        vec![ClientAction::SendChat {
            text: "pong (chat messages seen: 2)".to_owned(),
        }],
        "the non-matching message must have been delivered and counted, not dropped \
         before the guest saw it"
    );
}

/// The conductor is anchored somewhere the shipped `GameTick` actually runs, and it
/// reads the bus *before* `age_game_event_bus` trims it.
///
/// This is the assertion that would have caught anchoring the conductor in
/// `TickSet::Send` alongside the (private, unorderable) ager: it writes an event and
/// runs the schedule repeatedly, and every tick's event must be seen. An ordering
/// coin flip shows up here as a count that lags.
#[test]
fn every_ticks_events_reach_the_guest_not_every_other_ticks() {
    let mut app = client_app_with_host(true);

    let mut replies = Vec::new();
    for _ in 0..4 {
        app.world_mut().write_message(chat("ping"));
        app.world_mut().run_schedule(GameTick);
        replies = chat_actions(&app);
    }
    assert_eq!(
        replies,
        vec![
            ClientAction::SendChat { text: "pong (chat messages seen: 1)".to_owned() },
            ClientAction::SendChat { text: "pong (chat messages seen: 2)".to_owned() },
            ClientAction::SendChat { text: "pong (chat messages seen: 3)".to_owned() },
            ClientAction::SendChat { text: "pong (chat messages seen: 4)".to_owned() },
        ],
        "four ticks, four events, four replies — a gap means the conductor is racing \
         the bus ager"
    );
}

/// The act-side capability filter, end to end through the real `App`: a guest whose
/// grant is withdrawn produces nothing, and the refusal is **counted** rather than
/// silently dropped.
///
/// Note what makes this a real test rather than a restatement of `abi`'s unit test:
/// the guest is identical and still *returns* the action — the filter is what stops
/// it, and `refused_actions()` is the evidence it fired.
#[test]
fn an_action_the_guest_was_not_granted_is_refused_and_counted() {
    let wasm = support::build_example_plugin(&[]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    // Observe, but do not act. The guest does not know that and replies anyway.
    host.load_file(
        "chat-responder",
        &wasm,
        &CapabilitySet::from_iter([Capability::Log, Capability::ObserveChat]),
    )
    .expect("load");

    let mut app = lodestone_app::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    lodestone_app::spawn_session(&mut app, PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0));

    app.world_mut().write_message(chat("ping"));
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        chat_actions(&app),
        Vec::<ClientAction>::new(),
        "without `act:chat` the guest's reply must not reach the queue"
    );
    assert_eq!(
        app.world().resource::<WasmPlugins>().refused_actions(),
        1,
        "the refusal must be counted — a silent drop is the failure this counter exists to \
         make visible"
    );
}
