//! Clientbound player-info (tab list) packets for protocol 776.
//!
//! `player_info_update` (id 70) is an *action-bitmask* packet: a leading
//! `EnumSet<Action>` selects which per-entry fields follow, and every entry
//! then carries exactly the fields for the set bits, read in **Action ordinal
//! order**. This conditional-field shape cannot be expressed with the derive
//! macros, so the decoder is hand-written against the wire format of
//! `ClientboundPlayerInfoUpdatePacket` (behavioural reference only).
//!
//! The `EnumSet` is serialised as a fixed bit set of `ceil(N/8)` bytes
//! (`FriendlyByteBuf.writeFixedBitSet`); with the eight actions below that is a
//! single byte, bit `i` (LSB-first) selecting action ordinal `i`.
//!
//! Fields for unmodelled actions (`INITIALIZE_CHAT`, `UPDATE_LIST_ORDER`,
//! `UPDATE_HAT`, and the profile-property list inside `ADD_PLAYER`) are decoded
//! and discarded so the buffer stays aligned — a misparse there would leave
//! trailing bytes, which the adapter rejects.

use lodestone_core::{
    Ctx, Decode, Error, Reader, Result, plain_text_from_nbt_component, read_network_nbt,
};
use uuid::Uuid;

/// `Action` ordinals, matching the `EnumSet` bit positions on the wire.
mod action {
    pub const ADD_PLAYER: u8 = 0;
    pub const INITIALIZE_CHAT: u8 = 1;
    pub const UPDATE_GAME_MODE: u8 = 2;
    pub const UPDATE_LISTED: u8 = 3;
    pub const UPDATE_LATENCY: u8 = 4;
    pub const UPDATE_DISPLAY_NAME: u8 = 5;
    pub const UPDATE_LIST_ORDER: u8 = 6;
    pub const UPDATE_HAT: u8 = 7;
}

/// Maximum lengths from the vanilla stream codecs, used to bound string reads.
const MAX_NAME: usize = 16;
const MAX_PROP_NAME: usize = 64;
const MAX_PROP_VALUE: usize = 32_767;
const MAX_PROP_SIGNATURE: usize = 1_024;
/// `ByteBufCodecs.GAME_PROFILE_PROPERTIES` caps the property count at 16.
const MAX_PROPERTIES: i32 = 16;

/// One decoded tab-list entry. Each optional field is `Some` exactly when its
/// action bit was set in the packet, mirroring the "present in this update"
/// semantics the canonical [`PlayerListEntry`](lodestone_model::PlayerListEntry)
/// expresses with `Option`.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerInfoEntry {
    /// Profile UUID (always present; the entry key).
    pub uuid: Uuid,
    /// Player name, from `ADD_PLAYER`.
    pub name: Option<String>,
    /// Raw game-mode id, from `UPDATE_GAME_MODE`.
    pub game_mode: Option<i32>,
    /// Whether the player is listed, from `UPDATE_LISTED`.
    pub listed: Option<bool>,
    /// Reported latency in milliseconds, from `UPDATE_LATENCY`.
    pub latency: Option<i32>,
    /// Display-name component reduced to plain text, from `UPDATE_DISPLAY_NAME`.
    pub display_name: Option<String>,
}

/// Clientbound `player_info_update` (id 70).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerInfoUpdate {
    /// The updated entries.
    pub entries: Vec<PlayerInfoEntry>,
}

/// Reads a VarInt-prefixed byte array, discarding the bytes. Used to skip the
/// public-key blob and its signature inside a chat session.
fn skip_byte_array(r: &mut Reader<'_>) -> Result<()> {
    let len = r.var_i32()?;
    if len < 0 {
        return Err(Error::NegativeLength(len));
    }
    r.bytes(len as usize)?;
    Ok(())
}

/// Consumes an optional `RemoteChatSession.Data` (`INITIALIZE_CHAT`): a
/// nullability boolean, then, when present, the session UUID and a
/// `ProfilePublicKey.Data` (an instant, the public key, and its signature).
fn skip_chat_session(r: &mut Reader<'_>) -> Result<()> {
    if r.bool()? {
        let _session_id = r.uuid()?;
        let _expires_at = r.i64()?;
        skip_byte_array(r)?; // public key
        skip_byte_array(r)?; // key signature
    }
    Ok(())
}

/// Consumes the `ADD_PLAYER` profile-property multimap, returning the player
/// name. Each property is a name, a value, and an optional signature.
fn read_add_player(r: &mut Reader<'_>) -> Result<String> {
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
    for _ in 0..count {
        let _prop_name = r.string(MAX_PROP_NAME)?;
        let _prop_value = r.string(MAX_PROP_VALUE)?;
        if r.bool()? {
            let _signature = r.string(MAX_PROP_SIGNATURE)?;
        }
    }
    Ok(name)
}

impl Decode for PlayerInfoUpdate {
    fn decode(r: &mut Reader<'_>, _ctx: Ctx) -> Result<Self> {
        // EnumSet<Action> over 8 values → a single fixed-bitset byte.
        let mask = r.u8()?;
        let has = |bit: u8| mask & (1u8 << bit) != 0;

        let count = r.var_i32()?;
        if count < 0 {
            return Err(Error::NegativeLength(count));
        }
        let mut entries = Vec::with_capacity(count as usize);
        for _ in 0..count {
            let uuid = r.uuid()?;
            let mut entry = PlayerInfoEntry {
                uuid,
                name: None,
                game_mode: None,
                listed: None,
                latency: None,
                display_name: None,
            };
            // Fields appear in Action ordinal order for whichever bits are set.
            if has(action::ADD_PLAYER) {
                entry.name = Some(read_add_player(r)?);
            }
            if has(action::INITIALIZE_CHAT) {
                skip_chat_session(r)?;
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
                let component = read_network_nbt(r)?;
                entry.display_name = Some(plain_text_from_nbt_component(&component));
            }
            if has(action::UPDATE_LIST_ORDER) {
                let _list_order = r.var_i32()?;
            }
            if has(action::UPDATE_HAT) {
                let _show_hat = r.bool()?;
            }
            entries.push(entry);
        }
        Ok(Self { entries })
    }
}

/// Clientbound `player_info_remove` (id 69): a list of profile UUIDs to drop.
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
        let mut uuids = Vec::with_capacity(count as usize);
        for _ in 0..count {
            uuids.push(r.uuid()?);
        }
        Ok(Self { uuids })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_core::Writer;

    const CTX: Ctx = Ctx { version: 776 };

    /// Builds the network-NBT encoding of a plain `{"text":"..."}` component:
    /// a `TAG_Compound` (10) with a single `TAG_String` (8) field `text`.
    fn nbt_text_component(text: &str) -> Vec<u8> {
        let mut w = Writer::default();
        w.u8(10); // TAG_Compound, unnamed (network NBT drops the root name)
        w.u8(8); // TAG_String
        let key = b"text";
        w.u8(0);
        w.u8(key.len() as u8);
        w.bytes(key);
        let value = text.as_bytes();
        w.u8((value.len() >> 8) as u8);
        w.u8((value.len() & 0xff) as u8);
        w.bytes(value);
        w.u8(0); // TAG_End
        w.into_vec()
    }

    fn decode_exact<T: Decode>(bytes: &[u8]) -> T {
        let mut r = Reader::new(bytes);
        let value = T::decode(&mut r, CTX).expect("decode");
        r.ensure_empty().expect("no trailing bytes");
        value
    }

    #[test]
    fn decodes_full_add_player_entry() {
        // The action set the server sends for a joining player: ADD_PLAYER,
        // INITIALIZE_CHAT, UPDATE_GAME_MODE, UPDATE_LISTED, UPDATE_LATENCY,
        // UPDATE_DISPLAY_NAME, UPDATE_HAT, UPDATE_LIST_ORDER (all eight).
        let uuid = Uuid::from_u128(0x0123_4567_89ab_cdef_0011_2233_4455_6677);
        let mut w = Writer::default();
        w.u8(0xFF); // all eight action bits set
        w.var_i32(1); // one entry
        w.uuid(uuid);
        // ADD_PLAYER: name + zero properties
        w.string("Notch");
        w.var_i32(0);
        // INITIALIZE_CHAT: absent
        w.bool(false);
        // UPDATE_GAME_MODE: creative (1)
        w.var_i32(1);
        // UPDATE_LISTED: true
        w.bool(true);
        // UPDATE_LATENCY: 42 ms
        w.var_i32(42);
        // UPDATE_DISPLAY_NAME: present, plain component
        w.bool(true);
        w.bytes(&nbt_text_component("Notch!"));
        // UPDATE_LIST_ORDER: 0
        w.var_i32(0);
        // UPDATE_HAT: true
        w.bool(true);

        let update: PlayerInfoUpdate = decode_exact(&w.into_vec());
        assert_eq!(update.entries.len(), 1);
        let e = &update.entries[0];
        assert_eq!(e.uuid, uuid);
        assert_eq!(e.name.as_deref(), Some("Notch"));
        assert_eq!(e.game_mode, Some(1));
        assert_eq!(e.listed, Some(true));
        assert_eq!(e.latency, Some(42));
        assert_eq!(e.display_name.as_deref(), Some("Notch!"));
    }

    #[test]
    fn decodes_latency_only_update() {
        // A latency-only refresh (UPDATE_LATENCY, ordinal 4) for two players:
        // no name/gamemode/etc., proving the bitmask gates each field.
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let mut w = Writer::default();
        w.u8(1 << 4); // only UPDATE_LATENCY
        w.var_i32(2);
        w.uuid(a);
        w.var_i32(10);
        w.uuid(b);
        w.var_i32(20);

        let update: PlayerInfoUpdate = decode_exact(&w.into_vec());
        assert_eq!(update.entries.len(), 2);
        assert_eq!(update.entries[0].uuid, a);
        assert_eq!(update.entries[0].latency, Some(10));
        assert_eq!(update.entries[0].name, None);
        assert_eq!(update.entries[1].latency, Some(20));
    }

    #[test]
    fn decodes_remove() {
        let a = Uuid::from_u128(7);
        let b = Uuid::from_u128(8);
        let mut w = Writer::default();
        w.var_i32(2);
        w.uuid(a);
        w.uuid(b);
        let remove: PlayerInfoRemove = decode_exact(&w.into_vec());
        assert_eq!(remove.uuids, vec![a, b]);
    }

    #[test]
    fn rejects_negative_count() {
        let mut w = Writer::default();
        w.u8(0);
        w.var_i32(-1);
        let bytes = w.into_vec();
        let mut r = Reader::new(&bytes);
        assert!(PlayerInfoUpdate::decode(&mut r, CTX).is_err());
    }
}
