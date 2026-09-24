use std::sync::Arc;

use glam::{Mat4, Vec3};
use lodestone_assets::ResourceLocation;

use crate::camera::Frustum;
use crate::entity_pipeline::InstanceTint;

/// The texture identity a block-entity draw carries through batching.
///
/// The URL is deliberately an [`Arc<str>`]: a placed player head is retained
/// in world state across frames, and cloning the identity while extracting,
/// resolving, and batching must not copy an unbounded server-provided URL.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BlockEntityTexture {
    /// A sheet packaged with the client jar and owned by the block-entity
    /// texture map.
    Static(&'static str),
    /// A remote player skin, shared with `EntityRenderer::player_skins`.
    PlayerSkin(Arc<str>),
}

impl BlockEntityTexture {
    /// Returns this dynamic texture URL, if this is a remote player skin.
    #[must_use]
    pub fn player_skin_url(&self) -> Option<&str> {
        match self {
            Self::Static(_) => None,
            Self::PlayerSkin(url) => Some(url),
        }
    }
}

impl std::fmt::Display for BlockEntityTexture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Static(stem) => f.write_str(stem),
            Self::PlayerSkin(url) => f.write_str(url),
        }
    }
}

impl PartialEq<&str> for BlockEntityTexture {
    fn eq(&self, other: &&str) -> bool {
        matches!(self, Self::Static(stem) if stem == other)
    }
}

/// One resolved block entity, ready to batch.
#[derive(Debug, Clone, PartialEq)]
pub struct BlockEntityInstance {
    /// Model name (the mesh key).
    pub model: &'static str,
    /// Texture identity (the bind-group key).
    pub texture: BlockEntityTexture,
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
    /// Gamma-space `[r, g, b]` multiplied into the texel — `[255, 255, 255]`
    /// (`entity_pipeline::NO_TINT`'s rgb half) for "leave the texel alone".
    ///
    /// Per-instance tint already exists end to end for entities (sheep wool,
    /// dyed armour, the hurt overlay, the creeper flash all go through
    /// [`crate::entity_pipeline::EntityInstanceRaw::tint`]/[`InstanceTint`]) —
    /// this is that same plumbing reaching block entities. Every resolver in
    /// this module passes `[255, 255, 255]` today (no block-entity type here
    /// is tinted yet); a future banner base-colour or shulker-box dye reads
    /// this field instead of widening the pipeline.
    pub tint: [u8; 3],
}

/// One resolved translucent pattern-mask layer, ready for a caller to draw
/// directly through `EntityPipeline::banner_layer_pipeline` — the "small
/// separate ordered draw list" `docs/banner-shield-patterns.md` calls for.
///
/// **Deliberately not batched.** These draws are translucent and
/// depth-write-off, so they must submit in the item's own stored order —
/// [`plan_block_entities`]'s `(model, texture)` batching would let two
/// banners reusing the same two sprites in opposite orders interleave
/// incorrectly. Banners are rare, so a handful of unbatched draw calls per
/// banner costs nothing; a caller draws [`BannerInstances::layers`] in
/// order, one instance per draw.
#[derive(Debug, Clone, PartialEq)]
pub struct BannerLayerDraw {
    /// World transform for the flag part this layer paints over — the same
    /// value for every layer of one banner, since masks paint over the
    /// (posed, swaying) flag, never the pole/bar.
    pub transform: Mat4,
    /// The mask sprite to sample: `entity/banner/base` for the always-present
    /// base layer, then `entity/banner/<pattern-asset-id>` per stored
    /// pattern, in the item's own order. See
    /// [`crate::banner_pattern::PatternLayer::sprite`]'s doc — this is a full
    /// [`ResourceLocation`], not a bare asset id.
    pub sprite: ResourceLocation,
    /// Gamma-space `[r, g, b]` in `0.0..=1.0` to tint this layer's sampled
    /// texel by (see `crate::banner_pattern`'s gamma-space note — **do not**
    /// convert this to linear before multiplying the sampled texel).
    pub color: [f32; 3],
    /// Packed sky/block light — identical to the flag's own.
    pub light: u8,
}

/// Everything one ground/standing banner draws this frame: the opaque
/// body/flag (through the ordinary [`plan_block_entities`] batcher, same as
/// every other block-entity type in this module) plus the ordered,
/// translucent pattern-layer draw list (drawn separately, through
/// `EntityPipeline::banner_layer_pipeline`, in order) — see
/// [`BlockEntityModelSet::resolve_banner`]'s doc for the full draw-order
/// derivation.
#[derive(Debug, Clone, PartialEq)]
pub struct BannerInstances {
    /// The pole+bar, opaque, `entity/banner/banner_base`.
    pub body: BlockEntityInstance,
    /// The flag, opaque, same sheet as `body` — the plain cloth/wood pass,
    /// *not* a pattern. Already posed with this frame's sway.
    pub flag: BlockEntityInstance,
    /// The base-colour mask plus every stored pattern layer, in draw order —
    /// `1 + patterns.len().min(MAX_PATTERN_LAYERS)` entries, never empty
    /// (the base layer always draws, even with zero stored patterns).
    pub layers: Vec<BannerLayerDraw>,
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
    /// Texture identity.
    pub texture: BlockEntityTexture,
    /// `parts[p][i]` is part `p` of instance `i` — the same per-part instance
    /// layout the entity pass uses, because vertices are part-local and a lid
    /// only moves if its own matrices are uploaded.
    pub parts: Vec<Vec<Mat4>>,
    /// Packed light per instance.
    pub lights: Vec<u32>,
    /// Per-instance tint, lockstep with [`lights`](Self::lights) — every part
    /// of one instance shares its tint, so this lives once per instance
    /// rather than once per `parts` slot. Fed to
    /// [`crate::entity_pipeline::upload_instances_tinted`] the same way
    /// entity draws already are; a short/missing entry falls back to
    /// [`InstanceTint::NONE`], matching that function's own lockstep-fallback
    /// contract for `lights`.
    pub tints: Vec<InstanceTint>,
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
                batch.tints.push(InstanceTint::rgb(inst.tint));
                for (slot, m) in batch.parts.iter_mut().zip(&inst.part_transforms) {
                    slot.push(*m);
                }
            }
            None => batches.push(BlockEntityBatch {
                model: inst.model,
                texture: inst.texture.clone(),
                parts: inst.part_transforms.iter().map(|m| vec![*m]).collect(),
                lights: vec![u32::from(inst.light)],
                tints: vec![InstanceTint::rgb(inst.tint)],
            }),
        }
    }
    BlockEntityFrame { batches, stats }
}

/// Conservative world AABB of a local box under `m`, by transforming all eight
/// corners.
pub(crate) fn transformed_aabb(m: &Mat4, local_min: Vec3, local_max: Vec3) -> (Vec3, Vec3) {
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
