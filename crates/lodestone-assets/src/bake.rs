//! Baking resolved models into renderer-ready geometry ([`bake_model`]).
//!
//! This is the last asset stage before chunk meshing. It takes a
//! [`ResolvedModel`] (parent chain flattened, texture variables resolved) plus
//! the [`Atlas`] that holds its textures and produces a flat list of
//! [`BakedQuad`]s: four corner positions in block-local space, atlas-mapped
//! UVs, the geometric face direction, a (rotation-aware) cull direction, tint
//! index and shade flag. The renderer never walks JSON, resolves parents, or
//! touches the atlas layout — everything is precomputed here.
//!
//! The geometry math is a faithful port of vanilla's `FaceBakery`: default UV
//! derivation from the element box, the per-face corner winding, element
//! rotation (with the classic single-axis rescale), block model `x`/`y`
//! rotation about the block centre, face-`rotation` UV shifting, and `uvlock`
//! (UVs recomputed so they stay world-aligned under model rotation). Matching
//! vanilla here is what keeps rotated stairs, panes and fences from looking
//! scrambled.
//!
//! # Layering
//!
//! - [`bake_model`] is the pure geometry core: `ResolvedModel` + `Atlas` +
//!   [`ModelTransform`] → quads. It is trivially unit-testable with
//!   hand-computed fixtures.
//! - [`BlockBaker`] ties the stack together: it reads a block's blockstate,
//!   selects the applicable variant/multipart models for a property map (or a
//!   numeric block state id via a [`BlockStateRegistry`]), resolves each model,
//!   and bakes the union.
//!
//! Weighted variant lists are collapsed to a single model with a swappable
//! [`WeightSelector`]; [`FirstWeight`] (deterministic, take the first) is the
//! default. Vanilla uses position-hashed randomness, which a future selector
//! can supply by seeding [`SeededWeight`] from the block position.

use std::collections::BTreeMap;

use lodestone_model::{BlockStateRegistry, Identifier};

use crate::atlas::Atlas;
use crate::blockstate::{BlockStates, ModelRef};
use crate::error::BakeError;
use crate::location::ResourceLocation;
use crate::manager::ResourceManager;
use crate::model::{Direction, Element, ElementRotation, Face, ModelResolver, ResolvedModel};

/// Directions in a fixed order, used everywhere geometry is emitted so that
/// output is deterministic (a model's `faces` map has no inherent order).
const DIRECTIONS: [Direction; 6] = [
    Direction::Down,
    Direction::Up,
    Direction::North,
    Direction::South,
    Direction::East,
    Direction::West,
];

/// A single baked quad: four vertices with positions and UVs, plus the metadata
/// the mesher needs to place, cull, light and tint it.
///
/// Positions are in block-local space (`0.0..=1.0` for a full cube; elements
/// may extend slightly outside). The four vertices follow vanilla's winding for
/// [`direction`](Self::direction). UVs are normalised atlas coordinates
/// (`0.0..=1.0` across the whole atlas) for the sprite's **first** animation
/// frame; the renderer advances animated frames itself using the sprite's
/// frame metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct BakedQuad {
    /// The four corner positions in block-local space.
    pub positions: [[f32; 3]; 4],
    /// The four corner UVs in normalised atlas coordinates.
    pub uvs: [[f32; 2]; 4],
    /// The geometric facing of the quad, derived from its vertices after all
    /// rotation. Used by the mesher for shading and face grouping.
    pub direction: Direction,
    /// The direction whose occlusion culls this quad, rotated to follow the
    /// model's `x`/`y` rotation. `None` if the face is never culled.
    pub cullface: Option<Direction>,
    /// The biome/colour tint index, or `None` for an untinted quad.
    pub tint_index: Option<i32>,
    /// Whether this quad participates in directional shading (vanilla `shade`).
    pub shade: bool,
    /// The atlas layer the sprite lives on (always `0` for the single-atlas
    /// layout; present so a texture-array switch is not an API break).
    pub layer: u32,
    /// The animation slot of the sprite this quad samples, or `0` when the
    /// sprite is static. Copied from [`AtlasSprite::anim_slot`]; the renderer
    /// uses it to look up a per-frame V offset so the baked (frame-0) UVs
    /// advance without any mesh or atlas mutation. See [`AnimTable`].
    pub anim: u8,
    /// This quad's index into [`Atlas::sprites`]'s slice — the sprite its
    /// [`Self::uvs`] were baked against, recorded once here instead of
    /// re-derived by a UV containment scan.
    ///
    /// # Why this exists
    ///
    /// The baker already knows exactly which [`AtlasSprite`] it resolved
    /// (`atlas.sprite(location)`) when it computed [`Self::uvs`] from that
    /// sprite's frame rect, and used to throw the answer away — a caller
    /// that needed "which sprite does this quad sample" (per-block render
    /// layer, per-face occlusion) had to recover it geometrically, scanning
    /// every atlas sprite and testing UV containment, once per quad per
    /// block state. This field makes that lookup an array index.
    ///
    /// `0` for a quad built by a baker with no atlas index to record (fluid
    /// quads today, see [`crate::fluid`]) — harmless, because nothing reads
    /// a fluid quad's `sprite` field; fluid render-layer/occlusion never go
    /// through the sprite-rect path this field feeds.
    pub sprite: u32,
}

/// A baked model: every quad from every element, ready for the mesher.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BakedModel {
    /// The baked quads, in element order then [`DIRECTIONS`] order.
    pub quads: Vec<BakedQuad>,
    /// Normalised atlas UVs `[u0, v0, u1, v1]` of the model's `#particle`
    /// texture — vanilla's own baked-model "particle icon" accessor, the sprite that break
    /// and landing particles sample.
    ///
    /// This is **not** derivable from the quads: `grass_block` declares
    /// `"particle": "block/dirt"` while none of its faces use dirt, so a
    /// renderer that guessed from the first face would throw grass-coloured
    /// fragments where vanilla throws dirt. `None` when the model declares no
    /// `particle` variable or it resolves to no atlas sprite.
    pub particle_uv: Option<[f32; 4]>,
    /// The model's `ambientocclusion` flag (JSON default `true`), from the
    /// **first** model resolved for this state.
    ///
    /// Mirrors vanilla's own model-block-renderer "tesselate block" step, which reads
    /// `this.parts.getFirst().useAmbientOcclusion()` — a multipart block (e.g. a
    /// fence with several part models) is gated by its first part only, the same
    /// "first resolved model wins" rule [`particle_uv`](Self::particle_uv)
    /// already follows. This is **half** of vanilla's AO gate; the other half,
    /// `blockState.getLightEmission() == 0`, is a block-state property this
    /// crate has no source for yet (not in `blocks.json` — see `CLAUDE.md`'s
    /// data-sources note — and not read by any oracle dump in the repo), so it
    /// is not applied. A light-emitting full-cube model (e.g. `sea_lantern`)
    /// will therefore still take the smooth-AO path here where vanilla would
    /// flatten it.
    pub ambient_occlusion: bool,
}

impl BakedModel {
    /// Whether this model produced no geometry (e.g. `air`, or a model whose
    /// only element defines no faces).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.quads.is_empty()
    }
}

/// The block-model placement transform from a blockstate `ModelRef`: whole-model
/// rotation about the block centre plus the `uvlock` flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ModelTransform {
    /// Rotation about the X axis in degrees (`0`, `90`, `180`, `270`).
    pub x: i32,
    /// Rotation about the Y axis in degrees.
    pub y: i32,
    /// Whether UVs are locked to stay world-aligned under the rotation.
    pub uvlock: bool,
}

impl ModelTransform {
    /// Builds a transform from a blockstate [`ModelRef`].
    #[must_use]
    pub fn from_model_ref(model_ref: &ModelRef) -> Self {
        Self {
            x: model_ref.x,
            y: model_ref.y,
            uvlock: model_ref.uvlock,
        }
    }
}

/// Chooses one model from a weighted candidate list.
///
/// Blockstate variants (and multipart cases) may list several models with
/// weights for random visual variation. Baking collapses each list to one
/// model. This trait makes the policy swappable: the default [`FirstWeight`] is
/// deterministic, while a future implementation can reproduce vanilla's
/// position-hashed randomness by seeding [`SeededWeight`] per block.
pub trait WeightSelector: std::fmt::Debug {
    /// Returns the index into a non-empty weighted list to bake. The slice of
    /// weights is guaranteed non-empty; the returned index must be in range.
    fn select(&self, weights: &[u32]) -> usize;
}

/// Always selects the first candidate. Deterministic and order-stable.
#[derive(Debug, Clone, Copy, Default)]
pub struct FirstWeight;

impl WeightSelector for FirstWeight {
    fn select(&self, _weights: &[u32]) -> usize {
        0
    }
}

/// Selects deterministically from the weighted distribution using a fixed seed.
///
/// Seed the value from a block position to approximate vanilla's per-position
/// variation. Given the same seed and weights it always returns the same index.
#[derive(Debug, Clone, Copy)]
pub struct SeededWeight(pub u64);

impl WeightSelector for SeededWeight {
    fn select(&self, weights: &[u32]) -> usize {
        let total: u64 = weights.iter().map(|&w| u64::from(w.max(1))).sum();
        if total == 0 {
            return 0;
        }
        // A small deterministic mix of the seed, reduced into [0, total).
        let mut h = self.0.wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(31);
        h ^= h >> 29;
        let mut target = h % total;
        for (i, &w) in weights.iter().enumerate() {
            let w = u64::from(w.max(1));
            if target < w {
                return i;
            }
            target -= w;
        }
        weights.len() - 1
    }
}

// ---------------------------------------------------------------------------
// Pure geometry core
// ---------------------------------------------------------------------------

/// Options controlling how UVs are finalised during baking.
///
/// Defaults to a faithful, zero-inset bake so hand-computed fixtures stay exact.
/// The renderer — which owns mip generation — enables the anti-bleed inset.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BakeOptions {
    /// How far, in **source texels**, to pull each face's UVs toward the sprite
    /// centre. `0.0` (the default) reproduces vanilla base-level UVs exactly.
    ///
    /// This is a per-quad anti-bleed inset. With mipmapping a mip texel spans
    /// several source texels, so a quad whose UVs sit flush against a sprite edge
    /// samples across the atlas boundary into the neighbouring sprite — the
    /// classic voxel "texture bleed" seam, most visible on distant chunks where
    /// high mips dominate.
    ///
    /// Note (verified against decompiled 26.2 `TextureAtlasSprite`): vanilla no
    /// longer applies a per-quad UV-shrink-ratio field — that field is gone. Instead it
    /// pads every sprite in the atlas and computes UVs from the padded interior,
    /// so mips sample replicated gutter pixels rather than the neighbour. The
    /// structurally-faithful equivalent here is [`AtlasBuilder::with_padding`],
    /// which is size-correct across mixed sprite resolutions and keeps a sprite's
    /// full texel range addressable. This per-quad inset remains as a cheap
    /// fallback the renderer can enable (a starting point is `0.5` texel, raised
    /// toward the top mip level); it is texel-proportional, so mixed sprite sizes
    /// each shrink correctly.
    pub uv_inset_texels: f32,
}

impl Default for BakeOptions {
    fn default() -> Self {
        Self {
            uv_inset_texels: 0.0,
        }
    }
}

/// Bakes a resolved model under a placement transform into quads.
///
/// This is the pure geometry stage: it applies element and model rotation to
/// positions, derives or reuses face UVs, applies `uvlock` and face rotation,
/// and maps UVs into the atlas. Faces are emitted in [`DIRECTIONS`] order per
/// element for deterministic output.
///
/// # Errors
///
/// Returns [`BakeError::UnresolvedTexture`] if a face references an unresolved
/// texture variable, or [`BakeError::SpriteMissing`] if a resolved texture has
/// no sprite in `atlas`.
pub fn bake_model(
    model: &ResolvedModel,
    atlas: &Atlas,
    transform: ModelTransform,
) -> Result<Vec<BakedQuad>, BakeError> {
    bake_model_with(model, atlas, transform, &BakeOptions::default())
}

/// Bakes a model like [`bake_model`], with explicit [`BakeOptions`] (e.g. the
/// anti-bleed UV inset the renderer enables for mipmapping).
///
/// # Errors
///
/// Same as [`bake_model`].
pub fn bake_model_with(
    model: &ResolvedModel,
    atlas: &Atlas,
    transform: ModelTransform,
    options: &BakeOptions,
) -> Result<Vec<BakedQuad>, BakeError> {
    let model_rot = model_rotation(transform.x, transform.y);
    let mut quads = Vec::new();
    for element in &model.elements {
        for dir in DIRECTIONS {
            let Some(face) = element.faces.get(&dir) else {
                continue;
            };
            quads.push(bake_face(
                model, atlas, element, dir, face, transform, &model_rot, options,
            )?);
        }
    }
    Ok(quads)
}

/// Bakes a single face into a quad, following `FaceBakery::bakeQuad`.
#[allow(clippy::too_many_arguments)]
fn bake_face(
    model: &ResolvedModel,
    atlas: &Atlas,
    element: &Element,
    dir: Direction,
    face: &Face,
    transform: ModelTransform,
    model_rot: &Affine,
    options: &BakeOptions,
) -> Result<BakedQuad, BakeError> {
    // Resolve the texture and its atlas sprite.
    let location =
        model
            .resolve_texture(&face.texture)
            .ok_or_else(|| BakeError::UnresolvedTexture {
                variable: face.texture.clone(),
            })?;
    let sprite = atlas
        .sprite(location)
        .ok_or_else(|| BakeError::SpriteMissing {
            location: location.to_string(),
        })?;
    // Present by construction: `atlas.sprite(location)` just succeeded, so
    // `atlas.sprite_index(location)` — the same lookup, minus the deref —
    // cannot fail here. `unwrap_or(0)` rather than a second `?` because a
    // missing index is not a condition `BakeError` has a variant for and
    // cannot actually occur; see `Atlas::sprite`/`Atlas::sprite_index`, which
    // share one `HashMap` lookup.
    let sprite_index = atlas.sprite_index(location).unwrap_or(0) as u32;
    let (frame_min, frame_max) = sprite
        .frame_uv(0, atlas.width, atlas.height)
        .unwrap_or((sprite.uv_min, sprite.uv_max));

    // The face UV: explicit, or derived from the element box; then uvlocked.
    let base_uv = face
        .uv
        .unwrap_or_else(|| default_uv(dir, element.from, element.to));
    let mut face_uv = FaceUv {
        uvs: base_uv,
        rotation: face.rotation,
    };
    if transform.uvlock {
        face_uv = recompute_uvs(&face_uv, dir, model_rot);
    }

    // Vertex positions: element box corner -> element rotation -> model rotation.
    let shape = setup_shape(element.from, element.to);
    let elem_rot = element.rotation.as_ref();
    let corners = FACE_INFO[dir.index3d()];
    let mut positions = [[0.0f32; 3]; 4];
    let mut uvs = [[0.0f32; 2]; 4];
    for (i, corner) in corners.iter().enumerate() {
        let mut v = [shape[corner.0], shape[corner.1], shape[corner.2]];
        apply_element_rotation(&mut v, elem_rot);
        apply_model_rotation(&mut v, model_rot);
        positions[i] = v;

        let lu = face_uv.get_u(i) / 16.0;
        let lv = face_uv.get_v(i) / 16.0;
        uvs[i] = [
            lerp(frame_min[0], frame_max[0], lu),
            lerp(frame_min[1], frame_max[1], lv),
        ];
    }

    let direction = calculate_facing(&positions);
    if elem_rot.is_none() {
        recalculate_winding(&mut positions, &mut uvs, direction);
    }

    if options.uv_inset_texels > 0.0 {
        inset_uvs(
            &mut uvs,
            frame_min,
            frame_max,
            sprite.width,
            sprite.frame_height,
            options.uv_inset_texels,
        );
    }

    let cullface = face.cullface.map(|c| rotate_direction(model_rot, c));

    Ok(BakedQuad {
        positions,
        uvs,
        direction,
        cullface,
        tint_index: face.tintindex,
        shade: element.shade.unwrap_or(true),
        layer: sprite.layer,
        anim: sprite.anim_slot,
        sprite: sprite_index,
    })
}

/// Pulls a face's four UVs toward the sprite centre by `texels` source texels
/// (vanilla's own UV-shrink-ratio anti-bleed inset). Texel size is derived per-axis
/// from the sprite's frame rect so mixed sprite sizes each shrink correctly. The
/// shift toward centre is clamped so UVs never cross it.
fn inset_uvs(
    uvs: &mut [[f32; 2]; 4],
    frame_min: [f32; 2],
    frame_max: [f32; 2],
    sprite_width: u32,
    frame_height: u32,
    texels: f32,
) {
    if sprite_width == 0 || frame_height == 0 {
        return;
    }
    let du = (frame_max[0] - frame_min[0]).abs() / sprite_width as f32 * texels;
    let dv = (frame_max[1] - frame_min[1]).abs() / frame_height as f32 * texels;
    let cu = uvs.iter().map(|uv| uv[0]).sum::<f32>() / 4.0;
    let cv = uvs.iter().map(|uv| uv[1]).sum::<f32>() / 4.0;
    for uv in uvs.iter_mut() {
        uv[0] += (cu - uv[0]).clamp(-du, du);
        uv[1] += (cv - uv[1]).clamp(-dv, dv);
    }
}

/// Vanilla `BlockElement::uvsByFace`: default `[u1, v1, u2, v2]` (in 0..16) for
/// a face when the model omits explicit `uv`. `from`/`to` are in 0..16.
fn default_uv(dir: Direction, from: [f32; 3], to: [f32; 3]) -> [f32; 4] {
    let [fx, fy, fz] = from;
    let [tx, ty, tz] = to;
    match dir {
        Direction::Down => [fx, 16.0 - tz, tx, 16.0 - fz],
        Direction::Up => [fx, fz, tx, tz],
        Direction::North => [16.0 - tx, 16.0 - ty, 16.0 - fx, 16.0 - fy],
        Direction::South => [fx, 16.0 - ty, tx, 16.0 - fy],
        Direction::West => [fz, 16.0 - ty, tz, 16.0 - fy],
        Direction::East => [16.0 - tz, 16.0 - ty, 16.0 - fz, 16.0 - fy],
    }
}

/// Vanilla `FaceBakery::setupShape`: `[min_y, max_y, min_z, max_z, min_x, max_x]`
/// in block units, indexed by [`Direction::index3d`]-style face constants.
fn setup_shape(from: [f32; 3], to: [f32; 3]) -> [f32; 6] {
    [
        from[1] / 16.0, // MIN_Y (down)  = 0
        to[1] / 16.0,   // MAX_Y (up)    = 1
        from[2] / 16.0, // MIN_Z (north) = 2
        to[2] / 16.0,   // MAX_Z (south) = 3
        from[0] / 16.0, // MIN_X (west)  = 4
        to[0] / 16.0,   // MAX_X (east)  = 5
    ]
}

/// Per-face vertex corners as `(x_index, y_index, z_index)` into the
/// [`setup_shape`] array, indexed by the face's 3D data value.
const FACE_INFO: [[(usize, usize, usize); 4]; 6] = [
    // DOWN (0)
    [(4, 0, 3), (4, 0, 2), (5, 0, 2), (5, 0, 3)],
    // UP (1)
    [(4, 1, 2), (4, 1, 3), (5, 1, 3), (5, 1, 2)],
    // NORTH (2)
    [(5, 1, 2), (5, 0, 2), (4, 0, 2), (4, 1, 2)],
    // SOUTH (3)
    [(4, 1, 3), (4, 0, 3), (5, 0, 3), (5, 1, 3)],
    // WEST (4)
    [(4, 1, 2), (4, 0, 2), (4, 0, 3), (4, 1, 3)],
    // EAST (5)
    [(5, 1, 3), (5, 0, 3), (5, 0, 2), (5, 1, 2)],
];

/// Vanilla `FaceBakery::applyElementRotation`: rotate a vertex about the
/// element's rotation origin. The classic single-axis form uses the rescale
/// trick; the Euler form (hanging signs) applies the three angles in order
/// without rescale (those models render as block entities, so exact parity is
/// not required here).
fn apply_element_rotation(v: &mut [f32; 3], rot: Option<&ElementRotation>) {
    let Some(rot) = rot else {
        return;
    };
    let origin = [
        rot.origin[0] / 16.0,
        rot.origin[1] / 16.0,
        rot.origin[2] / 16.0,
    ];

    if let Some((axis, angle_deg)) = rot.single_axis() {
        let angle = angle_deg.to_radians();
        let mat = axis_rotation(axis, angle);
        let mut rescale = [1.0f32, 1.0, 1.0];
        if rot.rescale {
            let factor = if angle_deg.abs() == 22.5 {
                RESCALE_22_5
            } else {
                RESCALE_45
            };
            // Rescale the two axes perpendicular to the rotation axis.
            match axis {
                crate::model::Axis::X => rescale = [1.0, 1.0 + factor, 1.0 + factor],
                crate::model::Axis::Y => rescale = [1.0 + factor, 1.0, 1.0 + factor],
                crate::model::Axis::Z => rescale = [1.0 + factor, 1.0 + factor, 1.0],
            }
        }
        rotate_vertex_by(v, origin, &mat, rescale);
    } else {
        // General Euler rotation: apply X, then Y, then Z about the origin.
        let mat = mat3_mul(
            mat3_rot_z(rot.angles[2].to_radians()),
            mat3_mul(
                mat3_rot_y(rot.angles[1].to_radians()),
                mat3_rot_x(rot.angles[0].to_radians()),
            ),
        );
        rotate_vertex_by(v, origin, &Affine::from_rot(mat), [1.0, 1.0, 1.0]);
    }
}

/// Vanilla `FaceBakery::applyModelRotation`: rotate a vertex about the block
/// centre `(0.5, 0.5, 0.5)` with no rescale.
fn apply_model_rotation(v: &mut [f32; 3], model_rot: &Affine) {
    if model_rot.is_identity() {
        return;
    }
    rotate_vertex_by(v, [0.5, 0.5, 0.5], model_rot, [1.0, 1.0, 1.0]);
}

/// Vanilla `FaceBakery::rotateVertexBy`: translate to `origin`, apply the
/// rotation, scale by `rescale`, translate back.
fn rotate_vertex_by(v: &mut [f32; 3], origin: [f32; 3], mat: &Affine, rescale: [f32; 3]) {
    let local = [v[0] - origin[0], v[1] - origin[1], v[2] - origin[2]];
    let r = mat.transform_vec(local);
    v[0] = r[0] * rescale[0] + origin[0];
    v[1] = r[1] * rescale[1] + origin[1];
    v[2] = r[2] * rescale[2] + origin[2];
}

/// Vanilla `FaceBakery::calculateFacing`: the axis direction most aligned with
/// the quad's geometric normal.
fn calculate_facing(positions: &[[f32; 3]; 4]) -> Direction {
    let a = sub(positions[0], positions[1]);
    let b = sub(positions[2], positions[1]);
    let normal = cross(b, a);
    if !normal.iter().all(|c| c.is_finite()) || normal == [0.0, 0.0, 0.0] {
        return Direction::Up;
    }
    let mut best = Direction::Up;
    let mut best_dot = 0.0f32;
    for dir in DIRECTIONS {
        let d = dot(normal, dir.unit_vec());
        if d >= 0.0 && d > best_dot {
            best_dot = d;
            best = dir;
        }
    }
    best
}

/// Vanilla `FaceBakery::recalculateWinding`: re-canonicalise vertex order (and
/// carry each vertex's UV with it) to match `facing`'s corner layout. Only run
/// when the element has no rotation.
fn recalculate_winding(positions: &mut [[f32; 3]; 4], uvs: &mut [[f32; 2]; 4], facing: Direction) {
    let orig_pos = *positions;
    let orig_uv = *uvs;

    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for p in &orig_pos {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    // bbox indexed like setup_shape: [min_y, max_y, min_z, max_z, min_x, max_x].
    let bbox = [lo[1], hi[1], lo[2], hi[2], lo[0], hi[0]];
    let corners = FACE_INFO[facing.index3d()];
    for (i, corner) in corners.iter().enumerate() {
        let p = [bbox[corner.0], bbox[corner.1], bbox[corner.2]];
        positions[i] = p;
        for (k, op) in orig_pos.iter().enumerate() {
            if approx_eq(p[0], op[0]) && approx_eq(p[1], op[1]) && approx_eq(p[2], op[2]) {
                uvs[i] = orig_uv[k];
            }
        }
    }
}

// ---------------------------------------------------------------------------
// UV handling (BlockFaceUV + uvlock)
// ---------------------------------------------------------------------------

/// A face's UV rect (`[u1, v1, u2, v2]` in 0..16) plus a `rotation` of
/// 0/90/180/270 degrees, mirroring vanilla `BlockFaceUV`.
#[derive(Debug, Clone, Copy)]
struct FaceUv {
    uvs: [f32; 4],
    rotation: i32,
}

impl FaceUv {
    fn shifted_index(&self, vertex: usize) -> usize {
        (vertex + (self.rotation / 90) as usize) % 4
    }

    fn reverse_index(&self, vertex: usize) -> usize {
        (vertex + 4 - (self.rotation / 90) as usize) % 4
    }

    fn get_u(&self, vertex: usize) -> f32 {
        let i = self.shifted_index(vertex);
        self.uvs[if i != 0 && i != 1 { 2 } else { 0 }]
    }

    fn get_v(&self, vertex: usize) -> f32 {
        let i = self.shifted_index(vertex);
        self.uvs[if i != 0 && i != 3 { 3 } else { 1 }]
    }

    fn u_at(&self, index: usize) -> f32 {
        self.uvs[if index != 0 && index != 1 { 2 } else { 0 }]
    }

    fn v_at(&self, index: usize) -> f32 {
        self.uvs[if index != 0 && index != 3 { 3 } else { 1 }]
    }
}

/// Vanilla `FaceBakery::recomputeUVs`: transform the face UV rect through the
/// uvlock transform so it stays world-aligned under the model rotation.
fn recompute_uvs(face_uv: &FaceUv, dir: Direction, model_rot: &Affine) -> FaceUv {
    let lock = uv_lock_transform(model_rot, dir);

    let ri0 = face_uv.reverse_index(0);
    let u0 = face_uv.u_at(ri0);
    let v0 = face_uv.v_at(ri0);
    let p0 = lock.transform_point([u0 / 16.0, v0 / 16.0, 0.0]);
    let (nu0, nv0) = (16.0 * p0[0], 16.0 * p0[1]);

    let ri2 = face_uv.reverse_index(2);
    let u2 = face_uv.u_at(ri2);
    let v2 = face_uv.v_at(ri2);
    let p2 = lock.transform_point([u2 / 16.0, v2 / 16.0, 0.0]);
    let (nu2, nv2) = (16.0 * p2[0], 16.0 * p2[1]);

    let (out_u0, out_u2) = if signum(u2 - u0) == signum(nu2 - nu0) {
        (nu0, nu2)
    } else {
        (nu2, nu0)
    };
    let (out_v0, out_v2) = if signum(v2 - v0) == signum(nv2 - nv0) {
        (nv0, nv2)
    } else {
        (nv2, nv0)
    };

    let angle = (face_uv.rotation as f32).to_radians();
    let dir_vec = mat3_mul_vec(lock.rot, [angle.cos(), angle.sin(), 0.0]);
    let deg = dir_vec[1].atan2(dir_vec[0]).to_degrees();
    let rotation = (-((deg / 90.0).round() as i32) * 90).rem_euclid(360);

    FaceUv {
        uvs: [out_u0, out_v0, out_u2, out_v2],
        rotation,
    }
}

/// Vanilla `BlockMath::getUVLockTransform`.
fn uv_lock_transform(model_rot: &Affine, dir: Direction) -> Affine {
    let rotated = rotate_direction(model_rot, dir);
    let inv = model_rot.inverse_rigid();
    let t = uv_global_to_local(dir)
        .mul(&inv)
        .mul(&uv_local_to_global(rotated));
    block_center_to_corner(&t)
}

/// Vanilla's own block-math local-to-global UV transform constant.
fn uv_local_to_global(dir: Direction) -> Affine {
    use std::f32::consts::PI;
    let rot = match dir {
        Direction::South => Mat3::IDENTITY,
        Direction::East => mat3_rot_y(PI / 2.0),
        Direction::West => mat3_rot_y(-PI / 2.0),
        Direction::North => mat3_rot_y(PI),
        Direction::Up => mat3_rot_x(-PI / 2.0),
        Direction::Down => mat3_rot_x(PI / 2.0),
    };
    Affine::from_rot(rot)
}

/// Vanilla's own block-math global-to-local UV transform constant (inverse of local-to-global).
fn uv_global_to_local(dir: Direction) -> Affine {
    uv_local_to_global(dir).inverse_rigid()
}

/// `BlockMath::blockCenterToCorner`: `T(+0.5) * transform * T(-0.5)`.
fn block_center_to_corner(t: &Affine) -> Affine {
    Affine::translation([0.5, 0.5, 0.5])
        .mul(t)
        .mul(&Affine::translation([-0.5, -0.5, -0.5]))
}

// ---------------------------------------------------------------------------
// Direction helpers
// ---------------------------------------------------------------------------

impl Direction {
    /// The vanilla 3D data value (`down`=0, `up`=1, `north`=2, `south`=3,
    /// `west`=4, `east`=5), used to index face tables.
    const fn index3d(self) -> usize {
        match self {
            Direction::Down => 0,
            Direction::Up => 1,
            Direction::North => 2,
            Direction::South => 3,
            Direction::West => 4,
            Direction::East => 5,
        }
    }

    /// The unit normal vector of this direction.
    const fn unit_vec(self) -> [f32; 3] {
        match self {
            Direction::Down => [0.0, -1.0, 0.0],
            Direction::Up => [0.0, 1.0, 0.0],
            Direction::North => [0.0, 0.0, -1.0],
            Direction::South => [0.0, 0.0, 1.0],
            Direction::West => [-1.0, 0.0, 0.0],
            Direction::East => [1.0, 0.0, 0.0],
        }
    }
}

/// Vanilla `Direction::rotate`: the axis direction nearest the model-rotated
/// unit vector. Used to rotate a face's `cullface` with the model.
fn rotate_direction(model_rot: &Affine, dir: Direction) -> Direction {
    let v = model_rot.transform_vec(dir.unit_vec());
    let mut best = Direction::North;
    let mut best_dot = f32::NEG_INFINITY;
    for d in DIRECTIONS {
        let dp = dot(v, d.unit_vec());
        if dp > best_dot {
            best_dot = dp;
            best = d;
        }
    }
    best
}

// ---------------------------------------------------------------------------
// Minimal linear algebra (right-handed, column-vector, matching JOML)
// ---------------------------------------------------------------------------

const RESCALE_22_5: f32 = 0.417_119_3; // 1 / cos(22.5deg) - 1
const RESCALE_45: f32 = 0.414_213_57; // 1 / cos(45deg) - 1

/// A 3x3 rotation matrix (row-major, `v' = M * v`).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Mat3 {
    m: [[f32; 3]; 3],
}

impl Mat3 {
    const IDENTITY: Mat3 = Mat3 {
        m: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
    };
}

fn mat3_mul_vec(a: Mat3, v: [f32; 3]) -> [f32; 3] {
    [
        a.m[0][0] * v[0] + a.m[0][1] * v[1] + a.m[0][2] * v[2],
        a.m[1][0] * v[0] + a.m[1][1] * v[1] + a.m[1][2] * v[2],
        a.m[2][0] * v[0] + a.m[2][1] * v[1] + a.m[2][2] * v[2],
    ]
}

fn mat3_mul(a: Mat3, b: Mat3) -> Mat3 {
    let mut m = [[0.0f32; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a.m[i][0] * b.m[0][j] + a.m[i][1] * b.m[1][j] + a.m[i][2] * b.m[2][j];
        }
    }
    Mat3 { m }
}

fn mat3_transpose(a: Mat3) -> Mat3 {
    let mut m = [[0.0f32; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, cell) in row.iter_mut().enumerate() {
            *cell = a.m[j][i];
        }
    }
    Mat3 { m }
}

fn mat3_rot_x(a: f32) -> Mat3 {
    let (s, c) = a.sin_cos();
    Mat3 {
        m: [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]],
    }
}

fn mat3_rot_y(a: f32) -> Mat3 {
    let (s, c) = a.sin_cos();
    Mat3 {
        m: [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]],
    }
}

fn mat3_rot_z(a: f32) -> Mat3 {
    let (s, c) = a.sin_cos();
    Mat3 {
        m: [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]],
    }
}

/// The block model rotation `Ry(-y) * Rx(-x)` (vanilla `BlockModelRotation`
/// builds `rotateYXZ(-y, -x, 0)`).
fn model_rotation(x_deg: i32, y_deg: i32) -> Affine {
    let rx = mat3_rot_x(-(x_deg as f32).to_radians());
    let ry = mat3_rot_y(-(y_deg as f32).to_radians());
    Affine::from_rot(mat3_mul(ry, rx))
}

fn axis_rotation(axis: crate::model::Axis, angle: f32) -> Affine {
    let rot = match axis {
        crate::model::Axis::X => mat3_rot_x(angle),
        crate::model::Axis::Y => mat3_rot_y(angle),
        crate::model::Axis::Z => mat3_rot_z(angle),
    };
    Affine::from_rot(rot)
}

/// A rigid affine transform: rotation then translation (`v' = R*v + t`).
#[derive(Debug, Clone, Copy, PartialEq)]
struct Affine {
    rot: Mat3,
    t: [f32; 3],
}

impl Affine {
    fn from_rot(rot: Mat3) -> Self {
        Self { rot, t: [0.0; 3] }
    }

    fn translation(t: [f32; 3]) -> Self {
        Self {
            rot: Mat3::IDENTITY,
            t,
        }
    }

    fn is_identity(&self) -> bool {
        self.rot == Mat3::IDENTITY && self.t == [0.0, 0.0, 0.0]
    }

    /// `self ∘ other`: apply `other` first, then `self`.
    fn mul(&self, other: &Affine) -> Affine {
        Affine {
            rot: mat3_mul(self.rot, other.rot),
            t: add(mat3_mul_vec(self.rot, other.t), self.t),
        }
    }

    /// Inverse of a rigid transform: `R^T` and `-R^T * t`.
    fn inverse_rigid(&self) -> Affine {
        let rt = mat3_transpose(self.rot);
        let t = mat3_mul_vec(rt, self.t);
        Affine {
            rot: rt,
            t: [-t[0], -t[1], -t[2]],
        }
    }

    fn transform_point(&self, v: [f32; 3]) -> [f32; 3] {
        add(mat3_mul_vec(self.rot, v), self.t)
    }

    fn transform_vec(&self, v: [f32; 3]) -> [f32; 3] {
        mat3_mul_vec(self.rot, v)
    }
}

fn add(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

fn sub(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross(a: [f32; 3], b: [f32; 3]) -> [f32; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn dot(a: [f32; 3], b: [f32; 3]) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

fn lerp(a: f32, b: f32, t: f32) -> f32 {
    a + (b - a) * t
}

fn signum(x: f32) -> f32 {
    if x > 0.0 {
        1.0
    } else if x < 0.0 {
        -1.0
    } else {
        0.0
    }
}

fn approx_eq(a: f32, b: f32) -> bool {
    (a - b).abs() < 1.0e-5
}

// ---------------------------------------------------------------------------
// Top-level block baker
// ---------------------------------------------------------------------------

/// Bakes whole blocks: blockstate selection, model resolution and geometry
/// baking in one call.
///
/// Holds borrowed references to the pack stack, a model resolver (whose cache it
/// reuses), and the atlas the textures were stitched into.
#[derive(Debug)]
pub struct BlockBaker<'a> {
    manager: &'a ResourceManager,
    resolver: &'a ModelResolver<'a>,
    atlas: &'a Atlas,
}

impl<'a> BlockBaker<'a> {
    /// Creates a baker over a pack stack, model resolver and atlas.
    #[must_use]
    pub fn new(
        manager: &'a ResourceManager,
        resolver: &'a ModelResolver<'a>,
        atlas: &'a Atlas,
    ) -> Self {
        Self {
            manager,
            resolver,
            atlas,
        }
    }

    /// Bakes a block in a specific property state.
    ///
    /// Reads and parses the block's blockstate, selects the applicable
    /// variant/multipart models for `properties`, collapses each weighted list
    /// with `selector`, resolves and bakes each model, and unions the quads.
    ///
    /// # Errors
    ///
    /// Returns a [`BakeError`] if the blockstate is missing/invalid, a model
    /// fails to resolve, or a face's texture is unresolved or has no sprite.
    pub fn bake_block(
        &self,
        block: &ResourceLocation,
        properties: &BTreeMap<String, String>,
        selector: &dyn WeightSelector,
    ) -> Result<BakedModel, BakeError> {
        let bytes = self
            .manager
            .read_asset(block, "blockstates", "json")
            .ok_or_else(|| BakeError::Blockstate {
                block: block.to_string(),
                reason: "blockstate file not found".to_string(),
            })?;
        let states = BlockStates::parse(&bytes).map_err(|e| BakeError::Blockstate {
            block: block.to_string(),
            reason: e.to_string(),
        })?;

        let mut quads = Vec::new();
        let mut particle_uv = None;
        // `parts.getFirst()` in vanilla — see `BakedModel::ambient_occlusion`.
        // `true` is the correct value when a state bakes no parts at all (no
        // quads follow, so it is never read), and matches the JSON default for
        // the (overwhelmingly common) single-part case.
        let mut ambient_occlusion = true;
        let mut ambient_occlusion_set = false;
        for group in states.applicable_models(properties) {
            if group.is_empty() {
                continue;
            }
            let weights: Vec<u32> = group.iter().map(|r| r.weight).collect();
            let idx = selector.select(&weights).min(group.len() - 1);
            let model_ref = &group[idx];
            let resolved =
                self.resolver
                    .resolve(&model_ref.model)
                    .map_err(|e| BakeError::Model {
                        location: model_ref.model.to_string(),
                        source: e,
                    })?;
            // Vanilla takes the particle icon from the *first* model it bakes
            // for a state (multipart blocks contribute several); keep the first
            // that resolves so a fence post's particle isn't overwritten by a
            // side piece's.
            if particle_uv.is_none() {
                particle_uv = resolved
                    .resolve_texture("particle")
                    .and_then(|loc| self.atlas.sprite(loc))
                    .map(|sprite| {
                        // Frame 0 of an animated sprite, matching how a face
                        // bakes: the full sprite rect would span every frame.
                        let (min, max) = sprite
                            .frame_uv(0, self.atlas.width, self.atlas.height)
                            .unwrap_or((sprite.uv_min, sprite.uv_max));
                        [min[0], min[1], max[0], max[1]]
                    });
            }
            if !ambient_occlusion_set {
                ambient_occlusion = resolved.ambient_occlusion;
                ambient_occlusion_set = true;
            }
            let transform = ModelTransform::from_model_ref(model_ref);
            quads.extend(bake_model(&resolved, self.atlas, transform)?);
        }
        Ok(BakedModel {
            quads,
            particle_uv,
            ambient_occlusion,
        })
    }

    /// Bakes a block from a numeric block state id via a [`BlockStateRegistry`].
    ///
    /// # Errors
    ///
    /// Returns [`BakeError::UnknownState`] if the id is not in `registry`, or
    /// any error from [`bake_block`](Self::bake_block).
    pub fn bake_state(
        &self,
        registry: &dyn BlockStateRegistry,
        id: u32,
        selector: &dyn WeightSelector,
    ) -> Result<BakedModel, BakeError> {
        let resolved = registry.resolve(id).ok_or(BakeError::UnknownState { id })?;
        let block = block_location(resolved.block).map_err(|e| BakeError::Blockstate {
            block: resolved.block.to_string(),
            reason: e.to_string(),
        })?;
        self.bake_block(&block, resolved.properties, selector)
    }
}

/// Converts a version-free [`Identifier`] into an asset [`ResourceLocation`].
fn block_location(
    id: &Identifier,
) -> Result<ResourceLocation, crate::error::ResourceLocationError> {
    ResourceLocation::new(id.namespace(), id.path())
}
