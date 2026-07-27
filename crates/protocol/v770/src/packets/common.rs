//! Packets shared by the configuration and play states for protocol 776.

use lodestone_macros::{Decode, Encode};

/// The `keep_alive` packet body, identical in both directions and in both the
/// configuration and play states: a single big-endian 64-bit id.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Encode, Decode)]
pub struct KeepAlive {
    /// Challenge id that must be echoed back unchanged.
    pub id: i64,
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
