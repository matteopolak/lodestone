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
//! [`compute_column_light`] runs over one whole chunk column (all 16×16 columns
//! at once), so *intra*-chunk horizontal spread is exact. Light entering from a
//! neighbouring chunk is not pulled in, so cells under an overhang within 15 of
//! an x/z border can under-report by a neighbour's contribution — a residual
//! confined to the border region. [`compute_column_light_with_neighbours`]
//! closes that gap: it floods the same rule over a 3×3 neighbourhood and is
//! *exact* for the centre chunk, because light decays at least one level per
//! block and 15 < 16, so no source beyond the immediate neighbours can reach it.
//! A neighbour left absent is treated as an opaque seam, reproducing the isolated
//! result on that side — the honest, later-correctable state when a neighbour has
//! not loaded. [`diff_column_light`] reports edge and interior disagreements
//! separately so the seam residual is a watched number, not a caveat.
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
/// [`ChunkColumn`](crate::ChunkColumn) implements this directly; the
/// neighbour-aware [`compute_column_light_with_neighbours`] samples several of
/// these — a centre plus its neighbours — over one wide field.
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
const MAX_LIGHT: u8 = 15;

/// The horizontal footprint a light computation runs over. Single-column light
/// uses a `16×16` field; a neighbour-aware compute widens it to a `48×48` 3×3
/// chunk neighbourhood so light entering from adjacent chunks is pulled in
/// rather than dropped at the seam. `y` is the outer axis, so a whole horizontal
/// layer is contiguous; the order is internal (only the packed output must match
/// the wire nibble order).
#[derive(Clone, Copy)]
struct Field {
    wx: usize,
    wz: usize,
    height: usize,
}

impl Field {
    #[inline]
    fn area(&self) -> usize {
        self.wx * self.wz
    }
    #[inline]
    fn len(&self) -> usize {
        self.area() * self.height
    }
    #[inline]
    fn cell(&self, x: usize, y: usize, z: usize) -> usize {
        (y * self.area()) + (z * self.wx) + x
    }
    #[inline]
    fn uncell(&self, idx: usize) -> (usize, usize, usize) {
        let a = self.area();
        let y = idx / a;
        let rem = idx % a;
        (rem % self.wx, y, rem / self.wx)
    }
}

/// Computes sky and block light for one chunk column, returning a fully
/// populated [`ColumnLight`] spanning `section_count + 2` light sections (one
/// apron section below the world and one above, exactly as a `light_update`
/// packet carries).
///
/// This computes the column **in isolation**: light entering from a neighbouring
/// chunk is not pulled in, so cells under an overhang within 15 blocks of an x/z
/// border can under-report by a neighbour's contribution. That residual is
/// confined to the chunk border region and is *correctable* once the neighbour
/// loads — see [`compute_column_light_with_neighbours`], which is exact for the
/// centre chunk, and [`diff_column_light`], which reports edge and interior
/// disagreements separately so the residual is a watched number rather than a
/// caveat. On open terrain (every column a full-strength sky source) the
/// isolated result is already exact everywhere, which is why the superflat light
/// oracle agrees cell-for-cell.
#[must_use]
pub fn compute_column_light(
    blocks: &impl BlockVolume,
    props: &impl LightProperties,
) -> ColumnLight {
    let section_count = blocks.section_count();
    let min_y = blocks.min_y();
    let field = Field {
        wx: EDGE,
        wz: EDGE,
        height: (section_count + 2) * EDGE,
    };
    // Single column: every field cell is the centre chunk (offset 0,0), never a
    // barrier, so this reduces exactly to a 16×16 computation.
    compute_lit(section_count, min_y, field, 0, 0, props, |x, world_y, z| {
        Some(blocks.block(x, world_y, z))
    })
}

/// Up to nine chunk columns — a centre plus its eight neighbours — supplied to
/// [`compute_column_light_with_neighbours`] so the centre's light can be
/// computed with cross-chunk propagation resolved.
///
/// All columns must share the centre's `min_y` and `section_count` (same world),
/// which a `debug_assert` checks. Neighbours not supplied are treated as an
/// opaque barrier at the seam, which is exactly the isolated-column behaviour on
/// that side — the honest, later-correctable result when a neighbour has not
/// loaded yet.
pub struct Neighbourhood<'a, V: BlockVolume> {
    center: &'a V,
    // Indexed by `(dz + 1) * 3 + (dx + 1)`; the centre slot (4) is unused.
    neighbours: [Option<&'a V>; 9],
}

impl<'a, V: BlockVolume> Neighbourhood<'a, V> {
    /// Starts a neighbourhood with only the centre column loaded.
    #[must_use]
    pub fn new(center: &'a V) -> Self {
        Self {
            center,
            neighbours: [None; 9],
        }
    }

    /// Adds the neighbour at chunk offset `(dx, dz)`, each in `-1..=1` and not
    /// both zero (that is the centre). Later calls with the same offset replace.
    #[must_use]
    pub fn with(mut self, dx: i32, dz: i32, neighbour: &'a V) -> Self {
        assert!(
            (-1..=1).contains(&dx) && (-1..=1).contains(&dz) && (dx, dz) != (0, 0),
            "neighbour offset ({dx},{dz}) must be in -1..=1 and not the centre"
        );
        debug_assert_eq!(neighbour.min_y(), self.center.min_y(), "neighbour min_y");
        debug_assert_eq!(
            neighbour.section_count(),
            self.center.section_count(),
            "neighbour section_count"
        );
        self.neighbours[((dz + 1) * 3 + (dx + 1)) as usize] = Some(neighbour);
        self
    }

    #[inline]
    fn at(&self, dx: i32, dz: i32) -> Option<&V> {
        if (dx, dz) == (0, 0) {
            Some(self.center)
        } else {
            self.neighbours[((dz + 1) * 3 + (dx + 1)) as usize]
        }
    }
}

impl<V: BlockVolume> std::fmt::Debug for Neighbourhood<'_, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `V` need not be `Debug`; report the loaded footprint instead.
        let loaded: Vec<(i32, i32)> = (-1..=1)
            .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
            .filter(|&(dx, dz)| (dx, dz) != (0, 0) && self.at(dx, dz).is_some())
            .collect();
        f.debug_struct("Neighbourhood")
            .field("loaded_neighbours", &loaded)
            .finish()
    }
}

/// Computes the **centre** chunk's light with its loaded neighbours present, so
/// sky and block light that spread across a chunk seam are resolved.
///
/// This is exact for the centre chunk: a cell can receive light only from a
/// source within 15 blocks (each block crossed costs at least one level), and 15
/// is less than the 16-wide chunk, so a source two chunks away — a
/// neighbour-of-a-neighbour — can never reach the centre. A 3×3 neighbourhood
/// therefore contains every source the centre can see. Neighbours left out of
/// the [`Neighbourhood`] act as an opaque barrier, matching the isolated result
/// on that side; when such a neighbour later loads, recomputing lifts the centre
/// border to its correct value.
///
/// It runs the same flood over a 9× wider field, so it costs roughly 9× a single
/// column ([`compute_column_light`]); an incremental seam re-propagation touching
/// only the two columns at a changed boundary is the optimisation to add later,
/// behind this same interface, with a measurement.
#[must_use]
pub fn compute_column_light_with_neighbours(
    neighbourhood: &Neighbourhood<'_, impl BlockVolume>,
    props: &impl LightProperties,
) -> ColumnLight {
    let center = neighbourhood.center;
    let section_count = center.section_count();
    let min_y = center.min_y();
    let field = Field {
        wx: 3 * EDGE,
        wz: 3 * EDGE,
        height: (section_count + 2) * EDGE,
    };
    // Field x/z 0..48 map to chunk offset dx/dz in -1..=1; the centre chunk sits
    // at field offset (16, 16). A missing neighbour returns `None` → barrier.
    compute_lit(
        section_count,
        min_y,
        field,
        EDGE,
        EDGE,
        props,
        |fx, world_y, fz| {
            let dx = (fx / EDGE) as i32 - 1;
            let dz = (fz / EDGE) as i32 - 1;
            neighbourhood
                .at(dx, dz)
                .map(|v| v.block(fx % EDGE, world_y, fz % EDGE))
        },
    )
}

/// The shared core: sample blocks over `field` (via `sample`, which returns the
/// state id or `None` for a barrier cell), flood sky and block light, then pack
/// the centre `16×16` sub-column at field offset `(ox, oz)`.
fn compute_lit(
    section_count: usize,
    min_y: i32,
    field: Field,
    ox: usize,
    oz: usize,
    props: &impl LightProperties,
    sample: impl Fn(usize, i32, usize) -> Option<u32>,
) -> ColumnLight {
    let light_sections = section_count + 2;
    debug_assert_eq!(field.height, light_sections * EDGE);
    let field_bottom_y = min_y - EDGE as i32;

    // One pass over the field builds the opacity cache (shared by both floods)
    // and seeds block-light sources. Barrier cells are opaque and never sources,
    // so no light passes their seam and none is invented behind them.
    let mut opacity = vec![0u8; field.len()];
    let mut block = vec![0u8; field.len()];
    let mut block_buckets = Buckets::new();
    for y_rel in 0..field.height {
        let world_y = field_bottom_y + y_rel as i32;
        for fz in 0..field.wz {
            for fx in 0..field.wx {
                let idx = field.cell(fx, y_rel, fz);
                match sample(fx, world_y, fz) {
                    Some(state) => {
                        opacity[idx] = props.opacity(state).min(MAX_LIGHT);
                        let emission = props.emission(state).min(MAX_LIGHT);
                        if emission > 0 {
                            // A source holds its emission regardless of its own
                            // opacity (a jack-o'-lantern is opaque yet lit).
                            block[idx] = emission;
                            block_buckets.push(emission, idx as u32);
                        }
                    }
                    None => opacity[idx] = MAX_LIGHT,
                }
            }
        }
    }

    let sky = compute_sky(&field, &opacity);
    propagate(&field, &mut block, &opacity, &mut block_buckets);

    pack(section_count, light_sections, &field, ox, oz, &sky, &block)
}

/// Sky light: seed every cell open to the sky at 15, then propagate.
fn compute_sky(field: &Field, opacity: &[u8]) -> Vec<u8> {
    let mut level = vec![0u8; field.len()];
    let mut buckets = Buckets::new();

    for fz in 0..field.wz {
        for fx in 0..field.wx {
            // Scan down from the top of the field. Every cell is a full-strength
            // source until the first cell that dampens light at all; that cell
            // and everything below get their light from propagation instead.
            for y_rel in (0..field.height).rev() {
                let idx = field.cell(fx, y_rel, fz);
                if opacity[idx] != 0 {
                    break;
                }
                level[idx] = MAX_LIGHT;
                buckets.push(MAX_LIGHT, idx as u32);
            }
        }
    }

    propagate(field, &mut level, opacity, &mut buckets);
    level
}

/// The shared descending-level flood: drain level 15 down to 1, lifting each
/// in-bounds neighbour to `l - max(1, opacity(neighbour))` when that improves
/// it. A cell popped at a level below its current value is stale and skipped.
fn propagate(field: &Field, level: &mut [u8], opacity: &[u8], buckets: &mut Buckets) {
    let mut l = MAX_LIGHT;
    while l >= 1 {
        while let Some(idx) = buckets.pop(l) {
            let idx = idx as usize;
            if level[idx] != l {
                continue;
            }
            let (x, y_rel, z) = field.uncell(idx);
            for (nx, ny, nz) in neighbours(field, x, y_rel, z) {
                let nidx = field.cell(nx, ny, nz);
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

/// The (up to) six in-bounds orthogonal neighbours. Horizontal moves outside the
/// field's `wx`/`wz` are dropped — that boundary is the chunk seam for a
/// single-column field and the neighbourhood edge for a 3×3 field.
#[inline]
fn neighbours(
    field: &Field,
    x: usize,
    y: usize,
    z: usize,
) -> impl Iterator<Item = (usize, usize, usize)> {
    let mut out: [Option<(usize, usize, usize)>; 6] = [None; 6];
    if x > 0 {
        out[0] = Some((x - 1, y, z));
    }
    if x + 1 < field.wx {
        out[1] = Some((x + 1, y, z));
    }
    if z > 0 {
        out[2] = Some((x, y, z - 1));
    }
    if z + 1 < field.wz {
        out[3] = Some((x, y, z + 1));
    }
    if y > 0 {
        out[4] = Some((x, y - 1, z));
    }
    if y + 1 < field.height {
        out[5] = Some((x, y + 1, z));
    }
    out.into_iter().flatten()
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

/// Slices the two flat fields into per-light-section [`LightData`] for the centre
/// chunk at field offset `(ox, oz)`, collapsing uniform sections to a tag.
fn pack(
    section_count: usize,
    light_sections: usize,
    field: &Field,
    ox: usize,
    oz: usize,
    sky: &[u8],
    block: &[u8],
) -> ColumnLight {
    debug_assert_eq!(field.height, light_sections * EDGE);
    let mut out = ColumnLight::new(section_count);
    for s in 0..light_sections {
        *out.sky_mut(s) = pack_section(field, ox, oz, sky, s);
        *out.block_mut(s) = pack_section(field, ox, oz, block, s);
    }
    out
}

/// Packs the centre chunk's 16³ cells of light section `s` (the `16×16` column at
/// field offset `(ox, oz)`) into a [`LightData`], using vanilla's
/// `y<<8 | z<<4 | x` nibble order so it round-trips the wire exactly.
fn pack_section(field: &Field, ox: usize, oz: usize, data: &[u8], s: usize) -> LightData {
    let base_y = s * EDGE;
    let mut bytes = [0u8; 2048];
    for y_local in 0..EDGE {
        for z in 0..EDGE {
            for x in 0..EDGE {
                let value = data[field.cell(ox + x, base_y + y_local, oz + z)] & 0x0F;
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
    /// Disagreements (either layer) on a chunk **border** column — `x` or `z` in
    /// `{0, 15}`. This is where our single-column engine legitimately differs
    /// from a server that lit the cell with a neighbour loaded, so a non-zero
    /// count here alone is the known cross-chunk seam, not a defect. It is a
    /// *watched number*: an edge count that changes when a neighbour arrives is
    /// the plumbing gap that [`compute_column_light_with_neighbours`] closes.
    pub edge_disagreements: usize,
    /// Disagreements (either layer) on an **interior** cell — every cell that is
    /// not on a border column. An interior disagreement of any size is a real
    /// bug, never the seam: no neighbour can reach an interior cell that our own
    /// column's sources do not already dominate on open terrain, and under an
    /// overhang a neighbour-aware compute must be used before diffing.
    pub interior_disagreements: usize,
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

/// Whether chunk-local `(x, z)` sits on a border column (`x` or `z` in `{0, 15}`).
#[inline]
fn is_edge_column(x: usize, z: usize) -> bool {
    x == 0 || x == EDGE - 1 || z == 0 || z == EDGE - 1
}

/// Diffs two column lights cell-by-cell, restricted to the interior columns more
/// than `interior_margin` blocks from the x/z chunk border, and partitioning
/// disagreements into [`edge`](LightDiff::edge_disagreements) and
/// [`interior`](LightDiff::interior_disagreements) counts so the cross-chunk
/// residual is a watched number rather than a caveat. Pass a margin of `0` (or
/// call [`diff_column_light_full`]) to compare the whole column and see the edge
/// count; a non-zero margin excludes the border and leaves the edge count `0`.
///
/// The margin exists because [`compute_column_light`] does not pull light in from
/// neighbouring chunks, so border cells can legitimately disagree with a server
/// that lit them with neighbours loaded. [`compute_column_light_with_neighbours`]
/// removes that residual for the centre chunk.
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
pub fn diff_column_light(
    ours: &ColumnLight,
    server: &ColumnLight,
    interior_margin: usize,
) -> LightDiff {
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
                    let edge = is_edge_column(x, z);
                    if let Some(theirs) = server.sky(s).get(idx) {
                        diff.cells_compared += 1;
                        if ours.sky(s).get(idx).unwrap_or(0) != theirs {
                            diff.sky_disagreements += 1;
                            if edge {
                                diff.edge_disagreements += 1;
                            } else {
                                diff.interior_disagreements += 1;
                            }
                        }
                    }
                    if let Some(theirs) = server.block(s).get(idx) {
                        diff.cells_compared += 1;
                        if ours.block(s).get(idx).unwrap_or(0) != theirs {
                            diff.block_disagreements += 1;
                            if edge {
                                diff.edge_disagreements += 1;
                            } else {
                                diff.interior_disagreements += 1;
                            }
                        }
                    }
                }
            }
        }
    }
    diff
}

/// [`diff_column_light`] over the whole column (margin `0`), so every cell is
/// compared and the [`edge`](LightDiff::edge_disagreements) versus
/// [`interior`](LightDiff::interior_disagreements) split is populated. This is the
/// form a real-terrain oracle wants: assert `interior_disagreements == 0` (a hard
/// correctness claim) while reporting `edge_disagreements` as the known,
/// neighbour-correctable seam count.
#[must_use]
pub fn diff_column_light_full(ours: &ColumnLight, server: &ColumnLight) -> LightDiff {
    diff_column_light(ours, server, 0)
}

/// Returns `true` if `light` contains at least one section whose sky or block
/// light genuinely varies across cells — i.e. a horizontal gradient produced by
/// propagation spreading sideways.
///
/// This exists to make the *vacuous-world* trap fail closed. A live light oracle
/// that diffs computed light against a server is only meaningful if its input
/// actually exercises sideways propagation; on a superflat world under open sky
/// every section is uniform (sky 15 above the floor, 0 below, block 0
/// everywhere), so sky light never spreads horizontally and a `0 disagreements`
/// result is trivially true rather than evidence. The flaw there is the input,
/// not the assertion, so reading the test cannot find it — but asserting this
/// over the server's own light before diffing turns "accidentally ran on
/// superflat again" from a silent pass into a failure. Pair it with the negative
/// control the way `diff_column_light`'s cell count pairs with its comparison.
#[must_use]
pub fn light_exercises_propagation(light: &ColumnLight) -> bool {
    (0..light.light_section_count())
        .any(|s| section_has_gradient(light.sky(s)) || section_has_gradient(light.block(s)))
}

/// Whether one section's light holds two differing cell values. A `Uniform` or
/// `Missing` section is flat by definition; only an explicit array can vary.
fn section_has_gradient(data: &LightData) -> bool {
    match data {
        LightData::Values(arr) => {
            let first = arr.get(0);
            (1..NibbleArray::LEN).any(|i| arr.get(i) != first)
        }
        LightData::Missing | LightData::Uniform(_) => false,
    }
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
    // Vanilla's leaves are `lightDampening = 1` (`lodestone_data::light_props`'s
    // own stated scale: air and glass 0, water/ice/leaves 1, a full solid 15), so
    // this is `WATER`'s dampening under a name that reads as a tree canopy where
    // one is what the scene means.
    const LEAVES: u32 = 5;

    struct FakeProps {
        opacity: HashMap<u32, u8>,
        emission: HashMap<u32, u8>,
    }

    impl FakeProps {
        fn new() -> Self {
            let mut opacity = HashMap::new();
            opacity.insert(STONE, 15);
            opacity.insert(WATER, 1);
            opacity.insert(LEAVES, 1);
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
        ChunkColumn::new(
            -64,
            4,
            PaletteKind::block_states(),
            PaletteKind::biomes(),
            AIR,
            0,
        )
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
            assert_eq!(
                sky_at(&light, -64, 5, y, 9),
                15,
                "open column stays 15 at y={y}"
            );
        }
    }

    #[test]
    fn glass_does_not_shadow_sky_below_it() {
        let mut col = column();
        col.set_block(4, -20, 4, GLASS);
        let light = compute_column_light(&col, &FakeProps::new());
        // Dampening 0 ⇒ still a sky source below the glass, no shadow.
        assert_eq!(
            sky_at(&light, -64, 4, -21, 4),
            15,
            "cell below glass keeps full sky"
        );
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
        assert_eq!(
            sky_at(&light, -64, 8, -19, 8),
            15,
            "above the layer is open"
        );
        assert_eq!(
            sky_at(&light, -64, 8, -20, 8),
            0,
            "the opaque layer itself is dark"
        );
        assert_eq!(
            sky_at(&light, -64, 8, -21, 8),
            0,
            "sealed below the layer is dark"
        );
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
        assert!(
            under_edge < 15,
            "horizontal spread must attenuate, got {under_edge}"
        );
        assert_eq!(under_edge, 14, "one block under the roof edge");
        assert_eq!(deeper, 12, "attenuates one per block deeper under the roof");
    }

    #[test]
    fn block_light_spreads_and_decays_from_a_source() {
        let mut col = column();
        col.set_block(8, -32, 8, TORCH);
        let light = compute_column_light(&col, &FakeProps::new());
        assert_eq!(
            block_at(&light, -64, 8, -32, 8),
            14,
            "torch cell holds emission"
        );
        assert_eq!(block_at(&light, -64, 9, -32, 8), 13, "one block away");
        assert_eq!(block_at(&light, -64, 11, -32, 8), 11, "three blocks away");
        assert_eq!(
            block_at(&light, -64, 8, -32 + 14, 8),
            0,
            "beyond range is dark"
        );
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
        assert!(
            behind_wall < open_side,
            "wall shadows: {behind_wall} !< {open_side}"
        );
    }

    #[test]
    fn recompute_after_removing_a_source_leaves_no_stale_light() {
        let mut col = column();
        col.set_block(8, -32, 8, TORCH);
        let lit = compute_column_light(&col, &FakeProps::new());
        assert!(
            block_at(&lit, -64, 8, -32, 8) > 0,
            "lit while the torch is present"
        );

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

    /// A column with solid ground filling `y = -64..=-40`, so `y = -40` is the
    /// surface block ("the dirt") and everything above it is open air.
    fn ground_column() -> ChunkColumn {
        let mut c = column();
        for z in 0..16 {
            for x in 0..16 {
                for y in -64..=-40 {
                    c.set_block(x, y, z, STONE);
                }
            }
        }
        c
    }

    /// Breaking one surface block lets sky light straight back down to the bottom
    /// of the hole at **full** strength.
    ///
    /// The exact value is the whole point. Sky light is not a vertical rule with a
    /// per-step cost — every cell whose column is unobstructed all the way up is
    /// *itself* a 15 source (`ChunkSkyLightSources`), so the newly exposed cell is
    /// `15`, not `14`. Those are the two hypotheses and they differ here by exactly
    /// one level, which is why this asserts the number instead of `> 0`: a
    /// propagate-downward-with-attenuation engine passes any "it got brighter"
    /// check and fails this one.
    #[test]
    fn breaking_a_surface_block_admits_full_strength_sky_to_the_bottom_of_the_hole() {
        let props = FakeProps::new();
        let mut col = ground_column();
        assert_eq!(
            sky_at(&compute_column_light(&col, &props), -64, 8, -40, 8),
            0,
            "the surface block is opaque, so it is dark before the break"
        );

        col.set_block(8, -40, 8, AIR);
        let light = compute_column_light(&col, &props);
        assert_eq!(
            sky_at(&light, -64, 8, -40, 8),
            15,
            "the opened cell is itself a full-strength sky source; 14 would mean \
             sky light is being propagated down with a per-step cost instead"
        );
        assert_eq!(
            sky_at(&light, -64, 8, -41, 8),
            0,
            "and the still-solid block below it stays dark — no light invented past \
             the floor of the hole"
        );
    }

    /// **The reported bug, as a scene.** Break the bottom block of a tree trunk and
    /// the dirt under it, and the two-deep hole must not be pitch black.
    ///
    /// Neither opened cell can see the sky: the rest of the trunk is directly above
    /// them. Their light arrives *sideways*, from the open columns beside the trunk,
    /// which is why a propagator that only patches up a cell's immediate
    /// neighbourhood gets the first cell right and the second one wrong. Predicted
    /// levels, re-derived from vanilla's rule outside this file rather than read off
    /// the engine:
    ///
    /// | cell | route | level |
    /// |---|---|---|
    /// | `(7, -39, 8)` open column beside the trunk | own column is clear | `15` |
    /// | `(8, -39, 8)` where the trunk block was | one step sideways | `14` |
    /// | `(8, -40, 8)` where the dirt was | one more step down | `13` |
    ///
    /// The `13` is the load-bearing assertion. Under the "checks a few adjacent
    /// blocks" hypothesis it is `0`, because nothing lit is adjacent to it at the
    /// moment the edit happens — the cell above it is *also* newly opened and also
    /// dark. Under a real flood it is `13`. Both hypotheses agree on the `14`, so
    /// asserting only that cell would be a test that measures that the code runs.
    #[test]
    fn breaking_a_tree_trunk_and_the_dirt_under_it_is_not_pitch_black() {
        let props = FakeProps::new();
        let mut col = ground_column();
        // A five-block trunk standing on the surface at (8, 8).
        for y in -39..=-35 {
            col.set_block(8, y, 8, STONE);
        }

        // Before: both cells are solid, so both are dark. This is the state the
        // player is looking at, and it is also the state the client kept showing
        // after the break for as long as nothing re-sent the light.
        let before = compute_column_light(&col, &props);
        assert_eq!(sky_at(&before, -64, 8, -39, 8), 0, "trunk cell dark before");
        assert_eq!(sky_at(&before, -64, 8, -40, 8), 0, "dirt cell dark before");

        col.set_block(8, -39, 8, AIR); // the trunk block
        col.set_block(8, -40, 8, AIR); // the dirt under it
        let after = compute_column_light(&col, &props);

        assert_eq!(
            sky_at(&after, -64, 7, -39, 8),
            15,
            "the open column beside the trunk is the source this light comes from"
        );
        assert_eq!(
            sky_at(&after, -64, 8, -39, 8),
            14,
            "one step in from the open column — both hypotheses agree here, which is \
             why this cell alone proves nothing"
        );
        assert_eq!(
            sky_at(&after, -64, 8, -40, 8),
            13,
            "two steps from the nearest lit cell: 13 under a real flood fill, 0 under \
             a propagator that only relaxes a source's immediate neighbours"
        );
    }

    /// The same edit taken deeper, so the distance the flood has to travel is the
    /// variable and the predicted value moves with it one level per block.
    ///
    /// A bounded-radius patch-up is not one hypothesis but a family of them — radius
    /// 1, 2, 3 — and each is refuted by a different row of this table. Nothing short
    /// of a queue that keeps going until it runs out of level reaches `9` at the
    /// bottom.
    #[test]
    fn sky_light_reaches_the_bottom_of_a_freshly_dug_shaft() {
        let props = FakeProps::new();
        let mut col = ground_column();
        for y in -39..=-35 {
            col.set_block(8, y, 8, STONE);
        }
        // Break the bottom trunk block and dig six deep: y = -39 down to -44.
        for y in -44..=-39 {
            col.set_block(8, y, 8, AIR);
        }
        let light = compute_column_light(&col, &props);

        // Derived outside this file from `level(n) = l - max(1, dampening(n))` with
        // air's dampening `0`, starting from the 15 in the open column beside the
        // shaft. One level per block, all the way down.
        for (y, expected) in [
            (-39, 14),
            (-40, 13),
            (-41, 12),
            (-42, 11),
            (-43, 10),
            (-44, 9),
        ] {
            assert_eq!(
                sky_at(&light, -64, 8, y, 8),
                expected,
                "sky at y={y} must be {expected}; a radius-limited propagator returns \
                 0 from the first row it cannot reach"
            );
        }
        assert_eq!(
            sky_at(&light, -64, 8, -45, 8),
            0,
            "the unbroken floor below the shaft stays dark — the control that shows \
             this gate can report a zero at all"
        );
    }

    /// The same scene under a tree canopy, which is what the player actually had
    /// over their head.
    ///
    /// Two things change and both are worth pinning. The canopy is `dampening = 1`,
    /// so it costs a level to cross rather than sealing the column — the open ground
    /// beside the trunk sits at `8`, not `15`. And the hole therefore comes back at
    /// `7` and `6`.
    ///
    /// Those are the numbers, and they are exactly why this file re-derives instead
    /// of guessing: the plausible round answers here are "full daylight" or "one
    /// less than the neighbour", and both are wrong. `6` in particular is *below*
    /// the `8` of the open ground two blocks away, because the light has to go
    /// sideways and then down.
    #[test]
    fn a_canopy_dims_the_opened_hole_without_sealing_it() {
        let props = FakeProps::new();
        let mut col = ground_column();
        for y in -39..=-35 {
            col.set_block(8, y, 8, STONE);
        }
        // A two-thick canopy over the whole footprint, well clear of the trunk.
        for z in 0..16 {
            for x in 0..16 {
                col.set_block(x, -34, z, LEAVES);
                col.set_block(x, -33, z, LEAVES);
            }
        }

        let before = compute_column_light(&col, &props);
        assert_eq!(sky_at(&before, -64, 8, -39, 8), 0);
        assert_eq!(sky_at(&before, -64, 8, -40, 8), 0);

        col.set_block(8, -39, 8, AIR);
        col.set_block(8, -40, 8, AIR);
        let after = compute_column_light(&col, &props);

        // The canopy's own two levels of attenuation, then air down to the ground.
        assert_eq!(sky_at(&after, -64, 2, -33, 2), 14, "first canopy layer");
        assert_eq!(sky_at(&after, -64, 2, -34, 2), 13, "second canopy layer");
        assert_eq!(
            sky_at(&after, -64, 2, -39, 2),
            8,
            "open ground under the canopy: five more blocks of air below it"
        );
        assert_eq!(
            sky_at(&after, -64, 8, -39, 8),
            7,
            "the opened trunk cell, one step in from that 8"
        );
        assert_eq!(
            sky_at(&after, -64, 8, -40, 8),
            6,
            "and the opened dirt cell one step below that — dimmer than the ground \
             beside it, which is the shape a guess gets wrong"
        );
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
        assert_eq!(
            sky_at(&light, -64, 6, -22, 6),
            12,
            "air just below the water"
        );
    }

    #[test]
    fn above_and_below_world_apron_sections_are_present() {
        let col = column();
        let light = compute_column_light(&col, &FakeProps::new());
        // section_count + 2 light sections, indices 0..=5 for a 4-section column.
        assert_eq!(light.light_section_count(), 6);
        // The apron section above the world is open sky.
        assert_eq!(
            light.sky(5),
            &LightData::Uniform(15),
            "above-world apron is full sky"
        );
    }

    #[test]
    fn diff_reports_zero_over_a_nonzero_comparison_for_identical_light() {
        let mut a = ColumnLight::new(1);
        *a.sky_mut(1) = LightData::Uniform(15);
        *a.block_mut(1) = LightData::Uniform(0);
        let b = a.clone();
        let d = diff_column_light(&a, &b, 0);
        assert_eq!(d.disagreements(), 0, "identical light agrees");
        assert!(
            d.cells_compared > 0,
            "and it actually compared cells (not vacuous)"
        );
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
    fn propagation_check_rejects_a_superflat_style_uniform_column() {
        // Every section uniform (sky on above the floor, off below, block dark):
        // the shape a superflat world produces. No sideways spread, so an oracle
        // on this input would be vacuous — the guard must return false.
        let mut light = ColumnLight::new(4);
        for i in 0..light.light_section_count() {
            *light.sky_mut(i) = if i >= 3 {
                LightData::Uniform(15)
            } else {
                LightData::Uniform(0)
            };
            *light.block_mut(i) = LightData::Uniform(0);
        }
        assert!(
            !light_exercises_propagation(&light),
            "a fully-uniform column does not exercise propagation"
        );

        // A single section carrying a real horizontal gradient flips it to true.
        let mut arr = NibbleArray::filled(15);
        arr.set(NibbleArray::index(3, 4, 5), 11);
        *light.sky_mut(2) = LightData::Values(arr);
        assert!(
            light_exercises_propagation(&light),
            "one varying section means sideways spread is present"
        );
    }

    #[test]
    fn propagation_check_ignores_a_secretly_uniform_values_array() {
        // A Values array whose cells are all equal is still flat — it must not
        // count as exercising propagation just because it is not a Uniform tag.
        let mut light = ColumnLight::new(1);
        *light.sky_mut(0) = LightData::Values(NibbleArray::filled(7));
        assert!(!light_exercises_propagation(&light));
    }

    /// A column with a solid `STONE` roof plane across its whole footprint at
    /// `y = -8`. Everything below the roof is cut off from vertical sky and can
    /// only be lit horizontally — from a neighbour, if one is present.
    fn roofed_column() -> ChunkColumn {
        let mut c = column();
        for z in 0..16 {
            for x in 0..16 {
                c.set_block(x, -8, z, STONE);
            }
        }
        c
    }

    #[test]
    fn diff_partitions_disagreements_into_edge_and_interior() {
        // Two disagreements placed by hand: one on a border column (x = 0) and
        // one in the interior. The partition must attribute exactly one to each,
        // so "edge-only" becomes a number the caller can watch.
        let mut server = ColumnLight::new(1);
        *server.sky_mut(1) = LightData::Values(NibbleArray::filled(15));
        let mut ours = ColumnLight::new(1);
        let mut arr = NibbleArray::filled(15);
        arr.set(NibbleArray::index(0, 0, 5), 3); // border column
        arr.set(NibbleArray::index(8, 0, 8), 3); // interior
        *ours.sky_mut(1) = LightData::Values(arr);

        let diff = diff_column_light_full(&ours, &server);
        assert_eq!(diff.cells_compared, NibbleArray::LEN, "one full section compared");
        assert_eq!(diff.disagreements(), 2);
        assert_eq!(diff.edge_disagreements, 1);
        assert_eq!(diff.interior_disagreements, 1);
    }

    #[test]
    fn neighbour_light_crosses_a_chunk_seam_that_isolation_drops() {
        // The plumbing-gap proof. A west-neighbour A is open sky; the centre B is
        // roofed, so B's under-roof cells can only be lit by light spilling in
        // from A across the shared x-seam.
        let props = FakeProps::new();
        let b = roofed_column();
        let a_open = column(); // all air → open sky, 15 everywhere
        let a_roofed = roofed_column(); // dark under its own roof at the seam

        let iso = compute_column_light(&b, &props);
        let with_open =
            compute_column_light_with_neighbours(&Neighbourhood::new(&b).with(-1, 0, &a_open), &props);
        let with_roofed = compute_column_light_with_neighbours(
            &Neighbourhood::new(&b).with(-1, 0, &a_roofed),
            &props,
        );
        let with_none = compute_column_light_with_neighbours(&Neighbourhood::new(&b), &props);

        // Isolated: the under-roof seam cell is dark — it under-reports exactly
        // the neighbour contribution a server would have applied.
        assert_eq!(sky_at(&iso, -64, 1, -9, 8), 0, "isolated under-roof seam is dark");

        // With the open neighbour present, light crosses the seam and decays by
        // one per block: A(15) → B(0)=14 → B(1)=13. This is the combined-field
        // truth, computed by construction, pinned to a hand value.
        assert_eq!(sky_at(&with_open, -64, 1, -9, 8), 13, "open neighbour lights the seam");
        // Interior cells under the overhang depend on the neighbour too, and are
        // corrected exactly: B(5) = 15 - 1 - 5 = 9.
        assert_eq!(sky_at(&with_open, -64, 5, -9, 8), 9, "interior under overhang fixed");
        assert_eq!(sky_at(&iso, -64, 5, -9, 8), 0, "same interior cell dark in isolation");

        // §12.53: the assertion must fail if the neighbour's contribution is
        // suppressed. Swap the open neighbour for a roofed one — dark at the seam
        // — and the seam cell falls back to dark. The lift came from A's *blocks*
        // being open, not merely from taking the neighbour-aware code path.
        assert_eq!(
            sky_at(&with_roofed, -64, 1, -9, 8),
            0,
            "a roofed neighbour contributes nothing across the seam"
        );

        // A neighbourhood with no neighbours loaded reduces exactly to the
        // isolated column — the honest region-edge result, later correctable.
        assert_eq!(with_none, iso, "no neighbours ⇒ isolated behaviour, byte for byte");

        // Input floor: the corrected column is genuinely non-degenerate, so this
        // whole test is not the vacuous-world trap one level out.
        assert!(light_exercises_propagation(&with_open));
    }

    #[test]
    fn neighbour_aware_open_terrain_matches_isolation() {
        // When neighbours contribute nothing (all open sky), the neighbour-aware
        // compute must reproduce the isolated result byte for byte — no spurious
        // brightening from the wider field, no dimming from the barrier apron.
        // This is why the superflat oracle's `0 disagreements` stays honest.
        let props = FakeProps::new();
        let open = column();
        let iso = compute_column_light(&open, &props);
        let all = Neighbourhood::new(&open)
            .with(-1, 0, &open)
            .with(1, 0, &open)
            .with(0, -1, &open)
            .with(0, 1, &open)
            .with(-1, -1, &open)
            .with(1, -1, &open)
            .with(-1, 1, &open)
            .with(1, 1, &open);
        let with_all = compute_column_light_with_neighbours(&all, &props);
        assert_eq!(with_all, iso, "open neighbours change nothing");
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
