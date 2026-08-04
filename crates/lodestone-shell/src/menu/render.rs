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
use crate::menu::panorama::{self, PanoramaFaces, PanoramaRenderer};
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
    /// screen's title. `this.width` is `int` everywhere vanilla anchors off it
    /// (e.g. `this.width / 2 - 100` at `TitleScreen.java:144`), so `w / 2` is
    /// Java integer division — hence the `floor` (issue #401).
    ScreenTop,
    /// `(floor(w / 2), floor(h / 4) + 48)` — vanilla `TitleScreen.init`'s
    /// `topPos` (`TitleScreen.java:113`) for y, and the same `this.width / 2`
    /// as [`Origin::ScreenTop`] for x. Both are Java integer division, hence
    /// both `floor`s (issue #401: only the y one used to be here).
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
    /// `(floor(w / 2), h)` — bottom-centre, for the footer band of the account
    /// screen (Add Account / Select / Remove / Back) and the multiplayer
    /// screen's seven. Not vanilla-sourced like the others above: nothing in
    /// `TitleScreen`/`PauseScreen` anchors a widget row to the bottom edge. Since
    /// #396 it is where both `HeaderAndFooterLayout` footers are pinned, which is
    /// canvas-independent even though the arranged rects are not — see
    /// [`ACCOUNTS_REF_CANVAS`]. `floor`ed for the same reason as
    /// [`Origin::ScreenTop`] (issue #401): every consumer of this origin is a
    /// `Slot` centred *about* this x, and an unfloored anchor at an odd width
    /// puts that centring a half-pixel off whole, which blurs the text drawn
    /// there.
    ScreenBottom,
    /// `(floor(w / 4), 0)` — the death screen's title anchor (issue #103).
    /// `DeathScreen.visitText` draws it at `middleLine / 2` where
    /// `middleLine = this.width / 2` (`DeathScreen.java:118-120`), i.e.
    /// **centred on the screen's left quarter, not the middle** — this is
    /// vanilla's own layout (seemingly an oversight nobody ever fixed, not a
    /// deliberate design), reproduced faithfully rather than "corrected" to
    /// [`Origin::ScreenTop`]. Both are Java integer division —
    /// `floor(floor(w/2)/2) == floor(w/4)` for a non-negative `w`, so the two
    /// chained truncations collapse to the one `floor` here — and #401's audit
    /// of every unfloored `Origin::anchor` term caught this arm too, alongside
    /// [`Origin::ScreenTop`]/[`Origin::TitleTop`]/[`Origin::ScreenBottom`].
    DeathTitle,
    /// A widget of the settings tree (issue #55), resolved by
    /// [`super::options::placement_anchor`].
    ///
    /// The only [`Origin`] that carries data, and it has to: a settings row's
    /// position depends on the page, the entry, **and how far the list is
    /// scrolled**, none of which anything downstream of [`frame_for`] knows —
    /// this enum is precisely the seam where a canvas-dependent term gets to
    /// live, and the scroll rides along with it. The three shapes it covers are
    /// `OptionsScreen`'s arranged `HeaderAndFooterLayout`, an
    /// `OptionsSubScreen`'s footer band, and an `OptionsList` row; see
    /// [`super::options::Placement`].
    Settings(super::options::Placement),
}

impl Origin {
    /// The anchor point in logical pixels for a canvas of `width`×`height`.
    #[must_use]
    pub fn anchor(self, width: f32, height: f32) -> (f32, f32) {
        match self {
            Origin::ScreenTop => ((width * 0.5).floor(), 0.0),
            Origin::TitleTop => ((width * 0.5).floor(), (height / 4.0).floor() + 48.0),
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
            Origin::ScreenBottom => ((width * 0.5).floor(), height),
            Origin::DeathTitle => ((width * 0.25).floor(), 0.0),
            // Unlike every arm above, this one *runs a layout* rather than
            // evaluating an expression — `OptionsScreen`'s tree cannot be
            // arranged once per process the way `pause_block` is, because
            // `HeaderAndFooterLayout` places its content band from the canvas
            // height. See `super::options::root_widget_rects`.
            Origin::Settings(placement) => {
                super::options::placement_anchor(placement, width, height)
            }
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

/// The one entry the list has, drawn with vanilla's `NoWorldsEntry` geometry —
/// `text` is [`super::world_select::WorldSelectNav::world_row_label`], i.e. the
/// bundled world (issue #287).
///
/// `NoWorldsEntry`'s geometry rather than `WorldListEntry`'s is deliberate and
/// unchanged by #287: `WorldListEntry` draws a 32×32 icon plus three text lines
/// off a `LevelSummary` (`WorldSelectionList.java:494-502`), and there is no
/// world storage here to supply one. See `world_select`'s module docs.
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
pub fn world_list_row_label(text: &str) -> MenuLabel {
    // The `x` and `w` of the content rect are discarded — the label is centred on
    // the screen for the reason above — so the width passed here is arbitrary.
    // Reading the rect anyway, instead of restating `row_top + 2`, is
    // `CLAUDE.md`'s "derive layout from the same expression the draw uses".
    let (_, content_y, _, content_h) = world_list_row_content_rect(0, 0.0);
    MenuLabel {
        text: text.to_string(),
        origin: Origin::ScreenTop,
        dx: 0.0,
        dy: content_y + (content_h * 0.5).floor() - (STRING_WIDGET_H * 0.5).floor(),
        align: Align::Centre,
        colour: LABEL,
        scale: 1.0,
    }
}

// -- vanilla's `JoinMultiplayerScreen` / `ServerSelectionList` metrics --------
//
// Every number below is from `.cache/mc/26.2/client-src/net/minecraft/client/gui/
// screens/multiplayer/`, with the line named. Deliberately its own set of
// constants rather than shared with the world-select block above: the two
// screens agree on several values *by coincidence* (both list `itemHeight`s are
// 36, both content paddings are 2 because they inherit the same base class), and
// a shared constant would make a divergence in one screen silently move the
// other.

/// `new HeaderAndFooterLayout(this, 33, 60)` (`JoinMultiplayerScreen.java:30`) —
/// the header band. This is the default 33 spelled out, not
/// [`layout::DEFAULT_HEADER_AND_FOOTER_HEIGHT`], because the *footer* is not the
/// default and the pair is one constructor call.
const SERVER_LIST_HEADER_H: f32 = 33.0;
/// The same call's footer band: 60, because this screen's footer is two rows of
/// buttons rather than one.
const SERVER_LIST_FOOTER_H: f32 = 60.0;
/// `LinearLayout.vertical().spacing(4)` and both
/// `LinearLayout.horizontal().spacing(4)` rows (`:64,66,67`).
const SERVER_LIST_FOOTER_SPACING: i32 = 4;
/// `JoinMultiplayerScreen.TOP_ROW_BUTTON_WIDTH` (`:28`) — Join Server / Direct
/// Connection / Add Server.
const SERVER_LIST_TOP_BUTTON_W: f32 = 100.0;
/// `JoinMultiplayerScreen.LOWER_ROW_BUTTON_WIDTH` (`:29`) — Edit / Delete /
/// Refresh / Back.
const SERVER_LIST_LOWER_BUTTON_W: f32 = 74.0;
/// The `itemHeight` the list is constructed with: the last argument of
/// `new ServerSelectionList(…, 36)` (`:61-62`).
const SERVER_LIST_ITEM_H: f32 = 36.0;
/// `ServerSelectionList.getRowWidth()` (`ServerSelectionList.java:139-141`) — a
/// 305 px override of `AbstractSelectionList`'s 220.
const SERVER_LIST_ROW_W: f32 = 305.0;
/// `AbstractSelectionList.Entry.CONTENT_PADDING` (`AbstractSelectionList.java:435`).
/// The entry rect is inset by this on each side, so a 36 px row has a **32** px
/// content box — exactly [`SERVER_ENTRY_ICON`], which is why the favicon fills
/// the row's height.
const SERVER_LIST_ENTRY_PADDING: f32 = 2.0;
/// `getFirstEntryY() = getY() + 2` (`AbstractSelectionList.java:104-106`): the
/// gap above row 0. A different expression from [`SERVER_LIST_ENTRY_PADDING`]
/// that happens to be the same 2 — only one of them insets a row.
const SERVER_LIST_FIRST_ENTRY_Y: f32 = 2.0;
/// `OnlineServerEntry.ICON_SIZE` (`ServerSelectionList.java:246`).
const SERVER_ENTRY_ICON: f32 = 32.0;
/// `OnlineServerEntry.SPACING` (`:247`) — the gap the status icon and the status
/// text keep from the content's right edge, and from each other.
const SERVER_ENTRY_SPACING: f32 = 5.0;
/// `OnlineServerEntry.STATUS_ICON_WIDTH` (`:248`).
const SERVER_STATUS_ICON_W: f32 = 10.0;
/// `OnlineServerEntry.STATUS_ICON_HEIGHT` (`:249`).
const SERVER_STATUS_ICON_H: f32 = 8.0;
/// The gap between the favicon and the name/MOTD column: vanilla writes
/// `getContentX() + 32 + 3` (`:306,310`) — a literal 3, *not*
/// [`SERVER_ENTRY_SPACING`]'s 5.
const SERVER_ENTRY_TEXT_GAP: f32 = 3.0;
/// The first MOTD line's offset below the content's top: `getContentY() + 12`
/// (`:310`). Subsequent lines step by [`LINE_H`] (`+ 9 * i`).
const SERVER_ENTRY_MOTD_Y: f32 = 12.0;
/// How many MOTD lines a row shows — `Math.min(lines.size(), 2)` (`:309`).
const SERVER_ENTRY_MOTD_LINES: usize = 2;
/// The width the MOTD wraps to: `getContentWidth() - 32 - 2` (`:307`). The 2 is
/// its own literal, not the content padding.
const SERVER_ENTRY_MOTD_INSET: f32 = SERVER_ENTRY_ICON + 2.0;
/// A `StringWidget`'s height, which is what the title header is
/// (`StringWidget.java:15`, `HeaderAndFooterLayout.addTitleHeader`).
const SERVER_LIST_TITLE_H: f32 = 9.0;

/// The MOTD and status colour, `-8355712` (`ServerSelectionList.java:310,349`).
/// A mid grey — `0xFF808080`.
const SERVER_ENTRY_DIM: [f32; 4] = [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0];
/// `CANT_RESOLVE_TEXT`/`CANT_CONNECT_TEXT`'s `withColor(-65536)` (`:68-69`) —
/// pure red, and a *component* colour, so it overrides the `-8355712` the MOTD
/// line is otherwise drawn with.
const SERVER_ENTRY_BAD: [f32; 4] = [1.0, 0.0, 0.0, 1.0];
/// `ChatFormatting.RED`, `0xFF5555` — the version string an incompatible row
/// shows where a compatible one shows its player count (`:344-346`).
const SERVER_ENTRY_INCOMPATIBLE: [f32; 4] = [1.0, 85.0 / 255.0, 85.0 / 255.0, 1.0];
/// The selected row's interior, `-16777216` — opaque black, filled inside the
/// 1 px outline (`AbstractSelectionList.java:363-370`).
const SERVER_LIST_SELECTION_FILL: [f32; 4] = [0.0, 0.0, 0.0, 1.0];
/// The hovered row's icon dim, `fill(…, -1601138544)` (`ServerSelectionList.java:365`)
/// — `0xA0909090`, a translucent grey *over* the favicon, which is what makes
/// the join/move arrows on top of it readable.
const SERVER_ICON_DARKEN: [f32; 4] = [144.0 / 255.0, 144.0 / 255.0, 144.0 / 255.0, 160.0 / 255.0];

/// `ServerSelectionList.JOIN_SPRITE` and its highlighted twin (`:52-53`).
const SERVER_JOIN_SPRITES: (&str, &str) = ("server_list/join", "server_list/join_highlighted");
/// `MOVE_UP_SPRITE` / `MOVE_UP_HIGHLIGHTED_SPRITE` (`:54-55`).
const SERVER_MOVE_UP_SPRITES: (&str, &str) =
    ("server_list/move_up", "server_list/move_up_highlighted");
/// `MOVE_DOWN_SPRITE` / `MOVE_DOWN_HIGHLIGHTED_SPRITE` (`:56-57`).
const SERVER_MOVE_DOWN_SPRITES: (&str, &str) =
    ("server_list/move_down", "server_list/move_down_highlighted");
/// `FaviconTexture`'s fallback, blitted for a row whose server sent no usable
/// icon. A **loose** texture, so it reaches the atlas through
/// [`crate::resources::UNKNOWN_SERVER_TEXTURE`] rather than the sprite glob.
const SERVER_UNKNOWN_ICON: &str = "misc/unknown_server";

/// Vanilla's `JoinMultiplayerScreen.init` (`JoinMultiplayerScreen.java:48-130`)
/// as a real [`layout::HeaderAndFooterLayout`], arranged for a `width`×`height`
/// canvas.
///
/// Two notes before changing it:
///
/// - **The title cell is zero-width**, for the reason `world_select_layout`
///   gives: `addTitleHeader` adds a `StringWidget(title, font)` and there is no
///   font at arrange time, but the header frame centres its child, so a
///   zero-width cell lands exactly on the centre a real-width one would be
///   centred about.
/// - **The list is a [`layout::SpacerElement`]**, sized to
///   `layout.getContentHeight()` exactly as `:61-62` does. It has to take part in
///   the measurement — `HeaderAndFooterLayout`'s content clamp reads the content
///   frame's height — and a spacer is measured and never drawn, which is right:
///   the list draws through [`draw_server_entry`], not as a widget.
fn server_list_layout(width: f32, height: f32) -> layout::HeaderAndFooterLayout {
    let button = |w: f32| -> Box<dyn widget::LayoutElement> {
        Box::new(Widget::button(0.0, 0.0, w, WIDGET_H, ""))
    };
    let mut root = layout::HeaderAndFooterLayout::with_heights(
        width,
        height,
        SERVER_LIST_HEADER_H,
        SERVER_LIST_FOOTER_H,
    );

    // `this.layout.addTitleHeader(this.title, this.font)` (`:49`).
    root.add_to_header(Box::new(Widget::new(
        0.0,
        0.0,
        0.0,
        SERVER_LIST_TITLE_H,
        super::nav::SERVER_LIST_TITLE,
    )));

    let content_height = root.content_height();
    root.add_to_contents(Box::new(layout::SpacerElement::new(width, content_height)));

    // `LinearLayout footer = this.layout.addToFooter(LinearLayout.vertical().spacing(4));`
    // `footer.defaultCellSetting().alignHorizontallyCenter();` (`:64-65`) — the
    // *live* baseline, so both rows inherit the centring.
    let mut footer = layout::LinearLayout::vertical().spacing(SERVER_LIST_FOOTER_SPACING);
    {
        let baseline = footer.default_cell_setting();
        *baseline = baseline.align_horizontally_center();
    }
    let mut top = layout::LinearLayout::horizontal().spacing(SERVER_LIST_FOOTER_SPACING);
    for _ in 0..3 {
        top.add_child(button(SERVER_LIST_TOP_BUTTON_W));
    }
    footer.add_child(Box::new(top));
    let mut bottom = layout::LinearLayout::horizontal().spacing(SERVER_LIST_FOOTER_SPACING);
    for _ in 0..4 {
        bottom.add_child(button(SERVER_LIST_LOWER_BUTTON_W));
    }
    footer.add_child(Box::new(bottom));
    root.add_to_footer(Box::new(footer));

    root.arrange_elements();
    root
}

/// One arranged multiplayer screen: the title cell, the seven footer buttons,
/// and where the content band starts.
///
/// Same shape and same reason as `WorldSelectBlock`: the two bands are anchored
/// to *different* [`Origin`]s, so a flat list of absolute rects could not be
/// turned back into canvas-independent offsets.
#[derive(Debug)]
struct ServerListBlock {
    /// The header's one leaf — the title cell.
    title: (f32, f32, f32, f32),
    /// The footer's leaves, in [`super::nav::SERVER_LIST_BUTTONS`]' order.
    footer: Vec<(f32, f32, f32, f32)>,
    /// The content frame's top, i.e. `list.getY()`.
    content_top: f32,
    /// The canvas this was arranged at, so band offsets can be made relative to
    /// it.
    canvas: (f32, f32),
}

impl ServerListBlock {
    /// Arrange the tree at `width`×`height` and read its leaves back. The leaf
    /// counts are asserted for [`MenuBlock::of`]'s reason.
    fn at(width: f32, height: f32) -> Self {
        let root = server_list_layout(width, height);
        let header = layout::widget_rects(root.header());
        let footer = layout::widget_rects(root.footer());
        assert_eq!(
            header.len(),
            1,
            "the multiplayer header has {} leaves, the screen has 1 (the title)",
            header.len()
        );
        assert_eq!(
            footer.len(),
            super::nav::SERVER_LIST_BUTTONS.len(),
            "the multiplayer footer has {} leaves, the screen has {}",
            footer.len(),
            super::nav::SERVER_LIST_BUTTONS.len()
        );
        Self {
            title: header[0],
            footer,
            content_top: root.contents().y(),
            canvas: (width, height),
        }
    }

    /// The footer leaf `index` as a slot measured from [`Origin::ScreenBottom`].
    /// Its `dy` is negative — the footer is pinned to the bottom edge.
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

/// The multiplayer screen, arranged once at [`SERVER_LIST_REF_CANVAS`].
///
/// Canvas-independence is the same argument, and it is asserted the same way:
/// `the_server_list_slots_do_not_depend_on_the_reference_canvas` re-arranges at
/// three sizes and requires every slot to come out identical. The footer column
/// measures 308 wide at any width (both its rows do — `3 * 100 + 2 * 4` and
/// `4 * 74 + 3 * 4`), and the content band always begins at the header height,
/// because the list is sized to `getContentHeight()` exactly.
fn server_list_block() -> &'static ServerListBlock {
    static BLOCK: std::sync::OnceLock<ServerListBlock> = std::sync::OnceLock::new();
    BLOCK.get_or_init(|| ServerListBlock::at(SERVER_LIST_REF_CANVAS.0, SERVER_LIST_REF_CANVAS.1))
}

/// The canvas [`server_list_block`] arranges at. See that function.
const SERVER_LIST_REF_CANVAS: (f32, f32) = (854.0, 480.0);

/// The screen title, positioned from the arranged header's own title cell.
///
/// `Align::Centre` because that cell is zero-width and therefore *is* the text's
/// centre — see [`server_list_layout`].
#[must_use]
pub fn server_list_title_label() -> MenuLabel {
    let block = server_list_block();
    let (x, y, _, _) = block.title;
    MenuLabel {
        text: super::nav::SERVER_LIST_TITLE.to_string(),
        origin: Origin::ScreenTop,
        dx: x - block.canvas.0 * 0.5,
        dy: y,
        align: Align::Centre,
        colour: LABEL,
        scale: 1.0,
    }
}

/// Vanilla's rect for one footer button, read out of the arranged footer.
///
/// Exhaustive rather than an `as usize`, for [`title_slot`]'s reason: a new
/// variant must be a compile error, not a silent off-by-one across every rect.
#[must_use]
pub fn server_list_footer_slot(button: super::nav::ServerListButton) -> Slot {
    use super::nav::ServerListButton as B;
    let index = match button {
        B::Select => 0,
        B::Direct => 1,
        B::Add => 2,
        B::Edit => 3,
        B::Delete => 4,
        B::Refresh => 5,
        B::Back => 6,
    };
    server_list_block().footer_slot(index)
}

/// The left edge of every list row: `getRowLeft()`, which is
/// `getX() + this.width / 2 - getRowWidth() / 2` with `getX() == 0`
/// (`AbstractSelectionList.java:372-374`).
///
/// **Not `(width - 305) / 2`.** Vanilla halves each term separately with integer
/// division, so at an odd width the two differ by a pixel; the `floor`s are that
/// arithmetic, and they are why this takes a width instead of folding into a
/// [`Slot`]'s `dx`.
#[must_use]
pub fn server_row_left(width: f32) -> f32 {
    (width * 0.5).floor() - (SERVER_LIST_ROW_W * 0.5).floor()
}

/// The top of list row `index`: `getFirstEntryY() + (index - scrollAmount) *
/// itemHeight` (`AbstractSelectionList.java:143-150`), with `scrollAmount`
/// quantized to whole rows (issue #402).
///
/// **Row-quantized rather than vanilla's continuous pixel `scrollAmount`.**
/// Vanilla scissors the band, so a row can be half on-screen; this pipeline has
/// no scissor (see [`server_row_visible`]), so a partially-clipped row would
/// paint over the header or the footer instead of being cut. Skipping whole
/// rows is the only offset this draw model can express safely — [`MenuNav`
/// (`super::nav::MenuNav`)]'s `server_scroll` is therefore a row count, not a
/// pixel amount, and this is where that count is turned back into pixels.
#[must_use]
pub fn server_row_top(index: usize, scroll: usize) -> f32 {
    server_list_block().content_top
        + SERVER_LIST_FIRST_ENTRY_Y
        + (index as f32 - scroll as f32) * SERVER_LIST_ITEM_H
}

/// The rect of list row `index` at a `width`-wide canvas, scrolled by `scroll`
/// rows (issue #402).
#[must_use]
pub fn server_row_rect(index: usize, width: f32, scroll: usize) -> (f32, f32, f32, f32) {
    (
        server_row_left(width),
        server_row_top(index, scroll),
        SERVER_LIST_ROW_W,
        SERVER_LIST_ITEM_H,
    )
}

/// A row's *content* rect — the entry rect inset by
/// [`SERVER_LIST_ENTRY_PADDING`] on each side
/// (`AbstractSelectionList.java:477-506`). Everything an
/// `OnlineServerEntry` draws is measured from this, not from the row.
#[must_use]
pub fn server_row_content_rect(index: usize, width: f32, scroll: usize) -> (f32, f32, f32, f32) {
    let (x, y, w, h) = server_row_rect(index, width, scroll);
    (
        x + SERVER_LIST_ENTRY_PADDING,
        y + SERVER_LIST_ENTRY_PADDING,
        w - 2.0 * SERVER_LIST_ENTRY_PADDING,
        h - 2.0 * SERVER_LIST_ENTRY_PADDING,
    )
}

/// Whether row `index` is inside the list's band on a `height`-tall canvas at
/// `scroll` rows of offset — `extractListItems`' own visibility test,
/// `child.getY() + child.getHeight() >= getY() && child.getY() <= getBottom()`
/// (`AbstractSelectionList.java:346-352`).
///
/// This stands in for vanilla's **scissor**, which this pipeline has no
/// equivalent of: a row that would overflow into the footer is skipped entirely
/// rather than half-drawn, and — as of #402 — a row scrolled above the band
/// (`index < scroll`) is rejected outright rather than relying on the geometry
/// producing a negative top that happens to fail the bottom check, which is
/// what made the *old*, scroll-less version of this function look complete
/// while `row_rect` still answered for a row it would never draw. `row_rect`
/// now calls this too (through [`MenuRow::entry`]'s carried `scroll`), so a
/// click can no longer land on a row that is not on screen — see
/// `docs/server-list.md`'s `hit_testing_matches_what_is_drawn_after_scrolling`
/// for the executed control.
#[must_use]
pub fn server_row_visible(index: usize, height: f32, scroll: usize) -> bool {
    if index < scroll {
        return false;
    }
    let top = server_row_top(index, scroll);
    let list_top = server_list_block().content_top;
    let list_bottom = height - SERVER_LIST_FOOTER_H;
    top + SERVER_LIST_ITEM_H >= list_top && top <= list_bottom
}

/// Rows guaranteed visible at [`crate::config::MIN_SCALED_HEIGHT`] (vanilla's
/// `Window.java:453`), so scroll-into-view (keyboard) and the wheel's fallback
/// clamp are correct at every canvas and merely conservative at a larger one —
/// the same trade `options::LIST_WINDOW_PX` and `accounts::VISIBLE_ROWS` make,
/// for the same reason named on [`server_row_visible`]: this pipeline has no
/// scissor, so a window that ever *overestimates* what fits would paint a row
/// over the footer. Row-quantized rather than `LIST_WINDOW_PX`'s pixels, for
/// [`server_row_top`]'s reason.
#[must_use]
pub fn server_list_window_rows() -> usize {
    let list_top = server_list_block().content_top;
    let band = crate::config::MIN_SCALED_HEIGHT as f32 - list_top - SERVER_LIST_FOOTER_H;
    (band / SERVER_LIST_ITEM_H).floor().max(1.0) as usize
}

/// The largest legal `scroll` for `entry_count` rows at a `height`-tall canvas —
/// vanilla's `AbstractScrollArea::maxScrollAmount`, `max(0, contentHeight -
/// height)`, expressed in rows instead of pixels for [`server_row_top`]'s
/// reason. Used by the mouse wheel (`MenuNav::scroll_server_list`), which knows
/// the real canvas at the moment it fires — unlike keyboard scroll-into-view,
/// which uses the canvas-independent [`server_list_window_rows`] instead.
#[must_use]
pub fn server_list_max_scroll(entry_count: usize, height: f32) -> usize {
    let list_top = server_list_block().content_top;
    let visible = ((height - SERVER_LIST_FOOTER_H - list_top) / SERVER_LIST_ITEM_H)
        .floor()
        .max(0.0) as usize;
    entry_count.saturating_sub(visible)
}

/// The favicon's rect in row `index` — the content origin, 32×32
/// (`ServerSelectionList.java:313,438-440`).
///
/// **Public because the click needs it too.** `MenuNav::click` decides whether a
/// click joins, moves the row up or moves it down from which quadrant of *this*
/// rect the cursor is in, and a second copy of the arithmetic is how the
/// highlighted quadrant and the acting quadrant drift apart.
#[must_use]
pub fn server_entry_icon_rect(index: usize, width: f32, scroll: usize) -> (f32, f32, f32, f32) {
    let (cx, cy, _, _) = server_row_content_rect(index, width, scroll);
    (cx, cy, SERVER_ENTRY_ICON, SERVER_ENTRY_ICON)
}

/// The rect of the status icon in row `index`, and the x the status *text* is
/// right-aligned against.
///
/// `statusIconX = getContentRight() - 10 - 5` (`ServerSelectionList.java:329`),
/// at `getContentY()` — the icon is **not** vertically centred in the row.
#[must_use]
pub fn server_status_icon_rect(index: usize, width: f32, scroll: usize) -> (f32, f32, f32, f32) {
    let (cx, cy, cw, _) = server_row_content_rect(index, width, scroll);
    (
        cx + cw - SERVER_STATUS_ICON_W - SERVER_ENTRY_SPACING,
        cy,
        SERVER_STATUS_ICON_W,
        SERVER_STATUS_ICON_H,
    )
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
    /// Draw the row's background as vanilla's `AbstractSliderButton` track
    /// instead of a `Button` (issue #55).
    ///
    /// A settings screen's numeric options are sliders and its enums and
    /// booleans are `CycleButton`s (`OptionInstance.java:127-135`), and the two
    /// look nothing alike — a slider track has no bevel and no disabled variant.
    /// A `bool` rather than a value, because **no live option in this client is a
    /// slider**: `guiScale` is a `ClampingLazyMaxIntRange`, whose
    /// `createCycleButton()` is `true`, so it is a cycle button. Every slider we
    /// draw is therefore inactive and has no handle to place; see
    /// [`super::widget::Widget::slider`].
    pub slider: bool,
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
    /// Set on a [`super::Screen::ServerList`] row: everything an
    /// `OnlineServerEntry` draws that a button row has no field for.
    ///
    /// Its presence is what routes the row to [`draw_server_entry`] instead of
    /// [`draw_widget`], *before* the `slot` test — a list entry is not a button
    /// with an icon, it is a different drawable with three text columns and a
    /// hover overlay. `label` (the server's name) and `favicon` are read from the
    /// row itself rather than duplicated in here.
    pub entry: Option<ServerEntryView>,
    /// Set on a [`super::Screen::Accounts`] list row: the little an account row
    /// needs beyond `label`/`detail`/`trailing`/`head`, which it reads off the
    /// row itself exactly as a multiplayer entry reads `label`/`favicon`.
    ///
    /// Its presence routes the row to [`draw_account_entry`] and, in
    /// [`row_rect`], to [`accounts_row_rect`] — both tested *before* `slot`, for
    /// [`Self::entry`]'s reason: a list entry is not a button, and the row column
    /// is `floor(w / 2) - floor(305 / 2)`, which a `Slot` cannot express.
    pub account: Option<AccountEntryView>,
}

/// One account-list row's state (issues #66/#402).
///
/// Deliberately two fields. Everything else a row draws is already a [`MenuRow`]
/// field — the username is `label`, "Microsoft account" is `detail`, the
/// "Selected" marker is `trailing`, the head icon is `head` — and duplicating any
/// of them here is how a row and its draw end up disagreeing.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AccountEntryView {
    /// The row's index **in the rendered window**, not in the full account list:
    /// the frame builder has already applied the scroll offset, so this is what
    /// [`accounts_row_top`] multiplies and what a click hit-tests onto.
    pub index: usize,
    /// Whether the list cursor is on this row — `AccountsNav::highlighted`, which
    /// gets `AbstractSelectionList.extractItem`'s 1 px outline plus black
    /// interior.
    ///
    /// A different question from [`MenuFrame::selected`], which on this screen
    /// carries the **footer button** the mouse is over. Both are visible at once
    /// and are drawn completely differently — the same two-cursor split
    /// `docs/server-list.md` argues for the multiplayer screen.
    pub selected: bool,
}

/// A block of **wrapped, bounded** body text: the account screen's sign-in
/// failure reason, the URL it asks the player to open, and its save-error line.
///
/// ## Why this exists
///
/// A [`MenuLabel`] is one unwrapped line drawn at whatever scale it asks for, and
/// [`MenuFrame::message`] is the same thing centred at [`TEXT_SCALE`]. That is
/// fine for text *we* wrote and whose length we control. It is not fine for text
/// we did not: [`super::accounts::describe_auth_error`] renders an
/// `AuthError`, and several of that type's variants carry a snippet of whatever
/// Microsoft or Mojang actually returned — a few hundred characters of JSON with
/// no whitespace in it. Drawn as one scale-2 centred line, that ran off both
/// edges of the screen, which is what a player reported.
///
/// ## What is carried, and what is not
///
/// The **text**, not the lines. Wrapping has to be measured in the font the draw
/// will use, so it happens at draw time — the same reason
/// [`ServerEntryView::motd`] is carried unwrapped. The line *count* is not
/// carried either: [`Self::bottom`] says how much of the canvas to keep clear and
/// [`notice_rect`] turns that into however many whole [`LINE_H`] lines fit, so the
/// layout decides how much text a canvas shows rather than a constant deciding it
/// for every canvas.
#[derive(Debug, Clone, PartialEq)]
pub struct MenuNotice {
    /// The unwrapped text. May contain `\n`, and may be arbitrarily long.
    pub text: String,
    /// Anchor the block is measured from.
    pub origin: Origin,
    /// Horizontal offset from the anchor — the block's **left** edge.
    pub dx: f32,
    /// Vertical offset from the anchor — the **top** of the first line.
    pub dy: f32,
    /// The wrap column's width. No line may measure wider than this, including a
    /// line made of a single unbroken word (see [`wrap_bounded`]).
    pub w: f32,
    /// Pixels kept clear at the **bottom of the canvas**. The line count is
    /// `floor((height - bottom - top) / LINE_H)`.
    pub bottom: f32,
    /// RGBA, sRGB 0..1 verbatim — the shell's convention.
    pub colour: [f32; 4],
}

/// The rect a [`MenuNotice`] is bounded to on a `width`×`height` canvas: its wrap
/// column, and as many whole [`LINE_H`] lines as fit above
/// [`MenuNotice::bottom`].
///
/// **Public because the gate reads it.** A test that restated this arithmetic
/// would be asserting its own copy of the layout, which `CLAUDE.md` records as
/// having been wrong twice; this is the expression [`build`] draws from.
#[must_use]
pub fn notice_rect(notice: &MenuNotice, width: f32, height: f32) -> (f32, f32, f32, f32) {
    let (ax, ay) = notice.origin.anchor(width, height);
    let x = (ax + notice.dx).floor();
    let y = ay + notice.dy;
    let room = (height - notice.bottom - y).max(0.0);
    (x, y, notice.w, (room / LINE_H).floor() * LINE_H)
}

/// How many whole lines [`notice_rect`] found room for.
fn notice_lines(notice: &MenuNotice, width: f32, height: f32) -> usize {
    let (_, _, _, h) = notice_rect(notice, width, height);
    (h / LINE_H).floor().max(0.0) as usize
}

/// One multiplayer-list row's state, in the form
/// `ServerSelectionList.OnlineServerEntry.extractContent` needs it.
///
/// Everything here is resolved by [`server_list_frame`] — which sprite, which
/// colour, whether the move arrows apply — so the draw decides nothing except
/// where. The one thing it cannot resolve is *hover*, because that depends on the
/// canvas, and the canvas is only known at draw time (see [`MenuFrame::cursor`]).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ServerEntryView {
    /// The row's index in the list — vanilla's
    /// `ServerSelectionList.this.children().indexOf(this)`, which is what the
    /// pinging animation's phase and both move arrows key on.
    pub index: usize,
    /// The MOTD, unwrapped and possibly multi-line. Wrapped at draw time,
    /// because the wrap width is measured in the font the draw will use.
    pub motd: String,
    /// Draw the MOTD in [`SERVER_ENTRY_BAD`] — vanilla's `CANT_RESOLVE_TEXT` /
    /// `CANT_CONNECT_TEXT`, which carry their own red component colour.
    pub motd_is_error: bool,
    /// The right-aligned status column: the player count, or an incompatible
    /// server's version string.
    pub status: String,
    /// Draw `status` in [`SERVER_ENTRY_INCOMPATIBLE`] rather than
    /// [`SERVER_ENTRY_DIM`].
    pub status_is_error: bool,
    /// The `server_list/*` sprite for this row's state — see
    /// [`super::status::status_sprite`], which is the only thing that picks one.
    pub status_sprite: &'static str,
    /// Whether this is the list's selected entry (`getSelected() == this`), which
    /// is a different question from [`MenuFrame::selected`]: on this screen that
    /// field carries the *footer button* the cursor is over.
    pub selected: bool,
    /// `index > 0` — vanilla's guard on the move-up arrow (`:375`).
    pub can_move_up: bool,
    /// `index < servers.size() - 1` — the move-down guard (`:386`).
    pub can_move_down: bool,
    /// The list's current scroll offset, in rows (issue #402). Denormalized
    /// onto every entry (rather than added as a parameter to [`row_rect`] and
    /// every render function it calls) so `row_rect` — which `app.rs`'s
    /// hit-test reads too — can resolve a row's position and visibility from
    /// the row alone, with no second plumbing path from `MenuNav` to the draw.
    pub scroll: usize,
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
    /// The mouse position in **logical** pixels, when it is known.
    ///
    /// Every other screen here resolves the mouse to a *row index* before it ever
    /// reaches a frame ([`super::nav::MenuNav::hover`]), which is all a button
    /// needs. The multiplayer list needs more: vanilla's row draws a different
    /// sprite depending on which **quadrant of the 32 px favicon** the cursor is
    /// in (`ServerSelectionList.java:364-395`), and that cannot be decided before
    /// the canvas is known, because the icon's rect depends on it. So the raw
    /// position rides along on the frame and [`draw_server_entry`] does the
    /// quadrant test against the rect it is about to draw into.
    ///
    /// `None` means "no mouse has moved yet", which is the state a keyboard-only
    /// session and every hermetic test are in — and it must draw *no* hover
    /// overlay rather than one at `(0, 0)`.
    pub cursor: Option<(f32, f32)>,
    /// A wrapped, bounded block of body text — see [`MenuNotice`], which is also
    /// where the overflow bug this exists to fix is described.
    ///
    /// One per frame, because the three states that use it are mutually
    /// exclusive: a sign-in URL, a failure reason, or a save error. Distinct from
    /// [`Self::message`], which is a single unwrapped [`TEXT_SCALE`] line and is
    /// suppressed entirely on a `vanilla` frame.
    pub notice: Option<MenuNotice>,
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

// -- vanilla's `DisconnectedScreen` metrics -----------------------------------

/// `Button.builder(…).width(200)`, every call site
/// (`DisconnectedScreen.java:52,57,61,63`) — not [`widget::DEFAULT_WIDTH`]'s
/// 150.
const ERROR_BUTTON_W: f32 = 200.0;
/// Room reserved above the bottom edge for the one button this screen draws:
/// [`WIDGET_H`] plus a margin roughly matching vanilla's `padding(2)` between
/// stack children (`DisconnectedScreen.java:47`) plus some slack so the
/// button never crowds the edge on a small canvas.
const ERROR_BUTTON_BOTTOM_MARGIN: f32 = WIDGET_H + 20.0;
/// Where the title sits, from [`Origin::ScreenTop`].
///
/// Vanilla has no fixed y here — the whole stack is centred vertically by
/// `FrameLayout.centerInRectangle` (`:73-75`), which needs the reason text's
/// *wrapped line count* to size the stack, a draw-time fact `frame_for`
/// cannot see (it runs before the canvas is known — see [`Slot`]'s docs).
/// This anchors the title near the top instead, the same trade
/// [`accounts_failed_frame`] already makes for an identically-shaped screen.
const ERROR_TITLE_Y: f32 = 40.0;
/// The wrap column the reason text is bounded to.
///
/// Vanilla bounds its `MultiLineTextWidget` to `this.width - 50`
/// (`DisconnectedScreen.java:46`), which is canvas-*dependent* and therefore
/// not expressible as a fixed [`MenuNotice::w`] (the same reason
/// [`ACCOUNTS_ROW_W`] is fixed rather than derived per-canvas). Sized off
/// [`crate::config::MIN_SCALED_WIDTH`] so it is correct even at the smallest
/// canvas `calculate_gui_scale` can produce — the same conservative-at-minimum
/// trade [`super::options::LIST_WINDOW_PX`] makes vertically.
const ERROR_NOTICE_W: f32 = crate::config::MIN_SCALED_WIDTH as f32 - 50.0;

/// Builds vanilla's `DisconnectedScreen` (issue #392's framework epic — this
/// screen was still the pre-framework centred row stack, with no [`Slot`] on
/// its row and no wrapped-text bound on its reason, until now):
/// title, the disconnect reason wrapped and bounded exactly like
/// [`accounts_failed_frame`]'s failure message, and one real button
/// (`.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/DisconnectedScreen.java:42-70`).
///
/// **Two vanilla widgets are never built here.** The `gui.report_to_server`
/// and `gui.open_report_dir` buttons only appear when a `DisconnectionDetails`
/// carries a bug-report link or a saved crash report (`:48-58`); nothing in
/// this workspace produces either, so their absence is "present only when
/// vanilla would show it", not a missing row — the same rule the
/// multiplayer-screen footer's `Direct Connection` button already follows in
/// the other direction (present, but inactive).
///
/// **The button's label is vanilla's `gui.toTitle`** ("Back to Title
/// Screen"), not the `gui.toMenu` default ("Back to Server List") a
/// `DisconnectedScreen` shows when `allowsMultiplayer()` is true
/// (`:59-64`). [`super::UiState::dismiss_error`] always returns to
/// [`super::Screen::MainMenu`], never to a server list — that is vanilla's
/// `!allowsMultiplayer()` branch, reproduced honestly, rather than a label
/// that promises a screen this client does not return to.
///
/// **The title is `disconnect.lost`** ("Connection Lost"), vanilla's own
/// title for `ClientPacketListener.onDisconnect`'s ordinary mid-session
/// disconnect — the case [`super::UiState::session_failed`] models most often.
/// A failed *initial* connection attempt is titled `connect.failed` in
/// vanilla instead; this client has one generic error screen for both causes,
/// so one title has to be picked, and the mid-session one is both the more
/// common path and the truthful one when there was a session to lose.
///
/// `shouldCloseOnEsc()` is `false` in vanilla (`:82-85`) — Escape does
/// **not** dismiss this screen there, so a misclick cannot swallow a network
/// error before it is read. This client's Escape *does* dismiss it (see
/// `nav::MenuNav`'s `Screen::Error` arm), which is a pre-existing, separately
/// tested behaviour this pass does not change — this function is layout, not
/// input semantics.
#[must_use]
fn error_frame(reason: Option<&str>) -> MenuFrame<'static> {
    MenuFrame {
        rows: vec![MenuRow {
            label: "Back to Title Screen".to_string(),
            enabled: true,
            slot: Some(Slot {
                origin: Origin::ScreenBottom,
                dx: -(ERROR_BUTTON_W * 0.5),
                dy: -ERROR_BUTTON_BOTTOM_MARGIN,
                w: ERROR_BUTTON_W,
                h: WIDGET_H,
            }),
            ..Default::default()
        }],
        selected: 0,
        vanilla: true,
        labels: vec![MenuLabel {
            text: "Connection Lost".to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: ERROR_TITLE_Y,
            align: Align::Centre,
            colour: LABEL,
            scale: 1.0,
        }],
        // `reason.is_empty()` never happens in production (`session_failed`
        // always carries a real message), but an empty notice would still
        // draw zero lines correctly — no special-casing needed, unlike
        // `death_frame`'s optional message.
        notice: reason.map(|text| MenuNotice {
            text: text.to_string(),
            origin: Origin::ScreenTop,
            dx: -(ERROR_NOTICE_W * 0.5),
            dy: ERROR_TITLE_Y + LINE_H * 3.0,
            w: ERROR_NOTICE_W,
            bottom: ERROR_BUTTON_BOTTOM_MARGIN + WIDGET_H,
            colour: FG_BAD,
        }),
        ..Default::default()
    }
}

// -- the account screen's metrics ---------------------------------------------
//
// **There is no accounts screen in vanilla to port.** Minecraft picks an account
// in the launcher, outside the game — see `nav::MainButton::Accounts`, which
// says the same thing about the title-screen button that opens this. So the
// reference for every number below is *this repo's own* `JoinMultiplayerScreen`
// port two screens up: a `HeaderAndFooterLayout` title, a footer of
// `LinearLayout`-arranged buttons, and 36 px list rows in a 305 px column. Each
// constant therefore cites the server-list constant it deliberately matches
// rather than a jar line, and the two that differ say why they differ.
//
// Separate constants rather than reusing the `SERVER_LIST_*` ones, for the
// reason that block states in its own header comment: the agreement is a
// *choice*, and a shared constant would make a change to the multiplayer screen
// silently move this one.

/// The header band: [`SERVER_LIST_HEADER_H`]'s 33, which is also
/// [`layout::DEFAULT_HEADER_AND_FOOTER_HEIGHT`] — one 9 px title `StringWidget`
/// with 12 px of slack either side.
const ACCOUNTS_HEADER_H: f32 = 33.0;
/// The footer band: [`SERVER_LIST_FOOTER_H`]'s 60, **even though this screen's
/// footer is one row of buttons rather than two**. The 40 px a single 20 px row
/// leaves is not waste: the `FrameLayout` splits it 20/20, and the lower half is
/// where the key-hint line sits (see [`accounts_hint_dy`]). A 33 px band would
/// put that line off the bottom of the canvas.
const ACCOUNTS_FOOTER_H: f32 = 60.0;
/// `LinearLayout.horizontal().spacing(4)` — [`SERVER_LIST_FOOTER_SPACING`].
const ACCOUNTS_FOOTER_SPACING: i32 = 4;
/// One footer button: [`SERVER_LIST_LOWER_BUTTON_W`]'s 74, so the four of them
/// measure `4 * 74 + 3 * 4 = 308` — the same footer column width the
/// multiplayer screen's lower row has, which is what makes the two screens line
/// up rather than each being centred to its own width.
const ACCOUNTS_BUTTON_W: f32 = 74.0;
/// A list row's pitch: [`SERVER_LIST_ITEM_H`]'s 36. With
/// [`ACCOUNTS_ENTRY_PADDING`] a side that leaves a **32** px content box, which
/// is exactly [`ACCOUNTS_HEAD_ICON`] — the head fills the row's height the same
/// way a favicon does.
const ACCOUNTS_ITEM_H: f32 = 36.0;
/// A list row's width: [`SERVER_LIST_ROW_W`]'s 305.
const ACCOUNTS_ROW_W: f32 = 305.0;
/// `AbstractSelectionList.Entry.CONTENT_PADDING`'s 2, per side.
const ACCOUNTS_ENTRY_PADDING: f32 = 2.0;
/// `getFirstEntryY() = getY() + 2` — the gap above row 0. A different
/// expression from [`ACCOUNTS_ENTRY_PADDING`] that happens to be the same 2;
/// only one of them insets a row.
const ACCOUNTS_FIRST_ENTRY_Y: f32 = 2.0;
/// The head icon, [`SERVER_ENTRY_ICON`]'s 32 — the content box's full height.
const ACCOUNTS_HEAD_ICON: f32 = 32.0;
/// The gap between the head icon and the text column, [`SERVER_ENTRY_TEXT_GAP`].
const ACCOUNTS_TEXT_GAP: f32 = 3.0;
/// The gap the trailing "Selected" column keeps from the content's right edge,
/// and from the name — [`SERVER_ENTRY_SPACING`].
const ACCOUNTS_SPACING: f32 = 5.0;
/// The detail line's offset below the content's top, [`SERVER_ENTRY_MOTD_Y`].
const ACCOUNTS_DETAIL_Y: f32 = 12.0;
/// A `StringWidget`'s height — what the title header is.
const ACCOUNTS_TITLE_H: f32 = 9.0;
/// The account list's own title.
const ACCOUNTS_TITLE: &str = "Accounts";
/// The sign-in sub-flow's title.
const ACCOUNTS_SIGN_IN_TITLE: &str = "Sign in with Microsoft";
/// The failure state's title.
const ACCOUNTS_FAILED_TITLE: &str = "Sign-in failed";
/// How many lines a save-error notice is allowed. Two, because it sits *above*
/// the footer band and therefore grows upward into the list — unlike the
/// sign-in states' notice, which owns the whole content band.
const ACCOUNTS_SAVE_ERROR_LINES: f32 = 2.0;

/// A row's detail line: `-8355712`, the same mid grey a multiplayer row's MOTD
/// uses. Its own constant for the reason above.
const ACCOUNTS_DIM: [f32; 4] = [128.0 / 255.0, 128.0 / 255.0, 128.0 / 255.0, 1.0];
/// The highlighted row's interior, `-16777216` — opaque black, filled inside the
/// 1 px outline, exactly `AbstractSelectionList.extractItem`'s selection pass.
const ACCOUNTS_SELECTION_FILL: [f32; 4] = [0.0, 0.0, 0.0, 1.0];

/// The canvas [`accounts_block`] arranges at, for [`SERVER_LIST_REF_CANVAS`]'s
/// reason and with the same condition attached: every rect must be expressible
/// relative to an [`Origin`], which
/// `the_accounts_slots_do_not_depend_on_the_reference_canvas` asserts by
/// re-arranging at three canvases.
const ACCOUNTS_REF_CANVAS: (f32, f32) = (854.0, 480.0);

/// The account screen as a real [`layout::HeaderAndFooterLayout`], arranged for
/// a `width`×`height` canvas — [`server_list_layout`]'s shape, with a
/// four-button single-row footer instead of a 3 + 4 two-row one.
///
/// The two notes from that function apply here unchanged:
///
/// - **The title cell is zero-width.** There is no font at arrange time, and the
///   header frame centres its child, so a zero-width cell lands exactly on the
///   centre a real-width one would be centred about — which is what
///   [`accounts_title_label`] draws from.
/// - **The list is a [`layout::SpacerElement`]** sized to `content_height()`, so
///   it takes part in the measurement (`HeaderAndFooterLayout`'s content clamp
///   reads the content frame's height) and is never drawn — the rows draw
///   through [`draw_account_entry`], not as widgets.
fn accounts_layout(width: f32, height: f32) -> layout::HeaderAndFooterLayout {
    let mut root = layout::HeaderAndFooterLayout::with_heights(
        width,
        height,
        ACCOUNTS_HEADER_H,
        ACCOUNTS_FOOTER_H,
    );

    root.add_to_header(Box::new(Widget::new(
        0.0,
        0.0,
        0.0,
        ACCOUNTS_TITLE_H,
        ACCOUNTS_TITLE,
    )));

    let content_height = root.content_height();
    root.add_to_contents(Box::new(layout::SpacerElement::new(width, content_height)));

    // One row, so no `alignHorizontallyCenter` baseline is needed: the footer
    // `FrameLayout` centres its single child on its own. The server list needs
    // that baseline because its two rows are different widths.
    let mut footer = layout::LinearLayout::horizontal().spacing(ACCOUNTS_FOOTER_SPACING);
    for _ in 0..super::accounts::BUTTON_COUNT {
        footer.add_child(Box::new(Widget::button(
            0.0,
            0.0,
            ACCOUNTS_BUTTON_W,
            WIDGET_H,
            "",
        )));
    }
    root.add_to_footer(Box::new(footer));

    root.arrange_elements();
    root
}

/// One arranged account screen: the title cell, the four footer buttons, and
/// where the content band starts. [`ServerListBlock`]'s shape and its reason —
/// the two bands are anchored to different [`Origin`]s.
#[derive(Debug)]
struct AccountsBlock {
    /// The header's one leaf — the title cell.
    title: (f32, f32, f32, f32),
    /// The footer's leaves, in [`super::accounts::BUTTON_ADD`] order.
    footer: Vec<(f32, f32, f32, f32)>,
    /// The content frame's top, i.e. the list's `getY()`.
    content_top: f32,
    /// The canvas this was arranged at, so band offsets can be made relative
    /// to it.
    canvas: (f32, f32),
}

impl AccountsBlock {
    /// Arrange the tree at `width`×`height` and read its leaves back. The leaf
    /// counts are asserted for [`MenuBlock::of`]'s reason: a tree that no longer
    /// describes the screen must fail loudly rather than shift every rect by one.
    fn at(width: f32, height: f32) -> Self {
        let root = accounts_layout(width, height);
        let header = layout::widget_rects(root.header());
        let footer = layout::widget_rects(root.footer());
        assert_eq!(
            header.len(),
            1,
            "the account header has {} leaves, the screen has 1 (the title)",
            header.len()
        );
        assert_eq!(
            footer.len(),
            super::accounts::BUTTON_COUNT,
            "the account footer has {} leaves, the screen has {}",
            footer.len(),
            super::accounts::BUTTON_COUNT
        );
        Self {
            title: header[0],
            footer,
            content_top: root.contents().y(),
            canvas: (width, height),
        }
    }

    /// The footer leaf `index` as a slot measured from [`Origin::ScreenBottom`].
    /// Its `dy` is negative — the footer is pinned to the bottom edge.
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

/// The account screen, arranged once at [`ACCOUNTS_REF_CANVAS`].
fn accounts_block() -> &'static AccountsBlock {
    static BLOCK: std::sync::OnceLock<AccountsBlock> = std::sync::OnceLock::new();
    BLOCK.get_or_init(|| AccountsBlock::at(ACCOUNTS_REF_CANVAS.0, ACCOUNTS_REF_CANVAS.1))
}

/// The rect for account-screen button `index` (see
/// [`super::accounts::BUTTON_ADD`] and its siblings), read out of the arranged
/// footer rather than computed here — so the width that reaches pixels is the
/// one the layout produced.
fn accounts_button_slot(index: usize) -> Slot {
    accounts_block().footer_slot(index.min(super::accounts::BUTTON_COUNT - 1))
}

/// The single wide button the sign-in and failure states show, centred at the
/// same y the four action buttons occupy.
///
/// The y and height come off [`accounts_button_slot`] rather than being restated,
/// so the one-button states and the four-button state cannot end up on different
/// lines.
fn accounts_wide_button_slot() -> Slot {
    let row = accounts_button_slot(0);
    Slot {
        origin: row.origin,
        dx: -(widget::DEFAULT_WIDTH * 0.5),
        dy: row.dy,
        w: widget::DEFAULT_WIDTH,
        h: row.h,
    }
}

/// The key-hint line's offset from the bottom edge: 8 px below the arranged
/// button row, in the lower half of the slack [`ACCOUNTS_FOOTER_H`] leaves.
///
/// Derived from the arranged row rather than written as a constant, because the
/// failure mode of a constant here is a hint line drawn *through* the buttons —
/// and per `CLAUDE.md` a rect a gate restates is a rect that has been wrong
/// twice.
fn accounts_hint_dy() -> f32 {
    let row = accounts_button_slot(0);
    row.dy + row.h + 8.0
}

/// The screen title, positioned from the arranged header's own title cell.
///
/// `Align::Centre` because that cell is zero-width and therefore *is* the text's
/// centre — see [`accounts_layout`]. Takes the text because the three states of
/// this screen have three different titles.
fn accounts_title_label(text: &str) -> MenuLabel {
    let block = accounts_block();
    let (x, y, _, _) = block.title;
    MenuLabel {
        text: text.to_string(),
        origin: Origin::ScreenTop,
        dx: x - block.canvas.0 * 0.5,
        dy: y,
        align: Align::Centre,
        colour: LABEL,
        scale: 1.0,
    }
}

/// The left edge of every account row: `getRowLeft()`, i.e.
/// `floor(width / 2) - floor(rowWidth / 2)`.
///
/// **Not `(width - 305) / 2`** — two separate integer divisions, which is what
/// [`server_row_left`] documents at length. Reproduced here rather than being
/// approximated by a [`Slot`]'s `dx` so that this screen's rows sit on exactly
/// the same column as the multiplayer screen's at every canvas width, odd ones
/// included.
#[must_use]
pub fn accounts_row_left(width: f32) -> f32 {
    (width * 0.5).floor() - (ACCOUNTS_ROW_W * 0.5).floor()
}

/// The top of account row `index`, where `index` is its position **in the
/// rendered window** rather than in the full list: `getFirstEntryY() + index *
/// itemHeight`.
///
/// The scroll offset is applied by [`accounts_idle_frame`], which slices the
/// list before building rows — so this needs no scroll term and the row a click
/// hit-tests onto is the row that was drawn.
#[must_use]
pub fn accounts_row_top(index: usize) -> f32 {
    accounts_block().content_top + ACCOUNTS_FIRST_ENTRY_Y + index as f32 * ACCOUNTS_ITEM_H
}

/// The rect of account row `index` at a `width`-wide canvas.
#[must_use]
pub fn accounts_row_rect(index: usize, width: f32) -> (f32, f32, f32, f32) {
    (
        accounts_row_left(width),
        accounts_row_top(index),
        ACCOUNTS_ROW_W,
        ACCOUNTS_ITEM_H,
    )
}

/// A row's *content* rect — the row inset by [`ACCOUNTS_ENTRY_PADDING`] a side.
/// Everything a row draws is measured from this, not from the row.
#[must_use]
pub fn accounts_row_content_rect(index: usize, width: f32) -> (f32, f32, f32, f32) {
    let (x, y, w, h) = accounts_row_rect(index, width);
    (
        x + ACCOUNTS_ENTRY_PADDING,
        y + ACCOUNTS_ENTRY_PADDING,
        w - 2.0 * ACCOUNTS_ENTRY_PADDING,
        h - 2.0 * ACCOUNTS_ENTRY_PADDING,
    )
}

/// Whether rendered row `index` fits **entirely** between the content band's top
/// and the footer on a `height`-tall canvas.
///
/// [`server_row_visible`]'s job, one degree stricter: that one reproduces
/// `extractListItems`' test, which keeps a row that merely *overlaps* the band's
/// bottom edge (vanilla then scissors it). This pipeline has no scissor, so a
/// partially-visible row would paint over the footer buttons — and this screen's
/// footer is where its four actions are. A row that would not fit is therefore
/// skipped whole.
///
/// The consequence is the same bounded one the multiplayer list documents and
/// that #402 records: [`row_rect`] still answers for a skipped row, so a click
/// there selects it and nothing else. See [`super::accounts::VISIBLE_ROWS`],
/// which is the other half of this.
#[must_use]
pub fn accounts_row_visible(index: usize, height: f32) -> bool {
    accounts_row_top(index) + ACCOUNTS_ITEM_H <= height - ACCOUNTS_FOOTER_H
}

/// The wrapped-text notice the sign-in and failure states use: the content band,
/// in the same 305 px column the rows occupy, bounded below by the footer.
///
/// This is the fix for the reported overflow. The text it carries is **not
/// ours** — [`super::accounts::describe_auth_error`] renders an `AuthError`, and
/// several of that type's variants embed a
/// snippet of whatever Microsoft or Mojang actually returned — so it must wrap
/// *and* be clipped to a rect the layout sizes, which is exactly what
/// [`MenuNotice`] is.
fn accounts_notice(text: String, colour: [f32; 4]) -> MenuNotice {
    MenuNotice {
        text,
        origin: Origin::ScreenTop,
        // The row column's own left edge at an even canvas width. `dx` is
        // floored for the same reason `accounts_row_left` floors: a `Slot`-style
        // offset is `width * 0.5 + dx` unrounded, and this keeps the text block
        // on the rows' column rather than half a pixel off it.
        dx: -(ACCOUNTS_ROW_W * 0.5).floor(),
        dy: accounts_row_top(0),
        w: ACCOUNTS_ROW_W,
        bottom: ACCOUNTS_FOOTER_H,
        colour,
    }
}

/// The save-error notice: the same column, but anchored **above** the footer
/// band and only [`ACCOUNTS_SAVE_ERROR_LINES`] tall, because the list owns the
/// content band on the screen this one appears on.
///
/// Placed where the multiplayer screen puts its own save-error line, and for the
/// same reason: a failed `profiles.json` write has no vanilla equivalent, and a
/// player whose account choice silently fails to persist deserves the reason.
fn accounts_save_error_notice(text: String) -> MenuNotice {
    MenuNotice {
        text,
        origin: Origin::ScreenBottom,
        dx: -(ACCOUNTS_ROW_W * 0.5).floor(),
        dy: -(ACCOUNTS_FOOTER_H + LINE_H * ACCOUNTS_SAVE_ERROR_LINES + 2.0),
        w: ACCOUNTS_ROW_W,
        bottom: ACCOUNTS_FOOTER_H + 2.0,
        colour: FG_BAD,
    }
}

/// A centred line of body text in the account screen's content band, `line`
/// lines below the first row's top.
///
/// The offsets are all multiples of [`LINE_H`] from [`accounts_row_top`], so the
/// sign-in state's stack of hints sits on the same grid a list row's two text
/// lines do rather than on a second set of constants.
fn accounts_band_label(text: String, line: f32, colour: [f32; 4]) -> MenuLabel {
    MenuLabel {
        text,
        origin: Origin::ScreenTop,
        dx: 0.0,
        dy: accounts_row_top(0) + LINE_H * line,
        align: Align::Centre,
        colour,
        scale: 1.0,
    }
}

/// The key-hint line, centred along the bottom edge — this screen's stand-in for
/// the row-stack `footer` that a `vanilla` frame suppresses.
fn accounts_hint_label(text: &str) -> MenuLabel {
    MenuLabel {
        text: text.to_string(),
        origin: Origin::ScreenBottom,
        dx: 0.0,
        dy: accounts_hint_dy(),
        align: Align::Centre,
        colour: ACCOUNTS_DIM,
        scale: 1.0,
    }
}

/// Builds the account list's ordinary (no sign-in in flight) frame: the scrolling
/// account + offline list at [`ACCOUNTS_ITEM_H`] pitch, then the four action
/// buttons in the arranged footer.
///
/// ## Two cursors, as on the multiplayer screen
///
/// [`AccountEntryView::selected`] is the *list* cursor (`AccountsNav::highlighted`
/// — a 1 px outline over a black interior) and [`MenuFrame::selected`] is the
/// *button* the mouse is on (`AccountsNav::focus` past the end of the list, which
/// [`draw_widget`] turns into `widget/button_highlighted`). Both can be visible
/// at once, which is the whole reason they are separate fields; `usize::MAX`
/// highlights no button, which is the state whenever focus is on a row.
///
/// ## The row order is a coupling
///
/// `AccountsNav::hover` maps a **rendered** row index back through the scroll
/// window and then onto the four button slots, so the order here — `shown` list
/// rows, then Add / Select / Remove / Back — is load-bearing.
/// `the_account_rows_are_in_the_order_click_assumes` is the guard, the same shape
/// the settings and multiplayer screens carry against the same #391 bug.
#[must_use]
fn accounts_idle_frame(accounts: &super::accounts::AccountsNav) -> MenuFrame<'static> {
    use super::accounts::{
        AccountRow, BUTTON_ADD, BUTTON_CANCEL, BUTTON_COUNT, BUTTON_REMOVE, BUTTON_SELECT,
        VISIBLE_ROWS,
    };

    let all_rows = accounts.rows();
    let list_len = all_rows.len();
    let accounts_len = list_len.saturating_sub(1); // the offline row is always last
    let scroll = accounts.scroll().min(list_len.saturating_sub(1));
    let shown = list_len.saturating_sub(scroll).min(VISIBLE_ROWS);
    let highlighted = accounts.highlighted();
    let focus = accounts.focus();

    let mut rows: Vec<MenuRow> = all_rows[scroll..scroll + shown]
        .iter()
        .enumerate()
        .map(|(rendered, row)| {
            let view = AccountEntryView {
                index: rendered,
                selected: scroll + rendered == highlighted,
            };
            match row {
                AccountRow::Account(p) => MenuRow {
                    label: p.username.clone(),
                    detail: "Microsoft account".to_string(),
                    trailing: if accounts.is_selected(p.profile_id) {
                        "Selected".to_string()
                    } else {
                        String::new()
                    },
                    head: Some(default_head_icon()),
                    enabled: true,
                    account: Some(view),
                    ..Default::default()
                },
                // The offline entry is not an account and has no `profile_id`;
                // `selected.is_none()` **is** its selected state — see
                // `super::accounts`' module docs before changing this.
                AccountRow::Offline => MenuRow {
                    label: "Play offline".to_string(),
                    detail: "No sign-in required".to_string(),
                    trailing: if accounts.offline_selected() {
                        "Selected".to_string()
                    } else {
                        String::new()
                    },
                    head: Some(default_head_icon()),
                    enabled: true,
                    account: Some(view),
                    ..Default::default()
                },
            }
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
    rows.push(button_row(BUTTON_ADD, "Add Account", true));
    rows.push(button_row(BUTTON_SELECT, "Select", true));
    // The offline row cannot be removed (`AccountsNav::remove_highlighted`
    // refuses), so the button is inactive while the cursor is on it — the same
    // present-and-disabled treatment the multiplayer footer gives Join with an
    // empty list, rather than a button that silently does nothing.
    rows.push(button_row(
        BUTTON_REMOVE,
        "Remove",
        highlighted < accounts_len,
    ));
    rows.push(button_row(BUTTON_CANCEL, "Back", true));

    let selected = if focus < list_len {
        usize::MAX
    } else {
        shown + (focus - list_len).min(BUTTON_COUNT - 1)
    };

    let mut labels = vec![
        accounts_title_label(ACCOUNTS_TITLE),
        accounts_hint_label("Enter select   Del remove   Esc back"),
    ];
    if list_len == 1 {
        // Placed under the last row rather than under the title: the header band
        // is 33 px and holds a 9 px title, so there is no room for a subtitle
        // there. `accounts_row_top(shown)` is the first free row line, derived
        // from the same expression the rows themselves are placed by.
        labels.push(MenuLabel {
            text: "No accounts signed in - add one, or play offline".to_string(),
            origin: Origin::ScreenTop,
            dx: 0.0,
            dy: accounts_row_top(shown) + 4.0,
            align: Align::Centre,
            colour: ACCOUNTS_DIM,
            scale: 1.0,
        });
    }
    if list_len > shown {
        // Right-aligned on the hint line's own baseline, so the two cannot
        // collide however wide either gets.
        labels.push(MenuLabel {
            text: format!("Showing {}-{} of {}", scroll + 1, scroll + shown, list_len),
            origin: Origin::BottomRight,
            dx: -4.0,
            dy: accounts_hint_dy(),
            align: Align::Right,
            colour: ACCOUNTS_DIM,
            scale: 1.0,
        });
    }

    MenuFrame {
        rows,
        selected,
        vanilla: true,
        labels,
        // Not `message`: that is one unwrapped `TEXT_SCALE` line, and a keychain
        // or filesystem error carries an OS string of unknown length. See
        // `MenuNotice`.
        notice: accounts.save_error().map(accounts_save_error_notice),
        ..Default::default()
    }
}

/// Builds the account screen's frame while a sign-in is in flight: the URL to
/// open, the code to type if the flow has one, and a Cancel button.
///
/// **The URL is the [`MenuNotice`], not a label.** A loopback authorize URL is a
/// few hundred characters of query string with no whitespace in it, so it has to
/// wrap on character boundaries and be bounded to the content band — the same
/// requirement the failure message has, for the same reason.
///
/// The **code** is a label, and it is pre-clipped with [`clip`] rather than
/// carried whole. That is safe where the URL is not: `clip` measures at the
/// fixed-advance fallback font, whose 6 px per glyph is an upper bound on the
/// real proportional font, so a clip to half the row column is a conservative
/// bound in either font.
#[must_use]
fn accounts_flow_frame(
    user_code: Option<&str>,
    verification_uri: Option<&str>,
    waiting: bool,
) -> MenuFrame<'static> {
    let mut labels = vec![accounts_title_label(ACCOUNTS_SIGN_IN_TITLE)];
    labels.push(accounts_band_label(
        if waiting {
            "Waiting for you to finish signing in...".to_string()
        } else {
            "Contacting Microsoft...".to_string()
        },
        0.0,
        LABEL,
    ));
    if let Some(code) = user_code {
        labels.push(accounts_band_label(
            format!("Then enter this code: {}", clip(code, ACCOUNTS_ROW_W * 0.5, 1.0)),
            2.0,
            LABEL,
        ));
    }
    if verification_uri.is_some() {
        labels.push(accounts_band_label(
            "Your browser was opened at this address:".to_string(),
            4.0,
            ACCOUNTS_DIM,
        ));
    }
    labels.push(accounts_hint_label(if waiting {
        "O reopen browser   C copy code   Esc cancel"
    } else {
        "Esc cancel"
    }));

    MenuFrame {
        rows: vec![MenuRow {
            label: "Cancel".to_string(),
            enabled: true,
            slot: Some(accounts_wide_button_slot()),
            ..Default::default()
        }],
        selected: 0,
        vanilla: true,
        labels,
        notice: verification_uri.map(|uri| {
            let mut notice = accounts_notice(uri.to_string(), ACCOUNTS_DIM);
            // Below the three label lines above, on the same `LINE_H` grid.
            notice.dy += LINE_H * 5.0;
            notice
        }),
        ..Default::default()
    }
}

/// Builds the account screen's frame for a failed sign-in attempt.
///
/// **This is the frame the reported bug was in.** The message used to go into
/// [`MenuFrame::message`] — one `to_uppercase`d line, centred at [`TEXT_SCALE`]
/// with no wrap and no clip — so a reason built from a server's own response body
/// was both unreadably large and wider than the screen. It is now a
/// [`MenuNotice`]: wrapped in the draw's own font, broken inside an over-long
/// word, and clipped to as many lines as the content band holds.
#[must_use]
fn accounts_failed_frame(message: &str) -> MenuFrame<'static> {
    MenuFrame {
        rows: vec![MenuRow {
            label: "Back to Accounts".to_string(),
            enabled: true,
            slot: Some(accounts_wide_button_slot()),
            ..Default::default()
        }],
        selected: 0,
        vanilla: true,
        labels: vec![
            accounts_title_label(ACCOUNTS_FAILED_TITLE),
            accounts_hint_label("Enter or Esc continues"),
        ],
        notice: Some(accounts_notice(message.to_string(), FG_BAD)),
        ..Default::default()
    }
}

/// Builds vanilla's `JoinMultiplayerScreen` (#396): one row per saved server at
/// `ServerSelectionList`'s geometry, then the seven footer buttons.
///
/// ## What each row's state resolves to
///
/// The MOTD column is vanilla's `serverData.motd`, which the pinger *overwrites*
/// per state rather than keeping alongside the real MOTD: it is
/// `multiplayer.status.pinging` while a probe is in flight
/// (`ServerStatusPinger.java:65`) and the red `CANT_CONNECT_MESSAGE` when one
/// fails (`:168`). So a failed row shows its reason in the MOTD line and an empty
/// status column (`:169` sets `status` to empty), which is exactly where this
/// screen already put it.
///
/// The one row state that is **ours** is [`super::status::StatusSlot::Idle`] — a
/// row nothing has probed yet. Vanilla has no such state for longer than a frame,
/// so it has no text for it; this shows the address, which is the only thing
/// known about a server before it answers, and is what this screen showed for
/// every row before #396.
///
/// ## Selection, and vanilla's null
///
/// `JoinMultiplayerScreen.onSelectedChange` starts with **nothing** selected and
/// three inactive buttons (`:246-257`). This shell has a keyboard row cursor that
/// always points somewhere, so "has a selection" is modelled as "the list is not
/// empty" — see [`super::nav::ServerListButton::enabled`], which is where that
/// deviation is argued.
#[must_use]
fn server_list_frame(
    nav: &super::nav::MenuNav,
    statuses: &super::status::StatusCache,
    favicons: &mut FaviconCache,
) -> MenuFrame<'static> {
    use super::nav::SERVER_LIST_BUTTONS;
    use super::status::{self, ServerState, StatusCache, StatusSlot};

    let entries = nav.list().entries();
    let last = entries.len().saturating_sub(1);
    // One clock read for the whole frame, so every pinging row animates in step
    // (out of phase by index, which is `pinging_sprite`'s own doing).
    let millis = statuses.millis();
    // #402: read once and stamp onto every entry — see `ServerEntryView::scroll`.
    let scroll = nav.server_scroll();

    let mut rows: Vec<MenuRow> = entries
        .iter()
        .enumerate()
        .map(|(i, e)| {
            let slot = statuses.get(e);
            let state = slot.state(status::STATUS_PROTOCOL);
            let (motd, motd_is_error) = match slot {
                StatusSlot::Idle => (e.address_label(), false),
                StatusSlot::Pending => (status::PINGING_MOTD.to_string(), false),
                StatusSlot::Ok(s) => (s.motd.clone(), false),
                StatusSlot::Failed(why) => (why.clone(), true),
            };
            let (status_text, status_is_error) = match (state, slot) {
                (ServerState::Successful, StatusSlot::Ok(s)) => (s.players.clone(), false),
                // An incompatible server shows its *version* where a compatible
                // one shows its player count (`:344-346`), which is the whole
                // point: the row says what it speaks, in red.
                (ServerState::Incompatible, StatusSlot::Ok(s)) => (s.version.clone(), true),
                _ => (String::new(), false),
            };
            let latency = match slot {
                StatusSlot::Ok(s) => s.latency_ms,
                _ => None,
            };
            MenuRow {
                label: e.name.clone(),
                favicon: match slot {
                    StatusSlot::Ok(s) => s
                        .favicon_png
                        .as_deref()
                        .and_then(|png| favicons.get(&StatusCache::key(e), png)),
                    _ => None,
                },
                enabled: true,
                // No `slot`: a list row's left edge is `floor(width / 2) - 152`,
                // Java integer division on *each* term, which a `Slot`'s
                // `anchor + dx` cannot express (see `server_row_left`). `row_rect`
                // resolves it from `entry.index` instead, which keeps the draw and
                // `app.rs`'s hit-test on one definition all the same.
                entry: Some(ServerEntryView {
                    index: i,
                    motd,
                    motd_is_error,
                    status: status_text,
                    status_is_error,
                    status_sprite: status::status_sprite(state, latency, millis, i),
                    selected: i == nav.server_index(),
                    can_move_up: i > 0,
                    can_move_down: i < last,
                    scroll,
                }),
                ..Default::default()
            }
        })
        .collect();

    // `onSelectedChange`'s three conditional buttons plus the four unconditional
    // ones, in the order they are added to the two footer rows (`:68-125`).
    let has_selection = !entries.is_empty();
    for button in SERVER_LIST_BUTTONS {
        rows.push(MenuRow {
            label: button.label().to_string(),
            enabled: button.enabled(has_selection),
            slot: Some(server_list_footer_slot(button)),
            ..Default::default()
        });
    }

    let mut labels = vec![server_list_title_label()];
    // Not vanilla's: a failed `servers.json` write has no vanilla equivalent
    // (vanilla's `ServerList.save` swallows its own IOException into the log), and
    // a player who adds a server and sees it vanish deserves the reason. Placed
    // just above the footer band so it cannot collide with a row.
    if let Some(err) = nav.save_error() {
        labels.push(MenuLabel {
            text: err.to_uppercase(),
            origin: Origin::ScreenBottom,
            dx: 0.0,
            dy: -(SERVER_LIST_FOOTER_H + LINE_H + 2.0),
            align: Align::Centre,
            colour: FG_BAD,
            scale: 1.0,
        });
    }

    MenuFrame {
        rows,
        // On this screen `selected` is the **footer button** the cursor is over,
        // not the selected server: a list entry carries its own
        // `ServerEntryView::selected`, because vanilla draws the two completely
        // differently (a 1 px row outline versus `widget/button_highlighted`) and
        // both can be visible at once.
        selected: match nav.list_button() {
            Some(b) => entries.len() + b,
            None => usize::MAX,
        },
        vanilla: true,
        labels,
        cursor: nav.menu_cursor(),
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
        // Vanilla's `JoinMultiplayerScreen` (#396): a `HeaderAndFooterLayout`
        // title, the `ServerSelectionList`'s 36 px rows, and seven footer buttons
        // three of which are inactive with nothing selected. Built in its own
        // function because the row content alone is thirty lines of state
        // resolution — see `server_list_frame`.
        Screen::ServerList => Some(server_list_frame(nav, statuses, favicons)),
        // Vanilla's `ManageServerScreen` (the framework conversion this arm
        // used to lack entirely: no row here carried a `slot`, so every
        // widget drew through the pre-#392 centred stack instead of a real
        // `widget/button*`/`widget/text_field` sprite). See
        // `manage_server_slot` for the five widgets' vanilla rects.
        Screen::ServerEdit => {
            let form = nav.form();
            let title = if form.editing.is_some() {
                "Edit Server Info"
            } else {
                "Add Server"
            };
            // Vanilla disables Done rather than printing an error
            // (`ManageServerScreen.java:92-93`) — the greyed `widget/
            // button_disabled` sprite this row now draws *is* the feedback,
            // so no extra text duplicates it.
            let valid = form.is_valid();
            use super::nav::{ADDRESS_FIELD, CANCEL_ROW, DONE_ROW, NAME_FIELD, RESOURCE_PACK_ROW};
            Some(MenuFrame {
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
                        detail: "Server Name".to_string(),
                        enabled: true,
                        field: true,
                        edit: Some(form.fields.name.clone()),
                        slot: Some(manage_server_slot(NAME_FIELD)),
                        ..Default::default()
                    },
                    MenuRow {
                        label: form.address().to_string(),
                        detail: "Server Address".to_string(),
                        enabled: true,
                        field: true,
                        edit: Some(form.fields.address.clone()),
                        slot: Some(manage_server_slot(ADDRESS_FIELD)),
                        ..Default::default()
                    },
                    // Present and inactive — see `RESOURCE_PACK_ROW`'s doc on
                    // why: `ServerEntry` has no `pack_status` to cycle.
                    MenuRow {
                        label: "Server Resource Packs".to_string(),
                        enabled: false,
                        slot: Some(manage_server_slot(RESOURCE_PACK_ROW)),
                        ..Default::default()
                    },
                    MenuRow {
                        label: "Done".to_string(),
                        enabled: valid,
                        slot: Some(manage_server_slot(DONE_ROW)),
                        ..Default::default()
                    },
                    MenuRow {
                        label: "Cancel".to_string(),
                        enabled: true,
                        slot: Some(manage_server_slot(CANCEL_ROW)),
                        ..Default::default()
                    },
                ],
                selected: match form.field() {
                    FormField::Name => NAME_FIELD,
                    FormField::Address => ADDRESS_FIELD,
                },
                hovered: form.hovered_button(),
                vanilla: true,
                labels: vec![
                    MenuLabel {
                        text: title.to_string(),
                        origin: Origin::ScreenTop,
                        dx: 0.0,
                        dy: MANAGE_SERVER_TITLE_Y,
                        align: Align::Centre,
                        colour: LABEL,
                        scale: 1.0,
                    },
                    // Not vanilla — this client's own affordance, kept from
                    // the pre-conversion screen: SRV resolution and the
                    // name-falls-back-to-host rule have no vanilla widget to
                    // announce them (`ServerEntry::split_host_port`,
                    // `EditForm::to_entry`).
                    MenuLabel {
                        text: "Tab switches fields - an empty name uses the host".to_string(),
                        origin: Origin::ScreenBottom,
                        dx: 0.0,
                        dy: -16.0,
                        align: Align::Centre,
                        colour: FG_DIM,
                        scale: 1.0,
                    },
                ],
                ..Default::default()
            })
        }
        // Vanilla's `SelectWorldScreen` (issue #397, then #287): the title, the
        // search box, the six footer buttons — **four of them present and
        // disabled**, Create New World among them, with Play Selected World live
        // since #287 — and the one list row the list has. See
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
                    // The one world the list has (#287), read off the nav so the
                    // row drawn and the world **Play Selected World** launches
                    // are the same fact rather than two constants.
                    world_list_row_label(ws.world_row_label()),
                ],
                ..Default::default()
            })
        }
        // Vanilla's whole `OptionsScreen` tree (issue #55). This used to be two
        // hand-written rows in a centred stack with a key-hint footer; it is now
        // eight pages of `OptionsList` geometry built from a table, with the
        // controls this client does not honour drawn inactive. Every decision —
        // which page, which rows are visible, which are live, where each one
        // sits — belongs to `super::options`; this arm only supplies the three
        // things that live outside it.
        Screen::Settings => Some(super::options::settings_frame(
            nav.settings(),
            nav.options(),
            ui.settings_in_world(),
            nav.options_save_error(),
        )),
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
                SignInView::Requesting => accounts_flow_frame(None, None, false),
                SignInView::Waiting {
                    user_code,
                    verification_uri,
                } => accounts_flow_frame(
                    // Empty means "no code to show", which is the loopback flow:
                    // the browser is already open at the URL and there is nothing
                    // to type. The device-code flow still fills both. `None` is a
                    // shape `accounts_flow_frame` already handles — see the
                    // `Requesting` arm above, which passes it for both.
                    (!user_code.is_empty()).then_some(user_code.as_str()),
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
        // screen and landing as one spike at login. See `error_frame` for the
        // vanilla `DisconnectedScreen` this now reproduces.
        Screen::Error => Some(error_frame(ui.error())),
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
    // A multiplayer-list entry (#396) is placed by `AbstractSelectionList`'s
    // arithmetic, which a `Slot` cannot express: `getRowLeft()` halves the canvas
    // width and the row width *separately* with integer division, so it is not
    // `anchor + dx` for any anchor. Answered here rather than in the caller for
    // this function's whole reason — `app.rs`'s hit-test reads it too, so a second
    // definition is how a click lands on a row the draw put somewhere else.
    //
    // #402: gated on `server_row_visible` first, so a row that is scrolled out
    // of the band or would overflow into the footer reports **no** rect at all,
    // rather than one nothing draws at. That is what keeps a click from landing
    // on a row that is not on screen — `menu_row_at`'s `find` simply keeps
    // scanning past a `None` the same way it already does past the end of
    // `rows`. Contrast the account-row arm below, which still has this gap.
    if let Some(view) = row.entry.as_ref() {
        if !server_row_visible(view.index, height, view.scroll) {
            return None;
        }
        return Some(server_row_rect(view.index, width, view.scroll));
    }
    // An account row (#66/#402) is placed the same way and for the same reason —
    // `floor(width / 2) - floor(305 / 2)` is two integer divisions, not
    // `anchor + dx`. Answered here so the draw and `app.rs`'s hit-test read one
    // definition; note this also reports a rect for a row
    // `accounts_row_visible` would skip, which is the bounded consequence that
    // function documents.
    if let Some(view) = row.account.as_ref() {
        return Some(accounts_row_rect(view.index, width));
    }
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

// -- vanilla's `ManageServerScreen` metrics ----------------------------------
//
// `.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/ManageServerScreen.java`
// — the add/edit-server form `JoinMultiplayerScreen`'s Add/Edit buttons open.
// Every number is transcribed from there, not measured off this pipeline's
// own output.

/// Every widget's width: the two `EditBox`es and the three buttons below them
/// are all `200` (`:33-38,43,55,58`).
const MANAGE_SERVER_W: f32 = 200.0;
/// Every widget's x, relative to [`Origin::ScreenTop`]: `width / 2 - 100`
/// (same lines).
const MANAGE_SERVER_X: f32 = -100.0;
/// The name field's y (`:33`).
const MANAGE_SERVER_NAME_Y: f32 = 66.0;
/// The address field's y (`:38`).
const MANAGE_SERVER_ADDRESS_Y: f32 = 106.0;
/// The Resource Packs button's y, as an offset from [`Origin::TitleTop`]'s own
/// `height / 4 + 48` anchor: `height / 4 + 72` is `+ 24` from there (`:43`).
/// Reusing `TitleTop` rather than a second `height / 4` origin is the same
/// choice [`death_slot`] already made, for the same reason named there.
const MANAGE_SERVER_PACK_DY: f32 = 24.0;
/// Done's y: `height / 4 + 114` is `+ 66` from [`Origin::TitleTop`] (`:55`).
const MANAGE_SERVER_DONE_DY: f32 = 66.0;
/// Cancel's y: `height / 4 + 138` is `+ 90` from [`Origin::TitleTop`] (`:58`).
const MANAGE_SERVER_CANCEL_DY: f32 = 90.0;
/// Where this screen's title is drawn: vanilla `Screen`'s own generic
/// `drawCenteredString(title, width / 2, 20, …)` fallback — `ManageServerScreen`
/// overrides neither `render` nor `renderBackground`/`addTitle`, so its title
/// draws wherever the base `Screen` puts one, same as every simple dialog that
/// does not build a `HeaderAndFooterLayout` of its own.
const MANAGE_SERVER_TITLE_Y: f32 = 20.0;

/// One [`super::Screen::ServerEdit`] widget's [`Slot`] — vanilla's rects at
/// `ManageServerScreen.java:33-62`, read out of the constants above rather
/// than resolved by hand, so a click, a hover and the draw cannot disagree.
/// Row indices are [`super::nav::NAME_FIELD`] and its siblings.
#[must_use]
fn manage_server_slot(row: usize) -> Slot {
    use super::nav::{ADDRESS_FIELD, DONE_ROW, NAME_FIELD, RESOURCE_PACK_ROW};
    match row {
        NAME_FIELD => Slot {
            origin: Origin::ScreenTop,
            dx: MANAGE_SERVER_X,
            dy: MANAGE_SERVER_NAME_Y,
            w: MANAGE_SERVER_W,
            h: EDIT_BOX_H,
        },
        ADDRESS_FIELD => Slot {
            origin: Origin::ScreenTop,
            dx: MANAGE_SERVER_X,
            dy: MANAGE_SERVER_ADDRESS_Y,
            w: MANAGE_SERVER_W,
            h: EDIT_BOX_H,
        },
        RESOURCE_PACK_ROW => Slot {
            origin: Origin::TitleTop,
            dx: MANAGE_SERVER_X,
            dy: MANAGE_SERVER_PACK_DY,
            w: MANAGE_SERVER_W,
            h: WIDGET_H,
        },
        DONE_ROW => Slot {
            origin: Origin::TitleTop,
            dx: MANAGE_SERVER_X,
            dy: MANAGE_SERVER_DONE_DY,
            w: MANAGE_SERVER_W,
            h: WIDGET_H,
        },
        // `CANCEL_ROW`, and any row past it: the caller never asks for one
        // this screen does not have, so the match stays a `usize` rather than
        // an enum to share `Cell`-free indices with `EditForm`'s focus ids.
        _ => Slot {
            origin: Origin::TitleTop,
            dx: MANAGE_SERVER_X,
            dy: MANAGE_SERVER_CANCEL_DY,
            w: MANAGE_SERVER_W,
            h: WIDGET_H,
        },
    }
}

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

/// The two [`super::Screen::ServerEdit`] field rects at a given canvas —
/// vanilla's own `ManageServerScreen.java:33-42` rects, through
/// [`manage_server_slot`].
///
/// Exists so [`super::nav::EditForm::adding`] can seed its two `EditBox`es'
/// geometry before any frame exists — arrow navigation between them is
/// geometric and `displayPos` scrolling is measured against the width, so a
/// freshly-constructed box needs real bounds immediately. Reads the same
/// [`manage_server_slot`] the real per-frame rows resolve through (via their
/// own `slot`), rather than a second computation that could drift from it.
#[must_use]
pub fn field_row_rects(width: f32, height: f32) -> [(f32, f32, f32, f32); 2] {
    use super::nav::{ADDRESS_FIELD, NAME_FIELD};
    [
        manage_server_slot(NAME_FIELD).resolve(width, height),
        manage_server_slot(ADDRESS_FIELD).resolve(width, height),
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

    // A wrapped, bounded block of body text (see `MenuNotice`). Drawn here rather
    // than through `frame.labels` because it is the one piece of menu text whose
    // *content* is not ours — it can carry a server's own response body — so it
    // has to be wrapped in the font this draw measures with and clipped to a rect
    // the layout sizes. Unconditional rather than inside the `frame.vanilla`
    // branch above: nothing about it depends on which layout mode a screen is in.
    if let Some(notice) = frame.notice.as_ref() {
        let (nx, ny, nw, _) = notice_rect(notice, width, height);
        let lines = wrap_bounded(&b, &notice.text, nw, notice_lines(notice, width, height));
        for (i, line) in lines.iter().enumerate() {
            let lw = b.text_width(line, 1.0);
            b.text(
                line,
                (nx + (nw - lw) * 0.5).floor(),
                ny + LINE_H * i as f32,
                1.0,
                notice.colour,
            );
        }
    }

    for (i, row) in frame.rows.iter().enumerate() {
        // A multiplayer-list entry (#396) is neither a button nor a field: it is
        // an `ObjectSelectionList` row with a favicon, two text columns, a status
        // sprite and a quadrant hover overlay. Tested before `slot` because it
        // carries none — `row_rect` places it from `entry.index`.
        if row.entry.is_some() {
            draw_server_entry(&mut b, &frame.rows, i, width, height, frame.cursor);
            continue;
        }
        // An account row (#66/#402) is the same kind of thing one screen over: a
        // 36 px selection-list entry with a head icon and two small text columns,
        // not a button. Tested before `slot` for the same reason — it carries
        // none.
        if row.account.is_some() {
            draw_account_entry(&mut b, &frame.rows, i, width, height);
            continue;
        }
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

/// Draws one multiplayer-list row: the selection outline, the favicon, the name,
/// up to two wrapped MOTD lines, the right-aligned status column, the status
/// sprite, and — when the cursor is inside the row — the join/move-up/move-down
/// overlay on the icon.
///
/// Mirrors `ServerSelectionList.OnlineServerEntry.extractContent`
/// (`ServerSelectionList.java:267-397`) plus `AbstractSelectionList.extractItem`'s
/// selection pass (`:354-370`), and it **decides nothing**: which sprite, which
/// colour and which arrows apply are all resolved into [`ServerEntryView`] by
/// [`server_list_frame`]. What it does own is everything canvas-dependent — the
/// rects, and therefore the quadrant the cursor is in.
///
/// ## Two things that are not vanilla's, named rather than hidden
///
/// - **The MOTD wrap is greedy on whitespace**, where vanilla's `font.split` is a
///   full `StringSplitter` that also breaks inside an over-long word and carries
///   style across the break. A word wider than the column is therefore drawn past
///   the wrap width here instead of being cut; the column is 267 px, so it takes a
///   ~44-character unbroken run to notice.
/// - **The row's own background is the screen's**, not a per-row texture. Vanilla
///   blits `menu_list_background.png` tiled across the whole list band
///   (`AbstractSelectionList.java:226-238`) and draws *no* per-row fill for an
///   unselected row, so an unselected row here correctly paints nothing but its
///   content. The band texture itself is a loose `textures/gui/` PNG (the same
///   89-texture gap `resources.rs` documents) and is left to the flat [`BG`]
///   fill — which is what every other menu screen already draws.
fn draw_server_entry(
    b: &mut Quads<'_>,
    rows: &[MenuRow],
    i: usize,
    width: f32,
    height: f32,
    cursor: Option<(f32, f32)>,
) {
    let Some(row) = rows.get(i) else { return };
    let Some(view) = row.entry.as_ref() else { return };
    // `extractListItems` only draws the rows inside the band (`:346-352`); this is
    // that test, standing in for the scissor this pipeline has no equivalent of.
    // `row_rect` below now performs the same check on the way to its rect
    // (#402), so this one is a fast-out, not the only guard.
    if !server_row_visible(view.index, height, view.scroll) {
        return;
    }
    let Some((x, y, w, h)) = row_rect(rows, i, width, height) else {
        return;
    };

    // `extractItem`: the selected row gets a 1 px outline of `-1` when the list is
    // focused and `-8355712` when it is not, with the interior filled black —
    // drawn *under* the content (`:354-370`). This shell's list is focused
    // whenever the screen is up (there is nowhere else for the keyboard to be, and
    // the footer buttons are mouse-driven), so the outline is the focused one.
    if view.selected {
        b.rect(x, y, w, h, LABEL);
        b.rect(x + 1.0, y + 1.0, w - 2.0, h - 2.0, SERVER_LIST_SELECTION_FILL);
    }

    let (cx, cy, cw, _) = server_row_content_rect(view.index, width, view.scroll);
    let (ix, iy, iw, ih) = server_entry_icon_rect(view.index, width, view.scroll);
    let text_x = cx + SERVER_ENTRY_ICON + SERVER_ENTRY_TEXT_GAP;

    // The favicon, or `FaviconTexture`'s fallback when the server sent none
    // (`:313,438-440`). The mosaic path is this shell's stand-in for a per-server
    // runtime texture — see the module docs.
    if let Some(icon) = row.favicon.as_ref() {
        b.mosaic(icon, ix, iy, iw);
    } else {
        b.sprite(SERVER_UNKNOWN_ICON, ix, iy, iw, ih, LABEL);
    }

    // The status column first, because the name's room depends on where it lands.
    let (icon_x, icon_y, icon_w, icon_h) = server_status_icon_rect(view.index, width, view.scroll);
    b.sprite(view.status_sprite, icon_x, icon_y, icon_w, icon_h, LABEL);
    let status_w = b.text_width(&view.status, 1.0);
    let status_x = icon_x - status_w - SERVER_ENTRY_SPACING;
    if !view.status.is_empty() {
        let colour = if view.status_is_error {
            SERVER_ENTRY_INCOMPATIBLE
        } else {
            SERVER_ENTRY_DIM
        };
        b.text(&view.status, status_x, cy + 1.0, 1.0, colour);
    }

    // `graphics.text(font, serverData.name, contentX + 32 + 3, contentY + 1, -1)`
    // (`:306`). Vanilla does not clip the name — it can and does run under the
    // status column — but this shell has no scissor, so it is clipped to the room
    // the status column leaves rather than drawn over it.
    let name = clip_measured(b, &row.label, (status_x - text_x).max(0.0));
    b.text(name, text_x, cy + 1.0, 1.0, LABEL);

    // Up to two MOTD lines at `contentY + 12 + 9 * i`, wrapped to
    // `contentWidth - 32 - 2` (`:307-311`).
    let motd_colour = if view.motd_is_error {
        SERVER_ENTRY_BAD
    } else {
        SERVER_ENTRY_DIM
    };
    let wrap_w = (cw - SERVER_ENTRY_MOTD_INSET).max(0.0);
    for (line, text) in wrap_measured(b, &view.motd, wrap_w, SERVER_ENTRY_MOTD_LINES)
        .iter()
        .enumerate()
    {
        b.text(
            text,
            text_x,
            cy + SERVER_ENTRY_MOTD_Y + LINE_H * line as f32,
            1.0,
            motd_colour,
        );
    }

    // The hover overlay (`:364-395`). All three sprites blit at the *same* 32×32
    // icon rect, and only the one whose quadrant holds the cursor is drawn
    // highlighted — so the discriminator is position, not which row is hovered.
    let Some((mx, my)) = cursor else { return };
    if mx < x || mx >= x + w || my < y || my >= y + h {
        return;
    }
    b.rect(ix, iy, iw, ih, SERVER_ICON_DARKEN);
    let (rx, ry) = (mx - ix, my - iy);
    let pick = |hit: bool, sprites: (&'static str, &'static str)| {
        if hit { sprites.1 } else { sprites.0 }
    };
    let mut blit = |id: &'static str| b.sprite(id, ix, iy, iw, ih, LABEL);
    blit(pick(
        widget::over_right_half(rx, ry, iw),
        SERVER_JOIN_SPRITES,
    ));
    if view.can_move_up {
        blit(pick(
            widget::over_top_left_quarter(rx, ry, iw),
            SERVER_MOVE_UP_SPRITES,
        ));
    }
    if view.can_move_down {
        blit(pick(
            widget::over_bottom_left_quarter(rx, ry, iw),
            SERVER_MOVE_DOWN_SPRITES,
        ));
    }
}

/// Greedy whitespace wrap of `s` to at most `max_lines` lines of `max_px`, in
/// whatever font `b` draws with — this shell's stand-in for `Font.split`.
///
/// Explicit `\n`s break a line too, because a MOTD is a chat component that has
/// already been flattened to text by `lodestone_net` and may carry them; vanilla
/// splits on both.
///
/// A single word wider than `max_px` is *not* broken (see [`draw_server_entry`]'s
/// note), and lines past `max_lines` are dropped rather than truncated with an
/// ellipsis, because the font has no ellipsis glyph.
fn wrap_measured(b: &Quads<'_>, s: &str, max_px: f32, max_lines: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for paragraph in s.split('\n') {
        // Per paragraph, not per call: without this the first word after a `\n`
        // would be appended to the previous paragraph's last line whenever it
        // happened to fit, which is the one thing splitting on `\n` exists to stop.
        let mut open_line = false;
        for word in paragraph.split_whitespace() {
            if out.len() >= max_lines {
                return out;
            }
            let fits = open_line
                && out.last().is_some_and(|line| {
                    b.text_width(&format!("{line} {word}"), 1.0) <= max_px
                });
            if fits {
                if let Some(line) = out.last_mut() {
                    line.push(' ');
                    line.push_str(word);
                }
            } else {
                // A word that does not fit starts a line rather than overflowing
                // the one before it.
                out.push(word.to_string());
                open_line = true;
            }
        }
        // A blank line in the MOTD is a line: vanilla's split keeps it, and losing
        // it would pull a two-line MOTD's second line up into the first's place.
        if out.len() < max_lines && paragraph.trim().is_empty() {
            out.push(String::new());
        }
    }
    out.truncate(max_lines);
    out
}

/// [`wrap_measured`], except that a word wider than the column is **broken**
/// instead of overflowing it. Used by [`MenuNotice`].
///
/// ## Why this is a second function rather than a flag on the first
///
/// [`wrap_measured`] deliberately does *not* break inside a word, because
/// `ServerSelectionList` wraps a MOTD with `Font.split` and the difference only
/// shows on a ~44-character unbroken run — a documented simplification of the
/// multiplayer screen, and not one worth changing under it.
///
/// For a notice the simplification is a **bug**, not a rounding error, and it is
/// the bug that was reported: an `AuthError` can carry a snippet of a raw HTTP
/// response body, and JSON has no whitespace in it at all, so a whitespace-only
/// wrap emits one enormous line and the greedy fallback ("a word that does not
/// fit starts a line") does not save it. This is also *closer* to vanilla than
/// its sibling — `StringSplitter` breaks mid-word too — so the two are not a
/// fidelity choice, they are two different requirements.
///
/// The single-glyph guard is load-bearing: at a column narrower than one
/// character [`clip_measured`] returns `""`, and pushing an empty line forever is
/// how this would hang instead of drawing.
fn wrap_bounded(b: &Quads<'_>, s: &str, max_px: f32, max_lines: usize) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    if max_lines == 0 {
        return out;
    }
    for paragraph in s.split('\n') {
        // Per paragraph, for `wrap_measured`'s reason: an explicit newline must
        // not be swallowed by the next word happening to fit.
        let mut open_line = false;
        for word in paragraph.split_whitespace() {
            if out.len() >= max_lines {
                return out;
            }
            // Extend the open line if the whole word still fits on it. The fit is
            // computed into a `bool` first, exactly as `wrap_measured` does it:
            // holding the `out.last()` borrow across the `out.last_mut()` that
            // follows would not compile.
            let fits = open_line
                && out
                    .last()
                    .is_some_and(|line| b.text_width(&format!("{line} {word}"), 1.0) <= max_px);
            if fits {
                if let Some(line) = out.last_mut() {
                    line.push(' ');
                    line.push_str(word);
                }
                continue;
            }
            // Otherwise the word starts a line — and keeps starting lines until
            // what is left of it fits on one.
            let mut rest = word;
            loop {
                if out.len() >= max_lines {
                    return out;
                }
                let head = clip_measured(b, rest, max_px);
                let head = if head.is_empty() {
                    match rest.char_indices().nth(1) {
                        Some((i, _)) => &rest[..i],
                        None => rest,
                    }
                } else {
                    head
                };
                out.push(head.to_string());
                rest = &rest[head.len()..];
                if rest.is_empty() {
                    break;
                }
            }
            open_line = true;
        }
        if out.len() < max_lines && paragraph.trim().is_empty() {
            out.push(String::new());
        }
    }
    out.truncate(max_lines);
    out
}

/// Draws one account-list row: the cursor outline, the head icon, the username,
/// the "Selected" marker and the row's small detail line.
///
/// [`draw_server_entry`]'s shape at [`ACCOUNTS_ITEM_H`] pitch, minus the favicon
/// quadrants (there is nothing to reorder or join on this screen) and minus the
/// status sprite. Like that function it **decides nothing**: which row is
/// outlined and what the three text columns say are resolved into the row and its
/// [`AccountEntryView`] by [`accounts_idle_frame`]; what it owns is the
/// canvas-dependent part, which is the rects.
///
/// Every text column is measured and clipped in the font `b` draws with, and the
/// name is clipped to the room the marker leaves rather than being drawn under it
/// — this pipeline has no scissor, so an over-long username would otherwise
/// overprint the marker instead of being cut by it.
fn draw_account_entry(b: &mut Quads<'_>, rows: &[MenuRow], i: usize, width: f32, height: f32) {
    let Some(row) = rows.get(i) else { return };
    let Some(view) = row.account.as_ref() else {
        return;
    };
    if !accounts_row_visible(view.index, height) {
        return;
    }
    let Some((x, y, w, h)) = row_rect(rows, i, width, height) else {
        return;
    };

    // `AbstractSelectionList.extractItem`'s selection pass: a 1 px outline with
    // the interior filled black, drawn *under* the content (`:354-370`). The
    // outline is the focused variant because this screen's list is focused
    // whenever it is up — the buttons are a separate cursor (see
    // `accounts_idle_frame`).
    if view.selected {
        b.rect(x, y, w, h, LABEL);
        b.rect(x + 1.0, y + 1.0, w - 2.0, h - 2.0, ACCOUNTS_SELECTION_FILL);
    }

    let (cx, cy, cw, _) = accounts_row_content_rect(view.index, width);
    let text_x = cx + ACCOUNTS_HEAD_ICON + ACCOUNTS_TEXT_GAP;

    // The head, through the same `FaviconMosaic` path a server's favicon takes —
    // see `MenuRow::head` on why a head is not a second kind of drawable.
    if let Some(head) = row.head.as_ref() {
        b.mosaic(head, cx, cy, ACCOUNTS_HEAD_ICON);
    }

    // The marker first, because the name's room depends on where it lands.
    let marker_w = b.text_width(&row.trailing, 1.0);
    let marker_x = cx + cw - ACCOUNTS_SPACING - marker_w;
    if !row.trailing.is_empty() {
        b.text(&row.trailing, marker_x, cy + 1.0, 1.0, LABEL);
    }
    let name = clip_measured(b, &row.label, (marker_x - ACCOUNTS_SPACING - text_x).max(0.0));
    b.text(name, text_x, cy + 1.0, 1.0, LABEL);

    let detail_room = (cx + cw - ACCOUNTS_SPACING - text_x).max(0.0);
    let detail = clip_measured(b, &row.detail, detail_room);
    b.text(detail, text_x, cy + ACCOUNTS_DETAIL_Y, 1.0, ACCOUNTS_DIM);
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
    // A settings slider carries `AbstractSliderButton`'s track instead of
    // `AbstractButton`'s three `widget/button*` states (issue #55). Which sprite
    // set a row gets is still the *widget's* decision, not this function's — the
    // only thing decided here is which kind of widget the row is.
    let mut widget = if row.slider {
        Widget::slider(x, y, w, h, row.label.as_str())
    } else {
        Widget::button(x, y, w, h, row.label.as_str())
    };
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
    //
    // A slider asks a *different* question — `AbstractSliderButton.getSprite()`
    // passes `isActive()` and `isFocused()` alone, so hovering one does not
    // highlight it (`AbstractSliderButton.java:36-38`). Both predicates live on
    // `Widget`; neither is re-derived here.
    let background = if row.slider {
        widget.slider_background_sprite()
    } else {
        widget.background_sprite()
    };
    match background {
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
/// sprite pipeline, and a growable dynamic vertex buffer for each, plus the
/// cubemap panorama ([`crate::menu::panorama`]) that draws behind all three on an
/// out-of-world screen. Drawn in a `Clear` pass for a screen that owns the frame
/// and a `Load` pass for the pause overlay.
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
    /// The title screen's spinning cubemap, attached lazily on the first draw
    /// (see [`MenuRenderer::ensure_panorama`]). `None` leaves every screen on the
    /// flat [`BG`] backdrop, which is the pre-panorama behaviour.
    panorama: Option<PanoramaRenderer>,
    /// Whether the lazy panorama load has already been tried — same purpose as
    /// [`Self::gui_attempted`]: without it a jar-less run re-decodes six PNGs
    /// every frame.
    panorama_attempted: bool,
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
            panorama: None,
            panorama_attempted: false,
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

    /// Whether the title screen's cubemap panorama is bound, i.e. whether the
    /// out-of-world screens draw vanilla's spinning sky rather than the flat
    /// [`BG`] backdrop.
    ///
    /// Same discipline as [`Self::gui_attached`]: a gate that means to measure the
    /// panorama **must assert this**, because a jar-less run degrades silently to
    /// a fill that satisfies any "something drew here" assertion.
    #[must_use]
    pub fn panorama_attached(&self) -> bool {
        self.panorama.is_some()
    }

    /// How many of the bound panorama's six faces came from the launcher's
    /// asset-object store rather than `client.jar` — 6 is vanilla's real art, 0 is
    /// the jar's 1×1 grey stubs. `0` when no panorama is bound at all.
    ///
    /// `panorama_attached()` is **not** enough for a gate that means to measure the
    /// real sky: the jar stubs bind and draw perfectly, as a flat colour. See
    /// [`crate::asset_objects`].
    #[must_use]
    pub fn panorama_faces_from_object_store(&self) -> usize {
        self.panorama
            .as_ref()
            .map_or(0, PanoramaRenderer::faces_from_object_store)
    }

    /// Bind a panorama cubemap: uploads the six layers and builds its pipeline.
    ///
    /// Public so a gate can hand in a synthetic cubemap with six distinguishable
    /// faces — which is the only way to check the [`panorama::FACE_SUFFIXES`]
    /// order from pixels, since vanilla's shipped faces in 26.2 are a single flat
    /// grey (see `docs/menu-panorama.md`).
    pub fn attach_panorama(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        faces: &PanoramaFaces,
    ) {
        self.panorama_attempted = true;
        self.panorama = Some(PanoramaRenderer::new(
            device,
            queue,
            self.color_format,
            faces,
        ));
    }

    /// Drop back to the flat [`BG`] backdrop. The executed negative control for
    /// every "the panorama reached pixels" assertion.
    pub fn detach_panorama(&mut self) {
        self.panorama = None;
        // As `detach_gui`: leave the attempted flag set so the next draw does not
        // helpfully undo the control.
        self.panorama_attempted = true;
    }

    /// Load and bind the panorama cubemap on first use — twin of
    /// [`Self::ensure_gui`], and lazy for the same reason (the upload needs a
    /// `Queue`, which only the draw paths have).
    fn ensure_panorama(&mut self, device: &wgpu::Device, queue: &wgpu::Queue) {
        if self.panorama_attempted {
            return;
        }
        self.panorama_attempted = true;
        if let Some(faces) = crate::resources::load_panorama() {
            self.attach_panorama(device, queue, &faces);
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
        // The panorama is vanilla's out-of-world background: `extractBackground`
        // draws it whenever `minecraft.level == null`, which for this renderer is
        // exactly `!frame.overlay` (the one overlay frame is the pause menu, drawn
        // over a live world). See `docs/menu-panorama.md`.
        // `frame.logo` is set for the title screen and nothing else, which is the
        // one screen whose `extractBackground` override is empty.
        let panorama_dim = panorama::dim_for_screen(frame.logo);
        if !frame.overlay {
            self.ensure_panorama(device, queue);
            if let Some(pano) = self.panorama.as_mut() {
                pano.advance(std::time::Instant::now());
                pano.prepare(queue, width, height, panorama_dim);
            }
        }
        let panorama_drawn = !frame.overlay && self.panorama.is_some();
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

        // Three colour/sprite draws, one pass (four with the panorama in front of
        // them). The split is `MenuGeometry::backdrop_floats`:
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
            // The panorama replaces the flat backdrop rather than sitting under
            // it: it covers every pixel (you are inside a closed cube), so an
            // opaque `BG` quad drawn afterwards would hide it entirely. The
            // `menu_background.png` wash vanilla puts on top on every screen but
            // the title screen travels as `panorama_dim` in its own shader — see
            // `panorama::dim_for_screen`, and `docs/menu-panorama.md` on why the
            // multiply and a black quad at alpha 64/255 are the same operation.
            if let Some(pano) = self.panorama.as_ref()
                && panorama_drawn
            {
                pano.draw(&mut pass);
            }
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.buffer.slice(..));
            if backdrop_verts > 0 && !panorama_drawn {
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
        // `Screen::ALL`, not a list restated here: this loop's own copy was a
        // 12-entry literal plus an `assert_eq!(reached, 12)`, and #397's
        // `WorldSelect` made both stale at once — a completeness test defeated by
        // the very thing it exists to notice. The `match` below stays exhaustive,
        // which is what forces a new variant to be given a way to be *reached*;
        // `Screen::ALL`'s own docs say what that does and does not guarantee.
        for screen in Screen::ALL {
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
        // Derived, not restated. This no longer catches "a screen was added"
        // (`Screen::ALL` is what does, as far as anything can) — what it still
        // catches is this loop silently skipping one, e.g. a `continue` added to
        // the reach-the-screen `match` above.
        assert_eq!(
            reached,
            Screen::ALL.len(),
            "the loop skipped a screen it was handed"
        );
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
                // Our own protocol, so the row resolves to
                // `ServerState::Successful` and shows a player count rather than
                // the red version string an incompatible server gets.
                protocol: Some(crate::menu::status::STATUS_PROTOCOL),
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
        assert_eq!(
            f.rows.len(),
            1 + crate::menu::nav::SERVER_LIST_BUTTONS.len(),
            "one entry plus vanilla's seven footer buttons"
        );
        assert_eq!(f.rows[0].label, "HOME");
        let view = f.rows[0].entry.as_ref().expect("row 0 is a list entry");
        // The **whole** MOTD, newline included: the wrap to two lines happens at
        // draw time, in the font the draw measures with (`wrap_measured`).
        assert_eq!(view.motd, "A LODESTONE SERVER\nsecond line");
        assert!(!view.motd_is_error);
        // The status column is the player count, not the latency: vanilla puts
        // `formatPlayerCount` there and the round-trip only in the ping *sprite*
        // and its tooltip (`ServerStatusPinger.java:88`).
        assert_eq!(view.status, "3/20");
        assert!(!view.status_is_error);
        // 12 ms is the fastest bucket, so five bars. Asserted by identity — a gate
        // that only proved "a ping sprite drew" passes on all five.
        assert_eq!(view.status_sprite, "server_list/ping_5");
        assert!(view.selected, "the one row is the selected one");
        assert!(
            !view.can_move_up && !view.can_move_down,
            "a single row has nowhere to move"
        );
    }

    /// The three states that are *not* "answered by a compatible server" each get
    /// their own sprite, and the assertion is by **identity**: a gate that only
    /// proves a ping bar exists passes on all four rendering the same bar.
    #[test]
    fn every_row_state_resolves_to_its_own_status_sprite() {
        use crate::menu::status::{PINGING_SPRITES, ServerStatus};

        let mut nav = test_nav("states");
        let mut ui = UiState::new();
        add_server(&mut nav, &mut ui, "SLOW", "slow.example");

        // A compatible server, 700 ms — the fourth bucket down.
        let mut statuses = StatusCache::with_probe(std::sync::Arc::new(|_| {
            Ok(ServerStatus {
                motd: "hi".into(),
                players: "1/1".into(),
                protocol: Some(crate::menu::status::STATUS_PROTOCOL),
                latency_ms: Some(700),
                ..Default::default()
            })
        }));
        let entries = nav.list().entries().to_vec();
        // While the probe is in flight the row is `Pending`, which must animate.
        // Read *before* draining, and only asserted to be one of the five frames:
        // which one depends on a clock.
        statuses.refresh(&entries);
        let mut fav = FaviconCache::new();
        let pending = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        let pending_view = pending.rows[0].entry.clone().unwrap();
        assert!(
            PINGING_SPRITES.contains(&pending_view.status_sprite),
            "an in-flight row must animate, got {}",
            pending_view.status_sprite
        );
        assert_eq!(
            pending_view.motd, "Pinging...",
            "vanilla overwrites the MOTD while pinging"
        );
        assert!(
            pending_view.status.is_empty(),
            "and blanks the status column"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while statuses.pump() == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let slow = frame_for(&ui, &nav, &statuses, &mut fav).unwrap().rows[0]
            .entry
            .clone()
            .unwrap();
        assert_eq!(slow.status_sprite, "server_list/ping_2", "700 ms is two bars");

        // An answered server speaking a different protocol is *incompatible*, not
        // unreachable: its own sprite, and its version in place of a player count.
        let mut old = StatusCache::with_probe(std::sync::Arc::new(|_| {
            Ok(ServerStatus {
                motd: "hi".into(),
                players: "1/1".into(),
                version: "1.21.11".into(),
                protocol: Some(1),
                latency_ms: Some(5),
                ..Default::default()
            })
        }));
        old.refresh(&entries);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while old.pump() == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let view = frame_for(&ui, &nav, &old, &mut fav).unwrap().rows[0]
            .entry
            .clone()
            .unwrap();
        assert_eq!(view.status_sprite, "server_list/incompatible");
        assert_eq!(view.status, "1.21.11", "the version, where the count goes");
        assert!(view.status_is_error, "and in red");

        // And the four sprites are four different sprites.
        let mut all = vec![
            pending_view.status_sprite,
            slow.status_sprite,
            view.status_sprite,
        ];
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), 3, "two states share a sprite: {all:?}");
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
        let view = f.rows[0].entry.as_ref().expect("row 0 is a list entry");
        // The reason goes in the **MOTD** column and the status column stays
        // empty, which is vanilla's own arrangement: `onPingFailed` sets
        // `data.motd = CANT_CONNECT_MESSAGE` and `data.status` to empty
        // (`ServerStatusPinger.java:168-169`).
        assert_eq!(view.motd, "connection refused");
        assert!(
            view.motd_is_error,
            "a failure must be visually distinct from a MOTD"
        );
        assert!(view.status.is_empty(), "no player count to show");
        assert_eq!(
            view.status_sprite, "server_list/unreachable",
            "an unreachable row gets its own sprite, not a ping bar"
        );
    }

    /// With nothing to act on, Join / Edit / Delete are **present and inactive** —
    /// `onSelectedChange`'s three, which is #393's disabled path reaching its first
    /// list screen. Direct Connection is inactive whatever the selection.
    ///
    /// The control is executed rather than described: adding a server must flip all
    /// three, or "they are disabled" would pass on a screen whose buttons are
    /// *always* disabled.
    #[test]
    fn the_footer_buttons_are_present_and_three_are_inactive_with_no_selection() {
        use crate::menu::nav::{SERVER_LIST_BUTTONS, ServerListButton as B};

        let mut nav = test_nav("emptylist");
        let mut ui = UiState::new();
        ui.open_server_list();
        let statuses = StatusCache::with_probe(unavailable_probe());
        let mut fav = FaviconCache::new();
        let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

        // Every one of vanilla's seven is on screen even with an empty list — a
        // missing button is a layout that reads wrong, a greyed-out one reads
        // exactly like vanilla with the feature unavailable.
        assert_eq!(f.rows.len(), SERVER_LIST_BUTTONS.len());
        let row_of = |b: B| {
            SERVER_LIST_BUTTONS
                .iter()
                .position(|x| *x == b)
                .expect("every button is in the table")
        };
        for (i, button) in SERVER_LIST_BUTTONS.iter().enumerate() {
            assert_eq!(
                f.rows[i].label,
                button.label(),
                "row {i} is not {button:?} — the footer order is what click() assumes"
            );
        }
        for b in [B::Select, B::Edit, B::Delete, B::Direct] {
            assert!(!f.rows[row_of(b)].enabled, "{b:?} must be inactive");
        }
        for b in [B::Add, B::Refresh, B::Back] {
            assert!(f.rows[row_of(b)].enabled, "{b:?} must be active");
        }

        // Control: a selection enables three of the four, and Direct Connection
        // stays inactive because nothing here can honour it.
        add_server(&mut nav, &mut ui, "HOME", "mc.example.com");
        let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        let base = 1;
        for b in [B::Select, B::Edit, B::Delete] {
            assert!(
                f.rows[base + row_of(b)].enabled,
                "{b:?} must be active once a row exists"
            );
        }
        assert!(
            !f.rows[base + row_of(B::Direct)].enabled,
            "Direct Connection has no screen to open, selection or not"
        );
    }

    /// Vanilla's own rects for `JoinMultiplayerScreen` at 854×480, hand-derived
    /// from the Java rather than read back out of the layout — `CLAUDE.md`'s rule
    /// that an expected value must originate outside the code under test.
    ///
    /// The derivation, which is what a future reader has to be able to check:
    ///
    /// - `HeaderAndFooterLayout(this, 33, 60)`, so `getContentHeight()` is
    ///   `480 - 33 - 60` = **387**, and the list is sized to exactly that
    ///   (`:61-62`). The content clamp is then `min(33 + 30, 480 - 60 - 387)` =
    ///   `min(63, 33)` = **33** — flush under the header, because the content
    ///   fills the band.
    /// - `getFirstEntryY()` is `getY() + 2` = **35**, and rows stack by
    ///   `itemHeight` 36 with no gap.
    /// - `getRowLeft()` is `0 + 854/2 - 305/2` = `427 - 152` = **275**. Note the
    ///   two halvings are separate integer divisions; `(854 - 305) / 2` is 274.
    /// - `CONTENT_PADDING` insets the entry by 2 a side, so content is
    ///   `(277, 37, 301, 32)` and the 32 is exactly the favicon's height.
    /// - `statusIconX = getContentRight() - 10 - 5` = `578 - 15` = **563**, at
    ///   `getContentY()` = 37 — the status icon is *not* vertically centred.
    /// - The title is a 9 px `StringWidget` centred in the 854×33 header frame:
    ///   `round((33 - 9) / 2)` = **12** from the top, on `width / 2`.
    /// - The footer column is `3*100 + 2*4` = 308 wide on its top row and
    ///   `4*74 + 3*4` = 308 on its lower one — they match, which is why the
    ///   column is 308 and both rows sit at its left edge — and `20 + 4 + 20` = 44
    ///   tall. Centred in the 854×60 footer frame pinned at y 420:
    ///   `((854 - 308) / 2, 420 + (60 - 44) / 2)` = **(273, 428)**.
    #[test]
    fn the_server_list_rects_are_vanillas_own() {
        use crate::menu::nav::{SERVER_LIST_BUTTONS, ServerListButton as B};

        let expected = [
            // Top row: 100 wide, 104 apart.
            (B::Select, (273.0, 428.0, 100.0, 20.0)),
            (B::Direct, (377.0, 428.0, 100.0, 20.0)),
            (B::Add, (481.0, 428.0, 100.0, 20.0)),
            // Lower row: 74 wide, 78 apart, 24 px below.
            (B::Edit, (273.0, 452.0, 74.0, 20.0)),
            (B::Delete, (351.0, 452.0, 74.0, 20.0)),
            (B::Refresh, (429.0, 452.0, 74.0, 20.0)),
            (B::Back, (507.0, 452.0, 74.0, 20.0)),
        ];
        for (button, want) in expected {
            assert_eq!(
                server_list_footer_slot(button).resolve(V_W, V_H),
                want,
                "{button:?} is not where vanilla puts it"
            );
            // The enum's declared width and the arranged one must agree, or the
            // footer was built with its two rows swapped.
            assert_eq!(
                server_list_footer_slot(button).w,
                button.width(),
                "{button:?}'s arranged width is not its declared one"
            );
        }
        // Both footer gutters are 4 — this screen's, not the pause screen's 8.
        let (sx, _, sw, _) = server_list_footer_slot(B::Select).resolve(V_W, V_H);
        let (dx, ..) = server_list_footer_slot(B::Direct).resolve(V_W, V_H);
        assert_eq!(dx - (sx + sw), 4.0, "top row spacing");
        let (ex, _, ew, _) = server_list_footer_slot(B::Edit).resolve(V_W, V_H);
        let (delx, ..) = server_list_footer_slot(B::Delete).resolve(V_W, V_H);
        assert_eq!(delx - (ex + ew), 4.0, "lower row spacing");
        assert_eq!(SERVER_LIST_BUTTONS.len(), 7);

        // The rows, unscrolled.
        assert_eq!(server_row_rect(0, V_W, 0), (275.0, 35.0, 305.0, 36.0));
        assert_eq!(
            server_row_rect(1, V_W, 0),
            (275.0, 71.0, 305.0, 36.0),
            "rows stack by itemHeight with no gap"
        );
        assert_eq!(
            server_row_content_rect(0, V_W, 0),
            (277.0, 37.0, 301.0, 32.0),
            "CONTENT_PADDING insets the entry by 2, and 36 - 4 is the icon's 32"
        );
        assert_eq!(server_entry_icon_rect(0, V_W, 0), (277.0, 37.0, 32.0, 32.0));
        assert_eq!(
            server_status_icon_rect(0, V_W, 0),
            (563.0, 37.0, 10.0, 8.0),
            "contentRight - 10 - 5, at contentY"
        );
        // A scroll of 1 shifts every row up by one `itemHeight` (#402): row 1
        // at scroll 0 lands exactly where row 0 sits at scroll 1.
        assert_eq!(
            server_row_rect(1, V_W, 1),
            server_row_rect(0, V_W, 0),
            "scrolling by one row is the same shift as re-indexing by one row"
        );
        // `getRowLeft()` is not `(width - rowWidth) / 2`, and the difference shows
        // at an odd canvas: 855/2 = 427 either way here, 856 is where they split.
        assert_eq!(server_row_left(856.0), 276.0, "floor(856/2) - 152");
        assert_eq!(
            (856.0 - SERVER_LIST_ROW_W) / 2.0,
            275.5,
            "control: the naive centring is half a pixel off"
        );

        // The title.
        let title = server_list_title_label();
        assert_eq!(title.text, crate::menu::nav::SERVER_LIST_TITLE);
        assert_eq!((title.dx, title.dy), (0.0, 12.0));
        assert_eq!(title.align, Align::Centre);
        assert_eq!(title.origin, Origin::ScreenTop);
    }

    /// The whole screen is arranged **once**, at a reference canvas, and every
    /// rect is then expressed relative to an [`Origin`]. That is only sound if the
    /// arrangement is canvas-independent once so expressed — so re-arrange at three
    /// sizes and require identical slots.
    ///
    /// This is what stands between the screen and being correct at 854×480 and
    /// wrong everywhere else. It holds because the footer column measures 308 at
    /// any width and the content band always starts at the header height (the list
    /// is sized to `getContentHeight()`, so the clamp always picks it).
    ///
    /// **Even widths only, and that is a real limit rather than a convenient
    /// choice.** `Origin::ScreenBottom`'s x is `width * 0.5` unrounded, while
    /// `FrameLayout` truncates its centring, so at an odd logical width the two
    /// disagree by half a pixel — the same limit `Screen::WorldSelect`'s footer
    /// has, for the same reason. It is invisible in practice because
    /// `logical_canvas` divides the framebuffer by an integer scale and can
    /// produce a fractional width anyway; the row geometry, which *is* floored
    /// per-term, is exact at every width (see `server_row_left`).
    #[test]
    fn the_server_list_slots_do_not_depend_on_the_reference_canvas() {
        let reference = ServerListBlock::at(SERVER_LIST_REF_CANVAS.0, SERVER_LIST_REF_CANVAS.1);
        for (w, h) in [(320.0, 240.0), (1280.0, 720.0), (1920.0, 1080.0)] {
            let other = ServerListBlock::at(w, h);
            assert_eq!(
                other.content_top, reference.content_top,
                "the content band moved at {w}x{h}"
            );
            for i in 0..reference.footer.len() {
                assert_eq!(
                    other.footer_slot(i),
                    reference.footer_slot(i),
                    "footer slot {i} moved at {w}x{h}"
                );
            }
            // And the slot really resolves to where that canvas' own arrangement
            // put it, which is the assertion that makes the two derivations
            // independent rather than merely equal to each other.
            for (i, want) in other.footer.iter().enumerate() {
                assert_eq!(
                    reference.footer_slot(i).resolve(w, h),
                    *want,
                    "footer slot {i} does not land on {w}x{h}'s own arrangement"
                );
            }
        }
    }

    /// A nav sitting on the multiplayer screen with `servers` saved, reached the
    /// way a player reaches it.
    fn list_nav(tag: &str, servers: &[(&str, &str)]) -> (MenuNav, UiState) {
        let mut nav = test_nav(tag);
        let mut ui = UiState::new();
        ui.open_server_list();
        for (name, address) in servers {
            add_server(&mut nav, &mut ui, name, address);
        }
        assert_eq!(ui.screen(), Screen::ServerList, "premise: the list is up");
        assert_eq!(nav.list().len(), servers.len());
        (nav, ui)
    }

    /// The bounding box of every colour-stream vertex drawn in exactly `want`, in
    /// logical pixels, or `None` if that colour never appeared.
    ///
    /// Keyed on the **colour** rather than on a rect, because the thing under test
    /// here is *where* a mark landed: a rect-shaped detector would need to know the
    /// answer first. Reports a box, never a count, per `CLAUDE.md`.
    fn colour_bounds(colour: &[f32], w: f32, h: f32, want: [f32; 4]) -> Option<(f32, f32, f32, f32)> {
        let (mut x0, mut y0) = (f32::MAX, f32::MAX);
        let (mut x1, mut y1) = (f32::MIN, f32::MIN);
        let mut seen = false;
        for v in colour.chunks_exact(STRIDE) {
            if (2..6).any(|c| (v[c] - want[c - 2]).abs() > 1e-4) {
                continue;
            }
            seen = true;
            let px = (v[0] + 1.0) * 0.5 * w;
            let py = (1.0 - v[1]) * 0.5 * h;
            x0 = x0.min(px);
            y0 = y0.min(py);
            x1 = x1.max(px);
            y1 = y1.max(py);
        }
        seen.then_some((x0, y0, x1 - x0, y1 - y0))
    }

    /// #376's rule applied to this screen: the discriminator for a hover overlay
    /// is **position**. A gate that proved "an overlay drew in a row" would pass
    /// on an overlay nailed to row 0.
    ///
    /// The measurement is the icon-dim quad (`fill(…, -1601138544)`), which is the
    /// one part of the overlay that reaches the *colour* stream — the three arrow
    /// sprites need an atlas, and they get their own gate below.
    #[test]
    fn the_hover_overlay_follows_the_cursor_rather_than_the_row() {
        let (nav, ui) = list_nav("hover", &[("A", "a.example"), ("B", "b.example")]);
        let statuses = StatusCache::with_probe(unavailable_probe());
        let mut fav = FaviconCache::new();
        let mut f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

        let dim_at = |f: &MenuFrame<'_>| {
            colour_bounds(&geometry(f, V_W, V_H), V_W, V_H, SERVER_ICON_DARKEN)
        };
        // A tolerance, not `assert_eq!`: the measurement round-trips through NDC
        // and back (`2x/w - 1` then its inverse), so 277.0 comes out 277.00003.
        let is = |got: Option<(f32, f32, f32, f32)>, want: (f32, f32, f32, f32), what: &str| {
            let g = got.unwrap_or_else(|| panic!("{what}: nothing drew, expected {want:?}"));
            let near = (g.0 - want.0).abs() < 0.01
                && (g.1 - want.1).abs() < 0.01
                && (g.2 - want.2).abs() < 0.01
                && (g.3 - want.3).abs() < 0.01;
            assert!(near, "{what}: overlay at {g:?}, expected {want:?}");
        };

        // No cursor at all — a keyboard-only session, and every hermetic test.
        // This is also the control that makes the absences below real: if the
        // detector could not see the quad, every assertion here would pass on a
        // screen that never drew one.
        f.cursor = None;
        assert_eq!(dim_at(&f), None, "no cursor must mean no hover overlay");

        // Row 0, then row 1: the same overlay, one `itemHeight` lower.
        let icon0 = server_entry_icon_rect(0, V_W, 0);
        f.cursor = Some((icon0.0 + 4.0, icon0.1 + 4.0));
        is(dim_at(&f), icon0, "row 0's icon");
        let icon1 = server_entry_icon_rect(1, V_W, 0);
        f.cursor = Some((icon1.0 + 4.0, icon1.1 + 20.0));
        is(dim_at(&f), icon1, "row 1's icon");
        assert_eq!(
            icon1.1 - icon0.1,
            SERVER_LIST_ITEM_H,
            "premise: the two rows are a row apart, or this proves nothing"
        );

        // Vanilla's `hovered` is the *row*, not the icon: the cursor anywhere in
        // the row lights the icon up, and anywhere outside it does not.
        f.cursor = Some((icon0.0 + 200.0, icon0.1 + 4.0));
        is(dim_at(&f), icon0, "the whole row hovers");
        f.cursor = Some((10.0, 10.0));
        assert_eq!(dim_at(&f), None, "the backdrop is not a row");
    }

    /// A synthetic pack carrying the `server_list/*` sprites plus the button set,
    /// so sprite *identity* can be asserted with no jar — `button_pack`'s trick.
    fn server_list_pack() -> lodestone_assets::ResourceManager {
        use crate::menu::status::{PING_SPRITES, PINGING_SPRITES};
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
        // Every status sprite at vanilla's own 10×8, and the three 32×32 overlays.
        for id in PING_SPRITES.iter().chain(PINGING_SPRITES.iter()).chain([
            &crate::menu::status::INCOMPATIBLE_SPRITE,
            &crate::menu::status::UNREACHABLE_SPRITE,
        ]) {
            src.insert(
                format!("assets/minecraft/textures/gui/sprites/{id}.png"),
                solid_rgba_png(10, 8, [40, 90, 200, 255]),
            );
        }
        for (a, b) in [
            SERVER_JOIN_SPRITES,
            SERVER_MOVE_UP_SPRITES,
            SERVER_MOVE_DOWN_SPRITES,
        ] {
            for id in [a, b] {
                src.insert(
                    format!("assets/minecraft/textures/gui/sprites/{id}.png"),
                    solid_rgba_png(32, 32, [200, 40, 90, 255]),
                );
            }
        }
        // The favicon fallback is a **loose** texture, so it arrives through the
        // extras list rather than the sprite glob — the same path the logo takes.
        src.insert(
            crate::resources::UNKNOWN_SERVER_TEXTURE.1,
            solid_rgba_png(32, 32, [70, 70, 70, 255]),
        );
        lodestone_assets::ResourceManager::new(vec![Box::new(src) as Box<dyn ResourceSource>])
    }

    /// The atlas the two sprite gates below sample against.
    fn server_list_atlas() -> GuiAtlas {
        GuiAtlas::build_with_extras(
            &server_list_pack(),
            &[crate::resources::UNKNOWN_SERVER_TEXTURE],
        )
        .expect("synthetic atlas builds")
    }

    /// Whether any whole **quad** on the sprite stream samples inside `id`'s atlas
    /// region.
    ///
    /// `all_uvs_within`'s companion, and needed because the hover overlay blits
    /// **three** sprites into the same 32×32 rect: "every UV is inside join" is
    /// false by construction there, while "some quad is inside join_highlighted and
    /// none is inside join" is exactly the question.
    ///
    /// A *quad* rather than a vertex, and that is not fussiness: the packer may
    /// place two sprites edge to edge, and a vertex exactly on the shared edge is
    /// inside both regions to within any epsilon. A whole quad can only be inside
    /// one of two equal-sized regions.
    fn any_quad_within(sprite: &[f32], min: [f32; 2], max: [f32; 2]) -> bool {
        sprite
            .chunks_exact(SPRITE_FLOATS_PER_VERTEX * 6)
            .any(|quad| all_uvs_within(quad, min, max))
    }

    /// Every sprite-stream UV whose **destination** falls inside `rect`.
    ///
    /// The pair of questions together — where it landed and which region it
    /// sampled — is what makes a per-widget assertion possible on a stream that
    /// carries every sprite on the screen at once.
    fn uvs_in_dest(sprite: &[f32], w: f32, h: f32, rect: (f32, f32, f32, f32)) -> Vec<[f32; 2]> {
        let (rx, ry, rw, rh) = rect;
        sprite
            .chunks_exact(SPRITE_FLOATS_PER_VERTEX)
            .filter(|v| {
                let px = (v[0] + 1.0) * 0.5 * w;
                let py = (1.0 - v[1]) * 0.5 * h;
                px >= rx - 0.01 && px <= rx + rw + 0.01 && py >= ry - 0.01 && py <= ry + rh + 0.01
            })
            .map(|v| [v[2], v[3]])
            .collect()
    }

    /// The quadrant under the cursor decides which of the three overlay sprites is
    /// drawn **highlighted**, and the other two must stay plain. All three blit
    /// into the same rect, so this is asserted by atlas region rather than by
    /// position — position is what the previous gate covers.
    #[test]
    fn each_hovered_icon_quadrant_highlights_its_own_sprite() {
        let atlas = server_list_atlas();
        let (nav, ui) = list_nav(
            "quadrants",
            &[("A", "a.example"), ("B", "b.example"), ("C", "c.example")],
        );
        let statuses = StatusCache::with_probe(unavailable_probe());
        let mut fav = FaviconCache::new();
        let mut f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();

        let region = |id: &str| sprite_uv_bounds(&atlas, id);
        let regions = [
            SERVER_JOIN_SPRITES,
            SERVER_MOVE_UP_SPRITES,
            SERVER_MOVE_DOWN_SPRITES,
        ];
        // The six regions must be disjoint, or "sampled inside X" proves nothing.
        let all: Vec<([f32; 2], [f32; 2])> = regions
            .into_iter()
            .flat_map(|(a, b)| [region(a), region(b)])
            .collect();
        for i in 0..all.len() {
            for j in (i + 1)..all.len() {
                let (a, b) = (all[i], all[j]);
                assert!(
                    a.1[0] <= b.0[0] || b.1[0] <= a.0[0] || a.1[1] <= b.0[1] || b.1[1] <= a.0[1],
                    "two overlay sprites share atlas space: {a:?} {b:?}"
                );
            }
        }

        // Row 1 of three, so both move arrows apply.
        let (ix, iy, iw, ih) = server_entry_icon_rect(1, V_W, 0);
        let cases = [
            // (cursor, which of the three is highlighted)
            ((ix + iw * 0.75, iy + ih * 0.5), 0usize),
            ((ix + 4.0, iy + 4.0), 1),
            ((ix + 4.0, iy + ih - 4.0), 2),
        ];
        for ((mx, my), highlighted) in cases {
            f.cursor = Some((mx, my));
            let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;
            for (which, (plain, hot)) in regions.into_iter().enumerate() {
                let (p, hgt) = (region(plain), region(hot));
                if which == highlighted {
                    assert!(
                        any_quad_within(&sprite, hgt.0, hgt.1),
                        "cursor ({mx}, {my}) must highlight {hot}"
                    );
                    assert!(
                        !any_quad_within(&sprite, p.0, p.1),
                        "and must not also draw the plain {plain}"
                    );
                } else {
                    assert!(
                        any_quad_within(&sprite, p.0, p.1),
                        "cursor ({mx}, {my}) must still draw the plain {plain}"
                    );
                    assert!(
                        !any_quad_within(&sprite, hgt.0, hgt.1),
                        "and must not highlight {hot}"
                    );
                }
            }
        }

        // Row 0 has nowhere to move up to, so its arrow must not be drawn at all —
        // vanilla's `if (index > 0)` guard (`ServerSelectionList.java:375`).
        let (ix0, iy0, iw0, ih0) = server_entry_icon_rect(0, V_W, 0);
        f.cursor = Some((ix0 + 4.0, iy0 + 4.0));
        let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;
        let up = region(SERVER_MOVE_UP_SPRITES.0);
        let up_hot = region(SERVER_MOVE_UP_SPRITES.1);
        assert!(
            !any_quad_within(&sprite, up.0, up.1) && !any_quad_within(&sprite, up_hot.0, up_hot.1),
            "row 0 must draw no move-up arrow"
        );
        let down = region(SERVER_MOVE_DOWN_SPRITES.0);
        assert!(
            any_quad_within(&sprite, down.0, down.1),
            "control: its move-down arrow is there, so the detector works"
        );
        // And with no cursor, none of the six is drawn.
        f.cursor = None;
        let sprite = build(&f, Some(&atlas), None, V_W, V_H).sprite;
        for (plain, hot) in regions {
            let (p, hgt) = (region(plain), region(hot));
            assert!(!any_quad_within(&sprite, p.0, p.1), "{plain} without a cursor");
            assert!(!any_quad_within(&sprite, hgt.0, hgt.1), "{hot} without a cursor");
        }
    }

    /// The status sprite is asserted **by identity through the atlas**, at the rect
    /// vanilla puts it at: a gate that only proved a ping bar exists passes on all
    /// four states rendering the same bar.
    ///
    /// Also the footer's disabled path, per button, by the same joint test — where
    /// it landed *and* which region it sampled. The expected sprite comes from
    /// `WidgetSprites::get`, never spelled out.
    #[test]
    fn the_status_sprite_and_the_disabled_footer_sample_the_sprites_they_should() {
        use crate::menu::nav::{SERVER_LIST_BUTTONS, ServerListButton as B};
        use crate::menu::status::{PING_SPRITES, ServerStatus};

        let atlas = server_list_atlas();
        let (mut nav, mut ui) = list_nav("sprites", &[]);
        let statuses = StatusCache::with_probe(unavailable_probe());
        let mut fav = FaviconCache::new();

        // Empty list: Join / Edit / Delete / Direct all draw `button_disabled`,
        // each at its own rect, and the other three draw `button`.
        let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        let stream = build(&f, Some(&atlas), None, V_W, V_H).sprite;
        let check = |stream: &[f32], button: B, enabled: bool| {
            let want = widget::BUTTON_SPRITES.get(enabled, false);
            let (min, max) = sprite_uv_bounds(&atlas, want);
            let rect = server_list_footer_slot(button).resolve(V_W, V_H);
            let uvs = uvs_in_dest(stream, V_W, V_H, rect);
            assert!(!uvs.is_empty(), "{button:?} drew nothing at {rect:?}");
            assert!(
                uvs.iter().all(|uv| {
                    uv[0] >= min[0] - 1e-6
                        && uv[0] <= max[0] + 1e-6
                        && uv[1] >= min[1] - 1e-6
                        && uv[1] <= max[1] + 1e-6
                }),
                "{button:?} did not sample {want} (enabled={enabled})"
            );
        };
        for button in SERVER_LIST_BUTTONS {
            check(&stream, button, button.enabled(false));
        }

        // Control, executed: a saved server flips three of them, so the assertion
        // above measures the selection and not a screen that is always disabled.
        add_server(&mut nav, &mut ui, "HOME", "mc.example.com");
        let mut statuses = StatusCache::with_probe(std::sync::Arc::new(|_| {
            Ok(ServerStatus {
                motd: "hello".into(),
                players: "2/8".into(),
                protocol: Some(crate::menu::status::STATUS_PROTOCOL),
                latency_ms: Some(400),
                ..Default::default()
            })
        }));
        let entries = nav.list().entries().to_vec();
        statuses.refresh(&entries);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while statuses.pump() == 0 && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        let stream = build(&f, Some(&atlas), None, V_W, V_H).sprite;
        for button in SERVER_LIST_BUTTONS {
            check(&stream, button, button.enabled(true));
        }
        for b in [B::Select, B::Edit, B::Delete] {
            assert!(b.enabled(true) && !b.enabled(false), "control premise: {b:?}");
        }

        // 400 ms is the middle bucket. Asserted at the status icon's own rect, so
        // this is both "the right sprite" and "in the right place".
        let rect = server_status_icon_rect(0, V_W, 0);
        let uvs = uvs_in_dest(&stream, V_W, V_H, rect);
        assert!(!uvs.is_empty(), "no status sprite at {rect:?}");
        let (min, max) = sprite_uv_bounds(&atlas, PING_SPRITES[2]);
        assert!(
            uvs.iter().all(|uv| {
                uv[0] >= min[0] - 1e-6
                    && uv[0] <= max[0] + 1e-6
                    && uv[1] >= min[1] - 1e-6
                    && uv[1] <= max[1] + 1e-6
            }),
            "400 ms must sample {} — three bars",
            PING_SPRITES[2]
        );
        // Control: it is not sampling a *different* bucket's sprite, which is what
        // "some ping bar drew" would have accepted.
        let (fmin, fmax) = sprite_uv_bounds(&atlas, PING_SPRITES[4]);
        assert!(
            !uvs
                .iter()
                .all(|uv| uv[0] >= fmin[0] - 1e-6 && uv[0] <= fmax[0] + 1e-6
                    && uv[1] >= fmin[1] - 1e-6
                    && uv[1] <= fmax[1] + 1e-6),
            "the detector cannot tell ping_3 from ping_5"
        );
    }

    #[test]
    fn the_error_screen_carries_the_disconnect_reason() {
        // Since `error_frame`'s conversion onto the framework, the reason
        // lives in `notice` (a wrapped, bounded `MenuNotice`, like the
        // account screen's failure message) rather than `message` — a
        // `vanilla` frame suppresses `message` entirely (see `MenuNotice`'s
        // own doc on why an unwrapped line was the bug this pattern fixes).
        let nav = test_nav("err");
        let mut ui = UiState::new();
        ui.begin(SessionKind::Multiplayer);
        ui.session_failed("disconnected: Server closed");
        let statuses = StatusCache::with_probe(unavailable_probe());
        let mut fav = FaviconCache::new();
        let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        assert!(f.vanilla, "the disconnect screen is on the framework now");
        assert!(f.message.is_none(), "a vanilla frame draws no `message`");
        let notice = f.notice.expect("the reason must reach the screen");
        assert!(notice.text.contains("Server closed"), "{}", notice.text);
        assert_eq!(
            f.rows[0].label, "Back to Title Screen",
            "vanilla's gui.toTitle, since dismiss_error always returns to MainMenu"
        );
        assert!(f.rows[0].slot.is_some(), "the button is vanilla-placed now");
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
        use crate::menu::nav::{ADDRESS_FIELD, CANCEL_ROW, DONE_ROW, NAME_FIELD, RESOURCE_PACK_ROW};
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
        // Two text fields plus the framework conversion's three button rows.
        assert_eq!(f.rows.len(), 5);
        assert!(f.vanilla, "the framework conversion sets `vanilla`");
        assert!(f.rows[NAME_FIELD].field, "row 0 is a text field");
        assert!(f.rows[ADDRESS_FIELD].field, "row 1 is a text field");
        assert!(!f.rows[RESOURCE_PACK_ROW].field, "row 2 is a button, not text");
        assert_eq!(f.rows[NAME_FIELD].label, "abc");
        assert_eq!(f.selected, NAME_FIELD, "the name field has focus");
        // Vanilla disables Done rather than printing a message
        // (`ManageServerScreen.java:92-93`) — see `error_frame`'s sibling note
        // on why a `vanilla` frame's `message` is unused, and this screen's own
        // arm on why no extra label duplicates the disabled sprite.
        assert!(f.message.is_none(), "a vanilla frame draws no `message`");
        assert!(
            !f.rows[DONE_ROW].enabled,
            "an addressless form must not offer a working Done button"
        );
        assert!(f.rows[CANCEL_ROW].enabled, "Cancel always works");
        assert!(!f.rows[RESOURCE_PACK_ROW].enabled, "present, but inactive");
        for row in [NAME_FIELD, ADDRESS_FIELD, RESOURCE_PACK_ROW, DONE_ROW, CANCEL_ROW] {
            assert!(f.rows[row].slot.is_some(), "row {row} must be vanilla-placed");
        }

        nav.key(&mut ui, MenuKey::Tab);
        let f = frame_for(&ui, &nav, &statuses, &mut fav).unwrap();
        assert_eq!(f.selected, ADDRESS_FIELD, "Tab moves focus to the address");
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

    // -- the account screen (#66/#402) ----------------------------------------

    /// A nav whose `profiles.json` holds `names`, most-recently-used **first**
    /// (the order `AccountsNav::ordered` sorts into, so `names[0]` is row 0).
    /// Written beside a temp `servers.json`, which is where `MenuNav::with_path`
    /// looks for it.
    fn accounts_nav(tag: &str, names: &[&str]) -> MenuNav {
        let path = std::env::temp_dir().join(format!(
            "lodestone-render-accounts-{}-{tag}/servers.json",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let mut meta = lodestone_auth::metadata::AccountsMetadata::default();
        for (i, name) in names.iter().enumerate() {
            meta.upsert(lodestone_auth::metadata::AccountProfile {
                profile_id: uuid::Uuid::new_v4(),
                username: (*name).to_string(),
                skin_url: None,
                last_used: (names.len() - i) as u64,
            });
        }
        meta.save_to(&path.parent().unwrap().join("profiles.json"))
            .expect("the temp profiles file must be writable");
        MenuNav::with_path(path)
    }

    #[test]
    fn the_accounts_slots_do_not_depend_on_the_reference_canvas() {
        // The same argument the multiplayer screen's version of this makes: the
        // block is arranged **once** at `ACCOUNTS_REF_CANVAS`, which is sound only
        // if every rect it hands out is canvas-independent once expressed as a
        // `Slot`. Even widths only — `Origin::ScreenBottom`'s x is `width * 0.5`
        // unrounded while `FrameLayout` truncates, so an odd logical width differs
        // by half a pixel (the limit `Screen::WorldSelect`'s footer has too).
        for (w, h) in [(854.0, 480.0), (1280.0, 720.0), (640.0, 400.0)] {
            let live = AccountsBlock::at(w, h);
            assert_eq!(
                accounts_block().content_top,
                live.content_top,
                "the content band moved at {w}x{h}"
            );
            for i in 0..crate::menu::accounts::BUTTON_COUNT {
                let slot = accounts_button_slot(i);
                assert_eq!(
                    slot,
                    live.footer_slot(i),
                    "button {i}'s slot depends on the canvas"
                );
                // ...and it must resolve onto *that* canvas' own arrangement,
                // which is what makes the two derivations independent rather than
                // merely equal.
                let got = slot.resolve(w, h);
                let want = live.footer[i];
                assert!(
                    (got.0 - want.0).abs() < 0.01
                        && (got.1 - want.1).abs() < 0.01
                        && (got.2 - want.2).abs() < 0.01
                        && (got.3 - want.3).abs() < 0.01,
                    "button {i} resolves to {got:?} at {w}x{h}, arranged at {want:?}"
                );
            }
        }
        // The footer column measures `4 * 74 + 3 * 4`, which is the multiplayer
        // screen's lower row exactly — the agreement `ACCOUNTS_BUTTON_W`'s doc
        // claims, asserted rather than described.
        let first = accounts_button_slot(0).resolve(854.0, 480.0);
        let last = accounts_button_slot(crate::menu::accounts::BUTTON_COUNT - 1)
            .resolve(854.0, 480.0);
        let column = last.0 + last.2 - first.0;
        let want = 4.0 * ACCOUNTS_BUTTON_W + 3.0 * ACCOUNTS_FOOTER_SPACING as f32;
        assert!(
            (column - want).abs() < 0.01,
            "the footer column is {column}, not {want}"
        );
    }

    #[test]
    fn the_account_rows_are_in_the_order_click_assumes() {
        // `AccountsNav::hover` maps a **rendered** row index back through the
        // scroll window and then onto the four button slots, so this order is a
        // coupling between two files — the same guard shape the settings and
        // multiplayer screens carry against the same #391 bug.
        use crate::menu::accounts::{
            BUTTON_ADD, BUTTON_CANCEL, BUTTON_COUNT, BUTTON_REMOVE, BUTTON_SELECT,
        };
        let nav = accounts_nav("order", &["Alex", "Steve"]);
        let f = accounts_idle_frame(nav.accounts());

        assert_eq!(
            f.rows.len(),
            3 + BUTTON_COUNT,
            "two accounts + the offline entry + four buttons"
        );
        for (i, row) in f.rows.iter().take(3).enumerate() {
            let view = row
                .account
                .as_ref()
                .unwrap_or_else(|| panic!("row {i} is not a list row"));
            assert_eq!(view.index, i, "row {i} carries the wrong rendered index");
        }
        for (button, label) in [
            (BUTTON_ADD, "Add Account"),
            (BUTTON_SELECT, "Select"),
            (BUTTON_REMOVE, "Remove"),
            (BUTTON_CANCEL, "Back"),
        ] {
            let row = &f.rows[3 + button];
            assert_eq!(row.label, label, "button {button} is labelled wrong");
            assert_eq!(
                row.slot,
                Some(accounts_button_slot(button)),
                "{label} is not in its own footer slot"
            );
            assert!(row.account.is_none(), "{label} must not be a list row");
        }

        // The two cursors are separate: the keyboard starts on row 0, which is the
        // *list* cursor, and no footer button may be lit while it is there.
        assert!(f.rows[0].account.as_ref().unwrap().selected);
        assert_eq!(
            f.selected,
            usize::MAX,
            "a button is highlighted while focus is on a row"
        );
    }

    #[test]
    fn an_account_row_draws_inside_its_own_36px_row_and_not_the_one_below() {
        let nav = accounts_nav("rowpixels", &["Alex"]);
        let f = accounts_idle_frame(nav.accounts());
        let (w, h) = (854.0, 480.0);
        let v = geometry(&f, w, h);

        // Row 0 is Alex, row 1 the offline entry, row 2 is past the end.
        for i in 0..2 {
            let rect = accounts_row_rect(i, w);
            assert!(
                coverage(&v, w, h, rect) > 0.05,
                "row {i} drew nothing in {rect:?}: {}",
                coverage(&v, w, h, rect)
            );
        }
        let empty = accounts_row_rect(2, w);
        assert_eq!(
            coverage(&v, w, h, empty),
            0.0,
            "something drew in the row past the end, at {empty:?}"
        );

        // The 32 px head fills the content box's full height, which is the whole
        // point of a 36 px pitch with 2 px of padding.
        let (cx, cy, _, _) = accounts_row_content_rect(0, w);
        let head = (cx, cy, ACCOUNTS_HEAD_ICON, ACCOUNTS_HEAD_ICON);
        assert!(
            coverage(&v, w, h, head) > 0.95,
            "the head icon does not fill {head:?}: {}",
            coverage(&v, w, h, head)
        );
    }

    /// **The reported bug.** The sign-in failure reason was drawn as one
    /// unwrapped centred line at [`TEXT_SCALE`], so a message assembled from a
    /// server's own response body was both too large to read and wider than the
    /// screen.
    ///
    /// Measured by location, against the rect the *draw* derives — `notice_rect`
    /// is called here rather than restated, because `CLAUDE.md` records two gates
    /// whose restated rect was itself the thing that was wrong — and the failure
    /// output is a bounding box, not a fraction. The control is **executed**: the
    /// same detector, on the same frame, with a deliberately unbounded wrap
    /// column, must report a box outside the rect. Without it, "nothing
    /// overflowed" would pass just as well on a frame where nothing drew at all.
    #[test]
    fn a_long_sign_in_failure_is_wrapped_and_bounded_to_the_notice_rect() {
        // `lodestone-auth`'s `step_result` formats `"{status}: {snippet}"` with up
        // to 400 characters of whatever the server actually returned, and a JSON
        // body has **no whitespace in it** — so a wrap that only breaks on spaces
        // emits one enormous line, and this passes only because `wrap_bounded`
        // breaks mid-word.
        let body = format!(
            "401:{{\"XErr\":2148916238,\"Message\":\"{}\"}}",
            "x".repeat(360)
        );
        assert!(
            !body.contains(' '),
            "premise: the message has no whitespace to wrap on"
        );

        let (w, h) = (854.0, 480.0);
        let frame = accounts_failed_frame(&body);
        let notice = frame
            .notice
            .clone()
            .expect("the failure state must carry a notice");
        let (nx, ny, nw, nh) = notice_rect(&notice, w, h);
        let v = geometry(&frame, w, h);
        let got = colour_bounds(&v, w, h, notice.colour)
            .expect("the failure message reached no pixels at all");
        assert!(
            got.0 >= nx - 0.5
                && got.0 + got.2 <= nx + nw + 0.5
                && got.1 >= ny - 0.5
                && got.1 + got.3 <= ny + nh + 0.5,
            "the failure text drew at {got:?}, outside its notice rect {:?}",
            (nx, ny, nw, nh)
        );
        // Wrapped, not merely cut: one line's box is a single glyph tall.
        assert!(
            got.3 > LINE_H,
            "the message was cut to one line instead of wrapped: box {got:?}"
        );

        // The control. Same text, same detector, a column twice the canvas wide.
        let mut unbounded = accounts_failed_frame(&body);
        unbounded
            .notice
            .as_mut()
            .expect("the control still has a notice")
            .w = w * 2.0;
        let cv = geometry(&unbounded, w, h);
        let control = colour_bounds(&cv, w, h, notice.colour)
            .expect("the control drew nothing, so it proves nothing");
        assert!(
            control.0 + control.2 > nx + nw,
            "the detector cannot see an overflow: control box {control:?} against rect {:?}",
            (nx, ny, nw, nh)
        );
    }

    #[test]
    fn wrap_bounded_breaks_a_run_that_no_whitespace_wrap_could() {
        // The difference from `wrap_measured` in one test, with that function as
        // the control: what makes a second wrap necessary rather than a flag on
        // the first is that the multiplayer screen's greedy fallback ("a word that
        // does not fit starts a line") does nothing at all for a 400-character
        // token.
        let b = Quads::new(854.0, 480.0);
        let run = "x".repeat(400);
        let column = 120.0;

        let hard = wrap_bounded(&b, &run, column, 8);
        assert!(hard.len() > 1, "the run was not broken at all: {hard:?}");
        for (i, line) in hard.iter().enumerate() {
            let lw = b.text_width(line, 1.0);
            assert!(lw <= column, "line {i} measures {lw} in a {column} column");
        }

        let soft = wrap_measured(&b, &run, column, 8);
        assert_eq!(
            soft.len(),
            1,
            "wrap_measured's documented behaviour changed: {soft:?}"
        );
        assert!(
            b.text_width(&soft[0], 1.0) > column,
            "the control did not overflow, so it proves nothing"
        );

        // And it terminates on a column too narrow for a single glyph, rather
        // than pushing empty lines forever.
        let starved = wrap_bounded(&b, &run, 1.0, 4);
        assert_eq!(starved.len(), 4);
        assert!(starved.iter().all(|l| l.chars().count() == 1));
    }

    #[test]
    fn a_short_canvas_truncates_the_account_window_instead_of_drawing_over_the_footer() {
        // #402's residual gap, bounded rather than closed: `VISIBLE_ROWS` is a
        // count and this module has no canvas, so the *draw* is what refuses a row
        // that would not fit whole. The footer band is where all four actions are,
        // so a half-drawn row there is worse than a missing one.
        //
        // Checked against the **arranged button row's own y** rather than against
        // `accounts_row_visible`'s own formula, which would only restate it: two
        // independent derivations of one fact is the only shape that catches a
        // guard that is self-consistently wrong.
        use crate::menu::accounts::VISIBLE_ROWS;
        let (w, short) = (854.0, 240.0);
        let fitting = (0..VISIBLE_ROWS)
            .filter(|&i| accounts_row_visible(i, short))
            .count();
        assert!(
            fitting < VISIBLE_ROWS,
            "premise: {short} px must be too short for all {VISIBLE_ROWS} rows of the window"
        );
        assert!(fitting > 0, "premise: some rows must still fit at {short} px");

        let (_, button_y, _, _) = accounts_button_slot(0).resolve(w, short);
        for i in 0..VISIBLE_ROWS {
            if !accounts_row_visible(i, short) {
                continue;
            }
            let (_, y, _, rh) = accounts_row_rect(i, w);
            assert!(
                y + rh <= button_y,
                "row {i} is kept but reaches {}, past the button row at {button_y}",
                y + rh
            );
        }
        // The control: at a full-size canvas every row of the window fits, so the
        // premise above is measuring the canvas rather than a guard that is
        // unconditionally false.
        assert!(
            (0..VISIBLE_ROWS).all(|i| accounts_row_visible(i, 480.0)),
            "no row of the window fits even at 480 px"
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
        //
        // `.floor()`ed (issue #401): `854.0 / 4.0` is `213.5`, not a whole
        // pixel, where vanilla's `this.width / 2 / 2` is two Java integer
        // divisions and can only ever land on a whole pixel.
        assert_eq!(Origin::DeathTitle.anchor(V_W, V_H), ((V_W / 4.0).floor(), 0.0));
        assert_ne!(
            Origin::DeathTitle.anchor(V_W, V_H).0,
            Origin::ScreenTop.anchor(V_W, V_H).0,
            "the death title and the score/message lines are not on the same x"
        );
    }

    /// Issue #401: every width-derived [`Origin`] anchor is vanilla's `this.width`
    /// (always `int`) divided by a constant — Java integer division — so the x
    /// term must be `floor`ed. At an *even* width that is invisible, because
    /// `width * 0.5` (or `* 0.25`) is already a whole pixel; **no test before
    /// this one used an odd width**, which is exactly how the bug shipped. 855
    /// is odd and not a multiple of 4 either, so it exercises every one of the
    /// affected arms at once.
    ///
    /// Each assertion predicts *both* hypotheses from `width` alone — floored
    /// (right) and unfloored (the bug) — and requires landing on the floored
    /// one, per CLAUDE.md's magnitude-species rule: asserting only "the anchor
    /// moved" or "is not X.5" would pass for nearly any wrong number too.
    #[test]
    fn odd_width_anchors_are_floored_like_javas_integer_division() {
        let width = 855.0_f32;
        let height = 481.0_f32;

        let floored_half = (width * 0.5).floor();
        let unfloored_half = width * 0.5;
        assert_eq!(floored_half, 427.0, "sanity: floor(855/2) is 427, not 427.5");
        assert_ne!(floored_half, unfloored_half, "sanity: 855 is odd, so the two must differ");

        assert_eq!(
            Origin::ScreenTop.anchor(width, height),
            (floored_half, 0.0),
            "ScreenTop must not land on the unfloored {unfloored_half}"
        );
        assert_eq!(
            Origin::TitleTop.anchor(width, height),
            (floored_half, (height / 4.0).floor() + 48.0),
            "TitleTop's x must not land on the unfloored {unfloored_half}"
        );
        assert_eq!(
            Origin::ScreenBottom.anchor(width, height),
            (floored_half, height),
            "ScreenBottom must not land on the unfloored {unfloored_half}"
        );

        let floored_quarter = (width * 0.25).floor();
        let unfloored_quarter = width * 0.25;
        assert_eq!(floored_quarter, 213.0, "sanity: floor(855/4) is 213, not 213.75");
        assert_ne!(floored_quarter, unfloored_quarter, "sanity: 855/4 is not a whole pixel");
        assert_eq!(
            Origin::DeathTitle.anchor(width, height),
            (floored_quarter, 0.0),
            "DeathTitle must not land on the unfloored {unfloored_quarter}"
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
        // Four disabled, two enabled — #397's headline with #287's launch on top.
        // Create New World is *present* and inactive, which is what makes the
        // footer's shape vanilla's; Play is active because the list has a world.
        let enabled: Vec<&str> = f.rows[1..]
            .iter()
            .filter(|r| r.enabled)
            .map(|r| r.label.as_str())
            .collect();
        assert_eq!(enabled, vec!["Play Selected World", "Back"]);
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

        // The two free-standing strings: the title, and the one list row.
        let texts: Vec<&str> = f.labels.iter().map(|l| l.text.as_str()).collect();
        assert_eq!(
            texts,
            vec![
                crate::menu::world_select::WORLD_SELECT_TITLE,
                crate::menu::world_select::BUNDLED_WORLD.label,
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

    /// The list draws its one row, inside row 0's own content rect.
    ///
    /// This is the assertion that keeps "the list has a world" distinguishable
    /// from "the list failed to draw" — without it the two are the same picture,
    /// which is exactly the absence-needs-a-control rule. It is also the pixel
    /// half of #287's world list: the button that launches is only honest if the
    /// world it launches is on screen. The band is the row's content rect from
    /// `world_list_row_content_rect`, the same expression the label's position is
    /// derived from, and the failure output is a bounding box rather than a
    /// fraction.
    ///
    /// Two controls, both executed: the band *below* the row must be empty (so
    /// this is not measuring a frame that paints everywhere), and the same band
    /// on the **title screen** must be empty too (so it is not measuring
    /// something every menu draws there).
    #[test]
    fn the_world_list_draws_its_one_row_inside_row_zeros_content_rect() {
        let (nav, ui) = world_select_nav("ws-row");
        let frame = world_select_frame(&nav, &ui);
        let colour = geometry(&frame, V_W, V_H);

        let band = world_list_row_content_rect(0, V_W);
        let inside = band_coverage(&colour, V_W, V_H, band);
        assert!(
            inside.count > 0,
            "the world-list row reached no pixels inside {band:?}"
        );
        let bounds = inside.bounds.expect("a non-empty band has bounds");
        // It is a line of text, not a full-height fill: the row label is 9 px of
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
            "the row label is not centred: bounds {bounds:?}"
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

    /// The list row's label fits the row it is centred in.
    ///
    /// Vanilla's `NoWorldsEntry` gives its `StringWidget` no `maxWidth`
    /// (`WorldSelectionList.java:382-384`), so nothing clips it and a longer
    /// string would overhang the row. Measured with [`text_px`], the same
    /// fixed-advance measure the jar-less draw uses — the real vanilla font is
    /// narrower, so this is the conservative direction.
    #[test]
    fn the_world_list_row_label_fits_the_row_it_is_centred_in() {
        let (.., content_w, _) = world_list_row_content_rect(0, V_W);
        let measured = text_px(crate::menu::world_select::BUNDLED_WORLD.label, 1.0);
        assert!(
            measured <= content_w,
            "the world-list row label measures {measured} px in a {content_w} px row"
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
        // Upper-case, and `M` first, on purpose: the jar-less font's `M` is
        // `0b10001` in all seven rows (`hud/font.rs:97`), so its leftmost lit
        // column sits exactly on the box's `text_x`. That is what lets the x
        // assertion below be an equality rather than a bound — a glyph whose
        // column 0 is blank (`A`, `C`) would put the leftmost vertex a pixel or
        // two right of `text_x` and make the same test unable to tell a 2 px
        // error from a correct draw.
        for ch in "MC".chars() {
            nav.key(&mut ui, MenuKey::Char(ch));
        }
        let frame = world_select_frame(&nav, &ui);
        let row = frame.rows[0].clone();
        assert_eq!(
            row.edit.as_ref().map(|e| e.value().to_string()),
            Some("MC".to_string()),
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
        // The band spans the box's **whole** width, deliberately: the question is
        // where the text starts, so a band that begins at `text_x` would clip the
        // very error it is looking for and pass on a draw 4 px to the left.
        //
        // That makes the *focus outline* the thing to be careful about, and it is
        // what this gate got wrong on its first run. `band_coverage` counts
        // **vertices**, not covered area, and the jar-less outline's bottom bar
        // spans the full field width at `y + h - 2` — inside a `glyph_h`-tall
        // band vertically, with its only vertices at the box's own `x` and
        // `x + width`. So on a focused box the leftmost vertex in this band is the
        // box's edge, not the text's, and the gate accused the draw of painting
        // 4 px left of `text_x` when the draw was right and the 4 px was
        // `BORDER_INSET` in the gate's own reasoning. (#395's `EditBox` gate dodges
        // this by insetting its band to `text_x`/`inner_width`; that is the right
        // answer for measuring *what* drew and the wrong one for measuring
        // *where* it started.)
        //
        // So: measure the text on an **unfocused** clone — no outline, no caret,
        // nothing in the box but glyphs — and use the focused draw as the control
        // that this band really can see ink at the box's edge.
        let band = (fx, state.text_y, fw, GLYPH_H as f32 * TEXT_SCALE);
        let mut unfocused = row.clone();
        if let Some(e) = unfocused.edit.as_mut() {
            e.widget.focused = false;
        }
        let mut u = frame_with(vec![unfocused], 99);
        u.vanilla = true;
        let quiet = build(&u, Some(&atlas), None, V_W, V_H).colour;
        let inside = band_coverage(&quiet, V_W, V_H, band);
        assert!(
            inside.count > 0,
            "the typed text reached no pixels inside the box's own band {band:?}"
        );
        let bounds = inside.bounds.expect("a non-empty band has bounds");
        assert!(
            (bounds.0 - state.before_x).abs() < 0.01,
            "the text starts at {} where the box's own text_x is {} — a draw using \
             the row's PAD of 6, or the box's own x, fails here; bounds {bounds:?}",
            bounds.0,
            state.before_x
        );
        assert!(
            bounds.2 <= fx + fw + 0.01,
            "the text overran the box's right edge: bounds {bounds:?}"
        );

        // -- control ---------------------------------------------------------
        // The focused draw puts the outline's bottom bar in the same band, with a
        // corner vertex on the box's own `x`. So the band demonstrably *can* see
        // ink `BORDER_INSET` left of `text_x` — which is exactly the error the
        // assertion above denies, and without this the equality could be passing
        // because the band is blind to that column.
        let lit = band_coverage(&drawn.colour, V_W, V_H, band)
            .bounds
            .expect("a focused field paints its outline");
        assert!(
            (lit.0 - fx).abs() < 0.01,
            "the control did not reach the box's edge, so the assertion above is \
             not measuring what it claims: bounds {lit:?}"
        );
        assert!(
            state.before_x - fx > 0.0,
            "premise: text_x is inset from the box's x, or the two measurements \
             above cannot disagree"
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
