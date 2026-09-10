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
structure_starts_stage    which chunk starts which structure (placement.rs, mod.rs)
  ↓
structure_refs_stage      17×17 walk -> which chunks a start's box reaches
  ↓
beardifier_for(cx, cz)    terrain adaptation input for the fill stage (beardifier.rs)
  ↓
fill_stage                shape, with the beard term added at the final_density call site
  ↓
structure_place_stage     write every referenced start's pieces into this chunk (structures.rs)
```

A structure's pieces reach the grid one of five ways: **eager blocks** built once at start time
against a `StartContext` (the ordinary coded pieces), a **template** placed by
`structure_place_stage` (shipwreck, ocean ruin, igloo, ruined portal, every jigsaw structure), or a
**refinement** the placement stage runs against the chunk's real, already-surfaced-and-carved grid
(`buried_treasure`'s chest, whose termination condition needs a material distinction that does not
exist yet at start time). Stronghold writes are a fourth, ordered post-surface list: its enclosing
selector boxes skip a candidate only when the current state is air, while later decorations remain
unconditional. Keeping the guarded and unguarded writes in one list preserves their source order.

Each template completes a typed attachment-survival pass before the next piece is placed. The pass
revisits the written positions and their boundary, removes unsupported face attachments (including
ladders, wall signs, wall torches and wall banners) and below-supported rails and pressure plates,
then propagates removals to adjacent candidates. It uses generated block-state properties, collision
shapes and support predicates rather than parsing state names in the placement loop. This is the
survival part of the neighbour-shape lifecycle; connection-state recomputation for fences, walls,
panes, stairs and rails remains a separate extension point.

Concentric-ring sets are generator-wide: `StructureRegistry` resolves the placement set's preferred
biome holder-set, searches the 112-block square around each initial candidate at quart resolution,
and caches the relocated chunk list for `starts_at`. The resulting list is consumed by the same
structure-set gates as random-spread sets, so a ring candidate reaches the ordinary biome check,
piece generator and placement stages instead of stopping at parsed placement data.

Jigsaw pools also contain **feature elements**. `PoolStore::load` resolves their placed-feature
document into a `PoolFeaturePlacement`, retaining the assembled world-space origin instead of
reducing the element to its graph-only synthetic jigsaw block. `PoolFeaturePlacement::place` hands
that origin and the caller's structure random stream to the existing vegetal feature driver; its
placement modifiers therefore draw in document order and write into the same clipped grid as a
template. The placement-stage anchor must enumerate each feature element alongside template
placements: after adding the feature descriptor to `StructurePiece`, call it with the structure
stream and placement grid before later decoration stages. Do not reseed it from the decorating chunk
or substitute the chunk origin — either changes both its candidate positions and random sequence.

`StructureRegistry::feature_placement_key` uses the complete runtime registry's resource-location
order for every generation step. This remains true after dimension filtering removes structures
that still occupy positions in the complete registry. Datapack-only ids use the filtered registry's
resource-location order as a best-effort fallback because the resolver does not expose unrelated
registry values.

`OverworldGenerator::structure_place_stage` reorders the retained references by generation step
and that same complete registry position before writing pieces. The 17×17 source-chunk walk remains
the persistence and retention order; a stable tie-break keeps starts of one structure in that walk's
order while putting different structure types in the order their decoration lifecycle consumes.

`EndGenerator` memoises each pure `(seed, origin-chunk)` start calculation behind a bounded cache.
End-city placement enumerates only the random-spread placement cells that can produce an origin in
its 33×33 window, rather than probing every coordinate in that window. Registries containing a
context-dependent ring placement fall back to the complete rectangular walk, preserving the same
candidate superset. Sharing the cached `Arc` still avoids rebuilding the same piece tree while
preserving start and piece order. The cache is cleared at its ceiling; eviction can repeat work but
cannot change bytes, because a start depends only on its seed, origin and resolver data. The memo is
protected for concurrent generators; cold misses compute outside the lock, so unrelated origins can
proceed in parallel. `end_gen` also compares sequential and concurrent raw columns, including palette
order. On the release End fixture this reduced a cold 8×8 sweep from 125 to 132 chunks/s with one
worker and from 481 to 554 chunks/s with eight workers; all worker counts produced the same SHA-256
content digest.

Structure JSON crosses a strict serde boundary in `structure::json`: placement
records, jigsaw configurations, pool aliases, and template-pool elements use
closed discriminated enums and deny unknown fields. Wrong primitive types and
unknown variants become path-prefixed load errors instead of silently taking a
default. Processor lists and placed-feature bodies remain explicit string-or-
inline unions because those registry payloads have their own polymorphic
schemas; they are handed to their existing parsers unchanged.

Mineshaft starts eagerly retain their complete tree and bounding boxes, because the vertical shift
depends on the finished tree. Their block-writing walk is replayed for the decorating chunk instead:
the liquid-shell refusal is clipped to that chunk before the piece writes. This matters at a chunk
border, where water just outside the current chunk must not discard a corridor that is otherwise
valid inside it. Reconstructing the tree uses the owning start's stream, while its block walk uses
the target chunk's `underground_structures` decoration stream at the structure's captured
runtime-registry index within that step. That keeps the tree stable and makes probabilistic corridor
output follow the chunk that receives it. The remaining pre-surface reads stay explicitly recorded in
the structure ledger.

A ruined portal combines the latter two forms: the template first writes the frame, then its
placement-time refinement grows the netherrack skirt and downward columns and adds optional vines or
leaves. The refinement receives the target chunk's `surface_structures` stream, reset at the portal's
runtime registry index and shared by starts of that portal entry. The receiving grid still clips writes
to its own 16×16 columns, so the random sequence is the same lifecycle input even when the portal halo
crosses a chunk border.

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
entity data markers remain a server-side consumer concern. End-city chest markers now resolve to the bundled treasure
table in the server-side structure pass. `end_city` has its
piece generator and is consumed by the End dimension's structure stage. Both ruined-portal variants build a template
piece and run their post-template terrain refinement in their dimension's placement stage. Narrow per-structure deviations have their own ledger keys (for example,
a decoration step whose RNG is position- rather than
chunk-order-seeded). Coded chest facing is resolved from the receiving grid immediately before its write,
so the reorientation does not spend or shift the coded loot seed stream. Mineshaft block replay is already clipped per decorating chunk; its remaining
mineshaft-specific ledger row is the pre-surface world-read limitation. Template containers whose loot table lives in their own NBT, coded containers,
buried-treasure chests, and resolved pool feature elements are server- and placement-stage connected.
The desert-pyramid roof's world-seeded positional picks are wired through structure generation;
these completed paths must stay off the ledger. A stale
ledger row is worse than none, because it hides the real gap from the reader who came looking; keep ledger
text current when you close or narrow a gap.

## How to change it

- **Adding a structure with a template**: add a `StructureKind` variant, list its templates, and
  write its `*_pieces` function transcribing both the vanilla `generatePieces` call *and* its
  `postProcess` height fix-up — the second half is where real positioning lives, and porting only the
  first places the structure at the wrong Y.
- **Adding a coded structure** (no template): write its generator against the `coded::Builder`
  helper, which accumulates the whole eager block list; nothing else in the engine needs to change.
  Watch for local-vs-world coordinate confusion (orientation changes which axis "local Z" counts
  along) and remember `setOrientation` mirrors/rotates per a fixed table, not a general rule.
- **Adding a jigsaw structure**: verify block NBT survives template parsing (a jigsaw block's whole
  configuration — pool, target, joint — lives in the block's own NBT compound, which an ordinary
  placement loop would discard) and that the assembly RNG order matches vanilla's shuffle exactly,
  including that the element list is **weight-expanded before** the shuffle.
- **A piece that needs a *material* distinction not available at start time** (buried treasure's
  chest walk) is the case for `PieceRefinement`, run at placement time against the real grid — reach
  for the eager-blocks path first and only use this when the piece's own logic genuinely needs
  post-surface/post-carve material. A single ordered write list is also appropriate when only some
  writes need the real grid: store the predicate alongside each write, rather than separating
  guarded output from later unguarded decoration and changing overwrite order.
- **Never widen a structure's read/write neighbourhood without re-deriving the store's retention and
  pin radius** — see `docs/worldgen.md`'s staged-store guidance; a structure phase was the one that
  broke this rule once, by adding a stage above an existing pinned closure rather than by touching a
  driver.
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
The ruined-portal terrain stream and full post-template write walk use the external JVM capture from
`scripts/worldgen-oracle/RuinedPortalTerrainOracle.java`, recorded at
`crates/lodestone-worldgen/tests/support/coded_ruined_portal_terrain_external.txt`; the same portal
geometry is run against three target chunks, and the stream-derived distance draw plus resulting
netherrack hash differ from the position-only control.

## Configuration

None. Everything is data through `Resolver::{structure_set_ids, structure_set, structure,
structure_template, biome_tag}` — a resolver supplying none of them gets an inert engine (every
fixture resolver in the workspace deliberately does, which is what keeps the JVM parity fixtures
byte-identical while production places structures).

## Dependencies

`lodestone-worldgen-core`'s `rng` (seed derivations) and `density::Resolver`; `lodestone-worldgen`'s
`aquifer` (start-time column sampling), `biome` (the climate/biome filter), and
`feature::vegetation` (resolved pool-feature placement); the bundled corpus —
1,606 files byte-verified against the 26.2 server jar under `crates/lodestone-server/assets/`, with a
SHA-256 manifest as the drift gate rather than a duplicated copy — never hand-edit a bundled asset,
re-extract with `just regen-worldgen-structures`. `lodestone-core`'s NBT codec and `flate2` for
reading gzipped templates. Server-side wiring (`worldgen_data.rs`'s `Resolver` overrides,
`chunk_nbt`'s NBT writer, `EMBEDDED_STRUCTURE_TEMPLATES`) is what makes structures reach a served
world; see `docs/worldgen.md` for the generator this composes into and `docs/worldgen-dimensions.md`
for the Nether's own structure stage.
