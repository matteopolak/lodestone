//! Clientbound `player_info` packet -- the tab list.
//!
//! Byte-identical across every protocol these three crates cover (47, 340,
//! 754): diffed field by field against `vendor/minecraft-data`'s
//! `pc/1.8/protocol.json` and `pc/1.12.2/protocol.json` `packet_player_info`
//! type, and the v735 (1.16.5, protocol 754) crate's own hand-written
//! `Decode` impl was byte-for-byte identical to v340's before this move
//! (only doc comments and a test-only `CTX` constant differed). Unlike
//! 26.2's split `player_info_update`/`player_info_remove` pair (an
//! action-*bitmask* per entry), this era has a single `player_info` packet
//! whose **one leading `action` varint applies to every entry in the
//! packet**: `0` = add (name, properties, game mode, ping, display name all
//! at once), `1` = update game mode, `2` = update latency, `3` = update
//! display name, `4` = remove. That whole-packet-shared discriminant
//! (rather than a per-entry bitmask) is why this needs a hand-written
//! decoder: the derive macros' `#[mc(when = ...)]` expresses a field gated
//! on a sibling field within the *same* struct, not a shape shared by every
//! element of an array.
//!
//! [`PlayerInfo`] has no `#[derive(Packet)]` (it is decoded directly by each
//! family's adapter from the wire id, not dispatched by name through the
//! derive), so there is no `#[mc(protocols = ...)]` to carry its range --
//! this doc comment is the only place that range is recorded. Do not widen
//! its use past protocol 754 without checking a later era's shape first.
//!
//! Display names travel as a bare JSON string here, the same as every other
//! chat field in this era (`ClientboundChat::message`) -- this era predates
//! the network-NBT chat-component wire format modern versions use. Decoded
//! to `Text` at the adapter layer via `Text::from_json`, not here.

use lodestone_core::{Ctx, Decode, Error, Reader, Result};
use uuid::Uuid;

/// Maximum player name length (`STRING(16)` in vanilla's profile codec).
const MAX_NAME: usize = 16;
/// Maximum property name/value/signature lengths -- this era has no
/// dedicated property codec limit, so this uses the wire's own general
/// string cap.
const MAX_PROP_FIELD: usize = 32_767;
/// Maximum display-name JSON length, matching `ClientboundChat::message`'s
/// treatment of chat-shaped strings elsewhere in this crate.
const MAX_DISPLAY_NAME: usize = 32_767;

/// A single profile property from the `add_player` action (`name`, `value`,
/// optional `signature`) -- same triple as every other version's tab-list
/// property list. **This is where a remote player's skin comes from**
/// (`minecraft:textures`); it is carried into
/// [`lodestone_model::ProfileProperty`] at the adapter layer, never dropped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerInfoProperty {
    /// Property name, e.g. `textures`.
    pub name: String,
    /// Property value. Base64-encoded JSON for `textures`.
    pub value: String,
    /// Signature over the value, present only in online mode.
    pub signature: Option<String>,
}

/// The per-action payload a `player_info` entry carries, selected by the
/// packet's single leading `action` field (shared by every entry).
#[derive(Debug, Clone, PartialEq)]
pub enum PlayerInfoAction {
    /// `action == 0`: a full profile add.
    AddPlayer {
        /// Player name.
        name: String,
        /// Profile properties (skin texture, cape, …).
        properties: Vec<PlayerInfoProperty>,
        /// Raw game-mode id (`0..=3`).
        game_mode: i32,
        /// Reported latency in milliseconds.
        ping: i32,
        /// Display-name JSON, when the server set one.
        display_name: Option<String>,
    },
    /// `action == 1`.
    UpdateGameMode {
        /// Raw game-mode id.
        game_mode: i32,
    },
    /// `action == 2`.
    UpdateLatency {
        /// Reported latency in milliseconds.
        ping: i32,
    },
    /// `action == 3`.
    UpdateDisplayName {
        /// New display-name JSON, or `None` to clear it.
        display_name: Option<String>,
    },
    /// `action == 4`: no further fields.
    RemovePlayer,
}

/// One entry: a profile id plus the action-shaped payload above.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerInfoEntry {
    /// Profile UUID.
    pub uuid: Uuid,
    /// This entry's action data.
    pub action: PlayerInfoAction,
}

/// Clientbound `player_info`.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerInfo {
    /// Every entry in this update, each carrying the packet's single shared
    /// action.
    pub entries: Vec<PlayerInfoEntry>,
}

fn read_optional_string(r: &mut Reader<'_>, max_chars: usize) -> Result<Option<String>> {
    if r.bool()? {
        Ok(Some(r.string(max_chars)?))
    } else {
        Ok(None)
    }
}

fn read_add_player(r: &mut Reader<'_>) -> Result<PlayerInfoAction> {
    let name = r.string(MAX_NAME)?;
    let count = r.var_i32()?;
    if count < 0 {
        return Err(Error::NegativeLength(count));
    }
    // Bounded by the readable bytes: the count is attacker-controlled and
    // each property costs at least a few bytes, so `remaining()` is a sound
    // ceiling -- the same guard `lodestone-macros`' `decode_vec` applies.
    let mut properties = Vec::with_capacity((count as usize).min(r.remaining()));
    for _ in 0..count {
        let prop_name = r.string(MAX_PROP_FIELD)?;
        let value = r.string(MAX_PROP_FIELD)?;
        let signature = read_optional_string(r, MAX_PROP_FIELD)?;
        properties.push(PlayerInfoProperty {
            name: prop_name,
            value,
            signature,
        });
    }
    let game_mode = r.var_i32()?;
    let ping = r.var_i32()?;
    let display_name = read_optional_string(r, MAX_DISPLAY_NAME)?;
    Ok(PlayerInfoAction::AddPlayer {
        name,
        properties,
        game_mode,
        ping,
        display_name,
    })
}

impl Decode for PlayerInfo {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let action_id = r.var_i32()?;
        let count = r.var_i32()?;
        if count < 0 {
            return Err(Error::NegativeLength(count));
        }
        // Same reasoning as `read_add_player`'s cap: a uuid alone is 16
        // bytes, so `remaining()` bounds how many entries can actually
        // follow regardless of what `count` claims.
        let mut entries = Vec::with_capacity((count as usize).min(r.remaining()));
        for _ in 0..count {
            let uuid = r.uuid()?;
            let action = match action_id {
                0 => read_add_player(r)?,
                1 => PlayerInfoAction::UpdateGameMode {
                    game_mode: r.var_i32()?,
                },
                2 => PlayerInfoAction::UpdateLatency { ping: r.var_i32()? },
                3 => PlayerInfoAction::UpdateDisplayName {
                    display_name: read_optional_string(r, MAX_DISPLAY_NAME)?,
                },
                4 => PlayerInfoAction::RemovePlayer,
                other => {
                    return Err(Error::InvalidEnumVariant {
                        name: "player_info action",
                        value: other,
                    });
                }
            };
            entries.push(PlayerInfoEntry { uuid, action });
        }
        Ok(Self { entries })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_core::Writer;

    const CTX: Ctx = Ctx { version: 47 };

    fn decode_exact(bytes: &[u8]) -> PlayerInfo {
        let mut r = Reader::new(bytes);
        let value = PlayerInfo::decode(&mut r, CTX).expect("decode");
        r.ensure_empty().expect("no trailing bytes");
        value
    }

    #[test]
    fn decodes_full_add_player_entry_with_properties() {
        let uuid = Uuid::from_u128(0x0123_4567_89ab_cdef_0011_2233_4455_6677);
        let mut w = Writer::default();
        w.var_i32(0); // action = add_player
        w.var_i32(1); // one entry
        w.uuid(uuid);
        w.string("Notch");
        w.var_i32(2); // two properties
        w.string("textures");
        w.string("eyJ0ZXh0dXJlcyI6e319");
        w.bool(true);
        w.string("MOJANG_SIGNATURE");
        w.string("unsigned_prop");
        w.string("value");
        w.bool(false);
        w.var_i32(1); // game_mode = creative
        w.var_i32(42); // ping
        w.bool(true); // display name present
        w.string("{\"text\":\"Notch!\"}");

        let update = decode_exact(&w.into_vec());
        assert_eq!(update.entries.len(), 1);
        let entry = &update.entries[0];
        assert_eq!(entry.uuid, uuid);
        match &entry.action {
            PlayerInfoAction::AddPlayer {
                name,
                properties,
                game_mode,
                ping,
                display_name,
            } => {
                assert_eq!(name, "Notch");
                assert_eq!(properties.len(), 2, "the second property was mis-framed");
                assert_eq!(properties[0].name, "textures");
                assert_eq!(properties[0].signature.as_deref(), Some("MOJANG_SIGNATURE"));
                assert_eq!(properties[1].name, "unsigned_prop");
                assert_eq!(properties[1].signature, None);
                assert_eq!(*game_mode, 1);
                assert_eq!(*ping, 42);
                assert_eq!(display_name.as_deref(), Some("{\"text\":\"Notch!\"}"));
            }
            other => panic!("expected AddPlayer, got {other:?}"),
        }
    }

    #[test]
    fn decodes_latency_only_update_for_two_players() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let mut w = Writer::default();
        w.var_i32(2); // action = update_latency
        w.var_i32(2); // two entries
        w.uuid(a);
        // Pairwise-distinct latencies so a transposition between entries is
        // visible.
        w.var_i32(11);
        w.uuid(b);
        w.var_i32(87);

        let update = decode_exact(&w.into_vec());
        assert_eq!(update.entries.len(), 2);
        assert_eq!(update.entries[0].uuid, a);
        assert!(matches!(
            update.entries[0].action,
            PlayerInfoAction::UpdateLatency { ping: 11 }
        ));
        assert_eq!(update.entries[1].uuid, b);
        assert!(matches!(
            update.entries[1].action,
            PlayerInfoAction::UpdateLatency { ping: 87 }
        ));
    }

    #[test]
    fn decodes_update_display_name_including_an_explicit_clear() {
        let with_name = Uuid::from_u128(3);
        let cleared = Uuid::from_u128(4);
        let mut w = Writer::default();
        w.var_i32(3); // action = update_display_name
        w.var_i32(2);
        w.uuid(with_name);
        w.bool(true);
        w.string("{\"text\":\"Renamed\"}");
        w.uuid(cleared);
        w.bool(false); // explicit clear, no string follows

        let update = decode_exact(&w.into_vec());
        assert!(matches!(
            &update.entries[0].action,
            PlayerInfoAction::UpdateDisplayName { display_name: Some(s) }
                if s == "{\"text\":\"Renamed\"}"
        ));
        assert!(matches!(
            update.entries[1].action,
            PlayerInfoAction::UpdateDisplayName { display_name: None }
        ));
    }

    #[test]
    fn decodes_remove_entries_with_no_trailing_fields() {
        let a = Uuid::from_u128(7);
        let b = Uuid::from_u128(8);
        let mut w = Writer::default();
        w.var_i32(4); // action = remove_player
        w.var_i32(2);
        w.uuid(a);
        w.uuid(b);

        let update = decode_exact(&w.into_vec());
        assert_eq!(update.entries.len(), 2);
        assert!(matches!(
            update.entries[0].action,
            PlayerInfoAction::RemovePlayer
        ));
        assert!(matches!(
            update.entries[1].action,
            PlayerInfoAction::RemovePlayer
        ));
    }

    #[test]
    fn rejects_negative_entry_count() {
        let mut w = Writer::default();
        w.var_i32(4);
        w.var_i32(-1);
        let bytes = w.into_vec();
        let mut r = Reader::new(&bytes);
        assert!(PlayerInfo::decode(&mut r, CTX).is_err());
    }

    #[test]
    fn rejects_unknown_action_id() {
        let mut w = Writer::default();
        w.var_i32(9); // not a valid action
        w.var_i32(1);
        w.uuid(Uuid::from_u128(1));
        let bytes = w.into_vec();
        let mut r = Reader::new(&bytes);
        assert!(PlayerInfo::decode(&mut r, CTX).is_err());
    }
}
