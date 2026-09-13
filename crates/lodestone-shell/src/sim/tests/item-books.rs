use super::*;
pub(crate) fn give_main_hand_item(sim: &mut Sim, item: &str) {
    let local = sim.local;
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: 0,
                item: Some(lodestone_model::ItemStack::new(
                    item.parse().expect("valid item id"),
                    1,
                )),
            });
        }
    });
}

/// Builds two predictions through the real placement state machine rather than
/// seeding its private ledger. The acknowledgements below must therefore prove
/// the packet clears the state a real right-click writes, not a test-only copy.
fn two_pending_placements() -> Placement {
    let clicked = BlockPos::new(12, 70, -9);
    let context = UseOnContext {
        hand: Hand::Main,
        clicked,
        face: BlockFace::Up,
        cursor: Vec3f::new(0.5, 0.5, 0.5),
        inside_block: false,
        rotation: Rotation::new(0.0, 0.0),
        sneaking: false,
        has_item_in_hand: true,
        placing: Some("minecraft:stone".parse().expect("valid block id")),
        orientation: OrientationKind::Fixed,
    };
    let facts = PlacementFacts {
        clicked,
        target: clicked,
        clicked_replaceable: true,
        clicked_interactable: false,
        target_replaceable: true,
        target_obstructed: false,
    };
    let mut placement = Placement::new();
    for expected_sequence in [
        lodestone_model::PredictionSequence::new(1),
        lodestone_model::PredictionSequence::new(2),
    ] {
        let decision = placement.use_on(&context, &facts);
        assert!(matches!(
            decision,
            UseOnDecision::Place {
                prediction: lodestone_game::placement::PlacePrediction { sequence, .. },
                ..
            } if sequence == expected_sequence
        ));
    }
    placement
}

#[test]
fn block_changed_ack_retires_only_the_predictions_the_server_has_processed() {
    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.attach_net(net);
    sim.write(|world| {
        world.insert_resource(PlacementPredictor(two_pending_placements()));
    });

    feed.send(NetUpdate::BlockChangedAck {
        sequence: lodestone_model::PredictionSequence::new(1),
    })
        .expect("loopback accepts the server acknowledgement");
    sim.poll_net();
    assert_eq!(
        sim.read(|world| world.resource::<PlacementPredictor>().0.pending().len()),
        1,
        "acknowledging sequence 1 must retain the newer sequence 2 prediction"
    );

    // A stale acknowledgement cannot retire the still-pending newer prediction.
    feed.send(NetUpdate::BlockChangedAck {
        sequence: lodestone_model::PredictionSequence::new(0),
    })
        .expect("loopback accepts a stale acknowledgement");
    sim.poll_net();
    assert_eq!(
        sim.read(|world| world.resource::<PlacementPredictor>().0.pending().len()),
        1,
        "a stale acknowledgement must not clear sequence 2"
    );

    feed.send(NetUpdate::BlockChangedAck {
        sequence: lodestone_model::PredictionSequence::new(2),
    })
        .expect("loopback accepts the final acknowledgement");
    sim.poll_net();
    assert!(
        sim.read(|world| world.resource::<PlacementPredictor>().0.pending().is_empty()),
        "the matching acknowledgement must retire the remaining prediction"
    );
}

/// Installs one toggleable `PlayerInteract` veto and records every context the
/// production ask site presents to it.
pub(crate) fn install_player_interact_veto(
    sim: &mut Sim,
    deny: std::sync::Arc<std::sync::atomic::AtomicBool>,
    seen: std::sync::Arc<std::sync::Mutex<Vec<lodestone_ecs::veto::VerbContext>>>,
) {
    let mut vetoes = lodestone_ecs::veto::ActionVetoes::default();
    vetoes.register(
        lodestone_ecs::veto::Verb::PlayerInteract,
        "shell-test",
        0,
        move |ctx| {
            seen.lock()
                .expect("context recorder is not poisoned")
                .push(*ctx);
            if deny.load(std::sync::atomic::Ordering::Relaxed) {
                lodestone_ecs::veto::Verdict::Deny
            } else {
                lodestone_ecs::veto::Verdict::Allow
            }
        },
    );
    sim.write(|world| world.insert_resource(vetoes));
}

/// Same idiom as [`give_main_hand_item`], carrying a real
/// `minecraft:written_book_content` — the component
/// `crates/protocol/v770/src/adapter/inventory.rs`'s
/// `read_written_book_content` populates off the wire, folded in through the
/// same `ClientEvent` production uses rather than written into the menu by
/// hand.
fn give_main_hand_written_book(sim: &mut Sim, title: &str, author: &str, generation: u8, pages: &[&str]) {
    let local = sim.local;
    let mut stack = lodestone_model::ItemStack::new(
        "minecraft:written_book".parse().expect("valid item id"),
        1,
    );
    stack.components.written_book_content = Some(lodestone_model::WrittenBookContent {
        title: title.to_owned(),
        author: author.to_owned(),
        generation,
        pages: pages.iter().map(|p| lodestone_model::Text::literal(*p)).collect(),
        resolved: true,
    });
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: 0,
                item: Some(stack.clone()),
            });
        }
    });
}

/// The book-reading screen's producer: a signed book in the main hand must
/// be reported with the title, author, generation and pages the wire
/// carried.
///
/// **This is the link that was missing**, not the decode: v770 has decoded
/// `minecraft:written_book_content` into `ItemComponents` for as long as
/// `book_content_wiring.rs` has existed, and
/// `lodestone_game::item::ItemStack::written_book_content` had **zero**
/// production readers — so a signed book folded into the menu correctly and
/// reached nothing at all. Right-clicking it did nothing and its tooltip
/// said "Written Book".
///
/// Fields are pairwise distinct (a title and an author that are different
/// words, generation `2` rather than the `0` a fresh signature carries, two
/// differing pages) so a transposition of the two adjacent strings cannot
/// survive.
#[test]
fn a_signed_book_in_hand_is_reported_with_its_own_metadata_and_pages() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    assert!(
        sim.written_book_in_hand().is_none(),
        "precondition: an empty hand holds no book"
    );

    give_main_hand_written_book(&mut sim, "Wandering Notes", "Steve", 2, &["First page", "Second page"]);

    let open = sim
        .written_book_in_hand()
        .expect("a signed book in the main hand must open the reading screen");
    assert_eq!(
        open,
        crate::menu::book_view::BookViewOpen {
            title: "Wandering Notes".to_owned(),
            author: "Steve".to_owned(),
            generation: 2,
            pages: vec![
                lodestone_model::ResolvedText::literal("First page"),
                lodestone_model::ResolvedText::literal("Second page"),
            ],
        }
    );
}

/// The two book screens must not answer for each other's item. Without this,
/// `try_use`'s fork could route a signed book into the *editor* (which would
/// then send an `EditBook` for an immutable book) or a draft into the reader.
///
/// Both directions, collected rather than asserted in the loop.
#[test]
fn the_two_book_screens_do_not_claim_each_others_item() {
    let mut failures = Vec::new();

    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    give_main_hand_written_book(&mut sim, "T", "A", 0, &["p"]);
    if sim.writable_book_in_hand().is_some() {
        failures.push("a signed book was claimed by the editor's producer");
    }
    if sim.written_book_in_hand().is_none() {
        failures.push("control: a signed book was not claimed by the reader's producer");
    }

    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    give_main_hand_item(&mut sim, "minecraft:writable_book");
    if sim.written_book_in_hand().is_some() {
        failures.push("an unsigned draft was claimed by the reader's producer");
    }
    if sim.writable_book_in_hand().is_none() {
        failures.push("control: an unsigned draft was not claimed by the editor's producer");
    }

    assert!(failures.is_empty(), "{failures:?}");
}

/// A server `OPEN_BOOK` packet names the hand it intends to display. This
/// keeps that selector through the relay and verifies the projection does not
/// silently choose the main hand when an off-hand book was requested.
#[test]
fn server_open_book_keeps_its_offhand_selector() {
    use crate::net::NetUpdate;

    let (net, _actions, feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:stick");

    let local = sim.local;
    let mut offhand_book = lodestone_model::ItemStack::new(
        "minecraft:written_book".parse().expect("valid item id"),
        1,
    );
    offhand_book.components.written_book_content = Some(lodestone_model::WrittenBookContent {
        title: "Offhand notes".to_owned(),
        author: "Alex".to_owned(),
        generation: 0,
        pages: vec![lodestone_model::Text::literal("the requested book")],
        resolved: true,
    });
    sim.write(|w| {
        w.get_mut::<lodestone_ecs::SessionMenus>(local)
            .expect("local player has menus")
            .0
            .apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: lodestone_game::menu::OFFHAND_NATIVE as i32,
                item: Some(offhand_book),
            });
    });

    feed.send(NetUpdate::BookOpened { main_hand: false })
        .expect("loopback is connected");
    sim.poll_net();

    assert_eq!(sim.take_pending_book_open(), Some(false));
    assert!(sim.written_book_in_hand_at(true).is_none());
    assert_eq!(
        sim.written_book_in_hand_at(false)
            .expect("off-hand book must be selected")
            .title,
        "Offhand notes"
    );
}

/// A lectern carries just its display-book slot, not the usual 36 appended
/// player slots. Its page lives in menu property zero and is the exact button
/// index returned to the UI.
#[test]
fn lectern_book_view_reads_slot_zero_and_container_page_data() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    let mut book = lodestone_model::ItemStack::new(
        "minecraft:written_book".parse().expect("valid item id"),
        1,
    );
    book.components.written_book_content = Some(lodestone_model::WrittenBookContent {
        title: "Lectern manual".to_owned(),
        author: "Librarian".to_owned(),
        generation: 1,
        pages: vec![
            lodestone_model::Text::literal("first"),
            lodestone_model::Text::literal("second"),
        ],
        resolved: true,
    });
    let local = sim.local;
    sim.write(|w| {
        let mut menus = w
            .get_mut::<lodestone_ecs::SessionMenus>(local)
            .expect("local player has menus");
        menus.0.apply(&lodestone_model::ClientEvent::ScreenOpened {
            window_id: 12,
            menu_type: "minecraft:lectern".parse().expect("valid menu id"),
            title: lodestone_model::Text::literal("Lectern"),
        });
        menus.0.apply(&lodestone_model::ClientEvent::ContainerContent {
            window_id: 12,
            state_id: lodestone_model::ContainerStateId::new(4),
            items: vec![Some(book)],
            carried_item: None,
        });
        menus.0.apply(&lodestone_model::ClientEvent::ContainerData {
            window_id: 12,
            property: 0,
            value: 1,
        });
    });

    let (window_id, open, page) = sim.lectern_book_view().expect("lectern book opens");
    assert_eq!(window_id, 12);
    assert_eq!(page, 1);
    assert_eq!(open.title, "Lectern manual");
    assert_eq!(open.pages[1], lodestone_model::ResolvedText::literal("second"));
}
