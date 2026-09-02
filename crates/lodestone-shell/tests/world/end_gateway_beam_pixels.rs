//! Pixel gate: the end gateway's teleport beam must **draw**, in its own
//! screen rect, through the real [`RenderState::render`] path — the same
//! call `app.rs`'s frame loop makes.
//!
//! Closes the verification gap `docs/block-entity-renderers.md`'s
//! "A note on verification scope" names: the beam landed with real
//! CPU-side unit tests (the widened `push_beam_section`, the beacon's own
//! eight `beacon::tests` staying green as a no-op-for-beacon check) and a
//! real wiring chain, but no `#[ignore]`d GPU pixel gate — unlike every
//! other translucent pass in this corpus (the beacon beam itself), which
//! has one.
//!
//! # Reuses the beacon's pipeline objects, not a parallel pass
//!
//! `gpu/beacon_beam.rs::BeaconBeamRenderer::prepare_gateway`/
//! `draw_gateway_solid`/`draw_gateway_glow` draw through the identical
//! `solid_pipeline`/`glow_pipeline` [`beacon_beam_pixels.rs`] already proves
//! — a `wgpu::RenderPipeline` embeds no texture data, only a bind-group-
//! layout contract, so only a **second** texture bind group and a second,
//! smaller pair of vertex buffers are new. That is exactly why this gate
//! cannot simply trust the beacon's own tests: the gateway's draw calls,
//! its texture (`end_gateway_beam.png`) and its vertex buffers are all
//! genuinely different code paths that could independently be wired to
//! nothing, and `CLAUDE.md`'s `ALPHA_BLENDING` rule (a direction-only
//! assertion cannot tell a real blend from a hard discard-then-overwrite)
//! applies to this draw call specifically, not by inheritance from the
//! beacon's.
//!
//! # The rect comes from the real geometry function, not a baked mesh
//!
//! Like `beacon_beam_pixels.rs`, the expected rects project
//! [`end_gateway_beam_vertices`]'s own output through the same
//! [`Camera::view_projection`] `gpu/beacon_beam.rs::prepare_gateway` feeds
//! into the same function.
//!
//! # Two known animation phases, not a round delta
//!
//! `CLAUDE.md`'s rendering-constraints section: the solid core's cross-
//! section rotates by `animation_time * 2.25 - 45` degrees
//! (`push_beam_section`) — a real geometry change, not a shader-only
//! effect. A rotation delta that is a multiple of `90°` would alias against
//! the core's own 4-fold symmetry (a rotated square looks the same every
//! quarter turn), so the two `animation_time` values here are chosen so
//! `(t1 - t0) * 2.25` is **not** a multiple of 90 — `0.0` and `8.0` give a
//! `18°` turn.
//!
//! ```text
//! cargo test -p lodestone-shell --test end_gateway_beam_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{
    BeamVertex, Camera, EndGatewayBeamSpawn, GpuContext, HeadlessTarget, RenderTarget,
    end_gateway_beam_vertices,
};

const W: u32 = 320;
const H: u32 = 240;

/// The gateway's block position, three blocks ahead of the camera on `+Z`.
const GATEWAY: [i32; 3] = [0, 0, 3];

/// `Mth.floor(scale * beamDistance)` for a fully spawned/cooling-down beam —
/// chosen generously tall so the beam fills a real fraction of the frame at
/// this test's camera distance, unlike the beacon's own (visually infinite)
/// column.
const HEIGHT: i32 = 6;

/// Magenta-ish, gamma-space `0x00RRGGBB` — the same shape
/// `DyeColor::Magenta.packed_rgb()` produces for the spawning arm, without
/// importing the enum for one literal.
const COLOR: u32 = 0x00FF_20FF;

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

#[derive(Debug, Clone, Copy, PartialEq)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

impl Rect {
    fn area(self) -> usize {
        ((self.x1 - self.x0 + 1) as usize) * ((self.y1 - self.y0 + 1) as usize)
    }

    fn contains(self, x: u32, y: u32) -> bool {
        x >= self.x0 && x <= self.x1 && y >= self.y0 && y <= self.y1
    }

    fn padded(self, pad: u32) -> Rect {
        Rect {
            x0: self.x0.saturating_sub(pad),
            y0: self.y0.saturating_sub(pad),
            x1: (self.x1 + pad).min(W - 1),
            y1: (self.y1 + pad).min(H - 1),
        }
    }

    fn intersects(self, other: Rect) -> bool {
        self.x0 <= other.x1 && other.x0 <= self.x1 && self.y0 <= other.y1 && other.y0 <= self.y1
    }
}

fn bbox_of(pixels: &[u8], predicate: impl Fn(&[u8]) -> bool) -> Option<(Rect, usize)> {
    let mut rect: Option<Rect> = None;
    let mut count = 0usize;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        if !predicate(px) {
            continue;
        }
        count += 1;
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        rect = Some(match rect {
            None => Rect { x0: x, y0: y, x1: x, y1: y },
            Some(r) => Rect {
                x0: r.x0.min(x),
                y0: r.y0.min(y),
                x1: r.x1.max(x),
                y1: r.y1.max(y),
            },
        });
    }
    rect.map(|r| (r, count))
}

fn changed_bbox(a: &[u8], b: &[u8]) -> Option<(Rect, usize)> {
    let mut rect: Option<Rect> = None;
    let mut count = 0usize;
    for (i, (pa, pb)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
        let d = (i32::from(pa[0]) - i32::from(pb[0])).abs()
            + (i32::from(pa[1]) - i32::from(pb[1])).abs()
            + (i32::from(pa[2]) - i32::from(pb[2])).abs();
        if d <= 12 {
            continue;
        }
        count += 1;
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        rect = Some(match rect {
            None => Rect { x0: x, y0: y, x1: x, y1: y },
            Some(r) => Rect {
                x0: r.x0.min(x),
                y0: r.y0.min(y),
                x1: r.x1.max(x),
                y1: r.y1.max(y),
            },
        });
    }
    rect.map(|r| (r, count))
}

fn diff_count(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(pa, pb)| {
            let d = (i32::from(pa[0]) - i32::from(pb[0])).abs()
                + (i32::from(pa[1]) - i32::from(pb[1])).abs()
                + (i32::from(pa[2]) - i32::from(pb[2])).abs();
            d > 12
        })
        .count()
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

fn project(view_proj: glam::Mat4, world: glam::Vec3) -> (f32, f32) {
    let clip = view_proj * glam::Vec4::new(world.x, world.y, world.z, 1.0);
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;
    (
        (ndc_x * 0.5 + 0.5) * W as f32,
        (1.0 - (ndc_y * 0.5 + 0.5)) * H as f32,
    )
}

fn test_spawn(animation_time: f32) -> EndGatewayBeamSpawn {
    EndGatewayBeamSpawn {
        pos: GATEWAY,
        scale: 1.0,
        animation_time,
        height: HEIGHT,
        color: COLOR,
    }
}

/// Projects [`end_gateway_beam_vertices`]'s own output through `view_proj` —
/// the exact function `gpu/beacon_beam.rs::prepare_gateway` calls, never a
/// remembered literal. Returns `(solid_rect, glow_rect)`.
fn expected_rects(spawn: &EndGatewayBeamSpawn, view_proj: glam::Mat4) -> (Rect, Rect) {
    let (solid, glow) = end_gateway_beam_vertices(
        spawn.pos,
        spawn.scale,
        spawn.animation_time,
        spawn.height,
        spawn.color,
    );
    let rect_of = |verts: &[BeamVertex]| -> Rect {
        let mut min = (f32::MAX, f32::MAX);
        let mut max = (f32::MIN, f32::MIN);
        for v in verts {
            let (sx, sy) = project(view_proj, glam::Vec3::from(v.position));
            if sx.is_finite() && sy.is_finite() {
                let cx = sx.clamp(0.0, (W - 1) as f32);
                let cy = sy.clamp(0.0, (H - 1) as f32);
                min = (min.0.min(cx), min.1.min(cy));
                max = (max.0.max(cx), max.1.max(cy));
            }
        }
        Rect {
            x0: min.0.floor() as u32,
            y0: min.1.floor() as u32,
            x1: max.0.ceil().min((W - 1) as f32) as u32,
            y1: max.1.ceil().min((H - 1) as f32) as u32,
        }
    };
    (rect_of(&solid), rect_of(&glow))
}

/// Eye level with the gateway's own corner, a few blocks back on `-Z`,
/// looking straight down `+Z` — the same convention `beacon_beam_pixels.rs`
/// uses.
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, 0.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
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
fn an_end_gateway_beam_draws_in_its_own_screen_rect() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();
    let spawn = test_spawn(0.0);

    let (solid_rect, glow_rect) = expected_rects(&spawn, camera.view_projection());
    println!("solid rect {solid_rect:?}, glow rect {glow_rect:?}");
    assert!(
        glow_rect.area() > 50,
        "the beam projects to only {} px — this gate cannot measure anything \
         that small: {glow_rect:?}",
        glow_rect.area()
    );

    let mut shoot = |install: bool| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        if install {
            let spawn = spawn.clone();
            state.set_end_gateway_beam_source(move |_eye| vec![spawn.clone()]);
        }
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let subject_px = shoot(true);
    let control_px = shoot(false);

    let sky = sky_bytes();

    // Absolute: the control paints nothing inside the glow rect.
    let control_in_rect = non_sky_in(&control_px, glow_rect, sky);
    assert_eq!(
        control_in_rect, 0,
        "the control paints {control_in_rect} px inside the beam's own rect \
         {glow_rect:?} — something *else* draws there. Control's non-sky bbox: {:?}",
        bbox_of(&control_px, |px| is_non_sky(px, sky))
    );

    let subject_in_rect = non_sky_in(&subject_px, glow_rect, sky);
    let fill = subject_in_rect as f64 / glow_rect.area() as f64;
    assert!(
        fill > 0.15,
        "the beam fills only {:.1}% of its own projected rect {glow_rect:?} \
         ({subject_in_rect} of {} px). Subject's non-sky bbox: {:?}",
        fill * 100.0,
        glow_rect.area(),
        bbox_of(&subject_px, |px| is_non_sky(px, sky))
    );

    // Differential: every changed pixel must land inside the (padded) glow rect.
    let (changed_rect, changed_count) = changed_bbox(&subject_px, &control_px)
        .expect("installing an end gateway beam source changed no pixel at all — the pass is dead");
    println!("changed bbox {changed_rect:?} ({changed_count} px), fill {fill:.3}");
    let allowed = glow_rect.padded(4);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the beam's projected rect: changed {changed_rect:?}, \
         allowed {allowed:?}. Installing an end gateway beam source must not \
         repaint anything else in the frame."
    );
    assert!(
        solid_rect.x0 >= glow_rect.x0.saturating_sub(1) && solid_rect.x1 <= glow_rect.x1 + 1,
        "the solid core's rect {solid_rect:?} should sit inside the wider glow \
         rect {glow_rect:?} (radius 0.15 vs 0.175)"
    );
}

/// `height <= 0` must draw nothing — `TheEndGatewayRenderer.submit`'s own
/// `if (state.height > 0)` guard, and [`end_gateway_beam_vertices`]'s own
/// early return. A regression here would draw a beam for every ordinary
/// (non-spawning, non-cooling-down) gateway in the world.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn zero_height_draws_nothing() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();

    let mut shoot = |install_zero_height: bool| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        if install_zero_height {
            let spawn = EndGatewayBeamSpawn {
                pos: GATEWAY,
                scale: 1.0,
                animation_time: 0.0,
                height: 0,
                color: COLOR,
            };
            state.set_end_gateway_beam_source(move |_eye| vec![spawn.clone()]);
        }
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let no_beam_px = shoot(true);
    let no_source_px = shoot(false);

    let diff = diff_count(&no_beam_px, &no_source_px);
    assert_eq!(
        diff, 0,
        "a height-0 spawn painted {diff} px different from no source installed at \
         all — the guard against drawing an inert gateway's beam is not holding"
    );
}

/// `CLAUDE.md`'s `ALPHA_BLENDING` rule: a direction-only "some pixels
/// changed" assertion is satisfied identically by a real blend and by a
/// hard discard-then-overwrite. Same shape as `beacon_beam_pixels.rs`'s
/// `the_glow_blends_but_the_solid_core_does_not`, run against the
/// **gateway's own** draw calls and texture — not inherited from the
/// beacon's test, since `prepare_gateway`/`draw_gateway_solid`/
/// `draw_gateway_glow` are genuinely different code paths.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_glow_blends_but_the_solid_core_does_not() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let camera = camera();
    let spawn = test_spawn(0.0);
    let (solid_rect, glow_rect) = expected_rects(&spawn, camera.view_projection());

    let shoot = |clear: [f32; 3]| -> Vec<u8> {
        let mut target = HeadlessTarget::new(device, W, H, format);
        let mut state = RenderState::new(device, queue, format, W, H, None);
        state.set_clear_color(clear);
        let spawn = spawn.clone();
        state.set_end_gateway_beam_source(move |_eye| vec![spawn.clone()]);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let black_bg = shoot([0.0, 0.0, 0.0]);
    let white_bg = shoot([1.0, 1.0, 1.0]);

    let solid_padded = solid_rect.padded(2);
    let mut annulus_diff = 0usize;
    let mut annulus_total = 0usize;
    let mut solid_diff = 0usize;
    let mut solid_total = 0usize;
    for y in glow_rect.y0..=glow_rect.y1 {
        for x in glow_rect.x0..=glow_rect.x1 {
            let idx = ((y * W + x) * 4) as usize;
            let a = &black_bg[idx..idx + 4];
            let b = &white_bg[idx..idx + 4];
            let d = (i32::from(a[0]) - i32::from(b[0])).abs()
                + (i32::from(a[1]) - i32::from(b[1])).abs()
                + (i32::from(a[2]) - i32::from(b[2])).abs();
            if solid_padded.contains(x, y) {
                solid_total += 1;
                if d > 6 {
                    solid_diff += 1;
                }
            } else {
                annulus_total += 1;
                if d > 6 {
                    annulus_diff += 1;
                }
            }
        }
    }
    let annulus_ratio = annulus_diff as f64 / annulus_total.max(1) as f64;
    let solid_ratio = solid_diff as f64 / solid_total.max(1) as f64;
    println!(
        "annulus (glow only): {annulus_diff}/{annulus_total} px differ ({:.1}%); \
         solid core: {solid_diff}/{solid_total} px differ ({:.1}%)",
        annulus_ratio * 100.0,
        solid_ratio * 100.0
    );
    assert!(
        annulus_total > 0,
        "the glow rect {glow_rect:?} has no annulus outside the padded solid rect \
         {solid_padded:?} — widen the camera or move the gateway"
    );
    assert!(
        annulus_diff > 0,
        "zero pixels in the glow-only annulus changed between a black and a white \
         background — the gateway's glow pass is not reading the destination at \
         all, which is the hard-discard-then-overwrite failure mode `CLAUDE.md` \
         warns a direction-only assertion cannot see"
    );
    assert!(
        solid_ratio < annulus_ratio * 0.6,
        "the solid core's disagreement ratio ({:.1}%) is not substantially lower \
         than the always-blending annulus's ({:.1}%) — the gateway's opaque core \
         should be a genuine overwrite, independent of whatever was behind it",
        solid_ratio * 100.0,
        annulus_ratio * 100.0
    );
}

/// Two `animation_time` values `18°` apart in the solid core's rotation
/// (`animation_time * 2.25` — see the module doc for why a multiple of `90`
/// would alias). The same phase rendered twice must be pixel-identical.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_beam_rotates_between_two_known_phases() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let camera = camera();
    let mut target = HeadlessTarget::new(device, W, H, format);

    let mut shoot = |animation_time: f32| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        let spawn = test_spawn(animation_time);
        state.set_end_gateway_beam_source(move |_eye| vec![spawn.clone()]);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let at_0 = shoot(0.0);
    let at_8 = shoot(8.0);
    let at_0_again = shoot(0.0);

    let moved = diff_count(&at_0, &at_8);
    let repeat = diff_count(&at_0, &at_0_again);

    println!("px differing t=0->t=8 = {moved}, same-phase repeat = {repeat}");

    assert_eq!(
        repeat, 0,
        "the same animation phase rendered twice must be pixel-identical; {repeat} \
         differing px means the frame is non-deterministic and the comparison \
         below proves nothing"
    );
    assert!(
        moved > 5,
        "an 18-degree core rotation must visibly change the beam's silhouette; \
         only {moved} px differ, which reads as a static pose ignoring \
         `animation_time`"
    );
}

/// Same discipline every sibling gate in this crate documents: before
/// trusting "the control is clean", locate the unconditional first-person
/// arm and assert it is disjoint from the beam's rect.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_first_person_arm_is_somewhere_else() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();

    let state = RenderState::new(device, queue, format, W, H, None);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
    let pixels = target.read_texels(device, queue);

    assert!(
        stats.first_person_arm_drawn,
        "this test's premise is that the arm paints unconditionally; if it does \
         not, the sibling gate's control is clean for a *different* reason than \
         it claims and its rationale needs rewriting"
    );

    let sky = sky_bytes();
    let (arm_rect, _arm_count) = bbox_of(&pixels, |px| is_non_sky(px, sky))
        .expect("the arm draws, so a beam-free frame is not uniformly sky");
    println!("arm bbox {arm_rect:?}");

    let (_solid_rect, glow_rect) = expected_rects(&test_spawn(0.0), camera.view_projection());
    assert!(
        !arm_rect.intersects(glow_rect),
        "the first-person arm ({arm_rect:?}) overlaps the beam's rect \
         ({glow_rect:?}). The sibling test would then be measuring the arm, \
         which is exactly the false-control failure `CLAUDE.md` records. Move \
         the gateway or the camera; do not relax the assertion."
    );
}
