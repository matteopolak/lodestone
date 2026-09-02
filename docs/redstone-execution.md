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
| **Chunk boundaries** | **Unchanged, and still the largest real gap.** `react_to_notification` returns an empty cascade outside the 16×16 column it was handed, and `redstone::make_lookup` answers `minecraft:air` there. A circuit crossing a chunk border truncates silently today. That is a correctness gap this work does not close and does not worsen; it is the prerequisite for any contraption-scale benchmark that is larger than one column. |
| **Unloaded chunks** | Unchanged. The table is derived data: a column unloading drops it, a reload rebuilds it from the palette. Nothing is persisted, so there is no reload staleness. |
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

1. **Cross-column propagation.** The single largest gap between this model and
   anything a community contraption needs, and it is a *correctness* gap, not a
   performance one — a circuit crossing a chunk border truncates with no error.
   Everything below is less valuable than this.
2. **`redstone::make_lookup`'s allocation.** It returns
   `impl Fn(BlockPos) -> String` and heap-allocates a fresh `String` per read;
   `ChunkColumn::block_state` already returns `&str`, so the allocation is pure
   waste. 5899 of them in one active tick on `raid_farm`, against 837
   notifications — this is now the largest remaining constant factor by a wide
   margin. It is a signature change across roughly 60 call sites spanning
   `redstone*.rs`, `piston.rs`, `fire.rs`, `fluid.rs`, `block_placement.rs` and
   `server.rs`, which is why it is not folded into this change.
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
