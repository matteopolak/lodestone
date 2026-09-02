//! Reproduction for the owner's *near*-range report: **"for the blocks at a
//! weird angle (for example, at the same level that I'm standing but just
//! 20ish blocks away) they generally don't render much of it at all,
//! sometimes only 60% of the block (uniformly missing pixels and showing sky
//! instead)"**.
//!
//! # Why this is a different gate from the three that came before it
//!
//! `distant_flat_terrain_holes.rs`, `uneven_terrain_holes.rs` and
//! `far_grazing_ceiling_floor_holes.rs` all swept **far** range (10 to 24
//! chunks) over flat or stepped ground, and all three reached a clean verdict
//! once fog was accounted for. None of them can speak to this report: at
//! **20 blocks** no fog term is anywhere near onset, and the reported symptom
//! is not a missing region but *scattered* pixels inside a face that is
//! otherwise drawn — which is a per-fragment decision, not a cull.
//!
//! # The fixture, and why it is shaped like this
//!
//! Near **and** grazing are in tension on a plane: a planar surface seen at
//! incidence `θ` from perpendicular offset `p` is `p / cos θ` away, so a
//! grazing view of something 20 blocks off needs the surface to pass within a
//! few blocks of the eye. That is exactly the everyday case the owner
//! describes (standing beside a wall or a ledge and looking along it), and it
//! is the one geometry none of the prior gates built.
//!
//! So the world carries three shapes, all full cubes so a ray-vs-cell oracle
//! is exact:
//!
//! * a flat floor, `[0, FLOOR_TOP)`;
//! * a one-block-tall, one-block-wide **wall** running away from the eye at a
//!   perpendicular offset of `WALL_X` blocks, whose vertical face is seen at
//!   **82 degrees** of incidence at 20 blocks, and whose *top* face sits
//!   `0.62` blocks below eye level and so is seen at **1.8 degrees** — the
//!   report's "at the same level that I'm standing";
//! * six **floating single blocks** straddling eye level at 15 to 30 blocks,
//!   which is the "corners of blocks far away" half.
//!
//! # The statistic: area, not presence
//!
//! Not a sandwiched-sky heuristic, and not a binary "did anything paint here"
//! either. The report is a **magnitude** claim — *only 60% of the block* — and
//! a presence test passes at 60% exactly as it passes at 100%.
//!
//! So every pixel is ray-cast against the real block data (the oracle
//! `far_grazing_ceiling_floor_holes.rs` already carries, transcribed from
//! `Camera::basis`'s closed form and reading `World::block_state_at` — no
//! mesher, no rasteriser, no renderer code), and every pixel whose
//! 8-neighbourhood is not uniform is re-cast on a 4x4 sub-grid for its
//! **sub-pixel coverage**. The gate then compares painted area against oracle
//! area, per surface.
//!
//! The first version of this gate did the obvious thing instead — a centre ray
//! per pixel, then erode the mask by one pixel so the silhouette (where a
//! centre-sample and the rasteriser's coverage rule may legitimately disagree)
//! cannot produce a false failure. That is sound and it is also **blind to the
//! report**: a face at 82 degrees of incidence is *mostly* silhouette, so
//! erosion asserts only on the part of the image least able to fail. Both
//! versions were run and both are clean; only the second one could have failed.
//!
//! # What this measured
//!
//! Two block families (`stone`, `grass_block`), six camera configurations, six
//! surfaces each — 48 of the 72 carrying enough oracle area to measure. Painted
//! area over oracle area:
//!
//! | surface | ratio |
//! |---|---|
//! | wall side face, 82 deg incidence at 20 blocks | 1.0000 |
//! | floating blocks straddling eye level, 15-30 blocks | 0.9798 - 0.9882 |
//! | floor, 10-20 / 20-30 / 30-45 / 45-60 blocks | 1.0000 |
//!
//! The floaters' 2% is their own silhouette: painted area is counted per whole
//! pixel and oracle area is sub-pixel, and those are the only surfaces here
//! whose perimeter is a large fraction of their area (each is a single block
//! 15-30 blocks out). Its bounding box is a thin horizontal band across the
//! blocks' own rows and its contiguity is 0.97-1.00 — an outline, not speckle.
//!
//! So at the report's own conditions, and for **full-cube opaque blocks**, this
//! renderer paints what the geometry says it should. That rules out, for this
//! family: missing geometry, the distance/frustum/occlusion culls, the depth
//! test, `model.wgsl`'s cutout discard firing on an opaque sprite, atlas gutter
//! bleed, and fog. It says nothing about cutout blocks (leaves, cross plants,
//! panes), whose visibility genuinely is decided by filtered alpha — see
//! `cutout_minification_flicker_pixels`, which measures a 0.62-0.66 painted
//! ratio in its second-most-minified band and records it as an open residual.
//!
//! # The control, and what it calibrates
//!
//! [`a_deliberately_missing_section_is_detected`] skips one section's upload.
//! It fires: the floaters' ratio falls to **0.712** and the wall's to 0.9914,
//! with 648.8 and 207.0 missing area, **contiguity 1.000** in both. That last
//! number is the calibration the shape statistic needs — a lost *region* reads
//! as 1.0, so a clean run's contiguity is not evidence of anything on its own,
//! but a *failing* run's low contiguity would be.
//!
//! # Scope, stated plainly
//!
//! This proves the **draw**: real baked geometry from the vanilla
//! `client.jar`, the real mesher, the real pipeline, the real atlas and its
//! real mip chain, against an independent oracle. It installs no wire and no
//! ECS input, so it cannot speak to anything that goes wrong between a chunk
//! packet and `World`.
//!
//! Fail-closed: no GPU adapter and no vanilla `client.jar` are failures, never
//! skips.
//!
//! ```text
//! cargo test -p lodestone-shell --test near_grazing_face_coverage_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR, ThirdPersonBodyState};
use lodestone::mesher::{SectionGeometry, SectionKey, mesh_snapshot_models, snapshot_section, snapshot_visibility};
use lodestone::resources::BlockResources;
use lodestone_render::{
    Camera, GpuContext, HeadlessTarget, ModelMesh, RenderTarget, cull::within_view_distance,
    entity_anim::AnimInput, fog::FogSettings,
};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

const W: u32 = 640;
const H: u32 = 480;
/// Vanilla's shipped default, not the 40 the far gates used: this report is
/// about what the owner sees.
const FOV_Y_DEGREES: f32 = 70.0;
const RD_CHUNKS: i32 = 8;
const MIN_Y: i32 = 0;
const FLOOR_TOP: i32 = 64;
const SECTION_COUNT: usize = 8;
/// Vanilla's standing eye height above the block a player is stood on.
const EYE_HEIGHT: f32 = 1.62;
/// The wall's perpendicular offset from the eye. Chosen so its vertical face
/// is at 82 degrees of incidence where it is 20 blocks away — near and
/// grazing at once, which a plane cannot be at any larger offset.
const WALL_X: i32 = 3;
/// Nothing past this is what this gate is about; the far gates own that range.
const NEAR_LIMIT: f32 = 60.0;

/// See `distant_flat_terrain_holes.rs`'s identical helper's doc: `RenderState`
/// draws an unconditional first-person bare arm at a fixed screen rect
/// whenever no third-person body is reported, which reads as a hole and is
/// not one.
fn suppress_first_person_arm(state: &mut RenderState) {
    state.set_third_person_body_source(|| {
        Some(ThirdPersonBodyState {
            player_skin: None,
            feet: glam::Vec3::new(0.0, -10_000.0, 0.0),
            body_yaw_deg: 0.0,
            anim: AnimInput::default(),
            scale: 1.0,
            swim_amount: 0.0,
            slim: false,
            equipment: Vec::new(),
            equipment_skin: Vec::new(),
        })
    });
}

const DIFFERS_FROM_REFERENCE: i32 = 60;

fn differs(subject: &[u8], reference: &[u8]) -> bool {
    let d = (i32::from(subject[0]) - i32::from(reference[0])).abs()
        + (i32::from(subject[1]) - i32::from(reference[1])).abs()
        + (i32::from(subject[2]) - i32::from(reference[2])).abs();
    d > DIFFERS_FROM_REFERENCE
}

fn first_state_named(name: &str) -> u32 {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} is not in the 26.2 block-state table"))
}

fn oracle_ray_dir_f(camera: &Camera, px: f32, py: f32, w: u32, h: u32) -> glam::Vec3 {
    let (sy, cy) = camera.yaw.to_radians().sin_cos();
    let (sp, cp) = camera.pitch.to_radians().sin_cos();
    let right = glam::Vec3::new(-cy, 0.0, -sy);
    let up = glam::Vec3::new(-sy * sp, cp, cy * sp);
    let forward = glam::Vec3::new(-sy * cp, -sp, cy * cp);
    let half_y = (camera.fov_y_degrees.to_radians() * 0.5).tan();
    let half_x = half_y * camera.aspect;
    let ndc_x = 2.0 * px / w as f32 - 1.0;
    let ndc_y = 1.0 - 2.0 * py / h as f32;
    (forward + right * (ndc_x * half_x) + up * (ndc_y * half_y)).normalize()
}

/// Amanatides-Woo: visits exactly the voxels the segment passes through.
/// Verbatim in mechanism from `far_grazing_ceiling_floor_holes.rs`, which
/// pins it against a fixed-step reference.
fn oracle_first_solid_cell(
    world: &World,
    air: u32,
    origin: glam::Vec3,
    dir: glam::Vec3,
    max_dist: f32,
) -> Option<([i32; 3], f32)> {
    let o = [origin.x, origin.y, origin.z];
    let d = [dir.x, dir.y, dir.z];
    let mut cell = [o[0].floor() as i32, o[1].floor() as i32, o[2].floor() as i32];
    let mut step = [0i32; 3];
    let mut t_max = [f32::INFINITY; 3];
    let mut t_delta = [f32::INFINITY; 3];
    for a in 0..3 {
        if d[a] > 0.0 {
            step[a] = 1;
            t_delta[a] = 1.0 / d[a];
            t_max[a] = ((cell[a] + 1) as f32 - o[a]) / d[a];
        } else if d[a] < 0.0 {
            step[a] = -1;
            t_delta[a] = -1.0 / d[a];
            t_max[a] = (cell[a] as f32 - o[a]) / d[a];
        }
    }
    let mut t = 0.0f32;
    while t < max_dist {
        if let Some(state) = world.block_state_at(cell[0], cell[1], cell[2]) {
            if state != air {
                return Some((cell, t));
            }
        }
        let axis = if t_max[0] < t_max[1] && t_max[0] < t_max[2] {
            0
        } else if t_max[1] < t_max[2] {
            1
        } else {
            2
        };
        if !t_max[axis].is_finite() {
            break;
        }
        t = t_max[axis];
        cell[axis] += step[axis];
        t_max[axis] += t_delta[axis];
    }
    None
}

/// What the oracle says a pixel sees: which surface class, and how much of
/// the pixel that class covers.
#[derive(Clone, Copy, Default)]
struct Sample {
    /// Index into [`CLASSES`], or `usize::MAX` for "background / not a class
    /// this gate measures".
    class: usize,
    /// Fraction of the pixel the oracle says that class covers, in `0..=1`.
    coverage: f32,
}

/// The surfaces this gate measures separately. Splitting them is the point:
/// an aggregate over the whole frame is dominated by the floor under the
/// camera's nose, which is neither near-grazing nor at eye level, and would
/// bury a 60%-covered grazing face in six figures of healthy pixels.
const CLASSES: &[&str] = &[
    "wall side face (82 deg incidence at 20 blocks)",
    "floating blocks at eye level, 15-30 blocks",
    "floor 10-20 blocks",
    "floor 20-30 blocks",
    "floor 30-45 blocks",
    "floor 45-60 blocks",
];

/// Classifies one oracle hit. `None` means "not a surface this gate measures".
fn classify(cell: [i32; 3], t: f32) -> Option<usize> {
    if cell[0] == WALL_X && cell[1] == FLOOR_TOP {
        return Some(0);
    }
    if cell[1] == FLOOR_TOP + 1 {
        return Some(1);
    }
    if cell[1] < FLOOR_TOP {
        return match t {
            t if (10.0..20.0).contains(&t) => Some(2),
            t if (20.0..30.0).contains(&t) => Some(3),
            t if (30.0..45.0).contains(&t) => Some(4),
            t if (45.0..60.0).contains(&t) => Some(5),
            _ => None,
        };
    }
    None
}

fn oracle_class_at(
    world: &World,
    air: u32,
    camera: &Camera,
    camera_chunk: (i32, i32),
    x: f32,
    y: f32,
) -> Option<usize> {
    let dir = oracle_ray_dir_f(camera, x, y, W, H);
    let (cell, t) = oracle_first_solid_cell(world, air, camera.position, dir, NEAR_LIMIT)?;
    let hit_chunk = (cell[0] >> 4, cell[2] >> 4);
    if !within_view_distance(camera_chunk, hit_chunk, RD_CHUNKS as u32) {
        return None;
    }
    classify(cell, t)
}

/// Per-pixel class and **sub-pixel coverage**, which is what makes this gate a
/// magnitude measurement rather than a binary one.
///
/// A single centre ray answers "is this pixel's centre inside the surface",
/// and a gate built on that has to erode the silhouette away to stay honest —
/// which is precisely where a grazing face lives, so it ends up asserting only
/// on the part of the image that cannot fail. Instead every pixel whose
/// 8-neighbourhood is not uniform is re-sampled on a 4x4 sub-grid, giving the
/// fraction of the pixel the oracle says the surface covers. The gate then
/// compares **painted area against oracle area** over a whole surface, so
/// "only 60% of the block renders" is a claim it can actually evaluate.
///
/// Sub-sampling only the boundary is not an approximation: a pixel all of
/// whose neighbours' centres agree with its own is interior, where the exact
/// coverage is 1 (or 0) by construction, and the cost of the honest version is
/// 16 ray casts for every pixel in the frame.
fn oracle_samples(world: &World, air: u32, camera: &Camera) -> Vec<Sample> {
    let camera_chunk = (
        (camera.position.x / 16.0).floor() as i32,
        (camera.position.z / 16.0).floor() as i32,
    );
    let mut centre = vec![usize::MAX; (W * H) as usize];
    for y in 0..H {
        for x in 0..W {
            if let Some(c) =
                oracle_class_at(world, air, camera, camera_chunk, x as f32 + 0.5, y as f32 + 0.5)
            {
                centre[(y * W + x) as usize] = c;
            }
        }
    }
    let mut out = vec![Sample { class: usize::MAX, coverage: 0.0 }; centre.len()];
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) as usize;
            let mine = centre[i];
            let mut uniform = x > 0 && y > 0 && x + 1 < W && y + 1 < H;
            if uniform {
                for dy in -1i32..=1 {
                    for dx in -1i32..=1 {
                        let n = ((y as i32 + dy) as u32 * W + (x as i32 + dx) as u32) as usize;
                        uniform &= centre[n] == mine;
                    }
                }
            }
            if uniform {
                out[i] = Sample { class: mine, coverage: if mine == usize::MAX { 0.0 } else { 1.0 } };
                continue;
            }
            // Boundary pixel: 4x4 sub-grid, and the class is whichever one
            // covers the most of it.
            let mut counts = vec![0u32; CLASSES.len()];
            for sy in 0..4 {
                for sx in 0..4 {
                    let fx = x as f32 + (sx as f32 + 0.5) / 4.0;
                    let fy = y as f32 + (sy as f32 + 0.5) / 4.0;
                    if let Some(c) = oracle_class_at(world, air, camera, camera_chunk, fx, fy) {
                        counts[c] += 1;
                    }
                }
            }
            let best = counts.iter().enumerate().max_by_key(|(_, n)| **n).map(|(i, _)| i);
            match best {
                Some(c) if counts[c] > 0 => {
                    out[i] = Sample { class: c, coverage: counts[c] as f32 / 16.0 };
                }
                _ => {}
            }
        }
    }
    out
}

fn slab_and_wall_world(block: u32, air: u32) -> World {
    let mut world = World::new();
    let radius = RD_CHUNKS;
    for cx in -radius..=radius {
        for cz in -radius..=radius {
            let column = ChunkColumn::new(
                MIN_Y,
                SECTION_COUNT,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                air,
                0,
            );
            let mut light = ColumnLight::new(SECTION_COUNT);
            for i in 0..light.light_section_count() {
                *light.sky_mut(i) = lodestone_world::LightData::Uniform(15);
                *light.block_mut(i) = lodestone_world::LightData::Uniform(0);
            }
            world.load(
                ChunkPos::new(cx, cz),
                LoadedChunk::new(column, light, Heightmaps::new(), Vec::new()),
            );
        }
    }
    let lo = -radius * 16;
    let hi = radius * 16 + 15;
    let floor = world.fill_region([lo, MIN_Y, lo], [hi, FLOOR_TOP - 1, hi], block);
    assert!(floor > 0, "fixture: floor must actually write blocks");

    // The wall: one block wide, one block tall, running away from the eye.
    // Its top face is 0.62 blocks below eye level; its `-x` face is seen at
    // 82 degrees of incidence where it is 20 blocks out.
    let wall = world.fill_region([WALL_X, FLOOR_TOP, -16], [WALL_X, FLOOR_TOP, 100], block);
    assert!(wall > 0, "fixture: wall must actually write blocks");

    // Floating single blocks straddling eye level, at 15 to 30 blocks.
    let eye_block = FLOOR_TOP + 1;
    for &(x, z) in FLOATERS {
        let n = world.fill_region([x, eye_block, z], [x, eye_block, z], block);
        assert_eq!(n, 1, "fixture: floater at {x},{z} must write exactly one block");
    }
    world
}

const FLOATERS: &[(i32, i32)] = &[(-6, 15), (-2, 20), (2, 25), (6, 30), (-10, 22), (10, 18)];

#[allow(clippy::too_many_arguments)]
fn upload_all(
    world: &World,
    models: &lodestone_render::BlockModels,
    state: &mut RenderState,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    skip: Option<SectionKey>,
) -> usize {
    let mut uploaded = 0usize;
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            for si in 0..SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: MIN_Y };
                if skip == Some(key) {
                    continue;
                }
                let Some(snap) = snapshot_section(world, key) else { continue };
                let opaque = mesh_snapshot_models(&snap, models, false);
                let visibility = snapshot_visibility(&snap, models);
                state.upload_section(
                    device,
                    queue,
                    key,
                    &SectionGeometry::Model {
                        opaque,
                        water: ModelMesh::default(),
                        translucent_blocks: ModelMesh::default(),
                        visibility,
                    },
                );
                uploaded += 1;
            }
        }
    }
    uploaded
}

/// Production fog, unchanged. At 20 blocks against an 8-chunk render distance
/// the render-distance ramp has not started (it runs 115.2 to 128) and the
/// environmental term is inert, so unlike the far gates this one has no need
/// to neutralise it — and leaving it in place is what keeps the fixture the
/// owner's own configuration.
fn production_fog(color: [f32; 3]) -> FogSettings {
    FogSettings::for_render_distance(color, RD_CHUNKS as u32)
}

const FADE_COMPLETE_TICK: u64 = 200;

#[allow(clippy::too_many_arguments)]
fn render_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    target: &mut HeadlessTarget,
    atlas: &lodestone_render::BlockAtlas,
    world: &World,
    models: &lodestone_render::BlockModels,
    camera: &Camera,
    upload_terrain: bool,
    skip: Option<SectionKey>,
) -> (Vec<u8>, lodestone::gpu::RenderStats) {
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas));
    suppress_first_person_arm(&mut state);
    state.set_fog(production_fog(SKY_COLOR), RD_CHUNKS as u32);
    if upload_terrain {
        let uploaded = upload_all(world, models, &mut state, device, queue, skip);
        assert!(uploaded > 0, "fixture: some sections must have uploaded");
    }
    state.update_animation(queue, FADE_COMPLETE_TICK);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), camera, None, &[]);
    (target.read_texels(device, queue), stats)
}

fn camera_at(pitch_degrees: f32, yaw_degrees: f32) -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, FLOOR_TOP as f32 + EYE_HEIGHT, 0.5),
        yaw: yaw_degrees,
        pitch: pitch_degrees,
        fov_y_degrees: FOV_Y_DEGREES,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS as u32, 0),
    }
}

fn gpu() -> GpuContext {
    GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    )
}

fn load_vanilla() -> (BlockResources, std::sync::Arc<lodestone_render::BlockAtlas>) {
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "vanilla assets did not load (banner: {:?}) — this gate needs a real client.jar \
             under .cache/mc/26.2 (LODESTONE_ASSETS)",
            resources.banner
        )
    });
    (resources, atlas)
}

/// One surface's verdict: how much of it the oracle says is on screen, how
/// much of it the renderer painted, and — when some is missing — what shape
/// the missing set has.
struct ClassReport {
    name: &'static str,
    /// Oracle area, in pixels (sub-pixel coverage summed).
    expected: f32,
    /// Pixels the renderer painted that the oracle assigns to this class.
    painted: f32,
    /// Oracle area the renderer left showing the background.
    missing: f32,
    /// Of the missing pixels, the fraction with at least one missing
    /// 4-neighbour. The control measures this at 1.000 for a section that was
    /// never uploaded, so a value near 1 means a lost *region* and a low value
    /// means scattered per-fragment loss — the two hypotheses the report is
    /// between.
    contiguity: f32,
    bbox: Option<(u32, u32, u32, u32)>,
}

fn analyse(subject: &[u8], reference: &[u8], samples: &[Sample]) -> Vec<ClassReport> {
    let mut expected = vec![0.0f32; CLASSES.len()];
    let mut painted = vec![0.0f32; CLASSES.len()];
    let mut missing = vec![0.0f32; CLASSES.len()];
    let mut bbox: Vec<Option<(u32, u32, u32, u32)>> = vec![None; CLASSES.len()];
    let mut missing_mask = vec![usize::MAX; samples.len()];
    for y in 0..H {
        for x in 0..W {
            let i = (y * W + x) as usize;
            let s = samples[i];
            if s.class == usize::MAX {
                continue;
            }
            expected[s.class] += s.coverage;
            let px = i * 4;
            if differs(&subject[px..px + 4], &reference[px..px + 4]) {
                painted[s.class] += s.coverage;
            } else {
                missing[s.class] += s.coverage;
                missing_mask[i] = s.class;
                bbox[s.class] = Some(match bbox[s.class] {
                    None => (x, y, x, y),
                    Some(r) => (r.0.min(x), r.1.min(y), r.2.max(x), r.3.max(y)),
                });
            }
        }
    }
    let mut neighboured = vec![0.0f32; CLASSES.len()];
    let mut missing_count = vec![0.0f32; CLASSES.len()];
    for y in 1..H - 1 {
        for x in 1..W - 1 {
            let i = (y * W + x) as usize;
            let c = missing_mask[i];
            if c == usize::MAX {
                continue;
            }
            missing_count[c] += 1.0;
            let n = missing_mask[i - 1] != usize::MAX
                || missing_mask[i + 1] != usize::MAX
                || missing_mask[i - W as usize] != usize::MAX
                || missing_mask[i + W as usize] != usize::MAX;
            if n {
                neighboured[c] += 1.0;
            }
        }
    }
    CLASSES
        .iter()
        .enumerate()
        .map(|(i, name)| ClassReport {
            name,
            expected: expected[i],
            painted: painted[i],
            missing: missing[i],
            contiguity: if missing_count[i] > 0.0 { neighboured[i] / missing_count[i] } else { 0.0 },
            bbox: bbox[i],
        })
        .collect()
}

fn configs() -> Vec<(&'static str, f32, f32)> {
    vec![
        ("along the wall, level", 0.0, 0.0),
        ("along the wall, level, yawed toward it", 0.0, -6.0),
        ("along the wall, slightly down", 3.0, 0.0),
        ("along the wall, slightly up", -2.0, 0.0),
        ("across the floaters", 0.0, -12.0),
        ("across the floaters, other side", 0.0, 12.0),
    ]
}

fn sweep(block_name: &str) -> Vec<(String, ClassReport)> {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let block = first_state_named(block_name);
    let air = first_state_named("minecraft:air");
    let world = slab_and_wall_world(block, air);

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let mut out = Vec::new();
    for (label, pitch, yaw) in configs() {
        let camera = camera_at(pitch, yaw);
        let (reference, _) = render_frame(
            device, queue, format, &mut target, &atlas, &world, models, &camera, false, None,
        );
        let (subject, stats) = render_frame(
            device, queue, format, &mut target, &atlas, &world, models, &camera, true, None,
        );
        let samples = oracle_samples(&world, air, &camera);
        eprintln!("=== {block_name} / {label} (pitch={pitch}, yaw={yaw}) === sections drawn = {}", stats.sections_drawn);
        for report in analyse(&subject, &reference, &samples) {
            eprintln!(
                "  {:<48} oracle area {:>9.1}  painted {:>9.1}  ratio {:.4}  missing {:>8.1}  contiguity {:.3}  bbox {:?}",
                report.name,
                report.expected,
                report.painted,
                report.painted / report.expected.max(1.0),
                report.missing,
                report.contiguity,
                report.bbox,
            );
            out.push((format!("{block_name} / {label} / {}", report.name), report));
        }
    }
    out
}

/// The gate: **how much** of each near, eye-level, grazing surface actually
/// paints, against an independent ray-cast oracle's own sub-pixel coverage for
/// the same camera.
///
/// This is deliberately a magnitude assertion rather than a presence one. The
/// report is *"sometimes only 60% of the block"*, and a gate that only asked
/// "did anything paint here" would pass at 60% exactly as it passes at 100%.
/// The tolerance below is not fitted: painted area is counted per whole pixel
/// while oracle area is sub-pixel, so a surface's own silhouette contributes a
/// bounded few percent in either direction, and the hypothesis under test is
/// three to eight times larger than that.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn near_grazing_faces_paint_the_area_the_oracle_says_they_cover() {
    const MIN_AREA: f32 = 400.0;
    const TOLERANCE: f32 = 0.05;
    let mut failures = Vec::new();
    let mut measured = 0usize;
    for block in ["minecraft:stone", "minecraft:grass_block"] {
        for (label, report) in sweep(block) {
            if report.expected < MIN_AREA {
                continue;
            }
            measured += 1;
            let ratio = report.painted / report.expected;
            if !(1.0 - TOLERANCE..=1.0 + TOLERANCE).contains(&ratio) {
                failures.push((label, ratio, report.expected, report.missing, report.contiguity, report.bbox));
            }
        }
    }
    assert!(
        measured >= 24,
        "only {measured} surfaces carried enough oracle area to measure — the fixture is not \
         showing the geometry this gate exists to measure"
    );
    assert!(
        failures.is_empty(),
        "near, eye-level, grazing surfaces did not paint the area an independent ray-cast oracle \
         says they cover. (surface, painted/oracle, oracle area, missing, contiguity, bbox): \
         {failures:?}"
    );
}

/// Control: the detector must fire on a hole that is really there, and it
/// must say so with the *shape* statistic too. One section carrying the wall
/// and the near floor is never uploaded; the missing set must be large and
/// contiguous, which is what calibrates a low contiguity elsewhere as
/// something other than a lost region.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_deliberately_missing_section_is_detected() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let stone = first_state_named("minecraft:stone");
    let air = first_state_named("minecraft:air");
    let world = slab_and_wall_world(stone, air);

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera_at(0.0, 0.0);
    // The section holding the wall and the floor surface one chunk ahead.
    let victim = SectionKey { cx: 0, cz: 1, si: (FLOOR_TOP / 16) as usize, min_y: MIN_Y };

    let (reference, _) = render_frame(
        device, queue, format, &mut target, &atlas, &world, models, &camera, false, None,
    );
    let (subject, _) = render_frame(
        device, queue, format, &mut target, &atlas, &world, models, &camera, true, Some(victim),
    );
    let samples = oracle_samples(&world, air, &camera);
    let reports = analyse(&subject, &reference, &samples);
    let mut total_missing = 0.0f32;
    let mut worst_ratio = 1.0f32;
    let mut best_contiguity = 0.0f32;
    for report in &reports {
        eprintln!(
            "  control {:<40} oracle area {:>9.1}  painted {:>9.1}  ratio {:.4}  missing {:>8.1}  contiguity {:.3}  bbox {:?}",
            report.name,
            report.expected,
            report.painted,
            report.painted / report.expected.max(1.0),
            report.missing,
            report.contiguity,
            report.bbox,
        );
        total_missing += report.missing;
        if report.expected > 400.0 {
            worst_ratio = worst_ratio.min(report.painted / report.expected);
        }
        best_contiguity = best_contiguity.max(report.contiguity);
    }
    assert!(
        total_missing > 200.0 && worst_ratio < 0.95,
        "the control did not fire: a whole section of wall and floor was never uploaded and the \
         detector still reported {total_missing:.1} missing area at a worst ratio of \
         {worst_ratio:.4}. Nothing above it can be believed."
    );
    assert!(
        best_contiguity > 0.9,
        "the control fired but reported contiguity {best_contiguity:.3} for a lost *region*, \
         which would leave the shape statistic uncalibrated"
    );
}
