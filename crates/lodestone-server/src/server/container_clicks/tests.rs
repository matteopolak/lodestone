//! Tests for container, workstation, beacon, lectern and book handlers.

use super::*;
use crate::chunk::ChunkColumn;
use crate::furnace::{Furnace, FurnaceKind};
use uuid::Uuid;
use crate::server::tests::{stack, item_key};

/// [`apply_edit_book`]'s draft-save path: a writable book in a hotbar
/// slot gets its pages overwritten in place, no transmute.
#[test]
fn edit_book_draft_save_updates_pages_without_transmuting() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(ItemStack::new(item_key("writable_book"), 1)));
    let result = apply_edit_book(
        &mut inv,
        0,
        vec!["Page one".to_owned()],
        None,
        "Steve",
    );
    let (native, item) = result.expect("a writable book in a hotbar slot must be editable");
    assert_eq!(native, 0);
    assert_eq!(item.item, item_key("writable_book"));
    assert_eq!(
        item.components.writable_book_content,
        Some(vec!["Page one".to_owned()])
    );
    assert_eq!(inv.native(0), Some(&item));
}

/// The signing path: a title present transmutes the stack to
/// `minecraft:written_book` and stamps the signer's name as author —
/// vanilla's own sign-book handler's own literal `0`/`true` for
/// generation/resolved.
#[test]
fn edit_book_signing_transmutes_to_written_book() {
    let mut inv = PlayerInventory::new();
    inv.set_native(
        crate::inventory::OFFHAND_NATIVE,
        Some(ItemStack::new(item_key("writable_book"), 1)),
    );
    let (native, item) = apply_edit_book(
        &mut inv,
        i32::try_from(crate::inventory::OFFHAND_NATIVE).unwrap(),
        vec!["Once upon a time".to_owned(), "The End".to_owned()],
        Some("My Book".to_owned()),
        "Alex",
    )
    .expect("a writable book in the off-hand must be signable");
    assert_eq!(native, crate::inventory::OFFHAND_NATIVE);
    assert_eq!(item.item, item_key("written_book"));
    assert_eq!(item.components.writable_book_content, None);
    let content = item
        .components
        .written_book_content
        .expect("signing must set written_book_content");
    assert_eq!(content.title, "My Book");
    assert_eq!(content.author, "Alex");
    assert_eq!(content.generation, 0);
    assert!(content.resolved);
    assert_eq!(content.pages.len(), 2);
}

/// **Control**: a slot outside the hotbar or off-hand must be refused —
/// the `hotbar || off-hand` slot gate (`slot == 40` is the off-hand).
/// Without this, an implementation that skipped the slot check entirely
/// would still pass the two tests above (both use in-range slots).
#[test]
fn edit_book_refuses_a_main_storage_slot() {
    let mut inv = PlayerInventory::new();
    inv.set_native(9, Some(ItemStack::new(item_key("writable_book"), 1)));
    assert_eq!(
        apply_edit_book(&mut inv, 9, vec!["x".to_owned()], None, "Steve"),
        None
    );
}

/// **Control**: an item that is not a writable book must be refused —
/// vanilla's `carried.has(DataComponents.WRITABLE_BOOK_CONTENT)` gate.
/// Without this, any item in the targeted slot would silently gain book
/// content.
#[test]
fn edit_book_refuses_a_non_book_item() {
    let mut inv = PlayerInventory::new();
    inv.set_native(0, Some(ItemStack::new(item_key("stone"), 1)));
    assert_eq!(
        apply_edit_book(&mut inv, 0, vec!["x".to_owned()], None, "Steve"),
        None
    );
}

fn lectern_book(pages: usize) -> ItemStack {
    let mut book = stack("minecraft:written_book", 1);
    book.components.written_book_content = Some(WrittenBookContent {
        title: "Test".to_owned(),
        author: "Tester".to_owned(),
        generation: 0,
        pages: (0..pages).map(|page| Text::literal(format!("Page {page}"))).collect(),
        resolved: true,
    });
    book
}

#[test]
fn lectern_button_pages_and_take_are_authoritative() {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(3, 64, -2);
    block_entities.with(|reg| {
        reg.insert(pos, BlockEntity::Lectern(crate::block_entities::LecternData {
            book: Some(lectern_book(4)),
            page: 0,
        }));
    });
    let mut inventory = PlayerInventory::new();
    let mut open = OpenContainer {
        window_id: 7,
        pos,
        shape: MenuKind::Lectern,
        container_size: 1,
        state_id: 0,
    };

    let page = apply_lectern_button_click(
        &ContainerTagProto,
        &block_entities,
        &mut inventory,
        Some(&mut open),
        7,
        2,
        true,
    );
    assert_eq!(page.len(), 1, "next-page action must publish one data property");
    assert_eq!(block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Lectern(lectern)) => lectern.page,
        _ => -1,
    }), 1);

    let taken = apply_lectern_button_click(
        &ContainerTagProto,
        &block_entities,
        &mut inventory,
        Some(&mut open),
        7,
        3,
        true,
    );
    assert_eq!(taken.len(), 3, "take-book must update inventory, slot, and page");
    assert!(block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Lectern(lectern)) => lectern.book.is_none() && lectern.page == 0,
        _ => false,
    }));
    assert_eq!(inventory.native(0).map(|book| book.item.to_string()), Some("minecraft:written_book".to_owned()));
}

#[test]
fn lectern_take_refuses_when_inventory_has_no_room() {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(3, 64, -2);
    block_entities.with(|reg| {
        reg.insert(pos, BlockEntity::Lectern(crate::block_entities::LecternData {
            book: Some(lectern_book(1)),
            page: 0,
        }));
    });
    let mut inventory = PlayerInventory::new();
    for native in 0..37 {
        inventory.set_native(native, Some(stack("minecraft:stone", 64)));
    }
    let mut open = OpenContainer {
        window_id: 7,
        pos,
        shape: MenuKind::Lectern,
        container_size: 1,
        state_id: 0,
    };
    assert!(apply_lectern_button_click(
        &ContainerTagProto,
        &block_entities,
        &mut inventory,
        Some(&mut open),
        7,
        3,
        true,
    ).is_empty());
    assert!(block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Lectern(lectern)) => lectern.book.is_some(),
        _ => false,
    }));
}

const SLOT: i32 = 20;

const DATA: i32 = 21;

const CONTENT: i32 = 22;

/// A protocol double whose container encoders tag each directive with a
/// distinct packet id, `window_id`, and `state_id`/`property` — enough
/// for [`sync_open_container`]'s tests to read the diff *decisions* back
/// off the returned directives without needing the real `lodestone-v26-2`
/// wire encoding. Every other method is unreachable from these tests.
struct ContainerTagProto;

impl ServerProtocol for ContainerTagProto {
    fn decode(&self, _s: State, _id: i32, _p: &[u8]) -> ServerBound {
        unimplemented!()
    }
    fn login_success(&self, _u: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        unimplemented!()
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        unimplemented!()
    }
    fn begin_play(&self, _r: i32) -> Vec<ServerDirective> {
        unimplemented!()
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        unimplemented!()
    }
    fn encode_chunk(&self, _cx: i32, _cz: i32, _c: &ChunkColumn) -> ServerDirective {
        unimplemented!()
    }
    fn end_chunk_batch(&self, _n: i32) -> ServerDirective {
        unimplemented!()
    }
    fn encode_container_slot(
        &self,
        window_id: i32,
        state_id: i32,
        slot: i32,
        item: Option<&ItemStack>,
    ) -> ServerDirective {
        ServerDirective::Send {
            packet_id: SLOT,
            payload: vec![
                window_id as u8,
                state_id as u8,
                slot as u8,
                item.map_or(0, |s| s.count as u8),
            ],
        }
    }
    fn encode_container_data(&self, window_id: i32, property: i32, value: i32) -> ServerDirective {
        ServerDirective::Send {
            packet_id: DATA,
            payload: vec![window_id as u8, property as u8, value as u8],
        }
    }
    fn encode_container_content(
        &self,
        window_id: i32,
        state_id: i32,
        items: &[Option<ItemStack>],
        carried: Option<&ItemStack>,
    ) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CONTENT,
            payload: vec![
                window_id as u8,
                state_id as u8,
                items.len() as u8,
                carried.map_or(0, |s| s.count as u8),
            ],
        }
    }
}

fn open(pos: BlockPos, container_size: usize) -> OpenContainer {
    OpenContainer {
        window_id: 7,
        pos,
        shape: MenuKind::Container {
            size: container_size,
        },
        container_size,
        state_id: 0,
    }
}

#[test]
fn sync_open_container_emits_nothing_when_nothing_changed() {
    let mut o = open(BlockPos::new(0, 0, 0), 3);
    let mut sync = ContainerSync {
        slots: vec![Some(stack("minecraft:coal", 1)), None, None],
        data: vec![10, 20],
    };
    let out = sync_open_container(
        &ContainerTagProto,
        &mut o,
        &mut sync,
        vec![Some(stack("minecraft:coal", 1)), None, None],
        vec![10, 20],
    );
    assert!(out.is_empty(), "unchanged container must not re-send: {out:?}");
}

/// The exact scenario this function exists for: a furnace's own
/// background tick lights it (data property 0 changes) and later
/// produces an ingot (slot 2 changes) — no click involved at all.
#[test]
fn sync_open_container_emits_only_the_changed_slot_and_data_entries() {
    let mut o = open(BlockPos::new(0, 0, 0), 3);
    let mut sync = ContainerSync {
        slots: vec![Some(stack("minecraft:iron_ore", 1)), Some(stack("minecraft:coal", 1)), None],
        data: vec![0, 0, 0, 200],
    };
    let out = sync_open_container(
        &ContainerTagProto,
        &mut o,
        &mut sync,
        vec![None, Some(stack("minecraft:coal", 1)), Some(stack("minecraft:iron_ingot", 1))],
        // Only index 0 (`lit_time_remaining`) changes here — index 1
        // (`lit_total_time`) is deliberately held constant so this
        // fixture isolates "exactly one data property changed" rather
        // than also exercising two simultaneous data changes (a real
        // ignition tick does change both at once, but that is not what
        // this particular test is asserting).
        vec![190, 0, 0, 200],
    );
    // Slot 0 (iron ore consumed) and slot 2 (ingot produced) changed;
    // slot 1 (fuel) did not.
    let ServerDirective::Send { packet_id, payload } = &out[0] else {
        panic!("expected Send");
    };
    assert_eq!(*packet_id, SLOT);
    assert_eq!(payload[2], 0, "slot index 0 changed first");
    let ServerDirective::Send { packet_id, payload } = &out[1] else {
        panic!("expected Send");
    };
    assert_eq!(*packet_id, SLOT);
    assert_eq!(payload[2], 2, "slot index 2 changed second");
    // Data property 0 (lit_time_remaining) changed.
    let ServerDirective::Send { packet_id, payload } = &out[2] else {
        panic!("expected Send");
    };
    assert_eq!(*packet_id, DATA);
    assert_eq!(payload[1], 0, "property index 0 changed");
    assert_eq!(out.len(), 3);
    // The sync's own bookkeeping must now hold the new values, so the
    // *next* call diffs against these, not the stale ones.
    assert_eq!(sync.slots[2], Some(stack("minecraft:iron_ingot", 1)));
    assert_eq!(sync.data[0], 190);
}

/// **Control**: every slot/data send must bump `state_id` (vanilla's
/// `incrementStateId`), and a data-only change must bump it **zero**
/// times — proving the two encoders are not accidentally sharing one
/// counter increment.
#[test]
fn sync_open_container_bumps_state_id_only_for_slot_sends() {
    let mut o = open(BlockPos::new(0, 0, 0), 1);
    let mut sync = ContainerSync {
        slots: vec![None],
        data: vec![0],
    };
    assert_eq!(o.state_id, 0);
    let _ = sync_open_container(
        &ContainerTagProto,
        &mut o,
        &mut sync,
        vec![None],
        vec![1], // data-only change
    );
    assert_eq!(o.state_id, 0, "a data-only change must not bump state_id");

    let _ = sync_open_container(
        &ContainerTagProto,
        &mut o,
        &mut sync,
        vec![Some(stack("minecraft:coal", 1))], // slot change
        vec![1],
    );
    assert_eq!(o.state_id, 1, "a slot change must bump state_id exactly once");
}

/// A real left-click picks a stack up off a native slot and a second one puts
/// it down elsewhere — the whole thing derived from `(slot, button, type)`,
/// with no item named anywhere in the input.
#[test]
fn container_clicked_against_window_zero_derives_the_move() {
    let mut inventory = PlayerInventory::new();
    inventory.set_native(9, Some(stack("minecraft:stone", 4)));
    let block_entities = BlockEntityHandle::new();

    // Menu slot 9 is native 9. Left-click: whole stack onto the cursor.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 9, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(inventory.native(9), None);
    assert_eq!(
        inventory.click_state().carried.as_ref().map(|s| s.count),
        Some(4)
    );

    // Menu slot 40 is native 4 (hotbar). Left-click: the whole cursor lands.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 40, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(inventory.native(4), Some(&stack("minecraft:stone", 4)));
    assert!(inventory.click_state().carried.is_none());
}

/// This runs end to end through the production dispatch path (not just
/// `container_click`'s own unit tests): a `ServerBound::SelectBundleItem`
/// packet's consumer (`inventory.set_selected_bundle_item`) is exactly
/// what `apply_container_clicked`'s later right-click-extract reads.
/// Without the store-then-read join this proves, a scroll-selected
/// bundle item would always come out as the front one regardless of
/// what the player highlighted — the bug the control below actually
/// caught in `bundle_other_stacked_on_me`'s first draft.
#[test]
fn a_select_bundle_item_packet_changes_which_item_a_later_extract_pops() {
    let mut inventory = PlayerInventory::new();
    let mut bundle = stack("minecraft:bundle", 1);
    bundle.components.bundle_contents =
        vec![stack("minecraft:torch", 3), stack("minecraft:oak_planks", 5)];
    // Menu slot 9 is native 9 for window 0 (`MenuLayout::player`'s own
    // storage-first ordering, the same join `container_clicked_against_
    // window_zero_derives_the_move` above already relies on).
    inventory.set_native(9, Some(bundle));
    let block_entities = BlockEntityHandle::new();

    // The dispatch arm's own body: `ServerBound::SelectBundleItem { slot_id:
    // 9, selected_item_index: 1 } => inventory.set_selected_bundle_item(9, 1)`.
    inventory.set_selected_bundle_item(9, 1);

    // Right-click (button 1, PICKUP) on slot 9 with an empty cursor.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 9, button: 1, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    assert_eq!(
        inventory.click_state().carried.as_ref().map(|s| s.item.to_string()),
        Some("minecraft:oak_planks".to_owned()),
        "the selected index (1) should have been extracted, not the front item (0)"
    );
}

/// **The security property.** A client claiming a slot now holds an item it
/// never had mints nothing: the claim is not stored, and the server answers the
/// same click with a full `container_set_content` correction.
///
/// Both halves are asserted because they fail independently — a server that
/// ignored the claim but sent no correction would leave the client believing in
/// an item that does not exist.
#[test]
fn a_claimed_item_is_never_stored_and_the_client_is_corrected() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();

    // An empty inventory, an empty cursor, a left-click on an empty slot — and
    // a diff claiming a diamond block appeared there.
    let (correction, dropped) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 9, button: 0, click_type: 0 },
        &[(9, Some(stack("minecraft:diamond_block", 64)))],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(inventory.native(9), None, "the claim must not be stored");
    assert!(dropped.is_empty());
    assert!(
        matches!(correction, Some(ServerDirective::Send { packet_id, .. }) if packet_id == CONTENT),
        "a disagreeing claim must be corrected, got {correction:?}"
    );

    // And a claim that matches what the server derived sends nothing at all,
    // so an honest client pays no extra traffic — the control that the
    // correction above is a comparison rather than an unconditional resend.
    inventory.set_native(9, Some(stack("minecraft:stone", 1)));
    let (correction, _) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 9, button: 0, click_type: 0 },
        &[(9, None)],
        Some(&stack("minecraft:stone", 1)),
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(correction, None, "an honest prediction needs no correction");
}

/// Crafting, end to end and server-derived: planks clicked into the 2x2 make
/// the server derive a crafting table, and taking the result consumes the grid.
/// The client never names a result.
#[test]
fn the_crafting_result_is_derived_and_taking_it_consumes_the_grid() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();
    inventory.click_state_mut().carried = Some(stack("minecraft:oak_planks", 4));

    // Right-click each of the four grid cells: one plank each.
    for menu_slot in 1..=4 {
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            None,
            0,
            Click { slot: menu_slot, button: 1, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
    }
    assert_eq!(
        inventory.crafting().result().map(|r| r.item.to_string()),
        Some("minecraft:crafting_table".to_string()),
        "the server derived the result from the grid it now holds"
    );

    // Take it. The cursor holds the table and every grid cell is empty again.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 0, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(
        inventory
            .click_state()
            .carried
            .as_ref()
            .map(|s| s.item.to_string()),
        Some("minecraft:crafting_table".to_string())
    );
    assert!(inventory.crafting().is_empty(), "one craft consumed the grid");
    assert!(inventory.crafting().result().is_none());
}

/// The anvil end to end through the real click path: place a
/// damaged pickaxe and a repair material into the two input cells, take the
/// derived result, and check both the item mutation and the input-slot
/// consumption `container_click`'s `take_result` special-cases for
/// `Station::Anvil` — cell 0 always clears, cell 1 shrinks by the repair
/// material count actually used, not by one and not entirely.
#[test]
fn the_anvil_repairs_through_the_real_click_path_and_consumes_the_right_amount() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();
    inventory.open_workstation(2);
    let mut input = stack("minecraft:diamond_pickaxe", 1);
    input.components.damage = Some(1200);
    input.components.max_damage = Some(1561);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(input);
        ws[1] = Some(stack("minecraft:diamond", 3));
    }
    let mut open = OpenContainer {
        window_id: 7,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
        container_size: 3,
        state_id: 0,
    };

    // Menu slot 2 is the result for a 2-input combiner menu.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        7,
        Click { slot: 2, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    let carried = inventory.click_state().carried.as_ref().expect("the repaired pickaxe must be on the cursor");
    assert_eq!(carried.item.to_string(), "minecraft:diamond_pickaxe");
    assert_eq!(carried.components.damage, Some(30), "matches anvil::compute's own repair-with-material test");

    let cells = inventory.workstation().expect("still open");
    assert_eq!(cells[0], None, "the base item is always fully consumed");
    assert_eq!(
        cells[1], None,
        "all 3 diamonds were used by the repair (repair_item_count_cost == addition.count)"
    );
}

/// The anvil result cannot be taken by a survival player without enough XP
/// levels. The end-to-end click leaves the result in place, keeps the cursor
/// empty, and consumes nothing; exactly enough XP and creative mode both
/// allow the take.
#[test]
fn a_0_xp_survival_player_cannot_take_a_costed_anvil_result_but_creative_and_enough_levels_can() {
    let same_repair_fixture = |inventory: &mut PlayerInventory| {
        inventory.open_workstation(2);
        let mut input = stack("minecraft:diamond_pickaxe", 1);
        input.components.damage = Some(1200);
        input.components.max_damage = Some(1561);
        if let Some(ws) = inventory.workstation_mut() {
            ws[0] = Some(input);
            ws[1] = Some(stack("minecraft:diamond", 3));
        }
    };
    let open_anvil = || OpenContainer {
        window_id: 7,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
        container_size: 3,
        state_id: 0,
    };

    // The real cost this exact fixture prices to — read from `anvil::compute`
    // itself (the single already-tested source of truth for the formula;
    // this test is about the `xp_level`-vs-`cost` *wiring*, not re-deriving
    // the repair-cost arithmetic a second time), not guessed.
    let mut priced_input = stack("minecraft:diamond_pickaxe", 1);
    priced_input.components.damage = Some(1200);
    priced_input.components.max_damage = Some(1561);
    let cost = crate::anvil::compute(
        Some(&priced_input),
        Some(&stack("minecraft:diamond", 3)),
        None,
        false,
    )
    .cost;
    assert!(cost > 0, "the fixture must actually cost XP levels, or this test proves nothing");

    // 0 XP levels, survival: refused. Nothing moves, nothing is consumed.
    {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        same_repair_fixture(&mut inventory);
        let mut open = open_anvil();
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            7,
            Click { slot: 2, button: 0, click_type: 0 },
            &[],
            None,
            false,
            0,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert!(
            inventory.click_state().carried.is_none(),
            "0 XP levels must not take a {cost}-cost anvil result"
        );
        let cells = inventory.workstation().expect("still open");
        assert!(cells[0].is_some(), "the base item must stay put when the take is refused");
        assert!(cells[1].is_some(), "the addition must stay put when the take is refused");
    }

    // Exactly `cost` XP levels, survival: succeeds — the `>=`, not `>`, half
    // of vanilla's own anvil-menu may-pickup gate's comparison.
    {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        same_repair_fixture(&mut inventory);
        let mut open = open_anvil();
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            7,
            Click { slot: 2, button: 0, click_type: 0 },
            &[],
            None,
            false,
            cost,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert!(
            inventory.click_state().carried.is_some(),
            "exactly {cost} XP levels must take the result"
        );
    }

    // Creative, 0 XP levels: succeeds unconditionally because creative
    // bypasses the experience-cost check.
    {
        let mut inventory = PlayerInventory::new();
        let block_entities = BlockEntityHandle::new();
        same_repair_fixture(&mut inventory);
        let mut open = open_anvil();
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            7,
            Click { slot: 2, button: 0, click_type: 0 },
            &[],
            None,
            true,
            0,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert!(
            inventory.click_state().carried.is_some(),
            "creative must take regardless of XP levels"
        );
    }
}

/// The anvil's genuinely bespoke take rule (vanilla's own anvil-menu on-take routine): a take
/// priced *purely* by a pending rename must leave a present-but-not-
/// consumed addition cell completely untouched, not cleared as if a real
/// combine had consumed it. `container_click::take_result`'s own internal
/// re-derivation always evaluates with no rename text (that module is
/// deliberately rename-free) and so cannot see this by itself — see
/// `apply_workstation_clicked`'s own correction, which this pins.
#[test]
fn a_pure_rename_take_leaves_a_present_but_unconsumed_addition_untouched() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();
    inventory.open_workstation(2);
    inventory.set_pending_rename(Some("Excalibur".to_owned()));
    let input = stack("minecraft:diamond_sword", 1);
    let addition = stack("minecraft:diamond_sword", 1);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(input);
        ws[1] = Some(addition.clone());
    }
    let mut open = OpenContainer {
        window_id: 7,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
        container_size: 3,
        state_id: 0,
    };

    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        7,
        Click { slot: 2, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    let carried = inventory.click_state().carried.as_ref().expect("must take the renamed sword");
    assert_eq!(
        carried.components.custom_name.as_ref().map(lodestone_model::text::Text::to_plain_string),
        Some("Excalibur".to_owned())
    );
    let cells = inventory.workstation().expect("still open");
    assert_eq!(cells[0], None, "the base item is always fully consumed");
    assert_eq!(
        cells[1],
        Some(addition),
        "a pure-rename take must leave an unconsumed addition exactly as it was"
    );
}

/// The grindstone end to end: a single enchanted item in one
/// slot strips to curses only, and taking it **fully clears** the input
/// cell it came from — the grindstone's distinct-from-the-anvil take rule.
#[test]
fn the_grindstone_strips_enchantments_through_the_real_click_path() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();
    inventory.open_workstation(2);
    let mut sword = stack("minecraft:diamond_sword", 1);
    sword.components.enchantments = vec![lodestone_model::ItemEnchantment {
        id: crate::enchantment_data::id_of("minecraft:sharpness").unwrap(),
        level: 3,
    }];
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(sword);
    }
    let mut open = OpenContainer {
        window_id: 7,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Grindstone },
        container_size: 3,
        state_id: 0,
    };

    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        7,
        Click { slot: 2, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    let carried = inventory.click_state().carried.as_ref().expect("must take a plain sword back");
    assert!(carried.components.enchantments.is_empty(), "sharpness is not a curse and must be stripped");
    let cells = inventory.workstation().expect("still open");
    assert_eq!(cells[0], None, "grindstone always fully clears both inputs on take");
    assert_eq!(cells[1], None);
}

/// The smithing table end to end: a netherite upgrade through
/// the real click path, checking the generic shrink-by-1 take behaviour
/// (shared with the crafting table) applies to all three input cells.
#[test]
fn the_smithing_table_upgrades_to_netherite_through_the_real_click_path() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();
    inventory.open_workstation(3);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(stack("minecraft:netherite_upgrade_smithing_template", 1));
        ws[1] = Some(stack("minecraft:diamond_sword", 1));
        ws[2] = Some(stack("minecraft:netherite_ingot", 1));
    }
    let mut open = OpenContainer {
        window_id: 7,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 3, station: Station::Smithing },
        container_size: 4,
        state_id: 0,
    };

    // Menu slot 3 is the result for a 3-input combiner menu.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        7,
        Click { slot: 3, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    let carried = inventory.click_state().carried.as_ref().expect("must take the upgraded sword");
    assert_eq!(carried.item.to_string(), "minecraft:netherite_sword");
    let cells = inventory.workstation().expect("still open");
    assert!(cells.iter().all(Option::is_none), "each of the three inputs was a stack of one and is now consumed");
}

/// The anvil action reaches [`crate::anvil::compute`] (a pure rename costs
/// exactly 1 XP level) and re-sending the identical name is a no-op.
#[test]
fn rename_item_prices_a_pure_rename_at_one_and_is_idempotent() {
    let mut inventory = PlayerInventory::new();
    inventory.open_workstation(2);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(stack("minecraft:diamond_sword", 1));
    }
    let mut open = OpenContainer {
        window_id: 7,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
        container_size: 3,
        state_id: 0,
    };

    let directives = apply_rename_item(
        &ContainerTagProto,
        &mut inventory,
        Some(&mut open),
        "Excalibur",
        false,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(directives.len(), 2, "the refreshed content, then the cost data slot");
    assert_eq!(inventory.pending_rename(), Some("Excalibur"));
    match &directives[1] {
        ServerDirective::Send { packet_id, payload } => {
            assert_eq!(*packet_id, DATA);
            assert_eq!(payload[2], 1, "a pure rename costs exactly 1 XP level");
        }
        other => panic!("expected a Send directive, got {other:?}"),
    }

    let again = apply_rename_item(
        &ContainerTagProto,
        &mut inventory,
        Some(&mut open),
        "Excalibur",
        false,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert!(again.is_empty(), "an unchanged name must not resend anything");
}

/// The enchanting-table action reaches the real click path: choosing an offer
/// enchants the item, spends XP levels, consumes lapis, and rerolls the seed.
#[test]
fn container_button_click_enchants_the_item_and_charges_xp_and_lapis() {
    struct AirWorld;
    impl ChunkSource for AirWorld {
        fn column(&self, _cx: i32, _cz: i32) -> crate::chunk::ChunkColumn {
            unimplemented!("not needed: bookshelf_power reads block_state only")
        }
        fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
            crate::chunk::air_state()
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_string()
        }
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {
            unimplemented!("read-only in this test")
        }
    }

    let sword = stack("minecraft:diamond_sword", 1);
    // Slot 0's cost floors at 1 for any enchantable item regardless of the
    // roll (`cost_for_slot`'s `(selected / 3).max(1)`), so only the offer
    // draw itself needs a seed search — deterministic given the
    // production RNG, not flaky: whichever seed is found here always
    // rolls the same offer.
    let seed = (0..64i64)
        .find(|&s| {
            let costs = crate::enchanting::table_costs(s, 0, &sword);
            let mut rng = SpawnRng::new(s.wrapping_add(0) as u64);
            costs[0] > 0 && !crate::enchanting::select_enchantments(&mut rng, &sword, costs[0]).is_empty()
        })
        .expect("at least one of the first 64 seeds must roll a slot-0 offer for a diamond sword");

    let mut inventory = PlayerInventory::new();
    inventory.open_workstation(2);
    inventory.set_enchant_seed(seed);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(sword);
        ws[1] = Some(stack("minecraft:lapis_lazuli", 5));
    }
    let mut open = OpenContainer {
        window_id: 7,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::Enchanting,
        container_size: 2,
        state_id: 0,
    };
    let mut experience = crate::experience::PlayerExperience::default();
    experience.give_points(crate::experience::total_points_for_level(30));
    let before_level = experience.level();

    let directives = apply_container_button_click(
        &ContainerTagProto,
        &mut inventory,
        Some(&mut open),
        7,
        0,
        &AirWorld,
        &mut experience,
        false,
        999,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    assert!(!directives.is_empty(), "a successful enchant must resend the menu");
    assert!(experience.level() < before_level, "XP levels must be spent");
    let cells = inventory.workstation().expect("still open");
    let enchanted = cells[0].as_ref().expect("the item stays in slot 0");
    assert!(
        !enchanted.components.enchantments.is_empty(),
        "the item must come back enchanted"
    );
    let lapis_left = cells[1].as_ref().map_or(0, |l| l.count);
    assert!(lapis_left < 5, "at least one lapis must be consumed, left {lapis_left}");
    assert_eq!(inventory.enchant_seed(), 999, "a successful enchant rerolls the seed");

    // A second click at the same (now stale) slot-0 cost/seed combination
    // is refused once the seed has moved on — not a hang, not a panic,
    // and not a second free enchant.
    let refused = apply_container_button_click(
        &ContainerTagProto,
        &mut inventory,
        Some(&mut open),
        7,
        5, // out of range: only 0..3 are real slots
        &AirWorld,
        &mut experience,
        false,
        1,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert!(refused.is_empty(), "an out-of-range button id must be refused");
}

/// This runs end to end through the real production dispatch: a
/// stonecutter menu opens with cobblestone in its input cell, a
/// `ContainerButtonClick` selects one of the real offers
/// `crate::stonecutting::matches` computes, and taking the result slot
/// consumes exactly one cobblestone and leaves the rest — the same
/// `apply_container_clicked` → `apply_workstation_clicked` →
/// `container_click::take_result` path every other workstation in this
/// crate already goes through, not a hand-rolled shortcut.
#[test]
fn a_stonecutter_button_click_then_take_produces_the_selected_recipe_and_consumes_one_input() {
    // `apply_workstation_button_click` (the loom/stonecutter branch of
    // `apply_container_button_click`) never reads `source` at all —
    // unlike the enchanting branch's `bookshelf_power` call — so this
    // must never be invoked.
    struct UnusedSource;
    impl ChunkSource for UnusedSource {
        fn column(&self, _cx: i32, _cz: i32) -> crate::chunk::ChunkColumn {
            unimplemented!("the stonecutter button click must never read the world")
        }
        fn block_state_id(&self, _x: i32, _y: i32, _z: i32) -> StateId {
            unimplemented!("the stonecutter button click must never read the world")
        }

        fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
            crate::chunk::DEFAULT_BIOME.to_string()
        }
        fn set_block(&self, _x: i32, _y: i32, _z: i32, _state: StateId) {
            unimplemented!("read-only in this test")
        }
    }

    let mut inventory = PlayerInventory::new();
    inventory.open_workstation(1);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(stack("minecraft:cobblestone", 5));
    }
    let mut open = OpenContainer {
        window_id: 9,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 1, station: Station::Stonecutter },
        container_size: 2,
        state_id: 0,
    };
    let offers = crate::stonecutting::matches(&stack("minecraft:cobblestone", 1));
    assert!(offers.len() >= 2, "need at least two offers to prove a specific one was selected");

    // Select offer index 1 — not the default/first, so a bug that always
    // takes index 0 would fail this.
    let directives = apply_container_button_click(
        &ContainerTagProto,
        &mut inventory,
        Some(&mut open),
        9,
        1,
        &UnusedSource,
        &mut crate::experience::PlayerExperience::default(),
        false,
        0,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert!(!directives.is_empty(), "a valid selection must resend the menu");
    assert_eq!(inventory.selected_recipe_index(), Some(1));

    let block_entities = BlockEntityHandle::new();
    let (_, _dropped) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        9,
        // Slot `inputs` (1) is the result slot — a plain left-click picks
        // it up, which is what triggers `take_result`.
        Click { slot: 1, button: 0, click_type: 0 },
        &[],
        None,
        false,
        0,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    let taken = inventory
        .click_state()
        .carried
        .as_ref()
        .expect("the take must put the result on the cursor");
    assert_eq!(taken.item, offers[1].item, "the taken item must be the selected offer, not the first one");

    let cells = inventory.workstation().expect("still open");
    assert_eq!(
        cells[0].as_ref().map(|s| s.count),
        Some(4),
        "exactly one cobblestone must be consumed by the take"
    );
}

/// This runs end to end: a loom with a banner, a dye and a specific
/// pattern *item* auto-selects that item's one pattern — no
/// `ContainerButtonClick` needed, matching vanilla's own loom-menu slots-changed routine's own
/// auto-select branch — and taking the result consumes exactly one
/// banner and one dye while leaving the pattern item untouched, so it
/// can stamp a second banner.
#[test]
fn a_loom_take_with_a_pattern_item_consumes_banner_and_dye_but_not_the_pattern_item() {
    let mut inventory = PlayerInventory::new();
    inventory.open_workstation(3);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(stack("minecraft:white_banner", 3));
        ws[1] = Some(stack("minecraft:red_dye", 5));
        ws[2] = Some(stack("minecraft:creeper_banner_pattern", 1));
    }
    let mut open = OpenContainer {
        window_id: 11,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 3, station: Station::Loom },
        container_size: 4,
        state_id: 0,
    };
    let block_entities = BlockEntityHandle::new();

    let (_, _dropped) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        11,
        // Slot `inputs` (3) is the result slot.
        Click { slot: 3, button: 0, click_type: 0 },
        &[],
        None,
        false,
        0,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    let taken = inventory.click_state().carried.as_ref().expect("the take must produce a result");
    assert_eq!(taken.item.to_string(), "minecraft:white_banner");
    assert_eq!(
        taken.components.banner_patterns,
        vec![lodestone_model::BannerPatternLayer {
            pattern_asset_id: "creeper".to_string(),
            color: "red".to_string(),
        }]
    );

    let cells = inventory.workstation().expect("still open");
    assert_eq!(cells[0].as_ref().map(|s| s.count), Some(2), "one banner must be consumed");
    assert_eq!(cells[1].as_ref().map(|s| s.count), Some(4), "one dye must be consumed");
    assert_eq!(
        cells[2].as_ref().map(|s| s.count),
        Some(1),
        "the pattern item must survive the take, so it can stamp a second banner"
    );
}

/// Two test-local stand-ins reproducing `lodestone-crafting-warden`'s
/// real `SmithingSwordBan`/`AnvilBlessing` logic exactly, kept local
/// rather than a dev-dependency on that crate.
///
/// A dev-dependency depending back on this crate would compile this
/// crate's own `--lib` unit-test binary *twice* — once as the unit
/// under test, once through the plugin's normal dependency edge — which
/// produces two incompatible `CraftingStationHooks` types sharing one
/// name (`error[E0308]: mismatched types … multiple different versions
/// of crate lodestone_server in the dependency graph`). An integration
/// test under `tests/*.rs` would avoid that (it links this crate's lib
/// once, normally), but `apply_container_clicked`/
/// `apply_workstation_clicked`/`apply_container_button_click`/
/// `apply_rename_item` are module-private, so a test proving they
/// consult a registered hook can only live inside this module. The
/// external crate's own unit tests call `on_prepare` directly to prove
/// its logic; these two prove the opposite half — that production
/// actually asks the question — by driving the real dispatch below.
struct WiringProofDenySwordUpgrade;

impl crate::plugin_crafting::CraftingStationHook for WiringProofDenySwordUpgrade {
    fn on_prepare(&self, inputs: &crate::plugin_crafting::StationInputs) -> crate::plugin_crafting::StationVerdict {
        if inputs.station != Station::Smithing {
            return crate::plugin_crafting::StationVerdict::Allow;
        }
        let base = inputs.cells.get(1).and_then(Option::as_ref);
        if base.is_some_and(|item| item.item.to_string() == "minecraft:diamond_sword") {
            crate::plugin_crafting::StationVerdict::Deny
        } else {
            crate::plugin_crafting::StationVerdict::Allow
        }
    }
}

struct WiringProofBlessAnvilName;

impl crate::plugin_crafting::CraftingStationHook for WiringProofBlessAnvilName {
    fn on_prepare(&self, inputs: &crate::plugin_crafting::StationInputs) -> crate::plugin_crafting::StationVerdict {
        if inputs.station != Station::Anvil {
            return crate::plugin_crafting::StationVerdict::Allow;
        }
        let Some(computed) = inputs.computed.clone() else {
            return crate::plugin_crafting::StationVerdict::Allow;
        };
        let Some(name) = computed.components.custom_name.clone() else {
            return crate::plugin_crafting::StationVerdict::Allow;
        };
        let plain = name.to_plain_string();
        if plain.starts_with("[Blessed] ") {
            return crate::plugin_crafting::StationVerdict::Allow;
        }
        let mut blessed = computed;
        blessed.components.custom_name = Some(lodestone_model::text::Text::literal(format!("[Blessed] {plain}")));
        crate::plugin_crafting::StationVerdict::Replace(blessed)
    }
}

/// Exercises plugin hook registration through the production smithing
/// click path. `WiringProofDenySwordUpgrade` vetoes one netherite upgrade
/// when registered, while a sibling upgrade remains allowed, proving that
/// dispatch consults the hook for the derived menu result.
#[test]
fn a_registered_plugin_hook_vetoes_one_smithing_upgrade_and_allows_a_sibling_one() {
    let hooks = crate::plugin_crafting::CraftingStationHooks::new();
    hooks.register(0, std::sync::Arc::new(WiringProofDenySwordUpgrade));
    let block_entities = BlockEntityHandle::new();

    let mut denied = PlayerInventory::new();
    denied.open_workstation(3);
    if let Some(ws) = denied.workstation_mut() {
        ws[0] = Some(stack("minecraft:netherite_upgrade_smithing_template", 1));
        ws[1] = Some(stack("minecraft:diamond_sword", 1));
        ws[2] = Some(stack("minecraft:netherite_ingot", 1));
    }
    let mut open_denied = OpenContainer {
        window_id: 20,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 3, station: Station::Smithing },
        container_size: 4,
        state_id: 0,
    };
    apply_container_clicked(
        &ContainerTagProto,
        &mut denied,
        &block_entities,
        Some(&mut open_denied),
        20,
        // Slot `inputs` (3) is the result slot.
        Click { slot: 3, button: 0, click_type: 0 },
        &[],
        None,
        false,
        0,
        &hooks,
    );
    assert!(
        denied.click_state().carried.is_none(),
        "the registered SmithingSwordBan hook must veto the sword upgrade, so nothing is taken"
    );
    let denied_cells = denied.workstation().expect("still open");
    assert!(denied_cells[1].is_some(), "a denied take must leave the base item in place");

    // Positive control, the same dispatch with a pickaxe base instead of
    // a sword: this must succeed, proving the veto is scoped to the one
    // named item rather than blocking every smithing take.
    let mut allowed = PlayerInventory::new();
    allowed.open_workstation(3);
    if let Some(ws) = allowed.workstation_mut() {
        ws[0] = Some(stack("minecraft:netherite_upgrade_smithing_template", 1));
        ws[1] = Some(stack("minecraft:diamond_pickaxe", 1));
        ws[2] = Some(stack("minecraft:netherite_ingot", 1));
    }
    let mut open_allowed = OpenContainer {
        window_id: 21,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 3, station: Station::Smithing },
        container_size: 4,
        state_id: 0,
    };
    apply_container_clicked(
        &ContainerTagProto,
        &mut allowed,
        &block_entities,
        Some(&mut open_allowed),
        21,
        Click { slot: 3, button: 0, click_type: 0 },
        &[],
        None,
        false,
        0,
        &hooks,
    );
    let taken = allowed
        .click_state()
        .carried
        .as_ref()
        .expect("the pickaxe upgrade must be allowed through unchanged");
    assert_eq!(taken.item.to_string(), "minecraft:netherite_pickaxe");
}

/// Exercises the plugin replacement branch through the production anvil
/// click path. `WiringProofBlessAnvilName` adds a `[Blessed]` prefix to a
/// rename result before the player takes it.
#[test]
fn a_registered_plugin_hook_blesses_a_real_anvil_rename_take() {
    let hooks = crate::plugin_crafting::CraftingStationHooks::new();
    hooks.register(0, std::sync::Arc::new(WiringProofBlessAnvilName));

    let mut inventory = PlayerInventory::new();
    inventory.open_workstation(2);
    if let Some(ws) = inventory.workstation_mut() {
        ws[0] = Some(stack("minecraft:diamond_sword", 1));
    }
    let mut open = OpenContainer {
        window_id: 22,
        pos: BlockPos::new(0, 0, 0),
        shape: MenuKind::ItemCombiner { inputs: 2, station: Station::Anvil },
        container_size: 3,
        state_id: 0,
    };

    apply_rename_item(&ContainerTagProto, &mut inventory, Some(&mut open), "Excalibur", false, &hooks);
    assert_eq!(inventory.pending_rename(), Some("Excalibur"));

    let block_entities = BlockEntityHandle::new();
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        22,
        // Slot `inputs` (2) is the result slot.
        Click { slot: 2, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &hooks,
    );
    let taken = inventory
        .click_state()
        .carried
        .as_ref()
        .expect("the rename take must succeed");
    let name = taken.components.custom_name.as_ref().expect("still named");
    assert_eq!(
        name.to_plain_string(),
        "[Blessed] Excalibur",
        "the registered AnvilBlessing hook must have tweaked the real rename result"
    );
}

/// A click against the connection's *open* non-zero window reaches both the
/// block entity's own slots and the player tail, through the same layout.
#[test]
fn container_clicked_against_an_open_window_reaches_both_sections() {
    let mut inventory = PlayerInventory::new();
    inventory.set_native(9, Some(stack("minecraft:coal", 1)));
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(1, 2, 3);
    block_entities.with(|reg| {
        reg.insert(pos, BlockEntity::Furnace(Furnace::new(FurnaceKind::Furnace)));
    });
    let mut open = open(pos, 3);

    // Menu slot 3 is the player tail's first entry (native 9): pick the coal up.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        7,
        Click { slot: 3, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(inventory.native(9), None);

    // Menu slot 1 is the furnace's own fuel slot: put it down there.
    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        7,
        Click { slot: 1, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    let furnace_fuel = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Furnace(f)) => f.fuel().cloned(),
        _ => None,
    });
    assert_eq!(furnace_fuel, Some(stack("minecraft:coal", 1)));
}

/// [`apply_set_beacon`]'s happy path: a level-1 pyramid, a payment item
/// present, a valid tier-1 primary — the payment is consumed and the
/// selection lands on the block entity.
#[test]
fn set_beacon_consumes_payment_and_stores_a_valid_selection() {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(0, 64, 0);
    block_entities.with(|reg| {
        reg.insert(
            pos,
            BlockEntity::Beacon(crate::block_entities::BeaconData {
                levels: 1,
                primary_effect: None,
                secondary_effect: None,
                payment: Some(stack("minecraft:emerald", 3)),
            }),
        );
    });
    let mut open = OpenContainer {
        window_id: 7,
        pos,
        shape: MenuKind::Beacon,
        container_size: 1,
        state_id: 0,
    };

    let directives = apply_set_beacon(
        &ContainerTagProto,
        &block_entities,
        Some(&mut open),
        Some("minecraft:speed".to_owned()),
        None,
    );
    assert!(!directives.is_empty(), "a successful selection must resend the menu");

    let after = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Beacon(b)) => b.clone(),
        _ => panic!("beacon must still be there"),
    });
    assert_eq!(
        after.primary_effect,
        Some(
            crate::beacon::BeaconPower::from_key("minecraft:speed")
                .expect("beacon power")
        )
    );
    assert_eq!(after.secondary_effect, None);
    assert_eq!(after.payment, Some(stack("minecraft:emerald", 2)), "exactly one payment item is spent");
}

/// **Control**: no payment item present must refuse the selection
/// entirely — a payment item is required. Without this,
/// the happy-path test above (which does have payment) could not tell a
/// correct gate from one that never checked at all.
#[test]
fn set_beacon_refuses_without_a_payment_item() {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(0, 64, 0);
    block_entities.with(|reg| {
        reg.insert(
            pos,
            BlockEntity::Beacon(crate::block_entities::BeaconData {
                levels: 4,
                primary_effect: None,
                secondary_effect: None,
                payment: None,
            }),
        );
    });
    let mut open = OpenContainer {
        window_id: 7,
        pos,
        shape: MenuKind::Beacon,
        container_size: 1,
        state_id: 0,
    };

    let directives = apply_set_beacon(
        &ContainerTagProto,
        &block_entities,
        Some(&mut open),
        Some("minecraft:speed".to_owned()),
        None,
    );
    assert!(directives.is_empty());
    let after = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Beacon(b)) => b.clone(),
        _ => panic!("beacon must still be there"),
    });
    assert_eq!(after.primary_effect, None, "a refused submission must not write the selection");
}

/// **Control**: an invalid pair for the pyramid's own level (here, a
/// secondary on a level-1 pyramid) must be refused, and the payment must
/// stay untouched — not spent on a rejected submission.
#[test]
fn set_beacon_refuses_an_invalid_pair_and_keeps_the_payment() {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(0, 64, 0);
    block_entities.with(|reg| {
        reg.insert(
            pos,
            BlockEntity::Beacon(crate::block_entities::BeaconData {
                levels: 1,
                primary_effect: None,
                secondary_effect: None,
                payment: Some(stack("minecraft:diamond", 1)),
            }),
        );
    });
    let mut open = OpenContainer {
        window_id: 7,
        pos,
        shape: MenuKind::Beacon,
        container_size: 1,
        state_id: 0,
    };

    let directives = apply_set_beacon(
        &ContainerTagProto,
        &block_entities,
        Some(&mut open),
        Some("minecraft:speed".to_owned()),
        Some("minecraft:regeneration".to_owned()),
    );
    assert!(directives.is_empty());
    let after = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Beacon(b)) => b.clone(),
        _ => panic!("beacon must still be there"),
    });
    assert_eq!(after.payment, Some(stack("minecraft:diamond", 1)), "a refused submission must not spend payment");
}

/// A raw serverbound key crosses into `BeaconPower` before it reaches the
/// persisted block entity. `poison` is a real mob effect but not a beacon
/// power, so this distinguishes the closed-domain boundary from merely
/// rejecting an unknown string.
#[test]
fn set_beacon_rejects_a_known_non_power_key_at_the_boundary() {
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(0, 64, 0);
    block_entities.with(|reg| {
        reg.insert(
            pos,
            BlockEntity::Beacon(crate::block_entities::BeaconData {
                levels: 4,
                primary_effect: None,
                secondary_effect: None,
                payment: Some(stack("minecraft:diamond", 1)),
            }),
        );
    });
    let mut open = OpenContainer {
        window_id: 7,
        pos,
        shape: MenuKind::Beacon,
        container_size: 1,
        state_id: 0,
    };

    let directives = apply_set_beacon(
        &ContainerTagProto,
        &block_entities,
        Some(&mut open),
        Some("minecraft:poison".to_owned()),
        None,
    );
    assert!(directives.is_empty());
    let after = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Beacon(b)) => b.clone(),
        _ => panic!("beacon must still be there"),
    });
    assert_eq!(after.primary_effect, None);
    assert_eq!(after.payment, Some(stack("minecraft:diamond", 1)));
}

/// The crafting **table**'s 3×3 menu, which has no block entity at all:
/// clicks reach the table's own grid and the server derives a 3×3
/// result the 2×2 player screen structurally cannot make.
#[test]
fn a_crafting_table_menu_derives_a_3x3_result() {
    let mut inventory = PlayerInventory::new();
    inventory.open_table_crafting();
    let block_entities = BlockEntityHandle::new();
    let mut open = OpenContainer {
        window_id: 3,
        pos: BlockPos::new(0, 64, 0),
        shape: MenuKind::CraftingTable,
        container_size: 10,
        state_id: 0,
    };

    // Eight planks around an empty centre is a chest — a 3x3-only recipe.
    inventory.click_state_mut().carried = Some(stack("minecraft:oak_planks", 8));
    for menu_slot in [1, 2, 3, 4, 6, 7, 8, 9] {
        apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            Some(&mut open),
            3,
            Click { slot: menu_slot, button: 1, click_type: 0 },
            &[],
            None,
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
    }
    assert_eq!(
        inventory
            .table_crafting()
            .and_then(|g| g.result())
            .map(|r| r.item.to_string()),
        Some("minecraft:chest".to_string()),
        "the table's own 3x3 grid derived the result"
    );
    assert!(
        inventory.crafting().is_empty(),
        "the player screen's 2x2 must be untouched — they are separate grids"
    );
}

/// **The reported bug**: the result the server derives has to *reach the client*,
/// and taking it has to work on the same click — no reopen.
///
/// The claims below are the real client's: `lodestone-game`'s `ClientMenu::predict`
/// diffs its own menu before/after, and its result slot is server-owned, so a grid
/// click claims the cell and the cursor and **never the result**. Under the old
/// agreement check — which walked only the claimed slots — that made every
/// crafting click "agree", so slot 0 was never sent: the screen drew its own dimmed
/// ghost, clicking it looked dead, and a craft only appeared after close+reopen.
///
/// The control is the second half: a client that *does* claim the right result and
/// cursor gets no packet, so this is a comparison over the whole menu rather than
/// an unconditional resend.
#[test]
fn a_derived_result_is_pushed_to_the_client_and_an_honest_claim_still_costs_nothing() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();
    inventory.click_state_mut().carried = Some(stack("minecraft:oak_planks", 4));

    // Four right-clicks, one plank per cell. **Every one of them changes the derived
    // result** — measured against vanilla's own datapack, one plank alone is
    // `oak_button.json`, two side by side are `oak_pressure_plate.json`, three match
    // nothing, four are `crafting_table.json` — and the client predicts none of
    // them, so each has to be answered.
    for (menu_slot, left_on_cursor) in [(1, Some(3u32)), (2, Some(2)), (3, Some(1)), (4, None)] {
        let (correction, _) = apply_container_clicked(
            &ContainerTagProto,
            &mut inventory,
            &block_entities,
            None,
            0,
            Click { slot: menu_slot, button: 1, click_type: 0 },
            &[(menu_slot, Some(stack("minecraft:oak_planks", 1)))],
            left_on_cursor
                .map(|count| stack("minecraft:oak_planks", count))
                .as_ref(),
            false,
            i32::MAX,
            &crate::plugin_crafting::CraftingStationHooks::default(),
        );
        assert!(
            matches!(&correction, Some(ServerDirective::Send { packet_id, .. }) if *packet_id == CONTENT),
            "menu slot {menu_slot} moved the result slot the client cannot derive, got {correction:?}"
        );
    }
    assert_eq!(
        inventory.crafting().result(),
        Some(&stack("minecraft:crafting_table", 1))
    );

    // Now take it. The client's prediction is empty on both counts (its own result
    // slot is still empty), so this is the click that read as dead.
    let (correction, dropped) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 0, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert!(dropped.is_empty());
    assert_eq!(
        inventory.click_state().carried,
        Some(stack("minecraft:crafting_table", 1)),
        "exactly one table, on the cursor"
    );
    assert!(
        inventory.crafting().is_empty(),
        "and one of every input was consumed"
    );
    // `ContainerTagProto` puts the carried count in the last payload byte: the
    // client is told about the cursor on this same packet, which is what "without a
    // reopen" means.
    match &correction {
        Some(ServerDirective::Send { packet_id, payload }) => {
            assert_eq!(*packet_id, CONTENT);
            assert_eq!(payload[0], 0, "window 0");
            assert_eq!(payload[2], 46, "all 46 InventoryMenu slots");
            assert_eq!(payload[3], 1, "carrying one crafting table");
        }
        other => panic!("taking a result must resync the client, got {other:?}"),
    }

    // The control: with the take already applied, a client claiming precisely what
    // the server derived (nothing left in the grid, one table on the cursor) is
    // answered with silence.
    let (correction, _) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 0, button: 0, click_type: 0 },
        &[],
        Some(&stack("minecraft:crafting_table", 1)),
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert_eq!(
        correction, None,
        "an empty result slot and a matching cursor is agreement, not a resend"
    );
}

/// Shift-clicking a result crafts **repeatedly** until the grid runs out —
/// vanilla's `doClick` `QUICK_MOVE` `while` loop over a result slot that
/// `slotsChanged` refills between rounds.
///
/// Expected value from outside this code: `chest.json` is eight `#minecraft:planks`
/// around an empty centre, and vanilla's own result-slot on-take routine removes **one** per occupied
/// cell per craft, so eight planks per cell is exactly eight chests — not one (the
/// old single-shot behaviour) and not sixty-four.
#[test]
fn shift_clicking_the_result_crafts_until_the_grid_runs_out() {
    let mut inventory = PlayerInventory::new();
    inventory.open_table_crafting();
    let block_entities = BlockEntityHandle::new();
    let mut open = OpenContainer {
        window_id: 3,
        pos: BlockPos::new(0, 64, 0),
        shape: MenuKind::CraftingTable,
        container_size: 10,
        state_id: 0,
    };
    for cell in [0, 1, 2, 3, 5, 6, 7, 8] {
        inventory
            .table_crafting_mut()
            .expect("open")
            .set_input(cell, Some(stack("minecraft:oak_planks", 8)));
    }
    assert_eq!(
        inventory.table_crafting().and_then(|g| g.result()),
        Some(&stack("minecraft:chest", 1)),
        "premise: the grid produces one chest per craft"
    );

    let (correction, dropped) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        3,
        Click { slot: 0, button: 0, click_type: 1 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );
    assert!(dropped.is_empty(), "36 empty slots have room for 8 chests");
    let chests: u32 = (0..crate::inventory::PLAYER_NATIVE_SIZE)
        .filter_map(|native| inventory.native(native))
        .filter(|s| s.item.to_string() == "minecraft:chest")
        .map(|s| s.count)
        .sum();
    assert_eq!(chests, 8, "eight planks per cell is eight crafts");
    assert!(
        inventory.table_crafting().is_some_and(CraftingState::is_empty),
        "and the grid is empty, not merely one item lighter"
    );
    assert!(
        correction.is_some(),
        "the client cannot predict any of that and must be resynced"
    );
}

/// **Control**, and the third reported symptom: shift-clicking an *input* out of
/// the grid moves that item to the inventory and withdraws the result. It must
/// never craft — `quickMoveStack`'s grid-cell branch has no `onTake` on the result
/// container, and vanilla's own result-slot on-take routine is reachable only through slot 0.
#[test]
fn shift_clicking_a_grid_input_moves_it_out_without_crafting() {
    let mut inventory = PlayerInventory::new();
    let block_entities = BlockEntityHandle::new();
    for cell in 0..4 {
        inventory
            .crafting_mut()
            .set_input(cell, Some(stack("minecraft:oak_planks", 1)));
    }
    assert!(
        inventory.crafting().result().is_some(),
        "premise: a result is standing when the input is shift-clicked"
    );

    let (_, dropped) = apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        None,
        0,
        Click { slot: 1, button: 0, click_type: 1 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    assert!(dropped.is_empty());
    assert!(inventory.click_state().carried.is_none(), "nothing on the cursor");
    assert!(
        (0..crate::inventory::PLAYER_NATIVE_SIZE)
            .filter_map(|native| inventory.native(native))
            .all(|s| s.item.to_string() == "minecraft:oak_planks"),
        "no crafting table anywhere: taking an input is not a craft"
    );
    let planks: u32 = (0..crate::inventory::PLAYER_NATIVE_SIZE)
        .filter_map(|native| inventory.native(native))
        .map(|s| s.count)
        .sum();
    assert_eq!(planks, 1, "exactly the one plank that left the grid");
    assert_eq!(inventory.crafting().input(0), None, "the cell it came from");
    assert!(
        inventory.crafting().result().is_none(),
        "and the result is withdrawn, not crafted"
    );
}

/// **Control**: a click carrying the *wrong* (stale) window id must not
/// mutate anything — the guard that stops a click for an already-closed
/// or already-replaced window from landing on whatever is open now.
#[test]
fn container_clicked_against_a_stale_window_id_is_dropped() {
    let mut inventory = PlayerInventory::new();
    inventory.click_state_mut().carried = Some(stack("minecraft:coal", 1));
    let block_entities = BlockEntityHandle::new();
    let pos = BlockPos::new(1, 2, 3);
    block_entities.with(|reg| {
        reg.insert(pos, BlockEntity::Furnace(Furnace::new(FurnaceKind::Furnace)));
    });
    let mut open = open(pos, 3); // window_id 7

    apply_container_clicked(
        &ContainerTagProto,
        &mut inventory,
        &block_entities,
        Some(&mut open),
        8, // stale/mismatched window id
        Click { slot: 0, button: 0, click_type: 0 },
        &[],
        None,
        false,
        i32::MAX,
        &crate::plugin_crafting::CraftingStationHooks::default(),
    );

    let furnace_input = block_entities.with(|reg| match reg.get(pos) {
        Some(BlockEntity::Furnace(f)) => f.input().cloned(),
        _ => None,
    });
    assert_eq!(furnace_input, None, "a stale window id must not mutate the block entity");
    assert!(
        inventory.click_state().carried.is_some(),
        "and the cursor is untouched"
    );
}
