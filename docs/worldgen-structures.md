# Structure generation

## What it is

The structure engine: deciding which chunk gets which structure for a seed, and turning that
decision into real blocks — jittered-grid and concentric-ring placement, `.nbt` structure templates
and their processors, jigsaw pool assembly, hand-coded piece generators for structures with no
template, and the beardifier that reshapes terrain underneath an adaptation-bearing structure. Built
in phases (S1 placement, S2 templates, S3 beardifier, S4 jigsaw, S5+ coded pieces, mineshaft, and
per-structure closures since), on top of a bundled, byte-verified copy of vanilla's structure data.

## How it works

```text
Terrain263::structure_starts(cx, cz)   which structures start in this chunk (placement.rs, mod.rs)
  ↓
Terrain263::structure_refs(cx, cz)     17×17 candidate mask -> which starts' boxes reach this chunk
  ↓
Terrain263::beardifier_for(cx, cz)     terrain adaptation term for the 26.3 density fill
  ↓
Terrain263::place_structures(...)      write every referenced start's pieces, one step at a time
```

All four live in `lodestone_worldgen::terrain263::structures`. Starts sample a `StartContext` over
the 26.3 base terrain (column heights and fluid kinds before surface rules and carvers, and biomes
from the 26.3 climate tree), so a jigsaw start's ground-hugging Y matches the real server.

Reference gathering enumerates only placement-cell origins that can fall inside its
17×17 source window. Before requesting a source start, the origin index applies
the complete context-aware placement, frequency and exclusion predicate. Rejected
set memberships cannot produce a start, so they need no source-stage computation.
Start evaluation keeps its own eligibility gate before weighted selection, biome,
bounding-box and portal-spill checks. These gates use separate deterministic RNGs;
the index never advances the weighted selection or piece-generation stream.
Random-spread sets are
inverted by their cell math; ring sets use the existing context-aware origin index, including its empty
far-from-ring fast path. The candidate list is sorted by source chunk so retained
reference order is unchanged. A placement type without an exact inverse keeps the
complete rectangular walk as its fallback.

For the bundled catalog, candidate tuples are compacted into a region-local `u32` mask: one bit
represents one structure-set index at a source chunk. Iterating the rectangular mask in
source-X/source-Z order and consuming set bits from low to high is byte-equivalent to the prior
sorted tuple walk. Datapacks exposing more than 32 set indices retain that tuple walk as an
explicit fallback. Reference computations, placement-cell probes and one-time raw ring-reach
builds have counters so throughput runs can distinguish fewer candidate probes from faster start
evaluation.
`Structures263` caches starts per origin chunk and references per target chunk; the reference
cache restarts after 8,192 entries, each a handful of `Arc`s. Eviction can repeat work but cannot
change output, because a start depends only on its seed, origin and resolver data.

Keep index filtering independent of target terrain and completed start bounds.
Its source mask must describe every eligible set at that origin, not only sets
that affect the current target. A region-specific reach rejection cannot be
stored as a complete empty answer in the shared source-start slot. When extending
placements, use the context-aware gate so ring relocation and exclusion use the
same origin lists as start evaluation. Frequency-zero, frequency-one and exclusion
controls distinguish eligible origins from merely possible placement-cell origins.

Production dimension constructors build each registry against the set of biomes that their sampler
can reach. This removes structure sets that cannot pass the dimension's biome gate before any
placement-cell walk or structure-start evaluation, while preserving the resource-location order of
the retained registry for all random draws and placement ordering. Generic and fixture callers may
still use the unfiltered constructor.

The start biome gate uses the context's borrowed membership query, so the production sampler does
not allocate a biome id for every candidate. Its pre-surface block-kind reads retain only the
immediately previous coordinate result; this is enough for eager mineshaft predicates' repeated
checks without retaining a terrain region or allocating a cache table.

A structure's pieces reach the grid one of five ways: **eager blocks** built once at start time
against a `StartContext` (the ordinary coded pieces), a **template** placed by
`Terrain263::place_structures` (shipwreck, ocean ruin, igloo, ruined portal, every jigsaw structure), or a
**refinement** the placement stage runs against the chunk's real, already-surfaced-and-carved grid
(`buried_treasure`'s chest, whose termination condition needs a material distinction that does not
exist yet at start time). Stronghold writes are a fourth, ordered post-surface list: its enclosing
selector boxes skip a candidate only when the current state is air, while later decorations remain
unconditional. Keeping the guarded and unguarded writes in one list preserves their source order.

Structure products retain a bounded write trace at the grid boundary. Each in-bounds canonical
StateId write records its owning start, declared decoration step, per-source ordinal, destination
and requested state before assignment; repeated writes and same-state writes remain observable.
The local grid interner is crossed once at the write boundary, and packet-facing consumers should
use this trace instead of reconstructing history from a final block diff.

Template palettes and processor outputs are bound to canonical StateId values while the template is
loaded. Placement transforms resolve typed properties back to a state id before the grid boundary,
so no per-block state string is parsed or interned during structure placement. Block substitutions
start from the destination block's default state and overlay only the selected source properties;
other destination properties, including waterlogging, keep their defaults.

Fortress starts retain both their eager `Arc<Vec<CodedBlock>>` piece output and a typed runtime
descriptor containing piece kind, facing, chest decision, and end-cap seed. Placement reuses the
eager blocks and runs only the terrain-dependent support columns and chest-facing/loot finalization;
it must not regenerate the random piece tree. Starts produced without that descriptor retain the
legacy replay fallback, which is useful when extending persisted or fixture-created start formats.

Parsed structure blueprints are shared across seed changes only for fingerprinted immutable asset
bundles. The process cache retains at most eight blueprints; live registries own their products, so
eviction can only repeat parsing and never invalidate generation. Dynamic resolvers bypass it.

Concentric-ring sets are generator-wide: `StructureRegistry` resolves the placement set's preferred
biome holder-set, searches the 112-block square around each initial candidate at quart resolution,
and caches the relocated chunk list for `starts_at`. Fingerprinted asset bundles also share that
immutable list across equivalent seed/configuration generators; dynamic resolvers stay on a
registry-local cache. The process cache is capped at 32 products; eviction can repeat work but
cannot alter placement. During each ring set's build, each unique probe coordinate's immutable climate
target is sampled once (in parallel on native targets), while each biome-tree lookup and its cursor
history still runs in probe order. The resulting list is consumed by the same structure-set gates as
random-spread sets, so a ring candidate reaches the ordinary biome check, piece generator and
placement stages instead of stopping at parsed placement data.
Queries outside the initial-candidate relocation halo use an immutable empty view, so the
first ordinary spawn-area column does not pay the stronghold relocation scan. A later query
near a possible ring position materializes the full list, which is identical either way.

Jigsaw pools also contain **feature elements**. `PoolStore::load` resolves their placed-feature
document into a `PoolFeaturePlacement`, retaining the assembled world-space origin instead of
reducing the element to its graph-only synthetic jigsaw block. `PoolFeaturePlacement::place` hands
that origin and the caller's structure random stream to the feature-pool interpreter
([structure feature-pool features](worldgen-structure-pool-features.md)); its placement modifiers
therefore draw in document order and write into the same clipped grid as a template. The placement-stage anchor must enumerate each feature element alongside template
placements: after adding the feature descriptor to `StructurePiece`, call it with the structure
stream and placement grid before later decoration stages. Do not reseed it from the decorating chunk
or substitute the chunk origin — either changes both its candidate positions and random sequence.

`StructureRegistry::feature_placement_key` uses the complete runtime registry's resource-location
order for every generation step. This remains true after dimension filtering removes structures
that still occupy positions in the complete registry. Datapack-only ids use the filtered registry's
resource-location order as a best-effort fallback because the resolver does not expose unrelated
registry values.

`Terrain263::place_structures` orders the retained references by generation step and that same
complete registry position before writing pieces. The 17×17 source-chunk walk remains
the persistence and retention order; a stable tie-break keeps starts of one structure in that walk's
order while putting different structure types in the order decoration consumes them.

Structure JSON crosses a strict serde boundary in `structure::json`: placement
records, jigsaw configurations, pool aliases, and template-pool elements use
closed discriminated enums and deny unknown fields. Wrong primitive types and
unknown variants become path-prefixed load errors instead of silently taking a
default. Processor lists and placed-feature bodies remain explicit string-or-
inline unions because those registry payloads have their own polymorphic
schemas; they are handed to their existing parsers unchanged.

Target jigsaw candidates at template-local origin borrow the existing palette/rotation
records and shuffle a request-local index vector. Stable descending selection-priority
sorting moves indices instead of cloning and moving full records; the shuffle draw
sequence and attachment order are unchanged. Source-piece queries still return owned,
translated records. Empty and singleton queries need no index allocation. List
elements delegate only to their first element, and feature elements retain their
synthetic record. This adds no retained cache or working set.

Mineshaft starts eagerly retain their complete tree and bounding boxes, because the vertical shift
depends on the finished tree. Their block-writing walk is replayed for the decorating chunk against
that chunk's post-surface, post-carve block grid: the liquid-shell refusal and support/floor probes
see cave air, fluids and surface material already present when the piece writes. This matters at a
chunk border, where water just outside the current chunk must not discard a corridor that is otherwise
valid inside it. Reconstructing the tree uses the owning start's stream, while its block walk uses
the target chunk's `underground_structures` decoration stream at the structure's captured
runtime-registry index within that step. That keeps the tree stable and makes probabilistic corridor
output follow the chunk that receives it. The remaining pre-surface read is the eager tree's vertical
shift, and stays explicitly recorded in the structure ledger.

A ruined portal combines the latter two forms: the template first writes the frame, then its
placement-time refinement grows the netherrack skirt and downward columns and adds optional vines or
leaves. The refinement receives the target chunk's `surface_structures` stream, reset at the portal's
runtime registry index and shared by starts of that portal entry. The receiving grid still clips writes
to its own 16×16 columns, so the random sequence is the same whichever chunk receives a write, even
when the portal halo crosses a chunk border.

**Per-chunk independence forces every eager structure draw to be position-seeded, never chunk-order
dependent.** Vanilla resolves a lot of structure state lazily, the first time any chunk touches it,
and mutates a shared object other chunks then read back — a template piece's final Y, a coded
piece's average ground height, a decoration-time RNG draw. This engine generates chunks
independently and caches them, so "whichever chunk got there first" is not available; every one of
those questions is instead answered once, eagerly, from a pure function of `(seed, chunk)` — even
where that means computing an area-weighted approximation of what vanilla's own per-chunk-order
answer would have been. This is a deliberate, documented divergence, not an oversight, and it is
tracked per-structure on the ledger below.

**RNG draw order is the whole specification, not an implementation detail.** Structure assembly is
one long stream out of a seeded `WorldgenRandom`; a placement modifier, a jigsaw shuffle, a piece's
block-writing helper all draw in a fixed sequence and count. Getting a fan-out, a shuffle direction,
a weight-expansion, or which side of a biome-filter check a draw sits on wrong desyncs everything
downstream in that chunk (and, for shared streams, in that structure) while still producing a
structure that looks plausible. Grid-cell math is floor division (`div_euclid`), not truncating,
and vanilla's own block-to-quart conversion is `>> 2`, not `/ 4` — both common transcription
mistakes here.

**Piece generation is lazy in vanilla and must stay lazy for most structures.** A candidate that
fails its biome-position filter must consume no RNG, or every later structure at that seed moves.
Two structure families are the exception and are eager by construction: a mineshaft's and a
jigsaw structure's own generation point *depends on* their piece tree (mineshaft's vertical shift,
a jigsaw structure's assembled bounding box), so both build their whole piece list before the biome
filter can even run, carrying the half-consumed RNG stream across it in a `Stub`.

### The ledger

`StructureRegistry::unsupported()` names every structure, structure-set entry or placement type the
registry parsed but cannot fully generate, with a reason — read it rather than assuming coverage.
A structure on the ledger still gets a start when placement and biome say so, but with no children
and is filtered out of what actually reaches a chunk (a start with no children is invalid). Nether `fortress`
now builds its recursive coded piece tree, and Overworld `mansion` places its seeded shell, corridors, rooms and roofs;
entity data markers remain a server-side consumer concern. End-city chest markers resolve to the bundled treasure
table in the server-side structure pass. After preserving those filled containers and patterned banner payloads, the
same attachment pass materializes any remaining state-owned block entities; this includes template chest blocks with
no loot marker, which must still reach the packet as empty containers. `end_city` has its
piece generator and is consumed by the End dimension's structure stage. Both ruined-portal variants build a template
piece and run their post-template terrain refinement in their dimension's placement stage. Narrow per-structure deviations have their own ledger keys (for example,
a decoration step whose RNG is position- rather than
chunk-order-seeded). Coded chest facing is resolved from the receiving grid immediately before its write,
so the reorientation does not spend or shift the coded loot seed stream. Mineshaft block replay is already clipped per decorating chunk and reads the
receiving post-carve grid; its remaining mineshaft-specific ledger row is the eager tree's pre-surface vertical-shift read. Template containers whose loot table lives in their own NBT, coded containers,
buried-treasure chests, and resolved pool feature elements are server- and placement-stage connected.
The desert-pyramid roof's world-seeded positional picks are wired through structure generation;
these completed paths must stay off the ledger. A stale
ledger row is worse than none, because it hides the real gap from the reader who came looking; keep ledger
text current when you close or narrow a gap.

## How to change it

- **Adding a structure with a template**: add a `StructureKind` variant, list its templates, and
  write its `*_pieces` function covering both the reference piece generation *and* its
  post-processing height fix-up — the second half is where real positioning lives, and porting only the
  first places the structure at the wrong Y.
- **Adding a coded structure** (no template): write its generator against the `coded::Builder`
  helper, which accumulates the whole eager block list; nothing else in the engine needs to change.
  Watch for local-vs-world coordinate confusion (orientation changes which axis "local Z" counts
  along) and remember that orientation mirrors/rotates per a fixed table, not a general rule.
- **Adding a jigsaw structure**: verify block NBT survives template parsing (a jigsaw block's whole
  configuration — pool, target, joint — lives in the block's own NBT compound, which an ordinary
  placement loop would discard) and that the assembly RNG order matches vanilla's shuffle exactly,
  including that the element list is **weight-expanded before** the shuffle. The short-lived
  `JigsawBlockInfo` values keep `name`, `pool`, and `target` backed by that retained NBT through
  `JigsawText`; valid `front` and `top` orientations stay typed through assembly as well, while
  malformed orientation text retains its legacy fallback. Preserve the field defaults and do not
  turn those reads back into per-scan string copies.
  Local target queries must select the palette at `[0, 0, 0]`, retain stable shuffled
  ties, and consume the same RNG suffix as owned queries. Keep the independent
  permutation, reversed-tie negative control, bundled-record and assembly fixtures
  when changing `LocalJigsawBlocks` or the target query boundary.
- **A piece that needs a *material* distinction not available at start time** (buried treasure's
  chest walk) is the case for `PieceRefinement`, run at placement time against the real grid — reach
  for the eager-blocks path first and only use this when the piece's own logic genuinely needs
  post-surface/post-carve material. A single ordered write list is also appropriate when only some
  writes need the real grid: store the predicate alongside each write, rather than separating
  guarded output from later unguarded decoration and changing overwrite order.
- **Never widen a structure's read/write neighbourhood without re-deriving the decoration window.**
  `place_structures` receives only the decorating chunk's own blocks, and `REFS_RADIUS`,
  `BEARD_REACH` and `PORTAL_TERRAIN_REACH` bound how far a start may reach.
- **The exclusion-zone walk is one level deep, matching 26.2's data** (no set with an exclusion zone
  itself has one) — a datapack chaining two would need a real recursive walk.

## Evidence

Every closed structure set (one whose placement predicate cannot ever reject a biome-valid
candidate) is verified in both directions against a vanilla-authored save
(`.cache/mc/survival/world`, seed −195764831, generated months before this engine existed): every
recorded start reproduced at exactly its chunk, and zero extra starts anywhere in a large sampled
window. Block-level correctness (not just placement) is checked the same way for template and coded
structures — a signature block count at a known chunk, against a structure-free control over
identical data reading zero. `concentric_rings` (stronghold) placement uses an external JVM fixture
for the xoroshiro stream and synthetic all-preferred-biome relocation, then drives the captured chunk through
`StructureRegistry::starts_at`; the oracle world's generated area still does not reach a real
stronghold ring, so full world-save parity remains open.
The coded chest-facing rule has an independent asymmetric direction fixture at
`crates/lodestone-worldgen/tests/support/coded_chest_reorient_external.txt`, including a vertical-neighbour
control, plus a production-column gate over a jungle temple that checks both resulting facings and the
unchanged coded loot sidecar.
The desert-pyramid roof position and chest-stream gate use the external JVM capture from
`scripts/worldgen-oracle/PyramidRoofOracle.java`, recorded at
`crates/lodestone-worldgen/tests/support/coded_pyramid_roof_external.txt`; four asymmetric seeds
reject the former fixed-fork result while the four chest roll seeds remain byte-for-byte unchanged.
## Configuration

None. Everything is data through `Resolver::{structure_set_ids, structure_set, structure,
structure_template, biome_tag}` — a resolver supplying none of them gets an inert engine (every
fixture resolver in the workspace deliberately does, which is what keeps the JVM parity fixtures
byte-identical while production places structures).

## Dependencies

`lodestone-worldgen-core`'s `rng` (seed derivations) and the 26.3 engine
(start-time column sampling and the climate tree behind the biome filter); `lodestone-worldgen`'s
`resolver::Resolver` and `feature` (resolved pool-feature placement); the bundled corpus —
2,012 files byte-verified against the 26.3 server jar under `crates/lodestone-server/assets/`, with a
SHA-256 manifest as the drift gate rather than a duplicated copy — never hand-edit a bundled asset,
re-extract with `just regen-worldgen-structures`. `lodestone-core`'s NBT codec and `flate2` for
reading gzipped templates. Server-side wiring (`worldgen_data.rs`'s `Resolver` overrides,
`chunk_nbt`'s NBT writer, `EMBEDDED_STRUCTURE_TEMPLATES`) is what makes structures reach a served
world; see [world generation](worldgen.md) for the generator this composes into.
