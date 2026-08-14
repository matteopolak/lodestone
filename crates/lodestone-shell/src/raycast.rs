//! Voxel ray casting for block targeting — the front half of the interaction
//! loop (look at a block, then break or place against it).
//!
//! This is the Amanatides–Woo grid-DDA traversal: from the eye, step voxel by
//! voxel along the view ray and, **in each visited cell, clip the ray against
//! that cell's real outline boxes**, stopping at the nearest box entry within
//! reach. It is a pure function of a box-emitting closure so it can be
//! unit-tested against a synthetic world with no GPU, no window, and no
//! `lodestone-world` at all.
//!
//! The direction is taken straight from [`lodestone_render::Camera::forward`] so
//! the shell never re-derives the yaw/pitch → direction convention (the render
//! crate owns it, reconciled against vanilla). Vanilla's block-interaction reach
//! is 4.5 blocks from the eye; [`REACH`] matches.
//!
//! # That fix — a cell is not a cube
//!
//! This used to take an `is_solid(x, y, z) -> bool` occupancy predicate, so
//! **every pickable block was a unit cube to the hit test** while the selection
//! box on screen was already drawn from the real outline census. Reported from
//! play: leaf litter (`1/16` of a block tall) stayed highlighted and stayed
//! targetable with the crosshair plainly above it, because the ray only ever
//! asked "is this cell occupied", never "does the ray pass through the shape in
//! it".
//!
//! Vanilla is `Entity.pick` → `Level.clip` with `ClipContext.Block.OUTLINE`
//! (`Entity.java`), which walks the same DDA (`BlockGetter
//! .traverseBlocks`) and clips each cell's `state.getShape(…).toAabbs()` with
//! `AABB.clip`. [`raycast`] now does exactly that, and two consequences follow
//! that a cube-shaped hit test could not express:
//!
//! * **the hit face comes from the box that was actually hit**, not from the
//!   cell boundary the DDA crossed. Placement is face-driven, so a cube-derived
//!   face puts a block on the wrong side of a thin block even once the hit test
//!   itself is right;
//! * **the hit point is exact** ([`RayHit::hit`]), which is what vanilla's
//!   `BlockHitResult` carries and what `use_item_on`'s cursor field wants.
//!
//! An empty box list means "not targetable", and that is a real answer — air,
//! water, lava and `minecraft:light` all have an empty vanilla outline. There is
//! deliberately no cube fallback for it.

/// Vanilla block-interaction range, in blocks, measured from the eye.
pub const REACH: f64 = 4.5;

/// A block-local axis-aligned box the pick ray is clipped against — one entry of
/// vanilla's `state.getShape(…).toAabbs()`.
///
/// Coordinates are **block-local**, in the same `0..1`-per-cell space the version
/// census uses (`VersionAdapter::block_outline`), so a caller can hand the census
/// slice straight over without translating it; [`raycast`] offsets by the cell it
/// is visiting. They are *not* clamped to `0..1` — vanilla's outline census
/// ranges `-0.25..=1.25` (`pitcher_crop` reaches below zero).
///
/// Plain `f64` triples rather than [`lodestone_physics::Aabb`] or
/// [`lodestone_model::BlockAabb`] for the reason this module's docs give: it
/// stays a pure geometry module with no world, GPU or version dependency. The
/// two [`crate::collision`] adapters do the one-line widening from the `f32`
/// census type.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PickBox {
    /// Block-local minimum corner.
    pub min: [f64; 3],
    /// Block-local maximum corner.
    pub max: [f64; 3],
}

impl PickBox {
    /// The whole cell — the shape of a full block, and what the degraded
    /// no-version-census tier reduces every pickable block to.
    pub const CUBE: Self = Self {
        min: [0.0, 0.0, 0.0],
        max: [1.0, 1.0, 1.0],
    };
}

/// A block the view ray struck.
///
/// Not `Eq`: [`hit`](Self::hit) and [`distance`](Self::distance) are floats. No
/// consumer keys a map on a hit, and comparing two hits for exact equality would
/// be the wrong question anyway.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RayHit {
    /// World coordinates of the block that was hit.
    pub block: [i32; 3],
    /// Unit face normal of the struck face, pointing back toward the eye
    /// (e.g. `[0, 1, 0]` when the ray hit the top of a box). This is the face of
    /// the **outline box** that was hit, not of the cell: a ray that enters a
    /// leaf-litter cell through its `-X` side and then meets the `1/16`-tall
    /// litter box from above reports `[0, 1, 0]`, exactly as vanilla's
    /// `AABB.clip` does. Also the offset to the cell a placed block would
    /// occupy against that face.
    pub normal: [i32; 3],
    /// Exact world-space point where the ray entered the box — vanilla's
    /// `BlockHitResult.getLocation()`. `use_item_on`'s cursor field is this
    /// minus [`block`](Self::block); see [`Self::cursor`].
    pub hit: [f64; 3],
    /// Distance from the eye to [`hit`](Self::hit), in blocks along the
    /// normalised ray. This is what shortens the entity pick's search radius so
    /// an entity behind a block is never picked through it.
    pub distance: f64,
}

impl RayHit {
    /// A synthetic hit at the centre of one of the cell's own cube faces, for
    /// callers that inject a target instead of casting a ray (tests, and any
    /// future "target this block" command).
    ///
    /// [`distance`](Self::distance) is `0.0`: there is no eye in the picture, and
    /// its only consumer is the entity-reach clamp inside the cast itself.
    #[must_use]
    pub fn face_center(block: [i32; 3], normal: [i32; 3]) -> Self {
        let coord = |b: i32, n: i32| -> f64 {
            f64::from(b)
                + match n.signum() {
                    1 => 1.0,
                    -1 => 0.0,
                    _ => 0.5,
                }
        };
        Self {
            block,
            normal,
            hit: [
                coord(block[0], normal[0]),
                coord(block[1], normal[1]),
                coord(block[2], normal[2]),
            ],
            distance: 0.0,
        }
    }

    /// The cell adjacent to the hit face — where a placed block would go.
    #[must_use]
    pub fn place_position(&self) -> [i32; 3] {
        [
            self.block[0] + self.normal[0],
            self.block[1] + self.normal[1],
            self.block[2] + self.normal[2],
        ]
    }

    /// The hit point in **block-local** coordinates, which is exactly the cursor
    /// vector `ServerboundUseItemOnPacket` carries (vanilla writes
    /// `location - blockPos`, `writeBlockHitResult`).
    ///
    /// Narrowed to `f32` because that is the wire type. Not clamped to `0..1`:
    /// a box may legitimately reach outside its cell, and vanilla sends the raw
    /// difference.
    #[must_use]
    pub fn cursor(&self) -> [f32; 3] {
        [
            (self.hit[0] - f64::from(self.block[0])) as f32,
            (self.hit[1] - f64::from(self.block[1])) as f32,
            (self.hit[2] - f64::from(self.block[2])) as f32,
        ]
    }
}

/// Cast a ray from `origin` along `dir` (need not be normalised) up to `reach`
/// blocks, returning the nearest outline box it entered.
///
/// `pick_boxes(x, y, z, out)` appends the **block-local** ([`PickBox`]) outline
/// boxes of the cell at `(x, y, z)`; appending nothing means the cell cannot be
/// targeted, which is the right answer for air, water, lava and
/// `minecraft:light`. `out` is cleared before each call and reused across cells,
/// so the whole cast allocates at most once.
///
/// Traversal visits the origin cell first and then steps cell by cell, and
/// within a cell the **nearest** box wins — so a block whose outline is several
/// disjoint boxes (a fence's post and arms, a pane's cross, a stair's two
/// slabs) is tested in full. A box the origin is already *inside* is skipped,
/// which is what vanilla's `AABB.clip` does (it only ever reports a face
/// crossing), so standing inside a cobweb does not target it.
#[must_use]
pub fn raycast(
    origin: [f64; 3],
    dir: [f64; 3],
    reach: f64,
    pick_boxes: impl Fn(i32, i32, i32, &mut Vec<PickBox>),
) -> Option<RayHit> {
    let d = normalise(dir)?;

    let mut voxel = [
        origin[0].floor() as i32,
        origin[1].floor() as i32,
        origin[2].floor() as i32,
    ];
    let step = [sign(d[0]), sign(d[1]), sign(d[2])];

    // Distance (in ray-length units) to the first cell boundary on each axis,
    // and the per-cell increment thereafter.
    let mut t_max = [0.0f64; 3];
    let mut t_delta = [0.0f64; 3];
    for a in 0..3 {
        if d[a] == 0.0 {
            t_max[a] = f64::INFINITY;
            t_delta[a] = f64::INFINITY;
        } else {
            let next = if d[a] > 0.0 {
                f64::from(voxel[a]) + 1.0
            } else {
                f64::from(voxel[a])
            };
            t_max[a] = (next - origin[a]) / d[a];
            t_delta[a] = (1.0 / d[a]).abs();
        }
    }

    // One buffer for the whole cast; `clip_cell` clears it per cell.
    let mut boxes = Vec::new();

    // A generous cap so a degenerate ray can never loop forever. `+1` over the
    // pre-fix bound because the origin cell is now visited too.
    for _ in 0..(reach.ceil() as i32 * 3 + 9) {
        if let Some(hit) = clip_cell(origin, d, reach, voxel, &pick_boxes, &mut boxes) {
            return Some(hit);
        }
        // Advance across the nearest axis boundary.
        let axis = if t_max[0] < t_max[1] && t_max[0] < t_max[2] {
            0
        } else if t_max[1] < t_max[2] {
            1
        } else {
            2
        };
        voxel[axis] += step[axis];
        let t = t_max[axis];
        t_max[axis] += t_delta[axis];
        // The cell is entered beyond reach, so nothing inside it can be within
        // reach either: every box of a cell lies in that cell's `t` span.
        if t > reach {
            return None;
        }
    }
    None
}

/// The nearest outline box of one cell that the ray enters, as a [`RayHit`].
///
/// Cell-at-a-time is enough for "nearest box overall" because the DDA visits
/// cells in increasing entry distance and a cell's boxes lie within that cell's
/// own `t` span — so the first cell that yields any hit yields *the* hit.
fn clip_cell(
    origin: [f64; 3],
    d: [f64; 3],
    reach: f64,
    voxel: [i32; 3],
    pick_boxes: &impl Fn(i32, i32, i32, &mut Vec<PickBox>),
    boxes: &mut Vec<PickBox>,
) -> Option<RayHit> {
    boxes.clear();
    pick_boxes(voxel[0], voxel[1], voxel[2], boxes);
    let cell = [
        f64::from(voxel[0]),
        f64::from(voxel[1]),
        f64::from(voxel[2]),
    ];
    let mut best: Option<(f64, [i32; 3])> = None;
    for b in boxes.iter() {
        let min = [
            cell[0] + b.min[0],
            cell[1] + b.min[1],
            cell[2] + b.min[2],
        ];
        let max = [
            cell[0] + b.max[0],
            cell[1] + b.max[1],
            cell[2] + b.max[2],
        ];
        let Some((t, normal)) = clip_box(origin, d, reach, min, max) else {
            continue;
        };
        // `[0, 0, 0]` is "the origin is already inside this box", which vanilla's
        // `AABB.clip` reports as no crossing at all.
        if normal != [0, 0, 0] && best.is_none_or(|(bt, _)| t < bt) {
            best = Some((t, normal));
        }
    }
    let (t, normal) = best?;
    Some(RayHit {
        block: voxel,
        normal,
        hit: [
            origin[0] + d[0] * t,
            origin[1] + d[1] * t,
            origin[2] + d[2] * t,
        ],
        distance: t,
    })
}

/// Ray-vs-AABB slab test, mirroring vanilla's `AABB.clip(Vec3, Vec3)` used by
/// `Entity.getClippedBounds`/`ProjectileUtil`-style entity picking.
///
/// `dir` need not be normalised (matches [`raycast`]'s convention); `reach` is
/// in the same blocks-along-the-normalised-ray units as [`raycast`]'s. Returns
/// the entry distance in blocks when the ray hits the box within
/// `0..=reach`, `None` on a miss, behind the origin, or a degenerate
/// direction. The box itself is given as plain `min`/`max` triples rather than
/// [`lodestone_physics::Aabb`] so this module keeps the "no `lodestone-world`,
/// no GPU" independence its own docs promise — a caller with an `Aabb` passes
/// `[aabb.min_x, aabb.min_y, aabb.min_z]` / `[aabb.max_x, ...]`.
#[must_use]
pub fn ray_aabb(
    origin: [f64; 3],
    dir: [f64; 3],
    reach: f64,
    aabb_min: [f64; 3],
    aabb_max: [f64; 3],
) -> Option<f64> {
    let d = normalise(dir)?;
    clip_box(origin, d, reach, aabb_min, aabb_max).map(|(t, _)| t)
}

/// The slab clip both [`raycast`] and [`ray_aabb`] are built on: entry distance
/// plus the **face the ray entered through**, as a unit normal pointing back
/// along the ray. `d` must already be normalised (use [`normalise`]).
///
/// `[0, 0, 0]` for the normal means the origin is already inside the box, in
/// which case `t` is `0.0`. The two callers want opposite things there, which is
/// why this reports it instead of choosing:
///
/// * **blocks** ([`raycast`]) skip such a box, matching vanilla's `AABB.clip`,
///   which is written in terms of face crossings and returns
///   `Optional.empty()` for a start point inside;
/// * **entities** ([`ray_aabb`]) treat it as a hit at distance zero, matching
///   `ProjectileUtil.getEntityHitResult`, which special-cases
///   `aabb.contains(from)` before consulting `clip`.
fn clip_box(
    origin: [f64; 3],
    d: [f64; 3],
    reach: f64,
    aabb_min: [f64; 3],
    aabb_max: [f64; 3],
) -> Option<(f64, [i32; 3])> {
    let mut t_min = 0.0f64;
    let mut t_max = reach;
    // The axis whose *near* plane the ray crossed last is the face it entered
    // through; `None` while no near crossing has been seen, i.e. the origin is
    // inside every slab tested so far.
    let mut entry_axis: Option<usize> = None;
    for a in 0..3 {
        if d[a].abs() < 1e-12 {
            // Parallel to this axis: a hit requires the origin to already lie
            // within the slab, or the ray never enters it on this axis.
            if origin[a] < aabb_min[a] || origin[a] > aabb_max[a] {
                return None;
            }
            continue;
        }
        let inv = 1.0 / d[a];
        let mut t1 = (aabb_min[a] - origin[a]) * inv;
        let mut t2 = (aabb_max[a] - origin[a]) * inv;
        if t1 > t2 {
            std::mem::swap(&mut t1, &mut t2);
        }
        if t1 > t_min {
            t_min = t1;
            entry_axis = Some(a);
        }
        t_max = t_max.min(t2);
        if t_min > t_max {
            return None;
        }
    }
    let mut normal = [0, 0, 0];
    if let Some(a) = entry_axis {
        // Pointing back toward the eye: the ray travelling `+X` entered the
        // box's `-X` face.
        normal[a] = -sign(d[a]);
    }
    Some((t_min, normal))
}

/// `dir` as a unit vector, or `None` for a degenerate or non-finite direction.
fn normalise(dir: [f64; 3]) -> Option<[f64; 3]> {
    let len = (dir[0] * dir[0] + dir[1] * dir[1] + dir[2] * dir[2]).sqrt();
    if !len.is_finite() || len < 1e-9 {
        return None;
    }
    Some([dir[0] / len, dir[1] / len, dir[2] / len])
}

fn sign(v: f64) -> i32 {
    if v > 0.0 {
        1
    } else if v < 0.0 {
        -1
    } else {
        0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Adapt a boolean occupancy predicate to the box emitter, one full cube per
    /// occupied cell — the traversal-only fixture the DDA tests below want, and
    /// exactly what the degraded no-census tier feeds the ray in production.
    fn cubes(
        pred: impl Fn(i32, i32, i32) -> bool,
    ) -> impl Fn(i32, i32, i32, &mut Vec<PickBox>) {
        move |x, y, z, out| {
            if pred(x, y, z) {
                out.push(PickBox::CUBE);
            }
        }
    }

    /// A flat solid floor at all `y < 10`.
    fn floor(_x: i32, y: i32, _z: i32) -> bool {
        y < 10
    }

    #[test]
    fn looking_down_hits_the_floor_from_above() {
        // Eye at y=12 looking straight down hits the top of the y=9 block.
        let hit = raycast([0.5, 12.0, 0.5], [0.0, -1.0, 0.0], REACH, cubes(floor))
            .expect("should hit the floor");
        assert_eq!(hit.block, [0, 9, 0]);
        assert_eq!(hit.normal, [0, 1, 0], "hit the top face");
        assert_eq!(hit.place_position(), [0, 10, 0], "place goes on top");
        assert_eq!(hit.hit, [0.5, 10.0, 0.5], "entered the cube's top face");
        assert!((hit.distance - 2.0).abs() < 1e-12, "2 blocks down");
    }

    #[test]
    fn out_of_reach_misses() {
        // Floor top is at y=10; eye at y=20 looking down is >4.5 away.
        assert!(raycast([0.5, 20.0, 0.5], [0.0, -1.0, 0.0], REACH, cubes(floor)).is_none());
    }

    #[test]
    fn looking_up_into_empty_sky_misses() {
        assert!(raycast([0.5, 12.0, 0.5], [0.0, 1.0, 0.0], REACH, cubes(floor)).is_none());
    }

    #[test]
    fn side_face_normal_points_back_at_the_eye() {
        // A single wall block at x=3; ray travels +X into its −X face.
        let wall = |x: i32, _y: i32, _z: i32| x == 3;
        let hit =
            raycast([0.5, 0.5, 0.5], [1.0, 0.0, 0.0], 10.0, cubes(wall)).expect("hits wall");
        assert_eq!(hit.block, [3, 0, 0]);
        assert_eq!(hit.normal, [-1, 0, 0]);
        assert_eq!(hit.cursor(), [0.0, 0.5, 0.5], "the −X face, half up, mid-cell");
    }

    #[test]
    fn degenerate_direction_is_none() {
        assert!(raycast([0.0, 0.0, 0.0], [0.0, 0.0, 0.0], REACH, cubes(floor)).is_none());
    }

    #[test]
    fn diagonal_ray_still_lands_on_a_solid_cell() {
        let hit = raycast([0.5, 12.0, 0.5], [0.3, -1.0, 0.2], REACH, cubes(floor))
            .expect("angled ray still reaches the floor");
        assert!(
            hit.block[1] == 9,
            "landed on the floor surface, got {:?}",
            hit.block
        );
    }

    #[test]
    fn a_cell_with_no_boxes_is_never_targeted() {
        // Vanilla's answer for air, water, lava and `minecraft:light`: an empty
        // outline is a real answer, not a data gap, so the ray walks through.
        assert!(
            raycast(
                [0.5, 12.0, 0.5],
                [0.0, -1.0, 0.0],
                REACH,
                |_x, _y, _z, _out: &mut Vec<PickBox>| {}
            )
            .is_none()
        );
    }

    #[test]
    fn a_box_the_eye_is_already_inside_is_not_targeted() {
        // Vanilla's `AABB.clip` is written in terms of face crossings and reports
        // nothing for a start point inside the box — which is why standing inside
        // a cobweb (a full-cube outline with no collision) does not target it.
        // `ray_aabb` deliberately answers the *other* way for entities.
        // The eye's own cell only, so the miss is about that box and nothing else.
        let here = |x: i32, y: i32, z: i32, out: &mut Vec<PickBox>| {
            if [x, y, z] == [0, 0, 0] {
                out.push(PickBox::CUBE);
            }
        };
        assert!(raycast([0.5, 0.5, 0.5], [0.0, 0.0, -1.0], REACH, here).is_none());
        assert_eq!(
            ray_aabb(
                [0.5, 0.5, 0.5],
                [0.0, 0.0, -1.0],
                REACH,
                [0.0, 0.0, 0.0],
                [1.0, 1.0, 1.0]
            ),
            Some(0.0),
            "control: the entity form must still report a zero-distance hit, \
             matching ProjectileUtil's aabb.contains(from) special case"
        );
    }

    // -----------------------------------------------------------------------
    // Thin and multi-box outlines
    // -----------------------------------------------------------------------
    //
    // Every box below is hand-transcribed from 26.2's own source, so the
    // expected geometry originates outside this module:
    //
    // * `CarpetBlock.java` — `SHAPE = Block.column(16.0, 0.0, 1.0)`, i.e.
    //   x/z `0..16` and y `0..1` in sixteenths: **1/16 of a block tall**.
    //   `LeafLitterBlock` is the same height via `SegmentableBlock
    //   .getShapeHeight() = 1.0` (`SegmentableBlock.java`), over a
    //   quarter of the cell per segment.
    // * `StairBlock.java` — `SHAPE_OUTER = or(column(16, 0, 8),
    //   box(0, 8, 0, 8, 16, 8))` and `SHAPE_STRAIGHT` unions its 90° rotation:
    //   a full-width **half-height slab** plus a **half-width upper step**, two
    //   disjoint boxes.
    //
    // `crate::collision`'s `the_view_ray_clips_a_flat_blocks_real_outline_box`
    // is the other half of this pair: it runs the same rays through the real
    // per-state census, so neither test can pass on invented geometry.

    /// One-sixteenth of a block: a carpet's and leaf litter's outline height.
    const LITTER_TOP: f64 = 1.0 / 16.0;

    /// A single `1/16`-tall box filling cell `(0, 0, 0)` in x and z, and nothing
    /// anywhere else. Vanilla `Block.column(16.0, 0.0, 1.0)`.
    fn flat_litter(x: i32, y: i32, z: i32, out: &mut Vec<PickBox>) {
        if [x, y, z] == [0, 0, 0] {
            out.push(PickBox {
                min: [0.0, 0.0, 0.0],
                max: [1.0, LITTER_TOP, 1.0],
            });
        }
    }

    /// **The reported bug.** A horizontal ray through the flat block's *cell* at
    /// eye height passes well above its box, so vanilla does not target it — and
    /// before that fix this hit, because the cell was tested as a unit cube.
    ///
    /// The pair below the miss is the magnitude control: the transition is
    /// bracketed either side of `1/16`, so neither "reject everything" (which
    /// would fail the `hits` case) nor "accept everything" (the pre-fix
    /// behaviour, which fails the `misses` case) can pass. The offset is checked,
    /// not just the direction.
    #[test]
    fn a_ray_above_a_flat_blocks_box_misses_it_but_one_through_the_box_hits() {
        // 0.5 blocks up — eight times the box's own height.
        assert!(
            raycast([0.5, 0.5, 2.5], [0.0, 0.0, -1.0], REACH, flat_litter).is_none(),
            "a ray at y = 0.5 passes 7/16 of a block above a 1/16-tall box"
        );

        // Just above the box's top: still a miss.
        assert!(
            raycast(
                [0.5, LITTER_TOP + 0.01, 2.5],
                [0.0, 0.0, -1.0],
                REACH,
                flat_litter
            )
            .is_none(),
            "0.01 above the box top must still miss — the boundary is 1/16, not \
             some coarser approximation"
        );

        // Just below it: a hit, through the box's +Z face at the cell boundary.
        let hit = raycast(
            [0.5, LITTER_TOP - 0.01, 2.5],
            [0.0, 0.0, -1.0],
            REACH,
            flat_litter,
        )
        .expect("0.01 below the box top must hit");
        assert_eq!(hit.block, [0, 0, 0]);
        assert_eq!(hit.normal, [0, 0, 1], "entered through the box's +Z side");
        assert!(
            (hit.hit[2] - 1.0).abs() < 1e-12 && (hit.hit[1] - (LITTER_TOP - 0.01)).abs() < 1e-12,
            "entry point is the cell's z = 1 plane at the ray's own height, got {:?}",
            hit.hit
        );
    }

    /// **The negative control, kept live.** The pre-fix hit test is still
    /// expressible — it is [`cubes`], one unit cube per occupied cell — so the
    /// bug can be re-run rather than described. Both rays the test above asserts
    /// *miss* were run against it and asserted `is_none()`; both failed
    /// (`pre-fix: a ray 7/16 above the box still hit`). This pins what they do
    /// instead, so the control cannot rot into a premise that was never true:
    ///
    /// * the horizontal ray 7/16 of a block above the litter **hits**, which is
    ///   the reported symptom — a highlighted, targetable, punchable block the
    ///   crosshair is plainly not on;
    /// * the angled ray reports the face of the **cell boundary** it crossed, so
    ///   a placement went one cell south of the litter instead of on top of it.
    #[test]
    fn the_pre_fix_cube_hit_test_targets_a_flat_block_the_ray_passes_over() {
        let occupied = cubes(|x, y, z| [x, y, z] == [0, 0, 0]);

        let over = raycast([0.5, 0.5, 2.5], [0.0, 0.0, -1.0], REACH, &occupied)
            .expect("the pre-fix cube test hits — this is the bug");
        assert_eq!(over.block, [0, 0, 0]);
        assert_eq!(
            over.normal,
            [0, 0, 1],
            "and it reports the cell's south face, 7/16 above any real geometry"
        );

        let angled = raycast(
            [0.5, 0.5, 2.5],
            [0.0, -0.468_75, -2.0],
            REACH,
            &occupied,
        )
        .expect("the pre-fix cube test hits here too");
        assert_eq!(
            angled.normal,
            [0, 0, 1],
            "the cell boundary, not the litter's top — the face bug"
        );
        assert_eq!(
            angled.place_position(),
            [0, 0, 1],
            "so a block placed against it landed one cell south of the litter"
        );
    }

    /// **The face comes from the box, not from the cell.** A ray angled down into
    /// the flat block's cell crosses the cell's `z = 1` boundary first — which is
    /// the face the pre-fix traversal reported, together with a
    /// `place_position` one cell *south* of the litter instead of above it — and
    /// then meets the litter box from above.
    ///
    /// Hand-derived: from `(0.5, 0.5, 2.5)` along `(0, -0.46875, -2)`, the ray
    /// reaches `y = 1/16` after `s = 0.4375 / 0.46875 = 14/15` of that vector,
    /// where `z = 2.5 - 2 · 14/15 = 19/30 = 0.63333…`, inside the cell. So the
    /// entry face is `+Y` and the cursor is `(0.5, 0.0625, 0.63333…)`.
    #[test]
    fn the_hit_face_is_the_boxs_face_not_the_cell_boundary() {
        let hit = raycast(
            [0.5, 0.5, 2.5],
            [0.0, -0.468_75, -2.0],
            REACH,
            flat_litter,
        )
        .expect("the angled ray must reach the litter box");
        assert_eq!(hit.block, [0, 0, 0]);
        assert_eq!(
            hit.normal,
            [0, 1, 0],
            "the litter's top face — the cell boundary crossed was +Z, which is \
             what the pre-#375 cube traversal reported"
        );
        assert_eq!(
            hit.place_position(),
            [0, 1, 0],
            "so a block places on top of the litter, not one cell south of it"
        );
        assert!(
            (hit.hit[1] - LITTER_TOP).abs() < 1e-12
                && (hit.hit[2] - 19.0 / 30.0).abs() < 1e-12
                && (hit.hit[0] - 0.5).abs() < 1e-12,
            "hand-derived entry point (0.5, 1/16, 19/30), got {:?}",
            hit.hit
        );
    }

    /// A block whose outline is several disjoint boxes must have **all** of them
    /// tested, and the nearest must win. Vanilla's straight bottom stair:
    /// a full-width slab `y 0..1/2` plus an upper step `x 0..1/2, y 1/2..1`
    /// (`StairBlock.java`).
    #[test]
    fn every_box_of_a_multi_box_outline_is_tested_and_the_nearest_wins() {
        let stair = |x: i32, y: i32, z: i32, out: &mut Vec<PickBox>| {
            if [x, y, z] == [0, 0, 0] {
                out.push(PickBox {
                    min: [0.0, 0.0, 0.0],
                    max: [1.0, 0.5, 1.0],
                });
                out.push(PickBox {
                    min: [0.0, 0.5, 0.0],
                    max: [0.5, 1.0, 1.0],
                });
            }
        };

        // Eye at x = 2.5 travelling −X at y = 0.75: too high for the slab, so
        // only the *second* box can answer. Its +X face is at x = 0.5.
        let hit = raycast([2.5, 0.75, 0.5], [-1.0, 0.0, 0.0], REACH, stair)
            .expect("the upper step must be tested, not just the first box");
        assert_eq!(hit.block, [0, 0, 0]);
        assert_eq!(hit.normal, [1, 0, 0]);
        assert!(
            (hit.hit[0] - 0.5).abs() < 1e-12,
            "entered the step's own +X face at x = 1/2, got {:?}",
            hit.hit
        );

        // Same cell, lower: now the slab is nearer along −X (its face is at
        // x = 1), so the nearest-box rule must prefer it.
        let low = raycast([2.5, 0.25, 0.5], [-1.0, 0.0, 0.0], REACH, stair)
            .expect("the slab is still there");
        assert!(
            (low.hit[0] - 1.0).abs() < 1e-12,
            "the nearer box wins: the slab's +X face at x = 1, got {:?}",
            low.hit
        );
        assert!(
            low.distance < hit.distance,
            "and it is nearer: {} vs {}",
            low.distance,
            hit.distance
        );

        // Above both boxes: a miss, so the two positives above are not satisfied
        // by a cell-shaped hit test.
        assert!(
            raycast([2.5, 1.25, 0.5], [-1.0, 0.0, 0.0], REACH, stair).is_none(),
            "control: a ray over the whole stair must miss"
        );
    }

    /// The ray must keep walking after a cell whose boxes it misses. Pre-that fix it
    /// could not: the first *occupied* cell ended the cast.
    #[test]
    fn a_missed_thin_box_does_not_stop_the_ray_reaching_the_block_behind_it() {
        // Litter in cell (0,0,0), a full cube in cell (0,0,-1) behind it.
        let scene = |x: i32, y: i32, z: i32, out: &mut Vec<PickBox>| {
            flat_litter(x, y, z, out);
            if [x, y, z] == [0, 0, -1] {
                out.push(PickBox::CUBE);
            }
        };
        let hit = raycast([0.5, 0.5, 2.5], [0.0, 0.0, -1.0], REACH, scene)
            .expect("the cube behind the litter is still targetable");
        assert_eq!(
            hit.block,
            [0, 0, -1],
            "the ray passed over the litter box and hit the cube behind it"
        );
        assert_eq!(hit.normal, [0, 0, 1]);
    }

    #[test]
    fn ray_aabb_hits_a_box_dead_ahead() {
        // A 1x2x1 box (a player-shaped hitbox) centred on the origin's +X
        // axis, hit head-on.
        let t = ray_aabb(
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            10.0,
            [2.0, -1.0, -0.5],
            [3.0, 1.0, 0.5],
        )
        .expect("ray should enter the box");
        assert!((t - 2.0).abs() < 1e-9, "entry distance was {t}");
    }

    #[test]
    fn ray_aabb_misses_a_box_off_to_the_side() {
        assert!(
            ray_aabb(
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                10.0,
                [2.0, 5.0, 5.0],
                [3.0, 6.0, 6.0],
            )
            .is_none()
        );
    }

    #[test]
    fn ray_aabb_respects_reach() {
        // Box entry is at t=8, reach is only 4.5 — same "in range but too far"
        // case REACH enforces for blocks.
        assert!(
            ray_aabb(
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                4.5,
                [8.0, -1.0, -1.0],
                [9.0, 1.0, 1.0],
            )
            .is_none()
        );
    }

    #[test]
    fn ray_aabb_picks_the_nearer_of_two_boxes() {
        let near = ray_aabb(
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            10.0,
            [2.0, -1.0, -1.0],
            [3.0, 1.0, 1.0],
        );
        let far = ray_aabb(
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            10.0,
            [5.0, -1.0, -1.0],
            [6.0, 1.0, 1.0],
        );
        assert!(near.unwrap() < far.unwrap(), "the closer box must win a min-by comparison");
    }
}
