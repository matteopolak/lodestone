# Benchmark regression gate

## What it is

The committed half of the benchmark harness: `bench-baselines/*.json` holds what a
deterministic benchmark metric is *supposed* to be, `scripts/bench-gate.py` compares a
fresh run against it and fails on drift in either direction, and CI's `bench-gate` job
runs both on every push and pull request. It gates counts only — never a duration.

## Why the recorded half was not enough

Three pieces existed before this and none of them notices a regression on a machine that
has no history:

| piece | what it does | what it cannot do |
|---|---|---|
| 29 `benches/*.rs` across seven crates | measure, assert anti-vacuity properties, print | nothing invokes them automatically |
| each crate's `benches/support.rs` `record()` | append `{timestamp, git_sha, machine, profile, scene, metric, value, unit}` to `bench-results/<bench>.jsonl`, print a ±25% advisory against the previous same-machine run | the log is gitignored, so a fresh checkout and every CI runner start from nothing |
| `cargo xtask bench-compare` | ratio + verdict between two chosen entries of one such log | still needs two runs on one machine to exist first |

So the missing artifact was never a measurement. It was a *committed* number, plus
something that runs.

## What may be baselined, and what may never be

A baselined metric must be **deterministic**: a pure function of committed code and a
committed fixture. Counts of things the program did, byte totals of a structure whose
size is computed rather than sampled, fractions of a fixed capacity. Those read the same
on an M-series laptop and a loaded Linux runner.

`ALLOWED_UNITS` in `scripts/bench-gate.py` is that rule made structural rather than
documented — `--update` refuses to write an entry whose unit measures time, so a timing
cannot leak into a baseline later by someone extending the file. Both refusals are
exercised by the control suite.

**Durations are excluded on purpose, and it is the most important decision here.** A
committed duration baseline is a wall-clock ceiling wearing a different hat, and this repo
has already paid for one: a light test asserted `hood_best < 200.0` while its own comment
named the printed *ratio* as the deliverable. A 3×3 neighbourhood is nine columns and the
measured factor was ~8.7× — essentially linear and perfectly healthy — yet that ceiling
silently asserted `single_best < 22.2 ms`, an undocumented claim about how fast the
machine had to be, and it went red under load on code that was fine. Durations keep the
treatment they already have: recorded to `bench-results/`, compared by `cargo xtask
bench-compare` against the same machine's own history, advisory, never asserted.

## Would this have caught the regression that motivated the epic?

Yes, and that is the design criterion rather than a happy accident.

The regression was a render path rewriting one camera uniform per resident section per
frame — roughly 4000 buffer writes and 4000 bind-group binds per frame where one of each
sufficed. Median frame time 17.05 ms → 8.19 ms when it was fixed; main thread 94% of a
core → 56%.

That regression is **count-shaped**. The honest gate is "per-frame work does not grow
with resident section count", which is deterministic, and
`bench-baselines/render_submit.json` holds that pair at three render distances:
`terrain_per_draw_api_calls` and `terrain_drawn_sections` against
`terrain_drawable_sections` and `terrain_mdi_draw_slots`. At rd 16 that is 347 draws
against 2178 drawable sections; a per-section-uniform reintroduction pushes the first
number toward the second and the gate fails naming it.

A *timing* gate would have needed a ceiling somewhere between 8.19 and 17.05 ms on one
specific machine — the trap above. The count gate needs no such number.

Three honest limits on that claim, none of them small.

**The bind-group count itself is not gated, because it is not measured.**
`RenderStats::terrain_camera_bind_group_switches` lives in `lodestone-shell`'s
`benches/render_submit.rs`, which needs a GPU adapter and so does not run on a CI runner.
What CI gates is the `lodestone-render` side: draw-list sizes read out of the same
`WorldScene::plan_frame` a real frame builds its draw list from.

**One entry had to be removed for being a literal.** `terrain_mdi_api_calls` was recorded
as a constant `1` written in the bench source — "one multi-draw call per frame", read off
the renderer rather than measured. It was briefly in the first cut of this baseline, which
would have been a gate no code change could ever move: the vacuous shape dressed as a
regression check. It is gone from both the bench and the baseline; the bench's own comment
now says why.

**A regression that kept every count identical while making each call more expensive
passes.** That is what the advisory duration history is for, and it is the boundary of
what a count gate can do.

## Baseline storage, and who is allowed to move a number

- **Where it lives**: `bench-baselines/<bench>.json`, committed, one file per bench
  binary, keyed by `(scene, metric)`. Values sit beside their unit, a per-entry
  `tolerance_pct`, and a `required` flag.
- **Who updates it**: whoever writes the change that moves the number, in the same
  commit, via `just bench-baseline-update` (`scripts/bench-gate.py --update`). The
  update rewrites values only — tolerances and flags survive, so re-baselining can
  never quietly widen a band, and the control suite has a mutation proving that.
- **What happens on a legitimate improvement**: exactly the same thing. The band is
  **two-way**: an unexplained improvement fails too. That is deliberate. A one-way
  ratchet turns a baseline into noise, because the most common cause of a number
  improving is a benchmark that stopped doing the work — and a gate that celebrates
  that is worse than none. Making an improvement a red gate that you clear by committing
  the new number turns every real win into a reviewable line of diff, which is where the
  epic's own worked example (17.05 ms → 8.19 ms) should have been recorded and was not.
- **What stops the baseline becoming a wall**: nothing about updating it is privileged.
  It is one command and one committed diff, no approval step, no separate machine.
- **`required: false`** marks an entry whose measurement genuinely cannot run
  everywhere — the GPU-adapter-dependent arena occupancy metrics are the case today.
  Such an entry is reported `SKIP`, does not count toward `--min-compared`, and carries
  an `optional_because` string saying why.

## How the gate cannot pass vacuously

Three separate guards, each with a check in the control suite and a mutation proving the
check would notice its removal:

1. **`--min-compared N`** — fewer than `N` metrics actually compared exits **2**, not 0.
   An audit that checks nothing is unrun, not green. `just bench-gate` passes 40,
   against 50 comparable metrics on a machine with a GPU adapter and 42 without.
2. **A bench whose log is absent or empty exits 2** with a `NORUN` line naming it,
   separately from `--min-compared`, so a never-invoked bench does not report as a
   regression in every metric it owns.
3. **A required metric that stopped being recorded fails**, rather than silently
   dropping out of the comparison.

Exit status is `0` inside band, `1` drift or a required metric absent, `2` the gate did
not really run. Read it directly. `cargo test --workspace | grep … | tail` once reported
success in this repo while cargo returned 101.

## The control: a planted regression, run rather than described

`python3 scripts/test-bench-gate.py` — stdlib only, no pytest, the same shape as
`scripts/test-profile-cost-table.py` and for the same reason (these are Python scripts
and no crate owns them). 25 checks over synthetic fixtures in a temporary directory;
nothing is written inside the checkout.

The centre of it is a planted regression in the exact shape of the real one: a fixture
recording `camera_bind_group_switches = 347` against a baseline of `1`, with
`drawn_sections = 347` unchanged beside it. The gate must exit 1, must name the metric
that moved, and must not implicate the one that did not. Its control is the identical
fixture unplanted, which must exit 0 — without that pairing, a gate that always failed
would satisfy every red-expecting check in the file.

Every guard is then mutation-tested. Seven broken copies of `bench-gate.py` (via
`BENCH_GATE_PATH`, which points the suite at a copy so nothing in the shared checkout is
edited) each turn the suite red:

| mutation | check it kills |
|---|---|
| accept time units | a baselined duration is refused outright |
| one-sided band | an unexplained improvement fails too |
| ignore `--min-compared` | fewer comparisons than demanded is status 2 |
| missing required metric treated as skip | a required metric that vanished fails |
| unrun bench treated as green | a missing log is "did not run" even with the count guard off |
| ignore unit mismatch | a unit change is a failure, not a silent comparison |
| `--update` widens the tolerance | `--update` preserves the tolerance |

Two of these were written *after* a mutation survived, which is the whole point of
running the mutations rather than reasoning about them: the "unrun bench" check was
originally satisfied by the `--min-compared` guard firing on the same fixture, one
defect masking another, and the discriminating check (`--min-compared 0`) only exists
because the mutation survived.

That suite proves the value→verdict half of the chain. The bench→value half is proved by
running a real bench with a real change and watching the gate go red on the number it
recorded — see the next section.

## The end-to-end control against a real bench

Synthetic fixtures cannot show that the number a bench records is the number the gate
reads. Do that once, by hand, when changing anything in this path:

```bash
just bench-record && just bench-gate ; echo "exit=$?"     # expect 0
# edit a bench fixture so a gated count genuinely changes, then:
just bench-record && just bench-gate ; echo "exit=$?"     # expect 1, naming the metric
# revert the edit, then:
just bench-record && just bench-gate ; echo "exit=$?"     # expect 0 again
```

The third step is not optional. A gate that goes red and stays red proves nothing about
the gate.

That sequence has been run, against a real production regression rather than a fixture.
Dropping the reachability term from `WorldScene::plan_frame`'s visibility decision
(`let visible = in_frustum && reached` → `let visible = in_frustum`) is a realistic
over-draw bug: frustum culling still runs, so the bench's own `drawn < drawable`
anti-vacuity assertion still holds and the run completes normally. Measured:

| render distance | drawn sections, healthy | with the regression | ratio |
|---|---|---|---|
| rd 8 (867 sections) | 101 | 213 | 2.109 |
| rd 12 (1875 sections) | 205 | 433 | 2.112 |
| rd 16 (3267 sections) | 347 | 725 | 2.089 |

`just bench-gate` exited **1**, naming exactly six metrics — `terrain_drawn_sections` and
`terrain_per_draw_api_calls` at each of the three distances — with 44 others still inside
their bands, so the failure pointed at the regression rather than smearing across the
file. Reverting the one-line edit and re-running returned it to **0** with 50 metrics
compared. Conditions: macbook.local, aarch64 macOS, `cargo bench` profile, sha `a0e24e6`,
other agents building on the same machine (which is why the *counts* are the evidence and
the durations from those runs are not quoted).

## Configuration

| knob | where | effect |
|---|---|---|
| `--min-compared` | `just bench-gate` passes 40 | below this many comparisons, exit 2 |
| `tolerance_pct` | per baseline entry | ±band as a percentage; `0` means exact. Against a baseline of `0` it reads as an absolute allowance in the metric's own unit, since a ratio against zero is undefined and zero is a real value (a healthy leak probe records exactly zero bytes of growth) |
| `required` | per baseline entry | `false` skips the entry when unrecorded, for measurements that cannot run everywhere |
| `LODESTONE_BENCH_RESULTS` / `LODESTONE_BENCH_BASELINES` | environment | override either directory; `--results-dir`/`--baseline-dir` win over both |
| `BENCH_GATE_PATH` | environment, control suite only | point the suite at a copy of the gate, for mutation testing |

## Which benches CI runs, and why only those

`just bench-record` runs `meshing` and `render_submit` from `lodestone-render` and
`memory_footprint` from `lodestone-world`, in criterion's `--test` mode — one iteration
per benchmark, since the recorded counts need no samples. That subset is exactly the one
that is hermetic (no vanilla jar, no GPU adapter required, no network) *and*
count-producing. Neither package reaches `cpal`/`alsa-sys`, which is why that job needs no
apt step.

Deliberately excluded, each for a reason rather than an oversight:

- **`lodestone-worldgen`'s `generation`** — real generator, tens of seconds per sweep, and
  its interesting numbers are stage percentages and per-column timings.
- **`lodestone-shell`'s `render_submit` and `frame_profile`** — need a GPU adapter.
- **`lodestone-world`'s `session_rss`** — reads resident-set size through a Darwin-only
  syscall; the same FFI already broke a Linux CI job once at link time.
- **`lodestone-server`'s `server_tick`** — runs a real tick loop; see below.
- **everything else** — timings, which are never gated.

Adding a bench to the gated set is two edits: name its target in the `bench-record`
recipe, and add its deterministic metrics to a new `bench-baselines/<bench>.json`. Raise
`--min-compared` in the same commit or the new entries can silently stop being compared.

## The server tick

`crates/lodestone-server/benches/server_tick.rs` measures the one thing every other bench
in this workspace does not: the server's own 20 Hz loop. Deciding whether ticking should be
partitioned by region needs the tick's own cost first, and specifically needs to know
whether that cost is population-driven or fixed overhead — a question no client-side
benchmark can answer.

It drives a real `run_tick_loop` through `IntegratedServer::open_in_memory_with_mobs`
with the runtime built `start_paused(true)`: the loop's *waiting* is virtual and its
*work* is real, so 200 ticks cost work-time rather than ten seconds of sleeping, and the
tick count is deterministic (N advances produce N ticks) instead of a function of machine
speed.

**The trap it documents by avoiding it**: `TickStats::mspt_avg_ms` is derived from the
runtime's own clock, which is the clock being paused. Reading cost from it would report a
tick that costs approximately nothing — a number that is not so much wrong as meaningless,
and whose meaninglessness is invisible in the value. The bench measures with a plain
`std::time::Instant` around the advance loop instead, and prints both side by side and
labelled so the difference is on the record.

Its own control that it measures anything is a population sweep: after the constructor's
asynchronous world install completes, the fixture seeds the live server surface with exactly
zero or 48 mobs in the same 5x5 area. It asserts those exact rosters before requiring the
populated arm to do more chunk-source work. That avoids configuration-dependent demo population
and makes removed fixture seeding fail before it can make the work comparison vacuous. The fixture
resets its source counters after that setup, so the recorded per-tick counts exclude asynchronous
world installation.

### Fixture limits

Both sweep points use a flat, four-layer in-memory world with no terrain, redstone, block
entities, or real chunk churn. It is a deterministic simulation-floor fixture, not a
production-shaped cost sample. A generator-backed or region-backed source would fold column
generation into the first cold access and needs its own scene definition before its duration can
be compared. Do not infer a production tick cost from this fixture's wall-clock result.

Not in the CI gated set: it builds `lodestone-server`, `lodestone-v26-2` and their graph
for a job that would otherwise build only the render path. Run it directly:

```bash
cargo bench -p lodestone-server --bench server_tick
```

`TickStats` includes the three per-phase summaries and the worst recorded phase window, so
the bench can read them through `IntegratedServer::tick_stats()` without exposing its clock.
Each sweep asserts a cumulative phase count of 200 and a rolling phase count of
`min(200, TICK_HISTORY_LEN)` for every phase, then records every summary's rolling and cumulative
counts, p50/p95/p99/max duration, and over-budget count. It also records the worst duration and
its tick index, alongside the phase-labelled worst-duration metric retained for easy filtering.
The cumulative count proves every driven tick reached each phase; the rolling count preserves the
percentile window's bounded semantics. Under the paused runtime the phase durations are tied at
zero, so the first phase owns that diagnostic window; it proves the phase recorder is wired, but
is not a cost measurement. The wall-clock figures remain the cost figures for this fixture.

## Dependencies

- `python3` (stdlib only) for the gate and its control suite. A missing interpreter must
  fail the job loudly; "python3 absent ⇒ skip" is the precondition species of vacuous
  test.
- `criterion`, `default-features = false, features = ["cargo_bench_support"]` at every
  bench site — that combination excludes `plotters`, `rayon`, `itertools`, `regex` and
  `walkdir` entirely, so no bench pulls code into a non-bench build.
- `bench-results/` stays gitignored. It is per-machine measurement data; only
  `bench-baselines/` is repository state.

## Related

- [`roadmap/benchmarks.md`](./roadmap/benchmarks.md) — what is measured and why those
  things, plus the harness design this closes the last piece of.
- [`render-benchmarks.md`](./render-benchmarks.md) — the GPU-adapter benches and the
  frame profile.
- [`oracles-and-benchmarks.md`](./oracles-and-benchmarks.md) — live oracles and the
  worldgen sweeps.

## Shared recorder and local policy

The common JSONL recorder now lives in the native-only, opt-in
`lodestone-testsupport::bench_record` module. The seven ordinary benchmark families
reach it through tiny `benches/support.rs` re-export shims; worldgen retains a small
local wrapper for its counter-poisoning policy. That wrapper must fail closed for new
units, because the counter instrumentation changes timing and work-performed
measurements.
