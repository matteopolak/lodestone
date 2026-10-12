//! Frame-time cost of back-to-front sorting on a busy translucent scene: an
//! ocean out to render distance 12 under a camera that walks across blocks.
//!
//! Each frame's CPU time (encode and submit) plus the wait for the GPU is
//! recorded with sorting on and off over the same camera path, and the
//! difference is the cost. `#[ignore]`d; it prints, and only asserts that the
//! scene really exercises sorting.
//!
//! ```text
//! cargo test --release -p lodestone-shell --test world translucent_sort_cost -- --ignored --nocapture
//! ```

use std::time::Instant;

use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone::mesher::{
    SectionGeometry, SectionKey, mesh_snapshot_fluids, mesh_snapshot_models_layers, snapshot_section,
    snapshot_visibility,
};
use lodestone::resources::BlockResources;
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget, fog::FogSettings};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

const W: u32 = 1280;
const H: u32 = 720;
const RD_CHUNKS: i32 = 12;
const MIN_Y: i32 = 0;
const SECTION_COUNT: usize = 8;
const SEABED_TOP: i32 = 40;
const SEA_TOP: i32 = 62;
const FRAMES: usize = 150;

fn first_state_named(name: &str) -> u32 {
    (0..lodestone_data::block_states::STATE_COUNT)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some(name))
        .unwrap_or_else(|| panic!("{name} is not in the block-state table"))
}

fn ocean_world() -> World {
    let (air, stone, water) = (
        first_state_named("minecraft:air"),
        first_state_named("minecraft:stone"),
        first_state_named("minecraft:water"),
    );
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
            world.load(ChunkPos::new(cx, cz), LoadedChunk::new(column, light, Heightmaps::new(), Vec::new()));
        }
    }
    let extent = RD_CHUNKS * 16;
    world.fill_region([-extent, MIN_Y, -extent], [extent + 15, SEABED_TOP, extent + 15], stone);
    world.fill_region([-extent, SEABED_TOP + 1, -extent], [extent + 15, SEA_TOP, extent + 15], water);
    world
}

fn percentile(sorted: &[f64], q: f64) -> f64 {
    sorted[((sorted.len() - 1) as f64 * q).round() as usize]
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar; run with --release"]
fn translucent_sort_cost() {
    let ctx = GpuContext::new_headless_blocking().expect("a wgpu adapter is required");
    let device = ctx.device();
    let queue = ctx.queue();
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().expect("vanilla assets must load");
    let models = atlas.models().expect("baked models");
    let world = ocean_world();

    let started = Instant::now();
    let mut geometry = Vec::new();
    for cx in -RD_CHUNKS..=RD_CHUNKS {
        for cz in -RD_CHUNKS..=RD_CHUNKS {
            for si in 1..=4 {
                let key = SectionKey { cx, cz, si, min_y: MIN_Y };
                let Some(snap) = snapshot_section(&world, key, Default::default()) else { continue };
                let (mut opaque, translucent_blocks) = mesh_snapshot_models_layers(&snap, models, true, 2);
                let fluids = mesh_snapshot_fluids(&snap, models);
                opaque.merge(&fluids.lava);
                let visibility = snapshot_visibility(&snap, models);
                geometry.push((key, SectionGeometry::Model { opaque, water: fluids.water, translucent_blocks, visibility }));
            }
        }
    }
    let water_quads: usize = geometry
        .iter()
        .map(|(_, g)| match g {
            SectionGeometry::Model { water, .. } => water.quad_count(),
            _ => 0,
        })
        .sum();
    eprintln!("  meshed {} sections, {water_quads} water quads, in {:.1}s", geometry.len(), started.elapsed().as_secs_f64());

    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut measure = |sorting: bool| -> (Vec<f64>, usize, usize) {
        let mut state = RenderState::new(device, queue, format, W, H, Some(&atlas));
        state.set_fog(FogSettings::for_render_distance(SKY_COLOR, RD_CHUNKS as u32), RD_CHUNKS as u32);
        state.set_translucent_sorting(sorting);
        for (key, mesh) in &geometry {
            state.upload_section(device, queue, *key, mesh);
        }
        state.update_animation(queue, 200);
        let mut times = Vec::with_capacity(FRAMES);
        let (mut resorts, mut first_frame_resorts) = (0, 0);
        for i in 0..FRAMES + 5 {
            let camera = Camera {
                position: glam::Vec3::new(0.5 + i as f32 * 0.3, 75.0, 0.5),
                yaw: i as f32 * 1.5,
                pitch: 25.0,
                fov_y_degrees: 70.0,
                aspect: W as f32 / H as f32,
                near: 0.05,
                far: Camera::far_for_render_distance(RD_CHUNKS as u32, 0),
            };
            let frame = target.acquire().expect("headless acquire");
            let t0 = Instant::now();
            let stats = state.render(device, queue, frame.view(), &camera, None, &[]);
            let _ = device.poll(wgpu::PollType::wait_indefinitely());
            let ms = t0.elapsed().as_secs_f64() * 1000.0;
            if i == 0 {
                first_frame_resorts = stats.translucent_resorts;
                eprintln!("    first frame ({}): {ms:.2} ms, {} resorts", if sorting { "sorted" } else { "unsorted" }, stats.translucent_resorts);
            } else if i >= 5 {
                times.push(ms);
                resorts += stats.translucent_resorts;
            }
        }
        (times, resorts, first_frame_resorts)
    };
    // Unsorted first and last, so any drift in the machine's state shows.
    let (mut off_a, _, _) = measure(false);
    let (mut on, resorts, first) = measure(true);
    let (mut off_b, _, _) = measure(false);
    off_a.append(&mut off_b);
    for (name, times) in [("sorting off", &mut off_a), ("sorting on", &mut on)] {
        times.sort_by(f64::total_cmp);
        let mean = times.iter().sum::<f64>() / times.len() as f64;
        eprintln!(
            "  {name}: mean {mean:.2} ms, median {:.2}, p95 {:.2}, max {:.2} ({} frames)",
            percentile(times, 0.5),
            percentile(times, 0.95),
            percentile(times, 1.0),
            times.len()
        );
    }
    eprintln!("  resorts: {resorts} over {FRAMES} frames ({:.1} per frame), {first} on the first", resorts as f64 / FRAMES as f64);
    assert!(first > 0 && resorts > 0, "the scene did not exercise sorting");
}
