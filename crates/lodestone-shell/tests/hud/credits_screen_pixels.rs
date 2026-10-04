//! The credits roll reaches pixels: tiled background that scrolls, an edge
//! vignette, and the wordmark with its edition strip riding up with the text.
//!
//! Goes through the shipped path (`frame_for` then `MenuRenderer::render`) with
//! the real built-in pack attached. Set `LODESTONE_CREDITS_PNG_DIR` to also
//! write the two frames as PNGs for a visual check.
//!
//! ```text
//! cargo test -p lodestone-shell --test hud credits_screen_pixels -- --ignored --nocapture
//! ```

use lodestone::menu::credits::{CreditsInput, CreditsText};
use lodestone::menu::render::{FaviconCache, MenuRenderer, frame_for, logical_canvas};
use lodestone::menu::status::{StatusCache, unavailable_probe};
use lodestone::menu::{Screen, UiState};
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 854;
const H: u32 = 480;

fn write_png(name: &str, texels: &[u8]) {
    let Ok(dir) = std::env::var("LODESTONE_CREDITS_PNG_DIR") else {
        return;
    };
    let path = std::path::Path::new(&dir).join(name);
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, W, H);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().expect("png header");
        w.write_image_data(texels).expect("png data");
    }
    std::fs::write(&path, out).expect("png write");
    eprintln!("wrote {}", path.display());
}

fn luma(texels: &[u8], x: u32, y: u32) -> f32 {
    let i = ((y * W + x) * 4) as usize;
    (f32::from(texels[i]) + f32::from(texels[i + 1]) + f32::from(texels[i + 2])) / 3.0
}

fn row_mean(texels: &[u8], y: u32, x0: u32, x1: u32) -> f32 {
    (x0..x1).map(|x| luma(texels, x, y)).sum::<f32>() / (x1 - x0) as f32
}

#[test]
#[ignore = "requires a GPU adapter and the staged built-in resource archive"]
fn the_credits_roll_draws_scrolling_background_vignette_and_logo() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available",
    );
    let (device, queue) = (ctx.device(), ctx.queue());
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut menu = MenuRenderer::new(device, format);
    let atlas = lodestone::resources::load_menu_gui_atlas().expect("built-in pack");
    eprintln!("pack has credits_vignette: {}", atlas.contains("misc/credits_vignette"));
    menu.attach_gui(device, queue, atlas);
    menu.detach_panorama();

    let mut nav = crate::owned_nav::owned_nav("credits-pixels");
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut favicons = FaviconCache::new();
    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.show_credits();
    assert_eq!(ui.screen(), Screen::Credits);
    let text = CreditsText::load();
    assert!(nav.begin_credits(&text, "Steve"), "the installed pack must carry credits text");

    // Skip the poem, which is not what is under test: drive the roll until the
    // credits-proper logo is on screen, then take two frames a second apart.
    let (_, canvas_h) = logical_canvas(0, W, H);
    let mut shoot = |menu: &mut MenuRenderer, nav: &lodestone::menu::nav::MenuNav, ui: &UiState| {
        let mut favicons = FaviconCache::new();
        let frame = frame_for(ui, nav, &statuses, &mut favicons).expect("credits frame");
        let acquired = target.acquire().expect("acquire");
        menu.render(device, queue, acquired.view(), &frame, W, H);
        target.read_texels(device, queue)
    };
    let _ = &mut favicons;

    let mut ticks = 0;
    let mut logo_seen = None;
    while ticks < 4000 {
        nav.tick_credits(&mut ui, 0.05, canvas_h, CreditsInput { speedup: true, controls: 2, reverse: false });
        ticks += 1;
        let f = frame_for(&ui, &nav, &statuses, &mut FaviconCache::new()).expect("frame");
        if let Some(y) = f.credits_logo_y {
            if y < 40.0 {
                logo_seen = Some(y);
                break;
            }
        }
    }
    let y0 = logo_seen.expect("the credits logo never scrolled into view");
    let a = shoot(&mut menu, &nav, &ui);
    write_png("credits_a.png", &a);
    nav.tick_credits(&mut ui, 1.0, canvas_h, CreditsInput::default());
    let b = shoot(&mut menu, &nav, &ui);
    write_png("credits_b.png", &b);
    eprintln!("logo y at first frame = {y0}");

    // Vignette: the screen edge is darker than the interior on a row with no text
    // in the outer columns.
    let mid = H / 2;
    let edge = row_mean(&a, mid, 0, 6);
    let inner = row_mean(&a, mid, W / 8, W / 8 + 40);
    eprintln!("edge luma {edge:.1} vs inner {inner:.1}");
    assert!(edge < inner * 0.8, "no vignette: edge {edge:.1}, inner {inner:.1}");

    // The roll moves: the logo and text columns differ between the two frames.
    // (The built-in pack's menu_background is a flat colour, so the tiled
    // background's own scroll cannot show here; its offset is pinned by
    // `menu::credits` unit tests and the tile loop is exercised by this frame.)
    let moved = (0..H).filter(|y| {
        let r = ((*y * W + 170) * 4) as usize..((*y * W + 690) * 4) as usize;
        a[r.clone()] != b[r]
    }).count();
    eprintln!("text-column rows that changed over one second: {moved}");
    assert!(moved > 100, "the roll did not move ({moved} rows changed)");

    // The wordmark is bright where the roll puts it: a bright pixel inside the
    // logo's rectangle (centre 427 px, top at y0 logical * 2 here).
    let scale = 2;
    let (lx, ly) = (((W / scale) / 2 - 128) * scale, (y0 * scale as f32) as u32);
    let bright = (0..256 * scale).flat_map(|dx| (0..44 * scale).map(move |dy| (dx, dy)))
        .filter(|(dx, dy)| ly + dy < H && luma(&a, lx + dx, ly + dy) > 80.0).count();
    eprintln!("bright logo pixels = {bright}");
    assert!(bright > 300, "the wordmark is not at its scroll position");
}
