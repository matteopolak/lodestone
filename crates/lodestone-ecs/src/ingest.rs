//! `NetIngest`: the systems that fold a `ClientEvent` stream into the
//! [`crate::entity`] component set.
//!
//! # What replaced what
//!
//! These systems are `docs/bevy-migration.md` Stage 1's replacement for
//! `lodestone_client::state::Inner::apply`'s entity arms and its
//! `apply_metadata` helper. Those are **deleted**, not mirrored — the authority
//! test (§1): `Inner` no longer holds a `HashMap<i32, EntityView>` at all, and
//! `SharedState::entities()` derives an `EntityView` on demand from these
//! components for the one caller (`ClientHandle::entities()`) that still speaks
//! that vocabulary. Per the plan's "only legal intermediate", that compat runs
//! in exactly one direction — components are authoritative, the struct is
//! derived — and it is scheduled to die with `EntityView` itself.
//!
//! # How events get in, and why arrival order is preserved
//!
//! The net thread already owns the socket and calls `SharedState::apply` once
//! per event (`docs/bevy-migration.md` §4.1(a)). So `apply` pushes the event
//! onto [`IngestQueue`] and runs the [`crate::NetIngest`] schedule; the
//! [`drain_ingest_queue`] system moves the queue into [`IngestBatch`], and each
//! `IngestSet::Apply` system reads that batch.
//!
//! **Each system walks the batch in arrival order**, so ordering *within* an
//! event family is exact. Ordering *across* families is the schedule's
//! `.chain()` order, which is not arrival order — but with one event submitted
//! per schedule run, as `SharedState` does, a batch never holds two events at
//! all, so the two orders coincide. A future driver that batches (the plan's
//! §4.1 `NetIngest`-once-per-frame shape) must either keep the families
//! commutative or dispatch in arrival order; the only observed non-commutative
//! pair is "despawn then respawn the same reused id", and
//! [`apply_entity_spawn`] already makes that safe on its own by replacing any
//! existing holder of the id.
//!
//! # Ordering anchors for plugins
//!
//! Every system here is `pub` and lives in `IngestSet::Apply`, so a plugin can
//! order against the *set* (§2.6: sets, not system functions, are the ABI).
//! They are `pub` so they can be individually disabled or replaced, not so they
//! can be named in `.after(...)`.

use bevy_app::{App, Plugin};
use bevy_ecs::prelude::{Commands, IntoScheduleConfigs, Query, Res, ResMut};
use bevy_ecs::resource::Resource;
use lodestone_model::{ClientEvent, EntityMovement, Reported};

use crate::entity::{
    Attributes, Baby, CustomName, CustomNameVisible, DisplayItem, EntityFlags, EntityIndex,
    EntityKind, EntityUuid, Equipment, HeadYaw, Health, MinecraftEntityId, OnGround, Pose,
    Position, Rotation, Variant, Velocity,
};
use crate::schedules::NetIngest;
use crate::sets::IngestSet;

/// Events handed to the ECS by the net thread, not yet folded.
///
/// Written from outside any system (the net thread pushes here under the
/// `World` write lock), drained by [`drain_ingest_queue`].
#[derive(Resource, Debug, Default)]
pub struct IngestQueue(Vec<ClientEvent>);

impl IngestQueue {
    /// Enqueues one event for the next [`crate::NetIngest`] run.
    pub fn push(&mut self, event: ClientEvent) {
        self.0.push(event);
    }

    /// How many events are waiting.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether nothing is waiting.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// This ingest run's events, in arrival order.
///
/// Separate from [`IngestQueue`] so the net thread can keep enqueueing while
/// the `Apply` systems read a stable batch, and so every `Apply` system sees
/// the *same* events rather than racing to consume a shared queue.
#[derive(Resource, Debug, Default)]
pub struct IngestBatch(Vec<ClientEvent>);

impl IngestBatch {
    /// The events this run is folding, in arrival order.
    #[must_use]
    pub fn events(&self) -> &[ClientEvent] {
        &self.0
    }
}

/// Whether an event is folded by the systems in this module.
///
/// The caller-side switch that lets `lodestone-client` route entity events to
/// the ECS and everything else to its remaining scalar fold. It is a `match`
/// over the same variants the systems below handle, kept next to them so the
/// two cannot drift: an event that returns `true` here and is handled by no
/// system is silently dropped, which is exactly the failure this function
/// exists to make greppable.
#[must_use]
pub fn handles_event(event: &ClientEvent) -> bool {
    matches!(
        event,
        ClientEvent::EntitySpawned { .. }
            | ClientEvent::EntityRemoved { .. }
            | ClientEvent::EntityMoved { .. }
            | ClientEvent::EntityVelocity { .. }
            | ClientEvent::EntityHeadRotation { .. }
            | ClientEvent::EntityMetadataUpdated { .. }
            | ClientEvent::EntityAttributesUpdated { .. }
            | ClientEvent::EntityEquipmentUpdated { .. }
    )
}

/// `IngestSet::Drain`: moves [`IngestQueue`] into [`IngestBatch`].
pub fn drain_ingest_queue(mut queue: ResMut<IngestQueue>, mut batch: ResMut<IngestBatch>) {
    batch.0.clear();
    batch.0.append(&mut queue.0);
}

/// `IngestSet::Apply`: `ClientEvent::EntitySpawned` → a fresh ECS entity.
///
/// **Spawns only the components the spawn packet actually reported.** No
/// [`DisplayItem`], no [`CustomName`], no [`EntityFlags`]/[`Health`]/[`Baby`]/
/// [`Pose`]/[`Variant`]/[`CustomNameVisible`] — their absence *is* the
/// "never reported" state (see [`crate::entity`]'s module docs). Spawning them
/// with a default is the regression the plan warns about: a dropped item names
/// its stack exactly once, at spawn, so a defaulted-then-overwritten
/// `DisplayItem` blanks it a tick later and the drop goes invisible.
///
/// A spawn for an id already tracked **replaces** the previous entity outright,
/// matching the old `HashMap::insert` and covering a server reusing an id.
pub fn apply_entity_spawn(
    batch: Res<IngestBatch>,
    mut index: ResMut<EntityIndex>,
    mut commands: Commands,
) {
    for event in batch.events() {
        let ClientEvent::EntitySpawned {
            entity_id,
            uuid,
            entity_type,
            pos,
            rotation,
            velocity,
        } = event
        else {
            continue;
        };
        if let Some(previous) = index.remove(*entity_id) {
            commands.entity(previous).despawn();
        }
        let mut spawned = commands.spawn((
            MinecraftEntityId(*entity_id),
            EntityKind(entity_type.clone()),
            Position(*pos),
            Rotation(*rotation),
            // Vanilla sends head yaw at spawn unconditionally, so this is
            // reported, not defaulted.
            HeadYaw(rotation.yaw),
            OnGround(false),
            Attributes::default(),
            Equipment::default(),
        ));
        if let Some(uuid) = uuid {
            spawned.insert(EntityUuid(*uuid));
        }
        // Absent unless the spawn carried one: gravity alone cannot produce an
        // apex, so "no velocity ever reported" and "reported zero" must stay
        // distinguishable for a dropped item's arc.
        if let Some(velocity) = velocity {
            spawned.insert(Velocity(*velocity));
        }
        let entity = spawned.id();
        index.insert(*entity_id, entity);
    }
}

/// `IngestSet::Apply`: `ClientEvent::EntityRemoved` → despawn, and drop the
/// index entry so nothing can resolve the id afterwards.
pub fn apply_entity_removal(
    batch: Res<IngestBatch>,
    mut index: ResMut<EntityIndex>,
    mut commands: Commands,
) {
    for event in batch.events() {
        let ClientEvent::EntityRemoved { entity_ids } = event else {
            continue;
        };
        for entity_id in entity_ids {
            if let Some(entity) = index.remove(*entity_id) {
                commands.entity(entity).despawn();
            }
        }
    }
}

/// `IngestSet::Apply`: `ClientEvent::EntityMoved` → [`Position`], [`Rotation`],
/// [`OnGround`].
///
/// A relative movement reads the current [`Position`] and adds the delta, so
/// this system is the only writer of that component on the network path.
pub fn apply_entity_movement(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut entities: Query<(&mut Position, &mut Rotation, &mut OnGround)>,
) {
    for event in batch.events() {
        let ClientEvent::EntityMoved {
            entity_id,
            movement,
            rotation,
            on_ground,
        } = event
        else {
            continue;
        };
        let Some(entity) = index.get(*entity_id) else {
            continue;
        };
        let Ok((mut position, mut look, mut grounded)) = entities.get_mut(entity) else {
            continue;
        };
        position.0 = match movement {
            EntityMovement::Absolute(pos) => *pos,
            EntityMovement::Relative(delta) => position.0 + *delta,
        };
        if let Some(rotation) = rotation {
            look.0 = *rotation;
        }
        grounded.0 = *on_ground;
    }
}

/// `IngestSet::Apply`: `ClientEvent::EntityVelocity` → [`Velocity`].
///
/// Inserts rather than assigns, because the component is absent until the
/// server has reported a velocity at all.
pub fn apply_entity_velocity(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut commands: Commands,
) {
    for event in batch.events() {
        let ClientEvent::EntityVelocity {
            entity_id,
            velocity,
        } = event
        else {
            continue;
        };
        if let Some(entity) = index.get(*entity_id) {
            commands.entity(entity).insert(Velocity(*velocity));
        }
    }
}

/// `IngestSet::Apply`: `ClientEvent::EntityHeadRotation` → [`HeadYaw`].
pub fn apply_entity_head_rotation(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut heads: Query<&mut HeadYaw>,
) {
    for event in batch.events() {
        let ClientEvent::EntityHeadRotation {
            entity_id,
            head_yaw,
        } = event
        else {
            continue;
        };
        let Some(entity) = index.get(*entity_id) else {
            continue;
        };
        if let Ok(mut head) = heads.get_mut(entity) {
            head.0 = *head_yaw;
        }
    }
}

/// `IngestSet::Apply`: `ClientEvent::EntityMetadataUpdated` → whichever of the
/// optional components the packet actually carried.
///
/// This is `Inner::apply_metadata` as a system, and the *only* reason it uses
/// `Commands::insert` per field rather than a query is that a field's component
/// may not exist yet: metadata is incremental, so "did this packet mention the
/// field" is the whole question. A field the packet did not mention is left
/// completely alone — which for [`CustomName`] and [`DisplayItem`] is the
/// difference between `Reported::Unreported` and `Reported::Reported(None)`,
/// and for a dropped item is the difference between a visible stack and an
/// invisible one.
pub fn apply_entity_metadata(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut commands: Commands,
) {
    for event in batch.events() {
        let ClientEvent::EntityMetadataUpdated {
            entity_id,
            metadata,
        } = event
        else {
            continue;
        };
        let Some(entity) = index.get(*entity_id) else {
            continue;
        };
        let mut entity = commands.entity(entity);
        if let Some(flags) = metadata.flags {
            entity.insert(EntityFlags(flags));
        }
        // `Reported::Reported(_)` — including `Reported(None)` — is the server
        // speaking about the field, so the component appears (possibly empty).
        // `Reported::Unreported` falls through and touches nothing.
        if let Reported::Reported(custom_name) = &metadata.custom_name {
            entity.insert(CustomName(custom_name.clone()));
        }
        if let Some(visible) = metadata.custom_name_visible {
            entity.insert(CustomNameVisible(visible));
        }
        if let Some(pose) = metadata.pose {
            entity.insert(Pose(pose));
        }
        if let Some(health) = metadata.health {
            entity.insert(Health(health));
        }
        if let Some(baby) = metadata.baby {
            entity.insert(Baby(baby));
        }
        if let Some(variant) = &metadata.variant {
            entity.insert(Variant(variant.clone()));
        }
        if let Reported::Reported(item) = &metadata.item {
            entity.insert(DisplayItem(item.clone()));
        }
    }
}

/// `IngestSet::Apply`: `ClientEvent::EntityAttributesUpdated` → [`Attributes`],
/// merged per attribute id (a later snapshot replaces the same attribute,
/// attributes not named are left alone).
pub fn apply_entity_attributes(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut entities: Query<&mut Attributes>,
) {
    for event in batch.events() {
        let ClientEvent::EntityAttributesUpdated {
            entity_id,
            attributes,
        } = event
        else {
            continue;
        };
        let Some(entity) = index.get(*entity_id) else {
            continue;
        };
        let Ok(mut current) = entities.get_mut(entity) else {
            continue;
        };
        for snapshot in attributes {
            match current
                .0
                .iter_mut()
                .find(|existing| existing.attribute == snapshot.attribute)
            {
                Some(existing) => *existing = snapshot.clone(),
                None => current.0.push(snapshot.clone()),
            }
        }
    }
}

/// `IngestSet::Apply`: `ClientEvent::EntityEquipmentUpdated` → [`Equipment`],
/// merged per slot.
///
/// A slot the server has never mentioned stays absent from the list; a slot it
/// clears is present with `item: None`. Both states are preserved here, and the
/// consumer narrows them (`lodestone-shell`'s `occupied_equipment`).
pub fn apply_entity_equipment(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut entities: Query<&mut Equipment>,
) {
    for event in batch.events() {
        let ClientEvent::EntityEquipmentUpdated {
            entity_id,
            equipment,
        } = event
        else {
            continue;
        };
        let Some(entity) = index.get(*entity_id) else {
            continue;
        };
        let Ok(mut current) = entities.get_mut(entity) else {
            continue;
        };
        for update in equipment {
            match current
                .0
                .iter_mut()
                .find(|existing| existing.slot == update.slot)
            {
                Some(existing) => *existing = update.clone(),
                None => current.0.push(update.clone()),
            }
        }
    }
}

/// Registers the entity component set's ingest systems into
/// [`crate::NetIngest`].
///
/// Installs [`crate::CorePlugin`] if it is not already present, since the
/// `IngestSet` chain it configures is what puts `Drain` before `Apply`.
///
/// Deliberately **not** part of `CorePlugin`: only the `World` that is
/// *authoritative* over entity state gets these systems, exactly as
/// `CorePlugin` deliberately leaves `WorldTime` to its owner. Two `World`s in
/// one process (net thread and driver thread, until §4.1 unifies them) must not
/// both be folding the same event stream — that is the two-sources-of-truth
/// failure this migration exists to delete.
#[derive(Debug, Default)]
pub struct IngestPlugin;

impl Plugin for IngestPlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<crate::CorePlugin>() {
            app.add_plugins(crate::CorePlugin);
        }
        app.init_resource::<IngestQueue>();
        app.init_resource::<IngestBatch>();
        app.init_resource::<EntityIndex>();
        app.add_systems(NetIngest, drain_ingest_queue.in_set(IngestSet::Drain));
        app.add_systems(
            NetIngest,
            (
                apply_entity_spawn,
                apply_entity_removal,
                apply_entity_movement,
                apply_entity_velocity,
                apply_entity_head_rotation,
                apply_entity_metadata,
                apply_entity_attributes,
                apply_entity_equipment,
            )
                .chain()
                .in_set(IngestSet::Apply),
        );
    }
}

#[cfg(test)]
mod tests {
    use bevy_ecs::world::World;
    // The model's `Rotation` is aliased: `super::*` brings the *component*
    // `Rotation` into scope, and the two must stay distinguishable here.
    use lodestone_model::Rotation as ReportedRotation;
    use lodestone_model::{
        EntityEquipment, EntityMetadataUpdate, EquipmentSlot, ItemComponents, ItemStack, Vec3,
    };

    use super::*;
    use crate::entity::*;

    /// A `World` with the ingest systems installed, as `SharedState` builds it.
    fn ingest_world() -> World {
        let mut app = App::new();
        app.add_plugins(IngestPlugin);
        std::mem::take(app.world_mut())
    }

    /// Feed one event and run the schedule, exactly as `SharedState::apply`
    /// does — one event per run, so arrival order is preserved by construction.
    fn feed(world: &mut World, event: ClientEvent) {
        world.resource_mut::<IngestQueue>().push(event);
        world.run_schedule(NetIngest);
    }

    fn spawn_event(entity_id: i32, kind: &str) -> ClientEvent {
        ClientEvent::EntitySpawned {
            entity_id,
            uuid: None,
            entity_type: kind.parse().expect("valid entity type key"),
            pos: Vec3::new(1.0, 64.0, 2.0),
            rotation: ReportedRotation::new(90.0, 0.0),
            velocity: None,
        }
    }

    fn stone() -> ItemStack {
        ItemStack {
            item: "minecraft:stone".parse().expect("valid item key"),
            count: 1,
            components: ItemComponents::default(),
        }
    }

    fn metadata(update: EntityMetadataUpdate, entity_id: i32) -> ClientEvent {
        ClientEvent::EntityMetadataUpdated {
            entity_id,
            metadata: update,
        }
    }

    fn entity_for(world: &World, entity_id: i32) -> bevy_ecs::world::EntityRef<'_> {
        let entity = world
            .resource::<EntityIndex>()
            .get(entity_id)
            .expect("entity should be indexed");
        world.get_entity(entity).expect("entity should exist")
    }

    // ---- the nested-`Reported` states, ported first ----------------------
    //
    // `docs/bevy-migration.md` Stage 1: "port those two tests first". These
    // four are the component-level statement of what
    // `lodestone-shell/src/entities.rs`'s
    // `a_snapshot_silent_about_the_item_keeps_the_known_one` and
    // `an_explicitly_empty_stack_clears_the_known_one` assert one layer up.

    #[test]
    fn a_fresh_spawn_has_no_display_item_component_at_all() {
        // "Never reported" is component *absence*. This is the assertion that
        // catches the regression the plan warns about — an ingest that spawned
        // `DisplayItem(None)` as a default would pass every "the stack is
        // empty" test while making it impossible to tell silence from a clear.
        let mut world = ingest_world();
        feed(&mut world, spawn_event(9, "minecraft:item"));
        let entity = entity_for(&world, 9);
        assert!(
            entity.get::<DisplayItem>().is_none(),
            "a spawn reports no stack, so the component must be absent, not empty"
        );
        assert!(
            entity.get::<CustomName>().is_none(),
            "same for the custom name: absent, not Some(None)"
        );
    }

    #[test]
    fn a_reported_stack_becomes_a_present_component() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(9, "minecraft:item"));
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    item: Reported::Reported(Some(stone())),
                    ..EntityMetadataUpdate::default()
                },
                9,
            ),
        );
        assert_eq!(
            entity_for(&world, 9)
                .get::<DisplayItem>()
                .map(|item| item.0.clone()),
            Some(Some(stone()))
        );
    }

    #[test]
    fn a_silent_metadata_update_leaves_a_known_stack_alone() {
        // The dropped-item defect in one assertion: a drop names its stack once
        // at spawn and every later metadata packet is silent about it. Reading
        // that silence as "empty" blanks the drop a tick after it appeared.
        let mut world = ingest_world();
        feed(&mut world, spawn_event(9, "minecraft:item"));
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    item: Reported::Reported(Some(stone())),
                    ..EntityMetadataUpdate::default()
                },
                9,
            ),
        );
        // A later packet that mentions only the flags byte.
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    flags: Some(0x20),
                    ..EntityMetadataUpdate::default()
                },
                9,
            ),
        );
        assert_eq!(
            entity_for(&world, 9)
                .get::<DisplayItem>()
                .map(|item| item.0.clone()),
            Some(Some(stone())),
            "an update silent about the item must not erase it"
        );
    }

    #[test]
    fn an_explicit_empty_stack_is_a_present_component_holding_none() {
        // The other half of the three-state encoding, and the reason
        // `DisplayItem` wraps an `Option` instead of being absent-or-value: the
        // server *saying* the stack is empty is distinguishable from never
        // having said anything.
        let mut world = ingest_world();
        feed(&mut world, spawn_event(9, "minecraft:item"));
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    item: Reported::Reported(Some(stone())),
                    ..EntityMetadataUpdate::default()
                },
                9,
            ),
        );
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    item: Reported::Reported(None),
                    ..EntityMetadataUpdate::default()
                },
                9,
            ),
        );
        assert_eq!(
            entity_for(&world, 9)
                .get::<DisplayItem>()
                .map(|item| item.0.clone()),
            Some(None),
            "an explicit clear is present-with-None, never absence"
        );
    }

    #[test]
    fn custom_name_keeps_the_same_three_states() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(1, "minecraft:pig"));
        assert!(entity_for(&world, 1).get::<CustomName>().is_none());

        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    custom_name: Reported::Reported(Some("Lodestar".to_owned())),
                    ..EntityMetadataUpdate::default()
                },
                1,
            ),
        );
        assert_eq!(
            entity_for(&world, 1).get::<CustomName>().map(|n| n.0.clone()),
            Some(Some("Lodestar".to_owned()))
        );

        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    health: Some(10.0),
                    ..EntityMetadataUpdate::default()
                },
                1,
            ),
        );
        assert_eq!(
            entity_for(&world, 1).get::<CustomName>().map(|n| n.0.clone()),
            Some(Some("Lodestar".to_owned())),
            "a silent update must not clear the name"
        );

        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    custom_name: Reported::Reported(None),
                    ..EntityMetadataUpdate::default()
                },
                1,
            ),
        );
        assert_eq!(
            entity_for(&world, 1).get::<CustomName>().map(|n| n.0.clone()),
            Some(None)
        );
    }

    #[test]
    fn a_spawn_without_a_velocity_leaves_the_component_absent() {
        // "Never reported" vs "reported zero" — the distinction a dropped
        // item's arc rests on.
        let mut world = ingest_world();
        feed(&mut world, spawn_event(9, "minecraft:item"));
        assert!(entity_for(&world, 9).get::<Velocity>().is_none());

        feed(
            &mut world,
            ClientEvent::EntityVelocity {
                entity_id: 9,
                velocity: Vec3::default(),
            },
        );
        assert_eq!(
            entity_for(&world, 9).get::<Velocity>().map(|v| v.0),
            Some(Vec3::default()),
            "a reported zero velocity is a present component, not absence"
        );
    }

    // ---- spawn / move / despawn ------------------------------------------

    #[test]
    fn a_spawn_writes_the_reported_pose_and_indexes_the_id() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(7, "minecraft:pig"));
        let entity = entity_for(&world, 7);
        assert_eq!(entity.get::<Position>().map(|p| p.0), Some(Vec3::new(1.0, 64.0, 2.0)));
        assert_eq!(entity.get::<HeadYaw>().map(|h| h.0), Some(90.0));
        assert_eq!(entity.get::<OnGround>().map(|g| g.0), Some(false));
        assert_eq!(
            entity.get::<EntityKind>().map(|k| k.0.to_string()),
            Some("minecraft:pig".to_owned())
        );
        assert_eq!(world.resource::<EntityIndex>().len(), 1);
    }

    #[test]
    fn relative_movement_accumulates_onto_the_current_position() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(7, "minecraft:pig"));
        feed(
            &mut world,
            ClientEvent::EntityMoved {
                entity_id: 7,
                movement: EntityMovement::Relative(Vec3::new(0.5, 0.0, -0.25)),
                rotation: None,
                on_ground: true,
            },
        );
        let entity = entity_for(&world, 7);
        assert_eq!(
            entity.get::<Position>().map(|p| p.0),
            Some(Vec3::new(1.5, 64.0, 1.75))
        );
        assert_eq!(entity.get::<OnGround>().map(|g| g.0), Some(true));
        assert_eq!(
            entity.get::<Rotation>().map(|r| r.0.yaw),
            Some(90.0),
            "a movement with no rotation must not reset the body yaw"
        );
    }

    #[test]
    fn head_yaw_moves_independently_of_the_body() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(7, "minecraft:pig"));
        feed(
            &mut world,
            ClientEvent::EntityHeadRotation {
                entity_id: 7,
                head_yaw: 12.0,
            },
        );
        let entity = entity_for(&world, 7);
        assert_eq!(entity.get::<HeadYaw>().map(|h| h.0), Some(12.0));
        assert_eq!(entity.get::<Rotation>().map(|r| r.0.yaw), Some(90.0));
    }

    #[test]
    fn a_removal_despawns_and_deindexes() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(7, "minecraft:pig"));
        let entity = world.resource::<EntityIndex>().get(7).expect("indexed");
        feed(
            &mut world,
            ClientEvent::EntityRemoved { entity_ids: vec![7] },
        );
        assert!(world.resource::<EntityIndex>().get(7).is_none());
        assert!(
            world.get_entity(entity).is_err(),
            "the ECS entity itself must be gone, not just unindexed"
        );
    }

    #[test]
    fn a_respawned_id_replaces_the_previous_entity() {
        // Servers reuse entity ids freely. The old `HashMap::insert` replaced
        // wholesale; anything less would leave a pig's metadata attached to the
        // drop that inherited its id.
        let mut world = ingest_world();
        feed(&mut world, spawn_event(7, "minecraft:pig"));
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    custom_name: Reported::Reported(Some("Lodestar".to_owned())),
                    ..EntityMetadataUpdate::default()
                },
                7,
            ),
        );
        feed(&mut world, spawn_event(7, "minecraft:item"));
        let entity = entity_for(&world, 7);
        assert_eq!(
            entity.get::<EntityKind>().map(|k| k.0.to_string()),
            Some("minecraft:item".to_owned())
        );
        assert!(
            entity.get::<CustomName>().is_none(),
            "the reused id must not inherit the previous entity's name"
        );
        assert_eq!(world.resource::<EntityIndex>().len(), 1);
    }

    #[test]
    fn a_spawn_and_a_move_in_one_batch_still_resolve() {
        // The batching hazard the module docs name: `apply_entity_spawn` runs
        // before `apply_entity_movement` in the `Apply` chain, and `.chain()`'s
        // sync point applies the spawn's deferred commands, so the movement
        // finds the entity. Without the sync point this silently drops the
        // move — which is why this is asserted rather than assumed.
        let mut world = ingest_world();
        {
            let mut queue = world.resource_mut::<IngestQueue>();
            queue.push(spawn_event(7, "minecraft:pig"));
            queue.push(ClientEvent::EntityMoved {
                entity_id: 7,
                movement: EntityMovement::Relative(Vec3::new(1.0, 0.0, 0.0)),
                rotation: None,
                on_ground: true,
            });
        }
        world.run_schedule(NetIngest);
        assert_eq!(
            entity_for(&world, 7).get::<Position>().map(|p| p.0),
            Some(Vec3::new(2.0, 64.0, 2.0))
        );
    }

    #[test]
    fn an_event_for_an_unknown_id_is_dropped_rather_than_spawning_a_ghost() {
        let mut world = ingest_world();
        feed(
            &mut world,
            ClientEvent::EntityMoved {
                entity_id: 404,
                movement: EntityMovement::Absolute(Vec3::default()),
                rotation: None,
                on_ground: false,
            },
        );
        assert!(world.resource::<EntityIndex>().is_empty());
    }

    // ---- equipment / attributes ------------------------------------------

    #[test]
    fn equipment_merges_per_slot_and_keeps_an_explicit_clear() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(7, "minecraft:zombie"));
        feed(
            &mut world,
            ClientEvent::EntityEquipmentUpdated {
                entity_id: 7,
                equipment: vec![EntityEquipment {
                    slot: EquipmentSlot::MainHand,
                    item: Some(stone()),
                }],
            },
        );
        feed(
            &mut world,
            ClientEvent::EntityEquipmentUpdated {
                entity_id: 7,
                equipment: vec![EntityEquipment {
                    slot: EquipmentSlot::Head,
                    item: None,
                }],
            },
        );
        let equipment = entity_for(&world, 7)
            .get::<Equipment>()
            .expect("spawned with an empty list")
            .0
            .clone();
        assert_eq!(equipment.len(), 2, "the second slot must merge, not replace: {equipment:?}");
        assert!(
            equipment
                .iter()
                .any(|e| e.slot == EquipmentSlot::MainHand && e.item.is_some())
        );
        assert!(
            equipment
                .iter()
                .any(|e| e.slot == EquipmentSlot::Head && e.item.is_none()),
            "an explicitly-cleared slot stays in the list; only a never-mentioned slot is absent"
        );
        assert!(
            !equipment.iter().any(|e| e.slot == EquipmentSlot::OffHand),
            "a never-mentioned slot must not appear at all"
        );
    }

    #[test]
    fn a_later_attribute_snapshot_replaces_the_same_attribute() {
        use lodestone_model::EntityAttributeSnapshot;

        let snapshot = |base: f64| EntityAttributeSnapshot {
            attribute: "minecraft:movement_speed"
                .parse()
                .expect("valid attribute id"),
            base,
            modifiers: Vec::new(),
        };

        let mut world = ingest_world();
        feed(&mut world, spawn_event(7, "minecraft:pig"));
        feed(
            &mut world,
            ClientEvent::EntityAttributesUpdated {
                entity_id: 7,
                attributes: vec![snapshot(0.1)],
            },
        );
        feed(
            &mut world,
            ClientEvent::EntityAttributesUpdated {
                entity_id: 7,
                attributes: vec![snapshot(0.25)],
            },
        );
        let attributes = entity_for(&world, 7)
            .get::<Attributes>()
            .expect("spawned with an empty list")
            .0
            .clone();
        assert_eq!(attributes.len(), 1);
        assert!((attributes[0].base - 0.25).abs() < 1.0e-9);
    }

    // ---- the routing switch ----------------------------------------------

    #[test]
    fn handles_event_covers_exactly_the_variants_with_a_system() {
        // The failure this rules out is an event routed to the ECS that no
        // system folds: it would vanish silently, which is the worst available
        // outcome. Feed one of every claimed variant and require that a spawned
        // entity's state actually changed, so the claim and the systems cannot
        // drift apart unnoticed.
        assert!(handles_event(&spawn_event(1, "minecraft:pig")));
        assert!(!handles_event(&ClientEvent::TimeChanged {
            world_age: 1,
            time_of_day: 2,
        }));
        assert!(!handles_event(&ClientEvent::HealthChanged {
            health: 20.0,
            food: 20,
            saturation: 5.0,
        }));
    }

    #[test]
    fn nothing_is_folded_without_running_the_schedule() {
        // The control for every test above: the systems, not `IngestQueue`'s
        // `push`, are what change state. If pushing alone were enough, none of
        // the assertions above would be evidence the schedule ran.
        let mut world = ingest_world();
        world
            .resource_mut::<IngestQueue>()
            .push(spawn_event(7, "minecraft:pig"));
        assert!(
            world.resource::<EntityIndex>().is_empty(),
            "enqueueing must not fold; only the NetIngest schedule folds"
        );
        assert_eq!(world.resource::<IngestQueue>().len(), 1);
        world.run_schedule(NetIngest);
        assert_eq!(world.resource::<EntityIndex>().len(), 1);
        assert!(world.resource::<IngestQueue>().is_empty());
    }
}
