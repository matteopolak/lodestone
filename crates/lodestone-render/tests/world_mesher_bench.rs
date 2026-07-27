//! Mesh-lifecycle benchmark: build a world at a stated view distance from real
//! `Arc<ChunkSection>` snapshots and measure the *build* half of the pipeline.
//!
//! `scene_bench` measures culling (a pre-built scene); `scene_gpu` measures GPU
//! frame time (synthetic cubes). Neither exercises the mesher, arena and driver
//! that turn live `Arc` snapshots into resident geometry. This does: it feeds a
//! synthetic-but-realistic terrain through [`dirty_jobs`] → [`build_batch`]
//! (parallel on native targets) and reports mesh-build time, quad
//! counts and the per-frame cull budget at a **stated render distance**.
//!
//! ## Anti-vacuity
//!
//! A build benchmark that is "fast" because it built nothing is the same trap as
//! a gate that asserts nothing. So this asserts real geometry was produced
//! (`quads > 0`, and a meaningful fraction of sections are drawable) and, on the
//! cull side, that the frame both draws and culls ([`CullStats::is_meaningful`]).
//! The configuration (render distance, section grid, height range) is printed so
//! a millisecond number is reproducible.
//!
//! The GPU roundtrip (`gpu_world_mesher_upload_evict_roundtrip`) is `#[ignore]`d
//! and proves the *lifecycle*: [`WorldMesher::load_column`] makes sections
//! resident and drawable, and [`WorldMesher::unload_column`] frees every arena
//! span so residency returns to zero — the "free on unload" requirement, checked
//! against real allocator stats rather than asserted.

use std::collections::HashMap;
use std::hint::black_box;
use std::sync::Arc;
use std::time::Instant;

use lodestone_render::mesher::SectionSource;
use lodestone_render::world::BlockClassifier;
use lodestone_render::{
    Camera, Cell, CullStats, SpriteId, WorldMesher, build_batch, dirty_jobs, section_of,
};
use lodestone_world::{ChunkSection, PaletteKind};

const AIR: u32 = 0;
const STONE: u32 = 1;

/// A classifier where id 0 is lit air and everything else is a solid cube whose
/// sprite id is its state id.
#[derive(Debug)]
struct TerrainClassifier;

impl BlockClassifier for TerrainClassifier {
    fn classify(&self, state_id: u32, block_light: u8, sky_light: u8) -> Cell {
        if state_id == AIR {
            Cell {
                occludes: false,
                surface: None,
                block_light,
                sky_light,
            }
        } else {
            let mut c = Cell::solid(SpriteId(state_id as u16));
            c.block_light = block_light;
            c.sky_light = sky_light;
            c
        }
    }
}

/// A world backed by a section map; missing coords read as `None`.
struct MapWorld(HashMap<(i32, i32, i32), Arc<ChunkSection>>);

impl SectionSource for MapWorld {
    fn section(&self, coord: (i32, i32, i32)) -> Option<Arc<ChunkSection>> {
        self.0.get(&coord).cloned()
    }
}

/// Cheap deterministic surface height in `[16, 27]` for column `(cx, cz)`, so
/// the surface lives entirely in section y=1 and neighbouring columns differ,
/// producing real exposed side faces (not a single flat plane).
fn surface_height(cx: i32, cz: i32) -> i32 {
    let h = (cx.wrapping_mul(73_856_093) ^ cz.wrapping_mul(19_349_663)) as u32;
    16 + (h % 12) as i32
}

/// Build a section of a column: y=0 buried solid, y=1 the varying surface, y≥2
/// air. Returns `None` for a section that would be entirely air (so the source
/// reports absence exactly as a real elided all-air section does).
fn build_section(cx: i32, sy: i32, cz: i32) -> Option<Arc<ChunkSection>> {
    let height = surface_height(cx, cz);
    let base = sy * 16;
    if base >= height {
        return None; // fully above the surface — all air, elided.
    }
    let mut s = ChunkSection::new(PaletteKind::block_states(), PaletteKind::biomes(), AIR, 0);
    for ly in 0..16 {
        let world_y = base + ly as i32;
        if world_y < height {
            for x in 0..16 {
                for z in 0..16 {
                    s.set_block(x, ly, z, STONE);
                }
            }
        }
    }
    Some(Arc::new(s))
}

/// A render-distance-`rd` world over section rows `0..section_rows`.
fn build_world(rd: i32, section_rows: i32) -> MapWorld {
    let mut map = HashMap::new();
    for cx in -rd..=rd {
        for cz in -rd..=rd {
            for sy in 0..section_rows {
                if let Some(sec) = build_section(cx, sy, cz) {
                    map.insert((cx, sy, cz), sec);
                }
            }
        }
    }
    MapWorld(map)
}

/// Gather + build every column's dirty jobs for the whole world, timing the
/// build half. Returns (built sections, total quads, wall-clock seconds).
fn build_whole_world(
    world: &MapWorld,
    rd: i32,
    section_rows: i32,
    greedy: bool,
) -> (usize, u64, f64) {
    // One column-load per column reproduces the streaming path; dedupe the
    // resulting jobs so a section shared by neighbouring loads is built once.
    let mut jobs = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for cx in -rd..=rd {
        for cz in -rd..=rd {
            for job in dirty_jobs(world, cx, cz, 0..section_rows) {
                if seen.insert(job.coord) {
                    jobs.push(job);
                }
            }
        }
    }

    let start = Instant::now();
    let built = build_batch(black_box(jobs), &TerrainClassifier, greedy);
    let elapsed = start.elapsed().as_secs_f64();

    let quads: u64 = built.iter().map(|b| b.mesh.quad_count() as u64).sum();
    (built.len(), quads, elapsed)
}

#[test]
fn rd16_mesh_build_budget() {
    const RD: i32 = 16;
    const ROWS: i32 = 4; // sections 0..4 → world y 0..64
    let columns = (2 * RD + 1) * (2 * RD + 1);

    let world = build_world(RD, ROWS);
    let loaded_sections = world.0.len();

    let (built, quads, secs) = build_whole_world(&world, RD, ROWS, true);
    let ms = secs * 1e3;

    let parallel = cfg!(not(target_arch = "wasm32"));
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };
    println!("=== RD-16 mesh-build benchmark ===");
    println!("render distance:    {RD} chunks ({columns} columns)");
    println!("loaded sections:    {loaded_sections}  (non-air, over {ROWS} section rows)");
    println!("built sections:     {built}");
    println!("total quads:        {quads}");
    println!(
        "mesh build:         {ms:.2} ms  ({profile}, {} path, {:.1} sections/ms)",
        if parallel { "parallel" } else { "serial" },
        built as f64 / ms
    );

    // Anti-vacuity: the build produced real geometry, not an empty world.
    assert!(quads > 0, "the build must produce geometry");
    assert!(
        built >= loaded_sections,
        "every loaded section is meshed (some to empty): built {built} < loaded {loaded_sections}"
    );
    // With height variation, a large share of columns carry surface geometry.
    assert!(
        quads > columns as u64,
        "terrain with height variation exceeds one quad per column: {quads} <= {columns}"
    );
}

#[test]
fn rd16_frame_cull_budget() {
    // Build the scene through the real lifecycle, but on the CPU: synthesise the
    // draw regions the arena would produce so the cull budget is measurable
    // without a GPU. (The GPU residency path is covered by the #[ignore] test.)
    use lodestone_render::{ChunkSectionView, UniformLight};
    use lodestone_render::{DrawRegion, WorldScene, compute_visibility};

    const RD: i32 = 16;
    const ROWS: i32 = 4;
    let world = build_world(RD, ROWS);

    let mut scene = WorldScene::new();
    let light = UniformLight::default();
    let mut drawable_quads = 0u64;
    for (instance, (&coord, sec)) in world.0.iter().enumerate() {
        let instance = instance as u32;
        // Mesh just this section against its neighbours to get an honest quad
        // count and visibility.
        let jobs = dirty_jobs(&world, coord.0, coord.2, coord.1..coord.1 + 1);
        let quads = jobs
            .iter()
            .find(|j| j.coord == coord)
            .map(|j| {
                j.snapshot
                    .build_mesh(&TerrainClassifier, &light, true)
                    .quad_count()
            })
            .unwrap_or(0) as u32;
        let view = ChunkSectionView::new(sec.as_ref(), &TerrainClassifier, &light);
        let vis = compute_visibility(&view);
        let region = DrawRegion {
            first_index: 0,
            index_count: quads * 6,
            base_vertex: 0,
            instance,
            visible: false,
        };
        drawable_quads += u64::from(quads);
        scene.insert_section(coord, region, vis);
    }

    let camera = Camera {
        position: glam::Vec3::new(8.0, 40.0, 8.0),
        yaw: 0.0,
        pitch: 20.0,
        ..Camera::default()
    };
    assert_eq!(section_of(camera.position), (0, 2, 0));

    // Warm, then time.
    let warm = scene.plan_frame(&camera);
    let mut sink = warm.stats.drawn;
    let iters = 200;
    let start = Instant::now();
    for _ in 0..iters {
        let plan = scene.plan_frame(black_box(&camera));
        sink = sink.wrapping_add(black_box(plan.regions.len()));
    }
    black_box(sink);
    let micros = start.elapsed().as_secs_f64() * 1e6 / f64::from(iters);
    let stats: CullStats = warm.stats;

    println!("=== RD-16 frame-cull benchmark ===");
    println!("render distance:    {RD} chunks");
    println!("loaded sections:    {}", scene.loaded_len());
    println!(
        "drawable sections:  {} ({} quads total)",
        stats.drawable, drawable_quads
    );
    println!(
        "drawn:              {} ({} quads)   frustum-culled: {}   occlusion-culled: {}",
        stats.drawn, stats.drawn_quads, stats.culled_frustum, stats.culled_occlusion
    );
    println!("plan_frame:         {micros:.2} µs/frame");

    assert!(
        stats.is_meaningful(),
        "frame must draw AND cull, not draw-all/draw-none: {stats:?}"
    );
    assert!(
        stats.culled_frustum > 0,
        "the hemisphere behind the camera must be frustum-culled"
    );
}

/// GPU residency lifecycle: load makes sections resident and drawable; unload
/// frees every arena span. `#[ignore]`d — running it is an explicit request for
/// the GPU path, so it fails closed if no adapter is present.
#[test]
#[ignore = "requires a GPU adapter; run explicitly for the residency lifecycle"]
fn gpu_world_mesher_upload_evict_roundtrip() {
    let Some((device, queue)) = setup_device() else {
        panic!(
            "world_mesher_bench: no GPU adapter. This test is #[ignore]d, so running it is an \
             explicit request for the GPU residency path — run it on a machine with an adapter."
        );
    };

    const RD: i32 = 8; // smaller than the CPU bench; this proves residency, not throughput
    const ROWS: i32 = 4;
    let world = build_world(RD, ROWS);

    // Arenas sized generously for the RD-8 world.
    let mut mesher = WorldMesher::new(&device, 64 << 20, 32 << 20, true);

    let load_start = Instant::now();
    for cx in -RD..=RD {
        for cz in -RD..=RD {
            mesher.load_column(&world, &queue, &TerrainClassifier, cx, cz, 0..ROWS);
        }
    }
    let load_ms = load_start.elapsed().as_secs_f64() * 1e3;

    let loaded = mesher.scene().loaded_len();
    let resident = mesher.arena().resident_len();
    assert!(loaded > 0, "sections became scene-loaded");
    assert!(resident > 0, "surface sections became GPU-resident");
    assert!(
        resident <= loaded,
        "air sections are loaded but not resident: {resident} > {loaded}"
    );

    let camera = Camera {
        position: glam::Vec3::new(8.0, 40.0, 8.0),
        yaw: 0.0,
        pitch: 20.0,
        ..Camera::default()
    };
    let plan = mesher.plan_frame(&camera);
    assert!(
        plan.stats.is_meaningful(),
        "resident world must draw AND cull: {:?}",
        plan.stats
    );

    println!("=== GPU world-mesher residency ===");
    println!("render distance:    {RD} chunks");
    println!("load (build+upload):{load_ms:.2} ms  ({loaded} loaded, {resident} resident)");
    println!(
        "vertex arena used:  {} / {} bytes",
        mesher.arena().vertex_stats().used,
        mesher.arena().vertex_stats().capacity
    );

    // Free on unload: every span returns to the pool.
    for cx in -RD..=RD {
        for cz in -RD..=RD {
            mesher.unload_column(cx, cz, 0..ROWS);
        }
    }
    assert!(mesher.scene().is_empty(), "scene emptied on unload");
    assert_eq!(
        mesher.arena().resident_len(),
        0,
        "no section left resident after unload"
    );
    assert_eq!(
        mesher.arena().vertex_stats().used,
        0,
        "vertex arena fully reclaimed on unload"
    );
    assert_eq!(
        mesher.arena().index_stats().used,
        0,
        "index arena fully reclaimed on unload"
    );
    println!("after unload:       arenas reclaimed to 0 bytes ✓");
}

/// Bring up a headless device, or `None` if no adapter is available.
fn setup_device() -> Option<(wgpu::Device, wgpu::Queue)> {
    pollster::block_on(async {
        let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle());
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: None,
                apply_limit_buckets: false,
            })
            .await
            .ok()?;
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("world_mesher_bench device"),
                required_features: wgpu::Features::empty(),
                required_limits: adapter.limits(),
                ..Default::default()
            })
            .await
            .ok()?;
        Some((device, queue))
    })
}
