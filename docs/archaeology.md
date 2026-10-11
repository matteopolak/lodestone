# Archaeology brushing

## What it is

Suspicious sand and gravel hold a loot table until a player brushes them 10 times, when the rolled item drops and the block becomes plain sand or gravel. The block entity is `BlockEntity::Brushable` (`minecraft:brushable_block`).

## How it works

- **Placement.** Structure processors with an append-loot rule record a table per block; `StructureWorld::attach_loot` stores it on the dense grid (`take_loot`), and `structure_loot` emits a `Brushable` with table and seed. The desert pyramid's coded sand takes the same route.
- **Entity.** `lodestone_server::archaeology::Brushable` holds table and seed, then the rolled item, last hit face, last brush tick and stroke count. Save form is `LootTable` (+ `LootTableSeed` if nonzero) until rolled, then `item`. The client update tag carries `hit_direction` (0 down, 1 up, 2 north, 3 south, 4 west, 5 east) and `item`; `lodestone-shell` renders the item from it.
- **Use.** Right-clicking a block with a brush starts an `ItemInUse` of 200 ticks. `server::brushing::tick` re-aims each tick (a miss ends the use) and strokes at elapsed ticks where `elapsed % 10 == 5`. A stroke plays sound and particles, then calls `Brushable::brush`: ignored inside a 10-tick cooldown, the first counted stroke rolls the loot once, and the dusted state (0..3) steps with the count (0, 1-2 -> 1, 3-5 -> 2, 6+ -> 3). The tenth stroke removes the entity, drops the item from the hit face, turns the block into its base block, costs one durability, and sends level event 3008 (`NetUpdate::BrushCompleted` on the client).
- **Decay.** Counted strokes schedule a `Gravity` tick. `Brushable::settle` waits 40 ticks after the last stroke, then retracts 2 strokes every 4 ticks.
- **Falling.** The block falls like sand; the entity travels with the falling block and is reinserted where it lands.
- **Hydration.** Generated entities stay in the column until first brushed; saves of an edited chunk merge unhydrated brushables back in (`region_source::append_unhydrated_brushables`).
- **First person.** `entity_first_person` poses the brush hand with a swipe of `-15 + 75*cos` over the use progress.

## How to change it

Cadence constants live in `archaeology.rs` (cooldown, reset, retraction) and `server/brushing.rs` (use length, stroke phase). The wasm loop does not brush. Level event 3008 carries a block state id converted per wire in the 26.2 encoder.

## Configuration

None.

## Dependencies

Loot engine (`crate::loot`), gravity ticks (`gravity_tick`), block-entity registry and chunk NBT (`chunk_nbt`), worldgen structure placement.

## Tests

`crates/lodestone-server/tests/archaeology_brushing.rs` brushes structure-placed blocks over the 26.2 wire, and covers settle decay, falling with loot and a save/reopen round trip.
