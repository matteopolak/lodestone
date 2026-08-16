//! Pixel gate: **the container screen looks like vanilla, and the
//! hotbar dims behind it (that fix's leftover).**
//!
//! Two claims, two negative controls, per `CLAUDE.md`'s evidence standard
//! ("prove pixels changed... with a negative control that must fail the same
//! assertion").
//!
//! # Claim 1: the real `container/*.png` art draws, not the flat fill
//!
//! A point inside the panel but away from any slot well is measured with
//! [`ContainerBackground`] attached versus not. `container_screen.rs`'s own
//! coverage gate cannot see this difference — it only asks "was anything drawn
//! here", and the flat fill answers that just as well as the real texture does.
//!
//! # Claim 2: the hotbar actually dims behind an open container screen
//!
//! A real [`HudRenderer`] draws one bright hotbar item into the target first —
//! standing in for "the HUD, which draws unconditionally behind a
//! world-following screen". [`ContainerRenderer`] then draws on top,
//! **after** the HUD, exactly as `app.rs`'s per-frame draw now orders the two
//! passes. With a menu open, the container's full-canvas dim gradient must
//! darken that hotbar pixel. The negative control is the same two-pass sequence
//! with the container frame **closed** (`ContainerFrame::empty()`): nothing
//! should draw, and the hotbar pixel must be unchanged from the HUD-only shot —
//! proving the darkening above is caused by the open menu, not some other
//! artefact of drawing two passes into one target.
//!
//! ```text
//! cargo test -p lodestone-shell --test container_background_pixels -- --ignored --nocapture
//! ```

use lodestone::config::{AUTO_GUI_SCALE, calculate_gui_scale};
use lodestone::container::{ContainerFrame, ContainerRenderer, panel_origin, slot_layout};
use lodestone::hud::{DebugStats, HotbarSlot, HudFrame, HudRenderer};
use lodestone::resources::{load_container_background, load_item_atlas};
use lodestone_game::menu::Menu;
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};

/// Chosen so `calculate_gui_scale(AUTO, W, H) == 1` — the physical<->logical
/// canvas divide is a no-op, matching every sibling pixel gate in this crate
/// (`hotbar_block_item_pixels.rs`, `container_item_pixels.rs`).
const W: u32 = 480;
const H: u32 = 320;

/// The on-screen rect of hotbar cell `i`, mirroring
/// `hotbar_block_item_pixels.rs`'s own `cell_rect` (the procedural,
/// no-GUI-atlas branch of `hud.rs`'s private `draw_hotbar_items`, which this
/// test deliberately stays on by never attaching a HUD GUI atlas).
fn cell_rect(i: u32) -> [u32; 4] {
    let cx = W as f32 * 0.5;
    let cell = 22.0f32;
    let hx = cx - 9.0 * cell * 0.5;
    let hy = H as f32 - 6.0 - cell;
    let x = hx + 3.0 + i as f32 * cell;
    let y = hy + 3.0;
    [x as u32, y as u32, 16, 16]
}

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

/// Mean of the three colour channels at `(x, y)`.
fn brightness(pixels: &[u8], x: u32, y: u32) -> f32 {
    let i = ((y * W + x) * 4) as usize;
    (f32::from(pixels[i]) + f32::from(pixels[i + 1]) + f32::from(pixels[i + 2])) / 3.0
}

/// Mean brightness over `rect`.
fn mean_brightness(pixels: &[u8], rect: [u32; 4]) -> f32 {
    let [rx, ry, rw, rh] = rect;
    let mut sum = 0.0;
    let mut n = 0.0;
    for y in ry..ry + rh {
        for x in rx..rx + rw {
            sum += brightness(pixels, x, y);
            n += 1.0;
        }
    }
    sum / n
}

/// Mean absolute per-channel difference over `rect`.
fn mean_abs_diff(a: &[u8], b: &[u8], rect: [u32; 4]) -> f32 {
    let [rx, ry, rw, rh] = rect;
    let mut sum = 0.0;
    let mut n = 0.0;
    for y in ry..ry + rh {
        for x in rx..rx + rw {
            let i = ((y * W + x) * 4) as usize;
            sum += (i32::from(a[i]) - i32::from(b[i])).unsigned_abs() as f32
                + (i32::from(a[i + 1]) - i32::from(b[i + 1])).unsigned_abs() as f32
                + (i32::from(a[i + 2]) - i32::from(b[i + 2])).unsigned_abs() as f32;
            n += 3.0;
        }
    }
    sum / n
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_real_container_art_draws_and_it_dims_the_hotbar_behind_it() {
    assert_eq!(
        calculate_gui_scale(AUTO_GUI_SCALE, W, H),
        1,
        "the hotbar cell math below assumes W x H divides to itself under the GUI scale"
    );

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
    let item_atlas =
        load_item_atlas().expect("the item atlas must build from the same client.jar");

    let mut target = HeadlessTarget::new(device, W, H, format);

    // ---------------------------------------------------------------------
    // Claim 1: real background art vs. the flat programmatic fill.
    // ---------------------------------------------------------------------
    let menu = Menu::player();
    let frame = ContainerFrame::new(Some(&menu), "Inventory");

    // A point inside the panel, well clear of every slot well: the inventory
    // panel is 176x166 with the craft grid, armour column and main inventory
    // all below y=8; (60, 4) sits in the empty band between the panel's top
    // edge and its title/craft row, which the flat fill paints one colour and
    // the real texture paints the sheet's actual (non-uniform) art.
    let (px, py) = panel_point(&frame, 60.0, 4.0);

    let mut lit = ContainerRenderer::new(device, format);
    lit.attach_background(device, queue, format, background.clone());
    let acquired = target.acquire().expect("headless acquire");
    clear_view(device, queue, acquired.view());
    lit.render(device, queue, acquired.view(), &frame, W, H);
    let with_bg = target.read_texels(device, queue);

    let mut flat = ContainerRenderer::new(device, format);
    let acquired = target.acquire().expect("headless acquire");
    clear_view(device, queue, acquired.view());
    flat.render(device, queue, acquired.view(), &frame, W, H);
    let without_bg = target.read_texels(device, queue);

    let panel_rect = [px, py, 8, 8];
    let panel_diff = mean_abs_diff(&with_bg, &without_bg, panel_rect);
    eprintln!("=== container background pixel gate ===");
    eprintln!("panel_attached() before draw = {}", lit.background_attached());
    eprintln!("panel area mean abs diff (real art vs flat fill) = {panel_diff:.1}");
    assert!(
        lit.background_attached(),
        "attach_background must actually bind — otherwise the assertion below \
         would pass by accident (both renders drawing the same flat fill)"
    );
    assert!(
        panel_diff > 8.0,
        "the real `container/inventory.png` art must look different from the flat \
         programmatic panel fill at the same spot; got a mean channel diff of only \
         {panel_diff:.1}"
    );

    // ---------------------------------------------------------------------
    // Claim 2: the hotbar dims behind an open container screen.
    // ---------------------------------------------------------------------
    let diamond: HotbarSlot = HotbarSlot {
        item: "minecraft:diamond".parse().expect("valid item location"),
        count: 1,
        damage: None,
        max_damage: None,
        enchanted: false,
        dyed_color: None,
        potion_color: None,
        banner_patterns: Vec::new(),
    };
    let slots: Vec<Option<HotbarSlot>> = std::iter::once(Some(diamond))
        .chain(std::iter::repeat_with(|| None).take(8))
        .collect();
    let stats = DebugStats::default();
    let hud_frame = HudFrame {
        show_debug: false,
        crosshair: false,
        hotbar: None,
        hotbar_items: Some(&slots),
        ..HudFrame::new(&stats)
    };

    let mut hud = HudRenderer::new(device, format);
    hud.attach_items(device, queue, format, item_atlas);
    let hotbar_rect = cell_rect(0);

    // Draw the HUD alone: this is the "world + HUD" baseline every comparison
    // below is measured against.
    let acquired = target.acquire().expect("headless acquire");
    clear_view(device, queue, acquired.view());
    hud.render(device, queue, acquired.view(), acquired.view(), &hud_frame, W, H);
    let hud_only = target.read_texels(device, queue);
    let hud_only_brightness = mean_brightness(&hud_only, hotbar_rect);

    // Subject: HUD, then the container overlay **with a menu open** — the
    // real `app.rs` per-frame order after this change (container after HUD).
    let mut dim_lit = ContainerRenderer::new(device, format);
    dim_lit.attach_background(device, queue, format, background);
    let acquired = target.acquire().expect("headless acquire");
    clear_view(device, queue, acquired.view());
    hud.render(device, queue, acquired.view(), acquired.view(), &hud_frame, W, H);
    dim_lit.render(device, queue, acquired.view(), &frame, W, H);
    let dimmed = target.read_texels(device, queue);
    let dimmed_brightness = mean_brightness(&dimmed, hotbar_rect);

    // Negative control: identical sequence, but the container frame is
    // **closed** — `ContainerFrame::empty()` draws nothing at all, so the
    // hotbar pixel must be exactly what the HUD-only shot produced.
    let mut dim_control = ContainerRenderer::new(device, format);
    let empty_frame = ContainerFrame::empty();
    let acquired = target.acquire().expect("headless acquire");
    clear_view(device, queue, acquired.view());
    hud.render(device, queue, acquired.view(), acquired.view(), &hud_frame, W, H);
    dim_control.render(device, queue, acquired.view(), &empty_frame, W, H);
    let control = target.read_texels(device, queue);
    let control_brightness = mean_brightness(&control, hotbar_rect);

    eprintln!("hotbar brightness, HUD only            = {hud_only_brightness:.1}");
    eprintln!("hotbar brightness, container open       = {dimmed_brightness:.1}");
    eprintln!("hotbar brightness, container closed (control) = {control_brightness:.1}");

    assert!(
        dimmed_brightness < hud_only_brightness * 0.9,
        "the hotbar must read visibly darker once a container screen is open on \
         top of it (HUD-only={hud_only_brightness:.1}, dimmed={dimmed_brightness:.1}); \
         if this fails, either the dim gradient did not draw or the container pass \
         is not running after the HUD's"
    );
    assert!(
        (control_brightness - hud_only_brightness).abs() < 1.0,
        "the executed negative control: with the container frame closed, drawing \
         `ContainerRenderer::render` on top of the HUD must change nothing — \
         HUD-only={hud_only_brightness:.1}, control={control_brightness:.1}. A \
         difference here means the darkening above is not attributable to the open \
         menu"
    );
}

/// Converts a local widget-space offset `(lx, ly)` into the physical pixel this
/// gate reads back, using the same panel-origin math the module under test
/// uses — see `container::panel_origin`.
fn panel_point(frame: &ContainerFrame<'_>, lx: f32, ly: f32) -> (u32, u32) {
    let menu = frame.menu.expect("frame under test always carries a menu");
    let layout = slot_layout(menu);
    let (px, py) = panel_origin(&layout, W, H);
    let scale = calculate_gui_scale(AUTO_GUI_SCALE, W, H).max(1) as f32;
    (((px + lx) * scale) as u32, ((py + ly) * scale) as u32)
}
