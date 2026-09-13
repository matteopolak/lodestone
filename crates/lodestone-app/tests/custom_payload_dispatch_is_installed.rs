//! An island gate: the *production* plugin set must install the
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

use bevy_ecs::message::Message;
use lodestone_ecs::brand::{ReportedServerBrand, ServerBrandPayload};
use lodestone_ecs::events::GameEventBus;
use lodestone_ecs::player::ActionQueue;
use lodestone_ecs::plugin_channel::{
    OutboundPluginChannel, OutboundPluginChannelLimits, OutboundPluginChannelState,
    PluginChannelAppExt, PluginChannelState,
};
use lodestone_ecs::GameTick;
use lodestone_model::ClientAction;

#[derive(Message, Debug, Clone, PartialEq, Eq)]
struct ProductionProbe(Vec<u8>);

impl OutboundPluginChannel for ProductionProbe {
    const CHANNEL: &'static str = "example:production-probe";

    fn encode(&self) -> Vec<u8> {
        self.0.clone()
    }
}

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

/// The production composition must consume the bounded typed-channel path, not
/// merely expose it in a unit-test-only `App`. The resulting value is the
/// normal `ClientAction` consumed by the shell's action-queue drain; no socket,
/// fake channel, or raw packet constructor is involved here.
#[test]
fn client_app_connects_bounded_typed_send_to_the_action_queue() {
    let mut app = lodestone_app::client_app();
    app.add_outbound_plugin_channel_with_limits::<ProductionProbe>(
        OutboundPluginChannelLimits {
            max_messages_per_tick: 1,
            max_payload_bytes_per_tick: 3,
            max_payload_bytes_per_message: 3,
        },
    );
    app.world_mut()
        .write_message(ProductionProbe(b"one".to_vec()));
    app.world_mut()
        .write_message(ProductionProbe(b"drop".to_vec()));
    app.world_mut().run_schedule(GameTick);

    let queue = &app.world().resource::<ActionQueue>().0;
    assert_eq!(queue.len(), 1);
    assert_eq!(
        queue[0],
        ClientAction::SendCustomPayload {
            channel: "example:production-probe".parse().unwrap(),
            data: b"one".to_vec(),
        }
    );
    let stats = app
        .world()
        .resource::<OutboundPluginChannelState<ProductionProbe>>()
        .stats();
    assert_eq!(stats.queued_messages, 1);
    assert_eq!(stats.dropped_messages, 1);
    assert_eq!(stats.dropped_payload_bytes, 4);
}
