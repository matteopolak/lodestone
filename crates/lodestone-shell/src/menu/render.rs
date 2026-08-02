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
use crate::menu::nav::{MainButton, PauseButton};

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
const WIDGET_H: f32 = 20.0;
/// A vanilla wide button — `Button.BIG_WIDTH` (`Button.java:14`), used for the
/// title screen's top three rows (`TitleScreen.java:178,196,199`).
const WIDE_W: f32 = 200.0;
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
/// 204 px `BUTTON_WIDTH_FULL` (`PauseScreen.java:53`) plus the default cell's
/// 4 px left and right padding (`PauseScreen.java:93`), split across two
/// columns of 106 — so the grid is 212 wide and a *half*-width 98 px button
/// sits 4 px into its 106 px column. That is where the pause screen's 8 px
/// gutter comes from, and why its full-width buttons start at `W/2 - 102`
/// rather than the title screen's `W/2 - 100`.
pub const PAUSE_GRID_W: f32 = 212.0;
/// Height of the same grid: row 0 is `20 + paddingTop(50)` = 70
/// (`PauseScreen.java:98`) and rows 1..4 are `20 + 4` = 24 each, for
/// `70 + 4 * 24`.
pub const PAUSE_GRID_H: f32 = 166.0;
/// Vanilla's font line height, used to centre a label in its widget
/// (`ActiveTextCollector.java:73`).
const LINE_H: f32 = 9.0;
/// Vertical offset of the pause screen's title `StringWidget`
/// (`PauseScreen.java:88`).
const PAUSE_TITLE_Y: f32 = 40.0;
/// Baseline of the title screen's two corner strings — vanilla draws both at
/// `height - 10` (`TitleScreen.java:154,323`).
const CORNER_TEXT_Y: f32 = -10.0;

/// The three `widget/button*` sprites `AbstractButton.SPRITES` selects between
/// (`AbstractButton.java:18-22`). All three are `nine_slice` in the pack; their
/// border widths are read from the sibling `.png.mcmeta` by
/// [`GuiAtlas`](lodestone_render::GuiAtlas), **not** hardcoded here — which
/// matters, because `button_disabled`'s border is **1** while the other two are
/// **3**.
const SPRITE_BUTTON: &str = "widget/button";
/// See [`SPRITE_BUTTON`]. Selected when enabled *and* hovered/focused.
const SPRITE_BUTTON_HOVER: &str = "widget/button_highlighted";
/// See [`SPRITE_BUTTON`]. Selected whenever the widget is inactive, hovered or
/// not — `WidgetSprites::get` returns `disabledFocused == disabled` for the
/// three-argument constructor (`WidgetSprites.java:15-25`).
const SPRITE_BUTTON_OFF: &str = "widget/button_disabled";

/// An active button's label colour: plain white, `ARGB.white(alpha)`
/// (`AbstractButton.java:51` tints the sprite; the label itself is the
/// component's own default).
const LABEL: [f32; 4] = [1.0, 1.0, 1.0, 1.0];
/// An inactive button's label colour:
/// `AbstractWidget.WithInactiveMessage.defaultInactiveMessage` merges
/// `Style.withColor(-6250336)` (`AbstractWidget.java:318`), and
/// `-6250336 as u32 == 0xFF_A0_A0_A0` — grey 160.
const LABEL_OFF: [f32; 4] = [160.0 / 255.0, 160.0 / 255.0, 160.0 / 255.0, 1.0];
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
    /// (`PauseScreen.java:181`), whose `alignInDimension` is
    /// `(int) Mth.lerp(align, 0, length - widgetLength)`
    /// (`FrameLayout.java:113-116`) — a truncating cast, hence the `floor`s —
    /// with the grid's own size being [`PAUSE_GRID_W`]×[`PAUSE_GRID_H`].
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
            Origin::PauseGrid => (
                ((width - PAUSE_GRID_W) * 0.5).floor(),
                ((height - PAUSE_GRID_H) * 0.25).floor(),
            ),
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

/// Vanilla's rect for one title-screen widget, from
/// `TitleScreen.init`/`createNormalMenuOptions`
/// (`TitleScreen.java:105-205`).
///
/// The three icon buttons use `getHorizontalPosition(n, 3, 20)`
/// (`TitleScreen.java:170-173`): `totalWidth = 3 * 20 + 2 * 4 = 68`, so
/// `x = W/2 - 34 + (n - 1) * 24` for `n` in `1..=3`.
#[must_use]
pub fn title_slot(button: MainButton) -> Slot {
    let full = |dy: f32| Slot {
        origin: Origin::TitleTop,
        dx: -100.0,
        dy,
        w: WIDE_W,
        h: WIDGET_H,
    };
    let icon = |dx: f32| Slot {
        origin: Origin::TitleTop,
        dx,
        dy: TITLE_PITCH * 3.0,
        w: ICON_BTN,
        h: ICON_BTN,
    };
    let half = |dx: f32| Slot {
        origin: Origin::TitleTop,
        dx,
        dy: TITLE_PITCH * 4.0,
        w: TITLE_HALF_W,
        h: WIDGET_H,
    };
    match button {
        MainButton::Singleplayer => full(0.0),
        MainButton::Multiplayer => full(TITLE_PITCH),
        MainButton::Realms => full(TITLE_PITCH * 2.0),
        MainButton::Friends => icon(-34.0),
        MainButton::Language => icon(-10.0),
        MainButton::Accessibility => icon(14.0),
        MainButton::Options => half(-100.0),
        MainButton::Quit => half(2.0),
        // Not vanilla — see `MainButton::Accounts`'s docs and
        // `Origin::TopRight`'s. A corner widget, not one more stack row:
        // vanilla's own eight already reach to within 16 px of the bottom of
        // a 320×240 canvas, so nothing fits below them there. The gap above
        // the logo (`y < LOGO_Y`) is free at every canvas size instead.
        MainButton::Accounts => Slot {
            origin: Origin::TopRight,
            dx: -(ACCOUNTS_ENTRY_W + ACCOUNTS_ENTRY_MARGIN),
            dy: ACCOUNTS_ENTRY_MARGIN,
            w: ACCOUNTS_ENTRY_W,
            h: WIDGET_H,
        },
    }
}

/// Width of the non-vanilla `Accounts` corner button — see
/// [`Origin::TopRight`]'s docs for why it lives there rather than in
/// vanilla's own vertical stack.
const ACCOUNTS_ENTRY_W: f32 = 90.0;
/// Distance from the top-right corner to the `Accounts` button, both axes.
const ACCOUNTS_ENTRY_MARGIN: f32 = 4.0;

/// Vanilla's rect for one pause-screen widget, from
/// `PauseScreen.createPauseMenu` (`PauseScreen.java:91-183`), resolved by hand
/// through `GridLayout.arrangeElements` (`GridLayout.java:25-89`) and
/// `AbstractLayout.AbstractChildWrapper::setX`/`setY`
/// (`AbstractLayout.java:73-85`).
///
/// The derivation, since none of it is a round number by accident:
/// column widths are `[106, 106]` (the 204+8 full-width cell split over two
/// columns by `Divisor`); row heights are `[70, 24, 24, 24, 24]`, so row y
/// offsets are `[0, 70, 94, 118, 142]`. Each child's own offset inside its cell
/// is its `paddingLeft`/`paddingTop` because the default `xAlignment` is 0 — and
/// with `padding(4, 4, 4, 0)` a full-width button's `mostOffset` is also 4, so
/// alignment could not move it anyway. The icon row is the one centred cell
/// (`alignHorizontallyCenter`, `PauseScreen.java:154`):
/// `lerp(0.5, 4, 212 - 92 - 4) = 60`, and its own `LinearLayout` spaces four
/// 20 px children 4 px apart from there — 60, 84, 108, 132.
#[must_use]
pub fn pause_slot(button: PauseButton) -> Slot {
    let cell = |dx: f32, dy: f32, w: f32, h: f32| Slot {
        origin: Origin::PauseGrid,
        dx,
        dy,
        w,
        h,
    };
    match button {
        PauseButton::BackToGame => cell(4.0, 50.0, 204.0, WIDGET_H),
        PauseButton::Advancements => cell(4.0, 74.0, 98.0, WIDGET_H),
        PauseButton::Statistics => cell(110.0, 74.0, 98.0, WIDGET_H),
        PauseButton::ReportBugs => cell(60.0, 98.0, ICON_BTN, ICON_BTN),
        PauseButton::Feedback => cell(84.0, 98.0, ICON_BTN, ICON_BTN),
        PauseButton::Friends => cell(108.0, 98.0, ICON_BTN, ICON_BTN),
        PauseButton::PlayerReporting => cell(132.0, 98.0, ICON_BTN, ICON_BTN),
        // The full-width Options row is the `else` of vanilla's
        // `hasSingleplayerServer()` fork (`PauseScreen.java:157-163`); this
        // client has no integrated server, so that branch is the right one.
        PauseButton::Options => cell(4.0, 122.0, 204.0, WIDGET_H),
        PauseButton::QuitToTitle => cell(4.0, 146.0, 204.0, WIDGET_H),
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
    /// Draw the row as a text-entry field with a caret after `label`.
    pub field: bool,
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
    pub selected: usize,
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
                rows: vec![
                    MenuRow {
                        label: form.name.clone(),
                        detail: "NAME".to_string(),
                        enabled: true,
                        field: true,
                        ..Default::default()
                    },
                    MenuRow {
                        label: form.address.clone(),
                        detail: "ADDRESS - HOST OR HOST:PORT".to_string(),
                        enabled: true,
                        field: true,
                        ..Default::default()
                    },
                ],
                selected: match form.field {
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
            draw_widget(&mut b, &frame.rows, i, width, height, i == frame.selected);
            continue;
        }
        let Some((x, y, w, h)) = row_rect(&frame.rows, i, width, height) else {
            continue;
        };
        let selected = i == frame.selected;
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
) {
    let Some(row) = rows.get(i) else { return };
    let Some((x, y, w, h)) = row_rect(rows, i, width, height) else {
        return;
    };
    // `WidgetSprites::get(enabled, focused)` (`WidgetSprites.java:19-25`) with
    // `AbstractButton`'s three-argument sprite set: disabled wins over hovered,
    // which is why a greyed-out button under the cursor still looks greyed out.
    let sprite = if !row.enabled {
        SPRITE_BUTTON_OFF
    } else if selected {
        SPRITE_BUTTON_HOVER
    } else {
        SPRITE_BUTTON
    };
    if b.has_sprite(sprite) {
        b.sprite(sprite, x, y, w, h, LABEL);
    } else {
        // Jar-less fallback: the flat fills the menu has always used, so the
        // layout is still legible and still testable without a pack.
        let fill = if !row.enabled {
            ROW_OFF
        } else if selected {
            ROW_SEL
        } else {
            ROW_BG
        };
        b.rect(x, y, w, h, fill);
        if selected {
            b.outline(x, y, w, h, 1.0, FG);
        }
    }

    if let Some(icon) = row.icon {
        // `spriteOffset` is zero at every call site, so this is a plain centre.
        let ix = x + (w - ICON_SPRITE) * 0.5;
        let iy = y + (h - ICON_SPRITE) * 0.5;
        b.sprite(icon, ix.floor(), iy.floor(), ICON_SPRITE, ICON_SPRITE, ICON_TINT);
        return;
    }

    let colour = if row.enabled { LABEL } else { LABEL_OFF };
    // `extractScrollingStringOverContents(output, message, 2)` →
    // `acceptScrollingWithDefaultCenter(msg, x+2, x+w-2, y, y+h)`
    // (`AbstractButton.java:39-41`, `AbstractWidget.java:92-98`), whose centre
    // is `(left + right) / 2` and whose top is
    // `(top + bottom - lineHeight) / 2 + 1` (`ActiveTextCollector.java:59,73`).
    let (left, right) = (x + 2.0, x + w - 2.0);
    let tw = b.text_width(&row.label, 1.0);
    let label = if tw > right - left {
        // Vanilla scrolls an over-long label; we clip, which is the same static
        // frame a scroll happens to be showing at t=0.
        clip_measured(b, &row.label, right - left)
    } else {
        row.label.as_str()
    };
    let tw = b.text_width(label, 1.0);
    let tx = ((left + right) * 0.5 - tw * 0.5).floor();
    let ty = ((y + y + h - LINE_H) / 2.0).floor() + 1.0;
    b.text(label, tx, ty, 1.0, colour);
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
            coverage_of(&off, w, h, band, LABEL_OFF) > 0.02,
            "no grey label ink in a disabled button's rect: {}",
            coverage_of(&off, w, h, band, LABEL_OFF)
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
            coverage_of(&on, w, h, band, LABEL_OFF),
            0.0,
            "an enabled label must not be drawn grey"
        );
        assert_eq!(LABEL_OFF[0], 160.0 / 255.0, "vanilla's -6250336 is 0xFFA0A0A0");
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
        for id in [SPRITE_BUTTON, SPRITE_BUTTON_HOVER, SPRITE_BUTTON_OFF] {
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
        assert_eq!(corner(SPRITE_BUTTON), (3.0, 3.0));
        assert_eq!(corner(SPRITE_BUTTON_HOVER), (3.0, 3.0));
        assert_eq!(corner(SPRITE_BUTTON_OFF), (1.0, 1.0));

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
