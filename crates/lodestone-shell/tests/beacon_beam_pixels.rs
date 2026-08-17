//! Pixel gate: the beacon beam must **draw**, in its own screen rect,
//! through the real [`RenderState::render`] path — the same call `app.rs`'s
//! frame loop makes. Not a hole-in-the-world case like chest/skull (a 26.2
//! beacon's block model has real pyramid-frame geometry): before this
//! landed a beacon had no visual sign it was active at all.
//!
//! # The rect comes from the real geometry function, not a baked mesh
//!
//! Unlike the chest/skull/bell family this is procedural geometry —
//! [`lodestone_render::beacon_beam_vertices`] *is* the model, so the
//! expected rect here projects that function's own output through the same
//! [`Camera::view_projection`] the draw uses, rather than
//! `BlockEntityModelSet`'s baked corpus. Solid core and outer glow are
//! projected **separately**, because the two occupy genuinely different
//! footprints (radius `0.2` vs `0.25`) and the third test below needs to
//! tell them apart.
//!
//! # Proving the translucency is real, not a hard discard-then-overwrite
//!
//! `CLAUDE.md`'s rendering-constraints section is explicit that a
//! direction-only assertion is not enough for an `ALPHA_BLENDING` pipeline:
//! a hard discard-then-overwrite satisfies "some pixels changed" identically
//! to a real blend. [`the_glow_blends_but_the_solid_core_does_not`] renders
//! the identical scene against two different clear colours and checks the
//! **annulus** between the glow rect and the (smaller) solid rect: those
//! pixels must differ between the two backgrounds (real blending reads the
//! destination), while pixels *inside* the solid rect must be
//! byte-identical regardless of background (`BEACON_BEAM_OPAQUE` has
//! `Optional.empty()` blend — a genuine overwrite, per
//! `gpu/beacon_beam.rs`'s module doc table).
//!
//! # The negative control's premise is measured, not assumed
//!
//! Same discipline every sibling gate in this crate documents:
//! `the_first_person_arm_is_somewhere_else` locates the unconditional
//! first-person bare arm and asserts it is disjoint from the beam's rect,
//! rather than assuming a beam-free control frame is clean.
//!
//! ```text
//! cargo test -p lodestone-shell --test beacon_beam_pixels -- --ignored --nocapture
//! ```

use lodestone_render::{BeaconSpawn, BeamSection, Camera, GpuContext, HeadlessTarget, RenderTarget, beacon_beam_vertices};
use lodestone::gpu::{RenderState, SKY_COLOR};

const W: u32 = 320;
const H: u32 = 240;

/// The beacon's block position, three blocks ahead of the camera on `+Z`.
const BEACON: [i32; 3] = [0, 0, 3];

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

/// A single open-sky section: white, height irrelevant (it is the only, and
/// therefore last, section — `beacon_beam_vertices` always substitutes
/// `MAX_RENDER_Y` for that one), i.e. the common case of an unobstructed
/// beacon.
fn test_spawn() -> BeaconSpawn {
    BeaconSpawn {
        pos: BEACON,
        sections: vec![BeamSection {
            color: 0x00FF_FFFF,
            height: 4,
        }],
        animation_time: 0.0,
        beam_radius_scale: 1.0,
    }
}

/// Projects [`beacon_beam_vertices`]' own output through `view_proj` — the
/// exact function `gpu/beacon_beam.rs::prepare` calls, never a remembered
/// literal. Returns `(solid_rect, glow_rect)`.
fn expected_rects(spawn: &BeaconSpawn, view_proj: glam::Mat4) -> (Rect, Rect) {
    let (solid, glow) = beacon_beam_vertices(
        spawn.pos,
        &spawn.sections,
        spawn.animation_time,
        spawn.beam_radius_scale,
    );
    let rect_of = |verts: &[lodestone_render::BeamVertex]| -> Rect {
        let mut min = (f32::MAX, f32::MAX);
        let mut max = (f32::MIN, f32::MIN);
        for v in verts {
            let (sx, sy) = project(view_proj, glam::Vec3::from(v.position));
            // Points far above camera project behind or wildly off-screen
            // (the beam reaches y = base + 2048); clamp to the frame before
            // folding into the bounding box so a handful of off-screen
            // vertices cannot blow the rect out to nonsense.
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

/// Eye level with the beacon's base, a few blocks back on `-Z`, looking
/// straight down `+Z` — the same "yaw 0 faces +Z" convention
/// `bell_block_entity_pixels.rs` documents. Close enough that the (visually
/// infinite) column fills a large vertical angle at the beacon's own screen
/// column, so the rect this gate measures is never degenerately small.
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, 1.0, 0.0),
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
fn a_beacon_beam_draws_in_its_own_screen_rect() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();
    let spawn = test_spawn();

    let (solid_rect, glow_rect) = expected_rects(&spawn, camera.view_projection());
    println!("solid rect {solid_rect:?}, glow rect {glow_rect:?}");
    assert!(
        glow_rect.area() > 100,
        "the beam projects to only {} px — this gate cannot measure anything \
         that small: {glow_rect:?}",
        glow_rect.area()
    );

    let mut shoot = |install: bool| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        if install {
            let spawn = spawn.clone();
            state.set_beacon_source(move |_eye| vec![spawn.clone()]);
        }
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };

    let (subject_px, subject_stats) = shoot(true);
    let (control_px, control_stats) = shoot(false);

    assert!(
        subject_stats.beacon_beam_solid_vertices > 0,
        "the source is installed with a real section but zero solid vertices \
         reached the pass — check `beacon_beam.png` loaded from client.jar"
    );
    assert!(subject_stats.beacon_beam_glow_vertices > 0);
    assert_eq!(
        control_stats.beacon_beam_solid_vertices, 0,
        "RenderState::new must not default to an installed beacon source"
    );
    assert_eq!(control_stats.beacon_beam_glow_vertices, 0);

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

    // Differential: every changed pixel must land inside the (padded) glow
    // rect — the widest of the two projected footprints.
    let (changed_rect, changed_count) = changed_bbox(&subject_px, &control_px)
        .expect("installing a beacon source changed no pixel at all — the pass is dead");
    println!("changed bbox {changed_rect:?} ({changed_count} px), fill {fill:.3}");
    let allowed = glow_rect.padded(4);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the beam's projected rect: changed {changed_rect:?}, \
         allowed {allowed:?}. Installing a beacon source must not repaint anything \
         else in the frame."
    );
    assert!(
        solid_rect.x0 >= glow_rect.x0.saturating_sub(1) && solid_rect.x1 <= glow_rect.x1 + 1,
        "the solid core's rect {solid_rect:?} should sit inside the wider glow \
         rect {glow_rect:?} (radius 0.2 vs 0.25)"
    );
}

/// `CLAUDE.md`'s `ALPHA_BLENDING` rule: a direction-only "some pixels
/// changed" assertion is satisfied identically by a real blend and by a
/// hard discard-then-overwrite. This proves the distinction the two
/// `beacon_beam` pipelines are supposed to carry (see `gpu/beacon_beam.rs`'s
/// module doc table): the **glow** pass must read the destination (its
/// composite differs when the background does), and the **solid** pass must
/// not (`BEACON_BEAM_OPAQUE`'s `Optional.empty()` blend is a genuine
/// overwrite, independent of what was there before).
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_glow_blends_but_the_solid_core_does_not() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let camera = camera();
    let spawn = test_spawn();
    let (solid_rect, glow_rect) = expected_rects(&spawn, camera.view_projection());

    let shoot = |clear: [f32; 3]| -> Vec<u8> {
        let mut target = HeadlessTarget::new(device, W, H, format);
        let mut state = RenderState::new(device, queue, format, W, H, None);
        state.set_clear_color(clear);
        let spawn = spawn.clone();
        state.set_beacon_source(move |_eye| vec![spawn.clone()]);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let black_bg = shoot([0.0, 0.0, 0.0]);
    let white_bg = shoot([1.0, 1.0, 1.0]);

    // The annulus: inside the glow rect, outside the (padded) solid rect —
    // only the translucent glow paints here, never the opaque core.
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
         {solid_padded:?} — widen the camera or move the beacon"
    );
    assert!(
        annulus_diff > 0,
        "zero pixels in the glow-only annulus changed between a black and a white \
         background — the glow pass is not reading the destination at all, which is \
         the hard-discard-then-overwrite failure mode `CLAUDE.md` warns a \
         direction-only assertion cannot see"
    );
    // Not a strict zero: the solid rect is the *bounding box* of a rotated
    // diamond cross-section, which does not fill its own corners — those
    // corner pixels show bare background (correctly, since no solid
    // geometry covers them) and so trivially differ between the two clear
    // colours, exactly as the annulus does. The discriminating claim is the
    // one `BEACON_BEAM_OPAQUE` vs `BEACON_BEAM_TRANSLUCENT` actually makes:
    // the opaque core's disagreement ratio must be *substantially* lower
    // than the always-blending annulus's — a real blend on the solid pass
    // would push its ratio toward the annulus's ~100%, and "nothing draws
    // at all" would leave the two indistinguishable (both ~100%, since only
    // the bare clear colour would differ). Measured: annulus 100%, solid
    // ~34% — comfortably on the "opaque, mostly not blending" side.
    assert!(
        solid_ratio < annulus_ratio * 0.6,
        "the solid core's disagreement ratio ({:.1}%) is not substantially lower \
         than the always-blending annulus's ({:.1}%) — `BEACON_BEAM_OPAQUE` should \
         be a genuine overwrite (`ColorTargetState.DEFAULT`, `Optional.empty()` \
         blend) independent of whatever was behind it, so its rect should \
         disagree far less often than a translucent region does",
        solid_ratio * 100.0,
        annulus_ratio * 100.0
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
    assert_eq!(stats.beacon_beam_solid_vertices, 0);
    assert_eq!(stats.beacon_beam_glow_vertices, 0);

    let sky = sky_bytes();
    let (arm_rect, _arm_count) = bbox_of(&pixels, |px| is_non_sky(px, sky))
        .expect("the arm draws, so a beam-free frame is not uniformly sky");
    println!("arm bbox {arm_rect:?}");

    let (_solid_rect, glow_rect) = expected_rects(&test_spawn(), camera.view_projection());
    assert!(
        !arm_rect.intersects(glow_rect),
        "the first-person arm ({arm_rect:?}) overlaps the beam's rect \
         ({glow_rect:?}). The sibling test would then be measuring the arm, \
         which is exactly the false-control failure `CLAUDE.md` records. Move \
         the beacon or the camera; do not relax the assertion."
    );
}
