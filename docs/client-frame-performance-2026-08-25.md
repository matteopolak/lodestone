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

The next large-world investigation should not change culling or batching based
only on this authored spawn. Add deterministic waypoints or a denser generated
flight path that reproduces the reported 7–8 ms `world_encode_submit`, record
model/packed sections, entity/model-part counts, draw counts, arena upload bytes,
and pipeline switches, then profile `world.prepare_buffers`. Likely experiments,
in evidence order, are retained scratch vectors, static block-entity/model-part
plans, wider batch reuse, and only then spatial-model or culling changes.

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
