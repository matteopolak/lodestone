//! Client settings and player abilities for protocol 5.

use lodestone_macros::{Decode, Encode, Packet};

/// Serverbound `settings`.
///
/// # Differences from protocol 47's
///
/// Two, and the first is easy to miss. `chat_flags` here is a raw byte whose
/// low bits are the chat mode *and* whose `0x08` bit is the colours flag,
/// where protocol 47 split colours into its own boolean. And the trailing
/// field is a lone `difficulty` byte rather than protocol 47's displayed-skin
/// bitmask, so a skin-parts value sent here is read as a difficulty.
///
/// `show_cape` is the whole of this era's skin customisation: one boolean,
/// for the cape only.
#[derive(Debug, Clone, PartialEq, Eq, Encode, Decode, Packet)]
#[mc(name = "minecraft:settings", state = Play, bound = Server)]
pub struct Settings {
    /// Language and region, such as `en_GB`.
    #[mc(max = 16)]
    pub locale: String,
    /// Render distance in chunks.
    pub view_distance: i8,
    /// Packed chat mode and colour flag.
    pub chat_flags: i8,
    /// Whether chat colours are enabled.
    pub chat_colors: bool,
    /// Client-selected difficulty.
    pub difficulty: i8,
    /// Whether the player's cape is shown.
    pub show_cape: bool,
}

/// Clientbound `abilities`.
///
/// Byte-identical to protocol 47's, but not shared: the shared definition's
/// module docs record that its three-crate range was measured on the
/// *serverbound* pair as well, and this era's serverbound `abilities` carries
/// two extra flags. Splitting one of a matched pair off into a shared crate
/// invites the other to follow it, so both stay here.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:abilities", state = Play, bound = Client)]
pub struct PlayerAbilities {
    /// Bit flags: `0x01` invulnerable, `0x02` flying, `0x04` may fly,
    /// `0x08` creative-mode instant break.
    pub flags: i8,
    /// Flying speed.
    pub flying_speed: f32,
    /// Walking speed.
    pub walking_speed: f32,
}

/// Serverbound `abilities`.
///
/// Carries the full ability set back to the server, including two booleans
/// that protocol 47 folded into the flags byte.
#[derive(Debug, Clone, Copy, PartialEq, Encode, Decode, Packet)]
#[mc(name = "minecraft:abilities", state = Play, bound = Server)]
pub struct ServerboundAbilities {
    /// Bit flags, as in [`PlayerAbilities`].
    pub flags: i8,
    /// Flying speed.
    pub flying_speed: f32,
    /// Walking speed.
    pub walking_speed: f32,
}
