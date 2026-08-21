//! Pixel gates for the report *"some blocks are popping in and out weirdly,
//! like z-fighting-ish — for example, the leaves on the ground"*.
//!
//! # What this renders
//!
//! A flat grass world with a **ground plate** laid over the whole surface —
//! `leaf_litter`, a carpet, a snow layer, a pressure plate, a rail — meshed
//! through the real production path ([`snapshot_section`] +
//! [`mesh_snapshot_models`] + [`SectionGeometry::Model`]) and drawn through
//! the real [`RenderState`] against the real vanilla atlas and its real mip
//! chain. **No geometry is installed by the harness**: the plate's quads come
//! from the `client.jar` bake, so the plane the depth buffer has to separate
//! is the real one — `leaf_litter` sits `0.25/16 = 0.015625` blocks above its
//! block's floor, a carpet at `1/16`, a snow layer at `2/16`.
//!
//! # Why the obvious detector is the wrong one here, measured
//!
//! The instinctive test for z-fighting is a **sub-pixel camera nudge**: render
//! two frames from cameras a hair apart and count pixels that changed. That
//! detector was built, and then its control was run — the same production mesh
//! with the plate snapped **exactly** onto the block boundary, a separation no
//! projection can resolve. It moved **37 of 196608 pixels (0.02%)**, i.e. it
//! did not fire, and the reason is structural rather than a threshold problem:
//! a plate quad and the grass block's top quad have **identical `x`/`z`
//! extents**, so once their depths collapse to the same value the rasteriser
//! interpolates *identical* depth across both, and `LessEqual`
//! ([`lodestone_render::model_pipeline`]) hands the pixel to whichever was
//! drawn later — the same one, every frame, at every camera. Coplanar
//! full-block plates in this renderer do not speckle. They flip **wholesale**,
//! and only when draw order changes.
//!
//! So the detector here is [`reupload_instability`]: render one camera through
//! two **independently built** `RenderState`s — two separate meshing and
//! suballocation passes over the same world, which is what a remesh or a
//! chunk-reload does in a live session — and count pixels that disagree. That
//! is the axis a coplanar surface is actually unstable along, and its control
//! fires hard.
//!
//! # The second reading: dissolving at range
//!
//! [`coverage_by_band`] diffs the plated world against the identical world
//! with **no plate**, per horizontal band of the frame. At a shallow pitch a
//! band is a distance band, so a plate that stops being drawn (a cutout whose
//! mip alpha falls under the discard threshold, say) shows as coverage going
//! to zero in the far bands while the ground behind it is still plainly drawn.
//! Bands above the horizon are excluded by **derivation from the camera**, not
//! by eyeballing which ones looked like sky.
//!
//! # Scope, stated plainly
//!
//! These prove the **draw**: real baked geometry, real mesher, real pipeline,
//! real atlas. They do not exercise the wire or the ECS — the world is built
//! in-process — so they cannot speak to anything that goes wrong between a
//! server chunk packet and `World`, and they cannot represent a world with
//! uneven terrain around the plate.
//!
//! Fail-closed: no GPU adapter or no vanilla `client.jar` is a failure, never
//! a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test ground_plate_z_fight_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, ThirdPersonBodyState};
use lodestone::mesher::{
    SectionGeometry, SectionKey, mesh_snapshot_models, snapshot_section, snapshot_visibility,
};
use lodestone::resources::BlockResources;
use lodestone_render::{
    Camera, GpuContext, HeadlessTarget, ModelMesh, RenderTarget, entity_anim::AnimInput,
};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

const W: u32 = 512;
const H: u32 = 384;
const FOV_Y_DEGREES: f32 = 70.0;

/// Radius of the loaded/meshed world, in chunks.
const RD_CHUNKS: i32 = 8;
const MIN_Y: i32 = 0;
/// Ground fills `[MIN_Y, SURFACE_Y)`; the plate sits at `SURFACE_Y`.
const SURFACE_Y: i32 = 64;
const SECTION_COUNT: usize = 6;

/// Every plate this file exercises, with the height its horizontal quads sit
/// at above the block floor (measured by
/// `lodestone-render`'s `ground_plane_coplanarity_census`, not recalled).
const PLATES: &[(&str, &str)] = &[
    ("minecraft:leaf_litter[facing=north,segment_amount=4]", "0.015625"),
    ("minecraft:white_carpet", "0.0625"),
    ("minecraft:snow[layers=1]", "0.125"),
    ("minecraft:oak_pressure_plate[powered=false]", "0.0625"),
    ("minecraft:rail[shape=north_south,waterlogged=false]", "0.0625"),
];

/// Suppress `RenderState`'s unconditional first-person bare arm, which
/// otherwise paints a fixed screen rect in every frame — a documented
/// false-positive source for exactly this class of gate (`CLAUDE.md`).
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
        })
    });
}

fn state_id(state: &str) -> u32 {
    lodestone_data::block_states::state_id(state)
        .unwrap_or_else(|| panic!("{state} is not in the 26.2 block-state table"))
}

/// Grass ground up to `SURFACE_Y`; one layer of `plate` on top when `Some`.
fn plated_world(ground: u32, plate: Option<u32>, air: u32) -> World {
    let mut world = World::new();
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
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
                // A hermetic column's light defaults to `Missing`, which
                // resolves to 0 — everything renders black and the detectors
                // measure nothing. See `distant_flat_terrain_holes.rs`.
                *light.sky_mut(i) = lodestone_world::LightData::Uniform(15);
                *light.block_mut(i) = lodestone_world::LightData::Uniform(0);
            }
            world.load(
                ChunkPos::new(cx, cz),
                LoadedChunk::new(column, light, Heightmaps::new(), Vec::new()),
            );
        }
    }
    let lo = -RD_CHUNKS * 16;
    let hi = RD_CHUNKS * 16 + 15;
    let ground_written = world.fill_region([lo, MIN_Y, lo], [hi, SURFACE_Y - 1, hi], ground);
    assert!(ground_written > 0, "fixture: ground must actually be written");
    if let Some(plate) = plate {
        let plate_written = world.fill_region([lo, SURFACE_Y, lo], [hi, SURFACE_Y, hi], plate);
        assert!(plate_written > 0, "fixture: plate must actually be written");
    }
    world
}

/// How a section's model mesh is perturbed before upload.
#[derive(Clone, Copy, PartialEq)]
enum Perturb {
    /// Production geometry, untouched.
    None,
    /// The deliberate defect: every vertex whose fractional `y` lies strictly
    /// between the block boundary and `bound` is snapped **down** onto the
    /// boundary, so the plate becomes exactly coplanar with the grass block's
    /// top face. Applied to the production mesh rather than to a hand-built
    /// one, so the control differs from the subject in exactly this property.
    SnapPlateToBoundary { bound: f32 },
}

fn perturb(mesh: &mut ModelMesh, how: Perturb) -> usize {
    let Perturb::SnapPlateToBoundary { bound } = how else {
        return 0;
    };
    let mut moved = 0usize;
    for v in &mut mesh.vertices {
        let frac = v.position[1] - v.position[1].floor();
        if frac > 0.0 && frac <= bound {
            v.position[1] = v.position[1].floor();
            moved += 1;
        }
    }
    moved
}

/// Past the `0.75`s section fade-in — without this every section renders as
/// pure fog colour. See `distant_flat_terrain_holes.rs`'s `render_frame` doc.
const FADE_COMPLETE_TICK: u64 = 200;

/// A `RenderState` with the whole world already meshed and uploaded through
/// the production path, ready to render any number of cameras.
struct Scene {
    state: RenderState,
    moved: usize,
}

fn build_scene(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    atlas: &lodestone_render::BlockAtlas,
    world: &World,
    models: &lodestone_render::BlockModels,
    how: Perturb,
) -> Scene {
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas));
    suppress_first_person_arm(&mut state);
    let mut uploaded = 0usize;
    let mut moved = 0usize;
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            for si in 0..SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: MIN_Y };
                let Some(snap) = snapshot_section(world, key) else {
                    continue;
                };
                let mut opaque = mesh_snapshot_models(&snap, models, false);
                moved += perturb(&mut opaque, how);
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
    assert!(uploaded > 0, "fixture: some sections must have uploaded");
    state.update_animation(queue, FADE_COMPLETE_TICK);
    Scene { state, moved }
}

fn render(
    scene: &mut Scene,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &mut HeadlessTarget,
    camera: &Camera,
) -> Vec<u8> {
    let frame = target.acquire().expect("headless acquire");
    let _ = scene
        .state
        .render(device, queue, frame.view(), camera, None, &[]);
    target.read_texels(device, queue)
}

fn camera_at(pitch_degrees: f32, yaw_degrees: f32) -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, SURFACE_Y as f32 + 1.0 + 1.62, 0.5),
        yaw: yaw_degrees,
        pitch: pitch_degrees,
        fov_y_degrees: FOV_Y_DEGREES,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS as u32, 0),
    }
}

/// The screen row the horizon (elevation `0`) lands on, derived from the
/// camera rather than read off a picture. Positive pitch looks down, so the
/// horizon sits *above* the frame centre by `tan(pitch) / tan(fov/2)` of half
/// the frame height. Everything strictly below this row is ground.
fn horizon_row(camera: &Camera) -> f32 {
    let half = f64::from(H) / 2.0;
    let t = f64::from(camera.pitch.to_radians().tan())
        / f64::from((camera.fov_y_degrees / 2.0).to_radians().tan());
    (half - half * t) as f32
}

/// Per-channel difference above which two frames' pixels count as materially
/// different. Well above dither/rounding, well below a plate-vs-ground swap.
const FLIP_CHANNEL_DELTA: i32 = 24;

fn differs(p: &[u8], q: &[u8]) -> bool {
    (0..3).any(|c| (i32::from(p[c]) - i32::from(q[c])).abs() > FLIP_CHANNEL_DELTA)
}

fn flipped_pixels(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(p, q)| differs(p, q))
        .count()
}

/// Differing-pixel count per horizontal band, top of frame first.
fn by_band(a: &[u8], b: &[u8], bands: usize) -> Vec<usize> {
    let rows_per = H as usize / bands;
    let mut out = vec![0usize; bands];
    for y in 0..H as usize {
        let band = (y / rows_per).min(bands - 1);
        for x in 0..W as usize {
            let i = (y * W as usize + x) * 4;
            if differs(&a[i..i + 4], &b[i..i + 4]) {
                out[band] += 1;
            }
        }
    }
    out
}

fn gpu() -> GpuContext {
    GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         do NOT treat a skip as a pass",
    )
}

fn load_vanilla() -> std::sync::Arc<lodestone_render::BlockAtlas> {
    let resources = BlockResources::load(true);
    resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "vanilla assets did not load (banner: {:?}) — this gate needs a real \
             client.jar under .cache/mc/26.2",
            resources.banner
        )
    })
}

/// Pitches chosen for very different depth gradients across the plate:
/// near-straight-down (the largest depth separation the plate ever gets), a
/// normal walking view, and a shallow look toward the horizon (the smallest,
/// and the one that puts the far edge of the loaded world on screen).
const PITCHES: &[(&str, f32)] = &[("down 80", 80.0), ("walk 20", 20.0), ("graze 3", 3.0)];

/// Render one camera through **two independently built** `RenderState`s and
/// count pixels that disagree. Two uploads of one world is what a remesh or a
/// chunk reload produces in a live session; a surface whose winner is decided
/// by draw order rather than by depth flips wholesale between them.
///
/// Returns `(label, disagreeing pixels, vertices perturbed)` per pitch.
fn reupload_instability(
    plate_state: &str,
    how: Perturb,
    ctx: &GpuContext,
    atlas: &lodestone_render::BlockAtlas,
) -> Vec<(String, usize, usize)> {
    let device = ctx.device();
    let queue = ctx.queue();
    let models = atlas.models().expect("vanilla atlas must carry baked models");
    let world = plated_world(
        state_id("minecraft:grass_block[snowy=false]"),
        Some(state_id(plate_state)),
        state_id("minecraft:air"),
    );
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let mut first = build_scene(device, queue, format, atlas, &world, models, how);
    let firsts: Vec<Vec<u8>> = PITCHES
        .iter()
        .map(|&(_, pitch)| render(&mut first, device, queue, &mut target, &camera_at(pitch, 12.0)))
        .collect();
    let moved = first.moved;
    drop(first);

    let mut second = build_scene(device, queue, format, atlas, &world, models, how);
    PITCHES
        .iter()
        .zip(firsts)
        .map(|(&(label, pitch), a)| {
            let b = render(&mut second, device, queue, &mut target, &camera_at(pitch, 12.0));
            (label.to_string(), flipped_pixels(&a, &b), moved)
        })
        .collect()
}

/// How many bands the frame is split into for the dissolve detector.
const BANDS: usize = 8;
/// Shallow enough that the far bands are genuinely distant ground, steep
/// enough that most of the frame is ground rather than sky.
const DISSOLVE_PITCH: f32 = 8.0;

/// Pixels the plate paints, per distance band, at [`DISSOLVE_PITCH`]: the
/// plated world diffed against the identical world with no plate at all.
/// Also returns the derived horizon row, so a caller can exclude sky bands
/// without guessing which ones they are.
fn coverage_by_band(
    plate_state: &str,
    ctx: &GpuContext,
    atlas: &lodestone_render::BlockAtlas,
) -> (Vec<usize>, f32) {
    let device = ctx.device();
    let queue = ctx.queue();
    let models = atlas.models().expect("vanilla atlas must carry baked models");
    let ground = state_id("minecraft:grass_block[snowy=false]");
    let air = state_id("minecraft:air");
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = camera_at(DISSOLVE_PITCH, 12.0);

    let bare = plated_world(ground, None, air);
    let mut bare_scene = build_scene(device, queue, format, atlas, &bare, models, Perturb::None);
    let bare_px = render(&mut bare_scene, device, queue, &mut target, &camera);
    drop(bare_scene);

    let plated = plated_world(ground, Some(state_id(plate_state)), air);
    let mut plated_scene =
        build_scene(device, queue, format, atlas, &plated, models, Perturb::None);
    let plated_px = render(&mut plated_scene, device, queue, &mut target, &camera);

    (by_band(&bare_px, &plated_px, BANDS), horizon_row(&camera))
}

/// Total pixels in a frame, for reporting flips as a fraction.
const FRAME_PIXELS: usize = (W * H) as usize;

/// A second, independent upload of one unchanged world must reproduce the
/// frame. Measured: the real plates land at `0.0000..0.0002` and the coplanar
/// control at `0.09`, so this budget is fitted to neither side.
const FLIP_BUDGET_FRACTION: f64 = 0.02;

/// The control, and it must be read before any clean result below is believed:
/// the same production mesh with the plate snapped onto the block boundary is
/// decided by draw order, so two independent uploads disagree.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_coplanar_plate_is_detected() {
    let ctx = gpu();
    let atlas = load_vanilla();
    // `0.02` is above `leaf_litter`'s real `0.015625` offset and below every
    // other plate height in 26.2 (a carpet is `0.0625`), so it snaps exactly
    // the plate and nothing else.
    let how = Perturb::SnapPlateToBoundary { bound: 0.02 };
    let rows = reupload_instability(PLATES[0].0, how, &ctx, &atlas);
    let mut fired = 0usize;
    for (label, flips, moved) in &rows {
        let frac = *flips as f64 / FRAME_PIXELS as f64;
        println!(
            "control {label}: {flips}/{FRAME_PIXELS} disagree ({frac:.4}), {moved} vertices snapped"
        );
        assert!(
            *moved > 0,
            "control {label}: the deliberate defect moved no vertices — it did not run"
        );
        if frac > FLIP_BUDGET_FRACTION {
            fired += 1;
        }
    }
    assert!(
        fired > 0,
        "the control did not fire in any config: a plate made exactly coplanar with the \
         ground reproduced across two independent uploads, so the detector measures \
         nothing and no clean result from it is evidence"
    );
    println!("control fired in {fired}/{} configs", rows.len());
}

/// The ground-plate family across two independent uploads. Each entry sits at
/// a different real height above its block's floor, so one code path is
/// exercised at three distinct depth separations.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_ground_plate_family_survives_a_second_independent_upload() {
    let ctx = gpu();
    let atlas = load_vanilla();
    // Collect and assert on the collection: an `assert!` inside the loop would
    // prove exactly one arm and leave the rest unmeasured.
    let mut failures = Vec::new();
    for &(plate, height) in PLATES {
        for (label, flips, _) in reupload_instability(plate, Perturb::None, &ctx, &atlas) {
            let frac = flips as f64 / FRAME_PIXELS as f64;
            println!("{plate} (y+{height}) {label}: {flips}/{FRAME_PIXELS} disagree ({frac:.4})");
            if frac > FLIP_BUDGET_FRACTION {
                failures.push(format!("{plate} {label}: {frac:.4}"));
            }
        }
    }
    assert!(failures.is_empty(), "unstable ground plates: {failures:?}");
}

/// The second reading of the report: a plate that stops being drawn at range.
/// Every band of the frame that is **below the derived horizon** — i.e. is
/// ground, not sky — must show the plate painting something.
///
/// The near band doubles as this gate's own liveness control: a run where the
/// plate never drew at all fails rather than reading as "nothing dissolved".
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_ground_plate_keeps_painting_into_the_far_distance() {
    let ctx = gpu();
    let atlas = load_vanilla();
    let rows_per_band = H as usize / BANDS;
    let mut failures = Vec::new();
    for &(plate, height) in PLATES {
        let (bands, horizon) = coverage_by_band(plate, &ctx, &atlas);
        println!(
            "{plate} (y+{height}) coverage per band (far → near): {bands:?}, horizon row {horizon:.1}"
        );
        if bands[BANDS - 1] == 0 {
            failures.push(format!("{plate}: the plate painted nothing even up close"));
            continue;
        }
        for (i, &n) in bands.iter().enumerate() {
            // A band is ground only once its *top* row is below the horizon.
            let top_row = (i * rows_per_band) as f32;
            if top_row > horizon && n == 0 {
                failures.push(format!(
                    "{plate}: ground band {i} of {BANDS} (rows from {top_row}) shows no plate"
                ));
            }
        }
    }
    assert!(failures.is_empty(), "ground plate dissolves: {failures:?}");
}
