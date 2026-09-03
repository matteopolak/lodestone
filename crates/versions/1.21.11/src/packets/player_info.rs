//! Clientbound tab-list packets for this era (protocol 774).
//!
//! # The action bitmask, and the two bits the era below does not have
//!
//! The leading byte is a **set** of actions, not an ordinal, and each entry
//! carries the fields for every set bit. The 1.20.6 era assigns six bits;
//! this one assigns eight, adding `update_hat` (bit 6) and `update_list_order`
//! (bit 7).
//!
//! Fields are written in **bit order**, so those two land at the tail of every
//! entry. A decoder inherited from the era below stops after the display name
//! and leaves two bytes per entry unread — which the packet-level
//! "consumed exactly" check catches, but only because that check exists.
//!
//! The tail is where this decoder is most easily wrong in a way nothing else
//! notices: a bool and a varint are both one byte for the values a server
//! actually sends, so swapping them costs no length and raises no error. The
//! recorded-join gate pins the order by asking for a *distinguishable* pair —
//! a session whose skin flags turn the hat on, so the bool byte is `1` while
//! the list-order varint is `0`, and the two orders disagree about which is
//! which.
//!
//! The display-name component is **anonymous NBT**, as everywhere else on this
//! wire.

use lodestone_core::{Ctx, Decode, Error, Reader, Result, read_network_nbt};
use lodestone_model::Text;
use uuid::Uuid;

/// Action ordinals, matching the bit positions in the leading mask byte.
pub mod action {
    /// The entry carries a name and profile properties.
    pub const ADD_PLAYER: u8 = 0;
    /// The entry carries that player's chat-signing session.
    pub const INITIALIZE_CHAT: u8 = 1;
    /// The entry carries a game mode.
    pub const UPDATE_GAME_MODE: u8 = 2;
    /// The entry carries the tab-list visibility flag.
    pub const UPDATE_LISTED: u8 = 3;
    /// The entry carries a latency.
    pub const UPDATE_LATENCY: u8 = 4;
    /// The entry carries a display-name component.
    pub const UPDATE_DISPLAY_NAME: u8 = 5;
    /// The entry carries the hat-layer visibility flag.
    pub const UPDATE_HAT: u8 = 6;
    /// The entry carries a tab-list sort priority.
    pub const UPDATE_LIST_ORDER: u8 = 7;
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

/// A player's announced chat-signing session — the public key needed to verify
/// their signed messages.
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
    /// Whether that player's hat skin layer is shown, from `update_hat`.
    pub show_hat: Option<bool>,
    /// Tab-list sort priority, from `update_list_order`. Higher sorts first.
    pub list_order: Option<i32>,
}

/// Clientbound `minecraft:player_info_update` in its action-bitmask form.
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
        // Eight actions fill the fixed-bitset byte exactly, bit `i`
        // (LSB-first) selecting action ordinal `i`.
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
                show_hat: None,
                list_order: None,
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
            if has(action::UPDATE_HAT) {
                entry.show_hat = Some(r.bool()?);
            }
            if has(action::UPDATE_LIST_ORDER) {
                entry.list_order = Some(r.var_i32()?);
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

/// Clientbound `minecraft:player_info_remove`: a list of profile UUIDs to drop
/// from the tab list.
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
