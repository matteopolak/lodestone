# Client frame performance — 2026-08-24

## What it is

This is a measured client-rendering investigation at a physical 2560×1440,
render distance 24, release configuration. It covers ordinary Java terrain and
a dense Java-hosted showcase containing signs, player heads, patterned banners,
mapped item frames, equipped entities, particles, displays, and block entities.

The investigation identified the dominant presentation wait, profiled the
largest actionable CPU hot path, replaced per-frame entity instance-buffer
allocation with retained buffers, and repeated both workloads under the same
configuration.

## Configuration and method

- Machine: Apple arm64, macOS 26.5.2, native Metal through wgpu.
- Client: `target/release/lodestone`, physical framebuffer 2560×1440, render
  distance 24, unlimited frame option, `PresentMode::AutoNoVsync`.
- Server: Java 26.2 oracle, server view distance 25.
- Segments per trial: 20 seconds warmup, 30 seconds stationary, 60 seconds
  moving; three trials per workload.
- Baseline: `964709489e9f54a1962d105f5e9f9f69162ffa18`.
- Optimized: `8305a24f7c64974cba5b3ed3e4e3a3bdfd5fc501`.
- CPU samples: Samply, using the stationary showcase window for attribution.

The tables report the median of the three per-trial values. Phase values are
CPU wall time. They must not be relabelled as GPU duration, and phase means do
not add to frame interval because surface acquisition can wait for the GPU,
compositor, or presentation queue.

## What was slow

### Presentation is the frame-rate ceiling

Every stable workload converged on an 8.33 ms frame interval, or 120 frames per
second, despite the unlimited option and `AutoNoVsync`. Baseline stationary
showcase sampling attributed 18,195 of 29,997 weighted samples (60.7%) to the
Metal surface-acquire stack. The ordinary phase timer measured 5.2–6.7 ms mean
acquire time in warm runs.

This is a wait/back-pressure result, not evidence that the acquire function
does CPU computation for six milliseconds. The engine reaches the swapchain
before the next image is available. Removing CPU work therefore increases the
measured acquire wait while keeping p50 at 8.33 ms, which is exactly what the
optimized showcase did.

### Dense-scene instance upload was the actionable CPU bottleneck

The baseline showcase spent 1.53–1.56 ms per stationary frame in
`world.prepare_buffers`, versus 0.59–0.66 ms in warm ordinary terrain. The
baseline sampled stationary window found:

- 3,158 samples (10.5% of the window) in `upload_instances_tinted`;
- 2,469 of those (78.2%) immediately in
  `DeviceExt::create_buffer_init`;
- callers split 51.8% entity bodies, 44.8% block-entity parts, and 3.4% the
  direct world caller/banner path.

The renderer allocated one Metal vertex buffer per visible model part, every
frame. Its batching already reduced draw calls, but the instance buffer behind
each remaining draw was treated as disposable.

## Fix implemented

`InstanceBufferPool` now retains ordinal buffer slots across frames. A frame
resets only the slot cursor. Each entity or block-entity upload rewrites a
sufficient slot through `Queue::write_buffer`, and replaces only a slot whose
power-of-two capacity is too small. Existing batch keys, draw order, instance
counts, culling, and shaders are unchanged.

The pool covers ordinary entity bodies, water masks, armour, wool, capes,
elytra, orbs, sprite entities, paintings, spawner previews, banners, heads, and
other block-entity model parts. The legacy one-shot API remains available to
isolated render paths that do not participate in a world frame.

The GPU-independent lifecycle test proves a stable frame creates no new pool
slots and that growth replaces only the undersized ordinal slot. See
[`entity-rendering.md`](./entity-rendering.md) for ownership and extension
details and the [implementation plan](./superpowers/plans/2026-08-24-reuse-entity-instance-buffers.md)
for the original measured hypothesis.

## Before and after

### Dense showcase — three-trial medians

| segment / metric | baseline | pooled | change |
|---|---:|---:|---:|
| stationary frame p50 | 8.335 ms | 8.333 ms | effectively unchanged |
| stationary frame p95 / p99 | 8.608 / 8.749 ms | 8.581 / 8.702 ms | −0.027 / −0.047 ms |
| stationary `world.prepare_buffers` | 1.538 ms | 1.236 ms | **−19.6%** |
| stationary `world.queue_submit` | 0.449 ms | 0.299 ms | **−33.5%** |
| stationary `world_encode_submit` | 2.281 ms | 1.794 ms | **−21.4%** |
| stationary acquire | 5.240 ms | 5.762 ms | +0.522 ms wait/slack |
| moving frame p50 | 8.334 ms | 8.333 ms | effectively unchanged |
| moving frame p95 / p99 | 8.582 / 8.705 ms | 8.566 / 8.678 ms | −0.016 / −0.028 ms |
| moving `world.prepare_buffers` | 1.256 ms | 1.062 ms | **−15.4%** |
| moving `world.queue_submit` | 0.353 ms | 0.252 ms | **−28.8%** |
| moving `world_encode_submit` | 1.862 ms | 1.541 ms | **−17.3%** |
| moving acquire | 5.676 ms | 6.012 ms | +0.336 ms wait/slack |
| peak RSS | 865 MiB | 853 MiB | no regression; −1.4% |

All six baseline and all six optimized showcase segments recorded zero frames
over 16.67 ms or 33.3 ms. The optimized preparation and total-world values had
only 0.006 ms and 0.015 ms stationary spread respectively, so the improvement
is repeatable rather than a single warm-cache sample.

### Ordinary terrain — three-trial medians

| segment / metric | baseline | pooled | change |
|---|---:|---:|---:|
| stationary frame p50 | 8.349 ms | 8.347 ms | effectively unchanged |
| stationary `world.prepare_buffers` | 0.625 ms | 0.516 ms | −17.4% |
| stationary `world_encode_submit` | 1.191 ms | 1.082 ms | −9.2% |
| moving frame p50 | 8.344 ms | 8.345 ms | effectively unchanged |
| moving `world.prepare_buffers` | 0.645 ms | 0.592 ms | −8.3% |
| moving `world_encode_submit` | 1.253 ms | 1.173 ms | −6.4% |
| peak RSS | 853 MiB | 842 MiB | no regression; −1.2% |

The first moving terrain trial in each arm included cold chunk streaming. The
median retains that trial symmetrically; the two warm optimized runs stayed at
1.12–1.17 ms total world time versus 1.19–1.25 ms in the warm baseline runs.

### Post-change profile

The optimized stationary profile contains 29,988 weighted samples. The pooled
upload helper fell from 10.5% to 6.5% of samples and no longer descends into
`create_buffer_init`. Its remaining 1,963-sample neighbourhood spends 98.0% in
`Queue::write_buffer`.

That queue call still creates temporary wgpu staging buffers: Metal buffer
creation remains visible below `StagingBuffer::new`. The pool removed the
destination-buffer churn and produced the measured 15–21% preparation/encoding
gain, but it did not eliminate per-part staging allocations. This profile is the
basis for the next step below.

The post-change profile is
`bench-results/profiles/showcase-20260824-034952.json.gz`. Profiles and raw
JSONL/CSV results are local benchmark artifacts rather than committed source.

## Improvement plan

1. **Separate renderer throughput from presentation pacing.** Log the selected
   adapter, supported surface modes, selected mode, display refresh, and acquire
   wait in every benchmark record. Compare explicit `Immediate` when Metal
   exposes it, borderless/fullscreen presentation, and an offscreen render
   target. The offscreen arm is the max-throughput control; the windowed arm is
   the player-visible result. Success means we can tell a compositor/display
   ceiling from actual GPU saturation instead of calling all acquire time GPU
   rendering.
2. **Replace per-part queue writes with a frame instance arena.** Pack all
   `EntityInstanceRaw` data into one or a few growable, frame-rotated arenas,
   upload each arena once, and carry byte ranges into draw batches. A wgpu
   `StagingBelt` is an alternative only if its chunks are demonstrably recalled
   and reused on this backend. Gate it with the same dense scene: the remaining
   6.5% upload stack and `StagingBuffer::new` samples should collapse, while
   pixels and instance counts stay unchanged.
3. **Reuse CPU accumulation storage.** After the upload arena, profile again.
   The optimized stationary stack next shows ordinary entity preparation at
   15.6% of `render_inner`, block-entity preparation at roughly 12%,
   `RawVec::reserve` at 1.5%, and animated-model resolution around 4%. Retain
   per-frame transform/light/tint vectors, cache static block-entity part plans,
   and invalidate by block-entity content, light, or resource generation.
4. **Reduce submission and draws only with counters.** Encoder finish and queue
   submit remain measurable, but batching changes should be driven by draw,
   pipeline-switch, and upload-byte counters per pass. Consider a shared arena
   plus broader texture/model grouping or render bundles for static block
   entities before multi-draw/indirect complexity.
5. **Add hierarchical culling where it has leverage.** Terrain cull/draw CPU was
   only about 0.09 ms in warm runs, so rewriting terrain culling first is not
   supported by this profile. Dense static block entities should instead be
   indexed by section, reject whole sections before preparing sign glyphs,
   banners, heads, and item frames, and cache map/sign meshes by content.
6. **Measure the GPU before changing shaders or wgpu limits.** The existing
   synthetic timestamp readings stalled and were internally inconsistent on
   this Metal path. Add row-correlated timestamp queries or take an Xcode Metal
   capture for the dense and terrain arms. Only then decide whether occlusion,
   LOD, overdraw, bind-group layout, or pipeline options are the next GPU-side
   constraint.

## How to reproduce or change it

Use the commands and validation policy in
[`live-client-frame-benchmark.md`](./live-client-frame-benchmark.md):

```bash
CC=/usr/bin/clang RUSTC_WRAPPER= cargo build --release -p lodestone-shell --bin lodestone
just bench-client-terrain
just bench-client-showcase
python3 scripts/client-frame-benchmark.py --workload showcase --samply
```

Change the fixture in `scripts/benchmark-scenes/showcase.txt`, benchmark motion
in `crates/lodestone-shell/src/app/benchmark.rs`, phase boundaries in
`crates/lodestone-shell/src/gpu/frame.rs`, and the retained-buffer policy in
`crates/lodestone-render/src/entity_pipeline.rs`. Preserve the three-trial
configuration and compare commits by their recorded SHA.

## Configuration

There are no new runtime options for the instance-buffer pool. Benchmark flags,
durations, paths, endpoints, and environment variables are documented in
[`live-client-frame-benchmark.md`](./live-client-frame-benchmark.md).

## Dependencies

The benchmark depends on Lodestone’s release client, wgpu/Metal, the Java 26.2
oracle worlds, Apple’s container runtime, Python 3, and Samply for CPU profiles.
The optimization itself depends only on `lodestone-render`, wgpu buffers and
queue writes, and the shell’s existing frame-preparation order.
