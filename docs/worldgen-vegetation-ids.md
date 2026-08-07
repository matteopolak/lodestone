# Vegetal decoration on numeric ids

## What it is

The vegetation placement engine's port off block-state strings and onto Unit 3's
`StateId`s, plus the fixed bitsets that answer tag membership in O(1). Unit 8 of
[`plans/worldgen-rewrite.md`](plans/worldgen-rewrite.md); it took a steady-state warm column
from **20,678 heap allocations to 87** — vegetation's own share from 20,621 to 30 — while
changing **not one byte** of generated world.

## Why it existed to be fixed

[`worldgen-state-interning.md`](./worldgen-state-interning.md) ended with a precise
handover: after Unit 3, **99.7% of a warm column's remaining allocations were in the
vegetation stage**, and every other stage read exactly zero. Fill, surface, carve and ore
are cache hits on a warm column — they do not execute — so no amount of work on them could
move the counter. Unit 8 was the only unit that could.

Two costs, measured rather than assumed:

**Allocation.** `BlockStateProvider::get_state` returned `Option<String>`, cloning out of
the config it had just read — one allocation per grass blade, log and leaf. Each placement
modifier returned a fresh `Vec<BlockPos>` *per attempt*. The leaf `distance=N` rewrite and
the `waterlogged` fix-up each did `to_string()` + `replace_range` + re-intern, per leaf. And
one line in the census cloned a `String` on **every** unmodelled dispatch (see below) —
which turned out to be 95% of what was left once the engine itself stopped allocating.

**Per-draw cost.** The draw count is spec-bound (11,034 per column, and it still is), but
what each draw's consequences cost is ours. Every ground check, `validTreePos`,
`isAirOrLeaves` anchor and heightmap cell paid an interner `RwLock` **read guard** through
`name_of`, a `split('[')`, and a `HashSet<String>` probe.
[`worldgen-vegetation-census.md`](./worldgen-vegetation-census.md) counts **74,745 ground
rejections in one 136-chunk sweep**, and that is only the rejections that reach a census
bump — the tree footprint scan does more, and `height_world_surface` walked ~250 cells of
empty sky per probe, taking that guard on every one.

## How it works

### Tag membership as bitsets over state ids

[`feature/vegetation/ids.rs`](../crates/lodestone-worldgen/src/feature/vegetation/ids.rs)
holds one bitset per membership question, indexed by `StateId`. `StateId` wraps a `u16`, so
**65,536 ids is the whole space and the table can never need to grow** — it is exact, not a
generous over-allocation. 13 questions × 8 KiB, one `alloc_zeroed` per `VegTags`, and a
`VegTags` is per-generator.

The 13 are the 7 registry tags (`supports_vegetation`, `replaceable_by_trees`, `logs`,
`cannot_replace_below_tree_trunk`, `supports_cactus`, `supports_sugar_cane`, `leaves`) plus
six base-name equalities the old code spelled inline: `Air`, `Fluid`, `Water`, `Lava`,
`Cactus`, `SugarCane`. They share one mechanism because they ask the same question of the
same subject. `config::is_air`/`is_fluid` remain the *definitions* — `ids`' fill calls them
— so there is exactly one place that decides what counts as air.

`VegTags::bind` walks the interner's new ids and raises a watermark; the driver calls it
**once per decoration pass**, and that is the only place `StateInterner::len`'s lock is
taken on the decoration path. A query below the watermark is one relaxed atomic load and a
bit test.

### The watermark is a correctness gate, not an optimisation

Decoration **mints ids during its own pass** — a leaf rewritten to `distance=3` is a state
the interner may never have seen. Reading such an id from the bitset would answer `false`
for every tag, which is a **wrong answer, not a slow one**: an unexamined `oak_log` would
fail `#minecraft:logs` and change where leaves decay. Ids at or above the watermark
therefore take the pre-U8 string path.

That fallback also makes every pre-existing unit test work untouched. A test that builds
`VegTags::default()`, inserts into `tags.leaves` and calls `place_tree` directly never
binds, so the watermark is 0 and every query answers exactly what it answered before.

### Ids everywhere else

`get_state` borrows from the provider instead of cloning; `get_state_id` resolves to the id
the grid actually stores. That `id_of` is **not new cost** — `VegGrid::set_if_in_bounds`
already performed exactly it on the `String` it was handed, so U8 removed the allocation and
left the lookup where it was.

`distance=N` and `waterlogged` became memoised `id -> id` lookups
(`VegTags::rewrite`). Scratch that used to be allocated per call — `place_block_column`'s
layer heights, `place_tree`'s trunk and attachment vectors, `update_leaf_distances`' bucket
queue and visited set — is now reused `thread_local` buffers, taken-and-returned rather than
borrowed across the body so a hypothetical nested placement gets a correct (merely
allocating) fresh buffer instead of a `RefCell` panic. `truncate_layers`' index list and the
beehive candidate list became computed indices and a fixed array.

### Positions without a Vec

`VegPlacement::get_positions` returns `Positions::{None, One, Repeat}`. Enumerating every
arm shows the `Vec` it returned only ever held one of those three shapes, so the enum is
**exhaustive over what the old code could produce**, not a narrowing — the table is in
`Positions`' own doc comment. `Repeat(p, n)` recurses `n` times on the same position,
exactly what `vec![p; n]` meant. If a modifier that genuinely fans out to *different*
positions is ever added, it needs its own variant; it must not be smuggled in as a `Repeat`.

## How to change it, and the gotchas

- **Never mutate a `VegTags`' `HashSet`s after `bind` has run.** The bitset is a cache of
  those sets and nothing re-derives it, so an insert after binding is visible to the string
  path and invisible to the bitset. Production builds the sets once in `build_veg_tags` and
  never touches them; the tests that do mutate them never bind. If you need both, add a
  `rebind` that clears the masks and resets the watermark.
- **The interner's instance id is part of both the bitset fast-path condition and the
  rewrite memo key, and both are load-bearing.** This was not so at first, and
  `tree_placement_is_deterministic_across_two_independent_generators` caught it inside one
  test run: that test shares one `VegTags` across two grids with two *private* interners,
  the memo handed interner A's `StateId` to grid B, and `name_of` panicked with
  "the len is 8 but the index is 10". **A shorter tree would not have panicked — it would
  have stored a plausible wrong block.** Clearing on instance change is not sufficient
  cover, because nothing binds on the direct-placement path that tests use.
- **Add a `Tag` by adding a variant, a `Tag::ALL` entry and a `member` arm.** A missing
  `ALL` entry leaves that tag's bits permanently zero, which reads as "nothing is in this
  tag". `tag_all_is_complete_and_in_discriminant_order` fails on it.
- **`Tag::Fluid` must stay base-aware.** `carver/mod.rs` writes `minecraft:water[level=0]`,
  so a fluid is *not* a fixed handful of ids — which is why `VegGrid::height_ocean_floor`
  still resolves a name for the few cells it has already found to be non-air, while
  `height_world_surface` can compare against three cached ids. That air shortcut is exact
  **only because air carries no block-state properties**; `config::is_air`'s doc says not to
  add a property-carrying state to it, and
  `the_id_based_air_test_answers_what_the_string_scan_answered` is the differential control.
- **The residual 30 allocations are `VegGrid`'s own containers, not the engine.** The write
  overlay (`HashMap`) and the `dirty` log (`Vec`) start empty on a fresh grid and grow
  geometrically — `O(log writes)`.
  [`worldgen-in-place-decoration.md`](./worldgen-in-place-decoration.md) records that Unit 7
  deliberately did not pool them ("If scratch reuse is ever needed here it goes in a
  `thread_local` free-list"), so reaching literal zero is a change to that unit's medium and
  is left as a named follow-up rather than smuggled in here.
- **A `_ => false` in `member` is a silent no-op the way every terminal router arm in this
  repo is.** `member` is exhaustive over `Tag` on purpose; keep it that way.

## Measurements

Release, embedded server data, seed 42, warm 12×12 sweep, counting allocator in
`benches/generation.rs`, `--features gen-counters`:

| sha | total | vegetation | intern | other |
|---|---|---|---|---|
| `5344b8ad` (pre-U8) | 20,678 | 20,621 (99.7%) | 41 | 19 |
| `226920f5` (ids + bitsets) | 1,243 | 1,186 (95.4%) | — | — |
| `1519464d` (census fix) | **87** | **30** (34.5%) | 41 | 16 |

`intern` reads **41 in every run** — the returned column's own palette/blocks buffers, the
plan's explicit O(1) output allowance — so U8 neither improved nor disturbed it. Vegetation
RNG draws read **11,034 in all three**, unchanged: the walk still consumes exactly the
spec-bound draws, which is a parity signal as much as a performance one.

`ns/draw` fell from 628 to 309 across the same runs, but **treat that as context, not
evidence**: four agents were building concurrently and the bench's own drift band flagged
±25% swings on unrelated scenes in the same session. A third run of the same binary read
1,419 ns/draw. The counters reproduced to the digit; the timings did not.

At the pass level (`tests/vegetation_allocs.rs`, dev profile, savanna scene): a **warm pass
allocates 13** for 2,499 writes, against 82 cold, and **18,318 of 18,318** tag queries take
the bitset path.

## Gates

| gate | where | what it holds |
|---|---|---|
| `a_warm_vegetal_decoration_pass_allocates_only_its_grids_own_container_growth` | `tests/vegetation_allocs.rs` | the acceptance criterion, bracketed by both hypotheses, with a detector control |
| `allocations_are_geometric_growth_not_per_write` | same | the discriminating form — fails if the count ever scales with writes again |
| `the_id_based_air_test_answers_what_the_string_scan_answered` | same | the deleted string scan rebuilt and compared at all 2,304 columns of a decorated region |
| `the_bitset_answers_what_the_string_path_answers` | `ids.rs` | differential, both paths, plus the fallback exercised |
| `a_rewrite_never_escapes_the_interner_it_was_computed_in` | `ids.rs` | the regression for the bug above |
| `binding_a_second_interner_discards_the_first_ones_bits` | `ids.rs` | both directions of interner scoping |
| `vegetation_parity` | `tests/vegetation_parity.rs` | the JVM anchor — plains 30/30 and 57/57, savanna 185/185 and 115/116 |

**The scene is savanna, not plains, and that is not incidental.** `vegetation_parity`
measured that both plains fixtures place **zero** logs and zero leaves, because
`trees_plains` rolls no attempt ~95% of the time. A plains scene would exercise neither
`place_tree` nor the leaf `distance=N` rewrite nor the `waterlogged` fix-up — green while
claiming three sites it never reached, the *world* species of vacuous test. The census
assertions (`tree > 0`, `simple_block > 0`, `block_predicate_filter_in > 0`) make coverage a
measurement instead of a belief, and `slow_hits == 0` **with** `fast_hits > 0` is asserted
because either alone is satisfiable by a broken mechanism.

### Byte identity, and the baseline that was wrong

Whole-column byte identity was established in isolated worktrees with an md5-verified
identical harness on both arms, over 3 seeds × 5 chunks (3.1 MB, 61 distinct byte values,
642 palette/biome lines — non-degenerate), with a detector control confirming `cmp` reports
a single flipped byte.

The first attempt compared against `5344b8ad`, the sha the unit started from, and found
**18 differing cells**: one coal-ore blob at y = 58–59 of seed 42 chunk (20, −5). Chasing it
is worth recording, because every step of the diagnosis mattered:

1. The pre arm agreed with **itself** across two runs, so it was not nondeterminism.
2. It reproduced with a **single chunk and no warmup**, so it was not a store or ordering
   effect.
3. Adding *only* the two ids U8 causes to be interned (`cave_air`, `void_air`) to the old
   tree changed **nothing**, so interner id-assignment order is not world-visible —
   `worldgen-state-interning.md`'s central claim holds.
4. Forcing every tag query onto the string path did not remove it, so it was not the bitset.
5. Skipping the vegetation fold-back entirely, identically in both arms, still showed all 18
   cells — proving the difference was **upstream of vegetation**, which nothing in U8 can
   reach. Vegetation's own write count was identical (1,006) in both arms.

The cause was another unit: **`71dd8b22`, U9's biome RTree fix**, which the rewrite plan
already predicted "will legitimately shift biomes at ~1% of coordinates". A shifted biome
changes that chunk's ore list. Re-run against the correct control — the same tree with
`feature/vegetation/` reverted, so the two trees differ *only* in U8's files — the dumps are
**byte-identical, md5 `b447593891f74f364c3d3ac18ec61671` on both arms**. `f77def5e` (U5's
SIMD) and `7ba0176b`/`0a3ede8d` (U10) also changed no byte.

The lesson is cheap to state and was expensive to learn: **in a shared checkout, "compare
against the sha my brief named" is not the same as "compare against my change"**, and the
difference is other agents' landings. Build the control by reverting *your* files in *the
current* tree.

## Configuration

None of its own. `gen-counters` gates the counters the column-level figures are read from;
`ids`' own `fast_hits`/`slow_hits` are always compiled in (two thread-local `Cell`s) because
the acceptance gate needs them and they are not per-block. `LODESTONE_VEG_STRICT` and
`LODESTONE_VEG_SINGLE_SOURCE_DEBUG` are unchanged.

## Dependencies

`interner` for `StateId`/`StateInterner`, `VegGrid` for the read/write medium
([`worldgen-in-place-decoration.md`](./worldgen-in-place-decoration.md)), and
`compose::resolve_block_tag` for the tag closures `bind` fills the bitsets from. Consumed by
`overworld/decorate.rs`'s `vegetation_stage` through the unchanged driver signature — no
caller outside `feature/vegetation/` needed a change.
