# Redstone execution model: typed cells, kind-classified dispatch, and a bounded graph

## What it is

The plan for issue #548's rework of how redstone executes: replacing per-event rediscovery
(string-parsed block states, a fifteen-predicate dispatch chain, blind neighbour visits) with a
layered design — a typed cell representation, palette-derived reaction classification, a
cross-column world view, and, **only if counters then justify it**, an incrementally-invalidated
listener index. It is written against the device set that landed first (`docs/redstone.md`'s
"what each device needs of the execution model" table) rather than against an imagined redstone,
and its first finding is that the issue's own premise needs correcting: **there is no per-tick
rescan to replace**. The expensive thing is the per-event constant factor, and the missing thing
is cross-chunk propagation — not incrementality, which the current model already has.

## Status

**Layer B has landed, and it absorbed the part of Layer A that mattered.**
`lodestone_server::redstone_graph` is the palette-derived reaction classification (§3.2's Layer
B); `ChunkColumn::palette_reaction`/`reaction_class` is its per-column table, the third beside
`palette_ticking` and `palette_state_ids`; `random_tick::react_to_notification` now reads the
class first, returns immediately for `Inert`, and guards every arm on a class comparison instead
of a string predicate. The fifteen-predicate chain and the unconditional per-notification
block-state `String` clone are both gone. `redstone_counters::Snapshot::notifications_by_class`
is the instrument. Landed behaviour, measurements and the design's fallback cases are in
[`../redstone-execution.md`](../redstone-execution.md).

Still open, in the order that doc argues for:

- **Layer A's remaining half — `redstone::make_lookup`'s per-read `String` allocation.** Not
  done: it is a signature change across ~60 call sites reaching outside the redstone family
  (`fire.rs`, `fluid.rs`, `block_placement.rs`, `server.rs`), and it is now the largest remaining
  constant factor by a wide margin — 5899 allocating reads against 837 notifications on
  `raid_farm.litematic`'s active tick.
- **Layer C, the cross-column seam.** Untouched, and still the largest *correctness* gap.
- **§3.3's `WritePlan` unification (U4).** Untouched.
- **Layer D, the listener index (U7).** Deliberately not built; §8's argument stands and
  `../redstone-execution.md` records why the classification captures its win without its
  staleness class.

§9's first open question ("the actual cost split") is answered there against a real contraption
rather than the 15-cell proxy.

---

## 1. What the current model actually is

Measured by reading `random_tick.rs`, `tick.rs`, `scheduled_tick.rs`, `neighbor_update.rs` and
`redstone*.rs` as they are today, not by restating the issue.

### 1.1 It is already event-driven; an idle circuit costs zero

Redstone work runs from exactly three production entry points, all event-shaped:

1. `random_tick::propagate_and_react` — called once per just-mutated position, from a random tick
   that changed a block, from the scheduled-tick drain, and from placement
   (`server::propagate_placement` → `random_tick::react_at_placement`).
2. `tick::run_tick_loop`'s `block_ticks.drain_due(game_tick, MAX_SCHEDULED_TICKS_PER_TICK)` —
   processes only entries whose trigger tick is due, in `DRAIN_ORDER` (tick, priority, insertion).
3. The special-shape scheduled bodies beside it: `random_tick::run_tripwire_recheck`,
   `random_tick::settle_gravity_at`, the piston `finish_kind` commits, target decay, dispenser fire.

Nothing scans the world for redstone per tick. The random-tick pass classifies sections by a
palette mask compared as integers (`ChunkColumn`'s randomly-ticking classification), and the
drains touch only due entries. **A lit contraption at rest therefore already recomputes nothing**
— issue #548's proposed "null contraption" control passes today by construction. That control is
still worth building (§6), but as a regression tripwire, not as the win.

"Per-tick rescan" in the issue is really **per-event rediscovery**: every reaction re-derives who
it is, what its neighbours are, and what signal touches it, from scratch, through strings.

### 1.2 The per-event cost anatomy

For one neighbour notification reaching `random_tick::react_to_notification`:

- **Dispatch**: a chain of ~15 sequential family predicates, each a `base_name` split plus string
  compare, run against `column.block_state(..).to_string()` — one heap allocation per
  notification before anything is decided.
- **Cell reads**: `redstone::make_lookup` returns `impl Fn(BlockPos) -> String` and allocates a
  fresh `String` per read. `best_neighbor_signal` reads six neighbours; `signal_at`'s
  conductor-wrap reads six more per conductor; a comparator arm calls `input_signal` *and*
  `alternate_signal`; every property access (`get_u32_property` and friends) re-parses the
  bracket string.
- **Dust fan-out**: a wire power change fans out `wire_update_centres` — 7 centres × 6 directions
  = 42 notifications, duplicates included (vanilla-faithful), each paying the full dispatch and
  read cost above. A cascading run of N changed cells is O(N × 42) notifications.

The issue's own suspicion — "if string parsing is the bulk of the cost, the graph is optimising
the wrong layer" — is almost certainly right, and §6's counters exist to confirm it with a number
before anything structural is built.

### 1.3 The whole family is confined to one chunk column, silently

`react_to_notification` returns an empty cascade for any position outside the 16×16 column it was
handed; `make_lookup` answers `minecraft:air` for the same; the piston arm explicitly skips
out-of-column writes *and* their commits ("the same border limit the module doc already records
for the whole redstone family"); `tick.rs`'s drain fetches one `ChunkSource::column` per due
entry. **A circuit crossing a chunk border truncates with no error today.** This is the single
largest gap between the current model and anything a community contraption needs, and it is a
*correctness* gap, not a performance one. Any graph designed before this is fixed would be
designed against a 16×16 toy world — which is why the cross-column seam is a layer of this plan
(§3, Layer C) rather than a footnote.

### 1.4 The ordering machinery is already the specification, and it is good

- `neighbor_update::UPDATE_ORDER` (west, east, down, up, north, south) and
  `NeighborPropagator::propagate`'s depth-first cascade are direct ports of vanilla's
  `CollectingNeighborUpdater`, with the chained-update cap.
- `scheduled_tick::ScheduledTickQueue` reproduces `DRAIN_ORDER` (trigger tick, then priority,
  then insertion order) and the per-`(pos, kind)` dedup, as the documented single-container
  reduction of vanilla's per-chunk `LevelChunkTicks` — its own doc names the promotion path if a
  per-chunk registry ever exists.
- Dust's 42-notification set is ordered deterministically (centres in `[pos] ++ UPDATE_ORDER`,
  directions in `UPDATE_ORDER` within each) precisely because vanilla's own iteration order there
  is unspecified.

None of this changes. **The graph decides who to enqueue, never in what order to run** — the
issue's own seam ("memoize the strength lattice, keep the update queue's ordering semantics
untouched") is correct and this plan keeps it.

### 1.5 The dispatch shape, and where it already broke

Most families react through the single-cell arm shape: read, decide, either write one cell
(`Option`-of-new-state) or schedule one tick. Two families could not fit it and grew their own
entry points: the piston's two-phase `begin_move` plan (moving cells + cleared cells + base write
+ per-cell scheduled commits) and tripwire's `CalculatedState` multi-position write plan with
`run_tripwire_recheck` in the drain. `docs/redstone.md` records this as a structural finding:
the dispatch is provably too narrow for multi-cell writes. §3.3 makes the plan-shaped return the
norm rather than the exception.

### 1.6 One stale claim corrected before it misleads

`docs/redstone.md` still says every dust change is delivered to the client as `power=0`
(`resolve_state_id` exact-property-set matching). **That is fixed**: `resolve_state_id` now
delegates to `lodestone_data::block_states::state_id`, which has a subset tier plus a
default-state-overlay fallback, and `the_powered_run_reaches_the_client` is a live, passing gate.
The rework does not sit on top of a wire carrying wrong values.

---

## 2. What the devices require of the execution model

Distilled from `docs/redstone.md`'s per-device table, which is the requirements document:

| requirement | evidence |
|---|---|
| **Multi-cell atomic write plans** | piston `begin_move`; tripwire `CalculatedState` (two hooks plus every wire cell between, up to 41 cells) |
| **Triggers that are not neighbour notifications** | tripwire has *no* `neighborChanged` at all (placement + self-scheduled 10-tick poll); rails owe themselves a placement reaction; target fires from an external projectile-hit event |
| **Non-uniform propagation shapes** | rails notify `pos.below()` always and `pos.above()` only on a slope — not a six-direction fan-out; dust's 7-centre set; lever's two-layer `updateNeighbours` |
| **Cross-device state reads at range** | rail chains read up to 8 cells through *other rails'* `POWERED`; tripwire hook scans 41 cells live per recheck |
| **Read set ≠ notify set** | piston quasi-connectivity: `has_extend_signal` reads `pos.above()` but vanilla never *notifies* the piston when that cell changes — the difference **is** the BUD quirk |
| **Scheduled-tick variety** | one-shot rising-edge (dispenser, 4t), suppress-while-pending (target, 20/8t), conditional reschedule (tripwire), delay-and-priority diodes (`repeater_schedule_priority`) |

The last row before the scheduled-tick one is the sharpest design constraint in this whole plan,
and it decides §3.4.

---

## 3. The design

### 3.1 The framing that governs everything

Vanilla is the oracle for observable behaviour, not for the implementation. The correctness bar
is **observational equivalence with vanilla's update order, quirks included** — quasi-connectivity,
depth-first cascade interleaving, `DRAIN_ORDER` tie-breaks, the dust fan-out's duplicate
notifications. A faster engine that reorders two updates is a regression. Every layer below is
chosen so that it is *incapable* of changing order: the layers optimise what a notification
costs and who gets enqueued, never the sequence in which enqueued work runs.

### 3.2 The decision: three unconditional layers, one conditional one

**Layer A — the typed cell layer (kill the string tax).** A `RedstoneCell` (or `ParsedState`)
value: a `Kind` enum (Wire, Torch, Repeater, Comparator, Observer, Piston, Hopper, Openable,
NoteBlock, Rail, Dispenser, Tripwire, TripwireHook, Target, InputSource(..), Conductor, Inert)
plus a small packed property word (power 0–15, powered, facing, delay, locked, mode, shape —
none of the modelled families needs more than ~16 bits beyond the kind). Decoded **once per
palette entry** and cached on the column, exactly the way the randomly-ticking classification
already piggybacks palette appends — so the per-cell question "what are you" becomes an index
into a per-column table, invalidated by construction when the palette grows. The `lookup`
closure's type changes from `Fn(BlockPos) -> String` to `Fn(BlockPos) -> CellRef` (a `Copy`
value), and `get_u32_property`/`get_bool_property`/`get_str_property` calls inside the pure
modules become field reads. Writes still go through canonical state strings at the
`ChunkColumn::set_block` boundary — the encoder and persistence layers are untouched.

**Layer B — kind-classified dispatch.** `react_to_notification`'s predicate chain becomes a
`match` on the notified cell's `Kind`, read from the Layer-A table. A notification landing on an
`Inert`/`Conductor` cell costs one table read and a jump — the same asymptotic win a reverse
index would buy per notification, with **zero staleness risk**, because the classification is
derived from the palette rather than maintained beside it. The string predicates are kept as the
debug-build reference arm, the precedent being `randomly_ticking_palette_mask`: the validated
definition stays, `cfg`-gated, as the tripwire the fast path is checked against.

**Layer C — the cross-column seam.** Replace the single-column `make_lookup`/`react_to_notification`
world view with a bounded multi-column view (a 3×3 of columns around the reaction origin covers
every modelled read: dust's diagonal second layer, rail's 8-cell chain, tripwire's 41-cell span —
all within 41 blocks of the origin, and reactions triggered near a border reach at most one
column over). Writes landing in a neighbouring column go through the same `ChunkSource::set_block`
path; the drain already fetches columns per due entry. This also forces the `ScheduledTickQueue`
question its own doc anticipates: with reactions crossing columns, promote to per-chunk tick
containers plus the `LevelTicks` cross-container merge, or document why the single container's
`DRAIN_ORDER` is still faithful. Layer C is a correctness feature the devices need regardless of
any performance work, and it is the prerequisite that makes contraption-scale benchmarks
*possible* — today no community contraption can even exist in this world.

**Layer D — the listener index (conditional).** A per-column reverse index: for each cell, the
list of `(listener_pos, kind)` that vanilla's notification topology would reach when that cell
changes, stored pre-sorted in fan-out order, with a small border-edge registry for listeners one
column over. On a write, the executor enqueues listeners directly instead of enumerating
neighbours. **Built only if the counters after A–C show notification enumeration still
dominating** (§6 and §8 give the stop rule). The honest expectation is that it will not: after
Layer B, visiting an inert neighbour costs a table read, and the enumeration of 6 (or 42)
targets is itself the order-preserving act — skipping it saves very little and creates the one
defect class this subsystem currently cannot have (a stale edge, silently wrong rather than
slow). If it is built: edges derive **only from notification topology** (see §3.4), the index is
derived data (dropped on unload, rebuilt on load, never persisted), and a debug tripwire
compares every index-driven enqueue set against the rediscovery answer.

### 3.3 The unit of invalidation is the write plan, not the cell

Every reaction's output becomes one shape — a `WritePlan`: an ordered list of `(pos, new_state)`
writes, a list of scheduled ticks `(pos, kind, delay, priority)`, and a fan-out policy per write
(none / single `updateNeighborsAt` / dust's 7-centre set / rail's conditional extras). The
executor — today `propagate_and_react` plus the drain arms, unchanged in ordering — applies
writes in plan order, publishes events, schedules ticks, and fans out per policy. This:

- makes piston and tripwire the norm instead of exceptions (their entry points already produce
  exactly this data: `begin_move`'s moving/cleared/base triple, `CalculatedState`);
- gives Layer D, if built, its atomic invalidation unit ("one recomputation writes N nodes" —
  the edge type `docs/redstone.md` says the rework needs);
- collapses the single-cell families mechanically: `Option<new_state>` wraps into a one-write
  plan with the family's existing fan-out behaviour named rather than implied.

### 3.4 Node granularity, and what invalidates what

**A node is a cell position.** The wire-run supernode (one node per connected dust network) is
rejected for now, for three reasons stated so they can be re-argued with evidence later: each
dust cell's `power=N` is client-observable state that must be computed and delivered per cell
anyway; the order in which cells of a run settle interleaves with neighbour reactions and is
part of the observable cascade; and the win it targets (redundant recomputes within a run) is
already bounded by the fan-out's change-gating (`new_power != old_power` returns an empty
cascade). Revisit only with an oracle-gated order-sensitive circuit corpus in place (§5, U0).

**Edges mirror notification topology, never read sets.** This is the load-bearing sentence of
the whole design. A piston *reads* `pos.above()` (quasi-connectivity) but vanilla never
*notifies* it when that cell changes — the difference between the read set and the notify set is
precisely the BUD quirk, so an index built from "what does this node read" destroys
quasi-connectivity while looking more thorough. The issue's directionality insight (a repeater
pointing away is not invalidated by downstream changes) lands soundly one level up: the repeater
*is* notified in vanilla (it is a neighbour), and its reaction then reads only input/side/support
faces and schedules nothing. Face-sensitivity is therefore a per-kind pruning of *reactions*
(the observer's `n.from == watch` check is the existing example), not of notifications — safe to
add per family, verified by the differential ratchet (§6), and worth having whether or not
Layer D exists.

### 3.5 A memo crate is rejected

`comemo` and `salsa` memoize pure functions over tracked immutable inputs; redstone is a mutable
spatial grid whose observable behaviour includes the side-effect *order* of recomputation. The
hot cost here is not recomputation of a pure value (the strength lattice is cheap once reads are
cheap) but discovery and parsing, which Layers A–B remove without a dependency. A framework
dependency that shapes the whole subsystem would also have to earn its place in the wasm32
bundle `lodestone-server` links into, under this repo's clock/thread confinement rules. A
hand-rolled dirty-set over an explicit adjacency index (Layer D's shape) is strictly simpler and
keeps ordering in this crate's hands. This answers the issue's "evaluate, do not assume" with:
evaluated against the access pattern, and no.

### 3.6 Scope: per-column, per-dimension by ownership; global rejected

Indexes and cell tables are per-`ChunkColumn`, because everything about their lifecycle already
is: the ticked area follows the player (`tick_area::FollowArea`), columns enter and leave it,
only the overworld has a tick loop today and a second dimension would own its own loop and
therefore its own indexes. A global graph is a shared mutable structure that fights the
`&mut ChunkColumn` borrow story, the concurrent-agent file model, and unload semantics, for no
benefit a border-edge registry does not provide.

### 3.7 Chunk boundaries and unload

- **Boundary**: Layer C's 3×3 view answers reads; writes cross through `ChunkSource`; Layer D's
  border registry (if built) holds edges whose listener is in another column, keyed by the
  column pair and rebuilt whenever either side's index rebuilds.
- **Unload**: derived data only. A column unloading drops its cell table and index; nothing is
  persisted; a listener in an unloaded column is simply not enqueued — matching vanilla's "no
  tickets, no ticking" and the current follow-area behaviour. On reload, tables rebuild from the
  palette (cheap, per-entry) and the index (if any) rebuilds from cell kinds (bounded by column
  size). A circuit straddling the ticked-area edge behaves as vanilla's does at an unloaded
  border: the loaded half sees the unloaded half as absent. That is a semantic *choice* to gate
  against the real server (vanilla's border behaviour is itself observable), recorded in §9.

---

## 4. Migration path per device

The named seam — pure per-family decision functions reading the world through a lookup closure,
tested hermetically with fake worlds — **is the right cut and survives intact**. What changes is
the lookup's type and the dispatch around the seam, not the decisions inside it.

| device family | changes | does not change |
|---|---|---|
| all pure modules (`redstone.rs`, `redstone_wire/torch/diode/observer/openable/rail/note_block/dispenser/target.rs`) | lookup parameter type `Fn(BlockPos) -> String` → `Fn(BlockPos) -> CellRef`; property parsing → field reads (mechanical, per file, parallelisable) | every decision function's logic, name, tests' structure, jar citations |
| dust | fan-out policy named in its `WritePlan` (7-centre) instead of special-cased in `propagate_and_react` | `calculate_target_strength`, the 42-notification order |
| torch/repeater/comparator/observer | arm returns a schedule-only `WritePlan`; face-sensitivity mask added per kind | delays, priorities, `should_schedule_*` predicates |
| piston | `begin_move`'s output *is* a `WritePlan` — the adapter around it shrinks; out-of-column skip replaced by Layer C writes | `resolve`, `apply_move`, quasi-connectivity, two-phase commit kinds |
| tripwire | `CalculatedState` becomes a `WritePlan`; `run_tripwire_recheck` stays a scheduled body (its trigger is a poll, not a notification — the graph never subsumes it) | the scan/attach algorithm, the 10-tick recheck |
| rail | `extra_notifications` becomes the plan's fan-out policy | chain search, one-class-two-ids fact |
| dispenser/target/hopper/openable/note block | mechanical `WritePlan` wrap | everything else |
| oracle gates (`redstone_oracle_gate.rs`, `redstone_diode_oracle_gate.rs`, `redstone_placement_gate.rs`) | none — they drive `propagate_and_react`/placement, whose observable outputs must be byte-identical | their role: they are the migration's proof, run at every layer boundary |

No device is rewritten. Layer A is a type-threading pass per file; Layer B moves the dispatch;
§3.3 rewraps returns. The two devices that broke the old shape become the template for the new
one.

---

## 5. Decomposition into dispatchable units

File contention is the binding constraint: `random_tick.rs`, `tick.rs` and `server.rs` are choke
points; the per-family `redstone_*.rs` files and new files are free.

| unit | what | files | concurrency |
|---|---|---|---|
| **U0** | Order-sensitive oracle corpus: T-junction, repeater-locked latch, observer chain, a BUD rig — captured against the live 26.2 oracle *before* any rework, since current oracle coverage (dust attenuation, diode tables) does not pin ordering. `docs/redstone.md` already names this as the strongest missing evidence. | new `crates/lodestone-server/src/redstone_order_oracle_gate.rs` | fully concurrent with everything |
| **U1** | Redstone counters (feature-gated, thread-local per the harness's own lessons) + the null-contraption control + hand-derived predictions for one fixture | new `redstone_counters.rs`; minimal hooks in `redstone.rs` + one hook point in `random_tick.rs` (broker that single edit) | concurrent with U0 |
| **U2a** | `RedstoneCell`/`Kind` core type + palette-table plumbing | new `redstone_cell.rs`; `chunk.rs` (classification hook beside the randomly-ticking one) | after U1's baseline is recorded |
| **U2b–2k** | Per-family lookup-type conversion, one agent per `redstone_*.rs` file | each file its own cluster | **parallel with each other** once U2a lands |
| **U3** | Dispatch swap: `react_to_notification` predicate chain → kind match, string arm kept as debug reference | `random_tick.rs` | single agent; serialises after all U2 |
| **U4** | `WritePlan` unification (single-cell wrap, piston/tripwire adapters, fan-out policies) | `random_tick.rs`, `tick.rs` | single agent; serialises after U3 |
| **U5** | Cross-column seam: 3×3 view, cross-column writes, scheduled-tick container decision, border oracle gate | `random_tick.rs`, `tick.rs`, `chunk.rs`/`chunk_store.rs`, `server.rs` (placement path) | single agent; the big one; after U4 |
| **U6** | Contraption benchmark suite over the harness (`docs/benchmark-harness.md` patterns; this is the sixth `support.rs` site — promote it to a shared crate per that doc's own threshold, or take the seventh copy consciously) | new `crates/lodestone-server/benches/redstone.rs` | concurrent once U1 exists; scales up after U5 |
| **U7** | *(conditional on U1/U6 numbers after U5)* listener index + border registry + staleness tripwire | new `redstone_graph.rs`; integration in `random_tick.rs` | last, single agent, only if §8's bar is met |

Three lanes run concurrently at the start (U0, U1, U6-scaffolding); the U2 fan-out is the wide
parallel phase; U3→U4→U5 serialise on the choke files with one agent each. `server.rs` is
touched only in U5, once.

---

## 6. What to measure, and how

Counters, not durations — wall clock reproduces at ~10.8% here, instruction counts at 0.16–0.21%,
and the quantities this design tries to reduce are *counts* by nature.

**The counters** (feature-gated like `gen-counters`, thread-local `Cell`s, own test binary):

- `notifications_issued`, `reactions_dispatched` (by kind), `cell_reads`, `state_parses`
  (Layer A drives this to ~0 on the hot path), `wire_recomputes`, `signal_queries`,
  `schedules_requested` / `schedules_deduped`, and — the latency counter — `max_notifications_per_drain`
  (how much work sits inside one unserviced window; the keep-alive incident's lesson applied
  here, since a worst-case piston-clock cascade runs inside one tick's drain).
- With Layer D only: `edges_touched`, `listeners_enqueued`, `index_rebuild_cells`.
- The bench binary adds a counting allocator so `string_allocs` per event is attributed, not guessed.

**Validating the instrument before optimising the system** (each is an input that cannot
physically affect the quantity, the camera-rotation discriminator applied here):

- *Null contraption*: a lit circuit at rest across 100 ticks — every counter above must read 0.
  This is a tripwire for accidental de-incrementalisation, since it already holds today.
- *Inert placement at distance*: placing stone 20 blocks from the contraption must move no
  contraption counter.
- *Toggle symmetry*: lever on, settle, lever off, settle — all world state returns to baseline
  and the on/off counter totals match the hand-derived prediction.
- Predictions are derived in a separate script, never round numbers: e.g. for one lever toggle
  feeding a 15-dust run, predict `notifications_issued` exactly from the 7-centre × 6 arithmetic
  and the per-cell change count before running it. A counter that cannot predict cannot gate.

**The correctness instruments:**

- U0's order-sensitive oracle gates are the truth source at every layer boundary — expected
  values originate outside our code.
- A **differential ratchet** for the migration only: drive old and new dispatch over the fixture
  corpus and assert identical event sequences and schedule calls. This compares two things we
  control, so it is a migration tool, never the oracle; both arms in one process, interleaved.
- Layer D's debug tripwire: index-driven enqueue set == rediscovery answer, per write, in debug
  builds and the differential corpus.

**What the harness records** (via `support::record`, JSONL): per-scene counter totals and, as a
recorded-baseline-only duration, per-toggle settle cost — with the counter beside it so the
ratio is machine-independent. A counter run and a timing run are two runs, never one.

---

## 7. What this deliberately does not build

- **A memo/incremental-computation dependency** (`comemo`, `salsa`) — §3.5.
- **Wire-run supernodes / circuit compilation** — the issue's "index a whole contraption so it
  knows after tick 1 to enable xyz" is a redstone compiler (MCHPRS territory). It abandons
  observational order equivalence by design; not in this codebase's contract.
- **The piston/slime movability index** — a different graph over a different relation, named by
  the issue itself as separate. `piston::resolve` computes runs per event; slime/honey are not
  modelled yet; nothing to index.
- **Persisting any index or cell table** — derived data only; persistence adds a staleness class
  for zero measured need.
- **Per-face signal caching inside cells** — until a counter shows recomputation (not discovery)
  dominating, cached signal values are stored bugs waiting for an invalidation miss.
- **A global or cross-dimension graph** — §3.6.
- **Entity-dependent producers** (pressure plates, detector rail carts, tripwire crossing) —
  execution-model-independent gaps with their own prerequisites, per `docs/redstone.md`.

---

## 8. The risk of the rework being a net loss, honestly

**The full dependency graph, as imagined in the issue, is probably not worth building — and the
plan is shaped so that finding this out costs one instrumentation unit, not a rework.**

- The headline motivation is partially moot: incrementality at the tick level already exists;
  idle cost is already zero (§1.1).
- The dominant costs are almost certainly the string layer and blind dispatch — both removable
  by Layers A–B with **zero** new invalidation state, hence zero new ways to be silently wrong.
  After Layer B, a notification to an inert cell costs a table read; the remaining enumeration
  of 6–42 targets is itself what preserves vanilla's order.
- What Layer D saves beyond that is small; what it risks is the worst defect class available:
  rediscovery is self-healing every event, an index is wrong until someone notices. Every
  incident record in this repo about stale derived state argues for buying the index only with
  evidence.
- The genuinely missing thing for "large community contraptions" is not speed but **existence**:
  cross-column propagation (Layer C) and the unfinished piston physics. A benchmark suite worth
  believing cannot even be loaded before those.

**The stop rule, concretely**: after U5, run U6's contraption scenes. If
`notifications_issued × post-B per-notification cost` (attributed by U1's counters) is under
~10% of the redstone share of a tick on the largest scene that fits the ticked area, U7 is not
built, and this plan's record is amended to say so with the numbers. If dust-network settle cost
(wire_recomputes) dominates instead, the next candidate is wire-run batching *behind the oracle
corpus*, not a listener index — a different follow-up than #548 imagines, which is exactly why
the counters come first.

The layers that are unconditionally worth it: A (removes a per-event tax every future device
also pays), B (asymptotic dispatch win, correct by construction, precedented in this very file),
C (a correctness feature the device set needs regardless), U0 (evidence the whole family is
currently missing). That is the recommended #548: **less graph, more floor.**

---

## 9. Open questions that need running code or the oracle

- ~~The actual cost split (parse vs. read vs. enumerate vs. queue)~~ — **answered, partially.**
  U1's counters (`redstone_counters.rs`) were already landed by an earlier session; this pass
  used them for the first time against a "large" fixture rather than the single-cell one —
  `measured_cost_split_for_a_fifteen_cell_dust_run`, a 15-long dust run lit from one end
  (`MAX_PUSH_DEPTH`-scale, chosen as the largest single number this family already names, until
  U6's real contraption corpus exists). Isolated reading (see the concurrency caveat below):
  `notifications_issued=659, cell_reads=3038, reactions_total=155, signal_queries=152,
  wire_recomputes=152, state_parses=0, schedules_requested=0, schedules_deduped=0` — i.e.
  **4.61 cell reads per notification**, matching §1.2's "six neighbours plus per-conductor reads"
  hypothesis in shape (a small constant, not growing with run length), and **zero
  `state_parses`**, which is a genuine surprise the hypothesis did not predict: `own_signal` (the
  hook point) is apparently not on the hot path a dust settle actually takes for this fixture,
  which is worth a follow-up before trusting Layer A's sizing without also checking *which*
  parse site would shrink. This is deliberately **not a gate** (see the test's own doc comment):
  the single-cell test above it already shows the naive prediction at this scale is exactly
  `CLAUDE.md`'s "do not predict the round number" trap (a first attempt predicted
  `wire_recomputes == 15` and measured 145 for the *one-cell* case, because one settle's own
  7-centre fan-out compounds at every step a longer run advances), so this records real numbers
  and asserts only invariants that hold regardless of the exact compounding.

  **The counters are process-global and the isolation guard does not cover the whole binary.**
  Running this measurement under `--test-threads=2` alongside an unrelated test in a *different*
  module (`random_tick::tests`, which also calls `propagate_and_react` and therefore also bumps
  `notifications_issued`/`cell_reads` through the same static atomics) produced `659`, `667` and
  `674` across three runs — genuine contamination, not flakiness in the redstone logic: rerunning
  the identical fixture alone (`--test-threads=1`, or filtered to just this one test) reproduces
  `659`/`3038` exactly every time. `redstone_counters.rs`'s own `TEST_LOCK` only serialises tests
  *within its own module* against each other; it does not (and structurally cannot, being
  module-private) protect against any other test module in the crate that also drives
  `propagate_and_react` while the `redstone-counters` feature is on. This is the same species of
  hazard `CLAUDE.md` already names for `docs/README.md` drift and the docs-index gate — a guard
  that covers less than its own name suggests — and it means **any future counters-based
  measurement must be run either alone or with `--test-threads=1`**, or the reading is a
  hypothesis about contamination, not about redstone. Not mechanically fixed here (it would mean
  either widening the lock crate-wide or moving every counter-touching test under one module),
  but recorded so the next reading is not silently wrong the same way. §6's other counters
  (`wire_recomputes`, `signal_queries`, `reactions_total`, `schedules_*`) were **not** affected in
  this instance only because the contaminating test (a piston fixture) never touches the dust/
  torch/repeater/comparator/observer hook points — a coincidence of *which* test happened to run
  concurrently, not a property of the guard.

  **`TickClock` (`tick.rs`, issue #548's other named instrument) was checked and found
  unusable for this without editing off-limits files.** Its `ScheduledAndPhysics` phase bucket
  sits outside `scheduled.with`'s closure by construction (see its own doc), so it times fire,
  fluid, random ticks, falling blocks, vehicles, TNT, minecarts and dragons in the same bucket as
  redstone — not attributable to redstone specifically in a real server. More immediately: there
  is **no query path** exposed anywhere outside `tick.rs` itself (`phase_stats`/
  `worst_phase_window` have zero call sites in the rest of the crate), so reading it from a live
  server would mean wiring a new RCON/console command through `server.rs`/`commands/`, both
  off-limits to this pass. The U1 counters above were the reachable instrument and are also the
  more precise one (structural counts, not coarse wall-clock time sharing a bucket with five other
  systems) — consistent with §6's own preference for counters over durations.
- Vanilla's exact behaviour when a neighbour update crosses into an unloaded chunk (drop?
  defer?) — read `CollectingNeighborUpdater`/chunk-ticket sources, then gate at a real border
  with the live oracle before freezing Layer C's border semantics.
- Whether cross-column drains require per-chunk tick containers to keep `DRAIN_ORDER` faithful
  when two columns' due ticks interleave — `scheduled_tick.rs`'s own doc names the promotion;
  the question is whether the single container's global insertion order already matches.
- Whether the packed property word covers every modelled family without a second word (audit
  during U2a against each family's property reads; tripwire's `attached`/`disarmed` and rail
  shapes are the widest).
- Whether `has_scheduled` dedup checks interact with face-sensitivity masks anywhere a vanilla
  re-schedule would carry a different priority (repeater lock edges) — needs a targeted oracle
  measurement, not inspection.
