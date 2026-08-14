//! Pixel gate: a bell's body must **draw**, in its own screen rect, through
//! the real [`RenderState::render`] path — the same call `app.rs`'s frame
//! loop makes (the third block-entity type after chest and
//! skull).
//!
//! # Why this gate is the whole point of the change
//!
//! Unlike chest and skull, a bell's block model is **not** entirely empty —
//! `assets/minecraft/models/block/bell.json` has real geometry for the
//! attachment frame (the post/mount a bell hangs from). What is missing is
//! the swinging body and its flared rim (`BellRenderer`/`BellModel`,
//! `bell_body`/`bell_base` — see `docs/block-entity-renderers.md`'s Bell
//! section), so before this a bell was a *partial* hole: a frame with
//! nothing hanging in it, easy to mistake for "looks about right" in a quick
//! screenshot. This gate measures the missing piece specifically, the same
//! way `chest_block_entity_pixels.rs` measures a total hole: coverage
//! *inside the bell body's own projected rect*, never a frame average.
//!
//! # The metric, and why it is a rect and not a fraction
//!
//! Same two measurements the chest/skull gates use, over the same pair of
//! frames:
//!
//! 1. **Differential.** Subject minus control. Everything else in the frame
//!    — the clear, the unconditional first-person arm, and (in a real world)
//!    the block model's own frame geometry — is identical in both, so every
//!    changed pixel is the bell body/rim. Its **bounding box** must fall
//!    inside the bell's own projected rect.
//! 2. **Absolute, inside the rect.** The control must paint ~nothing there
//!    and the subject must fill most of it.
//!
//! This gate installs no block-model source at all (there is no terrain
//! world here), so "the control" is a frame with the ordinary
//! unconditional passes only — the same shape `chest_block_entity_pixels.rs`
//! uses, not a claim that a real bell's frame geometry is absent.
//!
//! The expected rect is projected from the **real baked vertices** of the
//! real corpus mesh, through the *same* [`Camera::view_projection`] the
//! render call uses and the *same* `part_transforms`
//! [`BlockEntityModelSet::resolve_bell`] produces — never a remembered
//! literal. Failure output prints a bounding box, never a percentage alone.
//!
//! # The shake is real motion, not merely a different number
//!
//! `bell_shake_angle`'s own unit tests (`lodestone-render`) already predict
//! the exact `(x_rot, z_rot)` the formula produces — see that crate's
//! `bell_shake_angle_matches_the_exact_vanilla_formula`. What those tests
//! cannot see is whether the angle actually reaches the mesh: this gate's
//! second test drives a real shake through `resolve_bell` and the real GPU
//! draw and asserts the rendered pixels actually change, localised inside
//! the bell's own rect — the same "does it move geometry, not just produce a
//! different number" standard `opening_moves_the_lid_and_lock_and_leaves_the_bottom_alone`
//! holds the chest lid to.
//!
//! # The negative control's premise is measured, not assumed
//!
//! `CLAUDE.md` records a "clears uniformly" control that failed at 3.5%
//! because of the unconditional first-person bare arm — a premise that was
//! false since before the feature under test existed. That arm draws in
//! every frame here too. `the_first_person_arm_is_somewhere_else` locates it
//! and asserts its bounding box is disjoint from the bell's rect, rather
//! than assuming the rect is clean.
//!
//! # What this gate does *not* prove
//!
//! `RenderState::set_bell_source` exists and this gate calls it directly, the
//! same way `chest_block_entity_pixels.rs` calls `set_block_entity_source`,
//! with a hand-built closure rather than `Sim::bell_source()`'s. **The
//! `app.rs` install call is landed** (`if let Some(f) = self.sim.bell_source()
//! { render.set_bell_source(f); }`, mirroring skull/sign exactly — see
//! `docs/block-entity-renderers.md`'s Bell section), and
//! `sim::tests::bell_source_tracks_connection_state_and_is_safe_before_login`
//! proves that accessor tracks connection state and is panic-safe before
//! login. What remains unproven **by any gate in this crate, for chest, skull,
//! sign or bell alike** is a real client actually drawing one through a live
//! `ClientHandle`: that needs a real login handshake plus a chunk carrying
//! both a `minecraft:bell` block state and a recorded block-entity entry, and
//! no test double here builds one yet. So this gate proves the render pass is
//! correct and reachable, not that a real client draws a bell today — but
//! that gap is pre-existing test-infrastructure scope, not something this
//! change introduced or could close by itself.
//!
//! ```text
//! cargo test -p lodestone-shell --test bell_block_entity_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{
    BellShakeDirection, BellSpawn, BlockEntityMesh, BlockEntityModelSet, Camera, GpuContext,
    HeadlessTarget, RenderTarget,
};

const W: u32 = 320;
const H: u32 = 240;

/// The bell's block position. Directly ahead of the camera on `+Z`.
const BELL: [i32; 3] = [0, 0, 2];

/// Manhattan RGB distance above which a pixel counts as "not the clear
/// colour". Matches the chest/skull gates' threshold.
const NON_SKY: i32 = 60;

/// Every block-entity sheet the jar ships, across every family. Derived, not
/// a literal — see `skull_block_entity_pixels.rs`'s `expected_sheets` doc for
/// why a hardcoded count here already went stale once (chest-only, then
/// skull added its own sheets to the same loader).
fn expected_sheets() -> usize {
    lodestone_render::block_entity_texture_stems().len()
}

fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}

fn is_non_sky(px: &[u8], sky: [u8; 3]) -> bool {
    let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
        + (i32::from(px[1]) - i32::from(sky[1])).abs()
        + (i32::from(px[2]) - i32::from(sky[2])).abs();
    d > NON_SKY
}

/// An inclusive pixel rect, in screen space.
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

    /// Grown by `pad` pixels on every side, clamped to the frame.
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

/// Bounding box of every pixel `predicate` accepts, plus the count — `None`
/// when nothing matched.
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

/// Bounding box of the pixels that differ between two frames.
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

/// The screen rect of a posed mesh, projected from its **real baked
/// vertices** through the very `part_transforms` the draw uses. Mirrors
/// `chest_block_entity_pixels.rs`'s helper of the same name.
fn posed_screen_rect(
    mesh: &BlockEntityMesh,
    part_transforms: &[glam::Mat4],
    view_proj: glam::Mat4,
) -> Rect {
    let mut min = (f32::MAX, f32::MAX);
    let mut max = (f32::MIN, f32::MIN);
    for (index, range) in mesh.parts.iter().enumerate() {
        let start = range.vertex_start as usize;
        let end = start + range.vertex_count as usize;
        for vertex in &mesh.vertices[start..end] {
            let world = part_transforms[index].transform_point3(glam::Vec3::from(vertex.position));
            let (sx, sy) = project(view_proj, world);
            min = (min.0.min(sx), min.1.min(sy));
            max = (max.0.max(sx), max.1.max(sy));
        }
    }
    assert!(min.0 < max.0 && min.1 < max.1, "no vertices projected");
    Rect {
        x0: min.0.max(0.0).floor() as u32,
        y0: min.1.max(0.0).floor() as u32,
        x1: (max.0.min((W - 1) as f32)).ceil() as u32,
        y1: (max.1.min((H - 1) as f32)).ceil() as u32,
    }
}

/// Eye near the bell body's own mid-height (`0.375..0.8125` in block space —
/// see `bell_model`'s doc), two blocks back on `-Z`, looking straight down
/// `+Z` (yaw `0` faces `+Z` in Minecraft's convention).
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, 0.55, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
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
fn a_bell_draws_in_its_own_screen_rect_where_the_block_model_has_nothing() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();

    // --- The expected rect, from the real corpus mesh and the real matrices ---
    let models = BlockEntityModelSet::load();
    let spawn = BellSpawn::at(BELL);
    let instance = models
        .resolve_bell(&spawn)
        .expect("the bell model must be in the corpus");
    let mesh = models.get(instance.model).expect("mesh");
    let bell_rect = posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection());
    println!("bell rect (from real baked vertices): {bell_rect:?}");
    assert!(
        bell_rect.area() > 100,
        "the bell projects to only {} px — this gate cannot measure anything \
         that small, so the camera, not the renderer, is wrong: {bell_rect:?}",
        bell_rect.area()
    );

    // --- Subject: the source installed. Control: no source at all. -----------
    let mut shoot = |install: bool| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        if install {
            state.set_bell_source(move |_eye| vec![BellSpawn::at(BELL)]);
        }
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };

    let (subject_px, subject_stats) = shoot(true);
    let (control_px, control_stats) = shoot(false);

    // --- The sheets really loaded (every family). -----------------------------
    let expected = expected_sheets();
    assert_eq!(
        subject_stats.block_entity_sheets_loaded, expected,
        "expected all {expected} block-entity sheets from client.jar; a short \
         count means the pack is missing or a stem is misspelled, and this \
         gate cannot distinguish that from a broken pass"
    );

    // --- The exact, non-approximate corroboration. ---------------------------
    assert_eq!(
        subject_stats.block_entities_drawn, 1,
        "the source is installed and the bell is in front of the camera"
    );
    assert_eq!(subject_stats.block_entities_culled, 0);
    assert_eq!(
        control_stats.block_entities_drawn, 0,
        "RenderState::new must not default to an installed bell source"
    );

    let sky = sky_bytes();

    // --- (2) Absolute, inside the rect. The control's premise, measured. -----
    let control_in_rect = non_sky_in(&control_px, bell_rect, sky);
    assert_eq!(
        control_in_rect, 0,
        "the control paints {control_in_rect} px inside the bell's own rect \
         {bell_rect:?} — something *else* draws there, so this gate would be \
         measuring that instead of the bell. Control frame's whole non-sky \
         bbox: {:?}",
        bbox_of(&control_px, |px| is_non_sky(px, sky))
    );

    let subject_in_rect = non_sky_in(&subject_px, bell_rect, sky);
    let fill = subject_in_rect as f64 / bell_rect.area() as f64;
    assert!(
        fill > 0.35,
        "the bell fills only {:.1}% of its own projected rect {bell_rect:?} \
         ({subject_in_rect} of {} px). Subject's non-sky bbox: {:?}",
        fill * 100.0,
        bell_rect.area(),
        bbox_of(&subject_px, |px| is_non_sky(px, sky))
    );

    // --- (1) Differential: every changed pixel must *be* the bell. -----------
    let (changed_rect, changed_count) = changed_bbox(&subject_px, &control_px)
        .expect("installing a bell source changed no pixel at all — the pass is dead");
    println!("changed bbox {changed_rect:?} ({changed_count} px), fill {fill:.3}");
    let allowed = bell_rect.padded(2);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the bell's projected rect: changed {changed_rect:?}, \
         allowed {allowed:?}. Installing a bell source must not repaint anything \
         else in the frame."
    );
    assert!(
        changed_count > bell_rect.area() / 3,
        "only {changed_count} px changed inside a {} px rect",
        bell_rect.area()
    );
}

/// A shaking bell must move real, rendered pixels — not merely produce a
/// different `bell_shake_angle` number (`lodestone-render`'s own unit tests
/// already predict that formula exactly). `ticks = pi^2 / 2` is the same
/// value those tests use, chosen because it makes `sin(ticks / pi) == 1`
/// exactly — the largest swing the formula produces near the start of a
/// ring, roughly 10 degrees, so this is a magnitude prediction ("some pixels
/// move, near the bell's own silhouette"), not merely a sign check.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn shaking_the_bell_moves_pixels_inside_its_own_rect() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let camera = camera();

    let models = BlockEntityModelSet::load();
    let resting = models.resolve_bell(&BellSpawn::at(BELL)).expect("resting");
    let mesh = models.get(resting.model).expect("mesh");
    let view_proj = camera.view_projection();
    let bell_rect = posed_screen_rect(mesh, &resting.part_transforms, view_proj);

    let ticks = std::f32::consts::PI * std::f32::consts::PI / 2.0;
    let shoot = |spawn: BellSpawn| -> Vec<u8> {
        let mut target = HeadlessTarget::new(device, W, H, format);
        let mut state = RenderState::new(device, queue, format, W, H, None);
        state.set_bell_source(move |_eye| vec![spawn]);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let resting_px = shoot(BellSpawn::at(BELL));
    let shaking_px = shoot(BellSpawn {
        shake: Some((BellShakeDirection::East, ticks)),
        ..BellSpawn::at(BELL)
    });

    let (diff_rect, diff_count) = changed_bbox(&resting_px, &shaking_px).expect(
        "a resting and a shaking bell produced pixel-identical frames — the shake \
         angle is computed but never reaches the mesh",
    );
    println!("resting-vs-shaking changed bbox {diff_rect:?} ({diff_count} px)");

    // A ~10 degree rotation about a pivot near the box's own top can swing
    // the far corners (up to ~6.7 texels from the pivot) outward by roughly
    // `6.7 * sin(10°) ≈ 1.2` texels (`≈0.07` blocks) beyond the *resting*
    // silhouette — measured at 2 px over a padding of 4 the first time this
    // gate ran, so the padding is widened rather than the rotation being
    // treated as a bug: the resting rect was never meant to bound a *posed*
    // one, only to prove the change stays local to the bell instead of
    // repainting the rest of the frame.
    let allowed = bell_rect.padded(10);
    assert!(
        allowed.x0 <= diff_rect.x0
            && allowed.y0 <= diff_rect.y0
            && diff_rect.x1 <= allowed.x1
            && diff_rect.y1 <= allowed.y1,
        "the shake changed pixels outside the bell's own rect: changed {diff_rect:?}, \
         allowed {allowed:?} — a real shake must not repaint anything else"
    );
}

/// What else already paints here — **measured**, not assumed.
///
/// `CLAUDE.md` records a control that asserted a frame "clears uniformly"
/// and failed at 3.5% because of the unconditional first-person bare arm.
/// That arm draws in this gate's frames too. This test locates it and
/// asserts it is disjoint from the bell's rect, so the sibling gates' clean-
/// control premise is a measurement rather than a hope.
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
    assert_eq!(stats.block_entities_drawn, 0);

    let sky = sky_bytes();
    let (arm_rect, arm_count) = bbox_of(&pixels, |px| is_non_sky(px, sky))
        .expect("the arm draws, so a bell-free frame is not uniformly sky");
    println!("arm bbox {arm_rect:?} ({arm_count} px)");

    let models = BlockEntityModelSet::load();
    let instance = models.resolve_bell(&BellSpawn::at(BELL)).expect("single bell");
    let mesh = models.get(instance.model).expect("mesh");
    let bell_rect = posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection());

    assert!(
        !arm_rect.intersects(bell_rect),
        "the first-person arm ({arm_rect:?}) overlaps the bell's rect \
         ({bell_rect:?}). The sibling gates would then be measuring the arm, \
         which is exactly the false-control failure `CLAUDE.md` records. Move \
         the bell or the camera; do not relax the assertion."
    );
}
