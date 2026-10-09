# Redstone execution model

## What it is

How a redstone change is executed: the neighbour-notification cascade, the scheduled-tick drain, and the palette-derived reaction classification (`lodestone_server::redstone_graph`) that decides which device family, if any, a notification dispatches to. It sits under [`redstone.md`](./redstone.md)'s per-device behaviour.

The model is event-driven with no per-tick neighbour scan, so an idle contraption costs zero. The cost that matters is the constant factor per notification; classification reduces it to an array read and a branch for inert cells.

## How it works

### Entry points and ordering

Redstone work enters from three event-shaped places:

1. `random_tick::propagate_and_react`, once per just-mutated position (random tick, scheduled-tick drain, placement).
2. `tick::run_tick_loop`'s scheduled-tick drain, touching only due entries.
3. Special-shape scheduled bodies: tripwire recheck, gravity settle, piston commits, target decay, dispenser fire.

`propagate_and_react` hands each mutation to `neighbor_update::NeighborPropagator::propagate`, a depth-first cascade over `UPDATE_ORDER` (west, east, down, up, north, south) with a chained-update cap. Each notification lands in `random_tick::react_to_notification`, which dispatches to one family or nothing.

**The ordering machinery is the specification.** `UPDATE_ORDER`, the depth-first interleave, the scheduled queue's `(trigger tick, priority, insertion)` drain order and per-`(pos, kind)` dedup, and dust's deterministic 7-centre by 6-direction fan-out are observable compatibility behaviour. A faster engine that reorders two updates is a regression.

### Typed state

Runtime redstone state is a validated `lodestone_data::block_states::StateId`. Property reads use the generated `PropertyKey` and `BuiltinPropertyValue` tables; mutations resolve through `Properties::state_for_block`. Names appear only at protocol, persistence and legacy-fixture boundaries. Comparator output is a numeric block-entity sidecar, never encoded into a state id. Piston push reactions classify `StateId::block()` against generated `Block` tables. Command-block modes resolve to `lodestone_model::CommandBlockMode` at protocol ingress.

Dispenser projectile selection crosses its text boundary once: `Item::from_name`, then `redstone_dispenser::arrow_entity_type` maps the three arrow variants to a generated `EntityType`. Unknown or plugin items follow the plain-toss fallback. Extend by adding `Item` variants to that typed classifier, not another string arm.

### Reaction classification

`redstone_graph::ReactionClass` has one variant per family plus `Inert`; `classify` maps a canonical state string to its class, mirroring `react_to_notification`'s predicate chain in the same order, first match wins. It is computed once per palette entry: `ChunkColumn` carries `palette_reaction` beside `palette_ticking` and `palette_state_ids`, appended in `ChunkColumn::intern` and rebuilt in `recalc_ticking_counts`, and `ChunkColumn::reaction_class` answers in two array indexes. `react_to_notification` reads the class first, returns immediately for `Inert`, and guards each arm on a class comparison.

Why not a reverse listener index: a stale edge is silently wrong while rediscovery self-heals; suppressing inert notifications at enumeration would move where the chained-update cap trips and change `issued`, whereas deciding at dispatch keeps it byte-identical; and the index would only save the array read classification already costs. The palette is append-only (`intern` is the only writer, `palette` private), so a classification cannot outlive its state and there is no invalidation step. `comemo`/`salsa` do not fit: this is a mutable grid whose recomputation order is observable.

### Scope and invariants

| case | handling |
|---|---|
| update order | preserved: classification changes what a notification costs, never which is issued, in what order, or counted against the cap |
| quasi-connectivity | preserved: it is a property of a piston's read set (`piston::has_extend_signal` reads `pos.above()`, which is not notified). Edges must mirror notification topology, never reads |
| delays and scheduling | preserved: scheduling happens inside arms |
| property-sensitive dispatch | works: a palette entry is a whole state string |
| neighbour-sensitive dispatch | not supported by design; such a test stays inside its arm (`redstone_graph`'s module doc states this as the rule for adding a family) |
| chunk boundaries | closed on every production path (below) |
| unloaded chunks | derived data is dropped on unload and rebuilt from the palette; nothing is persisted |
| piston/slime movability | out of scope: a different relation with different edges |

**Chunk seams.** `random_tick::RedstoneColumns` replaces the single `&ChunkColumn` that reaction dispatch, placement and tripwire arms read and write. Home stays a plain `&mut`; any neighbour a cascade reaches is fetched lazily via `ChunkSource::is_column_resident` (never generated) and cached for that one cascade only. Every production entry point takes a `world: &dyn ChunkSource` built over the real `ChunkStore`: the scheduled-tick drain, target-block hits, falling-block landings, `RandomTickScheduler::tick_chunk`'s fan-out, and `react_at_placement_with_entities`/`react_at_removal` under the placement and break handlers. The single-column form is `#[cfg(test)]`, so "no production cascade is bounded to one column" is compiler-checked.

### Lookup representation

Every `redstone*`, `piston` and `block_support`/`block_placement` query takes a `lookup: F` closure returning `redstone::WorldState` (`Arc<str>`), so a read is a refcount bump instead of a heap allocation. `ChunkColumn` carries a fourth derived table `palette_arc`, so `block_state_arc` answers with one `Arc::clone`; `chunk::air_state_arc()` is a process-wide `LazyLock` for out-of-bounds reads. `Arc<str>` derefs like `String`; call `.to_string()` only where ownership is needed (a world write, a `RandomTickEvent`, a `ScheduledTickQueue<String>`). Measured on the raid-farm replay: 5,899 allocations and 349,224 bytes per active tick avoided, via a counting allocator with a control proving the instrument counts.

## Evidence

### Workload and counters

`crates/lodestone-anvil/tests/redstone_benchmark.rs` stamps a real community contraption onto a flat world, runs the real `IntegratedServer` tick loop and reads `lodestone_server::redstone_counters`. Counters rather than durations: wall clock reproduces at about 10.8% here against 0.16-0.21% for structural counts.

```
cargo test -p lodestone-anvil --test redstone_benchmark -- --ignored --nocapture
```

`raid_farm.litematic` (1,393 blocks, 142 components) with two captured repeater rechecks re-injected:

| phase | notifications | cell_reads | state_parses | signal_queries | wire_recomputes |
|---|---|---|---|---|---|
| steady state | 0 | 0 | 0 | 0 | 0 |
| active | 837 | 5,899 | 55 | 164 | 157 |

The steady-state zero is real: there is no per-tick scan to remove. The active cost is per event (about 41 cell reads per component from two events), with `schedules_requested=3`, `schedules_deduped=15`, `max_notifications_per_drain=726`. These eight counters reproduced across three runs including one under six-way CPU load. `bee-and-crop-farm` is not an identity fixture: its steady-state notifications swing (153 vs 159) from random ticks over crops, always with empty cascades. A hermetic 15-cell dust run lit from one end reads `notifications_issued=659`, `cell_reads=3038`, `reactions_total=155`, `signal_queries=152`, `wire_recomputes=152`.

`Snapshot::notifications_by_class` buckets notifications by landing class at the same hook as `notifications_issued`. On a five-family fixture, 519 of 677 notifications (76.7%) were inert, so classification avoided 8,274 string-predicate probes (12.22 per notification) and 519 state-string clones. These are fixture-shaped, not universal. The counters are process-global and their `TEST_LOCK` covers only their own module: read them with `--test-threads=1` or a narrow filter.

### Exhaustive differential

`classification_agrees_with_the_dispatch_chain_for_every_state_in_the_game` walks every one of the 32,366 block states, rebuilds the canonical `name[k=v,...]` form and requires `classify` to match a second, independent transcription of the dispatch chain. Mismatches are collected and asserted as a set. `the_exhaustive_gate_can_actually_fail` is the control (a deliberately wrong classifier must yield a nonzero count), and `chain_probe_positions_match_the_dispatch_order` pins the cost model to the real arm order.

### Chunk-seam gates

Expected values come from outside this crate: `redstone_oracle_gate`'s `ORACLE_DUST_ATTENUATION` was probed cell by cell on a real server (three readings agreeing exactly), and the seam-invariance rule (the same geometry gives the same result wherever a column boundary falls; for tripwire, the same shape at a shorter legal length). The gates live in `random_tick`'s tests and drive the production entry points:

| gate | entry | predicted |
|---|---|---|
| `a_placed_source_drives_its_dust_run_across_a_chunk_seam_to_the_live_server_profile` | `propagate_placement_with_entities` | power 15 down to 4 across world x=14..25 |
| `breaking_a_tripwire_reaches_the_hook_in_the_next_chunk_and_matches_the_single_column_run` | `propagate_removal_with_entities` | both hooks attached and powered; one recheck 10 ticks out |
| `a_random_tick_mutation_notifies_the_observer_across_a_chunk_seam` | `RandomTickScheduler::tick_chunk` | exactly `[((16, 1, 8), "redstone:observer", 4175)]` |

Coordinates make the models disagree at the first cell past the seam: at x=16 the oracle gives 13, a truncating cascade 0, and a restart-at-full-strength cascade 15. The observer gate uses an unround `current_tick` of 4173 so a wrong scheduling model cannot land on 4175. Controls: substituting the single-column source at the production call sites makes all three fail at the cross-seam assertion while single-column reference arms still pass; and each gate re-run with the far chunk seeded but declared non-resident must show the old truncation (`TestWorld` has a seeded-but-unloaded split for this).

### Live contraption timeline

`crates/lodestone-fuzz` (see [`fuzzing.md`](./fuzzing.md) Track B) drives `RedstoneModelOracle`, which calls `react_at_placement_with_entities` and `block_tick_reaction::run_due_block_tick` (a module precisely so chains of delayed components are testable across ticks). A 20-cell row (source, dust, repeaters at delay 1, 4, 2) crossing a seam at the first hop and cell 17 measured on the live server three times: cell 1 power 15 on tick 0, cell 16 power 8 on tick 10, cell 18 power 15 on tick 14. The model matches tick by tick, `tests/redstone_contraption_ticks.rs` pins it without a server, and a no-cross-column model is caught on tick 0 at cell 1. Limit: the live read primitive answers only for positions whose possible states the caller enumerates, so widening a contraption means predicting each new cell's states.

## How to change it

Adding a family is three edits; the exhaustive gate fails if you make fewer:

1. A variant on `ReactionClass`, plus its `CLASS_NAMES`, `from_index` and `chain_probes` rows (the contiguity gate catches a miss).
2. An arm in `classify`, positioned to match the dispatch chain's order (first match wins).
3. The same arm in `reference_class` in `redstone_graph`'s tests, transcribed from the dispatch site; a differential whose arms share a derivation proves nothing.

Gotchas:

- The `Inert` early return is sound only while every unmatched state is a no-op. A future arm that acts on unmatched states breaks it with nothing going red.
- A predicate reading a neighbouring cell cannot be classified per palette entry.
- `chain_probes` is a cost model, consulted by no dispatch decision; update it with the chain order.

Open work is in [`plans/redstone-execution-model.md`](./plans/redstone-execution-model.md): a larger contraption in the live differential (comparators, observers, piston phases), measuring the neighbour-sensitive reads that stay in arms, and a listener index only if counters show dispatch dominating.

## Configuration

One cargo feature, `redstone-counters` on `lodestone-server`, default off. It only adds atomics and hook bodies and cannot change a decision. Without it `Snapshot` returns zeros, indistinguishable from "measured nothing", so confirm it is on.

## Dependencies

`lodestone_data::block_states` (state enumeration); the family predicates in `lodestone_server::{redstone, redstone_openable, redstone_rail, redstone_dispenser, redstone_note_block, piston, gravity_tick, command_block, mobs::tnt}`, which remain the definition; `crates/lodestone-anvil/tests/redstone_benchmark.rs` and `.cache/redstone-benchmarks/` for the workload.
