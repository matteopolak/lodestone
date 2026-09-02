//! Per-biome and per-dimension ambient sounds, as a generated static table.
//!
//! # What it is
//!
//! The lookup that answers "which [`AmbientSounds`] is in force where the player is
//! standing". Two layers, because vanilla uses two:
//!
//! * **Dimensions** carry the cave mood. Vanilla's own legacy-cave
//!   ambient-sound settings are set
//!   on the overworld dimension type and on the End (both in
//!   vanilla's own dimension-type bootstrap). The **Nether dimension sets nothing**.
//! * **Biomes** override it wholesale. Exactly five do — the Nether's — each with its
//!   own loop, mood and additions (vanilla's own per-nether-biome ambient-sound setters).
//!
//! That split is the thing to get right, and getting it wrong fails silently in
//! whichever direction you lean: a biome-only lookup finds cave ambience in **zero**
//! biomes and concludes the feature is absent, while a dimension-only lookup gives
//! every Nether biome cave ambience and none of its own loop. Use
//! [`ambient_sounds_at`], which composes both.
//!
//! # Where the data comes from
//!
//! The biome half is generated from
//! `crates/lodestone-server/assets/worldgen/biome/*.json` — the same vanilla-derived
//! files the music table uses, and equally unread before this: every consumer of
//! `EmbeddedResolver::biome_document` looks only at `carvers` and `features`.
//!
//! ```text
//! LODESTONE_REGEN=1 cargo test -p lodestone-sound --test biome_ambient_table
//! ```
//!
//! The dimension half is **not** generated, because it is not in our assets — there
//! is no `dimension_type/*.json` in this repo — so it is transcribed from
//! vanilla's own dimension-type bootstrap, cited on [`DIMENSION_AMBIENT`]. That is a
//! weaker evidence standard than the generated half, and it is called out here rather
//! than hidden: if a dimension-type asset dump ever lands, this should become
//! generated too.

use crate::ambient::AmbientSounds;

mod table;

pub use table::BIOME_AMBIENT;

/// Ambient sounds by dimension id, transcribed from vanilla's own dimension-type bootstrap.
///
/// * `overworld` — vanilla's own legacy-cave ambient-sound settings
/// * `the_end` — vanilla's own legacy-cave ambient-sound settings
/// * `the_nether` — **absent on purpose**: the Nether dimension type sets no
///   `AMBIENT_SOUNDS` attribute, so it falls through to
///   [`AmbientSounds::EMPTY`] and its biomes supply everything.
pub static DIMENSION_AMBIENT: &[(&str, AmbientSounds)] = &[
    ("overworld", AmbientSounds::LEGACY_CAVE),
    ("the_end", AmbientSounds::LEGACY_CAVE),
];

/// Strips a `minecraft:` (or any) namespace from a resource id.
fn strip_namespace(id: &str) -> &str {
    id.split_once(':').map_or(id, |(_, path)| path)
}

/// The biome's own [`AmbientSounds`], if it declares any. Accepts `nether_wastes` or
/// `minecraft:nether_wastes`.
pub fn biome_ambient(id: &str) -> Option<&'static AmbientSounds> {
    let key = strip_namespace(id);
    BIOME_AMBIENT
        .binary_search_by(|(name, _)| (*name).cmp(key))
        .ok()
        .map(|i| &BIOME_AMBIENT[i].1)
}

/// The dimension's [`AmbientSounds`], if it declares any.
pub fn dimension_ambient(id: &str) -> Option<&'static AmbientSounds> {
    let key = strip_namespace(id);
    DIMENSION_AMBIENT
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, sounds)| sounds)
}

/// The [`AmbientSounds`] in force for a player in `biome` within `dimension`.
///
/// The biome wins wholesale when it declares the attribute, because environment
/// attributes override rather than merge — see [`AmbientSounds::resolve`]. Falls back
/// to the dimension's value, then to [`AmbientSounds::EMPTY`].
pub fn ambient_sounds_at(dimension: &str, biome: &str) -> AmbientSounds {
    let dim = dimension_ambient(dimension).cloned().unwrap_or(AmbientSounds::EMPTY);
    AmbientSounds::resolve(biome_ambient(biome), &dim)
}

/// Number of biomes in the generated table, for callers that want to assert it was
/// generated rather than emptied.
pub fn table_len() -> usize {
    BIOME_AMBIENT.len()
}
