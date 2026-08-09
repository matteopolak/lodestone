//! The Stage-1 entity component set: one copy of every entity's
//! server-reported state, held as `bevy_ecs` components rather than as a
//! `HashMap<i32, EntityView>` in `lodestone_client::state::Inner`.
//!
//! # How `Reported<T>`'s three states survive the move to components
//!
//! `lodestone_model::Reported<T>` distinguishes three things, and all three are
//! load-bearing (`docs/bevy-migration.md`, Stage 1's "gotcha that will bite"):
//!
//! | `Reported<T>` | component representation |
//! |---|---|
//! | `Unreported` — the server has never mentioned the field | **component absent** |
//! | `Reported(None)` — the server explicitly cleared it | component **present**, inner `None` |
//! | `Reported(Some(v))` — the server set a value | component present, inner `Some(v)` |
//!
//! That is the plan's prescribed encoding, and it is strictly clearer than the
//! nested `Option` was — but only if nothing ever spawns these components with
//! a default. **A dropped item announces its stack exactly once, at spawn, and
//! then sends item-free metadata for the rest of its life**, so a
//! [`DisplayItem`] that were spawned as `DisplayItem(None)` and re-inserted
//! each metadata packet would blank the drop one tick after it appeared — the
//! "dropped item goes invisible" defect. [`apply_entity_spawn`] therefore
//! spawns **no** [`DisplayItem`] and **no** [`CustomName`], and
//! [`apply_entity_metadata`] only inserts one when the update actually carried
//! the field. The unit tests at the bottom of [`crate::ingest`] pin all three
//! states directly.
//!
//! The same "absent means never reported" rule covers the plain `Option` fields
//! of `EntityView` too — [`EntityFlags`], [`Health`], [`Baby`], [`Pose`],
//! [`Variant`], [`CustomNameVisible`], [`Velocity`], [`EntityUuid`] — which is
//! why they are newtypes over the *inner* value rather than over an `Option`.
//! Only the two genuinely three-state fields ([`CustomName`], [`DisplayItem`])
//! wrap an `Option`.
//!
//! # Per-slot nesting in [`Equipment`]
//!
//! `Equipment` is a `Vec<EntityEquipment>`, not a fixed array of `Option`s, for
//! the same reason `EntityView::equipment` was: a slot **absent** from the list
//! is "the server has never mentioned this slot", while a slot present with
//! `item: None` is an explicit "this slot is empty". Flattening to an array
//! loses that, so do not.

use std::collections::HashMap;

use bevy_ecs::component::Component;
use bevy_ecs::entity::Entity;
use bevy_ecs::resource::Resource;
use lodestone_model::{
    EntityAttributeSnapshot, EntityEquipment, EntityPose, EntityVariant, ItemStack, ResourceKey,
    Vec3,
};
use uuid::Uuid;

/// The server-assigned entity id — the key every `ClientEvent` names an entity
/// by, and the interpolation/draw key downstream.
///
/// Present on every networked entity. [`EntityIndex`] maps this back to a
/// `bevy_ecs` [`Entity`] so an id-addressed event can find its components in
/// O(1) without a full scan.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct MinecraftEntityId(pub i32);

/// The entity's UUID, when the spawn carried one.
///
/// **Absent** means the spawn did not carry one — the `Option` in
/// `EntityView::uuid`.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct EntityUuid(pub Uuid);

/// The entity type's canonical key (`minecraft:pig`, `minecraft:item`, …).
#[derive(Component, Debug, Clone, PartialEq, Eq, Hash)]
pub struct EntityKind(pub ResourceKey);

/// Feet position in world space, as last reported.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Position(pub Vec3);

/// Body yaw/pitch, as last reported.
///
/// A newtype over [`lodestone_model::Rotation`] rather than a re-definition:
/// the model type is the version-free vocabulary every `ClientEvent` speaks,
/// and duplicating its fields here would be a second source of truth for the
/// units.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Rotation(pub lodestone_model::Rotation);

/// Absolute head yaw in degrees.
///
/// Tracked separately from [`Rotation`] and never derived from it: vanilla
/// sends it unconditionally at spawn (`add_entity`) and updates it
/// independently via `rotate_head`, because a walking mob's head tracks its
/// target while its body keeps facing its movement direction.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct HeadYaw(pub f32);

/// Last-reported velocity in blocks per tick.
///
/// **Absent** means the server has never reported one, which is a different
/// state from a reported zero (`Velocity(Vec3::ZERO)`) — a dropped item's whole
/// arc depends on the difference, since gravity alone cannot produce an apex.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Velocity(pub Vec3);

/// Whether the server last reported this entity resting on the ground.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct OnGround(pub bool);

/// The shared entity flags byte (on-fire / crouching / sprinting / swimming /
/// invisible / glowing / fall-flying).
///
/// **Absent** means no metadata packet has reported it yet.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct EntityFlags(pub u8);

/// The entity's custom name.
///
/// One of the two genuinely three-state fields: **absent** is "never
/// reported", `CustomName(None)` is "explicitly cleared", `CustomName(Some(s))`
/// is the name it holds. See the module docs.
#[derive(Component, Debug, Clone, PartialEq, Eq)]
pub struct CustomName(pub Option<String>);

/// Whether the custom name renders above the entity. **Absent** until reported.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CustomNameVisible(pub bool);

/// The entity's pose. **Absent** until reported.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pose(pub EntityPose);

/// Current health in half-hearts (living entities only). **Absent** until
/// reported.
#[derive(Component, Debug, Clone, Copy, PartialEq)]
pub struct Health(pub f32);

/// Ticks remaining in the current hurt-flash window — vanilla's `hurtTime`
/// countdown. `LivingEntity.handleDamageEvent` (`LivingEntity.java:2044-2049`,
/// folding [`lodestone_model::ClientEvent::EntityDamaged`]) and
/// `LivingEntity.animateHurt` (`LivingEntity.java:1873-1876`, folding
/// [`lodestone_model::ClientEvent::EntityHurtAnimation`]) both reset the
/// identical pair of fields — `hurtDuration = 10; hurtTime = hurtDuration;` —
/// so one countdown here covers both reports. [`crate::ingest::tick_hurt_time`]
/// ages it toward zero, one per `GameTick`, the same rate
/// `LivingEntity.tick()` decrements the vanilla field.
///
/// **Absent** until the first report, like [`Health`]. Nothing in this crate
/// reads the countdown yet — it exists so a render-side hurt tint has real
/// data to key off, not a guessed decay; wiring that consumer is
/// `lodestone-shell::entities`'s, out of this crate's scope.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HurtTime(pub u32);

/// The block state a `minecraft:falling_block` entity is imitating —
/// `FallingBlockEntity.blockState`, as a global block-state id.
///
/// Folded by [`crate::ingest::apply_falling_block_state`] from
/// [`lodestone_model::ClientEvent::FallingBlockState`], which the version adapter
/// emits from the spawn packet's Object Data field. **Absent** on every other
/// entity, and absent on a falling block until its spawn packet is decoded — so
/// absence is the switch a renderer keys on, exactly as
/// `lodestone-shell`'s `ItemPhysics` does.
///
/// # Why the state id and not a resolved name
///
/// The id is what the wire carries and what the render side wants: the shell
/// resolves geometry by state id (`CrackResolver`'s per-state quad table is
/// indexed by it), so converting to a name here and back there would add two
/// lookups and a place for the two tables to disagree. A *server* consumer that
/// wants the name has `lodestone_data::block_states::block_name`.
///
/// Never updated after the spawn: vanilla has no packet that revises Object Data,
/// and `FallingBlockEntity` synchs no block-state field. A falling block that
/// changed which block it was would be a different entity.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct FallingBlockState(pub u32);

/// A **remote** entity's arm-swing progress — vanilla's `LivingEntity`
/// `swingTime`/`swinging`/`attackAnim`/`oAttackAnim`, folded from
/// `ClientboundAnimatePacket`'s `SWING_MAIN_HAND` action (id `0`) by
/// [`crate::ingest::apply_entity_animation`] and advanced once per tick by
/// [`crate::ingest::tick_entity_swing`].
///
/// # Why this duplicates three fields of [`lodestone_entity::pose::EntityPose`]
/// instead of embedding it
///
/// `EntityPose` is the *full* per-entity render pose — walk cycle, head/body
/// orientation and age alongside the swing clock — because that is what the
/// **local player's** third-person body (`Sim::body_pose` in
/// `lodestone-shell::sim`) needs: one pose, one entity, one clock. A tracked
/// network entity already has all of those *except* the swing clock, spread
/// across `lodestone-shell::entities`' `WalkAnim`/`InterpFrom`/`InterpTo` — on
/// a **different** `bevy_ecs::Entity`, since `EntityInterpPlugin` spawns a
/// render-side entity per mob distinct from this crate's ingest entity (see
/// `entities.rs`'s `EntityInterpPlugin` docs). Embedding `EntityPose` here
/// would carry a second, unused walk cycle and a body/head orientation nothing
/// reads; this type carries only the three fields (`swing_time`, `swinging`,
/// `swing_duration`) a remote swing actually needs, with the identical
/// algorithm — see [`Self::start_swing`], [`Self::tick`] and
/// [`Self::attack_anim_lerp`], each cross-referencing the `EntityPose` method
/// it mirrors term-for-term.
///
/// **Absent** until the first `SwingMainHand` report, like [`HurtTime`].
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct AttackSwing {
    swing_time: i32,
    swinging: bool,
    swing_duration: i32,
    /// Current tick's swing progress, `0.0..=1.0` — vanilla's `attackAnim`.
    pub attack_anim: f32,
    /// Previous tick's swing progress, for [`Self::attack_anim_lerp`]'s
    /// forward-wrapped interpolation — vanilla's `oAttackAnim`.
    pub o_attack_anim: f32,
}

impl AttackSwing {
    /// Begins a swing, or extends one already running — `LivingEntity.swing`,
    /// mirrored from [`lodestone_entity::pose::EntityPose::start_swing`].
    /// Swallows a restart before the half-way point, which is what turns a
    /// held mine's every-tick `SwingMainHand` report into one continuous arc
    /// instead of a stutter — see that method's doc for the full reasoning.
    pub fn start_swing(&mut self, duration: i32) {
        if !self.swinging || self.swing_time >= duration / 2 || self.swing_time < 0 {
            self.swing_time = -1;
            self.swinging = true;
            self.swing_duration = duration.max(1);
        }
    }

    /// One tick's advance — the swing half of
    /// [`lodestone_entity::pose::EntityPose::tick`] (`LivingEntity.updateSwingTime`).
    /// A no-op sawtooth hold at `0.0` before the first [`Self::start_swing`]
    /// call, since `swing_duration` defaults to `0` and is clamped to at least
    /// `1` in the division below rather than dividing by zero.
    pub fn tick(&mut self) {
        self.o_attack_anim = self.attack_anim;
        if self.swinging {
            self.swing_time += 1;
            if self.swing_time >= self.swing_duration {
                self.swing_time = 0;
                self.swinging = false;
            }
        } else {
            self.swing_time = 0;
        }
        self.attack_anim = self.swing_time.max(0) as f32 / self.swing_duration.max(1) as f32;
    }

    /// Interpolated swing progress for a partial tick — vanilla's
    /// `LivingEntity.getAttackAnim`, identical to
    /// [`lodestone_entity::pose::EntityPose::attack_anim_lerp`]: a negative
    /// delta is wrapped forward by one whole swing so the arm carries forward
    /// to rest instead of rewinding backward through the arc when a swing ends
    /// or restarts mid-tick. See that method's doc for why a plain lerp is
    /// wrong here.
    #[must_use]
    pub fn attack_anim_lerp(&self, partial_tick: f32) -> f32 {
        let mut diff = self.attack_anim - self.o_attack_anim;
        if diff < 0.0 {
            diff += 1.0;
        }
        self.o_attack_anim + diff * partial_tick
    }
}

/// How long an entity has been *using* an item, and with which hand — the state
/// behind a bow draw or a crossbow wind (issue #57).
///
/// # The server does not send the counter, so we keep our own
///
/// `LivingEntity`'s synced flags byte carries only a **boolean**: bit 0 is "an
/// item is in use", bit 1 is which hand. `useItemRemaining` is *never* synced.
/// Vanilla's own client does exactly what this type does — `onSyncedDataUpdated`
/// (`LivingEntity.java:3521-3529`) seeds its countdown the moment the bit flips
/// on, and ticks it locally.
///
/// # Counting up, not down
///
/// Vanilla counts `useItemRemaining` **down** from `getUseDuration`, then derives
/// `getTicksUsingItem() = duration - remaining` (`LivingEntity.java:3594`) — and
/// `getTicksUsingItem` is what every pose and every draw-power formula actually
/// reads. So [`ticks`](Self::ticks) counts **up** from zero, which is that same
/// quantity without needing `getUseDuration`. That matters: the duration is a
/// per-item value (`72000` for a bow, so "remaining" would be a number no pose
/// uses) and for a crossbow it depends on the Quick Charge enchantment level.
/// Counting up removes an item-data lookup from the hot path *and* removes a
/// whole class of "which duration did we assume" bug.
///
/// **Absent** until the first metadata packet mentioning the byte, like
/// [`AttackSwing`] and [`HurtTime`].
#[derive(Component, Debug, Clone, Copy, PartialEq, Default)]
pub struct ItemUse {
    /// Whether an item is in use right now (`LivingEntity.isUsingItem`).
    pub using: bool,
    /// Whether the item is in the off hand (`getUsedItemHand() == OFF_HAND`).
    /// Meaningless while `!using`.
    pub off_hand: bool,
    /// Ticks elapsed since the use began — vanilla's `getTicksUsingItem()`.
    /// Held at `0` while `!using`.
    pub ticks: u32,
}

impl ItemUse {
    /// Folds a freshly-received living-entity flags byte in, preserving or
    /// resetting [`ticks`](Self::ticks) as the *edge* dictates.
    ///
    /// # The whole point of this method is that a repeat is not an edge
    ///
    /// A server re-sends the same metadata byte freely — on entity re-track, on
    /// any other field in the same packet changing, and every time the player
    /// enters range. Resetting the counter on each packet would pin the draw at
    /// zero and produce a bow that is permanently un-drawn while looking, from the
    /// byte alone, perfectly correct. So the counter only resets on a **rising
    /// edge** (`!was_using && now using`) or on the hand changing, which vanilla
    /// treats the same way — `startUsingItem` is guarded by `!this.isUsingItem()`
    /// (`LivingEntity.java:3500`).
    pub fn apply_flags(&mut self, using: bool, off_hand: bool) {
        let restart = (using && !self.using) || (using && off_hand != self.off_hand);
        if restart {
            self.ticks = 0;
        }
        if !using {
            self.ticks = 0;
        }
        self.using = using;
        self.off_hand = off_hand;
    }

    /// One tick's advance. Only counts while in use, and **saturates** rather
    /// than wrapping: a bow's `getUseDuration` is `72000` ticks (an hour), so an
    /// entity left holding one is a real, reachable input, and a `u32` wrap would
    /// snap a fully-drawn bow back to slack.
    pub fn tick(&mut self) {
        if self.using {
            self.ticks = self.ticks.saturating_add(1);
        }
    }
}

/// The mob-flags byte's decoded state — today just **aggressive**, vanilla's
/// `Mob.isAggressive()` (issue #379).
///
/// # Why this is not a field on [`ItemUse`]
///
/// They come from *different bytes at different metadata indices* and mean
/// unrelated things. `ItemUse` is `LivingEntity`'s using-item state, which is what
/// a **player** sets when drawing a bow; this is `Mob`'s attack state, which is
/// what a **mob** sets. A skeleton shooting at you sets this and never the other,
/// and a player drawing a bow sets the other and never this — so folding them
/// would make "is the bow drawn" read off whichever byte arrived last.
///
/// # Why it has no tick counter
///
/// A bow draw's *fraction* has to be counted locally because vanilla never syncs
/// it. Aggressive has no fraction: `AbstractSkeletonRenderer` maps it straight to
/// `BOW_AND_ARROW`, which is a fixed pose, and `animateZombieArms` maps it to one
/// of two constants. So this is a plain latched boolean and `IngestSet::Apply`'s
/// `Commands::insert` (which *replaces* the component) is the right shape for it,
/// unlike `ItemUse`.
///
/// **Absent** until the first metadata packet carrying the byte, like
/// [`AttackSwing`], [`HurtTime`] and [`ItemUse`] — and absent forever for every
/// non-`Mob` entity, because the adapter withholds the byte for those (an armour
/// stand's index-15 byte means something else entirely).
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MobState {
    /// `Mob.isAggressive()` — set by the attack goals while a target is engaged.
    pub aggressive: bool,
}

/// Whether the entity is a baby (ageable mobs only). **Absent** until reported.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Baby(pub bool);

/// A creeper's fuse direction — `Creeper.DATA_SWELL_DIR`
/// ([`lodestone_model::event::EntityMetadataUpdate::creeper_swell_dir`]), `-1`
/// while idle or backing off, `1` while counting up to detonation. **Absent**
/// until the first report, like [`Baby`] — which for an idle, never-approached
/// creeper is forever, since `SynchedEntityData` never puts a field on the wire
/// that is already at its accessor default (the protocol adapter works around
/// this at spawn; see `docs/entity-rendering.md`'s "Creeper swell" section).
///
/// Only the direction is a component here, not `Creeper.DATA_IS_POWERED`/
/// `DATA_IS_IGNITED` alongside it: both decode at the protocol layer
/// (`EntityMetadataUpdate::creeper_powered`/`creeper_ignited`), but nothing
/// downstream of the ECS reads either one yet — `lodestone-shell::entities`'
/// `CreeperFuse`/white-flash-overlay chain only ever consumes the direction.
/// Per CLAUDE.md's island rule, add `powered`/`ignited` here (and their
/// `apply_entity_metadata` arms) only alongside whatever render path first
/// consumes them, not speculatively.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct CreeperSwellDir(pub i32);

/// An experience orb's XP value — `ExperienceOrb.DATA_VALUE`
/// ([`lodestone_model::event::EntityMetadataUpdate::experience_orb_value`]).
/// **Absent** until the first report, like [`CreeperSwellDir`], and absent
/// forever for every entity that is not an orb, because the protocol adapter
/// withholds the field for those (index 8's `INT` means something else on a
/// primed TNT, a fishing hook, a vehicle and a display entity).
///
/// The value's only consumer is the sprite: `ExperienceOrb.getIcon()` buckets it
/// into one of eleven cells of `experience_orb.png`. An orb whose value has never
/// been reported therefore draws cell 0, which is what vanilla's own accessor
/// default of `0` produces — not "draw nothing".
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExperienceOrbValue(pub i32);

/// The entity's cosmetic variant (sheep colour, villager profession, …).
///
/// **Absent** means the server sent no variant override, and a consumer should
/// draw the entity type's vanilla default — which is a different state from a
/// known-but-plain variant. Do not read absence as "unknown".
#[derive(Component, Debug, Clone, PartialEq)]
pub struct Variant(pub EntityVariant);

/// The entity's attributes, keyed by canonical id, as `update_attributes` last
/// reported them. Later snapshots for the same attribute replace earlier ones.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct Attributes(pub Vec<EntityAttributeSnapshot>);

/// What the entity is wearing and holding, as `set_equipment` last reported it.
///
/// A slot absent from the list is "never mentioned"; a slot present with
/// `item: None` is an explicit clear. See the module docs on why this is a list
/// of pairs rather than an array.
#[derive(Component, Debug, Clone, Default, PartialEq)]
pub struct Equipment(pub Vec<EntityEquipment>);

/// The item stack this entity *displays* — a dropped item's entire visible
/// identity, plus the display item of thrown projectiles and the eye of ender.
///
/// The second of the two three-state fields: **absent** is "never reported",
/// `DisplayItem(None)` is the server's explicit empty stack (which vanilla
/// draws as nothing), `DisplayItem(Some(stack))` is the stack it holds. See the
/// module docs — this is the component the "dropped item goes invisible"
/// regression lives in.
#[derive(Component, Debug, Clone, PartialEq)]
pub struct DisplayItem(pub Option<ItemStack>);

/// Who is riding this entity, in mounting order — `Entity.passengers`, folded
/// from `ClientboundSetPassengersPacket` by
/// [`crate::ingest::apply_entity_passengers`].
///
/// **Server entity ids, not `bevy_ecs::Entity`s**, and deliberately so: the
/// packet can name a passenger the client has not spawned yet (the vehicle's
/// `AddEntity` and the passenger's arrive in either order, and `SET_PASSENGERS`
/// can precede both), so resolving through [`EntityIndex`] at fold time would
/// silently drop the seat. The id survives; the lookup happens at read time,
/// where a miss is an honest "not tracked yet" rather than a lost seat.
///
/// # This is not `Option`-wrapped, and absence is not "never reported"
///
/// Unlike most of this module, the empty case is a *real* state the server
/// reports: `SET_PASSENGERS` with a zero-length array is how vanilla announces a
/// dismount. So `Passengers(vec![])` means "explicitly nobody", while the
/// component being **absent** means the same thing by default — an entity nobody
/// has ever mounted. Both read as "no riders", which is why this one field does
/// not need the three-state encoding the module docs describe.
///
/// [`crate::ingest::apply_entity_passengers`] is the only writer, and it
/// *replaces* the list wholesale: the packet is absolute, never a delta.
#[derive(Component, Debug, Clone, Default, PartialEq, Eq)]
pub struct Passengers(pub Vec<i32>);

/// The server entity id of the vehicle this entity is riding, if any —
/// `Entity.vehicle`, the reverse of [`Passengers`].
///
/// Derived by [`crate::ingest::apply_entity_passengers`] from the same packet
/// rather than reported separately: `SET_PASSENGERS` names the vehicle and lists
/// its riders, so the reverse edge is a fold of the forward one.
///
/// # Why the reverse edge is stored rather than searched
///
/// The question every consumer actually asks is "what am *I* riding" — the
/// camera, the `on_ground` override, the dismount key. Answering that from
/// [`Passengers`] alone is a scan over every tracked entity per tick. More
/// importantly a scan cannot be made *correct* cheaply: a passenger transferring
/// from one vehicle to another produces two `SET_PASSENGERS` packets in an
/// unspecified order, and this component is written by the same system that
/// writes both lists, so the transient double-membership a scan would see cannot
/// be observed here.
///
/// **Absent** means not riding anything. That is the whole state; there is no
/// "unreported" case, because a rider is always announced by the packet that
/// seats it.
#[derive(Component, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Vehicle(pub i32);

/// Server entity id → `bevy_ecs` [`Entity`].
///
/// Maintained eagerly by [`apply_entity_spawn`](crate::ingest::apply_entity_spawn)
/// and [`apply_entity_removal`](crate::ingest::apply_entity_removal) rather than
/// rebuilt by a scan, so a movement event in the *same* ingest batch as the
/// spawn it follows can still find its entity.
///
/// This is azalea's `EntityIdIndex` (`azalea-client/src/client.rs`) in
/// miniature, minus the per-client partition — we are a single client, so one
/// global index is the whole story.
#[derive(Resource, Debug, Default)]
pub struct EntityIndex(HashMap<i32, Entity>);

impl EntityIndex {
    /// The ECS entity for a server entity id, if it is currently tracked.
    #[must_use]
    pub fn get(&self, entity_id: i32) -> Option<Entity> {
        self.0.get(&entity_id).copied()
    }

    /// Records `entity` as the holder of `entity_id`, replacing any previous
    /// mapping (servers reuse ids freely).
    pub fn insert(&mut self, entity_id: i32, entity: Entity) {
        self.0.insert(entity_id, entity);
    }

    /// Forgets `entity_id`, returning the ECS entity it mapped to.
    pub fn remove(&mut self, entity_id: i32) -> Option<Entity> {
        self.0.remove(&entity_id)
    }

    /// Every tracked `(server id, ECS entity)` pair. Order is unspecified.
    pub fn iter(&self) -> impl Iterator<Item = (i32, Entity)> + '_ {
        self.0.iter().map(|(id, entity)| (*id, *entity))
    }

    /// How many entities are tracked.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether no entities are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// Forgets every mapping, without touching the entities they pointed to.
    ///
    /// The caller is responsible for despawning first —
    /// [`crate::ingest::reset_ingest_entities`] is the one place that does
    /// both, in that order.
    pub fn clear(&mut self) {
        self.0.clear();
    }
}
