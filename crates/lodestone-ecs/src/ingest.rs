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
    ArmorStandFlags, ArmorStandPose, Attributes, AttackSwing, Baby, CreeperSwellDir, CustomName,
    CustomNameVisible, DeathTime, DisplayBackgroundColor, DisplayBillboard, DisplayBlockState,
    DisplayBrightness, DisplayItem, DisplayItemContext, DisplayLeftRotation, DisplayLineWidth,
    DisplayRightRotation, DisplayScale, DisplayStyleFlags, DisplayText, DisplayTextOpacity,
    DisplayTranslation,
    EntityFlags, EntityIndex, EntityKind, EntityUuid, Equipment, ExperienceOrbValue,
    FallingBlockState, HeadYaw, Health, HurtTime, ItemFrameRotation, Leashed, MinecraftEntityId,
    PaintingVariant,
    MobState, OnGround,
    Passengers, Pose, Position, Rotation, Tamed, Variant, Vehicle, Velocity,
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
/// the ECS and everything else to its remaining scalar fold.
///
/// # This used to be the list, and that was the bug
///
/// It was a `matches!` over the variants the systems below handle — and because
/// `ClientEvent` is `#[non_exhaustive]`, a `matches!` in *this* crate can never be
/// exhaustive, so a new variant simply returned `false` here and `false` in
/// `crate::session::handles_event` and reached nothing. Four separate islands were
/// found that way (`EntityDamaged`/`EntityHurtAnimation`, air supply,
/// `DimensionTypeChanged`, `AbilitiesChanged`), each with a correct decode and a
/// green hermetic test.
///
/// The list now lives in [`lodestone_model::event::route`], next to the enum,
/// where the match **is** exhaustive and a new variant is a compile error until it
/// is routed. This function is one line so the predicate and the table cannot
/// drift apart.
///
/// # What it still does not prove
///
/// That a *claimed* event has a system behind it. `SharedState::apply` forwards on
/// this predicate, and a forwarded event no system folds is dropped just as
/// silently as an unrouted one. That half is
/// `tests::handles_event_covers_exactly_the_variants_with_a_system`'s job — the
/// table proves the decision was made, the coverage test proves the system exists.
#[must_use]
pub fn handles_event(event: &ClientEvent) -> bool {
    lodestone_model::event::route(event).ingest
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
/// Vanilla's `Entity.lerpMotion`
/// (`ClientPacketListener.handleSetEntityMotion` calls it) is
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
/// `LivingEntity.animateHurt` and
/// `LivingEntity.handleDamageEvent` both write
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
/// the parameter and does not store it, so there is
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

/// `IngestSet::Apply`: `ClientEvent::FallingBlockState` → [`FallingBlockState`].
///
/// The one thing a client is ever told about which block a `minecraft:falling_block`
/// is imitating. `FallingBlockEntity.defineSynchedData` registers `DATA_START_POS`
/// and nothing else, so the state arrives only in the spawn packet's Object Data
/// field and only once — see that event's own doc.
///
/// Id-addressed through [`EntityIndex`] like every other system in this set, so it
/// relies on the same `.chain()` sync point that lets a packet in the *same* batch
/// as the entity's `AddEntity` still resolve it. The adapter emits the two in that
/// order, so this always finds the entity.
///
/// A `None` from `index.get` is silently skipped rather than logged, matching
/// [`apply_entity_hurt_animation`]: an event for an entity we never spawned is a
/// packet for something out of view, not an error.
pub fn apply_falling_block_state(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut commands: Commands,
) {
    for event in batch.events() {
        let ClientEvent::FallingBlockState {
            entity_id,
            block_state_id,
        } = event
        else {
            continue;
        };
        if let Some(entity) = index.get(*entity_id) {
            commands
                .entity(entity)
                .insert(FallingBlockState(*block_state_id));
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

/// Vanilla's `EntityEvent.DEATH` (`EntityEvent.java`), the per-entity status byte a
/// server broadcasts from `LivingEntity.die()`.
const ENTITY_STATUS_DEATH: u8 = 3;

/// `IngestSet::Apply`: `ClientEvent::EntityStatus` → [`DeathTime`].
///
/// `EntityStatus` carries Mojang's raw per-entity-type event byte and was routed
/// **nowhere** before this system existed — decoded, tested, and consumed by
/// nothing. This claims exactly one of its ~40 codes; the rest are still
/// unhandled, and deliberately fall through rather than being logged, because most
/// are particle and sound effects with no subsystem here to receive them.
///
/// # Only byte 3, and only as an insert
///
/// `LivingEntity.handleEntityEvent`'s `case 3` plays the death sound and, for a
/// non-player, does `setHealth(0); die();`. `die()` is what makes
/// `isDeadOrDying()` true, which is what lets `tickDeath()` start incrementing —
/// so on this side the whole of that chain is "the entity now has a
/// [`DeathTime`]", and [`tick_death_time`] is the `tickDeath` half.
///
/// **One documented divergence.** Vanilla's `case 3` guards the kill with
/// `!(this instanceof Player)`, so a *remote player* falls over off their synced
/// health reaching zero rather than off this byte, one tick later than a mob
/// would. This does not reproduce that split: the server broadcasts byte 3 for
/// every `LivingEntity.die()` including players, so reacting to it uniformly
/// costs a sub-tick head start on a remote player's fall-over and saves a second
/// trigger path keyed on health that would have to agree with this one about when
/// death began.
///
/// Re-inserting on a repeat byte 3 would restart the animation, so the insert is
/// guarded on absence — a server that re-sends the byte (or a death that arrives
/// in two batches) must not snap a half-fallen mob back upright.
pub fn apply_entity_status(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    dying: Query<&DeathTime>,
    mut commands: Commands,
) {
    for event in batch.events() {
        let ClientEvent::EntityStatus { entity_id, status } = event else {
            continue;
        };
        if *status != ENTITY_STATUS_DEATH {
            continue;
        }
        // A `None` from `index.get` is silently skipped for
        // `apply_falling_block_state`'s reason: a status for an entity we never
        // spawned is a packet for something out of view, not an error.
        if let Some(entity) = index.get(*entity_id)
            && dying.get(entity).is_err()
        {
            commands.entity(entity).insert(DeathTime(0));
        }
    }
}

/// `TickSet::Animate`: age every dying entity's [`DeathTime`] **up**, one tick at a
/// time — `LivingEntity.tickDeath`'s `deathTime++`.
///
/// The opposite direction from [`tick_hurt_time`], and paired with it in the same
/// set for that reason: a mob's killing blow starts both counters, one running out
/// as the other runs up, and vanilla's red overlay is the *disjunction*
/// (`hurtTime > 0 || deathTime > 0`) precisely so the tint does not lapse in the
/// ten-tick gap between them.
///
/// Only entities that carry the component are touched, so this is a no-op for
/// everything alive — the component's absence is the "not dying" state, not a
/// zero.
pub fn tick_death_time(mut entities: Query<&mut DeathTime>) {
    for mut death in &mut entities {
        death.0 = death.0.saturating_add(1);
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

/// `IngestSet::Apply`: `ClientEvent::EntityPassengersChanged` → [`Passengers`]
/// on the vehicle and [`Vehicle`] on each rider.
///
/// # This was a total island
///
/// `SET_PASSENGERS` decoded correctly in
/// `lodestone_v770::adapter::V770Adapter::handle_play_entity` and was round-tripped by
/// `crates/protocol/v770/tests/entity_events.rs`, and a tree-wide grep for
/// `EntityPassengersChanged` found **four** hits: the decode, its two tests, and
/// the `ClientEvent` variant itself. Zero consumers, and — the usual
/// factory — no arm in [`handles_event`], so `SharedState::apply` never routed
/// it here at all. Adding the system without that arm would have reproduced the
/// island exactly, which is why the arm and this function landed together.
///
/// # The packet is absolute, so the fold must clear as well as set
///
/// `ClientboundSetPassengersPacket` carries the vehicle's **complete** rider
/// list, and a dismount is announced as that list going empty rather than as a
/// separate event. So every fold does three things in order:
///
/// 1. read the vehicle's *previous* [`Passengers`], and remove [`Vehicle`] from
///    every id in it that the new list does not contain — this is the only place
///    a dismount is observable;
/// 2. write the new list onto the vehicle;
/// 3. insert `Vehicle(vehicle_id)` on every id in the new list.
///
/// Skipping step 1 is what would strand a dismounted player: their `Vehicle`
/// would still name a boat they are no longer in, and
/// [`crate::player::player_physics`] would keep pinning them to its seat forever
/// with no packet left that could ever free them.
///
/// # A passenger the client has not spawned is kept, not dropped
///
/// `Passengers` stores raw server ids precisely so this system never has to
/// resolve one. `Vehicle` *is* a component and does need an [`EntityIndex`]
/// lookup — an id with no entity yet simply gets no `Vehicle` this batch. That
/// asymmetry is deliberate and it is safe in the direction that matters: the
/// forward list (which the camera and the seat maths read through the vehicle) is
/// always complete, and the reverse edge is a convenience that re-arrives with
/// the next `SET_PASSENGERS` the server sends on any seat change. It is also
/// self-healing for the local player, whose entity always exists by the time any
/// vehicle can seat them.
pub fn apply_entity_passengers(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut passengers: Query<&mut Passengers>,
    mut commands: Commands,
) {
    for event in batch.events() {
        let ClientEvent::EntityPassengersChanged {
            vehicle_id,
            passenger_ids,
        } = event
        else {
            continue;
        };
        let Some(vehicle) = index.get(*vehicle_id) else {
            continue;
        };
        // Step 1: whoever *was* aboard and is not on the new list has dismounted.
        // Read before the write below, so the comparison is against the real
        // previous state and not against what we are about to store.
        let departed: Vec<i32> = passengers
            .get(vehicle)
            .map(|previous| {
                previous
                    .0
                    .iter()
                    .copied()
                    .filter(|id| !passenger_ids.contains(id))
                    .collect()
            })
            .unwrap_or_default();
        for id in departed {
            if let Some(entity) = index.get(id) {
                commands.entity(entity).remove::<Vehicle>();
            }
        }
        // Step 2: the vehicle's list, replaced wholesale.
        if let Ok(mut current) = passengers.get_mut(vehicle) {
            current.0.clear();
            current.0.extend_from_slice(passenger_ids);
        } else {
            commands
                .entity(vehicle)
                .insert(Passengers(passenger_ids.clone()));
        }
        // Step 3: the reverse edge, for the riders the client can resolve.
        for id in passenger_ids {
            if let Some(entity) = index.get(*id) {
                commands.entity(entity).insert(Vehicle(*vehicle_id));
            }
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

/// `IngestSet::Apply`: the living-entity flags byte of
/// `ClientEvent::EntityMetadataUpdated` → [`ItemUse`].
///
/// # Why this is not another arm inside [`apply_entity_metadata`]
///
/// That system writes each field with `Commands::insert`, which *replaces* the
/// component. [`ItemUse`] carries a tick counter that must survive a repeated
/// metadata packet — see [`crate::entity::ItemUse::apply_flags`] for why a
/// server re-sending the same byte is the common case and why resetting on it
/// would pin every bow at un-drawn. So this needs read-modify-write against the
/// existing component, i.e. a `Query`, which is the same shape
/// [`apply_entity_animation`] uses for [`AttackSwing`] and for the same reason.
///
/// # `living_flags` is `None` on a non-living entity *by design*
///
/// The byte's metadata index is shared with a non-living entity's own flags byte
/// of the same serializer, so the version adapter withholds it unless it can
/// establish the entity is living. Nothing here has to re-check that: a `None`
/// means "not known to be living flags" and this system simply does not fold it.
pub fn apply_entity_item_use(
    batch: Res<IngestBatch>,
    index: Res<EntityIndex>,
    mut uses: Query<&mut crate::entity::ItemUse>,
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
        let Some(flags) = metadata.living_flags else {
            continue;
        };
        let Some(entity) = index.get(*entity_id) else {
            continue;
        };
        let decoded = lodestone_entity::metadata::LivingEntityFlags::from_bits(flags);
        let using = decoded.using_item();
        let off_hand = decoded.used_hand() == lodestone_entity::metadata::UsedHand::Off;
        if let Ok(mut item_use) = uses.get_mut(entity) {
            item_use.apply_flags(using, off_hand);
        } else {
            let mut item_use = crate::entity::ItemUse::default();
            item_use.apply_flags(using, off_hand);
            commands.entity(entity).insert(item_use);
        }
    }
}

/// `TickSet::Animate`: advance every entity's [`ItemUse`] one tick.
///
/// This is the client-side counter vanilla's own client keeps, because
/// `useItemRemaining` is not a synced field — see [`crate::entity::ItemUse`]. It
/// sits in `Animate` beside [`tick_entity_swing`] rather than in a physics set
/// because it drives a pose and nothing else.
pub fn tick_entity_item_use(mut entities: Query<&mut crate::entity::ItemUse>) {
    for mut item_use in &mut entities {
        item_use.tick();
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
        // `TamableAnimal.DATA_FLAGS_ID & 4`. Per-entity state —
        // there can be several tamed wolves at once, each independently tame
        // or not — so this belongs beside `Baby`/`Health` in `ingest`, not in
        // `crate::session`, which carries only the local player's own scalars.
        // See [`Tamed`]'s own doc for why "absent" still means "wild" for a
        // mob that was already tame when it entered view range.
        if let Some(tamed) = metadata.tamed {
            entity.insert(Tamed(tamed));
        }
        // The creeper fuse direction (`Creeper.DATA_SWELL_DIR`), the last hop of
        // the chain `docs/entity-rendering.md`'s "Creeper swell" section left
        // for this crate: `lodestone-shell::entities`' `CreeperFuse`/
        // `tick_creeper_fuse`/white-flash-overlay path is fully wired and reads
        // only `EntitySnapshot::creeper_swell_dir`, downstream of
        // `CreeperSwellDir` via `lodestone_client::state::entity_view`.
        if let Some(dir) = metadata.creeper_swell_dir {
            entity.insert(CreeperSwellDir(dir));
        }
        // An experience orb's XP value (`ExperienceOrb.DATA_VALUE`). This arm is
        // the reason an orb draws at all: the server has streamed the field since
        // the orb entity landed, and with no fold here it reached
        // `EntityMetadataUpdate` and stopped — a decoded field with no component,
        // which is the metadata-shaped version of an island.
        //
        // **`apply_entity_metadata`, not a session fold.** An orb's value is
        // per-*entity* state (there can be a hundred on the ground, each worth a
        // different amount), so it belongs in this system beside `Health`/`Baby`;
        // `crate::session` carries the *local player's* own scalars, and the XP
        // bar's level/progress — which is the local-player half of the same
        // feature — really does go there, off `set_experience` rather than off
        // metadata. Putting this one in `session` would compile and never run.
        if let Some(value) = metadata.experience_orb_value {
            entity.insert(ExperienceOrbValue(value));
        }
        // The eight-step rotation of the stack in an item frame
        // (`ItemFrame.DATA_ROTATION`). Per-*entity* state, so this system and
        // not `crate::session` — the fork that has cost work twice: an arm in
        // the wrong router compiles, its unit test passes, and it never runs.
        if let Some(rotation) = metadata.item_frame_rotation {
            entity.insert(ItemFrameRotation(rotation));
        }
        // Which painting is hung (`Painting.DATA_PAINTING_VARIANT_ID`).
        // Per-entity state, so this router and not `crate::session`. Cloned
        // rather than copied because the key is an owned identifier, and
        // `insert`'s replace semantics are right: a painting's variant can be
        // reassigned in place by a plugin.
        if let Some(ref variant) = metadata.painting_variant {
            entity.insert(PaintingVariant(variant.clone()));
        }
        // The *mob* flags byte — a different byte at a different
        // index from the living-entity one [`apply_entity_item_use`] folds, and
        // the one that actually makes a mob hold a weapon pose. It belongs in this
        // system rather than beside `ItemUse` because it is a plain latched
        // boolean with no counter, so `insert`'s replace-the-component semantics
        // are exactly right. Present only for entities the adapter established are
        // `Mob`s: an armour stand's index-15 byte means "show arms".
        if let Some(bits) = metadata.mob_flags {
            let flags = lodestone_entity::metadata::MobFlags::from_bits(bits);
            entity.insert(MobState {
                aggressive: flags.aggressive(),
                left_handed: flags.left_handed(),
            });
        }
        // The *armour stand* client-flags byte — the other claimant of the same
        // wire index `MobState` folds above, present only for entities the
        // adapter established are `ArmorStand`s (never both on the same
        // entity). This is the last hop the "hologram" chain needs before the
        // shell's own draw call site can read it: a decorative stand's
        // marker/no-base-plate/show-arms cosmetics were decoded end to end and
        // dropped on the floor here until this arm existed, exactly as `Tamed`
        // was before its own arm was added.
        if let Some(bits) = metadata.armor_stand_flags {
            let decoded = lodestone_entity::metadata::ArmorStandFlags::from_bits(bits);
            entity.insert(ArmorStandFlags {
                small: decoded.small(),
                show_arms: decoded.show_arms(),
                no_base_plate: decoded.no_base_plate(),
                marker: decoded.marker(),
            });
        }
        // The armour stand's six `ROTATIONS` pose accessors (indices 16-21),
        // the other half of what a decorative stand carries alongside the
        // client-flags byte above. Unlike every insert around it this one
        // **merges**: a metadata packet mentions only the accessors that
        // changed, so an update nudging one arm must leave the other five parts
        // where they were, exactly as vanilla's per-accessor `SynchedEntityData`
        // does. The base for a stand seen for the first time is vanilla's own
        // `defineId` default, not zeroes — see `ArmorStandPose`'s doc for why
        // that distinction is load bearing and why a *missing* component still
        // means "apply the default pose" rather than "apply nothing".
        let pose_update = metadata.armor_stand_pose;
        if !pose_update.is_empty() {
            // `entry`, not `insert`: this is the one fold in this system that
            // **merges** rather than replaces. A metadata packet mentions only
            // the accessors that changed, so an update nudging one arm must
            // leave the other five parts where they were — exactly vanilla's
            // per-accessor `SynchedEntityData` semantics.
            //
            // It has to be an entry rather than a read-then-insert because
            // `Commands` is deferred: a `Query` read here would see the
            // *pre-batch* pose, so two updates to the same stand in one batch
            // would each merge onto the same stale base and the first would be
            // silently lost. `and_modify` runs when the command is applied, in
            // command order, so the second merge sees the first's result.
            //
            // The base for a stand seen for the first time is vanilla's own
            // `defineId` default, not zeroes — and a *missing* component still
            // means "apply the default pose" rather than "apply nothing"; see
            // `ArmorStandPose`'s own doc for why that distinction is what
            // actually stops a moving stand swinging its arms.
            entity
                .entry::<ArmorStandPose>()
                .or_default()
                .and_modify(move |mut pose| pose.0 = pose.0.merged(pose_update));
        }
        if let Some(variant) = &metadata.variant {
            entity.insert(Variant(variant.clone()));
        }
        if let Reported::Reported(item) = &metadata.item {
            entity.insert(DisplayItem(item.clone()));
        }
    }
}

/// `IngestSet::Apply`: `ClientEvent::EntityMetadataUpdated` → whichever of the
/// `Display`-family components (`text_display`/`item_display`/`block_display`)
/// the packet actually carried.
///
/// A separate system from [`apply_entity_metadata`] rather than ten more arms
/// inside it — that system is already the widest fold in this file, and every
/// field here is unique to one entity family (`Display`) with no other reader,
/// so splitting it out keeps both functions' `git blame` and diff surface
/// scoped to what they actually own. Reads the *same* `EntityMetadataUpdated`
/// batch, id-addressed through [`EntityIndex`] like every other system in this
/// module, so it relies on the same `.chain()` sync point that lets a metadata
/// packet in the *same* batch as the entity's own `AddEntity` still resolve.
///
/// Before this system existed, `lodestone_v770`'s decoder already produced
/// every one of these fields — [`crate`]'s own `EntityMetadataUpdated` event
/// carried them end to end — and nothing here read them: a decoded field with
/// no component, the metadata-shaped island this repo's evidence standards
/// call out. `lodestone_render::display`'s billboard/transform geometry has
/// had zero producers until this system and its protocol-layer half landed.
pub fn apply_display_metadata(batch: Res<IngestBatch>, index: Res<EntityIndex>, mut commands: Commands) {
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
        if let Some(billboard) = metadata.display_billboard {
            entity.insert(DisplayBillboard(billboard));
        }
        if let Some(translation) = metadata.display_translation {
            entity.insert(DisplayTranslation(translation));
        }
        if let Some(scale) = metadata.display_scale {
            entity.insert(DisplayScale(scale));
        }
        if let Some(rotation) = metadata.display_left_rotation {
            entity.insert(DisplayLeftRotation(rotation));
        }
        if let Some(rotation) = metadata.display_right_rotation {
            entity.insert(DisplayRightRotation(rotation));
        }
        // `Reported::Reported(_)` is the server speaking about the field —
        // including the theoretical `Reported(None)` case a version adapter
        // never actually produces for this field (see
        // `EntityMetadataUpdate::display_text`'s own doc) — matching
        // `custom_name`'s handling above rather than `Some(_)`-gating.
        if let Reported::Reported(text) = &metadata.display_text {
            entity.insert(DisplayText(text.clone().unwrap_or_default()));
        }
        if let Some(width) = metadata.display_line_width {
            entity.insert(DisplayLineWidth(width));
        }
        if let Some(color) = metadata.display_background_color {
            entity.insert(DisplayBackgroundColor(color));
        }
        if let Some(opacity) = metadata.display_text_opacity {
            entity.insert(DisplayTextOpacity(opacity));
        }
        if let Some(flags) = metadata.display_text_style_flags {
            entity.insert(DisplayStyleFlags(flags));
        }
        if let Some(state) = metadata.display_block_state {
            entity.insert(DisplayBlockState(state));
        }
        if let Some(context) = metadata.display_item_context {
            entity.insert(DisplayItemContext(context));
        }
        // Declared on the base `Display` class, so it folds for every subtype
        // rather than beside one variant's payload above. The `-1` sentinel is
        // carried through rather than folded to absence here: a consumer has to
        // be able to tell "explicitly no override" from "never reported", and
        // both a re-reported `-1` and a first report of `-1` are the server
        // clearing an override it previously set.
        if let Some(brightness) = metadata.display_brightness_override {
            entity.insert(DisplayBrightness(brightness));
        }
    }
}

/// `IngestSet::Apply`: `ClientEvent::EntityLeashed` → [`Leashed`].
/// A dedicated system rather than an arm inside
/// [`apply_entity_metadata`] immediately above: `EntityLeashed` decodes from
/// `SET_ENTITY_LINK`, a wholly different packet from the metadata family
/// that system's arms all share, so folding it there would blur two
/// unrelated wire packets into one system for no reason beyond proximity.
///
/// Per-entity state — there can be several leashed mobs at once, each
/// independently attached or not — so this belongs beside every other
/// per-entity fold in this file, not in `crate::session`, which carries only
/// the local player's own scalars. `ClientEvent::EntityLeashed` routes to
/// `INGEST` alone (`lodestone_model::event::route`), never to `session`, so
/// this is mechanically the *only* system in the tree that ever sees it.
pub fn apply_entity_leash(batch: Res<IngestBatch>, index: Res<EntityIndex>, mut commands: Commands) {
    for event in batch.events() {
        let ClientEvent::EntityLeashed {
            entity_id,
            holder_id,
        } = event
        else {
            continue;
        };
        let Some(entity) = index.get(*entity_id) else {
            continue;
        };
        commands.entity(entity).insert(Leashed(*holder_id));
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
/// `IngestSet::Apply`: `ClientEvent::EntityMetadataUpdated` → the local player's
/// own [`crate::session::Vitals::on_fire`], when the event names our
/// id.
///
/// The sibling of [`apply_local_player_air_supply`] below, and it exists for the
/// identical reason: the shared-flags byte arrives as **per-entity** metadata for
/// any entity, and `apply_entity_metadata` does fold it into a generic
/// `EntityFlags` — including on our own entity. But
/// `lodestone_client::state::entity_view` requires
/// `EntityKind`/`Position`/`Rotation`/`HeadYaw`, and
/// [`apply_local_player_login`] deliberately gives the local player none of
/// them, precisely so a self-model never reaches `ClientHandle::entities()` and
/// renders at the camera's own eye. `entity_view()`'s early `?` therefore returns
/// before `flags` is read, and no amount of correct generic folding can surface
/// it. A session-scoped fold is the only route.
///
/// Bit 0 is `Entity.FLAG_ONFIRE`, read through
/// [`lodestone_entity::metadata::SharedEntityFlags`] rather than by testing
/// `& 0x01` inline — the flags byte carries seven other meanings and an inline
/// mask is the kind of thing that gets copied to the wrong bit later.
pub fn apply_local_player_on_fire(
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
        let Some(flags) = metadata.flags else {
            continue;
        };
        let Some(entity) = index.get(*entity_id) else {
            continue;
        };
        if let Ok(mut vitals) = locals.get_mut(entity) {
            vitals.on_fire = Some(
                lodestone_entity::metadata::SharedEntityFlags::from_bits(flags as i8).on_fire(),
            );
        }
    }
}

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
        // `apply_vehicle_moved` resets it. Shared with
        // `crate::player::LocalPlayerPlugin`, which also inits it; `init_resource`
        // is idempotent, and installing both plugins in either order leaves a
        // populated resource alone.
        app.init_resource::<crate::vehicle::ControlledVehicle>();
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
                // A different packet from the metadata family above
                // (`SET_ENTITY_LINK`, not `SET_ENTITY_DATA`), but id-addressed the
                // same way, so it relies on the same `.chain()` sync point after
                // `apply_entity_spawn` — a mob can arrive and be leashed in one batch
                // (`EntityStreamer::sync`'s own spawn-time `SET_ENTITY_LINK` emission
                // for an already-leashed mob).
                apply_entity_leash,
                // Reads the *same* `EntityMetadataUpdated` batch `apply_entity_metadata`
                // just walked, folding the local player's own air supply into `Vitals`
                // (a different component, on a different entity, than the generic
                // per-entity set above — see the system's own doc). Order relative to
                // `apply_entity_metadata` does not matter (disjoint components), but it
                // is placed right after it so the two stay visibly paired.
                apply_local_player_air_supply,
                apply_local_player_on_fire,
                // Third reader of the same batch, folding the *living*-entity flags
                // byte into `ItemUse`. A separate system because it
                // read-modify-writes a tick counter rather than replacing a
                // component — see its own doc.
                apply_entity_item_use,
                apply_entity_attributes,
                apply_entity_equipment,
                apply_entity_damaged,
                apply_entity_hurt_animation,
                // The other half of the hurt/death pair above: `EntityDamaged` and
                // `EntityHurtAnimation` start the countdown that runs *out*, this
                // starts the one that runs *up*. Id-addressed like them, so it
                // relies on the same `.chain()` sync point after
                // `apply_entity_spawn` — a mob can be spawned and killed in one
                // batch.
                apply_entity_status,
                apply_entity_animation,
                // The falling block's imitated state. Id-addressed, so it depends on
                // the same `.chain()` sync point after `apply_entity_spawn` that
                // `apply_entity_passengers` below spells out — the adapter emits
                // `FallingBlockState` in the same batch as the entity's `AddEntity`.
                apply_falling_block_state,
                // Ordered after `apply_entity_spawn` by the same `.chain()` sync
                // point the id-addressed systems above rely on, which is what lets
                // a `SET_PASSENGERS` in the *same* batch as the vehicle's
                // `AddEntity` still resolve the vehicle through `EntityIndex`.
                apply_entity_passengers,
                // The server's *rejection* of a vehicle position we predicted.
                // Ordered after `apply_entity_passengers` by the same `.chain()`
                // sync point, because the subject is whichever vehicle
                // `session::Riding` names and a correction can share a batch with
                // the `SET_PASSENGERS` that seated us.
                crate::vehicle::apply_vehicle_moved,
            )
                .chain()
                .in_set(IngestSet::Apply),
        );
        // A **separate** `add_systems` call rather than a twenty-first slot in the
        // tuple above: it reads the same `EntityMetadataUpdated` batch
        // `apply_entity_metadata` does (disjoint components, so relative order
        // never matters, exactly like `apply_local_player_air_supply`'s own note
        // two systems up), and the tuple above is already at the arity
        // `IntoSystemConfigs` is generated for. Still `IngestSet::Apply`, so it
        // still runs inside the same `NetIngest` schedule pass.
        app.add_systems(NetIngest, apply_display_metadata.in_set(IngestSet::Apply));
        // `tick_hurt_time`/`tick_entity_swing` live in `GameTick`/`TickSet::Animate`,
        // not `NetIngest` — they age [`HurtTime`]/[`AttackSwing`] once per simulated
        // tick regardless of how many (or how few) packets arrived that tick, the
        // same way `SessionHudPlugin::tick_hud_overlays` ages its own countdowns.
        // `IngestQueuePlugin` (added above) already guarantees `CorePlugin` is
        // present, which is what configures `TickSet::Animate` into the schedule at
        // all.
        app.add_systems(
            GameTick,
            (
                tick_hurt_time,
                // Paired with `tick_hurt_time` rather than merely adjacent to it —
                // see its own doc for why the two counters run in opposite
                // directions and why their consumer is a disjunction.
                tick_death_time,
                tick_entity_swing,
                tick_entity_item_use,
            )
                .in_set(TickSet::Animate),
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
        EntityEquipment, EntityMetadataUpdate, EquipmentSlot, ItemComponents, ItemStack, Text,
        Vec3,
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
                    custom_name: Reported::Reported(Some(Text::literal("Lodestar"))),
                    ..EntityMetadataUpdate::default()
                },
                1,
            ),
        );
        assert_eq!(
            entity_for(&world, 1)
                .get::<CustomName>()
                .map(|n| n.0.clone()),
            Some(Some(Text::literal("Lodestar")))
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
            Some(Some(Text::literal("Lodestar"))),
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

    /// The knockback half of local-player velocity handling. `ClientEvent::EntityVelocity` naming the
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

    // ---- using-item state reaching a component ------------------------------

    /// **The routing check, and the reason this feature is not an island.**
    /// `SharedState::apply` only forwards events one of the two `handles_event`
    /// switches lists, so `apply_entity_item_use` can be correct, registered and
    /// unit-tested green while never running in production.
    /// `EntityMetadataUpdated` is already claimed — asserted here so a later
    /// narrowing of that switch fails *this* test rather than silently deleting
    /// the bow pose.
    #[test]
    fn the_metadata_event_carrying_living_flags_is_claimed_by_this_module() {
        let event = metadata(
            EntityMetadataUpdate {
                living_flags: Some(0x01),
                ..EntityMetadataUpdate::default()
            },
            7,
        );
        assert!(
            handles_event(&event),
            "living flags ride `EntityMetadataUpdated`; if this module stops \
             claiming it, `apply_entity_item_use` never runs in production"
        );
    }

    /// The same routing check for the **mob** flags byte. It rides
    /// the same `EntityMetadataUpdated` event, so no new arm was needed — which is
    /// exactly why it is asserted: "no change required" is the state in which a
    /// later narrowing of the switch silently deletes a feature, and the switch
    /// was checked *before* the fold was written rather than after.
    #[test]
    fn the_metadata_event_carrying_mob_flags_is_claimed_by_this_module() {
        let event = metadata(
            EntityMetadataUpdate {
                mob_flags: Some(0x04),
                ..EntityMetadataUpdate::default()
            },
            7,
        );
        assert!(
            handles_event(&event),
            "mob flags ride `EntityMetadataUpdated`; if this module stops claiming it, \
             `apply_entity_metadata` never folds `MobState` in production and every \
             skeleton stays in the rest pose"
        );
    }

    /// End-to-end through the **real schedule**: a spawn, then a metadata packet
    /// carrying the mob-flags byte, produces a [`crate::entity::MobState`].
    ///
    /// Driven by `run_schedule`, not by calling the system, so a fold that was
    /// written but never registered fails here.
    #[test]
    fn mob_flags_fold_into_mob_state() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(21, "minecraft:skeleton"));
        assert!(
            entity_for(&world, 21).get::<MobState>().is_none(),
            "absent until the first byte mentions it, like ItemUse and HurtTime"
        );

        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    mob_flags: Some(0x04),
                    ..EntityMetadataUpdate::default()
                },
                21,
            ),
        );
        assert_eq!(
            entity_for(&world, 21).get::<MobState>(),
            Some(&MobState {
                aggressive: true,
                left_handed: false
            })
        );

        // And it *latches down* as well as up: an attack goal that releases its
        // target clears the bit, and a skeleton left permanently drawing would be
        // as visible a defect as one that never draws.
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    mob_flags: Some(0x00),
                    ..EntityMetadataUpdate::default()
                },
                21,
            ),
        );
        assert_eq!(
            entity_for(&world, 21).get::<MobState>(),
            Some(&MobState {
                aggressive: false,
                left_handed: false
            })
        );

        // A metadata packet that does not mention the byte must leave the
        // component completely alone — metadata is incremental, and the common
        // case is a health-only update arriving mid-draw.
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    mob_flags: Some(0x04),
                    ..EntityMetadataUpdate::default()
                },
                21,
            ),
        );
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    health: Some(3.0),
                    ..EntityMetadataUpdate::default()
                },
                21,
            ),
        );
        assert_eq!(
            entity_for(&world, 21).get::<MobState>(),
            Some(&MobState {
                aggressive: true,
                left_handed: false
            }),
            "a health-only update cleared the aggressive latch — a skeleton would drop its \
             draw every time it took damage"
        );
    }

    /// `MobFlags::LEFT_HANDED` (bit `0x02`) folds into [`MobState::left_handed`]
    /// independently of [`MobState::aggressive`] (bit `0x04`) — the two bits are
    /// adjacent same-typed fields on the same byte, so a fixture that only ever
    /// sets them equal (both true or both false) cannot see a transposition
    /// between them. `0x02` sets *only* left-handed and `0x06` sets *both*,
    /// deliberately distinct from each other and from the all-aggressive /
    /// all-calm bytes [`mob_flags_fold_into_mob_state`] already covers.
    #[test]
    fn mob_flags_left_handed_bit_folds_independently_of_aggressive() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(23, "minecraft:skeleton"));

        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    mob_flags: Some(0x02),
                    ..EntityMetadataUpdate::default()
                },
                23,
            ),
        );
        assert_eq!(
            entity_for(&world, 23).get::<MobState>(),
            Some(&MobState {
                aggressive: false,
                left_handed: true
            }),
            "left-handed alone must not also set aggressive — a transposition of the two bits \
             would still pass a fixture that only ever sets them equal"
        );

        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    mob_flags: Some(0x06),
                    ..EntityMetadataUpdate::default()
                },
                23,
            ),
        );
        assert_eq!(
            entity_for(&world, 23).get::<MobState>(),
            Some(&MobState {
                aggressive: true,
                left_handed: true
            }),
            "an aggressive left-handed mob must fold both bits"
        );
    }

    /// The same routing check as
    /// [`the_metadata_event_carrying_mob_flags_is_claimed_by_this_module`], for
    /// the armour-stand client-flags byte — the *other* claimant of the same
    /// wire index. Rides the same `EntityMetadataUpdated` event, so no new
    /// `handles_event` arm was needed.
    #[test]
    fn the_metadata_event_carrying_armor_stand_flags_is_claimed_by_this_module() {
        let event = metadata(
            EntityMetadataUpdate {
                armor_stand_flags: Some(0x18),
                ..EntityMetadataUpdate::default()
            },
            7,
        );
        assert!(
            handles_event(&event),
            "armour-stand flags ride `EntityMetadataUpdated`; if this module stops \
             claiming it, `apply_entity_metadata` never folds `ArmorStandFlags` in \
             production and a 'hologram' stand keeps its base plate forever"
        );
    }

    /// End-to-end through the real schedule, mirroring [`mob_flags_fold_into_mob_state`]:
    /// a spawn, then a metadata packet carrying the armour-stand flags byte,
    /// produces a [`crate::entity::ArmorStandFlags`] — the last hop before the
    /// shell's own draw call site (out of this crate's scope) can read it.
    ///
    /// The value used is 0x18 (`marker | no_base_plate`), the typical
    /// "hologram" configuration: pairwise-distinct in the sense CLAUDE.md
    /// requires — only two of the four bits are set, not all-or-nothing — so a
    /// transposition with any neighbouring bit or with `MobState`'s own
    /// `aggressive` bit (0x04, deliberately absent here) cannot survive
    /// unnoticed.
    #[test]
    fn armor_stand_flags_fold_into_armor_stand_flags_component() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(22, "minecraft:armor_stand"));
        assert!(
            entity_for(&world, 22).get::<ArmorStandFlags>().is_none(),
            "absent until the first byte mentions it, like MobState and Baby"
        );

        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    armor_stand_flags: Some(0x18),
                    ..EntityMetadataUpdate::default()
                },
                22,
            ),
        );
        assert_eq!(
            entity_for(&world, 22).get::<ArmorStandFlags>(),
            Some(&ArmorStandFlags {
                small: false,
                show_arms: false,
                no_base_plate: true,
                marker: true,
            })
        );
        // And it must not have landed in `MobState` — the two share a wire byte
        // and nothing else.
        assert!(
            entity_for(&world, 22).get::<MobState>().is_none(),
            "an armour stand's own flags byte must never surface as MobState"
        );

        // A metadata packet that does not mention the byte must leave the
        // component completely alone — the same incremental-update rule
        // `mob_flags_fold_into_mob_state`'s health-only case checks.
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    health: Some(20.0),
                    ..EntityMetadataUpdate::default()
                },
                22,
            ),
        );
        assert_eq!(
            entity_for(&world, 22).get::<ArmorStandFlags>(),
            Some(&ArmorStandFlags {
                small: false,
                show_arms: false,
                no_base_plate: true,
                marker: true,
            }),
            "a health-only update cleared the flags — an unrelated field must not \
             touch this component"
        );
    }

    /// The armour-stand pose fold, and the property that makes it different
    /// from every other arm in `apply_entity_metadata`: it **merges**.
    ///
    /// A metadata packet mentions only the accessors that changed, so an update
    /// nudging one arm must leave the other five parts where they were —
    /// vanilla's per-accessor `SynchedEntityData` semantics. An arm written with
    /// `insert` would pass a single-packet test and silently reset five parts on
    /// the second packet, which is the realistic case (a builder's editor sends
    /// one part at a time).
    ///
    /// Every value here is pairwise distinct across all three packets, so no
    /// pair of parts can be exchanged without an assertion moving.
    #[test]
    fn armor_stand_pose_fields_merge_onto_the_vanilla_default() {
        use lodestone_model::{ArmorStandPose as Pose, ArmorStandPoseUpdate, Vec3f};

        let mut world = ingest_world();
        feed(&mut world, spawn_event(23, "minecraft:armor_stand"));
        assert!(
            entity_for(&world, 23).get::<ArmorStandPose>().is_none(),
            "absent until the first packet mentions a part — which does NOT mean the \
             stand has no pose: a draw site must apply the vanilla default in that case, \
             or an unposed stand keeps the humanoid walk cycle"
        );

        // First packet: one arm only.
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    armor_stand_pose: ArmorStandPoseUpdate {
                        left_arm: Some(Vec3f::new(31.0, 32.0, 33.0)),
                        ..ArmorStandPoseUpdate::default()
                    },
                    ..EntityMetadataUpdate::default()
                },
                23,
            ),
        );
        assert_eq!(
            entity_for(&world, 23).get::<ArmorStandPose>(),
            Some(&ArmorStandPose(Pose {
                left_arm: Vec3f::new(31.0, 32.0, 33.0),
                ..Pose::VANILLA_DEFAULT
            })),
            "the five unmentioned parts must take vanilla's own defineId defaults, \
             not zeroes"
        );

        // Second packet: a *different* part. The first must survive it.
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    armor_stand_pose: ArmorStandPoseUpdate {
                        right_leg: Some(Vec3f::new(-61.0, -62.0, -63.0)),
                        ..ArmorStandPoseUpdate::default()
                    },
                    ..EntityMetadataUpdate::default()
                },
                23,
            ),
        );
        assert_eq!(
            entity_for(&world, 23).get::<ArmorStandPose>(),
            Some(&ArmorStandPose(Pose {
                left_arm: Vec3f::new(31.0, 32.0, 33.0),
                right_leg: Vec3f::new(-61.0, -62.0, -63.0),
                ..Pose::VANILLA_DEFAULT
            })),
            "the second packet reset the first part — this fold merges, it does not replace"
        );

        // And an update mentioning no part at all leaves the pose alone.
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    health: Some(20.0),
                    ..EntityMetadataUpdate::default()
                },
                23,
            ),
        );
        assert_eq!(
            entity_for(&world, 23).get::<ArmorStandPose>(),
            Some(&ArmorStandPose(Pose {
                left_arm: Vec3f::new(31.0, 32.0, 33.0),
                right_leg: Vec3f::new(-61.0, -62.0, -63.0),
                ..Pose::VANILLA_DEFAULT
            })),
            "an unrelated field must not touch this component"
        );
    }

    /// The router check for the pose, in the shape this module already uses for
    /// every other metadata-borne fact: the six accessors ride
    /// `EntityMetadataUpdated`, and if this module ever stops claiming that
    /// event `apply_entity_metadata` never runs and every armour stand goes back
    /// to swinging its arms as it moves.
    #[test]
    fn the_metadata_event_carrying_an_armor_stand_pose_is_claimed_by_this_module() {
        let event = metadata(
            EntityMetadataUpdate {
                armor_stand_pose: lodestone_model::ArmorStandPoseUpdate {
                    head: Some(lodestone_model::Vec3f::new(11.0, 12.0, 13.0)),
                    ..lodestone_model::ArmorStandPoseUpdate::default()
                },
                ..EntityMetadataUpdate::default()
            },
            7,
        );
        assert!(
            handles_event(&event),
            "armour-stand poses ride `EntityMetadataUpdated`; if this module stops \
             claiming it, `apply_entity_metadata` never folds `ArmorStandPose` and \
             every stand animates as a walking humanoid"
        );
    }

    /// The same routing check as
    /// [`the_metadata_event_carrying_mob_flags_is_claimed_by_this_module`], for
    /// the creeper swell direction. Rides the same `EntityMetadataUpdated`
    /// event, so no new `handles_event` arm was needed — asserted anyway per
    /// CLAUDE.md's own router-trap warning: "no change required" is exactly
    /// the state in which a later narrowing of the switch silently deletes a
    /// feature.
    #[test]
    fn the_metadata_event_carrying_creeper_swell_dir_is_claimed_by_this_module() {
        let event = metadata(
            EntityMetadataUpdate {
                creeper_swell_dir: Some(1),
                ..EntityMetadataUpdate::default()
            },
            7,
        );
        assert!(
            handles_event(&event),
            "creeper_swell_dir rides `EntityMetadataUpdated`; if this module stops \
             claiming it, `apply_entity_metadata` never folds `CreeperSwellDir` in \
             production and every creeper stays motionless while its fuse burns"
        );
    }

    /// End-to-end through the **real schedule**: a spawn, then a metadata packet
    /// carrying the creeper swell direction, produces a
    /// [`crate::entity::CreeperSwellDir`] — the last hop
    /// `docs/entity-rendering.md`'s "Creeper swell" section left open.
    /// Mirrors [`mob_flags_fold_into_mob_state`] above: absent until first
    /// reported, updates on report, and a metadata packet silent about the
    /// field must leave the component alone.
    #[test]
    fn creeper_swell_dir_fold_into_creeper_swell_dir_component() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(21, "minecraft:creeper"));
        assert!(
            entity_for(&world, 21).get::<CreeperSwellDir>().is_none(),
            "absent until the first packet mentions it, like MobState and ItemUse"
        );

        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    creeper_swell_dir: Some(1),
                    ..EntityMetadataUpdate::default()
                },
                21,
            ),
        );
        assert_eq!(
            entity_for(&world, 21).get::<CreeperSwellDir>(),
            Some(&CreeperSwellDir(1)),
            "a positive swell direction must reach the component unchanged"
        );

        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    creeper_swell_dir: Some(-1),
                    ..EntityMetadataUpdate::default()
                },
                21,
            ),
        );
        assert_eq!(
            entity_for(&world, 21).get::<CreeperSwellDir>(),
            Some(&CreeperSwellDir(-1)),
            "backing off must overwrite the previous direction, not merge with it"
        );

        // A metadata packet silent about the field (a health-only update, the
        // common case) must leave the last-reported direction alone — the same
        // "incremental, not a fresh snapshot" contract every other optional
        // metadata field here has.
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    health: Some(20.0),
                    ..EntityMetadataUpdate::default()
                },
                21,
            ),
        );
        assert_eq!(
            entity_for(&world, 21).get::<CreeperSwellDir>(),
            Some(&CreeperSwellDir(-1)),
            "a health-only update cleared the swell direction — a creeper mid-fuse would \
             freeze on screen every time it took damage"
        );
    }

    /// End-to-end through the **real schedule**: a spawn, then a metadata packet
    /// carrying `tamed: Some(true)`, produces a [`Tamed`] component —
    /// the fold `crates/lodestone-render/src/entity.rs`'s
    /// `entity_variant_sheet_for` needed a caller for, and
    /// `lodestone-shell/src/entities.rs::extract_entity_draws` now bridges off
    /// this exact component the same way it bridges `Variant`.
    #[test]
    fn tamed_metadata_folds_into_tamed_component() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(31, "minecraft:wolf"));
        assert!(
            entity_for(&world, 31).get::<Tamed>().is_none(),
            "absent until the first packet mentions it, like Baby and MobState"
        );

        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    tamed: Some(true),
                    ..EntityMetadataUpdate::default()
                },
                31,
            ),
        );
        assert_eq!(
            entity_for(&world, 31).get::<Tamed>(),
            Some(&Tamed(true)),
            "a tame report must reach the component unchanged"
        );

        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    health: Some(20.0),
                    ..EntityMetadataUpdate::default()
                },
                31,
            ),
        );
        assert_eq!(
            entity_for(&world, 31).get::<Tamed>(),
            Some(&Tamed(true)),
            "a health-only update must not clear a previously reported tame state"
        );
    }

    /// End-to-end through the **real schedule**: `ClientEvent::EntityLeashed`
    /// (decoded from `SET_ENTITY_LINK`) folds into [`Leashed`],
    /// covering a fresh attach, a detach, and — the case `handles_event`
    /// alone cannot prove — that `EntityLeashed` is routed to `INGEST` and
    /// therefore reaches this exact system in production, not merely in a
    /// hermetic call to `apply_entity_leash` directly.
    #[test]
    fn entity_leashed_folds_into_leashed_component() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(41, "minecraft:wolf"));
        assert!(
            entity_for(&world, 41).get::<Leashed>().is_none(),
            "absent until the first SET_ENTITY_LINK, like Tamed and Baby"
        );

        feed(
            &mut world,
            ClientEvent::EntityLeashed {
                entity_id: 41,
                holder_id: Some(77),
            },
        );
        assert_eq!(
            entity_for(&world, 41).get::<Leashed>(),
            Some(&Leashed(Some(77))),
            "an attach must reach the component with the real holder id"
        );

        feed(
            &mut world,
            ClientEvent::EntityLeashed {
                entity_id: 41,
                holder_id: None,
            },
        );
        assert_eq!(
            entity_for(&world, 41).get::<Leashed>(),
            Some(&Leashed(None)),
            "a detach must overwrite the previous holder with None, not leave it stale"
        );
    }

    /// A mob that is spawned **already leashed** — the join-late case
    /// `EntityStreamer::sync`'s spawn-time `SET_ENTITY_LINK` emission exists
    /// for — must still fold, even though the attach happened before this
    /// client ever saw the entity. Exercises the same `.chain()` sync point
    /// `apply_entity_passengers`'s own doc names: a mob can arrive and be
    /// leashed in one batch.
    #[test]
    fn a_mob_leashed_in_the_same_batch_as_its_spawn_still_folds() {
        let mut world = ingest_world();
        world.resource_mut::<IngestQueue>().push(spawn_event(51, "minecraft:wolf"));
        world.resource_mut::<IngestQueue>().push(ClientEvent::EntityLeashed {
            entity_id: 51,
            holder_id: Some(1),
        });
        world.run_schedule(NetIngest);
        assert_eq!(
            entity_for(&world, 51).get::<Leashed>(),
            Some(&Leashed(Some(1))),
            "a spawn and its leash link in the same batch must both resolve — \
             this is the wire shape a client joining view of an already-leashed mob sees"
        );
    }

    /// End-to-end through the **real schedule**: a spawn, then a metadata packet
    /// with the using-item bit, produces an [`crate::entity::ItemUse`] whose
    /// counter then advances on `GameTick`.
    ///
    /// Deliberately driven by `run_schedule` rather than by calling the system, so
    /// a system that was written but never registered fails here.
    #[test]
    fn living_flags_fold_into_item_use_and_the_counter_advances() {
        use crate::entity::ItemUse;
        let mut world = ingest_world();
        feed(&mut world, spawn_event(11, "minecraft:skeleton"));
        assert!(
            entity_for(&world, 11).get::<ItemUse>().is_none(),
            "absent until the first byte mentions it"
        );

        // Using, main hand.
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    living_flags: Some(0x01),
                    ..EntityMetadataUpdate::default()
                },
                11,
            ),
        );
        let got = *entity_for(&world, 11).get::<ItemUse>().expect("folded");
        assert!(got.using, "the using-item bit must reach the component");
        assert!(!got.off_hand);
        assert_eq!(got.ticks, 0, "no ticks have run yet");

        for _ in 0..5 {
            world.run_schedule(GameTick);
        }
        assert_eq!(
            entity_for(&world, 11).get::<ItemUse>().unwrap().ticks,
            5,
            "`tick_entity_item_use` must be registered in `TickSet::Animate` — a \
             counter stuck at 0 is a bow that never draws"
        );

        // A **repeat** of the same byte must not restart the draw. This is the
        // failure mode that looks perfect at the wire level: the server re-sends
        // metadata freely, and a reset here pins every bow un-drawn forever.
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    living_flags: Some(0x01),
                    ..EntityMetadataUpdate::default()
                },
                11,
            ),
        );
        assert_eq!(
            entity_for(&world, 11).get::<ItemUse>().unwrap().ticks,
            5,
            "a repeated metadata byte is not a rising edge"
        );

        // Releasing clears both the flag and the counter.
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    living_flags: Some(0x00),
                    ..EntityMetadataUpdate::default()
                },
                11,
            ),
        );
        let released = *entity_for(&world, 11).get::<ItemUse>().unwrap();
        assert!(!released.using);
        assert_eq!(released.ticks, 0);
        // ...and the counter stays put while released, rather than counting up
        // from a stale `using`.
        world.run_schedule(GameTick);
        assert_eq!(entity_for(&world, 11).get::<ItemUse>().unwrap().ticks, 0);
    }

    /// A metadata update that carries **no** living flags leaves [`ItemUse`]
    /// alone — the control for the fold above. Without it, a system that
    /// unconditionally inserted a default `ItemUse` on every metadata packet
    /// would pass the test above and silently clear a bow draw whenever any
    /// other field changed.
    #[test]
    fn metadata_without_living_flags_does_not_touch_item_use() {
        use crate::entity::ItemUse;
        let mut world = ingest_world();
        feed(&mut world, spawn_event(12, "minecraft:skeleton"));
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    living_flags: Some(0x01),
                    ..EntityMetadataUpdate::default()
                },
                12,
            ),
        );
        world.run_schedule(GameTick);
        world.run_schedule(GameTick);
        assert_eq!(entity_for(&world, 12).get::<ItemUse>().unwrap().ticks, 2);

        // Health only — `living_flags: None`.
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    health: Some(12.0),
                    ..EntityMetadataUpdate::default()
                },
                12,
            ),
        );
        let after = *entity_for(&world, 12).get::<ItemUse>().unwrap();
        assert!(after.using, "an unrelated field must not end the use");
        assert_eq!(after.ticks, 2, "...nor rewind the draw");
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

    /// Metadata naming our own id folds the on-fire bit into
    /// [`crate::session::Vitals::on_fire`] — the wiring
    /// [`apply_local_player_on_fire`] exists for, and the reason it has to exist
    /// at all: the generic `EntityFlags` fold *does* run on our own entity, but
    /// `entity_view()` can never surface it because the local player has no
    /// `EntityKind`.
    ///
    /// `0x01` is `Entity.FLAG_ONFIRE`; `0x08` (sprinting) is fed first as a
    /// **discriminator** — a fold that tested "any flags present" rather than the
    /// specific bit would report burning for a sprinting player, which is a much
    /// worse bug than not showing the overlay at all.
    #[test]
    fn entity_metadata_naming_the_local_player_folds_the_on_fire_bit_into_vitals() {
        let (mut world, local) = ingest_world_with_local_player();
        world.entity_mut(local).insert(crate::session::Vitals::default());
        feed(&mut world, login_event(3));

        assert_eq!(
            world.get::<crate::session::Vitals>(local).unwrap().on_fire,
            None,
            "unreported until the first metadata update naming us"
        );

        // Sprinting only: the flags byte is non-zero but bit 0 is clear.
        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    flags: Some(0x08),
                    ..EntityMetadataUpdate::default()
                },
                3,
            ),
        );
        assert_eq!(
            world.get::<crate::session::Vitals>(local).unwrap().on_fire,
            Some(false),
            "a non-zero flags byte without bit 0 must not read as burning"
        );

        feed(
            &mut world,
            metadata(
                EntityMetadataUpdate {
                    flags: Some(0x01),
                    ..EntityMetadataUpdate::default()
                },
                3,
            ),
        );
        assert_eq!(
            world.get::<crate::session::Vitals>(local).unwrap().on_fire,
            Some(true),
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

    /// A falling block's imitated block state reaches its component, and is
    /// **absent** until the spawn packet reports it.
    ///
    /// The absence half is the load-bearing one: state id `0` is a real state
    /// (`minecraft:air`), so a default-`0` component could not be told apart from
    /// "a falling block made of air" and the renderer would have no switch. The
    /// second half feeds the spawn and the state in **one batch**, which is how
    /// they really arrive — one `ADD_ENTITY` emits both — so it exercises the
    /// `.chain()` sync point rather than a two-batch sequence that would pass even
    /// if this system ran before `apply_entity_spawn`.
    #[test]
    fn a_falling_blocks_object_data_becomes_its_block_state_component() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(1, "minecraft:falling_block"));
        assert!(
            entity_for(&world, 1).get::<FallingBlockState>().is_none(),
            "absent until the spawn packet's Object Data field is folded — absence \
             is the switch a renderer keys on, not a sentinel 0"
        );

        feed(
            &mut world,
            ClientEvent::FallingBlockState {
                entity_id: 1,
                block_state_id: 1234,
            },
        );
        assert_eq!(
            entity_for(&world, 1).get::<FallingBlockState>().map(|s| s.0),
            Some(1234),
            "the state id must reach the component, or every falling block draws \
             whatever state id 0 resolves to"
        );

        // In the same batch as the spawn, which is how it really arrives: the
        // adapter emits `EntitySpawned` then `FallingBlockState` from one
        // `ADD_ENTITY`. This is what the `.chain()` sync point after
        // `apply_entity_spawn` buys, and without it the id would not resolve.
        let mut world = ingest_world();
        world
            .resource_mut::<IngestQueue>()
            .push(spawn_event(2, "minecraft:falling_block"));
        world.resource_mut::<IngestQueue>().push(ClientEvent::FallingBlockState {
            entity_id: 2,
            block_state_id: 77,
        });
        world.run_schedule(NetIngest);
        assert_eq!(
            entity_for(&world, 2).get::<FallingBlockState>().map(|s| s.0),
            Some(77),
            "a spawn and its Object Data in one batch must both land"
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

    /// The whole death-counter chain: `EntityStatus` byte 3 →
    /// [`apply_entity_status`] → [`DeathTime`] → [`tick_death_time`], plus both
    /// halves of the routing claim.
    ///
    /// Live player report: *"stuff dying doesnt have the death animation (the one
    /// where they turn red and tilt on their side)"*. `EntityStatus` was decoded,
    /// round-tripped by the `v770` tests, and routed **nowhere** — sitting in
    /// `event.rs`'s "claimed by nothing" list — so no byte it carried reached any
    /// system. This is the island the routing assertions below exist to catch.
    ///
    /// Both halves of the routing decision are asserted, not just the one that had
    /// to change: `ingest` must claim it (or nothing runs however correct the fold
    /// is) **and** `session` must not (a fold in the wrong router compiles, tests
    /// green through `feed()`, and never runs in production). The fold writes
    /// `DeathTime` on the entity the status names, which is per-entity state with a
    /// single writer in this module and no session system holding a mutable query on
    /// it.
    #[test]
    fn entity_status_death_starts_a_death_counter_that_ticks_up() {
        // The router, first — a green fold behind a router that is never asked is
        // exactly the shape this repo pays for most.
        let death = ClientEvent::EntityStatus {
            entity_id: 1,
            status: 3,
        };
        assert!(
            handles_event(&death),
            "EntityStatus is not routed to ingest, so apply_entity_status can never \
             run in production however green this test's feed() calls are"
        );
        assert!(
            lodestone_model::event::route(&death).session,
            "the local player's status 24..28 is a disjoint session permission \
             fold; this event must continue to reach both consumers"
        );

        let mut world = ingest_world();
        feed(&mut world, spawn_event(1, "minecraft:pig"));
        assert!(
            entity_for(&world, 1).get::<DeathTime>().is_none(),
            "a living entity must carry no DeathTime at all — absence is the \
             'not dying' state, not a zero"
        );

        // A status byte that is *not* death must not start the counter. `2` is
        // `onKineticHit`, a real neighbouring code, rather than an invented one.
        feed(
            &mut world,
            ClientEvent::EntityStatus {
                entity_id: 1,
                status: 2,
            },
        );
        assert!(
            entity_for(&world, 1).get::<DeathTime>().is_none(),
            "only EntityEvent.DEATH (3) starts the death counter"
        );

        feed(&mut world, death);
        let entity = entity_for(&world, 1).id();
        assert_eq!(
            world.get::<DeathTime>(entity).map(|d| d.0),
            Some(0),
            "inserted at zero, not one: vanilla's deathTime is still 0 when die() \
             runs, and both its consumers test deathTime > 0, so the first tick of \
             death draws upright"
        );

        // Counts **up**, the opposite direction from `tick_hurt_time`.
        for expected in 1..=25 {
            world.run_schedule(GameTick);
            assert_eq!(
                world.get::<DeathTime>(entity).map(|d| d.0),
                Some(expected),
                "tick_death_time must increment once per GameTick"
            );
        }

        // A repeat byte 3 must not restart the animation — a server that re-sends
        // it, or a death arriving in two batches, would snap a half-fallen mob
        // upright.
        feed(
            &mut world,
            ClientEvent::EntityStatus {
                entity_id: 1,
                status: 3,
            },
        );
        assert_eq!(
            world.get::<DeathTime>(entity).map(|d| d.0),
            Some(25),
            "a second death byte must leave the running counter alone"
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

    /// The island this closes: a `SwingMainHand` report reaches
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
                    custom_name: Reported::Reported(Some(Text::literal("Lodestar"))),
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
        // (see `docs/arm-swing-animation.md`): decoded, unit-tested at
        // the protocol layer, and reachable from a hermetic `feed()` call, but
        // absent from this `matches!` — so `SharedState::apply` never routed it
        // into `NetIngest` and `apply_entity_animation` never ran in production.
        assert!(handles_event(&ClientEvent::EntityAnimation {
            entity_id: 1,
            action: AnimationAction::SwingMainHand,
        }));
        // `EntityPassengersChanged` was the fifth instance of the
        // identical island: decoded at `v770`'s `SET_PASSENGERS`, round-tripped by
        // `crates/protocol/v770/tests/entity_events.rs`, and a tree-wide grep for
        // the variant returned exactly **four** hits — the decode, those two tests,
        // and the `ClientEvent` declaration. No consumer anywhere and no arm here.
        assert!(handles_event(&ClientEvent::EntityPassengersChanged {
            vehicle_id: 1,
            passenger_ids: vec![2],
        }));
        // `FallingBlockState` is per-entity state and therefore **`ingest`, not
        // `session`** — the fork CLAUDE.md records as having cost work twice, and
        // getting it wrong here compiles, tests green through a direct
        // `apply_falling_block_state` call, and never runs in production. Its cost
        // if it did: a falling block draws whatever block state id `0` resolves to,
        // silently, since the spawn packet's Object Data field is the only channel
        // the state ever travels on.
        assert!(handles_event(&ClientEvent::FallingBlockState {
            entity_id: 1,
            block_state_id: 7,
        }));
        // `EntityLeashed` (decoded from `SET_ENTITY_LINK`) — per-entity like
        // `FallingBlockState` immediately above, and used to sit in
        // `lodestone_model::event::route`'s "claimed by nothing" block until
        // `apply_entity_leash` existed to claim it.
        assert!(handles_event(&ClientEvent::EntityLeashed {
            entity_id: 1,
            holder_id: Some(2),
        }));
        // And **both** routers claim it, which is the part that is easy to get
        // wrong: this side folds the per-entity `Passengers`/`Vehicle` pair,
        // `session` folds the local player's own `Riding` scalar off the same
        // event. An arm in only one of the two would leave whichever half it
        // missed as a fold that never fires — the fork `CLAUDE.md` records as
        // having cost work twice. This is the assertion that pins the answer
        // rather than the reasoning.
        assert!(crate::session::handles_event(
            &ClientEvent::EntityPassengersChanged {
                vehicle_id: 1,
                passenger_ids: vec![2],
            }
        ));
        // `VehicleMoved` carries **no entity id**, which is exactly why it reads
        // like a `session` scalar and is not one: what
        // `crate::vehicle::apply_vehicle_moved` writes is the vehicle's own
        // `Position`/`Rotation`, per-entity components this module owns the sole
        // writer of, with the subject supplied by `session::Riding`. It was an
        // island until the client became authoritative over the vehicle it rides —
        // the correction cannot fire until there is a prediction to correct.
        assert!(handles_event(&ClientEvent::VehicleMoved {
            pos: lodestone_model::Vec3 {
                x: 1.0,
                y: 2.0,
                z: 3.0
            },
            yaw: 0.0,
            pitch: 0.0,
        }));
        // …and `session` must **not** claim it, or two routers would fight over
        // one write. This is the negative half of the fork check above.
        assert!(!crate::session::handles_event(&ClientEvent::VehicleMoved {
            pos: lodestone_model::Vec3 {
                x: 1.0,
                y: 2.0,
                z: 3.0
            },
            yaw: 0.0,
            pitch: 0.0,
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
        // Same shape for `DimensionTypeChanged`: folded by
        // `crate::session::apply_local_player_state` into `ServerDimensionType`,
        // beside `ServerDimension` off the same packet — so *this* module must
        // not claim it, and `session` must.
        //
        // This pair is the routing check, and it is the check that has caught the
        // island three times: a decode can be perfect, the component and the
        // system can be correct, and the whole chain still reaches zero pixels
        // because `SharedState::apply` only forwards what one of these two
        // switches lists.
        let dimension_type_changed = ClientEvent::DimensionTypeChanged {
            holder_id: 0,
            dimension_type: None,
            is_flat: false,
        };
        assert!(!handles_event(&dimension_type_changed));
        assert!(
            crate::session::handles_event(&dimension_type_changed),
            "registry-driven dimension facts must reach a fold, or #288's decode \
             is an island"
        );
    }

    /// `SET_PASSENGERS` names the vehicle and lists its riders, and the fold must
    /// produce **both** edges: the list on the vehicle and the back-pointer on
    /// each rider.
    #[test]
    fn seating_a_passenger_writes_both_edges() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(10, "minecraft:oak_boat"));
        feed(&mut world, spawn_event(11, "minecraft:pig"));
        feed(
            &mut world,
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 10,
                passenger_ids: vec![11],
            },
        );
        let boat = world
            .resource::<EntityIndex>()
            .get(10)
            .expect("the boat is tracked");
        let pig = world
            .resource::<EntityIndex>()
            .get(11)
            .expect("the pig is tracked");
        assert_eq!(
            world.get::<Passengers>(boat).map(|p| p.0.clone()),
            Some(vec![11]),
            "the vehicle must carry the rider list"
        );
        assert_eq!(
            world.get::<Vehicle>(pig).copied(),
            Some(Vehicle(10)),
            "the rider must carry the reverse edge"
        );
    }

    /// **The dismount case, which is the one with no event of its own.** Vanilla
    /// announces a dismount as the same absolute packet with the rider gone, so
    /// the fold has to compare against the previous list and *remove* the reverse
    /// edge. Without that step a dismounted rider keeps naming a vehicle it is no
    /// longer in, and nothing later can ever free it.
    #[test]
    fn an_empty_passenger_list_clears_the_reverse_edge() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(10, "minecraft:oak_boat"));
        feed(&mut world, spawn_event(11, "minecraft:pig"));
        feed(
            &mut world,
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 10,
                passenger_ids: vec![11],
            },
        );
        let pig = world
            .resource::<EntityIndex>()
            .get(11)
            .expect("the pig is tracked");
        // The precondition, asserted rather than assumed: if the seat never
        // existed, the clear below would pass vacuously.
        assert!(
            world.get::<Vehicle>(pig).is_some(),
            "precondition: the pig must be aboard before the dismount is meaningful"
        );
        feed(
            &mut world,
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 10,
                passenger_ids: Vec::new(),
            },
        );
        assert_eq!(
            world.get::<Vehicle>(pig).copied(),
            None,
            "an empty list must remove the reverse edge, not merely shorten the \
             vehicle's own list"
        );
        assert_eq!(
            world.get::<Passengers>(
                world
                    .resource::<EntityIndex>()
                    .get(10)
                    .expect("the boat is tracked")
            )
            .map(|p| p.0.clone()),
            Some(Vec::new()),
            "…and the vehicle's list must be empty, not stale"
        );
    }

    /// A rider the client has not spawned is kept in the vehicle's list rather
    /// than silently dropped: `SET_PASSENGERS` can arrive before the passenger's
    /// own `AddEntity`, and resolving through [`EntityIndex`] at fold time would
    /// lose the seat permanently.
    #[test]
    fn an_unspawned_passenger_id_survives_in_the_vehicles_list() {
        let mut world = ingest_world();
        feed(&mut world, spawn_event(10, "minecraft:minecart"));
        feed(
            &mut world,
            ClientEvent::EntityPassengersChanged {
                vehicle_id: 10,
                passenger_ids: vec![999],
            },
        );
        let cart = world
            .resource::<EntityIndex>()
            .get(10)
            .expect("the minecart is tracked");
        assert_eq!(
            world.get::<Passengers>(cart).map(|p| p.0.clone()),
            Some(vec![999]),
            "an id with no entity yet must still be recorded"
        );
        // The control: the id really is unresolvable, so the assertion above is
        // about the *forward* list surviving and not about a lookup happening to
        // succeed.
        assert_eq!(world.resource::<EntityIndex>().get(999), None);
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
