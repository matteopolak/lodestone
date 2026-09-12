use super::*;
fn give_main_hand_equippable_item(sim: &mut Sim, item: &str, slot: EquipmentSlot) {
    let local = sim.local;
    let mut stack =
        lodestone_model::ItemStack::new(item.parse().expect("valid item id"), 1);
    stack.components.equippable = Some(slot);
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: 0,
                item: Some(stack),
            });
        }
    });
}

/// `ClientAction::Stab` action must have a producer. The attack dispatch checks
/// the held item for the piercing-weapon component *before* the
/// normal ENTITY/BLOCK/MISS switch and takes it unconditionally — proved here
/// with an entity under the crosshair (the case the ordinary switch would
/// otherwise prefer, per `begin_attack_live_prefers_an_entity_target_over_mining`)
/// so a regression that let the entity switch win first would show up as a
/// missing `Stab`/extra `InteractEntity` rather than passing by coincidence.
#[test]
fn begin_attack_live_stabs_with_a_spear_instead_of_the_normal_attack_switch() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:diamond_spear");
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));

    sim.begin_attack_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter().any(|a| matches!(a, ClientAction::Stab)),
        "holding a piercing weapon must send Stab, got {sent:?}"
    );
    assert!(
        !sent.iter().any(|a| matches!(a, ClientAction::InteractEntity { .. })),
        "a piercing weapon must take over the switch entirely, not send the \
         ordinary attack packet too — got {sent:?}"
    );
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::SwingArm { hand: Hand::Main })),
        "vanilla still swings the main hand after piercingAttack — got {sent:?}"
    );
    let attacking = sim.read(|w| w.resource::<Attacking>().0);
    assert!(!attacking, "a stab must not arm the hold-to-mine loop either");
}

/// A non-spear weapon must not take the `Stab` branch — the negative control
/// for the item-identity gate `is_piercing_weapon` implements.
#[test]
fn begin_attack_live_does_not_stab_with_an_ordinary_sword() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:diamond_sword");
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));

    sim.begin_attack_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        !sent.iter().any(|a| matches!(a, ClientAction::Stab)),
        "an ordinary sword must never send Stab, got {sent:?}"
    );
    assert!(
        sent.iter().any(|a| matches!(
            a,
            ClientAction::InteractEntity {
                interaction: EntityInteraction::Attack,
                ..
            }
        )),
        "an ordinary sword must still take the normal attack switch, got {sent:?}"
    );
}

/// Finding 2 (combat scoping doc): before this fix, `use_item_live`
/// returned unconditionally after `interact_entity` whenever *any*
/// entity was targeted — hostile mobs included, the overwhelmingly
/// common combat case — so a bow or shield could never even start a use.
/// Vanilla's own `case ENTITY` only returns
/// on a *successful* interact; anything else falls through to the
/// generic use-item call (`:1730`) that actually raises a shield or
/// draws a bow.
///
/// This is the control the scoping doc asked for: it must fail
/// (`ClientAction::UseItem` absent) against the pre-fix `use_item_live`,
/// which this test's own doc-comment history confirms was checked by
/// hand (see the report for the reverted/restored run).
#[test]
fn use_item_live_falls_through_to_generic_use_with_an_entity_targeted() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:bow");
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        matches!(
            sent.first(),
            Some(ClientAction::InteractEntity {
                entity_id: 42,
                ..
            })
        ),
        "the entity interact itself must still be sent first, got {sent:?}"
    );
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::UseItem { hand: Hand::Main, .. })),
        "an entity target must fall through to the generic use-item send \
         (this is what raises a shield or draws a bow at a mob) — got {sent:?}"
    );
}

/// Finding 2's other half: with **no** target at all — open air, or a mob
/// just past block reach with nothing behind it — `use_item_live` used to
/// `return` with nothing sent. Vanilla's own `hitResult == null` path
/// skips the block/entity switch entirely and still reaches the
/// unconditional fallback.
#[test]
fn use_item_live_sends_generic_use_with_no_target_at_all() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:bow");
    assert!(sim.target().is_none(), "precondition: no block targeted");
    assert!(sim.entity_target().is_none(), "precondition: no entity targeted");

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::UseItem { hand: Hand::Main, .. })),
        "a miss (no block, no entity) must still send the generic use-item action \
         — got {sent:?}"
    );
}

/// Negative control for both tests above: an **empty** main hand must
/// send nothing generic to use, matching vanilla's own
/// `!heldItem.isEmpty()` guard at the same call site
///. Without this, "always send `UseItem`"
/// would satisfy the two tests above vacuously.
#[test]
fn use_item_generic_sends_nothing_with_an_empty_main_hand() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    assert!(sim.target().is_none());
    assert!(sim.entity_target().is_none());

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.is_empty(),
        "an empty main hand has nothing to use and must send nothing, got {sent:?}"
    );
}

#[test]
fn player_interact_veto_denies_entity_branch_before_any_effect() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:bow");
    sim.write(|world| world.resource_mut::<EntityRayTarget>().0 = Some(42));
    sim.set_ray_target_for_test(Some(RayHit::face_center([4, 64, 4], [0, 1, 0])));
    let local = sim.local;

    let deny = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    install_player_interact_veto(&mut sim, deny.clone(), seen.clone());

    sim.use_item_live();

    let denied_actions: Vec<ClientAction> =
        std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        denied_actions.is_empty(),
        "a denial must send nothing: {denied_actions:?}"
    );
    assert!(!sim.read(|world| world.resource::<UsingItem>().0));
    assert_eq!(
        sim.read(|world| world.get::<ItemUseEffects>(local).and_then(|effects| effects.0)),
        None,
        "a denial must not arm held-use movement effects"
    );
    assert_eq!(
        sim.read(|world| world.resource::<lodestone_ecs::player::ItemUseTicks>().0),
        None,
        "a denial must not start the held-use clock"
    );
    assert_eq!(
        *seen.lock().expect("context recorder is not poisoned"),
        vec![lodestone_ecs::veto::VerbContext::PlayerInteract {
            pos: None,
            target_entity_id: Some(42),
        }],
        "entity targeting wins over the simultaneous block-ray fixture"
    );
    assert!(!sim.body_pose.is_swinging(), "a denial must not start a local swing");

    deny.store(false, std::sync::atomic::Ordering::Relaxed);
    sim.use_item_live();
    let allowed_actions: Vec<ClientAction> =
        std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(matches!(
        allowed_actions.first(),
        Some(ClientAction::InteractEntity { entity_id: 42, .. })
    ));
    assert!(
        allowed_actions.iter().any(|action| matches!(
            action,
            ClientAction::UseItem { sequence: 1, .. }
        )),
        "the first allowed use must retain the first prediction sequence: {allowed_actions:?}"
    );
    assert!(sim.read(|world| world.resource::<UsingItem>().0));
    assert!(sim.body_pose.is_swinging(), "the allowed entity branch must still swing");
    assert_eq!(
        seen.lock().expect("context recorder is not poisoned").len(),
        2
    );
}

#[test]
fn player_interact_veto_denies_block_branch_before_prediction_or_send() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:bow");
    let clicked = BlockPos::new(4, 64, 4);
    let local = sim.local;
    sim.set_ray_target_for_test(Some(RayHit::face_center(
        [clicked.x, clicked.y, clicked.z],
        [0, 1, 0],
    )));

    let deny = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    install_player_interact_veto(&mut sim, deny.clone(), seen.clone());

    sim.use_item_live();

    let denied_actions: Vec<ClientAction> =
        std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        denied_actions.is_empty(),
        "a denial must send nothing: {denied_actions:?}"
    );
    let pending = sim.read(|world| world.resource::<PlacementPredictor>().0.pending().len());
    assert_eq!(pending, 0, "a denial must not add a pending prediction");
    assert!(!sim.read(|world| world.resource::<UsingItem>().0));
    assert_eq!(
        sim.read(|world| world.get::<ItemUseEffects>(local).and_then(|effects| effects.0)),
        None,
        "a denial must not arm held-use movement effects"
    );
    assert_eq!(
        *seen.lock().expect("context recorder is not poisoned"),
        vec![lodestone_ecs::veto::VerbContext::PlayerInteract {
            pos: Some(clicked),
            target_entity_id: None,
        }]
    );
    assert!(!sim.body_pose.is_swinging(), "a denial must not start a local swing");

    deny.store(false, std::sync::atomic::Ordering::Relaxed);
    sim.use_item_live();
    let allowed_actions: Vec<ClientAction> =
        std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        allowed_actions.iter().any(|action| matches!(
            action,
            ClientAction::UseItemOn { pos, sequence, .. }
                if *pos == clicked && sequence.raw() == 1
        )),
        "the first allowed block interaction must retain sequence one: {allowed_actions:?}"
    );
    assert!(
        allowed_actions.iter().any(|action| matches!(
            action,
            ClientAction::UseItem { sequence: 2, .. }
        )),
        "the allowed fallback must consume the next shared sequence: {allowed_actions:?}"
    );
    assert_eq!(
        seen.lock().expect("context recorder is not poisoned").len(),
        2
    );
}

#[test]
fn player_interact_veto_denies_air_branch_before_firework_boost() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:firework_rocket");
    sim.player_mut(|player| player.fall_flying = true);

    let deny = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
    let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    install_player_interact_veto(&mut sim, deny.clone(), seen.clone());

    sim.use_item_live();

    let denied_actions: Vec<ClientAction> =
        std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        denied_actions.is_empty(),
        "a denial must send nothing: {denied_actions:?}"
    );
    assert_eq!(
        sim.read(|world| world.resource::<lodestone_ecs::player::FireworkBoost>().0),
        0
    );
    assert_eq!(
        *seen.lock().expect("context recorder is not poisoned"),
        vec![lodestone_ecs::veto::VerbContext::PlayerInteract {
            pos: None,
            target_entity_id: None,
        }]
    );
    assert!(!sim.body_pose.is_swinging(), "a denial must not start a local swing");

    deny.store(false, std::sync::atomic::Ordering::Relaxed);
    sim.use_item_live();
    let allowed_actions: Vec<ClientAction> =
        std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        allowed_actions.iter().any(|action| matches!(
            action,
            ClientAction::UseItem { sequence: 1, .. }
        )),
        "the first allowed air use must retain sequence one: {allowed_actions:?}"
    );
    assert_eq!(
        sim.read(|world| world.resource::<lodestone_ecs::player::FireworkBoost>().0),
        20,
        "the allowed standard rocket use must start its exact 20-tick boost"
    );
    assert!(sim.body_pose.is_swinging(), "the allowed rocket use must still swing");
    assert_eq!(
        seen.lock().expect("context recorder is not poisoned").len(),
        2
    );
}

/// The third branch, and the one the two above left behind: with a **block**
/// under the crosshair, `use_item_live` used to `return` after its
/// `UseItemOn` + `SwingArm` pair and never reach the generic use.
///
/// That gate is worth more than the branch it fixes, because the server's
/// `ServerPlayerGameMode.useItemOn` never reaches `Item.use`: `USE_ITEM` is
/// the *only* route by which a boat is placed, food is eaten, a drink is
/// drunk, a helmet is equipped on use, or a bow starts drawing. All of that
/// worked aimed at open air or at a mob and did nothing aimed at a block —
/// which is why a boat could be placed over deep water (where the block ray
/// misses entirely) and not on a shoreline.
///
/// A boat is the subject deliberately: `minecraft:oak_boat` is not a block,
/// so it can never take the placement path and the fall-through is the whole
/// of its behaviour.
#[test]
fn use_item_live_falls_through_to_generic_use_with_a_block_targeted() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:oak_boat");
    sim.set_ray_target_for_test(Some(RayHit::face_center([4, 64, 4], [0, 1, 0])));
    assert!(sim.target().is_some(), "precondition: a block is targeted");
    assert!(
        sim.entity_target().is_none(),
        "precondition: no entity targeted, or the entity branch answers instead \
         and this proves nothing about the block one"
    );

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    let use_on = sent
        .iter()
        .position(|a| matches!(a, ClientAction::UseItemOn { .. }));
    let use_item = sent
        .iter()
        .position(|a| matches!(a, ClientAction::UseItem { hand: Hand::Main, .. }));
    assert!(
        use_on.is_some(),
        "the block interaction itself must still be sent, got {sent:?}"
    );
    assert!(
        use_item.is_some(),
        "a block target must also fall through to the generic use-item send — \
         this is the only packet that places a boat, eats food or draws a bow. \
         Got {sent:?}"
    );
    assert!(
        use_on < use_item,
        "`use_item_on` must precede `use_item` on the wire, as vanilla's \
         `case BLOCK` reaches `gameMode.useItem` only after `gameMode.useItemOn` \
         — got {sent:?}"
    );
}

/// The discriminating control for the branch above, and the reason it is not
/// an unconditional `use_item_generic()` call.
///
/// Vanilla's `case BLOCK` is **not** a `break` like `case ENTITY`'s:
/// `Minecraft.startUseItem` returns on `InteractionResult.Success` *and* on
/// `InteractionResult.Fail`, reaching the generic use only for a non-consuming
/// result. So a **placeable** item in hand must not produce a second send: the
/// item's own `useOn` is what answered, and falling through would let a carved
/// pumpkin aimed at an illegal face equip itself onto the player's head
/// instead of doing nothing.
///
/// `minecraft:stone` against this harness's target yields
/// `UseOnDecision::Nothing` (the loopback net publishes no `ClientHandle`, so
/// `block_at` reads `None` and the placement is declined as illegal) — which
/// is precisely vanilla's `Fail`, and precisely the arm an unconditional
/// fall-through would get wrong. Same `Sim`, same target, same call as the
/// gate above; only the held item differs.
#[test]
fn a_placeable_item_on_a_block_does_not_also_send_the_generic_use() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:stone");
    sim.set_ray_target_for_test(Some(RayHit::face_center([4, 64, 4], [0, 1, 0])));

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::UseItemOn { .. })),
        "control precondition: the block send must still happen, or the absence \
         below is just an unwired Sim — got {sent:?}"
    );
    assert!(
        !sent
            .iter()
            .any(|a| matches!(a, ClientAction::UseItem { .. })),
        "a block item answered the click itself, so the generic use must not \
         follow it — got {sent:?}"
    );
}

// -----------------------------------------------------------------------
// Armour equip prediction (`Sim::predict_equip_swap`)
// -----------------------------------------------------------------------
//
// Right-clicking an armour piece from the hotbar with nothing under the
// crosshair lands in `use_item_generic` — vanilla's own item `use()` falling
// into its own equip-swap component logic — the same landing site
// `use_item_live_sends_generic_use_with_no_target_at_all` already proves for
// a bow. Before `predict_equip_swap` existed that method sent `UseItem` and
// wrote nothing locally, so the helmet only appeared once the server's own
// `SET_SLOT` pair for the head and hotbar slots came back: the round trip
// the "missing client prediction" report was about.

/// The control: without the prediction, the head slot (menu index 5) starts
/// and stays empty across the call — this is the assertion that fails on the
/// pre-fix `use_item_generic`, which only sent `UseItem` and touched no menu
/// state at all.
#[test]
fn use_item_generic_predicts_the_armour_equip_swap_locally() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_equippable_item(&mut sim, "minecraft:diamond_helmet", EquipmentSlot::Head);
    assert!(sim.target().is_none(), "precondition: no block targeted");
    assert!(sim.entity_target().is_none(), "precondition: no entity targeted");
    assert!(
        sim.player_menu().slot_item(5).is_none(),
        "precondition: the head slot must start empty"
    );

    sim.use_item_live();

    // The prediction itself: the head slot shows the helmet *before* any
    // server acknowledgement — nothing here drains a socket or applies a
    // `ClientEvent` from the wire.
    let head = sim.player_menu().slot_item(5).cloned();
    assert!(
        head.as_ref()
            .is_some_and(|s| s.item().to_string() == "minecraft:diamond_helmet"),
        "the helmet must be predicted into the head slot locally, with no \
         server round trip — got {head:?}"
    );
    // A straight swap: nothing was worn before, so the hotbar slot the
    // helmet came from must be predicted empty, not still holding it.
    assert!(
        sim.player_menu().player_native(0).is_none(),
        "the hotbar slot the helmet was drawn from must be predicted empty \
         after the swap, got {:?}",
        sim.player_menu().player_native(0)
    );
    // The server is still authoritative and must hear the click — the
    // prediction is additive, not a replacement for the send.
    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::UseItem { hand: Hand::Main, .. })),
        "the swap must not skip the wire send — got {sent:?}"
    );
}

/// The reconciliation arm: a server that disagrees with the local guess must
/// win. Simulates the authoritative `SET_SLOT` pair a real server sends after
/// `broadcastChanges` diffs `player.inventoryMenu` — here, one that names a
/// *different* helmet than the one predicted (standing in for any server
/// refusal or race), which must overwrite the predicted contents rather than
/// leave them standing.
#[test]
fn a_disagreeing_server_set_slot_overwrites_the_predicted_equip() {
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_equippable_item(&mut sim, "minecraft:diamond_helmet", EquipmentSlot::Head);

    sim.use_item_live();
    assert_eq!(
        sim.player_menu()
            .slot_item(5)
            .map(|s| s.item().to_string()),
        Some("minecraft:diamond_helmet".to_string()),
        "precondition: the prediction landed the diamond helmet"
    );

    // The server's own truth disagrees — e.g. the swap never actually
    // happened server-side (enchantment/creative gating this client cannot
    // model, per `Sim::predict_equip_swap`'s own doc) and the head slot is
    // really still empty.
    let local = sim.local;
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&lodestone_model::ClientEvent::ContainerSlot {
                window_id: 0,
                state_id: lodestone_model::ContainerStateId::new(1),
                slot: 5,
                item: None,
            });
        }
    });

    assert!(
        sim.player_menu().slot_item(5).is_none(),
        "a disagreeing server SET_SLOT must overwrite the prediction, got {:?}",
        sim.player_menu().slot_item(5)
    );
}

// -----------------------------------------------------------------------
// Throwing a projectile item swings the arm
// -----------------------------------------------------------------------
//
// Vanilla's own snowball/egg/ender-pearl/throwable-potion use routines
// all return
// `InteractionResult.SUCCESS`, whose `swingSource()` is `CLIENT`
// — vanilla's own start-use-item routine swings on
// exactly that condition. `use_item_generic` is the shell's landing site for
// all four (none of them is a block or an `EntityRayTarget` hit in the common
// case). It used to call `swing_hand()` unconditionally whenever the main
// hand was non-empty; it now asks `generic_use_swings` first, so these gates
// are also the positive half of that table — the discriminating check is
// whether the swing actually **reaches the arm pose**, not merely whether
// `use_item_generic` was reached (`peak_swing_over` is `hand_swing_progress`
// wired the same way `a_queued_main_hand_swing_reaches_the_arm_pose` proves
// for a mining swing). The negative half is
// `a_use_that_vanilla_does_not_swing_for_leaves_the_arm_still`.

/// The common case: aiming at open air (or space, past reach) while throwing.
/// Vanilla's `hitResult == null` path skips the block/entity switch and still
/// reaches the unconditional generic-use fallback that actually throws the
/// snowball and swings.
#[test]
fn throwing_a_snowball_with_no_target_swings_the_arm() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:snowball");
    assert!(sim.target().is_none(), "precondition: no block targeted");
    assert!(sim.entity_target().is_none(), "precondition: no entity targeted");

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::SwingArm { hand: Hand::Main })),
        "throwing a snowball must queue a main-hand SwingArm for the wire, got {sent:?}"
    );
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "throwing a snowball must swing the local arm, progress peaked at {peak} \
         — this is the bug report: the wire action can be present while the local \
         arm still reads as rested"
    );
}

/// The other common case: aiming at ordinary terrain (dirt, stone — nothing
/// interactable) while throwing. A snowball is not a block, so it can never
/// take the placement path, and vanilla's `case BLOCK` falls through to the
/// same generic use as the no-target case whenever the block declines the
/// click (vanilla's own client entry point's use-item dispatch returns early
/// only on an outright fail result, but a plain block's `useItemOn` is `PASS`, not a fail).
#[test]
fn throwing_a_snowball_at_a_plain_block_still_swings_the_arm() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:snowball");
    sim.set_ray_target_for_test(Some(RayHit::face_center([4, 64, 4], [0, 1, 0])));
    assert!(sim.target().is_some(), "precondition: a block is targeted");

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::SwingArm { hand: Hand::Main })),
        "throwing a snowball at a plain block must still queue a SwingArm, got {sent:?}"
    );
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "throwing a snowball at a plain block must swing the local arm, \
         progress peaked at {peak}"
    );
}

/// The same gate for the other three throwables the report names, collected
/// rather than asserted one-at-a-time inside a loop — CLAUDE.md's own
/// warning about an `assert!` inside a `for` loop stopping at the first
/// failure and hiding the rest.
#[test]
fn every_named_throwable_swings_the_arm_with_no_target() {
    let items = [
        "minecraft:egg",
        "minecraft:ender_pearl",
        "minecraft:splash_potion",
    ];
    let mut failures = Vec::new();
    for item in items {
        let (net, _actions, _feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        sim.attach_net(net);
        give_main_hand_item(&mut sim, item);
        sim.use_item_live();
        let peak = peak_swing_over(&mut sim, 10);
        if peak <= 0.4 {
            failures.push(format!("{item}: peak={peak}"));
        }
    }
    assert!(
        failures.is_empty(),
        "these throwables did not swing the local arm: {failures:?}"
    );
}

/// Negative control for the three tests above: an **empty** main hand must
/// swing nothing, matching vanilla's own `!heldItem.isEmpty()` guard —
/// without this, "swing unconditionally" would satisfy the throw tests
/// vacuously.
#[test]
fn use_item_live_with_an_empty_hand_does_not_swing() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    assert!(sim.target().is_none());
    assert!(sim.entity_target().is_none());

    sim.use_item_live();

    let peak = peak_swing_over(&mut sim, 10);
    assert_eq!(
        peak, 0.0,
        "an empty main hand has nothing to throw and must not swing, got {peak}"
    );
}

// -----------------------------------------------------------------------
// ... and a use vanilla is silent for must not swing
// -----------------------------------------------------------------------
//
// The owner's report: *"right clicking with (i think) any item makes me swing
// my arm, which is wrong."* It did — `use_item_generic` swung for every
// non-empty main hand, and `use_item_live`'s block path swung for every
// click that reached it.
//
// Vanilla's rule is one condition, applied identically at all three of
// `Minecraft.startUseItem`'s call sites: swing only when the result is an
// `InteractionResult.Success` whose `swingSource()` is `CLIENT`.
// `InteractionResult.CONSUME` is `SwingSource.NONE`, and it is what a drawn
// bow, a raised shield, a spyglass and a bite of food all return; `PASS` is
// what an idle sword or pickaxe returns. All five were swinging here.

/// The negative half of the throwable table above, and the report itself.
///
/// Each item is paired with the vanilla method and result that decides it, so
/// a failure names *why* the expectation is what it is rather than only that
/// it was missed. Mismatches are collected rather than asserted inside the
/// loop — an `assert!` in the body would stop at the first item and leave the
/// rest as arguments instead of observations.
///
/// The `use_item_sent` column is the control: without it "no swing" is
/// satisfied by a `Sim` that never reached `use_item_generic` at all, which
/// would make the whole gate vacuous. Vanilla sends the `USE_ITEM` packet for
/// every non-empty hand regardless of what the result turns out to be — the
/// swing is the only thing gated.
#[test]
fn a_use_that_vanilla_does_not_swing_for_leaves_the_arm_still() {
    let silent = [
        ("minecraft:bow", "BowItem.use -> CONSUME (SwingSource::NONE)"),
        ("minecraft:crossbow", "CrossbowItem.use -> CONSUME"),
        ("minecraft:trident", "TridentItem.use -> CONSUME"),
        (
            "minecraft:spyglass",
            "SpyglassItem.use -> ItemUtils.startUsingInstantly -> CONSUME",
        ),
        (
            "minecraft:shield",
            "Item.use's minecraft:blocks_attacks arm -> CONSUME",
        ),
        (
            "minecraft:bread",
            "Item.use -> Consumable.startConsuming -> CONSUME",
        ),
        ("minecraft:diamond_sword", "Item.use -> PASS"),
        ("minecraft:diamond_pickaxe", "Item.use -> PASS"),
    ];
    let mut failures = Vec::new();
    for (item, vanilla) in silent {
        let (net, actions, _feed) = NetClient::loopback_with_feed();
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        sim.attach_net(net);
        give_main_hand_item(&mut sim, item);

        sim.use_item_live();

        let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
        let wire_swing = sent
            .iter()
            .any(|a| matches!(a, ClientAction::SwingArm { .. }));
        let use_item_sent = sent
            .iter()
            .any(|a| matches!(a, ClientAction::UseItem { hand: Hand::Main, .. }));
        let peak = peak_swing_over(&mut sim, 10);
        if wire_swing || peak > 0.0 || !use_item_sent {
            failures.push(format!(
                "{item} ({vanilla}): wire_swing={wire_swing} arm_peak={peak} \
                 use_item_sent={use_item_sent}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "these uses must reach the wire without swinging the arm — a swing here \
         is both a wrong local animation and a SwingArm every other player sees: \
         {failures:?}"
    );
}

/// The same rule on the **block** path, which has its own swing site.
///
/// `MultiPlayerGameMode.performUseItemOn` returns the base `Item.useOn`'s
/// `PASS` for a sword against plain stone, so `Minecraft.startUseItem`'s
/// `case BLOCK` swings nothing and falls through to the generic use — which,
/// for a sword, is also `PASS`. Before the fix this swung twice over: once
/// unconditionally after the `USE_ITEM_ON` send and once in the fall-through.
///
/// The `UseItemOn` assertion is the control — the click must genuinely have
/// taken the block branch, or the absent swing proves only that nothing ran.
#[test]
fn right_clicking_a_plain_block_with_a_sword_does_not_swing() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:diamond_sword");
    sim.set_ray_target_for_test(Some(RayHit::face_center([4, 64, 4], [0, 1, 0])));
    assert!(sim.target().is_some(), "precondition: a block is targeted");

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::UseItemOn { .. })),
        "control: the block branch must have been taken, got {sent:?}"
    );
    assert!(
        !sent
            .iter()
            .any(|a| matches!(a, ClientAction::SwingArm { .. })),
        "a sword against plain stone is vanilla's PASS at both the block and \
         the generic step, so no SwingArm may reach the wire — got {sent:?}"
    );
    let peak = peak_swing_over(&mut sim, 10);
    assert_eq!(
        peak, 0.0,
        "...and the local arm must stay at rest too, got {peak}"
    );
}

/// The discriminating positive for the block path's `Nothing` arm, and the
/// reason it is not simply "never swing on `Nothing`".
///
/// `UseOnDecision::Nothing` collapses two vanilla outcomes: the base
/// `Item.useOn`'s `PASS` (the sword above) and the overrides that act on the
/// block. `FlintAndSteelItem.useOn` returns `InteractionResult.SUCCESS`, so
/// vanilla swings **and returns** — it never reaches `gameMode.useItem`. Both
/// halves are asserted here, because a gate that only checked the swing would
/// pass against a version that also sent a spurious generic use.
#[test]
fn flint_and_steel_on_a_block_swings_and_stops_there() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:flint_and_steel");
    sim.set_ray_target_for_test(Some(RayHit::face_center([4, 64, 4], [0, 1, 0])));

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::UseItemOn { .. })),
        "control: the block branch must have been taken, got {sent:?}"
    );
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::SwingArm { hand: Hand::Main })),
        "flint and steel lights the block — vanilla's SUCCESS — so it must \
         swing, got {sent:?}"
    );
    assert!(
        !sent
            .iter()
            .any(|a| matches!(a, ClientAction::UseItem { .. })),
        "vanilla's `case BLOCK` returns on SUCCESS and never reaches \
         `gameMode.useItem`, so no generic use may follow — got {sent:?}"
    );
    let peak = peak_swing_over(&mut sim, 10);
    assert!(
        peak > 0.4,
        "the local arm must swing too, progress peaked at {peak}"
    );
}

/// One swing per click, on the entity path.
///
/// `Sim::interact_entity` swings unconditionally (see its doc: this client
/// models no `Entity.interact`, so there is no local result to gate on), and
/// the fall-through to `use_item_generic` used to swing again — two
/// `SwingArm` packets for one right-click, where vanilla's `case ENTITY`
/// swings at most once because it *returns* when it swings.
///
/// A snowball is the subject deliberately: it is the one item whose generic
/// use really is `SUCCESS`, so it would swing a second time on its own merits
/// if `already_swung` were not threaded through. With any silent item the
/// gate would pass for the wrong reason.
#[test]
fn an_entity_right_click_swings_exactly_once() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:snowball");
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));

    sim.use_item_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    let swings = sent
        .iter()
        .filter(|a| matches!(a, ClientAction::SwingArm { .. }))
        .count();
    assert_eq!(
        swings, 1,
        "one right-click on an entity must put exactly one SwingArm on the \
         wire, got {swings} in {sent:?}"
    );
    assert!(
        sent.iter()
            .any(|a| matches!(a, ClientAction::UseItem { hand: Hand::Main, .. })),
        "control: the generic use must still follow the interact, or the count \
         above is one only because the fall-through never happened — got {sent:?}"
    );
}

/// Finding 1: [`Sim::end_use_live`] must send `ReleaseUseItem` when a use
/// was actually in progress — the packet that was a serverbound island
/// (encoded by all four protocol adapters, zero producers anywhere in
/// this shell). Bow, crossbow and shield are all `useOnRelease() ==
/// true` and cannot complete a
/// use without it.
#[test]
fn end_use_live_sends_release_use_item_after_a_use_press() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:bow");
    assert!(sim.target().is_none());
    assert!(sim.entity_target().is_none());

    // The press: arms `UsingItem` (and, incidentally, sends the draw).
    sim.use_item_live();
    let _ = std::iter::from_fn(|| actions.try_recv().ok()).count();

    sim.end_use_live();
    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert_eq!(
        sent,
        vec![ClientAction::ReleaseUseItem],
        "releasing after a press must send exactly one ReleaseUseItem, got {sent:?}"
    );
}

/// Negative control: releasing with **no** prior press must send
/// nothing — proving `end_use_live` is actually gated on [`UsingItem`]
/// and not just "always send on release," which would pass the test
/// above vacuously.
#[test]
fn end_use_live_sends_nothing_with_no_prior_press() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);

    sim.end_use_live();

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(
        sent.is_empty(),
        "a release with no press before it must send nothing, got {sent:?}"
    );

    // And a second release right after the first (both with no press) is
    // still silent — the flag does not get "stuck on".
    sim.end_use_live();
    let sent_again: Vec<ClientAction> =
        std::iter::from_fn(|| actions.try_recv().ok()).collect();
    assert!(sent_again.is_empty(), "still nothing on a repeated release");
}

/// `ItemUseEffects`'s real writer: before this, the component
/// existed, was read by `compute_movement_intent`, and had zero production
/// writers — every query resolved `None` and the use-item slowdown/sprint
/// veto were both permanently inert. `use_item_live` must now resolve the
/// held item and write a **non-default** value through, not just leave the
/// component at whatever `Default` already gave it — a gate that only
/// checked "is it `Some`" would still pass a producer that always writes
/// `UseEffects::DEFAULT` regardless of what is actually held.
#[test]
fn use_item_live_writes_the_held_items_use_effects_not_a_constant() {
    let (net, _actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);

    let read_effects = |sim: &mut Sim| -> Option<UseEffects> {
        let local = sim.local;
        sim.write(|w| w.get::<ItemUseEffects>(local).and_then(|e| e.0))
    };

    // Precondition: nothing in progress yet.
    assert_eq!(read_effects(&mut sim), None, "no use in progress yet");

    // A spear must resolve to the SPEAR override (sprint allowed, no slow).
    give_main_hand_item(&mut sim, "minecraft:wooden_spear");
    sim.use_item_live();
    assert_eq!(
        read_effects(&mut sim),
        Some(UseEffects::SPEAR),
        "charging a spear must write UseEffects::SPEAR, not a constant DEFAULT"
    );
    sim.end_use_live();
    assert_eq!(
        read_effects(&mut sim),
        None,
        "releasing must clear the component back to None"
    );

    // An ordinary item (a bow) must resolve to DEFAULT, distinguishing this
    // from a producer that always writes SPEAR.
    give_main_hand_item(&mut sim, "minecraft:bow");
    sim.use_item_live();
    assert_eq!(
        read_effects(&mut sim),
        Some(UseEffects::DEFAULT),
        "drawing a bow must write UseEffects::DEFAULT"
    );
    sim.end_use_live();
    assert_eq!(read_effects(&mut sim), None);
}

/// The owner's report, end to end: *"when i right click in the air it slows
/// me down as if im eating... it should only slow me down if im actually
/// using an item like food, bow, etc."* Before [`item_has_use_animation`]'s
/// gate, `use_item_live` armed [`ItemUseEffects`] for *any* held item —
/// including an empty hand aimed at open air — so every right-click cut
/// ground speed by `UseEffects::DEFAULT`'s fifth-input scale regardless of
/// what, if anything, was in hand. Measures real displacement over a real
/// tick run (via [`Sim::step`]) against a no-click control walking the same
/// ticks with the same input, rather than merely asserting the resource
/// stayed unset.
#[test]
fn right_clicking_with_an_empty_hand_does_not_slow_movement() {
    let walk = |click: bool| -> f64 {
        let mut sim = Sim::new(test_config());
        // Player spawns at (0.5, feet, 0.5) facing north (-Z, yaw 180). Lay a
        // solid floor and clear head-room along -Z, the same unobstructed
        // straight-line setup `sprint_vs_walk`-shaped gates in this file use,
        // so the measured distance reflects the speed multiplier and not
        // terrain the default demo world happens to put in the way.
        let feet_y = sim.player().position.y.floor() as i32;
        for dz in -25..=1 {
            for dx in -1..=1 {
                sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
                sim.set_block_world([dx, feet_y, dz], id::AIR);
                sim.set_block_world([dx, feet_y + 1, dz], id::AIR);
                sim.set_block_world([dx, feet_y + 2, dz], id::AIR);
            }
        }
        // Settle onto the fresh floor first so the measured window is pure
        // ground-friction walking, not still-falling noise.
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        let start = sim.player().position;
        if click {
            assert!(sim.target().is_none(), "precondition: no block targeted");
            assert!(sim.entity_target().is_none(), "precondition: no entity targeted");
            sim.use_item_live();
        }
        sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        sim.player().position.subtract(start).length()
    };

    let plain = walk(false);
    let clicked = walk(true);
    assert!(
        (clicked - plain).abs() < 1e-6,
        "an empty-hand right-click in open air must not change ground speed \
         at all: plain={plain} clicked={clicked}"
    );
}

/// The positive control for the gate above: a **genuine** use item (food)
/// held through the same click *does* cut ground speed — proving the
/// harness can actually tell the two cases apart. Without this, the test
/// above would pass just as well against a version that never applies any
/// use-item slowdown at all, which is exactly the vacuous shape
/// `CLAUDE.md` warns a negative-only assertion can take.
#[test]
fn right_clicking_with_a_food_item_does_slow_movement() {
    let walk = |eating: bool| -> f64 {
        let mut sim = Sim::new(test_config());
        // Same unobstructed straight-line floor as the negative control above.
        let feet_y = sim.player().position.y.floor() as i32;
        for dz in -25..=1 {
            for dx in -1..=1 {
                sim.set_block_world([dx, feet_y - 1, dz], id::STONE);
                sim.set_block_world([dx, feet_y, dz], id::AIR);
                sim.set_block_world([dx, feet_y + 1, dz], id::AIR);
                sim.set_block_world([dx, feet_y + 2, dz], id::AIR);
            }
        }
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        if eating {
            give_main_hand_item(&mut sim, "minecraft:bread");
        }
        let start = sim.player().position;
        assert!(sim.target().is_none(), "precondition: no block targeted");
        assert!(sim.entity_target().is_none(), "precondition: no entity targeted");
        sim.use_item_live();
        sim.input_mut(|i| i.set(lodestone_controller::Action::Forward, true));
        for _ in 0..20 {
            sim.step(1.0 / 20.0);
        }
        sim.player().position.subtract(start).length()
    };

    let plain = walk(false);
    let eating = walk(true);
    assert!(
        eating < plain * 0.5,
        "eating bread must cut ground speed sharply (UseEffects::DEFAULT scales \
         input to a fifth): plain={plain} eating={eating}"
    );
}

/// A food use refused by the authoritative hunger gate must not arm the local
/// use state. `ConsumeState::resolve` already rejects the animation/particles,
/// but the press edge has its own movement-effect writer and used to arm it for
/// every consumable before the server could refuse a full-bar bread use.
#[test]
fn right_clicking_food_at_a_full_hunger_bar_does_not_slow_movement() {
    let mut sim = Sim::new(test_config());
    let local = sim.local;
    sim.write(|world| {
        world
            .get_mut::<Vitals>(local)
            .expect("the local player has vitals")
            .food = Some(lodestone_game::food::MAX_FOOD);
    });
    give_main_hand_item(&mut sim, "minecraft:bread");

    sim.use_item_live();

    assert!(!sim.read(|world| world.resource::<UsingItem>().0));
    assert_eq!(
        sim.read(|world| world.get::<ItemUseEffects>(local).and_then(|e| e.0)),
        None,
        "a full-bar bread use is refused and must not arm movement slowdown"
    );
}

/// Holding `key.use` through a completed food use must begin the next use,
/// matching `Minecraft.handleKeybinds` polling the held key rather than only
/// reacting to the original OS press edge.  The loopback action stream is the
/// client/server seam: a second `UseItem` is what lets the authoritative server
/// start the second bite.
#[test]
fn holding_use_restarts_food_after_its_consume_duration() {
    let (net, actions, _feed) = NetClient::loopback_with_feed();
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    sim.attach_net(net);
    give_main_hand_item(&mut sim, "minecraft:bread");

    // `Mode::Headless` has no stitched vanilla atlas, so `Sim::use_item`
    // intentionally takes the demo-world placement path. Exercise the live
    // press transition directly, as the nearby outbound-use seam tests do.
    sim.use_item_live();
    for _ in 0..32 {
        sim.step(1.0 / 20.0);
    }

    let sent: Vec<ClientAction> = std::iter::from_fn(|| actions.try_recv().ok()).collect();
    let uses = sent
        .iter()
        .filter(|action| matches!(action, ClientAction::UseItem { hand: Hand::Main, .. }))
        .count();
    assert_eq!(
        uses, 2,
        "holding use through bread's 32-tick duration must send a fresh UseItem, got {sent:?}"
    );
}

/// Vanilla's `getCurrentItemAttackStrengthDelay`/`getAttackStrengthScale`
///: with no [`Attributes`] component at all (the
/// pre-login default `attribute_value` falls back to — see
/// `no_attributes_component_folds_to_the_registry_default` in
/// `lodestone_ecs::player`'s own tests for the identical fallback one
/// layer down), the unarmed `attack_speed` default of `4.0` gives a
/// 5-tick delay, so the scale ramps linearly from `0.0` to `1.0` over
/// exactly 5 real `GameTick`s (via [`Self::step`], not a hand-called
/// tick function — the same "reachable through the schedule" bar
/// `lodestone_ecs::player`'s island-class tests hold `PhysicsState`/
/// `AttackStrengthTicker` to) and clamps there rather than overshooting.
#[test]
