# Oracles and benchmarks: runtimes, fuzzing, and real-workload measurement

## What it is

Where this repo gets ground truth and cost measurements from outside its own code: the Apple `container` runtime every JVM and vanilla-server oracle runs under, the property-based fuzz harness for wire decoders, the redstone benchmark harness that measures the real tick loop on downloaded contraptions, and the PGO experiment.

## How it works

### Oracle runtimes: Apple `container`, not Docker

Every JVM-oracle script under `scripts/live-oracles/` and `scripts/worldgen-oracle/`, and the Rust tests that once shelled out to `docker`, run under Apple's `container` CLI. There is no runtime switch and no Docker fallback. It boots faster (about 24 s vs 40 s) and releases its VM reservation on `stop` (roughly 1.1 to 1.3 GB resident while an oracle runs vs about 3 GB for Docker Desktop; about 50 MB residual afterwards vs over 2.5 GB).

Traps every script accounts for:

- Never publish a port with a host-IP prefix (`-p 127.0.0.1:PORT:PORT`): it accepts the connection and resets on the first byte. Bare `-p PORT:PORT` works (all interfaces, same as under Docker). Re-verify this relay after any `container` upgrade.
- An explicit image pull must pass `--platform linux/arm64`, or it fetches the whole multi-arch manifest.
- `--memory 3g` is required for every JVM script (the per-VM default is 1 GiB).
- `container logs` has no `--since`, only `-n <lines>`; scripts poll a fixed generous line count.

`scripts/live-oracles/lib.sh` reads `mc-version`, syncs the matching server jar into the oracle world, and forces `white-list=false`. The creative and survival oracles both listen on 25565 (RCON 25566), so run one at a time. Shell live gates connect with `lodestone::config::DEFAULT_PROTOCOL`.

`crates/lodestone-server/tests/entity_nbt_vanilla_oracle.rs` checks our entity-chunk decoder against `.cache/mc/survival/entity-census.json`, written by the stdlib-only `scripts/live-oracles/entity-census.py` from `.cache/mc/survival/world/entities`. The world is live (every survival gate and session adds entities), so rerun the script after touching it; a missing census makes the ignored tests panic with that instruction.

### Physics golden-trace shards

The physics suite replays 47 deterministic scenarios against the independent Python oracle `crates/lodestone-physics/tests/gen_golden.py`, one generated Rust file per scenario under `tests/support/golden_traces/` (`mod.rs` owns `GoldenTick`). Regenerate with `python3 crates/lodestone-physics/tests/gen_golden.py`; the drift gate `... --check` fails on a missing shard, a stale `.rs` file or any byte difference. The JVM movement comparison reads the same directory; add a scenario to `SCENARIOS`, never hand-edit shards.

### Fuzz harness (`lodestone-fuzz`)

`proptest` (a dev-dependency, not `cargo-fuzz`) checks properties needing no expected value: a decoder never panics on arbitrary bytes, a truncated valid packet errors cleanly, and a length prefix never forces an allocation disconnected from the bytes available. It complements round trips, which are only as strict as the shared misunderstanding between their halves. Every call goes through a `catch_unwind` wrapper (a direct panic would abort the test process rather than report one shrunk case). The four clientbound families and `v26-2`'s serverbound decode read the generated packet-id tables, so fuzzed ids track the generator. The frame `Codec` (length prefix, zlib) is fuzzed with a termination bound.

It found two bugs. A generated `decode_vec` preallocated at a wire-supplied length before checking remaining bytes (an 8-byte packet drove a 48 MB allocation); the fix caps preallocation at `len.min(reader.remaining())`, closing all thirteen affected fields in one macro edit. An unchecked `i32` multiply on a chunk coordinate in `multi_block_change` overflowed (panic in debug, wrong block in release); the fix is `checked_mul` plus an explicit refusal past the world-border bound, not a clamp (a clamp invents a position as the wrap did). Both are pinned by committed byte fixtures beside the seed file, and the harness's control test runs a deliberately buggy decoder to prove the panic wrapper fires (and does not for an in-bounds call).

### Redstone benchmark harness

`lodestone_anvil::schematic` (Litematica, Sponge, vanilla-structure containers) plus `crates/lodestone-anvil/tests/redstone_benchmark.rs` (`#[ignore]`d) load a real downloaded contraption into the production tick loop to measure neighbour-scan cost, built to decide whether an incrementally-invalidated redstone dependency graph is worth building. It reports whole-loop `TickStats` (context only, wall clock on a contended machine) and `redstone_counters`' process-global load-independent counts (notifications, cell reads, signal queries, wire recomputes) per elapsed tick.

A raw block-source load does not reproduce a neighbour-update cascade, so a fresh circuit sits at captured steady state with zero scan cost. Re-injecting the schematic's pending block ticks through the production scheduled-tick path closes the gap: on one farm, resuming two repeater ticks cascaded into hundreds of notifications and thousands of block-state reads in one tick, which is where a dependency graph would replace scanning. Fixtures are gitignored (internal benchmarking only) and tracked with source URL and a licence-clarity note (none carries an explicit license).

### Benchmark targets and helpers

Each benchmark crate sets `autobenches = false` and lists every benchmark with `[[bench]]` and `harness = false`, so `benches/support.rs` stays a module. A new benchmark needs its manifest entry; a helper stays a module behind an existing target.

### Process instruction counters

`lodestone_testsupport::process_counters::ProcessCounters` (feature `bench-record`, macOS) reads the process's retired instructions and cycles through `libc` (unrelated threads in the process are included), using the SDK's 296-byte v4 record and rejecting unavailable or backwards counters. Keep reads outside the compared operation, alternate pair order, and `black_box` outputs. Calibrate before trusting a comparison:

```sh
cargo test --release -p lodestone-testsupport --features bench-record --lib \
  retired_counter_distinguishes_noop_and_fourfold_arithmetic \
  -- --ignored --nocapture --test-threads=1
```

The control subtracts no-op overhead and predicts four times the count for four times the iterations. It validates accounting, not cache misses, port contention or per-thread attribution. Extend the helper rather than copying another syscall record.

### PGO experiment

Answer: not yet worth landing as a default, worth pursuing. A worldgen probe measured instructions retired (tighter run to run than any wall-clock on this shared machine) and showed a 14.6% reduction against the thin-LTO, single-codegen-unit baseline, with the output checksum unchanged. Caveats: one scene, one machine; a fixture data tree rather than embedded production data; worldgen only; and the two-pass build (instrument, train, re-optimise) is an unquantified maintenance cost, so it stays a separate pipeline (`just pgo-instrument` / `pgo-merge` / `run-pgo` / `build-pgo`), not a release-profile change.

## How to change it

- **New oracle script**: copy `creative.sh`: idempotent `container system start`, force-remove the named container, bare `-p`, `--memory 3g`, readiness by grepping the container log.
- **Fuzz family or state**: extend the one `Family`/`STATES` table (an enum variant plus one arm each in the adapter constructor and entry-table accessor).
- **Fuzz finding**: commit the exact bytes as a hex fixture and assert what the decoder does with them (a silent wrap or invented clamp both satisfy "did not panic").
- **Allocation counters** measure the whole test binary; scope them with `thread_local!`, not a shared `Mutex`.
- **Redstone fixture**: record source URL, author and licence note in the same commit.
- **Durations on this shared machine are not results**; re-run alone on a quiet machine, or use counters.

## Configuration

- No runtime-selection variable; `container` is the only path.
- `PROPTEST_CASES` overrides per-property case count.
- `.cache/redstone-benchmarks/` (gitignored) holds fixtures; the benchmark skips with a message when empty.

## Dependencies

- Apple `container` CLI (installed separately).
- `proptest` (dev-only); the four protocol-family crates as optional default-on features (an unconditional dependency would make every family undeletable).
- The redstone benchmark needs `lodestone-server` (counters feature), `lodestone-net` and `tokio` as dev-dependencies.
- PGO needs `llvm-profdata` (ships with the pinned nightly) and touches no `Cargo.toml` or `.cargo/config.toml`; it runs through `RUSTFLAGS`.
