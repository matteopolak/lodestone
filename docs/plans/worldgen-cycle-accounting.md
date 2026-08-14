# Worldgen cycle accounting

## What it is

The measurement programme that follows the allocation drive: instruction-denominated,
per-stage cost accounting for the worldgen pipeline, built on the task-level hardware
counters macOS exposes through `proc_pid_rusage`, and an A/B protocol that makes a
before/after performance claim defensible on this machine **without keeping two live
production code paths**. Owner-directed (2026-08-07); written from a read-only
architecture review at `6803146d`+. Planning artifact only — each build item below is
separately landable with its own evidence standard.

**Status (2026-08-08): the whole-column half is built.** `benches/generation.rs` now
reads `ri_instructions` per column *inside* the C_ss sweep and reports
`i_ss_median_instructions_per_column` (baseline **488,507,564**), with the struct-size
and 4×-scaling controls, and `benches/support.rs` refuses to record an instruction
count from a `gen-counters` build. **Per-stage** instruction accounting (the §"Per-stage
accounting" item below) is *not* built — the stage boundaries would each need a ~80,000
instruction read, which is the measured cost of a `proc_pid_rusage` call and is larger
than several of the ten stages. That constraint was not known when this plan was
written and it changes the per-stage design: see DESIGN.md §12.130.

## The problem this solves

Nine consecutive units of `docs/plans/worldgen-rewrite.md` declined to claim a
wall-clock speedup, all for the same structural reason, and the record says they were
right to decline:

- one stage swung **22%** across three runs of an identical binary while an allocation
  counter read 905,459 to the digit, 3 of 3 (DESIGN.md §12.98);
- full-burst totals **changed sign with arm order** — the second arm inherits the
  first's thermal and allocator state (§12.103);
- self time for **unchanged** code moved ×1.77–×2.25 between two `samply` captures
  (§12.104);
- a single un-interleaved C_ss reading of 78.2 ms against the 97.8 ms median was
  recorded and explicitly not claimed (§12.100).

The standing two-arm rule fixes this only when both arms run interleaved in one
process — but the two arms of a rewrite are **two builds of one symbol** (§12.102), and
the migration rule (worldgen-rewrite Q4.3) deletes the old path in the cutover commit
because two live paths is the two-worlds hazard. So no unit could interleave, and no
unit could claim. The recorded C_ss is 97.8 ms against a 1.0 ms goal and we do not know
how much of the structural work has reached the clock.

The resolution is not a cleverer wall-clock protocol. It is a metric whose noise floor
is below the effects we care about, plus a protocol whose arms are **build artifacts,
not source paths**.

## The instrument, measured before proposed

`proc_pid_rusage(getpid(), RUSAGE_INFO_V4, …)` returns `ri_instructions` and
`ri_cycles` for the calling process — populated on Apple Silicon (this repo's reference
hardware), **unprivileged**, zero on Intel. Measured on this machine, 2026-08-07,
*under concurrent-agent load* (the realistic condition), with a fixed 8M-iteration
SplitMix64 + 8 MiB pointer-walk kernel:

| metric | peak-to-peak spread, 7 in-process runs | cross-process |
|---|---|---|
| instructions retired | **0.58%** (104.18M–104.79M; first 5 runs ~0.07%) | **~0.12%** on matched rows |
| cycles | 5.4% | moved ~4% as machine load shifted |
| wall time | 10.8% (7,454–8,262 µs) | — |

One `proc_pid_rusage` call costs **~560–660 ns** (measured, 10k-call loop).

Why instructions hold when time does not: thermal state, DVFS, and P-vs-E-core
placement change how *fast* instructions retire, never *which* instructions a
deterministic program executes. Those are exactly the confounds §12.103 measured. The
residual ~0.1–0.6% is interrupt/allocator attribution noise, and it is 20–100× below
the wall-clock floor under the same load.

Two vacuity guards the instrument itself needs, stated now so they are not
rediscovered:

- **Assert `ri_instructions > 0` at startup.** On Intel or under Rosetta the fields
  read zero, and a zero-delta comparison would pass every equality check while
  measuring nothing. macOS-only is acceptable — the reference hardware is named in the
  C_ss definition — but the helper must fail loudly elsewhere, never return zeros.
- **Every recorded run re-measures its own noise floor.** The probe kernel above ships
  in the bench as a `#[inline(never)]` reference workload, measured N=5 at run start;
  the run refuses to record if its spread exceeds 1%. This is the detector-control
  discipline (CLAUDE.md, "assertions of an absence need a control") applied to the
  instrument itself — and it doubles as the cross-build drift control below.

## What instructions retired do not answer

Instructions are the **comparator**, not the goal. Stated limits, each with its
covering metric:

- **Not time.** A change that trades instructions for locality — precisely the live
  `RegionView::overlay` dense-array question, where §12.106 counted ~45% probe hits —
  can *raise* instruction count while *lowering* time, or vice versa. The covering
  metric is `ri_cycles`: stall-inclusive, ~5% floor, so it resolves ≥10–15% effects
  with paired medians. **IPC (instructions ÷ cycles), per stage, is the
  memory-boundedness diagnostic**: a stage whose IPC drops after a change got slower
  per instruction, whatever the instruction delta says.
- **Not a substitute for the wall clock at milestones.** C_ss ≤ 1.0 ms remains a
  wall-clock goal. Milestone wall-clock measurements keep the *unchanged* two-arm
  discipline — alternated artifact arms (below), quiet machine verified by
  `vm.swapusage`/`Swapouts`, spread reported, labelled as such. The instrument's job is
  to make those rare: unit-level acceptance moves to instructions, and the clock is
  consulted when a milestone worth its cost arrives.
- **Process-wide, not per-thread.** Exact for the single-threaded C_ss/C_cold benches.
  For the 289-column join burst it measures *total work* only — which is still useful
  (total instructions for a fixed burst is load-balancing-independent), but per-thread
  attribution stays with `samply` (categorical readings only, per §12.104) and the
  app-level counters.
- **No cache-miss counts.** Per-region PMU access (`kperf`) needs root/entitlements on
  macOS and is not a standing-harness dependency. When a specific question needs miss
  counts, escalate to Instruments/`kperf` as a one-off investigation and record the
  result; do not build the harness on it.

## The A/B protocol: arms are artifacts, not source paths

**Ruling this document exists to make: for instruction-denominated claims, sequential
arms across two builds are acceptable, and the interleaving constraint is retired for
this metric class.** The constraint existed because wall time drifts between arms;
instruction counts measured 0.1–0.6% reproducible *across processes, under load, while
cycles moved 4%* — the drift the interleave rule guards against does not reach this
metric. The two-arm interleaving rule **stands unchanged for durations**.

The protocol, scripted once (`xtask bench-ab` or `scripts/ab-bench.sh`) so the tenth
agent does not re-derive it:

1. **Arm A** is built from the pre-change sha in a throwaway detached worktree
   (`git worktree add --detach`), with a **private `--target-dir`** (never the shared
   `target/` — CLAUDE.md's poisoned-build-script hazard), named per-unit and deleted by
   exact name when done (the disk filled to 100% once already; worldgen-rewrite's disk
   note). The *binary* is kept; no second source path ever exists in the main tree.
   The migration rule is untouched.
2. **Arm B** is the working tree's bench binary.
3. Both arms: release profile, `gen-counters` **off** (the feature's atomics execute
   real instructions — a counters-on instruction reading is poisoned exactly as a
   counters-on timing is), same toolchain. **Check the pin**: `rust-toolchain.toml`
   travels with the sha, so a baseline from before a pin bump was built by a different
   compiler and the comparison must refuse, not proceed.
4. Run A and B sequentially (order-alternated if wall time is also being collected),
   ≥3 runs each. Gate on the instruction medians; report cycles and IPC beside them.
5. **The reference kernel is the cross-build control.** It is the same source in both
   binaries, `#[inline(never)]`, its own codegen unit; if its instruction count differs
   between arms by >0.5%, something other than the change under test moved (compiler,
   flags, feature set) and the comparison refuses. A control that cannot fail is
   vacuous; this one fails on exactly the class of environment drift that would
   otherwise be attributed to the diff.

**Alternatives weighed and rejected:**

- *Runtime-dispatched arms behind a bench-only feature* — keeps two live paths in the
  production crate (the two-worlds hazard the migration rule exists for), and the
  dispatch seam itself perturbs inlining in the code under test. Rejected.
- *A frozen reference implementation committed for benches* — ~16k lines that must
  keep compiling against moving shared APIs (interner, store, resolver); every
  refactor pays a tax to keep the corpse green, and the moment it drifts it validates
  nothing while looking like an oracle. Rejected.
- *Statistical brute force over wall time* — §12.103's alternated-rounds design is the
  best available shape and still could not claim at the effect sizes that matter; the
  noise is non-stationary (sign flips with arm order), so more rounds buy less than
  they appear to. Reserved for milestones, not units.

## Per-stage accounting, and how it cannot silently lose a stage

The breakdown that makes this *cycle accounting* rather than a single ratio:

- **Where the boundaries live:** `column_timed` already pins all seven stage
  signatures and derives `StageTimes` (worldgen-rewrite U4 note). The instrument
  extends that existing seam: read `(ri_instructions, ri_cycles)` at each boundary
  `column_timed` already crosses, storing deltas beside the durations. ~12 reads ×
  ~600 ns ≈ **7 µs per column against a 97.8 ms C_ss (0.007%)** — cheap enough to be
  unconditional in the diagnostic arm. Production `column()` is untouched;
  `column_timed`'s existing anti-drift control (byte-equality against `column()` with
  a fresh generator per arm) already gates the seam. This is an edit to
  `overworld/mod.rs`/`output.rs` — **brokered choke-point files**; the syscall wrapper
  itself belongs in a leaf helper.
- **Published metrics** (JSONL, existing `support::record` shape):
  `stage_<name>_instructions`, `stage_<name>_ipc`, `c_ss_instructions_median`,
  `c_cold_instructions`, and the derived headline
  `instructions_per_veg_draw` = vegetation-stage delta ÷ the RNG-draw counter.
  The draw counter comes from a **separate counters-on run** — sound to pair across
  runs precisely because app counters reproduce to the digit (905,459 3-of-3;
  11,034 draws unchanged across U18/U19), which is the property durations lack.
- **Vacuity guards, because the harness's own history is measuring a pipeline with
  stages missing** (the `Value::Null` resolver; the `patch=7x7` scene string over a
  3×3 sweep):
  - each live stage's instruction delta must exceed a non-vacuity **floor** (the
    deterministic analogue of the existing >1000 µs timing floors) — an early-returned
    stage reads near-zero instructions and fails loudly;
  - the paired counters-on run asserts `stage_entered` per stage (the below-the-early-
    return counters) and the exactly-once-per-closure counts (256/196/144 on a 12×12
    sweep) — two runs, two questions, never one run for both;
  - **scene strings are derived from the sweep constants, never restated** — the
    recorded `patch=` string and the loop bounds must come from one expression.
- **Inside a stage, the 600 ns read cost forces coarser tools, by design.** Per-draw
  or per-block reads would cost ~13% of the column (11,034 draws × 2 reads) and
  perturb what they measure. Within-stage attribution uses, in order: derived ratios
  (instructions per draw / per block / per probe, denominators from the counters run);
  `samply` **categorical** evidence (a named frame appearing/disappearing from the
  attribution table — §12.104's rule; the fixed `scripts/profile-cost-table.py` with
  its `(library, address)` join is the tool); and §12.106-style one-off mechanism
  counters (`scratch_misses()`-shaped) when a specific residual needs a name.

## Guard fix that must land with the instrument

`benches/support.rs`'s counters-poisoning guard **fails open**:
`timing_is_poisoned_by_counters` (`benches/support.rs`) refuses only units listed in
`ABSOLUTE_TIME_UNITS`, so a metric recorded in a *new* unit — including
`"instructions"` and `"cycles"` — records under `gen-counters` silently. That is the
same class as the four premise-false controls this drive caught: it reads as coverage.
The fix is to **invert the table to an allowlist**: a `COUNTER_SAFE_UNITS` list (`%`,
`x`, `calls`, `bytes`, `draws`, …) of units that may record while counters are on, and
refusal for everything else — so an unlisted unit fails **closed** and the loud
`REFUSING to record` line names it. `benches/` is U20's file set; this names the fix
for its owner rather than performing the edit.

## The campaign, once the instrument exists

- **Step 0 — re-baseline.** §12.98's per-stage shares (vegetation 52.0%, ore 22.2%)
  predate U8, U15's lookup work, U17, U18 and U19; they are stale and must not be used
  for targeting. First instrumented run publishes the instruction-denominated
  per-stage table (plus IPC and instructions-per-draw) in DESIGN.md as the ledger all
  later units diff against. The wall-clock C_ss is re-taken in the same session,
  labelled, so the instruction ledger has one time anchor.
- **The ledger is instructions; the goal stays wall-clock.** Unit acceptance criteria
  are written in instruction deltas (gateable at 1%); C_ss ≤ 1.0 ms remains the goal
  it always was (goal, not gate, per the owner's ruling), consulted at milestones via
  the artifact-arm wall-clock protocol.
- **First customer: the `RegionView::overlay` dense-array decision.** §12.106 refuted
  the presence-bitset by counting (~45% of 230,582 probes hit); the remaining
  candidate is the full dense array (1.77 MB + 3.1 MB per thread), whose cost is
  *cache pressure* — allocations read 0, wall time cannot see it, and instructions
  alone would mislead in the wrong direction. This is deliberately the proving case:
  it exercises the cycles/IPC half of the instrument, not just the instruction half,
  and a verdict either way validates the programme on a question counting alone
  could not close.
- **Second: vegetation per-draw cost.** §12.98's headline was ~4,781 ns/draw ≈ 14k
  cycles/draw, measured before U8/U17/U19. Re-measure as instructions-per-draw and
  cycles-per-draw; the worldgen-rewrite cost-per-draw candidates (bitset predicates,
  precompiled placement programs, incremental heightmaps) then land or are refused
  against a number with a 1% floor instead of a 22% one.

## Known gaps, stated rather than papered over

- **Two of `counters.rs`'s 24 counters are write-only** (measured 2026-08-07):
  `state_intern_new` and `state_name_lookups` are bumped from `interner.rs` (lines
  ~182/~244) and read by no test, bench or gate anywhere in `crates/` or `benches/` —
  and `docs/worldgen-state-interning.md` cites them as what the bench's attribution
  needs, which `benches/generation.rs` never reads. Delete them or give them a
  reader; this programme adds instruction metrics *beside* the counters and must not
  inherit the pattern (every metric it records has a named consumer above).
- **`ri_instructions`' spread was characterised on one kernel.** The 0.1–0.6% floor
  should be re-measured on the real bench workload in step 0 (the reference-kernel
  self-check does this automatically); if the worldgen pipeline's own spread is wider
  (e.g. any surviving `RandomState` iteration in a hot path is address-dependent), the
  gateable effect size moves with it and the ledger says so.
- **`benchmark-harness.md`'s Configuration section still requires the tokio
  `opt-level=1` workaround** (and the C_ss run snippet repeats it); the
  `rust-toolchain.toml` pin comment records the ICE fixed at `nightly-2026-08-07`
  (release workspace build + bench `--no-run` validated). Stale doc, U20's orbit;
  reported, not edited here.
