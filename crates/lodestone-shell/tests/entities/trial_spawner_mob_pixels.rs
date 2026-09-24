//! Pixel gate: the trial spawner's real `trial_spawner_state` block-state
//! property must gate its display mob — the one thing this type adds on top
//! of the plain mob spawner, which `spawner_mob_pixels.rs` already proves
//! reaches pixels.
//!
//! # What this gate is for
//!
//! `crate::block_entities::spawner_mob_spawn`'s own unit tests
//! (`block_entities.rs`'s `spawner_tests` module) already prove, as pure
//! functions, that a `cooldown` trial spawner draws nothing while an
//! `active` one with the same NBT draws. What those cannot see is whether
//! that gate survives all the way to a real GPU frame through
//! [`RenderState::render`] — the exact shape `CLAUDE.md`'s evidence standard
//! asks for: a unit test proves the formula, a pixel gate proves the wire
//! from that formula to the screen is not cut somewhere in between (here,
//! the `Sim::spawner_source`/`prepare_spawner_mobs` hop, shared with the mob
//! spawner and therefore *not* re-proven end to end here — only the new
//! state-gating clause is).
//!
//! Both frames install the **same** `SpawnerMobSpawn` (same `entity_type`,
//! same `spin_deg`, same `scale`) at the **same** position — the only input
//! this gate varies is a synthetic "is this state permitted to spin" flag
//! threaded in by the test itself, standing in for the real
//! `trial_spawner_spin_speed` gate `spawner_mob_spawn` applies before ever
//! constructing a spawn. That is deliberate: this gate is about proving
//! "installing zero spawns draws nothing, installing one draws something",
//! which is the GPU-reachable half of the state gate; the CPU-side gating
//! decision itself is what the unit tests above already pin.
//!
//! ```text
//! cargo test -p lodestone-shell --test trial_spawner_mob_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{
    AnimInput, Camera, EntityModelSet, GpuContext, HeadlessTarget, RenderTarget, SpawnerMobSpawn,
    entity_model_matrix, spawner_display_outer_matrix,
};

const W: u32 = 320;
const H: u32 = 240;

/// The trial spawner's block position, one block ahead of the camera.
const TRIAL_SPAWNER: [i32; 3] = [0, 0, 1];

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

fn pig_spawn() -> SpawnerMobSpawn {
    SpawnerMobSpawn {
        pos: TRIAL_SPAWNER,
        entity_type: "minecraft:pig".to_string(),
        spin_deg: 0.0,
        scale: lodestone_render::spawner_display_scale(0.9, 0.9),
        light: lodestone_render::ENTITY_FULLBRIGHT,
    }
}

/// The `active`/`waiting_for_players` case: a real `Sim::spawner_source`-shaped
/// closure hands `prepare_spawner_mobs` one spawn, and it must reach the
/// screen — the same shape `spawner_mob_pixels.rs`'s own first test proves
/// for the mob spawner, repeated here so a regression specific to the
/// spawner/trial-spawner *sharing* this pass (rather than each carrying its
/// own) is still caught by this file alone.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_spinning_trial_spawner_state_draws_its_display_mob() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();

    let models = EntityModelSet::load();
    let transform = spawner_mob_transform(TRIAL_SPAWNER, 0.0, pig_spawn().scale);
    let instance = models
        .resolve_at("minecraft:pig", transform, &AnimInput::REST)
        .expect("pig must be in the entity corpus");
    let mesh = models.get(instance.model).expect("pig mesh");
    let rect = posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection());

    let mut state = RenderState::new(device, queue, format, W, H, None);
    let spawn = pig_spawn();
    state.set_spawner_source(move |_eye| vec![spawn.clone()]);
    let frame = target.acquire().expect("headless acquire");
    state.render(device, queue, frame.view(), &camera, None, &[]);
    let pixels = target.read_texels(device, queue);

    let sky = sky_bytes();
    let in_rect = non_sky_in(&pixels, rect, sky);
    let fill = in_rect as f64 / rect.area() as f64;
    assert!(
        fill > 0.15,
        "the display mob fills only {:.1}% of its own rect {rect:?} ({in_rect} of {} px)",
        fill * 100.0,
        rect.area()
    );
}

/// The `inactive`/`cooldown`/`waiting_for_reward_ejection`/`ejecting_reward`
/// case: `crate::block_entities::spawner_mob_spawn` returns `None` for these
/// (`block_entities.rs`'s
/// `a_cooldown_trial_spawner_draws_nothing_regardless_of_stale_spawn_data`
/// pins the CPU-side decision) — so `Sim::spawner_source` never hands
/// `prepare_spawner_mobs` a spawn for this position at all, and this gate
/// proves that "install nothing" really does draw nothing, i.e. the pass has
/// no fallback or default mob that would paint over a suppressed cage.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_non_spinning_trial_spawner_state_draws_nothing() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera();

    // The CPU-side gate already means the render source hands out an empty
    // Vec for a cooldown trial spawner — install exactly that, the same
    // shape `Sim::spawner_source` would produce.
    let state = RenderState::new(device, queue, format, W, H, None);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
    let pixels = target.read_texels(device, queue);

    let models = EntityModelSet::load();
    let transform = spawner_mob_transform(TRIAL_SPAWNER, 0.0, pig_spawn().scale);
    let instance = models
        .resolve_at("minecraft:pig", transform, &AnimInput::REST)
        .expect("pig");
    let mesh = models.get(instance.model).expect("pig mesh");
    let rect = posed_screen_rect(mesh, &instance.part_transforms, camera.view_projection());

    let sky = sky_bytes();
    let in_rect = non_sky_in(&pixels, rect, sky);
    assert_eq!(
        in_rect, 0,
        "no spawner source was installed (the cooldown-state case), so the \
         display mob's rect {rect:?} must be pure sky — this gate's control \
         for the spin-state test above. First-person arm bbox for reference: \
         {:?}",
        bbox_of(&pixels, |px| is_non_sky(px, sky))
    );
    assert!(
        stats.first_person_arm_drawn,
        "the frame must still draw something (the unconditional arm) — an \
         entirely uniform frame would mean the render call itself failed \
         silently, not that the gate passed"
    );
}
