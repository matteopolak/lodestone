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
use lodestone_render::{GpuAtlas, GuiAtlas, GuiSpriteQuad};

use crate::hud::VanillaFont;
use crate::hud::glyph_rows;
use crate::hud::item_icon::{ColourStream, push_sprite_quad};
use crate::menu::edit_box::{self, EditBox};
use crate::menu::layout;
use crate::menu::nav::{MainButton, PauseButton};
use crate::menu::widget::{self, LayoutElement, Widget};

/// Bitmap-font cell metrics, matching [`crate::hud`]'s font (`glyph_rows`
/// returns seven 5-bit rows).
const GLYPH_W: usize = 5;
/// Height of one font cell, in font pixels.
const GLYPH_H: usize = 7;

/// Render scale for ordinary menu text (each font pixel becomes N×N screen px).
const TEXT_SCALE: f32 = 2.0;
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
/// (`ActiveTextCollector.java:73`).
const LINE_H: f32 = 9.0;
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
/// Full-screen backdrop for an [`MenuFrame::overlay`] frame — translucent, so
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
/// screen is topmost (`Screen.java:389-394`, gated on the
/// `menuBackgroundBlurriness` option, which vanilla lets the player set to 0).
/// At blurriness 0 this is exactly vanilla; above it, vanilla's menu reads
/// calmer over a busy world than ours does.
const OVERLAY_BG: [f32; 4] = [0.0, 0.0, 0.0, 64.0 / 255.0];
/// Fill of an unselected row.
const ROW_BG: [f32; 4] = [0.22, 0.22, 0.26, 1.0];
/// Fill of the highlighted row.
const ROW_SEL: [f32; 4] = [0.36, 0.40, 0.48, 1.0];
/// Fill of a row that cannot be activated.
const ROW_OFF: [f32; 4] = [0.16, 0.16, 0.18, 1.0];
/// Primary text.
const FG: [f32; 4] = [0.94, 0.94, 0.94, 1.0];
/// Secondary text (MOTD, address, hints).
const FG_DIM: [f32; 4] = [0.66, 0.68, 0.72, 1.0];
/// Failure text.
const FG_BAD: [f32; 4] = [0.92, 0.45, 0.42, 1.0];
/// Text-entry field background.
const FIELD_BG: [f32; 4] = [0.08, 0.08, 0.09, 1.0];

/// A favicon reduced to a small grid of colours, ready to draw as quads.
#[derive(Debug, Clone, PartialEq)]
pub struct FaviconMosaic {
    /// Cells per side.
    pub size: usize,
    /// `size * size` RGBA cells, row-major, top-left first.
    pub cells: Vec<[f32; 4]>,
}

/// Decodes `png` and box-filters it to a [`MOSAIC`]×[`MOSAIC`] colour grid.
///
/// Returns `None` if the bytes are not a decodable PNG — a server with a broken
/// icon still gets its MOTD, which is the whole reason `lodestone-net` decodes
/// the favicon as `Option` rather than failing the status.
#[must_use]
pub fn favicon_mosaic(png: &[u8]) -> Option<FaviconMosaic> {
    let img = Image::decode_png(png).ok()?;
    rgba_mosaic(&img.rgba, img.width as usize, img.height as usize)
}

/// Box-filters a small player-head icon down to a [`MOSAIC`]×[`MOSAIC`]
/// colour grid, from **already-decoded** RGBA bytes rather than a PNG file —
/// see [`default_head_icon`] for why that is the parameter this screen needs.
///
/// Shares [`rgba_mosaic`]'s box filter with [`favicon_mosaic`] rather than
/// re-deriving it: a head and a favicon are the same kind of drawable (a
/// small square texture reduced to coloured cells), so there is one filter,
/// not two that could silently drift apart.
#[must_use]
pub fn head_mosaic(rgba: &[u8], width: usize, height: usize) -> Option<FaviconMosaic> {
    rgba_mosaic(rgba, width, height)
}

/// A placeholder head icon used until skins are implemented (issue #62): a
/// flat skin-tone square with a darker hairline band across the top eighth
/// and two single-pixel eyes, at [`HEAD_SIZE`]×[`HEAD_SIZE`].
///
/// **The texture is the parameter, not the constant.** [`head_mosaic`] does
/// not know or care that [`DEFAULT_HEAD_RGBA`] is hand-authored pixels rather
/// than a downloaded skin — it is exactly the same call a real skin's decoded
/// face region would go through. Swapping this default out for
/// `head_mosaic(&decoded_skin_face, 8, 8)` once issue #62 lands a skin
/// fetch is the entire change; nothing in [`MenuRow`], [`draw_widget`]'s
/// icon-drawing branch, or the geometry builder needs to move.
#[must_use]
pub fn default_head_icon() -> FaviconMosaic {
    head_mosaic(&DEFAULT_HEAD_RGBA, HEAD_SIZE, HEAD_SIZE).expect("the embedded default head is a valid 8x8 RGBA grid")
}

/// Side length, in pixels, of [`DEFAULT_HEAD_RGBA`].
const HEAD_SIZE: usize = 8;

/// An 8×8 RGBA placeholder head: skin tone (`0xC8, 0x96, 0x64`) with a
/// darker top row (hair) and two single-pixel dark eyes on row 4. Hand-authored
/// pixels, not art — see [`default_head_icon`]'s docs on why that is fine.
const DEFAULT_HEAD_RGBA: [u8; HEAD_SIZE * HEAD_SIZE * 4] = build_default_head();

const fn build_default_head() -> [u8; HEAD_SIZE * HEAD_SIZE * 4] {
    const SKIN: [u8; 4] = [0xC8, 0x96, 0x64, 0xFF];
    const HAIR: [u8; 4] = [0x4A, 0x30, 0x1E, 0xFF];
    const EYE: [u8; 4] = [0x20, 0x20, 0x20, 0xFF];
    let mut out = [0u8; HEAD_SIZE * HEAD_SIZE * 4];
    let mut y = 0;
    while y < HEAD_SIZE {
        let mut x = 0;
        while x < HEAD_SIZE {
            let px = if y == 0 {
                HAIR
            } else if y == 3 && (x == 2 || x == 5) {
                EYE
            } else {
                SKIN
            };
            let i = (y * HEAD_SIZE + x) * 4;
            out[i] = px[0];
            out[i + 1] = px[1];
            out[i + 2] = px[2];
            out[i + 3] = px[3];
            x += 1;
        }
        y += 1;
    }
    out
}

/// The box filter shared by [`favicon_mosaic`] and [`head_mosaic`]: reduces
/// `width`×`height` RGBA pixels to [`MOSAIC`]×[`MOSAIC`] cells, averaging each
/// cell's source rect. Returns `None` for a zero-sized image.
#[must_use]
fn rgba_mosaic(rgba: &[u8], width: usize, height: usize) -> Option<FaviconMosaic> {
    if width == 0 || height == 0 {
        return None;
    }
    let (iw, ih) = (width, height);
    let mut cells = Vec::with_capacity(MOSAIC * MOSAIC);
    for cy in 0..MOSAIC {
        for cx in 0..MOSAIC {
            // Source rect for this cell. Each bound is forced to span at least
            // one pixel: for an icon *smaller* than the mosaic, plain integer
            // division gives `x0 == x1` for most cells, which would average
            // nothing and leave transparent (invisible) holes.
            let x0 = (cx * iw / MOSAIC).min(iw - 1);
            let x1 = ((cx + 1) * iw).div_ceil(MOSAIC).clamp(x0 + 1, iw);
            let y0 = (cy * ih / MOSAIC).min(ih - 1);
            let y1 = ((cy + 1) * ih).div_ceil(MOSAIC).clamp(y0 + 1, ih);
            let mut acc = [0f32; 4];
            let mut n = 0f32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * iw + x) * 4;
                    if i + 3 >= rgba.len() {
                        continue;
                    }
                    for (c, a) in acc.iter_mut().enumerate() {
                        *a += f32::from(rgba[i + c]);
                    }
                    n += 1.0;
                }
            }
            if n == 0.0 {
                cells.push([0.0, 0.0, 0.0, 0.0]);
            } else {
                cells.push([
                    acc[0] / n / 255.0,
                    acc[1] / n / 255.0,
                    acc[2] / n / 255.0,
                    acc[3] / n / 255.0,
                ]);
            }
        }
    }
    Some(FaviconMosaic {
        size: MOSAIC,
        cells,
    })
}

/// The anchor a [`Slot`] is measured from.
///
/// Vanilla never places a widget at a plain fraction of the canvas, so these are
/// the actual expressions from the two screens' `init` methods rather than
/// normalised alignments. Keeping them as named origins is what lets one `Slot`
/// be resolved against any canvas size — which the layout has to be, because the
/// logical canvas is only known at draw time (see [`logical_canvas`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    /// `(w / 2, 0)` — the top of the screen, for the logo band and the pause
    /// screen's title.
    ScreenTop,
    /// `(w / 2, floor(h / 4) + 48)` — vanilla `TitleScreen.init`'s `topPos`
    /// (`TitleScreen.java:113`), the y every title-screen row is offset from.
    /// `this.height / 4` is Java integer division, hence the `floor`.
    TitleTop,
    /// The top-left of vanilla `PauseScreen`'s **arranged** `GridLayout`:
    /// `(floor((w - 212) / 2), floor((h - 166) / 4))`.
    ///
    /// That comes from `FrameLayout.alignInRectangle(grid, 0, 0, w, h, 0.5, 0.25)`
    /// (`PauseScreen.java:181`), and since #394 it is *evaluated* rather than
    /// restated: [`layout::align_in_dimension`] applied to
    /// [`pause_grid_size`], which is the arranged
    /// [`GridLayout`](layout::GridLayout)'s own output. The `floor`s in the
    /// formula above are vanilla's truncating `(int)` cast
    /// (`FrameLayout.java:113-116`); the two differ only for a canvas narrower
    /// than the grid, which `calculate_gui_scale`'s 320 px floor rules out.
    PauseGrid,
    /// `(0, h)` — bottom-left corner text (the title screen's version string).
    BottomLeft,
    /// `(w, h)` — bottom-right corner text (the copyright line).
    BottomRight,
    /// `(w, 0)` — top-right corner, for the non-vanilla `Accounts` title-screen
    /// button (see [`super::nav::MainButton::Accounts`]). Vanilla's own eight
    /// widgets already fill a 320×240 canvas (`config::MIN_SCALED_*`, the real
    /// floor `calculate_gui_scale` can produce) to within 16 px, so a ninth
    /// row appended below them does not fit at the minimum window size —
    /// measured, not assumed: `every_vanilla_widget_is_on_screen_and_none_overlap`
    /// caught it the first time this button was placed as `full(TITLE_PITCH * 5.0)`.
    /// The gap above the logo (`y < LOGO_Y`, i.e. `y < 30`) is free at every
    /// canvas size instead, which is where this corner sits.
    TopRight,
    /// `(w / 2, h)` — bottom-centre, for the account screen's row of
    /// non-vanilla nine-slice buttons (Add account / Select / Remove /
    /// Cancel). Not vanilla-sourced like the others above: nothing in
    /// `TitleScreen`/`PauseScreen` anchors a widget row to the bottom edge.
    ScreenBottom,
    /// `(w / 4, 0)` — the death screen's title anchor (issue #103).
    /// `DeathScreen.visitText` draws it at `middleLine / 2` where
    /// `middleLine = this.width / 2` (`DeathScreen.java:118-120`), i.e.
    /// **centred on the screen's left quarter, not the middle** — this is
    /// vanilla's own layout (seemingly an oversight nobody ever fixed, not a
    /// deliberate design), reproduced faithfully rather than "corrected" to
    /// [`Origin::ScreenTop`].
    DeathTitle,
}

impl Origin {
    /// The anchor point in logical pixels for a canvas of `width`×`height`.
    #[must_use]
    pub fn anchor(self, width: f32, height: f32) -> (f32, f32) {
        match self {
            Origin::ScreenTop => (width * 0.5, 0.0),
            Origin::TitleTop => (width * 0.5, (height / 4.0).floor() + 48.0),
            Origin::PauseGrid => {
                let (grid_w, grid_h) = pause_grid_size();
                (
                    layout::align_in_dimension(0.0, width, grid_w, 0.5),
                    layout::align_in_dimension(0.0, height, grid_h, 0.25),
                )
            }
            Origin::BottomLeft => (0.0, height),
            Origin::BottomRight => (width, height),
            Origin::TopRight => (width, 0.0),
            Origin::ScreenBottom => (width * 0.5, height),
            Origin::DeathTitle => (width * 0.25, 0.0),
        }
    }
}

/// Where one vanilla-laid-out widget sits: an [`Origin`], an offset from it, and
/// a size. Pure — [`Slot::resolve`] turns it into a pixel rect for a given
/// canvas, and that rect is the **single** definition the renderer, the mouse
/// hover and the click hit-test all read (through [`row_rect`]).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Slot {
    /// The anchor this slot is measured from.
    pub origin: Origin,
    /// Horizontal offset from the anchor, in logical pixels.
    pub dx: f32,
    /// Vertical offset from the anchor, in logical pixels.
    pub dy: f32,
    /// Widget width in logical pixels.
    pub w: f32,
    /// Widget height in logical pixels.
    pub h: f32,
}

impl Slot {
    /// The pixel rect `(x, y, w, h)` for a canvas of `width`×`height`.
    #[must_use]
    pub fn resolve(self, width: f32, height: f32) -> (f32, f32, f32, f32) {
        let (ax, ay) = self.origin.anchor(width, height);
        (ax + self.dx, ay + self.dy, self.w, self.h)
    }
}

/// Vanilla's title-screen stack, **re-expressed** as a [`layout::LinearLayout`]
/// column: three full-width rows, the three icon buttons as a nested centred
/// horizontal row, then the Options/Quit pair as another horizontal row.
///
/// **Vanilla's `TitleScreen` uses no layout class at all** — it hand-centres on
/// `this.width / 2 - 100` and steps `topPos` by 24 (`TitleScreen.java:105-205`),
/// and #392's plan is explicit that a hand-arithmetic screen is legitimate
/// vanilla. What makes this re-expression faithful rather than invented is that
/// the two are *numerically identical*, which is not a coincidence:
///
/// - `spacing = 24` on 20 px buttons is a 4 px `rowSpacing`, so the rows land on
///   `0, 24, 48, 72, 96` either way.
/// - the column's width is `max(200, 68, 200) = 200`, so centring it on
///   `width / 2` is `width / 2 - 100`;
/// - `getHorizontalPosition(n, 3, 20)` is `width/2 - 34 + (n-1) * 24`
///   (`TitleScreen.java:170-173`), and a 68 px row centred in the 200 px column
///   is at `lerp(0.5, 0, 200 - 68) = 66`, i.e. `width/2 - 100 + 66` — the same
///   `width/2 - 34`. The 34 is `totalWidth / 2` and the 66 is `(200 - 68) / 2`;
///   they agree because `100 - 66 == 34`.
/// - `98 + 4 + 98 == 200`, so the half-width pair fills the column exactly and
///   its two children are at `+0` and `+102`.
///
/// `the_title_screen_rects_are_vanillas_own` asserts all eight rects against the
/// hand-derived table, so if the equality above ever stops holding, it fails.
fn title_menu_column() -> layout::LinearLayout {
    let button = |w: f32, h: f32| -> Box<dyn widget::LayoutElement> {
        Box::new(Widget::button(0.0, 0.0, w, h, ""))
    };
    // The gap `spacing = 24` leaves between two 20 px buttons.
    let row_spacing = (TITLE_PITCH - WIDGET_H) as i32;
    let mut column = layout::LinearLayout::vertical().spacing(row_spacing);
    for _ in 0..3 {
        column.add_child(button(WIDE_W, WIDGET_H));
    }
    // `getHorizontalPosition` centres the icon row in the stack's width.
    let mut icons = layout::LinearLayout::horizontal().spacing(row_spacing);
    for _ in 0..3 {
        icons.add_child(button(ICON_BTN, ICON_BTN));
    }
    column.add_child_settings(
        Box::new(icons),
        layout::LayoutSettings::defaults().align_horizontally_center(),
    );
    let mut pair = layout::LinearLayout::horizontal().spacing(row_spacing);
    for _ in 0..2 {
        pair.add_child(button(TITLE_HALF_W, WIDGET_H));
    }
    column.add_child(Box::new(pair));
    column.arrange_elements();
    column
}

/// Vanilla's `PauseScreen.createPauseMenu` (`PauseScreen.java:91-183`) as a real
/// [`layout::GridLayout`], arranged.
///
/// `menu_padding_top` is `MENU_PADDING_TOP` (50) in production; it is a parameter
/// only so `a_changed_cell_padding_moves_every_pause_rect` can run the negative
/// control #394 asks for — change one `LayoutSettings` padding value and watch the
/// rect assertions go red — against the real builder rather than a copy of it.
///
/// The full-width Options row is the `else` of vanilla's `hasSingleplayerServer()`
/// fork (`:157-163`); this client has no integrated server, so that branch is the
/// right one, and the grid therefore has five rows.
fn pause_menu_grid_with(menu_padding_top: i32) -> layout::GridLayout {
    let button = |w: f32, h: f32| -> Box<dyn widget::LayoutElement> {
        Box::new(Widget::button(0.0, 0.0, w, h, ""))
    };
    let mut grid = layout::GridLayout::new();
    {
        // `gridLayout.defaultCellSetting().padding(4, 4, 4, 0)` (`:93`) — the
        // *live* baseline, so every cell below inherits it.
        let baseline = grid.default_cell_setting();
        *baseline = baseline.padding_ltrb(
            PAUSE_BUTTON_PADDING,
            PAUSE_BUTTON_PADDING,
            PAUSE_BUTTON_PADDING,
            0,
        );
    }
    let mut helper = grid.create_row_helper(PAUSE_COLUMNS);
    // Back to Game: full width, and the one cell with the 50 px top padding.
    let first = helper.new_cell_settings().padding_top(menu_padding_top);
    helper.add_child_with(button(PAUSE_BUTTON_FULL_W, WIDGET_H), PAUSE_COLUMNS, first);
    // Advancements and Statistics share a row, one column each.
    helper.add_child(button(PAUSE_BUTTON_HALF_W, WIDGET_H));
    helper.add_child(button(PAUSE_BUTTON_HALF_W, WIDGET_H));
    // The four icon buttons are a nested horizontal row, spanning both columns
    // and horizontally centred in them (`:154`).
    let mut icons = layout::LinearLayout::horizontal().spacing(PAUSE_ICON_SPACING);
    for _ in 0..4 {
        icons.add_child(button(ICON_BTN, ICON_BTN));
    }
    let centred = helper.new_cell_settings().align_horizontally_center();
    helper.add_child_with(Box::new(icons), PAUSE_COLUMNS, centred);
    // Options, then Disconnect: both full width.
    helper.add_spanning(button(PAUSE_BUTTON_FULL_W, WIDGET_H), PAUSE_COLUMNS);
    helper.add_spanning(button(PAUSE_BUTTON_FULL_W, WIDGET_H), PAUSE_COLUMNS);
    drop(helper);
    grid.arrange_elements();
    grid
}

/// One arranged menu block: its own size, plus each leaf's rect in the order
/// `visit_widgets` yields them (which is insertion order, in vanilla too).
#[derive(Debug)]
struct MenuBlock {
    size: (f32, f32),
    cells: Vec<(f32, f32, f32, f32)>,
}

impl MenuBlock {
    /// Collect an **already-arranged** `root`'s leaves. `expected` is the number
    /// of drawable leaves the caller's button table needs; a mismatch is a tree
    /// that no longer describes the screen, and it must fail loudly rather than
    /// silently shift every rect by one.
    fn of(root: &dyn widget::LayoutElement, expected: usize) -> Self {
        let cells = layout::widget_rects(root);
        assert_eq!(
            cells.len(),
            expected,
            "the arranged tree has {} drawable leaves, the screen has {expected}",
            cells.len()
        );
        Self {
            size: (root.width(), root.height()),
            cells,
        }
    }
}

/// The title-screen column, arranged once.
///
/// Arranging is canvas-*independent* — only the final
/// `FrameLayout.alignInRectangle` step depends on the screen size, and that is
/// what [`Origin`] applies at draw time — so the tree is built once per process
/// rather than per frame. [`super::layout`]'s module docs say which of vanilla's
/// two two-phase timings this is, and why.
fn title_block() -> &'static MenuBlock {
    static BLOCK: std::sync::OnceLock<MenuBlock> = std::sync::OnceLock::new();
    // Vanilla's own eight. `MAIN_BUTTONS`' ninth, `Accounts`, is ours and is a
    // corner widget outside the column entirely.
    BLOCK.get_or_init(|| MenuBlock::of(&title_menu_column(), 8))
}

/// The pause-screen grid, arranged once. See [`title_block`].
fn pause_block() -> &'static MenuBlock {
    static BLOCK: std::sync::OnceLock<MenuBlock> = std::sync::OnceLock::new();
    // All nine of `PAUSE_BUTTONS`, four of them inside the nested icon row.
    BLOCK.get_or_init(|| MenuBlock::of(&pause_menu_grid_with(PAUSE_MENU_PADDING_TOP), 9))
}

/// The arranged pause grid's own `(width, height)` — what
/// [`Origin::PauseGrid`] aligns in the screen rect.
///
/// Public so a gate can check it against the hand-derived
/// [`PAUSE_GRID_W`]×[`PAUSE_GRID_H`] rather than restating either.
#[must_use]
pub fn pause_grid_size() -> (f32, f32) {
    pause_block().size
}

/// Vanilla's rect for one title-screen widget, from
/// `TitleScreen.init`/`createNormalMenuOptions`
/// (`TitleScreen.java:105-205`) — **read out of the arranged
/// `title_menu_column`**, not written down.
///
/// The offsets are relative to [`Origin::TitleTop`], whose x is `width / 2`,
/// so a cell's `dx` is its position in the column minus half the column's width.
#[must_use]
pub fn title_slot(button: MainButton) -> Slot {
    // The insertion order of `title_menu_column`, which is also `MAIN_BUTTONS`'
    // order for vanilla's own eight. Written as an exhaustive match rather than
    // `button as usize` so adding a variant fails to compile instead of silently
    // indexing the wrong cell.
    let index = match button {
        MainButton::Singleplayer => 0,
        MainButton::Multiplayer => 1,
        MainButton::Realms => 2,
        MainButton::Friends => 3,
        MainButton::Language => 4,
        MainButton::Accessibility => 5,
        MainButton::Options => 6,
        MainButton::Quit => 7,
        // Not vanilla — see `MainButton::Accounts`'s docs and
        // `Origin::TopRight`'s. A corner widget, not one more stack row:
        // vanilla's own eight already reach to within 16 px of the bottom of
        // a 320×240 canvas, so nothing fits below them there. The gap above
        // the logo (`y < LOGO_Y`) is free at every canvas size instead. It is
        // outside the arranged column entirely, which is why it returns early.
        MainButton::Accounts => {
            return Slot {
                origin: Origin::TopRight,
                dx: -(ACCOUNTS_ENTRY_W + ACCOUNTS_ENTRY_MARGIN),
                dy: ACCOUNTS_ENTRY_MARGIN,
                w: ACCOUNTS_ENTRY_W,
                h: WIDGET_H,
            };
        }
    };
    let block = title_block();
    let (x, y, w, h) = block.cells[index];
    Slot {
        origin: Origin::TitleTop,
        dx: x - block.size.0 * 0.5,
        dy: y,
        w,
        h,
    }
}

/// Width of the non-vanilla `Accounts` corner button — see
/// [`Origin::TopRight`]'s docs for why it lives there rather than in
/// vanilla's own vertical stack.
const ACCOUNTS_ENTRY_W: f32 = 90.0;
/// Distance from the top-right corner to the `Accounts` button, both axes.
const ACCOUNTS_ENTRY_MARGIN: f32 = 4.0;

/// Vanilla's rect for one pause-screen widget, from
/// `PauseScreen.createPauseMenu` (`PauseScreen.java:91-183`) — **read out of the
/// arranged grid** (`pause_menu_grid_with`) rather than resolved by hand.
///
/// It used to be a table of nine hand-derived offsets, and the derivation is
/// worth keeping because it is what the port has to reproduce: column widths are
/// `[106, 106]` (the 204+8 full-width cell split over two columns by `Divisor`);
/// row heights are `[70, 24, 24, 24, 24]`, so row y offsets are
/// `[0, 70, 94, 118, 142]`. Each child's own offset inside its cell is its
/// `paddingLeft`/`paddingTop` because the default `xAlignment` is 0 — and with
/// `padding(4, 4, 4, 0)` a full-width button's `mostOffset` is also 4, so
/// alignment could not move it anyway. The icon row is the one centred cell
/// (`alignHorizontallyCenter`, `PauseScreen.java:154`):
/// `lerp(0.5, 4, 212 - 92 - 4) = 60`, and its own `LinearLayout` spaces four
/// 20 px children 4 px apart from there — 60, 84, 108, 132.
///
/// That table now lives in `the_pause_screen_rects_are_vanillas_own`, where it is
/// the *expectation* instead of the implementation — an expected value has to come
/// from outside the code under test.
#[must_use]
pub fn pause_slot(button: PauseButton) -> Slot {
    // `pause_menu_grid_with`'s insertion order, which is `PAUSE_BUTTONS`' order.
    // Exhaustive rather than `button as usize` so a new variant is a compile
    // error and not a silent off-by-one across every rect.
    let index = match button {
        PauseButton::BackToGame => 0,
        PauseButton::Advancements => 1,
        PauseButton::Statistics => 2,
        PauseButton::ReportBugs => 3,
        PauseButton::Feedback => 4,
        PauseButton::Friends => 5,
        PauseButton::PlayerReporting => 6,
        PauseButton::Options => 7,
        PauseButton::QuitToTitle => 8,
    };
    let (dx, dy, w, h) = pause_block().cells[index];
    Slot {
        origin: Origin::PauseGrid,
        dx,
        dy,
        w,
        h,
    }
}

/// Vanilla's rect for one death-screen button (issue #103):
/// `this.width / 2 - 100, this.height / 4 + 72 | 96, 200, 20`
/// (`DeathScreen.java:47-58`). Both buttons share `x`; only `y` differs.
///
/// `height / 4 + 72` and `+ 96` are `Origin::TitleTop`'s own anchor
/// (`height / 4 + 48`, `TitleScreen.java:113`) plus `24`/`48` — the death
/// screen and the title screen both lay their stacks out from
/// `this.height / 4`, so reusing that origin here rather than adding a
/// second one is deliberate, not a coincidence to "clean up".
#[must_use]
pub fn death_slot(button: super::nav::DeathButton) -> Slot {
    use super::nav::DeathButton;
    let dy = match button {
        DeathButton::Respawn => 24.0,
        DeathButton::TitleScreen => 48.0,
    };
    Slot {
        origin: Origin::TitleTop,
        dx: -100.0,
        dy,
        w: WIDE_W,
        h: WIDGET_H,
    }
}

// -- vanilla's `SelectWorldScreen` metrics (issue #397) ----------------------
//
// Same rule as the block above: every number is transcribed from
// `.cache/mc/26.2/client-src`, with the file and line named, in logical GUI
// pixels.

/// The header band, spelled out as `8 + 9 + 8 + 20 + 4` in the constructor
/// (`SelectWorldScreen.java:31`) and left unreduced here for the reason it is
/// unreduced there: the parts *are* the layout — 8 px of slack above and below,
/// the 9 px title `StringWidget`, the 20 px search box, and the 4 px
/// `LinearLayout` spacing between the two.
const WORLD_SELECT_HEADER_H: f32 = 8.0 + 9.0 + 8.0 + 20.0 + 4.0;
/// The footer band (`SelectWorldScreen.java:31`). Two 20 px button rows 4 px
/// apart measure 44, so the band carries 16 px of slack, which the footer
/// `FrameLayout`'s inherited `align(0.5, 0.5)` splits 8/8.
const WORLD_SELECT_FOOTER_H: f32 = 60.0;
/// `LinearLayout.vertical().spacing(4)` in the header and `.rowSpacing(4)` in
/// the footer grid (`SelectWorldScreen.java:46,82`) — the same 4 either way.
const WORLD_SELECT_SPACING: i32 = 4;
/// `new GridLayout().columnSpacing(8)` (`SelectWorldScreen.java:82`).
const WORLD_SELECT_COLUMN_SPACING: i32 = 8;
/// `footer.createRowHelper(4)` (`SelectWorldScreen.java:84`).
const WORLD_SELECT_FOOTER_COLUMNS: usize = 4;
/// The search box's declared size — `new EditBox(font, this.width / 2 - 100, 22,
/// 200, 20, …)` (`SelectWorldScreen.java:55`). **The `x` and `y` in that call
/// are dead**: the header `LinearLayout` overwrites both when it arranges, which
/// is why the box lands at y `21` rather than the 22 written there.
const WORLD_SELECT_SEARCH_W: f32 = 200.0;
/// `.width(71)` on Edit / Delete / Re-Create / Back
/// (`SelectWorldScreen.java:91,96,103,106`). Play and Create take
/// `Button.DEFAULT_WIDTH` instead ([`widget::DEFAULT_WIDTH`]) and each spans two
/// of the four columns.
const WORLD_SELECT_SMALL_BTN_W: f32 = 71.0;
/// A `StringWidget`'s height: `StringWidget(message, font)` delegates to
/// `this(0, 0, font.width(...), 9, ...)` (`StringWidget.java:18-20`).
const STRING_WIDGET_H: f32 = 9.0;
/// `WorldSelectionList.getRowWidth()` (`WorldSelectionList.java:247-249`) — a
/// 270 px override of `AbstractSelectionList`'s own 220 (`:389-391`).
const WORLD_LIST_ROW_W: f32 = 270.0;
/// The list's `itemHeight`: the last argument of
/// `super(minecraft, width, height, 0, 36)` (`WorldSelectionList.java:112`).
const WORLD_LIST_ITEM_H: f32 = 36.0;
/// `AbstractSelectionList.Entry.CONTENT_PADDING` (`:436`). Every `getContentX`/
/// `getContentY` insets the entry rect by 2 and `getContentWidth`/
/// `getContentHeight` by 4 (`:477-495`), so a 36 px row has a **32** px content
/// box — which is exactly the world icon's 32×32 (`WorldListEntry.ICON_SIZE`,
/// `:400`).
const LIST_CONTENT_PADDING: f32 = 2.0;
/// `getFirstEntryY() = getY() + 2` (`AbstractSelectionList.java:104-106`): the
/// gap above the first row. Not [`LIST_CONTENT_PADDING`], even though it is also
/// 2 — they are different expressions and only one of them scales with a row.
const WORLD_LIST_FIRST_ENTRY_Y: f32 = 2.0;

/// The canvas [`world_select_block`] arranges its tree at.
///
/// [`layout::HeaderAndFooterLayout`] is the first container here that is
/// **canvas-dependent** — it pins the footer to `screen.height` and centres both
/// bands in `screen.width` — so unlike [`title_block`] and [`pause_block`] its
/// arranged rects are not directly reusable at another size. What *is*
/// canvas-independent is every rect once it is expressed relative to the right
/// [`Origin`]: the header column measures 200 wide and the footer grid 308
/// whatever the screen is, and the content band always begins at the header
/// height (see [`WorldSelectBlock::at`]). The slots are therefore derived once
/// here and asserted invariant at three different canvases by
/// `the_world_select_slots_do_not_depend_on_the_reference_canvas` — which is the
/// only thing standing between this and a screen that is correct at 854×480 and
/// wrong everywhere else.
const WORLD_SELECT_REF_CANVAS: (f32, f32) = (854.0, 480.0);

/// Vanilla's `SelectWorldScreen.init` (`SelectWorldScreen.java:44-107`) as a
/// real [`layout::HeaderAndFooterLayout`], arranged for a `width`×`height`
/// canvas.
///
/// Three things about it are worth knowing before changing it:
///
/// - **The title cell is zero-width on purpose.** Vanilla's
///   `StringWidget(this.title, this.font)` is `font.width(title)` wide, and this
///   shell has no font at arrange time. It does not matter: the cell is
///   `alignHorizontallyCenter`ed in the 200 px column, so a `w`-wide title lands
///   at `colX + (200 - w) / 2` and its *centre* is `colX + 100` for every `w`
///   short of 200. A zero-width cell puts the leaf rect exactly on that centre,
///   which is what [`world_select_title_label`] draws from — so the arranged
///   position is the real one rather than an approximation of it.
/// - **The list is a [`layout::SpacerElement`], not a widget.** It has to take
///   part in the measurement, because `HeaderAndFooterLayout`'s content clamp
///   reads the content frame's *height* (`min(headerHeight + 30, screenHeight -
///   footerHeight - contentHeight)`), and vanilla sizes the list to
///   `layout.getContentHeight()` exactly (`:68`) — which is what makes the clamp
///   pick the header height. A spacer's `visit_widgets` is a no-op, so it is
///   measured and never drawn, which is also true of vanilla's list here: it is
///   an `AbstractWidget` but not one this shell has ported.
/// - **`SharedConstants.DEBUG_WORLD_RECREATE` is a system-property debug flag**
///   (`SharedConstants.java:119`), false in any shipped client, so the sub-header
///   holds the search box alone (`:50-53`).
fn world_select_layout(width: f32, height: f32) -> layout::HeaderAndFooterLayout {
    let cell = |w: f32, h: f32| -> Box<dyn widget::LayoutElement> {
        Box::new(Widget::button(0.0, 0.0, w, h, ""))
    };
    let mut root = layout::HeaderAndFooterLayout::with_heights(
        width,
        height,
        WORLD_SELECT_HEADER_H,
        WORLD_SELECT_FOOTER_H,
    );

    // `LinearLayout header = this.layout.addToHeader(LinearLayout.vertical().spacing(4));`
    // `header.defaultCellSetting().alignHorizontallyCenter();` (`:46-47`)
    let mut header = layout::LinearLayout::vertical().spacing(WORLD_SELECT_SPACING);
    {
        let baseline = header.default_cell_setting();
        *baseline = baseline.align_horizontally_center();
    }
    header.add_child(Box::new(Widget::new(
        0.0,
        0.0,
        0.0,
        STRING_WIDGET_H,
        "Select World",
    )));
    let mut sub_header = layout::LinearLayout::horizontal().spacing(WORLD_SELECT_SPACING);
    sub_header.add_child(cell(WORLD_SELECT_SEARCH_W, WIDGET_H));
    header.add_child(Box::new(sub_header));
    root.add_to_header(Box::new(header));

    // The list, sized to the content band exactly as `:67-68` does.
    let content_height = root.content_height();
    root.add_to_contents(Box::new(layout::SpacerElement::new(width, content_height)));

    // `GridLayout footer = this.layout.addToFooter(new GridLayout().columnSpacing(8).rowSpacing(4));`
    // `footer.defaultCellSetting().alignHorizontallyCenter();` (`:82-84`)
    let mut footer = layout::GridLayout::new()
        .column_spacing(WORLD_SELECT_COLUMN_SPACING)
        .row_spacing(WORLD_SELECT_SPACING);
    {
        let baseline = footer.default_cell_setting();
        *baseline = baseline.align_horizontally_center();
    }
    let mut helper = footer.create_row_helper(WORLD_SELECT_FOOTER_COLUMNS);
    // Row 1: Play and Create, `Button.DEFAULT_WIDTH` each, two columns each
    // (`:85-88`). Their 150 px is what sets all four column widths: a two-column
    // span of 150 with an 8 px gutter splits 71/71 through `Divisor`, and the
    // four 71 px buttons below can then only *match* it.
    for _ in 0..2 {
        helper.add_spanning(cell(widget::DEFAULT_WIDTH, WIDGET_H), 2);
    }
    // Row 2: Edit, Delete, Re-Create, Back — one column each (`:89-106`).
    for _ in 0..4 {
        helper.add_child(cell(WORLD_SELECT_SMALL_BTN_W, WIDGET_H));
    }
    drop(helper);
    root.add_to_footer(Box::new(footer));

    root.arrange_elements();
    root
}

/// One arranged world-select screen: the header's leaf rects, the footer's, and
/// where the content band starts.
///
/// Split by band rather than flattened into one `Vec` the way [`MenuBlock`] is,
/// because the two bands are anchored to *different* [`Origin`]s — the header to
/// the top of the screen and the footer to the bottom — so a flat list of
/// absolute rects could not be converted to canvas-independent offsets.
#[derive(Debug)]
struct WorldSelectBlock {
    /// The header column's leaves, in insertion order: the title cell, then the
    /// search box.
    header: Vec<(f32, f32, f32, f32)>,
    /// The footer grid's leaves, in [`super::world_select::WORLD_SELECT_BUTTONS`]'
    /// order.
    footer: Vec<(f32, f32, f32, f32)>,
    /// The content frame's top, i.e. `list.getY()`.
    content_top: f32,
    /// The canvas this was arranged at, so the band offsets can be made relative
    /// to it.
    canvas: (f32, f32),
}

impl WorldSelectBlock {
    /// Arrange the tree at `width`×`height` and read its leaves back.
    ///
    /// The leaf counts are asserted rather than trusted, for [`MenuBlock::of`]'s
    /// reason: a tree that no longer describes the screen must fail loudly
    /// instead of silently shifting every rect by one.
    fn at(width: f32, height: f32) -> Self {
        let root = world_select_layout(width, height);
        let header = layout::widget_rects(root.header());
        let footer = layout::widget_rects(root.footer());
        assert_eq!(
            header.len(),
            2,
            "the world-select header has {} leaves, the screen has 2 (title, search)",
            header.len()
        );
        assert_eq!(
            footer.len(),
            super::world_select::WORLD_SELECT_BUTTONS.len(),
            "the world-select footer has {} leaves, the screen has {}",
            footer.len(),
            super::world_select::WORLD_SELECT_BUTTONS.len()
        );
        Self {
            header,
            footer,
            content_top: root.contents().y(),
            canvas: (width, height),
        }
    }

    /// A header leaf as a slot measured from [`Origin::ScreenTop`].
    fn header_slot(&self, index: usize) -> Slot {
        let (x, y, w, h) = self.header[index];
        Slot {
            origin: Origin::ScreenTop,
            dx: x - self.canvas.0 * 0.5,
            dy: y,
            w,
            h,
        }
    }

    /// A footer leaf as a slot measured from [`Origin::ScreenBottom`]. Its `dy`
    /// is negative — the footer is pinned to the bottom edge.
    fn footer_slot(&self, index: usize) -> Slot {
        let (x, y, w, h) = self.footer[index];
        Slot {
            origin: Origin::ScreenBottom,
            dx: x - self.canvas.0 * 0.5,
            dy: y - self.canvas.1,
            w,
            h,
        }
    }
}

/// The world-select screen, arranged once at [`WORLD_SELECT_REF_CANVAS`]. See
/// [`title_block`] on why arranging once is safe, and
/// [`WORLD_SELECT_REF_CANVAS`] on the extra condition that applies here.
fn world_select_block() -> &'static WorldSelectBlock {
    static BLOCK: std::sync::OnceLock<WorldSelectBlock> = std::sync::OnceLock::new();
    BLOCK.get_or_init(|| {
        WorldSelectBlock::at(WORLD_SELECT_REF_CANVAS.0, WORLD_SELECT_REF_CANVAS.1)
    })
}

/// The search box's rect, read out of the arranged header.
#[must_use]
pub fn world_select_search_slot() -> Slot {
    world_select_block().header_slot(1)
}

/// Vanilla's rect for one footer button, read out of the arranged grid.
///
/// Exhaustive rather than an `as usize`, for [`title_slot`]'s reason: a new
/// variant must be a compile error, not a silent off-by-one across every rect.
#[must_use]
pub fn world_select_slot(button: super::world_select::WorldSelectButton) -> Slot {
    use super::world_select::WorldSelectButton as B;
    let index = match button {
        B::Play => 0,
        B::Create => 1,
        B::Edit => 2,
        B::Delete => 3,
        B::ReCreate => 4,
        B::Back => 5,
    };
    world_select_block().footer_slot(index)
}

/// The title `StringWidget`'s label, positioned from the arranged header's own
/// title cell.
///
/// `Align::Centre` because the cell is zero-width and therefore *is* the text's
/// centre — see [`world_select_layout`]. `StringWidget.visitLines` draws at
/// `y + (height - 9) / 2`, which is `y` for a 9 px widget
/// (`StringWidget.java:64`), so the cell's `y` is the text's top.
#[must_use]
pub fn world_select_title_label() -> MenuLabel {
    let slot = world_select_block().header_slot(0);
    MenuLabel {
        text: super::world_select::WORLD_SELECT_TITLE.to_string(),
        origin: slot.origin,
        dx: slot.dx,
        dy: slot.dy,
        align: Align::Centre,
        colour: LABEL,
        scale: 1.0,
    }
}

/// The top of world-list row `index`, in logical pixels.
///
/// `getFirstEntryY() + index * itemHeight` with no scroll term, because
/// `scrollAmount` is 0 for a list that cannot overflow its band — which is the
/// only list this screen has (see [`super::world_select`]). Canvas-independent:
/// `list.getY()` is the content band's top, which
/// `HeaderAndFooterLayout.arrangeElements` clamps to exactly the header height
/// whenever the content is sized to `getContentHeight()`.
#[must_use]
pub fn world_list_row_top(index: usize) -> f32 {
    world_select_block().content_top + WORLD_LIST_FIRST_ENTRY_Y + index as f32 * WORLD_LIST_ITEM_H
}

/// The left edge of every world-list row: `getRowLeft()`, which is
/// `getX() + this.width / 2 - getRowWidth() / 2` with `getX() == 0`
/// (`AbstractSelectionList.java:372-374`). The `floor` is Java's integer
/// division of an odd canvas width, and it is the reason this takes a width
/// rather than being folded into a slot.
#[must_use]
pub fn world_list_row_left(width: f32) -> f32 {
    (width * 0.5).floor() - (WORLD_LIST_ROW_W * 0.5).floor()
}

/// The rect of world-list row `index` at a `width`-wide canvas.
#[must_use]
pub fn world_list_row_rect(index: usize, width: f32) -> (f32, f32, f32, f32) {
    (
        world_list_row_left(width),
        world_list_row_top(index),
        WORLD_LIST_ROW_W,
        WORLD_LIST_ITEM_H,
    )
}

/// A row's *content* rect — the entry rect inset by
/// [`LIST_CONTENT_PADDING`]/twice it (`AbstractSelectionList.java:477-495`).
/// This is where a `WorldListEntry` puts its 32×32 icon and, at
/// `x + 32 + 3`, its three text lines (`WorldSelectionList.java:494-502,569-571`).
#[must_use]
pub fn world_list_row_content_rect(index: usize, width: f32) -> (f32, f32, f32, f32) {
    let (x, y, w, h) = world_list_row_rect(index, width);
    (
        x + LIST_CONTENT_PADDING,
        y + LIST_CONTENT_PADDING,
        w - 2.0 * LIST_CONTENT_PADDING,
        h - 2.0 * LIST_CONTENT_PADDING,
    )
}

/// The one entry the list actually has: vanilla's `NoWorldsEntry` geometry
/// carrying [`super::world_select::NO_WORLDS_MESSAGE`].
///
/// `NoWorldsEntry.extractContent` centres a `StringWidget` in the entry's
/// content box — `setPosition(getContentXMiddle() - width / 2, getContentYMiddle()
/// - height / 2)` (`WorldSelectionList.java:392-396`) — and a `StringWidget`'s
/// own draw then adds `(height - 9) / 2`, which is 0 for its 9 px height. So the
/// text's top is `contentYMiddle - 4`, and the `4` is Java's `9 / 2`, not 4.5.
///
/// `contentXMiddle` is `getContentX() + getContentWidth() / 2` =
/// `rowLeft + 2 + 133`, and `rowLeft` is `floor(w / 2) - 135`, so the two 133/135
/// halves cancel and the centre is `floor(w / 2)` — the screen's own centre,
/// which is why this is an [`Origin::ScreenTop`] label with `dx: 0`.
#[must_use]
pub fn world_select_no_worlds_label() -> MenuLabel {
    // The `x` and `w` of the content rect are discarded — the label is centred on
    // the screen for the reason above — so the width passed here is arbitrary.
    // Reading the rect anyway, instead of restating `row_top + 2`, is
    // `CLAUDE.md`'s "derive layout from the same expression the draw uses".
    let (_, content_y, _, content_h) = world_list_row_content_rect(0, 0.0);
    MenuLabel {
        text: super::world_select::NO_WORLDS_MESSAGE.to_string(),
        origin: Origin::ScreenTop,
        dx: 0.0,
        dy: content_y + (content_h * 0.5).floor() - (STRING_WIDGET_H * 0.5).floor(),
        align: Align::Centre,
        colour: LABEL,
        scale: 1.0,
    }
}
/// Horizontal alignment of a [`MenuLabel`] about its anchored x.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Align {
    /// `x` is the text's left edge.
    Left,
    /// `x` is the text's centre.
    Centre,
    /// `x` is the text's right edge. The width is measured at draw time, which
    /// is why this is an alignment and not a pre-computed offset: vanilla's own
    /// `copyrightX = width - font.width(text) - 2` (`TitleScreen.java:110-111`)
    /// depends on the font, and the font is not known until the draw.
    Right,
}

/// A free-standing string a vanilla-laid-out screen draws, outside any widget.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuLabel {
    /// The text.
    pub text: String,
    /// Anchor the position is measured from.
    pub origin: Origin,
    /// Horizontal offset from the anchor, before [`Self::align`] is applied.
    pub dx: f32,
    /// Vertical offset from the anchor — the **top** of the line.
    pub dy: f32,
    /// How `dx` relates to the text's own box.
    pub align: Align,
    /// RGBA, sRGB 0..1 written verbatim (the shell's convention — see
    /// `docs/vanilla-hud-text.md`).
    pub colour: [f32; 4],
    /// Font-pixel scale. `1.0` for ordinary vanilla component text (every
    /// label before issue #103 used this implicitly — `build`'s `frame.vanilla`
    /// loop hardcoded it). The death screen's title needs `2.0`:
    /// `DeathScreen.visitText` sets `output.defaultParameters(normalParameters.
    /// withScale(2.0F))` before drawing it (`DeathScreen.java:23,119`).
    pub scale: f32,
}

/// One drawable row: a main-menu button, a server, or a form field.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MenuRow {
    /// Primary label, drawn at [`TEXT_SCALE`].
    pub label: String,
    /// Second line (MOTD, address, hint), drawn small and dim.
    pub detail: String,
    /// Right-aligned trailing text (players, latency).
    pub trailing: String,
    /// Favicon to draw at the row's left edge.
    pub favicon: Option<FaviconMosaic>,
    /// A player head to draw at the row's left edge instead of a favicon —
    /// the account list's own icon (issue #66/#62). Drawn through the exact
    /// same [`FaviconMosaic`] path as `favicon`: a head is not a conceptually
    /// different kind of "small square texture", so it gets no second
    /// drawable type or draw call to drift from the favicon one. See
    /// [`default_head_icon`] for why the *texture* is a parameter here
    /// rather than a hardcoded draw, which is what makes swapping in a real
    /// downloaded skin later (issue #62) a data change, not a rewrite.
    pub head: Option<FaviconMosaic>,
    /// Whether the row can be activated (a failed row is still selectable).
    pub enabled: bool,
    /// Draw `detail` in the failure colour.
    pub detail_is_error: bool,
    /// Draw the row as a text-entry field.
    ///
    /// With [`Self::edit`] set this only selects the field *fill* for the
    /// jar-less fallback; the caret, the selection and the visible slice all come
    /// from the widget. Without it, the pre-#395 draw applies: the whole label
    /// with a caret parked after it.
    pub field: bool,
    /// The live [`EditBox`] this row draws — a **clone**, taken per frame from
    /// [`super::nav::EditForm`]'s persistent widgets.
    ///
    /// This is the one piece of menu state that is not derivable from the screen
    /// (a caret and a scroll offset are not), so the widget outlives the frame
    /// and the frame carries a copy. `build`'s `draw_edit_box` repositions the
    /// copy into this frame's rect — `OptionsSubScreen.repositionElements`'
    /// order, not `rebuildWidgets`' — and then *asks* it for its geometry rather
    /// than restating any of `EditBox`'s arithmetic here. See
    /// [`super::edit_box`] and [`super::nav::EditForm`].
    pub edit: Option<EditBox>,
    /// Vanilla placement. `Some` puts the row at a rect derived from vanilla's
    /// own arithmetic ([`title_slot`] / [`pause_slot`]) and draws it as a real
    /// `widget/button*` nine-slice sprite; `None` keeps the centred row stack
    /// the server list, the edit form, Options and the error screen use.
    pub slot: Option<Slot>,
    /// A GUI sprite id drawn centred in the widget **instead of** `label` —
    /// vanilla's `SpriteIconButton.CenteredIcon`
    /// (`SpriteIconButton.java:236-244`). `label` is still carried (it is the
    /// tooltip/narration text in vanilla) but not drawn.
    pub icon: Option<&'static str>,
}

/// Everything one menu screen draws.
#[derive(Debug, Clone, Default)]
pub struct MenuFrame<'a> {
    /// Big heading, e.g. `"LODESTONE"`.
    pub title: &'a str,
    /// Small line under the heading.
    pub subtitle: &'a str,
    /// The rows, top to bottom.
    pub rows: Vec<MenuRow>,
    /// Index of the highlighted row. Out-of-range highlights nothing.
    ///
    /// On a screen with a single row cursor this is both "the keyboard is here"
    /// and "the mouse is here", which is why [`draw_widget`] feeds it to
    /// `Widget::focused`. On a screen with real focus
    /// ([`super::Screen::WorldSelect`]) it is the **focused** row only, and
    /// [`Self::hovered`] carries the other fact.
    pub selected: usize,
    /// The row the cursor is over, when that is a different question from
    /// [`Self::selected`].
    ///
    /// `None` on every screen with a row cursor, which is every screen except
    /// [`super::Screen::WorldSelect`] — so nothing about the existing screens'
    /// pixels changes. Vanilla's sprite argument is `isHoveredOrFocused()`
    /// (`AbstractButton.java:43-53`), the `||` of the two, and
    /// [`Widget::is_hovered_or_focused`] is where that join lives; this field is
    /// only how the second operand reaches it. See
    /// [`super::world_select::WorldSelectNav::hovered`] for the bug that made the
    /// split necessary — one flag would let a mouse-over steal the keyboard out
    /// of a text field.
    pub hovered: Option<usize>,
    /// Key-hint lines drawn at the bottom.
    pub footer: Vec<String>,
    /// A message above the footer, drawn in the failure colour.
    pub message: Option<String>,
    /// The user's `gui_scale` option (`0` = auto). [`frame_for`] stamps this
    /// onto every screen's frame, not just [`super::Screen::Settings`]'s — the
    /// whole menu must scale, not only the screen that edits the setting.
    /// Carried on the frame rather than as a new parameter to
    /// [`MenuRenderer::render`] so that call site (owned by `app.rs`) does not
    /// need to change. See [`logical_canvas`].
    pub gui_scale: u32,
    /// Whether this frame is drawn **over** an already-rendered scene rather
    /// than replacing it — [`Screen::Paused`](super::Screen::Paused)'s pause
    /// menu, via [`pause_frame`] and
    /// [`MenuRenderer::render_overlay`]. Changes only how [`geometry`] paints
    /// the full-screen backdrop (translucent instead of opaque, so the world
    /// stays visible behind the buttons); every other screen leaves this
    /// `false` via `..Default::default()`.
    pub overlay: bool,
    /// This frame reproduces one of **vanilla's own** screens: its rows carry
    /// [`MenuRow::slot`]s, its buttons draw as `widget/button*` nine-slice
    /// sprites, and the row-stack's centred title/subtitle/footer block is
    /// suppressed in favour of [`Self::labels`].
    ///
    /// A flag rather than an inference from `rows[0].slot.is_some()`: the two
    /// are different questions (a screen could gain one slotted row), and a
    /// screen silently switching layout mode because of a row edit is exactly
    /// the kind of drift this file's `owns_frame`/`frame_for` agreement test
    /// exists to prevent.
    pub vanilla: bool,
    /// Draw vanilla's `title/minecraft` + `title/edition` logo pair at the top —
    /// the title screen only. A no-op without a GUI atlas carrying those loose
    /// textures (see [`crate::resources::TITLE_TEXTURES`]).
    pub logo: bool,
    /// Free-standing strings at vanilla-derived positions: the pause screen's
    /// "Game Menu" heading, the title screen's version string and copyright
    /// line.
    pub labels: Vec<MenuLabel>,
}

/// Decoded favicon mosaics, keyed by the status cache's address key.
///
/// Without this, [`frame_for`] would decode every visible server's PNG **every
/// frame** — 60 zlib inflations per second per row for an image that never
/// changes. The cache is keyed by address rather than by row index so reordering
/// or renaming the list does not invalidate it.
#[derive(Debug, Default)]
pub struct FaviconCache {
    /// `None` means "we tried and it did not decode"; that is cached too, so a
    /// broken icon is not re-decoded forever.
    decoded: std::collections::HashMap<String, Option<FaviconMosaic>>,
}

impl FaviconCache {
    /// An empty cache.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The mosaic for `key`, decoding `png` on first use.
    pub fn get(&mut self, key: &str, png: &[u8]) -> Option<FaviconMosaic> {
        if let Some(hit) = self.decoded.get(key) {
            return hit.clone();
        }
        let m = favicon_mosaic(png);
        self.decoded.insert(key.to_string(), m.clone());
        m
    }

    /// Drops the entry for `key` (its server was deleted or re-addressed).
    pub fn forget(&mut self, key: &str) {
        self.decoded.remove(key);
    }

    /// Number of cached decodes, for tests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.decoded.len()
    }

    /// Whether nothing has been decoded yet.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.decoded.is_empty()
    }
}

/// Whether [`frame_for`] will produce a frame for `screen`, i.e. whether this
/// renderer owns the frame (and, in the app, the keyboard).
///
/// Kept beside `frame_for` with a test asserting the two agree for every screen:
/// a predicate that drifts from the builder gives either a screen drawn twice or
/// one drawn not at all.
///
/// [`Screen::Paused`] is **deliberately excluded**, even though it has its own
/// button rows and keyboard navigation (see [`pause_frame`] and
/// [`super::nav::MenuNav`]'s `key_paused`): this set governs the Clear pass
/// that replaces the whole frame, and the pause menu is drawn as an overlay
/// over the world instead (see [`MenuRenderer::render_overlay`]) — the world
/// keeps rendering (and, on a live server, keeps ticking) behind it. Adding
/// `Screen::Paused` here would stop the world rendering for as long as the
/// game is paused, which is exactly the regression [`super::Screen::Paused`]'s
/// own doc comment warns against.
#[must_use]
pub fn owns_frame(screen: super::Screen) -> bool {
    use super::Screen;
    matches!(
        screen,
        Screen::MainMenu
            | Screen::ServerList
            | Screen::ServerEdit
            | Screen::WorldSelect
            | Screen::Settings
            | Screen::Accounts
            | Screen::Error
    )
}

/// Vanilla's `title.credits` string (`en_us.json`), drawn bottom-right on the
/// title screen exactly as `TitleScreen.init` does
/// (`TitleScreen.java:49,110-111,150-160`). It refers to the Mojang GUI assets
/// this screen is drawn with, which are genuinely Mojang's, so it is reproduced
/// verbatim.
const COPYRIGHT: &str = "Copyright Mojang AB. Do not distribute!";

/// The bottom-left corner string, vanilla's
/// `"Minecraft " + version.name()` (+ `menu.modded` for a modified client,
/// `TitleScreen.java:314-323`).
///
/// A from-scratch reimplementation is about as "modified" as a client gets, so
/// naming Lodestone and its version here is this line's honest equivalent —
/// claiming to be plain `Minecraft 26.2` would be the dishonest option.
fn version_line() -> String {
    format!("Minecraft 26.2 (Lodestone {})", env!("CARGO_PKG_VERSION"))
}

/// Builds the pause menu's overlay frame: vanilla's **nine** widgets at
/// vanilla's rects (see [`pause_slot`] and [`super::nav::PauseButton`]), six of
/// them present-and-disabled, with the highlight tracking
/// [`super::nav::MenuNav::pause_index`].
///
/// Unlike [`frame_for`], this is not gated by [`owns_frame`] and takes no
/// `UiState`/`StatusCache`/`FaviconCache` — the pause menu has no server list
/// or connection status to show, just the nav's own selection. Callers draw it
/// with [`MenuRenderer::render_overlay`], not [`MenuRenderer::render`], every
/// frame the game is paused, over whatever the world/HUD/container passes
/// already drew — see the [`super::Screen::Paused`] doc comment for why that
/// split exists.
#[must_use]
pub fn pause_frame(nav: &super::nav::MenuNav) -> MenuFrame<'static> {
    use super::nav::PAUSE_BUTTONS;
    MenuFrame {
        rows: PAUSE_BUTTONS
            .iter()
            .map(|b| MenuRow {
                label: b.label().to_string(),
                enabled: b.enabled(),
                slot: Some(pause_slot(*b)),
                icon: b.icon(),
                ..Default::default()
            })
            .collect(),
        selected: nav.pause_index(),
        gui_scale: nav.gui_scale(),
        overlay: true,
        vanilla: true,
        // `PauseScreen.init` adds a `StringWidget` with the screen title at
        // y=40 when the pause menu is showing (`PauseScreen.java:87-88`); the
        // title itself is `menu.game` == "Game Menu" (`PauseScreen.java:63,73`).
        labels: vec![MenuLabel {
            text: "Game Menu".to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: PAUSE_TITLE_Y,
            align: Align::Centre,
            colour: LABEL,
            scale: 1.0,
        }],
        ..Default::default()
    }
}

/// The score line's format, vanilla's `deathScreen.score.value` with the
/// value substituted (`DeathScreen.java:38-39`).
const DEATH_SCORE_UNTRACKED: &str = "Score: 0";

/// Builds the death screen's overlay frame (issue #103): vanilla's
/// `DeathScreen` — the title, the server's death message, the score line, and
/// two buttons (Respawn / Title Screen) at vanilla's rects (see
/// [`death_slot`] and [`super::nav::DeathButton`]) — reproduced from
/// `.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/DeathScreen.java`.
///
/// Like [`pause_frame`], not gated by [`owns_frame`]: the world (and, on a
/// live server, the session) keeps rendering and ticking behind it — a dead
/// player is held with no chunk stream until the respawn this screen gates,
/// and this overlay must not itself stop that (see
/// [`super::Screen::Death`]'s doc comment). Callers draw it with
/// [`MenuRenderer::render_overlay`] every frame the death screen is up, and
/// resolve the highlighted row through [`super::nav::MenuNav::death_index`]
/// exactly like [`pause_frame`] does for [`super::nav::MenuNav::pause_index`].
///
/// `message` is the server's own death message
/// (`net::NetUpdate::Death`/`Sim::death_message`, already flattened to plain
/// text) — `None` draws no message line, matching vanilla's own `if
/// (this.causeOfDeath != null)` guard (`DeathScreen.java:122-124`).
///
/// Two simplifications named rather than silently taken:
/// - **No hardcore variant.** This client has no hardcore mode (nothing
///   decodes a client-visible hardcore flag), so the title is always
///   `deathScreen.title` ("You Died!") and the first button is always
///   `deathScreen.respawn` ("Respawn"), never the hardcore
///   `deathScreen.title.hardcore` ("Game Over!") / `deathScreen.spectate`
///   pair — see [`super::nav::DeathButton`].
/// - **The score line is always [`DEATH_SCORE_UNTRACKED`].** Vanilla's score
///   is `LocalPlayer.getScore()`, synced through a `Player`-entity metadata
///   field (`Player.DATA_SCORE_ID`) nothing in this workspace decodes yet.
///   Drawing the vanilla line at the vanilla position with the only value
///   available (0) is the same "present, honestly simplified" choice
///   `docs/main-menu.md`/`docs/pause-menu.md` make for a present-but-disabled
///   button, rather than omitting the line and drawing a screen vanilla would
///   not recognise the shape of.
///
/// The backdrop is [`OVERLAY_BG`] — the same flat dim [`pause_frame`] draws
/// — rather than vanilla's own reddish `fillGradient`
/// (`DeathScreen.java:134-136`): this pipeline's [`Quads::rect`] takes one
/// flat colour with no per-vertex gradient, and reproducing the gradient
/// would mean extending it for one screen. Left for polish, like the
/// panorama/splash-text gaps `docs/main-menu.md` names for the title screen.
#[must_use]
pub fn death_frame(nav: &super::nav::MenuNav, message: Option<&str>) -> MenuFrame<'static> {
    use super::nav::DEATH_BUTTONS;

    let mut labels = vec![
        // `output.defaultParameters(normalParameters.withScale(2.0F))` then
        // drawn at `(middleLine / 2, 30)` (`DeathScreen.java:119-120`) — see
        // `Origin::DeathTitle`'s doc for why that x is `width / 4`, not the
        // screen centre.
        MenuLabel {
            text: "You Died!".to_string(),
            origin: Origin::DeathTitle,
            dx: 0.0,
            dy: 30.0,
            align: Align::Centre,
            colour: LABEL,
            scale: 2.0,
        },
    ];
    if let Some(text) = message
        && !text.is_empty()
    {
        // `output.accept(CENTER, middleLine, 85, this.causeOfDeath)`
        // (`DeathScreen.java:123`) — `middleLine == width / 2`, i.e.
        // `Origin::ScreenTop`, at normal (1.0) scale.
        labels.push(MenuLabel {
            text: text.to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: 85.0,
            align: Align::Centre,
            colour: LABEL,
            scale: 1.0,
        });
    }
    // `output.accept(CENTER, middleLine, 100, this.deathScore)`
    // (`DeathScreen.java:126`) — always drawn, message or not.
    labels.push(MenuLabel {
        text: DEATH_SCORE_UNTRACKED.to_string(),
        origin: Origin::ScreenTop,
        dx: 0.0,
        dy: 100.0,
        align: Align::Centre,
        colour: LABEL,
        scale: 1.0,
    });

    MenuFrame {
        rows: DEATH_BUTTONS
            .iter()
            .map(|b| MenuRow {
                label: b.label().to_string(),
                enabled: true,
                slot: Some(death_slot(*b)),
                ..Default::default()
            })
            .collect(),
        selected: nav.death_index(),
        gui_scale: nav.gui_scale(),
        overlay: true,
        vanilla: true,
        labels,
        ..Default::default()
    }
}

/// Width of one account-screen action button (Add account / Select / Remove
/// / Cancel), in logical pixels. Not vanilla-sourced — see [`Origin::ScreenBottom`].
const ACCOUNTS_BUTTON_W: f32 = 130.0;
/// Horizontal gap between account-screen buttons.
const ACCOUNTS_BUTTON_GAP: f32 = 8.0;
/// Vertical distance from the bottom edge to the account-screen button row,
/// leaving room for the two lines of footer hint text below it.
const ACCOUNTS_BUTTON_BOTTOM: f32 = 74.0;

/// The rect for account-screen button `index` (0..4, see
/// [`super::accounts::BUTTON_ADD`] and its siblings), evenly spaced and
/// centred along the bottom of the screen.
fn accounts_button_slot(index: usize) -> Slot {
    let total_w = 4.0 * ACCOUNTS_BUTTON_W + 3.0 * ACCOUNTS_BUTTON_GAP;
    Slot {
        origin: Origin::ScreenBottom,
        dx: -total_w * 0.5 + index as f32 * (ACCOUNTS_BUTTON_W + ACCOUNTS_BUTTON_GAP),
        dy: -ACCOUNTS_BUTTON_BOTTOM,
        w: ACCOUNTS_BUTTON_W,
        h: WIDGET_H,
    }
}

/// Builds the account list's ordinary (no sign-in in flight) frame: the
/// scrollable account + offline list, then the four action buttons.
///
/// The list rows are unslotted (the centred row stack [`row_rect`] already
/// gives every other row-stack screen); the button row is slotted (real
/// `widget/button*` nine-slice sprites via [`draw_widget`], anchored to the
/// bottom edge independent of how many rows are above it) — see
/// [`row_rect`]'s doc comment for why mixing the two within one frame needed
/// a fix there, not here.
#[must_use]
fn accounts_idle_frame(accounts: &super::accounts::AccountsNav) -> MenuFrame<'static> {
    use super::accounts::{AccountRow, BUTTON_ADD, BUTTON_CANCEL, BUTTON_COUNT, BUTTON_REMOVE, BUTTON_SELECT, VISIBLE_ROWS};

    let all_rows = accounts.rows();
    let list_len = all_rows.len();
    let accounts_len = list_len.saturating_sub(1); // the offline row is always last
    let scroll = accounts.scroll().min(list_len.saturating_sub(1));
    let shown = list_len.saturating_sub(scroll).min(VISIBLE_ROWS);
    let focus = accounts.focus();

    let mut rows: Vec<MenuRow> = all_rows[scroll..scroll + shown]
        .iter()
        .map(|row| match row {
            AccountRow::Account(p) => MenuRow {
                label: p.username.clone(),
                detail: "MICROSOFT ACCOUNT".to_string(),
                trailing: if accounts.is_selected(p.profile_id) {
                    "SELECTED".to_string()
                } else {
                    String::new()
                },
                head: Some(default_head_icon()),
                enabled: true,
                ..Default::default()
            },
            AccountRow::Offline => MenuRow {
                label: "Play Offline".to_string(),
                detail: "NO SIGN-IN REQUIRED".to_string(),
                trailing: if accounts.offline_selected() {
                    "SELECTED".to_string()
                } else {
                    String::new()
                },
                head: Some(default_head_icon()),
                enabled: true,
                ..Default::default()
            },
        })
        .collect();

    // Explicit per-constant pushes, not a loop over a positional array: the
    // button *index* (which drives both the slot position and what
    // `AccountsNav::handle_key` does with it) must never silently drift from
    // its label just because the two lists were reordered independently.
    let button_row = |index: usize, label: &str, enabled: bool| MenuRow {
        label: label.to_string(),
        enabled,
        slot: Some(accounts_button_slot(index)),
        ..Default::default()
    };
    rows.push(button_row(BUTTON_ADD, "ADD ACCOUNT", true));
    rows.push(button_row(BUTTON_SELECT, "SELECT", true));
    rows.push(button_row(BUTTON_REMOVE, "REMOVE", accounts.highlighted() < accounts_len));
    rows.push(button_row(BUTTON_CANCEL, "CANCEL", true));

    let selected = if focus < list_len {
        focus.saturating_sub(scroll)
    } else {
        shown + (focus - list_len).min(BUTTON_COUNT - 1)
    };

    let mut footer = vec!["ENTER SELECT   DEL REMOVE   ESC CANCEL".to_string()];
    if list_len > VISIBLE_ROWS {
        footer.push(format!("SHOWING {}-{} OF {}", scroll + 1, scroll + shown, list_len));
    }

    MenuFrame {
        title: "ACCOUNTS",
        subtitle: if list_len == 1 {
            "NO ACCOUNTS SIGNED IN - ADD ONE, OR PLAY OFFLINE"
        } else {
            ""
        },
        rows,
        selected,
        footer,
        message: accounts.save_error(),
        ..Default::default()
    }
}

/// Builds the account screen's frame while a device-code sign-in is in
/// flight (or has just failed): the code/URL to show, or the failure.
#[must_use]
fn accounts_flow_frame(
    title: &'static str,
    user_code: Option<&str>,
    verification_uri: Option<&str>,
    waiting: bool,
) -> MenuFrame<'static> {
    let mut rows = Vec::new();
    if let Some(uri) = verification_uri {
        rows.push(MenuRow {
            label: uri.to_string(),
            detail: "GO TO THIS ADDRESS IN YOUR BROWSER (OPENED FOR YOU)".to_string(),
            enabled: true,
            ..Default::default()
        });
    }
    if let Some(code) = user_code {
        rows.push(MenuRow {
            label: code.to_string(),
            detail: "THEN ENTER THIS CODE".to_string(),
            enabled: true,
            ..Default::default()
        });
    }
    if rows.is_empty() {
        rows.push(MenuRow {
            label: "CONTACTING MICROSOFT...".to_string(),
            enabled: true,
            ..Default::default()
        });
    }
    let footer = if waiting {
        vec![
            "WAITING FOR YOU TO FINISH SIGNING IN...".to_string(),
            "O REOPEN BROWSER   C COPY CODE   ESC CANCEL".to_string(),
        ]
    } else {
        vec!["ESC CANCEL".to_string()]
    };
    MenuFrame {
        title,
        subtitle: "",
        rows,
        selected: usize::MAX,
        footer,
        ..Default::default()
    }
}

/// Builds the account screen's frame for a failed sign-in attempt.
#[must_use]
fn accounts_failed_frame(message: &str) -> MenuFrame<'static> {
    MenuFrame {
        title: "SIGN-IN FAILED",
        subtitle: "",
        rows: vec![MenuRow {
            label: "BACK TO ACCOUNTS".to_string(),
            enabled: true,
            ..Default::default()
        }],
        selected: 0,
        footer: vec!["ENTER OR ESC CONTINUES".to_string()],
        message: Some(message.to_uppercase()),
        ..Default::default()
    }
}

/// Builds the frame for whichever menu screen `ui` is on.
///
/// This is the single place menu *state* becomes menu *content*, so the app has
/// no per-screen branching and a test can assert what each screen shows without
/// a GPU. Returns `None` for any screen this renderer does not own, which is the
/// app's signal to render the world instead.
#[must_use]
pub fn frame_for<'a>(
    ui: &super::UiState,
    nav: &super::nav::MenuNav,
    statuses: &super::status::StatusCache,
    favicons: &mut FaviconCache,
) -> Option<MenuFrame<'a>> {
    use super::Screen;
    use super::nav::{FormField, MAIN_BUTTONS};
    use super::status::StatusSlot;

    let frame = match ui.screen() {
        // Vanilla's `TitleScreen`: the logo pair, eight widgets at vanilla's
        // rects (see `title_slot`) with four of them present-and-disabled, and
        // the two corner strings. No big "LODESTONE" heading and no key-hint
        // footer — the logo *is* the heading, and vanilla draws no footer.
        Screen::MainMenu => Some(MenuFrame {
            rows: MAIN_BUTTONS
                .iter()
                .map(|b| MenuRow {
                    label: b.label().to_string(),
                    enabled: b.enabled(),
                    slot: Some(title_slot(*b)),
                    icon: b.icon(),
                    ..Default::default()
                })
                .collect(),
            selected: nav.main_index(),
            vanilla: true,
            logo: true,
            labels: vec![
                MenuLabel {
                    text: version_line(),
                    origin: Origin::BottomLeft,
                    dx: 2.0,
                    dy: CORNER_TEXT_Y,
                    align: Align::Left,
                    colour: LABEL,
                    scale: 1.0,
                },
                MenuLabel {
                    text: COPYRIGHT.to_string(),
                    origin: Origin::BottomRight,
                    dx: -2.0,
                    dy: CORNER_TEXT_Y,
                    align: Align::Right,
                    colour: LABEL,
                    scale: 1.0,
                },
            ],
            ..Default::default()
        }),
        Screen::ServerList => {
            let entries = nav.list().entries();
            let rows: Vec<MenuRow> = entries
                .iter()
                .map(|e| {
                    let slot = statuses.get(e);
                    let (detail, is_error) = match slot {
                        StatusSlot::Idle => (e.address_label(), false),
                        StatusSlot::Pending => (format!("{}  PINGING", e.address_label()), false),
                        StatusSlot::Ok(s) => {
                            let motd = s.motd.split('\n').next().unwrap_or("").trim().to_string();
                            if motd.is_empty() {
                                (e.address_label(), false)
                            } else {
                                (motd, false)
                            }
                        }
                        StatusSlot::Failed(why) => (why.clone(), true),
                    };
                    let trailing = match slot {
                        StatusSlot::Ok(s) => match s.latency_ms {
                            Some(ms) => format!("{}  {ms}MS", s.players),
                            None => s.players.clone(),
                        },
                        _ => String::new(),
                    };
                    MenuRow {
                        label: e.name.clone(),
                        detail,
                        trailing,
                        favicon: match slot {
                            StatusSlot::Ok(s) => s
                                .favicon_png
                                .as_deref()
                                .and_then(|png| {
                                    favicons.get(&super::status::StatusCache::key(e), png)
                                }),
                            _ => None,
                        },
                        head: None,
                        enabled: true,
                        detail_is_error: is_error,
                        field: false,
                        edit: None,
                        // The server list is not one of vanilla's own screens
                        // (vanilla's is a scrolling `ObjectSelectionList`), so it
                        // stays on the centred row stack.
                        slot: None,
                        icon: None,
                    }
                })
                .collect();
            let subtitle = if rows.is_empty() {
                "NO SERVERS SAVED - PRESS A TO ADD ONE"
            } else {
                ""
            };
            Some(MenuFrame {
                title: "MULTIPLAYER",
                subtitle,
                rows,
                selected: nav.server_index(),
                footer: vec![
                    "ENTER JOIN   A ADD   E EDIT   D DELETE   R REFRESH".to_string(),
                    "ESC BACK".to_string(),
                ],
                message: nav.save_error().map(str::to_string),
                ..Default::default()
            })
        }
        Screen::ServerEdit => {
            let form = nav.form();
            Some(MenuFrame {
                title: if form.editing.is_some() {
                    "EDIT SERVER"
                } else {
                    "ADD SERVER"
                },
                subtitle: "",
                // `edit` carries a **clone of the live widget**, which is how
                // #395's persistent `EditBox` reaches a draw through a `&MenuNav`
                // frame builder: `build`'s `draw_edit_box` moves the clone into
                // this frame's rect and asks it where the text, caret and
                // selection go. `label` stays populated because it is what
                // `the_edit_form_shows_both_fields_and_marks_the_focused_one` and
                // every other frame-shape test read; nothing draws it now.
                rows: vec![
                    MenuRow {
                        label: form.name().to_string(),
                        detail: "NAME".to_string(),
                        enabled: true,
                        field: true,
                        edit: Some(form.fields.name.clone()),
                        ..Default::default()
                    },
                    MenuRow {
                        label: form.address().to_string(),
                        detail: "ADDRESS - HOST OR HOST:PORT".to_string(),
                        enabled: true,
                        field: true,
                        edit: Some(form.fields.address.clone()),
                        ..Default::default()
                    },
                ],
                selected: match form.field() {
                    FormField::Name => 0,
                    FormField::Address => 1,
                },
                footer: vec![
                    "TAB SWITCH FIELD   ENTER SAVE   ESC CANCEL".to_string(),
                    "AN EMPTY NAME USES THE HOST - AN EMPTY PORT ALLOWS SRV".to_string(),
                ],
                message: (!form.is_valid()).then(|| "AN ADDRESS IS REQUIRED".to_string()),
                ..Default::default()
            })
        }
        // Vanilla's `SelectWorldScreen` (issue #397): the title, the search box,
        // the six footer buttons — **five of them present and disabled**, Create
        // New World among them — and the one list row the list actually has. See
        // `super::world_select` for what is disabled and why, and
        // `world_select_slot` for the geometry.
        Screen::WorldSelect => {
            use super::world_select::WORLD_SELECT_BUTTONS;
            let ws = nav.world_select();
            let mut rows = Vec::with_capacity(1 + WORLD_SELECT_BUTTONS.len());
            rows.push(MenuRow {
                // Not drawn: `draw_edit_box` reads the widget. Populated for the
                // same reason the edit form's is — the frame-shape tests read it.
                label: ws.search().value().to_string(),
                enabled: true,
                field: true,
                edit: Some(ws.search().clone()),
                slot: Some(world_select_search_slot()),
                ..Default::default()
            });
            for button in WORLD_SELECT_BUTTONS {
                rows.push(MenuRow {
                    label: button.label().to_string(),
                    // The **widget's** live flag, not `WorldSelectButton::enabled`
                    // — see `WorldSelectNav::is_active` on why asking the enum here
                    // would be a second source of truth.
                    enabled: ws.is_active(button.row()),
                    slot: Some(world_select_slot(button)),
                    ..Default::default()
                });
            }
            Some(MenuFrame {
                rows,
                // The *focused* row. `usize::MAX` when nothing is focused, which
                // highlights nothing (see `MenuFrame::selected`) — rather than
                // `0`, which would light the search field up whenever focus was
                // cleared.
                selected: ws.focused_row().unwrap_or(usize::MAX),
                hovered: ws.hovered(),
                vanilla: true,
                labels: vec![
                    world_select_title_label(),
                    // The empty-list state, drawn rather than implied: without it
                    // "no worlds" and "the list failed to draw" are the same
                    // picture.
                    world_select_no_worlds_label(),
                ],
                ..Default::default()
            })
        }
        Screen::Settings => {
            let scale = nav.gui_scale();
            let label = if scale == crate::config::AUTO_GUI_SCALE {
                "GUI SCALE: AUTO".to_string()
            } else {
                format!("GUI SCALE: {scale}")
            };
            Some(MenuFrame {
                title: "OPTIONS",
                subtitle: "",
                rows: vec![
                    MenuRow {
                        label,
                        detail: "UP/DOWN CHANGES IT - AUTO FITS THE WINDOW".to_string(),
                        enabled: true,
                        ..Default::default()
                    },
                    // Vanilla's View Bobbing (`options.viewBobbing`). `selected:
                    // 0` below still points at the scale row and this screen has
                    // no cursor, so being second costs nothing — see
                    // `MenuNav::key_settings` on why each control owns a key
                    // rather than sharing a highlight.
                    MenuRow {
                        label: format!(
                            "VIEW BOBBING: {}",
                            if nav.view_bobbing() { "ON" } else { "OFF" }
                        ),
                        detail: "ENTER TOGGLES IT - THE CAMERA MOVES WITH YOUR STRIDE"
                            .to_string(),
                        enabled: true,
                        ..Default::default()
                    },
                ],
                selected: 0,
                footer: vec!["UP/DOWN SCALE   ENTER VIEW BOBBING   ESC BACK".to_string()],
                message: nav.options_save_error().map(str::to_string),
                ..Default::default()
            })
        }
        // The account list (issue #66). `pump` is called here, on every
        // frame this screen is showing, rather than from an `app.rs` hook —
        // see `accounts.rs`'s module docs on why that module is written to
        // work through a shared `&AccountsNav` reference.
        Screen::Accounts => {
            use super::accounts::SignInView;
            let accounts = nav.accounts();
            accounts.pump();
            Some(match accounts.sign_in_view() {
                SignInView::Idle => accounts_idle_frame(accounts),
                SignInView::Requesting => accounts_flow_frame("SIGN IN WITH MICROSOFT", None, None, false),
                SignInView::Waiting {
                    user_code,
                    verification_uri,
                } => accounts_flow_frame(
                    "SIGN IN WITH MICROSOFT",
                    Some(&user_code),
                    Some(&verification_uri),
                    true,
                ),
                SignInView::Failed { message } => accounts_failed_frame(&message),
            })
        }
        // The error screen is drawn by this renderer too, even though it is not
        // an `is_menu()` screen: a session that dies mid-game used to leave a
        // frozen world on screen with no explanation. `Screen::Connecting` is
        // deliberately *not* here — it keeps rendering the world so chunks mesh
        // and upload as they stream in, rather than piling up behind a loading
        // screen and landing as one spike at login.
        Screen::Error => Some(MenuFrame {
            title: "DISCONNECTED",
            subtitle: "",
            rows: vec![MenuRow {
                label: "BACK TO MENU".to_string(),
                enabled: true,
                ..Default::default()
            }],
            selected: 0,
            footer: vec!["ENTER OR ESC RETURNS TO THE MENU".to_string()],
            message: ui.error().map(|e| e.to_uppercase()),
            ..Default::default()
        }),
        _ => None,
    };
    // Stamped on every screen (not read back out of `nav` per-screen above) so
    // the whole menu scales, not only the settings screen that edits the
    // setting.
    frame.map(|mut f| {
        f.gui_scale = nav.gui_scale();
        f
    })
}

/// The logical canvas size [`geometry`] should lay its fixed pixel constants
/// into, given the real framebuffer size in physical pixels and the user's
/// `gui_scale` option (`0` = auto). This is the one function that fixes the
/// "menu draws half-size on Retina" report: it divides the framebuffer by the
/// effective integer [`crate::config::calculate_gui_scale`], exactly vanilla's
/// `Window.guiScaledWidth`/`Height` — so a fixed `ROW_W` comes out the same
/// *visual* size at any DPI, rather than shrinking as the physical framebuffer
/// grows. At `scale == 1` this is the identity (canvas == framebuffer), which
/// is why every `geometry`-calling test below, which passes a fixed size
/// directly, is unaffected by this existing.
#[must_use]
pub fn logical_canvas(gui_scale: u32, framebuffer_width: u32, framebuffer_height: u32) -> (f32, f32) {
    let scale = crate::config::calculate_gui_scale(gui_scale, framebuffer_width, framebuffer_height).max(1);
    (
        framebuffer_width as f32 / scale as f32,
        framebuffer_height as f32 / scale as f32,
    )
}

/// Per-glyph horizontal advance at `scale` (fixed advance: cell plus one column).
fn advance(scale: f32) -> f32 {
    (GLYPH_W as f32 + 1.0) * scale
}

/// Pixel width of `s` at `scale`.
#[must_use]
pub fn text_px(s: &str, scale: f32) -> f32 {
    s.chars().count() as f32 * advance(scale)
}

/// Truncates `s` so it fits in `max_px` at `scale`, appending nothing (the font
/// has no ellipsis glyph). Returns a slice of `s`.
fn clip(s: &str, max_px: f32, scale: f32) -> &str {
    let fits = (max_px / advance(scale)).floor().max(0.0) as usize;
    match s.char_indices().nth(fits) {
        Some((i, _)) => &s[..i],
        None => s,
    }
}

/// Height of the row stack for `rows`.
fn row_height(row: &MenuRow) -> f32 {
    if row.favicon.is_some() || row.head.is_some() || !row.detail.is_empty() {
        LIST_ROW_H
    } else {
        BUTTON_H
    }
}

/// The pixel rect of row `i`, given the viewport. Public so the renderer, the
/// mouse hover and the click hit-test share one definition of where a row
/// actually is — `app.rs`'s `menu_row_at` calls exactly this.
///
/// A row carrying a [`MenuRow::slot`] is placed by vanilla's arithmetic; every
/// other row falls through to the centred stack. Keeping both behind this one
/// signature is deliberate: it is what let the two vanilla screens change layout
/// entirely without `app.rs`'s hit-test changing at all, so keyboard selection,
/// hover and clicks could not drift apart from the draw.
#[must_use]
pub fn row_rect(rows: &[MenuRow], i: usize, width: f32, height: f32) -> Option<(f32, f32, f32, f32)> {
    let row = rows.get(i)?;
    if let Some(slot) = row.slot {
        return Some(slot.resolve(width, height));
    }
    // Only the *other* unslotted rows count toward the centred stack's total
    // height and this row's offset within it. Without this filter, a frame
    // that mixes centred rows with vanilla-positioned ones (the account
    // list's scrollable rows plus its slotted action buttons) would have
    // every slotted row's height silently added into the centred group's
    // math even though that row is drawn somewhere else entirely — no
    // existing screen mixes the two kinds, so this could not be observed
    // before the account screen needed it.
    let total: f32 = rows
        .iter()
        .filter(|r| r.slot.is_none())
        .map(|r| row_height(r) + ROW_GAP)
        .sum::<f32>()
        .max(0.0)
        - ROW_GAP;
    // Centred vertically, but never above the title block.
    let top = ((height - total) * 0.5).max(110.0);
    let y = top
        + rows[..i]
            .iter()
            .filter(|r| r.slot.is_none())
            .map(|r| row_height(r) + ROW_GAP)
            .sum::<f32>();
    let w = ROW_W.min(width - 2.0 * PAD);
    Some(((width - w) * 0.5, y, w, row_height(row)))
}

/// An [`EditBox`] row's **box** height: vanilla's own 20
/// (`EditBox.java:61-63`, `Button.DEFAULT_HEIGHT`), taken off the *top* of the
/// 40 px [`LIST_ROW_H`] the row occupies so its `detail` hint still fits
/// underneath.
///
/// 20 rather than the whole row is not an arbitrary choice: it puts
/// [`EditBox::text_y`] — `y + floor((h - 8) / 2)` — at `y + 6`, which is exactly
/// where the pre-#395 draw put the label (`y + PAD`). The conversion therefore
/// leaves the *text* where it was and only changes the background from a flat
/// fill to vanilla's real `widget/text_field` nine-slice.
pub const EDIT_BOX_H: f32 = 20.0;

/// The rect an [`EditBox`] row's box occupies, as distinct from the whole row's.
///
/// Derived from [`row_rect`] rather than restated, so the field, the row fill,
/// `app.rs`'s hit-test and [`super::nav::EditForm`]'s seed geometry cannot drift
/// apart — `CLAUDE.md`'s "derive layout from the same expression the draw uses".
#[must_use]
pub fn field_rect(
    rows: &[MenuRow],
    i: usize,
    width: f32,
    height: f32,
) -> Option<(f32, f32, f32, f32)> {
    let (x, y, w, _) = row_rect(rows, i, width, height)?;
    Some((x, y, w, EDIT_BOX_H))
}

/// The two [`super::Screen::ServerEdit`] field rects at a given canvas, through
/// [`field_rect`].
///
/// Exists so [`super::nav::EditForm::adding`] can seed its two `EditBox`es'
/// geometry from the layout the draw actually uses instead of hardcoding a width
/// — the boxes need real bounds *before* any frame exists, because arrow
/// navigation between them is geometric and `displayPos` scrolling is measured
/// against the width. Both probe rows carry a non-empty `detail`, which is what
/// makes [`row_height`] give them [`LIST_ROW_H`]; a blank one would be 30 px and
/// the seed would be a different rect from the draw.
#[must_use]
pub fn field_row_rects(width: f32, height: f32) -> [(f32, f32, f32, f32); 2] {
    let rows = [
        MenuRow {
            detail: "NAME".to_string(),
            field: true,
            ..Default::default()
        },
        MenuRow {
            detail: "ADDRESS".to_string(),
            field: true,
            ..Default::default()
        },
    ];
    [
        field_rect(&rows, 0, width, height).unwrap_or_default(),
        field_rect(&rows, 1, width, height).unwrap_or_default(),
    ]
}

/// Both vertex streams one menu frame produces.
///
/// Two streams because the buttons are **textured** (nine-slice sprites off the
/// GUI atlas) while everything else — backdrops, row fills, text — is a flat
/// coloured quad. They need different pipelines, so they cannot share a buffer.
///
/// `backdrop_floats` is the split the caller draws the sprite stream *between*:
/// the full-screen backdrop first, then every sprite, then the rest of the
/// colour stream. That ordering is load-bearing — a button's label is on the
/// colour stream, so drawing all colour before all sprites would bury every
/// label under the button it belongs to.
#[derive(Debug, Clone, Default)]
pub struct MenuGeometry {
    /// Interleaved `[x, y, r, g, b, a]` in NDC, two triangles per quad.
    pub colour: Vec<f32>,
    /// How many floats at the head of [`Self::colour`] are the full-screen
    /// backdrop quad.
    pub backdrop_floats: usize,
    /// Interleaved `[x, y, u, v, r, g, b, a]` in NDC + atlas UVs.
    pub sprite: Vec<f32>,
}

/// Builds the coloured-quad stream for one menu frame with no atlas and no
/// vanilla font — the jar-less path, and the shape every layout test uses.
///
/// Pure: no GPU, no state. Returns interleaved `[x, y, r, g, b, a]` in NDC.
#[must_use]
pub fn geometry(frame: &MenuFrame<'_>, width: f32, height: f32) -> Vec<f32> {
    build(frame, None, None, width, height).colour
}

/// Builds both vertex streams for one menu frame.
///
/// `atlas` supplies the real `widget/button*` nine-slice sprites, the icon
/// buttons' sprites and the title logo; `font` supplies vanilla's proportional
/// text. Both are `Option` and both degrade the same way every other vanilla
/// asset does in this crate: flat coloured button fills and the fixed-advance
/// 5×7 debug font, which is what a jar-less or headless run gets. Pure — no GPU.
#[must_use]
pub fn build(
    frame: &MenuFrame<'_>,
    atlas: Option<&GuiAtlas>,
    font: Option<&VanillaFont>,
    width: f32,
    height: f32,
) -> MenuGeometry {
    let mut b = Quads::new(width, height);
    b.atlas = atlas;
    b.font = font;
    let backdrop = if frame.overlay { OVERLAY_BG } else { BG };
    b.rect(0.0, 0.0, width, height, backdrop);
    let backdrop_floats = b.verts.len();

    if frame.logo {
        // Vanilla's `LogoRenderer`: the wordmark centred at y=30, the edition
        // strip centred under it overlapping by 7 px.
        b.sprite("title/minecraft", (width * 0.5).floor() - 128.0, LOGO_Y, LOGO_W, LOGO_H, LABEL);
        b.sprite(
            "title/edition",
            (width * 0.5).floor() - 64.0,
            EDITION_Y,
            EDITION_W,
            EDITION_H,
            LABEL,
        );
    }

    if frame.vanilla {
        for label in &frame.labels {
            let (ax, ay) = label.origin.anchor(width, height);
            let tw = b.text_width(&label.text, label.scale);
            let x = match label.align {
                Align::Left => ax + label.dx,
                Align::Centre => (ax + label.dx - tw * 0.5).floor(),
                Align::Right => ax + label.dx - tw,
            };
            b.text(&label.text, x, ay + label.dy, label.scale, label.colour);
        }
    } else {
        // The row-stack screens' own centred title block.
        let tw = text_px(frame.title, TITLE_SCALE);
        b.text(frame.title, (width - tw) * 0.5, 40.0, TITLE_SCALE, FG);
        if !frame.subtitle.is_empty() {
            let sw = text_px(frame.subtitle, TEXT_SCALE);
            b.text(
                frame.subtitle,
                (width - sw) * 0.5,
                40.0 + GLYPH_H as f32 * TITLE_SCALE + 8.0,
                TEXT_SCALE,
                FG_DIM,
            );
        }
    }

    for (i, row) in frame.rows.iter().enumerate() {
        if row.slot.is_some() {
            // A vanilla-positioned row can be a **text field** rather than a
            // button: `Screen::WorldSelect`'s search box is placed by the header
            // layout's arithmetic like every other widget on that screen, and
            // drawn as an `EditBox`. Checked before `draw_widget` because the two
            // draws are mutually exclusive — a field is not a button with text in
            // it, and `EditBox` has its own sprite set and its own predicate (see
            // `draw_edit_box`).
            if let Some(edit) = row.edit.as_ref() {
                if let Some((x, y, w, h)) = row_rect(&frame.rows, i, width, height) {
                    draw_edit_box(&mut b, edit, x, y, w, h);
                }
                continue;
            }
            draw_widget(
                &mut b,
                &frame.rows,
                i,
                width,
                height,
                i == frame.selected,
                frame.hovered == Some(i),
            );
            continue;
        }
        let Some((x, y, w, h)) = row_rect(&frame.rows, i, width, height) else {
            continue;
        };
        let selected = i == frame.selected;
        // A row carrying a live `EditBox` (#395) draws through the widget: it
        // owns the caret, the selection and the horizontal scroll, none of which
        // are derivable from a `MenuRow`. Its `detail` hint still draws
        // underneath, at the same offset the pre-widget path used.
        if let Some(edit) = row.edit.as_ref() {
            let (fx, fy, fw, fh) =
                field_rect(&frame.rows, i, width, height).unwrap_or((x, y, w, EDIT_BOX_H));
            draw_edit_box(&mut b, edit, fx, fy, fw, fh);
            if !row.detail.is_empty() {
                let room = (fw - 2.0 * PAD).max(0.0);
                let detail = clip(&row.detail, room, SMALL_SCALE);
                let colour = if row.detail_is_error { FG_BAD } else { FG_DIM };
                b.text(
                    detail,
                    fx + PAD,
                    fy + fh + 3.0,
                    SMALL_SCALE,
                    colour,
                );
            }
            continue;
        }
        let fill = if row.field {
            FIELD_BG
        } else if selected {
            ROW_SEL
        } else if row.enabled {
            ROW_BG
        } else {
            ROW_OFF
        };
        b.rect(x, y, w, h, fill);
        // A 2 px selection border, so the highlight survives a screenshot even
        // where the fill difference is subtle.
        if selected {
            b.outline(x, y, w, h, 2.0, FG);
        }

        let mut text_x = x + PAD;
        if let Some(icon) = row.favicon.as_ref().or(row.head.as_ref()) {
            let iy = y + (h - ICON) * 0.5;
            b.mosaic(icon, text_x, iy, ICON);
            text_x += ICON + PAD;
        }
        let label_room = (x + w - PAD) - text_x - text_px(&row.trailing, SMALL_SCALE) - PAD;
        let label = clip(&row.label, label_room.max(0.0), TEXT_SCALE);
        let label_y = if row.detail.is_empty() {
            y + (h - GLYPH_H as f32 * TEXT_SCALE) * 0.5
        } else {
            y + PAD
        };
        b.text(label, text_x, label_y, TEXT_SCALE, FG);
        if row.field && selected {
            // Caret: a solid block one advance past the text.
            b.rect(
                text_x + text_px(label, TEXT_SCALE),
                label_y,
                TEXT_SCALE,
                GLYPH_H as f32 * TEXT_SCALE,
                FG,
            );
        }
        if !row.detail.is_empty() {
            let dy = label_y + GLYPH_H as f32 * TEXT_SCALE + 3.0;
            let detail = clip(&row.detail, label_room.max(0.0), SMALL_SCALE);
            let colour = if row.detail_is_error { FG_BAD } else { FG_DIM };
            b.text(detail, text_x, dy, SMALL_SCALE, colour);
        }
        if !row.trailing.is_empty() {
            let tx = x + w - PAD - text_px(&row.trailing, SMALL_SCALE);
            b.text(
                &row.trailing,
                tx,
                y + (h - GLYPH_H as f32 * SMALL_SCALE) * 0.5,
                SMALL_SCALE,
                FG_DIM,
            );
        }
    }

    // Message and footer, bottom-up. Not on a vanilla screen: vanilla has no
    // key-hint footer, and reproducing its layout means reproducing what it
    // does *not* draw as well.
    if !frame.vanilla {
        let mut fy = height - 12.0 - GLYPH_H as f32 * SMALL_SCALE;
        for line in frame.footer.iter().rev() {
            let lw = text_px(line, SMALL_SCALE);
            b.text(line, (width - lw) * 0.5, fy, SMALL_SCALE, FG_DIM);
            fy -= GLYPH_H as f32 * SMALL_SCALE + 4.0;
        }
        if let Some(msg) = &frame.message {
            let mw = text_px(msg, TEXT_SCALE);
            b.text(
                msg,
                (width - mw) * 0.5,
                fy - GLYPH_H as f32 * TEXT_SCALE,
                TEXT_SCALE,
                FG_BAD,
            );
        }
    }

    MenuGeometry {
        colour: b.verts,
        backdrop_floats,
        sprite: b.sprites,
    }
}

/// Draws one vanilla widget: its `widget/button*` nine-slice background, then
/// either its centred label or its centred 15×15 icon sprite.
///
/// **This is [`widget::Widget`]'s consumer.** Nothing about which sprite or which
/// label colour a state produces is decided here any more — a [`Widget`] is built
/// from the row's own `enabled`/`selected` and then *asked*
/// ([`Widget::background_sprite`], [`Widget::message_colour`]), so the title
/// screen, the pause menu, the death screen and the account screen's action row
/// share one copy of vanilla's rules instead of a three-way `if` per screen. That
/// is the whole point of #393: the fourth screen must not write the blit a fourth
/// time.
///
/// The rect still comes from [`row_rect`] rather than from the widget, and
/// deliberately: that function is also `app.rs`'s hit-test, so it stays the single
/// definition of where a row is until #394 gives the layout containers somewhere
/// to write positions *to*.
///
/// Mirrors `AbstractButton.extractDefaultSprite` +
/// `Button.Plain.extractContents` (`AbstractButton.java:43-53`,
/// `Button.java:128-132`) and, for icons,
/// `SpriteIconButton.CenteredIcon.extractContents`
/// (`SpriteIconButton.java:236-244`).
fn draw_widget(
    b: &mut Quads<'_>,
    rows: &[MenuRow],
    i: usize,
    width: f32,
    height: f32,
    selected: bool,
    hovered: bool,
) {
    let Some(row) = rows.get(i) else { return };
    let Some((x, y, w, h)) = row_rect(rows, i, width, height) else {
        return;
    };
    // One widget, carrying this row's state. `focused` takes `selected`; on the
    // title screen, the pause menu, the death screen and the account screen
    // `hovered` is always `false`, because those still have a *single* row cursor
    // that both the keyboard and `MenuNav::hover` move — there is no second fact
    // to record. #395 split the two flags on `Widget` for the screens that do have
    // real focus (`Screen::ServerEdit`'s fields), and #397's `Screen::WorldSelect`
    // is the first screen to carry *both* through a frame, via
    // `MenuFrame::hovered`. Vanilla's sprite argument is `isHoveredOrFocused()`,
    // and that `||` lives in `Widget::is_hovered_or_focused`; do not re-derive it
    // in this function.
    //
    // Built per frame, so the message is copied per frame. That is the same cost
    // the row itself already pays — `frame_for` and `pause_frame` both rebuild
    // every `MenuRow` with a fresh `label.to_string()` every frame — and a menu
    // screen draws nine of these with no world behind it, so it is not worth a
    // lifetime parameter on `Widget` to avoid.
    let mut widget = Widget::button(x, y, w, h, row.label.as_str());
    widget.active = row.enabled;
    widget.focused = selected;
    widget.hovered = hovered;
    widget.icon = row.icon;
    // `AbstractWidget.extractRenderState` wraps everything in `if (this.visible)`
    // (`AbstractWidget.java:56-62`). No row sets this yet; the guard is here so
    // that the day one does, it does not have to be remembered.
    if !widget.visible {
        return;
    }

    // `WidgetSprites::get(active, hoveredOrFocused)` (`WidgetSprites.java:18-24`)
    // with `AbstractButton`'s three-argument sprite set: disabled wins over
    // hovered, which is why a greyed-out button under the cursor still looks
    // greyed out. The rule lives in `menu::widget`; this only asks.
    match widget.background_sprite() {
        Some(sprite) if b.has_sprite(sprite) => b.sprite(sprite, x, y, w, h, LABEL),
        _ => {
            // Jar-less fallback: the flat fills the menu has always used, so the
            // layout is still legible and still testable without a pack. The
            // predicate is the widget's `||`, not `focused` alone, so the fallback
            // cannot disagree with the sprite path above about which button is
            // lit — identical for every screen with a row cursor, since `hovered`
            // is false there.
            let fill = if !widget.active {
                ROW_OFF
            } else if widget.is_hovered_or_focused() {
                ROW_SEL
            } else {
                ROW_BG
            };
            b.rect(x, y, w, h, fill);
            if widget.is_hovered_or_focused() {
                b.outline(x, y, w, h, 1.0, FG);
            }
        }
    }

    if let Some(icon) = widget.icon {
        // `spriteOffset` is zero at every call site, so this is a plain centre.
        let (ix, iy) = widget.icon_rect(ICON_SPRITE);
        b.sprite(icon, ix, iy, ICON_SPRITE, ICON_SPRITE, ICON_TINT);
        return;
    }

    let colour = widget.message_colour();
    // `extractScrollingStringOverContents(output, message, 2)` →
    // `acceptScrollingWithDefaultCenter(msg, x+2, x+w-2, y, y+h)`
    // (`AbstractButton.java:39-41`, `AbstractWidget.java:92-98`), whose centre
    // is `(left + right) / 2` and whose top is
    // `(top + bottom - lineHeight) / 2 + 1` (`ActiveTextCollector.java:59,73`).
    let (left, right) = widget.content_span();
    let tw = b.text_width(&widget.message, 1.0);
    let label = if tw > right - left {
        // Vanilla scrolls an over-long label; we clip, which is the same static
        // frame a scroll happens to be showing at t=0.
        clip_measured(b, &widget.message, right - left)
    } else {
        widget.message.as_str()
    };
    let tw = b.text_width(label, 1.0);
    let tx = ((left + right) * 0.5 - tw * 0.5).floor();
    let ty = widget.label_top(LINE_H);
    b.text(label, tx, ty, 1.0, colour);
}

/// Draws one [`EditBox`]: its `widget/text_field` background, the selection
/// block, the text either side of the caret, and the caret.
///
/// **This is `EditBox`'s draw consumer, and it decides nothing.** Every offset
/// comes from [`EditBox::draw_state`], every colour from
/// [`EditBox::text_colour`], and the sprite id from
/// [`EditBox::background_sprite`] — which is `SPRITES.get(isActive(),
/// isFocused())`, *not* the button's `isHoveredOrFocused()`, so hovering a field
/// deliberately does not highlight it. Mirrors
/// `EditBox.extractWidgetRenderState` (`EditBox.java:404-473`).
///
/// ## The reposition, and why it is on a clone
///
/// The widget lives in [`super::nav::EditForm`] and outlives the frame;
/// `frame_for` takes `&MenuNav`, so the row carries a *copy* and this function
/// moves the copy into `(x, y, w, h)` before reading it. That is
/// `OptionsSubScreen.init`'s build → reposition order (`:28-34`) rather than
/// `PauseScreen`'s build → arrange, which is the switch #394 predicted would
/// happen "once a widget holds state". The seeded geometry in `EditForm` is what
/// the *input* side measures against; see [`field_row_rects`].
///
/// ## Two deliberate departures from the jar
///
/// - **The caret and the selection are 14 px tall, not 11.** Vanilla's are
///   `9`/`9 + 1` because its font is 9 px; this shell draws menu text at
///   [`TEXT_SCALE`] `2.0`, so a glyph is `GLYPH_H * 2 = 14` tall and an 11 px
///   caret would sit visibly short of the text it marks. The *horizontal*
///   arithmetic is already in scale-2 units inside the widget (see
///   `edit_box::MENU_TEXT_ADVANCE`); this is the vertical half of the same
///   consistency.
/// - **The append caret is a bar, not an `_` glyph.** `extractAppendCursor`
///   draws the underscore character (`TextCursorUtils.java:16-18`), and the
///   jar-less fallback font here has no guaranteed `_`. Drawing a baseline bar
///   keeps the insert/append distinction visible without depending on a glyph
///   that may not exist, and the distinction itself
///   ([`edit_box::EditBoxDraw::insert_cursor`]) is asserted in `edit_box`'s own
///   tests.
fn draw_edit_box(b: &mut Quads<'_>, edit: &EditBox, x: f32, y: f32, w: f32, h: f32) {
    let mut edit = edit.clone();
    edit.widget.x = x;
    edit.widget.y = y;
    edit.widget.width = w;
    edit.widget.height = h;
    // `AbstractWidget.extractRenderState`'s `if (this.visible)`
    // (`AbstractWidget.java:56-62`).
    if !edit.widget.visible {
        return;
    }

    match edit.background_sprite() {
        Some(sprite) if b.has_sprite(sprite) => b.sprite(sprite, x, y, w, h, LABEL),
        // Jar-less fallback: the flat field fill the form has always used, plus
        // a border when focused so the focused field is still identifiable in a
        // headless screenshot.
        _ => {
            b.rect(x, y, w, h, FIELD_BG);
            if edit.widget.focused {
                b.outline(x, y, w, h, 2.0, FG);
            }
        }
    }

    let state = edit.draw_state(None);
    let colour = edit.text_colour();
    let glyph_h = GLYPH_H as f32 * TEXT_SCALE;

    // Selection first, so the glyphs land on top of it. Vanilla inverts the text
    // under the block (`graphics.textHighlight(.., invertHighlightedTextColor)`);
    // a flat fill is the equivalent this pipeline can draw, and it is the same
    // colour the row cursor uses elsewhere.
    if let Some((from, to)) = state.highlight {
        let (lo, hi) = if from <= to { (from, to) } else { (to, from) };
        if hi > lo {
            b.rect(lo, state.text_y, hi - lo, glyph_h, ROW_SEL);
        }
    }
    if !state.before.is_empty() {
        b.text(&state.before, state.before_x, state.text_y, TEXT_SCALE, colour);
    }
    if !state.after.is_empty() {
        b.text(&state.after, state.after_x, state.text_y, TEXT_SCALE, colour);
    }
    if state.show_cursor {
        if state.insert_cursor {
            // `extractInsertCursor`: a 1 px bar, widened to `TEXT_SCALE` here for
            // the same reason the height is scaled.
            b.rect(state.cursor_x, state.text_y, TEXT_SCALE, glyph_h, colour);
        } else {
            b.rect(
                state.cursor_x,
                state.text_y + glyph_h - TEXT_SCALE,
                edit.advance,
                TEXT_SCALE,
                colour,
            );
        }
    }
    // The hint (`EditBox.hint`) draws only when the box is empty *and*
    // unfocused (`EditBox.java:438-440`), which is the opposite of a placeholder
    // that vanishes on the first keystroke.
    if let Some(hint) = edit.hint.as_deref() {
        if state.before.is_empty() && state.after.is_empty() && !edit.widget.focused {
            let room = (w - 2.0 * edit_box::BORDER_INSET).max(0.0);
            b.text(
                clip(hint, room, TEXT_SCALE),
                state.before_x,
                state.text_y,
                TEXT_SCALE,
                colour,
            );
        }
    }
}

/// Longest prefix of `s` that measures at most `max_px` in whatever font `b`
/// draws with. Separate from [`clip`] because that one assumes the fixed
/// advance; measurement and drawing must read the same font (see
/// `docs/vanilla-hud-text.md`).
fn clip_measured<'s>(b: &Quads<'_>, s: &'s str, max_px: f32) -> &'s str {
    let mut fits = 0;
    for (i, ch) in s.char_indices() {
        let end = i + ch.len_utf8();
        if b.text_width(&s[..end], 1.0) > max_px {
            return &s[..fits];
        }
        fits = end;
    }
    s
}

/// A pixel-space quad emitter to NDC for both streams.
///
/// Self-contained for the colour stream (mirrors [`crate::effects`]'s builder)
/// but it *does* borrow two `pub(crate)` HUD types now — `ColourStream` and
/// `push_sprite_quad` — rather than re-deriving their NDC arithmetic. Both are
/// read-only borrows of `hud/item_icon.rs`, which needed no change: the point of
/// not folding this renderer into the HUD's pass stands, while duplicating the
/// vertex maths would be a second definition that could silently drift.
struct Quads<'a> {
    w: f32,
    h: f32,
    verts: Vec<f32>,
    sprites: Vec<f32>,
    atlas: Option<&'a GuiAtlas>,
    font: Option<&'a VanillaFont>,
}

impl Quads<'_> {
    fn new(w: f32, h: f32) -> Self {
        Self {
            w,
            h,
            verts: Vec::new(),
            sprites: Vec::new(),
            atlas: None,
            font: None,
        }
    }

    /// Whether the bound atlas can draw `id` at all. Distinct from "did the
    /// draw emit anything", which is what makes the jar-less fallback a choice
    /// rather than a silent nothing.
    fn has_sprite(&self, id: &str) -> bool {
        self.atlas.is_some_and(|a| a.contains(id))
    }

    /// Emit a GUI sprite scaled into `(x, y, w, h)`, honouring its `.mcmeta`
    /// scaling — nine-slice borders included, read from the pack by
    /// [`GuiAtlas::geometry`]. A no-op with no atlas or an unknown id.
    fn sprite(&mut self, id: &str, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        let quads: Vec<GuiSpriteQuad> = match self.atlas {
            Some(a) => a.geometry(id, x, y, w, h),
            None => return,
        };
        for q in quads {
            push_sprite_quad(&mut self.sprites, self.w, self.h, q, c);
        }
    }

    /// Width of `s` in the font this builder will actually *draw* with — the
    /// proportional vanilla one when attached, the fixed 5×7 advance otherwise.
    fn text_width(&self, s: &str, scale: f32) -> f32 {
        match self.font {
            Some(f) => f.width(s, scale),
            None => text_px(s, scale),
        }
    }

    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        if w <= 0.0 || h <= 0.0 {
            return;
        }
        let to_ndc = |px: f32, py: f32| (2.0 * px / self.w - 1.0, 1.0 - 2.0 * py / self.h);
        let (x0, y0) = to_ndc(x, y);
        let (x1, y1) = to_ndc(x + w, y + h);
        let mut v = |vx: f32, vy: f32| {
            self.verts
                .extend_from_slice(&[vx, vy, c[0], c[1], c[2], c[3]]);
        };
        v(x0, y0);
        v(x1, y0);
        v(x1, y1);
        v(x0, y0);
        v(x1, y1);
        v(x0, y1);
    }

    /// Four thin rects forming a border just inside `(x, y, w, h)`.
    fn outline(&mut self, x: f32, y: f32, w: f32, h: f32, t: f32, c: [f32; 4]) {
        self.rect(x, y, w, t, c);
        self.rect(x, y + h - t, w, t, c);
        self.rect(x, y, t, h, c);
        self.rect(x + w - t, y, t, h, c);
    }

    /// One string at `(x, y)` (top-left of the first glyph).
    ///
    /// With a [`VanillaFont`] attached this is vanilla text — real glyphs, real
    /// advances, and the 1 px 25 %-brightness drop shadow — through the exact
    /// same code path the HUD uses. Without one it is the fixed-advance 5×7
    /// debug bitmap, unshadowed, as before. Measurement goes through
    /// [`Quads::text_width`], which picks whichever of the two this will draw
    /// with, so a centred label can never be laid out against the other font.
    fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        if let Some(f) = self.font {
            let (w, h) = (self.w, self.h);
            f.draw(
                &mut ColourStream {
                    verts: &mut self.verts,
                    w,
                    h,
                },
                s,
                x,
                y,
                scale,
                c,
            );
            return;
        }
        let mut cursor = x;
        for ch in s.chars() {
            if ch != ' ' {
                for (ry, row) in glyph_rows(ch).iter().enumerate() {
                    for rx in 0..GLYPH_W {
                        if (row >> (GLYPH_W - 1 - rx)) & 1 == 1 {
                            self.rect(
                                cursor + rx as f32 * scale,
                                y + ry as f32 * scale,
                                scale,
                                scale,
                                c,
                            );
                        }
                    }
                }
            }
            cursor += advance(scale);
        }
    }

    /// A favicon mosaic as a `side`×`side` px square of coloured cells.
    fn mosaic(&mut self, m: &FaviconMosaic, x: f32, y: f32, side: f32) {
        if m.size == 0 {
            return;
        }
        let cell = side / m.size as f32;
        for (i, c) in m.cells.iter().enumerate() {
            if c[3] <= 0.0 {
                continue;
            }
            let cx = (i % m.size) as f32;
            let cy = (i / m.size) as f32;
            self.rect(x + cx * cell, y + cy * cell, cell, cell, *c);
        }
    }
}

/// Number of `f32`s per vertex on the colour stream (`[x, y, r, g, b, a]`).
const FLOATS_PER_VERTEX: usize = 6;
/// Number of `f32`s per vertex on the sprite stream
/// (`[x, y, u, v, r, g, b, a]`). Matches `hud.rs`'s stride, because
/// `item_icon::push_sprite_quad` writes both streams' vertices — but that
/// constant is private to `hud`, so it is restated (and pinned by a test)
/// rather than reached into.
const SPRITE_FLOATS_PER_VERTEX: usize = 8;

/// The uploaded GUI atlas and the textured pipeline that samples it: what turns
/// a `widget/button` nine-slice into pixels. Absent on a jar-less run, where the
/// menu falls back to flat coloured button fills.
#[derive(Debug)]
struct MenuSprites {
    atlas: Arc<GuiAtlas>,
    /// Kept alive because the bind group's texture view is derived from it.
    #[allow(dead_code)]
    gpu: GpuAtlas,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
}

/// GPU renderer for the menu screens: a coloured-quad pipeline, a textured GUI
/// sprite pipeline, and a growable dynamic vertex buffer for each. Drawn in a
/// `Clear` pass for a screen that owns the frame and a `Load` pass for the pause
/// overlay.
#[derive(Debug)]
pub struct MenuRenderer {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
    /// The target format, kept so the sprite pipeline can be built later —
    /// [`MenuRenderer::new`] cannot build it, because uploading the atlas needs
    /// a `Queue` and `new` is only given a `Device`.
    color_format: wgpu::TextureFormat,
    /// The GUI sprite half, attached lazily on the first draw (see
    /// [`MenuRenderer::ensure_gui`]).
    sprites: Option<MenuSprites>,
    /// Whether the lazy load has already been tried. Without this a jar-less run
    /// would re-stitch (and fail) an atlas every single frame.
    gui_attempted: bool,
    /// Vanilla's proportional font, resolved once per process from the same jar.
    /// Needs no GPU resources, so it is resolved in `new` exactly as
    /// `HudRenderer` does. `None` on a jar-less run.
    font: Option<Arc<VanillaFont>>,
}

impl MenuRenderer {
    /// Builds the menu pipeline for a target of `color_format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("menu-shader"),
            source: wgpu::ShaderSource::Wgsl(MENU_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("menu-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("menu-pipeline"),
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

        let capacity_floats = 1 << 16;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("menu-verts"),
            size: (capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            buffer,
            capacity_floats,
            color_format,
            sprites: None,
            gui_attempted: false,
            font: VanillaFont::shared(),
        }
    }

    /// Whether the real GUI sprite atlas is bound, i.e. whether the buttons draw
    /// as vanilla's nine-slice `widget/button*` art rather than flat fills.
    ///
    /// A gate that means to measure vanilla button chrome **must assert this**:
    /// without it a missing jar silently degrades to the coloured-rectangle
    /// fallback and every "something drew in the button's rect" assertion still
    /// passes. Same discipline as `HudRenderer::font_attached`.
    #[must_use]
    pub fn gui_attached(&self) -> bool {
        self.sprites.is_some()
    }

    /// Whether vanilla text is in play. See [`Self::gui_attached`].
    #[must_use]
    pub fn font_attached(&self) -> bool {
        self.font.is_some()
    }

    /// Bind a GUI sprite atlas: uploads it, builds the textured pipeline, and
    /// binds it.
    ///
    /// The atlas must be one built with
    /// [`crate::resources::TITLE_TEXTURES`](crate::resources::TITLE_TEXTURES)
    /// for the title logo to draw; a plain [`GuiAtlas::build`] atlas gives
    /// correct buttons and no logo, because the logo is not a `gui/sprites`
    /// texture. Calling this replaces whatever was bound.
    pub fn attach_gui(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        atlas: Arc<GuiAtlas>,
    ) {
        let gpu = GpuAtlas::from_atlas(device, queue, atlas.atlas());
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("menu-sprite-shader"),
            source: wgpu::ShaderSource::Wgsl(MENU_SPRITE_WGSL.into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("menu-sprite-bgl"),
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
            label: Some("menu-sprite-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("menu-sprite-pipeline"),
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
                    format: self.color_format,
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
            label: Some("menu-sprite-bind"),
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
        let capacity_floats = 1 << 14;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("menu-sprite-verts"),
            size: (capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.gui_attempted = true;
        self.sprites = Some(MenuSprites {
            atlas,
            gpu,
            pipeline,
            bind_group,
            buffer,
            capacity_floats,
        });
    }

    /// Drop back to the flat coloured-rectangle buttons. The executed negative
    /// control for every "the real vanilla sprite drew here" assertion: with this
    /// called, a gate claiming to see `widget/button` must fail.
    pub fn detach_gui(&mut self) {
        self.sprites = None;
        // Deliberately leaves `gui_attempted` set, so `ensure_gui` does not
        // helpfully undo the control on the next draw.
        self.gui_attempted = true;
    }

    /// Load and bind the GUI atlas on first use.
    ///
    /// Lazy rather than an `attach_gui` call from `app.rs` for one reason: it
    /// needs a `Queue`, which `MenuRenderer::new`'s call site has but does not
    /// pass, and `app.rs` is not this change's to edit. Every draw path already
    /// receives both a `Device` and a `Queue`, so this is the one place that has
    /// what the upload needs. `attach_gui` stays public so `app.rs` can hand in a
    /// shared atlas later and skip the second stitch.
    fn ensure_gui(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.gui_attempted {
            return;
        }
        self.gui_attempted = true;
        if let Some(atlas) = crate::resources::load_menu_gui_atlas() {
            self.attach_gui(device, queue, atlas);
        }
    }

    /// Draws one menu frame, clearing the target first. For a screen owning
    /// the whole frame (see [`owns_frame`]) — nothing renders behind a menu,
    /// so clearing rather than loading is what keeps the last world frame
    /// from showing through.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &MenuFrame<'_>,
        width: u32,
        height: u32,
    ) {
        self.draw(
            device,
            queue,
            view,
            frame,
            width,
            height,
            wgpu::LoadOp::Clear(wgpu::Color {
                r: f64::from(BG[0]),
                g: f64::from(BG[1]),
                b: f64::from(BG[2]),
                a: 1.0,
            }),
        );
    }

    /// Draws one frame **over** whatever `view` already holds instead of
    /// clearing it first — for the pause menu (see [`pause_frame`]), which
    /// sits on top of the world, HUD and container passes the caller already
    /// ran this frame rather than replacing them (mirrors
    /// [`crate::effects::EffectsRenderer`]'s own Load-pass overlay). Every
    /// other detail — buffer growth, the vertex layout, the pipeline — is
    /// identical to [`render`](Self::render); only the load op differs, so a
    /// caller must never invoke both in the same frame — `Screen::Paused` is
    /// not an [`owns_frame`] screen for exactly this reason: [`render`] and
    /// `render_overlay` are alternatives, not a pair meant to compose.
    pub fn render_overlay(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &MenuFrame<'_>,
        width: u32,
        height: u32,
    ) {
        self.draw(
            device,
            queue,
            view,
            frame,
            width,
            height,
            wgpu::LoadOp::Load,
        );
    }

    /// Shared body of [`render`](Self::render) and
    /// [`render_overlay`](Self::render_overlay); only the pass's load op
    /// differs between them.
    fn draw(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &MenuFrame<'_>,
        width: u32,
        height: u32,
        load: wgpu::LoadOp<wgpu::Color>,
    ) {
        self.ensure_gui(device, queue);
        let (logical_w, logical_h) = logical_canvas(frame.gui_scale, width, height);
        let geo = build(
            frame,
            self.sprites.as_ref().map(|s| s.atlas.as_ref()),
            self.font.as_deref(),
            logical_w,
            logical_h,
        );
        if geo.colour.len() > self.capacity_floats {
            self.capacity_floats = geo.colour.len().next_power_of_two();
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("menu-verts"),
                size: (self.capacity_floats * 4) as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&geo.colour));

        if let Some(sprites) = self.sprites.as_mut()
            && !geo.sprite.is_empty()
        {
            if geo.sprite.len() > sprites.capacity_floats {
                sprites.capacity_floats = geo.sprite.len().next_power_of_two();
                sprites.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("menu-sprite-verts"),
                    size: (sprites.capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(&sprites.buffer, 0, bytemuck::cast_slice(&geo.sprite));
        }

        // Three draws, one pass. The split is `MenuGeometry::backdrop_floats`:
        // backdrop, then every GUI sprite, then the rest of the colour stream —
        // so a button's label lands *on* its nine-slice background rather than
        // under it. A render pass can rebind its pipeline, so this needs no
        // extra pass (and must not have one: the load op is only correct once).
        let backdrop_verts = (geo.backdrop_floats / FLOATS_PER_VERTEX) as u32;
        let colour_verts = (geo.colour.len() / FLOATS_PER_VERTEX) as u32;
        let sprite_verts = (geo.sprite.len() / SPRITE_FLOATS_PER_VERTEX) as u32;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("menu"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("menu-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.buffer.slice(..));
            if backdrop_verts > 0 {
                pass.draw(0..backdrop_verts, 0..1);
            }
            if let Some(sprites) = self.sprites.as_ref()
                && sprite_verts > 0
            {
                pass.set_pipeline(&sprites.pipeline);
                pass.set_bind_group(0, &sprites.bind_group, &[]);
                pass.set_vertex_buffer(0, sprites.buffer.slice(..));
                pass.draw(0..sprite_verts, 0..1);
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.buffer.slice(..));
            }
            if colour_verts > backdrop_verts {
                pass.draw(backdrop_verts..colour_verts, 0..1);
            }
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}

const MENU_WGSL: &str = include_str!("../shaders/menu.wgsl");

const MENU_SPRITE_WGSL: &str = include_str!("../shaders/menu_sprite.wgsl");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::menu::nav::{MenuKey, MenuNav};
    use crate::menu::status::{ServerStatus, StatusCache, unavailable_probe};
    use crate::menu::{Screen, SessionKind, UiState};

    /// Vertex stride in the emitted buffer.
    const STRIDE: usize = FLOATS_PER_VERTEX;

    /// A nav with a temporary (never-loaded) list path, so no test reads the
    /// developer's real `servers.json`.
    fn test_nav(tag: &str) -> MenuNav {
        let path = std::env::temp_dir().join(format!(
            "lodestone-render-{}-{tag}/servers.json",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        MenuNav::with_path(path)
    }

    fn add_server(nav: &mut MenuNav, ui: &mut UiState, name: &str, addr: &str) {
        let back = ui.screen();
        if back != Screen::ServerList {
            *ui = UiState::new();
            ui.open_server_list();
        }
        nav.key(ui, MenuKey::Char('a'));
        for c in name.chars() {
            nav.key(ui, MenuKey::Char(c));
        }
        nav.key(ui, MenuKey::Tab);
        for c in addr.chars() {
            nav.key(ui, MenuKey::Char(c));
        }
        nav.key(ui, MenuKey::Enter);
    }

    #[test]
    fn owns_frame_agrees_with_frame_for_on_every_screen() {
        // Two definitions of "this renderer owns the frame" that can disagree is
        // how a screen ends up drawn twice, or not at all. Walk every screen and
        // require the predicate and the builder to say the same thing.
        let mut nav = test_nav("owns");
        let mut fav = FaviconCache::new();
        let statuses = StatusCache::with_probe(unavailable_probe());

        let mut reached = 0;
        for screen in [
            Screen::MainMenu,
            Screen::ServerList,
            Screen::ServerEdit,
            Screen::WorldSelect,
            Screen::Settings,
            Screen::Accounts,
            Screen::Connecting,
            Screen::Playing,
            Screen::Chat,
            Screen::Container,
            Screen::Paused,
            Screen::Death,
            Screen::Error,
        ] {
            let mut ui = UiState::new();
            match screen {
                Screen::MainMenu => {}
                Screen::ServerList => ui.open_server_list(),
                Screen::ServerEdit => {
                    ui.open_server_list();
                    ui.open_server_edit();
                }
                Screen::WorldSelect => ui.open_world_select(),
                Screen::Settings => ui.open_settings(),
                Screen::Accounts => ui.open_accounts(),
                Screen::Connecting => ui.begin(SessionKind::Multiplayer),
                Screen::Playing => ui.enter_dev_world(),
                Screen::Chat => {
                    ui.enter_dev_world();
                    ui.open_chat();
                }
                Screen::Container => {
                    ui.enter_dev_world();
                    ui.open_container();
                }
                Screen::Paused => {
                    ui.enter_dev_world();
                    ui.pause();
                }
                Screen::Death => {
                    ui.enter_dev_world();
                    ui.die(Some("blew up".to_string()));
                }
                Screen::Error => {
                    ui.begin(SessionKind::Multiplayer);
                    ui.session_failed("connection refused");
                }
            }
            assert_eq!(ui.screen(), screen, "failed to reach {screen:?}");
            reached += 1;
            let built = frame_for(&ui, &nav, &statuses, &mut fav).is_some();
            assert_eq!(
                built,
                owns_frame(screen),
                "owns_frame and frame_for disagree about {screen:?}"
            );
            // And a frame it claims must actually be drawable.
            if built {
                let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
                // A vanilla-laid-out screen has no centred heading string — its
                // heading is the logo texture (title) or a positioned
                // `MenuLabel` (pause), so requiring `title` would be requiring
                // the *un*-vanilla layout. It must still say something.
                if f.vanilla {
                    assert!(
                        f.logo || !f.labels.is_empty(),
                        "{screen:?} is vanilla-laid-out but draws neither a logo nor a label"
                    );
                } else {
                    assert!(!f.title.is_empty(), "{screen:?} has no title");
                }
                assert!(
                    !geometry(&f, 1280.0, 720.0).is_empty(),
                    "{screen:?} draws nothing"
                );
            }
        }
        assert_eq!(reached, 12, "a screen was added without being covered here");
        let _ = &mut nav;
    }

    #[test]
    fn the_server_list_shows_the_motd_players_and_latency_from_a_status() {
        // The content gate: what the status decoder produced has to appear in the
        // row, not merely be cached.
        let mut nav = test_nav("content");
        let mut ui = UiState::new();
        add_server(&mut nav, &mut ui, "HOME", "mc.example.com");

        let mut statuses = StatusCache::with_probe(std::sync::Arc::new(|_| {
            Ok(ServerStatus {
                motd: "A LODESTONE SERVER\nsecond line".into(),
                players: "3/20".into(),
                version: "26.2".into(),
                favicon_png: None,
                latency_ms: Some(12),
            })
        }));
        let entries = nav.list().entries().to_vec();
        statuses.refresh(&entries);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while statuses.pump() == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        let mut fav = FaviconCache::new();
        let f = frame_for(&ui, &nav, &statuses, &mut fav).expect("the list draws");
        assert_eq!(f.rows.len(), 1);
        assert_eq!(f.rows[0].label, "HOME");
        assert_eq!(
            f.rows[0].detail, "A LODESTONE SERVER",
            "only the MOTD's first line fits a row"
        );
        assert!(f.rows[0].trailing.contains("3/20"), "{:?}", f.rows[0]);
        assert!(f.rows[0].trailing.contains("12"), "latency should show");
        assert!(!f.rows[0].detail_is_error);
    }

    #[test]
    fn a_failed_ping_shows_its_reason_in_the_error_colour() {
        let mut nav = test_nav("failed");
        let mut ui = UiState::new();
        add_server(&mut nav, &mut ui, "DEAD", "dead.example");

        let mut statuses = StatusCache::with_probe(std::sync::Arc::new(|_| {
            Err("connection refused".to_string())
        }));
        let entries = nav.list().entries().to_vec();
        statuses.refresh(&entries);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while statuses.pump() == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }

        let mut fav = FaviconCache::new();
        let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        assert_eq!(f.rows[0].detail, "connection refused");
        assert!(
            f.rows[0].detail_is_error,
            "a failure must be visually distinct from a MOTD"
        );
        assert!(f.rows[0].trailing.is_empty(), "no player count to show");
    }

    #[test]
    fn an_empty_server_list_says_how_to_add_one() {
        // A blank screen with no rows and no instruction is a dead end.
        let nav = test_nav("emptylist");
        let mut ui = UiState::new();
        ui.open_server_list();
        let statuses = StatusCache::with_probe(unavailable_probe());
        let mut fav = FaviconCache::new();
        let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        assert!(f.rows.is_empty());
        assert!(f.subtitle.contains('A'), "no hint: {:?}", f.subtitle);
        assert!(
            f.footer.iter().any(|l| l.contains("ADD")),
            "the footer should name the add key: {:?}",
            f.footer
        );
    }

    #[test]
    fn the_error_screen_carries_the_disconnect_reason() {
        let nav = test_nav("err");
        let mut ui = UiState::new();
        ui.begin(SessionKind::Multiplayer);
        ui.session_failed("disconnected: Server closed");
        let statuses = StatusCache::with_probe(unavailable_probe());
        let mut fav = FaviconCache::new();
        let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        let msg = f.message.expect("the reason must reach the screen");
        assert!(msg.contains("SERVER CLOSED"), "{msg}");
    }

    #[test]
    fn a_favicon_is_decoded_once_not_once_per_frame() {
        // 60 zlib inflations per second per row is the bug this prevents.
        let png = solid_png(8, [1, 2, 3, 255]);
        let mut fav = FaviconCache::new();
        assert!(fav.is_empty());
        let first = fav.get("a.example:25565", &png);
        assert!(first.is_some());
        assert_eq!(fav.len(), 1);
        for _ in 0..100 {
            assert_eq!(fav.get("a.example:25565", &png), first);
        }
        assert_eq!(fav.len(), 1, "one entry per address, whatever the frame count");

        // A failed decode is cached too, or a broken icon retries forever.
        assert!(fav.get("b.example:25565", b"not a png").is_none());
        assert_eq!(fav.len(), 2);
        fav.forget("b.example:25565");
        assert_eq!(fav.len(), 1);
    }

    /// What reached one rectangle of the colour stream: how many vertices, and
    /// **where**.
    ///
    /// A box rather than a fraction, per `CLAUDE.md`: a gate that reports only a
    /// count cannot tell a shifted widget from a missing one, and both of the
    /// control-premise failures recorded there were diagnosed by printing a
    /// bounding box instead of a percentage.
    #[derive(Debug)]
    struct BandCoverage {
        count: usize,
        /// `(x0, y0, x1, y1)` in logical pixels, or `None` when nothing reached.
        bounds: Option<(f32, f32, f32, f32)>,
    }

    /// Colour-stream vertices inside `band`, in logical pixels — the inverse of
    /// `Quads::rect`'s `(2x/w - 1, 1 - 2y/h)`.
    ///
    /// **Strict on y, inclusive on x**, and the asymmetry is the whole reason
    /// this reads a *band* rather than the field rect. `CLAUDE.md`'s rule is to
    /// ask what else already paints here; the answer is the field's own chrome:
    ///
    /// - its background fill and its focus outline's left/right edges sit at the
    ///   field's outer `x`, which is `BORDER_INSET` outside the band's — so the
    ///   horizontal test can be inclusive and still exclude them, which keeps the
    ///   caret's own left edge (exactly at `text_x`) counted;
    /// - its outline's **bottom** edge, though, lands *inside* the band's
    ///   vertical extent while spanning the full field width. Only a strict `y`
    ///   keeps it out, and an inclusive one would report a bounding box the width
    ///   of the whole field whatever the value was — a control that fires while
    ///   measuring something unrelated.
    fn band_coverage(
        colour: &[f32],
        w: f32,
        h: f32,
        band: (f32, f32, f32, f32),
    ) -> BandCoverage {
        let (bx, by, bw, bh) = band;
        let mut count = 0;
        let (mut x0, mut y0) = (f32::MAX, f32::MAX);
        let (mut x1, mut y1) = (f32::MIN, f32::MIN);
        for v in colour.chunks_exact(STRIDE) {
            let px = (v[0] + 1.0) * 0.5 * w;
            let py = (1.0 - v[1]) * 0.5 * h;
            if px >= bx - 0.01 && px <= bx + bw + 0.01 && py > by && py < by + bh {
                count += 1;
                x0 = x0.min(px);
                y0 = y0.min(py);
                x1 = x1.max(px);
                y1 = y1.max(py);
            }
        }
        BandCoverage {
            count,
            bounds: (count > 0).then_some((x0, y0, x1, y1)),
        }
    }

    /// #395's pixel gate: a real `EditBox` on a real screen, measured **inside
    /// its own rect**, with the caret at two different positions.
    ///
    /// Every bound here is derived from the widget rather than restated: the rect
    /// comes from [`field_rect`] (the same function the draw calls) and the text
    /// band from a clone of the live box repositioned into it, so the gate cannot
    /// pass by agreeing with a constant that the draw does not use.
    #[test]
    fn the_edit_box_draws_its_text_and_its_caret_inside_its_own_rect() {
        const W: f32 = 854.0;
        const H: f32 = 480.0;
        let mut nav = test_nav("editbox-pixels");
        let mut ui = UiState::new();
        ui.open_server_list();
        nav.key(&mut ui, MenuKey::Char('a'));
        assert_eq!(ui.screen(), Screen::ServerEdit, "premise: the form is open");
        let statuses = StatusCache::with_probe(unavailable_probe());
        let mut fav = FaviconCache::new();

        // The widget as the draw sees it: a clone of the live box moved into this
        // frame's rect, exactly as `draw_edit_box` does it.
        let probe_of = |frame: &MenuFrame<'_>| -> EditBox {
            let rect = field_rect(&frame.rows, 0, W, H).expect("row 0 is the name field");
            let mut probe = frame.rows[0]
                .edit
                .clone()
                .expect("the name row must carry its EditBox, or nothing draws");
            probe.widget.x = rect.0;
            probe.widget.y = rect.1;
            probe.widget.width = rect.2;
            probe.widget.height = rect.3;
            probe
        };
        let band_of = |probe: &EditBox| -> (f32, f32, f32, f32) {
            (
                probe.text_x(),
                probe.text_y(),
                probe.inner_width(),
                GLYPH_H as f32 * TEXT_SCALE,
            )
        };

        // The control, executed rather than described: an empty focused field
        // paints its caret and nothing else. If this were zero the band would be
        // pointing somewhere nothing draws and every measurement below would be of
        // the wrong rectangle.
        let empty = frame_for(&ui, &nav, &statuses, &mut fav).expect("the form owns its frame");
        let band = band_of(&probe_of(&empty));
        let blank = band_coverage(&geometry(&empty, W, H), W, H, band);
        assert!(
            blank.count > 0,
            "premise: a focused empty field paints a caret inside {band:?}, found \
             nothing — the band is in the wrong place"
        );
        let (_, blank_y0, _, blank_y1) = blank.bounds.unwrap();
        assert!(
            blank_y1 - blank_y0 < 4.0,
            "premise: with no value the band holds only the caret, so its vertical \
             extent is a bar and not a line of glyphs; got {}",
            blank_y1 - blank_y0
        );

        for c in "mc.example.com".chars() {
            nav.key(&mut ui, MenuKey::Char(c));
        }
        let typed = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        let probe = probe_of(&typed);
        assert_eq!(
            band_of(&probe),
            band,
            "the field must not move between frames, or the two measurements are \
             of different rectangles"
        );
        let full = band_coverage(&geometry(&typed, W, H), W, H, band);
        assert!(
            full.count > blank.count * 8,
            "typing must paint glyphs inside the field: empty {blank:?}, typed {full:?}"
        );
        let (x0, y0, x1, y1) = full.bounds.expect("checked non-empty above");
        // The band is the *counting window*, so "it is inside the band" would be
        // vacuous. The claim is that what was painted matches the widget's **own**
        // arithmetic: the leftmost pixel is the box's `text_x` and the rightmost is
        // its caret's right edge. Both are read off the widget, never restated —
        // a draw that used the row's `PAD` (6) instead of `BORDER_INSET` (4) would
        // land two pixels out and fail here.
        let state = probe.draw_state(None);
        assert!(
            (x0 - probe.text_x()).abs() <= 0.5,
            "the value must start at the box's own text_x {}, painted from {x0}",
            probe.text_x()
        );
        assert!(
            (x1 - (state.cursor_x + probe.advance)).abs() <= 0.5,
            "the rightmost pixel must be the caret's right edge {}, painted to {x1} \
             (bounds ({x0}, {y0})..({x1}, {y1}))",
            state.cursor_x + probe.advance
        );
        assert!(
            y1 - y0 >= GLYPH_H as f32 * TEXT_SCALE - 6.0,
            "a full line of glyphs must be present, not just the caret bar: the \
             band's vertical extent is only {}",
            y1 - y0
        );

        // The caret at two positions: one Backspace and the rightmost painted
        // pixel in the band must retreat by about one character — not by nothing
        // (a frozen caret) and not by the whole field (a re-laid-out one).
        nav.key(&mut ui, MenuKey::Backspace);
        let shorter = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        let after = band_coverage(&geometry(&shorter, W, H), W, H, band);
        let (_, _, x1_after, _) = after.bounds.expect("still drawing");
        let advance = probe.advance;
        assert!(
            x1_after < x1 - 1.0,
            "the caret must move left with the text: {x1} -> {x1_after}"
        );
        assert!(
            x1 - x1_after <= advance * 1.5,
            "one Backspace moved the right edge by {}, which is more than one \
             character ({advance} px)",
            x1 - x1_after
        );
        // And it landed on the shorter value's own caret, not just somewhere left.
        let shorter_probe = probe_of(&shorter);
        let shorter_state = shorter_probe.draw_state(None);
        assert!(
            (x1_after - (shorter_state.cursor_x + shorter_probe.advance)).abs() <= 0.5,
            "expected the caret's right edge at {}, painted to {x1_after}",
            shorter_state.cursor_x + shorter_probe.advance
        );
    }

    #[test]
    fn the_edit_form_shows_both_fields_and_marks_the_focused_one() {
        let mut nav = test_nav("form");
        let mut ui = UiState::new();
        ui.open_server_list();
        nav.key(&mut ui, MenuKey::Char('a'));
        for c in "abc".chars() {
            nav.key(&mut ui, MenuKey::Char(c));
        }
        let statuses = StatusCache::with_probe(unavailable_probe());
        let mut fav = FaviconCache::new();
        let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        assert_eq!(f.rows.len(), 2);
        assert!(f.rows.iter().all(|r| r.field), "both rows are text fields");
        assert_eq!(f.rows[0].label, "abc");
        assert_eq!(f.selected, 0, "the name field has focus");
        assert!(
            f.message.is_some(),
            "an addressless form must say so rather than looking ready to save"
        );

        nav.key(&mut ui, MenuKey::Tab);
        let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        assert_eq!(f.selected, 1, "Tab moves focus to the address");
    }

    fn frame_with(rows: Vec<MenuRow>, selected: usize) -> MenuFrame<'static> {
        MenuFrame {
            title: "LODESTONE",
            subtitle: "",
            rows,
            selected,
            footer: vec![],
            message: None,
            gui_scale: 0,
            overlay: false,
            ..Default::default()
        }
    }

    fn button(label: &str) -> MenuRow {
        MenuRow {
            label: label.to_string(),
            enabled: true,
            ..Default::default()
        }
    }

    /// Fraction of sample points inside the pixel rect `(x, y, w, h)` that any
    /// emitted quad covers with a colour other than the background.
    ///
    /// This is the coverage measurement the repo's rules call for: it asks
    /// *where* pixels landed, not how many vertices came out, so a layout bug
    /// that draws everything off-screen fails it.
    fn coverage(verts: &[f32], w: f32, h: f32, rect: (f32, f32, f32, f32)) -> f32 {
        let (rx, ry, rw, rh) = rect;
        const N: usize = 24;
        let mut hit = 0usize;
        for iy in 0..N {
            for ix in 0..N {
                let px = rx + rw * (ix as f32 + 0.5) / N as f32;
                let py = ry + rh * (iy as f32 + 0.5) / N as f32;
                // NDC of this sample.
                let nx = 2.0 * px / w - 1.0;
                let ny = 1.0 - 2.0 * py / h;
                if covered(verts, nx, ny) {
                    hit += 1;
                }
            }
        }
        hit as f32 / (N * N) as f32
    }

    /// Whether any emitted quad other than the full-screen background covers
    /// NDC point `(nx, ny)`. Quads are axis-aligned pairs of triangles, so the
    /// first and fifth vertex of each six give the corners.
    fn covered(verts: &[f32], nx: f32, ny: f32) -> bool {
        verts
            .chunks_exact(STRIDE * 6)
            .skip(1) // vertex 0..6 is the background clear rect
            .any(|q| {
                let (x0, y0) = (q[0], q[1]);
                let (x1, y1) = (q[STRIDE * 4], q[STRIDE * 4 + 1]);
                let (lo_x, hi_x) = (x0.min(x1), x0.max(x1));
                let (lo_y, hi_y) = (y0.min(y1), y0.max(y1));
                nx >= lo_x && nx <= hi_x && ny >= lo_y && ny <= hi_y
            })
    }

    /// The colour of the *last* (i.e. topmost-painted) quad covering NDC
    /// point `(nx, ny)`, or `None` if only the background is there.
    ///
    /// Unlike `covered`, which only answers "is anything here", this can
    /// tell a row's own fill (`ROW_BG`/`ROW_SEL`) apart from a border drawn
    /// on top of it in a different colour — necessary because the fill
    /// quad already covers every pixel the border does, so presence alone
    /// cannot distinguish "outlined" from "an ordinary row". Quads are
    /// pushed in paint order, so the last one in the buffer that covers the
    /// point is the one actually visible there.
    fn colour_at(verts: &[f32], nx: f32, ny: f32) -> Option<[f32; 4]> {
        verts
            .chunks_exact(STRIDE * 6)
            .skip(1) // vertex 0..6 is the background clear rect
            .filter(|q| {
                let (x0, y0) = (q[0], q[1]);
                let (x1, y1) = (q[STRIDE * 4], q[STRIDE * 4 + 1]);
                let (lo_x, hi_x) = (x0.min(x1), x0.max(x1));
                let (lo_y, hi_y) = (y0.min(y1), y0.max(y1));
                nx >= lo_x && nx <= hi_x && ny >= lo_y && ny <= hi_y
            })
            .last()
            .map(|q| [q[2], q[3], q[4], q[5]])
    }

    /// Fraction of sample points inside `(x, y, w, h)` whose topmost quad is
    /// (approximately) `colour` — see `colour_at`. Where `coverage`'s
    /// colour-blind "is anything here" cannot separate a highlight border
    /// from the row fill it is painted over, this can.
    fn coverage_of(
        verts: &[f32],
        w: f32,
        h: f32,
        rect: (f32, f32, f32, f32),
        colour: [f32; 4],
    ) -> f32 {
        let (rx, ry, rw, rh) = rect;
        const N: usize = 24;
        let mut hit = 0usize;
        for iy in 0..N {
            for ix in 0..N {
                let px = rx + rw * (ix as f32 + 0.5) / N as f32;
                let py = ry + rh * (iy as f32 + 0.5) / N as f32;
                let nx = 2.0 * px / w - 1.0;
                let ny = 1.0 - 2.0 * py / h;
                let matches = colour_at(verts, nx, ny)
                    .is_some_and(|c| c.iter().zip(colour).all(|(a, b)| (a - b).abs() < 0.01));
                if matches {
                    hit += 1;
                }
            }
        }
        hit as f32 / (N * N) as f32
    }

    #[test]
    fn every_vertex_lands_inside_the_viewport() {
        // The island's favourite disguise: geometry that exists and is drawn
        // entirely off-screen.
        let f = frame_with(
            vec![button("SINGLEPLAYER"), button("MULTIPLAYER"), button("QUIT")],
            1,
        );
        let v = geometry(&f, 1280.0, 720.0);
        assert!(!v.is_empty(), "a menu with rows must emit geometry");
        assert_eq!(v.len() % STRIDE, 0);
        for vert in v.chunks_exact(STRIDE) {
            assert!(
                (-1.001..=1.001).contains(&vert[0]) && (-1.001..=1.001).contains(&vert[1]),
                "vertex outside NDC: {:?}",
                &vert[..2]
            );
        }
    }

    #[test]
    fn the_selected_row_is_visibly_different_from_its_neighbours() {
        // Reading only the vertex count cannot tell a highlight from a no-op.
        // This compares the *border colour actually painted at the row's own
        // rect*, not merely whether anything is there — the row's own fill
        // (`ROW_BG`/`ROW_SEL`) already covers those pixels regardless of
        // selection, so a colour-blind `coverage` check cannot tell
        // "outlined" from "an ordinary row" (see `coverage_of`'s docs).
        let rows = vec![button("ONE"), button("TWO"), button("THREE")];
        let (w, h) = (1280.0, 720.0);
        let sel = geometry(&frame_with(rows.clone(), 1), w, h);
        let unsel = geometry(&frame_with(rows.clone(), 99), w, h);
        assert_ne!(
            sel, unsel,
            "selecting a row must change the emitted geometry"
        );

        let rect = row_rect(&rows, 1, w, h).expect("row 1 exists");
        // The selection border is 2 px inside the row; sample its top edge.
        let border = (rect.0 + 4.0, rect.1, rect.2 - 8.0, 2.0);
        assert!(
            coverage_of(&sel, w, h, border, FG) > 0.9,
            "the highlighted row should be outlined in FG: {:?}",
            coverage_of(&sel, w, h, border, FG)
        );
        assert!(
            coverage_of(&unsel, w, h, border, FG) < 0.05,
            "an unhighlighted row must not be outlined: {:?}",
            coverage_of(&unsel, w, h, border, FG)
        );
    }

    #[test]
    fn a_rows_text_lands_inside_that_rows_rect() {
        // Negative control included: a row's glyphs must be inside *its* rect
        // and absent from the rect below it, or the layout is off by a row.
        let rows = vec![button("AAAA"), button("BBBB")];
        let (w, h) = (1280.0, 720.0);
        let v = geometry(&frame_with(rows.clone(), 99), w, h);
        let (x, y, rw, rh) = row_rect(&rows, 0, w, h).unwrap();
        // Sample a band where the glyphs are, just right of the padding.
        let band = (x + PAD, y + rh * 0.35, text_px("AAAA", TEXT_SCALE), rh * 0.3);
        assert!(
            coverage(&v, w, h, band) > 0.25,
            "row 0's label is not in row 0's rect: {}",
            coverage(&v, w, h, band)
        );
        // And the gap between rows must be background only.
        let gap = (x, y + rh + 1.0, rw, ROW_GAP - 2.0);
        assert!(
            coverage(&v, w, h, gap) < 0.05,
            "something is drawn in the inter-row gap: {}",
            coverage(&v, w, h, gap)
        );
    }

    #[test]
    fn row_rects_are_ordered_non_overlapping_and_on_screen() {
        let rows: Vec<MenuRow> = (0..6).map(|i| button(&format!("ROW{i}"))).collect();
        let (w, h) = (1280.0, 720.0);
        let mut prev_bottom = 0.0f32;
        for i in 0..rows.len() {
            let (x, y, rw, rh) = row_rect(&rows, i, w, h).expect("row exists");
            assert!(y >= prev_bottom, "row {i} overlaps the one above");
            assert!(x >= 0.0 && x + rw <= w, "row {i} is off-screen: {x}+{rw}");
            assert!(y + rh <= h, "row {i} runs off the bottom");
            prev_bottom = y + rh;
        }
        assert!(row_rect(&rows, 99, w, h).is_none());
    }

    #[test]
    fn a_slotted_row_sharing_a_frame_does_not_perturb_the_centred_stacks_math() {
        // The bug this guards: `row_rect`'s centred-stack total used to sum
        // *every* row's height, including slotted ones, because no screen had
        // ever mixed the two kinds before the account screen (a scrollable
        // unslotted list plus slotted nine-slice action buttons). Build one
        // unslotted-only frame and one with an extra slotted row spliced in
        // between two unslotted rows, and require the *unslotted* rows to land
        // at identical rects in both — the slotted row must be invisible to
        // their stack.
        let (w, h) = (1280.0, 720.0);
        let plain: Vec<MenuRow> = vec![button("A"), button("B")];
        let plain_rects: Vec<_> = (0..plain.len())
            .map(|i| row_rect(&plain, i, w, h).unwrap())
            .collect();

        let mut mixed = vec![button("A")];
        mixed.push(MenuRow {
            label: "SLOTTED".to_string(),
            enabled: true,
            slot: Some(Slot {
                origin: Origin::ScreenTop,
                dx: 0.0,
                dy: 0.0,
                w: 50.0,
                h: 20.0,
            }),
            ..Default::default()
        });
        mixed.push(button("B"));

        let a_rect = row_rect(&mixed, 0, w, h).unwrap();
        let b_rect = row_rect(&mixed, 2, w, h).unwrap();
        assert_eq!(a_rect, plain_rects[0], "row A must not shift because a slotted row shares the frame");
        assert_eq!(b_rect, plain_rects[1], "row B must not shift either");

        // The slotted row itself is unaffected too — it always resolves via
        // its own `Slot`, never the stack.
        let slotted_rect = row_rect(&mixed, 1, w, h).unwrap();
        assert_eq!(slotted_rect, (1280.0 * 0.5, 0.0, 50.0, 20.0));
    }

    #[test]
    fn default_head_icon_is_a_real_mosaic_not_a_blank_or_transparent_one() {
        // The account screen's placeholder head must actually reach pixels —
        // an all-transparent or all-zero mosaic would draw nothing and look
        // exactly like a missing icon, which is indistinguishable from this
        // function being wired to nothing.
        let m = default_head_icon();
        assert_eq!(m.size, MOSAIC);
        assert_eq!(m.cells.len(), MOSAIC * MOSAIC);
        assert!(m.cells.iter().any(|c| c[3] > 0.0), "every cell was transparent");
        // Not a flat single colour either — the hairline row and eye pixels
        // must show up as *some* variation, or `head_mosaic`'s box filter
        // could be silently discarding the source detail.
        let first = m.cells[0];
        assert!(
            m.cells.iter().any(|c| c != &first),
            "the mosaic is a single flat colour; the hand-authored detail did not survive the filter"
        );
    }

    #[test]
    fn head_mosaic_is_the_same_drawable_favicon_mosaic_is() {
        // `head_mosaic` takes raw RGBA + dimensions (what a decoded skin's
        // face region would already be), unlike `favicon_mosaic`'s PNG bytes
        // — this pins that the two still produce the same shape of output
        // (same box filter) given equivalent solid-colour input.
        let rgba = vec![10u8, 200, 30, 255].repeat(4 * 4);
        let m = head_mosaic(&rgba, 4, 4).expect("a valid RGBA buffer must decode");
        assert_eq!(m.size, MOSAIC);
        for c in &m.cells {
            assert!((c[0] - 10.0 / 255.0).abs() < 0.01);
            assert!((c[1] - 200.0 / 255.0).abs() < 0.01);
            assert!((c[2] - 30.0 / 255.0).abs() < 0.01);
        }
    }

    #[test]
    fn a_favicon_mosaic_reaches_the_rows_icon_square() {
        // The whole point of the favicon path: real PNG bytes → pixels in the
        // row. A solid red 8x8 PNG must fill the icon square with red.
        let png = solid_png(8, [220, 20, 20, 255]);
        let m = favicon_mosaic(&png).expect("a solid PNG must decode");
        assert_eq!(m.size, MOSAIC);
        assert_eq!(m.cells.len(), MOSAIC * MOSAIC);
        for c in &m.cells {
            assert!(c[0] > 0.8 && c[1] < 0.2 && c[2] < 0.2, "cell was {c:?}");
            assert!(c[3] > 0.9, "opaque source must stay opaque: {c:?}");
        }

        let rows = vec![MenuRow {
            label: "SERVER".into(),
            detail: "a motd".into(),
            favicon: Some(m),
            enabled: true,
            ..Default::default()
        }];
        let (w, h) = (1280.0, 720.0);
        let v = geometry(&frame_with(rows.clone(), 0), w, h);
        let (x, y, _, rh) = row_rect(&rows, 0, w, h).unwrap();
        let icon = (x + PAD, y + (rh - ICON) * 0.5, ICON, ICON);
        assert!(
            coverage(&v, w, h, icon) > 0.95,
            "the favicon square is not covered: {}",
            coverage(&v, w, h, icon)
        );

        // Negative control: the same row with no favicon leaves that square to
        // the row fill, so the assertion above is measuring the icon and not
        // the row background.
        let mut bare = rows.clone();
        bare[0].favicon = None;
        let v2 = geometry(&frame_with(bare, 0), w, h);
        assert_ne!(
            v.len(),
            v2.len(),
            "dropping the favicon must remove its quads"
        );
    }

    #[test]
    fn a_broken_favicon_is_skipped_rather_than_panicking() {
        assert!(favicon_mosaic(b"not a png").is_none());
        assert!(favicon_mosaic(&[]).is_none());
        // A valid PNG header with a truncated body.
        assert!(favicon_mosaic(&[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a]).is_none());
    }

    #[test]
    fn a_favicon_smaller_than_the_mosaic_still_fills_every_cell() {
        // The bug this guards: integer division leaving empty source rects and
        // therefore transparent (invisible) cells for a 4x4 icon.
        let m = favicon_mosaic(&solid_png(4, [10, 200, 40, 255])).expect("decodes");
        assert!(
            m.cells.iter().all(|c| c[3] > 0.9),
            "a {MOSAIC}-cell mosaic of a 4x4 source left transparent cells"
        );
    }

    #[test]
    fn long_labels_are_clipped_instead_of_overrunning_the_row() {
        let rows = vec![MenuRow {
            label: "X".repeat(400),
            detail: "Y".repeat(400),
            trailing: "999/999".into(),
            enabled: true,
            ..Default::default()
        }];
        let (w, h) = (1280.0, 720.0);
        let v = geometry(&frame_with(rows.clone(), 0), w, h);
        let (x, y, rw, rh) = row_rect(&rows, 0, w, h).unwrap();
        // Nothing may be drawn to the right of the row.
        let outside = (x + rw + 2.0, y, 200.0, rh);
        assert_eq!(
            coverage(&v, w, h, outside),
            0.0,
            "text overran the row's right edge"
        );
    }

    #[test]
    fn owns_frame_excludes_paused_so_the_pause_menu_never_replaces_the_world() {
        // The specific regression this module's docs warn about: adding
        // `Screen::Paused` to `owns_frame` would make `app.rs`'s `draw_menu`
        // return `true` for it, skipping the world/HUD/container render path
        // entirely — the pause menu would work, but the game behind it would
        // stop rendering for as long as it was up.
        assert!(!owns_frame(Screen::Paused));
    }

    #[test]
    fn pause_frame_builds_vanillas_nine_widgets_in_order_and_tracks_the_highlight() {
        use crate::menu::nav::{PAUSE_BUTTONS, PauseButton};

        let mut nav = test_nav("pause-frame");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.pause();
        // Index 8, not 1: this screen now reproduces vanilla's whole grid, so
        // Disconnect is the ninth widget rather than the third. The old version
        // of this test asserted a three-row stack.
        nav.hover(&ui, PAUSE_BUTTONS.len() - 1);

        let f = pause_frame(&nav);
        assert!(f.overlay, "the pause menu must draw as an overlay");
        assert!(f.vanilla, "and it must be laid out from vanilla's arithmetic");
        assert_eq!(f.rows.len(), 9, "vanilla's pause grid has nine widgets");
        assert_eq!(f.rows[0].label, PauseButton::BackToGame.label());
        assert_eq!(f.rows[1].label, PauseButton::Advancements.label());
        assert_eq!(f.rows[2].label, PauseButton::Statistics.label());
        assert_eq!(f.rows[7].label, PauseButton::Options.label());
        assert_eq!(f.rows[8].label, PauseButton::QuitToTitle.label());
        assert_eq!(f.selected, 8, "selection follows the nav's pause_index");
        // Exactly three are live, and they are the three with actions.
        let live: Vec<&str> = f
            .rows
            .iter()
            .filter(|r| r.enabled)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(live, vec!["Back to Game", "Options...", "Disconnect"]);
        // The four icon buttons carry a sprite instead of a label.
        assert_eq!(f.rows.iter().filter(|r| r.icon.is_some()).count(), 4);
        assert!(f.rows.iter().all(|r| r.slot.is_some()));
        // And the heading is a positioned label, not the row stack's title.
        assert!(f.title.is_empty());
        assert_eq!(f.labels.len(), 1);
        assert_eq!(f.labels[0].text, "Game Menu");
        assert!(!geometry(&f, 1280.0, 720.0).is_empty());
    }

    /// The canvas vanilla's own default window resolves to (854×480 at GUI
    /// scale 1 is vanilla's canonical GUI size), so the expected rects below are
    /// the numbers a vanilla screenshot at that size would show.
    const V_W: f32 = 854.0;
    /// See [`V_W`].
    const V_H: f32 = 480.0;

    #[test]
    fn the_title_screen_rects_are_vanillas_own() {
        use crate::menu::nav::MainButton as B;
        // Hand-derived from `TitleScreen.init` / `createNormalMenuOptions`
        // (`TitleScreen.java:105-205`) at 854×480, *not* read back out of
        // `title_slot`: topPos = 480/4 + 48 = 168, rows every 24 px, the icon
        // row from `getHorizontalPosition(n, 3, 20)` = 427 - 34 + (n-1)*24, and
        // the Options/Quit pair at `W/2 - 100` / `W/2 + 2`, 98 wide.
        //
        // Since #394 `title_slot` computes these from an arranged
        // `LinearLayout` column instead of holding them as constants, so this is
        // the **no-move gate** for that conversion: the table is vanilla's own
        // hand arithmetic (which uses no layout class at all) and the values come
        // out of the layout tree. If the two ever disagree, one of them is wrong
        // and this says which button.
        let expected = [
            (B::Singleplayer, (327.0, 168.0, 200.0, 20.0)),
            (B::Multiplayer, (327.0, 192.0, 200.0, 20.0)),
            (B::Realms, (327.0, 216.0, 200.0, 20.0)),
            (B::Friends, (393.0, 240.0, 20.0, 20.0)),
            (B::Language, (417.0, 240.0, 20.0, 20.0)),
            (B::Accessibility, (441.0, 240.0, 20.0, 20.0)),
            (B::Options, (327.0, 264.0, 98.0, 20.0)),
            (B::Quit, (429.0, 264.0, 98.0, 20.0)),
        ];
        for (button, want) in expected {
            assert_eq!(
                title_slot(button).resolve(V_W, V_H),
                want,
                "{button:?} is not where vanilla puts it"
            );
        }
        // The 4 px gutter between Options and Quit is the title screen's, and it
        // is *not* the pause screen's 8 px one — a detail that is easy to
        // conflate, so pin both.
        let (ox, _, ow, _) = title_slot(B::Options).resolve(V_W, V_H);
        let (qx, ..) = title_slot(B::Quit).resolve(V_W, V_H);
        assert_eq!(qx - (ox + ow), 4.0, "title screen gutter");
    }

    #[test]
    fn the_pause_screen_rects_are_vanillas_own() {
        use crate::menu::nav::PauseButton as B;
        // Hand-derived from `PauseScreen.createPauseMenu` (`PauseScreen.java:91-183`)
        // through `GridLayout.arrangeElements`, at 854×480: the 212×166 grid is
        // aligned (0.5, 0.25) so its origin is (321, 78); row y offsets inside it
        // are [0, 70, 94, 118, 142] and each child sits at its own padding.
        //
        // These nine rects were `pause_slot`'s *implementation* until #394 and are
        // now its expectation: the values below come out of a real ported
        // `GridLayout`, and the table is the independent derivation they have to
        // agree with. Two derivations of the same arithmetic, one by hand from the
        // Java and one by running a port of it — which is the only shape of gate
        // that can catch a port that is self-consistently wrong.
        let gx = 321.0;
        let gy = 78.0;
        let expected = [
            (B::BackToGame, (gx + 4.0, gy + 50.0, 204.0, 20.0)),
            (B::Advancements, (gx + 4.0, gy + 74.0, 98.0, 20.0)),
            (B::Statistics, (gx + 110.0, gy + 74.0, 98.0, 20.0)),
            (B::ReportBugs, (gx + 60.0, gy + 98.0, 20.0, 20.0)),
            (B::Feedback, (gx + 84.0, gy + 98.0, 20.0, 20.0)),
            (B::Friends, (gx + 108.0, gy + 98.0, 20.0, 20.0)),
            (B::PlayerReporting, (gx + 132.0, gy + 98.0, 20.0, 20.0)),
            (B::Options, (gx + 4.0, gy + 122.0, 204.0, 20.0)),
            (B::QuitToTitle, (gx + 4.0, gy + 146.0, 204.0, 20.0)),
        ];
        for (button, want) in expected {
            assert_eq!(
                pause_slot(button).resolve(V_W, V_H),
                want,
                "{button:?} is not where vanilla puts it"
            );
        }
        // The grid origin itself, spelled out: 0.5/0.25 alignment of 212×166.
        assert_eq!(Origin::PauseGrid.anchor(V_W, V_H), (gx, gy));
        // A full-width pause button starts at `W/2 - 102`, not the title
        // screen's `W/2 - 100`, and the half-width pair has an 8 px gutter, not
        // 4 — both fall out of the 204+8 cell, and both are the details a
        // remembered layout gets wrong.
        assert_eq!(
            pause_slot(B::BackToGame).resolve(V_W, V_H).0,
            V_W / 2.0 - 102.0
        );
        let (ax, _, aw, _) = pause_slot(B::Advancements).resolve(V_W, V_H);
        let (sx, ..) = pause_slot(B::Statistics).resolve(V_W, V_H);
        assert_eq!(sx - (ax + aw), 8.0, "pause screen gutter");
        assert_eq!(
            (ax + aw + sx) / 2.0,
            V_W / 2.0,
            "the half-width pair straddles the centre line"
        );
    }

    #[test]
    fn the_pause_grid_size_is_the_arranged_layouts_own() {
        // `Origin::PauseGrid` aligns the grid's *measured* size in the screen
        // rect, so that size is load-bearing for all nine rects at once — a grid
        // 2 px too wide moves every button 1 px left. `PAUSE_GRID_W`/`_H` are the
        // hand derivation (204 + 4 + 4 wide; 70 + 4 * 24 tall) and this is the
        // only place they are compared with what the port computes.
        assert_eq!(pause_grid_size(), (PAUSE_GRID_W, PAUSE_GRID_H));
        // The same numbers reached the other way, from the arranged tree rather
        // than the cache, so the cache cannot be what is agreeing with itself.
        let grid = pause_menu_grid_with(PAUSE_MENU_PADDING_TOP);
        assert_eq!((grid.width(), grid.height()), (212.0, 166.0));
        // And the grid really does hold nine drawable leaves in `PAUSE_BUTTONS`
        // order — the four icon buttons among them come from a *nested*
        // `LinearLayout`, so this is also the assertion that `visit_widgets`
        // flattens the nesting rather than yielding the row as one child.
        assert_eq!(
            layout::widget_rects(&grid).len(),
            crate::menu::nav::PAUSE_BUTTONS.len()
        );
    }

    #[test]
    fn a_changed_cell_padding_moves_every_pause_rect() {
        // #394's negative control, executed rather than described: change one
        // `LayoutSettings` padding value and the rect assertions must go red. The
        // subject is the real builder with one argument varied, not a copy of it,
        // so this cannot pass by testing something else.
        //
        // `MENU_PADDING_TOP` is row 0's `paddingTop`. Dropping it by 10 must
        // (a) move Back to Game up 10, (b) shrink the grid 10, and therefore
        // (c) move every *later* row up 10 as well — a silently no-op arrange pass
        // would fail all three.
        let real = layout::widget_rects(&pause_menu_grid_with(PAUSE_MENU_PADDING_TOP));
        let short = layout::widget_rects(&pause_menu_grid_with(PAUSE_MENU_PADDING_TOP - 10));
        assert_eq!(real[0].1, 50.0);
        assert_eq!(short[0].1, 40.0, "row 0's padding must move row 0");
        for (i, (r, s)) in real.iter().zip(&short).enumerate() {
            assert_eq!(
                r.1 - s.1,
                10.0,
                "widget {i} did not move with the row above it"
            );
            assert_eq!(r.0, s.0, "and nothing may move horizontally");
        }
        let grid = pause_menu_grid_with(PAUSE_MENU_PADDING_TOP - 10);
        assert_eq!(
            (grid.width(), grid.height()),
            (PAUSE_GRID_W, PAUSE_GRID_H - 10.0),
            "the grid's own height is the sum of its rows, so it must shrink too"
        );
    }

    #[test]
    fn death_frame_builds_vanillas_two_widgets_in_order_and_tracks_the_highlight() {
        use crate::menu::nav::{DEATH_BUTTONS, DeathButton};

        let mut nav = test_nav("death-frame");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.die(Some("was slain by a Skeleton".to_string()));
        nav.hover(&ui, 1);

        let f = death_frame(&nav, ui.death_message());
        assert!(f.overlay, "the death screen must draw as an overlay");
        assert!(f.vanilla, "and be laid out from vanilla's arithmetic");
        assert_eq!(f.rows.len(), 2, "vanilla's death screen has two widgets");
        assert_eq!(f.rows[0].label, DeathButton::Respawn.label());
        assert_eq!(f.rows[1].label, DeathButton::TitleScreen.label());
        assert!(
            f.rows.iter().all(|r| r.enabled),
            "unlike title/pause, neither death-screen button is ever disabled"
        );
        assert!(f.rows.iter().all(|r| r.slot.is_some()));
        assert_eq!(f.selected, 1, "selection follows the nav's death_index");
        assert_eq!(DEATH_BUTTONS.len(), 2);

        // The heading is a positioned label (the title), not the row stack's
        // centred title string.
        assert!(f.title.is_empty());
        // Title + message + score.
        assert_eq!(f.labels.len(), 3);
        assert_eq!(f.labels[0].text, "You Died!");
        assert_eq!(f.labels[0].scale, 2.0, "vanilla scales the title 2x");
        assert_eq!(f.labels[1].text, "was slain by a Skeleton");
        assert_eq!(f.labels[2].text, "Score: 0");
        assert!(!geometry(&f, V_W, V_H).is_empty());

        // No message: two labels, not three, and the score line still draws —
        // matching vanilla's own `if (this.causeOfDeath != null)` guard.
        let no_message = death_frame(&nav, None);
        assert_eq!(no_message.labels.len(), 2);
        assert_eq!(no_message.labels[0].text, "You Died!");
        assert_eq!(no_message.labels[1].text, "Score: 0");
    }

    #[test]
    fn the_death_screen_rects_are_vanillas_own() {
        use crate::menu::nav::DeathButton as B;
        // Hand-derived from `DeathScreen.init` (`DeathScreen.java:42-60`) at
        // 854×480: both buttons are `width/2-100, height/4+72|96, 200x20`,
        // and `height/4+72 == TitleTop.anchor().1 + 24` since `TitleTop` is
        // itself `floor(height/4) + 48` — 168 + 24 = 192, 168 + 48 = 216.
        let expected = [
            (B::Respawn, (327.0, 192.0, 200.0, 20.0)),
            (B::TitleScreen, (327.0, 216.0, 200.0, 20.0)),
        ];
        for (button, want) in expected {
            assert_eq!(
                death_slot(button).resolve(V_W, V_H),
                want,
                "{button:?} is not where vanilla puts it"
            );
        }
    }

    #[test]
    fn the_death_screens_title_is_anchored_on_the_left_quarter_not_the_centre() {
        // The trap named in `Origin::DeathTitle`'s docs: `DeathScreen.
        // visitText` draws the title at `middleLine / 2` where `middleLine ==
        // width / 2`, i.e. `width / 4` — not `width / 2` like every other
        // centred heading in this file (`Origin::ScreenTop`). A layout
        // "corrected" to the screen centre would fail this by a wide margin.
        assert_eq!(Origin::DeathTitle.anchor(V_W, V_H), (V_W / 4.0, 0.0));
        assert_ne!(
            Origin::DeathTitle.anchor(V_W, V_H).0,
            Origin::ScreenTop.anchor(V_W, V_H).0,
            "the death title and the score/message lines are not on the same x"
        );
    }

    #[test]
    fn every_vanilla_widget_is_on_screen_and_none_overlap() {
        // The layout arithmetic has to hold at more than one canvas size, and a
        // widget that lands on top of another is a hit-test that activates the
        // wrong button.
        let nav = test_nav("vanilla-rects");
        let mut ui = UiState::new();
        let statuses = StatusCache::with_probe(unavailable_probe());
        let mut fav = FaviconCache::new();
        let title = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        ui.enter_dev_world();
        ui.pause();
        let pause = pause_frame(&nav);
        ui.enter_dev_world();
        ui.die(Some("fell from a high place".to_string()));
        let death = death_frame(&nav, ui.death_message());

        for (name, frame) in [("title", &title), ("pause", &pause), ("death", &death)] {
            // 320×240 is the smallest canvas `calculate_gui_scale` will produce
            // (see `config.rs`'s MIN_SCALED_*), so it is the real lower bound.
            for (w, h) in [(320.0f32, 240.0f32), (V_W, V_H), (1280.0, 720.0)] {
                let rects: Vec<(f32, f32, f32, f32)> = (0..frame.rows.len())
                    .map(|i| row_rect(&frame.rows, i, w, h).expect("a slotted row has a rect"))
                    .collect();
                for (i, r) in rects.iter().enumerate() {
                    assert!(
                        r.0 >= 0.0 && r.0 + r.2 <= w,
                        "{name} widget {i} off-screen horizontally at {w}x{h}: {r:?}"
                    );
                    assert!(
                        r.1 >= 0.0 && r.1 + r.3 <= h,
                        "{name} widget {i} off-screen vertically at {w}x{h}: {r:?}"
                    );
                }
                for (i, a) in rects.iter().enumerate() {
                    for (j, b) in rects.iter().enumerate().skip(i + 1) {
                        let overlap = a.0 < b.0 + b.2
                            && b.0 < a.0 + a.2
                            && a.1 < b.1 + b.3
                            && b.1 < a.1 + a.3;
                        assert!(
                            !overlap,
                            "{name} widgets {i} and {j} overlap at {w}x{h}: {a:?} {b:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn an_overlay_frames_backdrop_is_translucent_unlike_an_ordinary_menus() {
        // The whole point of `MenuFrame::overlay`: the paused world underneath
        // must stay visible, which only holds if the backdrop quad's alpha is
        // measurably below opaque. A negative control (an ordinary, non-overlay
        // frame) proves the opaque case still exists and this isn't just
        // measuring `geometry`'s general output.
        let nav = test_nav("pause-overlay-alpha");
        let overlay = pause_frame(&nav);
        let v = geometry(&overlay, 1280.0, 720.0);
        // The backdrop is the very first quad emitted (vertex 0..6); alpha is
        // the 4th of the 6 floats per vertex ([x, y, r, g, b, a]).
        let backdrop_alpha = v[5];
        assert!(
            backdrop_alpha < 0.9,
            "an overlay backdrop must let the world show through: alpha={backdrop_alpha}"
        );

        let ordinary = frame_with(vec![button("QUIT")], 0);
        let v2 = geometry(&ordinary, 1280.0, 720.0);
        assert!(
            (v2[5] - 1.0).abs() < f32::EPSILON,
            "a non-overlay menu's backdrop must stay opaque: alpha={}",
            v2[5]
        );
    }

    #[test]
    fn the_highlighted_pause_button_is_visibly_different_from_its_neighbours() {
        // Colour-aware, because the fill quad already covers every pixel a
        // border would: `coverage`'s "is anything here" cannot separate the
        // highlighted state from an ordinary row (see `coverage_of`'s docs).
        //
        // This is the *fallback* (no atlas) chrome — flat ROW_SEL / ROW_BG /
        // ROW_OFF fills. The real `widget/button*` sprite selection is gated
        // separately by `the_button_sprite_matches_vanillas_enabled_hovered_rule`.
        let mut nav = test_nav("pause-highlight");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.pause();

        // Options (index 7) is enabled, so it can actually be highlighted.
        nav.hover(&ui, 7);
        let (w, h) = (V_W, V_H);
        let frame = pause_frame(&nav);
        let sel = geometry(&frame, w, h);
        let mut unsel_frame = pause_frame(&nav);
        unsel_frame.selected = 99;
        let unsel = geometry(&unsel_frame, w, h);
        assert_ne!(sel, unsel, "selecting a pause row must change the geometry");

        // A strip of the button's *interior above its label*: the label's top is
        // `y + (h - 9)/2 + 1` == y+6 for a 20 px button, and the 1 px selection
        // border ends at y+1. Sampling y+2..y+4 therefore measures the fill and
        // only the fill — the first version of this test sampled the whole
        // interior and failed on the disabled row, because `colour_at` returns
        // the *topmost* quad and "Advancements" is dense enough in a 98 px button
        // to push label ink into more than 10 % of the samples.
        let inside = |i: usize| {
            let (x, y, rw, _rh) = row_rect(&frame.rows, i, w, h).expect("a slotted row has a rect");
            (x + 4.0, y + 2.0, rw - 8.0, 2.0)
        };
        assert!(
            coverage_of(&sel, w, h, inside(7), ROW_SEL) > 0.9,
            "the highlighted row is not filled with ROW_SEL: {}",
            coverage_of(&sel, w, h, inside(7), ROW_SEL)
        );
        // Negative control 1: the same rect with nothing selected is ROW_BG, and
        // carries no ROW_SEL at all.
        assert!(
            coverage_of(&unsel, w, h, inside(7), ROW_SEL) < 0.05,
            "an unhighlighted row must not use the selected fill"
        );
        assert!(
            coverage_of(&unsel, w, h, inside(7), ROW_BG) > 0.9,
            "an unhighlighted enabled row should be filled with ROW_BG"
        );
        // Negative control 2: a *disabled* row is a third, distinct colour and
        // never picks up the selected fill even when it is the selection —
        // vanilla's `WidgetSprites::get` gives disabled priority over hovered.
        let mut on_disabled = pause_frame(&nav);
        on_disabled.selected = 1; // Advancements
        let on_disabled = geometry(&on_disabled, w, h);
        assert!(
            coverage_of(&on_disabled, w, h, inside(1), ROW_OFF) > 0.9,
            "a disabled row must keep the disabled fill even while highlighted: {}",
            coverage_of(&on_disabled, w, h, inside(1), ROW_OFF)
        );
        assert!(
            coverage_of(&on_disabled, w, h, inside(1), ROW_SEL) < 0.05,
            "a disabled row must never draw the selected fill"
        );
        // And the three colours really are distinguishable, so the three
        // assertions above are measurements and not the same one three times.
        assert_ne!(ROW_SEL, ROW_BG);
        assert_ne!(ROW_SEL, ROW_OFF);
        assert_ne!(ROW_BG, ROW_OFF);
    }

    /// A synthetic pack carrying just the three `widget/button*` sprites, each a
    /// different size so its atlas region is identifiable, and each with a
    /// **different nine-slice border** in its `.mcmeta` — 3 / 3 / 1, exactly the
    /// real 26.2 pack's values, which is what lets a test tell "border read from
    /// the pack" apart from "border hardcoded to 3".
    #[cfg(test)]
    fn button_pack() -> lodestone_assets::ResourceManager {
        use lodestone_assets::{MemorySource, ResourceSource};
        let mut src = MemorySource::default();
        for (id, border) in [
            ("widget/button", 3u32),
            ("widget/button_highlighted", 3),
            ("widget/button_disabled", 1),
        ] {
            src.insert(
                format!("assets/minecraft/textures/gui/sprites/{id}.png"),
                solid_rgba_png(200, 20, [10, 20, 30, 255]),
            );
            src.insert(
                format!("assets/minecraft/textures/gui/sprites/{id}.png.mcmeta"),
                format!(
                    r#"{{"gui":{{"scaling":{{"type":"nine_slice","width":200,"height":20,"border":{border}}}}}}}"#
                )
                .into_bytes(),
            );
        }
        // A 15×15 icon, so the icon-button path has something to draw too.
        src.insert(
            "assets/minecraft/textures/gui/sprites/icon/language.png",
            solid_rgba_png(15, 15, [90, 200, 90, 255]),
        );
        lodestone_assets::ResourceManager::new(vec![Box::new(src) as Box<dyn ResourceSource>])
    }

    /// The atlas rect of a sprite id, in normalised UVs — the ground truth a
    /// "which sprite was sampled" assertion compares against.
    fn sprite_uv_bounds(atlas: &GuiAtlas, id: &str) -> ([f32; 2], [f32; 2]) {
        let loc: lodestone_assets::ResourceLocation =
            format!("minecraft:gui/sprites/{id}").parse().expect("location");
        let s = atlas.atlas().sprite(&loc).expect("sprite placed");
        let (aw, ah) = (atlas.atlas().width as f32, atlas.atlas().height as f32);
        (
            [s.x as f32 / aw, s.y as f32 / ah],
            [
                (s.x + s.width) as f32 / aw,
                (s.y + s.height) as f32 / ah,
            ],
        )
    }

    /// Whether every sprite-stream vertex's UV lies inside `(min, max)`.
    fn all_uvs_within(sprite: &[f32], min: [f32; 2], max: [f32; 2]) -> bool {
        !sprite.is_empty()
            && sprite.chunks_exact(SPRITE_FLOATS_PER_VERTEX).all(|v| {
                v[2] >= min[0] - 1e-6
                    && v[2] <= max[0] + 1e-6
                    && v[3] >= min[1] - 1e-6
                    && v[3] <= max[1] + 1e-6
            })
    }

    /// The **destination** bounding box of every sprite-stream vertex, back in
    /// logical pixels — the inverse of `Quads::rect`'s
    /// `(2x/w - 1, 1 - 2y/h)`.
    ///
    /// This is what turns "a sprite was drawn" into "a sprite was drawn *there*",
    /// and it reports a box rather than a fraction so a failure says where
    /// (`CLAUDE.md`: a gate that reports only a percentage cannot tell a shifted
    /// widget from a missing one). `GuiAtlas::geometry`'s quads "tile the target
    /// exactly, with no gaps or overlap", so for an integral rect this *is* the
    /// rect — but the round trip through NDC and back costs a few `f32` ulps
    /// (`327` can come back as `326.99997`), so callers compare within a hundredth
    /// of a pixel rather than with `assert_eq!`. Two orders of magnitude below the
    /// one pixel a real layout error moves something by.
    fn sprite_dest_bounds(sprite: &[f32], w: f32, h: f32) -> (f32, f32, f32, f32) {
        assert!(!sprite.is_empty(), "no sprite quads to measure");
        let (mut x0, mut y0) = (f32::MAX, f32::MAX);
        let (mut x1, mut y1) = (f32::MIN, f32::MIN);
        for v in sprite.chunks_exact(SPRITE_FLOATS_PER_VERTEX) {
            let px = (v[0] + 1.0) * 0.5 * w;
            let py = (1.0 - v[1]) * 0.5 * h;
            x0 = x0.min(px);
            y0 = y0.min(py);
            x1 = x1.max(px);
            y1 = y1.max(py);
        }
        (x0, y0, x1 - x0, y1 - y0)
    }

    /// Whether **any** emitted quad's UV *centre* lies strictly inside
    /// `(min, max)`.
    ///
    /// Centres, not vertices: the atlas packs sprites edge to edge, so a
    /// neighbouring sprite's quad has vertices exactly *on* this region's
    /// boundary. The first version of the icon test tested vertices and its
    /// negative control failed — correctly — because a button-background quad
    /// shares an edge with the icon's region.
    fn any_quad_centre_in(sprite: &[f32], min: [f32; 2], max: [f32; 2]) -> bool {
        sprite
            .chunks_exact(SPRITE_FLOATS_PER_VERTEX * 6)
            .any(|q| {
                let (u0, v0) = (q[2], q[3]);
                let (u1, v1) = (q[SPRITE_FLOATS_PER_VERTEX * 4 + 2], q[SPRITE_FLOATS_PER_VERTEX * 4 + 3]);
                let (cu, cv) = ((u0 + u1) * 0.5, (v0 + v1) * 0.5);
                cu > min[0] && cu < max[0] && cv > min[1] && cv < max[1]
            })
    }

    #[test]
    fn the_button_sprite_matches_vanillas_enabled_hovered_rule() {
        // `WidgetSprites::get(enabled, focused)` with `AbstractButton`'s
        // three-argument set (`AbstractButton.java:18-22`,
        // `WidgetSprites.java:15-25`): enabled+hovered → highlighted,
        // enabled → button, and **disabled wins over hovered** → disabled.
        //
        // The assertion is on *which atlas region the UVs sample*, not on "a
        // quad appeared" — the three states all cover the same pixels, so
        // presence alone cannot tell them apart.
        let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
        let one = |enabled: bool, selected: bool| {
            let rows = vec![MenuRow {
                label: "Options...".into(),
                enabled,
                slot: Some(Slot {
                    origin: Origin::ScreenTop,
                    dx: -100.0,
                    dy: 40.0,
                    w: 200.0,
                    h: 20.0,
                }),
                ..Default::default()
            }];
            let mut f = frame_with(rows, if selected { 0 } else { 99 });
            f.vanilla = true;
            build(&f, Some(&atlas), None, V_W, V_H).sprite
        };

        let plain = sprite_uv_bounds(&atlas, "widget/button");
        let hover = sprite_uv_bounds(&atlas, "widget/button_highlighted");
        let off = sprite_uv_bounds(&atlas, "widget/button_disabled");
        // The three regions must be disjoint, or "sampled inside X" proves
        // nothing. Different sizes are not enough; check the packer actually
        // separated them.
        for (a, b) in [(plain, hover), (plain, off), (hover, off)] {
            assert!(
                a.1[0] <= b.0[0] || b.1[0] <= a.0[0] || a.1[1] <= b.0[1] || b.1[1] <= a.0[1],
                "two button sprites share atlas space: {a:?} {b:?}"
            );
        }

        assert!(
            all_uvs_within(&one(true, false), plain.0, plain.1),
            "an idle enabled button must sample widget/button"
        );
        assert!(
            all_uvs_within(&one(true, true), hover.0, hover.1),
            "a hovered enabled button must sample widget/button_highlighted"
        );
        assert!(
            all_uvs_within(&one(false, true), off.0, off.1),
            "a hovered DISABLED button must still sample widget/button_disabled"
        );
        // The control that makes the last one a real measurement: the same
        // hovered flag on an *enabled* button does not sample the disabled
        // sprite, so the assertion is not passing because everything does.
        assert!(
            !all_uvs_within(&one(true, true), off.0, off.1),
            "the detector cannot tell the disabled sprite apart"
        );
        // And with no atlas there is no sprite stream at all — the jar-less
        // path, which is why the flat-fill fallback exists.
        let rows = vec![MenuRow {
            label: "Options...".into(),
            enabled: true,
            slot: Some(Slot {
                origin: Origin::ScreenTop,
                dx: -100.0,
                dy: 40.0,
                w: 200.0,
                h: 20.0,
            }),
            ..Default::default()
        }];
        let mut f = frame_with(rows, 0);
        f.vanilla = true;
        let bare = build(&f, None, None, V_W, V_H);
        assert!(bare.sprite.is_empty(), "no atlas must mean no sprite quads");
        assert!(
            bare.colour.len() > bare.backdrop_floats,
            "and the flat fallback must still draw the button"
        );
    }

    #[test]
    fn every_title_and_pause_widget_draws_the_sprite_the_widget_layer_picks() {
        use crate::menu::nav::{MAIN_BUTTONS, PAUSE_BUTTONS};

        // The island this rules out is the one #393 could most easily have
        // landed: `menu/widget.rs` compiles, its own tests are green, and
        // `draw_widget` keeps a private three-way `if` — so the widget layer is
        // dead code while every existing gate still passes.
        //
        // The expected sprite here is produced by `WidgetSprites::get`
        // (`menu::widget`), never spelled out, and the measurement is *which
        // atlas region the frame's own UVs sample*. So a `draw_widget` that
        // stopped consulting the widget would have to keep agreeing with
        // vanilla's rule by coincidence, for all 36 (button, focused) pairs, to
        // pass — and if the rule in `widget.rs` is wrong, this fails too.
        // #394 extends it in the other direction, without new machinery: each
        // case is now drawn at that button's **own** slot, and the sprite's
        // destination rect is asserted against it. `title_slot`/`pause_slot` read
        // the arranged layout tree since #394, so this is also the gate that says
        // the layout containers reach pixels — an arrange pass that silently
        // no-opped would put every widget at the block's origin and fail here
        // while every "a button drew something" check still passed.
        let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
        // Both real screens' real button states and real rects, labelled so a
        // failure names the button rather than an index. `icon: None` throughout:
        // the synthetic pack carries one icon sprite, and an icon quad would put a
        // second region in the stream and make `all_uvs_within` a weaker question
        // (it would not disturb `sprite_dest_bounds`, which the icon sits inside).
        let cases: Vec<(&'static str, bool, Slot)> = MAIN_BUTTONS
            .iter()
            .map(|b| (b.label(), b.enabled(), title_slot(*b)))
            .chain(
                PAUSE_BUTTONS
                    .iter()
                    .map(|b| (b.label(), b.enabled(), pause_slot(*b))),
            )
            .collect();
        // The premise, checked rather than assumed: both screens really do carry
        // a mix, or "the disabled sprite was chosen" is never exercised.
        assert!(
            cases.iter().any(|(_, e, _)| *e) && cases.iter().any(|(_, e, _)| !*e),
            "neither screen has a disabled button any more, so this gate is vacuous"
        );
        // And the rects are really distinct, or the position half of this gate is
        // satisfied by every widget landing in one place.
        let distinct: std::collections::BTreeSet<(i32, i32)> = cases
            .iter()
            .map(|(_, _, s)| {
                let (x, y, ..) = s.resolve(V_W, V_H);
                (x as i32, y as i32)
            })
            .collect();
        assert_eq!(
            distinct.len(),
            cases.len(),
            "two buttons share a position, so a widget stuck at the wrong one \
             could still pass"
        );

        for (label, enabled, slot) in cases {
            for focused in [false, true] {
                let rows = vec![MenuRow {
                    label: label.to_string(),
                    enabled,
                    slot: Some(slot),
                    ..Default::default()
                }];
                let mut f = frame_with(rows, if focused { 0 } else { 99 });
                f.vanilla = true;
                let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;

                let expected = widget::BUTTON_SPRITES.get(enabled, focused);
                let (min, max) = sprite_uv_bounds(&atlas, expected);
                assert!(
                    all_uvs_within(&sprite, min, max),
                    "{label} (enabled={enabled}, focused={focused}) did not sample \
                     {expected}, which is what WidgetSprites::get selects"
                );
                // The control for each case: flipping `active` must move the
                // sample off this region, so "inside {expected}" is a real
                // discriminator and not something every render satisfies.
                let flipped = widget::BUTTON_SPRITES.get(!enabled, focused);
                if flipped != expected {
                    let (fmin, fmax) = sprite_uv_bounds(&atlas, flipped);
                    assert!(
                        !all_uvs_within(&sprite, fmin, fmax),
                        "the detector cannot tell {expected} from {flipped}"
                    );
                }

                // Where it drew, in logical pixels, against the layout's own
                // answer for this button. The 0.01 is the NDC round trip's float
                // error, not slack in the layout — see `sprite_dest_bounds`.
                let drawn = sprite_dest_bounds(&sprite, V_W, V_H);
                let want = slot.resolve(V_W, V_H);
                let same = [
                    (drawn.0, want.0),
                    (drawn.1, want.1),
                    (drawn.2, want.2),
                    (drawn.3, want.3),
                ]
                .iter()
                .all(|(a, b)| (a - b).abs() < 0.01);
                assert!(
                    same,
                    "{label} (enabled={enabled}, focused={focused}) drew at {drawn:?}, \
                     not at {want:?} where the layout placed it"
                );
            }
        }
    }

    #[test]
    fn nine_slice_borders_come_from_the_mcmeta_not_a_constant() {
        // `widget/button` declares `border: 3` and `widget/button_disabled`
        // declares `border: 1` in the real 26.2 pack — read straight out of
        // `client.jar`. A renderer that hardcoded one border would draw the
        // disabled button's corners three times too large, which is exactly the
        // subtle wrongness the brief warned about.
        //
        // The synthetic pack repeats those two values, so the corner quad's own
        // destination size is the discriminator.
        let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
        let corner_size = |id: &str| {
            // Drawn far wider than native so every nine-slice piece appears.
            let quads = atlas.geometry(id, 0.0, 0.0, 400.0, 60.0);
            assert!(quads.len() >= 9, "{id} did not decompose: {}", quads.len());
            // The top-left piece is the one at the draw origin.
            let tl = quads
                .iter()
                .find(|q| q.dst[0] == 0.0 && q.dst[1] == 0.0)
                .expect("a nine-slice has a top-left corner");
            (tl.dst[2], tl.dst[3])
        };
        assert_eq!(corner_size("widget/button"), (3.0, 3.0));
        assert_eq!(
            corner_size("widget/button_disabled"),
            (1.0, 1.0),
            "the disabled sprite's border must come from its own .mcmeta"
        );
    }

    #[test]
    fn a_disabled_label_is_drawn_in_vanillas_grey_and_an_enabled_one_in_white() {
        // `AbstractWidget.WithInactiveMessage.defaultInactiveMessage` recolours
        // an inactive widget's message to `-6250336` == `0xFFA0A0A0`
        // (`AbstractWidget.java:314-335`). Assert the actual colour, with the
        // enabled case as the control.
        let slot = Slot {
            origin: Origin::ScreenTop,
            dx: -100.0,
            dy: 40.0,
            w: 200.0,
            h: 20.0,
        };
        let render = |enabled: bool| {
            let rows = vec![MenuRow {
                label: "MMMM".into(),
                enabled,
                slot: Some(slot),
                ..Default::default()
            }];
            let mut f = frame_with(rows, 99);
            f.vanilla = true;
            build(&f, None, None, V_W, V_H).colour
        };
        let (w, h) = (V_W, V_H);
        let (x, y, rw, rh) = slot.resolve(w, h);
        // Sample the label band across the middle of the button.
        let band = (x + rw * 0.3, y + rh * 0.3, rw * 0.4, rh * 0.4);
        let off = render(false);
        let on = render(true);
        assert!(
            coverage_of(&off, w, h, band, widget::INACTIVE_LABEL) > 0.02,
            "no grey label ink in a disabled button's rect: {}",
            coverage_of(&off, w, h, band, widget::INACTIVE_LABEL)
        );
        assert_eq!(
            coverage_of(&off, w, h, band, LABEL),
            0.0,
            "a disabled label must not be drawn in the enabled colour"
        );
        assert!(
            coverage_of(&on, w, h, band, LABEL) > 0.02,
            "no white label ink in an enabled button's rect: {}",
            coverage_of(&on, w, h, band, LABEL)
        );
        assert_eq!(
            coverage_of(&on, w, h, band, widget::INACTIVE_LABEL),
            0.0,
            "an enabled label must not be drawn grey"
        );
        // The colour under test comes from the widget layer, and *that* is
        // checked against vanilla's signed ARGB integer by
        // `widget::tests::vanillas_inactive_grey_is_derived_not_transcribed`
        // rather than being restated here. What this line pins is that the two
        // files still agree: the draw grey is the widget grey.
        assert_eq!(
            widget::INACTIVE_LABEL,
            widget::argb_to_rgba(widget::INACTIVE_MESSAGE_ARGB),
            "vanilla's -6250336 is 0xFFA0A0A0"
        );
    }

    #[test]
    fn an_icon_button_draws_its_sprite_and_no_label() {
        // Vanilla's `SpriteIconButton.CenteredIcon` draws the button background
        // plus a 15×15 sprite centred in it, and no text
        // (`SpriteIconButton.java:236-244`).
        let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
        let slot = Slot {
            origin: Origin::ScreenTop,
            dx: -10.0,
            dy: 40.0,
            w: 20.0,
            h: 20.0,
        };
        let row = |icon: Option<&'static str>| MenuRow {
            label: "Language...".into(),
            enabled: false,
            slot: Some(slot),
            icon,
            ..Default::default()
        };
        let render = |icon: Option<&'static str>| {
            let mut f = frame_with(vec![row(icon)], 99);
            f.vanilla = true;
            build(&f, Some(&atlas), None, V_W, V_H)
        };

        let icon = render(Some("icon/language"));
        let bare = render(None);
        let icon_uv = sprite_uv_bounds(&atlas, "icon/language");
        assert!(
            any_quad_centre_in(&icon.sprite, icon_uv.0, icon_uv.1),
            "the icon sprite never reached the sprite stream"
        );
        // The control: without the icon, nothing samples that atlas region.
        assert!(
            !any_quad_centre_in(&bare.sprite, icon_uv.0, icon_uv.1),
            "the detector matches the button background too"
        );
        // And it is exactly one extra quad, drawn at the centred 15×15 rect —
        // both variants draw the same nine-slice background.
        assert_eq!(
            icon.sprite.len() - bare.sprite.len(),
            SPRITE_FLOATS_PER_VERTEX * 6,
            "an icon button should add exactly one quad"
        );
        // And an icon button draws no label ink: with the icon set, the only
        // colour quads are the backdrop.
        assert_eq!(
            icon.colour.len(),
            icon.backdrop_floats,
            "an icon button must draw no text"
        );
        assert!(
            bare.colour.len() > bare.backdrop_floats,
            "but the same row *with* a label does draw text"
        );
    }

    #[test]
    fn the_pause_overlays_backdrop_is_vanillas_measured_black_at_alpha_64() {
        // `inworld_menu_background.png` decoded out of the real `client.jar` is
        // 16×16 greyscale+alpha with every pixel grey 0 / alpha 64
        // (`Screen.java:405,418-419` tiles it at 32 px). This pins the exact
        // value rather than "translucent enough".
        let nav = test_nav("overlay-exact");
        let v = geometry(&pause_frame(&nav), V_W, V_H);
        assert_eq!(&v[2..6], &[0.0, 0.0, 0.0, 64.0 / 255.0]);
    }

    #[test]
    #[ignore = "requires the vanilla pack (client.jar) under .cache/mc/<ver>"]
    fn every_sprite_id_the_vanilla_screens_name_exists_in_the_real_pack() {
        use crate::menu::nav::{MAIN_BUTTONS, PAUSE_BUTTONS};

        // The island this rules out: a mistyped sprite id draws *nothing*, and
        // every layout assertion above still passes because they use a synthetic
        // pack whose ids are the same strings the test itself wrote. Only the
        // real jar can say whether `pause_menu/social_interactions` is spelled
        // right.
        let atlas = crate::resources::load_menu_gui_atlas().expect(
            "no vanilla pack found; set LODESTONE_ASSETS to a root with client.jar",
        );
        // Every id the widget layer can select, taken from the record itself
        // rather than relisted — so a sprite added to `WidgetSprites` is covered
        // here the day it exists.
        let button_ids = [
            widget::BUTTON_SPRITES.enabled,
            widget::BUTTON_SPRITES.disabled,
            widget::BUTTON_SPRITES.enabled_focused,
            widget::BUTTON_SPRITES.disabled_focused,
        ];
        for id in button_ids {
            assert!(atlas.contains(id), "the pack has no {id}");
            assert_eq!(
                atlas.native_size(id),
                Some((200, 20)),
                "{id} is not the 200x20 its .mcmeta declares"
            );
        }
        for icon in MAIN_BUTTONS
            .iter()
            .filter_map(|b| b.icon())
            .chain(PAUSE_BUTTONS.iter().filter_map(|b| b.icon()))
        {
            assert!(atlas.contains(icon), "the pack has no icon sprite {icon}");
            assert!(atlas.native_size(icon).is_some(), "{icon} was not placed");
            // Deliberately *no* assertion on the native size, and this is a
            // belief that was held and measured false. "Vanilla's icon-button
            // sprites are 15×15" is true of every **blit** (`spriteWidth`/
            // `spriteHeight` are 15 at each call site — `CommonButtons.java:10,21`,
            // `FriendsButton.java:22`, `PauseScreen.java:104,115,134`) and true
            // of almost none of the **files**. Measured out of the real 26.2 jar:
            //
            //   icon/language 15×15, icon/accessibility 15×15,
            //   friends/friends 16×16, pause_menu/bug 13×13,
            //   pause_menu/social_interactions 20×20,
            //   pause_menu/player_reporting 15×14
            //
            // They are all `Stretch` (no `.mcmeta`), so vanilla scales each to
            // 15×15 — including *up* from 13 and *down* from 20. Two successive
            // versions of this gate asserted a native size and were failed by
            // `friends/friends` and then `pause_menu/bug`. Drawing at
            // [`ICON_SPRITE`] is what matches vanilla; the file size is not
            // something to check against.
        }
        // The two loose title textures, and their *declared* (not native) size:
        // 26.2 ships them at 4x, which is why the draw rect is 256x64 / 128x16.
        assert_eq!(atlas.native_size("title/minecraft"), Some((1024, 256)));
        assert_eq!(atlas.native_size("title/edition"), Some((512, 64)));

        // The real pack's nine-slice borders, which is where the hardcoding trap
        // is: 3 for button and button_highlighted, **1** for button_disabled.
        let corner = |id: &str| {
            let q = atlas.geometry(id, 0.0, 0.0, 400.0, 60.0);
            let tl = q
                .iter()
                .find(|q| q.dst[0] == 0.0 && q.dst[1] == 0.0)
                .expect("nine-slice top-left");
            (tl.dst[2], tl.dst[3])
        };
        assert_eq!(corner(widget::BUTTON_SPRITES.enabled), (3.0, 3.0));
        assert_eq!(corner(widget::BUTTON_SPRITES.enabled_focused), (3.0, 3.0));
        assert_eq!(corner(widget::BUTTON_SPRITES.disabled), (1.0, 1.0));

        // And the whole title frame draws through it: every sprite the two
        // screens ask for resolves to at least one quad.
        let nav = test_nav("real-pack");
        let mut ui = UiState::new();
        let statuses = StatusCache::with_probe(unavailable_probe());
        let mut fav = FaviconCache::new();
        let title = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        let geo = build(&title, Some(&atlas), None, V_W, V_H);
        // 9 nine-slice backgrounds (the 8 vanilla widgets plus the
        // non-vanilla `Accounts` row — see `MainButton::Accounts`) + 3 icons
        // + 2 logo quads, so comfortably more than one quad per widget, and
        // *nothing* on the flat-fill path.
        assert!(
            geo.sprite.len() / (SPRITE_FLOATS_PER_VERTEX * 6) > MAIN_BUTTONS.len(),
            "only {} sprite quads for {} widgets plus the logo",
            geo.sprite.len() / (SPRITE_FLOATS_PER_VERTEX * 6),
            MAIN_BUTTONS.len()
        );
        assert_eq!(
            geo.colour.len(),
            geo.backdrop_floats
                + geometry(&title, V_W, V_H).len()
                - geometry_button_fill_floats(&title, V_W, V_H)
                - geo.backdrop_floats,
            "with a real atlas no button may fall back to a flat fill"
        );

        ui.enter_dev_world();
        ui.pause();
        let pause = build(&pause_frame(&nav), Some(&atlas), None, V_W, V_H);
        assert!(
            pause.sprite.len() / (SPRITE_FLOATS_PER_VERTEX * 6) > PAUSE_BUTTONS.len(),
            "the pause screen's nine widgets did not all draw a sprite"
        );
    }

    /// Floats the flat-fill fallback would contribute for `frame`'s slotted rows
    /// (one quad each, plus a 4-quad outline for the selected one) — the term the
    /// real-pack gate subtracts to say "no button fell back".
    fn geometry_button_fill_floats(frame: &MenuFrame<'_>, _w: f32, _h: f32) -> usize {
        let slotted = frame.rows.iter().filter(|r| r.slot.is_some()).count();
        let selected = frame
            .rows
            .get(frame.selected)
            .is_some_and(|r| r.slot.is_some()) as usize;
        (slotted + selected * 4) * STRIDE * 6
    }

    /// A real single-colour PNG of arbitrary dimensions. `solid_png` below is
    /// square-only and is what the favicon tests want; the button pack needs
    /// 200×20.
    fn solid_rgba_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, w, h);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().expect("write header");
            let data: Vec<u8> = (0..w * h).flat_map(|_| rgba).collect();
            writer.write_image_data(&data).expect("write image");
        }
        out
    }

    #[test]
    fn logical_canvas_shrinks_a_retina_style_framebuffer_back_to_visual_size() {
        // A 2x HiDPI display reports a framebuffer double an ordinary window's
        // physical size for the same visual window. Auto scale must pick up
        // roughly double the scale too, so the logical canvas (what `geometry`
        // actually lays fixed pixel constants into) lands close to the same
        // apparent size in both cases — this is the fix for the "menu draws
        // half-size on Retina" report.
        let lo_dpi = logical_canvas(0, 1280, 720);
        let hi_dpi = logical_canvas(0, 2560, 1440);
        // Not a no-op: the canvas must actually shrink relative to the raw
        // framebuffer, or this is the exact island the change was for.
        assert!(hi_dpi.0 < 2560.0 && hi_dpi.1 < 1440.0);
        // And the two logical canvases must be close in size, not 2x apart,
        // which is what "half size on Retina" looked like before this existed.
        assert!(
            (lo_dpi.0 - hi_dpi.0).abs() < lo_dpi.0 * 0.5,
            "logical canvases diverged: {lo_dpi:?} vs {hi_dpi:?}"
        );
    }

    #[test]
    fn logical_canvas_is_the_identity_at_scale_one() {
        // A tiny framebuffer forces scale 1 (see `config`'s own tests), at
        // which point the logical canvas must equal the physical one exactly —
        // this is what keeps every fixed-size `geometry` test above valid.
        assert_eq!(logical_canvas(0, 200, 200), (200.0, 200.0));
    }

    #[test]
    fn logical_canvas_never_divides_by_zero_for_a_degenerate_framebuffer() {
        let (w, h) = logical_canvas(0, 0, 0);
        assert!(w.is_finite() && h.is_finite());
    }

    #[test]
    fn a_narrow_viewport_does_not_produce_out_of_range_geometry() {
        // Small windows are where layout arithmetic goes negative.
        for (w, h) in [(320.0f32, 240.0f32), (200.0, 900.0), (1.0, 1.0)] {
            let rows = vec![button("ONE"), button("TWO")];
            let v = geometry(&frame_with(rows, 0), w, h);
            for vert in v.chunks_exact(STRIDE) {
                assert!(
                    vert[0].is_finite() && vert[1].is_finite(),
                    "non-finite vertex at {w}x{h}"
                );
            }
        }
    }

    #[test]
    fn an_empty_menu_still_clears_the_screen() {
        // Otherwise the last world frame stays on screen behind a blank menu.
        let f = frame_with(vec![], 0);
        let v = geometry(&f, 1280.0, 720.0);
        assert!(
            v.len() >= STRIDE * 6,
            "an empty menu must still emit the background"
        );
    }


    // -- world select (issue #397) --------------------------------------------

    /// A nav and a `UiState` sitting on the world-select screen, reached the way
    /// a player reaches it: by activating the title screen's Singleplayer button.
    ///
    /// That is the anti-island premise for this whole screen — if the button no
    /// longer opens it, every test below fails at this assertion rather than
    /// quietly testing a screen nothing can reach.
    fn world_select_nav(tag: &str) -> (MenuNav, UiState) {
        let mut nav = test_nav(tag);
        let mut ui = UiState::new();
        assert_eq!(
            nav.main_button(),
            crate::menu::nav::MainButton::Singleplayer,
            "premise: Singleplayer is the initially selected title-screen button"
        );
        let action = nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(action, crate::menu::nav::MenuAction::None);
        assert_eq!(
            ui.screen(),
            Screen::WorldSelect,
            "the title screen's Singleplayer button must open the world list"
        );
        (nav, ui)
    }

    fn world_select_frame(nav: &MenuNav, ui: &UiState) -> MenuFrame<'static> {
        let statuses = StatusCache::with_probe(unavailable_probe());
        let mut fav = FaviconCache::new();
        frame_for(ui, nav, &statuses, &mut fav).expect("the world list owns its frame")
    }

    /// Vanilla's own rects for `SelectWorldScreen`, hand-derived from the Java at
    /// 854×480 rather than read back out of the layout — `CLAUDE.md`'s rule that
    /// an expected value must originate outside the code under test.
    ///
    /// The derivation, which is what a future reader has to be able to check:
    ///
    /// - The header column is `LinearLayout.vertical().spacing(4)` holding a 9 px
    ///   `StringWidget` and a nested 200×20 row, so it measures 200×33 and the
    ///   header `FrameLayout` (854×49, `align(0.5, 0.5)`) puts it at
    ///   `((854-200)/2, (49-33)/2)` = (327, 8). The search box is one spacing plus
    ///   the title below that: y = 8 + 9 + 4 = **21**, *not* the 22 written at
    ///   `SelectWorldScreen.java:55`, because the layout overwrites it.
    /// - The footer's four columns are all 71: Play's 150 px spanning two columns
    ///   with an 8 px gutter splits `Divisor(142, 2)` = 71/71, and the four 71 px
    ///   buttons can only match it. So the grid is `4*71 + 3*8` = **308** wide and
    ///   `20 + 4 + 20` = 44 tall, and the footer frame (854×60, pinned at y 420)
    ///   puts it at `((854-308)/2, 420 + (60-44)/2)` = (273, 428).
    /// - Within it: row 1 cells start at 0 and 158 (`71+8+71+8`), row 2 cells at
    ///   0, 79, 158, 237, and row 2 is 24 px down.
    /// - The content band's top is `min(headerHeight + 30, height - footerHeight -
    ///   contentHeight)` = `min(79, 480 - 60 - 371)` = **49**, i.e. flush under the
    ///   header, because vanilla sizes the list to `getContentHeight()` exactly.
    /// - The first list row is at `getY() + 2` = 51, 270 wide
    ///   (`getRowWidth()`), 36 tall, centred: `427 - 135` = 292.
    #[test]
    fn the_world_select_rects_are_vanillas_own() {
        use crate::menu::world_select::WorldSelectButton as B;
        let expected = [
            (B::Play, (273.0, 428.0, 150.0, 20.0)),
            (B::Create, (431.0, 428.0, 150.0, 20.0)),
            (B::Edit, (273.0, 452.0, 71.0, 20.0)),
            (B::Delete, (352.0, 452.0, 71.0, 20.0)),
            (B::ReCreate, (431.0, 452.0, 71.0, 20.0)),
            (B::Back, (510.0, 452.0, 71.0, 20.0)),
        ];
        for (button, want) in expected {
            assert_eq!(
                world_select_slot(button).resolve(V_W, V_H),
                want,
                "{button:?} is not where vanilla puts it"
            );
        }
        // The footer's 8 px gutter, which is the pause screen's and not the title
        // screen's 4 — the same conflation `the_title_screen_rects_are_vanillas_own`
        // pins from the other side.
        let (ex, _, ew, _) = world_select_slot(B::Edit).resolve(V_W, V_H);
        let (dx, ..) = world_select_slot(B::Delete).resolve(V_W, V_H);
        assert_eq!(dx - (ex + ew), 8.0, "footer column gutter");

        assert_eq!(
            world_select_search_slot().resolve(V_W, V_H),
            (327.0, 21.0, 200.0, 20.0),
            "the search box is placed by the layout, not by its own constructor"
        );
        let title = world_select_title_label();
        assert_eq!(
            (title.origin.anchor(V_W, V_H).0 + title.dx, title.dy),
            (427.0, 8.0),
            "the title is centred at the top of the header band"
        );
        assert_eq!(title.align, Align::Centre);

        assert_eq!(world_list_row_rect(0, V_W), (292.0, 51.0, 270.0, 36.0));
        assert_eq!(
            world_list_row_rect(1, V_W),
            (292.0, 87.0, 270.0, 36.0),
            "rows stack by itemHeight with no gap"
        );
        assert_eq!(
            world_list_row_content_rect(0, V_W),
            (294.0, 53.0, 266.0, 32.0),
            "CONTENT_PADDING insets the entry by 2, and 36 - 4 is the icon's 32"
        );
    }

    /// The slots must be the same at every canvas, or the screen is right at one
    /// size and wrong everywhere else.
    ///
    /// This is the condition `WORLD_SELECT_REF_CANVAS` rests on, and the only
    /// thing that makes arranging a *canvas-dependent* container once legitimate.
    /// 320×240 is the real floor `config::calculate_gui_scale` can produce; the
    /// widths are even, because an odd logical width truncates in vanilla's
    /// integer centring where `Origin`'s anchor does not — the same half-pixel
    /// `title_slot` has always had.
    #[test]
    fn the_world_select_slots_do_not_depend_on_the_reference_canvas() {
        for (w, h) in [(320.0f32, 240.0f32), (854.0, 480.0), (1920.0, 1080.0)] {
            let block = WorldSelectBlock::at(w, h);
            for i in 0..2 {
                assert_eq!(
                    block.header_slot(i),
                    world_select_block().header_slot(i),
                    "header slot {i} moved at {w}x{h}"
                );
            }
            for i in 0..crate::menu::world_select::WORLD_SELECT_BUTTONS.len() {
                assert_eq!(
                    block.footer_slot(i),
                    world_select_block().footer_slot(i),
                    "footer slot {i} moved at {w}x{h}"
                );
            }
            assert_eq!(
                block.content_top,
                world_select_block().content_top,
                "the content band moved at {w}x{h}"
            );
        }
    }

    /// The frame is the screen vanilla draws: seven widgets in vanilla's order,
    /// five of them present-and-disabled, at the rects the layout placed them.
    #[test]
    fn the_world_select_frame_is_the_screen_vanilla_draws() {
        use crate::menu::world_select::{SEARCH_FIELD, WORLD_SELECT_BUTTONS, WorldSelectButton};
        let (nav, ui) = world_select_nav("ws-frame");
        let f = world_select_frame(&nav, &ui);

        assert!(f.vanilla, "it reproduces one of vanilla's own screens");
        assert!(!f.logo, "the logo is the title screen's");
        assert_eq!(f.rows.len(), 1 + WORLD_SELECT_BUTTONS.len());

        // Row 0 is the search field, and it carries a real `EditBox` — the row
        // indices are `world_select`'s focus ids, so this is also the guard that
        // `app.rs`'s hit-test and the focus layer agree about what row 0 is.
        assert!(
            f.rows[SEARCH_FIELD].field && f.rows[SEARCH_FIELD].edit.is_some(),
            "row 0 must be the search box"
        );
        assert_eq!(
            f.selected, SEARCH_FIELD,
            "setInitialFocus puts the keyboard in the search box"
        );
        assert_eq!(f.hovered, None, "nothing is hovered before the mouse moves");

        // The six footer buttons, in vanilla's order, with vanilla's labels.
        let labels: Vec<&str> = f.rows[1..].iter().map(|r| r.label.as_str()).collect();
        assert_eq!(
            labels,
            vec![
                "Play Selected World",
                "Create New World",
                "Edit",
                "Delete",
                "Re-Create",
                "Back",
            ]
        );
        // Five disabled, one enabled — the headline of #397. Create New World is
        // *present* and inactive, which is what makes the footer's shape vanilla's.
        let enabled: Vec<&str> = f.rows[1..]
            .iter()
            .filter(|r| r.enabled)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(enabled, vec!["Back"]);
        assert!(
            !f.rows[WorldSelectButton::Create.row()].enabled,
            "Create New World must be present and disabled (issue #190)"
        );

        // Every row's rect is the slot the layout placed it in, through the same
        // `row_rect` `app.rs` hit-tests with.
        assert_eq!(
            row_rect(&f.rows, SEARCH_FIELD, V_W, V_H),
            Some(world_select_search_slot().resolve(V_W, V_H))
        );
        for button in WORLD_SELECT_BUTTONS {
            assert_eq!(
                row_rect(&f.rows, button.row(), V_W, V_H),
                Some(world_select_slot(button).resolve(V_W, V_H)),
                "{button:?}'s row is not at its slot"
            );
        }

        // The two free-standing strings: the title, and the empty-list message.
        let texts: Vec<&str> = f.labels.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                crate::menu::world_select::WORLD_SELECT_TITLE,
                crate::menu::world_select::NO_WORLDS_MESSAGE,
            ]
        );
    }

    /// Every world-select button draws the sprite the widget layer picks, at the
    /// rect the layout placed it in.
    ///
    /// The same gate `every_title_and_pause_widget_draws_the_sprite_the_widget_layer_picks`
    /// makes for the other two screens, and for the same reason: without it
    /// `world_select_slot` and `WorldSelectButton::enabled` could both be correct
    /// and reach zero pixels. The `enabled` flags come from the **real frame**, so
    /// this cannot drift from what the screen actually says.
    #[test]
    fn every_world_select_button_draws_the_sprite_the_widget_layer_picks() {
        use crate::menu::world_select::WORLD_SELECT_BUTTONS;
        let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
        let (nav, ui) = world_select_nav("ws-sprites");
        let frame = world_select_frame(&nav, &ui);

        // The premise: the screen really does carry a mix, or "the disabled
        // sprite was chosen" is never exercised.
        assert!(
            frame.rows[1..].iter().any(|r| r.enabled) && frame.rows[1..].iter().any(|r| !r.enabled),
            "this screen no longer has both an enabled and a disabled button"
        );
        // And the rects are really distinct, or a widget stuck at one position
        // could still pass.
        let distinct: std::collections::BTreeSet<(i32, i32)> = WORLD_SELECT_BUTTONS
            .iter()
            .map(|b| {
                let (x, y, ..) = world_select_slot(*b).resolve(V_W, V_H);
                (x as i32, y as i32)
            })
            .collect();
        assert_eq!(distinct.len(), WORLD_SELECT_BUTTONS.len());

        for button in WORLD_SELECT_BUTTONS {
            let row = frame.rows[button.row()].clone();
            let enabled = row.enabled;
            for focused in [false, true] {
                let mut f = frame_with(vec![row.clone()], if focused { 0 } else { 99 });
                f.vanilla = true;
                let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;

                let expected = widget::BUTTON_SPRITES.get(enabled, focused);
                let (min, max) = sprite_uv_bounds(&atlas, expected);
                assert!(
                    all_uvs_within(&sprite, min, max),
                    "{button:?} (enabled={enabled}, focused={focused}) did not sample \
                     {expected}, which is what WidgetSprites::get selects"
                );
                // Per-case control: flipping `active` must move the sample off
                // this region. For the five disabled buttons this is the #397
                // assertion run in reverse — an enabled Create New World must
                // *not* sample `widget/button_disabled`.
                let flipped = widget::BUTTON_SPRITES.get(!enabled, focused);
                if flipped != expected {
                    let (fmin, fmax) = sprite_uv_bounds(&atlas, flipped);
                    assert!(
                        !all_uvs_within(&sprite, fmin, fmax),
                        "the detector cannot tell {expected} from {flipped}"
                    );
                }

                let drawn = sprite_dest_bounds(&sprite, V_W, V_H);
                let want = world_select_slot(button).resolve(V_W, V_H);
                let same = [
                    (drawn.0, want.0),
                    (drawn.1, want.1),
                    (drawn.2, want.2),
                    (drawn.3, want.3),
                ]
                .iter()
                .all(|(a, b)| (a - b).abs() < 0.01);
                assert!(
                    same,
                    "{button:?} (enabled={enabled}, focused={focused}) drew at {drawn:?}, \
                     not at {want:?} where the layout placed it"
                );
            }
        }
    }

    /// A disabled world-select button's label is vanilla's grey, and it is that
    /// exact value.
    ///
    /// Predicted, not asserted as a direction — `CLAUDE.md`'s *magnitude*
    /// species. The expectation comes from `AbstractWidget.java:318`'s
    /// `-6250336` unpacked by `widget::argb_to_rgba`, and the enabled button
    /// beside it is the control that says the measurement can tell them apart.
    #[test]
    fn a_disabled_world_select_label_lands_on_vanillas_grey() {
        use crate::menu::world_select::WorldSelectButton as B;
        let (nav, ui) = world_select_nav("ws-grey");
        let frame = world_select_frame(&nav, &ui);
        let grey = widget::argb_to_rgba(widget::INACTIVE_MESSAGE_ARGB);
        assert_eq!(grey, widget::INACTIVE_LABEL);

        for (button, want, name) in [
            (B::Create, grey, "disabled"),
            (B::Back, widget::ACTIVE_LABEL, "enabled"),
        ] {
            let row = frame.rows[button.row()].clone();
            let rect = world_select_slot(button).resolve(V_W, V_H);
            let mut f = frame_with(vec![row], 99);
            f.vanilla = true;
            let colour = build(&f, None, None, V_W, V_H).colour;
            assert!(
                coverage_of(&colour, V_W, V_H, rect, want) > 0.0,
                "{button:?}'s {name} label did not reach {want:?} inside {rect:?}"
            );
            // The control: the *other* colour must not appear in the same rect,
            // or "the label is grey" is satisfied by a frame containing both.
            let other = if want == grey {
                widget::ACTIVE_LABEL
            } else {
                grey
            };
            assert_eq!(
                coverage_of(&colour, V_W, V_H, rect, other),
                0.0,
                "{button:?} drew {other:?} as well, so the colour is not a discriminator"
            );
        }
    }

    /// The empty list draws its message, inside row 0's own content rect.
    ///
    /// This is the assertion that keeps "there are no worlds" distinguishable
    /// from "the list failed to draw" — without it the two are the same picture,
    /// which is exactly the absence-needs-a-control rule. The band is the row's
    /// content rect from `world_list_row_content_rect`, the same expression the
    /// label's position is derived from, and the failure output is a bounding box
    /// rather than a fraction.
    ///
    /// Two controls, both executed: the band *below* the message must be empty
    /// (so this is not measuring a frame that paints everywhere), and the same
    /// band on the **title screen** must be empty too (so it is not measuring
    /// something every menu draws there).
    #[test]
    fn the_empty_world_list_draws_its_message_inside_row_zeros_content_rect() {
        let (nav, ui) = world_select_nav("ws-empty");
        let frame = world_select_frame(&nav, &ui);
        let colour = geometry(&frame, V_W, V_H);

        let band = world_list_row_content_rect(0, V_W);
        let inside = band_coverage(&colour, V_W, V_H, band);
        assert!(
            inside.count > 0,
            "the empty-list message reached no pixels inside {band:?}"
        );
        let bounds = inside.bounds.expect("a non-empty band has bounds");
        // It is a line of text, not a full-height fill: the message is 9 px of
        // glyphs centred in a 32 px box, so its vertical extent must be well
        // short of the band's.
        assert!(
            bounds.3 - bounds.1 < band.3 * 0.75,
            "what drew in {band:?} spans {:?} vertically — that is a fill, not a line of text",
            (bounds.1, bounds.3)
        );
        // And it is centred, so it must straddle the screen's own centre line.
        assert!(
            bounds.0 < V_W * 0.5 && bounds.2 > V_W * 0.5,
            "the message is not centred: bounds {bounds:?}"
        );

        // -- control 1: the row below it is empty ----------------------------
        let empty_band = world_list_row_content_rect(1, V_W);
        assert_eq!(
            band_coverage(&colour, V_W, V_H, empty_band).count,
            0,
            "something drew in row 1 as well, so the band is not a discriminator: {:?}",
            band_coverage(&colour, V_W, V_H, empty_band).bounds
        );

        // -- control 2: the same band on the title screen is empty -----------
        // What else already paints here? On the title screen, nothing: the logo
        // ends at y 94 and the button column starts at 168, and row 0's content
        // rect is y 53..85. If that ever stops being true this fires, which is
        // the point.
        let title_nav = test_nav("ws-empty-control");
        let title_ui = UiState::new();
        assert_eq!(title_ui.screen(), Screen::MainMenu, "the control is the title");
        let statuses = StatusCache::with_probe(unavailable_probe());
        let mut fav = FaviconCache::new();
        let title = frame_for(&title_ui, &title_nav, &statuses, &mut fav).expect("title frame");
        let title_colour = geometry(&title, V_W, V_H);
        assert_eq!(
            band_coverage(&title_colour, V_W, V_H, band).count,
            0,
            "the title screen already paints in {band:?}, so control 1 measures nothing: {:?}",
            band_coverage(&title_colour, V_W, V_H, band).bounds
        );
    }

    /// The empty-list message fits the row it is centred in.
    ///
    /// Vanilla's `NoWorldsEntry` gives its `StringWidget` no `maxWidth`
    /// (`WorldSelectionList.java:382-384`), so nothing clips it and a longer
    /// string would overhang the row. Measured with [`text_px`], the same
    /// fixed-advance measure the jar-less draw uses — the real vanilla font is
    /// narrower, so this is the conservative direction.
    #[test]
    fn the_empty_world_list_message_fits_the_row_it_is_centred_in() {
        let (.., content_w, _) = world_list_row_content_rect(0, V_W);
        let measured = text_px(crate::menu::world_select::NO_WORLDS_MESSAGE, 1.0);
        assert!(
            measured <= content_w,
            "the empty-list message measures {measured} px in a {content_w} px row"
        );
    }

    /// Hover and focus are two facts on this screen, and both reach the draw.
    ///
    /// The bug this rules out is concrete: with one flag, moving the mouse over
    /// the footer would pull the keyboard out of the search field. So the
    /// assertion is that hovering a button changes what *that button* draws while
    /// leaving the focused row alone.
    #[test]
    fn hovering_a_world_select_button_lights_it_without_moving_focus() {
        use crate::menu::world_select::{SEARCH_FIELD, WorldSelectButton as B};
        let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
        let (mut nav, mut ui) = world_select_nav("ws-hover");
        nav.hover(&ui, B::Back.row());
        let frame = world_select_frame(&nav, &ui);
        assert_eq!(frame.hovered, Some(B::Back.row()));
        assert_eq!(
            frame.selected, SEARCH_FIELD,
            "hovering must not move keyboard focus"
        );

        // Vanilla's sprite argument is `isHoveredOrFocused()`, so a hovered
        // *enabled* button draws `widget/button_highlighted`.
        let row = frame.rows[B::Back.row()].clone();
        let draw = |hovered: Option<usize>| {
            let mut f = frame_with(vec![row.clone()], 99);
            f.vanilla = true;
            f.hovered = hovered;
            build(&f, Some(&atlas), None, V_W, V_H).sprite
        };
        let (hi_min, hi_max) = sprite_uv_bounds(&atlas, widget::BUTTON_SPRITES.enabled_focused);
        assert!(
            all_uvs_within(&draw(Some(0)), hi_min, hi_max),
            "a hovered enabled button must sample widget/button_highlighted"
        );
        // The control: unhovered and unfocused, it must not.
        assert!(
            !all_uvs_within(&draw(None), hi_min, hi_max),
            "the detector cannot tell the highlighted sprite apart"
        );

        // A **disabled** hovered button still draws the disabled sprite —
        // `WidgetSprites`' three-argument collapse, the single rule a hand-rolled
        // highlight gets wrong.
        let create = frame.rows[B::Create.row()].clone();
        let mut f = frame_with(vec![create], 99);
        f.vanilla = true;
        f.hovered = Some(0);
        let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;
        let (off_min, off_max) = sprite_uv_bounds(&atlas, widget::BUTTON_SPRITES.disabled);
        assert!(
            all_uvs_within(&sprite, off_min, off_max),
            "a hovered DISABLED Create New World must still sample widget/button_disabled"
        );

        // And the click that hover would have preceded does nothing on it, which
        // is the other half of "present but disabled".
        let before = ui.screen();
        assert_eq!(
            nav.click(&mut ui, B::Create.row()),
            crate::menu::nav::MenuAction::None
        );
        assert_eq!(ui.screen(), before, "clicking Create must not open anything");
    }

    /// The search box draws as a **text field**, not as a button — a slotted row
    /// carrying an `EditBox` takes `draw_edit_box`'s path and not
    /// `draw_widget`'s.
    ///
    /// The discriminator is the synthetic pack itself: `button_pack()` carries
    /// `widget/button*` and no `widget/text_field*`, so a field falls back to its
    /// flat fill and emits **no sprite quads at all** where a button emits nine.
    /// The control is the same row drawn as a button, watched emitting them.
    #[test]
    fn the_search_box_draws_as_a_field_inside_its_own_slot() {
        let atlas = GuiAtlas::build(&button_pack()).expect("synthetic atlas builds");
        let (mut nav, mut ui) = world_select_nav("ws-search");
        for ch in "abc".chars() {
            nav.key(&mut ui, MenuKey::Char(ch));
        }
        let frame = world_select_frame(&nav, &ui);
        let row = frame.rows[0].clone();
        assert_eq!(
            row.edit.as_ref().map(|e| e.value().to_string()),
            Some("abc".to_string()),
            "typing on this screen goes into the search box"
        );

        let (fx, fy, fw, fh) = world_select_search_slot().resolve(V_W, V_H);
        let mut f = frame_with(vec![row.clone()], 0);
        f.vanilla = true;
        let drawn = build(&f, Some(&atlas), None, V_W, V_H);
        assert!(
            drawn.sprite.is_empty(),
            "the field sampled a button sprite, so it took draw_widget's path"
        );
        // Its background is the field fill, at the slot's own rect.
        assert!(
            coverage_of(&drawn.colour, V_W, V_H, (fx, fy, fw, fh), FIELD_BG) > 0.5,
            "the search box's fill did not reach {:?}",
            (fx, fy, fw, fh)
        );

        // -- control ---------------------------------------------------------
        // The same row without its `EditBox` is a button, and it must emit the
        // sprite quads the assertion above requires to be absent.
        let mut as_button = row.clone();
        as_button.edit = None;
        as_button.field = false;
        let mut g = frame_with(vec![as_button], 0);
        g.vanilla = true;
        let button_drawn = build(&g, Some(&atlas), None, V_W, V_H);
        assert!(
            !button_drawn.sprite.is_empty(),
            "a button drew no sprites either, so the discriminator measures nothing"
        );

        // The typed text lands inside the box's own text band — every bound asked
        // of a clone repositioned into the slot, exactly as `draw_edit_box` does,
        // rather than restated.
        let mut probe = row.edit.clone().expect("a live box");
        probe.widget.x = fx;
        probe.widget.y = fy;
        probe.widget.width = fw;
        probe.widget.height = fh;
        let state = probe.draw_state(None);
        let band = (fx, state.text_y, fw, GLYPH_H as f32 * TEXT_SCALE);
        let inside = band_coverage(&drawn.colour, V_W, V_H, band);
        assert!(
            inside.count > 0,
            "the typed text reached no pixels inside the box's own band {band:?}"
        );
        assert!(
            inside.bounds.is_some_and(|b| b.0 >= state.before_x - 0.01),
            "text drew left of the box's own text_x {}: bounds {:?}",
            state.before_x,
            inside.bounds
        );
    }

    /// A real single-colour PNG, encoded here so the favicon test's input is a
    /// genuine PNG stream (IHDR/IDAT/IEND with zlib and CRCs) rather than
    /// something only our own decoder would accept.
    fn solid_png(side: u32, rgba: [u8; 4]) -> Vec<u8> {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, side, side);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut writer = enc.write_header().expect("write header");
            let data: Vec<u8> = (0..side * side).flat_map(|_| rgba).collect();
            writer.write_image_data(&data).expect("write image");
        }
        out
    }
}
