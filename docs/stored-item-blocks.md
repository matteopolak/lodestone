# Stored-item blocks

## What it is

Placement and use of shulker boxes, jukeboxes, chiseled bookshelves and decorated pots, plus the stored contents of each. The pure rules live in `lodestone_server::stored_items`; the player-facing producer is `server::stored_items_use`.

## How it works

`apply_stored_item_use` runs from the use-item-on path just before the generic container menu opens, and is bypassed when the player sneaks with anything held.

- **Shulker box**: placement fills a `Container` block entity from the item's `container` component; use opens a 27-slot menu (`MenuKind::ShulkerBox`, which refuses shulker items, so boxes cannot nest). `lid_viewers::LidViewers` counts openers per box and emits the lid block event (kind 1, value = viewer count) plus the open or close sound on the first open and last close. `LidGuard` releases a player's lids when their connection ends. `destroy_block` drops an item that carries the contents back in `ItemComponents::container`; that component is saved by `item_nbt`, not sent to clients.
- **Jukebox**: a disc plays (level event 1010, data = the song's registry index, which is the alphabetical song order shared with `container_signal::DISC_SIGNALS`); use again ejects it (1011). Songs do not end by themselves.
- **Chiseled bookshelf**: the slot comes from the hit position on the front face (3 x 2 grid, `stored_items::hit_slot`); insert and take update `slot_N_occupied` and `last_interacted_slot`.
- **Decorated pot**: holds one item kind (same item and components) up to the stack limit; each insert or refusal sends a wobble block event (`Wobble::Positive`/`Negative`). Breaking with a sword, axe, pickaxe, shovel, hoe, trident or mace cracks it, and a cracked pot drops sherds; otherwise it drops itself.

Every insert and take goes through `BlockEntityRegistry`, whose mutation marks the comparator dirty set, so comparators re-read without polling. Breaking spills contents via `stored_items::broken_drops`.

## How to change it

New block kinds: add pure rules to `stored_items`, a branch in `apply_stored_item_use`, and the break behaviour in `destroy_block`. Block events ride `WorldEffect::BlockEvent`, encoded by `ServerProtocol::encode_block_event`.

Gaps: only player breaks use the spill path (explosions and pistons do not); hoppers can still push a shulker box into a shulker box; the lid sound pitch is fixed; the lid clearance check treats air, fluid and replaceable blocks as clear.

## Configuration

None. The break-tool list and book tag mirror the reference tags and are checked against them in `stored_items/tests.rs` when the reference cache is present.

## Dependencies

`block_entities` (the registry and dirty set), `container_click` (menu layouts), `container_signal` (comparator levels), `item_nbt` (component persistence), `effects::WorldEffect` and the per-family protocol encoder.
