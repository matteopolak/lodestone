//! Drawing the menu screens: a self-contained coloured-quad pipeline plus the
//! layout that turns menu state into pixels.
//!
//! ## What it is
//!
//! [`MenuRenderer`] owns its own shader, pipeline and vertex buffer, exactly
//! like [`crate::effects::EffectsRenderer`] and [`crate::container::ContainerRenderer`].
//! That is a deliberate structural choice, not a stylistic one: `hud.rs` and
//! `container.rs` are owned by other agents, and folding a fourth surface into
//! the HUD's single geometry pass would mean editing their files. The only thing
//! borrowed from the HUD is its **public** bitmap font, [`crate::hud::glyph_rows`].
//!
//! Unlike the HUD overlays this pass **clears** the frame, because on a menu
//! screen there is no world behind it — the app skips world rendering entirely
//! (see [`super::UiState::is_menu`]).
//!
//! ## How the layout works
//!
//! Everything is laid out in *pixels* from the top-left and converted to NDC at
//! emit time, so the arithmetic reads like a screenshot. [`geometry`] is pure —
//! it takes a [`MenuFrame`] and a viewport size and returns a `Vec<f32>` — which
//! is what lets the layout be tested (row rects, selection tracking, favicon
//! coverage) with no GPU at all.
//!
//! ## Favicons
//!
//! A server's favicon arrives as PNG bytes. Sampling it properly needs a texture
//! and a second bind group; instead [`favicon_mosaic`] decodes the PNG (via
//! [`lodestone_assets::Image`], which the shell already depends on) and box-filters
//! it down to a [`MOSAIC`]×[`MOSAIC`] grid of coloured cells, drawn as quads on
//! the pipeline that is already here. At the icon size the list uses each cell is
//! about two screen pixels, so the icon is recognisably the server's, and the
//! real favicon bytes are demonstrably reaching the screen rather than sitting in
//! a cache nothing reads. Swapping in a sampled texture later is a change to this
//! one function plus a bind group; nothing else moves.
//!
//! ## How to change it
//!
//! Sizes are the `const`s below and are in *logical* GUI pixels — not physical
//! ones. [`MenuRenderer::render`] converts the real framebuffer size (physical
//! pixels) down to a logical canvas via [`logical_canvas`] before handing it to
//! [`geometry`], using [`crate::config::calculate_gui_scale`] and whatever
//! `gui_scale` option [`frame_for`] stamped onto the [`MenuFrame`]. That is what
//! fixes the "menu draws half-size on Retina" report: on a HiDPI display the
//! framebuffer is larger than the logical window, `calculate_gui_scale` picks a
//! correspondingly larger integer scale, and dividing it back out keeps a fixed
//! `ROW_W`/`BUTTON_H` at the same *visual* size regardless of DPI. `geometry`
//! itself stays pixel-space and scale-agnostic — it does not know or care
//! whether the canvas it was given is physical or logical, which is why every
//! test below that calls it directly with a fixed size is unaffected by this.
//! Text is upper-case-only because that is what the HUD's bitmap font has
//! glyphs for; `glyph_rows` up-cases internally, so passing mixed case is
//! harmless but pointless.

use std::sync::Arc;

use lodestone_assets::Image;
use lodestone_model::command_tree::CommandTree;
use lodestone_model::text::{TextSpan, TextStyle};
use lodestone_render::{GpuAtlas, GuiAtlas, GuiSpriteQuad};

use crate::chat::Completion;
use crate::hud::VanillaFont;
use crate::hud::glyph_rows;
use crate::hud::item_icon::{ColourStream, push_sprite_quad};
use crate::menu::command_block;
use crate::menu::edit_box::{self, EditBox};
use crate::menu::layout;
use crate::menu::nav::{MainButton, PauseButton};
use crate::menu::panorama::{self, PanoramaFaces, PanoramaRenderer};
use crate::menu::widget::{self, LayoutElement, Widget};

// The parent module's own items, re-imported so that every `super::…` path in
// the submodules below still names `crate::menu::…`. A submodule's `super` is
// *this* module, not `menu`, so without these the paths would silently mean
// something else -- `command_block`, `edit_box` and `layout` are already in the
// block above and need no restating.
use super::{
    Screen, UiState, accounts, confirm, create_world, key_binds, language, nav, options, packs,
    social, stats, status, telemetry, world_select,
};

mod account_screen;
mod dispatch;
mod draw;
mod favicon;
mod frame;
mod measure;
mod origin;
mod renderer;
mod screens;
mod server_list;
mod title_pause;
mod world_list;

pub use account_screen::{
    accounts_band_top, accounts_list_spec, accounts_row_content_rect, accounts_row_left,
    accounts_row_rect, accounts_row_top, accounts_row_visible,
};
pub use dispatch::{frame_for, stamp_canvas_facts};
pub use draw::{MenuGeometry, SpriteCut, build, geometry};
pub use favicon::{FaviconMosaic, default_head_icon, favicon_mosaic, head_mosaic};
pub use frame::{
    AccountEntryView, Align, Arrow, FaviconCache, MenuBackdrop, MenuFrame, MenuLabel, MenuNotice,
    MenuProgress,
    MenuRow, PROGRESS_BAR_BG, PROGRESS_BAR_FG, PROGRESS_BAR_H, PROGRESS_BAR_W, PackEntryView,
    ServerEntryView, TabEntryView, WorldEntryView, notice_rect, owns_frame,
};
pub use measure::{
    EDIT_BOX_H, field_rect, field_row_rects, logical_canvas, menu_row_under, row_rect, text_px,
};
pub use origin::{Origin, Slot};
pub use renderer::MenuRenderer;
pub use screens::{
    command_block_frame, death_frame, loading_frame, loading_frame_with_progress, pause_frame,
};
pub use server_list::{
    SERVER_LIST_ITEM_H, server_entry_icon_rect, server_list_footer_slot, server_list_max_scroll,
    server_list_title_label, server_list_window_rows, server_row_content_rect, server_row_left,
    server_list_spec, server_row_rect, server_row_top, server_row_visible,
    server_scroll_model,
    server_status_icon_rect,
};
pub use title_pause::{death_slot, pause_grid_size, pause_slot, title_slot};
pub use world_list::{
    WORLD_LIST_ITEM_H, WORLD_LIST_LINE_DY, WORLD_LIST_TEXT_DX, world_list_icon_rect,
    world_list_row_content_rect,
    world_list_row_label, world_list_row_left, world_list_row_rect, world_list_row_top,
    world_list_row_visible, world_list_scroll_for, world_list_spec, world_list_text_width,
    world_list_visible_rows,
    world_list_window_rows, world_scroll_model, world_select_search_slot, world_select_slot,
    world_select_title_label,
};

/// Bitmap-font cell metrics, matching [`crate::hud`]'s font (`glyph_rows`
/// returns seven 5-bit rows).
const GLYPH_W: usize = 5;
/// Height of one font cell, in font pixels.
const GLYPH_H: usize = 7;

/// Render scale for ordinary menu text (each font pixel becomes N×N screen px).
const TEXT_SCALE: f32 = 2.0;
/// Render scale for an `EditBox`'s own text — **not** [`TEXT_SCALE`].
///
/// A player report caught `draw_edit_box` as the one vanilla-positioned
/// widget still drawing at [`TEXT_SCALE`] while its row siblings (the
/// Done/Cancel buttons on the same `ManageServerScreen`, via [`draw_widget`])
/// draw at `1.0`. Measured against the jar: vanilla's `Font.lineHeight` is
/// `9` (`Font.java:33`) inside `EditBox`'s 20 px box (`EditBox.java:61-63`),
/// a `0.45` ratio; `GLYPH_H(7) * TEXT_SCALE(2.0) = 14` in the same 20 px box
/// is `0.70` — exactly double. `GLYPH_H(7) * EDIT_TEXT_SCALE(1.0) = 7` is the
/// same ratio `draw_widget`'s buttons already use.
///
/// Paired with `edit_box.rs`'s `MENU_TEXT_ADVANCE` (`12.0 → 6.0`, i.e.
/// `(GLYPH_W + 1) * EDIT_TEXT_SCALE`) — the two must land in the same commit
/// or the caret advance disagrees with the glyphs it is stepping over.
const EDIT_TEXT_SCALE: f32 = 1.0;
/// Render scale for the small second line of a row (MOTD, address).
const SMALL_SCALE: f32 = 1.0;
/// Render scale for the screen title.
const TITLE_SCALE: f32 = 4.0;

/// Width of a menu button / list row, in pixels.
const ROW_W: f32 = 420.0;
/// Height of a main-menu button, in pixels.
const BUTTON_H: f32 = 30.0;
/// Height of a server-list row, in pixels — tall enough for the favicon.
const LIST_ROW_H: f32 = 40.0;
/// Vertical gap between rows, in pixels.
const ROW_GAP: f32 = 8.0;
/// Side length of the favicon square, in pixels.
const ICON: f32 = 32.0;
/// Padding inside a row, in pixels.
const PAD: f32 = 6.0;

/// Resolution of the favicon mosaic, in cells per side. 16 cells over a 32 px
/// icon is two screen pixels each.
pub const MOSAIC: usize = 16;

// -- vanilla screen metrics --------------------------------------------------
//
// Every number below is transcribed from `.cache/mc/26.2/client-src`, with the
// file and line named. They are *logical* GUI pixels: `logical_canvas` has
// already divided the framebuffer by the effective GUI scale, so these are the
// same units vanilla's `Screen.width`/`height` are in.

/// A vanilla button's height — `Button.DEFAULT_HEIGHT` (`Button.java:15`).
///
/// Read from [`widget::DEFAULT_HEIGHT`] rather than restated: the widget layer
/// and every slot below must not be able to drift apart.
const WIDGET_H: f32 = widget::DEFAULT_HEIGHT;
/// A vanilla wide button — `Button.BIG_WIDTH` (`Button.java:14`), used for the
/// title screen's top three rows (`TitleScreen.java:178,196,199`). See
/// [`WIDGET_H`] on why this is an alias rather than a literal.
const WIDE_W: f32 = widget::BIG_WIDTH;
/// The title screen's half-width button (`TitleScreen.java:146,148`). Note the
/// pair is `[W/2-100, 98]` and `[W/2+2, 98]` — a **4 px** gutter, unlike the
/// pause screen's 8 px one below.
const TITLE_HALF_W: f32 = 98.0;
/// Vertical pitch between the title screen's rows — `init`'s `spacing`
/// (`TitleScreen.java:112`, passed at `:117`).
const TITLE_PITCH: f32 = 24.0;
/// Side of an icon-only button on either screen (`TitleScreen.java:130`,
/// `PauseScreen.java:105`).
const ICON_BTN: f32 = 20.0;
/// The sprite drawn inside an icon button — 15×15 in every vanilla call site
/// (`CommonButtons.java:10,21`, `PauseScreen.java:104,115,134`).
const ICON_SPRITE: f32 = 15.0;

/// Logo destination width — `LogoRenderer.LOGO_WIDTH` (`LogoRenderer.java:13`).
const LOGO_W: f32 = 256.0;
/// Logo destination height. Vanilla blits 44 rows out of a 256×**64** declared
/// texture (`LogoRenderer.java:39`); the 20 rows below the cut are fully
/// transparent (measured: max alpha 0), so drawing the whole sprite into a
/// 256×64 rect is pixel-identical and needs no sub-rect blit. See
/// [`crate::resources::TITLE_TEXTURES`].
const LOGO_H: f32 = 64.0;
/// `LogoRenderer.DEFAULT_HEIGHT_OFFSET` (`LogoRenderer.java:21`).
const LOGO_Y: f32 = 30.0;
/// Edition strip size — 128×14 of a declared 128×**16**
/// (`LogoRenderer.java:17-20,43`); same all-transparent tail as the logo.
const EDITION_W: f32 = 128.0;
/// See [`EDITION_W`].
const EDITION_H: f32 = 16.0;
/// `heightOffset + LOGO_HEIGHT - EDITION_LOGO_OVERLAP` = `30 + 44 - 7`
/// (`LogoRenderer.java:22,42`).
const EDITION_Y: f32 = LOGO_Y + 44.0 - 7.0;

/// Width of vanilla's arranged pause-screen `GridLayout`: the widest cell is the
/// 204 px [`PAUSE_BUTTON_FULL_W`] plus the default cell's 4 px left and right
/// padding, split across two columns of 106 — so the grid is 212 wide and a
/// *half*-width 98 px button sits 4 px into its 106 px column. That is where the
/// pause screen's 8 px gutter comes from, and why its full-width buttons start at
/// `W/2 - 102` rather than the title screen's `W/2 - 100`.
///
/// **This is the hand derivation, not the value the draw uses.** Since #394 the
/// grid is really built and arranged (`pause_menu_grid_with`) and
/// [`pause_grid_size`] is what [`Origin::PauseGrid`] reads; this constant is the
/// independent expectation `the_pause_grid_size_is_the_arranged_layouts_own`
/// checks it against. Per `CLAUDE.md`, an expected value has to originate outside
/// the code under test — so do not "simplify" this into a call to the layout.
pub const PAUSE_GRID_W: f32 = 212.0;
/// Height of the same grid: row 0 is `20 + paddingTop(50)` = 70
/// (`PauseScreen.java:98`) and rows 1..4 are `20 + 4` = 24 each, for
/// `70 + 4 * 24`. See [`PAUSE_GRID_W`] on why this stays a hand-derived
/// constant.
pub const PAUSE_GRID_H: f32 = 166.0;

// -- vanilla's `PauseScreen` cell metrics (`PauseScreen.java:50-54`) ----------

/// `PauseScreen.COLUMNS` (`PauseScreen.java:50`).
const PAUSE_COLUMNS: usize = 2;
/// `PauseScreen.MENU_PADDING_TOP` (`:51`) — the first cell's `paddingTop`, which
/// is what pushes the whole menu below the "Game Menu" heading.
const PAUSE_MENU_PADDING_TOP: i32 = 50;
/// `PauseScreen.BUTTON_PADDING` (`:52`) — the default cell padding, applied as
/// `padding(4, 4, 4, 0)`: left, top, right, **no bottom** (`:93`).
const PAUSE_BUTTON_PADDING: i32 = 4;
/// `PauseScreen.BUTTON_WIDTH_FULL` (`:53`). Note it is 204, not
/// [`widget::BIG_WIDTH`]'s 200 — the pause screen is 4 px wider than the title
/// screen's stack, which is exactly the 8 px gutter's other half.
const PAUSE_BUTTON_FULL_W: f32 = 204.0;
/// `PauseScreen.BUTTON_WIDTH_HALF` (`:54`), also `openScreenButton`'s explicit
/// `.width(98)` (`:266-268`).
const PAUSE_BUTTON_HALF_W: f32 = 98.0;
/// The gap between the pause screen's four icon buttons —
/// `LinearLayout.horizontal().spacing(4)` (`:101`).
const PAUSE_ICON_SPACING: i32 = 4;
/// Vanilla's font line height, used to centre a label in its widget
/// (`ActiveTextCollector`).
///
/// `pub(super)` so a screen module outside `render` can centre its own text on a
/// row without restating the 9 — `packs::placement_anchor`'s empty-state line is
/// the first such caller.
pub(super) const LINE_H: f32 = 9.0;
/// Vertical offset of the pause screen's title `StringWidget`
/// (`PauseScreen.java:88`).
const PAUSE_TITLE_Y: f32 = 40.0;
/// Baseline of the title screen's two corner strings — vanilla draws both at
/// `height - 10` (`TitleScreen.java:154,323`).
const CORNER_TEXT_Y: f32 = -10.0;

/// An active button's label colour: plain white, `ARGB.white(alpha)`
/// (`AbstractButton.java:51` tints the sprite; the label itself is the
/// component's own default).
///
/// Also the tint every *sprite* on this pass is drawn with, which is why it
/// stays here rather than moving into [`widget`] alongside
/// [`widget::ACTIVE_LABEL`] — the two happen to be the same white, but one is a
/// widget's label colour and the other is this pipeline's untinted default.
const LABEL: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// Tint applied to a disabled button's *icon sprite*. Vanilla passes
/// `this.alpha` (1.0) and relies on the disabled background alone
/// (`SpriteIconButton.java:81`), so this is white.
const ICON_TINT: [f32; 4] = [1.0, 1.0, 1.0, 1.0];

/// Background colour of a menu screen (the vanilla dirt backdrop's dark tone).
const BG: [f32; 4] = [0.10, 0.10, 0.12, 1.0];
/// Full-screen backdrop for a [`MenuBackdrop::Dim`] frame — translucent, so
/// the world it is drawn over (still ticking, still rendering) stays visible
/// through it, unlike [`BG`]'s opaque fill for a screen that owns the whole
/// frame. Alpha is well short of 1.0 for exactly this reason; a test asserts
/// that rather than trusting the constant.
///
/// This is now **vanilla's exact value**, not an eyeballed one. `Screen`'s
/// in-world menu backdrop is `textures/gui/inworld_menu_background.png`
/// tiled at 32 px (`Screen.java:405,418-419`), and that file was decoded
/// straight out of `client.jar`: a 16×16 greyscale+alpha PNG in which **every
/// pixel is grey 0, alpha 64** — i.e. flat black at 64/255. (`menu_background.png`,
/// the out-of-world variant, is byte-for-byte the same.) So there is no dirt
/// texture to reproduce and nothing lost by drawing one quad instead of tiling:
/// a flat 25 %-black fill *is* the vanilla backdrop.
///
/// What is missing is the **blur** vanilla applies behind it when the pause
/// screen is topmost (`Screen.extractBlurredBackground`, gated on the
/// `menuBackgroundBlurriness` option, which vanilla lets the player set to 0).
/// At blurriness 0 this is exactly vanilla; above it, vanilla's menu reads
/// calmer over a busy world than ours does.
const OVERLAY_BG: [f32; 4] = [0.0, 0.0, 0.0, 64.0 / 255.0];

/// The tint a scrolling list's band carries on top of the screen backdrop —
/// `textures/gui/menu_list_background.png`, decoded out of the 26.2 `client.jar`:
/// a 16×16 greyscale+alpha PNG in which **every pixel is grey 0, alpha 112**. So
/// a flat quad at 112/255 black *is* the vanilla band background, exactly as
/// [`OVERLAY_BG`] is the whole-screen one, and there is no tiling to reproduce.
///
/// `AbstractSelectionList.extractListBackground` blits it across `getX()`/`getY()`
/// to `getRight()`/`getBottom()` before the rows, which is why this is drawn
/// under them and why its rect comes from
/// [`widget::ListSpec::chrome_rect`] rather than from anything restated.
///
/// **This is the "black filter over the panorama" the owner reported missing.**
/// Out of a world the stack is panorama → whole-screen `menu_background` (this
/// pass applies it as `panorama::dim_for_screen`) → this band tint; in a world it
/// is [`OVERLAY_BG`] → this band tint. `inworld_menu_list_background.png` is a
/// **separate file with byte-identical pixels** (also grey 0 / alpha 112), so the
/// in-world fork vanilla writes is a no-op in 26.2 and one constant is faithful to
/// both arms. Measured, not assumed — see `docs/menu-list-chrome.md`.
const LIST_BAND_TINT: [f32; 4] = [0.0, 0.0, 0.0, 112.0 / 255.0];
/// Height of one separator — `header_separator.png` and `footer_separator.png` are
/// both 32×**2**, and `extractListSeparators` blits them at that height.
const SEPARATOR_H: f32 = 2.0;
/// The **light** row of a separator: white at alpha 51.
///
/// Both separator textures are 32×2 greyscale+alpha with one flat colour per row.
/// `header_separator.png` is light-over-dark and `footer_separator.png` is
/// dark-over-light — mirror images, which is what makes the pair read as a bevel
/// facing the content in both directions. All four values decoded from the jar;
/// the two `inworld_*` variants are byte-identical to these, so there is nothing
/// to fork on.
const SEPARATOR_LIGHT: [f32; 4] = [1.0, 1.0, 1.0, 51.0 / 255.0];
/// The **dark** row of a separator: black at alpha 191.
const SEPARATOR_DARK: [f32; 4] = [0.0, 0.0, 0.0, 191.0 / 255.0];
/// Fill of an unselected row.
const ROW_BG: [f32; 4] = [0.22, 0.22, 0.26, 1.0];
/// Fill of the highlighted row.
const ROW_SEL: [f32; 4] = [0.36, 0.40, 0.48, 1.0];
/// Fill of a row that cannot be activated.
const ROW_OFF: [f32; 4] = [0.16, 0.16, 0.18, 1.0];
/// Primary text.
const FG: [f32; 4] = [0.94, 0.94, 0.94, 1.0];
/// `AbstractSliderButton.HANDLE_WIDTH` (`AbstractSliderButton.java:26`): the
/// handle is always 8 px wide, whatever the track's own width is.
///
/// `pub` because the mouse-drag hit-test needs it too — vanilla's
/// `setValueFromMouse` is `(mouse_x - (x + HANDLE_WIDTH/2)) / (width -
/// HANDLE_WIDTH)`, so the draw and the hit-test must use the *same* handle
/// width or the handle lags the cursor near the ends. See
/// `crate::app::menus`'s `menu_slider_fraction`.
pub const SLIDER_HANDLE_WIDTH: f32 = 8.0;
/// Secondary text (MOTD, address, hints).
const FG_DIM: [f32; 4] = [0.66, 0.68, 0.72, 1.0];
/// Failure text.
const FG_BAD: [f32; 4] = [0.92, 0.45, 0.42, 1.0];
/// Text-entry field background.
const FIELD_BG: [f32; 4] = [0.08, 0.08, 0.09, 1.0];

const MENU_WGSL: &str = include_str!("../shaders/menu.wgsl");

const MENU_SPRITE_WGSL: &str = include_str!("../shaders/menu_sprite.wgsl");

#[cfg(test)]
mod tests;
