# Structure feature-pool features

## What it is

The placed-feature interpreter that runs a jigsaw structure's feature-pool elements: the trees, hay and melon piles, flowers, cacti and sculk patches that villages, abandoned camps and ancient cities place as pieces. It lives in `lodestone_worldgen::feature` and is that module's only consumer; ordinary terrain decoration uses `lodestone-worldgen-feature-26-3` instead ([26.3 features](worldgen-features-26-3.md)).

## How it works

1. **Parse.** `structure::pool` parses a `minecraft:feature_pool_element` into `PoolElement::Feature`, resolving its placed feature once through `feature::vegetation::resolve_placed_feature_ref` into a `PlacedRef` (a list of `VegPlacement` modifiers plus one `ConfiguredFeature`).
2. **Carry.** Jigsaw assembly keeps each element's origin on the piece as `PieceRefinement::FeaturePlacements`.
3. **Place.** In the structure's decoration step `Terrain263::place_structures` calls `structure::feature_placement::place_feature_pool_elements` with the structure's shared, already-seeded stream. It wraps the structure world in a `VegGrid` (`with_borrowed_structure_source`), runs every element in document order, then replays the grid's captured writes into the world.
4. **Walk.** `place_placed_feature` applies modifiers depth-first: modifier 0 emits its positions (drawing as it goes), then modifier 1 runs fully for each, down to the feature body. Draw order depends on this nesting, so never collect positions breadth-first.

`VegGrid` reads blocks and the five heightmap lanes from the borrowed world, frozen for the grid's lifetime (generation heightmaps see the snapshot; live reads also see the grid's earlier writes). Writes go to a sparse `feature::overlay::Overlay`.

Modelled, exactly what the bundled pools reach:

| kind | modelled |
|---|---|
| configured features | `simple_block`, `tree`, `block_column`, `block_pile`, `sculk_patch`, `no_op` |
| placement modifiers | `count`, `random_offset`, `block_predicate_filter`, `environment_scan` |
| trunk placers | straight, forking, dark oak, giant, mega jungle, fancy, cherry |
| foliage placers | blob, spruce, pine, acacia, dark oak, jungle, mega pine, fancy, cherry |
| tree decorators | beehive, alter ground, trunk vine |

**Known gaps**
- The server's resolver reads `lodestone-server/assets/worldgen`, whose `placed_feature/` predates 26.3. Four placed features the 26.3 abandoned-camp pools name (`bamboo_in_structure`, `orange_poplar`, `red_poplar`, `yellow_poplar`) resolve as missing and place nothing. The 26.3 copies in `lodestone-worldgen-data-26-3` use a flattened shape (no `config` object) and types not modelled here (`overlay`, `sequence`, poplar trunk and foliage placers).
- The cocoa, leave-vine and pale-moss decorators parse to `Decorator::Unsupported` and place nothing, skipping their draws.
- The beehive decorator writes the nest and draws its occupants' hive times, but structure placement has no block-entity channel, so the bees are dropped (the 26.3 terrain decorator's beehives are unaffected).

The durable fix is running pool features through the 26.3 feature crate's own placed-feature `place`, which needs a mutable 26.3 `Level` inside structure placement rather than today's read-only callback returning a write list.

## How to change it

- A newly reachable feature type needs a `ConfiguredFeature` variant, an arm in `parse_configured_feature_doc` (`feature/vegetation/config.rs`) and a dispatch arm in `place_configured_feature` (`feature/vegetation/mod.rs`).
- The `other =>` arm makes unknown types silent: an unknown feature type places nothing, an unknown placement modifier makes the whole placed feature unsupported (dropping one would move every position), and an unknown trunk or foliage placer, or any root placer, fails the tree.
- Keep every body's draw order: all elements of one structure share one stream. `VegTags::bind` builds per-state tag bitsets and `PoolFeaturePlacement::place` calls it idempotently.
- To find what the pools reach, walk `template_pool/**` for `feature_pool_element`, follow each placed and configured feature transitively and collect every `type`.

## Configuration

`LODESTONE_VEG_STRICT=1` (read once) panics, naming the reason, when an `Unsupported` feature is placed. Features, tags and block facts come from the resolver `lodestone_server::worldgen_data` builds over embedded assets.

## Dependencies

`lodestone-worldgen-core` (`RandomSource`, `IntProvider`), `lodestone-data` (`StateId`s, properties, occlusion, collision), `crate::structure` (`StructureWorld`, pool parser, placement pass), `crate::dense_grid::base_facts` (air and fluid facts for heightmaps).
