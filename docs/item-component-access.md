# Item component read/write for plugins

## What it is

The typed surface a plugin uses to read and write an item stack's data components
(issue [#143](https://github.com/matteopolak/lodestone/issues/143)) — custom name, damage,
dye, enchantments, tool patch — plus the game → model lowering that lets a mutated stack
reach a consumer keyed on the other stack type.

Issue #143 asked for an **audit** first: "does a plugin have a *write* path to an item's
component set, or is `ItemComponents` read-only outside the decode path?" This doc records
the audit's answer and what closing it required.

## The audit result

There are **two** `ItemStack` types and **two** `ItemComponents` types, and they are shaped
differently:

| | `lodestone_model::ItemStack` | `lodestone_game::item::ItemStack` |
|---|---|---|
| fields | all `pub` | all private, accessors |
| components | closed struct of 9 typed fields | opaque `BTreeMap<Identifier, ComponentValue>` |
| who holds one | `Equipment`, `DisplayItem`, server `Inventory` | `Menus` / every container + HUD path |

Findings, each verified against the tree rather than assumed:

1. **Model side: already fully writable.** Every field is `pub`, and plugin-reachable
   carriers exist (`lodestone_ecs::entity::{DisplayItem, Equipment}`). Nothing to close.
2. **Game side: `components_mut()` was public and had zero production callers.** Every
   call site was a test. Not an accident — writing that map correctly required knowing the
   exact key string *and* the right `ComponentValue` variant *and* the
   `Inherited`-is-not-absent rule for `minecraft:tool`. Get any of the three wrong and the
   component reads back as absent, silently.
3. **`dyed_color` was silently dropped at the crate boundary — a real, shipped bug.** The
   forward `From<&model::ItemStack>` had no branch for it and this crate defined no key for
   it, so dyed leather armour rendered dyed on a *body* (that path reads the model stack off
   `Equipment`) while the same item's **GUI icon did not**. `hud/item_icon.rs` even
   documented the absence as a fact of life rather than a defect.
4. **There was no game → model conversion at all.** The only path in that direction was
   `lodestone_shell::sim`'s `tool_mining_item`, which reconstructs a model stack carrying
   **only** `minecraft:tool` and zeroes everything else — while its own doc claimed "the
   round trip is exact in both directions". So a plugin that mutated a game stack had
   nowhere to send the result.

So the write half existed in principle and was unusable in practice, and the read half was
lossy in one specific place.

## How it works

Typed accessor pairs on `lodestone_game::item::ItemStack`
(`crates/lodestone-game/src/item.rs`): `custom_name`/`set_custom_name`,
`damage`/`set_damage`, `dyed_color`/`set_dyed_color`, `enchantments`/`set_enchantments`,
`tool`/`set_tool`. All of them funnel through one private `write_component`, so the
"unparseable key is a silent no-op" behaviour lives in one place rather than eleven.

They are deliberately **not** a second store: every accessor reads and writes the same
`ItemComponents` map the decoder writes, so a plugin-built stack and a decoded one compare
equal and therefore *merge*. That property is the test
`a_plugin_write_is_indistinguishable_from_a_decoded_component`.

`impl From<&game::ItemStack> for lodestone_model::ItemStack` is the new lowering. Two
conventions it encodes:

- **Clearing removes, never zeroes.** `set_dyed_color(None)` deletes the component; an
  empty `set_enchantments(vec![])` deletes it too. Storing an empty component would make
  two otherwise-identical stacks refuse to merge.
- **`ToolPatch::Inherited` is not a value.** Setting it *removes* the component, and an
  absent component *reads back* as `Inherited`. Getting this backwards is the trap
  `lodestone_model::ToolPatch`'s own docs describe: it makes every real pickaxe mine at
  fist speed.

## How to change it

- **Adding an accessor: add it to the round-trip test in the same commit.**
  `every_modelled_component_survives_a_game_model_round_trip` populates *every* modelled
  component at once and asserts `model -> game -> model` is exact. A per-field test would
  pass while the whole-struct lowering dropped a neighbour, which is exactly how
  `dyed_color` went missing in the first place.
- **There are two `EquipmentSlot` types too.** `lodestone_game::container::EquipmentSlot`
  and `lodestone_model::EquipmentSlot` have the same name and the same eight variants and
  are distinct types. The lowering reads the equippable slot out of the component map *by
  name* through `lodestone_model::EquipmentSlot::from_name` rather than going through
  `container::equippable_slot`, which returns the wrong one. The compiler caught this; it
  would not have caught it if the two types had had different variant sets and a `From`
  impl existed.
- `from_name` is a hand-written inverse of `name`, and two hand-written inverse matches are
  exactly what drifts — `equipment_slot_names_round_trip` iterates `EquipmentSlot::ALL`
  rather than a restated list, and asserts `ALL.len() == 8` so a variant added to the enum
  but not to `ALL` cannot make the loop vacuously pass over a short list.

## Configuration

None.

## Dependencies

`lodestone-model` for the wire vocabulary (`Text`, `ToolPatch`, `ItemEnchantment`,
`EquipmentSlot`). No protocol crate — components here are `Identifier`-keyed, never
numeric.

## Known gaps

- **`has_unmodeled` does not cross into `lodestone-game` in either direction.** The forward
  conversion never carried it (documented there as deliberate) and the lowering therefore
  always writes `false`. Consequence to know: once a stack crosses into `lodestone-game`
  there is **no way to tell that its component set is partial**, so a plugin cannot
  distinguish "bare" from "we stopped decoding". Closing it needs a representation for the
  flag in the opaque map, which would change stack-merge equality for every live stack
  carrying an unmodelled component — a behaviour change worth its own issue rather than a
  rider on this one.
- **8 of 111 data components are modelled** (`DATA_COMPONENT_TYPE_COUNT = 111` in
  `lodestone-data`). A plugin needing an unmodelled one falls through to the escape hatch
  (#159, depend on a version crate directly) by design — issue #143's own scope refuses a
  raw-NBT carve-out in the main API.
- **`ComponentValue::Opaque` and `::Bool` still have zero constructors anywhere.** `Opaque`
  is the designed slot for an arbitrary plugin payload and remains an unused extension
  point — issue #147 (custom items) is what should claim that space.
- **Dropped-item entities carry no components at all.** `lodestone_shell::entities`'
  `TrackedStack` is `{ id, count }`, so anything a plugin writes onto a dropped item is
  lost structurally, independent of this seam.
