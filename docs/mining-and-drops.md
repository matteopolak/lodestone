# Mining, drops and loot tables

## What it is

How fast a held item mines a block (the item half of break-time math; the
block half — hardness — lives in [`docs/blocks.md`](./blocks.md)), and the
whole chain that turns a broken block or a dead mob into real item entities:
the server-side loot-table engine, its bundled corpus of vanilla tables, and
generated structures' pre-filled chests.

## How it works

### Tool mining speed

`crates/lodestone-data/src/tool.rs::mining(held, state_id: StateId) -> ToolMining`
implements the reference destroy-speed lookup and correct-tool-for-drops check.
Raw palette numbers are validated once at the version or wire boundary; the
generated-table lookup accepts only a `StateId`, so its result is total and
cannot encode an unknown state as a second, redundant `Option`.
The obvious approach — decode `minecraft:tool` off the wire and evaluate it —
is not enough, because most tools never carry it: a pickaxe's
`minecraft:tool` lives in its **prototype** component map (an empty
clientbound patch), a rule names blocks by **tag** (version data), and a rule
naming blocks directly uses **registry ids** that need the state→block map
(renumbered every version). So this is a version-owned census
(`crate::generated_tools::ITEM_TOOLS`, `generated::BLOCK_TAGS`,
`generated_block_registry::STATE_BLOCK`), dumped from a real 26.2 server
into `crates/lodestone-data/tests/support/tool_jvm.txt`,
regenerated with the same `LODESTONE_REGEN=1` pattern every generated table
here uses, and cross-checked against the generated `components/item/*.json`
reports rather than merely trusted. A wire-supplied `minecraft:tool`
(datapack items) overrides the prototype and funnels through the identical
`evaluate` function, so the two sources cannot diverge in how a rule list is
walked.

`ITEM_TOOLS` is a sparse table of `(u16, ToolDef)` pairs, sorted by
`minecraft:item` registry id. `default_tool` first resolves its canonical item
name through [`Item`](../crates/lodestone-data/src/item.rs), then binary-searches
that id; it deliberately rejects a bare item path even though `Item::from_name`
accepts one. This keeps the tool census from duplicating 38 canonical item
strings while preserving the component-key boundary. Block-tag names remain
strings because they are keys in the tag namespace, rather than item-registry
references.

`evaluate` follows the reference rule order: walk rules in order, first match wins
**independently** for speed and for correct-for-drops (a rule denying drops
does not stop the speed search), falling back to the default mining speed
and to `!requires_correct_tool || correct.unwrap_or(false)`. The bare-hand
case is that same formula with no rules: `speed: 1.0, correct_tool:
!requires_correct_tool` — see [`docs/blocks.md`](./blocks.md) for why feeding
the block's own flag straight into this field is the single most repeated
mistake in this area (45 ticks instead of 151 on bare-hand stone). Reference
values, all read from committed server-derived tables: diamond pickaxe on
stone, 6 ticks; bare hand on stone, 151; diamond pickaxe on obsidian, 188;
wooden pickaxe on obsidian (speed applies, tier denies drops), 2500; bare
hand on obsidian, 5001.

`drive_mining` (an ECS system, `crates/lodestone-shell/src/interact.rs`) reads
the selected hotbar slot off the `SelectedSlot` component (not a lock-taking
`ClientHandle` read — `drive_mining` runs under the same `World` write guard
`SessionMenus` is written through, so a read guard on the same lock would
freeze the client on the first tick of every dig), resolves it through
`tool_mining_item`, and calls `adapter.tool_mining(held, state_id)`, falling
back to `bare_handed_tool_mining` when nothing is held or the state is
unresolvable. `mining_efficiency`/`haste_amplifier`/`mining_fatigue`/
`block_break_speed` remain at their defaults — no enchantment, potion or
attribute input is modelled on this path yet.

`block_type_name`, the registry-id-to-name lookup used while decoding block
events, reads `generated_block_registry::BLOCK_REGISTRY_NAMES`. This table is
in registration order, as required by registry ids; the alphabetical
state-to-name lookup remains for consumers whose ids are alphabetical.

### Block drops

Breaking a block server-side rolls its loot table, spawns the result with the
specified position/velocity draw order, streams it to every connection, and
lets a player walk over it to collect it.

The entity-type field identifies the item-entity kind, not the item it carries.
The carried item travels in entity metadata at index 8 with the item-stack
serializer; see [`docs/items.md`](./items.md) for its decode contract. Keep
these values separate: an unknown entity type resolves through the entity
registry's fallback, while an item stack must retain its item key.

**The spawn draw order is the specification.** Five RNG draws, in the
order x, y, z, vx, vz — `vy` is a constant `0.2` and consumes no draw. A port
that draws in a different order, or draws for `vy`, produces a
statistically-plausible but per-seed-wrong cloud of items. Pickup and
merging use **two different, non-spherical volumes**: pickup is the player's
own box inflated `(1.0, 0.5, 1.0)` (asymmetric vertically — `0.5+0.25` below
the feet, `1.8+0.5` above), merging is `inflate(0.5, 0.0, 0.5)` — the `0.0`
is load-bearing, so stacks side-by-side merge but stacks one cell apart
vertically never do.

**Correct-tool is not a loot condition.** `drops_are_allowed` —
`!requires_correct_tool || is_correct_tool` — gates whether `drop_block_loot`
is even called. Folding it into the roll itself would be
wrong twice: the roll's RNG draws would still consume stream entries for the
next break, and a table with no explicit tool condition would still be
consulted. This computation is the *same* flag mining-speed's `correct_tool`
already is — see the tool-mining section above for the same confusion
recurring here.

**A break does not always write `AIR`.** The break path reads the cell's fluid
state first and writes its replacement block — a dry
block's fluid state is empty (air), but a **waterlogged** block leaves a real
water source behind, exactly as breaking a waterlogged slab does in real
Minecraft. This rule is specific to a player break or a support-collapse
cascade; an explosion or a piston's structural move both write `AIR`
unconditionally regardless of fluid content. Keep these paths distinct.

**Mob death loot** funnels through `MobSim::reap_dead`. Two differences from a
block drop: position
comes from the mob's own location (not a jittered cell), and the loot
context's `killed_by_player` is always `false` (there is no attacker field to
fill), so player-only rare drops and Looting bonuses never apply yet.

### Loot tables

`crates/lodestone-server/src/loot.rs` parses datapack loot-table JSON and rolls
it with the server's deterministic RNG. Its empty context has no entity, level,
or explosion, and `luck = 0`. Each pool's conditions
gate whether it rolls at all, a roll expands the entry tree (an
`alternatives` stops at its first satisfied child; `group`/`sequence` expand
every child), weights are summed and a bounded RNG draw picks the leaf,
and entry/pool/table functions apply in that order.

Every supported condition and function has a **defined empty-context value**
rather than an error, so a table with no unsupported feature rolls correctly
with nothing filled in. State-property conditions read the supplied block
state: for example, the mature-crop condition `age: "7"` selects the crop
drop rather than its seed fallback. A feature that parses but discards its
condition data is not covered by a parse-only gate;
`LootTable::context_blind_features()` reports recognised conditions that are
not evaluated.

**A present tool changes the RNG stream even at enchantment level 0.** The
bonus-count rule first requires a tool, then skips its draw when the level is
zero. Expressing ore drops as `count * max(1, random(level + 2))` has the
right arithmetic but the wrong draw count because it consumes a draw at level
zero. `uniform_bonus_count` is the opposite: it draws once at level zero when
any tool is present.

The bundle (`assets/loot_table/`) is **1,241 of Mojang's 1,355** 26.2 tables,
copied verbatim and regenerated with `just regen-loot-corpus` — never add a
table by hand, since the drift gate compares the tree against the cache in
both directions (every bundled table must be clean, and every clean cache
table must be bundled). The 114 excluded tables use a feature the roller does
not model (`copy_components`, `set_potion`, `enchant_with_levels`, mostly). A
four-entry allowlist (`enchant_randomly`, `exploration_map`, `set_name`,
`set_stew_effect`) lets a table in anyway when the *unsupported* part is
purely decorative — the item and count are still right, only the
enchantment/name/map-target/effect is absent — which is what lets the four
structure-chest tables (below) into the bundle at all.

### Structure chests

Generated shipwrecks, ocean ruins and igloos arrive with chest contents.
Four decisions carry the weight: the data markers that name a chest's table
come from the **raw template bytes**, not the parsed structure (the parser
deliberately drops marker blocks and their NBT compound — `metadata` strings
like `"supply_chest"` exist only in the file); the
piece list comes from `structure_references` (which structures reach *this*
chunk), not `structure_starts` (whose *origin* chunk a shipwreck's chest
routinely is not in — using the origin list loses most chests); the roll is
seeded from the chest's own position alone, since the source column
regenerates fresh on every request and a per-connection seed would make the
same chest hold different loot on every resend (the world seed does not need
mixing in — it already decided where the structure landed, so two seeds never
ask for a roll at the same coordinates); and a chest is a real
`BlockEntity::Container`, hydrated into the live registry on first click,
after which it is authoritative and a regeneration cannot refill it.

Shipwreck and igloo markers sit **one block above** the chest position;
an ocean-ruin marker **is** the chest position — getting this off by one
puts loot in a block of air where nothing can reach it, invisible to a test
that only counts rolls. `drowned` markers inside big ocean-ruin templates
are read and ignored: there is no structure-spawn path for a mob yet.

## How to change it

- Bundle more loot tables: teach the roller the feature they use, then `just
  regen-loot-corpus` — never add files under `assets/loot_table/` by hand.
- Add a condition/function/number-provider: add the enum variant, its parse
  arm, its **empty-context** semantics, and a test that exercises that empty
  context specifically.
- Support another structure's chests: add its `(structure id, marker)` pair
  to `marker_loot_table`, and to `marker_places_chest` if the marker creates
  the chest rather than finding one already in the template. Derive both
  entries from the raw template marker data.
- A mining-speed regression is almost always the `requires_correct_tool` /
  `correct_tool` field pair fed straight across instead of negated — check
  that first before adding new tool logic.

## Configuration

`--protocol <n>` / the `live` feature select which family's tool/tag/registry
census is resolved. `LODESTONE_REGEN=1` on the relevant `#[ignore]`d test
regenerates a committed table from a fresh JVM dump. `block_drops::BLOCK_DROPS_BEHAVIOR_SEED`
is the per-connection roll/placement seed, kept separate from the
composter's own seed so one cannot shift the other's draws.

## Dependencies

`lodestone_data::{hardness, tool, block_states}` for the per-state/per-item
censuses; `lodestone_model::VersionAdapter::{block_hardness, tool_mining}`
for the version-free entry point; `crate::mobs`/`MobSim` for item-entity
spawn, lifecycle and streaming; `crate::inventory` for pickup destination and
menu-slot mapping; `lodestone-worldgen`'s `structure` module for structure
starts and template transforms. See [`docs/blocks.md`](./blocks.md) for the
block-breaking sequence and hardness census this doc's tool half completes,
and [Registries: synchronized data, canonical block states, and generated tables](./registries.md) for how the
generated censuses are dumped and regenerated.
