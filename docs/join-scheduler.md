# Join generation scheduler

## What it is

The server-side scheduler that decides in what order, and with how much concurrency, the
`(2r + 1)²` chunk columns of a joining player's view are generated. It is a **primed sliding
window** over the wire order, and it replaced the per-ring barrier that `4307b59` reinstated —
Unit 10 of [`plans/worldgen-rewrite.md`](plans/worldgen-rewrite.md), issue #494.

Code: [`crates/lodestone-server/src/join_scheduler.rs`](../crates/lodestone-server/src/join_scheduler.rs),
driven from `server.rs`'s `ConfigurationFinished` arm. The wire *order* still comes from
`join_view_rings` in the same file.

**It is no longer only a join scheduler, despite the name.** `ColumnPipeline::enqueue` lets a
live pipeline take columns it was not built with, and `send_view_update` uses that to feed it the
newly-visible strip a walking player reveals — so a move is streamed by this machinery rather than
generated in one fan-out on the connection task. Everything below applies unchanged to those
columns; [`server-view-streaming.md`](server-view-streaming.md) is why the steady-state path
needed it and what else had to change for the pipeline to survive being empty.

## Why the barrier existed, and why it could go

The join loop used to walk the Chebyshev rings and, for each ring, spawn every one of its
columns into the blocking pool and **wait for all of them** before asking for the next ring. So
ring `r + 1` could not start until ring `r`'s slowest column finished, and the per-ring tails
stacked.

`5104adf` deleted it by spawning all 289–361 columns at once. `4307b59` reverted that:
*"cache contention with 289 concurrent generator calls"*. **Two defects were live in that revert
and only one of them was the cache:**

| defect | the shape it took | fixed by |
|---|---|---|
| racing misses recomputed | two `Mutex`-guarded FIFO memo caches; ~5,000 concurrent attempts on one `Arc<Mutex>` per burst | Unit 6's staged store ([`worldgen-staged-store.md`](worldgen-staged-store.md)) |
| **in-flight count scaled with the *view radius*** | `(2r + 1)²` concurrent `spawn_blocking` calls — 289 generator calls on an 8-core machine | this module |

Unit 6 (`34202a21`) removed the first. A stage now computes exactly once regardless of arrival
order, so the barrier's stated rationale — *"ring 0 seeds the cache, ring 1's columns hit those
cache entries"* — describes nothing.

The second was never the cache's fault, and deleting the barrier without addressing it would
reintroduce it. That is why Unit 10 is a **scheduler** rather than a deletion: the in-flight
count is derived from `available_parallelism`, never from the view, so `5104adf`'s failure is
unreachable at any radius.

## How it works

### The dependency edges

From the plan's parallel model, for a column `C`:

* fill / surface / carve depend on the seed alone — embarrassingly parallel;
* `ore(C)` reads `pre_ore(3×3(C))`;
* `veg(C)` reads `post_ore(3×3(C))`, which closes over `pre_ore(5×5(C))`;
* `top_layer(C)` depends on `veg(C)` alone.

So two columns at Chebyshev distance ≥ 5 share **no** store entry; adjacent columns share 20 of
their 25 pre-ore entries. Those shared entries are the real dependency edges, and the store's
per-entry `OnceLock` is what honours them — a second worker arriving mid-computation *waits for
the value* rather than computing its own copy.

**That per-entry wait is the synchronisation the barrier was standing in for**, and it is
per-edge rather than per-ring: two workers block only when they need the same chunk's same stage
at the same instant. Because the window is a **contiguous** window over the outward ring order,
the in-flight set is always spatially local, so those shared edges are hits rather than
independent cold computations. Nothing in the scheduler knows that; it falls out of scheduling in
wire order.

### The window

`ColumnPipeline::next` tops the in-flight set up to the window, then awaits and emits the
**oldest** one. Emission order is therefore the order the coordinates were handed in, whatever
order the pool finished them in — which is what keeps the emitted byte sequence a pure function
of `view_radius`.

`generation_window()` is `available_parallelism`, floored at 2 — **one in-flight column per
hardware thread**:

* **the floor of 2** is what makes a window a window, and stops the ring-overlap detector in the
  gates being vacuous on a single-core host.
* **there is no ceiling, deliberately.** Growth with cores rather than with radius is the entire
  difference from the reverted commit.

It was `2 × available_parallelism` until §12.132, where the factor of 2 turned out to cost a
third of the burst's throughput. It was there for encode overlap — the caller awaits a column then
writes it to the socket, and at a window of exactly `parallelism` the pool is idle for that
write — which is a real effect and much smaller than what the extra concurrency spends. A window
sweep over the real 289-column burst, **instructions retired flat to 1.4% across every arm** so
the comparison is of scheduling rather than of work:

| window | wall | speedup over serial | cycles/col vs serial | IPC |
|---|---|---|---|---|
| 4 | 4.78 s | 2.00× | 1.01× | 5.27 |
| 6 | 4.19 s | 2.28× | 1.05× | 5.09 |
| **8** | **3.67 s** | **2.60×** | 1.26× | 4.25 |
| 10 (`P`) | 4.28 s | 2.23× | 1.96× | 2.73 |
| 12 | 4.87 s | 1.96× | 2.54× | 2.11 |
| 20 (`2P`) | 6.43 s | **1.49×** | **4.39×** | **1.23** |

A U with a steep right-hand side. The cause is **cache capacity**, not a lock: instructions are
flat, cycle inflation is negligible up to a window of 4, and normalising each arm by its own
single-threaded IPC puts a shared generator's collapse at 1.59× against 1.40× for a private
generator per column. `tests/join_parallel_efficiency.rs` is the sweep and
`the_production_window_sits_at_the_measured_optimum` is the standing gate.

### Why "primed"

Issue #453's property is that the player's own column reaches the client after **one** column of
generation, not after the whole view. A plain sliding window loses it: the window is filled
*before* the head is awaited, so on a fast source the whole window completes first and "columns
generated before the first chunk was encoded" jumps from 1 to `window`.

So the first top-up is to **1**, and to `window` only after the head has been emitted. The head
column is generated alone — exactly the one-column serialisation #453 already bought and
documented as a deliberate trade — and every column after it runs with the window fully open.
The barrier is deleted for rings 1 onward and kept, deliberately, for the single column of
ring 0.

### Only the `Shared` arm is scheduled

`SourceRef::Borrowed` (the transport tests) holds a source that is not `'static`, so it cannot be
spawned: every batch is a `generate_columns_parallel` call that blocks until the whole batch
finishes. A window's whole payoff is overlapping generation with an already-finished column's
encode, and a blocking source has nothing to overlap.

Worse — measured while building the gates, and the reason the first version of this landing was
wrong: the rings' cumulative sizes are `1 + 8(1 + 2 + … + r)` = `1 + 4r(r + 1)`, and `r(r + 1)` is
always even, so **every ring boundary sits at an offset ≡ 1 (mod 8)** — exactly where a window-8
batch boundary sits. No batch even straddles a ring, and ring 8's single 64-column blocking batch
would become eight serial ones. The split adds barriers rather than removing them.

So that arm keeps the rings. What is held identical across the arms is the **wire order**, which
is what the client sees, and both arms are gated for it (`805a1fb`).

## What was measured

Release, real embedded data, seed 42, at `7ba0176b` in an isolated detached worktree.

**The 289-column burst, concurrent, with stage counters** (`join_scheduler_counters.rs`).
289 = 17×17, the shape `4307b59` names. Window 20 on this machine:

| | computed | hits | store_len | evictions |
|---|---|---|---|---|
| pre-ore | **441** | 5,698 | 441 | 0 |
| post-ore | **361** | 2,240 | — | 0 |

Three runs, all three identical including the hit counts — the same numbers Unit 6 landed. The
old FIFO cache's signature was over-computation **plus run-to-run variance** (452/452/448 and
380/383/372), so an exact, repeated match is the discriminating result.

**Why this cannot be measured serially.** Unit 6's central finding: serially a cache never has a
racing miss, so the old and new stores read identical counts. That is exactly the reasoning that
produced `4307b59` — *measure barrier removal serially and it looks free*. Any re-measurement of
this unit must be a concurrent burst.

**Structural counters** (`join_scheduler_gates.rs`, always-on, 289 columns over a stagger-only
probe source). Each subject reading sits beside a control that must fail it:

| arm | rings in flight | columns in flight | generated before 1st emit |
|---|---|---|---|
| window (subject) | **2** | **8** | **1** |
| per-ring barrier (control) | 1 | 64 | 1 |
| pre-#453 flat (control) | 9 | 279 | 289 |

The first two controls are the *same* arm, which is the point: `4307b59` conflated two defects,
and the barrier only ever addressed one of them.

**Time-to-first-chunk, unchanged.** Asserted as a counter (`generated before 1st emit == 1`, on
both arms, with the pre-#453 shape reporting 289) and separately as `check_proximity_stream`'s
rule 3 over a real loopback socket on the `Shared` arm. Reported as wall clock over six
alternated rounds measuring *only* the first emit, so the rest of the burst's load cannot
contaminate it: barrier 486 / 556 / 421 / 365 / 414 / 527 ms against window 491 / 512 / 405 /
347 / 403 / 488 ms — lower in 5 of 6, mean 441 ms against 462 ms.

**No total-throughput speedup is claimed.** Full-burst wall time with the arm order alternated
came to window 21.7 / 22.9 / 15.0 / 12.7 s against barrier 23.4 / 27.7 / 17.6 / 16.1 s — the
window arm lower 4 of 4 — but both arms drift by more than the gap across rounds on a machine
running four agents, and an earlier non-alternated run of the same harness showed the opposite
sign. Following Unit 6's precedent (DESIGN.md §12.100), the numbers are recorded and the claim
declined. The acceptance is the counters, which are arithmetic.

## How to change it

- **The window's width is the one tuning knob, and it must stay a function of cores.** Making it
  a function of the view is `5104adf`. `the_window_never_scales_with_the_view` fails if it does.
- **Do not widen the window to buy throughput without re-running the sweep.** The curve above is
  a U, the coefficient is a *proxy* for a cache bound this code cannot query, and the measured
  floor on the reference machine is 8–10 against an `available_parallelism()` of 10 — because that
  call counts 4 efficiency cores alongside 6 performance ones. `the_production_window_sits_at_the_measured_optimum`
  compares against the same run's best arm, so it gives a verdict on any machine without this file
  knowing anything about it.
- **The remaining loss is parking, not contention.** 24–37% of pool capacity across the whole
  sweep sits parked on the staged store's per-entry `OnceLock`, because a contiguous window means
  adjacent columns want the same pre-ore entries. `store::wait_stats()` counts and times it and
  reads exactly 0 single-threaded. Reducing it means changing what the parallel unit *is* —
  scheduling store entries rather than columns — which no ordering-preserving tweak to this file
  reaches.
- **Do not remove the priming.** A plain window is one line shorter and silently regresses #453
  from one column to `window` columns of generation before the first chunk reaches the client.
  `exactly_one_column_is_generated_before_the_first_emit` is the guard.
- **Emit in coordinate order, never in completion order.** The front of `inflight` always pairs
  with `coords[next_emit]`; a column that finishes early sits in the queue. Draining with a
  `select!`/`JoinSet` over the in-flight set would be the natural "optimisation" and would make
  the wire depend on thread scheduling.
- **If the store's closure radius widens, this scheduler does not need to know**, but the burst
  counters' expected values do: they are `(2·(r + 2) + 1)²` and `(2·(r + 1) + 1)²`, derived from
  the drivers in `join_scheduler_counters.rs`'s constants.
- **A counter gate needs its own binary.** `lodestone_worldgen::counters` are process-global
  atomics; the first version of Unit 6's gate shared a binary and read 502 against a true 256.
  Nothing in `join_scheduler_counters.rs` may generate except `measure()`, and all three of its
  arms run inside one `OnceLock` so the counters can be reset between them.
- **Never turn a burst counter regression into a widened tolerance.** 441/361 are derived from
  the drivers, not fitted to a run.

## Configuration

| knob | where | note |
|---|---|---|
| window width | `generation_window()` in `join_scheduler.rs` | `available_parallelism`, floor 2, no ceiling (was `2 ×` until §12.132) |
| wire order | `join_view_rings` in `server.rs` | Chebyshev rings outward; the scheduler never reorders |
| `GATE_WINDOW` | `tests/join_scheduler_gates.rs` | 8, fixed so the structural counters do not vary with the host |
| `gen-counters` | `lodestone-worldgen` feature, default **off** | required for the counter arm; inflates a burst ~3× |

## Dependencies

`tokio`'s blocking pool (`spawn_blocking`) on native targets; on `wasm32-unknown-unknown` the
window is forced to 1 and columns are generated inline, matching
`chunk.rs`'s `generate_columns_offloaded` `cfg`. The once-only property it relies on belongs to
[`worldgen-staged-store.md`](worldgen-staged-store.md); the ordering property it must preserve
belongs to issue #453 and is gated in `tests/serve_play.rs`.
