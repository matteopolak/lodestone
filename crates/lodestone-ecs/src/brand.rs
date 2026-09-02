//! The built-in `minecraft:brand` channel, installed in the
//! *production* plugin set.
//!
//! # What it is
//!
//! One [`crate::plugin_channel::PluginChannel`] implementation that ships inside
//! `lodestone-ecs` and is added by [`lodestone_app::client_app`] rather than by a
//! third-party plugin, so the whole `custom_payload` dispatch chain is live in the
//! shipped client instead of only in tests.
//!
//! # Why this exists when `crates/plugins/lodestone-server-brand` already does
//!
//! It decodes the same payload into an almost identical type, and that
//! duplication is deliberate. `lodestone-server-brand` is the *worked example* a
//! plugin author reads: it lives outside the tree's core crates precisely to
//! prove that a third party needs no privileged access. Installing it in
//! production would mean adding it as a dependency of `lodestone-app`, whose
//! manifest is guarded by a closed allowlist
//! (`crates/lodestone-app/tests/renderer_free_graph.rs`) that exists to keep a
//! headless consumer's dependency graph small — and the shell's own plugin tuple
//! is owned by a different cluster. A ~20-line decoder inside a crate
//! `lodestone-app` already depends on costs less than either.
//!
//! **The two must stay in agreement about the wire, and nothing enforces that**
//! because they are independent transcriptions of the same one-line vanilla
//! codec. That is the accepted cost; if `BrandPayload` ever grows a second field,
//! fix both.
//!
//! # The island this closes, which was two links long
//!
//! Before this module, `crates/lodestone-ecs/src/plugin_channel.rs` was
//! individually built, individually tested, and reached zero consumers, because
//! `add_plugin_channel` had no production call site. That hid a *second* island
//! behind it: [`crate::events::GameEventBusPlugin`] is opt-in and
//! `lodestone_client::SharedState` caches `game_event_bus_enabled` **once, at
//! construction**, so with no `PluginChannelPlugin` anywhere in the shipped
//! `App` the bus resource was absent and `push_to_game_event_bus` ran for **no
//! `ClientEvent` at all**. Installing this plugin fixes both at once:
//! `PluginChannelPlugin::build` adds the bus itself.
//!
//! So the honest description of what changed is not "brand decoding works" — it
//! is that the client now has a live `GameEvent` bus and a live per-channel
//! `custom_payload` dispatch, with this channel as the first thing riding them.
//!
//! # How to change it
//!
//! To add another built-in channel, put the type here, implement
//! `PluginChannel`, and call `add_plugin_channel` from
//! [`ServerBrandChannelPlugin::build`] — or give it its own plugin and add that
//! to `lodestone_app::client_app`. Do **not** reach for `ChannelRegistry`
//! (`lodestone-client`): that is a passive fold an *embedder* drives by hand and
//! has no call site inside the client, which is why it was the wrong half to
//! build on.
//!
//! # Configuration
//!
//! None. The channel name is a compile-time constant and a malformed one is a
//! startup panic by [`crate::plugin_channel`]'s contract.
//!
//! # Dependencies
//!
//! [`crate::plugin_channel`] for the dispatch and the bus,
//! [`crate::events::GameEvent`] for the stream it folds.

use bevy_app::{App, Plugin};
use bevy_ecs::message::MessageReader;
use bevy_ecs::prelude::{IntoScheduleConfigs, Message, ResMut, Resource};

use crate::plugin_channel::{PluginChannel, PluginChannelAppExt};
use crate::schedules::GameTick;
use crate::sets::EventPriority;

/// One decoded `minecraft:brand` payload: the brand string the server reports
/// for itself — `"vanilla"` for an unmodified server, `"Paper"`, `"fabric"`, and
/// so on.
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct ServerBrandPayload {
    /// The reported brand.
    pub brand: String,
}

impl PluginChannel for ServerBrandPayload {
    const CHANNEL: &'static str = "minecraft:brand";

    /// Vanilla's own brand payload is a single UTF string read — a VarInt
    /// byte length then exactly that many UTF-8 bytes — and nothing else.
    ///
    /// Returns `None` on a truncated VarInt, a length that overruns the buffer,
    /// **trailing bytes after the string**, or invalid UTF-8. Strictness is right
    /// here because the channel has exactly one shape: a payload that does not
    /// match it is another implementation's, not ours to guess at, and
    /// `PluginChannelState::rejected` keeps it distinguishable from silence.
    fn decode(data: &[u8]) -> Option<Self> {
        let (len, offset) = read_var_int(data)?;
        let len = usize::try_from(len).ok()?;
        let end = offset.checked_add(len)?;
        if end != data.len() {
            return None;
        }
        let text = std::str::from_utf8(data.get(offset..end)?).ok()?;
        Some(Self {
            brand: text.to_owned(),
        })
    }
}

/// Reads one Minecraft VarInt, returning its value and the bytes consumed.
///
/// Hand-rolled rather than taken from `lodestone-core`: `lodestone-ecs` does not
/// depend on the codec crate and a five-byte LEB128 read is smaller than the
/// coupling would be.
fn read_var_int(data: &[u8]) -> Option<(i32, usize)> {
    let mut value: i32 = 0;
    for index in 0..5 {
        let byte = *data.get(index)?;
        value |= i32::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            return Some((value, index + 1));
        }
    }
    // A sixth continuation byte is a malformed VarInt, not a longer number.
    None
}

/// What the client learned from the channel. **The thing to assert** — a
/// non-zero [`Self::announcements`] is a fact about a payload that really
/// traversed the decode, the bus and the dispatch, which
/// `App::is_plugin_added` is not.
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct ReportedServerBrand {
    /// The most recent brand the server reported, or `None` if it never has.
    pub brand: Option<String>,
    /// How many brand payloads were folded. A server may announce twice — once
    /// in Configuration and again in Play — so this is not capped at one.
    pub announcements: u32,
}

/// Registers [`ServerBrandPayload`] as a plugin channel and folds it into
/// [`ReportedServerBrand`].
///
/// Installed by `lodestone_app::client_app`, which is what makes the whole
/// `custom_payload` chain — and the `GameEvent` bus underneath it — live in
/// production. See the module doc.
#[derive(Debug, Default)]
pub struct ServerBrandChannelPlugin;

impl Plugin for ServerBrandChannelPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugin_channel::<ServerBrandPayload>();
        app.init_resource::<ReportedServerBrand>();
        app.add_systems(GameTick, record_server_brand.in_set(EventPriority::Normal));
    }
}

/// `EventPriority::Normal`: folds this tick's [`ServerBrandPayload`] messages
/// into [`ReportedServerBrand`].
///
/// `EventPriority::Normal` rather than `Lowest` so an ordinary plugin can order
/// itself either side of the fold; `dispatch_plugin_channel` runs
/// `.before(EventPriority::Lowest)`, i.e. before every tier, so this tick's
/// payloads are visible on this tick.
pub fn record_server_brand(
    mut inbox: MessageReader<ServerBrandPayload>,
    mut reported: ResMut<ReportedServerBrand>,
) {
    for ServerBrandPayload { brand } in inbox.read() {
        reported.brand = Some(brand.clone());
        reported.announcements += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::{ReportedServerBrand, ServerBrandChannelPlugin, ServerBrandPayload, read_var_int};
    use crate::plugin_channel::{PluginChannel, PluginChannelState};

    /// The expected bytes come from vanilla's own codec read as a record
    /// definition — a VarInt length then the UTF-8 body — not from an encoder of
    /// ours, because there is no brand *encoder* in this crate to round-trip
    /// against.
    #[test]
    fn decodes_a_hand_built_vanilla_brand_payload() {
        // `writeUtf("vanilla")`: VarInt 7, then the seven ASCII bytes.
        let bytes = b"\x07vanilla";
        assert_eq!(
            ServerBrandPayload::decode(bytes),
            Some(ServerBrandPayload {
                brand: "vanilla".to_owned()
            })
        );
    }

    /// The control for the strictness claim in [`ServerBrandPayload::decode`]'s
    /// doc: each of these must be rejected, and the first row proves the
    /// detector is not simply returning `None` for everything (the row above
    /// accepts).
    #[test]
    fn rejects_malformed_payloads() {
        // Length overruns the buffer.
        assert_eq!(ServerBrandPayload::decode(b"\x09vanilla"), None);
        // Trailing bytes after the string.
        assert_eq!(ServerBrandPayload::decode(b"\x07vanillaXX"), None);
        // Empty input: not even a length byte.
        assert_eq!(ServerBrandPayload::decode(b""), None);
        // Invalid UTF-8 body.
        assert_eq!(ServerBrandPayload::decode(b"\x02\xff\xfe"), None);
    }

    #[test]
    fn var_int_reads_multi_byte_values() {
        assert_eq!(read_var_int(&[0x00]), Some((0, 1)));
        assert_eq!(read_var_int(&[0x7f]), Some((127, 1)));
        assert_eq!(read_var_int(&[0x80, 0x01]), Some((128, 2)));
        // Six continuation bytes is malformed, not a wider integer.
        assert_eq!(read_var_int(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x01]), None);
    }

    /// `build` must install the channel state *and* the resource its consumers
    /// read. Asserting `is_plugin_added` instead would pass for a `build` that
    /// stopped inserting either.
    #[test]
    fn build_installs_the_channel_state_and_the_resource() {
        let mut app = bevy_app::App::new();
        app.add_plugins(crate::CorePlugin);
        app.add_plugins(ServerBrandChannelPlugin);
        assert!(
            app.world()
                .get_resource::<PluginChannelState<ServerBrandPayload>>()
                .is_some(),
            "add_plugin_channel did not install the per-channel state"
        );
        assert_eq!(
            app.world()
                .get_resource::<PluginChannelState<ServerBrandPayload>>()
                .map(|state| state.key().to_string()),
            Some("minecraft:brand".to_owned())
        );
        assert!(
            app.world().get_resource::<ReportedServerBrand>().is_some(),
            "the fold's own resource is missing"
        );
    }
}
