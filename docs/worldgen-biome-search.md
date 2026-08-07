# Worldgen biome search

## What it is

The climate → biome lookup layer: vanilla's `Climate.RTree` ported as a real search structure — and, since
the owner's ruling on #492, as the *definition of the answer* — plus a per-source-chunk memo, replacing an
uncached brute-force scan of the 7,594-row overworld climate table. Unit 9 of
[`docs/plans/worldgen-rewrite.md`](plans/worldgen-rewrite.md), the fix for its diagnostic **D5**. Measured
over a 6×6 sweep of real embedded data: **235,991,144 climate comparisons → 278,298**, an 848× reduction.

Code: [`crates/lodestone-worldgen/src/biome/`](../crates/lodestone-worldgen/src/biome/) — `mod.rs` (table,
quantization, brute-force reference, `BiomeTable`), `tree.rs` (the port), `memo.rs` (the memo). Driven from
[`crates/lodestone-worldgen/src/overworld/biome.rs`](../crates/lodestone-worldgen/src/overworld/biome.rs).

## The defect, and why no test could see it

`carve_stage` resolves a carver biome for every chunk in a **17×17 = 289**-chunk source neighbourhood
(`carver::NEIGHBOURHOOD_RANGE = 8`), once per pre-ore chunk, and `ore_stage` does the same for its own 3×3.
Each resolution was a full scan of the table. Measured, 6×6 sweep, seed 42, release:

| | searches | rows compared | per pre-ore chunk |
|---|---|---|---|
| before (`6102af4b`) | 31,076 | 235,991,144 | **2,359,911** |
| after | 2,276 | 278,298 | 2,783 |

The 2.36M figure independently reproduces the ~2.2M the plan derived by audit. Both totals are exact
products of constants derived from the drivers before measuring.

Two things about how this survived:

- **It was found by audit, not by a test.** Every gate was green throughout; the search was *correct*, just
  performed 305 searches per pre-ore chunk where the memoised sweep averages 22.8 (2,276 over 100). There is
  no assertion shape that notices redundant *correct* work, which is why
  `docs/plans/worldgen-rewrite.md` D6 makes counters a first-class deliverable.
- **The comment defending it was true when written.** `biome.rs` said *"a few thousand squared-distance
  comparisons per quart column is already fast"*, and at the time the only caller was the 16-per-chunk
  per-quart surface sample — 121,504 comparisons, genuinely trivial. Carver composition later took the
  searches per pre-ore chunk from **16 to 305** (16 quarts + a 289-chunk source window), a 19x rise in
  *count* with no change to per-search cost — and a cold `column()` closes over 25 pre-ore chunks on top of
  that. So the claim rotted without ever becoming visibly wrong. Corrected in place in
  `src/biome/mod.rs`'s module doc; the general lesson is CLAUDE.md rule 2's.

## Which vanilla search we implement, and why that was a decision (#492)

`Climate.ParameterList` ships **two** searches over the same table:

- `findValueBruteForce` — argmin squared distance, ties keep the **earliest table row**.
- `findValueIndex` → `RTree.search` — prunes with strict `>`, lets the **incumbent** win an exact distance
  tie, and seeds the incumbent from `RTree.lastResult`, a `ThreadLocal` holding the previous search's leaf.

**Vanilla calls the tree.** This engine called brute force from #405 onward, and Unit 9's first landing
deliberately made the tree reproduce brute force, on the reasoning that the old engine was the JVM-proven
bridge. Measuring the two against each other showed they resolve to **different biome ids at 0.98% of
arbitrary climate targets**. The owner ruled: do what vanilla does. So the tree is now the answer, and
brute force is the documented divergence.

### The disagreement is ties, and only ties

Established, not assumed — and the distinction decides whether the change is safe:

- If the tree found a *different nearest row*, brute force would simply have been wrong all along and the
  change would be a bug fix of unknown blast radius.
- If the disagreement is confined to **exact distance ties**, then neither search is wrong about distance;
  they take different members of a tied set, and the change is a tie-break swap.

It is the second. Span containment (below) forces both searches to land on the same minimum squared
distance, and the gates assert directly that **zero disagreements occur at a target with a unique
minimum** — measured over a complete 4⁶ lattice always-on, and a complete 9⁶ lattice plus 4.2M per-axis
unit steps in release. On the 4⁶ lattice: 64 row disagreements, all 64 resolving to a different biome id,
**0 at a unique minimum**. Over 200,000 arbitrary targets: 1,984 row disagreements, 1,950 resolving to a
different biome id (0.97%), **0 at a unique minimum**.

### What it changes in a real world, measured rather than inferred from silence

Every pre-existing gate passed unchanged after the swap — byte-identity, `dark_forest` (#480), the
JVM-anchored 17-coordinate biome fixture, both vegetation seam gates, `parallel_generation_*`, all 642
`lodestone-server` tests. **That is not the same as "nothing changed", and the difference mattered here.**
Measured directly, seed 42:

| consumer convention | sampled at | changed |
|---|---|---|
| carver/ore source biome | `y = 0`, 128×128 = 16,384 source chunks | **8** |
| surface biome (what a player sees) | each quart's own generated height, 12×12 chunks = 2,304 quarts | **0** |

So the swap really does move carver and ore selection, at 8 chunks in that region — and every existing gate
missed it because those chunks fall outside the fixture regions. The asymmetry is the interesting part and it
is why both conventions are measured: the `y = 0` convention lands where `depth`'s gradient is already
≈ +1.0, deep in cave climate-space where the table's rows crowd together and exact ties are reachable; the
surface convention samples near each column's own terrain, where over this region no tie occurred at all.

### The JVM fixture, taken where the two behaviours actually differ

A fixture at arbitrary coordinates would prove nothing: the two searches agree at >99% of real source
chunks, so a random probe set passes under *either* tie-break. So the fixture is the complete set of
divergent chunks above, each verified with `BiomeOracle sample 42 <x> 0 <z>` run **once per coordinate in
its own JVM process** — because `RTree.lastResult` seeds sample *N* from sample *N−1*, only a process's
first sample is the fresh-instance answer, and a batched run would have produced history-contaminated
expectations.

| source chunk | JVM `indexed` (vanilla, = ours) | JVM `brute` (= pre-#492 ours) |
|---|---|---|
| (26, −41) | `sunflower_plains` | `plains` |
| (31, 58) | `river` | `ocean` |
| (39, −60) | `forest` | `meadow` |
| (43, 61) | `river` | `beach` |
| (44, −2) | `deep_ocean` | `deep_cold_ocean` |
| (51, −18) | `cold_ocean` | `deep_cold_ocean` |
| (52, 15) | `ocean` | `cold_ocean` |
| (57, 25) | `ocean` | `cold_ocean` |

**8 of 8** agree with vanilla's `findValue` and **8 of 8** disagree with `findValueBruteForce` — which is
exactly what this engine produced before. `vanilla_tree_fixture_at_the_eight_divergent_source_chunks`
asserts both columns, so it is a characterisation of the divergence rather than a one-sided check, and it
fails if a future change makes the two coincide.

### `lastResult` reaches the answer, so vanilla is not a function

Traced from the source rather than guessed, because if the seed were a pure optimisation there would be no
tension at all:

- **It cannot change the returned distance.** A node's bound lower-bounds every leaf beneath it. A subtree
  is skipped only when `minDistance <= bound`, and at that moment every leaf inside is at distance
  `>= bound >= minDistance`; `minDistance` is always some real leaf's distance and only decreases. So no
  skipped subtree held anything better, and the returned leaf sits at the global minimum for **every**
  seed. `no_seed_can_change_the_minimum_distance` gates this.
- **It can change the returned row.** If `dist(candidate) == d_min`, then `minDistance == d_min` from the
  start; every subtree has `bound <= d_min` so is skipped unless `bound < d_min`, and any subtree that *is*
  entered yields a leaf at `>= d_min`, so the strict `minDistance > leafDistance` never fires. **The
  candidate itself is returned.** Exhibited concretely by `a_tying_seed_changes_the_returned_row`: at
  `[5332, 7970, 9712, 10919, 5156, -8536, 0]` the unseeded search returns row 7591 and a tying seed returns
  row 7590.

So vanilla's in-game biome at a tied target depends on what that worker thread searched previously. That is
not implementable as a function, and reproducing it would mean one seed produced different worlds across
runs.

**Therefore, precisely what this port does and does not do:**

| | |
|---|---|
| **Implemented** | vanilla's traversal with `candidate == null` — the first leaf in its pruned DFS child order at the minimum distance. The **fresh-instance answer**: what a newly constructed `ParameterList` returns, and the only history-free reading of "what vanilla does". |
| **Deliberately not implemented** | `lastResult` carry-over. No seeding of any kind, not even pruning-only: under incumbent-wins-a-tie, a hint that ties *is* the answer. |

This is a real, named divergence from a running vanilla server at tied targets, and it is the one the
determinism gates require.

## How it works

### The tree

`tree.rs` is a statement-for-statement port of `Climate.RTree.create`/`build`/`bucketize`/`sort`/`cost`/
`buildParameterSpace`, so node ordering comes from the game data's own row order run through vanilla's own
splitting heuristic. Fan-out 6, stable sorts (Java's `List.sort` is TimSort; Rust's `sort_by` is stable
too, and the difference would be a different tree), truncating `(min + max) / 2` centres, split axis by
minimum summed hull extent with the lowest axis winning ties. On the real table: 7594 → 1296 → 216 → 36 → 6.

**Child order is now load-bearing for the answer**, not merely for pruning speed — it is what breaks a tie.

One construction detail is deliberately not a transliteration. Vanilla computes its bucket size as
`(int) Math.pow(6.0, Math.floor(Math.log(n - 0.01) / Math.log(6.0)))`. `Math.log`/`Math.pow` are specified
only to 1 ulp, so a float port would make **node layout depend on the host libm** near any power of six.
The integer equivalent is "`6^k` for the largest `k` with `6^k < n`", and
`bucket_size_matches_vanillas_float_formula_for_every_plausible_n` checks the two agree for every `n` in
`2..100_000`. (This hazard was relayed to Unit 4, which took its noise octave factors off a 1-ulp `pow` in
`04ee733e`.)

### The memo

The plan's U9 row says "memoised per-source biome in the store", and
[`docs/worldgen-staged-store.md`](worldgen-staged-store.md) invites a fourth `StageSlot`. **Measured against
that store's own derivation, it is the wrong home**: `COLUMN_CLOSURE_RADIUS` (2) and `STORE_RETENTION` (512,
derived from a 289-column burst's 441-chunk closure) both come from the drivers, and a carver-source lookup
reaches 8 chunks beyond a pre-ore chunk. So a `column()`'s carver-source closure is radius **10** — a
441-chunk pin becomes 1,681 and the burst working set 441 → 1,369. Retention would have to exceed 1,369,
quadrupling worst-case live grid memory (an entry retains its pre-ore and post-ore grids too), or evict
inside a live request. Neither is worth paying for a four-byte value whose recomputation is one tree search.

So the memo is its own structure, shaped for its own access pattern:

- **Thread-local, so no lock exists at all.** The value is a pure function of the key, so per-thread copies
  cannot disagree. A shared map would re-create the contention U6 deleted, one layer down.
- **Direct-mapped on the low 5 bits of each chunk coordinate** — `((cz & 31) << 5) | (cx & 31)`, 1,024
  slots, 24 KiB per thread. Not a hash: any 17×17 window fits inside a 32×32 residue block, so **one carve
  stage's 289 lookups are collision-free by construction**, not by probability.
  `a_carver_source_window_never_self_collides` checks that over every window origin in more than a full
  residue period — and it reads the window width from `carver::NEIGHBOURHOOD_RANGE` rather than naming 17,
  with a `const _: ()` **compile-time** assertion that the window still fits a residue block. Hard-coded, a
  widened carver neighbourhood would have degraded the memo to a thrashing cache with every test still
  green: the vacuity would live in the *geometry*, not the assertion — the same shape U4 hit at
  `cell_width = 4`. That is why `carver::NEIGHBOURHOOD_RANGE` became `pub`, and why the counter gate derives
  its 289 and its 26 from it too.
- **Tagged with the full key** `(table_id, cx, cz)`, so a displaced residue is a miss, never a wrong biome
  — the store's "exact keys only" rule, whose scar here is a *clamped*-key cache that aliased two chunk
  coordinates and hung a JVM oracle.
- **Keyed by table identity too.** Tests build several generators on one thread; without `table_id`,
  generator B would read generator A's biomes.

A memo hit rate cannot change generated terrain, which makes this the half of Unit 9 with no parity risk.

### Why no file outside `src/biome/` and `src/overworld/biome.rs` changed

`usable_overworld_table` returns a `BiomeTable` (rows **plus** the tree) instead of a bare `Vec`.
`BiomeTable` derefs to `[BiomeParameterPoint]` and consumes into an iterator of them, so
`overworld/mod.rs`'s `DynamicBiome` literal, its `d.table.iter()`, and `lodestone_server::worldgen_data`'s
`table.into_iter().map(|p| p.biome)` all compile untouched. And `biome_for_carver_source` keeps its exact
signature — `-> &str` borrowed from `self` — because the memo stores a **table row** which indexes back into
the generator's own table, so `carve_stage` and `ore_stage` (U4's and U7's files) needed no edit.

A deliberate trade: a `Deref` to a slice hides that the type carries more than the slice. The alternative
was patching `overworld/mod.rs`, a measured choke-point file, while two other units were mid-flight in it.

## How to change it

- **Never seed the search.** Under vanilla's incumbent-wins-a-tie rule a seed that ties *is* the answer, so
  any "pruning hint" silently makes output depend on search history. This is the one line where the
  determinism of a served world lives.
- **Child order is the tie-break.** Any change to `build_level`, `bucketize`, or the sort comparators
  changes which biome is selected at tied targets. Re-run the release identity gates; a construction change
  cannot be checked by reading.
- **Adding a caller that resolves a biome per chunk position**: route it through `biome::memo::source_row`
  with `table.id()`, not a second cache. A caller with a *different* height convention is a different
  question — carver/ore sample at `y = 0`, vegetation at surface height, and the plan's U9 note is explicit
  that these are per-consumer and **must not be unified** (#480 was a biome resolved at `y = 0` instead of
  surface height, making `dark_forest` decorate as `lush_caves`).
- **Unit 11 (3-D biome sampling) inherits this file.** The seam is clean for quart-cell sampling:
  `BiomeTable::nearest`/`nearest_row` take a `[i64; 7]` and know nothing about geometry, and nothing in
  `tree.rs` or `memo.rs` assumes a 2-D world. What U11 must change is `biome_stage`, which builds 16 targets
  from `qz * 4 + qx`; the memo is keyed by *chunk* position and used only for the `y = 0` per-source-chunk
  question, so a per-quart-**cube** memo needs `qy` in the key and its own slot map — the current one would
  collide across `y`.
- **Do not delete `nearest_row_brute_force`.** It is no longer the target but it is the independent
  implementation that proves the distance claim, and it is what makes the #492 divergence measurable.

## Configuration

| knob | where | note |
|---|---|---|
| `CHILDREN_PER_NODE` | `tree.rs`, compile-time | 6 — vanilla's `Climate.RTree.CHILDREN_PER_NODE`. Changing it changes the tree *and the tie-break*. |
| `DIMENSIONS` | `tree.rs` | 7; vanilla throws if the parameter space is not 7 |
| `COORD_BITS` | `memo.rs` | 5 ⇒ a 32×32 residue block. **Cannot be lowered to 4**: 17 > 16, and collision-freedom is lost. Enforced by a `const` assert. |
| `gen-counters` | `lodestone-worldgen` feature, default off | required for the counter gate |

## Gates

Always-on (`cargo test -p lodestone-worldgen`): `src/biome/{tree,memo}.rs` unit tests, and
`tests/biome_tree_identity.rs` against the real 7,594-row table.

`#[ignore]`d, release:

```text
cargo test --release -p lodestone-worldgen --test biome_tree_identity -- --ignored --nocapture
cargo test --release -p lodestone-worldgen --features gen-counters \
  --test biome_search_counters -- --ignored --nocapture
```

| claim | strength | gate |
|---|---|---|
| every node's span contains its children's | **theorem premise**, complete enumeration over every node/child pair × 7 axes | `the_real_tree_is_shaped_right_and_every_node_contains_its_children` |
| same *minimum squared distance* as brute force | **always**, at every target | `the_minimum_distance_matches_brute_force_over_a_complete_lattice` + the 9⁶ and per-axis release sweeps |
| row disagreements are **only** ties | **always** — 0 at a unique minimum | `the_row_divergence_from_brute_force_is_exactly_the_tie_set` |
| `lastResult` cannot change the distance | **always** | `no_seed_can_change_the_minimum_distance` |
| `lastResult` *can* change the row | concrete case | `a_tying_seed_changes_the_returned_row` |
| **we match vanilla where the two searches differ** | JVM ground truth, 8/8 both columns | `vanilla_tree_fixture_at_the_eight_divergent_source_chunks` |
| what the swap moves in a real world | exact recorded counts (8 sources, 0 surface quarts) | `the_tiebreak_moves_exactly_the_eight_recorded_source_biomes_and_no_surface_quart` (release) |
| search count == derived prediction | exact, to the digit | `the_search_count_lands_on_the_memoised_prediction` (own binary — counters are process-global) |

### What "exhaustive" means here, and what it cannot mean

The space is `20001^6 ≈ 6.4e28` targets. **No gate can enumerate it**, and one claiming to has miscounted.
"We checked a lot of coordinates and they agreed" is the *magnitude* species of vacuous test. So the
distance claim is a **theorem whose single premise is verified by complete enumeration**, and the lattices
corroborate at three scales: a complete 4⁶ lattice always-on, a complete 9⁶ lattice (531,441 targets), and
every integer step of `[-11000, 11000]` on each of the six axes through 32 base points (4,224,192 targets).

### The controls, and *four* premise-false attempts

A broken node bound is only observable through a **prune**, and that single fact invalidated three natural
controls before one fired. All measured, not reasoned about after the fact:

| perturbed | wrong distances | why it cannot fire |
|---|---|---|
| a **leaf** | 0 | narrowing a leaf makes it less attractive, and 1 row of 7,594 never wins on the lattice |
| the **root** | 0 | the search iterates the root's children and never prunes on the root's own bound |
| the root's **first** child | 0 | `best_dist` starts at `i64::MAX`, so `best_dist > child_bound` is unconditionally true for the first child — it can **never** be pruned |
| a **later** child of the root | **464 of 4,096** | reached only once `best_dist` is finite, so its bound really does gate a prune |

The third row is the one worth remembering: it is specific to vanilla's strict-`>` pruning and did not exist
under the first landing's inclusive pruning, so re-pointing the gate at vanilla *created* a new premise-false
control. `distance_control_fires_when_a_prunable_tree_node_is_perturbed` therefore walks the root's children
from the back and fails loudly if none of them fires, rather than trusting a fixed index.

The `hull_containment_violations` control is checked in **both** directions: collapsing an interior node must
be reported, and narrowing a *leaf* must **not** be — a narrower child is still inside its parent's hull, so
firing there would mean the detector reports change rather than containment.

## Dependencies

`serde_json` for the table asset; `lodestone-worldgen-core`'s `counters` for the search hooks. The identity
gates read the real production asset
`crates/lodestone-server/assets/worldgen/biome_parameters/overworld.json` from a path, because
`EmbeddedResolver` is private to `lodestone-server` — and they assert its row count is 7,594, since this
crate's own fixtures supply **no** biome parameters and a gate against them would agree trivially over a
one-row table (the *world* species of vacuous test).
