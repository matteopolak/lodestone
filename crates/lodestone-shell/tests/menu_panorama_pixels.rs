//! The title screen's cubemap panorama reaches pixels, with the right face in
//! the right place.
//!
//! ## Why this gate exists
//!
//! A cubemap loader nothing draws is the island defect this repo has hit
//! seventeen times: `panorama.rs`'s own unit tests are a closed loop over
//! `assemble`/`wrap_degrees`/`view_projection` and stay green whether or not
//! `MenuRenderer` ever binds the thing. So this gate goes through the *shipped*
//! path — `frame_for` then `MenuRenderer::render`, exactly what `app.rs`'s
//! `draw_menu` calls — and reads the framebuffer back.
//!
//! ## Why a synthetic cubemap and not the real one
//!
//! 26.2's `client.jar` ships the panorama as six **1×1 solid grey**
//! `(98, 111, 113)` PNGs and a 1×1 fully transparent overlay (measured, 69 bytes
//! each). Against the real faces every face-order and orientation bug is
//! invisible by construction — the frame is uniform whatever you do. So the gate
//! attaches six faces of six distinguishable colours through the same
//! `attach_panorama` entry point and asserts *which layer lands where*.
//!
//! ## What the expected values come from
//!
//! Not from this repo. A cubemap's layers are `+X, -X, +Y, -Y, +Z, -Z` (identical
//! in the GL, Vulkan and WebGPU face-selection tables), and vanilla's
//! `CubeMapTexture.SUFFIXES` fills them in the order `_1, _3, _5, _4, _0, _2`.
//! Compose the two and `panorama_0` is the **+Z** face, `panorama_1` is **+X**,
//! `panorama_3` is **-X**. At spin 0 the model-view is `rotationX(190°)`, which
//! puts object-space +Z in front of the camera — so the middle of the screen must
//! be `panorama_0`, the right edge `panorama_1`, the left edge `panorama_3`.
//!
//! Only layer *selection* is under test here, never the within-face orientation:
//! the six faces are solid, so a wrong flip cannot pass or fail this gate. The
//! flip is pinned separately, and hermetically, by
//! `panorama::tests::assemble_stacks_faces_in_order_and_flips_each_vertically`.
//!
//! ## Colour space
//!
//! The target is **`Rgba8UnormSrgb`**, unlike the sibling menu gates' linear
//! `Rgba8Unorm`. The cubemap uploads as `Rgba8UnormSrgb` too, so sample → write
//! is a decode followed by the matching encode and the readback is byte-exact —
//! which is what lets this gate compare exact colours rather than ratios.
//!
//! ```text
//! cargo test -p lodestone-shell --test menu_panorama_pixels -- --ignored --nocapture
//! ```

use lodestone::menu::nav::{MainButton, MenuNav};
use lodestone::menu::panorama::{self, PanoramaFaces};
use lodestone::menu::render::{
    Align, FaviconCache, MenuBackdrop, MenuRenderer, frame_for, loading_frame, logical_canvas,
    text_px, title_slot,
};
use lodestone::menu::status::{StatusCache, unavailable_probe};
use lodestone::menu::{Screen, UiState};
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};

/// Framebuffer width. Wide enough (aspect 1.5) that the 85° horizontal spread
/// reaches past the cube's ±1 side walls, so the ±X faces are on screen at all:
/// `tan(42.5°) * 1.5 = 1.374 > 1`. At aspect 1.0 the side faces would be off
/// screen and the left/right assertions below would be vacuous.
const W: u32 = 480;
/// Framebuffer height.
const H: u32 = 320;

/// Side length of each synthetic face. Any size works — the faces are solid — so
/// this is small on purpose.
const FACE: u32 = 8;

/// One vivid colour per **cubemap layer**, i.e. indexed the same way
/// [`panorama::FACE_SUFFIXES`] is. Chosen far apart in RGB so a near-miss cannot
/// be mistaken for a hit, and none of them is a colour the menu itself paints.
const FACE_COLOURS: [[u8; 3]; 6] = [
    [255, 0, 0],     // layer 0 = +X  = panorama_1
    [0, 255, 0],     // layer 1 = -X  = panorama_3
    [0, 0, 255],     // layer 2 = +Y  = panorama_5
    [255, 255, 0],   // layer 3 = -Y  = panorama_4
    [255, 0, 255],   // layer 4 = +Z  = panorama_0
    [0, 255, 255],   // layer 5 = -Z  = panorama_2
];

/// Per-channel tolerance when matching a readback pixel to a face colour.
///
/// Not zero: the cubemap uploads as `Rgba8UnormSrgb` and the target is
/// `Rgba8UnormSrgb`, so a texel makes an sRGB decode → f32 → sRGB encode round
/// trip, which no spec promises is byte-exact. Three LSBs is generous against
/// that and still tells the six colours apart by a factor of eighty.
const TOL: u8 = 3;

/// Which face colour, if any, a readback pixel is.
fn face_of(px: [u8; 3]) -> Option<usize> {
    FACE_COLOURS
        .iter()
        .position(|c| (0..3).all(|i| c[i].abs_diff(px[i]) <= TOL))
}

/// The layer a cubemap samples for a direction whose major axis is +Z.
const LAYER_PLUS_Z: usize = 4;
/// Major axis +X.
const LAYER_PLUS_X: usize = 0;
/// Major axis -X.
const LAYER_MINUS_X: usize = 1;

/// Six solid faces, layer-major — the exact bytes `attach_panorama` uploads.
///
/// Built directly rather than through [`panorama::assemble`] so this gate does not
/// inherit whatever `assemble` believes: the mapping "layer n holds colour n" is
/// stated here, in the test, and `assemble` is pinned by its own unit tests.
fn synthetic_cubemap() -> PanoramaFaces {
    let texels = (FACE * FACE) as usize;
    let mut rgba = Vec::with_capacity(texels * 6 * 4);
    for colour in FACE_COLOURS {
        for _ in 0..texels {
            rgba.extend_from_slice(&[colour[0], colour[1], colour[2], 255]);
        }
    }
    PanoramaFaces {
        size: FACE,
        rgba,
        // Synthetic, so neither source: this gate is about layer selection, and
        // it must not claim to be the real art. The real-art gates below are the
        // ones that assert 6.
        from_object_store: 0,
    }
}

/// Mean and population standard deviation of per-pixel luminance inside a rect,
/// plus the bounding box of the darkest and brightest pixels found.
///
/// Standard deviation is the discriminator the real cubemap makes available and
/// the jar's stubs do not: vanilla's faces measure a luminance stdev of 3.7–13.6
/// each, while a stub face — and the flat `BG` backdrop — is *exactly* 0.
fn luminance_spread(texels: &[u8], rect: (u32, u32, u32, u32)) -> (f32, f32, (u32, u32, u32, u32)) {
    let (x0, y0, x1, y1) = rect;
    let mut values: Vec<(f32, u32, u32)> = Vec::new();
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * W + x) * 4) as usize;
            if i + 2 >= texels.len() {
                continue;
            }
            let lum = (f32::from(texels[i]) + f32::from(texels[i + 1]) + f32::from(texels[i + 2]))
                / 3.0;
            values.push((lum, x, y));
        }
    }
    if values.is_empty() {
        return (0.0, 0.0, (0, 0, 0, 0));
    }
    let n = values.len() as f32;
    let mean = values.iter().map(|v| v.0).sum::<f32>() / n;
    let var = values.iter().map(|v| (v.0 - mean) * (v.0 - mean)).sum::<f32>() / n;
    let darkest = values
        .iter()
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .expect("non-empty");
    let brightest = values
        .iter()
        .max_by(|a, b| a.0.total_cmp(&b.0))
        .expect("non-empty");
    (
        mean,
        var.sqrt(),
        (darkest.1, darkest.2, brightest.1, brightest.2),
    )
}

/// Count how many pixels of each face colour appear inside a rect, and the
/// bounding box of the `expected` one.
///
/// Returns `(counts_per_layer, other, bbox_of_expected)`. A gate that reports only
/// a fraction cannot tell a uniform-but-wrong frame from a localised blob, so the
/// bbox is what the failure messages print.
fn survey(
    texels: &[u8],
    rect: (u32, u32, u32, u32),
    expected: usize,
) -> ([u32; 6], u32, Option<(u32, u32, u32, u32)>) {
    let (x0, y0, x1, y1) = rect;
    let mut counts = [0u32; 6];
    let mut other = 0u32;
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * W + x) * 4) as usize;
            if i + 2 >= texels.len() {
                continue;
            }
            let px = [texels[i], texels[i + 1], texels[i + 2]];
            match face_of(px) {
                Some(layer) => {
                    counts[layer] += 1;
                    if layer == expected {
                        bbox = Some(match bbox {
                            None => (x, y, x + 1, y + 1),
                            Some((bx0, by0, bx1, by1)) => (
                                bx0.min(x),
                                by0.min(y),
                                bx1.max(x + 1),
                                by1.max(y + 1),
                            ),
                        });
                    }
                }
                None => other += 1,
            }
        }
    }
    (counts, other, bbox)
}

/// A strip of pure background straight ahead of the camera: the gap between the
/// logo and the first title button.
///
/// **Derived from the same expression the draw uses** — `title_slot`, which is
/// what both `geometry` and `app.rs`'s hit-test resolve — rather than from
/// hardcoded rows, and then checked against every widget rect by
/// [`assert_band_is_background`]. A band pinned to a literal is how a HUD gate
/// once measured twenty pixels above a row that was drawing perfectly.
fn straight_ahead_band() -> (u32, u32, u32, u32) {
    let (_, first_y, _, _) = title_slot(MainButton::Singleplayer).resolve(W as f32, H as f32);
    let y1 = first_y as u32 - 2;
    (W / 2 - 40, y1 - 20, W / 2 + 40, y1)
}

/// Fail if `band` overlaps any title-screen widget, which would make the survey a
/// measurement of button chrome rather than of the panorama.
fn assert_band_is_background(band: (u32, u32, u32, u32)) {
    let buttons = [
        MainButton::Singleplayer,
        MainButton::Multiplayer,
        MainButton::Realms,
        MainButton::Friends,
        MainButton::Language,
        MainButton::Accessibility,
        MainButton::Options,
        MainButton::Quit,
        MainButton::Accounts,
    ];
    for button in buttons {
        let (x, y, w, h) = title_slot(button).resolve(W as f32, H as f32);
        let (bx0, by0, bx1, by1) = band;
        let overlaps = (x as u32) < bx1
            && bx0 < (x + w) as u32
            && (y as u32) < by1
            && by0 < (y + h) as u32;
        assert!(
            !overlaps,
            "the probe band {band:?} overlaps {button:?} at \
             ({x}, {y}, {w}, {h}) — the layout moved, so re-derive the band \
             rather than loosening the assertion"
        );
    }
}

/// Human-readable survey line, naming layers by their vanilla suffix.
fn describe(counts: &[u32; 6], other: u32) -> String {
    let mut parts: Vec<String> = Vec::new();
    for (layer, n) in counts.iter().enumerate() {
        if *n > 0 {
            parts.push(format!("panorama{}={n}", panorama::FACE_SUFFIXES[layer]));
        }
    }
    parts.push(format!("not-a-face={other}"));
    parts.join(" ")
}

/// Assert `expected` is the dominant face inside `rect`, printing the bounding box
/// of the match on failure.
fn assert_face(
    texels: &[u8],
    rect: (u32, u32, u32, u32),
    expected: usize,
    what: &str,
) {
    let (counts, other, bbox) = survey(texels, rect, expected);
    let total: u32 = (rect.2 - rect.0) * (rect.3 - rect.1);
    let hit = counts[expected];
    let rival = counts
        .iter()
        .enumerate()
        .filter(|(layer, _)| *layer != expected)
        .map(|(_, n)| *n)
        .max()
        .unwrap_or(0);
    eprintln!(
        "{what}: rect={rect:?} ({total} px) expect panorama{} -> {}",
        panorama::FACE_SUFFIXES[expected],
        describe(&counts, other)
    );
    eprintln!("  bbox of expected face = {bbox:?}");
    assert!(
        hit > total / 4,
        "{what}: only {hit} of {total} px in {rect:?} are panorama{} \
         (bbox {bbox:?}); survey was {}. Either the panorama is not drawing at \
         all, or the face order in `panorama::FACE_SUFFIXES` is wrong",
        panorama::FACE_SUFFIXES[expected],
        describe(&counts, other)
    );
    assert!(
        hit > rival,
        "{what}: panorama{} is not the dominant face in {rect:?} — {}. A rival \
         face winning here means the cubemap layers are permuted",
        panorama::FACE_SUFFIXES[expected],
        describe(&counts, other)
    );
}

#[test]
#[ignore = "requires a GPU adapter"]
fn the_title_screen_draws_the_cubemap_panorama_with_vanillas_face_order() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    // sRGB, not linear: see the module docs on colour space.
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut menu = MenuRenderer::new(device, format);

    menu.attach_panorama(device, queue, &synthetic_cubemap());
    assert!(
        menu.panorama_attached(),
        "the synthetic cubemap must be bound or every measurement below is of the \
         flat backdrop"
    );

    let nav = MenuNav::with_path(std::env::temp_dir().join(format!(
        "lodestone-panorama-pixels-{}/servers.json",
        std::process::id()
    )));
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut favicons = FaviconCache::new();
    let ui = UiState::new();
    assert_eq!(ui.screen(), Screen::MainMenu, "this gate is the title screen's");

    let frame = frame_for(&ui, &nav, &statuses, &mut favicons).expect("the title screen draws");
    assert!(frame.logo, "the title screen frame is the one with `logo` set, \
             which is what suppresses the menu-background wash");
    assert_eq!(
        frame.backdrop,
        MenuBackdrop::Panorama,
        "the title screen owns its frame and asks for the panorama"
    );

    let mut shoot = |menu: &mut MenuRenderer| -> Vec<u8> {
        let acquired = target.acquire().expect("headless acquire");
        menu.render(device, queue, acquired.view(), &frame, W, H);
        target.read_texels(device, queue)
    };

    let shot = shoot(&mut menu);

    eprintln!("=== title screen panorama gate ===");
    eprintln!("spin is 0: a freshly attached renderer has advanced nothing yet");

    // ---- 1. Straight ahead is the +Z face ---------------------------------
    // `rotationX(PI)` maps object +Z to view -Z, which is where a right-handed
    // camera looks. The band is above the first button and below the logo, and
    // well inside the +Z region: 42.5 degrees of vertical spread plus the 10
    // degree tilt hands the lower fifth of a centre column to the +Y face, which
    // is correct and would make a full-height probe measure two faces.
    let band = straight_ahead_band();
    assert_band_is_background(band);
    assert_face(&shot, band, LAYER_PLUS_Z, "straight ahead");

    // ---- 2. The right edge is +X, the left edge -X ------------------------
    // At aspect 1.5 the outermost 16 px columns are past the cube's side walls:
    // |x| >= 1.28 there, against |y| <= 0.92 and |z| <= 1.15.
    assert_face(&shot, (W - 16, 0, W, H), LAYER_PLUS_X, "right edge");
    assert_face(&shot, (0, 0, 16, H), LAYER_MINUS_X, "left edge");

    // ---- 3. The negative control ------------------------------------------
    // Detach and the same three probes must stop seeing any face at all. Without
    // this, every assertion above could be passing on something else that happens
    // to paint the screen.
    menu.detach_panorama();
    assert!(!menu.panorama_attached(), "the control must actually detach");
    let control = shoot(&mut menu);
    let (counts, other, bbox) = survey(&control, band, LAYER_PLUS_Z);
    eprintln!("control (panorama detached): {}", describe(&counts, other));
    assert_eq!(
        counts[LAYER_PLUS_Z], 0,
        "the control still shows panorama{} at {bbox:?} — the detector is not \
         measuring the panorama at all, so the assertions above prove nothing",
        panorama::FACE_SUFFIXES[LAYER_PLUS_Z]
    );
    assert_eq!(
        counts.iter().sum::<u32>(),
        0,
        "the control shows some face colour: {}",
        describe(&counts, other)
    );
}

/// sRGB decode, for predicting the wash. The standard transfer function, written
/// out rather than taken from any of our own code.
fn srgb_to_linear(byte: u8) -> f32 {
    let x = f32::from(byte) / 255.0;
    if x <= 0.040_45 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}

/// sRGB encode, the inverse of [`srgb_to_linear`], back to a `0..=255` scale.
fn linear_to_srgb(x: f32) -> f32 {
    let y = if x <= 0.003_130_8 {
        x * 12.92
    } else {
        1.055 * x.powf(1.0 / 2.4) - 0.055
    };
    y * 255.0
}

/// The washed value of one channel byte, predicted from **outside** this repo's
/// render path: the panorama samples an `Rgba8UnormSrgb` cubemap (so the shader
/// sees linear), multiplies by `1 - MENU_BACKGROUND_DIM`, and writes to an
/// `Rgba8UnormSrgb` target (so the encode happens on the way out).
///
/// `MENU_BACKGROUND_DIM` is `64/255`, measured out of 26.2's `client.jar`:
/// `menu_background.png` is 16×16 and every pixel is grey 0, alpha 64.
fn washed(byte: u8) -> f32 {
    linear_to_srgb(srgb_to_linear(byte) * (1.0 - panorama::MENU_BACKGROUND_DIM))
}

/// Tolerance for the wash prediction, per channel. Wider than [`TOL`] because
/// this value is the result of a decode → multiply → encode round trip rather
/// than a passthrough, and still narrow enough that the three hypotheses in
/// [`the_loading_screen_draws_the_panorama_under_the_menu_background_wash`] —
/// 224, 255 and 191 — cannot be confused for each other.
const WASH_TOL: f32 = 5.0;

/// Count pixels in `rect` matching each of `candidates`, plus the bounding box of
/// the first candidate's matches.
///
/// Counting is what this needs rather than a mean: the probe band is not
/// uniformly one cubemap face (the existing title gate only asserts the expected
/// face is *dominant* there), so averaging would mix +Z with its neighbour and
/// land between every hypothesis.
fn count_candidates(
    texels: &[u8],
    rect: (u32, u32, u32, u32),
    candidates: &[[f32; 3]],
) -> (Vec<u32>, (u32, u32, u32, u32)) {
    let (x0, y0, x1, y1) = rect;
    let mut counts = vec![0u32; candidates.len()];
    let (mut bx0, mut by0, mut bx1, mut by1) = (u32::MAX, u32::MAX, 0, 0);
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * W + x) * 4) as usize;
            let px = [
                f32::from(texels[i]),
                f32::from(texels[i + 1]),
                f32::from(texels[i + 2]),
            ];
            for (slot, want) in candidates.iter().enumerate() {
                if (0..3).all(|c| (px[c] - want[c]).abs() <= WASH_TOL) {
                    counts[slot] += 1;
                    if slot == 0 {
                        bx0 = bx0.min(x);
                        by0 = by0.min(y);
                        bx1 = bx1.max(x);
                        by1 = by1.max(y);
                    }
                }
            }
        }
    }
    (counts, (bx0, by0, bx1, by1))
}

/// Fail if `band` overlaps the loading screen's one centred label, which would
/// make the survey a measurement of glyphs.
///
/// Derived from the same expression the draw uses — `Origin::anchor` then the
/// `Align` shift, with the width from `text_px` — rather than from a restated
/// constant, for the reason [`assert_band_is_background`] gives.
fn assert_band_is_clear_of_loading_label(
    band: (u32, u32, u32, u32),
    label: &lodestone::menu::render::MenuLabel,
) {
    let (ax, ay) = label.origin.anchor(W as f32, H as f32);
    let tw = text_px(&label.text, label.scale);
    let x = match label.align {
        Align::Left => ax + label.dx,
        Align::Centre => (ax + label.dx - tw * 0.5).floor(),
        Align::Right => ax + label.dx - tw,
    };
    let y = ay + label.dy;
    // One line of vanilla's font at scale 1.0 is 9 logical px tall.
    let (lx0, ly0, lx1, ly1) = (x, y, x + tw, y + 9.0 * label.scale);
    let (bx0, by0, bx1, by1) = band;
    let (fx0, fy0, fx1, fy1) = (bx0 as f32, by0 as f32, bx1 as f32, by1 as f32);
    let overlaps = lx0 < fx1 && fx0 < lx1 && ly0 < fy1 && fy0 < ly1;
    assert!(
        !overlaps,
        "the probe band {band:?} overlaps the loading label at \
         ({lx0}, {ly0})..({lx1}, {ly1}) — re-derive the band rather than loosening \
         the assertion"
    );
}

/// The loading screen draws the panorama **and** `menu_background.png`'s wash over
/// it, where it used to draw a flat fill and no sky at all.
///
/// # The defect this replaces
///
/// `loading_frame` set `overlay: true`, and that one flag did two jobs: it chose
/// the translucent backdrop colour in `build`, *and* it was the only thing
/// suppressing the panorama in `MenuRenderer::draw`. So asking for a wash turned
/// the sky off, and the screen came out as a flat clear with a translucent quad on
/// it. No vanilla path produces that: `ConnectScreen` overrides no background and
/// takes the base `Screen.extractBackground` (panorama → blur → wash), and
/// `LevelLoadingScreen.extractBackground`'s `OTHER` arm calls `extractPanorama`
/// with no `minecraft.level == null` gate at all.
///
/// # Three hypotheses, and why the wrong two are the interesting ones
///
/// "Not a flat fill" would be satisfied by any change, so this predicts the
/// **value** of a washed pixel and requires the measurement to land on one of
/// three answers, all computed from outside constants:
///
/// | hypothesis | +Z face's 255 channels read | means |
/// |---|---|---|
/// | wash applied in **linear** space | **224** | correct — the shader multiplies a decoded texel and the sRGB target re-encodes |
/// | no wash at all | 255 | `dim_for_screen` treated the loading screen as the title screen |
/// | wash applied in **gamma** space | 191 | the multiply moved out of the shader onto a byte |
///
/// They are 30+ apart, so [`WASH_TOL`] cannot confuse them.
///
/// # The cross arm is what makes the wash claim mean something
///
/// The same band, the same synthetic cubemap and the same renderer are also shot
/// with the **title** frame, which must read the **raw** 255. One frame washed and
/// one frame not, measured together, is what distinguishes "the wash is applied to
/// the right screens" from "everything is dark".
#[test]
#[ignore = "requires a GPU adapter"]
fn the_loading_screen_draws_the_panorama_under_the_menu_background_wash() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut menu = MenuRenderer::new(device, format);

    menu.attach_panorama(device, queue, &synthetic_cubemap());
    assert!(
        menu.panorama_attached(),
        "the synthetic cubemap must be bound or every measurement below is of the \
         flat backdrop"
    );

    // The band is resolved against physical pixels, so this gate needs the auto
    // gui scale at 480×320 to be 1. A precondition with a verdict, not a skip.
    let (logical_w, logical_h) = logical_canvas(0, W, H);
    assert!(
        (logical_w - W as f32).abs() < 0.5 && (logical_h - H as f32).abs() < 0.5,
        "logical canvas is {logical_w}×{logical_h} against a {W}×{H} target, so the \
         band below is in the wrong coordinate space"
    );

    let loading = loading_frame("Joining world...");
    assert_eq!(
        loading.backdrop,
        MenuBackdrop::Panorama,
        "the loading screen must ask for the panorama; this is the whole fix"
    );
    assert!(
        !loading.logo,
        "and it must not be the title screen, which is the one screen \
         `dim_for_screen` gives no wash"
    );

    let band = straight_ahead_band();
    assert_band_is_background(band);
    assert_band_is_clear_of_loading_label(band, &loading.labels[0]);
    let total: u32 = (band.2 - band.0) * (band.3 - band.1);

    // Slot 0 is the correct hypothesis, so `count_candidates` reports *its*
    // bounding box — the "where", not just the "how much".
    let raw = FACE_COLOURS[LAYER_PLUS_Z];
    let hypotheses: [[f32; 3]; 3] = [
        [washed(raw[0]), washed(raw[1]), washed(raw[2])],
        [f32::from(raw[0]), f32::from(raw[1]), f32::from(raw[2])],
        [
            f32::from(raw[0]) * (1.0 - panorama::MENU_BACKGROUND_DIM),
            f32::from(raw[1]) * (1.0 - panorama::MENU_BACKGROUND_DIM),
            f32::from(raw[2]) * (1.0 - panorama::MENU_BACKGROUND_DIM),
        ],
    ];

    let mut shoot = |menu: &mut MenuRenderer, frame: &_| -> Vec<u8> {
        let acquired = target.acquire().expect("headless acquire");
        menu.render(device, queue, acquired.view(), frame, W, H);
        target.read_texels(device, queue)
    };

    eprintln!("=== loading screen wash gate ===");
    eprintln!("band {band:?} ({total} px), +Z face is {raw:?}");
    eprintln!(
        "hypotheses: washed-linear {:?}, unwashed {:?}, washed-gamma {:?}",
        hypotheses[0], hypotheses[1], hypotheses[2]
    );

    let mut mismatches: Vec<String> = Vec::new();

    // ---- the loading screen: panorama present, wash applied ------------------
    let shot = shoot(&mut menu, &loading);
    let (counts, bbox) = count_candidates(&shot, band, &hypotheses);
    eprintln!(
        "loading: washed-linear={} unwashed={} washed-gamma={} (washed bbox {bbox:?})",
        counts[0], counts[1], counts[2]
    );
    if counts[0] <= total / 4 {
        mismatches.push(format!(
            "loading: only {} of {total} px match the washed +Z face (bbox {bbox:?}); \
             unwashed={} washed-gamma={}. Zero across all three means the panorama is \
             not drawing at all, which is the flat-fill regression",
            counts[0], counts[1], counts[2]
        ));
    }
    if counts[1] != 0 {
        mismatches.push(format!(
            "loading: {} px are the *unwashed* face, so `dim_for_screen` gave this \
             screen the title screen's zero dim",
            counts[1]
        ));
    }
    if counts[2] != 0 {
        mismatches.push(format!(
            "loading: {} px match a gamma-space multiply, so the wash moved out of \
             the shader",
            counts[2]
        ));
    }

    // ---- the cross arm: the title screen is *not* washed --------------------
    let nav = MenuNav::with_path(std::env::temp_dir().join(format!(
        "lodestone-loading-wash-{}/servers.json",
        std::process::id()
    )));
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut favicons = FaviconCache::new();
    let ui = UiState::new();
    let title = frame_for(&ui, &nav, &statuses, &mut favicons).expect("the title screen draws");
    assert!(title.logo, "the cross arm must be the title screen");
    let title_shot = shoot(&mut menu, &title);
    let (t_counts, t_bbox) = count_candidates(&title_shot, band, &hypotheses);
    eprintln!(
        "title:   washed-linear={} unwashed={} washed-gamma={} (washed bbox {t_bbox:?})",
        t_counts[0], t_counts[1], t_counts[2]
    );
    if t_counts[1] <= total / 4 {
        mismatches.push(format!(
            "title: only {} of {total} px are the unwashed +Z face — the title screen \
             is the one screen with no wash, so if this arm is dark the wash is being \
             applied to everything and the loading arm above proves nothing",
            t_counts[1]
        ));
    }
    if t_counts[0] != 0 {
        mismatches.push(format!(
            "title: {} px are washed, at {t_bbox:?}",
            t_counts[0]
        ));
    }

    // ---- the control: detach, and the loading band must show no face -------
    menu.detach_panorama();
    assert!(!menu.panorama_attached(), "the control must actually detach");
    let control = shoot(&mut menu, &loading);
    let (c_counts, c_bbox) = count_candidates(&control, band, &hypotheses);
    eprintln!(
        "control: washed-linear={} unwashed={} washed-gamma={} (bbox {c_bbox:?})",
        c_counts[0], c_counts[1], c_counts[2]
    );
    if c_counts.iter().sum::<u32>() != 0 {
        mismatches.push(format!(
            "control: with the panorama detached the band still matches a face \
             hypothesis ({c_counts:?} at {c_bbox:?}) — the detector is measuring \
             something other than the panorama, so nothing above is evidence"
        ));
    }

    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

/// The loaded cubemap is vanilla's **real** art out of the asset-object store,
/// 1024×1024 and richly non-uniform — not `client.jar`'s 1×1 grey stubs.
///
/// No GPU: this measures the decoded, stacked RGBA that `attach_panorama` would
/// upload. It is the cheapest possible check that the object-store preference in
/// `panorama::load` is working, and it fails loudly rather than skipping when the
/// store is unpopulated, because a skip here would leave the flat-sky regression
/// completely undetected.
#[test]
#[ignore = "requires the vanilla pack with a populated asset-object store"]
fn the_panorama_loads_vanillas_real_faces_from_the_asset_object_store() {
    let faces = lodestone::resources::load_panorama().expect(
        "no panorama could be loaded at all; set LODESTONE_ASSETS to a pack root \
         holding client.jar + generated/reports/blocks.json",
    );

    eprintln!("=== panorama source gate ===");
    eprintln!("face size            = {}x{}", faces.size, faces.size);
    eprintln!("faces from store     = {}/6", faces.from_object_store);
    eprintln!("assembled RGBA bytes = {}", faces.rgba.len());

    assert_eq!(
        faces.from_object_store, 6,
        "only {} of 6 panorama faces came from the asset-object store; the rest \
         are client.jar's 69-byte 1x1 grey stubs, which render a flat sky that \
         looks like a working panorama. Run: \
         cargo run -p xtask -- fetch-assets --version 26.2",
        faces.from_object_store
    );
    assert!(faces.is_real_art());

    // Vanilla 26.2's faces are 1024x1024. Asserted as a floor plus squareness
    // rather than an equality, since a resource pack may legitimately differ —
    // but the stub's 1 must fail, and that is the case this exists for.
    assert!(
        faces.size >= 64,
        "a {}x{} panorama face is a stub, not art",
        faces.size,
        faces.size
    );
    assert_eq!(
        faces.rgba.len(),
        faces.layer_bytes() * 6,
        "the stacked buffer is not six whole layers"
    );

    // Non-uniformity, per layer. Predicted from the real files, measured with PIL
    // before this gate was written: luminance stdev per face is 3.7 (panorama_5,
    // the up face, the flattest) through 13.6 (panorama_4). A stub face is
    // *exactly* 0.0, so the floor below separates the two hypotheses by a wide
    // margin rather than testing the sign of a difference.
    const STDEV_FLOOR: f32 = 1.0;
    let layer = faces.layer_bytes();
    let mut flattest = f32::INFINITY;
    for index in 0..6 {
        let bytes = &faces.rgba[index * layer..(index + 1) * layer];
        let lum: Vec<f32> = bytes
            .chunks_exact(4)
            .map(|p| (f32::from(p[0]) + f32::from(p[1]) + f32::from(p[2])) / 3.0)
            .collect();
        let n = lum.len() as f32;
        let mean = lum.iter().sum::<f32>() / n;
        let stdev = (lum.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n).sqrt();
        flattest = flattest.min(stdev);
        eprintln!(
            "layer {index} ({:>2}) mean={mean:6.1} stdev={stdev:5.1}",
            panorama::FACE_SUFFIXES[index]
        );
        assert!(
            stdev > STDEV_FLOOR,
            "layer {index} (panorama{}) has luminance stdev {stdev:.3}, at or below \
             the {STDEV_FLOOR} floor — a uniform face means the stub was loaded \
             (a stub reads exactly 0.0; vanilla's flattest real face reads 3.7)",
            panorama::FACE_SUFFIXES[index]
        );
    }
    eprintln!("flattest layer stdev = {flattest:.2} (vanilla's is ~3.7)");

    // The detector's control: the same computation over a deliberately flat
    // buffer must report 0, which is what proves the assertions above are
    // measuring variation and not something every buffer has.
    let flat = vec![0x40u8; layer];
    let lum: Vec<f32> = flat
        .chunks_exact(4)
        .map(|p| (f32::from(p[0]) + f32::from(p[1]) + f32::from(p[2])) / 3.0)
        .collect();
    let n = lum.len() as f32;
    let mean = lum.iter().sum::<f32>() / n;
    let control = (lum.iter().map(|v| (v - mean) * (v - mean)).sum::<f32>() / n).sqrt();
    eprintln!("control (flat buffer) stdev = {control:.6}");
    assert!(
        control <= f32::EPSILON,
        "the stdev detector reports {control} for a uniform buffer, so it cannot \
         tell a stub from real art and every assertion above is vacuous"
    );
}

/// With the real cubemap bound, the rendered title screen is non-uniform — and
/// with it detached, it is exactly flat.
///
/// This is the pixel-level twin of the gate above and the one that actually
/// distinguishes *bound* from *unbound*: the backdrop `MenuRenderer` falls back to
/// is a single flat quad, so its luminance stdev is 0 by construction.
#[test]
#[ignore = "requires a GPU adapter and the vanilla pack with a populated object store"]
fn the_real_panorama_paints_a_non_uniform_sky_where_the_backdrop_is_flat() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut menu = MenuRenderer::new(device, format);

    let faces = lodestone::resources::load_panorama().expect("the vanilla panorama loads");
    menu.attach_panorama(device, queue, &faces);
    assert_eq!(
        menu.panorama_faces_from_object_store(),
        6,
        "this gate measures the real art; with client.jar's flat stubs bound the \
         sky is uniform and the assertion below would fail for the wrong reason. \
         Run: cargo run -p xtask -- fetch-assets --version 26.2"
    );

    let nav = MenuNav::with_path(std::env::temp_dir().join(format!(
        "lodestone-panorama-real-{}/servers.json",
        std::process::id()
    )));
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut favicons = FaviconCache::new();
    let ui = UiState::new();
    let frame = frame_for(&ui, &nav, &statuses, &mut favicons).expect("the title screen draws");

    let band = straight_ahead_band();
    assert_band_is_background(band);

    let mut shoot = |menu: &mut MenuRenderer| -> Vec<u8> {
        let acquired = target.acquire().expect("headless acquire");
        menu.render(device, queue, acquired.view(), &frame, W, H);
        target.read_texels(device, queue)
    };

    let real = shoot(&mut menu);
    let (mean, stdev, bbox) = luminance_spread(&real, band);
    eprintln!("=== real panorama pixel gate ===");
    eprintln!("band {band:?}");
    eprintln!("real  mean={mean:.2} stdev={stdev:.3} (darkest x,y / brightest x,y = {bbox:?})");

    menu.detach_panorama();
    let control = shoot(&mut menu);
    let (c_mean, c_stdev, c_bbox) = luminance_spread(&control, band);
    eprintln!("flat  mean={c_mean:.2} stdev={c_stdev:.3} (darkest/brightest = {c_bbox:?})");

    // The control first: if the flat backdrop is not flat, the whole comparison
    // is measuring something else (this is how a sky gate once "proved" a bare
    // first-person arm).
    assert!(
        c_stdev <= 0.51,
        "the panorama-less backdrop is not flat (stdev {c_stdev:.3} at {c_bbox:?}); \
         something else is painting this band, so the assertion below proves nothing"
    );
    assert!(
        stdev > 4.0 * c_stdev.max(0.25),
        "the real panorama's band has stdev {stdev:.3} against the flat backdrop's \
         {c_stdev:.3} at {bbox:?} — a bound real cubemap must be visibly varied. \
         Either the cubemap is not reaching pixels or the stub faces were loaded"
    );
    assert!(
        stdev > 1.0,
        "stdev {stdev:.3} in {band:?} (extremes at {bbox:?}) is stub-flat; vanilla's \
         flattest face measures 3.7 over the whole face"
    );
}

/// The spin actually accumulates through the renderer's own clock, and stays in
/// vanilla's range. No GPU: this is about the accumulator, not pixels.
#[test]
fn ten_seconds_of_real_time_turns_the_panorama_twenty_degrees() {
    // Predicted from constants that originate in vanilla, not read off the code:
    // `Panorama.java:27` is 0.1 deg per realtime tick at `panoramaSpeed` 1.0, and
    // a tick is 1/20 s, so 10 s is 10 * 20 * 1.0 * 0.1 = 20 deg. The plausible
    // wrong hypothesis — treating the delta as *seconds* rather than ticks — would
    // give 1 deg, which this band excludes by a factor of twenty.
    let per_second = panorama::SPIN_DEGREES_PER_TICK
        * panorama::TICKS_PER_SECOND
        * panorama::DEFAULT_SPIN_SPEED;
    assert!((per_second - 2.0).abs() < 1e-6, "{per_second} deg/s, expected 2");
    let over_ten_seconds = panorama::wrap_degrees(per_second * 10.0);
    assert!(
        (over_ten_seconds - 20.0).abs() < 1e-4,
        "{over_ten_seconds} deg after ten seconds, expected 20 (1 deg would mean \
         the realtime delta is being read as seconds, not ticks)"
    );
    // A full turn takes three minutes, which is the number to quote when someone
    // reports the panorama as static.
    let seconds_per_turn = 360.0 / per_second;
    assert!(
        (seconds_per_turn - 180.0).abs() < 1e-3,
        "a revolution takes {seconds_per_turn} s"
    );
}
