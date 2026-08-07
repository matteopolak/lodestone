# Walking away from spawn: the staged store's dead retention ceiling

## What it is

The measured diagnosis of a player-visible defect — the game getting steadily worse the further
you walk from spawn — **and the record of the fix that closed it** ([#503](https://github.com/matteopolak/lodestone/issues/503)).
Per-column generation cost is **flat in distance from the origin**; what grew was *memory*.
`lodestone-worldgen`'s staged chunk store had a documented 512-entry retention ceiling that was
**never enforced on the generation path**, so the store grew linearly and without bound at
**21 entries (~7.9 MiB) per chunk of travel** for a normal 17×17 view. The instrument is
[`crates/lodestone-server/tests/walk_distance_curve.rs`](../crates/lodestone-server/tests/walk_distance_curve.rs).

`open_view` now checks the ceiling once its whole box is pinned. A 1,600-block stroll went from
**2,541 entries / 997.2 MiB and zero evictions** to a flat **512 entries / 324.6 MiB and 2,029
evictions**, byte-identical output. "The fix, as landed" below has the measurements; the change rules
live in [`worldgen-staged-store.md`](./worldgen-staged-store.md).

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

### What the replacement gate had to do differently

`a_view_walked_across_the_world_stays_inside_the_retention_ceiling`, in the same file, drives
`open_view()` over a **moving centre** — the one thing no existing test did. It is hermetic and costs
0.03 s, so it runs on every `cargo test`, and its `visit_column` helper reproduces `column()`'s real
store traffic (the 5×5 pin, pre-ore over that 5×5, post-ore over the 3×3) rather than a convenient
approximation.

The evidence that the model is faithful is not an argument, it is a coincidence too large to be one:
the hermetic gate's curve matched this doc's live-server measurement **digit-for-digit on all twelve
numbers** — 441 / 861 / 1,281 / 1,701 / 2,121 / 2,541 with zero evictions unfixed, and a flat 512 with
349 / 769 / 1,189 / 1,609 / 2,029 evictions fixed. A `u64`-payload model in `lodestone-worldgen` and a
real embedded-data server sweep in `lodestone-server` are independent implementations of the same
geometry, and they agree exactly.

The control was **built and observed failing before the fix existed**: on unmodified `open_view` the
gate reports `store_len 861 at step 20 exceeds 537 … Predicted 2541 if reclamation never runs`.

## The fix, as landed

Applied exactly as specified below. `open_view` checks the ceiling **after** the whole box is pinned,
in `store.rs`:

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
Spelled out, because it is the one thing a future reader is likely to "tidy": a reclaim pass skips
pinned entries, so after the loop this scope's closure is ineligible **by construction**. At iteration
*k* only *k* of the box's 25 entries carry a pin, and the unpinned remainder are typically the *oldest*
entries in the whole store — the neighbouring column visited a moment ago — so they sort to the **front**
of the candidate list. A pass inside the loop would evict precisely the slots the request is about to
compute into, turning a memory bound into a guaranteed hot-path recompute. `reclaim()` holds no lock
when called, so nothing pulls in the other direction.

### What it measured after landing

`view_walk_curve`, the same instrument and the same 17×17 view, release, this machine:

| blocks walked | `store_len` before → after | evictions after | RSS before → after |
|---|---|---|---|
| 0 (join view) | 441 → **441** | 0 | 208.9 → 208.5 MiB |
| 320 | 861 → **512** | 349 | 371.3 → 287.7 MiB |
| 960 | 1,701 → **512** | 1,189 | 681.8 → 324.3 MiB |
| 1,600 | 2,541 → **512** | 2,029 | 997.2 → **324.6 MiB** |

`store_len` plateaus at exactly the ceiling and the 21-entries-per-step slope moves *wholly* into
`evictions` — 349 at step 20 to 2,029 at step 100 is 21.0 per step, so nothing was lost or double
counted. **RSS is flat from step 60 on**: 324.3 → 324.5 → 324.6 MiB, +0.3 MiB across 640 blocks,
against +315 MiB over the same stretch before. The marginal rate is **504 KiB per block → 0.48 KiB per
block**. The instrument's own `kib_per_block` column now falls as `1/d`, which is the signature of a
constant rather than a rate, and is the shape to look for if this is ever re-measured.

The join view still evicts **zero**, which is why all four eviction-is-zero gates were unperturbed
(`staged_store_counters` 256/0, `staged_store_gates` 441/0, `in_place_decoration_counters` 25/0,
`join_scheduler_counters`) — 441 is under 512 by derivation, so a correct fix cannot move them.

**Byte identity**, two arms of the same tree differing only in `open_view`'s ceiling check, md5-verified
identical harness on both:

| dump | arm A (reverted) | arm B (fixed) | md5 |
|---|---|---|---|
| 140-column strip, 20 columns sampled | `store_len=720` evictions **0** | `store_len=512` evictions **208** | both `6d80318bab2d514416cba1dce0216f52` |
| `u15_column_dump`, 45 columns / 5 seeds | — | — | both `a9db7cf741214167db615fa8b9356fa8` |

Arm A's 720 is exactly the predicted `5 · (140 + 4)`, and `720 − 512 = 208` is exactly arm B's eviction
count, so the two arms account for the same entries and differ only in retaining them. Detector control
on both dumps: one bit flipped at offset 2,000,001 is reported `differ: char 2000002`, exit 1. The u15
hash matches the figure U18, U19 and U21 all recorded.

**The u15 dump is, on its own, vacuous for this change** and is reported only as the repo's standard
bar. Its scene is a fresh generator per seed over a 3×3 patch — a 49-entry closure — so it never
reaches the ceiling on *either* arm and would be byte-identical whatever `open_view` does. That is the
same *world* species that hid the defect, one level up, which is why the strip dump exists.

**Determinism under concurrent reclamation.** A 21×21 = 441-column burst on 8 threads, closure 625
against the 512 ceiling, every column compared to a serial arm: identical. The two arms reclaimed on
genuinely **different schedules** — 113 evictions serial against 577 parallel — so the result does not
depend on *when* entries were dropped.
`parallel_generation_is_deterministic_and_matches_serial` ran **12/12 green**, each run verified to
have matched exactly one test (`1 passed`) rather than silently filtering to none.

### Was the leak → pressure → slowdown chain closed? No.

Still **the leading explanation, not a completed chain**, exactly as first labelled. What is now also
true is that the chain's *premise* has been removed: the `O(d)` term is gone whether or not memory
pressure was the mechanism by which the owner felt it, so the fix does not depend on the chain.

The missing link is unchanged — the machine actually entering swap and generation slowing *as a
consequence*. Demonstrating it needs the leaking arm carried to multiple GB (≈ 5,000 blocks for 2.4 GB,
20,000 for 9.6 GB), and `CLAUDE.md` records unbounded test memory force-rebooting this 17 GB machine.
That experiment was deliberately **not** run. A cheaper wall-clock substitute was considered and
rejected on this repo's own evidence: §12.103 records a two-arm timing here **changing sign with arm
order**, and §12.104 records unchanged code moving ×1.8–2.3 between two captures because siblings were
compiling — so a fixed-vs-leaking timing at 1 GB, where a 17 GB machine is under no pressure at all,
could only produce a number that would later be attributed to the wrong cause.

Three things to check when landing it, none of which is a timing — all three checked, all three as
predicted:

- **`store_len` must plateau** near 512 in `view_walk_curve`, and `store_evictions` must become
  non-zero. Both are already printed. **Measured: flat 512, 2,029 evictions.** One consequence had to
  be repaired: `view_walk_curve`'s own non-degeneracy assertion was `store_len > 512`, which the fix
  *inverts* — it would now fail precisely because the defect it was written to expose is gone. It is
  now `store_len + store_evictions > 512`, the form `age_curve` already used, and the quantity that
  question is really about: how many entries the walk *asked for*.
- **The existing eviction-is-zero gates must stay green.** `staged_store_counters.rs` (16×16 = 256
  closure), `staged_store_gates.rs`, `in_place_decoration_counters.rs` and
  `join_scheduler_counters.rs` all assert zero evictions over closures of **at most 441** — under
  the 512 ceiling, so the fix should not perturb any of them. If one goes red, the ceiling is too
  low for that gate's closure, which is a real finding about the ceiling and not a test to relax.
  **All four green, all still reading zero.**
- **Byte identity.** Eviction changes *when* a stage is recomputed, never what it computes, so all
  13 parity binaries must be byte-identical. That is a claim worth checking rather than asserting:
  a recompute that produced different bytes would mean a stage is not a pure function of its key,
  which is the store's central assumption. **All 13 green (24 + 10 tests, 0 failed, 0 ignored), plus
  the two-arm dumps above.** Worth keeping the reasoning: the one place a strong-count change could
  have leaked into output is `decorate.rs`'s
  `Arc::try_unwrap(world).unwrap_or_else(|shared| (*shared).clone())`, whose branch depends on whether
  the store still holds the value. It is unreachable as a behaviour change on two independent grounds —
  the centre is *pinned* for the whole call so the count is never 1, and both branches yield the same
  content anyway — but it is the shape to look for if a future stage takes a mutable path.

The second-order hazard, which only becomes reachable once the fix lands: `reclaim()`
gathers candidates across all 64 shards and sorts them, `O(live · log live)` **per insert** while
over the ceiling. At `retention = 512` and ~21 inserts per chunk step that is a few thousand map
probes per step against a ~50 ms column — negligible, but it is negligible *because* the ceiling
holds, so the two changes are coupled. **Measured rather than left as an estimate**: the hermetic walk
gate performs 1,989 column visits and 2,029 evictions in **0.03 s release**, i.e. ~15 µs of
store-and-reclaim work per column, **0.03%** of a real column. That is a bound from a counter-shaped
run rather than a two-arm timing, deliberately — see §12.103 on what a two-arm timing does here.

Reclamation also introduces **no new contention**, which the store's own change rule ("no shared pool
here, ever"; `4307b59` is the scar) makes a hard requirement. The diff adds exactly one atomic
operation — a `Relaxed` load of `total`, on the insert path, behind a `fresh_inserts > 0` guard, so a
steady-state view that re-opens a resident neighbourhood does not even perform it. No lock was added;
`entry()` and `StageSlot::get_or_compute` are untouched, so an **entry hit is still a `OnceLock` atomic
load and no lock at all**. `reclaim()`'s candidate `Vec` lives on the reclaiming thread's own stack and
only one thread runs a pass at a time.

## Configuration

- `STORE_RETENTION = 512` — `crates/lodestone-worldgen/src/overworld/mod.rs`. Derived from a
  289-column join burst's 21×21 = 441 closure, so it must stay above 441.
- `COLUMN_CLOSURE_RADIUS = 2` — same file. The 5×5 pin `column()` opens. If a driver's
  neighbourhood widens, this widens with it and the per-step growth constant changes with it.
- `SHARD_COUNT = 64` — `store.rs`. Affects the `len() <= retention + SHARD_COUNT` slack only.
- The instrument's own knobs are in `walk_distance_curve.rs`: `BANDS`, `COLUMNS_PER_BAND`,
  `VIEW_RADIUS`, `VIEW_WALK_STEPS`. All four tests are `#[ignore]`d and print a curve rather than
  asserting a threshold.
- The regression gate's knobs are in `store.rs`'s test module: `WALK_RETENTION`,
  `WALK_CLOSURE_RADIUS`, `WALK_VIEW_RADIUS`, `WALK_STEPS`. The first two **restate private constants**
  from `overworld/mod.rs` and carry the same duplicated-constant hazard `walk_distance_curve.rs`
  documents — they are only ever used as an upper bound and as the predicted closure, so a stale value
  weakens the gate rather than breaking it, but re-read `mod.rs` before trusting them.
- `STRIP = 140`, `SAMPLE = 20` and `BURST_RADIUS = 10` in
  `crates/lodestone-server/tests/store_reclaim_identity.rs`, all derived from `STORE_RETENTION` via the
  `5 · (n + 4)` strip closure and the `(2R + 5)²` burst closure. **Shortening `STRIP` below ~100
  silently stops reaching the retention path**, and the gate then goes green while measuring nothing —
  which is why it asserts `evictions > 0` rather than trusting the constant.
- `LODESTONE_STORE_WALK_DUMP` — output path for the two-arm dumper in that file; unset means the
  dumper panics rather than skipping, on purpose.

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
