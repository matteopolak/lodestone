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

use std::collections::{BTreeMap, BTreeSet, HashMap};

use lodestone_assets::fluid::{FluidState, SpriteUv};
use lodestone_assets::tint::vanilla_tint_kind;
use lodestone_assets::{
    Atlas, AtlasBuilder, AtlasError, AtlasSprite, BakedQuad, BlockBaker, BlockStates, FirstWeight,
    ModelResolver, ResourceLocation, ResourceManager, TextureBinding,
};
use lodestone_model::BlockStateRegistry;

use crate::block_resolver::DefaultTints;
use crate::models::is_full_cube;
use crate::translucency::RenderLayer;

/// Number of slots in the tint palette uploaded to the model shader. A baked
/// quad's (repurposed) `tint_index` indexes this array; the model shader reads
/// `palette[tint]` and multiplies the sampled texel by it. 256 entries covers a
/// `u8` index with room to spare — vanilla needs well under 50 distinct default
/// tint colours (grass, foliage, dry-foliage, water, the fixed constants and the
/// 16 redstone levels).
pub(crate) const PALETTE_LEN: usize = 256;

/// The reserved palette index meaning "no tint": its slot stays white so an
/// untinted quad renders `tex.rgb * 1`. [`emit_baked_quad`](crate::models) writes
/// this for any quad whose `tint_index` is `None`.
pub(crate) const UNTINTED: u8 = 255;

/// Interns each distinct **default tint colour** into a small palette so a baked
/// quad can carry a stable palette index (in place of the raw model tint index)
/// that the model shader looks up. Distinct tint *sources* — grass vs. foliage
/// vs. a fixed constant — get distinct slots, which is exactly what the old
/// single hardcoded shader green destroyed.
pub(crate) struct TintPalette {
    colors: Vec<[f32; 4]>,
    lookup: HashMap<u32, u8>,
    next: u8,
}

impl std::fmt::Debug for TintPalette {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TintPalette")
            .field("distinct", &self.lookup.len())
            .finish()
    }
}

impl TintPalette {
    /// An empty palette: every slot white (the untinted identity), nothing
    /// interned yet.
    pub(crate) fn new() -> Self {
        Self {
            colors: vec![[1.0, 1.0, 1.0, 1.0]; PALETTE_LEN],
            lookup: HashMap::new(),
            next: 0,
        }
    }

    /// Intern a `0xRRGGBB` colour, returning its palette index. Repeated colours
    /// reuse their slot. Saturates just below [`UNTINTED`] so the reserved white
    /// sentinel is never overwritten (unreachable for any real vanilla pack).
    pub(crate) fn intern(&mut self, rgb: u32) -> u8 {
        if let Some(&idx) = self.lookup.get(&rgb) {
            return idx;
        }
        let idx = self.next.min(UNTINTED - 1);
        self.colors[idx as usize] = rgb_to_rgba(rgb);
        self.lookup.insert(rgb, idx);
        self.next = self.next.saturating_add(1);
        idx
    }

    /// The `PALETTE_LEN` palette entries, as the model shader's uniform expects.
    pub(crate) fn colors(&self) -> &[[f32; 4]] {
        &self.colors
    }
}

/// Decode a `0xRRGGBB` colour to a straight (non-linearised) RGBA multiplier,
/// matching how the model shader multiplies the sampled texel. The bytes are
/// used as-is — e.g. `0x91BD59` → `[0.5686, 0.7411, 0.349, 1.0]`, the exact
/// value the shader previously hardcoded for grass.
fn rgb_to_rgba(rgb: u32) -> [f32; 4] {
    let r = ((rgb >> 16) & 0xFF) as f32 / 255.0;
    let g = ((rgb >> 8) & 0xFF) as f32 / 255.0;
    let b = (rgb & 0xFF) as f32 / 255.0;
    [r, g, b, 1.0]
}

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

/// The number of progressive crack-overlay stages (`destroy_stage_0..=9`),
/// matching vanilla's `getDestroyStage` range.
pub const CRACK_STAGE_COUNT: usize = 10;

/// The `block/destroy_stage_<stage>` crack-overlay sprite location. `stage` is
/// `0..CRACK_STAGE_COUNT`; these sprites are referenced by no block model, so
/// they must be stitched into the atlas explicitly (like fluids).
fn crack_stage_location(stage: usize) -> ResourceLocation {
    format!("minecraft:block/destroy_stage_{stage}")
        .parse()
        .expect("valid destroy_stage location")
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
    /// Normalised atlas UVs `[u0, v0, u1, v1]` of the model's `#particle`
    /// sprite — what break/hit particles sample. See
    /// [`BakedModel::particle_uv`](lodestone_assets::bake::BakedModel::particle_uv)
    /// for why this cannot be derived from `quads`.
    pub particle_uv: Option<[f32; 4]>,
}

impl StateModel {
    /// An empty (air-like) model: no geometry, no occlusion, solid layer.
    #[must_use]
    fn empty() -> Self {
        StateModel {
            quads: Vec::new(),
            occludes: false,
            layer: RenderLayer::Solid,
            particle_uv: None,
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
    /// The default (plains) tint colours the baked quads' `tint_index` values
    /// index into. Uploaded to the model shader; see [`Self::tint_palette`].
    tint_palette: Vec<[f32; 4]>,
    /// Normalised atlas UVs `[u0, v0, u1, v1]` of each `destroy_stage_N`
    /// crack-overlay sprite, indexed by stage `0..CRACK_STAGE_COUNT`. The
    /// mining crack pass re-draws a block's model geometry sampling these.
    crack_stages: [[f32; 4]; CRACK_STAGE_COUNT],
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
        // The fixed default (plains) tint colours, sampled from the pack's real
        // colormap PNGs. Each tinted quad resolves its exact source colour here
        // and carries a palette index the model shader looks up — replacing the
        // single hardcoded green that made grass, leaves and every other tinted
        // quad render identically.
        let tints = DefaultTints::load(manager);
        let mut palette = TintPalette::new();
        let count = registry.state_count();
        let mut models = Vec::with_capacity(count as usize);
        let mut fluids = Vec::with_capacity(count as usize);
        for id in 0..count {
            let resolved = registry.resolve(id);
            let sm = match baker.bake_state(registry, id, &FirstWeight) {
                Ok(model) if !model.quads.is_empty() => {
                    let particle_uv = model.particle_uv;
                    let mut quads = model.quads;
                    // Rewrite each tinted quad's raw model tint index into a
                    // palette index for its resolved source colour. `None` (an
                    // untinted kind, e.g. a `tint_index` on a non-biome block)
                    // clears the tint so the quad renders its texture unchanged.
                    if let Some(r) = resolved.as_ref() {
                        for quad in &mut quads {
                            if let Some(raw) = quad.tint_index {
                                let kind = vanilla_tint_kind(r.block, raw, r.properties);
                                quad.tint_index =
                                    tints.color(kind).map(|rgb| i32::from(palette.intern(rgb)));
                            }
                        }
                    }
                    let layer = block_layer(&sprite_rects, &quads);
                    let occludes = is_full_cube(&quads) && layer == RenderLayer::Solid;
                    StateModel {
                        quads,
                        occludes,
                        layer,
                        particle_uv,
                    }
                }
                _ => StateModel::empty(),
            };
            models.push(sm);

            let fluid = resolved.and_then(|r| classify_fluid(r.block.path(), r.properties));
            fluids.push(fluid);
        }

        let water_sprites = resolve_fluid_sprites(&atlas, FluidKind::Water);
        let lava_sprites = resolve_fluid_sprites(&atlas, FluidKind::Lava);
        let crack_stages = std::array::from_fn(|stage| {
            let uv = sprite_uv(&atlas, &crack_stage_location(stage));
            [uv.min[0], uv.min[1], uv.max[0], uv.max[1]]
        });

        Ok(Self {
            atlas,
            models,
            empty: StateModel::empty(),
            fluids,
            water_sprites,
            lava_sprites,
            tint_palette: palette.colors().to_vec(),
            crack_stages,
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

    /// The default (plains) tint palette the baked quads index. Exactly
    /// [`PALETTE_LEN`] RGBA entries; a quad's `tint_index` (rewritten to a
    /// palette index by [`build`](Self::build)) selects one, and slot
    /// [`UNTINTED`] is white. Upload this to the model pipeline so tinted quads
    /// get their real per-source colour instead of one hardcoded green.
    #[must_use]
    pub fn tint_palette(&self) -> &[[f32; 4]] {
        &self.tint_palette
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

    /// Normalised atlas UVs `[u0, v0, u1, v1]` of a state's `#particle` sprite —
    /// the texture break and hit particles sample. `None` for air, unknown
    /// states, and models that declare no `particle` variable.
    #[must_use]
    pub fn particle_uv(&self, state_id: u32) -> Option<[f32; 4]> {
        self.state(state_id).particle_uv
    }

    /// The render layer of a state's geometry.
    #[must_use]
    pub fn layer(&self, state_id: u32) -> RenderLayer {
        self.state(state_id).layer
    }

    /// Normalised atlas UVs `[u0, v0, u1, v1]` of the `destroy_stage_<stage>`
    /// crack-overlay sprite, for the mining crack pass to re-texture a block's
    /// model geometry. `stage` is the vanilla destroy stage `0..=9` (the value
    /// `Mining::destroy_stage` / `BlockDestructionOverlays::stage_at` yield in
    /// the game layer); out-of-range stages return `None`.
    #[must_use]
    pub fn crack_stage_uv(&self, stage: u8) -> Option<[f32; 4]> {
        self.crack_stages.get(stage as usize).copied()
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
    let sprite = atlas.sprite(loc);
    let anim = sprite.map_or(0, |s| s.anim_slot);
    sprite
        .and_then(|s| s.frame_uv(0, atlas.width, atlas.height))
        .map_or(
            SpriteUv {
                min: [0.0, 0.0],
                max: [0.0, 0.0],
                anim: 0,
            },
            |(min, max)| SpriteUv { min, max, anim },
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
    // Crack-overlay stages are likewise referenced by no block model; add them
    // so the mining crack pass can sample them from this same atlas.
    for stage in 0..CRACK_STAGE_COUNT {
        let _ = builder.load(manager, &crack_stage_location(stage));
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
            anim: 0,
        }
    }

    #[test]
    fn tint_palette_interns_distinct_sources() {
        // The whole point of the palette is that different tint *sources* keep
        // different colours. Grass (#91BD59) and foliage (#77AB2F) are distinct
        // in vanilla; the old single hardcoded green collapsed them to one.
        let mut p = TintPalette::new();
        let grass = p.intern(0x0091_BD59);
        let foliage = p.intern(0x0077_AB2F);
        assert_ne!(
            grass, foliage,
            "grass and foliage must not share a palette slot"
        );
        // Interning a repeat colour reuses its slot rather than growing.
        assert_eq!(p.intern(0x0091_BD59), grass);

        // The reserved untinted sentinel stays white so untinted quads render
        // their texture unchanged (`tex.rgb * 1`).
        assert_eq!(p.colors()[UNTINTED as usize], [1.0, 1.0, 1.0, 1.0]);
        assert_eq!(p.colors().len(), PALETTE_LEN);

        // Decoded colours carry vanilla's green-dominant ratios, and foliage is
        // measurably greener than grass (G/R 1.44 vs 1.30) — the exact
        // distinction the user measured missing.
        let g = p.colors()[grass as usize];
        let f = p.colors()[foliage as usize];
        let gr_grass = g[1] / g[0];
        let gr_foliage = f[1] / f[0];
        assert!(
            (gr_grass - 189.0 / 145.0).abs() < 1e-3,
            "grass G/R should be ~1.303, got {gr_grass}"
        );
        assert!(
            (gr_foliage - 171.0 / 119.0).abs() < 1e-3,
            "foliage G/R should be ~1.437, got {gr_foliage}"
        );
        assert!(
            gr_foliage > gr_grass + 0.1,
            "foliage must render greener than grass: {gr_foliage} vs {gr_grass}"
        );
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
