//! Clientbound tab-list packets for this era (protocol 766).
//!
//! # Why this is not the shared decoder the oldest eras use
//!
//! Through 1.18 the tab list is one packet carrying a leading **action
//! ordinal** — add, update game mode, update latency, update display name,
//! remove — and every entry in the packet performs that one action. 1.19.3
//! replaced it with an **action bitmask**: the leading byte is a set, and each
//! entry carries the fields for *every* set bit, read in ordinal order. The
//! removal action moved out into its own packet, [`PlayerInfoRemove`].
//!
//! The two shapes share a first byte and nothing else. An `add_player`
//! ordinal is `0`, and an `add_player` bitmask is also `0x01`… but a bitmask
//! of `0x01` with `initialize_chat` unset is `1`, which an ordinal decoder
//! reads as "update game mode" — so a stale decoder does not fail, it
//! attributes the wrong action to every entry it reads.
//!
//! # What 766 has that its neighbours do not
//!
//! Six actions here against the modern family's eight: `update_list_order`
//! and `update_hat` are later additions, so this decoder must **not** read
//! them. The display-name component is **anonymous NBT** at 766, where the
//! 1.19 era writes a length-prefixed JSON string, so a decoder inherited from
//! there reads a two-byte length where a tag byte is.
//!
//! Both facts are checked against a real 766 join: a full-mask entry decodes
//! as name, zero properties, no chat session, game mode `0`, listed `true`,
//! latency `0`, no display name, and consumes the packet exactly.

use lodestone_core::{Ctx, Decode, Error, Reader, Result, read_network_nbt};
use lodestone_model::Text;
use uuid::Uuid;

/// Action ordinals, matching the bit positions in the leading mask byte.
mod action {
    pub const ADD_PLAYER: u8 = 0;
    pub const INITIALIZE_CHAT: u8 = 1;
    pub const UPDATE_GAME_MODE: u8 = 2;
    pub const UPDATE_LISTED: u8 = 3;
    pub const UPDATE_LATENCY: u8 = 4;
    pub const UPDATE_DISPLAY_NAME: u8 = 5;
}

/// String-length caps applied before any allocation, so a hostile server
/// cannot turn a declared length into an allocation attack.
const MAX_NAME: usize = 16;
const MAX_PROP_NAME: usize = 64;
const MAX_PROP_VALUE: usize = 32_767;
const MAX_PROP_SIGNATURE: usize = 1_024;
/// Profile-property count cap.
const MAX_PROPERTIES: i32 = 16;

/// One profile property: a name, a value, and an optional signature over the
/// value (present only in online mode).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileProperty {
    /// Property name, such as `textures`.
    pub name: String,
    /// Property value; base64-encoded JSON for `textures`.
    pub value: String,
    /// Signature over the value, when the server supplied one.
    pub signature: Option<String>,
}

/// A player's announced chat-signing session — the public key needed to
/// verify their signed messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteChatSessionData {
    /// That player's chat-session UUID.
    pub session_id: Uuid,
    /// Public-key expiry, epoch milliseconds.
    pub expires_at: i64,
    /// DER-encoded RSA public key, verbatim.
    pub public_key: Vec<u8>,
    /// The session server's signature over `public_key`, verbatim.
    pub key_signature: Vec<u8>,
}

/// One decoded tab-list entry. Each optional field is `Some` exactly when its
/// action bit was set, mirroring the "present in this update" semantics the
/// canonical [`PlayerListEntry`](lodestone_model::PlayerListEntry) expresses.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerInfoEntry {
    /// Profile UUID (always present; the entry key).
    pub uuid: Uuid,
    /// Player name, from `add_player`.
    pub name: Option<String>,
    /// Profile properties, from `add_player`. These carry the skin.
    pub properties: Option<Vec<ProfileProperty>>,
    /// That player's announced chat-signing session, from `initialize_chat`.
    pub chat_session: Option<RemoteChatSessionData>,
    /// Raw game-mode id, from `update_game_mode`.
    pub game_mode: Option<i32>,
    /// Whether the player is listed, from `update_listed`.
    pub listed: Option<bool>,
    /// Reported latency in milliseconds, from `update_latency`.
    pub latency: Option<i32>,
    /// Display-name component, from `update_display_name`. Anonymous NBT on
    /// this wire, not a JSON string.
    pub display_name: Option<Text>,
}

/// Clientbound `player_info` in its action-bitmask form.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerInfoUpdate {
    /// The raw action mask, retained so a consumer can tell "this update did
    /// not mention latency" from "this update set latency to zero" without
    /// re-deriving it from the `Option`s.
    pub actions: u8,
    /// The updated entries.
    pub entries: Vec<PlayerInfoEntry>,
}

/// Reads a VarInt-prefixed byte array.
fn read_byte_array(r: &mut Reader<'_>) -> Result<Vec<u8>> {
    let len = r.var_i32()?;
    if len < 0 {
        return Err(Error::NegativeLength(len));
    }
    Ok(r.bytes(len as usize)?.to_vec())
}

/// Reads an optional chat session: a nullability boolean, then, when present,
/// the session UUID, the key expiry, the key, and its signature.
fn read_chat_session(r: &mut Reader<'_>) -> Result<Option<RemoteChatSessionData>> {
    if !r.bool()? {
        return Ok(None);
    }
    let session_id = r.uuid()?;
    let expires_at = r.i64()?;
    let public_key = read_byte_array(r)?;
    let key_signature = read_byte_array(r)?;
    Ok(Some(RemoteChatSessionData {
        session_id,
        expires_at,
        public_key,
        key_signature,
    }))
}

/// Reads the `add_player` action: the player name, then the profile-property
/// list. Each property is a name, a value, and an optional signature.
fn read_add_player(r: &mut Reader<'_>) -> Result<(String, Vec<ProfileProperty>)> {
    let name = r.string(MAX_NAME)?;
    let count = r.var_i32()?;
    if count < 0 {
        return Err(Error::NegativeLength(count));
    }
    if count > MAX_PROPERTIES {
        return Err(Error::LimitExceeded {
            limit: MAX_PROPERTIES as usize,
            actual: count as usize,
        });
    }
    let mut properties = Vec::with_capacity(count as usize);
    for _ in 0..count {
        let prop_name = r.string(MAX_PROP_NAME)?;
        let value = r.string(MAX_PROP_VALUE)?;
        let signature = if r.bool()? {
            Some(r.string(MAX_PROP_SIGNATURE)?)
        } else {
            None
        };
        properties.push(ProfileProperty {
            name: prop_name,
            value,
            signature,
        });
    }
    Ok((name, properties))
}

impl Decode for PlayerInfoUpdate {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        // Six actions fit one fixed-bitset byte, bit `i` (LSB-first)
        // selecting action ordinal `i`. Bits 6 and 7 are unassigned at this
        // protocol and are deliberately not read as anything.
        let actions = r.u8()?;
        let has = |bit: u8| actions & (1u8 << bit) != 0;

        let count = r.var_i32()?;
        if count < 0 {
            return Err(Error::NegativeLength(count));
        }
        // Bounded by the readable bytes: the count is attacker-controlled and
        // each entry costs at least a uuid, so `remaining()` is a generous but
        // sound ceiling.
        let mut entries = Vec::with_capacity((count as usize).min(r.remaining()));
        for _ in 0..count {
            let uuid = r.uuid()?;
            let mut entry = PlayerInfoEntry {
                uuid,
                name: None,
                properties: None,
                chat_session: None,
                game_mode: None,
                listed: None,
                latency: None,
                display_name: None,
            };
            // Fields appear in action ordinal order, for whichever bits are set.
            if has(action::ADD_PLAYER) {
                let (name, properties) = read_add_player(r)?;
                entry.name = Some(name);
                entry.properties = Some(properties);
            }
            if has(action::INITIALIZE_CHAT) {
                entry.chat_session = read_chat_session(r)?;
            }
            if has(action::UPDATE_GAME_MODE) {
                entry.game_mode = Some(r.var_i32()?);
            }
            if has(action::UPDATE_LISTED) {
                entry.listed = Some(r.bool()?);
            }
            if has(action::UPDATE_LATENCY) {
                entry.latency = Some(r.var_i32()?);
            }
            if has(action::UPDATE_DISPLAY_NAME) && r.bool()? {
                entry.display_name = Some(Text::from_nbt(&read_network_nbt(r)?));
            }
            entries.push(entry);
        }
        Ok(Self { actions, entries })
    }
}

impl PlayerInfoUpdate {
    /// Whether this update carried the named action bit.
    #[must_use]
    pub const fn has_action(&self, bit: u8) -> bool {
        self.actions & (1u8 << bit) != 0
    }

    /// Whether this update added players (and therefore carries names and
    /// properties).
    #[must_use]
    pub const fn adds_players(&self) -> bool {
        self.has_action(action::ADD_PLAYER)
    }
}

/// Clientbound `player_remove`: a list of profile UUIDs to drop from the tab
/// list.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerInfoRemove {
    /// UUIDs of players removed from the tab list.
    pub uuids: Vec<Uuid>,
}

impl Decode for PlayerInfoRemove {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        let count = r.var_i32()?;
        if count < 0 {
            return Err(Error::NegativeLength(count));
        }
        let mut uuids = Vec::with_capacity((count as usize).min(r.remaining()));
        for _ in 0..count {
            uuids.push(r.uuid()?);
        }
        Ok(Self { uuids })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A full-mask single-entry `player_info` recorded from a real 766
    /// server. Every field's presence and value is fixed by the byte string,
    /// which came off the wire rather than out of this decoder.
    const FULL_MASK_ENTRY: &[u8] = &[
        0x3f, 0x01, 0x8c, 0x48, 0xf3, 0x38, 0x50, 0xa7, 0x3e, 0x77, 0xbe, 0x67, 0x5f, 0x16, 0x1d,
        0x9b, 0xd8, 0xf4, 0x0a, b'E', b'0', b'_', b'1', b'i', b't', b'q', b't', b'r', b'7', 0x00,
        0x00, 0x00, 0x01, 0x00, 0x00,
    ];

    #[test]
    fn full_mask_entry_decodes_and_consumes_the_packet() {
        let mut r = Reader::new(FULL_MASK_ENTRY);
        let update = PlayerInfoUpdate::decode(&mut r, Ctx { version: 766 }).unwrap();
        assert_eq!(r.remaining(), 0, "decoder must consume the whole packet");
        assert_eq!(update.actions, 0x3f);
        assert!(update.adds_players());
        let entry = &update.entries[0];
        assert_eq!(entry.name.as_deref(), Some("E0_1itqtr7"));
        assert_eq!(entry.properties.as_deref(), Some(&[][..]));
        assert_eq!(entry.chat_session, None);
        assert_eq!(entry.game_mode, Some(0));
        assert_eq!(entry.listed, Some(true));
        assert_eq!(entry.latency, Some(0));
        assert_eq!(entry.display_name, None);
    }

    #[test]
    fn a_latency_only_update_reads_no_other_field() {
        // Same connection, mask 0x10: latency alone.
        let bytes: Vec<u8> = [0x10u8, 0x01]
            .iter()
            .copied()
            .chain(FULL_MASK_ENTRY[2..18].iter().copied())
            .chain([0x00u8])
            .collect();
        let mut r = Reader::new(&bytes);
        let update = PlayerInfoUpdate::decode(&mut r, Ctx { version: 766 }).unwrap();
        assert_eq!(r.remaining(), 0);
        assert!(!update.adds_players());
        let entry = &update.entries[0];
        assert_eq!(entry.latency, Some(0));
        assert_eq!(entry.name, None);
        assert_eq!(entry.listed, None);
    }

    /// The control for the two decodes above: reading the display name as the
    /// 1.19 era does (a JSON string) must not silently succeed here.
    #[test]
    fn an_entry_with_a_display_name_is_not_a_json_string() {
        // mask 0x20 (display name only), one entry, present flag, then an
        // anonymous NBT string component: tag 0x08, u16 length, payload.
        let mut bytes: Vec<u8> = vec![0x20, 0x01];
        bytes.extend_from_slice(&FULL_MASK_ENTRY[2..18]);
        bytes.extend_from_slice(&[0x01, 0x08, 0x00, 0x02, b'h', b'i']);
        let mut r = Reader::new(&bytes);
        let update = PlayerInfoUpdate::decode(&mut r, Ctx { version: 766 }).unwrap();
        assert_eq!(r.remaining(), 0);
        assert_eq!(
            update.entries[0].display_name.as_ref().map(Text::to_plain_string),
            Some("hi".to_owned())
        );
        // A JSON-string read would have taken the tag byte as the high byte
        // of a length prefix: 0x0800 characters, far past the packet.
        let mut r = Reader::new(&bytes[18..]);
        assert!(r.bool().unwrap());
        assert!(r.string(262_144).is_err());
    }
}
