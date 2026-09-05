//! A native plugin must reach the production selection-and-server-echo path,
//! not only leave SelectSlotIntent in an isolated World.

use lodestone::config::{Config, Mode};
use lodestone::sim::Sim;
use lodestone_ecs::{ActionQueue, GameTick, PendingPluginKeyEvents, PhysicalKey};
use lodestone_hotbar_lock::HotbarLockPlugin;
use lodestone_model::ClientAction;

fn test_config() -> Config {
    Config {
        mode: Mode::Headless,
        render_distance: 2,
        ..Config::default()
    }
}

fn press_lock(sim: &Sim) {
    let mut world = sim.ecs().write();
    world
        .resource_mut::<PendingPluginKeyEvents>()
        .0
        .push(lodestone_ecs::PluginKeyEvent {
            key: PhysicalKey::named("KeyH"),
            pressed: true,
        });
}

fn tick_and_take_actions(sim: &Sim) -> Vec<ClientAction> {
    let mut world = sim.ecs().write();
    world.run_schedule(GameTick);
    world.resource_mut::<ActionQueue>().0.drain(..).collect()
}

/// Production route: external native plugin -> `Sim::client_app` ->
/// `GameTick` -> `SelectSlotIntent` -> shell `drive_select_slot` ->
/// `ActionQueue::SetCarriedItem`.
///
/// The baseline proves merely registering a plugin changes no selection. The
/// release control then runs the public human selection path after disabling
/// the lock; another tick must not take it back.
#[test]
fn hotbar_lock_echoes_the_selected_slot_then_releases_human_control() {
    let (plugin, state) = HotbarLockPlugin::new(PhysicalKey::named("KeyH"), 6)
        .expect("slot 6 is a valid hotbar selection");
    let mut app = Sim::client_app();
    app.add_plugins(plugin);
    let mut sim = Sim::from_app(app, test_config());

    assert_eq!(sim.selected_slot(), 0, "baseline: spawn selects slot zero");
    assert!(
        tick_and_take_actions(&sim).is_empty(),
        "baseline: an unpressed plugin must not issue a carried-item echo"
    );
    assert_eq!(sim.selected_slot(), 0, "baseline: no press keeps the human slot");

    press_lock(&sim);
    // Deferred component insertion is allowed to become visible in this tick
    // or the next. Either way, the real selection consumer must produce one
    // and only one carried-item echo across the two-pass window.
    let mut actions = tick_and_take_actions(&sim);
    actions.extend(tick_and_take_actions(&sim));
    assert!(state.enabled(), "the press enables the lock");
    assert_eq!(sim.selected_slot(), 6, "the plugin must select its configured slot");
    assert_eq!(
        actions,
        vec![ClientAction::SetCarriedItem { slot: 6 }],
        "the native intent must reach the shell's real server-echo queue"
    );

    press_lock(&sim);
    assert!(tick_and_take_actions(&sim).is_empty());
    assert!(!state.enabled(), "the second press releases the lock");
    sim.select_slot(2);
    assert_eq!(sim.selected_slot(), 2, "human selection changes after release");
    assert!(
        tick_and_take_actions(&sim).is_empty(),
        "release control: the disabled plugin must not reclaim the human slot"
    );
    assert_eq!(
        sim.selected_slot(),
        2,
        "release control: the human-selected slot survives a later plugin tick"
    );
}
