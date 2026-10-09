# Heavyweight server profiling

## What it is

The `heavy-scene-server` example is a finite, release-built workload for observing integrated-server CPU paths that feed a heavyweight client scene. It uses the production `IntegratedServer`, `ChunkSource`, join batching and version protocol seam; it is a profiling aid, not a gameplay server.

## How it works

`HeavySceneSpec` builds deterministic setup, post-join and mutation command lists for palette, transparency, light, liquid, sign, block-entity, entity, scheduled or mixed scenes. `--emit-scene` writes one versioned JSON object with the ordered commands, witness requirements and a SHA-256 scene hash, which a client runner consumes without rebuilding the scene.

Runtime mode starts an in-memory integrated server over a retained deterministic source, drives a protocol-776 handshake over `DuplexStream`, drains the complete join view and chunk-batch markers, and writes one JSONL record (requested, installed and consumed counters plus platform, process, phase, timing, status and failure metadata). A wall deadline bounds the run and peer, readiness, serialisation and output failures are returned rather than leaving a task running.

Supported runtime slices are `palette`, `transparency`, `light`, `liquid` and `entity` in `ready` phase; scheduled, mutation and other scenarios are valid only for plan emission and are rejected at runtime until real producers and tick consumers exist (they must not be read as results).

- Palette measures setup placements and the joined chunk wire traffic. The terrain variants count states actually installed in the retained source, then only the matching cells from chunk coordinates decoded off the join wire (stained glass/panes, sea lanterns, water), compacted into a one-chunk runtime view so the deadline is not spent on halo encoding. These ready-phase counters prove source-to-wire reachability, not client translucent or water meshing or relight/remesh completion.
- Entity waits for the real mob-seeding handoff, inserts each bounded summon into the live mob simulation and counts the population snapshots and add-entity packets after the tick loop publishes them; it uses a one-column view and an empty mob terrain seed (entity snapshots are independent of chunk payload), and the wire reader checks each spawn lies in the planned region. An entity run with no producers disables natural spawning and must fail its witness while still serving non-empty chunks. Populations are capped at 2,048 (larger scales emit plans but are rejected before a live server starts).
- Consumed setup counters count only coordinates decoded from wire chunk packets, so prefetched columns raise installed counts but cannot satisfy an out-of-view witness. The terrain control removes every transparency producer and must fail the translucent encoded-cell witness while payloads stay non-empty.
- Witness columns (opaque and translucent terrain, water, signs, block entities, entities, particles, relight changes, remesh submissions) are anti-vacuity controls for the client runner, each with a declared minimum. The harness measures no GPU execution and defines no timing gate.

## How to change it

Extend `HeavyScenario`, its builder and `requirements_for_scenario` together. Keep command ordering and hashing deterministic and derive expected counts from the builder, not from observed output. `HeavySceneSpec::MAX_SCALE` bounds command volume before any allocation (raise it only with a resource check). The raw peer flow is in `heavy_scene.rs`, the release entrypoint in `examples/heavy-scene-server.rs`; keep the source retained so edits stay observable on later lookups.

## Configuration

Flags: `--scenario`, `--seed`, `--scale`, `--phase`, `--ticks`, `--output`, `--wall-deadline-secs`, `--camera-plan`, `--smoke`, and `--emit-scene <path|->` for the immutable handoff (`target/release/examples/heavy-scene-server --emit-scene - --scenario mixed --seed 17 --scale 1 > /tmp/heavy-scene.json`).

`heavy-server-emit` and `samply-heavy-server` recipes run in the foreground and write outside tracked source. `samply-heavy-server` builds the release example then runs `scripts/samply-heavy-server.py`, which invokes it twice: `--emit-scene` for the handoff JSON, and under Samply through the real entity path. Only scale 1 or 2 is allowed (1,024 or 2,048 live entities, the harness cap); the server wall deadline is at most 60 s with a second Samply process deadline. It refuses to overwrite artifacts and fails unless the compressed capture, its `*.json.syms.json` sidecar, the emitted scene and exactly one complete runtime JSONL row are non-empty and agree on scene identity and population. `just samply-heavy-server-smoke` is the 12-second verification run. `just validate-heavy-server-profile <capture>` repeats the coherence checks without launching anything (deriving the `*.scene.json` and `*.runtime.jsonl` sidecars from the `*.json.gz` name, requiring the entity spec and SHA-256 identity and a matching one-row population); it does not parse the Samply payload, so use `profile-cost-table.py` for symbols.

On macOS, Samply must be self-signed for process attachment: the runner checks the code signature before emitting anything, and on a missing `com.apple.security.cs.debugger` entitlement you run `samply setup` interactively once (and after each Samply update; no `sudo`, local executable only). The locally compiled release binary is a supported target; system-signed executables like `/usr/bin/true` are not valid Samply controls.

Each run prints unique paths under `bench-results/profiles/`:

```bash
samply load bench-results/profiles/heavy-server-entity-20260905T010203Z.json.gz
python3 scripts/profile-cost-table.py bench-results/profiles/heavy-server-entity-20260905T010203Z.json.gz
```

Samply 0.13.1 captures can be inspected with `threadCPUDelta` and sidecar metadata; worker threads are server work, so do not judge from the main thread. The workflow covers the `entity --phase ready` slice only (no mutation or scheduled-tick coverage).

The heavyweight *client* runner consumes the same scene JSON: its setup phase wraps the plan's exact producer commands in a temporary datapack function in the local oracle world, issues `reload`, and calls the function through RCON. Each producer contributes to a temporary aggregate only when its command succeeds, and the function returns the expected aggregate only if none failed or was omitted. The runner removes its directory and aggregate after the benchmark or a setup error; reload and execution share a 90-second deadline, so the dense smoke profile's 7,937 setup actions avoid 7,937 socket deadlines.

## Dependencies

`lodestone-server::IntegratedServer` with its `ChunkSource` and chunk encoder seams, `lodestone-v26-2`, `serde`/`serde_json`, `sha2`, Tokio, and Samply plus `profile-cost-table.py` for optional analysis.
