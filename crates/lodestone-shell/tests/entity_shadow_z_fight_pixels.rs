//! Pixel gate for the owner report *"the entity ground-shadow decal z-fights
//! with the ground"*, over **real terrain**, at **four distances**.
//!
//! # Why a second shadow gate exists at all
//!
//! `entity_shadow_pixels.rs` renders the shadow against **no world sections at
//! all** — its own module doc says so, and for its question (does the pass
//! reach pixels?) that is the right fixture, because the decal reads as fresh
//! non-sky pixels against a plain sky clear. It is also exactly why that gate
//! **structurally cannot see this defect**: z-fighting is a property of two
//! surfaces competing for one depth value, and in that harness there is no
//! second surface. The corpus had one blind spot and the single gate in it
//! inherited it.
//!
//! So this one builds a real grass world through the production path
//! (`snapshot_section` + `mesh_snapshot_models` + `SectionGeometry::Model`)
//! against the real vanilla atlas, the same way `ground_plate_z_fight_pixels.rs`
//! does, and stands a mob on it.
//!
//! # The detector, and why it needs no tolerance
//!
//! The subject frame and the control frame differ **only** by
//! `RenderState::set_entity_shadows_enabled` — same world, same upload, same
//! camera, same entity, same `RenderState`. Every pixel the shadow pass does
//! not touch is therefore **byte-identical** between them, so the shadow's
//! footprint is recovered exactly by `a != b`, with no threshold to fit and no
//! blend arithmetic to predict (`CLAUDE.md`: you cannot predict a composited
//! byte through `ALPHA_BLENDING` on this backend — this gate never tries).
//!
//! # What this gate asserts, and what it was actually observed to catch
//!
//! Two readings are taken from that footprint, and they are **not** equally
//! well evidenced. Saying so is the point of this section.
//!
//! * **The decal reaches pixels at every distance** — a real, non-trivial
//!   footprint at all four subjects. This is the load-bearing assertion, and
//!   its control was *run and watched failing*: inverting
//!   [`lodestone_render::entity_pipeline::SHADOW_DEPTH_BIAS`]'s sign pushes the
//!   decal behind the ground instead of in front of it, and the footprint
//!   collapses to **0 px at every one of the four distances** while
//!   `shadow_pieces_drawn` stays unchanged — the pass still builds its
//!   geometry and none of it survives the depth test. That is precisely the
//!   failure this pipeline's bias exists to prevent, and it is a failure the
//!   no-terrain gate next door cannot see, because a decal with nothing behind
//!   it has no depth test to lose.
//!
//! * **The footprint is solid rather than speckled** — [`MAX_RUNS_PER_ROW`].
//!   This one has **never been observed to fire**, and it is recorded here as
//!   an unproven guard rather than as evidence. Across twelve headless
//!   configurations (distance, world-coordinate magnitude, far plane, grazing
//!   angle, sub-block feet offsets) the coplanar decal did not speckle even
//!   with no bias at all: `LessEqual` lets a tie through and the decal draws
//!   after the terrain, so a tie resolves in its favour. Coplanar surfaces in
//!   this renderer flip **wholesale**, not per pixel — `ground_plate_z_fight_
//!   pixels.rs` measured the same thing for ground plates. Treat a failure
//!   here as a real finding about the rasteriser, not as a flake.
//!
//! # Distance is the axis, because the defect is precision-dependent
//!
//! A single close-up frame is precisely the gate that passes while the bug
//! ships: through this renderer's **forward** `[0,1]` depth, one ULP of the
//! depth buffer is worth `2.44e-06` blocks of world separation at 2 blocks and
//! `1.55e-04` blocks at 16 — a **64x** swing, because the worth of a ULP grows
//! as the square of the distance. (Measured for `near = 0.05`, `far =
//! far_for_render_distance(12)`, against reversed-Z's `5.96e-08`..`4.77e-07`
//! over the same span; see `EntityPipeline::shadow_pipeline`'s doc.) A shadow
//! piece only ever exists within 16 blocks of the camera — `pow = (1 -
//! distSq / 256) * strength` must be positive — so [`SUBJECTS`] spans that
//! whole reachable range rather than a comfortable corner of it.
//!
//! # Scope, stated plainly
//!
//! This proves the **draw**: real baked terrain, real mesher, real pipeline,
//! real atlas, real `prepare_shadows` ground scan over a real `World`. It
//! installs its own `EntityDraw` and its own `ShadowGroundSource`, so it says
//! nothing about how production builds either — the ECS extract step and the
//! wire are not exercised here. `CLAUDE.md`'s rule applies unchanged: a pixel
//! gate proves the draw and proves nothing past the edge of its own fixture.
//!
//! Fail-closed: no GPU adapter or no vanilla `client.jar` is a failure, never
//! a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test entity_shadow_z_fight_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::EntityDraw;
use lodestone::gpu::{RenderState, ThirdPersonBodyState};
use lodestone::mesher::{
    SectionGeometry, SectionKey, mesh_snapshot_models, snapshot_section, snapshot_visibility,
};
use lodestone::resources::BlockResources;
use lodestone_render::{
    AnimInput, Camera, GpuContext, HeadlessTarget, ModelMesh, RenderTarget,
};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};

const W: u32 = 512;
const H: u32 = 384;
const FOV_Y_DEGREES: f32 = 70.0;

/// Radius of the loaded/meshed world, in chunks. Small on purpose: every
/// subject stands within 16 blocks of the camera (see the module doc), so a
/// wider world would only cost meshing time.
const RD_CHUNKS: i32 = 2;
const MIN_Y: i32 = 0;
/// Ground fills `[MIN_Y, SURFACE_Y)`, so the surface's top face — the plane
/// the decal has to win against — is at exactly `SURFACE_Y`.
const SURFACE_Y: i32 = 64;
const SECTION_COUNT: usize = 6;

/// How high above the ground surface the camera eye sits, in blocks. Well
/// above a standing player's 1.62 so that a subject at the far end of
/// [`SUBJECTS`] is still looked *down* at rather than seen edge-on — a
/// flat decal viewed edge-on covers no pixels and the gate would measure
/// nothing.
const EYE_ABOVE_SURFACE: f32 = 5.0;

/// Horizontal distances from the camera to the subject's feet, in blocks.
///
/// Chosen so the **true** camera-to-shadow distances (`hypot(d,
/// EYE_ABOVE_SURFACE)`) spread across the range a shadow can physically
/// occupy: `5.4`, `7.8`, `10.3`, `13.0` blocks against the hard 16-block
/// cutoff. Not round numbers relative to anything the code divides by.
const SUBJECTS: &[f32] = &[2.0, 6.0, 9.0, 12.0];

/// Maximum runs of footprint pixels a single scanline may contain.
///
/// A solid decal gives 1, or 2 where the mob's own silhouette splits the
/// footprint. A z-fighting decal gives tens — half the pixels lose the depth
/// test at random, so the count runs toward a quarter of the footprint's
/// width. This sits between the two predictions rather than beside either.
const MAX_RUNS_PER_ROW: usize = 4;

/// Minimum footprint, in pixels, before a reading is believed. A gate that
/// measured three pixels would report a perfect run count for a decal that
/// never drew — the *vacuity* guard, not a quality bar.
const MIN_FOOTPRINT_PX: usize = 40;

/// Suppress `RenderState`'s unconditional first-person bare arm, which
/// otherwise paints a fixed screen rect in every frame. It would be identical
/// in subject and control and so invisible to the `a != b` footprint, but the
/// mob is placed against it here and a documented false-positive source is
/// cheaper to remove than to reason about (`CLAUDE.md`).
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

/// Solid grass up to `SURFACE_Y`, air above.
fn flat_world(ground: u32, air: u32) -> World {
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
                // resolves to 0 — everything renders black and the detector
                // measures nothing. See `distant_flat_terrain_holes.rs`.
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
    let written = world.fill_region([lo, MIN_Y, lo], [hi, SURFACE_Y - 1, hi], ground);
    assert!(written > 0, "fixture: ground must actually be written");
    world
}

/// Past the `0.75`s section fade-in — without this every section renders as
/// pure fog colour. See `distant_flat_terrain_holes.rs`'s own note.
const FADE_COMPLETE_TICK: u64 = 200;

/// A `RenderState` with the whole world meshed and uploaded through the
/// production path, and the shadow ground sampler installed over that same
/// world — the half `entity_shadow_pixels.rs` fakes with a `y < 0` predicate.
fn build_scene(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    atlas: &lodestone_render::BlockAtlas,
    world: &World,
    models: &lodestone_render::BlockModels,
) -> RenderState {
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas));
    suppress_first_person_arm(&mut state);
    let mut uploaded = 0usize;
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
    assert!(uploaded > 0, "fixture: some sections must have uploaded");

    // The same question production asks (`NetClient::block_at`), answered from
    // the very world that was just meshed — so the decal is scanned against
    // the geometry it has to win the depth test against, not against a
    // synthetic predicate that happens to agree.
    let ground = state_id("minecraft:grass_block[snowy=false]");
    state.set_shadow_ground_source(move |[_, y, _]| {
        if (MIN_Y..SURFACE_Y).contains(&y) { Some(ground) } else { None }
    });

    state.update_animation(queue, FADE_COMPLETE_TICK);
    state
}

/// A mob standing on the surface `distance` blocks in front of the camera.
fn subject_at(distance: f32) -> EntityDraw {
    EntityDraw {
        hurt: false,
        block_state: None,
        item_frame_rotation: 0,
        id: 1,
        type_path: std::sync::Arc::from("zombie"),
        item: None,
        main_arm_left: false,
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_trim: Vec::new(),
        feet: glam::Vec3::new(0.5, SURFACE_Y as f32, 0.5 + distance),
        yaw: 0.0,
        head_yaw: 0.0,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput::REST,
        wool: None,
        count: 1,
        foil: false,
        item_dyed_color: None,
        item_potion_color: None,
        name_tag: None,
        item_use: None,
        creeper_swelling: 0.0,
        swim_amount: 0.0,
        death_time: 0.0,
        on_fire: false,
        invisible: false,
        armor_stand: None,
        player_skin: None,
        variant_sheet: None,
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
        painting: None,
    }
}

/// A camera looking down at the subject's feet from [`EYE_ABOVE_SURFACE`].
/// The pitch is **derived** from the geometry rather than chosen per subject,
/// so every distance is framed the same way and any difference between the
/// readings is the distance rather than the shot.
fn camera_for(distance: f32) -> Camera {
    Camera {
        position: glam::Vec3::new(0.5, SURFACE_Y as f32 + EYE_ABOVE_SURFACE, 0.5),
        yaw: 0.0,
        pitch: (EYE_ABOVE_SURFACE / distance).atan().to_degrees(),
        fov_y_degrees: FOV_Y_DEGREES,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(12, 0),
    }
}

/// The shadow's exact screen footprint: the pixels that differ at all between
/// the two frames. Nothing but the shadow pass can move a byte here, so this
/// needs no tolerance — see the module doc.
fn footprint(subject: &[u8], control: &[u8]) -> Vec<bool> {
    subject
        .chunks_exact(4)
        .zip(control.chunks_exact(4))
        .map(|(a, b)| a[..3] != b[..3])
        .collect()
}

/// What a footprint looks like: how many pixels, where, and how fragmented.
struct Shape {
    px: usize,
    bbox: (usize, usize, usize, usize),
    max_runs_per_row: usize,
    worst_row: usize,
    total_runs: usize,
    rows_touched: usize,
}

fn shape_of(mask: &[bool]) -> Shape {
    let (w, h) = (W as usize, H as usize);
    let (mut x0, mut y0, mut x1, mut y1) = (usize::MAX, usize::MAX, 0usize, 0usize);
    let (mut px, mut total_runs, mut rows_touched) = (0usize, 0usize, 0usize);
    let (mut max_runs_per_row, mut worst_row) = (0usize, 0usize);

    for y in 0..h {
        let mut runs = 0usize;
        let mut prev = false;
        for x in 0..w {
            let on = mask[y * w + x];
            if on {
                px += 1;
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
                if !prev {
                    runs += 1;
                }
            }
            prev = on;
        }
        if runs > 0 {
            rows_touched += 1;
            total_runs += runs;
            if runs > max_runs_per_row {
                max_runs_per_row = runs;
                worst_row = y;
            }
        }
    }
    Shape {
        px,
        bbox: if px == 0 { (0, 0, 0, 0) } else { (x0, y0, x1, y1) },
        max_runs_per_row,
        worst_row,
        total_runs,
        rows_touched,
    }
}

/// The gate. Collects every distance's verdict and asserts on the collection —
/// an `assert!` inside the loop would prove exactly one arm and leave the rest
/// as arguments rather than observations (`CLAUDE.md`).
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn the_ground_shadow_decal_wins_the_depth_test_at_every_distance() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "vanilla assets did not load (banner: {:?}) — this gate needs a real \
             client.jar under .cache/mc/26.2",
            resources.banner
        )
    });
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let world = flat_world(
        state_id("minecraft:grass_block[snowy=false]"),
        state_id("minecraft:air"),
    );
    let mut state = build_scene(device, queue, format, atlas.as_ref(), &world, models);

    eprintln!("=== entity ground-shadow z-fight gate (real terrain) ===");
    let mut failures: Vec<String> = Vec::new();

    for &distance in SUBJECTS {
        let camera = camera_for(distance);
        let draw = subject_at(distance);
        let true_distance = camera.position.distance(draw.feet);

        let mut shoot = |state: &RenderState| -> (Vec<u8>, usize) {
            let frame = target.acquire().expect("headless acquire");
            let stats = state.render(
                device,
                queue,
                frame.view(),
                &camera,
                None,
                std::slice::from_ref(&draw),
            );
            (target.read_texels(device, queue), stats.shadow_pieces_drawn)
        };

        state.set_entity_shadows_enabled(true);
        let (subject_px, pieces) = shoot(&state);
        state.set_entity_shadows_enabled(false);
        let (control_px, control_pieces) = shoot(&state);

        let s = shape_of(&footprint(&subject_px, &control_px));
        let mean = if s.rows_touched == 0 {
            0.0
        } else {
            s.total_runs as f64 / s.rows_touched as f64
        };
        eprintln!(
            "  d={distance:>4.1}b (true {true_distance:>4.1}b)  pieces={pieces:>2} \
             footprint={:>6}px  bbox=({},{})-({},{})  runs/row max={} (row {}) mean={mean:.2}",
            s.px, s.bbox.0, s.bbox.1, s.bbox.2, s.bbox.3, s.max_runs_per_row, s.worst_row,
        );

        if control_pieces != 0 {
            failures.push(format!(
                "d={distance}: the control has entityShadows off but drew \
                 {control_pieces} shadow pieces"
            ));
        }
        if pieces == 0 {
            failures.push(format!(
                "d={distance}: shadow_pieces_drawn=0 — the ground scan found no ground, \
                 so this reading is about the fixture and not about depth"
            ));
            continue;
        }
        if s.px < MIN_FOOTPRINT_PX {
            failures.push(format!(
                "d={distance} (true {true_distance:.1}b): the decal covered only {}px \
                 (floor {MIN_FOOTPRINT_PX}) while the pass built {pieces} pieces — the \
                 geometry exists and is not surviving the depth test against the coplanar \
                 ground face. Check `EntityPipeline::SHADOW_DEPTH_BIAS`: a bias of the \
                 wrong sign pushes the decal *behind* the ground and collapses the \
                 footprint to exactly this.",
                s.px
            ));
            continue;
        }
        if s.max_runs_per_row > MAX_RUNS_PER_ROW {
            failures.push(format!(
                "d={distance} (true {true_distance:.1}b): the decal's footprint is \
                 fragmented — row {} carries {} separate runs (budget {MAX_RUNS_PER_ROW}, \
                 mean {mean:.2} across {} rows), footprint {}px in bbox \
                 ({},{})-({},{}). That is the z-fighting signature: the depth comparison \
                 against the coplanar ground face is being decided by rasteriser rounding \
                 rather than by the pipeline's depth bias.",
                s.worst_row, s.max_runs_per_row, s.rows_touched, s.px,
                s.bbox.0, s.bbox.1, s.bbox.2, s.bbox.3,
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "the ground-shadow decal does not cleanly win the depth test:\n  - {}",
        failures.join("\n  - ")
    );
}

/// The sign of the ground shadow's polygon offset, asserted without a GPU.
///
/// `[0, 1]` depth means **lower is nearer**, so pulling a decal toward the
/// camera is a *negative* bias — the opposite of vanilla's reversed-Z, where
/// the same intent is spelled positive. Getting this backwards does not soften
/// the artefact, it inverts the fix: the decal is pushed behind the ground it
/// sits on and disappears altogether. That failure mode reads exactly like
/// "entity shadows were never wired", which is the report this feature was
/// built to close, so it is worth a gate that runs in every plain
/// `cargo test` rather than only behind `--ignored`.
///
/// The magnitudes are deliberately *not* pinned here — see
/// [`EntityPipeline::shadow_pipeline`]'s doc for why ten ULPs is the right
/// order and why the number cannot be re-derived from a world-space distance.
///
/// [`EntityPipeline::shadow_pipeline`]: lodestone_render::entity_pipeline::EntityPipeline::shadow_pipeline
#[test]
fn the_ground_shadow_bias_pulls_toward_the_camera() {
    let bias = lodestone_render::entity_pipeline::SHADOW_DEPTH_BIAS;
    assert!(
        bias.constant < 0,
        "SHADOW_DEPTH_BIAS.constant is {}; under this renderer's [0,1] depth a decal is \
         pulled toward the camera by a NEGATIVE constant. A positive one pushes the ground \
         shadow behind the ground and it vanishes.",
        bias.constant
    );
    assert!(
        bias.slope_scale < 0.0,
        "SHADOW_DEPTH_BIAS.slope_scale is {}; it must share the constant's sign, or a decal \
         seen at a grazing angle is pushed back exactly where the depth gradient across a \
         pixel is largest and the pull is needed most.",
        bias.slope_scale
    );
}
