# Block support, placement consumption, and `Item.use`

## What it is

Three joins the integrated server was missing, all of them producer-side gaps
rather than missing models: a block whose support is destroyed now pops off and
drops, placing a block now costs you the block, and a right-click in mid-air now
eats food and equips armour instead of doing nothing at all.

## How it works

### Support breaking — `crate::block_support` + `server::collapse_unsupported`

Vanilla destroys a block when its support leaves. `BlockBehaviour.updateShape`
returns `Blocks.AIR` and `Block.updateOrDestroy` turns that into
`Level.destroyBlock(pos, true)` — loot rolled, cell set to air. This crate had
the *delivery* mechanism (`crate::neighbor_update`'s depth-first cascade, faithful
and unit-tested) and **no reaction to deliver to**: every arm of
`random_tick::react_to_notification` was redstone or gravity, so torches,
flowers, rails, doors, beds and sugar cane were all unbreakable-by-support.

`crate::block_support` answers two questions:

| function | vanilla | answer |
|---|---|---|
| `requirement(pos, state)` | which cell `canSurvive` reads | `Supported(cell)` or `Partner { cell, block, property, value }` |
| `survives(pos, state, block_at)` | `canSurvive` itself | `true` for every block with no modelled rule |

`SUPPORT_KINDS` (291 rows) is **generated**, not hand-written, by
[`scripts/derive-block-support.py`](../scripts/derive-block-support.py): it walks
`Blocks.java`'s registrations for block name → implementing class, then every
`class X extends Y` under the decompiled block tree for the ancestor chain, and
maps each block to the nearest ancestor whose `canSurvive`/`updateShape` pair is a
self-destruct on one *named* support cell. Seven kinds come out —
below, attached-facing, attach-face, double-block, bed-part, hanging,
hanging-or-below.

The cascade lives in `server::collapse_unsupported`, called from
`server::destroy_block` right after the broken cell goes to air. It seeds a queue
with the broken cell's six neighbours, removes any whose support is gone, and
re-queues *that* cell's neighbours — which is what makes a stack of sugar cane
collapse all the way up and a door's upper half follow its lower. Bounded by
`MAX_SUPPORT_COLLAPSE` (64), this crate's stand-in for
`maxChainedNeighborUpdates`.

**The same landing gave `destroy_block` its neighbour fan-out.**
`propagate_placement` had exactly one caller (`apply_use_item_on`), so breaking a
block ran no `neighborChanged` pass at all — breaking a block beside redstone dust
never recomputed the dust. `destroy_block` now runs the shapes pass
(`collapse_unsupported`) and then the neighbour pass, in vanilla's order.

### Placement consumption — `server::apply_use_item_on`

`BlockItem.place`'s tail is `itemStack.consume(1, player)`, and
`ItemStack.consume` is `if (entity == null || !entity.hasInfiniteMaterials())
shrink(count)`. The placement branch never touched the inventory, so **every
placement was free**. It now routes through `consume_one`, which is creative-gated
and clears the slot outright at a count of one rather than leaving a zero-count
stack naming an item. The remainder is reported to the client on window 0's hotbar
slot — the same server-initiated slot update the composter, brewing-stand,
bone-meal and spawn-egg arms already send.

### `Item.use` — `crate::item_use` + `server::apply_use_item`

`apply_use_item` was a match on `launch_intent` and nothing else, so every
right-click that was not a bow or a throwable reached the server and did nothing.
`Item.use` in 26.2 has four arms in a fixed order:

1. `CONSUMABLE` → `Consumable.startConsuming` — **implemented**
2. `EQUIPPABLE`, gated on `swappable()` → `Equippable.swapWithEquipmentSlot` — **implemented**
3. `BLOCKS_ATTACKS` → `startUsingItem` (shield raise) — not implemented
4. `KINETIC_WEAPON` → `startUsingItem` plus a sound — not implemented

The order is load-bearing: an item that is both consumable and equippable eats
rather than equips.

**Eating ends on the server's own clock, not on a packet.** Vanilla's
`LivingEntity.updateUsingItem` counts `useItemRemaining` down and calls
`completeUsingItem` itself; the client sends nothing when a steak finishes. So
`dispatch_play_packet` carries an `item_in_use: &mut Option<ItemInUse>` beside
`bow_draw`, and `serve_play`'s per-tick arm is what lands the bite —
`finish_consuming`, which calls `FoodData::eat` and then `consume_one`.
`ReleaseUseItem` and `CarriedItemChanged` both clear the slot, which is
`Player.stopUsingItem`; `finish_consuming` also re-checks that the recorded item
is still in the recorded slot, because a container click can change it without
either packet.

`Equippable.swapWithEquipmentSlot`'s **count branch** is the part that is easy to
get wrong, and the one a single-helmet test cannot see:

| held count | equipment slot gets | hand gets | old piece goes to |
|---|---|---|---|
| `<= 1` | the whole held stack | the previously equipped piece (or keeps the held one in creative) | the hand |
| `> 1` | one | the rest | the **inventory**, or the floor if it does not fit |

## How to change it

* **A new support family**: add its vanilla base class to
  `scripts/derive-block-support.py`'s `BASE_KIND` and re-run it with `--rust`, then
  paste the rows in. Do not hand-add a block name and do not hand-transcribe the
  output — a hand-typed pass at this table lost 18 rows and invented 8, and only
  the invented half would have been caught by `block_support`'s census check. A
  class that inherits a `BASE_KIND` ancestor but has no `canSurvive` of its own
  belongs in `FORCE_NONE`, and every entry there was grepped first.
* **A new food**: one row in `item_use::FOODS`. The arithmetic is
  `crate::food::FoodData::eat`'s.
* **Gotcha — the sturdiness approximation.** `canSurvive`'s support test is
  `isFaceSturdy(UP, RIGID/CENTER)`, a geometric predicate over
  `getBlockSupportShape` that no census in this repo carries.
  `snow_support::face_full_up` is the *neighbouring* question (`isFaceFull` over
  the **collision** shape) and measurably not the same one: farmland reports
  `false` for all 8 of its states and soul sand for its only one, while crops and
  rails sit on both. So `block_support` asks only whether the support cell went to
  **air or a fluid** — a strict subset, failing in the safe direction. A torch on
  a fence is left alone rather than wrongly destroyed. Closing the gap means a new
  `isFaceSturdy` census in `lodestone-data`.
* **Gotcha — two blocks are supported by water.** `lily_pad` and `frogspawn` are
  excluded from the table for that reason; an "air or fluid" trigger would destroy
  every one of them on sight.
* **Gotcha — the creative fork on drops is not one fork but two.**
  `destroy_block`'s `drop_loot` is `!creative && block_drops`; the cascade's
  `cascade_drops` is `block_drops` **alone**, because vanilla's creative no-drop is
  `ServerPlayerGameMode.destroyBlock` choosing `removeBlock(pos, false)` for the
  block the player broke, while a self-destructing cell goes through
  `updateOrDestroy` and knows nothing about who caused it. A creative player mining
  the dirt under a flower does get the flower.
* **Gotcha — `state` is shadowed inside the placement branch.**
  `apply_use_item_on`'s `let (state, extra) = …` shadows the `&mut State`
  connection state, so nothing inside that block can call `apply`. The placement
  remainder is carried out in a local and sent after the block closes.

## Configuration

No new flags. The `block_drops` game rule (pre-26.2 `doTileDrops`) gates the
cascade's loot; game mode gates every consumption path
(`ItemStack.consume`'s `hasInfiniteMaterials`).

## Dependencies

* `lodestone_data::block_states` — the census the support table's names are
  checked against.
* `lodestone_data::item_prototypes` — `Equippable.slot()` and
  `allowedEntities.isEmpty()`, from a JVM dump. It does **not** carry `swappable`,
  which is why `item_use::UNSWAPPABLE` names the nine items whose registration
  sets it false.
* `crate::food::FoodData` — all hunger arithmetic; `item_use` supplies only the
  component values.
* `crate::block_drops` / `crate::loot` — the cascade's drops go through the
  existing path, never a new one.
* The decompiled 26.2 source under `.cache/mc/26.2/` — `Foods.java`,
  `Consumables.java`, `Items.java`, `Equippable.java`, and the block class tree.

## What is left

* **Entity-placing items are not done.** Boats (per-species plus `bamboo_raft`
  plus chest boats), minecarts, armour stands, buckets of fish and end crystals all
  still do nothing. `apply_use_item`'s ordered dispatch is the hook, but a boat is
  not a `SimMob` — it has no attributes and no goals, and it needs buoyancy and
  rideability — so this needs `crate::mobs`/`lodestone-entity` rather than a `use`
  arm. Spawn eggs are the one member of the family that works, through
  `apply_use_item_on`.
* **`Consumable.onConsume`'s effect lists**: a golden apple's regeneration, rotten
  flesh's hunger, chorus fruit's teleport, milk's `ClearAllStatusEffectsConsumeEffect`.
  Each needs `crate::mob_effects` wired to the completion callback this landing
  creates.
* **`usingConvertsTo`**: a stew should leave a bowl, honey a glass bottle.
* **Off-hand placement**: `ServerBound::UseItemOn` carries no `hand` field in this
  crate's decode, so `apply_use_item_on` reads the selected hotbar slot only. That
  is a protocol-side gap, not a `server.rs` one.
* **The `wasm32` `serve_play` loop has no per-tick timer**, so a browser session
  starts a bite and never lands it — the same shape as the drowning countdown that
  loop already documents.
* **The support cascade only runs from `destroy_block`.** A support removed by a
  piston, a fluid or an explosion does not trigger it; the reported case (a player
  breaking the block) does.
