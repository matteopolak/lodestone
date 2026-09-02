# Mining, drops and loot tables

## What it is

How fast a held item mines a block (the item half of break-time math; the
block half — hardness — lives in [`docs/blocks.md`](./blocks.md)), and the
whole chain that turns a broken block or a dead mob into real item entities:
the server-side loot-table engine, its bundled corpus of vanilla tables, and
generated structures' pre-filled chests.

## How it works

### Tool mining speed

`crates/lodestone-data/src/tool.rs::mining(held, state_id) -> Option<ToolMining>`
is vanilla's own destroy-speed lookup plus its correct-tool-for-drops check.
The obvious approach — decode `minecraft:tool` off the wire and evaluate it —
is not enough, because most tools never carry it: a vanilla pickaxe's
`minecraft:tool` lives in its **prototype** component map (an empty
clientbound patch), a rule names blocks by **tag** (version data), and a rule
naming blocks directly uses **registry ids** that need the state→block map
(renumbered every version). So this is a version-owned census
(`crate::generated_tools::ITEM_TOOLS`, `generated::BLOCK_TAGS`,
`generated_block_registry::STATE_BLOCK`), dumped from a real 26.2 server
(`ToolOracle.java` → `crates/lodestone-data/tests/support/tool_jvm.txt`,
regenerated with the same `LODESTONE_REGEN=1` pattern every generated table
here uses) and cross-checked against Mojang's own `components/item/*.json`
reports rather than merely trusted. A wire-supplied `minecraft:tool`
(datapack items) overrides the prototype and funnels through the identical
`evaluate` function, so the two sources cannot diverge in how a rule list is
walked.

`evaluate` replays vanilla exactly: walk rules in order, first match wins
**independently** for speed and for correct-for-drops (a rule denying drops
does not stop the speed search), falling back to vanilla's own default mining speed
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

A separate, unrelated fix landed alongside this: `block_type_name` (a
registry-id → name lookup used by `block_event` decoding, e.g. for note
blocks and pistons) had been indexing a table built in **alphabetical**
order from `blocks.json` against a registry id that is **registration**
order — so every note-block/chest/piston/gateway event named the wrong
block. The fix is a second generated table,
`generated_block_registry::BLOCK_REGISTRY_NAMES`, built from the same
`ToolOracle.java` dump (which needed a correct registry-order table anyway,
since a tool rule's explicit block list uses registry ids). The state→name
lookup used elsewhere (chunk palettes, the mesher) was unaffected — it was
already self-consistently alphabetical throughout.

### Block drops

Breaking a block server-side rolls its vanilla loot table, spawns the result
with vanilla's `popResource` position/velocity draw order, streams it to
every connection, and lets a player walk over it to collect it. The roller
(`loot.rs`) and the item-entity/pickup machinery both already existed before
this was wired end to end; the missing pieces were the call from a broken
block to the roll, the block→table-id mapping, and — a real shipped bug — the
value put on the wire.

**The dropped-item identity bug.** A dropped item's `EntitySnapshot::entity_type`
was set to the *item's own key* (e.g. `minecraft:bone_meal`), but
`entity_type` is an **entity** type field; resolving an unknown entity name
falls back to registry id 0, which is `minecraft:acacia_boat`. **Every
dropped item this server ever spawned arrived at the client as a boat**, with
a clean decode and no error anywhere — a fully-connected wire carrying the
wrong value, the exact class of bug a connectivity scanner cannot see. A
dropped item's true identity travels separately, in `ItemEntity.DATA_ITEM`
(entity-metadata index 8, serializer `ITEM_STACK`) — see
[`docs/items.md`](./items.md) for that field's own decode contract.

**`popResource`'s draw order is the specification.** Five RNG draws, in the
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
is even called, mirroring vanilla's own correct-tool-for-drops check in
its own block-destroy entry point. Folding it into the roll itself would be
wrong twice: the roll's RNG draws would still consume stream entries for the
next break, and a table with no explicit tool condition would still be
consulted. This computation is the *same* flag mining-speed's `correct_tool`
already is — see the tool-mining section above for the same confusion
recurring here.

**A break does not always write `AIR`.** Vanilla's real break/remove path
reads the cell's fluid state first and writes its *legacy block* — a dry
block's fluid state is empty (air), but a **waterlogged** block leaves a real
water source behind, exactly as breaking a waterlogged slab does in real
Minecraft. This rule is specific to a player break or a support-collapse
cascade; an explosion or a piston's structural move both write `AIR`
unconditionally regardless of fluid content, which is a genuine vanilla
divergence, not a bug to "fix" the same way.

**Mob death loot** funnels through one call site, `MobSim::reap_dead`, rather
than the four separate places a dead mob used to be removed from the
simulation — adding drops to only one of those four would have made a cow's
loot depend on how it died. Two differences from a block drop: position
comes from the mob's own location (not a jittered cell), and the loot
context's `killed_by_player` is always `false` (there is no attacker field to
fill), so player-only rare drops and Looting bonuses never apply yet.

### Loot tables

`crates/lodestone-server/src/loot.rs` parses Mojang's real datapack loot-table
JSON and rolls it with the server's own deterministic RNG — the "empty loot
context" #337 asked for: no entity, no level, no explosion, `luck = 0`.
Rolling replays vanilla's own pool-rolling routine exactly: each pool's conditions
gate whether it rolls at all, a roll expands the entry tree (an
`alternatives` stops at its first satisfied child; `group`/`sequence` expand
every child), weights are summed and a `nextInt(totalWeight)` picks the leaf,
and entry/pool/table functions apply in that order.

Every condition/function this module understands has a **defined
empty-context value** rather than an error, so a table with no unsupported
feature rolls correctly with nothing filled in. The one that mattered most in
practice was `block_state_property`, which — before `LootContext` carried a
real block state — **parsed and then hardcoded `false`**: 154 of the 1,241
bundled tables (258 conditions) took the wrong branch on every roll. Reading
`blocks/wheat.json` explains the shape exactly: the crop's own entry is gated
on `age: "7"`, so every wheat break dropped only the fallback seed, never
wheat, at any age — and the same shape silently zeroed slab/candle counts,
and made every bed and door drop **nothing at all**, because their whole pool
was gated on a state property. This is the sharpest instance of a general
rule: a feature that **parses but discards its own condition's data** is
invisible to a "does this parse" gate; `LootTable::context_blind_features()`
now separately reports conditions that are recognised but not evaluated,
specifically because the parse-only gate could not have caught this.

**A present tool changes the RNG stream even at enchantment level 0**, and
this is the single easiest thing here to get wrong — vanilla's own bonus-count
function
guards on `tool != null`, not on `level > 0`. The commonly-quoted
restatement of `ore_drops` as `count * max(1, nextInt(level + 2))` is
arithmetically right and **draw-count wrong**: it draws even at level 0,
where the real vanilla behaviour draws nothing at all with no tool and
nothing with an *unenchanted* tool either (the `if (level > 0)` guard sits
**inside** the function). `uniform_bonus_count` is the opposite trap — it
draws once even at level 0 as long as *any* tool is present.

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

Generated shipwrecks, ocean ruins and igloos arrive with real chest contents.
Four decisions carry the weight: the data markers that name a chest's table
come from the **raw template bytes**, not the parsed structure (the parser
deliberately drops marker blocks and their NBT compound, exactly as vanilla
does — `metadata` strings like `"supply_chest"` exist only in the file); the
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
  the chest rather than finding one already in the template — both come
  straight from that structure's own `handleDataMarker`.
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
and [`docs/lodestone-data-crate.md`](./registries.md) for how the
generated censuses are dumped and regenerated.
