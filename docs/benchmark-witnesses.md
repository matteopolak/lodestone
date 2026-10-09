# Benchmark witnesses

## What it is

The shared native and browser benchmark observer records bounded evidence from production redraw attempts and surface submissions. It checks stationary camera, foreground, framebuffer, settings and current settlement over an independently declared coordinate domain, keeping square view counts only as diagnostics.

## How it works

- `app::benchmark_witness::BenchmarkWitness` consumes the benchmark phase clock. `begin_attempt` finalises an unresolved earlier attempt as skipped and keeps a small scalar snapshot; world and menu submission hooks resolve it at the surface handoff (a menu or skipped observation carries no world camera, and offscreen rendering certifies no handoff). Paced attempts keep scalar checks and a separate count but use no sample row. Failed acquisitions stay skips; missing world handoffs and excessive gaps reject acceptance. The foreground cap comes from declared graphics settings under options pacing, or is uncapped under the benchmark override.
- Sampling is at most every 100 ms across all phases, and `sample_due` must be checked before reading terrain coverage. The deadline advances from actual observation time, so a stall yields one late row. Storage holds 2,048 preallocated rows; overflow is counted and rejects acceptance. JSON is built once at completion or interruption.
- Every handoff updates finite-camera extrema and checks framebuffer aspect; every attempt checks foreground, session/loading/input state and typed settings. The stationary camera compares declared eye, wrapped yaw, pitch, vertical FOV and aspect; clip planes are checked for ordering. Player feet are separate from the render eye, including third person.
- Terrain acceptance checks dimension, simulation distance, declared server radius, requested render radius, centre and domain provenance. The full tracking domain must be resident; the render domain (a unique subset of it, excluding dependency-only buffers) must be resident and renderer-settled. Both come from external coordinates and dependency evidence, never observed counts or an inferred square; nonzero draws and empty queues cannot stand in. Explicit empty sections may settle without geometry.
- The report labels settlement `current-view-settlement-at-sample` and sets `continuous_settlement_proven` false: 10 Hz samples cannot prove no gap between them. Stability duration comes from consecutive successes; failed or null observations or large gaps reset it. Foreground event-provenance flags are kept (default pacer state alone is not independent confirmation). World, asset and protocol metadata are caller declarations. Queue telemetry reports waiting mesh columns and pending meshes.
- Phase summaries carry witnessed boundaries, expected durations, first and last sample times, max gap, attempts, world and menu handoffs, skips and the first offending frame. Missing phases, late boundaries, insufficient stability, overflow and interruption reject acceptance; a `complete` report can still have `accepted: false`.

## How to change it

- Keep scalar observation at production attempt and submission boundaries; add measurements to typed snapshots and do coverage reads only behind `sample_due`. Sample the camera actually passed to the renderer after projection adjustments, never one rebuilt from player statistics or reused from a skipped frame.
- The simulation facade counts tracking positions against the resident world and render positions against per-column renderer settlement under one world read, without testing settlement for tracking-only buffers. Domain IDs identify external provenance and must not be inferred from the loaded count. Native and Wasm share one collector and schema (only the output sink differs); the presentation-capture schema stays independent.
- Controls cover cadence, phase edges, stalls, capacity, and deliberately wrong yaw, position, framebuffer height, focus, settings, radius, centre and settlement. Live native and browser runs are still needed to show production events reach the inputs.

## Configuration

`WitnessHeader::from_json` parses the declaration once: `metadata` (object, at most 65,536 encoded bytes), `expected_settings` (complete typed graphics declaration), `requested_framebuffer`, `requested_camera` (`eye`, `yaw_degrees`, `pitch_degrees`, `fov_y_degrees`, `aspect`, `near`, `far`), `requested_radius`, `expected_declared_radius`, `expected_dimension`, `expected_simulation_distance`, `expected_center`, `expected_domain_id`, `expected_domain` (tracking) and `expected_render_domain`. Each domain has 1-4,096 unique absolute `[chunk_x, chunk_z]`; the render domain is a subset of tracking; server and client radii need not match. Optional `phase_durations_ms` has five entries (waiting-for-join, warmup, mutation, stationary, moving), each unsigned or `null`; the caller binds them to production durations, and acceptance needs a positive stationary duration.

`WitnessPolicy` defaults: 250 ms max sample gap and boundary tolerance, 100 ms minimum stable duration, 0.001 block/degree pose tolerance, 0.0001 projection tolerance; all appear in the report and change the evidence contract only. Native: `LODESTONE_BENCHMARK_WITNESS_SPEC` supplies the declaration and `LODESTONE_BENCHMARK_WITNESS` the report sink. Browser: `benchmark.witness`, with one `benchmark-witness-complete` report forwarded via the progress bridge. Independent of frame-profile dumps and presentation capture.

## Dependencies

`lodestone_time` through `crate::platform::Instant`, `lodestone_render::Camera`, the typed graphics declaration and `serde_json` at ingress and completion. Production supplies `FramePacer` foreground state, simulation view identity and exact-domain settlement. It starts no loop, worker or timer.
