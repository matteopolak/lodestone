//! Container interaction packets: slot clicks, workstation and enchanting clicks, button clicks, renames, books, beacons, recipe placement and item drops.

use super::*;

/// Applies a `CONTAINER_CLICK` by **deriving** its result server-side
/// (`ServerBound::ContainerClicked`).
///
/// The click's slot/button/click-type go into [`crate::container_click::do_click`],
/// vanilla's own container-menu do-click routine, run over the menu read out of
/// this connection's real state. The client's `changed_slots`/`carried_item`
/// prediction is **never stored** — it is compared against what was derived, and a
/// disagreement sends a full corrective `container_set_content`. So an honest
/// client sees no extra traffic and a client naming an item it does not own is
/// corrected on the same packet.
///
/// The server derives the full menu result instead of trusting the client's
/// claimed diff, so a client cannot mint an item by naming an arbitrary slot.
/// The comparison covers crafting results as well as ordinary menu slots.
///
/// A click against a non-zero `window_id` that does not match the connection's
/// own tracked [`OpenContainer`] (a stale click for a window since closed or
/// replaced) is dropped rather than misapplied to whatever is open now.
///
/// Three menu shapes are served, and which one this is comes from the tracked
/// window rather than from the packet: window `0` is the player screen, an open
/// crafting table is [`MenuKind::CraftingTable`], anything else is a block-entity
/// container.
///
/// Returns the correcting directive to send (if any) and the stacks that left the
/// menu into the world (a throw, or a click outside the window) for the caller to
/// spawn. A directive rather than a send, so this stays a pure function of the
/// click and the unit tests below drive it with no connection.
#[allow(clippy::too_many_arguments)]
pub(super) fn apply_container_clicked<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    block_entities: &BlockEntityHandle,
    open_container: Option<&mut OpenContainer>,
    window_id: i32,
    click: Click,
    claimed_slots: &[(i32, Option<ItemStack>)],
    claimed_cursor: Option<&ItemStack>,
    creative: bool,
    xp_level: i32,
    // The narrow crafting-station hook registry — see
    // `apply_use_item_on`'s own `hooks` comment for why this is a targeted
    // handle rather than the whole `WorldStateHandle`.
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> (Option<ServerDirective>, Vec<ItemStack>) {
    // Which menu, and where its non-player slots live.
    let mut open = open_container;

    // Lectern slot zero is a read-only display. It is intentionally handled
    // before the generic click state machine, whose `Container` slot kind is
    // otherwise placeable. A forged click receives the authoritative one-slot
    // content rather than being allowed to write arbitrary items into the
    // block entity.
    if window_id != 0
        && open
            .as_ref()
            .is_some_and(|tracked| tracked.window_id == window_id && tracked.shape == MenuKind::Lectern)
    {
        let tracked = open.as_mut().expect("lectern predicate checked Some");
        let own = block_entities.with(|reg| {
            reg.get(tracked.pos)
                .map(BlockEntity::container_slots)
                .unwrap_or_default()
        });
        let items = read_menu(&MenuLayout::lectern(), inventory, None, &own);
        let state_id = tracked.next_state_id();
        return (
            Some(proto.encode_container_content(
                tracked.window_id,
                state_id,
                &items,
                inventory.click_state().carried.as_ref(),
            )),
            Vec::new(),
        );
    }

    // The workstation economy (anvil/grindstone/smithing) is a
    // second positionless-scratch shape alongside the crafting table, but its
    // cells live in `PlayerInventory::workstation` (a flat cell vector) rather
    // than a `CraftingState`, so it is handled by a dedicated function instead
    // of forcing it through `read_menu`'s `CraftingState`-shaped grid.
    if window_id != 0 {
        let combiner_station = open.as_ref().and_then(|tracked| {
            (tracked.window_id == window_id)
                .then_some(tracked.shape)
                .and_then(|shape| match shape {
                    MenuKind::ItemCombiner { station, .. } => Some(station),
                    _ => None,
                })
        });
        if let Some(station) = combiner_station {
            let tracked = open.expect("checked Some above via combiner_station");
            return apply_workstation_clicked(
                proto,
                inventory,
                tracked,
                click,
                claimed_slots,
                claimed_cursor,
                creative,
                station,
                xp_level,
                hooks,
            );
        }
        let is_enchanting = open
            .as_ref()
            .is_some_and(|tracked| tracked.window_id == window_id && tracked.shape == MenuKind::Enchanting);
        if is_enchanting {
            let tracked = open.expect("checked Some above via is_enchanting");
            return apply_enchanting_clicked(proto, inventory, tracked, click, claimed_slots, claimed_cursor, creative);
        }
    }

    let (layout, pos, uses_table_grid) = if window_id == 0 {
        (MenuLayout::player(), None, false)
    } else {
        let Some(tracked) = open.as_mut() else {
            return (None, Vec::new());
        };
        if tracked.window_id != window_id {
            return (None, Vec::new());
        }
        match tracked.shape {
            MenuKind::CraftingTable => (MenuLayout::crafting_table(), Some(tracked.pos), true),
            MenuKind::Lectern => (MenuLayout::lectern(), Some(tracked.pos), false),
            _ => (
                MenuLayout::container(tracked.container_size),
                Some(tracked.pos),
                false,
            ),
        }
    };

    let own = match (pos, uses_table_grid) {
        (Some(pos), false) => block_entities.with(|reg| {
            reg.get(pos)
                .map(BlockEntity::container_slots)
                .unwrap_or_default()
        }),
        _ => Vec::new(),
    };
    let grid_owner = if uses_table_grid {
        inventory.table_crafting().cloned()
    } else if window_id == 0 {
        Some(inventory.crafting().clone())
    } else {
        None
    };

    let mut slots = read_menu(&layout, inventory, grid_owner.as_ref(), &own);
    // The state the client saw when this menu was last sent is the baseline
    // for the agreement check below. Every disagreement receives a full
    // content packet.
    let before = slots.clone();
    let mut state = inventory.click_state().clone();
    // The open grid's dimensions, so `do_click_with` can re-derive the result slot
    // mid-click (`slotsChanged`) — which is what makes a shift-click on the result
    // craft repeatedly instead of once.
    let (grid_width, grid_height) = grid_owner
        .as_ref()
        .map_or((0, 0), |grid| (grid.width(), grid.height()));
    let recipe = |cells: &[Option<ItemStack>]| {
        crate::crafting::derive_result(grid_width, grid_height, cells)
    };
    // The last nested-item selection for this menu slot. The right-click
    // extraction branch reads it to choose which nested item comes out; the
    // following pickup click performs the extraction.
    let selected_bundle = |slot: usize| {
        MenuSlot::from_index(slot)
            .and_then(|slot| inventory.selected_bundle_item(slot))
            .map(BundleItemSlot::index)
    };
    let selected_bundle: Option<SelectedBundleIndex<'_>> = Some(&selected_bundle);
    let dropped = do_click_with(
        &layout,
        &mut slots,
        &mut state,
        click,
        creative,
        Some(&recipe),
        // `Player`/`Container`/`CraftingTable` layouts have no `mayPickup`
        // override anywhere in vanilla — only `ItemCombinerMenu`'s result
        // slot does, and that shape is handled by `apply_workstation_clicked`
        // above, never reaching here.
        None,
        selected_bundle,
    );
    *inventory.click_state_mut() = state;

    // Write back. Grid cells go last and through `set_input`, so the result slot
    // is re-derived from the grid rather than copied out of `slots` — a stale
    // result is the same defect as a trusted one.
    let mut grid_writes: Vec<(usize, Option<ItemStack>)> = Vec::new();
    let mut own_writes: Vec<(usize, Option<ItemStack>)> = Vec::new();
    for (index, kind) in layout.iter() {
        match kind {
            SlotKind::Player(native) => inventory.set_native(native, slots[index].clone()),
            SlotKind::Container(own_index) => own_writes.push((own_index, slots[index].clone())),
            SlotKind::Grid(cell) => grid_writes.push((cell, slots[index].clone())),
            SlotKind::Result => {}
        }
    }
    if let Some(pos) = pos.filter(|_| !own_writes.is_empty()) {
        block_entities.with(|reg| {
            if let Some(entity) = reg.get_mut(pos) {
                for (index, item) in &own_writes {
                    if let Some(index) = entity.container_slot(*index) {
                        entity.set_container_slot_at(index, item.clone());
                    }
                }
            }
        });
    }
    if !grid_writes.is_empty() {
        let grid = if uses_table_grid {
            inventory.table_crafting_mut()
        } else {
            Some(inventory.crafting_mut())
        };
        if let Some(grid) = grid {
            for (cell, item) in grid_writes {
                grid.set_input(cell, item);
            }
        }
    }

    // Re-read, so the comparison and the correction both carry the *derived*
    // result rather than whatever `do_click` left in the result slot.
    let own = match (pos, uses_table_grid) {
        (Some(pos), false) => block_entities.with(|reg| {
            reg.get(pos)
                .map(BlockEntity::container_slots)
                .unwrap_or_default()
        }),
        _ => Vec::new(),
    };
    let grid_owner = if uses_table_grid {
        inventory.table_crafting().cloned()
    } else if window_id == 0 {
        Some(inventory.crafting().clone())
    } else {
        None
    };
    let derived = read_menu(&layout, inventory, grid_owner.as_ref(), &own);

    // Did the client end up believing what the server derived? The client's belief is
    // **the pre-click state overwritten by the slots it claimed** — it does not claim
    // slots it thinks are unchanged — plus its claimed cursor.
    //
    // # Compare claims with the full derived menu
    //
    // The client can omit slots it cannot predict, especially a derived crafting
    // result. Compare its claimed slots against the full derived menu so the
    // result and any shifted inputs are corrected in the same response.
    //
    // A matching prediction needs no corrective packet; the no-traffic test
    // exercises that no-traffic branch.
    let cursor = inventory.click_state().carried.clone();
    let mut agrees = cursor.as_ref() == claimed_cursor;
    if agrees {
        let mut believed = before;
        for (menu_slot, claimed) in claimed_slots {
            match usize::try_from(*menu_slot).ok().filter(|i| *i < believed.len()) {
                Some(index) => believed[index] = claimed.clone(),
                // A claim naming a slot this menu does not have is itself a
                // disagreement: it cannot be reconciled, so correct the client.
                None => {
                    agrees = false;
                    break;
                }
            }
        }
        agrees = agrees && believed == derived;
    }
    if agrees {
        return (None, dropped);
    }

    let state_id = match open.as_mut() {
        Some(tracked) => tracked.next_state_id(),
        None => 0,
    };
    (
        Some(proto.encode_container_content(window_id, state_id, &derived, cursor.as_ref())),
        dropped,
    )
}

/// Reads one [`MenuKind::ItemCombiner`] menu's full slot vector — the
/// workstation cells, the player tail, and the live result derived from
/// [`workstation_result`] (never stored; always re-derived, the same
/// "recompute rather than cache" choice `crate::crafting`'s recipe closure
/// makes).
pub(super) fn read_workstation_menu(
    layout: &MenuLayout,
    inventory: &PlayerInventory,
    cells: &[Option<ItemStack>],
    station: Station,
    creative: bool,
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> Vec<Option<ItemStack>> {
    let result = workstation_result(
        station,
        cells,
        creative,
        inventory.pending_rename(),
        inventory.selected_recipe_index(),
        hooks,
    );
    layout
        .iter()
        .map(|(_, kind)| match kind {
            SlotKind::Player(native) => inventory.native(native).cloned(),
            SlotKind::Container(_) => None,
            SlotKind::Grid(cell) => cells.get(cell).cloned().flatten(),
            SlotKind::Result => result.clone(),
        })
        .collect()
}

/// One station's result from its own input cells — [`crate::anvil::compute`],
/// [`crate::anvil::grindstone_result`], [`crate::smithing::compute`],
/// [`crate::loom::result`] or [`crate::stonecutting::result`]. `rename` is
/// the anvil's pending typed name ([`PlayerInventory::pending_rename`]);
/// `selected` is the loom/stonecutter's chosen offer index
/// ([`PlayerInventory::selected_recipe_index`]) — every other station
/// ignores whichever of the two it does not use, the same "the other
/// stations ignore it" shape `rename` already had before `selected` existed.
///
/// `hooks` is the plugin seam: the result computed above is
/// the *input* to [`CraftingStationHooks::evaluate`], never the final
/// answer, so a plugin can allow, deny or replace it — see
/// `crate::plugin_crafting`'s own module doc for why this single function is
/// the right choke point.
pub(super) fn workstation_result(
    station: Station,
    cells: &[Option<ItemStack>],
    creative: bool,
    rename: Option<&str>,
    selected: Option<i32>,
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> Option<ItemStack> {
    let get = |i: usize| cells.get(i).and_then(Option::as_ref);
    let computed = match station {
        Station::Anvil => crate::anvil::compute(get(0), get(1), rename, creative).result,
        Station::Grindstone => crate::anvil::grindstone_result(get(0), get(1)),
        Station::Smithing => crate::smithing::compute(get(0), get(1), get(2)),
        Station::Loom => crate::loom::result(get(0), get(1), get(2), selected),
        Station::Stonecutter => crate::stonecutting::result(get(0), selected),
    };
    if hooks.is_empty() {
        // The common, zero-plugin case: skip building `StationInputs` (which
        // would otherwise clone every input cell on every menu read) at all.
        return computed;
    }
    let inputs = crate::plugin_crafting::StationInputs {
        station,
        cells: cells.to_vec(),
        computed: computed.clone(),
    };
    hooks.evaluate(&inputs, computed)
}

/// [`apply_container_clicked`]'s `MenuKind::ItemCombiner` branch: the anvil,
/// grindstone and smithing table all share this shape (`docs/workstation-economy.md`),
/// differing only in [`workstation_result`] (what the result slot shows) and
/// [`crate::container_click`]'s own per-station `may_place`/take rules. Kept as
/// a separate function rather than folded into `apply_container_clicked`
/// because the grid source is [`PlayerInventory::workstation`] (a flat cell
/// vector) rather than a [`crate::crafting::CraftingState`], so it cannot reuse
/// `read_menu`.
///
/// **XP is charged here**, not in [`crate::container_click`] — that module is
/// deliberately economy-free (see its own module doc). A take is detected the
/// same way the crafting-table path detects a craft: by comparing the result
/// cell before and after the click, since [`crate::container_click::do_click_with`]
/// already ran the whole click (including any take) by the time this reads
/// `slots` back.
pub(super) fn apply_workstation_clicked<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    tracked: &mut OpenContainer,
    click: Click,
    claimed_slots: &[(i32, Option<ItemStack>)],
    claimed_cursor: Option<&ItemStack>,
    creative: bool,
    station: Station,
    xp_level: i32,
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> (Option<ServerDirective>, Vec<ItemStack>) {
    let layout = MenuLayout::item_combiner(station);
    let cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let rename = inventory.pending_rename().map(str::to_owned);
    let selected_recipe_index = inventory.selected_recipe_index();
    let mut slots = read_workstation_menu(&layout, inventory, &cells, station, creative, hooks);
    let before = slots.clone();
    let mut state = inventory.click_state().clone();
    let recipe = |grid_cells: &[Option<ItemStack>]| {
        workstation_result(station, grid_cells, creative, rename.as_deref(), selected_recipe_index, hooks)
    };
    // The anvil-menu may-pickup gate: `(creative || experience_level >= cost) && cost > 0`.
    // `cost` is `crate::anvil::compute`'s own field, re-derived
    // from the pre-click cells and pending rename — never stored, the same
    // "recompute rather than cache" choice `workstation_result` above already
    // makes. `Grindstone`/`Smithing` pass `None`: neither result slot changes
    // this permission, so both retain the default allow-pickup behavior.
    let anvil_cost = crate::anvil::compute(
        cells.first().and_then(Option::as_ref),
        cells.get(1).and_then(Option::as_ref),
        rename.as_deref(),
        creative,
    )
    .cost;
    let anvil_may_pickup = move |_index: usize, _item: &ItemStack| (creative || xp_level >= anvil_cost) && anvil_cost > 0;
    let may_pickup: Option<MayPickup<'_>> = (station == Station::Anvil).then_some(&anvil_may_pickup as _);
    let dropped = do_click_with(
        &layout, &mut slots, &mut state, click, creative, Some(&recipe), may_pickup, None,
    );
    *inventory.click_state_mut() = state;

    let mut new_cells = cells.clone();
    for (index, kind) in layout.iter() {
        match kind {
            SlotKind::Player(native) => inventory.set_native(native, slots[index].clone()),
            SlotKind::Grid(cell) => {
                if let Some(slot) = new_cells.get_mut(cell) {
                    *slot = slots[index].clone();
                }
            }
            SlotKind::Container(_) | SlotKind::Result => {}
        }
    }
    // `container_click::take_result`'s own anvil branch re-derives the
    // outcome with `item_name: None` (that module is deliberately
    // economy/rename-free) purely to read `only_renaming`/
    // `repair_item_count_cost`, which is safe for every case except a take
    // priced *entirely* by a pending rename: seen with no name, that
    // evaluation returns `price <= 0` and takes the "nothing to combine"
    // early exit, so `only_renaming` comes back `false` and the addition
    // cell is wrongly cleared as if a real combine had consumed it. Correct
    // it here, where the real rename text is available — a no-op unless
    // this exact click just took such a result (cell 0 went from occupied to
    // empty).
    if station == Station::Anvil {
        let had_input = cells.first().cloned().flatten();
        let took_input = new_cells.first().is_some_and(Option::is_none);
        if let (Some(input), true) = (had_input, took_input) {
            let addition = cells.get(1).cloned().flatten();
            if let Some(addition_item) = addition.clone() {
                let outcome = crate::anvil::compute(Some(&input), Some(&addition_item), rename.as_deref(), creative);
                if outcome.result.is_some() && outcome.only_renaming && outcome.repair_item_count_cost == 0 {
                    if let Some(slot) = new_cells.get_mut(1) {
                        *slot = addition;
                    }
                }
            }
        }
    }
    if let Some(ws) = inventory.workstation_mut() {
        *ws = new_cells.clone();
    }

    let derived = read_workstation_menu(&layout, inventory, &new_cells, station, creative, hooks);

    let cursor = inventory.click_state().carried.clone();
    let mut agrees = cursor.as_ref() == claimed_cursor;
    if agrees {
        let mut believed = before;
        for (menu_slot, claimed) in claimed_slots {
            match usize::try_from(*menu_slot).ok().filter(|i| *i < believed.len()) {
                Some(index) => believed[index] = claimed.clone(),
                None => {
                    agrees = false;
                    break;
                }
            }
        }
        agrees = agrees && believed == derived;
    }
    if agrees {
        return (None, dropped);
    }
    let state_id = tracked.next_state_id();
    (
        Some(proto.encode_container_content(tracked.window_id, state_id, &derived, cursor.as_ref())),
        dropped,
    )
}

/// [`apply_container_clicked`]'s `MenuKind::Enchanting` branch. No result slot
/// and no take, so there is no economy to charge here at all — see
/// `crate::enchanting`'s own module doc for why the "choose an offer" action
/// (`ClientAction::ContainerButtonClick`) cannot reach this crate yet. This
/// only has to keep the two cells (item, lapis) in sync with clicks; the three
/// `container_set_data` costs are **not** recomputed live here — see
/// `docs/workstation-economy.md` for that scope note.
pub(super) fn apply_enchanting_clicked<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    tracked: &mut OpenContainer,
    click: Click,
    claimed_slots: &[(i32, Option<ItemStack>)],
    claimed_cursor: Option<&ItemStack>,
    creative: bool,
) -> (Option<ServerDirective>, Vec<ItemStack>) {
    let layout = MenuLayout::enchanting_table();
    let cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let read = |inv: &PlayerInventory, cells: &[Option<ItemStack>]| -> Vec<Option<ItemStack>> {
        layout
            .iter()
            .map(|(_, kind)| match kind {
                SlotKind::Player(native) => inv.native(native).cloned(),
                SlotKind::Grid(cell) => cells.get(cell).cloned().flatten(),
                SlotKind::Container(_) | SlotKind::Result => None,
            })
            .collect()
    };
    let mut slots = read(inventory, &cells);
    let before = slots.clone();
    let mut state = inventory.click_state().clone();
    let dropped = do_click_with(&layout, &mut slots, &mut state, click, creative, None, None, None);
    *inventory.click_state_mut() = state;

    let mut new_cells = cells;
    for (index, kind) in layout.iter() {
        match kind {
            SlotKind::Player(native) => inventory.set_native(native, slots[index].clone()),
            SlotKind::Grid(cell) => {
                if let Some(slot) = new_cells.get_mut(cell) {
                    *slot = slots[index].clone();
                }
            }
            SlotKind::Container(_) | SlotKind::Result => {}
        }
    }
    if let Some(ws) = inventory.workstation_mut() {
        *ws = new_cells.clone();
    }
    let derived = read(inventory, &new_cells);

    let cursor = inventory.click_state().carried.clone();
    let mut agrees = cursor.as_ref() == claimed_cursor;
    if agrees {
        let mut believed = before;
        for (menu_slot, claimed) in claimed_slots {
            match usize::try_from(*menu_slot).ok().filter(|i| *i < believed.len()) {
                Some(index) => believed[index] = claimed.clone(),
                None => {
                    agrees = false;
                    break;
                }
            }
        }
        agrees = agrees && believed == derived;
    }
    if agrees {
        return (None, dropped);
    }
    let state_id = tracked.next_state_id();
    (
        Some(proto.encode_container_content(tracked.window_id, state_id, &derived, cursor.as_ref())),
        dropped,
    )
}

/// [`ServerBound::RenameItem`]'s consumer — vanilla's own anvil-menu
/// item-name setter, reached
/// the same way its own rename-item handler gates it:
/// only when an anvil is currently open (no `window_id` on the wire to check
/// further — the real packet does not carry one either).
///
/// Returns the directives to resend (the refreshed content, then the
/// `cost` data slot — vanilla's own anvil-menu single `DataSlot`) once the rename
/// actually changed something; `Vec::new()` for a rejected/no-op rename or
/// when no anvil is open, matching `setItemName`'s own `validatedName !=
/// this.itemName` early return.
pub(super) fn apply_rename_item<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    tracked: Option<&mut OpenContainer>,
    name: &str,
    creative: bool,
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> Vec<ServerDirective> {
    let Some(tracked) = tracked else { return Vec::new() };
    if !matches!(tracked.shape, MenuKind::ItemCombiner { station: Station::Anvil, .. }) {
        return Vec::new();
    }
    let Some(validated) = crate::anvil::validate_rename(name) else {
        return Vec::new();
    };
    if inventory.pending_rename() == Some(validated.as_str()) {
        return Vec::new();
    }
    inventory.set_pending_rename(Some(validated));

    let cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let outcome = crate::anvil::compute(
        cells.first().and_then(Option::as_ref),
        cells.get(1).and_then(Option::as_ref),
        inventory.pending_rename(),
        creative,
    );
    let layout = MenuLayout::item_combiner(Station::Anvil);
    let items = read_workstation_menu(&layout, inventory, &cells, Station::Anvil, creative, hooks);
    let state_id = tracked.next_state_id();
    vec![
        proto.encode_container_content(tracked.window_id, state_id, &items, inventory.click_state().carried.as_ref()),
        // The "see the 1-XP rename cost" half `docs/workstation-economy.md`
        // named as the actually-missing piece.
        proto.encode_container_data(tracked.window_id, 0, outcome.cost),
    ]
}

/// [`ServerBound::EditBook`]'s consumer. Only hotbar and off-hand slots are
/// accepted, and the selected item must be a `minecraft:writable_book` carrying
/// its writable-book content marker. The decoded page and title limits are
/// enforced by the protocol layer.
///
/// Returns the native slot written and the replacement item for a
/// `CONTAINER_SET_SLOT` update. Returns `None` when validation fails.
pub(super) fn apply_edit_book(
    inventory: &mut PlayerInventory,
    slot: i32,
    pages: Vec<String>,
    title: Option<String>,
    author: &str,
) -> Option<(usize, ItemStack)> {
    let native = usize::try_from(slot).ok()?;
    if !(native < usize::from(HOTBAR_SIZE) || native == OFFHAND_NATIVE) {
        return None;
    }
    let mut item = inventory.native(native)?.clone();
    if item.item.path() != "writable_book" {
        return None;
    }
    match title {
        // A submitted title converts the draft to a written book with
        // generation `0` and resolved text.
        Some(title) => {
            item.item = "minecraft:written_book".parse().ok()?;
            item.components.writable_book_content = None;
            item.components.written_book_content = Some(WrittenBookContent {
                title,
                author: author.to_owned(),
                generation: 0,
                pages: pages.into_iter().map(Text::literal).collect(),
                resolved: true,
            });
        }
        // Without a title, replace the draft pages in place.
        None => {
            item.components.writable_book_content = Some(pages);
        }
    }
    inventory.set_native(native, Some(item.clone()));
    Some((native, item))
}

/// [`ServerBound::SetBeacon`]'s consumer — vanilla's own beacon-menu
/// update-effects routine, reached the same way its own set-beacon-packet handler gates it: only while a
/// beacon is currently open (vanilla's own `containerMenu instanceof
/// BeaconMenu` check).
///
/// `levels` is **not** re-derived here — vanilla's own beacon-menu levels getter reads the
/// block entity's own tracked field, last refreshed when the menu opened
/// (see `BeaconData::levels`'s own doc), the same snapshot vanilla's real
/// `ContainerData` would hold between its own 80-tick background
/// recomputes.
///
/// Returns the directives to resend (the refreshed payment slot, then all
/// three data values) once the submission actually changed something, or
/// `Vec::new()` for a refused one — no payment item, or
/// `crate::beacon::validate_beacon_effects` refuses the pair. Vanilla
/// disconnects the client on a refusal (`handleSetBeaconPacket`'s own
/// `this.disconnect(...)`); this crate instead treats it as a malformed
/// packet whose effect is dropped rather than the connection, the same
/// convention `PlayerInventory::set_selected_hotbar_slot`'s own doc already
/// states for an out-of-range packet field.
pub(super) fn apply_set_beacon<P: ServerProtocol>(
    proto: &P,
    block_entities: &BlockEntityHandle,
    tracked: Option<&mut OpenContainer>,
    primary: Option<String>,
    secondary: Option<String>,
) -> Vec<ServerDirective> {
    let Some(tracked) = tracked else { return Vec::new() };
    if tracked.shape != MenuKind::Beacon {
        return Vec::new();
    }
    let primary = match primary {
        Some(key) => match crate::beacon::BeaconPower::from_key(&key) {
            Some(power) => Some(power),
            None => return Vec::new(),
        },
        None => None,
    };
    let secondary = match secondary {
        Some(key) => match crate::beacon::BeaconPower::from_key(&key) {
            Some(power) => Some(power),
            None => return Vec::new(),
        },
        None => None,
    };
    let pos = tracked.pos;
    let updated = block_entities.with(|reg| {
        let Some(BlockEntity::Beacon(beacon)) = reg.get_mut(pos) else {
            return None;
        };
        beacon.payment.as_ref()?;
        if !crate::beacon::validate_beacon_effects(primary, secondary, beacon.levels) {
            return None;
        }
        beacon.primary_effect = primary;
        beacon.secondary_effect = secondary;
        // Remove one item from the payment slot.
        let consumed_all = beacon.payment.as_ref().is_some_and(|item| item.count <= 1);
        if consumed_all {
            beacon.payment = None;
        } else if let Some(payment) = &mut beacon.payment {
            payment.count -= 1;
        }
        Some((
            beacon.levels,
            beacon.primary_effect.clone(),
            beacon.secondary_effect.clone(),
            beacon.payment.clone(),
        ))
    });
    let Some((levels, primary, secondary, payment)) = updated else {
        return Vec::new();
    };
    let state_id = tracked.next_state_id();
    vec![
        proto.encode_container_slot(tracked.window_id, state_id, 0, payment.as_ref()),
        proto.encode_container_data(tracked.window_id, 0, i32::from(levels)),
        proto.encode_container_data(
            tracked.window_id,
            1,
            crate::beacon::encode_beacon_effect(primary),
        ),
        proto.encode_container_data(
            tracked.window_id,
            2,
            crate::beacon::encode_beacon_effect(secondary),
        ),
    ]
}

/// Applies one lectern reader button against the block entity that owns the
/// open window. Page changes are data-only updates (the book remains in slot
/// zero); taking the book clears the authoritative slot and resets the page,
/// then adds the exact stack to the player's inventory. A full inventory
/// refuses the take, so the server never loses a book it cannot deliver.
pub(super) fn apply_lectern_button_click<P: ServerProtocol>(
    proto: &P,
    block_entities: &BlockEntityHandle,
    inventory: &mut PlayerInventory,
    tracked: Option<&mut OpenContainer>,
    window_id: i32,
    button_id: i32,
    can_take: bool,
) -> Vec<ServerDirective> {
    let Some(tracked) = tracked else { return Vec::new() };
    if tracked.window_id != window_id || tracked.shape != MenuKind::Lectern {
        return Vec::new();
    }
    let pos = tracked.pos;
    let Some((book, current_page)) = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Lectern(lectern)) => lectern.book.clone().map(|book| (book, lectern.page)),
        _ => None,
    }) else {
        return Vec::new();
    };
    let page_count = book
        .components
        .written_book_content
        .as_ref()
        .map_or(0, |content| content.pages.len())
        .max(book.components.writable_book_content.as_ref().map_or(0, Vec::len));
    let page_count = page_count.max(1);
    let current_page = current_page
        .max(0)
        .min(i32::try_from(page_count - 1).unwrap_or(i32::MAX));

    let next_page = match button_id {
        1 => current_page.saturating_sub(1),
        2 => current_page.saturating_add(1).min(i32::try_from(page_count - 1).unwrap_or(i32::MAX)),
        id if id >= 100 => (id - 100).max(0).min(i32::try_from(page_count - 1).unwrap_or(i32::MAX)),
        3 => {
            if !can_take {
                return Vec::new();
            }
            // The lectern holds one book. `add` therefore either accepts it
            // in full or returns it untouched; restore the authoritative slot
            // if an inventory cannot accept it.
            let before_inventory = inventory.clone();
            if inventory.add(book.clone()).1.is_some() {
                return Vec::new();
            }
            let changed = block_entities.with(|reg| {
                let Some(BlockEntity::Lectern(lectern)) = reg.get_mut(pos) else {
                    return false;
                };
                lectern.book = None;
                lectern.page = 0;
                true
            });
            if !changed {
                // This should only be reachable if another world operation
                // removed the block entity between the snapshot and write.
                // Put the book back rather than deleting the player's item.
                let _ = inventory.take_matching(|item| item == &book);
                return Vec::new();
            }
            let mut directives = Vec::new();
            // The book can land in any player-storage slot, not necessarily
            // the selected hotbar slot. Publish each changed native slot in
            // window-0 menu coordinates so the visible inventory agrees with
            // the server immediately after the lectern action.
            for native in 0..crate::inventory::PLAYER_NATIVE_SIZE {
                if before_inventory.native(native) != inventory.native(native)
                    && let Some(menu_slot) = window_zero_menu_slot(native)
                {
                    directives.push(proto.encode_container_slot(
                        0,
                        0,
                        menu_slot,
                        inventory.native(native),
                    ));
                }
            }
            let state_id = tracked.next_state_id();
            directives.extend([
                proto.encode_container_slot(tracked.window_id, state_id, 0, None),
                proto.encode_container_data(tracked.window_id, 0, 0),
            ]);
            return directives;
        }
        _ => return Vec::new(),
    };
    if next_page == current_page {
        return Vec::new();
    }
    block_entities.with(|reg| {
        if let Some(BlockEntity::Lectern(lectern)) = reg.get_mut(pos) {
            lectern.page = next_page;
        }
    });
    vec![proto.encode_container_data(tracked.window_id, 0, next_page)]
}

/// [`ServerBound::ContainerButtonClick`]'s consumer —
/// vanilla's own enchantment-menu click-menu-button routine. `slot` (`button_id`, `0..3`) selects
/// which of the three offers; the lapis price is `slot + 1` and the XP price
/// is that slot's own [`crate::enchanting::table_costs`] entry, both
/// re-derived here rather than trusted from the client.
///
/// `fresh_seed` is a pre-drawn `[0, i32::MAX)` roll from the caller's own
/// `SpawnRng` — the same "pre-drawn value" shape `apply_use_item_on`'s
/// composter `roll` already uses — only consumed when the enchant actually
/// succeeds, matching vanilla's own on-enchantment-performed routine's own reroll.
///
/// Returns the directives to send (the XP update, if any levels were spent,
/// then the refreshed menu content) or `Vec::new()` when the click is
/// refused: wrong window, no item, no offer at that cost, insufficient
/// lapis/levels, or a roll that produced no enchantment.
pub(super) fn apply_container_button_click<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    tracked: Option<&mut OpenContainer>,
    window_id: i32,
    button_id: i32,
    source: &dyn ChunkSource,
    experience: &mut crate::experience::PlayerExperience,
    creative: bool,
    fresh_seed: i64,
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> Vec<ServerDirective> {
    let Some(tracked) = tracked else { return Vec::new() };
    if tracked.window_id != window_id {
        return Vec::new();
    }
    // Loom and stonecutter share this packet type but use different shapes and
    // pricing from the enchanting table. They select an offer without lapis or
    // experience cost; see `apply_workstation_button_click`.
    if let MenuKind::ItemCombiner { station: station @ (Station::Loom | Station::Stonecutter), .. } = tracked.shape {
        return apply_workstation_button_click(proto, inventory, tracked, station, button_id, creative, hooks);
    }
    if tracked.shape != MenuKind::Enchanting {
        return Vec::new();
    }
    let Some(slot) = usize::try_from(button_id).ok().filter(|&s| s < 3) else {
        return Vec::new();
    };
    let pos = tracked.pos;
    let cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let Some(item) = cells.first().cloned().flatten() else {
        return Vec::new();
    };
    let lapis = cells.get(1).cloned().flatten();

    let seed = inventory.enchant_seed();
    let bookcases = crate::enchanting::bookshelf_power(source, pos);
    let costs = crate::enchanting::table_costs(seed, bookcases, &item);
    let cost = costs[slot];
    let lapis_cost = i32::try_from(slot).unwrap_or(0) + 1;
    let has_lapis = creative || lapis.as_ref().is_some_and(|l| i32::try_from(l.count).unwrap_or(0) >= lapis_cost);
    let affordable = creative || (experience.level() >= lapis_cost && experience.level() >= cost);
    if cost <= 0 || !has_lapis || !affordable {
        return Vec::new();
    }

    // Vanilla's own enchantment-menu enchantment-list getter: reseeded per slot so each of the
    // three offers is an independent draw off the same base seed.
    let mut rng = SpawnRng::new(seed.wrapping_add(slot as i64) as u64);
    let offers = crate::enchanting::select_enchantments(&mut rng, &item, cost);
    if offers.is_empty() {
        return Vec::new();
    }

    let mut enchanted = item;
    if enchanted.item.to_string() == "minecraft:book" {
        enchanted.item = "minecraft:enchanted_book".parse().expect("valid key");
    }
    for offer in &offers {
        crate::anvil::apply_enchantment(&mut enchanted, offer.key, offer.level);
    }
    if !creative {
        experience.take_levels(cost);
    }
    let new_lapis = if creative {
        lapis
    } else {
        lapis.and_then(|l| {
            let remaining = l.count.saturating_sub(u32::try_from(lapis_cost).unwrap_or(0));
            (remaining > 0).then(|| {
                let mut shrunk = l;
                shrunk.count = remaining;
                shrunk
            })
        })
    };
    if let Some(ws) = inventory.workstation_mut() {
        if let Some(slot0) = ws.get_mut(0) {
            *slot0 = Some(enchanted);
        }
        if let Some(slot1) = ws.get_mut(1) {
            *slot1 = new_lapis;
        }
    }
    inventory.set_enchant_seed(fresh_seed);

    let mut directives = Vec::new();
    if !creative {
        directives.push(proto.encode_set_experience(experience.progress(), experience.level(), experience.total()));
    }
    let layout = MenuLayout::enchanting_table();
    let new_cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let items: Vec<Option<ItemStack>> = layout
        .iter()
        .map(|(_, kind)| match kind {
            SlotKind::Player(native) => inventory.native(native).cloned(),
            SlotKind::Grid(cell) => new_cells.get(cell).cloned().flatten(),
            SlotKind::Container(_) | SlotKind::Result => None,
        })
        .collect();
    let state_id = tracked.next_state_id();
    directives.push(proto.encode_container_content(tracked.window_id, state_id, &items, inventory.click_state().carried.as_ref()));
    directives
}

/// [`apply_container_button_click`]'s loom/stonecutter branch —
/// vanilla's own loom-menu/stonecutter-menu click-menu-button routines. Both just
/// pick which offer [`workstation_result`] shows next; neither has a lapis
/// or XP cost (contrast the enchanting table above), so this only ever needs
/// to validate the index and resend the menu.
///
/// `station == Station::Stonecutter`'s own reselect guard
/// (its own stonecutter-menu click-menu-button routine's `if (selectedRecipeIndex.get() ==
/// buttonId) return false;`) is reproduced; its own loom-menu click-menu-button routine has no
/// such guard and re-applies unconditionally when the index is valid.
pub(super) fn apply_workstation_button_click<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    tracked: &mut OpenContainer,
    station: Station,
    button_id: i32,
    creative: bool,
    hooks: &crate::plugin_crafting::CraftingStationHooks,
) -> Vec<ServerDirective> {
    if station == Station::Stonecutter && inventory.selected_recipe_index() == Some(button_id) {
        return Vec::new();
    }
    let layout = MenuLayout::item_combiner(station);
    let cells: Vec<Option<ItemStack>> = inventory.workstation().map(<[_]>::to_vec).unwrap_or_default();
    let get = |i: usize| cells.get(i).and_then(Option::as_ref);
    let offer_count = match station {
        Station::Loom => crate::loom::selectable_pattern_count(get(2)),
        Station::Stonecutter => crate::stonecutting::count(get(0)),
        Station::Anvil | Station::Grindstone | Station::Smithing => 0,
    };
    if usize::try_from(button_id).is_ok_and(|index| index < offer_count) {
        inventory.set_selected_recipe_index(Some(button_id));
    }
    let items = read_workstation_menu(&layout, inventory, &cells, station, creative, hooks);
    let state_id = tracked.next_state_id();
    vec![proto.encode_container_content(tracked.window_id, state_id, &items, inventory.click_state().carried.as_ref())]
}

/// Lays a recipe-book recipe out in the open crafting grid (the `PLACE_RECIPE` consumer).
///
/// Which grid depends on the window: `0` is the player screen's 2×2, an open
/// crafting table is its 3×3. A 3×3 recipe asked for on the 2×2 screen has no
/// placement and is refused — [`crate::crafting::place_recipe`] returns `false` and
/// nothing moves, which is vanilla's behaviour too.
///
/// Returns the full `container_set_content` the client needs, because a fill moves
/// items out of arbitrary inventory slots and there is no diff to send.
pub(super) fn apply_recipe_placed<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    open_container: Option<&mut OpenContainer>,
    window_id: i32,
    recipe_index: i32,
    use_max_items: bool,
) -> Option<ServerDirective> {
    let index = usize::try_from(recipe_index).ok()?;
    let (_, recipe) = crate::crafting::recipe_at_index(index)?;

    let mut open = open_container;
    let (layout, uses_table_grid) = if window_id == 0 {
        (MenuLayout::player(), false)
    } else {
        let tracked = open.as_mut()?;
        if tracked.window_id != window_id || tracked.shape != MenuKind::CraftingTable {
            return None;
        }
        (MenuLayout::crafting_table(), true)
    };

    // The grid is moved out and back so `place_recipe` can hold `&mut` on both it
    // and the inventory — they are two fields of the same struct.
    let mut grid = if uses_table_grid {
        inventory.table_crafting()?.clone()
    } else {
        inventory.crafting().clone()
    };
    if !crate::crafting::place_recipe(inventory, &mut grid, recipe, use_max_items) {
        return None;
    }
    if uses_table_grid {
        *inventory.table_crafting_mut()? = grid;
    } else {
        *inventory.crafting_mut() = grid;
    }

    let grid_owner = if uses_table_grid {
        inventory.table_crafting().cloned()
    } else {
        Some(inventory.crafting().clone())
    };
    let items = read_menu(&layout, inventory, grid_owner.as_ref(), &[]);
    let state_id = match open.as_mut() {
        Some(tracked) => tracked.next_state_id(),
        None => 0,
    };
    Some(proto.encode_container_content(
        window_id,
        state_id,
        &items,
        inventory.click_state().carried.as_ref(),
    ))
}

/// Spawns stacks that left a menu into the world as item entities — vanilla's
/// `player.drop(stack, true)`, which [`crate::container_click::do_click`] has no
/// world to make itself.
///
/// A connection with no tracked position yet drops nothing rather than spawning at
/// the origin, the same "no data yet, don't guess" gate [`apply_attack`] uses.
pub(super) fn spawn_dropped_stacks(
    mobs: &MobHandle,
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    rng: &mut SpawnRng,
    dropped: Vec<ItemStack>,
) {
    if dropped.is_empty() {
        return;
    }
    let Some((x, y, z)) = player_pos else { return };
    // Container throws use the hand position and forward impulse derived from
    // `player_rot`, matching the Q-key drop behavior. The pickup delay keeps a
    // thrown stack from being collected by the player immediately.
    let rotation = player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 });
    let position = Vec3::new(x, y + EYE_HEIGHT - crate::block_drops::THROW_HAND_DROP, z);
    mobs.with(|sim| {
        for stack in dropped {
            let count = u8::try_from(stack.count).unwrap_or(u8::MAX);
            // A fresh draw per stack, as vanilla does: `doClick`'s outside case
            // can throw several stacks in one click and each gets its own spread.
            let velocity =
                crate::block_drops::thrown_item_velocity(rotation.yaw, rotation.pitch, rng);
            sim.spawn_item(
                stack.item.clone(),
                position,
                velocity,
                ItemLifecycle {
                    pickup_delay: crate::block_drops::THROWN_PICKUP_DELAY_TICKS,
                    ..ItemLifecycle::newly_dropped(count, DEFAULT_MAX_STACK_SIZE)
                },
            );
        }
    });
}

/// Throws the selected hotbar stack into the world — `Q` (`whole_stack: false`,
/// one item) or `Ctrl+Q` (`whole_stack: true`, all of it).
///
/// The operation has three steps: remove items from the selected slot, record
/// the slot's *new* contents, and spawn the entity with
/// [`crate::block_drops::thrown_item_velocity`].
///
/// # The slot update
///
/// **The client receives no drop acknowledgement.** The server records the
/// selected slot's contents but does not send that bookkeeping as a packet;
/// no separate slot acknowledgement is required, which **suppresses** the
/// corrective broadcast that would otherwise follow. That
/// works because the client predicts the drop itself (`lodestone-client`'s
/// `drop_selected` does, and its doc records that an unpredicted drop leaves the
/// count permanently wrong — the item really is gone server-side).
///
/// A rejected drop sends one `container_set_slot` carrying the authoritative
/// content, while an accepted drop remains inert in the common case because it
/// equals what the client predicted.
/// A no-op drop returns `None` and sends nothing, because the client predicted no
/// change either.
///
/// Returns the directive to send and the stacks to spawn; the caller owns both
/// because it holds the `Connection` and the [`MobHandle`].
pub(super) fn apply_item_dropped<P: ServerProtocol>(
    proto: &P,
    inventory: &mut PlayerInventory,
    open_container: Option<&mut OpenContainer>,
    player_pos: Option<(f64, f64, f64)>,
    player_rot: Option<Rotation>,
    whole_stack: bool,
    rng: &mut SpawnRng,
    mobs: &MobHandle,
) -> Option<ServerDirective> {
    let native = usize::from(inventory.selected_hotbar_slot());
    let held = inventory.native(native)?.clone();
    if held.count <= 0 {
        return None;
    }
    // Remove the whole selected stack for a full-stack throw, or one item.
    let taken = if whole_stack { held.count } else { 1 };
    let mut thrown = held.clone();
    thrown.count = taken;
    let remaining = held.count - taken;
    inventory.set_native(
        native,
        (remaining > 0).then(|| {
            let mut rest = held.clone();
            rest.count = remaining;
            rest
        }),
    );

    // Spawned before the reply is built so a panic here cannot leave the client
    // told about an inventory change that produced no entity.
    if let Some((x, y, z)) = player_pos {
        let rotation = player_rot.unwrap_or(Rotation { yaw: 0.0, pitch: 0.0 });
        let velocity =
            crate::block_drops::thrown_item_velocity(rotation.yaw, rotation.pitch, rng);
        let position = Vec3::new(
            x,
            y + EYE_HEIGHT - crate::block_drops::THROW_HAND_DROP,
            z,
        );
        let count = u8::try_from(thrown.count).unwrap_or(u8::MAX);
        mobs.with(|sim| {
            sim.spawn_item(
                thrown.item.clone(),
                position,
                velocity,
                ItemLifecycle {
                    // 40, not `newly_dropped`'s 10: a player walking forwards
                    // would otherwise pick their own throw straight back up.
                    pickup_delay: crate::block_drops::THROWN_PICKUP_DELAY_TICKS,
                    ..ItemLifecycle::newly_dropped(count, DEFAULT_MAX_STACK_SIZE)
                },
            );
        });
    }

    // The hotbar's menu slot in whichever window is open. The player screen and
    // the crafting table both put native hotbar slot `n` at menu slot
    // `hotbar_start + n`; asking the layout rather than hardcoding 36 is what
    // keeps this right for a container whose payload half is a different size.
    let (layout, window_id, state_id) = match open_container {
        Some(tracked) => {
            let layout = match tracked.shape {
                MenuKind::CraftingTable => MenuLayout::crafting_table(),
                _ => MenuLayout::container(tracked.container_size),
            };
            let window_id = tracked.window_id;
            (layout, window_id, tracked.next_state_id())
        }
        None => (MenuLayout::player(), 0, 0),
    };
    // Asked of the layout rather than hardcoded as `36 + native`: a container
    // window's own slots come *first*, so the hotbar's menu index depends on the
    // container's size. The player screen is the only layout where it is 36.
    let menu_slot = layout
        .iter()
        .find(|&(_, kind)| kind == SlotKind::Player(native))
        .and_then(|(index, _)| i32::try_from(index).ok())?;
    Some(proto.encode_container_slot(
        window_id,
        state_id,
        menu_slot,
        inventory.native(native),
    ))
}
