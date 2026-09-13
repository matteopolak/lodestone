# World-generation throughput

## What it is

`crates/lodestone-worldgen/examples/throughput.rs` measures the three bundled dimensions at the world-generation boundary. It compares each dimension's shaped terrain prefix with its fully decorated column over a deterministic grid, without persistence, lighting, or packet encoding.

## How it works

The executable builds one production generator per dimension and seed, walks a row-major `N × N` chunk grid, and reports cold and warm passes. Cold starts with a fresh generator; warm repeats the same grid after the first pass so memoized stage work is visible. Overworld and Nether use `column_shaped`; End uses its pre-surface `shape_field`; all decorated paths call the public `column` method.

Each pass reports chunks per second, process CPU utilization when the host exposes `getrusage`, and sampled resident-set growth. A separate 16-chunk allocation pass uses a benchmark-local counting allocator, so allocation counts do not distort the wall-clock throughput numbers. The legacy rolling digest over non-air counts remains in the output for historical comparison, while an out-of-band SHA-256 content digest hashes every canonical block-state string, exposed biome cell, and generated block-entity sidecar (or the dimension's equivalent loot/gateway sidecar). The content pass is outside both the timed loop and allocation-counted closure, and cold/warm digests must agree.

Production Overworld batches use the source's indexed `columns` hook rather than
the scalar default. A pristine batch fans out over the shared world-generation
pool; when the hook is called from an already-admitted pool worker it joins that
same Rayon pool, avoiding a nested semaphore wait and a second worker set.
Edited columns retain the scalar path so the edit ledger remains authoritative.
The indexed collection preserves caller coordinate order, and the content digest
is the exactness control for worker scheduling.

The ordinary improved-noise entry point has a dedicated zero-scale path. It keeps the same coordinate, floor, and interpolation order as the general scaled entry while avoiding its scale/fudge branch; the focused `zero_scale_entry_is_bit_identical_to_scaled_entry` test checks the two entry points by result bits over fractional coordinates. This is a local, immutable change, so concurrent generators share no cache or mutable state.

The overworld biome stage searches its seven-axis parameter tree with an exact i64 squared-distance bound. A child visit stops accumulating once its partial sum reaches the incumbent distance; the cutoff is inclusive so equal distances remain pruned by the strict visit comparison and the incumbent tie-break does not change. The full-distance path remains in place for selected leaves and single-row roots. Tree identity controls cover cutoff equality, tied-row selection, real-table distance parity, and the fixed production fixture.

The surface stage keeps its nearby-quart footprint and climate targets in typed caches rather than cloning a `String` for every cell. Each stateful tree lookup still runs in the reference column order (`x`, `z`, descending `y`, after the column's initial query); reusing a row id would change the cursor's tie-break history. A lookup resolves the selected id back to a borrowed table name only while evaluating a surface rule, and fallback-biome contexts keep their one borrowed name.

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

The unified FEATURES dispatcher also takes its short-lived seeded-state map,
ore-transfer map, and two cross-adapter write buffers from a worker-local
scratch slot. They are cleared and returned after each dispatch, so their
capacity survives to the next chunk on that worker without a shared lock or
observable ordering change. Nested generation takes a separate slot; the
free-list is bounded to two entries per worker.

Vegetation heightmap probes cache one result per local `(x, z)` column for each
of the four predicates (`WORLD_SURFACE`, `WORLD_SURFACE_WG`,
`MOTION_BLOCKING`, and `OCEAN_FLOOR`). A probe still performs the complete
top-down scan on its first use, but repeated placement modifiers become a
constant-time `Cell` read. Any overlay write invalidates only its own column
for the three mutable predicates, including fixture seeding, so later probes
observe the same read-after-write state as an uncached scan. The immutable
`WORLD_SURFACE_WG` cache does not need invalidation because it reads source
terrain rather than the overlay. The cache is private to a `VegGrid`; it is
not shared across worker threads and does not change source-grid immutability.

The Overworld ore heightmap scan uses typed base-state facts cached alongside
the dense grid's numeric palette. Built-in facts come from generated canonical
state tables; extension entries remain an explicit conservative branch. The
16×16×height loop performs only palette-fact reads, with no string path. The
focused parity control compares this path with the former string predicate
across built-in property states and an extension state. In a 2,000-round
release control over a 16×384×16 grid, the numeric scan took 192.202625ms
versus 1.71046275s for the string oracle, with both producing digest 134000.

The five-by-five vegetation source router is chunk-aligned (`[-32, 48)` in
centre-relative coordinates). Its hot read path checks that window once and
uses shifts to select the source slot, rather than performing two floor
divisions for every block lookup. The slot table and boundary controls remain
unchanged, so padded reads still resolve to the same source or air.
Heightmap scans also retain that resolved source for the full vertical column;
the overlay remains checked per cell, but the source routing work is no longer
repeated for every Y coordinate.

Bounded density evaluation retains each thread-local `Scratch` slot's dense value and presence buffers when adjacent chunks change only their world-coordinate origin. Reconfiguration clears every presence flag before installing the new origin, so the retained allocations cannot expose a prior chunk's result; a dimension or dense/hashed-layout change still takes the existing rebuild path. The buffers are never shared between generators or worker threads.

Each enabled aquifer instance likewise takes its per-chunk fluid-status,
aquifer-location, and preliminary-surface caches from a bounded worker-local
pool. The caches are cleared when the instance is dropped and their backing
storage is retained for the next chunk; the sampler graphs and chunk-specific
bounds remain newly configured. Disabled aquifers retain empty caches, so this
does not add work to dimensions without aquifer simulation.

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

## How to change it

Keep the coordinate order, seed, and grid size in the command when comparing runs. Add a new dimension by extending `DimensionName`, `Generator::new`, `Generator::generate`, and the content-digest match arms. If a new generator has no shaped API, expose a base-terrain seam that does not run decoration rather than approximating shaped work by changing resolver data. Keep allocation counting and content hashing separate from timed passes because both observers change the hot path. When a shaped seam does not expose biomes or entities, the digest records zero sidecar entries rather than silently generating a different mode for verification.

The executable is intentionally not a server benchmark: do not add region-file I/O, light propagation, chunk packet encoding, or network scheduling to it. Those costs belong to their own measurements and would make the world-generation number ambiguous.

## Configuration

The positional arguments are `seed`, `grid-side`, `all|overworld|nether|end`, and `all|shaped|decorated`; defaults are `42`, `16`, `all`, and `all`. Release mode is required for representative throughput. The sample allocation count is capped at 16 chunks per mode so a full-grid run remains practical.

## Dependencies

The example uses the production constructors in `lodestone-server::worldgen_data`, the three generators in `lodestone-worldgen`, `memory-stats` for resident-set sampling, and `libc`'s process resource counter on Unix. It does not depend on persistence, lighting, packet, or transport modules.
