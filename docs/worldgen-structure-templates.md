# Structure templates and processors (worldgen phase S2)

## What it is

The engine that turns a structure *start* into actual blocks: a reader for the
1212 bundled `.nbt` structure templates, the processors vanilla runs between a
template's palette and the world, and the generation stage that writes the result
into a chunk. Phase **S2** of issue #514 — S0 staged the chunk statuses, S1
decided *where* structures go, and until this landed nothing wrote a block.

Three structure types place real blocks now: **shipwreck** (both `shipwreck` and
`shipwreck_beached`), **ocean ruin** (`ocean_ruin_cold`, `ocean_ruin_warm`, both
the single ruin and the large cluster) and **igloo** (including the ladder shaft
and basement). Every gap is named in
[`StructureRegistry::unsupported`](../crates/lodestone-worldgen/src/structure/mod.rs)
rather than left silent — see [Gaps](#gaps-all-on-the-ledger).

## How it works

```text
StructureRegistry::new           load the 71 templates the wired kinds can name
  ↓
starts_at(cx, cz)                S1's placement + biome filter
  ↓ passed
StructureKind::generate_pieces   pick template + rotation, resolve the piece's Y
  ↓                              -> StructurePiece { bounding_box, placement }
pre_ore_stage (per chunk)
  ↓
structure_place_stage            every referenced start whose piece box hits
  ↓                              this chunk
StructureTemplate::place         transform -> processors -> mirror/rotate -> grid
```

Files:

| file | holds |
|---|---|
| `structure/template.rs` | the NBT reader, `BlockState`, `Rotation`/`Mirror`, `transform`, `place` |
| `structure/processor.rs` | `BlockIgnore`, `BlockRot`, `Rule` |
| `structure/mod.rs` | the piece generators (`shipwreck_pieces`, `ocean_ruin_pieces`, `igloo_pieces`) and the `TemplateStore` |
| `overworld/structures.rs` | `structure_place_stage`, the write into the chunk grid |

### Where the write happens, and why there

`structure_place_stage` runs at the end of `pre_ore_stage_uncached`, right after
carving. That is vanilla's own order: `applyBiomeDecoration` places a step's
structures *before* that step's features, and all three wired kinds are
`surface_structures` (step 4) — ahead of `underground_ores` (6) and
`vegetal_decoration` (9). Ore and vegetation therefore see the structure's blocks,
and the whole thing is memoised once per chunk with the rest of the pre-ore
product.

Clipping needs no bounding box: the working grid spans this chunk's 16×16 columns
and `DenseBlockGrid::set` ignores a write outside it, so a piece straddling a
border writes its own half here and the rest when the neighbour generates. That is
`placeSettings.setBoundingBox(chunkBB)` for free.

### Everything random is position-seeded, and that is load-bearing

Vanilla resolves a template piece's final Y inside `postProcess`, mutating the
**shared** `StructureStart` object the first time any chunk places it; later chunks
see `heightAdjusted == true` and reuse the value. We cannot do that — our chunks
are generated independently and cached, so a Y that depended on which chunk got
there first would shear a shipwreck along a chunk border. So:

* **Piece positions are resolved eagerly**, in `generate_pieces`, from the same
  `_WG` noise columns vanilla's own "too big to fit in the worldgen region" branch
  uses (`Structure.getLowestY`, `getMeanFirstOccupiedHeight`).
* **Palette choice** is `RandomSource.create(Mth.getSeed(templatePosition))` and
  **`BlockRotProcessor`'s** keep/drop roll is `Mth.getSeed(blockPos)` — a fresh
  legacy LCG per position, exactly as vanilla, so two chunks placing two halves of
  one ruin agree without communicating.

`StructureKind::generate_pieces` runs **after** the biome filter, mirroring
vanilla's lazy `GenerationStub`: a biome-rejected candidate consumes no RNG and
samples no columns, so its neighbours' structures do not move.

### Multi-palette templates

Every shipwreck template carries **8 palettes** (the wood species) under a
`palettes` list, with one shared block list whose `state` indexes all of them. A
reader that only understood the single `palette` key would place *nothing* for
every shipwreck and look correct on igloos. `StructureTemplate::palette_for`
picks the palette.

## How to change it

* **Adding a template-driven structure**: add a `StructureKind` variant, list its
  templates in `template_ids` (they are loaded eagerly and a missing one demotes
  the kind to unsupported), and write its `*_pieces` function beside the other
  three. Transcribe the vanilla `*Structure.generatePieces` **and** the piece's
  `postProcess` height fix-up together — the second half is where the real
  positioning lives, and reading only the first gives you a structure at Y=90.
* **Rotation is per-property**, not per-block: `facing`, `axis`, `rotation`, and
  the four directional booleans. That covers every property in the 71 templates
  placed today. A property needing block-class knowledge (a stair's `shape` under a
  *mirror*, a rail's `shape`) is not handled; all three kinds use `Mirror.NONE`,
  where `shape` is invariant.
* **Processors return `Option`**: `None` means *drop this block*, which is not an
  error path — it is how air and rot work. Order matters: ocean ruins add
  `BlockRot` before `STRUCTURE_AND_AIR`, so a rotted-away block never reaches the
  ignore list.
* **The 40 bundled `worldgen/processor_list/*.json` documents are referenced by
  template *pools*, not by any structure placed here**, so nothing parses that
  registry yet. When S4's jigsaw needs it, add a `parse` beside the `Processor`
  variants and ledger every `processor_type` it does not cover.

### Gotchas, all paid for

* **`StartContext::first_occupied_height` is not `level.getHeight`.** The former is
  vanilla's `getFirstOccupiedHeight` (`getBaseHeight - 1`, the topmost occupied
  block); the latter is the first *free* Y. Every `postProcess` transcription calls
  `getHeight`, so the two differ by one and confusing them sinks every template one
  block into the ground. `free_height` in `structure/mod.rs` is the bridge.
* **An ocean ruin hangs off its origin chunk.** Its place settings carry no pivot,
  so a `CLOCKWISE_180` ruin extends *negatively* from the chunk's min corner — the
  start is in chunk `(5, 3)` while nearly all its blocks land in `(4, 2)`. A gate
  that swept only the start's own chunk measured one column of gravel and read as
  "placement is broken". Sweep the piece's bounding box.
* **A shipwreck's height scan uses the *unrotated* footprint** from the chunk's min
  corner, not the rotated bounding box. That is vanilla's own quirk; it is
  transcribed as-is.
* **`BlockIgnoreProcessor.STRUCTURE_AND_AIR` is why a shipwreck is full of sand.**
  Template air is dropped rather than placed, so the hull keeps whatever terrain
  was there. Dropping the processor would carve an air box out of the sea floor.
* **Igloo pieces probe one shared entrance column** by construction: each part's
  own offset cancels against its own probe, so all three parts land at the same
  surface height. Changing one offset without the other desynchronises the stack.
* **A structure with `terrain_adaptation: none` needs no beardifier**, which is why
  these three could land before S3. S3 has since landed
  ([`worldgen-beardifier.md`](./worldgen-beardifier.md)) and the `debug_assert!`
  that used to guard the gap in `pre_ore_stage` is gone, replaced by the real data
  dependency; every kind wired here is still `none`, so none of them is affected
  either way.

### Gaps, all on the ledger

`StructureRegistry::unsupported()` (surfaced as
`OverworldGenerator::structure_ledger()`) names these:

| key | why |
|---|---|
| `block_entity:append_loot` | a `capped` archaeology rule *does* place its `suspicious_sand`/`suspicious_gravel` block (ocean ruins, trail ruins, desert pyramid) and its `append_loot` table **is** bundled, but **nothing in the game brushes**: no `brushable_block` block entity and no brush interaction. The blocker is gameplay-side, not in worldgen. Corrected in S6 — the earlier wording blamed worldgen |
| `template:block_entity_nbt` | **132 bundled templates** carry a chest/barrel/dispenser/decorated pot whose `LootTable` lives in that *block's own* `nbt` compound (village 62, bastion 26, trial_chambers 19, ruined_portal 13, ancient_city 10, pillager_outpost 2). A **different mechanism** from a `structure_block` DATA marker, and the one `lodestone_server::structure_loot` does not read: the blocks are placed and the containers are empty. Replaced `template:data_markers`, which named the three structures whose markers **do** get rolled |
| `template:mirrored_shape` | a **rail** `shape` is not remapped under a mirror. A stair's is, as of S5: a coded piece with a SOUTH or WEST orientation carries a real `LEFT_RIGHT` mirror, so this stopped being inert — see `docs/worldgen-structure-coded.md` |
| `minecraft:ruined_portal` | still `Unsupported`, and the blocker list is now measured rather than estimated (`RuinedPortalStructure` 269 lines + `RuinedPortalPiece` 346). It needs: a **`Setup` list** parsed off the structure JSON with a weighted pick; the **template at stub time** (its size decides the bounding box, and the rotation/mirror/template draws all happen *before* the biome check, so it needs a `Stub` variant that owns a half-used random, exactly as jigsaw does); a **`findSuitableY`** that walks four corner columns for "3 of 4 corners opaque"; four processors (**`block_age`**, `protected_block`, `lava_submerged`, `blackstone_replace`) plus `rule` with `random_block_match`; and **`spreadNetherrack`**, a 29×29 apron off one shared stream in scan order that writes across chunk borders from the centre chunk only. `is_replaceable_at` is the existing world read; the apron's obsidian/lava test additionally needs a `BlockKind`-at-position |
| `minecraft:monument` | pieces are **coded**, not templated (~2,000 lines of `OceanMonumentPieces`). Placement and the 29-block biome survey are complete; only the pieces are missing, and this row did not exist until S5 went looking for it |
| `coded:buried_treasure_chest` | `buried_treasure` produces a start and a bounding box and places **zero blocks**. `postProcess` walks a cursor down from the ocean floor until the block *below* is sandstone/stone/andesite/granite/diorite, then writes five neighbours and one chest. **Not just a missing `BlockKind` read**: the sand it burrows through is a *surface-rule* product and the granite/diorite/andesite are *ore-blob* products, so the answer exists only at a stage the eager start pass sits above. Pre-surface, every solid block is `BlockKind::Stone`, so the walk would stop immediately and put the chest on top of the beach |
| `dimension:nether_structures` | **`bastion_remnant`, `fortress`, `nether_fossil` and `ruined_portal_nether` reach zero blocks in a served world whatever their piece generators do.** Their biome tags are Nether-only and `NetherGenerator` has **no structure stage** — no starts, no refs, no place, no beardifier. So `bastion_remnant` assembles, is on no other row, and is invisible in the Overworld because its biome filter can never pass. The fix is in `nether/mod.rs` |
| `coded:chests` | a coded piece's containers (`desert_pyramid` ×4, `jungle_temple` 2 chests + 2 dispensers) place their **block** and carry their table and roll seed on `StructurePiece::loot`, but nothing reads that list: `structure_loot` resolves loot from a template's raw bytes and a coded piece has no template |
| `coded:chest_reorient` | `StructurePiece.reorient` picks a chest's `facing` from its four horizontal neighbours' render-solidity *as written so far*; no block-state read exists on `StartContext`, so a coded chest keeps `facing=north`. Cosmetic |
| `coded:decoration_random` | `postProcess`'s `random` is the **decorating chunk's** stream, so vanilla's own answer is chunk-order dependent — `jungle_temple`'s 1,522 selector draws and every container roll seed. Taken from the structure's own per-chunk stream here, in vanilla's order and count |

Entities in templates are parsed and not placed. Igloo's "cap the shaft with snow
when there is no ladder below" fix-up is not implemented.

**Two of those rows were wrong for five phases, and the correction is the point.**
Both `block_entity:append_loot` and the old `template:data_markers` said the gap
"needs block entities and loot tables in worldgen". Neither is true: `lodestone-worldgen`
has had a block-entity layer since #520 (`overworld::block_entities`), and
`lodestone-server` has had a loot roller plus `structure_loot` since #337 — which
re-reads a template piece's raw bytes, finds its `structure_block` DATA markers,
rolls the table the marker names and attaches a **filled** container. A shipwreck
generated today arrives with 4–11 rolled stacks in its chests. So the DATA-marker
path is *closed*, and the row describing it as open was hiding the two gaps that
really are open: the 132 templates whose loot lives in a block's own `nbt`, and the
absence of any brush interaction. A ledger row that names the wrong gap is worse
than no row — it makes the right one invisible to exactly the reader who came
looking.

## Configuration

None. Everything is data:

* `Resolver::structure_template(id) -> Option<Vec<u8>>` serves one template's raw
  file bytes (gzip wrapper included — the reader accepts gzipped or bare NBT).
  `minecraft:shipwreck/with_mast` is `assets/structure/shipwreck/with_mast.nbt`.
  Default `None`, so a resolver that supplies nothing gets structures demoted to
  unsupported and named, never silently absent.

  **That default was itself the island**, and it is worth recording because the
  demotion is exactly what made it survivable rather than invisible: this engine
  placed real blocks for weeks of tree-time while `lodestone-server`'s
  `EmbeddedResolver` still took the `None`, so every template-driven structure was
  demoted to `Unsupported` and reached **zero blocks in the served world**. The
  ledger named it the whole time. It is served now, from
  `EMBEDDED_STRUCTURE_TEMPLATES` — all 1,212 templates as `include_bytes!`, ~3.4 MiB
  of rodata, generated by `crates/lodestone-server/build.rs` alongside
  `EMBEDDED_WORLDGEN` and `EMBEDDED_LOOT`.

  Two things about that table. The lookup is a `binary_search_by`, so **`build.rs`
  must sort it by key** — an unsorted table does not fail loudly, it silently
  misses. And `build.rs`'s recursive collector is now `collect_ext(root, dir, ext,
  out)`; the old `collect` hardcoded `"json"` and remains as a delegating wrapper.
* The templates themselves are the bundled corpus — see
  [`worldgen-structure-corpus.md`](./worldgen-structure-corpus.md). They are jar
  bytes; never hand-edit one.

## Dependencies

* `lodestone-core` — the NBT codec (`Nbt`, `Reader`, `read_named_nbt`).
* `flate2` — Mojang gzips every `.nbt` template.
* `lodestone-worldgen-core` — `rng::get_seed` and `LegacyRandomSource` for the
  position-seeded draws, and the `Resolver` trait the template bytes arrive
  through.
* `crate::dense_grid::DenseBlockGrid` — the write target, and the thing that
  clips a piece to a chunk.

---

## Jigsaw assembly (phase S4)

S4 is built on top of everything above; it adds no new *placement* concept, only a
way of choosing which templates go where. Three modules:

| file | role |
|---|---|
| `structure/pool.rs` | `worldgen/template_pool/*.json` (188) and `worldgen/processor_list/*.json` (40) → `TemplatePool`s of `PoolElement`s |
| `structure/jigsaw.rs` | `JigsawPlacement.addPieces` + its `Placer`: the BFS, the free-space accumulator, `canAttach` |
| `structure/mod.rs` | `StructureKind::Jigsaw`, the `Stub` that carries a half-consumed RNG across the biome filter |

**What places blocks:** `village_plains`, `village_desert`, `village_savanna`,
`village_snowy`, `village_taiga`, `pillager_outpost`, `ancient_city`.

### The prerequisite: block NBT had to be retained

`StructureTemplate::parse` used to drop each block's `nbt` compound — correct for
an ordinary block, and what vanilla's own placement loop does. But a **jigsaw
block's entire configuration lives in that compound**: `name`, `target`, `pool`,
`final_state`, `joint`, `placement_priority`, `selection_priority`. So S4's real
first commit was `TemplateBlock::nbt: Option<Arc<BlockNbt>>` plus
`StructureTemplate::filter_blocks`, which is vanilla's
`filterBlocks(position, settings, block, absolute = true)`.

`groundLevelDelta` was expected to be in there too and **is not**: in 26.2
`StructurePoolElement.getGroundLevelDelta()` is a constant `1` and nothing
overrides it. Older versions read a `bottom` data marker; the handoff note into S4
repeated that, and it is wrong for this version.

### RNG draw order is the whole specification

Assembly is a long stream out of one `WorldgenRandom` seeded
`setLargeFeatureSeed(seed, cx, cz)`. The nesting is where mistakes hide:

```text
start_height.sample(random)                      0 draws for `absolute`, 1 for `uniform`
Rotation.getRandom(random)                       1
startPool.getRandomTemplate(random)              1
                        <- biome filter runs here, on the centre piece's centre ->
per source jigsaw (shuffled: n-1 draws):
    targetPool.getShuffledTemplates(random)      size-1 draws, over the WEIGHT-EXPANDED list
    fallback.getShuffledTemplates(random)        size-1
    per candidate element:
        Rotation.getShuffled(random)             3
        per rotation:
            element.getShuffledJigsawBlocks(...) m-1
```

Two traps worth naming:

* **The element list is weight-expanded before the shuffle.**
  `village/plains/town_centers` has weights `50,50,50,50,1,1,1,1`, so its expanded
  list is **204** entries and one shuffle draws **203** times. Shuffling the 8 raw
  entries draws 7 and desynchronises everything after it.
* **`Util.shuffle` walks downward** (`for (int i = size; i > 1; i--)`). An upward
  Fisher–Yates consumes the same *number* of draws and produces a different
  permutation, which is the most plausible-looking way to be wrong here.

The biome filter sits *between* the centre draws and the BFS, which is why
`StructureKind::find_stub` returns a `Stub` that owns the half-consumed random
rather than a bare position: re-seeding across the filter would restart the stream.
Every other structure kind draws nothing in `findGenerationPoint`, so for them a
fresh stream in `generate_pieces` is exactly vanilla — that is why the `Stub` enum
has a `Plain` variant carrying only a position.

### Free space is exact, not approximate

Vanilla keeps free space in a `VoxelShape` and asks
`joinIsNotEmpty(free, AABB.of(targetBox).deflate(0.25), ONLY_SECOND)`. Every
operation it performs on that shape is "subtract an axis-aligned box", so the shape
is always `positive \ (b₁ ∪ b₂ ∪ …)`, and

```text
target ⊆ free   ⟺   target ⊆ positive  ∧  ∀i. target ∩ bᵢ = ∅
```

which is what `jigsaw::FreeSpace` evaluates, in `f64`, with the `deflate(0.25)`
kept. This is the *same set*, not a simplification that happens to work on
villages. The deflation is what makes two boxes sharing a face non-colliding; over
integer boxes it changes no answer, but keeping it keeps the expression vanilla's.

The accumulator is an **arena of shapes indexed by a `PieceState`**, because
vanilla's `MutableObject<VoxelShape>` is *shared and mutated* between sibling
states — cloning one per state would let two siblings occupy the same space. A
child whose target jigsaw lands *inside* the source piece's own box gets a
separate, lazily created shape (`sourceFree`), one per `tryPlacingChildren` call.

### Where assembly happens, and the two deviations that follow

Vanilla assembles lazily in `getPiecesBuilder()` and fixes piece Y in
`postProcess`, mutating a shared `StructureStart`. Assembly here is **eager, at
start time**, for the reason S2 recorded above: per-chunk memoised generation would
otherwise shear a village along a chunk border. Jigsaw assembly is a
whole-structure computation anyway, so this costs almost nothing — but two
consequences are real and both are on the ledger:

* **`GravityProcessor` reads a pre-beard column.** A `terrain_matching` element
  (village streets, farms) carries
  `GravityProcessor(WORLD_SURFACE_WG, -1)` from its projection, which vanilla
  evaluates against the decorating chunk's own heightmap — i.e. *after* the
  beardifier flattened it. Here the heights are sampled once per piece over its
  footprint at start time (`processor::ColumnHeights`) from a fresh `_WG` noise
  column. Chunk-independent by construction, which is the point; slightly
  different from vanilla on a slope.
* **Step order.** Every structure this engine places is written at the end of
  `pre_ore`, vanilla's `surface_structures` slot. Villages and outposts are
  `surface_structures`, so they are exact. `ancient_city` is
  `underground_decoration` (step 7, *after* ores), so ores can overwrite it here.

### Processors S4 added

`JigsawReplacementProcessor` (a jigsaw block becomes its own `final_state`, or is
dropped when that is `structure_void`), `GravityProcessor`,
`ProtectedBlockProcessor`, `BlockRotProcessor`'s `rottable_blocks` narrowing, and
`RuleProcessor`'s `location_predicate` — which reads the **world** state at the
target. That last one is why `StructureTemplate::place` is now two passes: vanilla's
`processBlockInfos` runs the whole chain over the whole block list before
`placeInWorld` writes anything, so a village street's bridge rule (`dirt_path` over
`water` → `oak_planks`) sees the pre-structure world and never an earlier block of
its own template.

`ProcessorList` parsing **refuses rather than defaults**. A rule with a real
`position_predicate` is not treated as `always_true`, because
`PosAlwaysTrueTest` draws nothing and `AxisAlignedLinearPosTest` draws one float:
defaulting would shift every later rule's roll and produce a structure that is
quietly wrong instead of loudly absent. The refusal demotes the structure and the
ledger names the `predicate_type`.

### Vanilla's own dangling template reference

`AncientCityStructurePools.java:113` names
`ancient_city/walls/intact_horizontal_wall_stairs_5`, and only `_1`..`_4` ship in
`.cache/mc/26.2/src/data/minecraft/structure/`. Vanilla tolerates this:
`StructureTemplateManager.getOrCreate` logs, caches an **empty** template (zero
size, no palette, no blocks), and the element stays in its pool — offered by the
shuffle, consuming its draws, never attaching. `StructureTemplate::empty()` does
the same, and the id is reported under a `dangling:` ledger key.

The distinction that keeps this from swallowing a real bundling gap: a resolver
serving **no** templates at all still hard-fails and demotes the structure, which
is the S2 island `lodestone-server`'s own
`no_structure_is_demoted_for_unloadable_templates` gate detects. Only a *single*
missing template in an otherwise complete bundle is treated as vanilla's dangling
reference.

### What S4 does not do, all on the ledger

**No jigsaw *structure* is refused any more.** The three that were — `trail_ruins` on `capped`,
`trial_chambers` on `pool_aliases`, `bastion_remnant` on `high_rampart`'s `axis_aligned_linear_pos` —
closed in S5's Part A, and `tests/structure_jigsaw.rs` now asserts all three are **supported** so a
regression cannot look like missing data. What is left are engine gaps rather than structures:

| ledger key | gap |
|---|---|
| `pool:feature_pool_element` | participates in the joint graph and the free-space accumulator (so the village around it is vanilla's) but places no blocks |
| `nbt:jigsaw_pool_element` | a persisted jigsaw child carries `Template`, not vanilla's `pool_element` compound |
| `jigsaw:step_order` | see above |
| `jigsaw:gravity_reads_a_pre_beard_column` | see above |
| `dangling:<template>` | see above |

### Evidence

`crates/lodestone-worldgen/tests/structure_jigsaw.rs`, seven arms. The *where*
comes from outside the repo: the two `village_plains` chunks are read out of the
vanilla-authored survival oracle world's `structures.starts` NBT
(`tests/support/structure_starts_survival.txt`, seed −195764831).

| arm | asserts |
|---|---|
| `the_jigsaw_structures_s4_models_are_not_on_the_ledger` | six structures **absent** from the ledger and four gaps **present** with the right reason; `town_centers` expands to `4×50 + 4` |
| `a_village_start_assembles_many_pieces_with_junctions` | > 5 pieces, junctions ≥ pieces, box wider than one chunk, both projections present |
| `a_village_chunk_gains_village_blocks_a_structureless_chunk_does_not` | > 200 village blocks over the covered chunks, **0** in the structure-free control, and **0** surviving jigsaw blocks |
| `the_beardifier_is_non_empty_inside_a_village` | S3 fires: a non-empty beard with a rigid box inside the village, empty everywhere in the control |
| `assembly_is_reproducible_across_generators` | two independently built generators produce identical piece lists — a real question, since `PoolStore` is a `HashMap` |
| `a_pillager_outpost_assembles_and_carries_a_list_element` | the only `list_pool_element` path, i.e. `extra_placements` |
| `an_ancient_city_assembles_underground` | the `start_jigsaw_name` anchor and the *absent* `project_start_to_heightmap`: the box must sit below y = 0 |
