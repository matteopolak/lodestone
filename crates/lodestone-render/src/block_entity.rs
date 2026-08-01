//! Block-entity renderers: the cuboid rigs vanilla's `BlockEntityRenderer`s
//! draw for blocks whose block model does not describe them (issue #23).
//!
//! Chest today. The module is shaped so a second type is an entry in
//! [`lodestone_assets::block_entity_models::BLOCK_ENTITY_MODELS`] plus a
//! `*_spawns → BlockEntityInstance` resolver, not a new pipeline.
//!
//! # Why this is not `crate::entity`, when it shares every primitive
//!
//! The bake is identical — vanilla's `ModelPart` has no idea whether its owner
//! is a mob or a chest, and this module reuses `CubeDef`/`PartDef`/`bake_entity_parts`
//! and [`crate::entity`]'s winding rule verbatim. **Placement is the difference,
//! and it is total:**
//!
//! | | entity | block entity |
//! |---|---|---|
//! | model space | Y-**down** | Y-**up** |
//! | placement | `entity_model_matrix`: `translate(feet) · rotY(180°−yaw) · scale(−s,−s,s) · translate(0,−1.501,0)` | [`block_entity_placement_matrix`]: `translate(pos) · rotateAround(−yaw, ½,0,½)` |
//! | anchor | the entity's feet | the block's corner |
//!
//! `ChestRenderer.submit`'s *entire* prologue is
//! `Matrix4f().rotationAround(Axis.YP.rotationDegrees(-facing.toYRot()), 0.5F, 0.0F, 0.5F)`
//! — no flip and no lift, because the chest's own texels are already block-space:
//! `bottom` spans y `0..10` texels (`0..0.625` blocks off the floor) and the
//! `lid` pivot at y `9` puts the closed lid's top at `14/16`, the real chest
//! height. Feeding a chest through the entity matrix buries it 1.5 blocks down,
//! upside down. `placement_does_not_flip_or_lift` is the assertion that catches
//! that, and it compares against a real matrix rather than restating a constant.
//!
//! # Determinant, and why there is nothing to get backwards here
//!
//! `CLAUDE.md`'s winding rule says to derive the front-facing sign from a real
//! camera rather than asserting a polarity. That warning applies where a
//! *handedness flip* is in play — the GUI item pose, and the entity path's
//! `scale(−1,−1,1)`. This placement matrix is a translation composed with a
//! rotation: `det = +1` exactly, for every facing, so it cannot reverse winding
//! and the quads' baked outward normals reach the rasteriser unchanged.
//! `placement_preserves_orientation` measures the determinant rather than
//! asserting it is "positive because rotations are".
//!
//! # The lid animation, and the two easings that are not the same one
//!
//! Vanilla applies **two** transforms to the raw openness and they live in
//! different classes, which is exactly the kind of thing a summary loses:
//!
//! 1. `ChestRenderer.submit` eases the *progress*:
//!    `open = 1 - open; open = 1 - open*open*open` — a cubic ease-out
//!    ([`chest_lid_openness`]).
//! 2. `ChestModel.setupAnim` turns the eased value into an *angle*:
//!    `lid.xRot = -(open * PI/2)`, and then `lock.xRot = lid.xRot`
//!    ([`chest_lid_x_rot`]).
//!
//! Collapsing these into one function that takes raw openness and returns an
//! angle would still look right at the two endpoints (0 and 1 are fixed points
//! of the ease) and be wrong for every frame in between — an animation bug that
//! a screenshot at rest cannot see. They are separate, and both are unit-tested
//! against the endpoints *and* the midpoint.
//!
//! # How to change it
//!
//! * A new block-entity type needs: a model in `lodestone-assets`, a texture-stem
//!   resolver here (see [`chest_texture_stem`]), a `*Spawn` input struct, and an
//!   arm in the shell's prepare. It does **not** need a new pipeline — everything
//!   here draws through [`crate::entity_pipeline::EntityPipeline`], which spends
//!   exactly two bind groups (camera+fog / texture) and so leaves the model
//!   shader's 4-group floor alone.
//! * Part names are the animation's only handle. [`BlockEntityMesh::index_of`]
//!   resolves `"lid"`/`"lock"` by name; renaming either in the asset corpus
//!   silently freezes the lid shut (the mesh still draws, so a coverage-only gate
//!   stays green). `lodestone-assets`'
//!   `lid_and_lock_share_the_pivot_the_animation_rotates_about` is the guard.
//! * [`chest_texture_stems`] is what the shell preloads. A material added to
//!   [`ChestMaterial`] and *not* to that list resolves to a stem with no bind
//!   group, and the shell falls back — visible, but wrong. Both are derived from
//!   the same match, so add the arm and the list entry together.

use glam::{Mat4, Vec3};
use lodestone_assets::block_entity_models::{BLOCK_ENTITY_MODELS, BlockEntityModelEntry};
use lodestone_assets::entity::{EntityModelDef, PartPose, bake_entity_parts};

use crate::camera::Frustum;
use crate::entity::{ENTITY_FULLBRIGHT, PartRange, push_part_quads};
use crate::models::ModelVertex;

/// Model name of a single chest, keying both the mesh set and the shell's
/// texture map.
pub const CHEST_SINGLE: &str = "chest";
/// Model name of a double chest's left half.
pub const CHEST_LEFT: &str = "chest_left";
/// Model name of a double chest's right half.
pub const CHEST_RIGHT: &str = "chest_right";

/// Which of vanilla's three chest *layers* an instance draws.
///
/// Not a pose of one layer: the halves are 15 texels wide against the single
/// chest's 14 and each omits the seam face, so this selects a different mesh.
/// Mirrors `ChestType` (`SINGLE`/`LEFT`/`RIGHT`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChestHalf {
    /// A lone chest.
    Single,
    /// The left half of a double chest.
    Left,
    /// The right half of a double chest.
    Right,
}

impl ChestHalf {
    /// The model name this half draws.
    #[must_use]
    pub const fn model(self) -> &'static str {
        match self {
            ChestHalf::Single => CHEST_SINGLE,
            ChestHalf::Left => CHEST_LEFT,
            ChestHalf::Right => CHEST_RIGHT,
        }
    }

    /// Parses vanilla's `type` block-state property value.
    ///
    /// A chest state always has `type`; anything unrecognised (a future value,
    /// a datapack block reusing the block entity) degrades to
    /// [`ChestHalf::Single`], which draws a complete chest rather than a
    /// half-open shell with a hole in it.
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "left" => ChestHalf::Left,
            "right" => ChestHalf::Right,
            _ => ChestHalf::Single,
        }
    }
}

/// Which chest sheet an instance draws with.
///
/// Mirrors `ChestRenderState.ChestMaterialType` (via `Sheets.chooseSprite`).
/// Copper's four weathering stages are separate arms rather than a nested enum
/// so [`chest_texture_stem`] stays one flat match with [`chest_texture_stems`]
/// derived from the same set of arms.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ChestMaterial {
    /// `minecraft:chest`.
    Regular,
    /// `minecraft:trapped_chest`.
    Trapped,
    /// `minecraft:ender_chest` — one sheet, no left/right variants.
    Ender,
    /// The seasonal override (`SpecialDates.isExtendedChristmas()`).
    Christmas,
    /// `minecraft:copper_chest`, unaffected.
    CopperUnaffected,
    /// `minecraft:exposed_copper_chest`.
    CopperExposed,
    /// `minecraft:weathered_copper_chest`.
    CopperWeathered,
    /// `minecraft:oxidized_copper_chest`.
    CopperOxidized,
}

impl ChestMaterial {
    /// Resolves a block's registry path (namespace stripped) to its chest
    /// material, or `None` if the path is not a chest at all.
    ///
    /// This is the *block*-driven half of `ChestRenderer.getChestMaterial`; the
    /// seasonal [`ChestMaterial::Christmas`] override is date-driven and belongs
    /// to the caller (see [`chest_material_with_season`]).
    #[must_use]
    pub fn from_block_path(path: &str) -> Option<Self> {
        Some(match path {
            "chest" => ChestMaterial::Regular,
            "trapped_chest" => ChestMaterial::Trapped,
            "ender_chest" => ChestMaterial::Ender,
            "copper_chest" => ChestMaterial::CopperUnaffected,
            "exposed_copper_chest" => ChestMaterial::CopperExposed,
            "weathered_copper_chest" => ChestMaterial::CopperWeathered,
            "oxidized_copper_chest" => ChestMaterial::CopperOxidized,
            _ => return None,
        })
    }
}

/// Applies vanilla's christmas override to a block-derived material.
///
/// `ChestRenderer.getChestMaterial` checks copper **first**, then ender, and
/// only then the seasonal flag — so a copper or ender chest keeps its own sheet
/// in December while a plain or trapped chest does not. Ordering this the
/// obvious way (season first) would repaint every copper chest for two weeks a
/// year, which is precisely the kind of defect nobody sees until December.
#[must_use]
pub fn chest_material_with_season(material: ChestMaterial, christmas: bool) -> ChestMaterial {
    if !christmas {
        return material;
    }
    match material {
        ChestMaterial::Regular | ChestMaterial::Trapped => ChestMaterial::Christmas,
        other => other,
    }
}

/// The jar texture stem (no `assets/<ns>/textures/` prefix, no `.png`) for a
/// material/half pair — `Sheets.chooseSprite` plus
/// `ChestSpecialRenderer.createDefaultTextures`'s `<prefix>`/`<prefix>_left`/
/// `<prefix>_right` naming.
///
/// **Ender is deliberately half-independent.** `Sheets.chooseSprite` returns the
/// single `ENDER_CHEST_LOCATION` for every `ChestType`, and the jar ships only
/// `entity/chest/ender.png` — no `ender_left`/`ender_right` exist. Deriving the
/// suffix uniformly would name a file that is not there and the chest would fall
/// back to a placeholder sheet, which reads as "the renderer is broken" rather
/// than "one texture is missing".
#[must_use]
pub const fn chest_texture_stem(material: ChestMaterial, half: ChestHalf) -> &'static str {
    match (material, half) {
        (ChestMaterial::Ender, _) => "entity/chest/ender",
        (ChestMaterial::Regular, ChestHalf::Single) => "entity/chest/normal",
        (ChestMaterial::Regular, ChestHalf::Left) => "entity/chest/normal_left",
        (ChestMaterial::Regular, ChestHalf::Right) => "entity/chest/normal_right",
        (ChestMaterial::Trapped, ChestHalf::Single) => "entity/chest/trapped",
        (ChestMaterial::Trapped, ChestHalf::Left) => "entity/chest/trapped_left",
        (ChestMaterial::Trapped, ChestHalf::Right) => "entity/chest/trapped_right",
        (ChestMaterial::Christmas, ChestHalf::Single) => "entity/chest/christmas",
        (ChestMaterial::Christmas, ChestHalf::Left) => "entity/chest/christmas_left",
        (ChestMaterial::Christmas, ChestHalf::Right) => "entity/chest/christmas_right",
        (ChestMaterial::CopperUnaffected, ChestHalf::Single) => "entity/chest/copper",
        (ChestMaterial::CopperUnaffected, ChestHalf::Left) => "entity/chest/copper_left",
        (ChestMaterial::CopperUnaffected, ChestHalf::Right) => "entity/chest/copper_right",
        (ChestMaterial::CopperExposed, ChestHalf::Single) => "entity/chest/copper_exposed",
        (ChestMaterial::CopperExposed, ChestHalf::Left) => "entity/chest/copper_exposed_left",
        (ChestMaterial::CopperExposed, ChestHalf::Right) => "entity/chest/copper_exposed_right",
        (ChestMaterial::CopperWeathered, ChestHalf::Single) => "entity/chest/copper_weathered",
        (ChestMaterial::CopperWeathered, ChestHalf::Left) => "entity/chest/copper_weathered_left",
        (ChestMaterial::CopperWeathered, ChestHalf::Right) => "entity/chest/copper_weathered_right",
        (ChestMaterial::CopperOxidized, ChestHalf::Single) => "entity/chest/copper_oxidized",
        (ChestMaterial::CopperOxidized, ChestHalf::Left) => "entity/chest/copper_oxidized_left",
        (ChestMaterial::CopperOxidized, ChestHalf::Right) => "entity/chest/copper_oxidized_right",
    }
}

/// Every material, for enumerating stems and for exhaustiveness in tests.
pub const CHEST_MATERIALS: &[ChestMaterial] = &[
    ChestMaterial::Regular,
    ChestMaterial::Trapped,
    ChestMaterial::Ender,
    ChestMaterial::Christmas,
    ChestMaterial::CopperUnaffected,
    ChestMaterial::CopperExposed,
    ChestMaterial::CopperWeathered,
    ChestMaterial::CopperOxidized,
];

/// Every chest sheet stem the renderer can ask for, deduplicated — what the
/// shell preloads into bind groups.
///
/// **Derived from [`chest_texture_stem`], never hand-listed.** A hand list is
/// how a material silently ends up with no bind group: the match compiles, the
/// list looks complete, and one chest in the world draws a placeholder.
#[must_use]
pub fn chest_texture_stems() -> Vec<&'static str> {
    let mut out = Vec::new();
    for material in CHEST_MATERIALS {
        for half in [ChestHalf::Single, ChestHalf::Left, ChestHalf::Right] {
            let stem = chest_texture_stem(*material, half);
            if !out.contains(&stem) {
                out.push(stem);
            }
        }
    }
    out
}

/// Vanilla's cubic ease-out on a chest's raw openness
/// (`ChestRenderer.submit`: `open = 1 - open; open = 1 - open³`).
///
/// `0 → 0`, `1 → 1`, and `0.5 → 0.875` — noticeably *ahead* of linear, which is
/// what makes a chest snap open and settle. See the module doc on why this is
/// separate from [`chest_lid_x_rot`].
#[must_use]
pub fn chest_lid_openness(raw: f32) -> f32 {
    let inverted = 1.0 - raw.clamp(0.0, 1.0);
    1.0 - inverted * inverted * inverted
}

/// The lid's (and lock's) X rotation in radians for an **already eased**
/// openness — `ChestModel.setupAnim`: `lid.xRot = -(open * PI/2)`.
///
/// Negative: the lid tips backwards, away from the chest's facing.
#[must_use]
pub fn chest_lid_x_rot(eased_openness: f32) -> f32 {
    -(eased_openness * std::f32::consts::FRAC_PI_2)
}

/// The world placement transform for a block entity at `pos` facing
/// `facing_yaw_deg`.
///
/// `Matrix4f().rotationAround(Axis.YP.rotationDegrees(-facing.toYRot()), 0.5F, 0.0F, 0.5F)`
/// composed with the block's own translation. `facing_yaw_deg` is Minecraft's
/// `Direction.toYRot()` (south `0`, west `90`, north `180`, east `270`) — the
/// **same convention** the entity path's `body_yaw_deg` uses, so a caller does
/// not have to remember two.
///
/// No Y flip and no feet lift; see the module doc's table for why that is the
/// whole difference from [`crate::entity::entity_model_matrix`].
#[must_use]
pub fn block_entity_placement_matrix(pos: [i32; 3], facing_yaw_deg: f32) -> Mat4 {
    let origin = Vec3::new(pos[0] as f32, pos[1] as f32, pos[2] as f32);
    let pivot = Vec3::new(0.5, 0.0, 0.5);
    Mat4::from_translation(origin + pivot)
        * Mat4::from_rotation_y(-facing_yaw_deg.to_radians())
        * Mat4::from_translation(-pivot)
}

/// `Direction.toYRot()` for vanilla's four horizontal facing names, or `None`
/// for a value that is not a horizontal direction.
///
/// South is `0` because that is what vanilla's `Direction` returns
/// (`Direction.SOUTH.toYRot() == 0`), not because it is the natural choice —
/// reading this off `Direction`'s declaration order instead gives
/// down/up/north/south/west/east and rotates every chest by a quarter turn.
#[must_use]
pub fn horizontal_facing_yaw(name: &str) -> Option<f32> {
    Some(match name {
        "south" => 0.0,
        "west" => 90.0,
        "north" => 180.0,
        "east" => 270.0,
        _ => return None,
    })
}

/// A CPU block-entity mesh: part-local vertices plus the part hierarchy needed
/// to rebuild transforms with per-part overrides each frame.
///
/// The hierarchy is kept here rather than in a [`crate::entity_anim::Skeleton`]
/// because `Skeleton` animates by *slot* — `head`, `right_arm`, the limb table —
/// and classifies anything without those names as `AnimFamily::Static`. A chest
/// is `Static` by that rule and would pose with a permanently shut lid. What a
/// block entity needs instead is a direct per-part pose override, which is a
/// different (and much smaller) mechanism, so it lives here.
#[derive(Debug, Clone)]
pub struct BlockEntityMesh {
    /// Four vertices per quad, part-local (no pose folded in).
    pub vertices: Vec<ModelVertex>,
    /// Six indices per quad, wound from each quad's baked outward normal.
    pub indices: Vec<u32>,
    /// One index sub-range per part, in bake (pre-order) order.
    pub parts: Vec<PartRange>,
    /// Part names, parallel to `parts`.
    pub part_names: Vec<String>,
    /// Parent index per part (`None` for the root); always less than the part's
    /// own index, so one forward pass composes the chain.
    pub part_parents: Vec<Option<usize>>,
    /// The authored pose per part; an override copies and adjusts it.
    pub part_rest: Vec<PartPose>,
    /// Local AABB minimum at rest, in block units.
    pub local_min: Vec3,
    /// Local AABB maximum at rest, in block units.
    pub local_max: Vec3,
}

impl BlockEntityMesh {
    /// Bakes a model definition into a renderable block-entity mesh.
    #[must_use]
    pub fn from_model(def: &EntityModelDef) -> Self {
        let baked = bake_entity_parts(def);
        let mut vertices = Vec::new();
        let mut indices = Vec::new();
        let mut parts = Vec::with_capacity(baked.len());
        let mut part_names = Vec::with_capacity(baked.len());
        let mut part_parents = Vec::with_capacity(baked.len());
        let mut part_rest = Vec::with_capacity(baked.len());

        for part in &baked {
            let index_start = indices.len() as u32;
            let vertex_start = vertices.len() as u32;
            push_part_quads(&part.quads, &mut vertices, &mut indices);
            parts.push(PartRange {
                index_start,
                index_count: indices.len() as u32 - index_start,
                vertex_start,
                vertex_count: vertices.len() as u32 - vertex_start,
            });
            part_names.push(part.name.clone());
            part_parents.push(part.parent);
            part_rest.push(part.rest);
        }

        let mut mesh = BlockEntityMesh {
            vertices,
            indices,
            parts,
            part_names,
            part_parents,
            part_rest,
            local_min: Vec3::ZERO,
            local_max: Vec3::ZERO,
        };
        // The rest AABB, measured through the same transform chain the draw uses
        // (`part_transforms` with no overrides) rather than from the texel
        // extents restated by hand.
        let rest = mesh.part_transforms(Mat4::IDENTITY, &[]);
        let mut min = Vec3::splat(f32::INFINITY);
        let mut max = Vec3::splat(f32::NEG_INFINITY);
        for (part_index, part) in baked.iter().enumerate() {
            for quad in &part.quads {
                for p in &quad.positions {
                    let posed = rest[part_index].transform_point3(Vec3::from(*p));
                    min = min.min(posed);
                    max = max.max(posed);
                }
            }
        }
        if mesh.indices.is_empty() {
            min = Vec3::ZERO;
            max = Vec3::ZERO;
        }
        mesh.local_min = min;
        mesh.local_max = max;
        mesh
    }

    /// The index of a part by name.
    #[must_use]
    pub fn index_of(&self, name: &str) -> Option<usize> {
        self.part_names.iter().position(|n| n == name)
    }

    /// Number of quads in the mesh.
    #[must_use]
    pub fn quad_count(&self) -> usize {
        self.indices.len() / 6
    }

    /// Composes one world matrix per part: `placement · chain(parent) · pose`,
    /// with `overrides` replacing the authored pose of the parts it names.
    ///
    /// `overrides` is `(part index, pose)`; a part named twice takes the last
    /// entry. The chain uses [`lodestone_assets::entity::Affine::of_pose`] rather
    /// than a local `rotationZYX`, so the rotation order can never drift from
    /// the one the bake itself used — a second implementation of `rotZYX` is
    /// exactly how a lid ends up hinging about the wrong axis with every unit
    /// test still green.
    #[must_use]
    pub fn part_transforms(&self, placement: Mat4, overrides: &[(usize, PartPose)]) -> Vec<Mat4> {
        use lodestone_assets::entity::Affine;
        let mut poses = self.part_rest.clone();
        for (index, pose) in overrides {
            if let Some(slot) = poses.get_mut(*index) {
                *slot = *pose;
            }
        }
        let mut chain: Vec<Affine> = Vec::with_capacity(poses.len());
        for (index, pose) in poses.iter().enumerate() {
            let local = Affine::of_pose(pose);
            let world = match self.part_parents[index] {
                // `parent < index` is guaranteed by `bake_entity_parts`'
                // pre-order, so the parent's composed transform already exists.
                Some(parent) => chain[parent].compose(&local),
                None => local,
            };
            chain.push(world);
        }
        chain
            .into_iter()
            .map(|a| placement * affine_to_mat4(&a))
            .collect()
    }
}

/// Widens an [`Affine`](lodestone_assets::entity::Affine) (row-major 3×3 plus a
/// translation) into a column-major [`Mat4`].
///
/// `Affine::m[i][j]` is *row* `i`, *column* `j`; `Mat4::from_cols_array_2d`
/// takes **columns**. The transpose here is the whole point — feeding the rows
/// in as columns yields the inverse rotation, which for a chest lid looks like
/// the lid opening *into* the chest and is easy to mistake for a sign error in
/// `chest_lid_x_rot`.
fn affine_to_mat4(a: &lodestone_assets::entity::Affine) -> Mat4 {
    Mat4::from_cols_array_2d(&[
        [a.m[0][0], a.m[1][0], a.m[2][0], 0.0],
        [a.m[0][1], a.m[1][1], a.m[2][1], 0.0],
        [a.m[0][2], a.m[1][2], a.m[2][2], 0.0],
        [a.t[0], a.t[1], a.t[2], 1.0],
    ])
}

/// The baked block-entity corpus: one [`BlockEntityMesh`] per entry in
/// [`BLOCK_ENTITY_MODELS`], baked on the CPU with no GPU involvement.
#[derive(Debug, Clone)]
pub struct BlockEntityModelSet {
    models: Vec<(&'static str, BlockEntityMesh)>,
}

impl BlockEntityModelSet {
    /// Bakes every ported block-entity model.
    #[must_use]
    pub fn load() -> Self {
        BlockEntityModelSet {
            models: BLOCK_ENTITY_MODELS
                .iter()
                .map(|entry: &BlockEntityModelEntry| {
                    (entry.name, BlockEntityMesh::from_model(&(entry.build)()))
                })
                .collect(),
        }
    }

    /// Iterates `(model name, mesh)`.
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &BlockEntityMesh)> {
        self.models.iter().map(|(n, m)| (*n, m))
    }

    /// The mesh for a model name.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&BlockEntityMesh> {
        self.models
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, m)| m)
    }

    /// Number of baked models.
    #[must_use]
    pub fn len(&self) -> usize {
        self.models.len()
    }

    /// Whether the corpus is empty.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.models.is_empty()
    }

    /// Resolves one chest into a drawable instance, or `None` if its model is
    /// not in the corpus.
    #[must_use]
    pub fn resolve_chest(&self, spawn: &ChestSpawn) -> Option<BlockEntityInstance> {
        let model = spawn.half.model();
        let mesh = self.get(model)?;
        let placement = block_entity_placement_matrix(spawn.pos, spawn.facing_yaw_deg);

        // The lid and the lock rotate together about the *same* pivot
        // (`lock.xRot = lid.xRot`), which is why the asset corpus makes them
        // siblings sharing `offset(0, 9, 1)` rather than nesting the lock.
        let x_rot = chest_lid_x_rot(chest_lid_openness(spawn.openness));
        let mut overrides = Vec::with_capacity(2);
        for name in ["lid", "lock"] {
            if let Some(index) = mesh.index_of(name) {
                let mut pose = mesh.part_rest[index];
                pose.x_rot = x_rot;
                overrides.push((index, pose));
            }
        }
        let part_transforms = mesh.part_transforms(placement, &overrides);

        // The cull AABB is taken from the *rest* bounds through the placement
        // matrix, deliberately ignoring the lid angle: an open lid only ever
        // grows the box backwards by a few texels, and recomputing per frame
        // would make a chest pop in and out of view as somebody opens it.
        let (aabb_min, aabb_max) = transformed_aabb(&placement, mesh.local_min, mesh.local_max);
        Some(BlockEntityInstance {
            model,
            texture: chest_texture_stem(spawn.material, spawn.half),
            transform: placement,
            part_transforms,
            aabb_min,
            aabb_max,
            light: spawn.light,
        })
    }
}

impl Default for BlockEntityModelSet {
    fn default() -> Self {
        Self::load()
    }
}

/// The version-free description of one chest to draw this frame.
///
/// The caller owns every field: block state → `facing_yaw_deg`/`half`, block
/// path → `material`, block event viewer count → `openness`, world light →
/// `light`. Keeping this a plain struct is what stops the render crate depending
/// on a protocol version or a client.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ChestSpawn {
    /// Block position (the block's minimum corner, in world coordinates).
    pub pos: [i32; 3],
    /// `Direction.toYRot()` of the chest's `facing` property.
    pub facing_yaw_deg: f32,
    /// Which layer to draw.
    pub half: ChestHalf,
    /// Which sheet to draw with.
    pub material: ChestMaterial,
    /// **Raw** openness in `0..=1` — the eased value is computed here, so a
    /// caller that already eased would double-ease.
    pub openness: f32,
    /// Packed sky/block light (`sky << 4 | block`) at this block. Pass
    /// [`ENTITY_FULLBRIGHT`] only when there is genuinely no world to sample.
    pub light: u8,
}

impl ChestSpawn {
    /// A closed, full-bright, south-facing single chest at `pos` — the minimum a
    /// hermetic gate needs.
    #[must_use]
    pub fn at(pos: [i32; 3]) -> Self {
        ChestSpawn {
            pos,
            facing_yaw_deg: 0.0,
            half: ChestHalf::Single,
            material: ChestMaterial::Regular,
            openness: 0.0,
            light: ENTITY_FULLBRIGHT,
        }
    }
}

/// One resolved block entity, ready to batch.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockEntityInstance {
    /// Model name (the mesh key).
    pub model: &'static str,
    /// Texture stem (the bind-group key).
    pub texture: &'static str,
    /// The placement matrix (block → world).
    pub transform: Mat4,
    /// One world matrix per part, in mesh part order.
    pub part_transforms: Vec<Mat4>,
    /// World AABB minimum, for culling.
    pub aabb_min: Vec3,
    /// World AABB maximum, for culling.
    pub aabb_max: Vec3,
    /// Packed sky/block light.
    pub light: u8,
}

/// One draw batch: every instance sharing a model **and** a texture.
///
/// Both keys matter. Batching on the model alone would draw a trapped chest with
/// a plain chest's sheet, because the mesh is identical and only the bind group
/// differs — a bug that looks like a texture-loading failure, not a batching
/// one.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockEntityBatch {
    /// Model name.
    pub model: &'static str,
    /// Texture stem.
    pub texture: &'static str,
    /// `parts[p][i]` is part `p` of instance `i` — the same per-part instance
    /// layout the entity pass uses, because vertices are part-local and a lid
    /// only moves if its own matrices are uploaded.
    pub parts: Vec<Vec<Mat4>>,
    /// Packed light per instance.
    pub lights: Vec<u32>,
}

impl BlockEntityBatch {
    /// Number of instances in this batch.
    #[must_use]
    pub fn count(&self) -> u32 {
        self.lights.len() as u32
    }
}

/// Culling counters for one frame.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BlockEntityCullStats {
    /// Instances offered.
    pub total: usize,
    /// Instances that survived the frustum.
    pub drawn: usize,
    /// Instances rejected by the frustum.
    pub culled_frustum: usize,
}

/// Everything the block-entity pass draws this frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BlockEntityFrame {
    /// Batches, keyed by `(model, texture)`.
    pub batches: Vec<BlockEntityBatch>,
    /// Culling counters.
    pub stats: BlockEntityCullStats,
}

/// Frustum-culls and batches resolved instances.
#[must_use]
pub fn plan_block_entities(
    instances: &[BlockEntityInstance],
    frustum: &Frustum,
) -> BlockEntityFrame {
    let mut batches: Vec<BlockEntityBatch> = Vec::new();
    let mut stats = BlockEntityCullStats {
        total: instances.len(),
        ..BlockEntityCullStats::default()
    };
    for inst in instances {
        if !frustum.intersects_aabb(inst.aabb_min, inst.aabb_max) {
            stats.culled_frustum += 1;
            continue;
        }
        stats.drawn += 1;
        match batches
            .iter_mut()
            .find(|b| b.model == inst.model && b.texture == inst.texture)
        {
            Some(batch) => {
                batch.lights.push(u32::from(inst.light));
                for (slot, m) in batch.parts.iter_mut().zip(&inst.part_transforms) {
                    slot.push(*m);
                }
            }
            None => batches.push(BlockEntityBatch {
                model: inst.model,
                texture: inst.texture,
                parts: inst.part_transforms.iter().map(|m| vec![*m]).collect(),
                lights: vec![u32::from(inst.light)],
            }),
        }
    }
    BlockEntityFrame { batches, stats }
}

/// Conservative world AABB of a local box under `m`, by transforming all eight
/// corners.
fn transformed_aabb(m: &Mat4, local_min: Vec3, local_max: Vec3) -> (Vec3, Vec3) {
    let mut min = Vec3::splat(f32::INFINITY);
    let mut max = Vec3::splat(f32::NEG_INFINITY);
    for i in 0..8 {
        let corner = Vec3::new(
            if i & 1 == 0 { local_min.x } else { local_max.x },
            if i & 2 == 0 { local_min.y } else { local_max.y },
            if i & 4 == 0 { local_min.z } else { local_max.z },
        );
        let world = m.transform_point3(corner);
        min = min.min(world);
        max = max.max(world);
    }
    (min, max)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::camera::Camera;

    fn set() -> BlockEntityModelSet {
        BlockEntityModelSet::load()
    }

    #[test]
    fn every_ported_model_bakes_with_geometry_and_parts() {
        let set = set();
        assert_eq!(set.len(), 3);
        for (name, mesh) in set.iter() {
            assert!(mesh.quad_count() > 0, "{name} baked no quads");
            assert_eq!(mesh.parts.len(), mesh.part_names.len());
            assert_eq!(mesh.parts.len(), mesh.part_rest.len());
            assert!(mesh.index_of("lid").is_some(), "{name} has no lid part");
            assert!(mesh.index_of("lock").is_some(), "{name} has no lock part");
            assert!(mesh.index_of("bottom").is_some(), "{name} has no bottom");
        }
    }

    /// The rest AABB must land in `0..1` on Y — the assertion an entity-space
    /// (Y-flipped, `−1.501`) placement fails. Measured through the same
    /// `part_transforms` the draw uses, not from restated texel extents.
    #[test]
    fn rest_bounds_sit_inside_the_block_above_the_floor() {
        for (name, mesh) in set().iter() {
            assert!(
                mesh.local_min.y >= -1e-5,
                "{name} dips below the floor: {}",
                mesh.local_min.y
            );
            assert!(
                (mesh.local_max.y - 14.0 / 16.0).abs() < 1e-4,
                "{name} closed lid tops at {} not 14/16",
                mesh.local_max.y
            );
        }
    }

    /// A chest's placement is a pure rigid motion: `det == +1` for every facing,
    /// so it cannot reverse a quad's winding. Measured, not asserted from
    /// "rotations have positive determinant".
    #[test]
    fn placement_preserves_orientation() {
        for name in ["south", "west", "north", "east"] {
            let yaw = horizontal_facing_yaw(name).expect(name);
            let m = block_entity_placement_matrix([3, 64, -7], yaw);
            assert!(
                (m.determinant() - 1.0).abs() < 1e-5,
                "{name}: det {}",
                m.determinant()
            );
        }
    }

    /// The concrete difference from the entity path, measured against the real
    /// `entity_model_matrix` rather than described. A block entity is neither
    /// flipped nor lifted.
    #[test]
    fn placement_does_not_flip_or_lift() {
        let pos = [0, 0, 0];
        let block = block_entity_placement_matrix(pos, 0.0);
        // The block-space origin stays at the block's own corner.
        let origin = block.transform_point3(Vec3::ZERO);
        assert!(origin.abs_diff_eq(Vec3::ZERO, 1e-5), "origin {origin}");
        // +Y stays +Y.
        let up = block.transform_vector3(Vec3::Y);
        assert!(up.abs_diff_eq(Vec3::Y, 1e-5), "up {up}");

        // The entity matrix, by contrast, flips Y and drops 1.501.
        let entity = crate::entity::entity_model_matrix(Vec3::ZERO, 0.0, 1.0);
        let entity_up = entity.transform_vector3(Vec3::Y);
        assert!(
            entity_up.y < 0.0,
            "the entity path is supposed to flip Y; if this fails the two \
             placements have converged and this test no longer measures anything"
        );
        // **`+1.501`, not `−1.501`** — measured, having first been written the
        // other way round. `entity_model_matrix` is
        // `translate(feet) · rotY · scale(−s,−s,s) · translate(0, −1.501, 0)`,
        // and the flip is applied *after* the lift, so the negative translate
        // comes out positive in world space. Reading the lift's sign off the
        // matrix expression left-to-right gets this backwards, which is the same
        // shape of error `CLAUDE.md` records for the depth-bias record.
        let entity_origin = entity.transform_point3(Vec3::ZERO);
        assert!(
            (entity_origin.y - crate::entity::MODEL_FEET_OFFSET).abs() < 1e-4,
            "entity origin y {}",
            entity_origin.y
        );
    }

    /// South is `0`, and reading the yaw off `Direction`'s *declaration* order
    /// instead (down/up/north/south/west/east) is a quarter-turn error on every
    /// chest in the world.
    #[test]
    fn facing_yaw_follows_direction_to_y_rot_not_declaration_order() {
        assert_eq!(horizontal_facing_yaw("south"), Some(0.0));
        assert_eq!(horizontal_facing_yaw("west"), Some(90.0));
        assert_eq!(horizontal_facing_yaw("north"), Some(180.0));
        assert_eq!(horizontal_facing_yaw("east"), Some(270.0));
        assert_eq!(horizontal_facing_yaw("up"), None);
        assert_eq!(horizontal_facing_yaw(""), None);
    }

    /// A north-facing chest's lock (which sticks out of the *front* at z ≈ 1 in
    /// model space) must end up on the low-Z side of the block. This is the
    /// check that a `+yaw` instead of `-yaw` would fail while every determinant
    /// and bounds test stayed green.
    #[test]
    fn facing_rotates_the_front_of_the_chest_to_the_named_side() {
        let set = set();
        let front_z = |facing: &str| -> Vec3 {
            let spawn = ChestSpawn {
                facing_yaw_deg: horizontal_facing_yaw(facing).unwrap(),
                ..ChestSpawn::at([0, 0, 0])
            };
            let inst = set.resolve_chest(&spawn).expect("resolve");
            let lock = set.get(inst.model).unwrap().index_of("lock").unwrap();
            // The lock's own pivot is at model (0, 9, 1); the latch box sits at
            // z 14..15 texels beyond it, i.e. the chest's front face.
            inst.part_transforms[lock].transform_point3(Vec3::new(0.5, 0.0, 15.0 / 16.0))
        };
        let south = front_z("south");
        let north = front_z("north");
        // Vanilla's south-facing chest has its latch on the +Z face.
        assert!(south.z > 0.9, "south latch z {}", south.z);
        assert!(north.z < 0.1, "north latch z {}", north.z);
        let west = front_z("west");
        let east = front_z("east");
        assert!(west.x < 0.1, "west latch x {}", west.x);
        assert!(east.x > 0.9, "east latch x {}", east.x);
    }

    /// The two easings, at both endpoints and the midpoint. `0.5 → 0.875` is the
    /// value that distinguishes vanilla's cubic ease-out from a linear ramp;
    /// the endpoints alone cannot.
    #[test]
    fn lid_easing_matches_vanillas_cubic_ease_out() {
        assert!((chest_lid_openness(0.0) - 0.0).abs() < 1e-6);
        assert!((chest_lid_openness(1.0) - 1.0).abs() < 1e-6);
        let mid = chest_lid_openness(0.5);
        assert!((mid - 0.875).abs() < 1e-6, "mid {mid}");
        assert!(mid > 0.5, "the ease must run ahead of linear");
        // Out-of-range input clamps rather than exploding into a lid that spins.
        assert!((chest_lid_openness(-1.0) - 0.0).abs() < 1e-6);
        assert!((chest_lid_openness(4.0) - 1.0).abs() < 1e-6);
    }

    #[test]
    fn lid_angle_is_a_quarter_turn_backwards_when_fully_open() {
        assert!((chest_lid_x_rot(0.0) - 0.0).abs() < 1e-6);
        let full = chest_lid_x_rot(1.0);
        assert!(
            (full - -std::f32::consts::FRAC_PI_2).abs() < 1e-6,
            "full {full}"
        );
    }

    /// The animation has to *move geometry*, not merely produce a different
    /// number. A closed and an open chest must differ in the lid's part matrix
    /// and agree in the bottom's — the second half is what catches an override
    /// applied to the whole model instead of the named parts.
    #[test]
    fn opening_moves_the_lid_and_lock_and_leaves_the_bottom_alone() {
        let set = set();
        let closed = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let open = set
            .resolve_chest(&ChestSpawn {
                openness: 1.0,
                ..ChestSpawn::at([0, 0, 0])
            })
            .unwrap();
        let mesh = set.get(closed.model).unwrap();
        let lid = mesh.index_of("lid").unwrap();
        let lock = mesh.index_of("lock").unwrap();
        let bottom = mesh.index_of("bottom").unwrap();
        assert_ne!(closed.part_transforms[lid], open.part_transforms[lid]);
        assert_ne!(closed.part_transforms[lock], open.part_transforms[lock]);
        assert_eq!(closed.part_transforms[bottom], open.part_transforms[bottom]);

        // And it moves the right way: a fully open lid's far edge rises above
        // the closed chest's own top, rather than sinking into the box.
        let far_edge = Vec3::new(0.5, 0.0, 14.0 / 16.0);
        let closed_y = closed.part_transforms[lid].transform_point3(far_edge).y;
        let open_y = open.part_transforms[lid].transform_point3(far_edge).y;
        assert!(
            open_y > closed_y + 0.3,
            "an open lid must rise: closed {closed_y}, open {open_y}"
        );
    }

    #[test]
    fn material_resolution_covers_every_chest_block_and_nothing_else() {
        assert_eq!(
            ChestMaterial::from_block_path("chest"),
            Some(ChestMaterial::Regular)
        );
        assert_eq!(
            ChestMaterial::from_block_path("trapped_chest"),
            Some(ChestMaterial::Trapped)
        );
        assert_eq!(
            ChestMaterial::from_block_path("ender_chest"),
            Some(ChestMaterial::Ender)
        );
        assert_eq!(
            ChestMaterial::from_block_path("oxidized_copper_chest"),
            Some(ChestMaterial::CopperOxidized)
        );
        assert_eq!(ChestMaterial::from_block_path("barrel"), None);
        assert_eq!(ChestMaterial::from_block_path("chest_boat"), None);
    }

    /// `getChestMaterial` checks copper and ender *before* the seasonal flag, so
    /// December must not repaint them.
    #[test]
    fn christmas_overrides_only_plain_and_trapped_chests() {
        assert_eq!(
            chest_material_with_season(ChestMaterial::Regular, true),
            ChestMaterial::Christmas
        );
        assert_eq!(
            chest_material_with_season(ChestMaterial::Trapped, true),
            ChestMaterial::Christmas
        );
        assert_eq!(
            chest_material_with_season(ChestMaterial::Ender, true),
            ChestMaterial::Ender
        );
        assert_eq!(
            chest_material_with_season(ChestMaterial::CopperWeathered, true),
            ChestMaterial::CopperWeathered
        );
        assert_eq!(
            chest_material_with_season(ChestMaterial::Regular, false),
            ChestMaterial::Regular
        );
    }

    /// Ender has one sheet for all three halves; every other material has three
    /// distinct ones. A uniform `_left`/`_right` suffix rule would name
    /// `ender_left.png`, which does not exist in the jar.
    #[test]
    fn ender_uses_one_sheet_and_others_use_three() {
        for half in [ChestHalf::Single, ChestHalf::Left, ChestHalf::Right] {
            assert_eq!(
                chest_texture_stem(ChestMaterial::Ender, half),
                "entity/chest/ender"
            );
        }
        for material in CHEST_MATERIALS {
            if *material == ChestMaterial::Ender {
                continue;
            }
            let stems: Vec<&str> = [ChestHalf::Single, ChestHalf::Left, ChestHalf::Right]
                .into_iter()
                .map(|h| chest_texture_stem(*material, h))
                .collect();
            assert_eq!(
                stems.len(),
                stems.iter().collect::<std::collections::BTreeSet<_>>().len(),
                "{material:?} reuses a sheet across halves: {stems:?}"
            );
        }
    }

    /// The preload list is derived from the same match the renderer asks
    /// through, so it cannot go stale: 7 materials × 3 halves + 1 ender = 22.
    #[test]
    fn every_stem_the_renderer_can_ask_for_is_in_the_preload_list() {
        let stems = chest_texture_stems();
        assert_eq!(stems.len(), 22, "{stems:?}");
        for material in CHEST_MATERIALS {
            for half in [ChestHalf::Single, ChestHalf::Left, ChestHalf::Right] {
                let stem = chest_texture_stem(*material, half);
                assert!(stems.contains(&stem), "{stem} missing from the preload list");
            }
        }
    }

    /// A camera 4 blocks back on `-Z` looking down `+Z` (yaw `0`) at the origin
    /// block — chests at `[0,0,0]`/`[1,0,0]` are in view, one 400 blocks behind
    /// is not.
    fn looking_at_origin() -> Camera {
        Camera {
            position: Vec3::new(0.5, 0.5, -4.0),
            yaw: 0.0,
            pitch: 0.0,
            ..Camera::default()
        }
    }

    #[test]
    fn planning_batches_by_model_and_texture_and_culls_what_is_behind() {
        let set = set();
        let front = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let trapped_same_mesh = set
            .resolve_chest(&ChestSpawn {
                material: ChestMaterial::Trapped,
                ..ChestSpawn::at([1, 0, 0])
            })
            .unwrap();
        assert_eq!(
            front.model, trapped_same_mesh.model,
            "a trapped chest shares the single-chest mesh; the batch key must \
             therefore include the texture"
        );
        let behind = set.resolve_chest(&ChestSpawn::at([0, 0, -400])).unwrap();

        let cam = looking_at_origin();
        let frame = plan_block_entities(
            &[front, trapped_same_mesh, behind],
            &Frustum::from_view_projection(cam.view_projection()),
        );
        assert_eq!(frame.stats.total, 3);
        assert_eq!(frame.stats.drawn, 2, "{:?}", frame.stats);
        assert_eq!(frame.stats.culled_frustum, 1);
        assert_eq!(
            frame.batches.len(),
            2,
            "two textures over one mesh must be two batches"
        );
        for batch in &frame.batches {
            assert_eq!(batch.count(), 1);
            assert_eq!(batch.parts.len(), set.get(batch.model).unwrap().parts.len());
        }
    }

    /// Two chests sharing model *and* texture must land in one batch with two
    /// instances per part — the whole point of instancing.
    #[test]
    fn identical_chests_share_one_batch() {
        let set = set();
        let a = set.resolve_chest(&ChestSpawn::at([0, 0, 0])).unwrap();
        let b = set.resolve_chest(&ChestSpawn::at([1, 0, 0])).unwrap();
        let cam = looking_at_origin();
        let frame =
            plan_block_entities(&[a, b], &Frustum::from_view_projection(cam.view_projection()));
        assert_eq!(frame.batches.len(), 1);
        assert_eq!(frame.batches[0].count(), 2);
        for part in &frame.batches[0].parts {
            assert_eq!(part.len(), 2);
        }
    }

    #[test]
    fn light_reaches_the_batch_unchanged() {
        let set = set();
        let dark = set
            .resolve_chest(&ChestSpawn {
                light: 0,
                ..ChestSpawn::at([0, 0, 0])
            })
            .unwrap();
        assert_eq!(dark.light, 0);
        let cam = looking_at_origin();
        let frame = plan_block_entities(&[dark], &Frustum::from_view_projection(cam.view_projection()));
        assert_eq!(frame.batches[0].lights, vec![0]);
    }

    #[test]
    fn an_unknown_half_degrades_to_a_whole_chest() {
        assert_eq!(ChestHalf::parse("single"), ChestHalf::Single);
        assert_eq!(ChestHalf::parse("left"), ChestHalf::Left);
        assert_eq!(ChestHalf::parse("right"), ChestHalf::Right);
        assert_eq!(ChestHalf::parse("sideways"), ChestHalf::Single);
    }
}
