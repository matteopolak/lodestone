//! Pixel gate: a copper golem statue must **draw**, in its own projected
//! screen rect, through the real [`RenderState::render`] path — the same
//! call `app.rs`'s frame loop makes.
//!
//! Closes the verification gap `docs/block-entity-renderers.md`'s
//! "A note on verification scope" names: the statue landed with real
//! CPU-side unit tests (pose algebra across four poses, the `det == +1`
//! flip check) and a real wiring chain, but no `#[ignore]`d GPU pixel gate.
//!
//! # Why this gate is the whole point of the change
//!
//! `assets/minecraft/models/block/copper_golem_statue.json` is zero-element
//! — the same "hole in the world" shape that made chest and skull first.
//! `lodestone-render`'s/`lodestone-assets`' own unit tests prove the bake
//! and every placement matrix, but none of them calls
//! `RenderState::prepare_block_entities` — a closed loop with respect to
//! this crate. This gate drives the real shell path instead.
//!
//! # The metric, and why it is a rect and not a fraction
//!
//! Same shape `skull_block_entity_pixels.rs`/`chest_block_entity_pixels.rs`
//! use: the expected rect is projected from the **real baked vertices** of
//! the real corpus mesh (`standing` pose), through the same
//! [`Camera::view_projection`] the render call uses and the same
//! `part_transforms` [`BlockEntityModelSet::resolve_copper_golem_statue`]
//! produces — never a remembered literal.
//!
//! # Two known poses, not one frame
//!
//! `standing`/`running`/`sitting`/`star` select among four **independently
//! transcribed** rigs (see `docs/block-entity-renderers.md`'s Copper golem
//! statue section) — a real risk that one pose's builder silently falls
//! back to another's. `standing` vs `star` (the two most different
//! silhouettes — the star pose spreads all four limbs) must project to
//! different rects.
//!
//! ```text
//! cargo test -p lodestone-shell --test copper_golem_statue_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{
    BlockEntityMesh, BlockEntityModelSet, Camera, CopperGolemOxidation, CopperGolemPose,
    CopperGolemStatueSpawn, GpuContext, HeadlessTarget, RenderTarget,
};

const W: u32 = 320;
const H: u32 = 240;

/// The statue's block position, three blocks ahead of the camera on `+Z`.
const STATUE: [i32; 3] = [0, 0, 3];

/// Manhattan RGB distance above which a pixel counts as "not the clear
/// colour". Matches every sibling block-entity pixel gate's threshold.
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

/// The screen rect of a posed mesh, projected from its real baked vertices
/// through the very `part_transforms` the draw uses. Mirrors
/// `skull_block_entity_pixels.rs`'s helper of the same name.
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

/// Eye near the statue's own mid-height, three blocks back on `-Z`, looking
/// straight down `+Z` (yaw `0` faces `+Z`).
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, 0.9, 0.0),
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

fn test_spawn(pose: CopperGolemPose) -> CopperGolemStatueSpawn {
    CopperGolemStatueSpawn {
        pos: STATUE,
        facing_yaw_deg: 0.0,
        pose,
        oxidation: CopperGolemOxidation::Unaffected,
        light: lodestone_render::ENTITY_FULLBRIGHT,
    }
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_copper_golem_statue_draws_in_its_own_screen_rect_where_no_block_model_could() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();

    // --- The expected rect, from the real corpus mesh and the real matrices ---
    let models = BlockEntityModelSet::load();
    let spawn = test_spawn(CopperGolemPose::Standing);
    let instance = models
        .resolve_copper_golem_statue(&spawn)
        .expect("the standing copper golem model must be in the corpus");
    let mesh = models.get(instance.model).expect("mesh");
    let statue_rect = posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection());
    println!("statue rect (from real baked vertices): {statue_rect:?}");
    assert!(
        statue_rect.area() > 200,
        "the statue projects to only {} px — this gate cannot measure anything \
         that small, so the camera, not the renderer, is wrong: {statue_rect:?}",
        statue_rect.area()
    );

    // --- Subject: the source installed. Control: no source at all. -----------
    let mut shoot = |install: bool| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        if install {
            state.set_copper_golem_statue_source(move |_eye| {
                vec![test_spawn(CopperGolemPose::Standing)]
            });
        }
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
        (target.read_texels(device, queue), stats)
    };

    let (subject_px, subject_stats) = shoot(true);
    let (control_px, control_stats) = shoot(false);

    assert_eq!(
        subject_stats.block_entities_drawn, 1,
        "the source is installed and the statue is in front of the camera"
    );
    assert_eq!(subject_stats.block_entities_culled, 0);
    assert_eq!(
        control_stats.block_entities_drawn, 0,
        "RenderState::new must not default to an installed copper golem statue source"
    );

    let sky = sky_bytes();

    // --- Absolute, inside the rect. The control's premise, measured. ---------
    let control_in_rect = non_sky_in(&control_px, statue_rect, sky);
    assert_eq!(
        control_in_rect, 0,
        "the control paints {control_in_rect} px inside the statue's own rect \
         {statue_rect:?} — something *else* draws there. Control frame's whole \
         non-sky bbox: {:?}",
        bbox_of(&control_px, |px| is_non_sky(px, sky))
    );

    let subject_in_rect = non_sky_in(&subject_px, statue_rect, sky);
    let fill = subject_in_rect as f64 / statue_rect.area() as f64;
    assert!(
        fill > 0.30,
        "the statue fills only {:.1}% of its own projected rect {statue_rect:?} \
         ({subject_in_rect} of {} px). Subject's non-sky bbox: {:?}",
        fill * 100.0,
        statue_rect.area(),
        bbox_of(&subject_px, |px| is_non_sky(px, sky))
    );

    // --- Differential: every changed pixel must *be* the statue. -------------
    let (changed_rect, changed_count) = changed_bbox(&subject_px, &control_px)
        .expect("installing a copper golem statue source changed no pixel at all — the pass is dead");
    println!("changed bbox {changed_rect:?} ({changed_count} px), fill {fill:.3}");
    let allowed = statue_rect.padded(2);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the statue's projected rect: changed {changed_rect:?}, \
         allowed {allowed:?}. Installing a copper golem statue source must not repaint \
         anything else in the frame."
    );
}

/// The `standing` and `star` poses are **independently transcribed**
/// `LayerDefinition`s, not one rig with a pose preset (see
/// `docs/block-entity-renderers.md`'s Copper golem statue section) — a real
/// risk that `copper_golem_pose` silently resolves the wrong model name and
/// one pose draws as another. `standing` (unnested seven-part tree) and
/// `star` (arms/legs nested, all four limbs spread) must project to
/// genuinely different rects, and the real rendered frames must differ too.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn standing_and_star_poses_project_to_different_rects() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let camera = camera();

    let models = BlockEntityModelSet::load();
    let standing = models
        .resolve_copper_golem_statue(&test_spawn(CopperGolemPose::Standing))
        .expect("standing");
    let star = models
        .resolve_copper_golem_statue(&test_spawn(CopperGolemPose::Star))
        .expect("star");
    let view_proj = camera.view_projection();

    let standing_mesh = models.get(standing.model).expect("standing mesh");
    let star_mesh = models.get(star.model).expect("star mesh");

    let standing_rect = posed_screen_rect(standing_mesh, &standing.part_transforms, view_proj);
    let star_rect = posed_screen_rect(star_mesh, &star.part_transforms, view_proj);
    println!("standing rect {standing_rect:?}, star rect {star_rect:?}");
    assert_ne!(
        standing_rect, star_rect,
        "the standing and star poses projected to the *identical* rect — one \
         pose's model resolution is silently substituting for the other"
    );

    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut shoot = |pose: CopperGolemPose| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        state.set_copper_golem_statue_source(move |_eye| vec![test_spawn(pose)]);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };
    let standing_px = shoot(CopperGolemPose::Standing);
    let star_px = shoot(CopperGolemPose::Star);
    let (diff_rect, diff_count) = changed_bbox(&standing_px, &star_px)
        .expect("the standing and star poses produced pixel-identical frames");
    println!("standing-vs-star changed bbox {diff_rect:?} ({diff_count} px)");
}

/// Same discipline every sibling gate in this crate documents: before
/// trusting "the control is clean", locate the unconditional first-person
/// arm and assert it is disjoint from the statue's rect.
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
        .expect("the arm draws, so a statue-free frame is not uniformly sky");
    println!("arm bbox {arm_rect:?} ({arm_count} px)");

    let models = BlockEntityModelSet::load();
    let instance = models
        .resolve_copper_golem_statue(&test_spawn(CopperGolemPose::Standing))
        .expect("single statue");
    let mesh = models.get(instance.model).expect("mesh");
    let statue_rect =
        posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection());

    assert!(
        !arm_rect.intersects(statue_rect),
        "the first-person arm ({arm_rect:?}) overlaps the statue's rect \
         ({statue_rect:?}). The sibling gate would then be measuring the arm, \
         which is exactly the false-control failure `CLAUDE.md` records. Move \
         the statue or the camera; do not relax the assertion."
    );
}
