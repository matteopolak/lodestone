//! Per-biome background music, as a generated static table.
//!
//! # What it is
//!
//! A lookup from biome id (`jungle`, or `minecraft:jungle`) to the
//! [`BackgroundMusic`] that biome's `minecraft:audio/background_music` attribute
//! carries, plus its optional `minecraft:audio/music_volume`. This is the input
//! [`MusicSituation::background_music`](crate::music::MusicSituation::background_music)
//! needs, and without it every biome would fall back to the same overworld track.
//!
//! # Where the data comes from, and why it is a table rather than a parse
//!
//! The values are **already in this repository** — `crates/lodestone-server/`
//! `assets/worldgen/biome/*.json`, 66 files, vanilla-derived. 42 of them carry
//! `minecraft:audio/background_music`; one (`pale_garden`) also carries
//! `minecraft:audio/music_volume: 0.0`. Nothing read them before this module:
//! `EmbeddedResolver::biome_document` exposes the whole document, but every
//! consumer only looks at `carvers` and `features`, so the `attributes` map was
//! parsed by no Rust code at all.
//!
//! Those files are embedded into `lodestone-server`, which a client must not
//! depend on, so the data is transcribed here **by a generator rather than by
//! hand** — see `tests/biome_music_table.rs`, which regenerates
//! `biome_music/table.rs` from the JSON and fails if the committed file has
//! drifted. That is the same generate-or-assert shape as
//! `lodestone-data`'s collision-shape and hardness tables:
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-sound --test biome_music_table
//! ```
//!
//! The point of the generator is the evidence standard: a hand-transcribed table
//! (which is the existing precedent in `lodestone-assets`' `tint.rs`) has no
//! oracle outside itself, whereas this one is checked against vanilla's own data
//! on every `cargo test`.
//!
//! # Gotchas
//!
//! * **Ids here are namespace-stripped**, and so are the sound keys inside them
//!   (`music.overworld.jungle`, not `minecraft:music.overworld.jungle`) — the
//!   latter because that is what the sound resolver wants. [`biome_music`]
//!   accepts either form of biome id.
//! * **A biome absent from the table is not "no music"** — it is "no *biome*
//!   music", and the caller should fall back to
//!   [`BackgroundMusic::overworld`](crate::music::BackgroundMusic::overworld) in
//!   the overworld. A biome that sets no `BACKGROUND_MUSIC` attribute leaves
//!   `EnvironmentAttributeMap.applyModifier`'s base value untouched, so vanilla's
//!   layered attribute system falls through to the dimension's own value
//!   (`BackgroundMusic.OVERWORLD`, set on the overworld dimension type in
//!   `DimensionTypes.bootstrap`) rather than any per-biome helper.
//!   `pale_garden` is the one biome that means
//!   *genuinely* no music, and it says so with a present-but-empty entry, which
//!   is why [`biome_music`] returns `Some(&EMPTY)` for it rather than `None`.
//!   Collapsing those two cases silences 24 biomes.
//! * The table is sorted by id and searched with [`slice::binary_search_by`]; the
//!   generator emits it sorted and the test asserts the ordering, so a hand edit
//!   that breaks the sort is caught rather than silently missing lookups.
//! * `music_volume` is a *fade target*, not a mute — see
//!   [`MusicManager::tick`](crate::music::MusicManager::tick).

use crate::music::BackgroundMusic;

mod table;

pub use table::BIOME_MUSIC;

/// One biome's audio-attribute values.
#[derive(Debug)]
pub struct BiomeMusic {
    /// The biome's `minecraft:audio/background_music`.
    pub music: BackgroundMusic,
    /// The biome's `minecraft:audio/music_volume`, when it sets one. `None` means
    /// the attribute's default of 1.0.
    pub music_volume: Option<f32>,
}

/// Strips a `minecraft:` (or any) namespace from a resource id.
fn strip_namespace(id: &str) -> &str {
    id.split_once(':').map_or(id, |(_, path)| path)
}

/// The biome's audio attributes, or `None` if it declares none.
///
/// Accepts `jungle` or `minecraft:jungle`.
pub fn biome_audio(id: &str) -> Option<&'static BiomeMusic> {
    let key = strip_namespace(id);
    BIOME_MUSIC
        .binary_search_by(|(name, _)| (*name).cmp(key))
        .ok()
        .map(|i| &BIOME_MUSIC[i].1)
}

/// The biome's [`BackgroundMusic`], or `None` if it declares none.
///
/// Remember that `Some(&BackgroundMusic::EMPTY)` (`pale_garden`) and `None` (a
/// biome with no attribute) mean different things: the former is "this biome has
/// no music", the latter is "this biome adds nothing, use the dimension default".
pub fn biome_music(id: &str) -> Option<&'static BackgroundMusic> {
    biome_audio(id).map(|a| &a.music)
}

/// The biome's `audio/music_volume`, defaulting to 1.0 as the attribute does.
pub fn biome_music_volume(id: &str) -> f32 {
    biome_audio(id)
        .and_then(|a| a.music_volume)
        .unwrap_or(1.0)
}

/// Convenience for the common caller shape: the effective [`BackgroundMusic`] for
/// a biome in the overworld, falling back to
/// [`BackgroundMusic::overworld`] when the biome sets no attribute.
///
/// Kept out of [`biome_music`] deliberately — the fallback is dimension-specific
/// (the Nether's biomes all set the attribute explicitly, and the End's do not use
/// this path at all), so baking it into the lookup would be wrong for two of the
/// three dimensions.
pub fn overworld_music_for(id: &str) -> BackgroundMusic {
    biome_music(id)
        .cloned()
        .unwrap_or_else(BackgroundMusic::overworld)
}

/// Number of biomes in the generated table. A guard for callers that want to
/// assert the table was actually generated rather than emptied.
pub fn table_len() -> usize {
    BIOME_MUSIC.len()
}
