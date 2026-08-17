//! [`ContainerGeometry`] — one frame of the container screen, folded into the
//! four vertex streams, plus the anvil/enchanting cost lines and the paint-drag
//! preview.
//!
//! Split out of `container.rs` verbatim.

use lodestone_assets::ItemAtlas;
use lodestone_game::item::ItemStack;
use lodestone_game::menu::{Menu, MenuKind, SpecialLayout};
use lodestone_render::{BlockModels, ModelVertex};

use crate::hud::VanillaFont;
use crate::hud::item_icon::{self, IconAssets, SpecialIconDraw};

use super::background::ContainerBackground;
use super::builder::Builder;
use super::frame::{ContainerFrame, LabelLayout, label_layout, menu_type_title_anchor};
use super::layout::{MenuHit, Rect, SlotLayout, hit_test_with_book, panel_origin_with_scale, slot_layout};
use super::player_preview::PlayerAvatar;
use super::{
    BG_FLOATS_PER_VERTEX, BLAST_FURNACE_BURN_PROGRESS, BLAST_FURNACE_LIT_PROGRESS,
    BREWING_BREW_PROGRESS, BREWING_BUBBLES, BREWING_FUEL_LENGTH, CELL, FLOATS_PER_VERTEX,
    FURNACE_BURN_PROGRESS, FURNACE_LIT_PROGRESS, HIGHLIGHT, HIGHLIGHT_INSET,
    MERCHANT_DISCOUNT_STRIKETHROUGH, MERCHANT_OUT_OF_STOCK, MERCHANT_TRADE_ARROW,
    MERCHANT_TRADE_ARROW_OUT_OF_STOCK, SLOT, SLOT_HIGHLIGHT_BACK, SLOT_HIGHLIGHT_FRONT,
    SMOKER_BURN_PROGRESS, SMOKER_LIT_PROGRESS,
};

/// Declared (16x-baseline) full-sprite size the furnace family's lit-flame
/// sub-rect is authored against — `AbstractFurnaceScreen.java`'s
/// `blitSprite(pipeline, litProgressSprite, 14, 14, 0, 14 - h, x, y, 14, h)`.
/// Shared by all three furnace variants: `FurnaceScreen`/`BlastFurnaceScreen`/
/// `SmokerScreen` all reuse `AbstractFurnaceScreen.extractBackground`, only
/// the sprite id differs. Needed by
/// [`ContainerBackground::sprite_subregion_quad`] to rescale the sub-rect for
/// a resource pack whose real pixels exceed this baseline — issue #582.
const FURNACE_LIT_DECLARED: (f32, f32) = (14.0, 14.0);
/// As [`FURNACE_LIT_DECLARED`], for the burn-progress bar —
/// `blitSprite(pipeline, burnProgressSprite, 24, 16, 0, 0, x, y, w, 16)`.
const FURNACE_BURN_DECLARED: (f32, f32) = (24.0, 16.0);
/// The brewing stand's fuel-length bar's declared size —
/// `BrewingStandScreen.java`'s
/// `blitSprite(pipeline, FUEL_LENGTH_SPRITE, 18, 4, 0, 0, x, y, len, 4)`.
const BREWING_FUEL_DECLARED: (f32, f32) = (18.0, 4.0);
/// The brewing stand's brew-progress bar's declared size —
/// `blitSprite(pipeline, BREW_PROGRESS_SPRITE, 9, 28, 0, 0, x, y, 9, len)`.
const BREWING_BREW_DECLARED: (f32, f32) = (9.0, 28.0);
/// The brewing stand's bubble-column's declared size —
/// `blitSprite(pipeline, BUBBLES_SPRITE, 12, 29, 0, 29 - len, x, y, 12, len)`.
const BREWING_BUBBLES_DECLARED: (f32, f32) = (12.0, 29.0);

/// `AnvilScreen.extractBackground`'s own blit:
/// `graphics.blitSprite(pipeline, hasItem ? TEXT_FIELD_SPRITE :
/// TEXT_FIELD_DISABLED_SPRITE, leftPos + 59, topPos + 20, 110, 16)`. Neither
/// `container/anvil/text_field` nor `text_field_disabled` is one of the
/// whole-panel sheets [`ContainerBackground`] stitches (it only loads
/// `gui/container/*.png` sheets, not `gui/sprites/container/anvil/**`), so
/// this crate cannot yet sample the real 9-sliced sprite from here without
/// also touching that loader. Vanilla's own `anvil.png`, at exactly this
/// rect, is a flat, fully opaque `(255, 0, 0)` placeholder (measured:
/// 1760/1760 = 110×16 pixels), which is what shows through — a solid red
/// box — when nothing draws over it.
const ANVIL_FIELD_X: f32 = 59.0;
/// See [`ANVIL_FIELD_X`].
const ANVIL_FIELD_Y: f32 = 20.0;
/// See [`ANVIL_FIELD_X`].
const ANVIL_FIELD_W: f32 = 110.0;
/// See [`ANVIL_FIELD_X`].
const ANVIL_FIELD_H: f32 = 16.0;
/// The border colour both `text_field.png` and `text_field_disabled.png`
/// share — measured as their most common non-fill, non-highlight pixel
/// (`(55, 55, 55)`, 123 of 1760 texels in each real sprite).
const ANVIL_FIELD_BORDER: [f32; 4] = [55.0 / 255.0, 55.0 / 255.0, 55.0 / 255.0, 1.0];
/// The enabled interior fill — `text_field.png`'s dominant pixel `(160, 145,
/// 114)` (1274 of 1760 texels, measured).
const ANVIL_FIELD_FILL: [f32; 4] = [160.0 / 255.0, 145.0 / 255.0, 114.0 / 255.0, 1.0];
/// The disabled interior fill — `text_field_disabled.png`'s dominant pixel
/// `(78, 71, 55)` (1274 of 1760 texels, measured).
const ANVIL_FIELD_FILL_DISABLED: [f32; 4] = [78.0 / 255.0, 71.0 / 255.0, 55.0 / 255.0, 1.0];
/// `EditBox::setTextColor(-1)` (`AnvilScreen.subInit`) — opaque white.
const ANVIL_FIELD_TEXT: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// Geometry for the container overlay: coloured chrome plus, when an item atlas
/// is attached, real slot icons on the two icon streams.
#[derive(Debug, Clone, PartialEq)]
pub struct ContainerGeometry {
    /// Flat `[x, y, r, g, b, a]` per vertex, with positions in NDC. Panel,
    /// slot wells, title, stack counts and durability bars.
    pub verts: Vec<f32>,
    /// Flat `[x, y, u, v, r, g, b, a]` per textured **item**-sprite vertex,
    /// sampling the [`ItemAtlas`]. Empty unless one was supplied.
    pub item_verts: Vec<f32>,
    /// The **enchantment-glint** copies of [`item_verts`](Self::item_verts) —
    /// see [`crate::hud::HudGeometry::glint_verts`]. Split at
    /// [`slot_glint_vertex_count`](Self::slot_glint_vertex_count) for the same
    /// reason the sprite stream is.
    pub glint_verts: Vec<f32>,
    /// The 3-D **block-item** icons, already posed into GUI pixel space on the
    /// CPU. Empty unless a [`BlockModels`] was supplied.
    pub model_verts: Vec<ModelVertex>,
    /// The **special-renderer** icons (chest, and the rest of the ex-
    /// `builtin/entity` family as their geometry lands): the baked block-entity
    /// mesh and sheet to draw plus a GUI-space placement, not vertices. See
    /// [`crate::hud::HudGeometry::special`] — the two screens carry the same
    /// stream because they share one `draw_item_icon`.
    pub(crate) special: Vec<SpecialIconDraw>,
    /// Flat `[x, y, u, v, r, g, b, a]` per vertex sampling
    /// [`ContainerBackground`]'s atlas — vanilla's real `container/*.png` panel
    /// art. Empty unless a background was supplied; drawn on its
    /// own pipeline (a different atlas than [`item_verts`](Self::item_verts))
    /// in its own pass, **before** the chrome pass, so the panel/well fills
    /// this stream would otherwise draw are suppressed in favour of the real
    /// art's own baked-in slot wells.
    pub bg_verts: Vec<f32>,
    /// A **third**, independent tier of textured `bg_verts`-shaped content —
    /// same `[x, y, u, v, r, g, b, a]` layout, same atlas — that draws in its
    /// own pass strictly between [`bg_verts`](Self::bg_verts)'s existing
    /// two-pass sequence and the carried tier: after the "chrome" colour pass,
    /// after this same pass's own [`mid_item_verts`](Self::mid_item_verts)/
    /// [`mid_verts`](Self::mid_verts) sibling, and before both
    /// [`bg_verts`](Self::bg_verts)'s "front" range and
    /// [`dim2_verts`](Self::dim2_verts). Advancements' widget frames are the
    /// one populated caller — see `menu::advancements`'s module doc for why a
    /// widget's frame has to draw later than every existing tier lets it, and
    /// why that is not physically the same constraint as
    /// [`bg_slot_vertex_count`](Self::bg_slot_vertex_count)'s split. Always
    /// empty for every other caller, which is what keeps the renderer's new
    /// pass a verified no-op for the container screens, the creative menu and
    /// the recipe panel.
    pub mid_bg_verts: Vec<f32>,
    /// [`mid_bg_verts`](Self::mid_bg_verts)'s plain-colour analogue: flat
    /// `[x, y, r, g, b, a]` vertices, drawn through the same untextured
    /// pipeline [`verts`](Self::verts) uses, in the same new pass as
    /// [`mid_bg_verts`](Self::mid_bg_verts) — the jar-less fallback for a
    /// widget's frame (no [`ContainerBackground`] attached) and a widget's own
    /// atlas-less icon swatch both land here. Always empty for every other
    /// caller.
    pub mid_verts: Vec<f32>,
    /// [`mid_bg_verts`](Self::mid_bg_verts)'s analogue on the flat **item**
    /// sprite stream ([`item_verts`](Self::item_verts)'s own atlas): a
    /// widget's own icon, when it resolves to a flat sprite. Always empty for
    /// every other caller. There is deliberately **no** `mid_model_verts` or
    /// `mid_special` sibling — [`IconStratum`](crate::hud::item_icon::IconStratum)
    /// has exactly two variants, `Slots` and `Carried`, and lives outside this
    /// mechanism's file ownership, so a widget icon backed by a 3-D block
    /// model or a special-renderer icon (a chest) has nowhere to move and
    /// stays in the ordinary carried tier — undimmed by the hover-dim, a
    /// documented, narrower gap than the one this field closes.
    pub mid_item_verts: Vec<f32>,
    /// [`mid_item_verts`](Self::mid_item_verts)'s enchantment-glint copy,
    /// mirroring [`glint_verts`](Self::glint_verts). Always empty for every
    /// other caller.
    pub mid_glint_verts: Vec<f32>,
    /// Plain `[x, y, r, g, b, a]` vertices for a translucent overlay drawn in
    /// its **own** pass, positioned strictly after
    /// [`mid_bg_verts`](Self::mid_bg_verts)/[`mid_verts`](Self::mid_verts)/
    /// [`mid_item_verts`](Self::mid_item_verts)/
    /// [`mid_glint_verts`](Self::mid_glint_verts) and before
    /// [`bg_verts`](Self::bg_verts)'s "front" range and the carried tier —
    /// Advancements' hover-dim (`AdvancementsView::fade`), which needs
    /// somewhere to land that darkens a widget's own frame and icon without
    /// also darkening the hover tooltip, drawn later still. Always empty for
    /// every other caller, which is what keeps this pass a verified no-op
    /// everywhere but the Advancements screen.
    pub dim2_verts: Vec<f32>,
    /// How many leading vertices of [`bg_verts`](Self::bg_verts) draw **under**
    /// the slot items: the panel art, the hover highlight's *back* sprite, and
    /// the empty-slot placeholders. The remainder is the highlight's *front*
    /// sprite, which vanilla draws after every slot
    /// (`extractSlotHighlightFront`, `AbstractContainerScreen.java`) and
    /// before `extractCarriedItem`'s `nextStratum()`.
    ///
    /// A caller drawing all of `bg_verts` in one pass loses the front sprite's
    /// whole purpose — it would sit under the item it is supposed to frame, which
    /// looks *almost* right and is why the split is recorded rather than assumed.
    /// Equal to the full length whenever nothing is hovered.
    pub bg_slot_vertex_count: usize,
    /// How many leading vertices of [`verts`](Self::verts) are the full-canvas
    /// dim gradient (vanilla's `extractTransparentBackground`, see
    /// [`Builder::gradient_rect_px`]). This has to draw in its own pass
    /// **before** [`bg_verts`](Self::bg_verts): the dim sits *under* the real
    /// panel art (vanilla's own `container/*.png` blit is the next thing
    /// drawn after its dim, not the other way around), while everything else
    /// in `verts` past this marker — the flat-fill fallback, the title, the
    /// wells — belongs *on top of* the panel art. A caller ignoring this and
    /// drawing all of `verts` as one "chrome" range would either dim the panel
    /// texture itself or draw the panel texture over an undimmed screen,
    /// depending on which pass it sandwiched the texture into.
    pub dim_vertex_count: usize,
    /// How many leading vertices of [`verts`](Self::verts) are *chrome* — the
    /// panel, the title and the slot wells. The remainder (stack counts,
    /// durability bars, the atlas-less swatch fallback) belongs **on top of**
    /// the icons, so the renderer draws this stream in two ranges with the icon
    /// passes in between.
    pub chrome_vertex_count: usize,
    /// How many leading vertices of [`verts`](Self::verts) belong to the **slot**
    /// stratum, i.e. everything except the carried stack's own count and
    /// durability bar. The remainder is drawn last, above the carried stack's
    /// icon.
    ///
    /// Equal to [`vertex_count`](Self::vertex_count) when nothing is carried, so
    /// the fourth range is simply empty.
    pub slot_vertex_count: usize,
    /// How many leading vertices of [`item_verts`](Self::item_verts) are slot
    /// icons; the remainder is the carried stack's flat sprite. See
    /// [`slot_vertex_count`](Self::slot_vertex_count).
    pub slot_item_vertex_count: usize,
    /// How many leading vertices of [`glint_verts`](Self::glint_verts) are slot
    /// icons; the remainder belongs to the carried stack. Without this split an
    /// enchanted stack on the cursor would have its glint drawn in the slot pass
    /// and then covered by its own sprite in the carried pass — invisible.
    pub slot_glint_vertex_count: usize,
    /// How many leading vertices of [`model_verts`](Self::model_verts) are slot
    /// icons; the remainder is the carried stack's 3-D block. **This one is not
    /// an ordering nicety** — the model pass is depth-tested, so a carried block
    /// has to be drawn in a pass that clears depth again or a slot block's near
    /// faces win over it. See [`crate::hud::item_icon::IconStratum`].
    pub slot_model_vertex_count: usize,
    /// How many leading entries of `special` are slot icons; the remainder is a
    /// carried block-entity item (a chest on the cursor).
    pub(crate) slot_special_count: usize,
    /// Rect covered by the widget, if anything was drawn — in the **logical**
    /// GUI canvas (physical `width`/`height` divided by the effective GUI
    /// scale, matching [`panel_origin`]), not raw physical pixels. A caller
    /// comparing this against a physical-pixel target (a screenshot, a
    /// framebuffer readback) must scale it up first, the same way [`hit_test`]
    /// scales a physical cursor position down before comparing the other way.
    pub widget_rect: Option<Rect>,
    /// The **inventory avatar**: where the player rig is drawn and where it is
    /// looking, or `None` on every screen that is not the player's own inventory.
    ///
    /// `Some` exactly when [`MenuKind::Player`] — vanilla calls
    /// `extractEntityInInventoryFollowsMouse` only from
    /// `InventoryScreen.extractBackground`, so a chest or a furnace has no avatar
    /// and drawing one there would be a divergence, not a bonus.
    ///
    /// This is a *placement*, not a vertex stream: the rig is 3-D and goes through
    /// `EntityPipeline` in its own pass, exactly as the special block-entity icons
    /// do. It carries the rect and the cursor in the **logical** canvas, like
    /// [`widget_rect`](Self::widget_rect), and it is derived from the same shifted
    /// panel origin every slot is — so an open recipe book moves the avatar with
    /// the panel for free.
    pub player_avatar: Option<PlayerAvatar>,
}

impl ContainerGeometry {
    /// Number of coloured vertices.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.verts.len() / FLOATS_PER_VERTEX
    }

    /// Builds container overlay geometry for a viewport, with no item atlas: the
    /// slot contents fall back to a colour swatch and a letter. This is the
    /// jar-less / headless path, and the negative control the pixel gate
    /// exercises.
    #[must_use]
    pub fn build(frame: &ContainerFrame<'_>, width: u32, height: u32) -> Self {
        Self::build_inner(
            frame,
            width,
            height,
            crate::config::AUTO_GUI_SCALE,
            &IconAssets {
                items: None,
                models: None,
            },
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn build_inner(
        frame: &ContainerFrame<'_>,
        width: u32,
        height: u32,
        gui_scale: u32,
        assets: &IconAssets<'_>,
        font: Option<&VanillaFont>,
        background: Option<&ContainerBackground>,
    ) -> Self {
        let Some(menu) = frame.menu else {
            return Self {
                verts: Vec::new(),
                item_verts: Vec::new(),
                glint_verts: Vec::new(),
                model_verts: Vec::new(),
                special: Vec::new(),
                bg_verts: Vec::new(),
                mid_bg_verts: Vec::new(),
                mid_verts: Vec::new(),
                mid_item_verts: Vec::new(),
                mid_glint_verts: Vec::new(),
                dim2_verts: Vec::new(),
                bg_slot_vertex_count: 0,
                dim_vertex_count: 0,
                chrome_vertex_count: 0,
                slot_vertex_count: 0,
                slot_item_vertex_count: 0,
                slot_glint_vertex_count: 0,
                slot_model_vertex_count: 0,
                slot_special_count: 0,
                widget_rect: None,
                player_avatar: None,
            };
        };
        let layout = slot_layout(menu);
        // `width`/`height` are the physical framebuffer; divide down to the
        // logical canvas the same way `menu/render.rs` and `crate::hud` do, so
        // the panel and its slots come out the same *visual* size at any DPI
        // instead of shrinking as the physical framebuffer grows. `panel_origin`
        // performs the identical division for the widget's origin — see its own
        // doc comment — so the two agree on what "the canvas" is by construction
        // rather than by coincidence.
        let (w, h) = crate::menu::render::logical_canvas(gui_scale, width, height);
        let (x, y) = panel_origin_with_scale(&layout, gui_scale, width, height);
        // An open recipe book moves the whole panel right — vanilla's
        // `updateScreenPosition`. Applied **here**, at the single origin every
        // slot, label, sprite and cost readout below is measured from, so nothing
        // downstream needs to know about it. Zero with the book closed, which is
        // every existing caller. `hit_test_with_book` adds the same delta.
        let x = x + super::layout::recipe_book_panel_shift(w, layout.width, frame.book_open);
        let mut b = Builder::new(w, h, font);

        // Vanilla's own dim behind an open container screen (that fix's
        // leftover). `AbstractContainerScreen::isInGameUi()` overrides `true`
        // (`AbstractContainerScreen.java`), which routes
        // `Screen::extractBackground` to `extractTransparentBackground`
        // (`Screen.java`) — a full-canvas vertical **gradient**, not the
        // pause menu's tiled dirt texture (that is the `else` branch, for
        // `isInGameUi() == false` screens). `-1072689136`/`-804253680` decoded:
        // ARGB (192,16,16,16) top to (208,16,16,16) bottom.
        //
        // This is what dims the HUD hotbar for free: the HUD draws unconditionally
        // behind any world-following screen (that fix's `hud_follows_world`),
        // and `app.rs` now draws this container pass *after* the HUD pass, so
        // this gradient paints straight over it — draw order, not a per-element
        // alpha (see `docs/container-screen.md`).
        b.gradient_rect_px(
            0.0,
            0.0,
            w,
            h,
            [16.0 / 255.0, 16.0 / 255.0, 16.0 / 255.0, 192.0 / 255.0],
            [16.0 / 255.0, 16.0 / 255.0, 16.0 / 255.0, 208.0 / 255.0],
        );
        let dim_floats = b.verts.len();

        // Vanilla's real `container/*.png` art, if attached. `None`
        // degrades to the flat programmatic panel this screen has always drawn
        // — the jar-less path and the negative control the pixel gate leans on.
        let bg_quads = background.and_then(|bg| bg.quads(menu, x, y));
        if let Some(quads) = &bg_quads {
            for q in quads {
                b.bg_sprite(*q);
            }
        } else {
            b.rect_px(
                x,
                y,
                layout.width,
                layout.height,
                [0.08, 0.075, 0.065, 0.88],
            );
            b.rect_px(
                x + 3.0,
                y + 3.0,
                layout.width - 6.0,
                layout.height - 6.0,
                [0.22, 0.20, 0.17, 0.70],
            );
        }
        // The furnace family's lit-flame and burn-progress bars, and the
        // brewing stand's fuel/brew/bubble bars. Vanilla draws
        // both in `extractBackground`, immediately after the panel blit and
        // before any slot content, so they belong in this same "under
        // items" bucket — gated on a background being attached for the same
        // reason the highlight pair above is (no sprite to draw without
        // one). The properties come straight off `frame.cost_data`, which is
        // `OpenMenuSnapshot::data` — the same `container_set_data` feed the
        // anvil/enchanting cost lines already read, so no new wiring is
        // needed to reach it from `app.rs`.
        if let Some(bg) = background {
            let data = |property: i32| -> i32 {
                frame
                    .cost_data
                    .iter()
                    .find(|(p, _)| *p == property)
                    .map_or(0, |(_, v)| *v)
            };
            match menu.special_layout() {
                Some(
                    kind @ (SpecialLayout::Furnace
                    | SpecialLayout::BlastFurnace
                    | SpecialLayout::Smoker),
                ) => {
                    // `AbstractFurnaceMenu`: data[0] litTime, data[1]
                    // litDuration, data[2] cookingProgress, data[3]
                    // cookingTotalTime (`AbstractFurnaceMenu.java`).
                    let (lit_sprite, burn_sprite) = match kind {
                        SpecialLayout::BlastFurnace => {
                            (BLAST_FURNACE_LIT_PROGRESS, BLAST_FURNACE_BURN_PROGRESS)
                        }
                        SpecialLayout::Smoker => (SMOKER_LIT_PROGRESS, SMOKER_BURN_PROGRESS),
                        _ => (FURNACE_LIT_PROGRESS, FURNACE_BURN_PROGRESS),
                    };
                    let lit_time = data(0);
                    if lit_time > 0 {
                        let lit_duration = if data(1) == 0 { 200 } else { data(1) };
                        let lit_progress =
                            (lit_time as f32 / lit_duration as f32).clamp(0.0, 1.0);
                        // `Mth.ceil(litProgress * 13.0F) + 1` (`:55`), which
                        // is `1..=14` for `litProgress` in `0.0..=1.0`.
                        let lit_h = (lit_progress * 13.0).ceil() + 1.0;
                        if let Some(q) = bg.sprite_subregion_quad(
                            lit_sprite,
                            FURNACE_LIT_DECLARED,
                            [0.0, 14.0 - lit_h, 14.0, lit_h],
                            [x + 56.0, y + 36.0 + 14.0 - lit_h, 14.0, lit_h],
                        ) {
                            b.bg_sprite(q);
                        }
                    }
                    let (cooking_progress, cooking_total) = (data(2), data(3));
                    let burn_progress = if cooking_total != 0 && cooking_progress != 0 {
                        (cooking_progress as f32 / cooking_total as f32).clamp(0.0, 1.0)
                    } else {
                        0.0
                    };
                    let burn_w = (burn_progress * 24.0).ceil();
                    if burn_w > 0.0
                        && let Some(q) = bg.sprite_subregion_quad(
                            burn_sprite,
                            FURNACE_BURN_DECLARED,
                            [0.0, 0.0, burn_w, 16.0],
                            [x + 79.0, y + 34.0, burn_w, 16.0],
                        )
                    {
                        b.bg_sprite(q);
                    }
                }
                Some(SpecialLayout::Brewing) => {
                    // `BrewingStandMenu`: data[0] brewingTicks, data[1] fuel
                    // (`BrewingStandMenu.java`).
                    let fuel = data(1);
                    let fuel_len = ((18 * fuel + 19) / 20).clamp(0, 18);
                    if fuel_len > 0
                        && let Some(q) = bg.sprite_subregion_quad(
                            BREWING_FUEL_LENGTH,
                            BREWING_FUEL_DECLARED,
                            [0.0, 0.0, fuel_len as f32, 4.0],
                            [x + 60.0, y + 44.0, fuel_len as f32, 4.0],
                        )
                    {
                        b.bg_sprite(q);
                    }
                    let tick_count = data(0);
                    if tick_count > 0 {
                        let brew_len = (28.0 * (1.0 - tick_count as f32 / 400.0)) as i32;
                        if brew_len > 0
                            && let Some(q) = bg.sprite_subregion_quad(
                                BREWING_BREW_PROGRESS,
                                BREWING_BREW_DECLARED,
                                [0.0, 0.0, 9.0, brew_len as f32],
                                [x + 97.0, y + 16.0, 9.0, brew_len as f32],
                            )
                        {
                            b.bg_sprite(q);
                        }
                        const BUBBLE_LENGTHS: [i32; 7] = [29, 24, 20, 16, 11, 6, 0];
                        let bubble_len = BUBBLE_LENGTHS[(tick_count / 2 % 7) as usize];
                        if bubble_len > 0
                            && let Some(q) = bg.sprite_subregion_quad(
                                BREWING_BUBBLES,
                                BREWING_BUBBLES_DECLARED,
                                [0.0, (29 - bubble_len) as f32, 12.0, bubble_len as f32],
                                [
                                    x + 63.0,
                                    y + 14.0 + (29 - bubble_len) as f32,
                                    12.0,
                                    bubble_len as f32,
                                ],
                            )
                        {
                            b.bg_sprite(q);
                        }
                    }
                }
                Some(SpecialLayout::Anvil) => {
                    // Cover vanilla's own red placeholder rect (see
                    // `ANVIL_FIELD_X`'s doc) with a flat approximation of the
                    // real `container/anvil/text_field[_disabled]` sprite —
                    // not the real 9-sliced bevel (this crate has no path to
                    // that sprite from here; see the doc comment above), but
                    // enough that a solid red box no longer shows through.
                    // `AnvilScreen.subInit`: `this.name.setEditable(this.menu
                    // .getSlot(0).hasItem())`, and `extractBackground` blits
                    // the disabled sprite variant on the same condition.
                    let has_item = menu.slot_item(0).is_some();
                    let fill = if has_item {
                        ANVIL_FIELD_FILL
                    } else {
                        ANVIL_FIELD_FILL_DISABLED
                    };
                    b.rect_px(
                        x + ANVIL_FIELD_X,
                        y + ANVIL_FIELD_Y,
                        ANVIL_FIELD_W,
                        ANVIL_FIELD_H,
                        ANVIL_FIELD_BORDER,
                    );
                    b.rect_px(
                        x + ANVIL_FIELD_X + 1.0,
                        y + ANVIL_FIELD_Y + 1.0,
                        ANVIL_FIELD_W - 2.0,
                        ANVIL_FIELD_H - 2.0,
                        fill,
                    );
                    // `new EditBox(font, xo + 62, yo + 24, 103, 12, ...)`
                    // (`AnvilScreen.subInit`), `setBordered(false)`: the
                    // inset a bordered box would add (`EditBox.java`:
                    // `this.bordered ? 4 : 0`) is zero, so text sits flush
                    // with the box's own x. `textY = getY() + (height - 8) /
                    // 2` centres it in the 12px-tall box. `textShadow`
                    // defaults `true` and `AnvilScreen` never clears it, so
                    // this is `shadowed_label`, not `label`.
                    if let Some(name) = frame.anvil_name
                        && !name.is_empty()
                    {
                        b.shadowed_label(name, x + 62.0, y + 26.0, 1.0, ANVIL_FIELD_TEXT);
                    }
                }
                _ => {}
            }
        }
        // Which slot the pointer is over — vanilla's `hoveredSlot`, set from
        // `getHoveredSlot(mouseX, mouseY)` every frame
        // (`AbstractContainerScreen.java`). Derived from the **same**
        // `hit_test_with_book` the click path calls, with the same `gui_scale`
        // *and* the same `book_open`, so the highlight cannot land on a different
        // slot than a click would — the failure mode `hit_test_with_scale`'s own
        // doc comment warns about, here made impossible by construction rather
        // than by matching two constants.
        //
        // **This used to call `hit_test_with_scale`, which is
        // `hit_test_with_book(…, false)`**, and that was the reported defect: the
        // draw above shifts the whole panel right by `recipe_book_panel_shift`
        // when the book is open, so an *unshifted* hover resolved the cursor
        // against slot rects one shift to the left of where they were drawn. The
        // visible symptom was hovering the open recipe book and watching an
        // inventory slot light up — the slot that *would* have been under the
        // cursor with the book closed. The click path and the tooltip were already
        // book-aware; only the highlight was not.
        //
        // `hover_blocked` is the second half of the same fix and a different fault
        // — the panel not consuming the pointer at all. See
        // `ContainerFrame::hover_blocked`; note it deliberately does **not** gate
        // the carried-stack draw below.
        //
        // `isHighlightable()` is not restated: base `Slot` returns `true` and the
        // only override in 26.2 is `NonInteractiveResultSlot` (the crafter and
        // the recipe-book ghost), which no menu this client models uses. In
        // particular a crafting table's `ResultSlot` does **not** override it, so
        // the result slot *is* highlighted — easy to assume otherwise given how
        // many other branches special-case it.
        let hovered = if frame.hover_blocked {
            None
        } else {
            frame.cursor.and_then(|[cx, cy]| {
                match hit_test_with_book(menu, gui_scale, width, height, cx, cy, frame.book_open) {
                    MenuHit::Slot(i) => Some(i),
                    MenuHit::Panel | MenuHit::Outside => None,
                }
            })
        };
        let slot_rect = |menu_index: usize| {
            layout
                .slots
                .iter()
                .find(|r| r.menu_index == menu_index)
                .map(|r| (x + r.x, y + r.y))
        };

        // `extractSlotHighlightBack` (`:153-157`), then the empty-slot
        // placeholders that `extractSlot` blits (`:224-230`), then — past the
        // marker below — `extractSlotHighlightFront` (`:159-163`). All three ride
        // the background stream because they sample the same atlas as the panel
        // art; the marker is what lets the renderer replay the front half *after*
        // the item passes, which is the entire reason vanilla has two highlight
        // sprites instead of one.
        //
        // Every one of these is gated on a background being attached. The
        // jar-less fallback path draws none of them, which is both honest (there
        // is no sprite to draw) and the negative control the tests lean on.
        if let Some(bg) = background {
            if let Some((hx, hy)) = hovered.and_then(slot_rect) {
                if let Some(q) = bg.sprite_quad(
                    SLOT_HIGHLIGHT_BACK,
                    hx - HIGHLIGHT_INSET,
                    hy - HIGHLIGHT_INSET,
                    HIGHLIGHT,
                    HIGHLIGHT,
                ) {
                    b.bg_sprite(q);
                }
            }
            // `extractSlot`'s `if (itemStack.isEmpty() && slot.isActive())` arm.
            // The id comes off the slot itself (`Slot::no_item_icon`), never from
            // a positional rule — `lodestone-game`'s
            // `no_item_icons::every_other_slot_declares_nothing` is the gate on
            // that, and its control is that a chest and a crafting table declare
            // none at all, so this loop draws nothing extra in them.
            for rect in &layout.slots {
                if menu.slot_item(rect.menu_index).is_some() {
                    continue;
                }
                let Some(id) = menu.slot(rect.menu_index).and_then(|s| s.no_item_icon) else {
                    continue;
                };
                if let Some(q) = bg.sprite_quad(id, x + rect.x, y + rect.y, CELL, CELL) {
                    b.bg_sprite(q);
                }
            }
        }
        // Everything appended to the background stream past here draws **after**
        // the slot item passes.
        let bg_slot_floats = b.bg_verts.len();
        if let Some(bg) = background
            && let Some((hx, hy)) = hovered.and_then(slot_rect)
            && let Some(q) = bg.sprite_quad(
                SLOT_HIGHLIGHT_FRONT,
                hx - HIGHLIGHT_INSET,
                hy - HIGHLIGHT_INSET,
                HIGHLIGHT,
                HIGHLIGHT,
            )
        {
            b.bg_sprite(q);
        }

        // Both labels, exactly as `AbstractContainerScreen::extractLabels` draws
        // them (`AbstractContainerScreen.java`):
        //
        //     graphics.text(font, title,               titleLabelX, titleLabelY, -12566464, false);
        //     graphics.text(font, playerInventoryTitle, inventoryLabelX, inventoryLabelY, -12566464, false);
        //
        // Three things this got wrong before, all of which the play report read
        // as one blurred "the font is wrong":
        //
        // * `-12566464` is `0xFF404040`, a **dark grey**, and the trailing
        //   `false` means **no drop shadow**. `Builder::label` honours the
        //   second; the first only applies against vanilla's own light panel art
        //   — the programmatic fallback's flat fill is dark, so dark grey on it
        //   would be invisible and it keeps a warm-light ink instead. That
        //   divergence is the jar-less path only, and the pixel gate asserts the
        //   vanilla value on the path that has the art.
        // * The text was pushed through `to_ascii_uppercase()`. Vanilla never
        //   does, `hud::font` has had lowercase glyphs all along, and the cost
        //   was worst on the thing the player noticed: a chest renamed "Loot"
        //   drew as "LOOT".
        // * It drew with `ColourStream::text` — the fixed-advance 5x7 *debug*
        //   font — while `Builder` was already holding a `VanillaFont` for stack
        //   counts. Right glyphs, wrong typeface and wrong advances.
        //
        // `label_layout` supplies the anchors; `titleLabelY` is 6, not 7, and
        // `titleLabelX` is not always 8.
        //
        // `menu_type_title_anchor` then corrects the nine real screens
        // `label_layout` structurally cannot (it sees no `menu_type` and no font):
        // the furnace family, brewing stand, dispenser/dropper, crafter, anvil,
        // loom, stonecutter and cartography table. A `None` — no `menu_type`
        // attached, or one it does not know — leaves `label_layout`'s anchor
        // exactly as it was, which is what keeps every existing caller unchanged.
        let labels = label_layout(menu, &layout);
        let labels = match menu_type_title_anchor(frame.menu_type, &layout, frame.title, font) {
            Some([title_x, title_y]) => LabelLayout {
                title_x,
                title_y,
                ..labels
            },
            None => labels,
        };
        let label_colour = if bg_quads.is_some() {
            [64.0 / 255.0, 64.0 / 255.0, 64.0 / 255.0, 1.0]
        } else {
            [0.88, 0.84, 0.73, 1.0]
        };
        b.label(
            frame.title,
            x + labels.title_x,
            y + labels.title_y,
            1.0,
            label_colour,
        );
        // `None` on the player inventory screen and nowhere else — see
        // `label_layout`.
        if let Some([lx, ly]) = labels.inventory {
            b.label(frame.inventory_label, x + lx, y + ly, 1.0, label_colour);
        }

        // The merchant's trade list and its second "Trades" label (issue
        // That fix's UI half) — see `super::merchant`'s module doc. A no-op for
        // every other screen, and for a merchant screen with no offers
        // received yet (`frame.trades` is `None`).
        draw_merchant_trades(
            &mut b, menu, frame, assets, background, font, x, y, label_colour,
        );

        // The anvil's XP cost and the enchanting table's three level costs —
        // `docs/container-cost-screens.md`'s "What is not yet wired" gap,
        // closed. Both are drawn from `frame.cost_data` alongside the two
        // labels above, matching vanilla's own `extractLabels` pass
        // (`AnvilScreen.java`, `EnchantmentScreen.java`).
        draw_container_costs(&mut b, menu, &layout, frame, x, y);

        // The beacon's primary/secondary power buttons and confirm/cancel
        // controls (issue #613's `SetBeaconEffects` remainder) — see
        // `crate::container::beacon`'s module doc for why these draw as
        // tinted swatches rather than real sprites.
        draw_beacon_panel(&mut b, menu, frame, x, y);

        // Every well first, so the colour stream splits cleanly into "chrome"
        // and "what goes on top of an icon". The icons are drawn between the two
        // halves (they are a separate pass, and the 3-D ones need a depth
        // buffer), so a stack count emitted in the same loop as its well would
        // end up *underneath* the sprite it is counting. Skipped when the real
        // background is attached: its own art already bakes in every slot well
        // at these exact pixel offsets (the layout constants were themselves
        // derived from vanilla's sheets — see `slot_layout`'s doc comment), so a
        // second flat well drawn on top would just be visual noise.
        if bg_quads.is_none() {
            for slot in &layout.slots {
                let sx = x + slot.x;
                let sy = y + slot.y;
                b.rect_px(sx - 1.0, sy - 1.0, SLOT, SLOT, [0.04, 0.035, 0.032, 0.92]);
                b.rect_px(sx, sy, CELL, CELL, [0.32, 0.30, 0.27, 0.86]);
            }
        }

        // The drag preview (part 2), resolved once for the whole frame.
        // `plan` is keyed by menu index below; `remainder` is what the cursor
        // would keep. Both come out of `Menu::quick_craft_plan` — the release
        // path's own arithmetic — so the preview cannot show a split the release
        // would not produce.
        let drag = drag_preview(menu, frame.drag);
        // Vanilla's 50%-white wash behind each previewed stack
        // (`AbstractContainerScreen.java`'s `fill(..., -2130706433)` —
        // `0x80FFFFFF`). Emitted **before** `chrome_floats` so it lands under the
        // icon it backs; everything past that marker draws over the icon passes.
        if let Some(preview) = &drag {
            for slot in &layout.slots {
                // Vanilla's wash is inside the `if (!done)` arm and gated on
                // `quickCraftStack`, i.e. on the cell surviving the placeability
                // check — not on bare membership. So this follows `cell`, not
                // `paints`, and a painted-but-unfillable cell gets neither wash
                // nor number.
                if preview.cell(slot.menu_index).is_some() && !preview.single {
                    b.rect_px(x + slot.x, y + slot.y, CELL, CELL, [1.0, 1.0, 1.0, 128.0 / 255.0]);
                }
            }
        }
        let chrome_floats = b.verts.len();

        for slot in &layout.slots {
            let sx = x + slot.x;
            let sy = y + slot.y;
            // A painted cell draws the *provisional* stack instead of its real
            // contents — vanilla replaces `itemStack` in `extractSlot` (`:217`)
            // rather than drawing a second thing on top.
            if let Some(preview) = &drag {
                // `quickCraftSlots.size() == 1` returns from `extractSlot`
                // (`:203-205`) before anything is drawn — so a one-cell paint
                // blanks the cell entirely, including whatever was already in it.
                // Easy to miss, and it is a real visible behaviour: a single-cell
                // drag is about to degrade to a plain place anyway.
                if preview.single {
                    if preview.paints(slot.menu_index) {
                        continue;
                    }
                } else if let Some(cell) = preview.cell(slot.menu_index) {
                    let mut provisional = preview.source.clone();
                    provisional.set_count(cell.count);
                    b.draw_stack_counted(
                        assets,
                        &provisional,
                        sx,
                        sy,
                        if cell.clamped {
                            item_icon::COUNT_INK_CLAMPED
                        } else {
                            item_icon::COUNT_INK
                        },
                    );
                    continue;
                }
            }
            let Some(stack) = menu.slot_item(slot.menu_index) else {
                continue;
            };
            b.draw_stack(assets, stack, sx, sy);
        }

        // Ghost preview: when the crafting result slot is still empty, show
        // what the grid would produce, dimmed — a hint before the server's own
        // `container_set_slot` lands, never a claim. This never touches `menu`
        // itself (the match runs fresh against `menu.crafting_grid()` every
        // frame), so a server disagreeing simply means next frame's real
        // `slot_item` draw takes over and this block stops firing — the same
        // "server truth always wins" contract every other slot already has.
        // See `docs/crafting.md`'s "who computes the result slot".
        if let Some(craft) = menu.craft_layout()
            && menu.slot_item(craft.result_slot).is_none()
            && let Some(book) = frame.recipe_book
            && let Some(grid) = menu.crafting_grid()
            && let Some(predicted) = book.match_grid(&grid)
            && let Some(rect) = layout.slots.iter().find(|r| r.menu_index == craft.result_slot)
        {
            let sx = x + rect.x;
            let sy = y + rect.y;
            b.draw_stack(assets, predicted, sx, sy);
            // Dim the icon just drawn: a translucent dark quad on the colour
            // stream, appended after the icon so it lands on top of it (see the
            // module doc on pass structure — everything past `chrome_floats`
            // draws over the icon passes regardless of append order among
            // itself). This is the same "same icon, lower apparent opacity"
            // treatment vanilla's own recipe-book ghosts use, and it is what
            // keeps a predicted result visually distinct from a confirmed one.
            b.rect_px(sx, sy, CELL, CELL, [0.05, 0.05, 0.05, 0.55]);
        }

        // Everything above is the **slot stratum**; everything below is the
        // carried stack, drawn in its own later stratum. This is vanilla's
        // `graphics.nextStratum()`, called immediately before it draws the
        // carried item and nowhere else on the screen
        // (`AbstractContainerScreen.java`).
        //
        // It has to be a stratum and not merely "appended last".
        // Append order only settles two of the four cases, because the GUI item
        // passes run **model first, then flat sprites** — the model pass is the
        // only one that needs a depth attachment and a pass's attachments are
        // fixed for its lifetime:
        //
        // | cursor holds | slot holds | before |
        // |---|---|---|
        // | flat sprite | flat sprite | correct — later in the same stream |
        // | flat sprite | 3-D block | correct — sprite pass runs after the model pass |
        // | **3-D block** | flat sprite | **wrong** — model pass runs *before* the sprite pass |
        // | **3-D block** | 3-D block | **wrong** — same depth, resolved by the depth buffer, not by append order |
        //
        // and the slot layer's stack counts, which are on the colour stream's
        // second run, painted over a flat carried sprite too. So the three
        // markers recorded below let the renderer replay all three streams as a
        // second stratum whose model pass clears depth again.
        let slot_floats = b.verts.len();
        let slot_item_floats = b.item_verts.len();
        let slot_glint_floats = b.glint_verts.len();
        let slot_model_verts = b.model_verts.len();
        let slot_special = b.special.len();

        // The carried stack — what the player has picked up and is dragging —
        // draws above every slot and below the tooltip (which this client does
        // not draw yet). Vanilla centres it on the cursor; `cursor` is `None`
        // unless the caller opted in via `ContainerFrame::with_cursor`, so every
        // existing caller (the headless gates, `tests/container_screen.rs`, a
        // menu with nothing carried) draws exactly as before.
        //
        // `frame.cursor` is documented as the same **physical** viewport space
        // `hit_test` takes, but this builder draws in the logical canvas (`w`,
        // `h` above) — dividing by the same effective scale `hit_test` divides
        // its own `x`/`y` by is what keeps the drawn stack centred on the actual
        // cursor instead of drifting off toward a corner as the scale grows.
        if let (Some([cx, cy]), Some(stack)) = (frame.cursor, menu.carried()) {
            let scale =
                crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
            let (cx, cy) = (cx / scale, cy / scale);
            // Mid-drag, vanilla shows the cursor holding what it would be *left*
            // with, not what it started with: `extractCarriedItem` (`:119-124`)
            // replaces the stack with `copyWithCount(quickCraftingRemainder)`
            // whenever more than one cell is painted. `remainder` is derived from
            // the same plan the cells drew from, so the numbers on screen add up.
            // A remainder of zero draws nothing (vanilla's `copyWithCount(0)` is
            // empty, and its yellow "0" decoration is not modelled).
            match drag.as_ref().filter(|p| !p.single) {
                Some(preview) if preview.remainder > 0 => {
                    let mut left = preview.source.clone();
                    left.set_count(preview.remainder);
                    b.draw_stack(assets, &left, cx - CELL * 0.5, cy - CELL * 0.5);
                }
                Some(_) => {}
                None => b.draw_stack(assets, stack, cx - CELL * 0.5, cy - CELL * 0.5),
            }
        }

        // The hovered slot's tooltip, **last of everything** — see
        // `super::tooltip`'s module doc for why the tail of this stream is what
        // puts it on top, and for the two things it deliberately cannot show
        // (lore, enchantment names). `None` here is every existing caller.
        //
        // `hovered` is passed in rather than re-resolved inside `emit_tooltip`: it
        // used to run its own `hit_test_with_book` here, which is how the tooltip
        // and the highlight came to be answering the same question with two
        // different calls — and they *did* disagree, because the highlight's call
        // was the unshifted one. One resolution, two consumers.
        if let Some(advanced) = frame.tooltips {
            // `frame.bundle_selection`'s own `slot` is matched against
            // `hovered` inside `emit_tooltip` — a selection tracked against a
            // slot the cursor is no longer over must not paint a highlight in
            // whatever the cursor now sits on.
            super::tooltip::emit_tooltip(
                &mut b,
                assets,
                menu,
                hovered,
                frame.cursor,
                advanced,
                gui_scale,
                width,
                height,
                (w, h),
                frame.bundle_selection,
            );
        }

        Self {
            bg_slot_vertex_count: bg_slot_floats / BG_FLOATS_PER_VERTEX,
            dim_vertex_count: dim_floats / FLOATS_PER_VERTEX,
            chrome_vertex_count: chrome_floats / FLOATS_PER_VERTEX,
            slot_vertex_count: slot_floats / FLOATS_PER_VERTEX,
            slot_item_vertex_count: slot_item_floats / crate::hud::SPRITE_FLOATS_PER_VERTEX,
            slot_glint_vertex_count: slot_glint_floats / crate::hud::SPRITE_FLOATS_PER_VERTEX,
            slot_model_vertex_count: slot_model_verts,
            slot_special_count: slot_special,
            verts: b.verts,
            item_verts: b.item_verts,
            glint_verts: b.glint_verts,
            model_verts: b.model_verts,
            special: b.special,
            bg_verts: b.bg_verts,
            // Not populated from `build_inner`'s own layout at all — every
            // caller through this path (the container screens) draws its
            // slots/chrome through the existing six-pass sequence, which has
            // no frame-then-icon-then-dim sandwich to fit. Only
            // `menu::advancements` fills these.
            mid_bg_verts: Vec::new(),
            mid_verts: Vec::new(),
            mid_item_verts: Vec::new(),
            mid_glint_verts: Vec::new(),
            dim2_verts: Vec::new(),
            widget_rect: Some(Rect {
                x,
                y,
                w: layout.width,
                h: layout.height,
            }),
            // The inventory avatar (`InventoryScreen.extractBackground`'s second
            // call). Measured from `x`/`y` — the **shifted** panel origin above —
            // for the reason that shift is applied there and nowhere else, and
            // with the cursor divided down by the same integer scale
            // `hit_test_with_book` divides by, so the head aims at where the
            // pointer visually is rather than at a physical-pixel coordinate
            // several times too far out.
            player_avatar: matches!(menu.kind(), MenuKind::Player).then(|| {
                let scale =
                    crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
                // `with_pose` carries the **live** animation state through
                // (`ContainerFrame::avatar_pose`, `AnimInput::REST` for a caller
                // with no `Sim`). Dropping it here is the whole difference between
                // the pose reaching the draw and the field existing unread.
                PlayerAvatar::new(
                    x,
                    y,
                    frame.cursor.map(|[cx, cy]| [cx / scale, cy / scale]),
                )
                .with_pose(frame.avatar_pose)
                // The local player's own uuid (issue #646), so the *default*
                // skin — when nothing local or fetched has claimed the
                // avatar — resolves through the same `default_skin_for_uuid`
                // call the world side uses. See `PlayerAvatar::uuid`'s doc.
                .with_uuid(frame.avatar_uuid)
            }),
        }
    }
}

/// Vanilla's `-8323296` (`0x80FF20`) — green, the anvil cost's affordable
/// colour and the enchanting row's affordable cost-number colour
/// (`AnvilScreen.java`, and the `col = -8323296` reassignment at
/// `EnchantmentScreen.java`).
const COST_GREEN: [f32; 4] = [128.0 / 255.0, 1.0, 32.0 / 255.0, 1.0];
/// Vanilla's `-40864` (`0xFFFF6060`) — red, the anvil's "too expensive" and
/// "can't afford" colour (`AnvilScreen.java`, `:107`).
const COST_RED: [f32; 4] = [1.0, 96.0 / 255.0, 96.0 / 255.0, 1.0];
/// Vanilla's `-12550384` (`0x407F10`) — the enchanting row's *disabled*
/// cost-number colour, exactly half [`COST_GREEN`]'s brightness
/// (`EnchantmentScreen.java`'s `col = -12550384`, itself
/// `ARGB.opaque((col & 16711422) >> 1)` of the enabled green applied to the
/// row's other text — the cost number reuses the same halved constant).
const COST_DISABLED_GREEN: [f32; 4] = [64.0 / 255.0, 127.0 / 255.0, 16.0 / 255.0, 1.0];

/// Draws the merchant screen's trade list: the second "Trades" label, and
/// each visible offer's cost/result icons, discount strikethrough and trade
/// arrow (that fix's UI half) — `MerchantScreen.extractLabels`/
/// `extractContents` (`MerchantScreen.java`). A no-op for any
/// screen without [`SpecialLayout::Merchant`], or a merchant screen with no
/// offers yet (`frame.trades` is `None`) — every existing caller.
///
/// The real payment/result slots (menu indices `0..3`) are **not** drawn
/// here — they are ordinary [`Menu`] slots and already draw through the loop
/// above this function's one call site, the same as every other screen's
/// slots. This only draws the trade *list*, which vanilla itself calls "fake
/// items" because they are not slots at all.
#[allow(clippy::too_many_arguments)]
fn draw_merchant_trades(
    b: &mut Builder<'_>,
    menu: &Menu,
    frame: &ContainerFrame<'_>,
    assets: &IconAssets<'_>,
    background: Option<&ContainerBackground>,
    font: Option<&VanillaFont>,
    x: f32,
    y: f32,
    label_colour: [f32; 4],
) {
    if menu.special_layout() != Some(SpecialLayout::Merchant) {
        return;
    }
    let Some(trades) = frame.trades else { return };

    // `merchant.trades` — `MerchantScreen.java`:
    // `5 - font.width(TRADES_LABEL) / 2 + 48`, i.e. centred on local x = 53.
    let trades_width = font.map_or(0.0, |f| f.width(frame.trades_label, 1.0));
    let trades_x = (53.0 - trades_width / 2.0).floor();
    b.label(frame.trades_label, x + trades_x, y + 6.0, 1.0, label_colour);

    let offers = trades.offers();
    for (i, offer) in offers.iter().enumerate().take(super::merchant::OFFER_ROWS) {
        let row = super::merchant::row_layout(i);
        let adjusted_a = super::merchant::adjusted_cost_a_count(offer);
        if let Some(stack) = super::merchant::cost_item_stack(offer.cost_a.0, adjusted_a) {
            b.draw_stack(assets, &stack, x + row.cost_a[0], y + row.cost_a[1]);
        }
        // The discount strikethrough — vanilla draws two icons (base and
        // adjusted price) side by side when they differ
        // (`extractAndDecorateCostA`, `MerchantScreen.java`); this
        // draws the adjusted price alone plus the strikethrough sprite,
        // which shows the same fact (a demand discount is active) without a
        // second overlapping icon.
        if offer.cost_a.1 != adjusted_a
            && let Some(bg) = background
            && let Some(q) = bg.sprite_quad(
                MERCHANT_DISCOUNT_STRIKETHROUGH,
                x + row.strikethrough[0],
                y + row.strikethrough[1],
                super::merchant::STRIKETHROUGH_W,
                super::merchant::STRIKETHROUGH_H,
            )
        {
            b.bg_sprite(q);
        }
        if let Some((id, count)) = offer.cost_b
            && let Some(stack) = super::merchant::cost_item_stack(id, count)
        {
            b.draw_stack(assets, &stack, x + row.cost_b[0], y + row.cost_b[1]);
        }
        if let Some(result) = &offer.result {
            let stack = ItemStack::from(result);
            b.draw_stack(assets, &stack, x + row.result[0], y + row.result[1]);
        }
        if let Some(bg) = background {
            let arrow_id = if offer.out_of_stock {
                MERCHANT_TRADE_ARROW_OUT_OF_STOCK
            } else {
                MERCHANT_TRADE_ARROW
            };
            if let Some(q) = bg.sprite_quad(
                arrow_id,
                x + row.arrow[0],
                y + row.arrow[1],
                super::merchant::ARROW_W,
                super::merchant::ARROW_H,
            ) {
                b.bg_sprite(q);
            }
        }
    }

    // The out-of-stock overlay is drawn once, for the **selected** row only
    // (`extractBackground`, `MerchantScreen.java`) — a fixed panel
    // position, not one per row.
    if let Some(bg) = background
        && let Some(selected) = offers.get(frame.selected_trade)
        && selected.out_of_stock
        && let Some(q) = bg.sprite_quad(
            MERCHANT_OUT_OF_STOCK,
            x + super::merchant::OUT_OF_STOCK_X,
            y + super::merchant::OUT_OF_STOCK_Y,
            super::merchant::OUT_OF_STOCK_W,
            super::merchant::OUT_OF_STOCK_H,
        )
    {
        b.bg_sprite(q);
    }
}

/// Draws the anvil's XP level cost and the enchanting table's three per-row
/// level costs, from `frame.cost_data` — the last hop
/// `docs/container-cost-screens.md` names. A no-op for every other screen
/// (`cost_data` empty, or `menu.special_layout()` neither `Anvil` nor
/// `Enchanting`), which is what keeps every existing caller unchanged.
fn draw_container_costs(
    b: &mut Builder<'_>,
    menu: &Menu,
    layout: &SlotLayout,
    frame: &ContainerFrame<'_>,
    x: f32,
    y: f32,
) {
    match menu.special_layout() {
        Some(SpecialLayout::Anvil) => draw_anvil_cost(b, menu, layout, frame, x, y),
        Some(SpecialLayout::Enchanting) => draw_enchanting_costs(b, menu, frame, x, y),
        _ => {}
    }
}

/// `AnvilScreen.extractLabels` (`AnvilScreen.java`): the XP cost in
/// the top-right of the panel, on a translucent backdrop, right-aligned.
/// `container_data(0)` is `AnvilMenu`'s one `DataSlot` (`AnvilMenu.java`).
fn draw_anvil_cost(
    b: &mut Builder<'_>,
    menu: &Menu,
    layout: &SlotLayout,
    frame: &ContainerFrame<'_>,
    x: f32,
    y: f32,
) {
    let cost = frame
        .cost_data
        .iter()
        .find(|(p, _)| *p == 0)
        .map_or(0, |(_, v)| *v);
    if cost <= 0 {
        return;
    }
    // `AnvilMenu`'s result slot is menu index 2 (`Menu::item_combiner(3, 2,
    // Anvil)` — see `docs/container-cost-screens.md`).
    const RESULT_SLOT: usize = 2;
    let line: Option<(String, [f32; 4])> = if cost >= 40 && !frame.has_infinite_materials {
        Some(("Too Expensive!".to_owned(), COST_RED))
    } else if menu.slot_item(RESULT_SLOT).is_none() {
        None
    } else {
        // `AnvilMenu::mayPickup` (`AnvilMenu.java`): affordable iff
        // infinite materials, or the player's level covers the cost.
        let may_pickup = frame.has_infinite_materials || frame.xp_level >= cost;
        let colour = if may_pickup { COST_GREEN } else { COST_RED };
        // `en_us.json`'s `container.repair.cost`: `"Enchantment Cost: %1$s"`.
        // No language table reaches this module (the same documented gap
        // `styled_hover_name` has for item names), so this is the resolved
        // English wording rather than a `translate` lookup — readable
        // either way, matching vanilla text only when the table is en_us.
        Some((format!("Enchantment Cost: {cost}"), colour))
    };
    let Some((text, colour)) = line else {
        return;
    };
    let text_width = b.font.map_or(0.0, |f| f.width(&text, 1.0));
    // `AnvilScreen.java`: `tx = imageWidth - 8 - font.width(line) - 2`,
    // `ty = 69`, backdrop `fill(tx - 2, 67, imageWidth - 8, 79, 0x4F000000)`.
    let tx = layout.width - 8.0 - text_width - 2.0;
    let ty = 69.0;
    b.rect_px(
        x + tx - 2.0,
        y + 67.0,
        (layout.width - 8.0) - (tx - 2.0),
        79.0 - 67.0,
        [0.0, 0.0, 0.0, 79.0 / 255.0],
    );
    b.shadowed_label(&text, x + tx, y + ty, 1.0, colour);
}

/// `EnchantmentScreen.extractBackground` (`EnchantmentScreen.java`):
/// the per-row level-cost number, bottom-right of each of the three offer
/// buttons. `container_data(0..3)` are `EnchantmentMenu.costs[0..3]`
/// (`EnchantmentMenu.java`).
///
/// **Deliberately does not draw the enchantment-name text.** That is
/// `EnchantmentNames`' Standard Galactic Alphabet cipher font, a whole
/// separate subsystem this build has no glyphs for — orthogonal to the cost
/// number `docs/container-cost-screens.md` scopes this work to.
fn draw_enchanting_costs(b: &mut Builder<'_>, menu: &Menu, frame: &ContainerFrame<'_>, x: f32, y: f32) {
    // `EnchantmentMenu`'s lapis slot is menu index 1 (`Menu::enchanting_table`
    // marks slot 1 `SlotKind::LapisOnly` — see `docs/container-cost-screens.md`).
    const LAPIS_SLOT: usize = 1;
    let gold_count = menu.slot_item(LAPIS_SLOT).map_or(0, ItemStack::count);
    for i in 0..3i32 {
        let cost = frame
            .cost_data
            .iter()
            .find(|(p, _)| *p == i)
            .map_or(0, |(_, v)| *v);
        if cost <= 0 {
            continue;
        }
        // `EnchantmentScreen.java`: disabled unless infinite materials,
        // or both enough lapis (`goldCount >= i + 1`) and enough levels.
        let afford = frame.has_infinite_materials
            || (gold_count >= i + 1 && frame.xp_level >= cost);
        let colour = if afford { COST_GREEN } else { COST_DISABLED_GREEN };
        let text = cost.to_string();
        let text_width = b.font.map_or(0.0, |f| f.width(&text, 1.0));
        // `EnchantmentScreen.java`: `leftPos = xo + 60`,
        // `leftPosText = leftPos + 20`, row `y = yo + 14 + 19*i`, cost drawn
        // at `(leftPosText + 86 - width, y + 16 + 7)`.
        let left_pos_text = x + 60.0 + 20.0;
        let row_y = y + 14.0 + 19.0 * i as f32;
        b.shadowed_label(
            &text,
            left_pos_text + 86.0 - text_width,
            row_y + 16.0 + 7.0,
            1.0,
            colour,
        );
    }
}

/// The beacon's own "Primary Power"/"Secondary Power" backdrop colour —
/// `BeaconScreen.extractLabels`' literal `-2039584` (an opaque light-grey),
/// re-expressed as normalised RGBA.
const BEACON_LABEL_COLOUR: [f32; 4] = [0xf0 as f32 / 255.0, 0xe0 as f32 / 255.0, 0xe0 as f32 / 255.0, 1.0];

/// `BeaconScreen.extractLabels`/`init` (`BeaconScreen.java`): the two
/// centred "Primary Power"/"Secondary Power" labels, the eight power
/// buttons and the confirm/cancel controls — see `crate::container::beacon`'s
/// module doc for why the buttons draw as tinted swatches rather than real
/// vanilla sprites (no GUI PNGs are bundled in this tree at all; every
/// container background here is loaded live from the player's own
/// `client.jar`, and this repo has no potion-effect icon atlas either — the
/// status-effect HUD chips already establish the same "flat tinted swatch"
/// simplification `crate::effects::tint_for` exists for).
///
/// A no-op for every non-beacon screen.
fn draw_beacon_panel(b: &mut Builder<'_>, menu: &Menu, frame: &ContainerFrame<'_>, x: f32, y: f32) {
    if menu.special_layout() != Some(SpecialLayout::Beacon) {
        return;
    }
    let levels = frame
        .cost_data
        .iter()
        .find(|(p, _)| *p == 0)
        .map_or(0, |(_, v)| *v);

    // `PRIMARY_EFFECT_LABEL`/`SECONDARY_EFFECT_LABEL`, centred at local
    // `(62, 10)`/`(169, 10)` (`BeaconScreen.extractLabels`). No language
    // table reaches this module — see `draw_anvil_cost`'s own doc for the
    // identical gap — so this is the resolved English wording.
    for (text, cx) in [("Primary Power", 62.0), ("Secondary Power", 169.0)] {
        let w = b.font.map_or(0.0, |f| f.width(text, 1.0));
        b.shadowed_label(text, x + cx - w / 2.0, y + 10.0, 1.0, BEACON_LABEL_COLOUR);
    }

    for button in super::beacon::power_buttons()
        .into_iter()
        .chain(super::beacon::upgrade_button(frame.beacon_primary))
    {
        let unlocked = i32::from(button.tier) < levels;
        let selected = if button.is_primary {
            frame.beacon_primary == Some(&button.effect)
        } else {
            frame.beacon_secondary == Some(&button.effect)
        };
        let tint = crate::effects::tint_for(button.effect.path());
        // Dimmed to a third brightness when the pyramid has not unlocked
        // this tier yet (`BUTTON_DISABLED_SPRITE`'s own darker art), lifted
        // halfway to white when selected (`BUTTON_SELECTED_SPRITE`'s own
        // brighter highlight) — approximations of those two sprite swaps,
        // not a transcription of their exact pixels.
        let mut colour = if unlocked {
            [tint[0], tint[1], tint[2], 1.0]
        } else {
            [tint[0] / 3.0, tint[1] / 3.0, tint[2] / 3.0, 1.0]
        };
        if selected {
            colour = [
                (colour[0] + 1.0) / 2.0,
                (colour[1] + 1.0) / 2.0,
                (colour[2] + 1.0) / 2.0,
                1.0,
            ];
        }
        b.rect_px(
            x + button.x,
            y + button.y,
            super::beacon::BUTTON,
            super::beacon::BUTTON,
            colour,
        );
    }

    // `BeaconConfirmButton`/`BeaconCancelButton.updateStatus` — confirm is
    // only bright when it would actually do something (a payment item
    // present and a primary chosen), cancel is always live.
    let has_payment = menu.slot_item(0).is_some();
    let can_confirm = frame.beacon_primary.is_some() && has_payment;
    let confirm = super::beacon::confirm_rect();
    let confirm_colour = if can_confirm {
        [80.0 / 255.0, 200.0 / 255.0, 80.0 / 255.0, 1.0]
    } else {
        [50.0 / 255.0, 90.0 / 255.0, 50.0 / 255.0, 1.0]
    };
    b.rect_px(x + confirm.x, y + confirm.y, confirm.w, confirm.h, confirm_colour);
    let cancel = super::beacon::cancel_rect();
    b.rect_px(
        x + cancel.x,
        y + cancel.y,
        cancel.w,
        cancel.h,
        [200.0 / 255.0, 80.0 / 255.0, 80.0 / 255.0, 1.0],
    );
}

/// Everything one frame's drag preview needs, resolved once (part 2).
///
/// Deliberately **not** a recomputation of the split: `cells` is
/// [`Menu::quick_craft_plan`]'s output verbatim and `remainder` is
/// [`Menu::quick_craft_remainder`]'s, both of which the release path itself uses.
#[derive(Debug)]
struct DragPreview {
    /// The carried stack the split is measured against — vanilla's `carried` in
    /// `extractSlot`, and the item the provisional stacks are copies of.
    source: ItemStack,
    /// Every slot in the paint set — vanilla's `quickCraftSlots`, which is what
    /// `extractSlot`'s `contains(slot)` gate tests. Kept separately from
    /// [`cells`](Self::cells) because the two answer different questions: this is
    /// "is this cell painted at all", which is what the one-cell blank turns on.
    painted: Vec<usize>,
    /// Per painted cell, in paint order, with its provisional count. Cells the
    /// release would refuse are **absent**, exactly as they are absent from the
    /// distribution — so a cell that is `painted` but has no entry here draws the
    /// wash and no number, which is honest rather than a guess.
    ///
    /// Vanilla's own preview filter is narrower (`canItemQuickReplace &&
    /// canDragTo`, `:207`, without `mayPlace` or the count check) and *removes*
    /// the failing slot from `quickCraftSlots` as a side effect of drawing. Using
    /// the release path's stricter filter here can only reject more, never fewer,
    /// so preview-vs-outcome agreement is preserved; and re-deriving the set from
    /// a draw call is not a thing a builder that runs per frame should do.
    cells: Vec<lodestone_game::click::QuickCraftCell>,
    /// What the cursor would be left holding.
    remainder: i32,
    /// `quickCraftSlots.size() == 1`. Vanilla's `extractSlot` returns before
    /// drawing anything in that case (`:203-205`), and `extractCarriedItem` skips
    /// its remainder substitution (`:119`) — both because a one-cell drag is about
    /// to be re-dispatched as an ordinary click.
    single: bool,
}

impl DragPreview {
    /// Whether `menu_index` is in the paint set at all.
    fn paints(&self, menu_index: usize) -> bool {
        self.painted.contains(&menu_index)
    }

    /// The provisional contents of `menu_index`, if the release would fill it.
    fn cell(&self, menu_index: usize) -> Option<&lodestone_game::click::QuickCraftCell> {
        self.cells.iter().find(|c| c.menu_index == menu_index)
    }
}

/// Resolve [`ContainerFrame::drag`] against a menu, or `None` when no drag is
/// armed, nothing is painted, or the cursor is empty — vanilla gates the whole
/// preview on `isQuickCrafting && quickCraftSlots.contains(slot) &&
/// !carried.isEmpty()` (`AbstractContainerScreen.java`).
///
/// Note the `single` case is still `Some`: it has to be, because it draws
/// *nothing* in the painted cell rather than falling back to the real contents,
/// and "draw nothing here" needs the cell to still be recognised as painted.
fn drag_preview(menu: &Menu, drag: Option<(i32, &[usize])>) -> Option<DragPreview> {
    let (kind, painted) = drag?;
    if painted.is_empty() {
        return None;
    }
    let source = menu.carried()?.clone();
    Some(DragPreview {
        cells: menu.quick_craft_plan(painted, kind, &source),
        remainder: menu.quick_craft_remainder(painted, kind, &source),
        single: painted.len() == 1,
        painted: painted.to_vec(),
        source,
    })
}

/// Coverage for the anvil rename box (the report: "in the anvil, the input
/// where it should show the text just shows a solid red box"). Root cause
/// was `AnvilScreen.extractBackground`'s `TEXT_FIELD_SPRITE`/
/// `TEXT_FIELD_DISABLED_SPRITE` overlay never being drawn at all — no code
/// anywhere in this crate referenced `container/anvil/text_field[_disabled]`
/// — so vanilla's own `anvil.png`, which bakes a flat opaque `(255, 0, 0)`
/// placeholder under exactly that rect for this reason, showed straight
/// through. Measured against the real asset: 1760/1760 pixels of the
/// `leftPos + 59, topPos + 20, 110, 16` rect are pure red.
///
/// These gates run against the **real** vanilla jar (background *and*
/// font), so they skip rather than fail when this environment has none —
/// the same precondition-skip every other real-asset gate in this crate
/// uses. They were run once with `Some(SpecialLayout::Anvil)`'s
/// `b.shadowed_label` call commented out (the historical bug, reproduced):
/// [`the_rename_box_draws_real_glyph_geometry_not_one_flat_quad`] and
/// [`glyph_vertex_count_scales_with_the_known_strings_own_width`] both went
/// red, confirming the gate actually fires on the regression it exists to
/// catch.
#[cfg(test)]
mod anvil_rename_field_tests {
    use super::*;
    use lodestone_model::{Identifier, Text};

    const VIEW: (u32, u32) = (1280, 720);

    fn jar_manager() -> Option<lodestone_assets::ResourceManager> {
        crate::resources::vanilla_manager()
    }

    /// A 3-slot anvil menu (`Menu::item_combiner(3, 2, SpecialLayout::Anvil)`)
    /// with slot 0 holding a diamond sword, optionally custom-named —
    /// mirrors `AnvilScreen.subInit`'s `this.name.setEditable(this.menu
    /// .getSlot(0).hasItem())` precondition (slot 0 is always occupied here,
    /// so the box is always the "enabled" variant).
    fn anvil_menu_with_item(custom_name: Option<&str>) -> Menu {
        let mut menu = Menu::item_combiner(3, 2, SpecialLayout::Anvil);
        let item = Identifier::new("minecraft", "diamond_sword").expect("valid id");
        let mut stack = ItemStack::new(item, 1);
        if let Some(name) = custom_name {
            stack.set_custom_name(Some(Text::literal(name)));
        }
        menu.set_slot_item(0, Some(stack));
        menu
    }

    /// Renders the anvil screen against the **real** vanilla assets — both
    /// the background (so the real `anvil.png` red placeholder is genuinely
    /// in play, not a synthetic stand-in) and the font (so glyph counts and
    /// widths are real vanilla metrics, not the fixed-advance debug font).
    /// `None` when this environment has no jar.
    fn anvil_geometry(menu: &Menu, anvil_name: Option<&str>) -> Option<ContainerGeometry> {
        let manager = jar_manager()?;
        let bg = ContainerBackground::build(&manager).expect("real background builds");
        let font = VanillaFont::shared()?;
        let frame = ContainerFrame::new(Some(menu), "Repair & Name").with_anvil_name(anvil_name);
        Some(ContainerGeometry::build_inner(
            &frame,
            VIEW.0,
            VIEW.1,
            crate::config::AUTO_GUI_SCALE,
            &IconAssets {
                items: None,
                models: None,
            },
            Some(&font),
            Some(&bg),
        ))
    }

    /// **The vertex-sampling trap, made concrete**: the border/fill rects
    /// that cover the red placeholder draw unconditionally, so a detector
    /// that only asks "did anything draw in the box" cannot tell a real
    /// label apart from one silently dropped — both leave `verts`
    /// non-empty. This is why the real gate below measures a **delta**
    /// against the no-name case instead.
    #[test]
    fn a_naive_nonempty_check_cannot_see_a_dropped_label() {
        let menu = anvil_menu_with_item(Some("Anvil7"));
        let Some(with_name) = anvil_geometry(&menu, Some("Anvil7")) else {
            eprintln!("skip: no real vanilla jar in this environment");
            return;
        };
        let Some(without_name) = anvil_geometry(&menu, None) else {
            return;
        };
        assert!(!with_name.verts.is_empty(), "premise: the box draws something");
        assert!(
            !without_name.verts.is_empty(),
            "premise: the covering rects draw even with no name — this is \
             the trap, not a bug"
        );
    }

    /// The real gate. `VanillaFont::draw_legacy` rasterises text as one
    /// small filled rect per contiguous horizontal ink run, per glyph row
    /// (`crate::hud::vanilla_font`) — a six-character string is dozens of
    /// tiny rects, never the single flat quad a border/fill call alone
    /// would leave. Isolated as the vertex-count *delta* against the same
    /// frame with no name, so the ever-present border/fill rects cannot pad
    /// the count.
    #[test]
    fn the_rename_box_draws_real_glyph_geometry_not_one_flat_quad() {
        let name = "Anvil7";
        let menu = anvil_menu_with_item(Some(name));
        let Some(with_name) = anvil_geometry(&menu, Some(name)) else {
            eprintln!("skip: no real vanilla jar in this environment");
            return;
        };
        let Some(without_name) = anvil_geometry(&menu, None) else {
            return;
        };
        assert!(
            with_name.verts.len() > without_name.verts.len(),
            "the label must add vertices beyond the border/fill rects"
        );
        let delta_floats = with_name.verts.len() - without_name.verts.len();
        let delta_verts = delta_floats / FLOATS_PER_VERTEX;
        // One flat quad is 6 vertices (`ColourStream::rect`); two (border +
        // fill, both already unconditional) is 12. A real 6-glyph string's
        // ink-run rasterisation is far beyond that.
        assert!(
            delta_verts > 12,
            "expected many glyph-ink rects for {name:?}, got {delta_verts} \
             vertices (<= 2 flat quads worth) — looks like a flat cover with \
             no real text"
        );
    }

    /// **Magnitude control**: the delta must scale with the font's *own*
    /// width metric ([`VanillaFont::width`]), computed independently of the
    /// rasteriser under test — not merely "some vertices appeared". A
    /// fixed threshold alone cannot distinguish "drew the right text" from
    /// "drew unrelated placeholder ink"; a longer known string must add
    /// visibly more.
    #[test]
    fn glyph_vertex_count_scales_with_the_known_strings_own_width() {
        let short = "Anvil7";
        let long = "Anvil7Repair9Sword";
        let menu_short = anvil_menu_with_item(Some(short));
        let menu_long = anvil_menu_with_item(Some(long));
        let (Some(g_short), Some(g_none_short)) = (
            anvil_geometry(&menu_short, Some(short)),
            anvil_geometry(&menu_short, None),
        ) else {
            eprintln!("skip: no real vanilla jar in this environment");
            return;
        };
        let (Some(g_long), Some(g_none_long)) = (
            anvil_geometry(&menu_long, Some(long)),
            anvil_geometry(&menu_long, None),
        ) else {
            return;
        };
        let delta = |a: &ContainerGeometry, b: &ContainerGeometry| a.verts.len() - b.verts.len();
        let d_short = delta(&g_short, &g_none_short);
        let d_long = delta(&g_long, &g_none_long);

        let font = VanillaFont::shared().expect("checked non-None by anvil_geometry above");
        let w_short = font.width(short, 1.0);
        let w_long = font.width(long, 1.0);
        assert!(
            w_long > w_short,
            "premise: the long fixture must actually be wider ({w_short}px vs {w_long}px)"
        );
        assert!(
            d_long > d_short,
            "glyph vertex count did not scale with the font's own width \
             (short {w_short}px -> {d_short} verts, long {w_long}px -> {d_long} verts)"
        );
    }
}
