# Walking away from spawn: the staged store's dead retention ceiling

## What it is

The measured diagnosis of a player-visible defect — the game getting steadily worse the further
you walk from spawn. Per-column generation cost is **flat in distance from the origin**; what
grows is *memory*. `lodestone-worldgen`'s staged chunk store has a documented 512-entry retention
ceiling that is **never enforced on the generation path**, so the store grows linearly and without
bound at **21 entries (~7.9 MiB) per chunk of travel** for a normal 17×17 view. The instrument is
[`crates/lodestone-server/tests/walk_distance_curve.rs`](../crates/lodestone-server/tests/walk_distance_curve.rs).

This doc is the record of the measurement and the reasoning. The fix is **not** in it: the term
lives in `crates/lodestone-worldgen/src/overworld/store.rs`, and the patch is in "The fix" below
for whoever owns that crate to land.

## The curve

Three arms, all in `walk_distance_curve.rs`, `--release`, this machine. Read the control first.

**Control — distance held fixed** (`control_distance_held_fixed`): nine fresh generators, nine
six-column walks, every one at the origin. Warm mean **61.5–62.4 ms**, spread ratio **1.01×**,
zero evictions. The instrument is stable, so a slope in the other arms means something. This
matters more than it looks: `CLAUDE.md` records this machine reproducing a worldgen wall-clock
figure to only 10.8%, and a 22% swing across three runs of an identical binary. A mean-of-three
inside one tight loop is a far better instrument than a whole-stage timing, and the control is how
we know that rather than assume it.

**Coordinate varies, generator age held fixed** (`distance_curve`): a fresh generator per band, six
columns per band, bands at chunk 0 … 65,536 (0 … 1,048,576 blocks).

| blocks from origin | cold column | warm mean | vs band 0 (warm) |
|---|---|---|---|
| 0 | 258.8 ms | 61.8 ms | 1.00× |
| 1,024 | 258.0 ms | 62.2 ms | 1.01× |
| 4,096 | 257.9 ms | 53.2 ms | 0.86× |
| 65,536 | 296.6 ms | 60.9 ms | 0.99× |
| 262,144 | 307.6 ms | 67.4 ms | 1.09× |
| 1,048,576 | 200.2 ms | 45.8 ms | **0.74×** |

**Flat.** At a million blocks out a column is *cheaper* than at spawn — that is terrain variation
(different biomes do different amounts of vegetation and ore work), not a distance term. The whole
0.74–1.09× span is smaller than the effect the report describes, and it has no monotone trend.
**There is no `O(distance)` CPU term in `OverworldGenerator::column`.**

**Generator age varies** (`age_curve`, and the realistic `view_walk_curve`): one generator, as
production has it — `OverworldChunkSource` builds one per world and keeps it for the world's life.

`view_walk_curve`, sliding a 17×17 view 100 chunk steps:

| blocks walked | `store_len` | evictions | RSS |
|---|---|---|---|
| 0 (join view) | 441 | 0 | 208.9 MiB |
| 320 | 861 | 0 | 371.3 MiB |
| 960 | 1,701 | 0 | 681.8 MiB |
| 1,600 | 2,541 | **0** | 997.2 MiB |

Exactly **21 entries per chunk step**, monotone, **zero evictions ever**, against a 512-entry
ceiling — 5× over it by the end of a 1,600-block stroll. RSS tracks `store_len` linearly at
**~7.9 MiB per chunk step = 504 KiB per block walked**.

`store_len = 441` immediately after the join view is the *same* number Unit 6 recorded and read as
healthy, and it was: 441 is exactly a 289-column view's 21×21 pre-ore closure. The defect is not
that it reaches 441, it is that it never comes back down.

## Why: `open_view` inserts, and only `entry` reclaims

Two insertion paths into `StagedStore`, and only one checks the ceiling.

`entry()` checks it, on a miss:

```rust
if inserted {
    let total = self.total.fetch_add(1, Ordering::Relaxed) + 1;
    if total > self.retention { self.reclaim(); }
}
```

`open_view()` inserts, counts, and never checks — it does `self.total.fetch_add(1, …)` for each
fresh slot and calls nothing.

Now the production call shape, `OverworldGenerator::column`:

```rust
let _view = self.store.open_view((cx, cz), COLUMN_CLOSURE_RADIUS);  // pre-creates the whole 5×5
let cached = self.pre_ore_stage(cx, cz);
```

`open_view` **pre-creates every slot in the 5×5 box first**. The only two `entry()` call sites —
`pre_ore_stage` and `post_ore_world` — are reached only from inside `column()`, and only for
coordinates inside that same box, because `COLUMN_CLOSURE_RADIUS = 2` *is* the pre-ore closure. So
`inserted` is **always false**, the ceiling branch is never taken, and **`reclaim()` never runs in
a real session at all.** It is dead code on the only path the game uses.

That is why `store_evictions()` reads 0 forever, and it is a stronger failure than the O(store)
reclaim thrash one might expect from reading the ceiling check: there is no thrash because there is
no reclamation.

### The order-of-growth, predicted rather than fitted

Walking `+x` by one chunk with view radius `R` exposes a leading strip of `2R + 1` new columns.
Each opens its own radius-2 pin, so the store's footprint gains a strip of `2R + 1 + 4` entries.
At `R = 8` that is **21** — which is exactly the measured slope. The term is **`O(d)`, linear in
distance travelled, not `O(d²)`**, and the constant is `2R + 5` entries per chunk.

Per-entry memory, also derived rather than fitted: of the 5 entries a 1-D step adds, 5 get a
pre-ore grid and 3 get a post-ore grid, at ~192 KiB each, so the mean is `8 × 192 / 5 ≈ 307` KiB.
Measured 323 KiB per entry. Agreement within 5% from an expectation computed outside the
measurement is what makes this a term rather than a correlation.

## Why this presents as *slowdown*, and the limit of the evidence

Extrapolating 504 KiB per block: 5,000 blocks ≈ 2.4 GB, 10,000 ≈ 4.8 GB, 20,000 ≈ 9.6 GB. This is
a 17 GB machine, and singleplayer runs the server **in the client's own process**
(`IntegratedServer`), so the leak consumes the renderer's headroom too. Linear memory growth into
memory pressure produces superlinear wall-clock degradation across everything — which is what
"exponentially slower as I walk away" feels like.

**Be honest about what was measured.** Confirmed by measurement: the unbounded `O(d)` memory
growth, its rate, and its mechanism. **Not** measured: the machine actually entering swap and
generation slowing as a result. That last step is inference from the rate plus the machine's size,
and proving it needs a multi-GB run `CLAUDE.md` explicitly warns against on this hardware. Treat
"leak → swap → slowdown" as the leading explanation, not as a completed chain.

## Why no test caught it — a *world*-species vacuity

`store.rs` has a gate for exactly this, and it passes:

```rust
fn unpinned_entries_are_reclaimable_once_the_scope_ends() {
    let store: StagedStore<Stages> = StagedStore::new(4);
    { let _scope = store.open_view((0, 0), 1); store.entry((0, 0)).a.get_or_compute(…); }
    let before = store.evicted();
    for i in 100..600 { store.entry((i, -i)).a.get_or_compute(…); }
    assert!(store.evicted() > before);
    assert!(store.len() <= store.retention + SHARD_COUNT, "live entries {} unbounded …");
}
```

It asserts boundedness, it has a live-detector control on `evicted()`, and it is *green*. It drives
its 500 inserts through **`entry()`** — the one path that reclaims. Production drives them through
**`open_view()`** — the one that does not. The test source is exemplary; the flaw is in the input.
This is the audit question `CLAUDE.md` names verbatim: *which implementation does this test's
transport actually resolve to, and is it the one production uses?* Both reclamation gates in that
file use a call shape `column()` cannot produce, so reclamation is green in CI and dead in the game.

## The fix (not applied here — `lodestone-worldgen` is another unit's)

Have `open_view` check the ceiling **after** the whole box is pinned, in `store.rs`:

```rust
pub fn open_view(&self, centre: ChunkPos, radius: i32) -> ViewScope<'_, E> {
    let epoch = self.epoch.fetch_add(1, Ordering::Relaxed) + 1;
    let mut fresh_inserts = 0usize;
    for pos in box_around(centre, radius) {
        // … unchanged …
        if fresh {
            self.total.fetch_add(1, Ordering::Relaxed);
            fresh_inserts += 1;
        }
    }
    // Reclaim only once the whole box is pinned: this scope's own closure is
    // then ineligible by construction, which is the property that keeps
    // eviction view-scoped rather than a capacity guess. Folding this into the
    // loop above would let the pass evict a slot this very request is about to
    // compute into.
    if fresh_inserts > 0 && self.total.load(Ordering::Relaxed) > self.retention {
        self.reclaim();
    }
    ViewScope { store: self, centre, radius }
}
```

The post-pin ordering is the whole correctness argument and is why this cannot go inside the loop.

Three things to check when landing it, none of which is a timing:

- **`store_len` must plateau** near 512 in `view_walk_curve`, and `store_evictions` must become
  non-zero. Both are already printed.
- **The existing eviction-is-zero gates must stay green.** `staged_store_counters.rs` (16×16 = 256
  closure), `staged_store_gates.rs`, `in_place_decoration_counters.rs` and
  `join_scheduler_counters.rs` all assert zero evictions over closures of **at most 441** — under
  the 512 ceiling, so the fix should not perturb any of them. If one goes red, the ceiling is too
  low for that gate's closure, which is a real finding about the ceiling and not a test to relax.
- **Byte identity.** Eviction changes *when* a stage is recomputed, never what it computes, so all
  13 parity binaries must be byte-identical. That is a claim worth checking rather than asserting:
  a recompute that produced different bytes would mean a stage is not a pure function of its key,
  which is the store's central assumption.

Also note the second-order hazard, which only becomes reachable once the fix lands: `reclaim()`
gathers candidates across all 64 shards and sorts them, `O(live · log live)` **per insert** while
over the ceiling. At `retention = 512` and ~21 inserts per chunk step that is a few thousand map
probes per step against a ~50 ms column — negligible, but it is negligible *because* the ceiling
holds, so the two changes are coupled.

## Configuration

- `STORE_RETENTION = 512` — `crates/lodestone-worldgen/src/overworld/mod.rs`. Derived from a
  289-column join burst's 21×21 = 441 closure, so it must stay above 441.
- `COLUMN_CLOSURE_RADIUS = 2` — same file. The 5×5 pin `column()` opens. If a driver's
  neighbourhood widens, this widens with it and the per-step growth constant changes with it.
- `SHARD_COUNT = 64` — `store.rs`. Affects the `len() <= retention + SHARD_COUNT` slack only.
- The instrument's own knobs are in `walk_distance_curve.rs`: `BANDS`, `COLUMNS_PER_BAND`,
  `VIEW_RADIUS`, `VIEW_WALK_STEPS`. All three tests are `#[ignore]`d and print a curve rather than
  asserting a threshold.

## What was ruled out

Recorded so it is not re-derived. Two independent read-only sweeps plus the measurements above:

- **Generation is anchored at the player, not at `(0, 0)`** — the owner's own hypothesis, and it is
  false. `ViewTracker::window` is `center ± radius`, `recenter` diffs
  `next.difference(&self.loaded)`, and `join_view_rings` walks outward from the player's column.
  Instrumented rather than merely read:
  `serve_play.rs`'s `generation_is_anchored_at_the_player_not_at_the_origin` records every
  coordinate a real `serve_connection` asks for during a recenter **80,000 blocks** from spawn and
  asserts the set is exactly the player's `(2r+1)²` window. Its detector control — anchoring
  `window()` at `(0, 0)` — was observed failing.
- **No `O(|coord|)` term anywhere in `lodestone-worldgen` or `lodestone-worldgen-core`.** Every
  loop bound, allocation size and index is a compile-time constant or a *difference* of
  coordinates. `wrap` in `noise/perlin.rs` folds large coordinates in closed form; every
  positional RNG seeding is a hash, not an iteration; `biome/memo.rs` masks to a fixed 1024-slot
  table.
- **`ChunkStore`'s 512-entry LRU, `border.rs`, the random-tick pass, the mesher's dirty sets, and
  `sim/**`** — all bounded by view size, not by distance or by session age.

## Open leads (read-only findings, **not** measured)

Found while sweeping, plausible, and each is its own unit. Do not treat as established:

- **`BlockEntityRegistry` never releases and is fully scanned every tick.**
  `block_entities.rs`'s `tick_all_with_hopper_lock` collects `self.entities.keys()` into a fresh
  `Vec` at 20 Hz from `tick.rs`, and the registry has no unload path at all (`remove` is
  documented as not-yet-wired; `region_source.rs` says outright it has no eviction). The sharp
  edge: for each hopper the tick closure calls `world.block_state(…)`, and if that column has aged
  out of `ChunkStore`'s LRU — which walking far away is exactly what does — the call regenerates a
  whole column **on the tick thread**, which `chunk_store.rs` itself documents at 222–909 ms
  against a 50 ms budget. If real, this is a *second*, CPU-side term that also grows with
  exploration, and unlike the store leak it does not need memory pressure to hurt.
- **Region files are re-read and fully re-parsed on every single column load.**
  `region_source.rs`'s `load` does `std::fs::read(&path)` then `RegionFile::parse` per column, with
  no open-file or parsed-region cache — 1024 full parses of the same multi-MB file to load one
  region's worth of columns.
- **The save path is `O(chunks written × registry size)`.** `extras_for` scans the whole
  block-entity registry per chunk written, and the write set is extended with every
  block-entity-bearing chunk, so it is quadratic in a quantity that grows with exploration.
- **`Scratch::reconfigure`'s reuse check is keyed on *absolute* bounds**, so
  `self.config == Some(want)` is false for every chunk and the documented allocation-reuse path is
  never taken for the aquifer's bounded sampler. A per-chunk constant, not a distance term, but it
  silently defeats a pool a module doc calls load-bearing. Storing the bounds *span* rather than
  the absolute box would fix it; the shapes are already relative.

## Dependencies

`lodestone-worldgen` (`overworld::OverworldGenerator`, `overworld::store::StagedStore`) and
`lodestone-server` (`overworld_generator`, `OverworldChunkSource`, `ViewTracker`,
`serve_connection`). The instrument needs `lodestone-server`'s **embedded** worldgen data, not
`lodestone-worldgen`'s test fixtures: the fixtures carry no biome documents, so both 3×3 drivers
early-return and a store measured against them would grow at the wrong rate entirely.
