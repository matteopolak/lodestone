# Section camera uniform (shared buffer + origin arena)

## What it is

The group-0 camera binding the model/fluid terrain pipelines
([`ModelPipeline`](../crates/lodestone-render/src/model_pipeline.rs)) read from,
split into a **shared** per-frame half (view-projection + fog) and a
**per-section** half (world origin) that lives in one physically resident
[`ArenaBuffer`](../crates/lodestone-render/src/arena.rs) and is addressed by a
dynamic offset at draw time, instead of one small buffer + one bind group per
section. This is the fix for issue #75.

## How it works

### The problem it replaced

Every `SectionGpu`/`ModelSectionGpu` used to carry its own `cam_buffer` +
`cam_bind_group`, and `RenderState::render_inner` rewrote **every resident
section's whole camera uniform** — `view_proj` bytes included — with
`queue.write_buffer`, once per section, every frame. A `samply` profile of a live
session (~94 s of play, `debug = 2` in `[profile.release]`, no codegen change)
found:

- Main thread CPU-saturated: 88.4 s CPU over 93.6 s wall (94% of one core, 76% of
  all process CPU).
- 93.4% of main-thread CPU inside `RenderState::render_inner`, of which
  `Queue::write_buffer` was **52.9%** (36.4% just `StagingBuffer::new` →
  `create_buffer`) and `Queue::submit`/`Device::maintain`/`command_encoder_finish`
  another ~58% combined (these overlap; the point is it is all API-call-count
  overhead, not data volume).
- The status line reported `sections=3880`, peaking near 5000 — so this was
  ~4000–5000 `write_buffer` calls per frame for data that is **almost entirely
  constant**: `view_proj` is identical for every section every frame, and
  `section_origin` is the section's fixed world position, constant for its whole
  lifetime. The loops were also uncapped, running over every resident entry, not
  the visible set.

### The fix: two bindings in group 0, one of them dynamic

`ModelPipeline::camera_layout` now has two bindings instead of one:

- **Binding 0** — `ModelSharedCameraUniform` (`view_proj` + this frame's
  `FogUniform`). Non-dynamic. Written **once per frame** via
  `update_model_shared_camera_buffer`, from the top of
  [`RenderState::render_inner`](../crates/lodestone-shell/src/gpu.rs).
- **Binding 1** — `SectionOriginUniform` (`vec4(origin.xyz, 0)`). **Dynamic
  offset.** Backed by one [`ArenaBuffer`](../crates/lodestone-render/src/arena.rs)
  (wrapped as `SectionOriginArena` in `gpu.rs`), sized in slots of
  `max(device.limits().min_uniform_buffer_offset_alignment, ArenaBuffer::MIN_ALIGN)`
  bytes — checking the *limit*, not the adapter, the same rule this repo's
  4-bind-group-floor note already established. Written **once**, when a section
  is uploaded (`RenderState::upload_section`), and never again for that section's
  lifetime. A remesh of an already-resident coord reuses its existing slot rather
  than reallocating (the origin is a pure function of the section key, so it
  never actually changes).

One bind group (`ModelRenderer::cam_bind_group`) is built **once**, over the
shared buffer and the whole arena buffer. Every section draw — opaque, water,
and the dropped-item pass, which all share the world camera and this frame's fog
— reuses that *same* bind group, varying only the dynamic offset passed to
`set_bind_group`:

```rust
pass.set_bind_group(0, &model.cam_bind_group, &[section.origin_alloc.offset() as u32]);
```

This is why the design is "the bigger win" of the two shapes the issue proposed
(a second binding with its own buffer *per section*, vs. one arena addressed by
offset): it removes not just the ~4000 writes/frame but the ~4000 separate
buffers and bind groups too, which is what was driving the `Storage::get`,
`update_bind_group_state` and `open_pass` overhead alongside the writes.

### Reserved zero slot

Slot 0 of the arena is permanently allocated and zeroed at construction
(`SectionOriginArena::zero_offset`). The dropped-item pass and the first-person
held-item pass both mesh their geometry with world positions already baked into
the vertices (like the mining-crack pass, which is untouched — see below), so
their "origin" is always zero; they bind the shared arena at that fixed offset
instead of needing a buffer of their own. The held item still needs its **own**
buffer at *binding 0* (`ModelRenderer::hand_cam_buffer`), because its `view_proj`
genuinely differs — `hand_projection` alone, no view matrix — but its binding 1
still points at the shared arena.

### What did *not* change

- **`CrackPipeline`** (the mining-crack overlay) has its own, independent
  single-binding `camera_layout` and was never part of the measured cost (it
  writes one buffer, once per frame, regardless of section count). Untouched.
- **`EntityPipeline`** already drew every entity through one shared camera
  buffer and per-instance vertex data — it was never the one-buffer-per-entity
  shape this fix addresses. Untouched.
- **The packed/demo-world path** (`BlockPipeline`, `RenderState::sections`)
  still gives every section its own camera buffer, rewritten every frame. It is
  the same shape the model path used to have, but the demo world's section count
  is bounded by `MAX_WORLD_RADIUS` (6, in `lodestone-shell/src/sim.rs`) — a few
  thousand sections at most — and it never runs in live play, so it was not the
  measured cost and is out of this fix's scope.
- **`lodestone-render/src/{section_arena.rs,arena.rs}`**'s existing
  `SectionArena`/`ArenaBuffer` types were not modified; `ArenaBuffer` is reused
  as-is for the new origin arena (it already suballocates equal-size regions out
  of one GPU buffer with a free list — exactly what a slot arena needs).

## How to change it

- **Bind group / offset shape lives in `ModelPipeline::build`**
  (`model_pipeline.rs`): the `camera_layout` descriptor, and
  `ModelPipeline::camera_bind_group(device, shared_buffer, origin_buffer)`.
  Both `ModelPipeline::new` (opaque) and `ModelPipeline::for_fluid` (water) build
  this same shape, so a layout change must be made once and applies to both.
- **The WGSL `Camera`/`Origin` structs** live in `MODEL_WGSL` and `FLUID_WGSL` in
  the same file. If you change `ModelSharedCameraUniform` or
  `SectionOriginUniform`'s Rust layout, update both WGSL structs' field order and
  padding to match — a silent mismatch shows up as geometry in the wrong place,
  not a validation error.
- **The arena and its capacity** live in `gpu.rs` as `SectionOriginArena` and
  `MODEL_ORIGIN_ARENA_SLOTS`. Capacity is a **fixed ceiling, not a growable one**
  — see that type's doc comment for the sizing margin. If a render-distance
  increase ever pushes past it, `SectionOriginArena::alloc` returns `None` and
  `RenderState::upload_section` logs a warning and drops that one section's
  geometry rather than panicking; raising `MODEL_ORIGIN_ARENA_SLOTS` is the fix,
  not a growable-arena rewrite (unless the margin turns out to be wrong in
  practice — profile before assuming it).
- **Every consumer of `ModelPipeline::camera_bind_group`** now needs two
  buffers, not one, and every `set_bind_group(0, …)` against it now needs a
  one-element dynamic-offset array, even where the offset is always `0` (a
  one-off draw with its own single-slot origin buffer built with
  `section_origin_buffer`). This touches more than `gpu.rs`: the HUD's 3-D item
  icons (`lodestone-shell/src/hud/item_icon.rs`) and every `lodestone-render`
  pixel gate that exercises `ModelPipeline` directly build a single permanent
  zero-origin slot the same way.
- **`RenderState::upload_section` and `remove_section` now take a `queue`**
  (upload did not need one before, since the old per-section buffer used
  `device.create_buffer_init`; the new path writes into an existing arena slot,
  which needs `queue.write_buffer`). Every call site was updated; a new one must
  supply it too.

## Configuration

Nothing user-facing. `MODEL_ORIGIN_ARENA_SLOTS` (`gpu.rs`) is the one constant
worth knowing about — see "How to change it" above.

## Dependencies

- `wgpu`'s dynamic-uniform-offset binding (`has_dynamic_offset: true`,
  `min_binding_size`, and the offset array on `set_bind_group`).
- `device.limits().min_uniform_buffer_offset_alignment`, queried rather than
  hardcoded — this repo has already been burned once by checking the adapter
  instead of the limit (the model shader's 4-bind-group floor).
- [`lodestone_render::arena::ArenaBuffer`](../crates/lodestone-render/src/arena.rs)
  and its `Suballocator` — reused unmodified for the origin arena's slot
  allocation and free list.
