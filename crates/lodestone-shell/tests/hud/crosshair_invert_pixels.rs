//! Pixel gate: the crosshair is drawn as the inverse of what is behind it.
//!
//! The expected colour comes from a vanilla 26.3 screenshot, not from the
//! blend equation: over a horizon of `(177, 209, 255)` vanilla's crosshair
//! pixels are `(78, 46, 0)`. A second backdrop, where a plain white or
//! alpha-blended mark would land somewhere else entirely, is the control.
//!
//! ```text
//! cargo test -p lodestone-shell --test hud crosshair_invert -- --ignored --nocapture
//! ```

use lodestone::hud::{DebugStats, HudFrame, HudRenderer};
use lodestone_render::{HeadlessTarget, RenderTarget};

const W: u32 = 640;
const H: u32 = 480;

fn clear_view(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView, rgb: [u8; 3]) {
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: Some("crosshair-gate-clear") });
    {
        let _pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("crosshair-gate-clear-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color {
                        r: f64::from(rgb[0]) / 255.0,
                        g: f64::from(rgb[1]) / 255.0,
                        b: f64::from(rgb[2]) / 255.0,
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
    }
    queue.submit([enc.finish()]);
}

#[test]
#[ignore = "requires a GPU adapter"]
fn the_crosshair_inverts_the_pixels_behind_it() {
    let ctx = lodestone_render::GpuContext::new_headless_blocking()
        .expect("headless GPU gate opted in via --ignored but no wgpu adapter is available");
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut hud = HudRenderer::new(device, format);
    let stats = DebugStats::default();
    let frame = HudFrame { show_debug: false, crosshair: true, hotbar: None, hotbar_items: None, ..HudFrame::new(&stats) };

    for (backdrop, expected) in [([177, 209, 255], [78, 46, 0]), ([40, 120, 200], [215, 135, 55])] {
        let acquired = target.acquire().expect("headless acquire");
        clear_view(device, queue, acquired.view(), backdrop);
        hud.render(device, queue, acquired.view(), acquired.view(), &frame, W, H);
        let pixels = target.read_texels(device, queue);
        let (mut marked, mut wrong) = (0usize, Vec::new());
        let (mut x0, mut y0, mut x1, mut y1) = (u32::MAX, u32::MAX, 0, 0);
        for (i, px) in pixels.chunks_exact(4).enumerate() {
            let rgb = [px[0], px[1], px[2]];
            if rgb == backdrop {
                continue;
            }
            let (x, y) = (i as u32 % W, i as u32 / W);
            (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
            marked += 1;
            if rgb.iter().zip(expected).any(|(&got, want)| got.abs_diff(want) > 1) && wrong.len() < 4 {
                wrong.push((x, y, rgb));
            }
        }
        assert!(marked > 0, "nothing painted over {backdrop:?}");
        assert!(
            wrong.is_empty(),
            "over {backdrop:?} every crosshair pixel must be {expected:?}; got {wrong:?} \
             ({marked} px painted, bbox x{x0}..{x1} y{y0}..{y1})"
        );
    }
}
