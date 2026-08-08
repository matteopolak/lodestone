# Structure placement and starts (worldgen phase S1)

## What it is

The engine that decides **which chunk gets which structure** for a seed: the two
`StructurePlacement` types, the four `frequency_reduction_method`s, per-set
weighted structure selection, per-structure start predicates, and the two new
worldgen stages (`structure_starts`, `structure_refs`) that carry the answer.
Phase **S1** of [`plans/structures.md`](./plans/structures.md) (issue #514),
built on the corpus that
[`worldgen-structure-corpus.md`](./worldgen-structure-corpus.md) bundled.

It places, it does not build. What S1 delivers is the placement answer, and that
answer is **verified in both directions against a vanilla-authored save** — see
[Evidence](#evidence).

**S2 has since landed** ([`worldgen-structure-templates.md`](./worldgen-structure-templates.md)):
shipwrecks, ocean ruins and igloos now build template pieces and write blocks, so
where this document says "no piece generator" read "no piece generator *for this
structure*" and check `StructureRegistry::unsupported` for which. **S3 has since
landed too** ([`worldgen-beardifier.md`](./worldgen-beardifier.md)): the beardifier
is a real evaluator, not a constant-zero leaf, and it consumes exactly the
`StructureRefs` seam this document describes. Jigsaw is S4 and the coded piece
generators (monument, mineshaft, stronghold, …) S5.

## How it works

```text
lodestone-worldgen/src/structure/placement.rs   the placement predicates
lodestone-worldgen/src/structure/mod.rs         registry, start predicates, ledger
lodestone-worldgen/src/overworld/structures.rs  the two stages + the column sampler
lodestone-worldgen-core/src/rng/mod.rs          set_large_feature_with_salt
lodestone-worldgen-core/src/density/mod.rs      four new Resolver methods
```

Per chunk, in structure-set registry order:

1. `placement.is_placement_chunk` — the jittered grid. `getPotentialStructureChunk`
   seeds a legacy source with `setLargeFeatureWithSalt(seed, gridX, gridZ, salt)`
   and draws the in-cell offset; the chunk is a placement chunk iff that offset
   lands on it.
2. `placement.passes_frequency` — skipped entirely (no draw) when `frequency >=
   1.0`, which is 18 of the 20 bundled sets.
3. the exclusion zone, if any — one set has one (`pillager_outposts` excludes
   within 10 chunks of a `villages` placement).
4. structure selection: a single-entry set takes its one structure; a multi-entry
   set seeds `setLargeFeatureSeed(seed, cx, cz)` and draws `nextInt(total)`
   repeatedly, removing a rejected option and its weight before the next draw.
5. the start predicate: the structure's own generation point, then the biome at
   that point (quart-wise, **including Y**) against the structure's `biomes` tag
   closure.

### Why `structure_starts` is the topmost stage

Vanilla's `ChunkStatus` order is `STRUCTURE_STARTS → STRUCTURE_REFERENCES →
BIOMES → NOISE`, and the reason is the beardifier: fill consults the structure
bounds intersecting the chunk to flatten terrain underneath. So the two new
`StageSlot`s sit *above* `pre_ore` in `ChunkStages`, and `pre_ore` gained exactly
one upstream edge — it reads its own chunk's `StructureRefs`. This inverts the
terrain-first intuition, and getting it backwards is the difference between a
village on flat ground and a village draped over a hillside.

The layering is acyclic because a start reads **no terrain product**: heights come
from `StartSampler`, which builds a fresh `AquiferSystem` and scans a column
downward, exactly as vanilla's `getBaseColumn` does. That is the load-bearing
detail — reusing `pre_ore`'s heightmap would be cheaper and would deadlock the
store's once-guards.

### The ledger

`StructureRegistry::unsupported()` names every set, structure or placement type
the registry parsed but cannot fully generate, with a reason. **Read it instead of
assuming coverage.** Today it names 30 of the 34 structures. A structure on the
ledger still gets a start when placement and biome say so, but with
`StructureStart::pieces_complete == false`, an empty `Children` list and a
placeholder box — and `OverworldGenerator::structure_starts` filters those out,
because vanilla reloads a start with no children as `INVALID`, which is worse
than absent.

Four sets are **closed** — every structure they can place has a decidable start,
so for those the answer is exactly vanilla's:

| set | structures | oracle starts (seed −195764831) |
|---|---|---|
| `shipwrecks` | shipwreck, shipwreck_beached | 11 |
| `ocean_ruins` | ocean_ruin_cold, ocean_ruin_warm | 16 |
| `buried_treasures` | buried_treasure | 2 |
| `ocean_monuments` | monument | 2 |

## Evidence

`crates/lodestone-worldgen/tests/structure_placement_oracle.rs`, against
`tests/support/structure_starts_survival.txt` — 102 starts read out of the
per-chunk `structures.starts` NBT of `.cache/mc/survival`, a world the real 26.2
server generated months before this engine existed. Nothing in the fixture passed
through this repo's encoder.

* **Positive**: all 31 closed-set starts are reproduced at exactly their chunk.
* **Negative**: over a 64×64-chunk window the oracle world has generated
  4,080/4,096 of, this engine produces **exactly** the 12 closed-set starts
  vanilla has there and no others. Without this half the positive test is
  satisfied by an engine that starts a shipwreck in every chunk.
* The ledger test asserts the ledger is non-empty and names the structures the
  gate deliberately does not cover — an accidentally-empty ledger would make the
  negative sweep pass for the wrong reason.

Both sweep tests are `#[ignore]`d (they build the production generator and sample
noise columns). Run with `--ignored --nocapture`.

**Not verified**: `concentric_rings` (stronghold). The ring math is ported from
`ChunkGeneratorStructureState.generateRingPositions` and gated by its record
definition only, because the oracle world's generated area does not reach a
stronghold ring (nearest ~1,280 blocks out). Do not report stronghold placement
as verified until the oracle world is extended under Apple `container`.

## How to change it

* **To add a structure**: add a `StructureKind` variant, parse it in
  `StructureKind::parse`, implement its arm of `find_generation_point`, and give
  `validity` an honest answer. The biome filter is applied by the caller because
  vanilla applies it uniformly in `findValidGenerationPoint`.
* **Piece generation is lazy in vanilla and must stay lazy.**
  `Structure.GenerationStub` holds `Either<Consumer<Builder>, Builder>`; the
  `Consumer` arm runs only *after* the biome filter, so a structure that fails
  its biome check consumes no RNG at all. Generating pieces eagerly "to see if it
  works" would shift the stream for every later structure in that chunk.
  `Either.right` structures (mineshaft, jigsaw) are eager **by definition** —
  their start position depends on their own pieces, which is why neither can be
  decided before S4/S5.

### Gotchas that cost real time

* **`setLargeFeatureWithSalt`'s arguments are shuffled at one of its two call
  sites.** `StructurePlacement.probabilityReducer` — the `default` reduction
  method — calls it as `(seed, salt, sourceX, sourceZ)`: the salt lands in the
  `x` parameter, chunk X in `z`, chunk Z in `blend`.
  `getPotentialStructureChunk` calls it the obvious way. Both are vanilla's and
  both are load-bearing, so the Rust method takes its parameters positionally and
  refuses to name them after their meaning.
* **The four `frequency_reduction_method`s are four different derivations**, not
  one with four thresholds. `legacy_type_1` does not use
  `setLargeFeatureWithSalt` at all and burns a `nextInt()` before its real draw;
  `legacy_type_2` substitutes vanilla's `HIGHLY_ARBITRARY_RANDOM_SALT` (10387320)
  for the set's own salt; `legacy_type_3` draws a `nextDouble` (two `next_bits`
  calls) where the others draw a `nextFloat`.
* **`triangular` spread spends two draws per axis, `linear` one.** Four sets use
  triangular (`end_cities`, `ocean_monuments`, `woodland_mansions`).
* **Grid-cell division is floor division, not truncating.** `-1 / 24 == 0` in
  Rust would put chunk −1 in cell 0 and place two structures in adjacent cells
  near the origin. `div_euclid`.
* **`QuartPos.fromBlock` is `>> 2`, not `/ 4`** — the same trap, in the
  monument's surrounding-biome scan.
* **Vanilla's persisted `buried_treasure` box is the post-placement one.**
  `BuriedTreasurePiece.postProcess` reassigns `this.boundingBox` after walking
  down to the first stone-backed position, so a freshly generated start and a
  reloaded one legitimately differ in Y (90 vs wherever it landed). Compare
  `id`/`ChunkX`/`ChunkZ`, not the box, for that structure.
* **The exclusion-zone walk is one level deep.** Vanilla recurses through the
  other set's own placement including *its* exclusion zone; in 26.2 the single
  zone points at a set with no zone, so one level is exact. A datapack chaining
  two would be silently under-excluded.

## ~~Not wired to production yet~~ — wired (`a617454d`)

**Both edits below have landed**, so `EmbeddedResolver` no longer returns an empty
`structure_set_ids` and the `structures` compound is no longer a permanently empty
stub. The section is kept because the *shape* of the wiring is the thing to know:

- `worldgen_data.rs`'s four `Resolver` overrides are the on/off switch for the
  whole engine. Every **fixture** resolver in the workspace still supplies none of
  them, deliberately — that is what keeps the 13 parity binaries and the composed
  seed-42 fixture byte-identical while production places structures.
- `chunk.rs` attaches the answer at `OverworldChunkSource::column` (and at
  `set_block`, so a chunk the player edits keeps its `starts` when saved), because
  a `GeneratedColumn` does not carry its own chunk coordinates and the two
  generator calls need them. `chunk_nbt::structures_to_nbt` is the writer.
- **No longer true** as of S2: template pieces exist for shipwreck, ocean ruin and
  igloo, and they do write blocks — a *third* server-side edit is now needed for
  that to happen in the served world (`Resolver::structure_template`, see
  [`worldgen-structure-templates.md`](./worldgen-structure-templates.md)).
- **Also no longer true** as of S3: the beardifier is a real evaluator
  ([`worldgen-beardifier.md`](./worldgen-beardifier.md)). Still true in *effect*,
  for a different reason: every adaptation-bearing structure is jigsaw (S4) or
  coded (S5), so no real start carries a beard yet and no terrain is flattened.

The original text, for the record:

The generator computes starts and references, and exposes them as
`OverworldGenerator::{structure_starts, structure_starts_including_incomplete,
structure_references, structure_ledger}`. **Two edits in
`crates/lodestone-server/src/` are still needed** and were out of this unit's
file ownership:

1. `worldgen_data.rs` — `EmbeddedResolver` must override the four new `Resolver`
   methods. The data is already embedded (`build.rs` sweeps every `*.json` under
   `assets/worldgen/`, which includes `structure/`, `structure_set/` and
   `tags/worldgen/biome/`), so each override is a one-line `try_json`:

   ```rust
   fn structure_set_ids(&self) -> Vec<String> {
       EMBEDDED_WORLDGEN.iter()
           .filter_map(|(id, _)| id.strip_prefix("structure_set/"))
           .map(|name| format!("minecraft:{name}"))
           .collect()
   }
   fn structure_set(&self, id: &str) -> Value { self.try_json(&format!("structure_set/{}", strip(id))) }
   fn structure(&self, id: &str) -> Value { self.try_json(&format!("structure/{}", strip(id))) }
   fn biome_tag(&self, id: &str) -> Value { self.try_json(&format!("tags/worldgen/biome/{}", strip(id))) }
   ```

2. `chunk_nbt.rs:466` — the empty `structures{References:{}, starts:{}}` compound
   becomes real NBT: `starts` from `structure_starts` (`id`, `ChunkX`, `ChunkZ`,
   `references`, `Children` — each child `id`/`BB`/`O`/`GD` plus `Template` for
   template pieces), `References` from `structure_references` (structure id →
   `long[]` of packed chunk keys). `StructureRefs::packed_by_structure` produces
   the `long[]` shape already.

Until both land, the integrated server's `EmbeddedResolver` returns the default
empty `structure_set_ids`, the generator's `structures` field is `None`, and the
whole engine costs zero draws per chunk. That is also why all 13 parity binaries
and the composed seed-42 fixture are byte-identical: every fixture resolver in
the workspace supplies no structure data. (Both have landed; the parity-fixture
half of that sentence is still true, because those resolvers are unchanged.)

## Configuration

None. Everything is data:
`Resolver::{structure_set_ids, structure_set, structure, biome_tag}`. A resolver
supplying none of them gets an inert engine.

## Dependencies

`lodestone-worldgen-core`'s `rng` (the seed derivations) and `density::Resolver`;
`lodestone-worldgen`'s `aquifer` (the column sampler) and `biome` (the climate
sampler); the bundled corpus in `crates/lodestone-server/assets/worldgen/`;
`.cache/mc/survival/world` as the oracle and `.cache/mc/26.2/src/` as the record
definition. Companions: [`plans/structures.md`](./plans/structures.md),
[`worldgen-structure-corpus.md`](./worldgen-structure-corpus.md),
[`worldgen-staged-store.md`](./worldgen-staged-store.md).
