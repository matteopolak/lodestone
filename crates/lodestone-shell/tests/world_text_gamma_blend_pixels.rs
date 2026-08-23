//! Pixel gate: the world's flat-colour text passes composite on **raw gamma
//! bytes**, the way vanilla does, at production's own surface format.
//!
//! # What is being asserted, and why no other gate here can see it
//!
//! `gpu/nametag.rs`, `gpu/sign_text.rs` and `gpu/display_text.rs` share one
//! shader (`shaders/nametag.wgsl`: flat vertex colour, no texture), so every
//! colour they submit is a vanilla gamma byte — a nametag's background plate at
//! `ARGB.color(0.25F, -16777216)`, a sign's `ARGB.scaleRGB(dye, 0.4)`, a
//! `text_display` panel. Vanilla is not colour-managed and blends those
//! directly on the framebuffer's stored bytes. Every pipeline in this crate
//! targets the swapchain's *sRGB* view, so without the fix the hardware decodes
//! the destination, blends in linear light and re-encodes, and the plate reads
//! **too weak against a bright backdrop**.
//!
//! Every other world-text gate in this directory (`nametag_pixels`,
//! `sign_text_pixels`, `text_display_pixels`, `world_text_over_geometry_pixels`)
//! builds an `Rgba8Unorm` target, where `RenderTarget::format` and
//! `raw_view_format` are the *same format*: the blend there was always on gamma
//! bytes, so the whole corpus is blind to this by construction — the shared
//! fixture-value blindness `CLAUDE.md` records, with the target format as the
//! shared value. This gate uses `Bgra8UnormSrgb`, which is what native
//! `wgpu-core`'s `Surface::get_default_config` actually picks, and asserts that
//! the two formats differ rather than assuming it.
//!
//! # The two hypotheses, both computed from outside this renderer
//!
//! The subject is the darkest pixel the tag adds to the frame: the background
//! plate over open sky, with no glyph ink in it. The plate is black at
//! `64/255`, so over a stored backdrop byte `b`:
//!
//! * **vanilla / raw view**: `b · (1 − 64/255)` — plain interpolation on the
//!   stored bytes, predictable exactly.
//! * **sRGB view (the bug)**: `encode(decode(b) · (1 − 64/255))`, which is
//!   strictly larger for every `b > 0`. Predicted only as a *bracket*: this
//!   codebase has measured `ALPHA_BLENDING` on Metal as a real but non-trivial
//!   function of the fragment alpha byte that resists a closed form, so an
//!   exact prediction on that arm would be fitted rather than derived.
//!
//! `b` is read out of the rendered control frame rather than recomputed from
//! `SKY_COLOR`, so a change to the sky's clear colour cannot silently make both
//! arms agree.
//!
//! **What this does not prove.** The target is a `HeadlessTarget`, not a
//! swapchain, so this gate covers `RenderState`'s half — that it pairs
//! re-pointed pipelines with a matching view. That `SurfaceTarget` reports the
//! same format pair and declares both in `view_formats` is
//! `lodestone_render::target`'s own claim, gated there.
//!
//! ```text
//! cargo test -p lodestone-shell --test world_text_gamma_blend_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::{EntityDraw, NameTag};
use lodestone::gpu::RenderState;
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// The plate's alpha, straight from `gpu/nametag.rs`'s `BACKGROUND_ARGB`
/// (`0x40000000` — black at vanilla's `getBackgroundOpacity` fallback of
/// `0.25`, rounded by `ARGB.as8BitChannel`). Restated here rather than imported
/// because the constant is private to that module; if the two ever disagree the
/// subject arm's exact prediction is what goes red.
const PLATE_ALPHA: f32 = 64.0 / 255.0;

/// The sRGB EOTF, byte in → linear out. IEC 61966-2-1, transcribed from the
/// standard rather than from any of this repo's own shaders — the expectation
/// has to come from outside the code under test.
fn decode(byte: u8) -> f32 {
    let c = f32::from(byte) / 255.0;
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

/// The inverse, linear in → byte out.
fn encode(linear: f32) -> u8 {
    let c = if linear <= 0.003_130_8 {
        linear * 12.92
    } else {
        1.055 * linear.powf(1.0 / 2.4) - 0.055
    };
    (c.clamp(0.0, 1.0) * 255.0).round() as u8
}

fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 1.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

fn base_draw(feet: glam::Vec3) -> EntityDraw {
    EntityDraw {
        hurt: false,
        block_state: None,
        item_frame_rotation: 0,
        id: 1,
        type_path: std::sync::Arc::from("pig"),
        item: None,
        main_arm_left: false,
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_trim: Vec::new(),
        feet,
        yaw: 0.0,
        head_yaw: 0.0,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput::REST,
        wool: None,
        count: 1,
        foil: false,
        item_dyed_color: None,
        item_potion_color: None,
        name_tag: None,
        item_use: None,
        creeper_swelling: 0.0,
        swim_amount: 0.0,
        death_time: 0.0,
        on_fire: false,
        invisible: false,
        armor_stand: None,
        player_skin: None,
        variant_sheet: None,
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
        painting: None,
        firework: None,
    }
}

/// `Bgra8UnormSrgb` read-back is B, G, R, A. The plate multiplies every channel
/// by the same factor, so one channel settles the question; green is the one
/// with the most headroom in `SKY_COLOR` and therefore the largest absolute gap
/// between the two hypotheses.
fn green(px: &[u8]) -> u8 {
    px[1]
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_nametag_plate_blends_on_gamma_bytes_at_the_surface_format() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    // Native's own swapchain format, so `format()` and `raw_view_format()`
    // genuinely differ and this gate has something to compare.
    let format = wgpu::TextureFormat::Bgra8UnormSrgb;
    let mut target = HeadlessTarget::new(device, W, H, format);
    assert_ne!(
        target.format(),
        target.raw_view_format(),
        "this gate is vacuous unless the corrected and raw formats differ — every other \
         world-text gate here uses Rgba8Unorm, where they coincide"
    );

    let cam = camera();
    let feet = glam::Vec3::new(0.0, 0.0, 6.0);
    let subject = EntityDraw {
        name_tag: Some(NameTag {
            text: lodestone_model::text::Text::literal("Babe"),
            // Not discrete, so the plate travels with the see-through
            // submission — see `gpu/nametag.rs`'s module doc.
            see_through: true,
        }),
        ..base_draw(feet)
    };
    let control = EntityDraw { name_tag: None, ..subject.clone() };

    // `gamma` picks which `(pipeline format, attachment view)` pair the world's
    // text passes get. `true` is production's, installed exactly the way
    // `app/redraw.rs` installs it; `false` is the pairing production had before
    // this fix, kept as the control that must land on the *other* hypothesis
    // rather than merely somewhere else.
    let mut shoot = |draw: &EntityDraw, gamma: bool| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        let frame = target.acquire().expect("headless acquire");
        if gamma {
            state.set_world_text_view(device, &frame);
            assert_eq!(
                state.world_text_format(),
                target.raw_view_format(),
                "set_world_text_view must re-point the text pipelines at the raw view's \
                 format, or the pass below is drawing through the pairing it replaced"
            );
        } else {
            assert_eq!(
                state.world_text_format(),
                format,
                "the control arm must keep the target's own (sRGB) format — that is the \
                 pairing being reproduced"
            );
        }
        state.render(device, queue, frame.view(), &cam, None, std::slice::from_ref(draw));
        drop(frame);
        target.read_texels(device, queue)
    };

    let control_px = shoot(&control, true);
    let raw_px = shoot(&subject, true);
    let srgb_px = shoot(&subject, false);

    // The backdrop the plate actually lands on, read out of the rendered frame
    // rather than recomputed from `SKY_COLOR` — a clear-colour change must not
    // be able to make both hypotheses agree behind this gate's back.
    let sky = green(&control_px[..4]);

    // Every pixel the tag adds, in each arm, and the darkest of them: the plate
    // over open sky with no glyph ink in it. Collected rather than reduced
    // inside an early-exiting loop so a failure can print what it measured.
    let darkest = |arm: &[u8]| -> Option<(u8, usize)> {
        let mut lo: Option<u8> = None;
        let mut n = 0usize;
        for (a, c) in arm.chunks_exact(4).zip(control_px.chunks_exact(4)) {
            if green(a) == green(c) && a[0] == c[0] && a[2] == c[2] {
                continue;
            }
            n += 1;
            lo = Some(lo.map_or(green(a), |v| v.min(green(a))));
        }
        lo.map(|v| (v, n))
    };

    let (raw_min, raw_n) = darkest(&raw_px).unwrap_or((0, 0));
    let (srgb_min, srgb_n) = darkest(&srgb_px).unwrap_or((0, 0));

    let vanilla = (f32::from(sky) * (1.0 - PLATE_ALPHA)).round() as i32;
    let linear = i32::from(encode(decode(sky) * (1.0 - PLATE_ALPHA)));

    eprintln!("=== world text plate blend at {format:?} ===");
    eprintln!("sky backdrop byte (G)        = {sky}");
    eprintln!("hypothesis A, vanilla gamma  = {vanilla}");
    eprintln!("hypothesis B, linear blend   = {linear}");
    eprintln!("raw-view  arm: darkest = {raw_min}  pixels changed = {raw_n}");
    eprintln!("srgb-view arm: darkest = {srgb_min}  pixels changed = {srgb_n}");

    // Premises, each of which would otherwise let an arm pass vacuously.
    assert!(
        sky > 40,
        "the backdrop must be bright enough for the two hypotheses to separate at all; \
         they share a fixed point at black. Measured sky byte {sky}"
    );
    assert!(
        raw_n > 0 && srgb_n > 0,
        "the tag must reach pixels in both arms — a missing client.jar loads no font and \
         `push_entity_quads` then emits no plate at all: raw {raw_n}, srgb {srgb_n}"
    );
    assert!(
        (vanilla - linear).abs() > 5,
        "the two hypotheses must be far enough apart to be told apart at this backdrop: \
         vanilla {vanilla}, linear {linear}"
    );

    // 1) Production's pairing reproduces vanilla's own blend to the byte. Raw
    //    alpha compositing is plain interpolation, so this one is exact.
    assert!(
        (i32::from(raw_min) - vanilla).abs() <= 2,
        "the raw-view pairing must reproduce vanilla's gamma-byte blend of the plate over \
         the sky: predicted {vanilla}, got {raw_min}"
    );
    // 2) And the pairing this replaced must land on the *other* hypothesis —
    //    bracketed rather than predicted, because the exact composite through
    //    `ALPHA_BLENDING` on an sRGB attachment is not a closed form here.
    //    Without this the first assertion would pass for a pipeline that is
    //    simply indifferent to its attachment's format.
    assert!(
        i32::from(srgb_min) > vanilla + 5,
        "the sRGB-view pairing must still come out markedly weaker than vanilla's plate \
         (this is the bug being fixed, reproduced live): vanilla {vanilla}, linear \
         hypothesis {linear}, got {srgb_min}"
    );
}
