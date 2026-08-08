# Worldgen staged sharded store

## What it is

The memoisation layer under `OverworldGenerator`'s 3×3-of-3×3 neighbourhood recursion: a
concurrent map from chunk position to a per-chunk entry holding one slot per intermediate
product, with **compute-exactly-once** as a structural property rather than a hope. Unit 6 of
[`docs/plans/worldgen-rewrite.md`](plans/worldgen-rewrite.md); it replaced two `Mutex`-guarded
FIFO caches whose contention had already forced a per-ring barrier back into `lodestone-server`
(`4307b59`), and it lands the plan's acceptance criterion exactly — each neighbour stage computed
once per chunk reached, 3 of 3 under 289-way concurrency, byte-identical to the old engine over
28,496,229 dumped bytes.

Code: [`crates/lodestone-worldgen/src/overworld/store.rs`](../crates/lodestone-worldgen/src/overworld/store.rs),
driven from `overworld/mod.rs`'s `pre_ore_stage` and `post_ore_world`.

## Why the old shape had to go

The generator needs neighbours' intermediate products, deeply: `vegetation_stage(C)` reads the
*post-ore* world of the 3×3 around `C`, and each of those post-ore worlds runs `ore_stage`, which
reads the *pre-ore* world of its own 3×3. So one cold `column()` closes over a 5×5 pre-ore region.
Memoising that is not optional — without it a sweep redoes each neighbour up to 9×, and a 144-chunk
debug sweep measured 700.57 s when ore composition landed, matching the predicted ~9×.

The two caches that did it (`PreOreCache`, `PostOreCache`) were each one
`Mutex<HashMap + VecDeque>`, FIFO-evicted at 512, and had two defects:

1. **One global lock per cache**, taken ~17 times per column for pre-ore alone. A 289-column join
   burst produced ~5,000 concurrent attempts on one `Arc<Mutex>`.
2. **Racing misses recomputed.** Both released the lock across the computation — correct, since
   holding it would serialise every worker — which let two threads racing one key both run the whole
   pipeline. `pre_ore_stage`'s own comment conceded *"the work really was done twice."*

`4307b59` is the record: *"Revert per-ring barrier removal — cache contention with 289 concurrent
generator calls."* The barrier was a workaround for the cache, and the cache a workaround for the
recomputation. A `Mutex` only excludes code that takes it, and then becomes the reason nobody
restructures the thing it guards (DESIGN.md §12.94).

## How it works

`StagedStore<E>` is generic over the per-chunk entry payload, so the store never depends back on the
generator. `overworld/mod.rs` supplies `ChunkStages { pre_ore, post_ore }`, one `StageSlot` each.

**Two levels of lock, and neither is global.**

| level | what it is | held for |
|---|---|---|
| shard | 64 independent `Mutex<HashMap<ChunkPos, Slot>>` | one `HashMap` probe, one `Arc` clone, one `u64` write |
| entry | a `OnceLock<Arc<T>>` per (chunk, stage) | a hit is an atomic load, **no lock**; a miss runs the computation inside the once-guard |

The **sharding key is the exact chunk position**, hashed by `shard_of`: both coordinates through
distinct odd multipliers, XOR with a rotate, then the *high* bits masked. Reading the high bits is
what makes it work — a naive `(cx ^ cz) & mask` maps whole diagonals onto one shard, which is
precisely the access pattern a 3×3 driver has. Two unit tests assert the scattering (≥15 of 64 shards
for a 5×5, ≥6 for a 3×3) against a bound derived from the uniform-hash expectation, so a degenerate
hash cannot pass as a sharded one.

Why it cannot meaningfully contend, stated as an argument rather than a hope:

- The shard critical section contains no computation and no allocation on the hit path (an insert
  allocates, once per chunk ever), so it is tens of nanoseconds against milliseconds of generation.
- 289 concurrent columns spread ~26 lookups each over 64 shards.
- Adjacent chunks — requested together — are in different shards by construction, not by luck.

**`OnceLock::get_or_init` is what makes once-only structural.** A second thread arriving mid-computation
*waits for the value* instead of computing its own copy. The hit/miss counter is bumped **inside** the
closure, so `pre_ore_computed` is the number of distinct chunks whose stages really ran; a thread that
loses the race is counted as a hit, which the old `bump(true)`-then-compute shape could not express.

**The wait is real, it is now measured, and it is the largest remaining loss in the join burst.**
A thread that arrives at an empty slot mid-computation is *parked* — correct, and deliberately
counted as a hit, so no counter in `lodestone_worldgen::counters` can see it. `store::wait_stats()`
can: `waits` / `wait_nanos` / `computes` / `compute_nanos`, process-global, nothing on the hit path
(`StageSlot::get_or_compute` pre-checks the `OnceLock` and returns before touching an `Instant`, so
the ~800 real misses in a 289-column burst pay for it and the ~26 hits per column do not).

`waits` is **exactly 0** in any single-threaded run, which is its calibration; §12.132 measured it
at **24–37% of pool capacity** (`wait_nanos / (window × wall)`) across every window from 4 to 20 on
the 289-column burst. The cause is structural rather than a defect: the generation window is
spatially *contiguous*, so adjacent in-flight columns share 20 of their 25 pre-ore entries and
`window - 1` workers can all be parked on the one entry the remaining worker is computing. That
sharing is exactly what makes them hits rather than cold computes, so it is a trade and not a bug —
but reducing it means making the parallel unit a **store entry** rather than a column, which is
open work. `crates/lodestone-server/tests/join_parallel_efficiency.rs` reports it per window.

**Deadlock-freedom is a rule, not luck.** `post_ore` may call `pre_ore` for any chunk; `pre_ore` calls
nothing in the store; no stage re-enters its own slot. The wait-for graph only points downward and its
lowest layer never waits. `get_or_init` *does* deadlock on self-reentry, so this ordering is
load-bearing — see "How to change it".

### Eviction is view-scoped, never capacity-FIFO

Capacity eviction is how a "cache" silently starts recomputing, and its neighbour — two distinct
chunks sharing one value — is on the record in this crate: `pre_ore_stage`'s doc describes a
*clamped-key* cache in `FeatureOracle.java` that aliased two chunk coordinates and hung a JVM oracle
on a non-reentrant semaphore. So:

- **Exact keys only.** `ChunkPos` is the literal `(cx, cz)`. Nothing rounds, clamps or merges a key.
- **In-flight neighbourhoods are pinned.** `column()`/`column_timed()` open a `ViewScope` over
  `STRUCTURE_CLOSURE_RADIUS` (= `COLUMN_CLOSURE_RADIUS + structures::REFS_RADIUS` = **10**), and pinned
  entries are *structurally ineligible* for reclamation. Eviction therefore cannot cause a recompute
  inside a request — not a probability argument.
- **The ceiling is derived.** `STORE_RETENTION = 2048` comes from the D4 burst itself: 289 columns are
  a 17×17 view whose full closure is 37×37 = **1,369** chunks, so retention must exceed 1,369 or the
  very burst this exists for could evict its own working set.
- **Both of those numbers were 2 and 512 until issue #514, and leaving them was a measured 4× C_ss
  regression** (DESIGN.md §12.130). `pre_ore` reads `structure_refs`, whose `REFS_RADIUS` = 8 walk makes
  one column's closure 21×21 = 441 entries rather than 25 — so the pin covered 25 of 441 and the 12×12
  sweep's 1,024-entry working set evicted the neighbours it was about to read back: `pre_ore_computed`
  **740 against a predicted 256**, steady-state allocations **87,882 against 118**, C_ss **79.9 ms
  against 21.0 ms**. Nothing about the eviction policy was wrong. The pin was narrower than the closure.
- **Entry count is not proportional to memory, which is why 2,048 is affordable.** Only entries whose
  `pre_ore`/`post_ore` slots were actually computed hold a dense grid (~192 KiB); the extra entries this
  ceiling admits are structure-starts-only and hold a `Vec` of starts. Per column of travel a session
  adds ~21 structure-only entries against ~5 terrain-bearing ones.
- **"Nothing was evicted" is checkable.** `store_evictions()` is observable, so the gates assert zero
  rather than assuming it. That is what licenses reading the stage counters as one-per-chunk.

Eviction is always *safe* regardless of policy — a slot's value is a pure function of its key and the
generator's fixed state, so dropping one can only cost a recompute, never a wrong answer. Reclamation
skips pinned entries and entries whose `Arc` is still held elsewhere, takes one shard lock at a time,
and lets losers of an `AtomicBool` swap carry on rather than pile up.

#### There are **two** insertion paths, and both must check the ceiling

The single most expensive thing to know about this file. `entry()` inserts, and `open_view()` inserts
**too** — pinning *creates* every slot in the box. And `column()` opens its pin **before** touching a
stage, so by the time `entry()` runs the slot already exists, `entry()`'s `inserted` flag is always
false, and the ceiling check inside it is **unreachable in the game**.

For one release only `entry()` checked the ceiling. The consequence was not eviction thrash, which is
what reading the check would predict — it was **no reclamation whatsoever**: `reclaim()` never ran in a
real session at all, and the store grew by `2R + 5` entries (21 at R=8, ~7.9 MiB) per chunk of travel,
without bound, for as long as the player kept walking. Issue
[#503](https://github.com/matteopolak/lodestone/issues/503),
[`worldgen-store-distance-leak.md`](./worldgen-store-distance-leak.md), DESIGN.md §12.108–§12.109.

`open_view()` now checks it **after the whole box is pinned**, and that ordering *is* the correctness
argument rather than a style choice:

- A reclaim pass skips pinned entries, so once the whole box is pinned this scope's closure is
  ineligible **by construction** — the property that keeps eviction view-scoped rather than a capacity
  guess.
- **Inside** the loop that guarantee does not hold yet. At iteration *k* only *k* of the box's entries
  carry a pin, and the rest are typically the **oldest unpinned entries in the whole store** — a
  neighbouring column visited a moment ago — so they sort to the *front* of the candidate list. A pass
  there would evict slots this very request is about to compute into, converting a memory bound into a
  guaranteed recompute on the hot path.

`reclaim()` holds no lock when it is called, so there is no competing reason to move it earlier.

Two costs are deliberately bounded rather than eliminated. The check is gated on `fresh_inserts > 0`,
so a steady-state view re-opening an already-resident neighbourhood does not even perform the atomic
load — the same "misses only" rule `entry()` follows. And `reclaim()` is `O(live · log live)` per
insert while over the ceiling; at retention 2,048 that is a few thousand map probes against a ~20 ms
column. Measured: the whole 100-step hermetic walk (1,989 column visits, 2,029 evictions) costs
**0.03 s in release**, ~15 µs of store-and-reclaim work per column, or **0.03%** of a real column.
That figure is negligible *because* the ceiling holds, so the two properties are coupled.

## What was measured

Release, real embedded data, seed 42. Closure sizes derived from the drivers *before* measuring.

**12×12 sweep** — pre-ore reaches `-2..=13` (16×16 = 256), post-ore reaches `-1..=12` (14×14 = 196):

| | computed | lookups served | evictions |
|---|---|---|---|
| pre-ore | **256** | 3,204 | 0 |
| post-ore | **196** | 1,296 | 0 |

`144 × 2 = 288` is the **wrong** reading of "chunks × stages": each stage has its own closure radius,
so the chunk count differs per stage.

**The serial sweep does not distinguish the two designs.** Measured: the old engine also read 256/196,
because serially a FIFO cache never has a racing miss and 512 > 256 never evicts. Anyone re-measuring
D4 single-threaded will find no difference and conclude there was nothing to fix. The defect is
concurrency, so the comparison is the **289-column burst**, three runs per arm:

| arm | pre-ore computed (true: 441) | post-ore computed (true: 361) |
|---|---|---|
| old (`4be59556`) | 452, 452, 448 | 380, 383, 372 |
| **new** | **441, 441, 441** | **361, 361, 361** |

The old arm over-computes *and varies run to run* — the racing-miss signature. The new arm lands on
the exact closure 3 of 3, deterministic down to the hit counts (5,698 / 2,240 every run).

**Byte identity.** A 12×12 sweep dumped as raw bytes (min_y, height, palette strings in order, every
block index, biome quarts — 28,496,229 bytes) is byte-identical under `cmp` against `4be59556`,
measured in an isolated detached worktree with its own `--target-dir` and an md5-verified identical
harness in both arms. The absence claim has a control: a copy with one bit flipped at offset
14,000,000 is reported differing at char 14,000,001.

**No speedup is claimed.** Counters-off burst wall time, six samples per arm alternated old/new/old/new:
old 40.2 / 44.7 / 50.7 / 54.3 / 55.3 / 106.1 s, new 41.6 / 43.4 / 52.3 / 61.1 / 80.0 / 103.1 s. The
ranges overlap almost completely and both arms' second round is inflated by machine load, so the drift
is time-ordered rather than arm-attributable. Following U3's precedent (DESIGN.md §12.100), the number
is recorded and the claim declined; the evidence is the counter, which is arithmetic. One calibration
fell out: **counters-on inflates this burst ~3×** (130–149 s against 40–55 s), so a counter run and a
timing run must never be the same run.

### The interner round trip U6 also deleted

Auditing against the plan's "nothing shared-mutable on the hot path" target found the store clean but
two loops in `overworld/decorate.rs` still routing every cell through the per-generator
`StateInterner`'s `RwLock`: `get` resolved a cell to a `&'static str` and `set` handed it straight back
to `interner.id_of` — to recover an id the source grid already held. Per post-ore chunk that is
**884,736** read guards in `stitch_region` (9 sources × 98,304 cells) plus **98,304** in the ore
fold-back. A read guard increments a shared atomic, so all eight burst workers were ping-ponging one
cache line: the same class of defect as the caches, one layer down, and invisible to a serial
measurement. Fixed with `get_id`/`set_id`, safe because every production grid shares one
`Arc<StateInterner>`, and palette *order* is untouched so `into_palette_and_blocks` still emits a
byte-identical `Vec<String>` (DESIGN.md §12.100's trap).

## How to change it

- **To add a stage** (Unit 9's memoised per-source biome is next): add a `StageSlot` field to
  `ChunkStages` in `overworld/mod.rs`. Nothing in `store.rs` needs to know what the stages are.
- **Add it *above* the stages it consumes, and never make a stage depend on itself.**
  `OnceLock::get_or_init` deadlocks on same-slot reentry. The current layering is
  `post_ore → pre_ore → nothing`; keep the graph pointing one way.
- **If a driver's neighbourhood widens, widen `STRUCTURE_CLOSURE_RADIUS` with it**, or the pin stops
  covering the request that needs it, and re-derive `STORE_RETENTION` from the new closure. **This is
  the rule issue #514 broke**, and it broke it by adding a stage *above* `pre_ore` rather than by
  touching a driver — so "did any neighbourhood widen?" has to be asked of new stages too, not only of
  the decoration drivers. `benches/generation.rs`'s sweep now asserts `store_evictions() == 0` and
  `structure_starts_computed == closure(10)`, which is what would catch the next one.
- **Do not add a shared scratch pool.** Any buffer reuse belongs in a `thread_local` free-list; a pool
  behind a lock re-creates exactly the contention this module deleted. The store currently owns **no**
  scratch at all — `ViewScope` re-derives its pinned set from `centre`/`radius` on drop rather than
  holding a buffer. Reclamation does **not** change this: it allocates its candidate `Vec` on the
  reclaiming thread's own stack frame and touches no shared buffer, and only one thread runs a pass at
  a time (losers of the `AtomicBool` swap carry on). Entry-level hits still take **no lock at all** — a
  `OnceLock` atomic load — and the ceiling fix added exactly one `Relaxed` load, on the insert path,
  behind a `fresh_inserts > 0` guard.
- **If you add a third insertion path, it must check the ceiling too, after pinning.** See "There are
  two insertion paths" above. The failure mode is silent: the store simply stops being bounded, every
  existing gate stays green, and the symptom reaches the player as the game degrading the further they
  walk.
- **Never gate reclamation on a call shape `column()` cannot produce.** Both original reclamation tests
  drove their inserts through `entry()`, which production cannot reach as an inserter, so both were
  green while reclamation was dead in the game — the *world* species, invisible in the test source.
  `a_view_walked_across_the_world_stays_inside_the_retention_ceiling` exists to drive `open_view()`
  over a **moving centre**, which is what production does. Do not "simplify" it to use `entry()`.
- **Do not "fix" a counter gate by adding tests to its binary.** The counters are process-global
  atomics. The first version of the counter gate shared a binary with the byte-identity and burst
  tests and, under `--test-threads=2`, read `pre_ore_computed = 502` against a true 256 — a 96%
  over-count that looks exactly like a broken store. A `OnceLock` around the sweep is **not**
  sufficient: it serialises tests that *read* the sweep and does nothing about tests that call
  `column()` themselves. Nothing in `staged_store_counters.rs` may generate except its `sweep()`.

## Gates

Always-on: `overworld::store`'s 10 unit tests. Three are worth knowing about —
`the_old_probe_release_compute_shape_recomputes_under_the_same_race` is a **negative control** that
observes the old cache shape recomputing under the same 16-thread race, so the once-only test is not
vacuous; `a_pinned_neighbourhood_survives_massive_over_pressure` drives the store 100× past its
ceiling and asserts reclamation *actually fired* before believing that a pinned entry survived; and
`a_view_walked_across_the_world_stays_inside_the_retention_ceiling` is the #503 regression guard.

The walk gate is the only test here that drives the **production** insertion path, and it is cheap
(0.03 s) because its payload is a `u64` rather than terrain. Three properties make it worth trusting:

- It **slides a 17×17 view 100 steps** and checks a *curve*, not a point — every 20th step is asserted
  and all samples print on failure.
- It states **both hypotheses from outside the measurement**: bounded at ≤ 512 + 25, or `441 + 21 × 100
  = 2,541` if reclamation never runs. The assertion requires the measurement to be within the first
  *and* a factor of four away from the second, so it is a magnitude check and not a direction one.
- Its join-view closure is asserted to be exactly **441**, which is what says the hermetic model
  reproduces production's geometry rather than some cheaper shape that could not leak in the first
  place. It held on both arms, and the model's whole curve matched the real server instrument
  digit-for-digit (441/861/1281/1701/2121/2541 unfixed; 512 with 349/769/1189/1609/2029 evictions
  fixed).

Byte identity under reclamation — `#[ignore]`d, release, `crates/lodestone-server/tests/store_reclaim_identity.rs`:

```text
cargo test --release -p lodestone-server --test store_reclaim_identity -- --ignored --nocapture \
  reclaimed_columns_regenerate_byte_identically
cargo test --release -p lodestone-server --test store_reclaim_identity -- --ignored --nocapture \
  a_concurrent_burst_past_the_ceiling_matches_serial_bytes
```

The first walks a 140-column strip (closure ~720 against the 512 ceiling), captures 20 columns' wire
bytes *before* anything is evicted, walks on until they are reclaimed, and regenerates them. The second
runs a **21×21 = 441-column** burst on 8 threads — deliberately wider than `staged_store_gates.rs`'s
R=8 burst, whose 441-entry closure is chosen to evict *nothing* — so reclamation and concurrent
generation happen at the same instant, and compares every column to a serial arm. Both arms reclaim,
and they reclaim on **different schedules** (113 evictions serial against 577 parallel) while producing
identical bytes, which is the interesting part: the result does not depend on *when* entries were
dropped. Both gates carry an `evictions > 0` detector, because without it either would be a re-run of
an existing no-eviction gate.

`#[ignore]`d, release, multi-minute — the end-to-end evidence:

```text
cargo test --release -p lodestone-server -p lodestone-worldgen \
  --features lodestone-worldgen/gen-counters \
  --test staged_store_counters -- --ignored --nocapture

cargo test --release -p lodestone-server --test staged_store_gates -- --ignored --nocapture
```

The counter gate forks on `counters::enabled()` rather than skipping: with counters off it asserts the
store's own entry count (an instrument-independent upper bound on stage computations) *and* that the
hooks are provably inert, so a zero could never be mistaken for a pass. `the_sweep_actually_drives_both_neighbourhoods`
is the *world*-species control — `lodestone-worldgen`'s own fixture resolvers supply no biome
documents, so both 3×3 drivers would early-return and a gate written against them would sweep 144
chunks, touch zero neighbours, and pass.

Also run by any change here: the 13 worldgen parity binaries, the `overworld_gen.rs` composed fixture,
`column_is_byte_identical_across_two_independently_constructed_generators`, both `worldgen_data.rs`
vegetation seam gates, and `parallel_generation_is_deterministic_and_matches_serial`.

## Configuration

| knob | where | note |
|---|---|---|
| `SHARD_COUNT` | `store.rs`, compile-time | 64; power of two so `shard_of` masks instead of dividing |
| `STORE_RETENTION` | `overworld/mod.rs` | 2,048, derived from the 289-column burst's 1,369-chunk closure |
| `COLUMN_CLOSURE_RADIUS` | `overworld/mod.rs` | 2, derived from the drivers (3×3 post-ore of 3×3 pre-ore) |
| `STRUCTURE_CLOSURE_RADIUS` | `overworld/mod.rs` | 10 = `COLUMN_CLOSURE_RADIUS + REFS_RADIUS`; **this is the pin radius** |
| `gen-counters` | `lodestone-worldgen` feature, default **off** | required for the counter arm of the gate |

## Dependencies

`std` only — `HashMap`, `Mutex`, `OnceLock`, and relaxed atomics. The store bumps no counters itself,
so it stays independent of `lodestone-worldgen-core`'s `counters`; the generator passes
`counters::bump_pre_ore`/`bump_post_ore` in as the outcome callback.

## Where this file lives, and where it will live

The plan's Unit 6 row names `src/engine/store.rs`, which needs a `pub mod engine;` line in `lib.rs`.
`lib.rs` was being rewritten by Unit 16's concurrent leaf-crate extraction when this landed, and it is
a choke point this repo has been burned on, so the store went under `overworld/` — a directory Unit 6
owns outright — for zero contention. It names no generator type, so relocating it under an `engine/`
module once Unit 4 creates one is a pure move plus a re-export.
`lodestone_worldgen::overworld::store` is public today so **Unit 10**'s server-side scheduler can
drive it. **U10 has landed** (`7ba0176b`, issue #494): the per-ring barrier is gone, replaced by a
window whose width comes from `available_parallelism` rather than the view radius, and the same
289-column burst driven through it reads 441/361 with hits 5,698/2,240 — this doc's numbers exactly,
three runs of three. See [`join-scheduler.md`](join-scheduler.md). It also found that the barrier's
removal needed *both* halves of `4307b59` addressed, not just this store's.
