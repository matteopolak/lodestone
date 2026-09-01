//! **Does the section-visibility walk over-cull when connectivity is
//! *partial*?** — the arm `far_grazing_ceiling_floor_holes.rs` says, in its own
//! module doc, that it structurally cannot test.
//!
//! # The gap this file exists to close
//!
//! That file's verdict (oracle-genuine holes = 0 at 24 chunks, at pitches down
//! to one degree off horizontal, looking at a floor and at a ceiling) is real,
//! and it is narrower than it looks. Its air gap is built from **all-air
//! sections**, which are never meshed, never enter the visibility graph, and are
//! therefore handed `SectionVisibility::all()` by `walk_visible_bounded` — the
//! most permissive connectivity a section can ever have. The BFS in that world
//! never evaluates a connectivity gate that can say *no*, so **no arrangement of
//! camera or distance in that fixture can fail for an over-culling walk**.
//!
//! A real ceiling is a cave roof or a building. Its sections carry *partial*
//! connectivity, and the "can I pass from the face I entered through to the face
//! I am leaving through" test actually decides something. This file builds that.
//!
//! # The fixture, and why its geometry is the point
//!
//! An enclosed hall: solid stone through section rows 3, 4 and 5 (`y` 48..95)
//! over a 25x25-chunk region, with an 8-block-tall air gap carved out of the
//! middle row across chunks -11..10 on both horizontal axes. Two worlds, which
//! differ only in where inside row 4 the gap sits:
//!
//! | world | gap `y` | row 4 connectivity | the surface under test | reached by |
//! |---|---|---|---|---|
//! | floor | 64..71 | sides + `NegY`, **not** `PosY` | top of `y=63`, in row **3** | a `NegY` step out of row 4 |
//! | ceiling | 72..79 | sides + `PosY`, **not** `NegY` | bottom of `y=80`, in row **5** | a `PosY` step out of row 4 |
//!
//! Three properties matter and each is asserted rather than assumed (see
//! [`hall_row_connectivity_is_genuinely_partial`], which needs no GPU and no
//! assets, and the in-gate re-check through production's own
//! `snapshot_visibility`):
//!
//! * row 4 is 2048 opaque cells — past `SPARSE_OPAQUE_MAX`, so it really floods
//!   rather than taking the sparse `all()` shortcut;
//! * row 4 connects exactly one of the two `Y` faces, so a walk that transposed
//!   `NegY`/`PosY`, or that read the entry face as the travel direction, loses
//!   the entire floor in one world and the entire ceiling in the other;
//! * rows 3 and 5 are **fully solid** — they connect nothing, so they can only
//!   ever be *reached*, never travelled through. There is no row below row 3 in
//!   this world at all (`MIN_Y = 48`, three sections), which is deliberate: an
//!   all-air row underneath would be absent from the graph, read as `all()`, and
//!   let the walk flood in beneath the floor and reach every row-3 section
//!   sideways — silently restoring the very permissiveness this fixture exists
//!   to remove.
//!
//! # The detectors, and which one carries the verdict
//!
//! **Primary, and exact: an A/B on the occlusion walk itself.** The same world,
//! the same camera, the same uploads, rendered twice — once with
//! `TerrainOcclusion::On` (production) and once with `TerrainOcclusion::Off`.
//! Every section the walk removes is by definition one the frustum and distance
//! tests kept, and all terrain here is opaque and depth-tested, so drawing
//! *more* of it cannot change a single pixel unless something drawn in the
//! `Off` arm was actually visible. The two frames must therefore be
//! **byte-identical**, with no threshold anywhere. Any differing pixel is a
//! section the walk culled and the camera could see.
//!
//! This is a stronger instrument than the sandwiched-sky detector the prior
//! gates use, in the one way that matters here: it sees geometry dropped in
//! front of *other geometry*, not only geometry dropped in front of sky.
//!
//! **Secondary: no pixel is background at all.** The camera is sealed inside the
//! hall and every wall is inside render distance, so every ray hits solid,
//! in-range stone — the frame must contain no sky. Each background pixel is
//! re-checked by `far_grazing_ceiling_floor_holes.rs`'s independent ray-cast
//! oracle, transcribed here, so a pixel whose own ray genuinely escapes or lands
//! out of range is exculpated rather than accused. Say plainly what this arm is
//! worth in *this* scene: because every ray does hit in-range stone, the oracle
//! exculpates nothing and the arm reduces to the colour threshold `differs`. It
//! is a check that the scene is really sealed and that nothing *else* drops
//! geometry, not a second independent verdict on the walk.
//!
//! # The two controls, and why one is not enough
//!
//! A null result is a real possible outcome here, so an uncontrolled one is
//! worth nothing. Two separate things have to be shown, and the obvious control
//! only shows the first:
//!
//! * [`a_deliberately_missing_floor_section_is_detected`] withholds exactly one
//!   floor section's upload. Both detectors must fire, and every background
//!   pixel must be classified by the oracle as a genuine bug — the world still
//!   holds real, in-range stone there, only the GPU upload was skipped. This
//!   proves the **pixel comparison and the oracle** can see missing geometry.
//!   Measured: 31,192 pixels, 31,192 genuine, 0 excused.
//! * [`a_lying_connectivity_record_makes_the_walk_over_cull_and_the_ab_sees_it`]
//!   uploads one hall section's real geometry with a **`SectionVisibility::NONE`**
//!   connectivity record, so the production walk strands everything behind it.
//!   This proves the A/B arm can see **an over-culling walk**, which is the
//!   thing the main gate is actually claiming did not happen — and it is a
//!   different claim from the first control, because a missing upload is absent
//!   from *both* A/B arms and moves that comparison not at all. Measured: 10,695
//!   pixels, and 155 sections drawn against the healthy frame's 171.
//!
//! Without the second, "the two frames were byte-identical" stays equally
//! consistent with a comparison that could never have differed.
//!
//! ```text
//! cargo test -p lodestone-shell --test partial_connectivity_hall_holes -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, SKY_COLOR, TerrainOcclusion, ThirdPersonBodyState};
use lodestone::mesher::{
    SectionGeometry, SectionKey, mesh_snapshot_models, snapshot_section, snapshot_visibility,
};
use lodestone::resources::BlockResources;
use lodestone_render::{
    Camera, Face, GpuContext, HeadlessTarget, ModelMesh, RenderTarget, SectionVisibility,
    cull::within_view_distance, entity_anim::AnimInput, fog::FogSettings,
};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

/// See `far_grazing_ceiling_floor_holes.rs`'s identical helper's doc:
/// `RenderState` draws an unconditional first-person bare arm at a fixed screen
/// rect whenever no third-person body is reported, which reads as a hole and is
/// not one.
fn suppress_first_person_arm(state: &mut RenderState) {
    state.set_third_person_body_source(|| {
        Some(ThirdPersonBodyState {
            // No skin: this fixture installs a body to suppress the first-person
            // arm, not to assert a sheet. The draw falls back to the model's own
            // texture, exactly as it did before this field existed.
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

const W: u32 = 640;
const H: u32 = 480;
const FOV_Y_DEGREES: f32 = 40.0;

/// The world spans section rows 3, 4 and 5 only — see the module doc for why an
/// all-air row below the floor would gut the fixture.
const MIN_Y: i32 = 48;
const SECTION_COUNT: usize = 3;
const WORLD_TOP: i32 = MIN_Y + (SECTION_COUNT as i32) * 16;

/// Chunk radius actually loaded and uploaded: 25 x 25 columns.
const LOAD_RADIUS: i32 = 12;

/// The air gap's chunk extent. Aligned to chunk boundaries so every column is
/// either entirely hall or entirely wall — a partially-hall column would give
/// its row-4 section a *different* partial connectivity, which is a second
/// scene property and belongs in its own fixture rather than smuggled into this
/// one.
const HALL_CHUNK_LO: i32 = -11;
const HALL_CHUNK_HI: i32 = 10;

/// Render distance, in chunks. Chosen so **every** loaded chunk is in range:
/// the furthest corner column is `(12, 12)`, which `within_view_distance`
/// shrinks to `(11, 11)` and tests as `242 < 256`. Nothing in this world is
/// ever distance-culled, so a background pixel cannot be excused as "beyond
/// render distance" — which is exactly what makes the secondary detector's
/// expectation *zero* rather than "some legitimate band".
const RD_CHUNKS: i32 = 16;

/// Rows: 3 is solid floor, 4 carries the gap, 5 is solid ceiling.
const GAP_HEIGHT: i32 = 8;
/// The floor world's gap sits at the **bottom** of row 4, so row 4's open cells
/// touch its `NegY` face and not its `PosY` face.
const FLOOR_WORLD_GAP_LO: i32 = 64;
/// The ceiling world's gap sits at the **top** of row 4, the mirror image.
const CEILING_WORLD_GAP_LO: i32 = 72;

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

fn bbox_of(pixels: &[(u32, u32)]) -> Option<Rect> {
    pixels.iter().fold(None, |r, &(x, y)| Some(grow(r, x, y)))
}

/// Every pixel where two frames disagree on **any** byte.
///
/// Exact rather than thresholded, deliberately: both arms draw the same opaque,
/// depth-tested geometry through the same pipeline with the same shading, so the
/// winning fragment for a pixel either is or is not the same one. A tolerance
/// here would only be able to hide the thing the comparison exists to find.
fn exact_pixel_diff(a: &[u8], b: &[u8], w: u32, h: u32) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            if a[idx..idx + 4] != b[idx..idx + 4] {
                out.push((x, y));
            }
        }
    }
    out
}

/// Every pixel that still matches the no-terrain reference frame, i.e. every
/// pixel showing background rather than terrain.
fn find_background(subject: &[u8], reference: &[u8], w: u32, h: u32) -> Vec<(u32, u32)> {
    let mut out = Vec::new();
    for y in 0..h {
        for x in 0..w {
            let idx = ((y * w + x) * 4) as usize;
            if !differs(&subject[idx..idx + 4], &reference[idx..idx + 4]) {
                out.push((x, y));
            }
        }
    }
    out
}

/// Independent ray-cast oracle, transcribed from
/// `far_grazing_ceiling_floor_holes.rs`'s function of the same name: the ray
/// direction is transcribed (not called) from `Camera::basis`'s documented
/// closed form, and solidity comes straight from `World::block_state_at` — no
/// mesher, no rasteriser, no shader, no renderer code at all.
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

/// Amanatides-Woo voxel walk, transcribed from
/// `far_grazing_ceiling_floor_holes.rs` (where `dda_and_stepper_agree` pins it
/// against a slow fixed-step reference sampler). Visits exactly the voxels the
/// segment passes through and returns the first solid one as `(block
/// coordinate, entry point, entry distance)`. A `None` world read is air.
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

/// The only distance the oracle has to march. `within_view_distance` shrinks
/// each chunk-axis delta by one before testing a Euclidean disc of radius
/// `RD_CHUNKS`, so no in-range chunk sits further than `RD_CHUNKS + 1` chunks
/// away on either horizontal axis and the furthest *point* of such a chunk is
/// `(RD_CHUNKS + 2) * 16` blocks out per axis. Beyond that bound a hit cannot be
/// in range, and `classify_holes` already treats an out-of-range hit and a clean
/// miss identically.
fn max_in_range_distance() -> f32 {
    let horizontal = ((RD_CHUNKS + 2) * 16) as f32;
    (2.0 * horizontal * horizontal).sqrt()
}

/// Classifies flagged pixels by independently ray-casting each one: a pixel the
/// oracle also calls sky (no hit, or a hit beyond the renderer's own configured
/// view distance) is legitimate; a pixel whose ray should have hit an *in-range*
/// block is a genuine renderer bug. Returns `(legitimate, genuine_bugs)`.
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

/// The enclosed hall. Solid stone through the whole three-row world, with an
/// 8-block air gap carved out of row 4 across the hall's chunk extent.
///
/// `LightData::Uniform(15)` for the same reason
/// `far_grazing_ceiling_floor_holes.rs` needs it: a hermetic `World`'s light
/// defaults to `LightData::Missing`, which resolves to full dark and makes
/// subject and reference byte-identical everywhere.
fn hall_world(stone: u32, air: u32, gap_lo: i32) -> World {
    let mut world = World::new();
    for cx in -LOAD_RADIUS..=LOAD_RADIUS {
        for cz in -LOAD_RADIUS..=LOAD_RADIUS {
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
    let lo = -LOAD_RADIUS * 16;
    let hi = LOAD_RADIUS * 16 + 15;
    let solid = world.fill_region([lo, MIN_Y, lo], [hi, WORLD_TOP - 1, hi], stone);
    assert!(solid > 0, "fixture: the solid rock must actually write blocks");
    let hall_lo = HALL_CHUNK_LO * 16;
    let hall_hi = HALL_CHUNK_HI * 16 + 15;
    let carved = world.fill_region(
        [hall_lo, gap_lo, hall_lo],
        [hall_hi, gap_lo + GAP_HEIGHT - 1, hall_hi],
        air,
    );
    assert!(carved > 0, "fixture: the hall must actually carve air");
    world
}

/// `compute_visibility_from` over the raw world blocks — the fixture's own
/// geometry, with no models, no atlas and no GPU in the way.
fn raw_connectivity(world: &World, air: u32, coord: (i32, i32, i32)) -> SectionVisibility {
    let origin = [coord.0 * 16, coord.1 * 16, coord.2 * 16];
    lodestone_render::compute_visibility_from(|x, y, z| {
        world
            .block_state_at(
                origin[0] + x as i32,
                origin[1] + y as i32,
                origin[2] + z as i32,
            )
            .is_some_and(|state| state != air)
    })
}

/// The expected connectivity of a hall row-4 section, stated as the fixture's
/// geometry rather than read off the walk: the open cells span the full
/// horizontal extent so all four side faces share one region, they touch
/// whichever `Y` face the gap is flush against, and they never touch the other.
struct ExpectedRow4 {
    /// The `Y` face the gap is flush against.
    open_y: Face,
    /// The `Y` face sealed by stone.
    sealed_y: Face,
}

fn expected_row4(gap_lo: i32) -> ExpectedRow4 {
    // Row 4 spans y 64..79. The gap is 8 tall, so it is flush against exactly
    // one of the two boundaries.
    if gap_lo == 64 {
        ExpectedRow4 { open_y: Face::NegY, sealed_y: Face::PosY }
    } else {
        assert_eq!(gap_lo + GAP_HEIGHT, 80, "the gap must be flush against a row-4 boundary");
        ExpectedRow4 { open_y: Face::PosY, sealed_y: Face::NegY }
    }
}

const SIDES: [Face; 4] = [Face::NegX, Face::PosX, Face::NegZ, Face::PosZ];

/// Asserts a row-4 section really is the partial thing this whole file turns on,
/// and collects every mismatch rather than aborting on the first — an `assert!`
/// inside the loop would turn every later face pair from an observation back
/// into an argument.
fn check_row4(label: &str, vis: SectionVisibility, gap_lo: i32) -> Vec<String> {
    let expected = expected_row4(gap_lo);
    let mut bad = Vec::new();
    for a in SIDES {
        for b in SIDES {
            if !vis.connects(a, b) {
                bad.push(format!("{label}: {a:?} should connect {b:?} (one open region spans the hall)"));
            }
        }
        if !vis.connects(a, expected.open_y) {
            bad.push(format!("{label}: {a:?} should connect {:?} (the gap is flush against it)", expected.open_y));
        }
        if vis.connects(a, expected.sealed_y) {
            bad.push(format!("{label}: {a:?} must NOT connect {:?} (stone seals it)", expected.sealed_y));
        }
    }
    if vis.connects(expected.open_y, expected.sealed_y) {
        bad.push(format!("{label}: {:?} must NOT connect {:?}", expected.open_y, expected.sealed_y));
    }
    bad
}

/// Asserts a fully-solid row connects nothing but each face to itself.
fn check_solid_row(label: &str, vis: SectionVisibility) -> Vec<String> {
    let mut bad = Vec::new();
    for a in Face::ALL {
        for b in Face::ALL {
            if a != b && vis.connects(a, b) {
                bad.push(format!("{label}: solid rock must not connect {a:?} to {b:?}"));
            }
        }
    }
    bad
}

/// The fog this gate renders with: vanilla's colours and sky disc, but with both
/// ramps pushed past the far plane so nothing in render distance fades at all.
///
/// `far_grazing_ceiling_floor_holes.rs` carries the measurement showing why this
/// is mandatory rather than convenient — with production fog, a chunk
/// `within_view_distance` calls in range reaches past the point where the fade
/// is pinned at 1.0, so there is a permanent annulus rendering exactly the
/// background colour that no threshold can separate from a hole. Here the hall's
/// far wall sits at ~250 blocks against an unmodified `RD_CHUNKS = 16` ramp
/// ending at 256, so production fog would fade the very surface under test.
/// Both arms of every comparison use these settings.
fn unfogged_settings(color: [f32; 3]) -> FogSettings {
    let mut fog = FogSettings::for_render_distance(color, RD_CHUNKS as u32);
    let beyond = Camera::far_for_render_distance(RD_CHUNKS as u32, 0) * 4.0;
    fog.start = beyond;
    fog.end = beyond * 2.0;
    fog.environmental_start = beyond;
    fog.environmental_end = beyond * 2.0;
    fog
}

const FADE_COMPLETE_TICK: u64 = 200;

/// What, if anything, to do wrong on the way to the GPU. Both variants exist to
/// make a detector fail — see the two control tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Sabotage {
    /// Upload every section exactly as production would.
    None,
    /// Never upload this section at all: real geometry, absent from the GPU and
    /// from the visibility graph.
    SkipUpload(SectionKey),
    /// Upload this section's real geometry but record its connectivity as
    /// [`SectionVisibility::NONE`] — a section the walk believes is solid rock
    /// and cannot pass through, while the rasteriser still has every quad.
    ///
    /// This is the *only* lever in the crate that makes the production walk
    /// over-cull on purpose, so it is the only thing that can turn the A/B arm's
    /// argument into an observation.
    PoisonVisibility(SectionKey),
}

/// Builds a `RenderState` and uploads every section of `world` into it, subject
/// to `sabotage`. Returns the state and how many sections uploaded.
fn state_with_terrain(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    atlas: &lodestone_render::BlockAtlas,
    models: &lodestone_render::BlockModels,
    world: &World,
    occlusion: TerrainOcclusion,
    sabotage: Sabotage,
) -> (RenderState, usize) {
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas));
    suppress_first_person_arm(&mut state);
    // Two separate requirements in one call: the render distance must match this
    // fixture's own `RD_CHUNKS` (`RenderState` defaults it to 8, and a mismatch
    // makes the view-distance cull disagree with what the world and the far clip
    // cover), and the settings must be `unfogged_settings` — see its doc.
    state.set_fog(unfogged_settings(SKY_COLOR), RD_CHUNKS as u32);
    state.set_terrain_occlusion(occlusion);
    let mut uploaded = 0usize;
    for cx in -LOAD_RADIUS..=LOAD_RADIUS {
        for cz in -LOAD_RADIUS..=LOAD_RADIUS {
            for si in 0..SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: MIN_Y };
                if sabotage == Sabotage::SkipUpload(key) {
                    continue;
                }
                let Some(snap) = snapshot_section(world, key) else {
                    continue;
                };
                let opaque = mesh_snapshot_models(&snap, models, false);
                let visibility = if sabotage == Sabotage::PoisonVisibility(key) {
                    SectionVisibility::NONE
                } else {
                    snapshot_visibility(&snap, models)
                };
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
    state.update_animation(queue, FADE_COMPLETE_TICK);
    (state, uploaded)
}

/// A `RenderState` with no terrain at all: the reference for "this pixel is
/// background".
fn sky_only_state(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    atlas: &lodestone_render::BlockAtlas,
) -> RenderState {
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas));
    suppress_first_person_arm(&mut state);
    state.set_fog(unfogged_settings(SKY_COLOR), RD_CHUNKS as u32);
    state.update_animation(queue, FADE_COMPLETE_TICK);
    state
}

fn draw(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &mut HeadlessTarget,
    state: &RenderState,
    camera: &Camera,
) -> (Vec<u8>, lodestone::gpu::RenderStats) {
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

/// The fixture's own premise, asserted with no GPU and no assets so it cannot
/// rot behind `--ignored`: the hall's row-4 sections carry **partial**
/// connectivity, and the rows above and below carry none.
///
/// This is the whole difference between this file and
/// `far_grazing_ceiling_floor_holes.rs`. If this ever starts reporting
/// `SectionVisibility::all()` for row 4 — because the gap grew, the sparse
/// shortcut's threshold moved, or the hall stopped being carved — then the gate
/// below silently becomes a second copy of that file and proves nothing new.
#[test]
fn hall_row_connectivity_is_genuinely_partial() {
    let stone = first_state_named("minecraft:stone");
    let air = first_state_named("minecraft:air");

    let mut bad = Vec::new();
    for (label, gap_lo) in [("floor world", FLOOR_WORLD_GAP_LO), ("ceiling world", CEILING_WORLD_GAP_LO)] {
        let world = hall_world(stone, air, gap_lo);
        // A hall-interior column, and one two chunks in from the hall's edge, so
        // the claim is not resting on the single section under the camera.
        for (cx, cz) in [(0, 0), (-9, 8)] {
            let row4 = raw_connectivity(&world, air, (cx, 4, cz));
            bad.extend(check_row4(&format!("{label} row 4 at ({cx}, {cz})"), row4, gap_lo));
            assert_ne!(
                row4,
                SectionVisibility::all(),
                "{label}: row 4 at ({cx}, {cz}) is fully open — the fixture has degenerated into \
                 the all-air scene this file exists to be different from"
            );
            for row in [3, 5] {
                bad.extend(check_solid_row(
                    &format!("{label} row {row} at ({cx}, {cz})"),
                    raw_connectivity(&world, air, (cx, row, cz)),
                ));
            }
        }
    }
    assert!(bad.is_empty(), "the hall fixture is not the shape this gate needs: {bad:#?}");
}

/// The headline. See the module doc for the two detectors and what each is
/// worth.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_visibility_walk_never_culls_visible_geometry_in_a_partly_connected_hall() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let stone = first_state_named("minecraft:stone");
    let air = first_state_named("minecraft:air");
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let sky = sky_only_state(device, queue, format, &atlas);

    // Grazing pitches first, because that is the owner's report; the moderate
    // ones are what make a vertical-step failure visible as a large blank
    // surface rather than a thin band. Two yaws, because a `Face::ALL`-order
    // dependence in the walk is angle-dependent and 0 degrees is exactly on an
    // axis — the coincident input.
    let configs: &[(f32, f32)] = &[
        (1.0, 0.0),
        (-1.0, 0.0),
        (5.0, 37.0),
        (-5.0, 37.0),
        (25.0, 0.0),
        (-25.0, 0.0),
        (60.0, 111.0),
        (-60.0, 111.0),
    ];

    let mut failures: Vec<String> = Vec::new();
    for (world_label, gap_lo) in [
        ("floor (gap flush to row 4's NegY; floor surface lives in solid row 3)", FLOOR_WORLD_GAP_LO),
        ("ceiling (gap flush to row 4's PosY; ceiling surface lives in solid row 5)", CEILING_WORLD_GAP_LO),
    ] {
        let world = hall_world(stone, air, gap_lo);
        let (on, uploaded) =
            state_with_terrain(device, queue, format, &atlas, models, &world, TerrainOcclusion::On, Sabotage::None);
        let (off, uploaded_off) =
            state_with_terrain(device, queue, format, &atlas, models, &world, TerrainOcclusion::Off, Sabotage::None);
        assert!(uploaded > 0 && uploaded == uploaded_off, "fixture: both arms must upload the same sections");

        // The graph the walk actually consumes, through production's own
        // producer — the raw-block check in `hall_row_connectivity_is_genuinely_partial`
        // proves the *geometry* is partial, this proves nothing between the
        // world and the graph flattened it back to `all()`.
        let key = SectionKey { cx: 0, cz: 0, si: 1, min_y: MIN_Y };
        let snap = snapshot_section(&world, key).expect("the camera's own row-4 section must mesh");
        let production_vis = snapshot_visibility(&snap, models);
        failures.extend(check_row4(&format!("{world_label}: production snapshot_visibility"), production_vis, gap_lo));

        let eye_y = gap_lo as f32 + 4.5;
        for &(pitch, yaw) in configs {
            let camera = camera_at(eye_y, pitch, yaw);
            let (frame_on, stats_on) = draw(device, queue, &mut target, &on, &camera);
            let (frame_off, stats_off) = draw(device, queue, &mut target, &off, &camera);
            let (frame_sky, _) = draw(device, queue, &mut target, &sky, &camera);

            assert!(
                stats_on.occlusion_active,
                "{world_label} pitch={pitch} yaw={yaw}: the reachable set did not install, so the \
                 A/B below is measuring nothing — see `frame_reachable`'s `None` cases"
            );
            assert!(
                !stats_off.occlusion_active,
                "{world_label} pitch={pitch} yaw={yaw}: the control arm still has occlusion \
                 enforcing, so both arms are the same frame"
            );

            let walk_diff = exact_pixel_diff(&frame_on, &frame_off, W, H);
            let background = find_background(&frame_on, &frame_sky, W, H);
            let (legitimate, genuine) =
                classify_holes(&world, air, &camera, RD_CHUNKS as u32, &background, W, H);

            eprintln!(
                "=== {world_label} | pitch={pitch} yaw={yaw} (RD={RD_CHUNKS} chunks) ===\n\
                 sections drawn (occl on/off) = {} / {}\n\
                 sections culled by occlusion = {}\n\
                 occlusion graph sections     = {}\n\
                 walk A/B differing pixels    = {} (expect 0), bbox {:?}\n\
                 background pixels            = {} (expect 0), bbox {:?}\n\
                 oracle-legitimate            = {legitimate}\n\
                 oracle-genuine bugs          = {}",
                stats_on.sections_drawn,
                stats_off.sections_drawn,
                stats_on.sections_culled_occlusion,
                stats_on.occlusion_graph_sections,
                walk_diff.len(),
                bbox_of(&walk_diff),
                background.len(),
                bbox_of(&background),
                genuine.len(),
            );

            // The walk must actually be removing sections, or the A/B is
            // comparing two identical frames and would pass with the walk
            // deleted. Row 3 and row 5 outside the camera's own column, plus
            // every wall section, are unreachable by construction.
            if stats_on.sections_culled_occlusion == 0 {
                failures.push(format!(
                    "{world_label} pitch={pitch} yaw={yaw}: the occlusion cull removed nothing, so \
                     the A/B arm is vacuous here"
                ));
            }
            if !walk_diff.is_empty() {
                let (_, walk_genuine) =
                    classify_holes(&world, air, &camera, RD_CHUNKS as u32, &walk_diff, W, H);
                failures.push(format!(
                    "{world_label} pitch={pitch} yaw={yaw}: the occlusion walk changed {} pixels \
                     (bbox {:?}); {} of them are pixels whose own ray hits real, in-range geometry. \
                     first few: {:?}",
                    walk_diff.len(),
                    bbox_of(&walk_diff),
                    walk_genuine.len(),
                    &walk_diff[..walk_diff.len().min(8)],
                ));
            }
            if !genuine.is_empty() {
                failures.push(format!(
                    "{world_label} pitch={pitch} yaw={yaw}: {} background pixels whose own \
                     independent ray-cast says the ray hits real, in-render-distance stone. \
                     first few: {:?}",
                    genuine.len(),
                    &genuine[..genuine.len().min(8)],
                ));
            }
        }
    }

    assert!(
        failures.is_empty(),
        "the partly-connected hall lost geometry the camera can see:\n{failures:#?}"
    );
}

/// Control: do both detectors fire on a real, known hole in this world?
///
/// Skips exactly one floor section's upload and requires the A/B pixel diff to
/// be non-empty *and* every background pixel to be classified by the oracle as a
/// genuine bug — the world still holds real, in-range stone there, so an oracle
/// that excused any of them could also launder a real missing-upload into a
/// false pass above.
///
/// Note which arm this control does and does not cover. It proves the *pixel
/// comparison* and the *oracle* can see a missing section. It does **not** prove
/// the A/B can see an over-culling walk: a section absent from the GPU is absent
/// from both A/B arms equally, so that comparison would not move at all if this
/// were the only control.
/// [`a_lying_connectivity_record_makes_the_walk_over_cull_and_the_ab_sees_it`]
/// is the one that covers it.
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
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let world = hall_world(stone, air, FLOOR_WORLD_GAP_LO);
    let eye_y = FLOOR_WORLD_GAP_LO as f32 + 4.5;

    // The floor surface is the top of `y = 63`, which lives in `si = 0`. The
    // victim column is one chunk ahead (yaw 0 looks along `+Z`), and the pitch
    // is derived from the fixture rather than guessed: the victim spans
    // `z = 16..31` at a drop of 4.5 blocks, i.e. 8.3 to 15.7 degrees below the
    // horizon, so 12 degrees puts it near the middle of a 40-degree frame.
    let victim = SectionKey { cx: 0, cz: 1, si: 0, min_y: MIN_Y };
    let camera = camera_at(eye_y, 12.0, 0.0);

    let sky = sky_only_state(device, queue, format, &atlas);
    let (full, uploaded_full) =
        state_with_terrain(device, queue, format, &atlas, models, &world, TerrainOcclusion::On, Sabotage::None);
    let (holed, uploaded_holed) = state_with_terrain(
        device, queue, format, &atlas, models, &world, TerrainOcclusion::On, Sabotage::SkipUpload(victim),
    );
    assert_eq!(
        uploaded_full,
        uploaded_holed + 1,
        "control fixture: exactly one section must have been withheld"
    );

    let (frame_full, _) = draw(device, queue, &mut target, &full, &camera);
    let (frame_holed, stats) = draw(device, queue, &mut target, &holed, &camera);
    let (frame_sky, _) = draw(device, queue, &mut target, &sky, &camera);

    let diff = exact_pixel_diff(&frame_full, &frame_holed, W, H);
    let background = find_background(&frame_holed, &frame_sky, W, H);
    let (legitimate, genuine) =
        classify_holes(&world, air, &camera, RD_CHUNKS as u32, &background, W, H);

    eprintln!(
        "=== control: floor section {victim:?} never uploaded ===\n\
         sections drawn        = {}\n\
         pixel diff vs full    = {} (expect > 0), bbox {:?}\n\
         background pixels     = {} (expect > 0), bbox {:?}\n\
         oracle-legitimate     = {legitimate} (expect 0)\n\
         oracle-genuine bugs   = {} (expect {})",
        stats.sections_drawn,
        diff.len(),
        bbox_of(&diff),
        background.len(),
        bbox_of(&background),
        genuine.len(),
        background.len(),
    );

    assert!(
        !diff.is_empty(),
        "control failed to fire: one floor section was deliberately never uploaded and the exact \
         pixel comparison found no difference at all — fix this before trusting a clean result \
         from the main gate."
    );
    assert!(
        !background.is_empty(),
        "control failed to fire: one floor section was deliberately never uploaded and no pixel \
         showed background — the camera is not looking at the victim."
    );
    assert_eq!(
        genuine.len(),
        background.len(),
        "control's own oracle check failed: {legitimate} of {} deliberately-missing-section pixels \
         were classified as legitimate out-of-range sky instead of a genuine bug — an oracle that \
         can be fooled by this would also launder a real missing-upload into a false pass on the \
         main gate.",
        background.len(),
    );
}

/// The control the A/B arm actually needs: make the production walk over-cull
/// on purpose, and require the A/B to see it.
///
/// One row-4 hall section three chunks straight ahead is uploaded with its real
/// geometry and a **lying** connectivity of [`SectionVisibility::NONE`]. The
/// walk now believes that section is solid rock. Every hall section beyond it on
/// the camera's own axis becomes unreachable — not because of the lie alone but
/// because of the never-reverse-an-axis rule, which forbids the only detours
/// around it: any path leaving `cx = 0` would have to come back, and a walk that
/// has travelled `-X` may never travel `+X` again. So the frame loses a wedge of
/// hall that the camera can plainly see, through the real
/// `walk_visible_bounded`, with no test double anywhere in the path.
///
/// This is what separates the main gate's A/B arm from an argument. Without it,
/// "the two frames were byte-identical" is equally consistent with a comparison
/// that could never have differed.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_lying_connectivity_record_makes_the_walk_over_cull_and_the_ab_sees_it() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let stone = first_state_named("minecraft:stone");
    let air = first_state_named("minecraft:air");
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let world = hall_world(stone, air, FLOOR_WORLD_GAP_LO);
    let eye_y = FLOOR_WORLD_GAP_LO as f32 + 4.5;
    // Level, looking along `+Z`, so the wedge the lie strands is straight ahead
    // and fills the middle of the frame.
    let camera = camera_at(eye_y, 0.0, 0.0);
    let liar = SectionKey { cx: 0, cz: 3, si: 1, min_y: MIN_Y };

    let (poisoned, uploaded_poisoned) = state_with_terrain(
        device, queue, format, &atlas, models, &world,
        TerrainOcclusion::On, Sabotage::PoisonVisibility(liar),
    );
    let (unculled, uploaded_unculled) = state_with_terrain(
        device, queue, format, &atlas, models, &world,
        TerrainOcclusion::Off, Sabotage::PoisonVisibility(liar),
    );
    assert_eq!(
        uploaded_poisoned, uploaded_unculled,
        "control fixture: both arms must upload the same sections — the lie is in the \
         connectivity record, not in what reaches the GPU"
    );

    let (frame_poisoned, stats_poisoned) = draw(device, queue, &mut target, &poisoned, &camera);
    let (frame_unculled, stats_unculled) = draw(device, queue, &mut target, &unculled, &camera);
    let sky = sky_only_state(device, queue, format, &atlas);
    let (frame_sky, _) = draw(device, queue, &mut target, &sky, &camera);

    let diff = exact_pixel_diff(&frame_poisoned, &frame_unculled, W, H);
    let background = find_background(&frame_poisoned, &frame_sky, W, H);
    let (legitimate, genuine) =
        classify_holes(&world, air, &camera, RD_CHUNKS as u32, &background, W, H);

    eprintln!(
        "=== control: section {liar:?} uploaded with a NONE connectivity record ===\n\
         sections drawn (occl on/off) = {} / {}\n\
         sections culled by occlusion = {}\n\
         walk A/B differing pixels    = {} (expect > 0), bbox {:?}\n\
         background pixels            = {} (expect > 0), bbox {:?}\n\
         oracle-legitimate            = {legitimate} (expect 0)\n\
         oracle-genuine bugs          = {}",
        stats_poisoned.sections_drawn,
        stats_unculled.sections_drawn,
        stats_poisoned.sections_culled_occlusion,
        diff.len(),
        bbox_of(&diff),
        background.len(),
        bbox_of(&background),
        genuine.len(),
    );

    assert!(
        !diff.is_empty(),
        "control failed to fire: a hall section three chunks ahead was recorded as impassable, \
         the walk must therefore have stranded everything behind it, and the exact pixel \
         comparison between the enforcing and the non-enforcing arm found no difference at all. \
         The main gate's A/B arm is measuring nothing."
    );
    assert!(
        !background.is_empty(),
        "control fired on the A/B but the stranded wedge did not reach sky, so the secondary \
         background detector was not exercised by this control."
    );
    assert_eq!(
        genuine.len(),
        background.len(),
        "{legitimate} of {} pixels stranded by a deliberately over-culling walk were classified \
         as legitimate out-of-range sky — an oracle that excuses those would excuse a real \
         over-cull too.",
        background.len(),
    );
}
