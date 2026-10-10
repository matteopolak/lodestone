use super::*;
use lodestone_assets::ResourceLocation;
use super::tab_panel::TAB_MAX_ROWS_PER_COL;

mod debug;
mod hotbar_crosshair;
mod chat;
mod vitals;
mod tab_sidebar;
mod bars;
mod pixels;

/// Decodes a colour-stream vertex buffer's NDC positions back to the pixel
/// space `ColourStream::rect` built them from, returning `(min_x, max_x,
/// min_y, max_y)`. The exact inverse of `to_ndc` in
/// `hud/item_icon.rs`'s `ColourStream::rect`.
fn ndc_bounds(verts: &[f32], w: f32, h: f32) -> (f32, f32, f32, f32) {
    let (mut min_x, mut max_x, mut min_y, mut max_y) =
        (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
    for v in verts.chunks_exact(FLOATS_PER_VERTEX) {
        let px = (v[0] + 1.0) * 0.5 * w;
        let py = (1.0 - v[1]) * 0.5 * h;
        min_x = min_x.min(px);
        max_x = max_x.max(px);
        min_y = min_y.min(py);
        max_y = max_y.max(py);
    }
    (min_x, max_x, min_y, max_y)
}

/// Twelve candidates named so their widths are equal, so the layout
/// arithmetic below is not also measuring a proportional font.
fn popup_candidates(n: usize) -> Vec<crate::chat::Candidate> {
    (0..n)
        .map(|i| crate::chat::Candidate {
            text: format!("cand{i:02}"),
            tooltip: None,
        })
        .collect()
}

/// A view of `n` players called `P0..P{n-1}`, all survival, all full bars.
fn tab_view(n: usize) -> crate::tablist::TabListView {
    crate::tablist::TabListView {
        rows: (0..n)
            .map(|i| crate::tablist::TabListRow {
                name: crate::overlay::plain_spans(format!("P{i}")),
                ping_sprite: "icon/ping_5",
                spectator: false,
            })
            .collect(),
        header: Vec::new(),
        footer: Vec::new(),
    }
}

/// Encode a solid-colour RGBA PNG so a `MemorySource` can stand in for a
/// real jar in a hermetic test — the same trick
/// `lodestone_render::gui_atlas`'s own tests use (no GPU, no disk).
fn solid_png(w: u32, h: u32, rgba: [u8; 4]) -> Vec<u8> {
    let mut data = Vec::new();
    {
        let mut encoder = png::Encoder::new(&mut data, w, h);
        encoder.set_color(png::ColorType::Rgba);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("png header");
        let pixels: Vec<u8> = (0..(w * h)).flat_map(|_| rgba).collect();
        writer.write_image_data(&pixels).expect("png data");
    }
    data
}

/// A minimal synthetic pack covering the boss-bar sprite ids exercised
/// below: two colours' background/progress plates plus one notch-overlay
/// pair, at vanilla's real 182×5 native size
/// (`.cache/mc/26.2/client-src/assets/.../gui/sprites/boss_bar/*.png`).
/// Content is an arbitrary flat colour per id — this is a **geometry**
/// gate (does the draw reach the atlas and land the right rect?), not a
/// pixel-colour gate, so what matters is that each id is *present* and
/// distinct, not what it looks like.
fn boss_bar_synthetic_atlas() -> GuiAtlas {
    let mut src = lodestone_assets::MemorySource::new("boss-bar-test");
    for (id, rgba) in [
        ("boss_bar/purple_background", [60, 20, 90, 255]),
        ("boss_bar/purple_progress", [170, 60, 220, 255]),
        ("boss_bar/red_background", [90, 20, 20, 255]),
        ("boss_bar/red_progress", [220, 40, 40, 255]),
        ("boss_bar/notched_6_background", [10, 10, 10, 255]),
        ("boss_bar/notched_6_progress", [250, 250, 250, 255]),
    ] {
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png"),
            solid_png(182, 5, rgba),
        );
    }
    let manager = lodestone_assets::ResourceManager::new(vec![
        Box::new(src) as Box<dyn lodestone_assets::ResourceSource>
    ]);
    GuiAtlas::build(&manager).expect("synthetic boss-bar atlas must build")
}

/// The bounding box (logical pixels) of each 6-vertex (two-triangle)
/// quad in `verts`, in emission order. `push_sprite_quad` always emits
/// exactly one quad per call, so grouping by six vertices recovers the
/// draw's own call order — one entry per `b.sprite`/`b.push_sprite_quad`
/// invocation.
fn quad_boxes(verts: &[f32], cw: f32, ch: f32) -> Vec<(f32, f32, f32, f32)> {
    let px = |x: f32| (x + 1.0) * 0.5 * cw;
    let py = |y: f32| (1.0 - y) * 0.5 * ch;
    verts
        .chunks(SPRITE_FLOATS_PER_VERTEX * 6)
        .map(|quad| {
            let mut x0 = f32::MAX;
            let mut y0 = f32::MAX;
            let mut x1 = f32::MIN;
            let mut y1 = f32::MIN;
            for v in quad.chunks(SPRITE_FLOATS_PER_VERTEX) {
                let (x, y) = (px(v[0]), py(v[1]));
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
            (x0, y0, x1, y1)
        })
        .collect()
}

/// A minimal synthetic pack covering the locator bar's two sprite ids —
/// same "geometry gate, not a pixel-colour gate" shape as
/// [`boss_bar_synthetic_atlas`], and at the bar's real native sizes:
/// 182x5 for the background, 9x9 for the dot (the locator bar's dot size).
fn locator_bar_synthetic_atlas() -> GuiAtlas {
    let mut src = lodestone_assets::MemorySource::new("locator-bar-test");
    for (id, size, rgba) in [
        ("hud/locator_bar_background", (182, 5), [40, 40, 40, 255]),
        (locator::DEFAULT_DOT_SPRITE, (9, 9), [255, 255, 255, 255]),
        // The XP bar's own sprites, needed only for this test's
        // mutual-exclusion control (`xp_only` below) — `b.sprite`
        // silently draws nothing for a missing id, so without these the
        // control would "pass" by drawing nothing for the wrong reason.
        ("hud/experience_bar_background", (182, 5), [20, 90, 20, 255]),
        ("hud/experience_bar_progress", (182, 5), [40, 200, 40, 255]),
    ] {
        src.insert(
            format!("assets/minecraft/textures/gui/sprites/{id}.png"),
            solid_png(size.0, size.1, rgba),
        );
    }
    let manager = lodestone_assets::ResourceManager::new(vec![
        Box::new(src) as Box<dyn lodestone_assets::ResourceSource>
    ]);
    GuiAtlas::build(&manager).expect("synthetic locator-bar atlas must build")
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

/// Covered sample cells and their bounding box inside `rect`, an
/// `(x0, y0, x1, y1)` NDC box — a real CPU rasteriser over the actual
/// triangle geometry, not a vertex-sample count. Shared by every rasterised
/// coverage gate in this file (`recipe_toast_gate`, `chat_hover_tooltip_gate`)
/// rather than one copy per gate, so a fix to the rasteriser cannot land in
/// one gate and not the other. Returns the box because a bare fraction
/// cannot distinguish a uniform-but-wrong frame from a localised blob.
#[cfg(test)]
pub(super) fn coverage(
    verts: &[f32],
    rect: (f32, f32, f32, f32),
    res: usize,
) -> (usize, usize, Option<(f32, f32, f32, f32)>) {
    let (rx0, ry0, rx1, ry1) = rect;
    let to_ndc = |i: usize| -1.0 + 2.0 * (i as f32 + 0.5) / res as f32;
    let (mut covered, mut inside) = (0usize, 0usize);
    let mut bbox: Option<(f32, f32, f32, f32)> = None;
    for gy in 0..res {
        for gx in 0..res {
            let (px, py) = (to_ndc(gx), to_ndc(gy));
            if px < rx0 || px > rx1 || py < ry0 || py > ry1 {
                continue;
            }
            inside += 1;
            let mut hit = false;
            for tri in verts.chunks_exact(FLOATS_PER_VERTEX * 3) {
                let (ax, ay) = (tri[0], tri[1]);
                let (bx, by) = (tri[FLOATS_PER_VERTEX], tri[FLOATS_PER_VERTEX + 1]);
                let (cx, cy) = (tri[FLOATS_PER_VERTEX * 2], tri[FLOATS_PER_VERTEX * 2 + 1]);
                let d = (bx - ax) * (cy - ay) - (cx - ax) * (by - ay);
                if d.abs() < f32::EPSILON {
                    continue;
                }
                let w0 = ((bx - px) * (cy - py) - (cx - px) * (by - py)) / d;
                let w1 = ((cx - px) * (ay - py) - (ax - px) * (cy - py)) / d;
                let w2 = 1.0 - w0 - w1;
                if w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0 {
                    hit = true;
                    break;
                }
            }
            if hit {
                covered += 1;
                bbox = Some(match bbox {
                    None => (px, py, px, py),
                    Some((x0, y0, x1, y1)) => (x0.min(px), y0.min(py), x1.max(px), y1.max(py)),
                });
            }
        }
    }
    (covered, inside, bbox)
}
