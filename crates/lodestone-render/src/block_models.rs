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
//! …and **per item, one geometry per model its definition tree can name**, not
//! one per item. That is [`ItemVariants`], and it is the axis the whole item path
//! lacked: `items/<id>.json` is a selector tree, this module used to resolve it
//! once at load against a static GUI context, and so every state- and
//! context-dependent item was flattened to its inventory form. 84 of 26.2's items
//! bake more than one model. Read [`ItemVariants`] before touching this file's
//! item half; `docs/item-variants.md` is the long form.
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
use lodestone_assets::item_tint::{self, ItemTintContext};
use lodestone_assets::tint::{Colormap, TintKind, vanilla_particle_tint_kind, vanilla_tint_kind};
use lodestone_assets::{
    AnimTable, Atlas, AtlasBuilder, AtlasError, AtlasSprite, BakeOptions, BakedQuad, BlockBaker,
    BlockStates, Direction, DisplayTransform, DisplayTransforms, Element, Face, FirstWeight,
    GuiItemContext, GuiLight,
    IconPart, ItemIconBuilder, ItemModel, ItemModelOutput, ItemPropertyContext, ModelResolver,
    ModelTransform, ResolvedModel, ResourceLocation,
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

/// Reserved palette slots for the four **biome-dependent** [`TintKind`]s —
/// [`TintKind::Grass`]/[`Foliage`](TintKind::Foliage)/
/// [`DryFoliage`](TintKind::DryFoliage)/[`Water`](TintKind::Water) — as
/// opposed to [`TintKind::Constant`]/[`TintKind::RedstonePower`], which do not
/// vary by position and keep going through [`TintPalette::intern`] as before.
///
/// # Why these need a *fixed* slot rather than an interned one
///
/// The palette is **one uniform buffer shared by every section drawn this
/// frame** (group 2, bound once — see `model_pipeline.rs`), so it can only
/// ever hold one colour per slot at a time. That is fine for a constant
/// colour, but grass in a desert and grass in a swamp are the same
/// [`TintKind`] with two different *real* colours, and no single slot in a
/// frame-shared buffer can hold both. So biome-dependent quads no longer
/// carry their final colour in the palette at all: [`crate::models::
/// ModelSectionView::biome_tint_at`] resolves the *real*, position-blended
/// colour at mesh time and writes it straight into the vertex (see
/// [`crate::models::ModelVertex::tint_rgb_override`]). These four slots exist
/// only so [`emit_baked_quad`](crate::models) can tell a biome-dependent quad
/// apart from a constant one by its `tint_index` alone, and so that a view
/// with **no** live biome data (GUI items, headless tests, a section whose
/// neighbourhood biome isn't wired) still renders the exact plains-default
/// colour these slots are pre-filled with in [`TintPalette::new`] — the
/// fallback path is not a special case, it is simply "no vertex override, so
/// the shader reads the slot like any other."
/// Reserved slot for [`TintKind::Water`].
pub const WATER_TINT_SLOT: u8 = UNTINTED - 1;
/// Reserved slot for [`TintKind::DryFoliage`].
pub const DRY_FOLIAGE_TINT_SLOT: u8 = UNTINTED - 2;
/// Reserved slot for [`TintKind::Foliage`].
pub const FOLIAGE_TINT_SLOT: u8 = UNTINTED - 3;
/// Reserved slot for [`TintKind::Grass`].
pub const GRASS_TINT_SLOT: u8 = UNTINTED - 4;

/// The first slot reserved for [`WATER_TINT_SLOT`]/[`DRY_FOLIAGE_TINT_SLOT`]/
/// [`FOLIAGE_TINT_SLOT`]/[`GRASS_TINT_SLOT`] — [`TintPalette::intern`] must
/// never hand out a slot at or past this index, or a popular constant colour
/// could collide with one of the four biome-dependent slots above.
const RESERVED_SLOTS_START: u8 = GRASS_TINT_SLOT;

/// The reserved slot for `kind`, or `None` for a kind that still goes through
/// [`TintPalette::intern`] ([`TintKind::Constant`]/[`TintKind::RedstonePower`]/
/// [`TintKind::None`]).
pub fn biome_tint_slot(kind: TintKind) -> Option<u8> {
    match kind {
        TintKind::Grass => Some(GRASS_TINT_SLOT),
        TintKind::Foliage => Some(FOLIAGE_TINT_SLOT),
        TintKind::DryFoliage => Some(DRY_FOLIAGE_TINT_SLOT),
        TintKind::Water => Some(WATER_TINT_SLOT),
        TintKind::None | TintKind::Constant(_) | TintKind::RedstonePower(_) => None,
    }
}

/// The inverse of [`biome_tint_slot`]: which biome-dependent [`TintKind`] a
/// vertex's `tint`/`quad.tint_index` byte names, or `None` for anything else
/// (untinted, or a position-independent constant/redstone colour). This is
/// what a live [`crate::models::ModelSectionView::biome_tint_at`]/
/// [`crate::models::FluidSectionView::water_tint_at`] implementor needs: the
/// mesher hands back the raw slot byte, and the view must know *which*
/// colormap/water lookup to run for it.
#[must_use]
pub fn biome_tint_kind_for_slot(slot: u8) -> Option<TintKind> {
    match slot {
        GRASS_TINT_SLOT => Some(TintKind::Grass),
        FOLIAGE_TINT_SLOT => Some(TintKind::Foliage),
        DRY_FOLIAGE_TINT_SLOT => Some(TintKind::DryFoliage),
        WATER_TINT_SLOT => Some(TintKind::Water),
        _ => None,
    }
}

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
    /// reuse their slot. Saturates just below [`RESERVED_SLOTS_START`] so
    /// neither the reserved white sentinel ([`UNTINTED`]) nor the four
    /// biome-dependent slots ([`biome_tint_slot`]) are ever overwritten by an
    /// unrelated constant colour (well under 50 distinct default tint colours
    /// exist in vanilla, so this is unreachable for any real pack).
    pub(crate) fn intern(&mut self, rgb: u32) -> u8 {
        if let Some(&idx) = self.lookup.get(&rgb) {
            return idx;
        }
        let idx = self.next.min(RESERVED_SLOTS_START - 1);
        self.colors[idx as usize] = rgb_to_rgba(rgb);
        self.lookup.insert(rgb, idx);
        self.next = self.next.saturating_add(1);
        idx
    }

    /// Force-set a **reserved** slot ([`biome_tint_slot`]'s return value) to a
    /// colour, bypassing `lookup`/`next` entirely. Used only for the four
    /// biome-dependent kinds, whose slot is fixed rather than interned — see
    /// [`biome_tint_slot`]'s doc for why a shared, frame-wide palette cannot
    /// hold their *real* per-position colour and what this fallback slot is
    /// for instead.
    pub(crate) fn reserve(&mut self, slot: u8, rgb: u32) {
        self.colors[slot as usize] = rgb_to_rgba(rgb);
    }

    /// The `PALETTE_LEN` palette entries, as the model shader's uniform expects.
    pub(crate) fn colors(&self) -> &[[f32; 4]] {
        &self.colors
    }
}

/// The palette index a tinted quad of `kind` should carry, given its resolved
/// **default (plains)** colour `rgb`: the fixed [`biome_tint_slot`] for the
/// four biome-dependent kinds (pre-filled with `rgb` as the no-live-data
/// fallback), or an ordinary [`TintPalette::intern`]ed slot for everything
/// else (`Constant`/`RedstonePower`, which never need a fallback because they
/// never have a *different* live value to fall back from).
fn palette_slot_for(kind: TintKind, rgb: u32, palette: &mut TintPalette) -> u8 {
    match biome_tint_slot(kind) {
        Some(slot) => {
            palette.reserve(slot, rgb);
            slot
        }
        None => palette.intern(rgb),
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
    /// The `block/water_overlay` sprite, for side faces against a
    /// `HalfTransparentBlock`/`LeavesBlock` neighbour. `None` for lava, which
    /// has no overlay material in vanilla (`FluidStateModelSet.LAVA_MODEL`
    /// passes `null`).
    pub overlay: Option<SpriteUv>,
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

/// Blocks whose class extends vanilla `HalfTransparentBlock` in 26.2 (glass,
/// stained glass, tinted glass, ice, blue ice, frosted ice, honey, slime), plus
/// every `LeavesBlock`. `FluidRenderer.tesselate` checks
/// `relativeBlock instanceof HalfTransparentBlock || relativeBlock instanceof
/// LeavesBlock` to decide whether a fluid side face touching this neighbour
/// uses the `water_overlay` material instead of `*_flow`, and to suppress the
/// side face's back copy (`addBackFace = !isOverlay`).
///
/// Neither the render layer nor the baked geometry can stand in for this: a
/// `slime_block`/`honey_block` sprite is fully opaque (they'd land on the
/// `Solid` layer, indistinguishable from any ordinary block by alpha), and
/// `LeavesBlock` renders `Cutout`, not `Translucent`, so no alpha-derived rule
/// separates "is this class" from "is this some other cutout/translucent
/// block". This is the same situation `UNCONDITIONAL_WATER_BLOCKS` already
/// documents — the fact lives in a class hierarchy Java expresses and no data
/// report carries — so it is a name list scanned from the decompiled 26.2
/// `Blocks.java` (`TransparentBlock::new`, `StainedGlassBlock::new` × 16
/// `DyeColor`s, `HalfTransparentBlock::new`, `IceBlock::new`,
/// `FrostedIceBlock::new`, `HoneyBlock::new`, `SlimeBlock::new`,
/// `TintedGlassBlock::new`), not guessed.
///
/// Deliberately **excludes** `copper_grate` and its weathering/waxed variants,
/// which also construct a `HalfTransparentBlock` subclass
/// (`WaterloggedTransparentBlock`) — a niche waterlogged block where getting
/// the overlay wrong is low-stakes, scoped out to keep this list to blocks
/// actually named in `docs/fluid-rendering.md`'s gap report plus their obvious
/// siblings (every glass colour, both ice variants).
const FLUID_OVERLAY_HALF_TRANSPARENT_BLOCKS: &[&str] = &[
    "glass",
    "tinted_glass",
    "ice",
    "blue_ice",
    "frosted_ice",
    "honey_block",
    "slime_block",
    "white_stained_glass",
    "orange_stained_glass",
    "magenta_stained_glass",
    "light_blue_stained_glass",
    "yellow_stained_glass",
    "lime_stained_glass",
    "pink_stained_glass",
    "gray_stained_glass",
    "light_gray_stained_glass",
    "cyan_stained_glass",
    "purple_stained_glass",
    "blue_stained_glass",
    "brown_stained_glass",
    "green_stained_glass",
    "red_stained_glass",
    "black_stained_glass",
];

/// Every `LeavesBlock` in 26.2, scanned the same way — `Blocks.java`'s eleven
/// `register(..., p -> new {Tinted,Untinted}ParticleLeavesBlock(...), ...)` /
/// `MangroveLeavesBlock` calls.
const FLUID_OVERLAY_LEAVES_BLOCKS: &[&str] = &[
    "oak_leaves",
    "spruce_leaves",
    "birch_leaves",
    "jungle_leaves",
    "acacia_leaves",
    "cherry_leaves",
    "dark_oak_leaves",
    "pale_oak_leaves",
    "mangrove_leaves",
    "azalea_leaves",
    "flowering_azalea_leaves",
];

/// Whether a fluid touching this block (by `block_path`) should use the
/// `water_overlay` material instead of `*_flow`. See
/// [`FLUID_OVERLAY_HALF_TRANSPARENT_BLOCKS`].
#[must_use]
fn is_fluid_overlay_neighbor(block_path: &str) -> bool {
    FLUID_OVERLAY_HALF_TRANSPARENT_BLOCKS.contains(&block_path)
        || FLUID_OVERLAY_LEAVES_BLOCKS.contains(&block_path)
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
    /// Whether the mesher should compute per-corner smooth ambient occlusion
    /// for this state's quads, or fall back to flat per-face light — vanilla's
    /// `ModelBlockRenderer.tesselateBlock` choosing `tesselateAmbientOcclusion`
    /// vs `tesselateFlat`. Sourced from
    /// [`BakedModel::ambient_occlusion`](lodestone_assets::bake::BakedModel::ambient_occlusion),
    /// which carries only the model-JSON half of vanilla's gate
    /// (`this.parts.getFirst().useAmbientOcclusion()`); the
    /// `blockState.getLightEmission() == 0` half has no data source in this
    /// crate yet and is not applied — see that field's doc for what is missing.
    pub ambient_occlusion: bool,
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
            ambient_occlusion: true,
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

/// **Every** baked form one item can take, plus the definition tree that chooses
/// between them — the variant axis.
///
/// # The defect this exists to fix
///
/// An item's `items/<id>.json` is a selector tree, and `BlockModels` used to
/// resolve it **once, at asset-load time, against a static context** that answered
/// every `condition` false, every `range` `0.0`, every `select` `None` except
/// `minecraft:display_context -> "gui"`. So one geometry was baked per item id and
/// there was no axis to vary it along. Measured over the real jar by
/// `tests/item_variant_gate.rs`, **84 items bake more than one model** (2,012
/// variants across 1,474 items) and every one of them was flattened:
///
/// * `display_context` (26 items) — a spyglass in the hand drew the flat
///   `item/spyglass` sprite instead of `item/spyglass_in_hand`'s 3-D tube, and
///   took `item/generated`'s `firstperson_righthand` pose rather than the in-hand
///   model's, because [`ItemIcon::display`](lodestone_assets::ItemIcon::display)
///   is the *first drawable part's* map.
/// * `using_item` + `use_duration` — a drawn bow stayed slack.
///
/// # How a frame picks one
///
/// [`Self::resolve`] runs the *real* resolver against a live
/// [`ItemPropertyContext`] — [`ItemStateContext`](crate::ItemStateContext) is the
/// one this crate supplies — and looks the chosen model ref up in the pre-baked
/// map. Resolution is pure and allocation-light (a `Vec` of at most a few
/// outputs); the *baking* is what had to happen up front, because a flat variant's
/// geometry is read out of the stitched atlas (see `ItemSpritePart`).
///
/// # How to change it
///
/// Adding a sourceable property means teaching
/// [`ItemStateContext`](crate::ItemStateContext) to answer it — nothing here
/// changes, because every variant is already baked. Adding a *node type* means
/// `lodestone_assets::item_model`. The one thing that would need work here is a
/// variant whose geometry is **not** a model ref (a `special` renderer), which is
/// deliberately absent: those are block-entity draws, not baked quads.
#[derive(Debug, Clone)]
pub struct ItemVariants {
    /// The parsed `items/<id>.json` selector tree, kept so a frame can re-resolve
    /// it. Parsed once at load — resolution needs no I/O.
    definition: ItemModel,
    /// Baked geometry per resolved model ref. Contains every model the tree can
    /// reach, including the GUI one.
    by_model: HashMap<ResourceLocation, ItemGeometry>,
    /// The ref an inventory slot resolves to, and the fallback for any context
    /// whose own resolution reaches nothing bakeable. `None` when the GUI form is
    /// a code-driven `special` renderer.
    gui: Option<ResourceLocation>,
    /// Every `minecraft:special` form this item's tree can reach, keyed by the
    /// **`base` model ref** the special node names.
    ///
    /// Kept beside [`Self::by_model`] rather than inside it because these forms have
    /// no geometry to bake at all: their triangles come from a block-entity rig, so
    /// what a caller needs is the `kind` plus the `base` model's `display` map. It is
    /// keyed by `base` and not by `kind` so a `select` that reaches two different
    /// special nodes (a trident in hand versus thrown, a shield blocking versus not)
    /// stays two entries — the same reason [`Self::by_model`] is not keyed by item.
    specials: HashMap<ResourceLocation, SpecialItemForm>,
}

/// One `minecraft:special` form of one item: which code-driven renderer draws it,
/// and where the `base` model says to put it in each of vanilla's nine display
/// contexts.
///
/// # Why there is no geometry here
///
/// A special item has none. `base` is a real item model, but **every one of the ten
/// special `base` models in 26.2 has no `elements` and no `layer0`** — only a
/// `particle` texture, which is a *block* texture and is not in the item atlas. So
/// the "fall back to the base sprite" reading of `IconPart::Special` draws exactly
/// zero pixels, and the geometry has to come from the block-entity rig
/// [`crate::special_item_rig`] names.
///
/// What `base` really carries, and the only reason it is resolved at all, is the
/// `display` map: a chest's `gui` pose is `[30, 45, 0]` at scale `0.625`, authored
/// on `item/template_chest`, and its `firstperson_righthand` pose likewise.
#[derive(Debug, Clone, PartialEq)]
pub struct SpecialItemForm {
    /// The special-renderer id — `minecraft:chest`, `minecraft:shulker_box`. Feed
    /// it to [`crate::special_item_rig`] with the item's own path.
    pub kind: String,
    /// The `base` model's own `display` map, all nine slots.
    pub display: DisplayTransforms,
}

impl ItemVariants {
    /// The parsed definition tree, for a caller that wants to enumerate or
    /// inspect the variants rather than resolve one.
    #[must_use]
    pub fn definition(&self) -> &ItemModel {
        &self.definition
    }

    /// The inventory-slot form: the geometry [`BlockModels::item`] returns.
    #[must_use]
    pub fn gui(&self) -> Option<&ItemGeometry> {
        self.by_model.get(self.gui.as_ref()?)
    }

    /// The geometry baked for one specific model ref (`minecraft:item/bow_pulling_2`),
    /// bypassing resolution. For gates that assert *which* variant a context picks.
    #[must_use]
    pub fn variant(&self, model: &ResourceLocation) -> Option<&ItemGeometry> {
        self.by_model.get(model)
    }

    /// Every baked variant as `(model_ref, geometry)`. Order is the backing
    /// `HashMap`'s — sort by ref if you need determinism.
    pub fn variants(&self) -> impl Iterator<Item = (&ResourceLocation, &ItemGeometry)> {
        self.by_model.iter()
    }

    /// How many distinct models this item bakes. `1` for the ~1,457 items with no
    /// branch node.
    #[must_use]
    pub fn variant_count(&self) -> usize {
        self.by_model.len()
    }

    /// Resolve the definition tree against live state and return that variant's
    /// geometry, falling back to the inventory form.
    ///
    /// The fallback is load-bearing rather than defensive: a context can legally
    /// resolve to a `special` node (a trident in the hand does) or to a model that
    /// baked nothing, and drawing the inventory form is what vanilla's own
    /// `MissingItemModel` degradation amounts to — strictly better than the item
    /// vanishing from the hand.
    ///
    /// A `composite` resolution yields several outputs; the **first** bakeable one
    /// wins, matching [`gui`](Self::gui) and for the same unparsed-per-part-
    /// transformation reason.
    #[must_use]
    pub fn resolve(&self, ctx: &impl ItemPropertyContext) -> Option<&ItemGeometry> {
        self.definition
            .resolve(ctx)
            .into_iter()
            .find_map(|output| match output {
                ItemModelOutput::Model { model, .. } => self.by_model.get(model),
                ItemModelOutput::Special { .. } => None,
            })
            .or_else(|| self.gui())
    }

    /// The **special-renderer** form `ctx` resolves to, if any — the answer
    /// [`Self::resolve`] structurally cannot give.
    ///
    /// A caller that draws item geometry must ask *both*: `resolve` for baked quads
    /// and this for a block-entity rig, in that order. Getting only the first is the
    /// bug this method exists to fix — a held chest resolved to a `Special` output,
    /// `resolve` returned `None` from that arm, fell through to `gui()`, and `gui`
    /// is `None` too for a special item, so the hand drew nothing at all while the
    /// inventory slot drew a real chest.
    ///
    /// **No `gui()`-style fallback here, deliberately.** `resolve`'s fallback is
    /// right for its own question (a context that reaches nothing bakeable should
    /// draw the inventory form rather than vanish), but a *special* fallback would
    /// mean drawing a chest rig for a context that resolved to something else
    /// entirely. `None` means "this context is not a special form", and a caller
    /// must be able to tell that from "it is one".
    #[must_use]
    pub fn resolve_special(&self, ctx: &impl ItemPropertyContext) -> Option<&SpecialItemForm> {
        self.definition
            .resolve(ctx)
            .into_iter()
            .find_map(|output| match output {
                ItemModelOutput::Special { base, .. } => self.specials.get(base),
                ItemModelOutput::Model { .. } => None,
            })
    }

    /// Every special form this item's tree can reach, as `(base ref, form)` — the
    /// enumeration half of [`Self::resolve_special`], for a gate that wants to
    /// assert the *set* rather than one context's choice.
    pub fn special_forms(&self) -> impl Iterator<Item = (&ResourceLocation, &SpecialItemForm)> {
        self.specials.iter()
    }

    /// Which model ref `ctx` resolves to, whether or not it baked — the diagnostic
    /// half of [`Self::resolve`], so a gate can assert the *choice* rather than
    /// inferring it from geometry that two variants might share.
    #[must_use]
    pub fn resolve_ref(&self, ctx: &impl ItemPropertyContext) -> Option<ResourceLocation> {
        self.definition
            .resolve(ctx)
            .into_iter()
            .find_map(|output| match output {
                ItemModelOutput::Model { model, .. } => Some(model.clone()),
                ItemModelOutput::Special { .. } => None,
            })
    }
}

/// One **variant** of one item whose model is 3-D geometry, discovered *before*
/// the atlas is stitched so the textures it reaches can be seeded into it.
///
/// `(item, model)` is the key, not `item` alone. An item's definition tree can
/// name several models — `bow` names four, `spyglass` two — and which one a frame
/// draws depends on the display context and on live stack state, neither of which
/// is known at asset-load time. So every reachable model is baked and the choice
/// is made per frame; see [`ItemVariants`].
#[derive(Debug, Clone)]
struct ItemModelPart {
    item: ResourceLocation,
    model: ResourceLocation,
    transform: DisplayTransform,
    display: DisplayTransforms,
    gui_light: GuiLight,
}

/// One variant of one item whose model is a flat `builtin/generated` layer stack,
/// which [`extruded_sprite_geometry`] turns into vanilla's thin extruded slab.
///
/// Discovered in the same pass as [`ItemModelPart`], and for the same reason —
/// the layer sprites live under `textures/item/`, which no *blockstate* reaches,
/// so they have to be seeded into the atlas before it is stitched.
///
/// **This is why variant discovery cannot be deferred to draw time.**
/// `item/bow_pulling_0`, `_1` and `_2` are `item/generated` sprite models whose
/// only difference from `item/bow` is a swapped `layer0`, so their geometry comes
/// out of the alpha outline of a texture that has to be *in the atlas already*. A
/// per-frame "resolve then bake" would find no sprite and draw nothing.
#[derive(Debug, Clone)]
struct ItemSpritePart {
    item: ResourceLocation,
    model: ResourceLocation,
    layers: Vec<SpriteLayer>,
    display: DisplayTransforms,
}

/// Everything the item half of [`BlockModels::build`] needs, discovered in one
/// pre-stitch pass: the parse of each definition, and every bakeable variant it
/// reaches, split by geometry kind.
struct ItemVariantParts {
    /// Per item: its parsed definition and which model its GUI slot resolves to.
    plans: Vec<ItemVariantPlan>,
    /// Every `(item, model)` variant whose model is 3-D geometry.
    models: Vec<ItemModelPart>,
    /// Every `(item, model)` variant whose model is a flat sprite stack.
    sprites: Vec<ItemSpritePart>,
    /// Every `(item, base ref, form)` a `minecraft:special` node reaches — no
    /// geometry, only the `kind` and the `base` model's `display` map. See
    /// [`SpecialItemForm`].
    specials: Vec<(ResourceLocation, ResourceLocation, SpecialItemForm)>,
    /// Notes for [`BlockModels::item_bake_misses`].
    notes: Vec<String>,
}

/// One item's definition tree plus the variant its inventory slot picks.
struct ItemVariantPlan {
    item: ResourceLocation,
    definition: ItemModel,
    /// The model ref [`GuiItemContext`] resolves to — `None` when the GUI form is
    /// a code-driven `special` renderer (chests, shields, banners) or renders
    /// nothing at all.
    gui: Option<ResourceLocation>,
}

/// Parse every item definition in the pack stack and enumerate **every** model it
/// can resolve to, classified into 3-D and flat-sprite variants.
///
/// # Why every variant and not just the GUI one
///
/// This pass used to resolve each definition exactly once, under
/// [`GuiItemContext`], and keep that single form. That is right for an inventory
/// slot and wrong everywhere else: `minecraft:display_context` is a `select`
/// property, so 26 of 26.2's items (`spyglass`, `trident`, the spears, every
/// bundle) name a *different* model in the hand than in the slot, and
/// `minecraft:using_item` / `minecraft:use_duration` make a drawn bow a different
/// model again. Baking one form flattened all of them to the inventory sprite.
///
/// [`ItemModel::outputs`] is the union over every branch, so the loop below is
/// context-free by construction: it cannot miss a variant a context might later
/// ask for, which is the property the atlas needs (see [`ItemSpritePart`]).
/// Duplicate refs are collapsed — `select`/`range_dispatch` reuse models freely,
/// and vanilla's `crossbow` names `item/crossbow` twice.
///
/// # The GUI form still gets singled out
///
/// [`ItemVariantPlan::gui`] preserves the old behaviour exactly: the first
/// [`IconPart::Model`] the GUI resolution produces, else its first
/// [`IconPart::Sprite`]. That "model before sprite" preference is not tree order —
/// it is what the single-geometry-per-item code did, and keeping it means this
/// change cannot silently move which part of a mixed `composite` an inventory
/// slot draws.
///
/// A `composite` GUI icon can hold several model parts; only the first is the GUI
/// form, and the item is named in [`BlockModels::item_bake_misses`]. In vanilla
/// 26.2 that is the 16 beds and nothing else: `items/<colour>_bed.json`
/// composites `block/<colour>_bed_head` with `block/<colour>_bed_foot` plus a
/// per-part `transformation` (`translation [0, 0, 1]`) that positions the foot
/// behind the head. `lodestone_assets`'s [`IconPart::Model`] does not carry that
/// transformation — `item_model.rs` never parses it — so concatenating the parts
/// would stack the foot *inside* the head and z-fight, which is strictly worse
/// than drawing the head alone. (Both parts are still *baked*, under their own
/// refs; nothing resolves to the foot, so nothing draws it.)
fn collect_item_variants(manager: &ResourceManager) -> ItemVariantParts {
    let builder = ItemIconBuilder::new(manager);
    let mut out = ItemVariantParts {
        plans: Vec::new(),
        models: Vec::new(),
        sprites: Vec::new(),
        specials: Vec::new(),
        notes: Vec::new(),
    };
    for id in item_ids(manager) {
        let Ok(definition) = builder.definition(&id) else {
            continue;
        };
        // Every model the tree can reach, deduplicated.
        //
        // **`Special` outputs are collected, not skipped**, and the note that used
        // to be here explaining why they were skipped was wrong in its second half:
        // it said "their `base` sprite reaches this path as the GUI form of the same
        // item anyway". It does not. Every special `base` model in 26.2 has no
        // `elements` and no `layer0`, so `part_for_model` classifies it as
        // undrawable and nothing at all reached `by_model` — which is why a held
        // chest drew zero pixels while the inventory slot drew a real one. What the
        // `base` does carry is the `display` map, and that is what `specials` keeps;
        // the geometry comes from the block-entity rig `special_item_rig` names.
        let mut seen = BTreeSet::new();
        // Deduplicated per **item**, not globally: `template_skull` is the `base` of
        // all six `minecraft:head` items, and a global set would give five of them no
        // display map at all.
        let mut seen_specials = BTreeSet::new();
        for output in definition.outputs() {
            if let ItemModelOutput::Special { base, kind } = output {
                // `part_for` (not `part_for_model`) is the entry point that resolves
                // a special node's `base` for its `display` map.
                if seen_specials.insert(base.clone())
                    && let Ok((_, Some(display))) = builder.part_for(output)
                {
                    out.specials.push((
                        id.clone(),
                        base.clone(),
                        SpecialItemForm {
                            kind: kind.to_string(),
                            display,
                        },
                    ));
                }
                continue;
            }
            let ItemModelOutput::Model { model, tints } = output else {
                continue;
            };
            if !seen.insert(model.clone()) {
                continue;
            }
            let part = match builder.part_for_model(model, tints) {
                Ok((part, display)) => part.map(|p| (p, display.unwrap_or(DisplayTransforms::NONE))),
                Err(e) => {
                    out.notes.push(format!("{id} ({model}): {e}"));
                    continue;
                }
            };
            // The `display` map is **this model's**, not the icon's — the fix for
            // the held-item transform. `item/spyglass_in_hand` authors no
            // `firstperson_righthand` at all and `item/bow_pulling_1` inherits
            // `item/bow`'s, and resolving the icon in the GUI context reported
            // `item/generated`'s for both.
            match part {
                Some((
                    IconPart::Model {
                        model: geometry,
                        transform,
                        gui_light,
                    },
                    display,
                )) => out.models.push(ItemModelPart {
                    item: id.clone(),
                    model: geometry,
                    transform,
                    display,
                    gui_light,
                }),
                // Every layer is kept — vanilla's `ItemModelGenerator.bake` walks
                // `layer0..layer4` and concatenates each layer's extrusion into
                // one quad collection, so a two-layer item (a dyed leather boot,
                // an enchanted book glint base) is two stacked slabs, not one.
                Some((IconPart::Sprite { layers }, display)) => {
                    out.sprites.push(ItemSpritePart {
                        item: id.clone(),
                        model: model.clone(),
                        layers,
                        display,
                    });
                }
                Some((IconPart::Special { .. }, _)) | None => {}
            }
        }
        let gui = gui_variant_of(&builder, &definition, &id, &mut out.notes);
        out.plans.push(ItemVariantPlan {
            item: id,
            definition,
            gui,
        });
    }
    out
}

/// Which model an inventory slot draws for `definition` — the GUI resolution's
/// first [`IconPart::Model`], else its first [`IconPart::Sprite`]. See
/// [`collect_item_variants`] for why the preference is that way round and not
/// tree order.
fn gui_variant_of(
    builder: &ItemIconBuilder<'_>,
    definition: &ItemModel,
    id: &ResourceLocation,
    notes: &mut Vec<String>,
) -> Option<ResourceLocation> {
    let mut first_model = None;
    let mut first_sprite = None;
    let mut model_parts = 0usize;
    for output in definition.resolve(&GuiItemContext) {
        let ItemModelOutput::Model { model, tints } = output else {
            continue;
        };
        let Ok((Some(part), _)) = builder.part_for_model(model, tints) else {
            continue;
        };
        match part {
            IconPart::Model { .. } => {
                model_parts += 1;
                first_model = first_model.or_else(|| Some(model.clone()));
            }
            IconPart::Sprite { .. } => first_sprite = first_sprite.or_else(|| Some(model.clone())),
            IconPart::Special { .. } => {}
        }
    }
    if model_parts > 1 {
        notes.push(format!(
            "{id}: composite icon has {model_parts} model parts, but IconPart::Model carries no \
             per-part transformation; only the first is drawn"
        ));
    }
    first_model.or(first_sprite)
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
/// `layer_slots[i]` is the palette slot layer `i`'s quads carry, or `None` to
/// leave that layer untinted — parallel to `layers`, and produced by
/// [`item_layer_tint_slots`] from the item definition's own `tints` list.
///
/// Vanilla stamps `tintIndex = layerIndex` on these quads and resolves the
/// colour from the item's `TintSource` list at draw time
/// (`CuboidItemModelWrapper.java:85-92`). We cannot write the layer index into
/// `BakedQuad::tint_index`, because that field indexes
/// `BlockModels::tint_palette` and layer `0` would collide with whatever
/// constant happens to be interned at slot 0. So the resolution happens *here*,
/// at bake time, and what lands in `tint_index` is the interned slot of the
/// already-resolved colour. That is sound because every tint this build can
/// resolve is a **constant per item** — a `default`, a fixed climate sample, or
/// a dye read from a stack we do not have at bake time — and a constant is
/// exactly what a frame-shared palette can hold. Per-*stack* variation (a dyed
/// leather helmet, a custom-colour potion) would need
/// [`ModelVertex::tint_rgb_override`](crate::models::ModelVertex::tint_rgb_override)
/// instead; see this module's item loop for why that is not wired yet.
///
/// This function used to emit every quad untinted, documenting it as a
/// deliberate narrowing. It was narrower than it read: `lily_pad`, `potion`,
/// `splash_potion`, `lingering_potion`, `tipped_arrow`, `filled_map`,
/// `firework_star` and the six leather items all have a non-identity item tint
/// and all of them rendered white.
fn extruded_sprite_geometry(
    atlas: &Atlas,
    layers: &[SpriteLayer],
    layer_slots: &[Option<u8>],
) -> Option<Vec<BakedQuad>> {
    let mut quads = Vec::new();
    for (index, layer) in layers.iter().enumerate() {
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
        let mut baked = bake_model_with(
            &model,
            atlas,
            ModelTransform::default(),
            &BakeOptions::default(),
        )
        .ok()?;
        // The synthesised model's faces carry no `tintindex`, so every quad
        // arrives `None`; stamping the resolved slot here is an assignment, not
        // a rewrite, and it applies to this layer's quads only.
        if let Some(slot) = layer_slots.get(index).copied().flatten() {
            for quad in &mut baked {
                quad.tint_index = Some(i32::from(slot));
            }
        }
        quads.extend(baked);
    }
    (!quads.is_empty()).then_some(quads)
}

/// Resolve an item definition's per-layer `tints` to palette slots, parallel to
/// `layers`.
///
/// The join that makes item tints work at all: `layers[i].tint` is the
/// `TintSource` the item definition put on layer `i`
/// (`CuboidItemModelWrapper.java:132` parses the list, `:89` evaluates it
/// per-layer), and this resolves each one through
/// [`lodestone_assets::item_tint::resolve`] and interns the result.
///
/// # Why `intern` and not `palette_slot_for`
///
/// [`palette_slot_for`] routes the four biome-dependent [`TintKind`]s to their
/// fixed reserved slots, which is right for a *block* quad whose colour varies
/// with the biome the player is standing in. An item's `minecraft:grass` tint is
/// not that: it is a **fixed** climate sample the definition names
/// (`{"temperature": 0.5, "downfall": 1.0}` in all six vanilla files), because an
/// item in your hotbar does not change colour when you walk into a swamp. So
/// these go through [`TintPalette::intern`] like any other constant, and a
/// grass-tinted *item* and a grass-tinted *block* correctly end up in different
/// slots holding the same plains green.
///
/// `misses` collects a note for any tint this build could not resolve, for
/// [`BlockModels::item_bake_misses`]. A source we simply do not know is worth
/// reporting; a *known* source with nothing to apply is not, and
/// [`is_known`](lodestone_assets::item_tint::is_known) is what separates them.
fn item_layer_tint_slots(
    item: &ResourceLocation,
    layers: &[SpriteLayer],
    grass_colormap: Option<&Colormap>,
    palette: &mut TintPalette,
    misses: &mut Vec<String>,
) -> Vec<Option<u8>> {
    let ctx = ItemTintContext {
        // No stack at bake time, so every component-reading source takes its
        // definition's `default` — which is vanilla's own behaviour for an
        // uncustomised stack, and therefore correct rather than approximate for
        // the overwhelming majority of what an inventory holds.
        components: None,
        grass_colormap,
    };
    layers
        .iter()
        .map(|layer| {
            let source = layer.tint.as_ref()?;
            match item_tint::resolve(source, &ctx) {
                Some(resolved) => Some(palette.intern(resolved.rgb())),
                None => {
                    if !item_tint::is_known(&source.kind) {
                        misses.push(format!(
                            "{item}: unknown item tint source \"{}\"",
                            source.kind
                        ));
                    }
                    None
                }
            }
        })
        .collect()
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
    /// Per-state: whether this block is a vanilla `HalfTransparentBlock` or
    /// `LeavesBlock` — the class `FluidRenderer.tesselate` checks on a fluid's
    /// horizontal neighbour to swap in the `water_overlay` material and drop
    /// the side face's back copy. Parallel to `models`. See
    /// [`is_fluid_overlay_neighbor`].
    fluid_overlay: Vec<bool>,
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
    /// Baked geometry for every item that has any, keyed by item id
    /// (`minecraft:stone`) — and, within each, by the **model ref** its definition
    /// tree resolved to. See the [module docs](self) for why it lives on a type
    /// called `BlockModels`, and [`ItemVariants`] for why one geometry per item is
    /// not enough.
    items: HashMap<ResourceLocation, ItemVariants>,
    /// Item models that did **not** bake, named. Recorded rather than fatal, for
    /// the same reason `ItemAtlasReport::missing_special_bases` is: a texture a
    /// vanilla blockstate never reaches (or that a resource pack drops) is an
    /// expected absence, not a reason to refuse to render the world.
    item_bake_misses: Vec<String>,
    /// The grass/foliage/dry-foliage colormaps, loaded once here so a mesher
    /// with live per-position biome data (a [`crate::models::ModelSectionView`]
    /// implementor) can resolve a **real** colour instead of the
    /// [`tint_palette`](Self::tint_palette)'s plains default — see
    /// [`Self::colormaps`]. `None` only when the pack has no colormap PNGs at
    /// all (tolerated, matching [`item_bake_misses`](Self::item_bake_misses)'s
    /// "an absence is not fatal" posture): the reserved palette slots still
    /// carry a usable fallback colour either way.
    colormaps: Option<lodestone_assets::tint::Colormaps>,
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
        // reused after it to bake, so the item definitions are parsed once.
        //
        // **Every variant**, not just the GUI form: a flat variant's geometry is
        // walked out of the alpha outline of a *stitched* sprite, so a variant
        // discovered after this point could never be baked at all. See
        // `collect_item_variants`.
        let ItemVariantParts {
            plans: item_plans,
            models: item_parts,
            sprites: sprite_parts,
            specials: item_specials,
            notes: mut item_bake_misses,
        } = collect_item_variants(manager);
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
        // Same three PNGs `tints` just sampled at plains, kept around whole so
        // a live mesher can sample them at a *real* biome's temperature/downfall
        // instead — see `colormaps` field doc.
        let colormaps = lodestone_assets::tint::Colormaps::load(manager).ok();
        let mut palette = TintPalette::new();
        let count = registry.state_count();
        let mut models = Vec::with_capacity(count as usize);
        let mut fluids = Vec::with_capacity(count as usize);
        let mut fluid_overlay = Vec::with_capacity(count as usize);
        for id in 0..count {
            let resolved = registry.resolve(id);
            let sm = match baker.bake_state(registry, id, &FirstWeight) {
                Ok(model) if !model.quads.is_empty() => {
                    let particle_uv = model.particle_uv;
                    let ambient_occlusion = model.ambient_occlusion;
                    let mut quads = model.quads;
                    // Rewrite each tinted quad's raw model tint index into a
                    // palette index for its resolved source colour. `None` (an
                    // untinted kind, e.g. a `tint_index` on a non-biome block)
                    // clears the tint so the quad renders its texture unchanged.
                    if let Some(r) = resolved.as_ref() {
                        for quad in &mut quads {
                            if let Some(raw) = quad.tint_index {
                                let kind = vanilla_tint_kind(r.block, raw, r.properties);
                                quad.tint_index = tints.color(kind).map(|rgb| {
                                    i32::from(palette_slot_for(kind, rgb, &mut palette))
                                });
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
                        ambient_occlusion,
                    }
                }
                _ => StateModel::empty(),
            };
            models.push(sm);

            let fluid = resolved.and_then(|r| classify_fluid(r.block.path(), r.properties));
            fluids.push(fluid);
            fluid_overlay.push(resolved.is_some_and(|r| is_fluid_overlay_neighbor(r.block.path())));
        }

        // Item geometry, baked against the same atlas and interning through the
        // same `palette` as the state loop above — the two facts that let the
        // GUI item pass reuse the terrain pipeline wholesale. It runs after the
        // states (rather than inside that loop) only because items are keyed by
        // id, not by state id; everything it shares is what matters.
        //
        // Keyed by `(item, model)`: one item can bake several variants, and the
        // per-item maps are assembled from these below.
        let mut baked: HashMap<(ResourceLocation, ResourceLocation), ItemGeometry> =
            HashMap::with_capacity(item_parts.len() + sprite_parts.len());
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
                        quad.tint_index = tints
                            .color(kind)
                            .map(|rgb| i32::from(palette_slot_for(kind, rgb, &mut palette)));
                    }
                }
            }
            baked.insert(
                (part.item.clone(), part.model.clone()),
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
            // The item definition's own `tints` list, resolved and interned
            // before the bake so each layer's quads can carry its slot.
            let layer_slots = item_layer_tint_slots(
                &part.item,
                &part.layers,
                tints.grass(),
                &mut palette,
                &mut item_bake_misses,
            );
            let Some(quads) = extruded_sprite_geometry(&atlas, &part.layers, &layer_slots) else {
                item_bake_misses.push(format!(
                    "{} ({}): sprite variant has {} layer(s), none of which stitched into the atlas",
                    part.item,
                    part.model,
                    part.layers.len()
                ));
                continue;
            };
            baked.insert(
                (part.item.clone(), part.model.clone()),
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

        // Regroup `(item, model) -> geometry` into `item -> ItemVariants`, pairing
        // each item's baked forms with the definition tree that chooses between
        // them.
        //
        // One draining pass and then a join, rather than a filter per item: the
        // obvious `baked.iter().filter(|((item, _), _)| ...)` inside the plan loop
        // is O(items x variants) *and* clones every geometry, so two copies of all
        // ~1,700 baked quad sets are live at once.
        //
        // An item with **neither** a bakeable variant nor a special form is absent.
        //
        // That condition used to be "no bakeable variant", full stop, and it is why
        // a held chest drew nothing: a chest's definition is one `special` node, so
        // it baked no geometry and was dropped here — `items.get("minecraft:chest")`
        // was `None`, and no amount of resolving downstream could recover it. An item
        // with only special forms now gets an entry with an **empty** `by_model`,
        // which leaves every geometry accessor answering exactly as before
        // (`gui()` is `None` because `plan.gui` is `None` for a special item, so
        // `BlockModels::item` still returns `None` and the GUI stream is untouched)
        // while `resolve_special` becomes reachable.
        let mut grouped: HashMap<ResourceLocation, HashMap<ResourceLocation, ItemGeometry>> =
            HashMap::with_capacity(item_plans.len());
        for ((item, model), geometry) in baked {
            grouped.entry(item).or_default().insert(model, geometry);
        }
        let mut special_forms: HashMap<ResourceLocation, HashMap<ResourceLocation, SpecialItemForm>> =
            HashMap::new();
        for (item, base, form) in item_specials {
            special_forms.entry(item).or_default().insert(base, form);
        }
        let mut items: HashMap<ResourceLocation, ItemVariants> =
            HashMap::with_capacity(grouped.len());
        for plan in item_plans {
            let by_model = grouped.remove(&plan.item).unwrap_or_default();
            let specials = special_forms.remove(&plan.item).unwrap_or_default();
            if by_model.is_empty() && specials.is_empty() {
                continue;
            }
            items.insert(
                plan.item,
                ItemVariants {
                    definition: plan.definition,
                    by_model,
                    gui: plan.gui,
                    specials,
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
            fluid_overlay,
            water_sprites,
            lava_sprites,
            tint_palette: palette.colors().to_vec(),
            animations,
            anim_frame_v,
            crack_stages,
            items,
            item_bake_misses,
            colormaps,
        })
    }

    /// The grass/foliage/dry-foliage colormaps, for a mesher that wants a
    /// biome's *real* colour (not the plains default baked into
    /// [`tint_palette`](Self::tint_palette)). `None` only when the pack's
    /// colormap PNGs failed to load at build time.
    #[must_use]
    pub fn colormaps(&self) -> Option<&lodestone_assets::tint::Colormaps> {
        self.colormaps.as_ref()
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
    ///
    /// **This is the *inventory* form specifically.** It is
    /// [`ItemVariants::gui`], and for the 84 items with several forms it is only
    /// one of several baked forms — a spyglass in the hand and a drawn bow are
    /// different geometry. Any caller that is not drawing an inventory slot wants
    /// [`item_forms`](Self::item_forms) and
    /// [`ItemVariants::resolve`] against the context it is drawing in.
    #[must_use]
    pub fn item(&self, item: &ResourceLocation) -> Option<&ItemGeometry> {
        self.items.get(item)?.gui()
    }

    /// Every baked form of an item plus the tree that chooses between them —
    /// the variant axis. `None` for an item with no bakeable geometry at all.
    #[must_use]
    pub fn item_forms(&self, item: &ResourceLocation) -> Option<&ItemVariants> {
        self.items.get(item)
    }

    /// The baked quads of an item's **inventory** icon — 3-D model or extruded
    /// sprite slab (empty when it has neither).
    /// Pose them with [`gui_item_pose`](crate::gui_item_pose) and mesh them with
    /// [`mesh_item_quads`](crate::mesh_item_quads); their UVs index
    /// [`atlas`](Self::atlas) and their tints [`tint_palette`](Self::tint_palette),
    /// exactly like [`quads`](Self::quads).
    #[must_use]
    pub fn item_quads(&self, item: &ResourceLocation) -> &[BakedQuad] {
        self.item(item).map_or(&[], |g| &g.quads)
    }

    /// The number of items with baked geometry of any kind.
    #[must_use]
    pub fn item_count(&self) -> usize {
        self.items.len()
    }

    /// The total number of baked `(item, model)` variants — at least
    /// [`item_count`](Self::item_count), and larger by however many extra models
    /// the pack's definitions name (vanilla 26.2: 2,012 for 1,474 items, with 84
    /// items contributing the extra 538).
    #[must_use]
    pub fn item_variant_count(&self) -> usize {
        self.items.values().map(ItemVariants::variant_count).sum()
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
    ///
    /// Yields the **inventory** form. A snapshotting consumer that draws items in
    /// the world or the hand wants [`item_forms_iter`](Self::item_forms_iter)
    /// instead, or it re-creates the flattening this API used to force.
    pub fn items(&self) -> impl Iterator<Item = (&ResourceLocation, &ItemGeometry)> {
        self.items
            .iter()
            .filter_map(|(id, variants)| variants.gui().map(|g| (id, g)))
    }

    /// Every item with baked geometry, as `(id, variants)` — the whole variant
    /// axis, for a consumer that snapshots the geometry and then resolves per
    /// frame (`lodestone-shell`'s `ModelRenderer::items` is exactly that).
    ///
    /// Order is arbitrary and unstable, as [`items`](Self::items).
    pub fn item_forms_iter(&self) -> impl Iterator<Item = (&ResourceLocation, &ItemVariants)> {
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

    /// Whether a state's quads should get per-corner smooth ambient occlusion
    /// (via [`crate::mesh_models`]'s corner sampling) or flat per-face light
    /// instead — see [`StateModel::ambient_occlusion`] for exactly which half
    /// of vanilla's gate this reflects.
    #[must_use]
    pub fn ambient_occlusion(&self, state_id: u32) -> bool {
        self.state(state_id).ambient_occlusion
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

    /// The still + flow (+ overlay, for water) sprite UVs for a fluid kind, into
    /// [`atlas`](Self::atlas).
    #[must_use]
    pub fn fluid_sprites(&self, kind: FluidKind) -> FluidSprites {
        match kind {
            FluidKind::Water => self.water_sprites,
            FluidKind::Lava => self.lava_sprites,
        }
    }

    /// Whether `state_id` is a vanilla `HalfTransparentBlock`/`LeavesBlock` — a
    /// fluid neighbour that should use the `water_overlay` material. See
    /// [`is_fluid_overlay_neighbor`].
    #[must_use]
    pub fn fluid_overlay(&self, state_id: u32) -> bool {
        self.fluid_overlay
            .get(state_id as usize)
            .copied()
            .unwrap_or(false)
    }
}

/// The `block/water_overlay` texture location — vanilla's
/// `FluidStateModelSet.WATER_MODEL`'s third `Material`. No blockstate or item
/// model references it, so it needs the same explicit atlas seeding as the
/// still/flow textures (see `build_complete_atlas`).
fn water_overlay_location() -> ResourceLocation {
    "minecraft:block/water_overlay"
        .parse()
        .expect("valid water_overlay location")
}

/// Resolve a fluid's still/flow (+ overlay, for water) sprite UV rects (first
/// animation frame) from the stitched atlas. Falls back to a zero rect if a
/// texture is missing, which bakes the fluid with a degenerate UV rather than
/// aborting the world.
fn resolve_fluid_sprites(atlas: &Atlas, kind: FluidKind) -> FluidSprites {
    let [still_loc, flow_loc] = fluid_texture_locations(kind);
    FluidSprites {
        still: sprite_uv(atlas, &still_loc),
        flow: sprite_uv(atlas, &flow_loc),
        // Only water has an overlay material in vanilla (`LAVA_MODEL`'s third
        // constructor argument is `null`).
        overlay: matches!(kind, FluidKind::Water).then(|| sprite_uv(atlas, &water_overlay_location())),
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
    // `water_overlay` likewise: referenced by no blockstate or item model (it's
    // wired up in Java as `FluidStateModelSet.WATER_MODEL`'s third `Material`).
    let _ = builder.load(manager, &water_overlay_location());
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

/// The `minecraft:special` item form plumbing — [`ItemVariants::resolve_special`]
/// and the fact that [`ItemVariants::resolve`] structurally cannot answer for it.
///
/// Hermetic: the definition tree is built from a literal copy of 26.2's own
/// `items/chest.json` shape rather than from the jar, so these run in `cargo test`
/// with no pack. The jar-backed half — that the real chest definition really is one
/// `special` node reaching `item/chest` — is
/// `lodestone-shell/tests/hotbar_special_item_pixels.rs`' business, which already
/// asserts the `kind`.
#[cfg(test)]
mod special_form_tests {
    use super::*;
    use lodestone_assets::GuiItemContext;

    /// 26.2's `items/chest.json`, shape for shape: a `minecraft:select` on the date
    /// whose non-default case is a Christmas texture, wrapping a `minecraft:special`
    /// whose `base` is `minecraft:item/chest`.
    ///
    /// The `select` layer is real and is reproduced rather than simplified away,
    /// because it is what makes the **fallback** arm the one every ordinary frame
    /// takes: nothing in this codebase supplies a `minecraft:local_time` property, so
    /// `ItemPropertyContext::select` returns `None` and `resolve_node` takes
    /// `fallback` — which is the plain chest. That is the correct default behaviour
    /// and the reason a chest is never drawn with the seasonal texture here.
    fn chest_definition() -> ItemModel {
        ItemModel::parse(
            br#"{
              "model": {
                "type": "minecraft:select",
                "property": "minecraft:local_time",
                "pattern": "MM-dd",
                "cases": [
                  {
                    "when": ["12-24", "12-25", "12-26"],
                    "model": {
                      "type": "minecraft:special",
                      "base": "minecraft:item/chest",
                      "model": { "type": "minecraft:chest", "texture": "minecraft:christmas" }
                    }
                  }
                ],
                "fallback": {
                  "type": "minecraft:special",
                  "base": "minecraft:item/chest",
                  "model": { "type": "minecraft:chest", "texture": "minecraft:normal" }
                }
              }
            }"#,
        )
        .expect("a parseable definition")
    }

    fn chest_variants() -> ItemVariants {
        let base: ResourceLocation = "minecraft:item/chest".parse().expect("a valid ref");
        let mut specials = HashMap::new();
        specials.insert(
            base,
            SpecialItemForm {
                kind: "minecraft:chest".to_string(),
                display: DisplayTransforms::NONE,
            },
        );
        ItemVariants {
            definition: chest_definition(),
            // **Empty on purpose**: a chest bakes no geometry at all. This is the
            // state the whole fix turns on — before it, an item in this state was
            // dropped from `BlockModels::items` entirely.
            by_model: HashMap::new(),
            gui: None,
            specials,
        }
    }

    /// The baked path answers `None` for a special-only item, at **every** accessor —
    /// and that is not a gap to be papered over, it is why a second accessor had to
    /// exist.
    ///
    /// **This is the control, and it is the arm that was failing before the fix.**
    /// The old held-item path was exactly `items.get(item).and_then(|v|
    /// v.resolve(&ctx))`, and every link in it yields nothing here: `resolve`'s
    /// `Special` arm is `None`, its `or_else(gui)` fallback is `None` because `gui` is
    /// `None` for a special item, and `by_model` is empty so there is nothing for
    /// either to have found. Zero geometry, three independent ways.
    #[test]
    fn the_baked_geometry_path_yields_nothing_for_a_special_only_item() {
        let variants = chest_variants();
        assert_eq!(variants.variant_count(), 0, "a chest bakes no model");
        assert!(variants.gui().is_none(), "and no inventory form");
        assert!(
            variants.resolve(&GuiItemContext).is_none(),
            "so `resolve` cannot answer, in the GUI context or any other — this is \
             the arm that drew an empty hand"
        );
        assert!(
            variants.variants().next().is_none(),
            "nothing baked means nothing to enumerate either"
        );
    }

    /// And the fix: the same item, the same context, through the new accessor —
    /// a `kind` a rig resolver can act on.
    ///
    /// The assertion is the `kind` string rather than "it is `Some`", because the
    /// `kind` is the load-bearing output: [`crate::special_item_rig`] is keyed on it,
    /// and a form carrying the wrong one resolves to a plausible wrong rig.
    #[test]
    fn the_special_accessor_answers_where_the_baked_one_cannot() {
        let variants = chest_variants();
        let form = variants
            .resolve_special(&GuiItemContext)
            .expect("a chest resolves to a special form");
        assert_eq!(form.kind, "minecraft:chest");
        // And the rig resolver accepts it, so the two halves really do join up.
        assert!(
            crate::special_item_rig(&form.kind, "chest").is_some(),
            "the kind this carries must be one `special_item_rig` recognises, or the \
             join is broken in the middle and both halves still look fine"
        );
    }

    /// The **seasonal** case: with no `minecraft:local_time` property supplied, the
    /// `select` takes its `fallback` — the plain chest — and the Christmas case is
    /// never reached.
    ///
    /// Asserted rather than left implicit, because "we silently dropped the seasonal
    /// texture" and "we correctly take the documented default" look identical from a
    /// screenshot in July. `outputs()` walks the whole tree, so both special nodes
    /// are discovered and both carry `kind == minecraft:chest`; only the *texture*
    /// differs, and that texture is not something this path reads at all — the sheet
    /// comes from the item path via `special_item_rig`. So the seasonal chest is a
    /// known, bounded shortfall: a chest on 25 December draws the ordinary sheet.
    #[test]
    fn with_no_date_property_the_select_takes_its_plain_fallback() {
        let definition = chest_definition();
        let resolved = definition.resolve(&GuiItemContext);
        assert_eq!(
            resolved.len(),
            1,
            "a `select` resolves to exactly one branch, never both"
        );
        // The whole tree still holds two special nodes — so the count below is what
        // proves resolution chose, rather than that there was only ever one choice.
        let all = definition.outputs().len();
        assert_eq!(
            all, 2,
            "the tree has both the seasonal and the plain special node; if this is 1 \
             the fallback test above proves nothing"
        );
    }
}
