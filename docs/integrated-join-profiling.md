# Integrated Join Profiling

## What it is

The `join_profile` binary is a finite, native profiling input for a real
singleplayer join. It uses the same shell client, integrated server, protocol
adapter, in-memory transport, and client-owned world used by the playable game.

## How it works

The harness starts one deterministic seed and view radius, records the actual
connection, login, first-chunk, first-resident, and all-resident boundaries,
then exits after the requested square is resident. It emits one `JOIN_PROFILE`
JSON record containing aggregate update counts, resident-column high water,
poll activity, dimensions, initial-spawn probes and generation time, errors,
and shutdown time. It also reports request-boundary counters for raw ensure
calls, request-session leaders, Existing hits, and packet-neighbour admissions;
these counters make a racing spawn warm-up auditable without enabling a
per-column log. The post-spawn retained-light warm-up submits its eight
neighbours as one ordered generation batch, sharing the production halo and
materializer boundary while leaving the centre out of the request list.
`just samply-integrated-join` builds the release binary and wraps the same run
with `samply record`.

For instruction-denominated evidence, `just profile-join-hardware integrated
--radius 1` records the same bounded run with macOS Instruments' CPU Counters
template. Its summary combines retired instructions, cycles, IPC, process
identity, and the phase boundaries from the `JOIN_PROFILE` record. Use a fixed
`--run-id` for before/after comparisons; set `LODESTONE_JOIN_XCTRACE_TEMPLATE`
when a local Instruments template exposes the desired counter pair under a
different name. The selected template must expose `Cycles Instructions` in
that order; the wrapper rejects other counter layouts instead of mislabelling
their values.

## How to change it

Keep the client and server entry point in `join_profile.rs`; adding a metric
should use an existing observable boundary or a bounded aggregate counter.
Avoid logging individual packets or columns because that changes the workload
and makes captures harder to compare. Use a fixed `--run-id` when comparing
captures from the same machine and configuration.

## Configuration

The `generation_requests` object contains `raw_ensure_calls`,
`request_session_leaders`, `existing_hits`, and
`packet_neighbour_admissions`. The binary accepts positional `seed`,
`view_radius`, and `deadline_seconds`;
the defaults are `4242`, `1`, and `240`. The radius is capped at 32. The
wrapper accepts the same workload as `--seed`, `--radius`, and
`--deadline-seconds`, plus `--output-dir`, `--run-id`, and `--dry-run`.
The hardware wrapper takes `integrated` or `client` first and accepts those
options plus `--template`. It keeps wall-time phase markers because the CPU
counter table is process-level; a phase duration must not be presented as a
phase CPU-counter total.

## Dependencies

The harness depends on `lodestone-shell`'s public `NetClient` surface,
`lodestone-registry`'s selected server protocol, `serde_json` for the single
record, and Samply only when the wrapper is used. It is native-only as a
profiling workload; the Wasm target retains a harmless diagnostic main.
