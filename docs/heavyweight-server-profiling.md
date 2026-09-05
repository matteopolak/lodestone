# Heavyweight server profiling

## What it is

The `heavy-scene-server` example is a finite, release-built workload for
observing integrated-server CPU paths that feed a heavyweight client scene. It
uses the production `IntegratedServer`, `ChunkSource`, join batching, and
version protocol seam; it is a profiling aid, not a gameplay server.

## How it works

`HeavySceneSpec` builds deterministic setup, post-join, and mutation command
lists for palette, transparency, light, liquid, sign, block-entity, entity,
scheduled, or mixed scenes. The `--emit-scene` mode writes one versioned JSON
object containing those ordered commands, witness requirements, and a SHA-256
scene hash. A client runner can consume that object without rebuilding the
scene.

Runtime mode starts an in-memory integrated server with a retained deterministic
source, drives a protocol-776 handshake over `DuplexStream`, drains the complete
join view and its chunk-batch markers, and writes one JSONL record. The record
contains requested, installed, and consumed counters plus platform, process,
phase, timing, status, and failure metadata. A wall deadline bounds the run;
peer, readiness, serialization, and output failures are returned instead of
leaving a server task running.

The supported runtime slices are `palette` and `entity` in `ready` phase.
Palette measures setup block placements and the resulting joined chunk wire
traffic. Entity waits for the real mob-seeding handoff, inserts each bounded
summon through `IntegratedServer::spawn_mob`, and counts the resulting
population snapshots and add-entity packets after the integrated tick loop has
published them. The wire reader also checks that every observed spawn lies in
the planned entity region. An entity run with no producers disables natural
spawning and must fail its entity witness despite still serving non-empty chunk
payloads. Runtime entity populations are capped at 2,048; larger scales remain
valid for immutable plan emission but are rejected before a live server starts.
Scheduled, mutation, and other scenario names remain valid for
immutable plan emission, but runtime mode rejects them until their real server
producers and tick consumers are wired; they must not be treated as profiling
results.

Consumed setup counters are restricted to the coordinates decoded from each
wire chunk packet. Prefetched source columns therefore contribute to installed
counts but cannot make an out-of-view setup pass the palette witness.

The witness columns are anti-vacuity controls for the separate client runner:
opaque terrain, translucent terrain, water, signs, block entities, entities,
particles, relight changes, and remesh submissions must each meet their declared
minimum. The harness does not measure GPU execution and does not define a timing
gate.

## How to change it

Extend `HeavyScenario`, its builder, and `requirements_for_scenario` together
when adding a workload. Keep command ordering and scene hashing deterministic;
derive expected counts from the builder rather than from the observed output.
`HeavySceneSpec::MAX_SCALE` bounds command volume before any builder allocates a
plan; raise it only with a focused resource check. The raw peer flow belongs in
`heavy_scene.rs`, while the release entrypoint
belongs in `examples/heavy-scene-server.rs`. Keep the source retained so edits
remain observable on a later lookup.

## Configuration

The example accepts `--scenario`, `--seed`, `--scale`, `--phase`, `--ticks`,
`--output`, `--wall-deadline-secs`, `--camera-plan`, and `--smoke`. Use
`--emit-scene <path|->` for the immutable handoff artifact. For example:

```bash
target/release/examples/heavy-scene-server --emit-scene - --scenario mixed --seed 17 --scale 1 > /tmp/heavy-scene.json
```

The `heavy-server-emit` and `samply-heavy-server` recipes stay foreground and
write captures/results outside tracked source. `profile-heavy-server` invokes
the existing `scripts/profile-cost-table.py` analyzer. The Samply recipe requires
the compressed capture, its `heavy-server.json.syms.json` presymbolication
sidecar, and the runtime JSONL record to be non-empty; a missing artifact fails
the recipe. Samply 0.13.1 captures may be inspected with their `threadCPUDelta`
data and sidecar metadata; worker threads are part of the server work, so do not
judge from the main thread alone. The recipe currently profiles the supported
`palette --phase ready` slice; the entity slice is available for bounded smoke
runs, but the recipe does not claim mutation or scheduled-tick coverage.
Recipe parameters are positional (`just samply-heavy-server palette 1 1`), as
required by `just`; the three values are scenario, seed, and scale.

## Dependencies

The harness relies on `lodestone-server::IntegratedServer`, its `ChunkSource`
and chunk encoder seams, `lodestone-v26-2` for the concrete wire protocol,
`serde`/`serde_json` for the plan and JSONL records, `sha2` for the scene hash,
Tokio for bounded async execution, and Samply plus the profile-cost-table script
for optional local capture analysis.
