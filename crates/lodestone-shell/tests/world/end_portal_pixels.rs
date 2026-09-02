//! Pixel gate: the end portal / end gateway star-field surface must
//! **draw**, in its own screen rect, through the real
//! [`RenderState::render`] path — the same call `app.rs`'s frame loop makes.
//!
//! Unlike every prior type in this issue, this is a genuine hole-in-the-world
//! case again: `end_portal.json`/`end_gateway.json` have zero model
//! elements, so before this landed a stronghold's portal and an End
//! island's gateway both drew nothing at all — no metric could see it, the
//! same way chest was before its own landing.
//!
//! # The rect comes from the real geometry function, not a baked mesh
//!
//! Like `beacon_beam_pixels.rs`, this is procedural geometry —
//! [`lodestone_render::end_portal_vertices`]/[`lodestone_render::end_gateway_vertices`]
//! *are* the model — so the expected rect projects those functions' own
//! output through the same [`Camera::view_projection`] the draw uses.
//!
//! # Two known animation phases, not one frame
//!
//! `CLAUDE.md`'s evidence standard: an animated effect needs two frames at
//! known phases to distinguish an animation from a static pose. The star
//! field's swirl depends on `GameTime`
//! ([`RenderState::set_end_portal_game_time`]), so
//! [`the_swirl_animates_between_two_known_game_times`] renders the same
//! portal at two different `GameTime` values and requires the two frames to
//! differ — a vertex-position gate alone cannot see this, since the geometry
//! (and therefore the projected rect) is identical at both times; only the
//! fragment shader's own texture sampling moves.
//!
//! # The negative control's premise is measured, not assumed
//!
//! Same discipline every sibling gate in this crate documents:
//! [`the_first_person_arm_is_somewhere_else`] locates the unconditional
//! first-person bare arm and asserts it is disjoint from the portal's rect,
//! rather than assuming a portal-free control frame is clean.
//!
//! ```text
//! cargo test -p lodestone-shell --test end_portal_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{
    Camera, EndGatewaySpawn, EndPortalSpawn, EndPortalVertex, GpuContext, HeadlessTarget,
    RenderTarget, end_gateway_vertices, end_portal_vertices,
};

const W: u32 = 320;
const H: u32 = 240;

/// The portal/gateway's block position, three blocks ahead of the camera on
/// `+Z`.
const BLOCK: [i32; 3] = [0, 0, 3];

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

fn rect_of(view_proj: glam::Mat4, verts: &[EndPortalVertex]) -> Rect {
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
}

/// Eye a few blocks back on `-Z`, level with the portal, looking straight
/// down `+Z` — the same "yaw 0 faces +Z" convention every sibling gate in
/// this crate uses. Pitched down slightly so the flattened `y ∈ [0.375,
/// 0.75]` portal slab is not an edge-on sliver at pitch 0.
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, 1.5, 0.0),
        yaw: 0.0,
        pitch: 20.0,
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

fn portal_spawn() -> EndPortalSpawn {
    EndPortalSpawn { pos: BLOCK }
}

/// All six faces open — the common case (a gateway surrounded by air, the
/// way one actually sits inside its worldgen frame).
fn gateway_spawn() -> EndGatewaySpawn {
    EndGatewaySpawn {
        pos: BLOCK,
        faces: vec![
            lodestone_assets::Direction::Down,
            lodestone_assets::Direction::Up,
            lodestone_assets::Direction::North,
            lodestone_assets::Direction::South,
            lodestone_assets::Direction::West,
            lodestone_assets::Direction::East,
        ],
    }
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn an_end_portal_draws_in_its_own_screen_rect() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();
    let spawn = portal_spawn();

    let rect = rect_of(camera.view_projection(), &end_portal_vertices(spawn.pos));
    println!("expected rect {rect:?}");
    assert!(
        rect.area() > 100,
        "the portal projects to only {} px — this gate cannot measure \
         anything that small: {rect:?}",
        rect.area()
    );

    let mut shoot = |install: bool| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        if install {
            let spawn = spawn.clone();
            state.set_end_portal_source(move |_eye| vec![spawn.clone()]);
        }
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };

    let (subject_px, subject_stats) = shoot(true);
    let (control_px, control_stats) = shoot(false);

    assert!(
        subject_stats.end_portal_vertices > 0,
        "the source is installed with a real portal but zero vertices reached \
         the pass — check `end_sky.png`/`end_portal.png` loaded from client.jar"
    );
    assert_eq!(
        control_stats.end_portal_vertices, 0,
        "RenderState::new must not default to an installed end-portal source"
    );

    let sky = sky_bytes();

    let control_in_rect = non_sky_in(&control_px, rect, sky);
    assert_eq!(
        control_in_rect, 0,
        "the control paints {control_in_rect} px inside the portal's own rect \
         {rect:?} — something *else* draws there. Control's non-sky bbox: {:?}",
        bbox_of(&control_px, |px| is_non_sky(px, sky))
    );

    let subject_in_rect = non_sky_in(&subject_px, rect, sky);
    let fill = subject_in_rect as f64 / rect.area() as f64;
    assert!(
        fill > 0.5,
        "the portal fills only {:.1}% of its own projected rect {rect:?} \
         ({subject_in_rect} of {} px) — an opaque, screen-filling quad pair \
         should cover nearly all of it. Subject's non-sky bbox: {:?}",
        fill * 100.0,
        rect.area(),
        bbox_of(&subject_px, |px| is_non_sky(px, sky))
    );

    let (changed_rect, changed_count) = changed_bbox(&subject_px, &control_px)
        .expect("installing an end-portal source changed no pixel at all — the pass is dead");
    println!("changed bbox {changed_rect:?} ({changed_count} px), fill {fill:.3}");
    let allowed = rect.padded(4);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the portal's projected rect: changed \
         {changed_rect:?}, allowed {allowed:?}"
    );
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn an_end_gateway_with_every_face_open_draws_the_whole_block() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();
    let spawn = gateway_spawn();

    let rect = rect_of(
        camera.view_projection(),
        &end_gateway_vertices(spawn.pos, &spawn.faces),
    );
    println!("expected rect {rect:?}");
    assert!(rect.area() > 100, "{rect:?}");

    let mut state = RenderState::new(device, queue, format, W, H, None);
    state.set_end_gateway_source(move |_eye| vec![spawn.clone()]);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
    let pixels = target.read_texels(device, queue);

    assert!(stats.end_portal_vertices > 0, "zero gateway vertices reached the pass");

    let sky = sky_bytes();
    let fill = non_sky_in(&pixels, rect, sky) as f64 / rect.area() as f64;
    assert!(
        fill > 0.5,
        "the gateway fills only {:.1}% of its own projected rect {rect:?}",
        fill * 100.0
    );
}

/// `CLAUDE.md`'s evidence standard: an animated effect needs two frames at
/// known phases, since a vertex-position rect alone cannot distinguish a
/// moving swirl from a static one — the geometry (and its projected rect)
/// never changes; only the fragment shader's own `GameTime`-driven sampling
/// does.
///
/// **`GAME_TIME_DELTA` must not be a round number, and this was measured,
/// not assumed.** `end_portal_layer_uv`'s per-layer UV shift for a
/// `GameTime` delta `dt` works out to `Δuv.y(layer) = 0.5 * dt * (3 +
/// layer)` — and a `Repeat`-addressed texture sample only sees the
/// *fractional* part of a UV. The first version of this test used `dt =
/// 200.0`, which makes `0.5 * dt == 100.0`, an integer, so `Δuv.y` was an
/// exact integer for *every* `layer` in `1..=16` simultaneously — the
/// swirl moved by whole texture repeats and landed back on the identical
/// texel, and the two frames came back **byte-identical inside the
/// portal's own rect**, caught only by rendering a real GPU frame (a
/// vertex-only or CPU-side check cannot see this: the geometry is
/// unchanged, only the fragment shader's sampling moves). `0.5 * dt` must
/// not be an integer for `dt` to guarantee a non-integer shift for every
/// layer, so `dt = 0.37` (`0.5 * 0.37 = 0.185`) is used instead.
const GAME_TIME_DELTA: f32 = 0.37;

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_swirl_animates_between_two_known_game_times() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let camera = camera();
    let spawn = portal_spawn();
    let rect = rect_of(camera.view_projection(), &end_portal_vertices(spawn.pos));

    let shoot = |game_time: f32| -> Vec<u8> {
        let mut target = HeadlessTarget::new(device, W, H, format);
        let mut state = RenderState::new(device, queue, format, W, H, None);
        let spawn = spawn.clone();
        state.set_end_portal_source(move |_eye| vec![spawn.clone()]);
        state.set_end_portal_game_time(game_time);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let t0 = shoot(0.0);
    let t1 = shoot(GAME_TIME_DELTA);

    let mut diff_in_rect = 0usize;
    for y in rect.y0..=rect.y1 {
        for x in rect.x0..=rect.x1 {
            let idx = ((y * W + x) * 4) as usize;
            let a = &t0[idx..idx + 4];
            let b = &t1[idx..idx + 4];
            let d = (i32::from(a[0]) - i32::from(b[0])).abs()
                + (i32::from(a[1]) - i32::from(b[1])).abs()
                + (i32::from(a[2]) - i32::from(b[2])).abs();
            if d > 12 {
                diff_in_rect += 1;
            }
        }
    }
    println!(
        "{diff_in_rect} px differ between GameTime 0.0 and {GAME_TIME_DELTA} inside {rect:?}"
    );
    assert!(
        diff_in_rect > 0,
        "rendering the same end portal at two different `GameTime` values \
         produced pixel-identical frames inside its own rect {rect:?} — the \
         swirl is not animating"
    );
}

/// Same discipline every sibling gate in this crate documents: before
/// trusting "the control is clean", locate the unconditional first-person
/// arm and assert it is disjoint from the portal's rect.
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
    assert_eq!(stats.end_portal_vertices, 0);

    let sky = sky_bytes();
    let (arm_rect, _arm_count) = bbox_of(&pixels, |px| is_non_sky(px, sky))
        .expect("the arm draws, so a portal-free frame is not uniformly sky");
    println!("arm bbox {arm_rect:?}");

    let portal_rect = rect_of(camera.view_projection(), &end_portal_vertices(BLOCK));
    assert!(
        !arm_rect.intersects(portal_rect),
        "the first-person arm ({arm_rect:?}) overlaps the portal's rect \
         ({portal_rect:?}). The sibling test would then be measuring the arm. \
         Move the portal or the camera; do not relax the assertion."
    );
}
