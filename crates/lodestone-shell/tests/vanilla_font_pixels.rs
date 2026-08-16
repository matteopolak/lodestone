//! Pixel gate: HUD text must reach the screen as **vanilla proportional
//! glyphs with a drop shadow**, not as the fixed-advance 5×7 debug font.
//!
//! # Why this cannot be a content assertion
//!
//! A font with every character correct and every *width* wrong satisfies
//! `assert_eq!` on the source string, on the glyph count, on the vertex count,
//! and on "did text draw". The defect is a property of the geometry between
//! glyphs, so it is only visible as distances between lit columns. That is what
//! this measures.
//!
//! # The two hypotheses, and the number that separates them
//!
//! Both are named as constants below so a reader can see which one the
//! assertion discriminates.
//!
//! * **Proportional** (vanilla): `i` advances 2 logical px, `W` advances 6. Ten
//!   of each at `scale = 2` therefore occupy visibly different widths, and the
//!   ratio of the two runs is ≈ [`PROPORTIONAL_RATIO`].
//! * **Fixed advance** (the debug font): every glyph advances 6 logical px, so
//!   the two runs are the *same* width and the ratio is ≈
//!   [`FIXED_ADVANCE_RATIO`] — barely above 1, because the only difference left
//!   is how much ink each glyph puts inside its identical cell.
//!
//! [`is_proportional`] is the assertion. It is applied to the subject **and to
//! the control**, and the control must fail it — that is the executed negative
//! control, not a description of one: the same renderer, the same frame, the
//! same measurement, with [`HudRenderer::detach_font`] restoring the debug font.
//!
//! Per-glyph advances are also read back individually: for a probe string
//! `"<c>W"`, the second glyph's ink starts exactly `advance(c) * scale` device
//! pixels along, because `W`'s ink begins at column 0 of its cell. That measures
//! `advance(c)` off the framebuffer, one character at a time, against vanilla's
//! published widths.
//!
//! # Colour space
//!
//! The target here is deliberately **`Rgba8Unorm`, not `Rgba8UnormSrgb`**, so the
//! HUD's colour floats land in the framebuffer verbatim and the shadow can be
//! asserted at its exact vanilla value. Vanilla's shadow is
//! `ARGB.scaleRGB(color, 0.25F)` — a *gamma-space* quarter — and the HUD's colour
//! convention is sRGB 0..1 written raw (`hud::legacy_rgb` divides vanilla's hex
//! codes by 255). On an sRGB target the same floats would be re-encoded on write
//! and a quarter would read back as ~54 %, which would make an exact assertion
//! impossible to state honestly.
//!
//! Fail-closed: no GPU adapter, or no `client.jar`, is a failure. In particular
//! [`HudRenderer::font_attached`] is asserted before anything is measured —
//! without that, a missing jar silently degrades to the debug font and every
//! "text drew something" assertion below would still pass.
//!
//! ```text
//! cargo test -p lodestone-shell --test vanilla_font_pixels -- --ignored --nocapture
//! ```

use lodestone::hud::{DebugStats, HudFrame, HudRenderer};
use lodestone_render::{GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 640;
const H: u32 = 480;

/// The HUD's chat scale, mirroring `hud::build_inner`.
const SCALE: f32 = 2.0;
/// The HUD's chat left margin, mirroring `hud::build_inner`.
const MARGIN: f32 = 6.0;
/// Chat line pitch: `(GLYPH_H + 2) * scale`.
const LINE_H: f32 = 18.0;

/// Ten `i` against ten `W`, with vanilla's 2 px and 6 px advances at
/// `scale = 2` and the pen starting at `MARGIN = 6`.
///
/// `W` steps 12 device px, so the tenth starts at `6 + 9*12 = 114` and inks its
/// 5 cell columns to 123; `i` steps 4, so the tenth starts at `6 + 9*4 = 42` and
/// inks its 1 column to 43. Both carry a `+2` px shadow, giving spans of
/// `6..=125` (120 px) and `6..=45` (40 px) — exactly `6 / 2`.
const PROPORTIONAL_RATIO: f32 = 3.0;

/// The same two runs under the fixed-advance 5×7 debug font: every glyph steps
/// 12 device px, so both runs occupy the same ten cells and differ only in where
/// the ink sits *inside* the cell.
///
/// `W`'s bitmap fills columns 0..=4, so its span is `6..=123` (118 px). `i`'s
/// bitmap is `0b00100 / 0b01100 / … / 0b01110` — columns 1..=3, inset by one on
/// the left — so its span is `8..=121` (114 px). The ratio is `118 / 114`, and
/// the reason it is not exactly 1.0 is that leftward inset, not any difference
/// in advance. **Measured, not predicted:** run the control and read it back.
const FIXED_ADVANCE_RATIO: f32 = 1.035;

/// The assertion under test, named so it can be applied to the control.
///
/// The band is ±8 %, which spans [`PROPORTIONAL_RATIO`] and excludes
/// [`FIXED_ADVANCE_RATIO`] by a factor of nearly three — comfortably wider than
/// any rasterisation slop and nowhere near the unfixed value.
fn is_proportional(ratio: f32) -> bool {
    (PROPORTIONAL_RATIO * 0.92..=PROPORTIONAL_RATIO * 1.08).contains(&ratio)
}

/// Vanilla's published advances for the probe characters, in logical pixels.
/// Hand-authored from outside this crate; see `lodestone-assets`'
/// `tests/vanilla_font_metrics.rs` for the same table's provenance.
const PROBE_ADVANCES: &[(char, f32)] = &[
    ('i', 2.0),
    ('l', 3.0),
    ('I', 4.0),
    ('t', 4.0),
    ('f', 5.0),
    ('W', 6.0),
    ('M', 6.0),
    ('~', 7.0),
];

/// Brightness above which a pixel is main text rather than its shadow. Chat's
/// base colour is `[0.92, 0.94, 1.0]`, so main text peaks at 255 and its 25 %
/// shadow at 64; 128 splits them with enormous margin.
const MAIN_MIN: u32 = 128;
/// Brightness above which a pixel counts as drawn at all (the backdrop is 0 and
/// chat's translucent backing is black over black, so it stays 0).
const LIT_MIN: u32 = 20;

fn brightness(px: &[u8], x: u32, y: u32) -> u32 {
    let i = ((y * W + x) * 4) as usize;
    u32::from(px[i].max(px[i + 1]).max(px[i + 2]))
}

/// The chat row band for the single line this gate draws: the line's top, less a
/// little headroom, through the bottom of its shadow.
fn band() -> std::ops::Range<u32> {
    let top = H as f32 - MARGIN - LINE_H;
    (top as u32 - 2)..(top as u32 + 24).min(H)
}

/// The columns of `band` holding a pixel at or above `min` brightness.
fn columns(px: &[u8], min: u32) -> Vec<u32> {
    let mut out = Vec::new();
    for x in 0..W {
        if band().any(|y| brightness(px, x, y) >= min) {
            out.push(x);
        }
    }
    out
}

/// `(first, last)` lit column of the row band, or `None` if nothing drew.
fn span(px: &[u8], min: u32) -> Option<(u32, u32)> {
    let cols = columns(px, min);
    Some((*cols.first()?, *cols.last()?))
}

/// Contiguous runs of `cols`.
fn runs(cols: &[u32]) -> Vec<(u32, u32)> {
    let mut out: Vec<(u32, u32)> = Vec::new();
    for &c in cols {
        match out.last_mut() {
            Some(last) if last.1 + 1 == c => last.1 = c,
            _ => out.push((c, c)),
        }
    }
    out
}

fn clear(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) {
    let mut enc = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
        label: Some("font-gate-clear"),
    });
    {
        let _p = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("font-gate-clear-pass"),
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

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn hud_text_draws_vanilla_proportional_glyphs_with_a_drop_shadow() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    // Linear, not sRGB: see the module docs on colour space.
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut hud = HudRenderer::new(device, format);

    assert!(
        hud.font_attached(),
        "the vanilla font must have loaded from client.jar; without it the HUD \
         silently falls back to the 5x7 debug font and every assertion below \
         would be measuring the thing this gate exists to reject. Set \
         LODESTONE_ASSETS to a pack root with client.jar + \
         generated/reports/blocks.json."
    );

    let stats = DebugStats::default();
    let mut shoot = |hud: &mut HudRenderer, line: &str| -> Vec<u8> {
        let chat = [(line, 0.0f32)];
        let frame = HudFrame {
            show_debug: false,
            crosshair: false,
            chat: &chat,
            ..HudFrame::new(&stats)
        };
        let acquired = target.acquire().expect("headless acquire");
        clear(device, queue, acquired.view());
        hud.render(device, queue, acquired.view(), acquired.view(), &frame, W, H);
        target.read_texels(device, queue)
    };

    // ---- 1. Proportional advance, measured as two run widths ---------------
    let narrow = shoot(&mut hud, "iiiiiiiiii");
    let wide = shoot(&mut hud, "WWWWWWWWWW");
    let (n0, n1) = span(&narrow, LIT_MIN).expect("the narrow run must draw something");
    let (w0, w1) = span(&wide, LIT_MIN).expect("the wide run must draw something");
    let narrow_w = (n1 - n0 + 1) as f32;
    let wide_w = (w1 - w0 + 1) as f32;
    let ratio = wide_w / narrow_w;

    eprintln!("=== vanilla font pixel gate ===");
    eprintln!("row band            = {:?}", band());
    eprintln!("10x'i' span         = {n0}..={n1}  ({narrow_w} px)");
    eprintln!("10x'W' span         = {w0}..={w1}  ({wide_w} px)");
    eprintln!("ratio               = {ratio:.3}");
    eprintln!("  proportional (vanilla)  = {PROPORTIONAL_RATIO:.3}");
    eprintln!("  fixed advance (5x7)     = {FIXED_ADVANCE_RATIO:.3}");

    // ---- 2. The drop shadow: an offset copy, not a blur --------------------
    // Every lit pixel is either main text or shadow; the shadow set must be
    // *exactly* the main set translated one logical pixel down-right, minus
    // whatever the main set covers. A blur would light the four other
    // neighbours too and fail this by inclusion.
    let off = SCALE as u32;
    let mut main = std::collections::HashSet::new();
    let mut shadow = std::collections::HashSet::new();
    let (bmin, bmax) = (band().start, band().end);
    for y in band() {
        for x in 0..W {
            let b = brightness(&wide, x, y);
            if b >= MAIN_MIN {
                main.insert((x, y));
            } else if b >= LIT_MIN {
                shadow.insert((x, y));
            }
        }
    }
    let expected: std::collections::HashSet<(u32, u32)> = main
        .iter()
        .map(|&(x, y)| (x + off, y + off))
        // Clip to the same window the observation was taken through, so the
        // comparison is not decided by the band's edge.
        .filter(|&(x, y)| x < W && (bmin..bmax).contains(&y))
        .filter(|p| !main.contains(p))
        .collect();

    let shadow_only: Vec<_> = shadow.difference(&expected).take(4).collect();
    let missing: Vec<_> = expected.difference(&shadow).take(4).collect();
    eprintln!("main px             = {}", main.len());
    eprintln!("shadow px           = {} (expected {})", shadow.len(), expected.len());

    // Brightness: vanilla's shadow is a quarter of the text colour. Chat's base
    // is [0.92, 0.94, 1.0] -> peak 255; a quarter is 63..64.
    let main_peak = main
        .iter()
        .map(|&(x, y)| brightness(&wide, x, y))
        .max()
        .expect("main text must exist");
    let shadow_peak = shadow
        .iter()
        .map(|&(x, y)| brightness(&wide, x, y))
        .max()
        .expect("a shadow must exist");
    eprintln!("main peak           = {main_peak}");
    eprintln!("shadow peak         = {shadow_peak} (vanilla: 0.25 * {main_peak} = {:.0})", f64::from(main_peak) * 0.25);

    // ---- 3. Per-glyph advances, read back one character at a time ----------
    // "<c>W": W's ink starts at column 0 of its cell, so the second run of
    // *main* pixels begins exactly advance(c) * SCALE px after the first.
    let mut advance_rows = Vec::new();
    for &(ch, want) in PROBE_ADVANCES {
        let px = shoot(&mut hud, &format!("{ch}W"));
        let groups = runs(&columns(&px, MAIN_MIN));
        assert!(
            groups.len() >= 2,
            "{ch:?}W must draw two separated ink runs; got {groups:?}. If they \
             merged, the first glyph is inking past its advance"
        );
        let measured = (groups[1].0 as f32 - MARGIN) / SCALE;
        advance_rows.push((ch, want, measured, groups[0], groups[1]));
    }
    eprintln!("--- per-glyph advance, measured off the framebuffer ---");
    for (ch, want, got, g0, g1) in &advance_rows {
        eprintln!("  {ch:?}: want {want} logical px, measured {got}  (runs {g0:?} {g1:?})");
    }

    // ---- 4. The executed negative control ----------------------------------
    // Same renderer, same frames, debug font restored.
    hud.detach_font();
    assert!(!hud.font_attached());
    let c_narrow = shoot(&mut hud, "iiiiiiiiii");
    let c_wide = shoot(&mut hud, "WWWWWWWWWW");
    let (cn0, cn1) = span(&c_narrow, LIT_MIN).expect("the control must still draw");
    let (cw0, cw1) = span(&c_wide, LIT_MIN).expect("the control must still draw");
    let c_ratio = (cw1 - cw0 + 1) as f32 / (cn1 - cn0 + 1) as f32;
    let c_shadowless = {
        let mut any = false;
        for y in band() {
            for x in 0..W {
                let b = brightness(&c_wide, x, y);
                if b >= LIT_MIN && b < MAIN_MIN {
                    any = true;
                }
            }
        }
        !any
    };
    eprintln!("--- control: debug font restored ---");
    eprintln!("10x'i' span         = {cn0}..={cn1}  ({} px)", cn1 - cn0 + 1);
    eprintln!("10x'W' span         = {cw0}..={cw1}  ({} px)", cw1 - cw0 + 1);
    eprintln!("ratio               = {c_ratio:.3}");
    eprintln!("is_proportional(subject) = {}", is_proportional(ratio));
    eprintln!("is_proportional(control) = {}", is_proportional(c_ratio));
    eprintln!("control has no shadow    = {c_shadowless}");

    // ================= assertions =================
    assert!(
        is_proportional(ratio),
        "ten 'i' and ten 'W' must occupy visibly different widths: got {narrow_w} \
         and {wide_w} px, ratio {ratio:.3}. The proportional hypothesis predicts \
         {PROPORTIONAL_RATIO:.3}; a fixed 6 px advance predicts \
         {FIXED_ADVANCE_RATIO:.3}, which is what an unfixed build produces"
    );
    assert!(
        narrow_w < wide_w * 0.5,
        "the narrow run must be dramatically shorter, not merely shorter: \
         {narrow_w} vs {wide_w}"
    );

    assert!(
        shadow_only.is_empty() && missing.is_empty(),
        "the shadow must be exactly the text translated (+{off}, +{off}) device \
         px and nothing else — a blur would light the other neighbours too. \
         Unexpected shadow pixels: {shadow_only:?}; missing: {missing:?}"
    );
    assert!(
        !shadow.is_empty(),
        "there must be a shadow at all; an empty shadow set would make the set \
         equality above vacuously true"
    );
    assert_eq!(main_peak, 255, "chat text peaks at its full colour");
    assert!(
        (60..=68).contains(&shadow_peak),
        "vanilla's shadow is ARGB.scaleRGB(color, 0.25) = 63/255 of the text \
         colour in gamma space; got {shadow_peak}. ~137 means the quarter was \
         taken in linear space and the shadow will read as grey"
    );

    for (ch, want, got, _, _) in &advance_rows {
        assert_eq!(
            got, want,
            "{ch:?} must advance {want} logical px like vanilla; measured {got}. \
             A fixed-advance font measures 6 for every one of these"
        );
    }

    // The control, observed firing.
    assert!(
        !is_proportional(c_ratio),
        "NEGATIVE CONTROL DID NOT FIRE: with the debug font restored the \
         proportional assertion still passed (ratio {c_ratio:.3}). That means \
         the measurement above is not sensitive to the defect it claims to catch"
    );
    assert!(
        (FIXED_ADVANCE_RATIO * 0.9..=FIXED_ADVANCE_RATIO * 1.1).contains(&c_ratio),
        "the control should land on the fixed-advance prediction \
         {FIXED_ADVANCE_RATIO:.3}; got {c_ratio:.3}"
    );
    assert!(
        c_shadowless,
        "the debug font draws no shadow, so the control frame must contain no \
         pixel between {LIT_MIN} and {MAIN_MIN} brightness; one there would mean \
         the shadow assertion above is measuring something else"
    );
}
