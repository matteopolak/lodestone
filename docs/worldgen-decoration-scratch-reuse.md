# Decoration scratch reuse

## What it is

The per-thread free-list that recycles the three containers worldgen's decoration
media write into — `RegionView`'s write overlay and `VegGrid`'s overlay and `dirty`
log — so a served column takes them already at capacity instead of growing them from
empty. Unit 19 of [`plans/worldgen-rewrite.md`](plans/worldgen-rewrite.md); it took a
warm served column from **87 heap allocations to 64** and the vegetation stage's own
share from **30 to 7**, while changing **not one byte** of generated world (45
columns, 5 seeds, `cmp` clean, both dumps md5 `a9db7cf741214167db615fa8b9356fa8`).

It lives in
[`feature/region_view.rs`](../crates/lodestone-worldgen/src/feature/region_view.rs)'s
private `scratch` module, because that file already owns the shared routing both
decoration media use and the alternative was a new module in `feature/mod.rs` — a
choke point.

## Why it existed to be fixed, and why two units had to leave it

[`worldgen-vegetation-ids.md`](./worldgen-vegetation-ids.md) ends with a precise
handover. Unit 8 took a warm column from 20,678 allocations to 87, of which:

| what | count | whose |
|---|---|---|
| the returned column's palette/blocks buffers | 41 | the plan's explicit O(1) **output** allowance |
| `VegGrid`'s overlay + `dirty` log growing from empty | 30 | Unit 7's medium |
| everything else | 16 | — |

Unit 8 could not take the 30. They are `O(log writes)` geometric container growth in
a file Unit 8 did not own, and
[`worldgen-in-place-decoration.md`](./worldgen-in-place-decoration.md) records Unit 7
*deliberately* not pooling them: "There is **no buffer pool**, shared or otherwise…
If scratch reuse is ever needed here it goes in a `thread_local` free-list."

That was not caution for its own sake. The reason is in
[`worldgen-staged-store.md`](./worldgen-staged-store.md): a **shared** pool behind a
lock puts 289 concurrent generator calls into cache contention on one cache line, and
commit `4307b59` is the measured incident — a per-ring barrier removal that had to be
reverted for exactly that. So the constraint on any reuse here is *per-thread, never
shared*, and Unit 19 took the escape hatch Unit 7 had already named rather than
inventing a different one.

## How it works

Two types, one free-list each, take-and-return:

- **`Overlay`** wraps a `FastMap<(i32, i32, i32), StateId>`. Used by
  `RegionView::overlay` (centre-relative local keys) and `VegGrid::blocks`
  (`VegGrid`-local keys). Note this also completes
  [`worldgen-fast-hashing.md`](./worldgen-fast-hashing.md)'s open row for
  `VegGrid::blocks` — it was still on `std`'s SipHash and is now on `hash::fast` as a
  side effect of sharing the type.
- **`WriteLog`** wraps a `Vec<(i32, i32, i32)>`. Used by `VegGrid::dirty`.

Both `Default::default()` pops from a `thread_local` free-list (or builds a fresh
container, counting a **miss**), and both `Drop` **clears and returns** the buffer.
`KEEP = 4` bounds each list, so a long-lived worker thread cannot turn the cache into
a leak.

The same commit closes the rest of
[`worldgen-fast-hashing.md`](./worldgen-fast-hashing.md)'s open ~0.8% vegetation row.
All three of its maps are now on `hash::fast`, and each was checked **by grepping its
own field name** rather than the file, which is what that doc prescribes:

| map | reached only through | why order-safe |
|---|---|---|
| `VegGrid::blocks` | `get`, `insert` | private to `grid.rs`; never iterated at all |
| `IdTags::rewrites` | `get`, `insert`, `clear` | a pure memo; never iterated |
| `tree.rs`'s `Bfs::visited` | `clear`, `insert`, `contains` | BFS **traversal** order comes from `buckets`, so the leaf `distance` values that reach the wire cannot depend on the hasher |

That last row is the one worth reading twice. "Never iterated" would have been enough
for the first two, but for a BFS the interesting question is not whether the set is
iterated — it is whether the *visit order* can see the hasher. It cannot, because the
queue is a `Vec<VecDeque<_>>` and the set only answers membership.

Three properties are load-bearing:

- **Cleared on return, not on take.** A buffer sitting in the free-list holds no
  keys, so a stale entry can never be read by whoever takes it next. Clearing on take
  would work equally well until someone added a path that read before clearing.
- **`clear()` keeps capacity.** That *is* the mechanism: the pass that follows finds
  the map and the log already sized for the previous column's high-water mark.
- **Take-and-return, never borrow across a body.** A nested or re-entrant
  construction gets a *fresh* (merely allocating) buffer rather than a `RefCell`
  panic — the same discipline Unit 8's `place.rs`/`tree.rs` scratch uses.

### Self-tuning rather than pre-sized, deliberately

Capacity converges to whatever that thread's largest column needed and then stops
growing; the growth is paid **once per thread**, not once per column.
[`worldgen-fast-hashing.md`](./worldgen-fast-hashing.md) explicitly declined to guess
a `with_capacity` here for want of a measured target value, and this removes the need
for one: there is no constant to be wrong about.

The visible consequence is that "zero allocations" is a *steady-state* claim.
`a_warm_pass_allocates_zero_at_every_scene_size` warms over all four of its scenes
**twice** before measuring, because after one round the retained capacity only covers
whichever scene ran last.

## How to change it, and the gotchas

- **Before letting anything else share this free-list, establish the iteration-order
  half of the argument at the new consumer.** A recycled `HashMap` has a different
  capacity, and therefore a different iteration order, from a fresh one. This is safe
  today for exactly two reasons, and they are *per consumer*: `VegGrid::blocks` is
  private to its own module and reached solely through `get`/`insert` — never
  iterated at all — and `RegionView::centre_writes_in_scan_order` sorts by the full
  key precisely so order cannot be observed. `overworld/mod.rs`'s module doc carries
  the post-mortem of the time a `RandomState`-ordered map fed the palette and palette
  order reached the wire. Recycling is the same hazard as a `FastMap` swap, and needs
  the same argument.
- **A gate that holds two media alive across its arms is not measuring reuse.**
  Production builds one medium per column and **drops it before building the next** —
  that drop is what returns the buffer. `vegetation_allocs.rs` held both arms' grids
  alive and so still read 13 after the pooling landed, measuring a cold growth while
  calling itself warm. `drop(cold_grid)` plus an assertion on
  `scratch_free_list_lengths()` is now how that arm proves it is warm.
- **`scratch_misses()` is the attribution instrument, and it is the thing to reach
  for first.** A warm column still allocating a handful of times says nothing on its
  own about *which* container is responsible. A zero here says **none of these
  three** is, so the search moves elsewhere without a profiler. Always compiled in,
  like `ids`' fast/slow counters and for the same reason: it is per-*medium*, not
  per-block.
- **Do not `census::reset()` inside a measured window.** `VegCensus::unsupported` is
  a `BTreeMap<String, usize>`; clearing it costs a `String` clone plus a tree node on
  the first unmodelled dispatch of each distinct reason — measured as **exactly 2**,
  flat across four scenes of 2,086–2,499 writes. `OverworldGenerator` never resets the
  census, so those 2 are the instrument, not the engine. Take deltas.

## What remains, and why

A warm served column allocates **64**: 41 intern + 16 other + 7 vegetation.

- The **41** are the returned column's own palette and blocks buffers — the plan's
  explicit O(1) output allowance. Unit 19 neither improved nor disturbed them; they
  read 41 before and after, as they did across the whole of Unit 8.
- The **7** attributed to the vegetation stage are **not** these containers, and that
  is measured rather than argued: `scratch_misses()` reads **0** across four warm
  interior columns, with a control that drains the free-list and observes the counter
  reach 2. They are the output side of `vegetation_stage` — the private copy of the
  centre's post-ore grid that `overworld/decorate.rs`'s own doc names as "the one
  copy this stage still makes", plus that copy's palette and index growth as
  vegetation's new states are appended to it. **That composition is inferred from the
  call site, not independently measured**; treat it as a claim and instrument it
  before acting on it.
- The **16** "other" is untouched and unattributed, exactly as Unit 8 left it.

## `RegionView::overlay` stayed a map — and the measurement says the opposite of what was expected

[`worldgen-fast-hashing.md`](./worldgen-fast-hashing.md) measured this overlay as the
single largest hashed map in the pipeline (**39.5% of all hash time, 8.3% of all
CPU**) and left a change rule: "a cheap hasher is the right fix only for a map that
has to stay a map. Where the key space is dense and bounded — which is true of every
coordinate-keyed cache in this engine — check for an array before reaching for
`FastMap`." U15 had already shown that shape paying off, replacing `ocean_floor_wg`
with a dense 48×48 array instead of re-hashing it.

So the expected answer here was a dense value array behind a presence bitset: the
overlay is keyed `(i32, i32, i32)` over a bounded region, reads probe it on every
cell, and the probes were assumed to be overwhelmingly **misses** — in which case a
108 KB bitset in front of the map removes nearly all the hashing for 1/16th of the
memory a dense value array costs.

**That assumption is false.** Measured over warm served columns at seed 42, counting
every `Overlay::get`/`insert` on the decoration path:

| warm column | probes | hits | inserts | miss rate |
|---|---|---|---|---|
| (0, 0) | 230,582 | 109,884 | 7,424 | 52.3% |
| (1, 0) | 190,818 | 83,868 | 6,652 | 56.0% |
| (0, 1) | 177,831 | 71,970 | 5,762 | 59.5% |

Roughly **45% of probes hit**: ~7,000 written cells absorb ~100,000 successful reads,
about 15 re-reads per written cell, because the heightmap scans and
`is_adjacent_to_air` walk back over cells decoration has just placed. A
bitset-in-front-of-a-map therefore removes only the ~55% that miss *and* adds a
bitset test to the 45% that hit — a much weaker result than the "nearly all of it"
the cheap design was chosen for. Removing the hashing outright needs the **full**
dense value array: 48×48×384 × `u16` = 1.77 MB plus a 108 KB occupancy bitset for
`RegionView`, and 64×64×384 (`VEG_PADDING` widens it) = 3.1 MB plus 192 KB for
`VegGrid` — per thread, pooled.

Unit 19 **did not** land that, and the reason is scope rather than merit: it is a
second change with its own byte-identity obligation, in the coordinate space that
produced this repo's worst worldgen defect, and it has one known trap already —
`VegGrid::seed_id` inserts **without a bounds check**, and `vegetation_allocs.rs`'s
own fixture seeds `REGION_MIN - 8 .. REGION_MAX + 8` into a grid whose footprint is
`REGION_MIN..REGION_MAX`, so out-of-footprint keys really do exist in the map today.
A dense structure must either size for them or prove dropping them is equivalent
(it is — `to_local_clamped` makes them unreachable — but that is an argument a gate
should carry, not a comment).

**No speedup is claimed here, in either direction.** The table above is a **counter**,
not a duration: it says how many hash operations happen, not how long they take.
`DESIGN.md` §12.103–§12.104 is the standing reason — U17 measured unchanged code
moving ×0.90 to ×2.25 between two captures on this machine. A follow-up unit taking
the dense conversion should quote the counter reaching zero, or interleave arms in one
process.

## Gates

| gate | where | what it holds |
|---|---|---|
| `a_warm_vegetal_decoration_pass_allocates_only_its_grids_own_container_growth` | `tests/vegetation_allocs.rs` | the pass-level criterion: **0**, as an equality, with the pre-U8 floor and the pre-U19 geometric ceiling both named |
| `a_warm_pass_allocates_zero_at_every_scene_size` | same | zero at four scenes spanning 2,086–2,499 writes, with the spread asserted so identical scenes cannot pass vacuously |
| `recycling_is_what_removes_the_containers_and_draining_the_free_list_puts_them_back` | same | the **causal** control: same pass, same scene, free-list drained between — 0 → 11 allocations |
| `a_warm_served_column_takes_every_decoration_container_from_the_free_list` | `tests/vegetation_column_allocs.rs` | `scratch_misses() == 0` over four warm production columns, with a drained-free-list control reaching 2. Deliberately gates **attribution, not a total** |
| `a_recycled_overlay_is_empty_and_is_really_the_recycled_one` | `region_view.rs` | no stale key survives a return, *and* a control that the buffer really was recycled — without it the test is about `HashMap::new()` |
| `a_recycled_write_log_is_empty_and_restarts_at_index_zero` | same | the same for write order, which reaches the served palette |
| `the_free_list_does_not_grow_without_bound` | same | 32 drops retain ≤ 4, and > 0 — a bound *and* the reuse it must not eliminate |
| `a_canopy_spans_the_chunk_seam_in_both_served_chunks` | `lodestone-server/tests/decoration_seam_spill.rs` | Unit 7's boundary-write control, unchanged and still live |

### Both controls were observed failing

Not described — run, watched, and restored by `cp` from a scratchpad backup with an
md5 check (never `git checkout`, per CLAUDE.md):

- **Removing `map.clear()` from `Overlay::drop`** (a recycled buffer carrying the
  previous column's writes) fails the production seam control at its *premise*:
  `logs_west=13, leaves_west=194, leaves_east=0` — the cross-seam spill vanishes
  entirely. Two of the four `vegetation_allocs` gates fail with it.
- **`source_slot` on `/ 16` instead of `div_euclid(16)`**, the routing bug Unit 7
  recorded, still drives the same control from 20 crossing rows to **0**, `bbox =
  None`. That one matters not because Unit 19 could cause it but because it proves the
  seam control is **still live** after this change rather than merely still green.

### Byte identity

Two isolated detached worktrees at the same sha, the "after" arm carrying **only**
this unit's four source files (verified by `git status` in that worktree), harness md5
**identical on both arms** (`99691badc02cca288a9071f0491d2fa7`). 45 columns, 5 seeds ×
3×3 patches, 8,899,204 bytes: `cmp` exit 0, both dumps md5
`a9db7cf741214167db615fa8b9356fa8`. Detector control: one bit flipped at offset
2,000,000 → `differ: char 2000001`, exit 1. Non-degeneracy: 64 distinct block states,
45 distinct block-array byte values, 1,414,441 non-air blocks.

The control was built by reverting **this unit's** files in the **current** tree, not
by comparing against the sha the work started from — `worldgen-vegetation-ids.md`
records Unit 8 losing time to exactly that, chasing an 18-cell disagreement that
turned out to be Unit 9's biome change landing in between. HEAD moved four times
during this unit's session, which is the ordinary condition here rather than bad luck.
The run was done twice for that reason — once for the pooling alone and once for the
finished four-file unit, two HEADs apart — and both produced the same dump md5, which
also happens to say the intervening commits were byte-neutral at these five seeds.

Vegetation RNG draws read **11,034 per column**, unchanged — the same figure Unit 8
recorded across all three of its arms. The plan marks the vegetation walk "must not"
change its draw order, and a constant draw count is a parity signal as much as a
performance one.

## Configuration

None. No feature gate, no env var, **no new dependency**. Crates were considered and
rejected on measurement rather than reputation: `smallvec`/`arrayvec` buy nothing
because the containers are recycled whole rather than being small (the `dirty` log
reaches ~2,500 entries), `bumpalo` solves many-short-lived-heterogeneous allocations
and the residual is neither, `memchr` has no scan left to accelerate since Unit 8's
interning, and `hashbrown`/`rustc-hash` would replace arithmetic that
`lodestone_worldgen_core::hash::fast`'s own module doc records **deliberately** declining to take as a
dependency — "a `Cargo.lock` edit in a shared checkout for the same arithmetic". `Cargo.lock` is
untouched by this unit.

`scratch_free_list_lengths`, `scratch_misses`, `reset_scratch_misses` and
`drain_scratch_free_lists` are public so gates can be built on them, in the same
spirit as `census::reset` and `ids::reset_counts`. `drain_scratch_free_lists` exists
specifically so an assertion of absence has a control.

## Dependencies

`std` only, plus [`worldgen-fast-hashing.md`](./worldgen-fast-hashing.md)'s `FastMap`
for the overlay. Consumed by
[`worldgen-in-place-decoration.md`](./worldgen-in-place-decoration.md)'s two media —
`RegionView` and `VegGrid` — and therefore by `overworld/decorate.rs`'s `ore_stage`
and `vegetation_stage` and `feature/mod.rs`'s ore engine, none of which needed a
change: both media keep their existing public API.
