//! Fluid geometry — water and lava.
//!
//! Vanilla does **not** render fluids through the block-model pipeline: their
//! blockstate models are empty, and a dedicated `LiquidBlockRenderer` builds the
//! surface at mesh time. So a fluid cell is invisible to [`crate::bake`] and must
//! be handled here.
//!
//! # Why this can't be a per-state bake
//!
//! A fluid's shape depends on its **neighbours**: the four top-corner heights are
//! averaged from the fluid heights of the surrounding cells (which is what makes
//! water slope toward a drop), the still-vs-flowing texture and its rotation come
//! from the flow vector (also a function of neighbour heights), and each side
//! face is emitted only if the adjacent cell doesn't occlude it. None of that is
//! knowable from a single block state id. Assets therefore cannot bake a fluid in
//! isolation.
//!
//! ## The mesher seam
//!
//! This module owns the *mechanism* and the vanilla-derived math; the **mesher**
//! must supply the neighbourhood. Concretely, for a fluid cell the mesher must
//! provide (all of which need chunk/world neighbour access):
//!
//! - the cell's own [`FluidState`] (`amount` 1..=8, `falling`), and whether the
//!   same fluid occupies the cell directly above (→ full-height, flat top);
//! - for the eight horizontal neighbours: their fluid render-height (or `None`),
//!   used to average the four [`corner_height`]s;
//! - for the four edge neighbours: a [`FlowNeighbor`] (own height, whether the
//!   block blocks motion, and the fluid height of the cell *below* it) so
//!   [`flow_horizontal`] can reproduce vanilla's `FlowingFluid.getFlow`;
//! - per-face occlusion flags, from **two independent** questions: a face
//!   touching a neighbouring full/opaque cell is culled
//!   (`isFaceOccludedByNeighbor`), **and** a face is culled when the block in the
//!   fluid's *own* cell already covers it (`isFaceOccludedBySelf` — see
//!   [`SelfOcclusion`]). Waterlogged geometry lives entirely in the second one;
//! - the biome water colour, resolved via the [`crate::tint`] seam (lava is
//!   untinted).
//!
//! Everything below is verified against 26.2's server sources
//! (`FlowingFluid`/`FluidState`): `getOwnHeight = amount / 9`, `getHeight =
//! sameAbove ? 1 : ownHeight`, and the full `getFlow` distance/step summation.
//! The corner-height **averaging weights**, the still/flowing texture selection
//! and the flowing-texture UV rotation are all verified against the client
//! `FluidRenderer`/`FluidModel` sources, so this module is jar-verified end to
//! end. Fluid tint comes from the fluid model's tint source (water =
//! `getAverageWaterColor`, lava untinted), resolved via the [`crate::tint`]
//! seam; per-face colour is left to the mesher/renderer.
//!
//! ## A family of bugs: self-cell questions answered from neighbours
//!
//! Two of the defects fixed here were the *same mistake* — a `FluidRenderer` test
//! about the fluid's **own** cell that we were answering from its neighbours:
//!
//! | vanilla test | what we did instead | symptom |
//! |---|---|---|
//! | `tesselate`'s `if (heightSelf >= 1.0F)` corner short-circuit | averaged the neighbours ([`corner_heights`]) | a falling column with a repeating horizontal gap |
//! | `isFaceOccludedBySelf` ([`SelfOcclusion`]) | only `isFaceOccludedByNeighbor` | waterlogged stairs z-fighting on their solid side |
//!
//! Both read as complete because the neighbour-facing sibling *exists* and is
//! correct, so nothing looks missing. **When a fluid defect is local to one cell,
//! check whether the vanilla predicate takes `pos`/`blockState` and no neighbour
//! before reaching for the neighbourhood** — expect a third instance.
//!
//! `bake_fluid` itself now owns vanilla's `~0.001` anti-z-fight insets, the
//! optional back faces (`FluidRenderer.addFace`'s reversed copy) and the
//! `water_overlay` material substitution against glass/ice/leaves neighbours —
//! the mesher only has to supply the neighbourhood facts
//! ([`FluidGeometry::back_up_face`], [`FluidGeometry::side_overlay`]) that
//! `FluidState.shouldRenderBackwardUpFace` and the `HalfTransparentBlock` /
//! `LeavesBlock` check need.

use crate::bake::BakedQuad;
use crate::model::Direction;

/// Height (in blocks) of a full source that has fluid above it: `8 / 9`.
pub const SOURCE_OWN_HEIGHT: f32 = 8.0 / 9.0;

/// The `0.8888889` constant from `FlowingFluid.getFlow`'s ledge case.
const LEDGE_CONST: f32 = 0.888_888_9;

/// A fluid cell's dynamic state, mirroring vanilla's `LEVEL`/`FALLING` fluid
/// properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidState {
    /// Fluid amount, `1..=8` (a full source is `8`).
    pub amount: u8,
    /// Whether this fluid is falling (fed from above).
    pub falling: bool,
}

impl FluidState {
    /// A full source block (`amount = 8`, not falling).
    #[must_use]
    pub fn source() -> Self {
        Self {
            amount: 8,
            falling: false,
        }
    }

    /// A flowing fluid with the given `amount` and falling flag.
    #[must_use]
    pub fn new(amount: u8, falling: bool) -> Self {
        Self { amount, falling }
    }

    /// The fluid's own surface height, `amount / 9` (verified
    /// `FlowingFluid.getOwnHeight`).
    #[must_use]
    pub fn own_height(&self) -> f32 {
        self.amount as f32 / 9.0
    }

    /// The rendered surface height: `1.0` when the same fluid sits directly
    /// above (a continuous column), else [`own_height`](Self::own_height)
    /// (verified `FlowingFluid.getHeight`).
    #[must_use]
    pub fn render_height(&self, same_fluid_above: bool) -> f32 {
        if same_fluid_above {
            1.0
        } else {
            self.own_height()
        }
    }
}

/// Vanilla `FluidRenderer.getHeight` for a neighbour cell: the value fed to
/// [`corner_height`].
///
/// - `1.0` if the same fluid occupies this cell *and* the cell above it (a
///   continuous column);
/// - the fluid's own height if the same fluid is here with nothing above;
/// - `0.0` if this cell is a *different* fluid/empty **and not solid** (air-like,
///   which vanilla still averages in, pulling the corner down);
/// - `-1.0` if this cell is a *different* fluid/empty **and solid** (excluded
///   from the average entirely).
///
/// The air-vs-solid distinction is why a source next to open air slopes down at
/// that corner but stays flush next to a solid block.
#[must_use]
pub fn neighbor_height(
    same_fluid: bool,
    same_fluid_above: bool,
    own_height: f32,
    solid: bool,
) -> f32 {
    if same_fluid {
        if same_fluid_above { 1.0 } else { own_height }
    } else if solid {
        -1.0
    } else {
        0.0
    }
}

/// Averages the four cells meeting at a top corner into a corner height,
/// reproducing vanilla `FluidRenderer.calculateAverageHeight`/`addWeightedHeight`
/// exactly.
///
/// `height_self` is the fluid being baked (its [`neighbor_height`]); `edge_a`
/// and `edge_b` are the two axis neighbours sharing this corner; `diagonal` is
/// the corner-diagonal cell. All four are [`neighbor_height`] values (so `-1.0`
/// means "solid, exclude" and `0.0` means "air, include with weight 1").
///
/// Vanilla weights near-full cells (`>= 0.8`) ten times as heavily as shallow
/// ones, snaps the corner to `1.0` if either edge (or, when sampled, the
/// diagonal) is a full column, and only samples the diagonal when at least one
/// edge carries fluid. Verified against the client `FluidRenderer` source.
#[must_use]
pub fn corner_height(height_self: f32, edge_a: f32, edge_b: f32, diagonal: f32) -> f32 {
    if edge_a >= 1.0 || edge_b >= 1.0 {
        return 1.0;
    }
    let mut sum = 0.0f32;
    let mut weight = 0.0f32;
    if edge_a > 0.0 || edge_b > 0.0 {
        if diagonal >= 1.0 {
            return 1.0;
        }
        add_weighted_height(&mut sum, &mut weight, diagonal);
    }
    add_weighted_height(&mut sum, &mut weight, height_self);
    add_weighted_height(&mut sum, &mut weight, edge_a);
    add_weighted_height(&mut sum, &mut weight, edge_b);
    sum / weight
}

fn add_weighted_height(sum: &mut f32, weight: &mut f32, height: f32) {
    if height >= 0.8 {
        *sum += height * 10.0;
        *weight += 10.0;
    } else if height >= 0.0 {
        *sum += height;
        *weight += 1.0;
    }
}

/// All four top-corner heights, in `[NW, NE, SE, SW]` order — the whole of
/// vanilla `FluidRenderer.tesselate`'s corner branch, **including the
/// short-circuit that [`corner_height`] alone does not carry**.
///
/// # Why this exists rather than four `corner_height` calls
///
/// `tesselate` does not average unconditionally. It first asks whether the
/// fluid's *own* rendered height is already full, and if so sets every corner to
/// `1.0` without consulting a single neighbour:
///
/// ```text
/// float heightSelf = this.getHeight(level, type, pos, blockState, fluidState);
/// if (heightSelf >= 1.0F) {
///    heightNorthEast = heightNorthWest = heightSouthEast = heightSouthWest = 1.0F;
/// } else {
///    ... calculateAverageHeight for each corner ...
/// }
/// ```
///
/// `heightSelf` reaches `1.0` exactly when the same fluid sits directly above
/// (`FlowingFluid.getHeight`'s `hasSameAbove` short-circuit) — never from its own
/// amount, because `WaterFluid.Source.getAmount` is **8**, so even a source's
/// `getOwnHeight` is `8/9`.
///
/// # What averaging instead of short-circuiting looked like
///
/// A vertically falling column of water in open air. Every cell has water above,
/// so `height_self` is `1.0` and vanilla draws all four corners at `1.0` — a
/// seamless column. Averaging instead pulls each corner down against the air
/// beside it: `corner_height(1.0, 0.0, 0.0, 0.0)` weights the full self cell ten
/// times and each air edge once, giving `10 / 12 = 0.8333`. Every block in the
/// column was rendered a sixth of a block short, so the column had a repeating
/// horizontal gap in it.
///
/// It presented as *triangular* wedges rather than clean bands because the
/// shortfall is not uniform once anything solid is adjacent: a solid neighbour
/// contributes `-1.0` and is dropped from the average entirely, so that corner
/// comes out `10 / 11 = 0.909` while the corner facing open air stays `0.8333`.
/// Two different corner heights on one quad is a sloped surface, and the
/// triangulation makes the slope read as a wedge.
///
/// Arguments are [`neighbor_height`] values: the four axis neighbours in
/// Minecraft's convention (north = `-Z`, south = `+Z`, east = `+X`, west = `-X`)
/// and the four corner diagonals.
#[must_use]
#[expect(
    clippy::too_many_arguments,
    reason = "one argument per cell vanilla's own corner branch consults; \
              grouping them would only move the unpacking to the caller"
)]
pub fn corner_heights(
    height_self: f32,
    north: f32,
    south: f32,
    east: f32,
    west: f32,
    diag_nw: f32,
    diag_ne: f32,
    diag_se: f32,
    diag_sw: f32,
) -> [f32; 4] {
    if height_self >= 1.0 {
        return [1.0; 4];
    }
    [
        corner_height(height_self, west, north, diag_nw),
        corner_height(height_self, east, north, diag_ne),
        corner_height(height_self, east, south, diag_se),
        corner_height(height_self, west, south, diag_sw),
    ]
}

/// One edge neighbour, as the mesher must describe it for flow computation.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FlowNeighbor {
    /// The neighbour fluid's own height (`0.0` if the cell holds no affecting
    /// fluid).
    pub own_height: f32,
    /// Whether the neighbour block blocks motion (an opaque, non-passable cell).
    pub blocks_motion: bool,
    /// The own height of the fluid in the cell *below* the neighbour (`0.0` if
    /// none). Used for vanilla's "flow off a ledge" case.
    pub below_own_height: f32,
}

/// Reproduces vanilla `FlowingFluid.getFlow`'s horizontal component.
///
/// Neighbours are given in Minecraft axis convention: north = `-Z`, south =
/// `+Z`, east = `+X`, west = `-X`. Returns the normalised `[x, z]` flow vector,
/// or `[0.0, 0.0]` when the surface is level (still). Verified line-for-line
/// against the server source.
#[must_use]
pub fn flow_horizontal(
    center_own_height: f32,
    north: FlowNeighbor,
    south: FlowNeighbor,
    east: FlowNeighbor,
    west: FlowNeighbor,
) -> [f64; 2] {
    let mut flow_x = 0.0f64;
    let mut flow_z = 0.0f64;
    for (neighbor, step_x, step_z) in [
        (north, 0.0, -1.0),
        (south, 0.0, 1.0),
        (east, 1.0, 0.0),
        (west, -1.0, 0.0),
    ] {
        let distance = neighbor_distance(center_own_height, neighbor);
        if distance != 0.0 {
            flow_x += step_x * distance as f64;
            flow_z += step_z * distance as f64;
        }
    }
    normalize2(flow_x, flow_z)
}

fn neighbor_distance(center_own_height: f32, neighbor: FlowNeighbor) -> f32 {
    let mut height = neighbor.own_height;
    if height == 0.0 {
        if neighbor.blocks_motion {
            return 0.0;
        }
        // The neighbour cell is empty and passable: look at the fluid below it,
        // so flow reaches toward a drop-off.
        height = neighbor.below_own_height;
        if height > 0.0 {
            return center_own_height - (height - LEDGE_CONST);
        }
        return 0.0;
    }
    center_own_height - height
}

fn normalize2(x: f64, z: f64) -> [f64; 2] {
    let mag = (x * x + z * z).sqrt();
    if mag == 0.0 {
        [0.0, 0.0]
    } else {
        [x / mag, z / mag]
    }
}

/// Which of the two fluid textures a surface uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FluidTexture {
    /// `*_still` — used when the surface is level (no flow).
    Still,
    /// `*_flow` — used when the surface flows; rotated by [`flow_angle`].
    Flowing,
}

/// Selects the still or flowing texture from a flow vector.
#[must_use]
pub fn select_texture(flow: [f64; 2]) -> FluidTexture {
    if flow[0] == 0.0 && flow[1] == 0.0 {
        FluidTexture::Still
    } else {
        FluidTexture::Flowing
    }
}

/// The angle (radians) the flowing texture is rotated by, from the flow vector.
///
/// Matches the client's `atan2(flow.z, flow.x) - PI/2`. Reconstructed from
/// documented vanilla behaviour (client renderer, not in the server decompile).
#[must_use]
pub fn flow_angle(flow: [f64; 2]) -> f32 {
    (flow[1].atan2(flow[0]) as f32) - std::f32::consts::FRAC_PI_2
}

/// Reduces a neighbour's outline shape to the one partial-occlusion case this
/// module can evaluate exactly: a **single** collision box spanning the full
/// `x`/`z` footprint of its cell — `dirt_path`, `farmland`, slabs, snow layers,
/// and every other "flat, height-only-reduced" shape. Returns its
/// `(min_y, max_y)` in block-local `0.0..=1.0` when it qualifies; `None`
/// otherwise (air, multiple boxes, or a box that doesn't span the full
/// footprint — stairs, fences, walls, panes).
///
/// This is the scoped subset of vanilla's `Shapes.blockOccludes`,
/// which the doc comment on
/// [`crate`][crate]'s `FluidSectionView::partial_occluder_y_range_at`
/// consumer explains the derivation for. `boxes` should come from the
/// neighbour's **outline** shape — `VersionAdapter::block_outline` /
/// `lodestone_data::outline_shapes::outline_boxes` — not its collision shape:
/// vanilla's `getOcclusionShape` is `state.getShape(...)`, the outline getter,
/// and the two disagree for roughly half of all 26.2 block states (see
/// `lodestone_data::outline_shapes`'s module docs).
#[must_use]
pub fn full_footprint_y_range(boxes: &[lodestone_model::BlockAabb]) -> Option<(f32, f32)> {
    const EPS: f32 = 1e-4;
    if boxes.len() != 1 {
        return None;
    }
    let only = &boxes[0];
    let full_x = only.min[0] <= EPS && only.max[0] >= 1.0 - EPS;
    let full_z = only.min[2] <= EPS && only.max[2] >= 1.0 - EPS;
    if full_x && full_z {
        Some((only.min[1], only.max[1]))
    } else {
        None
    }
}

/// Whether the union of `boxes` completely covers the unit square of the cell
/// face pointing `face`.
///
/// This is vanilla `Shapes.blockOccludes` specialised to the *one* call
/// `FluidRenderer.isFaceOccludedBySelf` makes — and that specialisation is what
/// makes an exact answer cheap. `isFaceOccludedBySelf(state, dir)` is
/// `isFaceOccludedByState(dir.getOpposite(), 1.0F, state)`, so the tested shape
/// is the **whole** unit cube (`Shapes.box(0,0,0, 1,1,1)`, height `1.0`, never
/// the fluid's real surface height) and the occluder is
/// `state.getFaceOcclusionShape(dir)`. With a full-cube probe, `blockOccludes`'s
/// slice comparison degenerates to "is the occluder's boundary layer the entire
/// face" — a pure 2-D coverage question with no fluid height in it at all.
///
/// Contrast [`full_footprint_y_range`], which serves the *neighbour*-facing
/// sibling `isFaceOccludedByNeighbor`: there the probe height is the fluid's own
/// corner height, so the answer really does depend on how deep the fluid is, and
/// the reduction has to carry a `y` range. The self call needs no scoping to a
/// single box, so this one is exact for any axis-aligned union — stairs and walls
/// included.
///
/// `boxes` must come from the **outline** shape
/// (`lodestone_data::outline_shapes::outline_boxes`), for the reason
/// [`full_footprint_y_range`] records: vanilla's `getOcclusionShape` is
/// `state.getShape(...)`.
///
/// # How the coverage test works
///
/// Only boxes touching the boundary plane contribute — vanilla slices the
/// occlusion shape at that layer (`VoxelShape.getFaceShape`) before comparing.
/// Their projections onto the two free axes are axis-aligned rectangles, so the
/// union covers `[0,1]²` iff it covers every strip between consecutive
/// `a`-breakpoints. Each strip is then a 1-D interval cover, answered greedily.
/// Both loops advance strictly, so both terminate, and neither allocates: this
/// runs inside the per-cell fluid loop.
#[must_use]
pub fn face_fully_covered(boxes: &[lodestone_model::BlockAabb], face: Direction) -> bool {
    const EPS: f32 = 1e-4;
    let (axis, at_max) = match face {
        Direction::Down => (1usize, false),
        Direction::Up => (1, true),
        Direction::North => (2, false),
        Direction::South => (2, true),
        Direction::West => (0, false),
        Direction::East => (0, true),
    };
    let (a, b) = match axis {
        0 => (1usize, 2usize),
        1 => (0usize, 2usize),
        _ => (0usize, 1usize),
    };
    let on_plane = |bx: &lodestone_model::BlockAabb| {
        if at_max {
            bx.max[axis] >= 1.0 - EPS
        } else {
            bx.min[axis] <= EPS
        }
    };
    if !boxes.iter().any(on_plane) {
        return false;
    }

    let mut a0 = 0.0f32;
    while a0 < 1.0 - EPS {
        // The next `a`-breakpoint strictly past `a0`, clamped to the cell edge.
        let mut a1 = 1.0f32;
        for bx in boxes.iter().filter(|bx| on_plane(bx)) {
            for c in [bx.min[a], bx.max[a]] {
                if c > a0 + EPS && c < a1 {
                    a1 = c;
                }
            }
        }
        let mid_a = 0.5 * (a0 + a1);
        // Greedy 1-D cover of `b` over the strip at `mid_a`.
        let mut b0 = 0.0f32;
        loop {
            if b0 >= 1.0 - EPS {
                break;
            }
            let mut reach = b0;
            for bx in boxes.iter().filter(|bx| on_plane(bx)) {
                let spans_a = bx.min[a] <= mid_a && bx.max[a] >= mid_a;
                if spans_a && bx.min[b] <= b0 + EPS && bx.max[b] > reach {
                    reach = bx.max[b];
                }
            }
            if reach <= b0 + EPS {
                return false;
            }
            b0 = reach;
        }
        a0 = a1;
    }
    true
}

/// Which of a fluid cell's faces the block **sharing that cell** already
/// occludes — vanilla `FluidRenderer.isFaceOccludedBySelf`, the half of
/// `shouldRenderFace` that is not about neighbours at all.
///
/// For a waterlogged stair the stair and the water occupy one cell, so the
/// water's face on the stair's solid side lands **coplanar** with the stair's own
/// face and the two z-fight. Vanilla does not inset the water; it declines to
/// emit the face.
///
/// # There is no `up`, and that is vanilla, not an omission
///
/// `FluidRenderer.tesselate` computes `renderUp` as bare
/// `!isNeighborSameFluid(self, above)` — it is the only one of the six faces that
/// does **not** go through `shouldRenderFace`, so the self test never reaches it.
/// A waterlogged top slab therefore still draws its water surface *inside* the
/// slab. Adding `up` here would be a divergence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SelfOcclusion {
    /// The bottom face.
    pub down: bool,
    /// The `-Z` side.
    pub north: bool,
    /// The `+Z` side.
    pub south: bool,
    /// The `+X` side.
    pub east: bool,
    /// The `-X` side.
    pub west: bool,
}

impl SelfOcclusion {
    /// Whether every face is un-occluded — the answer for an empty shape, and
    /// what a view that does not model self-occlusion returns.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// [`SelfOcclusion`] for a block whose outline shape is `boxes`, via
/// [`face_fully_covered`] on the five faces vanilla's `shouldRenderFace` covers.
///
/// The caller is responsible for the `canOcclude` half of vanilla's
/// `occlusionShape = canOcclude ? getOcclusionShape(state) : Shapes.empty()`:
/// pass an empty slice (or skip the call) for a block that does not occlude, or
/// every waterlogged leaves block — full-cube outline, `noOcclusion()` in vanilla
/// — would cull its own water away entirely.
#[must_use]
pub fn self_occlusion(boxes: &[lodestone_model::BlockAabb]) -> SelfOcclusion {
    if boxes.is_empty() {
        return SelfOcclusion::default();
    }
    SelfOcclusion {
        down: face_fully_covered(boxes, Direction::Down),
        north: face_fully_covered(boxes, Direction::North),
        south: face_fully_covered(boxes, Direction::South),
        east: face_fully_covered(boxes, Direction::East),
        west: face_fully_covered(boxes, Direction::West),
    }
}

/// A normalised sprite UV rectangle, resolved from the atlas by the mesher.
///
/// Passing UV rects (rather than an [`crate::atlas::Atlas`]) keeps this module
/// decoupled from atlas layout and trivially testable. The mesher supplies
/// `atlas.sprite(loc).frame_uv(0, w, h)`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpriteUv {
    /// Top-left UV.
    pub min: [f32; 2],
    /// Bottom-right UV.
    pub max: [f32; 2],
    /// The animation slot of the sprite this rect belongs to, or `0` when the
    /// sprite is static. Carried through to each fluid [`BakedQuad`] so flowing
    /// water and lava advance frames like any other animated sprite. See
    /// [`AtlasSprite::anim_slot`](crate::atlas::AtlasSprite::anim_slot).
    pub anim: u8,
}

impl SpriteUv {
    /// Interpolates a UV within the rect from unit coordinates `(u, v)`.
    #[must_use]
    pub fn at(&self, u: f32, v: f32) -> [f32; 2] {
        [
            self.min[0] + (self.max[0] - self.min[0]) * u,
            self.min[1] + (self.max[1] - self.min[1]) * v,
        ]
    }
}

/// Which faces of a fluid cell to emit; the mesher culls occluded faces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FaceSet {
    /// The top surface.
    pub up: bool,
    /// The bottom face.
    pub down: bool,
    /// The `-Z` side.
    pub north: bool,
    /// The `+Z` side.
    pub south: bool,
    /// The `+X` side.
    pub east: bool,
    /// The `-X` side.
    pub west: bool,
}

impl Default for FaceSet {
    fn default() -> Self {
        Self {
            up: true,
            down: true,
            north: true,
            south: true,
            east: true,
            west: true,
        }
    }
}

/// Whether each side face should sample the `water_overlay` material instead
/// of `*_flow`, matching vanilla's `relativeBlock instanceof HalfTransparentBlock
/// || relativeBlock instanceof LeavesBlock` check in `FluidRenderer.tesselate`.
/// An overlay side face also omits its back copy (`addBackFace = !isOverlay`).
/// Ignored (treated as all-`false`) when [`bake_fluid`] isn't given an overlay
/// sprite — lava has no overlay material in vanilla either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SideOverlay {
    /// The `-Z` side.
    pub north: bool,
    /// The `+Z` side.
    pub south: bool,
    /// The `+X` side.
    pub east: bool,
    /// The `-X` side.
    pub west: bool,
}

/// The resolved neighbourhood of a fluid cell — the mesher fills this, then
/// [`bake_fluid`] turns it into quads. This struct *is* the mesher seam.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FluidGeometry {
    /// Top-corner heights in `0..=1`, order **NW, NE, SE, SW** (`-X-Z`, `+X-Z`,
    /// `+X+Z`, `-X+Z`), each from [`corner_height`].
    pub corners: [f32; 4],
    /// Normalised horizontal flow (`[0,0]` when still).
    pub flow: [f64; 2],
    /// Which faces to emit. The mesher decides occlusion (vanilla
    /// `shouldRenderFace` + neighbour-height occlusion); a cleared flag means the
    /// face is culled.
    pub faces: FaceSet,
    /// Biome/colour tint index (`Some(0)` for water, `None` for lava). The
    /// mesher resolves the actual colour via the [`crate::tint`] seam
    /// ([`crate::tint::TintKind::Water`]) and multiplies it in, matching the
    /// fluid model's tint source.
    pub tint_index: Option<i32>,
    /// Whether the top surface's back-facing copy should also be emitted —
    /// vanilla `FluidState.shouldRenderBackwardUpFace`: true when any of the
    /// 3×3 neighbourhood at the cell directly above carries a *different*
    /// fluid (or none) over a non-solid-render block, i.e. the surface is
    /// visible from above through a gap at the rim. Ignored when
    /// [`FaceSet::up`] is cleared.
    pub back_up_face: bool,
    /// Per-side overlay-material selection; see [`SideOverlay`].
    pub side_overlay: SideOverlay,
}

/// Vanilla's z-fight avoidance nudge (`FluidRenderer.tesselate`'s `offs` /
/// `bottomOffs` / the `0.001F` side inset). Top corners are pulled down by this
/// much when the top face is drawn, side faces are inset this far from their
/// block boundary, and the bottom edge of a side face — and the bottom face
/// itself — sit this far above `y = 0`, but **only** when the bottom face is
/// also drawn (`bottomOffs = renderDown ? 0.001F : 0.0F`); a culled bottom face
/// leaves side faces flush with `y = 0`.
const Z_FIGHT_INSET: f32 = 0.001;

/// Bakes a fluid cell into renderer-ready quads, matching the vertex winding and
/// UV mapping of the client `FluidRenderer.tesselate`.
///
/// Emits the top surface (four corner heights), the requested side faces and the
/// bottom face. The top uses the still texture when [`FluidGeometry::flow`] is
/// zero, otherwise the flowing texture rotated by [`flow_angle`]; sides use the
/// left half of the flowing texture (or, per [`FluidGeometry::side_overlay`],
/// the left half of `overlay`) scaled vertically by their corner heights; the
/// bottom uses the still texture. Positions are in block-local space (`0..=1`,
/// inset by [`Z_FIGHT_INSET`] exactly where vanilla insets them). `cullface` is
/// `None` on every quad — fluids are culled by the mesher through [`FaceSet`],
/// not the block-model cull system.
///
/// `overlay` is `None` for fluids with no overlay material (lava, in vanilla);
/// [`FluidGeometry::side_overlay`] is then ignored and every side uses `flow`.
#[must_use]
pub fn bake_fluid(
    geom: &FluidGeometry,
    still: SpriteUv,
    flow: SpriteUv,
    overlay: Option<SpriteUv>,
) -> Vec<BakedQuad> {
    let mut quads = Vec::new();
    // Vanilla mutates `heightNorthWest` etc. in place once, before either the
    // top face or the side-face switch reads them — so the inset (applied only
    // when the top face actually draws) is visible to sides too. Mirrored here
    // by adjusting the corners the side loop below reads from.
    let [nw, ne, se, sw] = if geom.faces.up {
        geom.corners.map(|h| h - Z_FIGHT_INSET)
    } else {
        geom.corners
    };
    let flowing = select_texture(geom.flow) == FluidTexture::Flowing;
    // Side faces' bottom edge only lifts off y=0 when the bottom face is also
    // drawn (avoids z-fighting *between* them); otherwise it stays at y=0.
    let bottom_offs = if geom.faces.down { Z_FIGHT_INSET } else { 0.0 };

    if geom.faces.up {
        // Vanilla winding NW -> SW -> SE -> NE (counter-clockwise from above).
        let positions = [
            [0.0, nw, 0.0],
            [0.0, sw, 1.0],
            [1.0, se, 1.0],
            [1.0, ne, 0.0],
        ];
        let uvs = if flowing {
            top_flow_uvs(flow, geom.flow)
        } else {
            [
                still.at(0.0, 0.0),
                still.at(0.0, 1.0),
                still.at(1.0, 1.0),
                still.at(1.0, 0.0),
            ]
        };
        let quad = fluid_quad(
            positions,
            uvs,
            Direction::Up,
            geom.tint_index,
            if flowing { flow.anim } else { still.anim },
        );
        if geom.back_up_face {
            let back = back_face(&quad);
            quads.push(quad);
            quads.push(back);
        } else {
            quads.push(quad);
        }
    }

    if geom.faces.down {
        quads.push(fluid_quad(
            [
                [0.0, bottom_offs, 0.0],
                [1.0, bottom_offs, 0.0],
                [1.0, bottom_offs, 1.0],
                [0.0, bottom_offs, 1.0],
            ],
            [
                still.at(0.0, 0.0),
                still.at(1.0, 0.0),
                still.at(1.0, 1.0),
                still.at(0.0, 1.0),
            ],
            Direction::Down,
            geom.tint_index,
            still.anim,
        ));
    }

    // Side faces, per vanilla's per-direction corner selection. Each spans two
    // top corners (heights h0, h1) down to the (possibly lifted) base, using the
    // left half of the flow (or overlay) texture (u in [0, 0.5]) with v scaled
    // by height, inset `Z_FIGHT_INSET` off the block boundary.
    let eps = Z_FIGHT_INSET;
    let sides = [
        (
            geom.faces.north,
            geom.side_overlay.north,
            Direction::North,
            [0.0f32, 1.0],
            [eps, eps],
            nw,
            ne,
        ),
        (
            geom.faces.south,
            geom.side_overlay.south,
            Direction::South,
            [1.0, 0.0],
            [1.0 - eps, 1.0 - eps],
            se,
            sw,
        ),
        (
            geom.faces.west,
            geom.side_overlay.west,
            Direction::West,
            [eps, eps],
            [1.0, 0.0],
            sw,
            nw,
        ),
        (
            geom.faces.east,
            geom.side_overlay.east,
            Direction::East,
            [1.0 - eps, 1.0 - eps],
            [0.0, 1.0],
            ne,
            se,
        ),
    ];
    for (emit, use_overlay, dir, xs, zs, h0, h1) in sides {
        if !emit {
            continue;
        }
        let is_overlay = use_overlay && overlay.is_some();
        let sprite = if is_overlay {
            overlay.expect("checked by is_overlay")
        } else {
            flow
        };
        let quad = fluid_quad(
            [
                [xs[0], h0, zs[0]],
                [xs[1], h1, zs[1]],
                [xs[1], bottom_offs, zs[1]],
                [xs[0], bottom_offs, zs[0]],
            ],
            [
                sprite.at(0.0, (1.0 - h0) * 0.5),
                sprite.at(0.5, (1.0 - h1) * 0.5),
                sprite.at(0.5, 0.5),
                sprite.at(0.0, 0.5),
            ],
            dir,
            geom.tint_index,
            sprite.anim,
        );
        // `addBackFace = !isOverlay`: an overlay side face (glass/ice/leaves) is
        // single-sided; a plain flow side face gets a reversed back copy so it
        // reads correctly when seen from the far side (e.g. looking up through
        // the underside of a waterfall).
        if is_overlay {
            quads.push(quad);
        } else {
            let back = back_face(&quad);
            quads.push(quad);
            quads.push(back);
        }
    }

    quads
}

fn fluid_quad(
    positions: [[f32; 3]; 4],
    uvs: [[f32; 2]; 4],
    direction: Direction,
    tint_index: Option<i32>,
    anim: u8,
) -> BakedQuad {
    BakedQuad {
        positions,
        uvs,
        direction,
        cullface: None,
        tint_index,
        shade: false,
        layer: 0,
        anim,
        sprite: 0,
    }
}

/// The reversed-winding copy `FluidRenderer.addFace` emits for a double-sided
/// quad: vertex order `[0, 3, 2, 1]` instead of `[0, 1, 2, 3]`, so the face is
/// visible from the opposite side too. `direction` is carried through unused —
/// fluid quads are always `shade: false`, so [`crate::bake`]'s per-direction
/// shade constant never reads it — and `cullface` stays `None` like the front
/// copy.
fn back_face(front: &BakedQuad) -> BakedQuad {
    BakedQuad {
        positions: [
            front.positions[0],
            front.positions[3],
            front.positions[2],
            front.positions[1],
        ],
        uvs: [
            front.uvs[0],
            front.uvs[3],
            front.uvs[2],
            front.uvs[1],
        ],
        direction: front.direction,
        cullface: None,
        tint_index: front.tint_index,
        shade: false,
        layer: 0,
        anim: front.anim,
        sprite: 0,
    }
}

/// The flowing top-face UVs: the sprite sampled at its centre and rotated by the
/// flow angle. Verified against the client `FluidRenderer` (lines computing
/// `u00..v11` from `s`/`c`).
fn top_flow_uvs(flow_uv: SpriteUv, flow: [f64; 2]) -> [[f32; 2]; 4] {
    let angle = flow_angle(flow);
    let sin = angle.sin() * 0.25;
    let cos = angle.cos() * 0.25;
    [
        flow_uv.at(0.5 + (-cos - sin), 0.5 + (-cos + sin)),
        flow_uv.at(0.5 + (-cos + sin), 0.5 + (cos + sin)),
        flow_uv.at(0.5 + (cos + sin), 0.5 + (cos - sin)),
        flow_uv.at(0.5 + (cos - sin), 0.5 + (-cos - sin)),
    ]
}
