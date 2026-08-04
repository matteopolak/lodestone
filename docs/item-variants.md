# Item variants: one item, several baked geometries

## What it is

An item's appearance in 26.2 is not one model. `assets/minecraft/items/<id>.json` is a
**selector tree** (`condition` / `select` / `range_dispatch` / `composite`) whose leaves name
concrete models, and which leaf wins depends on where the item is being drawn and on live stack
state. A bow is `item/bow` at rest and `item/bow_pulling_0`, `_1` or `_2` as it is drawn; a
spyglass is the flat `item/spyglass` sprite in an inventory slot and `item/spyglass_in_hand`'s
3-D tube in the hand.

`lodestone_render::ItemVariants` is the axis that makes that possible: **every** model an item's
tree can reach is baked at asset-load time, and a frame resolves the tree against live state to
pick one.

## The defect it replaced

`lodestone_assets::item_model` already parsed and resolved the tree correctly and generally —
sorting `range_dispatch` entries, picking the greatest threshold `<= value * scale`, preserving
unknown node types. **There was no resolver missing.** The defect was one layer down:

`BlockModels::build` resolved each definition **once, at asset-load time, against a static
context** (`GuiItemContext`: every `condition` false, every `range` `0.0`, every `select` `None`
except `minecraft:display_context -> "gui"`), and stored the result in a
`HashMap<ResourceLocation, ItemGeometry>` — **one geometry per item id, with no variant axis**.

Measured over the real jar by `item_variant_gate::the_pack_bakes_more_variants_than_items`,
**84 items bake more than one model** and every one of them was flattened to its inventory form.
By item count the branch properties are `trim_material` 29, `display_context` 26,
`bundle/has_selected_item` 17, `block_state` 12, `using_item` 5, then singles.

Two visible consequences:

- **In-hand geometry.** A spyglass in first person drew the flat sprite instead of the tube;
  likewise every trident, spear and bundle.
- **The transform, not just the geometry.** `ItemIcon::display` is the *first drawable part's*
  model's display map, so a held item took `item/generated`'s `firstperson_righthand`
  (`[0, -90, 25]` at scale 0.68) rather than the in-hand model's. `item/spyglass_in_hand`
  declares no `firstperson_righthand` at all, so vanilla poses it with the identity — the old
  path was a plausible-looking wrong angle, which is worse than an absence because nothing
  reports it.

## How it works

### 1. Discovery, before the atlas is stitched

`collect_item_variants` (in `block_models.rs`) parses each definition once
(`ItemIconBuilder::definition`) and walks `ItemModel::outputs` — the **union over every branch**,
so it is context-free by construction and cannot miss a variant a context might later ask for.
Each distinct model ref is classified by `ItemIconBuilder::part_for_model` into a 3-D
`ItemModelPart` or a flat `ItemSpritePart`.

**This is the part that cannot be deferred to draw time, and it is why this was not small
wiring.** `item/bow_pulling_0/1/2` are `item/generated` sprite models — `parent: item/bow` →
`item/generated`, with only `layer0` swapped — so their geometry is walked out of the *alpha
outline of a stitched sprite* by `extruded_sprite_geometry`. Before this work those three
textures were in no atlas at all: unreachable from any blockstate, and unreachable from any
*baked* item model because only the GUI variant was ever resolved. A per-frame "resolve then
bake" would have found no sprite and drawn nothing, however correct the resolver.

`build_complete_atlas` therefore seeds every variant's textures, not just the GUI one's.

### 2. Baking, keyed by `(item, model)`

Each variant bakes against the same stitched atlas and interns through the same tint palette as
the block states — unchanged from before, only the key is wider. The results regroup into
`item -> ItemVariants { definition, by_model, gui }`.

`gui` is the ref the inventory slot resolves to, and it preserves the old preference exactly:
the first `IconPart::Model` the GUI resolution produces, else its first `IconPart::Sprite`. That
"model before sprite" order is not tree order — it is what the single-geometry code did — so
this change cannot silently move which part of a mixed `composite` an inventory slot draws.

### 3. Resolution, per draw

`ItemVariants::resolve(&ctx)` runs the real resolver and looks the chosen ref up in the pre-baked
map, falling back to the inventory form. The context is `lodestone_render::ItemStateContext`,
which lives in `item_render.rs` rather than in `lodestone_assets` because the property values are
live game state that the GPU-free asset crate deliberately does not own — it supplies only the
`ItemPropertyContext` trait.

## What consumes it

Five draw sites, all of which previously took the flattened form:

| site | file | context |
|---|---|---|
| first-person hand | `gpu/first_person.rs` | `ARM.display_slot(true)` |
| mobs' / remote players' hands | `gpu.rs::merge_held_items` | `arm.display_slot(false)` + `ItemUse` |
| dropped items | `gpu.rs::prepare_item_geometry` | `DisplaySlot::Ground` |
| projectiles | `gpu.rs::merge_thrown_item` | `DisplaySlot::Ground` |
| inventory / hotbar / container slots | `hud/item_icon.rs` | `BlockModels::item` (the GUI form) |

The display slot comes from `Arm::display_slot`, which is the **same expression**
`hand_transform` reads the pose from — so the variant and its transform cannot disagree about
which hand. Passing `first_person: false` where `true` is meant reads
`thirdperson_righthand`: a different rotation and scale, and a wrong angle rather than an
absence.

`ModelRenderer::items` (the shell's snapshot, taken while `BlockModels` is still borrowable) now
holds `ItemVariants` rather than `ItemGeometry`. Snapshotting one geometry per item there would
have re-created the whole defect one layer further out, which is why `BlockModels::items()` —
still yielding the inventory form — is *not* what that snapshot calls.

### The state half reaches pixels on mobs first

`ItemUse` (`lodestone_ecs::entity`) is already folded for mobs and remote players, and
`gpu.rs::merge_held_items` already poses their held items off `EntityDraw::equipment`, so
`EntityDraw::item_use` was the only missing hop. `off_hand` is load-bearing there in a way it is
not for the arm pose: vanilla's `using_item` is
`owner.isUsingItem() && owner.getUseItem() == itemStack`, so the flag must be compared against
the arm being drawn or a skeleton drawing a bow would bend its off-hand item too.

**The local player is the one remaining gap.** `ItemUse` is an *ingest* component and the local
player has no ingest entity (`apply_local_player_login` gives it no `EntityKind`/`Position`), so
our own bow still draws slack while every remote player's and every mob's does not. Closing it
needs a session-level fold shaped exactly like `Vitals`:

1. a session component carrying `using` / `off_hand` / `ticks`, folded in
   `session::handles_event` — **not** `ingest::handles_event`; per-entity state is `ingest`,
   local-player scalars are `session`, and an arm added to the wrong one compiles, unit-tests
   green and never runs;
2. a `PlayerSnapshot` field for it in `sim.rs`;
3. a `RenderState` source setter, read in `prepare_first_person_hand` where `hand_ctx` is
   currently built with `using: false`.

## Properties: what is sourced and what is not

Nothing is guessed. An unsourced property reads as *unset* (`false` / `None` / `0.0`), which
routes a `condition` to `on_false` and a `select`/`range_dispatch` to its `fallback` — i.e. to
the item's default appearance, which is what shipped before this existed.

**Sourced:**

| property | source | vanilla |
|---|---|---|
| `minecraft:display_context` | `ItemStateContext::display` | `ItemDisplayContext`, static per pass |
| `minecraft:using_item` | `ItemUse::using`, narrowed by hand | `isUsingItem() && getUseItem() == stack` |
| `minecraft:use_duration` | `ItemUse::ticks` **directly** | `stack.getUseDuration() - remaining` |
| `minecraft:crossbow/pull` | `ItemUse::ticks / CROSSBOW_CHARGE_TICKS` | `useDuration / getChargeDuration` |

**Unsourced, and why:** `trim_material`, `bundle/has_selected_item`, `block_state`,
`has_component`, `charge_type`, `broken`, `fishing_rod/cast`, `damage`, `count`, `cooldown`,
`custom_model_data` all need per-stack components that `lodestone_model::ItemComponents` has no
field for (and an unmodelled component halts the patch decode). `time`, `local_time`,
`context_dimension` and `compass` need a level clock or dimension this type is not given.
`use_cycle` needs per-item `getUseDuration` — see below.

### `use_duration` counts UP; `use_cycle` counts DOWN

This is the trap in the family, and it sits on the *other* property from the obvious one.
Vanilla's `UseDuration.get` returns `stack.getUseDuration(owner) - owner.getUseItemRemainingTicks()`,
i.e. `getTicksUsingItem()`, which **increases** from 0. `ItemUse::ticks` already *is* that number
— it counts up from the rising edge of the using-item bit precisely so no per-item
`getUseDuration` lookup is needed — so it is fed in **directly, with no inversion**.

`UseCycle` in the same package is `getUseItemRemainingTicks() % period`: the opposite direction,
and it needs the per-item `getUseDuration` we do not model (a brush's is 200 ticks). It is
therefore unsourced. An "obvious" `duration - ticks` inversion applied to `use_duration` to make
the two look alike would pin a drawn bow at `bow_pulling_0` forever while reading, from the
property name alone, perfectly correct —
`feeding_the_counter_backwards_would_pin_the_bow_at_full_draw` is the control for exactly that.

`crossbow/pull` additionally returns 0 in vanilla when the crossbow is already **charged**, which
reads the `minecraft:charged_projectiles` component we do not decode. A charged crossbow
therefore keeps whatever wind fraction its last using tick left instead of snapping back.

## Verification

**The existing pose gates are not controls for this work.** `first_person_hand_light_pixels` and
`thrown_and_held_item_pixels` both draw a held item and both pass unchanged, because every pose
and every variant is the identity at `using == false` — the only state either can produce. Their
green says nothing about whether a drawn bow resolves.

- `crates/lodestone-render/src/item_render.rs` tests — the crossings through the real
  `ItemStateContext`, with `using` driven **true**, plus the inversion control and an
  unsourced-property roster.
- `crates/lodestone-render/tests/item_variant_gate.rs` (`#[ignore]`d, needs the jar) — the half a
  hermetic test structurally cannot see: that all four `bow_pulling_*` geometries actually
  **baked**, that they sample **different atlas rects** (four aliases of one sprite would satisfy
  a quad-count assertion), and that the in-hand spyglass differs from the slot form in both
  geometry and `firstperson_righthand` transform.

**Bow crossings, predicted from Mojang's numbers and then measured.** `items/bow.json` carries
`scale: 0.05` with thresholds `0.65` and `0.9`, so the crossings are at `0.65 / 0.05 = 13` and
`0.9 / 0.05 = 18` ticks:

| ticks | model |
|---|---|
| not using | `item/bow` |
| 0–12 | `item/bow_pulling_0` |
| 13–17 | `item/bow_pulling_1` |
| ≥ 18 | `item/bow_pulling_2` |

## Configuration and cost

No flags. Both costs come from baking variants that no sourced property can currently select, and
both figures below are **printed by `the_pack_bakes_more_variants_than_items`** against the real
jar rather than quoted from a one-off probe — run it if you change the discovery pass:

- **Geometry: 2,012 baked `(item, model)` variants for 1,474 items with geometry** — 538 refs the
  old one-per-item path never baked. `clock` alone bakes 64 and `compass` 32, all resolving to
  their fallback until `minecraft:time` has a source.
- **Atlas: 2,061 stitched sprites.** Widened because every variant's textures are seeded, not just
  the GUI form's; the new arrivals are dominated by `item/clock_00..63`, `item/compass_00..31`, the
  44 `trims/items/*`, and the bundle open-front/back pair per dye colour. The atlas sizes itself
  from total area (`next_pow2(isqrt(total_area))`), so there is no cap to hit and no stitch failure
  — the cost is VRAM. Nearly all of the new sprites are 16×16 and none are animated, so no
  animation slots are consumed. There is deliberately **no before/after pair quoted here**: the
  "before" number cannot be reproduced without reverting the discovery pass, and an unreproducible
  number in prose is exactly the staleness this repo keeps paying for.

Narrowing the bake to variants whose selecting property is *currently* sourceable would recover
both, and is deliberately not done: it couples the baker to the context's capabilities, so the
day a property gains a source the geometry would silently be absent instead of merely unselected.

## How to change it

- **Sourcing another property** — teach `ItemStateContext` to answer it, and delete its row from
  the unsourced roster in `unsourced_properties_read_as_unset`. Nothing in `block_models.rs`
  changes: every variant is already baked. If the property needs per-stack data, the real work is
  upstream in `ItemComponents` and `RenderEquipment`, which narrows a stack to a bare item id
  long before a draw.
- **A new node type** — `lodestone_assets::item_model`. `ItemModelNode::Other` preserves
  unknown types rather than failing the parse, so a newer pack degrades to the fallback.
- **A new draw site** — resolve, do not reach for `BlockModels::item`. That accessor is the
  inventory form specifically.
- **Per-part `composite` transformations** are still unparsed, so a composite bakes every part
  under its own ref but only the first is ever resolved to. In vanilla 26.2 that is the 16 beds
  (`block/<colour>_bed_head` + `_foot` with a `translation [0, 0, 1]`), named in
  `BlockModels::item_bake_misses`.

## Dependencies

- `lodestone_assets`: `item_model` (the tree, `outputs`, `resolve`), `icon`
  (`ItemIconBuilder::{definition, part_for_model}`, `DisplayContextItemContext`), `model`
  (`DisplaySlot`, `DisplayTransforms`), `atlas`, `bake`.
- `lodestone_render`: `block_models` (`ItemVariants`, `ItemGeometry`), `item_render`
  (`ItemStateContext`), `entity` (`Arm::display_slot`, `hand_transform`).
- `lodestone_ecs`: `entity::ItemUse` — the using-item state, ticked locally because the server
  never syncs `useItemRemaining`.
- Vanilla reference: `client/renderer/item/properties/{numeric,conditional}/*`,
  `world/item/ItemDisplayContext.java`, `world/item/CrossbowItem.java`,
  `client/resources/model/cuboid/ItemModelGenerator.java`.

## Related

- [Item GUI geometry](./item-gui-geometry.md) — the pose/projection half, and the inventory slot.
- [First-person held item](./first-person-held-item.md) — the hand chain the resolved variant is
  posed by.
- [Arm poses](./item-use-arm-poses.md) — the *arms* half of the same `ItemUse` state; the model
  half is this doc. Both read `CROSSBOW_CHARGE_TICKS`, which is one constant shared between them
  so a crossbow's arms and its model cannot disagree about the same wind.
- [Dropped items](./dropped-items.md) — the `Ground` context's consumer.
