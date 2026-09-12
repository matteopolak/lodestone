use super::common::*;

/// **The second block-drop gate: the drop can be picked up, and
/// the pickup reaches the client.**
///
/// The chain under test is the player update → item pickup → inventory write →
/// a window-0 `container_set_slot`. Two assertions that a
/// weaker gate would omit:
///
/// * the item entity is **gone** from the sim afterwards — a pickup that credits
///   the inventory without removing the entity duplicates items forever, and an
///   inventory-only assertion cannot see it;
/// * the client was told **which slot** changed, and the announced slot is `36`
///   — window-0 menu coordinates for native hotbar slot `0`, the selected one.
///   A pickup that wrote the right item into the right native slot but announced
///   a native index instead of a menu index puts cobblestone in the player's
///   *main storage* row on screen while the server thinks it is in the hand.
///
/// `tick_for(10)` before the walk is required: the spawn path assigns a
/// 10-tick pickup delay, so the drop is deliberately
/// uncollectable when it spawns. See the control below.
#[tokio::test(start_paused = true)]
async fn a_dropped_item_is_collected_into_the_hotbar_and_announced() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Collector", 1).await;
    break_with_a_pickaxe(&mut client, 2, BREAK_POS).await;
    let _ = drain_available(&mut client).await;
    assert_eq!(mobs.with(|sim| sim.item_count()), 1, "precondition: a drop exists");

    // Let the 10-tick pickup delay elapse through the simulation's normal tick
    // path rather than advancing the lifecycle in isolation.
    mobs.with(|sim| sim.tick_for(12));

    // Stand on the block that was broken. The drop scatters within
    // ±0.25 of the block centre, and the pickup volume reaches 1.425 blocks
    // horizontally, so the centre is comfortably inside it from any roll.
    send_player_moved(
        &mut client,
        f64::from(BREAK_POS.x) + 0.5,
        f64::from(BREAK_POS.y),
        f64::from(BREAK_POS.z) + 0.5,
    )
    .await;
    let packets = drain_available(&mut client).await;

    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "the collected item entity must be removed from the world, not merely \
         copied into the inventory — otherwise it is an infinite item source"
    );

    let writes = container_slot_writes(&packets);
    assert_eq!(
        writes,
        vec![(36, "minecraft:cobblestone".to_owned(), 1)],
        "exactly one window-0 slot write, announcing menu slot 36 (native hotbar \
         slot 0, the selected one) holding one cobblestone; got {writes:?}"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

// ---------------------------------------------------------------------------
// The pickup animation (`TAKE_ITEM_ENTITY`) and its ordering.
// ---------------------------------------------------------------------------

/// Fills every one of the player's 36 native inventory slots with `filler`, then
/// overwrites `partial_slot` with `partial` — so a subsequent pickup of a stackable
/// item can only partly fit.
///
/// Creative writes, bracketed into and out of creative by [`send_creative_slot`],
/// which is how a real client gives itself an item. Menu coordinates: native `0..9`
/// is the hotbar at menu `36..45`, native `9..36` is storage at menu `9..36`.
async fn fill_inventory(
    client: &mut Connection<DuplexStream>,
    filler: &ItemStack,
    partial_slot: i16,
    partial: &ItemStack,
) {
    for native in 0..36_i16 {
        let menu = if native < 9 { native + 36 } else { native };
        send_creative_slot(client, menu, Some(filler)).await;
    }
    send_creative_slot(client, partial_slot, Some(partial)).await;
}

/// Every `TAKE_ITEM_ENTITY_S2C` in `packets`, as `(index, item_entity_id,
/// collector_id, amount)`, plus the index of the first `REMOVE_ENTITIES_S2C`
/// mentioning `item_entity_id`.
fn take_and_remove_indices(
    packets: &[(i32, Vec<u8>)],
    item_entity_id: i32,
) -> (Vec<(usize, i32, i32, i32)>, Option<usize>) {
    let mut takes = Vec::new();
    let mut remove_at = None;
    for (i, (id, payload)) in packets.iter().enumerate() {
        if *id == TAKE_ITEM_ENTITY_S2C {
            let mut r = Reader::new(payload);
            let item = r.var_i32().expect("item entity id");
            let collector = r.var_i32().expect("collector id");
            let amount = r.var_i32().expect("amount");
            takes.push((i, item, collector, amount));
        } else if *id == REMOVE_ENTITIES_S2C && remove_at.is_none() {
            let mut r = Reader::new(payload);
            let count = r.var_i32().expect("count");
            for _ in 0..count {
                if r.var_i32().expect("removed id") == item_entity_id {
                    remove_at = Some(i);
                    break;
                }
            }
        }
    }
    (takes, remove_at)
}

/// **The pickup animation is an outbound integration path.** The client consumes
/// `TAKE_ITEM_ENTITY`, while the server must emit it before removing the item.
///
/// # The claim, and why it needs a *full* pickup
///
/// **The take must precede the `REMOVE_ENTITIES` for the same entity.** The client
/// keeps the entity alive to interpolate it toward the collector and removes it
/// when the animation ends. A
/// server that removes first produces **no animation at all** with the packet present
/// and correct on the wire — precisely the way a superficially correct wire trace
/// can still omit the animation.
/// Asserted as an index comparison, which is a counter.
///
/// The pickup is therefore **full**: a partial one leaves the entity alive by
/// construction, so there is no removal to order against and the claim would be
/// unobservable. `amount`'s own discriminating case is the partial one and lives in
/// [`a_partial_pickup_announces_the_original_stack_count_not_the_amount_banked`].
///
/// The rig passes the `MobHandle` as the **entity source** as well, which no other
/// test in this file does, because `NoEntities` streams nothing and there would be no
/// `REMOVE_ENTITIES` at all.
#[tokio::test(start_paused = true)]
async fn a_pickup_announces_the_take_before_removing_the_item_entity() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let entities = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &entities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Magpie", 1).await;

    // Count 5 into an empty inventory: all of it fits, so the entity is removed.
    // Spawned directly because the block-break path pops exactly one cobblestone, and
    // a count of 1 could not tell `amount` apart from a hardcoded `1`.
    let item_entity_id = mobs.with(|sim| {
        sim.spawn_item(
            ResourceKey::from_str("minecraft:cobblestone").expect("valid key"),
            Vec3::new(
                f64::from(BREAK_POS.x) + 0.5,
                f64::from(BREAK_POS.y),
                f64::from(BREAK_POS.z) + 0.5,
            ),
            Vec3::new(0.0, 0.0, 0.0),
            ItemLifecycle {
                age: 20,
                pickup_delay: 0,
                count: 5,
                max_stack_size: 64,
            },
        )
    });

    // Stand above it first so the entity is streamed to the client *before* the
    // pickup. Without this the spawn and the removal could land in one pass and the
    // ordering claim would be about a different pair of packets.
    send_player_moved(
        &mut client,
        f64::from(BREAK_POS.x) + 0.5,
        f64::from(BREAK_POS.y) + 3.0,
        f64::from(BREAK_POS.z) + 0.5,
    )
    .await;
    let _ = drain_available(&mut client).await;

    send_player_moved(
        &mut client,
        f64::from(BREAK_POS.x) + 0.5,
        f64::from(BREAK_POS.y),
        f64::from(BREAK_POS.z) + 0.5,
    )
    .await;
    let packets = drain_available(&mut client).await;

    let (takes, remove_at) = take_and_remove_indices(&packets, item_entity_id);
    let mut problems: Vec<String> = Vec::new();

    match takes.as_slice() {
        [] => problems.push(
            "no TAKE_ITEM_ENTITY was sent at all: the pickup animation packet has no \
             producer, which is the reported bug"
                .to_owned(),
        ),
        [(take_at, item, collector, amount)] => {
            if *item != item_entity_id {
                problems.push(format!(
                    "the take names item entity {item}, but the drop is {item_entity_id}"
                ));
            }
            if *collector != LOCAL_PLAYER_ENTITY_ID {
                problems.push(format!(
                    "the take names collector {collector}, but the player is \
                     {LOCAL_PLAYER_ENTITY_ID}; the client lerps the item toward this id, \
                     so a wrong one sends the item to the wrong place"
                ));
            }
            if *amount != 5 {
                problems.push(format!(
                    "amount is {amount}, but the stack held 5; a hardcoded 1 is audible, \
                     because the amount drives the pickup sound's pitch"
                ));
            }
            match remove_at {
                None => problems.push(
                    "the item entity was never removed, so the pickup is an infinite \
                     item source and the ordering claim is moot"
                        .to_owned(),
                ),
                Some(remove_at) if remove_at <= *take_at => problems.push(format!(
                    "REMOVE_ENTITIES (packet {remove_at}) reached the client at or before \
                     the take (packet {take_at}). The client keeps the entity alive to \
                     interpolate it and removes it when the animation ends, so it has \
                     nothing left to animate: no pickup animation, with the packet \
                     present and correct"
                )),
                Some(_) => {}
            }
        }
        many => problems.push(format!(
            "expected exactly one take, got {}: {many:?}",
            many.len()
        )),
    }

    assert!(
        problems.is_empty(),
        "the pickup animation is wrong in {} way(s):\n  {}",
        problems.len(),
        problems.join("\n  "),
    );

    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        0,
        "precondition: the whole stack fitted, so the entity must be gone — otherwise \
         there was no removal for the ordering assertion to be about"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **`amount` is the original stack count, not the amount banked — and only a partial pickup can
/// tell them apart.**
///
/// The server records the stack count *before* adding it to the inventory, which
/// shrinks the stack in place. On a **full** pickup the two counts coincide, so that
/// case measures only that the code runs — the same corollary that made `oak_leaves`
/// the wrong choice for the item-collision gates.
///
/// The inventory is filled so exactly 2 of a 5-stack fit: the original count is `5`,
/// banked is `2`, and the assertion is `5`. The surviving entity's count is checked too, because
/// that is what establishes the pickup really was partial — without it a full pickup
/// would satisfy the `amount == 5` assertion and prove nothing.
#[tokio::test(start_paused = true)]
async fn a_partial_pickup_announces_the_original_stack_count_not_the_amount_banked() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let entities = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &entities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Fussy", 1).await;

    // 35 slots of a *different* item plus one holding 62 cobblestone, so only 2 of a
    // 5-cobblestone pickup can be placed.
    let filler = ItemStack::new(
        ResourceKey::from_str("minecraft:stone").expect("valid key"),
        64,
    );
    let nearly_full = ItemStack::new(
        ResourceKey::from_str("minecraft:cobblestone").expect("valid key"),
        62,
    );
    fill_inventory(&mut client, &filler, 36, &nearly_full).await;
    let _ = drain_available(&mut client).await;

    let item_entity_id = mobs.with(|sim| {
        sim.spawn_item(
            ResourceKey::from_str("minecraft:cobblestone").expect("valid key"),
            Vec3::new(
                f64::from(BREAK_POS.x) + 0.5,
                f64::from(BREAK_POS.y),
                f64::from(BREAK_POS.z) + 0.5,
            ),
            Vec3::new(0.0, 0.0, 0.0),
            ItemLifecycle {
                age: 20,
                pickup_delay: 0,
                count: 5,
                max_stack_size: 64,
            },
        )
    });

    send_player_moved(
        &mut client,
        f64::from(BREAK_POS.x) + 0.5,
        f64::from(BREAK_POS.y),
        f64::from(BREAK_POS.z) + 0.5,
    )
    .await;
    let packets = drain_available(&mut client).await;

    let (takes, _) = take_and_remove_indices(&packets, item_entity_id);
    let amounts: Vec<i32> = takes.iter().map(|&(_, _, _, amount)| amount).collect();
    assert_eq!(
        amounts,
        vec![5],
        "the take must report orgCount = 5, the stack's count before the inventory \
         took any of it. `2` means the banked amount was sent instead — which is \
         indistinguishable from correct on a full pickup, and is the reason this one \
         is partial"
    );

    assert_eq!(
        mobs.with(|sim| sim.item_position(item_entity_id)).is_some(),
        true,
        "precondition: the pickup must really have been partial, so the entity \
         survives holding the remainder. If it is gone the whole stack was banked and \
         orgCount could not be told apart from the banked amount"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **The control for the `banked > 0` gate: a pickup that fits nowhere announces
/// nothing.**
///
/// A pickup is announced only when adding to the inventory returns true. With every
/// slot full of a different item, a cobblestone drop fits nowhere, so there is no take and
/// the entity stays. Without this, an implementation that announced a take on every
/// overlap would pass the gate above.
#[tokio::test(start_paused = true)]
async fn a_pickup_into_a_full_inventory_announces_no_take() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let entities = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &entities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Hoarder", 1).await;

    // Every slot holds a full stack of something else, so nothing can be added.
    let filler = ItemStack::new(
        ResourceKey::from_str("minecraft:stone").expect("valid key"),
        64,
    );
    fill_inventory(&mut client, &filler, 36, &filler).await;
    let _ = drain_available(&mut client).await;

    let item_entity_id = mobs.with(|sim| {
        sim.spawn_item(
            ResourceKey::from_str("minecraft:cobblestone").expect("valid key"),
            Vec3::new(
                f64::from(BREAK_POS.x) + 0.5,
                f64::from(BREAK_POS.y),
                f64::from(BREAK_POS.z) + 0.5,
            ),
            Vec3::new(0.0, 0.0, 0.0),
            ItemLifecycle {
                age: 20,
                pickup_delay: 0,
                count: 1,
                max_stack_size: 64,
            },
        )
    });

    send_player_moved(
        &mut client,
        f64::from(BREAK_POS.x) + 0.5,
        f64::from(BREAK_POS.y),
        f64::from(BREAK_POS.z) + 0.5,
    )
    .await;
    let packets = drain_available(&mut client).await;

    let (takes, _) = take_and_remove_indices(&packets, item_entity_id);
    assert!(
        takes.is_empty(),
        "nothing fitted, so vanilla's `add(...)` would have returned false and no \
         take is sent; got {takes:?}"
    );
    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        1,
        "precondition: the drop must still be in the world, or this measured a \
         successful pickup that merely failed to announce itself"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **The control for the pickup gate: the delay is real.**
///
/// Identical to the test above but with **no** `tick_for`, so the drop still
/// carries its full 10-tick pickup delay. The player stands exactly on
/// it and must collect nothing.
///
/// This is the control that matters most here, because its absence is invisible:
/// a server that ignored `pickup_delay` entirely would pass the positive gate
/// (the item is collected — just a few ticks early), and the observable failure would
/// be that a player mining in survival never sees a drop bounce, because they
/// re-absorb it on the spawning tick. It also proves the pickup sweep is
/// genuinely gated rather than firing on every movement packet regardless.
#[tokio::test(start_paused = true)]
async fn a_freshly_popped_drop_is_not_collectable_before_its_delay_elapses() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Impatient", 1).await;
    break_with_a_pickaxe(&mut client, 2, BREAK_POS).await;
    let _ = drain_available(&mut client).await;
    assert_eq!(mobs.with(|sim| sim.item_count()), 1);

    // No `tick_for`: the pickup delay is still 10.
    send_player_moved(
        &mut client,
        f64::from(BREAK_POS.x) + 0.5,
        f64::from(BREAK_POS.y),
        f64::from(BREAK_POS.z) + 0.5,
    )
    .await;
    let packets = drain_available(&mut client).await;

    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        1,
        "the drop still carries its 10-tick pickup delay, so standing on it must \
         collect nothing"
    );
    assert!(
        container_slot_writes(&packets).is_empty(),
        "and no slot write may be announced: {:?}",
        container_slot_writes(&packets)
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}

/// **The other control on the pickup volume: distance is real.**
///
/// The delay has elapsed and the drop is collectable, but the player walks to a
/// position well outside the pickup sweep's inflated box (10 blocks away). Nothing
/// may be collected.
///
/// Without this, the positive pickup gate is satisfied by a server that collects
/// every drop in the world on any movement packet — which would look completely
/// correct in the one scene where the only drop is the one at your feet. The
/// separated position control prevents the fixture from hiding that broad rule.
#[tokio::test(start_paused = true)]
async fn a_drop_outside_the_pickup_volume_is_not_collected() {
    let (client_end, server_end) = memory_pair();
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &StoneSource,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Distant", 1).await;
    break_with_a_pickaxe(&mut client, 2, BREAK_POS).await;
    let _ = drain_available(&mut client).await;
    mobs.with(|sim| sim.tick_for(12));

    // Ten blocks away — far outside the 1.425-block horizontal reach, and inside
    // the same served column so the connection is not disconnected for a view
    // change mid-test.
    send_player_moved(
        &mut client,
        f64::from(BREAK_POS.x) + 10.5,
        f64::from(BREAK_POS.y),
        f64::from(BREAK_POS.z) + 0.5,
    )
    .await;
    let packets = drain_available(&mut client).await;

    assert_eq!(
        mobs.with(|sim| sim.item_count()),
        1,
        "a collectable drop ten blocks away must stay in the world; if this is 0 \
         the pickup sweep ignores position and the positive gate proves nothing"
    );
    assert!(
        container_slot_writes(&packets).is_empty(),
        "and nothing may be credited: {:?}",
        container_slot_writes(&packets)
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}


/// **A banned uuid is refused at login**, before `login_success`, with
/// the canonical refusal translation key on the wire — and the identical connection is
/// admitted once the ban is lifted.
///
/// The lifted-ban arm is the control: without it, a test that only asserts the
/// refusal cannot tell "the ban was enforced" from "this fixture never joins".
