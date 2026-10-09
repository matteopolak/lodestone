# Loot-table functions and enchantment selection

## What it is

The server-side loot-table parser and roller turn embedded datapack JSON into item stacks for block drops, mobs and generated containers. This doc covers the eager bundle audit, `minecraft:enchant_with_levels` (item-aware enchantments for End-city treasure), item-shaping functions and context-dependent conditions.

## How it works

- `LootTableSet::load_bundled` parses every embedded table before worldgen rolls any. In debug builds it checks each table's `LootTable::unsupported_features` against the decoration-only allowlist, so an unrelated table with a parsed-as-unsupported function can abort startup.
- `minecraft:enchant_with_levels` evaluates `levels`, then the shared selector: item enchantability adjustment and random spread, the highest level whose cost window contains the adjusted cost, a weighted pick, then compatible candidates while the extra-roll gate passes. Omitted `options` allows every known enchantment; the End-city table uses `#minecraft:on_random_loot` (`crate::enchantment_data::on_random_loot`); explicit ids work too. Unknown tags and ids stay in `unsupported_features` and yield no candidate rather than the full registry. A plain book becomes an enchanted book even with an empty candidate set. The item model stores results in its regular enchantment list, so `include_additional_cost_component` is audited as partially unsupported.
- Draw order and arithmetic follow the reference and the external End-city capture `crates/lodestone-server/tests/support/end_city_loot_jvm.txt`. `SpawnRng` is deterministic but not byte-compatible with the JVM stream, so tests assert the selection contract and consumer wiring, not per-seed wire identity.
- **Item-shaping functions.** `set_damage`, `set_potion` and `set_instrument` set components carried by the wire encoder and save codec ([item-save-format](item-save-format.md)). `set_damage`'s value is the fraction of durability left: stored damage is `floor((1 - clamp(value [+ current fraction left when add], 0, 1)) * max)`, zero stores no component, and an item without durability is untouched and draws nothing. `set_potion` keeps custom effects and recomputes the colour. `set_instrument` resolves options at parse time from `assets/tags/instrument/` (nested tags expand in listed order) with one `next_int(len)`; an unknown tag is unsupported.
- **Context conditions.** `LootContext` carries `biome` and `fishing_hook_in_open_water` beside luck, tool, block state and explosion radius. A `location_check` that is only a list of biome ids reads `biome`; an `entity_properties` check of the bobber's open-water state reads `fishing_hook_in_open_water`. Any other shape stays context-blind (constant false, listed by `context_blind_features`). Structure containers roll with the biome at the container (`structure_loot::context_at`); fishing rolls with luck, biome and open water.
- **Document shape.** The current datapack shape (one `condition` and one `modifier` per position, `type` instead of `condition`/`function`, stored predicates by id, `match_block`) is rewritten to the shape the parser reads by `loot::format::modernise_table`; older-shaped documents pass through. Stored predicates come from `assets/predicate/`. `function minecraft:filtered` is allowed in decoration only for the discard check after an exploration map, and a targetless map is kept.

## How to change it

- Refresh bundled recipes, item tags and stored predicates with `python3 scripts/regen-server-data.py` (they follow `mc-version`) and loot tables with `just regen-loot-corpus`, then update the pinned counts the failing tests name (`BUNDLED_CRAFTING_RECIPES`, loot bundle size). A new table must come from the current corpus and pass the clean-subset check; never add an asset by hand.
- A new function goes in `crates/lodestone-server/src/loot.rs`: variant, field parsing, empty-context semantics and a control fixture proving its RNG draws. Reuse `select_enchantments_filtered` for custom holder sets instead of duplicating the cost, compatibility and weighted-pick loop.
- If the item model gains stored enchantments or trade-cost components, update the `EnchantWithLevels` apply arm and remove the audit entry only after a wire-level test proves the component survives decoding.

## Configuration

Tables come from `crates/lodestone-server/assets/loot_table/`. `LODESTONE_REGEN=1` enables the ignored regeneration tests when a fresh external capture exists. No runtime flag changes selection rules.

## Dependencies

`serde_json`, `lodestone_model::ItemStack`, `crate::enchantment_data`, `crate::enchanting` (RNG and weights), `crate::anvil::apply_enchantment`, and `crate::structure_loot` (the production consumer behind the End-city fixture).
