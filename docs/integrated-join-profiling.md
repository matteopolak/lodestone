# Integrated Join Profiling

## What it is

The `join_profile` binary is a finite, native profiling input for a real singleplayer join. It uses the same shell client, integrated server, protocol adapter, in-memory transport and client-owned world as the playable game.

## How it works

- It starts one deterministic seed and view radius, records the connection, login, first-chunk, first-resident and all-resident boundaries, then exits once the requested square is resident. It emits one `JOIN_PROFILE` JSON record: aggregate update counts, resident-column high water, poll activity, dimensions, initial-spawn probes and generation time, errors, shutdown time, the latest chunk-event time, the server-position chunk centre and the missing coordinates in the view square (at most 169, with a separate count and truncation flag).
- `generation_requests` counts raw ensure calls, request-session leaders, existing hits and packet-neighbour admissions, making a racing spawn warm-up auditable without per-column logging. The post-spawn retained-light warm-up submits its eight neighbours as one ordered batch through the production halo and materialiser boundary, leaving the centre out.
- `just samply-integrated-join` builds the release binary and wraps the run in `samply record`. `just profile-join-hardware integrated --radius 1` records the same run with macOS Instruments' CPU Counters template and summarises retired instructions, cycles, IPC, process identity and the `JOIN_PROFILE` phase boundaries. The template must expose `Cycles Instructions` in that order or the wrapper rejects it; `LODESTONE_JOIN_XCTRACE_TEMPLATE` selects a differently named template. Counters are process-level, so a phase duration must not be presented as a phase CPU-counter total.

## How to change it

Keep the client and server entry point in `join_profile.rs`; add metrics from an existing observable boundary or a bounded aggregate counter. Never log individual packets or columns, which changes the workload and hurts comparability. Use a fixed `--run-id` for before/after comparisons on one machine.

## Configuration

- Positional `seed`, `view_radius`, `deadline_seconds` default to `4242`, `1`, `240`; radius is capped at 32. The wrapper takes `--seed`, `--radius`, `--deadline-seconds`, `--output-dir`, `--run-id`, `--dry-run`; the hardware wrapper takes `integrated` or `client` first plus `--template`.
- `missing_view_coordinates` holds `[x, z]` chunk pairs centred on the server-known player position (or `(0, 0)` before one exists); `latest_chunk_event_ms` is `null` if no chunk arrived.
- `LODESTONE_JOIN_PROFILE_WORLD_DIR` reopens a persistent world instead of a throwaway in-memory one; the server may modify it, so use a disposable copy.
- `LODESTONE_JOIN_TRACE=1` and `LODESTONE_WORLDGEN_LEDGER_TRACE=1` capture sampled join-stage events and failure-only retained-state comparisons; `join_profile` then installs a stderr subscriber (warnings plus the join trace, overridable with `RUST_LOG`). Each traced generation request is tagged `existing`, `snapshot` or `fallback` before its `generated` event, and mob seeding logs reused full columns over the seed-area total.

## Dependencies

`lodestone-shell`'s public `NetClient`, `lodestone-registry`'s selected server protocol, `serde_json`, and Samply when wrapped. Native-only; Wasm keeps a harmless diagnostic main.
