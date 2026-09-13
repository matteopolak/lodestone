# Loot-table functions and enchantment selection

## What it is

The server-side loot-table parser and roller turn embedded datapack JSON into
item stacks for block drops, mobs, and generated containers. This document
describes the eager bundle audit and the `minecraft:enchant_with_levels`
function that now supplies item-aware enchantments for End-city treasure.

## How it works

`LootTableSet::load_bundled` parses every embedded table before a worldgen
request rolls any one of them. In debug builds it checks each table's
`LootTable::unsupported_features` against the decoration-only allowlist. That
check is intentionally strict, but it means a table unrelated to the current
chunk can still abort startup when its function is only parsed as unsupported.

`minecraft:enchant_with_levels` evaluates its `levels` number provider, then
calls the shared enchantment selector. The selector applies the item
enchantability adjustment and random spread, finds the highest level whose cost
window contains the adjusted cost, chooses by registry weight, and continues
with compatible candidates while the additional-roll gate passes. An omitted
`options` field allows every known enchantment; the bundled End-city table uses
`#minecraft:on_random_loot`, whose membership is kept in
`crate::enchantment_data::on_random_loot`. Explicit enchantment ids are also
accepted. Unknown tags and ids remain visible in `unsupported_features` and
produce no candidate rather than being silently treated as the full registry.

The function converts a plain book to an enchanted book even when its filtered
candidate set is empty, matching the function's item conversion order. The
version-free item model stores the resulting enchantments in its regular
enchantment list; an `include_additional_cost_component` request is therefore
audited as partially unsupported while the core enchantment still runs.

The selection's draw order and arithmetic follow the committed decompiled
reference and the external End-city marker/loot capture at
`crates/lodestone-server/tests/support/end_city_loot_jvm.txt`. `SpawnRng` is
deterministic but is not a byte-compatible JVM stream, so tests assert the
selection contract and production consumer wiring rather than claiming
per-seed wire identity.

## How to change it

Add a new loot function in `crates/lodestone-server/src/loot.rs`: define its
variant, parse its fields, apply its empty-context semantics, and add a control
fixture that proves its RNG draws. Reuse `select_enchantments_filtered` when a
function supplies a custom enchantment holder set; do not duplicate the cost,
compatibility, or weighted-pick loop.

If the item model gains stored enchantments or trade-cost components, update
the `EnchantWithLevels` apply arm and remove the corresponding audit entry only
after a wire-level test proves the component survives decoding. A new bundled
table must come from the pinned corpus and pass the clean-subset check; use
`just regen-loot-corpus` rather than adding an asset manually.

## Configuration

The embedded table set comes from `crates/lodestone-server/assets/loot_table/`.
`LODESTONE_REGEN=1` enables the repository's ignored regeneration tests when a
fresh external capture is available. No runtime flag changes the function's
selection rules.

## Dependencies

The parser uses `serde_json` and `lodestone_model::ItemStack`. Selection reads
the shared `crate::enchantment_data` census and `crate::enchanting` RNG/weight
logic, while `crate::anvil::apply_enchantment` writes the model's internal
enchantment ids. `crate::structure_loot` is the production consumer exercised
by the external End-city marker fixture.
