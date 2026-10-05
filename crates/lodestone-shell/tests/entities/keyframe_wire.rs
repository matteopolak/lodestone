//! Keyframe animation state arrives from the wire: an entity-event byte or a
//! metadata field -> the real `IngestPlugin`/`EntityInterpPlugin` -> the per-track
//! timers -> the real `extract_entity_draws` -> `EntityDraw::anim.keyframes`.
//!
//! Controls differ in the one input the claim is about: a rabbit that never gets the
//! event, a pig that gets the same event, and a bat whose flag is clear.

use bevy_ecs::world::World;
use lodestone::entities::{EntityDraw, EntityInterpPlugin, extracted_entity_draws, fold_entities};
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, GameTick, NetIngest};
use lodestone_model::{ClientEvent, EntityMetadataUpdate, MobAppearance, Rotation, Vec3 as ModelVec3};
use lodestone_render::entity_keyframe::{Flag, Slot};

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

fn deliver(world: &mut World, events: Vec<ClientEvent>) {
    for event in events {
        world.resource_mut::<IngestQueue>().push(event);
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

#[test]
fn a_rabbit_hop_event_reaches_the_draw_as_a_running_hop() {
    let mut w = world(&[(1, "minecraft:rabbit"), (2, "minecraft:rabbit"), (3, "minecraft:pig")]);
    assert!(!draw_for(&mut w, 1).anim.keyframes.started(Slot::Hop), "nothing has happened yet");
    deliver(
        &mut w,
        vec![
            ClientEvent::EntityStatus { entity_id: 1, status: 1 },
            ClientEvent::EntityStatus { entity_id: 3, status: 1 },
        ],
    );
    for _ in 0..3 {
        w.run_schedule(GameTick);
    }
    assert!(draw_for(&mut w, 1).anim.keyframes.started(Slot::Hop));
    assert!(!draw_for(&mut w, 2).anim.keyframes.started(Slot::Hop), "control: no event, no hop");
    assert!(!draw_for(&mut w, 3).anim.keyframes.started(Slot::Hop), "control: a pig has no hop");
}

#[test]
fn a_bat_roost_flag_reaches_the_draw() {
    let mut w = world(&[(1, "minecraft:bat"), (2, "minecraft:bat")]);
    let flags = |bits| EntityMetadataUpdate {
        appearance: MobAppearance { bat_flags: Some(bits), ..MobAppearance::default() },
        ..Default::default()
    };
    deliver(
        &mut w,
        vec![
            ClientEvent::EntityMetadataUpdated { entity_id: 1, metadata: flags(1) },
            ClientEvent::EntityMetadataUpdated { entity_id: 2, metadata: flags(0) },
        ],
    );
    for _ in 0..2 {
        w.run_schedule(GameTick);
    }
    let roosting = draw_for(&mut w, 1).anim.keyframes;
    assert!(roosting.started(Slot::Rest) && roosting.has(Flag::Resting) && !roosting.started(Slot::Fly));
    let flying = draw_for(&mut w, 2).anim.keyframes;
    assert!(flying.started(Slot::Fly) && !flying.has(Flag::Resting), "control: a clear flag flies");
}

#[test]
fn a_sniffer_state_and_an_armadillo_state_reach_the_draw() {
    let mut w = world(&[(1, "minecraft:sniffer"), (2, "minecraft:armadillo")]);
    let update = |sniffer: Option<u8>, armadillo: Option<u8>| EntityMetadataUpdate {
        appearance: MobAppearance { sniffer_state: sniffer, armadillo_state: armadillo, ..MobAppearance::default() },
        ..Default::default()
    };
    deliver(
        &mut w,
        vec![
            ClientEvent::EntityMetadataUpdated { entity_id: 1, metadata: update(Some(5), None) },
            ClientEvent::EntityMetadataUpdated { entity_id: 2, metadata: update(None, Some(2)) },
        ],
    );
    for _ in 0..2 {
        w.run_schedule(GameTick);
    }
    assert!(draw_for(&mut w, 1).anim.keyframes.started(Slot::Dig));
    let armadillo = draw_for(&mut w, 2).anim.keyframes;
    assert!(armadillo.has(Flag::Hiding) && armadillo.started(Slot::Peek));
}

/// A camel's dash flag changing after it came into view restarts the client's
/// 55-tick dash cooldown, which nods its head: one tick later the draw carries
/// `45 * 54 / 55` degrees less the partial tick's share, and the posed head pitches
/// by that much more than the same draw without the nod. Control: a camel whose
/// flag never changed has no nod.
#[test]
fn a_camel_dash_flag_nods_the_drawn_head() {
    use lodestone_render::entity::EntityModelSet;
    let mut w = world(&[(1, "minecraft:camel"), (2, "minecraft:camel")]);
    let dash = EntityMetadataUpdate {
        appearance: MobAppearance { camel_dash: Some(true), ..MobAppearance::default() },
        ..Default::default()
    };
    deliver(&mut w, vec![ClientEvent::EntityMetadataUpdated { entity_id: 1, metadata: dash }]);
    w.run_schedule(GameTick);
    let nodding = draw_for(&mut w, 1).anim;
    let still = draw_for(&mut w, 2).anim;
    let bump = nodding.keyframes.pitch_bump();
    assert!(
        (45.0 * 53.0 / 55.0..=45.0 * 54.0 / 55.0 + 1.0e-4).contains(&bump),
        "one tick into the cooldown: {bump}"
    );
    assert_eq!(still.keyframes.pitch_bump(), 0.0, "control: no flag change, no nod");
    let set = EntityModelSet::load();
    let camel = &set.get("camel").expect("baked camel").skeleton;
    let head = camel.index_of("head").expect("a head");
    let nod = |anim| {
        let pose = camel.pose(anim)[head];
        // The head's forward axis tips down by the pitch: its Y component in the
        // model frame.
        pose.z_axis.y.atan2(pose.z_axis.z).to_degrees()
    };
    // The same draw without its nod: the dash animation itself also moves the head.
    let unbumped = lodestone_render::AnimInput { keyframes: nodding.keyframes.with_pitch_bump(0.0), ..nodding };
    let diff = (nod(&nodding) - nod(&unbumped)).abs();
    assert!((diff - bump).abs() < 0.5, "the drawn head pitches by the nod: {diff} vs {bump}");
}
