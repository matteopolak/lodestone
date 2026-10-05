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
