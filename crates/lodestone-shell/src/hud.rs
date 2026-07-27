//! The heads-up display: a crosshair and an F3-style debug overlay.
//!
//! The overlay is the shell's instrument panel — position, facing, FPS, frame
//! time, chunk/section/quad counts, VRAM and process memory — so it is the first
//! thing that reveals whether the pipeline is actually fast and the first thing
//! that shows a regression. The same [`DebugStats`] is also printed to stdout on
//! a timer, so headless and windowed runs both produce evidence.
//!
//! Rendering has two streams. Text, the crosshair, and overlay chrome are
//! emitted as solid-colour quads in one dynamic vertex buffer (positions in NDC,
//! RGBA per vertex). The survival vitals — hotbar, XP bar, hearts, hunger — draw
//! from the vanilla GUI sprite atlas once [`HudRenderer::attach_gui`] supplies
//! one, via a second textured vertex stream; without an atlas (jar-less or
//! headless runs) they fall back to procedural quads on the colour stream. Both
//! streams are flat `Vec<f32>`s so they need no `bytemuck::Pod` derive (which the
//! workspace's `deny(unsafe_code)` would reject) and draw in a `Load` pass over
//! the terrain with no depth.

mod font;

pub use font::glyph_rows;

use std::sync::Arc;

use lodestone_render::{GpuAtlas, GuiAtlas, GuiSpriteQuad};

use crate::overlay::{BossBarView, Sidebar};

/// Everything the debug overlay shows for one frame.
#[derive(Debug, Clone, Default)]
pub struct DebugStats {
    /// Player feet position (world blocks).
    pub position: [f64; 3],
    /// Yaw / pitch in degrees.
    pub yaw: f32,
    /// Pitch in degrees.
    pub pitch: f32,
    /// Smoothed frames per second.
    pub fps: f32,
    /// Last frame time in milliseconds.
    pub frame_ms: f32,
    /// Loaded chunk columns.
    pub chunk_count: usize,
    /// Columns resident in the *live client-owned* world, read through
    /// [`crate::net::NetClient::loaded_chunks`]. Distinct from `chunk_count`
    /// (the locally rendered world): while connected, `0` here is the
    /// chunk-blackout signal, and a non-zero count is the section-read seam
    /// proving live world data is reaching the shell.
    pub live_columns: usize,
    /// Live columns that failed to mesh (guard rejected or all-air centre on a
    /// column the server reports loaded). Mirrors [`crate::sim::Sim`]'s
    /// `mesh_drops` counter; shown next to `LIVE COLS` so a recurrence of the
    /// silent-drop defect class is visible at a glance. Healthy sessions read `0`.
    pub mesh_drops: u64,
    /// Uploaded (non-empty) mesh sections.
    pub section_count: usize,
    /// Quads currently resident.
    pub quads: usize,
    /// Approximate mesh VRAM in bytes.
    pub vram_bytes: usize,
    /// Resident process memory in bytes (0 if unavailable).
    pub rss_bytes: usize,
    /// Heap bytes owned by loaded world chunks (`World::heap_bytes`). Per the
    /// §12.24 ruling this is the single honest world-memory number — it reads
    /// the same whether the world is locally generated or client-owned.
    pub world_bytes: usize,
    /// Physics ticks per rendered frame since start (fixed-timestep health;
    /// vanilla runs 20 ticks/s, so at 50 FPS this settles near 0.4).
    pub frames_per_tick: f32,
    /// Whether the shell is in free-fly (noclip) mode rather than physics-walk.
    pub flying: bool,
    /// The block currently targeted by the view ray, if any.
    pub target: Option<[i32; 3]>,
    /// Entity instances drawn this frame (post-frustum-cull). `0` while
    /// disconnected or when no mobs are in view.
    pub entities_drawn: usize,
    /// A short connection/status line ("local world", "connecting…", …).
    pub status: String,
}

impl DebugStats {
    /// Compass facing derived from yaw (Minecraft convention: yaw 0 = south, +Z).
    #[must_use]
    pub fn facing(&self) -> &'static str {
        // Normalise to [0,360).
        let y = self.yaw.rem_euclid(360.0);
        // south=0/360, west=90, north=180, east=270 (yaw increases clockwise).
        match y {
            v if !(45.0..315.0).contains(&v) => "south (+Z)",
            v if v < 135.0 => "west (-X)",
            v if v < 225.0 => "north (-Z)",
            _ => "east (+X)",
        }
    }

    /// The overlay's text lines.
    #[must_use]
    pub fn lines(&self) -> Vec<String> {
        vec![
            "LODESTONE".to_string(),
            format!(
                "XYZ {:.2} {:.2} {:.2}",
                self.position[0], self.position[1], self.position[2]
            ),
            format!(
                "CHUNK {} {} {}",
                (self.position[0] as i64).div_euclid(16),
                (self.position[1] as i64),
                (self.position[2] as i64).div_euclid(16)
            ),
            format!(
                "FACING {} ({:.1}/{:.1})",
                self.facing(),
                self.yaw,
                self.pitch
            ),
            format!("FPS {:.0} ({:.2} MS)", self.fps, self.frame_ms),
            format!(
                "MODE {} F/T {:.2}",
                if self.flying { "FLY" } else { "WALK" },
                self.frames_per_tick
            ),
            match self.target {
                Some([x, y, z]) => format!("TARGET {x} {y} {z}"),
                None => "TARGET -".to_string(),
            },
            format!(
                "CHUNKS {} SECTIONS {} QUADS {}",
                self.chunk_count, self.section_count, self.quads
            ),
            format!(
                "LIVE COLS {} DROPS {} ENTITIES {}",
                self.live_columns, self.mesh_drops, self.entities_drawn
            ),
            format!(
                "MESH VRAM {} KB WORLD {} KB RSS {} MB",
                self.vram_bytes / 1024,
                self.world_bytes / 1024,
                self.rss_bytes / (1024 * 1024)
            ),
            self.status.to_uppercase(),
        ]
    }

    /// One-line stdout summary (primary evidence in headless / logged runs).
    #[must_use]
    pub fn one_line(&self) -> String {
        format!(
            "pos=({:.1},{:.1},{:.1}) facing={} mode={} f/t={:.2} target={} fps={:.0} frame={:.2}ms chunks={} live_cols={} drops={} entities={} sections={} quads={} vram={}KB world={}KB rss={}MB {}",
            self.position[0],
            self.position[1],
            self.position[2],
            self.facing(),
            if self.flying { "fly" } else { "walk" },
            self.frames_per_tick,
            match self.target {
                Some([x, y, z]) => format!("{x},{y},{z}"),
                None => "-".to_string(),
            },
            self.fps,
            self.frame_ms,
            self.chunk_count,
            self.live_columns,
            self.mesh_drops,
            self.entities_drawn,
            self.section_count,
            self.quads,
            self.vram_bytes / 1024,
            self.world_bytes / 1024,
            self.rss_bytes / (1024 * 1024),
            self.status,
        )
    }
}

/// Resident set size (physical memory) of this process in bytes, or 0 only if
/// the platform genuinely cannot report it.
///
/// Reads the real per-process figure via [`memory_stats`], which uses
/// `task_info` on macOS and `/proc/self/statm` on Linux. The shell stays within
/// the workspace's `deny(unsafe_code)` because the syscall FFI lives inside that
/// crate. Previously this returned 0 on every non-Linux host, so the HUD's
/// memory gauge read a flat zero on macOS — a signal that looks like evidence
/// and isn't (§12). The [`rss_is_observable`](tests) test guards against a
/// regression back to that.
#[must_use]
pub fn process_rss_bytes() -> usize {
    memory_stats::memory_stats().map_or(0, |m| m.physical_mem)
}

/// Bytes per HUD vertex: 2 position floats + 4 colour floats.
const FLOATS_PER_VERTEX: usize = 6;

/// Floats per textured-sprite vertex: position (x, y in NDC), atlas UV (u, v),
/// and an RGBA tint. The GUI sprite stream is separate from the colour stream
/// so the existing colour pipeline is untouched.
const SPRITE_FLOATS_PER_VERTEX: usize = 8;

/// Everything the HUD draws for one frame, bundled so the geometry builder and
/// the GPU renderer take one argument that can grow without churning every call
/// site. Borrows so building it per frame allocates nothing beyond the `chat`
/// slice the caller already has.
#[derive(Debug)]
pub struct HudFrame<'a> {
    /// The debug-overlay stats (drawn only when `show_debug`).
    pub stats: &'a DebugStats,
    /// Whether the F3 debug overlay is visible.
    pub show_debug: bool,
    /// Whether to draw the centre crosshair (suppressed on menus/pause).
    pub crosshair: bool,
    /// Recent chat lines, oldest-first; drawn bottom-left. Each is a legacy
    /// `§`-code string paired with its **age in seconds**, which drives the
    /// vanilla fade-out (older lines dim, then vanish, while the box is closed).
    pub chat: &'a [(&'a str, f32)],
    /// The in-progress chat input line, `Some` only while the chat box is open.
    pub chat_input: Option<&'a str>,
    /// Formatted player-list rows, `Some` only while the tab overlay is held.
    pub players: Option<&'a [String]>,
    /// The scoreboard sidebar to draw on the right edge, `Some` when displayed.
    pub sidebar: Option<&'a Sidebar>,
    /// Active boss bars, drawn stacked at the top-centre in render order.
    pub boss_bars: &'a [BossBarView],
    /// Current player health in `0..=20`, `Some` only on a live survival server.
    pub health: Option<f32>,
    /// Current food level in `0..=20`, `Some` only on a live survival server.
    pub food: Option<i32>,
    /// The selected hotbar slot in `0..9`, `Some` while in active play. Drawn as
    /// a 9-cell bar at the bottom centre with the selected cell highlighted.
    /// Item icons are deferred (the shell has no item-texture atlas yet), so the
    /// cells are empty frames for now — the frame and selection are honest, the
    /// contents are explicitly not modelled.
    pub hotbar: Option<usize>,
    /// The XP bar `(level, progress 0..=1)`, `Some` once the server has sent
    /// experience. Drawn as a green progress bar above the hotbar with the level
    /// centred above it. Off a live server this is `None` — no bar is drawn.
    pub xp: Option<(i32, f32)>,
    /// The title/subtitle overlay `(title, subtitle, alpha)`, drawn large and
    /// centred with a server-driven fade. `None` when no title is showing.
    pub title: Option<(String, Option<String>, f32)>,
    /// The action-bar message `(text, alpha)`, drawn just above the hotbar
    /// cluster with a fade. `None` when nothing is showing.
    pub action_bar: Option<(String, f32)>,
}

impl<'a> HudFrame<'a> {
    /// A frame that draws just the debug overlay and crosshair — the default
    /// single-player / pre-connect HUD, and a concise base for tests.
    #[must_use]
    pub fn new(stats: &'a DebugStats) -> Self {
        Self {
            stats,
            show_debug: true,
            crosshair: true,
            chat: &[],
            chat_input: None,
            players: None,
            sidebar: None,
            boss_bars: &[],
            health: None,
            food: None,
            hotbar: None,
            xp: None,
            title: None,
            action_bar: None,
        }
    }
}

/// Builds the HUD vertex stream (positions in NDC, RGBA per vertex) for a given
/// viewport. Pure, so it is unit-testable without a GPU.
#[derive(Debug)]
pub struct HudGeometry {
    /// Flat `[x, y, r, g, b, a]` per vertex.
    pub verts: Vec<f32>,
    /// Flat `[x, y, u, v, r, g, b, a]` per textured GUI-sprite vertex. Empty
    /// unless a [`GuiAtlas`] was supplied to [`HudGeometry::build_with_gui`].
    pub sprite_verts: Vec<f32>,
}

impl HudGeometry {
    /// Number of vertices.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.verts.len() / FLOATS_PER_VERTEX
    }

    /// Number of textured GUI-sprite vertices.
    #[must_use]
    pub fn sprite_vertex_count(&self) -> usize {
        self.sprite_verts.len() / SPRITE_FLOATS_PER_VERTEX
    }

    /// Build the whole HUD for `width`×`height` pixels from a [`HudFrame`],
    /// drawing the survival vitals (hotbar, XP, hearts, hunger) as procedural
    /// quads. This is the jar-less / headless path.
    #[must_use]
    pub fn build(frame: &HudFrame, width: u32, height: u32) -> Self {
        Self::build_inner(frame, width, height, None)
    }

    /// Like [`build`](Self::build), but draws the survival vitals from the real
    /// vanilla GUI atlas (hearts, hunger, XP bar, hotbar frame + selection)
    /// instead of procedural quads. Everything else (debug text, chat, sidebar,
    /// crosshair, …) is identical and still emitted to the colour stream.
    #[must_use]
    pub fn build_with_gui(frame: &HudFrame, width: u32, height: u32, gui: &GuiAtlas) -> Self {
        Self::build_inner(frame, width, height, Some(gui))
    }

    fn build_inner(frame: &HudFrame, width: u32, height: u32, gui: Option<&GuiAtlas>) -> Self {
        let mut b = Builder::new(width.max(1) as f32, height.max(1) as f32, gui);

        let scale = 2.0;
        let margin = 6.0;
        let glyph_h = font::GLYPH_H as f32;
        let line_h = (glyph_h + 2.0) * scale;

        // Debug text, top-left.
        if frame.show_debug {
            for (i, line) in frame.stats.lines().iter().enumerate() {
                let y = margin + i as f32 * line_h;
                b.text(line, margin, y, scale, [0.96, 0.98, 1.0, 1.0]);
            }
        }

        // Chat, bottom-left: an optional input line at the very bottom, with the
        // received log stacked above it. Received lines carry legacy `§` colour
        // codes (rendered as coloured runs) and fade out with age like vanilla
        // once the box is closed; while it's open, the full history stays lit.
        let chat_open = frame.chat_input.is_some();
        let input_y = b.h - margin - glyph_h * scale;
        if let Some(input) = frame.chat_input {
            // A translucent strip so text stays legible over bright terrain.
            b.rect_px(0.0, input_y - 3.0, b.w * 0.6, line_h, [0.0, 0.0, 0.0, 0.55]);
            // A trailing underscore stands in for a caret (no blink). The typed
            // line is always plain (input filters `§`), so a flat draw is right.
            b.text(
                &format!("> {input}_"),
                margin,
                input_y,
                scale,
                [1.0, 1.0, 1.0, 1.0],
            );
        }
        let chat_bottom = if chat_open { input_y } else { b.h - margin };
        // Show more history while actively typing than during play.
        let max_lines = if chat_open { 18 } else { 10 };
        for (i, (line, age)) in frame.chat.iter().rev().take(max_lines).enumerate() {
            // While open, every line is fully lit; while closed, lines fade over
            // their last two seconds of a ten-second life and then disappear.
            let alpha = if chat_open {
                1.0
            } else {
                chat_line_alpha(*age)
            };
            if alpha <= 0.0 {
                // Older-than-visible lines end the stack: everything above is
                // older still, so there is nothing more to draw.
                break;
            }
            let y = chat_bottom - (i as f32 + 1.0) * line_h;
            if y < margin {
                break;
            }
            b.rect_px(
                0.0,
                y - 1.0,
                b.w * 0.6,
                line_h,
                [0.0, 0.0, 0.0, 0.4 * alpha],
            );
            b.text_legacy(line, margin, y, scale, [0.92, 0.94, 1.0], alpha);
        }

        // Crosshair: a white plus at the centre.
        if frame.crosshair {
            let (cx, cy) = (b.w * 0.5, b.h * 0.5);
            let arm = 8.0;
            let thick = 2.0;
            let col = [1.0, 1.0, 1.0, 0.85];
            b.rect_px(cx - arm, cy - thick * 0.5, arm * 2.0, thick, col);
            b.rect_px(cx - thick * 0.5, cy - arm, thick, arm * 2.0, col);
        }

        // The hotbar (bottom-centre) and the survival pip rows above it. The
        // hotbar draws whenever we're in active play; the pips only on a live
        // survival server that reports health/food.
        let cx = b.w * 0.5;
        // With the vanilla GUI atlas attached, the vitals cluster (hotbar, XP,
        // hearts, hunger) draws from real sprites; without it — jar-less runs and
        // the headless negative-control path — it falls back to the procedural
        // quads below. Both branches return `bars_y`, the anchor the action bar
        // sits above, so the rest of the HUD is oblivious to which drew.
        let bars_y = if b.gui.is_some() {
            sprite_vitals(&mut b, frame)
        } else {
            let pip = 8.0;
            let gap = 2.0;
            let row_w = 10.0 * (pip + gap);

            // Hotbar: a 9-cell bar with the selected cell ringed in white. Item
            // icons are deferred (no item atlas yet) so the cells are empty wells —
            // the frame and selection are real, the contents explicitly aren't.
            let hotbar_top = if let Some(sel) = frame.hotbar {
                let sel = sel.min(8);
                let cell = 22.0;
                let hw = 9.0 * cell;
                let hx = cx - hw * 0.5;
                let hy = b.h - margin - cell;
                b.rect_px(
                    hx - 2.0,
                    hy - 2.0,
                    hw + 4.0,
                    cell + 4.0,
                    [0.0, 0.0, 0.0, 0.55],
                );
                for i in 0..9 {
                    let sx = hx + i as f32 * cell;
                    b.rect_px(
                        sx + 1.0,
                        hy + 1.0,
                        cell - 2.0,
                        cell - 2.0,
                        [0.28, 0.28, 0.30, 0.5],
                    );
                }
                // A 2px white ring around the selected cell (four edges).
                let sx = hx + sel as f32 * cell;
                let bw = 2.0;
                let col = [0.95, 0.97, 1.0, 0.95];
                b.rect_px(sx - 1.0, hy - 1.0, cell + 2.0, bw, col);
                b.rect_px(sx - 1.0, hy + cell + 1.0 - bw, cell + 2.0, bw, col);
                b.rect_px(sx - 1.0, hy - 1.0, bw, cell + 2.0, col);
                b.rect_px(sx + cell + 1.0 - bw, hy - 1.0, bw, cell + 2.0, col);
                hy
            } else {
                b.h - margin
            };

            // XP bar: a full-hotbar-width green progress bar just above the hotbar,
            // with the level number centred above it (vanilla green). Drawn only
            // once the server has sent experience (`frame.xp`); off a live server
            // this is `None` and nothing draws, keeping the gauge honest.
            let vitals_base = if let Some((level, progress)) = frame.xp {
                let bar_w = 9.0 * 22.0;
                let bx = cx - bar_w * 0.5;
                let bar_h = 4.0;
                let by = hotbar_top - bar_h - 5.0;
                b.rect_px(bx, by, bar_w, bar_h, [0.0, 0.0, 0.0, 0.7]);
                let fill = bar_w * progress.clamp(0.0, 1.0);
                if fill > 0.0 {
                    b.rect_px(bx, by, fill, bar_h, [0.47, 0.82, 0.16, 1.0]);
                }
                let level_gap = if level > 0 {
                    let s = level.to_string();
                    let tw = text_w(&s, scale);
                    b.text(
                        &s,
                        cx - tw * 0.5,
                        by - line_h,
                        scale,
                        [0.44, 0.92, 0.20, 1.0],
                    );
                    line_h
                } else {
                    0.0
                };
                by - level_gap
            } else {
                hotbar_top
            };

            // Health / food pip rows, sitting just above the hotbar (or the XP bar
            // when one is drawn). Each row is 10 pips of 2 units; a pip lights the
            // moment any of its two units is present (a deliberate simplification —
            // no half-pip art yet).
            let bars_y = vitals_base - pip - 4.0;
            if let Some(hp) = frame.health {
                b.pips(
                    hp,
                    cx - row_w - 8.0,
                    bars_y,
                    pip,
                    gap,
                    [0.86, 0.15, 0.16, 1.0],
                );
            }
            if let Some(food) = frame.food {
                b.pips(
                    food as f32,
                    cx + 8.0,
                    bars_y,
                    pip,
                    gap,
                    [0.78, 0.60, 0.20, 1.0],
                );
            }

            bars_y
        };

        // Action bar: a single centred line just above the vitals/XP cluster,
        // fading with the server-driven alpha. Legacy `§` colour codes render.
        if let Some((msg, alpha)) = frame.action_bar.as_ref().filter(|(_, a)| *a > 0.0) {
            let tw = text_w(msg, scale);
            b.text_legacy(
                msg,
                cx - tw * 0.5,
                bars_y - line_h - 6.0,
                scale,
                [1.0, 1.0, 1.0],
                *alpha,
            );
        }

        // Title / subtitle: a large centred overlay mid-screen, fading with the
        // server-driven alpha. Drawn only while a server-sent title is active,
        // so it costs nothing off a server that sends none.
        if let Some((title, subtitle, alpha)) = frame.title.as_ref().filter(|(_, _, a)| *a > 0.0) {
            let ts = scale * 4.0;
            let tw = text_w(title, ts);
            let ty = b.h * 0.40;
            b.text(title, (b.w - tw) * 0.5, ty, ts, [1.0, 1.0, 1.0, *alpha]);
            if let Some(sub) = subtitle {
                let ss = scale * 2.0;
                let sw = text_w(sub, ss);
                b.text(
                    sub,
                    (b.w - sw) * 0.5,
                    ty + ts * 9.0,
                    ss,
                    [1.0, 1.0, 1.0, *alpha],
                );
            }
        }

        // Boss bars: stacked title-over-bar at the top-centre. The fill is
        // tinted by the bar's colour and clamped progress; an empty slice draws
        // nothing, so this costs zero verts off a server that sends none.
        if !frame.boss_bars.is_empty() {
            let bar_w = b.w * 0.4;
            let bar_h = 6.0;
            let bx = (b.w - bar_w) * 0.5;
            for (i, bb) in frame.boss_bars.iter().enumerate() {
                let top = margin + i as f32 * (line_h + bar_h + 6.0);
                let tw = text_w(&bb.title, scale);
                b.text(
                    &bb.title,
                    (b.w - tw) * 0.5,
                    top,
                    scale,
                    [1.0, 1.0, 1.0, 1.0],
                );
                let bar_y = top + line_h;
                b.rect_px(bx, bar_y, bar_w, bar_h, [0.08, 0.08, 0.10, 0.75]);
                let fill = bar_w * bb.progress.clamp(0.0, 1.0);
                let c = bb.color;
                b.rect_px(bx, bar_y, fill, bar_h, [c[0], c[1], c[2], 0.95]);
            }
        }

        // Scoreboard sidebar: a right-edge, vertically-centred panel. The title
        // is centred; each row puts its label at the left and the score in red,
        // right-aligned — vanilla's layout. Absent when nothing is displayed.
        if let Some(side) = frame.sidebar {
            let pad = 4.0;
            let mut content_w = text_w(&side.title, scale);
            for l in &side.lines {
                content_w = content_w.max(text_w(&l.label, scale) + 12.0 + text_w(&l.score, scale));
            }
            let panel_w = content_w + pad * 2.0;
            let panel_h = (side.lines.len() as f32 + 1.0) * line_h + pad * 2.0;
            let px = b.w - panel_w - margin;
            let py = ((b.h - panel_h) * 0.5).max(margin);
            b.rect_px(px, py, panel_w, panel_h, [0.0, 0.0, 0.0, 0.55]);
            let title_x = px + (panel_w - text_w(&side.title, scale)) * 0.5;
            b.text(&side.title, title_x, py + pad, scale, [1.0, 1.0, 1.0, 1.0]);
            for (i, l) in side.lines.iter().enumerate() {
                let y = py + pad + (i as f32 + 1.0) * line_h;
                b.text(&l.label, px + pad, y, scale, [0.85, 0.90, 1.0, 1.0]);
                let sx = px + panel_w - pad - text_w(&l.score, scale);
                b.text(&l.score, sx, y, scale, [0.95, 0.35, 0.35, 1.0]);
            }
        }

        // Tab player-list overlay: a centred panel of rows while Tab is held.
        if let Some(players) = frame.players {
            let rows = players.len().max(1);
            let panel_h = (rows as f32 + 1.0) * line_h + margin * 2.0;
            let panel_w = b.w * 0.5;
            let px = cx - panel_w * 0.5;
            let py = (b.h - panel_h) * 0.5;
            b.rect_px(px, py, panel_w, panel_h, [0.0, 0.0, 0.0, 0.7]);
            b.text(
                &format!("PLAYERS ({})", players.len()),
                px + margin,
                py + margin,
                scale,
                [1.0, 1.0, 0.6, 1.0],
            );
            for (i, row) in players.iter().enumerate() {
                let y = py + margin + (i as f32 + 1.0) * line_h;
                b.text(row, px + margin, y, scale, [0.9, 0.95, 1.0, 1.0]);
            }
        }

        Self {
            verts: b.verts,
            sprite_verts: b.sprite_verts,
        }
    }
}

/// Draw the survival vitals cluster — hotbar frame, selection highlight, XP bar
/// (background + progress), hearts, and hunger — from the vanilla GUI atlas.
/// Returns `bars_y`, the top of the hearts/hunger row, which the action bar sits
/// above. Layout mirrors the procedural fallback closely so toggling the atlas
/// on or off does not visibly jump the HUD. A no-op-safe: [`Builder::sprite`]
/// draws nothing for a missing sprite, so a partial atlas degrades gracefully.
fn sprite_vitals(b: &mut Builder, frame: &HudFrame) -> f32 {
    // Vanilla "GUI Scale 2": native sprite pixels are doubled on screen. At an
    // integer scale the atlas sampler's Nearest magnification replicates texels
    // exactly, so on-screen pixels equal jar pixels — which the GPU gate checks.
    const S: f32 = 2.0;
    let white = [1.0, 1.0, 1.0, 1.0];
    let cx = b.w * 0.5;
    let margin = 6.0;

    // Hotbar (182x22 native), centred at the bottom, with the 24x23 selection
    // sprite over the chosen slot.
    let hw = 182.0 * S;
    let hh = 22.0 * S;
    let hx = cx - hw * 0.5;
    let hy = b.h - hh - margin;
    let mut cluster_top = b.h - margin;
    if let Some(sel) = frame.hotbar {
        b.sprite("hud/hotbar", hx, hy, hw, hh, white);
        // Vanilla draws the selection at native offset (slot*20 - 1, -1) from the
        // hotbar origin; the sprite is 24x23 so it overhangs the 20px slot pitch.
        let sel = sel.min(8) as f32;
        let sw = 24.0 * S;
        let sh = 23.0 * S;
        let sx = hx + (sel * 20.0 - 1.0) * S;
        let sy = hy - S;
        b.sprite("hud/hotbar_selection", sx, sy, sw, sh, white);
        cluster_top = hy;
    }

    // XP bar (182x5), just above the hotbar: full background, then the progress
    // sprite cropped left-to-right to its filled fraction.
    let bar_w = 182.0 * S;
    let bar_h = 5.0 * S;
    if let Some((level, progress)) = frame.xp {
        let by = hy - bar_h - 4.0;
        b.sprite("hud/experience_bar_background", hx, by, bar_w, bar_h, white);
        let p = progress.clamp(0.0, 1.0);
        if p > 0.0 {
            // Crop by shrinking both the destination width and the sampled UV
            // span, so the bar reveals its pattern instead of squashing it.
            for mut q in b.gui_geometry("hud/experience_bar_progress", hx, by, bar_w, bar_h) {
                let span = q.uv_max[0] - q.uv_min[0];
                q.dst[2] *= p;
                q.uv_max[0] = q.uv_min[0] + span * p;
                b.push_sprite_quad(q, white);
            }
        }
        // The level number stays coloured text (vanilla green), centred above.
        if level > 0 {
            let scale = 2.0;
            let line_h = (font::GLYPH_H as f32 + 2.0) * scale;
            let s = level.to_string();
            let tw = text_w(&s, scale);
            b.text(
                &s,
                cx - tw * 0.5,
                by - line_h,
                scale,
                [0.44, 0.92, 0.20, 1.0],
            );
        }
        cluster_top = by;
    }

    // Hearts (health) left, hunger right, one row above the cluster. Each icon
    // is 9x9 native, stepped 8px (vanilla spacing); a container/empty backing is
    // drawn first, then a full or half overlay per two points.
    let icon = 9.0 * S;
    let step = 8.0 * S;
    let row_y = cluster_top - icon - 4.0;
    if let Some(hp) = frame.health {
        let hp = hp.max(0.0);
        for i in 0..10 {
            let x = hx + i as f32 * step;
            b.sprite("hud/heart/container", x, row_y, icon, icon, white);
            let units = hp - i as f32 * 2.0;
            if units >= 2.0 {
                b.sprite("hud/heart/full", x, row_y, icon, icon, white);
            } else if units >= 1.0 {
                b.sprite("hud/heart/half", x, row_y, icon, icon, white);
            }
        }
    }
    if let Some(food) = frame.food {
        let food = food.max(0) as f32;
        for i in 0..10 {
            // Hunger fills right-to-left in vanilla.
            let x = hx + hw - icon - i as f32 * step;
            b.sprite("hud/food_empty", x, row_y, icon, icon, white);
            let units = food - i as f32 * 2.0;
            if units >= 2.0 {
                b.sprite("hud/food_full", x, row_y, icon, icon, white);
            } else if units >= 1.0 {
                b.sprite("hud/food_half", x, row_y, icon, icon, white);
            }
        }
    }

    row_y
}

/// Pixel width of `s` in the fixed-advance HUD font at `scale` (matches
/// [`Builder::text`]'s per-glyph advance, so right-alignment lines up exactly).
fn text_w(s: &str, scale: f32) -> f32 {
    s.chars().count() as f32 * (font::GLYPH_W as f32 + 1.0) * scale
}

/// A chat line is fully lit for most of its life, then fades over its last
/// [`CHAT_FADE_SECS`] before disappearing at [`CHAT_VISIBLE_SECS`] — matching
/// vanilla's "recent messages fade out when the box is closed" behaviour. Only
/// used while the chat box is closed; open, every line is drawn at full alpha.
fn chat_line_alpha(age: f32) -> f32 {
    const CHAT_VISIBLE_SECS: f32 = 10.0;
    const CHAT_FADE_SECS: f32 = 2.0;
    if age <= CHAT_VISIBLE_SECS - CHAT_FADE_SECS {
        1.0
    } else if age >= CHAT_VISIBLE_SECS {
        0.0
    } else {
        (CHAT_VISIBLE_SECS - age) / CHAT_FADE_SECS
    }
}

/// The RGB of one of the sixteen legacy `§` colour codes (`0`..=`9`, `a`..=`f`),
/// or `None` for a format/reset code. These are the standard Minecraft chat
/// foreground colours; the shell paints them locally, which is a rendering
/// concern (how to colour a run), not protocol knowledge.
fn legacy_rgb(code: char) -> Option<[f32; 3]> {
    let hex: u32 = match code.to_ascii_lowercase() {
        '0' => 0x000000,
        '1' => 0x0000aa,
        '2' => 0x00aa00,
        '3' => 0x00aaaa,
        '4' => 0xaa0000,
        '5' => 0xaa00aa,
        '6' => 0xffaa00,
        '7' => 0xaaaaaa,
        '8' => 0x555555,
        '9' => 0x5555ff,
        'a' => 0x55ff55,
        'b' => 0x55ffff,
        'c' => 0xff5555,
        'd' => 0xff55ff,
        'e' => 0xffff55,
        'f' => 0xffffff,
        _ => return None,
    };
    Some([
        ((hex >> 16) & 0xff) as f32 / 255.0,
        ((hex >> 8) & 0xff) as f32 / 255.0,
        (hex & 0xff) as f32 / 255.0,
    ])
}

struct Builder<'a> {
    w: f32,
    h: f32,
    verts: Vec<f32>,
    sprite_verts: Vec<f32>,
    gui: Option<&'a GuiAtlas>,
}

impl<'a> Builder<'a> {
    fn new(w: f32, h: f32, gui: Option<&'a GuiAtlas>) -> Self {
        Self {
            w,
            h,
            verts: Vec::new(),
            sprite_verts: Vec::new(),
            gui,
        }
    }

    /// Emit a GUI sprite scaled into the pixel rect `(x, y, w, h)`, tinted by
    /// `c`. A no-op when no atlas is attached or the id is unknown, so callers
    /// need not branch.
    fn sprite(&mut self, id: &str, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
        for q in self.gui_geometry(id, x, y, w, h) {
            self.push_sprite_quad(q, c);
        }
    }

    /// The raw textured quads for a sprite, for callers that post-process them
    /// (for example cropping the XP progress bar to its filled fraction). Empty
    /// when no atlas is attached or the id is unknown.
    fn gui_geometry(&self, id: &str, x: f32, y: f32, w: f32, h: f32) -> Vec<GuiSpriteQuad> {
        match self.gui {
            Some(gui) => gui.geometry(id, x, y, w, h),
            None => Vec::new(),
        }
    }

    /// Push one textured quad (two triangles) from an absolute-pixel destination
    /// rect and its atlas UVs, tinted by `c`.
    fn push_sprite_quad(&mut self, q: GuiSpriteQuad, c: [f32; 4]) {
        let to_ndc = |px: f32, py: f32| (2.0 * px / self.w - 1.0, 1.0 - 2.0 * py / self.h);
        let [dx, dy, dw, dh] = q.dst;
        let (x0, y0) = to_ndc(dx, dy);
        let (x1, y1) = to_ndc(dx + dw, dy + dh);
        let [u0, v0] = q.uv_min;
        let [u1, v1] = q.uv_max;
        let mut v = |vx: f32, vy: f32, tu: f32, tv: f32| {
            self.sprite_verts
                .extend_from_slice(&[vx, vy, tu, tv, c[0], c[1], c[2], c[3]]);
        };
        v(x0, y0, u0, v0);
        v(x1, y0, u1, v0);
        v(x1, y1, u1, v1);
        v(x0, y0, u0, v0);
        v(x1, y1, u1, v1);
        v(x0, y1, u0, v1);
    }

    /// Emit a pixel-space rectangle as two triangles in NDC.
    fn rect_px(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
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

    /// Emit a row of 10 pips representing a `0..=20` gauge (health/food). A pip
    /// lights once any of its two units is present; empty pips render as a dark
    /// slot so the gauge width reads at a glance.
    fn pips(&mut self, units: f32, x: f32, y: f32, pip: f32, gap: f32, full: [f32; 4]) {
        let empty = [0.12, 0.12, 0.14, 0.8];
        for i in 0..10 {
            let lit = units > (i as f32) * 2.0;
            let col = if lit { full } else { empty };
            self.rect_px(x + i as f32 * (pip + gap), y, pip, pip, col);
        }
    }

    /// Emit a string starting at pixel `(x, y)` (top-left of first glyph).
    fn text(&mut self, s: &str, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        let advance = (font::GLYPH_W as f32 + 1.0) * scale;
        let mut cursor = x;
        for ch in s.chars() {
            self.glyph(ch, cursor, y, scale, c);
            cursor += advance;
        }
    }

    /// Draw a single glyph with its top-left at `(x, y)`. Space and unknown
    /// handling match [`font::glyph_rows`]; blanks emit no quads.
    fn glyph(&mut self, ch: char, x: f32, y: f32, scale: f32, c: [f32; 4]) {
        if ch == ' ' {
            return;
        }
        let rows = font::glyph_rows(ch);
        for (ry, row) in rows.iter().enumerate() {
            for rx in 0..font::GLYPH_W {
                let bit = (row >> (font::GLYPH_W - 1 - rx)) & 1;
                if bit == 1 {
                    self.rect_px(
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

    /// Emit a string carrying legacy `§` colour/format codes as coloured runs.
    /// Colour codes (`§0`..=`§f`) recolour the following text; `§r` resets to
    /// `base`; format codes (`§k`/`l`/`m`/`n`/`o`) are consumed but not styled
    /// (the shell's bitmap font has no bold/italic variants). Each code pair is
    /// **zero-width**, matching vanilla's "`§` codes are 2 chars / 0 width", so
    /// coloured and plain text of the same visible length line up exactly.
    /// `alpha` scales every run for the fade-out.
    fn text_legacy(&mut self, s: &str, x: f32, y: f32, scale: f32, base: [f32; 3], alpha: f32) {
        let advance = (font::GLYPH_W as f32 + 1.0) * scale;
        let mut cursor = x;
        let mut rgb = base;
        let mut chars = s.chars();
        while let Some(ch) = chars.next() {
            if ch == '\u{00a7}' {
                // A colour/format code: consume the following selector, adjust
                // state, and advance the cursor by nothing.
                match chars.next() {
                    Some(code) => {
                        if let Some(c) = legacy_rgb(code) {
                            rgb = c;
                        } else if code.eq_ignore_ascii_case(&'r') {
                            rgb = base;
                        }
                        // Format codes (k/l/m/n/o) and unknowns: swallowed.
                    }
                    None => break,
                }
                continue;
            }
            self.glyph(ch, cursor, y, scale, [rgb[0], rgb[1], rgb[2], alpha]);
            cursor += advance;
        }
    }
}

/// GPU renderer for the HUD: a simple coloured-quad pipeline plus a growable
/// dynamic vertex buffer.
#[derive(Debug)]
pub struct HudRenderer {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
    gui: Option<GuiHud>,
}

/// The GPU resources for drawing HUD sprites from the vanilla GUI atlas: the
/// uploaded atlas texture, its textured pipeline + bind group, and a dynamic
/// vertex buffer. Present only once [`HudRenderer::attach_gui`] has run; absent
/// on jar-less / headless runs, where the HUD falls back to procedural quads.
#[derive(Debug)]
struct GuiHud {
    atlas: Arc<GuiAtlas>,
    #[allow(dead_code)]
    gpu: GpuAtlas,
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
}

impl HudRenderer {
    /// Build the HUD pipeline for a target of `color_format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hud-shader"),
            source: wgpu::ShaderSource::Wgsl(HUD_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("hud-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hud-pipeline"),
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
            label: Some("hud-verts"),
            size: (capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });

        Self {
            pipeline,
            buffer,
            capacity_floats,
            gui: None,
        }
    }

    /// Attach the vanilla GUI sprite atlas so the survival vitals (hearts,
    /// hunger, XP bar, hotbar frame + selection) render from real textures.
    /// Uploads the atlas, builds the textured pipeline, and binds it. Without
    /// this call the HUD keeps its procedural fallback — the jar-less runtime
    /// behaviour and the headless negative control the GPU gate exercises.
    pub fn attach_gui(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        color_format: wgpu::TextureFormat,
        atlas: Arc<GuiAtlas>,
    ) {
        let gpu = GpuAtlas::from_atlas(device, queue, atlas.atlas());
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("hud-sprite-shader"),
            source: wgpu::ShaderSource::Wgsl(HUD_SPRITE_WGSL.into()),
        });
        let bind_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("hud-sprite-bgl"),
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
            label: Some("hud-sprite-layout"),
            bind_group_layouts: &[Some(&bind_layout)],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("hud-sprite-pipeline"),
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
            label: Some("hud-sprite-bg"),
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
        let capacity_floats = 4096;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("hud-sprite-verts"),
            size: (capacity_floats * 4) as wgpu::BufferAddress,
            usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        self.gui = Some(GuiHud {
            atlas,
            gpu,
            pipeline,
            bind_group,
            buffer,
            capacity_floats,
        });
    }

    /// Draw the HUD over the current frame contents (a `Load` pass, no depth).
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        frame: &HudFrame,
        width: u32,
        height: u32,
    ) {
        // With the GUI atlas attached, the vitals come back as textured sprite
        // verts; otherwise the whole HUD is the procedural colour stream.
        let gui_atlas = self.gui.as_ref().map(|g| Arc::clone(&g.atlas));
        let geo = match &gui_atlas {
            Some(atlas) => HudGeometry::build_with_gui(frame, width, height, atlas),
            None => HudGeometry::build(frame, width, height),
        };
        if geo.verts.is_empty() && geo.sprite_verts.is_empty() {
            return;
        }

        // Grow + upload the colour stream.
        if !geo.verts.is_empty() {
            if geo.verts.len() > self.capacity_floats {
                self.capacity_floats = geo.verts.len().next_power_of_two();
                self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("hud-verts"),
                    size: (self.capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&geo.verts));
        }

        // Grow + upload the sprite stream (only when an atlas is attached).
        if !geo.sprite_verts.is_empty()
            && let Some(g) = self.gui.as_mut()
        {
            if geo.sprite_verts.len() > g.capacity_floats {
                g.capacity_floats = geo.sprite_verts.len().next_power_of_two();
                g.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("hud-sprite-verts"),
                    size: (g.capacity_floats * 4) as wgpu::BufferAddress,
                    usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
            }
            queue.write_buffer(&g.buffer, 0, bytemuck::cast_slice(&geo.sprite_verts));
        }
        let colour_count = geo.vertex_count() as u32;
        let sprite_count = geo.sprite_vertex_count() as u32;
        let mut encoder =
            device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("hud") });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hud-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Load,
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
            // Sprites first, then the colour stream (text) on top, so future
            // overlays like item counts land above the hotbar art.
            if let Some(g) = &self.gui
                && sprite_count > 0
            {
                pass.set_pipeline(&g.pipeline);
                pass.set_bind_group(0, &g.bind_group, &[]);
                pass.set_vertex_buffer(0, g.buffer.slice(..));
                pass.draw(0..sprite_count, 0..1);
            }
            if colour_count > 0 {
                pass.set_pipeline(&self.pipeline);
                pass.set_vertex_buffer(0, self.buffer.slice(..));
                pass.draw(0..colour_count, 0..1);
            }
        }
        queue.submit(std::iter::once(encoder.finish()));
    }
}

const HUD_WGSL: &str = r"
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

const HUD_SPRITE_WGSL: &str = r"
struct VsOut {
    @builtin(position) clip: vec4<f32>,
    @location(0) uv: vec2<f32>,
    @location(1) tint: vec4<f32>,
};

@group(0) @binding(0) var atlas_tex: texture_2d<f32>;
@group(0) @binding(1) var atlas_samp: sampler;

@vertex
fn vs_main(
    @location(0) pos: vec2<f32>,
    @location(1) uv: vec2<f32>,
    @location(2) tint: vec4<f32>,
) -> VsOut {
    var out: VsOut;
    out.clip = vec4<f32>(pos, 0.0, 1.0);
    out.uv = uv;
    out.tint = tint;
    return out;
}

@fragment
fn fs_main(in: VsOut) -> @location(0) vec4<f32> {
    return textureSample(atlas_tex, atlas_samp, in.uv) * in.tint;
}
";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rss_is_observable() {
        // The memory gauge must read a real, non-zero RSS on the host running
        // the tests. A zero here is exactly the broken-gauge regression the fix
        // addressed: a HUD field that reads 0 gets believed. A live process
        // always has a resident set, so >1 MiB is a safe, non-vacuous floor.
        let rss = process_rss_bytes();
        assert!(
            rss > 1 << 20,
            "process RSS should be observable (>1 MiB), got {rss} bytes — memory gauge is broken"
        );
    }

    #[test]
    fn facing_from_yaw() {
        let mut s = DebugStats {
            yaw: 0.0,
            ..Default::default()
        };
        assert_eq!(s.facing(), "south (+Z)");
        s.yaw = 90.0;
        assert_eq!(s.facing(), "west (-X)");
        s.yaw = 180.0;
        assert_eq!(s.facing(), "north (-Z)");
        s.yaw = 270.0;
        assert_eq!(s.facing(), "east (+X)");
        s.yaw = -90.0;
        assert_eq!(s.facing(), "east (+X)");
    }

    #[test]
    fn geometry_has_crosshair_and_text() {
        let stats = DebugStats {
            position: [1.0, 64.0, 2.0],
            status: "local world".into(),
            ..Default::default()
        };
        let geo = HudGeometry::build(&HudFrame::new(&stats), 320, 240);
        // Crosshair alone is 2 quads = 12 verts; text adds far more.
        assert!(geo.vertex_count() > 100, "expected glyphs + crosshair");
        assert_eq!(geo.verts.len() % FLOATS_PER_VERTEX, 0);
    }

    #[test]
    fn empty_string_advances_without_panicking() {
        let stats = DebugStats::default();
        let _ = HudGeometry::build(&HudFrame::new(&stats), 1, 1);
    }

    #[test]
    fn hiding_the_debug_overlay_removes_its_geometry() {
        let stats = DebugStats {
            status: "local world".into(),
            ..Default::default()
        };
        let mut frame = HudFrame::new(&stats);
        let with = HudGeometry::build(&frame, 640, 480).vertex_count();
        frame.show_debug = false;
        let without = HudGeometry::build(&frame, 640, 480).vertex_count();
        // Only the crosshair (2 quads = 12 verts) should remain.
        assert!(without < with, "F3 off must drop the overlay glyphs");
        assert_eq!(without, 12, "just the crosshair survives");
    }

    #[test]
    fn chat_input_and_log_add_geometry() {
        let stats = DebugStats::default();
        let base = HudGeometry::build(&HudFrame::new(&stats), 640, 480).vertex_count();
        let chat = [("<a> hi", 0.0_f32), ("<b> yo", 0.0)];
        let frame = HudFrame {
            chat: &chat,
            chat_input: Some("hello"),
            ..HudFrame::new(&stats)
        };
        let with_chat = HudGeometry::build(&frame, 640, 480).vertex_count();
        assert!(with_chat > base, "chat log + input line must add geometry");
    }

    #[test]
    fn chat_colour_codes_are_zero_width_and_recolour_runs() {
        let stats = DebugStats::default();
        // A `§c` prefix must not add glyph geometry (codes are 2 chars / 0 width):
        // "§chi" and "hi" draw the same number of lit pixels.
        let plain = [("hi", 0.0_f32)];
        let coded = [("\u{00a7}chi", 0.0_f32)];
        let plain_geo = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &plain,
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        let coded_geo = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &coded,
                ..HudFrame::new(&stats)
            },
            640,
            480,
        );
        assert_eq!(
            plain_geo.vertex_count(),
            coded_geo.vertex_count(),
            "a colour code must draw no glyphs of its own"
        );
        // …but the pixels must be a different colour, so the code isn't ignored.
        assert_ne!(
            plain_geo.verts, coded_geo.verts,
            "a colour code must recolour the run, not merely be stripped"
        );
    }

    #[test]
    fn chat_lines_fade_out_with_age_when_closed() {
        let stats = DebugStats::default();
        // A fresh line draws; a line older than the visible window draws nothing.
        let fresh = [("hello", 0.0_f32)];
        let stale = [("hello", 30.0_f32)];
        let fresh_n = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &fresh,
                ..HudFrame::new(&stats)
            },
            640,
            480,
        )
        .vertex_count();
        let stale_n = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &stale,
                ..HudFrame::new(&stats)
            },
            640,
            480,
        )
        .vertex_count();
        assert!(fresh_n > 0, "a fresh chat line must be visible");
        assert_eq!(
            stale_n, 0,
            "a line past its lifetime must vanish when closed"
        );

        // Opening the box (a chat_input present) resurrects the stale line.
        let opened = HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                chat: &stale,
                chat_input: Some(""),
                ..HudFrame::new(&stats)
            },
            640,
            480,
        )
        .vertex_count();
        assert!(
            opened > 0,
            "an open chat box shows history regardless of age"
        );
    }

    #[test]
    fn health_pips_scale_with_value() {
        let stats = DebugStats::default();
        let mut frame = HudFrame::new(&stats);
        frame.crosshair = false;
        frame.show_debug = false;
        frame.health = Some(0.0);
        let empty = HudGeometry::build(&frame, 640, 480);
        frame.health = Some(20.0);
        let full = HudGeometry::build(&frame, 640, 480);
        // Ten pips are always drawn (lit or dark), so the *count* is identical —
        // the gauge width reads regardless of value. This guards that a zero-HP
        // frame still renders the empty slots rather than nothing (which would
        // read as "no HUD" instead of "no health").
        assert_eq!(
            empty.vertex_count(),
            full.vertex_count(),
            "ten pip quads regardless of value"
        );
        assert_eq!(empty.vertex_count(), 10 * 6, "10 pips × 6 verts each");
        // …but the *colours* must differ: a full bar's lit pips can't share the
        // empty bar's dark colour, or the gauge would never actually read HP.
        assert_ne!(
            empty.verts, full.verts,
            "full vs empty must recolour the pips, not just redraw them"
        );
    }

    #[test]
    fn hotbar_draws_and_selection_moves_the_highlight() {
        let stats = DebugStats::default();
        let mut frame = HudFrame::new(&stats);
        frame.crosshair = false;
        frame.show_debug = false;

        // No hotbar → no hotbar geometry.
        frame.hotbar = None;
        let none = HudGeometry::build(&frame, 640, 480).vertex_count();

        // A hotbar adds a real run of geometry (panel + 9 cells + a 4-edge ring).
        frame.hotbar = Some(0);
        let sel0 = HudGeometry::build(&frame, 640, 480);
        assert!(
            sel0.vertex_count() > none,
            "an on-screen hotbar must add geometry, got {} vs {none}",
            sel0.vertex_count()
        );

        // Moving the selection keeps the vertex *count* identical (same panel,
        // 9 cells, 4-edge ring) but must move the ring — so the bytes differ. A
        // selection that never relocates the highlight would render as a hotbar
        // that ignores the held slot.
        frame.hotbar = Some(4);
        let sel4 = HudGeometry::build(&frame, 640, 480);
        assert_eq!(
            sel0.vertex_count(),
            sel4.vertex_count(),
            "selecting a different slot must not change the vertex count"
        );
        assert_ne!(
            sel0.verts, sel4.verts,
            "the selection ring must move to the newly-selected slot"
        );
    }

    #[test]
    fn tab_overlay_lists_players() {
        let stats = DebugStats::default();
        let names = vec!["Alice  12ms".to_string(), "Bob  30ms".to_string()];
        let frame = HudFrame {
            players: Some(&names),
            ..HudFrame::new(&stats)
        };
        let with = HudGeometry::build(&frame, 640, 480).vertex_count();
        let without = HudGeometry::build(&HudFrame::new(&stats), 640, 480).vertex_count();
        assert!(with > without, "the tab panel + names add geometry");
    }

    #[test]
    fn sidebar_draws_title_and_scored_rows() {
        use crate::overlay::{Sidebar, SidebarLine};
        let stats = DebugStats::default();
        let base = HudFrame {
            crosshair: false,
            show_debug: false,
            ..HudFrame::new(&stats)
        };
        let base_verts = HudGeometry::build(&base, 640, 480).vertex_count();

        let side = Sidebar {
            title: "Objectives".into(),
            lines: vec![
                SidebarLine {
                    label: "Kills".into(),
                    score: "7".into(),
                },
                SidebarLine {
                    label: "Deaths".into(),
                    score: "2".into(),
                },
            ],
        };
        let frame = HudFrame {
            sidebar: Some(&side),
            ..HudFrame {
                crosshair: false,
                show_debug: false,
                ..HudFrame::new(&stats)
            }
        };
        let with = HudGeometry::build(&frame, 640, 480);
        assert!(
            with.vertex_count() > base_verts,
            "a displayed sidebar must add the panel, title and rows"
        );

        // Anti-vacuity: the score text itself must be drawn, not just the panel.
        // Dropping the scores (blank strings) must reduce the geometry, so a
        // regression that stops rendering scores can't pass this.
        let scoreless = Sidebar {
            title: "Objectives".into(),
            lines: vec![
                SidebarLine {
                    label: "Kills".into(),
                    score: String::new(),
                },
                SidebarLine {
                    label: "Deaths".into(),
                    score: String::new(),
                },
            ],
        };
        let frame_scoreless = HudFrame {
            sidebar: Some(&scoreless),
            ..HudFrame {
                crosshair: false,
                show_debug: false,
                ..HudFrame::new(&stats)
            }
        };
        let without_scores = HudGeometry::build(&frame_scoreless, 640, 480).vertex_count();
        assert!(
            with.vertex_count() > without_scores,
            "the score glyphs must contribute geometry, not just the labels"
        );
    }

    #[test]
    fn boss_bar_fill_tracks_progress_and_colour() {
        use crate::overlay::BossBarView;
        let stats = DebugStats::default();
        let base = HudFrame {
            crosshair: false,
            show_debug: false,
            ..HudFrame::new(&stats)
        };
        let base_verts = HudGeometry::build(&base, 640, 480).vertex_count();

        let full = [BossBarView {
            title: "Ender Dragon".into(),
            progress: 1.0,
            color: [0.6, 0.2, 0.8],
        }];
        let frame_full = HudFrame {
            boss_bars: &full,
            ..HudFrame {
                crosshair: false,
                show_debug: false,
                ..HudFrame::new(&stats)
            }
        };
        let with_full = HudGeometry::build(&frame_full, 640, 480);
        assert!(
            with_full.vertex_count() > base_verts,
            "a boss bar must add its title and bar geometry"
        );

        // A zero-progress bar still draws the background track (same vert count),
        // but the fill colour disappears — so the verts must differ. This guards
        // that the fill actually tracks progress rather than being cosmetic.
        let empty = [BossBarView {
            title: "Ender Dragon".into(),
            progress: 0.0,
            color: [0.6, 0.2, 0.8],
        }];
        let frame_empty = HudFrame {
            boss_bars: &empty,
            ..HudFrame {
                crosshair: false,
                show_debug: false,
                ..HudFrame::new(&stats)
            }
        };
        let with_empty = HudGeometry::build(&frame_empty, 640, 480);
        assert_ne!(
            with_full.verts, with_empty.verts,
            "full vs empty progress must change the drawn fill"
        );
    }

    #[test]
    fn one_line_is_stable() {
        let stats = DebugStats {
            position: [0.5, 40.0, -3.5],
            fps: 60.0,
            frame_ms: 16.6,
            ..Default::default()
        };
        let line = stats.one_line();
        assert!(line.contains("fps=60"));
        assert!(line.contains("frame=16.60ms"));
    }

    /// Clear `view` to an opaque `rgb` background (Rgba8Unorm is linear, so the
    /// byte value lands verbatim). Used to give the HUD's `Load` pass a known
    /// backdrop for pixel readback.
    #[cfg(test)]
    fn clear_view(
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        rgb: [u8; 3],
    ) {
        let color = wgpu::Color {
            r: f64::from(rgb[0]) / 255.0,
            g: f64::from(rgb[1]) / 255.0,
            b: f64::from(rgb[2]) / 255.0,
            a: 1.0,
        };
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("clear"),
        });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        queue.submit(std::iter::once(encoder.finish()));
    }

    /// Pixel-readback proof that server chat *reaches pixels*, not merely that a
    /// frame counter ticks. Renders the HUD (chat only — no crosshair, overlay,
    /// hotbar or vitals) over a known grey backdrop and inspects the bottom-left
    /// chat region. The discriminator is luminance: the translucent backing
    /// panel is *darker* than the grey background, the near-white glyphs are
    /// *brighter* — so text and panel are counted separately and a blank-but-
    /// panelled line cannot masquerade as rendered text.
    ///
    /// Three frames make the assertion two-sided:
    /// * no message → the region is untouched background (zero of both);
    /// * a whitespace-only line → the panel draws (dark pixels) but no glyphs;
    /// * a real line → glyphs add bright pixels the panel-only frame lacks.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn chat_text_reaches_pixels() {
        use lodestone_render::{HeadlessTarget, RenderTarget};

        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (480u32, 320u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let mut hud = HudRenderer::new(device, format);
        let stats = DebugStats::default();

        const BG: u8 = 128;
        // Count bright (glyph) and dark (panel) pixels in the bottom-left chat
        // region, well clear of the bottom-centre hotbar/vitals (which are off
        // anyway) and the top-left debug overlay.
        let x_max = (w as f32 * 0.55) as u32;
        let y_min = (h as f32 * 0.60) as u32;

        let mut render = |chat: &[(&str, f32)]| -> (usize, usize) {
            let frame = target.acquire().expect("headless acquire");
            clear_view(device, queue, frame.view(), [BG, BG, BG]);
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: false,
                chat,
                ..HudFrame::new(&stats)
            };
            hud.render(device, queue, frame.view(), &hud_frame, w, h);
            let pixels = target.read_texels(device, queue);
            let (mut bright, mut dark) = (0usize, 0usize);
            for y in y_min..h {
                for x in 0..x_max {
                    let i = ((y * w + x) * 4) as usize;
                    let avg = (u32::from(pixels[i])
                        + u32::from(pixels[i + 1])
                        + u32::from(pixels[i + 2]))
                        / 3;
                    if avg > u32::from(BG) + 30 {
                        bright += 1;
                    } else if avg + 30 < u32::from(BG) {
                        dark += 1;
                    }
                }
            }
            (bright, dark)
        };

        let (blank_bright, blank_dark) = render(&[]);
        let (panel_bright, panel_dark) = render(&[(" ", 0.0)]);
        let (text_bright, text_dark) = render(&[("chat works", 0.0)]);

        eprintln!("=== chat readback (headless) ===");
        eprintln!("blank  bright={blank_bright} dark={blank_dark}");
        eprintln!("panel  bright={panel_bright} dark={panel_dark}");
        eprintln!("text   bright={text_bright} dark={text_dark}");

        // No message: pure background — neither panel nor glyphs.
        assert_eq!(
            (blank_bright, blank_dark),
            (0, 0),
            "with no chat, the chat region must be untouched background"
        );
        // A line draws its translucent backing panel (dark) but a space has no
        // glyphs, so almost no bright pixels.
        assert!(panel_dark > 0, "a chat line must draw its backing panel");
        assert!(
            panel_bright < 50,
            "a whitespace-only line must not paint glyph pixels, got {panel_bright}"
        );
        // The glyphs of a real line add bright pixels the panel-only frame lacks
        // — this is the assertion that fails if the text were blank.
        assert!(
            text_dark > 0,
            "the text line must also draw its backing panel"
        );
        assert!(
            text_bright > panel_bright + 150,
            "chat glyphs must reach pixels over the bare panel: text_bright={text_bright}, \
             panel_bright={panel_bright}"
        );
    }

    /// Pixel-readback proof that the **XP bar** reaches pixels once the server
    /// has sent experience — the same "prove it's on screen, with a control"
    /// discipline as the chat gate. The discriminator is *green dominance*: the
    /// vanilla XP fill (and the level digits) are green (`G` well above `R`/`B`),
    /// which the grey background, grey hotbar wells, red health and gold food
    /// pips all fail, so a green-dominant pixel can only be the XP bar.
    ///
    /// Two frames make it two-sided:
    /// * `xp = None` (no server experience) → zero green pixels, no bar;
    /// * `xp = Some((level, progress))` → a run of green fill + digit pixels.
    ///
    /// The control is the load-bearing half: it fails if the bar ever draws
    /// without server-sent experience (the §12.24 "plausible gauge" trap).
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn xp_bar_reaches_pixels() {
        use lodestone_render::{HeadlessTarget, RenderTarget};

        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (480u32, 320u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let mut hud = HudRenderer::new(device, format);
        let stats = DebugStats::default();

        const BG: u8 = 128;
        // The XP bar and level digits live at the bottom-centre; scan a generous
        // bottom band there. Hotbar/vitals/crosshair are all off so nothing else
        // paints here.
        let x0 = (w as f32 * 0.20) as u32;
        let x1 = (w as f32 * 0.80) as u32;
        let y0 = (h as f32 * 0.78) as u32;

        let mut render = |xp: Option<(i32, f32)>| -> usize {
            let frame = target.acquire().expect("headless acquire");
            clear_view(device, queue, frame.view(), [BG, BG, BG]);
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: false,
                xp,
                ..HudFrame::new(&stats)
            };
            hud.render(device, queue, frame.view(), &hud_frame, w, h);
            let pixels = target.read_texels(device, queue);
            let mut green = 0usize;
            for y in y0..h {
                for x in x0..x1 {
                    let i = ((y * w + x) * 4) as usize;
                    let (r, g, b) = (
                        u32::from(pixels[i]),
                        u32::from(pixels[i + 1]),
                        u32::from(pixels[i + 2]),
                    );
                    // Green-dominant: clearly more green than red or blue, and
                    // brighter than the grey background so unblended greys and
                    // the gold food pips (high red) are excluded.
                    if g > r + 40 && g > b + 40 && g > u32::from(BG) {
                        green += 1;
                    }
                }
            }
            green
        };

        let no_xp = render(None);
        let with_xp = render(Some((5, 0.5)));

        eprintln!("=== xp bar readback (headless) ===");
        eprintln!("no_xp green={no_xp}");
        eprintln!("with_xp green={with_xp}");

        // Control: off a live server (no experience) the bar must not draw.
        assert_eq!(
            no_xp, 0,
            "without server experience the XP bar must not draw a single green pixel"
        );
        // A half-full level-5 bar paints a wide green fill plus the green level
        // digit — hundreds of pixels. This fails if the bar were blank.
        assert!(
            with_xp > 150,
            "the XP bar's green fill must reach pixels once experience arrives, got {with_xp}"
        );
    }

    /// GPU gate: the **title/subtitle** overlay and the **action bar** must reach
    /// pixels once a server sends them, and must paint **nothing** when empty.
    ///
    /// This is the "show me pixels, with a control" shape, applied to the text
    /// path (the strongest control per the director's template): an empty overlay
    /// and a populated one must give measurably different coverage inside the
    /// widget's own rect, or the text path has proven nothing.
    ///
    /// Two independent bands are scanned — the title's mid-screen rect and the
    /// action bar's lower-centre rect — and each state paints only its own band.
    /// That isolation is a second control: a blanket-fill or wrong-clear bug would
    /// light the *other* band and fail. Everything else (hotbar, vitals,
    /// crosshair, debug) is off so nothing else paints in either band.
    #[test]
    #[ignore = "requires a GPU adapter; run with --ignored"]
    fn title_and_action_bar_reach_pixels() {
        use lodestone_render::{HeadlessTarget, RenderTarget};

        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (480u32, 320u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let mut hud = HudRenderer::new(device, format);
        let stats = DebugStats::default();

        const BG: u8 = 128;
        // Title band: mid-screen, centred (title draws at y≈0.40h, tall). Action
        // band: lower-centre, above the (absent) hotbar/vitals. x kept central so
        // the bottom-left chat feed never intrudes.
        let xa = (w as f32 * 0.15) as u32;
        let xb = (w as f32 * 0.85) as u32;
        let title_y0 = (h as f32 * 0.30) as u32;
        let title_y1 = (h as f32 * 0.64) as u32;
        let act_y0 = (h as f32 * 0.78) as u32;
        let act_y1 = (h as f32 * 0.96) as u32;

        // Count near-white text texels (white glyphs on the grey clear) in a band.
        let bright_in = |pixels: &[u8], y0: u32, y1: u32| -> usize {
            let mut n = 0usize;
            for y in y0..y1 {
                for x in xa..xb {
                    let i = ((y * w + x) * 4) as usize;
                    let (r, g, b) = (pixels[i], pixels[i + 1], pixels[i + 2]);
                    if r > BG + 40 && g > BG + 40 && b > BG + 40 {
                        n += 1;
                    }
                }
            }
            n
        };

        let mut render = |title: Option<(String, Option<String>, f32)>,
                          action_bar: Option<(String, f32)>|
         -> (usize, usize) {
            let frame = target.acquire().expect("headless acquire");
            clear_view(device, queue, frame.view(), [BG, BG, BG]);
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: false,
                title,
                action_bar,
                ..HudFrame::new(&stats)
            };
            hud.render(device, queue, frame.view(), &hud_frame, w, h);
            let pixels = target.read_texels(device, queue);
            (
                bright_in(&pixels, title_y0, title_y1),
                bright_in(&pixels, act_y0, act_y1),
            )
        };

        let (empty_title, empty_act) = render(None, None);
        let (shown_title, title_leak_act) =
            render(Some(("TITLE".into(), Some("subtitle".into()), 1.0)), None);
        let (act_leak_title, shown_act) = render(None, Some(("Action bar!".into(), 1.0)));

        eprintln!("=== title/action-bar readback (headless) ===");
        eprintln!("empty:  title_band={empty_title} act_band={empty_act}");
        eprintln!("title:  title_band={shown_title} act_band={title_leak_act}");
        eprintln!("action: title_band={act_leak_title} act_band={shown_act}");

        // Controls: with no server title/action-bar, neither band paints a pixel.
        assert_eq!(
            (empty_title, empty_act),
            (0, 0),
            "an empty HUD must not paint the title or action-bar rects"
        );
        // The title's large glyphs + subtitle cover hundreds of texels.
        assert!(
            shown_title > 100,
            "a server-sent title must reach pixels in its rect, got {shown_title}"
        );
        // The action-bar line is smaller but still tens of texels of white text.
        assert!(
            shown_act > 40,
            "a server-sent action bar must reach pixels in its rect, got {shown_act}"
        );
        // Isolation control: each widget paints only its own band. A blanket-fill
        // or wrong-clear bug would light the other band and trip these.
        assert_eq!(
            title_leak_act, 0,
            "the title overlay must not bleed into the action-bar rect"
        );
        assert_eq!(
            act_leak_title, 0,
            "the action bar must not bleed into the title rect"
        );
    }

    /// **The closing gate for the HUD-textures island**: proves the survival
    /// vitals draw from the *actual vanilla heart sprite in `client.jar`*, not
    /// the procedural fallback, by comparing rendered pixels texel-for-texel
    /// against the jar art — then EXECUTES the negative control (no atlas
    /// attached) and confirms the same assertion *fails*. A gate never watched
    /// fail proves nothing; "it draws" is not a gate.
    ///
    /// sRGB note: the atlas uploads as `Rgba8UnormSrgb` and we render into an
    /// `Rgba8UnormSrgb` target, so the sample→tint→store roundtrip re-encodes
    /// back to ~the source bytes. We compare only *opaque* source texels — the
    /// heart's transparent corners show the backdrop and carry no identity — and
    /// at an integer 2× scale each texel maps to a clean 2×2 Nearest block.
    #[test]
    #[ignore = "requires a GPU adapter and the vanilla client.jar"]
    fn hud_vitals_draw_the_real_heart_sprite() {
        use lodestone_assets::Image;
        use lodestone_render::{HeadlessTarget, RenderTarget};

        let manager = crate::resources::vanilla_manager().expect(
            "GPU gate opted in via --ignored but no vanilla client.jar was found; set \
             LODESTONE_ASSETS to a pack root containing client.jar, or populate \
             .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
        );
        let atlas =
            Arc::new(GuiAtlas::build(&manager).expect("build the GUI atlas from client.jar"));

        // The source art we must reproduce on screen.
        let heart_png = manager
            .read("assets/minecraft/textures/gui/sprites/hud/heart/full.png")
            .expect("client.jar must carry hud/heart/full.png");
        let heart = Image::decode_png(&heart_png).expect("decode hud/heart/full.png");
        assert_eq!(
            (heart.width, heart.height),
            (9, 9),
            "the heart sprite is 9x9 native"
        );

        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU, don't 'skip' — a silent pass here asserts nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        // sRGB target so the sampler's linear decode is re-encoded on store,
        // letting opaque texels land near the source PNG bytes.
        let format = wgpu::TextureFormat::Rgba8UnormSrgb;
        let (w, h) = (480u32, 320u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let stats = DebugStats::default();

        // A backdrop that is neither red (heart) nor grey, so an opaque heart
        // texel can never be mistaken for the background.
        const BG: [u8; 3] = [24, 96, 176];

        // Only health on: no hotbar, XP or hunger, so the hearts sit at a
        // location we can compute exactly. `sprite_vitals` uses S=2; with the
        // cluster anchored at the bottom, the first heart is at (cx-182, h-28)
        // and spans 18×18 px.
        let hud_frame = HudFrame {
            show_debug: false,
            crosshair: false,
            health: Some(20.0),
            food: None,
            xp: None,
            hotbar: None,
            ..HudFrame::new(&stats)
        };
        let s = 2u32;
        let cx = w / 2;
        let x0 = cx - 182;
        let y0 = h - 28;

        // Render one frame with `hud`, read it back, and score how many *opaque*
        // heart texels match the jar sprite within tolerance after the 2× Nearest
        // downsample.
        let mut score = |hud: &mut HudRenderer, tag: &str| -> (usize, usize) {
            let frame = target.acquire().expect("headless acquire");
            clear_view(device, queue, frame.view(), BG);
            hud.render(device, queue, frame.view(), &hud_frame, w, h);
            let pixels = target.read_texels(device, queue);
            const TOL: i32 = 24;
            let (mut opaque, mut matched) = (0usize, 0usize);
            for ty in 0..9u32 {
                for tx in 0..9u32 {
                    let si = ((ty * 9 + tx) * 4) as usize;
                    if heart.rgba[si + 3] < 250 {
                        continue; // transparent corner — no identity
                    }
                    opaque += 1;
                    let px = x0 + tx * s + s / 2;
                    let py = y0 + ty * s + s / 2;
                    let di = ((py * w + px) * 4) as usize;
                    let dr = i32::from(pixels[di]) - i32::from(heart.rgba[si]);
                    let dg = i32::from(pixels[di + 1]) - i32::from(heart.rgba[si + 1]);
                    let db = i32::from(pixels[di + 2]) - i32::from(heart.rgba[si + 2]);
                    if dr.abs() <= TOL && dg.abs() <= TOL && db.abs() <= TOL {
                        matched += 1;
                    }
                }
            }
            eprintln!("{tag}: matched {matched}/{opaque} opaque heart texels");
            (matched, opaque)
        };

        // Positive: atlas attached → real heart sprite → high match.
        let mut lit = HudRenderer::new(device, format);
        lit.attach_gui(device, queue, format, Arc::clone(&atlas));
        let (pos_matched, opaque) = score(&mut lit, "vanilla-atlas");
        assert!(
            opaque > 20,
            "the heart sprite must have a solid opaque body, got {opaque}"
        );

        // Negative control, EXECUTED: no atlas → procedural fallback → the same
        // region does NOT reproduce the jar heart, so the match collapses.
        let mut dark = HudRenderer::new(device, format);
        let (neg_matched, _) = score(&mut dark, "procedural-fallback (negative control)");

        let pos_frac = pos_matched as f32 / opaque as f32;
        let neg_frac = neg_matched as f32 / opaque as f32;
        eprintln!("=== heart-sprite gate: vanilla={pos_frac:.2} fallback={neg_frac:.2} ===");

        // Load-bearing: vanilla pixels match the jar; the fallback fails the very
        // same check; and the delta is wide enough that no coincidence passes
        // both.
        assert!(
            pos_frac > 0.80,
            "with the vanilla atlas the rendered hearts must reproduce hud/heart/full.png, \
             got {pos_matched}/{opaque}"
        );
        assert!(
            neg_frac < 0.40,
            "negative control failed to fail: the procedural fallback reproduced the jar \
             heart sprite ({neg_matched}/{opaque}) — the gate would be vacuous"
        );
        assert!(
            pos_frac - neg_frac > 0.40,
            "vanilla vs fallback delta too small to prove the atlas is what reaches pixels: \
             vanilla={pos_frac:.2} fallback={neg_frac:.2}"
        );
    }
}
