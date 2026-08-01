//! Container and inventory screen rendering.
//!
//! Slot state is folded by `lodestone-client`/`lodestone-game`; this module only
//! projects a [`Menu`](lodestone_game::menu::Menu) into rectangles, coloured
//! quads and **item icons**. The generic-container hotbar starts at `n + 27`,
//! not absolute slot 36.
//!
//! # Layout
//!
//! [`slot_layout`] dispatches on [`MenuKind`] and then, additively, on
//! [`Menu::craft_layout`]: a menu that reports a crafting grid gets the vanilla
//! crafting-table arrangement (grid + result to its right, player inventory
//! below) rather than the flat 9-wide run a plain container gets. That branch is
//! deliberately *not* a new `MenuKind` — a crafting table's quick-move regions
//! and content size are a generic container's, only its slot kinds and its
//! screen differ, and `MenuKind` is matched exhaustively across this crate.
//!
//! Every `SlotRect` carries the real `menu_index`, so there is no constant
//! offset anywhere: window 0 is `0` result / `1..=4` craft / `5..=8` armour /
//! `9..=35` main / `36..=44` hotbar / `45` offhand, while a `Generic { n }` has
//! neither armour nor offhand and its hotbar is at `n + 27`.
//!
//! # Icons
//!
//! Slot contents draw through [`crate::hud::item_icon`] — the same flat-sprite
//! and 3-D block-item pass the hotbar uses, with the same atlases, tint palette
//! and animation slots. Without [`ContainerRenderer::attach_items`] the screen
//! falls back to the hash-derived colour swatch and letter it drew before there
//! was an atlas to draw from, so a jar-less run still shows *something* in an
//! occupied slot.

use lodestone_game::click::{Click, ContainerInput, drag_header, drag_type, quick_craft_mask};
use lodestone_game::item::ItemStack;
use lodestone_game::menu::{CraftLayout, Menu, MenuKind, OUTSIDE_SLOT};
use lodestone_game::recipe::RecipeBook;
use lodestone_render::{BlockModels, GpuAtlas, GuiSpriteQuad, ModelVertex};

use lodestone_assets::{Atlas, AtlasBuilder, AtlasError, ItemAtlas, ResourceLocation, ResourceManager};

use std::sync::Arc;

use crate::hud::HotbarSlot;
use crate::hud::VanillaFont;
use crate::hud::item_icon::{
    self, ColourStream, IconAssets, IconRenderer, IconSink, SpecialIconDraw,
};

const FLOATS_PER_VERTEX: usize = 6;
const SLOT: f32 = 18.0;
const CELL: f32 = 16.0;

/// A pixel-space rectangle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rect {
    /// Left edge in pixels.
    pub x: f32,
    /// Top edge in pixels.
    pub y: f32,
    /// Width in pixels.
    pub w: f32,
    /// Height in pixels.
    pub h: f32,
}

/// One laid-out menu slot, in local widget coordinates.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlotRect {
    /// Menu-slot index.
    pub menu_index: usize,
    /// Left edge in local widget pixels.
    pub x: f32,
    /// Top edge in local widget pixels.
    pub y: f32,
    /// Width in pixels.
    pub w: f32,
    /// Height in pixels.
    pub h: f32,
}

/// Complete local layout for a menu.
#[derive(Debug, Clone, PartialEq)]
pub struct SlotLayout {
    /// Widget width in pixels.
    pub width: f32,
    /// Widget height in pixels.
    pub height: f32,
    /// Slot rectangles in menu-slot order.
    pub slots: Vec<SlotRect>,
}

/// The container screen to draw for one frame.
#[derive(Debug, Clone, Copy)]
pub struct ContainerFrame<'a> {
    /// Menu contents to draw. `None` draws nothing.
    pub menu: Option<&'a Menu>,
    /// The screen's own title, already resolved to words — vanilla's
    /// `AbstractContainerScreen.title`. Drawn at
    /// [`LabelLayout::title_x`]/[`title_y`](LabelLayout::title_y), which is
    /// **not** always `(8, 6)`: see [`label_layout`].
    ///
    /// For a server-opened container this is the `Text` from `OPEN_SCREEN` run
    /// through the language table ([`menu_title`]), so a chest renamed in an
    /// anvil opens as its custom name. Nothing here consults a table keyed on
    /// menu type; the generic name is only the server's default.
    pub title: &'a str,
    /// Vanilla's *second* label — `AbstractContainerScreen.playerInventoryTitle`,
    /// the word "Inventory" over the player's own storage rows.
    ///
    /// Unlike [`title`](Self::title) this never comes from a packet: vanilla
    /// reads it from `Inventory.getDisplayName()`, whose default is the
    /// client-side constant `Component.translatable("container.inventory")`
    /// (`Inventory.java:55`), so resolving it locally *is* the vanilla
    /// behaviour. The default below is `en_us.json:3218`'s value, which is what
    /// a jar-less run and every hermetic gate see; `app.rs` overrides it with
    /// the same key run through the live language table so a non-English client
    /// gets its own word.
    ///
    /// Drawn only when [`LabelLayout::inventory`] is `Some` — the player
    /// inventory screen omits it (`InventoryScreen.extractLabels`).
    pub inventory_label: &'a str,
    /// Viewport-pixel position of the mouse cursor, the same coordinate space
    /// [`hit_test`] takes — **not** local widget coordinates. `None` (the
    /// default from [`new`](Self::new)) draws no carried stack even if
    /// [`Menu::carried`] holds one, which is what keeps every existing caller
    /// (headless builds, the pixel gates, `tests/container_screen.rs`)
    /// unchanged: nothing here reads this field unless a caller opts in
    /// through [`with_cursor`](Self::with_cursor).
    pub cursor: Option<[f32; 2]>,
    /// The local recipe corpus (see `crate::resources::load_recipe_book`), for
    /// a **ghost preview** of the crafting result: `None` (the default) draws
    /// nothing extra, which is what keeps every existing caller (headless
    /// builds, the pixel gates, `tests/container_screen.rs`) unchanged. See
    /// [`with_recipe_book`](Self::with_recipe_book).
    pub recipe_book: Option<&'a RecipeBook>,
}

impl<'a> ContainerFrame<'a> {
    /// A frame for an optional menu, with no cursor position — the carried
    /// stack (if any) will not draw. Chain [`with_cursor`](Self::with_cursor)
    /// to supply one.
    #[must_use]
    pub fn new(menu: Option<&'a Menu>, title: &'a str) -> Self {
        Self {
            menu,
            title,
            inventory_label: DEFAULT_INVENTORY_LABEL,
            cursor: None,
            recipe_book: None,
        }
    }

    /// A frame that deliberately draws nothing.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            menu: None,
            title: "",
            inventory_label: DEFAULT_INVENTORY_LABEL,
            cursor: None,
            recipe_book: None,
        }
    }

    /// Override the player-inventory label with a translated one — see
    /// [`inventory_label`](Self::inventory_label) and
    /// [`player_inventory_label`].
    #[must_use]
    pub fn with_inventory_label(mut self, label: &'a str) -> Self {
        self.inventory_label = label;
        self
    }

    /// Attach the mouse position, in viewport pixels, so a loaded cursor
    /// (`menu.carried().is_some()`) draws the carried stack centred on it.
    #[must_use]
    pub fn with_cursor(mut self, cursor: Option<[f32; 2]>) -> Self {
        self.cursor = cursor;
        self
    }

    /// Attach a recipe book so an **empty** crafting result slot draws a
    /// dimmed ghost preview of what the grid would produce — never the real
    /// (undimmed) icon, and never written into `menu` itself. The server's own
    /// `container_set_slot` remains the only thing that ever fills the result
    /// slot for real; see `docs/crafting.md`'s "who computes the result slot".
    #[must_use]
    pub fn with_recipe_book(mut self, book: Option<&'a RecipeBook>) -> Self {
        self.recipe_book = book;
        self
    }
}

/// Resolve an open menu's server-authored title into the plain string
/// [`ContainerFrame::title`] draws.
///
/// A server does not send the words "Crafting"; it sends
/// `translate("container.crafting")` in `ClientboundOpenScreen`. Flattening that
/// with [`lodestone_model::Text::to_plain_string`] consults the model's tiny
/// built-in stub table (fourteen chat/death keys — `text.rs`'s
/// `default_translation`), which has no `container.*` entry, so the key falls
/// through to itself and **the raw key is what the panel draws** (issue #52).
///
/// This is the same read-boundary resolution the chat feed, the tab list and the
/// scoreboard sidebar already do; the container screen was the one HUD surface
/// that skipped it. `translate` is the language table — an
/// `lodestone_assets::Language` becomes one via `Language::translator`, and
/// `Sim::translator` hands out exactly that closure.
///
/// A missing key still falls back to the component's own `fallback`, then to the
/// key: losing a translation must never cost the title.
#[must_use]
pub fn menu_title(
    title: &lodestone_model::Text,
    translate: &dyn Fn(&str) -> Option<String>,
) -> String {
    lodestone_game::text::resolve_to_string(title, translate)
}

/// `en_us.json:3218`'s value for `container.inventory` — the fallback
/// [`ContainerFrame::inventory_label`] carries when no caller supplies a
/// translated one.
const DEFAULT_INVENTORY_LABEL: &str = "Inventory";

/// The player inventory screen's own title: **"Crafting"**, not "Inventory".
///
/// `InventoryScreen.java:28` passes `Component.translatable("container.crafting")`
/// to `super`, naming the 2×2 grid rather than the screen. This client used to
/// hardcode the string `"Inventory"` here (`app.rs`), which is wrong twice over:
/// wrong word, and — because it went in as the *title* — drawn at the title
/// anchor, which for this one screen is `x = 97` (`InventoryScreen.java:29`), not
/// `x = 8`.
///
/// Resolved through the language table for the same reason [`menu_title`] is: a
/// raw `container.crafting` on screen is issue #52's defect class.
#[must_use]
pub fn player_inventory_title(translate: &dyn Fn(&str) -> Option<String>) -> String {
    menu_title(
        &lodestone_model::Text::translate("container.crafting", vec![]),
        translate,
    )
}

/// Vanilla's `playerInventoryTitle` — `container.inventory`, "Inventory".
///
/// A *client-side* constant in vanilla too (`Inventory.java:55`'s `DEFAULT_NAME`),
/// so unlike a container's title this is legitimately resolved locally rather
/// than read off a packet. See [`ContainerFrame::inventory_label`].
#[must_use]
pub fn player_inventory_label(translate: &dyn Fn(&str) -> Option<String>) -> String {
    menu_title(
        &lodestone_model::Text::translate("container.inventory", vec![]),
        translate,
    )
}

/// Where a screen's two labels go, in **local widget pixels** (add
/// [`panel_origin`] to reach the canvas).
///
/// The reason this is a computed record and not four constants: `inventoryLabelY`
/// is `imageHeight - 94`, and `imageHeight` moves with the row count. Restating
/// it as a number is the exact failure `CLAUDE.md` documents for the HUD's
/// `cluster_top` — a gate measured 20 logical pixels above a row that was drawing
/// perfectly and reported zero.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LabelLayout {
    /// `titleLabelX`. `8` on a generic container, `29` on a crafting table,
    /// `97` on the player inventory screen.
    pub title_x: f32,
    /// `titleLabelY` — `6` everywhere in vanilla.
    pub title_y: f32,
    /// `(inventoryLabelX, inventoryLabelY)`, or `None` on the one screen that
    /// draws no such label.
    pub inventory: Option<[f32; 2]>,
}

/// Vanilla's label anchors for `menu`'s screen, derived from `layout` rather than
/// restated.
///
/// Read out of `.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/inventory/`:
///
/// | screen | `titleLabelX` | second label | source |
/// |---|---|---|---|
/// | generic container | `8` | yes | `AbstractContainerScreen.java:68-71` |
/// | crafting table | `29` | yes | `CraftingScreen.java:22` |
/// | player inventory | `97` | **no** | `InventoryScreen.java:29,73-75` |
///
/// The player inventory screen is the only one that omits the second label, and
/// it does so by *overriding `extractLabels`* to drop the second `graphics.text`
/// call entirely (`InventoryScreen.java:73-75`) — so the label is not wrong in
/// general, only there. Deleting it globally would trade one bug for another.
///
/// `inventory` is `[8, layout.height - 94]`: `inventoryLabelX = 8` and
/// `inventoryLabelY = imageHeight - 94` (`AbstractContainerScreen.java:70-71`,
/// restated by `ContainerScreen.java:17` for the row-count-dependent chest).
/// [`SlotLayout::height`] *is* `imageHeight` — 166 for the player and crafting
/// panels, `114 + rows * 18` for a chest, both matching vanilla's own
/// constructors — so this is the same expression the panel art is blitted with,
/// not a parallel one that can drift.
///
/// Not modelled: `AbstractFurnaceScreen.java:39` centres its title
/// (`(imageWidth - font.width(title)) / 2`), which is the only vanilla anchor
/// that depends on the *text*. There is no furnace [`MenuKind`] yet — a furnace
/// arrives here as a `Generic` and gets `x = 8`. When one is added, that branch
/// needs the measured title width and therefore the font, which this function
/// deliberately does not take.
#[must_use]
pub fn label_layout(menu: &Menu, layout: &SlotLayout) -> LabelLayout {
    match menu.kind() {
        MenuKind::Player => LabelLayout {
            title_x: 97.0,
            title_y: 6.0,
            inventory: None,
        },
        MenuKind::Generic { .. } => LabelLayout {
            title_x: if menu.craft_layout().is_some() { 29.0 } else { 8.0 },
            title_y: 6.0,
            inventory: Some([8.0, layout.height - 94.0]),
        },
    }
}

/// Vanilla's real container-background art (issue #51): `container/inventory`,
/// `container/crafting_table` and `container/generic_54`, stitched into one
/// small atlas.
///
/// Reproduced by hand rather than through
/// [`lodestone_render::GuiAtlas`](lodestone_render::GuiAtlas): these three PNGs
/// live at `textures/gui/container/**`, not `textures/gui/sprites/**`, so they
/// carry no sibling `.mcmeta` and vanilla does not scale them through any of
/// [`lodestone_assets::gui::GuiScaling`]'s three modes. Instead it blits
/// hand-placed sub-rectangles of each 256×256 sheet at native size —
/// `ContainerScreen.java:21-27` draws the chest background as *two* blits (the
/// row-count-dependent top part, then a fixed 96 px bottom part immediately
/// below it), `CraftingScreen.java:29-34` and `InventoryScreen.java:96-101` each
/// draw one whole-panel blit. `GuiScaling` has no variant for an arbitrary
/// sub-rect, so this reads the sheets' atlas placement directly and computes
/// the same UV windows vanilla's `blit` calls use, rather than forcing the
/// three-mode abstraction to do something it was never built for.
///
/// Deliberately GPU-free (mirrors [`lodestone_render::GuiAtlas`]'s own
/// producer/consumer split): [`ContainerBackground::build`] is the producer,
/// [`ContainerBackground::quads`] the pure consumer a test can call with no
/// device.
#[derive(Debug)]
pub struct ContainerBackground {
    atlas: Atlas,
    generic: ResourceLocation,
    crafting: ResourceLocation,
    inventory: ResourceLocation,
}

/// Which vanilla `container/*.png` sheet a menu's background draws from, and
/// (for the generic-chest case) how many rows are actually shown — vanilla
/// truncates the top blit's height to `rows * 18 + 17` rather than always
/// drawing all six.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BackgroundKind {
    Inventory,
    Crafting,
    Generic { rows: usize },
}

/// Mirrors [`slot_layout`]'s own dispatch: a menu with a [`Menu::craft_layout`]
/// draws the crafting table's background regardless of container size (today
/// that is always the 3×3 table), everything else generic draws the chest
/// sheet at its own row count, and [`MenuKind::Player`] draws the player
/// inventory sheet.
fn background_kind(menu: &Menu) -> BackgroundKind {
    match menu.kind() {
        MenuKind::Player => BackgroundKind::Inventory,
        MenuKind::Generic { container_size } => match menu.craft_layout() {
            Some(_) => BackgroundKind::Crafting,
            None => BackgroundKind::Generic {
                rows: container_size.div_ceil(9).clamp(1, 6),
            },
        },
    }
}

impl ContainerBackground {
    /// Loads and stitches the three sheets from a resource manager (in
    /// practice, `client.jar`).
    pub fn build(manager: &ResourceManager) -> Result<Self, AtlasError> {
        let generic = ResourceLocation::new("minecraft", "gui/container/generic_54")
            .expect("hardcoded location is always valid");
        let crafting = ResourceLocation::new("minecraft", "gui/container/crafting_table")
            .expect("hardcoded location is always valid");
        let inventory = ResourceLocation::new("minecraft", "gui/container/inventory")
            .expect("hardcoded location is always valid");
        let mut builder = AtlasBuilder::new();
        builder.load(manager, &generic)?;
        builder.load(manager, &crafting)?;
        builder.load(manager, &inventory)?;
        let atlas = builder.build()?;
        Ok(Self {
            atlas,
            generic,
            crafting,
            inventory,
        })
    }

    /// The stitched atlas, for GPU upload via
    /// [`GpuAtlas::from_atlas`](lodestone_render::GpuAtlas::from_atlas).
    #[must_use]
    pub fn atlas(&self) -> &Atlas {
        &self.atlas
    }

    /// The textured quad(s) vanilla's own `extractBackground` would blit for
    /// `menu`'s screen, with the panel's own top-left corner at `(x, y)` —
    /// see [`BackgroundKind`]'s doc comment for the Java call sites. `None`
    /// only if a sheet is missing from the atlas (never true of
    /// [`Self::build`]'s own output), which keeps this total rather than
    /// panicking on a hostile input.
    #[must_use]
    fn quads(&self, menu: &Menu, x: f32, y: f32) -> Option<Vec<GuiSpriteQuad>> {
        let (aw, ah) = (self.atlas.width as f32, self.atlas.height as f32);
        let uv = |loc: &ResourceLocation, local: [f32; 4]| -> Option<([f32; 2], [f32; 2])> {
            let sprite = self.atlas.sprite(loc)?;
            let [lx, ly, lw, lh] = local;
            Some((
                [(sprite.x as f32 + lx) / aw, (sprite.y as f32 + ly) / ah],
                [
                    (sprite.x as f32 + lx + lw) / aw,
                    (sprite.y as f32 + ly + lh) / ah,
                ],
            ))
        };
        match background_kind(menu) {
            BackgroundKind::Inventory => {
                let (uv_min, uv_max) = uv(&self.inventory, [0.0, 0.0, 176.0, 166.0])?;
                Some(vec![GuiSpriteQuad {
                    dst: [x, y, 176.0, 166.0],
                    uv_min,
                    uv_max,
                }])
            }
            BackgroundKind::Crafting => {
                let (uv_min, uv_max) = uv(&self.crafting, [0.0, 0.0, 176.0, 166.0])?;
                Some(vec![GuiSpriteQuad {
                    dst: [x, y, 176.0, 166.0],
                    uv_min,
                    uv_max,
                }])
            }
            BackgroundKind::Generic { rows } => {
                let top_h = (rows * 18 + 17) as f32;
                let (top_min, top_max) = uv(&self.generic, [0.0, 0.0, 176.0, top_h])?;
                let (bot_min, bot_max) = uv(&self.generic, [0.0, 126.0, 176.0, 96.0])?;
                Some(vec![
                    GuiSpriteQuad {
                        dst: [x, y, 176.0, top_h],
                        uv_min: top_min,
                        uv_max: top_max,
                    },
                    GuiSpriteQuad {
                        dst: [x, y + top_h, 176.0, 96.0],
                        uv_min: bot_min,
                        uv_max: bot_max,
                    },
                ])
            }
        }
    }
}

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
    /// art (issue #51). Empty unless a background was supplied; drawn on its
    /// own pipeline (a different atlas than [`item_verts`](Self::item_verts))
    /// in its own pass, **before** the chrome pass, so the panel/well fills
    /// this stream would otherwise draw are suppressed in favour of the real
    /// art's own baked-in slot wells.
    pub bg_verts: Vec<f32>,
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
    /// icon (issue #377).
    ///
    /// Equal to [`vertex_count`](Self::vertex_count) when nothing is carried, so
    /// the fourth range is simply empty.
    pub slot_vertex_count: usize,
    /// How many leading vertices of [`item_verts`](Self::item_verts) are slot
    /// icons; the remainder is the carried stack's flat sprite. See
    /// [`slot_vertex_count`](Self::slot_vertex_count).
    pub slot_item_vertex_count: usize,
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

    /// Builds container overlay geometry drawing **real item icons** from the
    /// atlases. `models` may be `None`, in which case flat sprite items draw and
    /// block items do not.
    #[must_use]
    pub fn build_with_icons(
        frame: &ContainerFrame<'_>,
        width: u32,
        height: u32,
        items: &ItemAtlas,
        models: Option<&BlockModels>,
    ) -> Self {
        Self::build_inner(
            frame,
            width,
            height,
            crate::config::AUTO_GUI_SCALE,
            &IconAssets {
                items: Some(items),
                models,
            },
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn build_inner(
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
                model_verts: Vec::new(),
                special: Vec::new(),
                bg_verts: Vec::new(),
                dim_vertex_count: 0,
                chrome_vertex_count: 0,
                slot_vertex_count: 0,
                slot_item_vertex_count: 0,
                slot_model_vertex_count: 0,
                slot_special_count: 0,
                widget_rect: None,
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
        let mut b = Builder::new(w, h, font);

        // Vanilla's own dim behind an open container screen (issue #61's
        // leftover). `AbstractContainerScreen::isInGameUi()` overrides `true`
        // (`AbstractContainerScreen.java:535-538`), which routes
        // `Screen::extractBackground` to `extractTransparentBackground`
        // (`Screen.java:375-377`) — a full-canvas vertical **gradient**, not the
        // pause menu's tiled dirt texture (that is the `else` branch, for
        // `isInGameUi() == false` screens). `-1072689136`/`-804253680` decoded:
        // ARGB (192,16,16,16) top to (208,16,16,16) bottom.
        //
        // This is what dims the HUD hotbar for free: the HUD draws unconditionally
        // behind any world-following screen (issue #61's `hud_follows_world`),
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

        // Vanilla's real `container/*.png` art (issue #51), if attached. `None`
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
        // Both labels, exactly as `AbstractContainerScreen::extractLabels` draws
        // them (`AbstractContainerScreen.java:189-191`):
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
        let labels = label_layout(menu, &layout);
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
        let chrome_floats = b.verts.len();

        for slot in &layout.slots {
            let sx = x + slot.x;
            let sy = y + slot.y;
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
        // (`AbstractContainerScreen.java:126`).
        //
        // It has to be a stratum and not merely "appended last" (issue #377).
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
            b.draw_stack(assets, stack, cx - CELL * 0.5, cy - CELL * 0.5);
        }

        Self {
            dim_vertex_count: dim_floats / FLOATS_PER_VERTEX,
            chrome_vertex_count: chrome_floats / FLOATS_PER_VERTEX,
            slot_vertex_count: slot_floats / FLOATS_PER_VERTEX,
            slot_item_vertex_count: slot_item_floats / crate::hud::SPRITE_FLOATS_PER_VERTEX,
            slot_model_vertex_count: slot_model_verts,
            slot_special_count: slot_special,
            verts: b.verts,
            item_verts: b.item_verts,
            model_verts: b.model_verts,
            special: b.special,
            bg_verts: b.bg_verts,
            widget_rect: Some(Rect {
                x,
                y,
                w: layout.width,
                h: layout.height,
            }),
        }
    }
}

/// Turn a menu slot's stack into the shared per-slot draw record, mirroring what
/// `app.rs` builds for the hotbar. `None` when the item id does not parse as a
/// [`ResourceLocation`], which no vanilla id does.
fn icon_record(stack: &lodestone_game::item::ItemStack) -> Option<HotbarSlot> {
    let item = ResourceLocation::parse(&stack.item().to_string()).ok()?;
    let damage = stack
        .components()
        .get_int(lodestone_game::item::DAMAGE_COMPONENT)
        .and_then(|v| u32::try_from(v).ok());
    let max_damage = stack
        .components()
        .get_int(lodestone_game::item::MAX_DAMAGE_COMPONENT)
        .and_then(|v| u32::try_from(v).ok());
    Some(HotbarSlot {
        item,
        count: stack.count().max(0) as u32,
        damage,
        max_damage,
        enchanted: false,
    })
}

/// Computes the slot layout in local widget coordinates.
///
/// The [`MenuKind`] match stays exhaustive over two variants; the crafting
/// screen is reached *additively* through [`Menu::craft_layout`], which is
/// exactly why that descriptor was put on [`Menu`] instead of in `MenuKind`. A
/// crafting table is a `Generic { container_size: 10 }` whose result and 3×3
/// grid happen to be its first ten slots, and laying those out as a 9-wide run
/// (which is what a plain container would do) puts the result slot in the middle
/// of the grid.
#[must_use]
pub fn slot_layout(menu: &Menu) -> SlotLayout {
    match menu.kind() {
        MenuKind::Player => player_layout(),
        MenuKind::Generic { container_size } => match menu.craft_layout() {
            Some(craft) => crafting_layout(craft, container_size),
            None => generic_layout(container_size),
        },
    }
}

/// Appends the 27-slot main inventory (9-wide rows starting at `base`) and the
/// 9-slot hotbar 58px below the last main row — the standard vanilla
/// arrangement shared by every screen that shows the player's own inventory
/// below its container-specific slots. Returns the hotbar's y so callers can
/// size their panel around it.
fn append_main_inventory(slots: &mut Vec<SlotRect>, base: usize, main_y: f32) -> f32 {
    for i in 0..27 {
        slots.push(slot(
            base + i,
            8.0 + (i % 9) as f32 * SLOT,
            main_y + (i / 9) as f32 * SLOT,
        ));
    }
    let hotbar_y = main_y + 58.0;
    for i in 0..9 {
        slots.push(slot(base + 27 + i, 8.0 + i as f32 * SLOT, hotbar_y));
    }
    hotbar_y
}

fn player_layout() -> SlotLayout {
    let mut slots = Vec::with_capacity(46);
    slots.push(slot(0, 154.0, 28.0));
    for i in 0..4 {
        slots.push(slot(
            1 + i,
            98.0 + (i % 2) as f32 * SLOT,
            18.0 + (i / 2) as f32 * SLOT,
        ));
    }
    for i in 0..4 {
        slots.push(slot(5 + i, 8.0, 8.0 + i as f32 * SLOT));
    }
    append_main_inventory(&mut slots, 9, 84.0);
    slots.push(slot(45, 77.0, 62.0));
    SlotLayout {
        width: 176.0,
        height: 166.0,
        slots,
    }
}

fn generic_layout(container_size: usize) -> SlotLayout {
    let cols = 9usize;
    let rows = container_size.div_ceil(cols).max(1);
    let mut slots = Vec::with_capacity(container_size + 36);
    for i in 0..container_size {
        slots.push(slot(
            i,
            8.0 + (i % cols) as f32 * SLOT,
            18.0 + (i / cols) as f32 * SLOT,
        ));
    }
    let main_y = 18.0 + rows as f32 * SLOT + 14.0;
    let hotbar_y = append_main_inventory(&mut slots, container_size, main_y);
    SlotLayout {
        width: 176.0,
        height: hotbar_y + 24.0,
        slots,
    }
}

/// The crafting-table arrangement: the input grid top-left, the take-only
/// result slot to its right, then the player's main storage and hotbar below.
///
/// The constants are vanilla's `crafting_table.png` slot origins for the 3×3
/// case — grid at `(30, 17)`, result at `(124, 35)`, main at `(8, 84)`, hotbar at
/// `(8, 142)`, panel `176x166` — expressed in terms of the grid's real
/// dimensions so a differently sized grid (none ships in vanilla) still lands
/// somewhere sane rather than on top of the inventory.
///
/// The result slot is drawn but never *filled* here: a vanilla server computes
/// the crafting result itself and pushes it as a `container_set_slot` for slot
/// 0, which `Menus::apply` reconciles into the menu. Reading `slot_item` is
/// therefore reading server truth; matching a recipe locally to fill this slot
/// would overwrite it with a guess.
fn crafting_layout(craft: CraftLayout, container_size: usize) -> SlotLayout {
    let grid_x = 30.0;
    let grid_y = 17.0;
    let cols = craft.width.max(1);
    let rows = craft.height.max(1);
    let mut slots = Vec::with_capacity(container_size + 36);

    slots.push(slot(
        craft.result_slot,
        grid_x + cols as f32 * SLOT + 40.0,
        grid_y + (rows as f32 - 1.0) * SLOT * 0.5,
    ));
    for i in 0..craft.cell_count() {
        slots.push(slot(
            craft.first_input + i,
            grid_x + (i % cols) as f32 * SLOT,
            grid_y + (i / cols) as f32 * SLOT,
        ));
    }

    let main_y = (grid_y + rows as f32 * SLOT + 13.0).max(84.0);
    let hotbar_y = append_main_inventory(&mut slots, container_size, main_y);
    SlotLayout {
        width: 176.0,
        height: hotbar_y + 24.0,
        slots,
    }
}

/// Where the panel's top-left corner lands, in the **logical** GUI canvas —
/// `width`/`height` in physical framebuffer pixels, divided by the effective
/// GUI scale exactly as [`crate::menu::render::logical_canvas`] does for the
/// menu screens (reused here rather than a second scale computation). [`hit_test`]
/// converts an incoming physical cursor position down to this same logical
/// space before comparing against it, which is what keeps the two agreeing.
///
/// The single source of the centring offset. [`ContainerGeometry::build_inner`]
/// and [`hit_test`] must agree to the pixel or the screen and the mouse disagree
/// about which slot is which — a bug that reads as "clicks land one slot off"
/// and is invisible in any screenshot.
///
/// Always lays out against [`crate::config::AUTO_GUI_SCALE`]. Use
/// [`panel_origin_with_scale`] to lay out against a specific (e.g. persisted
/// manual) `gui_scale` instead — this is a thin wrapper over it, kept so every
/// existing caller of this exact signature (the pixel gates,
/// `tests/container_screen.rs`) is unaffected.
#[must_use]
pub fn panel_origin(layout: &SlotLayout, width: u32, height: u32) -> (f32, f32) {
    panel_origin_with_scale(layout, crate::config::AUTO_GUI_SCALE, width, height)
}

/// As [`panel_origin`], but against an explicit `gui_scale` (`0` = auto) rather
/// than always auto. `app.rs`'s real windowed render/hit-test path uses this
/// with the persisted `Options.gui_scale` so a manual scale setting moves the
/// drawn panel and the click hit-rects together — see [`hit_test_with_scale`].
#[must_use]
pub fn panel_origin_with_scale(
    layout: &SlotLayout,
    gui_scale: u32,
    width: u32,
    height: u32,
) -> (f32, f32) {
    let (w, h) = crate::menu::render::logical_canvas(gui_scale, width, height);
    (
        ((w - layout.width) * 0.5).max(8.0),
        ((h - layout.height) * 0.5).max(8.0),
    )
}

/// What a viewport pixel is over, in an open menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuHit {
    /// A menu slot, by its real menu-slot index — feed it straight to
    /// [`Click`](lodestone_game::click::Click).
    Slot(usize),
    /// Inside the panel but not over a slot. Vanilla does **nothing** here; it
    /// is deliberately not a drop.
    Panel,
    /// Outside the panel. Vanilla treats a click here as the outside-slot
    /// sentinel (`-999`), i.e. throw the cursor stack into the world.
    Outside,
}

/// Resolves a viewport pixel to a menu slot, mirroring vanilla
/// `AbstractContainerScreen.findSlot` / `hasClickedOutside`.
///
/// The hit rect is vanilla's `isHovering(x, y, 16, 16, …)`: the 16×16 cell grown
/// by one pixel on every side, which is exactly the 18×18 well this module
/// draws — so the clickable area and the visible area are the same rectangle by
/// construction rather than by coincidence.
///
/// `x`/`y` are raw **physical** viewport pixels — the same space `width`/
/// `height` and the cursor position `app.rs` tracks are already in. This module
/// *does* apply a GUI scale of its own (the same effective scale
/// [`crate::config::calculate_gui_scale`] picks for [`panel_origin`] and the
/// drawn geometry): the incoming physical cursor is divided down to the same
/// logical space the widget was laid out in before anything is compared, so a
/// caller does not need to pre-scale the cursor itself.
#[must_use]
pub fn hit_test(menu: &Menu, width: u32, height: u32, x: f32, y: f32) -> MenuHit {
    hit_test_with_scale(menu, crate::config::AUTO_GUI_SCALE, width, height, x, y)
}

/// As [`hit_test`], but against an explicit `gui_scale` (`0` = auto). Must be
/// called with the **same** `gui_scale` the frame was last drawn with — see
/// [`panel_origin_with_scale`] — or clicks land on the wrong slot while the
/// screen still looks correct, exactly the class of bug this module's own
/// docs warn about.
#[must_use]
pub fn hit_test_with_scale(
    menu: &Menu,
    gui_scale: u32,
    width: u32,
    height: u32,
    x: f32,
    y: f32,
) -> MenuHit {
    let layout = slot_layout(menu);
    let (px, py) = panel_origin_with_scale(&layout, gui_scale, width, height);
    let scale = crate::config::calculate_gui_scale(gui_scale, width, height).max(1) as f32;
    let local_x = x / scale - px;
    let local_y = y / scale - py;
    if local_x < 0.0 || local_y < 0.0 || local_x >= layout.width || local_y >= layout.height {
        return MenuHit::Outside;
    }
    for rect in &layout.slots {
        if local_x >= rect.x - 1.0
            && local_x < rect.x + rect.w + 1.0
            && local_y >= rect.y - 1.0
            && local_y < rect.y + rect.h + 1.0
        {
            return MenuHit::Slot(rect.menu_index);
        }
    }
    MenuHit::Panel
}

/// Which mouse button a menu gesture used.
///
/// `Pick` is vanilla's `keyPickItem` (middle-click by default), which only does
/// anything with infinite materials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuButton {
    /// Primary / left.
    Left,
    /// Secondary / right.
    Right,
    /// Pick-block (middle by default).
    Pick,
}

impl MenuButton {
    /// The raw button number vanilla puts in the packet for an ordinary click.
    fn number(self) -> i32 {
        match self {
            Self::Left | Self::Pick => 0,
            Self::Right => 1,
        }
    }

    /// The drag distribution type this button paints with
    /// ([`drag_type`](lodestone_game::click::drag_type)).
    fn drag_kind(self) -> i32 {
        match self {
            Self::Left => drag_type::EVEN,
            Self::Right => drag_type::ONE,
            Self::Pick => drag_type::CLONE,
        }
    }
}

/// What the caller must tell the input machine about the menu at gesture time.
#[derive(Debug, Clone, Copy)]
pub struct MenuContext {
    /// Whether the cursor (carried stack) currently holds something. Read it off
    /// the *predicted* menu: `menu.carried().is_some()`.
    pub cursor_loaded: bool,
    /// Whether the player has infinite materials (creative), which enables
    /// pick-block cloning and the stack-per-slot drag type.
    pub creative: bool,
}

/// The GUI-side press/drag/release protocol, turning mouse events into the
/// [`Click`]s `Menus::click` expects.
///
/// This is the piece between [`hit_test`] and
/// [`Menus::click`](lodestone_game::menus::Menus::click), and it exists as a state
/// machine rather than a `fn(hit) -> Click` because **vanilla does not send a
/// click on mouse-down when the cursor is loaded**. Read
/// `AbstractContainerScreen.mouseClicked`: with a non-empty carried stack it only
/// sets `isQuickCrafting` and sends *nothing*; the packet goes out on
/// `mouseReleased`, as either a plain `PICKUP` (if the mouse never moved onto a
/// slot) or the `QUICK_CRAFT` start/add…/end sequence (if it did). A naive
/// press-to-`PICKUP` mapper looks right for every single-slot interaction and
/// silently loses the entire paint-drag gesture — the "distribute one item per
/// slot" right-drag most players use to fill a crafting grid.
///
/// The empty-cursor half *is* sent on press (`PICKUP` / `QUICK_MOVE` / `CLONE`),
/// and vanilla's `skipNextRelease` then suppresses the release, which is what
/// [`skip_next_release`](Self::press) models.
///
/// Ordering contract: [`press`](Self::press), then zero or more
/// [`dragged`](Self::dragged), then [`release`](Self::release). `dragged` never
/// emits — vanilla accumulates painted slots and sends the whole sequence from
/// `quickCraftToSlots` at release.
#[derive(Debug, Clone, Default)]
pub struct MenuInput {
    /// The button that armed a paint-drag, and the slots painted so far.
    drag: Option<(MenuButton, Vec<usize>)>,
    /// Set when the press already sent a click, so the release must not send one.
    skip_next_release: bool,
    /// Slot the previous press landed on, for double-click detection.
    last_slot: Option<usize>,
    /// The pending release should gather (`PICKUP_ALL`) instead.
    double_click: bool,
    /// Mirrors vanilla `AbstractContainerScreen.lastQuickMoved`: the stack
    /// held by the slot a `QUICK_MOVE` click was just sent for, or `None` for
    /// vanilla's `ItemStack.EMPTY`. Set at both the sites vanilla sets it —
    /// `:312` in [`press`](Self::press) and `:426` in [`release`](Self::release)
    /// — and read by the shift+double-click gather in `release`, which moves
    /// every slot matching *this* stack rather than gathering onto the
    /// cursor.
    last_quick_moved: Option<ItemStack>,
}

impl MenuInput {
    /// A fresh input machine with nothing armed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a paint-drag is currently armed. While this is true the screen
    /// should draw the drag preview rather than a hover highlight.
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Mouse-button press. Returns the clicks to send **now** (empty for the
    /// loaded-cursor case, which sends on release).
    ///
    /// `is_repeat` is the platform's double-click flag; combined with hitting the
    /// same slot twice it arms the gather that fires on release.
    ///
    /// `menu` is read only to capture `last_quick_moved` off the slot about to
    /// be quick-moved (vanilla `AbstractContainerScreen.java:312`) — it does
    /// not otherwise change what this method sends.
    pub fn press(
        &mut self,
        hit: MenuHit,
        button: MenuButton,
        shift: bool,
        ctx: MenuContext,
        is_repeat: bool,
        menu: &Menu,
    ) -> Vec<Click> {
        let cloning = button == MenuButton::Pick && ctx.creative;
        let slot_hit = match hit {
            MenuHit::Slot(i) => Some(i),
            _ => None,
        };
        self.double_click = is_repeat && slot_hit.is_some() && self.last_slot == slot_hit;
        self.last_slot = slot_hit;
        self.skip_next_release = false;
        self.drag = None;

        // A press with `Pick` and no infinite materials is vanilla's hotbar-rebind
        // path, which sends no container click at all.
        if button == MenuButton::Pick && !cloning {
            return Vec::new();
        }

        // Inside the panel but not over a slot: vanilla's `slotId` stays -1 and the
        // whole branch is skipped. Deliberately *not* a drop.
        let slot = match hit {
            MenuHit::Slot(i) => i as i32,
            MenuHit::Outside => OUTSIDE_SLOT,
            MenuHit::Panel => return Vec::new(),
        };

        if ctx.cursor_loaded {
            // Arm a paint-drag and send nothing; the release decides.
            self.drag = Some((button, Vec::new()));
            return Vec::new();
        }

        self.skip_next_release = true;
        // `quickKey` in vanilla: a shift-click on a real slot. Captured before
        // the `if` chain below because vanilla's own assignment
        // (`AbstractContainerScreen.java:312`) happens as a side effect of
        // computing this same condition, and `cloning` takes priority over it
        // there (the two are mutually exclusive `if`/`else` arms, not just
        // independent conditions).
        let quick_key = !cloning && shift && slot != OUTSIDE_SLOT;
        if quick_key {
            // An empty slot records vanilla's `ItemStack.EMPTY`, modelled here
            // as `None`.
            self.last_quick_moved = match hit {
                MenuHit::Slot(i) => menu.slot_item(i).cloned(),
                _ => None,
            };
        }
        let input = if cloning {
            ContainerInput::Clone
        } else if quick_key {
            ContainerInput::QuickMove
        } else if slot == OUTSIDE_SLOT {
            // Vanilla sends THROW at -999 here. The server no-ops it (its THROW
            // branch requires `slotIndex >= 0`), but sending what vanilla sends
            // keeps the packet stream identical rather than merely equivalent.
            ContainerInput::Throw
        } else {
            ContainerInput::Pickup
        };
        vec![Click {
            slot,
            button: button.number(),
            input,
        }]
    }

    /// The cursor moved to `hit` with the button still down. Records a painted
    /// slot; never emits.
    ///
    /// Filtering (cursor has enough items, the slot may accept them) is left to
    /// [`Menu::do_click`](lodestone_game::menu::Menu)'s own `can_drag_place`,
    /// which both sides run — an `ADD` the server rejects is simply not recorded
    /// there, so painting liberally cannot desynchronise.
    pub fn dragged(&mut self, hit: MenuHit) {
        let MenuHit::Slot(i) = hit else {
            return;
        };
        let Some((_, slots)) = self.drag.as_mut() else {
            return;
        };
        if !slots.contains(&i) {
            slots.push(i);
        }
    }

    /// Mouse-button release. Returns the clicks to send.
    ///
    /// `menu` gates the double-click gather branch and (for the shift variant)
    /// supplies the slots to sweep — see [`gather_shift_matches`](Self::gather_shift_matches)
    /// — and also captures `last_quick_moved` for the plain shift-click path,
    /// at the second of the two sites vanilla sets it
    /// (`AbstractContainerScreen.java:426`; the first is [`press`](Self::press)).
    pub fn release(
        &mut self,
        hit: MenuHit,
        button: MenuButton,
        shift: bool,
        ctx: MenuContext,
        menu: &Menu,
    ) -> Vec<Click> {
        let drag = self.drag.take();
        let gather = std::mem::take(&mut self.double_click);
        let skip = std::mem::take(&mut self.skip_next_release);

        // A release on a different button than the one that armed the drag cancels
        // it outright (vanilla returns early and swallows the next release too).
        if drag.as_ref().is_some_and(|(armed, _)| *armed != button) {
            self.skip_next_release = true;
            return Vec::new();
        }

        if gather && button == MenuButton::Left {
            if let MenuHit::Slot(i) = hit {
                // `AbstractContainerScreen.java:387`: the whole gather branch
                // (both this and the shift variant below) is gated on
                // `menu.canTakeItemForPickAll(ItemStack.EMPTY, slot)`. Every
                // result-bearing menu overrides that to exclude its own
                // result container (`Menu::can_take_for_pick_all` in
                // lodestone-game — private, so recomputed here from what the
                // shell already has; its server-side effect is covered by
                // `pickup_all_never_drains_the_crafting_result` in
                // `lodestone-game`). This is **not** a desync fix: a real
                // server honours a PICKUP_ALL/QUICK_MOVE aimed at the result
                // slot regardless, since `Menu::do_click` has no such gate —
                // skipping the packet here only suppresses non-vanilla client
                // UX, matching double-clicking a crafting result silently
                // sending nothing, as it does in the real game.
                let allowed = menu.craft_layout().is_none_or(|l| i != l.result_slot);
                if allowed {
                    return if shift {
                        self.gather_shift_matches(menu, i)
                    } else {
                        vec![Click::double(i)]
                    };
                }
                // Not allowed: fall through to the ordinary release handling
                // below, exactly as vanilla's `if` failing falls into its
                // `else` — the gather is skipped, not replaced with nothing.
            }
        }
        if skip {
            return Vec::new();
        }

        let painted = drag.map(|(_, slots)| slots).unwrap_or_default();
        if !painted.is_empty() {
            let kind = button.drag_kind();
            let mut clicks = Vec::with_capacity(painted.len() + 2);
            clicks.push(quick_craft(OUTSIDE_SLOT, drag_header::START, kind));
            for i in painted {
                clicks.push(quick_craft(i as i32, drag_header::ADD, kind));
            }
            clicks.push(quick_craft(OUTSIDE_SLOT, drag_header::END, kind));
            return clicks;
        }

        if !ctx.cursor_loaded {
            return Vec::new();
        }
        let slot = match hit {
            MenuHit::Slot(i) => i as i32,
            MenuHit::Outside => OUTSIDE_SLOT,
            MenuHit::Panel => return Vec::new(),
        };
        let clone_click = button == MenuButton::Pick && ctx.creative;
        // `AbstractContainerScreen.java:426`: the second `lastQuickMoved`
        // site, inside the (non-clone) loaded-cursor release path — mirrored
        // in `press` for the empty-cursor press path.
        let quick_key = !clone_click && shift && slot != OUTSIDE_SLOT;
        if quick_key {
            self.last_quick_moved = match hit {
                MenuHit::Slot(i) => menu.slot_item(i).cloned(),
                _ => None,
            };
        }
        let input = if clone_click {
            ContainerInput::Clone
        } else if quick_key {
            ContainerInput::QuickMove
        } else {
            ContainerInput::Pickup
        };
        vec![Click {
            slot,
            button: button.number(),
            input,
        }]
    }

    /// `AbstractContainerScreen.java:388-398`: shift+double-click does not
    /// gather onto the cursor — it sends one `QUICK_MOVE` per slot that is in
    /// the **same backing container** as the double-clicked slot, may be
    /// picked up, is non-empty, and matches `last_quick_moved`
    /// (`target.mayPickup(player) && target.hasItem() && target.container ==
    /// slot.container && canItemQuickReplace(target, lastQuickMoved, true)`).
    ///
    /// `target.container == slot.container` compares the **backing
    /// container** (`Slot::container`, an index into `Menu`'s container
    /// list), not the menu — getting this wrong would let a shift+double-click
    /// in a chest sweep the player's own inventory, or vice versa, since both
    /// live in the same `Menu`.
    ///
    /// `canItemQuickReplace(target, lastQuickMoved, true)` is called here only
    /// once `target.hasItem()` is already known true, at which point its
    /// `ignoreSize` argument (`true`) drops the remaining size check
    /// entirely, so it reduces to `isSameItemSameComponents(lastQuickMoved,
    /// target.getItem())`.
    fn gather_shift_matches(&self, menu: &Menu, origin: usize) -> Vec<Click> {
        let Some(last) = self.last_quick_moved.as_ref() else {
            return Vec::new();
        };
        let Some(origin_container) = menu.slot(origin).map(|s| s.container) else {
            return Vec::new();
        };
        let mut clicks = Vec::new();
        for target in 0..menu.slot_count() {
            if menu.slot(target).is_none_or(|s| s.container != origin_container) {
                continue;
            }
            if !menu.may_pickup(target) {
                continue;
            }
            let Some(target_item) = menu.slot_item(target) else {
                continue;
            };
            if !ItemStack::is_same_item_same_components(target_item, last) {
                continue;
            }
            clicks.push(Click::shift(target));
        }
        clicks
    }
}

fn quick_craft(slot: i32, header: i32, kind: i32) -> Click {
    Click {
        slot,
        button: quick_craft_mask(header, kind),
        input: ContainerInput::QuickCraft,
    }
}

fn slot(menu_index: usize, x: f32, y: f32) -> SlotRect {
    SlotRect {
        menu_index,
        x,
        y,
        w: CELL,
        h: CELL,
    }
}

fn item_label(path: &str) -> String {
    path.rsplit(['/', '_'])
        .find(|part| !part.is_empty())
        .and_then(|part| part.chars().next())
        .unwrap_or('?')
        .to_ascii_uppercase()
        .to_string()
}

fn item_color(path: &str) -> [f32; 4] {
    let mut hash = 0u32;
    for b in path.bytes() {
        hash = hash.wrapping_mul(33).wrapping_add(u32::from(b));
    }
    let hue = hash as f32 / u32::MAX as f32;
    let r = 0.35 + 0.35 * (hue * std::f32::consts::TAU).sin().abs();
    let g = 0.35 + 0.35 * ((hue + 0.33) * std::f32::consts::TAU).sin().abs();
    let b = 0.35 + 0.35 * ((hue + 0.66) * std::f32::consts::TAU).sin().abs();
    [r, g, b, 0.95]
}

/// The overlay's four vertex streams, filled in one pass over the layout. The
/// colour stream is this module's own; the item-sprite and block-model streams
/// are the shared hotbar ones (see [`crate::hud::item_icon`]); the background
/// stream samples [`ContainerBackground`]'s own atlas.
#[derive(Debug)]
struct Builder<'a> {
    w: f32,
    h: f32,
    verts: Vec<f32>,
    item_verts: Vec<f32>,
    model_verts: Vec<ModelVertex>,
    /// Special-renderer (block-entity) icons; see [`ContainerGeometry::special`].
    special: Vec<SpecialIconDraw>,
    /// Flat `[x, y, u, v, r, g, b, a]` per vertex, off
    /// [`ContainerBackground`]'s atlas.
    bg_verts: Vec<f32>,
    /// The vanilla proportional font, for stack counts. `None` on a jar-less
    /// run, where [`item_icon::draw_item_icon`] falls back to the fixed-advance
    /// 5×7 debug font — the same degradation the HUD's own text uses.
    font: Option<&'a VanillaFont>,
}

impl<'a> Builder<'a> {
    fn new(w: f32, h: f32, font: Option<&'a VanillaFont>) -> Self {
        Self {
            w,
            h,
            verts: Vec::new(),
            item_verts: Vec::new(),
            model_verts: Vec::new(),
            special: Vec::new(),
            bg_verts: Vec::new(),
            font,
        }
    }

    fn rect_px(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        self.colour().rect(x, y, w, h, c);
    }

    /// A pixel-space rectangle with a vertical gradient from `top` (its own top
    /// edge) to `bottom` (its bottom edge) — see [`ColourStream::gradient_rect`].
    fn gradient_rect_px(&mut self, x: f32, y: f32, w: f32, h: f32, top: [f32; 4], bottom: [f32; 4]) {
        self.colour().gradient_rect(x, y, w, h, top, bottom);
    }

    /// One [`GuiSpriteQuad`] onto the background stream, untinted.
    fn bg_sprite(&mut self, q: GuiSpriteQuad) {
        let (w, h) = (self.w, self.h);
        item_icon::push_sprite_quad(&mut self.bg_verts, w, h, q, [1.0, 1.0, 1.0, 1.0]);
    }

    fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        self.colour().text(s, x, y, scale, c);
    }

    /// One of vanilla's two container labels: the **proportional** font when one
    /// is attached, and **no drop shadow** either way — the trailing `false` in
    /// `AbstractContainerScreen.java:190-191`'s `graphics.text` calls. Every
    /// other text surface in this crate is shadowed, which is why this needs its
    /// own entry point rather than reusing `VanillaFont::draw`.
    ///
    /// Degrades to the fixed-advance 5×7 debug font on a jar-less run, the same
    /// way stack counts do — advances will be wrong, but the words are readable
    /// and the anchor is identical, so the geometry gate still measures the same
    /// thing.
    fn label(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        match self.font {
            Some(f) => {
                let mut cs = self.colour();
                f.draw_plain(&mut cs, s, x, y, scale, c);
            }
            None => self.colour().text(s, x, y, scale, c),
        }
    }

    /// A handle onto the colour stream, for the shared pixel-space primitives.
    fn colour(&mut self) -> ColourStream<'_> {
        ColourStream {
            w: self.w,
            h: self.h,
            verts: &mut self.verts,
        }
    }

    /// One slot's real icon, through the shared pass.
    fn item_icon(
        &mut self,
        assets: &IconAssets<'_>,
        record: &HotbarSlot,
        x: f32,
        y: f32,
        size: f32,
    ) {
        let (w, h) = (self.w, self.h);
        let mut sink = IconSink {
            colour: ColourStream {
                verts: &mut self.verts,
                w,
                h,
            },
            sprite: &mut self.item_verts,
            model: &mut self.model_verts,
            special: &mut self.special,
        };
        item_icon::draw_item_icon(&mut sink, assets, (w, h), record, x, y, size, self.font);
    }

    /// Draw one occupied cell's contents at `(x, y)`: the real icon when the
    /// item resolves against an attached atlas, else the hash-derived
    /// swatch-and-letter fallback. Shared by the per-slot loop and the carried
    /// stack, so an atlas-less run shows the cursor's stack exactly as it
    /// shows an occupied well.
    fn draw_stack(&mut self, assets: &IconAssets<'_>, stack: &lodestone_game::item::ItemStack, x: f32, y: f32) {
        match (assets.items, icon_record(stack)) {
            // The real thing: the shared hotbar icon pass, which also draws
            // the stack count and the durability bar.
            (Some(_), Some(record)) => self.item_icon(assets, &record, x, y, CELL),
            // No atlas (or an item id the atlas could never key): the old
            // hash-derived swatch plus a letter, so an occupied cell still
            // reads as occupied on a jar-less run.
            _ => {
                let color = item_color(stack.item().path());
                self.rect_px(x + 3.0, y + 3.0, 10.0, 10.0, color);
                let label = item_label(stack.item().path());
                self.text(&label, x + 5.0, y + 5.0, 1.0, [0.97, 0.95, 0.86, 1.0]);
                if stack.count() > 1 {
                    self.text(
                        &stack.count().to_string(),
                        x + 8.0,
                        y + 10.0,
                        1.0,
                        [0.98, 0.98, 0.92, 1.0],
                    );
                }
            }
        }
    }
}

/// GPU renderer for the container overlay.
#[derive(Debug)]
pub struct ContainerRenderer {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
    /// The flat item atlas and the 3-D block-item pass, shared verbatim with the
    /// hotbar. Both halves start detached, so [`render`](Self::render) alone
    /// keeps the pre-icon behaviour.
    icons: IconRenderer,
    /// The vanilla proportional font, resolved once per process exactly like
    /// [`HudRenderer`](crate::hud::HudRenderer)'s. `None` on a jar-less run,
    /// where stack counts draw with the fixed-advance debug font.
    font: Option<Arc<VanillaFont>>,
    /// Vanilla's real `container/*.png` panel art (issue #51). Starts detached,
    /// so [`render`](Self::render)/[`render_with_icons`](Self::render_with_icons)
    /// alone keep the pre-texture flat-fill behaviour — the jar-less path and
    /// the negative control the pixel gate leans on.
    background: Option<ContainerBackgroundGpu>,
}

/// The GPU half of [`ContainerBackground`]: its own tiny textured pipeline,
/// sampling a **different** atlas than [`IconRenderer`]'s item-sprite pass, so
/// it cannot share that pipeline or bind group.
#[derive(Debug)]
struct ContainerBackgroundGpu {
    /// Kept alive because the bind group's texture view is derived from it, and
    /// so [`ContainerBackground::quads`] stays reachable from the render path.
    data: Arc<ContainerBackground>,
    #[allow(dead_code)]
    gpu: GpuAtlas,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
}

impl ContainerRenderer {
    /// Builds the overlay pipeline.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("container-shader"),
            source: wgpu::ShaderSource::Wgsl(CONTAINER_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("container-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("container-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (FLOATS_PER_VERTEX * 4) as wgpu::BufferAddress,
                    step_mode: wgpu::VertexStepMode::Vertex,
                    attributes: &[
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x2,
                            offset: 0,
                            shader_location: 0,
                        },
                        wgpu::VertexAttribute {
                            format: wgpu::VertexFormat::Float32x4,
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
        let capacity_floats = 4096;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("container-verts"),
            size: (capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self {
            pipeline,
            buffer,
            capacity_floats,
            icons: IconRenderer::new(),
            font: VanillaFont::shared(),
            background: None,
        }
    }

    /// Attach vanilla's real `container/*.png` panel art (issue #51), so the
    /// screen draws the real texture instead of the flat programmatic fill.
    /// Independent of [`attach_items`](Self::attach_items)/
    /// [`attach_item_models`](Self::attach_item_models) — an atlas-less run can
    /// still have the real panel art (or vice versa).
    pub fn attach_background(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        background: Arc<ContainerBackground>,
    ) {
        let gpu = GpuAtlas::from_atlas(device, queue, background.atlas());
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("container-bg-shader"),
            source: wgpu::ShaderSource::Wgsl(CONTAINER_BG_WGSL.into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("container-bg-bgl"),
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
            label: Some("container-bg-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("container-bg-pipeline"),
            layout: Some(&layout),
            vertex: wgpu::VertexState {
                module: &shader,
                entry_point: Some("vs_main"),
                buffers: &[Some(wgpu::VertexBufferLayout {
                    array_stride: (8 * 4) as wgpu::BufferAddress,
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
            label: Some("container-bg-bind"),
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
        let capacity_floats = 512;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("container-bg-verts"),
            size: (capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.background = Some(ContainerBackgroundGpu {
            data: background,
            gpu,
            pipeline,
            bind_group,
            buffer,
            capacity_floats,
        });
    }

    /// Whether the real vanilla `container/*.png` art is bound — the gate for
    /// "this screen looks like vanilla" (issue #51) must assert this, exactly
    /// as [`MenuRenderer::gui_attached`](crate::menu::render::MenuRenderer::gui_attached)
    /// gates the title/pause screens' buttons: without it a missing jar
    /// silently degrades to the flat-fill fallback and a coverage-only
    /// assertion still passes.
    #[must_use]
    pub fn background_attached(&self) -> bool {
        self.background.is_some()
    }

    /// Whether the vanilla proportional font resolved — the second half of "this
    /// screen looks like vanilla". Without it the two container labels fall back
    /// to the fixed-advance 5×7 debug font, which is *legible* and therefore
    /// invisible to a coverage assertion: exactly how issue #370's "wrong font"
    /// survived. A gate asserting typeface must assert this first.
    #[must_use]
    pub fn font_attached(&self) -> bool {
        self.font.is_some()
    }

    /// Attach the flat item-sprite [`ItemAtlas`] so container slots draw real
    /// item icons instead of the colour-swatch fallback. Mirrors
    /// [`HudRenderer::attach_items`](crate::hud::HudRenderer::attach_items) and
    /// costs a second upload of the (small) item atlas; the *block* atlas, the
    /// expensive one, is borrowed rather than uploaded by
    /// [`attach_item_models`](Self::attach_item_models).
    pub fn attach_items(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        atlas: Arc<ItemAtlas>,
    ) {
        self.icons
            .attach_items(device, queue, color_format, atlas, "container-item");
    }

    /// Attach the **3-D block-item** pass, so container slots holding a block
    /// draw vanilla's isometric mini-block. Every resource is borrowed from the
    /// world renderer — the same block atlas, tint palette and animation slots
    /// the terrain and the hotbar use.
    pub fn attach_item_models(
        &mut self,
        device: &wgpu::Device,
        color_format: wgpu::TextureFormat,
        atlas_view: &wgpu::TextureView,
        atlas_sampler: &wgpu::Sampler,
        palette: &wgpu::Buffer,
        anim: &wgpu::Buffer,
    ) {
        self.icons.attach_item_models(
            device,
            color_format,
            atlas_view,
            atlas_sampler,
            palette,
            anim,
            "container-item-model",
        );
    }

    /// Draws the container overlay over the current frame, with **no** item
    /// icons: slot contents fall back to the colour swatch. The plain entry
    /// point, kept so existing callers and the headless gates are unchanged.
    /// Always lays out against [`crate::config::AUTO_GUI_SCALE`]; use
    /// [`render_scaled`](Self::render_scaled) for the real windowed path,
    /// which has a persisted `Options.gui_scale` to honour.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &ContainerFrame<'_>,
        width: u32,
        height: u32,
    ) {
        self.render_with_icons(device, queue, view, None, frame, None, width, height);
    }

    /// As [`render`](Self::render), but against an explicit `gui_scale` (`0` =
    /// auto) so the drawn panel matches whatever scale [`hit_test_with_scale`]
    /// is being called with for the same frame.
    pub fn render_scaled(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &ContainerFrame<'_>,
        gui_scale: u32,
        width: u32,
        height: u32,
    ) {
        self.render_with_icons_scaled(
            device, queue, view, None, frame, None, gui_scale, width, height,
        );
    }

    /// Draws the container overlay including **real item icons**.
    ///
    /// `models` supplies baked block-item geometry (`None` falls back to flat
    /// sprites only) and `depth` is a depth attachment matching the target size,
    /// normally [`RenderState::depth_view`](crate::gpu::RenderState::depth_view).
    /// Both are needed for a mini-block to draw; either being `None` degrades to
    /// flat sprites rather than erroring. The flat icons themselves need
    /// [`attach_items`](Self::attach_items) and nothing else.
    ///
    /// # Pass structure
    ///
    /// Three passes, in this order, all loading the existing colour — the same
    /// shape, and for the same reasons, as the HUD's:
    ///
    /// 1. **chrome** (no depth) — panel, slot wells, title;
    /// 2. **item models** (depth, **cleared**) — the isometric mini-blocks;
    /// 3. **flat icons + text** (no depth) — sprite icons, stack counts,
    ///    durability bars.
    ///
    /// The chrome must precede the icons (it is the well they sit in), and the
    /// counts must follow them (they sit on top). The model pass clears depth
    /// because the world's is still resident and would swallow a GUI item at
    /// clip depth ~0.5.
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_icons(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        frame: &ContainerFrame<'_>,
        models: Option<&BlockModels>,
        width: u32,
        height: u32,
    ) {
        self.render_with_icons_scaled(
            device,
            queue,
            view,
            depth,
            frame,
            models,
            crate::config::AUTO_GUI_SCALE,
            width,
            height,
        );
    }

    /// As [`render_with_icons`](Self::render_with_icons), but against an
    /// explicit `gui_scale` (`0` = auto) — see [`render_scaled`](Self::render_scaled).
    #[allow(clippy::too_many_arguments)]
    pub fn render_with_icons_scaled(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        depth: Option<&wgpu::TextureView>,
        frame: &ContainerFrame<'_>,
        models: Option<&BlockModels>,
        gui_scale: u32,
        width: u32,
        height: u32,
    ) {
        // Only ask for model geometry when there is somewhere to draw it.
        let want_models = self.icons.models_attached() && depth.is_some();
        let item_atlas = self.icons.item_atlas();
        let geo = ContainerGeometry::build_inner(
            frame,
            width,
            height,
            gui_scale,
            &IconAssets {
                items: item_atlas.as_deref(),
                models: models.filter(|_| want_models),
            },
            self.font.as_deref(),
            self.background.as_ref().map(|bg| bg.data.as_ref()),
        );
        // `geo.special` counts too — see the same guard in
        // `HudRenderer::render_with_item_models`. A frame whose only content is a
        // chest icon must not be discarded before it reaches `upload`.
        if geo.verts.is_empty()
            && geo.item_verts.is_empty()
            && geo.model_verts.is_empty()
            && geo.bg_verts.is_empty()
            && geo.special.is_empty()
        {
            return;
        }
        if geo.verts.len() > self.capacity_floats {
            self.capacity_floats = geo.verts.len().next_power_of_two();
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("container-verts"),
                size: (self.capacity_floats * 4) as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        if !geo.verts.is_empty() {
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&geo.verts));
        }
        // The background pass's own dynamic buffer, grown the same way as the
        // chrome one above.
        let bg_count = if let Some(bg) = self.background.as_mut() {
            if geo.bg_verts.len() > bg.capacity_floats {
                bg.capacity_floats = geo.bg_verts.len().next_power_of_two();
                bg.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("container-bg-verts"),
                    size: (bg.capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            if !geo.bg_verts.is_empty() {
                queue.write_buffer(&bg.buffer, 0, bytemuck::cast_slice(&geo.bg_verts));
            }
            (geo.bg_verts.len() / 8) as u32
        } else {
            0
        };
        // As in `HudRenderer::render_with_item_models`: `upload` feeds these
        // straight to `gui_ortho`, which must match the logical canvas
        // `ContainerGeometry::build_inner` posed the 3-D block-item vertices
        // into above, not the raw physical framebuffer.
        let (logical_w, logical_h) = crate::menu::render::logical_canvas(gui_scale, width, height);
        let (item_count, model_count) = self.icons.upload(
            device,
            queue,
            &geo.item_verts,
            &geo.model_verts,
            &geo.special,
            geo.slot_special_count,
            logical_w.max(1.0) as u32,
            logical_h.max(1.0) as u32,
            "container-item-verts",
        );

        let vertex_count = geo.vertex_count() as u32;
        let chrome_count = (geo.chrome_vertex_count as u32).min(vertex_count);
        let dim_count = (geo.dim_vertex_count as u32).min(chrome_count);
        // The three carried-stack splits (issue #377). Clamped against what
        // `upload` actually reported so a stream whose half is not attached (no
        // atlas, no depth) still yields an empty range rather than a bogus one.
        let slot_colour_count = (geo.slot_vertex_count as u32).clamp(chrome_count, vertex_count);
        let slot_item_count = (geo.slot_item_vertex_count as u32).min(item_count);
        let slot_model_count = (geo.slot_model_vertex_count as u32).min(model_count);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("container"),
        });
        // Draw order matters here and mirrors vanilla's own
        // `extractBackground`: the dim gradient goes down first (it sits under
        // everything, including the panel art), then the real panel texture (if
        // attached) draws on top of it, and only *then* the rest of this
        // stream's "chrome" — the flat-fill fallback (when there is no texture),
        // the title, the slot wells. Sandwiching the texture between the two
        // `verts` ranges is what keeps the dim from also darkening the panel
        // itself.
        if dim_count > 0 {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-dim-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.buffer.slice(..));
            pass.draw(0..dim_count, 0..1);
        }
        if bg_count > 0
            && let Some(bg) = self.background.as_ref()
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-bg-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&bg.pipeline);
            pass.set_bind_group(0, &bg.bind_group, &[]);
            pass.set_vertex_buffer(0, bg.buffer.slice(..));
            pass.draw(0..bg_count, 0..1);
        }
        if chrome_count > dim_count {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.buffer.slice(..));
            pass.draw(dim_count..chrome_count, 0..1);
        }

        self.icons.draw_models_range(
            &mut encoder,
            view,
            depth,
            0..slot_model_count,
            item_icon::IconStratum::Slots,
            "container-item-model-pass",
        );

        if slot_item_count > 0 || slot_colour_count > chrome_count {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-item-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.icons.draw_sprites_range(&mut pass, 0..slot_item_count);
            // Stack counts, durability bars and the atlas-less swatch fallback,
            // over whichever kind of icon drew beneath them.
            if slot_colour_count > chrome_count {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.buffer.slice(..));
                pass.draw(chrome_count..slot_colour_count, 0..1);
            }
        }

        // Vanilla's `nextStratum()` (issue #377): the carried stack replays all
        // three streams *after* every slot, and its model pass **clears depth
        // again** — that clear is what stops a slot block item's near faces
        // winning over a block on the cursor. See the layering table in
        // `build_inner` for the four cases and which two append order alone
        // could not fix.
        self.icons.draw_models_range(
            &mut encoder,
            view,
            depth,
            slot_model_count..model_count,
            item_icon::IconStratum::Carried,
            "container-carried-model-pass",
        );
        if item_count > slot_item_count || vertex_count > slot_colour_count {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-carried-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.icons
                .draw_sprites_range(&mut pass, slot_item_count..item_count);
            if vertex_count > slot_colour_count {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.buffer.slice(..));
                pass.draw(slot_colour_count..vertex_count, 0..1);
            }
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}

const CONTAINER_WGSL: &str = r"
struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) color: vec4<f32>,
};

@vertex
fn vs_main(@location(0) pos: vec2<f32>, @location(1) color: vec4<f32>) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(pos, 0.0, 1.0);
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return in.color;
}
";

/// A plain textured quad shader for [`ContainerBackground`]'s atlas — the same
/// shape as `menu/render.rs`'s `MENU_SPRITE_WGSL`, restated here rather than
/// shared because that one is `menu`'s own module-private constant.
const CONTAINER_BG_WGSL: &str = r"
struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) color: vec4<f32>,
};

@group(0) @binding(0) var atlas_tex: texture_2d<f32>;
@group(0) @binding(1) var atlas_smp: sampler;

@vertex
fn vs_main(
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) color: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(pos, 0.0, 1.0);
    out.uv = uv;
    out.color = color;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    let texel = textureSample(atlas_tex, atlas_smp, in.uv);
    return texel * in.color;
}
";

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::item::ItemStack;

    const VIEW: (u32, u32) = (1280, 720);

    /// A stand-in for `en_us.json`, holding only the `container.*` keys these
    /// tests need. Deliberately narrow: `lodestone_model`'s built-in stub table
    /// (`text::default_translation`) carries *no* `container.*` key at all, so a
    /// title path that ignored this closure could not accidentally pass.
    fn lang(key: &str) -> Option<String> {
        match key {
            "container.crafting" => Some("Crafting".to_owned()),
            "container.chest" => Some("Chest".to_owned()),
            _ => None,
        }
    }

    #[test]
    fn a_translate_menu_title_renders_words_not_the_raw_key() {
        // Issue #52, exactly as the server sends it: `ClientboundOpenScreen`
        // carries `translate("container.crafting")`, never the word "Crafting".
        let title = lodestone_model::Text::translate("container.crafting", vec![]);
        assert_eq!(menu_title(&title, &lang), "Crafting");

        // -- negative control -------------------------------------------------
        // The call this replaced. If this ever stops producing the raw key, the
        // assertion above has stopped proving anything — either the model grew a
        // `container.*` entry into its stub table, or something upstream is
        // resolving the component before we see it.
        assert_eq!(
            title.to_plain_string(),
            "container.crafting",
            "the translator-free flatten must still leak the key, or the test above is vacuous"
        );

        // …and the resolved title is what the panel actually draws, uppercased
        // by `build_chrome` the way vanilla's container titles are not — the
        // point here is only that it is words. A chest proves the key is read
        // rather than one hard-coded answer.
        let chest = lodestone_model::Text::translate("container.chest", vec![]);
        assert_eq!(menu_title(&chest, &lang), "Chest");
    }

    #[test]
    fn a_menu_title_survives_a_missing_language_table() {
        // The demo palette loads no `en_us.json`, and a server may send a key we
        // have no entry for. Neither may cost the title: `fallback` first, then
        // the key. A literal title (a renamed chest) is untouched either way.
        let with_fallback = lodestone_model::Text {
            content: lodestone_model::TextContent::Translate {
                key: "container.barrel".to_owned(),
                with: vec![],
                fallback: Some("Barrel".to_owned()),
            },
            ..lodestone_model::Text::default()
        };
        assert_eq!(menu_title(&with_fallback, &|_| None), "Barrel");

        let bare = lodestone_model::Text::translate("container.shulker_box", vec![]);
        assert_eq!(menu_title(&bare, &|_| None), "container.shulker_box");

        let named = lodestone_model::Text::literal("Bob's Loot");
        assert_eq!(menu_title(&named, &|_| None), "Bob's Loot");
    }

    fn survival() -> MenuContext {
        MenuContext {
            cursor_loaded: false,
            creative: false,
        }
    }

    fn loaded() -> MenuContext {
        MenuContext {
            cursor_loaded: true,
            creative: false,
        }
    }

    /// A plain player-inventory menu, for the many `press`/`release` tests
    /// below that need *a* [`Menu`] to satisfy the signature but do not care
    /// about its contents or its result slot. Tests that do care (the
    /// `canTakeItemForPickAll` gate, the shift+double-click gather) build
    /// their own.
    fn blank_menu() -> Menu {
        Menu::player()
    }

    /// Centre of a slot's hit rect, in **physical** viewport pixels — the same
    /// space [`hit_test`] takes, and `VIEW` (1280x720) is deliberately *not* the
    /// identity-scale case: `calculate_gui_scale(AUTO, 1280, 720) == 3` (see
    /// `config::tests::auto_scale_at_1280x720`), so this genuinely exercises the
    /// scale-conversion round trip rather than being inert at scale 1.
    /// `panel_origin`/`slot_layout` work in the *logical* canvas, so their
    /// result is scaled back up to physical pixels before returning — the
    /// inverse of what `hit_test` does to its incoming `x`/`y`.
    fn slot_point(menu: &Menu, menu_index: usize) -> (f32, f32) {
        let layout = slot_layout(menu);
        let (px, py) = panel_origin(&layout, VIEW.0, VIEW.1);
        let rect = layout
            .slots
            .iter()
            .find(|r| r.menu_index == menu_index)
            .unwrap_or_else(|| panic!("menu index {menu_index} has no rect"));
        let scale =
            crate::config::calculate_gui_scale(crate::config::AUTO_GUI_SCALE, VIEW.0, VIEW.1)
                .max(1) as f32;
        (
            (px + rect.x + rect.w * 0.5) * scale,
            (py + rect.y + rect.h * 0.5) * scale,
        )
    }

    // ---------------------------------------------------------------------
    // Layout: the transposition class of bug.
    //
    // These are the cheap checks that catch what is genuinely hard to see by
    // eye: a plausible, fully populated inventory whose slots are all shifted
    // by a constant. Every `SlotRect` carries a real menu index, so the gate is
    // that hit-testing a rect's own centre returns that same index — round-trip,
    // for every slot, in both layouts.
    // ---------------------------------------------------------------------

    #[test]
    fn every_slot_rect_hit_tests_back_to_its_own_menu_index() {
        for menu in [Menu::player(), Menu::crafting(3, 3), Menu::generic(27)] {
            let layout = slot_layout(&menu);
            assert_eq!(
                layout.slots.len(),
                menu.slot_count(),
                "every menu slot must be laid out exactly once"
            );
            let mut seen = vec![false; menu.slot_count()];
            for rect in &layout.slots {
                assert!(
                    !std::mem::replace(&mut seen[rect.menu_index], true),
                    "menu index {} laid out twice",
                    rect.menu_index
                );
                let (x, y) = slot_point(&menu, rect.menu_index);
                assert_eq!(
                    hit_test(&menu, VIEW.0, VIEW.1, x, y),
                    MenuHit::Slot(rect.menu_index),
                    "hit test disagreed with the rect it came from"
                );
            }
            assert!(seen.into_iter().all(|s| s), "a menu slot was never drawn");
        }
    }

    /// The `MenuKind` trap, stated as an assertion rather than a comment: the
    /// player screen's hotbar starts at 36 and it owns armour and an off-hand;
    /// a crafting table is a `Generic { container_size: 10 }` whose hotbar
    /// starts at **37** and which has neither. A single shared offset cannot
    /// satisfy both.
    #[test]
    fn crafting_and_player_hotbars_are_not_at_the_same_index() {
        let player = Menu::player();
        assert_eq!(player.kind(), MenuKind::Player);
        assert_eq!(player.slot_count(), 46);

        let table = Menu::crafting(3, 3);
        assert_eq!(table.kind(), MenuKind::Generic { container_size: 10 });
        assert_eq!(table.slot_count(), 46);

        // Same slot count, different meaning at the same index. Menu index 36 is
        // the player screen's first hotbar cell and the crafting screen's *last*
        // main-storage cell; the crafting hotbar begins one later.
        let layout = slot_layout(&table);
        let hotbar_first = layout
            .slots
            .iter()
            .find(|r| r.menu_index == 37)
            .expect("crafting hotbar starts at 37");
        let main_last = layout
            .slots
            .iter()
            .find(|r| r.menu_index == 36)
            .expect("crafting main storage ends at 36");
        assert!(
            hotbar_first.y > main_last.y,
            "the crafting hotbar row must sit below main storage; got hotbar y={} main y={}",
            hotbar_first.y,
            main_last.y
        );
        // And the player screen has slots the crafting screen does not.
        assert!(slot_layout(&player).slots.iter().any(|r| r.menu_index == 45));
        assert_eq!(
            player.craft_layout().map(|c| (c.width, c.height)),
            Some((2, 2))
        );
        assert_eq!(
            table.craft_layout().map(|c| (c.width, c.height)),
            Some((3, 3))
        );
    }

    /// The crafting screen must not lay the result slot on top of a grid cell —
    /// which is exactly what the plain 9-wide generic run would do with a
    /// container size of 10.
    #[test]
    fn the_result_slot_is_not_inside_the_grid() {
        let table = Menu::crafting(3, 3);
        let (rx, ry) = slot_point(&table, 0);
        assert_eq!(hit_test(&table, VIEW.0, VIEW.1, rx, ry), MenuHit::Slot(0));
        for cell in 1..=9 {
            let (cx, cy) = slot_point(&table, cell);
            assert!(
                (cx - rx).abs() > 1.0 || (cy - ry).abs() > 1.0,
                "grid cell {cell} landed on top of the result slot"
            );
        }
    }

    // ---------------------------------------------------------------------
    // The press/drag/release protocol.
    // ---------------------------------------------------------------------

    #[test]
    fn an_empty_cursor_sends_on_press_and_nothing_on_release() {
        let menu = blank_menu();
        let mut input = MenuInput::new();
        let clicks = input.press(
            MenuHit::Slot(37),
            MenuButton::Left,
            false,
            survival(),
            false,
            &menu,
        );
        assert_eq!(clicks, vec![Click::left(37)]);
        // `skipNextRelease`: the release must not send a second packet.
        assert!(
            input
                .release(MenuHit::Slot(37), MenuButton::Left, false, survival(), &menu)
                .is_empty()
        );
    }

    #[test]
    fn shift_press_is_a_quick_move() {
        let menu = blank_menu();
        let mut input = MenuInput::new();
        assert_eq!(
            input.press(
                MenuHit::Slot(0),
                MenuButton::Left,
                true,
                survival(),
                false,
                &menu
            ),
            vec![Click::shift(0)],
            "shift-clicking the result slot must be QUICK_MOVE — the repeat-craft gesture"
        );
    }

    /// The reason this is a state machine: with a loaded cursor the press sends
    /// **nothing**, and the ordinary click is emitted by the release. A
    /// press-to-`PICKUP` mapper passes every other test here and loses the drag.
    #[test]
    fn a_loaded_cursor_sends_the_click_on_release_not_on_press() {
        let menu = blank_menu();
        let mut input = MenuInput::new();
        assert!(
            input
                .press(MenuHit::Slot(1), MenuButton::Right, false, loaded(), false, &menu)
                .is_empty(),
            "vanilla only arms isQuickCrafting on press"
        );
        assert!(input.is_dragging());
        assert_eq!(
            input.release(MenuHit::Slot(1), MenuButton::Right, false, loaded(), &menu),
            vec![Click::right(1)],
            "no slot was painted, so it degrades to a plain place-one"
        );
        assert!(!input.is_dragging());
    }

    #[test]
    fn painting_slots_emits_the_full_quick_craft_sequence() {
        let menu = blank_menu();
        let mut input = MenuInput::new();
        input.press(MenuHit::Slot(1), MenuButton::Right, false, loaded(), false, &menu);
        for cell in [1usize, 2, 4, 5] {
            input.dragged(MenuHit::Slot(cell));
        }
        input.dragged(MenuHit::Slot(5)); // a repeat must not be painted twice
        let clicks = input.release(MenuHit::Slot(5), MenuButton::Right, false, loaded(), &menu);
        assert_eq!(clicks.len(), 6, "start + 4 slots + end, got {clicks:?}");
        assert_eq!(clicks[0].slot, OUTSIDE_SLOT);
        assert_eq!(clicks[5].slot, OUTSIDE_SLOT);
        assert!(
            clicks
                .iter()
                .all(|c| c.input == ContainerInput::QuickCraft)
        );
        assert_eq!(
            clicks[1..5].iter().map(|c| c.slot).collect::<Vec<_>>(),
            vec![1, 2, 4, 5]
        );
        // Right-drag distributes one item per slot.
        for c in &clicks {
            assert_eq!(
                lodestone_game::click::quick_craft_type(c.button),
                drag_type::ONE
            );
        }
    }

    /// The whole point of the sequence: driven into a real menu it distributes
    /// exactly as vanilla does, filling a 2×2 of the crafting grid one plank per
    /// cell. Nothing here fills the result slot — that is the server's.
    #[test]
    fn the_emitted_sequence_fills_a_crafting_grid_one_per_cell() {
        let mut menu = Menu::crafting(3, 3);
        menu.set_carried(Some(ItemStack::new(
            "minecraft:oak_planks".parse().unwrap(),
            8,
        )));
        let mut input = MenuInput::new();
        input.press(MenuHit::Slot(1), MenuButton::Right, false, loaded(), false, &menu);
        for cell in [1usize, 2, 4, 5] {
            input.dragged(MenuHit::Slot(cell));
        }
        for click in input.release(MenuHit::Slot(5), MenuButton::Right, false, loaded(), &menu) {
            click.apply(&mut menu, lodestone_game::click::PlayerCtx::survival());
        }
        for cell in [1usize, 2, 4, 5] {
            assert_eq!(
                menu.slot_item(cell).map(ItemStack::count),
                Some(1),
                "cell {cell} must hold exactly one plank"
            );
        }
        assert_eq!(menu.carried().map(ItemStack::count), Some(4));
        assert_eq!(
            menu.slot_item(0),
            None,
            "the client must never put anything in the result slot"
        );
    }

    #[test]
    fn a_click_inside_the_panel_but_off_a_slot_does_nothing() {
        let menu = blank_menu();
        let mut input = MenuInput::new();
        assert!(
            input
                .press(MenuHit::Panel, MenuButton::Left, false, survival(), false, &menu)
                .is_empty()
        );
        assert!(!input.is_dragging());
        assert!(
            input
                .release(MenuHit::Panel, MenuButton::Left, false, loaded(), &menu)
                .is_empty()
        );
    }

    #[test]
    fn releasing_a_loaded_cursor_outside_drops_it() {
        let menu = blank_menu();
        let mut input = MenuInput::new();
        input.press(MenuHit::Outside, MenuButton::Left, false, loaded(), false, &menu);
        assert_eq!(
            input.release(MenuHit::Outside, MenuButton::Left, false, loaded(), &menu),
            vec![Click::drop_cursor()]
        );
    }

    #[test]
    fn a_second_press_on_the_same_slot_gathers_on_release() {
        let menu = blank_menu();
        let mut input = MenuInput::new();
        input.press(MenuHit::Slot(9), MenuButton::Left, false, loaded(), false, &menu);
        input.release(MenuHit::Slot(9), MenuButton::Left, false, loaded(), &menu);
        input.press(MenuHit::Slot(9), MenuButton::Left, false, loaded(), true, &menu);
        assert_eq!(
            input.release(MenuHit::Slot(9), MenuButton::Left, false, loaded(), &menu),
            vec![Click::double(9)]
        );
    }

    #[test]
    fn pick_block_only_clones_with_infinite_materials() {
        let menu = blank_menu();
        let creative = MenuContext {
            cursor_loaded: false,
            creative: true,
        };
        let mut input = MenuInput::new();
        assert_eq!(
            input.press(MenuHit::Slot(3), MenuButton::Pick, false, creative, false, &menu),
            vec![Click::clone_slot(3)]
        );
        let mut survival_input = MenuInput::new();
        assert!(
            survival_input
                .press(MenuHit::Slot(3), MenuButton::Pick, false, survival(), false, &menu)
                .is_empty(),
            "middle-click in survival is a hotbar rebind, not a container click"
        );
    }

    // ---------------------------------------------------------------------
    // Gap (a): `canTakeItemForPickAll` — AbstractContainerScreen.java:387.
    // ---------------------------------------------------------------------

    /// Vanilla `AbstractContainerScreen.java:387` gates the whole double-click
    /// gather branch on `menu.canTakeItemForPickAll(ItemStack.EMPTY, slot)`,
    /// which every result-bearing menu overrides to exclude its own result
    /// slot. So double-clicking a crafting result must send **nothing** — not
    /// a desync fix (a real server honours the packet fine; `Menu::do_click`
    /// has no such gate), just non-vanilla UX otherwise.
    #[test]
    fn double_clicking_the_crafting_result_slot_sends_nothing() {
        let menu = Menu::crafting(3, 3);
        let craft = menu.craft_layout().expect("a crafting table has a grid");
        let result = MenuHit::Slot(craft.result_slot);
        let mut input = MenuInput::new();
        input.press(result, MenuButton::Left, false, survival(), false, &menu);
        input.release(result, MenuButton::Left, false, survival(), &menu);
        input.press(result, MenuButton::Left, false, survival(), true, &menu);
        assert_eq!(
            input.release(result, MenuButton::Left, false, survival(), &menu),
            Vec::new(),
            "canTakeItemForPickAll excludes the result slot from double-click gather"
        );
    }

    /// Control for the test above, proving the detector actually fires rather
    /// than every double-click silently sending nothing: the identical
    /// press/release sequence on an ordinary slot of the same menu must still
    /// gather.
    #[test]
    fn double_clicking_an_ordinary_slot_still_gathers() {
        let menu = Menu::crafting(3, 3);
        let mut input = MenuInput::new();
        input.press(MenuHit::Slot(10), MenuButton::Left, false, survival(), false, &menu);
        input.release(MenuHit::Slot(10), MenuButton::Left, false, survival(), &menu);
        input.press(MenuHit::Slot(10), MenuButton::Left, false, survival(), true, &menu);
        assert_eq!(
            input.release(MenuHit::Slot(10), MenuButton::Left, false, survival(), &menu),
            vec![Click::double(10)]
        );
    }

    // ---------------------------------------------------------------------
    // Gap (b): shift+double-click "move all matching" —
    // AbstractContainerScreen.java:388-398.
    // ---------------------------------------------------------------------

    /// The gather-by-shift branch sends one `QUICK_MOVE` per slot that shares
    /// the double-clicked slot's **backing container**, holds an item, and
    /// matches `last_quick_moved` — not a single `PICKUP_ALL`. Exercises the
    /// exact set and order of emitted slots, plus two controls: a
    /// wrong-item chest slot (must not appear) and a matching player-inventory
    /// slot in a *different* backing container (must not appear either — the
    /// `target.container == slot.container` restriction, not just an item
    /// match).
    #[test]
    fn shift_double_click_gathers_only_matching_slots_in_the_same_backing_container() {
        let mut menu = Menu::generic(9);
        let diamond = |count: i32| ItemStack::new("minecraft:diamond".parse().unwrap(), count);
        // Chest slots (container 0): three matching diamonds and one
        // non-matching dirt stack, at varied counts to show the match is
        // item-identity, not size.
        menu.set_slot_item(0, Some(diamond(1)));
        menu.set_slot_item(1, Some(ItemStack::new("minecraft:dirt".parse().unwrap(), 64)));
        menu.set_slot_item(2, Some(diamond(3)));
        menu.set_slot_item(4, Some(diamond(5)));
        // Player main storage (container 1): a matching diamond stack that
        // must NOT be swept — it is a different backing container than the
        // chest, even though it lives in the same `Menu`.
        menu.set_slot_item(20, Some(diamond(1)));

        let mut input = MenuInput::new();
        // A first shift-click on chest slot 0 is what populates
        // `last_quick_moved` in real play; reproduce it before the
        // shift+double-click.
        input.press(MenuHit::Slot(0), MenuButton::Left, true, survival(), false, &menu);
        input.release(MenuHit::Slot(0), MenuButton::Left, true, survival(), &menu);
        input.press(MenuHit::Slot(0), MenuButton::Left, true, survival(), true, &menu);
        let clicks = input.release(MenuHit::Slot(0), MenuButton::Left, true, survival(), &menu);

        assert!(
            clicks.iter().all(|c| c.input == ContainerInput::QuickMove),
            "shift+double-click gathers via QUICK_MOVE, not PICKUP_ALL: {clicks:?}"
        );
        assert_eq!(
            clicks.iter().map(|c| c.slot).collect::<Vec<_>>(),
            vec![0, 2, 4],
            "must sweep exactly the matching chest slots, in ascending slot order, and not \
             the wrong-item slot 1 or the different-container slot 20"
        );
    }

    /// Control for the test above: `last_quick_moved` is captured off the
    /// double-clicked slot's *own* contents at press time
    /// (`AbstractContainerScreen.java:312`), so shift+double-clicking an
    /// **empty** slot records vanilla's `ItemStack.EMPTY` — and
    /// `!this.lastQuickMoved.isEmpty()` then suppresses the gather entirely,
    /// sending nothing. This proves the emitted clicks in the test above come
    /// from a real match against a captured stack, not from the double-click
    /// alone.
    #[test]
    fn shift_double_click_on_an_empty_slot_sends_nothing() {
        let menu = Menu::generic(9); // slot 0 starts empty
        let mut input = MenuInput::new();
        input.press(MenuHit::Slot(0), MenuButton::Left, true, survival(), false, &menu);
        input.release(MenuHit::Slot(0), MenuButton::Left, true, survival(), &menu);
        input.press(MenuHit::Slot(0), MenuButton::Left, true, survival(), true, &menu);
        assert_eq!(
            input.release(MenuHit::Slot(0), MenuButton::Left, true, survival(), &menu),
            Vec::new(),
            "an empty slot's captured stack is vanilla's ItemStack.EMPTY, which suppresses \
             the shift+double-click gather"
        );
    }

    // ---------------------------------------------------------------------
    // Container background art (issue #51) and the hotbar dim (issue #61's
    // leftover). GPU-free: `ContainerBackground` is deliberately a pure
    // producer/consumer split (see its own doc comment) so this needs no
    // device. The GPU pixel proof lives in
    // `tests/container_background_pixels.rs`.
    // ---------------------------------------------------------------------

    #[test]
    fn background_kind_mirrors_slot_layouts_own_dispatch() {
        assert_eq!(background_kind(&Menu::player()), BackgroundKind::Inventory);
        assert_eq!(
            background_kind(&Menu::crafting(3, 3)),
            BackgroundKind::Crafting
        );
        // A single chest: one row.
        assert_eq!(
            background_kind(&Menu::generic(9)),
            BackgroundKind::Generic { rows: 1 }
        );
        // A double chest: six rows, `generic_54`'s own native row count.
        assert_eq!(
            background_kind(&Menu::generic(54)),
            BackgroundKind::Generic { rows: 6 }
        );
        // A hopper-sized (5-slot) container still rounds up to a whole row
        // rather than drawing a fractional one.
        assert_eq!(
            background_kind(&Menu::generic(5)),
            BackgroundKind::Generic { rows: 1 }
        );
    }

    /// A minimal in-memory pack with distinctly-sized solid-colour stand-ins
    /// for the three real sheets, so `ContainerBackground::build` succeeds
    /// hermetically — no `client.jar` needed for this test.
    fn synthetic_background() -> ContainerBackground {
        use lodestone_assets::{MemorySource, ResourceManager, ResourceSource};

        fn solid_png(w: u32, h: u32) -> Vec<u8> {
            let mut data = Vec::new();
            let mut encoder = png::Encoder::new(&mut data, w, h);
            encoder.set_color(png::ColorType::Rgba);
            encoder.set_depth(png::BitDepth::Eight);
            let mut writer = encoder.write_header().expect("png header");
            let pixels: Vec<u8> = (0..(w * h)).flat_map(|_| [10, 20, 30, 255]).collect();
            writer.write_image_data(&pixels).expect("png data");
            drop(writer);
            data
        }

        let mut src = MemorySource::default();
        for name in ["generic_54", "crafting_table", "inventory"] {
            src.insert(
                format!("assets/minecraft/textures/gui/container/{name}.png"),
                solid_png(256, 256),
            );
        }
        let manager = ResourceManager::new(vec![Box::new(src) as Box<dyn ResourceSource>]);
        ContainerBackground::build(&manager).expect("synthetic background builds")
    }

    #[test]
    fn a_single_chest_blits_vanillas_two_part_split_at_the_right_offsets() {
        let bg = synthetic_background();
        let menu = Menu::generic(27); // three rows
        let quads = bg
            .quads(&menu, 10.0, 20.0)
            .expect("every id used by `synthetic_background` is present");
        assert_eq!(quads.len(), 2, "the chest background is vanilla's two blits");
        // Top piece: `ContainerScreen.java:25` — height `rows*18+17`, at the
        // panel's own origin.
        assert_eq!(quads[0].dst, [10.0, 20.0, 176.0, 3.0 * 18.0 + 17.0]);
        // Bottom piece: `:26` — 96 tall, placed immediately below the top one,
        // sampling the sheet's fixed `v=126` row regardless of row count.
        assert_eq!(quads[1].dst, [10.0, 20.0 + (3.0 * 18.0 + 17.0), 176.0, 96.0]);
        assert!(
            quads[1].uv_min[1] > quads[0].uv_max[1],
            "the bottom piece samples further down the sheet (v=126) than the \
             top piece's own bottom edge (v={:.3}) — it must not be sampling \
             the same rows twice",
            quads[0].uv_max[1]
        );
    }

    #[test]
    fn a_double_chest_draws_a_taller_top_piece_than_a_single_one() {
        let bg = synthetic_background();
        let single = bg
            .quads(&Menu::generic(27), 0.0, 0.0)
            .expect("present");
        let double = bg
            .quads(&Menu::generic(54), 0.0, 0.0)
            .expect("present");
        assert_eq!(single[0].dst[3], 3.0 * 18.0 + 17.0);
        assert_eq!(double[0].dst[3], 6.0 * 18.0 + 17.0);
        assert!(
            double[0].dst[3] > single[0].dst[3],
            "a double chest's top blit must be taller than a single chest's"
        );
    }

    #[test]
    fn inventory_and_crafting_each_blit_one_whole_panel() {
        let bg = synthetic_background();
        let inventory = bg
            .quads(&Menu::player(), 4.0, 5.0)
            .expect("present");
        assert_eq!(inventory.len(), 1);
        assert_eq!(inventory[0].dst, [4.0, 5.0, 176.0, 166.0]);

        let crafting = bg
            .quads(&Menu::crafting(3, 3), 4.0, 5.0)
            .expect("present");
        assert_eq!(crafting.len(), 1);
        assert_eq!(crafting[0].dst, [4.0, 5.0, 176.0, 166.0]);

        // -- negative control ---------------------------------------------
        // The two must not sample the same sheet: `inventory.png` and
        // `crafting_table.png` are different files, so their UVs must land on
        // different placed regions of the atlas even though both request the
        // identical `(0,0,176,166)` local rect.
        assert_ne!(
            inventory[0].uv_min, crafting[0].uv_min,
            "inventory and crafting table must not sample the same atlas region"
        );
    }

    #[test]
    fn build_inner_without_a_background_falls_back_to_the_flat_fill_and_still_dims() {
        // No background attached: `build`/`build_with_icons` (used by every
        // existing test and gate in this file) must keep drawing something —
        // this is the jar-less path and the pixel gate's negative control.
        let menu = Menu::player();
        let frame = ContainerFrame::new(Some(&menu), "Inventory");
        let geo = ContainerGeometry::build(&frame, VIEW.0, VIEW.1);
        assert!(
            geo.dim_vertex_count > 0,
            "the full-canvas dim must draw even with no background attached — \
             it is independent of the panel art"
        );
        assert!(
            geo.bg_verts.is_empty(),
            "with no `ContainerBackground` attached, nothing should land on the \
             background-texture stream"
        );
        assert!(
            geo.chrome_vertex_count > geo.dim_vertex_count,
            "the flat-fill fallback panel must still draw after the dim when \
             there is no real background"
        );
    }
}
