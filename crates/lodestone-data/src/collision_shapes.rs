//! Block collision shapes for protocol 776 (Minecraft 26.2).
//!
//! Physics and navigation need the *collision* geometry of each block state:
//! the set of axis-aligned boxes a block occupies, in block-local coordinates
//! (`0.0..1.0` per axis, though fences/walls reach `y = 1.5`). This is not in
//! `blocks.json` — that report is block *properties* only; vanilla collision is
//! **code**-defined (vanilla's own "get collision shape" accessor, often neighbour-state-dependent
//! for stairs/fences/walls/panes). So there is no property-derived shortcut: the
//! only authoritative source is the game itself.
//!
//! # Data source: interrogate the real jar, not `minecraft-data`
//!
//! The table is generated from an authoritative dump produced by booting the
//! real 26.2 server headlessly (through vanilla's own version-detection and
//! bootstrap entry points) and walking every block state, dumping
//! its own collision-shape accessor's AABB decomposition for all 32,366 states. That dump is
//! version-exact and complete. The obvious third-party alternative,
//! `vendor/minecraft-data/blockCollisionShapes.json`, is stale for 26.2 (newest
//! pc entry 1.21.11): only ~92.3% of states reliably covered, ~7.7%
//! fallback/suspect, 30 blocks missing by name — i.e. silently wrong geometry on
//! possibly-common blocks. "Boot the jar and ask it" is the preferred data
//! source generally; see the crate report.
//!
//! # Memory design
//!
//! Identical to the block-state table: pure rodata, zero heap, O(1) by id.
//!
//! * The 32,366 states collapse to **326 distinct shapes** (most blocks are a
//!   full cube or empty), so a state maps to a `u16` shape index —
//!   `STATE_SHAPE: [u16; 32_366]` (~63 KiB).
//! * The 326 distinct shapes reference **716 de-duplicated boxes** total, held
//!   in `SHAPES: [&[Aabb]; 326]` pointing into rodata (~22 KiB of [`Aabb`] plus
//!   ~5 KiB of slice headers).
//!
//! Coordinates are [`f32`]: every one of the 26 distinct coordinate values in
//! the dump is *exactly* representable in `f32` (verified in the drift test), so
//! this is lossless versus the game's `double`s while halving the rodata. A
//! consumer wanting `f64` can widen at the seam — `f32 -> f64` is exact.

use crate::{block_states::StateId, generated_collision_shapes as table};

pub use table::STATE_COUNT;

/// An axis-aligned collision box in block-local coordinates.
///
/// This **is** [`lodestone_model::BlockAabb`], not a copy of it. The table used
/// to own its own struct, which meant the only way to hand shapes across the
/// version seam ([`VersionAdapter::block_collision`]) was to convert box by box
/// — turning a rodata slice into a per-query allocation, in the innermost loop of
/// the physics tick. Sharing one type identity makes the seam return
/// `&'static [Aabb]` straight out of rodata instead. The alias keeps this
/// module's own name working, so the generated table and every existing consumer
/// are unchanged.
///
/// [`VersionAdapter::block_collision`]: lodestone_model::VersionAdapter::block_collision
pub type Aabb = lodestone_model::BlockAabb;

/// The collision boxes for a validated block-state `id`.
///
/// An empty slice is a valid, meaningful result: the block has no collision
/// (e.g. air, water, lava, cobweb). Zero-heap: returns a `&'static [Aabb]`
/// straight from rodata. O(1) indexing, no search.
#[must_use]
pub fn collision_boxes(id: StateId) -> &'static [Aabb] {
    let shape = table::STATE_SHAPE[id.raw() as usize];
    table::SHAPES[shape as usize]
}
