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
//! * a per-state [`StateModel`] carrying the baked quads, **per-face** occlusion
//!   ([`StateModel::face_occludes`] — a cutout or translucent full cube such as
//!   leaves, glass or water must not cull its neighbours), and the block's
//!   [`RenderLayer`] derived from its sprites' alpha.
//!
//! # Why occlusion follows geometry, not a list
//!
//! Occlusion is computed from the baked geometry and the sprite alpha, never a
//! hardcoded per-block table. A per-version block list would be a
//! version-specific fact smuggled into a version-free crate and would rot the
//! first time Mojang changed a model. A cross-plant covers no boundary face, so
//! it never occludes; leaves cover all six but their sprite is cutout, so they do
//! not occlude either.
//!
//! It is decided **per face**, not per block: see `face_occlusion` below for the
//! measurement that forced that, and `docs/fluid-rendering.md` for the
//! water-shoreline bug the per-block version caused.
//!
//! # Render layer
//!
//! Vanilla's authoritative render type is a hardcoded per-block table in
//! version-specific Java, absent from every data report. We derive it from the
//! sprite alpha via [`RenderLayer::from_sprite_alpha`] (the heuristic
//! [`translucency`](crate::translucency) documents), taking the *most
//! transparent* layer across a block's sprites so a block with one translucent
//! face lands on the translucent pass.
//!
//! # Why item geometry lives here too
//!
//! `BlockModels` also owns [`ItemGeometry`] for every item with a drawable icon —
//! **both** streams, in one map keyed by item id:
//!
//! * [`IconPart::Model`] — a 3-D model, 752 of 26.2's 1,537 items;
//! * [`IconPart::Sprite`] — the flat `builtin/generated` layer stack, extruded
//!   into vanilla's thin slab by `extruded_sprite_geometry`, which is most of
//!   the remaining ~785.
//!
//! **The two are indistinguishable to a consumer**, and that is deliberate: a
//! dropped diamond, a diamond in a zombie's hand, a diamond in the first-person
//! hand and a thrown snowball all just look the item up. There is no
//! sprite-specific accessor and there should not be one — every consumer that
//! wanted "the sprite stream" wanted [`BlockModels::item`], and reading the two-stream
//! split in `ItemIcon` as a two-*map* split here has already sent four issues
//! hunting a missing accessor that never existed.
//!
//! The name `BlockModels` is a stretch; the honest framing is **"everything baked
//! against this atlas"**, and it is worth the stretch:
//!
//! * A block item's faces are block textures. Baking them here reuses the *same*
//!   stitched [`Atlas`] the terrain path already uploads — no second atlas, no
//!   second GPU upload, no duplicated 16 MB of sprites.
//! * A tinted item (grass block, oak leaves) must resolve to the *same* palette
//!   slot as the block, or the hotbar icon and the world block would render
//!   different greens. Sharing one [`TintPalette`] makes that automatic rather
//!   than a thing to keep in sync.
//! * The output is byte-compatible [`ModelVertex`](crate::ModelVertex) geometry,
//!   so the GUI item pass is the **existing** [`ModelPipeline`](crate::ModelPipeline)
//!   with its existing four bind groups, a different `view_proj`, and nothing
//!   else new.
//!
//! Baking needs a live [`ResourceManager`] (for [`ModelResolver`]), which nothing
//! downstream keeps, so it has to happen at asset-load time — i.e. inside
//! [`BlockModels::build`], which already owns one. See
//! [`item_render`](crate::item_render) for the pose half of the path.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use lodestone_assets::fluid::{FluidState, SpriteUv};
use lodestone_assets::tint::{vanilla_particle_tint_kind, vanilla_tint_kind};
use lodestone_assets::{
    AnimTable, Atlas, AtlasBuilder, AtlasError, AtlasSprite, BakeOptions, BakedQuad, BlockBaker,
    BlockStates, Direction, DisplayTransform, DisplayTransforms, Element, Face, FirstWeight,
    GuiItemContext, GuiLight,
    IconPart, ItemIconBuilder, ModelResolver, ModelTransform, ResolvedModel, ResourceLocation,
    ResourceManager, SpriteLayer, TextureBinding, bake_model_with,
};
use lodestone_model::{BlockStateRegistry, Identifier};

use crate::anim::{AnimFrame, AnimSlotUniform, SpriteAnimation};
use crate::block_resolver::DefaultTints;
use crate::models::{face_of_direction, quad_is_full_face};
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

/// Blocks whose `getFluidState` returns a water **source unconditionally**, with
/// no `waterlogged` property to key off.
///
/// Extracted from the decompiled 26.2 server by scanning every
/// `getFluidState` override under `net/minecraft/world/level/block/` whose body
/// returns `Fluids.WATER` without consulting `WATERLOGGED`. That scan yields
/// exactly these five classes and no others:
///
/// * `KelpBlock` / `KelpPlantBlock` → `Fluids.WATER.getSource(false)`
/// * `SeagrassBlock` / `TallSeagrassBlock` → `Fluids.WATER.getSource(false)`
/// * `BubbleColumnBlock` → `Fluids.WATER.getSource(false)`
///
/// A name list in a version-free crate is only acceptable because it is derived
/// from the jar rather than guessed, and because the alternative — inferring
/// "is underwater" from the model — is not expressible: these states look
/// identical to a land plant.
const UNCONDITIONAL_WATER_BLOCKS: [&str; 5] = [
    "kelp",
    "kelp_plant",
    "seagrass",
    "tall_seagrass",
    "bubble_column",
];

/// Classify a block state into the fluid it exposes, if any.
///
/// `minecraft:water`/`minecraft:lava` carry the fluid directly (via their `level`
/// property); any other block with `waterlogged=true` (stairs, slabs, fences…)
/// carries a water **source**, as do the handful of blocks that hardcode a water
/// `getFluidState` with no such property ([`UNCONDITIONAL_WATER_BLOCKS`]).
/// Everything else is `None`. Pure over the resolved block path + properties, so
/// it is unit-tested without a jar.
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
        _ if UNCONDITIONAL_WATER_BLOCKS.contains(&block_path)
            || props.get("waterlogged").is_some_and(|v| v == "true") =>
        {
            Some(FluidCell {
                kind: FluidKind::Water,
                state: FluidState::source(),
            })
        }
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
    /// Whether this state fully occludes its neighbours on **every** face — i.e.
    /// all six of [`face_occludes`](Self::face_occludes) hold. Leaves, glass and
    /// water are full-cube geometry but cutout/translucent, so they correctly do
    /// **not** cull neighbours.
    pub occludes: bool,
    /// Per-face occlusion, indexed by [`Face::index`](crate::section::Face::index)
    /// (`West, East, Down, Up, North, South`). A face occludes when *some* quad
    /// covers that whole boundary square (`cullface` = its own facing, coplanar,
    /// spanning `1×1`) **and** the sprite that quad samples is fully opaque.
    ///
    /// This is per-**face** rather than per-block on purpose: `grass_block` lays a
    /// transparent `grass_block_side_overlay` decal over four opaque
    /// `grass_block_side` faces, so its *block* layer is `Cutout` while every one
    /// of its six boundary faces is opaque. Judging occlusion by the block layer
    /// called it see-through and produced the water-shoreline bug (see the module
    /// docs and `docs/fluid-rendering.md`).
    pub face_occludes: [bool; 6],
    /// The render pass this block's geometry belongs to, derived from its sprite
    /// alpha (the most transparent layer across its faces).
    pub layer: RenderLayer,
    /// Normalised atlas UVs `[u0, v0, u1, v1]` of the model's `#particle`
    /// sprite — what break/hit particles sample. See
    /// [`BakedModel::particle_uv`](lodestone_assets::bake::BakedModel::particle_uv)
    /// for why this cannot be derived from `quads`.
    pub particle_uv: Option<[f32; 4]>,
    /// The linear-ish `[r, g, b]` multiplier a break/hit particle of this state
    /// applies on top of its `#particle` sprite — vanilla's
    /// `TerrainParticle`'s `rCol *= tintSource.colorAsTerrainParticle(…)`.
    /// `None` for an untinted state (the overwhelming majority).
    ///
    /// This is **not** derivable from the quads' `tint_index` values, for two
    /// independent reasons: the `#particle` sprite is a different texture from
    /// any face, and vanilla's particle tint is a separate virtual method that
    /// deliberately disagrees with the in-world face tint for `grass_block`
    /// (untinted particles over a `block/dirt` sprite) and for `water` /
    /// `bubble_column` (tinted particles over an untinted surface). See
    /// [`vanilla_particle_tint_kind`](lodestone_assets::tint::vanilla_particle_tint_kind).
    ///
    /// Like every other tint in this struct it is the **fixed plains default**,
    /// not the live biome colour, so a state's debris matches the terrain quads
    /// beside it rather than the biome it fell in.
    pub particle_tint: Option<[f32; 3]>,
}

impl StateModel {
    /// An empty (air-like) model: no geometry, no occlusion, solid layer.
    #[must_use]
    fn empty() -> Self {
        StateModel {
            quads: Vec::new(),
            occludes: false,
            face_occludes: [false; 6],
            layer: RenderLayer::Solid,
            particle_uv: None,
            particle_tint: None,
        }
    }
}

/// `0xRRGGBB` -> `[r, g, b]` in `0..=1`, the form a particle's colour
/// multiplier takes. Vanilla does the same division by 255 inline in
/// `TerrainParticle`'s constructor.
fn unpack_rgb(rgb: u32) -> [f32; 3] {
    [
        ((rgb >> 16) & 0xFF) as f32 / 255.0,
        ((rgb >> 8) & 0xFF) as f32 / 255.0,
        (rgb & 0xFF) as f32 / 255.0,
    ]
}

/// One item's baked inventory geometry: the 3-D mini-block a hotbar/inventory
/// slot draws for it.
///
/// The quads' UVs index [`BlockModels::atlas`] and their `tint_index` values
/// index [`BlockModels::tint_palette`], exactly like a block state's — that
/// sharing is the whole point (see the [module docs](self)). What is *not*
/// baked in is the pose: `transform` is a render-time matrix
/// ([`display_matrix`](crate::display_matrix)), because vanilla applies it on
/// the pose stack and because the same geometry is reused for the in-hand and
/// dropped forms under different slots' transforms.
#[derive(Debug, Clone)]
pub struct ItemGeometry {
    /// The baked quads, with absolute atlas UVs and palette tint indices.
    pub quads: Vec<BakedQuad>,
    /// The model's `display.gui` transform (the isometric pose), verbatim from
    /// the JSON — the `/16` and vanilla's clamps are applied by
    /// [`display_matrix`](crate::display_matrix), not here.
    ///
    /// The same value as `display.get(DisplaySlot::Gui)`; kept for the GUI
    /// callers that predate `display` and want exactly this slot.
    pub transform: DisplayTransform,
    /// **Every** `display` slot of the model, so the *same* baked quads can be
    /// posed as an inventory icon, a dropped item, a held item or a hat.
    ///
    /// This is the field that made the in-world item forms possible: before it,
    /// only `gui` survived the asset boundary, so
    /// [`crate::entity::BLOCK_ITEM_GROUND`] and its sibling had to name
    /// `block/block`'s and `item/generated`'s `ground` numbers as constants and
    /// pick between them by [`GuiLight`]. Prefer
    /// [`crate::entity::ground_transform`], which reads this and falls back to
    /// those constants only when the pack declared nothing.
    ///
    /// Verbatim JSON numbers, like `transform`.
    pub display: DisplayTransforms,
    /// The GUI lighting mode: [`GuiLight::Side`] keeps the per-face directional
    /// constants, [`GuiLight::Front`] flattens them.
    pub gui_light: GuiLight,
}

/// One item's [`IconPart::Model`], discovered *before* the atlas is stitched so
/// the textures it reaches can be seeded into it.
#[derive(Debug, Clone)]
struct ItemModelPart {
    item: ResourceLocation,
    model: ResourceLocation,
    transform: DisplayTransform,
    display: DisplayTransforms,
    gui_light: GuiLight,
}

/// One item's [`IconPart::Sprite`]: the flat `builtin/generated` layer stack that
/// [`extruded_sprite_geometry`] turns into vanilla's thin extruded slab.
///
/// Discovered in the same pass as [`ItemModelPart`], and for the same reason —
/// the layer sprites live under `textures/item/`, which no *blockstate* reaches,
/// so they have to be seeded into the atlas before it is stitched.
#[derive(Debug, Clone)]
struct ItemSpritePart {
    item: ResourceLocation,
    layers: Vec<SpriteLayer>,
    display: DisplayTransforms,
}

/// Resolve every item definition in the pack stack and keep the ones whose GUI
/// icon is a 3-D model or a flat sprite stack.
///
/// Resolution uses [`GuiItemContext`], not the default context: a handful of
/// items (`spyglass`, `trident`, the spears, the bundles) branch on
/// `minecraft:display_context` and would otherwise resolve to their *in-hand*
/// model. Items that fail to resolve, or that draw through a code-driven special
/// renderer, are simply absent — they are not this path's business.
///
/// An item contributes to **at most one** of the two lists, model first. A
/// `composite` icon mixing a model part and a sprite part would otherwise bake
/// two disjoint geometries under one id, and `BlockModels::items` is keyed by id.
///
/// A `composite` icon can hold several model parts; only the **first** is kept,
/// and the item is named in [`BlockModels::item_bake_misses`]. In vanilla 26.2
/// that is the 16 beds and nothing else: `items/<colour>_bed.json` composites
/// `block/<colour>_bed_head` with `block/<colour>_bed_foot` plus a per-part
/// `transformation` (`translation [0, 0, 1]`) that positions the foot behind the
/// head. `lodestone_assets`'s [`IconPart::Model`] does not carry that
/// transformation — `item_model.rs` never parses it — so concatenating the parts
/// would stack the foot *inside* the head and z-fight, which is strictly worse
/// than drawing the head alone. Keeping the first part and recording the item is
/// the honest option until the parser carries the per-part transform.
fn collect_item_model_parts(
    manager: &ResourceManager,
) -> (Vec<ItemModelPart>, Vec<ItemSpritePart>, Vec<String>) {
    let builder = ItemIconBuilder::new(manager);
    let mut parts = Vec::new();
    let mut sprites = Vec::new();
    let mut notes = Vec::new();
    for id in item_ids(manager) {
        let Ok(icon) = builder.icon_with(&id, &GuiItemContext) else {
            continue;
        };
        let mut models = icon.parts.iter().filter_map(|p| match p {
            IconPart::Model {
                model,
                transform,
                gui_light,
            } => Some(ItemModelPart {
                item: id.clone(),
                model: model.clone(),
                transform: *transform,
                // `ItemIcon`-level rather than per-part: see `ItemIcon::display`
                // for why. It is the *first drawable part's* map, and this loop
                // keeps the first model part, so the two agree in every case
                // that reaches a pixel — including the composite items noted
                // below, where only the first part is baked either way.
                display: icon.display,
                gui_light: *gui_light,
            }),
            _ => None,
        });
        if let Some(first) = models.next() {
            let extra = models.count();
            if extra > 0 {
                notes.push(format!(
                    "{id}: composite icon has {} model parts, but IconPart::Model carries no \
                     per-part transformation; only the first is baked",
                    extra + 1
                ));
            }
            parts.push(first);
            continue;
        }
        // No model part: the flat `builtin/generated` path. Every layer of the
        // *first* sprite part is kept — vanilla's `ItemModelGenerator.bake` walks
        // `layer0..layer4` and concatenates each layer's extrusion into one quad
        // collection, so a two-layer item (a dyed leather boot, an enchanted book
        // glint base) is two stacked slabs, not one.
        if let Some(IconPart::Sprite { layers }) =
            icon.parts.iter().find(|p| matches!(p, IconPart::Sprite { .. }))
        {
            sprites.push(ItemSpritePart {
                item: id.clone(),
                layers: layers.clone(),
                display: icon.display,
            });
        }
    }
    (parts, sprites, notes)
}

// ---------------------------------------------------------------------------
// Flat sprite items: vanilla's `ItemModelGenerator`
// ---------------------------------------------------------------------------
//
// A `builtin/generated` item model carries **no elements**. Vanilla synthesises
// them in `net.minecraft.client.resources.model.cuboid.ItemModelGenerator`
// (26.2), and the numbers below are that class read directly rather than
// approximated:
//
// * `MIN_Z = 7.5`, `MAX_Z = 8.5` — a 1/16-block-thick slab straddling the block
//   centre, which is why a dropped sword is a *slab* and not a zero-area quad
//   that vanishes edge-on.
// * a `SOUTH` face with UVs `(0, 0, 16, 16)` and a `NORTH` face with UVs
//   `(16, 0, 0, 16)` — the `u` flip is what keeps the back of the sprite from
//   reading mirrored.
// * side geometry walked from the sprite's **alpha outline**, one quad per
//   boundary texel, inset by `UV_SHRINK = 0.1` texel so an edge quad samples the
//   opaque interior rather than the transparent texel next door.
// * `guiLight() == FRONT`.

/// Vanilla `ItemModelGenerator.MIN_Z`: the slab's near face, in model units.
const SPRITE_MIN_Z: f32 = 7.5;
/// Vanilla `ItemModelGenerator.MAX_Z`: the slab's far face, in model units.
const SPRITE_MAX_Z: f32 = 8.5;
/// Vanilla `ItemModelGenerator.UV_SHRINK`: the per-edge UV inset, in **sprite
/// texels** (not model units — it is applied before the `xScale`/`yScale`).
const SPRITE_UV_SHRINK: f32 = 0.1;
/// Vanilla `ItemModelGenerator.SOUTH_FACE_UVS`.
const SPRITE_SOUTH_UVS: [f32; 4] = [0.0, 0.0, 16.0, 16.0];
/// Vanilla `ItemModelGenerator.NORTH_FACE_UVS` — note the reversed `u`.
const SPRITE_NORTH_UVS: [f32; 4] = [16.0, 0.0, 0.0, 16.0];
/// The texture variable the synthesised elements reference.
const SPRITE_TEXTURE_VAR: &str = "layer";

/// Vanilla `ItemModelGenerator.SideDirection`: which way an outline texel's edge
/// quad faces, and which neighbour texel's transparency creates it.
///
/// The `Direction` mapping is **deliberately counter-intuitive and is vanilla's**:
/// `LEFT` maps to `EAST` and `RIGHT` to `WEST`. Do not "fix" it — the enum's
/// `direction` is used for two different things (the neighbour step in
/// `checkTransition`, and the facing handed to the face bakery), and the bakery
/// recomputes the true facing from the vertices anyway.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum SideDirection {
    Up,
    Down,
    Left,
    Right,
}

impl SideDirection {
    const ALL: [Self; 4] = [Self::Up, Self::Down, Self::Left, Self::Right];

    /// Vanilla `SideDirection.direction`.
    fn direction(self) -> Direction {
        match self {
            Self::Up => Direction::Up,
            Self::Down => Direction::Down,
            Self::Left => Direction::East,
            Self::Right => Direction::West,
        }
    }

    /// Vanilla `SideDirection.isHorizontal` — which controls whether the edge
    /// quad's `v` range runs up or down, not whether the quad itself is
    /// horizontal.
    fn is_horizontal(self) -> bool {
        matches!(self, Self::Up | Self::Down)
    }

    /// The `(step_x, step_y)` of [`Self::direction`] in *image* space, which
    /// `checkTransition` **subtracts** to find the neighbour texel.
    fn step(self) -> (i32, i32) {
        match self {
            // Direction::Up  = (0, 1, 0) -> neighbour is (x, y - 1), the texel above.
            Self::Up => (0, 1),
            // Direction::Down = (0, -1, 0) -> neighbour is (x, y + 1), below.
            Self::Down => (0, -1),
            // Direction::East = (1, 0, 0) -> neighbour is (x - 1, y), to the left.
            Self::Left => (1, 0),
            // Direction::West = (-1, 0, 0) -> neighbour is (x + 1, y), to the right.
            Self::Right => (-1, 0),
        }
    }
}

/// Whether texel `(x, y)` of physical frame `frame` is fully transparent, reading
/// the sprite's pixels back out of the **stitched atlas**.
///
/// Vanilla's `SpriteContents.isTransparent` is `ARGB.alpha(pixel) == 0` — strictly
/// zero, not a cutout threshold — and out-of-bounds counts as transparent, which
/// is what closes the outline at the sprite border.
fn sprite_texel_transparent(atlas: &Atlas, sprite: &AtlasSprite, frame: u32, x: i32, y: i32) -> bool {
    if x < 0 || y < 0 || x >= sprite.width as i32 || y >= sprite.frame_height as i32 {
        return true;
    }
    let Some([fx, fy, _, _]) = sprite.frame_pixel_rect(frame) else {
        return true;
    };
    #[allow(clippy::cast_sign_loss)]
    let (px, py) = (fx + x as u32, fy + y as u32);
    let i = ((py * atlas.width + px) * 4) as usize;
    atlas.rgba.get(i + 3).copied().unwrap_or(0) == 0
}

/// Vanilla `ItemModelGenerator.getSideFaces`: every `(facing, x, y)` at which an
/// opaque texel borders a transparent one.
///
/// Vanilla collects these into a `HashSet` and therefore emits them in an
/// unspecified order. A [`BTreeSet`] is used instead so a given pack stack bakes
/// a byte-identical item set, which the rest of this module already guarantees.
/// The quads are opaque and mutually non-overlapping, so order is not observable.
///
/// Vanilla unions the outline over `getUniqueFrames()` — the frames the animation
/// metadata actually plays. We union over every *physical* frame in the strip,
/// which is a superset: a frame present in the PNG but never played can only add
/// an edge quad, never remove one. Almost every item sprite is static anyway.
fn sprite_side_faces(atlas: &Atlas, sprite: &AtlasSprite) -> BTreeSet<(SideDirection, u32, u32)> {
    let mut faces = BTreeSet::new();
    for frame in 0..sprite.frame_count {
        for y in 0..sprite.frame_height {
            for x in 0..sprite.width {
                #[allow(clippy::cast_possible_wrap)]
                let (ix, iy) = (x as i32, y as i32);
                if sprite_texel_transparent(atlas, sprite, frame, ix, iy) {
                    continue;
                }
                for facing in SideDirection::ALL {
                    let (sx, sy) = facing.step();
                    if sprite_texel_transparent(atlas, sprite, frame, ix - sx, iy - sy) {
                        faces.insert((facing, x, y));
                    }
                }
            }
        }
    }
    faces
}

/// A [`Face`] referencing the synthesised layer texture, with explicit UVs.
fn sprite_face(uv: [f32; 4]) -> Face {
    Face {
        uv: Some(uv),
        texture: format!("#{SPRITE_TEXTURE_VAR}"),
        // An item slab has no neighbours to be culled by. Vanilla uses
        // `addUnculledFace` for every one of these.
        cullface: None,
        rotation: 0,
        tintindex: None,
    }
}

/// A single-face [`Element`] with vanilla's `shade: true`.
fn sprite_element(from: [f32; 3], to: [f32; 3], faces: HashMap<Direction, Face>) -> Element {
    Element {
        from,
        to,
        rotation: None,
        faces,
        // Vanilla passes `shade = true` in `ItemLayerKey.compute`. It is inert for
        // the GUI and drop paths (both pose with `GuiLight::Front`, which flattens
        // the per-face constants) but is the honest value to record.
        shade: Some(true),
        light_emission: None,
        name: None,
    }
}

/// Vanilla `ItemModelGenerator.bakeExtrudedSprite` + `bakeSideFaces`: the
/// synthesised elements for **one** sprite layer.
fn sprite_layer_elements(atlas: &Atlas, sprite: &AtlasSprite) -> Vec<Element> {
    let mut elements = Vec::new();

    // The front and back of the slab. One element with two faces rather than
    // vanilla's two `bakeQuad` calls; `bake_model_with` emits one quad per face,
    // so the output is the same pair.
    let mut faces = HashMap::new();
    faces.insert(Direction::South, sprite_face(SPRITE_SOUTH_UVS));
    faces.insert(Direction::North, sprite_face(SPRITE_NORTH_UVS));
    elements.push(sprite_element(
        [0.0, 0.0, SPRITE_MIN_Z],
        [16.0, 16.0, SPRITE_MAX_Z],
        faces,
    ));

    // The edges. `x_scale`/`y_scale` map the sprite's own resolution onto the
    // 0..16 model grid, so a 32x32 pack texture extrudes at the same physical
    // size as a 16x16 one. Note `frame_height`, not `height`: vanilla's
    // `SpriteContents.height()` is one *frame*, and using the whole animation
    // strip would squash every edge quad toward the sprite's bottom.
    if sprite.width == 0 || sprite.frame_height == 0 {
        return elements;
    }
    let x_scale = 16.0 / sprite.width as f32;
    let y_scale = 16.0 / sprite.frame_height as f32;

    for (facing, tx, ty) in sprite_side_faces(atlas, sprite) {
        let (x, y) = (tx as f32, ty as f32);
        let u0 = x + SPRITE_UV_SHRINK;
        let u1 = x + 1.0 - SPRITE_UV_SHRINK;
        let (v0, v1) = if facing.is_horizontal() {
            (y + SPRITE_UV_SHRINK, y + 1.0 - SPRITE_UV_SHRINK)
        } else {
            (y + 1.0 - SPRITE_UV_SHRINK, y + SPRITE_UV_SHRINK)
        };

        let (mut start_x, mut start_y, mut end_x, mut end_y) = (x, y, x, y);
        match facing {
            SideDirection::Up => end_x += 1.0,
            SideDirection::Down => {
                end_x += 1.0;
                start_y += 1.0;
                end_y += 1.0;
            }
            SideDirection::Left => end_y += 1.0,
            SideDirection::Right => {
                start_x += 1.0;
                end_x += 1.0;
                end_y += 1.0;
            }
        }
        start_x *= x_scale;
        end_x *= x_scale;
        start_y *= y_scale;
        end_y *= y_scale;
        // Image `y` grows downward, model `y` upward.
        start_y = 16.0 - start_y;
        end_y = 16.0 - end_y;

        // Verbatim from vanilla, including the cases where `from > to` on one
        // axis (`Left`/`Right` always, because the flip above reverses the pair).
        // That is not a bug to normalise away: `bake_face` derives the true facing
        // from the resulting vertices and re-winds, exactly as
        // `FaceBakery.bakeQuad` does, so the reversed box is what produces the
        // correct outward normal. Clamping to min/max here would invert every
        // vertical edge quad.
        let (from, to) = match facing {
            SideDirection::Up => (
                [start_x, start_y, SPRITE_MIN_Z],
                [end_x, start_y, SPRITE_MAX_Z],
            ),
            SideDirection::Down => {
                ([start_x, end_y, SPRITE_MIN_Z], [end_x, end_y, SPRITE_MAX_Z])
            }
            SideDirection::Left => (
                [start_x, start_y, SPRITE_MIN_Z],
                [start_x, end_y, SPRITE_MAX_Z],
            ),
            SideDirection::Right => {
                ([end_x, start_y, SPRITE_MIN_Z], [end_x, end_y, SPRITE_MAX_Z])
            }
        };

        let mut faces = HashMap::new();
        faces.insert(
            facing.direction(),
            sprite_face([u0 * x_scale, v0 * y_scale, u1 * x_scale, v1 * y_scale]),
        );
        elements.push(sprite_element(from, to, faces));
    }

    elements
}

/// Bake the extruded slab for a whole layer stack into quads against `atlas`.
///
/// Returns `None` when no layer resolves to an atlas sprite, which is the same
/// "renders nothing" outcome vanilla's `QuadCollection.EMPTY` produces.
///
/// # Tint
///
/// Every quad comes out **untinted**, and that is a deliberate narrowing rather
/// than an oversight. Vanilla stamps `tintIndex = layerIndex` on these quads and
/// resolves the colour from the item's own `TintSource` list (leather dye, potion
/// colour, spawn-egg shell, map marker). A `BakedQuad::tint_index` here indexes
/// `BlockModels::tint_palette` — the *block* tint palette — so writing a layer
/// index into it would look up an unrelated grass or foliage green. Untinted
/// white is correct for the overwhelming majority of items and wrong only in the
/// dyed minority, whereas the alternative is wrong loudly and everywhere.
fn extruded_sprite_geometry(atlas: &Atlas, layers: &[SpriteLayer]) -> Option<Vec<BakedQuad>> {
    let mut quads = Vec::new();
    for layer in layers {
        let Some(sprite) = atlas.sprite(&layer.sprite) else {
            continue;
        };
        let mut textures = HashMap::new();
        textures.insert(
            SPRITE_TEXTURE_VAR.to_string(),
            TextureBinding::Resolved(layer.sprite.clone()),
        );
        // A synthesised `ResolvedModel` so the real, vanilla-derived face bakery
        // does the work: winding, `calculate_facing`, the UV-index mapping and the
        // animation slot all come out identical to a hand-written model JSON's.
        // Rolling vertices by hand here is precisely how a subtly inside-out
        // sprite would ship.
        let model = ResolvedModel {
            textures,
            elements: sprite_layer_elements(atlas, sprite),
            ambient_occlusion: false,
            gui_light: GuiLight::Front,
            display: HashMap::new(),
            texture_size: [sprite.width, sprite.frame_height],
            builtin: None,
        };
        let baked = bake_model_with(
            &model,
            atlas,
            ModelTransform::default(),
            &BakeOptions::default(),
        )
        .ok()?;
        quads.extend(baked);
    }
    (!quads.is_empty()).then_some(quads)
}

/// Discovers item ids by scanning for `assets/<ns>/items/<path>.json`, mirroring
/// `lodestone_assets::item_atlas`'s private scan. Sorted and deduplicated so a
/// given pack stack bakes a byte-identical item set.
fn item_ids(manager: &ResourceManager) -> Vec<ResourceLocation> {
    let mut ids = BTreeSet::new();
    for path in manager.list("assets/") {
        let Some(rest) = path.strip_prefix("assets/") else {
            continue;
        };
        let Some((namespace, tail)) = rest.split_once('/') else {
            continue;
        };
        let Some(item_path) = tail
            .strip_prefix("items/")
            .and_then(|p| p.strip_suffix(".json"))
        else {
            continue;
        };
        if let Ok(loc) = ResourceLocation::parse(&format!("{namespace}:{item_path}")) {
            ids.insert(loc);
        }
    }
    ids.into_iter().collect()
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
    /// Per-slot sprite animations, ordered by slot id (index `i` is slot
    /// `i + 1`; slot `0` is static and has no entry). A baked quad's
    /// [`anim`](lodestone_assets::bake::BakedQuad::anim) byte selects one; the
    /// renderer samples each at the current tick to build the shader uniform.
    animations: Vec<SpriteAnimation>,
    /// Normalised per-frame V height for each slot, parallel to
    /// [`animations`](Self::animations). Turns a sampled region index into the
    /// vertical UV offset the shader adds to a quad's baked frame-0 V.
    anim_frame_v: Vec<f32>,
    /// Normalised atlas UVs `[u0, v0, u1, v1]` of each `destroy_stage_N`
    /// crack-overlay sprite, indexed by stage `0..CRACK_STAGE_COUNT`. The
    /// mining crack pass re-draws a block's model geometry sampling these.
    crack_stages: [[f32; 4]; CRACK_STAGE_COUNT],
    /// Baked inventory geometry for every item whose icon is a 3-D model, keyed
    /// by item id (`minecraft:stone`). See the [module docs](self) for why it
    /// lives on a type called `BlockModels`.
    items: HashMap<ResourceLocation, ItemGeometry>,
    /// Item models that did **not** bake, named. Recorded rather than fatal, for
    /// the same reason `ItemAtlasReport::missing_special_bases` is: a texture a
    /// vanilla blockstate never reaches (or that a resource pack drops) is an
    /// expected absence, not a reason to refuse to render the world.
    item_bake_misses: Vec<String>,
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
        // Item models are discovered before the atlas is stitched so their
        // textures can be seeded into it (see `build_complete_atlas`), and
        // reused after it to bake, so the item definitions are resolved once.
        let (item_parts, sprite_parts, mut item_bake_misses) = collect_item_model_parts(manager);
        let atlas = build_complete_atlas(manager, &resolver, &item_parts, &sprite_parts)?;

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
                    // The tint a *particle* of this state takes. Resolved from
                    // the particle-specific lookup, not from the quads' tint
                    // indices: the `#particle` sprite is a different texture
                    // from any face (see `particle_uv`), and vanilla's
                    // `colorAsTerrainParticle` deliberately disagrees with the
                    // in-world face tint for `grass_block` and for water.
                    let particle_tint = resolved
                        .as_ref()
                        .and_then(|r| tints.color(vanilla_particle_tint_kind(r.block, r.properties)))
                        .map(unpack_rgb);
                    let layer = block_layer(&sprite_rects, &quads);
                    let face_occludes = face_occlusion(&sprite_rects, &quads);
                    StateModel {
                        quads,
                        occludes: face_occludes.iter().all(|o| *o),
                        face_occludes,
                        layer,
                        particle_uv,
                        particle_tint,
                    }
                }
                _ => StateModel::empty(),
            };
            models.push(sm);

            let fluid = resolved.and_then(|r| classify_fluid(r.block.path(), r.properties));
            fluids.push(fluid);
        }

        // Item geometry, baked against the same atlas and interning through the
        // same `palette` as the state loop above — the two facts that let the
        // GUI item pass reuse the terrain pipeline wholesale. It runs after the
        // states (rather than inside that loop) only because items are keyed by
        // id, not by state id; everything it shares is what matters.
        let mut items = HashMap::with_capacity(item_parts.len());
        for part in &item_parts {
            let resolved = match resolver.resolve(&part.model) {
                Ok(r) => r,
                Err(e) => {
                    item_bake_misses.push(format!("{} ({}): {e}", part.item, part.model));
                    continue;
                }
            };
            // `ModelTransform::default()`: that transform is the *blockstate*
            // placement rotation, and an item has no blockstate. The item's pose
            // is `part.transform`, applied at draw time by `item_render`.
            let mut quads = match bake_model_with(
                &resolved,
                &atlas,
                ModelTransform::default(),
                &BakeOptions::default(),
            ) {
                Ok(q) => q,
                Err(e) => {
                    item_bake_misses.push(format!("{} ({}): {e}", part.item, part.model));
                    continue;
                }
            };
            // Rewrite raw model tint indices into palette indices, as the state
            // loop does. An item has no `registry.resolve(state_id)`, so the
            // block identity comes from the item id (item `minecraft:grass_block`
            // → block `minecraft:grass_block`) and the properties are empty —
            // an inventory icon is always the default state.
            if let Ok(block) = part.item.to_string().parse::<Identifier>() {
                let no_props = BTreeMap::new();
                for quad in &mut quads {
                    if let Some(raw) = quad.tint_index {
                        let kind = vanilla_tint_kind(&block, raw, &no_props);
                        quad.tint_index =
                            tints.color(kind).map(|rgb| i32::from(palette.intern(rgb)));
                    }
                }
            }
            items.insert(
                part.item.clone(),
                ItemGeometry {
                    quads,
                    transform: part.transform,
                    display: part.display,
                    gui_light: part.gui_light,
                },
            );
        }

        // Flat sprite items, extruded into vanilla's thin slab. These are the
        // *majority* of items — every tool, ingot, gem and food — and before this
        // existed each of them reached zero pixels on the dropped-item pass, which
        // skips any item with no baked geometry.
        for part in &sprite_parts {
            let Some(quads) = extruded_sprite_geometry(&atlas, &part.layers) else {
                item_bake_misses.push(format!(
                    "{}: sprite icon has {} layer(s), none of which stitched into the atlas",
                    part.item,
                    part.layers.len()
                ));
                continue;
            };
            items.insert(
                part.item.clone(),
                ItemGeometry {
                    quads,
                    // `item/generated` declares no `display.gui`, so vanilla poses a
                    // flat item with `ItemTransform.NO_TRANSFORM`: the identity. The
                    // 0..16 slab then maps exactly onto the 16 px slot, which is why
                    // a flat inventory icon fills its cell edge to edge while a
                    // block item (scale 0.625) does not.
                    transform: DisplayTransform::default(),
                    // The real slots off `item/generated` (and `item/handheld`
                    // for the tools): `ground` [0,2,0]/0.5,
                    // `thirdperson_righthand`, `firstperson_righthand`, `head`,
                    // `fixed`. Notably **no** `gui`, which is why `transform`
                    // above is the identity and not read from here.
                    display: part.display,
                    // Vanilla `ItemModelGenerator.guiLight() == FRONT`, matching
                    // `item/generated`'s own `"gui_light": "front"`. This is also
                    // what routes the drop pass to `GENERATED_ITEM_GROUND`
                    // (translation [0, 2, 0], scale 0.5) instead of the block
                    // items' `[0, 3, 0]` / 0.25 — see `ground_transform_for`.
                    gui_light: GuiLight::Front,
                },
            );
        }

        let water_sprites = resolve_fluid_sprites(&atlas, FluidKind::Water);
        let lava_sprites = resolve_fluid_sprites(&atlas, FluidKind::Lava);
        let crack_stages = std::array::from_fn(|stage| {
            let uv = sprite_uv(&atlas, &crack_stage_location(stage));
            [uv.min[0], uv.min[1], uv.max[0], uv.max[1]]
        });

        // Resolve the atlas's animation slots into GPU-free timelines the
        // renderer drives with `anim.rs`. Ordered by slot id, so slot `s`'s data
        // is `animations[s - 1]` — matching the byte the baker stamped on quads.
        let anim_table = AnimTable::from_atlas(&atlas);
        let mut animations = Vec::with_capacity(anim_table.len());
        let mut anim_frame_v = Vec::with_capacity(anim_table.len());
        for slot in anim_table.slots() {
            animations.push(SpriteAnimation {
                frames: slot
                    .frames
                    .iter()
                    .map(|f| AnimFrame {
                        region: f.index,
                        hold_ticks: f.hold_ticks,
                    })
                    .collect(),
                interpolate: slot.interpolate,
            });
            anim_frame_v.push(slot.frame_v);
        }

        Ok(Self {
            atlas,
            models,
            empty: StateModel::empty(),
            fluids,
            water_sprites,
            lava_sprites,
            tint_palette: palette.colors().to_vec(),
            animations,
            anim_frame_v,
            crack_stages,
            items,
            item_bake_misses,
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

    /// The per-slot sprite animations, ordered by slot id (index `i` is slot
    /// `i + 1`). A baked quad's
    /// [`anim`](lodestone_assets::bake::BakedQuad::anim) byte of `s` refers to
    /// `sprite_animations()[s - 1]`. Empty when the pack has no animated block
    /// sprites.
    #[must_use]
    pub fn sprite_animations(&self) -> &[SpriteAnimation] {
        &self.animations
    }

    /// The per-slot normalised frame height (`frame_height / atlas_height`),
    /// aligned with [`sprite_animations`](Self::sprite_animations): entry `i`
    /// scales slot `i + 1`'s sampled region index into a concrete V offset.
    #[must_use]
    pub fn anim_frame_v(&self) -> &[f32] {
        &self.anim_frame_v
    }

    /// The number of animation slots (excludes the static sentinel `0`).
    #[must_use]
    pub fn anim_slot_count(&self) -> usize {
        self.animations.len()
    }

    /// Build the per-slot animation uniform array for game `tick`, ready to
    /// upload to the model/fluid shaders' animation bind group.
    ///
    /// Index `0` is the static sentinel (all-zero: no offset, no blend); index
    /// `s` (`1..=anim_slot_count`) is slot `s`, resolved by sampling its
    /// timeline at `tick` and converting the region indices into concrete V
    /// offsets via the slot's per-frame height. A quad's `anim` byte indexes
    /// this array directly in the shader, so a static quad (`anim == 0`) reads a
    /// no-op. The array always has at least one entry (the sentinel) so the
    /// uniform buffer is never zero-sized.
    #[must_use]
    pub fn anim_slot_uniforms(&self, tick: u64) -> Vec<AnimSlotUniform> {
        let mut out = Vec::with_capacity(self.animations.len() + 1);
        out.push(AnimSlotUniform::static_slot());
        for (anim, frame_v) in self.animations.iter().zip(&self.anim_frame_v) {
            out.push(AnimSlotUniform::from_sample(anim.sample(tick), *frame_v));
        }
        out
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

    /// The baked geometry of an item (`minecraft:stone`), or `None` for an item
    /// whose icon is a code-driven [`IconPart::Special`] renderer (chests,
    /// shulkers, shields, banners) or which resolves to nothing at all.
    ///
    /// **A flat sprite item is present**, not absent: its `builtin/generated`
    /// layer stack is extruded into vanilla's slab and inserted into this same map
    /// (see the module docs). This doc comment used to say "`None` for an item
    /// whose icon is a flat sprite", which was true until `9980a96` and was then
    /// cited as the root cause of four separate rendering issues — none of which
    /// it was.
    #[must_use]
    pub fn item(&self, item: &ResourceLocation) -> Option<&ItemGeometry> {
        self.items.get(item)
    }

    /// The baked quads of an item's icon — 3-D model or extruded sprite slab
    /// (empty when it has neither).
    /// Pose them with [`gui_item_pose`](crate::gui_item_pose) and mesh them with
    /// [`mesh_item_quads`](crate::mesh_item_quads); their UVs index
    /// [`atlas`](Self::atlas) and their tints [`tint_palette`](Self::tint_palette),
    /// exactly like [`quads`](Self::quads).
    #[must_use]
    pub fn item_quads(&self, item: &ResourceLocation) -> &[BakedQuad] {
        self.items.get(item).map_or(&[], |g| &g.quads)
    }

    /// The number of items with baked geometry of either kind.
    #[must_use]
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    /// Every item with baked geometry, as `(id, geometry)` pairs — 3-D models and
    /// extruded sprite slabs alike.
    ///
    /// The counterpart to [`item`](Self::item) for consumers that need to take a
    /// **snapshot** rather than answer one lookup. Block *states* are enumerable
    /// through [`state_count`](Self::state_count) because their ids are a dense
    /// `0..n` (which is how [`CrackResolver::from_models`](crate::CrackResolver)
    /// captures them); item ids are [`ResourceLocation`]s with no such index
    /// space, so without this a consumer outside this crate has no way to learn
    /// *which* items have geometry and is forced to thread a `&BlockModels`
    /// through every frame instead.
    ///
    /// Order is the backing `HashMap`'s and therefore arbitrary and unstable —
    /// sort by id if you need determinism.
    pub fn items(&self) -> impl Iterator<Item = (&ResourceLocation, &ItemGeometry)> {
        self.items.iter()
    }

    /// Item models that failed to bake, named (`"<item> (<model>): <error>"`).
    ///
    /// Empty for a complete vanilla pack except where a texture is genuinely
    /// unreachable from any blockstate. Recorded rather than fatal: a resource
    /// pack that drops a texture should degrade one hotbar icon, not refuse to
    /// build the world's geometry.
    #[must_use]
    pub fn item_bake_misses(&self) -> &[String] {
        &self.item_bake_misses
    }

    /// Whether a state fully occludes its neighbours (every one of its six
    /// boundary faces is a full opaque face).
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

    /// The `[r, g, b]` multiplier a break/hit particle of `state_id` applies on
    /// top of its `#particle` sprite, or `None` for an untinted state — see
    /// [`StateModel::particle_tint`] for why this is a separate lookup from the
    /// quads' tint indices, and why leaving it out renders foliage debris white.
    #[must_use]
    pub fn particle_tint(&self, state_id: u32) -> Option<[f32; 3]> {
        self.state(state_id).particle_tint
    }

    /// How many states carry a [`particle_tint`](Self::particle_tint).
    ///
    /// Exposed as an **anti-vacuity check**: a table that resolved no tints at
    /// all still satisfies "no state's debris is the wrong colour", so a gate on
    /// particle tinting has to be able to prove the table is populated. On a
    /// complete vanilla pack this is in the thousands (every leaf, grass, fern,
    /// stem, vine and redstone-wire state).
    #[must_use]
    pub fn particle_tinted_state_count(&self) -> usize {
        self.models
            .iter()
            .filter(|m| m.particle_tint.is_some())
            .count()
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
///
/// It additionally seeds every texture reachable from an **item** model
/// (`item_parts`), mirroring the explicit fluid and crack-stage seeding below.
/// Blockstate coverage already reaches 751 of 26.2's 753 item models, so this is
/// a small top-up rather than a second corpus — but the leftovers are real:
/// `structure_block`'s blockstate names four *mode-specific* models, so the
/// plain `block/structure_block` texture its item model uses is reachable from
/// no blockstate at all.
fn build_complete_atlas(
    manager: &ResourceManager,
    resolver: &ModelResolver,
    item_parts: &[ItemModelPart],
    sprite_parts: &[ItemSpritePart],
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
    // Item models: the GUI item pass bakes against this same atlas, so anything
    // an item model samples has to be in it.
    for part in item_parts {
        if let Ok(model) = resolver.resolve(&part.model) {
            for binding in model.textures.values() {
                if let TextureBinding::Resolved(loc) = binding {
                    textures.insert(loc.clone());
                }
            }
        }
    }
    // Flat sprite items: their `layerN` textures live under `textures/item/` and
    // are reached by no blockstate and no *model*'s texture map either (a
    // `builtin/generated` model's variables resolve, but nothing above resolved it
    // as a model). Without this, `extruded_sprite_geometry` finds no sprite and
    // every tool, ingot, gem and food renders nothing.
    for part in sprite_parts {
        for layer in &part.layers {
            textures.insert(layer.sprite.clone());
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

/// Per-face occlusion: for each of the six [`Face`] indices, whether some quad
/// covers that whole boundary square with a **fully opaque** sprite.
///
/// # Why this is per-face and not `is_full_cube(quads) && layer == Solid`
///
/// That older rule was wrong twice over, and both errors landed on the same
/// block. `grass_block[snowy=false]` bakes **ten** quads — six opaque cube faces
/// plus four coplanar `grass_block_side_overlay` decals whose sprite is binary
/// alpha (measured: exactly `{0, 255}`). So:
///
/// * `is_full_cube` demands exactly six quads and saw ten → `false`;
/// * `block_layer` takes the *most transparent* sprite over all quads, so the
///   decal dragged the whole block to `Cutout` → `layer != Solid`.
///
/// Vanilla does not derive occlusion from textures at all: `BlockBehaviour`'s
/// `initCache` sets `occlusionShape = canOcclude ? getOcclusionShape(...) :
/// Shapes.empty()`, and `canOcclude` is a `Properties` flag cleared only by
/// `noOcclusion()`/`noCollision()`. `GRASS_BLOCK`'s properties call neither, so
/// vanilla occludes; leaves, glass, ice, slime, honey, spawners, grates and
/// `powder_snow` all call `noOcclusion()`. That flag is Java, not data — it is in
/// no report — so a renderer without it must approximate. Asking *per face*
/// whether an opaque quad covers the boundary is the closest approximation the
/// baked geometry supports.
///
/// # The hollow-shell exception
///
/// One block defeats "opaque boundary face ⇒ occludes": `powder_snow` is six
/// **thin shells** (`[0,15.998,0]..[16,16,16]` and its five mirrors), each drawn
/// on both sides with an opaque sprite. Its outward faces do sit on the boundary,
/// so the rule above would call it occluding — but vanilla marks it
/// `noOcclusion()`, and for a reason we can detect: a model that draws its own
/// **interior** is see-through from inside, so culling the block behind it opens a
/// hole. The tell is a quad whose facing is the *opposite* of its `cullface`
/// (powder_snow's east shell carries a `west`-facing quad with `cullface: east`).
/// Such a quad vetoes occlusion on the face it lines.
///
/// Measured over all 32,366 states of 26.2: that veto fires on **exactly**
/// `powder_snow` and on **zero** blocks that occluded under the old rule. With it,
/// the complete set of states whose occlusion changes is
/// `{grass_block[snowy=false]}` — the one block the bug was about — and no state
/// anywhere *loses* occlusion, so the change cannot open a new hole.
///
/// The visible cost of the old rule was the reported water bug: a lake's shore
/// blocks are `grass_block`, so every shoreline cell was treated as air-like by
/// the fluid mesher — its side faces survived culling and drew the animated
/// `water_flow` sprite, and its corner heights averaged *down* toward the
/// "air" bank, tilting and animating the surface. Measured on an 8×8×8 pool:
/// 64 quads (all horizontal) with occluding walls, **384 quads, 284 of them
/// vertical** with non-occluding ones.
fn face_occlusion(sprites: &[SpriteRect], quads: &[BakedQuad]) -> [bool; 6] {
    let mut opaque_face = [false; 6];
    let mut interior_drawn = [false; 6];
    for quad in quads {
        let facing = face_of_direction(quad.direction);
        if let Some(cull) = quad.cullface {
            // A quad lining the inside of the `cull` shell: it faces inward, so
            // that shell is not a light-tight boundary.
            if face_of_direction(cull) == facing.opposite() {
                interior_drawn[facing.opposite().index()] = true;
            }
        }
        if !quad_is_full_face(quad) {
            continue;
        }
        if sprite_for_uv(sprites, uv_centroid(quad)).is_some_and(|sr| sr.layer == RenderLayer::Solid)
        {
            opaque_face[facing.index()] = true;
        }
    }
    std::array::from_fn(|i| opaque_face[i] && !interior_drawn[i])
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
        uv[0] >= s.uv_min[0] && uv[0] <= s.uv_max[0] && uv[1] >= s.uv_min[1] && uv[1] <= s.uv_max[1]
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
        let stairs = classify_fluid("oak_stairs", &props(&[("waterlogged", "true")]))
            .expect("a waterlogged block carries water");
        assert_eq!(stairs.kind, FluidKind::Water);
        assert_eq!(stairs.state, FluidState::source());

        // A non-waterlogged, non-fluid block exposes no fluid.
        assert!(classify_fluid("stone", &props(&[])).is_none());
        assert!(classify_fluid("oak_stairs", &props(&[("waterlogged", "false")])).is_none());
    }

    #[test]
    fn underwater_plants_carry_water_without_a_waterlogged_property() {
        // Kelp, seagrass and bubble columns have **no** `waterlogged` property:
        // vanilla hardcodes `getFluidState -> Fluids.WATER.getSource(false)` in
        // `KelpBlock`/`KelpPlantBlock`/`SeagrassBlock`/`TallSeagrassBlock`/
        // `BubbleColumnBlock`. Classifying them off `waterlogged` alone leaves an
        // air pocket around every plant in the ocean.
        for (path, p) in [
            ("kelp", vec![("age", "4")]),
            ("kelp_plant", vec![]),
            ("seagrass", vec![]),
            ("tall_seagrass", vec![("half", "lower")]),
            ("tall_seagrass", vec![("half", "upper")]),
            ("bubble_column", vec![("drag", "true")]),
        ] {
            let cell = classify_fluid(path, &props(&p))
                .unwrap_or_else(|| panic!("{path} must expose a water source"));
            assert_eq!(cell.kind, FluidKind::Water, "{path}");
            assert_eq!(cell.state, FluidState::source(), "{path}");
        }

        // The control: a land plant with the same shape (no `waterlogged`, a
        // cross model, an `age`/`half` property) must stay dry, so the rule
        // cannot pass by classifying every plant as water.
        assert!(classify_fluid("wheat", &props(&[("age", "4")])).is_none());
        assert!(classify_fluid("tall_grass", &props(&[("half", "lower")])).is_none());
        assert!(classify_fluid("sugar_cane", &props(&[("age", "4")])).is_none());
    }
}
