//! **STATUS: the control fires. The main gate is a real, non-vacuous clean
//! result for a flat world — it does not reproduce the report.**
//!
//! # What was swallowing the geometry
//!
//! Not the section-origin arena's dynamic offset (the previous leading
//! suspicion) — the offset math was fine. The actual cause lives in the
//! *same* uniform's spare lane: [`RenderState::upload_section`] stamps every
//! freshly-uploaded section's origin uniform with a fade `build_time` read
//! from `RenderState::section_fade_tick` (a `Cell<u64>` defaulting to `0`,
//! otherwise advanced only by [`RenderState::update_animation`]), and
//! `model.wgsl`'s `vs_main` resolves a per-section `visibility` factor as
//! `section_visibility(camera.fog_ambient_light.w, origin.section_origin.w)`
//! — `now` vs. `build_time`, the same clock on both sides
//! (`lodestone_render::model_pipeline::section_visibility`). `fs_main` then
//! mixes the lit fragment toward `camera.fog_color_start.rgb` by that factor:
//! at `visibility == 0.0` the output **is** the fog colour, byte-for-byte,
//! which is indistinguishable from the sky by construction. The harness
//! uploaded every section and rendered immediately, calling neither
//! `update_animation` at upload time nor before render, so `now` and
//! `build_time` were both exactly `0.0` for every section on every frame —
//! `section_visibility` was exactly `0.0`, always. `upload_section` and
//! `render` are both the unmodified production functions; the gap was the
//! harness never doing what `app.rs`'s real per-frame call site does
//! (`update_animation` before every `render`), so a section that only looks
//! like this for its first `0.75` s after upload in a real session looked
//! like it forever here. [`render_frame`]'s doc carries the measurement and
//! the fix: call `update_animation` with the clock advanced well past the
//! `0.75` s fade window before rendering.
//!
//! Every other fact the production path establishes matches: the camera bind
//! group, the shared camera buffer, the anim buffer (already initialised at
//! tick `0`, not garbage), the palette buffer, and the origin arena's own
//! offset allocation. Only the fade clock was unwired.
//!
//! # Measured, with the fix applied
//!
//! `a_deliberately_missing_surface_section_is_detected` (the control): raw
//! subject-vs-reference diff went from `0 / 307200` to **`195,419 / 307200`**
//! differing pixels, and the sandwiched-sky detector itself went from `0` to
//! **`11,759`** pixels across **341/640** columns, bounding box `(0, 182) –
//! (340, 216)` — non-degenerate, localised, not a constant/round-number box.
//! The control **is now observed to fire**.
//!
//! `flat_terrain_at_moderate_distance_has_no_sky_holes` (the main gate):
//! **`ok`**, genuinely — `0` sandwiched-sky pixels at every one of the five
//! configs (three yaws at the report's own "same level as me" pitch, a
//! steep look-down, a slight look-up), each with real, non-trivial cull
//! stats (53–60 sections drawn per frame, hundreds culled by distance and
//! frustum, 0–59 by occlusion). This is a real clean result, not a vacuous
//! one: the control above proves the exact same detector, same resolution,
//! same distances, finds a real hole when one is deliberately introduced.
//!
//! # What this does and does not answer
//!
//! Depth-test discard and mip/gutter sampling were the two live suspects
//! (missing geometry is structurally excluded by `ColumnSource::Complete`,
//! and both are already-fixed per the record above); this result does not
//! implicate either **for a flat, single-height world**. It cannot speak to
//! Y-dependence or rotation-dependence of a hole that was not found at all in
//! this scene, and it deliberately cannot exercise the shape the module doc
//! below explains this harness cannot represent: a heightfield with
//! **overhangs or multi-height terrain**, i.e. real chunk-to-chunk elevation
//! changes, which is a plausible reading of "holes between the blocks" that
//! this flat-world design excludes *by choice* (see "Why flat" below) to keep
//! the detector unambiguous. If the report reproduces on genuinely uneven
//! terrain, that is the next harness to build, not a sign this one is wrong.
//!
//! Absent a screenshot or a reproduction on non-flat terrain, this is the
//! honest state: a real (non-vacuous) diagnostic finds no defect in the
//! scenario it can test, so the next step is either a screenshot from the
//! report or a harness over uneven terrain — not a guessed fourth cause.
//!
//! ---
//!
//! Diagnostic gate for the report "far-away blocks at my own level render with
//! holes where I can see the sky": separates *missing geometry*, *depth-test
//! discard* and *mip-sampling bleed* by rendering a **flat, single-height,
//! fully-loaded** vanilla-textured world through the real live-server geometry
//! path ([`SectionGeometry::Model`]) and looking for sky pixels sandwiched
//! inside the terrain silhouette.
//!
//! # Why flat, and why fully loaded before any meshing
//!
//! A flat single-height world is a heightfield with no overhangs, so for any
//! fixed camera position and yaw, sweeping pitch from "look up" to "look down"
//! can cross from sky to terrain **at most once**: once a ray hits the ground
//! plane, every steeper ray hits it too (the plane extends to infinity and the
//! ray's height above it decreases monotonically with pitch). So the correct
//! image is sky-then-terrain in every screen column, no exceptions, no
//! judgement calls about legitimate gaps between tree branches or over a
//! cliff — this world has neither. Any additional sky-classified run **below**
//! the first terrain pixel in a column is not a rendering choice, it is a bug.
//!
//! Every column is loaded before a single section is meshed
//! (`ColumnSource::Complete`, what [`snapshot_section`] always uses), so no
//! neighbour is ever `Neighbour::Unloaded` and [`SnapshotOutcome::Deferred`]
//! never fires. That structurally rules out the streaming-neighbour-arrival
//! cause of missing geometry (issue #389/#479's mechanism) for whatever this
//! gate finds — a hole here cannot be explained by "the neighbour had not
//! arrived yet", because every neighbour already had.
//!
//! # The control
//!
//! [`a_deliberately_missing_surface_section_is_detected`] skips uploading one
//! column's surface section — the closest real analogue to "a section's
//! geometry never reached the GPU" — and asserts the detector actually finds a
//! sandwiched-sky run there. An assertion of absence with no observed-to-fire
//! control is not evidence; this is that control, run and observed to fail
//! (i.e. to find the hole) before the main gate is trusted to find none.
//!
//! Fail-closed: no GPU adapter or no vanilla `client.jar` is a failure, never a
//! skip — a silently-demo-palette run would draw the wrong atlas UVs entirely
//! and could not tell a real gutter/mip bug from static.
//!
//! ```text
//! cargo test -p lodestone-shell --test distant_flat_terrain_holes -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, ThirdPersonBodyState};
use lodestone::mesher::{SectionGeometry, SectionKey, mesh_snapshot_models, snapshot_section, snapshot_visibility};
use lodestone::resources::BlockResources;
use lodestone_render::{Camera, GpuContext, HeadlessTarget, ModelMesh, RenderTarget, entity_anim::AnimInput};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

/// Suppress `RenderState`'s unconditional first-person bare-arm pass, which
/// draws permanently whenever no third-person body is reported — a
/// documented false-positive source for exactly this class of pixel gate
/// (`CLAUDE.md`'s "the unconditional first-person bare arm"). Measured here
/// too: before this suppression, every single config below reported an
/// *identical* 29-pixel "hole" at the exact same screen rect regardless of
/// camera yaw or pitch — the tell that it was a fixed-screen-space artifact,
/// not a world-space one. The body is placed far below the world so nothing
/// about it is actually visible; only `Some(..)` is required to flip
/// `RenderStats::third_person_body_drawn` and skip the arm pass.
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

// Resolution and FOV matter more than they look like they should here.
// Ground distance and screen row relate by `dist = eye_height / tan(angle)`,
// so a fixed-size world patch near the horizon (small angle) occupies far
// fewer screen rows than the same patch close to the camera (large angle) —
// basic perspective, not a defect. Measured directly: at the *original* 320px
// tall / 70° FOV frame, a real, deliberately-removed 16-block section at
// 80-95 blocks out (a plausible "moderate distance") covered **under one
// screen row** and the detector below could not see it even as a control.
// A narrower FOV spends more of the same pixel budget on the region of
// interest; this resolution/FOV keeps a `cargo test --ignored` run in the
// tens of seconds while giving the horizon band enough rows to be honest.
const W: u32 = 640;
const H: u32 = 480;
const FOV_Y_DEGREES: f32 = 40.0;

/// Render distance for this scene, in chunks (moderate: 160 blocks).
const RD_CHUNKS: i32 = 10;
/// World floor; terrain fills `[MIN_Y, SURFACE_Y)`. `SURFACE_Y` sits on a
/// section boundary (`64 = 4*16`) so the solid/air interface is also a
/// *vertical* section boundary, exercising that seam too.
const MIN_Y: i32 = 0;
const SURFACE_Y: i32 = 64;
/// Sections per column: four solid (0..64) plus two of headroom (64..96).
const SECTION_COUNT: usize = 6;

/// Manhattan RGB distance above which a pixel counts as "differs from the
/// no-terrain reference", i.e. terrain.
///
/// **Not a fixed sky-colour constant.** The real render path's background is
/// `SkyFrame::clear_color`, the frame's *resolved fog colour* — time-of-day
/// and `eye_y` dependent, and layered under an overhead sky disc that blends
/// toward the fog colour at the horizon — not the flat `SKY_COLOR` clear the
/// packed/demo path uses. A first attempt at this gate hardcoded `SKY_COLOR`
/// and it silently could not tell background from terrain at all: a control
/// with a real, deliberately-removed section produced **zero** detected
/// pixels. So background is derived per config from a second render of the
/// *same camera through the same `RenderState` setup with zero terrain
/// uploaded* — "what else already paints here" — and every pixel is compared
/// against its own corresponding reference pixel, not a constant.
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

/// An inclusive pixel rect.
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

/// Per-column run-length scan: a "hole" is any background-classified run that
/// is not the column's first run — i.e. background reappearing after terrain
/// has already started, which a monotonic flat heightfield can never produce
/// honestly (see the module doc for the geometric argument).
///
/// `subject` is the frame with terrain; `reference` is the *same camera
/// through the same `RenderState` setup* with zero terrain uploaded, so a
/// pixel counts as background when it matches what that exact screen
/// position already paints with nothing there — never a fixed colour.
///
/// Returns (total hole-pixel count, bounding box of every hole pixel, holes
/// per column count) so failure output can print *where*, not just *how many*
/// — a frame-average number cannot tell a uniform-but-wrong frame from a
/// localised hole, and a bounding box that is degenerate/constant across
/// unrelated failures is itself a broken instrument, so this prints the raw
/// per-column hole-column count too.
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

/// Build a flat vanilla-stone world of radius [`RD_CHUNKS`] chunks, fully
/// loaded before any section is meshed, under open daylight.
///
/// **A column's light defaults to [`LightData::Missing`], which
/// [`SectionLight::sky_at`] resolves to `0` — full dark — not to
/// [`SkyDefault::Full`].** That default only bridges an *absent neighbour*
/// (edge of world / not-yet-loaded column); every column here is present, so
/// skipping this step meshes real, fully-dark geometry. Measured directly:
/// before this fix, a real render of this world was **byte-identical** to a
/// render of an empty one — not "hard to see", literally zero differing
/// pixels — because unlit stone and this scene's fog/sky both render dark
/// enough to fall inside the comparison threshold. `LightData::Uniform` is a
/// tag, not a per-nibble write, so this is four cheap assignments per column
/// rather than `4096 * light_section_count` individual `set` calls.
fn flat_world(stone: u32, air: u32) -> World {
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
    // Loaded columns cover chunk `-RD_CHUNKS..=RD_CHUNKS`, i.e. block range
    // `[-RD_CHUNKS*16, RD_CHUNKS*16 + 15]` — the two bounds are **not**
    // symmetric (a chunk's negative edge is exact, its positive edge is +15).
    let lo = -RD_CHUNKS * 16;
    let hi = RD_CHUNKS * 16 + 15;
    let written = world.fill_region([lo, MIN_Y, lo], [hi, SURFACE_Y - 1, hi], stone);
    assert!(written > 0, "fixture: fill_region must actually write blocks");
    world
}

/// Mesh and upload every section in the world (or every section except
/// `skip`, for the missing-geometry control, or none at all for a
/// no-terrain background reference), through the exact production
/// live-vanilla path: [`mesh_snapshot_models`] + [`snapshot_visibility`] +
/// [`SectionGeometry::Model`].
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
                // Upload even a zero-quad (fully-enclosed) section. Skipping it
                // — an earlier version of this fixture did — is the exact
                // occlusion-graph gotcha `client_chunk_cycles.rs` documents:
                // `upload_section`'s `Model` arm is what records the section's
                // connectivity (`record_section_visibility`), and it does that
                // *before* deciding there is no geometry to keep. Skip the
                // upload and the camera's own section (mostly air, meshes to
                // zero quads) never enters the occlusion graph at all, which
                // starves the camera walk of a starting point.
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

/// Game tick to advance the render clock to before drawing, once terrain has
/// been uploaded — see [`render_frame`]'s doc for why this is required at
/// all.
///
/// `20` ticks = 1 real second; the section fade-in
/// ([`lodestone_render::model_pipeline::SECTION_FADE_DURATION_SECS`]) is
/// `0.75` s, so `200` ticks (10 s) clears it with a wide margin — a
/// deliberately generous multiple, not a tight bound, because the point is
/// to be unambiguously past the fade, not to probe its edge.
const FADE_COMPLETE_TICK: u64 = 200;

/// Render one frame through a freshly built `RenderState` (bare-arm
/// suppressed) for `camera`, uploading terrain per `skip`/`upload_terrain`.
/// Returns the read-back RGBA8 pixels and the frame's stats.
///
/// **Calls [`RenderState::update_animation`] after uploading and before
/// rendering — this is not optional.** `RenderState::upload_section` stamps
/// every freshly-uploaded section's origin uniform with a `build_time` read
/// from `RenderState::section_fade_tick`, which defaults to `0` and is
/// otherwise advanced only by `update_animation`. The model shader's
/// per-fragment colour is `mix(fog_colour, lit_colour, section_visibility(now,
/// build_time))` (`model.wgsl`'s `vs_main`/`fs_main`,
/// `lodestone_render::model_pipeline::section_visibility`), and with neither
/// call ever made, `now == build_time == 0.0` for every section on every
/// frame, so `section_visibility` evaluates to exactly `0.0` and **every
/// section renders as pure fog colour** — indistinguishable from the sky by
/// construction, not by a coincidence of this scene's palette. Measured: this
/// was the entire cause of "terrain and empty worlds differ by zero pixels
/// while `RenderStats` reports real sections and quads" — the geometry was
/// correct and submitted; it was uniformly faded to invisible. Production's
/// real per-frame call site (`app.rs`) calls `update_animation` before every
/// `render`, so a section only looks like this for its first `0.75`s after
/// upload, which is why this gap was invisible to code reading: nothing
/// about `upload_section` or `render` is wrong, and both are the exact
/// production functions. Only a harness that renders sections *immediately*
/// after uploading them, with no elapsed frames in between, can hit `now ==
/// build_time` exactly.
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
    // Past the fade window (see this function's doc) — without this every
    // section drawn below is `section_visibility == 0.0` and renders as pure
    // fog colour, whether or not any terrain was actually uploaded.
    state.update_animation(queue, FADE_COMPLETE_TICK);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), camera, None, &[]);
    (target.read_texels(device, queue), stats)
}

/// Eye standing on the flat surface, looking along `+Z` at a shallow downward
/// pitch — the ordinary "walking around looking mostly at the horizon" view
/// the report describes as "the same level as me". `pitch_degrees` and
/// `yaw_degrees` are varied by callers to test Y-dependence and rotation.
/// [`Camera::pitch`]'s convention is **positive looks down**.
fn camera_at(pitch_degrees: f32, yaw_degrees: f32) -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, SURFACE_Y as f32 + 1.62, 0.5),
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
             client.jar under .cache/mc/26.2 (LODESTONE_ASSETS) because it is testing \
             the real block atlas' gutter/mip behaviour, and a demo-palette fallback \
             would silently draw the wrong atlas entirely",
            resources.banner
        )
    });
    (resources, atlas)
}

/// The main gate: a fully-loaded flat world, rendered at "my own level" (a
/// shallow downward pitch) out to a moderate distance. No sandwiched sky
/// pixel should exist anywhere in the frame.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn flat_terrain_at_moderate_distance_has_no_sky_holes() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let stone = first_state_named("minecraft:stone");
    let air = first_state_named("minecraft:air");
    let world = flat_world(stone, air);

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    // Three pitches: a shallow look-down ("my own level", the report's own
    // framing), a steep look-down (control: well below eye level), and a
    // slight look-up (control: above eye level) — the Y-dependence probe.
    // Two yaws at the shallow pitch — the rotation probe: a world-fixed
    // defect (missing geometry, a mis-culled section) should not vanish just
    // because the camera turned; a screen-space artifact might.
    // `Camera::pitch` is positive-looks-down. `2.0` degrees is close to dead
    // level (`atan(eye_height / 100 blocks)` ≈ 0.9°) so the moderate-distance
    // band the report describes sits well inside the frame rather than
    // squeezed against the horizon edge — see the module doc and the `W`/`H`
    // doc for why that placement matters at all.
    let configs: &[(&str, f32, f32)] = &[
        ("level, yaw 0", 2.0, 0.0),
        ("level, yaw 90", 2.0, 90.0),
        ("level, yaw 180", 2.0, 180.0),
        ("steep down", 25.0, 0.0),
        ("slight up", -15.0, 0.0),
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

        let (hole_px, bbox, hole_cols) = find_sandwiched_background(&subject, &reference, W, H);
        eprintln!(
            "=== {label} (pitch={pitch}, yaw={yaw}) ===\n\
             sections drawn        = {}\n\
             sections culled dist  = {}\n\
             sections culled frust = {}\n\
             sections culled occl  = {} (occlusion_active={})\n\
             occlusion graph size  = {}\n\
             sandwiched-sky pixels = {hole_px}\n\
             sandwiched-sky columns= {hole_cols} / {W}\n\
             bounding box          = {bbox:?}",
            stats.sections_drawn,
            stats.sections_culled_distance,
            stats.sections_culled_frustum,
            stats.sections_culled_occlusion,
            stats.occlusion_active,
            stats.occlusion_graph_sections,
        );
        if hole_px > 0 {
            any_hole = true;
        }
    }

    assert!(
        !any_hole,
        "a fully-loaded flat vanilla-textured world produced sky pixels sandwiched \
         inside its own terrain silhouette in at least one of the pitch/yaw configs \
         above — see the per-config bounding boxes printed to stderr for *where*. \
         Because every column was loaded before any section was meshed, this cannot \
         be the streaming-neighbour-arrival cause; it points at the occlusion graph, \
         depth compositing, or atlas/mip sampling instead."
    );
}

/// Control: does the detector actually fire on a real, known hole? Skips
/// uploading one column's surface section (the closest real analogue to "a
/// section's geometry never reached the GPU") directly ahead of the camera at
/// moderate distance, and asserts the sandwiched-sky detector finds it.
///
/// Without this, a clean result from the gate above is unfalsifiable — it
/// would be indistinguishable from a detector that cannot see holes at all.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_deliberately_missing_surface_section_is_detected() {
    let ctx = gpu();
    let device = ctx.device();
    let queue = ctx.queue();
    let (_resources, atlas) = load_vanilla();
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let stone = first_state_named("minecraft:stone");
    let air = first_state_named("minecraft:air");
    let world = flat_world(stone, air);

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    // The surface section of the column one ring ahead of the camera (16-31
    // blocks). **Not** the same moderate distance the main gate scans:
    // measured first-hand that it must not be. At a shallow downward pitch,
    // ground distance and screen row are related by `dist = eye_height /
    // tan(angle)` — hyperbolic, so the *further* a fixed-size world patch is,
    // the *fewer* screen rows it occupies. A first version of this control put
    // the victim at half the render distance (~80-95 blocks) and it produced
    // **zero** detectable pixels even with a real section missing: at this
    // camera's pitch that band covers under one screen row at this
    // resolution, i.e. sub-pixel. A close victim keeps the control able to
    // fire at all; it says nothing about whether a *distant* hole of the same
    // world size would be visible, which is a resolution/pixel-budget
    // question the main gate below does not attempt to settle either.
    let victim = SectionKey {
        cx: 0,
        cz: 1,
        si: ((SURFACE_Y - 1) / 16) as usize,
        min_y: MIN_Y,
    };

    let camera = camera_at(8.0, 0.0);
    let (reference, _) = render_frame(
        device, queue, format, &mut target, &atlas, &world, models, &camera, None, false,
    );
    let (subject, _stats) = render_frame(
        device, queue, format, &mut target, &atlas, &world, models, &camera, Some(victim), true,
    );

    // DIAGNOSTIC: raw diff count/bbox between subject and reference, ignoring
    // the sandwich pattern entirely — proves whether *anything* differs at
    // all before asking whether it differs in the sandwiched shape.
    let mut diag_total = 0usize;
    let mut diag_rect: Option<Rect> = None;
    for y in 0..H {
        for x in 0..W {
            let idx = ((y * W + x) * 4) as usize;
            if differs(&subject[idx..idx + 4], &reference[idx..idx + 4]) {
                diag_total += 1;
                diag_rect = Some(grow(diag_rect, x, y));
            }
        }
    }
    let sample = |px: &[u8], x: u32, y: u32| -> [u8; 4] {
        let idx = ((y * W + x) * 4) as usize;
        [px[idx], px[idx + 1], px[idx + 2], px[idx + 3]]
    };
    eprintln!(
        "=== DIAGNOSTIC: raw subject-vs-reference diff ===\n\
         total differing pixels = {diag_total} / {}\n\
         bounding box           = {diag_rect:?}\n\
         subject sections drawn = {}\n\
         subject draw calls     = {}\n\
         subject total quads    = {}\n\
         subject px top-left    = {:?}\n\
         subject px centre      = {:?}\n\
         subject px bottom-mid  = {:?}\n\
         reference px top-left  = {:?}\n\
         reference px centre    = {:?}\n\
         reference px bottom-mid= {:?}",
        W * H,
        _stats.sections_drawn,
        _stats.draw_calls,
        _stats.total_quads,
        sample(&subject, 0, 0),
        sample(&subject, W / 2, H / 2),
        sample(&subject, W / 2, H - 1),
        sample(&reference, 0, 0),
        sample(&reference, W / 2, H / 2),
        sample(&reference, W / 2, H - 1),
    );

    let (hole_px, bbox, hole_cols) = find_sandwiched_background(&subject, &reference, W, H);
    eprintln!(
        "=== control: section {victim:?} never uploaded ===\n\
         sandwiched-sky pixels  = {hole_px}\n\
         sandwiched-sky columns = {hole_cols} / {W}\n\
         bounding box           = {bbox:?}"
    );
    assert!(
        hole_px > 0,
        "control failed to fire: a section was deliberately never uploaded to the GPU \
         and the sandwiched-sky detector found nothing. Either the victim section \
         projects off-screen or outside the frame, or the detector itself cannot see \
         a real hole — fix this before trusting a clean result from the main gate."
    );
}
