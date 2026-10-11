# Block removal

## What it is

The one path every block removal shares. Mining, explosions, pistons, fluids, lost support and `/setblock` or `/fill` all end in `block_removal`, so a chest blown up by TNT spills as surely as one mined.

## How it works

`block_removal::destroy` (or `settle`, when the caller already rolled the loot) takes the cell's entity out of `BlockEntityRegistry` through `destroy_at`, the only function that can: the registry's plain `remove` is private to `block_entities`, and `block_removal/tests.rs` scans the sources for a second caller. What drops depends on the `Cause`:

- `Mined`: the loot table rolls with the held tool.
- `Blast`: the table rolls with the blast radius, which is what thins a crater. `block_drops::blast_blocks` only writes air and reports what it destroyed; the tick loop's detonation drain does the rest. A blast that reaches a non-resident cell undoes its writes (`ResidentTickSource::roll_back`) before it retries, so the retry still sees every block and removes each entity.
- `Natural`: a piston, fluid, lost support or `/setblock destroy`; no tool.
- `Cleared`: a command replaced the block (`/setblock` without `destroy`); the entity goes and nothing drops.

Contents spill whatever the cause (`BlockEntity::spilled_contents`), except that a shulker box and a decorated pot ride on the item they drop (`stored_items::broken_drops`). A generated container that nothing has opened still sits in the chunk column, so `Env::stored` falls back to the terrain's entity. Jukeboxes send the stop event.

A broken hive or nest releases its bees only when the cause calls for it: a player dig outside creative with a tool lacking silk touch, or a blast from primed TNT, a creeper, a wither or a TNT minecart (`Cause::Mined { creative, .. }`, `Cause::Blast { empties_hives, .. }`). Beds, anchors, crystals and every other cause just lose the bees. `MobSim::anger_bees_after_hive_break` then angers released bees within 4 blocks of the digger at them, and gives every other ungrudged bee within 8 blocks horizontally and 6 vertically a random nearby player as its target. A blast angers regardless of the player.

Open windows close from the viewing connection: each connection's container sync tick asks `open_containers::menu_still_valid`, which is false once the anchor is gone or the eye is 8.5 or more blocks from the block's cube (4.5 reach + 4.0). A chest-like, shulker, beacon or lectern menu is anchored by its registry entity (every removal takes it out, so every cause is covered); the crafting table, enchanting table and the anvil (any damage level), grindstone, smithing table, loom and stonecutter menus by the block itself (`station_blocks`). The tick sends `ServerProtocol::encode_close_container` (an encoder per hosted family), then `close_menu` settles the window as a client close does: grid cells and cursor return to the inventory (overflow is thrown), a beacon payment spills, furnace experience is paid, the lid is released.

Reaction code running over borrowed columns (the piston branch of `random_tick::redstone`) cannot spawn items, so it calls `destroy_deferred` and the tick loop drains the removals with `drain_deferred` once per tick.

`/setblock` and `/fill` take `replace` (default), `destroy` or `keep` (only into air). `apply_edit` is the single implementation for in-game, command-block and RCON edits.

## How to change it

A new removal site calls `destroy` with a `Cause`; never remove from the registry directly. A container kind that holds items teaches `spilled_contents` or `broken_drops` its slots. Shulker boxes are `Destroy` for pistons.

Gaps: no loot tables exist for containers (they use item component copy in the reference, handled by `broken_drops`); silk-touch hive drops lose their bees (no bee-carrying item component); hive release ignores campfire sedation; there is no cartography table menu; `/fill` has no hollow, outline or replace filter; `keep` feedback counts every cell.

## Configuration

The block-drops game rule (`WorldState::block_drops`) gates the loot half. Contents spill regardless.

## Dependencies

`block_entities` (registry), `block_drops` (tables, pop placement, `blast_blocks`), `stored_items`, the mob sim (item entities) and `tick::BlockTickFeed` (level events).
