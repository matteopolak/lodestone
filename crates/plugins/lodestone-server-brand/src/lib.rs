//! Issue #301's consumer: a plugin that learns the **server's** brand from the
//! `minecraft:brand` plugin channel.
//!
//! # What it is
//!
//! A ~60-line plugin that declares one [`PluginChannel`], decodes its payload,
//! and folds it into one resource. It exists because #301's real defect was not
//! a missing decode — `custom_payload` has decoded since long before the issue
//! was filed — but that **no plugin could be the consumer**. This is the
//! smallest honest thing that is one.
//!
//! # Why `minecraft:brand` specifically
//!
//! Because it is the one plugin channel a real vanilla server always sends, in
//! both directions, which means this plugin's gate can be fed **captured server
//! bytes**. A toy channel of our own invention could only ever be fed bytes our
//! own encoder produced, and `decode(encode(x)) == x` is satisfied by two
//! symmetric misunderstandings — the failure mode `CLAUDE.md`'s evidence
//! standards exist to prevent. `tests/reaches_a_plugin_from_captured_bytes.rs`
//! runs the committed capture through the real `v26-2` adapter.
//!
//! Note this is the *clientbound* half. The outbound `minecraft:brand` our
//! client announces on entering Configuration is a different, already-wired path
//! (`lodestone_model::ClientAction::SendBrand`, produced by
//! `lodestone_client::driver`) and nothing here touches it.
//!
//! # How it works
//!
//! ```text
//!   server bytes ──▶ v26-2 adapter ──▶ ClientEvent::CustomPayload
//!                                          │
//!                            SharedState::apply's game-event bus
//!                                          │
//!                                    GameEvent(..)
//!                                          │
//!                 lodestone_ecs::plugin_channel::dispatch_plugin_channel
//!                        (filters channel, calls ServerBrand::decode)
//!                                          │
//!                                  Messages<ServerBrand>
//!                                          │
//!                               record_server_brand (this crate)
//!                                          │
//!                                  ReportedServerBrand
//! ```
//!
//! [`ServerBrandPlugin::build`] is three lines and installs no bus, no schedule
//! and no message registration of its own: `add_plugin_channel` does all of it,
//! including opting into the game-event bus the payload arrives on. That is the
//! point of the seam — see `lodestone_ecs::plugin_channel`'s module doc.
//!
//! # How to change it
//!
//! * **Assert [`ReportedServerBrand`], never `is_plugin_added`.** A `build` that
//!   stopped inserting the resource still passes
//!   `App::is_plugin_added::<ServerBrandPlugin>()`.
//! * To listen on a different channel, implement [`PluginChannel`] on your own
//!   `#[derive(Message)]` type. Nothing in this crate is privileged; it is
//!   ordinary third-party plugin code.
//! * [`ServerBrand::decode`] is deliberately **strict about trailing bytes**. A
//!   `minecraft:brand` payload is exactly one vanilla-style length-prefixed string, so anything left
//!   over means we are not looking at a brand payload and guessing would be
//!   worse than rejecting — a rejection is counted in
//!   `PluginChannelState::rejected` rather than lost.
//!
//! # Dependencies
//!
//! `lodestone-ecs` for the plugin API, plus `bevy_ecs`/`bevy_app` directly
//! because the derives emit absolute `bevy_ecs::` paths. **No protocol family**
//! — the plugin is version-free; `lodestone-v26-2` is a dev-dependency of the
//! gate only.

use lodestone_ecs::app::{App, Plugin};
use lodestone_ecs::ecs::message::MessageReader;
use lodestone_ecs::ecs::prelude::{IntoScheduleConfigs, Message, Resource};
use lodestone_ecs::ecs::system::ResMut;
use lodestone_ecs::plugin_channel::{PluginChannel, PluginChannelAppExt};
use lodestone_ecs::{EventPriority, GameTick};

/// One `minecraft:brand` payload, decoded: the brand string the server reports
/// for itself (`"vanilla"` for an unmodified server, `"Paper"`, `"fabric"`, …).
///
/// Public field for the same reason `lodestone_ecs::GameEvent`'s is: a
/// subscriber already depends on this crate for the type.
#[derive(Message, Debug, Clone, PartialEq, Eq)]
pub struct ServerBrand {
    /// The reported brand.
    pub brand: String,
}

impl PluginChannel for ServerBrand {
    const CHANNEL: &'static str = "minecraft:brand";

    /// Vanilla's `BrandPayload` is a single `FriendlyByteBuf::readUtf` — a VarInt
    /// byte length then that many UTF-8 bytes — and nothing else.
    ///
    /// Rejects (returns `None`) on a truncated VarInt, a length that overruns the
    /// buffer, **trailing bytes after the string**, or invalid UTF-8. See the
    /// module doc on why strictness is the right default here.
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

/// Reads one Minecraft VarInt, returning its value and the number of bytes
/// consumed.
///
/// Hand-rolled rather than pulled from a protocol crate on purpose: this plugin
/// must not link a version family (see the module doc's dependencies section),
/// and a five-byte LEB128 read is smaller than the coupling would be.
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

/// What this plugin learned. **The thing to assert.**
#[derive(Resource, Debug, Default, Clone, PartialEq, Eq)]
pub struct ReportedServerBrand {
    /// The most recent brand the server reported, or `None` if it never has.
    pub brand: Option<String>,
    /// How many brand payloads were folded. A server may re-announce (once in
    /// Configuration and again in Play), so this is not capped at one.
    pub announcements: u32,
}

/// Registers [`ServerBrand`] as a plugin channel and folds it into
/// [`ReportedServerBrand`].
#[derive(Debug, Default)]
pub struct ServerBrandPlugin;

impl Plugin for ServerBrandPlugin {
    fn build(&self, app: &mut App) {
        app.add_plugin_channel::<ServerBrand>();
        app.init_resource::<ReportedServerBrand>();
        app.add_systems(
            GameTick,
            record_server_brand.in_set(EventPriority::Normal),
        );
    }
}

/// `EventPriority::Normal`: folds this tick's [`ServerBrand`] messages.
pub fn record_server_brand(
    mut inbox: MessageReader<ServerBrand>,
    mut reported: ResMut<ReportedServerBrand>,
) {
    for ServerBrand { brand } in inbox.read() {
        reported.brand = Some(brand.clone());
        reported.announcements += 1;
    }
}

#[cfg(test)]
mod tests {
    //! Unit coverage for the decoder only. The end-to-end gate — captured server
    //! bytes through the real adapter into [`ReportedServerBrand`] — is
    //! `tests/reaches_a_plugin_from_captured_bytes.rs`, deliberately an
    //! integration test so it consumes this crate exactly as a third party does.

    use super::{PluginChannel, ServerBrand, read_var_int};

    /// A vanilla-style length-prefixed string body: single-byte length, then the bytes.
    #[test]
    fn decodes_a_single_byte_length_brand() {
        let payload = b"\x07vanilla";
        assert_eq!(
            ServerBrand::decode(payload),
            Some(ServerBrand {
                brand: "vanilla".to_owned()
            })
        );
    }

    /// The empty string is legal and distinct from a rejection.
    #[test]
    fn decodes_an_empty_brand() {
        assert_eq!(
            ServerBrand::decode(b"\x00"),
            Some(ServerBrand {
                brand: String::new()
            })
        );
    }

    /// **The controls.** Each malformation must be rejected, so
    /// `PluginChannelState::rejected` is a real signal rather than a counter
    /// nothing increments.
    #[test]
    fn rejects_every_malformation() {
        // Empty buffer: no length byte at all.
        assert_eq!(ServerBrand::decode(b""), None);
        // Length overruns the buffer.
        assert_eq!(ServerBrand::decode(b"\x09vanilla"), None);
        // Trailing bytes after a complete string.
        assert_eq!(ServerBrand::decode(b"\x07vanilla!!"), None);
        // Invalid UTF-8 of the stated length.
        assert_eq!(ServerBrand::decode(b"\x02\xff\xfe"), None);
        // A VarInt that never terminates.
        assert_eq!(ServerBrand::decode(b"\xff\xff\xff\xff\xff"), None);
    }

    /// A two-byte VarInt length, to prove the reader is not single-byte-only —
    /// a 200-character brand is well within what a modded server sends.
    #[test]
    fn decodes_a_multi_byte_varint_length() {
        let brand = "m".repeat(200);
        let mut payload = vec![0xc8, 0x01]; // VarInt 200
        payload.extend_from_slice(brand.as_bytes());
        assert_eq!(ServerBrand::decode(&payload), Some(ServerBrand { brand }));
    }

    /// The VarInt reader's own arithmetic, against values computed outside it.
    #[test]
    fn var_int_reader_matches_known_encodings() {
        assert_eq!(read_var_int(&[0x00]), Some((0, 1)));
        assert_eq!(read_var_int(&[0x01]), Some((1, 1)));
        assert_eq!(read_var_int(&[0x7f]), Some((127, 1)));
        assert_eq!(read_var_int(&[0x80, 0x01]), Some((128, 2)));
        assert_eq!(read_var_int(&[0xc8, 0x01]), Some((200, 2)));
        assert_eq!(read_var_int(&[0xff, 0xff, 0x7f]), Some((2_097_151, 3)));
    }
}
