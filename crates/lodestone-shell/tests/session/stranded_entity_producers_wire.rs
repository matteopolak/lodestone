//! The **producer** half for two of the three entity types
//! `cargo xtask world-coverage` reported as stranded: `ClientEvent` → the real
//! `IngestPlugin`/`EntityInterpPlugin` pair → the real `extract_entity_draws` →
//! the `EntityDraw` field the draw site reads.
//!
//! # Why this exists beside `entity_sprite_pixels.rs`
//!
//! That gate reaches rasterised pixels, which is the strongest evidence
//! available that the *renderer* works — and it is structurally blind to this
//! question, because it installs its own `EntityDraw`. Three fixes in this
//! repository have shipped with a passing pixel gate and been broken in real
//! play for exactly that reason: production derives the same input from the ECS
//! extract step and the wire, and the gate never touched either.
//!
//! So this file asks the other half: **what constructs that input in
//! production, and does it contain what the fixture contained?**
//!
//! * `EntityDraw::projectile_owner` — the fishing line's anchor. Its only
//!   channel is the spawn packet's Object Data field; if the bridge in
//!   `extract_entity_draws` is missing, every bobber floats unattached and the
//!   pixel gate still passes.
//! * `EntityDraw::item` on an `ominous_item_spawner` — the whole of what that
//!   entity draws. The metadata fold is keyed on the `ITEM_STACK` *serializer*
//!   rather than on the entity type, so it should already work; asserting it
//!   is what turns "should" into a checked claim, and it is the assumption the
//!   spawner's draw path was written on.
//!
//! The third type, `dragon_fireball`, has no producer to test: it carries no
//! metadata and no Object Data, so its position and type are the whole input and
//! `EntitySpawned` already covers them.
//!
//! # The controls
//!
//! A pig sent nothing must carry `None` in both fields. Without that, "the field
//! is populated" is also satisfied by a bridge that hands every entity an owner
//! or an item, which would put a fishing line on every mob in the world.
//!
//! No GPU: the subject is the producer, and a headless adapter would add a skip
//! condition to a gate with no rendering in it.

use bevy_ecs::world::World;
use lodestone::entities::{EntityDraw, EntityInterpPlugin, extracted_entity_draws, fold_entities};
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, GameTick, NetIngest};
use lodestone_model::{
    ClientEvent, EntityMetadataUpdate, Reported, Rotation, Vec3 as ModelVec3, item::ItemStack,
};

const BOBBER: i32 = 1;
const SPAWNER: i32 = 2;
const PIG: i32 = 3;

/// The caster's id. Deliberately **not** equal to [`BOBBER`], and not adjacent
/// to it either: the two are the same type and travel in the same packet, so a
/// fixture that reused one value could not tell a correct decode from one that
/// echoed the entity id back.
const OWNER: i32 = 44;

/// What the spawner is holding. A distinctive item rather than the first thing
/// to hand, so a bridge that substituted a default would be visible.
const SPAWNER_ITEM: &str = "minecraft:diamond_block";

fn world_with_subjects() -> World {
    let mut app = App::new();
    app.add_plugins((IngestPlugin, EntityInterpPlugin));
    let mut world = std::mem::take(app.world_mut());

    for (id, kind) in [
        (BOBBER, "minecraft:fishing_bobber"),
        (SPAWNER, "minecraft:ominous_item_spawner"),
        (PIG, "minecraft:pig"),
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

    // The bobber's owner, exactly as the adapter emits it: a second event in the
    // same channel, after the spawn.
    world
        .resource_mut::<IngestQueue>()
        .push(ClientEvent::ProjectileOwner {
            entity_id: BOBBER,
            owner_id: OWNER,
        });
    // The spawner's stack, through the ordinary metadata channel — the same one
    // a dropped item's stack travels on, which is the point: nothing about this
    // fold is spawner-specific and this gate is what says so.
    world
        .resource_mut::<IngestQueue>()
        .push(ClientEvent::EntityMetadataUpdated {
            entity_id: SPAWNER,
            metadata: EntityMetadataUpdate {
                item: Reported::Reported(Some(ItemStack::new(
                    SPAWNER_ITEM.parse().expect("valid item key"),
                    1,
                ))),
                ..EntityMetadataUpdate::default()
            },
        });
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

/// The fishing line's anchor survives the whole producer chain.
///
/// A miss here is silent in every other instrument: the packet decodes, the
/// component lands, and the draw site simply never sees an owner — which it
/// reads as "this bobber has no line", the same as a bobber that genuinely has
/// none.
#[test]
fn a_fishing_bobbers_owner_reaches_the_draw_record() {
    let world = world_with_subjects();
    assert_eq!(
        draw_for(&world, BOBBER).projectile_owner,
        Some(OWNER),
        "the caster's id must reach EntityDraw, or the line pass has nothing to \
         anchor to and every bobber floats free"
    );
}

/// The specificity control: an entity nobody reported an owner for carries
/// `None`.
///
/// Without this, the assertion above is satisfied by a bridge that hands every
/// entity the same owner — which would hang a fishing line off every pig in the
/// world.
#[test]
fn an_entity_with_no_reported_owner_carries_none() {
    let world = world_with_subjects();
    assert_eq!(
        draw_for(&world, PIG).projectile_owner, None,
        "a pig has no caster"
    );
    // And the spawner, which *did* get metadata — but of a different kind. This
    // is the sharper half of the control: it proves the absence comes from the
    // owner channel specifically and not merely from "this entity was sent
    // nothing".
    assert_eq!(
        draw_for(&world, SPAWNER).projectile_owner,
        None,
        "an ominous item spawner is not a projectile"
    );
}

/// The ominous item spawner's stack reaches the field its draw path reads.
///
/// The metadata fold routes an `ITEM_STACK` field by its **serializer**, not by
/// its index or its entity type, so this should hold for any entity that reports
/// one. "Should" is the word this gate replaces: the spawner's draw path is
/// written entirely on this assumption, and nothing else in the tree checks it
/// for a non-`item` entity type.
#[test]
fn an_ominous_item_spawners_stack_reaches_the_draw_record() {
    let world = world_with_subjects();
    let draw = draw_for(&world, SPAWNER);
    assert_eq!(
        draw.item.as_ref().map(ToString::to_string),
        Some(SPAWNER_ITEM.to_owned()),
        "the spawner draws its stack and nothing else, so a missing item here is the \
         whole entity missing"
    );
    // The count travels with it and drives `rendered_amount`'s one-to-five
    // cluster, so it is part of the same claim rather than a separate one.
    assert_eq!(draw.count, 1, "the reported count must survive the fold");
}

/// The control for the arm above: an entity that reported no stack carries
/// `None`, so a spawner's item cannot be coming from a default somewhere.
#[test]
fn an_entity_with_no_reported_stack_carries_no_item() {
    let world = world_with_subjects();
    assert_eq!(draw_for(&world, PIG).item, None, "a pig holds no stack");
    assert_eq!(
        draw_for(&world, BOBBER).item,
        None,
        "and neither does a fishing bobber"
    );
}
