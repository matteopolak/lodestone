# Benchmark witnesses

## What it is

The shared native/browser benchmark observer records bounded evidence from production redraw attempts and surface submissions. It checks stationary camera, foreground, framebuffer, settings and current settlement over an independently supplied coordinate domain, while retaining square view counts as diagnostics.

## How it works

`app::benchmark_witness::BenchmarkWitness` consumes the existing benchmark phase clock. `begin_attempt` finalizes an unresolved preceding attempt as skipped, observes phase edges and retains a small scalar snapshot. World and menu submission hooks resolve that attempt at the existing surface handoff; a menu or skipped observation carries no world camera. Offscreen rendering does not certify a surface handoff.

Intentional paced attempts retain scalar checks and a separate count but consume no sample row or deadline. Failed acquisitions remain unresolved skips; missing world handoffs and excessive sample gaps reject acceptance. The required foreground cap comes from the declared graphics settings under options pacing, or is uncapped under the explicit benchmark override.

The observer samples no faster than every 100 ms across all phases. `sample_due` must be checked before reading terrain coverage. A sample advances the deadline from its actual observation time, so a stall creates one late row rather than catch-up rows. Storage is preallocated for 2,048 rows; overflow is counted and rejects acceptance. JSON is constructed once at completion or interruption, outside the frame hot path.

Every world handoff updates finite-camera extrema and checks framebuffer aspect. Every attempt checks foreground state, session/loading/input state and the typed settings comparison. Stationary camera comparison uses independently declared eye position, wrapped yaw, pitch, vertical FOV and aspect. Clip planes are recorded and checked for valid ordering. Player feet are separate from the actual render eye, including third-person views.

Terrain acceptance checks dimension, simulation distance, independently declared server radius, requested render radius, center and domain provenance. The full tracking domain must be resident; a separately declared render domain must be resident and renderer-settled. Render targets are a unique subset of the resident domain, excluding any dependency buffer that is not a render target. Both domains come from external coordinates and dependency evidence, not observed counts or an inferred square. Nonzero draws and empty queues cannot substitute for coordinate coverage. Explicit empty terrain sections may settle without geometry.

The report labels settlement `current-view-settlement-at-sample` and sets `continuous_settlement_proven` to false. Successful consecutive samples determine the retained stability duration; failed/null observations or excessive gaps reset it. A 10 Hz sample does not prove that no terrain gap occurred between samples. Foreground event-provenance flags are retained; default pacer state alone is not independent confirmation of browser foreground. World, asset and protocol metadata are caller declarations that need external confirmation; the collector does not independently establish those identities. Queue telemetry reports waiting mesh columns and pending meshes.

Required phase summaries include witnessed start/end boundaries, expected durations, first/last sample times, maximum gap, attempts, world/menu handoffs, skips and the first offending frame. Missing phases, late boundaries, insufficient stability, overflow and interruption reject acceptance. A `complete` report can still have `accepted: false`; completion describes collection, not readiness.

## How to change it

Keep scalar observation at production attempt/submission boundaries. Add measurements to typed snapshots, and perform coverage reads only behind `sample_due`. Sample the local camera actually passed to the renderer after projection adjustments. Never reconstruct it from player statistics or reuse a prior camera for a skipped frame.

The simulation facade counts borrowed tracking positions against the resident world and render positions against current per-column renderer settlement. It holds one world read and does not test renderer settlement for tracking-only buffers. Domain IDs identify external provenance; they must not be inferred from the current loaded count. Preserve the same collector and schema on native and Wasm; only the final output sink differs. The existing presentation-capture schema and columns remain independent.

Focused controls cover cadence/phase edges/stalls/capacity plus independent fixture camera/domain values and deliberately wrong yaw, position, framebuffer height, focus, settings, radius, center and settlement. Live native/browser controls remain necessary to establish that actual production events reach these inputs.

## Configuration

`WitnessHeader::from_json` parses a declaration once. It requires `metadata` (an object capped at 65,536 encoded bytes), `expected_settings` (the complete typed benchmark graphics declaration), `requested_framebuffer`, `requested_camera`, `requested_radius`, `expected_declared_radius`, `expected_dimension`, `expected_simulation_distance`, `expected_center`, `expected_domain_id`, `expected_domain` (tracking residency) and `expected_render_domain`. Each domain contains 1–4,096 unique absolute `[chunk_x, chunk_z]` positions. The render domain must be a subset of the tracking domain. Server and client radius declarations need not be equal.

`requested_camera` contains `eye`, `yaw_degrees`, `pitch_degrees`, `fov_y_degrees`, `aspect`, `near` and `far`. Optional `phase_durations_ms` has five entries ordered waiting-for-join, warmup, mutation, stationary, moving; each is unsigned milliseconds or `null`. The caller binds these declarations to the production benchmark durations. A positive stationary duration is required for acceptance.

`WitnessPolicy` defaults to a 250 ms maximum sample gap and boundary tolerance, 100 ms minimum stable sampled duration, 0.001 block/degree pose tolerance and 0.0001 projection tolerance. All policy values appear in the report. Changing tolerances changes the evidence contract, not production pacing or loading gates.

Native benchmark declarations are opt-in via `LODESTONE_BENCHMARK_WITNESS_SPEC`; the separate `LODESTONE_BENCHMARK_WITNESS` sink writes the completed report. Browser declarations use `benchmark.witness`, and the existing progress bridge forwards one `benchmark-witness-complete` report. These declarations and outputs are independent of frame-profile dumps and presentation-capture timing.

## Dependencies

The observer uses `lodestone_time` through `crate::platform::Instant`, `lodestone_render::Camera`, the existing typed benchmark graphics declaration and `serde_json` at ingress/completion. Production callers provide `FramePacer` foreground state, simulation view identity and exact-domain settlement after mesh uploads/readiness refresh. It starts no render, tick, worker or timer loop.
