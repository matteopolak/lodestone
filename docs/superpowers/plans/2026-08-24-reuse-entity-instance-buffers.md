# Reuse Dynamic Entity Instance Buffers

> **Execution:** Follow this plan in the current session with test-driven changes and foreground verification.

**Goal:** Remove per-frame GPU buffer allocation from animated entity and block-entity instance uploads without changing batching, visibility, draw order, or rendered pixels.

**Measured basis:** Three Java-backed 2560×1440 showcase trials put `world.prepare_buffers` at 1.53–1.56 ms for the stationary view, versus 0.59–0.66 ms in warm terrain. The Samply profile contains 29,997 weighted samples in the stationary window: 3,158 (10.5%) include `upload_instances_tinted`, and 2,469 of those (78.2%) immediately descend into `wgpu::DeviceExt::create_buffer_init`. Callers split between entity bodies (51.8%), opaque block-entity parts (44.8%), and banner layers (3.4%). Swapchain acquisition is separately dominant at 60.7% of stationary wall samples and is not addressed by this change.

**Design:** Add a reusable instance-buffer pool to `lodestone-render`. Each render frame resets a cursor but retains its allocated slots. Each upload takes the next slot, reuses it when its capacity is sufficient, or creates/replaces only that slot when the required byte count grows. Reused buffers receive the current instance bytes through `Queue::write_buffer`. `wgpu::Buffer` is a cheap cloneable handle, so existing batch ownership and draw code remain unchanged. Every upload in a frame gets a distinct slot, preserving all existing batch and banner-layer ordering.

---

## Task 1: Specify the slot lifecycle with a failing unit test

**Files:**
- Modify: `crates/lodestone-render/src/entity_pipeline.rs`

Add a GPU-independent test using a generic pool state and fake handles. It must prove:

1. the first frame creates one slot per upload;
2. resetting the frame cursor reuses those exact handles without creating more;
3. a larger upload replaces only its undersized slot;
4. later slots remain reusable after an earlier replacement.

Run the test by exact name and confirm it fails before implementing the state machine.

## Task 2: Implement the reusable upload pool

**Files:**
- Modify: `crates/lodestone-render/src/entity_pipeline.rs`
- Modify: `crates/lodestone-render/src/lib.rs`

Implement the generic slot state plus public `InstanceBufferPool`. The GPU wrapper must:

- use `VERTEX | COPY_DST` buffers;
- allocate at a geometric capacity at least as large as the requested bytes;
- reset only the cursor at frame start;
- clone the retained `wgpu::Buffer` handle into the existing batch value;
- call `Queue::write_buffer` for both new and reused slots;
- keep empty uploads as `None`.

Change `upload_instances_tinted` to accept the pool and queue. Run its targeted unit tests.

## Task 3: Route every tinted instance upload through one frame pool

**Files:**
- Modify: `crates/lodestone-shell/src/gpu.rs`
- Modify: `crates/lodestone-shell/src/gpu/frame.rs`
- Modify: `crates/lodestone-shell/src/gpu/entity_passes.rs`
- Modify: `crates/lodestone-shell/src/gpu/spawner_mobs.rs`
- Update any affected shell tests.

Store one pool on `RenderState`, reset it once at the start of `render_inner`, and pass it plus `Queue` to every `upload_instances_tinted` call. Do not change batch keys, part order, banner layer order, instance counts, or draw code.

Run targeted render/entity tests, then `cargo check -p lodestone-shell --all-targets` and the no-default-features seam check in the foreground.

## Task 4: Document and measure the result

**Files:**
- Modify: `docs/entity-rendering.md`
- Create: `docs/client-frame-performance-2026-08-24.md`
- Regenerate: `docs/README.md`

Document pool ownership, frame reset semantics, growth behavior, and the queue-ordering requirement. Record the exact baseline, profiler evidence, implementation, and limits in the dated performance report.

Build the release client, then run three full `showcase` trials under the same 2560×1440, render-distance-24 configuration. Compare medians across trials for:

- `world.prepare_buffers` (primary);
- `world_encode_submit`;
- frame interval p50/p95/p99;
- RSS peak.

If the primary metric does not improve repeatably, keep the evidence and revert the optimization with a follow-up patch rather than claiming success. If it improves, run one post-change Samply profile to verify that `create_buffer_init` no longer dominates the steady-state upload path.

## Task 5: Final foreground verification

Run the smallest complete checks covering all touched code, followed by repository-required checks that fit the remaining session. Report every command not run. Confirm `git diff --cached --name-only` is empty before committing explicit paths, regenerate the docs index, and inspect the resulting commit stat.
