//! Extends `distant_flat_terrain_holes.rs` to genuinely **uneven** terrain —
//! issue #670's own most recent comment names this as the next harness,
//! because the flat gate "cannot speak to genuinely uneven terrain —
//! chunk-to-chunk elevation changes are exactly what it excludes."
//!
//! # The invariant, generalised rather than reinvented
//!
//! The flat harness's detector (`find_sandwiched_background`, reused
//! unmodified here) asserts that within a single screen column, a
//! background-classified run can never reappear below a terrain-classified
//! one. Its own doc justifies that from flatness: a flat plane can only be
//! crossed once by a ray as pitch increases. **That argument generalises to
//! any heightfield with no overhangs, not just a flat one**, which is why
//! this file needs no new detector — only a world that actually varies in
//! height.
//!
//! The proof: fix the camera position and yaw (one screen column), and let
//! `H(d)` be the terrain height at horizontal distance `d` along that ray's
//! ground track — a well-defined function for any *heightfield* (single
//! solid-below/air-above height per horizontal position; a "staircase" of
//! flat tiers and vertical risers, exactly what this file builds, still
//! qualifies — `H` is a step function, still single-valued). The ray's own
//! height at distance `d` and pitch `θ` is `h(d, θ) = eye_y - d·tan(θ)`, a
//! *straight line through the eye* for fixed `θ`. First-hit distance is the
//! smallest `d` where `h(d, θ) ≤ H(d)`. As `θ` increases (steeper look-down),
//! `h(d, θ)` decreases for every fixed `d > 0` — the line pivots downward
//! around the eye — so if a ray at `θ₁` first hits at distance `D`
//! (`h(D, θ₁) = H(D)`), a steeper ray at `θ₂ > θ₁` satisfies
//! `h(D, θ₂) ≤ h(D, θ₁) = H(D)`, i.e. it has **already** crossed the terrain
//! by `D`. So first-hit distance is non-increasing in pitch, which is exactly
//! "once a column goes from sky to terrain, steeper rays (further down the
//! screen) never go back to sky" — **independent of whether `H` is constant**.
//! Overhangs are the one thing that breaks the proof (`H` stops being a
//! single-valued function of horizontal position), and this world has none.
//!
//! So on this world, exactly as on the flat one, any background run
//! reappearing below a terrain run in the same screen column is not a
//! legitimate cliff, ridge or ledge — every one of those is already excluded
//! by construction — it is a bug. What changes from the flat harness is only
//! that the *world* now has real elevation, so the three candidate causes
//! that ground themselves in unevenness specifically (a hole punched through
//! the interior of one riser face, a gap at a chunk seam where an elevation
//! step also lands, and depth-test discard/bias on a near-vertical face) can
//! actually be exercised. Missing geometry is excluded exactly as before —
//! every column loads before any section is meshed
//! (`ColumnSource::Complete`) — and mip/gutter sampling is excluded on the
//! same already-fixed basis `distant_flat_terrain_holes.rs` records.
//!
//! # The world: a ziggurat, not a bump
//!
//! Three tiers, each a full section (16 blocks) taller than the last —
//! `LEVEL0_Y = 64` (the outer plain, identical to the flat harness's own
//! `SURFACE_Y`), `LEVEL1_Y = 80`, `LEVEL2_Y = 96` — nested square rings
//! centred on the world origin with half-widths `RING1_HALF = 32` and
//! `RING2_HALF = 16`. Both half-widths are chosen as **exact multiples of the
//! 16-block chunk width**, so every riser (the vertical step between tiers)
//! sits exactly on a chunk seam: this is deliberately the "gap along a chunk
//! seam where both sides are solid and coplanar" candidate the module doc
//! calls out, made unavoidable rather than incidental. Every riser height
//! (16 blocks) is also exactly one section, matching the flat harness's own
//! choice to land its solid/air interface on a *vertical* section boundary
//! too.
//!
//! The camera stands on the outer plain, offset from the ziggurat along `+Z`
//! at a **moderate distance** (matching the report's own framing and the
//! flat harness's own distance band) rather than adjacent to it, so both
//! risers are crossed by the same shallow, "walking around looking at the
//! horizon" rays the report describes — not a contrived close-up.
//!
//! ```text
//! cargo test -p lodestone-shell --test uneven_terrain_holes -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, ThirdPersonBodyState};
use lodestone::mesher::{SectionGeometry, SectionKey, mesh_snapshot_models, snapshot_section, snapshot_visibility};
use lodestone::resources::BlockResources;
use lodestone_render::{Camera, GpuContext, HeadlessTarget, ModelMesh, RenderTarget, entity_anim::AnimInput};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

/// See `distant_flat_terrain_holes.rs`'s identical helper for why this exists
/// at all — `RenderState`'s unconditional first-person bare arm draws a
/// fixed-screen-space 29px rect regardless of camera angle, which reads as a
/// hole and is not one.
fn suppress_first_person_arm(state: &mut RenderState) {
    state.set_third_person_body_source(|| {
        Some(ThirdPersonBodyState {
            feet: glam::Vec3::new(0.0, -10_000.0, 0.0),
            body_yaw_deg: 0.0,
            anim: AnimInput::default(),
            scale: 1.0,
            slim: false,
            equipment: Vec::new(),
        })
    });
}

const W: u32 = 640;
const H: u32 = 480;
const FOV_Y_DEGREES: f32 = 40.0;

/// Same render distance as the flat harness (10 chunks / 160 blocks).
const RD_CHUNKS: i32 = 10;
const MIN_Y: i32 = 0;

/// Tier heights — see the module doc for why each is a section boundary.
const LEVEL0_Y: i32 = 64;
const LEVEL1_Y: i32 = 80;
const LEVEL2_Y: i32 = 96;
/// Ring half-widths — exact multiples of the 16-block chunk width, so every
/// riser lands exactly on a chunk seam (see the module doc).
const RING1_HALF: i32 = 32;
const RING2_HALF: i32 = 16;

/// Sections per column: `LEVEL2_Y` (96) plus two sections (32 blocks) of
/// headroom, all measured from `MIN_Y`.
const SECTION_COUNT: usize = ((LEVEL2_Y - MIN_Y) / 16 + 2) as usize;

fn differs(subject: &[u8], reference: &[u8]) -> bool {
    let d = (i32::from(subject[0]) - i32::from(reference[0])).abs()
        + (i32::from(subject[1]) - i32::from(reference[1])).abs()
        + (i32::from(subject[2]) - i32::from(reference[2])).abs();
    d > DIFFERS_FROM_REFERENCE
}

/// See `distant_flat_terrain_holes.rs`'s identical constant's doc: derived
/// per-pixel against a no-terrain reference render of the same camera, never
/// a fixed sky colour.
const DIFFERS_FROM_REFERENCE: i32 = 60;

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

/// Identical to `distant_flat_terrain_holes.rs`'s function of the same name —
/// see its doc for the full argument. The module doc above is what changed:
/// the geometric justification now covers any heightfield, not just a flat
/// one, and this world actually exercises that generalisation.
fn find_sandwiched_background(
    subject: &[u8],
    reference: &[u8],
    w: u32,
    h: u32,
) -> (usize, Option<Rect>, usize) {
    let mut total = 0usize;
    let mut rect: Option<Rect> = None;
    let mut hole_columns = 0usize;
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
            }
        }
        if column_has_hole {
            hole_columns += 1;
        }
    }
    (total, rect, hole_columns)
}

/// Build the ziggurat world described in the module doc: a flat `LEVEL0_Y`
/// plain everywhere, with two nested, chunk-aligned square rings raised to
/// `LEVEL1_Y` and `LEVEL2_Y`. Filled outermost-tier-first so each smaller,
/// taller fill only *adds* height within its own footprint — order-agnostic
/// correctness, see the module doc's construction note.
fn ziggurat_world(stone: u32, air: u32) -> World {
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

    let base = world.fill_region([lo, MIN_Y, lo], [hi, LEVEL0_Y - 1, hi], stone);
    assert!(base > 0, "fixture: base tier must actually write blocks");

    let ring1 = world.fill_region(
        [-RING1_HALF, MIN_Y, -RING1_HALF],
        [RING1_HALF - 1, LEVEL1_Y - 1, RING1_HALF - 1],
        stone,
    );
    assert!(ring1 > 0, "fixture: ring1 tier must actually write blocks");

    let ring2 = world.fill_region(
        [-RING2_HALF, MIN_Y, -RING2_HALF],
        [RING2_HALF - 1, LEVEL2_Y - 1, RING2_HALF - 1],
        stone,
    );
    assert!(ring2 > 0, "fixture: ring2 tier must actually write blocks");

    world
}

#[allow(clippy::too_many_arguments)]
fn upload_all(
    world: &World,
    models: &lodestone_render::BlockModels,
    state: &mut RenderState,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    skip: Option<SectionKey>,
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
                if Some(key) == skip {
                    continue;
                }
                let Some(snap) = snapshot_section(world, key) else {
                    continue;
                };
                let opaque = mesh_snapshot_models(&snap, models, false);
                // See `distant_flat_terrain_holes.rs`'s identical comment: a
                // zero-quad section still needs uploading so it enters the
                // occlusion graph.
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

/// See `distant_flat_terrain_holes.rs`'s identical constant's doc: the fade
/// clock defect this file was originally written to diagnose. `render_frame`
/// below advances past it exactly as the fixed flat harness does.
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
    skip: Option<SectionKey>,
    upload_terrain: bool,
) -> (Vec<u8>, lodestone::gpu::RenderStats) {
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas));
    suppress_first_person_arm(&mut state);
    let uploaded = upload_all(world, models, &mut state, device, queue, skip, upload_terrain);
    assert!(
        !upload_terrain || uploaded > 0,
        "fixture: some sections must have uploaded when terrain is requested"
    );
    state.update_animation(queue, FADE_COMPLETE_TICK);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), camera, None, &[]);
    (target.read_texels(device, queue), stats)
}

/// Camera standing on the outer (`LEVEL0_Y`) plain, offset `STANDOFF_Z`
/// blocks from the ziggurat's centre along `-Z`, looking toward it (`yaw`
/// `0` in this engine points `+Z`, matching `distant_flat_terrain_holes.rs`'s
/// own convention). `STANDOFF_Z` puts the near riser (`RING1_HALF` = 32
/// blocks from centre) at a genuinely moderate distance —
/// `STANDOFF_Z - RING1_HALF` blocks away — rather than adjacent to the
/// camera, matching the report's "far away" framing.
const STANDOFF_Z: i32 = 130;

fn camera_at(pitch_degrees: f32, yaw_degrees: f32) -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, LEVEL0_Y as f32 + 1.62, -(STANDOFF_Z as f32) + 0.5),
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
            "vanilla assets did not load (banner: {:?}) — this gate needs a real \
             client.jar under .cache/mc/26.2 (LODESTONE_ASSETS)",
            resources.banner
        )
    });
    (resources, atlas)
}

/// The main gate: the ziggurat world, rendered from moderate distance at
/// several yaws (edge-on to a riser, corner-on where two risers meet, and
/// tangential/flat-only as a sanity repeat of the already-clean flat result)
/// and pitches (the report's own "same level as me" shallow look-down, plus
/// steep-down and slight-up controls). No sandwiched sky pixel should exist
/// anywhere in any frame — see the module doc for why that is a hard
/// geometric requirement here, not just on flat ground.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn uneven_terrain_at_moderate_distance_has_no_sky_holes() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let stone = first_state_named("minecraft:stone");
    let air = first_state_named("minecraft:air");
    let world = ziggurat_world(stone, air);

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    // yaw 0: dead centre, edge-on to both risers (the primary reproduction
    // of the report). yaw ±14: still within the ziggurat's angular width at
    // this distance but off-centre enough to cross a *corner* of the square
    // rings — two perpendicular riser faces meeting, the sharpest local
    // geometry in the scene. yaw 90: tangential, sees only the flat plain —
    // a cheap repeat of the already-proven-clean flat result, now inside the
    // same world and camera rig as the uneven configs. Pitches: the report's
    // own shallow "same level as me" as the primary probe, plus a steep
    // look-down (control: well below eye level, crosses the near riser's
    // base) and a slight look-up (control: above eye level).
    let configs: &[(&str, f32, f32)] = &[
        ("centred on risers, level", 2.0, 0.0),
        ("ring corner, level", 2.0, 14.0),
        ("ring corner, level (other side)", 2.0, -14.0),
        ("tangential (flat only), level", 2.0, 90.0),
        ("centred on risers, steep down", 20.0, 0.0),
        ("centred on risers, slight up", -10.0, 0.0),
    ];

    let mut any_hole = false;
    for &(label, pitch, yaw) in configs {
        let camera = camera_at(pitch, yaw);
        let (reference, _) = render_frame(
            device, queue, format, &mut target, &atlas, &world, models, &camera, None, false,
        );
        let (subject, stats) = render_frame(
            device, queue, format, &mut target, &atlas, &world, models, &camera, None, true,
        );

        // Sanity: the scene must actually show *something* other than flat
        // ground for the riser-facing configs, or this gate would be exactly
        // as vacuous as the original zero-pixel-readback state
        // `distant_flat_terrain_holes.rs` records. Count pixels that differ
        // from the reference at all (terrain of any tier, not just holes).
        let mut terrain_px = 0usize;
        for i in (0..subject.len()).step_by(4) {
            if differs(&subject[i..i + 4], &reference[i..i + 4]) {
                terrain_px += 1;
            }
        }

        let (hole_px, bbox, hole_cols) = find_sandwiched_background(&subject, &reference, W, H);
        eprintln!(
            "=== {label} (pitch={pitch}, yaw={yaw}) ===\n\
             sections drawn        = {}\n\
             sections culled dist  = {}\n\
             sections culled frust = {}\n\
             sections culled occl  = {} (occlusion_active={})\n\
             terrain pixels        = {terrain_px} / {}\n\
             sandwiched-sky pixels = {hole_px}\n\
             sandwiched-sky columns= {hole_cols} / {W}\n\
             bounding box          = {bbox:?}",
            stats.sections_drawn,
            stats.sections_culled_distance,
            stats.sections_culled_frustum,
            stats.sections_culled_occlusion,
            stats.occlusion_active,
            W * H,
        );
        assert!(
            terrain_px > 0,
            "{label}: zero pixels differed from the no-terrain reference — this scene is \
             vacuous (see distant_flat_terrain_holes.rs's own zero-pixel-readback history) \
             and a clean sandwiched-sky result here would prove nothing"
        );
        if hole_px > 0 {
            any_hole = true;
        }
    }

    assert!(
        !any_hole,
        "the ziggurat world produced sky pixels sandwiched inside its own terrain silhouette \
         in at least one config above — see the per-config bounding boxes printed to stderr. \
         Every column was loaded before any section was meshed, so this cannot be the \
         streaming-neighbour-arrival cause; the module doc's monotonicity proof means this is \
         not a legitimate cliff/ledge either — it points at the occlusion graph, depth \
         compositing/bias on a near-vertical riser face, or a chunk-seam mesh gap instead."
    );
}

/// Control: does the detector actually fire on a real, known hole **in this
/// uneven scene**, not just the flat one? Skips uploading the surface section
/// of the column forming the near riser's exposed face (`RING1_HALF`,
/// `LEVEL1_Y`'s own surface section — the vertical wall a viewer walking up
/// to it would see) and asserts the sandwiched-sky detector finds it.
///
/// Uses its own close-up camera (not the main gate's moderate-distance rig):
/// `distant_flat_terrain_holes.rs`'s own control already measured that a
/// victim at moderate distance can project to under one screen row and be
/// undetectable even when genuinely missing — this reproduces that lesson's
/// fix (a *close* victim) in the uneven scene rather than re-deriving it.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_deliberately_missing_riser_section_is_detected() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let stone = first_state_named("minecraft:stone");
    let air = first_state_named("minecraft:air");
    let world = ziggurat_world(stone, air);

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    // The ring1 riser's own surface section, on the column forming its
    // `-Z`-facing wall (`z = -RING1_HALF`, i.e. chunk `cz = -RING1_HALF/16`).
    // `LEVEL1_Y` (80) is an exact section boundary, so its surface section is
    // index `LEVEL1_Y/16 - 1`.
    let victim = SectionKey {
        cx: 0,
        cz: -RING1_HALF / 16,
        si: (LEVEL1_Y / 16 - 1) as usize,
        min_y: MIN_Y,
    };

    // Close-up: standing just short of the riser, looking straight at it.
    let camera = Camera {
        position: glam::Vec3::new(0.5, LEVEL0_Y as f32 + 1.62, -(RING1_HALF as f32) - 8.5),
        yaw: 0.0,
        pitch: 8.0,
        fov_y_degrees: FOV_Y_DEGREES,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS as u32, 0),
    };

    let (reference, _) = render_frame(
        device, queue, format, &mut target, &atlas, &world, models, &camera, None, false,
    );
    let (subject, stats) = render_frame(
        device, queue, format, &mut target, &atlas, &world, models, &camera, Some(victim), true,
    );

    let (hole_px, bbox, hole_cols) = find_sandwiched_background(&subject, &reference, W, H);
    eprintln!(
        "=== control: riser section {victim:?} never uploaded ===\n\
         sections drawn         = {}\n\
         sandwiched-sky pixels  = {hole_px}\n\
         sandwiched-sky columns = {hole_cols} / {W}\n\
         bounding box           = {bbox:?}",
        stats.sections_drawn,
    );
    assert!(
        hole_px > 0,
        "control failed to fire: a riser section was deliberately never uploaded to the GPU \
         and the sandwiched-sky detector found nothing in this uneven scene. Either the \
         victim section projects off-screen, or the detector cannot see a real hole here — \
         fix this before trusting a clean result from the main gate."
    );
}
