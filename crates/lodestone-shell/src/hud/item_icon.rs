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
    BlockEntityModelSet, BlockModels, CHEST_SINGLE, CameraUniform, ChestHalf, ChestMaterial,
    EntityCameraUniform, EntityPipeline, GpuAtlas, GpuEntityModel, GuiSpriteQuad, ModelPipeline,
    ModelVertex, RenderLayer, chest_texture_stem, chest_texture_stems, entity_camera_buffer,
    fog::FogUniform, gui_item_pose, gui_ortho, mesh_item_quads, model_shared_camera_buffer,
    section_origin_buffer, update_model_shared_camera_buffer, upload_instances,
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
}

/// `ItemStack.hasFoil()` for a shell-side [`lodestone_game::item::ItemStack`],
/// delegating the actual predicate to
/// [`lodestone_render::glint::has_foil_enchantments`].
///
/// # Why this adapter exists
///
/// [`lodestone_render::glint::has_foil`] wants a
/// `lodestone_model::item::ItemComponents` — a struct with an `enchantments`
/// field. What the shell has in hand is `lodestone_game::item::ItemComponents`,
/// an opaque `BTreeMap<Identifier, ComponentValue>`, so that call does not
/// compile here and the two types are not interchangeable. This bridges them
/// without re-spelling the predicate, so the render crate stays the single owner
/// of *what foil means* and of the shortfalls documented there.
///
/// # The invariant this leans on
///
/// The key is present **only when the list is non-empty** —
/// `lodestone_game::item::ItemStack`'s `From<&lodestone_model::ItemStack>` inserts
/// `minecraft:enchantments` under an `if !stack.components.enchantments.is_empty()`
/// guard. So an absent key and a present-but-empty list mean the same thing here,
/// and the `is_empty` check inside `has_foil_enchantments` is what makes this
/// correct either way rather than dependent on that guard holding.
#[must_use]
pub(crate) fn stack_has_foil(stack: &lodestone_game::item::ItemStack) -> bool {
    match stack
        .components()
        .get_str(lodestone_game::item::ENCHANTMENTS_COMPONENT)
    {
        Some(lodestone_game::item::ComponentValue::Enchantments(list)) => {
            lodestone_render::glint::has_foil_enchantments(list)
        }
        _ => false,
    }
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

/// One special-renderer icon to draw: a baked block-entity mesh, the sheet it
/// samples, and where in GUI pixel space to put it.
///
/// `placement` goes straight into `BlockEntityMesh::part_transforms` in place of
/// the world's `block_entity_placement_matrix` — that substitution is the whole
/// seam between the world's chest and the one in your hand.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct SpecialIconDraw {
    /// The [`BlockEntityModelSet`] model name, e.g. [`CHEST_SINGLE`].
    pub model: &'static str,
    /// The jar texture stem, e.g. `entity/chest/normal`.
    pub texture: &'static str,
    /// GUI-pixel-space placement, from [`gui_item_pose`].
    pub placement: Mat4,
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
    if let Some(atlas) = assets.items
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
                            push_sprite_quad(sink.sprite, vw, vh, quad, sprite_layer_tint(layer));
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
                IconPart::Special { kind, .. } => {
                    push_special_icon(
                        sink.special,
                        &slot.item,
                        kind,
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
                            push_sprite_quad(sink.sprite, vw, vh, quad, sprite_layer_tint(layer));
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
                IconPart::Special { kind, .. } => {
                    push_special_icon(
                        sink.special,
                        &slot.item,
                        kind,
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
fn push_special_icon(
    out: &mut Vec<SpecialIconDraw>,
    item: &ResourceLocation,
    kind: &str,
    transform: &DisplayTransform,
    x: f32,
    y: f32,
    size: f32,
) {
    let Some((model, texture)) = special_icon_geometry(kind, item) else {
        return;
    };
    out.push(SpecialIconDraw {
        model,
        texture,
        placement: gui_item_pose([x, y, size, size], transform),
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
/// # What is not live yet
///
/// [`ItemTintContext`] is left at its default — no components, no colormap. That
/// is not a stub for `constant` (the majority source, which reads nothing) and it
/// is vanilla's own answer for an uncustomised stack: an undyed leather helmet's
/// brown *is* the definition's `dye` default. The one real miss is a **dyed**
/// stack, and it is a wire-side gap rather than a wiring one —
/// `lodestone_game::item::ItemComponents` has no `minecraft:dyed_color` member at
/// all (that crate defines no `DYED_COLOR_COMPONENT`), so the shell holds no live
/// dye to pass. `grass` resolves to `None` without a colormap, which
/// [`lodestone_assets::item_tint::resolve`] documents as the honest degradation.
fn sprite_layer_tint(layer: &SpriteLayer) -> [f32; 4] {
    let Some(source) = layer.tint.as_ref() else {
        return [1.0, 1.0, 1.0, 1.0];
    };
    let Some(resolved) = lodestone_assets::item_tint::resolve(source, &ItemTintContext::default())
    else {
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
}

/// One uploaded special-icon batch: the mesh and sheet to bind, one instance
/// buffer per part, and how many icons share them.
#[derive(Debug)]
struct SpecialBatch {
    model: &'static str,
    texture: &'static str,
    count: u32,
    parts: Vec<Option<wgpu::Buffer>>,
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

        let real = crate::resources::load_block_entity_textures();
        let mut textures = HashMap::new();
        for stem in chest_texture_stems() {
            let Some(img) = real.get(stem) else {
                // The loader already warned. A chest with no sheet draws nothing
                // rather than a magenta box, matching the world pass.
                continue;
            };
            let view = entity_sheet_texture(device, queue, img);
            textures.insert(stem, pipeline.texture_bind_group(device, &view, &sampler));
        }
        if textures.is_empty() {
            return None;
        }

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

        Some(SpecialIcons {
            pipeline,
            models,
            gpu_models,
            textures,
            cam_buffer,
            cam_bind_group,
            batches: Vec::new(),
            carried_batches: Vec::new(),
        })
    }

    /// How many sheets loaded — the counter that separates "no chest in a slot"
    /// from "no pack, so nothing can ever draw".
    fn sheet_count(&self) -> usize {
        self.textures.len()
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
fn gui_glint_uniform(speed: f64, strength: f32) -> GuiGlintUniform {
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
    /// Whether lazy construction has been attempted. Separate from
    /// `special.is_some()` so a jar-less run (where [`SpecialIcons::new`] returns
    /// `None`) does not re-decode nothing every frame forever.
    special_tried: bool,
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
            special_tried: false,
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
            bytemuck::bytes_of(&gui_glint_uniform(g_speed, g_strength)),
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
        }
        if special.is_empty() {
            return;
        }
        // Gated on the model pass, not on the format alone: `models_attached`
        // is the one signal both screens branch on, so a special icon can never
        // reach a frame the model pass was excluded from.
        let (Some(format), true) = (self.color_format, self.models.is_some()) else {
            return;
        };
        if !self.special_tried {
            self.special_tried = true;
            self.special = SpecialIcons::new(device, queue, format);
        }
        let Some(s) = self.special.as_mut() else {
            return;
        };

        let base = build_special_batches(device, s, &special[..carried_from]);
        let carried = build_special_batches(device, s, &special[carried_from..]);
        s.batches = base;
        s.carried_batches = carried;

        if !s.batches.is_empty() || !s.carried_batches.is_empty() {
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
        let mut keys: Vec<(&'static str, &'static str)> = Vec::new();
        let mut per_key: Vec<Vec<Mat4>> = Vec::new();
        for draw in special {
            let Some(mesh) = s.models.get(draw.model) else {
                continue;
            };
            if !s.textures.contains_key(draw.texture) {
                continue;
            }
            let transforms = mesh.part_transforms(draw.placement, &[]);
            match keys.iter().position(|k| *k == (draw.model, draw.texture)) {
                Some(i) => per_key[i].extend(transforms),
                None => {
                    keys.push((draw.model, draw.texture));
                    per_key.push(transforms);
                }
            }
        }

        for ((model, texture), flat) in keys.into_iter().zip(per_key) {
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
            let parts = (0..part_count)
                .map(|p| {
                    let per_icon: Vec<Mat4> = (0..count)
                        .map(|icon| flat[icon * part_count + p])
                        .collect();
                    // No `lights`: an inventory slot has no world light to
                    // sample, so `upload_instances` falls back to
                    // `ENTITY_FULLBRIGHT` for every instance.
                    upload_instances(device, &per_icon, &[])
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
        if count == 0 && specials.is_empty() {
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
                let Some(texture) = s.textures.get(batch.texture) else {
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
    }
}

#[cfg(test)]
mod pop_tests {
    use super::{ResourceLocation, SpriteLayer, pop_squeeze_rect, sprite_layer_tint, stack_has_foil};

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
        let tint = sprite_layer_tint(&constant_layer(0xFF71_C35C));

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
        assert_eq!(sprite_layer_tint(&layer), [1.0, 1.0, 1.0, 1.0]);
    }

    /// White (`0xFFFFFF`) is the conversion's fixed point, so a fully-bright
    /// constant tint must also come out as exact white. This separates "converts"
    /// from "darkens everything": a wrong conversion applied to white would
    /// show up here, and nowhere else.
    #[test]
    fn a_white_constant_tint_is_the_conversions_fixed_point() {
        let tint = sprite_layer_tint(&constant_layer(0xFFFF_FFFF));
        for (i, c) in tint.iter().enumerate() {
            assert!(
                (c - 1.0).abs() < 1e-6,
                "channel {i} of a white tint is {c}, not 1.0"
            );
        }
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
