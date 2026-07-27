//! Sky- and block-light propagation.
//!
//! This is the *engine* half of lighting: given a chunk's blocks it computes the
//! sky and block light every cell should hold, matching vanilla's results. It is
//! distinct from the *storage* half ([`crate::light`]) and from the network
//! [`LightPatch`](crate::LightPatch) path, which merely accepts light a real
//! server already computed. The engine only earns its keep where no server does
//! that for us — singleplayer and worldgen — but it is the sole source of truth
//! there, so a placeholder that merely *looks* lit is a silent bug.
//!
//! # Algorithm (read out of 26.2, not memory)
//!
//! Both layers share one propagation rule, taken from
//! `net/minecraft/world/level/lighting/{LightEngine,BlockLightEngine,SkyLightEngine}`:
//! a cell at level `l` lifts a neighbour to `l - opacity(neighbour)`, where
//! `opacity = max(1, lightDampening(neighbour))` (`LightEngine.getOpacity`), and
//! only when that is an improvement. Processed in descending level order (the
//! 15-bucket queue vanilla uses), one pass settles every cell at its maximum.
//!
//! The two layers differ only in their *sources*:
//!
//! * **Block light** seeds every cell whose block emits (`getLightEmission`) at
//!   that emission level.
//! * **Sky light** seeds every cell open to the sky at `15`. "Open" is the whole
//!   vertical column from the top down to — but not including — the first block
//!   that dampens light at all (`ChunkSkyLightSources.isEdgeOccluded`, whose
//!   scalar case is `dampening != 0`). This is *why* sky light appears to fall
//!   vertically without attenuating: it is not a special vertical rule, it is
//!   that every open cell is itself a full-strength source. Horizontal spread
//!   (under an overhang, into a cave mouth) then decays by the ordinary rule —
//!   the vertical/horizontal asymmetry that guessing gets wrong.
//!
//! `dampening` and `emission` come from an injected [`LightProperties`], so this
//! crate stays version- and registry-free: the value crate or the client hands
//! in a provider backed by the block registry rather than us hardcoding a table.
//!
//! # Seams
//!
//! Propagation runs over one whole chunk column (all 16×16 columns at once), so
//! *intra*-chunk horizontal spread is exact. Light entering from a neighbouring
//! chunk is not pulled in, so cells within 15 of an x/z border can under-report
//! by a neighbour's contribution; the correctness gate compares interior cells,
//! and a neighbour-aware volume is the next increment behind this same
//! interface.
//!
//! # Removal
//!
//! [`compute_column_light`] computes from zero, so it is correct by construction
//! after a block change (no stale bright cells — the trap a naive re-flood falls
//! into). Incremental decrease/re-increase BFS is a performance optimisation to
//! add *later, with a measurement*, behind this same function's interface.

use crate::light::{ColumnLight, LightData, NibbleArray};
use crate::section::ChunkSection;

/// Per-block-state light properties, injected so this crate needs no block
/// registry of its own.
///
/// Both values are keyed by the **global block-state id** (the same `u32` the
/// chunk containers store), because opacity and emission are properties of the
/// block *state*, not the block — a lit vs unlit furnace, or an open vs closed
/// shulker, differ.
pub trait LightProperties {
    /// How much light the state removes as light passes *into* it, `0..=15`
    /// (vanilla's `lightDampening`). Air and fully transparent blocks are `0`;
    /// full solids are `15`. The engine applies `max(1, ·)` itself, so even a
    /// `0`-dampening block still costs one level to cross — return the raw
    /// dampening here, not the stepped opacity.
    fn opacity(&self, state: u32) -> u8;

    /// How much light the state emits, `0..=15` (vanilla's `getLightEmission`).
    /// Non-emitters return `0`.
    fn emission(&self, state: u32) -> u8;
}

/// Read-only block access for the engine, in chunk-local `x`/`z` (`0..16`) and
/// world `y`.
///
/// [`ChunkColumn`](crate::ChunkColumn) implements this directly; a future
/// neighbour-aware implementation can answer for a 3×3 column neighbourhood
/// without changing the engine.
pub trait BlockVolume {
    /// Block-state id at chunk-local `(x, z)` and world `y`. Must return the air
    /// id for any `y` outside the built column (the engine reads one section of
    /// apron above and below).
    fn block(&self, x: usize, y: i32, z: usize) -> u32;

    /// Lowest world `y` of the built column.
    fn min_y(&self) -> i32;

    /// Number of block sections in the built column.
    fn section_count(&self) -> usize;
}

impl BlockVolume for crate::ChunkColumn {
    fn block(&self, x: usize, y: i32, z: usize) -> u32 {
        self.get_block(x, y, z)
    }
    fn min_y(&self) -> i32 {
        crate::ChunkColumn::min_y(self)
    }
    fn section_count(&self) -> usize {
        crate::ChunkColumn::section_count(self)
    }
}

const EDGE: usize = ChunkSection::EDGE; // 16
const AREA: usize = EDGE * EDGE; // 256 cells per y-layer
const MAX_LIGHT: u8 = 15;

/// Computes sky and block light for one chunk column, returning a fully
/// populated [`ColumnLight`] spanning `section_count + 2` light sections (one
/// apron section below the world and one above, exactly as a `light_update`
/// packet carries).
///
/// This is the whole public surface of the engine. It is deterministic and
/// depends only on `blocks` and `props`, so it is trivially testable against a
/// live server's light for the same chunk.
#[must_use]
pub fn compute_column_light(blocks: &impl BlockVolume, props: &impl LightProperties) -> ColumnLight {
    let section_count = blocks.section_count();
    let min_y = blocks.min_y();
    // Light sections span [section_count + 2]; the field covers that whole range
    // so the apron (open sky above the build limit, dark below it) is computed,
    // not assumed. `y_rel` 0 maps to the bottom of light section 0.
    let light_sections = section_count + 2;
    let height = light_sections * EDGE;
    let field_bottom_y = min_y - EDGE as i32;

    // Cache opacity per cell once; the BFS reads it many times per cell.
    let mut opacity = vec![0u8; AREA * height];
    for y_rel in 0..height {
        let world_y = field_bottom_y + y_rel as i32;
        for z in 0..EDGE {
            for x in 0..EDGE {
                let state = blocks.block(x, world_y, z);
                opacity[cell(x, y_rel, z)] = props.opacity(state).min(MAX_LIGHT);
            }
        }
    }

    let sky = compute_sky(&opacity, height);
    let block = compute_block(blocks, props, field_bottom_y, height);

    pack(section_count, light_sections, height, &sky, &block)
}

/// Flat index into a `AREA * height` field. `y` is the outer axis so a whole
/// horizontal layer is contiguous; the exact order is internal (only the packed
/// output must match the wire nibble order).
#[inline]
fn cell(x: usize, y_rel: usize, z: usize) -> usize {
    (y_rel * AREA) + (z * EDGE) + x
}

/// Sky light: seed every cell open to the sky at 15, then propagate.
fn compute_sky(opacity: &[u8], height: usize) -> Vec<u8> {
    let mut level = vec![0u8; AREA * height];
    let mut buckets = Buckets::new();

    for z in 0..EDGE {
        for x in 0..EDGE {
            // Scan down from the top of the field. Every cell is a full-strength
            // source until the first cell that dampens light at all; that cell
            // and everything below get their light from propagation instead.
            for y_rel in (0..height).rev() {
                let idx = cell(x, y_rel, z);
                if opacity[idx] != 0 {
                    break;
                }
                level[idx] = MAX_LIGHT;
                buckets.push(MAX_LIGHT, idx as u32);
            }
        }
    }

    propagate(&mut level, opacity, height, &mut buckets);
    level
}

/// Block light: seed every emitting cell at its emission, then propagate.
fn compute_block(
    blocks: &impl BlockVolume,
    props: &impl LightProperties,
    field_bottom_y: i32,
    height: usize,
) -> Vec<u8> {
    let mut level = vec![0u8; AREA * height];
    let mut opacity = vec![0u8; AREA * height];
    let mut buckets = Buckets::new();

    for y_rel in 0..height {
        let world_y = field_bottom_y + y_rel as i32;
        for z in 0..EDGE {
            for x in 0..EDGE {
                let state = blocks.block(x, world_y, z);
                let idx = cell(x, y_rel, z);
                opacity[idx] = props.opacity(state).min(MAX_LIGHT);
                let emission = props.emission(state).min(MAX_LIGHT);
                if emission > 0 {
                    // A source cell holds its emission regardless of its own
                    // opacity (a jack-o'-lantern is opaque yet lit).
                    level[idx] = emission;
                    buckets.push(emission, idx as u32);
                }
            }
        }
    }

    propagate(&mut level, &opacity, height, &mut buckets);
    level
}

/// The shared descending-level flood: drain level 15 down to 1, lifting each
/// in-bounds neighbour to `l - max(1, opacity(neighbour))` when that improves
/// it. A cell popped at a level below its current value is stale and skipped.
fn propagate(level: &mut [u8], opacity: &[u8], height: usize, buckets: &mut Buckets) {
    let mut l = MAX_LIGHT;
    while l >= 1 {
        while let Some(idx) = buckets.pop(l) {
            let idx = idx as usize;
            if level[idx] != l {
                continue;
            }
            let (x, y_rel, z) = uncell(idx);
            for (nx, ny, nz) in neighbours(x, y_rel, z, height) {
                let nidx = cell(nx, ny, nz);
                let cost = opacity[nidx].max(1);
                let new = l.saturating_sub(cost);
                if new > level[nidx] {
                    level[nidx] = new;
                    buckets.push(new, nidx as u32);
                }
            }
        }
        l -= 1;
    }
}

/// The (up to) six in-bounds orthogonal neighbours. Horizontal moves outside
/// `0..16` are dropped — the documented chunk seam.
#[inline]
fn neighbours(x: usize, y: usize, z: usize, height: usize) -> impl Iterator<Item = (usize, usize, usize)> {
    let mut out: [Option<(usize, usize, usize)>; 6] = [None; 6];
    if x > 0 {
        out[0] = Some((x - 1, y, z));
    }
    if x + 1 < EDGE {
        out[1] = Some((x + 1, y, z));
    }
    if z > 0 {
        out[2] = Some((x, y, z - 1));
    }
    if z + 1 < EDGE {
        out[3] = Some((x, y, z + 1));
    }
    if y > 0 {
        out[4] = Some((x, y - 1, z));
    }
    if y + 1 < height {
        out[5] = Some((x, y + 1, z));
    }
    out.into_iter().flatten()
}

#[inline]
fn uncell(idx: usize) -> (usize, usize, usize) {
    let y = idx / AREA;
    let rem = idx % AREA;
    (rem % EDGE, y, rem / EDGE)
}

/// A 15-level bucket queue (level `0` is never queued: a cell at 0 propagates
/// nothing). LIFO within a level is fine — the descending-level order, not the
/// order within a level, is what makes one pass sufficient.
struct Buckets {
    levels: [Vec<u32>; 16],
}

impl Buckets {
    fn new() -> Self {
        Self {
            levels: std::array::from_fn(|_| Vec::new()),
        }
    }
    #[inline]
    fn push(&mut self, level: u8, idx: u32) {
        self.levels[level as usize].push(idx);
    }
    #[inline]
    fn pop(&mut self, level: u8) -> Option<u32> {
        self.levels[level as usize].pop()
    }
}

/// Slices the two flat fields into per-light-section [`LightData`], collapsing
/// uniform sections to a tag.
fn pack(
    section_count: usize,
    light_sections: usize,
    height: usize,
    sky: &[u8],
    block: &[u8],
) -> ColumnLight {
    debug_assert_eq!(height, light_sections * EDGE);
    let mut out = ColumnLight::new(section_count);
    for s in 0..light_sections {
        *out.sky_mut(s) = pack_section(sky, s);
        *out.block_mut(s) = pack_section(block, s);
    }
    out
}

/// Packs the 16³ cells of light section `s` into a [`LightData`], using vanilla's
/// `y<<8 | z<<4 | x` nibble order so it round-trips the wire exactly.
fn pack_section(field: &[u8], s: usize) -> LightData {
    let base_y = s * EDGE;
    let mut bytes = [0u8; 2048];
    for y_local in 0..EDGE {
        for z in 0..EDGE {
            for x in 0..EDGE {
                let value = field[cell(x, base_y + y_local, z)] & 0x0F;
                let nibble = (y_local << 8) | (z << 4) | x;
                bytes[nibble >> 1] |= value << (4 * (nibble & 1));
            }
        }
    }
    LightData::from_array(NibbleArray::from_bytes(&bytes).expect("2048 bytes"))
}

/// The outcome of diffing our computed light against another source (typically a
/// live server's `light_update`), reported as **counts, not a boolean** — "0 of
/// N cells differ" is evidence a gate ran; "passed" is not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct LightDiff {
    /// Cells actually compared (the oracle skips light sections the other source
    /// elided, since an absent section asserts nothing to check against).
    pub cells_compared: usize,
    /// Compared sky-light cells whose values disagree.
    pub sky_disagreements: usize,
    /// Compared block-light cells whose values disagree.
    pub block_disagreements: usize,
}

impl LightDiff {
    /// Total disagreeing cells across both layers.
    #[must_use]
    pub fn disagreements(&self) -> usize {
        self.sky_disagreements + self.block_disagreements
    }

    /// Whether every compared cell agreed. A caller must still check
    /// [`cells_compared`](LightDiff::cells_compared) is non-zero — agreement over
    /// zero cells is the vacuous pass this project keeps finding.
    #[must_use]
    pub fn agrees(&self) -> bool {
        self.disagreements() == 0
    }
}

/// Diffs two column lights cell-by-cell, restricted to the interior columns more
/// than `interior_margin` blocks from the x/z chunk border.
///
/// The margin exists because this engine does not pull light in from neighbouring
/// chunks (see the module seam note), so border cells can legitimately disagree
/// with a server that lit them with neighbours loaded.
///
/// A column is [`EDGE`] (16) wide, so no interior cell is more than 7 blocks from
/// a border: **the margin must be less than `EDGE / 2` (8)**, or both axis loops
/// collapse to empty and the diff compares zero cells — a vacuous pass that looks
/// exactly like agreement. A `debug_assert` enforces this. A margin of `0`
/// compares everything and is *exact* for uniform terrain, where every column is
/// identical so no neighbour chunk could have contributed anything to exclude;
/// use a small non-zero margin only when a horizontal light gradient makes the
/// outermost columns depend on unseen neighbours. Light sections the `server`
/// source left [`Missing`](LightData::Missing) are skipped — an elided section is
/// not an assertion of zero, so there is nothing to check there.
#[must_use]
pub fn diff_column_light(ours: &ColumnLight, server: &ColumnLight, interior_margin: usize) -> LightDiff {
    debug_assert!(
        interior_margin < EDGE / 2,
        "interior_margin {interior_margin} >= EDGE/2 ({}) compares zero cells — a vacuous pass",
        EDGE / 2
    );
    let mut diff = LightDiff::default();
    let sections = ours.light_section_count().min(server.light_section_count());
    let lo = interior_margin;
    let hi = EDGE.saturating_sub(interior_margin);
    for s in 0..sections {
        for y in 0..EDGE {
            for z in lo..hi {
                for x in lo..hi {
                    let idx = NibbleArray::index(x, y, z);
                    if let Some(theirs) = server.sky(s).get(idx) {
                        diff.cells_compared += 1;
                        if ours.sky(s).get(idx).unwrap_or(0) != theirs {
                            diff.sky_disagreements += 1;
                        }
                    }
                    if let Some(theirs) = server.block(s).get(idx) {
                        diff.cells_compared += 1;
                        if ours.block(s).get(idx).unwrap_or(0) != theirs {
                            diff.block_disagreements += 1;
                        }
                    }
                }
            }
        }
    }
    diff
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{ChunkColumn, PaletteKind};
    use std::collections::HashMap;

    // Fixed test block ids and their properties.
    const AIR: u32 = 0;
    const STONE: u32 = 1; // full opaque
    const GLASS: u32 = 2; // transparent but present (dampening 0)
    const TORCH: u32 = 3; // emits 14, transparent
    const WATER: u32 = 4; // dampening 1

    struct FakeProps {
        opacity: HashMap<u32, u8>,
        emission: HashMap<u32, u8>,
    }

    impl FakeProps {
        fn new() -> Self {
            let mut opacity = HashMap::new();
            opacity.insert(STONE, 15);
            opacity.insert(WATER, 1);
            // AIR, GLASS, TORCH default to 0.
            let mut emission = HashMap::new();
            emission.insert(TORCH, 14);
            Self { opacity, emission }
        }
    }

    impl LightProperties for FakeProps {
        fn opacity(&self, state: u32) -> u8 {
            *self.opacity.get(&state).unwrap_or(&0)
        }
        fn emission(&self, state: u32) -> u8 {
            *self.emission.get(&state).unwrap_or(&0)
        }
    }

    /// A small column: min_y = -64, 4 sections (y = -64..0), air-filled.
    fn column() -> ChunkColumn {
        ChunkColumn::new(-64, 4, PaletteKind::block_states(), PaletteKind::biomes(), AIR, 0)
    }

    /// Reads sky light at world `(x, y, z)` from a computed column.
    fn sky_at(light: &ColumnLight, min_y: i32, x: usize, y: i32, z: usize) -> u8 {
        let s = ((y - (min_y - 16)) / 16) as usize;
        let yl = (y - (min_y - 16)).rem_euclid(16) as usize;
        light.sky(s).get(NibbleArray::index(x, yl, z)).unwrap()
    }
    fn block_at(light: &ColumnLight, min_y: i32, x: usize, y: i32, z: usize) -> u8 {
        let s = ((y - (min_y - 16)) / 16) as usize;
        let yl = (y - (min_y - 16)).rem_euclid(16) as usize;
        light.block(s).get(NibbleArray::index(x, yl, z)).unwrap()
    }

    #[test]
    fn sky_light_falls_vertically_without_attenuation() {
        let col = column();
        let light = compute_column_light(&col, &FakeProps::new());
        // An all-air column: every cell sees the sky, so every cell is 15 — a
        // per-step vertical decay would fail this at the bottom.
        for y in [-64, -40, -1] {
            assert_eq!(sky_at(&light, -64, 5, y, 9), 15, "open column stays 15 at y={y}");
        }
    }

    #[test]
    fn glass_does_not_shadow_sky_below_it() {
        let mut col = column();
        col.set_block(4, -20, 4, GLASS);
        let light = compute_column_light(&col, &FakeProps::new());
        // Dampening 0 ⇒ still a sky source below the glass, no shadow.
        assert_eq!(sky_at(&light, -64, 4, -21, 4), 15, "cell below glass keeps full sky");
    }

    #[test]
    fn a_solid_layer_blocks_sky_below_but_not_above() {
        let mut col = column();
        // A stone slab-plane across the whole chunk at y = -20.
        for z in 0..16 {
            for x in 0..16 {
                col.set_block(x, -20, z, STONE);
            }
        }
        let light = compute_column_light(&col, &FakeProps::new());
        assert_eq!(sky_at(&light, -64, 8, -19, 8), 15, "above the layer is open");
        assert_eq!(sky_at(&light, -64, 8, -20, 8), 0, "the opaque layer itself is dark");
        assert_eq!(sky_at(&light, -64, 8, -21, 8), 0, "sealed below the layer is dark");
    }

    #[test]
    fn sky_light_decays_horizontally_under_an_overhang() {
        let mut col = column();
        // A roof at y = -20 covering x = 8..16, leaving x = 0..8 open to sky.
        for z in 0..16 {
            for x in 8..16 {
                col.set_block(x, -20, z, STONE);
            }
        }
        let light = compute_column_light(&col, &FakeProps::new());
        // Just under the roof, light has spread sideways from the open half and
        // decayed by 1 per block — the vertical/horizontal asymmetry. x=8 is one
        // step in from the open x=7 column (which is 15), so 14, then 13...
        let under_edge = sky_at(&light, -64, 8, -21, 8);
        let deeper = sky_at(&light, -64, 10, -21, 8);
        assert!(under_edge < 15, "horizontal spread must attenuate, got {under_edge}");
        assert_eq!(under_edge, 14, "one block under the roof edge");
        assert_eq!(deeper, 12, "attenuates one per block deeper under the roof");
    }

    #[test]
    fn block_light_spreads_and_decays_from_a_source() {
        let mut col = column();
        col.set_block(8, -32, 8, TORCH);
        let light = compute_column_light(&col, &FakeProps::new());
        assert_eq!(block_at(&light, -64, 8, -32, 8), 14, "torch cell holds emission");
        assert_eq!(block_at(&light, -64, 9, -32, 8), 13, "one block away");
        assert_eq!(block_at(&light, -64, 11, -32, 8), 11, "three blocks away");
        assert_eq!(block_at(&light, -64, 8, -32 + 14, 8), 0, "beyond range is dark");
    }

    #[test]
    fn a_wall_blocks_block_light() {
        let mut col = column();
        col.set_block(8, -32, 8, TORCH);
        col.set_block(9, -32, 8, STONE); // wall east of the torch
        let light = compute_column_light(&col, &FakeProps::new());
        // The cell east of the wall must be far dimmer than the open west side:
        // light has to go around, not through opacity-15 stone.
        let behind_wall = block_at(&light, -64, 10, -32, 8);
        let open_side = block_at(&light, -64, 6, -32, 8);
        assert!(behind_wall < open_side, "wall shadows: {behind_wall} !< {open_side}");
    }

    #[test]
    fn recompute_after_removing_a_source_leaves_no_stale_light() {
        let mut col = column();
        col.set_block(8, -32, 8, TORCH);
        let lit = compute_column_light(&col, &FakeProps::new());
        assert!(block_at(&lit, -64, 8, -32, 8) > 0, "lit while the torch is present");

        // Remove the source and recompute from zero: correct by construction, so
        // no stale bright cell survives (the trap a naive increase-only re-flood
        // falls into).
        col.set_block(8, -32, 8, AIR);
        let dark = compute_column_light(&col, &FakeProps::new());
        for d in 0..14i32 {
            assert_eq!(
                block_at(&dark, -64, 8, -32 + d, 8),
                0,
                "no residual block light after the source is gone (d={d})"
            );
        }
    }

    #[test]
    fn water_attenuates_but_does_not_seal_sky() {
        let mut col = column();
        // Full water layers at y = -20 and -21, so only vertical light reaches
        // them (no open-air column beside them to relight the sides). Dampening 1
        // stops sky *sources* but lets light through with per-block decay.
        for z in 0..16 {
            for x in 0..16 {
                col.set_block(x, -20, z, WATER);
                col.set_block(x, -21, z, WATER);
            }
        }
        let light = compute_column_light(&col, &FakeProps::new());
        // Air above the water is 15; the first water cell is 15 - max(1,1) = 14,
        // the next 13, then air below continues to decay — attenuation, not a
        // hard shadow.
        assert_eq!(sky_at(&light, -64, 6, -19, 6), 15, "air above water");
        assert_eq!(sky_at(&light, -64, 6, -20, 6), 14, "first water cell");
        assert_eq!(sky_at(&light, -64, 6, -21, 6), 13, "second water cell");
        assert_eq!(sky_at(&light, -64, 6, -22, 6), 12, "air just below the water");
    }

    #[test]
    fn above_and_below_world_apron_sections_are_present() {
        let col = column();
        let light = compute_column_light(&col, &FakeProps::new());
        // section_count + 2 light sections, indices 0..=5 for a 4-section column.
        assert_eq!(light.light_section_count(), 6);
        // The apron section above the world is open sky.
        assert_eq!(light.sky(5), &LightData::Uniform(15), "above-world apron is full sky");
    }

    #[test]
    fn diff_reports_zero_over_a_nonzero_comparison_for_identical_light() {
        let mut a = ColumnLight::new(1);
        *a.sky_mut(1) = LightData::Uniform(15);
        *a.block_mut(1) = LightData::Uniform(0);
        let b = a.clone();
        let d = diff_column_light(&a, &b, 0);
        assert_eq!(d.disagreements(), 0, "identical light agrees");
        assert!(d.cells_compared > 0, "and it actually compared cells (not vacuous)");
        assert!(d.agrees());
    }

    #[test]
    fn diff_counts_exactly_the_differing_cells_and_skips_elided_sections() {
        let mut ours = ColumnLight::new(1);
        // Give ours a value in section 0, which the server left Missing: it must
        // be skipped, not counted as a disagreement.
        *ours.sky_mut(0) = LightData::Uniform(9);
        *ours.sky_mut(1) = LightData::Uniform(15);

        let mut server = ColumnLight::new(1);
        let mut arr = NibbleArray::filled(15);
        arr.set(NibbleArray::index(3, 4, 5), 14); // one disagreeing cell
        *server.sky_mut(1) = LightData::Values(arr);

        let d = diff_column_light(&ours, &server, 0);
        assert_eq!(d.sky_disagreements, 1, "exactly the one differing cell");
        assert_eq!(d.block_disagreements, 0);
    }

    #[test]
    #[should_panic(expected = "vacuous pass")]
    fn diff_rejects_a_margin_that_would_compare_zero_cells() {
        // A margin >= EDGE/2 collapses both axis loops to empty; the guard must
        // catch it rather than returning a zero-cell "agreement".
        let a = ColumnLight::new(1);
        let b = ColumnLight::new(1);
        let _ = diff_column_light(&a, &b, EDGE / 2);
    }

    #[test]
    fn diff_interior_margin_excludes_border_columns() {
        let mut ours = ColumnLight::new(1);
        *ours.sky_mut(1) = LightData::Uniform(15);
        let mut server = ColumnLight::new(1);
        let mut arr = NibbleArray::filled(15);
        arr.set(NibbleArray::index(0, 4, 0), 0); // a difference on the x=z=0 border
        *server.sky_mut(1) = LightData::Values(arr);

        assert_eq!(
            diff_column_light(&ours, &server, 0).sky_disagreements,
            1,
            "margin 0 counts the border cell"
        );
        assert_eq!(
            diff_column_light(&ours, &server, 1).sky_disagreements,
            0,
            "margin 1 excludes the seam-affected border, so no false positive"
        );
    }
}
