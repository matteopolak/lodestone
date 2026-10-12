//! Improved transparency: red stained glass behind a water wall composites the
//! same whichever order the draws happen, which the sorted blended pipeline
//! cannot do.
//!
//! # The scene
//!
//! The camera looks along +z across a stone floor at a backdrop. A wall of water
//! stands 8 blocks ahead and, behind it, a wall of red stained glass 16 blocks
//! ahead; both cover the same block of pixels, which therefore see water in front
//! of glass in front of the backdrop. The sorted pipeline draws all water before
//! all translucent blocks, so it paints the glass **over** the water (glass is
//! behind it, but water writes no depth). The reference rule is the other order:
//! water over (glass over backdrop).
//!
//! # The expectation
//!
//! Computed per pixel, in linear light, from single-layer frames of the *sorted*
//! pipeline, never from the pipeline under test. Take `S` the backdrop-only frame,
//! `Rw` water over the backdrop, `Rg` glass over the backdrop. The blend rule
//! `out = c*a + dst*(1-a)` makes the water wall's whole effect `Rw = Wc + T*S`,
//! where `T` is the light it lets through, so
//!
//! ```text
//! expected = Wc + T * Rg = Rw + T * (Rg - S)
//! ```
//!
//! The glass alpha cancels, so the glass sprite's per-texel alpha pattern needs no
//! model. `T` is measured, not assumed: repeating `S` and `Rw` over a second
//! backdrop colour gives `T = (Rw - Rw') / (S - S')` per pixel, and the median is
//! used. A one-block water wall shows two surfaces along a ray (the near face and
//! the far face's inward copy), which the gate checks by requiring `T` well under
//! the third a single surface passes. The pixels checked are those both walls
//! cover, found by intersecting each pixel's ray with the two wall planes.
//!
//! # Controls, all executed
//!
//! * The sorted two-layer frame must **disagree** with the expectation on nearly
//!   every checked pixel: if it did not, the scene cannot tell the orders apart.
//! * The improved-transparency frame must agree with it to quantisation.
//! * One layer alone, water and glass each, must match the sorted pipeline's own
//!   single-layer frame everywhere in the image, so the composite is not only
//!   close in the overlap.
//! * The improved frames must report a composite, and the sorted ones none: the
//!   frame took the path this gate is about.
//!
//! `#[ignore]`d and fail-closed: no adapter, no `FLOAT32_BLENDABLE` and no
//! vanilla `client.jar` are failures, never skips.
//!
//! ```text
//! cargo test -p lodestone-shell --test world improved_transparency -- --ignored --nocapture
//! ```

use lodestone::gpu::{RenderState, RenderStats, SKY_COLOR, ThirdPersonBodyState};
use lodestone::mesher::{
    SectionGeometry, SectionKey, mesh_snapshot_fluids, mesh_snapshot_models_layers, snapshot_section,
    snapshot_visibility,
};
use lodestone::resources::BlockResources;
use lodestone_render::{
    Camera, GpuContext, HeadlessTarget, RenderTarget, entity_anim::AnimInput, fog::FogSettings,
};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

const W: u32 = 640;
const H: u32 = 480;
const RD_CHUNKS: i32 = 4;
const MIN_Y: i32 = 0;
const FLOOR_TOP: i32 = 64;
const SECTION_COUNT: usize = 8;
const EYE: glam::Vec3 = glam::Vec3::new(0.5, FLOOR_TOP as f32 + 1.62, 0.5);
const FADE_COMPLETE_TICK: u64 = 200;

type Wall = ([i32; 3], [i32; 3]);
/// The water wall: blocks `x -6..=-1`, `y 65..=68`, one block thick at `z = 8`.
const WATER: Wall = ([-6, 65, 8], [-1, 68, 8]);
/// The glass wall behind it, wider and taller so it covers everything the water
/// does: blocks `x -10..=-1`, `y 64..=69` at `z = 16`.
const GLASS: Wall = ([-10, 64, 16], [-1, 69, 16]);
/// The backdrop at `z = 30`.
const BACKDROP: Wall = ([-14, 64, 30], [14, 76, 30]);

/// The water wall shows two surfaces along a ray (the near face and the far
/// face's inward copy). A single surface lets about a third of the light
/// through, so a measured transmittance under this bound means both are drawn.
const TWO_SURFACE_MAX_TRANSMITTANCE: f32 = 0.2;
/// Allowance in encoded 8-bit levels between the exact prediction and the
/// order-independent result, which carries rank-2 wavelet, half-float and
/// depth-bin quantisation across three stacked surfaces (measured worst 6.4).
const TOLERANCE: f32 = 8.0;
/// The sorted two-layer frame must miss the expectation by more than this many
/// levels on nearly every checked pixel.
const CONTROL_MIN_DEVIATION: f32 = 16.0;
/// Margin kept inside each wall's edges, in blocks, so a silhouette pixel is
/// never classified.
const EDGE_MARGIN: f32 = 0.15;

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

fn first_state_named(name: &str) -> u32 {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} is not in the block-state table"))
}

struct Ids {
    air: u32,
    stone: u32,
    white: u32,
    water: u32,
    glass: u32,
}

struct Layers {
    water: bool,
    glass: bool,
    /// The block the backdrop wall is made of.
    backdrop: u32,
}

fn build_world(ids: &Ids, layers: &Layers) -> World {
    let mut world = World::new();
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            let column = ChunkColumn::new(
                MIN_Y,
                SECTION_COUNT,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                ids.air,
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
    let extent = RD_CHUNKS * 16;
    assert!(
        world.fill_region([-extent, MIN_Y, -extent], [extent + 15, FLOOR_TOP - 1, extent + 15], ids.stone) > 0,
        "fixture: the floor must write blocks"
    );
    assert!(world.fill_region(BACKDROP.0, BACKDROP.1, layers.backdrop) > 0, "fixture: backdrop");
    if layers.water {
        assert!(world.fill_region(WATER.0, WATER.1, ids.water) > 0, "fixture: water wall");
    }
    if layers.glass {
        assert!(world.fill_region(GLASS.0, GLASS.1, ids.glass) > 0, "fixture: glass wall");
    }
    world
}

/// The production mesher's output for every section of `world`.
fn mesh_world(world: &World, models: &lodestone_render::BlockModels) -> Vec<(SectionKey, SectionGeometry)> {
    let mut out = Vec::new();
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            for si in 0..SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: MIN_Y };
                let Some(snap) = snapshot_section(world, key, Default::default()) else { continue };
                let (mut opaque, translucent_blocks) = mesh_snapshot_models_layers(&snap, models, true, 2);
                let fluids = mesh_snapshot_fluids(&snap, models);
                opaque.merge(&fluids.lava);
                let visibility = snapshot_visibility(&snap, models);
                out.push((
                    key,
                    SectionGeometry::Model { opaque, water: fluids.water, translucent_blocks, visibility },
                ));
            }
        }
    }
    out
}

struct Frame {
    rgba: Vec<u8>,
    stats: RenderStats,
}

#[allow(clippy::too_many_arguments)]
fn render_frame(
    device: &wgpu::Device,
    queue: &wgpu::Queue,
    format: wgpu::TextureFormat,
    target: &mut HeadlessTarget,
    atlas: &lodestone_render::BlockAtlas,
    geometry: &[(SectionKey, SectionGeometry)],
    camera: &Camera,
    improved: bool,
) -> Frame {
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas));
    suppress_first_person_arm(&mut state);
    state.set_fog(FogSettings::for_render_distance(SKY_COLOR, RD_CHUNKS as u32), RD_CHUNKS as u32);
    state.set_improved_transparency(improved);
    for (key, mesh) in geometry {
        state.upload_section(device, queue, *key, mesh);
    }
    state.update_animation(queue, FADE_COMPLETE_TICK);
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), camera, None, &[]);
    Frame { rgba: target.read_texels(device, queue), stats }
}

fn ray_dir(camera: &Camera, px: f32, py: f32) -> glam::Vec3 {
    let (sy, cy) = camera.yaw.to_radians().sin_cos();
    let (sp, cp) = camera.pitch.to_radians().sin_cos();
    let right = glam::Vec3::new(-cy, 0.0, -sy);
    let up = glam::Vec3::new(-sy * sp, cp, cy * sp);
    let forward = glam::Vec3::new(-sy * cp, -sp, cy * cp);
    let half_y = (camera.fov_y_degrees.to_radians() * 0.5).tan();
    let half_x = half_y * camera.aspect;
    let ndc_x = 2.0 * px / W as f32 - 1.0;
    let ndc_y = 1.0 - 2.0 * py / H as f32;
    (forward + right * (ndc_x * half_x) + up * (ndc_y * half_y)).normalize()
}

/// Whether the ray through pixel `(x, y)` crosses the front face of the wall
/// `bounds` (inclusive block coordinates, one thick in `z`), inside its edges by
/// [`EDGE_MARGIN`].
fn crosses_front_face(camera: &Camera, x: u32, y: u32, bounds: Wall) -> bool {
    let dir = ray_dir(camera, x as f32 + 0.5, y as f32 + 0.5);
    if dir.z <= 0.0 {
        return false;
    }
    let t = (bounds.0[2] as f32 - camera.position.z) / dir.z;
    let hit = camera.position + dir * t;
    hit.x >= bounds.0[0] as f32 + EDGE_MARGIN
        && hit.x <= bounds.1[0] as f32 + 1.0 - EDGE_MARGIN
        && hit.y >= bounds.0[1] as f32 + EDGE_MARGIN
        && hit.y <= bounds.1[1] as f32 + 1.0 - EDGE_MARGIN
}

struct Box2 {
    min: [u32; 2],
    max: [u32; 2],
    count: usize,
}

impl Box2 {
    fn new() -> Self {
        Self { min: [u32::MAX; 2], max: [0; 2], count: 0 }
    }
    fn add(&mut self, x: u32, y: u32) {
        self.min = [self.min[0].min(x), self.min[1].min(y)];
        self.max = [self.max[0].max(x), self.max[1].max(y)];
        self.count += 1;
    }
}

impl std::fmt::Display for Box2 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.count == 0 {
            return write!(f, "no pixels");
        }
        write!(
            f,
            "{} px in x {}..={}, y {}..={}",
            self.count, self.min[0], self.max[0], self.min[1], self.max[1]
        )
    }
}

fn to_linear(byte: f32) -> f32 {
    let c = byte / 255.0;
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

fn to_byte(linear: f32) -> f32 {
    let c = linear.clamp(0.0, 1.0);
    let e = if c <= 0.003_130_8 { c * 12.92 } else { 1.055 * c.powf(1.0 / 2.4) - 0.055 };
    e * 255.0
}

/// The pixel's channels in linear light.
fn linear(frame: &Frame, x: u32, y: u32) -> [f32; 3] {
    let i = ((y * W + x) * 4) as usize;
    std::array::from_fn(|c| to_linear(f32::from(frame.rgba[i + c])))
}

/// The pixel's encoded bytes as floats.
fn encoded(frame: &Frame, x: u32, y: u32) -> [f32; 3] {
    let i = ((y * W + x) * 4) as usize;
    std::array::from_fn(|c| f32::from(frame.rgba[i + c]))
}

fn max_channel_diff(a: [f32; 3], b: [f32; 3]) -> f32 {
    (0..3).map(|c| (a[c] - b[c]).abs()).fold(0.0, f32::max)
}

/// Pixels outside `tolerance` between two whole frames, as a bounding box.
fn frame_mismatch(a: &Frame, b: &Frame, tolerance: f32) -> (Box2, f32) {
    let mut bounds = Box2::new();
    let mut worst = 0.0f32;
    for y in 0..H {
        for x in 0..W {
            let d = max_channel_diff(encoded(a, x, y), encoded(b, x, y));
            worst = worst.max(d);
            if d > tolerance {
                bounds.add(x, y);
            }
        }
    }
    (bounds, worst)
}

#[test]
#[ignore = "requires a GPU adapter with FLOAT32_BLENDABLE and the vanilla client.jar"]
fn glass_behind_water_composites_water_over_glass_whatever_the_draw_order() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    assert!(
        lodestone_render::oit::supported(device),
        "the device lacks FLOAT32_BLENDABLE, so the order-independent path cannot run here"
    );
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "vanilla assets did not load (banner: {:?}) — this gate needs a real client.jar \
             under .cache/mc/<ver> (LODESTONE_ASSETS)",
            resources.banner
        )
    });
    let models = atlas.models().expect("vanilla atlas must carry baked models");

    let ids = Ids {
        air: first_state_named("minecraft:air"),
        stone: first_state_named("minecraft:stone"),
        white: first_state_named("minecraft:white_concrete"),
        water: first_state_named("minecraft:water"),
        glass: first_state_named("minecraft:red_stained_glass"),
    };
    let world = |water, glass, backdrop| mesh_world(&build_world(&ids, &Layers { water, glass, backdrop }), models);
    let stone_only = world(false, false, ids.stone);
    let water_only = world(true, false, ids.stone);
    let glass_only = world(false, true, ids.stone);
    let both = world(true, true, ids.stone);
    let white_only = world(false, false, ids.white);
    let water_on_white = world(true, false, ids.white);

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let camera = Camera {
        position: EYE,
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(RD_CHUNKS as u32, 0),
    };
    let mut render = |geometry: &[(SectionKey, SectionGeometry)], improved: bool| {
        render_frame(device, queue, format, &mut target, &atlas, geometry, &camera, improved)
    };

    // The sorted pipeline, one layer at a time: the reference frames.
    let s = render(&stone_only, false);
    let rw = render(&water_only, false);
    let rg = render(&glass_only, false);
    let s_white = render(&white_only, false);
    let rw_white = render(&water_on_white, false);
    // Both layers on the sorted pipeline (the control) and on the improved one.
    let sorted_both = render(&both, false);
    let improved_both = render(&both, true);
    // One layer on the improved pipeline.
    let improved_water = render(&water_only, true);
    let improved_glass = render(&glass_only, true);

    for (name, frame, composites) in [
        ("sorted stone", &s, 0),
        ("sorted water", &rw, 0),
        ("sorted glass", &rg, 0),
        ("sorted both", &sorted_both, 0),
        ("improved both", &improved_both, 1),
        ("improved water", &improved_water, 1),
        ("improved glass", &improved_glass, 1),
    ] {
        assert_eq!(
            frame.stats.oit_composites, composites,
            "{name}: expected {composites} order-independent composite(s), the frame reports {}",
            frame.stats.oit_composites
        );
    }

    // The pixels both walls cover.
    let mut overlap = Vec::new();
    for y in 0..H {
        for x in 0..W {
            if crosses_front_face(&camera, x, y, WATER) && crosses_front_face(&camera, x, y, GLASS) {
                overlap.push((x, y));
            }
        }
    }
    assert!(
        overlap.len() > 5_000,
        "only {} px see water in front of glass — the fixture is not showing the overlap this \
         gate measures",
        overlap.len()
    );

    // The water wall's transmittance, measured over two backdrops.
    let mut samples: Vec<f32> = Vec::new();
    for &(x, y) in &overlap {
        let (a, b) = (linear(&s, x, y), linear(&s_white, x, y));
        let (c, d) = (linear(&rw, x, y), linear(&rw_white, x, y));
        for ch in 0..3 {
            if (b[ch] - a[ch]).abs() > 0.2 {
                samples.push((d[ch] - c[ch]) / (b[ch] - a[ch]));
            }
        }
    }
    assert!(samples.len() > 5_000, "only {} usable transmittance samples", samples.len());
    samples.sort_by(f32::total_cmp);
    let transmittance = samples[samples.len() / 2];
    eprintln!(
        "  water wall transmittance: {transmittance:.4} (median of {} samples)",
        samples.len()
    );
    assert!(
        transmittance < TWO_SURFACE_MAX_TRANSMITTANCE,
        "the water wall lets {transmittance:.4} of the light through; one surface would let about \
         0.33, so the far face's inward copy is not being drawn and the scene is not the \
         water-over-glass-over-backdrop stack this gate measures"
    );

    let mut improved_off = Box2::new();
    let mut sorted_off = Box2::new();
    let mut improved_worst = 0.0f32;
    let mut sorted_worst = 0.0f32;
    let mut sorted_sum = 0.0f64;
    for &(x, y) in &overlap {
        let (s, rw, rg) = (linear(&s, x, y), linear(&rw, x, y), linear(&rg, x, y));
        let expected: [f32; 3] =
            std::array::from_fn(|c| to_byte(rw[c] + transmittance * (rg[c] - s[c])));
        let improved_dev = max_channel_diff(encoded(&improved_both, x, y), expected);
        let sorted_dev = max_channel_diff(encoded(&sorted_both, x, y), expected);
        improved_worst = improved_worst.max(improved_dev);
        sorted_worst = sorted_worst.max(sorted_dev);
        sorted_sum += f64::from(sorted_dev);
        if improved_dev > TOLERANCE {
            improved_off.add(x, y);
        }
        if sorted_dev > CONTROL_MIN_DEVIATION {
            sorted_off.add(x, y);
        }
    }
    let n = overlap.len();
    eprintln!("  overlap area: {n} px");
    eprintln!(
        "  improved vs expected: worst channel deviation {improved_worst:.1} levels; beyond \
         {TOLERANCE}: {improved_off}"
    );
    eprintln!(
        "  sorted   vs expected: worst channel deviation {sorted_worst:.1}, mean {:.1} levels; \
         beyond {CONTROL_MIN_DEVIATION}: {sorted_off}",
        sorted_sum / n as f64
    );

    // The control: the sorted pipeline paints these pixels in the other order.
    assert!(
        sorted_off.count * 10 >= n * 9,
        "control failed: only {} of {n} overlap pixels differ from the water-over-glass \
         expectation by more than {CONTROL_MIN_DEVIATION} levels on the sorted pipeline \
         ({sorted_off}); the scene cannot tell the two orders apart",
        sorted_off.count
    );
    assert!(
        improved_off.count == 0,
        "improved transparency left {} of {n} overlap pixels more than {TOLERANCE} levels from the \
         water-over-glass expectation: {improved_off} (worst deviation {improved_worst:.1})",
        improved_off.count
    );

    // One layer alone is the sorted pipeline's own frame, everywhere.
    for (name, improved, sorted) in [("water", &improved_water, &rw), ("glass", &improved_glass, &rg)] {
        let (bounds, worst) = frame_mismatch(improved, sorted, TOLERANCE);
        assert!(
            bounds.count == 0,
            "improved {name}-only frame differs from the sorted {name}-only frame beyond \
             {TOLERANCE} levels: {bounds} (worst deviation {worst:.1})"
        );
    }
}
