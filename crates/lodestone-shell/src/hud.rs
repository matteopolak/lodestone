//! The heads-up display: a crosshair and an F3-style debug overlay.
//!
//! The overlay is the shell's instrument panel — position, facing, FPS, frame
//! time, chunk/section/quad counts, VRAM and process memory — so it is the first
//! thing that reveals whether the pipeline is actually fast and the first thing
//! that shows a regression. The same [`DebugStats`] is also printed to stdout on
//! a timer, so headless and windowed runs both produce evidence.
//!
//! Rendering is intentionally texture-free: glyphs and the crosshair are emitted
//! as solid-colour quads in one dynamic vertex buffer (positions in NDC, RGBA
//! per vertex) and drawn in a `Load` pass over the terrain with no depth. The
//! vertex stream is a flat `Vec<f32>` so it needs no `bytemuck::Pod` derive
//! (which the workspace's `deny(unsafe_code)` would reject).

mod font;

pub use font::glyph_rows;

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
            format!("LIVE COLS {} ENTITIES {}", self.live_columns, self.entities_drawn),
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
            "pos=({:.1},{:.1},{:.1}) facing={} mode={} f/t={:.2} target={} fps={:.0} frame={:.2}ms chunks={} live_cols={} entities={} sections={} quads={} vram={}KB world={}KB rss={}MB {}",
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
    /// Recent chat lines, oldest-first; the last few are drawn bottom-left.
    pub chat: &'a [&'a str],
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
        }
    }
}

/// Builds the HUD vertex stream (positions in NDC, RGBA per vertex) for a given
/// viewport. Pure, so it is unit-testable without a GPU.
#[derive(Debug)]
pub struct HudGeometry {
    /// Flat `[x, y, r, g, b, a]` per vertex.
    pub verts: Vec<f32>,
}

impl HudGeometry {
    /// Number of vertices.
    #[must_use]
    pub fn vertex_count(&self) -> usize {
        self.verts.len() / FLOATS_PER_VERTEX
    }

    /// Build the whole HUD for `width`×`height` pixels from a [`HudFrame`].
    #[must_use]
    pub fn build(frame: &HudFrame, width: u32, height: u32) -> Self {
        let mut b = Builder::new(width.max(1) as f32, height.max(1) as f32);

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
        // received log stacked above it.
        let input_y = b.h - margin - glyph_h * scale;
        if let Some(input) = frame.chat_input {
            // A translucent strip so text stays legible over bright terrain.
            b.rect_px(0.0, input_y - 3.0, b.w * 0.6, line_h, [0.0, 0.0, 0.0, 0.55]);
            // A trailing underscore stands in for a caret (no blink).
            b.text(
                &format!("> {input}_"),
                margin,
                input_y,
                scale,
                [1.0, 1.0, 1.0, 1.0],
            );
        }
        let chat_bottom = if frame.chat_input.is_some() {
            input_y
        } else {
            b.h - margin
        };
        // Show more history while actively typing than during play.
        let max_lines = if frame.chat_input.is_some() { 18 } else { 8 };
        for (i, line) in frame.chat.iter().rev().take(max_lines).enumerate() {
            let y = chat_bottom - (i as f32 + 1.0) * line_h;
            if y < margin {
                break;
            }
            b.rect_px(0.0, y - 1.0, b.w * 0.6, line_h, [0.0, 0.0, 0.0, 0.4]);
            b.text(line, margin, y, scale, [0.92, 0.94, 1.0, 1.0]);
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

        // Health / food pip rows, bottom-centre, only on a live survival server.
        // Each row is 10 pips of 2 units; a pip lights the moment any of its two
        // units is present (a deliberate simplification — no half-pip art yet).
        let cx = b.w * 0.5;
        let pip = 8.0;
        let gap = 2.0;
        let row_w = 10.0 * (pip + gap);
        let bars_y = b.h - margin - pip - line_h;
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
                b.text(&bb.title, (b.w - tw) * 0.5, top, scale, [1.0, 1.0, 1.0, 1.0]);
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

        Self { verts: b.verts }
    }
}

/// Pixel width of `s` in the fixed-advance HUD font at `scale` (matches
/// [`Builder::text`]'s per-glyph advance, so right-alignment lines up exactly).
fn text_w(s: &str, scale: f32) -> f32 {
    s.chars().count() as f32 * (font::GLYPH_W as f32 + 1.0) * scale
}

struct Builder {
    w: f32,
    h: f32,
    verts: Vec<f32>,
}

impl Builder {
    fn new(w: f32, h: f32) -> Self {
        Self {
            w,
            h,
            verts: Vec::new(),
        }
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
            if ch != ' ' {
                let rows = font::glyph_rows(ch);
                for (ry, row) in rows.iter().enumerate() {
                    for rx in 0..font::GLYPH_W {
                        let bit = (row >> (font::GLYPH_W - 1 - rx)) & 1;
                        if bit == 1 {
                            self.rect_px(
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
        }
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
        let geo = HudGeometry::build(frame, width, height);
        if geo.verts.is_empty() {
            return;
        }
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

        let vertex_count = geo.vertex_count() as u32;
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
            pass.set_pipeline(&self.pipeline);
            pass.set_vertex_buffer(0, self.buffer.slice(..));
            pass.draw(0..vertex_count, 0..1);
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
        let chat = ["<a> hi", "<b> yo"];
        let frame = HudFrame {
            chat: &chat,
            chat_input: Some("hello"),
            ..HudFrame::new(&stats)
        };
        let with_chat = HudGeometry::build(&frame, 640, 480).vertex_count();
        assert!(with_chat > base, "chat log + input line must add geometry");
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
            ..base
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
            ..base
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
            ..base
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
            ..base
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
}
