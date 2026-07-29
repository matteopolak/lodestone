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

## How it works

### The seven `ContainerInput` modes

`ContainerInput` (`click.rs:37-52`) is vanilla's click mode, dispatched in
`Menu::do_click` (`click.rs:149-203`):

| mode | variant | button semantics |
| --- | --- | --- |
| 0 | `Pickup` | 0 = left (whole stack), 1 = right (half/one); slot `-999` drops the cursor |
| 1 | `QuickMove` | 0/1 = shift-click quick transfer |
| 2 | `Swap` | 0–8 = hotbar key, 40 = off-hand key |
| 3 | `Clone` | middle-click, creative only (`ctx.infinite_materials`) |
| 4 | `Throw` | 0 = drop one, 1 = drop stack |
| 5 | `QuickCraft` | the drag sequence below |
| 6 | `PickupAll` | double-click gather |

Ergonomic constructors for each: `Click::left`/`right` (`click.rs:791-807`),
`Click::shift` (`:829-837`), `Click::hotbar_swap`/`offhand_swap`
(`:840-857`), `Click::clone_slot` (`:860-867`), `Click::drop_one`/`drop_stack`
(`:869-887`), `Click::double` (`:889-897`).

### `QUICK_CRAFT`'s bit packing

The drag protocol packs a header and a type into one wire byte,
`click.rs:74-90`:

```rust
fn quick_craft_mask(header: u8, kind: u8) -> u8 { (header & 3) | ((kind & 3) << 2) }
fn quick_craft_header(mask: u8) -> u8 { mask & 3 }
fn quick_craft_type(mask: u8) -> u8 { (mask >> 2) & 3 }
```

`drag_header` is `{ START = 0, ADD = 1, END = 2 }`, `drag_type` is
`{ EVEN = 0, ONE = 1, CLONE = 2 }` (`click.rs:54-72`). START and END always
carry slot `-999` (`OUTSIDE_SLOT`) — `perform_drag` (`click.rs:699-723`)
sends both through `do_click(OUTSIDE_SLOT, ...)`. **Only ADD carries a real
slot**: its handler is the only arm of `do_quick_craft` that reads
`slot_index` at all (`click.rs:515-524`). There is no fourth packet for
"still dragging, mouse moved but no new slot painted" — the protocol's three
button-encoded headers are exhaustive; nothing is sent on a bare press.

### The three-stage drag machine, and what actually resets it

Stages: **START** arms a drag of a given type → **ADD** records one painted
slot → **END** distributes the cursor across every recorded slot. Implemented
in `Menu::do_quick_craft` (`click.rs:482-533`). Three things reset it
(`reset_quick_craft`, clearing `quick_craft_status`/`quick_craft_slots` but
**not** `quick_craft_type` — the single-slot degrade path below reads it back
after a reset, `menu.rs:819-826`):

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
   `click.rs:728-735`); an unrecognized 2-bit header value also resets
   (`click.rs:529-531`).

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
and `drag_never_paints_the_result_slot` (`menu.rs:1113-1143`) pin this.

Any ordinary (non-`QuickCraft`) click received while a drag is armed aborts
it *and* is itself swallowed (`click.rs:164-168`,
`ordinary_click_mid_drag_resets_and_is_itself_swallowed`, `menu.rs:977-1002`).
A single painted slot degrades to an ordinary pickup/place click rather than
a distribute (`finish_quick_craft`, `click.rs:552-561`) — this is where the
kept `quick_craft_type` matters.

### Per-menu quick-move (shift-click) orders

Dispatched by `(kind, craft)` in `quick_move` (`menu.rs:564-576`):

- **Generic container** (chests, barrels, ender chests, every
  `generic_9xN`, hoppers, dispensers, droppers, shulker boxes —
  `quick_move_generic`, `menu.rs:592-626`): container → player goes
  **backwards** (hotbar first); player → container goes forwards. Mirrors
  `ChestMenu.java:94-109`, and per the same comment also
  `HopperMenu`/`DispenserMenu`/`ShulkerBoxMenu`.
- **Crafting table** (`quick_move_crafting`, `menu.rs:648-673`, mirroring
  `CraftingMenu.java:107-152`): result slot → player inventory backwards;
  a grid cell → player inventory forwards; a player-inventory item first
  tries to *load the grid* (`first_input..grid_end`), only falling back to
  the main↔hotbar hop if the grid refuses it. "Shift-clicking planks in a
  crafting table loads the grid" (`menu.rs:643-645`).
- **Player inventory screen** (`quick_move_player`, `menu.rs:708-727`,
  mirroring `InventoryMenu.quickMoveStack`, `InventoryMenu.java:100-152`),
  in order:

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
  (`menu.rs:692-704`, regression test
  `shift_click_equips_armour_out_of_the_offhand_slot`, `menu.rs:1381-1388`).

### Two menus left deliberately generic

`AbstractFurnaceMenu` and `BrewingStandMenu` route shift-clicks *by item
kind* rather than by region — smeltables/fuel, or blaze
powder/ingredients/potions, to specific slots before falling back to the
main↔hotbar hop. Neither is modelled here: both predicates need data this
tree doesn't have — `canSmelt` needs the cooking-recipe input set, `isFuel`
needs the fuel-value registry — and guessing would just move the wrongness
rather than remove it (`menu.rs:592-611`, `menus.rs:371-397`). A furnace
therefore predicts a shift-click into container slot 0 where vanilla might
have picked slot 1 or done nothing, and the server corrects it one round
trip later — "a visible flicker, not a desync" (`menus.rs:393-397`). Routing
for these, if it lands, must be carried as a `Menu`-level descriptor (like
`CraftLayout`), not a new `MenuKind` variant — `MenuKind` is matched
exhaustively in `lodestone-shell`'s `slot_layout` (`menus.rs:399-403`).

### Prediction versus authority

`click.rs`'s module doc states the model plainly: this is "an original,
version-free predictor over a `Menu`. The client runs exactly this locally
to predict the result of a click before the server confirms it." `Menus`
(`crates/lodestone-game/src/menus.rs`) is the seam: `Menus::apply` routes
incoming server events into `ClientMenu`'s predict/reconcile machinery or
into menu-lifecycle bookkeeping, and `Menus::click` applies a click
optimistically, then returns the `ClickIntent` that goes on the wire — the
server's own reply, when it arrives, overwrites the prediction through
`apply` (`menus.rs:295-324`).

The clearest place prediction knowingly stops short of the truth is crafting
results. A client's `CraftingMenu` is built with no level access, so it
can't recompute the recipe after a take and its predicted loop exits after
exactly one craft — which is exactly what vanilla's own client predicts too.
The server crafts until the grid runs dry and reconciles the difference back
as `container_set_slot`s (`menu.rs:229-243`). `predicted_craft_result`
(`menus.rs:344-360`) is explicit about the boundary: "This is a prediction,
not the truth… read the result slot for what the player is actually holding
a claim to." `Menu::restore` is likewise labelled "server-authoritative
resync" (`menu.rs:364`).

### Armour is currently unequippable

Two prototype item components are missing entirely: `minecraft:equippable`
and `minecraft:max_stack_size`. Like `minecraft:tool`
(see [`tool-mining.md`](./tool-mining.md)), vanilla stores both in the
item's *built-in* component map, not in the clientbound patch — so nothing a
wire decoder produces will ever carry them without a version-owned census
(an item→equippable table, the same shape as `generated/tools.rs`). Until
that lands, `crate::container::equippable_slot` returns `None` for every
real stack, which disables `Slot::may_place` for an `Armor` slot outright:
no click of any kind can currently put armour on
(`Menu::empty_equip_target`'s doc, `menu.rs:729-763`). Every stack also
reports a max stack size of 64 regardless of the real item. This is asserted
*on purpose*, as the current wrong state, by
`canary_wire_stacks_carry_no_prototype_components` (`menu.rs:1441-1487`):
when the census lands this test starts failing, and that failure is the
reminder to delete it and re-point the armour/stack-cap tests at real wire
stacks. The positive-path tests for equip logic (`armour_slot_refuses_the_
wrong_equipment_position` etc.) build the component by hand in the meantime.

### Three quirks that are transcribed deliberately, because they read as bugs

`Menu::move_item_stack_to` (`menu.rs:493-549`) is the port of vanilla
`moveItemStackTo` (`AbstractContainerMenu.java:636-697`). Its doc comment
calls out three details explicitly kept rather than "fixed"
(`menu.rs:465-492`):

1. **The merge pass never calls `may_place`.** Only the empty-slot pass does
   (`slot.mayPlace`, `AbstractContainerMenu.java:682`, vs. no check at
   `:647`). A shift-click can top up an existing stack in a slot that would
   refuse the same item arriving into an empty cell.
2. **The merge pass is gated on `is_stackable`, not on the per-slot cap.**
   An unstackable item skips merging entirely and goes straight to the
   first empty slot (`AbstractContainerMenu.java:645`).
3. **The two passes measure their cap against different stacks.** The merge
   cap is `effective_max(slot, &target)` — the slot's *existing* stack
   (`:650`); the empty-slot cap is `effective_max(slot, moving)` — the
   *incoming* stack (`:683`). They agree whenever it's the same item, which
   the merge pass has already established by the time it runs — so it's
   only a difference in what the code says, not what it does, but it's what
   the source says, so it's transcribed as-is.

The empty-slot pass also stops after exactly one placement (`break` at
`AbstractContainerMenu.java:687`), which is why a caller moving more than
one stack's worth has to loop.

## How to change it

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
  (`menu.rs:849-862`) — "assertions of an absence need a control proving the
  detector works," matching `CLAUDE.md`'s rule. Every hand-derived expected
  value here comes from the `.cache/mc/26.2/src/net/minecraft/world/inventory/`
  decompile, never from this port's own implementation.
- **Furnace/brewing-stand routing**, if you take it on: it needs the
  fuel-value registry and the cooking-recipe input set decoded in the
  version crate first (same shape as the tool census); do not special-case
  slot numbers without that data, and thread it through as a `Menu`-level
  descriptor, not a new `MenuKind`.
- **Armour**, if you take it on: add an item→`equippable` census
  (`generated/tools.rs`-shaped) to the version crate, wire it into stack
  construction, then delete `canary_wire_stacks_carry_no_prototype_components`
  and re-point its assertions at real wire stacks.

## Configuration

None of its own — this is pure logic over `Menu`/`ItemStack`, driven by
whatever `lodestone-shell` feeds it from input events.

## Dependencies

- `lodestone_model::{ItemStack, ItemComponents}` — the wire-shaped stack this
  module's `may_place`/merge logic operates over.
- `crate::reconcile` (`lodestone-game`) — the predict/reconcile machinery
  `Menus` routes into; not itself covered by this doc.
- [`tool-mining.md`](./tool-mining.md) — the same prototype-vs-patch
  component split (`minecraft:tool`) that explains why `equippable` and
  `max_stack_size` are unreachable today.

## Tests

All in `crates/lodestone-game/src/{click.rs,menu.rs}`'s `#[cfg(test)]`
modules — hermetic, no server or GPU needed. Notable ones cited above:
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
`canary_wire_stacks_carry_no_prototype_components`. Every negative-result
test in this module is paired with a positive control that exercises the
same mechanism, per the module's own stated evidence standard.
