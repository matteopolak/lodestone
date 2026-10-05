//! Posture state arrives from the wire: the tameable sitting bit, the fox's flag
//! byte and the cat's lying flag -> the real `IngestPlugin`/`EntityInterpPlugin` ->
//! the per-track posture ramps -> the real `extract_entity_draws` ->
//! `EntityDraw::anim.posture` (and a sleeping fox's sheet).
//!
//! Controls differ in the one input the claim is about: a tame wolf whose sitting
//! bit is clear, a fox with no flags, a cat that is not lying, and a pig that gets
//! the same sitting bit.

use bevy_ecs::world::World;
use lodestone::entities::{EntityDraw, EntityInterpPlugin, extracted_entity_draws, fold_entities};
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, GameTick, NetIngest};
use lodestone_model::{ClientEvent, EntityMetadataUpdate, MobAppearance, Rotation, Vec3 as ModelVec3};
use lodestone_render::entity::EntityModelSet;

fn world(kinds: &[(i32, &str)]) -> World {
    let mut app = App::new();
    app.add_plugins((IngestPlugin, EntityInterpPlugin));
    let mut world = std::mem::take(app.world_mut());
    world.insert_resource(lodestone_ecs::WorldTime { age: 0, time_of_day: 0 });
    for (id, kind) in kinds {
        world.resource_mut::<IngestQueue>().push(ClientEvent::EntitySpawned {
            entity_id: *id,
            uuid: None,
            entity_type: kind.parse().expect("valid entity type key"),
            pos: ModelVec3::new(0.0, 64.0, 0.0),
            rotation: Rotation::new(0.0, 0.0),
            velocity: None,
        });
        world.run_schedule(NetIngest);
    }
    fold_entities(&mut world);
    world.run_schedule(GameTick);
    world
}

fn deliver(world: &mut World, updates: Vec<(i32, EntityMetadataUpdate)>) {
    for (entity_id, metadata) in updates {
        world.resource_mut::<IngestQueue>().push(ClientEvent::EntityMetadataUpdated { entity_id, metadata });
    }
    world.run_schedule(NetIngest);
    fold_entities(world);
}

fn draw_for(world: &mut World, id: i32) -> EntityDraw {
    world.run_schedule(Extract);
    extracted_entity_draws(world)
        .into_iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("entity {id} not among the extracted draws"))
}

fn sitting(sitting: bool) -> EntityMetadataUpdate {
    EntityMetadataUpdate { tamed: Some(true), sitting: Some(sitting), ..Default::default() }
}

fn appearance(appearance: MobAppearance) -> EntityMetadataUpdate {
    EntityMetadataUpdate { appearance, ..Default::default() }
}

#[test]
fn a_tamed_wolf_and_cat_sitting_bit_reaches_the_draw() {
    let mut w = world(&[(1, "minecraft:wolf"), (2, "minecraft:wolf"), (3, "minecraft:cat"), (4, "minecraft:pig")]);
    assert!(!draw_for(&mut w, 1).anim.posture.sitting, "nothing reported yet");
    deliver(&mut w, vec![(1, sitting(true)), (2, sitting(false)), (3, sitting(true)), (4, sitting(true))]);
    w.run_schedule(GameTick);
    assert!(draw_for(&mut w, 1).anim.posture.sitting);
    assert!(!draw_for(&mut w, 2).anim.posture.sitting, "control: a clear bit stands");
    assert!(draw_for(&mut w, 3).anim.posture.sitting);
    assert!(!draw_for(&mut w, 4).anim.posture.sitting, "control: a pig keeps no posture");
    // A tame wolf at full health carries its tail at 0.55 PI rather than the wild PI / 5.
    let tail = draw_for(&mut w, 1).anim.posture.tail_angle;
    assert!((tail - 0.55 * std::f32::consts::PI).abs() < 1.0e-5, "{tail}");
    // Standing up again reaches the draw too.
    deliver(&mut w, vec![(1, sitting(false))]);
    w.run_schedule(GameTick);
    assert!(!draw_for(&mut w, 1).anim.posture.sitting);
}

#[test]
fn a_fox_flag_byte_reaches_the_draw_and_a_sleeping_fox_closes_its_eyes() {
    let mut w = world(&[(1, "minecraft:fox"), (2, "minecraft:fox"), (3, "minecraft:fox")]);
    let fox = |flags| appearance(MobAppearance { fox_flags: Some(flags), ..MobAppearance::default() });
    deliver(&mut w, vec![(1, fox(0x20)), (2, fox(0x00)), (3, fox(0x04))]);
    for _ in 0..3 {
        w.run_schedule(GameTick);
    }
    let asleep = draw_for(&mut w, 1);
    assert!(asleep.anim.posture.sleeping && !asleep.anim.posture.sitting);
    assert_eq!(asleep.variant_sheet, Some("entity/fox/fox_sleep"));
    let awake = draw_for(&mut w, 2);
    assert!(!awake.anim.posture.sleeping, "control: no flags, no sleep");
    assert_ne!(awake.variant_sheet, Some("entity/fox/fox_sleep"), "control: an awake fox keeps its eyes open");
    // Three ticks of crouching: 0.2 a tick, so 0.6 at the start of the next tick.
    let crouch = draw_for(&mut w, 3).anim.posture;
    assert!(crouch.crouching && crouch.crouch_amount > 0.39 && crouch.crouch_amount <= 0.6 + 1.0e-5, "{crouch:?}");
}

#[test]
fn a_cat_lying_flag_ramps_into_the_draw() {
    let mut w = world(&[(1, "minecraft:cat"), (2, "minecraft:cat")]);
    let lying = |on| appearance(MobAppearance { cat_lying: Some(on), ..MobAppearance::default() });
    deliver(&mut w, vec![(1, lying(true)), (2, lying(false))]);
    for _ in 0..10 {
        w.run_schedule(GameTick);
    }
    let lain = draw_for(&mut w, 1).anim.posture;
    assert!(lain.lie_down > 0.99 && lain.lie_down_tail > 0.7, "{lain:?}");
    assert_eq!(draw_for(&mut w, 2).anim.posture.lie_down, 0.0, "control: a cat that is not lying");
}

/// The extracted posture changes the drawn pose: the sitting wolf's skeleton, posed
/// from the very `AnimInput` the extraction produced, differs from the standing one.
#[test]
fn the_extracted_sitting_posture_moves_the_wolf_skeleton() {
    let mut w = world(&[(1, "minecraft:wolf"), (2, "minecraft:wolf")]);
    deliver(&mut w, vec![(1, sitting(true)), (2, sitting(false))]);
    w.run_schedule(GameTick);
    let set = EntityModelSet::load();
    let wolf = &set.get("wolf").expect("baked wolf").skeleton;
    let body = wolf.index_of("body").expect("a body");
    let sat = wolf.pose(&draw_for(&mut w, 1).anim)[body];
    let stood = wolf.pose(&draw_for(&mut w, 2).anim)[body];
    // Sitting drops the body pivot 4 units (a quarter block, model Y is down) and tips it.
    let drop = (stood.w_axis.y - sat.w_axis.y).abs();
    assert!((drop - 0.25).abs() < 1.0e-4, "the body pivot moved {drop} blocks");
}
