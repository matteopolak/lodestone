# Redstone execution model

## What it is

How a redstone change actually gets *executed* — what wakes up, what it costs,
and why. This is the layer underneath `docs/redstone.md`'s per-device
behaviour: the neighbour-notification cascade, the scheduled-tick drain, and
the palette-derived reaction classification (`lodestone_server::redstone_graph`)
that decides which family, if any, a notification dispatches to.

Its headline result is a correction to the premise it was commissioned under.
The model was believed to do a **per-tick neighbour scan**. It does not, and
never did — it has been event-driven since it was written, and an idle
contraption already costs literally zero. The real cost was a large **constant
factor per notification**, and that is what got removed.

---

## The model, as it is

Redstone work enters from three event-shaped places. **Nothing scans the world
for redstone on a tick.**

1. `lodestone_server::random_tick::propagate_and_react` — called once per
   just-mutated position, from a random tick that changed a block, from the
   scheduled-tick drain, and from placement.
2. `lodestone_server::tick::run_tick_loop`'s scheduled-tick drain, which
   touches only entries whose trigger tick is due.
3. The special-shape scheduled bodies beside it (tripwire recheck, gravity
   settle, piston commits, target decay, dispenser fire).

`propagate_and_react` hands each mutation to
`lodestone_server::neighbor_update::NeighborPropagator::propagate`, a
depth-first cascade over `UPDATE_ORDER` (west, east, down, up, north, south)
with a chained-update cap — a port of vanilla's `CollectingNeighborUpdater`.
Each notification lands in `random_tick::react_to_notification`, which
dispatches to one device family or to nothing.

**The ordering machinery is the specification and is deliberately untouched by
everything below.** `UPDATE_ORDER`, the depth-first interleave, the
scheduled-tick queue's `(trigger tick, priority, insertion)` drain order and its
per-`(pos, kind)` dedup, and dust's deterministic 7-centre × 6-direction fan-out
are all observable vanilla behaviour. A faster engine that reorders two updates
is a regression, not an optimisation.

---

## The measurement that shaped the design

The workload is `crates/lodestone-anvil/tests/redstone_benchmark.rs`, which
stamps a real community contraption onto a flat world, runs the **real**
`IntegratedServer` tick loop, and reads `lodestone_server::redstone_counters`.
Counters rather than durations: wall clock reproduces at ~10.8% on this machine
against 0.16–0.21% for structural counts, and the quantities in question are
counts by nature. See `docs/redstone-benchmark-harness.md` for the fixtures and
their provenance.

```
cargo test -p lodestone-anvil --test redstone_benchmark -- --ignored --nocapture
```

`raid_farm.litematic` (1393 blocks, 142 redstone components) — its own two
captured mid-cycle repeater rechecks re-injected through the production
dispatch:

| phase | notifications | cell_reads | state_parses | signal_queries | wire_recomputes |
|---|---|---|---|---|---|
| steady state | 0 | 0 | 0 | 0 | 0 |
| while active | 837 | 5899 | 55 | 164 | 157 |

Two facts come out of that pair, and they point in opposite directions from the
premise:

- **The steady-state row is a real zero, not a rounding.** Incrementality at
  the tick level already exists. There is no per-tick scan to remove, and the
  "null contraption" control an execution-model rework would want passes by
  construction.
- **The active row is genuinely large.** Two events cascade into 837
  notifications and 5899 raw block-state reads inside one tick — about 41 cell
  reads per redstone component in the contraption, from two initiating events.

So the cost is per-*event*, and the question is what one event costs, not how
often the world is scanned.

### Where the per-event cost went

Before this work, `react_to_notification` opened **every** notification by

1. cloning the cell's block-state string onto the heap
   (`column.block_state(..).to_string()`), then
2. running a chain of fifteen family predicates over it — `is_gravity_block`,
   the `snowy` family, `is_wire`, `is_torch`, `is_repeater`, `is_piston`,
   `is_comparator`, `is_hopper`, `is_observer`, `is_openable`, the note block,
   `is_powered_rail_family`, `is_dispenser_family`, `is_tnt_block`,
   `is_command_block_family` — each of which splits the string at `[` and does a
   `strcmp`,

and only then decided, for the large majority of notifications, that the cell
reacts to nothing and the cascade is empty.

---

## The change: palette-derived reaction classification

`lodestone_server::redstone_graph` names the dispatch's own decision as a value.
`ReactionClass` has one variant per family plus `Inert`; `classify` maps a
canonical block-state string to its class, mirroring `react_to_notification`'s
predicate chain **in the same order, first match wins**.

The point of naming it is that it can then be computed **once per palette
entry** rather than once per notification. `ChunkColumn` already carried two
such tables for exactly this reason — `palette_ticking` (randomly-ticking
classification) and `palette_state_ids` (the string→id boundary that keeps the
protocol encoder off strings). `palette_reaction` is the third, appended in
`ChunkColumn::intern` and rebuilt in `ChunkColumn::recalc_ticking_counts`, and
`ChunkColumn::reaction_class` answers "what reacts here" in two array indexes.

`react_to_notification` now reads the class first and returns immediately for
`Inert`; every family guard is a `class == ReactionClass::X` comparison instead
of a string predicate. **No arm body changed.**

### Why this is the incremental structure, and what it is not

The shape the design brief imagined is a reverse index of edges: "when this cell
changes, wake these listeners". That was evaluated and deliberately not built.

- A stale edge is **silently wrong**; rediscovery is self-healing on every
  event. Every incident record in this repo about stale derived state argues for
  buying an index only with evidence.
- Skipping the *enumeration* of neighbours is not free of ordering risk.
  `NeighborPropagator::propagate` counts every notification it issues against
  its chained-update cap and returns the issued list; suppressing an inert
  notification at the propagator would move where the cap trips and change what
  `issued` contains. Deciding **at dispatch** rather than **at enumeration**
  keeps `issued` byte-identical.
- The win a listener index buys per notification is exactly the win the
  classification already buys: a notification landing on a cell that reacts to
  nothing costs one array read and a branch. What is left to save is the array
  read itself.

The classification has no invalidation step at all, and therefore no staleness
class. The palette is append-only (`ChunkColumn::intern` is the only writer and
`palette` is private, so that is compiler-enforced), so a classification cannot
outlive the state it classifies. The incrementality is by construction.

A memo/incremental-computation crate (`comemo`, `salsa`) was also considered and
rejected: both memoize pure functions over tracked immutable inputs, while this
is a mutable spatial grid whose observable behaviour includes the *order* of
recomputation. The hot cost was discovery and parsing, not recomputation of a
pure value. A framework dependency shaping the whole subsystem would also have
to earn its place in the wasm32 bundle this crate links into.

---

## What the design handles, and what it does not

Stated up front rather than discovered later.

| case | handled how |
|---|---|
| **Update order** | Untouched. The classification changes what a notification costs, never which notification is issued, in what order, or how many are counted against the chained-update cap. |
| **Quasi-connectivity** | Unaffected. QC is a property of a piston's *read* set (`piston::has_extend_signal` reads `pos.above()`, which vanilla never notifies it about); the classification only decides which arm runs, and the arm's reads are unchanged. Any future index built from *read* sets would destroy QC while looking more thorough — edges must mirror notification topology, never reads. |
| **Repeater/comparator delay, tick scheduling** | Untouched. Scheduling happens inside the arms; the queue, its `DRAIN_ORDER` and its dedup are not involved. |
| **Property-sensitive dispatch** | Works. A palette entry is a whole canonical state string, so a predicate reading `powered=true` classifies correctly per entry. |
| **Neighbour-sensitive dispatch** | **Not supported, by design.** A predicate that reads any *other* cell cannot be a per-entry classification. Such a test must stay inside its arm's body. `redstone_graph`'s module doc states this as the rule for adding a family. |
| **Chunk boundaries** | **Closed on every path a player action or the world tick can drive.** `lodestone_server::random_tick::RedstoneColumns` replaces the single `&ChunkColumn` the reaction dispatch, the placement arms and the tripwire arms read and write through: home stays a plain `&mut` borrow, and any neighbouring column a cascade actually reaches is fetched lazily via `ChunkSource::is_column_resident` (never generated) and cached for the rest of that one cascade. Every production entry point takes a `world: &dyn ChunkSource` and builds it over the real `ChunkStore` — `crate::tick`'s scheduled-tick drain (including the torch/repeater/comparator reads that precede a re-propagate), target-block hits, falling-block landings and `RandomTickScheduler::tick_chunk`'s notify fan-out, plus `random_tick::react_at_placement_with_entities` and `react_at_removal` under `crate::server`'s placement and break handlers. The single-column form is `#[cfg(test)]`: `propagate_and_react`, `propagate_and_react_with_entities` and the `NoNeighbors` source they build over do not exist outside a test build, which is what makes "no production cascade is bounded to one chunk column" a compiler-checked property rather than a comment. |
| **Unloaded chunks** | Unchanged. The table is derived data: a column unloading drops it, a reload rebuilds it from the palette. Nothing is persisted, so there is no reload staleness. `RedstoneColumns`'s own neighbour cache is stricter than a full reload would need: it is scoped to one cascade and dropped at the end of the call, so nothing about cross-chunk reach is retained between notifications either. |
| **Piston/slime movability** | Out of scope. "Which blocks move when this piston fires" is a connected-component query over a different relation with different edges. It shares the caching *idea* and none of the topology. |

---

## The evidence

### Exhaustive differential, not a sample

`redstone_graph`'s
`classification_agrees_with_the_dispatch_chain_for_every_state_in_the_game`
walks **every** block state in 26.2 (`lodestone_data::block_states::STATE_COUNT`,
**32,366** of them), rebuilds each into its canonical `name[k=v,…]` form, and
requires `classify` to agree with a second, independent transcription of the
dispatch chain written from the dispatch site rather than from `classify`. The
domain is finite and all of it is checked, so the claim is not "the two agree on
the states I thought of".

Mismatches are **collected and asserted on the collection**, not asserted inside
the loop: an `assert!` in the loop proves one arm and leaves the rest arguments
rather than observations.

`the_exhaustive_gate_can_actually_fail` is its control — a deliberately wrong
classifier must produce a non-zero disagreement count. Without it, "no
mismatches" is equally consistent with "the comparison never ran".

`chain_probe_positions_match_the_dispatch_order` pins the derived cost model
(below) against the chain's real arm order, so the reported saving is checkable
arithmetic rather than an assertion.

### Counters

`redstone_counters::Snapshot::notifications_by_class` buckets every notification
by the class of the cell it landed on, at the same hook as
`notifications_issued`, so the two sum identically. Two derived figures come off
it:

- `chain_probes_avoided` — how many string family predicates the old chain would
  have evaluated for the same notifications (each ran the chain from the top
  until its arm matched; an inert cell ran all fifteen).
- `dispatch_state_clones_avoided` — how many heap-allocated block-state string
  copies the old dispatch made and this one does not.

### What was measured after

**Behavioural identity on the real contraption.** The same harness command,
re-run against `raid_farm.litematic` with its two captured repeater rechecks
re-injected, reads:

```
notifications_issued=837  cell_reads=5899  state_parses=55
signal_queries=164  wire_recomputes=157
schedules_requested=3  schedules_deduped=15  max_notifications_per_drain=726
```

**Every one of those eight counters is identical to the baseline above**,
reproduced across three independent runs including one under deliberate
six-way CPU load. This is the differential that matters: it is the only
measurement phase driven by a deterministic re-injection rather than by a
wall-clock settle, and it is the one that exercises redstone.

One caveat, recorded rather than smoothed over. `bee-and-crop-farm`'s
*steady-state* row read `notifications_issued=153` at baseline and **159** in
every after-run — stable, reproduced three times, and unmoved by CPU load, so
not noise. It is nonetheless not attributable to this change: that phase
reports `cell_reads=0` and `max_notifications_per_drain=6` in both runs, which
means every notification in it returned an **empty cascade** and no family arm
that reads anything ever ran. The total is therefore
`6 × (independent mutations in the window)`, trimmed at column edges — a
quantity determined by the random-tick pass over that fixture's crops, which
this change cannot influence. Thirty-six commits and a good deal of other
agents' uncommitted `lodestone-server` work landed in the shared checkout
between the two measurements. The pre-change binary was not rebuilt to settle
it further.

**Behavioural identity on the hermetic fixture the previous cost-split pass
used.** `redstone_counters::tests::measured_cost_split_for_a_fifteen_cell_dust_run`
— a 15-long dust run lit from one end — reads, with the classification live:

```
notifications_issued=659  cell_reads=3038  reactions_total=155
state_parses=0  signal_queries=152  wire_recomputes=152
schedules_requested=0  schedules_deduped=0  max_notifications_per_drain=659
```

Every one of those is **identical** to the reading taken before the
classification existed. That is the point: the change is a constant-factor
change, and a counter that moved would mean a behavioural change.

**The saving, on a fixture with five families populated.** The 15-cell run plus
a non-interacting row carrying a repeater, an observer and a comparator
(`the_class_histogram_agrees_with_what_the_dispatch_actually_ran`, run with
`--test-threads=1`):

```
reaction-class histogram over 677 notifications:
  inert=519  wire=152  torch=3  repeater=1  comparator=1  observer=1
inert = 519 (76.7% of all notifications)
string family predicates avoided = 8274 (12.22 per notification)
block-state string clones avoided = 519
```

**Three quarters of the notifications in a live circuit land on a cell that
reacts to nothing.** Each of those previously cost a heap-allocated copy of the
cell's state string plus a full fifteen-predicate scan to establish exactly
that; each now costs one array index and a branch. The 12.22 predicates per
notification is the whole-fixture average, inert and non-inert together.

Both figures are for these fixtures' shapes, not universal constants — the
inert share depends on how much inert scaffolding a circuit is embedded in, and
a real contraption is embedded in a great deal more than a test fixture is.

**The counters are process-global and their `TEST_LOCK` only covers their own
module.** Any counters reading must be taken with `--test-threads=1` or a filter
narrow enough to exclude every other `propagate_and_react` caller in the crate,
or the number is a hypothesis about contamination rather than about redstone.

---

## Crossing a chunk seam: the three entry points, and their evidence

The reaction dispatch reached across a chunk seam before the placement, removal
and random-tick entry points did, so a circuit was cross-chunk only if the edge
that drove it arrived through the scheduled-tick drain. A block a *player*
placed or broke, and any mutation the random-tick pass made, still truncated at
the home column's own 16-wide footprint. Each of the three now takes a
`world: &dyn ChunkSource`.

### Where the expected values come from

Neither source is this crate.

**The live-server attenuation table.** `redstone_oracle_gate`'s
`ORACLE_DUST_ATTENUATION` is dust power by distance from its source, probed
cell-by-cell on a real 26.2 server with
`execute if block <pos> minecraft:redstone_wire[power=N]` for `N` in `0..=15`,
three independent readings that agreed exactly. That module carries the full
provenance.

**Spatial-boundary invariance.** A 16x16 column is an administrative unit of
*storage*; redstone has no player-visible concept of one. So the same relative
geometry must produce the same result whether or not a seam falls inside it,
and the reference reading is the same fixture laid out inside one column. For
the tripwire case the reference cannot be the same *length* — a legal run
reaches 41 cells, which is two and a half columns — so it is the same shape at
a shorter legal length, and the invariant asserted is that a hook's resulting
state does not depend on run length within the legal range or on where the seam
fell.

### The three gates, and what each predicts

All three live in `random_tick`'s test module and drive the **production**
entry point, not the reaction dispatch directly.

| gate | driven through | predicted value |
|---|---|---|
| `a_placed_source_drives_its_dust_run_across_a_chunk_seam_to_the_live_server_profile` | `server::propagate_placement_with_entities` | `15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4` at world x=14..25, per coordinate |
| `breaking_a_tripwire_reaches_the_hook_in_the_next_chunk_and_matches_the_single_column_run` | `server::propagate_removal_with_entities` | both hooks `attached=true, powered=true`; one recheck at the scanning hook, 10 ticks out |
| `a_random_tick_mutation_notifies_the_observer_across_a_chunk_seam` | `RandomTickScheduler::tick_chunk` | exactly `[((16, 1, 8), "redstone:observer", 4175)]` |

The coordinates are chosen so the candidate models disagree at the *first* cell
past the seam rather than only in aggregate. For the dust run, at world x=16 —
three cells from the source:

| model | power at x=16 |
|---|---|
| seam-invariant (the oracle) | 13 |
| cascade truncates at the column edge | 0 |
| cascade crosses but restarts at full strength | 15 |

A run ending on 0 or 15 under the correct model would make two of those three
coincide, which is why the run is twelve long and ends on 4. The observer gate
predicts the whole scheduled tick rather than its existence, over a deliberately
unround `current_tick` of 4173: a model scheduling at the current tick, at a
repeater's `2 * delay`, or under another kind lands somewhere else than 4175.

### The controls

Two per gate, and the second is the one that makes the first evidence.

**Watched fail.** With the three `world` arguments at the production call sites
replaced by the single-column source and nothing else changed, all three gates
fail at the cross-seam assertion while their single-column reference arms still
pass — so the failure is the seam, not the rig:

```
dust at world (x=16, y=1, z=8) in chunk (1, 0), 3 cell(s) from the placed source:
  our model says power=0, the live 26.2 server measured power=13.
  full profile: [(1, 15), (2, 14), (3, 0), (4, 0), ... (12, 0)]
the controlling hook at world x=2 ... left: powered=false, right: powered=true
the observer across the seam ... left: [], right: [((16, 1, 8), "redstone:observer", 4175)]
```

**Residency.** Each gate re-runs its own fixture with the far chunk's data
seeded and readable but declared *not* resident, and requires the pre-existing
truncation exactly. `TestWorld` carries a seeded-but-unloaded split for this:
without it, "unloaded" and "unseeded" are the same state and a control cannot
read back the cell it is proving was not written.

## The lookup allocation removal

The classification above cut what a notification costs to decide whether to
react at all. It left the read itself untouched: every `redstone::*`/
`redstone_wire::*`/`redstone_torch::*`/`redstone_diode::*`/
`redstone_observer::*`/`redstone_rail::*`/`redstone_tripwire::*`/
`redstone_openable::*`/`redstone_dispenser::*`/`piston::*`/`block_support::*`/
`block_placement::*` query function takes a `lookup: F` closure, and every one
of those closures used to return an owned `String` — a fresh heap allocation
and byte copy on **every call**, even though `ChunkColumn::block_state` and
`RedstoneColumns::state` were both already reading from data already sitting
in memory.

**The fix is a shared type, not a per-call-site rewrite.** `redstone::WorldState`
is `std::sync::Arc<str>`, and it replaces `String` in every one of those
closures' bound (`F: Fn(BlockPos) -> WorldState`) and in `redstone_dispenser`'s
two `&dyn Fn(BlockPos) -> WorldState` parameters. `ChunkColumn` gains a fourth
per-palette-entry derived table, `palette_arc: Vec<Arc<str>>`, built in the same
two places (`intern`, `recalc_ticking_counts`) as `palette_ticking`/
`palette_state_ids`/`palette_reaction` — so `ChunkColumn::block_state_arc`
answers a read with one `Arc::clone` (an atomic increment) instead of a fresh
allocation, exactly the way `block_state`'s `&str` answer was already free.
`RedstoneColumns::state`/`raw_state` (the cross-chunk read) call it through
whichever column — home or an already-loaded neighbour — the position falls
in; nothing about *which* column answers a read, or the residency boundary
that gates it, changed. `chunk::air_state_arc()` is the equivalent for the
"outside every reachable column" answer: a `LazyLock<Arc<str>>` built once per
process and cloned, not allocated, on every out-of-bounds read.

**Because `Arc<str>` derefs to `&str` exactly like `String` does**, almost
every call site needed no body change at all — `&state`, `.starts_with(..)`,
`base_name(&state)`, `is_wire(&state)` and every other read-only use kept
compiling unchanged. The real edits were at the boundary: a handful of places
that build a *new* owned `String` to write back to the world, store in a
`RandomTickEvent`, or feed a `ScheduledTickQueue<String>` now call
`.to_string()` once, at the point that actually needs ownership, rather than
every closure call needing it up front. `redstone_tripwire::WireSource::state`
became `WorldState` outright (the field is filled once, in
`find_controlling_hooks`, and read many times inside a 41-cell scan, the same
"build once, clone cheaply" shape as the palette table).

### Correctness

**Update order is untouched.** This is a return-type change on a query
closure; it touches nothing that decides which notification fires, in what
order, or how many count against the chained-update cap.

**Byte-identical counters, re-run against the same real contraption.**
`redstone_contraptions_report` (the same production tick loop, the same
`raid_farm.litematic` fixture, its own two captured repeater rechecks
re-injected) reads, after this change:

```
notifications_issued=837  cell_reads=5899  state_parses=55
signal_queries=164  wire_recomputes=157
schedules_requested=3  schedules_deduped=15  max_notifications_per_drain=726
```

— identical to every reading before it, including the one taken before the
classification work landed. A pure allocation-strategy change cannot show up
in a counter that only tracks decisions, and this run is the proof it does not.

### The measurement, and why it is not a wall-clock number

`TickStats.mspt_avg` from that same harness run swung from 3.6ms to 15.9ms on
one fixture and from 5.8ms to 54.3ms on another, in the same process, with no
code change between iterations — this machine runs several agents' concurrent
`cargo` builds, exactly the unreliability the harness's own module doc already
warns about. Wall-clock is not a fair instrument here.

What is fair: an **allocation count**, which is deterministic regardless of
concurrent load, cross-checked against a control that proves the counting
instrument actually counts (build a one-time `Arc<str>` pool and assert the
allocator saw it: nonzero, as expected — an instrument that always reads zero
would make the "zero" result below worthless). Replaying `raid_farm.litematic`'s
own measured while-active read rate (5899 cell reads, the real per-tick number
above, not a round one) against the real per-family mix of the fixture's 142
redstone components, under a counting global allocator, in a standalone
program independent of `lodestone-server`:

```
OLD (String::to_string() per read):  5899 allocations, 349224 bytes
NEW (Arc<str>::clone() per read):       0 allocations,      0 bytes
```

5899 heap allocations and 349 KB removed per active tick, at the real
production read rate measured on a real downloaded contraption. This is a
result about the operation this change actually replaces (a heap allocation
plus byte copy vs. an atomic increment), scaled by a real, independently
measured multiplier — not a claim about total tick time, which the harness's
own wall-clock caveat above already rules out measuring honestly on this
machine.

---

## How to change it

Adding a family to the dispatch is **three** edits, and the exhaustive gate
fails loudly if you make fewer than all three:

1. a variant on `ReactionClass` (and its `CLASS_NAMES` row, `from_index` row and
   `chain_probes` row — the contiguity gate catches a missed one);
2. an arm in `classify`, **positioned to match the dispatch chain's own order**,
   because the chain is first-match-wins and two overlapping predicates resolve
   by position;
3. the same arm in `reference_class` in `redstone_graph`'s tests, transcribed
   from the dispatch site — a differential whose two arms share a derivation
   proves nothing.

Gotchas:

- **The `Inert` early return is only sound while every unmatched state is a
  no-op.** It is today: `react_to_notification` falls through every arm to an
  empty cascade and mutates nothing. A future arm that does work for *unmatched*
  states would break that, and nothing would go red.
- **A predicate that reads a neighbouring cell cannot be classified per palette
  entry.** See the table above.
- **`chain_probes` is a cost model, not an instrument.** It is not consulted by
  any dispatch decision. If the chain's arm order changes, its rows must change
  with it, and its gate will say so.

---

## What to do next, in order of value

1. **A contraption-scale differential against a live server.** Every reading
   here is either an outside-sourced expected value replayed through the
   production path (the attenuation table, the tripwire scan constants, the
   observer delay) or a counter identity. What none of them covers is a whole
   downloaded contraption ticked side by side against a real 26.2 server for
   many ticks, which is the only instrument that can catch an *ordering*
   divergence rather than a final-state one. `.cache/redstone-benchmarks/`
   holds the fixtures; the harness already replays their captured pending
   ticks through production dispatch.
2. **The remaining reads that are not classified per palette entry.** The
   neighbour-sensitive predicates named in the table above stay inside their
   arms' bodies, so an inert cell adjacent to one still pays for the read.
   Worth measuring before touching: `notifications_by_class` already says how
   many notifications reach each family.
3. **A listener index**, only if 1 and 2 land and counters still show
   notification dispatch dominating. The bar is deliberately high: it is the one
   change here that introduces a defect class this subsystem currently cannot
   have.

---

## Configuration

One cargo feature, `redstone-counters` on `lodestone-server`, default **off**.
It is additive only — atomics plus hook bodies — and enabling it cannot change
any propagation or scheduling decision, only what gets counted. `Snapshot`
returns all zeros without it, which is indistinguishable from "measured and
found nothing", so check the feature is on before trusting a reading.

## Dependencies

`lodestone_data::block_states` for the exhaustive gate's state enumeration; the
family predicates in `lodestone_server::{redstone, redstone_openable,
redstone_rail, redstone_dispenser, redstone_note_block, piston, gravity_tick,
command_block, mobs::tnt}`, which stay where they are and stay the definition.
`crates/lodestone-anvil/tests/redstone_benchmark.rs` and
`.cache/redstone-benchmarks/` for the workload.
