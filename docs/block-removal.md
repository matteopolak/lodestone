# Block removal

## What it is

The one path every block removal shares. Mining, explosions, pistons, fluids, lost support and `/setblock` or `/fill` all end in `block_removal`, so a chest blown up by TNT spills as surely as one mined.

## How it works

`block_removal::destroy` (or `settle`, when the caller already rolled the loot) takes the cell's entity out of `BlockEntityRegistry` through `destroy_at`, the only function that can: the registry's plain `remove` is private to `block_entities`, and `block_removal/tests.rs` scans the sources for a second caller. What drops depends on the `Cause`:

- `Mined`: the loot table rolls with the held tool.
- `Blast`: the table rolls with the blast radius, which is what thins a crater. `block_drops::blast_blocks` only writes air and reports what it destroyed; the tick loop's detonation drain does the rest.
- `Natural`: a piston, fluid, lost support or `/setblock destroy`; no tool.
- `Cleared`: a command replaced the block (`/setblock` without `destroy`); the entity goes and nothing drops.

Contents spill whatever the cause (`BlockEntity::spilled_contents`), except that a shulker box and a decorated pot ride on the item they drop (`stored_items::broken_drops`). A generated container that nothing has opened still sits in the chunk column, so `Env::stored` falls back to the terrain's entity. Jukeboxes send the stop event and a broken hive releases its bees on a player's dig.

Reaction code running over borrowed columns (the piston branch of `random_tick::redstone`) cannot spawn items, so it calls `destroy_deferred` and the tick loop drains the removals with `drain_deferred` once per tick.

`/setblock` and `/fill` take `replace` (default), `destroy` or `keep` (only into air). `apply_edit` is the single implementation for in-game, command-block and RCON edits.

## How to change it

A new removal site calls `destroy` with a `Cause`; never remove from the registry directly. A container kind that holds items teaches `spilled_contents` or `broken_drops` its slots. Shulker boxes are `Destroy` for pistons.

Gaps: no loot tables exist for containers (they use item component copy in the reference, handled by `broken_drops`); a brewing stand spills its ingredient and fuel but not its bottles; `/fill` has no hollow, outline or replace filter; other players' open container windows are not closed when a block goes by a non-player cause; `keep` feedback counts every cell.

## Configuration

The block-drops game rule (`WorldState::block_drops`) gates the loot half. Contents spill regardless.

## Dependencies

`block_entities` (registry), `block_drops` (tables, pop placement, `blast_blocks`), `stored_items`, the mob sim (item entities) and `tick::BlockTickFeed` (level events).
