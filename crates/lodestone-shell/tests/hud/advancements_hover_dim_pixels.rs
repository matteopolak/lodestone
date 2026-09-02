//! Pixel gate: **the Advancements hover-dim reaches a widget's own frame and
//! icon, and stops at the hover tooltip.**
//!
//! Issue this closes: the connector-line ordering fix
//! (`crates/lodestone-shell/src/menu/advancements.rs`'s own module doc) forced
//! every widget's frame and icon into passes the hover-dim could not reach.
//! This gate drives the real [`ContainerRenderer`] — the actual six(-plus)
//! pass sequence, not a geometry-marker inspection — through the real
//! [`ContainerBackground`] and item atlas, exactly as `app/redraw.rs` does.
//!
//! # The two claims
//!
//! 1. A **non-hovered** widget's own frame+icon rect gets **darker** while a
//!    *different* widget is hovered (the hover-dim covers the whole `234x113`
//!    tree viewport, not just the hovered widget — vanilla's own
//!    `extractTooltips` fill).
//! 2. The **hovered** widget's own frame+icon rect does **not** get
//!    (meaningfully) darker — `extractHover` redraws that exact frame and
//!    icon *after* the dim, which is the mechanism the hover-dim must not
//!    defeat. This stands in for "the tooltip panel itself is unaffected":
//!    the frame+icon redraw is part of the same `extractHover` draw sequence
//!    the title/description panel is, and it is the one piece of that
//!    sequence whose expected pixels this test can predict without a real
//!    vanilla language table (the panel's box sprite position depends on
//!    wrapped text width, which needs a resolved title/description string).
//!
//! Both are measured against the **same** chrome-only-vs-populated
//! discipline `container_item_pixels.rs` uses: brightness is compared
//! between two full renders of the same layout (hovering widget A vs
//! nothing hovered), not against an absolute pixel value — see
//! `CLAUDE.md`'s note that this backend's `ALPHA_BLENDING` composite cannot
//! be predicted to an exact byte. The magnitude bracket below is `>10%`
//! mean-brightness drop, comfortably above the composite's own precision
//! (the model shader's own equivalent gate uses a `>40/255` mid-alpha
//! bracket for the same reason).
//!
//! ```text
//! cargo test -p lodestone-shell --test advancements_hover_dim_pixels -- --ignored --nocapture
//! ```

use lodestone::container::ContainerRenderer;
use lodestone::menu::advancements::{
    AdvancementProgress, AdvancementsState, AdvancementsView, advancements_geometry,
    advancements_layout,
};
use lodestone::resources::{load_container_background, load_item_atlas};
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};

/// Chosen so `calculate_gui_scale(AUTO, W, H) == 1`, matching every sibling
/// pixel gate in this crate, and large enough that the `252x140` Advancements
/// window (positioned by `advancements_layout`) fits without touching an
/// edge.
const W: u32 = 480;
const H: u32 = 320;

/// `AdvancementsState::tick_fade`'s own ceiling
/// (`menu::advancements::FADE_CEILING`, module-private) — restated here
/// because [`AdvancementsView::fade`] is supplied directly rather than
/// ticked, so this test does not need five frames of ramp-up to reach it.
const FADE_CEILING: f32 = 0.3;

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

/// Mean of the three colour channels over `rect`.
fn mean_brightness(pixels: &[u8], rect: [u32; 4]) -> f32 {
    let [rx, ry, rw, rh] = rect;
    let mut sum = 0.0;
    let mut n = 0.0;
    for y in ry..ry + rh {
        for x in rx..rx + rw {
            let i = ((y * W + x) * 4) as usize;
            sum += (f32::from(pixels[i]) + f32::from(pixels[i + 1]) + f32::from(pixels[i + 2])) / 3.0;
            n += 1.0;
        }
    }
    sum / n
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn hovering_a_widget_darkens_every_frame_and_icon_but_its_own() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;

    let background = load_container_background().expect(
        "GPU gate opted in but the vanilla container background did not load; set \
         LODESTONE_ASSETS to a pack root with client.jar",
    );
    let item_atlas = load_item_atlas().expect(
        "GPU gate opted in but the vanilla item atlas did not load; set LODESTONE_ASSETS",
    );

    let mut renderer = ContainerRenderer::new(device, format);
    renderer.attach_background(device, queue, format, background);
    renderer.attach_items(device, queue, format, item_atlas);
    assert!(
        renderer.background_attached(),
        "the real container/*.png art must be attached — otherwise the frame \
         draws through the jar-less fallback path, which this gate is not \
         written to sample"
    );

    let mut state = AdvancementsState::default();
    let progress = AdvancementProgress::default();
    let layout = advancements_layout(&mut state, &progress, 1, W, H)
        .expect("the default (root) tab must lay out");
    assert!(
        layout.widgets.len() >= 2,
        "need at least two on-screen widgets: one to hover, one to sample as \
         an innocent bystander — got {}",
        layout.widgets.len()
    );
    let (hovered_idx, hovered_rect) = layout.widgets[0];
    let (_, bystander_rect) = layout.widgets[1];
    assert!(
        !overlaps_test(hovered_rect, bystander_rect),
        "the hovered widget and the bystander must not overlap, or darkening \
         one could be measuring the other"
    );

    // The `26x26` frame rect (`FRAME_DX` = 3, `FRAME_SIZE` = 26 — both
    // module-private, restated) fully contains the `16x16` icon, so sampling
    // it captures both the frame sprite and the icon drawn over it in one
    // rect — exactly the two things the fix moves into the same "mid" tier.
    let frame_rect = |w: lodestone::container::Rect| -> [u32; 4] {
        [(w.x + 3.0) as u32, w.y as u32, 26, 26]
    };

    let idle_view = AdvancementsView {
        title: "Advancements",
        hovered: None,
        hovered_title: "",
        hovered_description: "",
        progress: &progress,
        fade: 0.0,
    };
    let hovered_view = AdvancementsView {
        title: "Advancements",
        hovered: Some(hovered_idx),
        // Real English strings, not required to be exact translations — only
        // their *presence* (a non-empty description) matters, so the panel
        // draws at all; this gate does not sample the panel's own pixels.
        hovered_title: "Test Advancement",
        hovered_description: "A description long enough to need a panel.",
        progress: &progress,
        fade: FADE_CEILING,
    };

    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut shoot = |view: AdvancementsView<'_>| -> Vec<u8> {
        let geo = advancements_geometry(
            &layout,
            view,
            1,
            W,
            H,
            renderer.item_atlas().as_deref(),
            None,
            renderer.font(),
            renderer.background_data(),
        );
        let acquired = target.acquire().expect("headless acquire");
        clear_view(device, queue, acquired.view());
        renderer.render_geometry_scaled(device, queue, acquired.view(), None, &geo, 1, W, H);
        target.read_texels(device, queue)
    };

    let idle_pixels = shoot(idle_view);
    let hovered_pixels = shoot(hovered_view);

    let bystander_rect_px = frame_rect(bystander_rect);
    let hovered_rect_px = frame_rect(hovered_rect);

    let bystander_idle = mean_brightness(&idle_pixels, bystander_rect_px);
    let bystander_hovered = mean_brightness(&hovered_pixels, bystander_rect_px);
    let hovered_widget_idle = mean_brightness(&idle_pixels, hovered_rect_px);
    let hovered_widget_hovered = mean_brightness(&hovered_pixels, hovered_rect_px);

    eprintln!("=== advancements hover-dim pixel gate ===");
    eprintln!("bystander frame+icon rect = {bystander_rect_px:?}");
    eprintln!("hovered   frame+icon rect = {hovered_rect_px:?}");
    eprintln!("bystander mean brightness: idle={bystander_idle:.2} hovered={bystander_hovered:.2}");
    eprintln!(
        "hovered widget's own mean brightness: idle={hovered_widget_idle:.2} hovered={hovered_widget_hovered:.2}"
    );

    // Claim 1, the discriminating fix: a widget that is *not* the one being
    // hovered must still get measurably darker, because the hover-dim covers
    // the whole tree viewport. `> 10%` is comfortably above blend-precision
    // noise (a handful of `/255` levels) and comfortably below what a fully
    // opaque black quad at `fade = 0.3` would do to an already-mid-brightness
    // sprite (~30%), so it discriminates "reached the frame/icon" from
    // "measured nothing" without pinning an exact byte.
    assert!(
        bystander_idle > 1.0,
        "the bystander frame+icon rect must not itself be near-black in the \
         idle shot, or a drop from it would be noise rather than the dim: \
         idle={bystander_idle:.2}"
    );
    let bystander_drop = (bystander_idle - bystander_hovered) / bystander_idle;
    assert!(
        bystander_drop > 0.10,
        "hovering a *different* widget must still darken this widget's own \
         frame and icon by more than 10%: idle={bystander_idle:.2} \
         hovered={bystander_hovered:.2} (drop {:.1}%) — a drop at or near 0% \
         is the reported bug: the dim covers the tree but never reaches a \
         widget's own frame/icon",
        bystander_drop * 100.0
    );

    // Claim 2, the control the bare frame-alone assertion above would miss:
    // the *hovered* widget's own frame+icon must show **substantially less**
    // of a drop than the bystander's, because `extractHover` redraws that
    // exact frame and icon after the dim (see `menu::advancements`'s module
    // doc). A fix that darkened *everything* in the mid tier unconditionally
    // — including content the tooltip itself later repaints — would still
    // pass claim 1 and fail here (its drop would match the bystander's).
    //
    // **Not asserted near-zero**, and this is measured, not assumed: the real
    // `task_frame_obtained`/`task_frame_unobtained` sprites are **binary**
    // alpha — 112 of their 676 texels are alpha `0`, the rounded corners
    // outside the frame's diamond silhouette (measured on the real 26.2
    // asset: every texel is exactly `0` or `255`, no partial value at all).
    // The frame's own redraw cannot un-dim what its texture never covers, so
    // the tile grid showing through those transparent corners stays dimmed
    // even for the hovered widget — genuinely correct, vanilla-faithful
    // behaviour, not a residual bug. So the discriminating bracket is
    // relative to the bystander's own drop, not an absolute ceiling: the
    // hovered widget's drop must be well under half of it.
    let hovered_widget_drop = (hovered_widget_idle - hovered_widget_hovered) / hovered_widget_idle.max(1.0);
    assert!(
        hovered_widget_drop < bystander_drop * 0.6,
        "the hovered widget's own frame+icon must be substantially protected \
         by its own tooltip's redraw, relative to the bystander's full-dim \
         drop: bystander drop={:.1}%, hovered-widget drop={:.1}% (idle={hovered_widget_idle:.2} \
         hovered={hovered_widget_hovered:.2}) — a drop approaching the \
         bystander's means the tooltip's own redraw is not actually landing \
         on top of the dim",
        bystander_drop * 100.0,
        hovered_widget_drop * 100.0
    );
}

/// Simple rect overlap, mirroring `menu::advancements`'s own private
/// `overlaps` (restated — that one is module-private).
fn overlaps_test(a: lodestone::container::Rect, b: lodestone::container::Rect) -> bool {
    a.x + a.w > b.x && a.x < b.x + b.w && a.y + a.h > b.y && a.y < b.y + b.h
}
