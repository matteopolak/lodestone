# Benchmark regression gate

## What it is

The committed half of the benchmark harness. `bench-baselines/*.json` holds what each deterministic benchmark metric should be, `scripts/bench-gate.py` fails on drift in either direction, and CI's `bench-gate` job runs both on every push and pull request. It gates counts only, never a duration.

## How it works

Benches append `{timestamp, git_sha, machine, profile, scene, metric, value, unit}` records to the gitignored `bench-results/<bench>.jsonl` through `lodestone-testsupport::bench_record`; the gate compares the latest record per `(scene, metric)` with the baseline (a fresh runner has no history, hence the committed baseline).

**What may be baselined.** Only a pure function of committed code and fixture: counts of work, byte totals of computed structures, fractions of a fixed capacity. `ALLOWED_UNITS` makes this structural and `--update` refuses a time unit. Durations would be a wall-clock ceiling that goes red under load; they stay advisory via `cargo xtask bench-compare` on the same machine's history. The design target is a count-shaped regression such as a render path rewriting a camera uniform per resident section per frame (about 4000 writes instead of one): `bench-baselines/render_submit.json` gates `terrain_per_draw_api_calls` and `terrain_drawn_sections` against `terrain_drawable_sections` and `terrain_mdi_draw_slots` at three render distances (rd 16: 347 draws of 2178 drawable sections). Limits: `RenderStats::terrain_camera_bind_group_switches` needs a GPU, so CI gates `WorldScene::plan_frame` draw-list sizes; a literal in bench source is vacuous and must not be baselined; a regression keeping counts while making calls dearer passes (duration history covers it).

**Baseline rules.**
- One file per bench binary keyed by `(scene, metric)` with `value`, `unit`, `tolerance_pct`, `required`; the `bench` identity is unique across files. Whoever moves a number updates it in the same commit with `just bench-baseline-update` (values only; tolerances and flags survive).
- The band is two-way: an unexplained improvement fails too (commonly a bench that stopped working).
- `required: false` (GPU arena occupancy) reports `SKIP`, does not count toward `--min-compared`, and needs an `optional_because` string. `value` and `tolerance_pct` must be finite, tolerance non-negative.
- A latest `null` or non-numeric result shadows older history (`MISSING`) until a numeric record revives it.

**Anti-vacuity guards** (each with a control and a mutation proving the check would notice its removal): `--min-compared N` (`just bench-gate` passes 40; 50 comparable with a GPU, 42 without) exits 2 when fewer; an absent or empty log exits 2 `NORUN`; a required metric no longer recorded fails; a duplicate `(scene, metric)` or duplicate `bench` identity exits 2; a null latest shadows history. Exit status: `0` in band, `1` drift or required metric absent, `2` the gate did not really run (read it directly, not through a pipe).

**Control suite.** `python3 scripts/test-bench-gate.py` (stdlib) runs 36 synthetic checks; the core is a planted regression (`camera_bind_group_switches = 347` against a baseline of `1`) that must exit 1 naming only the moved metric, paired with an unplanted fixture exiting 0. Eleven mutated gate copies (via `BENCH_GATE_PATH`) must each turn it red: accepting time units, one-sided band, ignoring `--min-compared`, missing-required as skip, unrun bench as green, ignoring unit mismatch, `--update` widening tolerance, duplicate keys, non-finite values, duplicate bench names, reusing older value after null.

**End-to-end control.** The suite proves value to verdict only; when changing this path prove bench to value once by hand:

```bash
just bench-record && just bench-gate ; echo "exit=$?"   # expect 0
# change a bench fixture so a gated count really moves, then:
just bench-record && just bench-gate ; echo "exit=$?"   # expect 1, naming the metric
# revert, then:
just bench-record && just bench-gate ; echo "exit=$?"   # expect 0 (a gate that stays red proves nothing)
```

Measured once with the reachability term dropped from `WorldScene::plan_frame` (over-draw): drawn sections at rd 8/12/16 went 101/205/347 to 213/433/725 (ratio about 2.1), the gate exited 1 naming exactly the six drawn/draw-call metrics with 44 in band, then 0 after reverting.

### Which benches CI runs

`just bench-record` runs `meshing`, `render_submit` (`lodestone-render`) and `memory_footprint` (`lodestone-world`) in criterion `--test` mode (one iteration): hermetic, count-producing, and neither package reaches `cpal`/`alsa-sys`. `render_submit` records `entity_upload_buffers` from the CPU upload plan, and with an adapter also runs the real uploads and asserts they match. Excluded: `lodestone-worldgen` `generation` (tens of seconds, timing-shaped), `lodestone-shell` `render_submit`/`frame_profile` (GPU), `lodestone-world` `session_rss` (Darwin-only syscall that once broke Linux link), `lodestone-server` `server_tick`. To gate a new bench: name it in the `bench-record` recipe, add `bench-baselines/<bench>.json`, raise `--min-compared` in the same commit.

### The server tick bench

`cargo bench -p lodestone-server --bench server_tick` (not CI-gated) measures the 20 Hz loop to show whether tick cost is population-driven or fixed overhead. It drives a real `run_tick_loop` through `IntegratedServer::open_in_memory_with_mobs` on a `start_paused(true)` runtime (virtual waiting, real work, deterministic tick counts); since `TickStats::mspt_avg_ms` reads the paused clock (about zero) it also times with `std::time::Instant`. Controls per scene:

- Population sweep: exactly 0 or 48 mobs in one 5x5 area, roster asserted and sampled each tick; the populated arm must retain more resident state.
- Cache starvation: the 5x5 area fits under the integrated cache floor, so `cold_columns_during_ticks` must be 0 over 200 ticks (a synthetic generation fed to the same predicate must be rejected first).
- Generated arm (`generated overworld world, mobs=0 area=1x1 view_radius=1`): exactly one setup generation and zero during 48 ticks; records `setup_column_generations`, `cold_columns_during_ticks`, `overrun_count`.
- Every arm requires zero `TickClock` overruns (a backlog check, no claim about host speed). Phase stats assert a cumulative count of 200 and rolling `min(200, TICK_HISTORY_LEN)`; under paused time durations tie at zero, so this proves the recorder is wired, not a cost.

The flat fixture (four-layer in-memory world) is a simulation-floor control; infer no production cost from its wall-clock. A session that both streams generated columns and answers keep-alives is an untested gap. `python3 scripts/keepalive-benchmark.py` runs the paused-clock keep-alive scenarios by exact name (silent client disconnected after an unanswered challenge; responsive client survives four intervals), each exactly one passing test, emitting `scenarios`, `passed`, `failed` counts (`wall_ms` advisory).

## How to change it

Moved a gated number: `just bench-baseline-update` in the same commit. New gated bench: see above. New guard: add a control and a mutation to `scripts/test-bench-gate.py`.

## Configuration

| knob | where | effect |
|---|---|---|
| `--min-compared` | `just bench-gate` passes 40 | below this, exit 2 |
| `tolerance_pct` | per entry | percent band; `0` exact; against baseline `0` an absolute allowance |
| `required` | per entry | `false` skips when unrecorded |
| `LODESTONE_BENCH_RESULTS` / `LODESTONE_BENCH_BASELINES` | env | override directories (`--results-dir`/`--baseline-dir` win) |
| `BENCH_GATE_PATH` | env, control suite | point the suite at a mutated copy |

## Dependencies

`python3` (stdlib only; a missing interpreter must fail the job loudly); `criterion` with `default-features = false, features = ["cargo_bench_support"]` at every bench site; `bench-results/` is gitignored per-machine data and only `bench-baselines/` is repo state; worldgen keeps a local `bench_record` wrapper that fails closed for new units. Related: [roadmap/benchmarks](./roadmap/benchmarks.md), [render benchmarks](./render-benchmarks.md), [oracles and benchmarks](./oracles-and-benchmarks.md).
