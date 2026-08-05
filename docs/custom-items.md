# Custom items

## What it is

The API a plugin uses to define an item the vanilla registry does not have (issue
[#147](https://github.com/matteopolak/lodestone/issues/147)): its own namespaced id, a
vanilla **base item** it is actually made of on the wire, and the components that make it
look and behave like something new.

## Why a custom item is a vanilla item plus a tag

The wire carries an item as a registry **index** into a fixed table, so a genuinely novel
item id is not representable — a server would have nothing to send and a vanilla client
nothing to look up. This is the same ceiling vanilla itself has, and the same one
[#140](https://github.com/matteopolak/lodestone/issues/140) hits for entity types.

Real Bukkit/Paper plugins solve it by attaching a `PersistentDataContainer` tag (usually
plus `custom_model_data`) to a vanilla item id and branching on the tag. We do exactly
that, typed:

```rust,ignore
CustomItem::new("myrpg:flamebrand".parse()?, "minecraft:diamond_sword".parse()?)
    .with_display_name(Text::literal("Flamebrand"))
    .with_custom_model_data(7)
```

`base` is not a fallback or a placeholder. It is what the item **is** on the wire,
permanently; the tag is what makes it yours.

## How it works

| piece | where | what |
|---|---|---|
| `CustomItem`, `CustomItemRegistry`, `CustomItemError` | `crates/lodestone-game/src/custom_item.rs` | definitions, validation, recognition |
| `CustomItems` resource, `CustomItemsPlugin`, `CustomItemsExt::add_custom_item` | `crates/lodestone-ecs/src/items.rs` | the one shared registry |
| `PLUGIN_ITEM_ID_COMPONENT`, `CUSTOM_MODEL_DATA_COMPONENT` + accessors | `crates/lodestone-game/src/item.rs` | the tag itself |

The identity tag is `lodestone:item_id`, **deliberately not `minecraft:`**. A real server
would reject an unknown `minecraft:` component, and a future decoder must not try to resolve
it against `lodestone-data`'s 111-entry component registry. Being outside that namespace is
what makes the tag inert to everything that does not know about it.

Two namespace rules, both enforced by `CustomItem::validate`, pointing in opposite
directions:

- the custom id **must not** be `minecraft:` — it would collide with the vanilla registry;
- the base item **must** be `minecraft:` — nothing else has a wire encoding, and accepting
  one would reproduce #140's `entity_type_id(..).unwrap_or(0)` trap, where an unknown key
  silently becomes something else entirely.

The registry is shared (a resource) rather than per-plugin because the multi-plugin case
needs one owner: an economy plugin has to be able to ask "is this token the shop plugin's?",
and `identify` can only answer that if both registered into the same place.

## How to change it

- **`identify` must stay a pure function of the stack's own components.** No slot position,
  no container, no side table — a stack that has travelled to the server and back must be
  recognisable from nothing but itself, and any other input is lost on that trip.
- **Adding a field to `CustomItem` means touching `apply_to` *and* the round-trip test**, or
  definitions will build stacks that silently do not carry the new field.
- **`with_display_name` stores `minecraft:custom_name` on purpose.** That is a real vanilla
  component, so the name reaches pixels through the display-name path that already exists.
  Inventing a field of its own would have needed new render wiring and would have been an
  island.

## Configuration

None.

## What is verified, and the controls

13 tests. The anti-island one is `a_custom_items_display_name_reaches_the_real_drawn_name`:
it drives `lodestone_game::item::styled_hover_name`, whose production caller is
`lodestone_ecs::session`'s held-item name fold and which
`crates/lodestone-shell/tests/held_item_name_pixels.rs` gates to actual pixels. It asserts
the custom name is present, that it **replaces** rather than appends to the vanilla name, and
that the italic style survived.

Controls, run and observed:

| control | asserts |
|---|---|
| `control_an_untagged_stack_draws_the_vanilla_name_unstyled` | the same base item, no definition → vanilla name, not italic |
| `control_a_plain_stack_of_the_same_base_item_identifies_as_nothing` | `identify` is not matching on the base item |
| `control_an_unregistered_id_is_not_identifiable` | the shared registry does not say yes to everything |
| `an_unknown_tag_degrades_to_the_vanilla_item` | an orphaned item is still a usable diamond sword |

## Known gaps

- **The identity tag does not survive a game → model round trip**, and
  `the_identity_tag_is_recognised_after_a_model_round_trip` asserts that *current* behaviour
  rather than the wish. `lodestone_model::ItemComponents` is a closed struct of nine typed
  fields with no slot for a `lodestone:`-namespaced component, so the tag is dropped the
  moment a stack is lowered — which in practice means **a custom item does not survive a
  trip through the server**. What does survive is everything vanilla-shaped, including
  `minecraft:custom_name`. Closing this needs the model to carry unmodelled components
  (see the `has_unmodeled` gap in [`item-component-access.md`](./item-component-access.md))
  or an explicit passthrough field. The test is written so that closing it **fails loudly**
  rather than silently changing meaning.
- **`custom_model_data` is not decoded from the wire either**, for the same reason: no field
  on the model's component struct. So the selector is writable and readable within
  `lodestone-game`, and a server-sent one is invisible. The v770 decode plus a model field
  is a separate, brokerable change (`crates/protocol/v770` is not this work's to touch).
- **The render-side model substitution hook is not built.** Issue #147's scope item (2) says
  it should coordinate with the client-side custom-draw-buffer issue
  ([#161](https://github.com/matteopolak/lodestone/issues/161)) "rather than inventing a
  second rendering path", and that is the right call — `lodestone-assets`' `item_tint.rs`
  already resolves `custom_model_data` to the JSON default with
  `TintProvenance::Unmodeled`, so the asset side has a seam waiting for a live value. Today a
  custom item can change its **name** on screen but not its **model**.
