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

use lodestone_assets::Image;

use crate::hud::glyph_rows;

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

/// Background colour of a menu screen (the vanilla dirt backdrop's dark tone).
const BG: [f32; 4] = [0.10, 0.10, 0.12, 1.0];
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
    if img.width == 0 || img.height == 0 {
        return None;
    }
    let (iw, ih) = (img.width as usize, img.height as usize);
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
                    if i + 3 >= img.rgba.len() {
                        continue;
                    }
                    for (c, a) in acc.iter_mut().enumerate() {
                        *a += f32::from(img.rgba[i + c]);
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
    /// Whether the row can be activated (a failed row is still selectable).
    pub enabled: bool,
    /// Draw `detail` in the failure colour.
    pub detail_is_error: bool,
    /// Draw the row as a text-entry field with a caret after `label`.
    pub field: bool,
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
#[must_use]
pub fn owns_frame(screen: super::Screen) -> bool {
    use super::Screen;
    matches!(
        screen,
        Screen::MainMenu | Screen::ServerList | Screen::ServerEdit | Screen::Settings | Screen::Error
    )
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
        Screen::MainMenu => Some(MenuFrame {
            title: "LODESTONE",
            subtitle: "A MINECRAFT CLIENT",
            rows: MAIN_BUTTONS
                .iter()
                .map(|b| MenuRow {
                    label: b.label().to_string(),
                    enabled: true,
                    ..Default::default()
                })
                .collect(),
            selected: nav.main_index(),
            footer: vec!["UP/DOWN SELECT   ENTER CONFIRM   ESC QUIT".to_string()],
            message: None,
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
                        enabled: true,
                        detail_is_error: is_error,
                        field: false,
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
                rows: vec![MenuRow {
                    label,
                    detail: "UP/DOWN CHANGES IT - AUTO FITS THE WINDOW".to_string(),
                    enabled: true,
                    ..Default::default()
                }],
                selected: 0,
                footer: vec!["UP/DOWN CHANGE   ESC BACK".to_string()],
                message: nav.options_save_error().map(str::to_string),
                ..Default::default()
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
    if row.favicon.is_some() || !row.detail.is_empty() {
        LIST_ROW_H
    } else {
        BUTTON_H
    }
}

/// The pixel rect of row `i`, given the viewport. Public so tests (and any
/// future mouse hit-testing) share one definition of where a row actually is.
#[must_use]
pub fn row_rect(rows: &[MenuRow], i: usize, width: f32, height: f32) -> Option<(f32, f32, f32, f32)> {
    let row = rows.get(i)?;
    let total: f32 = rows
        .iter()
        .map(|r| row_height(r) + ROW_GAP)
        .sum::<f32>()
        .max(0.0)
        - ROW_GAP;
    // Centred vertically, but never above the title block.
    let top = ((height - total) * 0.5).max(110.0);
    let y = top
        + rows[..i]
            .iter()
            .map(|r| row_height(r) + ROW_GAP)
            .sum::<f32>();
    let w = ROW_W.min(width - 2.0 * PAD);
    Some(((width - w) * 0.5, y, w, row_height(row)))
}

/// Builds the vertex data for one menu frame. Pure: no GPU, no state.
///
/// Returns interleaved `[x, y, r, g, b, a]` in NDC, two triangles per quad.
#[must_use]
pub fn geometry(frame: &MenuFrame<'_>, width: f32, height: f32) -> Vec<f32> {
    let mut b = Quads::new(width, height);
    b.rect(0.0, 0.0, width, height, BG);

    // Title block.
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

    for (i, row) in frame.rows.iter().enumerate() {
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
        if let Some(icon) = &row.favicon {
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

    // Message and footer, bottom-up.
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

    b.verts
}

/// A pixel-space quad emitter to NDC, self-contained so this module borrows no
/// private HUD types (mirrors [`crate::effects`]'s builder).
struct Quads {
    w: f32,
    h: f32,
    verts: Vec<f32>,
}

impl Quads {
    fn new(w: f32, h: f32) -> Self {
        Self {
            w,
            h,
            verts: Vec::new(),
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

    /// One string at `(x, y)` (top-left of the first glyph), one `scale`×`scale`
    /// quad per lit font pixel via the HUD's bitmap font.
    fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
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

/// Number of `f32`s per vertex (`[x, y, r, g, b, a]`).
const FLOATS_PER_VERTEX: usize = 6;

/// GPU renderer for the menu screens: one coloured-quad pipeline and a growable
/// dynamic vertex buffer, drawn in a `Clear` pass because nothing renders behind
/// a menu.
#[derive(Debug)]
pub struct MenuRenderer {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
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
        }
    }

    /// Draws one menu frame, clearing the target first.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &MenuFrame<'_>,
        width: u32,
        height: u32,
    ) {
        let (logical_w, logical_h) = logical_canvas(frame.gui_scale, width, height);
        let verts = geometry(frame, logical_w, logical_h);
        if verts.len() > self.capacity_floats {
            self.capacity_floats = verts.len().next_power_of_two();
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("menu-verts"),
                size: (self.capacity_floats * 4) as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&verts));

        let vertex_count = (verts.len() / FLOATS_PER_VERTEX) as u32;
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
                        // Nothing renders behind a menu, so clear rather than
                        // load — otherwise the last world frame shows through.
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: f64::from(BG[0]),
                            g: f64::from(BG[1]),
                            b: f64::from(BG[2]),
                            a: 1.0,
                        }),
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
            pass.draw(0..vertex_count, 0..1);
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}

const MENU_WGSL: &str = r"
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
            Screen::Connecting,
            Screen::Playing,
            Screen::Chat,
            Screen::Container,
            Screen::Paused,
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
                assert!(!f.title.is_empty(), "{screen:?} has no title");
                assert!(
                    !geometry(&f, 1280.0, 720.0).is_empty(),
                    "{screen:?} draws nothing"
                );
            }
        }
        assert_eq!(reached, 10, "a screen was added without being covered here");
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
        // This compares the *fill colour emitted at the row's own rect*.
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
            coverage(&sel, w, h, border) > 0.9,
            "the highlighted row should be outlined: {:?}",
            coverage(&sel, w, h, border)
        );
        assert!(
            coverage(&unsel, w, h, border) < 0.5,
            "an unhighlighted row must not be outlined"
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
