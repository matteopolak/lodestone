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
use lodestone_render::{BlockModels, ModelVertex};

use lodestone_assets::{ItemAtlas, ResourceLocation};

use std::sync::Arc;

use crate::hud::HotbarSlot;
use crate::hud::VanillaFont;
use crate::hud::item_icon::{self, ColourStream, IconAssets, IconRenderer, IconSink};

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
    /// Title to draw at the top-left of the panel.
    pub title: &'a str,
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
            cursor: None,
            recipe_book: None,
        }
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
    /// How many leading vertices of [`verts`](Self::verts) are *chrome* — the
    /// panel, the title and the slot wells. The remainder (stack counts,
    /// durability bars, the atlas-less swatch fallback) belongs **on top of**
    /// the icons, so the renderer draws this stream in two ranges with the icon
    /// passes in between.
    pub chrome_vertex_count: usize,
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
        )
    }

    fn build_inner(
        frame: &ContainerFrame<'_>,
        width: u32,
        height: u32,
        gui_scale: u32,
        assets: &IconAssets<'_>,
        font: Option<&VanillaFont>,
    ) -> Self {
        let Some(menu) = frame.menu else {
            return Self {
                verts: Vec::new(),
                item_verts: Vec::new(),
                model_verts: Vec::new(),
                chrome_vertex_count: 0,
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
        b.text(
            &frame.title.to_ascii_uppercase(),
            x + 8.0,
            y + 7.0,
            1.0,
            [0.88, 0.84, 0.73, 1.0],
        );

        // Every well first, so the colour stream splits cleanly into "chrome"
        // and "what goes on top of an icon". The icons are drawn between the two
        // halves (they are a separate pass, and the 3-D ones need a depth
        // buffer), so a stack count emitted in the same loop as its well would
        // end up *underneath* the sprite it is counting.
        for slot in &layout.slots {
            let sx = x + slot.x;
            let sy = y + slot.y;
            b.rect_px(sx - 1.0, sy - 1.0, SLOT, SLOT, [0.04, 0.035, 0.032, 0.92]);
            b.rect_px(sx, sy, CELL, CELL, [0.32, 0.30, 0.27, 0.86]);
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

        // The carried stack — what the player has picked up and is dragging —
        // draws last: above every slot (it is appended after them on all three
        // streams, and the icon streams draw in this same append order), below
        // the tooltip (which this client does not draw yet). Vanilla centres it
        // on the cursor; `cursor` is `None` unless the caller opted in via
        // `ContainerFrame::with_cursor`, so every existing caller (the headless
        // gates, `tests/container_screen.rs`, a menu with nothing carried) draws
        // exactly as before.
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
            chrome_vertex_count: chrome_floats / FLOATS_PER_VERTEX,
            verts: b.verts,
            item_verts: b.item_verts,
            model_verts: b.model_verts,
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

/// The overlay's three vertex streams, filled in one pass over the layout. The
/// colour stream is this module's own; the two icon streams are the shared
/// hotbar ones (see [`crate::hud::item_icon`]).
#[derive(Debug)]
struct Builder<'a> {
    w: f32,
    h: f32,
    verts: Vec<f32>,
    item_verts: Vec<f32>,
    model_verts: Vec<ModelVertex>,
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
            font,
        }
    }

    fn rect_px(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        self.colour().rect(x, y, w, h, c);
    }

    fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        self.colour().text(s, x, y, scale, c);
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
        }
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
        );
        if geo.verts.is_empty() && geo.item_verts.is_empty() && geo.model_verts.is_empty() {
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
            logical_w.max(1.0) as u32,
            logical_h.max(1.0) as u32,
            "container-item-verts",
        );

        let vertex_count = geo.vertex_count() as u32;
        let chrome_count = (geo.chrome_vertex_count as u32).min(vertex_count);
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("container"),
        });
        if chrome_count > 0 {
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
            pass.draw(0..chrome_count, 0..1);
        }

        self.icons.draw_models(
            &mut encoder,
            view,
            depth,
            model_count,
            "container-item-model-pass",
        );

        if item_count > 0 || vertex_count > chrome_count {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("container-item-pass"),
                color_attachments: &[Some(item_icon::load_colour_attachment(view))],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            self.icons.draw_sprites(&mut pass, item_count);
            // Stack counts, durability bars and the atlas-less swatch fallback,
            // over whichever kind of icon drew beneath them.
            if vertex_count > chrome_count {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.buffer.slice(..));
                pass.draw(chrome_count..vertex_count, 0..1);
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

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::item::ItemStack;

    const VIEW: (u32, u32) = (1280, 720);

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
}
