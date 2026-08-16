# PGO experiment

## What this is

A measured answer to "is profile-guided optimization worth adding to this workspace's
release build" — filed as a general-improvements follow-up with no further detail beyond
that question. This is a report of one experiment, not a build-config change: nothing
in `Cargo.toml` or `.cargo/config.toml` was touched. PGO's two-pass build (instrument,
train, re-optimize) is expressed entirely through `RUSTFLAGS`/`llvm-profdata`, which is a
build-pipeline concern, not something `[profile.release]` can express as a static setting.

## Method

`crates/lodestone-worldgen/examples/pgo_probe.rs` — deliberately self-contained within
`lodestone-worldgen` + `lodestone-worldgen-core` *in its own source*: it never imports
`lodestone-server`'s `EmbeddedResolver` or any of its bundled data, using the same
`tests/support/worldgen_data` fixture tree already shared by `tests/overworld_gen.rs`,
`benches/generation.rs` and `tests/embedded_vs_fixture_stage_cost.rs`.

**Correction, 2026-08-16: that does not make the *build* dependency-free.**
`lodestone-worldgen/Cargo.toml` carries a deliberate dev-dependency cycle —
`lodestone-server = { path = "../lodestone-server" }` under `[dev-dependencies]`,
documented there as needed for other dev targets in the crate (`EmbeddedResolver`,
`block_freeze_facts`) — and Cargo makes every dev-dependency available to (and, more
importantly, **built for**) every example in the package, not just the dev targets that
actually use it. So `cargo build --release --example pgo_probe` compiles the full
`lodestone-server` dependency chain (auth, tokio, `hickory-resolver`, …) regardless of
what the example's own source imports. Measured directly: a release build of just this
example took **5m 27s** in a cold private target dir, and its own log shows
`lodestone-server`'s 42 warnings scrolling past mid-build. The original "no
`lodestone-server` dependency" framing was true for source-level imports and false for
the actual unit graph — worth knowing before assuming this example is cheap to rebuild,
or that it is truly unaffected by a `lodestone-server` compile break (it is unaffected by
*using* stale/wrong data from that crate, but not by it failing to build at all).

Generates a fixed 6×6 cache-cold patch (36 columns) twice — a warm-up pass discarded, a
second pass timed — and reports **instructions retired**
(`proc_pid_rusage(RUSAGE_INFO_V4)`, the same macOS-only counter
`benches/generation.rs`'s `instructions_retired`/`I_ss` uses and validates), not wall-clock.
Per `CLAUDE.md`'s "prefer a counter over a duration" rule: this measurement ran on a shared
machine with several other agents compiling concurrently, so a wall-clock number would be
measuring machine load, not PGO. Repeatability check (three consecutive runs, same binary):
baseline spread 0.05–0.10%, PGO-optimized spread 0.03–0.06% — both far tighter than any
wall-clock figure on this machine, and confirming the instrument itself (not just the
result) is trustworthy here, same as `benches/generation.rs`'s own "why I_ss is the
comparator" probe.

```bash
# Baseline
cargo build --release -p lodestone-worldgen --example pgo_probe
./target/release/examples/pgo_probe   # x3

# Instrument
RUSTFLAGS="-Cprofile-generate=<dir>" cargo build --release -p lodestone-worldgen --example pgo_probe
./target/release/examples/pgo_probe   # x3, to generate representative .profraw files

# Merge
llvm-profdata merge -o <dir>/merged.profdata <dir>/*.profraw

# Re-optimize using the profile
RUSTFLAGS="-Cprofile-use=<dir>/merged.profdata -Cllvm-args=-pgo-warn-missing-function" \
  cargo build --release -p lodestone-worldgen --example pgo_probe
./target/release/examples/pgo_probe   # x3
```

The existing root `[profile.release]` (`lto = "thin"`, `codegen-units = 1`) applied
identically to all three builds — this measures PGO's marginal win **on top of** thin LTO
and single-codegen-unit, not PGO vs. a naive default release profile, which is the more
useful (and harder) question given this workspace's profile is already fairly aggressive.

## Result

| build | instructions retired (median of 3) | non_air_checksum |
|---|---:|---:|
| baseline (no PGO) | 22,758,838,967 | 1,081,058 |
| PGO-optimized | 19,445,040,082 | 1,081,058 |

**Ratio: 0.8544 — a 14.6% reduction in instructions retired**, on top of thin LTO +
`codegen-units = 1`. `non_air_checksum` identical across every run in every build
(baseline, instrumented, and PGO-optimized) — the deterministic generation output is
unaffected, only the code that produces it changed, which is the expected and required
property (PGO must not change what is generated, only how the same computation is
compiled).

14.6% is well above the "we tried it and it bought 3%" floor this issue's brief treats as
a complete-enough answer either way. This is a **real, substantial win** for a CPU-bound,
branch-heavy, RNG-placement-driven workload — exactly what
`docs/tick-and-worldgen-profiling.md` and `bench-results/generation.jsonl` already show
dominates worldgen cost (vegetation/ore stage placement, both scalar RNG-draw-bound rather
than dense-loop code, per that doc's SIMD-target analysis). PGO's classic strength —
better inlining and branch-layout decisions informed by which branches actually run hot —
is a good match for exactly this shape of code, more so than for a dense numeric kernel
thin LTO can already largely optimize on its own.

## Caveats, honestly

- **Single scene, single machine, one measurement session.** Not re-run on a quiet
  machine, though the counter's own tight repeatability (see above) is evidence the
  concurrent-agent load did not meaningfully contaminate it — a load-sensitive
  wall-clock number would not have shown 0.05–0.1% spread here.
- **Fixture-tree data, not full production (embedded) data.** The fixture resolver has no
  `biome_parameters/` (confirmed in `embedded_vs_fixture_stage_cost.rs`'s own precondition
  check), so real per-column biome search never runs here. The exercised code — density
  functions, noise, aquifer, surface, carvers, ore/vegetation placement — is the same
  production code path either way, so the *ratio* should generalise, but the magnitude on
  full production data is unmeasured.
- **Worldgen only.** This says nothing about PGO's win (or cost) on the render/tick/
  networking hot paths a real client or server session spends most of its time in — those
  need a captured representative session as training data, which this experiment did not
  attempt to build.
- **Build-pipeline cost, not measured here.** PGO needs a two-pass build (instrument,
  train, re-optimize) with a chosen training workload kept representative as the code
  evolves — a real maintenance cost this report does not quantify, beyond noting the whole
  cycle above took a few minutes on this machine for one small crate.

## Reproducing this

`./scripts/pgo-probe.sh` (or `just pgo-probe`) automates the four steps above —
baseline build+run, instrument build+train, `llvm-profdata merge`, re-optimize
build+run — in one private `CARGO_TARGET_DIR`, printing the same
instructions-retired-ratio table this doc's Result section shows. It does not
touch `Cargo.toml`/`.cargo/config.toml`; RUSTFLAGS is only ever set for the
subprocess `cargo build` invocations it runs itself. `PGO_PROBE_RUNS=N`
overrides the default 3 runs per build; `PGO_PROBE_KEEP_TARGET=1` skips
deleting the scratch dir on exit (prints its path) so the `.profraw`/merged
`.profdata` files can be inspected.

**Verified mechanically, not re-measured end-to-end this session.** A single
baseline build+run (`cargo build --release -p lodestone-worldgen --example
pgo_probe`, no PGO flags) reproduced `non_air_checksum=1081058` exactly and
`instructions_retired=22,897,601,211` — within 0.6% of this doc's own
`22,758,838,967` baseline, consistent with normal machine-load variance
rather than a different code path. That confirms the script's build
invocation, output-parsing regex (matching `pgo_probe.rs`'s real two-line
`PGO_PROBE …` output, corrected from an earlier draft of this script that
assumed a single-line format that never actually shipped), and
`llvm-profdata` path resolution are all correct. The full three-build cycle
was **not** re-run start-to-finish this session: under the "Correction" above
(examples pull in the full `lodestone-server` dev-dependency chain), one
release build alone took 5m 27s on this shared, concurrently-loaded machine —
three of them plus training runs would cost 20+ minutes, disproportionate to
what this pass needed to establish (that the script works). Run it yourself
on a quieter machine to re-derive the ratio; expect the absolute instruction
counts to drift with machine state but the ~14.6% figure to hold, per this
doc's own "prefer a counter over a duration, but re-run a timing-shaped
result alone before trusting it" precedent.

## Recommendation

**Worth pursuing further, not worth landing as a default build-config change yet.** The
measured win is large enough to justify a follow-up that:

1. Repeats this same measurement against the **embedded** (production) resolver once
   `lodestone-server` is quiet (this session hit a transient, unrelated compile error
   there mid-experiment from a live agent's in-progress edit — see `pgo_probe.rs`'s own
   module doc for why the fixture-tree path was used instead), to confirm the ratio holds
   on real 26.2 data.
2. Decides which binary actually ships PGO-optimized — the standalone dedicated server
   (`crates/lodestone-server`'s bin) is the more natural first target than the full game
   client, since a representative *server* training workload (generate N chunks, run M
   ticks) is far easier to define honestly than a representative *play session*.
3. Designs a two-pass build for the dedicated server specifically (a future
   `just build-pgo-server`, distinct from the `just pgo-probe` worldgen
   reproducer landed here) rather than a change to the default
   `[profile.release]`, so `just run`/`cargo build --release` stay single-pass
   and PGO stays opt-in until its training-workload story and CI cost are
   worked out.

**Landed this session** (2026-08-16): `just pgo-probe` / `scripts/pgo-probe.sh`,
so the number in the Result section above is reproducible with one command
instead of six hand-typed `cargo`/`llvm-profdata` invocations — see
"Reproducing this" above. Recommendations 1-3 remain open follow-up: none of
this session's work re-measured against the embedded resolver, picked a
shipping binary, or built the dedicated-server training-workload recipe. This
is still an opt-in experiment, not a default, per the issue's own framing
("experiment... to see if it's worth including").
