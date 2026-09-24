# World-generation throughput

## What it is

`crates/lodestone-worldgen/examples/throughput.rs` measures the three bundled dimensions at the world-generation boundary. It compares each dimension's shaped terrain prefix with its fully decorated column over a deterministic grid, without persistence, lighting, or packet encoding.

## How it works

The executable builds one production generator per dimension and seed, walks a row-major `N × N` chunk grid, and reports cold and warm passes. Cold starts with a fresh generator; warm repeats the same grid after the first pass so memoized stage work is visible. Overworld and Nether use `column_shaped`; End uses its pre-surface `shape_field`; all decorated paths call the public `column` method.

Each pass reports chunks per second, process CPU utilization when the host exposes `getrusage`, and sampled resident-set growth. A separate 16-chunk allocation pass uses a benchmark-local counting allocator, so allocation counts do not distort the wall-clock throughput numbers. The legacy rolling digest over non-air counts remains in the output for historical comparison, while an out-of-band SHA-256 content digest hashes every canonical block-state string, exposed biome cell, and generated block-entity sidecar (or the dimension's equivalent loot/gateway sidecar). The content pass is outside both the timed loop and allocation-counted closure, and cold/warm digests must agree.

With `gen-counters`, `full_column_conversions` counts logical complete-column conversion operations and `full_column_conversion_cells` reports their covered cells; neither is a byte-traffic measurement. Full Overworld output packing is counted once at the dense-to-compact packing boundary. These counters cover worldgen work, not packet encoding or neighbor-light preparation.

The strict server benchmark also reports `target_write_bookkeeping` when built
with `lodestone-worldgen/gen-counters`. Its local and spill counts are unique
dirty epoch cells by destination chunk. Override and carver revision attempts
count calls to the materializer's setters; insertions count new or changed
values appended to the ordered revision streams. Canonical-winner updates count
winner-map inserts or replacements. The winner attempt split classifies each
attempt as local (writer target equals destination chunk) or foreign, then as
vacant, replaced, or lost. `winner_vacant_share` is the share of attempts that
found an empty winner slot; it does not prove that no later writer will collide
with that cell. Authenticated-write calls include calls that return early
because the target output already contains the write.
The `epoch_dirty_*_raw` and `epoch_dirty_*_unique` counts separate append-only
dirty entries from distinct positions, with full targets and sparse padding
reported independently. They show the potential headroom for changing dirty
deduplication without treating repeated writes as distinct output changes.
The ordered deduplication uses a reusable bitset over each source's bounded
80×80 horizontal footprint and build height. It resets for each source; write
order still comes from the dirty log. At height 384 the scratch uses 300 KiB.
The line includes totals and averages per measured output. These are structural
diagnostics for the bookkeeping path, not counts of final block changes or a
throughput score. The counter control checks a reset followed by zero work,
then a small known sequence with both changed and unchanged revision attempts.

With `worldgen-stage-pmu` on macOS, the strict benchmark reports both inclusive
region costs and exclusive stack-top costs. `region_exclusive_pmu` subtracts
nested regions, then separates named stage bodies from the remaining work in
each region. `outside_region` covers request work without a region guard.
These are process-wide retired counters and include observer overhead; use an
uninstrumented run for the throughput and peak-memory control.

Production Overworld batches use the source's indexed `columns` hook rather than
the scalar default. A pristine batch fans out over the shared world-generation
pool; when the hook is called from an already-admitted pool worker it joins that
same Rayon pool, avoiding a nested semaphore wait and a second worker set.
Edited columns retain the scalar path so the edit ledger remains authoritative.
The indexed collection preserves caller coordinate order, and the content digest
is the exactness control for worker scheduling.

The synchronous dispatcher path reserves only the permits needed by the
submitted batch, capped by the worker budget. A short immutable batch can
therefore overlap another producer instead of monopolising idle workers; a
saturated dispatcher still falls back to ordered serial execution.

Production sessions prepare immutable source products once per admitted batch,
before shaped residents are dispatched. Reconstructing a target state machine
for ordered mutation and packet finalization does not repeat preparation.
Overworld preparation separates each bounded region's dependency geometry from
its missing output products. Density, climate, and structure inputs retain the
full region bounds, but already-ready terrain prefixes do not execute again
when another coordinate in the same region is missing. Readiness is sampled
under the same store lease used for publication, so eviction cannot invalidate
that selection.

The Overworld generator also exposes `OverworldBatchLease`. A production
dispatcher passes its complete admitted coordinate set to `lease_batch`, then
uses the lease's shaped/full column methods for the request. The store pins the
union of the radius-10 closures once and releases it once; scalar `column` and
`column_shaped` use the direct one-column `open_view` path, avoiding batch-bound
construction while retaining the same radius-10 pin. The lease's coverage
assertion preserves the eviction guarantee, while store lease counters report
exact opens, coordinate pins, and shard-map touches for a scalar-versus-batch
control.

Within one Overworld pre-ore request, the climate preparation is also shared:
one bordered quart grid supplies the biome-cell stage through its 4×4 centre
view and the surface scan through its full nearby-corner view. The preparation
counter is request-scoped; it must be one for a shaped production request, and
the view bounds are checked before any prepared-channel index is read.

The ordinary improved-noise entry point has a dedicated zero-scale path. It keeps the same coordinate, floor, and interpolation order as the general scaled entry while avoiding its scale/fudge branch; the focused `zero_scale_entry_is_bit_identical_to_scaled_entry` test checks the two entry points by result bits over fractional coordinates. The three blended-noise loops use a private non-zero-scale entry after their positive-scale contract is established; `noise_scaled` remains the general zero/non-zero API. The focused `nonzero_scaled_entry_is_bit_identical_to_general_entry` control covers negative, lattice-boundary, wrapped, and fractional coordinates plus varied positive scales and fudges. These are local, immutable changes, so concurrent generators share no cache or mutable state.

Perlin construction now derives a bounded active-octave list. Each non-zero
level retains its original slot order and the exact input/value factors reached
after the full slot walk, while the reverse power used by blended noise is also
captured. Point sampling therefore visits only active levels: it has no
empty-slot branch or per-sample factor update. The original slot map remains
only for signatures and the compatibility octave accessor. The focused core
controls compare sparse and all-zero amplitudes against an independent scalar
slot walk over negative, fractional, and wrapped coordinates; the gen-counters
binary reports active and skipped visits separately.

The overworld biome stage searches its seven-axis parameter tree with an exact i64 squared-distance bound. A child visit stops accumulating once its partial sum reaches the incumbent distance; the cutoff is inclusive so equal distances remain pruned by the strict visit comparison and the incumbent tie-break does not change. The full-distance path remains in place for selected leaves and single-row roots. Tree identity controls cover cutoff equality, tied-row selection, real-table distance parity, and the fixed production fixture.

Compact-tree bounds test the axes in measured cutoff order. The offset-zero
specialization omits its constant axis and uses the same remaining order. This
only changes how soon a child bound reaches the incumbent distance; exact sums,
equal-distance pruning, child order, and selected rows remain unchanged.

Production biome tables whose bounds fit `i32` use 64-byte search nodes. Leaves
store their row in the child-offset field that has no leaf meaning, while
internal nodes retain the same offsets and child order. Wide tables keep the
original representation. The compact and wide searches share exact-result,
cursor-sequence, tree-shape, and corrupted-row controls.

The surface stage keeps its nearby-quart footprint and climate targets in typed caches rather than cloning a `String` for every cell. Each unique quart also retains its canonical nearest leaf and distance. A later lookup keeps the incoming cursor leaf when its exact distance ties that canonical minimum; otherwise it selects the canonical leaf. This reproduces incumbent-wins-ties behavior without walking the tree again. A lookup resolves the selected id back to a borrowed table name only while evaluating a surface rule, and fallback-biome contexts keep their one borrowed name. All retained answers are bounded by the surface scan's existing quart footprint.

The mixed ore/vegetation bridge reuses its ordered overlay and changed-cell buffers for the lifetime of one feature stage. The buffers are cleared before reuse, while the deterministic coordinate sort and overwrite filtering remain unchanged; this removes cumulative temporary-vector traffic without changing generated content. Ore replay also keeps a cursor into the ordered write log: each entry sorts only writes appended since the previous entry, resolves every repeated key to its final overlay value, and lets the existing transfer map retain last-write semantics. Seeded read context is not logged because it is already present in that transfer map. The benchmark prints both allocation count and allocated bytes per sampled chunk, making cumulative-vector regressions visible even when allocation counts are similar.

Decoration-catalog membership selection borrows feature identifiers from the generator-scoped catalog while building each source's temporary membership set. The selection result and global index accounting are unchanged, but the hot loop no longer clones an owned identifier for every catalog entry just to perform a membership lookup.

The Overworld replay context now builds each of its nine source 3×3 biome unions once as borrowed palette names. A single catalog walk fans that union into the ordinary decoration, step-6 disk, step-6 non-ore, and ore streams while incrementing each global step index exactly once; placement results are still cloned only into the stream that consumes them. This removes repeated membership-set construction and three redundant catalog scans without caching mutable placement state. The focused catalog controls compare stream counts and raw indices against the individual selectors.

Dense-grid halo copies use a lazy source-palette mapping. The first occurrence
of each source palette entry still appends to the destination in scan order, so
the served palette and block digest are unchanged, but later cells reuse a
numeric destination index instead of hashing the same state id again. This is
one short-lived mapping allocation per copy, rather than one lookup per copied
cell; it is intentionally kept separate from the grid's persistent worker
scratch because source palette sizes vary by feature.

Overworld materialisation consumes the typed sparse surface diff a column at a
time while producing the dense grid's fixed `(z, x, y)` traversal. The builder
fills the unique cell carrier directly and interns states in that same order,
preserving both the block vector and palette bytes without a per-cell hash
lookup, coordinate conversion, sort, or copy-on-write check. This path is
intentionally scalar: palette assignment is order-dependent, so SIMD cannot
safely replace it without changing the observable palette contract.

The server's generated-column handoff derives palette classifications, section
ticking counts, and all three client heightmaps from the generator's flat cells
before `SectionedBlocks` packing. Heightmap predicates share one top-down walk
per XZ column and index the palette's validated state ids directly. This avoids
re-reading packed sections for the same metadata while preserving the packed
block output and the independently rescanned heightmap values.

After a column enters FEATURES, an edit rescans its affected XZ column once for
all three client maps. The scan records each predicate's own first matching
state, so `MOTION_BLOCKING_NO_LEAVES` can continue below a leaf while the other
maps retain their higher result; fluid and waterlogged states still use the
same motion predicate. Keep this path cache-free and compare it with the test
only naïve scan when changing a heightmap predicate or vertical bound.

The unified FEATURES dispatcher also takes its short-lived seeded-state map,
ore-transfer map, and two cross-adapter write buffers from a worker-local
scratch slot. They are cleared and returned after each dispatch, so their
capacity survives to the next chunk on that worker without a shared lock or
observable ordering change. Nested generation takes a separate slot; the
free-list is bounded to two entries per worker.

Vegetation heightmap probes cache one result per local `(x, z)` column for
each of the four predicates (`WORLD_SURFACE`, `WORLD_SURFACE_WG`,
`MOTION_BLOCKING`, and `OCEAN_FLOOR`). A surface miss walks only surface;
motion-blocking walks surface plus motion-blocking; and ocean-floor walks all
three compatible live lanes. This demand-aware ordering avoids chasing a
deeper companion that the caller did not request, while an ocean-first query
still resolves all live lanes in one downward walk. The immutable
`WORLD_SURFACE_WG` lane remains a separate source walk. Subsequent placement
modifiers become constant-time `Cell` reads. Any overlay write invalidates
only its own column for the three mutable predicates, including fixture
seeding, so later probes observe the same read-after-write state as an
uncached scan. The cache is private to a `VegGrid`; it is not shared across
worker threads and does not change source-grid immutability. The vegetation
census exposes total, primary, and companion-tail cell counts for bounded
experiments.

The Overworld ore heightmap scan uses typed base-state facts cached alongside
the dense grid's numeric palette. Built-in facts come from generated canonical
state tables; extension entries remain an explicit conservative branch. The
16×16×height loop performs only palette-fact reads, with no string path. The
focused parity control compares this path with the former string predicate
across built-in property states and an extension state. In a 2,000-round
release control over a 16×384×16 grid, the numeric scan took 192.202625ms
versus 1.71046275s for the string oracle, with both producing digest 134000.

The pre-ore packed materialisation walk also records the baseline ocean-floor
height for each centre column, so the later carver and structure passes do not
require a second full-column recount. Those passes carry a request-owned
four-word XZ mask and conservatively set a bit whenever their write path is
attempted, including writes that leave the state unchanged. Only marked
columns are rescanned before ores; an empty mask performs no post-mutation
vertical scan. Focused controls compare incremental heights and the final
block digest with a full scalar recount, bound visited cells by the marked
columns, and deliberately omit one bit to prove the stale-height control.

The five-by-five vegetation source router is chunk-aligned (`[-32, 48)` in
centre-relative coordinates). Its hot read path checks that window once and
uses shifts to select the source slot, rather than performing two floor
divisions for every block lookup. The slot table and boundary controls remain
unchanged, so padded reads still resolve to the same source or air.
Heightmap scans also retain that resolved source for the full vertical column;
the overlay remains checked per cell, but the source routing work is no longer
repeated for every Y coordinate.

Bounded density evaluation retains each thread-local `Scratch` slot's dense value and presence buffers when adjacent chunks change only their world-coordinate origin. Reconfiguration clears every presence flag before installing the new origin, so the retained allocations cannot expose a prior chunk's result; a dimension or dense/hashed-layout change still takes the existing rebuild path. The buffers are never shared between generators or worker threads.

Overworld fill requests one final-density vertical run per `(x, z)` and then
resolves the blocks through the target aquifer's status caches. This preserves
the original `lz → lx → ly` decision order while avoiding field-context setup
for every block; the scalar block path remains available to carvers and other
point consumers.

The production 4×8×4 final-density cell path evaluates its contiguous eight-lane
interpolation and shared-noodle output chunks with the crate's nightly portable
SIMD. The scalar helpers remain the exact reference and fallback for other cell
geometries. The vector lanes retain the existing lerp nesting and only commit
active mask lanes; focused field controls compare all 128 output bits across
full, partial, and empty masks, including signed zero, NaN, and cell-boundary
origins.

Each enabled aquifer instance likewise takes its per-chunk fluid-status,
aquifer-location, and preliminary-surface caches from a bounded worker-local
pool. The caches are cleared when the instance is dropped and their backing
storage is retained for the next chunk; the sampler graphs and chunk-specific
bounds remain newly configured. Disabled aquifers retain empty caches, so this
does not add work to dimensions without aquifer simulation.

The aquifer also exposes a consecutive vertical density-run boundary. Within
one 12-block grid anchor it updates the twelve candidate squared distances by
integer recurrence and preserves the existing later-candidate tie rule. The
production cell path carries that tiny state across adjacent eight-block Y
slices for each XZ column, so a slice boundary inside an anchor does not
rebuild the twelve candidates. A changed anchor or interrupted run invalidates
the state and uses the scalar path for the boundary sample; no persistent or
cross-column cache is introduced. The deterministic nonconstant seam control
measures two initializations for the reused pair versus three when each slice
starts cold, while asserting identical block output and scalar results.

Canyon carving keeps its depth-indexed width-factor table borrowed while each
ellipsoid is visited. The table is immutable for the whole tunnel, so cloning
it for every step was redundant heap traffic; the synchronous carve helper now
borrows it directly without changing the skip predicate or carve order.

Carver selection likewise borrows the generator-scoped `CarverConfig` slice for
each source chunk. The 17×17 source window therefore reuses the parsed biome
lists instead of cloning one `Vec<CarverConfig>` per source; source-biome lookup,
carver order, random seeds and block output are unchanged.


End decorated columns keep immutable source worlds in the generator-scoped
bounded memo, so scalar requests reuse overlapping three-by-three dependency
windows just like spatial batches. `DenseBlockGrid::copy_box_from` transfers
those sources into a private decoration region, and
`DenseBlockGrid::into_palette_and_blocks_box` folds the served centre directly
from the halo in the existing y-z-x order. This preserves palette insertion and
block order while avoiding a second centre-grid allocation; the End heightmap
pass likewise derives all three maps during one top-down cell scan.

The Nether surface scan memoizes each zoom corner's fiddle offsets and biome-table row in a fixed per-column cache. A 16×16×128 column touches only a small 6×34×6 corner window, so this removes repeated pseudo-random and climate-table work while preserving the exact corner selection and row lookup. Nether structure references similarly use the random-spread origin index when every configured set supports it, retaining the rectangular walk as the data-defined fallback.

Structure-set retries use a `u64` rejection mask for sets of up to 64 entries, avoiding a clone-and-remove allocation on each rejected candidate while retaining the original entry order, weights, and random draws. Larger data-defined sets use the allocation-safe vector fallback. Focused controls compare both paths for every rejection pattern through seven entries, several weight distributions and seeds, plus a 65-entry fallback case; these controls must pass before treating the optimization as parity-preserving.

The bundled Overworld's concentric-ring relocation now separates candidate RNG from biome sampling:
native builds sample the immutable climate targets in parallel, then perform the stateful biome-tree
lookups serially in their original order. On the shared arm64 host, the seed-42 constructor fell
from 49.6 s to 11.1 s in an unoptimized debug build; the release constructor remained about 2.1 s.
The target batch is discarded after the ring list is cached, so worker-local state and seed isolation
are unchanged.

Run a 256-chunk sweep in release mode:

```text
cargo run --release -p lodestone-worldgen --example throughput -- 42 16 all
```

Use `32` for 1,024 chunks, or select one dimension with `overworld`, `nether`, or `end` as the third argument. The optional fourth argument selects `shaped` or `decorated`; this is useful for a focused sampling profile. Profile the selected release binary with `samply record` to identify hot functions.

The production admission growth curve is a separate ignored server test:
`crates/lodestone-server/tests/overworld_growth_profile.rs`. It walks 256
bundled Overworld columns through both the raw source and the retained server
wrapper. It runs a contiguous walk and a stride-two walk: the latter reaches
the staged-store retention ceiling with the same 256 requests and is the
control for state-growth regressions. A separate ring-order arm uses the
connection join's actual Chebyshev admission order, so scheduler ordering is
covered without coupling this measurement to private connection code. Each
arm prints per-column latency, total throughput, staged-store wait counters,
and final retention/eviction counts. Run it in release mode on an otherwise
quiet host with `cargo test
--release -p lodestone-server --test overworld_growth_profile -- --ignored
--nocapture`. A latency cliff that coincides with evictions is a
cache-retention regression; a cliff without evictions belongs to the
intrinsic generator or its scheduling boundary.

`crates/lodestone-server/examples/bench_worldgen.rs` provides the smaller production-data control. In addition to scalar and parallel throughput, it warms the five-by-five dependency neighbourhood and prints one current per-stage sample from the same generator implementation. Pass `serial` as its third positional argument to stop before the parallel scaling sweep, which is useful when measuring a larger radius without saturating the host. Use the stage sample to choose a profiling target; it is diagnostic context rather than a regression threshold.

## How to change it

Keep the coordinate order, seed, and grid size in the command when comparing runs. Add a new dimension by extending `DimensionName`, `Generator::new`, `Generator::generate`, and the content-digest match arms. If a new generator has no shaped API, expose a base-terrain seam that does not run decoration rather than approximating shaped work by changing resolver data. Keep allocation counting and content hashing separate from timed passes because both observers change the hot path. When a shaped seam does not expose biomes or entities, the digest records zero sidecar entries rather than silently generating a different mode for verification.

The executable is intentionally not a server benchmark: do not add region-file I/O, light propagation, chunk packet encoding, or network scheduling to it. Those costs belong to their own measurements and would make the world-generation number ambiguous.

## Configuration

The positional arguments are `seed`, `grid-side`, `all|overworld|nether|end`, and `all|shaped|decorated`; defaults are `42`, `16`, `all`, and `all`. Release mode is required for representative throughput. The sample allocation count is capped at 16 chunks per mode so a full-grid run remains practical.

## Dependencies

The example uses the production constructors in `lodestone-server::worldgen_data`, the three generators in `lodestone-worldgen`, `memory-stats` for resident-set sampling, and `libc`'s process resource counter on Unix. It does not depend on persistence, lighting, packet, or transport modules.
