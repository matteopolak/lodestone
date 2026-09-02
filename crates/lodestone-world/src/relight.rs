//! Incremental client-side relight: vanilla's own bounded-box recompute run
//! on every block change, for a client that cannot wait for a server to
//! relight for it.
//!
//! # What it is
//!
//! The client's own light engine. A block change queues a relight;
//! [`World::run_pending_relight`] drains the queue, recomputes a bounded box of
//! sky and block light around each change, and reports which section meshes that
//! invalidates.
//!
//! # Why it exists
//!
//! A real vanilla server does **not** send you a light update for a block you
//! break. The block-update packet goes to everyone tracking the chunk, but the
//! light-update packet goes only to players for whom that chunk sits on the
//! **outer ring** of their loaded area. A player standing in a chunk is never
//! on that chunk's own border, so the breaker gets the block change and no
//! light with it, ever.
//!
//! Vanilla is fine because its client runs the same light engine the server
//! does: setting a block state re-checks the light engine at that position
//! whenever the old and new state's light properties differ, and the world
//! data structure that carries that check is shared between both sides. The
//! client's own per-tick loop then drains whatever light work that queued and
//! runs it through the same engine. Server light packets are a *correction*,
//! not the mechanism.
//!
//! Without this module the symptom is exact, and it is the reported one: a broken
//! block leaves a **pitch-black hole**. The mesher lights a face from the cell the
//! face opens into (`SnapshotLight::face_light`, matching vanilla's own
//! block-face lighting), and an opaque cell stores light `0` — so the moment a
//! solid becomes air its cell still holds `0`, and every face now exposed to it
//! renders at the shader's dark floor. The integrated server hides it by relighting
//! and pushing a `light_update` about a tick later, which is why singleplayer looks
//! right and a real server does not.
//!
//! # How it works
//!
//! [`World::set_block`](crate::World::set_block) and
//! [`World::set_blocks`](crate::World::set_blocks) record the position they wrote;
//! nothing recomputes light on the write itself. A `/fill` is 4096 writes under one
//! lock, and a relight per cell inside the packet handler is exactly the frame
//! stall vanilla avoids by batching onto its own tick.
//!
//! Each drain groups the queue **by section** — positions sharing a
//! `(x >> 4, y >> 4, z >> 4)` become one job — and each job recomputes a bounded
//! box rather than a whole column:
//!
//! * The box is the bounding box of the job's changes expanded by
//!   [`AFFECTED_RADIUS`] in every axis.
//! * Its outermost one-cell **shell is fixed**: those cells keep their stored light
//!   and act as immovable sources. That is what makes a bounded recompute *exact*.
//!   Light decays at least one level per cell crossed, so a change can only alter
//!   cells within 14 of itself (a cell 15 away would receive `15 - 15 = 0`), and
//!   every path from a source outside the box crosses the shell, whose stored value
//!   already sums up everything beyond it.
//! * Only the interior is recomputed — from **zero**, never from the stored value,
//!   so a block that now blocks light cannot leave a stale bright cell behind. The
//!   result is then **diffed** against the stored values and written back cell by
//!   cell. The diff is not an optimisation: writing every interior cell would
//!   expand ~26 `LightData::Missing`/`Uniform` tags per column into 2048-byte
//!   arrays, where vanilla materialises only 2–7 sky and 0–10 block sections per
//!   chunk.
//!
//! Sky light needs one extra rule, because it is not radius-bounded in `y`:
//! uncapping a shaft turns every cell down to its floor into a full-strength sky
//! source. So the box's `y` range also spans the vertical run of transparent cells
//! below each change — see [`sky_run_bottom`].
//!
//! Openness itself is read off the box's own top shell rather than by scanning to
//! the world ceiling: **a cell holds sky light 15 if and only if it is a sky
//! source**, because propagation costs `max(1, dampening)` so a cell that merely
//! received light tops out at 14. Openness then descends through the box by
//! `open(y) = open(y + 1) && dampening(y) == 0`, the same scalar rule vanilla's
//! own per-column openness tracking uses.
//!
//! # How to change it, and the gotchas
//!
//! * **The server still wins.** [`World::merge_light`](crate::World::merge_light)
//!   drops pending relights for the chunk it patches, so a real correction arriving
//!   before we get to it is never overwritten by our own recomputation — and one
//!   arriving after simply overwrites us. Both orders end on the server's data.
//!   Deleting that line reintroduces the divergence bug in a subtler form than the
//!   one this module fixes.
//! * **[`AFFECTED_RADIUS`] is derived, not tuned.** Shrinking it puts the fixed
//!   shell on cells the change really does alter, and stale-bright artifacts then
//!   appear at the box boundary instead of at the broken block — much harder to
//!   attribute to this code.
//! * **A `Missing` sky section means 15, not 0.** Vanilla elides every sky section
//!   above the top populated one and its own light lookup answers 15 there,
//!   while a genuinely dark section is sent as an explicit empty
//!   (`Uniform(0)`). Reading `Missing` as darkness blacks out everything above the
//!   terrain — and it is the same rule the mesher's `SkyDefault` already follows,
//!   so the two must not disagree.
//! * The dirty-section set this returns is what reaches pixels. A relight that
//!   changes light and dirties no mesh changes nothing on screen.
//! * Every budget here is a **cell count**, never a duration: a wall-clock figure
//!   taken on a loaded machine gets attributed to the wrong cause, while
//!   `cells_visited` is the same number every run for the same input.
//!
//! # Dependencies
//!
//! [`LightProperties`] (injected — this crate holds no block registry) and
//! [`crate::World`]'s own storage. The client host supplies the props: 26.2's
//! `lodestone_data::light_props` for a live session, the shell's demo table for the
//! offline world.

use std::collections::{BTreeMap, BTreeSet};

use crate::ChunkColumn;
use crate::light::{LightData, NibbleArray};
use crate::lighting::LightProperties;
use crate::section::ChunkSection;
use crate::world::{ChunkPos, World};

/// Section edge, 16.
const EDGE: i32 = ChunkSection::EDGE as i32;

/// Maximum light level, 15.
const MAX_LIGHT: u8 = 15;

/// How far, in cells, a single block change can alter light — and therefore how far
/// out the recomputed box's fixed shell sits.
///
/// Derived, not chosen. A cell receives at most `15 - path_length` from a source, so
/// a change 15 cells away contributes `0`: nothing. A shell at exactly this distance
/// is provably unaffected, so its stored light is still correct and can serve as an
/// immovable source. Interior cells — within 14 — are the ones actually recomputed.
pub const AFFECTED_RADIUS: i32 = 15;

/// Cell budget for one [`World::run_pending_relight`] call. Jobs past it stay queued
/// for the next drain, so a `/fill` spreads across frames instead of stalling one. A
/// single-block break costs 31³ = 29,791 cells, so this is about ten breaks a drain.
pub const RELIGHT_CELL_BUDGET: usize = 320_000;

/// Cell ceiling for a single job. A job bigger than this is dropped rather than run:
/// it can only come from uncapping a shaft hundreds of blocks deep, where the honest
/// outcome is a residual the next chunk resend corrects, not a multi-frame stall.
pub const RELIGHT_JOB_CEILING: usize = 1_200_000;

/// Cap on the dirty-section set one drain reports, so a pathological batch cannot
/// queue an unbounded re-mesh.
const DIRTY_SECTION_CAP: usize = 512;

/// How many per-job [`RelitJob`] records one drain keeps.
///
/// A bulk edit can produce hundreds of jobs and the host logs these one line each,
/// so the record is a **sample**, not a census — [`Relit::jobs`] stays the count.
/// Eight is enough to cover every job a hand-broken block produces.
pub const RELIT_DETAIL_CAP: usize = 8;

/// One job's diagnostic record: what it recomputed, and **which direction** it
/// moved the light.
///
/// This exists because the two failure modes of an incremental relight are
/// indistinguishable from their aggregate counters. [`Relit::cells_changed`] is the
/// same number whether the recompute darkened a hole correctly or flooded an
/// enclosed room with daylight; only the signed split separates them, and only
/// [`Self::sky_source_columns_from_missing`] says whether a flood was seeded from
/// data the server actually sent or from a [`LightData::Missing`] section this
/// engine resolves to `15` on its own authority.
///
/// Every field is a count or a level — never a duration — for the reason the module
/// docs give: a wall-clock figure taken on a loaded machine is attributed to the
/// wrong cause, while these are identical every run for the same input.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RelitJob {
    /// Inclusive world-space corners of the recomputed box.
    pub region_min: [i32; 3],
    /// Inclusive world-space corners of the recomputed box.
    pub region_max: [i32; 3],
    /// One representative changed block position from this job, so a log line
    /// points at a place in the world rather than only at a box.
    pub change: [i32; 3],
    /// Block changes coalesced into this job.
    pub changes: usize,
    /// Columns of the box whose **top shell** cell held sky light `15` and so was
    /// accepted as a sky source by the openness scan.
    pub sky_source_columns: usize,
    /// Of [`Self::sky_source_columns`], how many took their `15` from a
    /// [`LightData::Missing`] sky section rather than from a value the server sent.
    ///
    /// **This is the discriminator.** A `Missing` sky section legitimately means
    /// "above the top populated section, therefore full daylight"; if this is
    /// non-zero for a job deep underground, the engine is manufacturing sky sources
    /// out of absent data, and a large [`Self::sky_raised`] beside it is that
    /// fiction reaching storage.
    pub sky_source_columns_from_missing: usize,
    /// Interior cells whose sky light the write-back **raised**, and by how much at
    /// most.
    pub sky_raised: usize,
    /// Interior cells whose sky light the write-back **lowered**.
    pub sky_lowered: usize,
    /// Largest single-cell sky increase this job wrote, `0..=15`.
    pub max_sky_gain: u8,
    /// Interior cells whose block light the write-back **raised**.
    pub block_raised: usize,
    /// Interior cells whose block light the write-back **lowered**.
    pub block_lowered: usize,
    /// Largest single-cell block-light increase this job wrote, `0..=15`.
    pub max_block_gain: u8,
}

/// What one [`World::run_pending_relight`] drain did.
///
/// Counters rather than timings, deliberately — see the module docs.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Relit {
    /// Changed block positions in jobs that actually ran. Positions deferred,
    /// dropped, or outside a loaded column are excluded.
    pub input_blocks: usize,
    /// Coalesced jobs run: one per changed section, not one per changed block.
    pub jobs: usize,
    /// Cells recomputed, summed over jobs. The cost measure.
    pub cells_visited: usize,
    /// Cells whose sky or block light actually moved and was written back. The
    /// *work* measure, and the one that shows the relight did something at all.
    pub cells_changed: usize,
    /// Jobs left queued because [`RELIGHT_CELL_BUDGET`] ran out.
    pub deferred: usize,
    /// Jobs dropped for exceeding [`RELIGHT_JOB_CEILING`].
    pub dropped: usize,
    /// Sections whose mesh must be rebuilt, as absolute `(chunk_x, chunk_z,
    /// section_y)` — the same coordinate space a block-update dirty signal uses.
    /// Includes the neighbour a changed cell on a section boundary also dirties,
    /// because smooth light and ambient occlusion sample across the seam.
    pub dirty_sections: BTreeSet<(i32, i32, i32)>,
    /// Per-job diagnostics for the first [`RELIT_DETAIL_CAP`] jobs of this drain.
    ///
    /// A sample rather than a census — see [`RelitJob`] for why the aggregate
    /// counters above cannot answer the question this one exists for.
    pub detail: Vec<RelitJob>,
}

/// The lowest `y` whose sky-source status a change at `(x, y, z)` can flip.
///
/// Sky light is the one layer a radius bound does not contain: every cell open to
/// the sky is itself a full-strength source (`ChunkSkyLightSources`), so uncapping a
/// shaft promotes the whole shaft at once and capping one demotes it. Both cases are
/// the run of transparent cells directly *below* the change, which is why the scan
/// starts at `y - 1` and does not care which way the change went.
///
/// `bottom` clamps the scan to the world floor.
pub fn sky_run_bottom(
    x: usize,
    y: i32,
    z: usize,
    bottom: i32,
    column: &ChunkColumn,
    props: &impl LightProperties,
) -> i32 {
    let mut run = y;
    let mut probe = y - 1;
    while probe >= bottom && props.opacity(column.get_block(x, probe, z)) == 0 {
        run = probe;
        probe -= 1;
    }
    run
}

/// An inclusive world-space box.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Region {
    min: [i32; 3],
    max: [i32; 3],
}

impl Region {
    fn dims(&self) -> [usize; 3] {
        [
            (self.max[0] - self.min[0] + 1) as usize,
            (self.max[1] - self.min[1] + 1) as usize,
            (self.max[2] - self.min[2] + 1) as usize,
        ]
    }

    fn cells(&self) -> usize {
        let d = self.dims();
        d[0] * d[1] * d[2]
    }
}

/// One job's working set: parallel planes over the region, `y`-major so a whole
/// horizontal layer is contiguous.
struct Scratch {
    dims: [usize; 3],
    /// Raw dampening per cell; `MAX_LIGHT` for a cell we hold no chunk for.
    opacity: Vec<u8>,
    /// Block-light emission per cell.
    emission: Vec<u8>,
    /// Light as the world currently stores it, for the write-back diff.
    stored_sky: Vec<u8>,
    stored_block: Vec<u8>,
    /// Whether `stored_sky` at this cell came from a [`LightData::Missing`]
    /// section — i.e. is this engine's own `15`, not a value the server sent.
    ///
    /// Diagnostic only: nothing in the flood or the write-back reads it. It is a
    /// plane rather than a per-section flag because a job's box straddles sections
    /// and chunks, and the question is asked per top-shell cell.
    sky_from_missing: Vec<bool>,
    /// Light as recomputed.
    sky: Vec<u8>,
    block: Vec<u8>,
    /// Cells that keep their stored value: the outer shell, plus anything in a
    /// chunk we do not hold.
    fixed: Vec<bool>,
}

impl Scratch {
    fn new(region: &Region) -> Self {
        let dims = region.dims();
        let n = dims[0] * dims[1] * dims[2];
        Self {
            dims,
            opacity: vec![MAX_LIGHT; n],
            emission: vec![0; n],
            stored_sky: vec![0; n],
            stored_block: vec![0; n],
            sky_from_missing: vec![false; n],
            sky: vec![0; n],
            block: vec![0; n],
            fixed: vec![true; n],
        }
    }
}

#[inline]
fn index(dims: [usize; 3], lx: usize, ly: usize, lz: usize) -> usize {
    (ly * dims[2] + lz) * dims[0] + lx
}

#[inline]
fn unindex(dims: [usize; 3], idx: usize) -> (usize, usize, usize) {
    let plane = dims[0] * dims[2];
    let ly = idx / plane;
    let rem = idx % plane;
    (rem % dims[0], ly, rem / dims[0])
}

/// A 15-level bucket queue. The descending-level drain is what makes one pass settle
/// every cell at its maximum; order within a level does not matter. Level `0` is
/// never queued — a cell at `0` propagates nothing.
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
    fn push(&mut self, level: u8, idx: usize) {
        if level > 0 {
            self.levels[level as usize].push(idx as u32);
        }
    }
}

/// The shared flood: drain level 15 down to 1, lifting each in-region neighbour to
/// `l - max(1, dampening(neighbour))` when that improves it. Fixed cells are read as
/// sources and never written — that is what bounds the region.
fn propagate(
    dims: [usize; 3],
    opacity: &[u8],
    fixed: &[bool],
    level: &mut [u8],
    buckets: &mut Buckets,
) {
    let mut l = MAX_LIGHT;
    while l >= 1 {
        while let Some(idx) = buckets.levels[l as usize].pop() {
            let idx = idx as usize;
            if level[idx] != l {
                continue;
            }
            let (lx, ly, lz) = unindex(dims, idx);
            for (nx, ny, nz) in neighbours(dims, lx, ly, lz) {
                let nidx = index(dims, nx, ny, nz);
                if fixed[nidx] {
                    continue;
                }
                let new = l.saturating_sub(opacity[nidx].max(1));
                if new > level[nidx] {
                    level[nidx] = new;
                    buckets.push(new, nidx);
                }
            }
        }
        l -= 1;
    }
}

/// The up-to-six in-region orthogonal neighbours. A step outside the region is
/// dropped: the shell is fixed, so nothing legitimate ever needs to leave.
#[inline]
fn neighbours(
    dims: [usize; 3],
    x: usize,
    y: usize,
    z: usize,
) -> impl Iterator<Item = (usize, usize, usize)> {
    let mut out: [Option<(usize, usize, usize)>; 6] = [None; 6];
    if x > 0 {
        out[0] = Some((x - 1, y, z));
    }
    if x + 1 < dims[0] {
        out[1] = Some((x + 1, y, z));
    }
    if z > 0 {
        out[2] = Some((x, y, z - 1));
    }
    if z + 1 < dims[2] {
        out[3] = Some((x, y, z + 1));
    }
    if y > 0 {
        out[4] = Some((x, y - 1, z));
    }
    if y + 1 < dims[1] {
        out[5] = Some((x, y + 1, z));
    }
    out.into_iter().flatten()
}

/// Light-section index for world `y` in a column with this `min_y`.
///
/// Light sections are offset by one from block sections: light section `0` is the
/// apron below the world, so light section `i` covers block section `i - 1`. Same
/// convention [`crate::LightPatch`] and the wire use.
#[inline]
fn light_section_of(y: i32, min_y: i32) -> i32 {
    (y - min_y).div_euclid(EDGE) + 1
}

/// A cell whose light moved dirties its own section's mesh, plus every neighbour
/// section the cell physically touches — smooth light and ambient occlusion sample
/// across section faces, edges and corners, so a change at local index `0` or `15`
/// is visible in the section across that boundary. This is vanilla's own
/// rule for dirtying a changed section's neighbours, narrowed by the same
/// per-axis filter the block-update path uses rather than dirtying all 27
/// unconditionally.
///
/// **The emitted tuple is `(chunk_x, chunk_z, section_y)` — not `(x, y, z)`.** All
/// three components are `i32` section indices, so writing them in spatial order
/// transposes two of them with no type error and no failing round trip; it cost a run
/// here, because a fixture centred on chunk `(0, 0)` has `chunk_x == chunk_z` and the
/// swap is invisible. The consumer is a mesher keyed on column-plus-height, which is
/// why the horizontal pair comes first.
fn mark_dirty_sections(x: i32, y: i32, z: i32, out: &mut BTreeSet<(i32, i32, i32)>) {
    if out.len() >= DIRTY_SECTION_CAP {
        return;
    }
    let (sx, sy, sz) = (x.div_euclid(EDGE), y.div_euclid(EDGE), z.div_euclid(EDGE));
    let (bx, by, bz) = (x.rem_euclid(EDGE), y.rem_euclid(EDGE), z.rem_euclid(EDGE));
    for dx in -1..=1 {
        for dy in -1..=1 {
            for dz in -1..=1 {
                if (dx == -1 && bx != 0) || (dx == 1 && bx != EDGE - 1) {
                    continue;
                }
                if (dy == -1 && by != 0) || (dy == 1 && by != EDGE - 1) {
                    continue;
                }
                if (dz == -1 && bz != 0) || (dz == 1 && bz != EDGE - 1) {
                    continue;
                }
                out.insert((sx + dx, sz + dz, sy + dy));
            }
        }
    }
}

/// Seed and flood both layers over one job's scratch, recording into `job` how many
/// top-shell columns the openness scan accepted as sky sources and how many of those
/// were seeded from absent rather than sent data.
fn flood(scratch: &mut Scratch, has_skylight: bool, job: &mut RelitJob) {
    let dims = scratch.dims;
    let n = scratch.fixed.len();

    let mut sky_buckets = Buckets::new();
    let mut block_buckets = Buckets::new();

    for idx in 0..n {
        if scratch.fixed[idx] {
            // The shell keeps its stored light and seeds the flood.
            scratch.sky[idx] = scratch.stored_sky[idx];
            scratch.block[idx] = scratch.stored_block[idx];
            sky_buckets.push(scratch.sky[idx], idx);
            block_buckets.push(scratch.block[idx], idx);
        } else if scratch.emission[idx] > 0 {
            // A source holds its emission regardless of its own opacity — a
            // jack-o'-lantern is opaque yet lit.
            scratch.block[idx] = scratch.emission[idx];
            block_buckets.push(scratch.emission[idx], idx);
        }
    }

    if has_skylight {
        // Openness descends from the top shell, where sky light 15 *is* the
        // is-a-sky-source predicate: propagation costs at least one level, so no
        // cell that merely received light can hold 15.
        for lz in 0..dims[2] {
            for lx in 0..dims[0] {
                let top = index(dims, lx, dims[1] - 1, lz);
                if scratch.sky[top] != MAX_LIGHT {
                    continue;
                }
                job.sky_source_columns += 1;
                if scratch.sky_from_missing[top] {
                    job.sky_source_columns_from_missing += 1;
                }
                for ly in (0..dims[1] - 1).rev() {
                    let idx = index(dims, lx, ly, lz);
                    if scratch.opacity[idx] != 0 {
                        break;
                    }
                    if !scratch.fixed[idx] {
                        scratch.sky[idx] = MAX_LIGHT;
                        sky_buckets.push(MAX_LIGHT, idx);
                    }
                }
            }
        }
    }

    // Split the borrows: `propagate` reads the opacity and fixed planes while
    // writing one light plane.
    let Scratch {
        opacity,
        fixed,
        sky,
        block,
        ..
    } = scratch;
    propagate(dims, opacity, fixed, sky, &mut sky_buckets);
    propagate(dims, opacity, fixed, block, &mut block_buckets);
}

impl World {
    /// Runs vanilla's own bounded-box recompute for every block change recorded
    /// since the last drain, and reports which section meshes that invalidates.
    ///
    /// Call this once per frame or tick — it is the client's own per-tick light
    /// update drain.
    /// `props` must be keyed on the same block-state id space this world's sections
    /// hold; `has_skylight` is the connected dimension's own flag, and getting it
    /// wrong floods the Nether with daylight.
    ///
    /// This does not contradict [`merge_light`](World::merge_light)'s rule that the
    /// world *stores* light: the authority is still the server where one exists, and
    /// a patch from it drops any pending relight for its chunk. This fills the gap a
    /// real server leaves — see the module docs for the `getPlayers(pos, true)`
    /// broadcast that creates it.
    ///
    /// Returns [`Relit`], whose `dirty_sections` the caller **must** feed to its
    /// mesher: light that reaches no re-mesh reaches no pixels.
    pub fn run_pending_relight(
        &mut self,
        props: &impl LightProperties,
        has_skylight: bool,
    ) -> Relit {
        let mut out = Relit::default();
        if self.pending_relight.is_empty() {
            return out;
        }

        // Coalesce by section: positions sharing a section become one job, so a
        // `section_blocks_update` of 4096 cells is one box, not 4096.
        let pending = std::mem::take(&mut self.pending_relight);
        let mut jobs: BTreeMap<(i32, i32, i32), Vec<[i32; 3]>> = BTreeMap::new();
        for p in pending {
            jobs.entry((
                p[0].div_euclid(EDGE),
                p[1].div_euclid(EDGE),
                p[2].div_euclid(EDGE),
            ))
            .or_default()
            .push(p);
        }

        let mut spent = 0usize;
        for (_key, changes) in jobs {
            if spent >= RELIGHT_CELL_BUDGET {
                // Requeue whole, so the next drain runs it with the same bounds.
                self.pending_relight.extend(changes);
                out.deferred += 1;
                continue;
            }
            let Some(region) = self.region_for(&changes, props) else {
                continue;
            };
            let cells = region.cells();
            if cells > RELIGHT_JOB_CEILING {
                // Counted, not logged: this crate carries no `tracing` dependency
                // by design, and the host driving the drain reads
                // [`Relit::dropped`] and owns the warning.
                out.dropped += 1;
                continue;
            }
            spent += cells;
            out.input_blocks += changes.len();
            out.jobs += 1;
            out.cells_visited += cells;

            let mut job = RelitJob {
                region_min: region.min,
                region_max: region.max,
                change: *changes.first().expect("region_for returned Some"),
                changes: changes.len(),
                ..RelitJob::default()
            };
            let mut scratch = Scratch::new(&region);
            self.fill_scratch(&region, props, has_skylight, &mut scratch);
            flood(&mut scratch, has_skylight, &mut job);
            self.write_back(&region, &scratch, &mut out, &mut job);
            if out.detail.len() < RELIT_DETAIL_CAP {
                out.detail.push(job);
            }
        }
        out
    }

    /// The box one job recomputes: the changes' bounding box, extended down each
    /// change's sky run, then expanded by [`AFFECTED_RADIUS`] and clamped to the
    /// column's light range.
    ///
    /// `None` when the owning chunk is not loaded — nothing to relight, and no
    /// extent to read.
    fn region_for(&self, changes: &[[i32; 3]], props: &impl LightProperties) -> Option<Region> {
        let first = *changes.first()?;
        let chunk = self.get(ChunkPos::from_block(first[0], first[2]))?;
        let min_y = chunk.column.min_y();
        let max_y = chunk.column.max_y();
        // The light range is one apron section past the block range at each end,
        // matching `ColumnLight`'s `section_count + 2`.
        let (light_lo, light_hi) = (min_y - EDGE, max_y + EDGE - 1);

        let mut lo = first;
        let mut hi = first;
        for &c in changes {
            for a in 0..3 {
                lo[a] = lo[a].min(c[a]);
                hi[a] = hi[a].max(c[a]);
            }
            // A change can promote or demote every transparent cell below it to a
            // sky source, so the run's bottom joins the bounding box *before* the
            // radius is applied.
            if let Some(owner) = self.get(ChunkPos::from_block(c[0], c[2])) {
                let run = sky_run_bottom(
                    c[0].rem_euclid(EDGE) as usize,
                    c[1],
                    c[2].rem_euclid(EDGE) as usize,
                    owner.column.min_y(),
                    &owner.column,
                    props,
                );
                lo[1] = lo[1].min(run);
            }
        }

        Some(Region {
            min: [
                lo[0] - AFFECTED_RADIUS,
                (lo[1] - AFFECTED_RADIUS).max(light_lo),
                lo[2] - AFFECTED_RADIUS,
            ],
            max: [
                hi[0] + AFFECTED_RADIUS,
                (hi[1] + AFFECTED_RADIUS).min(light_hi),
                hi[2] + AFFECTED_RADIUS,
            ],
        })
    }

    /// Read blocks and stored light for every cell of `region`, one chunk at a time
    /// so the hash lookup is paid nine times rather than per cell.
    fn fill_scratch(
        &self,
        region: &Region,
        props: &impl LightProperties,
        has_skylight: bool,
        scratch: &mut Scratch,
    ) {
        let dims = scratch.dims;
        for cz in region.min[2].div_euclid(EDGE)..=region.max[2].div_euclid(EDGE) {
            for cx in region.min[0].div_euclid(EDGE)..=region.max[0].div_euclid(EDGE) {
                let Some(chunk) = self.get(ChunkPos::new(cx, cz)) else {
                    // Left as `Scratch::new`'s default: opaque, unlit, fixed — an
                    // honest barrier. Inventing light behind an unloaded chunk is
                    // the fictional-data failure this project keeps catching.
                    continue;
                };
                let min_y = chunk.column.min_y();
                let light_sections = chunk.light.light_section_count() as i32;
                let x_lo = region.min[0].max(cx * EDGE);
                let x_hi = region.max[0].min(cx * EDGE + EDGE - 1);
                let z_lo = region.min[2].max(cz * EDGE);
                let z_hi = region.max[2].min(cz * EDGE + EDGE - 1);
                for y in region.min[1]..=region.max[1] {
                    let ls = light_section_of(y, min_y);
                    if ls < 0 || ls >= light_sections {
                        // Outside the column's light range; keep the barrier
                        // default so nothing floods out of the world.
                        continue;
                    }
                    let ls = ls as usize;
                    let ly = (y - region.min[1]) as usize;
                    let y_in_section = (y - min_y).rem_euclid(EDGE) as usize;
                    let on_y_shell = y == region.min[1] || y == region.max[1];
                    for z in z_lo..=z_hi {
                        let sz = (z - cz * EDGE) as usize;
                        let lz = (z - region.min[2]) as usize;
                        let on_z_shell = z == region.min[2] || z == region.max[2];
                        for x in x_lo..=x_hi {
                            let sx = (x - cx * EDGE) as usize;
                            let lx = (x - region.min[0]) as usize;
                            let idx = index(dims, lx, ly, lz);
                            let nibble = NibbleArray::index(sx, y_in_section, sz);
                            let state = chunk.column.get_block(sx, y, sz);
                            scratch.opacity[idx] = props.opacity(state).min(MAX_LIGHT);
                            scratch.emission[idx] = props.emission(state).min(MAX_LIGHT);
                            // A `Missing` sky section is not darkness — see the
                            // module docs. This must agree with the mesher's
                            // `SkyDefault`, or the two disagree about the same cell.
                            scratch.stored_sky[idx] = match chunk.light.sky(ls) {
                                LightData::Missing if has_skylight => {
                                    scratch.sky_from_missing[idx] = true;
                                    MAX_LIGHT
                                }
                                other => other.get(nibble).unwrap_or(0),
                            };
                            scratch.stored_block[idx] =
                                chunk.light.block(ls).get(nibble).unwrap_or(0);
                            scratch.fixed[idx] = on_y_shell
                                || on_z_shell
                                || x == region.min[0]
                                || x == region.max[0];
                        }
                    }
                }
            }
        }
    }

    /// Diff the recomputed light against what was stored, write only the cells that
    /// moved, and record the sections whose mesh that invalidates.
    ///
    /// `job` collects the **signed** split of that diff. It is deliberately taken
    /// alongside `out` rather than derived from it afterwards: once the values are
    /// written, storage no longer holds what they replaced, so the direction each
    /// cell moved is only observable here.
    fn write_back(
        &mut self,
        region: &Region,
        scratch: &Scratch,
        out: &mut Relit,
        job: &mut RelitJob,
    ) {
        let dims = scratch.dims;
        for cz in region.min[2].div_euclid(EDGE)..=region.max[2].div_euclid(EDGE) {
            for cx in region.min[0].div_euclid(EDGE)..=region.max[0].div_euclid(EDGE) {
                let Some(chunk) = self.get_mut(ChunkPos::new(cx, cz)) else {
                    continue;
                };
                let min_y = chunk.column.min_y();
                let light_sections = chunk.light.light_section_count() as i32;
                let x_lo = region.min[0].max(cx * EDGE);
                let x_hi = region.max[0].min(cx * EDGE + EDGE - 1);
                let z_lo = region.min[2].max(cz * EDGE);
                let z_hi = region.max[2].min(cz * EDGE + EDGE - 1);
                for y in region.min[1]..=region.max[1] {
                    let ls = light_section_of(y, min_y);
                    if ls < 0 || ls >= light_sections {
                        continue;
                    }
                    let ls = ls as usize;
                    let ly = (y - region.min[1]) as usize;
                    let y_in_section = (y - min_y).rem_euclid(EDGE) as usize;
                    for z in z_lo..=z_hi {
                        let sz = (z - cz * EDGE) as usize;
                        let lz = (z - region.min[2]) as usize;
                        for x in x_lo..=x_hi {
                            let sx = (x - cx * EDGE) as usize;
                            let lx = (x - region.min[0]) as usize;
                            let idx = index(dims, lx, ly, lz);
                            if scratch.fixed[idx] {
                                continue;
                            }
                            let nibble = NibbleArray::index(sx, y_in_section, sz);
                            let mut moved = false;
                            if scratch.sky[idx] != scratch.stored_sky[idx] {
                                // An absent sky section was *read* as full daylight
                                // above, but `LightData::set` materialises one from
                                // **zero** — it cannot know the dimension, and says
                                // so. Poking a single nibble into it therefore
                                // rewrites the other 4,095 cells from daylight to
                                // darkness. Establish the section at the value this
                                // job actually read before writing into it, once:
                                // after the first write it is no longer absent, and
                                // re-filling would discard the writes already made.
                                if scratch.sky_from_missing[idx]
                                    && matches!(chunk.light.sky(ls), LightData::Missing)
                                {
                                    *chunk.light.sky_mut(ls) = LightData::Uniform(MAX_LIGHT);
                                }
                                if scratch.sky[idx] > scratch.stored_sky[idx] {
                                    job.sky_raised += 1;
                                    job.max_sky_gain = job
                                        .max_sky_gain
                                        .max(scratch.sky[idx] - scratch.stored_sky[idx]);
                                } else {
                                    job.sky_lowered += 1;
                                }
                                chunk.light.set_sky_light(ls, nibble, scratch.sky[idx]);
                                moved = true;
                            }
                            if scratch.block[idx] != scratch.stored_block[idx] {
                                if scratch.block[idx] > scratch.stored_block[idx] {
                                    job.block_raised += 1;
                                    job.max_block_gain = job
                                        .max_block_gain
                                        .max(scratch.block[idx] - scratch.stored_block[idx]);
                                } else {
                                    job.block_lowered += 1;
                                }
                                chunk.light.set_block_light(ls, nibble, scratch.block[idx]);
                                moved = true;
                            }
                            if moved {
                                out.cells_changed += 1;
                                mark_dirty_sections(x, y, z, &mut out.dirty_sections);
                            }
                        }
                    }
                }
            }
        }
    }
}
