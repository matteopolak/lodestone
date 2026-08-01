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
use bevy_ecs::entity::Entity;
use bevy_ecs::prelude::{Commands, IntoScheduleConfigs, Query, Res, ResMut, With};
use bevy_ecs::resource::Resource;
use bevy_ecs::world::World;
use lodestone_model::{AnimationAction, ClientEvent, EntityMovement, Reported};
use lodestone_physics::Vec3d;

use crate::entity::{
    Attributes, AttackSwing, Baby, CustomName, CustomNameVisible, DisplayItem, EntityFlags,
    EntityIndex, EntityKind, EntityUuid, Equipment, HeadYaw, Health, HurtTime, MinecraftEntityId,
    OnGround, Pose, Position, Rotation, Variant, Velocity,
};
use crate::player::{LocalPlayer, PhysicsState};
use crate::schedules::{GameTick, NetIngest};
use crate::sets::{IngestSet, TickSet};

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
        // `Login` is claimed by [`apply_local_player_login`], and *also* by
        // `crate::session::apply_local_player_state`. Both is correct and not a
        // double fold: they write disjoint state off the same event (this side
        // the index and the entity id component, that side the session scalars),
        // and `SharedState::apply` routes on `ingest::handles_event(e) ||
        // session::handles_event(e)`, so the event reaches the one schedule once
        // either way.
        ClientEvent::Login { .. }
            | ClientEvent::EntitySpawned { .. }
            | ClientEvent::EntityRemoved { .. }
            | ClientEvent::EntityMoved { .. }
            | ClientEvent::EntityVelocity { .. }
            | ClientEvent::EntityHeadRotation { .. }
            | ClientEvent::EntityMetadataUpdated { .. }
            | ClientEvent::EntityAttributesUpdated { .. }
            | ClientEvent::EntityEquipmentUpdated { .. }
            | ClientEvent::EntityDamaged { .. }
            | ClientEvent::EntityHurtAnimation { .. }
            | ClientEvent::EntityAnimation { .. }
    )
}

/// `IngestSet::Drain`: moves [`IngestQueue`] into [`IngestBatch`].
pub fn drain_ingest_queue(mut queue: ResMut<IngestQueue>, mut batch: ResMut<IngestBatch>) {
    batch.0.clear();
    batch.0.append(&mut queue.0);
}

/// `IngestSet::Apply`: `ClientEvent::Login` → the **local player** joins
/// [`EntityIndex`] under the id the server just assigned us.
///
/// # The hole this closes
///
/// [`EntityIndex`] used to be populated *only* by [`apply_entity_spawn`], driven
/// only by `ClientEvent::EntitySpawned` — and **vanilla never sends an
/// `AddEntity` for yourself, only `Login`**. So every id-addressed ingest system
/// silently `continue`d for our own id: the server's own `update_attributes` for
/// the local player was folded into nothing at all, which is why
/// `docs/swimming.md` could not reach Depth Strider's
/// `minecraft:water_movement_efficiency` however correct the arithmetic
/// underneath it was. The hole is *general* — any future per-player component fed
/// from entity ingest had it too — so it is closed here rather than by teaching
/// one system about one attribute.
///
/// # What the local player deliberately does **not** get
///
/// Only [`MinecraftEntityId`] and [`Attributes`]. **No** [`EntityKind`],
/// [`Position`], [`Rotation`] or [`HeadYaw`]: those would be a second copy of
/// `crate::player::PhysicsState`, and
/// `lodestone_client::state::entity_view` requires all four, so their absence is
/// also what keeps the local player out of `ClientHandle::entities()` and
/// therefore off the render path — a self-model drawn at our own camera. That
/// exclusion is asserted explicitly in `lodestone-client` rather than left to
/// depend on which components happen to be missing.
///
/// A relogin re-indexes: the previous id's entry is dropped first, so a server
/// that assigns a different id on reconnect cannot leave a stale mapping
/// pointing at us.
pub fn apply_local_player_login(
    batch: Res<IngestBatch>,
    mut index: ResMut<EntityIndex>,
    locals: Query<(Entity, Option<&MinecraftEntityId>), With<LocalPlayer>>,
    mut commands: Commands,
) {
    for event in batch.events() {
        let ClientEvent::Login { entity_id, .. } = event else {
            continue;
        };
        for (entity, previous) in &locals {
            if let Some(previous) = previous
                && previous.0 != *entity_id
            {
                index.remove(previous.0);
            }
            index.insert(*entity_id, entity);
            // `Attributes::default()` is an empty list, i.e. "the server has not
            // reported any attribute yet" — the same state a fresh spawn gets.
            // Re-inserting on a relogin is deliberate: last session's attributes
            // are not this session's.
            commands
                .entity(entity)
                .insert((MinecraftEntityId(*entity_id), Attributes::default()));
        }
    }
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
/// matching the old `HashMap::insert` and covering a server reusing an id — with
/// one exception: an id currently held by the **local player** is ignored
/// entirely. Since [`apply_local_player_login`] indexes our own id, the "replace
/// the previous holder" branch would otherwise `despawn` the local player entity,
/// taking `PhysicsState`, the HUD components and `Sim.local`'s identity with it —
/// every `expect("the local player always carries …")` in the driver panics one
/// frame later. Vanilla never sends an `AddEntity` for the local player, so this
/// costs nothing and the failure it prevents is total.
pub fn apply_entity_spawn(
    batch: Res<IngestBatch>,
    mut index: ResMut<EntityIndex>,
    locals: Query<(), With<LocalPlayer>>,
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
        if index
            .get(*entity_id)
            .is_some_and(|held| locals.contains(held))
        {
            continue;
        }
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
///
/// The **local player** is exempt, for the same reason [`apply_entity_spawn`] is:
/// our own id is in the index since [`apply_local_player_login`], and a
/// `remove_entities` naming it would despawn the entity the whole driver hangs
/// off. Both the index entry and the entity survive — the id stays resolvable,
/// because we are still that entity.
pub fn apply_entity_removal(
    batch: Res<IngestBatch>,
    mut index: ResMut<EntityIndex>,
    locals: Query<(), With<LocalPlayer>>,
    mut commands: Commands,
) {
    for event in batch.events() {
        let ClientEvent::EntityRemoved { entity_ids } = event else {
            continue;
        };
        for entity_id in entity_ids {
            if index
                .get(*entity_id)
                .is_some_and(|held| locals.contains(held))
            {
                continue;
            }
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

/// `IngestSet::Apply`: `ClientEvent::EntityVelocity` → [`Velocity`] for a
/// remote entity, or a direct replace of the local player's own
/// [`PhysicsState`] velocity when the event names **our** id.
///
/// # Why the local player takes a different path — this is the knockback fix
///
/// Vanilla's `Entity.lerpMotion` (`Entity.java:2649-2651`,
/// `handleSetEntityMotion` at `ClientPacketListener.java:623-629`) is
/// `this.setDeltaMovement(movement)` — an unconditional **replace**, despite
/// the "lerp" name — and `LocalPlayer` declares no override, so a
/// `ClientboundSetEntityMotionPacket` naming our own id (server-applied
/// knockback, an explosion, elytra push, …) means "overwrite your own
/// velocity", the exact field [`crate::player::player_physics`] integrates
/// every `TickSet::Physics`.
///
/// Before this arm existed every `EntityVelocity` — including one naming us —
/// fell into the generic `Velocity` insert below. Nothing reads `Velocity` for
/// the local player (motion comes from `PhysicsState`, never that component),
/// so server-sent knockback was silently absorbed into a component the
/// physics pipeline never looks at: the client took a hit and never moved.
///
/// # No staging component needed
///
/// This module's own docs ("How events get in") record that `NetIngest` runs
/// synchronously on the net thread as each packet decodes, strictly before
/// the driver's next `GameTick` — so a plain overwrite here is picked up by
/// that tick's `player_physics` exactly once, matching vanilla's one-shot
/// `setDeltaMovement`, with nothing buffered in between.
pub fn apply_entity_velocity(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut commands: Commands,
    mut locals: Query<&mut PhysicsState, With<LocalPlayer>>,
) {
    for event in batch.events() {
        let ClientEvent::EntityVelocity {
            entity_id,
            velocity,
        } = event
        else {
            continue;
        };
        let Some(entity) = index.get(*entity_id) else {
            continue;
        };
        if let Ok(mut physics) = locals.get_mut(entity) {
            physics.0.velocity = Vec3d::new(velocity.x, velocity.y, velocity.z);
            continue;
        }
        // Inserts rather than assigns, because the component is absent until
        // the server has reported a velocity at all.
        commands.entity(entity).insert(Velocity(*velocity));
    }
}

/// Vanilla's `hurtDuration`/`hurtTime` reset value, in ticks —
/// `LivingEntity.animateHurt` (`LivingEntity.java:1873-1876`) and
/// `LivingEntity.handleDamageEvent` (`LivingEntity.java:2044-2049`) both write
/// `hurtDuration = 10; hurtTime = hurtDuration;`.
const HURT_DURATION_TICKS: u32 = 10;

/// `IngestSet::Apply`: `ClientEvent::EntityDamaged` → [`HurtTime`].
///
/// Mirrors `LivingEntity.handleDamageEvent`'s countdown reset (see
/// [`HURT_DURATION_TICKS`]). The damage-type/cause/direct/source-position
/// fields the event also carries have no consumer here — this system's whole
/// job is starting the hurt-flash countdown a render layer would fade over,
/// which is `entities.rs`'s to add (out of this crate's scope; see
/// `docs/combat.md`).
pub fn apply_entity_damaged(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut commands: Commands,
) {
    for event in batch.events() {
        let ClientEvent::EntityDamaged { entity_id, .. } = event else {
            continue;
        };
        if let Some(entity) = index.get(*entity_id) {
            commands.entity(entity).insert(HurtTime(HURT_DURATION_TICKS));
        }
    }
}

/// `IngestSet::Apply`: `ClientEvent::EntityHurtAnimation` → [`HurtTime`].
///
/// The same countdown reset as [`apply_entity_damaged`] —
/// `LivingEntity.animateHurt` writes the identical two fields. The packet's
/// `yaw` is not carried into the component: vanilla's own override accepts
/// the parameter and does not store it (`LivingEntity.java:1873`), so there is
/// nothing to lose by not carrying it further here.
pub fn apply_entity_hurt_animation(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut commands: Commands,
) {
    for event in batch.events() {
        let ClientEvent::EntityHurtAnimation { entity_id, .. } = event else {
            continue;
        };
        if let Some(entity) = index.get(*entity_id) {
            commands.entity(entity).insert(HurtTime(HURT_DURATION_TICKS));
        }
    }
}

/// `TickSet::Animate`: age every entity's [`HurtTime`] toward zero, one tick
/// at a time — the same rate `LivingEntity.tick()` decrements vanilla's
/// `hurtTime` field. Runs over every entity that carries the component, local
/// player included, with no `With<LocalPlayer>` filter needed either way.
pub fn tick_hurt_time(mut entities: Query<&mut HurtTime>) {
    for mut hurt in &mut entities {
        hurt.0 = hurt.0.saturating_sub(1);
    }
}

/// `IngestSet::Apply`: `ClientEvent::EntityAnimation` → [`AttackSwing`].
///
/// **Only `AnimationAction::SwingMainHand` starts a swing.** The other four
/// named actions are deliberately not handled here, each for a different
/// reason (`ClientPacketListener.handleAnimate`, `.cache/mc/26.2/client-src`):
///
/// | action | vanilla does | why not here |
/// |---|---|---|
/// | `SwingOffHand` | `mob.swing(OFF_HAND)` | animates the **left** arm; `lodestone-render`'s `attack_anim` assumes the right arm is attacking (it does not decode a mob's main hand) and neither render consumer draws a swinging left arm, so a main-hand swing is the only one that reaches a pixel — the same reason `sim.rs`'s local-player swing ignores an off-hand `SwingArm` |
/// | `WakeUp` | `player.stopSleepInBed(false, false)` | not an animation at all; no sleep-pose rendering exists to leave a bed from |
/// | `CriticalHit` / `MagicCriticalHit` | spawns a tracked particle emitter | a particle burst, not a swing; this crate has no particle system to hand it to |
///
/// `AnimationAction::Other(_)` (an id this table does not name) is likewise
/// ignored. The duration is [`lodestone_entity::pose::swing_duration`] with
/// **no** effect inputs, for the identical reason `Sim::swing_hand` (the local
/// player's own swing, `lodestone-shell::sim`) has none: no per-entity
/// mob-effect state is reachable yet (`docs/arm-swing-animation.md`'s
/// "Configuration" section).
pub fn apply_entity_animation(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut swings: Query<&mut AttackSwing>,
    mut commands: Commands,
) {
    for event in batch.events() {
        let ClientEvent::EntityAnimation { entity_id, action } = event else {
            continue;
        };
        if *action != AnimationAction::SwingMainHand {
            continue;
        }
        let Some(entity) = index.get(*entity_id) else {
            continue;
        };
        let duration = lodestone_entity::pose::swing_duration(
            lodestone_entity::pose::DEFAULT_SWING_DURATION,
            None,
            None,
        );
        if let Ok(mut swing) = swings.get_mut(entity) {
            swing.start_swing(duration);
        } else {
            let mut swing = AttackSwing::default();
            swing.start_swing(duration);
            commands.entity(entity).insert(swing);
        }
    }
}

/// `TickSet::Animate`: advance every entity's [`AttackSwing`] one tick, the
/// same rate [`crate::entity::AttackSwing::tick`] models
/// `LivingEntity.updateSwingTime` at. Runs over every entity that carries the
/// component; a remote entity gains one only once [`apply_entity_animation`]
/// has seen its first `SwingMainHand` report, exactly like [`tick_hurt_time`]
/// and [`HurtTime`].
pub fn tick_entity_swing(mut entities: Query<&mut AttackSwing>) {
    for mut swing in &mut entities {
        swing.tick();
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

/// `IngestSet::Apply`: `ClientEvent::EntityMetadataUpdated` → the local
/// player's own [`crate::session::Vitals::air`], when the event names our id.
///
/// # Why this is not a third arm inside [`apply_entity_metadata`]
///
/// That system writes the *generic* per-entity component set (any tracked
/// entity — a drowning zombie's air supply is metadata too), and has no
/// [`crate::session::Vitals`] to write into: `Vitals` lives on the **session**
/// entity, folded by [`crate::session::apply_local_player_state`] off
/// `set_health` for the other three fields. Air supply is the one HUD vital
/// that does *not* arrive on `set_health` — it is metadata — so it needs this
/// second, session-scoped fold off the same event family instead.
///
/// # "Is this us"
///
/// Resolves the same way [`apply_entity_velocity`] does for its local-player
/// fork: look the event's id up in [`EntityIndex`], then check the resolved
/// entity carries [`LocalPlayer`]. A `Query` miss (a real mob's metadata, or
/// an id metadata arrived for before its `Vitals`-bearing session entity
/// exists) is silently skipped, matching every other id-addressed system here.
pub fn apply_local_player_air_supply(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut locals: Query<&mut crate::session::Vitals, With<LocalPlayer>>,
) {
    for event in batch.events() {
        let ClientEvent::EntityMetadataUpdated {
            entity_id,
            metadata,
        } = event
        else {
            continue;
        };
        let Some(air) = metadata.air_supply else {
            continue;
        };
        let Some(entity) = index.get(*entity_id) else {
            continue;
        };
        if let Ok(mut vitals) = locals.get_mut(entity) {
            vitals.air = Some(air);
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

/// Despawn every ingest-side entity and forget it, for a session teardown.
///
/// # The hole this closes
///
/// [`EntityIndex`] is populated by [`apply_local_player_login`] and
/// [`apply_entity_spawn`], and until now nothing ever cleared it on a session
/// end. A rejoin's server assigns an entirely fresh set of ids, so no
/// `EntityRemoved` for the previous session's entities ever arrives — they
/// were never despawned, stayed indexed under ids nothing would ever
/// reference again, and kept being enumerated: `SharedState::entities`
/// (`lodestone-client/src/state.rs`) walks [`EntityIndex`] directly to derive
/// its `EntityView`s, so every stale entity kept reaching the render fold and
/// drawing — frozen, since nothing addressed by its dead id could ever move
/// it again — right alongside the live duplicate the new session spawned
/// under its own id for the same mob. This is the render-side twin of
/// [`crate::player::reset_local_player`]: same reset-on-teardown shape, same
/// module the state it clears is defined in.
///
/// # The local player is exempt
///
/// Same reason [`apply_entity_spawn`] and [`apply_entity_removal`] exempt it:
/// the local player's `Entity` id is held by the driver (`Sim.local`) across
/// the whole reset, not just this call, and despawning it would take
/// `PhysicsState`, the HUD components and every session component with it —
/// exactly the panic `Sim::end_session`'s own local-player reset exists to
/// avoid. [`EntityIndex`] is still cleared **entirely**, including whatever
/// entry currently maps the local player's own id: that mapping is stale the
/// instant the session ends, and [`apply_local_player_login`] re-adds it from
/// scratch — by querying `With<LocalPlayer>`, not by reading the index — the
/// next time we log in. Nothing needs to resolve our own id in the gap
/// between sessions.
pub fn reset_ingest_entities(world: &mut World) {
    let tracked: Vec<Entity> = world
        .resource::<EntityIndex>()
        .iter()
        .map(|(_, entity)| entity)
        .collect();
    for entity in tracked {
        if world.get::<LocalPlayer>(entity).is_some() {
            continue;
        }
        if let Ok(entity_mut) = world.get_entity_mut(entity) {
            entity_mut.despawn();
        }
    }
    world.resource_mut::<EntityIndex>().clear();
}

/// Registers the [`IngestQueue`] → [`IngestBatch`] hand-off: the two resources
/// and the single [`drain_ingest_queue`] system in [`IngestSet::Drain`].
///
/// Its own plugin, and both [`IngestPlugin`] and [`crate::SessionPlugin`] add it
/// through `is_plugin_added`, because **`drain_ingest_queue` must be registered
/// exactly once per `World`.** `add_systems` does not deduplicate: a second copy
/// runs after the first, clears the batch it just filled and appends a
/// now-empty queue, so every `Apply` system sees zero events. That is a silent,
/// total ingest blackout, and it is invisible to a test that installs only one
/// of the two plugins — which is how it was found (the session unit tests passed
/// while `new_ingest_handle`, the shape production actually uses, folded
/// nothing).
#[derive(Debug, Default)]
pub struct IngestQueuePlugin;

impl Plugin for IngestQueuePlugin {
    fn build(&self, app: &mut App) {
        if !app.is_plugin_added::<crate::CorePlugin>() {
            app.add_plugins(crate::CorePlugin);
        }
        app.init_resource::<IngestQueue>();
        app.init_resource::<IngestBatch>();
        app.add_systems(NetIngest, drain_ingest_queue.in_set(IngestSet::Drain));
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
        if !app.is_plugin_added::<IngestQueuePlugin>() {
            app.add_plugins(IngestQueuePlugin);
        }
        app.init_resource::<EntityIndex>();
        app.add_systems(
            NetIngest,
            (
                // First in the chain, because `.chain()`'s sync point is what
                // applies its deferred `Commands` before the id-addressed systems
                // below run — a `Login` and an `update_attributes` for our own id
                // in one batch must still resolve. Same mechanism as the
                // spawn-then-move test.
                apply_local_player_login,
                apply_entity_spawn,
                apply_entity_removal,
                apply_entity_movement,
                apply_entity_velocity,
                apply_entity_head_rotation,
                apply_entity_metadata,
                // Reads the *same* `EntityMetadataUpdated` batch `apply_entity_metadata`
                // just walked, folding the local player's own air supply into `Vitals`
                // (a different component, on a different entity, than the generic
                // per-entity set above — see the system's own doc). Order relative to
                // `apply_entity_metadata` does not matter (disjoint components), but it
                // is placed right after it so the two stay visibly paired.
                apply_local_player_air_supply,
                apply_entity_attributes,
                apply_entity_equipment,
                apply_entity_damaged,
                apply_entity_hurt_animation,
                apply_entity_animation,
            )
                .chain()
                .in_set(IngestSet::Apply),
        );
        // `tick_hurt_time`/`tick_entity_swing` live in `GameTick`/`TickSet::Animate`,
        // not `NetIngest` — they age [`HurtTime`]/[`AttackSwing`] once per simulated
        // tick regardless of how many (or how few) packets arrived that tick, the
        // same way `SessionHudPlugin::tick_hud_overlays` ages its own countdowns.
        // `IngestQueuePlugin` (added above) already guarantees `CorePlugin` is
        // present, which is what configures `TickSet::Animate` into the schedule at
        // all.
        app.add_systems(
            GameTick,
            (tick_hurt_time, tick_entity_swing).in_set(TickSet::Animate),
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

    /// The same, plus the one [`LocalPlayer`] entity every real `World` has —
    /// `SharedState::default`'s session entity, or the driver's `Sim.local`.
    fn ingest_world_with_local_player() -> (World, bevy_ecs::entity::Entity) {
        let mut world = ingest_world();
        let local = world.spawn(LocalPlayer).id();
        (world, local)
    }

    fn login_event(entity_id: i32) -> ClientEvent {
        ClientEvent::Login {
            entity_id,
            game_mode: lodestone_model::GameMode::Creative,
            dimension: "minecraft:overworld".parse().expect("valid dimension id"),
        }
    }

    fn attributes_event(entity_id: i32, base: f64) -> ClientEvent {
        ClientEvent::EntityAttributesUpdated {
            entity_id,
            attributes: vec![lodestone_model::EntityAttributeSnapshot {
                attribute: "minecraft:water_movement_efficiency"
                    .parse()
                    .expect("valid attribute id"),
                base,
                modifiers: Vec::new(),
            }],
        }
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
            entity_for(&world, 1)
                .get::<CustomName>()
                .map(|n| n.0.clone()),
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
            entity_for(&world, 1)
                .get::<CustomName>()
                .map(|n| n.0.clone()),
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
            entity_for(&world, 1)
                .get::<CustomName>()
                .map(|n| n.0.clone()),
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

    // ---- combat: knockback and the hurt-flash countdown -------------------

    /// Issue #12's knockback half. `ClientEvent::EntityVelocity` naming the
    /// **local player's own** id must overwrite `PhysicsState.velocity`
    /// directly — vanilla's `Entity.lerpMotion` is
    /// `this.setDeltaMovement(movement)`, an unconditional replace, and
    /// `LocalPlayer` declares no override — rather than falling into the
    /// generic [`Velocity`] component the rest of this test file already pins
    /// (`a_spawn_without_a_velocity_leaves_the_component_absent`), which
    /// nothing reads for the local player.
    #[test]
    fn entity_velocity_naming_the_local_player_replaces_physics_state_velocity() {
        let (mut world, local) = ingest_world_with_local_player();
        world
            .entity_mut(local)
            .insert(PhysicsState(lodestone_physics::PlayerState::at(
                Vec3d::ZERO,
                0.0,
            )));
        feed(&mut world, login_event(3));
        feed(
            &mut world,
            ClientEvent::EntityVelocity {
                entity_id: 3,
                velocity: Vec3::new(1.0, 2.0, -3.0),
            },
        );
        assert_eq!(
            world.get::<PhysicsState>(local).map(|p| p.0.velocity),
            Some(Vec3d::new(1.0, 2.0, -3.0)),
            "knockback naming our own id must land in PhysicsState.velocity"
        );
        assert!(
            world.get::<Velocity>(local).is_none(),
            "the local player must not also get the generic `Velocity` \
             component — nothing reads it for the local player, and it would \
             be a second, wrong source of truth"
        );
    }

    /// Metadata naming our own id folds `air_supply` into the session
    /// entity's [`crate::session::Vitals::air`] — the wiring
    /// [`apply_local_player_air_supply`] exists for.
    #[test]
    fn entity_metadata_naming_the_local_player_folds_air_into_vitals() {
        let (mut world, local) = ingest_world_with_local_player();
        world.entity_mut(local).insert(crate::session::Vitals::default());
        feed(&mut world, login_event(3));

        assert_eq!(
            world.get::<crate::session::Vitals>(local).unwrap().air,
            None,
            "unreported until the first metadata update naming us"
        );

        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    air_supply: Some(247),
                    ..EntityMetadataUpdate::default()
                },
                3,
            ),
        );
        assert_eq!(
            world.get::<crate::session::Vitals>(local).unwrap().air,
            Some(247),
        );
    }

    /// **Control.** Air-supply metadata for a *different* (remote) entity must
    /// not leak into the local player's `Vitals` — proving the "is this us"
    /// resolution actually discriminates, not just that the happy path works.
    #[test]
    fn entity_metadata_for_a_remote_entity_does_not_touch_local_vitals() {
        let (mut world, local) = ingest_world_with_local_player();
        world.entity_mut(local).insert(crate::session::Vitals::default());
        feed(&mut world, login_event(3));
        feed(&mut world, spawn_event(9, "minecraft:zombie"));

        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    air_supply: Some(11),
                    ..EntityMetadataUpdate::default()
                },
                9,
            ),
        );
        assert_eq!(
            world.get::<crate::session::Vitals>(local).unwrap().air,
            None,
            "a zombie's own air supply must not be mistaken for ours"
        );
    }

    /// Both hurt reports reset the same countdown to the same value —
    /// `LivingEntity.handleDamageEvent` and `LivingEntity.animateHurt` write
    /// the identical pair of fields in vanilla.
    #[test]
    fn entity_damaged_and_hurt_animation_both_start_the_hurt_countdown() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(1, "minecraft:pig"));
        assert!(
            entity_for(&world, 1).get::<HurtTime>().is_none(),
            "absent until the first report, like Health"
        );

        feed(
            &mut world,
            ClientEvent::EntityDamaged {
                entity_id: 1,
                damage_type_id: 0,
                cause_id: None,
                direct_id: None,
                source_pos: None,
            },
        );
        assert_eq!(entity_for(&world, 1).get::<HurtTime>().map(|h| h.0), Some(10));

        feed(&mut world, spawn_event(2, "minecraft:pig"));
        feed(
            &mut world,
            ClientEvent::EntityHurtAnimation {
                entity_id: 2,
                yaw: 45.0,
            },
        );
        assert_eq!(
            entity_for(&world, 2).get::<HurtTime>().map(|h| h.0),
            Some(10),
            "EntityHurtAnimation resets the same countdown EntityDamaged does"
        );
    }

    /// [`tick_hurt_time`] ages the countdown by exactly one per `GameTick`,
    /// saturating at zero rather than wrapping — a `GameTick` run with no new
    /// hurt report must not resurrect an expired countdown.
    #[test]
    fn tick_hurt_time_ages_the_countdown_to_zero_and_no_further() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(1, "minecraft:pig"));
        feed(
            &mut world,
            ClientEvent::EntityDamaged {
                entity_id: 1,
                damage_type_id: 0,
                cause_id: None,
                direct_id: None,
                source_pos: None,
            },
        );
        let entity = entity_for(&world, 1).id();
        for expected in (0..10).rev() {
            world.run_schedule(GameTick);
            assert_eq!(world.get::<HurtTime>(entity).map(|h| h.0), Some(expected));
        }
        // One more tick past zero must not underflow.
        world.run_schedule(GameTick);
        assert_eq!(world.get::<HurtTime>(entity).map(|h| h.0), Some(0));
    }

    /// The island this closes (issue #10): a `SwingMainHand` report reaches
    /// [`AttackSwing`] on the *ingest* entity, and [`tick_entity_swing`] then
    /// carries it through a full swing and back to rest — the same six-tick
    /// arc [`lodestone_entity::pose::EntityPose`] drives for the local player.
    #[test]
    fn swing_main_hand_starts_a_swing_that_ticks_to_completion_and_stops() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(1, "minecraft:pig"));
        assert!(
            entity_for(&world, 1).get::<AttackSwing>().is_none(),
            "absent until the first SwingMainHand report, like HurtTime"
        );

        feed(
            &mut world,
            ClientEvent::EntityAnimation {
                entity_id: 1,
                action: AnimationAction::SwingMainHand,
            },
        );
        let entity = entity_for(&world, 1).id();
        assert!(
            world.get::<AttackSwing>(entity).is_some(),
            "a SwingMainHand report must insert AttackSwing"
        );

        // `DEFAULT_SWING_DURATION` is 6 ticks: `attack_anim` climbs
        // `0/6, 1/6, .., 5/6` and then the sixth tick resets `swing_time` to 0
        // and clears `swinging`, landing back at `attack_anim == 0.0` — the
        // same sawtooth `docs/arm-swing-animation.md` documents.
        let expected = [0.0_f32, 1.0 / 6.0, 2.0 / 6.0, 3.0 / 6.0, 4.0 / 6.0, 5.0 / 6.0, 0.0];
        for want in expected {
            world.run_schedule(GameTick);
            let got = world
                .get::<AttackSwing>(entity)
                .expect("still tracked")
                .attack_anim;
            assert!(
                (got - want).abs() < 1.0e-6,
                "attack_anim was {got}, wanted {want}"
            );
        }
        // One more tick with no new report must not resurrect the swing.
        world.run_schedule(GameTick);
        assert_eq!(world.get::<AttackSwing>(entity).map(|s| s.attack_anim), Some(0.0));
    }

    /// The negative control for the action-id filter documented on
    /// [`apply_entity_animation`]: every action byte other than
    /// `SwingMainHand` — including `SwingOffHand`, which vanilla *does* run
    /// through `LivingEntity.swing` — must leave [`AttackSwing`] absent,
    /// proving the filter actually runs rather than every action starting a
    /// swing by accident.
    #[test]
    fn only_swing_main_hand_starts_a_swing() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(1, "minecraft:pig"));
        for action in [
            AnimationAction::SwingOffHand,
            AnimationAction::WakeUp,
            AnimationAction::CriticalHit,
            AnimationAction::MagicCriticalHit,
            AnimationAction::Other(200),
        ] {
            feed(
                &mut world,
                ClientEvent::EntityAnimation {
                    entity_id: 1,
                    action,
                },
            );
            assert!(
                entity_for(&world, 1).get::<AttackSwing>().is_none(),
                "{action:?} must not start a swing"
            );
        }
    }

    /// [`AttackSwing::start_swing`] swallows a restart before the half-way
    /// point, exactly like [`lodestone_entity::pose::EntityPose::start_swing`]
    /// — the mechanism that turns a held mine's every-tick `SwingMainHand`
    /// report into one continuous arc rather than a stutter, per
    /// `docs/arm-swing-animation.md`.
    #[test]
    fn a_restart_before_the_half_way_point_is_swallowed() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(1, "minecraft:pig"));
        feed(
            &mut world,
            ClientEvent::EntityAnimation {
                entity_id: 1,
                action: AnimationAction::SwingMainHand,
            },
        );
        let entity = entity_for(&world, 1).id();
        world.run_schedule(GameTick); // swing_time: -1 -> 0, attack_anim = 0/6

        // A restart this early (well before the 3-tick half-way point of a
        // 6-tick swing) must be swallowed rather than snapping back to -1: it
        // must land exactly where an *un*-restarted swing would after the same
        // two ticks, not one tick behind.
        feed(
            &mut world,
            ClientEvent::EntityAnimation {
                entity_id: 1,
                action: AnimationAction::SwingMainHand,
            },
        );
        world.run_schedule(GameTick); // swing_time: 0 -> 1, attack_anim = 1/6
        let got = world.get::<AttackSwing>(entity).expect("tracked").attack_anim;
        // The discriminating value: a `start_swing` that did *not* swallow the
        // restart would reset `swing_time` to `-1` on the second call, and the
        // following tick would land back at `attack_anim == 0.0` instead.
        assert!(
            (got - 1.0 / 6.0).abs() < 1.0e-6,
            "a restart before the half-way point must not rewind the arc, got {got}"
        );
    }

    // ---- spawn / move / despawn ------------------------------------------

    #[test]
    fn a_spawn_writes_the_reported_pose_and_indexes_the_id() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(7, "minecraft:pig"));
        let entity = entity_for(&world, 7);
        assert_eq!(
            entity.get::<Position>().map(|p| p.0),
            Some(Vec3::new(1.0, 64.0, 2.0))
        );
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
            ClientEvent::EntityRemoved {
                entity_ids: vec![7],
            },
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
        assert_eq!(
            equipment.len(),
            2,
            "the second slot must merge, not replace: {equipment:?}"
        );
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

    // ---- the local player -------------------------------------------------

    #[test]
    fn login_indexes_the_local_player_so_its_own_attributes_fold() {
        // The seam this closes: vanilla sends no `AddEntity` for yourself, so
        // `EntityIndex` never had our own id and `apply_entity_attributes`
        // `continue`d past every `update_attributes` naming it. Depth Strider's
        // `water_movement_efficiency` is the attribute that made this visible.
        let (mut world, local) = ingest_world_with_local_player();
        feed(&mut world, login_event(7));
        assert_eq!(
            world.resource::<EntityIndex>().get(7),
            Some(local),
            "our own id must resolve to the local player entity"
        );
        assert_eq!(
            world.get::<MinecraftEntityId>(local).map(|id| id.0),
            Some(7)
        );

        feed(&mut world, attributes_event(7, 0.5));
        let attributes = world
            .get::<Attributes>(local)
            .expect("login inserts an empty Attributes")
            .0
            .clone();
        assert_eq!(attributes.len(), 1);
        assert!((attributes[0].base - 0.5).abs() < 1.0e-9);
    }

    #[test]
    fn without_the_login_the_local_players_attributes_are_dropped_on_the_floor() {
        // The control, and it is the *pre-fix behaviour* verbatim: same event, same
        // id, same entity — only the `Login` is missing. Without this,
        // the test above cannot distinguish "the login fold works" from "attribute
        // ingest would have found the local player anyway".
        let (mut world, local) = ingest_world_with_local_player();
        feed(&mut world, attributes_event(7, 0.5));
        assert!(
            world.resource::<EntityIndex>().get(7).is_none(),
            "nothing but Login can index the local player"
        );
        assert!(
            world.get::<Attributes>(local).is_none(),
            "an unindexed local player gets no Attributes component at all"
        );
    }

    #[test]
    fn a_relogin_under_a_new_id_drops_the_old_mapping() {
        let (mut world, local) = ingest_world_with_local_player();
        feed(&mut world, login_event(7));
        feed(&mut world, login_event(9));
        assert_eq!(world.resource::<EntityIndex>().get(9), Some(local));
        assert!(
            world.resource::<EntityIndex>().get(7).is_none(),
            "a stale id must not keep resolving to us — a mob could inherit it"
        );
        assert_eq!(world.resource::<EntityIndex>().len(), 1);
    }

    #[test]
    fn a_spawn_or_removal_naming_our_own_id_never_despawns_the_local_player() {
        // Indexing our own id put the local player inside reach of the two systems
        // that `despawn` by index. If either fired, `PhysicsState`, the HUD
        // component set and the driver's `Sim.local` identity would all vanish
        // mid-session and every `expect("the local player always carries …")`
        // would panic a frame later. Vanilla sends neither for the local player,
        // which is exactly why nothing else would catch it.
        let (mut world, local) = ingest_world_with_local_player();
        feed(&mut world, login_event(7));

        feed(&mut world, spawn_event(7, "minecraft:pig"));
        assert!(
            world.get_entity(local).is_ok(),
            "a spawn must not despawn us"
        );
        assert_eq!(world.resource::<EntityIndex>().get(7), Some(local));

        feed(
            &mut world,
            ClientEvent::EntityRemoved {
                entity_ids: vec![7],
            },
        );
        assert!(
            world.get_entity(local).is_ok(),
            "a removal must not despawn us"
        );
        assert_eq!(
            world.resource::<EntityIndex>().get(7),
            Some(local),
            "…and the id must stay resolvable, because we are still that entity"
        );
    }

    #[test]
    fn the_same_guard_still_replaces_a_reused_id_for_an_ordinary_entity() {
        // The control for the guard above: it must key on `LocalPlayer`, not
        // blanket-disable the replace/despawn paths that
        // `a_respawned_id_replaces_the_previous_entity` and
        // `a_removal_despawns_and_deindexes` depend on.
        let (mut world, _local) = ingest_world_with_local_player();
        feed(&mut world, login_event(7));
        feed(&mut world, spawn_event(11, "minecraft:pig"));
        let pig = world.resource::<EntityIndex>().get(11).expect("indexed");
        feed(&mut world, spawn_event(11, "minecraft:item"));
        assert!(
            world.get_entity(pig).is_err(),
            "an ordinary reused id still replaces its previous holder"
        );
        feed(
            &mut world,
            ClientEvent::EntityRemoved {
                entity_ids: vec![11],
            },
        );
        assert!(world.resource::<EntityIndex>().get(11).is_none());
    }

    // ---- session teardown (rejoin duplicates entities) --------------------
    //
    // The live bug: quitting and rejoining left every previous session's
    // ingest-side entity indexed under an id nothing would ever reference
    // again — nothing cleared `EntityIndex` on a session end. `SharedState::
    // entities` (`lodestone-client/src/state.rs`) enumerates `EntityIndex`
    // directly to derive its `EntityView`s, so the stale entity kept reaching
    // the render fold: it drew, frozen (no event could ever move it again,
    // since the new server hands out different ids), right beside the live
    // duplicate the new session spawned for the same mob under its new id.
    //
    // Both ids below are deliberately different session-to-session — a real
    // rejoin never reuses an id, and `apply_entity_spawn`'s existing
    // "replace a reused id" branch would silently mask this bug if the test
    // reused one.

    #[test]
    fn without_a_reset_a_rejoin_leaves_the_previous_sessions_mob_indexed_and_frozen() {
        // The control: the pre-fix behaviour verbatim — two sessions, no call
        // to `reset_ingest_entities` in between. If this did not fail, the
        // fix test below would prove nothing.
        let (mut world, _local) = ingest_world_with_local_player();

        // Session 1: log in under id 7, a mob spawns under id 11.
        feed(&mut world, login_event(7));
        feed(&mut world, spawn_event(11, "minecraft:pig"));
        let session_one_pig = world.resource::<EntityIndex>().get(11).expect("indexed");

        // Session ends — no `EntityRemoved` for id 11 ever arrives, because a
        // real disconnect just drops the socket; nothing sends one.

        // Session 2: a fresh login under a different id, and the same logical
        // mob reappears under a different id too, exactly as vanilla assigns
        // ids per-connection.
        feed(&mut world, login_event(20));
        feed(&mut world, spawn_event(31, "minecraft:pig"));

        assert!(
            world.get_entity(session_one_pig).is_ok(),
            "the previous session's mob was never despawned — this is the duplicate"
        );
        assert!(
            world.resource::<EntityIndex>().get(11).is_some(),
            "…and it is still indexed, still enumerable by SharedState::entities"
        );
        assert_eq!(
            world.resource::<EntityIndex>().len(),
            3,
            "the old pig, the new pig, and the local player — one mob drawn twice"
        );
    }

    #[test]
    fn reset_ingest_entities_clears_the_previous_sessions_mob_across_a_rejoin() {
        let (mut world, local) = ingest_world_with_local_player();

        feed(&mut world, login_event(7));
        feed(&mut world, spawn_event(11, "minecraft:pig"));
        let session_one_pig = world.resource::<EntityIndex>().get(11).expect("indexed");

        // The fix under test, at the point `Sim::end_session` now calls it.
        reset_ingest_entities(&mut world);

        feed(&mut world, login_event(20));
        feed(&mut world, spawn_event(31, "minecraft:pig"));

        assert!(
            world.get_entity(session_one_pig).is_err(),
            "the previous session's mob must be despawned, not merely deindexed"
        );
        assert!(
            world.resource::<EntityIndex>().get(11).is_none(),
            "its id must not still resolve"
        );
        assert_eq!(
            world.resource::<EntityIndex>().len(),
            2,
            "exactly the second session's local player and its one mob — no duplicate"
        );
        assert_eq!(
            world.resource::<EntityIndex>().get(20),
            Some(local),
            "the local player entity itself survives the reset and re-indexes under its new id"
        );
    }

    #[test]
    fn reset_ingest_entities_never_despawns_the_local_player() {
        // A blanket "despawn everything EntityIndex points at" would take the
        // local player with it — `PhysicsState`, the HUD components and
        // `Sim.local`'s identity all vanish, and per `sim.rs`'s own comment a
        // missing component there means "someone despawned the local player,
        // which is a bug". This is the guard proving that never happens.
        let (mut world, local) = ingest_world_with_local_player();
        feed(&mut world, login_event(7));
        feed(&mut world, spawn_event(11, "minecraft:pig"));

        reset_ingest_entities(&mut world);

        assert!(
            world.get_entity(local).is_ok(),
            "the local player entity must survive a session reset"
        );
        assert!(
            world.get::<LocalPlayer>(local).is_some(),
            "…still carrying its marker"
        );
        assert!(
            world.resource::<EntityIndex>().is_empty(),
            "the index is cleared entirely, including the now-stale local-player entry — \
             apply_local_player_login re-adds it by querying With<LocalPlayer>, not by \
             reading the index, so clearing it costs nothing"
        );

        // The driver-visible proof: a relogin re-indexes cleanly under a new
        // id, exactly as if this were the very first login.
        feed(&mut world, login_event(99));
        assert_eq!(world.resource::<EntityIndex>().get(99), Some(local));
        assert_eq!(world.resource::<EntityIndex>().len(), 1);
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
        assert!(handles_event(&login_event(1)));
        // `EntityDamaged`/`EntityHurtAnimation` were decoded islands before
        // this fix — real `ClientEvent`s with no `matches!` arm here, so
        // `SharedState::apply` routed them into the dead legacy `Inner::apply`
        // fallback instead of `NetIngest` and `apply_entity_damaged`/
        // `apply_entity_hurt_animation` never ran in production regardless of
        // what a hermetic `feed()`-based test showed (that helper bypasses
        // this exact gate). This is the control that would have caught it.
        assert!(handles_event(&ClientEvent::EntityDamaged {
            entity_id: 1,
            damage_type_id: 0,
            cause_id: None,
            direct_id: None,
            source_pos: None,
        }));
        assert!(handles_event(&ClientEvent::EntityHurtAnimation {
            entity_id: 1,
            yaw: 0.0,
        }));
        // `EntityAnimation` was the identical shape of island a third time
        // (issue #10 / `docs/arm-swing-animation.md`): decoded, unit-tested at
        // the protocol layer, and reachable from a hermetic `feed()` call, but
        // absent from this `matches!` — so `SharedState::apply` never routed it
        // into `NetIngest` and `apply_entity_animation` never ran in production.
        assert!(handles_event(&ClientEvent::EntityAnimation {
            entity_id: 1,
            action: AnimationAction::SwingMainHand,
        }));
        assert!(!handles_event(&ClientEvent::TimeChanged {
            world_age: 1,
            time_of_day: 2,
        }));
        // Claimed by `crate::session`, not here: this module has no system for it.
        assert!(!handles_event(&ClientEvent::HealthChanged {
            health: 20.0,
            food: 20,
            saturation: 5.0,
        }));
        assert!(
            crate::session::handles_event(&ClientEvent::HealthChanged {
                health: 20.0,
                food: 20,
                saturation: 5.0,
            }),
            "…and something must claim it, or it falls through to the scalar fold \
             that no longer has an arm for it and is silently dropped"
        );
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
