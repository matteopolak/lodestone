//! Issue #301's island gate: the *production* plugin set must install the
//! `custom_payload` dispatch, and with it the `GameEvent` bus it rides on.
//!
//! # Why this test and not a `plugin_channel` unit test
//!
//! `crates/lodestone-ecs/src/plugin_channel.rs` has a thorough test module and
//! every one of its tests builds its own `App` and calls `add_plugin_channel`
//! itself. That is a closed loop: the whole suite was green while **no
//! production `App` anywhere in the workspace registered a single channel**, so
//! the dispatch never ran outside a test and `SharedState`'s cached
//! `game_event_bus_enabled` was `false` in the shipped client — meaning
//! `push_to_game_event_bus` was skipped for *every* `ClientEvent`, not just for
//! `CustomPayload`.
//!
//! This test's subject is therefore [`lodestone_app::client_app`] — the thing
//! production actually calls — and it asserts state that only a real `build`
//! inserts.
//!
//! # The negative control
//!
//! [`a_bare_app_has_neither`] builds an `App` with only `CorePlugin` and
//! requires both assertions to **fail** there. Without it, a
//! `get_resource(...).is_some()` pair that happened to be true for some
//! unrelated reason would pass forever.

use lodestone_ecs::brand::{ReportedServerBrand, ServerBrandPayload};
use lodestone_ecs::events::GameEventBus;
use lodestone_ecs::plugin_channel::PluginChannelState;

/// The positive assertion: `client_app()` carries a live channel dispatch.
#[test]
fn client_app_installs_the_custom_payload_dispatch_and_the_bus() {
    let app = lodestone_app::client_app();
    let world = app.world();

    assert!(
        world
            .get_resource::<PluginChannelState<ServerBrandPayload>>()
            .is_some(),
        "client_app() registered no plugin channel — the dispatch is an island again"
    );
    assert_eq!(
        world
            .get_resource::<PluginChannelState<ServerBrandPayload>>()
            .map(|state| state.key().to_string()),
        Some("minecraft:brand".to_owned()),
        "the registered channel is not minecraft:brand"
    );
    assert!(
        world.get_resource::<ReportedServerBrand>().is_some(),
        "the channel's own fold target is missing, so a matched payload would go nowhere"
    );
    assert!(
        world.get_resource::<GameEventBus>().is_some(),
        "the GameEvent bus is absent, so SharedState will cache \
         game_event_bus_enabled = false and no ClientEvent reaches any plugin"
    );
}

/// The control. Both resources must be absent from an `App` that only has the
/// core plugin, or the assertions above measure nothing.
#[test]
fn a_bare_app_has_neither() {
    let mut app = lodestone_ecs::app::App::new();
    app.add_plugins(lodestone_ecs::CorePlugin);
    let world = app.world();

    assert!(
        world
            .get_resource::<PluginChannelState<ServerBrandPayload>>()
            .is_none(),
        "control premise false: CorePlugin alone already installs a channel state"
    );
    assert!(
        world.get_resource::<GameEventBus>().is_none(),
        "control premise false: CorePlugin alone already installs the bus, \
         so the positive test above proves nothing about client_app()"
    );
}
