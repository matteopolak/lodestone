//! Status-effect HUD overlay: the active potion effects, drawn as a top-right
//! stack of coloured chips (an "icon" swatch plus the effect name and remaining
//! time).
//!
//! ## Why this is its own module and its own renderer
//!
//! The scoreboard sidebar and boss bars are drawn *inside* [`crate::hud`] via
//! fields on its frame struct. Status effects deliberately are **not**: the HUD
//! is owned by another agent, and folding a second surface into its single
//! geometry pass would mean editing that file. Instead this module is
//! self-contained — it folds [`ActiveEffects`] into drawable [`EffectChip`]s,
//! emits its own coloured-quad geometry (reusing only the HUD's public
//! [`crate::hud::glyph_rows`] bitmap font), and owns a tiny [`EffectsRenderer`]
//! that composites over the frame in a `Load` pass after the HUD. The seam into
//! the app is a single render call, so nothing here collides with the HUD.
//!
//! ## Layering
//!
//! State folding lives in [`lodestone_game::effect`] (`update_mob_effect` /
//! `remove_mob_effect` fold into [`ActiveEffects`], which the sim ticks down);
//! this module only *interprets* that state into pixels. The effect *identity*
//! is a canonical [`Identifier`](lodestone_model::Identifier) — never a
//! version-specific numeric id — so this overlay is version-free like the rest
//! of the shell.

use lodestone_game::effect::ActiveEffects;

use crate::hud::glyph_rows;

/// Bitmap-font cell metrics, matching [`crate::hud`]'s font (`glyph_rows`
/// returns seven 5-bit rows). Kept as local constants because the HUD's `font`
/// module is private; the shape of `glyph_rows`' return type pins them.
const GLYPH_W: usize = 5;
const GLYPH_H: usize = 7;

/// Render scale for effect text (each font pixel becomes `SCALE`×`SCALE`).
const SCALE: f32 = 2.0;
/// Screen-edge margin, in pixels.
const MARGIN: f32 = 6.0;
/// Side length of the square colour "icon" swatch, in pixels.
const ICON: f32 = 20.0;
/// Padding between the swatch and the text column, in pixels.
const PAD: f32 = 4.0;
/// Vertical gap between stacked chips, in pixels.
const GAP: f32 = 4.0;

/// Per-glyph horizontal advance in pixels at [`SCALE`] (matches the HUD's
/// fixed-advance layout: cell width plus one spacing column).
fn advance() -> f32 {
    (GLYPH_W as f32 + 1.0) * SCALE
}

/// Pixel width of `s` in the fixed-advance font at [`SCALE`].
fn text_px(s: &str) -> f32 {
    s.chars().count() as f32 * advance()
}

/// A single ready-to-draw status-effect chip.
#[derive(Debug, Clone, PartialEq)]
pub struct EffectChip {
    /// Display label, e.g. `"SPEED II"` (level suffix omitted at level I).
    pub label: String,
    /// Remaining time as `"M:SS"`, or empty for an infinite effect.
    pub time: String,
    /// Swatch tint (RGB in `0..=1`), deterministic per effect id.
    pub tint: [f32; 3],
    /// Whether the effect is ambient (beacon/aura): drawn fainter.
    pub ambient: bool,
}

/// Fold the active effects into drawable chips, preserving the model's stable
/// insertion order. Effects that ask not to show a HUD icon (`show_icon =
/// false`) are omitted, matching vanilla.
#[must_use]
pub fn chips_from(fx: &ActiveEffects) -> Vec<EffectChip> {
    fx.iter()
        .filter(|e| e.show_icon)
        .map(|e| EffectChip {
            label: effect_label(e.id.path(), e.level()),
            time: time_string(e.duration_ticks),
            tint: tint_for(e.id.path()),
            ambient: e.ambient,
        })
        .collect()
}

/// Build the display label: the effect path with `_` turned to spaces and a
/// roman-numeral level suffix for level ≥ 2 (vanilla hides the `I`). The bitmap
/// font is upper-case only and up-cases internally, so case here is cosmetic.
fn effect_label(path: &str, level: u32) -> String {
    let name = path.replace('_', " ");
    if level >= 2 {
        format!("{name} {}", roman(level))
    } else {
        name
    }
}

/// Roman numeral for `n` (1..=3999); falls back to the decimal string outside
/// that range so an absurd amplifier still renders *something* legible rather
/// than an empty or wrong glyph run.
fn roman(mut n: u32) -> String {
    if n == 0 || n > 3999 {
        return n.to_string();
    }
    const TABLE: &[(u32, &str)] = &[
        (1000, "M"),
        (900, "CM"),
        (500, "D"),
        (400, "CD"),
        (100, "C"),
        (90, "XC"),
        (50, "L"),
        (40, "XL"),
        (10, "X"),
        (9, "IX"),
        (5, "V"),
        (4, "IV"),
        (1, "I"),
    ];
    let mut out = String::new();
    for &(v, s) in TABLE {
        while n >= v {
            out.push_str(s);
            n -= v;
        }
    }
    out
}

/// Format remaining ticks as `"M:SS"`, rounding up so a nearly-expired effect
/// still reads at least `0:01`. An infinite effect (`duration < 0`) has no
/// timer and yields the empty string.
fn time_string(duration_ticks: i32) -> String {
    if duration_ticks < 0 {
        return String::new();
    }
    // Round up: 1..=20 ticks → 1 s, so the timer never shows 0:00 while active.
    let secs = (duration_ticks + 19) / 20;
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// A deterministic, bright RGB tint per effect id. This is a *rendering* choice
/// (distinguish effects at a glance), not registry knowledge, so it is derived
/// from the id rather than a hand-maintained beneficial/harmful table. Distinct
/// ids get distinct tints; the same id is stable across frames and runs (a
/// fixed-key hasher, never `RandomState`).
/// `pub(crate)` since issue #613's beacon screen (`container::beacon`) reuses
/// this same hash-derived swatch colour for its power buttons — the
/// identical "no real sprite exists, so tint a flat quad" simplification
/// this HUD chip already established.
pub(crate) fn tint_for(path: &str) -> [f32; 3] {
    use std::hash::{Hash, Hasher};
    let mut h = std::hash::DefaultHasher::new();
    path.hash(&mut h);
    let v = h.finish();
    // Spread three bytes across a 0.35..=1.0 range so every channel stays bright
    // enough to read over the world, and no effect renders near-black.
    let chan = |shift: u32| 0.35 + ((v >> shift) & 0xff) as f32 / 255.0 * 0.65;
    [chan(0), chan(8), chan(16)]
}

/// Emit the top-right effect stack as coloured triangles in NDC (the same
/// `[x, y, r, g, b, a]` vertex layout the HUD pipeline consumes). An empty
/// slice emits nothing, so an effect-free player costs zero vertices.
#[must_use]
pub fn geometry(chips: &[EffectChip], width: f32, height: f32) -> Vec<f32> {
    let mut b = Quads::new(width, height);
    let chip_h = ICON.max(2.0 * GLYPH_H as f32 * SCALE + 2.0);
    for (i, chip) in chips.iter().enumerate() {
        let text_w = text_px(&chip.label).max(text_px(&chip.time));
        let chip_w = ICON + PAD + text_w;
        let x = width - MARGIN - chip_w;
        let y = MARGIN + i as f32 * (chip_h + GAP);

        let alpha = if chip.ambient { 0.55 } else { 0.9 };
        // Faint chip backdrop so text stays legible over bright terrain.
        b.rect(
            x - PAD,
            y - 2.0,
            chip_w + PAD * 2.0,
            chip_h + 4.0,
            [0.0, 0.0, 0.0, 0.45],
        );
        // Colour swatch ("icon" stand-in), left-aligned and vertically centred.
        let icon_y = y + (chip_h - ICON) * 0.5;
        let t = chip.tint;
        b.rect(x, icon_y, ICON, ICON, [t[0], t[1], t[2], alpha]);

        let tx = x + ICON + PAD;
        b.text(&chip.label, tx, y, [0.95, 0.95, 0.95, 1.0]);
        if !chip.time.is_empty() {
            let ty = y + GLYPH_H as f32 * SCALE + 1.0;
            b.text(&chip.time, tx, ty, [0.75, 0.78, 0.72, 1.0]);
        }
    }
    b.verts
}

/// A minimal pixel-space quad emitter to NDC, mirroring the HUD's builder but
/// self-contained (this module owns no dependency on the HUD's private types).
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

    /// Emit a pixel-space rectangle as two triangles in NDC.
    fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, c: [f32; 4]) {
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

    /// Emit a string at pixel `(x, y)` (top-left of the first glyph), one
    /// `SCALE`×`SCALE` quad per lit font pixel via the HUD's bitmap font.
    fn text(&mut self, s: &str, x: f32, y: f32, c: [f32; 4]) {
        let mut cursor = x;
        for ch in s.chars() {
            if ch != ' ' {
                let rows = glyph_rows(ch);
                for (ry, row) in rows.iter().enumerate() {
                    for rx in 0..GLYPH_W {
                        if (row >> (GLYPH_W - 1 - rx)) & 1 == 1 {
                            self.rect(
                                cursor + rx as f32 * SCALE,
                                y + ry as f32 * SCALE,
                                SCALE,
                                SCALE,
                                c,
                            );
                        }
                    }
                }
            }
            cursor += advance();
        }
    }
}

/// Number of `f32`s per vertex (`[x, y, r, g, b, a]`).
const FLOATS_PER_VERTEX: usize = 6;

/// GPU renderer for the status-effect overlay: a coloured-quad pipeline (same
/// trivial shader as the HUD's) plus a growable dynamic vertex buffer, drawn in
/// a `Load` pass so it composites over whatever is already on the frame.
#[derive(Debug)]
pub struct EffectsRenderer {
    pipeline: wgpu::RenderPipeline,
    buffer: wgpu::Buffer,
    capacity_floats: usize,
}

impl EffectsRenderer {
    /// Build the overlay pipeline for a target of `color_format`.
    #[must_use]
    pub fn new(device: &wgpu::Device, color_format: wgpu::TextureFormat) -> Self {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("effects-shader"),
            source: wgpu::ShaderSource::Wgsl(EFFECTS_WGSL.into()),
        });
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("effects-layout"),
            bind_group_layouts: &[],
            immediate_size: 0,
        });
        let pipeline = device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
            label: Some("effects-pipeline"),
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

        let capacity_floats = 2048;
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("effects-verts"),
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

    /// Draw the active-effects overlay over the current frame contents. Costs
    /// one buffer write and one draw; a no-op when no effect shows an icon.
    pub fn render(
        &mut self,
        device: &wgpu::Device,
        queue: &wgpu::Queue,
        view: &wgpu::TextureView,
        fx: &ActiveEffects,
        width: u32,
        height: u32,
    ) {
        let chips = chips_from(fx);
        let verts = geometry(&chips, width as f32, height as f32);
        if verts.is_empty() {
            return;
        }
        if verts.len() > self.capacity_floats {
            self.capacity_floats = verts.len().next_power_of_two();
            self.buffer = device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("effects-verts"),
                size: (self.capacity_floats * 4) as wgpu::BufferAddress,
                usage: wgpu::BufferUsages::VERTEX | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
        }
        queue.write_buffer(&self.buffer, 0, bytemuck::cast_slice(&verts));

        let vertex_count = (verts.len() / FLOATS_PER_VERTEX) as u32;
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("effects"),
        });
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("effects-pass"),
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

const EFFECTS_WGSL: &str = include_str!("shaders/effects.wgsl");

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::effect::StatusEffect;
    use lodestone_model::Identifier;

    fn id(path: &str) -> Identifier {
        Identifier::new("minecraft", path).unwrap()
    }

    fn effect(path: &str, amp: u8, dur: i32) -> StatusEffect {
        StatusEffect::new(id(path), amp, dur)
    }

    #[test]
    fn roman_covers_the_common_potion_levels() {
        assert_eq!(roman(1), "I");
        assert_eq!(roman(2), "II");
        assert_eq!(roman(4), "IV");
        assert_eq!(roman(5), "V");
        assert_eq!(roman(9), "IX");
        assert_eq!(roman(10), "X");
        // Out of the classic range it must still render legibly, not blank.
        assert_eq!(roman(5000), "5000");
    }

    #[test]
    fn time_string_rounds_up_and_marks_infinite() {
        assert_eq!(time_string(1800), "1:30"); // 90 s
        assert_eq!(time_string(200), "0:10");
        assert_eq!(
            time_string(1),
            "0:01",
            "a nearly-expired effect never shows 0:00"
        );
        assert_eq!(time_string(-1), "", "an infinite effect has no timer");
    }

    #[test]
    fn chips_fold_name_level_and_time_in_order() {
        let mut fx = ActiveEffects::new();
        fx.apply(effect("speed", 1, 1800)); // Speed II, 1:30
        fx.apply(effect("haste", 0, 200)); // Haste (level I → no suffix), 0:10
        let chips = chips_from(&fx);
        assert_eq!(chips.len(), 2);
        assert_eq!(chips[0].label, "speed II");
        assert_eq!(chips[0].time, "1:30");
        assert_eq!(chips[1].label, "haste", "level I hides the roman suffix");
        assert_eq!(chips[1].time, "0:10");
    }

    #[test]
    fn chips_omit_hidden_icons_and_mark_ambient() {
        let mut fx = ActiveEffects::new();
        fx.apply(StatusEffect {
            id: id("night_vision"),
            amplifier: 0,
            duration_ticks: -1,
            ambient: true,
            show_particles: true,
            show_icon: true,
        });
        fx.apply(StatusEffect {
            id: id("hidden"),
            amplifier: 0,
            duration_ticks: 100,
            ambient: false,
            show_particles: true,
            show_icon: false, // must not produce a chip
        });
        let chips = chips_from(&fx);
        assert_eq!(chips.len(), 1, "the show_icon=false effect is dropped");
        assert_eq!(chips[0].label, "night vision", "underscores become spaces");
        assert_eq!(chips[0].time, "", "infinite effect: no timer");
        assert!(chips[0].ambient);
    }

    #[test]
    fn distinct_effects_tint_differently_and_stably() {
        let a = tint_for("speed");
        let b = tint_for("poison");
        assert_ne!(a, b, "different effects must be visually distinguishable");
        assert_eq!(a, tint_for("speed"), "tint must be stable for a given id");
        for ch in a {
            assert!((0.35..=1.0).contains(&ch), "channels stay bright: {ch}");
        }
    }

    /// Rasterise the emitted quads onto a pixel grid and count coverage inside
    /// the widget's top-right rect. This is the anti-vacuity control: an empty
    /// effect set must light **zero** pixels there, while two effects must light
    /// a substantial run of glyph + swatch pixels. A no-op fold, an off-screen
    /// layout, or an empty geometry path fails one side.
    #[test]
    fn geometry_covers_the_top_right_rect_only_when_effects_are_present() {
        let (w, h) = (320.0_f32, 240.0_f32);
        // Widget lives in the top-right corner.
        let (rx0, ry0, rx1, ry1) = (w * 0.5, 0.0, w, h * 0.5);

        let lit_in_rect = |verts: &[f32]| -> usize {
            let mut grid = vec![false; (w as usize) * (h as usize)];
            for quad in verts.chunks_exact(FLOATS_PER_VERTEX * 6) {
                // Recover the pixel AABB of the axis-aligned quad from its verts.
                let (mut nx0, mut ny0, mut nx1, mut ny1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
                for v in quad.chunks_exact(FLOATS_PER_VERTEX) {
                    nx0 = nx0.min(v[0]);
                    nx1 = nx1.max(v[0]);
                    ny0 = ny0.min(v[1]);
                    ny1 = ny1.max(v[1]);
                }
                let px0 = ((nx0 + 1.0) * 0.5 * w).round() as i32;
                let px1 = ((nx1 + 1.0) * 0.5 * w).round() as i32;
                // NDC y is up; pixel y is down.
                let py0 = ((1.0 - ny1) * 0.5 * h).round() as i32;
                let py1 = ((1.0 - ny0) * 0.5 * h).round() as i32;
                for py in py0..py1 {
                    for px in px0..px1 {
                        if px >= 0 && px < w as i32 && py >= 0 && py < h as i32 {
                            grid[py as usize * w as usize + px as usize] = true;
                        }
                    }
                }
            }
            let mut n = 0;
            for py in 0..h as usize {
                for px in 0..w as usize {
                    if grid[py * w as usize + px] {
                        let (fx, fy) = (px as f32, py as f32);
                        if fx >= rx0 && fx < rx1 && fy >= ry0 && fy < ry1 {
                            n += 1;
                        }
                    }
                }
            }
            n
        };

        let empty = geometry(&[], w, h);
        assert_eq!(
            lit_in_rect(&empty),
            0,
            "an empty effect set must not paint the widget rect"
        );

        let mut fx = ActiveEffects::new();
        fx.apply(effect("speed", 1, 1800));
        fx.apply(effect("strength", 0, 600));
        let chips = chips_from(&fx);
        let full = geometry(&chips, w, h);
        let lit = lit_in_rect(&full);
        assert!(
            lit > 300,
            "two effects must light the swatches + name/timer glyphs in the top-right \
             rect, only {lit} px covered — the fold or geometry path may be a no-op"
        );
    }

    /// Headless GPU proof that the real pipeline draws: render the overlay to an
    /// offscreen target and read pixels back, asserting an empty set stays
    /// background and a populated set lights the top-right. Mirrors the HUD's
    /// house style — opted in with `--ignored`, and a **failure** (not a skip)
    /// when no adapter is present, so a green run is never vacuous.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn overlay_rasterises_to_pixels() {
        use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};

        let ctx = GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in via --ignored but no wgpu adapter is available; \
             run on a host with a GPU (or a software adapter such as \
             LIBGL_ALWAYS_SOFTWARE=1 / WGPU_BACKEND=gl), don't 'skip' — a silent pass here \
             would assert nothing",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (320u32, 240u32);
        let clear = wgpu::Color {
            r: 0.04,
            g: 0.04,
            b: 0.08,
            a: 1.0,
        };
        let bg = [10i32, 10, 20];

        // Render one frame and return the RGBA pixel buffer so callers can count
        // coverage in specific screen rects (not just a whole-frame total).
        let render_frame = |fx: &ActiveEffects| -> Vec<u8> {
            let mut target = HeadlessTarget::new(device, w, h, format);
            let frame = target.acquire().expect("headless acquire");
            {
                let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
                    label: Some("clear"),
                });
                enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("effects-clear"),
                    color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                        view: frame.view(),
                        depth_slice: None,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: wgpu::LoadOp::Clear(clear),
                            store: wgpu::StoreOp::Store,
                        },
                    })],
                    depth_stencil_attachment: None,
                    timestamp_writes: None,
                    occlusion_query_set: None,
                    multiview_mask: None,
                });
                queue.submit(std::iter::once(enc.finish()));
            }
            let mut renderer = EffectsRenderer::new(device, format);
            renderer.render(device, queue, frame.view(), fx, w, h);
            target.read_texels(device, queue)
        };

        // Count non-background pixels inside a screen-space rect [x0,x1)×[y0,y1).
        let lit_in = |pixels: &[u8], x0: u32, y0: u32, x1: u32, y1: u32| -> usize {
            let mut n = 0;
            for py in y0..y1 {
                for px in x0..x1 {
                    let i = ((py * w + px) * 4) as usize;
                    let d = (i32::from(pixels[i]) - bg[0]).abs()
                        + (i32::from(pixels[i + 1]) - bg[1]).abs()
                        + (i32::from(pixels[i + 2]) - bg[2]).abs();
                    if d > 40 {
                        n += 1;
                    }
                }
            }
            n
        };

        // The widget lives in the top-right quadrant; the bottom-left quadrant is
        // the localization control — it must stay background even when populated.
        let (mx, my) = (w / 2, h / 2);
        let widget = |p: &[u8]| lit_in(p, mx, 0, w, my);
        let corner = |p: &[u8]| lit_in(p, 0, my, mx, h);
        let whole = |p: &[u8]| lit_in(p, 0, 0, w, h);

        let empty = render_frame(&ActiveEffects::new());
        let mut fx = ActiveEffects::new();
        fx.apply(effect("speed", 1, 1800));
        fx.apply(effect("strength", 0, 600));
        let full = render_frame(&fx);

        let empty_lit = whole(&empty);
        let widget_lit = widget(&full);
        let corner_lit = corner(&full);

        eprintln!("=== effects overlay rasterisation ===");
        eprintln!("empty overlay lit px (whole frame) = {empty_lit}");
        eprintln!("two-effect overlay lit px (top-right widget rect) = {widget_lit}");
        eprintln!("two-effect overlay lit px (bottom-left corner control) = {corner_lit}");

        // Empty state → nothing drawn anywhere: catches a state-independent fill.
        assert!(
            empty_lit < 20,
            "an empty effect set should read as background, but {empty_lit} px were lit"
        );
        // Populated → substantial coverage *inside the widget rect*: proves the
        // fold + geometry + pipeline path actually paints swatches and glyphs.
        assert!(
            widget_lit > 300,
            "two effects should rasterise swatches + glyphs in the top-right rect, only \
             {widget_lit} lit — the pipeline or geometry path may be a no-op"
        );
        // Populated → the opposite corner stays background: a blanket-fill or
        // clear-colour bug would light this and fail here even though it lit the
        // widget rect. This is the load-bearing control (cf. entities' corner=0).
        assert_eq!(
            corner_lit, 0,
            "the effects overlay must stay in the top-right; {corner_lit} px leaked into the \
             bottom-left corner — a blanket fill would pass the widget check but fail here"
        );
    }
}
