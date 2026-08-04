//! Pixel gate: a skull/head must **draw**, in its own screen rect, through the
//! real [`RenderState::render`] path — the same call `app.rs`'s frame loop
//! makes (issue #23, the second block-entity type after chest).
//!
//! # Why this gate is the whole point of the change
//!
//! `assets/minecraft/models/block/skull.json` is, verbatim,
//! `{"textures":{"particle":"..."}}` — zero elements, the same "hole in the
//! world" shape that made chest first. `lodestone-render`'s own `block_entity`
//! unit tests prove the bake, both placement matrices (ground/wall) and the
//! type resolution — they are a **closed loop** with respect to this crate:
//! none of them calls `RenderState::prepare_block_entities`, so all of them
//! would stay green with the shell pass deleted. This gate drives the real
//! shell path instead, and asserts coverage *inside the subject's screen
//! rect* rather than a frame average, per `CLAUDE.md`'s dominant defect class.
//!
//! # The metric, and why it is a rect and not a fraction
//!
//! Same two measurements `chest_block_entity_pixels.rs` uses, over the same
//! pair of frames:
//!
//! 1. **Differential.** Subject minus control. Everything else in the frame —
//!    the clear, the unconditional first-person arm — is identical in both,
//!    so every changed pixel is the skull. Its **bounding box** must fall
//!    inside the skull's own projected rect.
//! 2. **Absolute, inside the rect.** The control must paint ~nothing there and
//!    the subject must fill most of it.
//!
//! The expected rect is projected from the **real baked vertices** of the
//! real corpus mesh, through the *same* [`Camera::view_projection`] the
//! render call uses and the *same* `part_transforms`
//! [`BlockEntityModelSet::resolve_skull`] produces — never a remembered
//! literal. Failure output prints a bounding box, never a percentage alone,
//! because a fraction cannot tell a uniform-but-wrong frame from a localised
//! blob.
//!
//! # The negative control's premise is measured, not assumed
//!
//! Before trusting "the control is clean", ask what else already paints in
//! this rect — `CLAUDE.md` records a "clears uniformly" control that failed
//! at 3.5% because of the unconditional first-person bare arm, a false
//! premise that had been wrong since long before the feature under test
//! existed. That arm is drawn in every frame here too (nothing installs a
//! third-person body). `the_first_person_arm_is_somewhere_else` *locates* it
//! and asserts its bounding box is disjoint from the skull's rect, rather
//! than assuming the rect is clean.
//!
//! # How this gate was actually developed and proved, not merely written
//!
//! `RenderState::set_skull_source`/`prepare_block_entities`'s skull half did
//! not exist on `main` when this file was drafted — `gpu.rs`/`gpu/*.rs` were
//! another agent's live work for the whole of the chest+skull session, so the
//! five-patch wiring this gate exercises could not be applied to the shared
//! checkout directly. It was instead hand-applied in a private
//! `git worktree add --detach` (the sanctioned isolation pattern for "move to
//! a newer/hypothetical commit" per `CLAUDE.md`), and this gate was written
//! and run **there first**, against a real GPU adapter, before the
//! orchestrating agent landed the same five patches on the shared checkout.
//! It was then re-run here, unmodified, once the wiring arrived, and passed
//! identically both times — the worktree run is what makes "expect it to
//! fail for want of the wiring, not for want of geometry" a checked claim
//! rather than a prediction.
//!
//! Measured green, both in the isolated worktree and against the real
//! wiring on the shared checkout:
//!
//! | gate | measurement |
//! |---|---|
//! | skull draws | rect `x136..184 y96..144` (2304 px); fill **88.1%**; changed bbox `x137..182 y97..142`, entirely inside |
//! | wall vs floor | floor rect `x136..184 y96..144`, wall rect `x134..186 y68..120` — distinct, and the two frames differ by 3762 px |
//! | arm is elsewhere | arm bbox `x247..319 y169..239`, disjoint from the skull rect |
//!
//! ```text
//! cargo test -p lodestone-shell --test skull_block_entity_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{
    BlockEntityMesh, BlockEntityModelSet, Camera, GpuContext, HeadlessTarget, RenderTarget,
    SkullSpawn,
};

const W: u32 = 320;
const H: u32 = 240;

/// The skull's block position. Directly ahead of the camera on `+Z`, close
/// enough that a half-block-tall box still projects to a measurable rect —
/// unlike a full-block chest, a skull cannot rely on distance-independent
/// coverage.
const SKULL: [i32; 3] = [0, 0, 2];

/// Manhattan RGB distance above which a pixel counts as "not the clear
/// colour". Matches `chest_block_entity_pixels.rs`'s threshold.
const NON_SKY: i32 = 60;

/// Every block-entity sheet the jar ships, across **both** families: 22 chest
/// stems (7 materials x 3 halves + 1 half-independent ender) plus 5 skull
/// stems (skeleton, wither skeleton, zombie, creeper, player). The shell's
/// texture loader and this pass's own GPU loader both iterate
/// `lodestone_render::block_entity_texture_stems()`, the union — so this
/// count is the union too, not skull's 5 alone, regardless of what this
/// particular gate installs as a source this frame.
const EXPECTED_SHEETS: usize = 27;

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
            None => Rect {
                x0: x,
                y0: y,
                x1: x,
                y1: y,
            },
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
            None => Rect {
                x0: x,
                y0: y,
                x1: x,
                y1: y,
            },
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

/// Eye near the skull's own mid-height (a skull is half a block tall, much
/// shorter than a chest), two blocks back on `-Z`, looking straight down `+Z`
/// (yaw `0` faces `+Z` in Minecraft's convention).
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, 0.25, 0.0),
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
fn a_skull_draws_in_its_own_screen_rect_where_no_block_model_could() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();

    // --- The expected rect, from the real corpus mesh and the real matrices ---
    let models = BlockEntityModelSet::load();
    let spawn = SkullSpawn::at(SKULL);
    let instance = models
        .resolve_skull(&spawn)
        .expect("the skull_mob model must be in the corpus");
    let mesh = models.get(instance.model).expect("mesh");
    let skull_rect = posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection());
    println!("skull rect (from real baked vertices): {skull_rect:?}");
    assert!(
        skull_rect.area() > 200,
        "the skull projects to only {} px — this gate cannot measure anything \
         that small, so the camera, not the renderer, is wrong: {skull_rect:?}",
        skull_rect.area()
    );

    // --- Subject: the source installed. Control: no source at all. -----------
    let mut shoot = |install: bool| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        if install {
            state.set_skull_source(move |_eye| vec![SkullSpawn::at(SKULL)]);
        }
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };

    let (subject_px, subject_stats) = shoot(true);
    let (control_px, control_stats) = shoot(false);

    // --- The sheets really loaded (both families — see EXPECTED_SHEETS). -----
    assert_eq!(
        subject_stats.block_entity_sheets_loaded, EXPECTED_SHEETS,
        "expected all {EXPECTED_SHEETS} block-entity sheets (chest + skull) from \
         client.jar; a short count means the pack is missing or a stem is \
         misspelled, and this gate cannot distinguish that from a broken pass"
    );

    // --- The exact, non-approximate corroboration. ---------------------------
    assert_eq!(
        subject_stats.block_entities_drawn, 1,
        "the source is installed and the skull is in front of the camera"
    );
    assert_eq!(subject_stats.block_entities_culled, 0);
    assert_eq!(
        control_stats.block_entities_drawn, 0,
        "RenderState::new must not default to an installed skull source"
    );

    let sky = sky_bytes();

    // --- (2) Absolute, inside the rect. The control's premise, measured. -----
    let control_in_rect = non_sky_in(&control_px, skull_rect, sky);
    assert_eq!(
        control_in_rect, 0,
        "the control paints {control_in_rect} px inside the skull's own rect \
         {skull_rect:?} — something *else* draws there, so this gate would be \
         measuring that instead of the skull. Control frame's whole non-sky \
         bbox: {:?}",
        bbox_of(&control_px, |px| is_non_sky(px, sky))
    );

    let subject_in_rect = non_sky_in(&subject_px, skull_rect, sky);
    let fill = subject_in_rect as f64 / skull_rect.area() as f64;
    assert!(
        fill > 0.45,
        "the skull fills only {:.1}% of its own projected rect {skull_rect:?} \
         ({subject_in_rect} of {} px). A skull is a solid box, so anything this \
         sparse means it drew partially, inside-out, or somewhere else. \
         Subject's non-sky bbox: {:?}",
        fill * 100.0,
        skull_rect.area(),
        bbox_of(&subject_px, |px| is_non_sky(px, sky))
    );

    // --- (1) Differential: every changed pixel must *be* the skull. ----------
    let (changed_rect, changed_count) = changed_bbox(&subject_px, &control_px)
        .expect("installing a skull source changed no pixel at all — the pass is dead");
    println!("changed bbox {changed_rect:?} ({changed_count} px), fill {fill:.3}");
    let allowed = skull_rect.padded(2);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the skull's projected rect: changed {changed_rect:?}, \
         allowed {allowed:?}. Installing a skull source must not repaint anything \
         else in the frame."
    );
    assert!(
        changed_count > skull_rect.area() / 2,
        "only {changed_count} px changed inside a {} px rect",
        skull_rect.area()
    );
}

/// A wall skull must draw somewhere **different** from a floor skull at the
/// same block position — the two placement matrices
/// (`skull_ground_placement_matrix`/`skull_wall_placement_matrix`) are
/// distinct code paths and a bug that made one silently fall back to the
/// other would still pass the first test (a floor skull with `rotation = 0`
/// and a north-facing wall skull could coincidentally project close, so this
/// asserts the *rects differ*, not merely that both draw).
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn wall_and_floor_skulls_project_to_different_rects() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let camera = camera();

    let models = BlockEntityModelSet::load();
    let floor = models.resolve_skull(&SkullSpawn::at(SKULL)).expect("floor");
    let wall = models
        .resolve_skull(&SkullSpawn {
            orientation: lodestone_render::SkullOrientation::Wall {
                facing_yaw_deg: 0.0,
            },
            ..SkullSpawn::at(SKULL)
        })
        .expect("wall");
    let mesh = models.get(floor.model).expect("mesh");
    let view_proj = camera.view_projection();

    let floor_rect = posed_screen_rect(mesh, &floor.part_transforms, view_proj);
    let wall_rect = posed_screen_rect(mesh, &wall.part_transforms, view_proj);
    println!("floor rect {floor_rect:?}, wall rect {wall_rect:?}");
    assert_ne!(
        floor_rect, wall_rect,
        "a floor and a wall skull at the same block position projected to the \
         *identical* rect — one placement matrix is silently substituting for \
         the other"
    );

    // Confirm this is visible in real pixels too, not just in the projected
    // rect maths: both frames must actually change something, and where they
    // change must differ.
    let shoot = |spawn: SkullSpawn| -> Vec<u8> {
        let mut target = HeadlessTarget::new(device, W, H, format);
        let mut state = RenderState::new(device, queue, format, W, H, None);
        state.set_skull_source(move |_eye| vec![spawn]);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };
    let floor_px = shoot(SkullSpawn::at(SKULL));
    let wall_px = shoot(SkullSpawn {
        orientation: lodestone_render::SkullOrientation::Wall {
            facing_yaw_deg: 0.0,
        },
        ..SkullSpawn::at(SKULL)
    });
    let (diff_rect, diff_count) = changed_bbox(&floor_px, &wall_px)
        .expect("a floor and a wall skull produced pixel-identical frames");
    println!("floor-vs-wall changed bbox {diff_rect:?} ({diff_count} px)");
}

/// What else already paints here — **measured**, not assumed.
///
/// `CLAUDE.md` records a control that asserted a frame "clears uniformly" and
/// failed at 3.5% because of the unconditional first-person bare arm. That
/// arm is drawn in this gate's frames too (nothing installs a third-person
/// body). This test locates it and asserts it is disjoint from the skull's
/// rect, so the sibling gate's clean-control premise is a measurement rather
/// than a hope.
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
        .expect("the arm draws, so a skull-free frame is not uniformly sky");
    println!("arm bbox {arm_rect:?} ({arm_count} px)");

    let models = BlockEntityModelSet::load();
    let instance = models
        .resolve_skull(&SkullSpawn::at(SKULL))
        .expect("single skull");
    let mesh = models.get(instance.model).expect("mesh");
    let skull_rect =
        posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection());

    assert!(
        !arm_rect.intersects(skull_rect),
        "the first-person arm ({arm_rect:?}) overlaps the skull's rect \
         ({skull_rect:?}). The sibling gate would then be measuring the arm, \
         which is exactly the false-control failure `CLAUDE.md` records. Move \
         the skull or the camera; do not relax the assertion."
    );
}
