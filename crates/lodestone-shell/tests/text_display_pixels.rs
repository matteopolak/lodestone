//! Pixel gate: a `text_display` entity must **draw**, in its own screen
//! rect, through the real [`RenderState::render`] path — the same call
//! `app.rs`'s frame loop makes. This is the control CLAUDE.md's evidence
//! standards require and the predecessor's own `lodestone_render::display`
//! module explicitly could not provide: six passing unit tests and zero
//! producers is exactly the island shape a gate at this layer alone can
//! certify as fine, so this test renders through the **entire** chain —
//! `DisplayDraw` → `RenderState::set_display_draws` →
//! `gpu/display_text.rs::DisplayTextRenderer` → real pixels — rather than
//! stopping at the geometry module.
//!
//! # Why the expected rect is a generous padded box, not a projected quad
//!
//! Unlike `beacon_beam_pixels.rs` (which projects `beacon_beam_vertices`'
//! own output through the camera), computing the *exact* glyph rect here
//! would need the jar-sourced `RasterFont` this test's own crate boundary
//! cannot reach (`super::nametag::load_font` is `pub(super)` inside
//! `crate::gpu`, not visible from `tests/`). The gate settles for a coarse,
//! generously-padded box around the entity's own screen-projected anchor —
//! it is a control asking "did anything reach the screen near where this
//! entity is", not a registration check.
//!
//! # The billboard-mode discriminating pair lives one layer down
//!
//! CLAUDE.md's evidence standards call for two camera angles to tell a
//! billboard from a fixed quad. That pair is exercised twice already —
//! `lodestone_render::display`'s own unit tests (pure orientation
//! geometry) and `gpu/display_text.rs`'s own unit tests (real vertex output
//! from the exact function this pass calls, `push_text_display_quads`) —
//! both watched to fail under a neutered orientation function before being
//! restored. A pixel-level version of the same pair was attempted here and
//! removed: comparing two full-frame shots at different camera *yaws* (the
//! only way to isolate orientation without also moving the entity's own
//! screen registration, which a camera *position* change does regardless of
//! billboard mode) still confounds the object's perspective foreshortening
//! with its billboard re-orientation in a way that does not cleanly
//! discriminate the two by raw pixel-diff magnitude — measured, not
//! assumed: a first version asserted `Center`'s diff would exceed `Fixed`'s
//! and the real numbers came out the other way round (`fixed_diff=3090`,
//! `center_diff=2768`), which is the "control whose premise can be false"
//! failure mode CLAUDE.md warns about, caught by actually running it rather
//! than reasoning about it. The vertex-level control is exact and does not
//! have this problem, so that is where this repo's own evidence standard is
//! satisfied for this claim.
//!
//! ```text
//! cargo test -p lodestone-shell --test text_display_pixels -- --ignored --nocapture
//! ```

use glam::Vec3;
use lodestone::display_entities::{DisplayDraw, TEXT_DISPLAY_TYPE_PATH};
use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::display::{BillboardMode, DisplayTransformation};
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;
const NON_SKY: i32 = 60;

fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}

fn is_non_sky(px: &[u8], sky: [u8; 3]) -> bool {
    let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
        + (i32::from(px[1]) - i32::from(sky[1])).abs()
        + (i32::from(px[2]) - i32::from(sky[2])).abs();
    d > NON_SKY
}

#[derive(Debug, Clone, Copy)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    fn contains(self, x: u32, y: u32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }
}

fn non_sky_in(pixels: &[u8], rect: Rect, sky: [u8; 3]) -> usize {
    let mut n = 0;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        if rect.contains(x, y) && is_non_sky(px, sky) {
            n += 1;
        }
    }
    n
}

fn non_sky_bbox(pixels: &[u8], sky: [u8; 3]) -> Option<Rect> {
    let mut r: Option<Rect> = None;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        if !is_non_sky(px, sky) {
            continue;
        }
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        r = Some(match r {
            None => Rect { x0: x, y0: y, x1: x, y1: y },
            Some(p) => Rect {
                x0: p.x0.min(x),
                y0: p.y0.min(y),
                x1: p.x1.max(x),
                y1: p.y1.max(y),
            },
        });
    }
    r
}

fn project(view_proj: glam::Mat4, world: Vec3) -> (f32, f32) {
    let clip = view_proj * glam::Vec4::new(world.x, world.y, world.z, 1.0);
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;
    (
        (ndc_x * 0.5 + 0.5) * W as f32,
        (1.0 - (ndc_y * 0.5 + 0.5)) * H as f32,
    )
}

/// Eye at the origin looking straight down `+Z` (yaw 0 faces `+Z`, the same
/// convention `beacon_beam_pixels.rs`/`bell_block_entity_pixels.rs` use),
/// close enough that the display panel fills a large, easily-measured
/// fraction of the frame.
fn camera() -> Camera {
    Camera {
        position: Vec3::new(0.5, 1.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

/// `Center` billboard: always faces the camera regardless of viewing angle,
/// which is what makes a coarse, angle-independent expected rect valid —
/// the panel's screen footprint does not rotate away as the camera moves.
fn test_draw() -> DisplayDraw {
    DisplayDraw {
        id: 1,
        type_path: TEXT_DISPLAY_TYPE_PATH,
        position: Vec3::new(0.5, 1.0, 3.0),
        entity_yaw: 0.0,
        entity_pitch: 0.0,
        billboard: BillboardMode::Center,
        transform: DisplayTransformation::default(),
        text: Some(lodestone_model::text::Text::literal("LODESTONE")),
        text_line_width: 200,
        // Vanilla's own default background (translucent black) — non-zero,
        // so the panel draws even where the (thin, jar-font-dependent) glyph
        // strokes might miss a coarse rect.
        text_background_color: 0x4000_0000_u32 as i32,
        text_opacity: -1,
        text_style_flags: 0,
        block_state: None,
        item: None,
        item_display_context: 0,
    }
}

fn gpu() -> GpuContext {
    GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    )
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_text_display_draws_in_its_own_screen_rect() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();
    let draw = test_draw();

    let (anchor_x, anchor_y) = project(camera.view_projection(), draw.position);
    let rect = Rect {
        x0: (anchor_x as i32 - 70).max(0) as u32,
        y0: (anchor_y as i32 - 70).max(0) as u32,
        x1: ((anchor_x as i32 + 70) as u32).min(W - 1),
        y1: ((anchor_y as i32 + 60) as u32).min(H - 1),
    };

    let mut shoot = |install: bool| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        if install {
            state.set_display_draws(vec![draw.clone()]);
        }
        let frame = target.acquire().expect("headless acquire");
        let _stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let subject_px = shoot(true);
    let control_px = shoot(false);
    let sky = sky_bytes();

    // The control — RenderState::new with no display draws installed —
    // must paint nothing inside the entity's own rect. This is the "watched
    // fail" half of the gate: with `RenderState::display_text.draw` briefly
    // commented out during development, *both* subject and control read
    // zero here and the assertion below (`subject_in_rect > 0`) went red,
    // proving this detector can actually see an island. Restoring the call
    // made it green again.
    let control_in_rect = non_sky_in(&control_px, rect, sky);
    assert_eq!(
        control_in_rect, 0,
        "the control (no text_display installed) paints {control_in_rect} px \
         inside the entity's own rect {rect:?} — something else draws there. \
         Control's non-sky bbox: {:?}",
        non_sky_bbox(&control_px, sky)
    );

    let subject_in_rect = non_sky_in(&subject_px, rect, sky);
    assert!(
        subject_in_rect > 100,
        "installing a text_display changed only {subject_in_rect} px inside \
         its own screen rect {rect:?} — the entity did not reach the screen. \
         Subject's non-sky bbox: {:?}",
        non_sky_bbox(&subject_px, sky)
    );
}
