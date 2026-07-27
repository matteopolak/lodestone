//! Real vanilla block **geometry**: `state_id -> baked model quads`.
//!
//! This is the model-path counterpart to [`BlockAtlas`](crate::BlockAtlas). Where
//! `BlockAtlas` projects every state to a six-face cube (the packed-vertex fast
//! path — see its "cubes first" scope note), `BlockModels` keeps the **real baked
//! geometry** each state resolves to: cross-shaped plants stay two crossed quads,
//! slabs stay half-height, stairs keep their steps, fluids and glass carry their
//! partial-alpha faces. It is the bridge the shell mesher needs to stop treating
//! every block as a full opaque cube.
//!
//! The heavy lifting is already done by [`lodestone_assets`]: [`BlockBaker`]
//! resolves a state's blockstate + models and bakes them into [`BakedQuad`]s with
//! absolute atlas UVs, `cullface`, `tint_index` and per-face winding. This type
//! ties that to the renderer:
//!
//! * a **complete** stitched [`Atlas`] of every block texture (not just the
//!   cube-face subset), so a baked quad's UVs always resolve to a real sprite;
//! * a per-state [`StateModel`] carrying the baked quads, a geometry-derived
//!   occlusion flag ([`is_full_cube`] **and** an opaque layer — a cutout or
//!   translucent full cube such as leaves, glass or water must not cull its
//!   neighbours), and the block's [`RenderLayer`] derived from its sprites'
//!   alpha.
//!
//! # Why occlusion follows geometry, not a list
//!
//! `occludes` is `is_full_cube(quads) && layer == Solid` — computed from the
//! baked geometry and the sprite alpha, never a hardcoded per-block table. A
//! per-version block list would be a version-specific fact smuggled into a
//! version-free crate and would rot the first time Mojang changed a model. A
//! cross-plant is not a full cube, so it never occludes; leaves are a full cube
//! but cutout, so they do not occlude either.
//!
//! # Render layer
//!
//! Vanilla's authoritative render type is a hardcoded per-block table in
//! version-specific Java, absent from every data report. We derive it from the
//! sprite alpha via [`RenderLayer::from_sprite_alpha`] (the heuristic
//! [`translucency`](crate::translucency) documents), taking the *most
//! transparent* layer across a block's sprites so a block with one translucent
//! face lands on the translucent pass.

use std::collections::BTreeSet;

use lodestone_assets::{
    Atlas, AtlasBuilder, AtlasError, AtlasSprite, BakedQuad, BlockBaker, BlockStates, FirstWeight,
    ModelResolver, ResourceLocation, ResourceManager, TextureBinding,
};
use lodestone_model::BlockStateRegistry;

use crate::models::is_full_cube;
use crate::translucency::RenderLayer;

/// Errors from [`BlockModels::build`].
#[derive(Debug, thiserror::Error)]
pub enum BlockModelsError {
    /// The stitched atlas failed to build.
    #[error("atlas build failed: {0}")]
    Atlas(#[from] AtlasError),
}

/// The baked geometry of a single block state.
#[derive(Debug, Clone)]
pub struct StateModel {
    /// The baked quads, with absolute atlas UVs. Empty for air / fluids with no
    /// blockstate model / states that fail to bake.
    pub quads: Vec<BakedQuad>,
    /// Whether this state fully occludes its neighbours: a full opaque cube.
    /// `is_full_cube(quads) && layer == Solid`, so leaves/glass/water (full-cube
    /// geometry but cutout/translucent) correctly do **not** cull neighbours.
    pub occludes: bool,
    /// The render pass this block's geometry belongs to, derived from its sprite
    /// alpha (the most transparent layer across its faces).
    pub layer: RenderLayer,
}

impl StateModel {
    /// An empty (air-like) model: no geometry, no occlusion, solid layer.
    #[must_use]
    fn empty() -> Self {
        StateModel {
            quads: Vec::new(),
            occludes: false,
            layer: RenderLayer::Solid,
        }
    }
}

/// Every vanilla block state's baked geometry plus the complete atlas its UVs
/// index. See the [module docs](self).
#[derive(Debug)]
pub struct BlockModels {
    atlas: Atlas,
    models: Vec<StateModel>,
    empty: StateModel,
}

impl BlockModels {
    /// Bake every state in `registry` against the real assets in `manager`.
    ///
    /// `manager` is a resource manager over a vanilla resource pack (a
    /// `client.jar` opened with `ZipSource`); `registry` maps each `state_id` to
    /// its block and properties (satisfied by a version crate's table via
    /// `lodestone-registry`, or the crate's own [`blocks_json_registry`]).
    ///
    /// [`blocks_json_registry`]: crate::blocks_json_registry
    ///
    /// # Errors
    ///
    /// Returns [`BlockModelsError::Atlas`] if the block atlas cannot be stitched.
    /// Individual states that fail to bake (missing blockstate, unresolved model)
    /// are tolerated and stored as empty geometry, matching how a real client
    /// renders an unknown block as nothing rather than aborting the world.
    pub fn build(
        manager: &ResourceManager,
        registry: &dyn BlockStateRegistry,
    ) -> Result<Self, BlockModelsError> {
        let resolver = ModelResolver::new(manager);
        let atlas = build_complete_atlas(manager, &resolver)?;

        // Precompute each atlas sprite's render layer once; a baked quad's layer
        // is the layer of the sprite its UVs land in.
        let sprite_rects: Vec<SpriteRect> = atlas
            .sprites()
            .iter()
            .map(|s| SpriteRect {
                uv_min: s.uv_min,
                uv_max: s.uv_max,
                layer: sprite_layer(&atlas, s),
            })
            .collect();

        let baker = BlockBaker::new(manager, &resolver, &atlas);
        let count = registry.state_count();
        let mut models = Vec::with_capacity(count as usize);
        for id in 0..count {
            let sm = match baker.bake_state(registry, id, &FirstWeight) {
                Ok(model) if !model.quads.is_empty() => {
                    let layer = block_layer(&sprite_rects, &model.quads);
                    let occludes = is_full_cube(&model.quads) && layer == RenderLayer::Solid;
                    StateModel {
                        quads: model.quads,
                        occludes,
                        layer,
                    }
                }
                _ => StateModel::empty(),
            };
            models.push(sm);
        }

        Ok(Self {
            atlas,
            models,
            empty: StateModel::empty(),
        })
    }

    /// The complete stitched block atlas, for
    /// [`GpuAtlas::from_atlas`](crate::GpuAtlas::from_atlas). Its UVs are what the
    /// baked quads index, so the renderer must upload **this** atlas, not
    /// `BlockAtlas`'s cube-only one.
    #[must_use]
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// The baked geometry of a state, or an empty model for air / out-of-range
    /// ids.
    #[must_use]
    pub fn state(&self, state_id: u32) -> &StateModel {
        self.models.get(state_id as usize).unwrap_or(&self.empty)
    }

    /// The baked quads of a state (empty for air / unknown).
    #[must_use]
    pub fn quads(&self, state_id: u32) -> &[BakedQuad] {
        &self.state(state_id).quads
    }

    /// Whether a state fully occludes its neighbours (a full opaque cube).
    #[must_use]
    pub fn occludes(&self, state_id: u32) -> bool {
        self.state(state_id).occludes
    }

    /// The render layer of a state's geometry.
    #[must_use]
    pub fn layer(&self, state_id: u32) -> RenderLayer {
        self.state(state_id).layer
    }

    /// The number of states baked.
    #[must_use]
    pub fn state_count(&self) -> usize {
        self.models.len()
    }
}

/// A sprite's UV rectangle plus its precomputed render layer, for mapping a baked
/// quad back to the sprite it samples.
#[derive(Debug, Clone, Copy)]
struct SpriteRect {
    uv_min: [f32; 2],
    uv_max: [f32; 2],
    layer: RenderLayer,
}

/// Stitch a complete block atlas: every texture referenced by any blockstate's
/// resolved models, so a baked quad's UVs always resolve to a real sprite.
///
/// This walks `assets/<ns>/blockstates/`, resolves each referenced model and
/// collects its resolved texture bindings — the same coverage `model_census`
/// proves bakes every state without a missing sprite.
fn build_complete_atlas(
    manager: &ResourceManager,
    resolver: &ModelResolver,
) -> Result<Atlas, AtlasError> {
    let mut textures: BTreeSet<ResourceLocation> = BTreeSet::new();
    for path in manager.list("assets/minecraft/blockstates/") {
        let Some(bytes) = manager.read(&path) else {
            continue;
        };
        let Ok(bs) = BlockStates::parse(&bytes) else {
            continue;
        };
        for r in bs.model_refs() {
            if let Ok(model) = resolver.resolve(&r.model) {
                for binding in model.textures.values() {
                    if let TextureBinding::Resolved(loc) = binding {
                        textures.insert(loc.clone());
                    }
                }
            }
        }
    }

    let mut builder = AtlasBuilder::new().with_mip_levels(4);
    for loc in &textures {
        // A missing texture is tolerated: the quad's UVs fall on whatever the
        // atlas packed, and a hard fault (below) aborts. Vanilla likewise renders
        // a missing texture rather than crashing.
        let _ = builder.load(manager, loc);
    }
    builder.build()
}

/// A sprite's render layer, derived from the alpha of its first animation frame.
fn sprite_layer(atlas: &Atlas, sprite: &AtlasSprite) -> RenderLayer {
    RenderLayer::from_sprite_alpha(&sprite_alpha(atlas, sprite))
}

/// Extract the per-texel alpha of a sprite's first frame from the stitched atlas.
fn sprite_alpha(atlas: &Atlas, sprite: &AtlasSprite) -> Vec<u8> {
    let aw = atlas.width;
    let mut out = Vec::with_capacity((sprite.width * sprite.frame_height) as usize);
    for row in 0..sprite.frame_height {
        let y = sprite.y + row;
        for col in 0..sprite.width {
            let x = sprite.x + col;
            let idx = ((y * aw + x) * 4 + 3) as usize;
            if let Some(&a) = atlas.rgba.get(idx) {
                out.push(a);
            }
        }
    }
    out
}

/// The render layer of a whole block: the most transparent layer across all its
/// quads' sprites (`Solid < Cutout < Translucent`), so a block with any
/// translucent face lands on the translucent pass.
fn block_layer(sprites: &[SpriteRect], quads: &[BakedQuad]) -> RenderLayer {
    let mut layer = RenderLayer::Solid;
    for quad in quads {
        if let Some(sr) = sprite_for_uv(sprites, uv_centroid(quad)) {
            layer = layer.max(sr.layer);
        }
    }
    layer
}

/// The centroid of a quad's four UVs — a point guaranteed to sit inside its
/// sprite's UV rect, robust to a corner landing exactly on a shared edge.
fn uv_centroid(quad: &BakedQuad) -> [f32; 2] {
    let mut u = 0.0;
    let mut v = 0.0;
    for c in &quad.uvs {
        u += c[0];
        v += c[1];
    }
    [u / 4.0, v / 4.0]
}

/// Find the sprite whose UV rect contains `uv`.
fn sprite_for_uv(sprites: &[SpriteRect], uv: [f32; 2]) -> Option<&SpriteRect> {
    sprites.iter().find(|s| {
        uv[0] >= s.uv_min[0]
            && uv[0] <= s.uv_max[0]
            && uv[1] >= s.uv_min[1]
            && uv[1] <= s.uv_max[1]
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_assets::Direction;

    fn quad_with_uv(uv: [f32; 2]) -> BakedQuad {
        BakedQuad {
            positions: [[0.0; 3]; 4],
            uvs: [uv; 4],
            direction: Direction::Up,
            cullface: None,
            tint_index: None,
            shade: true,
            layer: 0,
        }
    }

    #[test]
    fn uv_centroid_is_the_average_corner() {
        let q = BakedQuad {
            uvs: [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]],
            ..quad_with_uv([0.0, 0.0])
        };
        assert_eq!(uv_centroid(&q), [0.5, 0.5]);
    }

    #[test]
    fn sprite_for_uv_finds_the_containing_rect() {
        let sprites = vec![
            SpriteRect {
                uv_min: [0.0, 0.0],
                uv_max: [0.5, 1.0],
                layer: RenderLayer::Solid,
            },
            SpriteRect {
                uv_min: [0.5, 0.0],
                uv_max: [1.0, 1.0],
                layer: RenderLayer::Translucent,
            },
        ];
        assert_eq!(
            sprite_for_uv(&sprites, [0.25, 0.5]).map(|s| s.layer),
            Some(RenderLayer::Solid)
        );
        assert_eq!(
            sprite_for_uv(&sprites, [0.75, 0.5]).map(|s| s.layer),
            Some(RenderLayer::Translucent)
        );
        assert!(sprite_for_uv(&sprites, [2.0, 2.0]).is_none());
    }

    #[test]
    fn block_layer_takes_the_most_transparent_face() {
        // One translucent face drags the whole block onto the translucent pass.
        let sprites = vec![
            SpriteRect {
                uv_min: [0.0, 0.0],
                uv_max: [0.5, 1.0],
                layer: RenderLayer::Solid,
            },
            SpriteRect {
                uv_min: [0.5, 0.0],
                uv_max: [1.0, 1.0],
                layer: RenderLayer::Translucent,
            },
        ];
        let quads = vec![quad_with_uv([0.25, 0.5]), quad_with_uv([0.75, 0.5])];
        assert_eq!(block_layer(&sprites, &quads), RenderLayer::Translucent);

        // All-solid stays solid.
        let solid_only = vec![quad_with_uv([0.25, 0.5])];
        assert_eq!(block_layer(&sprites, &solid_only), RenderLayer::Solid);
    }

    #[test]
    fn empty_model_does_not_occlude() {
        let e = StateModel::empty();
        assert!(!e.occludes);
        assert!(e.quads.is_empty());
        assert_eq!(e.layer, RenderLayer::Solid);
    }
}
