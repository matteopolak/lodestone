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

use std::collections::{BTreeMap, BTreeSet};

use lodestone_assets::fluid::{FluidState, SpriteUv};
use lodestone_assets::{
    Atlas, AtlasBuilder, AtlasError, AtlasSprite, BakedQuad, BlockBaker, BlockStates, FirstWeight,
    ModelResolver, ResourceLocation, ResourceManager, TextureBinding,
};
use lodestone_model::BlockStateRegistry;

use crate::models::is_full_cube;
use crate::translucency::RenderLayer;

/// Which fluid occupies a cell. Water renders translucent and biome-tinted; lava
/// renders opaque and full-bright.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FluidKind {
    /// Water (`minecraft:water`, or the fluid of any `waterlogged` block).
    Water,
    /// Lava (`minecraft:lava`).
    Lava,
}

/// A fluid occupying a cell: its [`FluidKind`] and dynamic [`FluidState`]
/// (amount + falling), resolved from a block state's `level`/`waterlogged`
/// properties.
///
/// Vanilla does not render fluids through the block-model pipeline — their
/// blockstate models are empty — so [`BlockModels::quads`] returns nothing for a
/// fluid state. The mesher instead reads this classification, gathers the
/// neighbourhood a fluid's shape depends on, and bakes the surface through
/// [`lodestone_assets::fluid::bake_fluid`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FluidCell {
    /// Which fluid this is.
    pub kind: FluidKind,
    /// The fluid's amount (`1..=8`) and falling flag.
    pub state: FluidState,
}

/// The still + flow sprite UV rects of one fluid, resolved once from the stitched
/// atlas so the mesher can pass them straight to
/// [`bake_fluid`](lodestone_assets::fluid::bake_fluid).
#[derive(Debug, Clone, Copy)]
pub struct FluidSprites {
    /// The `*_still` sprite (level surfaces, bottom face).
    pub still: SpriteUv,
    /// The `*_flow` sprite (flowing surfaces, side faces).
    pub flow: SpriteUv,
}

/// Maps a fluid block's `level` property to its [`FluidState`], matching vanilla
/// `LiquidBlock.getFluidState`: `level 0` is a full source (`amount 8`), `1..=7`
/// are flowing (`amount = 8 - level`, taller near the source), and `>= 8` are
/// falling (`amount 8`).
fn fluid_state_from_level(level: u8) -> FluidState {
    if level == 0 {
        FluidState::source()
    } else if level >= 8 {
        FluidState::new(8, true)
    } else {
        FluidState::new(8 - level, false)
    }
}

/// Classify a block state into the fluid it exposes, if any.
///
/// `minecraft:water`/`minecraft:lava` carry the fluid directly (via their `level`
/// property); any other block with `waterlogged=true` (kelp, seagrass, stairs,
/// slabs…) carries a water **source**. Everything else is `None`. Pure over the
/// resolved block path + properties, so it is unit-tested without a jar.
fn classify_fluid(block_path: &str, props: &BTreeMap<String, String>) -> Option<FluidCell> {
    let level = || {
        props
            .get("level")
            .and_then(|s| s.parse::<u8>().ok())
            .unwrap_or(0)
    };
    match block_path {
        "water" => Some(FluidCell {
            kind: FluidKind::Water,
            state: fluid_state_from_level(level()),
        }),
        "lava" => Some(FluidCell {
            kind: FluidKind::Lava,
            state: fluid_state_from_level(level()),
        }),
        _ if props.get("waterlogged").is_some_and(|v| v == "true") => Some(FluidCell {
            kind: FluidKind::Water,
            state: FluidState::source(),
        }),
        _ => None,
    }
}

/// The `block/` texture locations of a fluid's still and flow sprites.
fn fluid_texture_locations(kind: FluidKind) -> [ResourceLocation; 2] {
    let (still, flow) = match kind {
        FluidKind::Water => ("minecraft:block/water_still", "minecraft:block/water_flow"),
        FluidKind::Lava => ("minecraft:block/lava_still", "minecraft:block/lava_flow"),
    };
    [
        still.parse().expect("valid fluid texture location"),
        flow.parse().expect("valid fluid texture location"),
    ]
}

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
    /// Per-state fluid classification (`None` for non-fluids). Parallel to
    /// `models`; a state can be *both* a model (a waterlogged stair) and a fluid.
    fluids: Vec<Option<FluidCell>>,
    /// Resolved still/flow UVs for water and lava, from the stitched atlas.
    water_sprites: FluidSprites,
    lava_sprites: FluidSprites,
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
        let mut fluids = Vec::with_capacity(count as usize);
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

            let fluid = registry
                .resolve(id)
                .and_then(|r| classify_fluid(r.block.path(), r.properties));
            fluids.push(fluid);
        }

        let water_sprites = resolve_fluid_sprites(&atlas, FluidKind::Water);
        let lava_sprites = resolve_fluid_sprites(&atlas, FluidKind::Lava);

        Ok(Self {
            atlas,
            models,
            empty: StateModel::empty(),
            fluids,
            water_sprites,
            lava_sprites,
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

    /// The fluid a state exposes, if any (water/lava blocks, or any waterlogged
    /// block). Fluids have empty [`quads`](Self::quads); the mesher renders them
    /// from this classification via [`bake_fluid`](lodestone_assets::fluid::bake_fluid).
    #[must_use]
    pub fn fluid(&self, state_id: u32) -> Option<FluidCell> {
        self.fluids.get(state_id as usize).copied().flatten()
    }

    /// The still + flow sprite UVs for a fluid kind, into [`atlas`](Self::atlas).
    #[must_use]
    pub fn fluid_sprites(&self, kind: FluidKind) -> FluidSprites {
        match kind {
            FluidKind::Water => self.water_sprites,
            FluidKind::Lava => self.lava_sprites,
        }
    }
}

/// Resolve a fluid's still/flow sprite UV rects (first animation frame) from the
/// stitched atlas. Falls back to a zero rect if the texture is missing, which
/// bakes the fluid with a degenerate UV rather than aborting the world.
fn resolve_fluid_sprites(atlas: &Atlas, kind: FluidKind) -> FluidSprites {
    let [still_loc, flow_loc] = fluid_texture_locations(kind);
    FluidSprites {
        still: sprite_uv(atlas, &still_loc),
        flow: sprite_uv(atlas, &flow_loc),
    }
}

/// The first-frame UV rect of an atlas sprite as a [`SpriteUv`], or a zero rect
/// when the sprite is absent.
fn sprite_uv(atlas: &Atlas, loc: &ResourceLocation) -> SpriteUv {
    atlas
        .sprite(loc)
        .and_then(|s| s.frame_uv(0, atlas.width, atlas.height))
        .map_or(
            SpriteUv {
                min: [0.0, 0.0],
                max: [0.0, 0.0],
            },
            |(min, max)| SpriteUv { min, max },
        )
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
    // Fluids have no blockstate model, so their textures are never collected
    // above; add them explicitly so `bake_fluid`'s UVs resolve into this atlas.
    for kind in [FluidKind::Water, FluidKind::Lava] {
        for loc in fluid_texture_locations(kind) {
            let _ = builder.load(manager, &loc);
        }
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

    fn props(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn water_source_and_flowing_levels_classify() {
        let source = classify_fluid("water", &props(&[("level", "0")])).expect("water is a fluid");
        assert_eq!(source.kind, FluidKind::Water);
        assert_eq!(source.state, FluidState::source());

        // Flowing level 1 is nearly full (amount 7), level 7 nearly empty (amount 1).
        assert_eq!(
            classify_fluid("water", &props(&[("level", "1")]))
                .unwrap()
                .state,
            FluidState::new(7, false)
        );
        assert_eq!(
            classify_fluid("water", &props(&[("level", "7")]))
                .unwrap()
                .state,
            FluidState::new(1, false)
        );
        // Falling variants (level >= 8) are full and falling.
        assert_eq!(
            classify_fluid("water", &props(&[("level", "8")]))
                .unwrap()
                .state,
            FluidState::new(8, true)
        );
    }

    #[test]
    fn lava_classifies_as_lava() {
        let lava = classify_fluid("lava", &props(&[("level", "0")])).expect("lava is a fluid");
        assert_eq!(lava.kind, FluidKind::Lava);
        assert_eq!(lava.state, FluidState::source());
    }

    #[test]
    fn waterlogged_blocks_carry_a_water_source() {
        let kelp = classify_fluid("kelp", &props(&[("waterlogged", "true")]))
            .expect("a waterlogged block carries water");
        assert_eq!(kelp.kind, FluidKind::Water);
        assert_eq!(kelp.state, FluidState::source());

        // A non-waterlogged, non-fluid block exposes no fluid.
        assert!(classify_fluid("stone", &props(&[])).is_none());
        assert!(classify_fluid("oak_stairs", &props(&[("waterlogged", "false")])).is_none());
    }
}
