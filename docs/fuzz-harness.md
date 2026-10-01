# Fuzz harness

## What it is

`lodestone-fuzz` holds the hermetic decoder properties and the Track B
tick-aligned differential harness. Track B's fixed replay is a deterministic,
bounded action script that compares a caller-named block-state region after
every tick and retains the seed and script alongside the first divergence.

The legacy status boundary has two deliberately separate lanes. `tests/legacy_status_model.rs`
generates valid UTF-16BE response packets and compares their parsed fields with
an independent layout model, including a detector control that must reject a
missing protocol field. `tests/legacy_status_raw_bytes.rs` complements it with
fixed malformed framing edges and 256 bounded arbitrary-byte cases. It drives
the production parser from the raw packet boundary and requires only clean
`Err`/`Ok` results, so a panic in the length, UTF-16, or field-validation path
cannot hide behind the valid-input model. The 4 KiB input cap keeps this
robustness lane cheap enough for the ordinary workspace test run.

`tests/redstone_target_strength_model.rs` covers another small but high-value
reaction boundary. It generates 256 hit positions on a 997-step cell grid,
predicts the analog level with integer distance and ceiling arithmetic, and
compares that prediction with the production target-strength helper. Its fixed
wrong-face control must disagree, so the campaign cannot pass if the helper
ignores the hit axis or if the assertion is accidentally removed.

## How it works

`differential::FixedActionReplay` owns an opaque replay seed, ordered
`ScriptStep`s, a `BlockStateRegion`, and a trailing settle horizon. Construction
rejects duplicate probes, empty candidate alphabets, unordered actions, and
work beyond the documented step/probe/tick limits before it creates an oracle.
`FixedActionReplay::run` delegates to `run_differential`, which applies each
tick's actions to both sides, advances both worlds once, then compares every
probe in deterministic order. `ReplayReport` keeps the complete replay case;
its `DifferentialOutcome::Diverged` value is the first tick and position that
disagreed, not an end-of-run aggregate.

`WorldOracle` is the common seam. Hermetic tests use a tiny in-memory map
oracle, while a real-server run can use `differential::rcon::RconOracle` when
the `rcon-oracle` feature is enabled. Both return a state only from the
region's caller-supplied candidate list, so the constrained server probe and
the in-process reader compare the same alphabet.

`run_differential` calls `WorldOracle::begin_comparison`, then checks both
oracles before and after each complete action group and observation group.
Deterministically stepped oracles keep the default no-op hooks. `RconOracle`
requires those reads to equal its current nominal game-time counter, and its
comparison advancement must observe exactly the next counter. It retains the
first disagreement while completing all probes and checking the final
boundary; a crossed boundary overrides both agreement and divergence with
`OracleFailureKind::Timeout`. Setup and cleanup remain outside strict mode;
`RconOracle::reset_baseline` re-anchors the counter and leaves that mode.
A zero `missed_deadlines` count alone does not prove valid timing.

Generated scenarios use the shared `campaign::generation::GenerationDomain` and `SearchBudget`
above that replay layer. A fixed ChaCha seed produces bounded, nondecreasing
tick sequences; the first disagreement is semantically shrunk without an
elapsed-time cutoff. The falling-block scenario uses this against an
`IntegratedServer` with two independent columns. Its expected world computes
the scheduled delay, gravity, drag, and landing arithmetic separately from the
server implementation, so it exercises production tick scheduling and falling
entities rather than an encode/decode round trip.

### Resumable live campaigns

The optional `differential-campaign` binary runs the same finite fluid and
redstone scenarios as the eight-case ignored integration tests. The tests and
command share the generation, shrinking, baseline reset, comparison and cleanup
implementations. Every evaluation still reaches `run_differential`; campaign
bookkeeping does not contain another tick or world-reset loop.
The shared circuit layout is exposed as `lodestone_fuzz::redstone_contraption`
and consumed directly by both production-model and live comparison tests.
With the campaign feature enabled, test support re-exports the library's
generated-search API instead of compiling a second copy.

```bash
cargo run -p lodestone-fuzz --features differential-campaign --bin differential-campaign -- \
  --scenario fluid --output .cache/differential/fluid --seed 0x54911e \
  --cases 1000 --run-cases 8 --shrink-attempts 32 --timing-attempts 3
cargo run -p lodestone-fuzz --features differential-campaign --bin differential-campaign -- \
  --scenario fluid --output .cache/differential/fluid --seed 0x54911e \
  --cases 1000 --run-cases 8 --shrink-attempts 32 --timing-attempts 3 --resume
cargo run -p lodestone-fuzz --features differential-campaign --bin differential-campaign -- \
  --scenario fluid --replay .cache/differential/fluid/replay.json
```

Start the local creative oracle with `just oracle-creative` before a live run.
The command accepts only numeric loopback endpoints and uses the local oracle's
development RCON password. It never reads host authentication files. The
generated action alphabet, coordinates, probe region and settle horizon are
fixed by the scenario. Replay validates all of those before opening RCON.

`checkpoint.json` records immutable configuration, generator/format versions,
the next case index, accounting and an optional minimized replay. Each completed
case boundary is written through a size-capped temporary file, synced and
atomically renamed. Resume refuses changed settings or incompatible formats.
It reconstructs the ChaCha stream one discarded tree at a time before the saved
index; this takes bounded linear work and keeps memory independent of the
campaign's case count. A failed case keeps its index and is regenerated after
another complete lane reset. A finding is saved before confirmation, so an
interruption during confirmation resumes its explicit replay. A confirmed
finding stops the campaign.

`accepted_cases` counts completed generated cases, including a confirmed
finding. `generated_evaluations` and `shrink_evaluations` count candidate
evaluations before timing retries. Search and confirmation/replay have separate
accounting: `oracle_attempts`, `retry_attempts`, `oracle_failures`,
`timing_failures`, `accepted_evaluations`, `accepted_ticks` and
`accepted_actions`. Accepted ticks and actions count one paired comparison,
not the sum of its two worlds. They include the actual prefix through a first
divergence, and exclude every failed attempt, even when that attempt performed
partial work. Setup, drain and teardown ticks are outside the candidate
comparison and do not contribute. A missed tick boundary is a timing failure;
retrying it never increases accepted coverage. The last synced checkpoint
covers completed boundaries only; work interrupted before a checkpoint write
is rerun and is absent from its saved counters.
The latest terminal oracle failure records its generation/shrink/replay phase,
tick, side and kind. Remote feedback and authentication material are not saved
in the checkpoint.

The output directory holds a checkpoint, one replay when found, a reusable lock
file and at most one temporary file per JSON output. Each JSON input or output
is capped at 1 MiB. Output and scenario/port locks use OS file locking and are
released on process exit; lock files remain for reuse. Do not run the campaign
alongside an ignored integration test using the same scenario lane: test
processes do not acquire the command's lane lock.

Exit status `0` means the requested slice completed without a finding (inspect
`status` to distinguish `ready` from `complete`), `1` means a divergence
was confirmed by replay, and `2` means invalid configuration, an oracle failure
or an unconfirmed replay. Standalone replay returns `0` only when the recorded
tick, position and state pair recur. These are bounded gameplay regression
campaigns, not broader client-state or piston/container live scenarios.

`differential_generated_text_json` is a separate parser lane. It builds a
bounded grammar for literal, scalar, sequence, and `extra` chat components,
serializes each value with `serde_json`, and compares production
`Text::from_json(...).to_plain_string()` to an independent left-to-right plain
text fold. The fixed seed and bounded case count make its corpus repeatable;
the test's wrong-reader control verifies that a mismatch produces a shrunk,
nonempty grammar value. It deliberately excludes translation and styling:
those require a language-table or rendering oracle, while this lane isolates
the JSON tree shape, scalar conversion, ordering, and escaping boundary.

The falling-block generator deliberately gives every generated step a zero
tick gap. Its source is a setup fixture, while the server retains a chunk
column after its first tick; extending this lane with later edits requires a
public world-mutation path rather than writing the fixture source directly.

## How to change it

Add a new fixed case by constructing `BlockStateRegion` and
`FixedActionReplay`, then run it against fresh oracle instances. Keep the seed
with the script even though this slice does not generate from it yet: it is the
stable identifier a later generator or a bug report can reuse. Add a hermetic
fake-oracle detector control for any change to replay ordering, region
validation, or divergence reporting; agreement-only coverage cannot prove the
comparison is capable of observing a mismatch.

Do not create a second replay loop for live tests. Implement `WorldOracle` or
use `RconOracle`, so real and hermetic cases share the tick ordering and
first-divergence semantics. Put a generated case above this fixed replay layer:
give `GenerationDomain` a finite, independently justified action alphabet and
set every `SearchBudget` field explicitly. Keep a generated test's case and
tick bounds small enough for an ordinary foreground test run, and retain a
wrong-read detector control for its comparison path.

Campaign implementation lives under `src/campaign/`. Add a scenario by
providing a finite domain, exact probe region and settle horizon, and route its
evaluator through the shared replay loop. Keep its existing detector controls.
Bump `GENERATION_VERSION` when strategy ordering, domain contents or generator
dependency behavior changes, or when comparison boundary policy changes;
previously accepted checkpoint coverage must not survive a stricter timing
contract. Explicit minimized replay scripts remain the durable reproducers
and must pass the current boundary checks when run again.

`tests/differential_tick_boundaries.rs` uses a scripted RCON peer with
independent counter replies to exercise the actual `RconOracle`. Its valid
agreement and first-divergence controls consume the whole probe group. A
one-tick crossing before the first action, during edits or observations, or
past the next awaited counter must instead return a typed timing failure.
Keep both matching-state and divergent-state crossing controls when changing
the hooks, because rejecting only one verdict leaves the other vulnerable.

## Configuration

`MAX_FIXED_REPLAY_STEPS`, `MAX_FIXED_REPLAY_PROBES`,
`MAX_FIXED_REPLAY_CANDIDATES`, and `MAX_FIXED_REPLAY_TICKS` bound fixed replay
work. `settle_ticks` is part of the replay and allows delayed world reactions
to be compared after the last action. Enable Cargo feature `rcon-oracle` only
for a real-server `RconOracle` run; hermetic replay tests require no container
or network service.

`SearchBudget` fixes a generated test's seed, case count, and shrink-attempt
limit. `GenerationDomain` bounds step count and tick gaps before any oracle is
created; its generated and replayed horizons are capped at 4,096 ticks.

The command requires `--scenario fluid|redstone` and `--output DIR`, or
`--replay FILE` for standalone replay. Default case budget is 1,000; without
`--run-cases`, that entire budget runs in the foreground and can take tens of
minutes. Use `--run-cases 8` for a smoke-sized slice and `--resume` for another
slice. Cases are capped at 1,000,000, shrink attempts at 4,096 and timing
attempts at 32. Default shrink and timing budgets are 32 and 3. Seeds default
to `0x54911e` for fluid and `0x5490eed` for redstone; decimal and `0x` input
are accepted. `--endpoint` defaults to `127.0.0.1:25571` and does not read
`LODESTONE_DIFFERENTIAL_RCON`; all campaign configuration is explicit.
Redstone baseline settling fails if it cannot observe twelve consecutive
quiet ticks within 96 observed ticks.

## Dependencies

The core replay abstraction depends only on `lodestone-fuzz`'s
`differential` module and the caller's `WorldOracle` implementations. The
optional real-server path uses `lodestone-testsupport` for RCON framing; it is
not a dependency of the hermetic fake-oracle tests.

The `differential-campaign` feature enables `rcon-oracle`, `proptest`,
`serde` and `serde_json`. Those generation/serialization libraries remain
test dependencies for ordinary test targets; they are optional library
dependencies for the command.
