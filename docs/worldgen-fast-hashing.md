# Worldgen fast hashing

## What it is

The worldgen engine's internal lookup tables use a cheap in-house `FxHash`-style
`BuildHasher` (`lodestone_worldgen_core::hash::fast`) instead of `std`'s
SipHash-1-3 `RandomState`. A `samply` profile measured **21.01% of all worldgen
CPU as self time inside SipHash** — the second-largest item in the whole
pipeline — and this is unit U17 of
[`plans/worldgen-rewrite.md`](plans/worldgen-rewrite.md) removing the part of it
that can be removed *safely*, where "safely" is a claim about `HashMap` iteration
order and not about speed.

## The attribution

Measured, not assumed. `samply` 0.13.1 against the release
`benches/generation.rs` binary at `b8763712`, weighted by
`samples.threadCPUDelta`, with every sample whose **leaf** frame is a hashing
symbol attributed to its nearest non-hashing caller:

| container | file | key | share of hash time | ≈ share of all CPU |
|---|---|---|---|---|
| `RegionView::overlay` | `feature/region_view.rs` | `(i32, i32, i32)` | **39.5%** | 8.3% |
| `StateInterner::ids` | `interner.rs` | `&'static str` | **20.8%** | 4.4% |
| `ocean_floor_wg` | `overworld/decorate.rs` | `(i32, i32)` | **12.8%** | 2.7% |
| `DenseBlockGrid::index_of` | `dense_grid.rs` | `StateId` (a `u16`) | **11.8%** | 2.5% |
| `SurfaceSystem`'s `surface_diff` | `surface/mod.rs` → `overworld/fill.rs` | `(i32, i32, i32)` | 5.5% | 1.2% |
| `top_layer::StatePredicate` | `feature/top_layer.rs` | `String` | 5.3% | 1.1% |
| `VegGrid::blocks`, `IdTags::rewrites`, `Bfs::visited` | `feature/vegetation/**` | mixed | 3.7% | 0.8% |

Two things that are **not** on that list, and both were candidates going in:

* **`overworld/store.rs`'s 64 shard `HashMap`s.** A column takes on the order of
  34 store probes; the sharded store never appears in the hash profile at all.
  Sharding a map does not make the map hot.
* **`engine/`'s density slot and cell caches.** U4 had already given them a
  private `FxHasher` for exactly this reason, so they were already paid for. U17
  promoted that hasher to `hash::fast` and deleted the private copy rather than
  ship a second one — see the type alias comment in `engine/scratch.rs`.

`reserve_rehash` appearing in the profile (6.8% of hash time, under
`RawTable<((i32,i32,i32), StateId)>`) is the ore overlay **growing**, not a
mis-sized static table.

## How it works

`hash::fast::FastHasher` folds each word into an accumulator with
`(h.rotate_left(5) ^ word).wrapping_mul(K)` for an odd 64-bit `K`, and `finish`
returns the accumulator **unrotated**. `FastMap<K, V>` / `FastSet<T>` are the
`std` containers parameterised by it, so semantics are unchanged — only the
hasher differs.

The unrotated `finish` is deliberate and measured. `hashbrown` takes the bucket
index from the **low** bits and the control byte from the **top 7**; a bare
multiply by an odd constant serves both, because multiplication by an odd
constant is a bijection modulo `2^n`, so the low `n` bits of the hash are a
permutation of the low `n` bits of the key. `StateId`s are handed out
`0, 1, 2, …`, so a `StateId`-keyed table collides *never*. An earlier draft ended
`finish` with `rotate_left(20)`, copying `rustc-hash`'s shape without checking
whether it helped here — it does not: 4096 sequential keys into 4096 buckets
occupy **3931** distinct buckets with the rotation and **4096** without. 3931 is
still far better than the ~2589 a uniformly random hash gives, which is exactly
why that would have survived review as "fine"; the test
`sequential_u16_keys_are_collision_free_in_the_low_bits` is what caught it.

## How to change it, and the gotcha that dominates this file

**Changing a hasher changes `HashMap` iteration order, and this repo has shipped
that bug.** `overworld/mod.rs`'s module doc carries the post-mortem: a
`RandomState`-ordered map fed the palette, and palette order reaches the wire
(`DenseBlockGrid::into_palette_and_blocks` must emit a byte-identical
`Vec<String>`).

So `FastMap` is only ever **half** of an argument. Before switching any map to
it, establish the other half at the map itself:

* **Never iterated** — a pure reverse-lookup accelerator beside an ordered `Vec`.
  This is what `DenseBlockGrid::index_of` (ordered structure: `palette: Vec`) and
  `StateInterner::ids` (ordered structure: `names: Vec`) are. Verify by grepping
  `.iter()` / `.keys()` / `.values()` / `.drain()` / `for (k, v) in` against
  **that specific field name**, not against the file.
* **Iterated, but the consumer imposes a total order of its own.** This is
  `RegionView::centre_writes_in_scan_order`, which sorts by the full key
  precisely so iteration order cannot be observed, and says so in its own doc.

Anything else — an order feeding a palette, a seed, a draw sequence, or a
serialised structure — must not use `FastMap`. Use a `BTreeMap`, an index-keyed
`Vec`, or insertion-order storage. `FxHash`-style hashers being fine for
non-adversarial keys is an argument for **safety**, never for
**order-independence**; both have to hold independently.

**Never for parity.** `hash::md5` and `hash::java_string_hash` reproduce values
vanilla also computes, so their output is load-bearing to the byte.
`hash::fast`'s output is load-bearing for *nothing* and may change between
commits without breaking a gate — which is why it must never derive a seed, an
id, or any value that reaches the wire.

## What was deliberately left alone

Three of the measured maps sit in files owned by other in-flight units at the
time of writing, and were reported to the orchestrator rather than edited (two
agents in one file is its own incident class):

| map | file | unit | ≈ CPU |
|---|---|---|---|
| `ocean_floor_wg` | `overworld/decorate.rs`, `feature/mod.rs` | U15 | 2.7% |
| `surface_diff` (type flows through the fill seam) | `overworld/fill.rs` | U15 | 1.2% |
| `VegGrid::blocks`, `IdTags::rewrites`, `Bfs::visited` | `feature/vegetation/**` | U8 | 0.8% |

Each is a point cache, never iterated, so each is a one-line `FastMap` swap
whenever its owner is free. Together they are the remaining ~4.7% of CPU that
U17 attributed but did not claim.

**Pre-sizing was also left alone.** `reserve_rehash` is real (6.8% of hash time)
and `RegionView::overlay` starts at capacity zero, but a `with_capacity` guess
is an allocation-size change with no measured target value, and rehashing under
`FastMap` is already an order of magnitude cheaper per key. Measure the actual
`writes()` distribution before picking a number.

## Configuration

None. No feature gate, no env var, no new dependency — `hash::fast` is ~30 lines
of arithmetic in a crate that is inside the wasm-confined set
(`cargo xtask check-isolation`), and pulling `rustc-hash` would have meant a
`Cargo.lock` edit in a shared checkout for the same arithmetic.

## Dependencies

`std` only. Consumed by `lodestone-worldgen`'s `dense_grid`, `interner` and
`feature::region_view`, and by `lodestone-worldgen-core`'s `engine::scratch`.

## Evidence

* **Byte identity.** 20 serve-boundary columns (4 seeds × 5 chunks, embedded
  production server data) dumped through `GeneratedColumn::into_raw` in isolated
  worktrees with an **md5-identical harness on every arm**
  (`dc3d2b4e412cc88bebabdb883d020684`): `cmp` clean, both dumps md5
  `518283b2719f4e1994016de8e690d51f`.

  The after-arm was run **twice**, and the second run is the one to trust. The
  first was taken in the shared checkout, where a concurrent unit had edited
  `overworld/decorate.rs` four minutes earlier — a confound that could only have
  produced a false *failure*, never a false pass, but it meant the arm was not an
  isolate of this change. The second was a fresh detached worktree at the
  baseline sha carrying **only** this unit's six files, verified by
  `git status` in that worktree. Both arms produced the same md5. On a shared
  checkout, "I diffed my own files" is not the same claim as "the arm contained
  only my files"; only the worktree establishes the second.
* **Detector control.** One bit flipped at offset 2,000,000 of the baseline dump
  → `cmp` reports `differ: char 2000001`, exit 1. The identity comparison exits
  0. The detector was run and observed failing before the clean result was
  believed.
* **Non-degeneracy.** 84 distinct byte values across the dump (U7's comparable
  figure was 85), 663,751 non-air blocks, per-column palettes 22–36 entries —
  asserted by the harness, so a uniform slab could not have `cmp`-matched
  trivially.
* **`parallel_generation_is_deterministic_and_matches_serial`** — the gate an
  order dependence would surface in — run **12 times**, 12 genuine passes, with
  the test name verified present in each run's output. (The first attempt at this
  was vacuous: `--exact` without the full module path reported
  `0 passed; 545 filtered out` and exit 0 twelve times over.)
