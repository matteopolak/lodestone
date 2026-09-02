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
//! (`vanilla's own friendly byte buf's own write fixed bit set`); with the eight actions below that is a
//! single byte, bit `i` (LSB-first) selecting action ordinal `i`.
//!
//! `UPDATE_LIST_ORDER` and `UPDATE_HAT` are now kept, not discarded: they
//! carry into `lodestone_model::event::PlayerListEntry::list_order`/
//! `hat_visible`, and `lodestone_game::tablist` folds them into
//! `PlayerListEntry::list_order`/`show_hat` — the tab-list sort order and the
//! second-skin-layer toggle. `INITIALIZE_CHAT` is likewise kept (see
//! [`RemoteChatSessionData`]) rather than discarded: this is the other
//! player's announced chat-signing session, and `adapter::player` carries it
//! into `lodestone_model::event::PlayerListEntry::chat_session`, where
//! `lodestone_client`'s driver reads it to verify their signed messages.

use lodestone_core::{Ctx, Decode, Error, Reader, Result, read_network_nbt};
use lodestone_model::Text;
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
/// `vanilla's own byte buf codecs's own game profile properties` caps the property count at 16.
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
    /// Display-name component, from `UPDATE_DISPLAY_NAME`. Decoded through
    /// [`Text::from_nbt`] — the same route `adapter::chat` uses for the
    /// identical NBT shape — rather than flattened to plain text, so an
    /// explicit hex colour on a player's display name survives instead of
    /// arriving as unstyled white.
    pub display_name: Option<Text>,
    /// Profile properties, from `ADD_PLAYER`.
    ///
    /// **These carry the skin.** They were decoded and discarded into `let _`
    /// until a decoder for `minecraft:textures` landed: before that, its base64
    /// payload never left this crate, so no remote player could have a skin.
    /// `None` means the
    /// update had no `ADD_PLAYER` action at all, which a merging fold must treat
    /// as "unchanged" rather than "no properties".
    pub properties: Option<Vec<ProfileProperty>>,
    /// This player's announced chat-signing session, from `INITIALIZE_CHAT`.
    /// `Some(None)` would be indistinguishable from "action absent" here, so
    /// (matching every other action in this struct) `None` means the
    /// `INITIALIZE_CHAT` bit was not set in this particular update; vanilla's
    /// own field is `Optional<vanilla's own remote chat session's own data>` because a player can
    /// announce "no session" explicitly (the nullability boolean on the wire
    /// reads `false`) — that inner case is folded into this same `None` since
    /// no caller here needs to tell "never announced" from "announced empty"
    /// apart.
    pub chat_session: Option<RemoteChatSessionData>,
    /// Tab-list sort key, from `UPDATE_LIST_ORDER`. Higher orders sort first
    /// (`lodestone_game::tablist::TabList::ordered_by`).
    pub list_order: Option<i32>,
    /// Whether the player's hat (second skin layer) renders in the tab list,
    /// from `UPDATE_HAT`.
    pub hat_visible: Option<bool>,
}

/// One profile property from `ADD_PLAYER`: a name, a value, and an optional
/// Mojang signature over the value (present only in online mode).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileProperty {
    /// Property name, e.g. `textures`.
    pub name: String,
    /// Property value. Base64-encoded JSON for `textures`.
    pub value: String,
    /// Signature over the value, when the server supplied one.
    pub signature: Option<String>,
}

/// A player's announced chat-signing session (`vanilla's own remote chat session's own data`):
/// their session UUID and Mojang-issued public key, as broadcast by
/// `INITIALIZE_CHAT`.
///
/// This is the *receiving* half of secure chat — the public key needed to
/// verify messages from this player (`lodestone_auth::verify_signature`).
///
/// **Now retained past this decode** (issue #283): the adapter carries this
/// into `lodestone_model::event::PlayerListEntry::chat_session` (see
/// `adapter::player`'s `PLAYER_INFO_UPDATE` arm), and `lodestone_client`'s
/// driver looks it up there to verify a signed `PLAYER_CHAT` message's
/// signature. What is still missing is the fuller per-sender chain validator
/// vanilla's own `RemoteChatSession`/`vanilla's own signed message chain's own decoder` runs —
/// link-index ordering and expiry are not enforced here, only the RSA check
/// itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteChatSessionData {
    /// This player's chat-session UUID.
    pub session_id: Uuid,
    /// Public-key expiry, epoch milliseconds (`vanilla's own profile public key's own data's own expires at`,
    /// `readInstant` = epoch millis).
    pub expires_at: i64,
    /// DER-encoded (X.509 `SubjectPublicKeyInfo`) RSA public key, verbatim.
    pub public_key: Vec<u8>,
    /// Mojang's signature over `public_key` (`signature_v2` on the wire —
    /// `vanilla's own profile public key's own data`'s codec field name — not independently verified
    /// by this client; only the server checks it against Mojang's own key).
    pub key_signature: Vec<u8>,
}

/// Clientbound `player_info_update` (id 70).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct PlayerInfoUpdate {
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

/// Reads an optional `vanilla's own remote chat session's own data` (`INITIALIZE_CHAT`): a
/// nullability boolean, then, when present, the session UUID and a
/// `vanilla's own profile public key's own data` (an instant, the public key, and its signature).
///
/// **Used to discard every field here** (`skip_byte_array`/`skip_chat_session`,
/// before this fix) — the session UUID, expiry and public key were read off
/// the wire purely to stay aligned and then thrown away, so no chat message
/// from any other player could ever be verified client-side. The bytes were
/// always correct; nothing carried them past this function.
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

/// Reads the `ADD_PLAYER` action: the player name, then the profile-property
/// multimap. Each property is a name, a value, and an optional signature.
///
/// **This used to discard the properties**, and that was the whole of the
/// remote-player skin gap: `minecraft:textures` — base64 JSON holding the skin URL and
/// its model declaration — was read off the wire into `let _` and never left this
/// crate, so every remote player rendered with the default skin. The bytes were
/// always correct; nothing carried them.
///
/// The limits are unchanged and still enforced before any allocation, so a hostile
/// server cannot turn "keep the properties" into an allocation attack.
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
        // EnumSet<Action> over 8 values → a single fixed-bitset byte.
        let mask = r.u8()?;
        let has = |bit: u8| mask & (1u8 << bit) != 0;

        let count = r.var_i32()?;
        if count < 0 {
            return Err(Error::NegativeLength(count));
        }
        // Bounded by the readable bytes, matching `decode_vec`: the count is
        // attacker-controlled and each entry costs at least a uuid, so
        // `remaining()` is a generous but sound ceiling. Without this a tiny
        // payload declaring a huge count OOMs the client before a single
        // entry is read.
        let mut entries = Vec::with_capacity((count as usize).min(r.remaining()));
        for _ in 0..count {
            let uuid = r.uuid()?;
            let mut entry = PlayerInfoEntry {
                uuid,
                name: None,
                game_mode: None,
                listed: None,
                latency: None,
                display_name: None,
                properties: None,
                chat_session: None,
                list_order: None,
                hat_visible: None,
            };
            // Fields appear in Action ordinal order for whichever bits are set.
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
                let component = read_network_nbt(r)?;
                entry.display_name = Some(Text::from_nbt(&component));
            }
            if has(action::UPDATE_LIST_ORDER) {
                entry.list_order = Some(r.var_i32()?);
            }
            if has(action::UPDATE_HAT) {
                entry.hat_visible = Some(r.bool()?);
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
        // Same cap as the update path above: a uuid is 16 bytes, so
        // `remaining()` bounds how many can actually follow.
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

    /// Builds the network-NBT encoding of a `{"text":"...","color":"#RRGGBB"}`
    /// component -- a two-field `TAG_Compound`, both `TAG_String`.
    fn nbt_hex_coloured_component(text: &str, hex: &str) -> Vec<u8> {
        fn string_field(w: &mut Writer, key: &[u8], value: &[u8]) {
            w.u8(8); // TAG_String
            w.u8(0);
            w.u8(key.len() as u8);
            w.bytes(key);
            w.u8((value.len() >> 8) as u8);
            w.u8((value.len() & 0xff) as u8);
            w.bytes(value);
        }
        let mut w = Writer::default();
        w.u8(10); // TAG_Compound, unnamed (network NBT drops the root name)
        string_field(&mut w, b"text", text.as_bytes());
        string_field(&mut w, b"color", hex.as_bytes());
        w.u8(0); // TAG_End
        w.into_vec()
    }

    /// A hex colour distinct from any legacy 16-colour value, so a decoder
    /// that only understood `§`-coded colours (or dropped colour entirely,
    /// as `plain_text_from_nbt_component` did) cannot pass this by accident.
    #[test]
    fn update_display_name_decodes_hex_colour_not_plain_text() {
        use lodestone_model::TextColor;

        let mut w = Writer::default();
        w.u8(1 << action::UPDATE_DISPLAY_NAME); // only this action bit set
        w.var_i32(1); // one entry
        w.uuid(Uuid::from_u128(1));
        w.bool(true); // UPDATE_DISPLAY_NAME present
        w.bytes(&nbt_hex_coloured_component("Nicked", "#ab12ef"));

        let update: PlayerInfoUpdate = decode_exact(&w.into_vec());
        let display_name = update.entries[0]
            .display_name
            .clone()
            .expect("UPDATE_DISPLAY_NAME bit was set");
        assert_eq!(
            display_name.style.color,
            Some(TextColor::Rgb(0x00ab_12ef)),
            "an explicit hex display-name colour must survive the decode, \
             got {:?} -- plain_text_from_nbt_component would report None here",
            display_name.style.color
        );
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
        // UPDATE_LIST_ORDER: a value distinct from every other numeric field
        // above, so a transposition inside the entry cannot survive.
        w.var_i32(7);
        // UPDATE_HAT: false, distinct from UPDATE_LISTED's `true` just above so
        // the two adjacent-in-spirit bools cannot coincide by chance.
        w.bool(false);

        let update: PlayerInfoUpdate = decode_exact(&w.into_vec());
        assert_eq!(update.entries.len(), 1);
        let e = &update.entries[0];
        assert_eq!(e.uuid, uuid);
        assert_eq!(e.name.as_deref(), Some("Notch"));
        assert_eq!(e.game_mode, Some(1));
        assert_eq!(e.listed, Some(true));
        assert_eq!(e.latency, Some(42));
        assert_eq!(
            e.display_name,
            Some(Text::literal("Notch!")),
            "a plain component still decodes to a plain literal"
        );
        assert_eq!(
            e.properties.as_deref(),
            Some(&[][..]),
            "ADD_PLAYER was present with zero properties -- Some(empty), not None"
        );
        assert_eq!(
            e.list_order,
            Some(7),
            "UPDATE_LIST_ORDER must be kept, not decoded-and-discarded"
        );
        assert_eq!(
            e.hat_visible,
            Some(false),
            "UPDATE_HAT must be kept, not decoded-and-discarded"
        );
    }

    /// `INITIALIZE_CHAT` used to be read purely to stay byte-aligned and then
    /// discarded (the old `skip_chat_session`); this is the control that it is
    /// now kept. Pairwise-distinct session UUID / expiry / public key /
    /// signature bytes so a field transposition inside `read_chat_session`
    /// would be visible, and a second entry with `INITIALIZE_CHAT` present but
    /// the nullability boolean `false` so both directions of that boolean are
    /// exercised in one test.
    #[test]
    fn initialize_chat_is_kept_not_discarded() {
        let with_session = Uuid::from_u128(1);
        let without_session = Uuid::from_u128(2);
        let session_id = Uuid::from_u128(0xaabb_ccdd_eeff_0011_2233_4455_6677_8899);
        let public_key: Vec<u8> = vec![0x30, 0x81, 0x9f, 0x02, 0x81, 0x81];
        let key_signature: Vec<u8> = vec![0x01, 0x02, 0x03, 0x04, 0x05];
        let expires_at: i64 = 1_700_000_000_123;

        let mut w = Writer::default();
        w.u8(1 << action::INITIALIZE_CHAT);
        w.var_i32(2);
        // Entry 1: a real chat session.
        w.uuid(with_session);
        w.bool(true);
        w.uuid(session_id);
        w.i64(expires_at);
        w.var_i32(public_key.len() as i32);
        w.bytes(&public_key);
        w.var_i32(key_signature.len() as i32);
        w.bytes(&key_signature);
        // Entry 2: `INITIALIZE_CHAT` present but no session announced.
        w.uuid(without_session);
        w.bool(false);

        let update: PlayerInfoUpdate = decode_exact(&w.into_vec());
        assert_eq!(update.entries.len(), 2);

        let session = update.entries[0]
            .chat_session
            .as_ref()
            .expect("entry 1 announced a session");
        assert_eq!(session.session_id, session_id);
        assert_eq!(session.expires_at, expires_at);
        assert_eq!(session.public_key, public_key);
        assert_eq!(session.key_signature, key_signature);

        assert_eq!(
            update.entries[1].chat_session, None,
            "the nullability boolean being false must decode to no session, not a default one"
        );
    }

    /// `read_add_player` used to consume all three fields of every
    /// property into `let _`, so `minecraft:textures` — the skin — was read off the
    /// wire and discarded, and no remote player could have one. The bytes were
    /// always right; nothing carried them.
    ///
    /// Two properties, one signed and one not, because the signature is a
    /// bool-prefixed optional *inside* the loop: getting that wrong mis-frames
    /// every property after the first, and a single-property test cannot see it.
    #[test]
    fn add_player_keeps_the_profile_properties_including_the_skin() {
        let uuid = Uuid::from_u128(7);
        let mut w = Writer::default();
        w.u8(1 << 0); // ADD_PLAYER only
        w.var_i32(1);
        w.uuid(uuid);
        w.string("Skinned");
        w.var_i32(2);
        // Property 1: signed, as an online-mode server sends `textures`.
        w.string("textures");
        w.string("eyJ0ZXh0dXJlcyI6e319");
        w.bool(true);
        w.string("MOJANG_SIGNATURE");
        // Property 2: unsigned, which is what makes the optional's framing
        // observable — a decoder that always read a signature would consume this
        // property's name as one.
        w.string("unsigned_prop");
        w.string("value");
        w.bool(false);

        let update: PlayerInfoUpdate = decode_exact(&w.into_vec());
        let properties = update.entries[0]
            .properties
            .as_ref()
            .expect("ADD_PLAYER was set, so properties must be Some");
        assert_eq!(properties.len(), 2, "the second property was mis-framed");
        assert_eq!(properties[0].name, "textures");
        assert_eq!(properties[0].value, "eyJ0ZXh0dXJlcyI6e319");
        assert_eq!(properties[0].signature.as_deref(), Some("MOJANG_SIGNATURE"));
        assert_eq!(properties[1].name, "unsigned_prop");
        assert_eq!(properties[1].signature, None);
    }

    /// The control for the merge rule: an update with **no** `ADD_PLAYER` action
    /// must report `None`, not `Some(vec![])`. The tab-list fold keys the
    /// keep-versus-clear decision on exactly that, so collapsing the two here
    /// would drop every remote skin on the next latency ping.
    #[test]
    fn an_update_without_add_player_reports_no_properties_rather_than_empty() {
        let mut w = Writer::default();
        w.u8(1 << 4); // UPDATE_LATENCY only
        w.var_i32(1);
        w.uuid(Uuid::from_u128(3));
        w.var_i32(15);

        let update: PlayerInfoUpdate = decode_exact(&w.into_vec());
        assert_eq!(update.entries[0].properties, None);
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
