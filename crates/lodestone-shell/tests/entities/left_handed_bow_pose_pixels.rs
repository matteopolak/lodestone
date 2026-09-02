//! Pixel gate: a **left-handed** remote skeleton must draw its bow with its
//! *left* arm, not its right — `Mob.isLeftHanded()`
//! ([`lodestone_ecs::entity::MobState::left_handed`]) had zero readers before
//! this fix, so an aggressive left-handed skeleton drew the ordinary
//! right-handed pose. See `crates/lodestone-ecs/src/entity.rs`'s
//! `MobState::left_handed` doc and `arm_pose_for` in
//! `crates/lodestone-shell/src/entities.rs`.
//!
//! # Why this cannot be a unit test
//!
//! `entities::tests::left_handedness_xors_with_the_hand_the_item_is_in`
//! already proves `arm_pose_for`'s XOR in isolation, calling the pure
//! function directly. It is structurally blind to the same class of bug
//! `aggressive_bow_pose_pixels.rs`'s own module doc names: a pure-function
//! gate cannot see whether `MobState::left_handed` ever reaches
//! `arm_pose_for` in production, or whether `AnimInput::arm_pose_left_hand`
//! ever reaches the *bone rotation* `Skeleton::pose_arms_for_item` applies —
//! only a frame rendered through the real `ClientEvent` -> ingest ->
//! `extract_entity_draws` -> `RenderState::render` chain can.
//!
//! # What is measured, and why this camera angle
//!
//! `aggressive_bow_pose_pixels.rs` uses a **broadside** camera (`BODY_YAW =
//! 90`) because the bow pose's dominant motion — both arms swinging from
//! hanging-down to forward-horizontal (`x_rot`) — projects almost entirely
//! into silhouette *width* from that angle. Handedness does not change that
//! shared `x_rot`: `Skeleton::pose_arms_for_item`'s `BOW_AND_ARROW` arm gives
//! both arms the identical `x_rot` regardless of `holding_in_right`. What
//! handedness *does* change is each arm's `y_rot` splay — the holding arm
//! stays close to centre (`-0.1` rad) and the off arm splays out
//! (`+0.5` rad), or the mirror image when left-handed — a rotation about the
//! *vertical* axis, which projects into silhouette width maximally from a
//! **face-on** camera (`BODY_YAW = 0`), not a broadside one. So this gate
//! uses the opposite camera angle from its sibling, for the same reason that
//! one avoids face-on: each camera is blind to the motion the other is
//! sensitive to.
//!
//! The measurement is not "does it move" (that is
//! `aggressive_bow_pose_pixels.rs`'s job) but **which half of the frame moves
//! more** — the changed-pixel count split at the subject's own screen
//! midline. A right-handed skeleton's off (splaying) arm is its left arm; a
//! left-handed skeleton's off arm is its right arm — mirrored physical arms,
//! so the side that moves more must flip between the two subjects. `CLAUDE.md`:
//! "measure by location, never by frame average" — this is exactly that,
//! applied to left vs. right rather than top vs. bottom.
//!
//! ```text
//! cargo test -p lodestone-shell --test left_handed_bow_pose_pixels -- --ignored --nocapture
//! ```

use bevy_ecs::world::World;
use lodestone::entities::{EntityDraw, EntityInterpPlugin, extracted_entity_draws, fold_entities};
use lodestone::gpu::RenderState;
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, GameTick, NetIngest};
use lodestone_model::event::EquipmentSlot;
use lodestone_model::{
    ClientEvent, EntityEquipment, EntityMetadataUpdate, ItemStack, Rotation, Vec3 as ModelVec3,
};
use lodestone_render::{ArmPose, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// Face-on: the mob's own left/right axis lines up with the camera's
/// horizontal screen axis, which is what makes the `y_rot` splay asymmetry
/// (the term handedness flips) visible in silhouette at all. See the module
/// doc for why this is the opposite choice from
/// `aggressive_bow_pose_pixels.rs`'s broadside camera.
const BODY_YAW: f32 = 0.0;

/// `Mob.DATA_MOB_FLAGS_ID`'s aggressive bit.
const AGGRESSIVE_BIT: u8 = 0x04;
/// `Mob.DATA_MOB_FLAGS_ID`'s left-handed bit (`Mob.isLeftHanded()`).
const LEFT_HANDED_BIT: u8 = 0x02;

fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 1.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

/// Two tracked skeletons at the same spot, each holding a bow in the main
/// hand — identical except for id, exactly like
/// `aggressive_bow_pose_pixels.rs`'s own fixture.
fn world_with_bow_carrying_skeletons(feet: glam::Vec3) -> World {
    let mut app = App::new();
    app.add_plugins((IngestPlugin, EntityInterpPlugin));
    let mut world = std::mem::take(app.world_mut());

    for id in [1, 2] {
        world
            .resource_mut::<IngestQueue>()
            .push(ClientEvent::EntitySpawned {
                entity_id: id,
                uuid: None,
                entity_type: "minecraft:skeleton".parse().expect("valid entity type key"),
                pos: ModelVec3::new(f64::from(feet.x), f64::from(feet.y), f64::from(feet.z)),
                rotation: Rotation::new(BODY_YAW, 0.0),
                velocity: None,
            });
        world
            .resource_mut::<IngestQueue>()
            .push(ClientEvent::EntityEquipmentUpdated {
                entity_id: id,
                equipment: vec![EntityEquipment {
                    slot: EquipmentSlot::MainHand,
                    item: Some(ItemStack::new(
                        "minecraft:bow".parse().expect("valid item key"),
                        1,
                    )),
                }],
            });
        world.run_schedule(NetIngest);
    }

    fold_entities(&mut world);
    world
}

/// The mob-flags metadata event, as the adapter emits it for a `Mob` —
/// aggressive always, left-handed only when asked.
fn mob_flags_event(entity_id: i32, left_handed: bool) -> ClientEvent {
    let bits = AGGRESSIVE_BIT | if left_handed { LEFT_HANDED_BIT } else { 0 };
    ClientEvent::EntityMetadataUpdated {
        entity_id,
        metadata: EntityMetadataUpdate {
            mob_flags: Some(bits),
            ..EntityMetadataUpdate::default()
        },
    }
}

fn draw_for(world: &World, id: i32) -> EntityDraw {
    extracted_entity_draws(world)
        .into_iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("entity {id} not among the extracted draws"))
}

/// Changed-pixel counts, split at the screen's horizontal midline.
#[derive(Debug, Clone, Copy)]
struct HalfSplit {
    left: u32,
    right: u32,
}

impl std::fmt::Display for HalfSplit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "left={} right={}", self.left, self.right)
    }
}

fn changed_half_split(a: &[u8], b: &[u8]) -> HalfSplit {
    let mut left = 0u32;
    let mut right = 0u32;
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            let d = (i32::from(a[i]) - i32::from(b[i])).abs()
                + (i32::from(a[i + 1]) - i32::from(b[i + 1])).abs()
                + (i32::from(a[i + 2]) - i32::from(b[i + 2])).abs();
            if d > 20 {
                if x < W / 2 {
                    left += 1;
                } else {
                    right += 1;
                }
            }
        }
    }
    HalfSplit { left, right }
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to prove a left-handed skeleton draws its \
            bow on the opposite arm from a right-handed one"]
fn a_left_handed_aggressive_skeleton_draws_its_bow_on_the_opposite_arm() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; run on a \
         host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let state = RenderState::new(device, queue, format, W, H, None);
    let cam = camera();

    let feet = glam::Vec3::new(0.0, 0.0, 4.0);
    let mut world = world_with_bow_carrying_skeletons(feet);
    world.run_schedule(Extract);

    let right_handed_rest = draw_for(&world, 1);
    let left_handed_rest = draw_for(&world, 2);

    world
        .resource_mut::<IngestQueue>()
        .push(mob_flags_event(1, false));
    world
        .resource_mut::<IngestQueue>()
        .push(mob_flags_event(2, true));
    world.run_schedule(NetIngest);
    for _ in 0..2 {
        world.run_schedule(GameTick);
    }
    world.run_schedule(Extract);

    let right_handed = draw_for(&world, 1);
    let left_handed = draw_for(&world, 2);

    // --- State assertions, ahead of any pixel: the whole chain, not the pure
    //     function `entities::tests::left_handedness_xors_with_the_hand_the_item_is_in`
    //     already covers in isolation.
    assert_eq!(
        right_handed.anim.arm_pose,
        ArmPose::BowAndArrow,
        "the right-handed control must still draw the bow"
    );
    assert_eq!(
        left_handed.anim.arm_pose,
        ArmPose::BowAndArrow,
        "the left-handed subject must still draw the bow — handedness must not suppress the \
         pose, only mirror it"
    );
    assert!(
        !right_handed.anim.arm_pose_left_hand,
        "a right-handed skeleton must draw with its main (right) arm — \
         MobState::left_handed reached AnimInput as true for an entity that never reported it"
    );
    assert!(
        left_handed.anim.arm_pose_left_hand,
        "a left-handed skeleton (mob_flags bit 0x02) must draw with its left arm — this is the \
         island: MobState::left_handed had zero readers, so this stayed false unconditionally"
    );

    // --- Pixels ------------------------------------------------------------
    let mut shoot = |draw: &EntityDraw| -> Vec<u8> {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, std::slice::from_ref(draw));
        assert_eq!(
            stats.entities_drawn, 1,
            "the subject failed to reach the entity pipeline — this gate would measure the \
             absence of an entity rather than the absence of a mirrored pose"
        );
        target.read_texels(device, queue)
    };

    let right_handed_rest_px = shoot(&right_handed_rest);
    let right_handed_px = shoot(&right_handed);
    let left_handed_rest_px = shoot(&left_handed_rest);
    let left_handed_px = shoot(&left_handed);

    let right_handed_split = changed_half_split(&right_handed_rest_px, &right_handed_px);
    let left_handed_split = changed_half_split(&left_handed_rest_px, &left_handed_px);

    eprintln!("=== LEFT-HANDED BOW POSE PIXEL GATE (face-on, {W}x{H}) ===");
    eprintln!("right-handed pose change: {right_handed_split}");
    eprintln!("left-handed  pose change: {left_handed_split}");

    // (a) Both subjects must actually move — the control that proves this
    //     gate is measuring a real pose change and not two frozen frames.
    let rh_total = right_handed_split.left + right_handed_split.right;
    let lh_total = left_handed_split.left + left_handed_split.right;
    assert!(
        rh_total > 20 && lh_total > 20,
        "both subjects must change a substantial number of pixels when the bow pose is applied \
         (right-handed={rh_total}, left-handed={lh_total}) — near zero means the pose never \
         reached pixels at all, which this gate cannot distinguish handedness within"
    );

    // (b) The side that moves *more* must flip between the two subjects: the
    //     off (splaying) arm is the mob's left arm when right-handed and its
    //     right arm when left-handed — mirrored physical arms produce a
    //     mirrored screen-space asymmetry. This is the magnitude assertion
    //     (`CLAUDE.md`: predict which side wins, not merely that something
    //     moved) and it is a strict inequality in *both* frames, not just a
    //     "different from each other" comparison, so a pipeline that ignores
    //     handedness and always splays the same physical arm fails this even
    //     if by coincidence the two totals differ.
    assert!(
        right_handed_split.left != right_handed_split.right,
        "right-handed subject: left/right changed-pixel counts are exactly equal \
         ({right_handed_split}) — the splay must favour one side, or this gate cannot tell \
         which arm moved more"
    );
    assert!(
        left_handed_split.left != left_handed_split.right,
        "left-handed subject: left/right changed-pixel counts are exactly equal \
         ({left_handed_split}) — the splay must favour one side, or this gate cannot tell which \
         arm moved more"
    );
    let right_handed_favours_left = right_handed_split.left > right_handed_split.right;
    let left_handed_favours_left = left_handed_split.left > left_handed_split.right;
    assert_ne!(
        right_handed_favours_left, left_handed_favours_left,
        "the dominant side of the pose change must flip between the right-handed subject \
         ({right_handed_split}) and the left-handed one ({left_handed_split}) — if it does not, \
         `MobState::left_handed` is not reaching the bone rotation `Skeleton::pose_arms_for_item` \
         applies, even though the state assertions above show it reaching `AnimInput`"
    );
}
