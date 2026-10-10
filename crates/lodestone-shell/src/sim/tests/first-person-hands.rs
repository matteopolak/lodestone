//! `Sim` → first-person witnesses: state held and advanced in `Sim` reaches the
//! per-hand pose the hand pass draws with, through the production conversion
//! (`first_person_hands::hands_frame`) and the renderer's own pose step
//! (`gpu::hand_poses`). Expected values are arithmetic from the reference
//! behaviour's constants (hand origin `(±0.56, -0.52, -0.72)`, lowering
//! `-0.6` blocks per unit, `0.4` height per tick), never read back from the
//! code under test.
use super::*;

const TICK: f64 = 1.0 / 20.0;

fn put(sim: &mut Sim, native_slot: i32, item: Option<(&str, u32)>) {
    let local = sim.local;
    sim.write(|w| {
        if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
            menus.0.apply(&lodestone_model::ClientEvent::InventorySlotChanged {
                slot: native_slot,
                item: item.map(|(id, count)| {
                    lodestone_model::ItemStack::new(id.parse().expect("valid item id"), count)
                }),
            });
        }
    });
}

/// Each drawn hand's pose and shown item id, exactly as the hand pass would
/// receive them this frame.
fn drawn_hands(sim: &Sim) -> Vec<(crate::gpu::HandPose, Option<String>)> {
    let frame = crate::sim::first_person_hands::hands_frame(&sim.first_person_hands_sample());
    crate::gpu::hand_poses(&frame, 0.0)
        .into_iter()
        .map(|(pose, item)| (pose, item.map(|held| held.item.to_string())))
        .collect()
}

/// The camera-space origin of a resting (unswung) held item in `pose` — the
/// first term of the item chain the hand pass meshes with.
fn origin(pose: crate::gpu::HandPose) -> glam::Vec3 {
    lodestone_render::entity::first_person_item_chain(pose.arm, 0.0, pose.inverse_arm_height)
        .transform_point3(glam::Vec3::ZERO)
}

fn assert_close(got: glam::Vec3, want: [f32; 3], what: &str) {
    let want = glam::Vec3::from(want);
    assert!(
        (got - want).abs().max_element() < 1e-5,
        "{what}: expected {want:?}, got {got:?}"
    );
}

fn settle(sim: &mut Sim) {
    for _ in 0..10 {
        sim.step(TICK);
    }
}

/// An off-hand stack in `Sim`'s inventory reaches a **left-arm** pose holding
/// that stack, mirrored from the main hand: origin `(-0.56, -0.52, -0.72)`
/// against the main hand's `(0.56, -0.52, -0.72)`. The off hand never swings.
#[test]
fn an_off_hand_stack_reaches_the_mirrored_left_hand_pose() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    put(&mut sim, 0, Some(("minecraft:iron_sword", 1)));
    put(&mut sim, 40, Some(("minecraft:totem_of_undying", 1)));
    settle(&mut sim);

    let hands = drawn_hands(&sim);
    assert_eq!(hands.len(), 2, "both hands draw: {hands:?}");
    let (main, main_item) = &hands[0];
    let (off, off_item) = &hands[1];
    assert!(main.is_main && !off.is_main);
    assert_eq!(main.arm, lodestone_render::entity::Arm::Right);
    assert_eq!(off.arm, lodestone_render::entity::Arm::Left);
    assert_eq!(main_item.as_deref(), Some("minecraft:iron_sword"));
    assert_eq!(off_item.as_deref(), Some("minecraft:totem_of_undying"));
    assert_eq!(off.attack_anim, 0.0, "the off hand never swings");
    assert_close(origin(*main), [0.56, -0.52, -0.72], "main hand at rest");
    assert_close(origin(*off), [-0.56, -0.52, -0.72], "off hand at rest");

    // Control: with the off-hand slot emptied (and the swap run through), the
    // off pose carries no item — which the hand pass draws as nothing.
    put(&mut sim, 40, None);
    settle(&mut sim);
    let hands = drawn_hands(&sim);
    assert_eq!(hands[1].1, None, "an emptied off hand shows nothing: {hands:?}");
}

/// Changing only the off-hand stack lowers only the off hand: one tick in, its
/// height is `0.6` (one `0.4` step down from `1.0`), so at the frame's partial
/// tick `a` it is drawn `0.4·a` below rest — `0.6·0.4·a` blocks lower — while
/// the main hand stays at rest. The old stack is still shown until the third
/// tick, where the height first falls below `0.1`.
#[test]
fn an_off_hand_swap_lowers_only_the_off_hand() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    put(&mut sim, 0, Some(("minecraft:iron_sword", 1)));
    put(&mut sim, 40, Some(("minecraft:totem_of_undying", 1)));
    settle(&mut sim);

    put(&mut sim, 40, Some(("minecraft:shield", 1)));
    // One whole tick plus half of the next, so the sample sits mid-tick where
    // a missing or reversed interpolation lands on a different number.
    sim.step(TICK * 1.5);
    let alpha = sim.clock().interp_alpha;
    assert!(alpha > 0.25 && alpha < 0.75, "precondition: mid-tick sample, got {alpha}");
    let hands = drawn_hands(&sim);
    let (main, _) = &hands[0];
    let (off, off_item) = &hands[1];
    assert_eq!(main.inverse_arm_height, 0.0, "the main hand must not move");
    let want = 0.4 * alpha;
    assert!(
        (off.inverse_arm_height - want).abs() < 1e-5,
        "off hand one tick into a swap at partial tick {alpha}: expected {want}, got {}",
        off.inverse_arm_height
    );
    assert_close(origin(*off), [-0.56, -0.52 - 0.6 * want, -0.72], "off hand lowering");
    assert_eq!(off_item.as_deref(), Some("minecraft:totem_of_undying"), "old stack still shown");

    sim.step(TICK);
    assert_eq!(drawn_hands(&sim)[1].1.as_deref(), Some("minecraft:totem_of_undying"));
    sim.step(TICK);
    assert_eq!(
        drawn_hands(&sim)[1].1.as_deref(),
        Some("minecraft:shield"),
        "the exchange lands on the third tick"
    );
}

fn set_attack_speed(sim: &mut Sim, speed: f64) {
    use std::str::FromStr;
    let local = sim.local_player();
    let key = lodestone_model::Identifier::from_str("minecraft:attack_speed").unwrap();
    sim.write(|w| {
        w.entity_mut(local).insert(Attributes(vec![lodestone_model::EntityAttributeSnapshot {
            attribute: key,
            base: speed,
            modifiers: Vec::new(),
        }]));
    });
}

/// An entity attack through the production click path lowers the held item
/// along the cooldown curve, and the lowering reaches the pose the hand pass
/// draws: `k` ticks after the attack, with a `1.6` attack speed (delay
/// `12.5` ticks), the height is the clamped step toward `((k + 1) / 12.5)³` —
/// `0.6, 0.2, 0.032768, 0.064` — drawn at the frame's partial tick and moved
/// `-0.6` blocks per unit below the hand's resting `-0.52`. The piercing
/// test below is the control: the same click that restarts only the attack
/// counter leaves the hand up.
#[test]
fn an_attack_lowers_the_held_item_along_the_cooldown_curve() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    set_attack_speed(&mut sim, 1.6);
    put(&mut sim, 0, Some(("minecraft:iron_sword", 1)));
    for _ in 0..20 {
        sim.step(TICK);
    }
    assert_eq!(drawn_hands(&sim)[0].0.inverse_arm_height, 0.0, "precondition: recovered");

    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));
    sim.begin_attack_live();
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = None);

    let heights = [1.0f32, 0.6, 0.2, 0.032768, 0.064];
    for k in 1..heights.len() {
        sim.step(TICK);
        let alpha = sim.clock().interp_alpha;
        let drawn = heights[k - 1] + (heights[k] - heights[k - 1]) * alpha;
        let want = 1.0 - drawn;
        let (main, item) = drawn_hands(&sim).remove(0);
        assert_eq!(item.as_deref(), Some("minecraft:iron_sword"));
        assert!(
            (main.inverse_arm_height - want).abs() < 1e-4,
            "tick {k} after the attack (partial {alpha}): expected lowering {want}, got {}",
            main.inverse_arm_height
        );
        assert_close(origin(main), [0.56, -0.52 - 0.6 * want, -0.72], "main hand lowered");
    }
}

/// The two counters reset on different events. A swing at nothing restarts
/// both (the hand dips); a piercing weapon's attack restarts only the attack
/// counter — the crosshair cooldown empties while the held spear stays up.
#[test]
fn a_piercing_attack_resets_the_cooldown_but_not_the_hand() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    put(&mut sim, 0, Some(("minecraft:iron_spear", 1)));
    for _ in 0..20 {
        sim.step(TICK);
    }
    let swap_ticks = |sim: &Sim| {
        sim.read(|w| w.get::<lodestone_ecs::ItemSwapTicker>(sim.local).unwrap().0)
    };
    let before = swap_ticks(&sim);
    assert!(before > 10);
    assert_eq!(sim.attack_strength_scale(), 1.0);

    sim.begin_attack_live();
    assert_eq!(sim.attack_strength_scale(), 0.0, "the stab empties the cooldown");
    assert_eq!(swap_ticks(&sim), before, "the stab leaves the hand counter alone");
    sim.step(TICK);
    assert_eq!(drawn_hands(&sim)[0].0.inverse_arm_height, 0.0, "the spear does not dip");

    // Control: the same click with a sword in hand (no piercing component)
    // and nothing targeted is a swing at nothing, which does restart both.
    put(&mut sim, 0, Some(("minecraft:iron_sword", 1)));
    for _ in 0..20 {
        sim.step(TICK);
    }
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = None);
    sim.set_target(None);
    sim.begin_attack_live();
    assert_eq!(swap_ticks(&sim), 0, "a miss restarts the hand counter");
}

/// Switching the main hand to a different item restarts both counters, so
/// the new item comes up along the cooldown curve rather than at the plain
/// swap rate; a count change of the same item does not.
#[test]
fn switching_weapons_restarts_the_cooldown() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    put(&mut sim, 0, Some(("minecraft:iron_sword", 1)));
    for _ in 0..20 {
        sim.step(TICK);
    }
    assert_eq!(sim.attack_strength_scale(), 1.0);
    put(&mut sim, 0, Some(("minecraft:iron_axe", 1)));
    sim.step(TICK);
    // The tick advanced the counter and then saw the new item, which put it
    // back to zero: the indicator is empty after the switch.
    assert_eq!(sim.attack_strength_scale(), 0.0);

    for _ in 0..20 {
        sim.step(TICK);
    }
    assert_eq!(sim.attack_strength_scale(), 1.0);
    // Control: more of the same item is not a switch.
    put(&mut sim, 0, Some(("minecraft:iron_axe", 2)));
    sim.step(TICK);
    assert_eq!(sim.attack_strength_scale(), 1.0, "a count change keeps the cooldown");
}

/// Starting to eat through the production use path snaps the main hand down
/// and raises it again — `1 - 0.4·a` lowering one tick later at partial tick
/// `a` — without swinging the arm; right-clicking a sword (no use of its own)
/// leaves the hand where it is.
#[test]
fn starting_to_eat_snaps_the_hand_down_and_raises_it_without_a_swing() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    put(&mut sim, 0, Some(("minecraft:iron_sword", 1)));
    settle(&mut sim);
    sim.set_target(None);
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = None);

    // Control: a sword's use passes, so its hand does not move.
    sim.use_item_live();
    assert_eq!(drawn_hands(&sim)[0].0.inverse_arm_height, 0.0, "a sword use passes");

    put(&mut sim, 0, Some(("minecraft:bread", 5)));
    settle(&mut sim);
    assert_eq!(drawn_hands(&sim)[0].0.inverse_arm_height, 0.0, "precondition: raised");
    sim.use_item_live();
    assert!(sim.read(|w| w.resource::<UsingItem>().0), "precondition: the use started");
    assert_eq!(drawn_hands(&sim)[0].0.inverse_arm_height, 1.0, "the use snaps the hand down");
    assert_eq!(sim.hand_swing_progress(), 0.0, "eating does not swing");

    sim.step(TICK);
    let alpha = sim.clock().interp_alpha;
    let want = 1.0 - 0.4 * alpha;
    let got = drawn_hands(&sim)[0].0.inverse_arm_height;
    assert!((got - want).abs() < 1e-5, "one tick on at partial {alpha}: want {want}, got {got}");
    assert_eq!(sim.hand_swing_progress(), 0.0, "and the rise does not swing either");
}

/// Paddling — controlling a boat with a movement key held — is read from the
/// production vehicle and intent state, lowers both hands, and refuses attack
/// and use clicks outright (no swing, no cooldown reset). Letting go of the
/// key frees the hands again.
#[test]
fn paddling_a_boat_lowers_both_hands_and_refuses_clicks() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    put(&mut sim, 0, Some(("minecraft:iron_sword", 1)));
    put(&mut sim, 40, Some(("minecraft:shield", 1)));
    for _ in 0..20 {
        sim.step(TICK);
    }
    let local = sim.local;
    let set = |sim: &mut Sim, boat: bool, forward: f32| {
        sim.write(|w| {
            w.resource_mut::<lodestone_ecs::vehicle::ControlledVehicle>().0 = Some(
                lodestone_ecs::vehicle::ControlledVehicleState {
                    server_id: 7,
                    family: if boat {
                        lodestone_ecs::vehicle::VehicleFamily::Boat
                    } else {
                        lodestone_ecs::vehicle::VehicleFamily::LandMount
                    },
                    motion: lodestone_physics::EntityMotion::at(lodestone_physics::Vec3d::new(
                        0.0, 64.0, 0.0,
                    )),
                    yaw: 0.0,
                    pitch: 0.0,
                    previous: lodestone_ecs::vehicle::VehicleRenderPose {
                        position: lodestone_physics::Vec3d::new(0.0, 64.0, 0.0),
                        yaw: 0.0,
                        pitch: 0.0,
                    },
                    boat: lodestone_physics::vehicle::BoatState::default(),
                    paddles: (false, false),
                },
            );
            w.get_mut::<lodestone_ecs::MovementIntent>(local).unwrap().0.forward = forward;
        });
    };

    // Control: a boat with no key held, and a non-boat mount with one, are
    // not busy.
    set(&mut sim, true, 0.0);
    sim.tick_first_person_hands();
    assert!(!sim.hands_busy(), "an idle boat leaves the hands free");
    set(&mut sim, false, 1.0);
    sim.tick_first_person_hands();
    assert!(!sim.hands_busy(), "only a boat takes the hands");

    set(&mut sim, true, 1.0);
    sim.tick_first_person_hands();
    assert!(sim.hands_busy());
    let hands = drawn_hands(&sim);
    let lowered = |hand: &crate::gpu::HandPose| hand.inverse_arm_height;
    // One busy tick: both heights `1.0 → 0.6`, drawn between at the partial tick.
    let alpha = sim.clock().interp_alpha;
    assert!((lowered(&hands[0].0) - 0.4 * alpha).abs() < 1e-5);
    assert!((lowered(&hands[1].0) - 0.4 * alpha).abs() < 1e-5);

    let before = sim.attack_strength_scale();
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = Some(42));
    sim.begin_attack_live();
    sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = None);
    assert_eq!(sim.attack_strength_scale(), before, "a paddling attack click does nothing");
    assert_eq!(sim.hand_swing_progress(), 0.0, "and does not swing");
    put(&mut sim, 0, Some(("minecraft:bread", 5)));
    sim.use_item_live();
    assert!(!sim.read(|w| w.resource::<UsingItem>().0), "a paddling use click does nothing");

    set(&mut sim, true, 0.0);
    sim.tick_first_person_hands();
    assert!(!sim.hands_busy(), "letting go frees the hands");
}

/// Negative controls for the use poses: holding a use (a drawn bow, a raised
/// shield, a loading crossbow) is one snap at the start and then a hand at
/// rest for as long as the use lasts — the cooldown does not move, nothing
/// re-lowers it — and releasing does not dip either hand. Drawing a bow or
/// loading a crossbow hides the off hand while the use lasts; raising a
/// shield leaves it drawn.
#[test]
fn a_held_use_keeps_its_hand_at_rest_and_only_a_bow_hides_the_off_hand() {
    for (item, hides_off) in [
        ("minecraft:bow", true),
        ("minecraft:crossbow", true),
        ("minecraft:shield", false),
    ] {
        let mut sim = Sim::new(test_config());
        sim.drain_all_meshes();
        put(&mut sim, 0, Some((item, 1)));
        put(&mut sim, 40, Some(("minecraft:totem_of_undying", 1)));
        for _ in 0..20 {
            sim.step(TICK);
        }
        sim.set_target(None);
        sim.write(|w| w.resource_mut::<EntityRayTarget>().0 = None);
        let cooldown = sim.attack_strength_scale();
        sim.use_item_live();
        assert!(sim.read(|w| w.resource::<UsingItem>().0), "{item}: precondition, use started");
        for _ in 0..3 {
            sim.step(TICK);
        }
        for tick in 0..20 {
            sim.step(TICK);
            let hands = drawn_hands(&sim);
            assert_eq!(hands[0].0.inverse_arm_height, 0.0, "{item}: tick {tick} of the use");
            assert_eq!(hands[0].1.as_deref(), Some(item));
            assert_eq!(hands[0].0.attack_anim, 0.0, "{item}: a held use never swings");
            assert_eq!(
                hands.len(),
                if hides_off { 1 } else { 2 },
                "{item}: which hands draw while in use: {hands:?}"
            );
        }
        assert_eq!(sim.attack_strength_scale(), cooldown, "{item}: the use leaves the cooldown");
        assert_eq!(sim.hand_swing_progress(), 0.0, "{item}: and never swings");

        sim.end_use_live();
        sim.step(TICK);
        let hands = drawn_hands(&sim);
        assert_eq!(hands.len(), 2, "{item}: both hands draw after release");
        assert_eq!(hands[0].0.inverse_arm_height, 0.0, "{item}: release does not dip the main");
        assert_eq!(hands[1].0.inverse_arm_height, 0.0, "{item}: nor the off hand");
    }
}

/// Negative controls for the plain poses: an empty main hand is a resting
/// bare-arm pose (a drawn main hand with no item) beside a drawn off hand
/// holding nothing, which the hand pass draws as nothing; a filled map in the
/// main hand with an empty off hand reaches the renderer as the main hand's
/// own item at rest, where the two-handed map pose is chosen.
#[test]
fn an_empty_hand_and_a_held_map_reach_the_renderer_at_rest() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    settle(&mut sim);
    let hands = drawn_hands(&sim);
    assert_eq!(hands.len(), 2);
    assert!(hands[0].0.is_main && hands[0].1.is_none(), "bare arm: {hands:?}");
    assert_eq!(hands[0].0.inverse_arm_height, 0.0);
    assert!(hands[1].1.is_none(), "empty off hand: {hands:?}");

    put(&mut sim, 0, Some(("minecraft:filled_map", 1)));
    settle(&mut sim);
    let hands = drawn_hands(&sim);
    assert_eq!(hands[0].1.as_deref(), Some("minecraft:filled_map"));
    assert_eq!(hands[0].0.inverse_arm_height, 0.0);
    assert!(hands[1].1.is_none());
}

/// Each hand's draw record carries its own stack's `minecraft:map_id`, which is
/// what lets the renderer show two different maps at once.
#[test]
fn each_held_map_reaches_the_renderer_with_its_own_map_id() {
    let mut sim = Sim::new(test_config());
    sim.drain_all_meshes();
    settle(&mut sim);
    let local = sim.local;
    for (slot, map_id) in [(0, 17), (40, 23)] {
        sim.write(|w| {
            if let Some(mut menus) = w.get_mut::<lodestone_ecs::SessionMenus>(local) {
                let mut stack = lodestone_model::ItemStack::new("minecraft:filled_map".parse().unwrap(), 1);
                stack.components.map_id = Some(map_id);
                menus.0.apply(&lodestone_model::ClientEvent::InventorySlotChanged { slot, item: Some(stack) });
            }
        });
    }
    settle(&mut sim);
    let frame = crate::sim::first_person_hands::hands_frame(&sim.first_person_hands_sample());
    assert_eq!(frame.main.item.as_ref().and_then(|held| held.map_id), Some(17));
    assert_eq!(frame.off.item.as_ref().and_then(|held| held.map_id), Some(23));
}
