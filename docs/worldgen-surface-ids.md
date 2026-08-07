# Surface-stage state ids

## What it is

The surface stage carries **interned `StateId`s** rather than `String`s across
every seam: the pre-surface callback, the rule interpreter's result states, the
clay-band table and the sparse diff handed to materialisation. Before this
(issue #501, U21) that stage performed **3,847,972 heap allocations** on a 3×3
cold sweep — **97.3% of the whole worldgen pipeline's heap traffic, and 18× the
entire ore path** — from four `to_string()`/`clone()` sites. It now performs
**690**, all of them one `HashMap`'s growth series.

This is U3's interning story (`worldgen-state-interning.md`) applied to the one
seam it never reached.

## The measurement

`crates/lodestone-worldgen/tests/ore_alloc_attribution.rs`, 3×3 cold sweep at
seed 42 over chunks (40..42, 40..42), embedded production data, real
`GlobalAlloc` calls binned by **innermost** `Stage`:

```
cargo test --release -p lodestone-worldgen --features gen-counters \
    --test ore_alloc_attribution -- --ignored --nocapture
```

`--features gen-counters` is required, not optional: without it
`counters::current_stage()` is a constant `Stage::Other` and every row lands in
one bucket, which reads as a working instrument reporting a surprising answer.

| stage | arm A (`eba23934`) | arm B (post-U21) |
|---|---|---|
| **surface** | **3,847,972** | **690** |
| carve | 101,651 | 101,651 |
| shape | 2,303 | 2,303 |
| biome | 784 | 784 |
| ore | 503 | 503 |
| materialize | 444 | 444 |
| aquifer | 441 | 441 |
| other | 250 | 250 |
| intern | 219 | 219 |
| vegetation | 158 | 158 |
| **total** | 3,954,725 | 107,443 |

Digit-stable across two runs per arm. **All nine other stages are
digit-identical** — the free blast-radius control U18 introduced, and the thing
that would have to move if this change reached outside its stage.

The stage runs 49 times over that sweep (`stage_entered[Surface]`), so it went
from **78,530 to 14 allocations per stage entry**.

### Where the 3.85M were

Sampled backtrace attribution, innermost three `lodestone_worldgen` frames:

| share of stage | site | what it was |
|---|---|---|
| **77.08%** | `surface_stage::{closure#0}` under `build_surface::{closure#0}` | `pre` returning `String`: two `"minecraft:air".to_string()` plus `default_block`/`default_fluid`/`default_lava` clones, **once per probe** |
| **21.92%** | `SurfaceSystem::try_apply` | `Rule::Block(String)`'s `Some(state.clone())`, once per matched rule |
| 0.63% | `build_surface::{closure#0}` | the out-of-range `block_at` air clamp |
| 0.35% | `surface_stage::{closure#2}` | `biome_at` cloning a biome name per column |

### Why the residual 690 is not a target

It is the diff map's own growth series and **nothing else**, and two independent
instruments agree — which is the bar, because two instruments only count as
agreement if they could have disagreed:

* The **sampled** backtrace table attributes 100% of the post-U21 surface
  allocations to a `FastMap` reallocation inside `build_surface`.
* The **unsampled** size histogram shows exactly 14 distinct sizes — 76, 144,
  280, 552, 1096, 2184, 4360, 8712, 17416, 34824, 69640, 139272, 278536,
  557064 bytes — each occurring **exactly 49 times**, once per stage entry.
  That is a doubling series for a 16-byte entry (`(i32,i32,i32)` + `StateId`),
  which the symbol table cannot tell you and the size table can.

14 × 49 = 686 of the 690. The remaining 4 are 64-byte allocations spread over
49 stage entries — below the 1-in-64 sampling resolution, so they are
unattributed rather than explained; do not quote a cause for them.

**The 14 is hand-derivable, not merely mechanism-derived.** `hashbrown`'s
capacity series is 3, 7, 14, 28, … 14336, 28672 — fourteen entries to first
exceed a chunk's ~18,200 rewrites at 87.5% load. `tests/surface_allocs.rs`
measures 14 for the ocean fixture (18,195 rewrites, 60,157 probes) *and* 14 for
the land fixture (18,434 rewrites, 83,472 probes) — identical despite a 1.39×
probe ratio, which is the whole claim in one pair of numbers.

## How it works

Three properties make the conversion **total** rather than a relocation of the
cost. Each is the thing to preserve if you change `surface/mod.rs`.

### 1. Nothing is interned during a scan

`Rule::Block` holds a `StateId` resolved in `SurfaceSystem::new`; the caller
hands over `PreState`s built from ids it already owns. There is no `id_of` and
no `name_of`, and therefore **no `RwLock`**, anywhere under `build_surface`.

This matters beyond allocation, and it is the trap the obvious fix falls into:
"return `StateId` from `pre`" via `interner.id_of(name)` would have *moved* the
cost from `String` to a lock probe — ~60,000 read guards per chunk on a table
shared by every concurrent generator call. `4307b59` is this repo's revert scar
for exactly that shape (cache contention across 289 concurrent generator calls),
and the allocation counter alone would have looked *better* while the real cost
got worse.

`nothing_is_interned_during_a_surface_scan` in `tests/surface_allocs.rs` is the
gate: `StateInterner::len()` is the observable, and it moves the moment an
`id_of` returns to this path. It also asserts `len() > 1` *before* the scan, so
it cannot pass by having nothing to intern.

### 2. `Rule::Bandlands`' computed name is a table subscript

This looked like the blocker, and issue #501 recorded it as the reason the fix
was filed rather than done inline: `SurfaceSystem.getBand` **computes** which
block it returns rather than selecting a static one, so there is no `&'static
str` to borrow.

But the set it computes *over* is finite. `generate_bands` fills exactly
`CLAY_BANDS_LEN` (192) entries drawn from seven names — `minecraft:terracotta`
plus six `*_terracotta` dye variants — so the entire value set `getBand` can
return is known once per world seed. `RuleParser::bandlands` interns the
finished table into a `Vec<StateId>` and `get_band` became an index and a
`Copy`.

**Verified, not assumed** (`CLAUDE.md` rule 2): `bandlands()` asserts both the
length and that every entry is in `BAND_BLOCK_NAMES`, once per generator, and
names the new block if a future version adds an eighth. `generate_bands` itself
is **untouched** — every RNG draw in it is world-defining — and its
`Vec<String>` costs 192 allocations per world, once.

### 3. Classification is derived from the string definition, never written down

The scan branches on air/fluid/stone, which a `String` let it read off the name
via `is_air`/`is_fluid`. `PreState` now carries a `PreClass` beside the id so the
branch is free.

That is a shortcut, and a wrong class is **not** a crash — it changes which
rules fire and still produces a plausible column, i.e. exactly the
fully-connected-wire-carrying-the-wrong-value shape. So the class is never
hand-written at a use site:

* `class_of_name` is the surviving string definition, the single source of truth.
* `OverworldGenerator`'s `default_block_pre` / `default_fluid_pre` /
  `default_lava_pre` are built by `PreState::from_name`, i.e. by applying
  `class_of_name` to the very string the settings supplied.
* `surface_stage` **re-derives all four** from those strings on every entry
  under `debug_assertions` and compares. Four warm `id_of` lookups per stage
  entry, not per probe.

The last one is total in a way a constant-vs-constant assertion is not: it
catches a wrong id *and* a wrong class, including the copy-paste that pairs
`default_fluid_pre` with `default_lava`'s string — same class, different id.
That control was run and observed failing (exit 101,
`default_fluid_pre must be default_fluid, interned and classified`).

## Controls that were run and observed failing

Neither of these was described; both were executed and watched.

| control | perturbation | observed |
|---|---|---|
| field lock-step | `default_fluid_pre` built from `default_lava`'s string — **same class, different id** | exit 101, the `debug_assert` in `surface_stage` fired; a constant-vs-constant assertion structurally could not see this |
| classification is load-bearing | `BlockKind::Water` handed the **right id with `PreClass::Stone`** | exit 101, `composed_surface_and_fluid_are_applied` failed with "surface rule barely ran: 59 surface-capped vs 197 stone-capped columns" |
| byte-identity detector | one bit flipped at offset 2,000,000 of arm A's dump | `cmp` printed `differ: char 2000001, line 6733`, exit 1 |

**The second control's most useful finding is which gate caught it.**
`surface_parity` stayed **green**, because it drives `PreState::from_name` — the
string-classified path — not the `BlockKind` shortcut production takes. The
composed `overworld_gen.rs` gate is the only in-crate gate live on the
classification shortcut. If you change how `fill.rs` classifies, `surface_parity`
will not tell you.

## Byte identity

`tests/u15_column_dump.rs`, 45 columns (5 seeds × 3×3 patch), the wire-facing
`GeneratedColumn::into_raw` product:

```
LODESTONE_U15_DUMP=/tmp/arm.bin \
  cargo test -p lodestone-worldgen --test u15_column_dump -- --ignored --nocapture
```

Both arms in **one** working tree — arm A built by copying the HEAD blobs of
*this unit's own six files* over the tree, never by checking out a sha, because
siblings land in between (U8 lost time to the alternative). Verified before
believing the result: no sibling had touched any of the six between the base sha
and HEAD, `git diff --cached` was empty, and the harness's own md5
(`99691badc02cca288a9071f0491d2fa7`) was identical on both arms.

| | arm A | arm B |
|---|---|---|
| md5 | `a9db7cf741214167db615fa8b9356fa8` | `a9db7cf741214167db615fa8b9356fa8` |
| bytes | 8,899,204 | 8,899,204 |
| columns | 45 | 45 |
| distinct block states | 64 | 64 |
| non-air blocks | 1,414,441 | 1,414,441 |

`cmp` exit 0. The md5 also equals U18's and U19's landed figure, but that is a
*corroboration*, not the evidence — it only means anything because the harness
md5 matches theirs too, so the scene really is the same one. The evidence is the
two arms agreeing in this tree, plus the detector control above.

## RNG order

**`rng_draws[Surface]` is 0 on both arms, and that control is vacuous — do not
quote it.** `bump_rng_draw` has exactly one site,
`lodestone-worldgen-core/src/rng/mod.rs`'s `WorldgenRandom::next_bits`, and the
surface system's positional draws (`surface_depth`'s `master.at(x,0,z)`,
`Cond::VerticalGradient`'s `next_float`) go through a bare backend rather than
`WorldgenRandom`. The counter therefore reads 0 for this stage whatever the code
does, and an "unchanged at 0" claim would be a premise-false control of exactly
the species this repo keeps catching.

The live RNG evidence is instead:

* `rng_draws[Carve]` 352,859, `rng_draws[Ore]` 992,537 and
  `rng_draws[Vegetation]` 40,917 — digit-identical across arms. Those stages
  consume the surface stage's output, so a perturbed surface draw order would
  move them.
* **Byte identity of the 45-column dump**, which is a strictly stronger
  statement about draw order than any count: it fixes the values *and* their
  positions, palette order included.

`tests/ore_alloc_attribution.rs` now prints the whole per-stage entry/draw table
rather than only `rng_draws[Ore]`, so the next unit does not have to edit that
harness — a harness edit is a thing that has to be md5-matched across both arms
of a byte-identity comparison, which makes it more expensive than it looks.

## `surface_diff` is now a `FastMap`

`worldgen-fast-hashing.md` listed this map as **open** (U17: 5.5% of hash time,
1.2% of all CPU) and requires the other half of the argument to be established
**at the map**, because a hasher swap changes iteration order and this repo has
already shipped a palette permutation from exactly that.

`SurfaceDiff` takes the *never iterated* form in production. Its only production
consumer, `materialize_world`, reads it by **point lookup** in the same fixed
`(lz, lx, ly)` order as its own base fill — precisely so the `DenseBlockGrid`
palette is appended deterministically — and `surface_parity` only `get`s.

The grep was run rather than asserted, and it found **one** iteration:
`tests/surface_allocs.rs`'s distinct-result count calls `diff.values()`. That
site takes the map's *second* permitted form — it `sort_unstable`s before
reducing to a length, so the hasher's order cannot be observed — and says so at
the call. Reproduce the check with:

```
grep -rn --include='*.rs' -E 'surface_diff\.(iter|keys|values|drain|into_iter)|\.values\(\)|for .* in .*surface_diff' crates/
```

against a `SurfaceDiff` **binding**, not against a file. Note that a grep for
`surface_diff.iter()` alone misses `diff.values()` on a differently-named
binding, which is how the one real site was nearly missed here.

No speed claim is made from this. Per `worldgen-fast-hashing.md`'s own rule, a
hasher change measured across two separate binaries is not a speedup
measurement; the only defensible statement here is the categorical one (the
hasher changed, and the map is provably never iterated). If you want a timing,
`docs/plans/worldgen-cycle-accounting.md` is how to get one.

## How to change it

* **Adding a `Cond` or `Rule` variant**: a result state must be interned in
  `RuleParser`, never in `try_apply`. If a new rule computes a name, ask whether
  the set it computes over is finite — `Bandlands` looked like it was not and
  was.
* **Changing what `pre` returns**: `PreState` is 4 bytes and `Copy`; keep it
  that way. If a future ore-vein field needs to carry a per-position state into
  this seam (`worldgen-ore-lookup-cost.md` predicts exactly that), `PreState` is
  already the right shape — a vein state is an id, and its class is `Stone`.
* **Do not aim an allocation gate at the warm per-column counter.** Fill,
  surface, carve and ore all read **0** there because the staged store serves
  them from cache. Use a cold sweep, or drive the stage directly as
  `tests/surface_allocs.rs` does.
* `top_material` still returns `Option<String>` because the **carver seam** was
  left on strings deliberately (out of scope for #501). It is
  allocation-neutral by construction — the pre-U21 body allocated one `String`
  for the biome and one for the matched state, this one allocates one for the
  state and none for the biome — so carve can only go down. It measured
  *unchanged* at 101,651, which means this scene never calls `top_material` at
  all: no carved block exposed dirt in those nine chunks. That is a coverage
  gap in the *scene*, not in the code; `carver_parity` is what exercises it.

## Deliberately left

* **A dense array instead of the diff map.** `worldgen-fast-hashing.md`'s
  headline rule is to prefer an array where the key space is dense and bounded,
  which `(i32,i32,i32)` over one chunk is. The numbers now exist to size it: a
  chunk's diff holds ~18,200 entries and the map's final table is **557,064
  bytes**, whereas a dense `16 × 384 × 16` array of `StateId` is **196,608
  bytes** — smaller, and **one** allocation instead of fourteen. So it would
  take the surface stage from 690 to ~49 and remove the hashing entirely.
  It is not done here because it is a second byte-identity run and a sentinel
  question ("unchanged" is not "air"), and because the remaining 690 is 0.6% of
  a scene whose largest term is now carve's 101,651.
* **The carver seam** (`top_material`, `carver::CarveEnv`'s `Option<String>`).
  Carve is now the **largest** allocation term in the pipeline at 101,651
  (94.6% of the post-U21 total), and its own site table attributes 71.8% to
  `CarveEnv::carve_block` under `create_tunnel` — a different cause from this
  unit's, and a different file.
* **`top_layer::StatePredicate`**, keyed by `String` (U17: 5.3% of hash time).
  Surface-adjacent, named in `worldgen-fast-hashing.md`, untouched here.

## Configuration

None. No feature flag, no env var, no new dependency — the conversion needed
none, and `memchr`/`smallvec`/`bumpalo` were all evaluated and rejected (see
below).

## Dependencies

No dependency was added, and that is a measured conclusion rather than a
preference:

* **`memchr`** — `is_fluid`'s `split('[')` survived into `class_of_name`, but
  `split` **does not allocate**, so it appears nowhere in an allocation
  attribution. A CPU question, not this one.
* **`smallvec`/`arrayvec`** — nothing here is a small container. The clay-band
  table is fixed at 192 and built once per world; the diff holds ~18,200
  entries.
* **`bumpalo`** — an arena suits short-lived *heterogeneous* allocations. After
  the conversion this stage makes 14 allocations of one shape, so there is
  nothing for an arena to amortise. Had the residual been many small
  heterogeneous strings it would have been the right answer; it is not.
* **`hashbrown`/`rustc-hash`** — `hash/fast.rs` already exists and documents its
  own refusal to take these as dependencies.

Internal: `crate::interner` (`StateId`, `StateInterner`),
`lodestone_worldgen_core::hash::FastMap`, `crate::counters` for the stage guard.

## Note on ownership

`crates/lodestone-worldgen/src/overworld/mod.rs` is edited by this unit even
though it is a cross-unit choke point. It was unavoidable: `SurfaceSystem::new`
must be handed the generator's `StateInterner`, which was constructed *after* it
in the `Self { .. }` literal, and the three `default_*_pre` fields have to live
next to the strings they are derived from. The edit is four hunks and touches
nothing else in that file.
