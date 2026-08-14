# Container clicks

## What it is

The client-side predictor for what a mouse click on an open container screen
does — vanilla `AbstractContainerMenu.doClick`, reimplemented version-free.
It runs locally the instant the player clicks, so the screen updates before
the server round-trips a confirmation, and it is deliberately faithful to
vanilla's quirks rather than "corrected," because a corrected version would
predict a different result than the server computes and desync the display
for one round trip. Lives in `crates/lodestone-game/src/{click.rs, menu.rs,
menus.rs}`. Landed from a line-by-line click audit against 26.2's decompiled
`AbstractContainerMenu`.

**The server has its own, independent port of the same function** since the
container-diff trust was closed: `crates/lodestone-server/src/container_click.rs`,
documented in `docs/server-inventory.md`. Two ports of one Java method is
deliberate — this one is a *prediction* over a client-side `Menu`, that one is the
*authority* over the server's real slots, and they must agree without sharing a
crate (`lodestone-server` is client-free). When they disagree the server sends a
correcting `container_set_content` and the client reconciles, which is exactly the
round trip described above.

## How it works

### The seven `ContainerInput` modes

`ContainerInput` (`click.rs`) is vanilla's click mode, dispatched in
`Menu::do_click` (`click.rs`):

| mode | variant | button semantics |
| --- | --- | --- |
| 0 | `Pickup` | 0 = left (whole stack), 1 = right (half/one); slot `-999` drops the cursor |
| 1 | `QuickMove` | 0/1 = shift-click quick transfer |
| 2 | `Swap` | 0–8 = hotbar key, 40 = off-hand key |
| 3 | `Clone` | middle-click, creative only (`ctx.infinite_materials`) |
| 4 | `Throw` | 0 = drop one, 1 = drop stack |
| 5 | `QuickCraft` | the drag sequence below |
| 6 | `PickupAll` | double-click gather |

Ergonomic constructors for each, all in `click.rs`: `Click::left`/`right`,
`Click::shift`, `Click::hotbar_swap`/`offhand_swap`, `Click::clone_slot`,
`Click::drop_one`/`drop_stack`, `Click::double`.

### `QUICK_CRAFT`'s bit packing

The drag protocol packs a header and a type into one wire byte, in
`click.rs`'s `quick_craft_mask`/`quick_craft_header`/`quick_craft_type`:

```rust
fn quick_craft_mask(header: u8, kind: u8) -> u8 { (header & 3) | ((kind & 3) << 2) }
fn quick_craft_header(mask: u8) -> u8 { mask & 3 }
fn quick_craft_type(mask: u8) -> u8 { (mask >> 2) & 3 }
```

`drag_header` is `{ START = 0, ADD = 1, END = 2 }`, `drag_type` is
`{ EVEN = 0, ONE = 1, CLONE = 2 }` (`click.rs`). START and END always
carry slot `-999` (`OUTSIDE_SLOT`) — `perform_drag` (`click.rs`)
sends both through `do_click(OUTSIDE_SLOT, ...)`. **Only ADD carries a real
slot**: its handler is the only arm of `do_quick_craft` that reads
`slot_index` at all (`click.rs`). There is no fourth packet for
"still dragging, mouse moved but no new slot painted" — the protocol's three
button-encoded headers are exhaustive; nothing is sent on a bare press.

### The three-stage drag machine, and what actually resets it

Stages: **START** arms a drag of a given type → **ADD** records one painted
slot → **END** distributes the cursor across every recorded slot. Implemented
in `Menu::do_quick_craft` (`click.rs`). Three things reset it
(`reset_quick_craft`, clearing `quick_craft_status`/`quick_craft_slots` but
**not** `quick_craft_type` — the single-slot degrade path below reads it back
after a reset, `menu.rs`):

1. **A bad header sequence** — a header that doesn't advance the expected
   state (with one vanilla-tolerated shortcut, start→end) resets:

   ```rust
   // click.rs:493-498
   if (expected != drag_header::ADD || header != drag_header::END) && expected != header {
       self.reset_quick_craft();
       return;
   }
   ```

2. **An empty cursor**, checked on every call:

   ```rust
   // click.rs:499-502
   if self.carried().is_none() {
       self.reset_quick_craft();
       return;
   }
   ```

3. **An invalid type**, checked only at START — `EVEN`/`ONE` are always
   valid, `CLONE` needs `infinite_materials` (`is_valid_quick_craft_type`,
   `click.rs`); an unrecognized 2-bit header value also resets
   (`do_quick_craft`, `click.rs`).

**An invalid slot does *not* reset the drag.** ADD's handler filters it and
keeps going:

```rust
// click.rs:515-524
drag_header::ADD => {
    if let Ok(index) = usize::try_from(slot_index) {
        let carried = self.carried().cloned();
        if let Some(carried) = carried && self.can_drag_place(index, &carried) {
            self.push_quick_craft_slot(index);
        }
    }
}
```

No `else` arm calls `reset_quick_craft` — a slot holding a different item, or
one that fails `can_drag_place`, is silently skipped and the drag stays
armed for the next ADD or END. `drag_skips_a_slot_holding_a_different_item`
and `drag_never_paints_the_result_slot` (`menu.rs`) pin this.

Any ordinary (non-`QuickCraft`) click received while a drag is armed aborts
it *and* is itself swallowed (`do_click`, `click.rs`,
`ordinary_click_mid_drag_resets_and_is_itself_swallowed`, `menu.rs`).
A single painted slot degrades to an ordinary pickup/place click rather than
a distribute (`finish_quick_craft`, `click.rs`) — this is where the
kept `quick_craft_type` matters.

### Per-menu quick-move (shift-click) orders

Dispatched by `(kind, craft)` in `quick_move` (`menu.rs`):

- **Generic container** (chests, barrels, ender chests, every
  `generic_9xN`, hoppers, dispensers, droppers, shulker boxes —
  `quick_move_generic`, `menu.rs`): container → player goes
  **backwards** (hotbar first); player → container goes forwards. Mirrors
  `ChestMenu.quickMoveStack`, and per the same comment also
  `HopperMenu`/`DispenserMenu`/`ShulkerBoxMenu`.
- **Crafting table** (`quick_move_crafting`, `menu.rs`, mirroring
  `CraftingMenu.quickMoveStack`): result slot → player inventory backwards;
  a grid cell → player inventory forwards; a player-inventory item first
  tries to *load the grid* (`first_input..grid_end`), only falling back to
  the main↔hotbar hop if the grid refuses it. "Shift-clicking planks in a
  crafting table loads the grid" (`crafting_table_shift_click_loads_the_grid_first`, `menu.rs`).
- **Player inventory screen** (`quick_move_player`, `menu.rs`,
  mirroring `InventoryMenu.quickMoveStack`), in order:

  | # | source | destination |
  | --- | --- | --- |
  | 1 | slot 0 (result) | `9..45` backwards |
  | 2 | `1..5` (craft grid) | `9..45` forwards |
  | 3 | `5..9` (armour) | `9..45` forwards |
  | 4 | armour item, matching slot empty | that one armour slot |
  | 5 | off-hand item, slot 45 empty | slot 45 |
  | 6 | `9..36` (main storage) | `36..45` (hotbar) |
  | 7 | `36..45` (hotbar) | `9..36` (main storage) |
  | 8 | anything else (i.e. slot 45) | `9..45` forwards |

  Auto-equip (4, 5) has to come before the main/hotbar hop, and has to be
  reachable from *every* source ≥ 9 including slot 45 — a prior bug let
  off-hand fall through to rule 8 instead of auto-equipping
  (`quick_move_player`, `menu.rs`, regression test
  `shift_click_equips_armour_out_of_the_offhand_slot`, `menu.rs`).

### Two menus left deliberately generic

`AbstractFurnaceMenu` and `BrewingStandMenu` route shift-clicks *by item
kind* rather than by region — smeltables/fuel, or blaze
powder/ingredients/potions, to specific slots before falling back to the
main↔hotbar hop. Neither is modelled here: both predicates need data this
tree doesn't have — `canSmelt` needs the cooking-recipe input set, `isFuel`
needs the fuel-value registry — and guessing would just move the wrongness
rather than remove it (`quick_move_generic`, `menu.rs`, `build_menu`'s doc
comment, `menus.rs`). A furnace
therefore predicts a shift-click into container slot 0 where vanilla might
have picked slot 1 or done nothing, and the server corrects it one round
trip later — "a visible flicker, not a desync" (`build_menu`, `menus.rs`). Routing
for these, if it lands, must be carried as a `Menu`-level descriptor (like
`CraftLayout`), not a new `MenuKind` variant — `MenuKind` is matched
exhaustively in `lodestone-shell`'s `slot_layout` (`build_menu`, `menus.rs`).

### Prediction versus authority

`click.rs`'s module doc states the model plainly: this is "an original,
version-free predictor over a `Menu`. The client runs exactly this locally
to predict the result of a click before the server confirms it." `Menus`
(`crates/lodestone-game/src/menus.rs`) is the seam: `Menus::apply` routes
incoming server events into `ClientMenu`'s predict/reconcile machinery or
into menu-lifecycle bookkeeping, and `Menus::click` applies a click
optimistically, then returns the `ClickIntent` that goes on the wire — the
server's own reply, when it arrives, overwrites the prediction through
`apply` (`menus.rs`).

The clearest place prediction knowingly stops short of the truth is crafting
results. A client's `CraftingMenu` is built with no level access, so it
can't recompute the recipe after a take and its predicted loop exits after
exactly one craft — which is exactly what vanilla's own client predicts too.
The server crafts until the grid runs dry and reconciles the difference back
as `container_set_slot`s (`do_quick_move`, `click.rs`). `predicted_craft_result`
(`menus.rs`) is explicit about the boundary: "This is a prediction,
not the truth… read the result slot for what the player is actually holding
a claim to." `Menu::restore` is likewise labelled "server-authoritative
resync" (`menu.rs`).

### One inventory, one owner

Vanilla has exactly **one** `Inventory`. `InventoryMenu`'s slots and every
`AbstractContainerMenu`'s player-section slots are all `Slot(inventory, i, x, y)`
— *references into it*. A crafting table's slots `10..46` **are** the player
inventory's slots, so a shift-click that lands in the hotbar mutates the one
container and the HUD reads the change for free.

`Menus` used to hold two `ClientMenu`s that each owned a full 41-slot player
`Container`: `player` (window 0, what `Sim::player_menu` → `app.rs`'s
`hotbar_records` reads) and `opened.menu` (what the container screen draws and
what every click mutates). Measured symptom: a quick-move into the hotbar updated
the container's copy and left window 0 untouched, so the item was **usable and
not drawn** — the server had it, only our HUD copy was stale — and it stayed
stale after the screen closed, because a vanilla server sends nothing on close
(`ServerPlayer.doCloseContainer` only calls `transferState`).

Rust will not lend one `Container` to two owned `Menu`s, so the aliasing is
modelled as **ownership that moves**, which is a stronger property than a sync:

| event | what happens to the storage |
| --- | --- |
| nothing open | `Menus::player` owns it |
| `container_set_content` builds a new open menu | `hand_inventory_to_opened` moves it into that menu, **before** the content packet is reconciled into it |
| `ScreenClosed`, or a different window replacing this one | `reclaim_inventory` moves it back |

At no instant do two copies of the player's 41 native slots exist inside a
`Menus`, so there is nothing to synchronise and nothing that can diverge. The
primitives are `Menu::{take,install}_player_inventory` and the `PlayerInventory`
newtype in `reconcile.rs` (a `ClientMenu` holds *two* `Menu`s by design —
predicted and confirmed — so "the one inventory" is a pair at that level; those
two are one window's two points in time and `reconcile` is what collapses them,
which is a different thing from two windows each owning a copy).

Three consequences, each of which is a trap if you do not know it:

* **`Menus::player()` returns a `Menu` by value, not `&Menu`.** While a container
  is open, `self.player`'s player section is an empty **husk**; `player()` clones
  the window-0 menu and reinstalls the live inventory over it, so no caller can
  obtain a stale — or blank — hotbar. Do not "optimise" it back to a borrow.
  `Menus::player_native(native)` is the borrow-friendly accessor for reading one
  stack (used by the mining-speed tool lookup in `lodestone-shell`'s `interact.rs`,
  which had the same stale read).
* **A window-0 `container_set_slot`/`container_set_content` has to be forwarded.**
  Vanilla routes container id `0` to `player.inventoryMenu`, whose slots reference
  the shared `Inventory`, so a window-0 update reaches an open chest's rows for
  free. Here `forward_window_zero_slot` re-addresses it, and it derives the
  native index from `Menu::slot_native` — the same `Slot` table the draw walks —
  rather than from a second transcription of the window-0 layout. Menu slots
  `0..5` (window 0's own 2×2 grid and result) have no native index and are left
  where they are.
* **The handoff moves all 41 natives, not the 36 the packet mentions.** A chest's
  `container_set_content` carries main + hotbar only; armour (`36..40`) and the
  off-hand (`40`) are in no chest packet at all, so moving only the packet's
  portion would wipe the player's armour from the HUD on every chest open.

`crates/lodestone-game/tests/inventory_is_shared.rs` pins all of it, driven
through `ScreenOpened` + `ContainerContent` + `Menus::click` — never by writing a
slot. Its control is the full pre-fix reproduction (handoff, reclaim, forward and
`player()`'s reinstall all disabled at once), **watched failing** at 5 of 10:
both quick-move-to-hotbar tests, the after-close test, the window-0-while-open
test and the native-update test, with both `control_*` tests and the two
"does not over-reach" tests staying green. Disabling only *part* of the fix was
not enough — with the handoff off but `player()`'s reinstall still on, 8 of 10
still passed, because reading through the owner alone repairs the read side.
That is worth remembering before trusting a partial break as a control.

### Armour equips — the prototype census landed in `67ff7c3`

This section originally recorded armour as unequippable because
`minecraft:equippable` and `minecraft:max_stack_size` were prototype item
components — vanilla stores both in the item's *built-in* component map, not
in the clientbound patch, so no wire decoder could produce them without a
version-owned census — and it flagged itself as likely to go stale within the
hour, in the same session, once that census landed. It has: `67ff7c3` added
`crates/lodestone-data/src/item_prototypes.rs` (an item→`{max_stack_size,
max_damage, equip_slot}` table, the shape `generated/tools.rs` set the
precedent for) and wired it into wire decoding at
`read_component_patch` (`crates/protocol/v770/src/adapter/inventory.rs`): the
effective `max_stack_size`/`max_damage`/`equippable` fields are seeded from
the prototype *before* the patch is read, and a patch that does mention one
overrides the seeded value. `crate::container::equippable_slot` reads
`stack.components().get_str("minecraft:equippable")`, so a stack decoded off
the real wire now resolves it, `Slot::may_place` accepts it into the matching
`Armor` slot, and `Menu::empty_equip_target` (`menu.rs`) finds it. No
click-mode change was needed — the routing in the table above was always
right; it had nothing to route.

The `"body"` → `Chest` fold this section used to describe is gone, and
deliberately: `EquipmentSlot::from_name` (`crates/lodestone-game/src/
container.rs`) leaves `"body"` unmapped, because vanilla gates humanoid
armour on `EquipmentSlot.Type.HUMANOID_ARMOR`,
which **excludes** `BODY` — `wolf_armor` and the four `*_horse_armor` items
are real `body`-slot equipment, so folding `"body"` into `Chest` would have
put animal armour in a player's chestplate slot the moment the census made
`"body"` reachable.

**One loose end this landing left behind:** `canary_wire_stacks_carry_
no_prototype_components` (`menu.rs`) is still in the tree, still
green, and its own comment ("if this now resolves, the equippable census
landed") is no longer an accurate description of what it tests. It builds its
"wire stack" as `lodestone_model::ItemStack { …, components:
lodestone_model::ItemComponents::default() }` directly — bypassing
`read_component_patch` entirely — so `equippable_slot` on it returns `None`
regardless of whether the census exists; the test cannot observe the fix it
was written to detect. It was not touched here (this pass is docs-only), but
the census landing is exactly the trigger its own comment named for deleting
it and re-pointing the armour/stack-cap tests at a stack built through the
real adapter, e.g. via a decoded `read_component_patch` call rather than a
hand-built `ItemComponents::default()`.

### Three quirks that are transcribed deliberately, because they read as bugs

`Menu::move_item_stack_to` (`menu.rs`) is the port of vanilla
`AbstractContainerMenu.moveItemStackTo`. Its doc comment
calls out three details explicitly kept rather than "fixed" (`menu.rs`):

1. **The merge pass never calls `may_place`.** Only the empty-slot pass does
   (`slot.mayPlace`, in `AbstractContainerMenu.moveItemStackTo`, vs. no
   check on the merge pass's equivalent). A shift-click can top up an
   existing stack in a slot that would refuse the same item arriving into
   an empty cell.
2. **The merge pass is gated on `is_stackable`, not on the per-slot cap.**
   An unstackable item skips merging entirely and goes straight to the
   first empty slot (`AbstractContainerMenu.moveItemStackTo`).
3. **The two passes measure their cap against different stacks.** The merge
   cap is `effective_max(slot, &target)` — the slot's *existing* stack;
   the empty-slot cap is `effective_max(slot, moving)` — the
   *incoming* stack. They agree whenever it's the same item, which
   the merge pass has already established by the time it runs — so it's
   only a difference in what the code says, not what it does, but it's what
   the source says, so it's transcribed as-is.

The empty-slot pass also stops after exactly one placement (`break` in
`AbstractContainerMenu.moveItemStackTo`), which is why a caller moving more than
one stack's worth has to loop.

## Audit against 26.2's `doClick`

A line-by-line walk of `AbstractContainerMenu.doClick`
against every branch in `click.rs`/`menu.rs`, done after the predictor above had
already landed. This was an audit, not a rewrite — the table below is the
record of what was checked, what was found undertested, and the one real bug
this pass found and fixed.

**How "covered" was established.** For every row marked "verified red," the
implementation was locally broken (a wrong constant, a removed guard, a
reordered call), the cited test was run and observed to fail, then the break
was reverted — satisfying `CLAUDE.md`'s "assertions of an absence need a
control proving the detector works," applied to test coverage itself rather
than to in-game state. Rows marked "pre-existing, suite discipline" were not
individually re-broken this pass (budget); confidence there instead comes
from (a) this module's tests already citing exact vanilla line numbers and
pairing every negative assertion with a positive control, per its own stated
evidence standard (the `tests` module's own doc comment, `menu.rs`), and (b) one representative spot-check
— breaking `quick_craft_place_count`'s `ONE` case from `1` to `2` — which
`right_drag_one_each` caught immediately, corroborating that the discipline
described in the comments is real and not just asserted.

| wire `ClickType` (mode / button) | 26.2 location | our location | test(s) | verified red? |
| --- | --- | --- | --- | --- |
| `PICKUP` left (`0`/`0`) | `AbstractContainerMenu.doClick` | `click.rs::do_pickup` | `left_click_places_whole_cursor_into_empty_slot`, `left_click_full_slot_empty_cursor_picks_up_whole`, `left_click_same_item_merges_up_to_cap`, `left_click_different_items_swaps_cursor_and_slot`, `placing_into_smaller_max_stack_respects_item_cap` | pre-existing, suite discipline |
| `PICKUP` right (`0`/`1`) | same | `do_pickup` | `right_click_places_one`, `right_click_full_slot_empty_cursor_takes_half_rounding_up` | pre-existing, suite discipline |
| `PICKUP`/`QUICK_MOVE` outside (slot `-999`) | `AbstractContainerMenu.doClick` | `do_drop_cursor` | `drop_cursor_outside_left_drops_all`, `drop_cursor_outside_right_drops_one` | pre-existing, suite discipline |
| `QUICK_MOVE` (`1`) + per-menu order | `AbstractContainerMenu.doClick`, `ChestMenu.quickMoveStack`, `CraftingMenu.quickMoveStack`, `InventoryMenu.quickMoveStack` | `do_quick_move`, `quick_move_generic`/`_crafting`/`_player` | `shift_click_from_hotbar_to_main_in_player_menu`, `shift_click_from_container_to_player_inventory`, `chest_to_player_fills_the_hotbar_first`, `crafting_table_shift_click_loads_the_grid_first`, `player_screen_shift_click_never_loads_the_two_by_two_grid`, `shift_click_equips_armour_before_trying_the_hotbar`/`_out_of_the_offhand_slot`, `shift_clicking_the_result_crafts_once_locally_and_again_on_server_refill` | pre-existing, suite discipline |
| `SWAP` hotbar keys (`2`/`0-8`) | `AbstractContainerMenu.doClick` | `do_swap` | `hotbar_swap_moves_between_slot_and_hotbar`, `hotbar_swap_exchanges_two_stacks`, `number_key_swapping_the_result_out_also_crafts` | pre-existing, suite discipline |
| `SWAP` off-hand key (`2`/`40`) | same, guard `buttonNum == 40` | `do_swap` | `offhand_swap_moves_between_slot_and_offhand`, `offhand_swap_exchanges_two_stacks` (new) | **yes** — removing the `\|\| button == 40` arm turned both red |
| `SWAP` overflow (`source.count() > cap`) | `AbstractContainerMenu.doClick` | `do_swap` overflow branch + `give_to_player` | `hotbar_swap_overflow_merges_into_the_remainder_it_left_behind`, `control_hotbar_swap_without_overflow_is_a_plain_exchange` (new) | **yes** — see Finding 1, a real bug this test caught and a fix landed for |
| `CLONE` (`3`) | `AbstractContainerMenu.doClick` | `do_clone` | `middle_click_clone_creative_fills_cursor_full_stack`, `middle_click_clone_noop_in_survival`, `middle_click_clone_refuses_when_cursor_is_occupied` (new) | new test not independently re-broken (one-line guard, low risk); other two pre-existing, suite discipline |
| `THROW` drop-one (`4`/`0`) | `AbstractContainerMenu.doClick` | `do_throw` | `throw_q_drops_one_from_slot`, `dropping_the_result_with_q_also_crafts` | pre-existing, suite discipline |
| `THROW` drop-stack (`4`/`1`) | `AbstractContainerMenu.doClick`, incl. the repeat-while-same-item loop | `do_throw` | `throw_ctrl_q_drops_whole_slot` | pre-existing, suite discipline; the missing loop is Finding 2 (proven inert, not fixed) |
| `THROW` `canDropItems` gate | `AbstractContainerMenu.doClick` | `do_throw`'s `ctx.can_drop` check | `throw_is_a_noop_when_the_player_cannot_drop_items` (new) | **yes** — removing the check turned it red |
| `QUICK_CRAFT` start/add/end, `EVEN`/`ONE`/`CLONE` (`5`) | `AbstractContainerMenu.doClick` | `do_quick_craft`, `finish_quick_craft` | `bare_drag_end_without_start_commits_nothing` / `control_well_formed_drag_does_commit`, `ordinary_click_mid_drag_resets_and_is_itself_swallowed` / `control_same_click_applies_when_no_drag_is_armed`, `drag_with_empty_cursor_commits_nothing`, `paint_stops_when_the_cursor_runs_out_of_items`, `even_split_clamps_at_the_slot_cap_and_returns_the_remainder`, `repainting_a_slot_does_not_inflate_the_divisor`, `clone_drag_resets_in_survival` / `control_clone_drag_commits_in_creative`, `drag_skips_a_slot_holding_a_different_item`, `drag_never_paints_the_result_slot`, `left_drag_even_split_across_three_slots`, `left_drag_even_split_leaves_remainder_on_cursor`, `right_drag_one_each`, `single_slot_drag_degrades_to_place`, `creative_middle_drag_places_full_stacks` | pre-existing, suite discipline — and the one representative spot-check (`right_drag_one_each` against a broken `ONE` place-count) was run this pass and did go red |
| `PICKUP_ALL` (`6`) | `AbstractContainerMenu.doClick` | `do_pickup_all` | `double_click_gathers_matching_partial_stacks_first`, `pickup_all_defers_a_maxed_slot_to_the_second_pass` / `control_pickup_all_takes_a_near_max_slot_in_the_first_pass`, `pickup_all_never_drains_the_crafting_result` | pre-existing, suite discipline |

### What this table is *scoped to*, and the bug that hid behind that scope

**The table above audits `doClick` and nothing else.** Every row cites a range
of `AbstractContainerMenu.java` against a function in
`crates/lodestone-game/src/`. That is the *machine* — the thing that interprets
a click packet once it exists. It says nothing about the layer that decides
**which packet is sent**, which is `MenuInput` in
`crates/lodestone-shell/src/container.rs`, a reimplementation of
`AbstractContainerScreen`'s press/drag/release protocol.

A player report said that taking from a crafting output onto a cursor
already holding the same item did nothing. The suspicion was that the audit had
missed `PICKUP`'s cursor-merge arm. It had not: the arm is in `do_pickup`
(`click.rs`), mirrors `AbstractContainerMenu.doClick` line for line,
and driving a bare `Click::left(result)` into the machine merges correctly.
**The `PICKUP` row is right.**

What was wrong was that no `PICKUP` was ever sent. `MenuInput::dragged`
recorded *every* slot the pointer crossed with the button down, where vanilla's
`mouseDragged` gates the paint on `shouldAddSlotToQuickCraft`
(`AbstractContainerScreen.shouldAddSlotToQuickCraft`), whose `slot.mayPlace(carried)` arm a
result slot always fails. So a click that jiggled the mouse by one pixel over
the output painted it, and the *emptiness of the painted set is what selects the
packet*:

| painted set at release | packet sent |
| --- | --- |
| non-empty | `QUICK_CRAFT` start / add… / end |
| empty | plain `PICKUP` (`AbstractContainerScreen.mouseReleased`'s `else if (!carried.isEmpty())`) |

The machine then dropped the `ADD` at its own `can_drag_place` — correctly, the
two gates agree — and `finish_quick_craft` saw an empty painted set at `END` and
returned. Result: a well-formed packet sequence that provably commits nothing,
with the merge arm sitting one layer below, fully working, never reached.

The old code's justification was explicit and half-true: filtering is "left to
`Menu::do_click`'s own `can_drag_place`, which both sides run — an `ADD` the
server rejects is simply not recorded there, so painting liberally cannot
desynchronise." Painting liberally indeed cannot desynchronise. It can still
**suppress a different click entirely**, and that is not a desync so nothing in
the desync argument could see it.

The lesson for this table: a row marked covered means *the machine's branch is
covered*. An end-to-end symptom needs the shell's protocol audited too, and
there is no equivalent table for it. The gates that now cover this arm are in
`container.rs`'s own test module (`dragging_across_a_crafting_result_sends_a_pickup_not_a_dead_drag`,
`the_resulting_pickup_merges_the_result_onto_the_matching_cursor`, plus
`control_dragging_across_a_placeable_cell_still_paints_it` so a `dragged` that
refuses everything cannot pass), **verified red**: with only the `may_place`
arm removed, both result-slot tests fail and the cursor reads
`left: Some(4), right: Some(5)` — the reported symptom exactly — while the
identity/count test stays green, so each arm of the gate is separately
discriminated rather than passing as a lump.

### Finding 1 (fixed): swap-overflow displaced the wrong stack into a fresh slot instead of merging it back

Number-key-swapping a stack bigger than the target slot's cap (`click.rs`'s
`do_swap` overflow branch, mirroring `AbstractContainerMenu.doClick`)
splits the overflow into the target slot and has to put the target's old
contents *somewhere* — `inventory.add(targetItemStack)` in vanilla,
`give_to_player` here.

The subtlety is aliasing. Vanilla's `source` is the literal object backing
`inventory.getItem(buttonNum)` (`Inventory.getItem`,
returns the live list element) and `ItemStack.split`
mutates it in place via `shrink`. So by the time `inventory.add` runs, the
native slot the swap came from **already shows its reduced remainder**, and a
same-item displaced stack naturally merges back into it. Our `ItemStack` isn't
aliased — before this fix, `give_to_player` was called *before*
`set_player_native` wrote the remainder back, so it scanned a stale,
pre-split native container. A reproduction
(egg with `max_stack_size` overridden to 16, 20 in the hotbar swapped onto 5
in a container slot) showed the displaced 5 landing in a **new, previously
empty** native slot instead of merging into the 4 left behind — confirmed by
running the test with the old call order and watching it fail
(`left: Some(4), right: Some(9)`), then again after reordering the two calls.
Fixed in `click.rs`'s `do_swap`: `set_player_native` now runs before
`give_to_player`. Regression test:
`hotbar_swap_overflow_merges_into_the_remainder_it_left_behind`
(`menu.rs`), with `control_hotbar_swap_without_overflow_is_a_plain_exchange`
as the non-overflow control.

**Fixed separately:** `give_to_player`'s own scan order (it used to
be a plain linear `0..36`, merge pass then first-empty pass) now models
vanilla's actual `Inventory.add` → `addResource` → `Inventory.getSlotWithRemainingSpace`
priority — the *selected* hotbar slot first, then
the off-hand at native 40, then a merge-only linear scan across
natives `0..36`, and only once none of those already hold a
mergeable stack does it fall back to `Inventory.getFreeSlot`, the first **empty** slot
in `0..36` — the off-hand is *never* used as an
empty-slot fallback, because `getFreeSlot` scans only the `items` field, which
vanilla sizes at exactly 36, independently of the
off-hand's separate `EquipmentSlot`-backed storage. `click.rs`'s
`give_to_player`/`mergeable_native` carry the ordering; `PlayerCtx` grew a
`selected_hotbar_slot` field to carry it in (default `0`, matching every
production caller today — `app.rs::send_menu_click` still hardcodes
`PlayerCtx::survival()` for the same reason it hardcodes game mode, so a real
selected-slot value is not yet threaded from live player state into a click).
Regression: `swap_overflow_gives_to_the_selected_hotbar_slot_before_a_lower_index`
(`menu.rs`), with `control_swap_overflow_without_a_selected_slot_still_scans_from_zero`
as the no-preference control — selected slot is `4`, not `0`, specifically so
a scan that merely "starts from 0" cannot pass by accident. Watched failing
pre-fix (`native 0` landed at `64`, `native 4` at `2` — the old in-order-scan
result) by temporarily disabling just the priority lookup and restoring it
after.

### Finding 2 (investigated, not a bug): `THROW`'s missing repeat-while-same-item loop

Vanilla's `THROW` branch loops for drop-stack (`buttonNum == 1`, in
`AbstractContainerMenu.doClick`): after the first `safeTake`, if the slot still holds the same
item it takes again, and again, until it doesn't. This is the same idiom as
`QUICK_MOVE`'s repeat loop, and exists for the same reason — a **server**-side
result slot recomputes and refills between takes.

`do_throw` here has no such loop. It is provably inert rather than missing:
`try_remove`'s `max_take` is always `i32::MAX` for a full-stack take, so it
always empties the slot completely in one call, and nothing in this crate's
`Menu` ever refills a slot mid-`do_click` — `on_take` (the only refill-shaped
hook) only decrements grid *input* cells, it never rewrites the *result*
slot. So the loop's condition (`ItemStack.isSameItem(slot.getItem(),
itemStack)`) can never be true after our single take; adding the loop would
be dead code today. This matches the documented model elsewhere in this file
("a client's `CraftingMenu`... predicted loop exits after exactly one
craft") — it is the same boundary, not a new one. Left as-is; noted here so
a future refill mechanism doesn't quietly reintroduce the gap without
someone remembering `THROW` needs the loop too.

> **That refill mechanism now exists — on the server side only.**
> `lodestone-server`'s `container_click::do_click_with` takes the menu's recipe
> corpus and re-derives the result slot mid-click, so its `QUICK_MOVE` loop really
> does craft repeatedly (see
> [`server-side-crafting.md`](./server-side-crafting.md)). Everything above still
> holds for `lodestone-game`: this crate's `Menu` has no corpus at click time, its
> predicted loop still exits after one craft, and the server's extra crafts arrive
> as an ordinary correction. `do_throw`'s missing loop stays inert *here*.
>
> **Correction (2026-08-14, while auditing issue #266): the server's own `THROW`
> arm had this exact gap until now.** The claim above that the server side "really
> does craft repeatedly" was verified for `QUICK_MOVE` but not for `THROW` —
> `do_click_with`'s `THROW` arm called `take_from` exactly once regardless of
> `click.button`, so a ctrl-Q on a crafting/smithing/anvil/grindstone result
> dropped one craft and silently discarded the rest of the consumed grid. Fixed in
> `container_click.rs` (a real repeat loop, gated on `same_item` — vanilla's
> `ItemStack.isSameItem`, item type only — matching `QUICK_MOVE`'s own loop, which
> was separately found using the *stricter* `isSameItemSameComponents` check and
> corrected to match). So the reasoning in this note was right about the client
> and incomplete about the server; both are now consistent with vanilla.

### Not a gap: `PICKUP_ALL`'s reverse-scan direction

`doClick`'s `PICKUP_ALL` branch supports `buttonNum != 0` (scan backwards from
the last slot). The real 26.2 client never sends it: double-click gather is
gated on `event.button() == 0` at the call site
(`AbstractContainerScreen.mouseClicked`), so `button` is always `0` on the
wire. `Click::double`'s hardcoded button-`0` is a correct match to what the
protocol actually carries, not a missing verb — there is nothing to test
because there is nothing real to distinguish it from.

### Stale doc pointer fixed by this pass

The **Tests** section below used to say every test lives in
`crates/lodestone-game/src/{click.rs,menu.rs}`'s own `#[cfg(test)]` modules.
That stopped being true at some point after it was written: `click.rs` has
zero tests of its own today — the positive-case suite is in
`crates/lodestone-game/tests/click_machine.rs` (and crafting-specific cases in
`tests/crafting_menu.rs`), while `menu.rs`'s `mod tests` holds the
negative-control suite this doc's own header describes. Corrected below.

## The shell layer's own verb table

The `doClick` table above audits the **machine**. This section is the audit the
table's own postscript said did not exist: `AbstractContainerScreen`'s
press/drag/release/**keyPressed** protocol, against
`crates/lodestone-shell/src/container.rs`'s `MenuInput`. It is a separate layer
and it has its own defect class — the machine can be perfect while no packet is
ever sent.

| vanilla entry point | 26.2 location | our location | state |
| --- | --- | --- | --- |
| `mouseClicked` | `AbstractContainerScreen.mouseClicked` | `MenuInput::press` | covered |
| `mouseDragged` + `shouldAddSlotToQuickCraft` | `AbstractContainerScreen.mouseDragged`, `AbstractContainerScreen.shouldAddSlotToQuickCraft` | `MenuInput::dragged` | covered |
| `mouseReleased` incl. `quickCraftToSlots` | `AbstractContainerScreen.mouseReleased`, `AbstractContainerScreen.quickCraftToSlots` | `MenuInput::release` | covered |
| `checkHotbarKeyPressed` (`SWAP`, hotbar + off-hand) | `AbstractContainerScreen.checkHotbarKeyPressed` | `app.rs`'s `KeyOutcome::ContainerSwap` | covered (`43692c5`, `1585e69`) |
| **`keyPressed`'s `THROW`/`CLONE`** | **`AbstractContainerScreen.keyPressed`** | **`MenuInput::key_pressed`** | **producer added; needs one `app.rs` binding** |
| `checkHotbarMouseClicked` | `AbstractContainerScreen.checkHotbarMouseClicked` | — | **not modelled** (see below) |
| `mouseScrolled` (`ItemSlotMouseAction`) | `AbstractContainerScreen.mouseScrolled` | — | not modelled; bundles only |

### `Q` inside an inventory did nothing, and it was an island in both directions

The most expensive finding of this pass, and it is invisible to every test that
exists. `Click::drop_one`/`drop_stack`, `do_throw` and its `can_drop` gate were
all added and tested by the audit above — the `THROW` rows say "covered", and
they are, *as machine branches*. They had **zero producers anywhere outside
`crates/protocol/`**, which is `ClientAction::SetFlying`'s shape exactly.
`ContainerInput::Throw` was reachable only at `OUTSIDE_SLOT`, and `doClick`'s own
`slotIndex >= 0` guard (`AbstractContainerMenu.doClick`) drops that — so the
whole THROW-from-a-slot branch could not execute in the real game, at all.

`MenuInput::key_pressed` is the missing producer. Three details are transcribed
rather than reasoned about, because each reads as a bug:

* **The gate is `hoveredSlot.hasItem()`, not an empty cursor.** Unlike
  `AbstractContainerScreen.checkHotbarKeyPressed`, this branch never consults the carried stack;
  `doClick` does. Copying that guard one method too far withholds a packet
  vanilla sends, which is a desync in the direction nothing corrects — the server
  simply never sees it. Control: adding a `cursor_loaded` guard turned
  `the_drop_key_needs_an_item_in_the_slot_but_not_an_empty_cursor` red at
  `left: [], right: [Click { slot: 9, button: 0, input: Throw }]`.
* **`PickItem` is not gated on infinite materials**, where `press`'s middle-click
  equivalent is. Not an inconsistency: `AbstractContainerScreen.mouseClicked` uses
  `hasInfiniteMaterials` to decide *which mouse button means clone*, while the
  permission lives in `AbstractContainerMenu.doClick`'s CLONE arm. A key has no such
  ambiguity, so vanilla sends it in survival and the menu drops it.
* **`else if`, not two `if`s** — a key bound to both actions clones only.

**The second direction: there is no `InputAction::Drop` in `keybinds.rs` at all.**
`key.drop` is on that module's deliberate "absent rather than listed and dead"
list, so `key_pressed` is not yet called and this is not finished. The binding is
worth more than it looks, because `ClientAction::DropSelectedItem` /
`DropSelectedItemStack` are a **second** island behind the same key — encoded,
round-trip tested, exercised by an `#[ignore]`d live gate, and produced by
nothing. One `Q` binding closes both, which is why this is not the "half a
feature" that `43692c5` correctly refused for the off-hand key.

### The two `Q` paths disagreed about prediction, and only one of them was wrong

Once the binding above landed, `Q` reached the wire from both sides — and the two
sides did **not** behave the same. A player found it: *"throwing out items with Q
doesn't update the count in my inventory or hotbar, but it does work properly
otherwise."*

| screen open? | shell entry point | lowers to | predicted? |
| --- | --- | --- | --- |
| yes | `App::send_container_drop` | `ContainerMenuKey::Drop` → `Click::drop_one`/`drop_stack` → `ContainerInput::Throw` → `Menu::do_throw`, through `Menus::click` → `ClientMenu::predict` | **yes**, always did |
| no | `App::send_drop_selected` | `ClientAction::DropSelectedItem`/`DropSelectedItemStack`, straight to `NetClient::send_action` | **no** — this was the bug |

Measured, not read off the call sites:
`drop_selected_prediction::the_container_screen_drop_key_predicts_too` drives the
container path and reads `4` from five cobblestone, and it stays green when the
gameplay port is neutered — the two paths are genuinely independent.

**Why the gameplay path could not be "late" rather than wrong.** `THROW` is a
container click, so the server answers it with a state id and, on a mismatch,
`broadcastFullState()` — a missed prediction there self-heals within a round trip.
`DROP_ITEM`/`DROP_ALL_ITEMS` are not container clicks at all. They reach
`ServerGamePacketListenerImpl.handlePlayerAction`, which calls `this.player.drop(false)`
/ `drop(true)` and **returns without sending any slot or content packet**. So the
count stayed stale *forever*, and the local mutation is the only thing that will
ever change it. Vanilla's client has always done it: `LocalPlayer.drop`
calls `getInventory().removeFromSelected(all)` into a variable it literally names
`prediction`, and *then* sends the packet.

**Where the fix lives, and why not at the call site.** The prediction is
`Menus::drop_selected` → `ClientMenu::remove_from_selected` →
`Menu::remove_from_selected`, a port of `Inventory.removeFromSelected`
(→ `Inventory.removeItem` →
`ContainerHelper.removeItem` → `ItemStack.split`).
Three things about that chain are load-bearing:

* **It routes through `inventory_owner_mut`**, for *One inventory, one owner*'s
  reason above. A drop while a container screen is open must mutate the
  container's menu, because window 0's player section is an empty husk then.
* **It writes `predicted` *and* `confirmed`.** This is the one place that is
  correct, and it is the opposite of what `predict` does. A container click leaves
  `confirmed` alone because the server echoes and `reconcile` decides; a drop gets
  no echo, and the server has already performed the identical removal. Leaving
  `confirmed` one item richer would make the next full `container_set_content`
  diff as a *visible correction that never happened*.
  `a_container_click_moves_only_the_prediction` is the control for that asymmetry.
* **A zero remainder becomes `None`, not `Some(count: 0)`.** `app/redraw.rs` maps
  any present stack to a `HotbarSlot` unconditionally and `draw_item_icon_counted`
  (`hud/item_icon.rs`) draws the number only `if slot.count > 1`, so a surviving zero-count stack draws
  an icon with no number in a slot the player just emptied.

The serverbound half stays at the call site: `NetClient::predict_drop_selected` is
prediction *only*, called immediately before `send_action` (vanilla's order), and
inside `drop_selected_action`'s `Some` arm so the **spectator** gate is not
duplicated — a spectator predicts nothing and sends nothing, decided once.

#### A limit of the pixel gate, worth knowing before extending it

`hotbar_drop_prediction_pixels.rs` compares `HudGeometry::build`'s colour stream,
which needs no GPU. With no item atlas attached, `item_verts` is empty, so a
**one**-item cell and an **empty** cell produce byte-identical streams — the stack
digits are suppressed at `count == 1` and the icon art is the only other
slot-dependent ink. Any assertion that has to separate "emptied" from "one left"
must therefore read `hotbar_records()[SELECTED]` (which is
`HudFrame::hotbar_items`' own element type, the value `draw_hotbar_items` forks on)
rather than the stream. Two smaller measured facts from the same file: `"5"` and
`"4"` do *not* cost the same number of vertices — the procedural glyph path is
stroke-based and a `5` is six quads more (101,016 floats against 100,800) — and a
count-1 frame is strictly shorter than either.

### `checkHotbarMouseClicked` is not modelled, and that is a real gap

`AbstractContainerScreen.checkHotbarMouseClicked` runs the hotbar/off-hand `SWAP` off a **mouse** button that is neither
left, right, nor pick — a side button someone has bound to `key.swapOffhand` or a
hotbar slot. `MenuButton` has only three variants, so there is nowhere for it to
arrive. Low value (it needs a rebind to reach) but recorded rather than left to
be rediscovered.

## Hover highlight and empty-slot placeholders

Two things reported from play, neither of which was the kind of gap it looked
like.

**The highlight is two sprites, not one.** `slot_highlight_back` and
`slot_highlight_front`, both blitted at `(slot.x - 4, slot.y - 4, 24, 24)`
(`AbstractContainerScreen.extractSlotHighlightBack`,
`AbstractContainerScreen.extractSlotHighlightFront`), with `extractSlots` between them —
one under the hovered slot's item and one over it. So `ContainerGeometry`'s
`bg_verts` now draws in **two ranges** split at `bg_slot_vertex_count`, and
`ContainerRenderer` replays the second after the item passes. A single highlight
appended with the panel art looks *almost* right and is what the naive version
produces; that was the watched-failing control.

**A belief this deleted:** `MenuInput::is_dragging`'s doc said the screen "should
draw the drag preview rather than a hover highlight". Measured false — both blits
are gated on `hoveredSlot != null && isHighlightable()` and on nothing else, *not*
on `isQuickCrafting`, so vanilla draws highlight and preview together mid-drag.
The wrong claim is kept in place, corrected, rather than deleted.

`isHighlightable()` is not restated in our code: base `Slot` returns `true` and
the only 26.2 override is `NonInteractiveResultSlot` (the crafter, the recipe-book
ghost), which no menu this client models uses. A crafting table's `ResultSlot`
does **not** override it, so the result slot *is* highlighted — worth stating
because so many other branches special-case that slot.

**The placeholders were neither an asset nor a data problem.**
`tests/container_slot_sprites.rs` had already measured all seven sprites present
in the GUI atlas and concluded the remainder was "a pipeline/bind-group job".
It was not: they are ordinary textures with an ordinary `.png.mcmeta`, so they
stitch into `ContainerBackground`'s **existing** atlas and reuse the bind group
and pipeline `attach_background` already builds. `AtlasBuilder` needed no new
capability. That note is now stale in the useful direction and is recorded here
rather than silently overtaken.

Two measured details worth keeping:

* The ids come off `Slot::no_item_icon`, never a positional rule. Replacing that
  with `match menu_index { 5..=8 => … }` went red twice: the off-hand (slot 45)
  stops drawing *and* a chest starts painting helmets into its sixth slot — the
  exact trap `lodestone-game/tests/no_item_icons.rs` was written to name.
* The highlight mcmeta declares `nine_slice` with border 4, but the sprite is
  natively 24×24 and the blit is 24×24, so **that path never stretches
  anything**. Implementing nine-slice for it is work with no observable effect.

Everything here is gated on a background being attached, so the jar-less
fallback draws none of it — honest, and the negative control the tests use.

## Title anchors for the screens `label_layout` does not model

`label_layout` has two anchors; nine real screens fell through to `Generic`'s
`(8, 6)` and were wrong there. `menu_type_title_anchor` (`container.rs`) carries
vanilla's values keyed on the wire `menu_type`, and `build_inner` lets it override
`label_layout`'s result.

**`MenuKind` was the wrong lever, and `label_layout`'s own doc comment said
otherwise for two commits.** It claimed the centred furnace title was "not
modelled" because "there is no furnace `MenuKind` yet". A furnace needs none: the
anchor keys off `menu_type`, which the server already sends and
`OpenMenuSnapshot::menu_type` already carries — and growing `MenuKind` is
constrained against anyway, since `slot_layout` matches it exhaustively.

**Three of the nine are decrements, not absolutes.** `LoomScreen`'s constructor and
`CartographyTableScreen`'s constructor are `titleLabelY -= 2`; `StonecutterScreen`'s constructor
is `titleLabelY--`. They resolve to 4/4/5 *only because* the inherited
`titleLabelY` is 6. If `label_layout`'s `title_y` ever stops being 6, all three go
wrong and nothing else would say so.

Beacon and merchant are excluded deliberately: different `imageWidth` (230/276),
their own background art, and `MerchantScreen.extractLabels` composes trade-level
text into the title rather than moving an anchor. Neither has a case in
`background_kind` or `slot_layout`, so an anchor alone would put correct text over
a still-wrong-shaped panel.

## How to change it

- **The screen's press/drag/release/key protocol** —
  `crates/lodestone-shell/src/container.rs`'s `MenuInput`. Audit against
  `AbstractContainerScreen`, not `AbstractContainerMenu`; the two tables in this
  doc are separate for that reason, and a symptom that reaches no pixels usually
  lives here rather than in the machine.
- **Click dispatch and the drag machine** — `crates/lodestone-game/src/click.rs`.
- **Menu shape, quick-move orders, `move_item_stack_to`** —
  `crates/lodestone-game/src/menu.rs`.
- **The predict/reconcile seam and server-event routing** —
  `crates/lodestone-game/src/menus.rs`. Reconciliation mechanics themselves
  (`ClientMenu::reconcile`, `ServerUpdate`, `ClickIntent::to_action`) live in
  `crate::reconcile`, not in these three files.
- **Adding a new click mode or menu layout**: match vanilla's decompiled
  `AbstractContainerMenu` (or the specific menu subclass) line-for-line
  first; every existing test in this module cites the exact vanilla line it
  pins, per the module's own evidence-standard doc comment
  (the `tests` module's own doc comment, `menu.rs`) — "assertions of an absence need a control proving the
  detector works," matching `CLAUDE.md`'s rule. Every hand-derived expected
  value here comes from the `.cache/mc/26.2/src/net/minecraft/world/inventory/`
  decompile, never from this port's own implementation.
- **Furnace/brewing-stand routing**, if you take it on: it needs the
  fuel-value registry and the cooking-recipe input set decoded in the
  version crate first (same shape as the tool census); do not special-case
  slot numbers without that data, and thread it through as a `Menu`-level
  descriptor, not a new `MenuKind`.
- **Armour** — done (`67ff7c3`); see above. What is left is cleanup, not a
  feature: delete `canary_wire_stacks_carry_no_prototype_components`
  (`menu.rs`) and re-point its assertions at a stack built through
  `read_component_patch` instead of a hand-built `ItemComponents::default()`.

## Configuration

None of its own — this is pure logic over `Menu`/`ItemStack`, driven by
whatever `lodestone-shell` feeds it from input events.

## Dependencies

- `lodestone_model::{ItemStack, ItemComponents}` — the wire-shaped stack this
  module's `may_place`/merge logic operates over.
- `crate::reconcile` (`lodestone-game`) — the predict/reconcile machinery
  `Menus` routes into; not itself covered by this doc.
- [`tool-mining.md`](./tool-mining.md) — the same prototype-vs-patch
  component split (`minecraft:tool`) that `minecraft:equippable` and
  `minecraft:max_stack_size` now also go through, per `67ff7c3`.
- [`item-prototypes.md`](./item-prototypes.md) — the census itself
  (`crates/lodestone-data/src/item_prototypes.rs`) and its wire-decode seam.

## Tests

Hermetic, no server or GPU needed — but **not** all in `click.rs`/`menu.rs`'s
own `#[cfg(test)]` modules as this section used to say. `click.rs` has no
tests of its own; the positive-case suite lives in
`crates/lodestone-game/tests/click_machine.rs`, crafting-specific cases in
`tests/crafting_menu.rs`, and `menu.rs`'s `mod tests` holds the
negative-control suite (every "commits nothing" / "resets" assertion paired
with a positive control, per the module's own evidence-standard comment,
the `tests` module's own doc comment in `menu.rs`) plus the swap-overflow and per-menu shift-click tests.
Notable ones cited above, across both locations:
`bare_drag_end_without_start_commits_nothing`,
`drag_with_empty_cursor_commits_nothing`, `clone_drag_resets_in_survival` /
`control_clone_drag_commits_in_creative`,
`drag_skips_a_slot_holding_a_different_item`,
`drag_never_paints_the_result_slot`,
`ordinary_click_mid_drag_resets_and_is_itself_swallowed`,
`chest_to_player_fills_the_hotbar_first`,
`crafting_table_shift_click_loads_the_grid_first` /
`player_screen_shift_click_never_loads_the_two_by_two_grid`,
`shift_click_equips_armour_before_trying_the_hotbar` /
`shift_click_equips_armour_out_of_the_offhand_slot` /
`control_shift_click_falls_through_when_the_armour_slot_is_taken`,
`quick_move_refuses_to_merge_differing_components` /
`control_quick_move_merges_identical_components`,
`pickup_all_never_drains_the_crafting_result`,
`offhand_swap_moves_between_slot_and_offhand` /
`offhand_swap_exchanges_two_stacks`,
`throw_is_a_noop_when_the_player_cannot_drop_items`,
`middle_click_clone_refuses_when_cursor_is_occupied`,
`hotbar_swap_overflow_merges_into_the_remainder_it_left_behind` /
`control_hotbar_swap_without_overflow_is_a_plain_exchange` — the last four
landed from the audit above, closing test-coverage gaps the audit
found (and, for the swap-overflow pair, a real bug alongside them),
`canary_wire_stacks_carry_no_prototype_components` — this last one is now
stale in a way worth flagging rather than silently trusting: it builds its
"wire stack" with `ItemComponents::default()` directly, never through
`read_component_patch`, so it stays green whether or not the prototype census
is wired up and can no longer serve as the reminder its own comment claims to
be (see "Armour equips" above). Every negative-result test in this module is
otherwise paired with a positive control that exercises the same mechanism,
per the module's own stated evidence standard.
