# Structure feature-pool features

## What it is

The placed-feature interpreter that runs a jigsaw structure's feature-pool elements: the trees, hay
and melon piles, flowers, cacti and sculk patches that villages, abandoned camps and ancient cities
place as pieces. It lives in `lodestone_worldgen::feature` and is the only consumer of that module.
Ordinary terrain decoration does not use it; the 26.3 generator decorates with
`lodestone-worldgen-feature-26-3` ([26.3 features](worldgen-features-26-3.md)).

## How it works

1. **Parse.** `structure::pool` parses a `minecraft:feature_pool_element` into
   `PoolElement::Feature`, resolving its placed feature once through
   `feature::vegetation::resolve_placed_feature_ref` into a `PlacedRef`: a list of `VegPlacement`
   modifiers and one `ConfiguredFeature`.
2. **Carry.** Jigsaw assembly keeps each element's origin on the piece as
   `PieceRefinement::FeaturePlacements`.
3. **Place.** During the structure's decoration step, `Terrain263::place_structures` calls
   `structure::feature_placement::place_feature_pool_elements` with the structure's shared,
   already-seeded placement stream. That wraps the structure world in a `VegGrid`
   (`VegGrid::with_borrowed_structure_source`), runs every element in document order, then
   replays the grid's captured writes into the structure world.
4. **Walk.** `place_placed_feature` applies the modifiers depth-first. Modifier 0 emits its
   positions, drawing as it goes, then modifier 1 runs fully for each of them, and so on down to the
   feature body. The draw order depends on that nesting, so do not collect positions breadth-first.

What is modelled is exactly what the bundled feature pools reach:

| kind | modelled |
|---|---|
| configured features | `simple_block`, `tree`, `block_column`, `block_pile`, `sculk_patch`, `no_op` |
| placement modifiers | `count`, `random_offset`, `block_predicate_filter`, `environment_scan` |
| trunk placers | straight, forking, dark oak, giant, mega jungle, fancy, cherry |
| foliage placers | blob, spruce, pine, acacia, dark oak, jungle, mega pine, fancy, cherry |
| tree decorators | beehive, alter ground, trunk vine |

`VegGrid` reads blocks and the five heightmap lanes from the borrowed structure world. The world is
frozen for the grid's lifetime: world-generation heightmaps see the snapshot, while live reads also
see the grid's own earlier writes. Writes land in a sparse `feature::overlay::Overlay`.

### Known gaps

- **The embedded data lags the pools.** The server's resolver reads `lodestone-server/assets/worldgen`,
  whose `placed_feature/` set predates 26.3. Four placed features that the 26.3 abandoned-camp pools
  name (`bamboo_in_structure`, `orange_poplar`, `red_poplar` and `yellow_poplar`) resolve as missing
  and place nothing. The 26.3 copies in `lodestone-worldgen-data-26-3` use a flattened document
  shape (no `config` object), plus types this parser does not model: `overlay`, `sequence`, and the
  poplar trunk and foliage placers.
- **Some decorators are unmodelled.** The cocoa, leave-vine and pale-moss tree decorators parse to
  `Decorator::Unsupported`. They place nothing and skip their draws.
- **Beehives arrive empty.** The beehive decorator writes the nest block and draws its two or three
  occupants' hive times, but structure placement has no block-entity channel, so the bees are
  dropped. The 26.3 terrain decorator's own beehives are unaffected.

The durable fix for all three is to run pool features through the 26.3 feature crate's own
placed-feature `place`. That needs a mutable 26.3 `Level` inside structure placement, rather than
today's read-only callback that returns a write list.

## How to change it

- **A newly reachable feature type** needs three edits: a `ConfiguredFeature` variant, an arm in
  `parse_configured_feature_doc` (`feature/vegetation/config.rs`), and a dispatch arm in
  `place_configured_feature` (`feature/vegetation/mod.rs`).
- **The `other =>` arm makes unknown types silent.** A type with no arm parses to
  `ConfiguredFeature::Unsupported` and places nothing. An unknown placement modifier makes the whole
  placed feature unsupported, because dropping one modifier would move every position. An unknown
  trunk or foliage placer, or any root placer, fails the whole tree.
- **Keep every body's draw order.** All feature elements of one structure share one stream, so an
  extra or missing draw moves every later element.
- **Bind tags once.** `VegTags::bind` builds the per-state tag bitsets. `PoolFeaturePlacement::place`
  calls it idempotently, so callers need not.
- To check what the pools reach, walk `template_pool/**` for `feature_pool_element` entries, then
  follow each placed and configured feature transitively and collect every `type` string.

## Configuration

- `LODESTONE_VEG_STRICT=1` turns placing an `Unsupported` feature into a panic that names the reason.
  It is read once per process.
- The bundled placed and configured features, block tags, and block freeze and survival facts come
  from the resolver that `lodestone_server::worldgen_data` builds over the embedded assets.

## Dependencies

- `lodestone-worldgen-core`: `RandomSource` and `IntProvider` sampling.
- `lodestone-data`: canonical `StateId`s, block properties, face occlusion and collision shapes.
- `crate::structure`: `StructureWorld`, the jigsaw pool parser and the placement pass.
- `crate::dense_grid::base_facts`: air and fluid facts for heightmap scans.
