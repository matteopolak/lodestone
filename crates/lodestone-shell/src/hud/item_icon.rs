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
// count's vertical anchor was derived from. Issue #384 replaced that derivation
// with vanilla's own constant (`COUNT_TOP`), so nothing in this module needs the
// font's metrics any more — see `COUNT_RIGHT`'s doc comment for why a derived
// anchor was the defect rather than the off-by-one.
use lodestone_assets::{
    Atlas, DisplaySlot, DisplayTransform, IconPart, ItemAtlas, ResourceLocation,
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
use super::{FLOATS_PER_VERTEX, HUD_SPRITE_WGSL, SPRITE_FLOATS_PER_VERTEX};

/// One occupied slot's drawable state, resolved shell-side from a
/// [`lodestone_game::menu::Menu`]. `item` is the item id
/// (`minecraft:diamond_pickaxe`) used to look up the resolved icon in the
/// [`ItemAtlas`]; `count` drives the stack number; `damage`/`max_damage` drive
/// the durability bar; `enchanted` marks items that should get the glint overlay
/// (deferred).
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
    /// Whether the stack is enchanted (glint overlay is deferred; see notes).
    pub enchanted: bool,
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
/// # Which kinds draw today
///
/// Only `minecraft:chest`, because it is the only block-entity model **#23 has
/// ported** — `BLOCK_ENTITY_MODELS` holds the three chest layers and nothing
/// else. The remaining nine return `None` and keep drawing nothing, which is the
/// pre-existing behaviour and *not* a regression; they become one match arm each
/// the day their geometry lands, with no change to any of the wiring below. See
/// `docs/gui-item-icons.md` for the table.
///
/// `None` is also the right answer for an id a `kind` does not recognise (a
/// datapack item declaring `minecraft:chest` over some other block): drawing
/// nothing beats drawing a plain oak chest for something that is not one.
fn special_icon_geometry(kind: &str, item: &ResourceLocation) -> Option<(&'static str, &'static str)> {
    match kind {
        "minecraft:chest" => {
            let material = ChestMaterial::from_block_path(item.path())?;
            // `ChestHalf::Single` is not a simplification: `ChestSpecialRenderer
            // .Unbaked`'s `chest_type` defaults to `ChestType.SINGLE`, and the
            // 26.2 item definitions never override it. The two double halves are
            // 15 texels wide against the single's 14 and each omits the face
            // meeting its partner, so they are separate meshes reachable only
            // from a placed block.
            Some((
                CHEST_SINGLE,
                chest_texture_stem(material, ChestHalf::Single),
            ))
        }
        _ => None,
    }
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
                        // Tint resolution (leather/potion/spawn-egg dyes,
                        // foliage) is deferred; untinted white is correct for
                        // the vast majority of items.
                        if let Some(spr) = atlas.sprite(&layer.sprite) {
                            push_sprite_quad(
                                sink.sprite,
                                vw,
                                vh,
                                GuiSpriteQuad {
                                    dst: [x, y, size, size],
                                    uv_min: spr.uv_min,
                                    uv_max: spr.uv_max,
                                },
                                [1.0, 1.0, 1.0, 1.0],
                            );
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

/// Where a stack count's **right edge** sits, in slot-local GUI pixels
/// (issue #384).
///
/// `GuiGraphicsExtractor.itemCount` (`:947-952`, and identically
/// `SpectatorGui.java:79`):
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
    pub(crate) fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        let advance = (font::GLYPH_W as f32 + 1.0) * scale;
        let mut cursor = x;
        for ch in s.chars() {
            self.glyph(ch, cursor, y, scale, c);
            cursor += advance;
        }
    }
}

/// Width in pixels of `s` at `scale` in the shell's fixed-width bitmap font.
/// Only correct when no vanilla font is attached — every layout site goes
/// through `Builder::text_width`, which picks this or the proportional
/// measure to match whichever font `Builder::text` will actually draw with.
pub(crate) fn text_w(s: &str, scale: f32) -> f32 {
    s.chars().count() as f32 * (font::GLYPH_W as f32 + 1.0) * scale
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

/// The GPU half of the item-icon pass, held by every screen that draws slots.
///
/// Both halves start detached. `attach_items` gives flat icons somewhere to
/// draw; `attach_item_models` gives block icons somewhere to draw. Neither is
/// required, and a renderer with neither draws no icons at all — the jar-less
/// runtime behaviour and the executed negative control in both pixel gates.
#[derive(Debug, Default)]
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
}

impl IconRenderer {
    /// A detached renderer: no atlas, no model pass, no icons.
    pub(crate) fn new() -> Self {
        Self::default()
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
/// (`AbstractContainerScreen.java:126`), which is called immediately before the
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
