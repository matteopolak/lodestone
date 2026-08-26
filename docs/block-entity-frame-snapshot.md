# Block-entity frame snapshot

## What it is

The block-entity frame snapshot is one immutable, camera-scoped gather of block-entity positions, block states, and packed entity light. It lets state-driven renderers share the same world read instead of independently scanning loaded chunks and reacquiring the chunk-world lock for every visible object's light.

## How it works

`block_entity_frame_snapshot` runs once after the frame's render camera is resolved. It calls `loaded_chunks()` before acquiring the chunk-world read guard, walks each loaded column's block-entity records, applies vanilla's fixed 64-block centre-distance cutoff, and records `{position, state_id, light}` for each surviving candidate.

Light is sampled directly from the candidate's already-borrowed `LoadedChunk`. Missing sky-light data uses the same dimension-aware `SkyDefault` policy as terrain and `entity_light_at`; missing block light resolves to zero. Out-of-world candidates retain the previous full-bright fallback.

The snapshot is held by `Arc` and installed into the renderer closures for chests, bells, shulker boxes, lecterns, enchanting tables, conduits, and copper golem statues. Each resolver filters the shared compact slice by block state and attaches its own animation state. The renderer still owns the closures, but none of these closures reads the live world again during the frame.

NBT-dependent families are intentionally outside this first slice. Signs, player heads, banners, decorated pots, item-bearing block entities, spawners, beacons, and portal/gateway renderers retain their specialised gathers. Copying arbitrary raw NBT into the compact snapshot would trade lock/scanning time for allocation and cache pressure; a future extension should store only already-decoded typed payloads for the specific matching state.

The 2026-08-26 Hermitcraft Samply comparison measured `prepare_block_entities` at 6.97% before and 4.19% after, while `entity_light_at` fell from 3.59% to 2.33%. The new shared gather accounted for 0.76%. Matching ordinary trials did not establish a whole-frame improvement because unrelated `sim_tick` variance exceeded the saved render-preparation time; see [`client-frame-performance-2026-08-25.md`](./client-frame-performance-2026-08-25.md).

## How to change it

The snapshot record and gather live in `crates/lodestone-shell/src/block_entities.rs`. Snapshot-backed spawn resolvers in that file must remain pure over `&BlockEntityFrameSnapshot` plus their animation tracker; accepting `SharedHandle` in one of those functions would reintroduce the hidden world-lock path this feature removes.

`Sim` creates and shares the `Arc` in `crates/lodestone-shell/src/sim/render_sources.rs`. `crates/lodestone-shell/src/app/redraw.rs` must create exactly one snapshot from `render_camera.position` and clone only its `Arc` for each installed source.

When adding an NBT-driven renderer, parse only the payload used by a matching block state while the world is locked. Do not add raw `Nbt` to every candidate. Preserve deterministic position sorting in each final typed spawn vector; snapshot iteration follows chunk storage order and is not a rendering-order contract.

Run the focused `frame_snapshot_tests` test, the shell all-target and version-free checks, then a matching release Samply capture. A source disappearing from the profile is not enough: compare the replacement snapshot's inclusive cost and ordinary frame phases too.

## Configuration

- View cutoff: `VIEW_DISTANCE` in `block_entities.rs`, currently vanilla's fixed 64 blocks.
- Sky-light fallback: the current dimension's `SkyDefault`.
- Benchmark workload: `python3 scripts/client-frame-benchmark.py --workload megaworld --debug-overlay closed --samply`.
- There is no runtime toggle; snapshot extraction is the normal live render path.

## Dependencies

- `lodestone-client::ClientHandle` for loaded chunks, world dimensions, and player dimension facts.
- `lodestone-world::LoadedChunk` for block-entity records, block states, and light columns.
- `lodestone-render::SkyDefault` and packed entity-light conventions.
- The per-type animation trackers in `block_entities.rs`.
- The render-source closure seam in `sim/render_sources.rs` and `gpu/sources.rs`.
