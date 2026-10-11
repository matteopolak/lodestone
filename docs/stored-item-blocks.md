# Stored-item blocks

## What it is

Placement and use of shulker boxes, jukeboxes, chiseled bookshelves and decorated pots, plus the stored contents of each. The pure rules live in `lodestone_server::stored_items`; the player-facing producer is `server::stored_items_use`.

## How it works

`apply_stored_item_use` runs from the use-item-on path just before the generic container menu opens, and is bypassed when the player sneaks with anything held.

- **Shulker box**: placement fills a `Container` block entity from the item's `container` component; use opens a 27-slot menu. Boxes never nest: the menu, a hopper and a dropper all ask `block_entities::Restriction::NoShulkerBoxes` (via `BlockEntity::restriction`) and a refused source slot is skipped. `lid_viewers::LidViewers` counts openers per box and emits the lid block event (kind 1, value = viewer count) plus the open or close sound on the first open and last close. `LidGuard` releases a player's lids when their connection ends. A broken box drops an item carrying the contents (trailing empty slots trimmed) in `ItemComponents::container`. The component is saved by `item_nbt` and sent as `minecraft:container` on both protocols (a list of up to 256 optional item-first templates); the client decoder reads it back, but the game crate's stack conversion does not keep it, so there is no shulker tooltip yet.
- **Jukebox**: a disc plays (level event 1010, data = the song's registry index, which is the alphabetical song order shared with `container_signal::DISC_SIGNALS`); use again ejects it (1011). `BlockEntityRegistry::tick_jukeboxes` runs every tick for jukeboxes whose block has a record: the song ends after its length plus one second (`stored_items::song_ticks`, lengths copied from the jar's song files and checked against them in tests), sends 1011 and clears `ticks_since_song_started`. The disc stays and the comparator keeps reading it. While a song plays the jukebox is a weak power source of 15 to every neighbour (`RedstoneLookup::jukebox_playing`, never a direct signal); starting, ending or ejecting marks the position in `analog_dirty` and the tick loop propagates the change from it.
- **Chiseled bookshelf**: the slot comes from the hit position on the front face (3 x 2 grid, `stored_items::hit_slot`); insert and take update `slot_N_occupied` and `last_interacted_slot`.
- **Decorated pot**: holds one item kind (same item and components) up to the stack limit; each insert or refusal sends a wobble block event (`Wobble::Positive`/`Negative`). Breaking with a sword, axe, pickaxe, shovel, hoe, trident or mace cracks it, and a cracked pot drops sherds; otherwise it drops itself.

Every insert and take goes through `BlockEntityRegistry`, whose mutation marks the comparator dirty set, so comparators re-read without polling. Every way a block leaves the world spills contents through `block_removal` (see [block-removal.md](./block-removal.md)).

## How to change it

New block kinds: add pure rules to `stored_items`, a branch in `apply_stored_item_use`, and, for what it drops, `stored_items::broken_drops`. Block events ride `WorldEffect::BlockEvent`, encoded by `ServerProtocol::encode_block_event`.

Gaps: the lid sound pitch is fixed; the lid clearance check treats air, fluid and replaceable blocks as clear.

## Configuration

None. The break-tool list and book tag mirror the reference tags and are checked against them in `stored_items/tests.rs` when the reference cache is present.

## Dependencies

`block_entities` (the registry and dirty set), `container_click` (menu layouts), `container_signal` (comparator levels), `item_nbt` (component persistence), `effects::WorldEffect` and the per-family protocol encoder.
