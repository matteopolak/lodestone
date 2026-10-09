# Structure generation

## What it is

The structure engine: deciding which chunk gets which structure for a seed and turning that into blocks. It covers jittered-grid and concentric-ring placement, `.nbt` templates and processors, jigsaw pool assembly, hand-coded piece generators, and the beardifier that reshapes terrain under an adaptation-bearing structure, all over a bundled, byte-verified copy of the reference structure data.

## How it works

```text
Terrain263::structure_starts(cx, cz)   which structures start in this chunk (placement.rs, mod.rs)
  ↓
Terrain263::structure_refs(cx, cz)     17x17 candidate mask -> which starts' boxes reach this chunk
  ↓
Terrain263::beardifier_for(cx, cz)     terrain adaptation term for the 26.3 density fill
  ↓
Terrain263::place_structures(...)      write every referenced start's pieces, one step at a time
```

All four live in `lodestone_worldgen::terrain263::structures`. Starts sample a `StartContext` over the 26.3 base terrain (column heights and fluid kinds before surface rules and carvers; biomes from the climate tree), so a jigsaw start's ground-hugging Y matches the real server.

### Reference gathering and caches

- Only placement-cell origins that can fall inside the 17x17 source window are enumerated. Before requesting a source start, the origin index applies the full context-aware placement, frequency and exclusion predicate; start evaluation keeps its own eligibility gate before weighted selection, biome, bounding-box and portal-spill checks. They use separate deterministic RNGs and the index never advances the selection or piece stream.
- Random-spread sets invert by their cell math; ring sets use the context-aware origin index (with an empty fast path far from the ring). Candidates sort by source chunk to keep reference order. A placement type without an exact inverse falls back to the rectangular walk.
- For the bundled catalog, candidates compact into a region-local `u32` mask (one bit per structure-set index per source chunk), iterated source-X/Z with low bits first: byte-equivalent to the sorted walk. Datapacks with more than 32 sets keep the tuple walk. Counters on reference computations, cell probes and raw ring-reach builds separate fewer probes from faster evaluation.
- `Structures263` caches starts per origin chunk and references per target chunk (references restart after 8,192 entries). Eviction can repeat work but never change output.
- Index filtering must stay independent of target terrain and completed start bounds, its source mask must describe every eligible set at that origin, and a region-specific reach rejection must never be stored as a complete empty answer in the shared slot. Use the context-aware gate when extending placements; frequency-zero, frequency-one and exclusion controls separate eligible from merely possible origins.
- Production dimension constructors build each registry against the biomes their sampler can reach, dropping unreachable sets before any walk while preserving resource-location order for draws. Generic and fixture callers may use the unfiltered constructor.
- The biome gate uses a borrowed membership query (no id allocation per candidate); pre-surface block-kind reads retain only the previous coordinate's result.

### How pieces reach the grid

Five routes: eager blocks built at start time against a `StartContext` (ordinary coded pieces); a template placed by `place_structures` (shipwreck, ocean ruin, igloo, ruined portal, every jigsaw structure); a refinement run at placement against the real surfaced-and-carved grid (`buried_treasure`'s chest, whose termination needs a material distinction not present at start); a stronghold post-surface ordered list where enclosing selector boxes skip a candidate only when the state is air and later decorations are unconditional (one list preserves source order); and a ruined portal's template-then-refinement combination.

- Writes keep a bounded trace at the grid boundary: each in-bounds canonical `StateId` write records its owning start, decoration step, per-source ordinal, destination and requested state (repeated and same-state writes observable). Packet-facing consumers use the trace rather than diffing blocks.
- Template palettes and processor outputs are bound to `StateId` at load; placement transforms resolve typed properties back to a state id, so no state string is parsed during placement. Block substitutions start from the destination's default state and overlay only selected source properties (waterlogging keeps its default).
- Fortress starts keep both the eager `Arc<Vec<CodedBlock>>` and a typed runtime descriptor (piece kind, facing, chest decision, end-cap seed). Placement reuses the blocks and only runs terrain-dependent support columns and chest finalization; it must not regenerate the random tree. Starts without a descriptor use a legacy replay fallback.
- Parsed blueprints are shared across seed changes only for fingerprinted immutable bundles (at most eight in a process cache); dynamic resolvers bypass it.
- Mineshaft starts keep the complete tree and boxes eagerly (the vertical shift depends on the finished tree); the block walk replays per decorating chunk against its post-surface post-carve grid, so liquid-shell refusal and support probes see real cave air and fluid (water just outside a chunk border must not discard a valid corridor). The tree uses the owning start's stream; the walk uses the target chunk's `underground_structures` stream at the structure's runtime-registry index. The remaining pre-surface read is the vertical shift, on the ledger.
- A ruined portal's refinement uses the target chunk's `surface_structures` stream, reset at the portal's registry index; the grid clips to its own columns so the sequence is the same whichever chunk receives a write.

### Rings, pools, ordering, JSON

- Concentric-ring sets are generator-wide: `StructureRegistry` resolves the preferred biome holder-set, searches a 112-block square around each initial candidate at quart resolution and caches the relocated list for `starts_at`. Fingerprinted bundles share it across equivalent seeds (process cache capped at 32); dynamic resolvers use a registry-local cache. Each probe's immutable climate target is sampled once (parallel on native) while biome-tree lookups and cursor history run in probe order. The list feeds the ordinary biome check, piece generator and placement. Queries outside the relocation halo use an immutable empty view, identical to the full list.
- Pool feature elements: `PoolStore::load` resolves the placed-feature document into a `PoolFeaturePlacement` keeping the assembled world-space origin; `PoolFeaturePlacement::place` hands that origin and the structure's random stream to the feature-pool interpreter ([structure feature-pool features](worldgen-structure-pool-features.md)), so modifiers draw in document order into the same clipped grid. The placement-stage anchor must enumerate feature elements with template placements, using the structure stream, never a reseed from the decorating chunk or its origin.
- `StructureRegistry::feature_placement_key` uses the complete runtime registry's order for every step, even after dimension filtering (datapack-only ids fall back to the filtered order). `place_structures` orders references by generation step then that registry position, with a stable tie-break keeping one structure's starts in the 17x17 walk order (the persistence order).
- Structure JSON crosses a strict serde boundary (`structure::json`): placement records, jigsaw configs, pool aliases and pool elements are closed enums with denied unknown fields, and errors are path-prefixed. Processor lists and placed-feature bodies stay string-or-inline unions handed to existing parsers.
- Target jigsaw candidates at a template-local origin borrow palette/rotation records and shuffle a request-local index vector (stable descending priority sort over indices; same shuffle draws and attachment order; empty and singleton queries allocate nothing; list elements delegate to the first element).

### Invariants

- Per-chunk independence forces every eager draw to be position-seeded. The reference resolves much lazily on first touch and mutates shared state (a template piece's final Y, a coded piece's average ground height, a decoration-time draw); this engine generates chunks independently and caches them, so each is answered once from a pure function of `(seed, chunk)`, sometimes with an area-weighted approximation. This is a documented divergence, tracked per structure on the ledger.
- RNG draw order is the specification. A wrong fan-out, shuffle direction, weight expansion or position of a draw relative to the biome filter desyncs everything downstream while still producing a plausible structure. Grid-cell math is floor division (`div_euclid`) and block-to-quart is `>> 2`, not `/ 4`.
- Piece generation stays lazy for most structures: a candidate failing its biome-position filter must consume no RNG. Mineshafts and jigsaw structures are eager by construction (their generation point depends on the piece tree) and carry the half-consumed stream across the filter in a `Stub`.

### The ledger

`StructureRegistry::unsupported()` names every structure, set entry or placement type parsed but not fully generated, with a reason. A ledger structure still gets a start but no children, and is filtered out (a start with no children is invalid). Fortress builds its recursive piece tree; mansion places its shell, corridors, rooms and roofs (entity data markers are the server's concern); end-city chest markers resolve to the bundled treasure table and the attachment pass materializes remaining state-owned block entities (including template chests with no loot marker, which still reach the packet as empty containers); `end_city` has its generator in the End structure stage; both ruined-portal variants run template and refinement. Coded chest facing is resolved from the receiving grid just before the write without shifting the loot stream. Template containers with NBT loot tables, coded containers, buried-treasure chests, pool feature elements and the desert-pyramid roof's world-seeded picks are connected and must stay off the ledger. Keep it current; a stale row hides the real gap.

## How to change it

- Template structure: add a `StructureKind` variant, list templates, write its `*_pieces` covering both piece generation and the height fix-up (the second half is real positioning).
- Coded structure: write a generator against `coded::Builder`; watch local-vs-world axes (orientation changes which axis local Z counts along; mirrors and rotations follow a fixed table).
- Jigsaw structure: verify block NBT survives template parsing (pool, target and joint live in the jigsaw block's own compound) and the element list is weight-expanded before the shuffle. `JigsawBlockInfo` keeps `name`, `pool`, `target` backed by NBT via `JigsawText`; valid `front`/`top` stay typed and malformed text keeps its legacy fallback; do not return to per-scan string copies. Local target queries select the palette at `[0, 0, 0]`, keep stable shuffled ties and consume the same RNG suffix as owned queries; keep the permutation, reversed-tie negative control, bundled-record and assembly fixtures when touching `LocalJigsawBlocks`.
- Pieces needing a post-surface material distinction use `PieceRefinement`; try eager blocks first. For mixed guarded and unguarded writes, keep one ordered list with the predicate beside each write.
- Never widen a read or write neighbourhood without re-deriving the decoration window: `place_structures` sees only the decorating chunk's blocks, bounded by `REFS_RADIUS`, `BEARD_REACH` and `PORTAL_TERRAIN_REACH`.
- The exclusion-zone walk is one level deep, matching 26.2 data; a datapack chaining two needs a recursive walk.

## Evidence

Every closed set (placement cannot reject a biome-valid candidate) is verified both ways against a reference-authored save (`.cache/mc/survival/world`, seed -195764831, generated before this engine existed): every recorded start reproduces at its chunk with zero extras in a large window. Template and coded block correctness is checked by signature block counts against a structure-free zero control. Stronghold (`concentric_rings`) uses an external JVM fixture for the xoroshiro stream and synthetic all-preferred relocation through `StructureRegistry::starts_at`; the oracle world does not reach a real ring, so full save parity is open. Coded chest facing has `crates/lodestone-worldgen/tests/support/coded_chest_reorient_external.txt` (vertical-neighbour control) plus a jungle-temple production gate; the desert-pyramid roof and chest stream use `scripts/worldgen-oracle/PyramidRoofOracle.java` captured in `coded_pyramid_roof_external.txt` (four asymmetric seeds reject the old fixed fork; chest rolls unchanged).

## Configuration

None. Everything is data through `Resolver::{structure_set_ids, structure_set, structure, structure_template, biome_tag}`; a resolver supplying none gets an inert engine, which fixture resolvers deliberately do so JVM parity fixtures stay byte-identical.

## Dependencies

`lodestone-worldgen-core` (`rng`, 26.3 start-time sampling and climate tree), `lodestone-worldgen` (`resolver::Resolver`, `feature`), the bundled corpus (2,012 files byte-verified against the 26.3 jar under `crates/lodestone-server/assets/`, SHA-256 manifest as drift gate; never hand-edit, re-extract with `just regen-worldgen-structures`), `lodestone-core` NBT and `flate2`. Server wiring (`worldgen_data.rs` resolver overrides, `chunk_nbt`, `EMBEDDED_STRUCTURE_TEMPLATES`) puts structures in a served world; see [world generation](worldgen.md).
