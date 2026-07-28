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
//! - per-face occlusion flags (a face touching a neighbouring full/opaque cell is
//!   culled);
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
//! seam; per-face colour and the `~0.001` z-fight insets vanilla applies are
//! left to the mesher/renderer.

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
}

/// Bakes a fluid cell into renderer-ready quads, matching the vertex winding and
/// UV mapping of the client `FluidRenderer.tesselate`.
///
/// Emits the top surface (four corner heights), the requested side faces and the
/// bottom face. The top uses the still texture when [`FluidGeometry::flow`] is
/// zero, otherwise the flowing texture rotated by [`flow_angle`]; sides use the
/// left half of the flowing texture, scaled vertically by their corner heights;
/// the bottom uses the still texture. Positions are in block-local space
/// (`0..=1`). `cullface` is `None` on every quad — fluids are culled by the
/// mesher through [`FaceSet`], not the block-model cull system — and vanilla's
/// `~0.001` anti-z-fight insets and optional back-faces are left to the mesher.
#[must_use]
pub fn bake_fluid(geom: &FluidGeometry, still: SpriteUv, flow: SpriteUv) -> Vec<BakedQuad> {
    let mut quads = Vec::new();
    let [nw, ne, se, sw] = geom.corners;
    let flowing = select_texture(geom.flow) == FluidTexture::Flowing;

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
        quads.push(fluid_quad(
            positions,
            uvs,
            Direction::Up,
            geom.tint_index,
            if flowing { flow.anim } else { still.anim },
        ));
    }

    if geom.faces.down {
        quads.push(fluid_quad(
            [
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
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
    // top corners (heights h0, h1) down to y=0, using the left half of the flow
    // texture (u in [0, 0.5]) with v scaled by height.
    let sides = [
        (
            geom.faces.north,
            Direction::North,
            [0.0f32, 1.0],
            [0.0f32, 0.0],
            nw,
            ne,
        ),
        (
            geom.faces.south,
            Direction::South,
            [1.0, 0.0],
            [1.0, 1.0],
            se,
            sw,
        ),
        (
            geom.faces.west,
            Direction::West,
            [0.0, 0.0],
            [1.0, 0.0],
            sw,
            nw,
        ),
        (
            geom.faces.east,
            Direction::East,
            [1.0, 1.0],
            [0.0, 1.0],
            ne,
            se,
        ),
    ];
    for (emit, dir, xs, zs, h0, h1) in sides {
        if !emit {
            continue;
        }
        quads.push(fluid_quad(
            [
                [xs[0], h0, zs[0]],
                [xs[1], h1, zs[1]],
                [xs[1], 0.0, zs[1]],
                [xs[0], 0.0, zs[0]],
            ],
            [
                flow.at(0.0, (1.0 - h0) * 0.5),
                flow.at(0.5, (1.0 - h1) * 0.5),
                flow.at(0.5, 0.5),
                flow.at(0.0, 0.5),
            ],
            dir,
            geom.tint_index,
            flow.anim,
        ));
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
