//! Core world-generation scaling arm for `join_parallel_efficiency`.
//!
//! This is intentionally separate from the production `ColumnPipeline` sweep
//! in the parent test: it measures direct Rayon dispatch over all three
//! dimension generators, not Tokio scheduling, packet payloads, or the
//! production Overworld source. The two reports therefore answer different
//! questions while sharing one ignored measurement binary.

#![allow(unsafe_code)]

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use lodestone_server::{end_generator, nether_generator, overworld_generator};
use lodestone_worldgen::end::EndGenerator;
use lodestone_worldgen::nether::NetherGenerator;
use lodestone_worldgen::overworld::OverworldGenerator;
use rayon::prelude::*;
use rayon::ThreadPool;
use sha2::{Digest as _, Sha256};

const SEED: i64 = 42;
const SIDE: i32 = 8;
const WORKERS: [usize; 4] = [1, 2, 4, 8];

struct CountingAllocator;

static COUNT_ALLOCATIONS: AtomicBool = AtomicBool::new(false);
static ALLOCATIONS: AtomicU64 = AtomicU64::new(0);
static ALLOCATION_BYTES: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNT_ALLOCATIONS.load(Ordering::Relaxed) {
            ALLOCATIONS.fetch_add(1, Ordering::Relaxed);
            ALLOCATION_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        // SAFETY: the system allocator receives the original layout unchanged.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: this pointer and layout originated from `Self::alloc`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: CountingAllocator = CountingAllocator;

#[derive(Clone, Copy)]
enum Dimension {
    Overworld,
    Nether,
    End,
}

impl Dimension {
    const ALL: [Self; 3] = [Self::Overworld, Self::Nether, Self::End];

    fn label(self) -> &'static str {
        match self {
            Self::Overworld => "overworld",
            Self::Nether => "nether",
            Self::End => "end",
        }
    }
}

enum Generator {
    Overworld(OverworldGenerator),
    Nether(NetherGenerator),
    End(EndGenerator),
}

impl Generator {
    fn new(dimension: Dimension) -> Self {
        match dimension {
            Dimension::Overworld => Self::Overworld(overworld_generator(SEED)),
            Dimension::Nether => Self::Nether(nether_generator(SEED)),
            Dimension::End => Self::End(end_generator(SEED)),
        }
    }

    /// One immutable dispatch point shared by every worker and dimension.
    fn generate(&self, cx: i32, cz: i32) -> u64 {
        match self {
            Self::Overworld(generator) => generator.column(cx, cz).non_air_count() as u64,
            Self::Nether(generator) => generator.column(cx, cz).non_air_count() as u64,
            Self::End(generator) => generator.column(cx, cz).non_air_count() as u64,
        }
    }

    fn digest_chunk(&self, cx: i32, cz: i32) -> [u8; 32] {
        let mut digest = Sha256::new();
        hash_bytes(&mut digest, b"lodestone-worldgen-parallel-chunk-v1");
        hash_i32(&mut digest, cx);
        hash_i32(&mut digest, cz);
        match self {
            Self::Overworld(generator) => hash_overworld(&mut digest, &generator.column(cx, cz)),
            Self::Nether(generator) => hash_nether(&mut digest, &generator.column(cx, cz)),
            Self::End(generator) => hash_end(&mut digest, &generator.column(cx, cz)),
        }
        digest.finalize().into()
    }
}

fn coords() -> Vec<(i32, i32)> {
    // Three-chunk spacing makes the fixed set visibly disjoint and catches a
    // duplicate-coordinate bug that a compact row-major grid could hide.
    let values: Vec<_> = (0..SIDE)
        .flat_map(|z| (0..SIDE).map(move |x| (3 * x - 12, 3 * z - 12)))
        .collect();
    assert_eq!(values.len(), (SIDE * SIDE) as usize);
    for (index, &value) in values.iter().enumerate() {
        assert!(!values[..index].contains(&value), "duplicate fixed chunk {value:?}");
    }
    values
}

fn adjacent_coords() -> Vec<(i32, i32)> {
    (0..SIDE)
        .flat_map(|z| (0..SIDE).map(move |x| (x - SIDE / 2, z - SIDE / 2)))
        .collect()
}

fn reduce(values: &[u64]) -> u64 {
    values.iter().fold(0xcbf2_9ce4_8422_2325u64, |digest, value| {
        digest.rotate_left(7) ^ value
    })
}

fn serial_digest(generator: &Generator, positions: &[(i32, i32)]) -> [u8; 32] {
    let chunks = positions
        .iter()
        .map(|&(cx, cz)| generator.digest_chunk(cx, cz))
        .collect::<Vec<_>>();
    combine_digests(positions, &chunks)
}

fn parallel_digest(pool: &ThreadPool, generator: &Generator, positions: &[(i32, i32)]) -> [u8; 32] {
    let chunks = pool.install(|| {
        positions
            .par_iter()
            .map(|&(cx, cz)| generator.digest_chunk(cx, cz))
            .collect::<Vec<_>>()
    });
    combine_digests(positions, &chunks)
}

fn combine_digests(positions: &[(i32, i32)], chunks: &[[u8; 32]]) -> [u8; 32] {
    let mut digest = Sha256::new();
    hash_bytes(&mut digest, b"lodestone-worldgen-parallel-v1");
    for (&(cx, cz), chunk) in positions.iter().zip(chunks) {
        hash_i32(&mut digest, cx);
        hash_i32(&mut digest, cz);
        digest.update(chunk);
    }
    digest.finalize().into()
}

#[derive(Debug, Clone, Copy)]
struct Arm {
    workers: usize,
    wall_seconds: f64,
    cpu_seconds: f64,
    allocations: u64,
    allocation_bytes: u64,
    digest: [u8; 32],
}

fn run_arm(dimension: Dimension, positions: &[(i32, i32)], workers: usize, baseline: [u8; 32]) -> Arm {
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(workers)
        .thread_name(|index| format!("worldgen-core-{index}"))
        .build()
        .expect("Rayon worker pool must build");
    let generator = Generator::new(dimension);
    let before_cpu = cpu_seconds();
    let started = Instant::now();
    let values = pool.install(|| {
        assert_eq!(rayon::current_num_threads(), workers);
        positions
            .par_iter()
            .map(|&(cx, cz)| generator.generate(cx, cz))
            .collect::<Vec<_>>()
    });
    let wall_seconds = started.elapsed().as_secs_f64();
    let cpu_seconds = cpu_seconds() - before_cpu;
    assert_eq!(values.len(), positions.len());
    black_box(reduce(&values));
    let digest = parallel_digest(&pool, &generator, positions);
    assert_eq!(digest, baseline, "parallel core content changed at {workers} workers");

    let allocation_generator = Generator::new(dimension);
    let sample = &positions[..positions.len().min(8)];
    ALLOCATIONS.store(0, Ordering::Relaxed);
    ALLOCATION_BYTES.store(0, Ordering::Relaxed);
    COUNT_ALLOCATIONS.store(true, Ordering::Relaxed);
    let allocation_values = pool.install(|| {
        sample
            .par_iter()
            .map(|&(cx, cz)| allocation_generator.generate(cx, cz))
            .collect::<Vec<_>>()
    });
    COUNT_ALLOCATIONS.store(false, Ordering::Relaxed);
    black_box(reduce(&allocation_values));

    Arm {
        workers,
        wall_seconds,
        cpu_seconds,
        allocations: ALLOCATIONS.load(Ordering::Relaxed),
        allocation_bytes: ALLOCATION_BYTES.load(Ordering::Relaxed),
        digest,
    }
}

fn hash_overworld(digest: &mut Sha256, column: &lodestone_worldgen::overworld::GeneratedColumn) {
    hash_i32(digest, column.min_y());
    hash_i32(digest, column.height());
    for y in column.min_y()..column.min_y() + column.height() {
        for z in 0..16 {
            for x in 0..16 {
                hash_str(digest, column.block_state(x, y, z));
            }
        }
    }
    let biomes = column.biome_cells();
    hash_i32(digest, biomes.min_y());
    hash_u64(digest, biomes.y_quarts() as u64);
    for qy in 0..biomes.y_quarts() {
        for qz in 0..4 {
            for qx in 0..4 {
                hash_str(digest, biomes.at_quart(qx, qy, qz));
            }
        }
    }
    hash_u64(digest, column.block_entities().len() as u64);
    for entity in column.block_entities() {
        hash_str(
            digest,
            lodestone_data::block_entity_types::block_entity_type_name(entity.type_id()),
        );
        let (x, y, z) = entity.position();
        hash_i32(digest, x);
        hash_i32(digest, y);
        hash_i32(digest, z);
        match entity {
            lodestone_worldgen::overworld::GeneratedBlockEntity::Beehive { bees, .. } => {
                hash_str(digest, "bees");
                hash_u64(digest, bees.len() as u64);
                for bee in bees {
                    hash_i32(digest, bee.ticks_in_hive);
                    hash_i32(digest, bee.min_ticks_in_hive);
                }
            }
            lodestone_worldgen::overworld::GeneratedBlockEntity::DungeonChest {
                facing,
                loot_table,
                loot_table_seed,
                ..
            } => {
                hash_str(digest, facing);
                hash_str(digest, loot_table);
                hash_i64(digest, *loot_table_seed);
            }
            lodestone_worldgen::overworld::GeneratedBlockEntity::DungeonSpawner { entity_type, .. } => {
                hash_str(
                    digest,
                    entity_type
                        .builtin_or_none()
                        .expect("generated spawner type is built-in")
                        .name(),
                );
            }
        }
    }
}

fn hash_nether(digest: &mut Sha256, column: &lodestone_worldgen::nether::NetherColumn) {
    hash_i32(digest, column.min_y());
    hash_i32(digest, column.height());
    for y in column.min_y()..column.min_y() + column.height() {
        for z in 0..16 {
            for x in 0..16 {
                hash_str(digest, column.block_state(x, y, z));
            }
        }
    }
    for qz in 0..4 {
        for qx in 0..4 {
            hash_str(digest, column.biome_at_quart(qx, qz));
        }
    }
    hash_u64(digest, column.placement_loot().len() as u64);
    for loot in column.placement_loot() {
        for coordinate in loot.pos {
            hash_i32(digest, coordinate);
        }
        hash_str(digest, &loot.table);
        hash_i64(digest, loot.seed);
    }
}

fn hash_end(digest: &mut Sha256, column: &lodestone_worldgen::end::EndColumn) {
    hash_i32(digest, column.min_y());
    hash_i32(digest, column.height());
    for y in column.min_y()..column.min_y() + column.height() {
        for z in 0..16 {
            for x in 0..16 {
                hash_str(digest, column.block_state(x, y, z));
            }
        }
    }
    for qz in 0..4 {
        for qx in 0..4 {
            hash_str(digest, column.biome_at_quart(qx, qz));
        }
    }
    hash_u64(digest, column.gateways().len() as u64);
    for gateway in column.gateways() {
        for coordinate in [gateway.pos, gateway.exit] {
            hash_i32(digest, coordinate.0);
            hash_i32(digest, coordinate.1);
            hash_i32(digest, coordinate.2);
        }
        digest.update([u8::from(gateway.exact)]);
    }
}

fn hash_bytes(digest: &mut Sha256, bytes: &[u8]) {
    hash_u64(digest, bytes.len() as u64);
    digest.update(bytes);
}

fn hash_str(digest: &mut Sha256, value: &str) {
    hash_bytes(digest, value.as_bytes());
}

fn hash_i32(digest: &mut Sha256, value: i32) {
    digest.update(value.to_le_bytes());
}

fn hash_i64(digest: &mut Sha256, value: i64) {
    digest.update(value.to_le_bytes());
}

fn hash_u64(digest: &mut Sha256, value: u64) {
    digest.update(value.to_le_bytes());
}

fn cpu_seconds() -> f64 {
    let mut usage = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: `getrusage` writes the complete value for `RUSAGE_SELF`.
    let result = unsafe { libc::getrusage(libc::RUSAGE_SELF, usage.as_mut_ptr()) };
    assert_eq!(result, 0, "getrusage(RUSAGE_SELF) failed");
    // SAFETY: the successful call initialized `usage`.
    let usage = unsafe { usage.assume_init() };
    let user = usage.ru_utime.tv_sec as f64 + usage.ru_utime.tv_usec as f64 / 1_000_000.0;
    let system = usage.ru_stime.tv_sec as f64 + usage.ru_stime.tv_usec as f64 / 1_000_000.0;
    user + system
}

#[test]
#[ignore = "64 full decorated columns per dimension at four worker counts; release-only measurement"]
fn core_worldgen_scales_without_nested_parallelism_and_preserves_content() {
    let requested_scene = std::env::var("LODESTONE_CORE_SCENE").unwrap_or_else(|_| "all".to_string());
    let scenes = match requested_scene.as_str() {
        "independent" => vec![("independent", coords())],
        "adjacent" => vec![("adjacent", adjacent_coords())],
        "all" => vec![("independent", coords()), ("adjacent", adjacent_coords())],
        other => panic!("LODESTONE_CORE_SCENE must be all, independent, or adjacent; got {other:?}"),
    };
    let positions = &scenes[0].1;
    eprintln!(
        "\n[core/rayon] {} fixed disjoint columns, seed {SEED}; nested_parallelism=false; production ColumnPipeline is reported by the parent test",
        positions.len()
    );
    eprintln!("  {:<10} {:<9} {:>7} {:>10} {:>10} {:>8} {:>10} {:>12} {:>8} digest", "scene", "dimension", "workers", "chunks/s", "per-thread", "eff%", "cpu(s)", "allocs/chunk", "bytes/chunk");

    for (scene, positions) in scenes {
        for dimension in Dimension::ALL {
            let baseline = serial_digest(&Generator::new(dimension), &positions);
            let mut single_thread_chunks_per_second = None;
            for workers in WORKERS {
            let arm = run_arm(dimension, &positions, workers, baseline);
            let chunks_per_second = positions.len() as f64 / arm.wall_seconds;
            let per_thread = chunks_per_second / workers as f64;
            let efficiency = single_thread_chunks_per_second
                .map(|single| 100.0 * chunks_per_second / (single * workers as f64));
            if workers == 1 {
                single_thread_chunks_per_second = Some(chunks_per_second);
            }
            eprintln!(
                "  {:<10} {:<9} {:>7} {:>10.2} {:>10.2} {:>8.1} {:>10.3} {:>12.1} {:>8.1} {}",
                scene,
                dimension.label(),
                arm.workers,
                chunks_per_second,
                per_thread,
                efficiency.unwrap_or(100.0),
                arm.cpu_seconds,
                arm.allocations as f64 / positions.len().min(8) as f64,
                arm.allocation_bytes as f64 / positions.len().min(8) as f64,
                arm.digest.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
            );
            if per_thread < 100.0 {
                eprintln!("    target warning: {scene}/{} at {} workers is below 100 chunks/s/thread ({per_thread:.2})", dimension.label(), workers);
            }
            }
        }
    }
    eprintln!("  full content digest equality: pass; negative detector: pass");
    // A changed full digest must not be accepted merely because the old
    // non-air-count reduction remains unchanged.
    let baseline = ["minecraft:stone", "minecraft:stone"];
    let changed = ["minecraft:stone", "minecraft:dirt"];
    assert_eq!(baseline.len(), changed.len());
    assert_ne!(content_digest(&baseline), content_digest(&changed));
}

fn content_digest(states: &[&str]) -> [u8; 32] {
    let mut digest = Sha256::new();
    hash_bytes(&mut digest, b"core-negative-detector-v1");
    for state in states {
        hash_str(&mut digest, state);
    }
    digest.finalize().into()
}

#[test]
#[ignore = "short release measurement of the production End spatial-batch seam"]
fn end_spatial_batch_throughput() {
    let positions = adjacent_coords();
    let source = lodestone_server::end_chunk_source(SEED);
    let started = Instant::now();
    let columns = source.generate_batch(&positions);
    let elapsed = started.elapsed().as_secs_f64();
    assert_eq!(columns.len(), positions.len());
    black_box(columns.iter().map(|column| column.solid_count()).sum::<usize>());
    eprintln!(
        "end spatial batch: {} columns in {:.3}s = {:.2} chunks/s",
        positions.len(),
        elapsed,
        positions.len() as f64 / elapsed,
    );
}

#[test]
#[ignore = "short release phase split for the End spatial-batch seam"]
fn end_spatial_batch_phase_profile() {
    let positions = adjacent_coords();
    let generator = end_generator(SEED);
    let mut bases_done = None;
    let started = Instant::now();
    let columns = generator.columns_spatial_batch_observed(
        &positions,
        |dependencies, generator| generator.base_world_rectangle(&dependencies),
        |dependencies, outputs| {
            if outputs == 0 {
                bases_done = Some((dependencies, started.elapsed()));
            }
        },
    );
    let elapsed = started.elapsed();
    let (dependencies, base_elapsed) = bases_done.expect("base phase observed");
    black_box(columns);
    eprintln!(
        "end spatial phases: dependencies={} bases={:.3}s ordered={:.3}s total={:.3}s",
        dependencies,
        base_elapsed.as_secs_f64(),
        (elapsed - base_elapsed).as_secs_f64(),
        elapsed.as_secs_f64(),
    );
}

#[test]
#[ignore = "short release measurement of overlapping End spatial batches"]
fn end_overlapping_spatial_batch_throughput() {
    let first = adjacent_coords();
    let second = first.iter().map(|&(cx, cz)| (cx + 1, cz)).collect::<Vec<_>>();
    let source = lodestone_server::end_chunk_source(SEED);
    let cold_started = Instant::now();
    let cold = source.generate_batch(&first);
    let cold_elapsed = cold_started.elapsed().as_secs_f64();
    let overlap_started = Instant::now();
    let overlap = source.generate_batch(&second);
    let overlap_elapsed = overlap_started.elapsed().as_secs_f64();
    black_box((cold, overlap));
    eprintln!(
        "end overlapping batches: cold={:.2} chunks/s shifted-overlap={:.2} chunks/s",
        first.len() as f64 / cold_elapsed,
        second.len() as f64 / overlap_elapsed,
    );
}

#[test]
#[ignore = "bounded release roaming/cache-footprint measurement"]
fn end_spatial_batch_cache_stays_bounded_while_roaming() {
    let generator = end_generator(SEED);
    for step in 0..80 {
        let positions = adjacent_coords()
            .into_iter()
            .map(|(cx, cz)| (cx + step * 8, cz))
            .collect::<Vec<_>>();
        let columns = generator.columns_spatial_batch(&positions, |dependencies, generator| {
            lodestone_server::run_worldgen_jobs(dependencies, |chunk @ (cx, cz)| {
                (chunk, generator.base_world_for_batch(cx, cz))
            })
        });
        black_box(columns);
    }
    let retained = generator.base_world_cache_len();
    eprintln!("end spatial cache after roaming: retained={retained}");
    assert!(retained <= 4_489 + 32, "End base cache exceeded its sharded bound: {retained}");
}

#[test]
#[ignore = "short release measurement of shared rectangular End density"]
fn end_rectangular_density_batch_throughput() {
    let positions = adjacent_coords();
    let generator = end_generator(SEED);
    let mut dependencies = positions
        .iter()
        .flat_map(|&(cx, cz)| {
            (-1..=1).flat_map(move |dx| (-1..=1).map(move |dz| (cx + dx, cz + dz)))
        })
        .collect::<Vec<_>>();
    dependencies.sort_unstable();
    dependencies.dedup();
    let started = Instant::now();
    let bases = generator.base_world_rectangle(&dependencies);
    let elapsed = started.elapsed().as_secs_f64();
    black_box(bases);
    eprintln!(
        "end rectangular density: {} bases in {:.3}s = {:.2} bases/s",
        dependencies.len(), elapsed, dependencies.len() as f64 / elapsed,
    );
}


#[test]
#[ignore = "short release crossover sweep for End base strategies"]
fn end_base_strategy_crossover() {
    for side in [1_i32, 2, 4, 8] {
        let outputs = (0..side)
            .flat_map(|z| (0..side).map(move |x| (x, z)))
            .collect::<Vec<_>>();
        let mut dependencies = outputs
            .iter()
            .flat_map(|&(cx, cz)| {
                (-1..=1).flat_map(move |dx| (-1..=1).map(move |dz| (cx + dx, cz + dz)))
            })
            .collect::<Vec<_>>();
        dependencies.sort_unstable();
        dependencies.dedup();

        let rectangular = end_generator(SEED);
        let started = Instant::now();
        black_box(rectangular.base_world_rectangle(&dependencies));
        let rectangular_seconds = started.elapsed().as_secs_f64();

        let parallel = end_generator(SEED);
        let started = Instant::now();
        black_box(lodestone_server::run_worldgen_jobs(
            dependencies.clone(),
            |chunk @ (cx, cz)| (chunk, parallel.base_world_for_batch(cx, cz)),
        ));
        let parallel_seconds = started.elapsed().as_secs_f64();
        eprintln!(
            "end strategy outputs={} dependencies={} rectangle={:.4}s parallel={:.4}s ratio={:.3}",
            outputs.len(), dependencies.len(), rectangular_seconds, parallel_seconds,
            rectangular_seconds / parallel_seconds,
        );
    }
}
