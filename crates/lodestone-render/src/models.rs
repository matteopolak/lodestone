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
//! **Verified against the tree (not assumed): today this policy's first bullet
//! has zero production callers.** `lodestone-shell`'s live (`Vanilla`)
//! classifier always attaches a real `BlockModels`
//! (`crate::block_resolver::BlockAtlas::with_models`, called unconditionally by
//! `BlockResources::try_vanilla`), so `mesh_one`'s `classifier.models()` branch
//! is always `Some` and every live block — stone and dirt included — is meshed
//! by [`mesh_models_layers`] through the wide, never-merged, per-quad path this
//! module owns. [`is_packed_cube`]/[`mesh_greedy`](crate::mesh::mesh_greedy)/
//! `SectionGeometry::Packed` are reachable **only** through
//! `ShellClassifier::Demo`, the offline 10-block sandbox palette, whose own
//! doc says water there "renders opaque in this demo" — it has no translucent
//! block in its palette at all. So the D1 argument below (packed cubes
//! dominate rendered *volume*) is a real, measured fact about the baked model
//! set, but is not, right now, a fact about what the live game draws: every
//! live block currently pays the wide [`ModelVertex`] cost (152 vs 72 bytes
//! per quad including indices — see [`model_vram_bytes`] vs
//! [`crate::vertex::vram_bytes`]), and [`is_packed_cube`] requiring a real
//! [`RenderLayer`] (below) is a no-op on today's live rendering *precisely
//! because* nothing calls it there yet — it only forecloses the mistake for
//! whoever wires the packed path back in for live full cubes.
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
//! would still leave ≥2 pipelines, while paying **2.1× per quad** (72 → 152 bytes
//! incl. indices; 2.67× per vertex, 12 → 32 — the 28→32 growth is
//! `tint_rgb_override`, added later for real per-position biome tint) on the
//! single largest consumer, (1).
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

use glam::{Mat4, Vec3};
use lodestone_assets::fluid::{
    FaceSet, FlowNeighbor, FluidGeometry, SelfOcclusion, SideOverlay, bake_fluid, corner_heights,
    flow_horizontal, neighbor_height,
};
use lodestone_assets::{BakedQuad, Direction, GuiLight};

use crate::block_models::{FluidCell, FluidKind, FluidSprites};
use crate::fluid_grid::{FluidGrid, FluidNeighborCell};
use crate::section::{Face, SECTION_SIZE};
use crate::translucency::RenderLayer;

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
    /// The animation slot of the sprite this vertex samples, or `0` when the
    /// sprite is static. Repurposes the first padding byte (the stride is
    /// unchanged): the shader reads it as `packed.z` and, when non-zero, offsets
    /// the sampled V by the slot's per-frame amount to advance the animation.
    pub anim: u8,
    /// `0` for "run the fragment shader's ordinary cutout discard" (every
    /// caller before this field had meaning); nonzero skips it, so the fully
    /// sampled texel — including whatever colour sits under an alpha hole —
    /// paints solid.
    ///
    /// This is vanilla's `options.cutoutLeaves == false` (FAST): vanilla does
    /// not achieve solid leaves by editing the texture or the geometry, it
    /// renders leaves through the *solid* render pass, which simply never
    /// runs the alpha test — so the RGB already sitting under a "hole" texel
    /// is what paints, exactly as it does here. [`crate::model_pipeline`]
    /// only ever builds **one** opaque pipeline (the shader's cutout discard
    /// is unconditional otherwise — see `model.wgsl`'s `fs_main`), so this is
    /// the pass-level bypass the render side needs without a second pipeline
    /// or a second bake: a per-vertex flag rather than a per-draw-call
    /// setting, because leaves share one section's opaque mesh with every
    /// other block and are not their own draw call.
    ///
    /// Set only by `mesher::mesh_models` for a quad whose block is one of
    /// `BlockModels::is_leaves` **and** the live `cutout_leaves` option is
    /// off (`mesher::ModelSectionView::force_opaque_at`). Every other
    /// caller — GUI items, the held/dropped-item mesh, entities, fluids,
    /// headless tests — leaves this `0` and keeps rendering exactly as
    /// before this field existed, the same "additive, defaults to inert"
    /// shape [`Self::tint_rgb_override`]'s doc argues for.
    ///
    /// Was padding (`_pad`) before this — same byte, same stride, no vertex
    /// layout change: a new *meaning* for an existing wire position, not a
    /// new one. It still costs nothing against the render crate's four-bind-
    /// group floor for the same reason [`Self::tint_rgb_override`] does not:
    /// a vertex attribute, not a bind group.
    pub cutout_bypass: u8,
    /// A **real, position-resolved** biome tint colour, or an untinted/inert
    /// sentinel — see [`Self::tint_rgb_override`]. `tint` above still indexes
    /// the frame-shared palette (group 2) for [`TintKind::Constant`]/
    /// [`TintKind::RedstonePower`]/untinted quads, which do not vary by
    /// position; this field exists because the palette *cannot* hold a
    /// different colour per section for the four kinds that do (grass,
    /// foliage, dry-foliage, water — see `block_models::biome_tint_slot`'s
    /// doc). `.w` (alpha) is the override flag: `0` means "no override, read
    /// the palette at `tint` as before" and `255` means "use `.rgb` directly".
    /// A **new, additive vertex attribute** rather than a vertex-format
    /// change to the existing four: no bind group is touched (vertex
    /// attributes cost nothing against the 4-group floor), and every existing
    /// caller that never sets this (GUI items, headless tests, an untinted
    /// quad) keeps rendering exactly as before because the flag defaults to
    /// `0`.
    ///
    /// [`TintKind::Constant`]: lodestone_assets::tint::TintKind::Constant
    /// [`TintKind::RedstonePower`]: lodestone_assets::tint::TintKind::RedstonePower
    pub tint_rgb_override: [u8; 4],
}

impl ModelVertex {
    /// The `wgpu` vertex-buffer layout for the wide model vertex, **not**
    /// carrying [`tint_rgb_override`](Self::tint_rgb_override) as a shader
    /// input.
    ///
    /// Four attributes over the 32-byte stride: position (`Float32x3`), UV
    /// (`Float32x2`), AO (`Float32`), and the packed `light`/`tint`/`anim`/
    /// `cutout_bypass` tail as one `Uint8x4`. Locations `0..=3`.
    ///
    /// This is the layout [`crate::entity_pipeline`] builds its own
    /// **instance** buffer's attributes on top of, starting at location `4`
    /// (`entity_pipeline.rs`: "Instance attributes start at location 4, past
    /// `ModelVertex`'s 0..=3") — so this method's location range is a promise
    /// to that pipeline, not just a description of the struct, and must never
    /// grow. [`Self::vertex_layout_with_biome_tint`] is the model/fluid
    /// pipelines' own five-attribute variant, kept as a **separate** method
    /// for exactly this reason: growing *this* one to expose
    /// `tint_rgb_override` at location `4` collided with the entity
    /// pipeline's instance attributes there — measured directly, a
    /// `wgpu` validation panic ("Two or more vertex attributes were assigned
    /// to the same location in the shader: 4") on every entity pixel gate,
    /// caught by running them after this feature's other 771 render-crate
    /// tests had already gone green.
    ///
    /// The `array_stride` is still the full 32-byte struct size (not 28):
    /// `tint_rgb_override` still occupies real space in every uploaded
    /// vertex, it is just four bytes this layout doesn't expose to a shader —
    /// exactly like the entity shader already not reading the `light` field
    /// meaningfully (`push_part_quads`'s doc: "The entity shader does **not**
    /// read this byte") despite the field being present in every vertex.
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

    /// [`Self::vertex_layout`] plus a fifth attribute exposing
    /// [`tint_rgb_override`](Self::tint_rgb_override) at location `4` — the
    /// real, position-resolved biome colour. Used by the **model** and
    /// **fluid** pipelines only (`model_pipeline.rs`'s `ModelPipeline::build`),
    /// neither of which has an instance buffer contesting location `4`. Do
    /// **not** use this for the entity pipeline — see [`Self::vertex_layout`]'s
    /// doc for why that collides.
    #[must_use]
    pub const fn vertex_layout_with_biome_tint() -> wgpu::VertexBufferLayout<'static> {
        const ATTRS: [wgpu::VertexAttribute; 5] = [
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
            wgpu::VertexAttribute {
                format: wgpu::VertexFormat::Uint8x4,
                offset: 28,
                shader_location: 4,
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

    /// Append `other`'s geometry into `self`, rebasing its indices onto the
    /// current vertex count. Used to fold lava (which meshes through the fluid
    /// path) into the opaque model mesh it shares a pass with.
    pub fn merge(&mut self, other: &ModelMesh) {
        let base = self.vertices.len() as u32;
        self.vertices.extend_from_slice(&other.vertices);
        self.indices.extend(other.indices.iter().map(|&i| i + base));
    }
}

/// Bytes per wide model vertex (asserted to be 32 — see
/// `tint_rgb_override`'s doc for the 28→32 growth). This is the format the
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
///
/// `cullface == Some(direction)` is the model author's own declaration that the
/// face sits on the block boundary, so this is data-driven rather than inferred
/// from coordinates alone. Public because occlusion is decided **per face** in
/// [`block_models`](crate::block_models) — a block whose *layer* is cutout can
/// still present opaque boundary faces (`grass_block`).
#[must_use]
pub fn quad_is_full_face(q: &BakedQuad) -> bool {
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

/// Vanilla `BlockModelLighter.prepareQuadShape`'s `faceCubic`
/// (`BlockModelLighter.java:265-272`): whether the quad's plane is flush with
/// the block boundary on its own facing axis.
///
/// **Not** [`quad_is_full_face`], which additionally demands a full `1×1` span
/// and `cullface == direction`. Vanilla's test is planarity plus position
/// only, so a stair's top step qualifies and a cross blade or a fence post's
/// side does not.
#[must_use]
fn quad_is_on_face_boundary(q: &BakedQuad) -> bool {
    const EPS: f32 = 1e-4;
    let (fixed, plane) = face_plane(q.direction);
    q.positions.iter().all(|p| (p[fixed] - plane).abs() <= EPS)
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
/// ([`is_full_cube`]) whose faces are all **untinted** and whose block
/// [`RenderLayer`] is [`RenderLayer::Solid`].
///
/// Two disqualifiers, not one. The packed vertex has no per-vertex colour, so a
/// tinted cube (grass, leaves, water still-cube) must use the wider
/// [`ModelVertex`] path even though its geometry is a cube. And the packed
/// block shader (`block.wgsl`, unlike [`crate::shaders`]'s `model.wgsl`) has no
/// alpha-test discard and no blending — it draws depth-written and opaque
/// unconditionally — so a full cube whose real per-texel alpha is `Cutout` or
/// `Translucent` (stained glass, ice, tinted glass, slime, honey: measured,
/// every texel of each carries alpha strictly between 0 and 255, see
/// `crates/lodestone-render/tests/block_models_gate.rs` and the sibling gate
/// for `is_packed_cube` itself) would render as a solid, see-through-nothing
/// block if this predicate ever routed it there. **`layer` must come from the
/// same [`crate::block_models::BlockModels::layer`] derivation the model path
/// already trusts** — never re-derived or assumed — so this function cannot be
/// satisfied by geometry alone; a caller is forced to look up the real layer
/// before asking.
#[must_use]
pub fn is_packed_cube(quads: &[BakedQuad], layer: RenderLayer) -> bool {
    is_full_cube(quads)
        && layer == RenderLayer::Solid
        && quads.iter().all(|q| q.tint_index.is_none())
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

    /// Vanilla's `BlockBehaviour.skipRendering` clause of `Block.
    /// shouldRenderFace`: whether the face of the block at `(x, y, z)` facing
    /// its neighbour at (possibly out-of-section) `(nx, ny, nz)` is never
    /// drawn because the two are the exact same `HalfTransparentBlock` (glass,
    /// every stained-glass colour, tinted glass, ice, blue ice, frosted ice,
    /// honey, slime) — see
    /// [`BlockModels::skips_rendering_against`](crate::BlockModels::skips_rendering_against).
    ///
    /// Independent of [`occludes_at`](Self::occludes_at): every member of this
    /// class answers `false` there (vanilla's `noOcclusion()`), which is
    /// exactly why their interior faces need this second, class-keyed check —
    /// without it, a wall of the same translucent block draws every interior
    /// face and reads as a wireframe lattice.
    ///
    /// Defaults to `false`, reproducing the pre-fix behaviour exactly for a
    /// view that has not opted in (every hermetic test/GUI-item view except
    /// the ones this fixes).
    fn skips_rendering_against(&self, x: i32, y: i32, z: i32, nx: i32, ny: i32, nz: i32) -> bool {
        let _ = (x, y, z, nx, ny, nz);
        false
    }

    /// Sky/block light at section-local `(x, y, z)`, packed sky<<4 | block.
    ///
    /// This is the block's **own** cell. For an opaque full cube that value is
    /// `0` in every dimension — light does not propagate *into* a solid — so it
    /// is almost never the value a visible face should carry. Implement
    /// [`face_light_at`](Self::face_light_at) instead; this stays as the
    /// fallback for views that model no neighbourhood (tests, GUI items).
    fn light_at(&self, x: usize, y: usize, z: usize) -> u8 {
        let _ = (x, y, z);
        0xF0
    }

    /// Packed sky/block light for a quad of the block at `(x, y, z)` facing
    /// `dir` — the light of the **neighbouring cell the face opens into**.
    ///
    /// This is vanilla's rule (`ModelBlockRenderer` reads
    /// `getLightColor(level, state, pos.relative(quad.getDirection()))`), and it
    /// is not a refinement: sampling the block's own cell renders every opaque
    /// block at its stored light, which the light engine defines as `0`. A world
    /// meshed that way is uniformly dark, and a *just-placed* block — whose cell
    /// still holds the air light it replaced until the server's relight lands —
    /// renders full-bright against it. Reading the neighbour instead makes the
    /// stale own-cell value unreachable, so there is no bright window at all.
    ///
    /// The default forwards to [`light_at`](Self::light_at) so existing views
    /// keep their behaviour.
    fn face_light_at(&self, x: usize, y: usize, z: usize, dir: Direction) -> u8 {
        let _ = dir;
        self.light_at(x, y, z)
    }

    /// Packed sky/block light at a **signed** coordinate that may fall
    /// outside the block whose face is being lit.
    ///
    /// Used only by the ambient-occlusion corner sampler
    /// ([`quad_corner_sample`]), which needs the two edge-adjacent
    /// neighbours and the diagonal around each of a quad's four corners —
    /// [`face_light_at`](Self::face_light_at) only ever resolves the single
    /// cell a face opens into, not the cells beside it. Defaults to
    /// full-bright, matching [`light_at`](Self::light_at)'s default, for
    /// views that model no neighbourhood at all (tests, GUI items).
    fn corner_light_at(&self, x: i32, y: i32, z: i32) -> u8 {
        let _ = (x, y, z);
        0xF0
    }

    /// Whether the block at a **signed** coordinate darkens an adjacent
    /// ambient-occlusion corner — vanilla's
    /// `BlockBehaviour.getShadeBrightness(state, level, pos) == 0.2F`
    /// (`BlockBehaviour.java:315-317`), the value
    /// `BlockModelLighter.prepareQuadAmbientOcclusion` averages into every
    /// smooth-lit vertex (`BlockModelLighter.java:45-110`).
    ///
    /// **This is deliberately not [`occludes_at`](Self::occludes_at).** That one
    /// is a *rendering* predicate (does an opaque quad cover the boundary on all
    /// six faces), which is the right question for `cullface` and the wrong one
    /// here: vanilla's is a *collision* predicate,
    /// `state.isCollisionShapeFullBlock(..) ? 0.2F : 1.0F`, with seven class
    /// overrides. The two agree on stone, on slabs, on water and — by
    /// coincidence, via `TransparentBlock`'s override — on glass, and they
    /// disagree on **every full collision cube whose model does not occlude for
    /// culling**: leaves above all, plus slime, spawner, beacon and ice.
    /// Dumping vanilla's own answer measured **39 states across 30 blocks**
    /// where a derivation from the collision shape alone is wrong, in both
    /// directions — see `lodestone_data::shade_brightness`.
    ///
    /// The player-visible symptom of getting this wrong is the one issue #22's
    /// remaining divergence named: the underside of a tree canopy stays
    /// full-bright, where vanilla renders it markedly dimmer.
    ///
    /// Only the **AO** half of [`quad_corner_sample`] consults this. The
    /// *light* half keeps using [`occludes_at`](Self::occludes_at), because
    /// vanilla's smooth-light substitution is keyed on a third predicate again
    /// (`translucentN` = `!isViewBlocking || getLightDampening() == 0`, plus
    /// `LightCoordsUtil.smoothBlend`'s packed-light-is-zero test) which
    /// `occludes_at` is much the nearer stand-in for. Swapping both would make
    /// a leaf cell hand its own darkness to its neighbours' *light*, which
    /// vanilla does not do.
    ///
    /// Defaults to [`occludes_at`](Self::occludes_at) so a view that models no
    /// block registry (tests, GUI items) keeps its previous behaviour exactly,
    /// and so this stays a one-method override for views that do.
    fn ao_occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
        self.occludes_at(x, y, z)
    }

    /// Whether ambient occlusion applies to the block at section-local
    /// `(x, y, z)`, or its quads should fall back to flat per-face light —
    /// vanilla's `ModelBlockRenderer.tesselateBlock` choosing between
    /// `tesselateAmbientOcclusion` and `tesselateFlat`:
    /// `this.ambientOcclusion && blockState.getLightEmission() == 0 &&
    /// parts.getFirst().useAmbientOcclusion()`.
    ///
    /// `this.ambientOcclusion` is the renderer-wide "Smooth Lighting" video
    /// option, which this client has no equivalent setting for (smooth
    /// lighting is always on), so this method only needs to answer the
    /// remaining two, block-specific conditions. It currently answers only
    /// the model half (`useAmbientOcclusion`, the JSON `ambientocclusion`
    /// property) — see
    /// [`BlockModels::ambient_occlusion`](crate::BlockModels::ambient_occlusion)
    /// for why the light-emission half is not applied yet.
    ///
    /// Defaults to `true`, matching the JSON default and the overwhelming
    /// majority of blocks — existing [`ModelSectionView`] implementations
    /// (tests, GUI items) need no change to keep compiling.
    fn ambient_occlusion_at(&self, x: usize, y: usize, z: usize) -> bool {
        let _ = (x, y, z);
        true
    }

    /// Whether the block at section-local `(x, y, z)` should skip the model
    /// shader's cutout discard entirely and render every sampled texel
    /// opaque — vanilla's `options.cutoutLeaves == false` (FAST), which draws
    /// leaves through the *solid* render pass rather than the alpha-tested
    /// one. See [`ModelVertex::cutout_bypass`] for why this is a per-vertex
    /// flag and not a second pipeline.
    ///
    /// Defaults to `false` (run the ordinary cutout discard), so every
    /// existing [`ModelSectionView`] implementation — tests, GUI items —
    /// needs no change to keep compiling and keeps rendering exactly as
    /// before this method existed.
    fn force_opaque_at(&self, x: usize, y: usize, z: usize) -> bool {
        let _ = (x, y, z);
        false
    }

    /// The [`RenderLayer`](crate::translucency::RenderLayer) of one quad of the
    /// block at section-local `(x, y, z)` — vanilla's per-**quad** render-layer
    /// bucketing, and the thing that decides both which mesh the quad lands in
    /// and whether the fragment shader's cutout discard runs on it.
    ///
    /// `SectionCompiler` sends every quad to `quad.materialInfo().layer()`,
    /// which `ChunkSectionLayer.byTransparency` derives from the transparency
    /// of that quad's *own* sprite. So a block state that mixes sprites is
    /// split across passes: `grass_block`'s six opaque cube faces draw through
    /// `SOLID_TERRAIN` — which defines no `ALPHA_CUTOUT` and therefore runs
    /// **no alpha test at all** — while its four coplanar
    /// `grass_block_side_overlay` decals draw through `CUTOUT_TERRAIN` at
    /// `0.5`. A per-block-state layer cannot express that: rolling the state up
    /// to "the most transparent layer across its faces" alpha-tests the opaque
    /// faces too, and under minification the mip chain can pull a filtered
    /// alpha below the threshold at a sprite edge, discarding a fragment
    /// vanilla paints opaque.
    ///
    /// Returning [`Solid`](crate::translucency::RenderLayer::Solid) is what
    /// sets [`ModelVertex::cutout_bypass`] on the emitted vertices, which is
    /// this renderer's stand-in for vanilla's separate `SOLID_TERRAIN`
    /// pipeline (see that field's doc for why the bypass is per-vertex and not
    /// a second pipeline). Returning
    /// [`Translucent`](crate::translucency::RenderLayer::Translucent) routes
    /// the quad into [`mesh_models_layers`]'s **second** mesh, which is drawn
    /// with real alpha blending and back-to-front order — a genuinely
    /// partial-alpha sprite (stained glass, ice, the nether-portal swirl).
    ///
    /// Defaults to `None`, meaning "this view cannot classify quads": every
    /// quad then lands in the first mesh with the ordinary cutout discard, the
    /// behaviour every [`ModelSectionView`] had before per-quad layers existed.
    /// Only a view backed by a real [`BlockModels`](crate::BlockModels)
    /// registry (`SnapshotModelView` in `lodestone-shell`) answers it, through
    /// [`BlockModels::sprite_layer`](crate::BlockModels::sprite_layer) on the
    /// quad's own [`BakedQuad::sprite`] index.
    ///
    /// [`ModelVertex::cutout_bypass`]: ModelVertex::cutout_bypass
    /// [`BakedQuad::sprite`]: lodestone_assets::bake::BakedQuad::sprite
    fn quad_layer(&self, x: usize, y: usize, z: usize, quad: &BakedQuad) -> Option<RenderLayer> {
        let _ = (x, y, z, quad);
        None
    }

    /// The **real**, position-resolved colour for a biome-dependent quad at
    /// section-local `(x, y, z)`, or `None` to fall back to the frame-shared
    /// palette's plains-default colour at that `slot` — exactly today's
    /// behaviour.
    ///
    /// `slot` is the quad's `tint_index` byte (`quad.tint_index.map_or(255,
    /// |t| t as u8)`), which [`mesh_models`] already computed to build the
    /// vertex; a view only needs to recognise the four reserved slots
    /// (`crate::block_models::{GRASS_TINT_SLOT, FOLIAGE_TINT_SLOT,
    /// DRY_FOLIAGE_TINT_SLOT, WATER_TINT_SLOT}`) and can ignore any other
    /// value (untinted, or a constant/redstone colour that is not
    /// position-dependent and has nothing to override).
    ///
    /// Defaults to `None` — no live biome data — so every existing view (GUI
    /// items, headless tests, a demo world with no biome grid at all) keeps
    /// rendering the exact plains-default palette colour it always has. This
    /// is the seam a real [`lodestone_assets::tint::BiomeTint`] implementor
    /// plugs into; see `crates/lodestone-shell/src/mesher.rs`'s
    /// `SnapshotModelView` for the live one.
    fn biome_tint_at(&self, x: usize, y: usize, z: usize, slot: u8) -> Option<[u8; 3]> {
        let _ = (x, y, z, slot);
        None
    }
}

/// AO shade of a fully-occluding corner neighbour. Mirrors [`crate::mesh`]'s
/// constant of the same value — vanilla's darkest ambient-occlusion sample is
/// `0.2`, not `0.0`, so a corner with all three neighbours occluding still
/// averages to `0.4` once the always-open front cell's `1.0` is folded in.
/// Duplicated rather than shared because that copy is private and keyed to
/// `crate::mesh`'s `Cell`, not [`ModelSectionView`].
const AO_OCCLUDED: f32 = 0.2;

/// Vanilla only substitutes an occluding neighbour's light with the centre
/// light once the centre itself is lit above this threshold
/// (`LightCoordsUtil.smoothBlend`). Mirrors [`crate::mesh`]'s constant.
const SMOOTH_LIGHT_MIN_CENTRE: u8 = 2;

/// In-plane `(u, v)` unit axes of `face` — the two directions a quad's corner
/// steps along to reach its edge-adjacent AO neighbours (the diagonal is
/// `u + v`). Mirrors `crate::mesh::face_geom`'s `u`/`v` (that table is private
/// to the demo mesher and carries a `base` this function does not need).
const fn face_uv_axes(face: Face) -> ([i32; 3], [i32; 3]) {
    match face {
        Face::NegX => ([0, 0, 1], [0, 1, 0]),
        Face::PosX => ([0, 1, 0], [0, 0, 1]),
        Face::NegY => ([1, 0, 0], [0, 0, 1]),
        Face::PosY => ([0, 0, 1], [1, 0, 0]),
        Face::NegZ => ([0, 1, 0], [1, 0, 0]),
        Face::PosZ => ([1, 0, 0], [0, 1, 0]),
    }
}

/// Index of the single nonzero component of a unit axis.
fn axis_of(v: [i32; 3]) -> usize {
    if v[0] != 0 {
        0
    } else if v[1] != 0 {
        1
    } else {
        2
    }
}

/// Rounds a `0..=15` light average to the nearest representable nibble.
fn round_level(v: f32) -> u8 {
    v.round().clamp(0.0, 15.0) as u8
}

/// Vanilla-style smooth light and ambient occlusion for one vertex of a quad.
///
/// Ported from `ModelBlockRenderer.AmbientOcclusionFace`. `np` is the cell the
/// quad's face opens into (block position + face normal) — the same cell
/// [`ModelSectionView::face_light_at`] already resolved into `centre_light`.
/// `p` is the vertex's block-local position (`quad.positions[i]`); projecting
/// it onto the face's in-plane axes ([`face_uv_axes`]) picks which of the four
/// possible corners it belongs to, then samples the two edge-adjacent
/// neighbours and the diagonal at that corner:
///
/// * **AO** averages a per-cell shade (`1.0` open, [`AO_OCCLUDED`] occluding)
///   over those three plus the always-open front cell. "Occluding" here is
///   [`ModelSectionView::ao_occludes_at`] — vanilla's `getShadeBrightness`, a
///   *collision* test — **not** [`ModelSectionView::occludes_at`]. Read that
///   method's docs before touching this: using the culling predicate is what
///   left the underside of a tree canopy full-bright.
/// * **Light** averages the same four cells' sky/block levels, but an
///   occluding neighbour's value is replaced by the centre light
///   (`smoothBlend`) once the centre itself is lit above
///   [`SMOOTH_LIGHT_MIN_CENTRE`] — so a corner against a wall does not read as
///   pitch black. This half keeps [`ModelSectionView::occludes_at`], because
///   vanilla keys it on view-blocking / light dampening rather than on shade
///   brightness.
///
/// **Not ported**: vanilla weights these four samples by how much of the
/// quad's actual face area is nearest each cube corner, which matters for a
/// quad that doesn't span a full block face (a stair or slab). This always
/// takes the corner nearest the vertex outright — exactly what the full-cube
/// demo mesher ([`crate::mesh`]) does too, just generalised to a vertex
/// position that may not land exactly on a cube corner. Also not ported:
/// vanilla's `translucentN` hidden-diagonal substitution, which only affects
/// non-cube models' interior faces.
fn quad_corner_sample(
    view: &dyn ModelSectionView,
    np: [i32; 3],
    face: Face,
    p: [f32; 3],
    centre_light: u8,
) -> (f32, u8) {
    let (u, v) = face_uv_axes(face);
    let su = if p[axis_of(u)] >= 0.5 { 1 } else { -1 };
    let sv = if p[axis_of(v)] >= 0.5 { 1 } else { -1 };
    let a = [np[0] + su * u[0], np[1] + su * u[1], np[2] + su * u[2]];
    let b = [np[0] + sv * v[0], np[1] + sv * v[1], np[2] + sv * v[2]];
    let d = [a[0] + sv * v[0], a[1] + sv * v[1], a[2] + sv * v[2]];

    // Two different predicates, on purpose — see
    // `ModelSectionView::ao_occludes_at`. Vanilla's AO term is
    // `getShadeBrightness` (a collision test); its smooth-light substitution is
    // keyed on view-blocking / light dampening instead, for which `occludes_at`
    // is the nearer stand-in. Leaves are the population that separates them.
    let shade_occ = |c: [i32; 3]| view.ao_occludes_at(c[0], c[1], c[2]);
    let (shade_a, shade_b, shade_d) = (shade_occ(a), shade_occ(b), shade_occ(d));
    let ao_of = |o: bool| if o { AO_OCCLUDED } else { 1.0 };
    let ao = (ao_of(shade_a) + ao_of(shade_b) + ao_of(shade_d) + 1.0) * 0.25;

    let occ = |c: [i32; 3]| view.occludes_at(c[0], c[1], c[2]);
    let (occ_a, occ_b, occ_d) = (occ(a), occ(b), occ(d));

    let centre_sky = centre_light >> 4;
    let centre_block = centre_light & 0xF;
    let substitute = |occludes: bool, sample: u8, centre_v: u8| {
        if occludes && centre_v > SMOOTH_LIGHT_MIN_CENTRE {
            centre_v
        } else {
            sample
        }
    };
    let light_of = |c: [i32; 3], occludes: bool| {
        let raw = view.corner_light_at(c[0], c[1], c[2]);
        (
            substitute(occludes, raw >> 4, centre_sky),
            substitute(occludes, raw & 0xF, centre_block),
        )
    };
    let (sky_a, block_a) = light_of(a, occ_a);
    let (sky_b, block_b) = light_of(b, occ_b);
    let (sky_d, block_d) = light_of(d, occ_d);
    let sky =
        round_level((centre_sky as f32 + sky_a as f32 + sky_b as f32 + sky_d as f32) / 4.0);
    let block = round_level(
        (centre_block as f32 + block_a as f32 + block_b as f32 + block_d as f32) / 4.0,
    );
    (ao, (sky << 4) | block)
}

/// Mesh the non-cube geometry of a section, emitting each visible baked quad
/// once, never merged. A quad is culled only when it carries a `cullface` and
/// the neighbouring block in that direction fully occludes it.
///
/// # Smooth lighting and ambient occlusion
///
/// Each vertex gets its own AO factor and smoothed light from
/// [`quad_corner_sample`], keyed on the cell the quad's face opens into
/// ([`ModelSectionView::face_light_at`]) and the vertex's own position within
/// that face. The AO factor rides in the per-vertex `ao` slot alongside the
/// constant per-face directional shade (multiplied together — see
/// `emit_baked_quad`); the shader already multiplies `ao * light_term` per
/// vertex in gamma space (`4e8f058`'s rule), so a finer-grained `ao` is a
/// drop-in, no shader change needed. When the two AO values on one diagonal of
/// a quad disagree, the quad is triangulated along the other diagonal
/// (vanilla's anisotropy fix), matching `crate::mesh::emit_quad`.
///
/// The fluid path ([`mesh_fluids`]) stays flat by design — see its own docs.
///
/// Merges [`mesh_models_layers`]'s two outputs into one — the shape this
/// function had before block-level [`RenderLayer`](crate::translucency::RenderLayer)
/// routing existed, kept for callers (tests, benches) that only want the
/// combined geometry and do not draw the translucent half through its own
/// blended pass.
#[must_use]
pub fn mesh_models(view: &dyn ModelSectionView) -> ModelMesh {
    let (mut mesh, translucent) = mesh_models_layers(view);
    mesh.merge(&translucent);
    mesh
}

/// Like [`mesh_models`], but keeps every quad on the
/// [`RenderLayer::Translucent`](crate::translucency::RenderLayer::Translucent)
/// pass ([`ModelSectionView::quad_layer`]) in a **second** mesh instead of
/// folding it into the first.
///
/// The split exists because the two meshes are drawn through different
/// pipelines: the first (`Solid` + `Cutout`) is depth-written and uses a
/// single alpha-test/discard shader; the second needs real alpha blending,
/// no depth write and back-to-front ordering — see
/// `lodestone-shell`'s `gpu/frame.rs` translucent-block draw pass.
///
/// The split is per **quad**, matching vanilla: `SectionCompiler` opens one
/// buffer per `ChunkSectionLayer` and picks the buffer from
/// `quad.materialInfo().layer()`, so a single block state's geometry can and
/// does land in more than one of them. A water cauldron is the clearest case —
/// its opaque body writes depth on the solid pass and its partial-alpha liquid
/// blends on the translucent one, which is precisely what a per-block-state
/// routing could not express.
#[must_use]
pub fn mesh_models_layers(view: &dyn ModelSectionView) -> (ModelMesh, ModelMesh) {
    let mut mesh = ModelMesh::default();
    let mut translucent_mesh = ModelMesh::default();
    let n = SECTION_SIZE;
    for y in 0..n {
        for z in 0..n {
            for x in 0..n {
                let quads = view.quads_at(x, y, z);
                if quads.is_empty() {
                    continue;
                }
                // Per *block*, matching vanilla: `tesselateBlock` picks AO or
                // flat once per block (`parts.getFirst().useAmbientOcclusion()`),
                // not per quad, so every quad of a `"ambientocclusion": false`
                // model (or, once light emission is threaded through, a torch or
                // glowstone) renders flat together.
                let ao_enabled = view.ambient_occlusion_at(x, y, z);
                // Vanilla's `state.isCollisionShapeFullBlock(level, pos)` clause
                // of `faceCubic` (`BlockModelLighter.java:265-272`). We have no
                // collision-shape table on this trait; `occludes_at` on the
                // block's *own* cell covers the population the clause exists
                // for — opaque full cubes, whose interior quads must still be
                // lit from the neighbour. A non-opaque full collision cube
                // (slime, spawner, ice) falls to the own cell instead, which
                // for a non-opaque cell carries real light, so the
                // approximation errs bright rather than black.
                let own_is_full_cube = view.occludes_at(x as i32, y as i32, z as i32);
                // Vanilla's FAST leaves: real per-block, not per-quad, matching
                // `ambient_occlusion_at` above — a block either renders through
                // the solid pass or it does not.
                let force_opaque = view.force_opaque_at(x, y, z);
                for quad in quads {
                    if let Some(cf) = quad.cullface {
                        let nrm = face_of_direction(cf).normal();
                        let (nx, ny, nz) = (
                            x as i32 + nrm[0],
                            y as i32 + nrm[1],
                            z as i32 + nrm[2],
                        );
                        // `Block.shouldRenderFace`'s two early-outs, in order:
                        // the neighbour's shape fully occludes (`occludes_at`),
                        // or this exact `HalfTransparentBlock` neighbours
                        // itself (`skips_rendering_against`) — see that
                        // method's doc for why occlusion alone cannot cull an
                        // ice/glass wall's interior faces.
                        if view.occludes_at(nx, ny, nz)
                            || view.skips_rendering_against(
                                x as i32, y as i32, z as i32, nx, ny, nz,
                            )
                        {
                            continue;
                        }
                    }
                    // Vanilla `ModelBlockRenderer.tesselateFlat` (:165, :175,
                    // :186-187) plus `BlockModelLighter.prepareQuadFlat`
                    // (:205-208) and `.prepareQuadAmbientOcclusion` (:39, :117):
                    //   * a quad in a *culled* bucket is lit from the cell its
                    //     `cullface` opens into — the bucket direction, which is
                    //     not always `quad.direction` (powder_snow, see
                    //     `block_models.rs:2031`);
                    //   * an *unculled* quad is lit from the neighbour only when
                    //     its plane is flush with the block boundary
                    //     (`faceCubic`), otherwise from the block's OWN cell.
                    // A cross blade is unculled and its plane is diagonal, so
                    // it is lit from its own cell. Sampling the neighbour reads
                    // the interior of an adjacent solid, which the light engine
                    // stores as 0 — the "grass is black on one side" report.
                    let sample_dir = quad.cullface.or_else(|| {
                        (quad_is_on_face_boundary(quad) || own_is_full_cube)
                            .then_some(quad.direction)
                    });
                    let (np, light) = match sample_dir {
                        Some(d) => {
                            let n = face_of_direction(d).normal();
                            let np = [x as i32 + n[0], y as i32 + n[1], z as i32 + n[2]];
                            (np, view.face_light_at(x, y, z, d))
                        }
                        // Vanilla's `faceCubic == false` branch: the ring and
                        // the centre light both move back onto the block's own
                        // cell. `corner_light_at` at the own coordinate IS the
                        // own cell's exact packed light, and using it keeps the
                        // centre value consistent with the ring — a `max`-over-
                        // neighbourhood centre against exact corners is the
                        // self-inconsistency `grass_light_response_gate.rs:
                        // 255-270` documents.
                        None => (
                            [x as i32, y as i32, z as i32],
                            view.corner_light_at(x as i32, y as i32, z as i32),
                        ),
                    };
                    let corners = if ao_enabled {
                        let face = face_of_direction(quad.direction);
                        [0, 1, 2, 3]
                            .map(|i| quad_corner_sample(view, np, face, quad.positions[i], light))
                    } else {
                        // `tesselateFlat`: uniform light, no per-corner AO — the
                        // same fallback the fluid path uses.
                        [(1.0, light); 4]
                    };
                    let tint = quad.tint_index.map_or(255u8, |t| t as u8);
                    let tint_rgb_override = view.biome_tint_at(x, y, z, tint);
                    // Vanilla's per-quad `ChunkSectionLayer` bucketing
                    // (`SectionCompiler`'s `quadOutput` reads
                    // `quad.materialInfo().layer()`), resolved once here and
                    // used for both halves of what that layer decides: which
                    // mesh the quad lands in, and whether the alpha test runs.
                    //
                    // A `Solid` quad bypasses the cutout discard because
                    // vanilla's `SOLID_TERRAIN` pipeline defines no
                    // `ALPHA_CUTOUT` and so runs no test at all — the block's
                    // *other* faces being cutout does not change that. This is
                    // what keeps a `grass_block`'s opaque top out of an alpha
                    // test whose mip-filtered result can dip below the
                    // threshold at a sprite edge under minification.
                    //
                    // `None` (a view that cannot classify quads: every test
                    // fixture, the GUI item baker) keeps the pre-per-quad
                    // behaviour — one mesh, discard on.
                    let quad_layer = view.quad_layer(x, y, z, quad);
                    let target: &mut ModelMesh = if quad_layer == Some(RenderLayer::Translucent) {
                        &mut translucent_mesh
                    } else {
                        &mut mesh
                    };
                    emit_baked_quad(
                        target,
                        quad,
                        [x as f32, y as f32, z as f32],
                        corners,
                        tint_rgb_override,
                        force_opaque || quad_layer == Some(RenderLayer::Solid),
                    );
                }
            }
        }
    }
    (mesh, translucent_mesh)
}

/// Directional shading, matching vanilla's constant per-face factors so a shaded
/// quad reads correctly even before smooth lighting is applied. A quad with
/// `shade: false` (fluids, full-bright faces) is unshaded.
///
/// Shared with the GUI item path ([`mesh_item_quads`]), whose `gui_light: side`
/// items — 734 of the 753 model items in 26.2 — are lit by exactly these
/// constants.
fn face_shade(quad: &BakedQuad) -> f32 {
    if quad.shade {
        match quad.direction {
            Direction::Up => 1.0,
            Direction::Down => 0.5,
            Direction::North | Direction::South => 0.8,
            Direction::East | Direction::West => 0.6,
        }
    } else {
        1.0
    }
}

/// Emit one quad's four vertices, `corners[i]` supplying vertex `i`'s
/// `(ao_factor, light)` from [`quad_corner_sample`] — or a uniform
/// `(1.0, light)` per vertex for callers that skip smooth lighting (fluids).
/// The AO factor multiplies into the constant per-face directional shade, so
/// a flat caller (`ao_factor` always `1.0`) reproduces the pre-smoothing
/// behaviour exactly.
///
/// `tint_rgb_override` is the *real*, position-resolved colour for a
/// biome-dependent quad ([`ModelSectionView::biome_tint_at`]/
/// [`FluidSectionView::water_tint_at`]), or `None` to keep reading the
/// frame-shared palette at `quad.tint_index` as before — see
/// [`ModelVertex::tint_rgb_override`]'s doc for why these are two different
/// mechanisms rather than one.
///
/// `force_opaque` stamps [`ModelVertex::cutout_bypass`] on all four vertices,
/// i.e. "this quad draws through a pass that runs no alpha test". Two
/// independent things ask for it and both are vanilla: the FAST-leaves preset,
/// which routes a whole leaf block to `SOLID_TERRAIN`
/// (`ModelBlockRenderer.forceOpaque`), and a quad whose own sprite is
/// [`RenderLayer::Solid`], which lands on `SOLID_TERRAIN` for the ordinary
/// reason that its material says so.
fn emit_baked_quad(
    mesh: &mut ModelMesh,
    quad: &BakedQuad,
    origin: [f32; 3],
    corners: [(f32, u8); 4],
    tint_rgb_override: Option<[u8; 3]>,
    force_opaque: bool,
) {
    let base = mesh.vertices.len() as u32;
    let shade = face_shade(quad);
    let tint = quad.tint_index.map_or(255u8, |t| t as u8);
    let tint_rgb_override = match tint_rgb_override {
        Some([r, g, b]) => [r, g, b, 255],
        None => [0, 0, 0, 0],
    };
    let cutout_bypass = u8::from(force_opaque);
    for i in 0..4 {
        let p = quad.positions[i];
        let (ao_factor, light) = corners[i];
        mesh.vertices.push(ModelVertex {
            position: [origin[0] + p[0], origin[1] + p[1], origin[2] + p[2]],
            uv: quad.uvs[i],
            ao: shade * ao_factor,
            light,
            tint,
            anim: quad.anim,
            cutout_bypass,
            tint_rgb_override,
        });
    }
    // Flip the triangulation diagonal when AO disagrees across it, so the
    // interpolated darkening stays symmetric (mirrors `crate::mesh::emit_quad`).
    let d02 = corners[0].0 + corners[2].0;
    let d13 = corners[1].0 + corners[3].0;
    if d02 > d13 {
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base + 2, base + 3, base]);
    } else {
        mesh.indices.extend_from_slice(&[
            base + 1,
            base + 2,
            base + 3,
            base + 3,
            base,
            base + 1,
        ]);
    }
}

/// The packed light byte a GUI item vertex carries: sky `15`, block `0`. The
/// model shader's light term ([`crate::light`]) evaluates to exactly `1.0` at
/// full light — `get_brightness(1) = 1` and `notGamma(1) = 1` — so a GUI item is
/// full-bright regardless of where the player is standing, which is what an
/// inventory slot is. That exactness is also why replacing the old linear ramp
/// with vanilla's curve left every GUI-item gate byte-identical.
pub const GUI_ITEM_LIGHT: u8 = 0xF0;

/// Mesh one item's baked geometry into a GUI-ready [`ModelMesh`], posed by
/// `pose` (build it with [`gui_item_pose`](crate::gui_item_pose)).
///
/// Deliberately **not** [`mesh_models`] with a one-block view, because three of
/// that function's rules are wrong for an inventory slot:
///
/// * **`cullface` is ignored.** A slot has no neighbours, so a quad culled
///   against "the block to the north" would vanish for no reason. Every quad is
///   emitted; the pipeline's back-face culling is what removes the far faces.
/// * **Positions are transformed, not offset.** `pose` carries the model's
///   `display.gui` transform and the slot placement, so the emitted positions
///   are already in GUI pixel space and only need
///   [`gui_ortho`](crate::gui_ortho) to reach clip space.
/// * **Light is fixed** at [`GUI_ITEM_LIGHT`].
///
/// `gui_light` rides in the per-vertex `ao` slot, which the shader multiplies
/// into the light term: [`GuiLight::Side`] keeps the per-face directional
/// constants (so the isometric cube reads as three differently-lit faces), while
/// [`GuiLight::Front`] flattens every face to `1.0` — vanilla's flat, front-lit
/// mode for models that are really a sprite standing up.
#[must_use]
pub fn mesh_item_quads(quads: &[BakedQuad], pose: Mat4, gui_light: GuiLight) -> ModelMesh {
    let mut mesh = ModelMesh::default();
    for quad in quads {
        let base = mesh.vertices.len() as u32;
        let shade = match gui_light {
            GuiLight::Side => face_shade(quad),
            GuiLight::Front => 1.0,
        };
        let tint = quad.tint_index.map_or(255u8, |t| t as u8);
        for i in 0..4 {
            let p = pose.transform_point3(Vec3::from(quad.positions[i]));
            mesh.vertices.push(ModelVertex {
                position: p.into(),
                uv: quad.uvs[i],
                ao: shade,
                light: GUI_ITEM_LIGHT,
                tint,
                anim: quad.anim,
                cutout_bypass: 0,
                // GUI/inventory icons always render vanilla's fixed default
                // tint (never the world biome the player happens to stand
                // in — an item in your hotbar does not change colour), so
                // this path never has an override to carry.
                tint_rgb_override: [0, 0, 0, 0],
            });
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    mesh
}

/// Mesh one **block state's** baked geometry into a world-space [`ModelMesh`],
/// posed by `pose` and lit by a single packed `light` byte — vanilla's
/// `SubmitNodeCollector.submitMovingBlock`, the path a block model takes when it
/// is drawn somewhere other than its own cell.
///
/// Two consumers, and the second is why this is a named seam rather than a
/// falling-block special case:
///
/// * `FallingBlockRenderer.submit`, whose whole body is
///   `poseStack.translate(-0.5, 0, -0.5)` then `submitMovingBlock`.
/// * `PistonHeadRenderer`, which draws the head and the pushed block the same
///   way. Like the campfire, it bakes no layer and owns no cuboid rig, so it
///   needs this rather than [`EntityPipeline`](crate::EntityPipeline).
///
/// # How this differs from [`mesh_item_quads`], and from [`mesh_models`]
///
/// | | [`mesh_models`] (terrain) | [`mesh_item_quads`] (GUI) | this |
/// |---|---|---|---|
/// | `cullface` | honoured against neighbours | ignored | **ignored** |
/// | positions | offset by cell | transformed by `pose` | transformed by `pose` |
/// | shade | per-face + smooth AO | per-face, or flat for `gui_light: front` | **per-face, always** |
/// | light | sampled per corner | fixed [`GUI_ITEM_LIGHT`] | **one supplied byte** |
///
/// `cullface` is ignored for [`mesh_item_quads`]'s reason: a moving block has no
/// neighbours to cull against, and a quad dropped because "the block to the north
/// occludes it" would leave a hole in mid-air. The pipeline's own back-face
/// culling removes the far faces.
///
/// Shade is **always** the per-face directional constant, with no `GuiLight`
/// branch: `submitMovingBlock` goes through the block renderer, which has no
/// notion of a model's `gui_light`. Flattening it would make a falling sand block
/// read as a uniformly-lit cube, which looks like a lighting bug rather than a
/// pose bug.
///
/// `light` is one byte for the whole mesh because that is what vanilla does —
/// `MovingBlockRenderState` carries a single `blockPos` and the light is sampled
/// there once, not per corner. Sampling per corner would need neighbour data this
/// path does not have.
#[must_use]
pub fn mesh_moving_block_quads(quads: &[BakedQuad], pose: Mat4, light: u8) -> ModelMesh {
    let mut mesh = ModelMesh::default();
    for quad in quads {
        let base = mesh.vertices.len() as u32;
        let shade = face_shade(quad);
        let tint = quad.tint_index.map_or(255u8, |t| t as u8);
        for i in 0..4 {
            let p = pose.transform_point3(Vec3::from(quad.positions[i]));
            mesh.vertices.push(ModelVertex {
                position: p.into(),
                uv: quad.uvs[i],
                ao: shade,
                light,
                tint,
                anim: quad.anim,
                cutout_bypass: 0,
                // No biome override: `MovingBlockRenderState` does carry a
                // `biome`, but resolving it needs the position-keyed biome grid
                // the terrain mesher owns and this path has no access to. The
                // frame-shared palette at `quad.tint_index` is what a
                // non-biome-tinted block (every state this draws today — sand,
                // red sand, gravel are untinted) resolves to anyway, so the
                // deviation is currently invisible. A moving *grass block* would
                // show it as the default green rather than the local biome's.
                tint_rgb_override: [0, 0, 0, 0],
            });
        }
        mesh.indices
            .extend_from_slice(&[base, base + 1, base + 2, base, base + 2, base + 3]);
    }
    mesh
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
    /// Whether the block at `(x, y, z)` is a `HalfTransparentBlock` or
    /// `LeavesBlock` in vanilla terms (glass, ice, honey, slime, tinted glass,
    /// leaves) — the neighbour class `FluidRenderer.tesselate` checks to swap a
    /// touching fluid side face onto the `water_overlay` material and suppress
    /// its back copy.
    ///
    /// Defaults to `false` everywhere, which reproduces the pre-overlay
    /// behaviour exactly (every side face uses `*_flow` with a back face) —
    /// existing [`FluidSectionView`] implementations need no change to keep
    /// compiling. See `docs/fluid-rendering.md`'s "Known gaps" for the concrete
    /// live-shell patch that overrides this from real block classification.
    fn overlay_at(&self, x: i32, y: i32, z: i32) -> bool {
        let _ = (x, y, z);
        false
    }

    /// The **real**, position-resolved water colour at `(x, y, z)`, or `None`
    /// to fall back to `fluid.wgsl`'s hardcoded `#3F76E4` default — exactly
    /// today's behaviour. Only ever consulted for a water cell (lava is never
    /// tinted); see [`ModelSectionView::biome_tint_at`]'s doc for why this is
    /// a vertex override rather than a palette lookup — the fluid pipeline
    /// has no palette bind group at all (`ModelPipeline::for_fluid`), so it
    /// was always going to need its own colour path.
    ///
    /// Defaults to `None`, so every existing view (headless tests, a demo
    /// world with no biome grid) keeps rendering the exact fixed water colour
    /// it always has.
    fn water_tint_at(&self, x: i32, y: i32, z: i32) -> Option<[u8; 3]> {
        let _ = (x, y, z);
        None
    }

    /// The neighbour's outline shape at `(x, y, z)`, reduced to the one case
    /// this crate can evaluate exactly: a single box spanning the full `x`/`z`
    /// footprint of its cell — `dirt_path`, `farmland`, slabs, snow layers,
    /// and every other "flat, height-only-reduced" shape. Returns its
    /// `(min_y, max_y)` in block-local `0.0..=1.0`, from
    /// [`lodestone_assets::fluid::full_footprint_y_range`].
    ///
    /// This is the still-open half of `FluidRenderer.isFaceOccludedByState`'s
    /// three-way branch (`Shapes.java:244`'s `blockOccludes`/`getFaceShape`):
    /// [`occludes_at`](Self::occludes_at) already handles the `Shapes.block()`
    /// fast path (a genuinely full, opaque cube) and `Shapes.empty()` (nothing
    /// occludes); this covers the `else` branch's *scoped* subset. A shape
    /// with holes, steps or a partial footprint (stairs, fences, walls,
    /// multi-box shapes) is out of scope — see `docs/fluid-rendering.md`'s
    /// "Known gaps" — and should answer `None` here exactly as air would,
    /// falling back to today's boolean-only culling.
    ///
    /// Defaults to `None` everywhere, which reproduces the pre-fix behaviour
    /// exactly (a partial occluder never culls a fluid side face) — existing
    /// [`FluidSectionView`] implementations need no change to keep compiling.
    fn partial_occluder_y_range_at(&self, x: i32, y: i32, z: i32) -> Option<(f32, f32)> {
        let _ = (x, y, z);
        None
    }

    /// Which of the fluid cell's own faces the block **sharing that cell**
    /// already covers — vanilla `FluidRenderer.isFaceOccludedBySelf`, the other
    /// half of `shouldRenderFace`.
    ///
    /// This asks about `(x, y, z)` *itself*, never a neighbour, which is exactly
    /// why it needs its own method: every other query on this trait is about the
    /// neighbourhood, and answering this one from the neighbourhood is the bug it
    /// exists to fix. A waterlogged stair shares one cell with its water, so the
    /// water's face on the stair's solid side is coplanar with the stair's own
    /// face and the two z-fight; vanilla culls the face rather than insetting the
    /// water.
    ///
    /// Consulted once per fluid cell (not per neighbour), so it is a live call on
    /// the view rather than a [`FluidGrid`] field. Defaults to all-`false`, which
    /// reproduces the pre-fix behaviour exactly — the same compatibility shape
    /// [`overlay_at`](Self::overlay_at) and
    /// [`partial_occluder_y_range_at`](Self::partial_occluder_y_range_at) use.
    ///
    /// An implementation should hand
    /// [`lodestone_assets::fluid::self_occlusion`] the state's **outline** boxes,
    /// and must apply vanilla's `canOcclude` gate itself: see that function's
    /// doc for why a waterlogged leaves block otherwise culls all of its own
    /// water away.
    fn self_occlusion_at(&self, x: i32, y: i32, z: i32) -> SelfOcclusion {
        let _ = (x, y, z);
        SelfOcclusion::default()
    }

    /// [`fluid_at`](Self::fluid_at), [`occludes_at`](Self::occludes_at) and
    /// [`overlay_at`](Self::overlay_at) for one cell, **in a single call** —
    /// the fill primitive for [`crate::fluid_grid::FluidGrid`].
    ///
    /// The default composes the three, so every existing implementor keeps
    /// compiling and answers identically. Override it when the three share
    /// work: `SnapshotFluidView`'s three accessors each redo the same three
    /// `split16`s, the same snapshot-slot index and the same
    /// `PalettedContainer::get` bit-unpack, so folding them into one call
    /// removes two thirds of the fill's cost. See `docs/fluid-rendering.md`.
    ///
    /// Deliberately *not* including
    /// [`partial_occluder_y_range_at`](Self::partial_occluder_y_range_at):
    /// it does not pack into the grid's 16 bits and is consulted at most four
    /// times per *surface* cell, so it stays a live call.
    fn cell_at(&self, x: i32, y: i32, z: i32) -> FluidNeighborCell {
        FluidNeighborCell {
            fluid: self.fluid_at(x, y, z),
            occludes: self.occludes_at(x, y, z),
            overlay: self.overlay_at(x, y, z),
        }
    }
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
/// `kind_bits` being baked: its own height if it is the same fluid (snapped to
/// `1.0` when that same fluid continues above), `-1.0` if it is a solid block
/// (excluded from the average) or `0.0` if it is air-like.
///
/// Reads the precomputed [`FluidGrid`] rather than the view. `kind_bits` is the
/// centre cell's [`PackedCell::kind_bits`](crate::fluid_grid::PackedCell::kind_bits),
/// which is never [`KIND_NONE`](crate::fluid_grid::KIND_NONE), so
/// `c.kind_bits() == kind_bits` is exactly the old `Some(f) if f.kind == kind`
/// — a fluid-free cell can never match it.
#[inline]
fn neighbor_height_in(grid: &FluidGrid, kind_bits: u16, x: i32, y: i32, z: i32) -> f32 {
    let c = grid.get(x, y, z);
    if c.kind_bits() == kind_bits {
        let above_same = grid.get(x, y + 1, z).kind_bits() == kind_bits;
        neighbor_height(true, above_same, c.own_height(), false)
    } else {
        neighbor_height(false, false, 0.0, c.occludes())
    }
}

/// The [`FlowNeighbor`] describing the cell one step `(dx, dz)` from `(x, y, z)`.
#[inline]
fn flow_neighbor_in(
    grid: &FluidGrid,
    kind_bits: u16,
    x: i32,
    y: i32,
    z: i32,
    dx: i32,
    dz: i32,
) -> FlowNeighbor {
    let (nx, nz) = (x + dx, z + dz);
    let here = grid.get(nx, y, nz);
    let below = grid.get(nx, y - 1, nz);
    FlowNeighbor {
        own_height: if here.kind_bits() == kind_bits {
            here.own_height()
        } else {
            0.0
        },
        blocks_motion: here.occludes(),
        below_own_height: if below.kind_bits() == kind_bits {
            below.own_height()
        } else {
            0.0
        },
    }
}

/// Vanilla `FluidState.shouldRenderBackwardUpFace`: whether the fluid's top
/// surface needs a reversed back copy so it stays visible when seen from
/// above, e.g. through the rim gap where the surface dips below a solid
/// ceiling. True when any cell in the 3×3 neighbourhood **directly above** the
/// fluid (`y + 1`, matching vanilla's `above.offset(ox, 0, oz)`) carries a
/// *different* fluid (or none) over a cell that doesn't fully occlude.
///
/// `occludes_at` stands in for vanilla's `isSolidRender` — they agree for a
/// plain opaque cube (the dominant case) and this mirrors the same
/// approximation `mesh_fluids` already makes for `blocks_motion`/`isSolid` in
/// [`flow_neighbor_in`]; see `docs/fluid-rendering.md`.
#[inline]
fn should_render_backward_up_face_in(
    grid: &FluidGrid,
    kind_bits: u16,
    x: i32,
    y: i32,
    z: i32,
) -> bool {
    let above_y = y + 1;
    for oz in -1..=1 {
        for ox in -1..=1 {
            let c = grid.get(x + ox, above_y, z + oz);
            if c.kind_bits() != kind_bits && !c.occludes() {
                return true;
            }
        }
    }
    false
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
///
/// The **up** face is *not* culled just because the block above occludes: per
/// `isFaceOccludedByState`'s `direction != UP || height == 1.0` short-circuit,
/// a full solid neighbour only culls the top surface when every corner height
/// is already `1.0` (a same-fluid column stacked one cell short of the ceiling)
/// — which is why water under a solid block still draws its surface into the
/// `1/9`-block gap.
///
/// Every face except **up** additionally goes through `shouldRenderFace`'s *self*
/// half, [`FluidSectionView::self_occlusion_at`]: the block sharing the fluid's
/// own cell. That is what stops a waterlogged stair's water emitting a face
/// coplanar with the stair's own solid side.
///
/// A **side** face additionally checks [`FluidSectionView::partial_occluder_y_range_at`]
/// against `max` of the two corners on that edge — `dirt_path`/`farmland` banks
/// and other full-footprint, height-reduced neighbours now cull the way vanilla's
/// `blockOccludes` general branch does for that scoped shape family. See
/// `docs/fluid-rendering.md`'s "Known gaps" for what is still unmodelled (shapes
/// with holes, steps, or a partial footprint — stairs, fences, walls).
///
/// # Why this is generic rather than `&dyn`
///
/// Every accessor below is called on the order of **fifty times per fluid
/// cell** (the `nh` corner probes alone are twelve `neighbor_height_at`s, each
/// two `fluid_at`s), and through a trait object none of them can inline: each
/// pays an indirect call plus a full re-decode of the coordinate. Taking `V` by
/// value-type lets the whole neighbourhood walk inline into one loop body. The
/// `?Sized` bound keeps `mesh_fluids(&dyn FluidSectionView)` compiling for any
/// caller that genuinely wants the dynamic form (`dyn Trait: Trait` holds), so
/// this is source-compatible — it just stops being the *default*.
///
/// Note this was the *second* axis issue #542 proposed, on the stated grounds
/// that "`mesh_models` is generic and inlines". **That premise is false** —
/// `mesh_models` above is `&dyn` too. The conclusion survives (measured 9.5% of
/// the term, `DESIGN.md` §12.124); the reasoning behind it did not.
#[must_use]
pub fn mesh_fluids<V: FluidSectionView + ?Sized>(view: &V) -> FluidMeshes {
    let mut out = FluidMeshes::default();
    // The whole neighbourhood, resolved once. Every probe below is an index
    // into this instead of a call back through `view` — see
    // `crate::fluid_grid`. `any_fluid` is also the "contains no fluid"
    // precheck, free as a by-product of the fill.
    let grid = FluidGrid::build(view);
    if !grid.any_fluid() {
        return out;
    }
    // `fluid_sprites` is a function of the *kind*, so the old per-cell call was
    // thousands of identical lookups per section. Memoised per kind rather than
    // hoisted unconditionally, so a view is still never asked about a fluid its
    // section does not contain.
    let mut water_sprites: Option<FluidSprites> = None;
    let mut lava_sprites: Option<FluidSprites> = None;
    let n = SECTION_SIZE;
    for y in 0..n {
        for z in 0..n {
            for x in 0..n {
                let (xi, yi, zi) = (x as i32, y as i32, z as i32);
                let cell = grid.get(xi, yi, zi);
                let Some(fc) = cell.fluid() else {
                    continue;
                };
                let kind = fc.kind;
                let kb = cell.kind_bits();
                let self_h = neighbor_height_in(&grid, kb, xi, yi, zi);
                let nh = |dx: i32, dz: i32| neighbor_height_in(&grid, kb, xi + dx, yi, zi + dz);
                // `[NW, NE, SE, SW]`. This was four bare `corner_height` calls,
                // which is `calculateAverageHeight` without the branch above it:
                // `FluidRenderer.tesselate` sets every corner to `1.0` when the
                // fluid's own rendered height already is, and only averages
                // otherwise. A falling column has the same fluid above every cell,
                // so it takes the short-circuit in vanilla and was taking the
                // average here — `10 / 12` against the surrounding air. See
                // `corner_heights` for the measurement and why it looked like a
                // triangle rather than a band.
                let corners = corner_heights(
                    self_h,
                    nh(0, -1),
                    nh(0, 1),
                    nh(1, 0),
                    nh(-1, 0),
                    nh(-1, -1),
                    nh(1, -1),
                    nh(1, 1),
                    nh(-1, 1),
                );
                let flow = flow_horizontal(
                    fc.state.own_height(),
                    flow_neighbor_in(&grid, kb, xi, yi, zi, 0, -1),
                    flow_neighbor_in(&grid, kb, xi, yi, zi, 0, 1),
                    flow_neighbor_in(&grid, kb, xi, yi, zi, 1, 0),
                    flow_neighbor_in(&grid, kb, xi, yi, zi, -1, 0),
                );

                let same =
                    |dx: i32, dy: i32, dz: i32| grid.get(xi + dx, yi + dy, zi + dz).kind_bits() == kb;
                let emit = |dx: i32, dy: i32, dz: i32| {
                    let c = grid.get(xi + dx, yi + dy, zi + dz);
                    c.kind_bits() != kb && !c.occludes()
                };
                // Vanilla's `isFaceOccludedByNeighbor(UP, min(corners), aboveState)`
                // only culls the top face when the *fully occluding* fast path
                // (`Shapes.block()`) also has `height == 1.0`, which needs every
                // corner at a full column — not merely a solid block above.
                let up_occluded =
                    grid.get(xi, yi + 1, zi).occludes() && corners.iter().all(|&h| h >= 1.0);
                // A side face's `isFaceOccludedByNeighbor(dir, max(h0, h1), state)`
                // general branch (`Shapes.blockOccludes`, non-full-cube occluder):
                // for the scoped single-box, full-`x`/`z`-footprint case this
                // collapses to "does the occluder's own y-range fully cover
                // `[0, face_height]`?" — the same derivation
                // `docs/fluid-rendering.md`'s "Known gaps" walks through. `corners`
                // is `[NW, NE, SE, SW]`.
                // `partial_occluder_y_range_at` stays a live call on the view:
                // it returns two `f32`s (so it does not pack into the grid) and
                // it is only reached for a *surface* cell's non-occluding side
                // neighbour, a few percent of an ocean section — see
                // `crate::fluid_grid`'s module docs.
                let side_occluded = |dx: i32, dz: i32, face_height: f32| {
                    grid.get(xi + dx, yi, zi + dz).occludes()
                        || view
                            .partial_occluder_y_range_at(xi + dx, yi, zi + dz)
                            .is_some_and(|(min_y, max_y)| {
                                min_y <= 1e-4 && max_y + 1e-4 >= face_height
                            })
                };
                // `shouldRenderFace`'s *self* half: the block sharing this cell.
                // Note vanilla applies it to `down` and the four sides but **not**
                // to `up` — `renderUp` is bare `!isNeighborSameFluid`, the one
                // face that skips `shouldRenderFace` entirely. See
                // `SelfOcclusion`.
                let self_occ = view.self_occlusion_at(xi, yi, zi);
                let faces = FaceSet {
                    up: !same(0, 1, 0) && !up_occluded,
                    down: emit(0, -1, 0) && !self_occ.down,
                    north: !same(0, 0, -1)
                        && !self_occ.north
                        && !side_occluded(0, -1, corners[0].max(corners[1])),
                    south: !same(0, 0, 1)
                        && !self_occ.south
                        && !side_occluded(0, 1, corners[2].max(corners[3])),
                    east: !same(1, 0, 0)
                        && !self_occ.east
                        && !side_occluded(1, 0, corners[1].max(corners[2])),
                    west: !same(-1, 0, 0)
                        && !self_occ.west
                        && !side_occluded(-1, 0, corners[0].max(corners[3])),
                };

                let tint_index = match kind {
                    FluidKind::Water => Some(0),
                    FluidKind::Lava => None,
                };
                let sprites = *match kind {
                    FluidKind::Water => water_sprites
                        .get_or_insert_with(|| view.fluid_sprites(FluidKind::Water)),
                    FluidKind::Lava => {
                        lava_sprites.get_or_insert_with(|| view.fluid_sprites(FluidKind::Lava))
                    }
                };
                let side_overlay = SideOverlay {
                    north: faces.north && grid.get(xi, yi, zi - 1).overlay(),
                    south: faces.south && grid.get(xi, yi, zi + 1).overlay(),
                    east: faces.east && grid.get(xi + 1, yi, zi).overlay(),
                    west: faces.west && grid.get(xi - 1, yi, zi).overlay(),
                };
                let back_up_face =
                    faces.up && should_render_backward_up_face_in(&grid, kb, xi, yi, zi);
                let geom = FluidGeometry {
                    corners,
                    flow,
                    faces,
                    tint_index,
                    back_up_face,
                    side_overlay,
                };
                let quads = bake_fluid(&geom, sprites.still, sprites.flow, sprites.overlay);

                let (mesh, light, tint_rgb_override) = match kind {
                    FluidKind::Water => (
                        &mut out.water,
                        view.light_at(x, y, z),
                        view.water_tint_at(xi, yi, zi),
                    ),
                    // Lava is never tinted (`tint_index` above is `None` for
                    // it), so it never has an override to look up either.
                    FluidKind::Lava => (&mut out.lava, 0xFF, None),
                };
                // Fluids stay flat by design: uniform light, no AO, per the
                // module docs on `mesh_fluids`.
                let corners = [(1.0, light); 4];
                for quad in &quads {
                    // Fluids never carry `force_opaque`: `cutoutLeaves` is a
                    // leaves-only option, and water/lava are `Translucent`
                    // geometry the model shader's cutout discard never
                    // touches in the first place.
                    emit_baked_quad(
                        mesh,
                        quad,
                        [x as f32, y as f32, z as f32],
                        corners,
                        tint_rgb_override,
                        false,
                    );
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
        // 32, not 28: `tint_rgb_override` (a real per-position biome colour,
        // additive to the existing four attributes) grew the stride by one
        // more `Uint8x4`.
        assert_eq!(MODEL_BYTES_PER_VERTEX, 32);
        assert_eq!(crate::vertex::BYTES_PER_VERTEX, 12);
        // Per quad, including u32 indices: packed 72 B, wide 152 B ≈ 2.1×.
        assert_eq!(crate::vertex::vram_bytes(1), 72);
        assert_eq!(model_vram_bytes(1), 152);
        // Collapsing the dominant cube geometry to the wide format would more
        // than double its VRAM/bandwidth — the cost the split buys back.
        assert!(model_vram_bytes(1_000) > crate::vertex::vram_bytes(1_000) * 2);
    }

    fn cube_face(dir: Direction, cull: Option<Direction>) -> BakedQuad {
        // A degenerate quad — all four "corners" collapsed to one point — so
        // the exact in-plane shape stays irrelevant to the culling logic under
        // test (and every AO-ring test built on this fixture keeps sampling
        // one shared corner). But that single point now sits on `dir`'s own
        // block-boundary plane, because `quad_is_on_face_boundary` (used by
        // `mesh_models` to pick `faceCubic`) reads real positions: a point
        // left at the origin looks like a face flush with `dir`'s plane only
        // for `Down`/`North`/`West` and silently fails the boundary test for
        // `Up`/`South`/`East`, which would route light through the wrong
        // branch for those tests.
        let (fixed, plane) = face_plane(dir);
        let mut p = [0.0f32; 3];
        p[fixed] = plane;
        BakedQuad {
            positions: [p; 4],
            uvs: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            direction: dir,
            cullface: cull,
            tint_index: None,
            shade: true,
            layer: 0,
            anim: 0,
            sprite: 0,
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

    /// A block at `(8, 8, 8)` returning `quads_a`, and one at `(9, 8, 8)`
    /// returning `quads_b`, with `(9, 8, 8)`'s [`quad_layer`] answer fixed by
    /// `b_translucent` and `(8, 8, 8)`'s left at `Cutout` so the two blocks
    /// differ only in the property under test.
    ///
    /// [`quad_layer`]: ModelSectionView::quad_layer
    struct TwoBlocks {
        quads_a: Vec<BakedQuad>,
        quads_b: Vec<BakedQuad>,
        b_translucent: bool,
    }
    impl ModelSectionView for TwoBlocks {
        fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
            match (x, y, z) {
                (8, 8, 8) => &self.quads_a,
                (9, 8, 8) => &self.quads_b,
                _ => &[],
            }
        }
        fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
            false
        }
        fn quad_layer(
            &self,
            x: usize,
            y: usize,
            z: usize,
            _quad: &BakedQuad,
        ) -> Option<RenderLayer> {
            if (x, y, z) == (9, 8, 8) && self.b_translucent {
                Some(RenderLayer::Translucent)
            } else {
                Some(RenderLayer::Cutout)
            }
        }
    }

    /// The regression gate for the portal-opacity fix: `mesh_models_layers`
    /// must keep a [`ModelSectionView::quad_layer`]-`Translucent` quad out
    /// of the first (opaque/cutout) mesh and put it in the second, while an
    /// ordinary block's quads stay in the first — and `mesh_models` (the
    /// merged form existing callers use) must still carry both blocks'
    /// geometry either way, so this split cannot silently drop a quad.
    #[test]
    fn mesh_models_layers_routes_translucent_blocks_to_the_second_mesh() {
        let view = TwoBlocks {
            quads_a: vec![cube_face(Direction::Up, None)],
            quads_b: vec![cube_face(Direction::Up, None)],
            b_translucent: true,
        };
        let (opaque, translucent) = mesh_models_layers(&view);
        assert_eq!(
            opaque.quad_count(),
            1,
            "the non-translucent block's quad must be the only one in the opaque mesh"
        );
        assert_eq!(
            translucent.quad_count(),
            1,
            "the translucent block's quad must land in the second mesh, not be dropped"
        );

        // Control: with both blocks non-translucent, every quad stays in the
        // first mesh and the second is empty — proving the split is driven by
        // `quad_layer`, not by something incidental to having two blocks.
        let both_opaque_view = TwoBlocks {
            quads_a: vec![cube_face(Direction::Up, None)],
            quads_b: vec![cube_face(Direction::Up, None)],
            b_translucent: false,
        };
        let (opaque2, translucent2) = mesh_models_layers(&both_opaque_view);
        assert_eq!(opaque2.quad_count(), 2);
        assert_eq!(translucent2.quad_count(), 0);

        // `mesh_models` (the merged, backward-compatible form) must carry
        // every quad from both blocks regardless of the split — a caller that
        // only wants combined geometry must see no change.
        let merged = mesh_models(&view);
        assert_eq!(
            merged.quad_count(),
            2,
            "mesh_models must merge both meshes, dropping nothing"
        );
    }

    /// [`mesh_models`] must ask for light **per quad**, keyed on that quad's
    /// facing, not once per block.
    ///
    /// This is the render half of the "placed blocks are super bright" fix. A
    /// per-block light forces every face of a block to carry the block's own
    /// cell value, which the light engine stores as `0` inside any opaque solid
    /// — so terrain meshes uniformly dark while a just-placed block, whose cell
    /// still holds the sky light of the air it replaced, meshes full-bright.
    /// Reading per face lets the consumer hand back the neighbouring cell's
    /// light, which is what vanilla's `ModelBlockRenderer` does.
    #[test]
    fn mesh_models_asks_for_light_per_quad_facing() {
        use std::cell::RefCell;

        /// Answers a distinct light per direction, and records what was asked.
        struct PerFace {
            quads: Vec<BakedQuad>,
            asked: RefCell<Vec<(usize, usize, usize, Direction)>>,
        }
        impl ModelSectionView for PerFace {
            fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
                if (x, y, z) == (8, 8, 8) { &self.quads } else { &[] }
            }
            // Every AO corner neighbour occludes, and `corner_light_at` below
            // returns `0x00` — so `quad_corner_sample`'s `smoothBlend`
            // substitution replaces every occluded, above-threshold nibble
            // with the centre's own value, and the below-threshold nibble
            // falls back to the (also `0x00`) raw sample. Either way each
            // corner's blended light reproduces `centre_light` exactly, so
            // this keeps testing the thing it existed to test — light is
            // asked **per quad facing** — without smooth lighting's
            // per-vertex variation getting in the way.
            fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
                true
            }
            fn corner_light_at(&self, _x: i32, _y: i32, _z: i32) -> u8 {
                0x00
            }
            fn light_at(&self, _x: usize, _y: usize, _z: usize) -> u8 {
                // The own-cell answer. If `mesh_models` ever falls back to this,
                // every face below carries 0x00 and the assertions fail.
                0x00
            }
            fn face_light_at(&self, x: usize, y: usize, z: usize, dir: Direction) -> u8 {
                self.asked.borrow_mut().push((x, y, z, dir));
                match dir {
                    Direction::Up => 0xF0,
                    Direction::Down => 0x00,
                    _ => 0x0B,
                }
            }
        }

        let view = PerFace {
            quads: vec![
                cube_face(Direction::Up, None),
                cube_face(Direction::Down, None),
                cube_face(Direction::North, None),
            ],
            asked: RefCell::new(Vec::new()),
        };
        let mesh = mesh_models(&view);

        // Three quads, four vertices each, in emission order.
        assert_eq!(mesh.vertices.len(), 12);
        let per_quad: Vec<u8> = mesh.vertices.chunks_exact(4).map(|q| q[0].light).collect();
        assert_eq!(
            per_quad,
            vec![0xF0, 0x00, 0x0B],
            "each quad must carry the light of its own facing; a per-block light \
             would make all three equal"
        );
        for quad in mesh.vertices.chunks_exact(4) {
            assert!(quad.iter().all(|v| v.light == quad[0].light));
        }
        assert_eq!(
            *view.asked.borrow(),
            vec![
                (8, 8, 8, Direction::Up),
                (8, 8, 8, Direction::Down),
                (8, 8, 8, Direction::North),
            ],
            "the mesher must query the cell and the quad's facing, once per quad"
        );
    }

    /// The default [`ModelSectionView::face_light_at`] falls back to
    /// [`ModelSectionView::light_at`], so a view that models no neighbourhood
    /// (GUI items, fixtures) keeps its previous behaviour.
    #[test]
    fn face_light_defaults_to_the_own_cell_light() {
        struct OwnOnly;
        impl ModelSectionView for OwnOnly {
            fn quads_at(&self, _x: usize, _y: usize, _z: usize) -> &[BakedQuad] {
                &[]
            }
            fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
                false
            }
            fn light_at(&self, _x: usize, _y: usize, _z: usize) -> u8 {
                0x7C
            }
        }
        assert_eq!(OwnOnly.face_light_at(1, 2, 3, Direction::East), 0x7C);
        // And the trait's own default (no `light_at` override) is still 0xF0.
        struct Bare;
        impl ModelSectionView for Bare {
            fn quads_at(&self, _x: usize, _y: usize, _z: usize) -> &[BakedQuad] {
                &[]
            }
            fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
                false
            }
        }
        assert_eq!(Bare.face_light_at(0, 0, 0, Direction::Up), 0xF0);
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
            anim: 0,
            sprite: 0,
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
        assert!(is_packed_cube(&cube, RenderLayer::Solid));
    }

    #[test]
    fn cutout_or_translucent_full_cube_is_not_packed() {
        // Untinted, geometrically a full cube — exactly what an alpha-blind
        // predicate would wrongly accept. `is_packed_cube` must reject it once
        // the real layer says otherwise, because the packed block shader has
        // no discard and no blending: it draws depth-written and opaque
        // unconditionally.
        let cube = full_cube();
        assert!(is_full_cube(&cube));
        assert!(
            !is_packed_cube(&cube, RenderLayer::Cutout),
            "a cutout full cube (leaves-shaped alpha on a cube, e.g. plain glass) \
             must not take the opaque-only packed path"
        );
        assert!(
            !is_packed_cube(&cube, RenderLayer::Translucent),
            "a translucent full cube (stained glass, ice, tinted glass, slime, honey) \
             must not take the opaque-only packed path"
        );
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
        assert!(!is_packed_cube(&cube, RenderLayer::Solid));
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

    // --- Smooth lighting / ambient occlusion ------------------------------

    /// A view with a single occluding cell, for probing [`quad_corner_sample`]
    /// directly without going through a full [`mesh_models`] pass.
    struct SingleOccluder {
        at: [i32; 3],
    }
    impl ModelSectionView for SingleOccluder {
        fn quads_at(&self, _x: usize, _y: usize, _z: usize) -> &[BakedQuad] {
            &[]
        }
        fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
            [x, y, z] == self.at
        }
    }

    #[test]
    fn ao_matches_vanillas_one_occluder_ratio_and_leaves_the_far_corner_bright() {
        // An Up-face quad on block (8,8,8): `np` is (8,9,8), and a single
        // occluder sits at its `-X` edge neighbour (7,9,8).
        let view = SingleOccluder { at: [7, 9, 8] };
        // Vertex at (x=0,z=0): its edge neighbour *is* the occluder, so one of
        // the three corner samples (a/b/d) occludes. Vanilla's one-occluder AO
        // ratio is 0.8 — `(1.0 + 1.0 + 1.0 + 0.2) / 4`.
        let (ao_near, _) = quad_corner_sample(&view, [8, 9, 8], Face::PosY, [0.0, 1.0, 0.0], 0xF0);
        assert!((ao_near - 0.8).abs() < 1e-6, "got {ao_near}");
        // Vertex at (x=1,z=0): none of its three corner samples touch (7,9,8),
        // so it stays fully bright — proving the AO is per-*corner*, not
        // smeared across the whole quad.
        let (ao_far, _) = quad_corner_sample(&view, [8, 9, 8], Face::PosY, [1.0, 1.0, 0.0], 0xF0);
        assert!((ao_far - 1.0).abs() < 1e-6, "got {ao_far}");
    }

    /// The AO term reads [`ModelSectionView::ao_occludes_at`] (vanilla's
    /// `getShadeBrightness`) and the light term reads
    /// [`ModelSectionView::occludes_at`] (the culling predicate). The two are
    /// driven to **opposite** answers here so neither can be mistaken for the
    /// other — the leaves case is `ao` only, and it darkened nothing before this
    /// split existed.
    #[test]
    fn ao_reads_the_shade_predicate_and_light_reads_the_culling_one() {
        /// `occludes_at` says no, `ao_occludes_at` says yes — a leaf cell.
        struct ShadeOnly {
            at: [i32; 3],
        }
        impl ModelSectionView for ShadeOnly {
            fn quads_at(&self, _x: usize, _y: usize, _z: usize) -> &[BakedQuad] {
                &[]
            }
            fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
                false
            }
            fn ao_occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
                [x, y, z] == self.at
            }
            /// Pitch black **only** in the occluding cell, full-bright elsewhere
            /// — so the light average is a different number depending on whether
            /// `smoothBlend` substituted, and the two hypotheses are separable.
            fn corner_light_at(&self, x: i32, y: i32, z: i32) -> u8 {
                if [x, y, z] == self.at { 0x00 } else { 0xF0 }
            }
        }
        /// The mirror: `occludes_at` says yes, `ao_occludes_at` says no.
        struct CullingOnly {
            at: [i32; 3],
        }
        impl ModelSectionView for CullingOnly {
            fn quads_at(&self, _x: usize, _y: usize, _z: usize) -> &[BakedQuad] {
                &[]
            }
            fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
                [x, y, z] == self.at
            }
            fn ao_occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
                false
            }
            fn corner_light_at(&self, x: i32, y: i32, z: i32) -> u8 {
                if [x, y, z] == self.at { 0x00 } else { 0xF0 }
            }
        }

        // Same geometry as the test above: an Up quad on (8,8,8), occluder at
        // the vertex's `-X` edge neighbour.
        let leaf = ShadeOnly { at: [7, 9, 8] };
        let (ao, light) =
            quad_corner_sample(&leaf, [8, 9, 8], Face::PosY, [0.0, 1.0, 0.0], 0xF0);
        assert!(
            (ao - 0.8).abs() < 1e-6,
            "a cell that occludes for shade brightness but not for culling must still darken \
             the corner to vanilla's one-occluder ratio 0.8, got {ao} — 1.0 here is the \
             canopy-underside bug"
        );
        // …and it must NOT trigger `smoothBlend`, whose key is view-blocking /
        // light dampening. The dark cell's stored 0x00 therefore stands:
        // `round((15 + 15 + 15 + 0) / 4) = 11` sky — the lit centre, the two
        // bright cells of the corner triple, and the dark occluding one.
        assert_eq!(
            light, 0xB0,
            "the light term must ignore ao_occludes_at: with no culling occluder the raw dark \
             sample stands, giving round((15+15+15+0)/4) = 11 sky. 0xF0 would mean the shade \
             predicate leaked into smoothBlend"
        );

        // The mirror: culling-only occlusion must leave AO untouched and drive
        // the light substitution instead.
        let culling = CullingOnly { at: [7, 9, 8] };
        let (ao, light) =
            quad_corner_sample(&culling, [8, 9, 8], Face::PosY, [0.0, 1.0, 0.0], 0xF0);
        assert!(
            (ao - 1.0).abs() < 1e-6,
            "a cell that occludes only for culling must not darken the corner, got {ao}"
        );
        assert_eq!(
            light, 0xF0,
            "the culling occluder's dark 0x00 sample must be replaced by the lit centre \
             (smoothBlend), giving 15 sky"
        );
    }

    #[test]
    fn smooth_blend_substitutes_the_centre_only_above_the_threshold() {
        // Every corner neighbour occludes and (per this synthetic view)
        // stores no light of its own, so any brightness in the result must
        // come from the `smoothBlend` substitution, not the raw sample.
        struct AllOccludedAndDark;
        impl ModelSectionView for AllOccludedAndDark {
            fn quads_at(&self, _x: usize, _y: usize, _z: usize) -> &[BakedQuad] {
                &[]
            }
            fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
                true
            }
            fn corner_light_at(&self, _x: i32, _y: i32, _z: i32) -> u8 {
                0x00
            }
        }
        let view = AllOccludedAndDark;

        // Centre lit above `SMOOTH_LIGHT_MIN_CENTRE`: every occluded corner
        // sample is replaced by the centre's own value, so the corner reads
        // exactly the centre's light rather than the neighbours' stored 0 —
        // the "corner against a wall must not read pitch black" rule.
        let (_, bright) = quad_corner_sample(&view, [8, 9, 8], Face::PosY, [0.0, 1.0, 0.0], 0xF0);
        assert_eq!(bright, 0xF0, "above-threshold centre must substitute in fully");

        // Centre at/below the threshold: substitution does not fire, so the
        // corner reads the neighbours' raw (dark) value and comes out dim.
        let (_, dim) = quad_corner_sample(&view, [8, 9, 8], Face::PosY, [0.0, 1.0, 0.0], 0x20);
        assert!(dim >> 4 <= 2, "below-threshold centre must not substitute, got {dim:#04x}");
    }

    #[test]
    fn ambient_occlusion_at_false_flattens_ao_through_mesh_models() {
        // A block at (8,8,8) with a single Up-face quad and a real occluder at
        // its `-X` edge neighbour (7,9,8) — the same occluder placement
        // `ao_matches_vanillas_one_occluder_ratio_and_leaves_the_far_corner_bright`
        // uses directly on `quad_corner_sample`. Here `ambient_occlusion_at`
        // reports `false`, so per vanilla's `tesselateFlat` fallback
        // `mesh_models` must skip corner sampling entirely and emit every
        // vertex at full AO despite the occluder being real.
        struct OccluderAoDisabled;
        impl ModelSectionView for OccluderAoDisabled {
            fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
                static QUAD: std::sync::OnceLock<Vec<BakedQuad>> = std::sync::OnceLock::new();
                if (x, y, z) == (8, 8, 8) {
                    QUAD.get_or_init(|| vec![cube_face(Direction::Up, None)])
                } else {
                    &[]
                }
            }
            fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
                [x, y, z] == [7, 9, 8]
            }
            fn ambient_occlusion_at(&self, _x: usize, _y: usize, _z: usize) -> bool {
                false
            }
        }

        let mesh = mesh_models(&OccluderAoDisabled);
        assert_eq!(mesh.quad_count(), 1);
        assert!(
            mesh.vertices.iter().all(|v| (v.ao - 1.0).abs() < 1e-6),
            "ambient_occlusion_at() = false must flatten every corner's AO factor to 1.0 \
             (folded with the constant Up-face shade, also 1.0) even though a real occluder \
             is present — got {:?}",
            mesh.vertices.iter().map(|v| v.ao).collect::<Vec<_>>()
        );

        // Executed negative control: the identical occluder with the trait's
        // *default* `ambient_occlusion_at` (`true`) must actually darken every
        // vertex — proving the flat result above is caused by the flag being
        // `false`, not by this occluder placement being inert for some other
        // reason (e.g. a mistaken coordinate).
        struct OccluderAoEnabled;
        impl ModelSectionView for OccluderAoEnabled {
            fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
                static QUAD: std::sync::OnceLock<Vec<BakedQuad>> = std::sync::OnceLock::new();
                if (x, y, z) == (8, 8, 8) {
                    QUAD.get_or_init(|| vec![cube_face(Direction::Up, None)])
                } else {
                    &[]
                }
            }
            fn occludes_at(&self, x: i32, y: i32, z: i32) -> bool {
                [x, y, z] == [7, 9, 8]
            }
        }
        let control = mesh_models(&OccluderAoEnabled);
        assert!(
            control.vertices.iter().all(|v| v.ao < 0.99),
            "control premise violated: with AO enabled (the trait default) this same \
             occluder must darken every vertex (they share one degenerate all-zero \
             position, so all four sample the same corner), or the flattened result above \
             proves nothing. Got {:?}",
            control.vertices.iter().map(|v| v.ao).collect::<Vec<_>>()
        );
    }

    #[test]
    fn emit_baked_quad_flips_triangulation_when_ao_disagrees_across_a_diagonal() {
        // Vanilla's anisotropy fix: cut along whichever diagonal is brighter,
        // so the darker corner never bleeds its shade across the quad via
        // interpolation. `crate::mesh::emit_quad` applies the identical rule.
        let quad = cube_face(Direction::Up, None);

        let mut cut_02 = ModelMesh::default();
        // Corners 0 and 2 (diagonal) bright, 1 and 3 dark: cut along 0-2.
        emit_baked_quad(
            &mut cut_02,
            &quad,
            [0.0, 0.0, 0.0],
            [(1.0, 0xF0), (0.2, 0x00), (1.0, 0xF0), (0.2, 0x00)],
            None,
            false,
        );
        assert_eq!(cut_02.indices, vec![0, 1, 2, 2, 3, 0]);

        let mut cut_13 = ModelMesh::default();
        // The other diagonal (1-3) bright instead: cut flips to 1-3.
        emit_baked_quad(
            &mut cut_13,
            &quad,
            [0.0, 0.0, 0.0],
            [(0.2, 0x00), (1.0, 0xF0), (0.2, 0x00), (1.0, 0xF0)],
            None,
            false,
        );
        assert_eq!(cut_13.indices, vec![1, 2, 3, 3, 0, 1]);
    }

    #[test]
    fn force_opaque_sets_the_cutout_bypass_byte_on_every_emitted_vertex() {
        // The render-side half of the leaves fix: a quad emitted with
        // `force_opaque = true` must carry a nonzero `cutout_bypass` on all
        // four vertices, and `false` must carry a zero one — the fragment
        // shader's `in.cutout_bypass != 0u` gate reads exactly this byte.
        let quad = cube_face(Direction::Up, None);

        let mut opaque = ModelMesh::default();
        emit_baked_quad(
            &mut opaque,
            &quad,
            [0.0, 0.0, 0.0],
            [(1.0, 0xF0); 4],
            None,
            true,
        );
        assert!(
            opaque.vertices.iter().all(|v| v.cutout_bypass != 0),
            "force_opaque=true must set cutout_bypass on every vertex, got {:?}",
            opaque.vertices.iter().map(|v| v.cutout_bypass).collect::<Vec<_>>()
        );

        let mut cutout = ModelMesh::default();
        emit_baked_quad(
            &mut cutout,
            &quad,
            [0.0, 0.0, 0.0],
            [(1.0, 0xF0); 4],
            None,
            false,
        );
        assert!(
            cutout.vertices.iter().all(|v| v.cutout_bypass == 0),
            "force_opaque=false must leave cutout_bypass zero, got {:?}",
            cutout.vertices.iter().map(|v| v.cutout_bypass).collect::<Vec<_>>()
        );
    }

    #[test]
    fn mesh_models_reads_force_opaque_per_block_from_the_view() {
        // The mesher-level control: a `ModelSectionView` whose
        // `force_opaque_at` reports `true` must produce a mesh where every
        // vertex carries the bypass, proving `mesh_models` actually reads the
        // trait method rather than always passing `false` through. The
        // default-`false` population (every existing view) is the negative
        // control this needs — see the sibling struct below.
        struct AlwaysOpaque;
        impl ModelSectionView for AlwaysOpaque {
            fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
                static QUAD: std::sync::OnceLock<Vec<BakedQuad>> = std::sync::OnceLock::new();
                if x == 0 && y == 0 && z == 0 {
                    QUAD.get_or_init(|| vec![cube_face(Direction::Up, None)])
                } else {
                    &[]
                }
            }
            fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
                false
            }
            fn force_opaque_at(&self, _x: usize, _y: usize, _z: usize) -> bool {
                true
            }
        }
        let mesh = mesh_models(&AlwaysOpaque);
        assert!(!mesh.vertices.is_empty(), "premise: the fixture block meshed");
        assert!(
            mesh.vertices.iter().all(|v| v.cutout_bypass != 0),
            "a view reporting force_opaque_at=true must reach every emitted vertex"
        );

        // Control: the default `force_opaque_at` (unimplemented here) is
        // `false`, so the identical fixture through a view that does not
        // override it must emit zero.
        struct DefaultOpacity;
        impl ModelSectionView for DefaultOpacity {
            fn quads_at(&self, x: usize, y: usize, z: usize) -> &[BakedQuad] {
                static QUAD: std::sync::OnceLock<Vec<BakedQuad>> = std::sync::OnceLock::new();
                if x == 0 && y == 0 && z == 0 {
                    QUAD.get_or_init(|| vec![cube_face(Direction::Up, None)])
                } else {
                    &[]
                }
            }
            fn occludes_at(&self, _x: i32, _y: i32, _z: i32) -> bool {
                false
            }
        }
        let control = mesh_models(&DefaultOpacity);
        assert!(!control.vertices.is_empty(), "premise: the fixture block meshed");
        assert!(
            control.vertices.iter().all(|v| v.cutout_bypass == 0),
            "the default force_opaque_at must leave cutout_bypass at zero"
        );
    }

    // --- GUI item meshing ------------------------------------------------

    #[test]
    fn item_quads_ignore_cullface() {
        // A GUI slot has no neighbours, so a `cullface` must not remove a quad —
        // the pipeline's back-face culling is what hides the far faces. Meshing
        // a full cube (every face culled by its own neighbour) through the world
        // path with occluding neighbours would drop faces; here all six survive.
        let cube = full_cube();
        let m = mesh_item_quads(&cube, Mat4::IDENTITY, GuiLight::Side);
        assert_eq!(m.quad_count(), 6);
        assert_eq!(m.vertices.len(), 24);
    }

    #[test]
    fn item_vertices_are_full_bright() {
        let m = mesh_item_quads(&full_cube(), Mat4::IDENTITY, GuiLight::Side);
        assert!(m.vertices.iter().all(|v| v.light == GUI_ITEM_LIGHT));
        // The shader runs `max(sky, block)` through vanilla's `lightmap.fsh`
        // curve; sky 15 makes that exactly 1.0. Written out from the formula, not
        // read from `crate::light`, so this stays an external anchor.
        let sky = f32::from(GUI_ITEM_LIGHT >> 4) / 15.0;
        let block = f32::from(GUI_ITEM_LIGHT & 0xF) / 15.0;
        let level = sky.max(block);
        let curved = level / (4.0 - 3.0 * level);
        let term = curved + ((1.0 - (1.0 - curved).powi(4)) - curved) * 0.5;
        assert!((term - 1.0).abs() < 1e-6);
    }

    #[test]
    fn side_lighting_keeps_the_per_face_constants_and_front_flattens_them() {
        let cube = full_cube();
        let side = mesh_item_quads(&cube, Mat4::IDENTITY, GuiLight::Side);
        // `full_cube` is ordered Down, Up, North, South, East, West; the shade
        // constants are the same ones `mesh_models` applies.
        let ao = |i: usize| side.vertices[i * 4].ao;
        assert!((ao(0) - 0.5).abs() < 1e-6, "down");
        assert!((ao(1) - 1.0).abs() < 1e-6, "up");
        assert!((ao(2) - 0.8).abs() < 1e-6, "north");
        assert!((ao(3) - 0.8).abs() < 1e-6, "south");
        assert!((ao(4) - 0.6).abs() < 1e-6, "east");
        assert!((ao(5) - 0.6).abs() < 1e-6, "west");

        // `gui_light: front` is flat: every face reads 1.0.
        let front = mesh_item_quads(&cube, Mat4::IDENTITY, GuiLight::Front);
        assert!(front.vertices.iter().all(|v| (v.ao - 1.0).abs() < 1e-6));
    }

    #[test]
    fn the_pose_is_applied_to_positions() {
        let mut q = cube_face(Direction::Up, None);
        q.positions = [
            [0.0, 1.0, 0.0],
            [1.0, 1.0, 0.0],
            [1.0, 1.0, 1.0],
            [0.0, 1.0, 1.0],
        ];
        let pose =
            Mat4::from_translation(Vec3::new(10.0, 0.0, 0.0)) * Mat4::from_scale(Vec3::splat(2.0));
        let m = mesh_item_quads(std::slice::from_ref(&q), pose, GuiLight::Side);
        assert_eq!(m.vertices[0].position, [10.0, 2.0, 0.0]);
        assert_eq!(m.vertices[2].position, [12.0, 2.0, 2.0]);
    }

    #[test]
    fn item_quads_carry_their_palette_tint_and_animation_slot() {
        let mut q = cube_face(Direction::Up, None);
        q.tint_index = Some(7);
        q.anim = 3;
        let m = mesh_item_quads(std::slice::from_ref(&q), Mat4::IDENTITY, GuiLight::Side);
        assert!(m.vertices.iter().all(|v| v.tint == 7 && v.anim == 3));

        // An untinted quad falls back to the reserved white palette slot.
        let plain = cube_face(Direction::Up, None);
        let m = mesh_item_quads(std::slice::from_ref(&plain), Mat4::IDENTITY, GuiLight::Side);
        assert!(m.vertices.iter().all(|v| v.tint == 255));
    }

    #[test]
    fn item_triangles_keep_the_quad_winding() {
        let m = mesh_item_quads(&full_cube(), Mat4::IDENTITY, GuiLight::Side);
        assert_eq!(m.indices[..6], [0, 1, 2, 0, 2, 3]);
        assert_eq!(m.indices[6..12], [4, 5, 6, 4, 6, 7]);
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
        /// Cells that answer `overlay_at` true (a stand-in for a glass/ice/leaves
        /// neighbour). Empty by default, matching the trait's `false` default.
        overlays: HashSet<(i32, i32, i32)>,
        /// Whether `fluid_sprites` should hand back a distinguishable overlay
        /// rect at all — `None` reproduces a fluid with no overlay material
        /// (lava), even if `overlays` is non-empty.
        overlay_sprite: Option<()>,
        /// Cells that answer `partial_occluder_y_range_at` with a real
        /// `(min_y, max_y)` — a stand-in for a `dirt_path`/`farmland`-shaped
        /// full-footprint, height-reduced neighbour. Empty by default,
        /// matching the trait's `None` default.
        partial_occluders: HashMap<(i32, i32, i32), (f32, f32)>,
    }
    impl FakeFluidView {
        fn with_overlay_material(mut self) -> Self {
            self.overlay_sprite = Some(());
            self
        }
        fn overlay(&mut self, x: i32, y: i32, z: i32) {
            self.overlays.insert((x, y, z));
        }
        /// Marks `(x, y, z)` as a full-footprint, height-reduced occluder
        /// spanning `min_y..=max_y`.
        fn partial_occluder(&mut self, x: i32, y: i32, z: i32, min_y: f32, max_y: f32) {
            self.partial_occluders.insert((x, y, z), (min_y, max_y));
        }
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
                anim: 0,
            };
            FluidSprites {
                still: unit,
                flow: unit,
                overlay: self.overlay_sprite.map(|_| SpriteUv {
                    min: [0.25, 0.25],
                    max: [0.75, 0.75],
                    anim: 0,
                }),
            }
        }
        fn overlay_at(&self, x: i32, y: i32, z: i32) -> bool {
            self.overlays.contains(&(x, y, z))
        }
        fn partial_occluder_y_range_at(&self, x: i32, y: i32, z: i32) -> Option<(f32, f32)> {
            self.partial_occluders.get(&(x, y, z)).copied()
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

        // Open air on every side (including above) means the surface gets a
        // back-facing top copy (`shouldRenderBackwardUpFace`) and every one of
        // the 4 open sides gets a reversed back copy too (`addBackFace`,
        // suppressed only for an overlay side face — none here): top 1+1,
        // sides 4*(1+1), bottom culled by the floor.
        assert_eq!(
            m.water.quad_count(),
            10,
            "top (front+back) + 4 sides (front+back each), bottom culled by floor"
        );
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

        // Each cell loses its shared (east/west) face — and that face would
        // have been a front+back pair (open air, no overlay), so each cell
        // loses 2 quads: 4 fewer than 2x lone.
        assert_eq!(
            pair_quads,
            lone_sides * 2 - 4,
            "the shared water-water face (front+back) on both cells must be culled"
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

    // --- The shoreline: a pool bounded by real banks -----------------------

    /// A pool of water walled on all four sides and floored, with open air above.
    /// `bank_occludes` chooses whether the bank blocks are reported as occluding,
    /// which is the single bit the reported water bug turned on the wrong way.
    fn walled_pool(bank_occludes: bool) -> FakeFluidView {
        let mut v = FakeFluidView::default();
        for y in 0..8 {
            for z in 0..16 {
                for x in 0..16 {
                    let inside = (4..12).contains(&x) && (4..12).contains(&z);
                    if inside {
                        v.water(x, y, z, FluidState::source());
                    } else if bank_occludes {
                        v.solid(x, y, z);
                    }
                }
            }
        }
        // The floor under the pool, and the banks one cell outside the section.
        for z in 0..16 {
            for x in 0..16 {
                if bank_occludes {
                    v.solid(x, -1, z);
                }
            }
        }
        v
    }

    /// How many of a fluid mesh's quads are *vertical* (a side face), and how
    /// many horizontal surfaces are **level** (all four corners at one height).
    fn face_profile(mesh: &ModelMesh) -> (usize, usize, usize) {
        let mut vertical = 0;
        let mut level_top = 0;
        let mut sloped_top = 0;
        for q in mesh.vertices.chunks(4) {
            let ys: Vec<f32> = q.iter().map(|v| v.position[1]).collect();
            let flat = ys.iter().all(|y| (y - ys[0]).abs() < 1e-6);
            if !flat {
                // Either a genuine side face, or a *sloped* top surface. Tell them
                // apart by whether the quad has any vertical extent beyond the
                // corner-height spread: a side face spans down to the cell base.
                let (lo, hi) = ys.iter().fold((f32::MAX, f32::MIN), |(l, h), &y| {
                    (l.min(y), h.max(y))
                });
                if hi - lo > 0.5 {
                    vertical += 1;
                } else {
                    sloped_top += 1;
                }
            } else {
                level_top += 1;
            }
        }
        (vertical, level_top, sloped_top)
    }

    /// The whole reported bug in one assertion, with the pre-fix behaviour as the
    /// executed negative control.
    ///
    /// The user's report was: water "shows the 'flowing down' effect on the edges
    /// that touch non-water blocks". Vanilla's `FluidRenderer.tesselate` culls a
    /// fluid side face whose neighbour occludes it
    /// (`!isFaceOccludedByNeighbor(faceDir, max(h0, h1), faceState)`, and for a
    /// `Shapes.block()` occluder that test is `direction != UP` — i.e. always true
    /// for a horizontal face). So a pool walled in solid blocks must emit **only**
    /// its top surface, and that surface must be level: `FluidRenderer.getHeight`
    /// returns `-1.0` for a solid non-fluid neighbour, which
    /// `addWeightedHeight` drops from the average entirely, whereas an *air*
    /// neighbour contributes `0.0` and drags the corner down.
    ///
    /// Both halves run here. The bank that occludes is the fixed behaviour; the
    /// bank that does not is exactly what `grass_block` used to report, and it
    /// fails every assertion below — 284 side faces and a tilted rim instead of
    /// zero and flat.
    #[test]
    fn a_walled_pool_emits_only_its_level_top_surface() {
        let (vertical, level, sloped) = face_profile(&mesh_fluids(&walled_pool(true)).water);
        assert_eq!(
            vertical, 0,
            "a pool walled in occluding blocks must emit no side faces at all — every \
             one of them would draw the animated water_flow sprite over the bank"
        );
        assert_eq!(sloped, 0, "no corner may slope toward an occluding bank");
        // The whole sky above the pool is open air, so every top-surface cell's
        // `shouldRenderBackwardUpFace` ring is all-air too: each of the 8x8
        // level quads gets a back copy (128), matching vanilla's own open-lake
        // behaviour rather than a single-sided sheet.
        assert_eq!(
            level, 128,
            "the 8x8 top surface of the pool's topmost layer, front+back"
        );

        // Negative control, executed: the pre-fix occlusion answer for a
        // grass-block bank. Every assertion above must fail on it.
        let (bad_vertical, _bad_level, bad_sloped) =
            face_profile(&mesh_fluids(&walled_pool(false)).water);
        assert!(
            bad_vertical > 0 && bad_sloped > 0,
            "control must reproduce the bug: a non-occluding bank has to yield side \
             faces ({bad_vertical}) and sloped rim quads ({bad_sloped}); if it does not, \
             this gate cannot see the defect it exists to catch"
        );
    }

    // --- Known-gap closures: up-face culling, overlay, back faces ----------

    /// Gap: "the up face is not culled by a solid block above" (vanilla draws
    /// it). `isFaceOccludedByState`'s `direction != UP || height == 1.0F` only
    /// culls the top face for a *full* solid neighbour when every corner is
    /// already `1.0` — never true for a plain source surrounded by open air on
    /// its sides, whose corners sit at `8/9`. So water directly under a solid
    /// ceiling must still draw its top surface into the `1/9` gap.
    #[test]
    fn water_under_a_solid_ceiling_still_draws_its_top_surface() {
        let mut v = FakeFluidView::default();
        v.water(8, 8, 8, FluidState::source());
        v.solid(8, 9, 8); // ceiling directly above
        let m = mesh_fluids(&v);

        let has_up = m.water.vertices.chunks(4).any(|q| {
            let ys: Vec<f32> = q.iter().map(|vx| vx.position[1]).collect();
            ys.iter().all(|y| (y - ys[0]).abs() < 1e-6) && ys[0] > 8.5
        });
        assert!(
            has_up,
            "a solid block directly above must not cull the fluid's top surface \
             — vanilla only culls it when every corner height is already 1.0"
        );

        // Executed negative control: the pre-fix rule (cull whenever the
        // neighbour occludes, regardless of corner height) must fail this same
        // assertion — it is exactly the divergence this test exists to catch.
        let same = |dx: i32, dy: i32, dz: i32| {
            matches!(v.fluid_at(8 + dx, 8 + dy, 8 + dz), Some(f) if f.kind == FluidKind::Water)
        };
        let pre_fix_up_emitted = !same(0, 1, 0) && !v.occludes_at(8, 9, 8);
        assert!(
            !pre_fix_up_emitted,
            "control premise: the pre-fix whole-occludes rule must in fact cull \
             this face, or this test cannot distinguish the fix from a no-op"
        );
    }

    // --- Known-gap closure: partial occluders (dirt_path/farmland banks) ---

    /// Gap: "partial occluders are not modelled" — a `dirt_path`/`farmland`
    /// bank (full `x`/`z` footprint, reduced height) still drew a spurious
    /// side face because the old test was purely `occludes_at`'s whole-block
    /// boolean, which such a bank never satisfies. `partial_occluder_y_range_at`
    /// closes the scoped case: a tall-enough full-footprint neighbour now culls
    /// the touching side exactly as `Shapes.blockOccludes`'s general branch
    /// would.
    #[test]
    fn a_tall_full_footprint_bank_culls_the_side_face_it_fully_covers() {
        let mut v = FakeFluidView::default();
        v.water(8, 8, 8, FluidState::source());
        // Not a full cube (occludes_at stays false) but tall enough (min 0,
        // max 1) to cover any face height a lone source can produce.
        v.partial_occluder(9, 8, 8, 0.0, 1.0); // east bank

        let m = mesh_fluids(&v);
        assert!(
            !v.occludes_at(9, 8, 8),
            "control premise: the bank must not be a whole-block occluder, or \
             this test cannot tell the new mechanism from the old one"
        );
        let east_face_present = m.water.vertices.chunks(4).any(|q| {
            q.iter()
                .all(|vx| (vx.position[0] - 9.0).abs() < 0.01)
        });
        assert!(
            !east_face_present,
            "a full-footprint bank taller than the fluid's own face height must \
             cull that side face, matching vanilla's blockOccludes general branch"
        );
    }

    /// Negative control, magnitude-checked rather than direction-only: a bank
    /// with the *same* full footprint but too short to reach the fluid's face
    /// height must leave the face un-culled — the height comparison has to be
    /// real, not merely "some occluder is present".
    #[test]
    fn a_short_full_footprint_bank_does_not_cull_the_side_face() {
        let mut v = FakeFluidView::default();
        v.water(8, 8, 8, FluidState::source());
        // Full footprint, but only 0.1 blocks tall — far short of a lone
        // source's corner heights (which sit well above 0.1 once averaged).
        v.partial_occluder(9, 8, 8, 0.0, 0.1);

        let m = mesh_fluids(&v);
        let east_face_present = m.water.vertices.chunks(4).any(|q| {
            q.iter()
                .all(|vx| (vx.position[0] - 9.0).abs() < 0.01)
        });
        assert!(
            east_face_present,
            "a bank too short to cover the fluid's face height must not cull it \
             — otherwise this mechanism culls on presence alone, not height"
        );
    }

    /// A second negative control: a full-footprint occluder that is tall
    /// enough at its *top* but floats above the cell base (`min_y > 0`) must
    /// not cull either — vanilla's `blockOccludes` requires the occluder to
    /// reach the near boundary (`min(axis) == 0`), matching a real
    /// `dirt_path`/`farmland` shape but not a raised platform.
    #[test]
    fn a_raised_full_footprint_occluder_does_not_cull_the_side_face() {
        let mut v = FakeFluidView::default();
        v.water(8, 8, 8, FluidState::source());
        v.partial_occluder(9, 8, 8, 0.5, 1.0); // floats above y=0

        let m = mesh_fluids(&v);
        let east_face_present = m.water.vertices.chunks(4).any(|q| {
            q.iter()
                .all(|vx| (vx.position[0] - 9.0).abs() < 0.01)
        });
        assert!(
            east_face_present,
            "an occluder that does not reach the cell's base must not cull the \
             side face — the near-boundary condition in blockOccludes is real"
        );
    }

    /// Gap: "no `water_overlay` material for glass/ice/leaves neighbours" —
    /// `mesh_fluids` must route a side face against an `overlay_at` neighbour
    /// onto the overlay sprite, and per vanilla's `addBackFace = !isOverlay`,
    /// must not emit that face's back copy.
    #[test]
    fn side_face_against_an_overlay_neighbor_uses_the_overlay_sprite_and_has_no_back_face() {
        let mut v = FakeFluidView::default().with_overlay_material();
        v.water(8, 8, 8, FluidState::source());
        v.overlay(8, 8, 7); // north neighbour is glass/ice/leaves-like
        let m = mesh_fluids(&v);

        // North is a vertical quad (side face); with an overlay neighbour it
        // must appear exactly once (no reversed back copy).
        let north_quads: Vec<_> = m
            .water
            .vertices
            .chunks(4)
            .filter(|q| {
                let ys: Vec<f32> = q.iter().map(|vx| vx.position[1]).collect();
                let (lo, hi) = ys
                    .iter()
                    .fold((f32::MAX, f32::MIN), |(l, h), &y| (l.min(y), h.max(y)));
                hi - lo > 0.5 && q.iter().all(|vx| (vx.position[2] - 8.0).abs() < 0.01)
            })
            .collect();
        assert_eq!(
            north_quads.len(),
            1,
            "an overlay side face must have no back copy (addBackFace = !isOverlay)"
        );
        // The overlay sprite (from FakeFluidView) is the [0.25,0.25]..[0.75,0.75]
        // rect; a plain flow face would sample the [0,0]..[1,1] unit rect
        // instead, so every UV must land strictly inside the overlay rect.
        assert!(
            north_quads[0]
                .iter()
                .all(|vx| vx.uv[0] >= 0.25 && vx.uv[0] <= 0.75),
            "overlay side face must sample the overlay sprite, not flow"
        );
    }

    /// Same neighbourhood, but the view reports no overlay material at all
    /// (`with_overlay_material` not called) — matching lava, which has none in
    /// vanilla. The `overlay_at` flag must be ignored and the back face
    /// restored, proving `bake_fluid`'s `overlay: Option<SpriteUv>` gate (not
    /// just `SideOverlay`) is what the mesher actually threads through.
    #[test]
    fn overlay_flag_without_an_overlay_material_falls_back_to_flow_with_a_back_face() {
        let mut v = FakeFluidView::default(); // no with_overlay_material()
        v.water(8, 8, 8, FluidState::source());
        v.overlay(8, 8, 7);
        let m = mesh_fluids(&v);

        let north_quads = m.water.vertices.chunks(4).filter(|q| {
            let ys: Vec<f32> = q.iter().map(|vx| vx.position[1]).collect();
            let (lo, hi) = ys
                .iter()
                .fold((f32::MAX, f32::MIN), |(l, h), &y| (l.min(y), h.max(y)));
            hi - lo > 0.5 && q.iter().all(|vx| (vx.position[2] - 8.0).abs() < 0.01)
        });
        assert_eq!(
            north_quads.count(),
            2,
            "no overlay sprite resolved (lava-like): back face must be restored"
        );
    }
    // -----------------------------------------------------------------------
    // `mesh_moving_block_quads` — the moving-block-model seam's mesh primitive
    // -----------------------------------------------------------------------

    /// A downward-facing full-cell quad, so `face_shade` resolves to the `Down`
    /// constant (`0.5`) rather than the `Up` one — chosen because `0.5` is the one
    /// face constant that cannot be confused with the unshaded `1.0`.
    fn down_quad() -> BakedQuad {
        BakedQuad {
            positions: [
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
            ],
            uvs: [[0.25, 0.5], [0.75, 0.5], [0.75, 0.9], [0.25, 0.9]],
            direction: lodestone_assets::Direction::Down,
            cullface: Some(lodestone_assets::Direction::Down),
            tint_index: Some(3),
            shade: true,
            layer: 0,
            anim: 5,
            sprite: 0,
        }
    }

    /// Positions are transformed by the pose, and every other per-quad property is
    /// carried through untouched.
    ///
    /// The property assertions are the point rather than padding: this path exists
    /// because the crack mesher — the *other* consumer of the same per-state quad
    /// snapshot — deliberately **replaces** the UVs with a `destroy_stage` rect.
    /// Reusing that mesher for a moving block would draw a block-shaped crack
    /// overlay, so a gate that only checked positions would not have noticed.
    #[test]
    fn moving_block_quads_are_posed_and_keep_their_own_uvs_tint_and_anim() {
        let pose = Mat4::from_translation(Vec3::new(10.0, -4.0, 2.5));
        let mesh = mesh_moving_block_quads(&[down_quad()], pose, 0x2B);

        assert_eq!(mesh.vertices.len(), 4);
        assert_eq!(mesh.indices, vec![0, 1, 2, 0, 2, 3], "two triangles per quad");

        let expected_positions = [
            [10.0, -4.0, 2.5],
            [11.0, -4.0, 2.5],
            [11.0, -4.0, 3.5],
            [10.0, -4.0, 3.5],
        ];
        let mismatched: Vec<(usize, [f32; 3], [f32; 3])> = mesh
            .vertices
            .iter()
            .enumerate()
            .filter(|(i, v)| {
                let want = expected_positions[*i];
                (0..3).any(|a| (v.position[a] - want[a]).abs() > 1e-5)
            })
            .map(|(i, v)| (i, v.position, expected_positions[i]))
            .collect();
        assert!(
            mismatched.is_empty(),
            "posed positions wrong at (index, got, want): {mismatched:?}"
        );

        for (i, v) in mesh.vertices.iter().enumerate() {
            assert_eq!(v.uv, down_quad().uvs[i], "vertex {i} lost its own UV");
            assert_eq!(v.light, 0x2B, "vertex {i} did not take the supplied light");
            assert_eq!(v.tint, 3, "vertex {i} lost its palette index");
            assert_eq!(v.anim, 5, "vertex {i} lost its animation slot");
            assert_eq!(
                v.tint_rgb_override, [0, 0, 0, 0],
                "this path has no biome override to carry"
            );
        }
    }

    /// The shade is the **per-face directional constant**, not the GUI path's
    /// `gui_light: front` flattening to `1.0`.
    ///
    /// Both readings are evaluated: a `Down` face is `0.5`, and a `shade: false`
    /// quad is `1.0`. A gate that only checked the first would pass under an
    /// implementation that flattened everything, because `1.0` is also a legal
    /// shade — so the discriminating claim is that the two inputs produce
    /// *different* values.
    #[test]
    fn moving_block_shade_is_per_face_and_not_flattened() {
        let shaded = mesh_moving_block_quads(&[down_quad()], Mat4::IDENTITY, 0xF0);
        let mut unshaded_quad = down_quad();
        unshaded_quad.shade = false;
        let unshaded = mesh_moving_block_quads(&[unshaded_quad], Mat4::IDENTITY, 0xF0);

        assert_eq!(shaded.vertices[0].ao, 0.5, "a Down face is vanilla's 0.5");
        assert_eq!(unshaded.vertices[0].ao, 1.0, "`shade: false` is unshaded");
        assert_ne!(
            shaded.vertices[0].ao, unshaded.vertices[0].ao,
            "control: if these coincide the shade term is being flattened and this \
             gate measures nothing"
        );
    }

    /// `cullface` is ignored, so a quad that would be culled against a neighbour is
    /// still emitted.
    ///
    /// A moving block has no neighbours; a quad dropped because "the block to the
    /// north occludes it" would leave a hole in mid-air. The fixture's quad carries
    /// `cullface: Some(Down)` precisely so this is a real test rather than a
    /// restatement.
    #[test]
    fn moving_block_quads_ignore_cullface() {
        assert!(
            down_quad().cullface.is_some(),
            "control: the fixture must actually declare a cullface"
        );
        let mesh = mesh_moving_block_quads(&[down_quad()], Mat4::IDENTITY, 0xF0);
        assert_eq!(mesh.quad_count(), 1, "a cullface must not drop the quad");
    }

    /// No quads means an empty mesh rather than a panic — the answer for air and for
    /// every `RenderShape.INVISIBLE` block, which callers read as "draw nothing".
    #[test]
    fn moving_block_quads_of_nothing_is_an_empty_mesh() {
        let mesh = mesh_moving_block_quads(&[], Mat4::IDENTITY, 0xF0);
        assert_eq!(mesh.quad_count(), 0);
        assert!(mesh.vertices.is_empty() && mesh.indices.is_empty());
    }
}
