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
| `processor:minecraft:capped` | ocean ruins' 5 suspicious sand/gravel blocks: needs the archaeology loot pass and a shuffled-index walk over the whole processed list |
| `template:data_markers` | `structure_block` `DATA` markers are dropped, so template loot chests (shipwreck supply/treasure/map, ocean ruin chest) are not placed: needs block entities and loot tables in worldgen |
| `template:mirrored_shape` | a stair/rail `shape` is not remapped under a mirror (inert today: every placed structure uses `Mirror.NONE`) |
| `minecraft:ruined_portal` | still `Unsupported`: its own vertical placement, air pocket, and blackstone/lava/`block_age` processors are a unit of their own |
| `minecraft:monument` | pieces are **coded**, not templated (~1,400 lines of `OceanMonumentPieces`) — S5, not S2 |

Entities in templates are parsed and not placed, for the same reason as loot
chests. Igloo's "cap the shaft with snow when there is no ladder below" fix-up is
not implemented.

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
