# Client frame performance — 2026-08-25

## What it is

This is the large-world follow-up to the 2026-08-24 client rendering study. It
measures the release client in fullscreen on the MacBook's built-in display
while joined to the official Hermitcraft Season 10 Java world, compares F3
closed and open, attributes the open-overlay cost with CPU sampling, and records
the result of caching HUD glyph geometry.

The investigation found that F3's metric gathering was cheap. Re-expanding
every glyph into flat-colour scanline quads every frame was not. The fix caches
tint-independent glyph ink runs and retains the whole F3 vertex layer on the
GPU, refreshing it at 10 Hz or immediately after a visibility/layout/font
change.

## How it works

The benchmark uses `just bench-client-megaworld`, documented in
[`live-client-frame-benchmark.md`](./live-client-frame-benchmark.md). Each arm
runs three release trials at render distance 24 and 3024×1898, with 45 seconds
of warmup, 30 seconds stationary, and 60 seconds moving. The Java server uses
view distance 25. Workload counters make section-count changes visible instead
of attributing them to the code change.

CPU phase columns are wall-clock spans. GPU values are asynchronous wgpu
timestamp queries. `world` and `first_person` are real pass measurements;
`world_total` and `hud_total` remain diagnostic dummy-pass spans and are not
bounds or additive components.

The baseline commit was `8d39f925`. Both optimized stages were measured from
the same working tree on top of that commit:

1. cache a glyph's source-colour horizontal ink runs after the first raster;
2. build F3 into a dedicated retained vertex buffer and upload it no more than
   once per 100 ms, with immediate invalidation when required.

The saved pre-fix CPU sample is
`bench-results/profiles/megaworld-open-20260825-144909.json.gz`. Symbolizing its
hottest Lodestone leaves identified `VanillaFont::draw_ink`,
`GlyphRaster::texel_rgba`, and `ColourStream::rect`: each visible F3 glyph was
being scanned texel by texel and expanded into rectangles on every redraw.

A later closed-F3 capture, recorded before the map snapshot change, is
`bench-results/profiles/megaworld-closed-20260825-210124.json.gz` with its
`.json.syms.json` sidecar. `threadCPUDelta`-weighted call paths attributed 14.0%
of sampled client CPU to `Sim::maps`; about half fed the map render source and
47.4% fed `Sim::map_debug` even though F3 was closed. `MapState::clone` alone
accounted for 13.8%. This profile selected copy-on-write map snapshots as the
first world-extraction optimization.

The post-map capture is
`bench-results/profiles/megaworld-closed-20260826-120939.json.gz`; it confirms
`Sim::maps` fell from 14.0% to 0.01% and `MapState::clone` disappeared. It also
made the next repeated-extraction cost visible: state-driven block-entity
renderers independently walked the same block-entity records and called
`entity_light_at` once per surviving object. The shared block-entity snapshot
was measured with
`bench-results/profiles/megaworld-closed-20260826-122258.json.gz`.

## Measurements

### F3 cost before the fix

The matched three-trial baseline kept exactly 2,168 model sections and zero
packed sections in all six measured segments.

| metric (three-trial median) | F3 closed | F3 open | open penalty |
|---|---:|---:|---:|
| stationary frame p50 | 5.750 ms | 7.756 ms | +2.006 ms / +34.9% |
| moving frame p50 | 5.225 ms | 7.406 ms | +2.181 ms / +41.7% |
| stationary `hud.hud_draw` | 0.110 ms | 2.163 ms | +2.053 ms |
| moving `hud.hud_draw` | 0.105 ms | 2.161 ms | +2.056 ms |
| stationary `hud.debug_gather` | — | 0.070 ms | small |
| moving `hud.debug_gather` | — | 0.070 ms | small |

The exact closed-arm draw values vary slightly between trials; the important
ratio is stable: gathering the F3 strings cost about 0.07 ms, while rebuilding
their geometry cost about 2.16 ms. Optimizing metric collection first would
therefore have targeted roughly three percent of the overlay's CPU cost.

### Staged and final improvement

| segment / metric | baseline | ink-run cache | retained F3 layer | final change |
|---|---:|---:|---:|---:|
| stationary frame p50 | 7.756 ms | 8.337 ms | 7.022 ms | **−9.5%** |
| moving frame p50 | 7.406 ms | 8.040 ms | 6.620 ms | **−10.6%** |
| stationary `hud.hud_draw` | 2.163 ms | 1.849 ms | 0.279 ms | **−87.1%** |
| moving `hud.hud_draw` | 2.161 ms | 1.846 ms | 0.271 ms | **−87.4%** |
| stationary HUD total | 2.915 ms | 2.839 ms | 1.263 ms | **−56.7%** |
| moving HUD total | 2.893 ms | 2.792 ms | 1.236 ms | **−57.3%** |

The ink-run cache isolated the cost of rerasterizing/rescanning pixels and
improved HUD drawing by about 14.5%. Retaining the complete debug layer removed
the larger cost: rebuilding the same transformed rectangles and writing the
ordinary colour vertex buffer every frame.

The final three trials measured stationary `hud.hud_draw` at
0.277–0.285 ms and moving at 0.265–0.326 ms. The final frame p50 spread was
6.759–7.104 ms stationary. Two moving trials stayed at 6.400 and 6.620 ms; the
third reached 7.822 ms while its section counter rose from 2,168 to 2,479 and
`world_encode_submit` rose to 4.140 ms. That trial is retained rather than
discarded: it demonstrates real world-load sensitivity and explains the frame
variation with a measured workload change.

The aggregate frame result understates the HUD improvement because the final
runs did more world work. Baseline stationary `world.prepare_buffers` was
2.321 ms; the final median was 2.857 ms. Even with 0.536 ms more preparation,
frame p50 improved by 0.734 ms.

### Map and block-entity snapshot results

Copy-on-write map storage and the closed-F3 gather gate removed the profiled
map-copy path: `Sim::maps` fell from 14.0% of sampled client CPU to 0.01%, and
`MapState::clone` no longer appeared. Across matching ordinary stationary
trials, `hud.frame_gather` fell from a 0.695 ms median to 0.020 ms (−97.1%).
Frame p50 moved from 5.750 ms to 5.688 ms (−1.1%) because `sim_tick` and world
preparation varied upward during the post-change trials.

The next slice creates one camera-scoped `{position, state, light}` snapshot
for chests, bells, shulker boxes, lecterns, enchanting tables, conduits, and
copper golem statues. In matched Samply captures:

| sampled CPU function | before shared snapshot | after | relative change |
|---|---:|---:|---:|
| `prepare_block_entities` | 6.97% | 4.19% | −39.9% |
| `entity_light_at` | 3.59% | 2.33% | −35.1% |
| replacement `block_entity_frame_snapshot` | — | 0.76% | one shared scan |

The former per-type chest, bell, shulker, lectern, enchanting-table, conduit,
and copper-statue gathers disappeared as standalone profile costs. This is a
real targeted CPU reduction, but the ordinary whole-frame result is not a win:

| stationary metric (three-trial median) | before | after | change |
|---|---:|---:|---:|
| frame p50 | 5.688 ms | 5.873 ms | +3.2% |
| `world.prepare_buffers` | 3.012 ms | 2.980 ms | −1.1% |
| `sim_tick` | 2.736 ms | 2.920 ms | +6.7% |

All stationary trials visited exactly 2,168 model sections and zero packed
sections. The after p50 spread was 5.183–5.959 ms, wider than the local render
saving; unrelated simulation variance erased it. The conclusion is deliberately
split: the profile validates the extraction change, while these ordinary trials
do **not** establish an end-to-end frame-time improvement.

The historical `megaworld.moving` rows in this document used a straight
forward input. Live inspection on 2026-08-26 showed that it eventually walked
into authored geometry and stayed in a corner, so those rows are valid only as
the exact workload they recorded—not as sustained traversal measurements. The
benchmark now delays creative-flight activation until after post-join server
configuration and uses a five-second climbing, full-circle orbit. Stationary
megaworld rows are unaffected. New moving rows must not be ratioed against the
old straight-walk rows.

### Reachability-cache correction

The next fresh closed-F3 profile at `097a03d8` put
`reachable_from_camera` at 7.29% self CPU, far above the next Lodestone leaves
(`extract_entity_draws` and `fold_entities` at 1.55% each). The camera cache
already keyed a walk by the 8-block camera cell and visibility-graph generation,
but every section upload replaced its graph entry and bumped that generation,
including replacements with exactly the same connectivity. Ongoing remeshing
therefore caused a full bounded breadth-first walk again even while the camera
was stationary.

`VisibilityGraph::insert` now preserves its generation when a replacement is
identical. A focused unit test proves that an equal replacement is a cache hit;
a changed connectivity or a real removal still invalidates the walk. The
post-change full runs used the same Hermitcraft closed-F3 workload, built-in
laptop fullscreen selection, and 3024×1898 framebuffer. Stationary work stayed
at 2,168 model sections and zero packed sections.

| metric | before | after | change |
|---|---:|---:|---:|
| stationary frame p50 | 8.252 ms | 4.595 ms | **−44.3%** |
| moving frame p50 | 5.130 ms | 3.866 ms | **−24.6%** |
| stationary `world.prepare_buffers` | 3.295 ms | 1.572 ms | **−52.3%** |
| `reachable_from_camera` sampled self CPU | 7.29% | 1.06% | **−85.5%** |

The before artifacts are
`bench-results/profiles/megaworld-closed-20260826-170044.json.gz` and its
`.json.syms.json` sidecar; the after pair is
`bench-results/profiles/megaworld-closed-20260826-171038.json.gz`. Samply
samples are diagnostic rather than timing measurements, but their disappearance
matches the fixed-workload frame-time result. The post-change profile's leading
Lodestone leaf is now `relight::propagate` (4.95% self), followed by
`relight_changed_blocks` (1.84%); do not optimize the remaining
`reachable_from_camera` 1.06% in isolation.

### Current CPU and GPU boundary

With F3 fixed, the representative 2,168-section final trials are CPU-heavy in
these places:

| CPU phase | stationary | moving | interpretation |
|---|---:|---:|---|
| `world.prepare_buffers` | 2.79–2.91 ms | 2.51–2.60 ms | dominant CPU render preparation |
| `sim_tick` | 2.54–2.58 ms | 2.43–2.48 ms | simulation, chunk/light work included |
| HUD total | 1.26–1.36 ms | 1.23–1.24 ms | mostly frame-data gathering now |
| `world.encoder_finish` | about 0.35 ms | about 0.35 ms | command finalization |
| `world.queue_submit` | 0.18–0.19 ms | about 0.16 ms | no longer the principal world CPU cost |
| `world.terrain_cull_draw` | about 0.09 ms | about 0.09 ms | traversal/culling itself is small here |

The final GPU timestamp medians varied with world load: `world` was
3.42–4.13 ms and `first_person` 2.73–3.36 ms across the three runs. These are
whole-pass measurements, not a shader/draw breakdown, and must not be added.
The adapter does not expose timestamp queries inside render passes, so an
Xcode/Metal capture is still required to divide world GPU time into terrain,
entities, particles, overdraw, bandwidth, and pipeline stalls.

The GPU values were higher than baseline while CPU frame time improved. That is
another reason not to infer GPU duration by subtracting named CPU phases from
frame interval. The run reached different world content and section counts.

## How to change it

Glyph-run caching lives in
`crates/lodestone-shell/src/hud/vanilla_font.rs`. `CachedGlyphInk` stores
source-colour runs, never caller tint or transformed coordinates. Extend its
key whenever a new raster-producing dimension is added. Keep cache access
poison-tolerant because font loading already treats poisoned shared state as a
recoverable failure.

The retained F3 layer and its refresh policy live in
`crates/lodestone-shell/src/hud.rs`. `DebugGeometryStamp` must cover every input
that changes geometry independently of the live debug values. Visibility,
framebuffer size, GUI scale, and the renderer's monotonic font revision currently force immediate
refresh. Ordinary value changes wait for the 100 ms interval. Keep debug
geometry first in the colour draw order; the remaining HUD colour buffer must
still render over it exactly as the old single stream did.

Do not lower the interval without re-running the open-F3 arm. At 120–500 Hz,
even a small per-refresh builder cost becomes a frame-time cost again. Do not
raise it without checking coordinates, FPS, and memory values remain useful to
players.

Section-reachability caching is split across
`crates/lodestone-shell/src/gpu/occlusion.rs` (camera-cell and graph-generation
key) and `crates/lodestone-render/src/visibility.rs` (semantic graph generation).
Keep the latter invalidation precise: a new or changed connectivity entry and a
real removal must bump it; an identical mesh re-upload must not. Widening this
to a time-based refresh would reintroduce the cost and has no rendering-fidelity
justification.

The next large CPU investigation should start with relighting, not the remaining
reachability cost. The fresh post-change profile ranks `relight::propagate` at
4.95% self and `relight_changed_blocks` at 1.84%; establish a stable changed-
block/section workload counter before choosing between reducing propagation work
and deferring redundant re-meshes. `fold_entities` remains a separate candidate
only after that measurement.

That counter pass is now installed. On the built-in 3024×1898 fullscreen
Hermit workload, at most 77 changed blocks across 10 source sections caused
366,529 propagation-cell visits and 34 dirty sections while stationary; the
moving segment reached 355,076 visits from 98 blocks across 10 sections.
Lovelier moving similarly reached 350,223 visits from 47 blocks across 10
sections. Remesh-queue coalescing was zero in all of these peaks and submission
stopped at the existing 24-section budget, so downstream queue deduplication is
not the major opportunity. The matching profile is
`bench-results/profiles/megaworld-closed-20260826-173208.json.gz` (plus its
symbol sidecar): `relight_changed_blocks` was 6.95% inclusive, flood 4.30%, and
propagation 3.81% inclusive / 3.78% self. The next implementation choice is
therefore between spatially coalescing relight jobs and replacing the broad
flood frontier with a more incremental frontier; no architecture change was
made in this counter-only pass.

The remaining block-entity work is a lower-priority extension: signs, heads,
banners, pots, item-bearing entities, spawners, beacons, and portals still own
specialised gathers. If they join the shared snapshot, store only typed decoded
payloads for matching states; never copy raw NBT into every candidate. See
[`block-entity-frame-snapshot.md`](./block-entity-frame-snapshot.md).

For GPU attribution, capture one fixed waypoint in Xcode's Metal debugger and
compare counters per encoder/pass. Use the existing wgpu timestamps as the
regression gate around that capture; a capture is diagnostic and changes
timing, so it is not a replacement for the three ordinary trials.

## Configuration

- F3 geometry refresh interval:
  `DEBUG_GEOMETRY_REFRESH_INTERVAL` in `hud.rs`, currently 100 ms.
- Benchmark render distance: 24 client / 25 server.
- Benchmark debug arm: `--debug-overlay open|closed|both`.
- Full run: `just bench-client-megaworld`.
- Open large-build run: `just bench-client-lovelier`.
- One-arm smoke: `python3 scripts/client-frame-benchmark.py --workload
  megaworld --debug-overlay open --smoke`.

## Dependencies

- The HUD colour pipeline and its retained wgpu vertex buffers.
- `lodestone-assets` font raster providers through `VanillaFont`.
- The pinned megaworld installer and Java oracle described in
  [`live-client-frame-benchmark.md`](./live-client-frame-benchmark.md).
- wgpu `TIMESTAMP_QUERY` for whole-pass GPU measurements.
- Samply plus macOS symbols for CPU attribution; Xcode/Metal is required for
  attribution inside a GPU render pass.
