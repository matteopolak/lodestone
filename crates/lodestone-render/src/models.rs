//! Arbitrary baked-model geometry (stairs, fences, cross plants, slabs) and how
//! it flows through meshing alongside the fast full-cube path.
//!
//! # The shape mismatch, and the answer
//!
//! The [`mesh`](crate::mesh) module's greedy/simple meshers assume every block
//! is a **full cube with one sprite per face**: that assumption is what lets
//! greedy merge coplanar faces and what lets face culling be a single
//! neighbour test. Real blocks are described by
//! [`lodestone_assets::BakedQuad`]s carrying *arbitrary* geometry and their own
//! per-quad `cullface`.
//!
//! Greedy meshing **cannot merge non-cube geometry** — two stair quads aren't
//! coplanar unit faces, and merging them would be nonsense. Vanilla's answer is
//! essentially "don't merge anything that isn't a full cube face"; in fact
//! vanilla doesn't greedy-merge at all, it just emits each model quad and relies
//! on `cullface` occlusion plus its render layers. **Our policy:**
//!
//! * A block whose baked model *is* a full opaque cube — recognised by the
//!   geometry-derived predicate [`is_packed_cube`] — goes through the packed
//!   12-byte [`mesh_greedy`](crate::mesh::mesh_greedy) path and *is* merged.
//! * Every other block is meshed here, **per quad, never merged**: each
//!   [`BakedQuad`] is emitted verbatim (translated to its block position) unless
//!   its `cullface` neighbour fully occludes it.
//!
//! # Why a separate vertex type (the D1 measurement)
//!
//! The packed [`PackedVertex`](crate::vertex::PackedVertex) stores position in
//! 6 bits per axis — a block-resolution grid, perfect for cube corners but far
//! too coarse for baked models, whose vertices land on a 1/16-block grid and may
//! even poke slightly outside the cube. Model geometry therefore uses
//! [`ModelVertex`], a wider float-position vertex.
//!
//! We measured the real baked set (all 32,366 v770 block states, via
//! `tests/model_census.rs`): only **8.5 % of *states*** are packed cubes
//! (2,622), and the dominant overworld *surfaces* — grass (tinted top) and water
//! (a fluid, non-cube) — are **not** among them. So the split is *not* justified
//! by "the fast path owns most surfaces". It is justified by two other facts:
//!
//! 1. **Volume, not state count.** The blocks that fill a world by *count* —
//!    stone, deepslate, dirt, sand — are packed cubes; 410 of 1,196 blocks
//!    (34 %) are full-cube in *every* state. Weighted by rendered blocks rather
//!    than distinct states, the packed path carries the overwhelming majority.
//! 2. **Different UV/animation strategy.** The packed path stores a sprite id +
//!    tile coordinates and resolves animation in-shader; the model path bakes
//!    absolute atlas UVs. That difference — not merely vertex width — is what
//!    makes one format serve both awkwardly.
//!
//! Keeping the split earns its complexity **only because the predicate is
//! derived from the baked model** (see [`is_full_cube`]): no hardcoded,
//! version-specific block list is smuggled into this version-free crate, so the
//! fast-path gate cannot silently rot when Mojang changes a model.
//!
//! # Re-decided with the full consumer set (blocks *and* entities)
//!
//! `impl-assets`/version work adds a **third** vertex consumer: entity meshes.
//! Entities are hand-ported `CubeDef`/`PartDef` hierarchies baked to float-position
//! quads (`EntityQuad`) sampling a **per-entity texture sheet**, not the block
//! atlas, with a simpler per-model lighting term and no biome tint. That reopens
//! the D1 question: if the renderer must carry a wide float vertex + shader for
//! entities *anyway*, does a separate packed block format still earn its keep?
//!
//! The consumer set is:
//!
//! 1. **Bulk terrain full cubes** — greedy-merged, block atlas. Dominant by
//!    *volume* (stone/deepslate/dirt/sand fill the vertical world). Packable.
//! 2. **Baked non-cube block models** — per-quad, block atlas, biome tint, world
//!    smooth lighting. Wide float ([`ModelVertex`]).
//! 3. **Entity meshes** — per-quad, *per-entity* texture sheet, entity lighting,
//!    no tint, never greedy-merged. Wide float; can **share [`ModelVertex`]'s
//!    layout** with (2), differing only in bind group and shader.
//!
//! The decisive observation: there is **no single code path to win** by dropping
//! the packed format. (2) and (3) already need *different pipelines* from each
//! other — different texture source and lighting — regardless of vertex width, so
//! the shader/pipeline count is set by texture-source and lighting differences,
//! not by how many vertex *formats* exist. Collapsing blocks to one wide format
//! would still leave ≥2 pipelines, while paying **1.9× per quad** (72 → 136 bytes
//! incl. indices; 2.33× per vertex, 12 → 28) on the single largest consumer, (1).
//! See [`model_vram_bytes`] vs [`crate::vertex::vram_bytes`].
//!
//! So entities *strengthen* the split rather than weakening it: the wide path's
//! fixed cost is amortized across (2) **and** (3), which means the packed format
//! is a pure-win specialization layered on a wide path that exists no matter what.
//! **Decision: keep the split** (packed cubes + shared-wide models/entities).
//!
//! Residual risk, stated honestly and measurable at the Phase-5 registry gate:
//! the packed path only ever carries (1). On a *surface-only* view many exposed
//! faces are grass tops (tinted → not packed) or water (fluid → not packed), so
//! the packed share of *submitted* quads there is smaller than the volume
//! argument implies. It recovers underground/among cliffs where solid stone
//! dominates — exactly where the most sections load and the VRAM ceiling bites —
//! but the real fraction of *rendered* packed quads is worth measuring once the
//! live registry lets us mesh real columns instead of synthetic sections.
//!
//! # Animation lives on the immutable atlas, not the vertex
//!
//! Note both block paths resolve animation the same way and it touches neither
//! vertex format: `impl-assets` keeps every physical animation frame resident as
//! its own atlas region, so the atlas is immutable after build and animation is a
//! per-material uniform (current/next region + blend) resolved in-shader. See
//! [`crate::anim`]. That is why the packed path's fixed sprite id survives
//! animation — the *uniform*, not the mesh or the texture, changes per tick.

use lodestone_assets::fluid::{
    FaceSet, FluidGeometry, FlowNeighbor, bake_fluid, corner_height, flow_horizontal,
    neighbor_height,
};
use lodestone_assets::{BakedQuad, Direction};

use crate::block_models::{FluidCell, FluidKind, FluidSprites};
use crate::section::{Face, SECTION_SIZE};

/// A vertex for arbitrary baked-model geometry. Wider than the packed cube
/// vertex because model positions need sub-block precision and atlas-relative
/// UVs rather than per-face tile ids.
#[derive(Debug, Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
#[repr(C)]
pub struct ModelVertex {
    /// World/section-local position in blocks.
    pub position: [f32; 3],
    /// Normalised atlas UV.
    pub uv: [f32; 2],
    /// Per-vertex ambient occlusion in `0.0..=1.0` (1.0 = unoccluded).
    pub ao: f32,
    /// Packed sky (high nibble) and block (low nibble) light, `0..=15` each.
    pub light: u8,
    /// Biome tint index, or `255` for untinted.
    pub tint: u8,
    /// Padding to keep the struct `4`-byte aligned and `Pod`-friendly.
    pub _pad: [u8; 2],
}

impl ModelVertex {
    /// The `wgpu` vertex-buffer layout for the wide model vertex.
    ///
    /// Four attributes over the 28-byte stride: position (`Float32x3`), UV
    /// (`Float32x2`), AO (`Float32`), and the packed `light`/`tint`/pad tail as a
    /// single `Uint8x4` the shader unpacks. Locations `0..=3`.
    #[must_use]
    pub const fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 4] = [
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x3,
                offset: 0,
                shader_location: 0,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32x2,
                offset: 12,
                shader_location: 1,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Float32,
                offset: 20,
                shader_location: 2,
            },
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint8x4,
                offset: 24,
                shader_location: 3,
            },
        ];
        wgpu::VertexBufferLayout {
            array_stride: MODEL_BYTES_PER_VERTEX as u64,
            step_mode: wgpu::VertexStepMode::Vertex,
            attributes: &ATTRS,
        }
    }
}

/// A CPU mesh of arbitrary model geometry.
#[derive(Debug, Default, Clone)]
pub struct ModelMesh {
    /// Emitted vertices, four per quad.
    pub vertices: Vec<ModelVertex>,
    /// Triangle indices, six per quad.
    pub indices: Vec<u32>,
}

impl ModelMesh {
    /// Number of quads emitted.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }
}

/// Bytes per wide model vertex (asserted to be 28). This is the format the
/// non-cube block path uses, and the format the entity path can share (see the
/// D1 note above): the two differ by bind group and lighting, not by layout.
pub const MODEL_BYTES_PER_VERTEX: usize = core::mem::size_of::<ModelVertex>();

/// VRAM (bytes) for `quad_count` quads meshed with the **wide** [`ModelVertex`]
/// (4 verts + 6 `u32` indices per quad). Compare with
/// [`crate::vertex::vram_bytes`] (packed) to price the two-format split against
/// collapsing every block to the wide format.
#[must_use]
pub const fn model_vram_bytes(quad_count: usize) -> usize {
    let vertices = quad_count * 4 * MODEL_BYTES_PER_VERTEX;
    let indices = quad_count * 6 * core::mem::size_of::<u32>();
    vertices + indices
}

/// Maps [`lodestone_assets::Direction`] to this crate's [`Face`].
#[must_use]
pub fn face_of_direction(d: Direction) -> Face {
    match d {
        Direction::West => Face::NegX,
        Direction::East => Face::PosX,
        Direction::Down => Face::NegY,
        Direction::Up => Face::PosY,
        Direction::North => Face::NegZ,
        Direction::South => Face::PosZ,
    }
}

/// The fixed axis (`0=x`, `1=y`, `2=z`) and plane value (`0.0` or `1.0`) that a
/// full unit face on `d` must lie in.
fn face_plane(d: Direction) -> (usize, f32) {
    match d {
        Direction::West => (0, 0.0),
        Direction::East => (0, 1.0),
        Direction::Down => (1, 0.0),
        Direction::Up => (1, 1.0),
        Direction::North => (2, 0.0),
        Direction::South => (2, 1.0),
    }
}

/// Whether a single quad is a full unit face on its own `direction`: coplanar
/// with the cube face, spanning the whole `1×1` square, and culled by the
/// neighbour in that direction.
fn quad_is_full_face(q: &BakedQuad) -> bool {
    const EPS: f32 = 1e-4;
    // Must be culled by exactly its own facing neighbour — a face that is never
    // culled (`cullface: None`, e.g. a cross-plant blade) is not a cube face.
    if q.cullface != Some(q.direction) {
        return false;
    }
    let (fixed, plane) = face_plane(q.direction);
    let (a, b) = match fixed {
        0 => (1usize, 2usize),
        1 => (0, 2),
        _ => (0, 1),
    };
    let mut corners = 0u8; // bitmask of the four {0,1}×{0,1} combinations
    for p in &q.positions {
        if (p[fixed] - plane).abs() > EPS {
            return false;
        }
        let ca = snap01(p[a]);
        let cb = snap01(p[b]);
        match (ca, cb) {
            (Some(ca), Some(cb)) => corners |= 1 << (ca * 2 + cb),
            _ => return false,
        }
    }
    corners == 0b1111
}

/// Snaps a coordinate to `0` or `1` (returned as `0`/`1`), or `None` if it is
/// not within epsilon of a unit-cube corner.
fn snap01(v: f32) -> Option<u8> {
    const EPS: f32 = 1e-4;
    if v.abs() <= EPS {
        Some(0)
    } else if (v - 1.0).abs() <= EPS {
        Some(1)
    } else {
        None
    }
}

/// Whether a baked model is exactly a full opaque cube: six axis-aligned unit
/// faces, one per direction, each spanning its whole face and carrying a
/// `cullface` equal to its facing direction.
///
/// This is the routing predicate that decides whether a block can take the
/// cheap packed-vertex path. It is **derived from the baked geometry, never a
/// hardcoded block list** — a per-version block list would be a version-specific
/// fact smuggled into a version-free crate and would rot silently the first time
/// Mojang changed a model. Any block whose baked quads happen to form a full
/// cube qualifies; anything else (stairs, slabs, cross-plants, fluids, models
/// with extra inner faces or non-cube elements) does not.
///
/// Note this tests *geometry* only. Tinted cubes (grass overlay, foliage) are
/// still full cubes here; whether the packed vertex can carry their tint is a
/// separate question — see [`is_packed_cube`].
#[must_use]
pub fn is_full_cube(quads: &[BakedQuad]) -> bool {
    if quads.len() != 6 {
        return false;
    }
    let mut seen = 0u8;
    for q in quads {
        if !quad_is_full_face(q) {
            return false;
        }
        seen |= 1 << face_of_direction(q.direction).index();
    }
    seen == 0b0011_1111
}

/// Whether a baked model can take the packed 8-byte cube path: a full cube
/// ([`is_full_cube`]) whose faces are all **untinted**. The packed vertex has no
/// per-vertex colour, so a tinted cube (grass, leaves, water still-cube) must
/// use the wider [`ModelVertex`] path even though its geometry is a cube.
#[must_use]
pub fn is_packed_cube(quads: &[BakedQuad]) -> bool {
    is_full_cube(quads) && quads.iter().all(|q| q.tint_index.is_none())
}

/// The mesher's view of a section for the model path.
///
/// `quads_at` yields a block's baked geometry (empty for air / full cubes, which
/// take the packed path instead). `occludes_at` reports whether the block at a
/// **signed** coordinate fully occludes an adjacent face — signed so the mesher
/// can test one block past a section boundary into a neighbour, which is exactly
/// what `cullface` needs.
pub trait ModelSectionView {
    /// The baked quads of the block at section-local `(x, y, z)`, each `0..16`.
    fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad];
    /// Whether the block at (possibly out-of-section) `(x, y, z)` fully occludes
    /// the face pointing back towards its neighbour.
    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool;
    /// Sky/block light at section-local `(x, y, z)`, packed sky<<4 | block.
    fn light_at(&self, x: usize, y: usize, z: usize) -> u8 {
        let _ = (x, y, z);
        0xF0
    }
}

/// Mesh the non-cube geometry of a section, emitting each visible baked quad
/// once, never merged. A quad is culled only when it carries a `cullface` and
/// the neighbouring block in that direction fully occludes it.
#[must_use]
pub fn mesh_models(view: &dyn ModelSectionView) -> ModelMesh {
    let mut mesh = ModelMesh::default();
    let n = SECTION_SIZE;
    for y in 0..n {
        for z in 0..n {
            for x in 0..n {
                let quads = view.quads_at(x, y, z);
                if quads.is_empty() {
                    continue;
                }
                let light = view.light_at(x, y, z);
                for quad in quads {
                    if let Some(cf) = quad.cullface {
                        let nrm = face_of_direction(cf).normal();
                        if view.occludes_at(x as i32 + nrm[0], y as i32 + nrm[1], z as i32 + nrm[2])
                        {
                            continue;
                        }
                    }
                    emit_baked_quad(&mut mesh, quad, [x as f32, y as f32, z as f32], light);
                }
            }
        }
    }
    mesh
}

fn emit_baked_quad(mesh: &mut ModelMesh, quad: &BakedQuad, origin: [f32; 3], light: u8) {
    let base = mesh.vertices.len() as u32;
    // Directional shading, matching vanilla's constant per-face factors so a
    // shaded quad reads correctly even before smooth lighting is applied.
    let shade = if quad.shade {
        match quad.direction {
            Direction::Up => 1.0,
            Direction::Down => 0.5,
            Direction::North | Direction::South => 0.8,
            Direction::East | Direction::West => 0.6,
        }
    } else {
        1.0
    };
    let tint = quad.tint_index.map_or(255u8, |t| t as u8);
    for i in 0..4 {
        let p = quad.positions[i];
        mesh.vertices.push(ModelVertex {
            position: [origin[0] + p[0], origin[1] + p[1], origin[2] + p[2]],
            uv: quad.uvs[i],
            ao: shade,
            light,
            tint,
            _pad: [0, 0],
        });
    }
    // Two triangles, matching the packed path's winding.
    mesh.indices
        .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
}

/// The mesher's view of a section for the **fluid** path.
///
/// Fluids are not baked per state (their shape depends on neighbours — see
/// [`lodestone_assets::fluid`]), so this view answers the neighbourhood queries
/// [`mesh_fluids`] needs: which fluid occupies a (possibly out-of-section) cell,
/// whether a cell fully occludes an adjacent fluid face, the per-cell light, and
/// the still/flow sprite rects for each fluid kind. Coordinates are **signed** so
/// the mesher can reach one cell past a section boundary into a neighbour.
pub trait FluidSectionView {
    /// The fluid occupying `(x, y, z)`, or `None` for a non-fluid cell.
    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell>;
    /// Whether the block at `(x, y, z)` fully occludes an adjacent fluid face (a
    /// solid, opaque full cube culls the fluid face touching it).
    fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool;
    /// Packed sky/block light at in-section `(x, y, z)`.
    fn light_at(&self, x: usize, y: usize, z: usize) -> u8 {
        let _ = (x, y, z);
        0xF0
    }
    /// The still/flow sprite rects for a fluid kind, into the model atlas.
    fn fluid_sprites(&self, kind: FluidKind) -> FluidSprites;
}

/// Water and lava geometry meshed from a section, on their two separate passes:
/// water is translucent (blended, sorted), lava opaque and full-bright.
#[derive(Debug, Default, Clone)]
pub struct FluidMeshes {
    /// Translucent water surface geometry.
    pub water: ModelMesh,
    /// Opaque, full-bright lava geometry.
    pub lava: ModelMesh,
}

/// The [`neighbor_height`] of the cell at `(x, y, z)` relative to a fluid of
/// `kind` being baked: its own height if it is the same fluid (snapped to `1.0`
/// when that same fluid continues above), `-1.0` if it is a solid block
/// (excluded from the average) or `0.0` if it is air-like.
fn neighbor_height_at(view: &dyn FluidSectionView, kind: FluidKind, x: i32, y: i32, z: i32) -> f32 {
    match view.fluid_at(x, y, z) {
        Some(f) if f.kind == kind => {
            let above_same = matches!(view.fluid_at(x, y + 1, z), Some(a) if a.kind == kind);
            neighbor_height(true, above_same, f.state.own_height(), false)
        }
        _ => neighbor_height(false, false, 0.0, view.occludes_at(x, y, z)),
    }
}

/// The [`FlowNeighbor`] describing the cell one step `(dx, dz)` from `(x, y, z)`.
fn flow_neighbor_at(
    view: &dyn FluidSectionView,
    kind: FluidKind,
    x: i32,
    y: i32,
    z: i32,
    dx: i32,
    dz: i32,
) -> FlowNeighbor {
    let (nx, nz) = (x + dx, z + dz);
    let own_height = match view.fluid_at(nx, y, nz) {
        Some(f) if f.kind == kind => f.state.own_height(),
        _ => 0.0,
    };
    let below_own_height = match view.fluid_at(nx, y - 1, nz) {
        Some(f) if f.kind == kind => f.state.own_height(),
        _ => 0.0,
    };
    FlowNeighbor {
        own_height,
        blocks_motion: view.occludes_at(nx, y, nz),
        below_own_height,
    }
}

/// Mesh the fluid cells of a section into water/lava geometry.
///
/// For each fluid cell the mesher reconstructs the vanilla `FluidRenderer`
/// neighbourhood — four averaged corner heights, the flow vector, and the face
/// set (a face is culled when the neighbour is the same fluid or a solid cube) —
/// and bakes it via [`bake_fluid`]. Water carries a tint index (the fluid pass
/// applies the water colour); lava is untinted and emitted **full-bright**
/// (light `0xFF`) since it is an emitter. Positions are section-local, matching
/// [`mesh_models`].
#[must_use]
pub fn mesh_fluids(view: &dyn FluidSectionView) -> FluidMeshes {
    let mut out = FluidMeshes::default();
    let n = SECTION_SIZE;
    for y in 0..n {
        for z in 0..n {
            for x in 0..n {
                let (xi, yi, zi) = (x as i32, y as i32, z as i32);
                let Some(fc) = view.fluid_at(xi, yi, zi) else {
                    continue;
                };
                let kind = fc.kind;
                let self_h = neighbor_height_at(view, kind, xi, yi, zi);
                let nh = |dx: i32, dz: i32| neighbor_height_at(view, kind, xi + dx, yi, zi + dz);
                let corners = [
                    corner_height(self_h, nh(-1, 0), nh(0, -1), nh(-1, -1)), // NW
                    corner_height(self_h, nh(1, 0), nh(0, -1), nh(1, -1)),   // NE
                    corner_height(self_h, nh(1, 0), nh(0, 1), nh(1, 1)),     // SE
                    corner_height(self_h, nh(-1, 0), nh(0, 1), nh(-1, 1)),   // SW
                ];
                let flow = flow_horizontal(
                    fc.state.own_height(),
                    flow_neighbor_at(view, kind, xi, yi, zi, 0, -1),
                    flow_neighbor_at(view, kind, xi, yi, zi, 0, 1),
                    flow_neighbor_at(view, kind, xi, yi, zi, 1, 0),
                    flow_neighbor_at(view, kind, xi, yi, zi, -1, 0),
                );

                let same = |dx: i32, dy: i32, dz: i32| {
                    matches!(view.fluid_at(xi + dx, yi + dy, zi + dz), Some(f) if f.kind == kind)
                };
                let emit = |dx: i32, dy: i32, dz: i32| {
                    !same(dx, dy, dz) && !view.occludes_at(xi + dx, yi + dy, zi + dz)
                };
                let faces = FaceSet {
                    up: emit(0, 1, 0),
                    down: emit(0, -1, 0),
                    north: emit(0, 0, -1),
                    south: emit(0, 0, 1),
                    east: emit(1, 0, 0),
                    west: emit(-1, 0, 0),
                };

                let tint_index = match kind {
                    FluidKind::Water => Some(0),
                    FluidKind::Lava => None,
                };
                let sprites = view.fluid_sprites(kind);
                let geom = FluidGeometry {
                    corners,
                    flow,
                    faces,
                    tint_index,
                };
                let quads = bake_fluid(&geom, sprites.still, sprites.flow);

                let (mesh, light) = match kind {
                    FluidKind::Water => (&mut out.water, view.light_at(x, y, z)),
                    FluidKind::Lava => (&mut out.lava, 0xFF),
                };
                for quad in &quads {
                    emit_baked_quad(mesh, quad, [x as f32, y as f32, z as f32], light);
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_vertex_prices_the_split() {
        // The D1 re-decision rests on these numbers being real, not vibes.
        assert_eq!(MODEL_BYTES_PER_VERTEX, 28);
        assert_eq!(crate::vertex::BYTES_PER_VERTEX, 12);
        // Per quad, including u32 indices: packed 72 B, wide 136 B ≈ 1.9×.
        assert_eq!(crate::vertex::vram_bytes(1), 72);
        assert_eq!(model_vram_bytes(1), 136);
        // Collapsing the dominant cube geometry to the wide format would nearly
        // double its VRAM/bandwidth — the cost the split buys back.
        assert!(model_vram_bytes(1_000) > crate::vertex::vram_bytes(1_000) * 18 / 10);
    }

    fn cube_face(dir: Direction, cull: Option<Direction>) -> BakedQuad {
        // A unit quad on the face `dir`; exact corner positions are irrelevant
        // to the culling logic under test.
        BakedQuad {
            positions: [[0.0; 3]; 4],
            uvs: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            direction: dir,
            cullface: cull,
            tint_index: None,
            shade: true,
            layer: 0,
        }
    }

    /// A single block at the section centre with a supplied quad list, and a
    /// configurable ring of occluding neighbours.
    struct OneBlock {
        quads: Vec<BakedQuad>,
        occlude_dir: Option<Direction>,
    }
    impl ModelSectionView for OneBlock {
        fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
            if (x, y, z) == (8, 8, 8) {
                &self.quads
            } else {
                &[]
            }
        }
        fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
            match self.occlude_dir {
                Some(d) => {
                    let nrm = face_of_direction(d).normal();
                    (x, y, z) == (8 + nrm[0], 8 + nrm[1], 8 + nrm[2])
                }
                None => false,
            }
        }
    }

    #[test]
    fn cross_plant_quads_are_never_culled() {
        // A cross plant is two quads with no cullface: always emitted, even
        // surrounded by solid blocks.
        let view = OneBlock {
            quads: vec![
                cube_face(Direction::North, None),
                cube_face(Direction::East, None),
            ],
            occlude_dir: Some(Direction::Up), // irrelevant: quads have no cullface
        };
        assert_eq!(mesh_models(&view).quad_count(), 2);
    }

    #[test]
    fn cube_face_is_culled_by_occluding_neighbour() {
        let view = OneBlock {
            quads: vec![cube_face(Direction::Up, Some(Direction::Up))],
            occlude_dir: Some(Direction::Up),
        };
        assert_eq!(mesh_models(&view).quad_count(), 0);
    }

    #[test]
    fn cube_face_is_kept_when_neighbour_is_open() {
        let view = OneBlock {
            quads: vec![cube_face(Direction::Up, Some(Direction::Up))],
            occlude_dir: None,
        };
        assert_eq!(mesh_models(&view).quad_count(), 1);
    }

    #[test]
    fn each_quad_emits_four_vertices_and_two_triangles() {
        let view = OneBlock {
            quads: vec![cube_face(Direction::Up, None)],
            occlude_dir: None,
        };
        let m = mesh_models(&view);
        assert_eq!(m.vertices.len(), 4);
        assert_eq!(m.indices.len(), 6);
    }

    #[test]
    fn shade_and_tint_flow_into_vertices() {
        let mut q = cube_face(Direction::Down, None);
        q.tint_index = Some(3);
        let view = OneBlock {
            quads: vec![q],
            occlude_dir: None,
        };
        let m = mesh_models(&view);
        // Down face shade factor is 0.5.
        assert!((m.vertices[0].ao - 0.5).abs() < 1e-6);
        assert_eq!(m.vertices[0].tint, 3);
    }

    /// A full unit face on `dir`, correctly placed in its face plane, culled by
    /// its own neighbour.
    fn full_face(dir: Direction) -> BakedQuad {
        let (fixed, plane) = face_plane(dir);
        let (a, b) = match fixed {
            0 => (1usize, 2usize),
            1 => (0, 2),
            _ => (0, 1),
        };
        let corner = |ca: f32, cb: f32| {
            let mut p = [0.0f32; 3];
            p[fixed] = plane;
            p[a] = ca;
            p[b] = cb;
            p
        };
        BakedQuad {
            positions: [
                corner(0.0, 0.0),
                corner(1.0, 0.0),
                corner(1.0, 1.0),
                corner(0.0, 1.0),
            ],
            uvs: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            direction: dir,
            cullface: Some(dir),
            tint_index: None,
            shade: true,
            layer: 0,
        }
    }

    fn full_cube() -> Vec<BakedQuad> {
        [
            Direction::Down,
            Direction::Up,
            Direction::North,
            Direction::South,
            Direction::East,
            Direction::West,
        ]
        .into_iter()
        .map(full_face)
        .collect()
    }

    #[test]
    fn full_cube_is_recognised() {
        let cube = full_cube();
        assert!(is_full_cube(&cube));
        assert!(is_packed_cube(&cube));
    }

    #[test]
    fn cube_missing_a_face_is_not_a_cube() {
        let mut cube = full_cube();
        cube.pop(); // drop the West face
        assert!(!is_full_cube(&cube));
    }

    #[test]
    fn duplicate_face_is_not_a_cube() {
        let mut cube = full_cube();
        cube[5] = full_face(Direction::Up); // two Up faces, no West
        assert!(!is_full_cube(&cube));
    }

    #[test]
    fn face_without_cullface_is_not_a_cube() {
        let mut cube = full_cube();
        cube[0].cullface = None; // an uncullable face (cross-plant-like)
        assert!(!is_full_cube(&cube));
    }

    #[test]
    fn slab_half_height_face_is_not_a_full_face() {
        let mut cube = full_cube();
        // Squash the North face to half height: no longer spans the unit square.
        for p in &mut cube[2].positions {
            p[1] *= 0.5;
        }
        assert!(!is_full_cube(&cube));
    }

    #[test]
    fn inset_face_off_the_plane_is_not_a_full_face() {
        let mut cube = full_cube();
        // Pull the Up face down to y=0.9: coplanar test fails.
        for p in &mut cube[1].positions {
            p[1] = 0.9;
        }
        assert!(!is_full_cube(&cube));
    }

    #[test]
    fn extra_seventh_quad_disqualifies() {
        let mut cube = full_cube();
        cube.push(full_face(Direction::Up));
        assert!(!is_full_cube(&cube));
    }

    #[test]
    fn tinted_cube_is_a_cube_but_not_packed() {
        let mut cube = full_cube();
        cube[1].tint_index = Some(0); // tinted top (grass-like)
        assert!(is_full_cube(&cube));
        assert!(!is_packed_cube(&cube));
    }

    #[test]
    fn positions_are_translated_to_the_block_origin() {
        let mut q = cube_face(Direction::Up, None);
        q.positions = [
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let view = OneBlock {
            quads: vec![q],
            occlude_dir: None,
        };
        let m = mesh_models(&view);
        // Block is at (8,8,8), so the first corner lands at (8,9,8).
        assert_eq!(m.vertices[0].position, [8.0, 9.0, 8.0]);
    }

    // --- Fluid meshing ---------------------------------------------------

    use lodestone_assets::fluid::{FluidState, SpriteUv};
    use std::collections::{HashMap, HashSet};

    /// A synthetic fluid neighbourhood: a map of cell -> fluid and a set of
    /// occluding (solid) cells. Sprites are unit rects so quad counts and
    /// positions are the thing under test, not UVs.
    #[derive(Default)]
    struct FakeFluidView {
        fluids: HashMap<(i32, i32, i32), FluidCell>,
        solids: HashSet<(i32, i32, i32)>,
    }
    impl FakeFluidView {
        fn water(&mut self, x: i32, y: i32, z: i32, state: FluidState) {
            self.fluids.insert(
                (x, y, z),
                FluidCell {
                    kind: FluidKind::Water,
                    state,
                },
            );
        }
        fn solid(&mut self, x: i32, y: i32, z: i32) {
            self.solids.insert((x, y, z));
        }
    }
    impl FluidSectionView for FakeFluidView {
        fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
            self.fluids.get(&(x, y, z)).copied()
        }
        fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
            self.solids.contains(&(x, y, z))
        }
        fn fluid_sprites(&self, _kind: FluidKind) -> FluidSprites {
            let unit = SpriteUv {
                min: [0.0, 0.0],
                max: [1.0, 1.0],
            };
            FluidSprites {
                still: unit,
                flow: unit,
            }
        }
    }

    #[test]
    fn lone_water_source_emits_a_surface_below_the_full_block() {
        // A single water source with air above and a solid floor below: emits the
        // top surface + four sides, culls the bottom against the floor.
        let mut v = FakeFluidView::default();
        v.water(8, 8, 8, FluidState::source());
        v.solid(8, 7, 8); // floor below
        let m = mesh_fluids(&v);

        assert_eq!(m.water.quad_count(), 5, "top + 4 sides, bottom culled by floor");
        assert!(m.lava.vertices.is_empty(), "no lava");

        // The top surface sits below the full block: a source's corners are
        // pulled down by the surrounding air, so it is strictly below y+1.
        let top_y = m
            .water
            .vertices
            .iter()
            .map(|vx| vx.position[1])
            .fold(f32::MIN, f32::max);
        assert!(
            top_y > 8.0 && top_y < 9.0,
            "water surface should sit between the block base and a full cube, got {top_y}"
        );
    }

    #[test]
    fn shared_face_between_two_water_cells_is_not_emitted() {
        // Two adjacent water sources: the face between them is culled, the outer
        // faces remain. This is the "faces between two water blocks must not be
        // emitted" requirement.
        let mut lone = FakeFluidView::default();
        lone.water(8, 8, 8, FluidState::source());
        let lone_sides = mesh_fluids(&lone).water.quad_count();

        let mut pair = FakeFluidView::default();
        pair.water(8, 8, 8, FluidState::source());
        pair.water(9, 8, 8, FluidState::source()); // east neighbour, same fluid
        let pair_quads = mesh_fluids(&pair).water.quad_count();

        // Each cell loses its shared (east/west) face: 2 fewer than 2x lone.
        assert_eq!(
            pair_quads,
            lone_sides * 2 - 2,
            "the shared water-water face on both cells must be culled"
        );
    }

    #[test]
    fn lava_meshes_on_the_opaque_pass_full_bright() {
        let mut v = FakeFluidView::default();
        v.fluids.insert(
            (8, 8, 8),
            FluidCell {
                kind: FluidKind::Lava,
                state: FluidState::source(),
            },
        );
        let m = mesh_fluids(&v);
        assert!(m.water.vertices.is_empty(), "lava is not water");
        assert!(m.lava.quad_count() > 0, "lava emits geometry");
        // Full-bright: every lava vertex carries max sky+block light.
        assert!(
            m.lava.vertices.iter().all(|vx| vx.light == 0xFF),
            "lava must be emitted full-bright"
        );
    }
}
