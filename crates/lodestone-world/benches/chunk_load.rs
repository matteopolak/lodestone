//! Chunk-insertion throughput for the real per-chunk multiplayer consumer
//! (the client-side chunk-loading half of this crate's per-chunk throughput
//! benchmarks): [`World::load`], the
//! exact call `protocol/v26-2/src/adapter.rs`'s `LEVEL_CHUNK_WITH_LIGHT` handler
//! makes once a packet is fully decoded —
//! `world.load(pos, LoadedChunk::new(chunk.column, chunk.light,
//! chunk.heightmaps, chunk.block_entities))`, moving every part with no clone.
//!
//! # Why this bench does not also compute light
//!
//! `lodestone-shell/src/net.rs` states the client's own invariant explicitly,
//! in its module doc: **"MP consumes server light; SP computes it. Do not run
//! `compute_column_light` on live columns."** For the wire path this bench
//! measures, light and heightmaps arrive *already decoded* — the post-decode
//! "light propagation" work is exactly zero here, by design, not by
//! omission. The other per-chunk consumer —
//! `compute_column_light`, which `lodestone-shell/src/worldgen.rs` calls for
//! every locally generated (singleplayer) column — gets its own dedicated
//! `light_propagation` bench in this same crate. Heightmap *decode* (the wire
//! parse, as opposed to this bench's plain struct move) is `heightmap_decode`,
//! also in this directory: `Heightmaps::decode` is called directly by
//! `protocol/v26-2/src/packets/chunk.rs`'s `decode_heightmaps`, upstream of the
//! `World::load` this bench measures.
//!
//! # What would make this measurement a lie
//!
//! **Duration species.** `World::load` is a `HashMap::insert`, so calling it
//! with an ever-growing set of distinct positions across thousands of
//! criterion iterations would (a) let the map grow without bound, memory the
//! bench was never meant to use, and (b) drift the number as the map
//! rehashes/grows — iteration 1 and iteration 50,000 would not be measuring
//! the same thing. Real chunk streaming does not grow forever either: as the
//! player moves, the trailing edge unloads at roughly the rate the leading
//! edge loads (see `World`'s own module doc). This bench models that steady
//! state by cycling a fixed ring of positions (a 5×5 patch, the same shape
//! `lodestone-worldgen`'s `generation` bench uses for the same reason), so
//! after the first lap every `load` call replaces an existing entry rather
//! than growing the map — bounded, representative of steady-state streaming,
//! and safe for criterion's repeated-iteration model.
//!
//! **World species.** Insertion cost does not depend on chunk *content* the
//! way light propagation or pathfinding does (a `HashMap::insert` of an
//! already-built `LoadedChunk` costs the same regardless of what is inside
//! it), so there is no "vacuous scene" failure mode to guard against here in
//! the way `light_propagation.rs` must. The realistic terrain/light/heightmap
//! shapes below are used anyway, for the same reason `chunk_light_decode`
//! gives in `lodestone-v26-2`: an honest fixture is worth having even where
//! content-invariance means it is not load-bearing for correctness of the
//! measurement.
//!
//! Run with: `cargo bench -p lodestone-world --bench chunk_load`

mod support;

use std::hint::black_box;
use std::time::Instant;

use criterion::{Criterion, criterion_group, criterion_main};
use lodestone_core::{Nbt, NbtTag};
use lodestone_world::{
    BlockEntity, ChunkColumn, ChunkPos, ColumnLight, Heightmap, Heightmaps, LoadedChunk,
    NibbleArray, PaletteKind, World,
};

const MIN_Y: i32 = -64;
const SECTIONS: usize = 24; // 1.18+ overworld: y = -64..320.
const WORLD_HEIGHT: u32 = 384;

/// Same shape as `tests/memory.rs`'s `realistic_terrain_column`: solid stone
/// below the surface, a varied surface band, air above — "what a player
/// standing in the world actually holds", not an edge case.
fn realistic_terrain_column() -> ChunkColumn {
    let mut col = ChunkColumn::new(
        MIN_Y,
        SECTIONS,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        0,
        0,
    );
    let stone = 1u32;
    for y in MIN_Y..40 {
        for z in 0..16 {
            for x in 0..16 {
                col.set_block(x, y, z, stone);
            }
        }
    }
    for y in 40..48 {
        for z in 0..16 {
            for x in 0..16 {
                let id = 1 + ((x + z + (y as usize)) % 6) as u32;
                col.set_block(x, y, z, id);
            }
        }
    }
    col
}

/// A lit, terrain-following light column: full sky above the surface, none
/// below, with the boundary sections carrying real per-cell arrays rather than
/// uniform tags — the shape a resident, lit chunk actually carries on the
/// wire, matching `lodestone-v26-2`'s `chunk_light_decode` fixture for the same
/// reason (a uniform-only column would make container access nearly free).
fn realistic_light() -> ColumnLight {
    let mut light = ColumnLight::new(SECTIONS);
    let n = light.light_section_count();
    for i in 0..n {
        for index in 0..NibbleArray::LEN {
            let sky = if i >= 9 {
                15u8.saturating_sub(((index / 256) % 4) as u8)
            } else {
                0
            };
            light.set_sky_light(i, index, sky);
            if i < 9 && index % 173 == 0 {
                light.set_block_light(i, index, 12);
            }
        }
    }
    light
}

/// Two heightmap types (MOTION_BLOCKING/WORLD_SURFACE-shaped) with genuinely
/// varied per-column heights, matching the surface band `realistic_terrain_column`
/// carves.
fn realistic_heightmaps() -> Heightmaps {
    let mut maps = Heightmaps::new();
    let mut motion = Heightmap::new(WORLD_HEIGHT);
    let mut surface = Heightmap::new(WORLD_HEIGHT);
    for x in 0..16 {
        for z in 0..16 {
            let h = (40 + (x + z) % 8) as i32 - MIN_Y; // height above min_y
            motion.set(x, z, h as u32);
            surface.set(x, z, (h + 1) as u32);
        }
    }
    maps.insert(0, motion);
    maps.insert(4, surface);
    maps
}

/// A couple of block entities — a container with items, a sign with text —
/// so the bench moves the same shape of `Vec<BlockEntity>` a real column
/// carries rather than an empty one.
fn realistic_block_entities() -> Vec<BlockEntity> {
    vec![
        BlockEntity {
            rel_x: 3,
            rel_z: 9,
            y: 44,
            type_id: 12, // opaque registry id at this layer
            nbt: Nbt::Compound(vec![(
                "Items".to_string(),
                Nbt::List {
                    element_type: NbtTag::Compound,
                    elements: vec![],
                },
            )]),
        },
        BlockEntity {
            rel_x: 10,
            rel_z: 2,
            y: 45,
            type_id: 7,
            nbt: Nbt::Compound(vec![("Text1".to_string(), Nbt::String("\"hi\"".to_string()))]),
        },
    ]
}

fn realistic_chunk() -> LoadedChunk {
    LoadedChunk::new(
        realistic_terrain_column(),
        realistic_light(),
        realistic_heightmaps(),
        realistic_block_entities(),
    )
}

/// A bounded ring of positions (5×5 = 25), matching `lodestone-worldgen`'s
/// `generation` bench's patch size and — critically — its reason for being
/// bounded: see the module doc's "duration species" note.
fn ring_positions() -> Vec<ChunkPos> {
    (-2..=2)
        .flat_map(|cz| (-2..=2).map(move |cx| ChunkPos::new(cx, cz)))
        .collect()
}

fn bench_chunk_insertion(c: &mut Criterion) {
    let positions = ring_positions();
    let mut world = World::new();

    // Warm the map to its steady-state size before timing anything, so both
    // the diagnostic loop and criterion's own loop start from the same
    // bounded-map condition.
    for &pos in &positions {
        world.load(pos, realistic_chunk());
    }

    let scene = format!(
        "5x5 ring ({} positions), realistic terrain/light/heightmaps/block-entities",
        positions.len()
    );

    // One-shot diagnostic: median load() cost cycling the ring, recorded with
    // metadata.
    let n = positions.len();
    let mut per_load_us: Vec<f64> = Vec::with_capacity(n * 4);
    for _lap in 0..4 {
        for &pos in &positions {
            let chunk = realistic_chunk();
            let t = Instant::now();
            black_box(world.load(pos, chunk));
            per_load_us.push(t.elapsed().as_secs_f64() * 1e6);
        }
    }
    per_load_us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let median = per_load_us[per_load_us.len() / 2];
    support::record(support::Record {
        bench: "chunk_load",
        metric: "load_median_us",
        scene: &scene,
        value: median,
        unit: "us",
    });
    println!(
        "World::load median: {median:.3} us/call over {} calls ({})",
        per_load_us.len(),
        scene
    );
    assert_eq!(
        world.len(),
        positions.len(),
        "the ring must stay bounded, never grow across the diagnostic loop"
    );

    // Criterion's own headline number over the same bounded ring. Building
    // the `LoadedChunk` happens in `iter_batched`'s `setup` closure, which
    // criterion excludes from the timed region — otherwise this would (and,
    // caught while writing this bench, briefly did) measure chunk
    // *construction* dominated by ~2,000 `set_block` calls, not the
    // `World::load` insertion this bench is named for: the one-shot
    // diagnostic above reported ~2 us/call while a first draft that built the
    // chunk inside `b.iter` reported ~850 us/call for the identical
    // operation, a 450x gap that was purely measurement error.
    //
    // A second, subtler measurement trap was caught the same way: `load`
    // returns the *evicted* chunk (`Option<LoadedChunk>`, `None` only on the
    // very first insert at a position). `adapter.rs`'s real call site
    // (`world.load(pos, LoadedChunk::new(...));`) discards that return value
    // as a bare statement, so — per ordinary Rust drop timing — the evicted
    // chunk's `Drop` (24 `Arc<ChunkSection>` decrements, a `Vec<BlockEntity>`
    // with recursive `Nbt` drops, the light/heightmap arrays) runs
    // synchronously as part of *that* statement, before the next line
    // executes. A routine closure that just returns `world.load(...)` does
    // **not** measure that: criterion's `iter_batched` stops the clock before
    // dropping whatever the routine returns, so the evicted chunk would be
    // freed *after* timing stopped — silently cheaper than the real call site
    // by exactly one chunk's worth of drop cost. `drop(...)` inside the
    // closure, before it returns `()`, keeps that cost inside the timed
    // region, matching both the diagnostic above and the real adapter call.
    let idx = std::cell::Cell::new(0usize);
    c.bench_function("world/chunk_insertion", |b| {
        b.iter_batched(
            || {
                let i = idx.get();
                idx.set(i + 1);
                (positions[i % positions.len()], realistic_chunk())
            },
            |(pos, chunk)| {
                let evicted = world.load(black_box(pos), black_box(chunk));
                drop(black_box(evicted));
            },
            // `PerIteration`, not `SmallInput`: `SmallInput` tells criterion the
            // *setup* is cheap enough to batch many together ahead of the timed
            // region, but `realistic_chunk()` is not cheap (~2,000 `set_block`
            // calls) — measured while writing this bench, `SmallInput` let
            // criterion build batches of hundreds of full realistic chunks in
            // one `Vec` right before timing started, and the resulting
            // allocator/cache pressure bled into the "timed" region as wild
            // variance (a single-run point estimate swinging from ~3 us to
            // ~330 us with no code change). `PerIteration` forces batch size 1,
            // so setup and the timed routine strictly alternate one chunk at a
            // time, matching how `World::load` is actually called.
            criterion::BatchSize::PerIteration,
        )
    });

    assert_eq!(
        world.len(),
        positions.len(),
        "the ring must stay bounded after criterion's own loop too"
    );
}

criterion_group!(benches, bench_chunk_insertion);
criterion_main!(benches);
