# Item save format

## What it is

`lodestone_server::item_nbt` is the one save-file form of an item stack, `{id, count, components?}`,
shared by container block entities, dropped item entities, the Anvil player file and the native
player record. A component the server can put on a stack therefore survives a restart wherever the
stack sits.

## How it works

`stack_to_nbt` returns the compound fields plus `complete`, which is false when the stack carries
something without a saved form here. `stack_from_nbt` reads one back. A component it does not read
sets `ItemComponents::has_unmodeled`, so a later strict save refuses the stack instead of quietly
writing it without that component.

Saved components, in the reference server's shapes:

| component | saved as |
|---|---|
| `custom_data` | the compound itself (the model keeps it as network-NBT bytes) |
| `damage`, `repair_cost`, `dyed_color` | `Int` |
| `enchantments` / `stored_enchantments` | `{"minecraft:efficiency": 4}`; a book's list goes under `stored_enchantments` |
| `custom_name` | a text component: a bare string when unstyled, else a compound |
| `lore` | a list of text components; all strings, or all compounds when any line needs one |
| `potion_contents` | `{potion: "minecraft:poison", custom_name?}`; the bare-id short form is read too |
| `instrument` | the instrument id (a reference; an inline instrument has no saved form) |
| `writable_book_content` | `{pages: [{raw: "…"}]}` |
| `written_book_content` | `{title: {raw}, author, generation?, pages: [{raw: <component>}], resolved?}` |

Prototype-derived fields (`max_stack_size`, `max_damage`, `equippable`) and the mixed potion colour
are never saved; they come back from the item prototype and the potion. Enchantments are saved by
name (`enchantment_data::name_of`), because the model's ids are positions in a session's registry.
An integer out of the saved range, an unknown enchantment or potion id, or any other modelled field
(trim, map id, profile, …) makes the stack incomplete.

Who does what with an incomplete stack:

- **Player records** (Anvil `player_data`, native `world_storage`) refuse the save, keeping the
  previous file or record. The native slot keeps `custom_data` in its own field and every other
  component as the network-NBT `components` compound (field 5 of `PlayerInventorySlot`).
- **Container block entities** (`chunk_nbt`) and **dropped item entities** (`entity_storage` for
  Anvil, `ItemEntityState.components` in the native store) write what they can and log a warning:
  one odd stack must not fail a whole dimension's save.

A dropped item keeps its components while in the world too: `ItemState.components` carries them to
the item-entity metadata, to pickup, and to merging, which only joins stacks whose components are
equal.

A brewing stand saves each bottle's potion as that stack's `potion_contents`. Older saves carried
the names in a `lodestone:potions` list, which is still read when a stack has no potion.

The witness is `crates/lodestone-server/tests/item_nbt_vanilla.rs`. It reads a chest that the
official 26.3 server filled and saved (`capture_item_components.py --saved` in the 26.3 protocol
crate). Each stack must read as the stack its `give` describes and write back to the server's
compound, comparing keys as a set because the server writes them in hash order.

## How to change it

- **Adding a component:** capture it first. Add a case to `SAVED_CASES` in
  `crates/versions/26.3/tests/capture_item_components.py`, rerun with `--saved`, and add the
  expected stack in `item_nbt_vanilla.rs`. Only then write the reader and writer arms. A shape
  guessed from the decompiled codec alone has been wrong before: written-book pages are NBT
  components, not the JSON strings the codec's name suggests.
- Clear the field from `rest` in `components_to_nbt` when you write it, or `complete` stays false.
- Keep the player paths strict. Dropping a component from a player's inventory is silent data loss.

## Configuration

None.

## Dependencies

`lodestone_core` NBT, `enchantment_data` (enchantment names), `lodestone_data::potion` (potion
names and colours), and `Text::to_nbt` / `Text::from_nbt` in `lodestone-model`.
