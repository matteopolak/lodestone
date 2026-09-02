//! Packets shared by the configuration and play states for protocol 776.

use lodestone_macros::{Decode, Encode};
use uuid::Uuid;

/// The `keep_alive` packet body, identical in both directions and in both the
/// configuration and play states: a single big-endian 64-bit id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct KeepAlive {
    /// Challenge id that must be echoed back unchanged.
    pub id: i64,
}

/// The `ping`/`pong` packet body, identical in both directions and in both the
/// configuration and play states: a single big-endian 32-bit id, distinct from
/// [`KeepAlive`]'s 64-bit id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct Pong {
    /// Challenge id echoed back from the corresponding `ping`.
    pub id: i32,
}

/// The `ping_request` packet body, shared between the status and play
/// states (vanilla's own serverbound ping-request packet):
/// a single big-endian 64-bit client clock reading. In play, vanilla sends
/// this periodically from the F3 debug overlay's network graph
/// (its own ping-debug monitor), independent of [`Pong`]'s server-initiated
/// challenge/response.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct PingRequest {
    /// Client's local clock reading in milliseconds.
    pub time: i64,
}

/// Serverbound `teleport_to_entity` packet.
///
/// Sent while spectating to teleport to an entity by uuid, e.g. clicking a
/// player in the tab list (`ServerboundTeleportToEntityPacket`). Wire
/// layout: a single raw 16-byte UUID, not a VarInt entity id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct TeleportToEntity {
    /// Uuid of the entity to teleport to.
    pub uuid: Uuid,
}

/// Serverbound `custom_payload` packet body for the `minecraft:brand` channel.
///
/// Wire layout: the channel identifier (a UTF string, since `writeIdentifier`
/// writes the same VarInt-length-prefixed UTF-8 as `writeUtf`), then the
/// brand-specific payload, which for `BrandPayload` is a single UTF string.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct BrandPayload {
    /// Plugin channel identifier; always `minecraft:brand` for this payload.
    pub channel: String,
    /// Free-form client brand string, such as `vanilla`.
    pub brand: String,
}

/// Serverbound `resource_pack` response packet.
///
/// Wire layout: a raw 16-byte UUID identifying the pack, then a VarInt
/// `Action` enum ordinal (`0` successfully_loaded … `7` discarded).
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct ResourcePackResponse {
    /// Id of the resource pack this response concerns.
    pub id: Uuid,
    /// Outcome ordinal (see the type's doc comment for the enum values).
    #[mc(varint)]
    pub action: i32,
}

/// Serverbound `cookie_response`, shared by the Login, Configuration and Play
/// states — vanilla's own server cookie-packet listener is common to all
/// three (confirmed against the decompiled 26.2 source).
///
/// Wire layout: the cookie key (a UTF string — vanilla's own identifier
/// writer writes the same VarInt-length-prefixed UTF-8 as its plain string
/// writer, matching [`BrandPayload`]'s
/// `channel`), then the payload as a nullable byte array
/// (vanilla's own nullable wrapper over its own byte-array codec, capped at
/// 5120): a bool presence flag,
/// and if `true`, a VarInt length followed by that many raw bytes. Both
/// halves fall out of `lodestone-core`'s blanket `Option<T>: Encode` and
/// `Vec<u8>: Encode` impls with no `#[mc(...)]` needed — the payload is
/// already capped at 5120 bytes by the clientbound `store_cookie` decoder
/// that produced it, not re-checked here.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct CookieResponse {
    /// Cookie key, echoed from the `cookie_request` this answers.
    pub key: String,
    /// The previously stored cookie payload, or `None` if this client has
    /// none for `key`.
    pub payload: Option<Vec<u8>>,
}

/// Serverbound `client_information` packet describing client settings.
///
/// Wire layout: string language (max 16 chars), signed byte view distance,
/// varint chat visibility, boolean chat colors, unsigned byte skin model
/// customisation bitmask, varint main hand, boolean text filtering, boolean
/// allow server listings, varint particle status.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode)]
pub struct ClientInformation {
    /// Client locale, such as `en_us`.
    #[mc(max = 16)]
    pub language: String,
    /// Requested render distance in chunks.
    pub view_distance: i8,
    /// Chat visibility mode (`0` full, `1` commands only, `2` hidden).
    #[mc(varint)]
    pub chat_visibility: i32,
    /// Whether chat colors are enabled.
    pub chat_colors: bool,
    /// Displayed skin part bitmask.
    pub model_customization: u8,
    /// Dominant hand (`0` left, `1` right).
    #[mc(varint)]
    pub main_hand: i32,
    /// Whether the client filters text via a partner service.
    pub text_filtering: bool,
    /// Whether the client allows appearing in server player-list samples.
    pub allows_listing: bool,
    /// Particle rendering level (`0` all, `1` decreased, `2` minimal).
    #[mc(varint)]
    pub particle_status: i32,
}

impl Default for ClientInformation {
    fn default() -> Self {
        Self {
            language: "en_us".to_owned(),
            view_distance: 8,
            chat_visibility: 0,
            chat_colors: true,
            model_customization: 0,
            main_hand: 1,
            text_filtering: false,
            allows_listing: false,
            particle_status: 0,
        }
    }
}
