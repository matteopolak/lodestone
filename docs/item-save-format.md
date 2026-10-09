# Item save format

## What it is

`lodestone_server::item_nbt` is the one save form of an item stack, `{id, count, components?}`, shared by container block entities, dropped item entities, the Anvil player file and the native player record, so any component the server can put on a stack survives a restart wherever it sits.

## How it works

`stack_to_nbt` returns the compound plus `complete`, false when the stack carries something with no saved form here. `stack_from_nbt` reads one back; a component it does not read sets `ItemComponents::has_unmodeled`, so a later strict save refuses the stack rather than silently dropping it.

| component | saved as |
|---|---|
| `custom_data` | the compound itself (the model keeps network-NBT bytes) |
| `damage`, `repair_cost`, `dyed_color` | `Int` |
| `enchantments` / `stored_enchantments` | `{"minecraft:efficiency": 4}`; a book's list goes under `stored_enchantments` |
| `custom_name` | text component: bare string when unstyled, else a compound |
| `lore` | list of text components; all strings, or all compounds if any line needs one |
| `potion_contents` | `{potion: "minecraft:poison", custom_name?}` (bare-id short form also read) |
| `instrument` | the instrument id (an inline instrument has no saved form) |
| `writable_book_content` | `{pages: [{raw: "…"}]}` |
| `written_book_content` | `{title: {raw}, author, generation?, pages: [{raw: <component>}], resolved?}` |

- Prototype-derived fields (`max_stack_size`, `max_damage`, `equippable`) and the mixed potion colour are never saved; they come back from the prototype and potion. Enchantments save by name (`enchantment_data::name_of`) because model ids are positions in a session's registry. An out-of-range integer, an unknown enchantment or potion id, or any other modelled field (trim, map id, profile) makes the stack incomplete.
- Incomplete stacks: **player records** (Anvil `player_data`, native `world_storage`) refuse the save and keep the previous file or record (the native slot keeps `custom_data` in its own field and everything else as the network-NBT `components` compound, field 5 of `PlayerInventorySlot`); **container block entities** (`chunk_nbt`) and **dropped item entities** (`entity_storage`, `ItemEntityState.components`) write what they can and log a warning, since one odd stack must not fail a dimension's save.
- A dropped item keeps components in the world: `ItemState.components` feeds item-entity metadata, pickup and merging (which joins only equal components). A brewing stand saves each bottle's potion as that stack's `potion_contents`; older saves' `lodestone:potions` list is still read when a stack has no potion.
- Witness: `crates/lodestone-server/tests/item_nbt_vanilla.rs` reads a chest the official 26.3 server filled and saved (`capture_item_components.py --saved` in the 26.3 protocol crate). Each stack must read as its `give` describes and write back to the server's compound, comparing keys as a set (the server writes them in hash order).

## How to change it

- **New component:** capture first. Add a case to `SAVED_CASES` in `crates/versions/26.3/tests/capture_item_components.py`, rerun with `--saved`, add the expected stack in `item_nbt_vanilla.rs`, then write reader and writer arms. A shape guessed from the decompiled codec has been wrong before (written-book pages are NBT components, not JSON strings).
- Clear the field from `rest` in `components_to_nbt` when you write it, or `complete` stays false.
- Keep player paths strict: dropping a component from a player inventory is silent data loss.

## Configuration

None.

## Dependencies

`lodestone_core` NBT, `enchantment_data`, `lodestone_data::potion`, and `Text::to_nbt` / `Text::from_nbt` in `lodestone-model`.
