//! The armour-stand pose reaches the rig's input: `ClientEvent` → the real
//! `IngestPlugin`/`EntityInterpPlugin` pair → the real `extract_entity_draws` →
//! `EntityDraw::anim.armor_stand_pose`.
//!
//! # Why this gate exists beside the rig's own unit gates
//!
//! `lodestone-render`'s `entity_anim` gates prove the *rig*: given a pose, the
//! six part rotations are assigned over the humanoid base pass exactly as
//! `ArmorStandArmorModel.setupAnim` does. They install their own `AnimInput`,
//! so they are structurally blind to the question this file asks — **what
//! constructs that input in production, and does it contain a pose at all?**
//!
//! That question is the whole defect. A stand carried along by a moving
//! contraption swung its arms like a running player, and an item in its hand
//! swung off the same arm, because six `ROTATIONS` fields were decoded for
//! alignment and dropped. Wiring the decode without wiring the extract step
//! would fix nothing: `extract_entity_draws` is where a *type* becomes a pose,
//! and the case that matters most is the one carrying no pose metadata at all.
//!
//! # The three claims, and why the third is the one that ships the fix
//!
//! 1. A stand that has reported a pose carries it, merged onto vanilla's
//!    defaults for the parts the packet did not mention.
//! 2. A zombie carries `None` — the specificity control. Without it, "the field
//!    is populated" is also satisfied by a chain that hands every humanoid a
//!    pose, which would freeze every mob in the game.
//! 3. **A stand that has reported nothing still carries
//!    `ArmorStandPose::VANILLA_DEFAULT`.** Vanilla's assignment is
//!    unconditional — `ArmorStand`'s own `defineId` calls register a non-zero
//!    default pose, and `setupAnim` writes it over the walk cycle whether or not
//!    a server ever sent one. A chain that populated this field only from the
//!    ECS component would pass claims 1 and 2 and leave every unposed stand
//!    animating, which is precisely the reported symptom.
//!
//! No GPU: the subject is the *producer*, and a headless adapter would add a
//! skip condition to a gate that has no rendering in it.

use bevy_ecs::world::World;
use lodestone::entities::{EntityDraw, EntityInterpPlugin, extracted_entity_draws, fold_entities};
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, GameTick, NetIngest};
use lodestone_model::{
    ArmorStandPose, ArmorStandPoseUpdate, ClientEvent, EntityMetadataUpdate, Rotation,
    Vec3 as ModelVec3, Vec3f,
};

/// The armour stand that gets posed, the one that never does, and a zombie.
const POSED_STAND: i32 = 1;
const UNPOSED_STAND: i32 = 2;
const ZOMBIE: i32 = 3;

/// Two parts, deliberately not all six: the merge is the property under test, so
/// the packet has to leave some parts unmentioned for the defaults to be
/// observable. Every value is pairwise distinct across both triples, so no two
/// parts can be exchanged without an assertion moving.
fn reported_pose_update() -> ArmorStandPoseUpdate {
    ArmorStandPoseUpdate {
        left_arm: Some(Vec3f::new(31.0, 32.0, 33.0)),
        right_leg: Some(Vec3f::new(-61.0, -62.0, -63.0)),
        ..ArmorStandPoseUpdate::default()
    }
}

fn world_with_stands() -> World {
    let mut app = App::new();
    app.add_plugins((IngestPlugin, EntityInterpPlugin));
    let mut world = std::mem::take(app.world_mut());

    for (id, kind) in [
        (POSED_STAND, "minecraft:armor_stand"),
        (UNPOSED_STAND, "minecraft:armor_stand"),
        (ZOMBIE, "minecraft:zombie"),
    ] {
        world
            .resource_mut::<IngestQueue>()
            .push(ClientEvent::EntitySpawned {
                entity_id: id,
                uuid: None,
                entity_type: kind.parse().expect("valid entity type key"),
                pos: ModelVec3::new(0.0, 64.0, 0.0),
                rotation: Rotation::new(0.0, 0.0),
                velocity: None,
            });
        world.run_schedule(NetIngest);
    }

    // Only one of the two stands is ever posed. The zombie is sent a pose too —
    // see `a_zombie_is_never_given_a_pose` for why that is the sharper control
    // than sending it nothing.
    for id in [POSED_STAND, ZOMBIE] {
        world
            .resource_mut::<IngestQueue>()
            .push(ClientEvent::EntityMetadataUpdated {
                entity_id: id,
                metadata: EntityMetadataUpdate {
                    armor_stand_pose: reported_pose_update(),
                    ..EntityMetadataUpdate::default()
                },
            });
    }
    world.run_schedule(NetIngest);

    fold_entities(&mut world);
    world.run_schedule(GameTick);
    world.run_schedule(Extract);
    world
}

fn draw_for(world: &World, id: i32) -> EntityDraw {
    extracted_entity_draws(world)
        .into_iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("entity {id} not among the extracted draws"))
}

/// A posed stand's six rotations reach `AnimInput`, with the four parts the
/// packet did not mention taking vanilla's own defaults rather than zeroes.
#[test]
fn a_posed_stands_rotations_reach_the_rigs_input() {
    let world = world_with_stands();
    assert_eq!(
        draw_for(&world, POSED_STAND).anim.armor_stand_pose,
        Some(ArmorStandPose {
            left_arm: Vec3f::new(31.0, 32.0, 33.0),
            right_leg: Vec3f::new(-61.0, -62.0, -63.0),
            ..ArmorStandPose::VANILLA_DEFAULT
        }),
        "the reported parts must arrive and the unreported ones must take \
         ArmorStand's own defineId defaults"
    );
}

/// The claim that actually fixes the reported bug: a stand nobody has ever posed
/// still carries a pose, so the rig still overwrites the humanoid walk cycle.
///
/// This is the arm a chain that read only the ECS component would fail. The
/// component is absent here — nothing ever mentioned this stand's parts — and
/// the field must still be `Some`.
#[test]
fn an_unposed_stand_still_carries_the_vanilla_default_pose() {
    let world = world_with_stands();
    assert_eq!(
        draw_for(&world, UNPOSED_STAND).anim.armor_stand_pose,
        Some(ArmorStandPose::VANILLA_DEFAULT),
        "a stand that has reported no pose must still be posed — vanilla's \
         ArmorStandArmorModel.setupAnim assigns the six rotations unconditionally, \
         so `None` here leaves the walk cycle standing and the stand swings its \
         arms as it moves"
    );
}

/// The specificity control, and it is deliberately the *hard* version: the
/// zombie was sent the same pose metadata the posed stand was, and must still
/// carry `None`.
///
/// Sending it nothing would prove only that an absent packet yields an absent
/// pose. Sending it the packet proves the gate is the **entity type**, which is
/// what stops every humanoid in the game freezing into an armour stand's pose —
/// the exact inverse defect, and the one a fix that keyed on the component alone
/// would introduce.
#[test]
fn a_zombie_is_never_given_a_pose() {
    let world = world_with_stands();
    assert_eq!(
        draw_for(&world, ZOMBIE).anim.armor_stand_pose,
        None,
        "only an armour stand takes the pose assignment; a zombie carrying one \
         would have its walk cycle overwritten and stand frozen"
    );
}

/// The premise the three claims above rest on: all three entities really were
/// extracted, so an assertion about one of them is measuring a draw rather than
/// an empty list.
#[test]
fn all_three_subjects_reach_the_extracted_draw_list() {
    let world = world_with_stands();
    let ids: Vec<i32> = extracted_entity_draws(&world).into_iter().map(|d| d.id).collect();
    for id in [POSED_STAND, UNPOSED_STAND, ZOMBIE] {
        assert!(ids.contains(&id), "entity {id} missing from {ids:?}");
    }
}
