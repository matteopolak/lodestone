# Structures delivery plan

## What it is

The remaining gate and evidence work for the structure engine (placement, templates, beardifier, jigsaw, coded piece generators), which is implemented. How the engine works is in [Structure generation](../worldgen-structures.md); this doc keeps the oracle evidence ledger and the open obligations.

## How it works

The evidence base for every structure gate is the vanilla-authored survival world at `.cache/mc/survival/world`: seed -195764831 (read from `world/data/minecraft/world_gen_settings.dat`, not `level.dat`), 14,499 overworld chunks in 29 region files, with full `structures.starts` and `References` NBT per chunk. Gates compare our computed starts and pieces against that NBT, so expected values never round-trip through our encoder.

Start census of the oracle area (102 total):

| type | starts |
|---|---|
| mineshaft | 46 |
| ocean_ruin | 16 |
| trial_chambers | 13 |
| shipwreck | 11 |
| ruined_portal | 9 |
| ocean_monument | 2 |
| village | 2 |
| buried_treasure | 2 |
| trail_ruins | 1 |

Absent from the area: stronghold, desert_pyramid, igloo, swamp_hut, jungle_temple, pillager_outpost, ancient_city, mansion. A gated type must have a nonzero oracle count, asserted by the gate itself.

## Open obligations

- **Zero-oracle structures** (the absent list plus fortress) cannot claim piece-list equality. Their gates are record-derived arithmetic and self-consistency until the oracle world is extended under Apple `container` (`scripts/worldgen-oracle/`).
- **Stronghold ring math** is gated only by a hand-computed first-ring fixture; do not report it verified until an oracle start exists.
- **Village coverage** is two plains villages. The other village biomes, pillager_outpost and ancient_city are unexercised; gate output should print per-type instance counts so a small-sample pass does not generalise.
- **Jigsaw expansion RNG** is one long seeded walk; a single mis-ordered draw scrambles every later piece, and the piece-list gate localises but cannot explain it. Extend the oracle world with more villages early.
- **Incomplete generators** stay in `StructureRegistry::unsupported` and are visible through `OverworldGenerator::structure_starts_including_incomplete`, never persisted or block-placed. `feature_pool_element` referencing an unimplemented feature places nothing and must be named in that ledger.
- Out of scope: in-structure mob spawn overrides (parsed, not consumed), chest loot and spawner contents, `/locate`, Nether/End dimension hosting.

## How to change it

- A new gate needs a control observed failing (e.g. perturb a salt by +1, rotate one shipwreck piece) and a floor on compared starts so an empty ledger cannot pass vacuously.
- Placement draw order per structure set is the specification; plausible-looking placement that draws differently is a different world.
- Structure-free chunks must cost nothing: `beardifier_evals == 0` and `template_blocks_placed == 0` exactly.
- The 12 worldgen parity binaries (`crates/lodestone-worldgen/tests/*_parity.rs` plus `overworld_gen.rs`) must stay byte-identical.

## Dependencies

`crates/lodestone-worldgen` and `-core`, the bundled corpus under `crates/lodestone-server/assets/` (`worldgen/` JSON, `structure/` templates), `.cache/mc/survival/world`, `scripts/worldgen-oracle/`, and [worldgen-rewrite](./worldgen-rewrite.md).
