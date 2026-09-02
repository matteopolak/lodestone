# Oracles and benchmarks: runtimes, fuzzing, and real-workload measurement

## What it is

Where this repo gets ground truth and cost measurements from outside its own code: the Apple
`container` runtime every JVM/vanilla-server oracle now runs under, the property-based fuzz
harness that checks wire decoders for crash-freedom rather than round-trip agreement, the
redstone benchmark harness that measures the real tick loop against downloaded contraptions, and
the profile-guided-optimization experiment that measured whether PGO is worth adding to the
release build.

## How it works

### Oracle runtimes: Apple `container`, not Docker

Every JVM-oracle script under `scripts/live-oracles/` and `scripts/worldgen-oracle/`, plus the
Rust tests that used to shell out to `docker` directly, run their real vanilla server or JVM
oracle under Apple's `container` CLI instead. **There is no runtime-selection switch and no
Docker fallback** — `container` is simply what every one of these paths invokes now. The reason
is resource cost on a machine shared by many concurrent agent builds: `container` boots faster
(~24s vs ~40s to ready) and, more importantly, releases its VM reservation on `stop` rather than
holding it (measured roughly 1.1–1.3 GB resident while one oracle runs versus roughly 3 GB for
Docker Desktop, and ~50 MB residual after stopping versus over 2.5 GB retained by Docker's VM).

Three traps every ported script had to account for, each measured directly against these images
on this machine:

- **Never publish a port with a host-IP prefix** (`-p 127.0.0.1:PORT:PORT`) — it accepts the TCP
  connection and then resets on the first byte, every time. The bare `-p PORT:PORT` form works
  and listens on all interfaces, which is parity with how these scripts always published ports
  under Docker, not a new exposure. Treat this specific port-relay path as a fragility hotspot
  and re-verify it after any `container` upgrade.
- **An explicit image pull must pass `--platform linux/arm64`**, or the default fetches the
  entire multi-arch manifest — many times the size of the single-arch layer actually needed.
  None of the existing scripts pull explicitly (an on-demand pull already defaults to the host's
  own architecture), but this fires the moment anyone "helpfully" pre-pulls an image by hand.
- **`--memory 3g` is required on every script that runs a JVM.** The per-VM default is 1 GiB,
  and every oracle here runs with a larger heap than that, with no override.

`container logs` has no `--since` flag, only `-n <lines>` — a script that used to poll a
wall-clock log window now polls a fixed line count instead, sized generously enough to outlast
one round trip of its own polling interval.

### The fuzz/property-testing harness (`lodestone-fuzz`)

Property-based fuzzing (via `proptest`, a plain dev-dependency — not `cargo-fuzz`/libFuzzer,
which would need a separate corpus and job even though this workspace's toolchain is already
nightly-pinned) for every wire decoder, checking properties that need no expected value at all:
a decoder must never panic on arbitrary bytes, a truncated prefix of a valid packet must error
cleanly, and a length prefix must never force an allocation disconnected from the bytes actually
available. **This complements, not replaces, `decode(encode(x)) == x` round trips** — a round
trip can only ever be as strict as a shared misunderstanding between its two halves, and this
repo has shipped defects invisible to exactly that blind spot before. Every property test calls
a decoder through a `catch_unwind` wrapper (never directly, or a panic aborts the whole test
process instead of reporting one shrunk failing case) and — for the four client protocol
families' clientbound decode, and `v26-2`'s serverbound decode (the only family that hosts) —
reads the real generated packet-id tables, so "which packet ids get fuzzed" tracks the code
generator rather than a hand-maintained list. The frame-level `Codec` (length prefix and zlib
compression, ahead of every family's own decode) is fuzzed too, with a termination bound so a
codec that spins instead of erroring fails a cap rather than hanging the suite.

Two real bugs were found and fixed this way. First: a generated `decode_vec` path
pre-allocated a `Vec` at a wire-supplied length **before** checking that length against bytes
actually remaining, so a crafted 8-byte packet drove a 48-million-byte allocation before the
first per-element read failed — fixed by capping the pre-allocation at
`len.min(reader.remaining())`, a change that closed the hole for every one of the thirteen
affected fields across all four protocol families in one macro edit, with no per-field opt-in
required. Second: an unchecked `i32` multiply on a wire-supplied chunk coordinate in
`multi_block_change` decode could overflow — panicking in debug and silently wrapping to a wrong
block position in release — fixed with a `checked_mul` plus an explicit refusal past vanilla's
own world-border bound, deliberately not clamped, since a clamp invents a position exactly the
way the release-mode wrap did. Both fixes are pinned by a committed byte-fixture regression test
in addition to the fuzzer's own seed file, because a generated repro alone is not a durable gate.
A fixed, deliberately-buggy decoder lives in the harness's own control test, asserting the panic
wrapper actually reports a panic (and does not falsely report one for an in-bounds call) — an
absence assertion is only as good as a control proving the detector fires.

### The redstone benchmark harness

A schematic loader (`lodestone_anvil::schematic`, reading Litematica/Sponge/vanilla-structure
containers) plus a benchmark (`crates/lodestone-anvil/tests/redstone_benchmark.rs`, `#[ignore]`d
like every other live/slow gate) that loads a real, publicly-downloaded redstone contraption
into the actual production tick loop and reports where its per-tick cost goes — built to settle
whether an incrementally-invalidated redstone dependency graph is worth building, by measuring
real neighbour-scan cost rather than arguing from a synthetic circuit. It reports two things:
whole-loop `TickStats` (context only — a wall-clock duration on a shared, contended machine, not
trustworthy as an absolute number) and `redstone_counters`' process-global, load-independent
counts (notifications issued, cell reads, signal queries, wire recomputes), reported per elapsed
tick so the rate stays meaningful even when the loop falls behind under load.

Loading a contraption via a raw block-source write does not reproduce a live neighbour-update
cascade, so a freshly-loaded circuit starts at its captured **steady state** with nothing
scheduled to perturb it — measured directly, every real downloaded fixture showed exactly zero
neighbour-scan cost at rest, which is a genuine floor-case result but not the number the original
question needed. Re-injecting a schematic's own pending block ticks (a repeater mid-cycle, a fire
spread tick) through the same production scheduled-tick path a live server uses closes that gap:
on one real farm, resuming just two already-scheduled repeater ticks cascaded into hundreds of
notifications and thousands of raw block-state reads in a single tick — the number the original
question actually needed, and evidence that neighbour-scan cost is real and concentrated exactly
where a dependency graph would replace it with direct activation. Every fixture used here is
tracked with its source URL and a licence-clarity note (none carries an explicit license; all are
public repositories whose own text describes the files as shared for reuse) and is gitignored
rather than committed — used only for internal, non-redistributed benchmarking.

### The PGO experiment

A single measured answer to "is profile-guided optimization worth adding to the release build":
**not yet worth landing as a default, but worth pursuing further.** A self-contained worldgen
probe (instructions-retired, not wall-clock — the same macOS-only counter this project prefers
whenever a measurement runs on a machine shared with other agents' concurrent builds, because a
counter's run-to-run spread was measured tighter than any wall-clock figure taken the same way)
showed a **14.6% reduction in instructions retired** for a PGO-optimized build over the same thin-LTO,
single-codegen-unit baseline this workspace already ships, with the deterministic output
checksum unchanged across every build — proof PGO changed only how the computation compiles, not
what it computes. The caveats that keep this an experiment rather than a landed feature: it is a
single scene on a single machine; it exercises a fixture data tree rather than full production
(embedded) resolver data, so the ratio should generalize but the magnitude is unmeasured on real
data; it covers only worldgen, saying nothing about render/tick/networking hot paths; and PGO's
own two-pass build (instrument, train with a representative workload, re-optimize) is a real,
unquantified maintenance cost that a static build-config change cannot express — it has to be a
separate, explicit build pipeline (`just pgo-instrument`/`pgo-merge`/`run-pgo`/`build-pgo`), not
a change to the default release profile.

## How to change it, and the gotchas

- **Adding a seventh oracle script**: copy `creative.sh`'s pattern — idempotent `container system
  start`, force-remove any existing container by name, bare `-p` port publishing, `--memory 3g`,
  and poll readiness by grepping the container's own log for its "ready" line.
- **Adding a family or state to the fuzz sweep**: extend the one `Family`/`STATES` table the
  deterministic sweep and the randomized property both read from — a new family needs one enum
  variant and one match arm each in the adapter constructor and the entry-table accessor.
- **When a fuzz target finds a bug, commit the input.** Drop the exact bytes into a committed hex
  fixture and assert against them directly, in addition to keeping the fuzzer's own seed file —
  they fail for independent reasons and neither replaces the other. Assert what the decoder
  *does* with the bad input, not merely that it survives: a release-mode silent wrap or an
  invented clamped position both satisfy "did not panic" while being real, separate defects.
- **A process-wide allocation counter measures every allocation in the whole test binary**,
  including unrelated concurrently-running tests in the same file — scope the counter with a
  `thread_local!`, never a shared `Mutex` (a lock only excludes code that takes it; an unrelated
  test that never calls the measuring helper still contaminates a shared global).
- **Adding a redstone-benchmark fixture**: record its source URL, author, and a licence-clarity
  note in the same commit as adding it — an artefact with no recorded provenance is not something
  this harness should be pointed at again.
- **A duration measured on this shared dev machine is not a result** — re-run any wall-clock
  figure alone, on a quiet machine, before trusting it; every counter-based measurement in this
  cluster exists specifically to route around that hazard.

## Configuration

- No oracle-runtime-selection variable exists; `container` is the only path.
- `PROPTEST_CASES` overrides the fuzz harness's per-property case count without editing source.
- `.cache/redstone-benchmarks/` (gitignored) holds fetched schematic fixtures; the benchmark
  test prints a skip message rather than failing when it is empty.
- `just pgo-instrument` / `pgo-merge` / `run-pgo` / `build-pgo` / `pgo-probe` drive the PGO
  experiment's build pipeline; none of it is wired into the default release profile.

## Dependencies

- Apple `container` CLI (installed separately, not a build dependency) for every live oracle.
- `proptest` (dev-only) for the fuzz harness; all four protocol-family crates as optional,
  default-on features (an unconditional dependency there would make every family undeletable).
- `lodestone-anvil`'s schematic loader depends on nothing beyond what that crate already ships;
  the redstone benchmark harness itself additionally needs `lodestone-server` (with its
  counters feature enabled), `lodestone-net`, and `tokio` as dev-dependencies.
- The PGO experiment needs `llvm-profdata` (ships with the pinned nightly toolchain) and touches
  no `Cargo.toml`/`.cargo/config.toml` — the whole two-pass build is expressed through `RUSTFLAGS`.
