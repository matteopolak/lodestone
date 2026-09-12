//! Server-side mob simulation — the *consumer* that ticks mob AI.
//!
//! `lodestone-entity` owns a complete goal scheduler, A\* pathfinder, and the
//! [`NavigatingMob`] composition that wires them together over the version-free
//! [`lodestone_entity::pathfinding::PathWorld`] seam. The server tick loop
//! advances this simulation and
//! publishes its snapshots to the connection layer. Clients interpolate those
//! positions; mob decisions and movement remain server-side in this module.
//!
//! Two pieces, deliberately kept separate rather than fused:
//!
//! * [`ChunkWorld`] adapts the server's own [`crate::chunk::ChunkColumn`] terrain
//!   (which
//!   stores complete block-state strings, not just a solid/air bit — see its
//!   own doc comment) into a [`lodestone_entity::pathfinding::PathWorld`]. It is
//!   the terrain adapter for
//!   `lodestone-render`'s `world.rs`: this crate owns terrain *storage*,
//!   `lodestone-entity` owns the traversal reasoning, and the adapter is the
//!   single seam between them. It classifies each cell through the real
//!   26.2 per-block-state census (`lodestone_data::path_types` +
//!   `collision_shapes`) rather than a solid/air guess — and it
//!   stays version-free doing it, because `lodestone-data` is 26.2 *game*
//!   data (tags, collision geometry, ...) with no protocol dependency of its
//!   own (`docs/lodestone-data-crate.md`), not a `crates/protocol/*` crate.
//!   `base_path_type`/`collision_top` distinguish water, lava, fences,
//!   trapdoors, and damaging blocks for navigation.
//!   `lodestone_entity::pathfinding::PathWorld::collides`
//!   intentionally keeps the coarse jump-clearance/diagonal-reach sweep over
//!   [`crate::chunk::ChunkColumn::is_solid`]; shape-aware AABB checks remain outside this
//!   adapter.
//! * [`MobSim`] owns the live mobs and advances them one tick at a time. The
//!   world outlives the sim (the mobs borrow it), which is why `ChunkWorld` is a
//!   value the caller holds and hands to [`MobSim::new`] by reference.
//!
//! # Live mob ticking
//!
//! Entity packets are produced by the version adapter and consumed by the
//! connection streaming pass. `MobSim` is `Send`, so a tick task can own the
//! simulation while the connection task reads snapshots; the compile-time
//! `assert_send::<MobSim<'static>>()` check documents that requirement.
//!
//! [`crate::tick::run_tick_loop`] keeps a [`ChunkWorld`] snapshot and a seeded
//! [`MobSim`], advances it once per server tick, and republishes snapshots into
//! the shared `EntitySource` consumed by the
//! [`serve_connection`](crate::serve_connection) streaming pass. See
//! [`crate::IntegratedServer::open_in_memory_with_mobs`] for the production
//! setup and `docs/live-mob-sim.md` for the remaining terrain/biome-aware
//! spawning boundary.

use std::collections::HashMap;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use lodestone_data::{
    block_states, collision_shapes, entity_dimensions, entity_type::EntityType, potion::PotionId,
};
// `collide` and `CollisionView` are the item pass's swept resolve against the real
// per-state shape census (see `LiveBlockCollision`); `Vec3d` is the physics crate's own
// vector, which `Vec3` (this crate's) is converted to at that seam rather than
// through the whole module.
use lodestone_physics::{CollisionView, EntityDimensions, Vec3d, collision::collide};
use lodestone_entity::ai::roster::{self, SpeciesContext};
use lodestone_entity::brain::{FROG_FOOD_SPECIES, NearbyBrainEntity, is_brain_species};
use lodestone_entity::ai::navigating_mob::{
    BABY_START_AGE, DEFAULT_FOLLOW_RANGE, PARENT_AGE_AFTER_BREEDING,
};
use lodestone_entity::ai::mob::{EatenBlock, ProjectileLaunch};
use lodestone_entity::ai::{Goal, GoalSelector, MobController, NavigatingMob};
use lodestone_entity::attribute::default_attributes;
use lodestone_entity::equipment::{
    self, attack_damage_from_attributes, defenses_from_attributes,
    knockback_resistance_from_attributes,
};
use lodestone_entity::explosion::Aabb as ExplosionAabb;
use lodestone_entity::vibration::{
    ALLAY_LISTENER_RADIUS, PostedVibration, VibrationEvent, WARDEN_LISTENER_RADIUS,
    is_vibration_listener, nearest_listenable, nearest_note_block_play,
};
use lodestone_entity::item_entity::{ItemEntityRegistry, ItemLifecycle, ItemMotion};
use lodestone_entity::pathfinding::{MobShape, PathType};
use lodestone_entity::projectile::{Projectile, ProjectileRegistry};
use lodestone_entity::spawn_equipment::{self, EquipRandom};
use lodestone_entity::{
    AttributeMap, DamageFlags, Defenses, HurtCooldown, HurtDecision, RayView, entity_damage,
    seen_percent,
};
use lodestone_model::{
    BlockPos, Difficulty, EntityEquipment, EquipmentSlot as ModelEquipmentSlot, Identifier,
    ItemStack, ResourceKey, Rotation, Vec3,
};
use uuid::Uuid;

use crate::chunk::ChunkSource;
use crate::entity_handoff::{EntityHandoffToken, EntityOwnershipHandoff};
#[cfg(test)]
use crate::chunk::AIR;
use crate::protocol::{EntitySnapshot, MetadataField};
use crate::mob_spawn::{
    DespawnOutcome, MobCategory, SpawnCandidate, SpawnCandidateSource, SpawnRng, SpawnState,
    check_despawn,
};
use crate::server::EntitySource;

/// The chunk-local owner of an entity effect produced during one serial tick.
///
/// This is deliberately a planning boundary rather than a worker handle. The
/// simulation still visits entities in its established vector order; effects
/// retain that sequence when the central tick task consumes owner batches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntityTickOwner {
    /// The column containing the entity when it produced the effect.
    Chunk { cx: i32, cz: i32 },
}

/// One effect handed from an entity-owned simulation phase to the central
/// publisher.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityTickEffect {
    /// The owner that produced this effect.
    pub owner: EntityTickOwner,
    /// The entity position used to assign that owner.
    pub source: Vec3,
    /// Its old serial visit position among effects drained in this hand-off.
    pub sequence: usize,
    effect: crate::effects::WorldEffect,
}

impl EntityTickEffect {
    /// The wire-visible effect the central publisher must emit.
    #[must_use]
    pub fn effect(&self) -> &crate::effects::WorldEffect {
        &self.effect
    }
}

/// The messages one chunk owner returns from an entity simulation phase.
///
/// Batches group effects by owner, while [`EntityTickEffect::sequence`] keeps
/// today's cross-owner serial publication order explicit. A future worker may
/// produce one batch independently but must still use that central merge.
#[derive(Debug, Clone, PartialEq)]
pub struct EntityTickEffectBatch {
    /// The owner that produced every effect in this batch.
    pub owner: EntityTickOwner,
    effects: Vec<EntityTickEffect>,
}

/// One completed chunk-owner batch of entity-push impulses.
#[derive(Debug, Clone)]
pub(crate) struct EntityPushOwnerBatch {
    owner: EntityTickOwner,
    expected_batch_count: usize,
    effects: Vec<EntityPushEffect>,
}

#[derive(Debug, Clone)]
struct EntityPushEffect {
    owner: EntityTickOwner,
    serial: usize,
    id: i32,
    impulse: Vec3,
}

/// One completed chunk-owner batch of burn-counter updates and damage.
#[derive(Debug, Clone)]
pub(crate) struct BurnTickOwnerBatch {
    owner: EntityTickOwner,
    plan: u64,
    expected_batch_count: usize,
    effects: Vec<BurnTickEffect>,
}

#[derive(Debug, Clone)]
struct BurnTickEffect {
    owner: EntityTickOwner,
    serial: usize,
    id: i32,
    burn: crate::burning::BurnState,
    damage: f32,
}

/// One immutable burn-counter input owned by a source chunk for one pass.
#[derive(Debug, Clone, Copy)]
struct BurnTickInput {
    owner: EntityTickOwner,
    serial: usize,
    id: i32,
    burn: crate::burning::BurnState,
    in_water: bool,
    fire_immune: bool,
    fire_resistance: bool,
}

/// One completed source-chunk batch of leash decisions.
#[derive(Debug, Clone)]
pub(crate) struct LeashTickOwnerBatch {
    owner: EntityTickOwner,
    plan: u64,
    expected_batch_count: usize,
    expected_effect_count: usize,
    effects: Vec<LeashTickEffect>,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum LeashTickAction {
    Keep,
    Orphan,
    Pull(Vec3),
    Snap(Vec3),
}

#[derive(Debug, Clone)]
struct LeashTickEffect {
    owner: EntityTickOwner,
    serial: usize,
    id: i32,
    action: LeashTickAction,
}

/// One immutable leashed-mob input owned by a source chunk for a single
/// owner-worker pass.
#[derive(Debug, Clone, Copy)]
struct LeashTickInput {
    owner: EntityTickOwner,
    serial: usize,
    id: i32,
    position: Vec3,
    holder: LeashHolder,
}

impl EntityTickEffectBatch {
    /// Effects in the owner's original serial order.
    #[must_use]
    pub fn effects(&self) -> &[EntityTickEffect] {
        &self.effects
    }
}

#[derive(Debug, Clone, PartialEq)]
struct PendingEntityTickEffect {
    owner: EntityTickOwner,
    source: Vec3,
    effect: crate::effects::WorldEffect,
}

fn entity_tick_owner(position: Vec3) -> EntityTickOwner {
    EntityTickOwner::Chunk {
        cx: (position.x.floor() as i32).div_euclid(16),
        cz: (position.z.floor() as i32).div_euclid(16),
    }
}

fn batch_entity_tick_effects(
    effects: Vec<PendingEntityTickEffect>,
) -> Vec<EntityTickEffectBatch> {
    let mut batches: Vec<EntityTickEffectBatch> = Vec::new();
    for (sequence, pending) in effects.into_iter().enumerate() {
        let effect = EntityTickEffect {
            owner: pending.owner,
            source: pending.source,
            sequence,
            effect: pending.effect,
        };
        if let Some(batch) = batches.iter_mut().find(|batch| batch.owner == effect.owner) {
            batch.effects.push(effect);
        } else {
            batches.push(EntityTickEffectBatch {
                owner: effect.owner,
                effects: vec![effect],
            });
        }
    }
    batches
}

fn merge_entity_push_owner_batches(
    mut batches: Vec<EntityPushOwnerBatch>,
) -> Vec<EntityPushEffect> {
    let expected_batch_count = batches
        .first()
        .map(|batch| batch.expected_batch_count)
        .expect("entity-push completion must contain every tick-start owner batch");
    let mut owners = std::collections::HashSet::new();
    for batch in &batches {
        assert_eq!(
            batch.expected_batch_count, expected_batch_count,
            "entity-push completions must originate from one tick-start plan"
        );
        assert!(
            owners.insert(batch.owner),
            "entity-push completion may not contain one owner twice"
        );
        assert!(
            batch.effects.iter().all(|effect| effect.owner == batch.owner),
            "an entity-push owner batch may contain only its own effects"
        );
    }
    assert_eq!(
        batches.len(),
        expected_batch_count,
        "entity-push completion must contain every tick-start owner batch exactly once"
    );
    let mut effects: Vec<_> = batches
        .drain(..)
        .flat_map(|batch| batch.effects)
        .collect();
    effects.sort_unstable_by_key(|effect| effect.serial);
    for (serial, effect) in effects.iter().enumerate() {
        assert_eq!(
            effect.serial, serial,
            "entity-push completion must retain every tick-start serial slot exactly once"
        );
    }
    effects
}

fn merge_burn_tick_owner_batches(mut batches: Vec<BurnTickOwnerBatch>) -> Vec<BurnTickEffect> {
    let first = batches
        .first()
        .expect("burn completion must contain every tick-start owner batch");
    let plan = first.plan;
    let expected_batch_count = first.expected_batch_count;
    let mut owners = std::collections::HashSet::new();
    for batch in &batches {
        assert_eq!(
            (batch.plan, batch.expected_batch_count),
            (plan, expected_batch_count),
            "burn completions must originate from one tick-start plan"
        );
        assert!(
            owners.insert(batch.owner),
            "burn completion may not contain one owner twice"
        );
        assert!(
            batch.effects.iter().all(|effect| effect.owner == batch.owner),
            "a burn owner batch may contain only its own effects"
        );
    }
    assert_eq!(
        batches.len(),
        expected_batch_count,
        "burn completion must contain every tick-start owner batch exactly once"
    );
    let mut effects: Vec<_> = batches
        .drain(..)
        .flat_map(|batch| batch.effects)
        .collect();
    effects.sort_unstable_by_key(|effect| effect.serial);
    for (serial, effect) in effects.iter().enumerate() {
        assert_eq!(
            effect.serial, serial,
            "burn completion must retain every tick-start serial slot exactly once"
        );
    }
    effects
}

fn merge_leash_tick_owner_batches(mut batches: Vec<LeashTickOwnerBatch>) -> Vec<LeashTickEffect> {
    let first = batches
        .first()
        .expect("leash completion must contain every tick-start owner batch");
    let plan = first.plan;
    let expected_batch_count = first.expected_batch_count;
    let expected_effect_count = first.expected_effect_count;
    let mut owners = std::collections::HashSet::new();
    for batch in &batches {
        assert_eq!(
            (batch.plan, batch.expected_batch_count, batch.expected_effect_count),
            (plan, expected_batch_count, expected_effect_count),
            "leash completions must originate from one tick-start plan"
        );
        assert!(
            owners.insert(batch.owner),
            "leash completion may not contain one owner twice"
        );
        assert!(
            batch.effects.iter().all(|effect| effect.owner == batch.owner),
            "a leash owner batch may contain only its own effects"
        );
    }
    assert_eq!(
        batches.len(),
        expected_batch_count,
        "leash completion must contain every tick-start owner batch exactly once"
    );
    let mut effects: Vec<_> = batches
        .drain(..)
        .flat_map(|batch| batch.effects)
        .collect();
    assert_eq!(
        effects.len(),
        expected_effect_count,
        "leash completion must retain every tick-start leash"
    );
    effects.sort_unstable_by_key(|effect| effect.serial);
    assert!(
        effects.windows(2).all(|pair| pair[0].serial < pair[1].serial),
        "leash completion may contain each tick-start serial slot only once"
    );
    effects
}

mod block_ids;

// Re-exported so `crate::mobs::block_state_id`/`block_state_id_or_default` keep
// resolving for every existing caller outside this module — `block_breaking.rs`,
// `block_drops.rs`, `boat.rs`, `spawn_egg.rs`, `random_tick.rs`, `effects.rs` and
// `piston.rs` all name them through that exact path, and none of those files are
// this split's to touch.
pub(crate) use block_ids::{block_state_id, block_state_id_or_default};

mod world;

// Re-exported so `crate::mobs::ChunkWorld` keeps resolving: `lib.rs`'s own
// `pub use mobs::{..., ChunkWorld, ...}`, plus `tick_area.rs`, `integrated.rs`
// and `tick.rs`, all name it through that exact path.
pub use world::ChunkWorld;

mod golem;

// Re-exported for path stability: unlike `ChunkWorld` above, no external
// caller names `crate::mobs::GolemConstruction`/`GolemSpecies` today (checked
// crate-wide), but both are `pub`, so keeping `mobs::GolemConstruction` and
// `mobs::GolemSpecies` resolving costs one line and avoids relying on that
// staying true.
pub use golem::{GolemConstruction, GolemSpecies};

// Villager professions and workstation claiming. `pub`
// (not re-exported at the top level) so `crate::server` can reach
// `crate::mobs::villager::trades::offers_up_to` when it builds a
// `MERCHANT_OFFERS` packet from an `InteractOutcome::OpenTrade`.
pub mod villager;

// Species helpers stay private to this module tree; `pub(super)` exposes them
// to descendant simulation modules without adding an external API path.
mod species;
mod sim_mob;
mod handle;
pub use handle::{LiveMobSource, MobHandle};
mod sim_config_spawn;
mod sim_tick;
mod sim_interactions;
mod sim_combat;
mod sim_spawning;
mod sim_entities;
mod sim_persistence;
mod sim_effects;
mod sim_snapshots;

// No re-export: `ProjectileHit`/`projectile_damage_type`/`first_solid_along`
// `ProjectileHit`/`projectile_damage_type`/`first_solid_along` remain private;
// public `MobSim` methods provide the external projectile surface.
mod projectiles;

// Public `MobSim` methods provide the item surface. `merge_neighbouring_items` is
// `pub(super)` in `items.rs` because `tick_with_terrain` below calls it via
// `self.merge_neighbouring_items()` — a method call, so no `items::` prefix
// is needed at that call site either.
mod items;

// No re-export: every `impl MobSim` method was already `pub`. `tick_orbs` is
// `pub(super)` for the same reason `merge_neighbouring_items` is (called from
// `tick_with_terrain` below), and `ORB_BEHAVIOR_SEED` is `pub(super)` because
// `MobSim::new` reads it directly as `orbs::ORB_BEHAVIOR_SEED`.
mod orbs;

// The merge is crate-visible so only the tick loop can consume owner results.
// The simulation methods themselves remain on `MobSim`.
mod falling_blocks;
pub(crate) use falling_blocks::merge_falling_block_tick_effect_batches;

// No re-export: every `impl MobSim` method here was already `pub`;
// `VehicleCollision` stays private, used only within this file's own
// `tick_vehicles`.
mod vehicles;

// No re-export: every `impl MobSim` method in each is already `pub`. See
// `mobs::dragon`/`mobs::end_crystal`'s own module docs for the pure
// `crate::dragon` state machine each drives.
mod dragon;
mod end_crystal;

// See `mobs::wither`/`mobs::wither_pattern`'s own module docs: the wither
// boss's `crate::wither` state driven with real inputs, plus the summon-
// structure block-pattern matcher (`golem.rs`'s own approach, duplicated for
// a different cell alphabet rather than widened, per that module's own
// closed-`GolemCell`-enum shape).
mod wither;
mod wither_pattern;

// `pub(crate)`, unlike every sidecar module above: `crate::block_drops`,
// `crate::random_tick`, `crate::tick` and `crate::server` all need
// `tnt::is_tnt_block`/`TICK_TNT_PRIME`/`DEFAULT_FUSE_TIME` to recognise and
// schedule TNT ignition from outside this crate's mob simulation, which none
// of the boat/falling-block/orb sidecars need. Every `impl MobSim` method
// here was already `pub`; `TntCollision` stays private, used only within this
// file's own `tick_tnt`. `TNT_LAUNCH_SEED` is `pub(super)` for the same
// reason `ORB_BEHAVIOR_SEED` is — `MobSim::new` reads it directly as
// `tnt::TNT_LAUNCH_SEED`.
pub(crate) mod tnt;

// `pub(crate)`, the same shape `tnt` is above: `crate::redstone_dispenser`,
// `crate::item_use` and `crate::server` all need
// `minecart::{MinecartKind, is_rail_block, rail_shape, placement_position}`
// to recognise a rail and derive a spawn position from outside this crate's
// mob simulation. Every `impl MobSim` method here was already `pub`;
// `MinecartCollision` stays private, used only within this file's own
// `tick_minecarts`.
pub(crate) mod minecart;

// No re-export: `LiveBolt` is `pub(super)`, visible within `mobs` and its
// descendants, and every `impl MobSim` method here is either `pub` already or
// `pub(super)` because only `crate::tick` (through `MobHandle::with`) and this
// module's own driver plumbing call it.
mod lightning;

// Public `MobSim` methods (`cast_fishing_bobber`, `retrieve_fishing_bobber`, …)
// provide the fishing surface; `FishHookState`/
// `FishingBobber` stay `pub(super)`, and `FISHING_ROLL_SEED` is read directly
// as `fishing::FISHING_ROLL_SEED` by `MobSim::new`, the same shape
// `orbs::ORB_BEHAVIOR_SEED` uses.
mod fishing;

// Raid support covers the raid half; patrols are documented in the module doc
// and `docs/pillager-patrols.md`. Public `MobSim` methods provide the raid
// surface; `RAID_ROLL_SEED` is read the same way
// `fishing::FISHING_ROLL_SEED` is.
mod raid;

// The warden anger consumer for the vibration
// substrate (`crate::mobs::vibration` — re-exported from `lodestone_entity`).
// `pub` because `warden::AngerLevel` is part of `SimMob::warden_anger_level`'s
// public return type.
pub mod warden;

// The sniffer's seek/dig/rise/egg-drop
// state machine. `pub` for the same reason `warden` is — `sniffer::SnifferState`
// is part of `SimMob::snapshot`'s metadata output.
pub mod sniffer;

// Piston entity shoving. Not `pub` — `crate::tick` reaches it
// through `MobSim::shove_from_piston` alone, which `MobSim` (already
// re-exported) already carries.
mod piston_shove;

/// Reads a computed attribute value from `attrs` by bare path (e.g.
/// `"max_health"`), applying the registry default when the attribute is not
/// explicitly present — mirrors [`AttributeMap::value`]'s own fallback so a
/// caller never has to special-case an absent key.
fn attr(attrs: &AttributeMap, path: &str) -> f64 {
    Identifier::new_borrowed("minecraft", path)
        .ok()
        .and_then(|id| attrs.value(&id))
        .unwrap_or(0.0)
}

/// [`attr`], but answering **`None`** when `attrs` does not actually carry the
/// attribute, instead of silently substituting the registry default.
///
/// # Why this is not the same function with a different default
///
/// [`attr`]'s `unwrap_or(0.0)` looks like the miss case and is nearly
/// unreachable: [`AttributeMap::value`] already falls back to
/// `default_def(key).default` for an absent instance, so it returns `Some` for
/// every attribute the registry knows. `attr(&AttributeMap::new(),
/// "follow_range")` is therefore **32.0**, not `0.0` — and 32.0 is the one value
/// `follow_range` must never take, because the generic mob attribute setup
/// overrides it to `16.0` for *every* mob, so no living entity in the
/// game ever carries the registry number (the registry default and the
/// builder override live in two different places; see `DEFAULT_FOLLOW_RANGE`'s
/// own doc).
///
/// So a caller that needs "the species really declares this" cannot get it by
/// range-checking [`attr`]'s result — the wrong value is inside the plausible
/// range. It has to ask whether the instance exists, which is what this does.
/// `control_the_attribute_lookup_misses_to_the_registry_default_not_zero` pins
/// both readings so this distinction cannot quietly collapse.
fn attr_present(attrs: &AttributeMap, path: &str) -> Option<f64> {
    Identifier::new_borrowed("minecraft", path)
        .ok()
        .and_then(|id| attrs.get(&id))
        .map(lodestone_entity::attribute::AttributeInstance::value)
}

/// The per-tick velocity decay a grounded mob's horizontal motion is
/// subjected to on ordinary, unmodified-friction terrain: standard block
/// friction combined with the constant air-drag factor every entity carries
/// regardless of the block underfoot. See `docs/mob-species-spawning.md` for
/// the measured conversion documented in `docs/mob-species-spawning.md`.
const AI_GROUND_FRICTION: f64 = 0.6 * 0.91;

/// Converts a requested ground speed — a goal's speed multiplier applied
/// to the mob's `movement_speed` attribute, the unit every roster goal in
/// this crate already hands to [`NavigatingMob`](lodestone_entity::ai::navigating_mob::NavigatingMob)'s
/// `move_to` — into the sustained blocks-per-tick rate an AI-driven mob
/// actually converges on.
///
/// The AI movement controller does not drive a mob at full input magnitude the
/// way a player's WASD does: the forward input it feeds into
/// the entity's own travel step is numerically the *same* value as the
/// per-tick speed scale applied to that input, so the two multiply — the
/// per-tick thrust actually added to the mob's velocity is the *square* of
/// the requested speed, not the value itself. That thrust then accumulates
/// against [`AI_GROUND_FRICTION`] every tick until it converges on this
/// steady cruising speed. See `docs/mob-species-spawning.md` for the exact
/// methods this reproduces and the live-oracle measurement it was checked
/// against (a real zombie's measured mean pursuit speed against its
/// predicted value).
fn ai_ground_speed(requested_speed: f64) -> f64 {
    (requested_speed * requested_speed) / (1.0 - AI_GROUND_FRICTION)
}

/// The health and combat-stat defaults for a mob type: `(max_health,
/// attack_damage, defenses, knockback_resistance)`.
///
/// Folds through [`default_attributes`] when `entity_type` is one of the
/// templates this module knows (the zombie family, skeleton family,
/// creeper, spider, and the common animals); for anything else it falls back
/// to an empty [`AttributeMap`], whose [`AttributeMap::value`] already resolves
/// every path to the generic `RangedAttribute` default (`max_health` 20,
/// `attack_damage` 2, no armor, no knockback resistance) — the same "unknown
/// type gets the generic default, never a guess" shape
/// [`resolve_mob_shape`](crate::resolve_mob_shape) uses for census geometry.
///
/// `knockback_resistance` (`minecraft:knockback_resistance`, registry default
/// `0.0`) is read here rather than folded into [`Defenses`] because it is a
/// *physics* property — `lodestone_physics::knockback::knockback_impulse`'s
/// own `knockback_resistance` parameter — not a damage-reduction one;
/// `Defenses` is exhaustively the damage pipeline's own fields (see
/// `lodestone_entity::damage`'s module doc, "knockback impulse... `impl-physics`
/// builds the knockback velocity from the other side").
///
/// **Deliberately takes no `is_baby`.** Checked against every species this
/// sim spawns babies for: the zombie family's own attribute builder and every breedable
/// animal's attribute builder set `max_health`/`attack_damage`/`armor`
/// identically regardless of age — only the hitbox
/// ([`species_shape`]/[`baby_dimensions`]) and, for the zombie family, the
/// movement speed ([`baby_speed_multiplier`]) actually differ. Threading a
/// parameter through that would change nothing for any modeled species is
/// the "vacuous species" this repo's own evidence section warns about;
/// re-check this comment before adding one, rather than assuming it is
/// missing.
/// Goat spawn horn state uses a pre-broken-horn roll:
/// a non-baby check gated on a `< 0.1` float draw, then
/// a coin flip to pick which horn — narrowed to "not a baby" being
/// unconditionally true here, since [`MobSim::spawn_species`] always spawns
/// adult-shaped (see that method's own doc comment). `(has_left, has_right)`,
/// both `true` for every non-goat species and for the roll's own miss.
///
/// `rng.next_int(2) == 0` supplies the boolean-from-bounded-int draw — the
/// same coin-flip shape [`raid::bonus_spawns`] already uses for its own
/// `nextInt(2)` roll, not a bit-identical transcription of Java's real
/// `nextBoolean` implementation.
fn goat_horn_spawn_roll(species_path: &str, rng: &mut SpawnRng) -> (bool, bool) {
    if species_path != "goat" || rng.next_f32() >= 0.1 {
        return (true, true);
    }
    if rng.next_int(2) == 0 { (false, true) } else { (true, false) }
}

fn combat_defaults(entity_type: &ResourceKey) -> (f32, f32, Defenses, f64) {
    let attrs = default_attributes(entity_type).unwrap_or_else(AttributeMap::new);
    let max_health = attr(&attrs, "max_health") as f32;
    let attack_damage = attr(&attrs, "attack_damage") as f32;
    let defenses = Defenses {
        armor: attr(&attrs, "armor") as f32,
        armor_toughness: attr(&attrs, "armor_toughness") as f32,
        ..Defenses::default()
    };
    let knockback_resistance = attr(&attrs, "knockback_resistance");
    (max_health, attack_damage, defenses, knockback_resistance)
}

/// [`SpawnRng`] exposes the `next_f32`/`next_int` draws required by
/// [`lodestone_entity::spawn_equipment`]. This local implementation keeps the
/// RNG dependency at the seam without exposing the server's concrete type to
/// the entity crate; the orphan rule permits it because `SpawnRng` is local.
impl EquipRandom for SpawnRng {
    fn next_f32(&mut self) -> f32 {
        SpawnRng::next_f32(self)
    }

    fn next_int(&mut self, bound: i32) -> i32 {
        SpawnRng::next_int(self, bound)
    }
}

/// Generic age-scale fallback: half size while a mob is a baby, full size
/// otherwise. It applies only when [`baby_dimensions`] has no species entry;
/// species-specific dimensions take precedence over this fallback.
const DEFAULT_BABY_AGE_SCALE: f32 = 0.5;

/// Species-specific baby dimensions (`width`, `height`) before the `SCALE`
/// attribute is applied. The table covers breedable passive animals, wolves,
/// and the zombie family; every other species uses
/// [`DEFAULT_BABY_AGE_SCALE`] against its base dimensions.
fn baby_dimensions(entity_type: &ResourceKey) -> Option<(f32, f32)> {
    Some(match entity_type.path() {
        // The zombie family (husk, zombified piglin, drowned, zombie villager)
        // each redeclare the identical literal for their own baby dimensions.
        "zombie" | "husk" | "zombie_villager" | "drowned" | "zombified_piglin" => (0.49, 0.98),
        // Shared by the cow and mooshroom species.
        "cow" | "mooshroom" => (0.45, 0.7),
        "sheep" => (0.45, 0.65),
        "pig" => (0.45, 0.45),
        "chicken" => (0.3, 0.4),
        "rabbit" => (0.24, 0.4),
        "wolf" => (0.3, 0.425),
        _ => return None,
    })
}

/// Baby-only movement uses a multiplicative speed factor of `1.5` for the
/// zombie family. Breedable animals in this simulation have no baby speed
/// factor; their hitboxes shrink instead. Every unlisted species uses `1.0`.
fn baby_speed_multiplier(entity_type: &ResourceKey) -> f64 {
    match entity_type.path() {
        "zombie" | "husk" | "zombie_villager" | "drowned" | "zombified_piglin" => 1.5,
        _ => 1.0,
    }
}

/// Resolves a species' body from the real 26.2 dimension census, folded with
/// its `attrs`' `SCALE`/`STEP_HEIGHT` — see [`SimMob::spawn_species`]'s own doc
/// comment for why this duplicates (rather than calls)
/// [`crate::resolve_mob_shape`]'s fold: that function takes a
/// `&dyn VersionAdapter` for a version-aware caller, but `MobSim` already
/// reads `lodestone_data` directly for its path/collision census, so there is
/// no adapter to thread through here.
///
/// `is_baby` selects [`baby_dimensions`]'s per-species literal, falling back
/// to [`DEFAULT_BABY_AGE_SCALE`] against the census base. The `SCALE`
/// attribute is applied once, uniformly, after either selection.
fn species_shape(entity_type: &ResourceKey, attrs: &AttributeMap, is_baby: bool) -> MobShape {
    let scale = attr(attrs, "scale") as f32;
    let step_height = attr(attrs, "step_height") as f32;
    let base = (entity_type.namespace() == "minecraft")
        .then(|| EntityType::from_name(entity_type.path()))
        .flatten()
        .map(entity_dimensions::base_dimensions);
    let (width, height) = if is_baby {
        baby_dimensions(entity_type).unwrap_or_else(|| {
            let (w, h) = base.map_or((0.6, 1.95), |d| (d.width, d.height));
            (w * DEFAULT_BABY_AGE_SCALE, h * DEFAULT_BABY_AGE_SCALE)
        })
    } else {
        base.map_or((0.6, 1.95), |d| (d.width, d.height))
    };
    let mut shape = MobShape::land(width * scale, height * scale);
    shape.max_up_step = step_height;
    shape.can_open_doors = species_can_open_doors(entity_type);
    shape.can_float = species_can_float(entity_type);
    for &(kind, malus) in species_malus_overrides(entity_type) {
        shape.malus_overrides.insert(kind, malus);
    }
    shape
}

/// Species whose setup unconditionally enables
/// door-opening in the pathfinder's node evaluator, folded here since
/// [`MobShape::land`]'s default (mirroring the evaluator's own field default)
/// is `false` and [`species_shape`] is the only production caller that could
/// ever set it otherwise.
///
/// The zombie family is deliberately **not** here: its
/// door-breaking behind a spawn-time regional-difficulty coin flip, not a
/// species constant, so it is rolled once per spawn in
/// [`MobSim::spawn_species`] instead. See `docs/mob-species-spawning.md` for
/// both the unconditional set below and the zombie-family roll's citation.
fn species_can_open_doors(entity_type: &ResourceKey) -> bool {
    matches!(
        entity_type.path(),
        "vindicator" | "villager" | "piglin" | "piglin_brute"
    )
}

/// Species whose setup installs float-on-liquid behavior (or calls the
/// navigator's float setter directly), so the pathfinder should treat
/// water as swimmable rather than avoided.
///
/// Deliberately excludes every aquatic species this sim spawns (`guardian`,
/// `elder_guardian`, `drowned`): their navigation always swims, so the flag is
/// structurally inert for them rather than merely unmodelled here.
/// Also excludes species with no ground navigation at all (`ghast`, `blaze`),
/// for the same reason. See `docs/mob-species-spawning.md`.
fn species_can_float(entity_type: &ResourceKey) -> bool {
    matches!(
        entity_type.path(),
        "bee" | "cat"
            | "chicken"
            | "cow"
            | "mooshroom"
            | "horse"
            | "donkey"
            | "mule"
            | "pig"
            | "rabbit"
            | "sheep"
            | "wolf"
            | "creeper"
            | "enderman"
            | "spider"
            | "cave_spider"
            | "witch"
            | "pillager"
            | "parrot"
            | "vindicator"
            | "villager"
    )
}

/// Per-species pathfinding-malus overrides, folded onto
/// [`PathType::malus`]'s default table by [`species_shape`]. A species not
/// listed carries no overrides, so the default table applies
/// unchanged — that is the correct answer for most species, not a gap.
///
/// Every entry comes from the species' setup data, including the base animal
/// `FIRE_IN_NEIGHBOR`/`FIRE` overrides folded into each animal-derived
/// species' arm below (this function has no separate "is an Animal" pass to
/// apply them in, so they are duplicated per arm exactly as each species'
/// setup chain applies them. See `docs/mob-species-spawning.md` for the full
/// measurement table.
fn species_malus_overrides(entity_type: &ResourceKey) -> &'static [(PathType, f32)] {
    match entity_type.path() {
        "bee" => &[
            (PathType::Fire, -1.0),
            (PathType::Water, -1.0),
            (PathType::WaterBorder, 16.0),
            (PathType::Cocoa, -1.0),
            (PathType::Fence, -1.0),
        ],
        "cat" | "cow" | "mooshroom" | "horse" | "donkey" | "mule" | "pig" | "rabbit"
        | "sheep" => &[(PathType::FireInNeighbor, 16.0), (PathType::Fire, -1.0)],
        "wolf" => &[
            (PathType::FireInNeighbor, 16.0),
            (PathType::Fire, -1.0),
            (PathType::PowderSnow, -1.0),
            (PathType::OnTopOfPowderSnow, -1.0),
        ],
        "chicken" => &[
            (PathType::FireInNeighbor, 16.0),
            (PathType::Fire, -1.0),
            (PathType::Water, 0.0),
        ],
        "parrot" => &[
            (PathType::FireInNeighbor, -1.0),
            (PathType::Fire, -1.0),
            (PathType::Cocoa, -1.0),
        ],
        "blaze" => &[
            (PathType::Water, -1.0),
            (PathType::Lava, 8.0),
            (PathType::FireInNeighbor, 0.0),
            (PathType::Fire, 0.0),
        ],
        "strider" => &[
            (PathType::Water, -1.0),
            (PathType::Lava, 0.0),
            (PathType::FireInNeighbor, 0.0),
            (PathType::Fire, 0.0),
        ],
        "guardian" | "elder_guardian" => &[(PathType::Water, 0.0)],
        "enderman" => &[(PathType::Water, -1.0)],
        "wither_skeleton" => &[(PathType::Lava, 8.0)],
        "drowned" => &[(PathType::Water, 0.0)],
        "zombified_piglin" => &[(PathType::Lava, 8.0)],
        "piglin" | "piglin_brute" | "villager" => {
            &[(PathType::FireInNeighbor, 16.0), (PathType::Fire, -1.0)]
        }
        "warden" => &[
            (PathType::UnpassableRail, 0.0),
            (PathType::Damaging, 8.0),
            (PathType::PowderSnow, 8.0),
            (PathType::Lava, 8.0),
            (PathType::Fire, 0.0),
            (PathType::FireInNeighbor, 0.0),
        ],
        _ => &[],
    }
}

/// Distance past which a lead snaps.
const LEASH_TOO_FAR_DIST: f64 = 12.0;

/// Distance past which the leash applies a pull after accounting for the
/// entities' bounding-box widths; the current seam uses the documented coarse
/// approximation in [`MobSim::tick_leashes`].
const LEASH_ELASTIC_DIST: f64 = 6.0;

/// Temptation search radius. The ranged attribute is bounded by
/// `0.0..=2048.0` and supplies this value to the temptation goal.
///
/// This value lives in the perception feed; the other ranges below are
/// per-goal-instance arguments and stay with their behavior.
const TEMPT_RANGE: f64 = 10.0;

/// Radius used by the roster's avoid-threat behaviors for cats, wolves, and
/// armadillos.
const AVOID_RANGE: f64 = 6.0;

/// The vertical half-extent of the avoid-threat search box: the box is
/// inflated by the horizontal search distance on X/Z but by a flat `3.0` on
/// Y, so a threat directly overhead is out of range sooner than
/// one to the side.
const AVOID_RANGE_Y: f64 = 3.0;

/// Breeding partner-search radius. Both the targeting range and bounding-box
/// inflation use `8.0`.
const BREED_RANGE: f64 = 8.0;

/// The horizontal/vertical box [`feed_perception`](MobSim::feed_perception)
/// pre-filters candidates to before handing them to a brain-driven mob's
/// [`lodestone_entity::brain::NearbyBrainEntity`] feed.
///
/// Deliberately wider than `NearestHostileSensor::RANGE` (`8.0`, in
/// `lodestone_entity::brain::sensor`): this coarse host-side cut keeps the
/// feed cheap to build, and the sensor applies its own range on top.
const NEARBY_HOSTILE_SCAN_RANGE: f64 = 16.0;
const NEARBY_HOSTILE_SCAN_RANGE_Y: f64 = 8.0;

/// How close two parents must be for the breeding goal to actually produce a
/// child — a squared-distance check against `9.0`.
/// Reused here to identify *which* other mob was the partner when resolving a
/// [`NavigatingMob::take_bred`] event, since by then both parents' love state
/// has already been cleared by `breed()` itself.
const BREED_DISTANCE_SQR: f64 = 9.0;

/// The follow-parent goal's search box, inflated `8.0` horizontal, `4.0`
/// vertical.
const FOLLOW_PARENT_RANGE: f64 = 8.0;
const FOLLOW_PARENT_RANGE_Y: f64 = 4.0;

/// The long-distance patrol goal's companion search box, inflated by `16.0`.
/// Isotropic in vanilla (one `inflate` argument covers all three axes), unlike
/// [`FOLLOW_PARENT_RANGE`]'s horizontal/vertical split.
const PATROL_COMPANION_RANGE: f64 = 16.0;

/// What [`MobSim`] needs to know about one connected player in order to feed
/// mob perception. See [`MobSim::set_players`].
///
/// Not `Copy`, because [`held_item`](Self::held_item) owns a [`ResourceKey`].
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerPerception {
    /// The player's current position.
    pub position: Vec3,
    /// The item the player is currently holding, if any — straight from their
    /// `PlayerInventory`'s selected hotbar slot.
    ///
    /// The **item itself** rather than a pre-computed "is this tempting?"
    /// boolean, because the answer is per-*species*: wheat tempts a cow and a
    /// sheep, a potato tempts only a pig, and pumpkin seeds only a chicken
    /// (see [`tempt_food`]). A boolean here would have to be either wrong for
    /// some species or computed once per (player, species) pair by the caller,
    /// which is the feed's job, not the producer's.
    pub held_item: Option<ResourceKey>,
    /// The player's normalised view direction. The gaze test
    /// uses [`lodestone_entity::ai::mob::is_in_view_cone`]
    /// takes this directly as its `look` argument. `Vec3::new(0.0, 0.0, 1.0)`
    /// (looking due "south") is the default when a producer has not resolved
    /// a real angle yet.
    pub view_direction: Vec3,
}

/// **Who** a connected player is, as the mob simulation needs to know it.
///
/// # Why both, and what each one is for
///
/// Ownership is keyed on the **uuid** and nothing else. Vanilla stores a tamed
/// animal's owner in its own owner-uuid metadata field, whose serializer is
/// the shared "optional entity reference" one; that resolves to
/// vanilla's entity-reference wire codec, which is just the uuid's own
/// stream codec — sixteen
/// raw bytes. The NBT form (the same entity-reference type's own store/read)
/// is the same uuid. So the uuid is what both the wire and the
/// save file demand, and it is also the only identity that *survives*: a runtime
/// entity id is reassigned on every reconnect, and this server derives an
/// offline-mode uuid from the username, so a pet's owner is still the same
/// person tomorrow.
///
/// The **entity id** is carried alongside because the rest of this sim's
/// identity vocabulary is `i32` entity ids — [`SimMob::attack_target_id`],
/// [`SimMob::owner_id`], [`EntitySnapshot`]'s ids — and a mob that wants to
/// exclude its owner from a target, or a snapshot that wants to name the owning
/// entity, cannot say so in uuids. Vanilla makes exactly this split:
/// its own entity-reference type stores the uuid and *caches* the resolved
/// live entity.
///
/// So: **the uuid is the identity; the entity id is the handle.** Storing only
/// the entity id would make ownership evaporate on reconnect; storing only the
/// uuid would make it unnameable to anything else in this crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlayerIdentity {
    /// The player's account uuid — what ownership is keyed on. In offline mode
    /// this is derived from the username, so it is stable across sessions.
    pub uuid: Uuid,
    /// The player's runtime entity id, valid for this session only.
    pub entity_id: i32,
}

/// One connected player as the mob simulation sees them: who they are, plus what
/// a mob can sense about them.
///
/// # Why this is a second type rather than two more fields on [`PlayerPerception`]
///
/// Because the two answer different questions and only one of them is
/// *perception*. A mob senses a position and a held item; it does not sense a
/// uuid. Ownership is a relation between a mob and a **person**, resolved by the
/// host, and putting the person inside the sense data would say that a mob can
/// see who you are.
///
/// The practical consequence is the useful one: `From<PlayerPerception>` yields a
/// view with **no** identity, which is exactly the honest state for a producer
/// that has not been taught to supply one. An unidentified player can still be
/// looked at and tempted; they simply cannot own anything, and no mob will ever
/// resolve them as an owner. That is a correct neutral default rather than a
/// wrong one — contrast keying ownership on a nil uuid, which would make every
/// unidentified player the owner of every pet tamed by any other.
#[derive(Debug, Clone, PartialEq)]
pub struct PerceivedPlayer {
    /// Who this player is, or `None` for a producer that supplies no identity.
    pub identity: Option<PlayerIdentity>,
    /// What a mob can sense about them.
    pub perception: PlayerPerception,
}

impl From<PlayerPerception> for PerceivedPlayer {
    fn from(perception: PlayerPerception) -> Self {
        Self {
            identity: None,
            perception,
        }
    }
}

/// What [`MobSim::interact`] did — vanilla's own success/consume/pass
/// interaction-result enum, narrowed to
/// the outcomes this crate can actually produce.
///
/// Richer than a `bool` because the caller has to do different things with each:
/// a tame attempt consumes the item whether it succeeded or not, a sit toggle
/// consumes nothing (a success that explicitly withholds the item), and a `Pass`
/// must fall through to whatever else a right-click does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InteractOutcome {
    /// Nothing on this mob responded. Vanilla's own pass-through result.
    Pass,
    /// A tame roll succeeded; the mob is now owned by the actor.
    Tamed,
    /// A tame roll failed. The item is still consumed — that is what makes taming
    /// cost bones rather than patience.
    TameFailed,
    /// A tame pet's owner toggled its sitting order. Consumes no item.
    SitToggled {
        /// The order's new value.
        sitting: bool,
    },
    /// The mob entered love mode; a partner in love within range will now breed
    /// with it.
    InLove,
    /// A hurt tame pet was healed. The arm that runs *instead* of breeding.
    Fed,
    /// A horse family member's `Temper` rose. Carries the new value, because the
    /// tame probability is a function of it and a caller that cannot read it
    /// cannot tell "fed" from "nearly tame".
    TemperRaised {
        /// The horse family's temper value after the gain.
        temper: i32,
    },
    /// A professioned villager's trade screen should open. The menu
    /// interaction takes precedence over the generic item-use chain and
    /// consumes no item, the same as [`SitToggled`](Self::SitToggled).
    OpenTrade {
        /// The villager's current profession — never `None`/`Nitwit`, which
        /// have no trades and never reach this arm (see
        /// [`MobSim::interact`]'s villager short-circuit).
        profession: villager::Profession,
        /// The villager's trade level, `1..=5` — how many levels of trades to
        /// accumulate (see `villager::trades::offers_up_to`).
        level: i32,
    },
    /// A mount interaction succeeded and the actor is aboard. The caller uses
    /// this outcome to send a passengers update; no item is consumed.
    Mounted,
    /// A golden apple starts conversion for a weakened zombie villager and
    /// consumes one item. The no-weakness arm (a plain success that does **not**
    /// reduce the stack) is reported as [`Pass`](Self::Pass) instead; see
    /// [`MobSim::interact`]'s zombie-villager short-circuit for why that
    /// simplification is disclosed rather than a distinct variant.
    ZombieVillagerConversionStarted,
    /// An empty-handed allay was given an item. The interaction consumes one
    /// item; [`MobSim::interact`] handles the surrounding carrying rules.
    ///
    /// **No server-side set-equipment encoder is available** — this crate's
    /// server protocol has no set-equipment producer at all (only a
    /// client-side decoder, for joining someone else's server), so the held
    /// item is real server-side state but is absent from client-visible snapshots.
    /// That absence is not specific to the allay: every mob's
    /// `NavigatingMob::main_hand_item` has the identical problem.
    ItemGiven,
    /// An allay duplicated itself after satisfying the dance, item, and
    /// cooldown gates. One item is consumed by the interaction.
    ///
    /// **Disclosed substitution**: this crate has no jukebox-playback producer,
    /// so the allay arm uses "has recently heard a note block" (the same
    /// [`SimMob::allay_liked_noteblock`] state `DELIVER` reads) as its dance
    /// signal. This keeps duplication tied to an observable event while
    /// documenting the missing playback state explicitly.
    AllayDuplicated,
}

impl InteractOutcome {
    /// Whether the interaction consumed one of the held item.
    ///
    /// `SitToggled`/`OpenTrade` are the exceptions: these successes explicitly
    /// withhold the item. A pet you sit
    /// down does not eat whatever you happened to be holding, and opening a
    /// trade screen is not an item-use call either.
    #[must_use]
    pub fn consumes_item(self) -> bool {
        !matches!(
            self,
            Self::Pass | Self::SitToggled { .. } | Self::OpenTrade { .. } | Self::Mounted
        )
    }

    // `ZombieVillagerConversionStarted` is *not* added to the `!matches!` list
    // above: it falls through to the default `true` arm, matching vanilla's
    // own one-item consume call on that branch.

    /// The particle type vanilla's matching entity-status broadcast would make the
    /// client spawn, or `None` for an outcome with no visual.
    ///
    /// Status `6` → smoke, `7` → heart (the tame-particle burst), `18`
    /// → heart (the love-mode burst, seven hearts, same visual).
    #[must_use]
    fn particle(self) -> Option<&'static str> {
        match self {
            Self::Tamed | Self::InLove | Self::AllayDuplicated => Some("minecraft:heart"),
            Self::TameFailed => Some("minecraft:smoke"),
            Self::Pass
            | Self::SitToggled { .. }
            | Self::Fed
            | Self::TemperRaised { .. }
            | Self::OpenTrade { .. }
            | Self::Mounted
            | Self::ZombieVillagerConversionStarted
            | Self::ItemGiven => None,
        }
    }
}

/// One vanilla taming-particle-shaped burst, as a `LEVEL_PARTICLES` packet.
///
/// Vanilla spawns seven particles client-side at a per-axis random offset
/// within the mob's own width/height, plus half a block of extra height,
/// with a Gaussian-distributed `* 0.02`
/// per-axis velocity. The `LEVEL_PARTICLES` packet carries the count and a
/// per-axis spread, so the same burst is expressed as one packet: seven
/// particles, spread half a block horizontally (vanilla's random-offset draw is
/// ±width/2 about the centre, and 1.0 is a rough stand-in for the mob's width),
/// centred half a block above the mob's feet.
fn taming_particles(particle: &str, pos: Vec3) -> crate::effects::WorldEffect {
    crate::effects::WorldEffect::Particles {
        particle: particle.to_owned(),
        pos: Vec3::new(pos.x, pos.y + 0.5, pos.z),
        offset: lodestone_model::Vec3f::new(0.5, 0.5, 0.5),
        // Vanilla's per-particle velocity is a Gaussian draw scaled by `0.02`, so the
        // burst barely drifts. `max_speed` is the packet's own scale for that.
        max_speed: 0.02,
        count: 7,
        long_distance: false,
    }
}

/// Who owns a tamed mob.
///
/// Two variants because the two are genuinely different relations rather than one
/// with a wider key. A player owner is a **uuid** — the identity carried on
/// the wire and in NBT
/// alike, and the only identity that survives a reconnect. A mob owner is a
/// runtime **entity id**, because nothing persists it and there is no uuid to
/// resolve; that flavour serves ownership questions such as sharing a grudge
/// with another mob of the same owner.
///
/// Collapsing them into one `i32` is what made ownership unable to name a player,
/// and collapsing them into one `Uuid` would require inventing uuids for mobs
/// that the wire would then never carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MobOwner {
    /// Another live [`SimMob`], by runtime entity id.
    Mob(i32),
    /// A player, by account uuid.
    Player(Uuid),
}

/// A parrot (or another shoulder-riding species) currently riding a shoulder
/// — [`MobSim::shoulder_riders`]' value type. The persistent format stores
/// only what [`MobSim::tick_shoulder_dismounts`] needs to respawn something
/// recognisable — a disclosed loss of the original's variant/health/name.
///
#[derive(Debug, Clone)]
struct ShoulderRider {
    /// What to respawn on dismount.
    entity_type: ResourceKey,
    /// The game tick this mob mounted. A dismount condition applies only after
    /// the 20-tick minimum ride.
    mounted_tick: u64,
}

/// What a lead is tied to: a player, another leashable mob, or a
/// fence-knot decoration entity. This sim has no non-living decoration-entity
/// concept ([`SimMob`] assumes health, an `AttributeMap` and a goal
/// selector, none of which a knot has), so a fence anchor is a bare
/// [`BlockPos`] rather than a spawned entity — see [`MobSim::try_leash_to_fence`]'s
/// own doc comment for what that costs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeashHolder {
    /// A player, by account uuid — resolved to a live position through
    /// [`MobSim::players`]' `identity`.
    Player(Uuid),
    /// Another live [`SimMob`], by runtime entity id.
    Mob(i32),
    /// A fence post, represented by its world position without a separate
    /// decoration entity.
    Fence(BlockPos),
}

/// The result of [`MobSim::try_leash`]. A caller can derive its packet response
/// without repeating the leash-specific branching.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeashOutcome {
    /// The mob is now leashed to the given holder. The caller must consume
    /// one `minecraft:lead` from the placer's hand.
    Attached,
    /// The mob was leashed to the interacting player and is now free. `true`
    /// means a `minecraft:lead` item was spawned at the mob's position; `false`
    /// means no item was spawned (the creative/infinite-materials arm). The
    /// caller supplies that distinction through `try_leash`'s `creative`
    /// parameter; this sim has no game-mode state of its own.
    Detached { dropped_lead: bool },
    /// Neither arm applied — not leashable, out of range, or the holder
    /// requested is not a fresh attach for an already-player-held mob.
    Refused,
}

/// Default creeper explosion radius (a flat byte constant, `3`),
/// used flat by
/// [`MobSim::tick`]'s detonation trigger. A charged creeper doubles this for a
/// lightning-charged creeper (an explosion-multiplier field set to `2.0` when
/// powered, `1.0` otherwise); `SimMob` has no
/// "powered" state anywhere in this crate (no lightning-charging is
/// implemented), so that multiplier is not modelled — a disclosed gap, not a
/// silent one.
const CREEPER_EXPLOSION_RADIUS: f32 = 3.0;

/// One live mob in the simulation: its [`NavigatingMob`] body and its own
/// [`GoalSelector`].
///
/// Configure it after spawning with [`add_goal`](SimMob::add_goal) and
/// [`set_attack_target`](SimMob::set_attack_target); observe it with
/// [`position`](SimMob::position) / [`path_searches`](SimMob::path_searches).
/// A live persistent grudge, resolved by the host.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Anger {
    /// The absolute [`MobSim::tick_count`] at which this grudge expires. The
    /// grudge is live while `tick_count < end_time`.
    end_time: u64,
    /// Where the offending entity was when the grudge was set. A position
    /// rather than an id because that is all
    /// [`MobController::angry_target`] carries; an identity relation would be
    /// needed to preserve a target entity, but this state stores only its
    /// position.
    target: Vec3,
}

/// Persistent-anger duration, in ticks, **inclusive at both ends**.
///
/// The duration is a seconds-based range of `[20, 39]` converted
/// to a uniform-int range of `[400, 780]` ticks — seconds convert to ticks by
/// multiplying by 20, so this is
/// already ticks. Identical for all four neutral species.
///
/// **Ticks, not seconds.** Sampling `[20, 39]` here would expire a grudge in
/// under two seconds; `anger_expires_inside_the_jars_tick_window` separates
/// those two hypotheses explicitly rather than asserting a grudge merely ends.
const ANGER_TICKS: (u64, u64) = (400, 780);

/// Zombified-piglin alert interval — a seconds-based range of
/// `[4, 6]` converted to
/// `[80, 120]` ticks, the throttle on the piglin's own private
/// alert-others step. Deliberately **not** [`ANGER_TICKS`]: it is a different
/// window and this mechanism never reuses the shared grudge-duration value.
const PIGLIN_ALERT_INTERVAL_TICKS: (i32, i32) = (80, 120);

/// Time window, in ticks, during which a player's hit counts toward a mob's
/// death experience.
const PLAYER_HURT_EXPERIENCE_TIME: u64 = 100;

/// Default ambient-sound interval (`80`) — the forced gap
/// [`roll_ambient_sound`] enforces after an idle vocalisation fires (and
/// after a hurt sound, via [`MobSim::note_vocalisation`]) before the
/// per-tick chance of firing again starts climbing from zero.
const AMBIENT_SOUND_INTERVAL: i32 = 80;

/// Armadillo damage sets a "danger detected recently"
/// memory with an 80-tick expiry — the ticks [`SimMob::armadillo_danger_ticks`] is (re)set to on
/// every hit that passes the invulnerability gate.
const ARMADILLO_DANGER_TICKS: i32 = 80;

/// Axolotl play-dead duration (`200` ticks) — the timer value on a successful
/// roll.
const AXOLOTL_PLAY_DEAD_TICKS: i32 = 200;

/// An independent, deterministic per-*hit* approximation of the two
/// bounded-int-under-3 draws used by the axolotl play-dead roll — the same
/// "no shared RNG stream reaches this seam" shape [`camel_sit_roll`]'s own
/// doc discloses, salted from the hit itself (the mob's id plus the
/// pre-hit health and raw-damage bit patterns) rather than from a tick
/// counter, since this fires once per hit rather than once per tick. The
/// two draws are mixed with different constants so they do not correlate.
/// Returns `(nextInt(3), nextInt(3))` in declaration order; the caller reproduces
/// `first == 0 && (second < damage || health_ratio < 0.5)` itself.
fn axolotl_play_dead_roll(id: u64, health_bits: u32, damage_bits: u32) -> (u32, u32) {
    let seed = (u64::from(health_bits) << 32) | u64::from(damage_bits);
    let mix1 = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(id)
        .wrapping_mul(1_442_695_040_888_963_407)
        >> 33;
    let mix2 = seed
        .wrapping_mul(2_862_933_555_777_941_757)
        .wrapping_add(id ^ 0x9E37_79B9_7F4A_7C15)
        .wrapping_mul(3_935_559_000_370_003_845)
        >> 33;
    ((mix1 % 3) as u32, (mix2 % 3) as u32)
}

/// Allay "forget a heard note block" timer — the literal `600` a heard note
/// block's cooldown memory is (re)set to.
const ALLAY_NOTEBLOCK_COOLDOWN_TICKS: i32 = 600;

/// Allay self-duplication cooldown constant.
const ALLAY_DUPLICATION_COOLDOWN_TICKS: i32 = 6000;

/// One-slot container max stack size — the allay's inventory holds at
/// most one stack of whatever it is carrying.
const ALLAY_INVENTORY_MAX: u32 = 64;

/// Allay item-pickup reach. The seam has no bounding box to inflate, so this
/// radius is chosen generously
/// enough that a flying allay actually reaches ground items in its path
/// without this crate's plain squared-distance check missing an item a real
/// box-overlap test would have caught. A disclosed narrowing, the same
/// species as [`RamTarget::CONTACT_RANGE`]'s own "no bounding box on this
/// seam" cut.
const ALLAY_ITEM_PICKUP_RADIUS: f64 = 1.5;

/// Item delivery uses a close-enough/too-far band and throws while moving;
/// this seam's [`MobSim::allay_deliver_items`] instead
/// drops the instant the allay is within this distance of its liked
/// note block's "one above" cell, so the mob is standing there when this
/// fires.
const ALLAY_DELIVER_ARRIVAL_DISTANCE: f64 = 2.5;

/// Random-sitting camel minimum pose time (20
/// seconds, converted to ticks) — the minimum
/// ticks a camel must hold its current pose before [`camel_random_sitting`]
/// is eligible to flip it again in either direction. The toggle is gated by
/// this duration regardless of which way it is about to flip.
const CAMEL_RANDOM_SITTING_MIN_TICKS: i64 = 400;

/// Sitting-pose ordinal `10`, which this codebase's `pose_from_id`
/// maps to `EntityPose::Sitting` in the protocol metadata. Reused here for a species other
/// than the warden, which is the only other current `MetadataField::Pose`
/// producer.
const CAMEL_POSE_SITTING: u32 = 10;

/// Default standing pose ordinal, `0`.
const CAMEL_POSE_STANDING: u32 = 0;

/// Camel dash-cooldown constant — the reset value applied on a rider-triggered
/// jump, and the gate the dash handler checks
/// (cooldown at or below zero) before a new dash can start.
const CAMEL_DASH_COOLDOWN_TICKS: i32 = 55;

/// Camel dash minimum duration — the fixed duration used by the dash state. The
/// "is dashing" state stays `true` until
/// the cooldown drops under `50` and the camel is grounded, in a liquid, or
/// carrying a passenger,
/// i.e. until the camel has travelled for at least this many ticks *and*
/// has landed — but a client-authoritative mount reports no on-ground state to
/// this seam at all (nothing here simulates a ridden mob's physics; see
/// `lodestone_physics::vehicle`'s module doc), so
/// [`SimMob::camel_is_dashing`] uses this minimum alone as a disclosed
/// stand-in for the real landing-triggered reset.
const CAMEL_DASH_MINIMUM_DURATION_TICKS: i32 = 5;

/// An independent, deterministic per-tick coin flip for random-sitting camel
/// behavior. The reference behavior re-rolls this choice (one
/// of four equally-weighted idle behaviours) only when no walk target is
/// set — a brain-internal signal `MobSim` cannot read, since installing a
/// [`BrainGoal`](lodestone_entity::brain::BrainGoal) into a mob's goal
/// selector is one-way (see this file's own doc on why the sim has no read
/// into it). So this is a disclosed simplification, not a transcription: an
/// independent per-tick draw, salted differently from
/// [`roll_ambient_sound`]'s hash so the two streams do not correlate for a
/// camel that is eligible for both in the same tick. `% 2400` is a local
/// simulation constant, chosen only to keep
/// the expected wait (~2 minutes once eligible) long enough to read as a
/// deliberate rest rather than a flicker.
fn camel_sit_roll(id: u64, tick_count: u64) -> bool {
    let mix = tick_count
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(id)
        .wrapping_mul(1_442_695_040_888_963_407)
        >> 33;
    mix % 2400 == 0
}

/// Random-sitting camel behavior runs for a live camel not already forced to
/// stand by [`MobSim::tick`]'s water check (see that call
/// site's doc). Flips [`SimMob::camel_sitting`] when eligible and
/// [`camel_sit_roll`] fires.
///
/// Gated on the conditions this sim can see: at least
/// [`CAMEL_RANDOM_SITTING_MIN_TICKS`] since the last pose change, not
/// leashed, and not ridden (`m.rider`, the same field
/// [`MobSim::tick`]'s goal-tick gate already reads for "is something else
/// driving this mob's movement"). **Disclosed simplification**: the simulation
/// also omits grounded and panicking checks; `on_ground` has no
/// per-tick reading for a walking land mob in this sim, and `is_panicking`
/// exists but is deliberately left out here — it already forces a mob's
/// *movement* into fleeing, so a hurt camel refuses to path toward a sit target
/// without this function needing to duplicate that gate.
fn camel_random_sitting(m: &mut SimMob<'_>, tick_count: u64) {
    if m.is_leashed() || m.rider.is_some() {
        return;
    }
    let pose_time = tick_count as i64 - m.camel_pose_tick;
    if pose_time < CAMEL_RANDOM_SITTING_MIN_TICKS {
        return;
    }
    if camel_sit_roll(m.id as u64, tick_count) {
        m.camel_sitting = !m.camel_sitting;
        m.camel_pose_tick = tick_count as i64;
    }
}

/// The flat knockback power applied to
/// **every** damaging hit, regardless of the attacker's own
/// `minecraft:attack_knockback` attribute — a fixed `0.4` power fed into the
/// knockback impulse. This is separate from,
/// and applied before, any attacker-specific bonus (sprint attack,
/// enchantments) — see [`MobSim::attack`]'s own doc comment for why the two
/// are chained as two `knockback_impulse` calls rather than summed into one.
const MELEE_DEFAULT_KNOCKBACK_POWER: f64 = 0.4;

/// Wraps a bare species path back into a [`ResourceKey`] so
/// [`mob_experience_reward`] can consult [`is_hostile_species`], which takes one.
///
/// A parse rather than a second copy of that function's species list, for the reason
/// that list's own doc gives: a duplicated hostility table is one more thing to go
/// stale. An unparseable path answers "not hostile", which lands on the documented
/// `0` fallback.
fn hostile_probe(path: &str) -> ResourceKey {
    ResourceKey::new_borrowed("minecraft", path).unwrap_or_else(|_| item_entity_type())
}

/// One draw from [`ANGER_TICKS`] using inclusive uniform-int sampling:
/// `lo + nextInt(hi - lo + 1)`.
///
/// The `+ 1` is the inclusive upper bound, and dropping it is the classic
/// off-by-one that makes 780 unreachable — a difference no "does the grudge
/// expire" assertion could see.
fn grudge_ticks(mob: &mut impl MobController) -> u64 {
    let (lo, hi) = ANGER_TICKS;
    let span = i32::try_from(hi - lo + 1).expect("the anger window fits in i32");
    lo + u64::try_from(mob.next_i32(span)).unwrap_or(0)
}

/// One draw from [`PIGLIN_ALERT_INTERVAL_TICKS`], same `lo + nextInt(hi - lo
/// + 1)` shape as [`grudge_ticks`] and the same off-by-one reasoning applies.
fn piglin_alert_interval(mob: &mut impl MobController) -> i32 {
    let (lo, hi) = PIGLIN_ALERT_INTERVAL_TICKS;
    lo + mob.next_i32(hi - lo + 1)
}

/// Whether `species` propagates a grudge to nearby same-species mobs when it
/// is newly hurt, and if so the alert box's half-extents and whether the
/// alerted mob's owner must match the victim's.
///
/// Zombified piglins use an alert box of **±35 XZ, ±10 Y** with no owner
/// filter. Wolves use **±16 XZ, ±10 Y** and require the same owner UUID,
/// because the two species have different group-alert rules. The owner filter
/// applies only to wolves; see `docs/plans/mob-ai-roster.md` and
/// `roster::neutral` for the range derivations.
///
/// This one-shot path has **no line-of-sight check**. `RayView::is_clear` (used by
/// `crate::explosion`'s exposure sampling and `crate::mobs::projectiles`)
/// would be the primitive if one were ever needed here.
///
/// **A second, wholly separate mechanism also propagates piglin aggro:** the
/// zombified-piglin tick step makes an alert call every tick while the piglin
/// has a target, throttled
/// by its own alert-interval range of `[80, 120]` ticks
/// ([`PIGLIN_ALERT_INTERVAL_TICKS`], deliberately not the shared grudge
/// window `ANGER_TICKS` reuses) and gated on live line of sight to the
/// piglin's *current* target — so a piglin pack keeps growing every couple of
/// seconds as long as the alerting piglin can still see whoever it is
/// chasing, not just once at acquisition. [`MobSim::attack`]'s "only on a
/// *new* grudge" gate covers the one-shot "alert others of the same owner"
/// goal (accurately,
/// per the paragraph above); this ongoing one is a second, independent
/// producer, resolved in [`MobSim::tick`]'s own per-mob loop
/// ([`SimMob::piglin_alert_ticks`] carries the throttle) and applied to the
/// rest of `self.mobs` afterwards, reusing this function's own box for the
/// propagation. The disclosed approximation is that this seam's
/// sensing-based line-of-sight-to-target check wants a live entity reference this seam does
/// not carry (see [`MobController::angry_target`]'s own doc for why), so the
/// line-of-sight check and the alerted position both read
/// `MobController::attack_target` instead — the position the piglin's own
/// anger-gated target row last fed it, not a continuously-refreshed live
/// target.
fn alert_species(species_path: &str) -> Option<(f64, f64, bool)> {
    match species_path {
        "zombified_piglin" => Some((35.0, 10.0, false)),
        "wolf" => Some((16.0, 10.0, true)),
        _ => None,
    }
}

/// One draw of the bee self-destruct roll, evaluated once
/// every 5th tick since the sting connected (`elapsed % 5 == 0`, checked by
/// the caller): a bounded-int draw against `clamp(1200 - elapsed, 1, 1200)`, testing for zero.
///
/// The clamp is what bounds it at both ends — at `elapsed == 1200` the
/// divisor is `1` and the roll is unconditional, and `1200` is itself a
/// multiple of `5` so that tick is never skipped; **a stung bee is certainly
/// alive one tick after the sting** (elapsed `1`, not a multiple of 5, so the
/// caller never calls this at all) **and certainly dead by 1200 ticks after
/// it**. See `roster::neutral::BEE`'s own doc comment for the citation this
/// was derived from.
///
/// Deterministic and independent of the mob's own [`MobController`] RNG
/// stream, for the same reason [`roll_ambient_sound`] avoids it: this can
/// fire on a cadence unrelated to that stream's own consumers, and drawing
/// from the shared stream here would shift every subsequent AI draw. Not a
/// faithful `java.util.Random` reproduction — a multiplicative hash of
/// `(tick_count, id, elapsed)`, the same style already used for the ambient
/// sound pitch/timing rolls in this module.
fn bee_sting_death_roll(tick_count: u64, id: i32, elapsed: u64) -> bool {
    if elapsed >= 1200 {
        return true;
    }
    let denom = (1200 - elapsed).max(1);
    let mix = tick_count
        .wrapping_mul(2_654_435_761)
        .wrapping_add(id as u64)
        .wrapping_mul(40_503)
        .wrapping_add(elapsed.wrapping_mul(2_246_822_519));
    mix % denom == 0
}

/// One tick of the generic idle-vocalisation roll:
/// if alive and a bounded-int-under-1000 draw is less than the ambient-sound
/// timer (post-increment), reset the timer and play the ambient sound.
/// The timer starts
/// at `0` and climbs by one every tick that does not fire, so the per-tick
/// chance of firing ramps from `0` toward certainty rather than being a flat
/// roll; firing resets it to `-`[`AMBIENT_SOUND_INTERVAL`], enforcing a hard
/// cooldown before the ramp restarts (mirrored on a hurt sound too, in
/// [`MobSim::note_vocalisation`]).
///
/// **Must not draw from the mob's own [`MobController`] RNG stream.** That
/// stream is also what every AI goal draws from (wander targets, look
/// timers, …), and this roll runs unconditionally every tick for every live
/// mob — consuming from it here would shift every subsequent AI draw by
/// however many calls this makes, exactly the failure mode
/// [`MobSim::note_vocalisation`]'s own doc already flags for the same
/// reason: "consuming from a shared generator here would shift every other
/// draw." Mixed from `tick_count` and the mob's id instead, the same
/// approach that function already uses.
fn roll_ambient_sound(m: &mut SimMob<'_>, tick_count: u64) -> Option<crate::effects::WorldEffect> {
    let id = m.id as u64;
    // A small multiplicative hash of (tick_count, id) — not vanilla's RNG,
    // deterministic and cheap, and critically independent of the mob's own
    // `next_i32`/`next_f32` stream. `% 1000` matches `random.nextInt(1000)`'s
    // range.
    let mix = tick_count
        .wrapping_mul(2_654_435_761)
        .wrapping_add(id)
        .wrapping_mul(40_503);
    let roll = (mix % 1000) as i32;
    let fired = roll < m.ambient_sound_time;
    m.ambient_sound_time += 1;
    if !fired {
        return None;
    }
    m.ambient_sound_time = -AMBIENT_SOUND_INTERVAL;
    // Vanilla's own generic voice-pitch formula: `(rand - rand) * 0.2 + centre`,
    // centre `1.5` for a baby and `1.0` for an adult — unlike the hurt/death
    // pitch, which vanilla draws from a *different*, baby-blind formula (see
    // `MobSim::note_vocalisation`'s own doc for why that one differs). Same
    // tick_count/id phase approach `note_vocalisation` already uses for its
    // own pitch, so both land in `[centre - 0.1, centre + 0.1]`.
    let phase = (tick_count.wrapping_mul(31).wrapping_add(id)) % 21;
    let centre = if m.is_baby() { 1.5 } else { 1.0 };
    let pitch = centre - 0.1 + phase as f32 * 0.01;
    crate::effects::mob_ambient_sound(
        m.entity_type.to_string().as_str(),
        m.position(),
        m.category == MobCategory::Monster,
        pitch,
        tick_count as i64,
    )
}

#[derive(Debug)]
pub struct SimMob<'w> {
    id: i32,
    mob: NavigatingMob<'w>,
    goals: GoalSelector,
    category: MobCategory,
    /// Vanilla's own "no action time" field: ticks since the mob last "did something".
    /// Advanced each [`MobSim::tick`] and consulted by the despawn gates; reset
    /// when the mob is within a player's immune radius.
    no_action_time: i32,
    /// Whether the mob is exempt from natural despawn (named, persistence-
    /// required, or a persistent category). Persistent mobs skip the gates.
    persistent: bool,
    /// Stable identity for the mob's sim-entry lifetime, encoded verbatim in the
    /// spawn packet. Assigned once at [`MobSim::spawn`].
    uuid: Uuid,
    /// Canonical entity-type key (e.g. `minecraft:zombie`). The sim spawns mobs
    /// by spawn-rule [`MobCategory`], not species, so this is a documented
    /// placeholder (defaulting to `minecraft:zombie`, matching the default
    /// `Monster` category) until species-aware spawning lands; a consumer that
    /// knows the species sets it with [`set_entity_type`](SimMob::set_entity_type).
    entity_type: ResourceKey,
    /// Equipment assigned during species spawning. This is retained as owned
    /// simulation state so plugin observations read the same values that
    /// combat attributes and ranged goals consume.
    equipment: spawn_equipment::EquipmentSlots,
    /// Current health. A hit that drives this to `0.0` removes the mob from
    /// the sim at the end of the tick that landed it (vanilla's immediate
    /// death removal).
    health: f32,
    /// The `minecraft:max_health` attribute value resolved at spawn — the ceiling
    /// [`heal`](SimMob::heal) clamps to.
    ///
    /// Recorded rather than re-resolved because it is what decides the *order* of
    /// a tame pet's interaction arms: vanilla's own wolf interaction handler feeds a hurt pet
    /// (food item held, and current health below max) and only falls through to
    /// the breeding and sit arms when it cannot. Without the ceiling that
    /// condition is unanswerable and the arms silently reorder.
    max_health: f32,
    /// Armour/resistance/absorption state `damage::apply_reductions` reads for
    /// every incoming hit; absorption is written back after each hit.
    defenses: Defenses,
    /// Remaining-fire-ticks state — see `crate::burning`'s own module
    /// doc for the full mechanic. Currently only ever raised by a fireball's
    /// impact (`MobSim::resolve_projectile_hit`) and consumed by
    /// [`MobSim::tick_burning`]; standing in a fire/lava block does not yet
    /// ignite a mob because this path has no block-state fire source.
    burn: crate::burning::BurnState,
    /// Persistent-anger state, host-side (the anger deadline):
    /// the **absolute game tick** the grudge ends at, plus where the entity it
    /// is held against was when it was set.
    ///
    /// `None` means no live grudge; the wire-facing representation uses a
    /// sentinel `-1` end time.
    ///
    /// **A deadline, not a countdown.** 26.2 stores an absolute game time and
    /// compares against it; a decrementing counter
    /// drifts against a stepped tick loop. The comparison is against
    /// [`MobSim::tick_count`], which is the only clock this sim has.
    ///
    /// This lives on the host rather than on `NavigatingMob` because
    /// [`MobController::angry_target`] is deliberately an *answer*, not a
    /// query: the seam has no shared clock, so the host resolves expiry and
    /// only `Option<Vec3>` crosses. See that method's own doc comment.
    anger: Option<Anger>,
    /// A bee's sting is expressed as the [`MobSim::tick_count`] when the sting
    /// connected rather than a bare flag, since the self-destruct roll needs
    /// elapsed time and a decrementing counter would drift against a stepped
    /// tick loop for the same reason
    /// [`Anger::end_time`] is an absolute deadline rather than a countdown.
    ///
    /// `None` for a bee that has never stung. Set when a bee's attack connects
    /// and never cleared; a stung bee remains on a path to death.
    stung_at: Option<u64>,
    /// Vanilla's own "ticks until next alert" field — the throttle on the piglin's
    /// own private alert-others call, a second, wholly separate group-aggro
    /// mechanism from [`Anger`]'s one-shot [`alert_species`] propagation on a
    /// *new* grudge: this one re-fires every `[80, 120]`-tick interval for as
    /// long as the piglin keeps a target, which is what makes a real piglin
    /// pack keep growing every few seconds while chasing a player rather than
    /// only once at the first hit.
    ///
    /// A countdown rather than a deadline (unlike [`Anger::end_time`]) because
    /// vanilla's own field is one: `ticksUntilNextAlert` decrements every tick
    /// and is redrawn each time it bottoms out, with no absolute-time
    /// semantics to preserve. `-1` is this crate's "no active timer" sentinel
    /// (vanilla has no equivalent — a fresh piglin's field starts at `0`,
    /// which fires on its very first tick with a target; this sentinel
    /// instead rolls a fresh interval with no immediate fire, a disclosed
    /// simplification since the seam has no "did I have a target last tick"
    /// signal to detect the true acquisition edge from). Reset to `-1` the
    /// moment [`MobController::attack_target`] goes empty, so a piglin that
    /// loses and later reacquires a target rerolls rather than resuming a
    /// stale countdown.
    piglin_alert_ticks: i32,
    /// The armadillo's own "danger detected recently" memory, collapsed to a plain
    /// countdown — the real jar tracks a rolling/scared/unrolling
    /// animation sub-state machine (10/50/30-tick phases) purely for the
    /// client-visible roll animation, but the **gameplay** consequences
    /// (halved incoming damage, no love, no ambient sound, no
    /// player interaction beyond a brush) are identical across all three
    /// phases and keyed on one thing — whether this timer is still running.
    /// `0` is idle (matches the idle state's absence of danger memory);
    /// any positive value is "scared" in the collapsed sense.
    ///
    /// Set to [`ARMADILLO_DANGER_TICKS`] by every hit that passes the
    /// invulnerability gate ([`SimMob::apply_damage`]) — vanilla's own
    /// hurt handler unconditionally refreshes the memory to 80 ticks and
    /// rolls the armadillo up (a no-op if already scared), so a second hit while
    /// still curled keeps the timer topped up rather than letting it run
    /// down mid-fight. Decremented once per [`MobSim::tick`] for every live
    /// armadillo. `0` for every non-armadillo species, where nothing reads
    /// it.
    ///
    /// **Disclosed narrowing**: real vanilla's hurt handler only refreshes this for a
    /// living-entity attacker and rolls back out early for an
    /// environmental (tagged) one; this seam's
    /// `apply_damage` has no attacker-identity or damage-type-tag input to
    /// discriminate on (the same simplification this function's own
    /// `note_hurt` comment already discloses for panic), so *any* damage an
    /// armadillo takes triggers/refreshes it. **Also disclosed**: real
    /// vanilla's "can stay rolled up" check additionally refuses to roll up while panicking, in
    /// a liquid, leashed, ridden or a rider — none of those gates are
    /// checked here.
    armadillo_danger_ticks: i32,
    /// The axolotl's own "playing dead" brain memory countdown, collapsed to a plain
    /// countdown — the same shape [`armadillo_danger_ticks`]'s own doc
    /// already establishes for a memory this crate has no brain-timer
    /// primitive to host directly. `0` is idle; any positive value means
    /// [`SimMob::axolotl_is_playing_dead`] reports `true`. Set to
    /// [`AXOLOTL_PLAY_DEAD_TICKS`] by [`SimMob::apply_damage`]'s own
    /// `axolotl_play_dead_roll` gate, decremented once per [`MobSim::tick`]
    /// for every live axolotl. `0` for every non-axolotl species, where
    /// nothing reads it.
    axolotl_play_dead_ticks: i32,
    /// The camel's own "is sitting" state — real, client-visible
    /// sitting pose (see [`CAMEL_POSE_SITTING`]). Toggled by
    /// [`camel_random_sitting`]'s own per-tick approximation of
    /// vanilla's own random-sitting camel behaviour, and forced back to `false` the instant this
    /// camel enters water (vanilla's own per-tick update stands a sitting
    /// camel up instantly on contact with water). `false`
    /// for every non-camel species, where nothing reads it.
    camel_sitting: bool,
    /// The [`MobSim::tick_count`] this camel's sit state last changed —
    /// gates [`CAMEL_RANDOM_SITTING_MIN_TICKS`], the collapsed stand-in for
    /// vanilla's own pose-time getter. `0` for every non-camel species.
    camel_pose_tick: i64,
    /// The camel's own dash-cooldown field — set to [`CAMEL_DASH_COOLDOWN_TICKS`] by
    /// [`MobSim::trigger_camel_dash`] (vanilla's own rider-jump handler) and
    /// decremented once per [`MobSim::tick`] for every live camel, exactly
    /// like [`axolotl_play_dead_ticks`]'s own countdown shape.
    /// [`SimMob::camel_is_dashing`] derives the client-visible dash
    /// flag from this counter. `0` for every non-camel species.
    camel_dash_cooldown: i32,
    /// The sniffer's own state metadata field — this mob's current phase in the seek/dig/rise
    /// loop. `SnifferState::Idling` for every non-sniffer species, where
    /// nothing reads it. See [`sniffer`] module doc for the whole state
    /// machine.
    sniffer_state: sniffer::SnifferState,
    /// Ticks remaining in [`sniffer_state`](Self::sniffer_state) — a timed
    /// state's own countdown ([`sniffer::SNIFFING_MIN_TICKS`]..=`MAX`,
    /// [`sniffer::DIGGING_MIN_TICKS`]..=`MAX`, [`sniffer::RISING_TICKS`]) or
    /// [`sniffer::SEARCHING_TIMEOUT_TICKS`] while `Searching`. Meaningless
    /// while `Idling`.
    sniffer_state_ticks: i32,
    /// The sniffer's own sniff-cooldown field — [`sniffer::SNIFF_COOLDOWN_TICKS`] set by
    /// [`sniffer::MobSim::tick_sniffers`] once a dig finishes, gating the
    /// next sniff. `0` for every non-sniffer species.
    sniffer_sniff_cooldown: i32,
    /// A host-found candidate dig position, present only during
    /// `SnifferState::Searching` — fed to this mob's own brain each tick
    /// through `BrainMob::sniffer_dig_target`
    /// ([`MobSim::feed_perception`]'s own sniffer line), which is *all* the
    /// brain sees of this state machine. See [`sniffer`] module doc for the
    /// division of labour.
    sniffer_dig_target: Option<Vec3>,
    /// Vanilla's own explored-positions getter — up to
    /// [`sniffer::EXPLORED_POSITIONS_CAP`] positions this sniffer has
    /// already dug, most recent first, so [`sniffer::MobSim::tick_sniffers`]'s
    /// own dig-position search does not repeat one. Empty for every
    /// non-sniffer species.
    sniffer_explored: Vec<Vec3>,
    /// Vanilla's own allay-brain "liked note block position"/"cooldown"
    /// memory pair, collapsed into one field: `Some((pos, ticks))` while a
    /// heard note block is still "recent" (`ticks > 0`), `None` otherwise.
    /// Written by [`MobSim::resolve_vibrations`]'s allay arm
    /// (vanilla's own "heard a note block" handler), decremented once per [`MobSim::tick`] for
    /// every live allay, and cleared outright once the countdown reaches
    /// zero — a disclosed simplification of vanilla's own split (real
    /// vanilla's own liked-position memory lingers with no TTL of its own; only the
    /// cooldown memory expires, and its own deposit-position getter erases the
    /// position separately the next time it is read as ineligible). `None`
    /// for every non-allay species.
    allay_liked_noteblock: Option<(Vec3, i32)>,
    /// Vanilla's own one-slot inventory container collapsed to
    /// a plain count — every picked-up item is, by construction
    /// (vanilla's own item-equality gate for pickup eligibility), the same item as
    /// [`MobController::main_hand_item`], so only a count is needed, not a
    /// second item identity. Filled by [`MobSim::allay_pick_up_items`],
    /// drained one at a time by [`MobSim::allay_deliver_items`]. `0` for
    /// every non-allay species.
    allay_inventory_count: u32,
    /// Vanilla's own allay duplication-cooldown field, ticks remaining before
    /// [`MobSim::interact`]'s duplication arm can fire again. `0` for every
    /// non-allay species (and every allay not currently on cooldown).
    allay_duplication_cooldown: i32,
    /// The [`MobSim::tick_count`] at which this mob stops counting as
    /// player-killed, expressed as an absolute deadline for [`Anger`]'s reason.
    ///
    /// **This is the gate on XP dropping at all.** The drop-experience path requires
    /// this deadline to be set, so a mob that starves, drowns, burns, falls
    /// or is killed by another mob drops **no** experience — only a kill a player had
    /// a hand in within [`PLAYER_HURT_EXPERIENCE_TIME`] ticks does. Awarding
    /// unconditionally would turn any mob farm into an XP farm and is the plausible
    /// simplification to avoid.
    ///
    /// `None` for a mob no player has ever hit.
    hurt_by_player_until: Option<u64>,
    /// Raw melee damage this mob's own attacks deal (`ATTACK_DAMAGE`
    /// attribute), applied to the target named by
    /// [`attack_target_id`](SimMob::attack_target_id) when an attack connects.
    attack_damage: f32,
    /// The invulnerability-frame gate for hits landing on *this* mob
    /// (`damage::HurtCooldown`), ticked once per sim tick regardless of
    /// whether anything hit this tick.
    hurt_cooldown: HurtCooldown,
    /// Ambient-sound time: an increasing-probability countdown for
    /// this mob's idle vocalisation (cow moo, zombie groan, …), ticked once
    /// per sim tick regardless of whether a goal moved this mob. Starts at
    /// `0` — see
    /// [`MobSim::roll_ambient_sound`] for the roll this drives.
    ambient_sound_time: i32,
    /// The id of another live [`SimMob`] this mob's melee attacks should
    /// damage, set alongside [`set_attack_target`](SimMob::set_attack_target)'s
    /// `Vec3` (which only drives movement — the goal/navigation seam has no
    /// entity identity, just positions).
    attack_target_id: Option<i32>,
    /// Who owns this mob, if anyone — the ownership relation. A tamed animal's
    /// owner is a **player** uuid, which is expressible here:
    /// [`PerceivedPlayer`] carries a [`PlayerIdentity`] at the perception seam. The
    /// mob-to-mob flavour is kept because the enderman/wolf-pack work needs it
    /// and nothing about a uuid replaces it.
    ///
    /// The seam carries the resolved *position*
    /// ([`MobController::owner_position`]); the identity lives here, because
    /// only a census can hold it.
    ///
    /// `None` for a wild mob.
    owner: Option<MobOwner>,
    /// Whether this mob is *tame at all*, independent of whether its owner is
    /// currently resolvable — the `0x04` bit
    /// of its shared entity-flags metadata field.
    ///
    /// Not derived from [`owner`](Self::owner) being `Some`, and this is the
    /// distinction that matters: a tamed pet whose owner has logged out keeps
    /// its `owner` (the uuid is durable) but has **no resolvable position**, and
    /// a mob-owned pet has an owner that is not a player at all. Both are tame.
    /// Deriving tameness from a *resolved* owner would un-tame every pet the
    /// moment its owner left the player list, and goals read this.
    tame: bool,
    /// Vanilla's own "ordered to sit" field — the sitting **intent** an owner's
    /// right-click toggles, which is what `SitWhenOrderedToGoal` reads. NBT
    /// round-trips it as `Sitting`.
    ///
    /// Kept here rather than only on the [`NavigatingMob`] because it is
    /// persisted state that outlives any goal, and because the interaction that
    /// toggles it is a host event, not a goal.
    ordered_to_sit: bool,
    /// Vanilla's own horse-family temper field — how close a horse family member is to accepting
    /// a rider, `0..=getMaxTemper()`. Raised by feeding
    /// ([`horse_temper_gain`]); read by the tame roll
    /// ([`MobSim::attempt_horse_tame`]).
    ///
    /// `0` for every species outside the horse family, where nothing reads it.
    temper: i32,
    /// `minecraft:knockback_resistance` attribute value (`0.0..=1.0`),
    /// `lodestone_physics::knockback::knockback_impulse`'s own
    /// `knockback_resistance` parameter for a hit landing on *this* mob. See
    /// [`combat_defaults`]'s doc comment for why this is not folded into
    /// [`Defenses`].
    knockback_resistance: f64,
    /// What a lead currently ties this mob to, if anything — vanilla's own
    /// leash-data holder type. `None` is vanilla's "no leash data present" case;
    /// there is no separate "has data but no holder" state modelled, since
    /// nothing in this sim needs the delayed-load half vanilla's own
    /// save-restore path
    /// exists for (persistence is a different crate's concern).
    leash_holder: Option<LeashHolder>,
    /// Vanilla's own "last lightning bolt uuid" mooshroom field — which bolt (by this sim's own
    /// lightning-bolt entity id, not a real UUID; see `mobs/lightning.rs`'s
    /// module doc for why) last toggled this mob's variant, so a bolt whose
    /// `hit_entities` fires across several ticks cannot flip the same mob
    /// twice. `None` means never struck. Read by no species today except the
    /// mooshroom guard itself — see that module's doc for why nothing yet
    /// consumes the toggle this guards.
    last_lightning_bolt: Option<i32>,
    /// This villager's profession; [`villager::Profession::None`] is the
    /// default
    /// for every non-villager species, and for a villager that has not
    /// claimed a workstation yet. Only meaningful when
    /// [`entity_type`](Self::entity_type) is `minecraft:villager`.
    profession: villager::Profession,
    /// The workstation block position [`profession`](Self::profession) was
    /// claimed from, if any — `None` for an unemployed villager or a
    /// non-villager. Cleared alongside `profession` reverting to `None` when
    /// [`MobSim::tick_villager_professions`] finds the claim gone.
    workstation: Option<BlockPos>,
    /// Vanilla's own villager-data trade level, `1..=5`. `1` for every non-villager and every
    /// freshly spawned villager (vanilla's own villager-data field default).
    villager_level: i32,
    /// Accumulated trading xp toward [`villager::max_xp_for_level`]'s next
    /// threshold. Vanilla's own villager-xp field.
    villager_xp: i32,
    /// This villager's persistent trade economics — per-offer demand, restock
    /// cadence and use counts, keyed alongside the `(profession, level)` it
    /// was built for so [`SimMob::ensure_trades`] can tell when it has gone
    /// stale. `None` for a non-villager or one with no profession yet.
    ///
    /// Rebuilding on a profession/level change (rather than merging in the
    /// newly unlocked tier) loses whatever demand/restock state the old
    /// tiers had accumulated — a real simplification, not vanilla's
    /// `updateTrades`, but strictly better than the previous state, which
    /// discarded that state on *every* menu open rather than only a tier
    /// change.
    trades: Option<(villager::Profession, i32, crate::villager_trade::VillagerTrades)>,
    /// Ticks until this mob's next job search, decremented in
    /// [`MobSim::tick_villager_professions`]. Throttles the bounded terrain
    /// scan [`villager::find_and_claim_workstation`] runs — see that
    /// function's own doc for why the scan itself is not free.
    job_search_cooldown: i32,
    /// Ticks until this mob's next chest/lit-furnace/bed search, decremented
    /// in [`MobSim::tick_cat_block_search`] — the same throttling shape as
    /// [`job_search_cooldown`](Self::job_search_cooldown), for the identical
    /// reason: a bounded terrain scan every tick for every cat is not free.
    /// Only meaningful for `minecraft:cat`.
    cat_search_cooldown: i32,
    /// Ticks since this mob last dismounted a shoulder ride (or was spawned)
    /// — vanilla's own shoulder-ride cooldown counter, incremented once
    /// per tick alongside [`no_action_time`](Self::no_action_time) and fed to
    /// [`MobController::ticks_since_shoulder_dismount`]. Only meaningful for
    /// `minecraft:parrot`; a species that never mounts a shoulder does not
    /// read it.
    shoulder_dismount_ticks: i32,
    /// The bed this villager has claimed as its home point-of-interest, if any — `None`
    /// for an unclaimed villager or a non-villager. Cleared alongside a
    /// ticket release when [`MobSim::tick_villager_beds`] finds the claim
    /// gone (destroyed, or no longer a bed). Native-only: meaningless (and
    /// never set) on `wasm32`, where [`villager::BedClaims`] does not exist.
    bed: Option<BlockPos>,
    /// Ticks until this mob's next bed search, decremented in
    /// [`MobSim::tick_villager_beds`] — the same throttling shape as
    /// [`job_search_cooldown`](Self::job_search_cooldown).
    bed_search_cooldown: i32,
    /// The bell this villager has claimed as its meeting point-of-interest, if any —
    /// [`bed`](Self::bed)'s sibling. Cleared alongside a ticket release when
    /// [`MobSim::tick_villager_bells`] finds the claim gone. Native-only, for
    /// [`bed`](Self::bed)'s own reason.
    meeting_point: Option<BlockPos>,
    /// Ticks until this mob's next bell search, decremented in
    /// [`MobSim::tick_villager_bells`] — [`bed_search_cooldown`](Self::bed_search_cooldown)'s
    /// sibling.
    bell_search_cooldown: i32,
    /// The nearest warden-listenable vibration this tick, if this mob is a
    /// listener species ([`is_vibration_listener`]) and one was posted in
    /// range — the vibration substrate, resolved host-side by
    /// [`MobSim::resolve_vibrations`]. `None` for every other mob, and for a
    /// listener with nothing audible in range this tick. Consumed by
    /// [`MobSim::resolve_warden_anger`] (the anger-resolution step) into
    /// [`warden_anger`](Self::warden_anger)/[`warden_anger_target`](Self::warden_anger_target).
    nearest_vibration: Option<PostedVibration>,
    /// Vanilla's own per-suspect anger map's value for this mob's own **single**
    /// tracked suspect — a real, disclosed narrowing of vanilla's per-suspect
    /// map (which tracks several candidates at once and picks the angriest
    /// via its own sorter). `0..=`[`warden::MAX_ANGER`], decayed by
    /// [`warden::ANGER_DECAY_PER_TICK`] every tick
    /// ([`MobSim::resolve_warden_anger`]) the way vanilla's own per-tick anger
    /// decay does. Meaningless (always `0`) for a
    /// non-listener species.
    warden_anger: i32,
    /// The entity id [`warden_anger`](Self::warden_anger) is banked against —
    /// The last vibration source associated with this anger state. A vibration
    /// from a **different** source replaces it and resets anger to `0` before
    /// the new event is absorbed. `None` means anger has decayed to `0` or the
    /// target is absent.
    warden_anger_target: Option<i32>,
    /// Warden emergence duration (134 ticks) counted down from spawn. `0` for
    /// every non-warden species and for a warden past its emerge window. While
    /// positive, the warden is invulnerable and does not strike; the warden
    /// activity resolver gives emergence priority over fighting. See
    /// [`warden`] for the digging and despawn behavior that is not modeled.
    warden_emerge_ticks: i32,
    /// Sonic-boom cooldown (40 ticks), ticked down once a boom lands. `0` for
    /// every non-warden species.
    warden_sonic_boom_cooldown: i32,
    /// Dig-cooldown TTL (`1200`) at spawn, refreshed to that value on every
    /// angry warden tick and decremented toward `0` otherwise. Digging becomes
    /// eligible only once this reaches `0`. `0` for every non-warden species.
    warden_dig_cooldown: i32,
    /// Digging duration (100 ticks) counted down while in the digging pose.
    /// [`warden::MobSim::resolve_warden_anger`] removes this mob once it reaches
    /// `0`. `0` for every non-warden species and for a warden not currently
    /// digging.
    warden_digging_ticks: i32,
    /// Whether the goat's left horn is present. `true` for every non-goat
    /// species and for a goat that has not lost the horn. The value is rolled
    /// once at spawn (`< 0.1` removes it); no block-contact path removes a horn
    /// later because this seam has no block-state read.
    has_left_horn: bool,
    /// Whether the goat's right horn is present; see
    /// [`has_left_horn`](Self::has_left_horn) for the same rule.
    has_right_horn: bool,
    /// `minecraft:spawn_reinforcements` base value. Rolled once at spawn for
    /// the zombie family (`< 0.1`) and decremented by
    /// [`ZOMBIE_REINFORCEMENT_CALLER_CHARGE`] each successful call-in. The
    /// accumulated value is `0.0` for every non-zombie-family species. The
    /// leader bonus (`difficulty_modifier * 0.05`, adding `0.5..0.75` and
    /// enabling full health and door breaking) is not modeled.
    reinforcement_chance: f64,
    /// This mob's own gossip ledger: what it believes about every UUID it has
    /// an opinion of. Empty for every
    /// non-villager species; a converted zombie villager's ledger is seeded
    /// at conversion time ([`villager::reputation::apply_reputation_event`]
    /// with [`villager::reputation::ReputationEventType::ZombieVillagerCured`]).
    gossip: villager::gossip::GossipContainer,
    /// The tick this mob's gossip last decayed, for the 24000-tick cadence.
    /// `None` before the first decay check; the first check records the
    /// timestamp rather than decaying immediately.
    last_gossip_decay_tick: Option<u64>,
    /// This mob's own "golem detected recently" memory
    /// — the absolute tick at which it stops suppressing a golem-summon
    /// attempt, or `None` while the memory is absent. Set after a successful
    /// spawn; proximity to an already-present iron golem does not set it in this
    /// model.
    /// Only meaningful for `minecraft:villager`.
    golem_detected_until: Option<u64>,
    /// Live zombie-villager conversion state — `Some` only
    /// while [`entity_type`](Self::entity_type) is `minecraft:zombie_villager`
    /// and a golden apple has been used on it while weakened. `None` for
    /// every other mob, and for a zombie villager that has not been cured
    /// yet.
    conversion: Option<villager::conversion::ConversionState>,
    /// This mob's live status effects — vanilla's own active-effects map. Populated
    /// by a splash/lingering potion's impact
    /// ([`MobSim::resolve_projectile_impacts`] via
    /// `crate::mobs::projectiles::resolve_potion_splash`); nothing yet ticks it
    /// periodically for a mob the way `crate::server`'s vitals tick does for a
    /// player, so a poison/wither/regeneration effect landed here does not yet
    /// deal its own periodic damage or heal — see [`crate::mob_effects`]'s
    /// module doc for the splash side of this gap.
    effects: crate::mob_effects::ActiveEffects,
    /// The **player entity id** riding this mob, or `None` — the mob-mounted-by-
    /// player half of the passenger model (vanilla's own horse-family mount
    /// interaction leads into its generic start-riding path), independent of
    /// [`TrackedVehicle::rider`] (boats) and [`TrackedMinecart::rider`]
    /// (minecarts): those are AI-less item-entities in their own maps, while a
    /// mounted mob keeps its full [`SimMob`] identity, health and (when
    /// unridden) goal AI. See [`MobSim::mount_mob`] for the occupancy rules.
    rider: Option<i32>,
}

struct ProjectileMeta {
    uuid: Uuid,
    entity_type: ResourceKey,
    /// The owner admitted for this projectile's next tick-start plan.
    ///
    /// The central projectile writer advances this only after the source-stop
    /// and durable-save barrier has admitted the completed destination.
    tick_owner: crate::mobs::projectiles::ProjectileTickOwner,
    /// The bounded handoff state for this projectile's cross-owner movement.
    handoff: EntityOwnershipHandoff,
    /// The entity id that launched it, if known.
    ///
    /// Load-bearing for the impact pass, not bookkeeping: a projectile is spawned
    /// at its shooter's eye, *inside* the shooter's own bounding box, so without
    /// this a skeleton's first arrow strikes the skeleton. Vanilla's own guard is
    /// two-part — its own "can hit entity" check refuses the owner until
    /// a "has left owner" check has seen the projectile clear it, and
    /// vanilla's own margin computation keeps the hitbox at zero inflation for the
    /// first two ticks — and this is the first half.
    owner: Option<i32>,
    /// A built-in potion identity validated when the raw item component crosses
    /// into this simulation, for a splash or lingering potion only. `None`
    /// covers every other throwable, a stack with no potion component, and an
    /// extension or malformed value. [`MobSim::resolve_projectile_impacts`]
    /// reads this to decide what [`crate::mob_effects::potion_splash_effects`]
    /// applies on impact.
    potion: Option<PotionId>,
}

/// Wire identity plus fall dynamics for one tracked dropped item.
///
/// [`ItemEntityRegistry`]  tracks only the age/pickup-delay/count
/// *lifecycle* — deliberately world- and wire-free, per its own doc comment.
/// The item's identity and its [`ItemMotion`] (the fall-dynamics state) live
/// here, on the server-authoritative side for item state.
#[derive(Debug, Clone)]
struct ItemState {
    uuid: Uuid,
    item: ResourceKey,
    motion: ItemMotion,
    /// The owner admitted by the last central source-stop/destination-start
    /// barrier. Workers use this value as their tick-start authority rather
    /// than re-deriving ownership from a state that another owner may have
    /// already moved.
    owner: ItemTickOwner,
}

/// The chunk that owns a dropped item at the start of its tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum ItemTickOwner {
    Chunk { cx: i32, cz: i32 },
}

impl ItemTickOwner {
    fn for_position(position: Vec3) -> Self {
        Self::Chunk {
            cx: (position.x.floor() as i32).div_euclid(16),
            cz: (position.z.floor() as i32).div_euclid(16),
        }
    }

    fn tick_owner(self) -> crate::tick_region::TickOwner {
        match self {
            Self::Chunk { cx, cz } => crate::tick_region::TickOwner::Chunk { cx, cz },
        }
    }
}

/// One completed dropped-item owner batch.
#[derive(Debug, Clone)]
pub(crate) struct ItemTickOwnerBatch {
    owner: ItemTickOwner,
    plan: u64,
    expected_batch_count: usize,
    effects: Vec<ItemTickEffect>,
}

#[derive(Debug, Clone)]
struct ItemTickEffect {
    owner: ItemTickOwner,
    destination: ItemTickOwner,
    serial: usize,
    id: i32,
    lifecycle: ItemLifecycle,
    state: ItemState,
    discard: bool,
}

#[derive(Debug, Clone)]
struct ItemTickInput {
    owner: ItemTickOwner,
    serial: usize,
    id: i32,
    lifecycle: ItemLifecycle,
    state: ItemState,
}

fn merge_item_tick_owner_batches(mut batches: Vec<ItemTickOwnerBatch>) -> Vec<ItemTickEffect> {
    let first = batches
        .first()
        .expect("item owner completion must contain every tick-start owner batch");
    let plan = first.plan;
    let expected_batch_count = first.expected_batch_count;
    let mut owners = std::collections::HashSet::new();
    for batch in &batches {
        assert_eq!(
            (batch.plan, batch.expected_batch_count),
            (plan, expected_batch_count),
            "item owner completions must originate from one tick-start plan"
        );
        assert!(
            owners.insert(batch.owner),
            "item owner completion may not contain one owner twice"
        );
        assert!(
            batch.effects.iter().all(|effect| effect.owner == batch.owner),
            "an item owner batch may contain only its own effects"
        );
    }
    assert_eq!(
        batches.len(),
        expected_batch_count,
        "item owner completion must contain every tick-start owner batch exactly once"
    );
    let mut effects: Vec<_> = batches
        .drain(..)
        .flat_map(|batch| batch.effects)
        .collect();
    effects.sort_unstable_by_key(|effect| effect.serial);
    for (serial, effect) in effects.iter().enumerate() {
        assert_eq!(
            effect.serial, serial,
            "item owner completion must retain every tick-start serial slot exactly once"
        );
    }
    effects
}

/// One live experience orb.
///
/// # `value` and `count` are different numbers and both are player-visible
///
/// `value` is vanilla's own value metadata field: the points **one** absorption pays out, and the only
/// field on the wire. `count` is vanilla's own orb-count field, how many orbs this single
/// entity stands for — vanilla's own merge step adds the absorbed orb's count and its
/// own player-touch handler
/// decrements it, discarding the entity at zero. So a merged orb is one entity, one
/// texture frame, and several separate absorptions of `value` points each.
///
/// Reading `count` as "the points this orb is worth" is the plausible wrong model: it
/// makes a merged pile pay out `value` once instead of `count` times, so a big drop
/// silently loses most of its XP while every orb still looks right on screen.
///
/// # Why `ItemMotion` carries the position and *not* the tick
///
/// [`ItemMotion`] is used purely as the position/velocity/`on_ground` triple, because
/// [`settle_entity`] already resolves that triple against real block shapes.
/// [`ItemMotion::tick`] is **not** called for an orb: an item's gravity is 0.04 and
/// its landing bounce is `velocity.y *= -0.5`, while vanilla's own orb-gravity getter
/// is `0.03` and its bounce is `-fallSpeed * 0.4` off the *pre-move* fall speed. See
/// [`MobSim::tick_orbs`], which transcribes vanilla's own orb per-tick update in its own order.
#[derive(Debug, Clone)]
struct OrbState {
    uuid: Uuid,
    /// The chunk owner that is allowed to mutate this orb at tick start.
    ///
    /// This is carried across the owner hand-off instead of being re-derived
    /// from the mutable position by the next phase. The central apply boundary
    /// updates it only after a completion has passed validation, so one orb
    /// cannot be observed as owned by both source and destination chunks.
    owner: (i32, i32),
    /// Vanilla's own value metadata field — points per absorption.
    value: i32,
    /// Vanilla's own orb-count field — absorptions remaining before the entity is discarded.
    count: i32,
    /// Vanilla's own age field, in ticks. Discarded at [`ORB_LIFETIME`], and reset to `0`
    /// by a merge so a pile does not expire on its oldest member's clock.
    age: i32,
    motion: ItemMotion,
}

/// Wire identity plus motion for one live falling-block entity — the
/// falling-block analogue of [`ItemState`].
///
/// The `state` string is the block the entity is *imitating*
/// (vanilla's own block-state field) and is what goes back into the world on
/// landing. It also resolves the add-entity packet's own object-data field —
/// vanilla's own add-entity-packet builder passes
/// the block-state id — which is the **only** channel a client
/// learns what a falling block looks like: vanilla's own metadata registration registers
/// only the start-position field and nothing else, so the state is never in an entity-metadata
/// packet. A falling block streamed with object data `0` draws as whatever state
/// id `0` happens to be, silently, exactly as an item entity with no reported
/// stack drew nothing.
#[derive(Debug, Clone)]
struct TrackedFallingBlock {
    uuid: Uuid,
    /// The imitated block state, e.g. `minecraft:sand`.
    state: String,
    motion: crate::gravity_tick::FallingBlockMotion,
    /// Where the fall ends, resolved once by
    /// `crate::gravity_tick::find_landing_y` against the live world at spawn
    /// time. See [`FallingBlockMotion::step`](crate::gravity_tick::FallingBlockMotion::step)
    /// for why this is captured rather than re-read each tick.
    landing_y: i32,
}

/// The result of [`MobSim::attack`] resolving a melee hit against a live mob.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AttackOutcome {
    /// The target's remaining health after the hit (`0.0` if it died).
    pub health: f32,
    /// Whether this hit reduced health to `0.0` and removed the mob from the
    /// sim.
    pub killed: bool,
    /// Damage that actually reached health — `0.0` if the hit was fully
    /// ignored by the invulnerability-frame gate, matching
    /// [`SimMob::apply_damage`]'s own return convention.
    pub damage_dealt: f32,
    /// The target's velocity after knockback (unchanged from its pre-hit
    /// value whenever the call's `knockback_power` was `<= 0.0`), in
    /// blocks/tick — ready to encode on the next
    /// [`snapshots`](MobSim::snapshots) call.
    pub velocity: Vec3,
}

/// The server-side mob simulation: owns the live mobs and advances them.
///
/// The [`ChunkWorld`] is borrowed (the mobs path over it), so the caller holds
/// the world and hands it here. Drive the sim with [`tick`](MobSim::tick) once
/// per game tick, or [`tick_for`](MobSim::tick_for) to run many.
///
/// Also owns a [`ProjectileRegistry`] and an [`ItemEntityRegistry`]. The shared
/// server tick calls [`tick`](MobSim::tick), which advances projectiles and
/// dropped items alongside mobs; keeping the registries together preserves one
/// snapshot and collision path for all three entity kinds.
/// with no new task, and [`snapshots`](MobSim::snapshots) puts every entity
/// kind on the same wire path mobs already proved reaches a real client.
#[derive(Debug)]
pub struct MobSim<'w> {
    world: &'w ChunkWorld,
    mobs: Vec<SimMob<'w>>,
    projectiles: ProjectileRegistry,
    projectile_meta: HashMap<i32, ProjectileMeta>,
    items: ItemEntityRegistry,
    item_state: HashMap<i32, ItemState>,
    /// Live `ExperienceOrb`s, keyed by network entity id.
    ///
    /// A plain map for [`falling_blocks`](Self::falling_blocks)' reason rather than a
    /// registry in `lodestone-entity`: an orb's lifecycle is an age counter and a
    /// merge rule, and **the merge rule is keyed on the network entity id**
    /// (`(orb.getId() - id) % 40 == 0`), which a version-free registry that does not
    /// own ids structurally cannot express.
    orbs: HashMap<i32, OrbState>,
    /// The bounded-int-under-40 draw vanilla's own orb-merge attempt makes per spawned
    /// denomination, on its own stream so awarding XP cannot shift which roll a mob
    /// spawn or a block drop sees.
    orb_rng: SpawnRng,
    /// Vanilla's own default-equipment-population step's own random draws — the
    /// armour-upgrade roll and each species' weapon roll — on its own stream
    /// for [`orb_rng`](Self::orb_rng)'s reason: rolling a drowned's trident
    /// must not shift which denomination an orb merges into or which roll a
    /// despawn check sees.
    equipment_rng: SpawnRng,
    /// Vanilla's own goat spawn-finalization's own `< 0.1` pre-broken-horn roll,
    /// plus the coin flip that picks which horn — on its own stream
    /// for [`orb_rng`](Self::orb_rng)'s reason.
    goat_horn_rng: SpawnRng,
    /// Vanilla's own zombie spawn-finalization's own door-breaking roll
    /// (a `< difficultyModifier * 0.1` float draw sets the can-break-doors flag),
    /// covering the whole zombie family — on its own stream for
    /// [`orb_rng`](Self::orb_rng)'s reason: rolling whether a zombie can open
    /// doors must not shift which denomination an orb merges into or which
    /// roll a despawn check sees.
    door_rng: SpawnRng,
    /// Vanilla's own "special difficulty multiplier" getter fed to every spawn's
    /// [`lodestone_entity::spawn_equipment::populate_default_equipment_slots`]
    /// call. `0.0` by default — vanilla's own value for a fresh world's
    /// effective difficulty (`< 2.0`) — so armour never rolls until a caller
    /// wires a real regional-difficulty reading through
    /// [`set_spawn_difficulty`](Self::set_spawn_difficulty). The drowned's
    /// trident roll is independent of this (the drowned does not call its
    /// parent's version),
    /// so it works with no wiring at all.
    spawn_special_multiplier: f32,
    /// Vanilla's own "difficulty is hard" check, the second (non-continuous) input
    /// [`base_armor_roll`](lodestone_entity::spawn_equipment::base_armor_roll)
    /// needs alongside `spawn_special_multiplier` — see that function's own
    /// doc for why a saturated `special_multiplier` does not imply this.
    /// `false` by default.
    spawn_hard_difficulty: bool,
    /// Vanilla's own "is spawning monsters" check — the `spawn_mobs` game rule, fed
    /// alongside [`spawn_hard_difficulty`](Self::spawn_hard_difficulty) since
    /// vanilla's own zombie hurt-handler's reinforcement call gates on both. `false` by
    /// default, so an unwired caller sees zero reinforcements rather than
    /// silently-always-on ones.
    spawn_monsters_enabled: bool,
    /// Vanilla's own zombie hurt-handler's own random draw for the reinforcement
    /// chance roll — on its own stream for [`orb_rng`](Self::orb_rng)'s
    /// reason: whether a hit zombie calls for backup must not shift which
    /// denomination an orb merges into or which roll a despawn check sees.
    reinforcement_rng: SpawnRng,
    /// Reinforcement calls [`attack`](Self::attack) has decided should
    /// happen — the *roll* only, queued for `crate::tick::run_tick_loop` to
    /// place, the same decide-here/place-there split
    /// [`pending_lightning_fires`](Self::pending_lightning_fires) already
    /// established: finding a valid spawn position needs the live world this
    /// version-free sim does not hold. See
    /// [`take_reinforcement_calls`](Self::take_reinforcement_calls).
    pending_reinforcements: Vec<ReinforcementCall>,
    /// Live `FallingBlockEntity`s, keyed by network entity id — the falling
    /// sand/gravel a `crate::gravity_tick::TICK_GRAVITY` scheduled tick created.
    ///
    /// A plain map here rather than a registry in `lodestone-entity` beside
    /// [`ItemEntityRegistry`]/[`ProjectileRegistry`], because a falling block has
    /// no lifecycle to model separately from its motion: it exists for the
    /// duration of one fall, carries no age or merge rules, and its only
    /// version-free part ([`FallingBlockMotion`]) already lives in
    /// `crate::gravity_tick` next to the `FallingBlock` port that creates it.
    falling_blocks: HashMap<i32, TrackedFallingBlock>,
    next_id: i32,
    tick_count: u64,
    /// The latest dropped-item owner plan issued from this simulation.
    item_owner_plan: u64,
    /// The newest dropped-item owner plan accepted by the central writer.
    applied_item_owner_plan: u64,
    /// Source-stop/destination-start barrier for dropped items that cross a
    /// chunk owner during their motion pass.
    item_handoff: EntityOwnershipHandoff,
    /// The latest experience-orb owner plan issued from this simulation.
    orb_owner_plan: u64,
    /// The newest experience-orb owner plan accepted by the central writer.
    applied_orb_owner_plan: u64,
    /// Source-stop/destination-start barrier for experience orbs crossing a
    /// chunk owner during their motion pass.
    orb_handoff: EntityOwnershipHandoff,
    /// The latest burn-counter owner plan issued from this simulation.
    burn_owner_plan: u64,
    /// The newest burn-counter owner plan accepted by the central writer.
    applied_burn_owner_plan: u64,
    /// The latest leash-owner plan issued from this simulation. A completion
    /// must name this exact generation before the central writer accepts it.
    leash_owner_plan: u64,
    /// The newest leash-owner plan already applied by the central writer.
    /// Keeping this separate from [`leash_owner_plan`](Self::leash_owner_plan)
    /// rejects replayed completions even when they contain only `Keep` effects.
    applied_leash_owner_plan: u64,
    /// The latest primed-explosive owner plan issued from this simulation.
    tnt_owner_plan: u64,
    /// The newest primed-explosive owner plan already applied by the central
    /// writer. Keeping this separate rejects replayed completions.
    applied_tnt_owner_plan: u64,
    /// The latest unridden-vehicle owner plan issued from this simulation.
    vehicle_owner_plan: u64,
    /// The newest unridden-vehicle owner plan already applied by the central
    /// writer. Keeping this separate rejects replayed completions.
    applied_vehicle_owner_plan: u64,
    /// The latest minecart owner plan issued from this simulation.
    minecart_owner_plan: u64,
    /// The newest minecart owner plan already applied by the central writer.
    applied_minecart_owner_plan: u64,
    /// The latest fishing-bobber owner plan issued from this simulation.
    fishing_owner_plan: u64,
    /// The newest fishing-bobber owner plan accepted by the central writer.
    fishing_applied_owner_plan: u64,
    /// The latest projectile-motion owner plan issued from this simulation.
    projectile_owner_plan: u64,
    /// The newest projectile-motion owner plan already applied by the central
    /// writer. Keeping this separate rejects replayed completions.
    applied_projectile_owner_plan: u64,
    /// Cells the last tick's item-settling pass asked [`LiveBlockCollision`] for —
    /// see [`items_settled_probe_count`](Self::items_settled_probe_count).
    item_probe_count: u64,
    /// Every detonation [`tick`](Self::tick) has triggered since the last
    /// [`take_detonations`](Self::take_detonations) call.
    /// `tick` itself has no wire access — it only knows `self.world` — so
    /// this is the handoff point a driver ([`crate::tick::run_tick_loop`])
    /// drains into an [`crate::tick::ExplosionFeed`] for a connection to
    /// turn into a real `EXPLODE` packet. See that method's own doc comment
    /// for why draining, not just reading, is what keeps a detonation from
    /// being broadcast twice.
    pending_detonations: Vec<Detonation>,
    /// Grazed blocks awaiting the driver's world mutation, as
    /// `(mob block position, which of the two blocks)`.
    ///
    /// The same handoff shape as [`pending_detonations`](Self::pending_detonations)
    /// above, and for a stronger reason: this sim holds `world: &'w ChunkWorld`
    /// **immutably**, so [`tick`](Self::tick) structurally *cannot* apply the
    /// eat. Drained by [`take_grazes`](Self::take_grazes).
    ///
    /// Position is the mob's own block position, not the eaten block's, because
    /// the two `EatenBlock` variants are relative to it: `AtFeet` is that cell,
    /// `Below` is one down. Storing the mob's cell keeps the arithmetic with the
    /// consumer that knows what each variant means.
    pending_grazes: Vec<(BlockPos, EatenBlock)>,
    /// Players struck by a hostile mob's melee attack this tick, awaiting the
    /// driver's `PlayerVitals::apply_damage` call — the same
    /// handoff shape as [`pending_detonations`](Self::pending_detonations)
    /// above and for the same reason: this sim owns no connection and cannot
    /// reach a player's authoritative health itself. Drained by
    /// [`take_player_hits`](Self::take_player_hits). See [`PlayerHit`]'s own
    /// doc comment for how a target position resolves to a player identity.
    pending_player_hits: Vec<PlayerHit>,
    /// Players caught in an elder guardian's mining-fatigue pulse this tick
    /// This has the same handoff shape as
    /// [`pending_player_hits`](Self::pending_player_hits) above and for the
    /// same reason: this sim owns no connection and cannot reach a player's
    /// `ActiveEffects` itself, nor send the `GUARDIAN_ELDER_EFFECT` game
    /// event. Drained by
    /// [`take_mining_fatigue_auras`](Self::take_mining_fatigue_auras). See
    /// [`MiningFatigueAura`]'s own doc comment for exactly what the caller
    /// owes vanilla.
    pending_mining_fatigue: Vec<MiningFatigueAura>,
    /// Hurt and death sounds awaiting the driver, the same handoff
    /// shape as the two above and for the same reason: this sim owns no
    /// connection. Drained by [`take_vocalisations`](Self::take_vocalisations).
    ///
    /// `apply_damage` records the sound outcome for each damage or death event;
    /// the driver encodes those outcomes for connected players.
    pending_vocalisations: Vec<crate::effects::WorldEffect>,
    /// Idle ambient vocalisations awaiting the driver — the same handoff shape
    /// as [`pending_vocalisations`](Self::pending_vocalisations) and for the
    /// same reason, but rolled every tick per mob
    /// ([`roll_ambient_sound`]) rather than recorded at a damage funnel.
    /// Drained by [`take_ambient_sounds`](Self::take_ambient_sounds).
    ///
    /// Before this, `MobSim` had no periodic ambient-sound producer at all —
    /// hurt and death were the only mob sounds a client could ever hear, so
    /// ordinary exploration (no combat) was silent but for footsteps.
    pending_ambient_sounds: Vec<PendingEntityTickEffect>,
    /// Per-entity animation cues awaiting the driver — the *visible* half of the
    /// same hits [`pending_vocalisations`](Self::pending_vocalisations) makes
    /// audible, and recorded at the same funnels for the same reason (this sim
    /// owns no connection). Drained by
    /// [`take_entity_animations`](Self::take_entity_animations).
    ///
    /// Two packets, not one, because vanilla uses two: the hurt flash is the
    /// `HURT_ANIMATION` packet and the fall-over is
    /// the `ENTITY_EVENT` packet's byte 3
    /// (vanilla's own death handler broadcasts that entity-status event). Before this a mob could be
    /// beaten to death and simply *vanish* — no flash, no tip-over — because
    /// `ServerProtocol` had no encoder for either.
    pending_animations: Vec<MobAnimation>,
    /// Every connected player's perception-relevant state, refreshed by a
    /// driver through [`set_players`](Self::set_players) and consumed by
    /// [`tick`](Self::tick) to feed each mob's `nearest_player`/`temptation`.
    ///
    /// [`set_players`](Self::set_players) supplies the player position used by
    /// eight perception methods; the live mob tick calls it before goal updates.
    players: Vec<PerceivedPlayer>,
    /// Raw `(player entity id, game tick they lay down)` pairs for every
    /// currently sleeping player — the player-position feed for
    /// shoulder-ride dismount behavior. Fed once per tick by
    /// [`set_sleeping_players`](Self::set_sleeping_players) from
    /// `crate::sleep::SleepState`'s own roster, which is keyed by entity id
    /// (the same id [`PlayerIdentity::entity_id`] carries) rather than by
    /// uuid — this sim resolves the join against
    /// [`players`](Self::players)' own identities at the point of use
    /// (`feed_perception`'s owner census, and
    /// [`tick_shoulder_dismounts`](Self::tick_shoulder_dismounts)) rather
    /// than pre-joining here, so a sim with no player registry (the common
    /// singleplayer shape) still compiles and simply never resolves anyone
    /// asleep.
    sleeping_players: Vec<(i32, u64)>,
    /// One tamed mob currently perched on its owner's shoulder, keyed by
    /// owner uuid — the shoulder-riding state. **One slot per
    /// owner**, not vanilla's two (left/right); see
    /// [`resolve_shoulder_mounts`](Self::resolve_shoulder_mounts)'s own doc
    /// for what that costs. The mob entity is absent from
    /// [`mobs`](Self::mobs) while it holds this slot — only its type and the
    /// tick it mounted survive, enough to respawn it in
    /// [`tick_shoulder_dismounts`](Self::tick_shoulder_dismounts).
    shoulder_riders: HashMap<Uuid, ShoulderRider>,
    /// The `nextInt(3)` / `nextInt(10)` / `nextInt(maxTemper)` draws the taming
    /// mechanisms make, on their own stream so a tame attempt cannot shift which
    /// roll a mob spawn, a despawn pass or an XP award sees — the same isolation
    /// [`orb_rng`](Self::orb_rng) exists for.
    ///
    /// Injectable through [`set_tame_rng`](Self::set_tame_rng), which is how a
    /// gate drives a tame roll to both sides of its threshold instead of
    /// asserting that taming "sometimes" happens.
    tame_rng: SpawnRng,
    /// The `random.nextInt(2401)` conversion-time roll
    /// ([`villager::conversion::roll_conversion_ticks`]) plus the per-tick
    /// `nextFloat()` progress draws ([`villager::conversion::conversion_progress`]),
    /// on their own stream for [`tame_rng`](Self::tame_rng)'s reason: curing a
    /// zombie villager must not shift which roll a tame attempt or a mob spawn
    /// sees.
    zombie_conversion_rng: SpawnRng,
    /// The RNG [`spread_villager_gossip`](Self::spread_villager_gossip) draws
    /// from for the gossip ledger's weighted selection, on its own stream for
    /// the same isolation reason
    /// [`zombie_conversion_rng`](Self::zombie_conversion_rng) is separate.
    gossip_spread_rng: SpawnRng,
    /// The `random.nextInt(7) + 1` draw vanilla's own
    /// post-breeding child-finalization step makes for the experience orb a
    /// successful mating pops, on its own stream for [`tame_rng`](Self::tame_rng)'s
    /// reason: a breeding event must not shift which roll a tame attempt sees.
    breed_rng: SpawnRng,
    /// The `mob_drops` game rule, mirrored in by
    /// [`set_mob_drops`](Self::set_mob_drops). `true` by default, which is vanilla's
    /// own default and the behaviour before the rule was readable.
    mob_drops: bool,
    /// Live rideable **vehicles** — every `AbstractBoat` a player has placed,
    /// keyed by network entity id.
    ///
    /// A registry of its own rather than a [`SimMob`], and that is the whole
    /// design: a boat has no attributes, no goals and no AI, so
    /// [`spawn_species`](Self::spawn_species) would give it a mob's component set
    /// and produce a boat that *wanders*. It also has to stop being
    /// server-driven the instant a player sits in it —
    /// vanilla's own generic "is client authoritative" check delegates to the controlling passenger
    /// and its own player-specific override is `true` — which is a property no
    /// mob has.
    ///
    /// A plain map for the reason [`falling_blocks`](Self::falling_blocks) is
    /// one: there is no version-free lifecycle to model beyond the motion, and
    /// the motion is [`lodestone_physics::vehicle`]'s, shared with the client so
    /// a boat we *watch* and a boat we *ride* cannot disagree about a slab.
    vehicles: HashMap<i32, TrackedVehicle>,
    /// Live `PrimedTnt`, keyed by network entity id — see [`TrackedTnt`] for
    /// why this is a plain map beside [`vehicles`](Self::vehicles) rather than
    /// a [`SimMob`].
    tnt: HashMap<i32, TrackedTnt>,
    /// Live minecarts — every `AbstractMinecart` subclass, keyed by network
    /// entity id. See [`TrackedMinecart`] for the shape and `mobs::minecart`'s
    /// own module doc for the physics.
    minecarts: HashMap<i32, TrackedMinecart>,
    /// The `random.nextDouble()` draw a fresh primed-tnt entity's launch direction
    /// makes (vanilla's own three-argument constructor), on its own stream for
    /// [`orb_rng`](Self::orb_rng)'s reason: priming TNT must not shift which
    /// roll a mob spawn, a block drop or anything else sees.
    tnt_rng: SpawnRng,
    /// Vanilla's own patrol-spawner "next tick" field — ticks remaining before the next
    /// patrol-spawn attempt, decremented once per
    /// [`run_patrol_spawn_cycle`](Self::run_patrol_spawn_cycle) call
    /// regardless of whether it does anything, exactly as vanilla's own
    /// generic custom-spawner update decrements its own countdown every world tick.
    patrol_next_tick: i32,
    /// The `random.nextInt(…)` draws [`run_patrol_spawn_cycle`](Self::run_patrol_spawn_cycle)
    /// makes, on its own stream for the same isolation reason
    /// [`tame_rng`](Self::tame_rng) is separate from every other roll: a
    /// patrol-spawn attempt must not shift which roll a mob spawn, a despawn
    /// pass or a tame attempt sees.
    patrol_rng: SpawnRng,
    /// Vanilla's own wandering-trader-spawner "tick delay" field — ticks remaining before
    /// the next 1200-tick poll, decremented once per
    /// [`run_wandering_trader_spawn_cycle`](Self::run_wandering_trader_spawn_cycle)
    /// call regardless of outcome, exactly as `patrol_next_tick` is.
    trader_tick_delay: i32,
    /// Vanilla's own saved-data "spawn delay" field — the
    /// 24000-tick delay nested inside the 1200-tick poll. This crate has no
    /// save/load for it (see the doc comment on
    /// [`run_wandering_trader_spawn_cycle`](Self::run_wandering_trader_spawn_cycle)),
    /// so it resets with every fresh `MobSim` rather than surviving a
    /// restart.
    trader_spawn_delay: i32,
    /// Vanilla's own saved-data "spawn chance" field — climbs 25→75
    /// by 25 each time the outer roll is attempted and misses, and resets to
    /// 25 on an actual spawn.
    trader_spawn_chance: i32,
    /// The `random.nextInt(…)` draws
    /// [`run_wandering_trader_spawn_cycle`](Self::run_wandering_trader_spawn_cycle)
    /// makes, on its own stream for the same isolation reason
    /// [`patrol_rng`](Self::patrol_rng) is separate from every other roll.
    trader_rng: SpawnRng,
    /// Live lightning sidecars, keyed by network entity id —
    /// the same shape [`orbs`]'s [`OrbState`] map establishes: no
    /// [`NavigatingMob`]/[`GoalSelector`] body, because a bolt has no box and
    /// no AI. See `mobs/lightning.rs`'s module doc.
    lightning_bolts: HashMap<i32, lightning::LiveBolt>,
    /// Fire-ignition attempts a live bolt's [`lightning::tick_bolt`] made this
    /// tick, awaiting the driver's world mutation — the same handoff shape as
    /// [`pending_grazes`](Self::pending_grazes) and for the identical reason:
    /// `world: &'w ChunkWorld` is an immutable pathfinding snapshot, not the
    /// live `ChunkStore`, so this sim cannot place the fire itself. Drained by
    /// [`take_lightning_fires`](Self::take_lightning_fires).
    pending_lightning_fires: Vec<BlockPos>,
    /// Every projectile-vs-block impact this tick's
    /// [`resolve_projectile_impacts`](Self::resolve_projectile_impacts) found,
    /// awaiting the driver — see [`ProjectileBlockHit`]'s own doc for why this
    /// sim cannot resolve a target block's power write itself. Drained by
    /// [`take_projectile_block_hits`](Self::take_projectile_block_hits).
    pending_projectile_block_hits: Vec<ProjectileBlockHit>,
    /// The live workstation claim ledger [`tick_villager_professions`](Self::tick_villager_professions)
    /// reads and writes. See [`villager::WorkstationClaims`]'s
    /// own doc for why this reuses `crate::poi_storage::PoiRecord` rather
    /// than a parallel claim table, and for what is deliberately not built
    /// (no on-disk persistence, no block-event hook).
    ///
    /// Native-only, same as [`villager::WorkstationClaims`] itself — see
    /// that type's own doc for why (it reuses `crate::poi_storage`, which is
    /// gated the same way, and this crate compiles for `wasm32-unknown-unknown`).
    #[cfg(not(target_arch = "wasm32"))]
    workstation_claims: villager::WorkstationClaims,
    /// The live bed claim ledger [`tick_villager_beds`](Self::tick_villager_beds)
    /// reads and writes (the raid trigger). See
    /// [`villager::BedClaims`]'s own doc for why this reuses
    /// `crate::poi_storage::PoiRecord` and what is deliberately not built.
    ///
    /// Native-only, for [`workstation_claims`](Self::workstation_claims)'s
    /// own reason.
    #[cfg(not(target_arch = "wasm32"))]
    bed_claims: villager::BedClaims,
    /// The live bell claim ledger [`tick_villager_bells`](Self::tick_villager_bells)
    /// reads and writes (the `MEET` schedule activity) — see
    /// [`villager::BellClaims`]'s own doc for why this exists and what it
    /// feeds.
    ///
    /// Native-only, for [`workstation_claims`](Self::workstation_claims)'s
    /// own reason.
    #[cfg(not(target_arch = "wasm32"))]
    bell_claims: villager::BellClaims,
    /// The real world time-of-day, `0..24000`, host-fed once per tick by
    /// [`set_day_time`](Self::set_day_time) — what [`feed_perception`](Self::feed_perception)
    /// hands every villager's [`NavigatingMob::set_day_time`] so
    /// `crate::brain`'s villager schedule (`WORK`/`MEET`/`REST`/`IDLE`) has a
    /// real clock to switch against, rather than the per-mob monotonic
    /// counter `BrainMob::game_time` is (see that method's own doc for why
    /// the two must not be confused). `0` (perpetual midnight) until a real
    /// driver calls the setter — every hermetic test that never calls it
    /// keeps a villager's schedule at the very start of its `IDLE` window,
    /// which is a harmless default rather than a silent lie, since `0` is a
    /// real, reachable time of day.
    day_time: i32,
    /// Vibrations real producers posted this tick (the vibration substrate) —
    /// resolved into each listener's [`SimMob::nearest_vibration`] by
    /// [`resolve_vibrations`](Self::resolve_vibrations), which also drains
    /// this back to empty so nothing crosses into the next tick. No
    /// `wasm32` gate: unlike the villager claim ledgers, this touches no
    /// `std::fs`-backed type.
    posted_vibrations: Vec<PostedVibration>,
    /// Live ender dragons, keyed by network entity id — see [`TrackedDragon`]
    /// and `mobs::dragon`'s own module doc for the phase/heal state each one
    /// drives and exactly what is a real port vs. a simplification.
    dragons: HashMap<i32, TrackedDragon>,
    /// Live end crystals, keyed by network entity id — see [`TrackedCrystal`]
    /// and `mobs::end_crystal`'s own module doc.
    crystals: HashMap<i32, TrackedCrystal>,
    /// The dragon's own phase-transition/crystal-rescan RNG rolls
    /// (`random.nextInt(crystals+3)`, `.nextInt(10)`, ...), on its own stream
    /// for [`tnt_rng`](Self::tnt_rng)'s reason: a dragon tick must not shift
    /// which roll a mob spawn, a block drop, or anything else sees.
    dragon_rng: SpawnRng,
    /// Live fishing bobbers, keyed by network entity id — see
    /// [`fishing::FishingBobber`] and `mobs::fishing`'s own module doc.
    fishing_bobbers: HashMap<i32, fishing::FishingBobber>,
    /// The bobber cast/bob/bite/loot-roll RNG stream, on its own stream for
    /// [`dragon_rng`](Self::dragon_rng)'s reason.
    fishing_rng: SpawnRng,
    /// Live raids, keyed by this sim's own raid id (not a
    /// network entity id — a raid has no entity of its own; see
    /// [`raid::Raid`] and `mobs::raid`'s own module doc).
    raids: HashMap<i32, raid::Raid>,
    /// The next id [`raid::MobSim::start_raid`] assigns — a separate counter
    /// from [`next_id`](Self::next_id) because a raid id is never a network
    /// entity id and must never collide with one being reused after a raid
    /// despawns its raiders.
    next_raid_id: i32,
    /// Hero of the Village grants a raid victory has queued but no
    /// connection has drained yet — see
    /// [`raid::MobSim::take_hero_of_the_village_grants`]'s own doc for why
    /// this is a queue rather than an inline effect application, and
    /// [`raid::MobSim::tick_raids`]'s for where it is filled.
    pending_hero_grants: Vec<(Uuid, i32)>,
    /// The wave-spawn-position/spawn-count RNG stream, on its own stream for
    /// [`dragon_rng`](Self::dragon_rng)'s reason.
    raid_rng: SpawnRng,
    /// Live withers, keyed by network entity id — see [`TrackedWither`] and
    /// `mobs::wither`'s own module doc for the emergence/heal/skull-fire
    /// state each one drives.
    withers: HashMap<i32, TrackedWither>,
    /// The wither's own dangerous-skull roll, on its own stream for the same
    /// reason [`dragon_rng`](Self::dragon_rng) is.
    wither_rng: SpawnRng,
    /// This session's End dragon fight controller state
    /// (vanilla's own end-dragon-fight persisted flags), lazily created by
    /// [`dragon::MobSim::record_dragon_death`] on the first real kill —
    /// `None` before that, matching vanilla's own default-fight-state
    /// constructor's own
    /// "no scan has happened yet" starting point. See
    /// [`dragon::MobSim::dragon_fight_killed`]'s own doc for what reads this
    /// and `dragon::MobSim::record_dragon_death`'s for the process-lifetime
    /// (not yet disk-persisted) caveat.
    dragon_fight: Option<crate::dragon::fight::FightState>,
    /// Vanilla's own end-dragon-fight gateways field — the shuffled pool
    /// [`crate::dragon::fight::GatewayPool`] consumes one slice from per
    /// kill. Lazily shuffled on the first real kill, alongside
    /// [`dragon_fight`](Self::dragon_fight) and for the identical
    /// process-lifetime-only reason (see
    /// [`dragon::MobSim::record_dragon_death`]'s own doc).
    dragon_gateways: Option<crate::dragon::fight::GatewayPool>,
    /// Vanilla's own generic list-shuffle helper's own random draw, ported against
    /// [`GatewayPool::shuffled`](crate::dragon::fight::GatewayPool::shuffled) —
    /// on its own stream for [`orb_rng`](Self::orb_rng)'s reason. Only ever
    /// drawn from once (the pool shuffles a single time, lazily), but kept
    /// as a stream rather than a one-shot seed so a future re-shuffle (a
    /// fresh arena, say) has somewhere to draw from without disturbing any
    /// other roll.
    gateway_shuffle_rng: SpawnRng,
    /// Every dragon death since the last [`dragon::MobSim::take_dragon_deaths`]
    /// call — the same `pending_*`/`take_*` handoff shape as
    /// [`pending_detonations`](Self::pending_detonations), for the same
    /// reason: this sim holds `world` immutably and owns no connection, so
    /// it cannot place the exit portal or the egg itself.
    pending_dragon_deaths: Vec<dragon::DragonDeathOutcome>,
}

/// One live boat entity — wire identity, motion, and who is aboard.
///
/// # Why the rider is here and not on the connection
///
/// `MobSim::tick` is the only thing that advances a boat, and it must **not**
/// advance a ridden one: the rider's client owns that boat's position and reports
/// it through its own paddle/move-vehicle packet. So the "is anyone aboard" bit has to be readable from
/// inside the tick, which means it lives on the vehicle. A per-connection flag
/// would leave the tick fighting the client, which is the specific failure mode
/// (*"a boat that fights the player"*) this shape exists to prevent.
#[derive(Debug, Clone)]
struct TrackedVehicle {
    uuid: Uuid,
    /// The entity type, e.g. `minecraft:oak_boat`. Carried rather than derived:
    /// the twenty boat types differ only in their texture, and the client resolves
    /// the model from this key alone.
    entity_type: ResourceKey,
    motion: lodestone_physics::EntityMotion,
    /// The hull's yaw in degrees — vanilla's own yaw setter, written by the placing player and
    /// then by vanilla's own boat-control step on whichever side is authoritative.
    yaw: f32,
    /// Vanilla's own boat between-tick state, so the server's float pass and the
    /// client's are literally the same code over the same fields.
    boat: lodestone_physics::vehicle::BoatState,
    /// The **player entity id** of the controlling passenger, or `None` for an
    /// empty boat. `Some` suspends the server-side tick entirely.
    rider: Option<i32>,
    /// Vanilla's own boat paddle-left/right metadata fields — the rider's last reported
    /// paddle-boat packet, purely cosmetic (a *second* connected
    /// player's own paddle animation; the rider's own client always animates
    /// locally regardless of what this crate streams back). See
    /// [`MetadataField::BoatPaddles`](crate::protocol::MetadataField::BoatPaddles)
    /// for the wire-index collision this stands clear of.
    paddle_left: bool,
    paddle_right: bool,
    /// Vanilla's own shared vehicle "hurt" metadata field — ticks remaining on the rocking animation,
    /// set to `10` by a hit and counted down one per tick.
    hurt_time: i32,
    /// Vanilla's own shared vehicle "hurt direction" metadata field — which way the hull tips. Negated on
    /// every hit, so consecutive punches rock it alternately, and its registered
    /// default is **`1`**, not `0`: the client multiplies the whole rock angle by
    /// it, so a zero here draws a perfectly still boat.
    hurt_dir: i32,
    /// Vanilla's own shared vehicle "damage" metadata field — accumulated damage x 10, decayed by
    /// `1.0` per tick. It is the amplitude of the rock.
    damage: f32,
}

/// One live primed-tnt entity — wire identity, motion and the fuse countdown.
///
/// A plain map for [`falling_blocks`](Self::falling_blocks)'s reason: no
/// lifecycle beyond the motion and a counter, so a `SimMob`'s species/goal
/// machinery would be pure overhead for an entity with no AI and no box that
/// matters (it is not selector-visible and nothing paths around it).
///
/// The block state it imitates (vanilla's own block-state metadata field) is **not**
/// carried here: this crate's only producers (`TntBlock::prime`'s several call
/// sites) always construct vanilla's own default tnt block state
/// — nothing here ever sets it to
/// anything else — so a per-entity field would
/// carry one value forever. See `mobs::tnt`'s module doc for the rest of what
/// is deliberately simplified.
#[derive(Debug, Clone)]
struct TrackedTnt {
    uuid: Uuid,
    motion: lodestone_physics::EntityMotion,
    /// Vanilla's own fuse metadata field — ticks remaining before detonation, counting
    /// down from [`tnt::DEFAULT_FUSE_TIME`]. Detonates the tick this reaches
    /// `0`, matching vanilla's own per-tick fuse check.
    fuse: i32,
}

/// One live ender dragon — wire identity, position/yaw, health, the
/// [`crate::dragon::phase::PhaseManager`] driving its phase, and the
/// [`crate::dragon::crystal::NearestCrystal`] tracker its heal reads. See
/// `mobs::dragon`'s own module doc for the per-tick behaviour and exactly
/// which parts are a real vanilla port vs. a named simplification (flight is
/// a simplified orbit, not vanilla's node-graph pathfinding).
#[derive(Debug, Clone)]
struct TrackedDragon {
    uuid: Uuid,
    position: Vec3,
    /// Body yaw, in degrees — driven by the simplified orbit
    /// (`mobs::dragon::tick_one_dragon`), not a real look-at-target
    /// computation.
    yaw: f32,
    health: f32,
    max_health: f32,
    phase: crate::dragon::phase::PhaseManager,
    nearest_crystal: crate::dragon::crystal::NearestCrystal,
    /// Vanilla's own fight-origin getter — the arena centre this dragon orbits
    /// and measures egg/portal distances from.
    fight_origin: Vec3,
    /// The simplified orbit's current angle, in radians — this module's own
    /// state, not a vanilla field (see `mobs::dragon`'s module doc).
    orbit_angle: f64,
}

/// One live wither — wire identity, position, health, the invulnerable
/// "emerging" countdown and skull-fire cooldown. See `mobs::wither`'s own
/// module doc for the per-tick behaviour and exactly which parts are a real
/// vanilla port vs. a named simplification (no movement, one firing
/// schedule standing in for vanilla's three independent heads).
#[derive(Debug, Clone)]
struct TrackedWither {
    uuid: Uuid,
    position: Vec3,
    yaw: f32,
    health: f32,
    max_health: f32,
    /// Vanilla's own wither invulnerability metadata field — `crate::wither::INVULNERABLE_TICKS`
    /// counting down to `0`; `0` means the wither is in its active phase.
    invulnerable_ticks: i32,
    /// Vanilla's own generic tick-count field — this wither's own age, read by
    /// `crate::wither::should_heal_while_invulnerable`/`_active`.
    age: i64,
    /// Ticks until the next skull may fire — see `mobs::wither`'s module doc
    /// for why this is one schedule rather than vanilla's three per-head
    /// timers.
    next_skull_tick: i32,
}

/// One live end crystal — wire identity and a fixed position. See
/// `mobs::end_crystal`'s own module doc for why this tracks nothing else
/// (no pillar to stand on, no cage, no beam-target metadata yet).
#[derive(Debug, Clone, Copy)]
struct TrackedCrystal {
    uuid: Uuid,
    position: Vec3,
}

/// One live minecart entity — wire identity, kind, rail-following motion,
/// riding, and the per-kind extras (a container's slots, a furnace's fuel and
/// push, a TNT cart's fuse). See `mobs::minecart`'s own module doc for the
/// physics this drives and everything deliberately simplified.
///
/// A plain map for the same reason [`TrackedVehicle`]/[`TrackedTnt`] are:
/// no `SimMob` goal machinery, because a minecart has no AI beyond
/// rail-following.
#[derive(Debug, Clone)]
struct TrackedMinecart {
    uuid: Uuid,
    kind: minecart::MinecartKind,
    motion: lodestone_physics::EntityMotion,
    /// Vanilla's own minecart yaw field, computed from the direction of travel
    /// each tick its rail-following behavior moves it — never set by a
    /// placer, unlike a boat's.
    yaw: f32,
    /// Vanilla's own previous-yaw field — the previous tick's yaw, read by the flip-detection
    /// comparison alone.
    yaw_o: f32,
    /// Vanilla's own minecart "flipped" rotation state: when the travel
    /// direction reverses near a dead stop, the sprite's *heading* flips
    /// 180° instead of visibly spinning through it.
    flipped: bool,
    /// The **player entity id** riding this cart, or `None`. Only
    /// [`minecart::MinecartKind::is_rideable`] kinds are ever `Some`.
    rider: Option<i32>,
    /// A container kind's own inventory (`MinecartKind::container_size`
    /// slots; empty for every non-container kind). See `mobs::minecart`'s
    /// own module doc for why nothing yet opens a menu against this.
    slots: Vec<Option<lodestone_model::ItemStack>>,
    /// Vanilla's own furnace-minecart fuel field — ticks of burn time remaining.
    fuel: i32,
    /// Vanilla's own furnace-minecart push field — the constant self-propulsion vector while
    /// fuelled (`y` always `0.0`).
    push: lodestone_physics::Vec3d,
    /// Vanilla's own tnt-minecart fuse field — `-1` unprimed, counts down to `0` (detonate).
    fuse: i32,
}

/// One per-entity animation cue a hit produced, for
/// [`take_entity_animations`](MobSim::take_entity_animations) to hand a driver.
///
/// Two variants because vanilla sends two different packets, and the split is
/// not cosmetic: the hurt flash is the `HURT_ANIMATION` packet (a VarInt id
/// and a `float`) while the death tip-over is the `ENTITY_EVENT` packet (a
/// fixed-width `int` id and a status byte). A driver cannot collapse them.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum MobAnimation {
    /// The mob flashed red — the `HURT_ANIMATION` packet.
    ///
    /// No yaw is carried because vanilla's is a constant for anything that is not
    /// a player: vanilla's own generic hurt-direction getter returns `0.0F` and only
    /// its player-specific override changes it, so a mob's hurt animation is always the pure
    /// roll. Adding a field here would invite a producer to invent one.
    Hurt {
        /// The mob's entity id.
        entity_id: i32,
    },
    /// The mob died — emit the death animation event, which starts the
    /// client's death counter and tips the body onto its side.
    Died {
        /// The mob's entity id.
        entity_id: i32,
    },
}

/// One detonation [`MobSim::tick`] triggered this tick, for
/// [`take_detonations`](MobSim::take_detonations) to hand a driver — the
/// minimum a [`ServerProtocol::encode_explode`](crate::protocol::ServerProtocol::encode_explode)
/// call needs. This crate tracks no block-destruction model, so there is
/// nothing else (a block list, a knockback vector) to carry yet; the remaining
/// explosion fields are intentionally absent from this event.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Detonation {
    /// The blast's centre, in world space.
    pub centre: Vec3,
    /// The blast radius (`CREEPER_EXPLOSION_RADIUS` for every producer
    /// today).
    pub radius: f32,
}

/// One player struck by a hostile mob's melee attack this tick, for
/// [`take_player_hits`](MobSim::take_player_hits) to hand a driver.
///
/// `SimMob::attack_target_id` names only "another live `SimMob`" by its own
/// doc comment, so it structurally cannot carry a player: the goal seam
/// targets and attacks a bare `Vec3`, never an identity. This is resolved by matching that target
/// position against `self.players`' [`feed_perception`]-fed positions in the
/// same tick's [`tick`](MobSim::tick) — safe because nothing mutates a
/// player's fed position between the feed at the top of the tick and the
/// goal ticks that consume it. A grudge-target attack (the anger-gated
    /// anger-target row) can miss this match: its target is a
/// position remembered from whenever the grudge was set, not refreshed to
/// the player's current position, so a moved player will not match. That is
/// a disclosed gap, not a silent one — ordinary hostile-melee (zombie,
/// skeleton, …) always targets the live `nearest_player` feed and matches
/// every time.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlayerHit {
    /// Who was hit.
    pub identity: PlayerIdentity,
    /// Raw `ATTACK_DAMAGE`, unreduced — the driver runs it through the real
    /// armour/i-frame pipeline via `PlayerVitals::apply_damage`, the same
    /// split [`SimMob::apply_damage`] draws for a mob victim.
    pub raw_damage: f32,
    /// The attacking mob's position, for the driver's hurt-direction/knockback
    /// calculation (`crate::vitals::HurtDirection::from_source`).
    pub attacker_pos: Vec3,
}

/// One vanilla zombie hurt-handler reinforcement roll that passed — the *decision*
/// only. Vanilla's own hurt-handler then searches up to 50 candidate positions
/// against the live world for a valid one (vanilla's own spawn-position-ok check,
/// no player within 7 blocks, unobstructed, no collision, no liquid unless
/// the species tolerates it) and only spawns if one is found; this sim holds
/// no live world (`world: &'w ChunkWorld` is an immutable borrow, same reason
/// [`pending_lightning_fires`](MobSim::pending_lightning_fires) exists), so
/// the search and the actual spawn are the driver's job — see
/// [`take_reinforcement_calls`](MobSim::take_reinforcement_calls).
#[derive(Debug, Clone, PartialEq)]
pub struct ReinforcementCall {
    /// The calling zombie's position — a per-axis floor of its own position, the
    /// search's own origin.
    pub position: Vec3,
    /// The reinforcement's own entity type — always the caller's own type,
    /// so a husk calls in a husk and so on.
    pub entity_type: ResourceKey,
    /// Who the reinforcement should target on arrival — the caller's own
    /// current attack target if it has one, else the attacker that just hit
    /// it (vanilla's own hurt-handler's own "no live target" fallback).
    pub target_id: i32,
}

/// Vanilla's own elder-guardian effect-interval constant — the aura's cadence in ticks
/// (vanilla's own AI step gates on `(tickCount + getId()) % 1200 == 0`).
///
/// This sim tracks no per-mob generic tick-count field (only [`SimMob::age`], which
/// is the *growth* timer, and [`MobSim::tick_count`], the world's own tick
/// counter) — the same substitution [`bee_sting_death_roll`]'s own doc
/// already uses `tick_count` for. Mixing the world tick with the mob's id
/// keeps the same per-mob stagger vanilla's own entity-id offset gives (two elder
/// guardians spawned on the same tick still pulse on different ticks), and
/// the periodicity is unaffected by the offset between "ticks this world has
/// run" and "ticks since this particular mob was created" — both are exact
/// multiples of `ELDER_GUARDIAN_EFFECT_INTERVAL` apart.
const ELDER_GUARDIAN_EFFECT_INTERVAL: u64 = 1200;

/// Vanilla's own elder-guardian effect-radius constant, in blocks — spherical,
/// a distance check in vanilla's own "add effect to players around" helper,
/// not a box.
pub const ELDER_GUARDIAN_EFFECT_RADIUS: f64 = 50.0;

/// Vanilla's own elder-guardian effect-duration constant, in ticks — how long each pulse's
/// `minecraft:mining_fatigue` application lasts.
pub const ELDER_GUARDIAN_EFFECT_DURATION: i32 = 6000;

/// Vanilla's own elder-guardian effect-amplifier constant — Mining Fatigue III (0-indexed amplifier
/// `2`).
pub const ELDER_GUARDIAN_EFFECT_AMPLIFIER: u32 = 2;

/// One player caught in an elder guardian's mining-fatigue pulse this tick —
/// vanilla's own elder-guardian AI step calling
/// its own "add effect to players around" helper, for
/// [`take_mining_fatigue_auras`](MobSim::take_mining_fatigue_auras) to hand a
/// driver. The same handoff shape as [`PlayerHit`] above and for the
/// identical reason: this sim owns no connection, so it can neither reach a
/// player's `ActiveEffects` (that lives on the driver's own `Player` state)
/// nor send a game-event packet.
///
/// # What the consumer owes vanilla
///
/// For each returned identity, whose gamemode the driver — not this sim,
/// which tracks no gamemode — must confirm is survival (or adventure;
/// vanilla's own "is survival" check's own definition) before doing either of the
/// following, per vanilla's own "add effect to players around" helper:
///
/// * Call `ActiveEffects::apply("minecraft:mining_fatigue",
///   `[`ELDER_GUARDIAN_EFFECT_DURATION`]`, `[`ELDER_GUARDIAN_EFFECT_AMPLIFIER`]`)`.
///   `apply`'s own "only take over if stronger or ending sooner" semantics
///   already implement vanilla's redundant-application guard, so this list is
///   **not** pre-filtered by the target's current effect — every player
///   within radius is reported every pulse, exactly as
///   vanilla's own helper's player query is unconditional on the
///   *distance* clause and only the effect clause is conditional.
/// * Send that player's connection a `GUARDIAN_ELDER_EFFECT` game event
///   (vanilla's own generic game-event packet), the screen-darkening warning — vanilla's
///   own "silent ? 0.0 : 1.0" parameter has no sim-side equivalent
///   (silence is a per-mob NBT flag this sim does not model for elder
///   guardians), so the driver should send `1.0`.
///
/// This sim deliberately does **not** replicate vanilla's own "is allied to" check:
/// nothing in this codebase gives a mob a scoreboard team, so every survival
/// player in range is always a valid target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MiningFatigueAura {
    /// Who was caught in the pulse.
    pub target: PlayerIdentity,
}

/// One projectile-vs-block impact [`MobSim::resolve_projectile_impacts`] found
/// for [`take_projectile_block_hits`](MobSim::take_projectile_block_hits)
/// to hand a driver — the same handoff shape as
/// [`pending_grazes`](MobSim::pending_grazes)/[`pending_lightning_fires`](MobSim::pending_lightning_fires)
/// and for the identical reason: `MobSim::world` is an immutable pathfinding
/// snapshot, not the live `ChunkStore`, so this sim can neither read the real
/// current block state (to check it is actually still a `minecraft:target`)
/// nor write a new one, and has no `ScheduledTickQueue` to consult for
/// `redstone_target::apply_hit`'s `has_pending_decay` guard. This is deliberately
/// data about *every* block a projectile stopped against, not just a target —
/// the driver is what already knows which block is there and dispatches
/// accordingly, the same division `pending_lightning_fires` draws.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ProjectileBlockHit {
    /// The struck cell.
    pub pos: BlockPos,
    /// Which face axis the hit entered through —
    /// `crate::redstone_target::redstone_strength`'s own `hit_axis` parameter.
    pub axis: crate::redstone_target::HitAxis,
    /// The hit point's fractional position within the cell, each in `[0.0,
    /// 1.0]` — vanilla's own per-axis fractional-part helper applied to the
    /// hit location.
    pub frac: Vec3,
    /// Whether the projectile was an arrow (`redstone_target::activation_duration`'s
    /// 20-vs-8-tick split) — the base arrow entity, not the spectral arrow or
    /// trident,
    /// carrying their own subclass distinctions this sim does not model; see
    /// [`resolve_projectile_impacts`](MobSim::resolve_projectile_impacts) for
    /// exactly which registry paths set this.
    pub is_arrow: bool,
}

// The integrated server owns the sim behind an `Arc<Mutex<…>>` and hands it to
// a `tokio::spawn`ed connection task as an `EntitySource`, which requires
// `Send`. `MobSim` stores goals as `Box<dyn Goal>`, so this holds only because
// `Goal: Send`; pin it here so a future `!Send` goal or field fails to compile
// with a clear pointer, instead of surfacing as an opaque spawn error at the
// call site.
const _: fn() = || {
    fn assert_send<T: Send>() {}
    assert_send::<MobSim<'static>>();
};

/// Vanilla's own falling-block entity type's registry key — the falling-block twin of
/// [`item_entity_type`], and parsed per call for the same reason that one is: a
/// falling block is a rare, short-lived entity, and the parse is cheaper than the
/// `OnceLock` clone it would replace.
fn falling_block_entity_type() -> ResourceKey {
    crate::gravity_tick::FALLING_BLOCK_ENTITY_TYPE
        .parse()
        .expect("`minecraft:falling_block` is a valid resource key")
}

mod collision;
use collision::{LiveBlockCollision, ITEM_DIMENSIONS, VOID_DESPAWN_DEPTH, settle_item, settle_mob};
/// The entity-type key every dropped item streams as.
///
/// `minecraft:item` is the entity type; the *stack* is metadata. Naming the key
/// rather than the numeric id keeps this crate version-free, exactly as
/// `crate::players`' `player_entity_type` does for `minecraft:player` — and for
/// the same reason: `entity_type_id(name).unwrap_or(0)` on the encode side turns
/// a wrong key into `minecraft:acacia_boat` with no error, so the key is worth
/// stating once in one place.
fn item_entity_type() -> ResourceKey {
    "minecraft:item"
        .parse()
        .expect("`minecraft:item` is a valid resource key")
}

/// Default seed for [`MobSim`]'s tame-roll stream. Arbitrary and fixed, exactly
/// like [`ORB_BEHAVIOR_SEED`] — what matters is that it is a *separate* stream,
/// so a tame attempt cannot shift which roll a spawn or a despawn pass sees.
/// Replace it per test with [`MobSim::set_tame_rng`].
const TAME_ROLL_SEED: u64 = 0x5441_4d45_5f52_4f4c;

/// Default seed for [`MobSim::zombie_conversion_rng`]. See [`TAME_ROLL_SEED`]
/// for why it is separate. ASCII `"ZVILLAGE"`.
const ZOMBIE_VILLAGER_CONVERSION_SEED: u64 = 0x5A56_494C_4C41_4745;

/// Default seed for [`MobSim::gossip_spread_rng`]. See [`TAME_ROLL_SEED`] for
/// why it is separate. ASCII `"GOSSIPRN"`.
const GOSSIP_SPREAD_SEED: u64 = 0x474F_5353_4950_524E;

/// Default seed for the breeding experience-orb stream. See
/// [`TAME_ROLL_SEED`] for why it is separate.
const BREED_XP_SEED: u64 = 0x4252_4545_445f_5850;

/// Default seed for [`MobSim::patrol_rng`]. See [`TAME_ROLL_SEED`] for why it
/// is separate.
const PATROL_SPAWN_SEED: u64 = 0x5041_5452_4f4c_5f52;

/// Default seed for [`MobSim::equipment_rng`]. See [`TAME_ROLL_SEED`] for why
/// it is separate.
const EQUIPMENT_ROLL_SEED: u64 = 0x4551_5549_505f_524f;

/// Default seed for [`MobSim::goat_horn_rng`] — vanilla's own goat
/// spawn-finalization's own
/// pre-broken-horn roll. See [`TAME_ROLL_SEED`] for why it is separate.
/// ASCII `"GOATHORN"`.
const GOAT_HORN_ROLL_SEED: u64 = 0x474F_4154_484F_524E;

/// Default seed for [`MobSim::door_rng`]. See [`TAME_ROLL_SEED`] for why it
/// is separate. ASCII `"DOORBRKS"`.
const DOOR_BREAK_ROLL_SEED: u64 = 0x444F_4F52_4252_4B53;

/// Default seed for [`MobSim::reinforcement_rng`]. See [`TAME_ROLL_SEED`] for
/// why it is separate. ASCII `"REINFORC"`.
const REINFORCEMENT_ROLL_SEED: u64 = 0x5245_494E_464F_5243;

/// Default seed for [`MobSim::gateway_shuffle_rng`]. See [`TAME_ROLL_SEED`]
/// for why it is separate. ASCII `"GATEWAYS"`.
const GATEWAY_SHUFFLE_SEED: u64 = 0x4741_5445_5741_5953;

/// Vanilla's own zombie hurt-handler's own local (`existingAmount - 0.05`) — the permanent
/// amount subtracted from the caller's own `SPAWN_REINFORCEMENTS_CHANCE`
/// base each time it successfully calls one in, so a single zombie cannot
/// call in an unbounded chain every tick it stays hurt.
const ZOMBIE_REINFORCEMENT_CALLER_CHARGE: f64 = 0.05;

/// Vanilla's own zombie reinforcement-callee-charge constant's amount (`-0.05F`,
/// as an add-value attribute modifier) — see
/// [`SimMob::apply_reinforcement_callee_charge`]'s own doc.
const ZOMBIE_REINFORCEMENT_CALLEE_CHARGE: f64 = 0.05;

/// The `early_game.json` timeline's `gameplay/can_pillager_patrol_spawn` gate,
/// transcribed as a plain tick count rather than read from a general timeline
/// engine — this crate has no `EnvironmentAttributes`/timeline reader at all,
/// and building one is out of scope for one boolean keyframe.
/// `.cache/mc/26.2/src/data/minecraft/timeline/early_game.json`'s track has
/// exactly two keyframes: `false` at tick `0`, `true` at tick `120000`, and
/// no in-between ramp, so a single threshold constant reproduces it exactly —
/// unlike [`patrol_group_size`], which approximates a genuinely continuous
/// vanilla formula.
const PATROL_TIMELINE_GATE: u64 = 120_000;

/// Vanilla's own wandering-trader spawner's own constants —
/// default tick-delay/default spawn-delay/min-spawn-chance/
/// max-spawn-chance/spawn-chance-increase.
const WANDERING_TRADER_TICK_DELAY: i32 = 1200;
const WANDERING_TRADER_SPAWN_DELAY: i32 = 24_000;
const WANDERING_TRADER_MIN_SPAWN_CHANCE: i32 = 25;
const WANDERING_TRADER_MAX_SPAWN_CHANCE: i32 = 75;
const WANDERING_TRADER_SPAWN_CHANCE_INCREASE: i32 = 25;

/// Default seed for [`MobSim::trader_rng`]. See [`TAME_ROLL_SEED`] for why
/// it is separate.
const WANDERING_TRADER_SPAWN_SEED: u64 = 0x5452_4144_455f_524e;

/// The entity-type key every experience orb streams as.
///
/// Named rather than numeric for [`item_entity_type`]'s reason, which that function's
/// doc records with the measured consequence: `entity_type_id(name).unwrap_or(0)` on
/// the encode side silently turns a wrong key into `minecraft:acacia_boat`.
fn orb_entity_type() -> ResourceKey {
    "minecraft:experience_orb"
        .parse()
        .expect("`minecraft:experience_orb` is a valid resource key")
}

/// Whether `a` and `b` are within `reach` on **every** axis — an AABB-overlap test
/// stated as a per-axis comparison rather than a radius.
///
/// The distinction is the one `merge_neighbouring_items` records: vanilla's merge
/// searches are box intersections, and a Euclidean radius accepts a diagonal pair the
/// box rejects.
fn within_box(a: Vec3, b: Vec3, reach: f64) -> bool {
    (a.x - b.x).abs() < reach && (a.y - b.y).abs() < reach && (a.z - b.z).abs() < reach
}

/// Vanilla's own player eye-height getter for a standing player — the
/// player entity type's own dimensions table's
/// eye-height value, `1.62`.
///
/// Used only for `followNearbyPlayer`'s aim point, which is *half* this above the
/// player's feet.
const PLAYER_EYE_HEIGHT: f64 = 1.62;

/// Squared horizontal+vertical distance between two positions (vanilla
/// `distanceToSqr`).
fn dist_sqr(a: Vec3, b: Vec3) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let dz = a.z - b.z;
    dx * dx + dy * dy + dz * dz
}

/// Vanilla `Entity::push(Entity)`'s horizontal impulse pair for two
/// overlapping entities at `p_i`/`p_j` — see [`MobSim::push_entities`]'s own
/// doc comment for the overlap test this assumes has already passed.
///
/// Delegates the actual formula to
/// [`lodestone_physics::pair_push_vector`] rather than re-transcribing it: that
/// function already carries `docs/entity-push.md`'s full citation (the
/// Chebyshev-not-Euclidean normaliser, the widened `0.01f`/`0.05f` literals,
/// the `NaN`-rejecting `!(dd >= …)` form) and 150 passing tests including
/// golden traces against an independent Python oracle. Keeping one
/// implementation in the crate that already proved it, rather than a second
/// hand-rolled copy here, is what keeps the two from silently drifting apart.
///
/// Returns `None` when the pair is not within `touch` blocks horizontally
/// (this port's overlap test — see the doc comment on the caller for how it
/// narrows vanilla's real AABB intersection) or when `pair_push_vector`'s own
/// dead zone rejects a near-coincident pair. Otherwise returns
/// `(impulse_for_p_i, impulse_for_p_j)`.
fn push_impulse(p_i: Vec3, p_j: Vec3, touch: f64) -> Option<(Vec3, Vec3)> {
    let overlap_dx = p_i.x - p_j.x;
    let overlap_dz = p_i.z - p_j.z;
    if (overlap_dx * overlap_dx + overlap_dz * overlap_dz).sqrt() > touch {
        return None;
    }
    let to_physics = |v: Vec3| lodestone_physics::Vec3d { x: v.x, y: v.y, z: v.z };
    // `pair_push_vector(self, other)` returns the vector FROM `self` TOWARD
    // `other`; vanilla's own `this.push(-xa,0,-za)` / `entity.push(xa,0,za)`
    // negates it for the near side and keeps it for the far side — see that
    // function's own doc comment for why the caller, not the function, does
    // the negation.
    let v = lodestone_physics::pair_push_vector(to_physics(p_i), to_physics(p_j))?;
    Some((Vec3::new(-v.x, 0.0, -v.z), Vec3::new(v.x, 0.0, v.z)))
}

/// The position of the nearest `accept`ed item to `from`, optionally restricted
/// to an axis-aligned box of `(horizontal, vertical)` half-extents.
///
/// This is vanilla's two-step shape, kept as two steps on purpose: every
/// perception search in `ai/goal/` filters by a *box* (`getEntitiesOfClass(…,
/// getBoundingBox().inflate(dx, dy, dz))`) and only then picks the nearest by
/// squared distance (`getNearestEntity`). Collapsing it into a single radius
/// test would be wrong in the corners — most visibly for
/// [`AVOID_RANGE_Y`](AVOID_RANGE_Y), where vanilla's vertical extent is a flat
/// `3.0` regardless of the horizontal one.
fn nearest_by<T>(
    items: &[T],
    from: Vec3,
    position: impl Fn(&T) -> Vec3,
    accept: impl Fn(&T) -> bool,
    range: Option<(f64, f64)>,
) -> Option<Vec3> {
    items
        .iter()
        .filter(|item| accept(item))
        .map(|item| position(item))
        .filter(|pos| match range {
            None => true,
            Some((horizontal, vertical)) => {
                (pos.x - from.x).abs() <= horizontal
                    && (pos.z - from.z).abs() <= horizontal
                    && (pos.y - from.y).abs() <= vertical
            }
        })
        .min_by(|a, b| dist_sqr(*a, from).total_cmp(&dist_sqr(*b, from)))
}

/// The [`SimMob::patrol_target`] of the nearest **other**, patrol-*leading*
/// mob within [`PATROL_COMPANION_RANGE`] blocks of `from`, if any.
///
/// A `nearest_by`-shaped query that cannot reuse [`nearest_by`] itself: the
/// distance test there is against the *same* field the function returns, but
/// here the distance test is against a candidate's **position** (vanilla's
/// `getBoundingBox().inflate(16.0)`) while the value a follower actually wants
/// back is that candidate's **patrol target** — a different field. See
/// [`MobController::patrol_group_target`](lodestone_entity::ai::MobController::patrol_group_target)
/// for why a follower needs this at all rather than running its own census.
fn nearest_patrol_leader_target(mobs: &[SimMob<'_>], from: Vec3, exclude_id: i32) -> Option<Vec3> {
    mobs.iter()
        .filter(|m| m.id != exclude_id && m.is_patrol_leader())
        .filter(|m| dist_sqr(m.position(), from) <= PATROL_COMPANION_RANGE * PATROL_COMPANION_RANGE)
        .min_by(|a, b| dist_sqr(a.position(), from).total_cmp(&dist_sqr(b.position(), from)))
        .and_then(SimMob::patrol_target)
}

/// The highest solid-block Y at `(x, z)` within `world`'s loaded vertical
/// range, or `None` if the whole column reads air (or is unloaded) — the
/// ground a freshly seeded mob should stand on. A linear scan from the top
/// down; called only where a mob is placed rather than every tick — at seed
/// time, and from [`MobSim::run_patrol_spawn_cycle`], which itself only
/// reaches this on the rare tick a patrol attempt actually fires — so this is
/// not a hot path either way.
fn surface_y(world: &ChunkWorld, x: i32, z: i32) -> Option<i32> {
    let top = world.min_y + world.height - 1;
    (world.min_y..=top).rev().find(|&y| world.is_solid(x, y, z))
}

/// Approximates vanilla's own no-active-raid "current difficulty at
/// position, effective difficulty" formula for [`MobSim::run_patrol_spawn_cycle`]'s group
/// size, the ceiling of the effective difficulty plus one
/// (vanilla's own patrol-spawner group-size formula).
///
/// Vanilla's effective difficulty is a continuous value accumulated per
/// region over real playtime plus the current moon phase
/// (`LocalDifficulty`), roughly `0.75` (fresh Peaceful/Easy world) up to
/// `6.75` (long-played Hard, full moon). This crate tracks neither the
/// accumulation nor the moon phase, so each [`Difficulty`] enum value stands
/// in for a fixed point roughly in the middle of its own real range —
/// disclosed in [`MobSim::run_patrol_spawn_cycle`]'s own doc comment, and
/// picked so `ceil(value) + 1` lands on a group size vanilla actually
/// produces at that difficulty rather than an edge value.
fn patrol_group_size(difficulty: Difficulty) -> i32 {
    let effective: f64 = match difficulty {
        Difficulty::Peaceful => 0.0,
        Difficulty::Easy => 1.0,
        Difficulty::Normal => 2.0,
        Difficulty::Hard => 3.0,
    };
    effective.ceil() as i32 + 1
}

/// Seeds `count` zombies in a ring of radius 6 blocks around `(center_x,
/// center_z)`, each placed on the real terrain surface (skipped if the column
/// has no solid ground within `world`'s loaded range) with a baseline
/// wander/look goal set — the same defaults [`MobSim::run_spawn_cycle`] gives
/// a naturally-spawned mob.
///
/// This is **not** vanilla natural spawning: there is no light-level,
/// biome, or pack-size logic here, because no terrain/biome-aware
/// [`SpawnCandidateSource`] implementation exists in production yet (the
/// trait exists; every current impl is a test mock — see `mob_spawn.rs`).
/// Building that is a separate, considerably larger feature. This exists
/// purely so the actual subject — computed AI motion reaching the
/// wire — has a population to move; a caller that wants real spawning wires
/// [`MobSim::run_spawn_cycle`] in its place once a real source exists.
fn seed_demo_mobs(sim: &mut MobSim<'_>, center_x: i32, center_z: i32, count: usize) {
    let world = sim.world();
    // `count`, **not** `count.max(1)`. The floor was here until singleplayer
    // needed to be mob-free: it made a request for zero demo mobs silently
    // produce one zombie, so "turn the demo population off" was not expressible
    // at all. Vanilla does not seed a demo population; a caller asking for none
    // must get none.
    for i in 0..count {
        let species = DEMO_SPECIES[i % DEMO_SPECIES.len()];
        let key = ResourceKey::from_str(&format!("minecraft:{species}"))
            .expect("DEMO_SPECIES entries are valid paths");
        let angle = (i as f64) * std::f64::consts::TAU / (count.max(1) as f64);
        let x = center_x + (angle.cos() * 6.0).round() as i32;
        let z = center_z + (angle.sin() * 6.0).round() as i32;
        let Some(y) = surface_y(world, x, z) else {
            continue;
        };
        let pos = Vec3::new(f64::from(x) + 0.5, f64::from(y + 1), f64::from(z) + 0.5);
        // Through `spawn_species`, not `spawn` plus a hardcoded component set.
        // This is the **only** production path that creates a
        // mob a connected client can see, so it is also the only place the
        // per-species roster can reach pixels: routed this way, a demo zombie
        // gets the complete target-selection, attack, and look-at behavior
        // instead of wandering obliviously past the player.
        //
        // The shape, speed and A* budget were hardcoded here as `0.6 × 1.95`,
        // `0.23` and `400`; `spawn_species` derives the first two from the same
        // dimension census and `movement_speed` attribute and gets the same
        // numbers, and the third from `follow_range * 16` = `560`, preserving
        // the measured follow-range budget rather than a call-site guess.
        sim.spawn_species(key, pos);
    }
}

/// The species [`seed_demo_mobs`] cycles through, in order.
///
/// # What this is for
///
/// [`seed_demo_mobs`] cycles a client-visible demonstration roster. The list
/// covers every roster family plus an additional hostile entry, making each
/// family observable to a connected client while keeping this helper separate
/// from spawn eggs and spawner blocks.
///
/// # Order is load-bearing, twice
///
/// The seeder cycles this list, so with production's `mob_count` of 6
/// (`lodestone-shell/src/net.rs`) a player sees exactly the **first six**
/// entries. Those six are therefore one per roster family plus one, so that a
/// default singleplayer world exercises every family rather than six variations
/// on a monster:
///
/// | # | species | family |
/// |---|---|---|
/// | 0 | `zombie` | `hostile_melee` |
/// | 1 | `cow` | `passive` |
/// | 2 | `wolf` | `neutral` |
/// | 3 | `blaze` | `ranged` |
/// | 4 | `guardian` | `specialist` |
/// | 5 | `creeper` | `hostile_melee` (the swelling behavior is the most visible) |
///
/// `zombie` is first for a second, narrower reason: `MobSim::set_next_id(1000)`
/// plus spawn order makes entity id 1000 deterministic, and
/// `crates/protocol/v770/tests/live_mob_sim.rs` relies on that. Keeping the
/// zombie at index 0 leaves the *first* demo mob exactly what it has always
/// been.
///
/// # Gotcha when adding to this list
///
/// Every entry must be a species some roster family claims, or it silently
/// spawns with `roster::FALLBACK` (wander and look) — visible, but proving
/// nothing about any goal table. `demo_species_are_all_rostered_and_span_every_family`
/// fails rather than letting that through. An entry also needs a
/// `type_spec` arm in `lodestone_entity::attribute`, or it runs at the 0.7
/// registry default; that is pinned separately by
/// `every_rostered_species_has_a_type_spec_arm`.
///
/// This is still a demo ring on flat ground, not natural spawning — a guardian
/// on land is a real consequence and an accepted one, since the alternative is
/// that `specialist.rs` stays unobservable.
pub const DEMO_SPECIES: &[&str] = &[
    "zombie",
    "cow",
    "wolf",
    "blaze",
    "guardian",
    "creeper",
    // Beyond production's count of 6, but reached by any caller asking for
    // more, and each one another family's table on screen.
    "skeleton",
    "spider",
    "sheep",
    "chicken",
    "enderman",
    "snow_golem",
];

#[cfg(test)]
mod tests;
