# Shared Entity Instance Upload Arena

> **Execution:** Follow this plan in the current shared checkout with test-driven changes and foreground verification.

**Goal:** Replace per-batch tinted-instance queue writes with one retained CPU arena, one retained GPU buffer, and one upload per rendered frame while preserving every existing draw and batch boundary.

**Measured basis:** After the first buffer-pool optimization, the 2560×1440 dense Java showcase still spends about 1.1–1.2 ms in `world.prepare_buffers`. The remaining CPU stack is led by repeated `wgpu::Queue::write_buffer` calls and native `StagingBuffer::new` allocations. wgpu batches submission but does not coalesce these staging allocations on native backends.

**Design:** `lodestone-render::InstanceBufferArena` appends the existing `EntityInstanceRaw` records to one retained `Vec<u8>` and returns exact byte ranges. After all tinted batches are prepared, `RenderState` uploads the populated bytes once to one geometrically grown `VERTEX | COPY_DST` buffer. Batch structs carry ranges; their existing draw sites bind slices of the shared frame buffer.

---

## Task 1: Specify arena range and lifecycle behavior with failing tests

**Files:**
- Modify: `crates/lodestone-render/src/entity_pipeline.rs`

Add GPU-independent tests for a generic arena state. They must prove:

1. sequential appends return contiguous exact ranges;
2. empty appends return `None` without moving the cursor;
3. all `EntityInstanceRaw` range boundaries satisfy wgpu's four-byte alignment;
4. `begin_frame` clears length while retaining CPU capacity;
5. required GPU capacity grows geometrically and does not shrink;
6. appending after marking the frame uploaded is rejected in debug/test builds.

Run each new test by exact name and observe the initial compile/test failure before adding production state.

## Task 2: Implement and export the arena

**Files:**
- Modify: `crates/lodestone-render/src/entity_pipeline.rs`
- Modify: `crates/lodestone-render/src/lib.rs`

Replace `InstanceBufferPool` and `upload_instances_tinted_pooled` with:

- a generic byte-arena state that owns the retained CPU bytes and lifecycle flag;
- public `InstanceBufferArena::begin_frame`;
- a tinted append function returning `Option<Range<u64>>`;
- public `InstanceBufferArena::upload`, which creates/reuses one GPU buffer and performs exactly one non-empty `Queue::write_buffer` call.

Keep the standalone `upload_instances_tinted` API for first-person and other isolated callers. Use checked conversions and geometric capacity. Run the exact new tests and existing entity-pipeline tests.

## Task 3: Migrate frame preparation to ranges

**Files:**
- Modify: `crates/lodestone-shell/src/gpu.rs`
- Modify: `crates/lodestone-shell/src/gpu/block_entities.rs`
- Modify: `crates/lodestone-shell/src/gpu/state.rs`
- Modify: `crates/lodestone-shell/src/gpu/frame.rs`
- Modify: `crates/lodestone-shell/src/gpu/entity_passes.rs`
- Modify: `crates/lodestone-shell/src/gpu/spawner_mobs.rs`
- Update affected tests in those modules.

Rename `RenderState.instance_buffers` to `instance_arena`. Begin it once at frame start. Change every world-space tinted-instance producer to append and store `Range<u64>` rather than `wgpu::Buffer`. Leave flame, shadow, fishing, water-mask-specific geometry, first-person held objects, and non-tinted formats on their current upload paths.

Immediately after `prepare_block_entities`, upload the arena once and retain the returned shared buffer through the render pass. At each migrated draw, bind `shared_buffer.slice(range.clone())`. Preserve batch keys, counts, texture selection, part order, banner-layer order, and indexed ranges.

Use `rg` to prove there are no remaining world-frame calls to the pooled API or world batch fields owning tinted-instance buffers.

## Task 4: Compile and correctness verification

Run in the foreground:

1. the exact arena unit tests;
2. relevant `lodestone-render` entity-pipeline tests;
3. targeted shell render/entity tests affected by batch field changes;
4. `cargo check -p lodestone-shell --all-targets`;
5. `cargo check -p lodestone-shell --no-default-features`;
6. `cargo check --workspace --all-targets` if the targeted checks are green and time permits.

Do not run `cargo fmt`; format only edited lines by hand. Diagnose failures before changing behavior.

## Task 5: Repeat the live benchmark and profile

Build the release client and run the identical Java-backed dense showcase at 2560×1440, render distance 24, unlimited frame cap, and VSync disabled. Record three trials for the same stationary and moving segments.

Compare against commit `8305a24f` using:

- `world.prepare_buffers` median and spread (primary);
- active CPU time and `world_encode_submit`;
- frame p50/p95/p99 and swapchain-acquire time;
- RSS peak;
- a new Samply profile's `Queue::write_buffer` and `StagingBuffer::new` stacks.

The frame median may remain at the 120 Hz 8.333 ms cadence. Accept the change only if preparation improves beyond trial noise and the repeated staging stack materially shrinks without a functional regression. If the arena loses, preserve the measurements and restore the pooled implementation in a follow-up commit rather than obscuring the result.

## Task 6: Document, index, and commit the verified result

**Files:**
- Modify: `docs/entity-rendering.md`
- Modify: `docs/client-frame-performance-2026-08-24.md`
- Regenerate: `docs/README.md`

Document arena ownership, append/upload ordering, range alignment, geometric growth, queue ordering, and the out-of-scope formats. Add exact before/after measurements, profiler attribution, limitations, and the next ranked bottlenecks to the performance report.

Run `cargo xtask docs-index`. Re-run the smallest relevant verification after any documentation code snippets change. Confirm the shared index has zero staged paths before committing explicit owned paths, inspect the commit stat, and report every repository-wide check not run.
