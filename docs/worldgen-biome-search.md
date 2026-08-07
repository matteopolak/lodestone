# Worldgen biome search

## What it is

The climate → biome lookup layer: vanilla's `Climate.RTree` ported as a real search structure, plus a
per-source-chunk memo, replacing an uncached brute-force scan of the 7,594-row overworld climate table.
Unit 9 of [`docs/plans/worldgen-rewrite.md`](plans/worldgen-rewrite.md), the fix for its diagnostic
**D5**. Measured over a 6×6 sweep of real embedded data: **235,991,144 climate comparisons → 258,747**,
a 912× reduction, with the biome selected at every coordinate provably unchanged.

Code: [`crates/lodestone-worldgen/src/biome/`](../crates/lodestone-worldgen/src/biome/) —
`mod.rs` (table, quantization, brute-force reference, `BiomeTable`), `tree.rs` (the port),
`memo.rs` (the per-source-chunk memo). Driven from
[`crates/lodestone-worldgen/src/overworld/biome.rs`](../crates/lodestone-worldgen/src/overworld/biome.rs).

## The defect, and why no test could see it

`carve_stage` resolves a carver biome for every chunk in a **17×17 = 289**-chunk source neighbourhood
(`carver::NEIGHBOURHOOD_RANGE = 8`), once per pre-ore chunk, and `ore_stage` does the same for its own
3×3. Each resolution was a full scan of the 7,594-row table. Measured, 6×6 sweep, seed 42, release:

| | searches | rows compared | per pre-ore chunk |
|---|---|---|---|
| before (`6102af4b`) | 31,076 | 235,991,144 | **2,359,911** |
| after | 2,276 | 258,747 | 2,587 |

The 2.36M figure independently reproduces the ~2.2M the plan derived by audit (and a sibling's 2.37M).
Both numbers are exact products of derived constants, not samples — see "Gates" below.

Two things about how this survived:

- **It was found by audit, not by a test.** Every gate was green throughout; the search was *correct*,
  just performed 305 searches per pre-ore chunk where the memoised sweep averages 22.8 (2,276 over
  100). There is no assertion shape that notices redundant *correct* work, which is why
  `docs/plans/worldgen-rewrite.md` D6 makes counters a first-class deliverable.
- **The comment defending it was true when written.** `biome.rs` said *"a few thousand
  squared-distance comparisons per quart column is already fast"*, and at the time the only caller was
  the 16-per-chunk per-quart surface sample — 121,504 comparisons, genuinely trivial. Carver composition
  later took the searches per pre-ore chunk from **16 to 305** (16 quarts + a 289-chunk source window),
  a 19x rise in *count* with no change to per-search cost — and a cold `column()` closes over 25 pre-ore
  chunks on top of that. So the claim rotted without ever becoming visibly wrong. Corrected in place in
  `src/biome/mod.rs`'s module doc; the general lesson is CLAUDE.md rule 2's.

## How it works

### The tree: structure ported literally, search deliberately not

`tree.rs` is a statement-for-statement port of `Climate.RTree.create`/`build`/`bucketize`/`sort`/`cost`/
`buildParameterSpace` (26.2 de-obfuscated `net/minecraft/world/level/biome/Climate.java`). Node ordering
therefore comes from the game data's own row order run through vanilla's own splitting heuristic —
nothing re-derives a bucketing scheme, which is the property the plan's Q2 asks for. Fan-out 6, stable
sorts (Java's `List.sort` is TimSort; Rust's `sort_by` is stable too, and the difference would be a
different tree), truncating `(min + max) / 2` centres, and the split axis chosen by minimum summed hull
extent with the lowest axis winning a tie.

One construction detail is *not* a transliteration, on purpose. Vanilla computes its bucket size as
`(int) Math.pow(6.0, Math.floor(Math.log(n - 0.01) / Math.log(6.0)))`. `Math.log`/`Math.pow` are
specified only to 1 ulp, so a float port would make **node layout depend on the host libm** near any
power of six. The integer equivalent is "`6^k` for the largest `k` with `6^k < n`", and
`bucket_size_matches_vanillas_float_formula_for_every_plausible_n` checks the two agree for every
`n` in `2..100_000` rather than asserting the equivalence in a comment.

**The search is where this diverges from vanilla, and it is the unit's central decision.** Vanilla's
`SubTree.search` prunes with strict `>`, lets the incumbent win an exact distance tie, and seeds the
incumbent from `RTree.lastResult` — a `ThreadLocal` holding the previous search's leaf. So vanilla's
tree is **not a pure function of the target**: on a tie the answer depends on what that thread searched
before. Vanilla's own `findValueBruteForce` breaks ties by earliest table row instead. Vanilla ships
both and calls the tree.

Porting that verbatim would put thread-local search history into chunk generation, and
`column_is_byte_identical_across_two_independently_constructed_generators` would become a coin flip. So
this port keeps **brute force's tie-break**, which is also the behaviour every JVM-anchored biome
fixture in this repo was proven against:

| vanilla | here |
|---|---|
| prune when `best > child_bound` (strict) | descend when `child_bound <= best_dist` — ties are **never** pruned on distance |
| incumbent wins a tie | **lowest table row wins a tie** |
| `ThreadLocal` last-result seed decides ties | last-result seed is a pruning *hint*, provably unable to change the answer |

That makes result-identity a **theorem**: the search returns the lexicographic minimum of
`(distance, row)` over all leaves, which is exactly what brute force returns. It rests on one premise —
a subtree's span contains its children's, so `Node::distance` lower-bounds every leaf beneath it — and
that premise is checked by complete enumeration over every node/child pair on all 7 axes. Because the
answer is an order-independent min-reduction, the seed hint can live in a `Relaxed` `AtomicU32` shared
across threads instead of a `ThreadLocal`: any node id is a legal seed, a torn read is a legal seed, and
none of them can change the result. `the_result_is_invariant_under_every_search_seed` gates that.

### The memo: thread-local and direct-mapped, not a store stage

The plan's U9 row says "memoised per-source biome in the store", and
[`docs/worldgen-staged-store.md`](worldgen-staged-store.md) invites a fourth `StageSlot`. **Measured
against that store's own derivation, it is the wrong home.** `COLUMN_CLOSURE_RADIUS` (2) and
`STORE_RETENTION` (512, derived from a 289-column burst's 441-chunk closure) both come from the
drivers, and a carver-source lookup reaches 8 chunks beyond a pre-ore chunk — so a `column()`'s
carver-source closure is radius **10**: the pin grows 441 → 1,681 and the burst's working set 441 →
1,369. Retention would have to exceed 1,369, quadrupling worst-case live grid memory (an entry retains
its pre-ore and post-ore grids too), or entries would be evicted inside a live request — the exact
property that store gates at zero. Neither is worth paying for a four-byte value whose recomputation is
one tree search.

So `memo.rs` is its own structure, shaped for its own access pattern:

- **Thread-local, so no lock exists at all.** The value is a pure function of the key, so per-thread
  copies cannot disagree. A shared map would re-create, one layer down, exactly the contention U6
  deleted.
- **Direct-mapped on the low 5 bits of each chunk coordinate** — `((cz & 31) << 5) | (cx & 31)`, 1,024
  slots, 24 KiB per thread. Not a hash: any 17×17 window fits inside a 32×32 residue block, so **one
  carve stage's 289 lookups are collision-free by construction**, not by probability.
  `a_carver_source_window_never_self_collides` checks that over every window origin in more than a full
  residue period — and it reads the window width from `carver::NEIGHBOURHOOD_RANGE` rather than naming
  17, with a `const _: ()` **compile-time** assertion that the window still fits a residue block.
  Hard-coded, a widened carver neighbourhood would have degraded the memo to a thrashing cache with
  every test still green: the vacuity would live in the *geometry*, not the assertion — the same shape
  U4 hit at `cell_width = 4`. That is why `carver::NEIGHBOURHOOD_RANGE` became `pub`, and why the
  counter gate derives its 289 and its 26 from it too.
- **Tagged with the full key** `(table_id, cx, cz)`, so a displaced residue is a miss, never a wrong
  biome — the store's "exact keys only" rule, whose scar in this crate is a *clamped*-key cache that
  aliased two chunk coordinates and hung a JVM oracle.
- **Keyed by table identity too.** Tests build several generators on one thread; without `table_id`,
  generator B would read generator A's biomes.

A memo hit rate cannot change generated terrain, which makes this the half of Unit 9 with no parity
risk. The half that carries parity risk is the tree.

### Why no file outside `src/biome/` and `src/overworld/biome.rs` changed

`usable_overworld_table` now returns a `BiomeTable` (rows **plus** the tree) instead of a bare `Vec`.
`BiomeTable` derefs to `[BiomeParameterPoint]` and consumes into an iterator of them, so
`overworld/mod.rs`'s `DynamicBiome` literal, its `d.table.iter()`, and
`lodestone_server::worldgen_data`'s `table.into_iter().map(|p| p.biome)` all compile untouched. And
`biome_for_carver_source` keeps its exact signature — `-> &str` borrowed from `self` — because the memo
stores a **table row** which indexes back into the generator's own table, so `carve_stage` and
`ore_stage` (U4's and U7's files) needed no edit.

That is a deliberate trade: a `Deref` to a slice hides that the type carries more than the slice. The
alternative was a patch to `overworld/mod.rs`, one of this repo's measured choke-point files, while two
other units were mid-flight in it.

## A finding this unit did not act on

`vanillas_own_tree_and_brute_force_are_compared_on_the_real_table` measures the question the tie-break
decision hangs on. Vanilla's own two searches, on the real 7,594-row table:

| target set | vanilla's tree vs vanilla's brute force, **by biome id** | minimum tied across different biome ids |
|---|---|---|
| complete 9⁶ lattice (531,441 targets) | 16,526 (3.1%) | 67,313 (12.7%) |
| 200,000 arbitrary non-round targets | 1,950 (**0.98%**) | 2,108 (1.05%) |

Both arms agree with our tree at 0 disagreements against brute force, so nothing here is a Unit 9
regression. But it means **vanilla selects a different biome from our engine at roughly 1% of climate
coordinates**, and has since biome assignment landed — because vanilla calls the tree and we have always
called brute force. The regular lattice inflates the rate ~13× through symmetry, which is why the
arbitrary-target arm is the one that bounds it.

Unit 9 deliberately does **not** change this. The plan's Q4.5 rollback rule makes the old engine the
bridge and the JVM oracle the tie-breaker, and reclassifying behaviour without a JVM fixture is
explicitly disallowed. `scripts/worldgen-oracle/BiomeOracle.java` mode `sample` already dumps *both*
resolutions, so the fixture needed to settle it is cheap to produce. Filed as its own issue.

## How to change it

- **Adding a caller that resolves a biome per chunk position**: route it through
  `biome::memo::source_row` with `table.id()`, not through a second cache. Adding a caller with a
  *different* height convention is a different question — carver/ore sample at `y = 0`, vegetation at
  surface height, and `docs/plans/worldgen-rewrite.md`'s U9 note is explicit that these are per-consumer
  and **must not be unified** (issue #480 was a biome resolved at `y = 0` instead of surface height,
  making `dark_forest` decorate as `lush_caves`).
- **Unit 11 (3-D biome sampling) inherits this file.** The seam is left clean for quart-cell sampling:
  `BiomeTable::nearest`/`nearest_row` take a `[i64; 7]` target and know nothing about geometry, and
  nothing in `tree.rs` or `memo.rs` assumes a 2-D world. What U11 must change is `biome_stage`, which
  today builds 16 targets from `qz * 4 + qx`; the memo is keyed by *chunk* position and is only used by
  the `y = 0` per-source-chunk question, so a per-quart-**cube** memo would be a new key type (add `qy`)
  and want its own slot map — the current one would collide across `y`.
- **Never widen the prune to `>=` on distance.** `child_bound == best_dist` must descend, or a tie whose
  lower row lives inside the pruned subtree is lost and the tree stops matching brute force. This is the
  one line where the theorem lives.
- **If you change the tree's construction, re-run the identity gates in release.** A construction change
  cannot be checked by reading: it changes bucket boundaries, and a wrong hull is invisible until a
  pruning gate catches it.
- **Do not add a counter to `lodestone-worldgen-core`** without brokering it — that crate is Unit 4's.
  `biome_rows_compared`'s doc comment there still says it is `biome_searches * table_len`, which was
  true before this unit and is now the *brute-force* relationship only; the hook is reused for node
  evaluations, which is the honest analogue. That one-line doc fix is outstanding.

## Configuration

| knob | where | note |
|---|---|---|
| `CHILDREN_PER_NODE` | `tree.rs`, compile-time | 6 — vanilla's `Climate.RTree.CHILDREN_PER_NODE`. Changing it changes the tree, and therefore needs the identity gates re-run |
| `DIMENSIONS` | `tree.rs` | 7; vanilla throws if the parameter space is not 7 |
| `COORD_BITS` | `memo.rs` | 5 ⇒ a 32×32 residue block. **Cannot be lowered to 4**: 17 > 16, and the collision-free property is lost |
| `gen-counters` | `lodestone-worldgen` feature, default off | required for the counter gate |

## Gates

Always-on (`cargo test -p lodestone-worldgen`):

- `src/biome/{tree,memo}.rs` unit tests — the bucket-size equivalence over `2..100_000`, leaf-count and
  hull containment across several table sizes with a control, tree-vs-brute-force on a synthetic table,
  seed invariance, the carver-window collision-freedom proof, and the memo's tag/displacement behaviour.
- `tests/biome_tree_identity.rs` — the real 7,594-row table: complete node/child hull enumeration, a
  complete 4⁶ lattice, seed invariance, and **two controls**.

`#[ignore]`d, release:

```text
cargo test --release -p lodestone-worldgen --test biome_tree_identity -- --ignored --nocapture
cargo test --release -p lodestone-worldgen --features gen-counters \
  --test biome_search_counters -- --ignored --nocapture
```

- `identity_holds_over_a_complete_nine_per_axis_lattice` — every point of a 9⁶ = 531,441 lattice.
- `identity_holds_at_every_unit_step_along_every_axis` — every integer of `[-11000, 11000]` on each of
  the 6 axes through 32 base points, 4,224,192 targets.
- `vanillas_own_tree_and_brute_force_are_compared_on_the_real_table` — the finding above.
- `the_search_count_lands_on_the_memoised_prediction` — its own binary, because counters are
  process-global atomics (`docs/worldgen-staged-store.md` records a gate that read 502 against a true
  256 by sharing one).

### What "exhaustive" means here, and what it cannot mean

The space is `20001^6 ≈ 6.4e28` targets. **No gate can enumerate it**, and one claiming to has
miscounted. "We checked a lot of coordinates and they agreed" is the *magnitude* species of vacuous test:
it proves the searches usually agree. So identity is established as a **theorem whose single premise is
verified exhaustively** (every node/child pair × every axis — a complete enumeration), and the lattices
are corroboration that would catch a flaw in the theorem's reasoning. Each gate's doc names exactly what
it is exhaustive over.

The controls are what make any of it evidence, and **the first two choices of control were premise-false
in the safe-looking direction** — worth recording, because both looked fine:

| perturbed node | mismatches | why |
|---|---|---|
| a **leaf** (last flattened node) | 0 | narrowing a leaf only makes it less attractive, and 1 row of 7,594 never wins on the lattice |
| the **root** (node 0) | 0 | `nearest_row` descends into the root unconditionally and never prunes on its own bound |
| **interior node 1** (root's first child) | 32+ (collection capped) | its bound really does gate a prune, over ~1/6 of the table |

A control on either of the first two would have reported "identity is robust" while measuring nothing —
CLAUDE.md's "a control's premise can be false before the feature under test ever existed", reproduced
exactly.

## Dependencies

`serde_json` for the table asset; `lodestone-worldgen-core`'s `counters` for the search hooks. The
identity gates read the real production asset
`crates/lodestone-server/assets/worldgen/biome_parameters/overworld.json` from a path, because
`EmbeddedResolver` is private to `lodestone-server` — and they assert its row count is 7,594, since this
crate's own fixtures supply **no** biome parameters and a gate against them would agree trivially over a
one-row table (the *world* species of vacuous test).
