//! Pixel gate: **the title screen's buttons must be the built-in
//! `widget/button*` art, not coloured rectangles.**
//!
//! `menu/render.rs`'s own unit tests measure *coverage inside a widget's rect*
//! and the *UVs* a quad samples. Neither can see the last hop: an atlas that
//! never uploaded, a pipeline that never bound, a sprite pass drawn in the wrong
//! order and buried under the backdrop. Coverage cannot tell vanilla's bevelled
//! button from a flat fill — that is exactly the failure this file exists to
//! reject, and it is the shape the container gate's docs warn about ("coverage
//! cannot tell a picture of a diamond from a coloured square").
//!
//! # What discriminates a texture from a fill
//!
//! `widget/button.png` was decoded straight out of `client.jar`. It is a 200×20
//! **greyscale, no-alpha** image with a bevel, measured per-row mean (sRGB, as
//! stored in the file):
//!
//! ```text
//! row  0 :   0.0   <- 1 px black outer border
//! row  1 : 167.4   <- top highlight
//! rows 2..=16 : ~110
//! rows 17..=18:  ~85   <- bottom shadow
//! row 19 :   0.0
//! ```
//!
//! So three independent, orthogonal discriminators fall out of the art itself:
//!
//! 1. **Bevel** — `row1 / row18`. A flat fill is exactly 1.0.
//! 2. **Enabled vs disabled** — `widget/button_disabled` is measurably darker
//!    (interior ~14 against ~45 linear), so the two are told apart without
//!    reading a colour constant of ours.
//! 3. **Hovered** — `widget/button_highlighted`'s top highlight row is brighter
//!    than `button`'s (158 against 129 linear), where `button_disabled`'s is
//!    near black (20). A single pixel row settles which sprite drew.
//!
//! The expected rows come from the built-in pack's own PNGs
//! (`assets/resource-packs/whimscape-26.1-26.3-r2.zip`, the `widget/button`,
//! `button_disabled` and `button_highlighted` sprites), decoded with PIL and
//! averaged over x 6..30 per channel-mean row, then linearised as below. Row 0
//! and row 19 are 1 px black borders in all three.
//!
//! ```text
//! row   button   button_disabled   button_highlighted     (linear, 0..255)
//!   1   128.9        19.6              158.2
//!  2-16  45.2        13.8               42.6
//!  17    62.2        19.6               59.2
//!  18    19.6         8.0               24.6
//! ```
//!
//! # The readback is LINEAR, not the file's sRGB
//!
//! This cost a wrong first version of the constants below, so it is spelled out.
//! `GpuAtlas::from_atlas` uploads with **`Rgba8UnormSrgb`**
//! (`lodestone-render/src/texture.rs`), so `textureSample` hands the shader
//! *linearised* values. Writing those to this gate's non-sRGB target stores them
//! linear. On the real window — an sRGB surface — they are re-encoded on write
//! and display correctly; this is the same path the HUD's hearts and hotbar take,
//! so it is the established behaviour and not a bug introduced here. But it means
//! the file's row means must be linearised before being compared to a readback:
//!
//! ```text
//! row 1 : 128.9 linear (top highlight)
//! row 18:  19.6 linear (bottom shadow)   -> ratio 6.58
//! ```
//!
//! Measured bevel: **6.54** against the 6.58 predicted — and 1.00 for the
//! flat-fill control. The band below is around the linear figure; a
//! gamma-space reading of the same file would show a ratio of about 2.6.
//!
//! The interior *means* are deliberately compared as a **ratio to each other**
//! rather than to an absolute: the sampled band spans the centred label, whose
//! white ink lifts both figures. The ratio survives that; an absolute would
//! not.
//!
//! # Controls, executed
//!
//! * [`MenuRenderer::detach_gui`] restores the flat-fill fallback in the *same*
//!   renderer and the *same* frame. The bevel assertion is applied to it and
//!   **must fail** — that is the executed negative control for "these are real
//!   textures", not a description of one.
//! * The hovered-border check is run on a *non*-hovered button too, which must
//!   see a black border. Without that, "the white row is there" could just mean
//!   every button has one.
//!
//! # Geometry
//!
//! `480×320` because `calculate_gui_scale(AUTO, 480, 320) == 1`, so the logical
//! canvas equals the framebuffer and a 20 px button really is 20 device pixels —
//! the row indices above map 1:1 onto the readback. At that size vanilla's
//! `topPos = 320/4 + 48 = 128`, so Singleplayer is at `(140, 128, 200, 20)`.
//!
//! Target format is **`Rgba8Unorm`, not `Rgba8UnormSrgb`**, so the sampled texel
//! values land in the framebuffer verbatim and the measured row means above can
//! be compared directly. On an sRGB target every number would be re-encoded and
//! an exact band would be impossible to state honestly.
//!
//! Fail-closed: a missing GPU or a missing `client.jar` is a failure, never a
//! skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test menu_button_pixels -- --ignored --nocapture
//! ```

use lodestone::config::{AUTO_GUI_SCALE, calculate_gui_scale};
use lodestone::menu::nav::MainButton;
use lodestone::menu::render::{MenuFrame, MenuRenderer, frame_for, row_rect, title_slot};
use lodestone::menu::status::{StatusCache, unavailable_probe};
use lodestone::menu::{Screen, UiState};
use lodestone::menu::render::FaviconCache;
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};

/// Framebuffer width. See the module docs on why this size.
const W: u32 = 480;
/// Framebuffer height.
const H: u32 = 320;

/// `row1 / row18` for `widget/button` as it lands in a **linear** readback:
/// 128.9 / 19.6 (see the module docs for where the rows come from).
const BEVEL_REFERENCE: f32 = 6.58;
/// A flat fill's ratio — the fallback, and the executed negative control.
const BEVEL_FLAT: f32 = 1.0;
/// Lower bound of the accepted band, about -30 % of [`BEVEL_REFERENCE`].
/// Excludes [`BEVEL_FLAT`] by a factor of four, and `button_disabled`'s own
/// 2.45 (19.6 / 8.0) by a wide margin.
const BEVEL_MIN: f32 = 4.6;
/// Upper bound of the accepted band, so a *stronger*-than-art contrast (a
/// double gamma, say) fails rather than passing as "even more bevelled".
const BEVEL_MAX: f32 = 8.6;

/// Interior mean of `widget/button` (rows 2..=16), linear. Reported, not
/// asserted — see the module docs on why the interiors are compared as a ratio.
const INTERIOR_ENABLED: f32 = 45.2;
/// Interior mean of `widget/button_disabled`, linear.
const INTERIOR_DISABLED: f32 = 13.8;

/// Midway between `button`'s top highlight row (128.9) and
/// `button_highlighted`'s (158.2): above it, the highlighted sprite drew.
const HOVER_TOP_MIN: f32 = 143.0;
/// An unhighlighted button's top highlight row stays below the same line.
const PLAIN_TOP_MAX: f32 = 143.0;
/// `button_disabled`'s top highlight row is 19.6; a hovered disabled button
/// must still read near it.
const DISABLED_TOP_MAX: f32 = 60.0;

fn clear(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) {
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("menu-gate-clear"),
    });
    {
        let _p = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("menu-gate-clear-pass"),
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
    queue.submit(std::iter::once(enc.finish()));
}

/// Mean luminance of the framebuffer row `y` across `x0..x1`.
fn row_mean(texels: &[u8], y: u32, x0: u32, x1: u32) -> f32 {
    let mut sum = 0f32;
    let mut n = 0f32;
    for x in x0..x1 {
        let i = ((y * W + x) * 4) as usize;
        if i + 2 >= texels.len() {
            continue;
        }
        sum += (f32::from(texels[i]) + f32::from(texels[i + 1]) + f32::from(texels[i + 2])) / 3.0;
        n += 1.0;
    }
    if n == 0.0 { 0.0 } else { sum / n }
}

/// Mean luminance over rows `y0..y1` across `x0..x1`.
fn band_mean(texels: &[u8], y0: u32, y1: u32, x0: u32, x1: u32) -> f32 {
    let mut sum = 0f32;
    let mut n = 0f32;
    for y in y0..y1 {
        sum += row_mean(texels, y, x0, x1);
        n += 1.0;
    }
    if n == 0.0 { 0.0 } else { sum / n }
}

/// The device-pixel rect of `button` on the title screen, taken from the same
/// `title_slot`/`row_rect` the renderer and `app.rs`'s hit-test use.
fn button_rect(button: MainButton) -> (u32, u32, u32, u32) {
    let scale = calculate_gui_scale(AUTO_GUI_SCALE, W, H).max(1) as f32;
    assert_eq!(scale, 1.0, "this gate's row indices assume GUI scale 1");
    let (x, y, w, h) = title_slot(button).resolve(W as f32, H as f32);
    (x as u32, y as u32, w as u32, h as u32)
}

/// `row1 / row18` inside a button rect — the bevel ratio. Sampled away from the
/// label, in the button's left margin, so glyph ink cannot contaminate it.
fn bevel(texels: &[u8], rect: (u32, u32, u32, u32)) -> f32 {
    let (x, y, w, h) = rect;
    // The nine-slice's left corner column is 3 px wide; skip it and the label,
    // which starts near the centre. 6..30 px in is interior on both counts.
    let (x0, x1) = (x + 6, x + 30.min(w - 6));
    let top = row_mean(texels, y + 1, x0, x1);
    let bottom = row_mean(texels, y + h - 2, x0, x1);
    if bottom <= 0.5 { f32::INFINITY } else { top / bottom }
}

#[test]
#[ignore = "requires a GPU adapter and the staged built-in resource archive"]
fn title_screen_buttons_draw_vanillas_nine_slice_art_not_flat_fills() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    // Linear, not sRGB: see the module docs on colour space.
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut menu = MenuRenderer::new(device, format);

    // Attach the real pack explicitly rather than relying on `MenuRenderer`'s
    // lazy `ensure_gui`, so a missing jar fails *here*, loudly, instead of
    // silently degrading to the fallback and passing every assertion below
    // against the thing this gate exists to reject.
    let atlas = lodestone::resources::load_menu_gui_atlas().expect(
        "no built-in pack found; set LODESTONE_ASSETS to a root holding lodestone-resources.zip \
         + generated/reports/blocks.json",
    );
    menu.attach_gui(device, queue, atlas);
    assert!(
        menu.gui_attached(),
        "the GUI atlas must be bound or every measurement below is of the fallback"
    );
    // Pin the backdrop to the flat `BG` fill, so that every absolute luminance
    // bound in this gate (the backdrop control above the logo,
    // the "nothing is drawn in the gap below the button" bound) is measured
    // against a **compile-time constant instead of a 2.6 MB asset**.
    //
    // The cubemap panorama now draws behind every out-of-world screen, and its six
    // real faces are 1024×1024 of varied sky delivered through the launcher's
    // asset-object store — so without this, three bounds in a button-chrome gate
    // would depend on which part of Mojang's sky happens to land in each sampled
    // rect, and on whether the object store is even populated (unpopulated falls
    // back to flat grey stubs, a different number again). That is a confound, not
    // a measurement, and no re-calibration fixes it because the backdrop is no
    // longer a constant to calibrate against.
    //
    // Calling this *before* the first draw also stops `ensure_panorama` loading
    // ~2.6 MB of PNG that this gate would then throw away. The panorama has its
    // own gates in `menu_panorama_pixels.rs`; see `docs/menu-panorama.md`.
    menu.detach_panorama();
    assert!(!menu.panorama_attached());

    let nav = crate::owned_nav::owned_nav("menu-pixels");
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut favicons = FaviconCache::new();
    let ui = UiState::new();
    assert_eq!(ui.screen(), Screen::MainMenu);

    let mut shoot = |menu: &mut MenuRenderer, frame: &MenuFrame<'_>| -> Vec<u8> {
        let acquired = target.acquire().expect("headless acquire");
        clear(device, queue, acquired.view());
        menu.render(device, queue, acquired.view(), frame, W, H);
        target.read_texels(device, queue)
    };

    // The frame the shipped client draws: `app.rs`'s `draw_menu` calls exactly
    // `frame_for` then `MenuRenderer::render`.
    let mut frame = frame_for(&ui, &nav, &statuses, &mut favicons).expect("the title screen draws");
    // Singleplayer (index 0) is the highlight by default; move it off so the
    // plain and hovered sprites can both be measured on known widgets.
    frame.selected = usize::MAX;
    let plain = shoot(&mut menu, &frame);
    // Optional capture for eyeballing the layout: set to a file path to keep
    // the title screen as a PNG.
    if let Ok(path) = std::env::var("LODESTONE_TITLE_PNG") {
        let mut out = Vec::new();
        {
            let mut enc = png::Encoder::new(&mut out, W, H);
            enc.set_color(png::ColorType::Rgba);
            enc.set_depth(png::BitDepth::Eight);
            let mut w = enc.write_header().expect("png header");
            w.write_image_data(&plain).expect("png data");
        }
        std::fs::write(&path, out).expect("png write");
        eprintln!("wrote {path}");
    }

    let sp = button_rect(MainButton::Singleplayer);
    let realms = button_rect(MainButton::Realms);
    let (x0, x1) = (sp.0 + 6, sp.0 + sp.2 - 6);

    eprintln!("=== menu button pixel gate ===");
    eprintln!("Singleplayer rect      = {sp:?}");
    eprintln!("Minecraft Realms rect  = {realms:?}");

    // ---- 1. The bevel: real art, not a flat fill --------------------------
    let sp_bevel = bevel(&plain, sp);
    eprintln!("Singleplayer bevel     = {sp_bevel:.3}");
    eprintln!("  art widget/button      = {BEVEL_REFERENCE:.3}  (band {BEVEL_MIN}..{BEVEL_MAX})");
    eprintln!("  a flat fill            = {BEVEL_FLAT:.3}");
    assert!(
        (BEVEL_MIN..=BEVEL_MAX).contains(&sp_bevel),
        "the button's bevel is {sp_bevel:.3}, outside the \
         {BEVEL_MIN}..{BEVEL_MAX} band around the art's {BEVEL_REFERENCE:.2}. \
         A flat fill reads {BEVEL_FLAT:.1}; a value above the band means the \
         contrast is being transformed on top of the sRGB decode"
    );

    // ---- 2. Enabled vs disabled art ---------------------------------------
    let sp_interior = band_mean(&plain, sp.1 + 3, sp.1 + sp.3 - 3, x0, x1);
    let realms_interior = band_mean(&plain, realms.1 + 3, realms.1 + realms.3 - 3, x0, x1);
    eprintln!(
        "enabled interior mean  = {sp_interior:.1} (art, linearised: {INTERIOR_ENABLED}, plus label ink)"
    );
    eprintln!(
        "disabled interior mean = {realms_interior:.1} (art, linearised: {INTERIOR_DISABLED}, plus label ink)"
    );
    assert!(
        realms_interior < sp_interior * 0.75,
        "the disabled Realms button is not visibly darker than the enabled \
         Singleplayer one ({realms_interior:.1} vs {sp_interior:.1}); \
         widget/button_disabled's interior is ~14 against widget/button's ~45"
    );
    // And it is flat where the enabled one is bevelled — an independent
    // property of the disabled sprite, so this is a second discriminator and
    // not a restatement of the brightness one.
    let realms_bevel = bevel(&plain, realms);
    eprintln!("disabled bevel         = {realms_bevel:.3} (source art: 2.45)");
    assert!(
        realms_bevel < BEVEL_MIN,
        "widget/button_disabled has almost no bevel in the pack (2.45), but a full one was measured \
         ({realms_bevel:.3}) — the enabled sprite is being drawn for a disabled \
         button"
    );

    // ---- 3. Hovered picks widget/button_highlighted ------------------------
    // Its top highlight row is brighter than `button`'s; `button_disabled`'s is
    // near black.
    let plain_top = row_mean(&plain, sp.1 + 1, x0, x1);
    frame.selected = 0; // Singleplayer
    let hovered = shoot(&mut menu, &frame);
    let hover_top = row_mean(&hovered, sp.1 + 1, x0, x1);
    eprintln!("plain top highlight row   = {plain_top:.1} (source art: 128.9)");
    eprintln!("hovered top highlight row = {hover_top:.1} (source art: 158.2)");
    assert!(
        hover_top > HOVER_TOP_MIN,
        "a highlighted button must draw widget/button_highlighted, whose top \
         highlight row reads 158; measured {hover_top:.1}"
    );
    assert!(
        plain_top < PLAIN_TOP_MAX,
        "the control failed: an unhighlighted button's top highlight row should \
         read 129, measured {plain_top:.1} — the detector cannot tell the \
         highlighted sprite apart"
    );

    // A hovered *disabled* button must still be the disabled sprite: the
    // sprite-selection rule most likely to be got wrong by a hand-rolled
    // highlight.
    // By position in `MAIN_BUTTONS`, not `as usize`: the enum's declaration
    // order and the array's order agree today, but only the array is the index
    // space the renderer and the hit-test actually use.
    frame.selected = lodestone::menu::nav::MAIN_BUTTONS
        .iter()
        .position(|b| *b == MainButton::Realms)
        .expect("Realms is a title-screen widget");
    let hovered_off = shoot(&mut menu, &frame);
    let off_top = row_mean(&hovered_off, realms.1 + 1, x0, x1);
    eprintln!("hovered-disabled top row  = {off_top:.1} (source art: 19.6)");
    assert!(
        off_top < DISABLED_TOP_MAX,
        "a hovered but disabled button drew a lit sprite ({off_top:.1}); \
         vanilla gives disabled priority over hovered"
    );

    // ---- 4. The logo reached pixels ---------------------------------------
    // The "LODESTONE" lettering is menu-font text; if the draw is skipped the
    // band is bare backdrop.
    let logo_x = (W / 2 - 128) as u32;
    let logo_band = band_mean(&plain, 30, 74, logo_x + 8, logo_x + 248);
    // The control: the same-width band *above* the logo, which is backdrop only.
    let above = band_mean(&plain, 4, 24, logo_x + 8, logo_x + 248);
    eprintln!("logo band mean         = {logo_band:.2}");
    eprintln!("backdrop control mean  = {above:.2}");
    assert!(
        logo_band > above + 4.0,
        "the title lettering did not reach pixels: logo band {logo_band:.2} vs \
         backdrop {above:.2}"
    );

    // ---- 5. The executed negative control ---------------------------------
    // Same renderer, same frame, atlas detached: the flat-fill fallback. The
    // bevel assertion must now FAIL, which is what proves assertions 1-3 were
    // measuring the real art and not something every render has.
    frame.selected = usize::MAX;
    // The lettering is text, so it survives losing the atlas; the logo flag is
    // what this control switches off to prove the band measures the lettering.
    frame.logo = false;
    menu.detach_gui();
    assert!(!menu.gui_attached());
    let control = shoot(&mut menu, &frame);
    let control_bevel = bevel(&control, sp);
    let control_logo = band_mean(&control, 30, 74, logo_x + 8, logo_x + 248);
    eprintln!("control bevel          = {control_bevel:.3} (must be flat)");
    eprintln!("control logo band      = {control_logo:.2} (must match backdrop)");
    assert!(
        control_bevel < BEVEL_MIN,
        "the negative control still shows a bevel ({control_bevel:.3}) — the \
         flat-fill fallback is not actually flat, so the bevel assertion above \
         proves nothing"
    );
    assert!(
        (control_logo - above).abs() < 4.0,
        "the negative control still drew the title lettering ({control_logo:.2} vs backdrop \
         {above:.2})"
    );
    assert_ne!(
        row_mean(&control, sp.1 + 5, x0, x1),
        row_mean(&plain, sp.1 + 5, x0, x1),
        "the control render is identical to the textured one, so `detach_gui` \
         did nothing and this whole gate is vacuous"
    );

    // Row rects and the draw agree: the button really is where the hit-test
    // thinks it is. Sampling one row *below* the button must be backdrop.
    let below = row_mean(&plain, sp.1 + sp.3 + 1, x0, x1);
    eprintln!("row below the button   = {below:.2}");
    assert!(
        below < 40.0,
        "something is drawn in the 4 px gap below Singleplayer ({below:.2}); the \
         drawn rect is taller than `row_rect` reports and the hit-test is wrong"
    );
    assert_eq!(
        row_rect(&frame.rows, 0, W as f32, H as f32).map(|r| (r.0 as u32, r.1 as u32)),
        Some((sp.0, sp.1)),
        "row_rect and title_slot disagree, so the hit-test and the draw would too"
    );
}
