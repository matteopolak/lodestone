# Fuzz harness

## What it is

`lodestone-fuzz` holds the hermetic decoder properties and the tick-aligned differential harness. The differential replay is a deterministic, bounded action script that compares a caller-named block-state region after every tick and keeps the seed and script with the first divergence. An optional resumable campaign command runs finite fluid, redstone and waterlogging scenarios against a live reference server.

## How it works

### Small model-checked lanes

- `tests/legacy_status_model.rs` generates valid UTF-16BE status responses and compares parsed fields with an independent layout model (with a detector control that must reject a missing protocol field). `tests/legacy_status_raw_bytes.rs` adds fixed malformed framing edges and 256 bounded arbitrary-byte cases (4 KiB cap) that drive the production parser and require only clean `Err`/`Ok`.
- `tests/redstone_target_strength_model.rs` generates 256 hit positions on a 997-step grid, predicts the analog level with integer distance and ceiling arithmetic, and compares with the production helper; a fixed wrong-face control must disagree.
- `differential_generated_text_json` builds a bounded grammar of literal, scalar, sequence and `extra` chat components, serialises with `serde_json`, and compares `Text::from_json(...).to_plain_string()` with an independent left-to-right fold. A wrong-reader control proves a mismatch shrinks to a nonempty value. It excludes translation and styling (they need a language or rendering oracle).

### Fixed replay

`differential::FixedActionReplay` owns an opaque replay seed, ordered `ScriptStep`s, a `BlockStateRegion` and a trailing settle horizon. Construction rejects duplicate probes, empty candidate alphabets, unordered actions and work beyond the step, probe and tick limits before any oracle exists. `run` delegates to `run_differential`, which applies each tick's actions to both sides, advances both worlds once and compares every probe in deterministic order. `ReplayReport` keeps the whole case; `DifferentialOutcome::Diverged` is the first tick and position that disagreed, not an aggregate.

`WorldOracle` is the shared seam: a tiny in-memory map oracle for hermetic tests, `differential::rcon::RconOracle` (feature `rcon-oracle`) for a real server. Both return a state only from the region's candidate list.

`run_differential` calls `WorldOracle::begin_comparison` and checks both oracles before and after each complete action and observation group. Deterministic oracles keep no-op hooks. `RconOracle` requires those reads to equal its nominal game-time counter, and its comparison advance to observe exactly the next counter. It retains the first disagreement while finishing all probes and the final boundary, and a crossed boundary overrides agreement and divergence with `OracleFailureKind::Timeout`. Setup and cleanup are outside strict mode; `RconOracle::reset_baseline` re-anchors and leaves it. A zero `missed_deadlines` count alone does not prove valid timing.

### Generated scenarios

`campaign::generation::GenerationDomain` and `SearchBudget` sit above the replay: a fixed ChaCha seed produces bounded nondecreasing tick sequences, and the first disagreement is shrunk semantically (no elapsed-time cutoff). The falling-block scenario runs against an `IntegratedServer` with two independent columns; its expected world computes scheduled delay, gravity, drag and landing separately from the server. Its generator gives every step a zero tick gap, edits go through `IntegratedServer::set_resident_block_state_id`, and the tick-feed wait accepts a counter at or beyond the request, so timed support removal and later edits need exact admission acknowledgements before earning aligned coverage.

### Resumable live campaigns

The `differential-campaign` binary runs the same finite fluid, redstone and waterlogging scenarios as the eight-case ignored integration tests, sharing generation, shrinking, baseline reset, comparison and cleanup. Every evaluation reaches `run_differential`; bookkeeping contains no second tick or reset loop. The shared circuit layout is `lodestone_fuzz::redstone_contraption`. `campaign::fluid_lane` holds the shared fluid comparison, baseline probe, clock admission and cleanup; scenario modules own layouts and setup, and test targets include the helper directly so one layout does not compile the other as unused.

The waterlogging scenario is an open-top three-cell trench with a fixed dry bottom oak slab in the centre. Generated edits place air or source water only at the two ends (at most six steps, six ticks between). End probes distinguish air and all sixteen water levels, the centre probe dry and wet slabs, and baseline validation checks floor, walls, end caps and top. A twelve-tick trailing horizon bounds a candidate to 43 observed ticks.

Directed live cases cover opposing-source hydration, level-three flow beside a dry slab, and a wet slab beside level-six water; all agree with the reference under strict boundaries, as does a captured five-action source-removal replay. The level-three case compares decay and hydration in the short trench, not a persistent upstream front. The wet-slab case initialises through a tick-zero edit; generated replay keeps the centre slab fixed and rejects directed-only edits. A hermetic wrong-slab reader proves shrinking and replay detection, not reference parity. A group crossing a counter boundary earns no accepted coverage.

```bash
cargo run -p lodestone-fuzz --features differential-campaign --bin differential-campaign -- \
  --scenario fluid --output .cache/differential/fluid --seed 0x54911e \
  --cases 1000 --run-cases 8 --shrink-attempts 32 --timing-attempts 3
# add --resume for the next slice
cargo run -p lodestone-fuzz --features differential-campaign --bin differential-campaign -- \
  --scenario fluid --replay .cache/differential/fluid/replay.json
```

Start the local creative oracle with `just oracle-creative` first. The command accepts only numeric loopback endpoints with the oracle's development RCON password, reads no host authentication files, and fixes the action alphabet, coordinates, probe region and settle horizon per scenario (replay validates all before opening RCON).

`checkpoint.json` records immutable configuration, generator and format versions, the next case index, accounting and an optional minimised replay. Each completed case is written through a size-capped temporary file, synced and atomically renamed. Resume refuses changed settings or formats and reconstructs the ChaCha stream by discarding one tree at a time (bounded linear work, memory independent of case count). A failed case is regenerated after another full lane reset; a finding is saved before confirmation so interruption resumes its explicit replay; a confirmed finding stops the campaign.

Accounting: `accepted_cases` counts completed cases (including a confirmed finding); `generated_evaluations` and `shrink_evaluations` count candidates before timing retries; search and confirmation/replay account separately with `oracle_attempts`, `retry_attempts`, `oracle_failures`, `timing_failures`, `accepted_evaluations`, `accepted_ticks` and `accepted_actions`. Accepted ticks and actions count one paired comparison (not two worlds' sum), include the prefix through a first divergence, and exclude every failed attempt and all setup, drain and teardown ticks. A missed boundary is a timing failure and a retry never adds coverage. Work interrupted before a checkpoint write is rerun. The latest terminal oracle failure records phase, tick, side and kind; remote feedback and authentication material are never saved.

The output directory holds a checkpoint, at most one replay, a reusable lock file and at most one temporary file per JSON output; each JSON input or output is capped at 1 MiB. OS file locks cover output and scenario/port and release on exit. Do not run a campaign beside an ignored integration test on the same lane: test processes do not take the lane lock.

Exit `0` means the slice finished without a finding (check `status` for `ready` versus `complete`), `1` a divergence confirmed by replay, `2` invalid configuration, oracle failure or unconfirmed replay. Standalone replay returns `0` only when the recorded tick, position and state pair recur. These are bounded gameplay regression campaigns, not client-state, piston or container scenarios.

## How to change it

- New fixed case: build a `BlockStateRegion` and `FixedActionReplay`, run against fresh oracles, and keep the seed with the script (the stable identifier for reports). Any change to replay ordering, region validation or divergence reporting needs a hermetic fake-oracle detector control; agreement-only coverage cannot prove the comparison sees mismatches.
- No second replay loop for live tests: implement `WorldOracle` or use `RconOracle`. A generated case goes above the replay: give `GenerationDomain` a finite, independently justified action alphabet, set every `SearchBudget` field, keep bounds foreground-friendly, and retain a wrong-read control.
- New campaign scenario (under `src/campaign/`): a finite domain, exact probe region and settle horizon, routed through the shared replay loop, keeping existing controls. Bump `GENERATION_VERSION` when strategy ordering, domain contents, generator dependency behaviour or boundary policy changes (stale coverage must not survive a stricter timing contract). Minimised replay scripts are the durable reproducers and must pass current checks when rerun.
- `tests/differential_tick_boundaries.rs` drives a scripted RCON peer with independent counter replies through the real `RconOracle`: valid agreement and first-divergence controls consume the whole group, and a one-tick crossing before the first action, during edits or observations, or past the next awaited counter must return a typed timing failure. Keep both matching-state and divergent-state crossing controls when changing the hooks.

## Configuration

- `MAX_FIXED_REPLAY_STEPS`, `MAX_FIXED_REPLAY_PROBES`, `MAX_FIXED_REPLAY_CANDIDATES`, `MAX_FIXED_REPLAY_TICKS` bound fixed replay; `settle_ticks` is part of the replay. Enable `rcon-oracle` only for real-server runs.
- `SearchBudget` fixes seed, case count and shrink limit; `GenerationDomain` bounds steps and tick gaps (horizon capped at 4,096 ticks).
- Command: `--scenario fluid|redstone|waterlogging` with `--output DIR`, or `--replay FILE`. Default budget is 1,000 cases and, without `--run-cases`, runs entirely in the foreground (tens of minutes); `--run-cases 8` is a smoke slice. Caps: cases 1,000,000, shrink attempts 4,096, timing attempts 32; defaults 32 and 3. Seeds default to `0x54911e` (fluid), `0x5490eed` (redstone), `0x549a7e` (waterlogging), decimal or `0x`. `--endpoint` defaults to `127.0.0.1:25571` and ignores `LODESTONE_DIFFERENTIAL_RCON`. Redstone baseline settling needs twelve consecutive quiet ticks within 96 observed.

## Dependencies

The replay core depends only on the `differential` module and the caller's `WorldOracle`; `RconOracle` uses `lodestone-testsupport` for RCON framing and is not a dependency of hermetic tests. Feature `differential-campaign` enables `rcon-oracle`, `proptest`, `serde` and `serde_json` (test dependencies for ordinary targets, optional library dependencies for the command).
