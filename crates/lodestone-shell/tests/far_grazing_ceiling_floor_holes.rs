//! Reproduction for the owner's report: "for blocks far away and at weird
//! angles, it stops rendering them and instead just shows the sky... for
//! blocks really high up (like a ceiling), floors, etc." — new information the
//! three prior passes did not have, because none of them tested a literal
//! **ceiling** (a horizontal surface *above* the camera) or pushed distance
//! and grazing angle anywhere near as far as this file does.
//!
//! # What the prior passes established, and what they left untested
//!
//! `distant_flat_terrain_holes.rs` (a flat floor, 160-block render distance,
//! shallow pitches) found nothing. `uneven_terrain_holes.rs` (a stepped
//! ziggurat, same 160-block distance) found 215 raw sandwiched-sky pixels, of
//! which an independent ray-cast oracle (checking both "does the ray
//! genuinely hit a block" and "is that hit within the renderer's own
//! configured view distance") confirmed 213 were legal — corner grazing or
//! genuinely beyond render distance — leaving 2 residual pixels. Neither file
//! tested a **ceiling**, and neither pushed render distance past 10 chunks or
//! pitch past 25°/-15°.
//!
//! # The mistake this file made on its first pass, and why the oracle is not optional
//!
//! A first version of this file reused the *naive* "any background pixel
//! after a terrain pixel in the same column is a bug" detector from
//! `distant_flat_terrain_holes.rs`, on the reasoning that a flat plane (floor
//! or ceiling alone) still satisfies that detector's monotonic-single-crossing
//! proof. At a genuinely large render distance (24 chunks) and near-grazing
//! pitch it reported ~37,000 "hole" pixels spanning the full screen width —
//! which looked like exactly the severe, report-shaped defect this file was
//! built to find.
//!
//! It was a false positive, caught by hand-deriving the same ray-plane
//! intersection independently in Python: this world has **two** surfaces
//! (floor below eye level, ceiling above), and near the horizon a ray sweeps
//! from hitting the (in-range) ceiling, to the (now out-of-range) ceiling, to
//! the (still out-of-range) floor, to the (finally in-range) floor, all within
//! a wide, contiguous band of screen rows — because at a shallow pitch, a huge
//! span of rows all correspond to ray directions nearly parallel to both
//! planes, and the true first-hit distance for that whole span exceeds the
//! configured render distance. The naive detector's proof only covers a
//! *single* monotonic heightfield; it says nothing about two independent
//! planes racing to the horizon on either side of the eye, and every "hole" in
//! that first run was this — legitimate, correct render-distance culling, not
//! missing geometry.
//!
//! The generalisable shape, which this repo has now paid for three times: **a
//! screen-space invariant asserted over a whole frame is usually a truth about
//! one privileged configuration**, and the fixture that breaks it is the one
//! that adds a second instance of the thing the derivation assumed there was
//! one of. `uneven_terrain_holes.rs` found its version off-centre (the
//! derivation held only at the image's centre column); this file found a second
//! at a second *surface*. Before trusting a sandwiched-sky detector, ask how
//! many independent surfaces the scene has — the proof covers one.
//!
//! This is exactly the lesson `uneven_terrain_holes.rs` already recorded (its
//! own naive version was wrong off-centre; this one was wrong for a second,
//! different reason) and exactly why its `classify_holes` oracle exists:
//! **every flagged pixel gets its own independent ray-cast against the real
//! block data**, and a hit beyond `within_view_distance` is exculpated exactly
//! like a hit that misses entirely. This file now reuses that oracle verbatim
//! rather than re-deriving "is this a legitimate case" by hand a second time.
//!
//! # The verdict, and the second false positive that had to be removed to get it
//!
//! With fog neutralised (see [`unfogged_settings`], which carries the
//! measurement) this gate is **green**: `oracle-genuine = 0` for all six
//! configs, against 19,017-34,704 raw sandwiched-sky pixels per config that the
//! oracle exculpates as genuinely beyond render distance. Nothing in
//! `lodestone_render`'s distance cull, frustum classify or visibility walk drops
//! in-range geometry at this scene shape, at 24 chunks, at pitches down to
//! 1 degree off horizontal, looking at either a floor or a ceiling.
//!
//! Two distinct false positives had to be cleared before that number meant
//! anything — the two-plane horizon effect below, and fog. They are the same
//! species: **the scene contains a second, legitimate way for a pixel to be
//! sky, and the detector cannot see it.**
//!
//! # What this fixture structurally cannot test
//!
//! Say this plainly, because a green result here is narrower than it looks. The
//! air between the two slabs is **all-air sections**, and `walk_visible_bounded`
//! treats a section absent from the graph as `SectionVisibility::all()` — fully
//! open. So the reachability walk spreads through this world under the most
//! permissive connectivity it can ever have. A real ceiling is a cave roof or a
//! building, where connectivity is *partial* and the walk's "can I pass from the
//! face I entered to the face I am leaving" gate actually bites. **This gate
//! cannot fail for an over-culling BFS**, which is the mechanism most likely to
//! strand a real-world ceiling, and a fixture whose corpus is one fully-open
//! scene is blind to it by construction.
//!
//! ```text
//! cargo test -p lodestone-shell --test far_grazing_ceiling_floor_holes -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR, ThirdPersonBodyState};
use lodestone::mesher::{SectionGeometry, SectionKey, mesh_snapshot_models, snapshot_section, snapshot_visibility};
use lodestone::resources::BlockResources;
use lodestone_render::{
    Camera, GpuContext, HeadlessTarget, ModelMesh, RenderTarget, cull::within_view_distance,
    entity_anim::AnimInput, fog::FogSettings,
};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

/// See `distant_flat_terrain_holes.rs`'s identical helper's doc: `RenderState`
/// draws an unconditional first-person bare arm at a fixed screen rect
/// whenever no third-person body is reported, which reads as a hole and is
/// not one.
fn suppress_first_person_arm(state: &mut RenderState) {
    state.set_third_person_body_source(|| {
        Some(ThirdPersonBodyState {
            feet: glam::Vec3::new(0.0, -10_000.0, 0.0),
            body_yaw_deg: 0.0,
            anim: AnimInput::default(),
            scale: 1.0,
            swim_amount: 0.0,
            slim: false,
            equipment: Vec::new(),
        })
    });
}

const W: u32 = 640;
const H: u32 = 480;
const FOV_Y_DEGREES: f32 = 40.0;

/// Genuinely far: 24 chunks (384 blocks), well past the 10-chunk (160-block)
/// distance both prior gates used.
const RD_CHUNKS: i32 = 24;
const MIN_Y: i32 = 0;
/// Floor fills `[0, FLOOR_TOP)`; ceiling fills `[CEIL_BOTTOM, CEIL_TOP)`; air
/// in between is where the camera stands and looks. Both interfaces sit on
/// section boundaries, matching both prior gates' own choice.
const FLOOR_TOP: i32 = 64;
const CEIL_BOTTOM: i32 = 96;
const CEIL_TOP: i32 = 112;
const SECTION_COUNT: usize = (CEIL_TOP / 16) as usize;

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

#[derive(Debug, Clone, Copy)]
struct Rect {
    x0: u32,
    y0: u32,
    x1: u32,
    y1: u32,
}

fn grow(rect: Option<Rect>, x: u32, y: u32) -> Rect {
    match rect {
        None => Rect { x0: x, y0: y, x1: x, y1: y },
        Some(r) => Rect {
            x0: r.x0.min(x),
            y0: r.y0.min(y),
            x1: r.x1.max(x),
            y1: r.y1.max(y),
        },
    }
}

/// Identical logic to both prior gates' function of the same name — also
/// returns every flagged pixel's own coordinates, which the oracle below
/// needs to interrogate each one individually (see `uneven_terrain_holes.rs`).
fn find_sandwiched_background(
    subject: &[u8],
    reference: &[u8],
    w: u32,
    h: u32,
) -> (usize, Option<Rect>, usize, Vec<(u32, u32)>) {
    let mut total = 0usize;
    let mut rect: Option<Rect> = None;
    let mut hole_columns = 0usize;
    let mut pixels = Vec::new();
    for x in 0..w {
        let mut seen_terrain = false;
        let mut column_has_hole = false;
        for y in 0..h {
            let idx = ((y * w + x) * 4) as usize;
            let sub = &subject[idx..idx + 4];
            let ref_px = &reference[idx..idx + 4];
            if differs(sub, ref_px) {
                seen_terrain = true;
            } else if seen_terrain {
                total += 1;
                column_has_hole = true;
                rect = Some(grow(rect, x, y));
                pixels.push((x, y));
            }
        }
        if column_has_hole {
            hole_columns += 1;
        }
    }
    (total, rect, hole_columns, pixels)
}

/// Independent ray-cast oracle, identical in mechanism to
/// `uneven_terrain_holes.rs`'s function of the same name: the ray direction is
/// transcribed (not called) from `Camera::basis`'s documented closed form, and
/// solidity comes straight from `World::block_state_at` — no mesher, no
/// rasteriser, no shader, no renderer code at all.
fn oracle_ray_dir(camera: &Camera, px: u32, py: u32, w: u32, h: u32) -> glam::Vec3 {
    let (sy, cy) = camera.yaw.to_radians().sin_cos();
    let (sp, cp) = camera.pitch.to_radians().sin_cos();
    let right = glam::Vec3::new(-cy, 0.0, -sy);
    let up = glam::Vec3::new(-sy * sp, cp, cy * sp);
    let forward = glam::Vec3::new(-sy * cp, -sp, cy * cp);
    let half_y = (camera.fov_y_degrees.to_radians() * 0.5).tan();
    let half_x = half_y * camera.aspect;
    let ndc_x = 2.0 * (px as f32 + 0.5) / w as f32 - 1.0;
    let ndc_y = 1.0 - 2.0 * (py as f32 + 0.5) / h as f32;
    (forward + right * (ndc_x * half_x) + up * (ndc_y * half_y)).normalize()
}

/// Marches a ray through `world`'s real block data at a fixed small step.
///
/// This is the oracle's *reference* implementation — simple enough to read as
/// obviously correct, and far too slow to run the main sweep with (see
/// [`oracle_first_solid_cell`]). It survives only as the thing
/// `dda_and_stepper_agree` pins the fast walk against, so the swap to a voxel
/// walk is evidenced rather than assumed.
///
/// A `None` read (chunk not loaded, or `y` outside the column) is treated as
/// air. Returns the first solid-hit world point, or `None` if the ray reaches
/// `max_dist` clean.
fn oracle_ray_hits_solid_stepped(
    world: &World,
    air: u32,
    origin: glam::Vec3,
    dir: glam::Vec3,
    max_dist: f32,
) -> Option<(f32, glam::Vec3)> {
    const STEP: f32 = 0.02;
    let mut t = 0.0f32;
    while t < max_dist {
        let p = origin + dir * t;
        let (bx, by, bz) = (p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32);
        if let Some(state) = world.block_state_at(bx, by, bz) {
            if state != air {
                return Some((t, p));
            }
        }
        t += STEP;
    }
    None
}

/// The only distance the oracle has to march, and the reason this gate can
/// reach a verdict at all.
///
/// `within_view_distance` shrinks each chunk-axis delta by one before testing a
/// Euclidean disc of radius `RD_CHUNKS`, so no in-range chunk sits further than
/// `RD_CHUNKS + 1` chunks away on either horizontal axis, and the furthest
/// *point* of such a chunk is `(RD_CHUNKS + 2) * 16` blocks out per axis.
/// Beyond that bound a hit cannot be in range — and `classify_holes` already
/// treats an out-of-range hit and a clean miss **identically**, as legitimate.
/// So stopping the march here produces exactly the classification a march to
/// `camera.far` produces, with none of the work.
///
/// The work matters more than it looks: `Camera::far_for_render_distance` is
/// `4x` the render distance in blocks — 1536 here — so a march to the far plane
/// spends roughly two thirds of every ray in a region where the answer is
/// already decided no matter what it finds.
fn max_in_range_distance() -> f32 {
    let horizontal = ((RD_CHUNKS + 2) * 16) as f32;
    (2.0 * horizontal * horizontal).sqrt()
}

/// Marches a ray through `world`'s real block data, visiting **exactly** the
/// voxels the segment passes through (the standard Amanatides-Woo walk), and
/// returns the first solid one as `(block coordinate, entry point, entry
/// distance)`. A `None` read is air, exactly as in the stepped reference above.
///
/// This exists because the fixed-step sampler is what denied this gate a
/// verdict on its first attempt, and the arithmetic is worth keeping written
/// down: at `STEP = 0.02` over a 1536-block far plane, one pixel costs 76,800
/// world reads, and at a grazing pitch almost every ray runs the *whole* length
/// before it hits anything — the floor is 16 blocks below the eye, so at 1 deg
/// of pitch its true first hit is ~917 blocks out. Times the ~37,000 pixels a
/// grazing config flags, that is billions of reads per config, six configs
/// deep. The control passed only because its 30-deg-down rays hit within a few
/// blocks and never paid the grazing cost — which is the tell worth
/// remembering: **a control can be cheap for exactly the reason the subject is
/// expensive**, so a control that returns promptly is no evidence the sweep it
/// guards will return at all.
///
/// A voxel walk is also strictly *more* faithful than any fixed step, which can
/// only ever approximate the moment a ray enters a cell.
fn oracle_first_solid_cell(
    world: &World,
    air: u32,
    origin: glam::Vec3,
    dir: glam::Vec3,
    max_dist: f32,
) -> Option<([i32; 3], glam::Vec3, f32)> {
    let o = [origin.x, origin.y, origin.z];
    let d = [dir.x, dir.y, dir.z];
    let mut cell = [o[0].floor() as i32, o[1].floor() as i32, o[2].floor() as i32];
    let mut step = [0i32; 3];
    // A zero direction component never crosses a boundary on that axis, so its
    // "next crossing" is infinitely far — which is what keeps it out of the
    // per-iteration minimum below without a special case.
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
                return Some((cell, origin + dir * t, t));
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

/// Classifies every flagged pixel by independently ray-casting it: a pixel the
/// oracle also calls sky (no hit, or a hit beyond the renderer's own
/// configured view distance) is legitimate; a pixel where the oracle says the
/// ray should have hit an *in-range* block is a genuine renderer bug. Returns
/// `(legitimate, genuine_bugs)`, the second as `(x, y, world point the oracle
/// hit)`.
fn classify_holes(
    world: &World,
    air: u32,
    camera: &Camera,
    render_distance_chunks: u32,
    hole_pixels: &[(u32, u32)],
    w: u32,
    h: u32,
) -> (usize, Vec<(u32, u32, glam::Vec3)>) {
    let camera_chunk = (
        (camera.position.x / 16.0).floor() as i32,
        (camera.position.z / 16.0).floor() as i32,
    );
    // See `max_in_range_distance`: past this bound every outcome — hit or
    // miss — is already classified legitimate, so marching further cannot
    // change a single verdict.
    let march_limit = camera.far.min(max_in_range_distance());
    let mut legitimate = 0usize;
    let mut genuine_bugs = Vec::new();
    for &(x, y) in hole_pixels {
        let dir = oracle_ray_dir(camera, x, y, w, h);
        match oracle_first_solid_cell(world, air, camera.position, dir, march_limit) {
            // Deriving the chunk from the integer cell rather than from the
            // floating entry point is deliberate: the entry point sits exactly
            // on a voxel boundary, where a `floor` can land on either side.
            Some((cell, entry, _)) => {
                let hit_chunk = (cell[0] >> 4, cell[2] >> 4);
                if within_view_distance(camera_chunk, hit_chunk, render_distance_chunks) {
                    genuine_bugs.push((x, y, entry));
                } else {
                    legitimate += 1;
                }
            }
            None => legitimate += 1,
        }
    }
    (legitimate, genuine_bugs)
}

/// A floor-and-ceiling world: solid `[0, FLOOR_TOP)`, air, solid
/// `[CEIL_BOTTOM, CEIL_TOP)`. See `distant_flat_terrain_holes.rs`'s identical
/// helper's doc for why `LightData::Uniform` is required (a `Missing` default
/// resolves to full dark and makes subject and reference byte-identical).
fn slab_world(stone: u32, air: u32) -> World {
    slab_world_radius(stone, air, RD_CHUNKS)
}

/// [`slab_world`] at an arbitrary radius, so the oracle's own differential
/// gate can build a cheap world instead of the sweep's 49x49-column one.
fn slab_world_radius(stone: u32, air: u32, radius: i32) -> World {
    let mut world = World::new();
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
    let floor = world.fill_region([lo, MIN_Y, lo], [hi, FLOOR_TOP - 1, hi], stone);
    assert!(floor > 0, "fixture: floor must actually write blocks");
    let ceil = world.fill_region([lo, CEIL_BOTTOM, lo], [hi, CEIL_TOP - 1, hi], stone);
    assert!(ceil > 0, "fixture: ceiling must actually write blocks");
    world
}

#[allow(clippy::too_many_arguments)]
fn upload_all(
    world: &World,
    models: &lodestone_render::BlockModels,
    state: &mut RenderState,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    upload_terrain: bool,
) -> usize {
    let mut uploaded = 0usize;
    if !upload_terrain {
        return uploaded;
    }
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            for si in 0..SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: MIN_Y };
                let Some(snap) = snapshot_section(world, key) else {
                    continue;
                };
                let opaque = mesh_snapshot_models(&snap, models, false);
                let visibility = snapshot_visibility(&snap, models);
                let geometry = SectionGeometry::Model {
                    opaque,
                    water: ModelMesh::default(),
                    translucent_blocks: ModelMesh::default(),
                    visibility,
                };
                state.upload_section(device, queue, key, &geometry);
                uploaded += 1;
            }
        }
    }
    uploaded
}

/// The fog this gate renders with: vanilla's colours and sky disc, but with
/// **both** fog ramps pushed past the far plane so nothing in render distance
/// fades at all.
///
/// # Why the gate cannot run with real fog, measured
///
/// This is not a convenience. With production fog
/// (`FogSettings::for_render_distance(SKY_COLOR, 24)` — a ramp from 345.6 to
/// 384 blocks, plus the 0..1024 environmental term), the first oracle-filtered
/// sweep reported 2,634 / 2,682 / 0 / 2,614 / 2,596 / 1,469 "genuine" pixels
/// across the six configs. Every one of those 11,995 pixels was fog:
///
/// | quantity, over all 11,995 | value |
/// |---|---|
/// | minimum hit distance | 376.1 blocks |
/// | minimum fog factor | 0.795 |
/// | pixels with fog < 0.5 | **0** |
/// | pixels at fog >= 0.999 (exactly the fog colour) | 9,473 |
///
/// Not one flagged pixel lay nearer than 376 blocks of a 384-block fade end.
/// The renderer was drawing all of it; fog was fading it to `SKY_COLOR`, which
/// is exactly what vanilla does and exactly what the sky-coloured reference
/// frame contains.
///
/// The confound is structural rather than a near miss, which is why the fix is
/// to remove fog rather than to widen an exculpation: `within_view_distance`
/// is a test on the **chunk grid** and it shrinks each axis delta by one before
/// testing a disc, so a chunk it calls in-range reaches **419 blocks**
/// Euclidean — 35 blocks past the point where fog is pinned at 1.0. There is
/// therefore a permanent annulus in which the oracle says "resident geometry"
/// and the renderer correctly says "fog colour", and no threshold placed inside
/// it would be anything but fitted to the answer.
///
/// The swap was confirmed rather than assumed, and the confirmation is exact.
/// Re-running the identical sweep with only these settings changed moved every
/// raw count down by **precisely** the number of pixels the oracle had called
/// genuine, in all five non-zero configs:
///
/// | config | genuine, fogged | raw holes fogged -> unfogged | delta |
/// |---|---|---|---|
/// | floor 1 deg | 2634 | 37128 -> 34494 | 2634 |
/// | floor 5 deg | 2682 | 37386 -> 34704 | 2682 |
/// | ceiling -1 deg | 2614 | 37108 -> 34494 | 2614 |
/// | ceiling -5 deg | 2596 | 37300 -> 34704 | 2596 |
/// | ceiling -20 deg | 1469 | 20486 -> 19017 | 1469 |
///
/// Every pixel the fogged run accused became terrain, and none of the
/// out-of-range remainder moved. A hypothesis that predicts five counts to the
/// pixel is not a threshold that happened to fit.
///
/// Neutralising fog is the assertion this gate actually wants: it exists to
/// find geometry a **cull** dropped, and a fixture that also fades the far rim
/// to the background cannot tell the two apart. Both the subject and the
/// reference frame use these settings, so the sky disc is identical in each and
/// `differs` still isolates terrain and nothing else.
fn unfogged_settings(color: [f32; 3]) -> FogSettings {
    let mut fog = FogSettings::for_render_distance(color, RD_CHUNKS as u32);
    // Past the far plane, so the ramps are inert for every fragment that can
    // possibly be drawn rather than merely distant.
    let beyond = Camera::far_for_render_distance(RD_CHUNKS as u32, 0) * 4.0;
    fog.start = beyond;
    fog.end = beyond * 2.0;
    fog.environmental_start = beyond;
    fog.environmental_end = beyond * 2.0;
    fog
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
) -> (Vec<u8>, lodestone::gpu::RenderStats) {
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas));
    suppress_first_person_arm(&mut state);
    // Two separate requirements in one call. The render distance must match
    // this fixture's own `RD_CHUNKS` — `RenderState` defaults it to 8, and a
    // mismatch makes the view-distance cull disagree with what the world and
    // the far clip actually cover. The settings must be `unfogged_settings`;
    // see its doc for the measurement showing production fog otherwise
    // supplies every "hole" this gate finds.
    state.set_fog(unfogged_settings(SKY_COLOR), RD_CHUNKS as u32);
    let uploaded = upload_all(world, models, &mut state, device, queue, upload_terrain);
    assert!(
        !upload_terrain || uploaded > 0,
        "fixture: some sections must have uploaded when terrain is requested"
    );
    state.update_animation(queue, FADE_COMPLETE_TICK);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), camera, None, &[]);
    (target.read_texels(device, queue), stats)
}

fn camera_at(eye_y: f32, pitch_degrees: f32, yaw_degrees: f32) -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, eye_y, 0.5),
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

/// Sweeps distance and grazing angle far past what the two prior gates
/// covered, on a flat floor+ceiling world. Every raw sandwiched-sky pixel is
/// re-checked by the independent ray-cast oracle (see the module doc for why
/// the naive version of this gate is not trustworthy on this world) — only a
/// pixel whose own ray hits real, *in-range* geometry counts as a bug.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn far_grazing_views_of_flat_floor_and_ceiling() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let stone = first_state_named("minecraft:stone");
    let air = first_state_named("minecraft:air");
    let world = slab_world(stone, air);

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    // Eye in open air between floor and ceiling.
    let eye_y = (FLOOR_TOP as f32 + CEIL_BOTTOM as f32) / 2.0;

    let configs: &[(&str, f32, f32)] = &[
        ("floor, very shallow (1 deg)", 1.0, 0.0),
        ("floor, shallow (5 deg)", 5.0, 0.0),
        ("floor, moderate (20 deg)", 20.0, 0.0),
        ("ceiling, very shallow (-1 deg)", -1.0, 0.0),
        ("ceiling, shallow (-5 deg)", -5.0, 0.0),
        ("ceiling, moderate (-20 deg)", -20.0, 0.0),
    ];

    let mut all_genuine_bugs: Vec<(&str, u32, u32, glam::Vec3)> = Vec::new();
    for &(label, pitch, yaw) in configs {
        let camera = camera_at(eye_y, pitch, yaw);
        let (reference, _) =
            render_frame(device, queue, format, &mut target, &atlas, &world, models, &camera, false);
        let (subject, stats) =
            render_frame(device, queue, format, &mut target, &atlas, &world, models, &camera, true);

        let (hole_px, bbox, hole_cols, hole_pixels) = find_sandwiched_background(&subject, &reference, W, H);
        let (legitimate, genuine_bugs) =
            classify_holes(&world, air, &camera, RD_CHUNKS as u32, &hole_pixels, W, H);
        eprintln!(
            "=== {label} (pitch={pitch}, yaw={yaw}, RD={RD_CHUNKS} chunks) ===\n\
             sections drawn        = {}\n\
             sections culled dist  = {}\n\
             sections culled frust = {}\n\
             sections culled occl  = {} (occlusion_active={})\n\
             occlusion graph size  = {}\n\
             sandwiched-sky pixels = {hole_px} (raw, pre-oracle)\n\
             sandwiched-sky columns= {hole_cols} / {W}\n\
             bounding box          = {bbox:?}\n\
             oracle-legitimate     = {legitimate} (beyond render distance, or ray never hits)\n\
             oracle-genuine bugs   = {} (ray hits real, in-range geometry)",
            stats.sections_drawn,
            stats.sections_culled_distance,
            stats.sections_culled_frustum,
            stats.sections_culled_occlusion,
            stats.occlusion_active,
            stats.occlusion_graph_sections,
            genuine_bugs.len(),
        );
        for (x, y, hit) in genuine_bugs {
            all_genuine_bugs.push((label, x, y, hit));
        }
    }

    assert!(
        all_genuine_bugs.is_empty(),
        "the far, grazing floor/ceiling world produced sky pixels whose own independent \
         ray-cast says the ray should have hit real, in-render-distance geometry instead. \
         Mismatches (config, x, y, world point the oracle hit): {all_genuine_bugs:?}"
    );
}

/// Control: does the detector (raw + oracle) actually fire on a real, known
/// hole in this far/grazing world? Skips uploading one nearby column's floor
/// surface section and asserts both that the raw detector finds it and that
/// the oracle correctly classifies every one of those pixels as a genuine bug
/// (the world still holds real stone there — only the GPU upload was
/// skipped), never as legitimate out-of-range sky.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_deliberately_missing_floor_section_is_detected() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let stone = first_state_named("minecraft:stone");
    let air = first_state_named("minecraft:air");
    let world = slab_world(stone, air);

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let eye_y = (FLOOR_TOP as f32 + CEIL_BOTTOM as f32) / 2.0;

    // Close, steep-down view of the floor directly ahead, matching both prior
    // gates' own lesson: a victim section at moderate-to-far distance can
    // project to under one screen row and be undetectable even when
    // genuinely missing.
    let camera = camera_at(eye_y, 30.0, 0.0);
    let victim = SectionKey { cx: 0, cz: 1, si: (FLOOR_TOP / 16 - 1) as usize, min_y: MIN_Y };

    let (reference, _) =
        render_frame(device, queue, format, &mut target, &atlas, &world, models, &camera, false);

    // Upload everything except the victim.
    let mut state = RenderState::new(device, queue, format, W, H, Some(&atlas));
    suppress_first_person_arm(&mut state);
    state.set_fog(unfogged_settings(SKY_COLOR), RD_CHUNKS as u32);
    let mut uploaded = 0usize;
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            for si in 0..SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: MIN_Y };
                if key == victim {
                    continue;
                }
                let Some(snap) = snapshot_section(&world, key) else { continue };
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
    assert!(uploaded > 0, "fixture: some sections must upload");
    state.update_animation(queue, FADE_COMPLETE_TICK);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
    let subject = target.read_texels(device, queue);

    let (hole_px, bbox, hole_cols, hole_pixels) = find_sandwiched_background(&subject, &reference, W, H);
    let (legitimate, genuine_bugs) =
        classify_holes(&world, air, &camera, RD_CHUNKS as u32, &hole_pixels, W, H);
    eprintln!(
        "=== control: floor section {victim:?} never uploaded ===\n\
         sections drawn         = {}\n\
         sandwiched-sky pixels  = {hole_px}\n\
         sandwiched-sky columns = {hole_cols} / {W}\n\
         bounding box           = {bbox:?}\n\
         oracle-legitimate      = {legitimate} (expect 0)\n\
         oracle-genuine bugs    = {} (expect {hole_px})",
        stats.sections_drawn,
        genuine_bugs.len(),
    );
    assert!(
        hole_px > 0,
        "control failed to fire: a floor section was deliberately never uploaded and the \
         sandwiched-sky detector found nothing — fix this before trusting a clean result \
         from the main gate."
    );
    assert_eq!(
        genuine_bugs.len(),
        hole_px,
        "control's own oracle check failed: {legitimate} of {hole_px} deliberately-missing- \
         section pixels were classified as legitimate out-of-range sky instead of a genuine \
         bug — an oracle that can be fooled by this would also launder a real missing-upload \
         bug into a false pass on the main gate."
    );
}


/// Pins the fast voxel walk the sweep actually uses against the slow fixed-step
/// sampler it replaced, because swapping an oracle's implementation is exactly
/// the kind of change that can launder a false pass into the gate it feeds.
///
/// Needs no GPU and no vanilla assets — only `World` — so unlike the two gates
/// above it runs in an ordinary `cargo test` and cannot rot behind `--ignored`.
///
/// The world is deliberately small (a 3-chunk radius, not the sweep's 24) so
/// the reference sampler's 0.02 step is affordable; the rays are what carry the
/// discrimination, not the world size. They include a pitch of exactly 0 and a
/// yaw of exactly 0 — the cases that put a hard zero in a direction component
/// and drive the walk's infinite-`t_max` path, which no mid-range angle
/// reaches. (`Mth`-style pole behaviour is not in play here: these directions
/// come from the same transcribed `Camera::basis` closed form both oracles use,
/// so any error in it cancels and this gate is only comparing the two marches.)
#[test]
fn dda_and_stepper_agree() {
    let stone = first_state_named("minecraft:stone");
    let air = first_state_named("minecraft:air");
    let world = slab_world_radius(stone, air, 3);
    let eye_y = (FLOOR_TOP as f32 + CEIL_BOTTOM as f32) / 2.0;
    let max_dist = 48.0f32;

    let mut hits = 0usize;
    let mut misses = 0usize;
    // Collect, never assert inside the loop: an `assert!` in here would abort
    // on the first mismatch and turn every later ray from an observation back
    // into an argument.
    let mut mismatches: Vec<String> = Vec::new();

    for &pitch in &[0.0f32, 1.0, -1.0, 5.0, -5.0, 20.0, -20.0, 45.0, -45.0, 89.0, -89.0] {
        for &yaw in &[0.0f32, 37.0, 90.0, 180.0, -143.0] {
            let camera = camera_at(eye_y, pitch, yaw);
            // A coarse stride over the frame: the point is angular coverage,
            // not every pixel, and the reference sampler is slow.
            for py in (0..H).step_by(37) {
                for px in (0..W).step_by(53) {
                    let dir = oracle_ray_dir(&camera, px, py, W, H);
                    let fast = oracle_first_solid_cell(&world, air, camera.position, dir, max_dist);
                    let slow =
                        oracle_ray_hits_solid_stepped(&world, air, camera.position, dir, max_dist);
                    match (fast, slow) {
                        (None, None) => misses += 1,
                        (Some((cell, _, t_fast)), Some((t_slow, p_slow))) => {
                            hits += 1;
                            let slow_cell = [
                                p_slow.x.floor() as i32,
                                p_slow.y.floor() as i32,
                                p_slow.z.floor() as i32,
                            ];
                            // The walk reports the exact entry; the sampler
                            // reports the first 0.02 tick at or after it, so
                            // the walk may lead by up to one step but must
                            // never lag, and must never overshoot a cell.
                            if !(-0.001..=0.021).contains(&(t_slow - t_fast)) {
                                mismatches.push(format!(
                                    "pitch={pitch} yaw={yaw} px=({px},{py}): \
                                     entry distance disagrees, walk={t_fast} sampler={t_slow}"
                                ));
                            }
                            if (cell[0] >> 4, cell[2] >> 4) != (slow_cell[0] >> 4, slow_cell[2] >> 4)
                            {
                                mismatches.push(format!(
                                    "pitch={pitch} yaw={yaw} px=({px},{py}): \
                                     hit chunk disagrees, walk={:?} sampler={:?}",
                                    (cell[0] >> 4, cell[2] >> 4),
                                    (slow_cell[0] >> 4, slow_cell[2] >> 4),
                                ));
                            }
                        }
                        (f, sl) => mismatches.push(format!(
                            "pitch={pitch} yaw={yaw} px=({px},{py}): one oracle hit and the \
                             other missed, walk={f:?} sampler={sl:?}"
                        )),
                    }
                }
            }
        }
    }

    // Without these two, a pair of oracles that both returned `None` for every
    // ray would agree perfectly and prove nothing.
    assert!(hits > 100, "differential is vacuous: only {hits} rays hit anything at all");
    assert!(
        misses > 0,
        "differential never exercised the miss path: all {hits} rays hit something, so \
         agreement on `None` is untested"
    );
    assert!(
        mismatches.is_empty(),
        "the voxel walk and the fixed-step reference sampler disagree on {} of {} rays \
         ({hits} hits, {misses} misses): {mismatches:#?}",
        mismatches.len(),
        hits + misses,
    );
}
