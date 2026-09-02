//! Issue #301's end-to-end gate: **real wire bytes reach a real plugin's
//! resource.**
//!
//! # Why this file is shaped the way it is
//!
//! Three properties, each of which a cheaper test would have silently lacked.
//!
//! **1. It is an integration test in a plugin crate, so it consumes the plugin
//! API exactly as a third party does.** It registers
//! [`ServerBrandPlugin`] with `add_plugins` and then never mentions
//! `PluginChannel`, `add_plugin_channel`, `PluginChannelState`, or
//! `dispatch_plugin_channel` again. A test that reached into the registry and
//! called the fold directly would pass against a seam no plugin can actually
//! use — the shape issue #467 hid behind.
//!
//! **2. The bytes are not ours.** `FIXTURE` is hand-derived from the 26.2 wire
//! specification (see the file's own provenance header) and is decoded by the
//! real [`V770Adapter`]. Nothing in this crate or this file encodes a
//! `custom_payload`, so `decode(encode(x)) == x` is not available as a way to
//! pass.
//!
//! **3. It asserts the resource, not the plugin.**
//! `App::is_plugin_added::<ServerBrandPlugin>()` stays true for a `build` that
//! stopped inserting [`ReportedServerBrand`] or stopped registering the channel.
//! Every assertion here is about folded data.
//!
//! # The one hop this file does not itself drive, stated rather than implied
//!
//! The step from `ClientEvent` to `Messages<GameEvent>` belongs to
//! `lodestone_client::state::SharedState::apply`'s `push_to_game_event_bus`,
//! which this crate cannot call (`apply` is `pub(crate)`, and depending on
//! `lodestone-client` from a plugin would invert the plugin boundary). This file
//! writes the `GameEvent` directly, and that hop is covered elsewhere by two
//! facts rather than being assumed:
//!
//! * `push_to_game_event_bus` **does not match on the event at all** — it is one
//!   `world.write_message(GameEvent(event.clone()))` — so it cannot have a
//!   missing arm for `CustomPayload` specifically. A source-scanning guard,
//!   `lodestone_client::state::tests::game_event_bus_write_site_has_no_match_on_the_event`,
//!   keeps it that way.
//! * It only runs when the [`GameEventBus`](lodestone_ecs::GameEventBus) marker
//!   is present, and
//!   `lodestone_ecs::plugin_channel::tests::channel_dispatch_requires_no_second_opt_in`
//!   gates that `add_plugin_channel` installs it. That is the link that would
//!   otherwise have made this whole path an island: a plugin author who
//!   registered a channel and nothing else would receive nothing forever.

use lodestone_ecs::app::App;
use lodestone_ecs::{GameEvent, GameTick};
use lodestone_model::{ClientEvent, ConnectionState, Directive, VersionAdapter};
use lodestone_server_brand::{ReportedServerBrand, ServerBrandPlugin};
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;

/// The committed capture. See the file itself for provenance and layout.
const FIXTURE: &str = include_str!("support/custom_payload_brand_vanilla.hex");

/// Parses the whitespace-and-`#`-comment hex format the fixture is written in.
///
/// # Panics
///
/// On any token that is not a hex byte pair — a malformed fixture must fail
/// loudly, never silently decode to a shorter buffer that then "reasonably"
/// fails to parse as a brand payload.
fn hex_bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for line in text.lines() {
        let line = line.split('#').next().unwrap_or("");
        for token in line.split_whitespace() {
            out.push(
                u8::from_str_radix(token, 16)
                    .unwrap_or_else(|_| panic!("fixture token {token:?} is not a hex byte")),
            );
        }
    }
    assert!(!out.is_empty(), "fixture parsed to zero bytes");
    out
}

/// Runs `body` through the real `v770` adapter as a clientbound
/// `custom_payload` and returns the single event it emits.
fn decode_custom_payload(body: &[u8]) -> ClientEvent {
    let adapter = V770Adapter::new();
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::CUSTOM_PAYLOAD,
            body,
        )
        .expect("the fixture must decode as a clientbound custom_payload");
    match directives.as_slice() {
        [Directive::Emit(event)] => event.clone(),
        other => panic!("expected exactly one emitted event, got {other:?}"),
    }
}

/// An app composed the way a third party composes one: add the plugin, tick.
fn app_with_plugin() -> App {
    let mut app = App::new();
    app.add_plugins(ServerBrandPlugin);
    app
}

/// A `custom_payload` body for `channel` carrying `payload` bytes verbatim,
/// built with an independent length-prefixer — used only to build the *negative
/// control's* foreign-channel frame, never the subject.
fn body_for(channel: &str, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let len = u8::try_from(channel.len()).expect("short test channel");
    assert!(len < 0x80, "single-byte VarInt only");
    out.push(len);
    out.extend_from_slice(channel.as_bytes());
    out.extend_from_slice(payload);
    out
}

/// The fixture really is what its header says it is — checked before anything
/// is concluded from it, so a fixture that rotted into some other packet cannot
/// quietly become the thing under test.
#[test]
fn the_fixture_decodes_to_a_brand_payload_on_the_brand_channel() {
    let event = decode_custom_payload(&hex_bytes(FIXTURE));
    let ClientEvent::CustomPayload { channel, data } = event else {
        panic!("expected CustomPayload, got {event:?}");
    };
    assert_eq!(channel.to_string(), "minecraft:brand");
    // The payload is the *undecoded* vanilla string-read body: length byte then bytes.
    assert_eq!(data, b"\x07vanilla");
}

/// **The headline.** Real bytes in, a plugin's own resource out, in one tick,
/// with no plugin-API type named beyond the plugin itself.
#[test]
fn a_real_brand_payload_reaches_the_plugins_resource() {
    let event = decode_custom_payload(&hex_bytes(FIXTURE));

    let mut app = app_with_plugin();
    app.world_mut().write_message(GameEvent(event));
    app.world_mut().run_schedule(GameTick);

    let reported = app.world().resource::<ReportedServerBrand>();
    assert_eq!(
        reported.brand.as_deref(),
        Some("vanilla"),
        "the decoded brand must reach the plugin's resource"
    );
    assert_eq!(reported.announcements, 1);
}

/// **Control 1.** A payload on a channel nobody registered must reach nothing.
/// Without this, the headline is satisfied by a fold that forwards every
/// `custom_payload` regardless of channel — which would be the generic
/// `ClientEvent` the issue is about, not a registry.
#[test]
fn a_payload_on_a_foreign_channel_reaches_nothing() {
    let event = decode_custom_payload(&body_for("example:other", b"\x07vanilla"));

    let mut app = app_with_plugin();
    app.world_mut().write_message(GameEvent(event));
    app.world_mut().run_schedule(GameTick);

    let reported = app.world().resource::<ReportedServerBrand>();
    assert_eq!(reported.brand, None);
    assert_eq!(reported.announcements, 0);
}

/// **Control 2.** The resource exists *because* the plugin was added. An app
/// without it has no `ReportedServerBrand` at all, so the headline's
/// `resource::<ReportedServerBrand>()` is reading something the plugin really
/// installed rather than something the `App` has by default.
#[test]
fn without_the_plugin_the_resource_does_not_exist() {
    let app = App::new();
    assert!(app.world().get_resource::<ReportedServerBrand>().is_none());
}

/// **Control 3.** A malformed payload on the *right* channel must not reach the
/// resource either — so the headline is not passing merely because the channel
/// matched. Pairs with the plugin's own decoder controls.
#[test]
fn a_malformed_payload_on_the_brand_channel_does_not_reach_the_resource() {
    // States length 9 but carries 7 bytes: `ServerBrand::decode` returns `None`.
    let event = decode_custom_payload(&body_for("minecraft:brand", b"\x09vanilla"));

    let mut app = app_with_plugin();
    app.world_mut().write_message(GameEvent(event));
    app.world_mut().run_schedule(GameTick);

    let reported = app.world().resource::<ReportedServerBrand>();
    assert_eq!(reported.brand, None);
    assert_eq!(reported.announcements, 0);
}

/// A second announcement updates the resource and counts — a server may send
/// `minecraft:brand` in Configuration and again in Play, so the fold must not
/// be a latch.
#[test]
fn a_second_announcement_is_folded_too() {
    let event = decode_custom_payload(&hex_bytes(FIXTURE));
    let modded = decode_custom_payload(&body_for("minecraft:brand", b"\x05Paper"));

    let mut app = app_with_plugin();
    app.world_mut().write_message(GameEvent(event));
    app.world_mut().run_schedule(GameTick);
    app.world_mut().write_message(GameEvent(modded));
    app.world_mut().run_schedule(GameTick);

    let reported = app.world().resource::<ReportedServerBrand>();
    assert_eq!(reported.brand.as_deref(), Some("Paper"));
    assert_eq!(reported.announcements, 2);
}
