/// Negative controls for the container click semantics.
///
/// The positive cases live in `tests/click_machine.rs`. What is here is the
/// other half that `CLAUDE.md`'s evidence standard demands: *"assertions of an
/// absence need a control proving the detector works"*. Every test below that
/// asserts nothing happened is paired with a **control** which differs by the
/// one thing the rule turns on and which must observably succeed — so a
/// regression that makes the mechanism fire always, or never, is caught either
/// way rather than being satisfied vacuously by a menu that simply refuses
/// everything.
///
/// Every expected value is hand-derived from the 26.2 decompile, cited per
/// test. None is
/// derived by running our own implementation.
use super::*;
    use crate::click::{
        Click, ContainerInput, PlayerCtx, drag_header, drag_type, quick_craft_mask,
    };
    use crate::item::{ComponentValue, ItemComponents};
    use lodestone_model::{Identifier, Text};

    fn id(name: &str) -> Identifier {
        name.parse().expect("valid identifier")
    }

    fn stack(name: &str, count: i32) -> ItemStack {
        ItemStack::new(id(name), count)
    }

    /// The same item carrying a `minecraft:custom_name`, i.e. same item,
    /// *different* components — the pair vanilla `isSameItemSameComponents` must
    /// refuse to merge. The payload shape matches what
    /// [`ItemStack::from`] produces for a wire stack that carried a custom name,
    /// so these are the components an adapter would really hand us.
    fn named(name: &str, count: i32, label: &str) -> ItemStack {
        let mut components = ItemComponents::new();
        components.insert(
            id("minecraft:custom_name"),
            ComponentValue::Text(Text::literal(label)),
        );
        ItemStack::with_components(id(name), count, components)
    }

    /// A stack whose `minecraft:equippable` names an armour position. Hand-built
    /// because nothing populates this component from the wire yet — see
    /// [`Menu::empty_equip_target`].
    fn equippable(name: &str, slot: &str) -> ItemStack {
        let mut components = ItemComponents::new();
        components.insert(
            id("minecraft:equippable"),
            ComponentValue::Str(slot.to_string()),
        );
        ItemStack::with_components(id(name), 1, components)
    }

    fn count_at(menu: &Menu, index: usize) -> Option<i32> {
        menu.slot_item(index).map(ItemStack::count)
    }

    fn carried_count(menu: &Menu) -> Option<i32> {
        menu.carried().map(ItemStack::count)
    }

    fn drag(slot: i32, header: i32, kind: i32) -> Click {
        Click {
            slot,
            button: quick_craft_mask(header, kind),
            input: ContainerInput::QuickCraft,
    }

    /// Total item count across every menu slot plus the cursor. A drag must
    /// conserve it exactly; an off-by-one in the even split shows up here even
    /// when every individual slot assertion is written to match the bug.
    fn total_items(menu: &Menu) -> i32 {
        (0..menu.slot_count())
            .filter_map(|i| menu.slot_item(i))
            .map(ItemStack::count)
            .sum::<i32>()
            + menu.carried().map_or(0, ItemStack::count)
    }

    // --- QUICK_CRAFT: drags that must reset and commit nothing ---

    /// Vanilla's own quick-craft header-check step. The header sequence is checked
    /// against the *previous* status: `(expected != 1 || header != 2) &&
    /// expected != header` resets. A bare `END` arrives with `expected == 0` and
    /// `header == 2`, so `(true || false) && (0 != 2)` holds and the drag is
    /// reset — nothing is placed and the cursor is untouched.
    #[test]
    fn bare_drag_end_without_start_commits_nothing() {
        let mut menu = Menu::generic(27);
        menu.set_carried(Some(stack("minecraft:stone", 9)));

        // No START, no ADD: just the commit packet.
        drag(OUTSIDE_SLOT, drag_header::END, drag_type::EVEN)
            .apply(&mut menu, PlayerCtx::survival());

        assert_eq!(count_at(&menu, 0), None, "no slot may be written");
        assert_eq!(
            carried_count(&menu),
            Some(9),
            "the cursor must be returned whole"
        );
    }

    /// The control for [`bare_drag_end_without_start_commits_nothing`]: the same
    /// three slots, the same cursor, but a well-formed START/ADD…/END sequence
    /// must place. Without this, a `Menu` that had lost the ability to commit a
    /// drag at all would pass the negative test.
    #[test]
    fn control_well_formed_drag_does_commit() {
        let mut menu = Menu::generic(27);
        menu.set_carried(Some(stack("minecraft:stone", 9)));
        menu.perform_drag(drag_type::EVEN, &[0, 1, 2], PlayerCtx::survival());
        assert_eq!(count_at(&menu, 0), Some(3));
        assert_eq!(count_at(&menu, 1), Some(3));
        assert_eq!(count_at(&menu, 2), Some(3));
        assert_eq!(carried_count(&menu), None);
    }

    /// Vanilla's own click-dispatch step: *any* non-`QUICK_CRAFT` click while
    /// a drag is armed takes the `else if (this.quickcraftStatus != 0)` branch,
    /// which resets and falls out of `doClick` entirely. So the interrupting
    /// click is **also** swallowed — it does not pick anything up — and the
    /// subsequent `END` finds an empty painted set.
    #[test]
    fn ordinary_click_mid_drag_resets_and_is_itself_swallowed() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(5, Some(stack("minecraft:dirt", 8)));
        menu.set_carried(Some(stack("minecraft:stone", 9)));
        let ctx = PlayerCtx::survival();

        drag(OUTSIDE_SLOT, drag_header::START, drag_type::EVEN).apply(&mut menu, ctx.clone());
        drag(0, drag_header::ADD, drag_type::EVEN).apply(&mut menu, ctx.clone());
        drag(1, drag_header::ADD, drag_type::EVEN).apply(&mut menu, ctx.clone());

        // The interrupt. A left-click on an occupied slot would normally swap.
        Click::left(5).apply(&mut menu, ctx.clone());
        assert_eq!(
            count_at(&menu, 5),
            Some(8),
            "the interrupting click must be swallowed, not applied"
        );
        assert_eq!(carried_count(&menu), Some(9));

        // And the commit that follows has nothing left to commit.
        drag(OUTSIDE_SLOT, drag_header::END, drag_type::EVEN).apply(&mut menu, ctx);
        assert_eq!(count_at(&menu, 0), None);
        assert_eq!(count_at(&menu, 1), None);
        assert_eq!(carried_count(&menu), Some(9));
    }

    /// The control for the interrupt: with the drag *not* armed, the identical
    /// left-click on slot 5 does swap cursor and slot. This is what proves the
    /// assertion above is observing the reset rather than a menu where clicking
    /// never worked.
    #[test]
    fn control_same_click_applies_when_no_drag_is_armed() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(5, Some(stack("minecraft:dirt", 8)));
        menu.set_carried(Some(stack("minecraft:stone", 9)));
        Click::left(5).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(
            menu.slot_item(5).map(|s| s.item().path().to_string()),
            Some("stone".into())
        );
        assert_eq!(carried_count(&menu), Some(8));
    }

    /// Vanilla's own drag-start step: an empty cursor at any stage resets
    /// the drag. The paint stage therefore cannot record slots against nothing,
    /// and the commit cannot invent items.
    #[test]
    fn drag_with_empty_cursor_commits_nothing() {
        let mut menu = Menu::generic(27);
        // Cursor deliberately empty.
        menu.perform_drag(drag_type::EVEN, &[0, 1, 2], PlayerCtx::survival());
        assert_eq!(total_items(&menu), 0);
    }

    /// Vanilla's own drag paint and commit steps. The paint
    /// guard is `carried.getCount() > quickcraftSlots.size()` — strictly greater
    /// — so a cursor of 2 can only ever paint 2 slots: the third `ADD` sees
    /// `2 > 2` and is dropped. The even split is then over 2, not 3.
    ///
    /// This is the off-by-one the parent task called the classic bug, stated as
    /// an assertion: a naive implementation paints all three and divides 2 by 3,
    /// placing zero everywhere.
    #[test]
    fn paint_stops_when_the_cursor_runs_out_of_items() {
        let mut menu = Menu::generic(27);
        menu.set_carried(Some(stack("minecraft:stone", 2)));
        menu.perform_drag(drag_type::EVEN, &[0, 1, 2], PlayerCtx::survival());
        assert_eq!(count_at(&menu, 0), Some(1));
        assert_eq!(count_at(&menu, 1), Some(1));
        assert_eq!(count_at(&menu, 2), None, "the third slot is never painted");
        assert_eq!(carried_count(&menu), None);
        assert_eq!(total_items(&menu), 2, "a drag conserves items exactly");
    }

    /// Vanilla's own drag commit step. The per-slot amount is clamped by
    /// `min(source.getMaxStackSize(), slot.getMaxStackSize(source))` **after**
    /// adding what the slot already holds, and the shortfall stays on the
    /// cursor. Slot 0 starts at 62 of a 64 cap, so it can only take 2 of its
    /// nominal 5; the other 3 must come back.
    #[test]
    fn even_split_clamps_at_the_slot_cap_and_returns_the_remainder() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(0, Some(stack("minecraft:stone", 62)));
        menu.set_carried(Some(stack("minecraft:stone", 10)));
        menu.perform_drag(drag_type::EVEN, &[0, 1], PlayerCtx::survival());
        // place count = 10 / 2 = 5. Slot 0: min(5 + 62, 64) = 64, so +2.
        assert_eq!(count_at(&menu, 0), Some(64));
        // Slot 1: min(5 + 0, 64) = 5.
        assert_eq!(count_at(&menu, 1), Some(5));
        // 10 - 2 - 5 = 3 back on the cursor.
        assert_eq!(carried_count(&menu), Some(3));
        assert_eq!(total_items(&menu), 72);
    }

    /// Vanilla's own painted-accumulator field — a
    /// `HashSet`, so a slot dragged over twice counts once. With `[0, 1, 0, 1]`
    /// the divisor must be 2, not 4: 8 items becomes 4 each, not 2 each.
    #[test]
    fn repainting_a_slot_does_not_inflate_the_divisor() {
        let mut menu = Menu::generic(27);
        menu.set_carried(Some(stack("minecraft:stone", 8)));
        menu.perform_drag(drag_type::EVEN, &[0, 1, 0, 1], PlayerCtx::survival());
        assert_eq!(count_at(&menu, 0), Some(4));
        assert_eq!(count_at(&menu, 1), Some(4));
        assert_eq!(carried_count(&menu), None);
    }

    /// Vanilla's own quick-craft-type validity check: type 2 requires
    /// `player.hasInfiniteMaterials()`, so a
    /// middle-drag in survival resets at the START stage and commits nothing.
    #[test]
    fn clone_drag_resets_in_survival() {
        let mut menu = Menu::generic(27);
        menu.set_carried(Some(stack("minecraft:stone", 64)));
        menu.perform_drag(drag_type::CLONE, &[0, 1], PlayerCtx::survival());
        assert_eq!(count_at(&menu, 0), None);
        assert_eq!(count_at(&menu, 1), None);
        assert_eq!(carried_count(&menu), Some(64));
    }

    /// The control: the identical sequence with infinite materials places a full
    /// stack per slot (`getQuickCraftPlaceCount` case 2, `:733`).
    #[test]
    fn control_clone_drag_commits_in_creative() {
        let mut menu = Menu::generic(27);
        menu.set_carried(Some(stack("minecraft:stone", 64)));
        menu.perform_drag(drag_type::CLONE, &[0, 1], PlayerCtx::creative());
        assert_eq!(count_at(&menu, 0), Some(64));
        assert_eq!(count_at(&menu, 1), Some(64));
    }

    /// Vanilla's own quick-replace eligibility check is applied at
    /// both the paint and commit stages, and it refuses an occupied slot holding
    /// a different item. Slot 1 holds dirt, so it is never painted and the split
    /// is over the two remaining slots.
    #[test]
    fn drag_skips_a_slot_holding_a_different_item() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(1, Some(stack("minecraft:dirt", 1)));
        menu.set_carried(Some(stack("minecraft:stone", 9)));
        menu.perform_drag(drag_type::EVEN, &[0, 1, 2], PlayerCtx::survival());
        assert_eq!(count_at(&menu, 0), Some(4));
        assert_eq!(
            menu.slot_item(1).map(|s| s.item().path().to_string()),
            Some("dirt".into()),
            "the foreign stack must be untouched"
        );
        assert_eq!(count_at(&menu, 2), Some(4));
        assert_eq!(carried_count(&menu), Some(1));
    }

    /// The result slot rejects placement (vanilla's own result-slot placement
    /// check always returns `false`), and both drag stages test `slot.mayPlace`.
    /// A drag across a crafting grid that clips the result
    /// slot must skip it and divide over the grid cells only.
    #[test]
    fn drag_never_paints_the_result_slot() {
        let mut menu = Menu::crafting(3, 3);
        menu.set_carried(Some(stack("minecraft:stone", 4)));
        // Slot 0 is the result; 1 and 2 are grid cells.
        menu.perform_drag(drag_type::EVEN, &[0, 1, 2], PlayerCtx::survival());
        assert_eq!(count_at(&menu, 0), None, "the result slot is take-only");
        assert_eq!(count_at(&menu, 1), Some(2));
        assert_eq!(count_at(&menu, 2), Some(2));
        assert_eq!(carried_count(&menu), None);
    }

    // --- Merging: refused for differing components ---

    /// Vanilla's own click-deposit step gates the deposit on the two
    /// stacks being the same item with the same components. Two stacks of the
    /// same item with different components must **swap**, not merge —
    /// so neither count changes and the identities exchange.
    #[test]
    fn pickup_refuses_to_merge_stacks_with_differing_components() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(0, Some(named("minecraft:diamond_sword", 1, "Excalibur")));
        menu.set_carried(Some(stack("minecraft:diamond_sword", 1)));

        Click::left(0).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(count_at(&menu, 0), Some(1), "no merge to 2");
        assert_eq!(carried_count(&menu), Some(1), "no merge to 0");
        assert!(
            menu.slot_item(0).unwrap().components().is_empty(),
            "the plain stack is now in the slot: this was a swap"
        );
        assert!(
            !menu.carried().unwrap().components().is_empty(),
            "the named stack is now on the cursor"
        );
    }

    /// The control: identical components *do* merge, on the `:452-454` branch.
    /// Without it the test above passes for a `Menu` that never merges at all.
    #[test]
    fn control_pickup_merges_stacks_with_identical_components() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(0, Some(named("minecraft:stone", 1, "Rock")));
        menu.set_carried(Some(named("minecraft:stone", 1, "Rock")));
        Click::left(0).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 0), Some(2));
        assert_eq!(carried_count(&menu), None);
    }

    /// Vanilla's own stack-move-to merge pass tests the same predicate,
    /// so a shift-click must not stack a
    /// named item onto a plain one either. It falls through to the empty-slot
    /// pass and lands in the first free cell instead.
    #[test]
    fn quick_move_refuses_to_merge_differing_components() {
        let mut menu = Menu::generic(27);
        // Container slot 0 holds the named stack to be shift-moved out.
        menu.set_slot_item(0, Some(named("minecraft:stone", 4, "Rock")));
        // A plain stack of the same item sits in the *last* player slot, which is
        // where a backwards merge pass would reach first.
        let last = menu.slot_count() - 1;
        menu.set_slot_item(last, Some(stack("minecraft:stone", 10)));

        Click::shift(0).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(
            count_at(&menu, last),
            Some(10),
            "the plain stack must not absorb the named one"
        );
        assert_eq!(
            count_at(&menu, last - 1),
            Some(4),
            "the named stack takes the next empty slot instead"
        );
    }

    /// The control: make the destination stack's components match and the same
    /// shift-click merges into it rather than taking a fresh slot.
    #[test]
    fn control_quick_move_merges_identical_components() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(0, Some(named("minecraft:stone", 4, "Rock")));
        let last = menu.slot_count() - 1;
        menu.set_slot_item(last, Some(named("minecraft:stone", 10, "Rock")));

        Click::shift(0).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(count_at(&menu, last), Some(14));
        assert_eq!(count_at(&menu, last - 1), None);
    }

    // --- PICKUP_ALL: the maxed-slot skip ---

    /// Vanilla's own pick-all gather step. It runs **two** passes over
    /// the slot list, and pass 0 skips any slot whose stack is already at its
    /// own max (`itemStack.getCount() != itemStack.getMaxStackSize()`). So a
    /// full stack is only drawn from once every partial one has been consumed.
    ///
    /// Cursor 4 + partials 30 and 20 = 54; the remaining 10 then comes off the
    /// full 64, leaving 54 behind. An implementation with a single pass would
    /// hit the full stack first and leave the partials untouched.
    #[test]
    fn pickup_all_defers_a_maxed_slot_to_the_second_pass() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(0, Some(stack("minecraft:stone", 64))); // maxed, first in order
        menu.set_slot_item(1, Some(stack("minecraft:stone", 30)));
        menu.set_slot_item(2, Some(stack("minecraft:stone", 20)));
        menu.set_carried(Some(stack("minecraft:stone", 4)));

        // Gather is triggered on an empty slot (slot 3), as the real double-click
        // does once the first click has lifted the stack onto the cursor.
        Click::double(3).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(carried_count(&menu), Some(64));
        assert_eq!(count_at(&menu, 1), None, "partials are drained first");
        assert_eq!(count_at(&menu, 2), None);
        assert_eq!(
            count_at(&menu, 0),
            Some(54),
            "the maxed slot is only tapped in pass 1, for the shortfall"
        );
    }

    /// The control for the skip: one item short of max, the *same* slot is drawn
    /// from in pass 0. This is what makes the assertion above about ordering
    /// meaningful rather than a statement that slot 0 is never touched.
    #[test]
    fn control_pickup_all_takes_a_near_max_slot_in_the_first_pass() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(0, Some(stack("minecraft:stone", 63))); // one short of max
        menu.set_slot_item(1, Some(stack("minecraft:stone", 30)));
        menu.set_carried(Some(stack("minecraft:stone", 4)));

        Click::double(3).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(carried_count(&menu), Some(64));
        assert_eq!(
            count_at(&menu, 0),
            Some(3),
            "pass 0 drained 60 of the 63 before reaching slot 1"
        );
        assert_eq!(count_at(&menu, 1), Some(30), "slot 1 was never needed");
    }

    /// Vanilla's own pick-all eligibility check requires
    /// `this.canTakeItemForPickAll(carried, target)`, which every result-bearing
    /// menu overrides to exclude its own result container — every one of
    /// them carries the identical
    /// `target.container != this.resultSlots` line.
    ///
    /// Vacuuming the result slot would craft an item the player never asked for
    /// *and* silently charge the grid for it, because taking from the result runs
    /// vanilla's own result-slot take hook.
    #[test]
    fn pickup_all_never_drains_the_crafting_result() {
        let mut menu = Menu::crafting(3, 3);
        // A result the server has pushed, and a matching stack in the inventory.
        menu.set_slot_item(0, Some(stack("minecraft:stone", 8)));
        let last = menu.slot_count() - 1;
        menu.set_slot_item(last, Some(stack("minecraft:stone", 5)));
        // Grid cells that on_take would decrement if the result were taken.
        menu.set_slot_item(1, Some(stack("minecraft:cobblestone", 3)));
        menu.set_carried(Some(stack("minecraft:stone", 1)));

        Click::double(last - 1).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(
            count_at(&menu, 0),
            Some(8),
            "the result slot must not be a gather source"
        );
        assert_eq!(
            count_at(&menu, 1),
            Some(3),
            "and so the grid must not be charged"
        );
        assert_eq!(
            carried_count(&menu),
            Some(6),
            "only the ordinary inventory stack was gathered"
        );
    }

    // --- QUICK_MOVE ordering, per menu ---

    /// Vanilla's own chest quick-move step moves container contents out with
    /// `backwards = true`,
    /// so a chest empties into the **hotbar** (the tail of the menu slot list)
    /// before the main storage rows. Getting the flag wrong is invisible in an
    /// empty inventory and obvious to a player.
    #[test]
    fn chest_to_player_fills_the_hotbar_first() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(0, Some(stack("minecraft:stone", 10)));
        Click::shift(0).apply(&mut menu, PlayerCtx::survival());
        let last = menu.slot_count() - 1;
        assert_eq!(count_at(&menu, last), Some(10));
        assert_eq!(count_at(&menu, 27), None, "main storage is untouched");
    }

    /// Vanilla's own crafting-table quick-move step: a shift-click from the player rows of a crafting
    /// table tries the **grid** (`1..10`) first, and only falls back to the
    /// main↔hotbar hop if the grid takes nothing.
    #[test]
    fn crafting_table_shift_click_loads_the_grid_first() {
        let mut menu = Menu::crafting(3, 3);
        let hotbar = menu.slot_count() - 9;
        menu.set_slot_item(hotbar, Some(stack("minecraft:oak_planks", 1)));
        Click::shift(hotbar).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 1), Some(1), "into the first grid cell");
        assert_eq!(count_at(&menu, hotbar), None);
    }

    /// Vanilla's own player-inventory quick-move step has **no** such branch: its chain
    /// never targets the 2×2 grid, so the same
    /// gesture on the player's own screen does the main↔hotbar hop instead.
    /// This is the negative control for the test above — the two menus must not
    /// share one implementation.
    #[test]
    fn player_screen_shift_click_never_loads_the_two_by_two_grid() {
        let mut menu = Menu::player();
        menu.set_slot_item(36, Some(stack("minecraft:oak_planks", 1))); // hotbar[0]
        Click::shift(36).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 1), None, "grid cell 1 must stay empty");
        assert_eq!(count_at(&menu, 9), Some(1), "it goes to main storage");
    }

    fn assert_declared_furnace_input_does_not_spill_into_fuel(layout: SpecialLayout) {
        let mut menu = Menu::furnace(layout);
        let hotbar = menu.slot_count() - 9;
        menu.set_slot_item(0, Some(stack("minecraft:stone", 64)));
        menu.set_slot_item(hotbar, Some(stack("minecraft:raw_iron", 1)));

        Click::shift(hotbar).apply(
            &mut menu,
            PlayerCtx::survival().with_furnace_input_items(vec![id("minecraft:raw_iron")]),
        );

        assert_eq!(count_at(&menu, 0), Some(64), "the full ingredient slot stays intact");
        assert_eq!(
            count_at(&menu, 1),
            None,
            "a declared input must not spill into the fuel slot"
        );
        assert_eq!(count_at(&menu, hotbar), Some(1), "the input remains with the player");
    }

    fn assert_declared_furnace_input_moves_to_ingredient_slot(layout: SpecialLayout) {
        let mut menu = Menu::furnace(layout);
        let hotbar = menu.slot_count() - 9;
        menu.set_slot_item(hotbar, Some(stack("minecraft:raw_iron", 1)));

        Click::shift(hotbar).apply(
            &mut menu,
            PlayerCtx::survival().with_furnace_input_items(vec![id("minecraft:raw_iron")]),
        );

        assert_eq!(count_at(&menu, 0), Some(1), "the declared input fills slot 0");
        assert_eq!(count_at(&menu, 1), None, "the fuel slot stays empty");
        assert_eq!(count_at(&menu, hotbar), None, "the input leaves the player inventory");
    }

    #[test]
    fn furnace_declared_input_moves_to_ingredient_slot() {
        assert_declared_furnace_input_moves_to_ingredient_slot(SpecialLayout::Furnace);
    }

    #[test]
    fn blast_furnace_declared_input_moves_to_ingredient_slot() {
        assert_declared_furnace_input_moves_to_ingredient_slot(SpecialLayout::BlastFurnace);
    }

    #[test]
    fn smoker_declared_input_moves_to_ingredient_slot() {
        assert_declared_furnace_input_moves_to_ingredient_slot(SpecialLayout::Smoker);
    }

    #[test]
    fn furnace_declared_input_does_not_spill_into_fuel() {
        assert_declared_furnace_input_does_not_spill_into_fuel(SpecialLayout::Furnace);
    }

    #[test]
    fn blast_furnace_declared_input_does_not_spill_into_fuel() {
        assert_declared_furnace_input_does_not_spill_into_fuel(SpecialLayout::BlastFurnace);
    }

    #[test]
    fn smoker_declared_input_does_not_spill_into_fuel() {
        assert_declared_furnace_input_does_not_spill_into_fuel(SpecialLayout::Smoker);
    }

    #[test]
    fn furnace_non_input_preserves_generic_fuel_slot_fallback() {
        let mut menu = Menu::furnace(SpecialLayout::Furnace);
        let hotbar = menu.slot_count() - 9;
        menu.set_slot_item(0, Some(stack("minecraft:stone", 64)));
        menu.set_slot_item(hotbar, Some(stack("minecraft:raw_iron", 1)));

        Click::shift(hotbar).apply(
            &mut menu,
            PlayerCtx::survival().with_furnace_input_items(vec![id("minecraft:raw_copper")]),
        );

        assert_eq!(count_at(&menu, 1), Some(1), "non-inputs retain generic ordering");
        assert_eq!(count_at(&menu, hotbar), None);
    }

    #[test]
    fn furnace_without_property_set_preserves_generic_fuel_slot_fallback() {
        let mut menu = Menu::furnace(SpecialLayout::Furnace);
        let hotbar = menu.slot_count() - 9;
        menu.set_slot_item(0, Some(stack("minecraft:stone", 64)));
        menu.set_slot_item(hotbar, Some(stack("minecraft:raw_iron", 1)));

        Click::shift(hotbar).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(count_at(&menu, 1), Some(1), "missing data retains generic ordering");
        assert_eq!(count_at(&menu, hotbar), None);
    }

    /// Branches 4 and 5 of vanilla's own player-inventory quick-move step
    /// precede the main↔hotbar hop, so a helmet
    /// in main storage equips rather than moving to the hotbar.
    #[test]
    fn shift_click_equips_armour_before_trying_the_hotbar() {
        let mut menu = Menu::player();
        menu.set_slot_item(9, Some(equippable("minecraft:diamond_helmet", "head")));
        Click::shift(9).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 5), Some(1), "menu slot 5 is the head slot");
        assert_eq!(count_at(&menu, 9), None);
        assert_eq!(count_at(&menu, 36), None, "not the hotbar");
    }

    /// The regression this test guards against. Vanilla reaches the auto-equip branches
    /// from *every* source slot at or after 9, which includes menu slot 45, the
    /// off-hand: a helmet stashed in the off-hand shift-clicks up onto the head.
    /// Testing for an equip target only inside the `9..36` / `36..45` arms let
    /// slot 45 fall through to branch 8 and dump it into storage.
    #[test]
    fn shift_click_equips_armour_out_of_the_offhand_slot() {
        let mut menu = Menu::player();
        menu.set_slot_item(45, Some(equippable("minecraft:diamond_helmet", "head")));
        Click::shift(45).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 5), Some(1), "equipped onto the head");
        assert_eq!(count_at(&menu, 45), None);
        assert_eq!(count_at(&menu, 9), None, "not dumped into main storage");
    }

    /// The control for the branch order: with the head slot already **occupied**,
    /// branch 4's `!slots.get(8 - index).hasItem()` fails and the item takes the
    /// ordinary path out of the off-hand into storage (branch 8, `9..45`
    /// forwards). Without this, the test above would pass for an implementation
    /// that equips unconditionally and overwrites worn armour.
    #[test]
    fn control_shift_click_falls_through_when_the_armour_slot_is_taken() {
        let mut menu = Menu::player();
        menu.set_slot_item(5, Some(equippable("minecraft:iron_helmet", "head")));
        menu.set_slot_item(45, Some(equippable("minecraft:diamond_helmet", "head")));
        Click::shift(45).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(
            menu.slot_item(5).map(|s| s.item().path().to_string()),
            Some("iron_helmet".into()),
            "worn armour must not be displaced"
        );
        assert_eq!(
            count_at(&menu, 9),
            Some(1),
            "the diamond helmet goes to storage"
        );
        assert_eq!(count_at(&menu, 45), None);
    }

    /// Vanilla's own armour-slot placement check is
    /// `slot == equippable.slot()`. A chestplate
    /// must not enter the head slot, by any route — here the direct place.
    #[test]
    fn armour_slot_refuses_the_wrong_equipment_position() {
        let mut menu = Menu::player();
        menu.set_carried(Some(equippable("minecraft:diamond_chestplate", "chest")));
        Click::left(5).apply(&mut menu, PlayerCtx::survival()); // 5 = head
        assert_eq!(count_at(&menu, 5), None);
        assert_eq!(carried_count(&menu), Some(1), "the cursor keeps it");
    }

    /// The control: the matching position accepts. Together with the test above
    /// this pins `may_place` to the *position*, not to "armour slots reject
    /// everything" — which is, today, exactly what happens for any stack that
    /// came off the wire, because nothing populates `minecraft:equippable`. See
    /// [`Menu::empty_equip_target`].
    #[test]
    fn control_armour_slot_accepts_the_matching_position() {
        let mut menu = Menu::player();
        menu.set_carried(Some(equippable("minecraft:diamond_helmet", "head")));
        Click::left(5).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 5), Some(1));
        assert_eq!(carried_count(&menu), None);
    }

    /// The other half of the canary above: once the effective fields *are*
    /// populated, the conversion must carry them and armour must go on.
    ///
    /// This exercises `From<&lodestone_model::ItemStack>`, which is the only
    /// path a wire stack takes into the menu model. The values here stand in for
    /// what the v26-2 prototype census folds in during decode — this crate cannot
    /// depend on a version crate to decode for real, so the fields are set
    /// directly and the *conversion* is what is under test.
    #[test]
    fn populated_prototype_components_survive_the_conversion_and_equip() {
        let helmet = ItemStack::from(&lodestone_model::ItemStack {
            item: id("minecraft:diamond_helmet"),
            count: 1,
            components: lodestone_model::ItemComponents {
                equippable: Some(lodestone_model::event::EquipmentSlot::Head),
                max_stack_size: Some(1),
                max_damage: Some(363),
                ..lodestone_model::ItemComponents::default()
            },
        });

        assert_eq!(
            crate::container::equippable_slot(&helmet),
            Some(EquipmentSlot::Head),
            "the equippable slot must survive the wire->menu conversion"
        );
        assert_eq!(
            helmet.max_stack_size(),
            1,
            "a real per-item cap must not fall back to 64"
        );
        assert!(
            !helmet.is_stackable(),
            "carrying max_damage must make a damageable item unstackable"
        );

        let mut menu = Menu::player();
        menu.set_carried(Some(helmet));
        Click::left(5).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(
            count_at(&menu, 5),
            Some(1),
            "a diamond helmet must now actually go into the head slot"
        );
    }

    /// The control the old suite could not express, and the one that would have
    /// caught `"chest" | "body"`.
    ///
    /// `wolf_armor` is genuinely `body`, and vanilla's own humanoid-armour gate
    /// excludes `BODY`. If `body` is ever
    /// folded into `Chest` again, this fails while every positive test above
    /// keeps passing.
    #[test]
    fn animal_body_armour_is_refused_by_the_player_chestplate_slot() {
        let wolf = ItemStack::from(&lodestone_model::ItemStack {
            item: id("minecraft:wolf_armor"),
            count: 1,
            components: lodestone_model::ItemComponents {
                equippable: Some(lodestone_model::event::EquipmentSlot::Body),
                max_stack_size: Some(1),
                ..lodestone_model::ItemComponents::default()
            },
        });

        assert_eq!(
            crate::container::equippable_slot(&wolf),
            None,
            "`body` must not resolve to a humanoid armour slot"
        );

        let mut menu = Menu::player();
        menu.set_carried(Some(wolf));
        // Slot 6 is the chestplate position on the player screen.
        Click::left(6).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(
            count_at(&menu, 6),
            None,
            "wolf armour must not be wearable as a chestplate"
        );
    }
    /// Vanilla's own number-key swap step: swapping a bigger stack
    /// onto a slot whose cap is smaller than the incoming count splits the
    /// overflow into the slot and pushes the slot's *previous* contents back
    /// into the inventory via `inventory.add`.
    ///
    /// The subtlety is aliasing: vanilla's `source` is the *same object* as
    /// its own live inventory-item lookup (returns the live list element, not a
    /// copy), and vanilla's own stack-split step mutates that object in place
    /// via `shrink`. So by the time `inventory.add` runs, the hotbar slot the
    /// swap came from *already* shows its reduced remainder — and a same-item
    /// displaced stack merges back into it rather than taking a fresh slot.
    /// Egg's stack cap is overridden to 16 here purely to make the overflow
    /// branch reachable without exceeding the real 64-cap game items use.
    #[test]
    fn hotbar_swap_overflow_merges_into_the_remainder_it_left_behind() {
        let mut menu = Menu::player();
        // hotbar key 0 -> native 0 -> menu slot 36.
        menu.set_slot_item(
            36,
            Some(stack("minecraft:egg", 20).with_max_stack_size(16)),
        );
        // Target: main storage slot 9 holds 5 eggs.
        menu.set_slot_item(9, Some(stack("minecraft:egg", 5).with_max_stack_size(16)));
        Click::hotbar_swap(9, 0).apply(&mut menu, PlayerCtx::survival());

        // cap = min(64, 16) = 16; source(20) > cap, so 16 eggs land in slot 9 and
        // 4 remain in the hotbar slot the swap came from.
        assert_eq!(count_at(&menu, 9), Some(16), "the overflow split fills the slot to its cap");
        assert_eq!(
            count_at(&menu, 36),
            Some(9),
            "the displaced 5 eggs merge back into the 4 left in the hotbar slot"
        );
        // No new slot should have been used for the overflow.
        for i in 37..45 {
            assert_eq!(count_at(&menu, i), None, "slot {i} must stay empty");
        }
    }

    /// The control: with room to spare (cap not exceeded), the ordinary
    /// no-overflow swap path is unaffected by the reordering above — source and
    /// target simply trade places.
    #[test]
    fn control_hotbar_swap_without_overflow_is_a_plain_exchange() {
        let mut menu = Menu::player();
        menu.set_slot_item(36, Some(stack("minecraft:egg", 10)));
        menu.set_slot_item(9, Some(stack("minecraft:egg", 5)));
        Click::hotbar_swap(9, 0).apply(&mut menu, PlayerCtx::survival());
        assert_eq!(count_at(&menu, 9), Some(10));
        assert_eq!(count_at(&menu, 36), Some(5));
    }

    /// `give_to_player`'s overflow-displacement scan used to be a
    /// plain `0..36` merge-then-fill pass, ignoring vanilla's real priority —
    /// the *selected* hotbar slot first, then the off-hand, only then a linear
    /// scan (vanilla's own slot-with-remaining-space search).
    ///
    /// A torch sits in *both* native 0 (menu slot 36, room for 4) and native 4
    /// (menu slot 40, the *selected* slot, room for 63) when a second torch
    /// stack is displaced from a container slot by an unrelated egg swap.
    /// Vanilla drains the selected slot first and never touches native 0's
    /// torch; the pre-fix scan drained native 0 first because it always
    /// started its linear pass at index 0, regardless of what was selected.
    /// Selected slot is 4, not 0, precisely so a scan that merely "starts from
    /// 0" cannot pass this test by accident (`CLAUDE.md`'s *world*-species
    /// vacuous-test trap). Watched failing pre-fix: native 0 landed at 64 and
    /// native 4 at 2, the old in-order-scan result.
    #[test]
    fn swap_overflow_gives_to_the_selected_hotbar_slot_before_a_lower_index() {
        let mut menu = Menu::player();
        // hotbar key 8 -> native 8 -> menu slot 44: the oversized source.
        menu.set_slot_item(
            44,
            Some(stack("minecraft:egg", 20).with_max_stack_size(16)),
        );
        // Target: main storage slot 9 holds a torch, unrelated to the egg
        // swap, so its displacement exercises `give_to_player`'s ordinary scan
        // rather than the same-item remainder-merge finding 1 already fixed.
        menu.set_slot_item(9, Some(stack("minecraft:torch", 5)));
        // A torch at native 0, almost full: an in-order 0..36 scan drains into
        // this one first.
        menu.set_slot_item(36, Some(stack("minecraft:torch", 60)));
        // A torch at native 4, the *selected* hotbar slot, with plenty of room.
        menu.set_slot_item(40, Some(stack("minecraft:torch", 1)));

        let ctx = PlayerCtx {
            infinite_materials: false,
            can_drop: true,
            selected_hotbar_slot: 4,
            furnace_input_items: None,
        };
        Click::hotbar_swap(9, 8).apply(&mut menu, ctx);

        assert_eq!(
            count_at(&menu, 36),
            Some(60),
            "native 0's torch must be untouched — the selected slot is tried first"
        );
        assert_eq!(
            count_at(&menu, 40),
            Some(6),
            "the displaced 5 torches land in the selected hotbar slot"
        );
    }

    /// The control for the test above: with no *selected* slot preference in
    /// play (slot 0 selected, matching every other test's default), the
    /// linear-scan behaviour is unchanged — the lowest-index mergeable slot
    /// still wins, same as before this fix.
    #[test]
    fn control_swap_overflow_without_a_selected_slot_still_scans_from_zero() {
        let mut menu = Menu::player();
        menu.set_slot_item(
            44,
            Some(stack("minecraft:egg", 20).with_max_stack_size(16)),
        );
        menu.set_slot_item(9, Some(stack("minecraft:torch", 5)));
        menu.set_slot_item(36, Some(stack("minecraft:torch", 60)));
        menu.set_slot_item(40, Some(stack("minecraft:torch", 1)));

        Click::hotbar_swap(9, 8).apply(&mut menu, PlayerCtx::survival());

        assert_eq!(count_at(&menu, 36), Some(64), "native 0 fills first when it is not the selected slot");
        assert_eq!(count_at(&menu, 40), Some(2), "only the 1 remaining torch reaches native 4");
    }

    // --- item-combiner menus: anvil / grindstone / smithing / enchanting ---

    /// [`Menu::item_combiner`]'s result slot is take-only, matching
    /// vanilla's own item-combiner result-slot placement override
    /// — the anvil/grindstone shape
    /// (`container_size = 3, result_slot = 2`).
    #[test]
    fn item_combiner_result_slot_rejects_placement() {
        let menu = Menu::item_combiner(3, 2, SpecialLayout::Anvil);
        assert!(
            !menu.may_place(2, &stack("minecraft:diamond_pickaxe", 1)),
            "the result slot must reject a placed item, matching ItemCombinerMenu"
        );
        // The two input slots are untouched — anvil's own placement predicate
        // accepts anything, so this only proves the
        // result slot is the *one* that changed.
        assert!(menu.may_place(0, &stack("minecraft:diamond_pickaxe", 1)));
        assert!(menu.may_place(1, &stack("minecraft:diamond_pickaxe", 1)));
    }

    /// The smithing table shape: `container_size = 4, result_slot = 3`.
    #[test]
    fn item_combiner_covers_the_smithing_table_shape() {
        let menu = Menu::item_combiner(4, 3, SpecialLayout::Smithing);
        assert!(!menu.may_place(3, &stack("minecraft:netherite_upgrade_smithing_template", 1)));
        assert!(menu.may_place(0, &stack("minecraft:netherite_upgrade_smithing_template", 1)));
        assert!(menu.may_place(1, &stack("minecraft:diamond_pickaxe", 1)));
        assert!(menu.may_place(2, &stack("minecraft:netherite_ingot", 1)));
    }

    /// [`Menu::enchanting_table`]'s slot 1 accepts only lapis lazuli
    /// (vanilla's own enchanting-table lapis-slot restriction); slot 0 (the item to enchant) accepts
    /// anything, matching the plain `Slot` vanilla gives it.
    #[test]
    fn enchanting_table_lapis_slot_rejects_non_lapis() {
        let menu = Menu::enchanting_table();
        assert!(
            !menu.may_place(1, &stack("minecraft:diamond", 1)),
            "the lapis slot must reject a non-lapis item"
        );
        assert!(
            menu.may_place(1, &stack("minecraft:lapis_lazuli", 1)),
            "the lapis slot must accept lapis lazuli"
        );
        assert!(menu.may_place(0, &stack("minecraft:diamond_sword", 1)));
    }

    /// Control for the test above: an *ordinary* generic container never
    /// applies the lapis restriction, so this can only pass because
    /// `enchanting_table` specifically marked slot 1 — not because `may_place`
    /// rejects diamonds everywhere.
    #[test]
    fn control_generic_container_has_no_lapis_restriction() {
        let menu = Menu::generic(2);
        assert!(menu.may_place(1, &stack("minecraft:diamond", 1)));
    }

    /// The anvil and grindstone are mechanically identical (`container_size =
    /// 3, result_slot = 2`) but must carry *different* [`SpecialLayout`]s —
    /// `lodestone-shell`'s `slot_layout` places their three slots at
    /// completely different pixel positions, and `Menu` has no other field that could
    /// tell them apart.
    #[test]
    fn special_layout_distinguishes_menus_with_identical_mechanics() {
        let anvil = Menu::item_combiner(3, 2, SpecialLayout::Anvil);
        let grindstone = Menu::item_combiner(3, 2, SpecialLayout::Grindstone);
        assert_eq!(anvil.special_layout(), Some(SpecialLayout::Anvil));
        assert_eq!(grindstone.special_layout(), Some(SpecialLayout::Grindstone));
        assert_eq!(Menu::enchanting_table().special_layout(), Some(SpecialLayout::Enchanting));
        assert_eq!(
            Menu::generic(3).special_layout(),
            None,
            "an ordinary generic container has no special layout"
        );
    }

    // -- plan_recipe_auto_fill ------------------------------

    /// A crafting table's 3×3: coal at menu slot 12, a stick at menu slot 20
    /// (both inside the `10..=36` main-storage range this menu reports —
    /// see the module's own slot-order table), recipe wants coal above stick
    /// in a **1-wide** pattern placed into the real **3-wide** grid.
    /// `Recipe::placement` lays that pattern out row-major against the
    /// grid's own width, so coal (row 0, col 0) is grid cell `0` and stick
    /// (row 1, col 0) is grid cell `3` — **not** cell `1`, which is what a
    /// hand-count that forgot the grid is 3 cells per row, not 1, would
    /// predict. `craft.first_input == 1` then offsets both: menu slots `1`
    /// and `4`.
    #[test]
    fn plan_recipe_auto_fill_offsets_crafting_table_cells_by_first_input() {
        let mut menu = Menu::crafting(3, 3);
        menu.set_slot_item(12, Some(stack("minecraft:coal", 5)));
        menu.set_slot_item(20, Some(stack("minecraft:stick", 3)));
        let torch = crate::recipe::Recipe::Shaped(crate::recipe::ShapedRecipe::new(
            1,
            2,
            vec![
                Some(crate::recipe::Ingredient::Item(id("minecraft:coal"))),
                Some(crate::recipe::Ingredient::Item(id("minecraft:stick"))),
            ],
            stack("minecraft:torch", 4),
        ));
        let tags = crate::recipe::TagResolver::new();
        let plan = menu
            .plan_recipe_auto_fill(&torch, &tags)
            .expect("both ingredients present in main storage");
        assert_eq!(
            plan,
            vec![
                crate::recipe::PlacementStep { cell: 1, source_slot: 12 },
                crate::recipe::PlacementStep { cell: 4, source_slot: 20 },
            ]
        );
    }

    /// A furnace-family menu has no `craft_layout` at all — its single
    /// ingredient slot is menu index `0`, reached through
    /// `special_layout` instead. Predicts `cell: 0` unmodified (no offset to
    /// apply, unlike the crafting-table case above).
    #[test]
    fn plan_recipe_auto_fill_targets_furnace_ingredient_slot_zero() {
        let mut menu = Menu::furnace(SpecialLayout::Furnace);
        // Main storage starts at `container_size == 3`.
        menu.set_slot_item(15, Some(stack("minecraft:porkchop", 8)));
        let smelting = crate::recipe::Recipe::Cooking(crate::recipe::CookingRecipe {
            kind: crate::recipe::CookingKind::Smelting,
            ingredient: crate::recipe::Ingredient::Item(id("minecraft:porkchop")),
            result: stack("minecraft:cooked_porkchop", 1),
            experience: 0.35,
            cooking_time: 200,
            category: crate::recipe::RecipeCategory::Food,
        });
        let tags = crate::recipe::TagResolver::new();
        let plan = menu.plan_recipe_auto_fill(&smelting, &tags).expect("porkchop is in main storage");
        assert_eq!(plan, vec![crate::recipe::PlacementStep { cell: 0, source_slot: 15 }]);
    }

    /// A blast-furnace recipe never matches a plain furnace's ingredient
    /// slot: `CookingRecipe::placement` only returns `Some` for `(1, 1)`, and
    /// `Recipe::book_type` distinguishes furnace/blast-furnace/smoker, but
    /// `plan_recipe_auto_fill` does not itself check the kind matches the
    /// menu — this pins that a *smoking*-only ingredient (raw chicken, not
    /// modelled as smeltable here) with no matching inventory item still
    /// correctly returns `None` via `plan_auto_fill`'s own all-or-nothing
    /// rule, rather than silently placing the wrong thing.
    #[test]
    fn plan_recipe_auto_fill_returns_none_when_ingredient_is_absent() {
        let menu = Menu::furnace(SpecialLayout::Furnace);
        let smelting = crate::recipe::Recipe::Cooking(crate::recipe::CookingRecipe {
            kind: crate::recipe::CookingKind::Smelting,
            ingredient: crate::recipe::Ingredient::Item(id("minecraft:iron_ore")),
            result: stack("minecraft:iron_ingot", 1),
            experience: 0.7,
            cooking_time: 200,
            category: crate::recipe::RecipeCategory::Blocks,
        });
        let tags = crate::recipe::TagResolver::new();
        assert_eq!(menu.plan_recipe_auto_fill(&smelting, &tags), None);
    }

    /// A menu with neither a crafting grid nor a furnace-family
    /// `special_layout` (a plain chest) has nothing to auto-fill at all.
    #[test]
    fn plan_recipe_auto_fill_none_for_a_menu_with_no_grid() {
        let mut menu = Menu::generic(27);
        menu.set_slot_item(30, Some(stack("minecraft:coal", 5)));
        let torch = crate::recipe::Recipe::Shaped(crate::recipe::ShapedRecipe::new(
            1,
            1,
            vec![Some(crate::recipe::Ingredient::Item(id("minecraft:coal")))],
            stack("minecraft:torch", 4),
        ));
        let tags = crate::recipe::TagResolver::new();
        assert_eq!(menu.plan_recipe_auto_fill(&torch, &tags), None);
    }

    /// Auto-fill never draws from armour or off-hand, even when they hold a
    /// matching item — vanilla's own placement helper only ever walks
    /// the player's main+hotbar list. The player-inventory screen's 2×2
    /// puts armour at menu slots `5..=8`; a coal "helmet" placed there must
    /// be invisible to the planner.
    #[test]
    fn plan_recipe_auto_fill_never_draws_from_armour_or_offhand() {
        let mut menu = Menu::player();
        menu.set_slot_item(5, Some(stack("minecraft:coal", 1))); // armour range
        menu.set_slot_item(45, Some(stack("minecraft:coal", 1))); // off-hand
        let torch = crate::recipe::Recipe::Shapeless(crate::recipe::ShapelessRecipe::new(
            vec![crate::recipe::Ingredient::Item(id("minecraft:coal"))],
            stack("minecraft:torch", 4),
        ));
        let tags = crate::recipe::TagResolver::new();
        assert_eq!(menu.plan_recipe_auto_fill(&torch, &tags), None);
    }
