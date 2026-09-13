//! Multi-section culling benchmark — the world-scale counterpart to the
//! single-section `live_gate`. It measures [`WorldScene::plan_frame`] (frustum +
//! occlusion culling and draw-list assembly) over a full **stated view
//! distance** and reports frame time together with the draw/cull breakdown.
//!
//! Two disciplines, both learned earlier this session, are baked in:
//!
//! * **Stated configuration.** A performance number without a reproducible
//!   configuration is not a measurement, so every reported figure is printed
//!   next to its view distance and section count.
//! * **Visible failure mode.** A renderer that is "fast" because it culled
//!   *everything* is the pixel-domain version of a gate that passes while
//!   asserting nothing. Each config asserts [`CullStats::is_meaningful`] — the
//!   frame both drew something and culled something — and the specific cull
//!   mechanism it is meant to exercise. A regression to draw-all or draw-none
//!   fails here rather than silently reporting an impressive µs.
//!
//! This is CPU-only and hermetic (no GPU, no window, no server), so it runs in
//! the default suite as an always-on regression. The GPU frame-time counterpart
//! lives behind `--ignored` in `live_gate`.

use std::hint::black_box;
use std::time::Instant;

use lodestone_render::{Camera, CullStats, DrawRegion, SectionVisibility, WorldScene, section_of};

const INDICES_PER_QUAD: u32 = 6;

/// A drawable section region carrying `quads` worth of indices.
fn solid_mesh(instance: u32, quads: u32) -> DrawRegion {
    DrawRegion {
        first_index: 0,
        index_count: quads * INDICES_PER_QUAD,
        base_vertex: 0,
        instance,
        visible: true,
    }
}

/// An empty (air) region — routes the occlusion walk but draws nothing.
fn air_mesh() -> DrawRegion {
    DrawRegion {
        first_index: 0,
        index_count: 0,
        base_vertex: 0,
        instance: 0,
        visible: true,
    }
}

/// Average `plan_frame` time over `iters` runs, in microseconds. Uses
/// `black_box` so the plan cannot be optimised away, and sums a byte of the
/// output so the compiler must actually produce it.
fn bench_plan(scene: &WorldScene, camera: &Camera, iters: u32) -> (f64, CullStats) {
    // Warm up (caches, first-frame allocation).
    let warm = scene.plan_frame(camera);
    let mut sink = warm.stats.drawn;

    let start = Instant::now();
    for _ in 0..iters {
        let plan = scene.plan_frame(black_box(camera));
        sink = sink.wrapping_add(black_box(plan.regions.len()));
    }
    let elapsed = start.elapsed();
    black_box(sink);

    let micros = elapsed.as_secs_f64() * 1e6 / f64::from(iters);
    (micros, warm.stats)
}

#[test]
fn bench_flat_terrain_view_distance() {
    // Flat world matching the live 26.2 server: two solid layers (buried +
    // surface) under an air layer the camera stands in. Stated view distance.
    const RD: i32 = 12; // render distance in chunks
    let columns = (2 * RD + 1) * (2 * RD + 1);

    let mut scene = WorldScene::new();
    for x in -RD..=RD {
        for z in -RD..=RD {
            let instance = ((x + RD) * (2 * RD + 1) + (z + RD)) as u32;
            // y=0 buried solid layer, y=1 surface solid layer, y=2 air (camera).
            scene.insert_section(
                (x, 0, z),
                solid_mesh(instance, 128),
                SectionVisibility::solid(),
            );
            scene.insert_section(
                (x, 1, z),
                solid_mesh(instance + 100_000, 128),
                SectionVisibility::solid(),
            );
            scene.insert_section((x, 2, z), air_mesh(), SectionVisibility::all());
        }
    }

    // Camera standing on the surface (section (0,2,0)), looking out and slightly
    // down across the terrain.
    let camera = Camera {
        position: glam::Vec3::new(8.0, 40.0, 8.0),
        yaw: 0.0,
        pitch: 20.0,
        ..Camera::default()
    };
    assert_eq!(section_of(camera.position), (0, 2, 0));

    let (micros, stats) = bench_plan(&scene, &camera, 300);

    println!("=== flat-terrain culling benchmark ===");
    println!(
        "view distance:      {RD} chunks ({columns} columns, {} sections)",
        scene.loaded_len()
    );
    println!("drawable sections:  {}", stats.drawable);
    println!(
        "drawn:              {} ({} quads)",
        stats.drawn, stats.drawn_quads
    );
    println!(
        "culled (frustum):   {}   culled (occlusion): {}",
        stats.culled_frustum, stats.culled_occlusion
    );
    println!("plan_frame:         {micros:.2} µs/frame");

    // Non-vacuity: the frame both drew and culled.
    assert!(
        stats.is_meaningful(),
        "flat-terrain frame must draw AND cull, not draw-all/draw-none: {stats:?}",
    );
    // Frustum must reject the hemisphere behind the camera.
    assert!(
        stats.culled_frustum > 0,
        "half the world is behind the camera; frustum must cull it: {stats:?}",
    );
    // The buried layer sits behind the surface layer, so occlusion must hide it.
    assert!(
        stats.culled_occlusion > 0,
        "the buried solid layer must be occlusion-culled behind the surface: {stats:?}",
    );
    // And it must not draw the whole world.
    assert!(
        stats.drawn < stats.drawable,
        "a meaningful frame never draws every drawable section: {stats:?}",
    );
    // Invariant that makes the numbers trustworthy.
    assert_eq!(
        stats.drawable,
        stats.drawn + stats.culled_frustum + stats.culled_occlusion,
    );
}

#[test]
fn bench_occlusion_scales_behind_a_wall() {
    // A solid wall plane at z=1 seals off a field of drawable sections at
    // z>=2. From the camera the wall draws; everything behind it must be
    // occlusion-culled regardless of how much of it there is.
    const R: i32 = 8;
    let mut scene = WorldScene::new();
    scene.insert_section((0, 0, 0), air_mesh(), SectionVisibility::all());
    for x in -R..=R {
        for y in -R..=R {
            // Air plane at z=0 so the walk spreads across the wall face.
            scene.insert_section((x, y, 0), air_mesh(), SectionVisibility::all());
            // Solid wall at z=1.
            let wall = ((x + R) * (2 * R + 1) + (y + R)) as u32;
            scene.insert_section((x, y, 1), solid_mesh(wall, 128), SectionVisibility::solid());
            // Drawable field sealed behind the wall.
            for z in 2..=5 {
                let inst = 1_000_000 + wall * 10 + z as u32;
                scene.insert_section((x, y, z), solid_mesh(inst, 128), SectionVisibility::all());
            }
        }
    }

    let camera = Camera {
        position: glam::Vec3::new(8.0, 8.0, 8.0),
        ..Camera::default() // yaw 0 → +Z, straight at the wall
    };
    assert_eq!(section_of(camera.position), (0, 0, 0));

    let (micros, stats) = bench_plan(&scene, &camera, 300);
    let sealed = (2 * R + 1) * (2 * R + 1) * 4; // z=2..=5 field

    println!("=== occlusion-behind-wall benchmark ===");
    println!("sealed field:       {sealed} drawable sections behind the wall");
    println!(
        "drawn:              {}   culled(occlusion): {}   culled(frustum): {}",
        stats.drawn, stats.culled_occlusion, stats.culled_frustum
    );
    println!("plan_frame:         {micros:.2} µs/frame");

    assert!(stats.is_meaningful(), "must draw AND cull: {stats:?}");
    assert!(
        stats.culled_occlusion >= (2 * R + 1) as usize,
        "the sealed field behind the wall must be occlusion-culled: {stats:?}",
    );
    assert!(
        stats.drawn > 0,
        "the wall the camera faces must draw: {stats:?}",
    );
}
