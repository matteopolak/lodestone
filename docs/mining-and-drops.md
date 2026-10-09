# Mining, drops and loot tables

## What it is

How fast a held item mines a block (the item half of break-time maths; hardness is in [blocks](blocks.md)) and the chain that turns a broken block or dead mob into item entities: the server-side loot-table engine, its bundled corpus of tables, and generated structures' pre-filled chests.

## How it works

### Tool mining speed

`crates/lodestone-data/src/tool.rs::mining(held, state_id: StateId) -> ToolMining` implements the destroy-speed lookup and correct-tool-for-drops check. Raw palette numbers are validated once at the wire boundary; the generated lookup takes only a `StateId`, so it is total.

Decoding `minecraft:tool` off the wire is not enough: most tools never carry it (a pickaxe's lives in its prototype component map, with an empty clientbound patch), rules name blocks by tag (version data), and direct rules use registry ids needing the state-to-block map (renumbered per version). So it is a version-owned census (`crate::generated_tools::ITEM_TOOLS`, `generated::BLOCK_TAGS`, `generated_block_registry::STATE_BLOCK`) dumped from a real server into `crates/lodestone-data/tests/support/tool_jvm.txt` with `LODESTONE_REGEN=1` and cross-checked against the generated `components/item/*.json` reports. A wire-supplied `minecraft:tool` (datapack items) overrides the prototype and goes through the same `evaluate`.

`ITEM_TOOLS` is a sparse `(u16, ToolDef)` table sorted by item registry id; `default_tool` resolves a canonical item name through [`Item`](../crates/lodestone-data/src/item.rs) and binary-searches (it rejects a bare item path though `Item::from_name` accepts one). Block-tag names stay strings.

`evaluate` walks rules in order, first match wins independently for speed and for correct-for-drops (a rule denying drops does not stop the speed search), falling back to default speed and `!requires_correct_tool || correct.unwrap_or(false)`. Bare hand is the same formula with no rules: `speed: 1.0, correct_tool: !requires_correct_tool`. Feeding the block's own flag straight into this field is the most repeated mistake (45 ticks instead of 151 on bare-hand stone; see [blocks](blocks.md)). Reference values: diamond pickaxe on stone 6 ticks; bare hand on stone 151; diamond pickaxe on obsidian 188; wooden pickaxe on obsidian (speed applies, tier denies drops) 2500; bare hand on obsidian 5001.

`drive_mining` (`crates/lodestone-shell/src/interact.rs`) reads the hotbar slot from the `SelectedSlot` component, not a lock-taking `ClientHandle` read (it runs under the `World` write guard `SessionMenus` is written through; a read guard would freeze the client on the first dig tick), resolves it through `tool_mining_item`, and calls `adapter.tool_mining(held, state_id)`, falling back to `bare_handed_tool_mining` when nothing is held or the state is unresolvable. `mining_efficiency`, `haste_amplifier`, `mining_fatigue` and `block_break_speed` stay at defaults client-side.

The server validator (`crates/lodestone-server/src/block_breaking.rs::progress_per_tick`) adds the Efficiency level from the held stack via `enchantment_data` (`level^2 + 1`, only for tools whose census speed beats hand speed; an unrelated enchantment must not change timing). Potion effects, player break-speed attributes, underwater and on-ground state are follow-ups; the validator's headroom keeps legitimate breaks while rejecting implausible instant stops.

Creative breaking is instant (including negative hardness) and arms a five-tick client delay for held input, but each new press takes the direct path. Progressive survival breaks use the same delay after `STOP`; zero-hardness survival breaks do not arm it. The window path records each ray hit when the press arrives, so a press and release between ticks is still delivered, and queued presses are consumed one per tick in order. The client writes an instant break into its local chunk store and requests a re-mesh before the server answers, retaining the original state and block-entity record until the mining/placement sequence is acknowledged: an acknowledgement without a replacement update rolls back and remeshes, and an authoritative air update keeps the break. This lives in `BreakPredictions` and the network-update fold, not a block-entity special case, and clears on session and dimension change. Breaks of at least 50 ms emit a `lodestone_server::stall` warning with edit, drop, light-queue and neighbour-fanout timings (threshold fixed in `destroy_block`).

`block_type_name` reads `generated_block_registry::BLOCK_REGISTRY_NAMES` (registration order, as registry ids require; the alphabetical state-to-name lookup serves alphabetical ids).

### Block drops

A server-side break rolls its loot table, spawns items with the specified position and velocity draw order, streams them to every connection, and lets a player collect them. The entity-type field names the item-entity kind; the carried item travels in metadata index 8 with the item-stack serializer ([items](items.md)). An unknown entity type resolves through the registry fallback, an item stack must keep its key.

- Spawn draws are the spec: five draws in order x, y, z, vx, vz; `vy` is a constant `0.2` with no draw. Another order, or a `vy` draw, gives a plausible but per-seed-wrong cloud.
- Pickup uses the player box inflated `(1.0, 0.5, 1.0)` (`0.5+0.25` below the feet, `1.8+0.5` above); merging uses `inflate(0.5, 0.0, 0.5)` (the `0.0` is load-bearing: side-by-side stacks merge, vertical neighbours never).
- Correct-tool is not a loot condition. `drops_are_allowed` (`!requires_correct_tool || is_correct_tool`) gates whether `drop_block_loot` is called at all; folding it into the roll would still consume RNG draws for the next break and consult tables with no tool condition.
- A break does not always write `AIR`: it reads the cell's fluid state and writes that, so a waterlogged block leaves a water source. This is for player breaks and support-collapse cascades; explosions and piston moves write `AIR` unconditionally.
- Mob death loot goes through `MobSim::reap_dead`: position is the mob's location (not a jittered cell) and `killed_by_player` is always `false`, so player-only drops and Looting never apply yet.

### Loot tables

`crates/lodestone-server/src/loot.rs` parses datapack loot JSON and rolls it with the deterministic RNG; the empty context has no entity, level or explosion and `luck = 0`. Pool conditions gate rolling; a roll expands the entry tree (`alternatives` stops at its first satisfied child, `group`/`sequence` expand all), weights are summed, a bounded draw picks the leaf, and entry, pool and table functions apply in that order. Every supported condition and function has a defined empty-context value, so a table with no unsupported feature rolls correctly. State-property conditions read the block state (the mature-crop condition `age: "7"` picks the crop over the seed). `LootTable::context_blind_features()` reports recognised conditions that are not evaluated (a parse-only gate misses them).

A present tool changes the RNG stream even at enchantment level 0: the bonus-count rule requires a tool and then skips its draw at level zero, so `count * max(1, random(level + 2))` has the right arithmetic but the wrong draw count. `uniform_bonus_count` draws once at level zero whenever a tool is present.

The bundle `assets/loot_table/` is 1,246 of the pinned 1,355 26.2 tables, copied verbatim by `just regen-loot-corpus` (never add by hand: the drift gate compares both directions). The 109 excluded tables use unmodelled features (`copy_components`, `set_potion`, mostly). A four-entry allowlist (`enchant_randomly`, `exploration_map`, `set_name`, `set_stew_effect`) admits tables whose unsupported part is purely decorative, which is what admits the structure-chest tables. End-city treasure is fully evaluated, including level-based enchantments.

### Structure chests

Generated shipwrecks, ocean ruins, igloos and End cities arrive filled. Four decisions: the data markers naming a chest's table come from the raw template bytes (the parser drops marker blocks and their `metadata` strings like `"supply_chest"`); the piece list comes from `structure_references` (structures reaching this chunk), not `structure_starts` (a shipwreck's chest is routinely outside the origin chunk); the roll is seeded from the chest position alone (the column regenerates per request, and the world seed already decided placement); and a chest is a real `BlockEntity::Container` hydrated into the live registry on first click, after which regeneration cannot refill it.

Shipwreck, igloo and End-city markers sit one block above the chest; an ocean-ruin marker is the chest position. Off by one puts loot in air, invisible to a roll-counting test. `drowned` markers in large ocean-ruin templates are ignored (no structure-spawn path for mobs).

## How to change it

- Bundle more tables: teach the roller the feature, then `just regen-loot-corpus`.
- New condition, function or number provider: the enum variant, parse arm, empty-context semantics, and a test of that empty context.
- Another structure's chests: add its `(structure id, marker)` to `marker_loot_table` (and `marker_places_chest` if the marker creates the chest), derived from raw template markers.
- A mining-speed regression is almost always the `requires_correct_tool`/`correct_tool` pair fed across instead of negated.

## Configuration

`--protocol <n>` or the `live` feature selects which family's tool, tag and registry census is used. `LODESTONE_REGEN=1` on the `#[ignore]`d test regenerates a table from a fresh JVM dump. `block_drops::BLOCK_DROPS_BEHAVIOR_SEED` is the per-connection roll and placement seed, separate from the composter's.

## Dependencies

`lodestone_data::{hardness, tool, block_states}`; `lodestone_model::VersionAdapter::{block_hardness, tool_mining}`; `crate::mobs`/`MobSim` (item entities); `crate::inventory` (pickup); `lodestone-worldgen`'s `structure` module. See [blocks](blocks.md) and [registries](registries.md).
