//! A **temporal** pixel gate for the report *"some blocks are popping in and
//! out weirdly, like z-fighting-ish — for example, the leaves on the ground"*.
//!
//! # Why a sweep and not a frame
//!
//! `ground_plate_z_fight_pixels` ruled out depth: the family's real offsets
//! resolve throughout the render distance, and a coplanar plate does not
//! speckle in this renderer, it flips wholesale. What is left is the other
//! reading of "popping": a **cutout** surface under minification. `model.wgsl`
//! discards at `tex.a < 0.5`, so once a leaf-litter plate is small enough on
//! screen that the sampler is filtering — across texels within a mip level and
//! across two mip levels — the *filtered* alpha becomes a visibility decision,
//! and the set of surviving texels churns as the camera moves.
//!
//! That is invisible to any single frame: one static frame of a shimmering
//! surface looks exactly like one static frame of a stable one. So this file
//! measures a **sweep**. The ground is a periodic tiling of one block, so
//! translating the camera by a fraction of a block leaves every aggregate
//! property of the image unchanged *in expectation* — how much of the frame
//! the plate paints at a given distance does not depend on where between two
//! block corners the eye happens to be. A renderer that is stable under
//! minification therefore holds that number nearly constant across the sweep;
//! one that is aliasing wobbles it frame to frame, and the wobble **is** the
//! reported flicker.
//!
//! # The statistic
//!
//! For each camera in the sweep the plated world is diffed against the
//! identical world with **no plate**, giving a per-pixel "the plate painted
//! here" mask, and that mask is counted per horizontal band. A band at a
//! shallow pitch is a distance band, so the far bands are the minified ones.
//! Per band the gate reports `mean`, and `jitter` = mean absolute change
//! between adjacent sweep steps, as a fraction of the mean. Jitter is the
//! number that matters; the mean is there so a band that painted nothing at
//! all cannot read as perfectly stable.
//!
//! # Controls
//!
//! [`a_mipless_atlas_flickers`] rebuilds the same corpus at `mipmapLevels = 0`
//! and runs the identical sweep. With no mip chain a minified cutout aliases
//! as hard as this renderer can make it alias, so that run must show
//! materially more jitter in the far bands than the shipped atlas does. Read
//! it before believing any clean number below it: without it, "the far bands
//! are stable" is equally consistent with a detector that measures nothing.
//!
//! # What this measured
//!
//! Three samplers, same fixture, same atlas, same cameras. `ratio` is the
//! band's 1x painted area over the 4x-supersampled reference for the same
//! camera; `jitter` is the second-difference statistic described above.
//!
//! | band | plain `textureSample` | vanilla `sampleNearest` | vanilla `sampleRGSS` |
//! |---|---|---|---|
//! | 3 (most minified) | ratio 0.401, jitter 0.0130 | 0.399, 0.0102 | **0.779, 0.0089** |
//! | 4 | 0.653, 0.0065 | 0.658, 0.0100 | 0.619, 0.0074 |
//! | 5 | 0.924, 0.0117 | 0.944, 0.0114 | 0.943, 0.0112 |
//! | 6 (magnified) | 0.959, 0.0047 | 0.959, 0.0047 | 0.959, 0.0047 |
//! | 7 (nearest) | 1.030, 0.0038 | 1.030, 0.0038 | 1.030, 0.0038 |
//!
//! Two things follow, and both were surprises. **Vanilla's default sampler is
//! not the fix** — `sampleNearest` is what `TextureFilteringMethod.NONE`
//! selects, and porting it faithfully moved the most minified band's coverage
//! by 2% (0.401 to 0.399, i.e. nothing). Only the supersampling arm does real
//! work there, which is why `model.wgsl` takes it unconditionally rather than
//! reproducing vanilla's default. And **band 4 stays materially
//! under-painted under every sampler** (0.62 to 0.66); nothing here explains
//! that, and it is left as a measured, open residual rather than absorbed
//! into a budget that hides it.
//!
//! # Scope, stated plainly
//!
//! This proves the **draw** — real baked geometry from `client.jar`, the real
//! mesher, the real pipeline, the real atlas and its real mip chain — over a
//! real camera path. It installs no wire and no ECS input, and its world is
//! flat and uniform, so it cannot speak to a plate sitting on uneven terrain
//! or to anything that goes wrong between a chunk packet and `World`.
//!
//! Fail-closed: no GPU adapter and no vanilla `client.jar` are failures, never
//! skips.
//!
//! ```text
//! cargo test -p lodestone-shell --test cutout_minification_flicker_pixels -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, ThirdPersonBodyState};
use lodestone::mesher::{
    SectionGeometry, SectionKey, mesh_snapshot_models, snapshot_section, snapshot_visibility,
};
use lodestone::resources::BlockResources;
use lodestone_render::{
    BlockAtlas, Camera, GpuContext, HeadlessTarget, ModelMesh, RenderTarget, entity_anim::AnimInput,
};
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};

const W: u32 = 512;
const H: u32 = 384;
const FOV_Y_DEGREES: f32 = 70.0;

const RD_CHUNKS: i32 = 8;
const MIN_Y: i32 = 0;
const SURFACE_Y: i32 = 64;
const SECTION_COUNT: usize = 6;

/// The subject: the block the report names. `segment_amount=4` is the full
/// 16×16 plane, i.e. the largest area of cutout this family ever presents.
const PLATE: &str = "minecraft:leaf_litter[facing=north,segment_amount=4]";

/// Shallow enough that the ground recedes to the horizon, so the lower bands
/// are near ground and the upper ground bands are heavily minified — the
/// regime the cutout discard is unstable in.
const SWEEP_PITCH: f32 = 12.0;
/// Off-axis so the sweep direction is not parallel to a texel axis, which
/// would let a whole row of the plate cross its discard threshold at once.
const SWEEP_YAW: f32 = 27.0;

/// Camera positions per sweep, and how far the eye travels over all of them.
///
/// The span is deliberately **small**: an eighth of a block over 24 steps puts
/// each step well under a pixel of legitimate on-screen motion in the far
/// bands, so anything that moves the measured area by a lot between two
/// adjacent frames moved it for a reason other than the picture changing.
const SWEEP_STEPS: usize = 24;
const SWEEP_SPAN_BLOCKS: f32 = 0.125;

/// Horizontal bands the frame is split into.
const BANDS: usize = 8;

/// Past the 0.75 s section fade-in — without this every section renders as
/// pure fog colour.
const FADE_COMPLETE_TICK: u64 = 200;

fn state_id(state: &str) -> u32 {
    lodestone_data::block_states::state_id(state)
        .unwrap_or_else(|| panic!("{state} is not in the 26.2 block-state table"))
}

/// Suppress `RenderState`'s unconditional first-person bare arm, which
/// otherwise paints a fixed screen rect into every frame.
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
                // resolves to 0 and renders everything black.
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
    if let Some(plate) = plate {
        let written = world.fill_region([lo, SURFACE_Y, lo], [hi, SURFACE_Y, hi], plate);
        assert!(written > 0, "fixture: plate must actually be written");
    }
    world
}

fn build_scene(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    atlas: &BlockAtlas,
    world: &World,
) -> RenderState {
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas));
    suppress_first_person_arm(&mut state);
    upload_world(device, queue, &mut state, atlas, world);
    state
}

/// Mesh and upload every section of `world` through the production path.
fn upload_world(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    state: &mut RenderState,
    atlas: &BlockAtlas,
    world: &World,
) {
    let models = atlas.models().expect("atlas must carry baked models");
    let mut uploaded = 0usize;
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            for si in 0..SECTION_COUNT {
                let key = SectionKey {
                    cx,
                    cz,
                    si,
                    min_y: MIN_Y,
                };
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
    state.update_animation(queue, FADE_COMPLETE_TICK);
}

fn render(
    state: &mut RenderState,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    target: &mut HeadlessTarget,
    camera: &Camera,
) -> Vec<u8> {
    let frame = target.acquire().expect("headless acquire");
    let _ = state.render(device, queue, frame.view(), camera, None, &[]);
    target.read_texels(device, queue)
}

/// The sweep's `i`th camera: the eye slides along its own forward bearing by
/// [`SWEEP_SPAN_BLOCKS`] in total.
fn camera_at(step: usize) -> Camera {
    let t = SWEEP_SPAN_BLOCKS * step as f32 / SWEEP_STEPS as f32;
    let yaw = SWEEP_YAW.to_radians();
    Camera {
        position: glam::Vec3::new(
            0.5 + t * -yaw.sin(),
            SURFACE_Y as f32 + 1.0 + 1.62,
            0.5 + t * yaw.cos(),
        ),
        yaw: SWEEP_YAW,
        pitch: SWEEP_PITCH,
        fov_y_degrees: FOV_Y_DEGREES,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS as u32, 0),
    }
}

/// The screen row elevation `0` lands on, derived from the camera rather than
/// read off a picture. Positive pitch looks down, so the horizon sits above
/// the frame centre; everything strictly below is ground.
fn horizon_row(camera: &Camera) -> f32 {
    let half = f64::from(H) / 2.0;
    let t = f64::from(camera.pitch.to_radians().tan())
        / f64::from((camera.fov_y_degrees / 2.0).to_radians().tan());
    (half - half * t) as f32
}

/// Per-channel difference above which two frames' pixels count as materially
/// different. Well above dither/rounding, well below leaf-litter over grass.
const CHANNEL_DELTA: i32 = 24;

fn differs(p: &[u8], q: &[u8]) -> bool {
    (0..3).any(|c| (i32::from(p[c]) - i32::from(q[c])).abs() > CHANNEL_DELTA)
}

/// Pixels the plate painted, per horizontal band, top of frame first.
fn coverage_by_band(bare: &[u8], plated: &[u8]) -> Vec<usize> {
    let rows_per = H as usize / BANDS;
    let mut out = vec![0usize; BANDS];
    for y in 0..H as usize {
        let band = (y / rows_per).min(BANDS - 1);
        for x in 0..W as usize {
            let i = (y * W as usize + x) * 4;
            if differs(&bare[i..i + 4], &plated[i..i + 4]) {
                out[band] += 1;
            }
        }
    }
    out
}

/// One band's behaviour over the whole sweep.
#[derive(Debug, Clone, Copy)]
struct BandStats {
    mean: f64,
    /// Mean absolute **first** difference between adjacent sweep steps, over
    /// the mean. Reported for context only: a band close to the camera moves a
    /// lot of content across the frame edge per step, so its first difference
    /// is dominated by legitimate motion and is large even when nothing is
    /// aliasing.
    drift: f64,
    /// Mean absolute **second** difference, over the mean. The flicker number.
    /// Translating the eye by a sub-pixel step changes a band's painted area
    /// *smoothly* — content entering and leaving is a near-linear trend over a
    /// span this short, and a linear trend has zero second difference. Only a
    /// quantity that jumps between neighbouring frames survives, which is
    /// exactly what a discard threshold being crossed by filtered alpha looks
    /// like.
    jitter: f64,
}

fn band_stats(series: &[usize]) -> BandStats {
    let n = series.len() as f64;
    let mean = series.iter().map(|&v| v as f64).sum::<f64>() / n;
    let first: f64 = series
        .windows(2)
        .map(|w| (w[1] as f64 - w[0] as f64).abs())
        .sum::<f64>()
        / (series.len() - 1) as f64;
    let second: f64 = series
        .windows(3)
        .map(|w| (w[2] as f64 - 2.0 * w[1] as f64 + w[0] as f64).abs())
        .sum::<f64>()
        / (series.len() - 2) as f64;
    if mean > 0.0 {
        BandStats {
            mean,
            drift: first / mean,
            jitter: second / mean,
        }
    } else {
        BandStats {
            mean,
            drift: 0.0,
            jitter: 0.0,
        }
    }
}

/// Runs the whole sweep against one atlas and returns per-band statistics
/// together with the derived horizon row.
fn sweep(ctx: &GpuContext, atlas: &BlockAtlas) -> (Vec<BandStats>, f32) {
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let ground = state_id("minecraft:grass_block[snowy=false]");
    let air = state_id("minecraft:air");

    let bare_world = plated_world(ground, None, air);
    let mut bare_scene = build_scene(device, queue, format, atlas, &bare_world);
    let bares: Vec<Vec<u8>> = (0..SWEEP_STEPS)
        .map(|s| render(&mut bare_scene, device, queue, &mut target, &camera_at(s)))
        .collect();
    drop(bare_scene);

    let plated_world_ = plated_world(ground, Some(state_id(PLATE)), air);
    let mut plated_scene = build_scene(device, queue, format, atlas, &plated_world_);

    let mut series: Vec<Vec<usize>> = vec![Vec::with_capacity(SWEEP_STEPS); BANDS];
    for (s, bare) in bares.iter().enumerate() {
        let plated = render(
            &mut plated_scene,
            device,
            queue,
            &mut target,
            &camera_at(s),
        );
        for (band, n) in coverage_by_band(bare, &plated).into_iter().enumerate() {
            series[band].push(n);
        }
    }

    (
        series.iter().map(|s| band_stats(s)).collect(),
        horizon_row(&camera_at(0)),
    )
}

/// Bands strictly below the horizon that carry a real amount of plate. The
/// near bands are magnified and never the problem; the interesting ones are
/// the minified bands just under the horizon.
fn ground_bands(stats: &[BandStats], horizon: f32) -> Vec<usize> {
    let rows_per = H as f32 / BANDS as f32;
    (0..BANDS)
        .filter(|&i| (i as f32) * rows_per > horizon && stats[i].mean > 200.0)
        .collect()
}

fn report(label: &str, stats: &[BandStats], horizon: f32) {
    println!("--- {label} (horizon row {horizon:.1})");
    for (i, s) in stats.iter().enumerate() {
        println!(
            "  band {i} (rows {:>3}..): mean {:>8.1} px, drift {:.4}, jitter {:.4}",
            (i * (H as usize / BANDS)),
            s.mean,
            s.drift,
            s.jitter
        );
    }
}

/// How much a reference frame is supersampled by, per axis.
const SS: u32 = 4;

/// The area the plate *should* paint in each band, measured by rendering the
/// same camera at [`SS`]x resolution per axis and box-averaging the
/// plated-vs-bare mask back down. This is an expectation from **outside** the
/// sampler under test: at 4x per axis the ground is sixteen times less
/// minified, so the mip level the hardware picks is four levels sharper and
/// the answer is dominated by the geometry rather than by whatever the 1x
/// sample does at the discard boundary. A 1x sampler that paints far less
/// area than this is dissolving the plate; one that paints far more is
/// smearing it.
fn supersampled_coverage(ctx: &GpuContext, atlas: &BlockAtlas, step: usize) -> Vec<f64> {
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (bw, bh) = (W * SS, H * SS);
    let mut target = HeadlessTarget::new(device, bw, bh, format);

    let ground = state_id("minecraft:grass_block[snowy=false]");
    let air = state_id("minecraft:air");
    let camera = Camera {
        aspect: bw as f32 / bh as f32,
        ..camera_at(step)
    };

    let mut shot = |world: &World| {
        let mut state = RenderState::new(device, queue, format, bw, bh, Some(atlas));
        suppress_first_person_arm(&mut state);
        upload_world(device, queue, &mut state, atlas, world);
        let frame = target.acquire().expect("headless acquire");
        let _ = state.render(device, queue, frame.view(), &camera, None, &[]);
        target.read_texels(device, queue)
    };
    let bare = shot(&plated_world(ground, None, air));
    let plated = shot(&plated_world(ground, Some(state_id(PLATE)), air));

    let rows_per = H as usize / BANDS;
    let mut out = vec![0f64; BANDS];
    for y in 0..bh as usize {
        let band = ((y / SS as usize) / rows_per).min(BANDS - 1);
        for x in 0..bw as usize {
            let i = (y * bw as usize + x) * 4;
            if differs(&bare[i..i + 4], &plated[i..i + 4]) {
                out[band] += 1.0;
            }
        }
    }
    for v in &mut out {
        *v /= f64::from(SS * SS);
    }
    out
}

fn gpu() -> GpuContext {
    GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         do NOT treat a skip as a pass",
    )
}

fn shipped_atlas() -> std::sync::Arc<BlockAtlas> {
    let resources = BlockResources::load(true);
    resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "vanilla assets did not load (banner: {:?}) — this gate needs a real \
             client.jar under .cache/mc/26.2",
            resources.banner
        )
    })
}

/// The same corpus rebuilt at an explicit mip depth, so the control can ask
/// for a chain this renderer never ships.
fn atlas_at_mip_levels(levels: u32) -> BlockAtlas {
    use lodestone_assets::{ResourceManager, ZipSource};
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("workspace root")
        .join(".cache/mc/26.2");
    let jar = std::fs::read(root.join("client.jar")).expect("read client.jar");
    let report = root.join("generated/reports/blocks.json");
    let report_bytes =
        std::fs::read(&report).unwrap_or_else(|e| panic!("read {}: {e}", report.display()));
    let registry = lodestone_render::BlocksJsonRegistry::from_slice(&report_bytes)
        .unwrap_or_else(|e| panic!("load {}: {e}", report.display()));
    let zip = ZipSource::from_bytes(jar).expect("open client.jar");
    let manager = ResourceManager::new(vec![Box::new(zip)]);
    let atlas = BlockAtlas::build_with_mip_levels(&manager, &registry, levels)
        .unwrap_or_else(|e| panic!("build atlas at mip_levels={levels}: {e}"));
    // At the same depth: the model pass binds *these* models' atlas, not the
    // `BlockAtlas` above, so a control that varied only the latter would vary
    // nothing the terrain draw can see.
    let models = lodestone_render::BlockModels::build_with_mip_levels(&manager, &registry, levels)
        .unwrap_or_else(|e| panic!("build models: {e}"));
    atlas.with_models(models)
}

/// The control. A minified cutout with **no mip chain at all** is this
/// renderer aliasing as hard as it can, so the sweep must report materially
/// more jitter in the minified bands than the shipped atlas does. If this does
/// not fire, the statistic below measures nothing.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_mipless_atlas_flickers() {
    let ctx = gpu();
    let shipped = shipped_atlas();
    let (good, horizon) = sweep(&ctx, &shipped);
    report("shipped atlas", &good, horizon);
    drop(shipped);

    let mipless = atlas_at_mip_levels(0);
    assert_eq!(
        mipless.atlas().mip_count(),
        1,
        "control fixture: the mipless atlas must really have no chain"
    );
    let (bad, _) = sweep(&ctx, &mipless);
    report("mipless atlas", &bad, horizon);

    let bands = ground_bands(&good, horizon);
    assert!(
        !bands.is_empty(),
        "no ground band carried enough plate to measure — the fixture, not the renderer"
    );
    // Collect and assert on the collection: an assert inside the loop would
    // prove exactly one band and leave the rest unobserved.
    let worse: Vec<usize> = bands
        .iter()
        .copied()
        .filter(|&b| bad[b].jitter > good[b].jitter * 1.5)
        .collect();
    println!(
        "ground bands {bands:?}; mipless is >1.5x jitterier in {worse:?}"
    );
    assert!(
        !worse.is_empty(),
        "removing the whole mip chain did not raise measured jitter in any ground \
         band, so this sweep cannot see cutout aliasing and no clean result from it \
         is evidence"
    );
}

/// The subject. Reported rather than thresholded on its own: this number is
/// the baseline a sampling change has to move, and the run prints it.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn leaf_litter_holds_its_coverage_across_a_slow_sweep() {
    let ctx = gpu();
    let atlas = shipped_atlas();
    let (stats, horizon) = sweep(&ctx, &atlas);
    report("shipped atlas", &stats, horizon);
    let bands = ground_bands(&stats, horizon);
    assert!(
        !bands.is_empty(),
        "no ground band carried enough plate to measure — the fixture, not the renderer"
    );
    let noisy: Vec<String> = bands
        .iter()
        .filter(|&&b| stats[b].jitter > JITTER_BUDGET)
        .map(|&b| format!("band {b}: jitter {:.4}", stats[b].jitter))
        .collect();
    assert!(
        noisy.is_empty(),
        "leaf litter's painted area wobbles across a sub-block camera sweep: {noisy:?}"
    );
}

/// The dissolve half of the report, measured against an outside expectation.
///
/// "Popping in and out" is not only shimmer: a cutout that minifies past what
/// the sampler can hold simply stops passing the discard, so a distant plate
/// thins out and fills back in as the eye approaches. That is a *magnitude*
/// question — "the plate paints something out there" is satisfied by one
/// pixel — so it needs a predicted area, and the prediction comes from a
/// [`SS`]x supersampled render of the same camera rather than from this
/// renderer's own 1x output.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn distant_leaf_litter_paints_what_a_supersampled_reference_says_it_should() {
    let ctx = gpu();
    let atlas = shipped_atlas();
    let (stats, horizon) = sweep(&ctx, &atlas);
    let reference = supersampled_coverage(&ctx, &atlas, 0);
    println!("--- 1x vs {SS}x-supersampled coverage (horizon row {horizon:.1})");
    let mut thin = Vec::new();
    for b in ground_bands(&stats, horizon) {
        let ratio = stats[b].mean / reference[b].max(1.0);
        println!(
            "  band {b}: 1x {:>8.1} px, {SS}x reference {:>8.1} px, ratio {ratio:.3}",
            stats[b].mean, reference[b]
        );
        if !(COVERAGE_FLOOR..=1.35).contains(&ratio) {
            thin.push(format!("band {b}: ratio {ratio:.3}"));
        }
    }
    assert!(
        thin.is_empty(),
        "leaf litter's painted area diverges from a {SS}x supersampled render of the          same camera: {thin:?}"
    );
}

/// The share of a 4x-supersampled render's painted area a 1x band has to
/// reach. Placed between the two hypotheses rather than fitted to either: the
/// plain-`textureSample` build measured **0.401** in the most minified band
/// and `sample_rgss` measures **0.779** there, with the worst remaining band
/// (4, and unexplained — see this file's table) at **0.619**.
const COVERAGE_FLOOR: f64 = 0.55;

/// Mean second-difference in a band's painted area, as a fraction of that
/// area. The magnified bands measure 0.0038-0.0047 (pure camera motion, and
/// identical under every sampler and every mip depth tried), the minified
/// bands 0.0074-0.0114, and the no-mip-chain control 0.0323. Set above the
/// shipped bands and well below the control, so it fails on a real regression
/// in filtering without tracking ordinary frame-to-frame motion.
const JITTER_BUDGET: f64 = 0.020;
