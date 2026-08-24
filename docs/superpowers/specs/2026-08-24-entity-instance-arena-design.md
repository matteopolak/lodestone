# Design: Shared entity instance upload arena

## What it is

This change replaces Lodestone's per-batch tinted-instance GPU uploads with one
frame-owned CPU byte arena and one retained GPU vertex buffer. Entity, armour,
cape, elytra, painting, orb, sprite, spawner-mob, banner, and block-entity batches
keep byte ranges into that shared buffer instead of owning separate buffers.

The goal is to turn dozens of steady-state `Queue::write_buffer` calls and native
wgpu staging allocations into one upload without changing visibility, batch keys,
draw order, instance contents, or rendered pixels.

## Why this design

The Java-backed dense showcase measured `world.prepare_buffers` at about 1.1–1.2
ms after buffer pooling. The remaining CPU profile is dominated by
`Queue::write_buffer` and wgpu's native `StagingBuffer::new`. wgpu defers these
copies until the next submit, but each native `write_buffer` call still creates a
fresh staging allocation. `write_buffer_with` avoids an intermediate caller-side
copy but has the same staging behavior.

Three approaches were considered:

1. Keep one destination buffer per batch and use `wgpu::util::StagingBelt`.
   This reuses large mapped staging chunks but retains one copy command per batch
   and the fragmented destination layout.
2. Pack all tinted instances into one Lodestone-owned frame arena and upload it
   once. This removes both per-batch staging allocations and per-batch destination
   management while preserving the existing draws through buffer slices.
3. Write directly into a persistently mapped primary vertex buffer. This depends
   on optional `MAPPABLE_PRIMARY_BUFFERS` support and is not a portable baseline.

The second approach is selected because it addresses the measured allocation and
submission pattern with ordinary `VERTEX | COPY_DST` buffers on every backend.

## How it works

`InstanceBufferArena` owns a retained `Vec<u8>`, an optional `wgpu::Buffer`, and
the GPU buffer's capacity. `begin_frame` clears the byte vector without releasing
its allocation. Each tinted-instance preparation appends the existing
`EntityInstanceRaw` bytes and receives an exact `Range<u64>` covering its data.
Empty instance lists still return `None`.

After the final tinted batch is prepared, `upload` ensures that the GPU buffer can
hold the complete arena. Capacity grows geometrically to at least the next power of
two and never shrinks during the arena's lifetime. The method then makes exactly one
`Queue::write_buffer` call for a non-empty frame and returns a cloned shared buffer
handle for command encoding.

Every affected draw binds `shared_buffer.slice(batch.range.clone())` at vertex slot
1. Range order follows preparation order, while draw order remains whatever the
existing render pass specifies. The CPU and GPU buffers may retain unused capacity,
but only the populated byte range is uploaded and only each batch's exact range is
bound.

`EntityInstanceRaw` is 76 bytes, a multiple of wgpu's four-byte copy and vertex
buffer alignment. Because the arena starts at zero and appends only whole instance
records, every range boundary is valid without padding. Unit tests make this
alignment invariant explicit.

The upload happens before render-pass encoding and before the queue submission that
uses it. Queue ordering guarantees that the copy precedes the draws in that submit
and that a later frame's copy is ordered after earlier submitted use of the same
buffer. A multi-buffer frame ring is intentionally deferred unless measurement
shows a resource hazard or GPU serialization attributable to this reuse.

## Data flow

```text
visible tinted objects
  -> existing transform/light/tint conversion
  -> append EntityInstanceRaw bytes to CPU arena
  -> store exact byte range on existing draw batch
  -> one Queue::write_buffer for the populated arena
  -> bind shared GPU buffer slice for each existing draw
  -> unchanged indexed/instanced draws
```

## Error handling and invariants

- An empty frame performs no upload and returns no shared GPU buffer.
- An empty individual batch returns `None` and contributes no range.
- Checked integer conversion rejects an arena too large to represent as `u64`;
  normal GPU allocation and validation failures remain wgpu errors, as today.
- The retained GPU buffer is replaced only when the required bytes exceed its
  capacity. Existing ranges are offsets, so growth before encoding does not alter
  them.
- All appends must finish before `upload`; appending after upload in the same frame
  is a lifecycle error guarded in debug builds and covered by tests.
- Draw code never binds a range without the shared buffer returned by that frame's
  upload.

## Scope

This arena covers only the existing `EntityInstanceRaw` tinted-instance path.
Flames, shadows, fishing lines, water masks, map textures, uniforms, and other
dynamic formats retain their current buffers. Fine-grained GPU timestamp capture,
present-mode selection, static geometry caches, and multi-draw changes are separate
measurement-driven work.

## Testing and performance acceptance

GPU-independent unit tests cover contiguous ranges, empty appends, four-byte
alignment, begin-frame reuse, geometric capacity growth, and the append-after-upload
lifecycle guard. Existing entity/render tests cover conversion and draw-batch
behavior. Targeted shell checks cover all migrated callers and the version-free
seam.

The performance acceptance test repeats the same 2560 x 1440, render-distance-24
dense Java showcase used for the pooled-buffer baseline. The primary metric is
`world.prepare_buffers`; secondary evidence is active CPU frame time, world encode
time, frame percentiles, RSS, and a new Samply profile. Success requires a
repeatable improvement beyond trial spread and removal or material reduction of the
per-batch `Queue::write_buffer` / `StagingBuffer::new` stack. The 8.333 ms frame
median may remain unchanged because swapchain acquisition currently paces the run
at the 120 Hz display cadence.

## How to change it

Add another instance format only when it has the same lifetime and upload cadence;
do not mix differently aligned records into this arena without adding explicit
padding. New tinted-instance producers must append before the single upload point
and carry a range rather than creating a GPU buffer.

If a future backend exposes a cheaper mapped path, change only the arena's upload
implementation while keeping its append/range contract. If measurements show the
single retained buffer stalls across frames, introduce a small ring inside the
arena rather than spreading frame-slot ownership back through every batch type.

## Configuration

The arena is always enabled and has no player-facing option. CPU and GPU capacities
grow from workload demand and persist for the `RenderState` lifetime. Benchmark
configuration and artifact paths remain those documented in
`client-frame-performance-2026-08-24.md`.

## Dependencies

- `lodestone-render::EntityInstanceRaw` and its existing tinted-instance conversion.
- `wgpu::Buffer`, `Device`, and `Queue` using `VERTEX | COPY_DST` usage.
- `lodestone-shell::RenderState` for begin-frame, preparation, one upload, and draw
  orchestration.
- The Java-backed showcase benchmark and Samply workflow documented in
  `client-frame-performance-2026-08-24.md`.
