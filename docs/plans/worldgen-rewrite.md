# Worldgen rewrite plan

## What it is

The plan for rewriting `crates/lodestone-worldgen`'s generation engine from scratch for speed —
targeting sub-millisecond steady-state serial chunk generation at bit-exact vanilla 26.2 parity —
plus the jar-derived inventory of everything full parity requires that the repo does not have yet
(structures, Nether/End, ore veins, the missing decoration steps, 3-D biomes, world presets), each
item blocker-classed. Owner-directed (2026-08-06/07). Planning artifact only; each unit below is a
separately landable piece of work with its own evidence standard.

## Status and ground rules

- `HEAD` at planning time: `4307b59`. The workspace is on **nightly**, moved by the owner
  specifically so `#![feature(portable_simd)]` is available. There is **one** SIMD implementation —
  no `#[cfg]` scalar fallback, ever: a dual path is two worlds from one seed waiting to happen.
  **Settled (2026-08-07):** the toolchain is pinned to `channel = "nightly-2026-08-07"`
  (rustc `84b36a78a`); renewing the pin is a deliberate, single-commit act. Trap for whoever
  renews it: the dated channel is a **publish** date, and adjacent days are different compilers —
  `nightly-2026-08-06` (rustc `7608eb7b0`) ICEs on tokio at `opt-level=3`, breaking release
  benches. Release builds and benches are verified clean on the pinned compiler; verify the same
  two things before landing any future pin bump.
- **Implementation mandate (owner ruling, 2026-08-07): performance work may rewrite whatever it
  needs, in any crate.** Units are *scoped* to the clusters below for scheduling, but the mandate
  is not confined to `lodestone-worldgen`: encodings, data layout, the serve boundary
  (`GeneratedColumn`, `ChunkSource`, `ChunkColumn`), palette/section storage, bit-packing, and the
  protocol-side chunk encode are all fair game where measurement says the win is there. Wider
  blast radius means the guard rails matter more, not less:
  - Parity, the gate set, and the cutover rollback rule (Q4.5) are unchanged and absolute — more
    reach is more ways to change the world by accident.
  - The health standard is **`cargo test --workspace --no-fail-fast`**, never `cargo check`
    (a check cannot see an assertion). Baseline at planning time: **6647 passed / 0 failed /
    403 ignored across 536 binaries** — each landing preserves 0 failed and explains any delta
    in the other counts.
  - A unit that touches wire-facing encoding needs evidence originating **outside our own
    encoder** — captured server bytes or a real vanilla client, never a round-trip; the recorded
    scar is hermetic self-encoded fixtures passing while a live gate produced 49 × "unexpected
    end of input".
  - Cross-crate changes still land as one reviewable commit per coherent change, with the
    boundary change separable from the optimisation that motivated it.
- **Architecture authority (owner grant, 2026-08-07):** structural change — splitting files,
  extracting sub-crates — is explicitly authorized where it reduces contention or improves
  maintainability. In this plan that is not a nicety: `overworld.rs` is the single file that made
  the engine middle a pipeline, so decomposition is a scheduled unit (U16) placed ahead of the
  units it unblocks, with its own discipline — pure-move commits, gated exactly like logic
  changes, never landed together with one. See [Decomposition](#decomposition-u16-detail).
- Parity is **bit-exact**: the placement engine reproduces vanilla's depth-first `flatMap`
  RNG-consumption order, and the existing gates (`lodestone-worldgen-parity`'s composed fixture,
  the per-stage `*_parity.rs` suites, `FeatureOracle`/`VegetationOracle`) are the definition of
  correct. Every unit says whether it can move an RNG draw, and the ones that cannot prove it.
- Where the two goals genuinely conflict, this plan says so (see [the verdict](#q3-is-sub-ms-serial-generation-achievable-at-11-parity))
  rather than quietly choosing. The owner has asked to revisit against real measurements.

## Q1: What is wrong with the current architecture

Measured or directly cited defects, not adjectives. Cites are to the tree at `4307b59`.

- **D1 — per-block recursive interpretation of a boxed enum tree.** `Density` is an AST of ~30
  variants with `Box<Density>` children (`density/mod.rs`); `NoiseChunkSampler::eval`
  (`density/chunk.rs`) re-walks it for every query, and `fill_stage` (`overworld/fill.rs`)
  makes **98,304 `AquiferSystem::block_at` calls per chunk**, each walking `final_density` plus up
  to 7 more trees. Cell interpolation exists and matches vanilla's 4×8×4 corner scheme, but it is
  implemented as memoised *point queries* (8 corner lookups through `RefCell`-guarded per-slot
  caches per block) where vanilla prefills each cell once. **Corrected (`08f5d98a`, DESIGN.md
  §12.101): the arithmetic was already right and this paragraph previously prescribed the wrong
  fix.** Vanilla has *two* interpolation orders — `Mth.lerp3` (X-inner) under `fillingCell`, and
  the incremental `advanceCellX`/`updateForY` chain (Y-inner) — different IEEE 754 expressions,
  and the block field only ever sees the first, because `NoiseChunk.java` wraps
  `final_density` in a code-only `cache_all_in_cell` whose cache is prefilled under
  `fillingCell == true`. `chunk.rs` implements exactly that; its module doc claimed the opposite
  (now fixed, with a standing inverted guard). The defect here is the per-block *lookup cost*,
  not the nesting: branchy dispatch + pointer-chasing + hash-or-index lookups per block, where
  vanilla pays a sliding lerp over a prefilled cell.
- **D2 — `String` block states end-to-end.** `DenseBlockGrid` palette-interns but `get`/`set`
  traffic in `&str` with a `HashMap<String,u16>` probe per write (`dense_grid.rs`).
  `stitch_region` (since removed — see "How to change it") copies all 9 sources' full 16×384×16 grids into a fresh
  48×384×48 `RegionGrid` string-by-string — ~2.8M get/set pairs per `column()` **even when every
  cache hits** — and `stitch_veg_region` (since removed — see "How to change it") calls `state.to_string()` per cell:
  ~885k heap allocations per column, warm. Vanilla decorates in place and copies nothing.
- **D3 — per-chunk reconstruction of per-seed state.** `build_aquifer` (`overworld/fill.rs`)
  deep-clones **eight full density trees** per chunk and rebuilds fresh slot caches; nothing is
  pooled or reused across chunks (fresh `Vec`s, grids, palettes, sparse-diff `HashMap`s per call).
- **D4 — the 3×3-of-3×3 neighbourhood recursion, blunted by two global-mutex FIFO caches.**
  Vegetation needs 8 neighbours' *post-ore* worlds; each of those needs its own 3×3 pre-ore
  neighbourhood, so one cold `column()` touches a 5×5 = 25-chunk pre-ore region, 9 ore RNG walks,
  and ~17 `pre_ore_stage` lookups (revert `4307b59`'s own numbers). The caches
  (`PreOreCache`/`PostOreCache`, `overworld/store.rs`) are each one `Mutex<HashMap + VecDeque>`,
  FIFO-evicted at 512: a 289-column join burst produced ~5000 concurrent lock attempts on one
  `Arc<Mutex>` and forced the per-ring barrier back in (`4307b59` reverting `5104adf`). A mutex
  only excludes code that takes it — and then becomes the reason nobody restructures the thing it
  guards. The barrier is a workaround for the cache, and the cache is a workaround for the
  recomputation; the recomputation itself is the defect.
- **D5 — an uncached O(7,594) biome search in the hottest loop.** `carve_stage` resolves a carver
  biome for each of a **17×17 = 289 source-chunk neighbourhood** per chunk
  (`carver/mod.rs`, `NEIGHBOURHOOD_RANGE = 8`), and each resolution is a brute-force
  nearest-neighbour scan over the ~7,594-row climate parameter table (`biome/mod.rs`'s `nearest_biome` explicitly
  declines vanilla's RTree as "already fast"). That is ~2.2M squared-distance comparisons per
  pre-ore chunk, ×25 on a cold column, with zero memoisation even between two adjacent chunks
  whose 289-source windows overlap almost entirely.
- **D6 — no counters, so every past performance story here was a timing.** The stage-split bench
  is good (10 buckets, non-vacuity floors) but nothing counts evaluations, allocations, or cache
  hits, which is how a 9× structural regression was discovered by a 700.57s test run
  (`overworld.rs` module doc) instead of by an assertion.

What is **not** wrong, and must be preserved: the parity method (per-stage JVM oracles + composed
fixture + generate-or-assert), the `Resolver`/version-free data seam (DESIGN.md §12.30: worldgen is
data), the RNG primitives (`WorldgenRandom`, generic not dyn, proven), and the depth-first placement
engine's *semantics*.

### Target architecture

1. **Numeric block-state ids everywhere inside the engine.** A per-generator interned state table
   built once from the resolver (`u16` ids; `GeneratedColumn`'s palette derived at the serve
   boundary). No `String` in any per-block path.
2. **A flattened density graph.** The `Density` tree compiles (per seed, once) into a contiguous
   `Vec` of components addressed by index — children are indices, not `Box`es — with per-chunk
   evaluation state (interpolator planes, flat-cache tables, cell caches) in reusable buffers owned
   by a per-chunk scratch object, not `RefCell`s inside the sampler. Evaluation fills each cell's
   corner lattice once and interpolates in **`Mth.lerp3`'s X-inner order — the order
   `cache_all_in_cell` selects for the block field (`08f5d98a`, §12.101) — never the
   `advanceCellX`/`updateForY` incremental chain**, which is a different IEEE 754 expression:
   measured, the two nestings differ on 60,300 of 393,216 blocks, and swapping them takes
   `chunk_parity` from 98,304/98,304 to 90,563/98,304. (PumpkinMC independently converged on the
   cell-fill shape; see Q2.)
3. **A staged per-chunk store instead of caches.** Each chunk's intermediate products (pre-ore
   grid + heights + biomes; post-ore grid) live in a sharded store keyed by chunk pos with a
   per-entry stage state machine, retained while any in-flight request's neighbourhood needs them
   (view-radius-scoped, not FIFO-guessed). The 3×3 drivers keep their exact proven semantics and
   canonical source order — the store only guarantees each stage computes **once**, and the
   "compute exactly once per chunk per stage" property becomes a counter assertion, not a hope.
   No global mutex; per-shard or per-entry locking, contention bounded by real dependency edges.
   This is what deletes the per-ring barrier structurally (its rationale — "ring 0 seeds the
   cache" — stops describing anything).
4. **In-place region decoration.** Decoration writes go through a region view borrowing the 3×3
   (or 5×5) chunks' dense grids directly, routing writes by coordinate — no `RegionGrid` stitch
   copies, no `VegGrid` re-seeding, no fold-back pass.
5. **Biome resolution as a first-class cached layer**: per-source-chunk carver/ore biome memoised
   in the store entry, and the climate search itself ported to vanilla's RTree (which is also the
   gate for 3-D biome sampling, #405's deferred half).
6. **SIMD (`std::simd`) in the numeric kernels only** — lane-parallel across independent lattice
   positions, never across an accumulation chain. See [SIMD policy](#simd-policy).

## Q2: What PumpkinMC teaches, and what it does not

Read at `caf954d17043e1f618f4afe254cbcd479492d80b` (full survey at that sha). Notably: Pumpkin has
**no SIMD anywhere** (scalar Perlin/simplex, verified by grep) and **no published benchmark
numbers** (criterion benches exist, no claims) — its speed comes entirely from structure, which
both validates the D1–D3 diagnosis and means SIMD is headroom Pumpkin left on the table.

**Adopt (parity-safe engineering shape):**

- **Flattened component stack.** Pumpkin's density DAG is a topologically-sorted flat
  `Box<[ProtoNoiseFunctionComponent]>`; nodes hold integer indices into the same array, never
  pointers, and evaluation recurses by re-slicing (`proto_noise_router.rs`,
  `chunk_noise_router.rs`). Per-seed immutable proto router; per-chunk instantiation borrows
  the shared nodes by reference and allocates only the small mutable wrappers. This is
  target-architecture item 2.
- **Vanilla's wrapper set, ported as real machinery**: `DensityInterpolator` (literal port of
  vanilla's 8→4→2→1 trilinear reduction with start/end buffer swap per X-slice), `FlatCache`
  (**eagerly pre-filled at construction** — no cache-miss branch in the hot loop), `Cache2D`,
  `CacheOnce`, `CellCache`, with a batched `fill()` API — columnar evaluation, the seam SIMD
  wants. `populate_noise` reproduces vanilla's **exact cell nesting and iteration order**
  (`sample_start_density → cell_x → cell_z → cell_y.rev() → local y/x/z lerps → swap_buffers`),
  with cell dims read from `NoiseSettings` data, not hardcoded.
- **Thread-local buffer pools** (`F64_BUFFER_POOL` free-list) for interpolator scratch — the
  arena-reuse answer to D3.
- **Flat numeric chunk representation**: `ProtoChunk { flat_block_map: Box<[BlockStateId]>,
  flat_biome_map: Box<[u8]>, four [i16; 256] heightmaps, stage: StagedChunkEnum, carving_mask }`
  (`proto_chunk.rs`). Target items 1 and 3.
- **Staged generation as an explicit dependency graph**: an 11-stage `StagedChunkEnum` mirroring
  vanilla `ChunkStatus`, per-stage radius/dependency tables in one auditable place
  (`get_direct_radius()` returns 1 for Features — the 3×3), a `slotmap`-backed DAG scheduler with
  in-degree tracking over a rayon pool, `dashmap` for the published chunk map, and a
  `WorldGenRegion`-analogue windowed `Cache` of `ProtoChunk`s for cross-chunk stages. Evidence
  the staged-store shape (target item 3) works in production Rust.
- **Biome search: vanilla's tree, not a rewrite.** Pumpkin's `BiomeTree` is **generated from the
  real game data's node structure** (not re-derived by its own splitting heuristic), searched with
  vanilla's exact branch-and-bound pruning plus a thread-local last-result node used to seed
  `best_dist` — vanilla's own `RTree.lastResult` locality trick. Climate sampling is RNG-free and
  pure, so search strategy is a pure-speed question. Direct blueprint for U9.
- **Per-wrapper isolation fixtures**: Pumpkin tests individual wrapper types (`CellCache`-only,
  interpolator-only) against captured vanilla chunk dumps, not just end-to-end output — a fixture
  granularity worth copying in U4, since it localises a bit-mismatch to one wrapper.
- **Existence proof for structures in Rust**: stronghold, jigsaw + jigsaw_placement, mansion,
  desert pyramid, jungle temple are implemented (~6k lines under `generation/structure/`).
  Useful as a scale estimate and module map for U14, *not* as a correctness reference.

**Reject or treat with suspicion:**

- **Build-time codegen of worldgen data** (`pumpkin-data`'s generated noise router,
  `tools/pumpkin-codegen`). Fast, but it welds one data version into the binary — our `Resolver`
  seam and version-free engine are deliberate (DESIGN.md §12.30, plan §3). We get the same win by
  compiling JSON → flat typed IR **once per generator construction**, which we already half-do;
  the defect was never "JSON is parsed" but "the IR is a boxed tree walked per block".
- **Any of its actual placement/feature behaviour as an oracle.** The confirmed divergence map,
  from its own sources: coral (`coral_claw.rs` — vanilla's `Util.shuffle` skipped, a random
  direction draw replaced by a constant), tree decorators (`attached_to_logs.rs` — positions
  unshuffled, direction hardcoded to `[0]`), and **surface rules knowingly ~1% off** — Pumpkin's
  own test asserts `mismatches <= 1060` post-surface against a vanilla dump where raw noise
  asserts `== 0` (`proto_chunk_test.rs`). Decoration and carvers have **no** vanilla-dump
  gates there at all. So: raw-terrain machinery is trustworthy engineering; everything from
  surface rules outward keeps going to our JVM oracles exclusively. Terrain *blending* is
  unfinished in Pumpkin but irrelevant to fresh-world generation.
- **Its stage reorder** (Biomes before StructureStart, vanilla runs StructureStarts first;
  `chunk_state.rs`). Safe only because biome sampling is pure; adopt vanilla's order anyway in
  U14 — cheap insurance against any structure predicate that turns out to be order-sensitive.

## Q3: Is sub-ms serial generation achievable at 1:1 parity?

**The number, defined so it can be held to:**

> **C_ss (steady-state serial cost)** — median wall time of `column(cx, cz)` over the 100 interior
> chunks of a 12×12 sweep, single thread, release profile, embedded server data (all stages real:
> shape+aquifer, biome, surface, carve, ore, vegetation, top-layer, intern), seed 42, on this
> machine (Apple Silicon, the repo's reference hardware), with the staged store warm in the sense a
> sweep makes natural (each neighbour stage computed once, by the sweep itself) and a
> counter-asserted invariant that every stage ran **exactly once per chunk** across the sweep.
> Structures excluded until built (they then join the definition explicitly). Lighting and protocol
> encode excluded (different subsystems).
>
> **C_cold (cold-region cost)** — wall time of the first `column()` in a fresh region (25 pre-ore
> chunks, 9 ore walks from nothing).

Target: **C_ss ≤ 1.0 ms**, C_cold ≤ 8 ms. **Goal, not gate** (owner ruling, 2026-08-06): sub-ms
stays the direction of travel even if a checkpoint measures it unmet. The U2 (baseline) and U4
(new density engine) checkpoints decide **how much further to push and where** — a missed target
is a recorded number plus a named next lever, never a stop condition and never a licence to weaken
parity. The only thing a checkpoint can remove from the plan is an optimisation that measurement
shows is not worth its complexity.

**Honest verdict: plausible, not proven — and it decomposes cleanly into "structural waste we can
delete" vs "irreducible parity-bound work".**

- The measured disaster figures are dominated by structural waste: the 144-chunk debug sweep at
  ~700.57s (≈4.9s/chunk debug) predates the memo caches and counts ~9× redundant pipelines; the
  ~2.2M-comparison-per-chunk biome scan (D5) and the ~2.8M-cell stitch copies (D2) are pure
  overhead vanilla does not perform at all. Deleting waste does not move an RNG draw.
- The irreducible floor at parity is: ~1,225 corner evaluations per interpolated slot per chunk
  (5×49×5 lattice) + flat-cache quart sampling + the surface-rule walk.
  **Be precise about what U4's corner win is: a lookup win, not arithmetic** — ~786,432 per-block
  corner *lookups* per chunk collapse to ~6,144 corner *evaluations*, while the lerp's
  multiply-adds are unchanged. That bounds how much of `C_ss` U4 alone can move; a flat `C_ss`
  after U4 is this accounting showing itself, not a failed implementation. The remaining floor
  also includes the 289-source carver
  probability gates + the ore and vegetation RNG walks. The RNG walks are **sequential by
  definition** — vanilla's draw order is the spec — so SIMD and parallelism cannot touch them.
  But be precise about what that argument covers: the spec fixes **which numbers are drawn and in
  what order**, not **how expensively each draw's consequences are evaluated**. Draw count is
  spec-bound; cost per draw is ours. See
  [Vegetation: cost per draw](#vegetation-cost-per-draw) — that is where the remaining headroom
  lives. U2's first release baseline now exists — recorded in **DESIGN.md §12.98**, provisional
  while re-measured counters-off on the pinned toolchain. This plan cites the section rather than
  restating the number, so the record has one home.
- **Where the goals genuinely conflict:** if, *after* D2/D3 are dead and the per-draw costs of
  the vegetation walk are driven to O(1) (bitset predicates, precompiled placement programs,
  incremental column probes — the candidates below), the walk still measures ≥1 ms in release,
  then every remaining cost is spec-bound draw count and sub-ms C_ss at strict parity is out of
  reach. Sub-ms remains the goal (see above); the recourses are throughput-shaped (the parallel
  store, view-radius pipelining), not correctness-shaped. This plan proposes no approximation
  anywhere; the checkpoint puts the number in front of the owner with the levers ranked.
- Prior probability check, stated as opinion not evidence: vanilla itself spends single-digit
  milliseconds per chunk per thread on much of this in a JIT'd JVM; a Rust engine with flat ids,
  a flattened DAG, no copies, and no allocation in the hot path beating it by ~5–10× is a
  reasonable engineering bet. Sub-ms with vegetation included is the aggressive edge of that bet.

### Vegetation: cost per draw

The candidates for making the spec-bound RNG walk cheap per draw, each with a parity verdict.
"No" is an acceptable outcome per the owner — but no candidate is refused on the sequentiality
argument alone, because that argument is about order, not cost. Each candidate lands (or is
refused with a measurement) inside U8 unless noted.

1. **Precompiled placement programs — parity-safe.** Compile each biome's placement-modifier
   pipelines once per seed into flat typed programs (fixed-arg ops in a `Vec`, resolved ids, no
   `serde_json::Value` reads, no enum-tree dispatch per attempt). Same draws, same order — only
   the interpreter around the draws changes. The feature *lists* are already resolved per seed
   (`build_biome_vegetation`); this extends that to the modifier chains and feature configs.
2. **Tag membership as bitsets over U3's numeric ids — parity-safe.** `supports_vegetation`,
   `replaceable_by_trees`, `logs`, `cannot_replace_below_tree_trunk` become fixed bitsets indexed
   by state id (≤8 KiB per tag): O(1) bit test, zero allocation, no RNG involvement. These
   predicates run enormously often — `docs/worldgen-vegetation-census.md` counts **74,745 ground
   rejections in one 136-chunk sweep**, every one of which is currently a string/hash operation.
3. **Per-column surface probes — parity-safe only if mutation-aware, and that is the trap.**
   Most scattered attempts (`random_offset` spreads ±7 xz / ±3 y) die on a heightmap/ground
   probe whose answer is column-invariant — *between mutations*. Vanilla's heightmaps update
   incrementally as decoration places blocks (a tree placed earlier in the step changes later
   `MOTION_BLOCKING` queries), so a snapshot precomputed before the step answers **stale** and is
   parity-unsafe. The safe form is vanilla's own: per-column tops maintained **incrementally on
   write** — O(1) probe, update only on the (rare) placements, never recomputed per attempt.
4. **Precomputed foliage/trunk offset tables — partially safe; precompute enumeration, never
   outcomes.** For a given drawn parameter tuple (trunk height, radius, offset), the *candidate
   position set and its iteration order* are deterministic and reusable — a per-config table
   keyed by the small parameter space. The per-position draws inside foliage placement (leaf
   skip chances etc.) are spec-bound and stay live, in order. So: table-drive the loop bounds
   and offset arithmetic; never cache anything downstream of a draw. Measure first whether the
   enumeration arithmetic is actually hot before building the tables.
5. **Cheap rejection in vanilla's order — parity-safe.** The draw always happens first (spec);
   the *test* that follows it becomes O(1) against candidates 2 and 3, and the per-position biome
   check rides U9's memoised per-quart biome instead of a climate search. Nothing about rejection
   order or count changes — only the price of each rejection.

What this does **not** cover: reducing draw count (spec-bound, refused) and cross-feature
caching of placed shapes (downstream of draws, refused — a cached tree is a wrong tree the
moment any draw differs).

## Q4: Migration strategy

Strangler pattern inside `lodestone-worldgen`, stage-by-stage, with the old engine as a bridge
oracle. The crate's existing code is itself JVM-proven, which makes "byte-identical to the old
engine on N chunks × M seeds" a legitimate intermediate gate — cheap to run everywhere — with the
real JVM fixtures re-run at each landing as the anchor (old-engine-equality alone would just
propagate a shared misunderstanding; both gates run, always).

1. New engine grows in **new files** (`src/engine/` cluster) while `overworld.rs` keeps working —
   no shared-file contention with concurrent agents until a cutover commit, each of which is small
   (swap one stage's internals behind the existing private stage method).
2. Cutovers happen **one stage at a time**, innermost first (density evaluation → fill; then
   representation; then store; then decoration medium), each landing with:
   `column_is_byte_identical_across_two_independently_constructed_generators`, the full
   `*_parity.rs` suites, `lodestone-worldgen-parity`'s composed fixture gate, and the two
   production-seam vegetation gates in `worldgen_data.rs` — all green in the same commit.
3. The old path for a stage is deleted **in the cutover commit**, not left as a fallback — two
   live paths is the two-worlds hazard again, and `cargo xtask connectedness` cannot see a
   crate-internal island (CLAUDE.md §2).
4. `GeneratedColumn` and the `ChunkSource` seam stay stable **by default**, so the game keeps
   working at every intermediate sha — but stability is a scheduling default, not a boundary of
   the mandate (see Status): a unit with a measured reason may redesign the serve boundary,
   `ChunkColumn`, section/palette storage, or the protocol-side encode, under the mandate's
   guard rails (external-origin evidence for anything wire-facing, one reviewable commit per
   coherent change, full-workspace test standard).
5. **Rollback rule for cutover gates, stated before the first cutover.** A byte-equality mismatch
   at *any* gate seed **blocks the landing** — one seed out of five failing is a failure, and it
   is the likeliest real outcome, which is exactly why it gets a rule now rather than a
   rationalisation later. What happens next, in order: localise with the per-wrapper fixtures
   (Q2) to a single wrapper/stage; then the **JVM oracle is the tie-breaker**, because the old
   engine is a bridge, not the spec — if investigation shows the *old* engine is the wrong one,
   that finding lands first as its own fix with its own JVM fixture, and the cutover rebases on
   it. Never allowed, under any schedule pressure: widening a tolerance, dropping a seed from the
   gate set, reclassifying a mismatch as "expected" without a JVM fixture proving vanilla
   produces it, or landing the cutover with the investigation open.

   **Worked example of why the tolerance rule is absolute** (measured, `08f5d98a` / §12.101):
   implementing the wrong one of vanilla's two interpolation orders scores **90,563/98,304** on
   `chunk_parity` — 7,741 blocks per chunk, every one a last-place float difference, worst gap
   `1.78e-15`; across the field, 60,300 of 393,216 blocks. A 92% pass with float-dust residuals
   reads *exactly* like a "tighten the epsilon" discussion — which is how a wrong **algorithm**
   survives review as a **precision** problem. The tolerance is the alarm; widening it is
   cutting the wire.

## Benchmark definition (Unit 1 detail)

`benches/generation.rs` already has criterion + JSONL recording, a 10-stage split with per-stage
non-vacuity floors, and an anti-drift control (`column_timed` vs `column`). Unit 1 **extends** it;
it does not start over. What it adds:

- **Counters, feature-gated (`gen-counters`, relaxed atomics, compiled out by default):**
  `block_at` calls; density component evaluations (by kind); corner evaluations per slot;
  palette interns; heap allocations in the column path (counting-allocator wrapper in the bench
  binary only); `pre_ore`/`post_ore` stage computations vs lookups (hits/misses); biome
  nearest-neighbour searches; RNG draws per stage. A counter beats a duration — this repo has a
  measured 585× mis-attributed timing on record, and the counters-off re-measure runs made the
  same point inside this exact codebase: three vegetation timings on one identical binary read
  63.42 / 63.77 / 52.28 ms — a 22% swing — while the allocation counter read **905,459 to the
  digit, three times of three** (2026-08-07).
- **Calibration assertions**: on one known chunk, counters must equal hand-derived expectations
  (e.g. exactly 98,304 `block_at` calls today; exactly 1,225 corner evals per interpolated slot
  after U4). A counter that cannot predict is a counter that cannot gate.
- **The C_ss / C_cold benches** exactly as defined in Q3, against **embedded server data** (the
  fixture-tree resolver stays for the shape-only benches; the "world" vacuity species — a full
  bench against data that makes stages no-ops — is the documented history of this exact file).
- **Two-arm rule**: any before/after comparison runs both arms interleaved in one process; a
  timing-shaped regression is re-run alone before being believed (CLAUDE.md).
- **Profiling (`samply` — an instrument, never a gate):** a sampled profile answers *where*,
  not *how much*. Use `samply` to decide which frames to attack; use counters to decide whether
  an attack worked. A profile is never a unit's acceptance criterion, and a duration is never
  preferred to a counter when both are available. The workflow already exists — cite it, do not
  reinvent it: [`../roadmap/benchmarks.md`](../roadmap/benchmarks.md) documents `samply` +
  `[profile.release] debug = 2` + `threadCPUDelta` weighting (`scripts/profile-cost-table.py`),
  and records both that a plain `cargo build --release` already carries the DWARF `samply` needs
  (deliberately no separate profiling profile to keep in sync) and the precedent that profiling
  has paid for itself here: #75 was found only because a `samply` session was run when no bench
  suite existed. `samply 0.13.1` is installed at `~/.cargo/bin/samply`. A different instrument
  for a different question: `lodestone-shell` carries `tracing-chrome` for span-timeline
  flamegraphs — a sampled profile says where CPU went; a span timeline says when stages ran and
  overlapped. Profiles are large artifacts: write them under a scratch path adjacent to the
  unit's private `--target-dir` and delete by exact name when the unit ends.
- Acceptance criteria for later units are expressed **in these counters** (U3: zero String
  allocations steady-state; U6: stage computations exactly equal to **each stage's own closure
  count** — for a 12×12 sweep, pre-ore 16×16 = 256 and post-ore 14×14 = 196; U7: zero stitch
  copies), so the harness is the contract, not a dashboard. U6's criterion originally read
  "chunks × stages", whose natural reading (144 × 2 = 288) is **wrong** — each stage closes over
  its own neighbourhood radius; do not gate on 288.
- **If a change has a concurrency dimension, a serial measurement can conclude there was nothing
  to fix.** Measured while landing U6: a serial sweep reads 256/196 under **both** the old FIFO
  cache and the new store — serially, a cache never has a racing miss, so the two implementations
  are indistinguishable by the very counter that defines the unit. The discriminating instrument
  was a 289-column concurrent join burst: old **452/452/448 and varying across runs**, new
  **441/441/441 exact**. Gate concurrency-dimension changes under concurrency.

## SIMD policy

Nightly + `portable_simd` is settled; one implementation, no scalar twin. Rules of engagement:

- **Lanes across independent positions only** (batches of lattice corners / column cells). Each
  lane computes the same scalar IEEE expression sequence it would compute alone, so vectorising
  this way cannot reassociate anything — parity-safe *by construction*, still gated bit-exact
  against the JVM fixtures like everything else.
- **Never across an accumulation chain**: octave sums in Perlin/blended noise have a fixed
  vanilla evaluation order; a horizontal-add tree is a different summation order and a different
  world. Octave loops stay sequential per position.
- **No `mul_add`** (or any FMA-introducing op) anywhere in ported numerics — fused rounding
  differs from vanilla's separate multiply-then-add.
- **Ordering of effort**: D2/D3 (allocation, interning, copies) are attacked *before* SIMD.
  ~885k Strings and ~2.8M copied cells per warm column is not a vectorisation problem, and if
  U3+U6+U7 land the target, SIMD (U5) may legitimately shrink to "the noise kernels that still
  show in the profile". "Not useful here" is an acceptable U5 outcome; the profile decides.

## Parallelism and allocation budget

First-class goals (owner ruling, 2026-08-06), with the acceptance criteria in U1's counters.

**Parallel model — single-writer chunks over a stage wavefront.** The dependency edges, explicitly:
fill/surface/carve of a chunk depend on nothing but the seed (embarrassingly parallel);
`ore(C)` reads `pre_ore(3×3(C))`; `veg(C)` reads `post_ore(3×3(C))`, which closes over
`pre_ore(5×5(C))`; `top_layer(C)` depends on `veg(C)` alone. The scheduler runs the resulting
wavefront: a chunk's stage becomes ready when its edges are satisfied, so achievable concurrency
is the frontier width — for a join burst, essentially *all* columns minus a one-chunk halo per
stage, with serial depth O(stages), not O(radius). No rings, no barriers. Crucially, **every
chunk's grid has exactly one writer — its own serve task**: decoration keeps the proven
per-centre fold (all 9 sources computed, writes routed into the centre's own grid, out-of-bounds
writes dropped), so neighbour products are consumed strictly as read-only `Arc` snapshots and no
cross-chunk write lock exists anywhere. Determinism is by construction, and
`parallel_generation_is_deterministic_and_matches_serial` stays as the gate.

**Shared mutable state after U6 — none on the hot path.** The only shared structure is the store's
map itself: sharded by chunk-pos hash (fixed shard count; a shard lock is held for the duration of
a lookup/insert of an `Arc`, nanoseconds, never across a computation — the discipline
`pre_ore_stage` already follows, minus the single global mutex and minus FIFO bookkeeping).
Per-entry stage transitions are an atomic state machine on the entry, so two workers contend only
when they need the *same entry's* transition at the same instant — which is a real dependency
edge, not incidental sharing. The block loops, density evaluation, and decoration walks touch
zero shared mutable state. D4's scar (`4307b59`, ~5000 lock attempts on one `Arc<Mutex>`) is the
control: the join-burst bench (U10) must show shard-lock time not measurable at 289 concurrent
columns.

**Allocation budget — a counter-asserted acceptance criterion, not an aspiration.** Steady-state
serve of one column: **0 heap allocations from the hot path**, with an explicit output allowance
of O(1) allocations for the returned `GeneratedColumn`'s own `palette`/`blocks` buffers (they
leave the function; everything else comes from scratch). All scratch — interpolator planes,
cell buffers, region views, RNG carriers — from **per-thread** pools (`thread_local` free-lists,
Pumpkin's `F64_BUFFER_POOL` pattern; never a shared pool behind a lock, which would serialise
what the store just parallelised). U1's `column_heap_allocs` counter (counting-allocator wrapper
in the bench binary) gates it, with the ratchet per unit: U3 deletes the ~885k Strings, U4 the
tree clones and per-chunk slot caches, U7 the stitch grids, U10 asserts the end state. The ~885k
figure is the single most damning number in the diagnosis; its disappearance is measured, not
implied.

## Decomposition (U16 detail)

**Status: delivered.** Phases A and B are in-tree (`overworld/` and `feature/vegetation/` exist
as directories), Phase C landed as `4aa7ac85` — extracting the *corrected* closed set including
`counters` (see Phase C below, corrected in `4be59556`). The as-built ownership map, and the
gotchas the split actually hit, are in
[`../worldgen-module-layout.md`](../worldgen-module-layout.md); this section remains as the
design record. Older docs naming `overworld.rs`/`feature/vegetation.rs` read as the directory of
the same name, deliberately not bulk-rewritten.

Enabled by the architecture grant above. Measured surface at planning time (counts drift while U3
is live in this crate): `feature/vegetation.rs` 3,661 lines, `overworld.rs` 1,832,
`feature/mod.rs` 1,100, `density/mod.rs` 1,052; crate total 16,335 across 44 workspace members
(`cargo metadata`), so sub-crates are idiomatic here, not novel. `overworld.rs` sits in the
cluster of six units — that one file is why the middle was scheduled as a pipeline. Decomposition
converts the file bottleneck into a fan-out, so it runs early: **strictly after U3 lands** (U3 is
live in `crates/lodestone-worldgen/**` now; U16 starts from post-U3 state and must not be folded
into U3's landing) and before the units it frees.

**Phase A — split `overworld.rs` along its existing stage seams.** The seams already exist as
named methods (`pre_ore_stage`, `ore_stage`, `stitch_region`, `vegetation_stage`,
`top_layer_stage`, `post_ore_world`), so this is a move, not a design. Target layout — names
indicative, the ownership column is the point:

| new file | moves | owned after by |
|---|---|---|
| `overworld/mod.rs` (thin) | `OverworldGenerator::new`, `column`/`column_timed` orchestration, `PreOreCache`/`PostOreCache` fields | U6 (the store replaces the caches) |
| `overworld/fill.rs` | the fill/aquifer half of `pre_ore_stage{,_uncached}` | U4, then U15 |
| `overworld/biome.rs` | `DynamicBiome` + biome nearest-neighbour sampling | U9, then U11 |
| `overworld/decorate.rs` | `ore_stage`, `stitch_region`, `vegetation_stage`, `top_layer_stage`, `stitch_veg_region` | U7, then U12 |
| `overworld/output.rs` | `GeneratedColumn`, `StageTimes` | read-mostly; changes brokered |

U4, U6, U7 and U9 must end this unit owning **different files**; that is the acceptance shape.

**Phase B — `feature/vegetation.rs` (3,661 lines, U8's entire scope) splits here, in U16 — not as
U8's first step.** U8 depends on U7, so "U8's first step" would keep the split serialized behind
U7; doing it in U16 costs nothing extra (same pure-move discipline, same gates) and means U7 ends
up owning only the driver/seeding file while U8's interior can fan out — including across agents
inside U8 — the moment U7's seam lands. The split is driver/seeding in one file, per-feature
interior in `feature/vegetation/` submodules; the depth-first recursion order is untouched by
construction, and the parity suite proves it anyway.

**Phase C — one leaf sub-crate, and only that one for now.** Verdicts from a measured
`use crate::` scan (2026-08-07), not from the module diagram:

- **`{hash, math, rng, noise, density}` → `lodestone-worldgen-core`: yes.** Measured a closed
  set — `math` imports nothing crate-internal, `rng` only `hash`, `noise` only `math`/`rng`,
  `density` only `math`/`noise`/`rng` — so the boundary exists today and the extraction is
  mechanical. 3,670 lines, ~22% of the crate. **Correction, measured in U16 (see
  [`docs/worldgen-module-layout.md`](../worldgen-module-layout.md)): this set is NOT closed, and
  the module it misses is the one that matters.** Re-running the scan while separating *code*
  lines from *doc-comment* lines finds **8 real call sites into `crate::counters`** —
  `density/chunk.rs`'s `bump_density_eval`/`bump_corner_lookup`/`bump_slot_hit` and `rng`'s
  `next_bits` hook — while every other apparent edge (`feature` 6, `overworld` 2, `biome` 2,
  `aquifer` 1, `compose` 1) is an intra-doc link and no dependency at all. And `counters`
  depends back on `density::Density::KIND_COUNT` for two array sizes and a loop bound, so this
  is a **cycle**, which a crate boundary cannot cut: extracting the five modules as written puts
  8 call sites in the new crate pointing at the old one. The set that *is* closed is these five
  **plus `counters`** (measured zero code edges out, ~4,372 lines) — that is the extraction to
  do. `KIND_COUNT` needs no change: with `counters` inside the core crate the edge is
  intra-crate, and making it local to `counters` is not a clean move anyway (`KIND_NAMES`'s
  array length and two of `density`'s own tests are written against it, so the only shape
  available is a duplicate plus a drift guard — and that guard re-creates the same edge as a
  dev-dependency). Buys: feature-layer iteration (U7/U8/U12, the
  most-edited code) stops rebuilding the numeric core; the leaf is independently testable (JVM
  noise/density fixtures run without building the other ~12.7k lines); it is exactly the surface
  U5 SIMDs; and U4's new `engine/` can be born inside it instead of moved later. Costs, honestly:
  the rebuild win accrues only to the *top* of the stack — editing the leaf still rebuilds every
  dependent — plus one more crate and the Phase-C guard runs below.
- **`carver`/`surface`/`aquifer` middle crate: no.** No two units contend for those directories,
  the rebuild win is ~2.6k lines, and `aquifer` is entangled with the fill path U4 is about to
  rewrite — a crate boundary through an active rewrite gets redrawn. Revisit after U4 if the
  numbers argue for it.
- **`feature/` as its own crate: defer until after U8.** The boundary would cut exactly along
  U7's region-view seam and U8's port, both scheduled rewrites; a speculative crate boundary is
  its own maintenance burden. Carve it when the decoration medium is stable, if ever.

**Discipline — the unit's evidence:**

- **Pure-move commits only.** No logic changes, no signature changes beyond visibility, nothing
  reordered. Review under `git diff --color-moved=dimmed-zebra`; new lines other than
  `mod`/`use`/visibility plumbing are defects.
- **The byte-identity gate and the full parity suite run against the moved tree in the same
  commit**, plus `just health`. A "pure move" that changes RNG order is the single worst outcome
  available — landed bare, the failure would be attributed to whichever unit lands next.
- **A split never shares a commit with a logic change.** If both are needed, the move lands
  first, green, alone.
- Phase C additionally: **`just check-seam`** (the version seam), and **`cargo xtask
  check-isolation` + `cargo xtask check-connected`** (wasm confinement and the plugins→engine
  dependency direction) — the two classes of break a crate split can cause that a file split
  structurally cannot.

### Concurrency after U16 (recomputed, data-honest)

File ownership was only one of two constraints. The data edges remain — U6←U3, U7←U3+U6,
U8←U3+U7, U9←U6, U10←U6+U7, U11←U9, U5←U4+U2, U12←U7, U15←U4 — so the payoff is smaller than the
file split suggests, and worth stating exactly:

- **The critical path does not shorten: U3 → U16 → U6 → U7 → U8.** Decomposition cannot remove a
  data edge; what it removes is the *false* serialization of everything else queued behind that
  path.
- **Wave 1** (U16 lands): **U4 ∥ U6** — `fill.rs` + new `engine/` vs `mod.rs` + `store.rs`. The
  one shared point is the driver→fill call site, a one-line change brokered by the orchestrator.
  U13/U14 data extraction free-runs alongside, as ever.
- **Wave 2** (U4 + U6 landed): **U5 ∥ U7 ∥ U9 ∥ U15** — engine kernels / `decorate.rs` + feature
  medium / `biome.rs` / `fill.rs` veins. Four units that were previously a queue.
- **Wave 3** (U7 landed): **U8 ∥ U10 ∥ U12** — vegetation interior / server scheduler (different
  crate) / new decoration modules — and **U11** once U9 lands.
- Net: the six-unit serial middle becomes width **2 → 4 → 4** around an unchanged five-unit
  critical path. Before U16, the same work was width 1 throughout. That is the whole payoff, and
  it is bought by an M-cost pure-move unit.

## Unit list

Costs: S ≲ 1 session, M ≈ 1–2, L ≈ 3+, XL = epic (own issue tree). "RNG" = can this unit change
any RNG draw or consumption order? Every unit's baseline evidence: `just health` green plus the
gates named in Q4 step 2; per-unit evidence listed is *additional*. A `samply` profile is the
right way for a unit to *choose* its targets (see the harness section's profiling rules), but
acceptance is always counters and gates — never a profile, never a bare duration.

| # | Unit | Cluster (files) | Depends | Cost | RNG order |
|---|------|-----------------|---------|------|-----------|
| U1 | Benchmark harness: counters + C_ss/C_cold + calibration | `benches/`, `src/` counter hooks, `docs/benchmark-harness.md` | — | M | none |
| U2 | Release baseline + profile on embedded data; publish per-stage µs + counters; re-negotiate targets | bench-results, this doc, DESIGN §12 entry | U1 | S | none |
| U3 | Numeric ids: interned `u16` states through dense_grid/carver/ore/top-layer; `String` only at serve boundary | `dense_grid.rs`, `carver/`, `feature/mod.rs`, `feature/top_layer.rs`, `overworld.rs` | U1 | L | none (representation) |
| U4 | Flattened density engine + vanilla cell-fill (`Mth.lerp3` order — §12.101, **not** the incremental walk) + per-chunk scratch (kills D1, D3) — order fix landed `08f5d98a`; remainder re-scoped in **#490** | new `engine/` (in the U16 core crate), then `density/`, `aquifer/`, `overworld/fill.rs` cutover | U1, U16 | L | none (no RNG in density) |
| U5 | `std::simd` noise kernels behind U4's batched fill API | `noise/`, `src/engine/` | U4, U2 profile | M | none (position-lane only) |
| U6 | Staged sharded store replacing both mutex caches; drivers unchanged — **landed** `34202a21` | `overworld/store.rs`, `overworld/mod.rs` | U3, U16 | L | **must not** — byte-identical gate (held) |
| U7 | In-place region decoration view; delete stitch copies + `RegionGrid`/`VegGrid` re-seeding | `feature/mod.rs`, `feature/vegetation.rs`, `overworld.rs` | U3, U6 | M | **must not** — same driver order |
| U8 | Vegetation engine port to ids + region view (the 3.6k-line module) | `feature/vegetation.rs` | U3, U7 | L | **must not** — depth-first recursion untouched |
| U9 | Biome layer: memoised per-source biome in store + RTree port | `biome.rs`, `src/engine/` | U6 | M | none, but **values must match brute force exactly** |
| U10 | Server scheduler: dependency-edge generation, delete per-ring barrier — **landed** `7ba0176b`/`0a3ede8d` (#494), see [`../join-scheduler.md`](../join-scheduler.md) | `lodestone-server/src/{server,chunk,join_scheduler}.rs` | U6, U7 | M | none |
| U11 | 3-D biome sampling (4×4×4 quart cells) on the RTree | `biome.rs`, `overworld.rs`, serve boundary | U9 | M | **changes biome-dependent placement inputs** — vanilla-ward; needs fresh JVM fixtures |
| U12 | Missing decoration steps: lakes, springs, geodes/icebergs, disks, dungeons/fossils | `feature/`, `compose.rs` | U7 | L | additive (per-feature `set_feature_seed` isolates streams; index-preservation already in place) |
| U13 | Nether + End generation — **unit group NE**, own issue tree; see [inventory](#full-parity-inventory-jar-derived-262) | new engine instantiations + data + server dimension plumbing | U4–U9 | XL (group) | new content |
| U14 | Structures — **unit group S**, own issue tree; see [inventory](#full-parity-inventory-jar-derived-262) | new `src/structure/`, data, beardifier hookup, ChunkStatus contract | U6, U7 | XL (group) | new content |
| U15 | Ore-vein system (`OreVeinifier`): large copper/iron veins from the `vein_toggle`/`vein_ridged`/`vein_gap` router channels, applied during fill | `src/engine/`, `overworld/fill.rs` | U4 | M | none (positional RNG, per-block chooser) — **changes overworld terrain toward vanilla**; needs a vein-positive JVM fixture |
| U16 | Decomposition: split `overworld.rs` and `vegetation.rs` on stage seams; extract `lodestone-worldgen-core` leaf crate — [detail](#decomposition-u16-detail) | `overworld.rs` → `overworld/`, `feature/vegetation.rs` → `feature/vegetation/`, new leaf crate | U3 | M | **must not** — pure moves only; byte-identity + full parity on the moved tree, same commit |

**Scheduling note (shared checkout):** until U16 lands, `overworld.rs` remains the choke point
across U3/U4/U6/U7/U9/U11 — **at most one in-flight owner at a time**, brokered by the
orchestrator. **U16 exists to end that regime**: it converts the file bottleneck into the wave
structure in [Concurrency after U16](#concurrency-after-u16-recomputed-data-honest). Post-U16 the
middle is still not a free fan-out — the data edges cap it at width 2 → 4 → 4 and the critical
path U3 → U16 → U6 → U7 → U8 stays serial, so a six-agent fan-out across U3–U9 remains
unschedulable. Genuinely parallel-safe at any time: U1/U2 (benches) and the data-extraction
phases of U13/U14.

**Disk note:** per-unit private `--target-dir`s and profile artifacts run to tens of GB each;
abandoned ones filled the disk to 100% mid-planning (2026-08-07, ~147 GB reclaimed, ENOSPC broke
tool output session-wide). Name them per-unit, delete by exact name when the unit ends, and have
the orchestrator sweep stale `/private/tmp/lt-*` dirs periodically — a sixteen-unit rewrite will
reproduce this otherwise.

**Per-unit notes — the trap most likely to sink each:**

- **U1**: the trap is the file's own history — a bench whose resolver silently supplies no data
  measures a pipeline with stages missing ("world" vacuity). Every new bench asserts its stages
  actually ran, by counter. *(Found while planning: `bench_stage_split`'s recorded scene string
  says `patch=7x7(49 chunks)` while the code sweeps 3×3 — stale metadata that mispairs
  `bench-compare` history; fix in U1.)*
- **U2**: a timing taken while other agents compile is a sample; run alone, prefer counters, and
  record machine state alongside (`vm.swapusage`, not `Pages free`).
- **U3**: palette/order determinism at the serve boundary — the `RandomState` iteration-order bug
  already happened once (`overworld/mod.rs`'s module-doc post-mortem, near `materialize_world`); the byte-identical control is
  the gate. Evidence adds: steady-state String-allocation counter == 0.
- **U4**: the semantic subtleties the interpreter hides — `Mul`'s `v1 == 0.0` short-circuit,
  `interpolated`-inside-corner transparency (`interpolate=false`), `flat_cache`'s forced `y=0`,
  `cache_once`/`cache_2d` scoping — and above all the interpolation order: the block field is
  `Mth.lerp3` under a code-only `cache_all_in_cell` (§12.101). Evidence adds:
  `DensityChunkOracle` fixtures bit-exact **for component functions only — the oracle itself
  drives the incremental `advanceCellX`/`updateForY` chain (its own source, lines 76–83), so it
  is *not* authoritative for the block field's interpolation order**; that gate is `chunk_parity`
  (98,304/98,304) plus the standing inverted guard landed in `08f5d98a`. Whole chunks
  byte-identical to old sampler at ≥3 seeds, corner-eval counter == predicted lattice.
  **Status: partially delivered** — `08f5d98a` landed the order fix, the module doc and the
  inverted guard; the flattened engine, the cutover, `legacy_random_source` (#486) and
  `end_islands` are re-scoped in **#490**. Two measured structural constraints for the engine
  design: `column_timed` pins all seven stage signatures and derives `StageTimes` from the
  `build_aquifer`/`fill_stage` boundary; and `AquiferTrees` pins **eight** `Density` fields by
  struct literal, so a compiled-graph cache has nowhere to live **unless `Builder::build`'s
  returned `Density` carries it** — a route needing no non-owned edits (outside `density/`,
  `Density` variants are only constructed, 11 sites; `Spline` has no external users), which also
  makes `t.erosion.clone()` an `Arc` bump, delivering D3's eight-deep-clones fix for free.
- **U5**: FMA and reduction reassociation (see policy). Evidence: bit-identical to U4 output
  across the full fixture set — an internal refactor gate *plus* the JVM fixtures, because
  "identical to our own previous output" alone is `decode(encode(x))` in disguise.
- **U6** (**landed**, `34202a21` + `9c4f0967`, doc `5814da19`): the aliasing trap —
  `pre_ore_stage`'s doc records a clamped-key cache aliasing two chunks and hanging a JVM oracle.
  Exact keys only; eviction is view-scoped, never capacity-FIFO. Evidence as delivered:
  stage-computation counter == each stage's closure count (256/196 on a 12×12 sweep — see the
  corrected criterion in the harness section); sweep byte-identical to old engine; and the
  289-column join burst as the *discriminating* gate, since the serial counter reads identically
  under old cache and new store (old burst: 452/452/448 varying; new: 441/441/441 exact).
- **U7**: the **VegGrid absolute-vs-local precedent** — a coordinate-space bug produced zero
  vegetation in every served chunk with the unit suite green, and the gate that caught it was
  later deleted. Evidence adds: a boundary-write control (a feature known to spill across the
  seam, asserted present on both sides), plus the two live `worldgen_data.rs` gates.
- **U8**: also owns the five [cost-per-draw candidates](#vegetation-cost-per-draw), each landed
  or refused with a measurement. Its trap: breadth-first "optimisation" of the depth-first
  recursion — instant RNG desync;
  also `TrapezoidInt`-vs-`Uniform` (same support, different draw count) is the recorded shape of
  subtle stream desync. Evidence adds: `VegetationOracle` plains 30/30 + 57/57 exact, savanna
  fixtures re-run (two named residuals, 11/185 and 1/116, are pre-existing — do not absorb them).
- **U9**: the tree must be **result-identical** to brute force — vanilla ships both and uses the
  tree; any tie-breaking or pruning difference is a different biome at some coordinate. Follow
  Pumpkin's shape (tree structure taken from the game data's own node ordering, vanilla's pruning
  search, thread-local last-result seeding — see Q2), but prove it locally. Evidence:
  exhaustive equality brute-vs-tree over a large sampled climate volume + `BiomeOracle` fixtures.
  (This is also #405's "y = 0 trap" territory — sampling height conventions are per-consumer and
  already divergent by design: carver/ore at y=0, vegetation at surface. Do not "unify" them.)
- **U10**: "wait" must never mean a background monitor (harness marks the agent complete);
  scheduler determinism gate `parallel_generation_is_deterministic_and_matches_serial` already
  exists — keep it, plus a fresh-generator-per-arm rule (its own doc records the memo-cache
  self-agreement trap).
- **U11**: this is the one engine unit that **intentionally changes output** (toward vanilla).
  It needs new `ComposedChunkOracle` fixtures with 3-D biome resolution before the diff, or every
  gate melts at once with no way to tell progress from regression.
- **U12**: step-index preservation — `build_biome_ores` already skips-but-keeps-index; per-feature
  reseeding (`set_feature_seed`) means adding a feature cannot desync its neighbours' streams.
  The trap is assuming that and not proving it: each new feature type lands with its own
  `FeatureOracle`-shaped fixture, and the composed postfeatures gap must shrink monotonically.
- **U13 (group NE)**: measured blocker inventory (2026-08-07 jar audit — better than previously
  believed): the bundle already carries **all 66 biome documents and all 35 density-function
  files byte-identical to the jar**, including `nether/`, `end/`, `overworld_amplified/` and
  `overworld_large_biomes/`. **Corrected 2026-08-08 (re-measured): the data top-up has since
  landed** — all 7 `noise_settings`, all 63 noises and `biome_parameters/nether.json` are
  bundled (#485 `dd40b1f`/`a379f34` plus the corpus sibling `6c6c0e10`); no `[data]` item
  remains for either dimension. Missing from the *engine*:
  the `minecraft:end_islands` density type (the only DF type used anywhere in vanilla's worldgen
  data that we do not implement — measured by full type census across all 7 noise_settings) and
  the three non-multi-noise biome sources. So: engine instantiation directly — no data top-up
  remains — then server dimension plumbing (#330: only overworld is
  hosted; portal/dimension-switch is **gameplay**, not worldgen — a Nether generator is testable
  against oracles without any portal existing). **The executable sequence is
  [`nether-and-end.md`](./nether-and-end.md).**
- **U14 (group S)**: phased S0–S4 in the [inventory](#full-parity-inventory-jar-derived-262) —
  S0 ChunkStatus contract, S1 placement/locate (pure math, no blocks), S2 templates (**data
  extraction done — corrected 2026-08-08**: 188 template pools + 40 processor lists + 34
  structures + 20 sets + 1,212 `.nbt` templates under `assets/structure/`, landed `6c6c0e10`
  under #484, byte-identical to the jar), S3 beardifier (currently a constant-0 leaf in
  `density/mod.rs` — a real engine seam, not free), S4 jigsaw. `structure_spawn_overrides` and
  in-structure mob spawning (#221/#222) are **gameplay-blocked**, not worldgen-blocked. Wants
  its own issue tree; do not execute group S from this document. **The executable sequence is
  [`structures.md`](./structures.md).**

## Full-parity inventory (jar-derived, 26.2)

Everything full vanilla parity requires, present or absent, so nothing is discovered late.
**Method**: every count below was measured on 2026-08-07 against
`.cache/mc/26.2/versions/26.2/server-26.2.jar` (the real jar inside the bundler wrapper) and the
de-obfuscated `src/` — not recalled from memory. Blocker classes: **[data]** absent from the
bundle, **[engine]** absent engine primitive, **[gameplay]** cannot finish even with perfect
worldgen, **[unwritten]** nothing blocks it, **[out-of-scope]** deliberately excluded.

**Caveat (learned the hard way — `08f5d98a`, §12.101): this census is authoritative about which
density-function *types* exist and silent about how they are *evaluated*.** It was built by
walking the `noise_settings` JSON, and the marker that selects the block field's interpolation
arithmetic — `cache_all_in_cell` — appears **nowhere in 26.2's worldgen JSON** (0 hits across
both the bundled corpus and the jar's own `data/minecraft/worldgen/`, with a working detector:
the `minecraft:interpolated` control hits 2 and 8). Vanilla applies it in code,
`NoiseChunk.java`. This is the same class as CLAUDE.md's `registries.json` trap: an
authoritative source answering a *neighbouring* question. A data census cannot see evaluation
order; only the decompiled call path can.

**Data completeness, bundle vs jar** (`assets/worldgen/` vs `data/minecraft/worldgen/`):

| registry | jar | bundled | delta |
|---|---|---|---|
| biome | 66 | 66 | complete |
| placed_feature / configured_feature | 262 / 226 | 262 / 226 | complete |
| density_function | 35 | 35 | complete, **byte-identical all 35** (diffed) |
| configured_carver | 4 | 4 | complete |
| noise | 63 | 63 | complete (was 61; the 2 nether noises landed `dd40b1f`) |
| noise_settings | 7 | 7 | complete (was 1; `dd40b1f` + the corpus sibling) |
| multi_noise parameter lists | 2 | nether + overworld (+`overworld_temperature`, provenance unverified — #485) | complete (nether landed `a379f34`) |
| structure / structure_set | 34 / 20 | 34 / 20 | complete (was 0/0; `6c6c0e10`, #484) |
| template_pool / processor_list | 188 / 40 | 188 / 40 | complete (was 0/0; `6c6c0e10` — plus 1,212 `.nbt` under `assets/structure/`, not `assets/worldgen/`) |
| world_preset / flat presets | 7 / 9 | 7 / 9 | complete (was 0/0; `6c6c0e10`) |

**The chunk-status pipeline — the scheduling contract, and a structures prerequisite.**
Vanilla's progression (read from `chunk/status/ChunkStatus.java`): `EMPTY → STRUCTURE_STARTS →
STRUCTURE_REFERENCES → BIOMES → NOISE → SURFACE → CARVERS → FEATURES → INITIALIZE_LIGHT → LIGHT →
SPAWN → FULL`. Structure starts run **before** noise so the beardifier can consult them during
fill. U6's staged store is the natural home: its stage enum grows toward this contract, and doing
so is a **prerequisite of group S**, not a detail inside it (phase S0 below). [engine]

**Heightmaps as persisted artefacts.** Vanilla maintains six (`Heightmap.java`):
`WORLD_SURFACE_WG` / `OCEAN_FLOOR_WG` (worldgen-time) and `WORLD_SURFACE`, `OCEAN_FLOOR`,
`MOTION_BLOCKING`, `MOTION_BLOCKING_NO_LEAVES` (persisted/sent). Our engine computes one ad-hoc
solid-top array. Load-bearing twice already: #437's gate reads vanilla's own `WORLD_SURFACE`,
and vegetation cost candidate 3 *is* the incremental-update semantics of these maps. Becomes a
named deliverable inside U6 (storage) + U8 (incremental updates). [engine]

**Density-function types.** Measured census: across all 7 noise_settings plus all 35 DF files,
the **only** DF type the engine lacks is `minecraft:end_islands` (one use, `end.json`). Notably,
26.2 has **no `weird_scaled_sampler`** — the noodle/spaghetti/pillars caves are expressed via
`interval_select`, which we already implement; anyone porting from 1.21-era sources will look
for a type that no longer exists. [engine, one type]

**The ore-vein system (U15).** Vanilla's `OreVeinifier` generates the large copper/iron veins as
a block chooser during fill, driven by the `vein_toggle`/`vein_ridged`/`vein_gap` router
channels — all three present in the bundled `overworld.json`, **entirely unimplemented in the
engine** (grep: zero non-comment hits). This is an *overworld* parity gap in the current world,
not a new-dimension feature; it is invisible to the existing `(0,0)` composed fixture only
because that chunk happens not to prove a vein, so U15's gate must include a vein-positive JVM
fixture. [engine]

**Biome sources.** Vanilla has four (`level/biome/`): `MultiNoiseBiomeSource` (ours),
`TheEndBiomeSource`, `CheckerboardColumnBiomeSource`, `FixedBiomeSource`. The End is **not**
multi-noise; single-biome and debug presets need fixed/checkerboard. All three others: [engine,
small — each is a page of logic].

**Group NE — Nether.** Terrain: `nether.json` noise settings + nether parameter list + 2 noises
[data — **landed**, #485 `dd40b1f`/`a379f34`; #485 also corrects the lava-sea claim below:
`aquifers_enabled` is false and the Nether is *not* an aquifer with lava as second fluid],
lava-sea aquifer behaviour (vanilla hardcodes the second fluid as lava — already
modelled) and per-dimension surface-rule coverage (the census shows nether/end settings use only
condition types the overworld also uses, but per-dimension verification is part of the unit)
[unwritten once data lands]. Biomes (basalt deltas, soul sand valley, crimson/warped forest,
nether wastes, warped forest) are **already bundled** — 66/66. Fortress and bastion are group S
work, not terrain work. Server-side: dimension registry/travel is #330 [gameplay-adjacent; the
generator itself is oracle-testable without it].

**Group NE — End.** `TheEndBiomeSource` + `end_islands` DF type [engine]; `end.json` [data —
**landed**, #485 `dd40b1f`];
the obsidian pillars (`end_spike` configured feature) and chorus plants land via U12's
step-census machinery [unwritten]; end cities + gateways are group S; the dragon fight and
respawn mechanics are [gameplay], not worldgen.

**Group S — structures, the full enumeration** (34, from the jar): ancient_city,
bastion_remnant, buried_treasure, desert_pyramid, end_city, fortress, igloo, jungle_pyramid,
mansion, mineshaft, mineshaft_mesa, monument, nether_fossil, ocean_ruin_cold, ocean_ruin_warm,
pillager_outpost, ruined_portal (×7 variants), shipwreck, shipwreck_beached, stronghold,
swamp_hut, trail_ruins, trial_chambers, village (×5 variants). Shared machinery: jigsaw +
template pools (188) + processor lists (40) + structure sets (20) with their placement types
(concentric-rings for stronghold, random-spread for the rest) [data + engine];
`structure_spawn_overrides` — where a structure's mob spawn table lives — is parsed with the
rest but **[gameplay]** to honour (the spawning system consumes it, not the generator).
Phasing: **S0** ChunkStatus contract in the store (above) → **S1** placement/locate (structure
sets + rings/spread math, `/locate`-answerable, zero blocks, oracle: vanilla `/locate` dumps) →
**S2** template structures (NBT structure templates + processors; data extraction first) →
**S3** beardifier (real terrain adaptation replacing the constant-0 leaf) → **S4** jigsaw
(villages, ancient city, bastion, trial chambers — the XL tail). Group S is a **unit group with
its own issue tree**; this plan fixes only its phase boundaries and evidence standards.

**World presets and generator types** (7 presets + 9 flat presets, enumerated from the jar:
normal, amplified, large_biomes, single_biome_surface, flat, flat_all_dimensions,
debug_all_block_states). Amplified and large_biomes are **pure data** over the existing engine
(their DF files are already bundled) [data: noise_settings only]. Superflat is a trivial
separate generator [unwritten, small]. Debug world is a special block-grid generator [unwritten,
small, low value]. Single-biome needs `FixedBiomeSource` (above). Honest cost: cheap once the
engine is data-driven — none of these justify scheduling before the engine units land.

**Blending / upgrade data (`BlendingData`) — explicitly [out-of-scope]**, stated so it reads as
a decision rather than an omission: it exists to blend chunks generated by *older versions* into
new terrain. We generate fresh worlds only; there is no old-version chunk data to blend against.
The engine keeps vanilla's empty-blender constants (`blend_alpha`=1, `blend_offset`=0,
`blend_density` transparent), which is exactly vanilla's behaviour on a fresh world. Revisit
only if importing pre-26.2 worlds ever becomes a goal.

**Also checked, already covered elsewhere**: decoration-step census (U12), 3-D biomes (U11),
lighting (different subsystem, excluded from C_ss by definition), mob spawn placement (#221/#222,
[gameplay]).

## Stale claims found while planning (reported, not edited)

- The task brief's "75 biome documents" — the bundle has **66** (`assets/worldgen/biome/`),
  corroborated by DESIGN.md §12.30's own census. 262 placed / 226 configured are correct.
- `benches/generation.rs`'s `bench_stage_split` — scene string `patch=7x7(49 chunks)` over a 3×3 sweep (see U1). Now fixed: the scene string is derived from `coords` rather than restated.
- `biome/mod.rs`'s `nearest_biome`'s "brute force is already fast" — true in isolation, contradicted in composition
  by the 289-source × 25-chunk multiplier (D5). The comment predates carver composition.
- `overworld/mod.rs`'s module doc self-reports one stale sentence ("Vegetation … still-not-composed",
  corrected in-place) — already flagged in the file, nothing to do.

## Unverified, needs follow-up

- **PumpkinMC residue** (the divergence map itself is now verified — see Q2): whether its
  generated `BiomeTree` preserves vanilla's node ordering exactly (asserted by construction there,
  not spot-checked), and the provenance of its captured vanilla chunk dumps (no dump-generation
  tool is committed in that checkout). Neither blocks anything here — we build our own fixtures.
- **Release-profile baseline** — now exists (U2 landed; DESIGN.md §12.98) but is **provisional**:
  the counters-on figure is being re-measured counters-off on the pinned toolchain. Treat any
  quoted C_ss as superseded by §12.98's latest entry. Every other performance number above from
  the tree is debug-profile or partial.
- **Vegetation walk cost in release with D2/D3 fixed** — the make-or-break number for the sub-ms
  verdict (Q3); measured at the U4 checkpoint.
- **Vanilla's decoration-step census against the 26.2 jar** (exact step list our composition still
  skips) — derive from `RegistryDataLoader`-adjacent code and the biome JSONs during U12, not
  from memory.
- The savanna vegetation residuals (11/185, 1/116) — pre-existing, mechanism unfound; they bound
  U8's "no new residuals" gate rather than being absorbed by it.
