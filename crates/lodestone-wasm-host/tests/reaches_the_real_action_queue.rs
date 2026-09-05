//! The gate that answers `CLAUDE.md`'s island rule for this crate: a
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

use std::sync::Arc;

use lodestone_ecs::events::GameEvent;
use lodestone_ecs::player::{
    ActionQueue, BreakIntent, BreakOutcome, BreakStatus, CollisionSource, Egress, LocalPlayer,
    PhysicsState, PlayerCollision, SelectedSlot,
};
use lodestone_ecs::{GameTick, app::App};
use lodestone_model::{ClientAction, ClientEvent, PlayerInput, Text};
use lodestone_physics::{Aabb, CollisionView, PlayerState, Vec3d};
use lodestone::{config::Config, sim::Sim};
use lodestone_wasm_host::{
    Capability, CapabilitySet, InventoryClickButton, InventoryClickIntent, InventoryClickMode,
    PendingWasmMenuClicks, PluginHost, WasmHostPlugin, WasmPlugins,
};

fn chat(text: &str) -> GameEvent {
    GameEvent(ClientEvent::Chat {
        text: Text::literal(text),
        kind: lodestone_model::event::ChatKind::Chat,
        sender: None,
        ack: None,
    })
}

fn responder_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ObserveChat, Capability::ActChat])
}

fn look_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ActLook])
}

fn look_host_policy() -> CapabilitySet {
    let mut policy = CapabilitySet::default_policy();
    policy.insert(Capability::ActLook);
    policy
}

fn movement_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ActMovement])
}

fn movement_host_policy() -> CapabilitySet {
    let mut policy = CapabilitySet::default_policy();
    policy.insert(Capability::ActMovement);
    policy
}

fn break_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([
        Capability::Log,
        Capability::ActChat,
        Capability::ActBreak,
        Capability::ObserveBreak,
    ])
}

fn break_host_policy() -> CapabilitySet {
    let mut policy = CapabilitySet::default_policy();
    policy.insert(Capability::ActBreak);
    policy.insert(Capability::ObserveBreak);
    policy
}

fn select_slot_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ActSelectSlot])
}

fn select_slot_host_policy() -> CapabilitySet {
    let mut policy = CapabilitySet::default_policy();
    policy.insert(Capability::ActSelectSlot);
    policy
}

fn inventory_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([
        Capability::Log,
        Capability::ObserveInventory,
        Capability::ActChat,
    ])
}

fn inventory_click_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ActInventoryClick])
}

fn inventory_click_host_policy() -> CapabilitySet {
    let mut policy = CapabilitySet::default_policy();
    policy.insert(Capability::ActInventoryClick);
    policy
}

fn inventory_quick_move_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ActInventoryQuickMove])
}

fn inventory_quick_move_host_policy() -> CapabilitySet {
    let mut policy = CapabilitySet::default_policy();
    policy.insert(Capability::ActInventoryQuickMove);
    policy
}

fn inventory_hotbar_swap_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ActInventoryHotbarSwap])
}

fn inventory_hotbar_swap_host_policy() -> CapabilitySet {
    let mut policy = CapabilitySet::default_policy();
    policy.insert(Capability::ActInventoryHotbarSwap);
    policy
}

fn inventory_drop_cursor_capabilities() -> CapabilitySet {
    CapabilitySet::from_iter([Capability::Log, Capability::ActInventoryDropCursor])
}

fn inventory_drop_cursor_host_policy() -> CapabilitySet {
    let mut policy = CapabilitySet::default_policy();
    policy.insert(Capability::ActInventoryDropCursor);
    policy
}

fn inventory_event() -> GameEvent {
    GameEvent(ClientEvent::InventorySlotChanged {
        slot: 4,
        item: Some(lodestone_model::ItemStack::new(
            "minecraft:gold_ingot".parse().expect("valid item key"),
            13,
        )),
    })
}

/// An intentionally empty but live collision view. `PlayerCollision::NoWorld`
/// freezes physics by contract, so the movement integration gate needs this
/// explicit control to prove that the normal physics system read the guest input.
#[derive(Debug)]
struct EmptyCollision;

impl CollisionView for EmptyCollision {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<Aabb>) {}
}

impl CollisionSource for EmptyCollision {
    fn with_view(&self, f: &mut dyn FnMut(&dyn CollisionView)) {
        f(self);
    }
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
    let mut host = PluginHost::new(look_host_policy()).expect("engine");
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

/// A separately-built guest claims the copied look intent, and the existing
/// local-player consumer commits it before the controller derives and queues this
/// tick's movement report. This is deliberately not a direct component unit test:
/// it proves the ABI boundary reaches the production physics/send chain.
#[test]
fn a_wasm_look_intent_reaches_the_existing_physics_and_send_consumers() {
    let wasm = support::build_example_plugin(&["look"]);
    let mut host = PluginHost::new(look_host_policy()).expect("engine");
    host.load_file("look-owner", &wasm, &look_capabilities())
        .expect("the look fixture must load");

    let mut app = lodestone_app::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    lodestone_app::spawn_session(&mut app, PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0));
    *app.world_mut().resource_mut::<Egress>() = Egress { in_world: true, live: true };
    app.world_mut().run_schedule(GameTick);

    let rotation = {
        let world = app.world_mut();
        let mut players = world.query_filtered::<&PhysicsState, bevy_ecs::query::With<LocalPlayer>>();
        let state = players.single(world).expect("one spawned local player");
        (state.0.yaw, state.0.pitch)
    };
    assert_eq!(rotation, (37.5, -12.0));
    let actions = action_queue(&app);
    assert!(
        actions.iter().any(|action| matches!(
            action,
            ClientAction::Move { rotation, .. }
                if rotation.yaw == 37.5 && rotation.pitch == -12.0
        )),
        "the normal controller sender must report the WASM-owned look in this tick's move action: {actions:?}"
    );
}

/// The host-side capability gate must reject the same guest output before it can
/// claim the local-player component. This control distinguishes a missing look
/// change from a guest that simply returned no action.
#[test]
fn a_wasm_look_intent_without_its_capability_is_refused_before_physics() {
    let wasm = support::build_example_plugin(&["look"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file("look-owner", &wasm, &CapabilitySet::from_iter([Capability::Log]))
        .expect("the guest remains valid when its data-flow grant is withheld");

    let mut app = lodestone_app::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    lodestone_app::spawn_session(&mut app, PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0));
    app.world_mut().run_schedule(GameTick);

    let rotation = {
        let world = app.world_mut();
        let mut players = world.query_filtered::<&PhysicsState, bevy_ecs::query::With<LocalPlayer>>();
        let state = players.single(world).expect("one spawned local player");
        (state.0.yaw, state.0.pitch)
    };
    assert_eq!(rotation, (0.0, 0.0));
    assert_eq!(app.world().resource::<WasmPlugins>().refused_actions(), 1);
}

/// A separately-built guest overrides copied input only after the ordinary
/// controller producer runs. The existing physics and controller egress then
/// consume it; this does not test a component in isolation or let the guest
/// manufacture a packet.
#[test]
fn a_wasm_movement_intent_reaches_the_existing_physics_and_egress_consumers() {
    let wasm = support::build_example_plugin(&["movement"]);
    let mut host = PluginHost::new(movement_host_policy()).expect("engine");
    host.load_file("movement-owner", &wasm, &movement_capabilities())
        .expect("the movement fixture must load");

    let mut app = lodestone_app::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    lodestone_app::spawn_session(&mut app, PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0));
    app.insert_resource(PlayerCollision::View(Arc::new(EmptyCollision)));
    *app.world_mut().resource_mut::<Egress>() = Egress { in_world: true, live: true };
    app.world_mut().run_schedule(GameTick);

    let velocity = {
        let world = app.world_mut();
        let mut players = world.query_filtered::<&PhysicsState, bevy_ecs::query::With<LocalPlayer>>();
        players.single(world).expect("one spawned local player").0.velocity
    };
    assert!(
        velocity.x != 0.0 && velocity.z != 0.0,
        "the normal physics step must consume the guest axes, got {velocity:?}"
    );
    let actions = action_queue(&app);
    assert!(
        actions.iter().any(|action| matches!(
            action,
            ClientAction::SetPlayerInput(PlayerInput {
                forward: true,
                backward: false,
                left: false,
                right: true,
                jump: true,
                shift: true,
                sprint: true,
            })
        )),
        "the normal controller egress must report the guest-owned input in this tick: {actions:?}"
    );
}

/// The guest is loaded with the same action capability but returns no movement
/// action. This negative control proves the conductor does not invent movement
/// merely because a guest has permission to drive it.
#[test]
fn control_a_guest_with_movement_capability_but_no_movement_action_stays_idle() {
    let wasm = support::build_example_plugin(&[]);
    let mut host = PluginHost::new(movement_host_policy()).expect("engine");
    host.load_file("chat-responder", &wasm, &movement_capabilities())
        .expect("the quiet fixture must load");

    let mut app = lodestone_app::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    lodestone_app::spawn_session(&mut app, PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0));
    *app.world_mut().resource_mut::<Egress>() = Egress { in_world: true, live: true };
    app.world_mut().run_schedule(GameTick);

    assert!(
        action_queue(&app)
            .iter()
            .any(|action| matches!(action, ClientAction::SetPlayerInput(PlayerInput::EMPTY))),
        "the normal controller's idle input must remain intact when the guest returns no action"
    );
    assert_eq!(app.world().resource::<WasmPlugins>().refused_actions(), 0);
}

/// The same compiled movement guest is denied before it can overwrite the
/// controller's intent. The empty input and refusal counter make this distinct
/// from a guest that simply chose not to return an action.
#[test]
fn a_wasm_movement_intent_without_its_capability_is_refused_before_physics() {
    let wasm = support::build_example_plugin(&["movement"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file("movement-owner", &wasm, &CapabilitySet::from_iter([Capability::Log]))
        .expect("the guest remains valid when its data-flow grant is withheld");

    let mut app = lodestone_app::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    lodestone_app::spawn_session(&mut app, PlayerState::at(Vec3d::new(0.5, 1.0, 0.5), 0.0));
    *app.world_mut().resource_mut::<Egress>() = Egress { in_world: true, live: true };
    app.world_mut().run_schedule(GameTick);

    assert!(
        action_queue(&app)
            .iter()
            .any(|action| matches!(action, ClientAction::SetPlayerInput(PlayerInput::EMPTY))),
        "without `act:movement` the guest must not replace the controller input"
    );
    assert_eq!(app.world().resource::<WasmPlugins>().refused_actions(), 1);
}

/// A separately compiled guest owns a persistent mining target. The composed
/// client reaches the real shell consumer, which rejects an absent-world target
/// through its normal ray validation;
/// the guest observes that finite state, releases the claim, and emits a chat
/// action. This proves the host did not bypass validation or manufacture a dig
/// packet just because a guest asked to break.
#[test]
fn a_wasm_break_lifecycle_reaches_the_shell_mining_consumer_and_returns_a_bounded_outcome() {
    let wasm = support::build_example_plugin(&["break"]);
    let mut host = PluginHost::new(break_host_policy()).expect("engine");
    host.load_file("break-owner", &wasm, &break_capabilities())
        .expect("the break fixture must load");

    let mut app = Sim::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    let sim = Sim::from_app(app, Config::default());

    {
        let mut world = sim.ecs().write();
        *world.resource_mut::<Egress>() = Egress { in_world: true, live: true };
        world.run_schedule(GameTick);
        let mut players = world.query_filtered::<(&BreakIntent, &BreakOutcome), bevy_ecs::query::With<LocalPlayer>>();
        let (intent, outcome) = players.single(&world).expect("the guest must install one mining claim");
        assert_eq!(intent.pos, lodestone_model::BlockPos::new(4, 64, 4));
        assert_eq!(
            outcome.0,
            BreakStatus::Rejected(lodestone_ecs::player::BreakRejection::UnreachableOrObstructed),
            "the shell must reject an absent-world target through its normal ray validation"
        );
    }

    {
        let mut world = sim.ecs().write();
        world.run_schedule(GameTick);
        let mut players = world.query_filtered::<(), bevy_ecs::query::With<BreakIntent>>();
        assert!(
            players.iter(&world).next().is_none(),
            "the guest's explicit abort must release the persistent ownership claim"
        );
        assert!(
            world
                .resource::<ActionQueue>()
                .0
                .iter()
                .any(|action| matches!(
                    action,
                    ClientAction::SendChat { text } if text == "break: status=rejected"
                )),
            "the changed shell outcome must reach the guest exactly as a bounded observation"
        );
        assert_eq!(world.resource::<WasmPlugins>().refused_actions(), 0);
    }
}

/// A separately-built guest selects one hotbar slot through the production
/// shell consumer. The assertion is both ends of the contract: the real
/// selected component changes and the server-facing carried-item echo joins the
/// normal action queue. No outcome event is expected because selection has no
/// legality vocabulary beyond the consumer's finite no-op gates.
#[test]
fn a_wasm_hotbar_intent_reaches_the_shell_selection_and_echo_consumers() {
    let wasm = support::build_example_plugin(&["select-slot"]);
    let mut host = PluginHost::new(select_slot_host_policy()).expect("engine");
    host.load_file("select-slot", &wasm, &select_slot_capabilities())
        .expect("the selection fixture must load");

    let mut app = Sim::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    let sim = Sim::from_app(app, Config::default());

    let actions = {
        let mut world = sim.ecs().write();
        world.run_schedule(GameTick);
        let mut players = world.query_filtered::<&SelectedSlot, bevy_ecs::query::With<LocalPlayer>>();
        assert_eq!(
            players.single(&world).expect("one local player").0,
            6,
            "the shell consumer must commit the guest's selection"
        );
        world.resource::<ActionQueue>().0.clone()
    };
    assert!(
        actions
            .iter()
            .any(|action| matches!(action, ClientAction::SetCarriedItem { slot: 6 })),
        "the shell consumer must echo the changed selection to the ordered server queue: {actions:?}"
    );
}

/// Capability denial happens before the guest can claim the selection intent;
/// this proves the normal hotbar state remains under human/shell control rather
/// than merely showing that a fixture happened to return no action.
#[test]
fn a_wasm_hotbar_intent_without_its_capability_is_refused_before_shell_selection() {
    let wasm = support::build_example_plugin(&["select-slot"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file("select-slot", &wasm, &CapabilitySet::from_iter([Capability::Log]))
        .expect("the guest remains valid when its data-flow grant is withheld");

    let mut app = Sim::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    let sim = Sim::from_app(app, Config::default());
    let mut world = sim.ecs().write();
    world.run_schedule(GameTick);

    let mut players = world.query_filtered::<&SelectedSlot, bevy_ecs::query::With<LocalPlayer>>();
    assert_eq!(players.single(&world).expect("one local player").0, 0);
    assert!(
        !world
            .resource::<ActionQueue>()
            .0
            .iter()
            .any(|action| matches!(action, ClientAction::SetCarriedItem { .. })),
        "a refused guest action must not enter the shell's carried-item echo path"
    );
    assert_eq!(world.resource::<WasmPlugins>().refused_actions(), 1);
}

/// The component model allows any `u8`, so the production consumer remains the
/// authority for the `0..=8` hotbar range. A malformed guest request is consumed
/// as the same finite no-op the native intent receives: no selection mutation and
/// no carried-item echo.
#[test]
fn a_wasm_out_of_range_hotbar_intent_is_a_shell_validated_no_op() {
    let wasm = support::build_example_plugin(&["select-slot-invalid"]);
    let mut host = PluginHost::new(select_slot_host_policy()).expect("engine");
    host.load_file("select-slot-invalid", &wasm, &select_slot_capabilities())
        .expect("the malformed selection fixture must still load");

    let mut app = Sim::client_app();
    app.add_plugins(WasmHostPlugin::new(host));
    let sim = Sim::from_app(app, Config::default());
    let mut world = sim.ecs().write();
    world.run_schedule(GameTick);

    let mut players = world.query_filtered::<&SelectedSlot, bevy_ecs::query::With<LocalPlayer>>();
    assert_eq!(players.single(&world).expect("one local player").0, 0);
    assert!(
        !world
            .resource::<ActionQueue>()
            .0
            .iter()
            .any(|action| matches!(action, ClientAction::SetCarriedItem { .. })),
        "an out-of-range slot must never be echoed to the server"
    );
    assert_eq!(world.resource::<WasmPlugins>().refused_actions(), 0);
}

/// A separately built guest observes a real client inventory-slot event through
/// the same `GameEvent(ClientEvent)` bus native plugins use. The item remains a
/// copied key/count value: no guest ever receives or can mutate `SessionMenus`.
#[test]
fn a_wasm_inventory_observer_receives_the_canonical_slot_change() {
    let wasm = support::build_example_plugin(&["inventory"]);
    let mut policy = CapabilitySet::default_policy();
    policy.insert(Capability::ObserveInventory);
    let mut host = PluginHost::new(policy).expect("engine");
    host.load_file("inventory", &wasm, &inventory_capabilities())
        .expect("the inventory fixture must load");

    let mut app = client_app_with_host(false);
    app.add_plugins(WasmHostPlugin::new(host));
    app.world_mut().write_message(inventory_event());
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        chat_actions(&app),
        vec![ClientAction::SendChat {
            text: "inventory: slot=4 item=minecraft:gold_ingotx13".to_owned(),
        }],
        "the separately-built guest must receive the canonical item identity and count"
    );
}

/// The default host policy deliberately withholds inventory observation. The
/// fixture is identical to the positive test and still runs, so no reply proves
/// the capability gate—not a missing guest, event bus, or client schedule.
#[test]
fn a_wasm_inventory_observer_is_default_denied() {
    let wasm = support::build_example_plugin(&["inventory"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file(
        "inventory",
        &wasm,
        &CapabilitySet::from_iter([Capability::Log, Capability::ActChat]),
    )
    .expect("a data-flow observation may be withheld after the guest loads");

    let mut app = client_app_with_host(false);
    app.add_plugins(WasmHostPlugin::new(host));
    app.world_mut().write_message(inventory_event());
    app.world_mut().run_schedule(GameTick);

    assert_eq!(chat_actions(&app), Vec::<ClientAction>::new());
    assert_eq!(app.world().resource::<WasmPlugins>().refused_actions(), 0);
}

/// A separately compiled guest reaches the shell handoff, but the handoff still
/// carries only the bounded input that the live menu predictor needs. The
/// shell's `bounded_clicks_reach_the_live_menu_predictor_and_invalid_slots_do_not`
/// test drives that resource shape into `ClientHandle`; this gate proves the
/// host composition and capability boundary that precede it.
#[test]
fn a_wasm_inventory_click_reaches_the_bounded_shell_handoff() {
    let wasm = support::build_example_plugin(&["inventory-click"]);
    let mut host = PluginHost::new(inventory_click_host_policy()).expect("engine");
    host.load_file("inventory-click", &wasm, &inventory_click_capabilities())
        .expect("the explicitly granted click fixture must load");

    let mut app = client_app_with_host(false);
    app.add_plugins(WasmHostPlugin::new(host));
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        app.world_mut()
            .resource_mut::<PendingWasmMenuClicks>()
            .take(),
        vec![InventoryClickIntent::Slot {
            slot: 36,
            mode: InventoryClickMode::Pickup(InventoryClickButton::Left),
        }],
        "the guest must reach the shell handoff as bounded copied input, not a packet"
    );
    assert_eq!(app.world().resource::<WasmPlugins>().refused_actions(), 0);
}

/// The guest's maximal ABI slot survives host lowering as copied data. The
/// shell-consumer control rejects this exact value against the live menu before
/// it can reach `ClientHandle::menu_click`.
#[test]
fn a_wasm_inventory_click_keeps_the_invalid_slot_bounded_until_shell_validation() {
    let wasm = support::build_example_plugin(&["inventory-click-invalid"]);
    let mut host = PluginHost::new(inventory_click_host_policy()).expect("engine");
    host.load_file(
        "inventory-click-invalid",
        &wasm,
        &inventory_click_capabilities(),
    )
    .expect("the explicitly granted invalid-click fixture must load");

    let mut app = client_app_with_host(false);
    app.add_plugins(WasmHostPlugin::new(host));
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        app.world_mut()
            .resource_mut::<PendingWasmMenuClicks>()
            .take(),
        vec![InventoryClickIntent::Slot {
            slot: u16::MAX,
            mode: InventoryClickMode::Pickup(InventoryClickButton::Right),
        }],
        "the host must preserve the ABI boundary and leave live menu range validation to the shell"
    );
}

/// The identical guest still emits its click, but default policy must stop it
/// before it can reach the shell handoff. The granted test above is the control:
/// it proves an empty handoff is a denial rather than an uncalled guest.
#[test]
fn a_wasm_inventory_click_is_default_denied_before_the_shell_handoff() {
    let wasm = support::build_example_plugin(&["inventory-click"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file("inventory-click", &wasm, &CapabilitySet::from_iter([Capability::Log]))
        .expect("a data-flow action may be withheld after the guest loads");

    let mut app = client_app_with_host(false);
    app.add_plugins(WasmHostPlugin::new(host));
    app.world_mut().run_schedule(GameTick);

    assert!(
        app.world_mut()
            .resource_mut::<PendingWasmMenuClicks>()
            .take()
            .is_empty(),
        "an ungranted click must not reach the shell handoff"
    );
    assert_eq!(app.world().resource::<WasmPlugins>().refused_actions(), 1);
}

/// A separately compiled guest reaches the same bounded handoff with only a
/// slot. Its separately granted capability keeps a pickup/place grant from
/// becoming permission to move an entire stack.
#[test]
fn a_wasm_inventory_quick_move_reaches_the_bounded_shell_handoff() {
    let wasm = support::build_example_plugin(&["inventory-quick-move"]);
    let mut host = PluginHost::new(inventory_quick_move_host_policy()).expect("engine");
    host.load_file(
        "inventory-quick-move",
        &wasm,
        &inventory_quick_move_capabilities(),
    )
    .expect("the explicitly granted quick-move fixture must load");

    let mut app = client_app_with_host(false);
    app.add_plugins(WasmHostPlugin::new(host));
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        app.world_mut()
            .resource_mut::<PendingWasmMenuClicks>()
            .take(),
        vec![InventoryClickIntent::Slot {
            slot: 36,
            mode: InventoryClickMode::QuickMove,
        }],
        "the guest must hand off only the quick-move slot, never a menu or packet"
    );
    assert_eq!(app.world().resource::<WasmPlugins>().refused_actions(), 0);
}

/// The maximal copied slot reaches shell validation unchanged, so the shell's
/// live-menu check—not a duplicate host-side menu cache—decides whether it can
/// enter prediction.
#[test]
fn a_wasm_inventory_quick_move_keeps_the_invalid_slot_bounded_until_shell_validation() {
    let wasm = support::build_example_plugin(&["inventory-quick-move-invalid"]);
    let mut host = PluginHost::new(inventory_quick_move_host_policy()).expect("engine");
    host.load_file(
        "inventory-quick-move-invalid",
        &wasm,
        &inventory_quick_move_capabilities(),
    )
    .expect("the explicitly granted invalid quick-move fixture must load");

    let mut app = client_app_with_host(false);
    app.add_plugins(WasmHostPlugin::new(host));
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        app.world_mut()
            .resource_mut::<PendingWasmMenuClicks>()
            .take(),
        vec![InventoryClickIntent::Slot {
            slot: u16::MAX,
            mode: InventoryClickMode::QuickMove,
        }]
    );
}

/// The granted integration control above proves this empty handoff means the
/// default-denied capability gate ran rather than that the guest was inert.
#[test]
fn a_wasm_inventory_quick_move_is_default_denied_before_the_shell_handoff() {
    let wasm = support::build_example_plugin(&["inventory-quick-move"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file(
        "inventory-quick-move",
        &wasm,
        &CapabilitySet::from_iter([Capability::Log]),
    )
    .expect("a data-flow action may be withheld after the guest loads");

    let mut app = client_app_with_host(false);
    app.add_plugins(WasmHostPlugin::new(host));
    app.world_mut().run_schedule(GameTick);

    assert!(
        app.world_mut()
            .resource_mut::<PendingWasmMenuClicks>()
            .take()
            .is_empty(),
        "an ungranted quick move must not reach the shell handoff"
    );
    assert_eq!(app.world().resource::<WasmPlugins>().refused_actions(), 1);
}

/// A separately compiled guest reaches the same bounded copied queue with a
/// number-key swap. Its dedicated grant does not widen pickup/place or quick
/// move authority.
#[test]
fn a_wasm_inventory_hotbar_swap_reaches_the_bounded_shell_handoff() {
    let wasm = support::build_example_plugin(&["inventory-hotbar-swap"]);
    let mut host = PluginHost::new(inventory_hotbar_swap_host_policy()).expect("engine");
    host.load_file(
        "inventory-hotbar-swap",
        &wasm,
        &inventory_hotbar_swap_capabilities(),
    )
    .expect("the explicitly granted hotbar-swap fixture must load");

    let mut app = client_app_with_host(false);
    app.add_plugins(WasmHostPlugin::new(host));
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        app.world_mut()
            .resource_mut::<PendingWasmMenuClicks>()
            .take(),
        vec![InventoryClickIntent::Slot {
            slot: 36,
            mode: InventoryClickMode::HotbarSwap(3),
        }],
        "the guest must hand off only copied swap data, never menu state or a packet"
    );
    assert_eq!(app.world().resource::<WasmPlugins>().refused_actions(), 0);
}

/// The maximal hotbar key stays copied until the shell's live validation gate;
/// the host must not narrow or reinterpret it as a different menu operation.
#[test]
fn a_wasm_inventory_hotbar_swap_keeps_an_invalid_key_bounded_until_shell_validation() {
    let wasm = support::build_example_plugin(&["inventory-hotbar-swap-invalid"]);
    let mut host = PluginHost::new(inventory_hotbar_swap_host_policy()).expect("engine");
    host.load_file(
        "inventory-hotbar-swap-invalid",
        &wasm,
        &inventory_hotbar_swap_capabilities(),
    )
    .expect("the explicitly granted invalid hotbar-swap fixture must load");

    let mut app = client_app_with_host(false);
    app.add_plugins(WasmHostPlugin::new(host));
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        app.world_mut()
            .resource_mut::<PendingWasmMenuClicks>()
            .take(),
        vec![InventoryClickIntent::Slot {
            slot: 36,
            mode: InventoryClickMode::HotbarSwap(9),
        }]
    );
}

/// The same guest action under the fail-closed policy is counted and never
/// reaches the bounded shell queue; the granted test above is its control.
#[test]
fn a_wasm_inventory_hotbar_swap_is_default_denied_before_the_shell_handoff() {
    let wasm = support::build_example_plugin(&["inventory-hotbar-swap"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file(
        "inventory-hotbar-swap",
        &wasm,
        &CapabilitySet::from_iter([Capability::Log]),
    )
    .expect("a data-flow action may be withheld after the guest loads");

    let mut app = client_app_with_host(false);
    app.add_plugins(WasmHostPlugin::new(host));
    app.world_mut().run_schedule(GameTick);

    assert!(
        app.world_mut()
            .resource_mut::<PendingWasmMenuClicks>()
            .take()
            .is_empty(),
        "an ungranted hotbar swap must not reach the shell handoff"
    );
    assert_eq!(app.world().resource::<WasmPlugins>().refused_actions(), 1);
}

/// The no-argument drop request crosses the same bounded handoff without
/// leaking cursor contents, an outside-slot sentinel, or a packet to the guest.
#[test]
fn a_wasm_inventory_cursor_drop_reaches_the_bounded_shell_handoff() {
    let wasm = support::build_example_plugin(&["inventory-drop-cursor"]);
    let mut host = PluginHost::new(inventory_drop_cursor_host_policy()).expect("engine");
    host.load_file(
        "inventory-drop-cursor",
        &wasm,
        &inventory_drop_cursor_capabilities(),
    )
    .expect("the explicitly granted cursor-drop fixture must load");

    let mut app = client_app_with_host(false);
    app.add_plugins(WasmHostPlugin::new(host));
    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        app.world_mut()
            .resource_mut::<PendingWasmMenuClicks>()
            .take(),
        vec![InventoryClickIntent::DropCursor],
        "the guest must hand off only a cursor-drop request, never cursor state or a packet"
    );
    assert_eq!(app.world().resource::<WasmPlugins>().refused_actions(), 0);
}

/// The granted integration control above proves that this empty handoff is the
/// fail-closed capability boundary rather than an inert separately built guest.
#[test]
fn a_wasm_inventory_cursor_drop_is_default_denied_before_the_shell_handoff() {
    let wasm = support::build_example_plugin(&["inventory-drop-cursor"]);
    let mut host = PluginHost::new(CapabilitySet::default_policy()).expect("engine");
    host.load_file(
        "inventory-drop-cursor",
        &wasm,
        &CapabilitySet::from_iter([Capability::Log]),
    )
    .expect("a data-flow action may be withheld after the guest loads");

    let mut app = client_app_with_host(false);
    app.add_plugins(WasmHostPlugin::new(host));
    app.world_mut().run_schedule(GameTick);

    assert!(
        app.world_mut()
            .resource_mut::<PendingWasmMenuClicks>()
            .take()
            .is_empty(),
        "an ungranted cursor drop must not reach the shell handoff"
    );
    assert_eq!(app.world().resource::<WasmPlugins>().refused_actions(), 1);
}
