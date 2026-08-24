# Client frame performance — 2026-08-24

## What it is

This is a measured client-rendering investigation at render distance 24 in a
release configuration. It covers ordinary Java terrain and a dense Java-hosted
showcase containing signs, player heads, patterned banners, mapped item frames,
equipped entities, particles, displays, and block entities.

The investigation identified the dominant presentation wait, profiled the
largest actionable CPU hot path, first retained per-batch buffers, then replaced
the remaining per-batch queue writes with one frame instance arena. The latest
run also samples real GPU render-pass timestamps on the MacBook's internal
fullscreen display.

## Configuration and method

- Machine: Apple arm64, macOS 26.5.2, native Metal through wgpu.
- Client: `target/release/lodestone`, render distance 24, unlimited frame
  option, `PresentMode::AutoNoVsync`.
- Server: Java 26.2 oracle, server view distance 25.
- Segments per trial: 20 seconds warmup, 30 seconds stationary, 60 seconds
  moving; three trials per workload.
- Allocation baseline: `964709489e9f54a1962d105f5e9f9f69162ffa18`.
- Retained per-batch pool: `8305a24f7c64974cba5b3ed3e4e3a3bdfd5fc501`.
- Shared arena implementation: `7a3733f7`.
- Latest measured benchmark policy: `0b9307fc`.
- CPU samples: Samply, using the stationary showcase window for attribution.

The tables report the median of the three per-trial values. Phase values are
CPU wall time. They must not be relabelled as GPU duration, and phase means do
not add to frame interval because surface acquisition can wait for the GPU,
compositor, or presentation queue.

The allocation-baseline and retained-pool trials used a 2560×1440 window. The
arena follow-up uses hardware-selected borderless fullscreen on the MacBook's
internal panel, whose notch-safe drawable is 3024×1898. Frame interval and
swapchain-acquire numbers across those presentation modes are therefore not an
A/B result. The explicitly instrumented CPU preparation phases are shown as a
directional comparison, with this limitation stated rather than hidden.

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

## Fixes implemented

The first change introduced `InstanceBufferPool`, retaining ordinal destination
buffers across frames. It removed per-frame destination-buffer creation but
still issued one `Queue::write_buffer` per visible model part.

The second change replaces that pool with `InstanceBufferArena`. Every tinted
entity/block-entity producer appends `EntityInstanceRaw` bytes into one retained
CPU vector and stores a byte range. After all producers finish, the renderer
grows one retained GPU vertex buffer geometrically if needed and performs one
non-empty `Queue::write_buffer`; draws bind slices of that shared buffer.
Existing batch keys, draw order, instance counts, culling, and shaders are
unchanged.

The arena covers ordinary entity bodies, water-mask placement, armour, wool, capes,
elytra, orbs, sprite entities, paintings, spawner previews, banners, heads, and
other block-entity model parts. The legacy one-shot API remains available to
isolated render paths that do not participate in a world frame.

GPU-independent lifecycle tests prove contiguous aligned ranges, stable CPU/GPU
capacity reuse, geometric growth, byte-for-byte record conversion, and rejection
of append-after-upload or double-upload. See
[`entity-rendering.md`](./entity-rendering.md) for ownership and extension
details, the [pool plan](./superpowers/plans/2026-08-24-reuse-entity-instance-buffers.md),
and the [arena plan](./superpowers/plans/2026-08-24-entity-instance-arena.md).

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

### Shared-arena follow-up — internal fullscreen

The arena was measured in three full showcase trials at 3024×1898 on the
hardware built-in MacBook panel. The table compares its CPU phases to the pooled
run because those are the code paths changed; frame/acquire values are reported
separately because the presentation mode and drawable changed.

| segment / metric | pooled window | shared arena fullscreen | directional change |
|---|---:|---:|---:|
| stationary `world.prepare_buffers` | 1.236 ms | 0.822 ms | **−33.5%** |
| stationary `world.queue_submit` | 0.299 ms | 0.124 ms | **−58.5%** |
| stationary `world_encode_submit` | 1.794 ms | 1.185 ms | **−33.9%** |
| stationary active CPU phases (acquire excluded) | 2.504 ms | 1.917 ms | **−23.4%** |
| moving `world.prepare_buffers` | 1.062 ms | 0.793 ms | **−25.3%** |
| moving `world.queue_submit` | 0.252 ms | 0.127 ms | **−49.7%** |
| moving `world_encode_submit` | 1.541 ms | 1.139 ms | **−26.1%** |
| moving active CPU phases (acquire excluded) | 2.254 ms | 1.885 ms | **−16.4%** |

Within the three fullscreen arena trials, stationary `world.prepare_buffers`
spanned 0.804–0.831 ms and moving spanned 0.784–0.796 ms. The improvement is
larger than either spread. The exact upload call count is now one non-empty
arena write per frame by construction instead of one write per batch.

The same fullscreen trials measured stationary frame p50 at 2.068–2.097 ms
(median 2.069 ms) and moving at 2.035–2.114 ms (median 2.081 ms). Mean acquire
was 1.65–1.97 ms stationary and 1.64–2.06 ms moving. Those figures describe the
current internal-fullscreen presentation path; the large change from the old
8.33 ms windowed cadence must not be attributed to the arena.

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

### Post-pool profile and post-arena measurements

The optimized stationary profile contains 29,988 weighted samples. The pooled
upload helper fell from 10.5% to 6.5% of samples and no longer descends into
`create_buffer_init`. Its remaining 1,963-sample neighbourhood spends 98.0% in
`Queue::write_buffer`.

That queue call still creates temporary wgpu staging buffers: Metal buffer
creation remains visible below `StagingBuffer::new`. The pool removed the
destination-buffer churn and produced the measured 15–21% preparation/encoding
gain, but it did not eliminate per-part staging allocations. This profile is the
basis for the next step below.

The post-pool profile is
`bench-results/profiles/showcase-20260824-034952.json.gz`. Profiles and raw
JSONL/CSV results are local benchmark artifacts rather than committed source.

The post-arena Samply capture is
`bench-results/profiles/showcase-20260824-121716.json.gz`. It completed normally,
but its saved Firefox-profile data requires Samply's local symbol server plus
the hosted Firefox Profiler UI to resolve function names; that UI was not
available under the app's browser security policy. No new stack percentage is
claimed from the unsymbolicated file. The arena result instead rests on the
row-correlated CPU phase instrument above, whose preparation improvement is
larger than the full three-trial spread.

### GPU pass timestamps

One additional full internal-fullscreen showcase trial retained 109
asynchronous wgpu timestamp snapshots:

| GPU segment | median | p95 | interpretation |
|---|---:|---:|---|
| `world` | 0.83 ms | 1.09 ms | real block render pass: terrain, entities, block entities, particles, weather, outline and debug geometry |
| `first_person` | 0.78 ms | 1.03 ms | real hand/held-item render pass |
| `world_total` | 0.25 ms | 0.77 ms | diagnostic dummy-pass span; not a bound |
| `hud_total` | 0.57 ms | 0.80 ms | diagnostic dummy-pass span; not a bound |

The two real-pass readings are trustworthy whole-pass durations. They must not
be added: Apple-silicon Metal can pipeline stages across passes, and wgpu does
not expose `TIMESTAMP_QUERY_INSIDE_PASSES` or
`TIMESTAMP_QUERY_INSIDE_ENCODERS` on this adapter. The aggregate spans use a
private dummy attachment that does not order against the real attachments;
their being smaller than enclosed passes is the known proof that they are hints,
not measurements. A Metal/Xcode capture is still required for shader, tile,
bandwidth, or draw-level attribution inside the 0.83 ms world pass.

## Improvement plan

1. **Separate renderer throughput from presentation pacing.** Log the selected
   adapter, supported surface modes, selected mode, display refresh, and acquire
   wait in every benchmark record. Compare explicit `Immediate` when Metal
   exposes it, borderless/fullscreen presentation, and an offscreen render
   target. The offscreen arm is the max-throughput control; the windowed arm is
   the player-visible result. Success means we can tell a compositor/display
   ceiling from actual GPU saturation instead of calling all acquire time GPU
   rendering.
2. **Frame instance arena — completed.** All world-space tinted
   `EntityInstanceRaw` batches now share one retained CPU/GPU arena and one
   non-empty queue write. The dense scene improved preparation 25–34% and total
   active CPU 16–23%, with every targeted Metal pixel gate unchanged.
3. **Reuse CPU accumulation storage and cache static plans.** The post-pool
   stationary stack next showed ordinary entity preparation at
   15.6% of `render_inner`, block-entity preparation at roughly 12%,
   `RawVec::reserve` at 1.5%, and animated-model resolution around 4%. Retain
   per-frame transform/light/tint vectors, cache static block-entity part plans,
   and invalidate by block-entity content, light, pose, or resource generation.
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
6. **Capture inside the GPU world pass before changing shaders or wgpu limits.**
   Real pass timestamps put the dense world pass at 0.83 ms median / 1.09 ms
   p95 and first-person at 0.78 / 1.03 ms. Use an Xcode Metal capture for draw,
   tile, shader, bandwidth, and attachment attribution; wgpu cannot subdivide a
   pass on this Apple GPU. Only then decide whether occlusion, LOD, overdraw,
   bind-group layout, or pipeline options are the next GPU-side constraint.

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
`crates/lodestone-shell/src/gpu/frame.rs`, and the arena policy in
`crates/lodestone-render/src/entity_pipeline.rs`. Preserve the three-trial
configuration and compare commits by their recorded SHA.

## Configuration

There are no runtime options for the instance arena. Benchmark flags,
durations, paths, endpoints, and environment variables are documented in
[`live-client-frame-benchmark.md`](./live-client-frame-benchmark.md).

## Dependencies

The benchmark depends on Lodestone’s release client, wgpu/Metal, the Java 26.2
oracle worlds, Apple’s container runtime, Python 3, and Samply for CPU profiles.
The optimization itself depends only on `lodestone-render`, wgpu buffers and
queue writes, and the shell’s existing frame-preparation order. Hardware
built-in display selection on macOS additionally uses CoreGraphics as documented
in [`live-client-frame-benchmark.md`](./live-client-frame-benchmark.md).
