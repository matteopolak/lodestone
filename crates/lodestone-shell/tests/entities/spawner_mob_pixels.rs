//! Pixel gate: the mob spawner's miniature display mob must **draw**, at its
//! own nested/shrunk/tilted placement, through the real
//! [`RenderState::render`] path — the same call `app.rs`'s frame loop makes.
//!
//! # Why this pass is not a `BlockEntityModelSet` gate
//!
//! Unlike chest/skull/bell, the spawner's cage is **not** a hole in the
//! world and does not go through `prepare_block_entities` at all —
//! `models/block/spawner.json` is a real `cube_all_inner_faces` block model,
//! drawn by the ordinary terrain mesher (untested here, since this harness
//! has no terrain world; see `docs/block-entity-renderers.md`'s Mob spawner
//! section). What this gate proves is the **miniature mob** —
//! `gpu/spawner_mobs.rs`'s `prepare_spawner_mobs`, which resolves through the
//! *ordinary* `EntityModelSet`/`EntityPipeline` mobs already use, at a nested
//! placement [`lodestone_render::spawner_display_outer_matrix`] builds.
//!
//! # The metric
//!
//! Same shape as `bell_block_entity_pixels.rs`: a differential (subject minus
//! control) whose changed bbox must fall inside the expected rect, plus an
//! absolute fill measurement inside that rect. The expected rect is projected
//! from the mob's **real baked vertices**, through the *same* composition
//! `prepare_spawner_mobs` builds (`spawner_display_outer_matrix` composed
//! with `entity_model_matrix` at the origin) and the *same*
//! `Camera::view_projection` the render call uses — never a remembered
//! literal.
//!
//! `minecraft:pig` is the subject rather than `zombie`: a pig's base hitbox
//! (`0.9 × 0.9`) sits at or under `spawner_display_scale`'s `1.0` threshold,
//! so it draws at the **un-shrunk** `0.53125` scale — the largest a display
//! mob ever gets, and the easiest for a hermetic 320×240 frame to measure.
//!
//! # A spin is an animation — the second test needs two known phases
//!
//! `spawner_spin_degrees`'s own unit tests (`lodestone-render`) already
//! predict the interpolated angle from `(o_spin, spin, partial_tick)`
//! exactly. What those cannot see is whether the angle actually reaches
//! rendered pixels: this gate's second test renders the same spawn at two
//! different `spin_deg` values, `0.0` and `90.0` — chosen as the formula's
//! own quarter-turn, not a value picked because it "looks right" — and
//! asserts the frames differ, localised inside the mob's own (padded) rect.
//!
//! ```text
//! cargo test -p lodestone-shell --test spawner_mob_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{
    AnimInput, Camera, EntityModelSet, GpuContext, HeadlessTarget, RenderTarget, SpawnerMobSpawn,
    entity_model_matrix, spawner_display_outer_matrix,
};

const W: u32 = 320;
const H: u32 = 240;

/// The spawner's block position, one block ahead of the camera on `+Z`.
const SPAWNER: [i32; 3] = [0, 0, 1];

/// A pig's real base hitbox (`lodestone_data::entity_dimensions`), pinned
/// here so the expected-scale computation below and `spawner_mob_scale`'s own
/// resolution cannot silently drift apart without this gate noticing —
/// `spawner.rs`'s own unit tests already predict `spawner_display_scale(0.9,
/// 0.9) == 0.53125` from these two numbers.
const PIG_WIDTH: f32 = 0.9;
const PIG_HEIGHT: f32 = 0.9;

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

/// The exact composition `gpu/spawner_mobs.rs`'s `prepare_spawner_mobs`
/// builds: block translation, then vanilla's spin/tilt/shrink pose stack,
/// then the entity's own placement at the origin.
fn spawner_mob_transform(pos: [i32; 3], spin_deg: f32, scale: f32) -> glam::Mat4 {
    let block_translate = glam::Mat4::from_translation(glam::Vec3::new(
        pos[0] as f32,
        pos[1] as f32,
        pos[2] as f32,
    ));
    block_translate
        * spawner_display_outer_matrix(spin_deg, scale)
        * entity_model_matrix(glam::Vec3::ZERO, 0.0, 1.0)
}

/// The screen rect of a posed entity mesh, projected from its **real baked
/// vertices**, mirroring `bell_block_entity_pixels.rs`'s `posed_screen_rect`.
fn posed_screen_rect(
    mesh: &lodestone_render::EntityMesh,
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

/// Eye one block back on `-Z` from the spawner, at roughly the cage's own
/// mid-height, looking straight down `+Z` (yaw `0` faces `+Z`).
fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, 0.35, 0.0),
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

fn pig_spawn(spin_deg: f32) -> SpawnerMobSpawn {
    SpawnerMobSpawn {
        pos: SPAWNER,
        entity_type: "minecraft:pig".to_string(),
        spin_deg,
        scale: lodestone_render::spawner_display_scale(PIG_WIDTH, PIG_HEIGHT),
        light: lodestone_render::ENTITY_FULLBRIGHT,
    }
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_spawner_mob_draws_at_its_nested_placement() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();

    // --- The expected rect, from the real corpus mesh and the real nested
    // transform `prepare_spawner_mobs` composes. ---
    let models = EntityModelSet::load();
    let transform = spawner_mob_transform(SPAWNER, 0.0, pig_spawn(0.0).scale);
    let instance = models
        .resolve_at("minecraft:pig", transform, &AnimInput::REST)
        .expect("pig must be in the entity corpus");
    let mesh = models.get(instance.model).expect("pig mesh");
    let rect = posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection());
    println!("spawner mob rect (from real baked vertices): {rect:?}");
    assert!(
        rect.area() > 50,
        "the display mob projects to only {} px — this gate cannot measure \
         anything that small, so the camera, not the renderer, is wrong: {rect:?}",
        rect.area()
    );

    // --- Subject: the source installed. Control: no source at all. ---
    let mut shoot = |install: bool| -> Vec<u8> {
        let mut state = RenderState::new(device, queue, format, W, H, None);
        if install {
            let spawn = pig_spawn(0.0);
            state.set_spawner_source(move |_eye| vec![spawn.clone()]);
        }
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let subject_px = shoot(true);
    let control_px = shoot(false);
    let sky = sky_bytes();

    // --- Absolute, inside the rect. The control's premise, measured. ---
    let control_in_rect = non_sky_in(&control_px, rect, sky);
    assert_eq!(
        control_in_rect, 0,
        "the control paints {control_in_rect} px inside the display mob's own \
         rect {rect:?} — something else draws there, so this gate would be \
         measuring that instead. Control frame's whole non-sky bbox: {:?}",
        bbox_of(&control_px, |px| is_non_sky(px, sky))
    );

    let subject_in_rect = non_sky_in(&subject_px, rect, sky);
    let fill = subject_in_rect as f64 / rect.area() as f64;
    assert!(
        fill > 0.15,
        "the display mob fills only {:.1}% of its own projected rect {rect:?} \
         ({subject_in_rect} of {} px). Subject's non-sky bbox: {:?}",
        fill * 100.0,
        rect.area(),
        bbox_of(&subject_px, |px| is_non_sky(px, sky))
    );

    // --- Differential: every changed pixel must *be* the display mob. ---
    let (changed_rect, changed_count) = changed_bbox(&subject_px, &control_px).expect(
        "installing a spawner source changed no pixel at all — the pass is dead",
    );
    println!("changed bbox {changed_rect:?} ({changed_count} px), fill {fill:.3}");
    let allowed = rect.padded(4);
    assert!(
        allowed.x0 <= changed_rect.x0
            && allowed.y0 <= changed_rect.y0
            && changed_rect.x1 <= allowed.x1
            && changed_rect.y1 <= allowed.y1,
        "pixels changed outside the display mob's projected rect: changed \
         {changed_rect:?}, allowed {allowed:?}. Installing a spawner source \
         must not repaint anything else in the frame."
    );
}

/// The spin must move real, rendered pixels — not merely produce a different
/// `spawner_spin_degrees` number (`lodestone-render`'s own unit tests already
/// predict that formula exactly). Two known phases, `0°` and `90°` — the
/// formula's own quarter turn, not a value chosen because it "looks right".
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn spinning_the_display_mob_moves_pixels_inside_its_own_padded_rect() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let camera = camera();

    let models = EntityModelSet::load();
    let view_proj = camera.view_projection();
    // The **union** of the real projected rect at both phases, not the
    // resting rect padded by a guessed margin: a 90-degree yaw swings a
    // pig's snout/tail to a genuinely different silhouette, and the union of
    // two real measurements is what "the change stays local to the display
    // mob" actually means — a fixed pixel pad is exactly the kind of
    // plausible-round-number guess `CLAUDE.md`'s evidence standard warns
    // against.
    let rect_at = |spin_deg: f32| {
        let transform = spawner_mob_transform(SPAWNER, spin_deg, pig_spawn(spin_deg).scale);
        let instance = models
            .resolve_at("minecraft:pig", transform, &AnimInput::REST)
            .expect("pig");
        let mesh = models.get(instance.model).expect("pig mesh");
        posed_screen_rect(mesh, &instance.part_transforms, view_proj)
    };
    let rest_rect = rect_at(0.0);
    let spun_rect = rect_at(90.0);
    let union_rect = Rect {
        x0: rest_rect.x0.min(spun_rect.x0),
        y0: rest_rect.y0.min(spun_rect.y0),
        x1: rest_rect.x1.max(spun_rect.x1),
        y1: rest_rect.y1.max(spun_rect.y1),
    };

    let shoot = |spawn: SpawnerMobSpawn| -> Vec<u8> {
        let mut target = HeadlessTarget::new(device, W, H, format);
        let mut state = RenderState::new(device, queue, format, W, H, None);
        state.set_spawner_source(move |_eye| vec![spawn.clone()]);
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };

    let resting_px = shoot(pig_spawn(0.0));
    let spun_px = shoot(pig_spawn(90.0));

    let (diff_rect, diff_count) = changed_bbox(&resting_px, &spun_px).expect(
        "spin_deg = 0 and spin_deg = 90 produced pixel-identical frames — the \
         spin angle is computed but never reaches the mesh",
    );
    println!("resting-vs-spun changed bbox {diff_rect:?} ({diff_count} px)");

    // A small pad on top of the real union for anti-aliasing/rounding at
    // the silhouette edge, not for the rotation itself.
    let allowed = union_rect.padded(4);
    assert!(
        allowed.x0 <= diff_rect.x0
            && allowed.y0 <= diff_rect.y0
            && diff_rect.x1 <= allowed.x1
            && diff_rect.y1 <= allowed.y1,
        "the spin changed pixels outside the display mob's own rect: changed \
         {diff_rect:?}, allowed {allowed:?} — a real spin must not repaint \
         anything else"
    );
}

/// What else already paints here — measured, not assumed, the same discipline
/// `bell_block_entity_pixels.rs`'s sibling test records.
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
        "this test's premise is that the arm paints unconditionally; if it \
         does not, the sibling gates' clean-control premise is clean for a \
         different reason than claimed"
    );

    let sky = sky_bytes();
    let (arm_rect, _arm_count) = bbox_of(&pixels, |px| is_non_sky(px, sky))
        .expect("the arm draws, so a spawner-free frame is not uniformly sky");
    println!("arm bbox {arm_rect:?}");

    let models = EntityModelSet::load();
    let transform = spawner_mob_transform(SPAWNER, 0.0, pig_spawn(0.0).scale);
    let instance = models
        .resolve_at("minecraft:pig", transform, &AnimInput::REST)
        .expect("pig");
    let mesh = models.get(instance.model).expect("pig mesh");
    let rect = posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection());

    assert!(
        !arm_rect.intersects(rect),
        "the first-person arm ({arm_rect:?}) overlaps the display mob's rect \
         ({rect:?}). The sibling gates would then be measuring the arm. Move \
         the spawner or the camera; do not relax the assertion."
    );
}
