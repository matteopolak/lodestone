# Design: Unified world render snapshot

## What it is

This change replaces Lodestone's collection of independently sampled world-render
sources with one immutable, frame-scoped snapshot. The snapshot is built once from
the post-simulation world state and is then shared by block-entity, map, entity-item,
particle-adjacent, and diagnostic render consumers without reacquiring the world or
deep-cloning unchanged data.

The first implementation is synchronous. It removes repeated work without adding a
frame of latency, speculative ticks, or an invalidation protocol whose correctness is
harder to prove than the current renderer.

## Measured reason for the design

The decision is based on the presymbolicated Samply capture
`bench-results/profiles/megaworld-closed-20260825-210124.json.gz`, recorded against
the Java-backed render-distance-24 megaworld in fullscreen on the built-in 3024 x
1898 display. `scripts/profile-cost-table.py` joined 17,463 raw addresses through
Samply's sidecar and weighted the main thread by `threadCPUDelta` rather than sample
count.

The important inclusive costs were:

| Main-thread subtree | CPU share | Consequence |
|---|---:|---|
| `Sim::step` | 24.32% | Keep simulation exact; optimise its measured relight/fold islands separately. |
| `Sim::maps` | 14.00% | Stop deep-cloning the complete map store twice per frame. |
| `reachable_from_camera` | 9.58% | Preserve the existing cache, then fix graph-generation churn if it remains hot. |
| all `block_entities::*` gather samples, unioned | 7.74% | Replace the many independent loaded-chunk scans with one gather. |
| `Queue::write_buffer` | 6.34% | Do not undo the instance arena: its one upload is only 0.23% of total CPU. |
| `RenderState::prepare_block_entities` | 4.35% | Feed it already-gathered rows and retain its existing model batching. |

The two `Sim::maps` call paths split 50.9% to the installed map source and 47.4%
to `map_debug`; the latter still ran with the F3 overlay closed. Both cloned every
`MapState`, including its 16 KiB colour grid. The block-entity share is spread over
more than twenty individually modest functions (`sign_spawns` 1.24%,
`beacon_spawns` 0.69%, `spawner_mob_spawns` 0.63%, `skull_spawns` 0.54%, and so
on), which is exactly why optimising one renderer at a time is the wrong shape.

The capture also answers the earlier buffer question. wgpu does defer
`Queue::write_buffer` copies to submit, but its native path still creates staging
buffers. In this profile, new section uploads account for 2.44% of total CPU and
other writes directly under `render_inner` for 2.21%; the retained entity instance
arena accounts for only 0.23%. A second generic write-buffer batching rewrite is
therefore not the first move.

## Selected design

### One extraction boundary

After `Sim::step` and after the frame camera is known, `WindowApp::redraw` asks
`Sim` for one `WorldRenderSnapshot`. Live-server extraction takes one shared client
handle, obtains one loaded-chunk list and one world read guard, and walks every
loaded chunk's block-entity records once. The offline/integrated path builds the same
public snapshot shape from its local world.

The builder classifies records by renderer family while the record and its chunk are
hot. It resolves shared facts once: block position, block state/name, packed light,
distance key, and the immutable NBT/payload handle. Renderer-specific animation
values such as chest lid progress, bell shake, banner phase, conduit tick, and
spawner spin are applied from the frame clock and the existing small trackers without
rescanning chunks.

The resulting object is immutable for the rest of the frame. `RenderState` receives
it as a value/`Arc`, not as twenty boxed callbacks that can each observe a different
world revision.

### Hot rows and cold payloads

The snapshot groups hot, fixed-size rows by consumer rather than storing a single
large enum vector. Chests, skulls, signs, banners, beacons, item-bearing blocks, and
the other renderer families each get a retained `Vec<Row>`. Rows contain the data
needed for culling, sorting, and common pose selection. Variable text, item
components, skins, patterns, and NBT-derived data stay behind shared immutable
handles and are touched only by the renderer that needs them.

This structure makes the common loops sequential and cache-friendly without forcing
every renderer to pull a wide union containing fields it never reads. Vector
capacities are retained across frames and lengths are cleared at `begin_frame`.

### Maps are copy-on-write snapshots

`MapStore` publishes an `Arc<MapSnapshot>` (or an equivalent generation-tagged
shared value) rather than returning a deep-cloned `BTreeMap<i32, MapState>`.
Individual map payloads are shared and become copy-on-write only when a
`MapItemData` patch modifies that map. A frame with no map packet performs only an
`Arc` clone.

The render snapshot carries the shared map view once. The map renderer and the F3
summary read that same view; `map_debug` is not gathered while the overlay is
closed. GPU map textures become generation-keyed retained resources, so an unchanged
128 x 128 picture is not recreated and uploaded every frame.

This change does not preserve the current accidental limitation that all visible
maps use the lowest id forever. The snapshot API is keyed by map id, and the entity
extraction path must retain `ItemComponents::map_id` for held and framed maps. A
temporary `None` fallback may remain for callers that genuinely lack an id, but it
must not require cloning every known map.

### Renderer consumption

`RenderState::prepare_block_entities`, sign text, beacon/gateway beams, moving
blocks, spawner mobs, item-model geometry, and framed maps consume borrowed slices
from `WorldRenderSnapshot`. Their existing model resolution, frustum tests, batch
keys, instance arena, and draw order remain unchanged in the first pass.

The old source setter types remain only as narrow compatibility seams for tests while
the production call site migrates. Once every production consumer reads the
snapshot, the setters and per-frame closure installation are removed together; a
half-migrated permanent state would keep the repeated-scan hazard alive.

## Data flow

```text
network / integrated world updates
  -> exact Sim::step and event fold
  -> one WorldRenderSnapshot extraction
       -> shared generation-tagged maps
       -> one loaded-chunk + block-entity scan
       -> retained typed row vectors
       -> frame clock + small animation trackers
  -> RenderState borrows snapshot slices
       -> existing cull / pose / batch planning
       -> existing instance arena + retained map textures
       -> existing render passes
```

## Consistency and invalidation

- The synchronous snapshot represents one completed `Sim::step`; no consumer can
  observe half of a network fold or a later block update than its siblings.
- Local movement, collision, targeting, input, and packet emission remain on the
  exact simulation path. This design performs no future-tick prediction.
- A changed map id advances that map's generation only. Unchanged map textures and
  summaries remain reusable.
- Chunk load, unload, block-entity replacement, block-state change, and light change
  are reflected at the next synchronous extraction. The first pass does not require
  a persistent dirty index to be correct.
- Renderer resources are cleared when the session identity changes, so an `Arc`
  from a disconnected world cannot survive into the next one.

## Reachability and relight follow-ups

The snapshot is deliberately not used as an excuse to combine unrelated caches.
`reachable_from_camera` already caches on `(camera 8-block cell, visibility graph
generation)`. Its 9.58% share means the next investigation should measure why that
generation changes so often in the megaworld—most likely streaming section uploads—
then batch graph updates or preserve connectivity generations when replacement data
is identical. Replacing the reachability algorithm before measuring invalidation
frequency risks changing pixels for no gain.

Relighting is similarly separate. `relight_changed_blocks` is 8.86% inclusive and
`lodestone_world::relight::propagate` is 5.37%. After the render snapshot lands, add
a dirty-epoch fast path and measure stationary versus moving segments independently.
A cached future-tick simulation does not target this work: most presented frames do
not advance a fixed tick, while chunk/light dirtiness can invalidate a predicted
result immediately.

## Error handling and invariants

- Snapshot extraction must degrade to empty typed slices when there is no live world;
  it must not retain a previous session's data.
- A malformed block entity is skipped only by its renderer-family decoder, without
  aborting extraction of other records in the same chunk.
- Shared payloads are immutable after publication. Mutation uses copy-on-write or a
  new generation, never interior mutation visible to the renderer.
- Per-family ordering remains the current deterministic position/distance ordering.
  Tests compare the old gather and new snapshot outputs before the old path is
  removed.
- The snapshot owns or shares every value it exposes; no world read guard or ECS
  borrow reaches GPU encoding.

## Testing and performance acceptance

GPU-independent tests build mixed chunks containing signs, heads, banners, chests,
beacons, item-bearing blocks, and malformed records, then assert that one extraction
matches the existing per-family gathers. Map tests assert O(1) snapshot cloning,
copy-on-write isolation after a patch, id-correct lookup, and no debug summary work
while F3 is closed. Session-reset tests prove stale snapshots cannot render.

The performance gate repeats the same fullscreen built-in-display megaworld with F3
closed and open. Comparable runs use the normal benchmark path; a fresh Samply
capture is attribution evidence and is not compared as a frame-time arm because
sampling changes timing.

Primary acceptance metrics are lower `prepare` and `world.prepare_buffers` CPU
means and removal of `Sim::maps` plus the repeated block-entity gather family from
the top of the symbolicated profile. Secondary metrics are frame p50/p95/p99,
`world_encode_submit`, GPU pass timestamps, section counts, RSS, map texture uploads,
world-lock acquisitions, block-entity records scanned, and snapshot rebuild/reuse
counts. GPU time must remain neutral within run spread; this change primarily removes
CPU extraction and upload churn rather than shader work.

## How to change it

Add a new renderer family by extending the snapshot builder's single classification
match and adding a narrow typed slice. Do not add another closure that calls
`loaded_chunks()` or acquires the world independently. If a new family needs large
payloads, share those payloads and keep them out of the hot row.

Only introduce a persistent per-chunk dirty index after the synchronous version is
measured. That second stage should cache extracted rows by chunk revision and rebuild
only dirty chunks; it must preserve the same immutable snapshot contract so renderer
code does not change again. A worker-thread double buffer is a later implementation
choice behind the same contract if synchronous extraction remains material.

## Configuration

The snapshot is always enabled once migrated and has no player-facing option.
Benchmark selection remains `--workload megaworld` with
`--debug-overlay closed|open`. Samply benchmark recordings always request
`--unstable-presymbolicate` so the profile and its `.syms.json` sidecar can be
analysed without a hosted UI.

## Dependencies

- `lodestone-shell::Sim`, `WindowApp::redraw`, and `RenderState` for extraction and
  frame ownership.
- `lodestone-client::ClientHandle` and `lodestone-world::World` for live chunk,
  block-state, light, and block-entity data.
- `lodestone-game::maps::{MapStore, MapState}` and the `SessionMaps` ECS component.
- Existing block-entity animation trackers and the frame clock.
- Existing entity/model batching, frustum culling, and `InstanceBufferArena`.
- `scripts/client-frame-benchmark.py`, Samply, and
  `scripts/profile-cost-table.py` for performance verification.
