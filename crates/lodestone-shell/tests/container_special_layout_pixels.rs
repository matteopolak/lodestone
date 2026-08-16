//! Pixel gate: the anvil, grindstone, smithing table and enchanting table
//! draw their **own real background art**, not the
//! generic-chest panel every one of them silently fell back to before
//! `Menu::special_layout` existed.
//!
//! Modelled on `container_background_pixels.rs`'s own "Claim 1" (real art vs.
//! the flat/wrong fallback, measured by mean channel diff over a strip clear
//! of every slot well) — the pattern this repo trusts for "the real art
//! draws" claims. The **negative control that must fail the same assertion**
//! is a same-`container_size` `Menu::generic`: if `background_kind`'s
//! `special_layout` check were ever removed or misrouted, every one of these
//! four menus would render identically to its own control and the positive
//! assertions below would fail loudly rather than silently passing for the
//! wrong reason.
//!
//! ```text
//! cargo test -p lodestone-shell --test container_special_layout_pixels -- --ignored --nocapture
//! ```

use std::sync::Arc;

use lodestone::config::{AUTO_GUI_SCALE, calculate_gui_scale};
use lodestone::container::{ContainerBackground, ContainerFrame, ContainerRenderer, panel_origin, slot_layout};
use lodestone::resources::load_container_background;
use lodestone_game::menu::{Menu, SpecialLayout};
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};

/// Chosen so `calculate_gui_scale(AUTO, W, H) == 1`, matching every sibling
/// pixel gate in this crate.
const W: u32 = 480;
const H: u32 = 320;

fn clear_view(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) {
    let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("gate-clear"),
    });
    {
        let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("gate-clear-pass"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view,
                depth_slice: None,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
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

/// Mean absolute per-channel difference between `rect_a` of `a` and `rect_b`
/// of `b` — same size, potentially different position in each buffer (the
/// two menus under comparison do not always share a panel origin).
fn mean_abs_diff_two_rects(a: &[u8], rect_a: [u32; 4], b: &[u8], rect_b: [u32; 4]) -> f32 {
    let [ax, ay, rw, rh] = rect_a;
    let [bx, by, bw, bh] = rect_b;
    assert_eq!((rw, rh), (bw, bh), "rects being compared must be the same size");
    let mut sum = 0.0;
    let mut n = 0.0;
    for dy in 0..rh {
        for dx in 0..rw {
            let ia = (((ay + dy) * W + (ax + dx)) * 4) as usize;
            let ib = (((by + dy) * W + (bx + dx)) * 4) as usize;
            sum += (i32::from(a[ia]) - i32::from(b[ib])).unsigned_abs() as f32
                + (i32::from(a[ia + 1]) - i32::from(b[ib + 1])).unsigned_abs() as f32
                + (i32::from(a[ia + 2]) - i32::from(b[ib + 2])).unsigned_abs() as f32;
            n += 3.0;
        }
    }
    sum / n
}

/// Converts a local widget-space offset into the physical pixel this gate
/// reads back — `container_background_pixels.rs`'s own `panel_point`.
fn panel_point(menu: &Menu, lx: f32, ly: f32) -> (u32, u32) {
    let layout = slot_layout(menu);
    let (px, py) = panel_origin(&layout, W, H);
    let scale = calculate_gui_scale(AUTO_GUI_SCALE, W, H).max(1) as f32;
    (((px + lx) * scale) as u32, ((py + ly) * scale) as u32)
}

fn render(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &mut HeadlessTarget,
    background: &Arc<ContainerBackground>,
    menu: &Menu,
) -> Vec<u8> {
    let frame = ContainerFrame::new(Some(menu), "Title");
    let mut renderer = ContainerRenderer::new(device, wgpu::TextureFormat::Rgba8Unorm);
    renderer.attach_background(device, queue, wgpu::TextureFormat::Rgba8Unorm, Arc::clone(background));
    let acquired = target.acquire().expect("headless acquire");
    clear_view(device, queue, acquired.view());
    renderer.render(device, queue, acquired.view(), &frame, W, H);
    target.read_texels(device, queue)
}

/// One `(menu, a point clear of every slot well)` case per `special_layout`
/// screen. The point is picked from each sheet's own top strip — vanilla's
/// real art is never a flat colour there, unlike the generic chest's fixed
/// grey fill, so a real vs. fallback diff at this point is meaningful for all
/// four.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_four_special_layout_screens_draw_their_own_real_background() {
    assert_eq!(
        calculate_gui_scale(AUTO_GUI_SCALE, W, H),
        1,
        "the panel math below assumes W x H divides to itself under the GUI scale"
    );

    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let background = load_container_background().expect(
        "GPU gate opted in but the vanilla container background did not load; set \
         LODESTONE_ASSETS to a pack root with client.jar",
    );
    let mut target = HeadlessTarget::new(device, W, H, wgpu::TextureFormat::Rgba8Unorm);

    // A band across the panel's top strip, `y` in `[9, 18)`: clear of every
    // real layout's nearest slot (`grindstone`'s is closest, at `y = 19`) and
    // of the generic layout's own first slot row (`y = 18`).
    //
    // `y in [0, 16)` was tried first (and briefly a single corner pixel
    // before that) and both are a coincident-input bug, not a measurement of
    // this feature: vanilla's own `container/*.png` sheets share one flat
    // window-border colour across their entire top edge, independent of
    // which screen they belong to — a real `anvil.png` vs. `generic_54.png`
    // row-by-row diff (extracted from `client.jar` and compared with `PIL`,
    // outside this renderer) is ~0 for every row `y < 15` and only exceeds
    // 60/255 once `y >= 16`, where the anvil-specific icon and slot-well art
    // begins. So `[0, 16)` sampled almost nothing but the shared border on
    // every case, and the `anvil` case's failure was the gate finding its
    // own blind spot, not a regression in `background_kind` (still
    // `BackgroundKind::Anvil`, still `whole_panel(&self.anvil)` — read
    // straight off `crate::container::background::quads`). `[9, 18)` sits
    // below the shared border and above the nearest real slot on every
    // case; measured (same PNG-diff method) at 18.4–31.8 across all four
    // real sheets vs. `generic_54.png`, comfortably clear of the `8.0`
    // threshold below with margin to spare, while still ending strictly
    // before `y = 18`/`19` so no slot-well pixel enters the sample.
    let strip = (176u32, 9u32);
    let strip_y0 = 9.0f32;

    let cases: [(&str, Menu); 4] = [
        ("anvil", Menu::item_combiner(3, 2, SpecialLayout::Anvil)),
        (
            "grindstone",
            Menu::item_combiner(3, 2, SpecialLayout::Grindstone),
        ),
        (
            "smithing",
            Menu::item_combiner(4, 3, SpecialLayout::Smithing),
        ),
        ("enchanting_table", Menu::enchanting_table()),
    ];

    eprintln!("=== special-layout background pixel gate ===");
    for (name, menu) in &cases {
        assert!(
            menu.special_layout().is_some(),
            "{name}: test setup bug — this menu must carry a special_layout"
        );
        let container_size = match menu.kind() {
            lodestone_game::menu::MenuKind::Generic { container_size } => container_size,
            lodestone_game::menu::MenuKind::Player => unreachable!(),
        };
        let control = Menu::generic(container_size);

        let subject_pixels = render(device, queue, &mut target, &background, menu);
        let control_pixels = render(device, queue, &mut target, &background, &control);

        // The subject and control panels are **not** necessarily the same
        // size or origin: `special_layout_positions` fixes `main_y = 84`
        // (vanilla's real panel is `176x166`), while `generic_layout`'s panel
        // height depends on `container_size` (a 3-slot generic container is
        // only one row tall, `176x132`) — so each is read at its own
        // `panel_point`, not a shared rect.
        let (px, py) = panel_point(menu, 0.0, strip_y0);
        let (cpx, cpy) = panel_point(&control, 0.0, strip_y0);
        let diff = mean_abs_diff_two_rects(
            &subject_pixels,
            [px, py, strip.0, strip.1],
            &control_pixels,
            [cpx, cpy, strip.0, strip.1],
        );
        eprintln!("{name}: mean abs diff vs same-size generic control = {diff:.1}");
        assert!(
            diff > 8.0,
            "{name} must draw its own real `container/*.png` art, visibly \
             different from a same-sized plain generic container's chest \
             panel at the same point — got a mean channel diff of only {diff:.1}. \
             If this is 0, `background_kind` fell back to the generic case and \
             the special_layout art is an island."
        );
    }
}
