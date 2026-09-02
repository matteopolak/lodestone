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

A structure's pieces reach the grid one of three ways: **eager blocks** built once at start time
against a `StartContext` (every coded piece — pyramids, mineshafts), a **template** placed by
`structure_place_stage` (shipwreck, ocean ruin, igloo, ruined portal, every jigsaw structure), or a
**refinement** the placement stage runs against the chunk's real, already-surfaced-and-carved grid
(`buried_treasure`'s chest, whose termination condition needs a material distinction that does not
exist yet at start time).

**Per-chunk independence forces every random draw to be position-seeded, never chunk-order
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
and is filtered out of what actually reaches a chunk (vanilla itself treats a start with no children
as invalid). As of this writing the remaining gaps are: `monument` and `stronghold` (coded pieces not
yet ported — 2,000 and 1,766 lines of Java respectively), a Nether-only `ruined_portal_nether`
variant (parses but is refused — its `in_nether` placement branch is unverified here), a handful of
narrow per-structure deviations each documented at its own ledger key (loot living in a block's own
NBT rather than a structure-block marker, a coded chest's facing always reading `north`, a decoration
step whose RNG is position- rather than chunk-order-seeded), and `end_city` (template-piece, not
jigsaw — see `worldgen-dimensions.md`). Two ledger rows have previously named the *wrong* gap
(claiming worldgen lacked block entities/loot tables when it had gained both) — a stale ledger row is
worse than none, because it hides the real gap from the reader who came looking; keep ledger text
current when you close or narrow a gap.

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
  post-surface/post-carve material.
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
identical data reading zero. `concentric_rings` (stronghold) placement math is verified only against
its record definition; the oracle world's generated area does not reach a stronghold ring.

## Configuration

None. Everything is data through `Resolver::{structure_set_ids, structure_set, structure,
structure_template, biome_tag}` — a resolver supplying none of them gets an inert engine (every
fixture resolver in the workspace deliberately does, which is what keeps the JVM parity fixtures
byte-identical while production places structures).

## Dependencies

`lodestone-worldgen-core`'s `rng` (seed derivations) and `density::Resolver`; `lodestone-worldgen`'s
`aquifer` (start-time column sampling) and `biome` (the climate/biome filter); the bundled corpus —
1,606 files byte-verified against the 26.2 server jar under `crates/lodestone-server/assets/`, with a
SHA-256 manifest as the drift gate rather than a duplicated copy — never hand-edit a bundled asset,
re-extract with `just regen-worldgen-structures`. `lodestone-core`'s NBT codec and `flate2` for
reading gzipped templates. Server-side wiring (`worldgen_data.rs`'s `Resolver` overrides,
`chunk_nbt`'s NBT writer, `EMBEDDED_STRUCTURE_TEMPLATES`) is what makes structures reach a served
world; see `docs/worldgen.md` for the generator this composes into and `docs/worldgen-dimensions.md`
for the Nether's own structure stage.
