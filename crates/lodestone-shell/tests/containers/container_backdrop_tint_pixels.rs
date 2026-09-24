//! Location-level GPU gate for the container backdrop tint.
//!
//! The backdrop is a straight-alpha black gradient over the already-rendered
//! game image. This gate seeds an offscreen target with both bright and nearly
//! black pixels, renders each container-family geometry producer, and reads a
//! point outside the panel. Every colour channel must decrease, the target
//! alpha must remain opaque, and the stronger bottom alpha must be darker than
//! the top. A closed-frame render is the executed no-overlay control.
//!
//! ```text
//! cargo test -p lodestone-shell --test containers -- --ignored --nocapture
//! ```

use lodestone::container::{
    ContainerFrame, ContainerGeometry, ContainerRenderer, CreativeState, CreativeView,
    creative_geometry,
};
use lodestone::menu::advancements::{
    AdvancementProgress, AdvancementsState, AdvancementsView, advancements_geometry,
    advancements_layout,
};
use lodestone_game::menu::Menu;
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 480;
const H: u32 = 320;
const PROBE_X: u32 = 12;
const PROBE_TOP_Y: u32 = 16;
const PROBE_BOTTOM_Y: u32 = H - 16;

fn seed_target(target: &HeadlessTarget, queue: &wgpu::Queue, colour: [u8; 4]) {
    let mut row = Vec::with_capacity((W * 4) as usize);
    for _ in 0..W {
        row.extend_from_slice(&colour);
    }
    let mut pixels = Vec::with_capacity((W * H * 4) as usize);
    for _ in 0..H {
        pixels.extend_from_slice(&row);
    }
    queue.write_texture(
        wgpu::TexelCopyTextureInfo {
            texture: target.texture(),
            mip_level: 0,
            origin: wgpu::Origin3d::ZERO,
            aspect: wgpu::TextureAspect::All,
        },
        &pixels,
        wgpu::TexelCopyBufferLayout {
            offset: 0,
            bytes_per_row: Some(W * 4),
            rows_per_image: Some(H),
        },
        wgpu::Extent3d { width: W, height: H, depth_or_array_layers: 1 },
    );
}

fn pixel(pixels: &[u8], x: u32, y: u32) -> [u8; 4] {
    let i = ((y * W + x) * 4) as usize;
    [pixels[i], pixels[i + 1], pixels[i + 2], pixels[i + 3]]
}

fn render_geometry(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &mut HeadlessTarget,
    renderer: &mut ContainerRenderer,
    geometry: &ContainerGeometry,
    seed: [u8; 4],
) -> Vec<u8> {
    seed_target(target, queue, seed);
    let acquired = target.acquire().expect("headless target acquire");
    renderer.render_geometry_scaled(device, queue, acquired.view(), None, geometry, 1, W, H);
    target.read_texels(device, queue)
}

fn render_ordinary(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &mut HeadlessTarget,
    renderer: &mut ContainerRenderer,
    frame: &ContainerFrame<'_>,
    seed: [u8; 4],
) -> Vec<u8> {
    seed_target(target, queue, seed);
    let acquired = target.acquire().expect("headless target acquire");
    renderer.render(device, queue, acquired.view(), frame, W, H);
    target.read_texels(device, queue)
}

fn assert_darkens(before: [u8; 4], after: [u8; 4], label: &str) {
    for channel in 0..3 {
        assert!(
            after[channel] < before[channel],
            "{label} channel {channel} must darken at the outside-panel probe: before={before:?}, after={after:?}"
        );
    }
    assert_eq!(
        after[3], before[3],
        "{label} backdrop must preserve the opaque destination alpha"
    );
}

#[test]
#[ignore = "requires a GPU adapter"]
fn container_backdrop_is_a_black_straight_alpha_overlay_at_a_world_location() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; run on a host with a GPU",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let menu = Menu::player();
    let frame = ContainerFrame::new(Some(&menu), "Inventory");
    let dark_seed = [8, 12, 20, 255];
    let bright_seed = [200, 150, 100, 255];

    // The ordinary path is the production path for every chest, station and
    // player-inventory screen. The probe is deliberately outside the centred
    // panel, so only the full-canvas backdrop can change it.
    let mut ordinary = ContainerRenderer::new(device, format);
    let dark = render_ordinary(device, queue, &mut target, &mut ordinary, &frame, dark_seed);
    assert_darkens(dark_seed, pixel(&dark, PROBE_X, PROBE_TOP_Y), "ordinary dark scene");
    let bright_top = render_ordinary(
        device,
        queue,
        &mut target,
        &mut ordinary,
        &frame,
        bright_seed,
    );
    let bright_bottom = render_ordinary(
        device,
        queue,
        &mut target,
        &mut ordinary,
        &frame,
        bright_seed,
    );
    let top = pixel(&bright_top, PROBE_X, PROBE_TOP_Y);
    let bottom = pixel(&bright_bottom, PROBE_X, PROBE_BOTTOM_Y);
    assert_darkens(bright_seed, top, "ordinary bright scene at top");
    assert_darkens(bright_seed, bottom, "ordinary bright scene at bottom");
    assert!(
        bottom[0] < top[0] && bottom[1] < top[1] && bottom[2] < top[2],
        "the bottom alpha must be stronger than the top alpha: top={top:?}, bottom={bottom:?}"
    );

    // The creative and advancements screens share the same renderer but build
    // their own geometry. Rendering both here prevents either producer from
    // retaining a stale tint literal that the ordinary path no longer uses.
    let creative = creative_geometry(
        &CreativeState::default(),
        CreativeView { menu: Some(&menu), title: "Building Blocks", cursor: None, tooltips: None },
        1,
        W,
        H,
        None,
        None,
        None,
        None,
    );
    let mut advancement_state = AdvancementsState::default();
    let progress = AdvancementProgress::default();
    let advancement_layout = advancements_layout(&mut advancement_state, &progress, 1, W, H)
        .expect("the default advancement tab has a layout");
    let advancements = advancements_geometry(
        &advancement_layout,
        AdvancementsView {
            title: "Advancements",
            hovered: None,
            hovered_title: "",
            hovered_description: "",
            progress: &progress,
            fade: 0.0,
        },
        1,
        W,
        H,
        None,
        None,
        None,
        None,
    );
    let mut family_renderer = ContainerRenderer::new(device, format);
    let creative_pixels = render_geometry(
        device,
        queue,
        &mut target,
        &mut family_renderer,
        &creative,
        dark_seed,
    );
    assert_darkens(
        dark_seed,
        pixel(&creative_pixels, PROBE_X, PROBE_TOP_Y),
        "creative dark scene",
    );
    let advancement_pixels = render_geometry(
        device,
        queue,
        &mut target,
        &mut family_renderer,
        &advancements,
        dark_seed,
    );
    assert_darkens(
        dark_seed,
        pixel(&advancement_pixels, PROBE_X, PROBE_TOP_Y),
        "advancements dark scene",
    );

    // Executed negative control: no menu means no geometry and therefore no
    // colour or alpha change at the same location.
    let closed = render_ordinary(
        device,
        queue,
        &mut target,
        &mut ordinary,
        &ContainerFrame::empty(),
        dark_seed,
    );
    assert_eq!(
        pixel(&closed, PROBE_X, PROBE_TOP_Y),
        dark_seed,
        "a closed container frame must leave the seeded world pixel unchanged"
    );
}
