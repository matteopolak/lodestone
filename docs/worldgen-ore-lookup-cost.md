# Ore placement: lookup cost, and the vein system that is still missing

## What it is

The cost model of the `UNDERGROUND_ORES` decoration stage — the most expensive stage in chunk
generation, **28.7% of a steady-state column** after DESIGN.md §12.149 (38.7% before it, and 43.54%
of stage time when this doc was written) — plus the record of what U15 and §12.149 removed from it,
why neither removal can move an RNG draw, and the two non-obvious couplings that any future
`OreVeinifier` port has to respect. It is the companion to
[`worldgen-in-place-decoration.md`](./worldgen-in-place-decoration.md) (which owns the *medium*
decoration writes travel through) and to [`worldgen-fast-hashing.md`](./worldgen-fast-hashing.md)
(which makes the engine's remaining lookup tables cheaper); this doc is about the lookups the ore
engine stopped performing at all.

## How it works

`OverworldGenerator::ore_stage` runs vanilla's real `blockStateWriteRadius(1)` driver: **nine full
`UNDERGROUND_ORES` passes per chunk**, one per source chunk of the 3×3 neighbourhood, each with its
own origin and its own `decorationSeed`, all writing into one shared `RegionView`. Inside a pass,
`place_placed_feature` reproduces Java's depth-first `flatMap` nesting, and `place_ore_feature` →
`do_place` → `try_place_ore` walks the blob.

### The profile that motivated U15

`samply` 0.13.1, release, `threadCPUDelta`-weighted, `bench_ore_composition_sweep`, seed 42, 4×4
plains-fixture patch. The ore placement engine proper (`feature::apply_one_source`) is **22.85% of
process CPU**; inside that subtree the self-time split was:

| self share of ore subtree | frame |
|---|---|
| 42.17% | `place_placed_feature::recurse` (with `do_place`/`try_place_ore`/`is_adjacent_to_air` inlined) |
| 19.23% | `core::hash::sip::Hasher::write` |
| 5.76% | `RegionView::get` |
| 3.85% | `hash_one::<&(i32,i32,i32)>` |
| 3.24% | `hash_one::<&str>` |
| 2.95% | `OreInput::get_height` |
| 2.78% | `hash_one::<&(i32,i32)>` |
| 3.04% | `RawTable::reserve_rehash` (two instantiations) |

**~32% of the ore engine's self time was hashing**, and none of it was hashing anything vanilla
computes. Two of the three named containers are gone as of U15:

1. **`OreInput::ocean_floor_wg` was a `HashMap<(i32,i32), i32>`.** It is probed once per cell of
   `place_ore_feature`'s pre-placement probe box — up to 27 × 27 for the `size = 64` blob ores — for
   every emitted position of every ore of all nine sources. It is now
   `feature::RegionHeights`, a dense `48 × 48` array indexed by the *already clamped*
   region-local key `OreInput::region_local` produces. `overworld/decorate.rs`'s own doc had
   predicted this exact fix ("if it ever matters, the win is a dense `[i32; 48 * 48]` array rather
   than a `HashMap`, and the clamp has to move with it") — and the clamp did move with it: it stays
   in `region_local`, and `RegionHeights`'s accessors document that they assume a pre-clamped key.
2. **`do_place`'s `tested` was a `HashSet<(i32,i32,i32)>`.** It is now `VisitedBox`, a dense bitset
   over the blob's own bounding box — which is vanilla's own representation (`OreFeature.doPlace`
   uses a `BitSet` keyed by a flat index into the blob box). One `Vec` allocation replaces a
   `HashSet` plus its whole `reserve_rehash` doubling chain, and a bit test replaces a SipHash.
3. **`try_place_ore` allocated a `String` per candidate position.** The base name was
   `.to_string()`d for one reason only: to end the immutable borrow of the view before the write.
   The loop now decides *which* target wins under the immutable borrow and performs the single write
   after it, so the `String` is gone and the base name is a borrowed slice.

### Why `VisitedBox`'s bounds are derived, not computed algebraically

`VisitedBox::over_spheres` makes a first pass over the **same per-sphere bound expressions the
placement loop below it walks**, and takes their union. It would have been shorter to bound the box
algebraically from `size`/`spread_xy`/`max_radius` — and that would have been a second derivation
to keep in step with the first. A box one block small does not fail loudly; it drops a dedup, which
re-tests a position and **places an extra ore**. Derived this way the box is exactly the set of
coordinates the loop can visit, and `insert` asserts rather than tolerating an out-of-box
coordinate, so a future drift between the two is a panic and not a changed world.

## How to change it, and the gotchas

**RNG order is the world.** None of the three changes above touches a draw: the 3×3 driver's
per-source `set_feature_seed` ordering, the depth-first recursion, and
`should_skip_air_check`'s per-matching-target `nextFloat` all fire in the same places, in the same
order, the same number of times. The evidence is a counter, not an argument —
`counters::rng_draws[Stage::Ore]` over a 12×12 embedded sweep reads **10,346,061** before and
after, and **495,301** on a cold column before and after, both digit-identical across repeated
runs.

**A control on the index formula is premise-false, and it was caught here.** The first draft of the
`RegionHeights` gate compared `RegionHeights::get` against the `HashMap` over every region column,
with a companion control asserting that a *transposed* index would be caught. The control was run
against a deliberately transposed `index` and **passed** — because `set` and `get` share one `index`
function, so permuting it permutes writes and reads together and nothing observable changes. The
index formula is not a correctness property at all; any bijection over the region is equivalent.
What *is* observable is the composition the driver performs, so the gate now drives
`OreInput::get_height` end to end (stitch offsets → clamp → read) against the deleted `HashMap`
lookup written out longhand, and its control perturbs the **stitch**, which does fire. If you add a
container here, ask what the control would have to perturb to be observed — not merely whether a
control exists.

**`in_tag` is done, and it was worth −13.54% of a whole column** (DESIGN.md §12.149). This section
used to end "the next lookup on this path is `in_tag`, and it is not done", and it was right about
both the target and the fix: `try_place_ore` resolved `RuleTest::TagMatch` through a
`HashMap<String, HashSet<String>>`, two string hashes per target test, **81,316 target tests per
interior column, every one a tag test**. `TargetCache` now answers it from a `u16` compare:

* 16 slots direct-mapped on the raw `StateId`, full key compare, holding a **bitmask** of which of
  `config.targets` match that state — plus each target's own interned `StateId`, which removes the
  other string cost, one `StateInterner::id_of` per write over **62,796 writes per column**.
* Built inside `do_place`, which handles exactly one `OreConfig`, so the config is not in the key.
  **Do not hoist it to a `thread_local!`**: without config identity in the key and a clear between
  generators, that is DESIGN.md §12.143's stale-scratch failure — a value from a *different
  function*, at a plausible magnitude.
* `match_targets_by_name` is the original name-based test, moved out verbatim and still the only
  implementation of it, so the cached and uncached paths cannot drift.

**Why a bitmask and not a "first matching target" index.** `canPlaceOre` can refuse a matching
target, and vanilla then continues to the *next* matching one, drawing another `nextFloat`. Only a
mask consumed in ascending order reproduces the sequence of matching targets, and therefore the draw
sequence, exactly. A config wider than `MAX_CACHED_TARGETS = 8` bypasses the cache; vanilla's widest
`UNDERGROUND_ORES` config has two targets.

`is_adjacent_to_air` was deliberately **not** converted. It runs 735 times per column against 79,148
candidates — 0.9% of the stage — and it is where the correctness risk was concentrated.

### What the measurement was, and the control that is better than the byte comparison

`feature::ore_probe` (thread-local, `gen-counters`-gated, inert otherwise) reports fourteen event
counts per column. **Twelve are digit-identical across the change** — blobs 962, spheres 17,717,
candidates 79,148, already-tested 208,580, height probes 215,607, region reads 83,504,
overlay-answered reads 14,699, writes 62,796, air checks 735 — while `target_tests` goes
**81,316 → 3,113**, a 96.2% cache hit rate. So the placement walk did not move and only the string
tests were removed. The U15 dump is byte-identical on top of that (8,902,157 bytes, md5
`c0ef05ac09ba3f90175a14b0f9a69d50`, with a one-flipped-byte control), and `I_ss` fell
**429,998,204 → 371,799,524** over eight interleaved rounds, eight of eight negative.

### The remaining cost, and the one that is structural

`ore` is now **28.7%** of a steady-state column (was 38.7%), and ~85% of it is the single inlined
`place_placed_feature::recurse` frame. Its address histogram splits as ~45% `do_place` sphere
arithmetic, ~5.6% `VisitedBox::insert`, and **~23.7% hashbrown — `RegionView`'s write overlay**,
which is ~5.8% of a column. See DESIGN.md §12.149 for the dense form and the two non-obvious
properties that make it safe (the flat index is monotone in `(y, lz, lx)`, so it *is* the fold-back's
sort; and a write log admits duplicate keys where a `HashMap` does not, so the fold-back must read
each cell's final value out of the array).

**The 9× is real and is not collectable.** Every chunk's own `UNDERGROUND_ORES` pass runs nine times
across a swept world — once as its own centre, once per neighbour. The RNG is not the obstacle
(`apply_one_source` re-seeds from the source's own origin, so the nine streams are independent) and
neither is memoisation (`post_ore` already exists, one layer too coarse). The obstacle is
`OreInput::region_local`: it clamps into the **driving** chunk's box, so `apply_one_source(S)` is not
a function of `S` alone and its write-set cannot be cached per source. Widening ore's read region to
5×5 is the cure `region_view.rs`'s `WIDE_RADIUS` doc already names, and it is a world change with its
own parity surface. Vegetation has already paid that price, which makes vegetation — not ore — the
place where per-source caching may be reachable without changing the world.

## The ore-vein system is still missing, and here is what a port must respect

`assets/worldgen/noise_settings/overworld.json` carries `ore_veins_enabled: true` and all three
router channels — `vein_toggle`, `vein_ridged`, `vein_gap`, over the `minecraft:ore_veininess`,
`ore_vein_a` and `ore_vein_b` noises — and **nothing in the engine consumes any of them**. So
vanilla's large copper and iron veins do not generate. This is a live Overworld parity gap, not a
new-dimension feature; it is invisible to the existing `(0, 0)` composed fixture only because that
chunk happens not to prove a vein, which is why the gate for it **must** include a vein-positive
JVM fixture. Tracked as [#496](https://github.com/matteopolak/lodestone/issues/496).

The port target is `OreVeinifier.create` (26.2). Two couplings, both read out of the decompiled
source rather than assumed, and both of which change the shape of the implementation:

1. **The veinifier only sees positions where the aquifer returned `null`.** `NoiseChunk` composes
   the fill rule as a `MaterialRuleList` — aquifer first, veinifier second — and
   `MaterialRuleList.calculate` returns the **first non-null** filler result.
   `Aquifer.computeSubstance` returns `null` exactly when the position is solid terrain, and the
   caller turns `null` into `settings.defaultBlock()`. So a vein replaces the *default block* and
   never air, water or lava. In our engine that is exactly `BlockKind::Stone`, so the veinifier's
   natural home is a second pass over `fill_stage`'s field.
2. **Vein blocks survive surface rules, and they also change surface rules' inputs.**
   `SurfaceSystem.buildSurface` applies a rule only under `if (old == this.defaultBlock)`, so
   granite/tuff/copper_ore/deepslate_iron_ore/raw-ore blocks are never rewritten — which is why the
   veinifier picks `deepslate_iron_ore` for the deep band itself rather than relying on a deepslate
   surface rule. But the same loop's `isStone` lookahead (`!isAir && fluidState.isEmpty()`) counts
   vein blocks as stone, so a vein *does* shift `stoneBelowDepth`, which feeds surface-rule
   conditions. **A vein is therefore not a cosmetic block substitution applied after the fact** —
   it has to exist in the field before `surface_stage` runs, or surface output diverges.

   Checked, and this is the good news: our `SurfaceSystem::build_surface` already has both halves —
   the `old == self.default_block` guard and an `is_stone` that is `!is_air && !is_fluid`, matching
   vanilla exactly. So the surface stage needs no change; what it needs is a field that carries the
   vein states. The engine-side work is (a) plumbing the three router channels plus vanilla's
   `oreRandom` positional factory through `AquiferTrees`, and (b) widening the fill field from
   `BlockKind`'s four variants to something that can carry a per-position vein state into
   `surface_stage`'s `pre` closure and `materialize_world`.

## Configuration

None at run time. The counters quoted above need `--features gen-counters` (default off, every hook
compiles to an empty function without it). `LODESTONE_ORE_SINGLE_SOURCE_DEBUG` still forces the
centre-only pass for isolating "is the centre pass correct" from "does real 3×3 spill widen the
gap"; it is a debug escape hatch, not part of `column()`.

## Dependencies

`feature::region_view::RegionView` for the read/write medium,
`overworld::decorate` for the driver and the heightmap stitch, `interner`/`dense_grid` for numeric
state ids, and `lodestone-worldgen-core`'s `counters` for the RNG-draw and allocation counters the
claims above rest on. The profiling workflow is
[`roadmap/benchmarks.md`](./roadmap/benchmarks.md) — with one caveat recorded in
[#496](https://github.com/matteopolak/lodestone/issues/496): `scripts/profile-cost-table.py` cannot
read a samply 0.13.1 profile, because it targets the hoisted `profile["shared"]` layout of
processed-profile ≥ 56 while samply 0.13.1 emits version 55 with per-thread tables.
