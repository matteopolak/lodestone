//! The shared item-icon pass: everything needed to turn an item id into drawn
//! pixels in a 16×16 GUI slot, independent of *which* screen owns the slot.
//!
//! This started life inside [`crate::hud`], serving the nine hotbar cells. The
//! container/inventory screen ([`crate::container`]) needs exactly the same
//! thing — an icon, a stack count, a durability bar — for up to 46 slots, and
//! the alternative to sharing was a second copy of the atlas upload, the two
//! pipelines, the growable buffers and the pose/mesh call. Two copies of a
//! *pipeline* is not a style problem: it is a second 30 MB atlas upload and a
//! second tint palette that can silently drift green from the world's.
//!
//! # The two halves
//!
//! * **CPU** — [`draw_item_icon`] appends to three streams it does not own
//!   ([`IconSink`]): flat sprite quads, 3-D model vertices, and plain coloured
//!   quads for the count/durability chrome. Each screen keeps its own buffers
//!   and its own layout; only the per-slot emission is shared.
//! * **GPU** — [`IconRenderer`] owns the item-sprite pipeline (its own atlas
//!   texture) and the 3-D item-model pass (which *borrows* the world renderer's
//!   block atlas, tint palette and animation slots). Both are attached
//!   explicitly and both are absent by default, so a jar-less or headless run
//!   draws no icons at all — the negative control both pixel gates exercise.
//!
//! # Two kinds of icon
//!
//! Which stream a slot reaches is decided by the [`IconPart`] the item atlas
//! resolved:
//!
//! * [`IconPart::Sprite`] — the flat `item/generated` majority, one textured
//!   quad per layer off the item atlas;
//! * [`IconPart::Model`] — a **block** item, drawn as vanilla's isometric
//!   mini-block from geometry baked against the *block* atlas
//!   ([`BlockModels::item`]).
//!
//! * [`IconPart::Special`] — the ex-`builtin/entity` family (chest, shulker box,
//!   banner, shield, …), which in vanilla has **no item model and no block
//!   model**: every triangle comes from a block-entity renderer, and
//!   `BlockEntityWithoutLevelRenderer` reuses that same renderer for the
//!   inventory icon. We do the same, through [`SpecialIcons`] — see
//!   [`special_icon_geometry`] for which `kind`s have ported geometry today.
//!
//! # `Special` is a third stream, not a third case of the second
//!
//! It would be natural to expect a chest icon to ride the [`IconPart::Model`]
//! stream. It cannot, and the reason is the texture rather than the geometry.
//! The vertices, indices and part hierarchy transfer **unchanged** —
//! `BlockEntityMesh::part_transforms` takes an arbitrary placement matrix, so
//! [`gui_item_pose`] slots in exactly where `block_entity_placement_matrix` goes
//! in the world — but a chest's UVs are `[0,1]` against a standalone 64×64
//! `entity/chest/*.png`, while the model stream binds the stitched **block**
//! atlas, which contains nothing under `textures/entity/`. Routing a chest
//! through [`ModelIcons`] samples arbitrary block texels.
//!
//! And [`ModelIcons`] cannot simply bind a second texture: it spends **all four**
//! bind groups (camera / atlas / palette / anim), which is `wgpu`'s portable
//! `max_bind_groups` floor. So this pass reuses `EntityPipeline` instead, which
//! spends exactly **two**, consumes the same `ModelVertex` layout, and is
//! depth-tested against the depth attachment [`IconRenderer::draw_models`]
//! already clears — so it records into that *existing* pass and adds no fifth
//! group anywhere. See `docs/block-entity-renderers.md`.
//!
//! # Why not the `base` sprite fallback
//!
//! [`IconPart::Special`] carries a `base` model "the renderer can fall back to",
//! and drawing it as a flat quad looks like the cheap way out. It is not a way
//! out at all: **every one of the ten special `base` models in 26.2 has no
//! `elements` and no `layer0`** — only a `particle` texture, which is a *block*
//! texture (`block/oak_planks` for a chest, `block/soul_sand` for a skull) and is
//! not in the item atlas. `icon.rs`'s `classify_model` therefore classifies them
//! as undrawable, so the "fallback" draws the same zero pixels as no fallback at
//! all. Measured against the real jar, not assumed — the check is
//! `the_base_sprite_fallback_is_vacuous_for_every_special_kind` in
//! `tests/hotbar_special_item_pixels.rs`.
//!
//! What `base` *does* carry, and what this module uses it for, is the real
//! `display` map — a chest's `gui` pose is `[30, 45, 0]` at scale `0.625`,
//! authored on `item/template_chest`.

use std::collections::HashMap;
use std::sync::Arc;

use glam::Mat4;
// `font::metrics` used to be imported here for `LINE_HEIGHT`, which the stack
// count's vertical anchor was derived from. That fix replaced that derivation
// with vanilla's own constant (`COUNT_TOP`), so nothing in this module needs the
// font's metrics any more — see `COUNT_RIGHT`'s doc comment for why a derived
// anchor was the defect rather than the off-by-one.
use lodestone_assets::{
    Atlas, DisplaySlot, DisplayTransform, IconPart, ItemAtlas, ItemTintContext, ResourceLocation,
    SpriteLayer,
};
use lodestone_render::{
    BlockEntityModelSet, BlockModels, CameraUniform, EntityCameraUniform, EntityPipeline, GpuAtlas,
    GpuEntityModel, GuiSpriteQuad, ModelPipeline, ModelVertex, RenderLayer, block_entity_texture_stems,
    entity_camera_buffer, fog::FogUniform, gui_item_pose, gui_ortho, mesh_item_quads, ItemStateContext,
    model_shared_camera_buffer, section_origin_buffer, update_model_shared_camera_buffer,
    upload_instances_tinted,
};

use super::font;
use super::vanilla_font::{self, VanillaFont};
use super::{FLOATS_PER_VERTEX, HUD_GLINT_WGSL, HUD_SPRITE_WGSL, SPRITE_FLOATS_PER_VERTEX};

/// One occupied slot's drawable state, resolved shell-side from a
/// [`lodestone_game::menu::Menu`]. `item` is the item id
/// (`minecraft:diamond_pickaxe`) used to look up the resolved icon in the
/// [`ItemAtlas`]; `count` drives the stack number; `damage`/`max_damage` drive
/// the durability bar; `enchanted` marks items that draw the glint overlay.
///
/// Re-exported as [`crate::hud::HotbarSlot`], which is the name the hotbar has
/// always called it; the container screen builds the same record per menu slot.
#[derive(Debug, Clone)]
pub struct ItemIcon {
    /// The item id, e.g. `minecraft:stone` — the [`ItemAtlas`] icon key.
    pub item: ResourceLocation,
    /// Stack size; the number is drawn bottom-right when `> 1`.
    pub count: u32,
    /// Current damage, `Some` for damageable items that have taken damage.
    pub damage: Option<u32>,
    /// Max durability, `Some` for damageable items; pairs with `damage` for the
    /// bar fraction.
    pub max_damage: Option<u32>,
    /// Whether the stack should draw the enchantment glint. Fill it with
    /// [`stack_has_foil`] rather than by hand — it was hardcoded `false` at every
    /// producer until that fix, which is why nothing glinted.
    pub enchanted: bool,
    /// Index-zero `minecraft:custom_model_data` selector for per-stack item
    /// model resolution. `None` is vanilla's absent-component zero.
    pub custom_model_data: Option<i32>,
    /// The stack's `minecraft:dyed_color`, straight off
    /// `lodestone_game::item::ItemStack::dyed_color`. `None` for an undyed
    /// stack (or any non-dyeable item) — fed into [`sprite_layer_tint`]'s
    /// [`ItemTintContext`](lodestone_assets::item_tint::ItemTintContext) so the
    /// `dye` tint source resolves against the real stack instead of the
    /// definition's brown default. A producer with no live stack in hand
    /// (a recipe result, an advancement icon) leaves this `None`, which is the
    /// honest answer for those, not a shortcut.
    pub dyed_color: Option<u32>,
    /// The stack's already-mixed `minecraft:potion_contents` colour, straight
    /// off `lodestone_game::item::ItemStack::potion_color`. `None` for a
    /// non-potion item or one with no potion contents at all — see
    /// [`dyed_color`](Self::dyed_color)'s doc for the same "no live stack means
    /// `None`" contract.
    pub potion_color: Option<u32>,
    /// The stack's `minecraft:banner_patterns`, straight off
    /// `lodestone_game::item::ItemStack::banner_patterns`. Empty for every
    /// non-banner item, for a plain banner carrying no loom patterns, and for
    /// a producer with no live stack (a recipe result, an advancement icon)
    /// — the same "no live stack means the empty/absent state" contract
    /// [`dyed_color`](Self::dyed_color) documents, fed into
    /// [`push_special_icon`]'s translucent pattern-layer draws.
    pub banner_patterns: Vec<lodestone_model::BannerPatternLayer>,
    /// The stack's `minecraft:base_color`, straight off
    /// `lodestone_game::item::ItemStack::base_color`. `None` for a
    /// never-dyed shield and for every non-shield item — the same "no live
    /// stack means the empty/absent state" contract
    /// [`dyed_color`](Self::dyed_color) documents, fed into
    /// [`push_special_icon`]'s shield base-mask layer.
    pub base_color: Option<String>,
    /// The texture URL declared by the stack's `minecraft:profile` — a
    /// **custom** player head, the decorative kind a server places, whose whole
    /// appearance is that property.
    ///
    /// `None` for every other item, for a head carrying no profile (a plain
    /// `minecraft:player_head`, which really is Steve), and for a producer with
    /// no live stack — the same "no live stack means the empty/absent state"
    /// contract [`dyed_color`](Self::dyed_color) documents. Fill it with
    /// [`stack_skin_url`] rather than by hand: that is also what starts the
    /// fetch, and a URL that nothing ever requested resolves to the default
    /// sheet forever.
    ///
    /// [`Arc<str>`] rather than `String` for [`lodestone_render::BlockEntityTexture`]'s
    /// reason: the URL arrives from the server, it is cloned through the record,
    /// the draw and the batch key, and none of those hops should copy an
    /// unbounded server-provided string.
    pub skin: Option<Arc<str>>,
}

/// Say once, per process, that a head declares a skin we cannot use.
///
/// Reached from the per-slot draw loop, so it runs for every head in every open
/// container on every frame — `Once` is what keeps it a report rather than a
/// flood, and the message therefore describes the class rather than the one
/// stack that happened to be first.
///
/// This is a warning and not a silent default because the failure is invisible
/// at the draw site: a plain head *is* a head, so nothing looks broken, nothing
/// is red, and the only evidence is that it is the wrong face. That is exactly
/// why a custom head drew Steve in every inventory for as long as it did.
fn warn_a_head_profile_declares_no_usable_skin() {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        tracing::warn!(
            target: "assets",
            "a player head's minecraft:profile carries a `textures` property that \
             decodes to no usable skin url, so it draws the DEFAULT skull sheet. \
             Either the base64/JSON is malformed or its payload has no SKIN entry"
        );
    });
}

/// The skin URL a stack's `minecraft:profile` declares, **and the fetch that
/// makes it drawable**.
///
/// The side effect is deliberate and belongs here rather than at the call
/// sites: every producer of an [`ItemIcon`] needs both halves, and a producer
/// that resolved the URL without requesting it would draw the default sheet
/// forever with nothing red — the same island shape this whole path already
/// cost. `crate::remote_skins::request` owns deduplication and failure
/// memoisation, so calling it once per head per frame is a hash lookup after
/// the first, exactly as the placed-head path's own per-frame
/// `request_player_head_skins` is.
///
/// The decode is `crate::remote_skins::skin_for_textures_property`, shared with
/// the placed-head path rather than re-spelled: both start from the same
/// base64-JSON Mojang payload, and a second parse beside a working one is worse
/// than none.
///
/// `None` is the normal answer for every non-head item and for a head with no
/// profile. A profile that carries a `textures` property we cannot use is *not*
/// normal and says so — see [`warn_a_head_profile_declares_no_usable_skin`].
#[must_use]
pub(crate) fn profile_skin_url(profile: &lodestone_model::ItemProfile) -> Option<Arc<str>> {
    let value = profile
        .properties
        .iter()
        .find(|p| p.name == "textures")
        .map(|p| p.value.as_str())?;
    let Some(skin) = crate::remote_skins::skin_for_textures_property(value) else {
        warn_a_head_profile_declares_no_usable_skin();
        return None;
    };
    // Idempotent per URL; see this function's doc.
    crate::remote_skins::request(&skin.url);
    Some(Arc::<str>::from(skin.url))
}

/// [`profile_skin_url`] for a game-facing stack.
///
/// This narrow wrapper keeps every consumer that already has a full game stack
/// on the same profile decoder and request path as producers that still hold
/// the model-layer stack decoded directly from `SET_EQUIPMENT`.
#[must_use]
pub(crate) fn stack_skin_url(stack: &lodestone_game::item::ItemStack) -> Option<Arc<str>> {
    // Match PlayerHeadSpecialRenderer.extractArgument: `PROFILE` is read by
    // the player-head item renderer, not by every arbitrary stack that happens
    // to carry that data component. The item-model component may replace only
    // the client definition lookup; the underlying player-head item remains
    // the semantic owner of the profile.
    if stack.item().namespace() != "minecraft" || stack.item().path() != "player_head" {
        return None;
    }
    profile_skin_url(&stack.profile()?)
}

/// `ItemStack.hasFoil()` for a shell-side [`lodestone_game::item::ItemStack`],
/// delegating the actual predicate to
/// [`lodestone_render::glint::has_foil_for_item`].
///
/// # Why this adapter exists
///
/// [`lodestone_render::glint::has_foil_for_item`] wants a borrowed
/// `&[lodestone_model::item::ItemEnchantment]` alongside the item id. What the
/// shell has in hand is `lodestone_game::item::ItemComponents`, an opaque
/// `BTreeMap<Identifier, ComponentValue>`, so pulling the enchantments list out
/// needs this bridge — it does not re-spell the predicate itself, so the render
/// crate stays the single owner of *what foil means* (enchantments content, and
/// now also the seven-item baked-override census) and of the shortfalls
/// documented there.
///
/// # Two sources feed the predicate now, not one
///
/// Until the baked-override census landed, this function read only the
/// `minecraft:enchantments` component, which is why an unenchanted
/// `minecraft:enchanted_book` never glinted: vanilla's own `isEnchanted` does
/// not read `STORED_ENCHANTMENTS` either, so no amount of attaching
/// enchantment content to the stack could have fixed it. `has_foil_for_item`
/// checks the item id against the baked census first and only falls back to
/// the enchantments list when the census has no opinion — see its doc, and
/// [`lodestone_render::glint::has_foil_enchantments`]'s, for the full rule and
/// the remaining live per-stack-override gap.
///
/// # The invariant the enchantments half leans on
///
/// The key is present **only when the list is non-empty** —
/// `lodestone_game::item::ItemStack`'s `From<&lodestone_model::ItemStack>` inserts
/// `minecraft:enchantments` under an `if !stack.components.enchantments.is_empty()`
/// guard. So an absent key and a present-but-empty list mean the same thing here,
/// and the `is_empty` check inside `has_foil_enchantments` (which
/// `has_foil_for_item` falls back to) is what makes this correct either way
/// rather than dependent on that guard holding.
#[must_use]
pub(crate) fn stack_has_foil(stack: &lodestone_game::item::ItemStack) -> bool {
    let enchantments: &[lodestone_model::item::ItemEnchantment] = match stack
        .components()
        .get_str(lodestone_game::item::ENCHANTMENTS_COMPONENT)
    {
        Some(lodestone_game::item::ComponentValue::Enchantments(list)) => list,
        _ => &[],
    };
    lodestone_render::glint::has_foil_for_item(&stack.item().to_string(), enchantments)
}

/// The baked resources an icon resolves against. Both are optional and both
/// being `None` is the jar-less path, where [`draw_item_icon`] emits chrome
/// (count, durability) but no icon.
#[derive(Debug, Clone, Copy)]
pub(crate) struct IconAssets<'a> {
    /// The flat item-sprite atlas; `None` leaves sprite icons undrawn.
    pub items: Option<&'a ItemAtlas>,
    /// The baked model set, for items whose icon is a 3-D mini-block; `None`
    /// leaves block icons undrawn.
    pub models: Option<&'a BlockModels>,
}

/// The three vertex streams an icon appends to, borrowed from whichever screen
/// is drawing. Kept as three `&mut Vec` rather than an owned struct so the
/// caller's existing buffers are filled in place and nothing is copied.
pub(crate) struct IconSink<'o> {
    /// The count text and durability bar.
    pub colour: ColourStream<'o>,
    /// Flat `[x, y, u, v, r, g, b, a]` quads sampling the **item** atlas.
    pub sprite: &'o mut Vec<f32>,
    /// 3-D block-item geometry, already posed into GUI pixel space.
    pub model: &'o mut Vec<ModelVertex>,
    /// One entry per [`IconPart::Special`] slot with ported geometry: *which*
    /// block-entity mesh and sheet to draw, and the placement matrix to draw it
    /// under. Unlike [`Self::model`] this is **not** vertices — the block-entity
    /// meshes are uploaded once at attach time and posed per frame through a
    /// per-part instance buffer, exactly as the world pass does, so a chest icon
    /// costs ~14 matrices rather than a re-transformed vertex copy.
    pub special: &'o mut Vec<SpecialIconDraw>,
    /// The **glint** stream: a copy of every flat sprite quad belonging to an
    /// enchanted stack, in the same `[x, y, u, v, r, g, b, a]` layout as
    /// [`Self::sprite`] so one `push_sprite_quad` builds both.
    ///
    /// A separate stream rather than a per-vertex flag because the enchanted
    /// quads are not contiguous within [`Self::sprite`] — one enchanted sword
    /// among nine hotbar cells would need nine draw ranges — and because the
    /// glint draws on its own pipeline with its own blend anyway. Only
    /// [`IconPart::Sprite`] reaches here: the 3-D block-item and special-renderer
    /// streams glint through `lodestone_render::glint`'s depth-`EQUAL` pipeline,
    /// which this one exists precisely because it cannot serve.
    pub glint: &'o mut Vec<f32>,
}

/// One special-renderer icon draw. Two shapes, because a banner needs a
/// second, translucent kind of draw no other special renderer does — see
/// [`push_special_icon`]'s banner branch.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum SpecialIconDraw {
    /// A baked block-entity mesh, the sheet it samples, and where in GUI
    /// pixel space to put it — a chest, a shulker box, a skull, or (now
    /// always **untinted**) a banner's own pole/bar and flag.
    ///
    /// `placement` goes straight into `BlockEntityMesh::part_transforms` in
    /// place of the world's `block_entity_placement_matrix` — that
    /// substitution is the whole seam between the world's chest and the one
    /// in your hand.
    Mesh {
        /// The [`BlockEntityModelSet`] model name, e.g. [`lodestone_render::CHEST_SINGLE`].
        model: &'static str,
        /// Which sheet to sample: a jar texture stem (`entity/chest/normal`) for
        /// every kind whose appearance is fixed by its item id, or a fetched
        /// **player skin** URL for a custom head.
        ///
        /// [`lodestone_render::BlockEntityTexture`] rather than a second,
        /// GUI-local enum: the world's placed-head pass already solved exactly
        /// this — a batch key that is `&'static str` for a packaged sheet and an
        /// [`Arc<str>`] for a server-provided URL — and two spellings of one
        /// identity is how a head ends up correct in the world and plain in a
        /// slot, which is the bug this field exists to close.
        texture: lodestone_render::BlockEntityTexture,
        /// GUI-pixel-space placement, from [`gui_item_pose`].
        placement: Mat4,
        /// Gamma-space `[r, g, b]` multiplied into every texel. `[255, 255,
        /// 255]` (a no-op) for every kind, banner included — colour rides
        /// [`Self::BannerLayer`] now, not this field.
        tint: [u8; 3],
    },
    /// One translucent banner/shield pattern-mask layer, drawn over a
    /// banner's flag mesh (or a shield's whole plate+handle mesh — see
    /// [`Self::BannerLayer`]'s `family`) through the alpha-blended
    /// banner-layer pipeline — the GUI-icon sibling of
    /// `super::super::gpu::block_entities::BannerLayerDrawBatch` and
    /// `super::super::gpu::first_person::HandBannerLayerDraw`. See
    /// [`push_special_icon`]'s doc for why colour lives here rather than on
    /// [`Self::Mesh`]'s own `tint`.
    BannerLayer {
        /// Which mesh/mask-map this layer resolves against — see
        /// [`PatternFamily`]'s doc.
        family: PatternFamily,
        /// Bare pattern asset id, keying [`SpecialIcons::banner_patterns`] or
        /// `::shield_patterns`, per [`Self::family`].
        pattern: String,
        /// GUI-pixel-space placement — the same one the corresponding
        /// [`Self::Mesh`] entry carries, so the mask sits exactly over it.
        placement: Mat4,
        /// Gamma-space `[r, g, b]` bytes to tint the mask by.
        color: [u8; 3],
    },
}

/// Which mesh and which mask map a [`SpecialIconDraw::BannerLayer`] resolves
/// against — the GUI-icon sibling of
/// `super::super::gpu::first_person::HandPatternFamily`, for the identical
/// reason: a banner's layers redraw only the `"flag"` part of
/// `"banner_flag"`, a shield's redraw the *whole* `"shield"` mesh (both
/// `plate` and `handle`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PatternFamily {
    Banner,
    Shield,
}

/// The block-entity mesh and sheet for one special-renderer `kind`, keyed on the
/// **`kind`** and not on the item id.
///
/// Keying on the item id is the obvious shortcut and it is wrong: the family is
/// ten `kind`s over 91 item definitions (chest, trapped chest, ender chest, four
/// weathering stages of copper chest, 17 shulker box colours, 16 banners, shield,
/// trident, conduit, decorated pot, six heads, player head, and 32 copper golem
/// statue states). A chest-only `match` on `minecraft:chest` the item leaves the
/// other twelve chest definitions invisible too.
///
/// The item id is still consulted, but only *within* a `kind`, to pick the sheet:
/// `kind` says "this is a chest", the item path says "an oxidized copper one".
/// That is exactly vanilla's split — `ChestSpecialRenderer.Unbaked` carries the
/// `texture` field the item definition names, and `chest_type` defaults to
/// `SINGLE`, which is why an item chest is always the single-chest layer and
/// never one of the two double halves.
///
/// # This is now a one-line delegation, and that is the point
///
/// The mapping used to live here, in the GUI pass, with a chest arm and nothing
/// else. It now lives in [`lodestone_render::special_item_rig`], because the *3-D*
/// surfaces — the first-person hand, another entity's hand, a dropped stack, an
/// item frame — need exactly the same answer, and a second copy is how a chest ends
/// up correct in the inventory and oak-coloured in the hand. Read that function for
/// which `kind`s resolve today and why the others do not; this wrapper exists only
/// to strip the namespace off the item id.
fn special_icon_geometry(kind: &str, item: &ResourceLocation) -> Option<(&'static str, &'static str)> {
    lodestone_render::special_item_rig(kind, item.path())
}

/// Draw one slot's icon into the `size`×`size` rect at `(x, y)`: the icon
/// itself, its durability bar, and its stack count. `view` is the target
/// viewport in pixels, which is all the NDC conversion needs. `font` is the
/// vanilla proportional font the stack count draws with; `None` falls back to
/// the fixed-advance 5×7 debug font, exactly as every other piece of text in
/// this crate degrades on a jar-less run.
///
/// See the [module docs](self) for which icon kind reaches which stream.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_item_icon(
    sink: &mut IconSink<'_>,
    assets: &IconAssets<'_>,
    view: (f32, f32),
    slot: &ItemIcon,
    x: f32,
    y: f32,
    size: f32,
    font: Option<&VanillaFont>,
) {
    draw_item_icon_counted(sink, assets, view, slot, x, y, size, font, COUNT_INK);
}

/// The ink vanilla's `itemDecorations` draws a stack count in: plain white.
pub(crate) const COUNT_INK: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// `ChatFormatting.YELLOW` — `0xFFFF55`, the colour vanilla's
/// `AbstractContainerScreen.extractSlot` (`:214`) uses for a drag preview's count
/// once it has been **clamped** by the destination cell's cap, so the player can
/// see the split was cut short. See [`draw_item_icon_counted`].
pub(crate) const COUNT_INK_CLAMPED: [f32; 4] = [1.0, 1.0, 85.0 / 255.0, 1.0];

/// As [`draw_item_icon`], but with an explicit ink for the stack count.
///
/// Exists for one caller: the drag preview, which draws a *provisional* count and
/// needs vanilla's yellow when that count was clamped. Everything else passes
/// [`COUNT_INK`] through [`draw_item_icon`], so no other call site learns about
/// this parameter.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_item_icon_counted(
    sink: &mut IconSink<'_>,
    assets: &IconAssets<'_>,
    view: (f32, f32),
    slot: &ItemIcon,
    x: f32,
    y: f32,
    size: f32,
    font: Option<&VanillaFont>,
    count_ink: [f32; 4],
) {
    let (vw, vh) = view;
    let scale = size / 16.0;
    let custom_model_drawn = slot.custom_model_data.is_some_and(|value| {
        push_item_model_variant(sink.model, assets.models, &slot.item, value as f32, x, y, size)
    });
    if !custom_model_drawn
        && let Some(atlas) = assets.items
        && let Some(icon) = atlas.icon(&slot.item)
    {
        for part in &icon.parts {
            match part {
                IconPart::Sprite { layers } => {
                    for layer in layers {
                        if let Some(spr) = atlas.sprite(&layer.sprite) {
                            let quad = GuiSpriteQuad {
                                dst: [x, y, size, size],
                                uv_min: spr.uv_min,
                                uv_max: spr.uv_max,
                            };
                            push_sprite_quad(sink.sprite, vw, vh, quad, sprite_layer_tint(layer, slot));
                            if slot.enchanted {
                                push_glint_quad(sink.glint, vw, vh, quad);
                            }
                        }
                    }
                }
                // The part's own `model`/`transform`/`gui_light` are *not* used
                // here: `BlockModels::build` resolved and baked exactly this
                // part (through the same `GuiItemContext`) and stored the
                // transform alongside the quads, so the item id is the whole
                // key. Going back to the part would risk the two disagreeing.
                IconPart::Model { .. } => {
                    push_item_model(sink.model, assets.models, &slot.item, x, y, size);
                }
                // The pose comes from `icon.display`, not from the part: the
                // part's `base` is a geometry-less shell whose only real content
                // *is* its `display` map, and `icon.display` is already that map
                // (`ItemIconBuilder::part_for` resolves the base model purely to
                // read it). `DisplaySlot::Gui` on a chest is `[30, 45, 0]` at
                // `0.625` — note **45**, where a block item is 225, so a chest
                // faces the viewer rather than showing a corner.
                IconPart::Special {
                    kind,
                    transformation,
                    ..
                } => {
                    push_special_icon(
                        sink.special,
                        &slot.item,
                        kind,
                        &slot.banner_patterns,
                        slot.base_color.as_deref(),
                        slot.skin.as_ref(),
                        transformation,
                        &icon.display.get(DisplaySlot::Gui),
                        x,
                        y,
                        size,
                    );
                }
            }
        }
    }

    // Durability bar: a 13px track 2px above the slot bottom, filled by the
    // remaining fraction, hue lerped green→red. Vanilla only draws it for a
    // damaged item, so a pristine tool shows none.
    if let (Some(dmg), Some(max)) = (slot.damage, slot.max_damage)
        && dmg > 0
        && max > 0
    {
        let remaining = 1.0 - (dmg.min(max) as f32 / max as f32);
        let bx = x + 2.0 * scale;
        let bw = 13.0 * scale;
        let by = y + size - 3.0 * scale;
        let bh = 2.0 * scale;
        sink.colour.rect(bx, by, bw, bh, [0.0, 0.0, 0.0, 1.0]);
        let col = [1.0 - remaining, remaining, 0.0, 1.0];
        sink.colour
            .rect(bx, by, (bw * remaining).max(1.0 * scale), bh, col);
    }

    // Stack count, bottom-right, on the colour stream so it lands on top of
    // the icon.
    //
    // `scale` (`size / 16.0`) is the same factor the icon itself is drawn at,
    // so text sized at `scale` sits in the same GUI-pixel convention as
    // everything else in the slot — which is what makes it look right once a
    // real `gui_scale` multiplies every one of these sizes uniformly. The
    // previous fixed-font fallback instead scaled the digits by an extra 2x
    // on top of that, which is what actually produced the "too big" bug: the
    // count ballooned relative to the icon it sits on, not just relative to
    // the slot. **Do not reintroduce a second multiplier here.**
    if slot.count > 1 {
        let s = slot.count.to_string();
        let tw = match font {
            Some(f) => f.width(&s, scale),
            None => text_w(&s, scale),
        };
        let tx = x + COUNT_RIGHT * scale - tw;
        let ty = y + COUNT_TOP * scale;
        match font {
            // Vanilla text: real glyph widths and vanilla's own 1px / 25%
            // brightness drop shadow, both handled by `VanillaFont::draw`.
            Some(f) => f.draw(&mut sink.colour, &s, tx, ty, scale, count_ink),
            // Jar-less fallback: the fixed-advance 5x7 debug font, with the
            // same vanilla shadow colour rather than a pure black one.
            None => {
                let shadow = vanilla_font::shadow_of(count_ink);
                sink.colour.text(&s, tx + scale, ty + scale, scale, shadow);
                sink.colour.text(&s, tx, ty, scale, count_ink);
            }
        }
    }
}

/// Vanilla's pickup-"pop" destination rect (`Hud.java`), as a pure
/// function so the transform math is checkable with no atlas, no sink and no
/// `ItemStack` — just numbers in, numbers out.
///
/// `squeeze = 1 + pop / 5`; vanilla's
/// `translate(x+8, y+12); scale(1/squeeze, (squeeze+1)/2); translate(-(x+8), -(y+12))`
/// is an axis-aligned, non-uniform scale about the fixed point `(x + 8, y +
/// 12)` (both scaled by `size / 16`, matching every other length in
/// [`draw_item_icon_popped`]) — i.e. each edge moves to
/// `pivot + (edge - pivot) * scale`, **not** a rect re-centred on the pivot:
/// `8` happens to be half of the 16px icon's width, so the pivot is the
/// horizontal centre and the two are equivalent for `x`, but `12` is not half
/// of its height, so for `y` they are not, and the pivot sits below the
/// icon's own vertical centre. `pop <= 0.0` returns the original square rect
/// unchanged (`squeeze == 1.0` makes both axis scales `1.0`, the identity).
fn pop_squeeze_rect(x: f32, y: f32, size: f32, pop: f32) -> [f32; 4] {
    let scale = size / 16.0;
    let squeeze = 1.0 + pop.max(0.0) / 5.0;
    let scale_x = 1.0 / squeeze;
    let scale_y = (squeeze + 1.0) / 2.0;
    let pivot_x = x + 8.0 * scale;
    let pivot_y = y + 12.0 * scale;
    let new_x = pivot_x + (x - pivot_x) * scale_x;
    let new_y = pivot_y + (y - pivot_y) * scale_y;
    [new_x, new_y, size * scale_x, size * scale_y]
}

/// As [`draw_item_icon`], but the icon layer squashes/stretches through
/// vanilla's pickup "pop" animation before settling.
///
/// `pop` is vanilla's `ItemStack.getPopTime() - partialTick`
/// (`Hud.java`): `5.0` the instant a stack lands in the slot — set by
/// `Inventory.add` whenever an item merges into or fills one
/// (`Inventory.java`) — decaying to `0.0` over 5 ticks
/// (`ItemStack.java`, one tick per call there). `0.0` (idle) draws
/// pixel-identically to [`draw_item_icon`]; every caller of that function is
/// unaffected by this one existing.
///
/// Only the **flat [`IconPart::Sprite`] layer squashes.** A 3-D block-item
/// mini-icon or a special-renderer (chest) icon draws undistorted, at the
/// original square rect — a deliberate, documented narrowing (most hotbar
/// items are flat sprites; vanilla's single pose-stack transform covers all
/// three, this does not), not a decode-parity claim.
///
/// The durability bar and stack count draw **unsquashed**, at the original
/// `(x, y, size)` — vanilla's own `graphics.itemDecorations` call sits after
/// the pose is popped (`Hud.java`), outside the transform, and
/// [`draw_item_icon_counted`] already draws that tail at squeeze `1.0`; this
/// duplicates just that tail rather than sharing it, so this function stays
/// fully self-contained and callers of [`draw_item_icon_counted`] (the
/// container screen) are untouched by its existence.
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw_item_icon_popped(
    sink: &mut IconSink<'_>,
    assets: &IconAssets<'_>,
    view: (f32, f32),
    slot: &ItemIcon,
    x: f32,
    y: f32,
    size: f32,
    font: Option<&VanillaFont>,
    pop: f32,
) {
    if pop <= 0.0 {
        draw_item_icon(sink, assets, view, slot, x, y, size, font);
        return;
    }
    let (vw, vh) = view;
    let scale = size / 16.0;
    let [dst_x, dst_y, dst_w, dst_h] = pop_squeeze_rect(x, y, size, pop);

    if let Some(atlas) = assets.items
        && let Some(icon) = atlas.icon(&slot.item)
    {
        for part in &icon.parts {
            match part {
                IconPart::Sprite { layers } => {
                    for layer in layers {
                        if let Some(spr) = atlas.sprite(&layer.sprite) {
                            let quad = GuiSpriteQuad {
                                dst: [dst_x, dst_y, dst_w, dst_h],
                                uv_min: spr.uv_min,
                                uv_max: spr.uv_max,
                            };
                            push_sprite_quad(sink.sprite, vw, vh, quad, sprite_layer_tint(layer, slot));
                            if slot.enchanted {
                                push_glint_quad(sink.glint, vw, vh, quad);
                            }
                        }
                    }
                }
                // Undistorted — see the doc comment above.
                IconPart::Model { .. } => {
                    push_item_model(sink.model, assets.models, &slot.item, x, y, size);
                }
                IconPart::Special {
                    kind,
                    transformation,
                    ..
                } => {
                    push_special_icon(
                        sink.special,
                        &slot.item,
                        kind,
                        &slot.banner_patterns,
                        slot.base_color.as_deref(),
                        slot.skin.as_ref(),
                        transformation,
                        &icon.display.get(DisplaySlot::Gui),
                        x,
                        y,
                        size,
                    );
                }
            }
        }
    }

    // Durability bar + stack count: unsquashed — see the doc comment above.
    // Duplicated from `draw_item_icon_counted`'s tail rather than shared.
    if let (Some(dmg), Some(max)) = (slot.damage, slot.max_damage)
        && dmg > 0
        && max > 0
    {
        let remaining = 1.0 - (dmg.min(max) as f32 / max as f32);
        let bx = x + 2.0 * scale;
        let bw = 13.0 * scale;
        let by = y + size - 3.0 * scale;
        let bh = 2.0 * scale;
        sink.colour.rect(bx, by, bw, bh, [0.0, 0.0, 0.0, 1.0]);
        let col = [1.0 - remaining, remaining, 0.0, 1.0];
        sink.colour
            .rect(bx, by, (bw * remaining).max(1.0 * scale), bh, col);
    }

    if slot.count > 1 {
        let s = slot.count.to_string();
        let tw = match font {
            Some(f) => f.width(&s, scale),
            None => text_w(&s, scale),
        };
        let tx = x + COUNT_RIGHT * scale - tw;
        let ty = y + COUNT_TOP * scale;
        match font {
            Some(f) => f.draw(&mut sink.colour, &s, tx, ty, scale, COUNT_INK),
            None => {
                let shadow = vanilla_font::shadow_of(COUNT_INK);
                sink.colour.text(&s, tx + scale, ty + scale, scale, shadow);
                sink.colour.text(&s, tx, ty, scale, COUNT_INK);
            }
        }
    }
}

/// Where a stack count's **right edge** sits, in slot-local GUI pixels.
///
/// `GuiGraphicsExtractor.itemCount` (`:947-952`, and identically
/// `SpectatorGui.java`):
///
/// ```java
/// this.text(font, amount, x + 19 - 2 - font.width(amount), y + 6 + 3, -1, true);
/// ```
///
/// so `19 - 2 = 17` — **one pixel past** the 16 px icon's right edge, which is why
/// this is not `size`.
///
/// # Why these are constants and the old code was derived
///
/// The previous anchor was `x + size - width` and `y + size - LINE_HEIGHT * scale`.
/// Both are *derivations*, and that is the actual defect rather than the off-by-one:
/// they drift whenever the cell size or the font's line height changes, and they
/// agree with vanilla at no glyph height at all. `LINE_HEIGHT` is 9, so the old
/// top was `y + 16 - 9 = y + 7` against vanilla's `y + 9` — **2 px high**, which is
/// the "should sit lower" half of the report. Vanilla's offsets are constants
/// relative to the slot origin; these are those constants.
///
/// Scaled by `scale` (`size / 16.0`) like every other length in this function, so a
/// non-16 px cell places the count proportionally rather than at a fixed pixel
/// offset from a differently-sized icon.
const COUNT_RIGHT: f32 = 17.0;
/// Where a stack count's **top edge** sits, in slot-local GUI pixels — vanilla's
/// `y + 6 + 3`. See [`COUNT_RIGHT`].
const COUNT_TOP: f32 = 9.0;

/// Emit the 3-D isometric mini-block for a **block** item into the `size`×`size`
/// pixel rect at `(x, y)`.
///
/// The geometry was baked once at asset-load time against the *block* atlas and
/// interned through the *block* tint palette, so nothing is resolved here: the
/// pose is `gui_item_pose` over the stored `display.gui` transform, and
/// [`mesh_item_quads`] applies it, fixes the light full-bright, and puts
/// `gui_light` in the `ao` slot. The result is already in **GUI pixel space** (x
/// right, y down, z toward the viewer) — the pass's `view_proj` is
/// [`gui_ortho`], which finishes the job.
///
/// Indices are expanded into a flat triangle list because the other two streams
/// are non-indexed; the expansion preserves the mesh's winding, which is
/// load-bearing (see `docs/item-gui-geometry.md`: the visible faces are the ones
/// back-face culling keeps, and they must also be the nearest under the depth
/// test). A no-op when no model set is attached or the item has no baked
/// geometry, so jar-less runs keep the old empty well.
fn push_item_model(
    out: &mut Vec<ModelVertex>,
    models: Option<&BlockModels>,
    item: &ResourceLocation,
    x: f32,
    y: f32,
    size: f32,
) {
    let Some(models) = models else {
        return;
    };
    let Some(geometry) = models.item(item) else {
        return;
    };
    let pose = gui_item_pose([x, y, size, size], &geometry.transform);
    let mesh = mesh_item_quads(&geometry.quads, pose, geometry.gui_light);
    out.extend(mesh.indices.iter().map(|&i| mesh.vertices[i as usize]));
}

/// Dynamic counterpart to [`push_item_model`] for a stack whose custom model
/// data selects a non-default branch. Generated sprite variants are baked into
/// `BlockModels` too, so this covers both a pack's 3-D gun and a flat override.
fn push_item_model_variant(
    out: &mut Vec<ModelVertex>,
    models: Option<&BlockModels>,
    item: &ResourceLocation,
    custom_model_data: f32,
    x: f32,
    y: f32,
    size: f32,
) -> bool {
    let Some(forms) = models.and_then(|models| models.item_forms(item)) else {
        return false;
    };
    let context = ItemStateContext::new(DisplaySlot::Gui).with_custom_model_data(custom_model_data);
    log_diamond_sword_model_resolution("gui", item, custom_model_data, &context, forms);
    let Some(geometry) = forms.resolve(&context) else {
        return false;
    };
    let pose = gui_item_pose([x, y, size, size], &geometry.transform);
    let mesh = mesh_item_quads(&geometry.quads, pose, geometry.gui_light);
    out.extend(mesh.indices.iter().map(|&i| mesh.vertices[i as usize]));
    true
}

/// Reports one GUI resolution for each `(pack generation, custom-model-data)`
/// pair. The target is deliberately opt-in: `RUST_LOG=pack_trace=debug` turns
/// a visible vanilla sword into its component/selector/model evidence without
/// writing one line per frame.
fn log_diamond_sword_model_resolution(
    surface: &'static str,
    item: &ResourceLocation,
    custom_model_data: f32,
    context: &ItemStateContext,
    forms: &lodestone_render::ItemVariants,
) {
    if item.namespace() != "minecraft"
        || item.path() != "diamond_sword"
        || !tracing::enabled!(target: "pack_trace", tracing::Level::DEBUG)
    {
        return;
    }
    static LAST: std::sync::OnceLock<std::sync::Mutex<Option<(u64, i32)>>> = std::sync::OnceLock::new();
    let generation = crate::resources::pack_generation();
    let data = custom_model_data as i32;
    let Ok(mut last) = LAST.get_or_init(|| std::sync::Mutex::new(None)).lock() else {
        return;
    };
    if *last == Some((generation, data)) {
        return;
    }
    *last = Some((generation, data));

    let outputs = forms.definition().resolve(context);
    let chosen_model_is_baked = outputs.iter().any(|output| {
        matches!(output, lodestone_assets::ItemModelOutput::Model { model, .. } if forms.variant(model).is_some())
    });
    tracing::debug!(
        target: "pack_trace",
        surface,
        item = %item,
        custom_model_data,
        ?context,
        ?outputs,
        chosen_model_is_baked,
        resolved_geometry = forms.resolve(context).is_some(),
        "diamond-sword item-model selector evaluated"
    );
}

/// Record one [`IconPart::Special`] slot for the block-entity icon pass.
///
/// A no-op for a `kind` whose geometry is not ported (see
/// [`special_icon_geometry`]) — the pre-existing zero-pixel behaviour, not a new
/// one. Nothing here touches the GPU or needs the pass to be attached: the
/// placement matrix is cheap and [`IconRenderer::upload`] discards the list when
/// there is nowhere to draw it, which keeps this function callable from the
/// jar-less path exactly like its two siblings.
///
/// # The pose is vanilla's, including the part that looks like an off-by-one
///
/// [`gui_item_pose`] composes `T(centre) · S(w, -h, min(w,h)) · display_matrix`,
/// and `display_matrix` ends with the `-0.5` centring translate. A chest spans
/// `y 0..14` texels, so centring it about `0.5` puts it ~0.6 px low in a 16 px
/// cell rather than resting on the cell floor. That is **not** a bug to correct:
/// `ItemTransform.apply` ends with `pose.translate(-0.5F, -0.5F, -0.5F)` in both
/// its branches, and `ItemStackRenderState.Layer.applyTransform` calls it for the
/// `specialRenderer != null` branch and the ordinary quad branch from the *same*
/// line — read from the record definition rather than from a summary of the call
/// site. Vanilla centres a chest icon about the block centre too.
///
/// `node_transform` is the item definition's whole root-to-`special`
/// `"transformation"` chain, outermost first, and is folded *underneath*
/// `transform`'s `display.gui` pose via
/// [`lodestone_render::compose_special_item_transform`] — see that
/// function's doc for why it is a right-, not left-, multiply, and for why an
/// ancestor node's entry counts.
///
/// `patterns` is the slot's own decoded `minecraft:banner_patterns` (empty
/// for every non-banner/non-shield slot and for a producer with no live
/// stack, e.g. a recipe result) — consulted on the `minecraft:banner` and
/// `minecraft:shield` branches, to build the translucent
/// [`SpecialIconDraw::BannerLayer`] entries that carry the real colour.
/// `base_color` is the slot's `minecraft:base_color` (shield only —
/// `None` for a banner), which decides both a shield's opaque sheet
/// ([`lodestone_render::shield_has_patterns`]) and its base-mask tint.
#[allow(clippy::too_many_arguments)]
fn push_special_icon(
    out: &mut Vec<SpecialIconDraw>,
    item: &ResourceLocation,
    kind: &str,
    patterns: &[lodestone_model::BannerPatternLayer],
    base_color: Option<&str>,
    skin: Option<&Arc<str>>,
    node_transform: &[lodestone_assets::ItemNodeTransform],
    transform: &DisplayTransform,
    x: f32,
    y: f32,
    size: f32,
) {
    let outer = gui_item_pose([x, y, size, size], transform);
    let placement = lodestone_render::compose_special_item_transform(outer, kind, node_transform);

    // The banner rig is two meshes sharing one placement — see
    // `lodestone_render::banner_item_rig`'s doc — so it is pushed as two
    // (untinted) mesh draws, plus one translucent `BannerLayer` entry per
    // resolved pattern layer (base colour first, then every loom pattern in
    // order), rather than through the single-mesh path every other kind uses
    // below.
    if kind == "minecraft:banner"
        && let Some(rig) = lodestone_render::banner_item_rig(item.path())
        && let Some(base) = lodestone_render::banner_item_base_color(item.path())
    {
        out.push(SpecialIconDraw::Mesh {
            model: rig.body.0,
            texture: lodestone_render::BlockEntityTexture::Static(rig.body.1),
            placement,
            tint: [255, 255, 255],
        });
        out.push(SpecialIconDraw::Mesh {
            model: rig.flag.0,
            texture: lodestone_render::BlockEntityTexture::Static(rig.flag.1),
            placement,
            tint: [255, 255, 255],
        });
        let stored: Vec<lodestone_render::StoredPatternLayer> = patterns
            .iter()
            .filter_map(|layer| {
                Some(lodestone_render::StoredPatternLayer {
                    pattern_asset_id: layer.pattern_asset_id.clone(),
                    color: lodestone_render::DyeColor::from_name(&layer.color)?,
                })
            })
            .collect();
        for layer in lodestone_render::banner_pattern_layers(base, &stored) {
            let Some(pattern) = layer.sprite.path().rsplit('/').next() else {
                continue;
            };
            out.push(SpecialIconDraw::BannerLayer {
                family: PatternFamily::Banner,
                pattern: pattern.to_string(),
                placement,
                color: lodestone_render::gamma_rgb_to_bytes(layer.color),
            });
        }
        return;
    }

    // The shield rig is one mesh (plate+handle together) pushed once,
    // opaque, then re-submitted per pattern layer through the identical
    // translucent pass — `ShieldSpecialRenderer.submit` ported. See
    // `gpu/first_person.rs`'s `prepare_special_hand` shield branch, which
    // this mirrors for the GUI-icon surface.
    if kind == "minecraft:shield" {
        let has_patterns = lodestone_render::shield_has_patterns(base_color, patterns.len());
        let rig = lodestone_render::shield_item_rig(has_patterns);
        out.push(SpecialIconDraw::Mesh {
            model: rig.0,
            texture: lodestone_render::BlockEntityTexture::Static(rig.1),
            placement,
            tint: [255, 255, 255],
        });
        if has_patterns {
            let stored: Vec<lodestone_render::StoredPatternLayer> = patterns
                .iter()
                .filter_map(|layer| {
                    Some(lodestone_render::StoredPatternLayer {
                        pattern_asset_id: layer.pattern_asset_id.clone(),
                        color: lodestone_render::DyeColor::from_name(&layer.color)?,
                    })
                })
                .collect();
            let base_dye = base_color
                .and_then(lodestone_render::DyeColor::from_name)
                .unwrap_or(lodestone_render::DyeColor::White);
            for layer in lodestone_render::shield_pattern_layers(base_dye, &stored) {
                let Some(pattern) = layer.sprite.path().rsplit('/').next() else {
                    continue;
                };
                out.push(SpecialIconDraw::BannerLayer {
                    family: PatternFamily::Shield,
                    pattern: pattern.to_string(),
                    placement,
                    color: lodestone_render::gamma_rgb_to_bytes(layer.color),
                });
            }
        }
        return;
    }

    let Some((model, texture)) = special_icon_geometry(kind, item) else {
        return;
    };
    // A custom head's own sheet, replacing the default skull stem
    // `special_item_rig` resolves. Exactly the substitution the placed-head pass
    // makes on `SkullSpawn::texture`, and for the same reason: the rig is right,
    // only the sheet is per-stack. `skin` is `None` for every other kind and for
    // a plain head, which leaves the resolved stem untouched.
    let texture = match skin {
        Some(url) => lodestone_render::BlockEntityTexture::PlayerSkin(Arc::clone(url)),
        None => lodestone_render::BlockEntityTexture::Static(texture),
    };
    out.push(SpecialIconDraw::Mesh {
        model,
        texture,
        placement,
        tint: [255, 255, 255],
    });
}

/// The vertex multiplier for one sprite layer's tint, **converted into the space
/// `hud_sprite.wgsl` actually multiplies in** rather than forwarded raw.
///
/// # Why a conversion is needed at all
///
/// Vanilla is not colour-managed: it multiplies the tint into the sampled texel
/// as raw bytes, so its answer is `texel_byte * tint_byte / 255` — a
/// **gamma-space** multiply. We cannot reproduce that by passing the same bytes,
/// because the item atlas is `Rgba8UnormSrgb` ([`GpuAtlas`]) and so is the
/// swapchain, so the hardware decodes the texel to **linear** before
/// `hud_sprite.wgsl`'s single `textureSample(...) * in.tint` and re-encodes on
/// write. The multiply lands in linear light whatever we hand it.
///
/// That the round trip is real, and not something to hedge against, is settled by
/// an observable rather than an assumption: with an sRGB atlas and a *non*-sRGB
/// target, an untinted `[1.0; 4]` sprite would store `srgb_to_linear(texel)`,
/// taking a mid-grey 128 down to 55 — every HUD sprite in the game would be
/// visibly dark. They are not.
///
/// Forwarding `tint_byte / 255` therefore scales in the wrong space and pulls
/// every factor toward 1.0 — the wash-out that fix warns about, and the same
/// gamma-vs-linear trap that fix measured arriving from the data side. Under sRGB's
/// pure-power model the correction is exact and, usefully, **texel-independent**,
/// so it belongs here on the tint and not in the shader:
///
/// ```text
/// want:  linear_to_srgb(L_out) = texel * t   =>  L_out = (texel * t)^2.2
/// have:  L_out = srgb_to_linear(texel) * m   =        texel^2.2 * m
///   =>   m = t^2.2 = srgb_to_linear(t)          (texel cancels)
/// ```
///
/// # What is live, and what still is not
///
/// `slot.dyed_color`/`slot.potion_color` feed [`ItemTintContext::components`]
/// with a `lodestone_model::item::ItemComponents` built just for this call —
/// only those two fields are non-default, because `dye` and `potion` are the
/// only two tint sources any producer of an [`ItemIcon`] can supply today (see
/// each field's own doc for why a `None` there is honest rather than a stub).
/// `map_color`, `firework_explosion` and `custom_model_data` still resolve to
/// their definition's `default` regardless, because [`ItemIcon`] carries no
/// component for any of them — that is
/// [`TintProvenance::Unmodeled`](lodestone_assets::item_tint::TintProvenance::Unmodeled),
/// not a wiring gap this function could close. `grass_colormap` is left `None`
/// for the same reason it always was: nothing downstream of this call threads a
/// pack colormap through, so `grass` resolves to `None` (no colormap) exactly as
/// [`lodestone_assets::item_tint::resolve`] documents.
fn sprite_layer_tint(layer: &SpriteLayer, slot: &ItemIcon) -> [f32; 4] {
    let Some(source) = layer.tint.as_ref() else {
        return [1.0, 1.0, 1.0, 1.0];
    };
    // Only `dyed_color`/`potion_color` are populated — the two live sources;
    // every other field takes its `Default`, which is exactly the "component
    // absent" state `resolve` expects for a source this build cannot supply.
    let components = lodestone_model::item::ItemComponents {
        dyed_color: slot.dyed_color,
        potion_color: slot.potion_color,
        ..Default::default()
    };
    let ctx = ItemTintContext {
        components: Some(&components),
        grass_colormap: None,
    };
    let Some(resolved) = lodestone_assets::item_tint::resolve(source, &ctx) else {
        return [1.0, 1.0, 1.0, 1.0];
    };
    // `rgb()` and not `argb`: item tints are opaque multipliers in every vanilla
    // case, and that method's doc names this as the gamma-space-multiplier form.
    let rgb = resolved.rgb();
    let channel = |shift: u32| {
        lodestone_render::fog::srgb_to_linear_f32(f32::from(((rgb >> shift) & 0xFF) as u8) / 255.0)
    };
    [channel(16), channel(8), channel(0), 1.0]
}

/// Push one textured quad (two triangles) from an absolute-pixel destination
/// rect and its atlas UVs, tinted by `c`, into a `SPRITE_FLOATS_PER_VERTEX`
/// stream. Shared by the item atlas and the GUI atlas: the vertex layout is the
/// same, only the bound texture differs.
pub(crate) fn push_sprite_quad(
    verts: &mut Vec<f32>,
    vw: f32,
    vh: f32,
    q: GuiSpriteQuad,
    c: [f32; 4],
) {
    let to_ndc = |px: f32, py: f32| (2.0 * px / vw - 1.0, 1.0 - 2.0 * py / vh);
    let [dx, dy, dw, dh] = q.dst;
    let (x0, y0) = to_ndc(dx, dy);
    let (x1, y1) = to_ndc(dx + dw, dy + dh);
    let [u0, v0] = q.uv_min;
    let [u1, v1] = q.uv_max;
    let mut v = |vx: f32, vy: f32, tu: f32, tv: f32| {
        verts.extend_from_slice(&[vx, vy, tu, tv, c[0], c[1], c[2], c[3]]);
    };
    v(x0, y0, u0, v0);
    v(x1, y0, u1, v0);
    v(x1, y1, u1, v1);
    v(x0, y0, u0, v0);
    v(x1, y1, u1, v1);
    v(x0, y1, u0, v1);
}

/// Push the glint copy of one item-sprite quad into [`IconSink::glint`].
///
/// Same rect, same atlas UVs, white tint — `hud_glint.wgsl` samples the item
/// atlas at those UVs for its silhouette mask and ignores the tint entirely, so
/// the colour is only there to fill the shared vertex layout. Kept as its own
/// function rather than a `push_sprite_quad(.., WHITE)` call at the two sites so
/// the "why is this quad duplicated" answer is where the duplication is.
fn push_glint_quad(verts: &mut Vec<f32>, vw: f32, vh: f32, q: GuiSpriteQuad) {
    push_sprite_quad(verts, vw, vh, q, [1.0, 1.0, 1.0, 1.0]);
}

/// A borrowed handle onto a screen's **colour** vertex stream plus the viewport
/// it is measured against — the three values every pixel-space primitive needs.
/// Bundled so `rect`/`text`/`glyph` take their own arguments and not the
/// stream's, and so the two screens' `Builder`s share one implementation of each.
pub(crate) struct ColourStream<'a> {
    /// Flat `[x, y, r, g, b, a]` per vertex, positions in NDC.
    pub verts: &'a mut Vec<f32>,
    /// Viewport width in pixels.
    pub w: f32,
    /// Viewport height in pixels.
    pub h: f32,
}

impl ColourStream<'_> {
    /// Emit a pixel-space rectangle as two triangles in NDC.
    pub(crate) fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        debug_assert_eq!(FLOATS_PER_VERTEX, 6);
        let to_ndc = |px: f32, py: f32| (2.0 * px / self.w - 1.0, 1.0 - 2.0 * py / self.h);
        let (x0, y0) = to_ndc(x, y);
        let (x1, y1) = to_ndc(x + w, y + h);
        let verts = &mut *self.verts;
        let mut v = |vx: f32, vy: f32| {
            verts.extend_from_slice(&[vx, vy, c[0], c[1], c[2], c[3]]);
        };
        v(x0, y0);
        v(x1, y0);
        v(x1, y1);
        v(x0, y0);
        v(x1, y1);
        v(x0, y1);
    }

    /// Emit a pixel-space rectangle as two triangles in NDC, with a vertical
    /// gradient from `top` (the rect's own top edge) to `bottom` (its bottom
    /// edge). The GPU interpolates per-vertex colour across each triangle, so
    /// this needs no second pipeline — just two colours instead of one at emit
    /// time. Used for vanilla's translucent dim behind an open container screen
    /// (`AbstractContainerScreen::extractTransparentBackground`, a
    /// `fillGradient`, not a flat fill — see `container.rs`'s own doc comment).
    pub(crate) fn gradient_rect(
        &mut self,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        top: [f32; 4],
        bottom: [f32; 4],
    ) {
        debug_assert_eq!(FLOATS_PER_VERTEX, 6);
        let to_ndc = |px: f32, py: f32| (2.0 * px / self.w - 1.0, 1.0 - 2.0 * py / self.h);
        let (x0, y0) = to_ndc(x, y);
        let (x1, y1) = to_ndc(x + w, y + h);
        let verts = &mut *self.verts;
        let mut v = |vx: f32, vy: f32, c: [f32; 4]| {
            verts.extend_from_slice(&[vx, vy, c[0], c[1], c[2], c[3]]);
        };
        v(x0, y0, top);
        v(x1, y0, top);
        v(x1, y1, bottom);
        v(x0, y0, top);
        v(x1, y1, bottom);
        v(x0, y1, bottom);
    }

    /// Emit one flat-shaded pixel-space triangle in NDC — the primitive the F3
    /// profiler pie chart's wedges are built from (`hud::draw_profiler_chart`).
    /// Deliberately the general case rather than a pie-specific "wedge"
    /// method: a triangle fan is a sequence of these, and nothing else here
    /// needs a dedicated non-rectangular shape yet.
    pub(crate) fn triangle(&mut self, p0: (f32, f32), p1: (f32, f32), p2: (f32, f32), c: [f32; 4]) {
        debug_assert_eq!(FLOATS_PER_VERTEX, 6);
        let to_ndc = |px: f32, py: f32| (2.0 * px / self.w - 1.0, 1.0 - 2.0 * py / self.h);
        let verts = &mut *self.verts;
        let mut v = |p: (f32, f32)| {
            let (x, y) = to_ndc(p.0, p.1);
            verts.extend_from_slice(&[x, y, c[0], c[1], c[2], c[3]]);
        };
        v(p0);
        v(p1);
        v(p2);
    }

    /// Draw a single glyph with its top-left at `(x, y)`. Space and unknown
    /// handling match [`font::glyph_rows`]; blanks emit no quads.
    pub(crate) fn glyph(&mut self, ch: char, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        if ch == ' ' {
            return;
        }
        let rows = font::glyph_rows(ch);
        for (ry, row) in rows.iter().enumerate() {
            for rx in 0..font::GLYPH_W {
                let bit = (row >> (font::GLYPH_W - 1 - rx)) & 1;
                if bit == 1 {
                    self.rect(
                        x + rx as f32 * scale,
                        y + ry as f32 * scale,
                        scale,
                        scale,
                        c,
                    );
                }
            }
        }
    }

    /// Emit a string starting at pixel `(x, y)` (top-left of the first glyph).
    ///
    /// A legacy `§`+code pair is **consumed and drawn as nothing**, matching what
    /// `VanillaFont::draw` does with the same string. This font has no styled
    /// glyph variants and no per-run colour, so the code's *effect* is dropped —
    /// but the two characters must not become two glyphs, or a jar-less run shows
    /// `§7` where the real font shows grey text. Skipping is the whole of the
    /// difference between "colour unavailable" and "codes on screen".
    pub(crate) fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        let advance = (font::GLYPH_W as f32 + 1.0) * scale;
        let mut cursor = x;
        let mut chars = s.chars();
        while let Some(ch) = chars.next() {
            if ch == lodestone_model::text::LEGACY_PREFIX {
                // Both characters, or just the dangling `§` — vanilla's
                // `iterateFormatted` emits neither.
                if chars.next().is_none() {
                    break;
                }
                continue;
            }
            self.glyph(ch, cursor, y, scale, c);
            cursor += advance;
        }
    }

    /// The jar-less sibling of [`text`](Self::text) for a styled
    /// [`TextSpan`](lodestone_model::text::TextSpan) list: same fixed-advance
    /// 5×7 glyphs, but each character coloured by its own span's
    /// [`TextColor`](lodestone_model::text::TextColor) — falling back to
    /// `base` when a span carries none — instead of one flat colour for the
    /// whole string.
    ///
    /// Without this, a jar-less caller holding spans would have to flatten
    /// them to a `§`-coded string first to reuse `text`, which is exactly the
    /// `Text::to_legacy_string`-style loss this whole change exists to avoid:
    /// a [`TextColor::Rgb`](lodestone_model::text::TextColor::Rgb) has no
    /// legacy code and no glyph-level colour to fall back to either, so it
    /// would render in `base` even on a jar-less run. Spans carry the colour
    /// straight through instead. Mirrors
    /// `hud::Builder::text_spans`'s own jar-less fallback; see
    /// `container::builder::Builder::shadowed_label_spans`, its caller.
    pub(crate) fn spans(
        &mut self,
        spans: &[lodestone_model::text::TextSpan],
        x: f32,
        y: f32,
        scale: f32,
        base: [f32; 3],
        alpha: f32,
    ) {
        let advance = (font::GLYPH_W as f32 + 1.0) * scale;
        let mut cursor = x;
        for span in spans {
            let rgb = span.style.color.map_or(base, vanilla_font::text_color_rgb);
            for ch in span.text.chars() {
                self.glyph(ch, cursor, y, scale, [rgb[0], rgb[1], rgb[2], alpha]);
                cursor += advance;
            }
        }
    }
}

/// Width in pixels of `s` at `scale` in the shell's fixed-width bitmap font,
/// legacy `§`+code pairs counted as zero-width to match [`ColourStream::text`].
/// Only correct when no vanilla font is attached — every layout site goes
/// through `Builder::text_width`, which picks this or the proportional
/// measure to match whichever font `Builder::text` will actually draw with.
pub(crate) fn text_w(s: &str, scale: f32) -> f32 {
    let mut visible = 0usize;
    let mut chars = s.chars();
    while let Some(ch) = chars.next() {
        if ch == lodestone_model::text::LEGACY_PREFIX {
            if chars.next().is_none() {
                break;
            }
            continue;
        }
        visible += 1;
    }
    visible as f32 * (font::GLYPH_W as f32 + 1.0) * scale
}

/// The colour attachment every GUI overlay pass uses: load what was already
/// drawn, store what we add. A free function rather than a closure because each
/// pass needs the borrow of `view` to end with its own `RenderPassDescriptor`.
pub(crate) fn load_colour_attachment(
    view: &wgpu::TextureView,
) -> wgpu::RenderPassColorAttachment<'_> {
    wgpu::RenderPassColorAttachment {
        view,
        depth_slice: None,
        resolve_target: None,
        ops: wgpu::Operations {
            load: wgpu::LoadOp::Load,
            store: wgpu::StoreOp::Store,
        },
    }
}

/// The GPU resources for drawing item icons from the flat [`ItemAtlas`]: the
/// uploaded item-sprite atlas, its textured pipeline + bind group, and a dynamic
/// vertex buffer.
#[derive(Debug)]
struct SpriteIcons {
    atlas: Arc<ItemAtlas>,
    #[allow(dead_code)]
    gpu: GpuAtlas,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
}

/// The GPU resources for drawing **3-D block items** in GUI slots.
///
/// Unlike [`SpriteIcons`] this owns no texture: it reuses the world's
/// [`ModelPipeline`] and binds the *same* block atlas, tint palette and
/// animation slots the terrain pass does. Four bind groups is exactly what the
/// model shader declares — camera (0), atlas (1), palette (2), animation (3) —
/// and `wgpu`'s portable `max_bind_groups` floor is **4**, so there is no room
/// for a fifth. That is why the GUI camera and its (disabled) fog share group 0
/// binding 0, with the (always-zero) origin at binding 1 — two *bindings*, not
/// a fifth *group* — and why nothing here introduces a new group: a
/// five-group variant validates on an adapter that reports 8 and fails on the
/// floor, which is a bug no local screenshot can find.
///
/// The three sharings are each load-bearing rather than merely tidy:
///
/// * **atlas** — a block item's faces *are* block textures; a second upload would
///   cost tens of megabytes to draw a handful of 16 px icons;
/// * **palette** — a grass block's slot icon and the world block resolve to the
///   same slot, so they cannot drift to different greens;
/// * **animation slots** — magma, sea lantern and prismarine icons advance in
///   lock-step with the world, for free, off the one per-frame uniform write.
#[derive(Debug)]
struct ModelIcons {
    pipeline: ModelPipeline,
    /// Group 0 binding 0: the GUI orthographic `view_proj` with fog disabled.
    /// Rewritten each frame because it depends on the target size.
    camera_buffer: wgpu::Buffer,
    /// Group 0 binding 1: a permanent zero section origin — icon geometry is
    /// placed by its own vertex positions, not a per-section offset, so this
    /// is never rewritten after construction. Never read back either: kept
    /// alive purely so the buffer [`Self::camera_bind_group`] references
    /// outlives it (`wgpu` resources are `Arc`-backed, so this would be safe
    /// to drop right after building the bind group, but keeping the handle is
    /// clearer than relying on that).
    #[allow(dead_code)]
    origin_buffer: wgpu::Buffer,
    camera_bind_group: wgpu::BindGroup,
    /// Group 1: the shared block atlas (view + sampler borrowed at attach time;
    /// the bind group holds its own strong reference).
    atlas_bind_group: wgpu::BindGroup,
    /// Group 2: the shared tint palette.
    palette_bind_group: wgpu::BindGroup,
    /// Group 3: the shared per-slot animation uniforms.
    anim_bind_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    capacity_bytes: usize,
}

/// The GPU resources for drawing the **special-renderer** family (today: chests)
/// in GUI slots — the ex-`builtin/entity` items that have no item model at all.
///
/// # Why this is `EntityPipeline` and not [`ModelIcons`]
///
/// See the module docs: the geometry transfers unchanged but the *texture* does
/// not, and [`ModelIcons`] has no bind group left to spend. `EntityPipeline`
/// spends two of four, takes the same [`ModelVertex`] layout, and is
/// `depth_write`/`Less` against the depth attachment
/// [`IconRenderer::draw_models`] clears to `1.0` — a GUI icon's clip depth lands
/// near `0.5`, so it passes. It is also `cull_mode: None`, which is why the GUI
/// pose's **negative** determinant is a non-issue here: unlike the model stream,
/// nothing depends on the winding surviving the `y` flip.
///
/// # Everything is owned, not borrowed
///
/// The opposite of [`ModelIcons`], and for the opposite reason. There is nothing
/// to share: the block atlas, tint palette and animation slots are all irrelevant
/// to an entity sheet, and the world's own block-entity pass
/// (`gpu/block_entities.rs`) keeps its bind groups against its own
/// `EntityPipeline` instance's layouts. Re-decoding the 22 chest PNGs here costs
/// ~360 KB of 64×64 textures once, which is not worth reaching across the `gpu`
/// module boundary for — and `entity_texture_from_image` is `pub(super)` to `gpu`
/// anyway.
#[derive(Debug)]
struct SpecialIcons {
    pipeline: EntityPipeline,
    /// The baked block-entity corpus, for `part_transforms`. CPU-side; the
    /// uploaded counterpart is [`Self::gpu_models`].
    models: BlockEntityModelSet,
    gpu_models: HashMap<&'static str, GpuEntityModel>,
    /// Keyed by **texture stem**, not model name: a trapped chest shares the
    /// single-chest mesh and differs only here. Keying by model draws every
    /// trapped chest in plain oak — the same trap `gpu/block_entities.rs`
    /// documents.
    textures: HashMap<&'static str, wgpu::BindGroup>,
    /// Fetched **player skins**, one bind group per texture URL — the GUI-icon
    /// sibling of `super::super::gpu::entities::EntityRenderer::player_skins`,
    /// and the reason a custom head in a slot can now draw its own face.
    ///
    /// Separate from [`Self::textures`] and keyed by `String` for that map's
    /// reason inverted: a fetched skin's identity arrives on the wire, so it
    /// cannot be a `&'static str` without leaking one string per distinct skin
    /// per session. It is also the only map here that grows *after* the pass is
    /// built — filled by [`Self::install_ready_player_skins`] on whichever frame
    /// `crate::remote_skins` finishes the fetch.
    ///
    /// A miss falls back to the default skull sheet **and logs**: a plain head is
    /// a head, so the wrong face is invisible at the draw site and the only
    /// evidence is a log line.
    player_skins: HashMap<String, wgpu::BindGroup>,
    /// Kept so [`Self::install_ready_player_skins`] can build a bind group after
    /// bring-up. Every sheet this pass binds — packaged or fetched — must sample
    /// through the same nearest-filter sampler, or a fetched skin would be the
    /// one blurry icon on the screen.
    sampler: wgpu::Sampler,
    /// How many consecutive frames a head has wanted a skin this pass could not
    /// bind. Drives the same one-line-per-episode reporting
    /// [`IconRenderer::special_declines`] does, for the same reason: this runs
    /// inside the frame loop.
    skin_declines: u32,
    /// Group 0: `gui_ortho` with fog disabled. Rewritten each frame because the
    /// projection depends on the target size.
    cam_buffer: wgpu::Buffer,
    cam_bind_group: wgpu::BindGroup,
    /// This frame's batches, rebuilt by [`IconRenderer::upload`] and consumed by
    /// [`IconRenderer::draw_models`]. Held rather than returned because
    /// `draw_models`' signature is shared by two screens and a per-part instance
    /// buffer list does not fit its `count: u32`.
    batches: Vec<SpecialBatch>,
    /// The carried stack's batches, kept separate from [`Self::batches`] so the
    /// container screen can draw them in a later stratum — see [`IconStratum`].
    /// A grouped batch cannot straddle the two, because the group is what a
    /// single draw call binds.
    carried_batches: Vec<SpecialBatch>,
    /// The banner **pattern-layer** pass — the GUI-icon sibling of
    /// `super::super::gpu::block_entities::BlockEntityRenderer
    /// ::banner_layer_pipeline`: alpha-blended, depth-write off, and
    /// `fs_main_no_cutout` so a mask's soft edges blend instead of being
    /// discarded.
    banner_layer_pipeline: wgpu::RenderPipeline,
    /// One texture bind group per banner **mask**, keyed by the bare pattern
    /// asset id — the GUI-icon sibling of
    /// `super::super::gpu::block_entities::BlockEntityRenderer
    /// ::banner_patterns`. Empty without a vanilla pack, matching that fail-
    /// open exactly.
    banner_patterns: HashMap<String, wgpu::BindGroup>,
    /// The shield sibling of [`Self::banner_patterns`] — see
    /// `gpu/block_entities.rs`'s `BlockEntityRenderer::shield_patterns` for
    /// why this is a separate map, not a rename of the banner one.
    shield_patterns: HashMap<String, wgpu::BindGroup>,
    /// This frame's uploaded pattern-layer draws, ordered — the slot-layer
    /// sibling of [`Self::batches`].
    banner_layers: Vec<SpecialBannerLayerBatch>,
    /// The carried stack's pattern-layer draws — the sibling of
    /// [`Self::carried_batches`].
    carried_banner_layers: Vec<SpecialBannerLayerBatch>,
}

/// One uploaded special-icon batch: the mesh and sheet to bind, one instance
/// buffer per part, and how many icons share them.
#[derive(Debug)]
struct SpecialBatch {
    model: &'static str,
    texture: lodestone_render::BlockEntityTexture,
    count: u32,
    parts: Vec<Option<wgpu::Buffer>>,
}

/// One banner pattern-mask layer, uploaded and ready to draw — the GUI-icon
/// sibling of `super::super::gpu::block_entities::BannerLayerDrawBatch`.
///
/// **Strictly ordered, and never batched across icons**: each entry is one
/// draw of the *same* flag geometry with its own mask and its own colour, and
/// two banners in adjacent slots reusing the same pattern in opposite orders
/// could not both be right if these were coalesced.
#[derive(Debug)]
struct SpecialBannerLayerBatch {
    /// Which mesh/mask-map this layer resolves against — see
    /// [`PatternFamily`]'s doc.
    family: PatternFamily,
    /// Bare pattern asset id, keying [`SpecialIcons::banner_patterns`] or
    /// `::shield_patterns`, per [`Self::family`].
    pattern: String,
    /// A one-instance buffer carrying the mesh's own GUI-pixel-space
    /// placement and this layer's gamma-space colour as the instance tint.
    instances: wgpu::Buffer,
}

impl SpecialIcons {
    /// Bake the corpus, upload it, and build one bind group per available sheet.
    ///
    /// Returns `None` when **no** sheet loaded, which is the jar-less case: with
    /// no textures there is nothing this pass could ever draw, and reporting that
    /// as "not attached" keeps [`IconRenderer`]'s negative control meaningful
    /// rather than leaving an attached pass that silently skips every batch.
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
    ) -> Option<Self> {
        let pipeline = EntityPipeline::new(device, color_format);
        let models = BlockEntityModelSet::load();

        let sampler = device.create_sampler(&wgpu::SamplerDescriptor {
            label: Some("lodestone-special-icon-sampler"),
            mag_filter: wgpu::FilterMode::Nearest,
            min_filter: wgpu::FilterMode::Nearest,
            ..Default::default()
        });

        let mut gpu_models = HashMap::new();
        for (name, mesh) in models.iter() {
            if let Some(gpu) = GpuEntityModel::upload_parts(
                device,
                &mesh.vertices,
                &mesh.indices,
                mesh.parts.clone(),
            ) {
                gpu_models.insert(name, gpu);
            }
        }

        // `crate::resources::load_block_entity_textures()` already decodes every
        // stem `block_entity_texture_stems()` names (skulls, bells, banners,
        // shulker boxes, books, decorated pots — not just chests), but this pass
        // used to build a bind group for only the `chest_texture_stems()` subset
        // of what it had just loaded. `special_item_rig` resolves a player head
        // to `skull_texture_stem(SkullType::Player)` fine — the geometry and the
        // `IconPart::Special` slot both land — and `build_special_batches` then
        // silently dropped the draw at its `!s.textures.contains_key(draw.texture)`
        // guard, because no skull sheet had ever reached this map. That is the
        // whole bug: a player head (and every other non-chest special icon) was
        // resolved, matched a real rig and sheet, and still drew zero pixels in
        // an inventory slot, while the exact same rig drew correctly in the hand
        // (`RenderState::prepare_special_hand` binds through the world's
        // `BlockEntityRenderer`, which has always loaded the full stem list).
        let real = crate::resources::load_block_entity_textures();
        let mut textures = HashMap::new();
        for stem in block_entity_texture_stems() {
            let Some(img) = real.get(stem) else {
                // The loader already warned. A special icon with no sheet draws
                // nothing rather than a magenta box, matching the world pass.
                continue;
            };
            let view = entity_sheet_texture(device, queue, img);
            textures.insert(stem, pipeline.texture_bind_group(device, &view, &sampler));
        }
        if textures.is_empty() {
            // Never a bare `None`. This is the exact point at which the whole
            // special-icon stream goes dark, and returning `None` here reads at
            // the call site as "nothing to draw" rather than "could not look" —
            // the two must never share a value. Name which of the two it was:
            // an empty `real` means the pack stack itself did not open (or held
            // none of these sheets), a non-empty one means the stems and the
            // pack disagree, which is a different bug entirely.
            tracing::warn!(
                target: "assets",
                decoded_by_loader = real.len(),
                stems_wanted = block_entity_texture_stems().len(),
                models_baked = gpu_models.len(),
                "the GUI special-renderer icon pass could not build: no block-entity sheet \
                 was decoded, so every chest, shulker box, banner, shield and skull icon in \
                 a slot will be blank"
            );
            return None;
        }
        tracing::debug!(
            target: "assets",
            sheets = textures.len(),
            models = gpu_models.len(),
            "built the GUI special-renderer icon pass"
        );

        // `FogUniform::disabled()` leaves the sky-darken lane at its `0.0`
        // sentinel, which `EntityCameraUniform::sky_darken` reads back as `1.0`
        // (full daylight). An inventory slot is not in the world: it must not
        // dim at night, and taking the lane literally would render every chest
        // icon at the 0.2 floor.
        let cam_buffer = entity_camera_buffer(
            device,
            EntityCameraUniform {
                camera: CameraUniform {
                    view_proj: Mat4::IDENTITY.to_cols_array_2d(),
                    section_origin: [0.0, 0.0, 0.0, 0.0],
                },
                fog: FogUniform::disabled(),
            },
        );
        let cam_bind_group = pipeline.camera_bind_group(device, &cam_buffer);

        // The banner masks — the GUI-icon sibling of
        // `gpu/block_entities.rs`'s identical load. `base` is included: it is
        // layer 0 of every banner's mask list, tinted by the stack's own base
        // colour.
        let mut banner_patterns = HashMap::new();
        if let Some(manager) = crate::resources::vanilla_manager() {
            match lodestone_assets::banner_pattern_atlas::BannerPatternAtlas::load(&manager) {
                Ok(masks) => {
                    let ids: Vec<String> = masks.pattern_ids().map(str::to_string).collect();
                    for id in ids {
                        if let Some(img) = masks.get(&id) {
                            let view = entity_sheet_texture(device, queue, img);
                            banner_patterns
                                .insert(id, pipeline.texture_bind_group(device, &view, &sampler));
                        }
                    }
                }
                Err(e) => tracing::warn!(target: "assets", "load banner patterns (icon pass): {e}"),
            }
        }

        // The shield masks — the identical shape as the banner masks just
        // above, over `ShieldPatternAtlas`'s own `entity/shield/` tree.
        let mut shield_patterns = HashMap::new();
        if let Some(manager) = crate::resources::vanilla_manager() {
            match lodestone_assets::banner_pattern_atlas::ShieldPatternAtlas::load(&manager) {
                Ok(masks) => {
                    let ids: Vec<String> = masks.pattern_ids().map(str::to_string).collect();
                    for id in ids {
                        if let Some(img) = masks.get(&id) {
                            let view = entity_sheet_texture(device, queue, img);
                            shield_patterns
                                .insert(id, pipeline.texture_bind_group(device, &view, &sampler));
                        }
                    }
                }
                Err(e) => tracing::warn!(target: "assets", "load shield patterns (icon pass): {e}"),
            }
        }
        let banner_layer_pipeline = pipeline.banner_layer_pipeline(device, color_format);

        Some(SpecialIcons {
            pipeline,
            models,
            gpu_models,
            textures,
            // Nothing until a head with a profile is drawn *and* its fetch
            // lands; see the field's own doc for why a miss is not a failure.
            player_skins: HashMap::new(),
            sampler,
            skin_declines: 0,
            cam_buffer,
            cam_bind_group,
            batches: Vec::new(),
            carried_batches: Vec::new(),
            banner_layer_pipeline,
            banner_patterns,
            shield_patterns,
            banner_layers: Vec::new(),
            carried_banner_layers: Vec::new(),
        })
    }

    /// How many sheets loaded — the counter that separates "no chest in a slot"
    /// from "no pack, so nothing can ever draw".
    fn sheet_count(&self) -> usize {
        self.textures.len()
    }

    /// Give every custom head in this frame's draw list a bind group, if
    /// `crate::remote_skins` has finished fetching its sheet.
    ///
    /// **Pull, not drain.** `remote_skins::drain_ready` is the world entity
    /// pass's one-shot queue and has exactly one consumer; draining it here
    /// would steal a placed head's sheet rather than share it. Asking
    /// `remote_skins::sheet` for a URL we have no bind group for is idempotent,
    /// costs one hash lookup per unresolved head per frame, and self-heals: a
    /// record built on the frame the fetch was *started* resolves on whichever
    /// later frame it lands, with no ordering to get right.
    ///
    /// Every head that still cannot be resolved is **reported**. The draw falls
    /// back to the default skull sheet — a plain head is a head, so nothing looks
    /// broken and nothing goes red — which is precisely why the decline may not
    /// be silent.
    fn install_ready_player_skins(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        special: &[SpecialIconDraw],
    ) {
        let mut unresolved = 0usize;
        let mut installed = 0usize;
        for draw in special {
            let SpecialIconDraw::Mesh { texture, .. } = draw else {
                continue;
            };
            let Some(url) = texture.player_skin_url() else {
                continue;
            };
            if self.player_skins.contains_key(url) {
                continue;
            }
            let Some(image) = crate::remote_skins::sheet(url) else {
                unresolved += 1;
                continue;
            };
            let view = entity_sheet_texture(device, queue, &image);
            let bind = self
                .pipeline
                .texture_bind_group(device, &view, &self.sampler);
            self.player_skins.insert(url.to_owned(), bind);
            installed += 1;
        }
        if installed > 0 {
            tracing::debug!(
                target: "assets",
                installed,
                skins = self.player_skins.len(),
                "bound custom head skins for the GUI icon pass"
            );
        }
        if unresolved == 0 {
            if self.skin_declines > 0 {
                tracing::info!(
                    target: "assets",
                    frames_default = self.skin_declines,
                    skins = self.player_skins.len(),
                    "every custom head in a slot now has its own skin bound"
                );
                self.skin_declines = 0;
            }
            return;
        }
        self.skin_declines = self.skin_declines.saturating_add(1);
        if self.skin_declines == 1 {
            // One line per episode, not per frame — the recovery line above
            // closes it, so an episode's length is readable from the pair.
            tracing::warn!(
                target: "assets",
                heads = unresolved,
                "custom head skins are drawing the DEFAULT skull sheet in a GUI slot: \
                 their texture is not fetched yet (normal for the first frames after one \
                 appears), the fetch failed, or its host is outside the texture allow list. \
                 The same heads draw correctly once crate::remote_skins finishes"
            );
        }
    }
}

/// Upload one entity sheet as an sRGB texture.
///
/// A near-copy of `gpu::entities::entity_texture_from_image`, which is
/// `pub(super)` to the `gpu` module and so not reachable from here. The one
/// load-bearing line is the format: **`Rgba8UnormSrgb`, not `Rgba8Unorm`.** A
/// vanilla PNG holds gamma-encoded bytes; binding it as plain `Unorm` hands the
/// shader `0.50` where the linear value is `0.21` and an sRGB target then encodes
/// it a second time — measured at +48% on every mob pixel when that pass got it
/// wrong, which is bright enough to look deliberate.
fn entity_sheet_texture(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    img: &lodestone_assets::Image,
) -> wgpu::TextureView {
    let size = wgpu::Extent3d {
        width: img.width,
        height: img.height,
        depth_or_array_layers: 1,
    };
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("lodestone-special-icon-sheet"),
        size,
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
        view_formats: &[],
    });
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: &texture,
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &img.rgba,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(img.width * 4),
            rows_per_image: Some(img.height),
        },
        size,
    );
    texture.create_view(&wgpu::TextureViewDescriptor::default())
}

/// The GPU objects behind a textured-quad sprite pass: an uploaded atlas, an
/// alpha-blended pipeline for `color_format`, its bind group, and a dynamic
/// vertex buffer sized for `capacity_floats` floats.
///
/// Returned by [`build_sprite_pipeline`] rather than assembled into a caller's
/// own struct, because the caller's struct also carries an `atlas: Arc<_>`
/// field of a type ([`GuiAtlas`](lodestone_render::GuiAtlas) vs [`ItemAtlas`])
/// that differs per call site and is not this function's concern.
pub(crate) struct SpritePipeline {
    pub(crate) gpu: GpuAtlas,
    pub(crate) pipeline: wgpu::RenderPipeline,
    pub(crate) bind_group: wgpu::BindGroup,
    pub(crate) buffer: wgpu::Buffer,
    pub(crate) capacity_floats: usize,
}

/// Build a textured-quad sprite pipeline over `atlas`: upload it, create the
/// bind-group layout, pipeline layout, render pipeline and bind group it
/// needs, and allocate a `capacity_floats`-float dynamic vertex buffer.
///
/// Shared by [`crate::hud::HudRenderer::attach_gui`],
/// [`crate::menu::render::MenuRenderer::attach_gui`] and
/// [`IconRenderer::attach_items`] below — the three GUI-sprite `attach_*`
/// paths that were, before this function existed, ~30 lines of hand-copied
/// `wgpu` descriptors apiece, differing only in shader source, target format,
/// buffer capacity and label.
///
/// **This is a code dedup, not a resource one.** Every call still builds its
/// own bind-group layout, pipeline and bind group from scratch — `wgpu` does
/// not deduplicate structurally-equal layouts (`docs/armour-rendering.md`),
/// and nothing before this function shared an instance across the three call
/// sites either, so that property carries over unchanged.
///
/// `label` is reused verbatim for every descriptor (shader, layout, pipeline,
/// bind group, buffer alike) rather than threading six distinct per-resource
/// labels through the signature. `attach_gui`'s two callers used to give each
/// object its own suffixed label (`"hud-sprite-bgl"`, `"hud-sprite-pipeline"`,
/// …); that granularity is debugger-only, invisible to every pixel this crate
/// draws, and already how [`IconRenderer::attach_items`]'s pre-existing
/// `label: &'static str` parameter worked for its own two callers.
pub(crate) fn build_sprite_pipeline(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    atlas: &Atlas,
    shader_wgsl: &str,
    color_format: wgpu::TextureFormat,
    capacity_floats: usize,
    label: &'static str,
) -> SpritePipeline {
    let gpu = GpuAtlas::from_atlas(device, queue, atlas);
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some(label),
        source: wgpu::ShaderSource::Wgsl(shader_wgsl.into()),
    });
    let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some(label),
        entries: &[
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Float { filterable: true },
                    view_dimension: wgpu::TextureViewDimension::D2,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
                count: None,
            },
        ],
    });
    let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
        label: Some(label),
        bind_group_layouts: &[Some(&bind_layout)],
        immediate_size: 0,
    });
    let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some(label),
        layout: Some(&layout),
        vertex: wgpu::VertexState {
            module: &shader,
            entry_point: Some("vs_main"),
            buffers: &[Some(wgpu::VertexBufferLayout {
                array_stride: (SPRITE_FLOATS_PER_VERTEX * 4) as wgpu::BufferAddress,
                step_mode: wgpu::VertexStepMode::Vertex,
                attributes: &[
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 0,
                        shader_location: 0,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x2,
                        offset: 8,
                        shader_location: 1,
                    },
                    wgpu::VertexAttribute {
                        format: wgpu::VertexFormat::Float32x4,
                        offset: 16,
                        shader_location: 2,
                    },
                ],
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        },
        fragment: Some(wgpu::FragmentState {
            module: &shader,
            entry_point: Some("fs_main"),
            targets: &[Some(wgpu::ColorTargetState {
                format: color_format,
                blend: Some(wgpu::BlendState::ALPHA_BLENDING),
                write_mask: wgpu::ColorWrites::ALL,
            })],
            compilation_options: wgpu::PipelineCompilationOptions::default(),
        }),
        primitive: wgpu::PrimitiveState {
            topology: wgpu::PrimitiveTopology::TriangleList,
            ..Default::default()
        },
        depth_stencil: None,
        multisample: wgpu::MultisampleState::default(),
        multiview_mask: None,
        cache: None,
    });
    let bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some(label),
        layout: &bind_layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(&gpu.view),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: wgpu::BindingResource::Sampler(&gpu.sampler),
            },
        ],
    });
    let buffer = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (capacity_floats * 4) as wgpu::BufferAddress,
        usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    SpritePipeline {
        gpu,
        pipeline,
        bind_group,
        buffer,
        capacity_floats,
    }
}

/// This frame's [`GuiGlintUniform`], at the player's **Glint Speed** and **Glint
/// Strength** (`Options.java`, two `UnitDouble`s defaulting to `0.5` and
/// `0.75`).
///
/// The clock is wall-clock milliseconds, vanilla's `Util.getMillis()` — the same
/// origin `gpu/glint.rs` keys the world and hand glint off, so all three shimmer
/// in phase. **Both options must be pushed to all three or they fall out of
/// phase**, which is why the world/hand pair reads
/// `crate::gpu::RenderState::glint_options` and this reads
/// [`IconRenderer::glint_speed`]/`glint_strength`: two owners, one pair of
/// values, pushed from the same place each frame.
///
/// Clamped through `lodestone_render::glint::clamp_speed`/`clamp_strength` — the
/// *same* pair `gpu::glint::glint_uniform` uses, deliberately, because two copies
/// of the domain is how the GUI icon and the held item would come to shimmer at
/// different rates.
fn gui_glint_uniform(speed: f64, strength: f32, atlas_px: [u32; 2]) -> GuiGlintUniform {
    // Same clock as `gpu::glint::glint_now_ms` — deliberately, so the GUI icon and
    // the held item shimmer at one rate — and `crate::platform::epoch_duration` for
    // the same reason it uses it: `SystemTime::now()` traps on wasm32.
    let millis = crate::platform::epoch_duration().as_secs_f64() * 1000.0;
    let speed = lodestone_render::glint::clamp_speed(speed);
    let strength = lodestone_render::glint::clamp_strength(strength);
    GuiGlintUniform {
        tex_matrix: lodestone_render::glint::glint_texture_matrix(
            millis,
            speed,
            // `Scale::Item` — the `glint` render type, which is every item form
            // including a GUI slot icon.
            lodestone_render::glint::Scale::Item,
            // The **item atlas**, not the model atlas the world and hand glints
            // sample: this pass transforms the icon quad's own item-atlas UV, and
            // the scale is atlas-relative.
            atlas_px,
        )
        .to_cols_array_2d(),
        fade: [strength, 0.0, 0.0, 0.0],
    }
}

impl GuiGlint {
    fn new(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        items: &GpuAtlas,
        img: &lodestone_assets::Image,
        label: &'static str,
    ) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(label),
            source: wgpu::ShaderSource::Wgsl(HUD_GLINT_WGSL.into()),
        });
        // `Rgba8Unorm`, **not** `_Srgb`, for the reason `gpu/glint.rs`'s module
        // doc gives at length: the GLINT blend squares the sampled byte in gamma
        // space, so the hardware must not decode it to linear on the way in.
        let size = wgpu::Extent3d {
            width: img.width,
            height: img.height,
            depth_or_array_layers: 1,
        };
        let texture = device.create_texture(&wgpu::TextureDescriptor {
            label: Some(label),
            size,
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::TEXTURE_BINDING | wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        queue.write_texture(
            wgpu::TexelCopyTextureInfo {
                texture: &texture,
                mip_level: 0,
                origin: wgpu::Origin3d::ZERO,
                aspect: wgpu::TextureAspect::All,
            },
            &img.rgba,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(img.width * 4),
                rows_per_image: Some(img.height),
            },
            size,
        );
        let glint_view = texture.create_view(&wgpu::TextureViewDescriptor::default());
        let glint_sampler = lodestone_render::glint::glint_sampler(device);

        let uniform_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &[wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Uniform,
                    has_dynamic_offset: false,
                    min_binding_size: None,
                },
                count: None,
            }],
        });
        let tex_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Texture {
                sample_type: wgpu::TextureSampleType::Float { filterable: true },
                view_dimension: wgpu::TextureViewDimension::D2,
                multisampled: false,
            },
            count: None,
        };
        let smp_entry = |binding: u32| wgpu::BindGroupLayoutEntry {
            binding,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Filtering),
            count: None,
        };
        let texture_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some(label),
            entries: &[tex_entry(0), smp_entry(1), tex_entry(2), smp_entry(3)],
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some(label),
            bind_group_layouts: &[Some(&uniform_layout), Some(&texture_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some(label),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                // `hud_sprite.wgsl`'s layout verbatim: the glint stream is built
                // by the same `push_sprite_quad`, and the tint at offset 16 is
                // declared here but not consumed by the shader.
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (SPRITE_FLOATS_PER_VERTEX * 4) as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 8,
                            shader_location: 1,
                        },
                    ],
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            },
            fragment: Some(wgpu::FragmentState {
                module: &shader,
                entry_point: Some("fs_main"),
                targets: &[Some(wgpu::ColorTargetState {
                    format: color_format,
                    // `BlendFunction.GLINT`: `dst += src * src`, destination alpha
                    // untouched. Not ADDITIVE and not TRANSLUCENT — both are the
                    // obvious guess and both were measured wrong.
                    blend: Some(lodestone_render::glint::glint_blend()),
                    write_mask: wgpu::ColorWrites::ALL,
                })],
                compilation_options: wgpu::PipelineCompilationOptions::default(),
            }),
            primitive: wgpu::PrimitiveState {
                topology: wgpu::PrimitiveTopology::TriangleList,
                ..Default::default()
            },
            // No depth: every GUI sprite pass here runs without a depth
            // attachment, which is the whole reason this pipeline exists rather
            // than `lodestone_render::glint`'s depth-`EQUAL` one.
            depth_stencil: None,
            multisample: wgpu::MultisampleState::default(),
            multiview_mask: None,
            cache: None,
        });
        let uniform = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: std::mem::size_of::<GuiGlintUniform>() as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let uniform_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &uniform_layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: uniform.as_entire_binding(),
            }],
        });
        let texture_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &texture_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&items.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(&items.sampler),
                },
                wgpu::BindGroupEntry {
                    binding: 2,
                    resource: wgpu::BindingResource::TextureView(&glint_view),
                },
                wgpu::BindGroupEntry {
                    binding: 3,
                    resource: wgpu::BindingResource::Sampler(&glint_sampler),
                },
            ],
        });
        let capacity_floats = 1024;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: (capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            texture,
            uniform,
            uniform_bind_group,
            texture_bind_group,
            buffer,
            capacity_floats,
            atlas_px: [items.width, items.height],
        }
    }
}

/// The GPU half of the item-icon pass, held by every screen that draws slots.
///
/// Both halves start detached. `attach_items` gives flat icons somewhere to
/// draw; `attach_item_models` gives block icons somewhere to draw. Neither is
/// required, and a renderer with neither draws no icons at all — the jar-less
/// runtime behaviour and the executed negative control in both pixel gates.
/// **`Default` is hand-written below, not derived** — the two glint fields must
/// boot at vanilla's `0.5`/`0.75` and a derived `0.0` would silently switch the
/// shimmer off.
#[derive(Debug)]
pub(crate) struct IconRenderer {
    sprites: Option<SpriteIcons>,
    models: Option<ModelIcons>,
    /// The block-entity icon pass (chests). Built **lazily**, on the first frame
    /// that actually contains a special icon — see [`Self::upload`].
    special: Option<SpecialIcons>,
    /// How many consecutive frames the lazy build has been *declined* — either
    /// because there was nowhere to draw (no model pass attached) or because
    /// [`SpecialIcons::new`] returned `None`. Reset to `0` the moment a build
    /// succeeds.
    ///
    /// # Why a counter and not the `special_tried` bool this replaces
    ///
    /// That bool latched. It was set on the *first* attempt and nothing ever
    /// cleared it, so a build that failed once left the whole special stream
    /// dark for the rest of the process — every chest, shulker box, banner,
    /// shield and player-head icon in every slot — with no error, no retry and
    /// nothing in the log. A permanently dark stream is now unrepresentable:
    /// the build is retried whenever `special.is_none()`, so recovery needs
    /// only the next frame on which the underlying reason has gone away.
    ///
    /// Retrying is affordable because the expensive case cannot recur: a run
    /// with no pack has no [`ItemAtlas`] either, so `draw_item_icon` never
    /// reaches an `IconPart::Special` and this function returns at its
    /// `special.is_empty()` guard long before any decode is attempted.
    ///
    /// The counter exists only to keep the log honest — one line per episode
    /// rather than one per frame — and to report how long an episode lasted
    /// when it ends.
    special_declines: u32,
    /// The colour format [`Self::attach_item_models`] was given, kept for the
    /// lazy build. `None` means the model pass was never attached, which is also
    /// the condition under which the special pass must stay dark: both are the
    /// same "there is somewhere to draw 3-D icons" signal, and both gates' negative
    /// control turns exactly on it.
    color_format: Option<wgpu::TextureFormat>,
    /// The 2-D GUI glint pass, attached by [`Self::attach_glint`]. `None` on a
    /// jar-less run or a pack with no glint sheet, in which case enchanted icons
    /// draw without their shimmer — the same "is a thing attached" degradation
    /// every other stream here has.
    glint: Option<GuiGlint>,
    /// Vanilla's **Glint Speed**/**Glint Strength** accessibility options, read by
    /// [`gui_glint_uniform`] once per frame in [`Self::upload_glint`].
    ///
    /// Held here rather than passed to `upload_glint` because that method's
    /// callers are the HUD and container renderers, and the uniform is per *frame*
    /// while their call is per *screen* — the same split
    /// [`Self::upload_glint`]'s own doc already records for the scroll offsets.
    ///
    /// Seeded to `lodestone_render::glint::DEFAULT_SPEED`/`DEFAULT_STRENGTH`,
    /// which are vanilla's shipped values, so an `IconRenderer` nobody pushes
    /// options into is byte-identical to what it was before these existed.
    glint_speed: f64,
    /// See [`Self::glint_speed`].
    glint_strength: f32,
}

/// The GPU resources for the **2-D GUI glint**: the shimmer over a flat item
/// icon in a slot.
///
/// A separate pipeline from `lodestone_render::glint`'s and it has to be. That
/// one consumes [`ModelVertex`] and depth-tests `EQUAL` against the pass beneath
/// it; a GUI sprite pass has an 8-float vertex and no depth attachment at all. So
/// the silhouette mask comes from re-sampling the *item atlas* instead — see
/// `shaders/hud_glint.wgsl`.
///
/// Two bind groups (uniform / the two textures), well under `wgpu`'s portable
/// `max_bind_groups` floor of 4, and its own uniform buffer rather than a share
/// of anyone else's: `queue.write_buffer` is ordered against the **submit**, not
/// the encoder, so a buffer shared with the world or hand glint would hand every
/// pass in the frame the last value written.
#[derive(Debug)]
struct GuiGlint {
    pipeline: wgpu::RenderPipeline,
    /// The uploaded glint sheet, kept alive explicitly — it is the subject here,
    /// not a side effect of the bind group's strong reference.
    #[allow(dead_code)]
    texture: wgpu::Texture,
    uniform: wgpu::Buffer,
    uniform_bind_group: wgpu::BindGroup,
    /// Group 1: the item atlas and the glint sheet, with a sampler each.
    texture_bind_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
    /// The **item atlas**'s dimensions, captured at build time.
    ///
    /// `hud_glint.wgsl` transforms the icon quad's item-atlas UV, and vanilla's
    /// glint scale is expressed in atlas-normalised units — so it means something
    /// different on a sheet of a different size, and this sheet is not the one the
    /// world and hand glints sample. See
    /// `lodestone_render::glint::atlas_correction`.
    atlas_px: [u32; 2],
}

/// `hud_glint.wgsl`'s group-0 uniform: the glint texture matrix plus
/// `GlintAlpha`.
#[repr(C)]
#[derive(Debug, Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct GuiGlintUniform {
    tex_matrix: [[f32; 4]; 4],
    /// `.x` is `GlintAlpha`; the rest is padding to the 16-byte alignment.
    fade: [f32; 4],
}

/// Not derived, and that is the whole reason this impl is hand-written: the two
/// glint fields must start at vanilla's shipped `0.5`/`0.75`, and
/// `#[derive(Default)]` would start them at `0.0` — a stationary, fully
/// transparent shimmer, i.e. the glint silently switched off on every screen for
/// any caller that never pushes the options. Every other field's `Default` is the
/// detached state the struct's own doc describes and is unchanged.
impl Default for IconRenderer {
    fn default() -> Self {
        Self {
            sprites: None,
            models: None,
            special: None,
            special_declines: 0,
            color_format: None,
            glint: None,
            glint_speed: lodestone_render::glint::DEFAULT_SPEED,
            glint_strength: lodestone_render::glint::DEFAULT_STRENGTH,
        }
    }
}

impl IconRenderer {
    /// A detached renderer: no atlas, no model pass, no icons.
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Push vanilla's **Glint Speed**/**Glint Strength** accessibility options
    /// down for the 2-D GUI glint, the third of the three glint sites (the world
    /// and hand pair live on `crate::gpu::RenderState`). Read once per frame by
    /// [`Self::upload_glint`].
    ///
    /// Clamping lives in [`gui_glint_uniform`], so this stores the raw pushed
    /// value and the domain is stated once.
    pub(crate) fn set_glint_options(&mut self, speed: f64, strength: f32) {
        self.glint_speed = speed;
        self.glint_strength = strength;
    }

    /// This frame's GUI glint speed and strength as they will reach the uniform —
    /// already clamped, so a gate can predict the value the shader sees rather than
    /// the one that was pushed.
    #[must_use]
    pub(crate) fn glint_options(&self) -> (f64, f32) {
        (
            lodestone_render::glint::clamp_speed(self.glint_speed),
            lodestone_render::glint::clamp_strength(self.glint_strength),
        )
    }

    /// The attached flat item atlas, if any. Cloned (an `Arc`) so the caller can
    /// hand it to the CPU builder without holding a borrow of `self`.
    pub(crate) fn item_atlas(&self) -> Option<Arc<ItemAtlas>> {
        self.sprites.as_ref().map(|s| Arc::clone(&s.atlas))
    }

    /// Whether the 3-D item-model pass is attached. Building model geometry when
    /// it is not is pure waste — there is nowhere to draw it.
    ///
    /// Also gates the **special** (block-entity) icon pass, which is built lazily
    /// off the same signal: both need a depth attachment and both are absent on a
    /// jar-less run, so the two screens' single `want_models` branch covers both
    /// and no caller has to learn about a second flag.
    pub(crate) fn models_attached(&self) -> bool {
        self.models.is_some()
    }

    /// How many block-entity sheets the special-icon pass loaded, or `0` when it
    /// is not built. The counter that tells "no chest in any slot" apart from "no
    /// pack, so a chest could never draw" — the distinction a pixel gate cannot
    /// make on its own.
    pub(crate) fn special_sheet_count(&self) -> usize {
        self.special.as_ref().map_or(0, SpecialIcons::sheet_count)
    }

    /// Drop the special-renderer pass so the next frame that needs one rebuilds
    /// it against the **current** pack stack. The reload-time sibling of
    /// [`Self::attach_items`]/[`Self::attach_item_models`], and it has to exist
    /// separately from them because this pass is *not* attached — it builds
    /// itself lazily on first use, off `queue`, which those two do not have.
    ///
    /// # Why a reload needs this at all
    ///
    /// [`SpecialIcons`] owns everything it draws with (its own doc says so):
    /// the block-entity sheets come from `crate::resources::load_block_entity_textures`,
    /// which reads the pack stack *live* at the moment of the call. So unlike
    /// the flat and 3-D item streams — whose defect on a resource-pack
    /// generation bump was sampling a **dropped** atlas — this pass keeps
    /// sampling a perfectly valid one that simply belongs to the *previous*
    /// pack. A pack that restyles a chest, a shulker box, a banner or a skull
    /// therefore reached the world's own block-entity pass and never reached a
    /// GUI slot, with nothing red and nothing dropped.
    ///
    /// The lazy build itself no longer latches — see [`Self::special_declines`]
    /// — so this is now purely about *which pack* the sheets came from, not
    /// about recovering a stream that failed to build.
    ///
    /// Cheap for the same reason the lazy build is affordable: nothing is
    /// rebuilt here, and the next rebuild only happens on a frame that actually
    /// carries a special icon.
    /// Log one episode of the special pass declining to draw, and count the
    /// frames it lasts.
    ///
    /// Warns on the **first** frame of an episode and stays quiet after that:
    /// this runs inside the frame loop, so one line per frame would bury the
    /// very thing it exists to surface. The paired recovery line is emitted by
    /// [`Self::prepare_special`] when a later build succeeds, so an episode
    /// always has both ends in the log and its length is readable from them.
    fn note_special_decline(&mut self, wanted: usize, why: &'static str) {
        self.special_declines = self.special_declines.saturating_add(1);
        if self.special_declines == 1 {
            tracing::warn!(
                target: "assets",
                icons_dropped = wanted,
                "the GUI special-renderer icon pass drew nothing: {why}"
            );
        }
    }

    pub(crate) fn reload_special(&mut self) {
        self.special = None;
        self.special_declines = 0;
    }

    /// Attach the flat item-sprite [`ItemAtlas`] so slots draw real item icons.
    /// Uploads the atlas texture, builds a textured pipeline, and binds it.
    pub(crate) fn attach_items(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        atlas: Arc<ItemAtlas>,
        label: &'static str,
    ) {
        let sp = build_sprite_pipeline(
            device,
            queue,
            atlas.atlas(),
            HUD_SPRITE_WGSL,
            color_format,
            4096,
            label,
        );
        self.sprites = Some(SpriteIcons {
            atlas,
            gpu: sp.gpu,
            pipeline: sp.pipeline,
            bind_group: sp.bind_group,
            buffer: sp.buffer,
            capacity_floats: sp.capacity_floats,
        });
    }

    /// Attach the **2-D GUI glint** pass, so an enchanted flat item icon in a
    /// slot shimmers. `img` is the decoded `enchanted_glint_item.png`
    /// ([`crate::resources::load_glint_texture`]).
    ///
    /// Must be called **after** [`Self::attach_items`] — the pass masks itself
    /// against the item atlas, so there is nothing to bind without one, and this
    /// is a no-op in that case. Nothing else in the frame changes: a renderer
    /// without this pass draws every icon exactly as before.
    pub(crate) fn attach_glint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        img: &lodestone_assets::Image,
        label: &'static str,
    ) {
        let Some(sprites) = self.sprites.as_ref() else {
            return;
        };
        self.glint = Some(GuiGlint::new(
            device,
            queue,
            color_format,
            &sprites.gpu,
            img,
            label,
        ));
    }

    /// Grow and upload the glint stream, returning the vertex count to draw.
    /// Zero when the stream is empty or the pass is not attached, so the caller's
    /// draw can be unconditional.
    ///
    /// Also rewrites the pass's own uniform with this frame's scroll offsets —
    /// that is what makes the shimmer move, and it is why this is a separate
    /// method rather than another parameter on [`Self::upload`]: the uniform is
    /// per-frame, not per-screen.
    pub(crate) fn upload_glint(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        verts: &[f32],
        label: &'static str,
    ) -> u32 {
        if verts.is_empty() {
            return 0;
        }
        // Copied out before the `&mut self.glint` borrow below, not read through
        // it: `self.glint_speed` and the mutable pass borrow cannot coexist.
        let (g_speed, g_strength) = (self.glint_speed, self.glint_strength);
        let Some(g) = self.glint.as_mut() else {
            return 0;
        };
        if verts.len() > g.capacity_floats {
            g.capacity_floats = verts.len().next_power_of_two();
            g.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some(label),
                size: (g.capacity_floats * 4) as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&g.buffer, 0, bytemuck::cast_slice(verts));
        queue.write_buffer(
            &g.uniform,
            0,
            bytemuck::bytes_of(&gui_glint_uniform(g_speed, g_strength, g.atlas_px)),
        );
        (verts.len() / SPRITE_FLOATS_PER_VERTEX) as u32
    }

    /// Record the glint draw for one sub-range of the uploaded stream into an
    /// **already-open** pass — the same shape as
    /// [`Self::draw_sprites_range`], and it must be recorded *after* it in the
    /// same pass so the shimmer lands over the icon rather than under it.
    pub(crate) fn draw_glint_range(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        range: std::ops::Range<u32>,
    ) {
        if range.is_empty() {
            return;
        }
        let Some(g) = &self.glint else {
            return;
        };
        pass.set_pipeline(&g.pipeline);
        pass.set_bind_group(0, &g.uniform_bind_group, &[]);
        pass.set_bind_group(1, &g.texture_bind_group, &[]);
        pass.set_vertex_buffer(0, g.buffer.slice(..));
        pass.draw(range, 0..1);
    }

    /// Attach the GPU side of the **3-D block-item** icon pass, so slots holding
    /// a block draw vanilla's isometric mini-block instead of an empty well.
    ///
    /// Every resource is *borrowed from the world renderer* rather than created:
    /// `atlas_view`/`atlas_sampler` are the stitched block atlas
    /// ([`RenderState::model_atlas_view`](crate::gpu::RenderState::model_atlas_view)),
    /// `palette` its tint palette, `anim` its per-slot animation uniforms. See
    /// [`ModelIcons`] for why each of those sharings matters. `wgpu` resources
    /// are `Arc`-backed and a bind group keeps its own strong reference, so the
    /// borrows need not outlive this call.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn attach_item_models(
        &mut self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
        palette: &wgpu::Buffer,
        anim: &wgpu::Buffer,
        label: &'static str,
    ) {
        // `Solid` (not `Translucent`): depth writes on, back-face culling on with
        // `FrontFace::Ccw`. Both are required — culling is what removes the three
        // far faces of the cube, and the depth test is what stops the near ones
        // being overdrawn by them. The shader's cutout discard also drops
        // near-transparent texels, so a cross-shaped model does not paint a box.
        let pipeline = ModelPipeline::for_layer(device, color_format, RenderLayer::Solid);
        // A placeholder view_proj; `upload` rewrites it from the live target size
        // before every draw. `model_shared_camera_buffer` sizes the buffer for
        // the camera **and** the folded fog block, and writes fog disabled.
        let camera_buffer = model_shared_camera_buffer(device, [[0.0; 4]; 4]);
        // Icon geometry carries its own screen position in its vertices, so
        // its origin binding is a permanent zero — built once, never rewritten.
        let origin_buffer = section_origin_buffer(device, [0.0, 0.0, 0.0]);
        let camera_bind_group = pipeline.camera_bind_group(device, &camera_buffer, &origin_buffer);
        let atlas_bind_group = device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some(label),
            layout: &pipeline.atlas_layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(atlas_view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: wgpu::BindingResource::Sampler(atlas_sampler),
                },
            ],
        });
        let palette_bind_group = pipeline.palette_bind_group(device, palette);
        let anim_bind_group = pipeline.anim_bind_group(device, anim);
        let capacity_bytes = 16 * 1024;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some(label),
            size: capacity_bytes as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.models = Some(ModelIcons {
            pipeline,
            camera_buffer,
            origin_buffer,
            camera_bind_group,
            atlas_bind_group,
            palette_bind_group,
            anim_bind_group,
            buffer,
            capacity_bytes,
        });
        // The special (block-entity) pass needs a `queue` to upload its sheets and
        // this call has none — every caller's wrapper takes `device` only, up
        // through `app.rs`. Rather than widen four signatures across three
        // contended files, remember the format and build on first use in
        // `upload`, which has both.
        self.color_format = Some(color_format);
    }

    /// Grow the buffers as needed, upload both streams, and rewrite the model
    /// pass's GUI camera for the current target size. Returns the
    /// `(sprite, model)` vertex counts the caller should draw — **zero** for a
    /// stream whose half is not attached, so the draws can be issued
    /// unconditionally.
    ///
    /// `gui_ortho` is the whole projection for the model stream: the vertices
    /// are already posed into GUI pixel space, the section origin is zero (they
    /// are not section-local), and fog is disabled (an inventory slot is not in
    /// the world).
    ///
    /// `width`/`height` must be the same **GUI pixel space** the caller posed
    /// those vertices into — the *logical* canvas (physical framebuffer divided
    /// by the effective GUI scale), not necessarily the physical render target
    /// size. Both call sites (`HudRenderer::render_with_item_models`,
    /// `ContainerRenderer::render_with_icons`) pass the logical size for exactly
    /// this reason; passing the raw physical size back here while the CPU pose
    /// used the logical one would make the model pass disagree with the flat
    /// sprite/colour passes it shares a frame with about how big a "GUI pixel"
    /// is.
    /// `special` is the third stream: block-entity icons, uploaded as per-part
    /// instance matrices against meshes that are already resident. It is not
    /// reflected in the returned counts — the two screens pass those straight to
    /// `draw_sprites`/`draw_models` as vertex counts, and a special batch has no
    /// single vertex count — so [`Self::draw_models`] reads the batches back off
    /// `self` instead. That is also what lets its signature stay unchanged.
    ///
    /// `special_carried_from` splits that third stream into the two **strata**
    /// [`IconStratum`] names: `special[..from]` belongs to the slots and
    /// `special[from..]` to the carried stack drawn above them. The sprite and
    /// model streams need no such argument — they are plain vertex slices, so the
    /// caller draws sub-ranges of one upload — but a special batch is *grouped*
    /// by `(model, sheet)` during upload, and a group spanning the split would
    /// draw a carried chest in the slot stratum. Pass `special.len()` for a
    /// screen with no carried stack (the hotbar).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn upload(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        sprite_verts: &[f32],
        model_verts: &[ModelVertex],
        special: &[SpecialIconDraw],
        special_carried_from: usize,
        width: u32,
        height: u32,
        label: &'static str,
    ) -> (u32, u32) {
        let mut sprite_count = 0;
        let mut model_count = 0;
        self.prepare_special(
            device,
            queue,
            special,
            special_carried_from.min(special.len()),
            width,
            height,
        );

        if !sprite_verts.is_empty()
            && let Some(s) = self.sprites.as_mut()
        {
            if sprite_verts.len() > s.capacity_floats {
                s.capacity_floats = sprite_verts.len().next_power_of_two();
                s.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: (s.capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(&s.buffer, 0, bytemuck::cast_slice(sprite_verts));
            sprite_count = (sprite_verts.len() / SPRITE_FLOATS_PER_VERTEX) as u32;
        }

        if !model_verts.is_empty()
            && let Some(m) = self.models.as_mut()
        {
            let bytes = std::mem::size_of_val(model_verts);
            if bytes > m.capacity_bytes {
                m.capacity_bytes = bytes.next_power_of_two();
                m.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some(label),
                    size: m.capacity_bytes as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(&m.buffer, 0, bytemuck::cast_slice(model_verts));
            update_model_shared_camera_buffer(
                queue,
                &m.camera_buffer,
                gui_ortho(width, height).to_cols_array_2d(),
                lodestone_render::fog::FogUniform::disabled(),
            );
            model_count = model_verts.len() as u32;
        }

        (sprite_count, model_count)
    }

    /// Build this frame's special-icon batches: lazily construct the pass, group
    /// the draws by `(model, sheet)`, compose one matrix per mesh part per icon,
    /// and upload them as per-part instance buffers.
    ///
    /// # Why `part_transforms` per icon rather than one matrix per icon
    ///
    /// Because the mesh's vertices are **part-local** — the same property that
    /// lets the world pass animate a chest lid. `part_transforms(placement, &[])`
    /// composes `placement · chain(parent) · rest_pose` for every part, and the
    /// instance buffer for part *p* holds part *p*'s matrix for every icon in the
    /// batch. Uploading one matrix per icon and letting the vertices carry their
    /// own offsets would collapse the hierarchy and put the lid at the origin.
    ///
    /// `&[]` overrides: an item chest is `openness = 0`, which is
    /// `ChestSpecialRenderer.Unbaked`'s default and means the rest pose *is* the
    /// closed lid. Nothing here drives the lid clock, so a chest in the inventory
    /// cannot drift open with the one you are standing at.
    fn prepare_special(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        special: &[SpecialIconDraw],
        carried_from: usize,
        width: u32,
        height: u32,
    ) {
        // Clear first and unconditionally: last frame's batches must not survive
        // into a frame that closed the inventory. Every early return below is a
        // frame that draws no special icons.
        if let Some(s) = self.special.as_mut() {
            s.batches.clear();
            s.carried_batches.clear();
            s.banner_layers.clear();
            s.carried_banner_layers.clear();
        }
        if special.is_empty() {
            return;
        }
        // Gated on the model pass, not on the format alone: `models_attached`
        // is the one signal both screens branch on, so a special icon can never
        // reach a frame the model pass was excluded from.
        //
        // Every `return` from here down is a frame on which real icons were
        // asked for and none was drawn, so each one says so. Nothing in this
        // function may decline silently: the stream being dark is invisible at
        // the draw site (a special item has no flat sprite to fall back to, so
        // the slot is simply empty) and it cost three investigations to find
        // by pixel-hunting what one log line names outright.
        let (Some(format), true) = (self.color_format, self.models.is_some()) else {
            self.note_special_decline(
                special.len(),
                if self.color_format.is_none() {
                    "the 3-D item-model pass was never attached (no colour format recorded), \
                     so there is nowhere to draw a block-entity icon"
                } else {
                    "the 3-D item-model pass is not attached (no ModelIcons), so there is \
                     nowhere to draw a block-entity icon"
                },
            );
            return;
        };
        // Retried whenever the pass is absent, never latched — see
        // [`Self::special_declines`].
        if self.special.is_none() {
            self.special = SpecialIcons::new(device, queue, format);
        }
        let Some(s) = self.special.as_mut() else {
            self.note_special_decline(
                special.len(),
                "SpecialIcons::new decoded no block-entity sheet at all — the vanilla pack \
                 stack could not be opened, or none of block_entity_texture_stems() resolved \
                 inside it. Every chest, shulker box, banner, shield and skull icon is dark \
                 until this succeeds",
            );
            return;
        };
        if self.special_declines > 0 {
            tracing::info!(
                target: "assets",
                frames_dark = self.special_declines,
                sheets = s.sheet_count(),
                "the GUI special-renderer icon pass built and is drawing again"
            );
            self.special_declines = 0;
        }

        // Before the batches, because a batch's bind group is looked up at draw
        // time and a skin that lands this frame should be bound this frame.
        s.install_ready_player_skins(device, queue, special);

        let base = build_special_batches(device, s, &special[..carried_from]);
        let carried = build_special_batches(device, s, &special[carried_from..]);
        s.batches = base;
        s.carried_batches = carried;
        s.banner_layers = build_banner_layer_batches(device, s, &special[..carried_from]);
        s.carried_banner_layers = build_banner_layer_batches(device, s, &special[carried_from..]);

        if !s.batches.is_empty()
            || !s.carried_batches.is_empty()
            || !s.banner_layers.is_empty()
            || !s.carried_banner_layers.is_empty()
        {
            // `gui_ortho(width, height)`, exactly as the model stream's camera —
            // the placements were composed in the same GUI pixel space, so the
            // two passes must project through the same matrix or a chest and the
            // block beside it disagree about how big a GUI pixel is.
            queue.write_buffer(
                &s.cam_buffer,
                0,
                bytemuck::bytes_of(&EntityCameraUniform {
                    camera: CameraUniform {
                        view_proj: gui_ortho(width, height).to_cols_array_2d(),
                        section_origin: [0.0, 0.0, 0.0, 0.0],
                    },
                    fog: FogUniform::disabled(),
                }),
            );
        }
    }
}

/// Group one stratum's [`SpecialIconDraw`]s into uploaded per-part instance
/// batches. Factored out of [`IconRenderer::prepare_special`] so the slot layer
/// and the carried stack can each get their own batch list without the grouping
/// being written twice — see [`IconStratum`].
fn build_special_batches(
    device: &wgpu::Device,
    s: &SpecialIcons,
    special: &[SpecialIconDraw],
) -> Vec<SpecialBatch> {
    let mut out: Vec<SpecialBatch> = Vec::new();
    if special.is_empty() {
        return out;
    }
    {
        // Group by `(model, sheet)` — the batch key the world pass uses, and for
        // the same reason: a trapped chest and a plain one share the mesh and
        // differ only in bind group, so keying on the model alone would draw both
        // with whichever sheet happened to be bound.
        let mut keys: Vec<(&'static str, lodestone_render::BlockEntityTexture)> = Vec::new();
        let mut per_key: Vec<Vec<Mat4>> = Vec::new();
        // Parallel to `per_key`, but indexed **per icon** rather than per part —
        // every icon sharing a `(model, texture)` key needs its own tint, since
        // two different-coloured banners both resolve to `(BANNER_FLAG,
        // banner_base)`. Kept separate from `per_key` rather than zipped into it
        // because `per_key` is part-major after the transpose below and this
        // stays icon-major throughout.
        let mut per_key_tint: Vec<Vec<[u8; 3]>> = Vec::new();
        for draw in special {
            // Only `Mesh` entries batch here — `BannerLayer` entries are
            // strictly ordered and drawn in a separate, unbatched pass; see
            // `build_banner_layer_batches`.
            let SpecialIconDraw::Mesh {
                model,
                texture,
                placement,
                tint,
            } = draw
            else {
                continue;
            };
            let Some(mesh) = s.models.get(*model) else {
                continue;
            };
            // A packaged stem with no bind group can never draw, so the batch is
            // dropped here as it always was. A **fetched** skin is different: its
            // bind group arrives later by construction, and the draw falls back
            // to the default skull sheet meanwhile (`draw_models_range`), so
            // dropping it here would turn "the wrong face for a few frames" into
            // "an empty slot until the network answers".
            if let lodestone_render::BlockEntityTexture::Static(stem) = texture
                && !s.textures.contains_key(stem)
            {
                continue;
            }
            let transforms = mesh.part_transforms(*placement, &[]);
            match keys.iter().position(|k| k.0 == *model && k.1 == *texture) {
                Some(i) => {
                    per_key[i].extend(transforms);
                    per_key_tint[i].push(*tint);
                }
                None => {
                    keys.push((*model, texture.clone()));
                    per_key.push(transforms);
                    per_key_tint.push(vec![*tint]);
                }
            }
        }

        for (((model, texture), flat), icon_tints) in
            keys.into_iter().zip(per_key).zip(per_key_tint)
        {
            let Some(mesh) = s.models.get(model) else {
                continue;
            };
            let part_count = mesh.parts.len();
            if part_count == 0 || flat.is_empty() {
                continue;
            }
            // `flat` is icon-major (`part_transforms` returns one matrix per part,
            // appended per icon); the instance buffers must be **part**-major, one
            // buffer per part holding that part's matrix for each icon. Getting
            // this transpose wrong draws every part of the mesh at some other
            // part's position, which for a chest is a scattered pile of boxes.
            let count = flat.len() / part_count;
            let tints: Vec<lodestone_render::InstanceTint> = icon_tints
                .iter()
                .map(|&rgb| lodestone_render::InstanceTint::rgb(rgb))
                .collect();
            let parts = (0..part_count)
                .map(|p| {
                    let per_icon: Vec<Mat4> = (0..count)
                        .map(|icon| flat[icon * part_count + p])
                        .collect();
                    // No `lights`: an inventory slot has no world light to
                    // sample, so `upload_instances_tinted` falls back to
                    // `ENTITY_FULLBRIGHT` for every instance. `tints` is
                    // `InstanceTint::NONE` (a no-op multiply) for every kind but
                    // the banner flag, so this is byte-identical to the old
                    // `upload_instances` call for every existing consumer.
                    upload_instances_tinted(device, &per_icon, &[], &tints)
                })
                .collect();
            out.push(SpecialBatch {
                model,
                texture,
                count: count as u32,
                parts,
            });
        }
    }
    out
}

/// Upload one stratum's [`SpecialIconDraw::BannerLayer`] entries as one-instance
/// buffers, in order — the GUI-icon sibling of [`build_special_batches`], and
/// never merged with it: a layer needs its own pipeline, its own mask bind
/// group, and (unlike an opaque batch) must never be reordered or coalesced
/// with another icon's layers. See [`SpecialBannerLayerBatch`]'s doc.
fn build_banner_layer_batches(
    device: &wgpu::Device,
    s: &SpecialIcons,
    special: &[SpecialIconDraw],
) -> Vec<SpecialBannerLayerBatch> {
    special
        .iter()
        .filter_map(|draw| {
            let SpecialIconDraw::BannerLayer {
                family,
                pattern,
                placement,
                color,
            } = draw
            else {
                return None;
            };
            // The mesh's own **local** part transform(s), not the raw item
            // placement — `banner_flag_model`'s cube sits under a `"flag"`
            // child part with its own `PartPose::offset(0.0, -44.0, 0.0)`
            // relative to the root, exactly like the opaque `Mesh` draw above
            // resolves via `mesh.part_transforms`. Skipping that composition
            // (an earlier version of this function did) draws the mask
            // vertices in the *root's* space instead of the flag's, which for
            // a 16×16 icon lands the geometry entirely outside the visible
            // cell — measured live: `moved px=0` in every case, opaque flag
            // and translucent layer both present, until this was fixed.
            //
            // A shield has no single named part to single out this way — its
            // layers re-tint the *whole* mesh, so its own world matrix is
            // just `*placement` itself (`shield_model`'s root is
            // `PartPose::ZERO`, and both `plate`/`handle` sit directly under
            // it with no further offset).
            let world = match family {
                PatternFamily::Banner => {
                    let flag_mesh = s.models.get("banner_flag")?;
                    let flag_index = flag_mesh.index_of("flag")?;
                    flag_mesh
                        .part_transforms(*placement, &[])
                        .into_iter()
                        .nth(flag_index)?
                }
                PatternFamily::Shield => *placement,
            };
            let instances = upload_instances_tinted(
                device,
                &[world],
                &[],
                &[lodestone_render::InstanceTint::rgb(*color)],
            )?;
            Some(SpecialBannerLayerBatch {
                family: *family,
                pattern: pattern.clone(),
                instances,
            })
        })
        .collect()
}

/// Which **stratum** an icon draw belongs to — vanilla's `graphics.nextStratum()`
/// in `AbstractContainerScreen.extractCarriedItem`
/// (`AbstractContainerScreen.java`), which is called immediately before the
/// carried stack is drawn and nowhere else on that screen.
///
/// The distinction is not cosmetic and cannot be expressed as push order, which
/// is why it is a type. The GUI item passes run **model first, then flat
/// sprites**, because only the model pass needs a depth attachment and its
/// attachments are fixed for its lifetime. So within one stratum a block item
/// always loses to a flat sprite, and two block items at the same GUI depth
/// resolve against a depth buffer rather than against append order. A carried
/// stack has to be *above every slot*, whichever of the four combinations of
/// (flat, block) × (cursor, slot) it happens to be — so it gets a second, later
/// stratum whose model pass clears depth again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IconStratum {
    /// The slot layer: wells, slot contents, their counts and durability bars.
    Slots,
    /// The carried (cursor) stack, drawn above the slot layer.
    Carried,
}

impl IconRenderer {
    /// Record the flat item-sprite draw into an **already-open** pass, so the
    /// caller can keep icons in the same pass as its other 2-D chrome. A no-op
    /// when `count` is zero or the atlas is not attached.
    pub(crate) fn draw_sprites(&self, pass: &mut wgpu::RenderPass<'_>, count: u32) {
        self.draw_sprites_range(pass, 0..count);
    }

    /// As [`draw_sprites`](Self::draw_sprites), but for one sub-range of the
    /// uploaded sprite stream — how the container screen draws its slot icons and
    /// then, in a later pass, the carried stack. Both strata live in one upload
    /// because they are one contiguous `Vec<f32>`; only the *draw* splits.
    pub(crate) fn draw_sprites_range(
        &self,
        pass: &mut wgpu::RenderPass<'_>,
        range: std::ops::Range<u32>,
    ) {
        if range.is_empty() {
            return;
        }
        let Some(s) = &self.sprites else {
            return;
        };
        pass.set_pipeline(&s.pipeline);
        pass.set_bind_group(0, &s.bind_group, &[]);
        pass.set_vertex_buffer(0, s.buffer.slice(..));
        pass.draw(range, 0..1);
    }

    /// Record the 3-D block-item pass. It gets its **own** pass because it is
    /// the only part of a GUI overlay that needs a depth buffer, and it *clears*
    /// depth rather than loading it: the world's depth is still resident from
    /// the terrain pass and would occlude a GUI item sitting at clip depth ~0.5.
    /// Nothing later in the frame reads depth, so clearing it here is free.
    ///
    /// A no-op when `count` is zero, the pass is not attached, or the caller has
    /// no depth attachment to lend.
    /// It also carries the **special** (block-entity) icons, which share the pass
    /// rather than opening a second one: they need exactly the same depth clear,
    /// and a second pass would clear it again and erase the block items already
    /// drawn.
    ///
    /// `count` is the *model* stream's vertex count, and deliberately does not
    /// gate the special draw: a hotbar holding only a chest has `count == 0`, so
    /// returning early on it would reproduce this issue's original bug one layer
    /// down — the pass would exist, be attached, hold uploaded batches, and never
    /// run. The guard therefore asks whether **either** stream has work.
    pub(crate) fn draw_models(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        count: u32,
        label: &'static str,
    ) {
        self.draw_models_range(encoder, view, depth, 0..count, IconStratum::Slots, label);
    }

    /// As [`draw_models`](Self::draw_models), but for one sub-range of the
    /// uploaded model stream and one [`IconStratum`]'s special batches.
    ///
    /// **Each stratum clears depth again**, which is the whole point: that is what
    /// makes vanilla's `nextStratum()` mean "above everything drawn so far" for
    /// depth-tested geometry, and it is why a carried block item cannot be
    /// resolved into a slot block item's silhouette. Clearing is free here for the
    /// same reason the first clear is — nothing later in the frame reads depth.
    pub(crate) fn draw_models_range(
        &self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        range: std::ops::Range<u32>,
        stratum: IconStratum,
        label: &'static str,
    ) {
        let count = range.end.saturating_sub(range.start);
        let specials = self.special.as_ref().map_or(&[][..], |s| match stratum {
            IconStratum::Slots => s.batches.as_slice(),
            IconStratum::Carried => s.carried_batches.as_slice(),
        });
        let banner_layers = self.special.as_ref().map_or(&[][..], |s| match stratum {
            IconStratum::Slots => s.banner_layers.as_slice(),
            IconStratum::Carried => s.carried_banner_layers.as_slice(),
        });
        if count == 0 && specials.is_empty() && banner_layers.is_empty() {
            return;
        }
        let Some(depth_view) = depth else {
            return;
        };
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some(label),
            color_attachments: &[Some(load_colour_attachment(view))],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth_view,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Store,
                }),
                stencil_ops: None,
            }),
            timestamp_writes: None,
            occlusion_query_set: None,
            multiview_mask: None,
        });
        if count > 0
            && let Some(m) = &self.models
        {
            pass.set_pipeline(&m.pipeline.pipeline);
            // The dynamic offset is always 0: `origin_buffer` is a single
            // permanent zero slot (see `ModelIcons::origin_buffer`'s doc).
            pass.set_bind_group(0, &m.camera_bind_group, &[0]);
            pass.set_bind_group(1, &m.atlas_bind_group, &[]);
            pass.set_bind_group(2, &m.palette_bind_group, &[]);
            pass.set_bind_group(3, &m.anim_bind_group, &[]);
            pass.set_vertex_buffer(0, m.buffer.slice(..));
            pass.draw(range, 0..1);
        }

        // The special-renderer icons (chests), through `EntityPipeline`: two bind
        // groups, the same `ModelVertex` layout, indexed and instanced per part.
        // Second, so a chest cannot be depth-rejected by a block item that has
        // not been drawn yet — they never share a slot, but the order is free.
        if let Some(s) = &self.special
            && !specials.is_empty()
        {
            pass.set_pipeline(&s.pipeline.pipeline);
            pass.set_bind_group(0, &s.cam_bind_group, &[]);
            for batch in specials {
                let Some(model) = s.gpu_models.get(batch.model) else {
                    continue;
                };
                // A fetched custom-head skin, else the packaged stem. A dynamic
                // miss falls back to the default skull sheet for that frame,
                // exactly as the world's placed-head pass falls back to Steve —
                // and `SpecialIcons::install_ready_player_skins` has already
                // logged that it is happening.
                let texture = match &batch.texture {
                    lodestone_render::BlockEntityTexture::Static(stem) => s.textures.get(stem),
                    lodestone_render::BlockEntityTexture::PlayerSkin(url) => {
                        s.player_skins.get(url.as_ref()).or_else(|| {
                            s.textures.get(lodestone_render::skull_texture_stem(
                                lodestone_render::SkullType::Player,
                            ))
                        })
                    }
                };
                let Some(texture) = texture else {
                    continue;
                };
                pass.set_bind_group(1, texture, &[]);
                pass.set_vertex_buffer(0, model.vertices.slice(..));
                pass.set_index_buffer(model.indices.slice(..), wgpu::IndexFormat::Uint32);
                for (range, buffer) in model.parts.iter().zip(&batch.parts) {
                    let (Some(buffer), true) = (buffer.as_ref(), range.index_count > 0) else {
                        continue;
                    };
                    pass.set_vertex_buffer(1, buffer.slice(..));
                    let end = range.index_start + range.index_count;
                    pass.draw_indexed(range.index_start..end, 0, 0..batch.count);
                }
            }
        }

        // A banner's or shield's own translucent pattern-layer draws, over the
        // same opaque geometry the loop above just drew — the GUI-icon
        // sibling of `gpu/frame.rs`'s world-space banner-layer pass and
        // `gpu/first_person.rs`'s hand one. Third, so a mask's soft edges
        // blend against pixels the opaque pass already wrote rather than
        // against whatever this pass loaded.
        //
        // `banner_layers` mixes both families across every slot in this
        // stratum (a screen can hold a banner in one slot and a shield in
        // another), so this draws each family in its own pass rather than
        // assuming one geometry for the whole list — the GUI-icon analogue
        // of `gpu/first_person.rs`'s per-call family tag, needed here because
        // one call covers many items rather than one held stack.
        //
        // **`index_of("flag")`, not `.parts.first()`, for the banner arm.**
        // `banner_flag_model`'s root part carries no cube of its own (only a
        // `"flag"` child does), so its own `PartRange` is `index_count == 0`
        // — `.first()` would silently select that empty range and draw zero
        // indices. See `gpu/frame.rs`'s identical fix for the measurement
        // that found this. A shield has no such named part: its layers
        // re-tint the whole mesh, so every part with real geometry draws.
        if let Some(s) = &self.special {
            for family in [PatternFamily::Banner, PatternFamily::Shield] {
                let layers: Vec<&SpecialBannerLayerBatch> =
                    banner_layers.iter().filter(|l| l.family == family).collect();
                if layers.is_empty() {
                    continue;
                }
                let (mesh_name, single_part): (&str, Option<&str>) = match family {
                    PatternFamily::Banner => ("banner_flag", Some("flag")),
                    PatternFamily::Shield => ("shield", None),
                };
                let mask_map = match family {
                    PatternFamily::Banner => &s.banner_patterns,
                    PatternFamily::Shield => &s.shield_patterns,
                };
                let Some(gpu) = s.gpu_models.get(mesh_name) else {
                    continue;
                };
                let ranges: Vec<lodestone_render::entity::PartRange> = match single_part {
                    Some(part_name) => s
                        .models
                        .get(mesh_name)
                        .and_then(|mesh| mesh.index_of(part_name))
                        .and_then(|i| gpu.parts.get(i))
                        .filter(|r| r.index_count > 0)
                        .copied()
                        .into_iter()
                        .collect(),
                    None => gpu.parts.iter().filter(|r| r.index_count > 0).copied().collect(),
                };
                if ranges.is_empty() {
                    continue;
                }
                pass.set_pipeline(&s.banner_layer_pipeline);
                pass.set_bind_group(0, &s.cam_bind_group, &[]);
                pass.set_vertex_buffer(0, gpu.vertices.slice(..));
                pass.set_index_buffer(gpu.indices.slice(..), wgpu::IndexFormat::Uint32);
                for layer in layers {
                    let Some(mask) = mask_map.get(&layer.pattern) else {
                        continue;
                    };
                    pass.set_bind_group(1, mask, &[]);
                    pass.set_vertex_buffer(1, layer.instances.slice(..));
                    for range in &ranges {
                        let end = range.index_start + range.index_count;
                        pass.draw_indexed(range.index_start..end, 0, 0..1);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod pop_tests {
    use super::{ItemIcon, ResourceLocation, SpriteLayer, pop_squeeze_rect, sprite_layer_tint, stack_has_foil};

    /// A bare `ItemIcon` with no live stack behind it — `dyed_color` and
    /// `potion_color` both `None`, i.e. every source falls back to the
    /// definition's own default. What every test below wants: these gates are
    /// about the gamma conversion, not the component lookup, and
    /// [`super::item_tint_component_gates`] below is where the component side
    /// is pinned.
    fn blank_icon() -> ItemIcon {
        ItemIcon {
            item: ResourceLocation::parse("minecraft:stick").expect("static id parses"),
            count: 1,
            damage: None,
            max_damage: None,
            enchanted: false,
            custom_model_data: None,
            dyed_color: None,
            potion_color: None,
            banner_patterns: Vec::new(),
            base_color: None,
            skin: None,
        }
    }

    /// `pop <= 0.0` must reproduce the original square rect exactly — the
    /// "not animating" control for [`super::draw_item_icon_popped`]'s early
    /// return, and the reason that function is safe to call unconditionally
    /// from `hud.rs` with an idle `0.0` every settled frame.
    #[test]
    fn idle_pop_is_the_original_square_rect() {
        assert_eq!(pop_squeeze_rect(10.0, 20.0, 16.0, 0.0), [10.0, 20.0, 16.0, 16.0]);
        assert_eq!(pop_squeeze_rect(10.0, 20.0, 16.0, -3.0), [10.0, 20.0, 16.0, 16.0]);
    }

    /// Predicted value at the instant a stack lands (`pop == 5.0`, vanilla's
    /// `setPopTime(5)`): `squeeze = 1 + 5/5 = 2.0`, `scale_x = 0.5`,
    /// `scale_y = 1.5`. Pivot at `(x+8, y+12) = (18, 32)` (scale `16/16 ==
    /// 1.0`). `new_x = 18 + (10-18)*0.5 = 14`, `new_y = 32 + (20-32)*1.5 =
    /// 14`, `w = 16*0.5 = 8`, `h = 16*1.5 = 24`.
    ///
    /// Two wrong hypotheses this pins against: a **uniform** 2x scale (the
    /// "it's just bigger" guess) would predict a `32×32` square, not an
    /// oblong; and a **rect-centred-on-the-pivot** scale (treating `(x+8,
    /// y+12)` as the rect's own centre, which it is only for `x`) would
    /// predict `y == 20` unchanged — the pivot is `12` down but the resulting
    /// half-height is `12`, so a centred rect coincidentally also lands on
    /// `20` at this exact size, which is why [`pivot_scales_with_icon_size_not_just_the_rect`]
    /// below uses a different size specifically to separate the two.
    #[test]
    fn pop_five_is_a_2x_squeeze_at_the_vanilla_pivot() {
        assert_eq!(pop_squeeze_rect(10.0, 20.0, 16.0, 5.0), [14.0, 14.0, 8.0, 24.0]);
    }

    /// Two opposite phases at a non-16px size, which is what actually
    /// separates "scale about the fixed pivot point" from "resize the rect
    /// centred on the pivot" for `y` — at `size == 16.0` the two coincide (see
    /// the note on [`pop_five_is_a_2x_squeeze_at_the_vanilla_pivot`]) and a
    /// wrong centred-rect implementation would pass unnoticed. At `size = 32`
    /// (`scale = 2.0`), pivot is `(x + 16, y + 24) = (26, 44)`.
    #[test]
    fn pivot_scales_with_icon_size_not_just_the_rect() {
        // Half decay (`pop = 2.5`): `squeeze = 1.5`, `scale_x = 1/1.5`,
        // `scale_y = 1.25`. `w = 32/1.5 = 21.333...`, `h = 32*1.25 = 40.0`.
        // `new_x = 26 + (10-26)/1.5 = 26 - 10.666... = 15.333...`.
        // `new_y = 44 + (20-44)*1.25 = 44 - 30 = 14.0` — the
        // centred-rect wrong hypothesis would instead predict `44 - 20 = 24.0`.
        let r = pop_squeeze_rect(10.0, 20.0, 32.0, 2.5);
        assert!((r[2] - 32.0 / 1.5).abs() < 1e-4, "w = {}", r[2]);
        assert_eq!(r[3], 40.0);
        assert!((r[0] - 15.333_33).abs() < 1e-3, "x = {}", r[0]);
        assert!(
            (r[1] - 14.0).abs() < 1e-4,
            "y = {} (centred-rect wrong hypothesis predicts 24.0)",
            r[1]
        );

        // Fully settled (`pop = 0.0`) at the same size must be the plain
        // square again — the opposite phase from the case above.
        assert_eq!(pop_squeeze_rect(10.0, 20.0, 32.0, 0.0), [10.0, 20.0, 32.0, 32.0]);
    }

    /// The sRGB electro-optical transfer function, **written from the published
    /// standard** rather than called from `lodestone_render::fog`.
    ///
    /// This duplication is the point: it is the external anchor. Asserting
    /// [`sprite_layer_tint`] against the very function it calls would be
    /// `decode(encode(x))` — satisfied by two symmetric misunderstandings — so the
    /// expectation has to come from the spec (IEC 61966-2-1: a 12.92 linear
    /// segment below 0.04045, then `((c + 0.055) / 1.055) ^ 2.4`).
    fn srgb_eotf_from_spec(c: f64) -> f64 {
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// A `minecraft:constant` tint layer carrying `argb`.
    fn constant_layer(argb: u32) -> SpriteLayer {
        SpriteLayer {
            sprite: ResourceLocation::parse("minecraft:item/lily_pad").expect("static id parses"),
            tint: Some(lodestone_assets::TintSource {
                kind: "minecraft:constant".to_string(),
                default: Some(argb as i32),
                grass: None,
                index: 0,
            }),
        }
    }

    /// [`sprite_layer_tint`] must land on the **converted** hypothesis and be far
    /// from the **forwarded-raw** one.
    ///
    /// This is the *magnitude* discrimination the repo's evidence standard asks
    /// for, not a direction check: both hypotheses agree on the sign and on the
    /// ordering of the three channels, so a gate asserting only "darker than
    /// white, and green is the largest" passes identically for the washed-out
    /// version that shipped. So predict **both** values and require the
    /// measurement to land on one.
    ///
    /// The subject colour is vanilla's own `lily_pad` **item** tint,
    /// `0x71C35C` — deliberately not the *block* tint `0x208030`, which is a
    /// different colour for the same plant. Vanilla never consults `BlockColors`
    /// for an item, so a wiring that reached for the block table would land here
    /// on visibly wrong numbers rather than merely on a different code path.
    #[test]
    fn a_constant_tint_is_converted_into_the_shaders_space_not_forwarded_raw() {
        let tint = sprite_layer_tint(&constant_layer(0xFF71_C35C), &blank_icon());

        for (i, byte, name) in [(0usize, 0x71u32, "R"), (1, 0xC3, "G"), (2, 0x5C, "B")] {
            let t = f64::from(byte) / 255.0;
            let converted = srgb_eotf_from_spec(t);
            let forwarded = t;

            let got = f64::from(tint[i]);
            let d_converted = (got - converted).abs();
            let d_forwarded = (got - forwarded).abs();

            assert!(
                d_converted < 1e-4,
                "{name}: tint {got} is not the converted value {converted} \
                 (spec sRGB EOTF of {t}); off by {d_converted}"
            );
            // The two hypotheses are 0.16 to 0.28 apart on these channels, so a
            // 0.05 floor is nowhere near either boundary — this asserts the gate
            // can actually tell them apart, rather than that they happen to differ.
            assert!(
                d_forwarded > 0.05,
                "{name}: the forwarded-raw hypothesis {forwarded} is only \
                 {d_forwarded} away from the measured {got} — this gate cannot \
                 discriminate, so its pass means nothing"
            );
        }

        // Item tints are opaque multipliers in every vanilla case.
        assert_eq!(tint[3], 1.0, "alpha must stay 1.0, not carry the tint's");
    }

    /// Control: the conversion must be the *reason* the previous gate passes.
    ///
    /// Feeding the same channel bytes through the forwarded-raw formula and
    /// asserting it *fails* the tight tolerance proves the `< 1e-4` assertion
    /// above is load-bearing rather than satisfied by any plausible number. Run
    /// this and the discrimination is real; delete it and a future refactor that
    /// re-broadens the tolerance goes unnoticed.
    #[test]
    fn control_the_forwarded_raw_tint_would_fail_the_conversion_assertion() {
        for byte in [0x71u32, 0xC3, 0x5C] {
            let t = f64::from(byte) / 255.0;
            let converted = srgb_eotf_from_spec(t);
            assert!(
                (t - converted).abs() >= 1e-4,
                "byte {byte}: raw {t} and converted {converted} are within the \
                 gate's own tolerance, so the gate above proves nothing"
            );
        }
    }

    /// An untinted layer is exactly white — the pre-fix behaviour, which must
    /// survive for the ~97% of items that declare no tint at all. A conversion
    /// applied unconditionally would darken every item in the game.
    #[test]
    fn an_untinted_layer_stays_exactly_white() {
        let layer = SpriteLayer {
            sprite: ResourceLocation::parse("minecraft:item/stick").expect("static id parses"),
            tint: None,
        };
        assert_eq!(sprite_layer_tint(&layer, &blank_icon()), [1.0, 1.0, 1.0, 1.0]);
    }

    /// White (`0xFFFFFF`) is the conversion's fixed point, so a fully-bright
    /// constant tint must also come out as exact white. This separates "converts"
    /// from "darkens everything": a wrong conversion applied to white would
    /// show up here, and nowhere else.
    #[test]
    fn a_white_constant_tint_is_the_conversions_fixed_point() {
        let tint = sprite_layer_tint(&constant_layer(0xFFFF_FFFF), &blank_icon());
        for (i, c) in tint.iter().enumerate() {
            assert!(
                (c - 1.0).abs() < 1e-6,
                "channel {i} of a white tint is {c}, not 1.0"
            );
        }
    }

    /// The reported case (issue #605's second half): an **unenchanted**
    /// `minecraft:enchanted_book` still glints, because
    /// `lodestone_render::glint::has_foil_for_item`'s baked census answers
    /// before the (empty) enchantments list is ever consulted — content-based
    /// glint was, and remains, structurally unable to fix this, since vanilla's
    /// `enchanted_book` stores its enchantments under `STORED_ENCHANTMENTS`,
    /// which `isEnchanted`/`isFoil` never read.
    ///
    /// A plain unenchanted item with no baked entry (`minecraft:stick`) is the
    /// negative control in the same test, so a census that matched everything
    /// would fail here rather than only being caught by omission.
    #[test]
    fn stack_has_foil_reads_the_baked_override_for_an_unenchanted_enchanted_book() {
        use lodestone_game::item::ItemStack;

        let book = ItemStack::new(
            "minecraft:enchanted_book".parse().expect("static id parses"),
            1,
        );
        assert!(
            stack_has_foil(&book),
            "an unenchanted enchanted_book must glint from the baked override"
        );

        let stick = ItemStack::new("minecraft:stick".parse().expect("static id parses"), 1);
        assert!(
            !stack_has_foil(&stick),
            "a plain item with no baked override and no enchantments must not glint"
        );
    }

    /// `stack_has_foil` reads the enchantments component, and reads it through the
    /// render crate's predicate.
    ///
    /// Both directions, because "always true" and "always false" are the two ways
    /// this can be wrong and `enchanted` was hardcoded `false` for long enough
    /// that the false case is the regression to fear.
    #[test]
    fn stack_has_foil_tracks_the_enchantments_component() {
        use lodestone_game::item::{ComponentValue, ENCHANTMENTS_COMPONENT, ItemStack};

        let id: lodestone_model::Identifier =
            "minecraft:diamond_sword".parse().expect("static id parses");
        let plain = ItemStack::new(id.clone(), 1);
        assert!(
            !stack_has_foil(&plain),
            "an unenchanted stack must not glint"
        );

        let mut enchanted = ItemStack::new(id, 1);
        enchanted.components_mut().insert(
            ENCHANTMENTS_COMPONENT.parse().expect("static id parses"),
            ComponentValue::Enchantments(vec![lodestone_model::item::ItemEnchantment {
                id: 0,
                level: 1,
            }]),
        );
        assert!(
            stack_has_foil(&enchanted),
            "a stack carrying one enchantment must glint"
        );

        // An explicitly *empty* list is the same as no component — the invariant
        // `stack_has_foil`'s doc leans on, asserted rather than assumed.
        let mut empty_list = ItemStack::new(
            "minecraft:stick".parse().expect("static id parses"),
            1,
        );
        empty_list.components_mut().insert(
            ENCHANTMENTS_COMPONENT.parse().expect("static id parses"),
            ComponentValue::Enchantments(Vec::new()),
        );
        assert!(
            !stack_has_foil(&empty_list),
            "an empty enchantments list must not glint"
        );
    }
}

/// The colour gate for the potion/dye tint-wiring fix: `sprite_layer_tint`
/// used to resolve every source against `ItemTintContext::default()`
/// regardless of the `ItemIcon` it was handed, so a correct resolver still
/// painted the wrong colour. These assert the **emitted quad's colour** —
/// what actually reaches `push_sprite_quad`'s vertex stream — not
/// `TintProvenance`; `TintProvenance::Component` already came back right
/// before this fix and was never the missing link.
#[cfg(test)]
mod tint_wiring_tests {
    use super::{ItemIcon, ItemTintContext, ResourceLocation, SpriteLayer, sprite_layer_tint};

    /// The sRGB EOTF, independently re-derived from the published standard
    /// (IEC 61966-2-1) rather than called from `lodestone_render::fog` — see
    /// `pop_tests::srgb_eotf_from_spec`'s doc for why: asserting the function
    /// under test against the very conversion it calls internally is
    /// `decode(encode(x))`, satisfied by two symmetric misunderstandings.
    fn srgb_eotf(c: f64) -> f64 {
        if c <= 0.040_45 {
            c / 12.92
        } else {
            ((c + 0.055) / 1.055).powf(2.4)
        }
    }

    /// The linear-space quad colour `sprite_layer_tint` must emit for a
    /// pre-blend sRGB `0xRRGGBB`, per channel, alpha fixed at `1.0` — item
    /// tints are opaque multipliers in every vanilla case.
    fn expect_linear(rgb: u32) -> [f64; 4] {
        let ch = |shift: u32| srgb_eotf(f64::from((rgb >> shift) & 0xFF) / 255.0);
        [ch(16), ch(8), ch(0), 1.0]
    }

    fn icon(dyed_color: Option<u32>, potion_color: Option<u32>) -> ItemIcon {
        ItemIcon {
            item: ResourceLocation::parse("minecraft:stick").expect("static id parses"),
            count: 1,
            damage: None,
            max_damage: None,
            enchanted: false,
            custom_model_data: None,
            dyed_color,
            potion_color,
            banner_patterns: Vec::new(),
            base_color: None,
            skin: None,
        }
    }

    /// `minecraft:potion`'s tint source, `default` set to vanilla's
    /// `PotionContents.BASE_POTION_COLOR` (`-13_083_194`) — the same default
    /// every real `potion`/`splash_potion`/`lingering_potion`/`tipped_arrow`
    /// item definition carries.
    fn potion_layer() -> SpriteLayer {
        SpriteLayer {
            sprite: ResourceLocation::parse("minecraft:item/potion").expect("static id parses"),
            tint: Some(lodestone_assets::TintSource {
                kind: "minecraft:potion".to_string(),
                default: Some(-13_083_194),
                grass: None,
                index: 0,
            }),
        }
    }

    /// `minecraft:dye`'s tint source, `default` set to vanilla's
    /// `DyedItemColor.LEATHER_COLOR` (`-6_265_536`) — every vanilla `dye`
    /// item definition's own default.
    fn dye_layer() -> SpriteLayer {
        SpriteLayer {
            sprite: ResourceLocation::parse("minecraft:item/leather_helmet")
                .expect("static id parses"),
            tint: Some(lodestone_assets::TintSource {
                kind: "minecraft:dye".to_string(),
                default: Some(-6_265_536),
                grass: None,
                index: 0,
            }),
        }
    }

    /// The discriminating gate: six items whose expected pre-blend colour is
    /// known from vanilla's own constants, run through the *actual* function
    /// `draw_item_icon_counted`/`draw_item_icon_popped` call
    /// (`sprite_layer_tint`) rather than through `item_tint::resolve`
    /// directly — that already has its own coverage in `lodestone-assets` and
    /// would prove nothing about whether `ItemIcon`'s fields actually reach
    /// it. Mismatches are collected and asserted on together, never inside
    /// the loop — an `assert!` there would abort at the first failure and
    /// hide how many of the six sources were actually broken.
    #[test]
    fn potion_and_dye_components_reach_the_emitted_quad_colour() {
        let cases: Vec<(&str, SpriteLayer, ItemIcon, u32)> = vec![
            // `MobEffects.SPEED`'s particle colour. A single-effect potion's
            // mixed colour is the effect's own colour regardless of
            // amplifier (the weighted average has one term), so
            // `potion_color` is set directly rather than re-deriving the
            // mixing formula, which `lodestone-data`'s own tests already
            // cover.
            (
                "swiftness",
                potion_layer(),
                icon(None, Some(0xFF33_EBFF)),
                0x33_EBFF,
            ),
            // `MobEffects.INSTANT_DAMAGE`'s particle colour — more than 60
            // apart from swiftness on every channel (118, 134, 149; see
            // `potion_pair_is_discriminating_on_every_channel` below), so no
            // pairwise-equal fixture value can hide a transposition.
            (
                "strong_harming",
                potion_layer(),
                icon(None, Some(0xFFA9_656A)),
                0xA9_656A,
            ),
            // The control: no `potion_contents` at all resolves to
            // `PotionContents.BASE_POTION_COLOR`, proving this gate is not
            // merely "not the default" — it pins the *specific* default too.
            (
                "water_bottle_control",
                potion_layer(),
                icon(None, None),
                0x38_5DC6,
            ),
            (
                "dyed_leather_a",
                dye_layer(),
                icon(Some(0x11_2233), None),
                0x11_2233,
            ),
            (
                "dyed_leather_b",
                dye_layer(),
                icon(Some(0x88_99AA), None),
                0x88_99AA,
            ),
            // The dye control: an undyed leather item resolves to
            // `DyedItemColor.LEATHER_COLOR`, vanilla's own definition default.
            (
                "undyed_leather_default",
                dye_layer(),
                icon(None, None),
                0xA0_6540,
            ),
        ];

        let mut mismatches = Vec::new();
        for (name, layer, slot, expected_rgb) in &cases {
            let got = sprite_layer_tint(layer, slot);
            let want = expect_linear(*expected_rgb);
            for (i, w) in want.iter().enumerate().take(3) {
                let d = (f64::from(got[i]) - w).abs();
                if d >= 1e-4 {
                    mismatches.push(format!(
                        "{name} channel {i}: got {}, want {w} (expected rgb {expected_rgb:06X}), off by {d}",
                        got[i]
                    ));
                }
            }
            if (got[3] - 1.0).abs() >= 1e-6 {
                mismatches.push(format!("{name}: alpha is {}, not 1.0", got[3]));
            }
        }
        assert!(
            mismatches.is_empty(),
            "{} of {} channel checks failed:\n{}",
            mismatches.len(),
            cases.len() * 3,
            mismatches.join("\n")
        );
    }

    /// Control for the gate above: the swiftness/strong_harming pair really
    /// is more than 60 apart on every channel, so the gate could not pass by
    /// coincidence even with a badly wired context.
    #[test]
    fn potion_pair_is_discriminating_on_every_channel() {
        let a = [0x33i32, 0xEB, 0xFF];
        let b = [0xA9i32, 0x65, 0x6A];
        for i in 0..3 {
            let d = (a[i] - b[i]).abs();
            assert!(
                d > 60,
                "channel {i}: swiftness/strong_harming are only {d} apart, \
                 not enough to discriminate a wiring bug from coincidence"
            );
        }
    }

    /// The pre-fix behaviour, pinned as its own control: resolving every
    /// source against `ItemTintContext::default()` — the bug this whole
    /// module exists to catch — collapses all three potions and all three
    /// dyed items in [`potion_and_dye_components_reach_the_emitted_quad_colour`]
    /// down to the *same* default colour apiece, indistinguishable from one
    /// another. If a future refactor reintroduces `ItemTintContext::default()`
    /// at the call site, this is the assertion that proves the discriminating
    /// gate above would have caught it — the detector fires here.
    #[test]
    fn default_context_collapses_every_case_the_real_context_discriminates() {
        let default_ctx = ItemTintContext::default();
        let potion_default = lodestone_assets::item_tint::resolve(
            potion_layer().tint.as_ref().expect("layer carries a tint"),
            &default_ctx,
        )
        .expect("a source with a default always resolves")
        .rgb();
        let dye_default = lodestone_assets::item_tint::resolve(
            dye_layer().tint.as_ref().expect("layer carries a tint"),
            &default_ctx,
        )
        .expect("a source with a default always resolves")
        .rgb();

        // Every potion case would have collapsed to the same default —
        // including swiftness and strong_harming, which the real fix keeps
        // 118-149 apart per channel.
        assert_eq!(potion_default, 0x38_5DC6, "unneutered potion default drifted");
        assert_ne!(
            potion_default, 0x33_EBFF,
            "swiftness must differ from the neutered default for the gate above to mean anything"
        );
        assert_ne!(
            potion_default, 0xA9_656A,
            "strong_harming must differ from the neutered default for the gate above to mean anything"
        );

        // Same shape for dye.
        assert_eq!(dye_default, 0xA0_6540, "unneutered dye default drifted");
        assert_ne!(
            dye_default, 0x11_2233,
            "dyed_leather_a must differ from the neutered default for the gate above to mean anything"
        );
        assert_ne!(
            dye_default, 0x88_99AA,
            "dyed_leather_b must differ from the neutered default for the gate above to mean anything"
        );
    }
}
