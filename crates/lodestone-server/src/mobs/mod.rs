//! Server-side mob simulation — the *consumer* that ticks mob AI.
//!
//! `lodestone-entity` owns a complete goal scheduler, A\* pathfinder, and the
//! [`NavigatingMob`] composition that wires them together over the version-free
//! [`PathWorld`] seam. The server tick loop advances this simulation and
//! publishes its snapshots to the connection layer. Clients interpolate those
//! positions; mob decisions and movement remain server-side in this module.
//!
//! Two pieces, deliberately kept separate rather than fused:
//!
//! * [`ChunkWorld`] adapts the server's own [`ChunkColumn`] terrain (which
//!   stores complete block-state strings, not just a solid/air bit — see its
//!   own doc comment) into a [`PathWorld`]. It is the terrain adapter for
//!   `lodestone-render`'s `world.rs`: this crate owns terrain *storage*,
//!   `lodestone-entity` owns the traversal reasoning, and the adapter is the
//!   single seam between them. It classifies each cell through the real
//!   26.2 per-block-state census (`lodestone_data::path_types` +
//!   `collision_shapes`) rather than a solid/air guess — and it
//!   stays version-free doing it, because `lodestone-data` is 26.2 *game*
//!   data (tags, collision geometry, ...) with no protocol dependency of its
//!   own (`docs/lodestone-data-crate.md`), not a `crates/protocol/*` crate.
//!   `base_path_type`/`collision_top` distinguish water, lava, fences,
//!   trapdoors, and damaging blocks for navigation. `PathWorld::collides`
//!   intentionally keeps the coarse jump-clearance/diagonal-reach sweep over
//!   [`ChunkColumn::is_solid`]; shape-aware AABB checks remain outside this
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

use lodestone_data::{block_states, collision_shapes, entity_dimensions, entity_types, path_types};
// `collide` and `CollisionView` are the item pass's swept resolve against the real
// per-state shape census (see `ItemCollision`); `Vec3d` is the physics crate's own
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
use lodestone_entity::pathfinding::{Aabb, BlockCues, MobShape, PathType, PathWorld};
use lodestone_entity::projectile::{Projectile, ProjectileRegistry, TrackedProjectile};
use lodestone_entity::spawn_equipment::{self, EquipRandom};
use lodestone_entity::{
    AttributeMap, DamageFlags, Defenses, HurtCooldown, HurtDecision, RayView, entity_damage,
    seen_percent,
};
use lodestone_model::{BlockPos, Difficulty, Identifier, ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use crate::chunk::{AIR, ChunkColumn, ChunkSource};
use crate::gravity_tick::FallingBlockEffect;
use crate::protocol::{EntitySnapshot, MetadataField};
use crate::mob_spawn::{
    DespawnOutcome, MobCategory, SpawnCandidateSource, SpawnRng, SpawnState, check_despawn,
};
use crate::server::EntitySource;

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

// No re-export: every method here was already `pub`.
mod falling_blocks;

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
    let base = entity_types::entity_type_id_parts(entity_type.namespace(), entity_type.path())
        .and_then(entity_dimensions::base_dimensions);
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

impl<'w> SimMob<'w> {
    /// The entity id assigned at spawn.
    #[must_use]
    pub fn id(&self) -> i32 {
        self.id
    }

    /// Adds a prioritised goal (higher priority preempts lower on shared flags),
    /// returning `&mut self` so goals can be chained at spawn.
    pub fn add_goal(&mut self, priority: i32, goal: Box<dyn Goal>) -> &mut Self {
        self.goals.add(priority, goal);
        self
    }

    /// Sets the mob's current attack target.
    pub fn set_attack_target(&mut self, target: Option<Vec3>) {
        self.mob.set_attack_target(target);
    }

    /// Puts this animal into love mode for
    /// [`LOVE_TICKS`](lodestone_entity::ai::navigating_mob::LOVE_TICKS)
    /// (vanilla's own animal "set in love" call) — what feeding
    /// it a breeding item does. [`MobSim::tick`]'s partner search only
    /// considers mobs in this state.
    pub fn set_in_love(&mut self) -> &mut Self {
        self.mob.set_in_love();
        self
    }

    /// Whether this animal is currently in love mode.
    #[must_use]
    pub fn is_in_love(&self) -> bool {
        self.mob.is_in_love()
    }

    /// Remaining love-mode ticks (vanilla's own love-time getter).
    #[must_use]
    pub fn love_time(&self) -> i32 {
        self.mob.love_time()
    }

    /// The mob's age timer: negative while a baby (counting up to `0`),
    /// positive as the post-breeding parent cooldown (counting down to `0`).
    #[must_use]
    pub fn age(&self) -> i32 {
        self.mob.age()
    }

    /// Sets the age timer — e.g.
    /// [`BABY_START_AGE`](lodestone_entity::ai::navigating_mob::BABY_START_AGE)
    /// to spawn this mob as a baby.
    ///
    /// **Also re-derives the hitbox and movement step when this crosses the
    /// baby/adult boundary** — vanilla's own ageable-mob age setter unconditionally
    /// refreshes dimensions,
    /// and until this only [`spawn_species`](Self) ever computed
    /// [`species_shape`]: a mob bred into babyhood, or one growing up, kept
    /// its spawn-time adult box and adult step speed forever. Gated on the
    /// boundary rather than run on every call, since a baby's per-tick
    /// countdown from [`BABY_START_AGE`](lodestone_entity::ai::navigating_mob::BABY_START_AGE)
    /// to `0` would otherwise re-resolve both twenty-four thousand times for
    /// no observable change.
    pub fn set_age(&mut self, age: i32) -> &mut Self {
        let was_baby = self.mob.is_baby();
        self.mob.set_age(age);
        let is_baby = self.mob.is_baby();
        if is_baby != was_baby {
            let attrs = default_attributes(&self.entity_type).unwrap_or_else(AttributeMap::new);
            let mut shape = species_shape(&self.entity_type, &attrs, is_baby);
            // `can_open_doors` for the zombie family is a spawn-time RNG roll
            // (see `MobSim::spawn_species`), not a function of size — preserve
            // it across this refresh rather than re-deriving the static
            // per-species default, which would silently reset a zombie that
            // rolled `true` back to `false` the moment it grows up.
            shape.can_open_doors = self.mob.shape().can_open_doors;
            self.mob.set_shape(shape);
            let base_speed = attr(&attrs, "movement_speed");
            let multiplier = if is_baby {
                baby_speed_multiplier(&self.entity_type)
            } else {
                1.0
            };
            self.mob.set_step_per_tick(ai_ground_speed(base_speed * multiplier));
        }
        self
    }

    /// This mob's current per-tick movement step — reflects
    /// [`baby_speed_multiplier`] once [`set_age`](Self::set_age) has crossed
    /// the baby/adult boundary.
    #[must_use]
    pub fn step_per_tick(&self) -> f64 {
        self.mob.step_per_tick()
    }

    /// Whether this mob is a baby (`age < 0`), which gates following a parent
    /// and excludes it from breeding.
    #[must_use]
    pub fn is_baby(&self) -> bool {
        self.mob.is_baby()
    }

    /// Whether this mob is inside its post-damage panic window
    /// ([`PANIC_DAMAGE_TICKS`](lodestone_entity::ai::navigating_mob::PANIC_DAMAGE_TICKS)).
    #[must_use]
    pub fn is_panicking(&self) -> bool {
        self.mob.is_panicking()
    }

    /// Vanilla's own "is scared" check — whether this mob is currently curled up
    /// (halved incoming damage; see [`apply_damage`](Self::apply_damage)).
    /// Always `false` for a non-armadillo species, where the backing field
    /// never leaves `0`.
    #[must_use]
    pub fn armadillo_is_scared(&self) -> bool {
        self.armadillo_danger_ticks > 0
    }

    /// Vanilla's own "is playing dead" check — whether this mob is currently in its
    /// play-dead window (see [`apply_damage`](Self::apply_damage)'s own
    /// axolotl arm for the trigger). Always `false` for a non-axolotl
    /// species, where the backing field never leaves `0`.
    #[must_use]
    pub fn axolotl_is_playing_dead(&self) -> bool {
        self.axolotl_play_dead_ticks > 0
    }

    /// Vanilla's own "is camel sitting" check. Always `false` for a non-camel species,
    /// where the backing field never leaves its default.
    #[must_use]
    pub fn camel_is_sitting(&self) -> bool {
        self.camel_sitting
    }

    /// Vanilla's own "is dashing" check — see the `camel_dash_cooldown` field's own doc
    /// for the disclosed "fixed minimum duration, not a real
    /// landing-triggered reset" narrowing this derives from. Always `false`
    /// for a non-camel species, where the backing field never leaves `0`.
    #[must_use]
    pub fn camel_is_dashing(&self) -> bool {
        self.camel_dash_cooldown > CAMEL_DASH_COOLDOWN_TICKS - CAMEL_DASH_MINIMUM_DURATION_TICKS
    }

    /// The position of the note block this allay most recently heard and
    /// still remembers (vanilla's own liked-note-block-position memory), if the
    /// cooldown hasn't lapsed — see the backing field's own doc. `None` for
    /// every non-allay species.
    #[must_use]
    pub fn allay_liked_noteblock(&self) -> Option<Vec3> {
        self.allay_liked_noteblock.and_then(|(pos, ticks)| (ticks > 0).then_some(pos))
    }

    /// How many items this allay is currently carrying beyond the one held
    /// in its hand — see the backing field's own doc. `0` for every
    /// non-allay species.
    #[must_use]
    pub fn allay_inventory_count(&self) -> u32 {
        self.allay_inventory_count
    }

    /// The position of whatever most recently hurt this mob, while inside the
    /// retaliation window
    /// ([`LAST_HURT_BY_TICKS`](lodestone_entity::ai::navigating_mob::LAST_HURT_BY_TICKS)).
    #[must_use]
    pub fn last_hurt_by(&self) -> Option<Vec3> {
        self.mob.last_hurt_by()
    }

    /// Whether the mob's feet cell holds water, read from the world (never
    /// injected) — the input for floating behavior.
    #[must_use]
    pub fn in_water(&self) -> bool {
        self.mob.in_water()
    }

    /// Whether the mob's feet cell holds lava.
    #[must_use]
    pub fn in_lava(&self) -> bool {
        self.mob.in_lava()
    }

    /// The nearest-player position [`MobSim::tick`] last fed this mob, if any.
    /// `None` when no player is known — including when nothing has ever called
    /// [`MobSim::set_players`], which is still the case in production; see that
    /// method's doc comment.
    #[must_use]
    pub fn nearest_player(&self) -> Option<Vec3> {
        self.mob.nearest_player()
    }

    /// The tempting-entity position [`MobSim::tick`] last fed this mob.
    #[must_use]
    pub fn temptation(&self) -> Option<Vec3> {
        self.mob.temptation()
    }

    /// The threat position [`MobSim::tick`] last fed this mob, from
    /// [`avoided_species`]'s table.
    #[must_use]
    pub fn avoid_threat(&self) -> Option<Vec3> {
        self.mob.avoid_threat()
    }

    /// The nearest-adult position [`MobSim::tick`] last fed this mob, which is
    /// the target for parent-following behavior. Always `None` for an adult.
    #[must_use]
    pub fn parent_candidate(&self) -> Option<Vec3> {
        self.mob.parent_position()
    }

    /// Who owns this mob, if anyone. `None` for a wild (untamed) mob.
    #[must_use]
    pub fn owner(&self) -> Option<MobOwner> {
        self.owner
    }

    /// The **mob** id of this mob's owner, if it is owned by a mob. `None` both
    /// for a wild mob and for one owned by a *player* — read
    /// [`owner`](Self::owner) when the difference matters.
    #[must_use]
    pub fn owner_id(&self) -> Option<i32> {
        match self.owner {
            Some(MobOwner::Mob(id)) => Some(id),
            _ => None,
        }
    }

    /// The uuid of this mob's owner, if it is owned by a player.
    #[must_use]
    pub fn owner_uuid(&self) -> Option<Uuid> {
        match self.owner {
            Some(MobOwner::Player(uuid)) => Some(uuid),
            _ => None,
        }
    }

    /// Sets this mob's owner id (the mob-to-mob flavour of
    /// [`set_owner`](Self::set_owner)).
    pub fn set_owner_id(&mut self, owner_id: Option<i32>) -> &mut Self {
        self.set_owner(owner_id.map(MobOwner::Mob))
    }

    /// Sets this mob's owner — vanilla's own tamed-animal owner setter.
    ///
    /// **Does not set the tame flag**, and the asymmetry is vanilla's:
    /// vanilla's own owner-reference setter sets the owner-uuid metadata field and *then* calls
    /// the tame-flags setter with the tame bit set but the "gift particles" bit clear, two separate pieces of state.
    /// [`tame`](Self::tame) is the call that
    /// does both, and is what a taming interaction should use.
    pub fn set_owner(&mut self, owner: Option<MobOwner>) -> &mut Self {
        self.owner = owner;
        self
    }

    /// Resets this mob's own [`shoulder_dismount_ticks`](Self::shoulder_dismount_ticks)
    /// counter — called with `0` the tick a dismounted parrot respawns, the
    /// same way vanilla's `rideCooldownCounter` starts at `0` on a fresh
    /// entity.
    pub fn set_shoulder_dismount_ticks(&mut self, ticks: i32) -> &mut Self {
        self.shoulder_dismount_ticks = ticks;
        self
    }

    /// What a lead currently ties this mob to, if anything.
    #[must_use]
    pub fn leash_holder(&self) -> Option<LeashHolder> {
        self.leash_holder
    }

    /// Whether a lead is currently attached — vanilla's own "is leashed" check,
    /// which additionally requires a non-null leash holder; this sim has no
    /// "has leash data but no resolved holder" state (see the field's own
    /// doc comment), so `Some` and "leashed" coincide exactly.
    #[must_use]
    pub fn is_leashed(&self) -> bool {
        self.leash_holder.is_some()
    }

    /// Directly sets the leash holder, bypassing [`MobSim::try_leash`]'s
    /// distance/species gating — for a host that has already decided (e.g.
    /// restoring a save, or [`MobSim::try_leash_to_fence`]'s re-parent of an
    /// already-leashed mob onto a fresh knot).
    pub fn set_leash_holder(&mut self, holder: Option<LeashHolder>) -> &mut Self {
        self.leash_holder = holder;
        self
    }

    /// Whether this mob is tame — vanilla's own "is tame" check.
    ///
    /// Distinct from [`owner_position`](Self::owner_position) being `Some`: a
    /// tamed pet whose owner is offline is still tame.
    #[must_use]
    pub fn is_tame(&self) -> bool {
        self.tame
    }

    /// Tames this mob to `owner` — vanilla's own tame-to-player call, which is
    /// the tame-flags setter with both bits set, plus the owner setter.
    pub fn tame(&mut self, owner: MobOwner) -> &mut Self {
        self.owner = Some(owner);
        self.tame = true;
        self.mob.set_tame(true);
        self
    }

    /// Whether the owner has told this mob to sit — vanilla's own
    /// "is ordered to sit" check, the persisted intent.
    #[must_use]
    pub fn is_ordered_to_sit(&self) -> bool {
        self.ordered_to_sit
    }

    /// Sets the sitting order — vanilla's own "set ordered to sit" call.
    ///
    /// Pushes straight through to the [`NavigatingMob`] as well as recording it
    /// here, so `SitWhenOrderedToGoal` sees the order on the *same* tick rather
    /// than one tick late. Every other perception input is refreshed by
    /// [`MobSim::feed_perception`], but an order given by an interaction arrives
    /// between ticks and a one-tick lag is visible as a pet that ignores the
    /// first click.
    pub fn set_ordered_to_sit(&mut self, ordered_to_sit: bool) -> &mut Self {
        self.ordered_to_sit = ordered_to_sit;
        self.mob.set_ordered_to_sit(ordered_to_sit);
        self
    }

    /// Whether this mob is currently in the sitting **pose** —
    /// `SitWhenOrderedToGoal`'s observable output, which is what the `0x01`
    /// bit of vanilla's shared entity-flags metadata field carries. Read this to answer "did the goal run",
    /// and [`is_ordered_to_sit`](Self::is_ordered_to_sit) to answer "was it
    /// told to".
    #[must_use]
    pub fn is_in_sitting_pose(&self) -> bool {
        self.mob.is_in_sitting_pose()
    }

    /// Whether this mob is part of an active pillager patrol — vanilla's own
    /// patrolling-monster "is patrolling" check. Kept only on the [`NavigatingMob`]
    /// (unlike [`tame`](Self::tame)/[`owner`](Self::owner)): nothing outside the
    /// AI seam and [`MobSim`]'s own patrol census reads it, so there is no
    /// second host-side record to keep in sync.
    #[must_use]
    pub fn is_patrolling(&self) -> bool {
        self.mob.is_patrolling()
    }

    /// Whether this mob leads its patrol — vanilla's own
    /// patrolling-monster "is patrol leader" check.
    #[must_use]
    pub fn is_patrol_leader(&self) -> bool {
        self.mob.is_patrol_leader()
    }

    /// This mob's own current long-distance patrol waypoint — vanilla's own
    /// patrolling-monster patrol-target getter.
    #[must_use]
    pub fn patrol_target(&self) -> Option<Vec3> {
        self.mob.patrol_target()
    }

    /// Marks this mob as patrolling (or not) — vanilla's own
    /// patrolling-monster "set patrolling" call.
    pub fn set_patrolling(&mut self, patrolling: bool) -> &mut Self {
        self.mob.set_patrolling(patrolling);
        self
    }

    /// Marks this mob as its patrol's leader (or not) — vanilla's own
    /// patrolling-monster "set patrol leader" call. Does not also set
    /// [`patrolling`](Self::set_patrolling); see [`NavigatingMob::set_patrol_leader`]'s
    /// own doc comment for why the two are separate calls here.
    pub fn set_patrol_leader(&mut self, leader: bool) -> &mut Self {
        self.mob.set_patrol_leader(leader);
        self
    }

    /// Sets this mob's own long-distance patrol waypoint — vanilla's own
    /// patrolling-monster "set patrol target"/"find patrol target" calls.
    pub fn set_patrol_target(&mut self, target: Option<Vec3>) -> &mut Self {
        self.mob.set_patrol_target(target);
        self
    }

    /// Feeds a non-leader the patrol group's shared waypoint, as
    /// [`MobSim`]'s own per-tick census resolves it. See
    /// [`MobController::patrol_group_target`]'s own doc comment for why this
    /// exists.
    pub fn set_patrol_group_target(&mut self, target: Option<Vec3>) -> &mut Self {
        self.mob.set_patrol_group_target(target);
        self
    }

    /// Vanilla's own horse-family temper getter — how close this horse is to accepting a
    /// rider. Always `0` outside the horse family.
    #[must_use]
    pub fn temper(&self) -> i32 {
        self.temper
    }

    /// Vanilla's own horse-family temper setter, clamped to `0..=max` by the caller. Exists so a
    /// gate can stage a horse at a chosen temper instead of feeding it 34 times.
    pub fn set_temper(&mut self, temper: i32) -> &mut Self {
        self.temper = temper;
        self
    }

    /// The position of this mob's owner as the [`MobController`] seam reports
    /// it — what [`MobSim::tick`]'s feed last resolved from
    /// [`owner_id`](Self::owner_id). `None` until the feed has run, and for a
    /// wild mob.
    #[must_use]
    pub fn owner_position(&self) -> Option<Vec3> {
        self.mob.owner_position()
    }

    /// Teleports this mob directly to `pos` (the instant-relocation primitive) —
    /// the host command the enderman's damage-triggered
    /// teleport and gaze-triggered "teleport towards" reduce to. Rewrites
    /// position immediately and abandons any in-progress path (vanilla's own
    /// generic teleport-to call).
    pub fn teleport_to(&mut self, pos: Vec3) -> &mut Self {
        self.mob.teleport_to(pos);
        self
    }

    /// Records a self-inflicted damage request (the self-damage primitive) — the
    /// bee's sting self-destruct. Drained and
    /// applied by [`MobSim::tick`] through the normal damage pipeline.
    pub fn damage_self(&mut self, amount: f32) -> &mut Self {
        self.mob.damage_self(amount);
        self
    }

    /// The mob's current attack-target *position* (the point its attack
    /// behavior chases), as distinct from
    /// [`attack_target_id`](SimMob::attack_target_id)'s entity identity. This
    /// is the state retaliation writes when the mob is attacked.
    #[must_use]
    pub fn attack_target(&self) -> Option<Vec3> {
        self.mob.attack_target()
    }

    /// Whether a goal has this mob holding jump this tick — the observable
    /// effect of its water-escape behavior.
    #[must_use]
    pub fn is_jumping(&self) -> bool {
        self.mob.is_jumping()
    }

    /// The last position a goal asked this mob to look at, if any — the
    /// observable effect of `LookAtPlayerGoal`. Distinct from
    /// [`head_yaw`](SimMob::head_yaw), which is the derived angle; this is the
    /// target the goal actually chose, so a test can assert *what* the mob
    /// turned toward rather than merely that some angle changed.
    #[must_use]
    pub fn facing(&self) -> Option<Vec3> {
        self.mob.facing()
    }

    /// `no_action_time` **as the goals see it**, through the
    /// [`MobController`] seam.
    ///
    /// Deliberately separate from [`no_action_time`](SimMob::no_action_time),
    /// which reads the sim's own record. The two must stay equal: the sim
    /// increments its record every tick and goals must observe that value
    /// through the controller seam rather than the trait default `0`.
    /// Keeping both readable is what lets a test assert the equality rather
    /// than assume it.
    #[must_use]
    pub fn mob_no_action_time(&self) -> i32 {
        MobController::no_action_time(&self.mob)
    }

    /// How many goals are installed on this mob. Used to assert a
    /// [`MobSim::tick`]-spawned child inherited a goal set rather than arriving
    /// inert.
    #[must_use]
    pub fn goal_count(&self) -> usize {
        self.goals.len()
    }

    /// Marks the mob ignited, forcing a
    /// creeper's swell direction to climb every tick regardless of
    /// proximity check. A no-op for a mob whose [`NavigatingMob`] never has anything
    /// else move its swell direction off `-1` (every non-creeper species).
    pub fn ignite(&mut self) -> &mut Self {
        self.mob.ignite();
        self
    }

    /// Whether this mob is currently ignited. See [`ignite`](Self::ignite).
    #[must_use]
    pub fn is_ignited(&self) -> bool {
        self.mob.is_ignited()
    }

    /// The current fuse counter (vanilla's own creeper fuse field), `0..=MAX_SWELL`
    /// for a creeper; permanently `0` for a species nothing ever moves off
    /// [`swell_dir`](Self::swell_dir)'s `-1` default.
    #[must_use]
    pub fn swell(&self) -> i32 {
        self.mob.swell()
    }

    /// The mob's current swell direction (vanilla's own swell-direction getter).
    #[must_use]
    pub fn swell_dir(&self) -> i32 {
        self.mob.swell_dir()
    }

    /// Sets which live mob (by id) this mob's connecting melee attacks damage.
    /// The goal/navigation seam only ever deals in positions
    /// ([`set_attack_target`](Self::set_attack_target)); this is the identity
    /// [`MobSim::tick`] needs to resolve a strike into an actual
    /// [`apply_damage`](Self::apply_damage) call on the right mob.
    pub fn set_attack_target_id(&mut self, target_id: Option<i32>) -> &mut Self {
        self.attack_target_id = target_id;
        self
    }

    /// The id of the mob this one's connecting attacks currently damage, if set.
    #[must_use]
    pub fn attack_target_id(&self) -> Option<i32> {
        self.attack_target_id
    }

    /// Current health. Reaches `0.0` (never negative) when the mob has taken
    /// lethal damage; [`MobSim::tick`] removes a mob whose health is `0.0` at
    /// the end of the tick that landed the killing blow.
    #[must_use]
    pub fn health(&self) -> f32 {
        self.health
    }

    /// Overrides current health (e.g. to stage a near-death mob in a test).
    /// Clamped to `>= 0.0`.
    pub fn set_health(&mut self, health: f32) -> &mut Self {
        self.health = health.max(0.0);
        self
    }

    /// The `minecraft:max_health` attribute resolved at spawn.
    #[must_use]
    pub fn max_health(&self) -> f32 {
        self.max_health
    }

    /// Vanilla's own generic heal call: raises health toward
    /// [`max_health`](Self::max_health), never past it.
    pub fn heal(&mut self, amount: f32) -> &mut Self {
        self.health = (self.health + amount).min(self.max_health);
        self
    }

    /// Overrides the raw melee damage this mob's attacks deal, in place of the
    /// type's `ATTACK_DAMAGE` default resolved at spawn.
    pub fn set_attack_damage(&mut self, attack_damage: f32) -> &mut Self {
        self.attack_damage = attack_damage;
        self
    }

    /// The raw melee damage this mob's attacks currently deal.
    #[must_use]
    pub fn attack_damage(&self) -> f32 {
        self.attack_damage
    }

    /// Overrides this mob's defensive state (armour/toughness/absorption) in
    /// place of the type's defaults resolved at spawn.
    pub fn set_defenses(&mut self, defenses: Defenses) -> &mut Self {
        self.defenses = defenses;
        self
    }

    /// This mob's current defensive state.
    #[must_use]
    pub fn defenses(&self) -> &Defenses {
        &self.defenses
    }

    /// Overrides this mob's `minecraft:knockback_resistance` value in place
    /// of the type's default resolved at spawn.
    pub fn set_knockback_resistance(&mut self, knockback_resistance: f64) -> &mut Self {
        self.knockback_resistance = knockback_resistance;
        self
    }

    /// This mob's current `minecraft:knockback_resistance` value.
    #[must_use]
    pub fn knockback_resistance(&self) -> f64 {
        self.knockback_resistance
    }

    /// Applies a velocity impulse to this mob — see
    /// [`NavigatingMob::apply_knockback`] for the exact one-tick-displacement
    /// mechanic this forwards to.
    pub fn apply_knockback(&mut self, impulse: Vec3) {
        self.mob.apply_knockback(impulse);
    }

    /// Runs the full vanilla hit pipeline against this mob for one incoming
    /// hit of `raw_damage`: the invulnerability-frame gate
    /// ([`HurtCooldown::on_hurt`]), then armour/resistance/enchantment/
    /// absorption reduction ([`apply_reductions`](lodestone_entity::apply_reductions)),
    /// then subtracts the result from [`health`](Self::health) (floored at
    /// `0.0`). A hit fully inside the i-frame window and no stronger than the
    /// one that opened it is ignored entirely, exactly as vanilla drops a
    /// weaker follow-up hit.
    ///
    /// Returns the damage that actually reached health (`0.0` if the hit was
    /// ignored, if it was fully absorbed, or if the mob was already dead).
    pub fn apply_damage(&mut self, raw_damage: f32, flags: DamageFlags) -> f32 {
        if self.health <= 0.0 {
            return 0.0;
        }
        // Vanilla's own "is invulnerable to" check: a digging-or-emerging gate blocks every hit
        // except one tagged as bypassing invulnerability entirely (void,
        // `/kill`, and similarly out-of-world sources). That carve-out is not
        // modelled as a `DamageFlags` bit here (see
        // `lodestone_entity::damage::DamageFlags`'s field list — it has no
        // `bypasses_invulnerability`), so this is a narrower, disclosed
        // invulnerability than vanilla's: a hit that should still land during
        // emerge is blocked too. Only the `Emerging` half is modelled — see
        // `crate::mobs::warden`'s module doc for the `Digging` half this
        // crate does not build.
        if self.entity_type.path() == "warden" && self.warden_emerge_ticks > 0 {
            return 0.0;
        }
        // Vanilla's own armadillo hurt-handler override, which runs *before* the
        // invulnerability-frame check below (it wraps the generic hurt
        // handler, not the "actually hurt" one) — a curled-up armadillo halves the raw hit before
        // anything else sees it, including whether the hit even breaks
        // through i-frames.
        let raw_damage = if self.entity_type.path() == "armadillo" && self.armadillo_danger_ticks > 0 {
            ((raw_damage - 1.0) / 2.0).max(0.0)
        } else {
            raw_damage
        };
        // Vanilla's own axolotl hurt-handler — runs before the generic hurt
        // handler's own
        // invulnerability-frame gate, exactly like the armadillo halving
        // above, so this reads `self.health`/`raw_damage` directly rather
        // than the post-`on_hurt` `amount`. **Disclosed narrowing**: real
        // vanilla additionally requires a live source/direct entity
        // — this seam has no attacker-identity input to gate on, the same
        // simplification `armadillo_danger_ticks`'s own doc already
        // discloses for the same missing attacker-identity input.
        if self.entity_type.path() == "axolotl"
            && self.axolotl_play_dead_ticks <= 0
            && self.in_water()
            && raw_damage < self.health
        {
            let health_ratio = self.health / self.max_health;
            let (roll_a, roll_b) =
                axolotl_play_dead_roll(self.id as u64, self.health.to_bits(), raw_damage.to_bits());
            if roll_a == 0 && ((roll_b as f32) < raw_damage || health_ratio < 0.5) {
                self.axolotl_play_dead_ticks = AXOLOTL_PLAY_DEAD_TICKS;
            }
        }
        let amount = match self.hurt_cooldown.on_hurt(raw_damage, flags) {
            HurtDecision::Ignored => return 0.0,
            HurtDecision::Full { amount } | HurtDecision::Topup { amount } => amount,
        };
        let outcome = lodestone_entity::apply_reductions(amount, &self.defenses, flags);
        self.defenses.absorption = outcome.remaining_absorption;
        self.health = (self.health - outcome.to_health).max(0.0);
        // Every hit that is not swallowed by invulnerability opens the panic
        // window. The attacker position is carried by callers that know it;
        // environmental damage leaves the mob panicking without a retaliation
        // target.
        //
        // Placed here, in the single funnel every damage path already goes
        // through, so a new damage source cannot forget it.
        self.mob.note_hurt(None);
        // Vanilla's own armadillo hurt-handler's own tail: refresh (or start) the danger
        // memory on every hit that reached this point — see
        // `armadillo_danger_ticks`'s own doc for the disclosed narrowing
        // (real vanilla additionally requires a living-entity attacker and
        // gates the roll itself on the "can stay rolled up" check).
        if self.entity_type.path() == "armadillo" {
            self.armadillo_danger_ticks = ARMADILLO_DANGER_TICKS;
        }
        outcome.to_health
    }

    /// This mob's live status effects — see [`Self::apply_effect`] to add one.
    #[must_use]
    pub fn effects(&self) -> &crate::mob_effects::ActiveEffects {
        &self.effects
    }

    /// Applies one status effect through vanilla's own stacking rule
    /// (vanilla's own generic add-effect call → [`crate::mob_effects::EffectInstance::update`]
    /// — see that type's own doc for the "remembered, not ignored or replaced"
    /// table). Returns whether the active instance changed, matching
    /// [`crate::mob_effects::ActiveEffects::apply`]'s own return.
    pub fn apply_effect(&mut self, effect_id: &str, duration: i32, amplifier: u32) -> bool {
        self.effects.apply(effect_id, duration, amplifier)
    }

    /// Whether this mob is visibly on fire — vanilla's own "is on fire" check's
    /// remaining-fire-ticks-positive test.
    #[must_use]
    pub fn is_on_fire(&self) -> bool {
        self.burn.is_on_fire()
    }

    /// Vanilla's own "ignite for seconds" call — raises the burn counter, never lowers it
    /// (see [`crate::burning::BurnState::ignite_for_seconds`]). The fireball
    /// impact path (`MobSim::resolve_projectile_hit`) is the only production
    /// caller today.
    pub fn ignite_for_seconds(&mut self, seconds: f32) {
        self.burn.ignite_for_seconds(seconds);
    }

    /// The mob's current position.
    #[must_use]
    pub fn position(&self) -> Vec3 {
        self.mob.position()
    }

    /// The mob's collision body — the box [`MobSim::explode`] samples for
    /// blast exposure.
    #[must_use]
    pub fn shape(&self) -> &MobShape {
        self.mob.shape()
    }

    /// How many A\* searches this mob has run — the count that proves the
    /// pathfinder is actually being driven (a stubbed `move_to` never searches).
    #[must_use]
    pub fn path_searches(&self) -> u32 {
        self.mob.path_searches()
    }

    /// Whether the mob still has a path it is following.
    #[must_use]
    pub fn has_path(&self) -> bool {
        self.mob.has_path()
    }

    /// The mob's spawn category (drives its despawn distances).
    #[must_use]
    pub fn category(&self) -> MobCategory {
        self.category
    }

    /// Sets the mob's spawn category. Used by the spawn driver so a mob's
    /// despawn behaviour matches the category it was spawned as.
    pub fn set_category(&mut self, category: MobCategory) -> &mut Self {
        self.category = category;
        self
    }

    /// The mob's current `no_action_time` age timer (ticks since it last acted).
    #[must_use]
    pub fn no_action_time(&self) -> i32 {
        self.no_action_time
    }

    /// Whether the mob is exempt from natural despawn.
    #[must_use]
    pub fn is_persistent(&self) -> bool {
        self.persistent
    }

    /// Marks the mob persistent (named / persistence-required) so it never
    /// naturally despawns, mirroring vanilla `isPersistenceRequired`.
    pub fn set_persistent(&mut self, persistent: bool) -> &mut Self {
        self.persistent = persistent;
        self
    }

    /// The mob's stable UUID, encoded verbatim in the spawn packet.
    #[must_use]
    pub fn uuid(&self) -> Uuid {
        self.uuid
    }

    /// The mob's canonical entity-type key (e.g. `minecraft:zombie`). See the
    /// field docs for the placeholder caveat.
    #[must_use]
    pub fn entity_type(&self) -> &ResourceKey {
        &self.entity_type
    }

    /// Sets the mob's canonical entity-type key. Used by a species-aware spawn
    /// driver so the encoded spawn packet names the right entity.
    pub fn set_entity_type(&mut self, entity_type: ResourceKey) -> &mut Self {
        self.entity_type = entity_type;
        self
    }

    /// The mob's body rotation (degrees). Body yaw tracks the movement
    /// direction; ground mobs keep a level body, so pitch is 0.
    #[must_use]
    pub fn rotation(&self) -> Rotation {
        Rotation::new(self.mob.body_yaw(), 0.0)
    }

    /// The mob's head yaw in degrees — toward its look target if a goal set one,
    /// otherwise the body yaw. Matches `ClientEvent::EntityHeadRotation`.
    #[must_use]
    pub fn head_yaw(&self) -> f32 {
        self.mob.head_yaw()
    }

    /// The mob's velocity in **blocks per tick** (the unit vanilla's wire packing
    /// assumes), i.e. the position delta applied on the last tick.
    #[must_use]
    pub fn velocity(&self) -> Vec3 {
        self.mob.velocity()
    }

    /// This villager's profession — `villager::Profession::None` for every
    /// non-villager and every villager that has not claimed a workstation.
    #[must_use]
    pub fn profession(&self) -> villager::Profession {
        self.profession
    }

    /// The workstation this villager claimed, if any.
    #[must_use]
    pub fn workstation(&self) -> Option<BlockPos> {
        self.workstation
    }

    /// The bed this villager has claimed as its home point-of-interest, if any.
    #[must_use]
    pub fn bed(&self) -> Option<BlockPos> {
        self.bed
    }

    /// The bell this villager has claimed as its meeting point-of-interest, if any —
    /// [`bed`](Self::bed)'s sibling.
    #[must_use]
    pub fn meeting_point(&self) -> Option<BlockPos> {
        self.meeting_point
    }

    /// The nearest warden-listenable vibration this tick, if any — the
    /// vibration substrate. See the `nearest_vibration` field's own doc for
    /// what this drives ([`MobSim::resolve_warden_anger`]).
    #[must_use]
    pub fn nearest_vibration(&self) -> Option<PostedVibration> {
        self.nearest_vibration
    }

    /// This mob's own tracked anger level — see the `warden_anger` field's
    /// own doc for the single-suspect narrowing. `0` for a non-listener
    /// species.
    #[must_use]
    pub fn warden_anger(&self) -> i32 {
        self.warden_anger
    }

    /// The entity id [`warden_anger`](Self::warden_anger) is banked against,
    /// if any.
    #[must_use]
    pub fn warden_anger_target(&self) -> Option<i32> {
        self.warden_anger_target
    }

    /// Vanilla's own anger-level bucketing — this mob's own anger bucketed into vanilla's
    /// three named levels.
    #[must_use]
    pub fn warden_anger_level(&self) -> warden::AngerLevel {
        warden::AngerLevel::from_anger(self.warden_anger)
    }

    /// Vanilla's own "has left horn" check. Meaningless for a non-goat species.
    #[must_use]
    pub fn has_left_horn(&self) -> bool {
        self.has_left_horn
    }

    /// Vanilla's own "has right horn" check. Meaningless for a non-goat species.
    #[must_use]
    pub fn has_right_horn(&self) -> bool {
        self.has_right_horn
    }

    /// `minecraft:spawn_reinforcements`'s current base value. See
    /// [`reinforcement_chance`](Self::reinforcement_chance)'s own field doc.
    #[must_use]
    pub fn reinforcement_chance(&self) -> f64 {
        self.reinforcement_chance
    }

    /// `ZOMBIE_REINFORCEMENT_CALLEE_CHARGE` — the permanent `-0.05` a freshly
    /// placed reinforcement is charged against its own (independently
    /// randomized) `reinforcement_chance`, on top of whatever
    /// [`spawn_species`](Self::spawn_species)'s own
    /// vanilla-derived reinforcements-chance randomizer roll gave it, so a chain of
    /// reinforcements-calling-reinforcements tapers off rather than
    /// sustaining indefinitely. The driver (`crate::tick::run_tick_loop`)
    /// calls this on the mob [`spawn_species`](Self::spawn_species) just
    /// returned, since only it can tell "this spawn is a reinforcement" from
    /// "this spawn is anything else".
    pub fn apply_reinforcement_callee_charge(&mut self) -> &mut Self {
        self.reinforcement_chance -= ZOMBIE_REINFORCEMENT_CALLEE_CHARGE;
        self
    }

    /// Vanilla's own villager-data trade level, `1..=5`.
    #[must_use]
    pub fn villager_level(&self) -> i32 {
        self.villager_level
    }

    /// Accumulated trading xp toward the next level.
    #[must_use]
    pub fn villager_xp(&self) -> i32 {
        self.villager_xp
    }

    /// Assigns (or clears, with `villager::Profession::None`) this mob's
    /// profession and workstation together — the two always change in
    /// lockstep (see [`MobSim::tick_villager_professions`]'s doc for why a
    /// claim and a profession are never set independently).
    pub(crate) fn set_profession(
        &mut self,
        profession: villager::Profession,
        workstation: Option<BlockPos>,
    ) {
        self.profession = profession;
        self.workstation = workstation;
    }

    /// Ensures [`Self::trades`] matches the current `(profession,
    /// villager_level)`, rebuilding it from
    /// [`crate::villager_trade::VillagerTrades::for_profession`] when either
    /// has moved on since the last build — see that field's own doc for
    /// what a rebuild costs. `None` for a non-villager or an unemployed one
    /// (`Profession::None`), which get no economics at all, matching every
    /// other villager-only accessor in this file.
    fn ensure_trades(&mut self) -> Option<&mut crate::villager_trade::VillagerTrades> {
        if self.profession == villager::Profession::None {
            self.trades = None;
            return None;
        }
        let fresh = !matches!(
            &self.trades,
            Some((p, l, _)) if *p == self.profession && *l == self.villager_level
        );
        if fresh {
            self.trades = Some((
                self.profession,
                self.villager_level,
                crate::villager_trade::VillagerTrades::for_profession(self.profession, self.villager_level),
            ));
        }
        self.trades.as_mut().map(|(_, _, trades)| trades)
    }

    /// Applies a completed trade's xp reward (`TradeRecord::xp`) and
    /// advances this villager's level via [`villager::level_up`] — vanilla's
    /// own "reward trade xp" step feeding its own "set villager xp" setter. Previously
    /// nothing in this crate ever called this: `villager_level` was
    /// initialised to `1` and never mutated again, so no villager could ever
    /// reach a level-2..5 trade no matter how much it was traded with. A
    /// level change is picked up the next [`Self::ensure_trades`] call,
    /// which rebuilds the offer list to include the newly unlocked tier.
    fn give_villager_xp(&mut self, xp: i32) {
        self.villager_xp += xp;
        self.villager_level = villager::level_up(self.villager_level, self.villager_xp);
    }

    /// Lowers the mob into a version-free [`EntitySnapshot`] for the encode seam.
    /// This is the whole identity/motion surface a [`ServerProtocol`] needs to
    /// build spawn/move/remove packets; the server holds the previous snapshot
    /// per connection so the protocol can stay stateless.
    ///
    /// `metadata` is the per-species entity-metadata field list —
    /// general across mobs (see [`MetadataField`]'s own doc comment), not a
    /// creeper-only mechanism, even though a creeper was the only producer
    /// for a long time. [`crate::server::EntityStreamer`] diffs this exactly like
    /// every other field here, so a change reaches [`ServerProtocol::encode_set_entity_data`]
    /// through the same spawn/update path `position`/`rotation` already use —
    /// no second wiring for the next mob that needs a metadata field.
    ///
    /// `CreeperSwellDir` is always included for a creeper, even at its `-1`
    /// default: unlike `CreeperIgnited` (monotonic — set once, never
    /// cleared, so *absence* safely means "still false"), `swell_dir` can
    /// legitimately return to `-1` mid-episode during retreat,
    /// and that transition must reach the client exactly like the climb to
    /// `1` did — a client that keeps whatever `swell_dir` it was last sent
    /// would integrate the fuse in the wrong direction forever if a
    /// retreat-to-`-1` were ever skipped as "just the default".
    ///
    /// `MetadataField::Baby` is the same shape as `CreeperSwellDir`, not as
    /// `CreeperIgnited`: a mob **grows up**, so absence cannot safely mean
    /// "still a baby" the way it can mean "still not ignited". It is pushed
    /// unconditionally for every species eligible for it (see the species
    /// switch below), carrying the current `is_baby()` value whether that is
    /// `true` or `false`, so the adult transition reaches the client the same
    /// way the arrival as a baby did.
    #[must_use]
    pub fn snapshot(&self) -> EntitySnapshot {
        let mut metadata = Vec::new();
        if self.entity_type.path() == "creeper" {
            metadata.push(MetadataField::CreeperSwellDir(self.swell_dir()));
            if self.is_ignited() {
                metadata.push(MetadataField::CreeperIgnited(true));
            }
        }
        // Index 18's byte, **whose layout depends on the species** — see
        // `MetadataField::TamableFlags`. The species switch has to be here, in the
        // producer, because nothing downstream can recover it: four different `BYTE`
        // fields share index 18, one apiece on the tameable-animal, horse-family,
        // sheep and shulker metadata tables, and no `entity_census` column separates them, so
        // an encoder handed a single shared "tamed" variant would have to guess.
        //
        // Emitted only for a tame mob: a wild one's byte is all-zero, which is the
        // client's own default, and `EntityStreamer::sync` skips an empty metadata
        // list entirely — so a wild mob costs no extra packet.
        //
        // Species with no arm here stream nothing, which is the honest state rather
        // than a gap to fill speculatively: a tame llama or fox needs its own flag
        // layout read off the dump first.
        if self.tame {
            match self.entity_type.path() {
                "wolf" | "cat" | "parrot" | "ocelot" => {
                    metadata.push(MetadataField::TamableFlags {
                        tame: true,
                        sitting: self.is_in_sitting_pose(),
                    });
                }
                "horse" | "donkey" | "mule" | "skeleton_horse" | "zombie_horse" => {
                    metadata.push(MetadataField::HorseFlags { tame: true });
                }
                _ => {}
            }
        }
        // Index 16's boolean, shared by the ageable-mob, zombie and zoglin
        // "is baby" metadata fields
        // (`crates/protocol/v770/tests/support/entity_data_index_jvm.txt`)
        // — but also by the creeper's own swell-direction field, an `INT`, which is why the
        // species switch has to live here rather than in a shared "is baby"
        // encoder: a `MetadataField::Baby` emitted for a creeper would write
        // a boolean where the swell direction belongs. Scoped to exactly the
        // species this sim tracks age for — [`baby_dimensions`] and
        // [`baby_speed_multiplier`]'s own species lists — which are also the
        // only species mechanically confirmed (via `.cache/mc/26.2/src/`) to
        // descend from the ageable-mob or zombie base classes, the two
        // classes whose own "is baby" field resolves to this index.
        //
        // Pushed unconditionally rather than only while `is_baby()` is true:
        // see this method's own doc comment for why the grown-up transition
        // needs the same treatment as the arrival.
        match self.entity_type.path() {
            "cow" | "mooshroom" | "sheep" | "pig" | "chicken" | "rabbit" | "wolf" | "zombie"
            | "husk" | "zombie_villager" | "drowned" | "zombified_piglin" => {
                metadata.push(MetadataField::Baby(self.is_baby()));
            }
            _ => {}
        }
        // Villager metadata field, index 19 — the field a
        // client's own villager renderer/profession-layer actually reads
        // to pick a texture. Pushed unconditionally for every villager, at
        // whatever `profession`/`villager_level` currently are (including
        // `None`/`1`), for the same reason `Baby` above is pushed
        // unconditionally: a profession transition needs to reach the client
        // the same way the initial value did, not only while it is
        // "interesting". `kind` is always `minecraft:plains` — see
        // `crate::mobs::villager`'s module doc for why biome-derived type is
        // out of scope.
        if self.entity_type.path() == "villager" {
            metadata.push(MetadataField::VillagerData {
                kind: ResourceKey::from_str("minecraft:plains").expect("static key is valid"),
                profession: ResourceKey::from_str(&format!("minecraft:{}", self.profession.path()))
                    .expect("every Profession::path() is a valid identifier path"),
                level: self.villager_level,
            });
        }
        // Vanilla's own goat "has left/right horn" metadata fields, indices
        // 19/20 — see
        // `MetadataField::GoatHorns`'s own doc for the collision this species
        // switch resolves and why it is pushed unconditionally.
        if self.entity_type.path() == "goat" {
            metadata.push(MetadataField::GoatHorns {
                has_left: self.has_left_horn,
                has_right: self.has_right_horn,
            });
        }
        // Vanilla's own axolotl "playing dead" metadata field, index 19 — same "unconditional, so
        // the reset reaches the client too" shape as `GoatHorns` above.
        if self.entity_type.path() == "axolotl" {
            metadata.push(MetadataField::PlayingDead(self.axolotl_play_dead_ticks > 0));
        }
        // Vanilla's own shared pose metadata field, index 6 — pushed unconditionally for a warden,
        // not only while emerging, for the same "the reset must reach the
        // client too" reason `Baby`/`CreeperSwellDir` are: see
        // `MetadataField::Pose`'s own doc.
        if self.entity_type.path() == "warden" {
            metadata.push(MetadataField::Pose(if self.warden_emerge_ticks > 0 {
                warden::POSE_EMERGING
            } else if self.warden_digging_ticks > 0 {
                warden::POSE_DIGGING
            } else {
                warden::POSE_STANDING
            }));
        }
        // Same "unconditional, so the reset reaches the client too" shape as
        // the warden arm above — a camel that has just stood up must send
        // the standing pose (`0`), not merely stop sending the sitting pose.
        if self.entity_type.path() == "camel" {
            metadata.push(MetadataField::Pose(if self.camel_sitting {
                CAMEL_POSE_SITTING
            } else {
                CAMEL_POSE_STANDING
            }));
            // Vanilla's own camel dash metadata field — same "unconditional, so the reset reaches the
            // client too" shape as the pose push just above: a camel that
            // has finished dashing must send `false`, not merely stop
            // sending `true`.
            metadata.push(MetadataField::Dash(self.camel_is_dashing()));
        }
        // Vanilla's own sniffer state metadata field — same "unconditional, so the reset reaches
        // the client too" shape as the camel arm above. Pushed for a
        // sniffer only; see `sniffer::SnifferState::wire_ordinal` for the
        // real jar ordinal this carries.
        if self.entity_type.path() == "sniffer" {
            metadata.push(MetadataField::SnifferState(self.sniffer_state.wire_ordinal()));
        }
        EntitySnapshot {
            id: self.id,
            uuid: self.uuid,
            entity_type: self.entity_type.clone(),
            position: self.position(),
            rotation: self.rotation(),
            head_yaw: self.head_yaw(),
            velocity: self.velocity(),
            metadata,
            // No mob supplies additional spawn data here.
            object_data: 0,
            // Resolved by `MobSim::snapshots`, not here: `leash_holder` names a
            // player by uuid, and only `MobSim` (through `self.players`) can turn
            // that into the wire entity id `EntitySnapshot::leash_link` carries.
            // `SimMob` alone has no player list to resolve against.
            leash_link: None,
        }
    }
}

/// Wire identity for one tracked projectile.
///
/// [`ProjectileRegistry`]  deliberately stays version-free — its
/// own doc comment says a caller's `id`/ballistic state is all it tracks — so
/// the uuid and canonical entity-type key a spawn packet needs live here,
/// exactly the split [`SimMob`] already makes between `NavigatingMob`'s
/// version-free body and this crate's wire metadata.
#[derive(Debug, Clone)]
struct ProjectileMeta {
    uuid: Uuid,
    entity_type: ResourceKey,
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
    /// The thrown stack's raw `minecraft:potion` network id
    /// (`lodestone_model::item::ItemComponents::potion`), for a splash or
    /// lingering potion only — `None` for every other throwable, and `None` for
    /// a potion whose stack carried no resolved `minecraft:potion_contents`
    /// (a bare/uncomponented stack). [`MobSim::resolve_projectile_impacts`]
    /// reads this to decide what [`crate::mob_effects::potion_splash_effects`]
    /// applies on impact.
    potion: Option<i32>,
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
    /// Cells the last tick's item-settling pass asked [`ItemCollision`] for —
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
    pending_ambient_sounds: Vec<crate::effects::WorldEffect>,
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

impl<'w> MobSim<'w> {
    /// Creates an empty simulation over `world`.
    #[must_use]
    pub fn new(world: &'w ChunkWorld) -> Self {
        Self {
            world,
            mobs: Vec::new(),
            projectiles: ProjectileRegistry::new(),
            projectile_meta: HashMap::new(),
            items: ItemEntityRegistry::new(),
            item_state: HashMap::new(),
            orbs: HashMap::new(),
            orb_rng: SpawnRng::new(orbs::ORB_BEHAVIOR_SEED),
            equipment_rng: SpawnRng::new(EQUIPMENT_ROLL_SEED),
            goat_horn_rng: SpawnRng::new(GOAT_HORN_ROLL_SEED),
            door_rng: SpawnRng::new(DOOR_BREAK_ROLL_SEED),
            spawn_special_multiplier: 0.0,
            spawn_hard_difficulty: false,
            spawn_monsters_enabled: false,
            reinforcement_rng: SpawnRng::new(REINFORCEMENT_ROLL_SEED),
            pending_reinforcements: Vec::new(),
            falling_blocks: HashMap::new(),
            next_id: 1,
            tick_count: 0,
            item_probe_count: 0,
            pending_detonations: Vec::new(),
            pending_grazes: Vec::new(),
            pending_player_hits: Vec::new(),
            pending_mining_fatigue: Vec::new(),
            pending_vocalisations: Vec::new(),
            pending_ambient_sounds: Vec::new(),
            pending_animations: Vec::new(),
            players: Vec::new(),
            sleeping_players: Vec::new(),
            shoulder_riders: HashMap::new(),
            tame_rng: SpawnRng::new(TAME_ROLL_SEED),
            zombie_conversion_rng: SpawnRng::new(ZOMBIE_VILLAGER_CONVERSION_SEED),
            gossip_spread_rng: SpawnRng::new(GOSSIP_SPREAD_SEED),
            breed_rng: SpawnRng::new(BREED_XP_SEED),
            mob_drops: true,
            vehicles: HashMap::new(),
            tnt: HashMap::new(),
            minecarts: HashMap::new(),
            tnt_rng: SpawnRng::new(tnt::TNT_LAUNCH_SEED),
            // Vanilla's own field default (`private int nextTick;`, never
            // explicitly initialised, so Java's `0`) — the very first call
            // sees `nextTick <= 0` and may attempt a patrol on tick one,
            // subject to every other gate still applying.
            patrol_next_tick: 0,
            patrol_rng: SpawnRng::new(PATROL_SPAWN_SEED),
            // Vanilla's own field default (`private int tickDelay = 1200;`)
            // — the constructor sets it explicitly, unlike `nextTick`, so
            // the first call does not roll before tick 1200.
            trader_tick_delay: WANDERING_TRADER_TICK_DELAY,
            trader_spawn_delay: WANDERING_TRADER_SPAWN_DELAY,
            trader_spawn_chance: WANDERING_TRADER_MIN_SPAWN_CHANCE,
            trader_rng: SpawnRng::new(WANDERING_TRADER_SPAWN_SEED),
            lightning_bolts: HashMap::new(),
            pending_lightning_fires: Vec::new(),
            pending_projectile_block_hits: Vec::new(),
            #[cfg(not(target_arch = "wasm32"))]
            workstation_claims: villager::WorkstationClaims::new(),
            #[cfg(not(target_arch = "wasm32"))]
            bed_claims: villager::BedClaims::new(),
            #[cfg(not(target_arch = "wasm32"))]
            bell_claims: villager::BellClaims::new(),
            day_time: 0,
            posted_vibrations: Vec::new(),
            dragons: HashMap::new(),
            withers: HashMap::new(),
            wither_rng: SpawnRng::new(wither::WITHER_SKULL_SEED),
            crystals: HashMap::new(),
            dragon_rng: SpawnRng::new(dragon::DRAGON_PHASE_SEED),
            fishing_bobbers: HashMap::new(),
            fishing_rng: SpawnRng::new(fishing::FISHING_ROLL_SEED),
            raids: HashMap::new(),
            next_raid_id: 1,
            pending_hero_grants: Vec::new(),
            raid_rng: SpawnRng::new(raid::RAID_ROLL_SEED),
            dragon_fight: None,
            dragon_gateways: None,
            gateway_shuffle_rng: SpawnRng::new(GATEWAY_SHUFFLE_SEED),
            pending_dragon_deaths: Vec::new(),
        }
    }

    /// Replaces the RNG the taming mechanisms draw from — the injection point a
    /// tame-chance gate needs.
    ///
    /// A tame *chance* cannot be gated by observing that taming sometimes
    /// happens; that measures only that the code runs. Seed this with a stream
    /// whose first draw is known and the outcome becomes a prediction. The draw
    /// order and count are part of the specification, so a gate that reseeds
    /// between attempts is also asserting how many draws each mechanism makes.
    pub fn set_tame_rng(&mut self, rng: SpawnRng) -> &mut Self {
        self.tame_rng = rng;
        self
    }

    /// Sets the `DifficultyInstance` inputs every subsequent
    /// [`spawn_species`](Self::spawn_species) call feeds to
    /// [`lodestone_entity::spawn_equipment::populate_default_equipment_slots`]'s
    /// armour-upgrade roll: `special_multiplier` (`DifficultyInstance
    /// ::getSpecialMultiplier`, `0.0`..`1.0`) and whether the world's base
    /// difficulty is Hard.
    ///
    /// `crate::tick::run_tick_loop` is the real production caller — it
    /// resolves a `DifficultyInstance` (from world difficulty, game time and
    /// moon phase) once per tick and feeds
    /// [`DifficultyInstance::special_multiplier`](crate::regional_difficulty::DifficultyInstance::special_multiplier)
    /// and [`DifficultyInstance::is_hard`](crate::regional_difficulty::DifficultyInstance::is_hard)-shaped
    /// values here. Left at the `0.0`/`false` defaults, a spawn never rolls
    /// armour, which is vanilla's own behaviour for a fresh world's effective
    /// difficulty (below `2.0`).
    pub fn set_spawn_difficulty(&mut self, special_multiplier: f32, hard: bool) -> &mut Self {
        self.spawn_special_multiplier = special_multiplier;
        self.spawn_hard_difficulty = hard;
        self
    }

    /// Vanilla's own "is spawning monsters" check — the `spawn_mobs` game rule, gating
    /// [`attack`](Self::attack)'s zombie hurt-handler reinforcement roll
    /// alongside [`set_spawn_difficulty`](Self::set_spawn_difficulty)'s
    /// `hard` flag. `false` by default, matching every other spawn-difficulty
    /// input here: an unwired caller sees zero reinforcements rather than
    /// silently-always-on ones.
    pub fn set_spawn_monsters_enabled(&mut self, enabled: bool) -> &mut Self {
        self.spawn_monsters_enabled = enabled;
        self
    }

    /// Host injection point: the real world time-of-day, `0..24000` — see
    /// [`day_time`](Self::day_time)'s own field doc for what this feeds and
    /// why. `crate::tick::run_tick_loop` is the real production caller,
    /// reading `WorldState::time().day_time` (reduced mod 24000) once per
    /// tick, ahead of `tick_with_terrain`.
    pub fn set_day_time(&mut self, day_time: i32) -> &mut Self {
        self.day_time = day_time;
        self
    }

    /// Replaces the RNG [`run_patrol_spawn_cycle`](Self::run_patrol_spawn_cycle)
    /// draws from — the injection point a patrol-spawn gate needs, for the same
    /// reason [`set_tame_rng`](Self::set_tame_rng) exists.
    pub fn set_patrol_rng(&mut self, rng: SpawnRng) -> &mut Self {
        self.patrol_rng = rng;
        self
    }

    /// Replaces the RNG
    /// [`run_wandering_trader_spawn_cycle`](Self::run_wandering_trader_spawn_cycle)
    /// draws from — the injection point a trader-spawn gate needs, for the
    /// same reason [`set_patrol_rng`](Self::set_patrol_rng) exists.
    pub fn set_trader_rng(&mut self, rng: SpawnRng) -> &mut Self {
        self.trader_rng = rng;
        self
    }

    /// Overwrites [`run_wandering_trader_spawn_cycle`](Self::run_wandering_trader_spawn_cycle)'s
    /// two nested countdowns directly — the injection point a gate needs to
    /// stage a sim past the 1200-tick poll and the 24000-tick delay without
    /// calling the cycle that many times. `0` for either drives the *next*
    /// call straight to the roll, matching vanilla's own `<= 0` checks.
    pub fn set_trader_timers(&mut self, tick_delay: i32, spawn_delay: i32) -> &mut Self {
        self.trader_tick_delay = tick_delay;
        self.trader_spawn_delay = spawn_delay;
        self
    }

    /// Overwrites [`tick_count`](Self::tick_count) directly — the injection
    /// point a gate needs to stage the sim past
    /// [`run_patrol_spawn_cycle`](Self::run_patrol_spawn_cycle)'s timeline
    /// gate without actually ticking 120,000 times. Mirrors
    /// [`SimMob::set_temper`]'s reason: staging state a real playthrough
    /// would only reach by repetition.
    pub fn set_tick_count(&mut self, tick_count: u64) -> &mut Self {
        self.tick_count = tick_count;
        self
    }

    /// How many cells the **last** tick's item-settling pass asked the collision
    /// oracle about.
    ///
    /// This is the cost of routing items through swept collision, in the one unit
    /// that survives being read on a machine with four other builds running. It
    /// scales with item count and with how fast each item is moving (a faster item
    /// sweeps a longer box), so it is also the number that says whether a floor
    /// covered in drops can eat a tick — the question a per-item measurement
    /// structurally cannot answer.
    #[must_use]
    pub fn items_settled_probe_count(&self) -> u64 {
        self.item_probe_count
    }

    /// This villager's accumulated trading xp — `crate::server`'s own
    /// consumer for the `MERCHANT_OFFERS` packet's `villager_xp` field,
    /// alongside the `profession`/`level` an [`InteractOutcome::OpenTrade`]
    /// already carries. `0` for a non-villager or an unknown id — a
    /// harmless default rather than a panic, the same convention every
    /// other by-id accessor in this file uses.
    #[must_use]
    pub fn villager_xp(&self, mob_id: i32) -> i32 {
        self.mobs
            .iter()
            .find(|m| m.id == mob_id)
            .map_or(0, |m| m.villager_xp)
    }

    /// This villager's priced offer list for one moment in time, backed by
    /// its *persistent* [`crate::villager_trade::VillagerTrades`]. The
    /// persistent third trade-state field, demand, and uses persist between
    /// menu opens, while reputation and Hero of
    /// the Village are folded into a clone of each offer's price
    /// (`reset_special_price_diff` first, matching
    /// [`crate::mobs::villager::reputation::update_special_prices`]'s own
    /// contract): the *persisted* `special_price_diff` never accumulates
    /// across menu opens, only the persisted `uses`/`demand` do.
    ///
    /// Empty for a non-villager, an unknown id, or an unemployed villager —
    /// see [`SimMob::ensure_trades`].
    #[must_use]
    pub fn villager_offers(
        &mut self,
        mob_id: i32,
        reputation: i32,
        hero_of_the_village_amplifier: Option<u32>,
    ) -> Vec<crate::villager_trade::OfferState> {
        let Some(m) = self.get_mut(mob_id) else {
            return Vec::new();
        };
        let Some(trades) = m.ensure_trades() else {
            return Vec::new();
        };
        let mut offers = trades.offers.clone();
        for offer in &mut offers {
            offer.reset_special_price_diff();
        }
        villager::reputation::update_special_prices(&mut offers, reputation, hero_of_the_village_amplifier);
        offers
    }

    /// Executes a purchase against this villager's *persistent* offer at
    /// `index` — [`crate::villager_trade::VillagerTrades::try_trade`]'s
    /// first production caller. Pricing is computed the same way
    /// [`Self::villager_offers`] displays it (reset then
    /// re-discounted from `reputation`/`hero_of_the_village_amplifier`), so
    /// what a player sees is what they pay; [`OfferState::take`] then
    /// enforces the live, persisted cost *and* out-of-stock state for real,
    /// where every previous caller always saw a fresh `uses: 0` offer no
    /// matter how many times it had been bought.
    ///
    /// On success, feeds the trade's xp reward into
    /// [`SimMob::give_villager_xp`] — vanilla's own "notify trade" path,
    /// also previously unreached, which is why no villager could level up.
    /// Returns `None` — nothing mutated — for an unknown mob/villager, an
    /// out-of-range index, or a refused (out-of-stock/unsatisfied) offer.
    pub fn try_villager_trade(
        &mut self,
        mob_id: i32,
        index: usize,
        reputation: i32,
        hero_of_the_village_amplifier: Option<u32>,
    ) -> Option<crate::villager_trade::TradeTake> {
        let m = self.get_mut(mob_id)?;
        let trades = m.ensure_trades()?;
        let offer = trades.offers.get_mut(index)?;
        offer.reset_special_price_diff();
        let mut priced = [*offer];
        villager::reputation::update_special_prices(&mut priced, reputation, hero_of_the_village_amplifier);
        *offer = priced[0];
        let cost_a = offer.modified_cost_a_count();
        let cost_b = offer.record.wants_b.map_or(0, |(_, count)| count);
        let take = trades.try_trade(index, cost_a, cost_b)?;
        m.give_villager_xp(take.xp);
        Some(take)
    }

    /// Replaces the set of players mob perception can see, for
    /// [`tick`](Self::tick) to consume. The world tick calls this setter with
    /// player positions before goal evaluation, allowing
    /// [`MobController::nearest_player`] and [`MobController::temptation`] to
    /// read the shared perception input.
    ///
    /// # Why the parameter is generic
    ///
    /// It accepts anything that converts into a [`PerceivedPlayer`], which in
    /// practice means a `Vec<PerceivedPlayer>` **or** a bare
    /// `Vec<PlayerPerception>`. Both shapes are wanted at once and neither is
    /// transitional sugar for the other: taming needs the identity, so the real
    /// producer supplies views, while every gate that only cares where a mob
    /// looks is clearer without a uuid it does not use. A `PlayerPerception`
    /// converts to a view with **no identity**, which is the honest state for a
    /// producer that has none — see [`PerceivedPlayer`].
    pub fn set_players<I, P>(&mut self, players: I) -> &mut Self
    where
        I: IntoIterator<Item = P>,
        P: Into<PerceivedPlayer>,
    {
        self.players = players.into_iter().map(Into::into).collect();
        self
    }

    /// The players mob perception currently sees, with their identities.
    #[must_use]
    pub fn players(&self) -> &[PerceivedPlayer] {
        &self.players
    }

    /// Refreshes the sleeping-player roster — see
    /// [`sleeping_players`](Self::sleeping_players)'s own field doc. The
    /// world tick loop calls this once per tick with
    /// `crate::sleep::SleepState`'s own `(entity id, lay-down tick)` pairs,
    /// the same shared-state join [`set_players`](Self::set_players) already
    /// performs for position.
    pub fn set_sleeping_players(&mut self, sleepers: Vec<(i32, u64)>) -> &mut Self {
        self.sleeping_players = sleepers;
        self
    }

    /// The position of the player with this identity's uuid, if they are in the
    /// current player list — the resolution vanilla's
    /// own entity-reference resolver performs for
    /// its own tamed-animal owner getter.
    ///
    /// Keyed on the **uuid**, never on the entity id, for the reason
    /// [`PlayerIdentity`] gives: the entity id is reassigned per session, so a
    /// pet whose owner reconnects would resolve to whoever inherited that id.
    #[must_use]
    fn player_position(&self, uuid: Uuid) -> Option<Vec3> {
        self.players
            .iter()
            .find(|v| v.identity.is_some_and(|id| id.uuid == uuid))
            .map(|v| v.perception.position)
    }

    /// Resolves a [`LeashHolder`] to the wire entity id
    /// [`ServerProtocol::encode_set_entity_link`] needs as its target — the
    /// encoder takes a live entity id,
    /// and this is "which id" for each of the three holder shapes this sim
    /// tracks. Only `MobSim` can answer it — a bare `LeashHolder::Player` carries
    /// a uuid, not a session-scoped entity id, and resolving that needs
    /// `self.players` — which is why it is not a method on [`SimMob`] itself.
    ///
    /// [`LeashHolder::Player`] resolves through the same uuid-keyed lookup
    /// [`player_position`](Self::player_position) uses, for the identical reason
    /// given there: entity ids are reassigned per session, so keying on the uuid
    /// is what keeps a reconnecting owner's leash pointed at the right client.
    ///
    /// [`LeashHolder::Mob`] is already a wire id (`SimMob::id`), so this returns
    /// it verbatim — no lookup needed.
    ///
    /// [`LeashHolder::Fence`] returns `None`: this sim never spawns a
    /// `LeashFenceKnotEntity` (see that variant's own doc comment for why), so
    /// there is no entity id on the wire to link to yet. A mob leashed to a fence
    /// is tracked correctly server-side and draws no rope until a knot entity
    /// exists — a disclosed gap, not a silent one.
    #[must_use]
    fn resolve_leash_target(&self, holder: LeashHolder) -> Option<i32> {
        match holder {
            LeashHolder::Player(uuid) => self
                .players
                .iter()
                .find(|v| v.identity.is_some_and(|id| id.uuid == uuid))
                .map(|v| v.identity.expect("just matched").entity_id),
            LeashHolder::Mob(id) => Some(id),
            LeashHolder::Fence(_) => None,
        }
    }

    /// Overrides the id the next [`spawn`](Self::spawn) call assigns (and
    /// every one after it, still incrementing by one each time).
    ///
    /// Exists for a caller that shares its mob ids' wire namespace with a
    /// real protocol's own reserved ids. `MobSim::new`'s default start (`1`)
    /// collided, in production, with `V770ServerProtocol`'s
    /// `LOCAL_PLAYER_ENTITY_ID` (also `1`, `crates/protocol/v770/src/server_protocol.rs`):
    /// a real client never spawns "itself" as a separate `ADD_ENTITY`, so the
    /// very first mob a fresh [`MobSim`] ever spawns silently failed to
    /// appear — found live by `crates/protocol/v770/tests/live_mob_sim.rs`,
    /// which consistently observed 2 of 3 seeded mobs, never 3, until
    /// `run_mob_tick_loop` started calling this. `MobSim::new`'s default is
    /// left unchanged (`1`) so every existing hermetic test keeps its
    /// already-asserted ids stable; only a caller wired to a real wire
    /// protocol needs to call this.
    pub fn set_next_id(&mut self, next_id: i32) -> &mut Self {
        self.next_id = next_id;
        self
    }

    /// The id the next [`spawn`](Self::spawn) call will assign.
    ///
    /// The read side of [`set_next_id`](Self::set_next_id), and it answers one
    /// question nothing else can: **has this sim been reseeded yet?**
    /// [`MobHandle::reseed`] replaces the whole sim and then calls
    /// `set_next_id(1000)`, while [`MobSim::new`] starts at `1` — so a caller that
    /// must not touch a sim about to be thrown away (a saved-entity restore, a
    /// `/summon` racing world open) can tell the difference. Without it, that
    /// caller has to guess, and guessing wrong is silent: the work lands in the
    /// sim that is discarded a moment later.
    #[must_use]
    pub fn next_id(&self) -> i32 {
        self.next_id
    }

    /// Spawns a mob at `pos` with body `shape`, moving `step_per_tick` blocks per
    /// tick (derived from its movement-speed attribute) and an A\* open-set
    /// budget of `visited_budget` (vanilla `floor(followRange * 16)`).
    ///
    /// Returns a mutable handle so the caller can attach goals and a target
    /// before the first tick.
    pub fn spawn(
        &mut self,
        pos: Vec3,
        shape: MobShape,
        step_per_tick: f64,
        visited_budget: i32,
    ) -> &mut SimMob<'w> {
        let entity_type = ResourceKey::from_str("minecraft:zombie").expect("static key is valid");
        self.spawn_with_type(pos, shape, step_per_tick, visited_budget, entity_type)
    }

    /// The shared body of [`spawn`](Self::spawn) and
    /// [`spawn_species`](Self::spawn_species): everything except *which*
    /// `entity_type` (and therefore which [`combat_defaults`]) the new mob
    /// gets.
    fn spawn_with_type(
        &mut self,
        pos: Vec3,
        shape: MobShape,
        step_per_tick: f64,
        visited_budget: i32,
        entity_type: ResourceKey,
    ) -> &mut SimMob<'w> {
        let id = self.next_id;
        self.next_id += 1;
        let (max_health, attack_damage, defenses, knockback_resistance) =
            combat_defaults(&entity_type);
        let is_warden = entity_type.path() == "warden";
        self.mobs.push(SimMob {
            id,
            mob: NavigatingMob::new(self.world, shape, pos, step_per_tick, visited_budget, id as u64),
            goals: GoalSelector::new(),
            category: MobCategory::Monster,
            no_action_time: 0,
            persistent: false,
            uuid: Uuid::new_v4(),
            entity_type,
            health: max_health,
            max_health,
            defenses,
            burn: crate::burning::BurnState::new(),
            anger: None,
            stung_at: None,
            piglin_alert_ticks: -1,
            armadillo_danger_ticks: 0,
            axolotl_play_dead_ticks: 0,
            camel_sitting: false,
            camel_pose_tick: 0,
            camel_dash_cooldown: 0,
            sniffer_state: sniffer::SnifferState::Idling,
            sniffer_state_ticks: 0,
            sniffer_sniff_cooldown: 0,
            sniffer_dig_target: None,
            sniffer_explored: Vec::new(),
            allay_liked_noteblock: None,
            allay_inventory_count: 0,
            allay_duplication_cooldown: 0,
            hurt_by_player_until: None,
            attack_damage,
            hurt_cooldown: HurtCooldown::default(),
            ambient_sound_time: 0,
            attack_target_id: None,
            owner: None,
            tame: false,
            ordered_to_sit: false,
            temper: 0,
            knockback_resistance,
            leash_holder: None,
            last_lightning_bolt: None,
            profession: villager::Profession::None,
            workstation: None,
            villager_level: 1,
            villager_xp: 0,
            trades: None,
            job_search_cooldown: 0,
            cat_search_cooldown: 0,
            shoulder_dismount_ticks: 0,
            bed: None,
            bed_search_cooldown: 0,
            meeting_point: None,
            bell_search_cooldown: 0,
            nearest_vibration: None,
            warden_anger: 0,
            warden_anger_target: None,
            warden_emerge_ticks: if is_warden { warden::EMERGE_DURATION_TICKS } else { 0 },
            warden_sonic_boom_cooldown: 0,
            warden_dig_cooldown: if is_warden { warden::DIGGING_COOLDOWN_TICKS } else { 0 },
            warden_digging_ticks: 0,
            has_left_horn: true,
            has_right_horn: true,
            reinforcement_chance: 0.0,
            gossip: villager::gossip::GossipContainer::new(),
            last_gossip_decay_tick: None,
            golem_detected_until: None,
            conversion: None,
            effects: crate::mob_effects::ActiveEffects::new(),
            rider: None,
        });
        self.mobs.last_mut().expect("just pushed")
    }

    /// Spawns a mob of a specific species at `pos`, resolving its body and
    /// behavior from the per-species data tables.
    ///
    /// * **Shape** comes from the 26.2 dimension census
    ///   ([`lodestone_data::entity_dimensions`], keyed by
    ///   [`lodestone_data::entity_types::entity_type_id_parts`]) folded with the
    ///   type's `SCALE`/`STEP_HEIGHT` attributes — the same math
    ///   [`crate::resolve_mob_shape`] uses for a version-aware caller, read
    ///   directly here since `MobSim` already depends on `lodestone_data` for
    ///   its path/collision census above. Falls back to `MobShape::land(0.6,
    ///   1.95)` for a species the census does not know by name, matching that
    ///   function's own "explicit fallback, never a silent guess" contract.
    /// * **Combat stats** come from [`combat_defaults`], already species-aware.
    /// * **Speed**: the type's `movement_speed` attribute value feeds
    ///   [`SpeciesContext`](lodestone_entity::ai::roster::SpeciesContext) as-is
    ///   (every roster goal multiplies it by its own speed constants before it
    ///   reaches motion), but the actual kinematic-follower rate handed to
    ///   [`spawn_with_type`] is [`ai_ground_speed`] of that attribute. A bare
    ///   attribute value is not the mob's real blocks/tick rate; see
    ///   `docs/mob-species-spawning.md` for the conversion measurement.
    /// * **Goals** come from [`lodestone_entity::ai::roster`], which resolves the
    ///   species path to a prioritized set. This function does not know
    ///   individual species: a species with no roster entry gets `roster::FALLBACK`
    ///   (wander and look around).
    ///
    ///   The roster connects these behavior goals to production spawning, and
    ///   perception supplied by [`tick`](Self::tick) drives them during a tick.
    ///
    ///   Two consequences worth knowing when reading a mob's behaviour:
    ///   priorities use the roster's absolute values. For example, a creeper's
    ///   swell goal is at priority 2 and its melee goal at priority 4. Melee
    ///   speed is a multiplier on the mob's `movement_speed`; hostile roster
    ///   entries are above the `0.2` lower bound (the slowest entry is a zombie
    ///   at `0.23`).
    pub fn spawn_species(&mut self, entity_type: ResourceKey, pos: Vec3) -> &mut SimMob<'w> {
        let mut attrs = default_attributes(&entity_type).unwrap_or_else(AttributeMap::new);
        // Always spawns adult-shaped; a caller wanting a baby applies
        // `set_age(BABY_START_AGE)` afterward, which re-derives the shape
        // through the same function (see `SimMob::set_age`'s own doc).
        let mut shape = species_shape(&entity_type, &attrs, false);
        // The zombie family's door-breaking is a spawn-time coin flip scaled
        // by regional difficulty, not a species constant, so `species_shape`
        // cannot set it — rolled here, once, on its own RNG stream for the
        // same reason every other spawn-time roll on this sim gets one (see
        // `door_rng`'s own doc). See `docs/mob-species-spawning.md` for the
        // vanilla formula and the "leader zombie" bonus this does not model.
        if matches!(
            entity_type.path(),
            "zombie" | "husk" | "zombie_villager" | "drowned" | "zombified_piglin"
        ) {
            shape.can_open_doors = self.door_rng.next_f32() < self.spawn_special_multiplier * 0.1;
        }
        let base_speed = attr(&attrs, "movement_speed");
        // `minecraft:follow_range`, read **once** and fed to both consumers, so
        // target acquisition and the A* budget cannot drift apart.
        //
        // `attr_present` rather than `attr`: for a species `default_attributes`
        // has no template for, `attrs` is empty and `attr` returns the *registry*
        // default of **32.0** — not 0.0, and not a harmless approximation. 32.0
        // is the single value this attribute never legitimately holds, because
        // The generic attribute fallback is 16.0 for every mob, so the registry
        // default is not the effective range. Falling back explicitly to
        // `DEFAULT_FOLLOW_RANGE` keeps an unlisted species usable.
        //
        // Species that raise it do so in their own attribute builder — the
        // zombie family 35.0, blaze 48.0,
        // enderman 64.0 — and `attribute.rs::type_spec` has arms for only
        // thirteen species. So `zombie` gets its real 35.0 here
        // while `zombie_villager` uses the generic 16.0 fallback here.
        // The explicit fallback keeps this behavior visible rather than assumed; the required
        // `type_spec` arms, not a fallback tuned to flatter the zombie family.
        let follow_range = attr_present(&attrs, "follow_range").unwrap_or(DEFAULT_FOLLOW_RANGE);
        let visited_budget = (follow_range * 16.0).floor() as i32;
        let hostile = species::is_hostile_species(&entity_type);

        // Captured before `entity_type` moves into `spawn_with_type` below, so
        // the equipment roll (which also needs the species path, after the
        // move) has its own owned copy rather than fighting the borrow.
        let species_path = entity_type.path().to_owned();

        // Built *before* `entity_type` is moved into the spawn. `SpeciesContext`
        // wants the raw attribute — every roster goal supplies its own
        // speed multiplier on top — so it is *not* `ai_ground_speed`-converted
        // here; the conversion happens once, below, for the kinematic
        // follower's own rate.
        let goals = roster::goals_for(&species_path, &SpeciesContext::new(base_speed));

        // Vanilla's own default-equipment-population step — what this mob spawns holding
        // and wearing (`lodestone_entity::spawn_equipment`'s module doc has
        // the full per-species table). Folded into `attrs` *before*
        // `spawn_with_type` reads combat stats from a fresh
        // `default_attributes` call of its own, so the two cannot disagree on
        // the base and only equipment is layered on top here.
        let equipped = spawn_equipment::populate_default_equipment_slots(
            &species_path,
            &mut self.equipment_rng,
            self.spawn_special_multiplier,
            self.spawn_hard_difficulty,
        );
        equipment::apply_equipment(&mut attrs, equipped.iter());

        // Vanilla's own goat spawn-finalization's own pre-broken-horn roll — see
        // `goat_horn_spawn_roll`'s own doc. Rolled here, before `entity_type`
        // moves into `spawn_with_type` below, for the identical reason
        // `species_path` was captured above.
        let (has_left_horn, has_right_horn) = goat_horn_spawn_roll(&species_path, &mut self.goat_horn_rng);

        // Vanilla's own reinforcements-chance randomizer — its attribute-handling
        // step calls
        // it for the whole zombie family (husk/drowned/zombie-villager/
        // zombified-piglin all extend the base zombie class and override neither method —
        // the same species list `can_open_doors` above already establishes).
        // Rolled here for the identical reason `has_left_horn`/
        // `has_right_horn` are: before `entity_type` moves into
        // `spawn_with_type` below.
        let reinforcement_chance = if matches!(
            species_path.as_str(),
            "zombie" | "husk" | "zombie_villager" | "drowned" | "zombified_piglin"
        ) {
            self.reinforcement_rng.next_f64() * 0.1
        } else {
            0.0
        };

        let mob = self.spawn_with_type(
            pos,
            shape,
            ai_ground_speed(base_speed),
            visited_budget,
            entity_type,
        );
        mob.has_left_horn = has_left_horn;
        mob.has_right_horn = has_right_horn;
        mob.reinforcement_chance = reinforcement_chance;
        mob.set_category(if hostile {
            MobCategory::Monster
        } else {
            MobCategory::Creature
        })
        .set_persistent(!hostile);
        for (priority, goal) in goals {
            mob.add_goal(priority, goal);
        }
        // The `FOLLOW_RANGE` attribute reaches the controller, which is what
        // bounds target acquisition. Without this every hosted mob used
        // the seam's `DEFAULT_FOLLOW_RANGE`, so the zombie family — the only
        // family `seed_demo_mobs` spawns — targeted at 16 blocks instead of its
        // real 35.0. A wrong *value* on a fully connected wire, which is the
        // failure mode `cargo xtask connectedness` structurally cannot see.
        //
        // Set here rather than in `feed_perception` on purpose: this is a species
        // attribute resolved once at spawn, not per-tick perception. Putting it in
        // the feed would mean re-reading `default_attributes` for every mob every
        // tick, and would invite a second source of truth for a number
        // `visited_budget` above already derives from this exact read.
        mob.mob.set_follow_range(follow_range);
        // What the mob's main hand holds (a drowned's trident roll is the one
        // production reader today, through `MobController::main_hand_item` and
        // `RangedAttackGoal`'s `requires_main_hand` gate), and `equip_attrs`
        // folded above overriding `spawn_with_type`'s bare-species combat
        // numbers with the equipped versions — armour, weapon damage,
        // netherite's knockback resistance.
        mob.mob.set_main_hand_item(equipped.main_hand.clone());
        mob.defenses = defenses_from_attributes(&attrs);
        mob.attack_damage = attack_damage_from_attributes(&attrs);
        mob.knockback_resistance = knockback_resistance_from_attributes(&attrs);
        mob
    }

    /// Removes a mob by entity id, returning whether one was actually removed.
    ///
    /// The missing despawn half of a native plugin's spawn/despawn/modify
    /// surface: [`spawn_species`](Self::spawn_species) plus
    /// [`SimMob::id`] already give a caller "spawn and get an id back", and this
    /// is the same [`self.mobs`](MobSim) retain shape already used inline at
    /// the creeper self-detonation and [`reap_dead`](Self::reap_dead) call
    /// sites, named and made public rather than duplicated a third time.
    ///
    /// **Cannot remove a player.** Player entity ids are allocated from
    /// `PLAYER_ENTITY_ID_BASE` and live in `PlayerRegistry`, never in
    /// `self.mobs` — so a plugin calling this with a connected player's id is a
    /// harmless no-op, never an accidental disconnect. This is the server-side
    /// analogue of the client's `apply_entity_removal` skipping an id held by
    /// `LocalPlayer`.
    ///
    /// **Drops no loot and grants no experience** — unlike
    /// [`reap_dead`](Self::reap_dead)'s death sweep, this is vanilla's plain
    /// generic entity-remove call, not a kill. A plugin that wants a despawned mob to
    /// drop loot calls whatever already grants that on a real death, not this.
    pub fn remove_mob(&mut self, id: i32) -> bool {
        let before = self.mobs.len();
        self.mobs.retain(|m| m.id != id);
        self.mobs.len() != before
    }

    /// Given a just-placed carved pumpkin or jack o'lantern at `pumpkin_pos`,
    /// checks whether it completes a valid snow- or iron-golem block pattern
    /// and, if so, spawns the golem — vanilla's
    /// own carved-pumpkin "try spawn golem" step.
    ///
    /// Tries the snow golem pattern first and returns on a match, exactly as
    /// vanilla's early `return` does — a pumpkin that happens to complete
    /// both (impossible for these two shapes, but the order is part of the
    /// port) only ever produces the snow golem.
    ///
    /// **A pure detection query, not a world mutation.** `MobSim` holds only
    /// a read-only [`PathWorld`] and has no block-*write* authority, so
    /// `block_at` is the caller's own world oracle (the
    /// [`tick_with_terrain`](Self::tick_with_terrain) idiom) and
    /// [`GolemConstruction::consumed`] is a report, not an action — the
    /// caller (the block-placement owner) is the one that actually clears
    /// those cells, exactly as the documented scope says: "given this
    /// placement, does a valid pattern exist, and if so spawn the golem".
    pub fn try_construct_golem(
        &mut self,
        block_at: &dyn Fn(i32, i32, i32) -> String,
        pumpkin_pos: (i32, i32, i32),
    ) -> Option<GolemConstruction> {
        if let Some(found) = golem::find_golem_pattern(block_at, golem::SNOW_GOLEM_PATTERN, pumpkin_pos) {
            // `getBlock(0, 2, 0)` — the bottom snow block's cell.
            let feet = found.translate(0, 2, 0);
            let consumed = found.consumed(golem::SNOW_GOLEM_PATTERN);
            let id = self
                .spawn_species(
                    "minecraft:snow_golem".parse().expect("valid key"),
                    golem::golem_feet_to_spawn_pos(feet),
                )
                .id();
            return Some(GolemConstruction {
                species: GolemSpecies::Snow,
                id,
                consumed,
            });
        }
        if let Some(found) = golem::find_golem_pattern(block_at, golem::IRON_GOLEM_PATTERN, pumpkin_pos) {
            // `getBlock(1, 2, 0)` — the bottom-centre iron block's cell.
            let feet = found.translate(1, 2, 0);
            let consumed = found.consumed(golem::IRON_GOLEM_PATTERN);
            let id = self
                .spawn_species(
                    "minecraft:iron_golem".parse().expect("valid key"),
                    golem::golem_feet_to_spawn_pos(feet),
                )
                .id();
            // vanilla additionally calls its own "set player created" setter,
            // which suppresses this golem attacking
            // the player who angered it and is checked on NBT save/load.
            // This sim has no such per-golem flag and no player-directed
            // hostility model for a neutral mob to suppress — a disclosed
            // gap, not a silent omission: a player-built iron golem here
            // behaves identically to a village-spawned one.
            return Some(GolemConstruction {
                species: GolemSpecies::Iron,
                id,
                consumed,
            });
        }
        None
    }

    /// Advances every mob one tick: run its goals (which drive A\* and path
    /// following through the [`MobController`] seam), then step the follower.
    /// Each mob's `no_action_time` ages by one tick and is first cleared for any
    /// persistent mob, or
    /// one within its category's immune radius of a player from
    /// [`set_players`](Self::set_players). See the body for why that reset lives
    /// here rather than only in [`despawn_pass`](MobSim::despawn_pass), which
    /// has no production caller and left the counter monotonic — permanently
    /// disabling every idle-throttled goal five seconds into a world.
    ///
    /// A melee attack that connected this tick is resolved into a real
    /// [`SimMob::apply_damage`] call against whichever mob its
    /// [`attack_target_id`](SimMob::attack_target_id) names — the goal
    /// scheduler only ever produces the *intent* to strike (a position, via
    /// [`NavigatingMob::take_new_attacks`]); this is where that intent becomes
    /// a real health change. Resolution runs in a second pass over collected
    /// events, after every mob's own AI has ticked, so an attacker damaging
    /// another mob never needs two simultaneous mutable borrows into the same
    /// `Vec`. A mob whose health reaches `0.0` is removed at the end of the
    /// tick that killed it.
    /// One tick, settling dropped items against this sim's own terrain snapshot.
    ///
    /// **Production should call [`tick_with_terrain`](Self::tick_with_terrain)
    /// instead**, and the difference is a real gameplay bug rather than a
    /// preference: the snapshot only covers the 7×7 `mob_area` columns taken when
    /// the world opened, so items dropped anywhere else fall straight through the
    /// ground. See [`settle_item`]. This entry point stays for hermetic callers,
    /// whose fixture world *is* the whole world they care about.
    pub fn tick(&mut self) {
        let world = self.world;
        self.tick_with_terrain(&|x, y, z| world.block_state(x, y, z).to_owned());
    }

    /// Ticks between one unemployed villager's job searches — throttles
    /// [`villager::find_and_claim_workstation`]'s bounded terrain scan (see
    /// that function's own doc for the cost it is bounding). 100 ticks is a
    /// scope choice, not a transcribed vanilla constant: nothing in this
    /// codebase ports `AssignProfessionFromJobSite`'s own interval.
    #[cfg(not(target_arch = "wasm32"))]
    const JOB_SEARCH_INTERVAL_TICKS: i32 = 100;

    /// One villager-profession pass: throttled job search for
    /// unemployed villagers, and re-verification for employed ones.
    ///
    /// Re-verification, not an event hook, is how "losing the block loses
    /// the job" is implemented — see [`villager`]'s own module doc for why,
    /// and for the one-tick lag that trade-off buys. A villager whose
    /// workstation position no longer resolves to the profession it was
    /// claimed under (destroyed, or replaced with a different workstation
    /// type) releases its ticket and goes back to unemployed on the very
    /// next call.
    ///
    /// Native-only (the wasm32 scope note) — see
    /// [`villager::WorkstationClaims`]'s own doc. A villager spawned in a
    /// `wasm32` (browser singleplayer) world keeps whatever profession it
    /// already had and simply never claims a new one.
    #[cfg(not(target_arch = "wasm32"))]
    fn tick_villager_professions(&mut self) {
        let world = self.world;
        let claims = &mut self.workstation_claims;
        // Vanilla's own villager restock step's own cadence check (its own
        // per-AI-tick brain activity
        // call, not built here — see `villager_trade`'s module doc), run
        // once per profession pass for every employed villager instead.
        // `tick_count` is this sim's only clock (see its own field doc);
        // `restock_day` divides it into vanilla's 24000-tick day the same
        // way `day_time` is derived elsewhere in this file.
        let restock_time = self.tick_count as i64;
        let restock_day = restock_time / 24_000;
        for mob in &mut self.mobs {
            if mob.entity_type.path() != "villager" {
                continue;
            }
            if let Some(pos) = mob.workstation {
                let state = world.block_state(pos.x, pos.y, pos.z);
                let still_valid = villager::poi_type_for_block(villager::bare_block_id(state))
                    .and_then(villager::profession_for_poi_type)
                    == Some(mob.profession);
                if !still_valid {
                    claims.remove(pos);
                    mob.set_profession(villager::Profession::None, None);
                } else if let Some(trades) = mob.ensure_trades() {
                    trades.maybe_restock(restock_time, restock_day);
                }
                continue;
            }
            // A profession with no job site (`Nitwit`) has nothing to search
            // for; only `None` (truly unemployed) runs the search below.
            if mob.profession != villager::Profession::None {
                continue;
            }
            if mob.job_search_cooldown > 0 {
                mob.job_search_cooldown -= 1;
                continue;
            }
            mob.job_search_cooldown = Self::JOB_SEARCH_INTERVAL_TICKS;
            let feet = mob.position();
            let origin = BlockPos::new(
                feet.x.floor() as i32,
                feet.y.floor() as i32,
                feet.z.floor() as i32,
            );
            if let Some((pos, profession)) =
                villager::find_and_claim_workstation(origin, world, claims)
            {
                mob.set_profession(profession, Some(pos));
            }
        }
    }

    /// Bed search interval — [`JOB_SEARCH_INTERVAL_TICKS`](Self::JOB_SEARCH_INTERVAL_TICKS)'s
    /// own scope choice, reused for the identical reason: nothing in this
    /// codebase ports `AcquirePoi`'s own per-behavior scheduling.
    #[cfg(not(target_arch = "wasm32"))]
    const BED_SEARCH_INTERVAL_TICKS: i32 = 100;

    /// One villager-bed pass (the raid trigger): throttled bed
    /// search for an unclaimed villager, re-verification for a claimed one.
    ///
    /// Independent of [`tick_villager_professions`](Self::tick_villager_professions):
    /// a bed (vanilla's own "home" memory) and a job site
    /// (vanilla's own "job site" memory) are two separate memories in vanilla,
    /// and a villager can hold either, both, or neither at once. Same
    /// re-verification shape as professions — see that method's own doc for
    /// why a poll, not an event hook, is how "losing the bed loses the
    /// claim" is implemented, and the one-tick lag that trade-off buys.
    ///
    /// Native-only, for [`tick_villager_professions`](Self::tick_villager_professions)'s
    /// own reason.
    #[cfg(not(target_arch = "wasm32"))]
    fn tick_villager_beds(&mut self) {
        let world = self.world;
        let claims = &mut self.bed_claims;
        for mob in &mut self.mobs {
            if mob.entity_type.path() != "villager" {
                continue;
            }
            if let Some(pos) = mob.bed {
                let state = world.block_state(pos.x, pos.y, pos.z);
                let still_valid = villager::is_bed_block(villager::bare_block_id(state));
                if !still_valid {
                    claims.remove(pos);
                    mob.bed = None;
                }
                continue;
            }
            if mob.bed_search_cooldown > 0 {
                mob.bed_search_cooldown -= 1;
                continue;
            }
            mob.bed_search_cooldown = Self::BED_SEARCH_INTERVAL_TICKS;
            let feet = mob.position();
            let origin = BlockPos::new(
                feet.x.floor() as i32,
                feet.y.floor() as i32,
                feet.z.floor() as i32,
            );
            if let Some(pos) = villager::find_and_claim_bed(origin, world, claims) {
                mob.bed = Some(pos);
            }
        }
    }

    /// The live equivalent of
    /// [`crate::poi_storage::PoiStorage::occupied_in_range`] restricted to
    /// `home` POIs: every bed claimed through [`tick_villager_beds`](Self::tick_villager_beds)
    /// within `radius` real blocks of `center`. The raid trigger
    /// (vanilla's own raid-creation-or-extension step's own point-of-interest
    /// range query over the `#village` tag, occupied only) is this method's reason to exist: a bed
    /// claimed through [`villager::BedClaims`] is never written to the
    /// on-disk `poi/` region set (see that type's own doc), so a caller
    /// wiring the real trigger against *live* villagers reads this rather
    /// than (or in addition to) [`crate::poi_storage::PoiStorage::occupied_in_range`],
    /// which can only ever see a bed claim that has been persisted to disk.
    ///
    /// Native-only, for [`tick_villager_beds`](Self::tick_villager_beds)'s
    /// own reason.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn occupied_homes_in_range(&self, center: BlockPos, radius: i32) -> Vec<BlockPos> {
        self.bed_claims.occupied_in_range(center, radius)
    }

    /// The full point-of-interest range query, filtered to the `#village`
    /// point-of-interest tag and occupied only, that the raid trigger actually
    /// needs — every claimed bed, workstation *or* bell within `radius` real
    /// blocks of `center`, unioning [`occupied_homes_in_range`](Self::occupied_homes_in_range)
    /// with [`villager::WorkstationClaims::occupied_in_range`] and
    /// [`villager::BellClaims::occupied_in_range`].
    ///
    /// [`occupied_homes_in_range`](Self::occupied_homes_in_range) alone is
    /// narrower than vanilla's `#village` tag (`home` + `meeting` +
    /// `#acquirable_job_site`, per `point_of_interest_type/village.json`) —
    /// a village whose villagers have claimed jobs and a bell but no bed yet
    /// would never trigger a raid through the beds-only query. This is the
    /// one [`super::raid`]'s `create_or_extend_raid` and `crate::server`'s
    /// Bad-Omen-to-Raid-Omen conversion check both use instead.
    ///
    /// Native-only, for [`occupied_homes_in_range`](Self::occupied_homes_in_range)'s
    /// own reason.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn occupied_village_pois_in_range(&self, center: BlockPos, radius: i32) -> Vec<BlockPos> {
        let mut found = self.bed_claims.occupied_in_range(center, radius);
        found.extend(self.workstation_claims.occupied_in_range(center, radius));
        found.extend(self.bell_claims.occupied_in_range(center, radius));
        found
    }

    /// Bell search interval — [`JOB_SEARCH_INTERVAL_TICKS`](Self::JOB_SEARCH_INTERVAL_TICKS)'s
    /// own scope choice, reused for the identical reason.
    #[cfg(not(target_arch = "wasm32"))]
    const BELL_SEARCH_INTERVAL_TICKS: i32 = 100;

    /// One villager-bell pass (the `MEET` schedule activity):
    /// throttled bell search for an unclaimed villager, re-verification for
    /// a claimed one — [`tick_villager_beds`](Self::tick_villager_beds)'s own
    /// shape, restricted to [`villager::BellClaims`]/[`villager::find_and_claim_bell`]
    /// and with **no occupancy exclusion** (a bell hands out 32 tickets, so
    /// nothing here needs to check whether another villager already claimed
    /// this exact bell — [`villager::find_and_claim_bell`]'s own search
    /// already tries the next ticket via `try_claim` regardless).
    ///
    /// Independent of [`tick_villager_beds`](Self::tick_villager_beds)/
    /// [`tick_villager_professions`](Self::tick_villager_professions): a
    /// bell (vanilla's own "meeting point" memory), a bed
    /// (vanilla's own "home" memory) and a job site
    /// (vanilla's own "job site" memory) are three separate memories in vanilla,
    /// and a villager can hold any combination of the three at once.
    ///
    /// Native-only, for [`tick_villager_professions`](Self::tick_villager_professions)'s
    /// own reason.
    #[cfg(not(target_arch = "wasm32"))]
    fn tick_villager_bells(&mut self) {
        let world = self.world;
        let claims = &mut self.bell_claims;
        for mob in &mut self.mobs {
            if mob.entity_type.path() != "villager" {
                continue;
            }
            if let Some(pos) = mob.meeting_point {
                let state = world.block_state(pos.x, pos.y, pos.z);
                let still_valid = villager::is_bell_block(villager::bare_block_id(state));
                if !still_valid {
                    claims.remove(pos);
                    mob.meeting_point = None;
                }
                continue;
            }
            if mob.bell_search_cooldown > 0 {
                mob.bell_search_cooldown -= 1;
                continue;
            }
            mob.bell_search_cooldown = Self::BELL_SEARCH_INTERVAL_TICKS;
            let feet = mob.position();
            let origin = BlockPos::new(
                feet.x.floor() as i32,
                feet.y.floor() as i32,
                feet.z.floor() as i32,
            );
            if let Some(pos) = villager::find_and_claim_bell(origin, world, claims) {
                mob.meeting_point = Some(pos);
            }
        }
    }

    /// Throttles [`tick_cat_block_search`](Self::tick_cat_block_search)'s
    /// bounded terrain scan — the same shape
    /// [`JOB_SEARCH_INTERVAL_TICKS`](Self::JOB_SEARCH_INTERVAL_TICKS) is, and
    /// for the identical reason: a scope choice, not a copied constant. The
    /// scan rechecks every 100 ticks independently of whether a movement
    /// behavior is eligible to start; the interval bounds terrain work without
    /// coupling it to the movement scheduler.
    const CAT_BLOCK_SEARCH_INTERVAL_TICKS: i32 = 100;
    /// The sitting search bounds: horizontal range 8 and vertical range 1,
    /// centered on the mob's block position.
    const CAT_SIT_HORIZONTAL_RANGE: i32 = 8;
    const CAT_SIT_VERTICAL_RANGE: i32 = 1;
    /// The bed search bounds: horizontal range 8, vertical start -2, and
    /// vertical range 6.
    const CAT_BED_HORIZONTAL_RANGE: i32 = 8;
    const CAT_BED_VERTICAL_MIN: i32 = -2;
    const CAT_BED_VERTICAL_MAX: i32 = 6;

    /// The cat block-spiral search, run here rather than inside either goal —
    /// `docs/mob-block-perception.md`'s own guidance for a goal that needs to
    /// search a neighbourhood ("must not be built on [block cues]… that is a
    /// host-computed candidate position instead"), the same shape
    /// [`tick_villager_professions`](Self::tick_villager_professions) already
    /// is for the identical reason. See
    /// [`lodestone_entity::ai::MobController::cat_sit_target`]'s own doc for
    /// the seam this feeds.
    ///
    /// The scan checks the whole box and keeps the closest valid cell by real
    /// squared distance. It therefore does not depend on ring traversal order
    /// when several valid cells are present.
    ///
    /// Throttled per mob by [`SimMob::cat_search_cooldown`], the same shape
    /// [`job_search_cooldown`](SimMob::job_search_cooldown) already uses.
    ///
    /// No `wasm32` gate — unlike [`tick_villager_professions`](Self::tick_villager_professions),
    /// this touches no `std::fs`-backed type.
    fn tick_cat_block_search(&mut self) {
        let world = self.world;
        for mob in &mut self.mobs {
            if mob.entity_type.path() != "cat" {
                continue;
            }
            if mob.cat_search_cooldown > 0 {
                mob.cat_search_cooldown -= 1;
                continue;
            }
            mob.cat_search_cooldown = Self::CAT_BLOCK_SEARCH_INTERVAL_TICKS;
            let pos = mob.position();
            let origin = BlockPos::new(
                pos.x.floor() as i32,
                pos.y.floor() as i32,
                pos.z.floor() as i32,
            );

            // A sitting target is a chest, a lit furnace, or a bed's non-head
            // part.
            let sit = Self::find_nearest_cat_block(
                world,
                origin,
                Self::CAT_SIT_HORIZONTAL_RANGE,
                -Self::CAT_SIT_VERTICAL_RANGE,
                Self::CAT_SIT_VERTICAL_RANGE,
                |state| {
                    let bare = villager::bare_block_id(state);
                    bare == "chest"
                        || (bare == "furnace" && state.contains("lit=true"))
                        || (bare.ends_with("_bed") && !state.contains("part=head"))
                },
            );
            mob.mob.set_cat_sit_target(sit);

            // A bed target accepts either bed part; unlike sitting, no
            // head/foot distinction is needed here.
            let bed = Self::find_nearest_cat_block(
                world,
                origin,
                Self::CAT_BED_HORIZONTAL_RANGE,
                Self::CAT_BED_VERTICAL_MIN,
                Self::CAT_BED_VERTICAL_MAX,
                |state| villager::bare_block_id(state).ends_with("_bed"),
            );
            mob.mob.set_cat_bed_target(bed);
        }
    }

    /// The bounded box scan [`tick_cat_block_search`](Self::tick_cat_block_search)
    /// runs for both cat goals: every cell in `[-horiz, horiz]` horizontally
    /// and `[y_min, y_max]` vertically around `origin`, gated by the same
    /// headroom check vanilla's own valid-target check makes
    /// (an "is empty block" test on the cell above, approximated here as the `#air`
    /// tag's three members — `air`/`cave_air`/`void_air` — rather than a real
    /// per-block-state emptiness census). Returns the nearest match's
    /// stand-on point: one block above the matched cell, block-centred,
    /// matching vanilla's own generic "move to block" goal's own
    /// move-to-target getter (one block above).
    fn find_nearest_cat_block(
        world: &ChunkWorld,
        origin: BlockPos,
        horiz: i32,
        y_min: i32,
        y_max: i32,
        is_valid: impl Fn(&str) -> bool,
    ) -> Option<Vec3> {
        let mut best: Option<(i32, Vec3)> = None;
        for dy in y_min..=y_max {
            for dx in -horiz..=horiz {
                for dz in -horiz..=horiz {
                    let x = origin.x + dx;
                    let y = origin.y + dy;
                    let z = origin.z + dz;
                    let above = world.block_state(x, y + 1, z);
                    if !matches!(villager::bare_block_id(above), "air" | "cave_air" | "void_air") {
                        continue;
                    }
                    let state = world.block_state(x, y, z);
                    if !is_valid(state) {
                        continue;
                    }
                    let dist = dx * dx + dy * dy + dz * dz;
                    let better = match best {
                        Some((best_dist, _)) => dist < best_dist,
                        None => true,
                    };
                    if better {
                        best = Some((
                            dist,
                            Vec3::new(f64::from(x) + 0.5, f64::from(y) + 1.0, f64::from(z) + 0.5),
                        ));
                    }
                }
            }
        }
        best.map(|(_, pos)| pos)
    }

    /// Ticks between gossip-spread passes. The whole-pass throttle is
    /// intentionally separate from the per-pair gossip values; it keeps the
    /// radius-bounded scan deterministic and bounded.
    const GOSSIP_SPREAD_INTERVAL_TICKS: u64 = 100;
    /// How close two villagers must be to gossip this pass. This crate uses an
    /// explicit squared radius so the pair scan remains bounded.
    const GOSSIP_SPREAD_RADIUS_SQR: f64 = 64.0; // 8 blocks

    /// Nearby villagers exchange gossip during a periodic radius-bounded scan
    /// over every villager pair. The pass is an approximation of the
    /// sensor-driven meeting behavior; the `villager` module supplies the
    /// workstation-claiming boundary separately.
    ///
    /// Both directions of a meeting pair exchange from a **pre-transfer
    /// snapshot** of each side (`source_a`/`source_b`, cloned before either
    /// mutates), so the second transfer never reads the first transfer's
    /// updated state. The result is independent of pair-transfer order.
    fn spread_villager_gossip(&mut self) {
        if self.tick_count % Self::GOSSIP_SPREAD_INTERVAL_TICKS != 0 {
            return;
        }
        let mut rng = self.gossip_spread_rng.clone();
        let villagers: Vec<(usize, Vec3)> = self
            .mobs
            .iter()
            .enumerate()
            .filter(|(_, m)| m.entity_type.path() == "villager")
            .map(|(i, m)| (i, m.position()))
            .collect();
        for a in 0..villagers.len() {
            for b in (a + 1)..villagers.len() {
                let (ia, pa) = villagers[a];
                let (ib, pb) = villagers[b];
                let dist_sqr =
                    (pa.x - pb.x).powi(2) + (pa.y - pb.y).powi(2) + (pa.z - pb.z).powi(2);
                if dist_sqr > Self::GOSSIP_SPREAD_RADIUS_SQR {
                    continue;
                }
                let (lo, hi) = if ia < ib { (ia, ib) } else { (ib, ia) };
                let (left, right) = self.mobs.split_at_mut(hi);
                let source_lo = left[lo].gossip.clone();
                let source_hi = right[0].gossip.clone();
                left[lo]
                    .gossip
                    .transfer_from(&source_hi, |bound| rng.next_int(bound), 10);
                right[0]
                    .gossip
                    .transfer_from(&source_lo, |bound| rng.next_int(bound), 10);
            }
        }
        self.gossip_spread_rng = rng;
    }

    /// Ticks between golem-summon checks. The periodic check runs every 100
    /// ticks.
    const GOLEM_SUMMON_INTERVAL_TICKS: u64 = 100;
    /// Host-side radius for the hostile-nearby check: 8 blocks squared. The
    /// test is recomputed from the live mob list rather than a villager memory.
    const GOLEM_SUMMON_HOSTILE_RANGE_SQR: f64 = 64.0;
    /// Axis-aligned agreement box for golem spawning, inflated `10.0` on every
    /// axis.
    const GOLEM_AGREEMENT_RADIUS: f64 = 10.0;
    /// Number of villagers required by the hurt/hostile agreement path. The
    /// gossip-transfer path uses a separate threshold and is not part of this
    /// check.
    const GOLEM_VILLAGERS_NEEDED: usize = 3;
    /// Memory lifetime after a successful golem spawn: `599` ticks before the
    /// next summon attempt is eligible.
    const GOLEM_DETECTED_TTL: u64 = 599;

    /// Golem-summon-on-hurt: the 100-tick cadence checks each villager that is
    /// hurt or has a hostile nearby.
    ///
    /// # Why this lives on `MobSim` rather than in a single-mob behavior
    ///
    /// It needs other villagers' state (the agreement count) and the ability to
    /// create a new entity. A single-mob behavior cannot provide either, so
    /// "is this villager hurt or does it see a hostile" is recomputed here from
    /// [`SimMob::last_hurt_by`] and [`species::is_hostile_species`]
    /// over `self.mobs`, matching the same hurt/nearby-hostile inputs
    /// sensors
    /// would answer rather than reading their output.
    ///
    /// # Three explicit behavior boundaries
    ///
    /// * **Sleep state is not an eligibility input.** Villager records do not
    ///   carry bed state, so the hurt/hostile agreement check uses the
    ///   available mob state without an additional rest requirement.
    /// * **Placement uses a fixed adjacent cell.** The terrain interface is a
    ///   pathfinding snapshot rather than a live column scan, so the agreement
    ///   result places the golem one block beside the triggering villager.
    /// * **One spawn candidate is evaluated per pass.** Candidates are sorted
    ///   by id and the first qualifying candidate keeps the result deterministic.
    fn tick_golem_summon(&mut self) {
        if self.tick_count % Self::GOLEM_SUMMON_INTERVAL_TICKS != 0 {
            return;
        }
        let tick_count = self.tick_count;
        for m in &mut self.mobs {
            if m.entity_type.path() == "villager"
                && m.golem_detected_until.is_some_and(|until| tick_count >= until)
            {
                m.golem_detected_until = None;
            }
        }

        let hostile_positions: Vec<Vec3> = self
            .mobs
            .iter()
            .filter(|m| species::is_hostile_species(&m.entity_type))
            .map(|m| m.position())
            .collect();

        // `wantsToSpawnGolem`: not on cooldown, and hurt or a hostile nearby.
        let candidates: Vec<(i32, Vec3)> = self
            .mobs
            .iter()
            .filter(|m| m.entity_type.path() == "villager" && m.golem_detected_until.is_none())
            .filter(|m| {
                let pos = m.position();
                m.last_hurt_by().is_some()
                    || hostile_positions.iter().any(|hp| {
                        let d = *hp - pos;
                        d.dot(d) <= Self::GOLEM_SUMMON_HOSTILE_RANGE_SQR
                    })
            })
            .map(|m| (m.id, m.position()))
            .collect();

        if candidates.is_empty() {
            return;
        }

        let (_, origin_pos) = candidates[0];
        let within_box = |pos: Vec3| {
            (pos.x - origin_pos.x).abs() <= Self::GOLEM_AGREEMENT_RADIUS
                && (pos.y - origin_pos.y).abs() <= Self::GOLEM_AGREEMENT_RADIUS
                && (pos.z - origin_pos.z).abs() <= Self::GOLEM_AGREEMENT_RADIUS
        };
        let agreeing = candidates.iter().filter(|&&(_, pos)| within_box(pos)).take(5).count();
        if agreeing < Self::GOLEM_VILLAGERS_NEEDED {
            return;
        }

        let golem_pos = Vec3::new(origin_pos.x + 1.0, origin_pos.y, origin_pos.z);
        self.spawn_species(
            "minecraft:iron_golem".parse().expect("static key"),
            golem_pos,
        );

        // `nearbyVillagers.forEach(GolemSensor::golemDetected)`: every
        // villager in the search box, not only the ones that individually
        // wanted a golem — vanilla marks the whole nearby set.
        let until = tick_count + Self::GOLEM_DETECTED_TTL;
        for m in &mut self.mobs {
            if m.entity_type.path() == "villager" && within_box(m.position()) {
                m.golem_detected_until = Some(until);
            }
        }
    }

    /// This villager's summed reputation toward `player` —
    /// vanilla's own "get player reputation" getter. `0` for a non-villager mob or an
    /// untracked player, matching
    /// [`villager::gossip::GossipContainer::reputation`]'s own default.
    #[must_use]
    pub fn villager_reputation(&self, villager_id: i32, player: uuid::Uuid) -> i32 {
        self.get(villager_id)
            .map(|m| m.gossip.reputation(player))
            .unwrap_or(0)
    }

    /// Applies a reputation event directly to `villager_id`'s
    /// own gossip ledger — the entry point [`attack_from_player`](Self::attack_from_player)
    /// uses internally, and what any future caller with a villager id and a
    /// source uuid in hand (a wired `SELECT_TRADE` handler for `Trade`, a
    /// golem-death hook for `GolemKilled`) should call once it exists. A
    /// no-op if `villager_id` names no live mob.
    pub fn record_reputation_event(
        &mut self,
        villager_id: i32,
        event: villager::reputation::ReputationEventType,
        source: uuid::Uuid,
    ) {
        if let Some(mob) = self.get_mut(villager_id) {
            villager::reputation::apply_reputation_event(&mut mob.gossip, event, source);
        }
    }

    /// One tick, settling dropped items against a caller-supplied solidity
    /// oracle — the live world, when the caller has one.
    ///
    /// Only the item-settling pass consults `block_state`; everything else still
    /// reads the snapshot, because mob pathfinding genuinely wants a view that
    /// does not change underneath a search in progress. Items are the opposite
    /// case: an item has to land on the block that is there *this* tick.
    ///
    /// **The oracle is a block-state *name*, not a solid/air boolean.** A name
    /// distinguishes shapes such as a bottom slab, soul sand, and a grass patch
    /// when [`ItemCollision`] computes the resting surface.
    pub fn tick_with_terrain(&mut self, block_state: &dyn Fn(i32, i32, i32) -> String) {
        // Feed every mob's perception inputs before its goals run. The pass
        // supplies `nearest_player`, `temptation`, `avoid_threat`,
        // `no_action_time`, `partner_candidate`, and `parent_candidate`; the
        // subsequent goal tick evaluates `can_use` against those values.
        //
        // `no_action_time` increments before the perception pass, so each goal
        // sees the value for the current simulation tick. The seam test checks
        // that the same value reaches both the controller and the simulated mob.
        //
        // The reset check runs immediately before the increment. Reusing
        // [`check_despawn`] keeps the player-distance reset rule in one place;
        // this call reads only `reset_timer`, passes `rng_hit_800: false`, and
        // ignores `discard` because removal belongs to [`despawn_pass`].
        for m in &mut self.mobs {
            let pos = m.position();
            let nearest = self
                .players
                .iter()
                .map(|p| dist_sqr(p.perception.position, pos))
                .min_by(f64::total_cmp);
            // Player proximity is the **only** reset condition here, and
            // deliberately *not* vanilla's other one.
            //
            // Vanilla's own "check despawn" step's `else` branch does clear the timer every tick
            // for a mob that requires persistence, keyed on
            // its own "is persistence required or requires custom persistence"
            // check. Keying
            // this off `SimMob::persistent` would look like a faithful port and
            // would not be one, because that flag carries a **wider** meaning
            // here than vanilla's: `spawn_species` sets it from `!hostile`, so
            // every passive animal is `persistent` in this crate. Vanilla animals
            // are not persistence-required — they opt out of distance
            // despawning through their own "removes when far away" override returning false,
            // which the despawn check consults for
            // *discarding* and never for the timer. Only a name-tagged or
            // summoned mob takes vanilla's `else` branch.
            //
            // Including it therefore would not have been "more vanilla": it would
            // have given every cow, pig and sheep in the world a permanently open
            // idle throttle regardless of whether any player was near. Measured,
            // not reasoned — the first draft did include it, and
            // `tests/mob_sim.rs`'s
            // `no_action_time_crosses_the_seam_instead_of_staying_on_the_sim_record`
            // failed its own precondition, because its cow's counter could no
            // longer climb past 100 at all. `despawn_pass` treats `persistent` the
            // same way (an early `return true`, with no reset), so the two agree.
            //
            // Modelling vanilla's real persistence branch needs a flag that means
            // `isPersistenceRequired` and nothing else; that is a separate change
            // to what `spawn_species` records, not something to smuggle in here.
            let reset = nearest.is_some_and(|dist_sqr| {
                crate::mob_spawn::check_despawn(m.category, dist_sqr, m.no_action_time, false, true)
                    .reset_timer
            });
            if reset {
                m.no_action_time = 0;
            }
            m.no_action_time = m.no_action_time.saturating_add(1);
            // Vanilla's own shoulder-riding per-tick update's own unconditional
            // ride-cooldown-counter increment, mirrored the same way `no_action_time`
            // is above.
            m.shoulder_dismount_ticks = m.shoulder_dismount_ticks.saturating_add(1);
        }
        self.feed_perception();

        // Retain the attacked position from each record so the resolution pass
        // can identify which player, if any, receives the hit.
        let mut hits: Vec<(Option<i32>, Vec3, f32, Vec3)> = Vec::new();
        let mut detonations: Vec<(i32, Vec3)> = Vec::new();
        let mut bred: Vec<(i32, Vec3, ResourceKey)> = Vec::new();
        // Accumulated into a local and moved into
        // `self.pending_grazes` after the loop, not pushed directly — `self` is
        // mutably borrowed by `&mut self.mobs` for the whole loop, exactly as it
        // is for `hits`/`detonations`/`bred`.
        let mut grazes: Vec<(BlockPos, EatenBlock)> = Vec::new();
        let mut launches: Vec<(i32, ProjectileLaunch)> = Vec::new();
        // Self-inflicted damage requests are drained per mob and resolved
        // below, after `hits`.
        let mut self_damage: Vec<(i32, f32)> = Vec::new();
        // Idle ambient vocalisations rolled this tick — accumulated into a
        // local for the same reason `grazes`/`bred` are: `self.mobs` is
        // mutably borrowed for the whole loop.
        let mut ambient_sounds: Vec<crate::effects::WorldEffect> = Vec::new();
        // Elder guardian mining-fatigue pulses rolled this tick —
        // accumulated into a local for the same reason `grazes`/`bred` are:
        // `self.mobs` is mutably borrowed for the whole loop, and reading
        // `self.players` from inside it (a *different* field) is fine, but
        // this sim still cannot push straight onto `self.pending_mining_fatigue`
        // without an extra borrow of `self` the loop already avoids for the
        // others.
        let mut mining_fatigue: Vec<MiningFatigueAura> = Vec::new();
        // Vanilla's own zombified-piglin alert-others call, resolved the same way
        // `hits`/`bred` are: accumulated into a local while `self.mobs` is
        // mutably borrowed for the per-mob loop below, then applied to the
        // rest of `self.mobs` in a second pass afterwards. Each entry is
        // (alerting piglin's own position, its live target's position) — see
        // `piglin_alert_ticks`'s own doc comment for the mechanism and the
        // disclosed target-position approximation.
        let mut piglin_alerts: Vec<(Vec3, Vec3)> = Vec::new();
        // The morning-gift request and per-tick shoulder-mount request — both
        // drained per mob the same way `bred`/`grazes` are (own mob id, since
        // resolving either needs a second look at `self.mobs`/`self.players`
        // after the per-mob loop releases its borrow).
        let mut gift_requests: Vec<i32> = Vec::new();
        let mut shoulder_requests: Vec<i32> = Vec::new();
        let tick_count = self.tick_count;
        // The disconnect self-heal for a mounted mob — the mob twin of
        // `tick_vehicles`' identical guard for a boat (see that comment for
        // why it is gated on a *non-empty* roster: `set_players` starts empty
        // before anyone has moved, and treating that as "nobody is connected"
        // would evict a rider the instant they mounted). Without this a mount
        // whose rider crashed or quit stays `Some(id)` forever and never ticks
        // its own goal AI again below.
        if !self.players.is_empty() {
            let connected: Vec<i32> = self
                .players
                .iter()
                .filter_map(|p| p.identity.map(|identity| identity.entity_id))
                .collect();
            if !connected.is_empty() {
                for mob in &mut self.mobs {
                    if mob.rider.is_some_and(|rider| !connected.contains(&rider)) {
                        mob.rider = None;
                    }
                }
            }
        }
        for m in &mut self.mobs {
            // Vanilla ages `invulnerableTime`/`hurtTime` every tick regardless
            // of whether the mob was hit this tick.
            m.hurt_cooldown.tick();
            // A ridden mob's movement is client-authoritative
            // (`MobSim::apply_mob_move`), the same handover `tick_vehicles`
            // documents for an unridden boat: running goal AI here too would
            // fight the rider's own reports and produce jitter, so a mount's
            // goal selector simply does not tick while ridden.
            if m.rider.is_none() {
                m.mob.tick(&mut m.goals);
            }
            // Vanilla's own generic per-tick base update's ambient-sound roll runs every tick a
            // mob is alive, independent of any goal — see
            // `roll_ambient_sound`'s own doc.
            if m.health > 0.0 {
                if let Some(effect) = roll_ambient_sound(m, tick_count) {
                    ambient_sounds.push(effect);
                }
            }
            // Vanilla's own zombified-piglin AI step's private alert-others call
            // — see `piglin_alert_ticks`'s own doc comment for the mechanism.
            // `attack_target()` doubles as "current live target position" per
            // that same disclosed approximation (this seam has no entity
            // reference to resolve a byte-exact live-target getter from).
            if m.health > 0.0 && m.entity_type.path() == "zombified_piglin" {
                match m.mob.attack_target() {
                    Some(_) if m.piglin_alert_ticks < 0 => {
                        m.piglin_alert_ticks = piglin_alert_interval(&mut m.mob);
                    }
                    Some(target_pos) if m.piglin_alert_ticks == 0 => {
                        let world = self.world;
                        if world.is_clear(m.position(), target_pos) {
                            piglin_alerts.push((m.position(), target_pos));
                        }
                        m.piglin_alert_ticks = piglin_alert_interval(&mut m.mob);
                    }
                    Some(_) => {
                        m.piglin_alert_ticks -= 1;
                    }
                    None => {
                        m.piglin_alert_ticks = -1;
                    }
                }
            }
            // `armadillo_danger_ticks`'s countdown — see its own doc comment.
            // A dead armadillo does not un-scare (matching every other
            // per-mob timer in this loop, which is likewise gated on
            // `health > 0.0`; a corpse's fields are frozen, not ticked).
            if m.health > 0.0 && m.armadillo_danger_ticks > 0 {
                m.armadillo_danger_ticks -= 1;
            }
            // `axolotl_play_dead_ticks`'s countdown — same "a corpse's
            // fields are frozen, not ticked" gate as the armadillo one
            // above.
            if m.health > 0.0 && m.axolotl_play_dead_ticks > 0 {
                m.axolotl_play_dead_ticks -= 1;
            }
            // `allay_liked_noteblock`'s cooldown countdown, and
            // `allay_duplication_cooldown`'s — see both fields' own docs.
            // Cleared outright at zero rather than left as `Some((pos, 0))`,
            // the disclosed simplification `allay_liked_noteblock`'s own doc
            // names.
            if m.health > 0.0 && m.entity_type.path() == "allay" {
                if let Some((pos, ticks)) = m.allay_liked_noteblock {
                    m.allay_liked_noteblock = if ticks > 1 { Some((pos, ticks - 1)) } else { None };
                }
                if m.allay_duplication_cooldown > 0 {
                    m.allay_duplication_cooldown -= 1;
                }
            }
            // A camel entering water clears its sitting pose before the random
            // sitting toggle runs. Only one of the two branches fires in a tick.
            if m.health > 0.0 && m.entity_type.path() == "camel" {
                if m.camel_sitting && m.in_water() {
                    m.camel_sitting = false;
                    m.camel_pose_tick = tick_count as i64;
                } else {
                    camel_random_sitting(m, tick_count);
                }
                // A living camel decrements its dash cooldown; corpse fields
                // remain frozen like every other per-mob timer in this loop.
                if m.camel_dash_cooldown > 0 {
                    m.camel_dash_cooldown -= 1;
                }
            }
            let new_attacks = m.mob.take_new_attacks();
            // A bee's sting connects when its attack event is emitted. Only the
            // first sting matters; clearing `anger` here prevents reacquisition.
            if !new_attacks.is_empty() && m.stung_at.is_none() && m.entity_type.path() == "bee" {
                m.stung_at = Some(tick_count);
                m.anger = None;
            }
            for target_pos in new_attacks {
                // Carry the attacker's position so the victim can retaliate and
                // identify the source of the hit.
                hits.push((m.attack_target_id, target_pos, m.attack_damage, m.position()));
            }
            // A stung bee's self-destruct roll — see
            // `bee_sting_death_roll` for the exact formula
            // and its two derived bounds (certainly alive at sting+1,
            // certainly dead by sting+1200).
            if let Some(stung_at) = m.stung_at {
                let elapsed = tick_count.saturating_sub(stung_at);
                if elapsed > 0 && elapsed % 5 == 0 && bee_sting_death_roll(tick_count, m.id, elapsed)
                {
                    // A fixed, large amount rather than `m.health`: this is a
                    // lethal roll, not a graded hit, and `apply_damage`'s
                    // reductions (armour, absorption) must not be able to
                    // leave a "certainly dead" tick non-lethal.
                    m.mob.damage_self(10_000.0);
                }
            }
            if m.mob.take_detonated() {
                detonations.push((m.id, m.position()));
            }
            // Drain the breeding flag. The mob controller records the event;
            // this driver resolves it into a child because it owns the entity
            // registry and the partner-independent spawn decision.
            if m.mob.take_bred() {
                bred.push((m.id, m.position(), m.entity_type().clone()));
            }
            // Same one-shot-flag drain shape as `take_bred` above.
            if m.mob.take_gift_requested() {
                gift_requests.push(m.id);
            }
            if m.mob.take_shoulder_ride_requested() {
                shoulder_requests.push(m.id);
            }
            // The goal records *that* a block was eaten and which of
            // the two positions it was; it cannot mutate the world, because this
            // sim borrows `world: &'w ChunkWorld` immutably. So this takes the
            // same route `pending_detonations` does — accumulate here, and let
            // `crate::tick::run_tick_loop` (which owns mutable chunk access)
            // apply it. The tick loop owns mutable chunk access, so the
            // simulation records the event and the loop performs the write.
            for what in m.mob.take_new_eaten() {
                grazes.push((m.mob.block_position(), what));
            }
            // Paired with the launching mob's id so the impact pass can exclude
            // it: a projectile is created inside its shooter's own bounding box,
            // so without an owner a skeleton's arrow hits the skeleton.
            launches.extend(m.mob.take_new_launches().into_iter().map(|l| (m.id, l)));
            for amount in m.mob.take_self_damage() {
                self_damage.push((m.id, amount));
            }
            // Villager gossip decays on a 24000-tick cadence. `None` records
            // the first timestamp without applying decay.
            if m.entity_type.path() == "villager" {
                match m.last_gossip_decay_tick {
                    None => m.last_gossip_decay_tick = Some(tick_count),
                    Some(last) if tick_count >= last + 24000 => {
                        m.gossip.decay();
                        m.last_gossip_decay_tick = Some(tick_count);
                    }
                    Some(_) => {}
                }
            }
            // Advance a zombie-villager conversion countdown.
            if m.entity_type.path() == "zombie_villager"
                && let Some(mut state) = m.conversion
            {
                let pos = m.position();
                let world = self.world;
                let progress = villager::conversion::conversion_progress(
                    || self.zombie_conversion_rng.next_f32(),
                    || villager::conversion::count_nearby_special_blocks(world, pos),
                );
                state.remaining_ticks -= progress;
                if state.remaining_ticks <= 0 {
                    // Conversion changes the species-derived combat stats,
                    // category, and gossip seed; profession, level, and XP are
                    // already fields on `SimMob`.
                    m.set_entity_type(
                        ResourceKey::from_str("minecraft:villager").expect("static key"),
                    );
                    m.category = MobCategory::Creature;
                    let (max_health, attack_damage, defenses, knockback_resistance) =
                        combat_defaults(&m.entity_type);
                    m.max_health = max_health;
                    m.health = m.health.min(max_health);
                    m.attack_damage = attack_damage;
                    m.defenses = defenses;
                    m.knockback_resistance = knockback_resistance;
                    if let Some(starter) = state.starter {
                        villager::reputation::apply_reputation_event(
                            &mut m.gossip,
                            villager::reputation::ReputationEventType::ZombieVillagerCured,
                            starter,
                        );
                    }
                    // Conversion applies nausea for 200 ticks at amplifier 0.
                    // It is a timed effect visible through `SimMob::effects()`.
                    m.effects.apply("minecraft:nausea", 200, 0);
                    m.conversion = None;
                    let block_pos = BlockPos::new(
                        pos.x.floor() as i32,
                        pos.y.floor() as i32,
                        pos.z.floor() as i32,
                    );
                    ambient_sounds.push(crate::effects::WorldEffect::LevelEvent {
                        event: crate::effects::SOUND_ZOMBIE_CONVERTED,
                        pos: block_pos,
                        data: 0,
                        global: false,
                    });
                } else {
                    m.conversion = Some(state);
                }
            }
            // Emit the elder-guardian mining-fatigue pulse on its periodic
            // interval. `tick_count` is the simulation clock for this schedule.
            if m.entity_type.path() == "elder_guardian"
                && tick_count.wrapping_add(m.id as u64) % ELDER_GUARDIAN_EFFECT_INTERVAL == 0
            {
                let source_pos = m.position();
                for player in &self.players {
                    let Some(identity) = player.identity else {
                        continue;
                    };
                    let delta = source_pos - player.perception.position;
                    if delta.dot(delta)
                        < ELDER_GUARDIAN_EFFECT_RADIUS * ELDER_GUARDIAN_EFFECT_RADIUS
                    {
                        mining_fatigue.push(MiningFatigueAura { target: identity });
                    }
                }
            }
        }
        self.push_entities();
        self.pending_grazes.extend(grazes);
        self.pending_ambient_sounds.extend(ambient_sounds);
        self.pending_mining_fatigue.extend(mining_fatigue);
        // Propagate zombified-piglin alerts after the per-mob loop releases
        // each `SimMob` borrow. The shared box from
        // `alert_species("zombified_piglin")` bounds the one-shot pack alert,
        // and mobs with an existing grudge keep their current target.
        if let Some((box_xz, box_y, _)) = alert_species("zombified_piglin") {
            for (source_pos, target_pos) in piglin_alerts {
                for other in &mut self.mobs {
                    if other.entity_type.path() != "zombified_piglin" || other.anger.is_some() {
                        continue;
                    }
                    let p = other.position();
                    if (p.x - source_pos.x).abs() > box_xz
                        || (p.z - source_pos.z).abs() > box_xz
                        || (p.y - source_pos.y).abs() > box_y
                    {
                        continue;
                    }
                    other.anger = Some(Anger {
                        end_time: tick_count + grudge_ticks(&mut other.mob),
                        target: target_pos,
                    });
                }
            }
        }
        for (shooter, launch) in launches {
            use lodestone_entity::ai::roster::ranged::{integrates_as_arrow, projectile_entity_type};
            let projectile = if integrates_as_arrow(launch.kind) {
                Projectile::arrow(launch.origin, launch.velocity)
            } else {
                Projectile::throwable(launch.origin, launch.velocity)
            };
            let key = ResourceKey::from_str(&format!("minecraft:{}", projectile_entity_type(launch.kind)))
                .expect("static projectile key");
            self.spawn_projectile_from(key, projectile, Some(shooter));
        }
        for (target_id, target_pos, raw_damage, attacker_pos) in hits {
            if let Some(target_id) = target_id
                && let Some(target) = self.mobs.iter_mut().find(|m| m.id == target_id)
            {
                let applied = target.apply_damage(raw_damage, DamageFlags::default());
                target.mob.note_hurt(Some(attacker_pos));
                self.note_vocalisation(target_id, applied);
                continue;
            }
            // `attack_target_id` identifies another `SimMob`; player targets
            // arrive as a position with `target_id == None`. Match that
            // position against the player registry because this event carries
            // no player identity. The exact comparison is documented by
            // `PlayerHit`, including the possible stale-target miss.
            if let Some(identity) = self
                .players
                .iter()
                .find(|p| dist_sqr(p.perception.position, target_pos) < 1e-6)
                .and_then(|p| p.identity)
            {
                self.pending_player_hits.push(PlayerHit {
                    identity,
                    raw_damage,
                    attacker_pos,
                });
                // A tamed pet retaliates against the source that hurt its
                // owner. The event carries the attacker's position rather than
                // an entity identity, so each owned pet records that position
                // for its next target selection.
                for pet in &mut self.mobs {
                    if pet.owner_uuid() == Some(identity.uuid)
                        && pet.is_tame()
                        && pet.health() > 0.0
                    {
                        pet.mob.set_owner_hurt_by(Some(attacker_pos));
                    }
                }
            }
        }
        // Self-inflicted damage from a bee's sting self-destruct. `damage_self` only
        // records the intent; health lives here, so it is applied through the
        // same pipeline as a melee hit (invulnerability and armour reductions
        // included). Resolve it before retaining live mobs so a fatal event
        // removes its mob in the same tick.
        for (id, amount) in self_damage {
            if let Some(m) = self.get_mut(id) {
                let applied = m.apply_damage(amount, DamageFlags::default());
                self.note_vocalisation(id, applied);
            }
        }
        self.reap_dead();
        self.resolve_breeding(bred);
        // Drain the cat morning-gift roll/spawn and parrot shoulder-mount
        // request collected alongside `bred`.
        self.resolve_cat_gifts(gift_requests);
        self.resolve_shoulder_mounts(shoulder_requests);
        self.tick_shoulder_dismounts();

        // A detonation removes the initiating mob explicitly after applying
        // blast damage. This keeps self-removal independent of whether terrain
        // shields the mob from its own blast.
        for (id, pos) in detonations {
            self.explode(pos, CREEPER_EXPLOSION_RADIUS, DamageFlags::default());
            self.mobs.retain(|m| m.id != id);
            // Record the detonation separately from damage so a connected client
            // can receive the explosion event (particle and sound handling) even
            // when the blast does not damage an entity. See `take_detonations`'s
            // doc comment for the drain side.
            self.pending_detonations.push(Detonation {
                centre: pos,
                radius: CREEPER_EXPLOSION_RADIUS,
            });
        }

        // Advance both registries from this shared tick. Resolve projectile
        // impacts before motion so the swept segment is clipped at the first
        // collision; moving first would place impacts one tick late and could
        // carry an arrow through a wall.
        self.resolve_projectile_impacts();
        self.projectiles.tick();
        for despawned_item_id in self.items.tick() {
            self.item_state.remove(&despawned_item_id);
        }
        // **items land.** `ItemMotion::tick` is the entity's own
        // motion — gravity, translate, drag — and its doc comment has always said
        // "block collision that would zero a component is the world crate's job
        // and is expressed here through `on_ground`". Nothing ever did that job:
        // `on_ground` was set `false` by `ItemMotion::new` and never written
        // again, so every dropped item accelerated downward forever, fell through
        // the terrain, and streamed to the client until its 6000-tick despawn.
        //
        // That is also why merging never happened. `merge_neighbouring_items`
        // requires `|dy| < ITEM_MERGE_REACH_Y` (0.25), and two stacks dropped even
        // one tick apart fall at permanently different speeds — so the vertical
        // test could never pass for anything but two items spawned on the same
        // tick. Settling them onto a surface is what makes the merge reachable,
        // which is why the item lifecycle and inventory handoff are one operation.
        let world = self.world;
        let mut fell_out_of_the_world: Vec<i32> = Vec::new();
        let view = ItemCollision {
            block_state,
            probe_count: std::cell::Cell::new(0),
        };
        for (&id, state) in &mut self.item_state {
            let before = state.motion.position;
            state.motion.tick();
            settle_item(&view, &mut state.motion, before);
            if state.motion.position.y < f64::from(world.min_y) - VOID_DESPAWN_DEPTH {
                fell_out_of_the_world.push(id);
            }
        }
        // **The cost of the sweep, as a counter rather than a duration.** Swept
        // collision against real shapes is strictly more work per item than one
        // boolean lookup was, and the number of items in one tick is unbounded — so
        // the thing worth measuring is not per-item cost but how much of one tick a
        // floor covered in drops can consume. A counter is what a gate can assert
        // and what survives being read on a loaded machine; see
        // `items_settled_probe_count`.
        self.item_probe_count = view.probe_count.get();
        // Vanilla's own "check below world" discard, and not merely tidiness: an item
        // that escapes the world (a column the snapshot does not cover, so
        // `is_solid` is false everywhere) would otherwise keep being ticked and
        // streamed for its full 6000-tick life at ever-increasing depth.
        for id in fell_out_of_the_world {
            self.item_state.remove(&id);
            self.items.remove(id);
        }
        self.merge_neighbouring_items();
        // Experience orbs, on the same live-terrain oracle the items above use and for
        // the same reason: an orb settled against the sim's static `ChunkWorld`
        // snapshot would fall through any block the player has placed and rest on any
        // block they have mined. `tick_orbs` reads `tick_count` for its merge phase, so
        // it runs before the increment below.
        self.tick_orbs(&view);
        // A fireball's `ignite_seconds` used to reach nothing: computed by
        // `lodestone_entity::projectile::impact_effect` and read by no
        // production caller. `resolve_projectile_impacts` above is what can
        // raise a mob's burn counter (through `resolve_projectile_hit`); this
        // is the consumption half, run every tick regardless of whether a
        // fireball landed this one.
        self.tick_burning();
        self.tick_leashes();
        #[cfg(not(target_arch = "wasm32"))]
        self.tick_villager_professions();
        // villager bed claiming — see `tick_villager_beds`'s own
        // doc for why this is a separate memory from the job site above.
        #[cfg(not(target_arch = "wasm32"))]
        self.tick_villager_beds();
        // villager bell claiming — see `tick_villager_bells`'s
        // own doc for why this is a third, independent memory from the job
        // site and bed above.
        #[cfg(not(target_arch = "wasm32"))]
        self.tick_villager_bells();
        // fishing bobbers. Reads `self.world` (the static
        // per-tick terrain snapshot, not the live `view` oracle the item/orb
        // passes just above use) — see `fishing::MobSim::tick_fishing_bobbers`'s
        // own doc for why a bobber's whole interesting life is spent sitting
        // in open water, where the two oracles agree.
        self.tick_fishing_bobbers();
        // Raids. Wave spawning and victory/defeat need no live
        // terrain oracle either — see `raid::MobSim::tick_raids`'s own doc.
        self.tick_raids();
        // Nearby-villager gossip spread. No `wasm32` gate — unlike
        // `tick_villager_professions`, this touches no `std::fs`-backed type.
        self.spread_villager_gossip();
        // Golem-summon-on-hurt. No `wasm32` gate, for
        // `spread_villager_gossip`'s own reason.
        self.tick_golem_summon();
        // The cat's chest/lit-furnace/bed candidate search.
        self.tick_cat_block_search();
        // Allay item-carry-and-deliver — pick up matching ground
        // items, then throw one at a live delivery target. Order matters:
        // an item picked up this very tick could in principle also be
        // delivered this tick if the allay is already standing at its
        // target, matching vanilla's own same-tick pickup-then-throw
        // possibility rather than an arbitrary one-tick lag.
        self.allay_pick_up_items();
        self.allay_deliver_items();
        // The vibration substrate. Last, so every producer
        // earlier in this tick (currently just `reap_dead`'s `entity_die`)
        // has already posted before a listener resolves its nearest answer.
        self.resolve_vibrations();
        // Turns this tick's `nearest_vibration` answer
        // into real warden anger and, once angry and in range, a real melee
        // hit — see `warden::MobSim::resolve_warden_anger`'s own doc.
        self.resolve_warden_anger();
        // The sniffer's own seek/dig/
        // rise/egg-drop state machine — see `sniffer::MobSim::tick_sniffers`'s
        // own doc. No particular ordering dependency on the calls above;
        // placed last alongside the warden consumer as the other
        // per-species host-side driver this tick runs.
        self.tick_sniffers();

        self.tick_count += 1;
    }

    /// Shoves apart entities whose bodies overlap — vanilla's own generic
    /// entity-push step,
    /// invoked once per tick for every pushable neighbour by
    /// vanilla's own generic living-entity "push entities" step (queries
    /// pushable neighbours,
    /// then pushes each in turn), called near the end of
    /// vanilla's own generic living-entity per-tick base update, after that tick's own movement has already
    /// been applied — the same ordering `tick_with_terrain` gives this call,
    /// right after the per-mob loop that runs `m.mob.tick(...)`.
    ///
    /// # The formula lives in `lodestone-physics`, not here
    ///
    /// [`push_impulse`] delegates to [`lodestone_physics::pair_push_vector`],
    /// which already carries the full citation of vanilla's own generic entity-push step —
    /// see `docs/entity-push.md` for the derivation, including the
    /// genuinely-non-obvious `sqrt(max(|dx|,|dz|))` Chebyshev normaliser
    /// (not `sqrt(dx²+dz²)`) and the widened `0.01f`/`0.05f` literals. That
    /// module is otherwise **unwired** — `docs/entity-push.md`'s own "Wiring"
    /// section says nothing in `lodestone-shell`, `lodestone-ecs` or
    /// `lodestone-client` calls it yet, because its documented use case is
    /// the *client-authoritative local player* feeling a push from nearby
    /// entities, which is a different half of vanilla's symmetric rule from
    /// this one: a *server-authoritative mob* being shoved by a player or
    /// another mob. This call site is the first production consumer.
    ///
    /// # What this port narrows, disclosed
    ///
    /// * **Overlap is a horizontal-distance-under-combined-half-width test**,
    ///   not vanilla's real AABB intersection
    ///   (its own entity-query/pushable-neighbour helpers), which also accounts for
    ///   height overlap. Two mobs stacked exactly on top of one another with
    ///   no horizontal offset therefore push in this port and would not
    ///   collide in vanilla (their Y ranges might not overlap) — an edge case
    ///   this seam's `PathWorld`/`SimMob` do not carry enough geometry to
    ///   resolve exactly.
    /// * **Applied once per pair per tick**, not vanilla's twice (each side's
    ///   own "push entities" step invokes a push against the other, so a
    ///   living pair receives the impulse from *both* directions every tick).
    ///   The formula itself is unchanged; this halves the net closing-speed
    ///   reduction relative to vanilla's double application, a scope cut
    ///   rather than a transcription error.
    /// * **Player recoil is not applied.** A player's own position/velocity
    ///   in this codebase is client-authoritative (the client sends
    ///   `move_player_pos`; the server does not own a player's velocity the
    ///   way it owns a mob's), so shoving a player back needs a clientbound
    ///   self-velocity packet the client applies to its own physics —
    ///   `crates/protocol/**` and `crates/lodestone-shell/**`, both outside
    ///   this crate. This pass pushes the **mob** away from an intersecting
    ///   player (the reported "I can't push pigs" symptom: the pig now moves
    ///   out of the way), but the player itself is not nudged.
    /// * **"is pushable"/vehicle/passenger exclusions are not modelled** —
    ///   every [`SimMob`] is treated as pushable, matching vanilla's default
    ///   for a plain living entity with nothing riding it.
    /// * **Mount cramming damage is not modelled** — vanilla's own
    ///   `maxEntityCramming` gamerule check in the same method.
    fn push_entities(&mut self) {
        let n = self.mobs.len();
        if n == 0 {
            return;
        }
        let positions: Vec<Vec3> = self.mobs.iter().map(SimMob::position).collect();
        let widths: Vec<f64> = self
            .mobs
            .iter()
            .map(|m| f64::from(m.shape().width))
            .collect();
        let mut impulses = vec![Vec3::default(); n];

        // Mob-mob pairs.
        for i in 0..n {
            for j in (i + 1)..n {
                let touch = (widths[i] + widths[j]) / 2.0;
                if let Some((a, b)) = push_impulse(positions[i], positions[j], touch) {
                    impulses[i].x += a.x;
                    impulses[i].z += a.z;
                    impulses[j].x += b.x;
                    impulses[j].z += b.z;
                }
            }
        }

        // Player-mob pairs: only the mob side is pushed — see this method's
        // own doc comment for why player recoil needs a different seam.
        const PLAYER_WIDTH: f64 = 0.6; // Vanilla's own player attribute builder has no override; the default entity bounding box is 0.6 wide.
        for i in 0..n {
            for p in &self.players {
                let touch = (widths[i] + PLAYER_WIDTH) / 2.0;
                if let Some((mob_impulse, _)) = push_impulse(positions[i], p.perception.position, touch) {
                    impulses[i].x += mob_impulse.x;
                    impulses[i].z += mob_impulse.z;
                }
            }
        }

        for (mob, impulse) in self.mobs.iter_mut().zip(impulses) {
            if impulse.x != 0.0 || impulse.z != 0.0 {
                mob.apply_knockback(impulse);
            }
        }
    }

    /// Advances every mob's burn counter one tick and applies the damage it
    /// reports — the consumption half of vanilla's own generic per-tick base update's fire section
    /// (see `crate::burning`'s own module doc for the full mechanic), scoped
    /// to what actually reaches a mob today.
    ///
    /// **What ignites a mob**: only a fireball/wither-skull impact
    /// ([`MobSim::resolve_projectile_hit`]) currently raises the counter —
    /// this pass never itself ignites anything. Standing in a fire or lava
    /// block does **not** ignite a mob here; that half of `baseTick` is a
    /// disclosed gap (`crate::burning`'s own "What is not here" section
    /// already named "mob burning" as unwired at all — this closes the
    /// consumption half, not the block-contact ignition half).
    ///
    /// **What puts it out**: water contact only, read through
    /// [`SimMob::in_water`] (vanilla's own per-tick base update's water-block fire-clear call).
    /// Fire immunity ([`species::is_fire_immune`]) clears the counter outright
    /// rather than merely refusing damage, matching
    /// [`crate::burning::BurnState::tick`]'s own `fire_immune` handling.
    /// `standing_in` is always `None` (no fire/lava contact modelled for
    /// mobs), so the lava-guard and per-block contact-damage halves of
    /// [`crate::burning::BurnState::tick`] never fire from this call site —
    /// only the every-20-ticks burn tick itself does.
    fn tick_burning(&mut self) {
        let mut hits: Vec<(i32, f32)> = Vec::new();
        for m in &mut self.mobs {
            if m.in_water() {
                m.burn.clear();
                continue;
            }
            let fire_immune = species::is_fire_immune(&m.entity_type);
            let fire_resistance = m.effects.get("minecraft:fire_resistance").is_some();
            let out = m.burn.tick(None, fire_immune, fire_resistance);
            if out.damage > 0.0 {
                hits.push((m.id, out.damage));
            }
        }
        for (id, damage) in hits {
            if let Some(m) = self.get_mut(id) {
                let applied = m.apply_damage(
                    damage,
                    DamageFlags::for_damage_type_name("minecraft:on_fire").unwrap_or_default(),
                );
                self.note_vocalisation(id, applied);
            }
        }
        self.reap_dead();
    }

    /// Per-tick leash physics: pull leashed mobs toward their holder, and
    /// snap (dropping a lead item) past [`LEASH_TOO_FAR_DIST`] — vanilla's
    /// own leash per-tick update.
    ///
    /// **Simplified, and disclosed rather than silent.** Real vanilla
    /// computes a spring/torque interaction across up to four
    /// attachment-point pairs and applies angular momentum to yaw
    /// (its own elastic-interaction check/computation).
    /// This applies one straight-line impulse toward the holder's position
    /// instead, through [`SimMob::apply_knockback`] — the same "hand
    /// velocity application to the physics owner rather than growing a
    /// second model here" seam `explosion.rs`/`damage.rs` already use for
    /// combat knockback. Three things this does not carry:
    ///
    /// - No yaw torque (vanilla's own angular-momentum field and yaw setter).
    /// - **No per-entity bounding-box subtraction from the elastic
    ///   threshold** — vanilla's actual pull distance is
    ///   the elastic distance minus both entities' own bounding-box widths;
    ///   this uses the flat [`LEASH_ELASTIC_DIST`] constant, so a very wide
    ///   mob starts pulling slightly later than vanilla would.
    /// - **A holder that cannot be resolved this tick silently drops the
    ///   leash with no item spawned** — vanilla's own "cannot interact with
    ///   level" check's
    ///   branch, narrowed to its entity-drops-off arm (its own remove-leash path)
    ///   only. A disconnected player or a removed leash-holder mob loses the
    ///   leashed mob's attachment rather than the mob dropping a lead for a
    ///   holder that is not really gone (a reconnecting player, in
    ///   particular) — the safer of the two wrong answers, but still a
    ///   simplification worth naming.
    fn tick_leashes(&mut self) {
        let mut snapped: Vec<(i32, Vec3)> = Vec::new();
        let mut pulled: Vec<(i32, Vec3)> = Vec::new();
        let mut orphaned: Vec<i32> = Vec::new();

        for i in 0..self.mobs.len() {
            let Some(holder) = self.mobs[i].leash_holder else {
                continue;
            };
            let mob_pos = self.mobs[i].position();
            let holder_pos = match holder {
                LeashHolder::Player(uuid) => self
                    .players
                    .iter()
                    .find(|p| p.identity.as_ref().map(|i| i.uuid) == Some(uuid))
                    .map(|p| p.perception.position),
                LeashHolder::Mob(id) => self.mobs.iter().find(|m| m.id == id).map(SimMob::position),
                LeashHolder::Fence(pos) => Some(Vec3::new(
                    f64::from(pos.x) + 0.5,
                    f64::from(pos.y) + 0.5,
                    f64::from(pos.z) + 0.5,
                )),
            };
            let Some(holder_pos) = holder_pos else {
                orphaned.push(self.mobs[i].id);
                continue;
            };
            let distance = dist_sqr(mob_pos, holder_pos).sqrt();
            if distance > LEASH_TOO_FAR_DIST {
                snapped.push((self.mobs[i].id, mob_pos));
            } else if distance > LEASH_ELASTIC_DIST {
                let excess = distance - LEASH_ELASTIC_DIST;
                let dir = Vec3::new(
                    (holder_pos.x - mob_pos.x) / distance,
                    (holder_pos.y - mob_pos.y) / distance,
                    (holder_pos.z - mob_pos.z) / distance,
                );
                // Capped, so a mob yanked from far away is pulled steadily
                // rather than teleported in one tick — vanilla's own spring
                // is likewise bounded (`SPRING_DAMPENING`).
                let pull = excess.min(1.0) * 0.3;
                pulled.push((
                    self.mobs[i].id,
                    Vec3::new(dir.x * pull, dir.y * pull, dir.z * pull),
                ));
            }
        }

        for id in orphaned {
            if let Some(mob) = self.get_mut(id) {
                mob.set_leash_holder(None);
            }
        }
        for (id, impulse) in pulled {
            if let Some(mob) = self.get_mut(id) {
                mob.apply_knockback(impulse);
            }
        }
        for (id, pos) in snapped {
            if let Some(mob) = self.get_mut(id) {
                mob.set_leash_holder(None);
            }
            self.spawn_item(
                "minecraft:lead".parse().expect("valid key"),
                pos,
                Vec3::new(0.0, 0.0, 0.0),
                lodestone_entity::item_entity::ItemLifecycle::newly_dropped(
                    1,
                    lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                ),
            );
        }
    }

    /// Populates every mob's [`MobController`] perception inputs from this
    /// sim's own census plus [`set_players`](Self::set_players)' player list.
    ///
    /// Two passes, and the split is a borrow-checker necessity rather than a
    /// style choice: deciding mob `i`'s threat/partner/parent means reading
    /// every *other* mob, so the decisions are computed under shared borrows
    /// first and applied under a mutable one second. It is the same shape
    /// [`tick`](Self::tick) already uses for melee resolution.
    ///
    /// Nothing here is species-*goal* knowledge — that is the roster's job.
    /// The only species table it consults is [`avoided_species`], which answers
    /// "is that a threat to me", a perception question.
    fn feed_perception(&mut self) {
        let n = self.mobs.len();
        let mut nearest_player = vec![None; n];
        let mut temptation = vec![None; n];
        let mut threat = vec![None; n];
        let mut partner = vec![None; n];
        let mut parent = vec![None; n];
        let mut owner = vec![None; n];
        let mut patrol_group = vec![None; n];
        let mut stared_at = vec![false; n];
        let mut nearby_entities: Vec<Vec<NearbyBrainEntity>> = vec![Vec::new(); n];
        // How long each mob's owner has been asleep, used by shoulder and
        // morning-gift behavior; see the per-mob computation below for the
        // uuid/entity-id join.
        let mut owner_sleep_ticks: Vec<Option<u32>> = vec![None; n];
        // the nearest visible zombified piglin, fed to a
        // piglin's `AVOID` brain activity.
        let mut nearest_visible_zombified = vec![None; n];
        // the nearest eligible tongue-attack prey, fed to a
        // frog's `TONGUE` brain activity.
        let mut nearest_attackable_food = vec![None; n];
        // an allay's own delivery target, fed to its `DELIVER`
        // brain activity.
        let mut delivery_target = vec![None; n];

        // --- persistent anger (the anger deadline) -------------------------------
        //
        // Resolved here, in the feed, for the same reason every other
        // pre-computed answer is: `MobController::angry_target` hands the goal
        // an `Option<Vec3>`, never a query, because the seam has no shared game
        // clock to compare an absolute deadline against. So the host does the
        // comparison and only the answer crosses.
        //
        // `now >= end_time` clears the grudge outright rather than merely
        // reporting `None`; an expired grudge must not come back if the clock
        // is read again.
        let now = self.tick_count;

        // Warden pursuit: a warden tracks its own suspect
        // (`SimMob::warden_anger`/`warden_anger_target`, entirely separate
        // from the `SimMob::anger` primitive the loop below reads) and never
        // populates `me.anger`, so without this it would always feed `None`
        // here and its `Brain`'s `FIGHT` activity (`warden_brain`) would
        // never become eligible. Resolved in its own pre-pass, over an
        // immutable borrow of `self.mobs`, because it needs to look up a
        // *different* mob's current position by id — the same reason
        // `partner`/`parent`/`owner` above are resolved before the mutating
        // loop rather than inside it. Gated on `AngerLevel::Angry` (not
        // merely "has a tracked suspect") so an `Agitated` warden — anger
        // above zero but below the chase threshold — does not already start
        // walking, matching `resolve_warden_anger`'s own gate on the strike
        // itself.
        let warden_pursuit_target: Vec<Option<Vec3>> = self
            .mobs
            .iter()
            .map(|me| {
                if me.entity_type().path() != "warden"
                    || me.warden_emerge_ticks > 0
                    // Digging outranks fighting in the activity priority list,
                    // the same reason `warden_emerge_ticks`
                    // above already gates this off — a digging warden must
                    // not also start walking toward whatever it is angry at.
                    || me.warden_digging_ticks > 0
                    || !warden::AngerLevel::from_anger(me.warden_anger).is_angry()
                {
                    return None;
                }
                let target_id = me.warden_anger_target?;
                self.mobs.iter().find(|m| m.id == target_id).map(SimMob::position)
            })
            .collect();

        for (i, me) in self.mobs.iter_mut().enumerate() {
            if me.anger.is_some_and(|a| now >= a.end_time) {
                me.anger = None;
            }
            // A warden never sets `me.anger`, and no non-warden mob ever
            // gets a `warden_pursuit_target` entry (the closure above
            // returns `None` for every other species) — the two halves of
            // this `or` can never both be `Some` for the same mob, so this
            // is a merge of disjoint producers, not a priority order between
            // two that could disagree.
            let target = me.anger.map(|a| a.target).or(warden_pursuit_target[i]);
            me.mob.set_angry_target(target);
        }

        for i in 0..n {
            let me = &self.mobs[i];
            let pos = me.position();
            let species = me.entity_type().path().to_owned();

            // --- nearest player -------------------------------------------
            // Fed with **no range cut**, deliberately: vanilla's range for this
            // lives in the *goal*'s targeting conditions (`LookAtPlayerGoal`
            // takes a look-distance, 6.0F or 8.0F per species,
            // set in its own constructor), not on the mob, and our
            // `LookAtPlayerGoal::can_use` applies exactly that cut itself
            // (`goals.rs`). Cutting here as well would silently take the
            // minimum of two ranges and make the goal's own parameter a lie.
            nearest_player[i] =
                nearest_by(&self.players, pos, |p| p.perception.position, |_| true, None);

            // --- temptation -----------------------------------------------
            // The range *is* on the mob here (vanilla's own tempt-range attribute), so it
            // belongs in the feed. See `TEMPT_RANGE`.
            //
            // The item test is per-species (`tempt_food`), which is why
            // `PlayerPerception` carries the held item rather than a boolean:
            // the same wheat that tempts a cow does nothing to a chicken.
            let foods = species::tempt_food(&species);
            if !foods.is_empty() {
                temptation[i] = nearest_by(
                    &self.players,
                    pos,
                    |p| p.perception.position,
                    |p| {
                        p.perception
                            .held_item
                            .as_ref()
                            .is_some_and(|item| foods.contains(&item.path()))
                    },
                    Some((TEMPT_RANGE, TEMPT_RANGE)),
                );
            }

            // --- avoid threat ---------------------------------------------
            let avoided = species::avoided_species(&species);
            if !avoided.is_empty() {
                threat[i] = nearest_by(
                    &self.mobs,
                    pos,
                    SimMob::position,
                    |other| other.id != me.id && avoided.contains(&other.entity_type().path()),
                    Some((AVOID_RANGE, AVOID_RANGE_Y)),
                );
            }

            // --- breeding partner -----------------------------------------
            // Vanilla's own generic "can mate" check: the
            // partner must be the *same class* and both must be in love. A
            // baby cannot breed (vanilla's own "can fall in love" check gates on age), and
            // The continuing-breed check additionally requires the partner
            // not be panicking — enforced here
            // too, since feeding a panicking partner would start the goal only
            // for it to abort on the next tick.
            if me.is_in_love() && !me.is_baby() {
                partner[i] = nearest_by(
                    &self.mobs,
                    pos,
                    SimMob::position,
                    |other| {
                        other.id != me.id
                            && other.entity_type() == me.entity_type()
                            && other.is_in_love()
                            && !other.is_baby()
                            && !other.is_panicking()
                    },
                    Some((BREED_RANGE, BREED_RANGE)),
                );
            }

            // --- parent ---------------------------------------------------
            // Vanilla's own follow-parent goal: no goal while this mob's own
            // age is non-negative (adult),
            // and the candidate must itself have a non-negative age, i.e. be an
            // adult, searched over an `8.0, 4.0, 8.0` inflation.
            if me.is_baby() {
                parent[i] = nearest_by(
                    &self.mobs,
                    pos,
                    SimMob::position,
                    |other| {
                        other.id != me.id
                            && other.entity_type() == me.entity_type()
                            && !other.is_baby()
                    },
                    Some((FOLLOW_PARENT_RANGE, FOLLOW_PARENT_RANGE_Y)),
                );
            }

            // --- owner ----------------------------------------------------
            // The owner *identity* is a census fact (`SimMob::owner`); only the
            // resolved position can cross the seam
            // (`MobController::owner_position`), so this is resolved here
            // exactly like partner/parent.
            //
            // Both flavours resolve, and the player one is what taming produces:
            // vanilla's owner is a uuid (its own owner-uuid metadata field) and
            // its own owner getter resolves it against the level every time it is asked,
            // which is what `player_position` does here. A tamed pet whose owner
            // is not in the list resolves to `None` — offline, or in another
            // dimension, which are the same two cases vanilla's
            // own "owner's level differs from this level" check covers — and `None` is the correct
            // answer rather than a stale last-known position: a pet must not
            // path toward where you were an hour ago.
            //
            // `is_tame` is fed *unconditionally* below rather than derived from
            // this, because a mob is tame whether or not its owner is resolvable.
            match me.owner {
                Some(MobOwner::Mob(oid)) => {
                    owner[i] = nearest_by(
                        &self.mobs,
                        pos,
                        SimMob::position,
                        |other| other.id == oid,
                        None,
                    );
                }
                Some(MobOwner::Player(uuid)) => {
                    owner[i] = self.player_position(uuid);
                    // how long that same player has been asleep,
                    // joined through `self.players`' own uuid<->entity_id
                    // pairing (`PlayerIdentity`) against
                    // `self.sleeping_players`' entity-id-keyed roster — see
                    // `sleeping_players`'s own field doc for why the join
                    // happens here rather than the sleep roster carrying
                    // uuids itself.
                    if let Some(entity_id) = self
                        .players
                        .iter()
                        .find_map(|p| p.identity.filter(|id| id.uuid == uuid).map(|id| id.entity_id))
                    {
                        owner_sleep_ticks[i] = self
                            .sleeping_players
                            .iter()
                            .find(|&&(id, _)| id == entity_id)
                            .map(|&(_, since)| self.tick_count.saturating_sub(since) as u32);
                    }
                }
                None => {}
            }

            // --- nearest visible zombified piglin  -------------
            // A piglin's own "avoid" brain activity. No range cut lives on the
            // mob in the jar (vanilla's own piglin-specific sensor reads whatever
            // its own "nearest visible living entities" sensor already gathered), so this
            // reuses the same generous scan box `nearby_entities` above uses
            // for brain species, restricted to `zombified_piglin` and gated
            // on species the same way `threat[i]`/`temptation[i]` already
            // gate on a non-empty predicate table.
            if species == "piglin" {
                nearest_visible_zombified[i] = nearest_by(
                    &self.mobs,
                    pos,
                    SimMob::position,
                    |other| other.id != me.id && other.entity_type().path() == "zombified_piglin",
                    Some((NEARBY_HOSTILE_SCAN_RANGE, NEARBY_HOSTILE_SCAN_RANGE_Y)),
                );
            }

            // --- nearest eligible tongue-attack prey  ----------
            // A frog's own "tongue" brain activity. Vanilla's own frog-attackables
            // sensor's own
            // range is its own target-detection-distance constant (10.0F); `FROG_FOOD_SPECIES`
            // is the host-side stand-in for vanilla's own "can eat" check's
            // own frog-food entity-type tag (see that constant's own doc
            // for the disclosed size-1 narrowing this does not model).
            if species == "frog" {
                nearest_attackable_food[i] = nearest_by(
                    &self.mobs,
                    pos,
                    SimMob::position,
                    |other| {
                        other.id != me.id
                            && other.health > 0.0
                            && FROG_FOOD_SPECIES.contains(&other.entity_type().path())
                    },
                    Some((10.0, 10.0)),
                );
            }

            // --- allay delivery target  -------------------------
            // Vanilla's own "get item deposit position" helper's note-block half
            // (its own "should deposit items at liked noteblock" check): only offered once
            // there is something to deliver, a recently-heard note block is
            // still remembered, and the block there is still really a note
            // block (a player could have mined it since). One tick behind
            // `resolve_vibrations`'s own write, the same lag every other
            // activity-swap species' own tests already document — `hearing`
            // runs at the end of the *previous* tick's `MobSim::tick`.
            if species == "allay"
                && me.allay_inventory_count > 0
                && let Some((liked_pos, ticks)) = me.allay_liked_noteblock
                && ticks > 0
                && crate::redstone::base_name(self.world.block_state(
                    liked_pos.x as i32,
                    liked_pos.y as i32,
                    liked_pos.z as i32,
                )) == crate::redstone_note_block::NOTE_BLOCK
            {
                delivery_target[i] = Some(Vec3::new(liked_pos.x, liked_pos.y + 1.0, liked_pos.z));
            }

            // --- patrol group target ---------------------------------------
            // A leader never reads this — it computes its own
            // fresh target from `LongDistancePatrolGoal` itself; only a
            // non-leading, still-patrolling member needs the host's census.
            // See `nearest_patrol_leader_target`'s own doc comment for why
            // this cannot reuse `nearest_by`.
            if me.is_patrolling() && !me.is_patrol_leader() {
                patrol_group[i] = nearest_patrol_leader_target(&self.mobs, pos, me.id);
            }

            // --- gaze (the view-direction feed) -----------------------------------
            // `MobController::is_being_stared_at` is host-fed: the geometry is
            // `lodestone_entity::ai::mob::is_in_view_cone`, vanilla's exact
            // `dot > 1.0 - coneSize / dist`. Line of sight is the same
            // disclosed gap `find_nearest_target` already carries — no world
            // raycast at this seam, erring permissive. The carved-pumpkin
            // disguise check (vanilla's own "player not wearing disguise item"
            // condition) is not
            // modelled either: `PlayerPerception` has no armour-slot data yet.
            //
            // `0.025` is the enderman's own view-cone-size constant; this feed is per-mob, not per-species, so
            // every mob gets the same tolerance today — the only consumer is
            // `EndermanFreezeWhenLookedAt`, so this is not yet observably
            // wrong, but a second gaze-gated species with a different
            // `coneSize` would need this to become species-aware.
            let mob_eye = Vec3::new(pos.x, pos.y + f64::from(me.shape().height) * 0.85, pos.z);
            stared_at[i] = self.players.iter().any(|p| {
                let player_eye = Vec3::new(
                    p.perception.position.x,
                    p.perception.position.y + PLAYER_EYE_HEIGHT,
                    p.perception.position.z,
                );
                lodestone_entity::ai::mob::is_in_view_cone(
                    player_eye,
                    p.perception.view_direction,
                    mob_eye,
                    0.025,
                    true,
                )
            });

            // --- nearby entities (brain target-acquisition primitive) ------
            // Only built for brain-driven species: every other species'
            // `BrainMob::nearby_entities` default (empty) is never read, so
            // scanning the whole mob list for a goal-driven zombie would be
            // pure waste — the same cost-avoidance `avoided_species`'s
            // `is_empty()` check above already applies to a different feed.
            if is_brain_species(&species) {
                nearby_entities[i] = self
                    .mobs
                    .iter()
                    .filter(|other| {
                        other.id != me.id
                            && (other.position().x - pos.x).abs() <= NEARBY_HOSTILE_SCAN_RANGE
                            && (other.position().z - pos.z).abs() <= NEARBY_HOSTILE_SCAN_RANGE
                            && (other.position().y - pos.y).abs() <= NEARBY_HOSTILE_SCAN_RANGE_Y
                    })
                    .map(|other| NearbyBrainEntity {
                        id: other.id,
                        position: other.position(),
                        hostile: species::is_hostile_species(other.entity_type()),
                    })
                    .collect();
            }
        }

        // a plain field read, not a per-mob computation, so it
        // lives outside the loop below like every other constant the loop
        // reuses (`tick_count`) — `self.mobs.iter_mut()` only borrows the
        // `mobs` field, so this and that are disjoint borrows regardless.
        let day_time = self.day_time;
        let block_center = |p: BlockPos| {
            Vec3::new(f64::from(p.x) + 0.5, f64::from(p.y) + 0.5, f64::from(p.z) + 0.5)
        };

        for (i, m) in self.mobs.iter_mut().enumerate() {
            // Not folded into the chain below: `set_tame`/`set_ordered_to_sit`
            // read `m`'s own record while the chain holds `m.mob` mutably.
            let (tame, ordered_to_sit) = (m.tame, m.ordered_to_sit);
            // same reason as `tame`/`ordered_to_sit` above — read
            // before `m.mob` is borrowed mutably by the chain below.
            let shoulder_dismount_ticks = m.shoulder_dismount_ticks;
            m.mob.set_tame(tame).set_ordered_to_sit(ordered_to_sit);
            // the villager POI-claim feed
            // (`crate::brain::VillagerPoiSensor`'s own source), read before
            // `m.mob` is borrowed mutably below — `m.workstation`/`m.bed`/
            // `m.meeting_point` are `None` for every non-villager species,
            // so this is safe to feed unconditionally, the same "harmless
            // default" shape `set_nearby_entities` already is for a
            // goal-driven mob.
            let job_site = m.workstation.map(block_center);
            let home = m.bed.map(block_center);
            let meeting_point = m.meeting_point.map(block_center);
            m.mob
                .set_nearest_player(nearest_player[i])
                .set_temptation(temptation[i])
                .set_avoid_threat(threat[i])
                // The sim has incremented this every tick since long before
                // this mob's record, but it never crossed the
                // `MobController` seam, so idle
                // suppression read the trait default `0` and never fired.
                .set_no_action_time(m.no_action_time)
                .set_love_partner_candidate(partner[i])
                .set_parent_candidate(parent[i])
                .set_owner(owner[i])
                .set_patrol_group_target(patrol_group[i])
                .set_stared_at(stared_at[i])
                .set_nearby_entities(std::mem::take(&mut nearby_entities[i]))
                .set_job_site(job_site)
                .set_home(home)
                .set_meeting_point(meeting_point)
                .set_owner_sleep_ticks(owner_sleep_ticks[i])
                .set_nearest_visible_zombified(nearest_visible_zombified[i])
                .set_nearest_attackable_food(nearest_attackable_food[i])
                .set_delivery_target(delivery_target[i])
                // a sniffer's own host-found dig-search target,
                // fed to its `Brain`'s `WalkToPoi` — see
                // `sniffer::MobSim::tick_sniffers`'s own doc for the state
                // machine that produces this. `None` for every non-sniffer
                // species, the same harmless-default shape every other
                // host-computed-candidate field here already is.
                .set_sniffer_dig_target(m.sniffer_dig_target)
                .set_ticks_since_shoulder_dismount(shoulder_dismount_ticks)
                .set_day_time(day_time);
        }
    }

    /// A player right-clicked a mob with (or without) an item — vanilla's own
    /// generic mob-interact dispatch reaching each species' own interaction
    /// override, the single producer for taming, sitting,
    /// feeding and breeding.
    ///
    /// # The dispatch order is the specification
    ///
    /// Vanilla's per-species interaction overrides are nested `if` chains that end in
    /// their parent's own version, so *which arm wins* is as much a part of the port as
    /// the constants are. Two orderings that both "tame a wolf" differ
    /// observably: feeding a hurt tame wolf meat must heal it, **not** put it in
    /// love, and only once it is at full health does the same item breed it
    /// (the wolf's own interaction override's first arm, then its parent's
    /// generic animal interaction). This method's arms are in that order and each one
    /// names which vanilla interaction override it comes from, in prose.
    ///
    /// # What is deliberately not here
    ///
    /// Collar dyeing, wolf body armour and its repair, the parrot's poisonous
    /// cookie, and mounting a tame horse. Each needs an item model this crate
    /// does not have (dye components, equipment slots, damage values) or a
    /// passenger model that does not exist.
    ///
    /// Returns [`InteractOutcome::Pass`] when nothing responded, which is the
    /// caller's signal to fall through to whatever it does with an unconsumed
    /// right-click.
    /// Attaches or detaches a lead between `mob_id` and the player `holder` —
    /// vanilla's own generic entity-interact step's two leash-specific branches (excluding
    /// its sneak-multi-attach branch; see this method's own "not
    /// implemented" note).
    ///
    /// - If `mob_id` is already leashed to `holder`, detaches it (vanilla's
    ///   own "current holder is this player" arm) and reports whether a
    ///   `minecraft:lead` item should be spawned (`creative` mirrors
    ///   vanilla's own "has infinite materials" check, which this sim has no game-mode
    ///   state of its own to answer).
    /// - Else, if `holding_lead` and the mob is not already held by a
    ///   *player* (vanilla's own "current holder is not a player"
    ///   guard — one player cannot steal another's leashed mob just by
    ///   holding a lead), attaches it to `holder`, dropping any existing
    ///   non-player leash first exactly as vanilla's own drop-leash call does
    ///   before its own set-leashed-to call.
    /// - Otherwise refuses: not leashable, no lead in hand, or out of
    ///   [`LEASH_TOO_FAR_DIST`] (vanilla's own "can have a leash attached to"
    ///   check's own snap-distance check).
    ///
    /// **Not implemented**: vanilla's sneak-right-click branch, which
    /// re-parents *every* mob already leashed to `holder` onto whatever
    /// entity was clicked, in one interaction. This only ever moves the one
    /// `mob_id` named — a real gap for a player leashing several animals to
    /// one another, not merely an unlikely input.
    pub fn try_leash(
        &mut self,
        mob_id: i32,
        holder: Uuid,
        holding_lead: bool,
        creative: bool,
    ) -> LeashOutcome {
        let Some(mob) = self.get(mob_id) else {
            return LeashOutcome::Refused;
        };
        if mob.leash_holder() == Some(LeashHolder::Player(holder)) {
            let pos = mob.position();
            self.get_mut(mob_id)
                .expect("just found")
                .set_leash_holder(None);
            let dropped_lead = !creative;
            if dropped_lead {
                // Spawned here, not left to the caller, for the same reason
                // `tick_leashes`' snap branch spawns its own item: one place
                // decides "a lead item now exists in the world", so a future
                // second call site cannot forget it or double it.
                self.spawn_item(
                    "minecraft:lead".parse().expect("valid key"),
                    pos,
                    Vec3::new(0.0, 0.0, 0.0),
                    lodestone_entity::item_entity::ItemLifecycle::newly_dropped(
                        1,
                        lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                    ),
                );
            }
            return LeashOutcome::Detached { dropped_lead };
        }
        if !holding_lead || matches!(mob.leash_holder(), Some(LeashHolder::Player(_))) {
            return LeashOutcome::Refused;
        }
        if !species::is_leashable_species(mob.entity_type()) {
            return LeashOutcome::Refused;
        }
        let mob_pos = mob.position();
        let Some(holder_pos) = self
            .players
            .iter()
            .find(|p| p.identity.as_ref().map(|i| i.uuid) == Some(holder))
            .map(|p| p.perception.position)
        else {
            return LeashOutcome::Refused;
        };
        if dist_sqr(mob_pos, holder_pos).sqrt() > LEASH_TOO_FAR_DIST {
            return LeashOutcome::Refused;
        }
        self.get_mut(mob_id)
            .expect("just found")
            .set_leash_holder(Some(LeashHolder::Player(holder)));
        LeashOutcome::Attached
    }

    /// Right-clicking a fence while holding a lead: re-parents every mob
    /// currently leashed to `holder` (the player) onto a knot at `fence_pos`
    /// — vanilla's own lead-item "bind player mobs" call. Unlike vanilla this never spawns
    /// a fence-knot decoration entity; see [`LeashHolder::Fence`]'s own doc
    /// comment for why, and for what that costs a real client (no visible
    /// knot to render or right-click).
    ///
    /// **Simplified from vanilla's own scan**: vanilla's own "bind player
    /// mobs" call only
    /// re-parents mobs within a 32-block radius of `fence_pos`; this moves
    /// every mob leashed to `holder` regardless of distance from the fence.
    /// The two coincide in practice — a leashed mob is already capped at
    /// [`LEASH_TOO_FAR_DIST`] (12 blocks) from `holder`, and a player using
    /// this interaction is, by construction, standing at the fence — but a
    /// contrived setup (holder far from the fence, mob far from holder in
    /// the other direction) could observe the difference.
    ///
    /// Returns the ids re-leashed; empty means no mob was leashed to
    /// `holder` at all, matching vanilla's own pass-through result.
    pub fn try_leash_to_fence(&mut self, holder: Uuid, fence_pos: BlockPos) -> Vec<i32> {
        let mut moved = Vec::new();
        for mob in &mut self.mobs {
            if mob.leash_holder == Some(LeashHolder::Player(holder)) {
                mob.leash_holder = Some(LeashHolder::Fence(fence_pos));
                moved.push(mob.id);
            }
        }
        moved
    }

    /// Spawns a wandering trader at `pos` with 1–2 leashed llama escorts —
    /// the entity-spawn half of vanilla's own wandering-trader spawner's own
    /// spawn call.
    /// Returns the trader's id and every llama actually spawned.
    ///
    /// **This is only the "given a spawn position, create the entity group"
    /// half.** Vanilla's own wandering-trader spawner itself is a generic
    /// custom-spawner driven by
    /// the world tick with its own 1200-tick poll, a 24000-tick base delay,
    /// a climbing 25→75% chance, a player-anchored 48-block search for a
    /// meeting-point point-of-interest (falling back to the player), and a
    /// "no wandering trader spawns" biome-tag exclusion — none of which
    /// exists in this crate. That whole cycle belongs beside
    /// [`crate::mob_spawn`]'s existing per-species natural-spawn cap/timer
    /// engine, a file outside this pass's ownership; see this session's
    /// broker note (wandering trader spawn cycle) for the exact shape a
    /// caller there needs.
    ///
    /// **Simplified escort placement.** Vanilla's own "try to spawn llama
    /// for" step
    /// searches up to 10 candidate positions within 4 blocks and can fail to
    /// find space, so "2 attempts" does not guarantee 2 llamas. This always
    /// places both at fixed offsets (`+2, 0, 0` and `-2, 0, 0` from the
    /// trader) with no space check — this sim has no per-cell obstruction
    /// query at the `MobSim` level the way vanilla's own block-getter seam does, and
    /// two llamas beside an already-chosen valid trader spawn are the common
    /// case in practice.
    ///
    /// **Wares are not generated.** Vanilla's own wandering-trader "update
    /// trades" step builds
    /// its offer list from its own buying/uncommon/common wandering-trader
    /// trade-set tables
    /// — this crate has no merchant-offer/trade-table model at all yet (see
    /// the villager-trading work this is deliberately distinct from). A
    /// spawned trader here has no wares and cannot be traded with.
    pub fn spawn_wandering_trader(&mut self, pos: Vec3) -> (i32, Vec<i32>) {
        let trader_id = self
            .spawn_species("minecraft:wandering_trader".parse().expect("valid key"), pos)
            .id();
        let mut llamas = Vec::new();
        for dx in [2.0, -2.0] {
            let llama_id = self
                .spawn_species(
                    "minecraft:trader_llama".parse().expect("valid key"),
                    Vec3::new(pos.x + dx, pos.y, pos.z),
                )
                .id();
            self.get_mut(llama_id)
                .expect("just spawned")
                .set_leash_holder(Some(LeashHolder::Mob(trader_id)));
            llamas.push(llama_id);
        }
        (trader_id, llamas)
    }

    pub fn interact(
        &mut self,
        mob_id: i32,
        actor: PlayerIdentity,
        held_item: Option<&ResourceKey>,
    ) -> InteractOutcome {
        let Some(mob) = self.mobs.iter().find(|m| m.id == mob_id) else {
            return InteractOutcome::Pass;
        };
        let species = mob.entity_type().path().to_owned();
        let pos = mob.position();
        let item = held_item.map(|k| k.path().to_owned());
        let item = item.as_deref();

        // Vanilla's own villager interaction override is a full override, not an
        // animal-interaction
        // fall-through: a villager is never tameable, so this has to be a
        // short-circuit ahead of the `tame_mechanism` dispatch below rather
        // than another arm inside it.
        if species == "villager" {
            let profession = mob.profession;
            let level = mob.villager_level;
            let has_offers = !villager::trades::offers_up_to(profession, level).is_empty();
            let outcome = if matches!(
                profession,
                villager::Profession::None | villager::Profession::Nitwit
            ) || !has_offers
            {
                // No job, or a job this crate has not ported real trades for
                // yet (`villager::trades`' own doc names which professions
                // those are) — an honest `Pass` rather than an empty screen.
                InteractOutcome::Pass
            } else {
                InteractOutcome::OpenTrade { profession, level }
            };
            return outcome;
        }

        // Zombie-villager interaction: only the golden-apple/weakness case
        // gets special handling. A golden apple used without Weakness (the
        // plain success result, which
        // does **not** reduce the stack) and every other item both fall
        // through to the generic dispatch below, which resolves to `Pass`
        // for a zombie villager exactly as its parent's generic interaction does for any
        // non-tameable monster — see `InteractOutcome::ZombieVillagerConversionStarted`'s
        // own doc for why that no-weakness arm is disclosed as `Pass` rather
        // than a distinct variant.
        if species == "zombie_villager" && item == Some("golden_apple") {
            let has_weakness = mob.effects().amplifier_of("minecraft:weakness").is_some();
            if !has_weakness {
                return InteractOutcome::Pass;
            }
            let state = villager::conversion::start_converting(Some(actor.uuid), |bound| {
                self.zombie_conversion_rng.next_int(bound)
            });
            let remaining_ticks = state.remaining_ticks;
            // Vanilla's own zombie-villager-cure sound's own play call:
            // `1.0F + random.nextFloat()` volume, `random.nextFloat() * 0.7F
            // + 0.3F` pitch (vanilla's own "start converting" call).
            let volume = 1.0 + self.zombie_conversion_rng.next_f32();
            let pitch = self.zombie_conversion_rng.next_f32() * 0.7 + 0.3;
            let seed = i64::from(self.zombie_conversion_rng.next_int(i32::MAX));
            if let Some(mob) = self.mobs.iter_mut().find(|m| m.id == mob_id) {
                mob.effects.remove("minecraft:weakness");
                // Vanilla's own difficulty-minus-one-clamped-to-zero formula: `0` on Easy/Normal/Hard
                // (ids 1-3), and this crate tracks no live difficulty integer
                // for a zombie villager's own amplifier calc — see this
                // module's `conversion` doc for the disclosed simplification.
                mob.effects.apply("minecraft:strength", remaining_ticks, 0);
                mob.conversion = Some(state);
            }
            if let Some(effect) = crate::effects::zombie_villager_cure_sound(pos, volume, pitch, seed) {
                self.pending_vocalisations.push(effect);
            }
            return InteractOutcome::ZombieVillagerConversionStarted;
        }

        // Allay interaction: duplication is checked first, then the
        // empty-handed carrying gift. An allay is never tameable, so — like
        // the villager and zombie-villager arms above — this is a
        // short-circuit ahead of the `tame_mechanism` dispatch rather than
        // another case inside it.
        //
        // See `InteractOutcome::AllayDuplicated`'s own doc for the disclosed
        // `isDancing()` substitution the duplication arm makes.
        //
        // **Not modelled here**: taking the item back (an empty-hand
        // right-click on a carrying allay).
        if species == "allay" {
            if item == Some("amethyst_shard")
                && mob.allay_liked_noteblock.is_some_and(|(_, ticks)| ticks > 0)
                && mob.allay_duplication_cooldown <= 0
            {
                self.spawn_species(
                    ResourceKey::from_str("minecraft:allay").expect("static key is valid"),
                    pos,
                )
                .allay_duplication_cooldown = ALLAY_DUPLICATION_COOLDOWN_TICKS;
                if let Some(mob) = self.mobs.iter_mut().find(|m| m.id == mob_id) {
                    mob.allay_duplication_cooldown = ALLAY_DUPLICATION_COOLDOWN_TICKS;
                }
                // This short-circuit returns before the shared
                // `outcome.particle()` tail below runs (the same reason the
                // villager/zombie-villager arms above never reach it
                // either), so vanilla's own allay entity-event handler's status-18 heart burst
                // has to be pushed here directly rather than relying on that
                // generic path.
                self.pending_vocalisations
                    .push(taming_particles("minecraft:heart", pos));
                return InteractOutcome::AllayDuplicated;
            }
            let already_holding = mob.mob.main_hand_item().is_some();
            if already_holding || item.is_none() {
                return InteractOutcome::Pass;
            }
            let given = item.map(str::to_owned);
            if let Some(mob) = self.mobs.iter_mut().find(|m| m.id == mob_id) {
                mob.mob.set_main_hand_item(given);
            }
            return InteractOutcome::ItemGiven;
        }

        // Vanilla's own camel interaction override is a full override, the same "not an
        // animal-interaction fall-through" shape as the villager arm
        // above — a camel is tamed unconditionally
        // (its own "is tamed" check), so unlike the horse family there is no temper
        // roll to gate riding on at all. Only the empty-handed
        // mount-ride half is ported: vanilla's own "secondary use active" check's
        // inventory-GUI branch and its own "is food" check's heal/age-up/love branch both
        // need machinery this crate does not have for this species yet
        // (a horse-style inventory screen, and a `camel` row in
        // `species::breeding_food` — a real, disclosed, still-missing gap,
        // not silently dropped), so any held item is left as `Pass` rather
        // than guessed at.
        if species == "camel" {
            if mob.is_baby() || item.is_some() {
                return InteractOutcome::Pass;
            }
            return if self.mount_mob(mob_id, actor.entity_id) {
                InteractOutcome::Mounted
            } else {
                // Already ridden by someone else — `mount_mob`'s own "one
                // map's worry" refusal.
                InteractOutcome::Pass
            };
        }

        let outcome = match species::tame_mechanism(&species) {
            Some(species::TameMechanism::Temper { max_temper }) => {
                self.interact_horse(mob_id, actor, item, max_temper)
            }
            Some(mechanism) => self.interact_tamable(mob_id, actor, item, &species, mechanism),
            // Every other species goes straight to vanilla's own generic
            // animal interaction.
            None => self.interact_animal(mob_id, item, &species),
        };

        // Vanilla's particles are an entity-status broadcast with status
        // `6`, `7`, or `18`,
        // which the *client* expands into a burst
        // (vanilla's own taming/love-mode particle spawners). This server has no `ENTITY_EVENT` encoder, so the burst is published
        // directly as a `LEVEL_PARTICLES` packet with the same particle type,
        // count and Gaussian spread the client would have produced. A disclosed
        // substitution, not an approximation of the visual: seven heart or smoke
        // particles at a randomized offset plus half a block of height either way.
        if let Some(particle) = outcome.particle() {
            self.pending_vocalisations
                .push(taming_particles(particle, pos));
        }
        outcome
    }

    /// Vanilla's own wolf/cat/parrot interaction overrides — the tameable-animal chain.
    fn interact_tamable(
        &mut self,
        mob_id: i32,
        actor: PlayerIdentity,
        item: Option<&str>,
        species: &str,
        mechanism: species::TameMechanism,
    ) -> InteractOutcome {
        let species::TameMechanism::FoodRoll {
            items,
            one_in,
            sit_on_success,
        } = mechanism
        else {
            return InteractOutcome::Pass;
        };
        let Some(mob) = self.mobs.iter().find(|m| m.id == mob_id) else {
            return InteractOutcome::Pass;
        };

        if mob.is_tame() {
            // Vanilla's own "is owned by" check — a tame animal ignores everyone but its owner.
            // Vanilla's cat wraps its whole body in this check and the wolf
            // repeats it per arm; the effect is the same.
            if mob.owner_uuid() != Some(actor.uuid) {
                return InteractOutcome::Pass;
            }
            // Vanilla's own wolf interaction override's first arm: is-food and
            // health-below-max → feed. **Before** the breeding arm, which is
            // reached only through its parent's generic interaction.
            let is_food = item.is_some_and(|i| species::breeding_food(species).contains(&i));
            if is_food && mob.health() < mob.max_health() {
                let heal = species::tame_feed_heal(species);
                let mob = self.get_mut(mob_id).expect("checked above");
                mob.heal(heal);
                return InteractOutcome::Fed;
            }
            // Its parent's generic animal interaction's love arm.
            if is_food && self.try_set_in_love(mob_id) {
                return InteractOutcome::InLove;
            }
            // Vanilla's own "did not consume the action, and is owned by the
            // player" check flips the sitting order. The *last* arm, so anything
            // above it suppresses the toggle — which is why an owner feeding a
            // hurt pet does not also sit it down.
            let mob = self.get_mut(mob_id).expect("checked above");
            let sitting = !mob.is_ordered_to_sit();
            mob.set_ordered_to_sit(sitting);
            return InteractOutcome::SitToggled { sitting };
        }

        // Untamed. The taming item is checked first and it is **not** the food
        // tag for the wolf: the bone item.
        if item.is_some_and(|i| items.contains(&i)) {
            // Vanilla's own wolf interaction override's "is not angry" guard. The cat and the
            // parrot have no such gate, and `anger` is `None` for them anyway,
            // so this is one condition rather than a per-species branch.
            if self.get(mob_id).is_some_and(|m| m.anger.is_some()) {
                return InteractOutcome::Pass;
            }
            // Vanilla's own "try to tame" step: one bounded-int draw, success on exactly `0`.
            let success = self.tame_rng.next_int(one_in) == 0;
            let mob = self.get_mut(mob_id).expect("checked above");
            if success {
                mob.tame(MobOwner::Player(actor.uuid));
                // Vanilla's own navigation-stop plus target-clear, then, for
                // the wolf and
                // the cat only, its own sit-order setter.
                mob.set_attack_target(None);
                mob.set_attack_target_id(None);
                if sit_on_success {
                    mob.set_ordered_to_sit(true);
                }
                return InteractOutcome::Tamed;
            }
            return InteractOutcome::TameFailed;
        }

        // Still its parent's generic animal interaction: an **untamed** wolf
        // fed meat really does fall in love in vanilla, because the bone check
        // above did not match and the chain continues.
        if item.is_some_and(|i| species::breeding_food(species).contains(&i))
            && self.try_set_in_love(mob_id)
        {
            return InteractOutcome::InLove;
        }
        InteractOutcome::Pass
    }

    /// Vanilla's own horse-family interaction override → its own "handle eating" step.
    ///
    /// The horse family's whole mechanism is a persisted counter, so this arm
    /// makes **no** tame roll: feeding raises `Temper` and nothing else. The roll
    /// lives in [`attempt_horse_tame`](Self::attempt_horse_tame), which
    /// `RunAroundLikeCrazyGoal` drives while a player is riding.
    fn interact_horse(
        &mut self,
        mob_id: i32,
        actor: PlayerIdentity,
        item: Option<&str>,
        max_temper: i32,
    ) -> InteractOutcome {
        let Some(item) = item else {
            // An empty-handed right-click is vanilla's own mount-ride call — vanilla's only
            // route to the tame roll (on an untamed horse; see
            // `attempt_horse_tame`'s doc for the one disclosed deviation) and,
            // now that a passenger model exists, to actually boarding a tamed
            // one. A baby is excluded exactly as vanilla's own
            // "is a vehicle, or is a baby" guard at the top of its interaction
            // override
            // routes it to its parent's generic animal interaction instead, which has no
            // empty-handed arm at all.
            let Some(mob) = self.get(mob_id) else {
                return InteractOutcome::Pass;
            };
            if mob.is_baby() {
                return InteractOutcome::Pass;
            }
            return if !mob.is_tame() {
                self.attempt_horse_tame(mob_id, actor, max_temper)
            } else if self.mount_mob(mob_id, actor.entity_id) {
                InteractOutcome::Mounted
            } else {
                // Already ridden by someone else.
                InteractOutcome::Pass
            };
        };

        // Vanilla's own "handle eating" step's arms in order: heal, age-up,
        // love, temper. Love is
        // gated on tamed, age exactly zero, and not already in love, and only the two
        // gold items reach it.
        let mut used = false;
        if species::horse_breeding_items(item)
            && self.get(mob_id).is_some_and(SimMob::is_tame)
            && self.try_set_in_love(mob_id)
        {
            return InteractOutcome::InLove;
        }

        let gain = species::horse_temper_gain(item);
        let mob = match self.get_mut(mob_id) {
            Some(mob) => mob,
            None => return InteractOutcome::Pass,
        };
        // `if (temper > 0 && (itemUsed || !isTamed()) && getTemper() <
        // getMaxTemper())`. `hay_block` has `temper == 0` and so raises nothing,
        // however much of it you feed — the trap `horse_temper_gain` documents.
        if gain > 0 && mob.temper() < max_temper {
            let raised = (mob.temper() + gain).clamp(0, max_temper);
            mob.set_temper(raised);
            used = true;
        }
        if used {
            let temper = mob.temper();
            InteractOutcome::TemperRaised { temper }
        } else {
            InteractOutcome::Pass
        }
    }

    /// Vanilla's own generic animal interaction for a species with no taming at all — the cow, sheep,
    /// pig, chicken and rabbit route, and the only thing feeding them does.
    fn interact_animal(
        &mut self,
        mob_id: i32,
        item: Option<&str>,
        species: &str,
    ) -> InteractOutcome {
        if item.is_some_and(|i| species::breeding_food(species).contains(&i))
            && self.try_set_in_love(mob_id)
        {
            return InteractOutcome::InLove;
        }
        InteractOutcome::Pass
    }

    /// Vanilla's own generic animal interaction's love arm as a single testable condition:
    /// age exactly zero and can-fall-in-love, then its own "set in love" call.
    ///
    /// **`age == 0` exactly**, not `!is_baby()`. The two differ on a parent
    /// inside its post-breeding cooldown, whose age is a positive countdown: it
    /// is not a baby and it still cannot fall in love. Reading `!is_baby()` here
    /// would let a pair breed every 60 ticks forever.
    fn try_set_in_love(&mut self, mob_id: i32) -> bool {
        let Some(mob) = self.get_mut(mob_id) else {
            return false;
        };
        if mob.age() != 0 || mob.is_in_love() {
            return false;
        }
        mob.set_in_love();
        true
    }

    /// Vanilla's own "run around like crazy" goal's per-tick tame roll for the horse family:
    /// a bounded-int-under-max-temper draw, tested against the current temper, and on failure
    /// a `+5` temper modification plus its own "make mad" call.
    ///
    /// # The one disclosed deviation, and why it is not silent
    ///
    /// Vanilla reaches this roll from a **goal** that runs while a player is a
    /// passenger, gated on its own `random.nextInt(adjustedTickDelay(50)) == 0`
    /// — so a rider gets roughly one attempt every 25 ticks until the horse
    /// yields. This server has no passenger model at all, so there is nothing to
    /// stay mounted on and no goal to tick. The attempt is therefore made **once
    /// per mount attempt** (one empty-handed right-click), and the 1-in-50 outer
    /// gate is not drawn.
    ///
    /// What is *not* changed is the part that makes the horse a different
    /// mechanism from the wolf: the roll is still `nextInt(maxTemper) < temper`,
    /// still fails at temper `0` with certainty, and still adds 5 temper per
    /// failure — so a horse still has to be fed or ridden repeatedly, and the
    /// number of attempts it takes is vanilla's. Only the *pacing* differs.
    pub fn attempt_horse_tame(
        &mut self,
        mob_id: i32,
        actor: PlayerIdentity,
        max_temper: i32,
    ) -> InteractOutcome {
        let Some(mob) = self.get(mob_id) else {
            return InteractOutcome::Pass;
        };
        if mob.is_tame() || max_temper <= 0 {
            return InteractOutcome::Pass;
        }
        let temper = mob.temper();
        let success = self.tame_rng.next_int(max_temper) < temper;
        let mob = self.get_mut(mob_id).expect("checked above");
        if success {
            // Vanilla's own "tame with name" call: sets owner plus the tame
            // flag. Note it does **not**
            // order the horse to sit — horses have no sitting pose at all.
            mob.tame(MobOwner::Player(actor.uuid));
            InteractOutcome::Tamed
        } else {
            let raised = (temper + 5).clamp(0, max_temper);
            mob.set_temper(raised);
            InteractOutcome::TameFailed
        }
    }

    /// Turns each drained [`NavigatingMob::take_bred`] event into a real child
    /// mob and applies vanilla's post-breeding cooldown to **both** parents.
    ///
    /// Vanilla's own post-breeding child-finalization step
    /// does three things: sets the post-breeding age cooldown on both parents,
    /// resets love on both, and spawns the child. `NavigatingMob::breed` can
    /// only do the love reset on the mob that ran the goal — it has no notion
    /// of the partner or of creating an entity — so the other two are here.
    ///
    /// Identifying the partner is the interesting part: by the time this runs,
    /// `breed()` has already cleared the breeder's love state, so "the other
    /// mob still in love" is not a usable key. It uses proximity instead —
    /// vanilla only breeds when the pair is within
    /// [`BREED_DISTANCE_SQR`](BREED_DISTANCE_SQR) (the breeding goal's own
    /// squared-distance check),
    /// so the nearest same-species adult inside that radius *is* the partner.
    fn resolve_breeding(&mut self, bred: Vec<(i32, Vec3, ResourceKey)>) {
        if bred.is_empty() {
            return;
        }
        // A mob already consumed as someone else's partner must not breed
        // again this tick. Both animals of a pair can legitimately reach
        // `loveTime >= 60` on the same tick — each holds the other as its
        // partner candidate — and without this guard one mating produces two
        // children, doubling the population every time.
        let mut consumed: std::collections::HashSet<i32> = std::collections::HashSet::new();
        for (breeder_id, breeder_pos, species) in bred {
            if consumed.contains(&breeder_id) {
                continue;
            }
            let partner_id = self
                .mobs
                .iter()
                .filter(|m| {
                    m.id != breeder_id
                        && m.entity_type().path() == species.path()
                        && !m.is_baby()
                        && !consumed.contains(&m.id)
                        && dist_sqr(m.position(), breeder_pos) < BREED_DISTANCE_SQR
                })
                .min_by(|a, b| {
                    dist_sqr(a.position(), breeder_pos)
                        .total_cmp(&dist_sqr(b.position(), breeder_pos))
                })
                .map(SimMob::id);

            consumed.insert(breeder_id);
            for id in [Some(breeder_id), partner_id].into_iter().flatten() {
                consumed.insert(id);
                if let Some(m) = self.get_mut(id) {
                    m.set_age(PARENT_AGE_AFTER_BREEDING);
                    m.mob.reset_love();
                }
            }

            // The child spawns through `spawn_species`, not `spawn_with_type`,
            // so it inherits the same goal set and category any other mob of
            // its species gets — a child that could not act would be a fresh
            // island of exactly the kind this connectivity check exists to close.
            let child = self.spawn_species(species, breeder_pos);
            child.set_age(BABY_START_AGE);

            // Vanilla's own post-breeding child-finalization step's last statement:
            // if the `mob_drops` gamerule is set, spawn an experience orb worth
            // a bounded-int-under-7-plus-1 draw.
            //
            // **Constructed, not awarded**, and the distinction is visible:
            // vanilla's own orb-award helper splits an amount into denominations and tries
            // its own merge-to-existing step first, whereas breeding builds one orb with
            // one value directly. Routing this through `award_experience` would
            // let a second mating in the same spot silently fold into the first
            // orb, so `spawn_orb` is the right call even though the values are
            // small enough that denomination splitting would be a no-op.
            //
            // The gate is the `mob_drops` rule, exactly as for a mob's death
            // reward — breeding on a `doMobLoot false` server pops nothing.
            if self.mob_drops {
                let value = self.breed_rng.next_int(7) + 1;
                self.spawn_orb(value, breeder_pos, Vec3::new(0.0, 0.0, 0.0));
            }
        }
    }

    /// Runs [`tick`](MobSim::tick) `n` times.
    pub fn tick_for(&mut self, n: u64) {
        for _ in 0..n {
            self.tick();
        }
    }

    /// Drains and returns every [`Detonation`] [`tick`](Self::tick) has
    /// triggered since the last call — the handoff
    /// [`crate::tick::run_tick_loop`] uses to publish onto an
    /// [`crate::tick::ExplosionFeed`] every server tick, mirroring how
    /// [`items`](Self::item_count)' own despawn ids are drained rather than
    /// merely read. Draining (not just reading) is what keeps a detonation
    /// from being broadcast twice if a caller is slow to call this before
    /// the next [`tick`](Self::tick) runs.
    pub fn take_detonations(&mut self) -> Vec<Detonation> {
        std::mem::take(&mut self.pending_detonations)
    }

    /// Drains every hurt/death sound recorded since the last call.
    ///
    /// Drained rather than read for [`take_detonations`](Self::take_detonations)'
    /// reason — a slow consumer must not play the same hit twice.
    pub fn take_vocalisations(&mut self) -> Vec<crate::effects::WorldEffect> {
        std::mem::take(&mut self.pending_vocalisations)
    }

    /// Drains every idle ambient vocalisation [`tick`](Self::tick) has rolled
    /// since the last call — [`take_vocalisations`](Self::take_vocalisations)'
    /// periodic sibling. Drained for the same reason: a slow consumer must not
    /// replay the same moo twice.
    pub fn take_ambient_sounds(&mut self) -> Vec<crate::effects::WorldEffect> {
        std::mem::take(&mut self.pending_ambient_sounds)
    }

    /// Drains every per-entity animation cue recorded since the last call — the
    /// visible sibling of [`take_vocalisations`](Self::take_vocalisations), and
    /// drained rather than read for the same reason: a slow consumer must not
    /// flash the same hit twice.
    pub fn take_entity_animations(&mut self) -> Vec<MobAnimation> {
        std::mem::take(&mut self.pending_animations)
    }

    /// Records the hurt or death sound **and animation** for a hit that landed on
    /// mob `id` — vanilla's own generic hurt/die handlers playing
    /// their own hurt-sound/death-sound getters, plus the damage-event/entity-status-3
    /// broadcasts those two methods send alongside.
    ///
    /// Called from every funnel that applies damage rather than from
    /// [`SimMob::apply_damage`] itself, because the queue lives on the sim and
    /// `apply_damage` holds only the one mob. `applied <= 0.0` (a hit fully
    /// swallowed by i-frames or absorption) is silent *and* invisible, matching
    /// vanilla's own generic hurt handler returning before either broadcast — the guard is
    /// its own "took full damage" check there and the same `applied > 0.0` here.
    ///
    /// **Must be called before the end-of-tick `retain`**, or a killing blow
    /// finds no mob to read the species and position from and dies silently.
    ///
    /// # Why the sound and the animation share one entry point
    ///
    /// They share a *cause*. Vanilla emits both from inside its own generic
    /// hurt/die handlers
    /// under the same guard, so splitting them into two recorders here would give
    /// two chances for one damage funnel to be taught about one of them and not
    /// the other — which is exactly how the animation came to be missing while
    /// every funnel already had the sound.
    fn note_vocalisation(&mut self, id: i32, applied: f32) {
        if applied <= 0.0 {
            return;
        }
        let Some(mob) = self.mobs.iter_mut().find(|m| m.id == id) else {
            return;
        };
        // Vanilla's own "play hurt sound" step calls its own ambient-sound-time
        // reset before
        // playing the hurt sound itself, so a mob that just yelped in pain
        // does not also roll an idle vocalisation on the very next tick.
        mob.ambient_sound_time = -AMBIENT_SOUND_INTERVAL;
        // Hurt *and* death on a killing blow, in that order, because vanilla sends
        // both: its own generic hurt handler broadcasts the damage event and only then calls die,
        // which broadcasts byte 3. The client needs the flash to have started for
        // the tip-over to look like a death rather than a teleport.
        self.pending_animations
            .push(MobAnimation::Hurt { entity_id: id });
        if mob.health <= 0.0 {
            self.pending_animations
                .push(MobAnimation::Died { entity_id: id });
        }
        // Vanilla draws pitch from the level RNG; this sim's only clock is
        // `tick_count`, and consuming from a shared generator here would shift
        // every other draw. Mixed with the id so two mobs hit in one tick differ.
        let phase = (self.tick_count.wrapping_mul(31).wrapping_add(id as u64)) % 21;
        let pitch = 0.9 + phase as f32 * 0.01;
        let effect = crate::effects::mob_vocalisation(
            mob.entity_type.to_string().as_str(),
            mob.position(),
            mob.health <= 0.0,
            mob.category == MobCategory::Monster,
            pitch,
            self.tick_count as i64,
        );
        if let Some(effect) = effect {
            self.pending_vocalisations.push(effect);
        }
    }

    /// Drains every graze [`tick`](Self::tick) has recorded since the last call,
    /// as `(mob block position, which block)`.
    ///
    /// Drained rather than read for [`take_detonations`](Self::take_detonations)'
    /// reason — a slow consumer must not apply the same eat twice — and it exists
    /// at all because this sim cannot apply it itself: `world: &'w ChunkWorld` is
    /// an immutable borrow.
    ///
    /// # Consumer behavior
    ///
    /// With block mutation enabled:
    ///
    /// * [`EatenBlock::AtFeet`] → destroy the block at that cell, **no drops**
    ///   (its own destroy-block call with drops disabled).
    /// * [`EatenBlock::Below`] → set the cell one down to `minecraft:dirt`, plus
    ///   level event `2001` for the break particles.
    ///
    /// The "ate" notification is emitted **even when block mutation suppresses
    /// the block change**, so wool regrowth and world mutation are separable —
    /// the gamerule check belongs on the consumer, never in the goal.
    ///
    /// The consumer drains this queue to apply wool regrowth (unshearing and
    /// aging the coat by 60 ticks), which is entity metadata on the wire.
    pub fn take_grazes(&mut self) -> Vec<(BlockPos, EatenBlock)> {
        std::mem::take(&mut self.pending_grazes)
    }

    /// Drains every player hit by a hostile mob's melee attack since the last
    /// call — the player-facing twin of
    /// [`take_detonations`](Self::take_detonations)'s handoff shape, for the
    /// identical reason: this sim owns no connection, so the driver
    /// (`crate::server::serve_play`'s `vitals_tick` arm) is what turns each
    /// entry into a real `PlayerVitals::apply_damage` call and a `SET_HEALTH`/
    /// hurt-animation packet. See [`PlayerHit`]'s own doc comment for how a
    /// target position resolves to an identity, and its disclosed gap for a
    /// grudge target.
    pub fn take_player_hits(&mut self) -> Vec<PlayerHit> {
        std::mem::take(&mut self.pending_player_hits)
    }

    /// Drains every player caught in an elder guardian's mining-fatigue pulse
    /// since the last call — the same handoff shape as
    /// [`take_player_hits`](Self::take_player_hits) above and for the
    /// identical reason: this sim owns no connection, so the driver is what
    /// turns each entry into a real `ActiveEffects::apply` call and a
    /// `GUARDIAN_ELDER_EFFECT` game event. See [`MiningFatigueAura`]'s own
    /// doc comment for exactly what the driver must apply.
    pub fn take_mining_fatigue_auras(&mut self) -> Vec<MiningFatigueAura> {
        std::mem::take(&mut self.pending_mining_fatigue)
    }

    /// Drains every zombie reinforcement roll that passed since the last call
    /// — see [`ReinforcementCall`]'s own doc for the 50-candidate terrain
    /// search the driver performs before spawning one.
    pub fn take_reinforcement_calls(&mut self) -> Vec<ReinforcementCall> {
        std::mem::take(&mut self.pending_reinforcements)
    }

    /// Drains every fire-ignition attempt a live lightning bolt made this
    /// tick — [`pending_lightning_fires`](Self::pending_lightning_fires)'s own
    /// doc explains why this sim cannot place the fire itself. The driver
    /// (`crate::tick::run_tick_loop_with_weather`) is expected to test each
    /// position with `crate::fire::can_survive` against the *live* world and
    /// write `crate::fire::state_for_placement` only where the cell is air and
    /// survives — this drain hands over candidates, not verified placements;
    /// the "air and can-survive" gate stays at the call site.
    pub fn take_lightning_fires(&mut self) -> Vec<BlockPos> {
        std::mem::take(&mut self.pending_lightning_fires)
    }

    /// Drains every projectile-vs-block impact recorded since the last call —
    /// see [`ProjectileBlockHit`]'s own doc for why this sim hands the write
    /// to a driver rather than resolving it here. The driver is expected to
    /// read the *live* block at each `pos`, check it is really still
    /// `redstone_target::TARGET`, consult its own `ScheduledTickQueue` for
    /// `has_pending_decay`, and call `redstone_target::apply_hit`.
    pub fn take_projectile_block_hits(&mut self) -> Vec<ProjectileBlockHit> {
        std::mem::take(&mut self.pending_projectile_block_hits)
    }

    /// Every live mob or connected player's position, floored to a
    /// [`BlockPos`] — the living-entity-in-box census for a lightning target.
    /// Pre-culling is deferred to the caller; `lightning::find_lightning_target_around`
    /// filters to its own search box internally.
    #[must_use]
    pub fn living_entity_positions(&self) -> Vec<BlockPos> {
        self.mobs
            .iter()
            .map(|m| lightning::floor_block_pos(m.position()))
            .chain(
                self.players
                    .iter()
                    .map(|p| lightning::floor_block_pos(p.perception.position)),
            )
            .collect()
    }

    /// The number of ticks advanced so far.
    #[must_use]
    pub fn tick_count(&self) -> u64 {
        self.tick_count
    }

    /// Applies an explosion centred at `centre` with blast `radius` (TNT is
    /// `4.0`) to every live mob, through the real ray-sampled exposure model
    /// (`explosion::seen_percent`, sampled against the sim's own
    /// [`ChunkWorld`] via its [`RayView`] impl) and damage formula
    /// (`explosion::entity_damage`), landing through the same
    /// [`SimMob::apply_damage`] pipeline a melee hit uses. Before this,
    /// `explosion.rs` had no consumer anywhere in the tree — its exposure grid
    /// and damage formula were exercised only by their own hermetic unit
    /// tests, with no path from "an explosion happened" to a health value
    /// anywhere changing.
    ///
    /// `flags` lets the caller pick which reduction stages the blast bypasses;
    /// a plain `DamageFlags::default()` runs armour/absorption normally.
    ///
    /// Returns `(id, damage_dealt)` for every mob that took nonzero damage,
    /// and removes any mob the blast killed. A mob whose exposure is fully
    /// blocked (every sampled ray hits terrain before the centre) takes no
    /// damage and is absent from the result — a wall genuinely shields it,
    /// this is not a distance cutoff.
    pub fn explode(&mut self, centre: Vec3, radius: f32, flags: DamageFlags) -> Vec<(i32, f32)> {
        let mut dealt = Vec::new();
        for m in &mut self.mobs {
            let shape = m.shape();
            let box_ = ExplosionAabb::from_size(
                m.position(),
                f64::from(shape.width),
                f64::from(shape.height),
            );
            let box_center = Vec3::new(
                (box_.min.x + box_.max.x) / 2.0,
                (box_.min.y + box_.max.y) / 2.0,
                (box_.min.z + box_.max.z) / 2.0,
            );
            let exposure = seen_percent(centre, box_, self.world);
            if exposure <= 0.0 {
                continue;
            }
            let distance = (box_center - centre).length();
            let raw = entity_damage(radius, distance, exposure);
            if raw <= 0.0 {
                continue;
            }
            let applied = m.apply_damage(raw, flags);
            if applied > 0.0 {
                dealt.push((m.id, applied));
            }
        }
        // After the loop rather than inside it: `note_vocalisation`
        // needs `&mut self` while the loop holds `&mut self.mobs`, and it must
        // still precede the retain below so a mob the blast killed is read for
        // its death sound before it leaves.
        for &(id, applied) in &dealt {
            self.note_vocalisation(id, applied);
        }
        self.reap_dead();
        dealt
    }

    /// Resolves a melee attack against a live mob: runs the damage pipeline
    /// ([`SimMob::apply_damage`]) and, whenever the hit lands, applies the
    /// knockback impulse ([`lodestone_physics::knockback::knockback_impulse`]).
    /// Both results are written to the target state before the next
    /// [`snapshots`](Self::snapshots) call emits an entity packet.
    ///
    /// # Two knockback contributions
    ///
    /// Each damaging hit applies a flat `0.4` contribution and then the
    /// caller-supplied `knockback_power` bonus. A non-sprinting hit passes
    /// `0.0` for the bonus; sprinting adds the configured extra contribution.
    ///
    /// The two calls are chained through the same `knockback_impulse` primitive:
    /// the second call receives the first call's output velocity. This preserves
    /// the two successive halving/subtraction operations; one call with the
    /// summed power would halve the pre-hit velocity only once.
    ///
    /// # Direction
    ///
    /// Both calls use the horizontal vector from the target to the attacker,
    /// `dx = attacker_pos.x - target_pos.x` and
    /// `dz = attacker_pos.z - target_pos.z`. The impulse subtracts that
    /// direction from velocity, moving the target away from the attacker.
    ///
    /// [`NavigatingMob`] stores no ground-contact flag, so the attack uses the
    /// grounded branch of `knockback_impulse` with its `0.4`-capped vertical
    /// hop.
    ///
    /// Returns `None` if `target_id` names no live mob. Returns `Some` for
    /// every resolved hit, including one ignored by invulnerability frames
    /// (see [`AttackOutcome::damage_dealt`]). A killing blow removes the mob
    /// immediately rather than deferring removal to the next [`tick`](Self::tick).
    pub fn attack(
        &mut self,
        target_id: i32,
        attacker_pos: Vec3,
        raw_damage: f32,
        flags: DamageFlags,
        knockback_power: f64,
    ) -> Option<AttackOutcome> {
        // Read before the mutable borrow below: the grudge deadline is
        // absolute, so it needs the clock as of this tick.
        let now = self.tick_count;
        let (health, velocity, damage_dealt, pack_alert) = {
            let mob = self.get_mut(target_id)?;
            let damage_dealt = mob.apply_damage(raw_damage, flags);
            // Record the attacker's position with the damage event so the mob's
            // retaliation logic can select a target and its knockback logic can
            // use the same direction.
            mob.mob.note_hurt(Some(attacker_pos));
            // Start the persistent grudge alongside the retaliation record, so
            // both state changes share this attack's simulation tick.
            //
            // Every mob records the grudge; only species with an anger-gated
            // target rule read `angry_target`, so the shared state does not
            // alter species that have no such rule.
            let was_already_angry = mob.anger.is_some();
            let end_time = now + grudge_ticks(&mut mob.mob);
            mob.anger = Some(Anger {
                end_time,
                target: attacker_pos,
            });
            // Group alerting is enabled for the species returned by
            // `alert_species`. It runs only when this hit creates a grudge;
            // repeated hits during one grudge do not re-alert the group.
            let pack_alert = if was_already_angry {
                None
            } else {
                alert_species(mob.entity_type.path()).map(|(box_xz, box_y, need_owner_match)| {
                    (
                        mob.entity_type.clone(),
                        mob.position(),
                        mob.owner_uuid(),
                        need_owner_match,
                        box_xz,
                        box_y,
                    )
                })
            };
            // Mark this as player-attributed damage for
            // `PLAYER_HURT_EXPERIENCE_TIME` ticks; the death-loot path uses this
            // deadline when deciding whether to award experience.
            mob.hurt_by_player_until = Some(now + PLAYER_HURT_EXPERIENCE_TIME);
            if damage_dealt > 0.0 && mob.health() > 0.0 {
                let target_pos = mob.position();
                // Vector from the target to the attacker; the impulse moves the
                // target away from that source position.
                let dx = attacker_pos.x - target_pos.x;
                let dz = attacker_pos.z - target_pos.z;
                let v = mob.velocity();
                let jitter = || (1.0, 0.0);
                // Coincident horizontal positions use a fixed non-degenerate
                // fallback because this call has no random source. That case
                // needs only one fallback draw to produce a valid direction.
                //
                // First call: the mandatory flat knockback contribution on
                // every damaging hit.
                let after_default = lodestone_physics::knockback::knockback_impulse(
                    lodestone_physics::geometry::Vec3d { x: v.x, y: v.y, z: v.z },
                    true, // always the grounded branch — see this method's own doc comment.
                    MELEE_DEFAULT_KNOCKBACK_POWER,
                    dx,
                    dz,
                    mob.knockback_resistance(),
                    jitter,
                );
                // Second call: the attacker-specific bonus, chained onto the
                // first call's result.
                let new_velocity = if knockback_power > 0.0 {
                    lodestone_physics::knockback::knockback_impulse(
                        after_default,
                        true,
                        knockback_power,
                        dx,
                        dz,
                        mob.knockback_resistance(),
                        jitter,
                    )
                } else {
                    after_default
                };
                mob.apply_knockback(Vec3::new(new_velocity.x, new_velocity.y, new_velocity.z));
            }
            (mob.health(), mob.velocity(), damage_dealt, pack_alert)
        };
        // Resolve group alerts after the mutable borrow of `target_id` ends.
        // Each matching same-species mob in the alert box that has no active
        // grudge receives the victim's deadline and target position.
        if let Some((species_key, victim_pos, victim_owner, need_owner_match, box_xz, box_y)) =
            pack_alert
        {
            for other in &mut self.mobs {
                if other.id == target_id || other.entity_type != species_key {
                    continue;
                }
                if need_owner_match && other.owner_uuid() != victim_owner {
                    continue;
                }
                if other.anger.is_some() {
                    continue;
                }
                let p = other.position();
                if (p.x - victim_pos.x).abs() > box_xz
                    || (p.z - victim_pos.z).abs() > box_xz
                    || (p.y - victim_pos.y).abs() > box_y
                {
                    continue;
                }
                other.anger = Some(Anger {
                    end_time: now + grudge_ticks(&mut other.mob),
                    target: attacker_pos,
                });
            }
        }
        // before the removal below, so a killing blow is read for
        // its death sound rather than finding no mob.
        self.note_vocalisation(target_id, damage_dealt);
        let killed = health <= 0.0;
        if killed {
            // Through `reap_dead`, not a bare retain: a melee kill must drop the
            // same loot an explosion kill does. Health is already `0.0` here, so
            // the shared reaper picks exactly this mob out.
            self.reap_dead();
        }
        Some(AttackOutcome {
            health,
            killed,
            damage_dealt,
            velocity,
        })
    }

    /// How far a witnessing villager can be from a killed one and still record
    /// the death. The fixed radius uses the same squared-distance shape as
    /// [`GOSSIP_SPREAD_RADIUS_SQR`](Self::GOSSIP_SPREAD_RADIUS_SQR).
    const VILLAGER_KILLED_WITNESS_RADIUS_SQR: f64 = 100.0; // 10 blocks

    /// [`attack`](Self::attack), plus villager-reputation updates: a
    /// player-identified attacker hurting or killing a villager writes
    /// `VillagerHurt`/`VillagerKilled` gossip through
    /// [`villager::reputation::apply_reputation_event`].
    ///
    /// A separate method keeps `attack`'s signature focused on damage. The
    /// server attack path calls this method for player-attributed hits, while
    /// `attacker` is `None` when the swinging actor is unavailable; the gossip
    /// write is skipped and the damage behavior remains [`attack`](Self::attack).
    ///
    /// A hit's gossip is written to the **victim's own** ledger. A kill's gossip
    /// is written to **every nearby witnessing villager's own** ledger instead,
    /// because a killed victim is removed before the witness update.
    pub fn attack_from_player(
        &mut self,
        target_id: i32,
        attacker: Option<PlayerIdentity>,
        attacker_pos: Vec3,
        raw_damage: f32,
        flags: DamageFlags,
        knockback_power: f64,
    ) -> Option<AttackOutcome> {
        // Withers live in `self.withers`, separate from the ordinary mob map,
        // and use their own armor and emergence gates without mob anger,
        // gossip, or knockback state.
        if self.withers.contains_key(&target_id) {
            return self.attack_wither(target_id, raw_damage);
        }
        // Dragons likewise live in `self.dragons` and use the dedicated dragon
        // damage path.
        if self.dragons.contains_key(&target_id) {
            return self.attack_dragon(target_id, raw_damage);
        }
        // End crystals live in `self.crystals` and use a one-hit destruction
        // branch, so they do not enter the mob damage pipeline. Boats, rafts,
        // and minecarts live in `self.vehicles` and use their own damage
        // response without health, armor, knockback, or mob reputation state.
        if self.vehicles.contains_key(&target_id) {
            return self.attack_vehicle(target_id, raw_damage);
        }
        if self.crystals.contains_key(&target_id) {
            self.destroy_end_crystal(target_id)?;
            return Some(AttackOutcome {
                health: 0.0,
                killed: true,
                damage_dealt: raw_damage,
                velocity: Vec3::new(0.0, 0.0, 0.0),
            });
        }
        let target_was_villager = self
            .get(target_id)
            .is_some_and(|m| m.entity_type.path() == "villager");
        // Capture raid membership before `self.attack` mutates the target;
        // `raid_containing_raider` reads the live raider list, which is pruned
        // during the next raid tick.
        let target_raid_id = self.raid_containing_raider(target_id);
        let target_pos_before = self.get(target_id).map(SimMob::position);
        let outcome = self.attack(target_id, attacker_pos, raw_damage, flags, knockback_power)?;
        if let Some(actor) = attacker
            && outcome.killed
            && let Some(raid_id) = target_raid_id
        {
            self.add_raid_hero(raid_id, actor.uuid);
        }
        // Zombie-family reinforcement performs only its probability roll here;
        // the terrain search belongs to the spawn driver. It requires a
        // successful hit, hard difficulty, and the `spawn_mobs` rule. Reborrow
        // the target after the random draw because the RNG is a sibling field.
        if !outcome.killed && outcome.damage_dealt > 0.0 && self.spawn_hard_difficulty && self.spawn_monsters_enabled {
            let reinforcement_info = self.get(target_id).and_then(|mob| {
                matches!(
                    mob.entity_type.path(),
                    "zombie" | "husk" | "zombie_villager" | "drowned" | "zombified_piglin"
                )
                .then(|| {
                    (
                        mob.entity_type.clone(),
                        mob.position(),
                        mob.reinforcement_chance,
                        mob.attack_target_id,
                    )
                })
            });
            if let Some((entity_type, position, chance, own_target)) = reinforcement_info
                && self.reinforcement_rng.next_f32() < chance as f32
                // Vanilla's own reinforcement-target resolution: the
                // mob's own current attack target, falling back to whoever
                // just hit it (only if that attacker is a living entity).
                && let Some(reinforcement_target) = own_target.or_else(|| attacker.map(|a| a.entity_id))
            {
                if let Some(mob) = self.get_mut(target_id) {
                    mob.reinforcement_chance -= ZOMBIE_REINFORCEMENT_CALLER_CHARGE;
                }
                self.pending_reinforcements.push(ReinforcementCall {
                    position,
                    entity_type,
                    target_id: reinforcement_target,
                });
            }
        }
        // Owner-directed retaliation: a wolf (or any
        // tamed pet) joins whatever
        // fight its owner just started, reading the owner's own "last hurt
        // mob" field on
        // the *owner's* own living-entity state. This is the same field the
        // mob retaliation behavior reads, just recorded on the player instead
        // — see `NavigatingMob::set_owner_hurt_target`'s own
        // doc comment for the decay rule. Every tame pet owned by the
        // attacking player gets the target's pre-attack position (matching
        // the villager-witness resolution just below, which uses the same
        // `target_pos_before` for the identical reason: `target_id` may no
        // longer resolve to a live `SimMob` once `self.attack` has killed it).
        if let Some(actor) = attacker
            && let Some(pos) = target_pos_before
        {
            for pet in &mut self.mobs {
                if pet.owner_uuid() == Some(actor.uuid) && pet.is_tame() && pet.health() > 0.0 {
                    pet.mob.set_owner_hurt_target(Some(pos));
                }
            }
        }
        if let Some(actor) = attacker
            && target_was_villager
        {
            if outcome.killed {
                if let Some(pos) = target_pos_before {
                    for witness in &mut self.mobs {
                        if witness.entity_type.path() != "villager" {
                            continue;
                        }
                        let p = witness.position();
                        let dist_sqr =
                            (p.x - pos.x).powi(2) + (p.y - pos.y).powi(2) + (p.z - pos.z).powi(2);
                        if dist_sqr > Self::VILLAGER_KILLED_WITNESS_RADIUS_SQR {
                            continue;
                        }
                        villager::reputation::apply_reputation_event(
                            &mut witness.gossip,
                            villager::reputation::ReputationEventType::VillagerKilled,
                            actor.uuid,
                        );
                    }
                }
            } else if let Some(mob) = self.get_mut(target_id) {
                villager::reputation::apply_reputation_event(
                    &mut mob.gossip,
                    villager::reputation::ReputationEventType::VillagerHurt,
                    actor.uuid,
                );
            }
        }
        Some(outcome)
    }

    /// [`attack_from_player`](Self::attack_from_player)'s wither branch — a
    /// melee hit is never an arrow or a wind charge and never bypasses the
    /// emergence-invulnerability gate, so both of
    /// [`damage_wither`](Self::damage_wither)'s bool parameters are fixed
    /// `false`; `damage_wither` itself already applies the powered-armour
    /// and invulnerable-emergence refusals and removes the wither on a
    /// killing blow. A wither never moves (see `mobs::wither`'s own module
    /// doc), so the outcome's `velocity` is always zero rather than a
    /// knockback impulse.
    fn attack_wither(&mut self, target_id: i32, raw_damage: f32) -> Option<AttackOutcome> {
        let health = self.damage_wither(target_id, raw_damage, false, false)?;
        Some(AttackOutcome {
            health,
            killed: health <= 0.0,
            damage_dealt: raw_damage,
            velocity: Vec3::new(0.0, 0.0, 0.0),
        })
    }

    /// The number of live mobs.
    #[must_use]
    pub fn len(&self) -> usize {
        self.mobs.len()
    }

    /// Whether the simulation has no mobs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mobs.is_empty()
    }

    /// A mob by id, if present.
    #[must_use]
    pub fn get(&self, id: i32) -> Option<&SimMob<'w>> {
        self.mobs.iter().find(|m| m.id == id)
    }

    /// A mob by id, mutably, if present.
    pub fn get_mut(&mut self, id: i32) -> Option<&mut SimMob<'w>> {
        self.mobs.iter_mut().find(|m| m.id == id)
    }

    /// The **player entity id** riding `id`, if `id` names a mounted mob.
    ///
    /// The mob-mounted-by-player half of the passenger model — the twin of
    /// [`vehicle_rider`](Self::vehicle_rider) (boats) and the minecart
    /// equivalent in [`mobs::minecart`](crate::mobs::minecart), for a mount
    /// that keeps its own `SimMob` identity and goal AI rather than living in
    /// a separate AI-less map.
    #[must_use]
    pub fn mob_rider(&self, id: i32) -> Option<i32> {
        self.get(id).and_then(|m| m.rider)
    }

    /// The mob `player_entity_id` is riding, if any.
    #[must_use]
    pub fn mob_ridden_by(&self, player_entity_id: i32) -> Option<i32> {
        self.mobs
            .iter()
            .find(|m| m.rider == Some(player_entity_id))
            .map(|m| m.id)
    }

    /// Vanilla's own generic "start riding" call for a mounted mob — its own
    /// horse-family mount interaction's
    /// occupancy half. Mirrors [`mount_vehicle`](Self::mount_vehicle)'s shape
    /// exactly; the difference is what it operates on, a full [`SimMob`]
    /// rather than an AI-less [`TrackedVehicle`].
    ///
    /// Species-specific eligibility — tamed, not a baby, no sneak-click, a
    /// saddle if the species requires one — is deliberately **not** checked
    /// here, the same division `mount_vehicle` draws with
    /// `using_secondary_action`: those are `interact_horse`'s (or a future
    /// per-species interact arm's) job, because they differ by species and
    /// this method's only responsibility is the universal occupancy rule.
    ///
    /// Refuses when `id` is not a live mob or already carries a *different*
    /// rider. A player already riding something else — mob or vehicle — is
    /// left untouched here: unlike vehicles, this crate has exactly one
    /// producer of mob mounts today ([`MobSim::interact`]'s horse-family arm),
    /// so cross-kind dismount-first is the caller's job until a second
    /// producer exists, matching this method's own "one map's worry" scope.
    ///
    /// Returns `true` when the player is now aboard — the caller's cue to
    /// send `SET_PASSENGERS`.
    pub fn mount_mob(&mut self, id: i32, player_entity_id: i32) -> bool {
        let Some(mob) = self.get(id) else {
            return false;
        };
        if mob.rider.is_some_and(|rider| rider != player_entity_id) {
            return false;
        }
        if let Some(previous) = self.mob_ridden_by(player_entity_id) {
            if previous != id {
                if let Some(old) = self.get_mut(previous) {
                    old.rider = None;
                }
            }
        }
        if let Some(mob) = self.get_mut(id) {
            mob.rider = Some(player_entity_id);
        }
        true
    }

    /// Vanilla's own generic "stop riding" call for whatever mob `player_entity_id` is aboard,
    /// returning the mob it left. Called on an explicit dismount as well as
    /// on disconnect: a mount whose rider vanished must resume its own goal
    /// AI ([`tick`](Self::tick) skips a ridden mob's goal tick entirely — see
    /// that skip's own comment), or it stands frozen forever exactly as an
    /// unhealed boat would.
    pub fn dismount_mob(&mut self, player_entity_id: i32) -> Option<i32> {
        let id = self.mob_ridden_by(player_entity_id)?;
        if let Some(mob) = self.get_mut(id) {
            mob.rider = None;
        }
        Some(id)
    }

    /// Vanilla's own camel rider-jump handler/rider-jump executor — the rider-triggered half
    /// of camel dash. Called from `crate::server`'s `ServerBound::PlayerInput`
    /// consumer on every received `jump: true` (see that call site's own
    /// comment for why a received packet already is the rising edge).
    ///
    /// Refuses when `player_entity_id` rides no mob, the mob it rides is
    /// not a camel, or `SimMob::camel_dash_cooldown` has not yet reached
    /// zero (vanilla's own rider-jump handler's own cooldown-at-or-below-zero
    /// gate). Two of
    /// vanilla's three gates are not checked at all — see
    /// `ServerBound::PlayerInput`'s consumer for why (no saddle-equip
    /// model, no `onGround` for a client-authoritative mount).
    ///
    /// Sets `camel_dash_cooldown` to [`CAMEL_DASH_COOLDOWN_TICKS`], which
    /// both gates the next dash and — through
    /// [`SimMob::camel_is_dashing`] — makes the next [`snapshots`](Self::snapshots)
    /// diff carry the dash flag as `true` to every other connected viewer. The
    /// actual position impulse (vanilla's own rider-jump executor's velocity
    /// add) is not
    /// applied here: this crate has no server-side ridden-mob physics at
    /// all (`lodestone_physics::vehicle`'s module doc — a mounted camel is
    /// exactly as client-authoritative as a horse or a boat), so the visible
    /// leap itself is the rider's own client's job, not this seam's.
    ///
    /// Returns whether a dash actually started, so a caller that wants to
    /// know (a future sound/particle producer) can tell a real trigger from
    /// a no-op jump press.
    pub fn trigger_camel_dash(&mut self, player_entity_id: i32) -> bool {
        let Some(id) = self.mob_ridden_by(player_entity_id) else {
            return false;
        };
        let Some(mob) = self.get_mut(id) else {
            return false;
        };
        if mob.entity_type.path() != "camel" || mob.camel_dash_cooldown > 0 {
            return false;
        }
        mob.camel_dash_cooldown = CAMEL_DASH_COOLDOWN_TICKS;
        true
    }

    /// Accepts a client-authoritative move for the mob `player_entity_id` is
    /// riding — the mob-mount twin of
    /// [`apply_vehicle_move`](Self::apply_vehicle_move), and intended for the
    /// same `VehicleMoved` wire packet: vanilla's client is authoritative over
    /// its own ridden entity's position regardless of whether that entity is
    /// a boat or a horse (vanilla's own player-specific "is client
    /// authoritative" override does not
    /// distinguish them), so the two share one packet and should share one
    /// dispatch, trying this after (or instead of)
    /// [`apply_vehicle_move`](Self::apply_vehicle_move) depending on which one
    /// the rider is actually aboard.
    ///
    /// Returns `false` if the player rides no mob (including "rides a
    /// vehicle instead", which is not this method's map to touch).
    pub fn apply_mob_move(&mut self, player_entity_id: i32, position: Vec3, yaw: f32) -> bool {
        let Some(id) = self.mob_ridden_by(player_entity_id) else {
            return false;
        };
        let Some(mob) = self.get_mut(id) else {
            return false;
        };
        mob.mob.set_position(position).set_body_yaw(yaw);
        true
    }

    /// The world this sim's mobs path over. Exposed so a caller holding only
    /// a `&mut MobSim` (e.g. [`MobHandle::with`]) can still reach terrain —
    /// see [`seed_demo_mobs`]'s use of this to resolve spawn-surface Y
    /// without a second, separately-threaded `&ChunkWorld` parameter.
    #[must_use]
    pub(crate) fn world(&self) -> &'w ChunkWorld {
        self.world
    }

    /// The position of the mob with `id`, if present.
    #[must_use]
    pub fn position(&self, id: i32) -> Option<Vec3> {
        self.get(id).map(SimMob::position)
    }

    /// Iterates the live mobs.
    pub fn iter(&self) -> impl Iterator<Item = &SimMob<'w>> {
        self.mobs.iter()
    }

    /// Every mob and dropped item in this sim, as the records
    /// [`crate::entity_storage`] persists.
    ///
    /// # Why this is not [`snapshots`](Self::snapshots)
    ///
    /// [`EntitySnapshot`] is the *wire* view: it carries an `id` (a per-session
    /// entity id that means nothing across a restart), no health, and no item
    /// lifecycle. A save built from it would come back as full-health mobs and
    /// dropped items that never despawn. This is the disk view, and the two
    /// deliberately do not share a type.
    ///
    /// **Projectiles are excluded.** An arrow in flight has no persisted
    /// identity in this sim (`ProjectileMeta` carries a uuid and a type but the
    /// registry holds no owner, no pickup state and no damage), so writing one
    /// would persist an object we could not faithfully restore. Vanilla does
    /// save them; that is a follow-up, and it is named in `docs/entity-persistence.md`
    /// rather than left to be discovered as a missing mob.
    #[cfg(not(target_arch = "wasm32"))]
    #[must_use]
    pub fn saved_entities(&self) -> Vec<crate::entity_storage::SavedEntity> {
        let mut out: Vec<crate::entity_storage::SavedEntity> = self
            .mobs
            .iter()
            .map(|mob| crate::entity_storage::SavedEntity {
                id: mob.entity_type.clone(),
                uuid: mob.uuid,
                pos: mob.position(),
                motion: mob.velocity(),
                rotation: mob.rotation(),
                health: Some(mob.health),
                item: None,
                age: None,
                pickup_delay: None,
                extra: Vec::new(),
            })
            .collect();
        for (&id, state) in &self.item_state {
            let lifecycle = self.items.get(id).copied().unwrap_or_default();
            out.push(crate::entity_storage::SavedEntity {
                id: item_entity_type(),
                uuid: state.uuid,
                pos: state.motion.position,
                motion: state.motion.velocity,
                rotation: Rotation::new(0.0, 0.0),
                health: None,
                item: Some((state.item.clone(), lifecycle.count)),
                age: Some(lifecycle.age),
                pickup_delay: Some(lifecycle.pickup_delay),
                extra: Vec::new(),
            });
        }
        out
    }

    /// Puts saved records back into the sim, returning how many were restored.
    ///
    /// A record whose `id` is `minecraft:item` becomes a tracked dropped item;
    /// anything else becomes a mob through [`spawn_species`](Self::spawn_species),
    /// so a restored cow gets the same shape, attributes and A\* budget a freshly
    /// spawned one does — the alternative (a bare position) would restore mobs
    /// that cannot path.
    ///
    /// **The stored uuid is reinstated, not regenerated**, because
    /// [`crate::entity_storage::EntityStorage::save`] clears stale records by
    /// uuid identity: a fresh uuid on load would make the next save unable to
    /// recognise its own entity, and the mob would be duplicated on every
    /// restart.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn restore_saved(&mut self, entities: &[crate::entity_storage::SavedEntity]) -> usize {
        let mut restored = 0usize;
        for saved in entities {
            if saved.id == item_entity_type() {
                let Some((item, count)) = saved.item.clone() else {
                    // An `Item`-less item entity is vanilla's own "empty stack"
                    // case, which it discards on load too.
                    continue;
                };
                let id = self.spawn_item(
                    item,
                    saved.pos,
                    saved.motion,
                    ItemLifecycle {
                        age: saved.age.unwrap_or(0),
                        pickup_delay: saved.pickup_delay.unwrap_or(0),
                        count: count.max(1),
                        ..ItemLifecycle::default()
                    },
                );
                if let Some(state) = self.item_state.get_mut(&id) {
                    state.uuid = saved.uuid;
                }
                restored += 1;
                continue;
            }
            // Checked **before** spawning, not after: a stored `0.0` is a mob
            // that died in the tick the process was killed, and spawning it to
            // then skip it would leave a zero-health corpse in the sim that
            // nothing sweeps, because the death pass only runs on damage.
            if saved.health.is_some_and(|health| health <= 0.0) {
                continue;
            }
            let mob = self.spawn_species(saved.id.clone(), saved.pos);
            mob.uuid = saved.uuid;
            if let Some(health) = saved.health {
                mob.set_health(health);
            }
            restored += 1;
        }
        restored
    }

    /// Runs one despawn check over every non-persistent mob, given the nearest
    /// player's position (vanilla `getNearestPlayer(-1.0)`), removing mobs the
    /// two distance gates discard and resetting the age timer of any within the
    /// immune radius.
    ///
    /// `nearest_player` is `None` when no player is loaded, in which case vanilla
    /// runs no despawn logic at all — the mobs are simply kept. The `1/800`
    /// gate-B roll is drawn per candidate mob from `rng`, with one success in
    /// every 800 outcomes.
    ///
    /// Returns the number of mobs discarded.
    pub fn despawn_pass(&mut self, nearest_player: Option<Vec3>, rng: &mut SpawnRng) -> usize {
        let Some(player) = nearest_player else {
            return 0;
        };
        let before = self.mobs.len();
        self.mobs.retain_mut(|m| {
            if m.persistent {
                return true;
            }
            let dist_sqr = dist_sqr(m.mob.position(), player);
            let rng_hit_800 = rng.next_int(800) == 0;
            let outcome = check_despawn(m.category, dist_sqr, m.no_action_time, rng_hit_800, true);
            match outcome {
                DespawnOutcome { discard: true, .. } => false,
                DespawnOutcome {
                    reset_timer: true, ..
                } => {
                    m.no_action_time = 0;
                    true
                }
                _ => true,
            }
        });
        before - self.mobs.len()
    }

    /// Runs one natural-spawn cycle over `chunks`, respecting the per-category
    /// global caps in `state`.
    ///
    /// For each chunk and each spawnable category still under its cap, the
    /// [`SpawnCandidateSource`] is asked for the group it would spawn there; each
    /// member becomes a real mob through [`spawn_species`](Self::spawn_species),
    /// so it arrives with the species' own body, attributes and vanilla goal set
    /// rather than a placeholder. Nothing here decides *which* mob or *where* —
    /// that is the source's registry/terrain-dependent job.
    ///
    /// The **category is the spawn list's**, not
    /// [`spawn_species`](Self::spawn_species)' hostile/friendly guess: vanilla's
    /// category is a property of the `EntityType` registration, and the biome
    /// list's key is exactly that. It is overridden after the spawn for the same
    /// reason the cap is keyed by it.
    ///
    /// A group is truncated the moment its category reaches the cap, so the cap
    /// can never be exceeded even though the source drew a whole cluster.
    ///
    /// Returns the number of mobs spawned.
    pub fn run_spawn_cycle(
        &mut self,
        state: &mut SpawnState,
        source: &mut dyn SpawnCandidateSource,
        chunks: &[(i32, i32)],
    ) -> usize {
        let mut spawned = 0;
        for &(cx, cz) in chunks {
            for category in MobCategory::SPAWNING {
                if !state.can_spawn(category) {
                    continue;
                }
                for candidate in source.cluster(category, cx, cz) {
                    if !state.can_spawn(category) {
                        break;
                    }
                    let mob = self.spawn_species(candidate.entity_type, candidate.pos);
                    mob.set_category(category)
                        .set_persistent(category.is_persistent());
                    state.record(category);
                    spawned += 1;
                }
            }
        }
        spawned
    }

    /// Builds a [`SpawnState`] for `spawnable_chunks` from a census of the mobs
    /// currently alive, exactly as vanilla rebuilds `SpawnState` each cycle.
    #[must_use]
    pub fn census(&self, spawnable_chunks: i32) -> SpawnState {
        let mut state = SpawnState::new(spawnable_chunks);
        for m in &self.mobs {
            state.record(m.category);
        }
        state
    }

    /// Runs one patrol-spawn tick — vanilla's own patrol-spawner port
    /// (a 92-line generic custom-spawner). Meant to be called
    /// once per server tick, mirroring vanilla's own generic custom-spawner
    /// update: the
    /// internal countdown is decremented every call regardless of whether
    /// anything ends up spawning, so calling this less often than once a
    /// tick would make patrols rarer than vanilla rather than merely
    /// checked less often.
    ///
    /// `world` is the terrain a spawn candidate is checked against, and it
    /// must be a *live, player-following* snapshot — not this sim's own
    /// static `self.world` — because a patrol spawns near a **player**, and
    /// `self.world` is a fixed footprint around wherever `mob_area` was when
    /// the world opened. Feeding `self.world` here would reproduce the exact
    /// bug natural spawning already had and fixed: patrols would work near
    /// spawn and stop working entirely once a player walked away from it.
    /// The caller should hand in the same snapshot `crate::natural_spawn`
    /// already builds each cycle.
    ///
    /// `spawn_patrols` is the game rule of the same name; `is_bright_outside`
    /// is vanilla's own "is bright outside" check — day and not thundering
    /// — collapsed to a caller-supplied bool because no weather state crosses
    /// this seam yet.
    ///
    /// Returns the number of pillagers actually spawned this call (`0` on
    /// almost every call — vanilla's own interval is roughly once every
    /// 12000–13200 ticks per world, i.e. every 10–11 minutes).
    ///
    /// # Disclosed, not modelled
    ///
    /// `docs/pillager-patrols.md` has the full account; the summary:
    ///
    /// * **No spectator filter and no village-proximity check.** Neither a
    ///   spectator flag nor a POI/village census exists on this seam.
    /// * **No block-light check** (vanilla's own "check patrolling monster
    ///   spawn rules" step's own
    ///   block-brightness-above-8 test). [`ChunkWorld`] carries
    ///   block *identity*, not light — the same limit `natural_spawn`'s
    ///   caller-supplied light cache exists to work around for the mobs that
    ///   need it, which this method does not have access to.
    /// * **Vanilla's own "is valid empty spawn block" check is approximated** as "two blocks of open
    ///   air above the surface", with no fluid-state check.
    /// * [`patrol_group_size`] approximates vanilla's own
    ///   "current-difficulty-at-position, effective difficulty" formula, a continuous formula this crate has no
    ///   moon-phase or accumulated regional-difficulty state to compute, with
    ///   a per-[`Difficulty`]-enum constant.
    pub fn run_patrol_spawn_cycle(
        &mut self,
        world: &ChunkWorld,
        spawn_patrols: bool,
        is_bright_outside: bool,
        difficulty: Difficulty,
    ) -> usize {
        self.patrol_next_tick -= 1;
        if self.patrol_next_tick > 0 {
            return 0;
        }
        // Vanilla re-arms the countdown *before* any of the gates below can
        // reject the attempt (`this.nextTick = this.nextTick + 12000 +
        // random.nextInt(1200)`, unconditionally, immediately after the `<=
        // 0` check) — so a rejected attempt still waits a full interval
        // before the next one, rather than retrying every tick.
        self.patrol_next_tick += 12_000 + self.patrol_rng.next_int(1_200);
        if !spawn_patrols || !is_bright_outside {
            return 0;
        }
        if self.tick_count < PATROL_TIMELINE_GATE {
            return 0;
        }
        if self.patrol_rng.next_int(5) != 0 {
            return 0;
        }
        if self.players.is_empty() {
            return 0;
        }
        let player_pos = self.players
            [self.patrol_rng.next_int(self.players.len() as i32) as usize]
            .perception
            .position;

        let sign_x = if self.patrol_rng.next_int(2) == 0 {
            -1.0
        } else {
            1.0
        };
        let sign_z = if self.patrol_rng.next_int(2) == 0 {
            -1.0
        } else {
            1.0
        };
        let dx = f64::from(24 + self.patrol_rng.next_int(24)) * sign_x;
        let dz = f64::from(24 + self.patrol_rng.next_int(24)) * sign_z;
        let mut spawn_x = (player_pos.x + dx).floor() as i32;
        let mut spawn_z = (player_pos.z + dz).floor() as i32;

        let group_size = patrol_group_size(difficulty);
        let pillager: ResourceKey = "minecraft:pillager".parse().expect("valid key");
        let mut spawned = 0;
        for i in 0..group_size {
            // Vanilla's own natural-spawner "is valid empty spawn block"
            // check + this method's own
            // "not modelled" note: a surface exists and there are two open
            // cells above it. `None`/`false` both mean "no valid cell here".
            let spawn_ok = surface_y(world, spawn_x, spawn_z).filter(|&surface| {
                !world.is_solid(spawn_x, surface + 1, spawn_z)
                    && !world.is_solid(spawn_x, surface + 2, spawn_z)
            });
            if let Some(surface) = spawn_ok {
                let pos = Vec3::new(
                    f64::from(spawn_x) + 0.5,
                    f64::from(surface + 1),
                    f64::from(spawn_z) + 0.5,
                );
                let mob = self.spawn_species(pillager.clone(), pos);
                let id = mob.id;
                mob.set_category(MobCategory::Monster);
                self.get_mut(id)
                    .expect("just spawned")
                    .set_patrolling(true);
                if i == 0 {
                    // Vanilla's own "find patrol target" step: `-500 + nextInt(1000)` on both
                    // axes, offset from the mob's *own* spawn position.
                    let tx = f64::from(self.patrol_rng.next_int(1_000) - 500);
                    let tz = f64::from(self.patrol_rng.next_int(1_000) - 500);
                    let leader = self.get_mut(id).expect("just spawned");
                    leader.set_patrol_leader(true);
                    leader.set_patrol_target(Some(Vec3::new(pos.x + tx, pos.y, pos.z + tz)));
                }
                spawned += 1;
            } else if i == 0 {
                // The leader's own spawn attempt failed — vanilla abandons
                // the whole group rather than trying a different member
                // first (its own patrol-spawner's own early-break).
                break;
            }
            spawn_x += self.patrol_rng.next_int(5) - self.patrol_rng.next_int(5);
            spawn_z += self.patrol_rng.next_int(5) - self.patrol_rng.next_int(5);
        }
        // A follower spawned this same call has no group target until
        // `feed_perception` next runs its patrol census — a one-tick startup
        // lag, not a correctness gap: `LongDistancePatrolGoal::can_use`
        // requires `patrol_target().is_some()`, so it simply does not fire
        // until then.
        spawned
    }

    /// Runs the wandering-trader spawn cycle against the live, player-following
    /// `world` snapshot. The caller supplies the `spawn_wandering_traders` rule,
    /// while this simulation owns the cycle counters and needs one call per tick.
    ///
    /// Cycle counters are session state; this simulation does not persist them.
    /// Spawn selection uses a random player's position and omits point-of-interest
    /// search, biome exclusion, collision checks, and post-spawn home/despawn
    /// fields because those inputs are not part of this simulation's state.
    ///
    /// Returns the trader's entity id on a successful spawn.
    pub fn run_wandering_trader_spawn_cycle(
        &mut self,
        world: &ChunkWorld,
        spawn_wandering_traders: bool,
    ) -> Option<i32> {
        self.trader_tick_delay -= 1;
        if self.trader_tick_delay > 0 {
            return None;
        }
        self.trader_tick_delay = WANDERING_TRADER_TICK_DELAY;
        if !spawn_wandering_traders {
            return None;
        }
        self.trader_spawn_delay -= WANDERING_TRADER_TICK_DELAY;
        if self.trader_spawn_delay > 0 {
            return None;
        }
        self.trader_spawn_delay = WANDERING_TRADER_SPAWN_DELAY;
        let chance = self.trader_spawn_chance;
        self.trader_spawn_chance = (self.trader_spawn_chance
            + WANDERING_TRADER_SPAWN_CHANCE_INCREASE)
            .min(WANDERING_TRADER_MAX_SPAWN_CHANCE);
        // `random.nextInt(100) <= chanceToSpawn` is the entry condition;
        // missing it draws nothing further and leaves the climbed chance
        // in place for next time.
        if self.trader_rng.next_int(100) > chance {
            return None;
        }
        if self.players.is_empty() {
            // Vanilla's own spawn call's "no random player found" arm returns
            // `true` — a "success" for chance-reset purposes — without
            // drawing further or spawning anything.
            self.trader_spawn_chance = WANDERING_TRADER_MIN_SPAWN_CHANCE;
            return None;
        }
        // Vanilla's own spawn call's own extra one-in-ten gate, drawn only
        // once a player
        // exists — vanilla's short-circuit on "no player found" above skips
        // this draw entirely, which is why the empty-players check has to
        // come first rather than being folded into a single `if`.
        if self.trader_rng.next_int(10) != 0 {
            return None;
        }
        let player_pos = self.players
            [self.trader_rng.next_int(self.players.len() as i32) as usize]
            .perception
            .position;
        let reference = (player_pos.x.floor() as i32, player_pos.z.floor() as i32);
        // Vanilla's own "find spawn position near" step: up to 10 candidates
        // within a 48-block
        // radius, first one with a real surface wins. No "is spawn position
        // ok" check beyond "a column exists here" — see the gaps disclosed
        // above.
        let mut spawn_pos = None;
        for _ in 0..10 {
            let x = reference.0 + self.trader_rng.next_int(96) - 48;
            let z = reference.1 + self.trader_rng.next_int(96) - 48;
            if let Some(surface) = surface_y(world, x, z) {
                spawn_pos = Some(Vec3::new(
                    f64::from(x) + 0.5,
                    f64::from(surface + 1),
                    f64::from(z) + 0.5,
                ));
                break;
            }
        }
        let pos = spawn_pos?;
        let (trader_id, _llamas) = self.spawn_wandering_trader(pos);
        self.trader_spawn_chance = WANDERING_TRADER_MIN_SPAWN_CHANCE;
        Some(trader_id)
    }

    /// Whether a death rolls its loot table — the `mob_drops` game rule, handed in
    /// by `crate::tick::run_tick_loop` once a tick (this type is version-free and
    /// holds no world-state handle). Defaults to vanilla's own default, `true`, so a
    /// sim nobody sets it on behaves exactly as before the rule existed.
    pub fn set_mob_drops(&mut self, allowed: bool) {
        self.mob_drops = allowed;
    }

    /// Discards every mob Peaceful forbids — vanilla's own "check despawn" guard,
    /// peaceful difficulty and not allowed-in-peaceful for the type. Returns how
    /// many were removed.
    ///
    /// Rolls **no** loot: vanilla's peaceful sweep is `discard()`, not a death, so a
    /// player switching to Peaceful does not get a floor covered in rotten flesh.
    ///
    /// **The predicate is the per-type `notInPeaceful` flag
    /// ([`crate::mob_spawn::allowed_in_peaceful`]), not
    /// [`is_hostile_species`].** The two disagree in both directions and the
    /// disagreement is visible: `is_hostile_species` is a 22-name list serving the
    /// *category* question, so it kept a slime, magma cube, silverfish, phantom,
    /// vex, ravager, hoglin or warden alive on Peaceful — and slimes really do
    /// spawn here, because `crate::natural_spawn` models slime chunks. In the other
    /// direction the flag keeps a shulker and a piglin, which vanilla also keeps
    /// and which a monster-category test would delete.
    pub fn remove_monsters(&mut self) -> usize {
        let before = self.mobs.len();
        self.mobs
            .retain(|m| crate::mob_spawn::allowed_in_peaceful(m.entity_type.path()));
        before - self.mobs.len()
    }

    /// Removes every mob at or below zero health, rolling its death loot table
    /// on the way out (the mob tick's death-loot chain).
    ///
    /// This is the central mob-removal path. Each dead mob contributes the
    /// loot table selected by [`crate::block_drops::mob_loot_table_id`], and
    /// each resulting stack becomes
    /// an item entity at the mob's position with the configured drop velocity.
    ///
    /// Rolls in the **empty** loot context, so `killed_by_player` is `false` and
    /// `enchanted_count_increase` (looting) contributes nothing: rare drops gated
    /// on a player kill do not appear. That is honest rather than approximated —
    /// the context has no attacker field to fill (see [`crate::loot`]).
    fn reap_dead(&mut self) {
        let now = self.tick_count;
        // `drops_experience` is vanilla's own drop-experience call's own guard, read here
        // while the mob still exists: a player's hit within the last
        // `PLAYER_HURT_EXPERIENCE_TIME` ticks, and not a baby
        // (its own "should drop experience" check is "not a baby").
        //
        // `drops_ominous_bottle` is vanilla's own "captain without raid"
        // raider predicate
        // (`hasRaid=false, isCaptain=true`) — see
        // [`drop_ominous_bottle`](Self::drop_ominous_bottle)'s own doc for
        // why it is resolved here rather than through
        // [`drop_death_loot`](Self::drop_death_loot)'s generic table roll.
        // `raid_containing_raider` reads `self.raids` only, so calling it
        // from inside this `self.mobs.iter()` closure borrows disjointly —
        // both borrows are shared, so nothing here needs deferring the way
        // the mutable passes below do.
        let dead: Vec<(i32, ResourceKey, Vec3, bool, bool)> = self
            .mobs
            .iter()
            .filter(|m| m.health <= 0.0)
            .map(|m| {
                let by_player = m.hurt_by_player_until.is_some_and(|until| now < until);
                let drops_ominous_bottle = m.entity_type.path() == "pillager"
                    && m.is_patrol_leader()
                    && self.raid_containing_raider(m.id).is_none();
                (
                    m.id,
                    m.entity_type.clone(),
                    m.position(),
                    by_player && !m.is_baby(),
                    drops_ominous_bottle,
                )
            })
            .collect();
        if dead.is_empty() {
            return;
        }
        self.mobs.retain(|m| m.health > 0.0);
        for (id, entity_type, position, drops_experience, drops_ominous_bottle) in dead {
            self.drop_death_loot(&entity_type, position);
            if drops_ominous_bottle {
                self.drop_ominous_bottle(position);
            }
            // Drop ordinary death loot before experience, so the two output
            // streams retain their stable ordering.
            if drops_experience {
                self.drop_death_experience(&entity_type, position);
            }
            // A death posts an entity-die event at the dying mob's position,
            // carrying that mob's id as the source. Other event producers are
            // posted by their owning systems.
            self.post_vibration(position, VibrationEvent::EntityDie, Some(id));
        }
    }

    /// Posts one vibration for a producer. The optional `source` identifies
    /// the entity responsible when the producer has one; see
    /// [`PostedVibration::source`]'s own doc.
    pub fn post_vibration(&mut self, position: Vec3, event: VibrationEvent, source: Option<i32>) {
        self.posted_vibrations.push(PostedVibration { position, event, source });
    }

    /// Resolves this tick's nearest-vibration answer for every listener
    /// species, then drains the posted log back to empty. Runs at the *end*
    /// of the tick, deliberately not inside
    /// [`feed_perception`](Self::feed_perception) (which runs before
    /// [`reap_dead`](Self::reap_dead) posts anything): a death this same
    /// tick must be audible this same tick, not one tick late — the same
    /// reasoning [`tick_orbs`](Self::tick_orbs) already gives for reading
    /// `tick_count` before its own increment.
    fn resolve_vibrations(&mut self) {
        let posted = std::mem::take(&mut self.posted_vibrations);
        for mob in &mut self.mobs {
            mob.nearest_vibration = if is_vibration_listener(mob.entity_type.path()) {
                nearest_listenable(mob.position(), WARDEN_LISTENER_RADIUS, &posted)
            } else {
                None
            };
            // The allay note-block consumer: an allay within
            // `ALLAY_LISTENER_RADIUS` of a `NoteBlockPlay` this tick either
            // adopts it (when no liked note block exists) or refreshes its
            // cooldown for the same position heard again; a different position
            // while one is already liked is
            // ignored.
            if mob.entity_type.path() == "allay"
                && let Some(heard) =
                    nearest_note_block_play(mob.position(), ALLAY_LISTENER_RADIUS, &posted)
            {
                match mob.allay_liked_noteblock {
                    Some((pos, _)) if pos == heard.position => {
                        mob.allay_liked_noteblock = Some((pos, ALLAY_NOTEBLOCK_COOLDOWN_TICKS));
                    }
                    None => {
                        mob.allay_liked_noteblock =
                            Some((heard.position, ALLAY_NOTEBLOCK_COOLDOWN_TICKS));
                    }
                    Some(_) => {}
                }
            }
        }
    }

    /// Vanilla's own "pick up item" inventory-carrier helper /
    /// allay-specific "wants to pick up" check: a
    /// held-item allay with inventory room absorbs the nearest matching
    /// dropped item within [`ALLAY_ITEM_PICKUP_RADIUS`], the ground half of
    /// this crate's own [`ALLAY_ITEM_PICKUP_RADIUS`] doc-disclosed
    /// bounding-box substitution. Two passes for the same borrow-checker
    /// reason [`feed_perception`](Self::feed_perception)'s own doc gives:
    /// deciding what to pick up reads `self.item_state` while mutating
    /// `self.mobs` would need it held mutably too.
    ///
    /// **Disclosed narrowing**: this simulation has no access to the shared
    /// block-mutation rule at this seam, so every eligible allay picks up.
    fn allay_pick_up_items(&mut self) {
        struct Candidate {
            mob_index: usize,
            position: Vec3,
            held_item: String,
            room: u32,
        }
        let candidates: Vec<Candidate> = self
            .mobs
            .iter()
            .enumerate()
            .filter_map(|(mob_index, m)| {
                if m.entity_type.path() != "allay" || m.health <= 0.0 {
                    return None;
                }
                let held_item = m.mob.main_hand_item()?.to_owned();
                let room = ALLAY_INVENTORY_MAX.saturating_sub(m.allay_inventory_count);
                if room == 0 {
                    return None;
                }
                Some(Candidate { mob_index, position: m.position(), held_item, room })
            })
            .collect();

        let radius_sq = ALLAY_ITEM_PICKUP_RADIUS * ALLAY_ITEM_PICKUP_RADIUS;
        for candidate in candidates {
            let hit = self
                .item_state
                .iter()
                .filter(|(_, state)| {
                    state.item.path() == candidate.held_item
                        && dist_sqr(state.motion.position, candidate.position) <= radius_sq
                })
                .min_by(|a, b| {
                    dist_sqr(a.1.motion.position, candidate.position)
                        .total_cmp(&dist_sqr(b.1.motion.position, candidate.position))
                })
                .map(|(&id, _)| id);
            let Some(id) = hit else { continue };
            let stack = u32::from(self.items.get(id).map_or(0, |l| l.count));
            let take = stack.min(candidate.room);
            if take == 0 {
                continue;
            }
            let remaining = stack - take;
            if remaining == 0 {
                self.remove_item(id);
            } else {
                self.set_item_count(id, u8::try_from(remaining).unwrap_or(u8::MAX));
            }
            self.mobs[candidate.mob_index].allay_inventory_count += take;
        }
    }

    /// Allay item delivery: a carrying allay within
    /// [`ALLAY_DELIVER_ARRIVAL_DISTANCE`] of its liked note-block's `.above()`
    /// cell throws one item from its inventory there per tick — a real dropped
    /// [`ItemEntity`](lodestone_entity::item_entity), not a state flag, so a
    /// player can actually walk over and collect it. Throws use a 20-tick
    /// cadence with a small random velocity; this model drains one item per
    /// tick and does not model velocity spread.
    ///
    /// **Not delivered to a liked player as a fallback** because no player
    /// delivery target is available in this simulation seam.
    fn allay_deliver_items(&mut self) {
        struct Delivery {
            mob_index: usize,
            drop_position: Vec3,
        }
        let deliveries: Vec<Delivery> = self
            .mobs
            .iter()
            .enumerate()
            .filter_map(|(mob_index, m)| {
                if m.entity_type.path() != "allay" || m.health <= 0.0 || m.allay_inventory_count == 0
                {
                    return None;
                }
                let (liked_pos, ticks) = m.allay_liked_noteblock?;
                if ticks <= 0 {
                    return None;
                }
                let above = Vec3::new(liked_pos.x, liked_pos.y + 1.0, liked_pos.z);
                if dist_sqr(m.position(), above) > ALLAY_DELIVER_ARRIVAL_DISTANCE * ALLAY_DELIVER_ARRIVAL_DISTANCE {
                    return None;
                }
                Some(Delivery { mob_index, drop_position: above })
            })
            .collect();

        for delivery in deliveries {
            let Some(item) = self.mobs[delivery.mob_index].mob.main_hand_item() else {
                continue;
            };
            let Ok(item) = ResourceKey::from_str(&format!("minecraft:{item}")) else {
                continue;
            };
            self.spawn_item(
                item,
                delivery.drop_position,
                Vec3::new(0.0, 0.0, 0.0),
                ItemLifecycle::newly_dropped(1, lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE),
            );
            self.mobs[delivery.mob_index].allay_inventory_count -= 1;
        }
    }

    /// Vanilla's own generic drop-experience call: pops this species' reward as orbs at `position`.
    ///
    /// The caller has already applied vanilla's two eligibility tests (see
    /// [`reap_dead`](Self::reap_dead)); this applies the third,
    /// `level.getGameRules().get(GameRules.MOB_DROPS)`, which is the same rule
    /// [`drop_death_loot`](Self::drop_death_loot) honours — so `/gamerule mobDrops
    /// false` suppresses XP as well as items, exactly as vanilla does.
    ///
    /// The reward roll rides [`orb_rng`](Self::orb_rng) rather than a position-seeded
    /// stream: unlike a loot roll, an animal's `1 + nextInt(3)` has no reason to be
    /// reproducible from the death site, and putting it on the orb stream keeps every
    /// orb-related draw in one sequence.
    fn drop_death_experience(&mut self, entity_type: &ResourceKey, position: Vec3) {
        if !self.mob_drops {
            return;
        }
        let reward = species::mob_experience_reward(entity_type, &mut self.orb_rng);
        if reward <= 0 {
            return;
        }
        self.award_experience(position, Vec3::new(0.0, 0.0, 0.0), reward);
    }

    /// Rolls `entity_type`'s death loot table and spawns the result at
    /// `position`. See [`reap_dead`](Self::reap_dead) for the vanilla chain.
    ///
    /// Seeded from the tick count and the position, so a death is deterministic
    /// for a given world state without threading a connection's RNG into the sim.
    fn drop_death_loot(&mut self, entity_type: &ResourceKey, position: Vec3) {
        if !self.mob_drops {
            return;
        }
        let Some(table) = crate::block_drops::mob_loot_table_id(entity_type) else {
            return;
        };
        let tables = crate::block_drops::bundled_tables();
        if tables.get(&table).is_none() {
            return;
        }
        let mut rng = SpawnRng::new(
            (self.tick_count as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (position.x.to_bits() ^ position.z.to_bits().rotate_left(31)),
        );
        let rolled = tables.roll(&table, &crate::loot::LootContext::default(), &mut rng);
        for stack in rolled {
            if stack.count == 0 {
                continue;
            }
            let velocity = crate::block_drops::dropped_item_velocity(&mut rng);
            let count = u8::try_from(stack.count).unwrap_or(u8::MAX);
            self.spawn_item(
                stack.item.clone(),
                position,
                velocity,
                ItemLifecycle::newly_dropped(
                    count,
                    lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                ),
            );
        }
    }

    /// The ominous bottle item off a pillager patrol captain's death —
    /// vanilla's `entities/pillager.json` loot pool, gated on
    /// vanilla's own "captain without raid" raider predicate (`hasRaid=false,
    /// isCaptain=true`): a patrol leader ([`SimMob::is_patrol_leader`]) not
    /// currently a member of any active raid
    /// ([`raid::MobSim::raid_containing_raider`]). [`reap_dead`](Self::reap_dead)
    /// resolves the gate (it needs both a live patrol-leader flag and a raid
    /// census, neither available inside a loot roll) and calls this only
    /// when it holds.
    ///
    /// **Not routed through [`drop_death_loot`](Self::drop_death_loot)'s
    /// generic bundled-loot-table engine.** `crate::loot`'s own
    /// `entity_properties` condition is context-blind — `LootContext` carries
    /// no entity data at all (see that module's own doc) — so a bundled
    /// `entities/pillager.json` would silently roll `false` on exactly the
    /// gate this drop needs: the identical hole `block_state_property` was
    /// before `LootContext::block_state` existed. A dedicated call site,
    /// [`drop_death_experience`](Self::drop_death_experience)'s own shape,
    /// until entity-conditioned loot context lands generically.
    ///
    /// **Disclosed narrowing**: vanilla rolls a uniform `0..=4` amplifier
    /// onto the bottle's own `minecraft:ominous_bottle_amplifier` component
    /// (its own "set ominous bottle amplifier" loot function); every bottle dropped here is
    /// amplifier `0` instead of the real roll, because persisting a
    /// per-stack amplifier needs a new field on
    /// `lodestone_model::ItemComponents`, which this session's ownership
    /// does not reach (`crates/lodestone-model/**` — see
    /// `docs/raids-and-patrols.md` §5 for the exact hunk).
    /// `crate::server::finish_drinking_ominous_bottle` is the consumer this
    /// feeds; amplifier `0` is still a real, working value there —
    /// `raid::absorb_raid_omen(0, 0) == 1` starts a genuine raid — so this is
    /// "always the weakest roll", not "does nothing".
    fn drop_ominous_bottle(&mut self, position: Vec3) {
        if !self.mob_drops {
            return;
        }
        let bottle: ResourceKey = "minecraft:ominous_bottle".parse().expect("a literal item id is always valid");
        let mut rng = SpawnRng::new(
            (self.tick_count as u64)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                ^ (position.x.to_bits() ^ position.z.to_bits().rotate_left(31)),
        );
        let velocity = crate::block_drops::dropped_item_velocity(&mut rng);
        self.spawn_item(
            bottle,
            position,
            velocity,
            ItemLifecycle::newly_dropped(1, lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE),
        );
    }

    /// Vanilla's own environment-attribute "cat waking-up gift chance" value
    /// at `day_time` —
    /// hand-transcribed from its one modifier track
    /// (its own timelines table's cat-waking-up-gift-chance row: a maximum
    /// float modifier,
    /// constant easing, keyframes `0.0F` at tick 362 and `0.7F` at tick
    /// 23667 within the 24000-tick day cycle) rather than read from a general
    /// timeline engine — this crate has no environment-attribute/timeline
    /// reader at all, the same disclosed gap
    /// [`natural_spawn::surface_slime_spawn_chance`](crate::natural_spawn)'s own
    /// doc names for the moon-phase slime chance, and building one for a
    /// single step function would be out of proportion to what it buys.
    ///
    /// A `CONSTANT` easing is a step function: the attribute holds `0.0` from
    /// tick 362 up to (not including) 23667, and `0.7` from 23667 wrapping
    /// through midnight back to 362 — so a cat's gift only has a real chance
    /// to land in the pre-dawn stretch of the night, which is when a player
    /// who slept through to morning actually wakes.
    fn cat_gift_chance(day_time: i32) -> f32 {
        let t = day_time.rem_euclid(24_000);
        if !(362..23_667).contains(&t) { 0.7 } else { 0.0 }
    }

    /// Resolves every cat morning-gift request
    /// recorded this tick: rolls [`Self::cat_gift_chance`] at
    /// the current [`MobSim::day_time`], and on success rolls
    /// `gameplay/cat_morning_gift` and spawns the result at the cat's own
    /// position — the same loot-table-then-`spawn_item` shape
    /// [`drop_death_loot`](Self::drop_death_loot) already uses.
    ///
    /// **Disclosed simplification**: no random relocation occurs before the
    /// drop. The item spawns at the cat's current position.
    fn resolve_cat_gifts(&mut self, gift_requests: Vec<i32>) {
        if gift_requests.is_empty() {
            return;
        }
        let chance = Self::cat_gift_chance(self.day_time);
        let table = ResourceKey::new("minecraft", "gameplay/cat_morning_gift")
            .expect("a static loot-table key parses");
        let tables = crate::block_drops::bundled_tables();
        for id in gift_requests {
            let Some(pos) = self.get(id).map(SimMob::position) else {
                continue;
            };
            let mut rng = SpawnRng::new(
                (self.tick_count as u64)
                    .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                    ^ (pos.x.to_bits() ^ pos.z.to_bits().rotate_left(31))
                    ^ (id as u64),
            );
            if rng.next_f32() >= chance {
                continue;
            }
            let rolled = tables.roll(&table, &crate::loot::LootContext::default(), &mut rng);
            for stack in rolled {
                if stack.count == 0 {
                    continue;
                }
                let velocity = crate::block_drops::dropped_item_velocity(&mut rng);
                let count = u8::try_from(stack.count).unwrap_or(u8::MAX);
                self.spawn_item(
                    stack.item.clone(),
                    pos,
                    velocity,
                    ItemLifecycle::newly_dropped(
                        count,
                        lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
                    ),
                );
            }
        }
    }

    /// Resolves every shoulder-mount request recorded this tick. This crate has
    /// no per-player NBT inventory, so [`SimMob::owner`] resolves to a UUID
    /// plus [`self.shoulder_riders`](Self::shoulder_riders) — one slot per
    /// owner — is the stand-in: the
    /// parrot mob is removed the same way [`Self::despawn_pass`] removes any
    /// other mob, and [`Self::tick_shoulder_dismounts`] is what brings it
    /// back.
    ///
    fn resolve_shoulder_mounts(&mut self, shoulder_requests: Vec<i32>) {
        for id in shoulder_requests {
            let Some(m) = self.get(id) else { continue };
            let Some(MobOwner::Player(uuid)) = m.owner else {
                continue;
            };
            if self.shoulder_riders.contains_key(&uuid) {
                // A slot is already taken — vanilla tries the second
                // shoulder here; this crate models one slot per owner (see
                // this method's own doc), so a second parrot simply fails to
                // mount and stays in the world, exactly as a vanilla parrot
                // does once both shoulders are full.
                continue;
            }
            self.shoulder_riders.insert(
                uuid,
                ShoulderRider {
                    entity_type: m.entity_type().clone(),
                    mounted_tick: self.tick_count,
                },
            );
            self.mobs.retain(|m| m.id != id);
        }
    }

    /// Dismounts every shoulder rider whose owner meets a dismount condition,
    /// respawning the mob at the owner's position — vanilla's own
    /// player-side "remove entities on shoulder"/"respawn entity on shoulder"
    /// calls,
    /// gated the same way on `mounted_tick + 20 <
    /// gameTime` so a parrot cannot fall off the instant it lands.
    ///
    /// **Disclosed simplification**: vanilla's own "handle shoulder entities" step fires
    /// on five conditions (`fallDistance > 0.5`, in water, flying, sleeping,
    /// in powder snow) — this models only **sleeping**, because it is the
    /// only one of the five this sim already tracks for an owner
    /// ([`Self::sleeping_players`], built for [`Self::resolve_cat_gifts`]'s
    /// own owner-sleep feed). The other four need per-player physical state
    /// (fall distance, ability flags, block-at-feet) this crate's player
    /// census does not carry.
    fn tick_shoulder_dismounts(&mut self) {
        if self.shoulder_riders.is_empty() {
            return;
        }
        let sleeping: std::collections::HashSet<i32> =
            self.sleeping_players.iter().map(|&(id, _)| id).collect();
        let sleeping_owners: Vec<Uuid> = self
            .players
            .iter()
            .filter_map(|p| {
                let identity = p.identity?;
                sleeping.contains(&identity.entity_id).then_some(identity.uuid)
            })
            .collect();
        let tick_count = self.tick_count;
        let mut to_dismount = Vec::new();
        for (&uuid, rider) in &self.shoulder_riders {
            if tick_count < rider.mounted_tick + 20 {
                continue;
            }
            if sleeping_owners.contains(&uuid) {
                to_dismount.push(uuid);
            }
        }
        for uuid in to_dismount {
            let Some(rider) = self.shoulder_riders.remove(&uuid) else {
                continue;
            };
            let Some(owner_pos) = self.player_position(uuid) else {
                // Owner disconnected while carrying the parrot — drop the
                // rider entirely rather than respawn it at a stale position;
                // there is no live position to respawn at.
                continue;
            };
            self.spawn_species(rider.entity_type, owner_pos)
                .tame(MobOwner::Player(uuid))
                .set_shoulder_dismount_ticks(0);
        }
    }

    /// Every live entity this sim owns — mobs, projectiles, dropped items —
    /// lowered to the wire-facing [`EntitySnapshot`] the encode seam needs.
    ///
    /// This is the merged sibling of iterating [`iter`](Self::iter) alone:
    /// [`crate::tick::run_tick_loop`] (previously [`run_mob_tick_loop`])
    /// publishes this (not just the mobs) to [`LiveMobSource`], which is what
    /// actually gets a spawned projectile or
    /// dropped item onto the same `add_entity`/`move_entity`/`remove_entity`
    /// wire path mobs already proved reaches a real client
    /// (`entity_streaming_live.rs`) — without this, ticking the registries
    /// above would still be a closed loop that reaches zero pixels.
    #[must_use]
    pub fn snapshots(&self) -> Vec<EntitySnapshot> {
        let mut out: Vec<EntitySnapshot> = self
            .mobs
            .iter()
            .map(|m| {
                let mut snap = m.snapshot();
                // Only `MobSim` can resolve a `LeashHolder` to a wire entity id
                // (a player holder needs `self.players`) — see
                // `resolve_leash_target`'s own doc for the three shapes.
                snap.leash_link = m.leash_holder().and_then(|holder| self.resolve_leash_target(holder));
                snap
            })
            .collect();
        for t in self.projectiles.iter() {
            if let Some(meta) = self.projectile_meta.get(&t.id) {
                out.push(EntitySnapshot {
                    id: t.id,
                    uuid: meta.uuid,
                    entity_type: meta.entity_type.clone(),
                    position: t.projectile.position,
                    rotation: Rotation::new(0.0, 0.0),
                    head_yaw: 0.0,
                    velocity: t.projectile.velocity,
                    metadata: Vec::new(),
                    // The base arrow entity and friends leave the add-entity
                    // packet's
                    // data at `0`; only add-entity-packet overrides carry one,
                    // and no projectile this sim spawns has one.
                    object_data: 0,
                    // A projectile is never leashable, vanilla's own interface for that.
                    leash_link: None,
                });
            }
        }
        for (&id, state) in &self.item_state {
            out.push(EntitySnapshot {
                id,
                // **`minecraft:item`, not the item's own key.** This field is an
                // *entity* type. The fixed item entity type keeps a dropped
                // stack on the item-entity rendering path rather than treating
                // its registry key as an entity type.
                //
                // The item's *identity* belongs in `metadata` instead, at the
                // item-stack metadata slot described below.
                uuid: state.uuid,
                entity_type: item_entity_type(),
                position: state.motion.position,
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: state.motion.velocity,
                // **The field that makes a drop draw at all.** A
                // client draws nothing for an item entity whose stack it has
                // not been told: the renderer returns early on an empty stack,
                // and this project's own
                // client receives the same stack update.
                // The metadata must therefore carry the stack for a block drop
                // to draw while it falls, merges, and remains pickable.
                //
                // This is the **only** place in the tree that constructs a
                // `MetadataField::Item`, and that is load-bearing rather than
                // incidental: the item-stack metadata field's wire index (8) is
                // shared with other entity fields, so the encoder
                // in `crates/protocol/v770/src/server_protocol.rs` relies on
                // every `Item` field belonging to a `minecraft:item` entity by
                // construction. This loop iterates `item_state`, so it does.
                // Never push one from the mob or projectile loops above.
                //
                // The count is the *entity's* stack size and lives on the
                // lifecycle, not on `ItemState` — the same
                // `map_or(1, |l| l.count)` read `merge_neighbouring_items` uses
                // above, with the same default for the (unreachable in
                // practice) case of state without a lifecycle.
                metadata: vec![MetadataField::Item {
                    item: state.item.clone(),
                    count: self.items.get(id).map_or(1, |lifecycle| lifecycle.count),
                }],
                // The stack travels as metadata (above), not as object data.
                object_data: 0,
                // A dropped item is never leashable.
                leash_link: None,
            });
        }
        // `ExperienceOrb`. Iterated in **sorted** id order, like the falling blocks
        // below and unlike the two loops above: an orb's whole visible behaviour is a
        // multi-tick drift toward the player, so a `HashMap` order would reshuffle
        // which of two orbs the snapshot stream updates first every tick.
        let mut orb_ids: Vec<i32> = self.orbs.keys().copied().collect();
        orb_ids.sort_unstable();
        for id in orb_ids {
            let Some(orb) = self.orbs.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: orb.uuid,
                entity_type: orb_entity_type(),
                position: orb.motion.position,
                // A random rotation has no consumer: the client billboards the
                // sprite at the camera.
                // Sending a rotation would be sending a value with no consumer.
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: orb.motion.velocity,
                // **The field that decides which of the eleven sprite frames draws.**
                // Vanilla's own icon-bucketing getter buckets the orb's own value getter — not `count`, and not
                // linearly — so an orb whose value never reaches the client draws frame
                // 0 (the smallest) whatever it is worth. Vanilla's own metadata
                // registration registers
                // its own value field and nothing else, so metadata is the only channel;
                // there is no object data on the add-entity packet to carry it.
                //
                // `count` is deliberately *not* sent: vanilla does not synchronise it,
                // and a client that knew it would still draw one sprite.
                metadata: vec![MetadataField::ExperienceOrbValue { value: orb.value }],
                object_data: 0,
                // An experience orb is never leashable, vanilla's own interface for that.
                leash_link: None,
            });
        }
        // The falling-block entity. The **only** producer of a non-zero
        // `object_data` in this crate: vanilla's own add-entity packet passes
        // the block-state id, and that field is the sole channel
        // by which a client learns which block is falling (see
        // `TrackedFallingBlock`'s own doc for why metadata cannot carry it).
        //
        // Iterated in **sorted** id order, unlike the two loops above. A falling
        // block's whole point is a smooth multi-tick animation, and
        // `EntityStreamer::sync` walks this list in order to emit spawns and
        // updates; a `HashMap` order would reshuffle which of two simultaneous
        // falls is updated first from tick to tick for no reason.
        let mut falling_ids: Vec<i32> = self.falling_blocks.keys().copied().collect();
        falling_ids.sort_unstable();
        for id in falling_ids {
            let Some(tracked) = self.falling_blocks.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: tracked.uuid,
                entity_type: falling_block_entity_type(),
                position: tracked.motion.position,
                // Vanilla's own falling-block entity never rotates: its own
                // fall step sets no `yRot`/`xRot`
                // and nothing writes them afterwards. A falling block that visibly
                // spun would be a *more* interesting animation and a wrong one.
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: Vec3::new(0.0, tracked.motion.velocity_y, 0.0),
                // Vanilla's own metadata registration registers its own
                // start-position field alone, and that
                // accessor's value is the entity's own spawn cell — which the
                // client recovers from the add-entity packet's position in
                // its own client-side reconstruction. So there is genuinely
                // nothing to send.
                metadata: Vec::new(),
                // `unwrap_or(0)` rather than skipping the entity: an unresolvable
                // state is a data-table gap, and streaming the entity with a wrong
                // texture is a visible bug a reader can chase, while silently
                // dropping it reproduces the original teleport with no trace. The
                // three states `crate::gravity_tick::is_gravity_block` accepts all
                // resolve.
                object_data: block_states::state_id(&tracked.state).unwrap_or(0) as i32,
                // A falling block is never leashable, vanilla's own interface for that.
                leash_link: None,
            });
        }
        // Vehicles — the boats. Sorted ids, like the two loops above and for the
        // same reason: a boat's whole point is a smooth multi-tick glide, so a
        // `HashMap` order would reshuffle which of two boats `EntityStreamer::sync`
        // updates first every tick.
        let mut vehicle_ids: Vec<i32> = self.vehicles.keys().copied().collect();
        vehicle_ids.sort_unstable();
        for id in vehicle_ids {
            let Some(vehicle) = self.vehicles.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: vehicle.uuid,
                entity_type: vehicle.entity_type.clone(),
                position: Vec3::new(
                    vehicle.motion.position.x,
                    vehicle.motion.position.y,
                    vehicle.motion.position.z,
                ),
                // **The yaw is the point.** A boat's hull is the only thing that
                // shows which way it faces, and the placing player's action
                // supplies it — a boat streamed at yaw 0 always points south
                // however you placed it. The pitch stays 0.
                rotation: Rotation::new(vehicle.yaw, 0.0),
                // A boat is not a living entity, so there is no separate
                // head rotation to send; the rotate-head packet is only sent
                // for entities that have one.
                head_yaw: 0.0,
                velocity: Vec3::new(
                    vehicle.motion.velocity.x,
                    vehicle.motion.velocity.y,
                    vehicle.motion.velocity.z,
                ),
                // Boat metadata contains paddle-left and paddle-right values,
                // on top of the shared vehicle hurt state.
                //
                // The paddle pair is emitted — the `PADDLE_BOAT`
                // remainder — via `MetadataField::BoatPaddles`, whose own doc
                // has the index-11/12 collision this loop is the guard for
                // (every entry here is a boat by construction, never the
                // living entity/thrown-trident that also claim those
                // indices). Always included, even at its `false, false`
                // default — the same "always included" convention
                // `CreeperSwellDir`'s own doc states, and load-bearing here:
                // a stop-paddling transition must reach a diffing consumer as
                // a real `false, false` rather than as an absent field.
                // Bubble-time metadata stays unsent: nothing in this crate's
                // boat physics tracks a bubble-column timer.
                metadata: vec![
                    crate::protocol::MetadataField::BoatPaddles {
                        left: vehicle.paddle_left,
                        right: vehicle.paddle_right,
                    },
                    // Shared vehicle hurt state. Always included, at its
                    // resting `(0, 1, 0.0)` as well, for `BoatPaddles`' own
                    // stated reason: the *end* of a rock has to reach a diffing
                    // consumer as a real zero rather than as an absent field, or
                    // the hull stays tipped over for as long as the boat exists.
                    crate::protocol::MetadataField::VehicleHurt {
                        time: vehicle.hurt_time,
                        dir: vehicle.hurt_dir,
                        damage: vehicle.damage,
                    },
                ],
                // The boat supplies no additional spawn data.
                object_data: 0,
                // A boat is never leashable.
                leash_link: None,
            });
        }
        // Primed TNT. Sorted ids, for the same reason every other sidecar loop
        // in this method is: a stable per-tick update order for
        // the snapshot stream.
        let mut tnt_ids: Vec<i32> = self.tnt.keys().copied().collect();
        tnt_ids.sort_unstable();
        for id in tnt_ids {
            let Some(t) = self.tnt.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: t.uuid,
                entity_type: tnt::tnt_entity_type(),
                position: Vec3::new(t.motion.position.x, t.motion.position.y, t.motion.position.z),
                // A primed TNT entity never rotates — its base
                // entity's rotation fields
                // stay `0.0` for the whole of its short life.
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: Vec3::new(t.motion.velocity.x, t.motion.velocity.y, t.motion.velocity.z),
                // The fuse metadata field — see `MetadataField::TntFuse`'s own
                // doc for why this is index 8's fifth `INT` claimant and must be
                // class-guarded on decode.
                metadata: vec![MetadataField::TntFuse(t.fuse)],
                // No additional spawn data is needed.
                object_data: 0,
                // Never leashable.
                leash_link: None,
            });
        }
        // Minecarts. Sorted ids, for the same reason every other sidecar loop
        // in this method is.
        let mut minecart_ids: Vec<i32> = self.minecarts.keys().copied().collect();
        minecart_ids.sort_unstable();
        for id in minecart_ids {
            let Some(cart) = self.minecarts.get(&id) else {
                continue;
            };
            // Furnace-minecart fuel metadata uses index 13, shared with
            // its own command-block-minecart command-name field (a `STRING`) under a
            // different serializer; this is the only producer of a
            // `MinecartFuel` field and it only ever fires from the furnace
            // loop, so the two can never collide the way `MetadataField::Item`'s
            // own doc describes for index 8.
            let metadata = if cart.kind.is_furnace() {
                vec![MetadataField::MinecartFuel(cart.fuel > 0)]
            } else {
                Vec::new()
            };
            out.push(EntitySnapshot {
                id,
                uuid: cart.uuid,
                entity_type: cart.kind.entity_type(),
                position: Vec3::new(cart.motion.position.x, cart.motion.position.y, cart.motion.position.z),
                rotation: Rotation::new(cart.yaw, 0.0),
                // A minecart is not a living entity; no separate head
                // rotation packet is ever sent for one.
                head_yaw: 0.0,
                velocity: Vec3::new(cart.motion.velocity.x, cart.motion.velocity.y, cart.motion.velocity.z),
                metadata,
                // No additional spawn data is needed.
                object_data: 0,
                // Never leashable.
                leash_link: None,
            });
        }
        // The lightning-bolt entity. Sorted ids for the same
        // reason the two
        // loops above are: a bolt is short-lived but real entities, and a
        // `HashMap` order would reshuffle which of two simultaneous strikes
        // the snapshot stream updates first.
        //
        // **Empty metadata is correct, not an omission**: the lightning entity
        // has no metadata fields,
        // so there is nothing to send.
        let mut bolt_ids: Vec<i32> = self.lightning_bolts.keys().copied().collect();
        bolt_ids.sort_unstable();
        for id in bolt_ids {
            let Some(bolt) = self.lightning_bolts.get(&id) else {
                continue;
            };
            out.push(EntitySnapshot {
                id,
                uuid: bolt.uuid,
                entity_type: lightning::lightning_bolt_entity_type(),
                position: bolt.pos,
                // A bolt never rotates or moves once struck.
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: Vec3::new(0.0, 0.0, 0.0),
                metadata: Vec::new(),
                // No additional spawn data is needed for this lightning entity.
                object_data: 0,
                // Never a `Leashable`.
                leash_link: None,
            });
        }
        self.push_dragon_snapshots(&mut out);
        self.push_end_crystal_snapshots(&mut out);
        self.push_wither_snapshots(&mut out);
        // Live fishing bobbers.
        self.fishing_bobber_snapshots(&mut out);
        // Live raiders spawned by an active raid stream through
        // the ordinary mob loop at the top of this function — `raid.rs`
        // spawns them with `spawn_species`, exactly as a patrol does — so
        // there is nothing to append here.
        out
    }
}

/// Vanilla's own falling-block entity type's registry key — the falling-block twin of
/// [`item_entity_type`], and parsed per call for the same reason that one is: a
/// falling block is a rare, short-lived entity, and the parse is cheaper than the
/// `OnceLock` clone it would replace.
fn falling_block_entity_type() -> ResourceKey {
    crate::gravity_tick::FALLING_BLOCK_ENTITY_TYPE
        .parse()
        .expect("`minecraft:falling_block` is a valid resource key")
}

/// How far below the world's floor an item may sink before it is discarded —
/// vanilla's own "check below world" threshold (its own min-Y-minus-64 comparison).
const VOID_DESPAWN_DEPTH: f64 = 64.0;

/// The dropped item's hitbox — vanilla's own item-entity type
/// dimensions, `0.25 × 0.25`, with **no** auto-step.
///
/// `step_height` is `0.0` rather than the `0.6` an ordinary mob resolves from its
/// `STEP_HEIGHT` attribute: vanilla's own item entity never overrides its
/// max-up-step getter, and
/// the base entity's own default returns `0.0`. Getting this wrong would let a dropped item climb
/// a slab it slid into, which is the sort of thing that looks like a physics
/// improvement in a screenshot.
const ITEM_DIMENSIONS: EntityDimensions = EntityDimensions::new(0.25, 0.25, 0.0);

/// A [`CollisionView`] over a caller-supplied block-state oracle, serving the real
/// per-block-state collision shapes.
///
/// # Why this exists rather than a `Fn(i32, i32, i32) -> bool`
///
/// The item pass used to take exactly that: one boolean per cell, `!is_air_or_fluid`,
/// with an item resolved as a **point** and its rest height hardcoded to `by + 1`.
/// Read out of `lodestone_data`'s generated shape table rather than predicted, that
/// is wrong for most of the blocks a player actually drops things onto:
///
/// | block state | true collision top | the boolean's answer |
/// |---|---|---|
/// | `short_grass`, `tall_grass`, `snow[layers=1]` | **0.0** — no collision at all | solid, so the item rests a full block *above* the ground |
/// | `oak_slab[type=bottom]` | 0.5 | 1.0 |
/// | `enchanting_table` | 0.75 | 1.0 |
/// | `soul_sand`, `mud`, `chest` | 0.875 | 1.0 |
/// | `dirt_path` | 0.9375 | 1.0 |
/// | `oak_fence` | **1.5** — uncapped | 1.0, i.e. *too low* |
///
/// The grass row is the one with the visible symptom, and it is the common case:
/// almost any grassy surface has a plant on it, so almost every dropped item floated.
/// The fence row is worth keeping because it fails in the opposite direction, so a
/// gate that only ever checked "not too high" would miss it.
///
/// # Cost, stated because it is strictly more work per item
///
/// The boolean was one map lookup per cell. This is a `String` from the oracle, a
/// name→id lookup, and an O(1) rodata index — then `collide` sweeps the cells the
/// item's expanded box spans rather than probing one column. `probe_count` is
/// incremented per cell so the cost is a **counter** a gate can assert on rather
/// than a duration, and `items_settled_probe_count` exposes it.
struct ItemCollision<'a> {
    block_state: &'a dyn Fn(i32, i32, i32) -> String,
    probe_count: std::cell::Cell<u64>,
}

impl CollisionView for ItemCollision<'_> {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<lodestone_physics::Aabb>) {
        self.probe_count.set(self.probe_count.get() + 1);
        let name = (self.block_state)(x, y, z);
        // **`block_state_id` then `block_states::state_id`, and emphatically NOT
        // `block_state_id_or_default`.** That helper resolves a bare name to the
        // block's *lowest* state id, and its own doc comment says in as many words
        // that it "is not a substitute for `block_state_id` where the properties
        // matter (collision shapes, path types)". It was used here for one iteration
        // and a bare `minecraft:oak_slab` resolved to a full cube — the lowest id is
        // not `type=bottom` — so the item rested at the top of the cell and the fix
        // reproduced the very bug it removes. `block_states::state_id` consults
        // `span.default`, which is vanilla's real `defaultBlockState()`.
        //
        // The exact-string map is tried first because it is O(1) and because a
        // `ChunkSource` normally hands back a full canonical state; `state_id` scans
        // the block's own span, which is short but not free.
        let Some(id) = block_state_id(&name).or_else(|| block_states::state_id(&name)) else {
            return;
        };
        let Some(shape) = collision_shapes::collision_boxes(id) else {
            return;
        };
        let (bx, by, bz) = (f64::from(x), f64::from(y), f64::from(z));
        for b in shape {
            out.push(lodestone_physics::Aabb::new(
                bx + f64::from(b.min[0]),
                by + f64::from(b.min[1]),
                bz + f64::from(b.min[2]),
                bx + f64::from(b.max[0]),
                by + f64::from(b.max[1]),
                bz + f64::from(b.max[2]),
            ));
        }
    }
}

/// Resolves one item's collision with the terrain after [`ItemMotion::tick`] has
/// already moved it, and records whether it is resting.
///
/// This is the "world crate's job" [`ItemMotion::tick`]'s doc comment always
/// deferred and nothing ever did.
///
/// # What it models, and what it does not
///
/// Vertical only. Vanilla resolves the item's full `0.25 × 0.25 × 0.25` AABB
/// against every intersecting shape in its own generic entity-move step; this pushes the item out of
/// a solid cell it has sunk into, zeroes a downward velocity when that happens,
/// and sets `on_ground` from the cell beneath. Horizontal collision is left out
/// deliberately rather than by oversight: a dropped item's horizontal velocity is
/// a fraction of a block per tick and decays by `ITEM_AIR_DRAG` every tick, so it
/// cannot cross a wall in the time it takes to stop — whereas gravity is
/// unbounded, which is why the vertical case was the one with a visible symptom.
/// The single-column test also means an item is treated as a point at its own
/// centre rather than a cube, so it can settle in a cell whose neighbour is where
/// vanilla's wider box would have caught it. Both are visible as an item resting
/// slightly off-centre in a corner, never as an item falling through the floor.
///
/// Per-block friction is likewise not looked up: `block_friction` keeps
/// [`lodestone_entity::item_entity::DEFAULT_BLOCK_FRICTION`], so an item slides on
/// ice exactly as it does on stone. Vanilla reads
/// `getBlockPosBelowThatAffectsMyMovement().getBlock().getFriction()`; wiring that
/// needs a per-block friction census this crate does not carry.
/// Settles one item against a solidity oracle.
///
/// # Why this takes a closure and not the sim's own `ChunkWorld`
///
/// Settling uses the live `ChunkSource` supplied to
/// [`MobSim::tick_with_terrain`], so placed and removed blocks affect collision
/// at the player's current coordinates. The plain `tick` entry point retains its
/// `ChunkWorld` snapshot for hermetic callers.
fn settle_item(view: &dyn CollisionView, motion: &mut ItemMotion, before: Vec3) {
    settle_entity(view, ITEM_DIMENSIONS, motion, before);
}

/// [`settle_item`] with the hitbox as a parameter, so an experience orb
/// (`0.5 × 0.5`) resolves against the same swept collision an item (`0.25 × 0.25`)
/// does.
///
/// Split out rather than copied: the *geometry* differs between the two entities and
/// nothing else does, and a second copy of the restitution rules is how one of them
/// ends up with the pre-swept-collision point test again. `motion` is the
/// position/velocity/`on_ground` triple only — the caller has already applied whatever
/// per-entity gravity and drag its own `tick` uses.
fn settle_entity(
    view: &dyn CollisionView,
    dimensions: EntityDimensions,
    motion: &mut ItemMotion,
    before: Vec3,
) {
    // The movement the caller's own tick just applied by translating outright. It is
    // recovered rather than recomputed so this cannot drift from that arithmetic
    // (gravity, then translate, then drag, then the landing bounce).
    let attempted = Vec3d::new(
        motion.position.x - before.x,
        motion.position.y - before.y,
        motion.position.z - before.z,
    );

    // **The ordering is deliberately identical to what it replaced**: gravity and
    // drag still happen inside `ItemMotion::tick`, before the collision, and this
    // still runs after. Vanilla's own item-entity per-tick update collides
    // *between* them, so its
    // friction reads the post-move `onGround`. Matching that is a separate change to
    // a crate outside this one; keeping the order fixed here means the only thing
    // the implementation alters is the **geometry**, which is what makes the existing
    // settling gates still meaningful rather than merely still green.
    let bb = dimensions.bounding_box(Vec3d::new(before.x, before.y, before.z));
    let resolved = collide(view, attempted, bb, motion.on_ground, dimensions.step_height);

    motion.position = Vec3::new(
        before.x + resolved.x,
        before.y + resolved.y,
        before.z + resolved.z,
    );

    // Vanilla's own generic entity-move step's own "restitute movement after
    // collisions" helper: zero each component the
    // sweep could not fully apply. Horizontal is included now — the old point test
    // could not see a wall at all, and its doc comment argued that was safe because
    // an item's horizontal velocity decays before it can cross one. That argument
    // holds for *slow* items and not for a thrown one, and it is free to get right
    // here because `collide` resolves all three axes in one call.
    if (resolved.x - attempted.x).abs() > f64::EPSILON {
        motion.velocity.x = 0.0;
    }
    if (resolved.z - attempted.z).abs() > f64::EPSILON {
        motion.velocity.z = 0.0;
    }
    if (resolved.y - attempted.y).abs() > f64::EPSILON {
        motion.velocity.y = 0.0;
    }

    // Vanilla's own rule (its own generic "set on ground with movement" call): grounded when the sweep
    // ate downward movement. This replaces a point probe one epsilon below the
    // bottom face, which is why `ITEM_SUPPORT_EPSILON` is gone: there is no longer a
    // boundary-straddling floor() to defend against, and an item resting on a slab
    // has no block boundary under its feet to probe in the first place.
    motion.on_ground = attempted.y < 0.0 && (resolved.y - attempted.y).abs() > f64::EPSILON;
}

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

// NOTE: this module owns `ChunkWorld` + `MobSim`; the acceptance gate lives in
// `tests/mob_sim.rs` so it drives them through the crate's *public* API — the
// same discipline the rest of the project uses (a consumer that is only a
// `#[cfg(test)]` fake proves nothing about the public seam).

/// A live [`EntitySource`] fed by a background-ticked [`MobSim`] (the live mob tick).
/// [`IntegratedServer::open_in_memory_with_mobs`](crate::IntegratedServer::open_in_memory_with_mobs)
/// constructs one alongside [`crate::tick::run_tick_loop`] (the shared tick loop; this
/// used to be [`run_mob_tick_loop`] before the mob and block-entity tick
/// loops were unified into one), the task that owns the sim and republishes
/// its snapshots here every tick.
///
/// Deliberately the same shape as `entity_streaming_live.rs`'s own test-only
/// `SharedSnapshotSource` (an `Arc<Mutex<Vec<EntitySnapshot>>>` behind
/// [`EntitySource`]) — that test already proved the read side of this shape
/// reaches a real client; this type is the production version, now fed by a
/// real simulation instead of a hand-mutated `Vec`.
#[derive(Debug, Clone, Default)]
pub struct LiveMobSource(
    Arc<Mutex<Vec<EntitySnapshot>>>,
    /// The dragon fight's boss bars, published the same way `.0` is — see
    /// [`publish_boss_bars`](Self::publish_boss_bars). A second field rather
    /// than folding into `.0` because a boss bar is not an entity and has no
    /// `EntitySnapshot` shape to borrow.
    Arc<Mutex<Vec<crate::protocol::BossBarSnapshot>>>,
);

impl EntitySource for LiveMobSource {
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.0
            .lock()
            .expect("live mob snapshot lock poisoned")
            .clone()
    }

    fn boss_bars(&self) -> Vec<crate::protocol::BossBarSnapshot> {
        self.1
            .lock()
            .expect("live mob boss-bar lock poisoned")
            .clone()
    }
}

impl LiveMobSource {
    /// Replaces the published snapshot set. Called once per tick — in
    /// production by [`crate::tick::run_tick_loop`], and directly by the
    /// tick-source test. The
    /// next `snapshots()` call from any connection (there may be several,
    /// e.g. open-to-LAN) sees the new set. `pub(crate)`, not private: the
    /// unified loop lives in a sibling module (`tick.rs`) and needs to call
    /// this directly rather than through a second wrapper.
    pub(crate) fn publish(&self, snapshots: Vec<EntitySnapshot>) {
        *self.0.lock().expect("live mob snapshot lock poisoned") = snapshots;
    }

    /// Replaces the published boss-bar set — the [`boss_bars`](EntitySource::boss_bars)
    /// twin of [`publish`](Self::publish), called from the same tick-loop
    /// call site right after it (see `crate::tick::run_tick_loop`).
    pub(crate) fn publish_boss_bars(&self, bars: Vec<crate::protocol::BossBarSnapshot>) {
        *self.1.lock().expect("live mob boss-bar lock poisoned") = bars;
    }
}

/// A shared, mutation-capable handle onto one live [`MobSim`] — the
/// counterpart [`crate::BlockEntityHandle`] already established for block
/// entities, and the exact piece the combat census named as
/// missing: *"there is no way to reach a live mob's health from a
/// connection's own task... `MobSim` is ticked entirely inside its own
/// background task and is never wrapped in a shared, lockable handle."*
/// [`LiveMobSource`] is deliberately read-only (a snapshot cache for
/// streaming, fed *by* the tick loop); this is the mutation-capable sibling a
/// connection needs to actually damage/knock back a mob a player attacked —
/// see `crate::server::apply_attack`, its one production caller.
///
/// # Why `MobSim<'static>`, and the leak that produces it
///
/// [`MobSim`] borrows its [`ChunkWorld`] (`MobSim<'w>`), but a handle shared
/// with a separately-`tokio::spawn`ed connection task must be `'static` (that
/// is what `tokio::spawn` requires of everything it captures). [`new`](Self::new)
/// resolves this with [`Box::leak`]: the `ChunkWorld` a caller hands in is
/// leaked once, for the process's remaining lifetime, rather than borrowed
/// for one task's own stack frame the way [`run_mob_tick_loop`]'s previous
/// (pre-handle) implementation did.
///
/// This is a **deliberate, bounded** leak, not an oversight.
/// `run_mob_tick_loop`'s own doc comment already discloses that its
/// `ChunkWorld` snapshot is static for the sim's whole lifetime — a fixed
/// area around the mob-spawn center, never widened after the initial load.
/// Leaking it only changes *whose* lifetime "static" is measured against:
/// "static for this one task" becomes "static for the process" — the same
/// bytes, held slightly longer, for the one [`MobSim`] a running
/// [`crate::IntegratedServer`] ever constructs per call to
/// [`open_in_memory_with_mobs`](crate::IntegratedServer::open_in_memory_with_mobs).
/// A caller that constructs many short-lived handles (e.g. one per test) does
/// leak once per handle — acceptable for a bounded terrain snapshot in a
/// process that exits shortly after, the same trade-off `MobSim`'s own
/// `assert_send::<MobSim<'static>>()` const-check already anticipated by
/// name.
#[derive(Debug, Clone)]
pub struct MobHandle(Arc<Mutex<MobSim<'static>>>);

impl Default for MobHandle {
    /// A handle over an empty, mobless sim backed by a tiny leaked
    /// [`ChunkWorld`] — the "nothing ticks it, but it is real and safe to
    /// attack against" default [`crate::BlockEntityHandle::default`] already
    /// establishes for connections built without a live mob population
    /// (`IntegratedServer::open_in_memory`/`open_in_memory_with_entities`/`bind`).
    /// An `Attack` packet against any entity id here simply finds no mob
    /// ([`MobSim::attack`] returns `None`) — a harmless no-op, never a panic.
    fn default() -> Self {
        Self::new(ChunkWorld::new(-64, 384))
    }
}

impl MobHandle {
    /// Builds a handle over a fresh, empty [`MobSim`] backed by a leaked copy
    /// of `world` — see the struct's own doc comment for why leaking is the
    /// deliberate choice here.
    #[must_use]
    pub fn new(world: ChunkWorld) -> Self {
        let world: &'static ChunkWorld = Box::leak(Box::new(world));
        Self(Arc::new(Mutex::new(MobSim::new(world))))
    }

    /// Builds a handle already seeded with [`seed_demo_mobs`]'s baseline
    /// population, snapshotting `world_source` the same way the previous
    /// (pre-handle) `run_mob_tick_loop` did at the top of its own future —
    /// see that function's doc comment for the `cx_range`/`cz_range`/
    /// `mob_center` scope notes, unchanged by this refactor.
    #[must_use]
    pub fn seeded<S: ChunkSource>(
        world_source: &S,
        cx_range: std::ops::RangeInclusive<i32>,
        cz_range: std::ops::RangeInclusive<i32>,
        center_x: i32,
        center_z: i32,
        mob_count: usize,
    ) -> Self {
        let handle = Self::default();
        handle.reseed(
            ChunkWorld::from_source(world_source, cx_range, cz_range),
            center_x,
            center_z,
            mob_count,
        );
        handle
    }

    /// Replaces this handle's terrain snapshot **and** its population with a
    /// fresh [`MobSim`] over `world`, seeded exactly as
    /// [`seeded`](Self::seeded) would have.
    ///
    /// # Why this exists
    ///
    /// `seeded` did the whole job inside
    /// [`crate::IntegratedServer::open_in_memory_with_mobs`]'s body, *before any
    /// task spawned* — so the 49-column `ChunkWorld::from_source` snapshot it
    /// needs was on the critical path of opening a world, at ~909 ms per
    /// composed column. Vanilla does not block world-open on mob population, and
    /// neither does this crate any more: the constructor now builds a
    /// [`Default`] handle (empty, mobless, safe to attack against — see that
    /// impl's own doc comment) and a background task calls this once the terrain
    /// it needs has been fetched off-thread.
    ///
    /// # What is deliberately thrown away
    ///
    /// Everything: the old `MobSim` is dropped, not merged. That is correct for
    /// the one caller — a handle that has only ever been `Default` has no
    /// population to lose, and `set_next_id(1000)` must be re-applied to the new
    /// sim anyway. It is **not** a general "load more terrain" primitive; a mob
    /// spawned in the window before the first reseed would vanish. Widening the
    /// snapshot as the player walks (this module's long-standing documented
    /// scope cut) needs a sim that can *extend* its world, not replace it.
    ///
    /// Takes `&self`, like every other accessor here, because the sim lives
    /// behind the handle's own `Mutex` — so this is safe to call from a
    /// background task while the connection task holds a clone.
    pub fn reseed(&self, mut world: ChunkWorld, center_x: i32, center_z: i32, mob_count: usize) {
        // Drain pending generation spawns while `world` is still an owned local,
        // before it leaks to `'static` below. The list is non-empty only while
        // these chunks are ever generated — see `ChunkWorld`'s own field doc
        // (`pending_generation_spawns`) for why that is what keeps a fresh
        // world's `SPAWN`-stage animals from duplicating across a restart: a
        // reload of an existing world loads these same chunks from disk, which
        // never populates this list.
        let pending_generation_spawns = world.take_pending_generation_spawns();
        // Leaked for the same reason `new` leaks: `MobSim` borrows its world for
        // `'static`. See the struct's own doc comment — one bounded snapshot per
        // reseed, and production reseeds exactly once per world.
        let world: &'static ChunkWorld = Box::leak(Box::new(world));
        self.with(|sim| {
            *sim = MobSim::new(world);
            // See `MobSim::set_next_id`'s own doc comment: id `1` collides
            // with `LOCAL_PLAYER_ENTITY_ID` on the wire.
            sim.set_next_id(1000);
            // Exactly `mob_count`, including zero — see [`seed_demo_mobs`].
            seed_demo_mobs(sim, center_x, center_z, mob_count);
            // Place the `SPAWN` stage's proposed animals as real mobs,
            // re-validated against the per-species placement rule
            // and this world's own light through the exact gate the
            // tick-driven cycle uses — see
            // `NaturalSpawner::validate_generation_spawns`'s doc for why this
            // reuses rather than re-implements it.
            if !pending_generation_spawns.is_empty() {
                let mut spawner = crate::natural_spawn::NaturalSpawner::new(
                    crate::worldgen_data::bundled_biome_spawners().clone(),
                    0,
                )
                .with_world_seed(crate::worldgen_data::active_world_seed());
                spawner.begin_cycle(std::sync::Arc::new(world.clone()), 0, Vec::new());
                for candidate in spawner.validate_generation_spawns(pending_generation_spawns) {
                    let mob = sim.spawn_species(candidate.entity_type, candidate.pos);
                    mob.set_category(MobCategory::Creature)
                        .set_persistent(MobCategory::Creature.is_persistent());
                }
            }
        });
    }

    /// Runs `f` against the locked sim, returning its result — the same
    /// funnel-every-access shape [`crate::BlockEntityHandle::with`]
    /// established, for the identical "no caller can forget to handle a
    /// poisoned lock inconsistently" reason.
    pub fn with<R>(&self, f: impl FnOnce(&mut MobSim<'static>) -> R) -> R {
        let mut guard = self.0.lock().expect("mob sim lock poisoned");
        f(&mut guard)
    }
}

impl EntitySource for MobHandle {
    /// A `MobHandle` is a legitimate [`EntitySource`] all on its own — no
    /// separate [`LiveMobSource`] cache required — for any caller that mutates
    /// the sim directly and does not also need a background tick loop
    /// ([`crate::tick::run_tick_loop`]) republishing it on a
    /// timer. Production (`IntegratedServer::open_in_memory_with_mobs`) still layers
    /// [`LiveMobSource`] on top so the tick loop's own AI motion reaches the
    /// wire on its own cadence; a test that only cares about a hand-placed,
    /// unticked mob (e.g. an attack test) can use the handle directly instead.
    fn snapshots(&self) -> Vec<EntitySnapshot> {
        self.with(|sim| sim.snapshots())
    }

    fn boss_bars(&self) -> Vec<crate::protocol::BossBarSnapshot> {
        self.with(|sim| sim.boss_bars())
    }
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

/// The `follow_range` attribute reaches the controller that bounds target
/// acquisition, including the no-target case.
#[cfg(test)]
mod follow_range_tests {
    // Also home to the death-loot gate, which reuses this module's
    // `flat_world` rather than growing a second copy of it.
    use super::*;

    /// A floor wide enough for a mob at the origin and a player out past 36
    /// blocks, so nothing here depends on a mob standing in the void.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=48 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    /// Spawns `species` at the origin through the **production** path
    /// ([`MobSim::spawn_species`], what `seed_demo_mobs` calls), feeds one player
    /// `distance` blocks away on +X, and reports whether the mob ever acquires a
    /// target within `ticks`.
    ///
    /// `attack_target()` is the observable, not `can_use`: target acquisition
    /// is throttled, so this ticks a
    /// generous bound and checks after each — a fixed single tick would measure
    /// the throttle rather than the range.
    fn acquires_at(species: &str, distance: f64, ticks: usize) -> bool {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str(&format!("minecraft:{species}")).expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.set_players(vec![PlayerPerception {
            position: Vec3::new(distance, 0.0, 0.0),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        }]);
        for _ in 0..ticks {
            sim.tick();
            if sim.get(id).expect("alive").attack_target().is_some() {
                return true;
            }
        }
        false
    }

    /// `push_impulse`'s exact vanilla formula, at an off-axis input chosen so
    /// two plausible implementations diverge: Chebyshev normalisation
    /// (`sqrt(max(|dx|, |dz|))`, what `Entity::push` actually does) against
    /// the more obvious Euclidean one (`sqrt(dx² + dz²)`, what a port that
    /// "corrected" the formula would produce). `dx=0.3, dz=0.4` was chosen
    /// precisely because `max(0.3, 0.4) = 0.4 != dx² + dz² = 0.25`, so the
    /// two hypotheses give different numbers and this input can actually
    /// tell them apart — a symmetric or on-axis pair could not.
    #[test]
    fn push_impulse_matches_a_hand_computed_off_axis_example_not_the_euclidean_alternative() {
        let p_i = Vec3::new(0.0, 0.0, 0.0);
        let p_j = Vec3::new(0.3, 0.0, 0.4);

        // Hand-computed from `Entity::push`'s own arithmetic, independent of
        // `push_impulse`'s implementation:
        //   dd = max(0.3, 0.4) = 0.4; dd = sqrt(0.4) = 0.632455532...
        //   xa = 0.3 / dd = 0.474341649...; za = 0.4 / dd = 0.632455532...
        //   pow = min(1.0, 1.0 / dd) = 1.0 (1/dd ≈ 1.581 > 1)
        //   xa *= 0.05 = 0.023717082...; za *= 0.05 = 0.031622777...
        let dd = 0.4f64.sqrt();
        let expected_xa = (0.3 / dd) * 0.05;
        let expected_za = (0.4 / dd) * 0.05;

        // The wrong hypothesis, evaluated first: Euclidean normalisation
        // (dist = sqrt(0.3² + 0.4²) = 0.5) gives a different pair of numbers.
        let euclid_dist = (0.3f64 * 0.3 + 0.4 * 0.4).sqrt();
        let wrong_xa = (0.3 / euclid_dist) * 0.05;
        let wrong_za = (0.4 / euclid_dist) * 0.05;
        assert!(
            (expected_xa - wrong_xa).abs() > 1.0e-4 && (expected_za - wrong_za).abs() > 1.0e-4,
            "precondition: the chosen input must actually separate the two \
             hypotheses, got chebyshev=({expected_xa}, {expected_za}) \
             euclidean=({wrong_xa}, {wrong_za})"
        );

        let (impulse_i, impulse_j) =
            push_impulse(p_i, p_j, 1.0).expect("well within the touch threshold");
        assert!(
            (impulse_i.x - -expected_xa).abs() < 1.0e-9 && (impulse_i.z - -expected_za).abs() < 1.0e-9,
            "p_i must be pushed away from p_j by the Chebyshev-derived amount, \
             expected ({}, {}), got ({}, {})",
            -expected_xa,
            -expected_za,
            impulse_i.x,
            impulse_i.z
        );
        assert!(
            (impulse_j.x - expected_xa).abs() < 1.0e-9 && (impulse_j.z - expected_za).abs() < 1.0e-9,
            "p_j must be pushed away from p_i by the same magnitude, opposite \
             sign, expected ({expected_xa}, {expected_za}), got ({}, {})",
            impulse_j.x,
            impulse_j.z
        );
        assert!(
            (impulse_i.x - wrong_xa).abs() > 1.0e-4,
            "the Euclidean hypothesis must NOT match — if it does, the \
             normalisation silently changed to the wrong formula"
        );
    }

    /// The control an absence assertion needs: a pair separated just beyond
    /// the touch threshold must produce no impulse at all, proving the
    /// detector (the overlap test) actually fires rather than being
    /// unconditionally permissive.
    #[test]
    fn push_impulse_is_none_just_outside_the_touch_threshold() {
        let p_i = Vec3::new(0.0, 0.0, 0.0);
        let p_j = Vec3::new(0.61, 0.0, 0.0);
        assert!(
            push_impulse(p_i, p_j, 0.6).is_none(),
            "0.61 blocks apart must not touch at a 0.6 threshold"
        );
        // And the positive control, one hair inside: proves 0.6 is not simply
        // always None.
        let p_k = Vec3::new(0.59, 0.0, 0.0);
        assert!(
            push_impulse(p_i, p_k, 0.6).is_some(),
            "0.59 blocks apart must touch at a 0.6 threshold"
        );
    }

    /// Wires `push_impulse` into the real per-tick sim: two overlapping mobs
    /// must actually separate over real ticks, and a third mob spawned well
    /// outside touch range is the control that must not move at all — an
    /// absence assertion (mob-mob pushing is un-wired) needs a detector
    /// proven to fire, which the near pair provides.
    #[test]
    fn overlapping_mobs_separate_over_real_ticks_and_a_distant_one_does_not_move() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:pig").expect("valid key");
        let near_a = sim.spawn_species(key.clone(), Vec3::new(0.0, 0.0, 0.0)).id();
        let near_b = sim.spawn_species(key.clone(), Vec3::new(0.3, 0.0, 0.0)).id();
        // Inside `flat_world`'s own `-8..=48` solid floor (with margin, so an
        // idle pig standing on real ground does not also start falling —
        // idle mobs have real gravity now, see `NavigatingMob::advance` —
        // which would be a second, unrelated source of displacement this
        // control does not mean to exercise).
        let far = sim.spawn_species(key, Vec3::new(45.0, 0.0, 0.0)).id();

        for _ in 0..20 {
            sim.tick();
        }

        let gap = (sim.get(near_a).expect("alive").position()
            - sim.get(near_b).expect("alive").position())
        .x
        .abs();
        assert!(
            gap > 0.3,
            "two overlapping pigs must separate over 20 ticks of pushing, \
             gap only grew to {gap}"
        );
        assert_eq!(
            sim.get(far).expect("alive").position(),
            Vec3::new(45.0, 0.0, 0.0),
            "control: a pig with nothing nearby must not be displaced by the \
             push pass at all"
        );
    }

    /// The primary reported symptom ("I can't push entities like pigs"): a
    /// player walking into a mob must shove it out of the way. Player recoil
    /// is deliberately not asserted — see `MobSim::push_entities`'s own doc
    /// comment for why that half needs a different seam.
    #[test]
    fn a_player_overlapping_a_mob_pushes_the_mob_away() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:pig").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.set_players(vec![PlayerPerception {
            position: Vec3::new(0.2, 0.0, 0.0),
            held_item: None,
            view_direction: Vec3::new(0.0, 0.0, 1.0),
        }]);

        for _ in 0..10 {
            sim.tick();
        }

        let moved = sim.get(id).expect("alive").position();
        assert!(
            moved.x < 0.0,
            "a player standing at +x inside the pig's body must push the pig \
             toward -x; pig ended at x={}",
            moved.x
        );
    }

    /// A killed mob drops its loot table's items.
    ///
    /// The expected values are independent of the roller: two pools use
    /// `rolls: 1`, leather uses `uniform 0..2`, and beef uses `uniform 1..3`.
    /// A kill therefore yields at least beef; the leather stack may be absent,
    /// while the beef count is never zero.
    #[test]
    fn a_killed_cow_drops_its_loot_table() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:cow").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.get_mut(id).expect("alive").set_health(1.0);

        let outcome = sim
            .attack(id, Vec3::new(1.0, 0.0, 0.0), 100.0, DamageFlags::default(), 0.0)
            .expect("the cow is a live target");
        assert!(outcome.killed);

        let dropped = sim.dropped_items();
        assert!(
            !dropped.is_empty(),
            "a cow's death must drop something — entities/cow.json guarantees the beef pool"
        );
        for (item, count) in &dropped {
            assert!(
                matches!(item.as_str(), "minecraft:leather" | "minecraft:beef"),
                "cow.json names only leather and beef, got {item}"
            );
            assert!(*count > 0, "a zero-count stack must be filtered, got {item}");
        }
        assert!(
            dropped.iter().any(|(item, _)| item == "minecraft:beef"),
            "the beef pool is `rolls: 1` with `uniform 1..3`, so it is never absent: {dropped:?}"
        );
    }

    /// Armadillo roll-up, gated through the production path
    /// (`MobSim::attack`, `crate::server::apply_attack`'s entry point) rather
    /// than calling `apply_damage` on a bare `SimMob` — the
    /// same standard this crate applies to every other combat gate. A first
    /// hit lands at full strength and switches the armadillo to "scared";
    /// a **second** hit, once the invulnerability window has cleared, is
    /// halved via `(damage - 1) / 2`.
    #[test]
    fn armadillo_rolls_up_after_a_hit_and_halves_the_next_one() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:armadillo").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.get_mut(id).expect("alive").set_health(100.0);

        assert!(
            !sim.get(id).expect("alive").armadillo_is_scared(),
            "precondition: a fresh armadillo is not scared"
        );

        let first = sim
            .attack(id, Vec3::new(1.0, 0.0, 0.0), 10.0, DamageFlags::default(), 0.0)
            .expect("the armadillo is a live target");
        assert_eq!(first.damage_dealt, 10.0, "the triggering hit itself is not reduced");
        assert!(
            sim.get(id).expect("alive").armadillo_is_scared(),
            "a hit that passes the i-frame gate must roll the armadillo up"
        );

        // Clear the 20-tick invulnerability window (see `HurtCooldown::on_hurt`)
        // so the second hit is not merely dropped as "not stronger".
        for _ in 0..11 {
            sim.tick();
        }
        assert!(
            sim.get(id).expect("alive").armadillo_is_scared(),
            "11 of 80 danger ticks must not have expired the scared state yet"
        );

        let second = sim
            .attack(id, Vec3::new(1.0, 0.0, 0.0), 10.0, DamageFlags::default(), 0.0)
            .expect("still a live target");
        assert_eq!(
            second.damage_dealt, 4.5,
            "Armadillo.hurtServer: (10.0 - 1.0) / 2.0 == 4.5, while scared"
        );
    }

    /// **Control**: the identical fixture and hit sequence against a cow —
    /// a species `apply_damage`'s armadillo branch must never touch — proves
    /// the halving is armadillo-specific rather than a general "second hit is
    /// cheaper" bug the first test could not, by itself, distinguish from a
    /// mistake in the i-frame/topup arithmetic.
    #[test]
    fn only_armadillo_halves_a_repeat_hit_a_cow_does_not() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:cow").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.get_mut(id).expect("alive").set_health(100.0);

        sim.attack(id, Vec3::new(1.0, 0.0, 0.0), 10.0, DamageFlags::default(), 0.0)
            .expect("live target");
        for _ in 0..11 {
            sim.tick();
        }
        let second = sim
            .attack(id, Vec3::new(1.0, 0.0, 0.0), 10.0, DamageFlags::default(), 0.0)
            .expect("still live");
        assert_eq!(second.damage_dealt, 10.0, "a cow never rolls up, so the second hit is unreduced");
    }

    /// The danger memory expires 80 ticks after the hit that (re)armed it,
    /// with nothing further landing in between — `Armadillo`'s own
    /// `DANGER_DETECTED_RECENTLY` memory duration — and the armadillo
    /// un-scares on schedule rather than staying curled forever.
    #[test]
    fn armadillo_unscares_eighty_ticks_after_its_last_hit() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:armadillo").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.get_mut(id).expect("alive").set_health(100.0);

        sim.attack(id, Vec3::new(1.0, 0.0, 0.0), 10.0, DamageFlags::default(), 0.0)
            .expect("live target");
        assert!(sim.get(id).expect("alive").armadillo_is_scared());

        for _ in 0..79 {
            sim.tick();
        }
        assert!(
            sim.get(id).expect("alive").armadillo_is_scared(),
            "the 79th tick must still be inside the 80-tick window"
        );
        sim.tick();
        assert!(
            !sim.get(id).expect("alive").armadillo_is_scared(),
            "the 80th tick with no further hit must let the timer reach zero"
        );
    }

    /// A floor with a real `minecraft:water` layer at `y=0` — distinct from
    /// `flat_world`'s dry ground (`y=-1` stone, `y=0` air) so the axolotl
    /// play-dead gate below can prove its own `in_water()` precondition
    /// rather than assuming the fixture provides it.
    fn water_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=48 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
                world.set_block(x, 0, z, "minecraft:water");
            }
        }
        world
    }

    /// Axolotl play-dead (`AXOLOTL_PLAY_DEAD_TICKS` = `200`), gated through the production path
    /// (`MobSim::attack` → `SimMob::apply_damage`). The trigger is
    /// probabilistic (`axolotl_play_dead_roll`'s own two `nextInt(3)`-shaped
    /// draws), so — the "predict the value" standard, applied to a
    /// probabilistic trigger instead of a fixed one — this searches a small
    /// range of raw-damage values for one this mob's own roll stream
    /// actually fires on, using the exact function `apply_damage` calls,
    /// rather than looping the real attack call hoping for a hit.
    #[test]
    fn a_hurt_axolotl_in_water_plays_dead_on_a_winning_roll() {
        let world = water_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:axolotl").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.get_mut(id).expect("alive").set_health(100.0);

        assert!(
            !sim.get(id).expect("alive").axolotl_is_playing_dead(),
            "precondition: a fresh axolotl is not playing dead"
        );
        assert!(
            sim.get(id).expect("alive").in_water(),
            "precondition: the fixture must actually place the axolotl in water"
        );

        let health_bits = 100.0_f32.to_bits();
        let raw_damage = (1..30)
            .map(|d| d as f32)
            .find(|&d| {
                let (a, b) = axolotl_play_dead_roll(id as u64, health_bits, d.to_bits());
                a == 0 && (b as f32) < d
            })
            .expect("a 1-in-3 draw over 29 tries must fire at least once");

        sim.attack(id, Vec3::new(1.0, 0.0, 0.0), raw_damage, DamageFlags::default(), 0.0)
            .expect("the axolotl is a live target");

        assert!(
            sim.get(id).expect("alive").axolotl_is_playing_dead(),
            "a hit whose own roll stream fires must set the play-dead window"
        );
    }

    /// **Control**: the identical fixture and winning-roll damage against a
    /// dry axolotl (`flat_world`, no water) must never play dead — proving
    /// `in_water()` is a real gate here, not dead code the roll bypasses.
    #[test]
    fn a_dry_axolotl_never_plays_dead_even_on_a_winning_roll() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:axolotl").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.get_mut(id).expect("alive").set_health(100.0);
        assert!(!sim.get(id).expect("alive").in_water(), "precondition: dry ground");

        let health_bits = 100.0_f32.to_bits();
        let raw_damage = (1..30)
            .map(|d| d as f32)
            .find(|&d| {
                let (a, b) = axolotl_play_dead_roll(id as u64, health_bits, d.to_bits());
                a == 0 && (b as f32) < d
            })
            .expect("a 1-in-3 draw over 29 tries must fire at least once");

        sim.attack(id, Vec3::new(1.0, 0.0, 0.0), raw_damage, DamageFlags::default(), 0.0)
            .expect("the axolotl is a live target");

        assert!(
            !sim.get(id).expect("alive").axolotl_is_playing_dead(),
            "a dry axolotl must never enter the play-dead window regardless of the roll"
        );
    }

    /// Random-sitting camel behaviour, gated through the real production
    /// tick path (`MobSim::tick`, the loop `camel_random_sitting` is called
    /// from) rather than by calling the function on a bare `SimMob`. A camel
    /// left alone long enough must eventually sit down — proving both the
    /// state flip and that it reaches the wire as the real sitting-pose
    /// ordinal (`10`) `MetadataField::Pose` already carries for the warden,
    /// not merely a private bookkeeping bool nothing reads.
    ///
    /// The wait is intentionally generous (`CAMEL_RANDOM_SITTING_MIN_TICKS`
    /// eligibility plus a healthy multiple of `camel_sit_roll`'s ~1-in-2400
    /// expected wait): this is a deterministic hash of `(id, tick_count)`,
    /// not real-clock timing, so the same seed always produces the same
    /// outcome on every run — there is nothing here for a busy machine to
    /// perturb, unlike the timing hazards this crate's own docs warn about
    /// elsewhere.
    #[test]
    fn a_camel_left_alone_eventually_sits_and_reports_the_real_pose() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:camel").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        assert!(
            !sim.get(id).expect("alive").camel_is_sitting(),
            "precondition: a freshly spawned camel is standing"
        );
        assert!(
            !sim.get(id)
                .expect("alive")
                .snapshot()
                .metadata
                .contains(&MetadataField::Pose(CAMEL_POSE_SITTING)),
            "precondition: a standing camel must not report the sitting pose"
        );

        let mut sat = false;
        for _ in 0..20_000 {
            sim.tick();
            if sim.get(id).expect("alive").camel_is_sitting() {
                sat = true;
                break;
            }
        }
        assert!(sat, "a camel left alone for 20,000 ticks never sat down");
        assert!(
            sim.get(id)
                .expect("alive")
                .snapshot()
                .metadata
                .contains(&MetadataField::Pose(CAMEL_POSE_SITTING)),
            "a sitting camel must report the real sitting-pose ordinal to the client"
        );
    }

    /// **Control**: the identical wait against a cow, a species
    /// `MobSim::snapshot`'s camel branch never touches — proves the pose
    /// report is camel-specific rather than every mob eventually gaining a
    /// `Pose` field this test's positive twin could not, by itself,
    /// distinguish from a species-blind bug in the metadata builder.
    #[test]
    fn only_a_camel_ever_reports_a_sitting_pose_a_cow_never_does() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:cow").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        for _ in 0..20_000 {
            sim.tick();
            assert!(
                !sim.get(id)
                    .expect("alive")
                    .snapshot()
                    .metadata
                    .contains(&MetadataField::Pose(CAMEL_POSE_SITTING)),
                "a cow must never report the sitting pose, however long it stands around"
            );
        }
    }

    /// The camel dash path exercises the rider-jump handler and
    /// rider-jump executor end to end through the real production path —
    /// `MobSim::interact` (mounting) then `MobSim::trigger_camel_dash` (the
    /// `ServerBound::PlayerInput` jump-bit consumer's own call), not a
    /// hand-built double. Checks the whole real chain a rider drives: mount
    /// succeeds, dash starts, the wire metadata reports it, and it resets
    /// once `CAMEL_DASH_MINIMUM_DURATION_TICKS` have passed.
    #[test]
    fn camel_dash_triggers_through_a_real_mount_and_reaches_the_metadata_wire() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:camel").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        let rider = PlayerIdentity { uuid: uuid::Uuid::new_v4(), entity_id: 42 };

        let mounted = sim.interact(id, rider, None);
        assert_eq!(mounted, InteractOutcome::Mounted, "an empty-handed click on an adult camel must mount it");
        assert!(
            !sim.get(id)
                .expect("alive")
                .snapshot()
                .metadata
                .contains(&MetadataField::Dash(true)),
            "precondition: a freshly mounted camel is not yet dashing"
        );

        assert!(
            sim.trigger_camel_dash(rider.entity_id),
            "a jump press aboard a fresh camel (cooldown already at zero) must start a dash"
        );
        assert!(
            sim.get(id)
                .expect("alive")
                .snapshot()
                .metadata
                .contains(&MetadataField::Dash(true)),
            "a dashing camel must report the dash metadata flag as true to the wire"
        );

        for _ in 0..CAMEL_DASH_MINIMUM_DURATION_TICKS {
            sim.tick();
        }
        assert!(
            sim.get(id)
                .expect("alive")
                .snapshot()
                .metadata
                .contains(&MetadataField::Dash(false)),
            "the dash flag must reset to false once the minimum duration elapses — the client \
             must see the reset, not merely stop seeing `true`"
        );
    }

    /// Vanilla's own rider-jump handler's cooldown-at-or-below-zero gate: a second jump press
    /// while still cooling down must not restart the dash window, and the
    /// full 55-tick cooldown (`CAMEL_DASH_COOLDOWN_TICKS`) must actually
    /// elapse — not merely the 5-tick minimum duration — before a third
    /// press succeeds. A magnitude check on the real constant, not a
    /// direction-only assertion.
    #[test]
    fn camel_dash_cannot_retrigger_until_the_full_cooldown_elapses() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:camel").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        let rider = PlayerIdentity { uuid: uuid::Uuid::new_v4(), entity_id: 42 };
        assert_eq!(sim.interact(id, rider, None), InteractOutcome::Mounted);

        assert!(sim.trigger_camel_dash(rider.entity_id), "first press must dash");
        assert!(
            !sim.trigger_camel_dash(rider.entity_id),
            "an immediate second press must not restart the cooldown"
        );

        for _ in 0..(CAMEL_DASH_COOLDOWN_TICKS - 1) {
            sim.tick();
            assert!(
                !sim.trigger_camel_dash(rider.entity_id),
                "a press before the full cooldown has elapsed must keep failing"
            );
        }
        sim.tick();
        assert!(
            sim.trigger_camel_dash(rider.entity_id),
            "a press once the full {CAMEL_DASH_COOLDOWN_TICKS}-tick cooldown has elapsed must succeed"
        );
    }

    /// **Controls**: a baby camel refuses to mount at all
    /// (vanilla's own camel interaction override's own "is not a baby" gate), and a species that
    /// bypasses `MobSim::interact` entirely — mounted directly through the
    /// low-level, species-blind `mount_mob` — still never dashes, proving
    /// `trigger_camel_dash`'s own species check is real and not merely
    /// inherited from the mount path.
    #[test]
    fn a_baby_camel_refuses_to_mount_and_a_mounted_cow_never_dashes() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);

        let baby_key = ResourceKey::from_str("minecraft:camel").expect("valid key");
        let baby_id = sim.spawn_species(baby_key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.get_mut(baby_id).expect("alive").set_age(BABY_START_AGE);
        let rider = PlayerIdentity { uuid: uuid::Uuid::new_v4(), entity_id: 42 };
        assert_eq!(
            sim.interact(baby_id, rider, None),
            InteractOutcome::Pass,
            "a baby camel must refuse an empty-handed mount attempt"
        );

        let cow_key = ResourceKey::from_str("minecraft:cow").expect("valid key");
        let cow_id = sim.spawn_species(cow_key, Vec3::new(5.0, 0.0, 0.0)).id();
        let cow_rider = PlayerIdentity { uuid: uuid::Uuid::new_v4(), entity_id: 43 };
        assert!(
            sim.mount_mob(cow_id, cow_rider.entity_id),
            "the low-level mount primitive is species-blind by its own doc"
        );
        assert!(
            !sim.trigger_camel_dash(cow_rider.entity_id),
            "a mounted cow must never dash — `trigger_camel_dash` must check the species itself"
        );
    }

    /// Ominous-bottle producer: a pillager patrol leader killed while
    /// **not** a member of any active raid must drop `minecraft:ominous_bottle`.
    /// Three controls on the same death path, each isolating one clause of
    /// the predicate the real one needs both halves of:
    ///
    /// * a pillager that is a captain but **is** in an active raid must not
    ///   drop one (`hasRaid` flips the gate),
    /// * a pillager that is **not** a captain must not drop one even
    ///   uninvolved in any raid (`isCaptain` flips the gate),
    /// * a vindicator patrol leader must not drop one — only
    ///   `entities/pillager.json` carries this loot pool in vanilla, even
    ///   though vindicators can lead patrols too.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_pillager_patrol_captain_without_a_raid_drops_an_ominous_bottle() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:pillager").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.get_mut(id).expect("alive").set_patrol_leader(true);
        sim.get_mut(id).expect("alive").set_health(1.0);

        let outcome = sim
            .attack(id, Vec3::new(1.0, 0.0, 0.0), 100.0, DamageFlags::default(), 0.0)
            .expect("the pillager is a live target");
        assert!(outcome.killed);

        let dropped = sim.dropped_items();
        assert!(
            dropped.iter().any(|(item, count)| item == "minecraft:ominous_bottle" && *count == 1),
            "a patrol captain with no active raid must drop exactly one ominous bottle: {dropped:?}"
        );
    }

    /// **Control**: a pillager captain that belongs to an active raid must
    /// not drop the bottle — `hasRaid` half of `CAPTAIN_WITHOUT_RAID`.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_pillager_captain_inside_an_active_raid_drops_no_ominous_bottle() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let raid_id = sim.start_raid(Vec3::new(0.0, 0.0, 0.0), Difficulty::Easy, 1).expect("Easy is not Peaceful");
        let key = ResourceKey::from_str("minecraft:pillager").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.get_mut(id).expect("alive").set_patrol_leader(true);
        sim.get_mut(id).expect("alive").set_health(1.0);
        // Puts `id` on the raid's own raider list — the exact state
        // `raid_containing_raider` reads to decide `hasRaid`.
        sim.raids.get_mut(&raid_id).expect("just started").raiders.push(id);

        let outcome = sim
            .attack(id, Vec3::new(1.0, 0.0, 0.0), 100.0, DamageFlags::default(), 0.0)
            .expect("the pillager is a live target");
        assert!(outcome.killed);

        let dropped = sim.dropped_items();
        assert!(
            !dropped.iter().any(|(item, _)| item == "minecraft:ominous_bottle"),
            "a captain that belongs to an active raid must not drop the bottle: {dropped:?}"
        );
    }

    /// **Control**: a non-captain pillager, involved in no raid, drops no
    /// bottle — `isCaptain` half of `CAPTAIN_WITHOUT_RAID`.
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_non_captain_pillager_drops_no_ominous_bottle() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:pillager").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.get_mut(id).expect("alive").set_health(1.0);
        assert!(!sim.get(id).expect("alive").is_patrol_leader(), "control precondition: not a leader");

        let outcome = sim
            .attack(id, Vec3::new(1.0, 0.0, 0.0), 100.0, DamageFlags::default(), 0.0)
            .expect("the pillager is a live target");
        assert!(outcome.killed);

        let dropped = sim.dropped_items();
        assert!(
            !dropped.iter().any(|(item, _)| item == "minecraft:ominous_bottle"),
            "a rank-and-file pillager must not drop the bottle: {dropped:?}"
        );
    }

    /// **Control**: a vindicator patrol leader, involved in no raid, drops
    /// no bottle — only `entities/pillager.json` carries this loot pool in
    /// vanilla, even though a vindicator can lead a patrol too
    /// (`PatrollingMonster` is not species-specific).
    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn a_vindicator_patrol_leader_drops_no_ominous_bottle() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:vindicator").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        sim.get_mut(id).expect("alive").set_patrol_leader(true);
        sim.get_mut(id).expect("alive").set_health(1.0);

        let outcome = sim
            .attack(id, Vec3::new(1.0, 0.0, 0.0), 100.0, DamageFlags::default(), 0.0)
            .expect("the vindicator is a live target");
        assert!(outcome.killed);

        let dropped = sim.dropped_items();
        assert!(
            !dropped.iter().any(|(item, _)| item == "minecraft:ominous_bottle"),
            "only a pillager carries this loot pool in vanilla: {dropped:?}"
        );
    }

    const TICKS: usize = 80;

    /// **Control for the attribute fallback.**
    ///
    /// The miss case for `follow_range` is **32.0**, not `0.0`. `attr`'s
    /// `unwrap_or(0.0)` reads like the fallback and is unreachable for any
    /// attribute the registry knows, because `AttributeMap::value` already
    /// substitutes `default_def(key).default` for an absent instance.
    ///
    /// The distinction determines what a useful guard can test. A guard of the shape
    /// `if r > 0.0 { r } else { DEFAULT }` is **dead code** — it never fires, and
    /// an unlisted species keeps the registry's 32.0, which is precisely the one
    /// number `follow_range` never legitimately holds (the generic
    /// mob attribute builder
    /// overrides it to 16.0 for every mob). The wrong value sits *inside* the
    /// plausible range, so only instance presence can detect the miss.
    ///
    /// Predicted from the `follow_range` default definition and
    /// `AttributeMap::value`'s `else` branch, then measured. If this ever reads
    /// 0.0, `attr` changed and the `attr_present` split is redundant.
    #[test]
    fn control_the_attribute_lookup_misses_to_the_registry_default_not_zero() {
        // **Structurally** unlistable, not merely unlisted.
        //
        // This id is outside the `minecraft` namespace, so
        // `default_attributes` must return `None` before it consults
        // `type_spec`. The miss case is structural rather than dependent on
        // which species tables are populated.
        let unlisted = Identifier::from_str("modded:not_a_vanilla_mob").expect("valid id");
        assert!(
            default_attributes(&unlisted).is_none(),
            "default_attributes must answer None outside the minecraft namespace, \
             or the miss case below is not reachable at all"
        );

        let empty = AttributeMap::new();
        assert_eq!(
            attr(&empty, "follow_range"),
            32.0,
            "the miss case is the registry default, so a `> 0.0` guard can never fire"
        );
        assert_eq!(
            attr_present(&empty, "follow_range"),
            None,
            "instance presence is the only reading that can see the miss"
        );

        // A listed case carries its own value, so the split above does not
        // discard every attribute.
        let zombie = default_attributes(&Identifier::from_str("minecraft:zombie").unwrap())
            .expect("zombie has a type_spec arm");
        assert_eq!(
            attr_present(&zombie, "follow_range"),
            Some(35.0),
            "vanilla's own zombie attribute builder sets FOLLOW_RANGE to 35.0"
        );
    }

    /// **The gate.** A zombie must acquire at its real 35.0, which requires
    /// separating 35 from *both* wrong candidates rather than merely showing that
    /// targeting works at all.
    ///
    /// | distance | expected | what it rules out |
    /// |---|---|---|
    /// | 20 | acquires | `DEFAULT_FOLLOW_RANGE` 16.0 (the pre-fix value) |
    /// | 34 | acquires | the registry's 32.0 as well |
    /// | 36 | **no** | an unbounded feed, and blaze/enderman's 48/64 |
    ///
    /// Asserting only "a zombie acquires a nearby player" passes at 16, at 32 and
    /// at 35 alike, which is the magnitude-species vacuous test: right subject,
    /// predicate too weak to distinguish the hypotheses.
    #[test]
    fn a_zombie_acquires_at_its_real_follow_range_not_16_or_32() {
        assert!(
            acquires_at("zombie", 20.0, TICKS),
            "a zombie must acquire a player at 20 blocks; failing here means the \
             controller is still on DEFAULT_FOLLOW_RANGE (16.0) and #455's host \
             half never landed"
        );
        assert!(
            acquires_at("zombie", 34.0, TICKS),
            "a zombie must acquire at 34 blocks — inside its real 35.0 but outside \
             both 16.0 and the registry's 32.0, so this is the assertion that pins \
             the value rather than merely the wiring"
        );
        assert!(
            !acquires_at("zombie", 36.0, TICKS),
            "a zombie must NOT acquire at 36 blocks: the cut is real and bounded at \
             35.0, not merely large. Without this the gate above passes for any \
             range >= 34, including an unbounded feed"
        );
    }

    /// The unlisted-species fallback is observable at spawn time.
    ///
    /// A fixed per-mob seed keeps the acquisition boundary stable: the
    /// 15-block hit succeeds and the 17-block hit fails.
    ///
    /// The fallback case uses an id outside the roster. Such an id has no
    /// target goal, so this assertion measures the range installed at spawn
    /// rather than target acquisition.
    ///
    /// [`MobSim::spawn_species`] reads
    /// `attr_present(…).unwrap_or(DEFAULT_FOLLOW_RANGE)` for any key. The
    /// test checks the generic fallback at the spawn seam directly.
    #[test]
    fn an_unlisted_species_still_falls_back_at_the_spawn_path() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        // Outside the `minecraft` namespace, so `default_attributes` answers
        // `None` structurally — see the control above.
        let key = ResourceKey::from_str("modded:not_a_vanilla_mob").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        let got = MobController::follow_range(&sim.get(id).expect("alive").mob);
        assert_eq!(
            got, DEFAULT_FOLLOW_RANGE,
            "an unlisted species must fall back to vanilla's own generic mob \
             attribute builder's 16.0"
        );
        assert_ne!(
            got, 32.0,
            "32.0 is the registry default and the one value follow_range never \
             legitimately holds — reading it here means `attr`'s registry \
             fallback is reaching the controller, the exact defect #455's \
             brokered patch would have left in place"
        );

        // Control: a *listed* species must read its own jar value through the
        // same accessor, so the assertions above are a property of the fallback
        // and not of `follow_range` always answering 16.
        let zombie = ResourceKey::from_str("minecraft:zombie").expect("valid key");
        let zid = sim.spawn_species(zombie, Vec3::new(2.0, 0.0, 0.0)).id();
        assert_eq!(
            MobController::follow_range(&sim.get(zid).expect("alive").mob),
            35.0,
            "vanilla's own zombie attribute builder — if this also reads 16.0 \
             the accessor is not observing what spawn_species installed"
        );
    }
}

/// Host-resolved persistent-anger deadline tests.
#[cfg(test)]
mod anger_tests {
    use super::*;

    /// The grudge window in ticks, stated **independently of [`ANGER_TICKS`]**.
    /// Twenty-to-thirty-nine seconds at 20 ticks per second yields the
    /// inclusive range `[400, 780]`.
    ///
    /// These literals are load-bearing: reading the seconds as ticks would
    /// produce `[20, 39]`, and deriving them from `ANGER_TICKS` would allow a
    /// bad duration to move both the implementation and its expectation.
    const JAR_LO: u64 = 400;
    const JAR_HI: u64 = 780;

    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    /// Spawns one mob through [`MobSim::spawn_species`], hits it once, and
    /// reports the tick offset at which `angry_target` first reads `None`.
    ///
    /// Drives `MobSim` through its normal perception path, keeping the
    /// AI-goal and spawn-category behavior under test.
    ///
    /// The attacker position is placed well outside `flat_world`'s solid `±8`
    /// platform. The attacker remains outside the walkable platform, so the bee
    /// cannot path to it or clear `anger` through an attack. This function
    /// measures **grudge duration**, not "does the
    /// mob's own combat ever run" — an attacker outside the walkable platform
    /// (so no path exists to it, for any of the four species' plausible
    /// speeds) decouples the two.
    fn ticks_until_anger_clears(species: &str, limit: u64) -> Option<u64> {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str(&format!("minecraft:{species}")).expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        let attacker = Vec3::new(128.0, 0.0, 0.0);
        sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
            .expect("the mob must still be alive to hold a grudge");

        // One tick to run the feed, then poll.
        for elapsed in 0..limit {
            sim.tick();
            if sim.get(id).expect("alive").mob.angry_target().is_none() {
                return Some(elapsed);
            }
        }
        None
    }

    /// **The gate.** A grudge must expire inside the measured `[400, 780]`
    /// tick window. Twenty-to-thirty-nine seconds at 20 ticks per second
    /// yields this range; treating the seconds as ticks would yield `[20, 39]`.
    ///
    /// Predicting only "it eventually expires" is satisfied by both hypotheses
    /// and by an off-by-one on the inclusive upper bound. Both bounds are
    /// asserted so the inclusive interval is checked directly.
    #[test]
    fn anger_expires_inside_the_jars_tick_window() {
        let (lo, hi) = (JAR_LO, JAR_HI);
        // Generous headroom over `hi`, so "never expired" is distinguishable
        // from "expired late" rather than both timing out.
        let limit = hi * 2;

        for species in ["wolf", "bee", "enderman", "zombified_piglin"] {
            let elapsed = ticks_until_anger_clears(species, limit).unwrap_or_else(|| {
                panic!("{species}'s grudge never expired within {limit} ticks")
            });
            assert!(
                elapsed >= lo,
                "{species}'s grudge expired after {elapsed} ticks, before the jar's \
                 minimum of {lo}. A value in [20, 39] means rangeOfSeconds(20, 39) \
                 was read as seconds; it already returns ticks"
            );
            assert!(
                elapsed <= hi,
                "{species}'s grudge lasted {elapsed} ticks, past the jar's maximum \
                 of {hi}"
            );
        }
    }

    /// The grudge must be **live** immediately after the hit, and must name the
    /// attacker's position — not merely be non-`None` at some later point.
    ///
    /// Control for the test above: without this, a mob whose anger was never
    /// set at all would "expire" at tick 0 and only the lower-bound assertion
    /// would catch it, for the wrong reason.
    #[test]
    fn a_hit_starts_a_grudge_naming_the_attacker() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:wolf").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        assert_eq!(
            sim.get(id).expect("alive").mob.angry_target(),
            None,
            "an unprovoked neutral mob must hold no grudge — if this is Some, \
             every neutral species is hostile on sight"
        );

        let attacker = Vec3::new(3.0, 0.0, 4.0);
        sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
            .expect("alive");
        sim.tick();

        assert_eq!(
            sim.get(id).expect("alive").mob.angry_target(),
            Some(attacker),
            "the grudge must name where the attacker was"
        );
    }

    /// The deadline is **absolute**, so a grudge refreshed by a second hit
    /// extends from the *new* tick rather than from the first.
    ///
    /// This is the assertion a decrementing counter passes only by accident:
    /// it pins that the stored value is compared against `tick_count` rather
    /// than decremented, by advancing the clock a long way between two hits and
    /// requiring the grudge to outlive the first deadline's worst case.
    #[test]
    fn a_second_hit_extends_the_deadline_from_the_new_tick() {
        let (lo, hi) = (JAR_LO, JAR_HI);
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:wolf").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        let attacker = Vec3::new(1.0, 0.0, 0.0);
        sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
            .expect("alive");
        // Advance well past the first grudge's *minimum* but not its maximum,
        // then hit again.
        for _ in 0..lo {
            sim.tick();
        }
        sim.attack(id, attacker, 1.0, DamageFlags::default(), 0.0)
            .expect("alive");

        // The refreshed grudge must still be live `lo` ticks later, which the
        // first grudge could not guarantee: its worst case was `hi`, and we are
        // now at `lo + lo = 800 > hi`.
        for _ in 0..lo {
            sim.tick();
        }
        assert!(
            lo + lo > hi,
            "this test's arithmetic assumes 2*{lo} exceeds {hi}; if the window \
             changed, the schedule below no longer proves anything"
        );
        assert_eq!(
            sim.get(id).expect("alive").mob.angry_target(),
            Some(attacker),
            "the second hit must extend the deadline from the tick it landed on; \
             a grudge that has already expired here means the deadline was not \
             recomputed against the current clock"
        );
    }

    /// Vanilla's own zombified-piglin alert-interval's own `[80, 120]` window, stated
    /// independently of [`PIGLIN_ALERT_INTERVAL_TICKS`] for the same reason
    /// [`JAR_LO`]/[`JAR_HI`] are stated independently of [`ANGER_TICKS`]
    /// above: a magnitude check that read the expectation off the constant
    /// under test would pass even if the constant itself were wrong. Drawn
    /// from a real spawned mob's own RNG stream (never a hand-rolled double),
    /// the same [`MobController`] seam production reads.
    #[test]
    fn piglin_alert_interval_rolls_inside_the_jars_80_to_120_window() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:zombified_piglin").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        for _ in 0..1000 {
            let draw = piglin_alert_interval(&mut sim.get_mut(id).expect("alive").mob);
            assert!(
                (80..=120).contains(&draw),
                "piglin_alert_interval drew {draw}, outside the jar's [80, 120] \
                 ALERT_INTERVAL window"
            );
        }
    }

    /// The ongoing piglin group-alert mechanism is isolated from the one-shot
    /// owner-group propagation it accompanies.
    ///
    /// The one-shot arm fires when [`MobSim::attack`] creates a new grudge.
    /// The neighbour is spawned after that event, so it cannot receive the
    /// one-shot notification. An ensuing grudge therefore demonstrates the
    /// periodic alert timer rather than the immediate group notification.
    #[test]
    fn a_piglin_holding_a_target_alerts_a_neighbour_that_did_not_exist_for_the_one_shot_alert() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:zombified_piglin").expect("valid key");

        let alerting = sim.spawn_species(key.clone(), Vec3::new(0.0, 0.0, 0.0)).id();
        let attacker = Vec3::new(3.0, 0.0, 4.0);
        sim.attack(alerting, attacker, 1.0, DamageFlags::default(), 0.0)
            .expect("alive");

        // The neighbour did not exist for the hit above, so the immediate
        // group census could not have reached it.
        let neighbour = sim.spawn_species(key, Vec3::new(5.0, 0.0, 0.0)).id();
        assert_eq!(
            sim.get(neighbour).expect("alive").mob.angry_target(),
            None,
            "precondition: a freshly spawned neighbour must start with no grudge"
        );

        // 120 ticks covers the interval's own worst case
        // (`PIGLIN_ALERT_INTERVAL_TICKS`'s upper bound), plus headroom for the
        // alerting piglin's anger-gated target row to turn its grudge into a
        // real `attack_target` (what `piglin_alert_ticks` reads to decide it
        // has something to alert about).
        for _ in 0..200 {
            sim.tick();
            if sim
                .get(neighbour)
                .expect("alive")
                .mob
                .angry_target()
                .is_some()
            {
                return;
            }
        }
        panic!(
            "the neighbour was never alerted within 200 ticks — the ongoing \
             maybeAlertOthers mechanism has no live producer"
        );
    }
}

/// MobSim seam primitive tests (instant relocation / self-damage / target
/// identity): the host half of the seam primitives in `lodestone-entity`. The
/// gaze feed is not supplied by this seam — see
/// [`PlayerPerception`]'s lack of a view vector.
#[cfg(test)]
mod primitives_tests {
    use super::*;

    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

/// A host teleport command rewrites position immediately and
    /// survives the next tick — an instant relocation, not a fast walk.
    #[test]
    fn teleport_to_moves_the_mob_instantly_and_survives_a_tick() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:enderman").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();

        // Inside this module's own solid floor (`-8..=8` on both axes) —
        // `MobSim::teleport_to` (unlike the goal-driven enderman blink) is
        // the raw, unvalidated primitive and always lands exactly on
        // target, but a target with no ground under it would now correctly
        // start falling on the very next tick (idle mobs have real gravity,
        // see `NavigatingMob::advance`), which is not what this test means
        // to exercise.
        let target = Vec3::new(5.0, 0.0, 5.0);
        sim.get_mut(id).expect("alive").teleport_to(target);
        assert_eq!(
            sim.position(id),
            Some(target),
            "teleport must move the mob to exactly the target"
        );

        sim.tick();
        assert_eq!(
            sim.position(id),
            Some(target),
            "a tick after teleport must not undo it"
        );
    }

    /// A `damage_self` request is drained by [`MobSim::tick`] and resolved into
    /// real health change. A bee that damages itself for its full health is
    /// gone at the end of the same tick.
    #[test]
    fn damage_self_is_resolved_into_a_real_self_kill() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let key = ResourceKey::from_str("minecraft:bee").expect("valid key");
        let id = sim.spawn_species(key, Vec3::new(0.0, 0.0, 0.0)).id();
        let health = sim.get(id).expect("alive").health();

        sim.get_mut(id).expect("alive").damage_self(health);
        assert_eq!(
            sim.get(id).expect("alive").health(),
            health,
            "the request alone must not change health — only the tick drain resolves it"
        );
        sim.tick();
        assert!(
            sim.get(id).is_none(),
            "a mob that damaged itself for its full health must be removed by \
             the end of the tick"
        );
    }

    /// An owner id set on the host resolves to an owner *position*
    /// across the seam each tick.
    #[test]
    fn owner_id_resolves_to_an_owner_position_across_the_seam() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let wolf = ResourceKey::from_str("minecraft:wolf").expect("valid key");
        let owner_id = sim.spawn_species(wolf.clone(), Vec3::new(0.0, 0.0, 0.0)).id();
        let pet_id = sim.spawn_species(wolf, Vec3::new(3.0, 0.0, 3.0)).id();
        sim.get_mut(pet_id).expect("alive").set_owner_id(Some(owner_id));

        assert_eq!(
            sim.get(pet_id).expect("alive").owner_position(),
            None,
            "before the first tick the seam has not resolved the owner"
        );

        sim.tick();
        let owner_pos = sim.get(owner_id).expect("alive").position();
        assert_eq!(
            sim.get(pet_id).expect("alive").owner_position(),
            Some(owner_pos),
            "the feed must resolve the owner id to the owner's current position"
        );
    }

    /// The gaze feed reaches `is_being_stared_at` through the integrated
    /// simulation, not only through isolated entity-level checks.
    ///
    /// **The discriminating pair**: two sims, each with one enderman at the
    /// *identical* position and one player at the *identical* position — the
    /// only difference is the player's `view_direction`. A gate that only
    /// varied position (closer/farther) could not tell a real gaze test from
    /// a distance check; this one cannot vary anything else, because nothing
    /// else differs.
    #[test]
    fn the_gaze_feed_reaches_is_being_stared_at_and_a_look_away_does_not() {
        let world = flat_world();
        let player_pos = Vec3::new(0.0, 0.0, 0.0);
        let enderman_pos = Vec3::new(0.0, 0.0, 10.0);

        // Resolve the real eye positions the feed itself uses, rather than
        // guessing them — `feed_perception`'s own formula for the mob eye
        // (`height * 0.85`) and `PLAYER_EYE_HEIGHT` for the player.
        let mut probe = MobSim::new(&world);
        let probe_id = probe
            .spawn_species(ResourceKey::from_str("minecraft:enderman").expect("valid key"), enderman_pos)
            .id();
        let mob_eye_height = f64::from(probe.get(probe_id).expect("spawned").shape().height) * 0.85;
        let mob_eye = Vec3::new(enderman_pos.x, enderman_pos.y + mob_eye_height, enderman_pos.z);
        let player_eye = Vec3::new(player_pos.x, player_pos.y + PLAYER_EYE_HEIGHT, player_pos.z);
        let delta = Vec3::new(mob_eye.x - player_eye.x, mob_eye.y - player_eye.y, mob_eye.z - player_eye.z);
        let dist = (delta.x * delta.x + delta.y * delta.y + delta.z * delta.z).sqrt();
        let looking_at = Vec3::new(delta.x / dist, delta.y / dist, delta.z / dist);
        // Exactly opposite the enderman — as far outside the cone as a unit
        // vector can be (`dot == -1`), not a near-miss.
        let looking_away = Vec3::new(-looking_at.x, -looking_at.y, -looking_at.z);

        // The naive (non-distance-adjusted) hypothesis this feed's own doc
        // warns against: reading `coneSize` (0.025) as the tolerance
        // directly gives threshold `1.0 - 0.025 = 0.975`. At `looking_at`
        // (`dot == 1.0`) both the naive and the real (`1.0 - 0.025/dist`)
        // hypotheses agree — accepted either way — which is exactly why the
        // boundary case belongs to `lodestone_entity`'s own
        // `is_in_view_cone_boundary_at_the_endermans_own_cone_size` gate and
        // not here; this test's job is only "does the feed reach the goal
        // at all", which a dead-on look and a dead-opposite one already
        // settle without needing a razor's-edge input.
        assert!(dist > 1.0, "the fixture must not degenerate to zero distance: dist={dist}");

        let mut watched = MobSim::new(&world);
        let watched_id = watched
            .spawn_species(ResourceKey::from_str("minecraft:enderman").expect("valid key"), enderman_pos)
            .id();
        watched.set_players(vec![PlayerPerception {
            position: player_pos,
            held_item: None,
            view_direction: looking_at,
        }]);
        watched.tick();
        assert!(
            watched.get(watched_id).expect("alive").mob.is_being_stared_at(),
            "a player looking straight at the enderman must set is_being_stared_at"
        );

        let mut unwatched = MobSim::new(&world);
        let unwatched_id = unwatched
            .spawn_species(ResourceKey::from_str("minecraft:enderman").expect("valid key"), enderman_pos)
            .id();
        unwatched.set_players(vec![PlayerPerception {
            position: player_pos,
            held_item: None,
            view_direction: looking_away,
        }]);
        unwatched.tick();
        assert!(
            !unwatched.get(unwatched_id).expect("alive").mob.is_being_stared_at(),
            "a player looking directly away, from the identical position, must not"
        );
    }
}

/// Block-identity cues read from generated tag data, and the graze handoff out
/// of an immutably borrowed world.
#[cfg(test)]
mod block_cues_tests {
    use super::*;

    /// The jar's real `#minecraft:edible_for_sheep` membership
    /// (`data/minecraft/tags/block/edible_for_sheep.json`), transcribed here
    /// **only as the expectation**. The implementation does not contain this
    /// list — it resolves the tag through `lodestone_data::tool`, which is
    /// generated from the jar — so this is an independent statement of the answer
    /// rather than a restatement of the code under test.
    const JAR_EDIBLE: &[&str] = &[
        "minecraft:short_grass",
        "minecraft:short_dry_grass",
        "minecraft:tall_dry_grass",
        "minecraft:fern",
    ];

    /// A single cell of `block` with air around it, at a fixed position.
    fn world_of(block: &str) -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        world.set_block(0, 0, 0, block);
        world
    }

    /// **The gate that a hand-written tag list fails.**
    ///
    /// Every member of the jar's tag must classify as edible. Three of the four
    /// would have been missed by the obvious `short_grass | tall_grass` guess:
    /// `short_dry_grass`, `tall_dry_grass` and `fern`. A sheep would have refused
    /// to graze a fern, and no test in the tree would have said so.
    #[test]
    fn every_jar_tag_member_classifies_as_edible_for_sheep() {
        for block in JAR_EDIBLE {
            let world = world_of(block);
            assert!(
                world.block_cues(0, 0, 0).edible_for_sheep,
                "{block} is in #minecraft:edible_for_sheep and must classify as edible — \
                 a hand-written list missing it is exactly how this stays silently wrong"
            );
        }
    }

    /// **The other half of the same mistake: the guess's false positive.**
    ///
    /// `tall_grass` is *not* in `#minecraft:edible_for_sheep` — the jar tag has
    /// four entries and that is not one of them. It is the block most likely to be
    /// added by anyone writing the list from memory, and asserting only the
    /// positives above would let it through.
    #[test]
    fn tall_grass_is_not_edible_for_sheep_despite_looking_like_it_should_be() {
        let world = world_of("minecraft:tall_grass");
        assert!(
            !world.block_cues(0, 0, 0).edible_for_sheep,
            "minecraft:tall_grass is absent from the jar's edible_for_sheep tag; \
             classifying it as edible means the tag is being guessed, not read"
        );
    }

    /// `grass_block` is the *equality* cue, not a tag member — vanilla's own
    /// "eat block" goal tests it
    /// with block equality. So it must set
    /// `grass_block` and must **not** set `edible_for_sheep`: a sheep standing on
    /// grass eats the block below, a sheep standing in short grass eats the block
    /// at its feet, and conflating the two would make either mechanism fire in the
    /// wrong place.
    #[test]
    fn grass_block_is_the_equality_cue_and_not_a_tag_member() {
        let cues = world_of("minecraft:grass_block").block_cues(0, 0, 0);
        assert!(cues.grass_block, "grass_block must set its own cue");
        assert!(
            !cues.edible_for_sheep,
            "grass_block is not in the edible tag — the two cues are independent"
        );
    }

    /// The negative control. Ordinary blocks and air must set neither cue,
    /// otherwise the positives above are satisfied by a classifier that says yes
    /// to everything.
    #[test]
    fn control_ordinary_blocks_set_no_cue_at_all() {
        for block in ["minecraft:stone", "minecraft:dirt", "minecraft:oak_log"] {
            let cues = world_of(block).block_cues(0, 0, 0);
            assert!(
                !cues.edible_for_sheep && !cues.grass_block,
                "{block} must set no cue; a classifier that says yes to everything \
                 passes every positive assertion above"
            );
        }
    }

    /// Property strings must not defeat the lookup: `block_state` yields a full
    /// state string, so a cue keyed on the raw string would miss any block with
    /// properties. `tall_dry_grass` is a real tag member *and* carries a
    /// `half`/`facing`-style property list in some states, which is why this is a
    /// distinct case rather than a restatement of the first test.
    #[test]
    fn a_state_with_properties_still_classifies() {
        let mut world = ChunkWorld::new(-64, 384);
        world.set_block(0, 0, 0, "minecraft:short_grass");
        assert!(world.block_cues(0, 0, 0).edible_for_sheep);
        // The `grass_block` arm goes through the same property strip.
        world.set_block(0, 1, 0, "minecraft:grass_block[snowy=false]");
        assert!(
            world.block_cues(0, 1, 0).grass_block,
            "a state with a property list must still match the equality cue — \
             `block_state` returns the full string, properties included"
        );
    }

    /// **The handoff gate.** A grazing mob's eat must survive `MobSim::tick` and
    /// emerge from [`MobSim::take_grazes`].
    ///
    /// The test supplies the goal directly, so the assertion covers only the
    /// `take_new_eaten` → `pending_grazes` → `take_grazes` handoff. The
    /// production roster intentionally has no sheep-eating goal.
    ///
    /// It is deliberately **not** an assertion about the eat interval. That is
    /// `lodestone-entity`'s `block_perception.rs` gate, which distinguishes 444
    /// predicted eats from 286 — and which also recorded that a rate measured in a
    /// mutating world measures grass scarcity instead. Nothing drains the world
    /// here, so supply is infinite and the tick budget only has to make "at least
    /// one eat" overwhelmingly likely: at the halved 1-in-500 adult interval,
    /// 20,000 ticks puts the probability of zero at about e^-40.
    #[test]
    fn a_grazing_mob_hands_its_eat_to_the_driver() {
        let mut world = ChunkWorld::new(-64, 384);
        // Grass to stand on, short grass to stand in — so both cues are live and
        // whichever arm fires, the handoff is exercised.
        //
        // Wide enough that idle wandering cannot walk the sheep off it in
        // 20,000 ticks. That is not padding: at 5×5 the sheep reached the edge and
        // grazed at (-2, 0, -2), and outside the patch there is no floor at all,
        // so a narrower world tests falling rather than grazing.
        for x in -24..=24 {
            for z in -24..=24 {
                world.set_block(x, -1, z, "minecraft:grass_block");
                world.set_block(x, 0, z, "minecraft:short_grass");
            }
        }

        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                ResourceKey::from_str("minecraft:sheep").expect("valid key"),
                Vec3::new(0.5, 0.0, 0.5),
            )
            .id();
        sim.get_mut(id).expect("just spawned").add_goal(
            5,
            Box::new(lodestone_entity::ai::goals::EatBlockGoal::new()),
        );

        assert!(
            sim.take_grazes().is_empty(),
            "precondition: nothing is pending before any tick, so the assertion \
             below cannot be satisfied by a stale entry"
        );

        let mut grazes = Vec::new();
        for _ in 0..20_000 {
            sim.tick();
            grazes.extend(sim.take_grazes());
            if !grazes.is_empty() {
                break;
            }
        }

        assert!(
            !grazes.is_empty(),
            "a sheep standing in short grass on a grass block must record an eat \
             that reaches take_grazes; empty means the handoff is broken and #238 \
             can never mutate the world"
        );
        // The recorded position must be the *mob's* cell, not the eaten block's —
        // the consumer resolves `AtFeet` as that cell and `Below` as one down, so
        // reporting the eaten cell would make the `Below` arm write dirt a block
        // too low.
        //
        // **`y` is the whole assertion.** `x`/`z` are identical for both
        // candidates, so they carry no information about which one this is; only
        // the height distinguishes the mob's feet (`0`) from the grass block it
        // stands on (`-1`). An earlier draft of this pinned the full triple to
        // `(0, 0, 0)` and failed at `(-2, 0, -2)` — idle wandering had walked
        // the sheep two blocks before it grazed, so that assertion was testing a
        // false premise (that the mob holds still) rather than the handoff.
        let (pos, _what) = grazes[0];
        assert_eq!(
            pos.y, 0,
            "the handoff must carry the mob's own feet cell (y=0), not the grass \
             block below it (y=-1) — the EatenBlock variants are relative to the mob"
        );
        assert!(
            (-24..=24).contains(&pos.x) && (-24..=24).contains(&pos.z),
            "the graze must be recorded somewhere on the prepared patch, got \
             ({}, {}) — off-patch means the sheep grazed a cell with no grass",
            pos.x,
            pos.z
        );

        // Draining really drains: a second read must not re-report the same eat,
        // or a slow consumer would apply it twice.
        assert!(
            sim.take_grazes().is_empty(),
            "take_grazes must drain, not merely read"
        );
    }
}

/// Age-scaled hitbox and baby-only movement modifier, including the
/// `species_shape`/`SimMob::set_age` path that applies `is_baby`.
#[cfg(test)]
mod baby_shape_tests {
    use super::*;

    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for z in 0..16 {
            for x in 0..16 {
                world.set_block(x, 0, z, "minecraft:stone");
            }
        }
        world
    }

    fn above_floor() -> Vec3 {
        Vec3::new(8.0, 1.0, 8.0)
    }

    /// **A baby zombie is the real `0.49×0.98` literal, not a halved adult.**
    ///
    /// `0.6×1.95` halved is `0.3×0.975` — close enough to the true value that
    /// an assertion only checking "shrank" would pass under either
    /// hypothesis. Predicting the exact literal is what separates a real
    /// `BABY_DIMENSIONS` port from the generic `getAgeScale()` fallback.
    #[test]
    fn a_baby_zombie_is_the_exact_vanilla_literal_not_a_halved_adult() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:zombie".parse().expect("valid key"), above_floor())
            .id();
        {
            let adult = sim.get(id).expect("spawned");
            assert_eq!(adult.shape().width, 0.6, "adult zombie width");
            assert_eq!(adult.shape().height, 1.95, "adult zombie height");
        }

        let mob = sim.get_mut(id).expect("spawned");
        mob.set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
        let baby = sim.get(id).expect("still spawned");
        assert_eq!(
            baby.shape().width,
            0.49,
            "baby zombie width is the literal BABY_DIMENSIONS, not 0.6 * 0.5 = 0.3"
        );
        assert_eq!(
            baby.shape().height,
            0.98,
            "baby zombie height is the literal BABY_DIMENSIONS, not 1.95 * 0.5 = 0.975"
        );
    }

    /// Growing back up re-derives the adult shape — the boundary crossing
    /// runs in both directions, not just baby-ward.
    #[test]
    fn growing_up_restores_the_adult_shape() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), above_floor())
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE)
            .id();
        assert_eq!(sim.get(id).expect("spawned").shape().width, 0.45, "baby cow width");

        sim.get_mut(id).expect("spawned").set_age(0);
        let grown = sim.get(id).expect("still spawned");
        assert!(!grown.is_baby(), "age 0 is the cooldown-free adult reading");
        assert_eq!(grown.shape().width, 0.9, "adult cow width restored");
        assert_eq!(grown.shape().height, 1.4, "adult cow height restored");
    }

    /// **Control: a species with no `baby_dimensions` entry uses the real
    /// `LivingEntity` fallback (half size), not a made-up constant.**
    ///
    /// Skeletons never naturally have babies, but `is_baby()` only reads the
    /// age counter — nothing species-gates it — so this is the discriminating
    /// input for the fallback arm specifically: a skeleton's adult box is
    /// `0.6×1.99`, and the *wrong* hypothesis (no fallback at all, i.e. the
    /// shape not changing) would leave it at `0.6×1.99` where the fallback
    /// predicts `0.3×0.995`.
    #[test]
    fn control_a_species_with_no_baby_table_entry_uses_the_generic_age_scale() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:skeleton".parse().expect("valid key"), above_floor())
            .id();
        let adult_width = sim.get(id).expect("spawned").shape().width;
        let adult_height = sim.get(id).expect("spawned").shape().height;
        assert_eq!(adult_width, 0.6, "adult skeleton width");
        assert_eq!(adult_height, 1.99, "adult skeleton height");

        sim.get_mut(id)
            .expect("spawned")
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
        let baby = sim.get(id).expect("still spawned");
        assert_eq!(
            baby.shape().width,
            adult_width * 0.5,
            "no BABY_DIMENSIONS entry falls back to LivingEntity's own 0.5 age scale"
        );
        assert_eq!(
            baby.shape().height,
            adult_height * 0.5,
            "no BABY_DIMENSIONS entry falls back to LivingEntity's own 0.5 age scale"
        );
    }

    /// **The zombie family's baby speed boost is `base * 1.5`, and a cow's
    /// stays flat** — the discriminating pair the residue's "attribute
    /// change" half asks for. `step_per_tick` now reports the AI-driven
    /// kinematic-follower rate, not the bare attribute (see
    /// `ai_ground_speed`'s own doc): predicted here from the same outside
    /// constants (vanilla's default ground friction, `0.6 * 0.91`) in a
    /// separate expression, not by calling the function under test, so a
    /// shared bug cannot cancel out. `0.23 * 1.5 = 0.345` is still the
    /// attribute-level prediction; squaring and dividing by
    /// `1 - 0.6 * 0.91` is the extra step `ai_ground_speed` adds.
    #[test]
    fn baby_zombie_speeds_up_and_baby_cow_does_not() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let friction = 1.0 - 0.6 * 0.91;
        let predicted = |attribute: f64| attribute * attribute / friction;

        let zombie_id = sim
            .spawn_species("minecraft:zombie".parse().expect("valid key"), above_floor())
            .id();
        let zombie_adult_speed = sim.get(zombie_id).expect("spawned").step_per_tick();
        assert!(
            (zombie_adult_speed - predicted(0.23)).abs() < 1e-9,
            "adult zombie ground speed must be movement_speed(0.23) squared over \
             (1 - 0.6*0.91), got {zombie_adult_speed}, predicted {}",
            predicted(0.23)
        );
        sim.get_mut(zombie_id)
            .expect("spawned")
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
        let zombie_baby_speed = sim.get(zombie_id).expect("still spawned").step_per_tick();
        assert!(
            (zombie_baby_speed - predicted(0.23 * 1.5)).abs() < 1e-9,
            "baby zombie speed must be exactly ai_ground_speed(0.23 * 1.5), got \
             {zombie_baby_speed}, predicted {}",
            predicted(0.23 * 1.5)
        );
        assert!(
            zombie_baby_speed > zombie_adult_speed,
            "the baby boost must still win after the ground-speed conversion, not \
             just at the attribute level"
        );

        let cow_id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), above_floor())
            .id();
        let cow_adult_speed = sim.get(cow_id).expect("spawned").step_per_tick();
        assert!(
            (cow_adult_speed - predicted(0.2)).abs() < 1e-9,
            "adult cow ground speed must be movement_speed(0.2) squared over \
             (1 - 0.6*0.91), got {cow_adult_speed}, predicted {}",
            predicted(0.2)
        );
        sim.get_mut(cow_id)
            .expect("spawned")
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE);
        let cow_baby_speed = sim.get(cow_id).expect("still spawned").step_per_tick();
        assert!(
            (cow_baby_speed - cow_adult_speed).abs() < 1e-9,
            "a cow has no SPEED_MODIFIER_BABY — baby speed must equal adult speed exactly"
        );
    }

    /// **Control proving `ai_ground_speed` is load-bearing, not decorative**:
    /// with the bare `movement_speed` attribute used directly (the pre-fix
    /// behaviour this repo's own evidence standards require a control for),
    /// a pig's per-tick movement step is `0.25` — noticeably higher than the
    /// `ai_ground_speed(0.25)` this fix now produces, which is the measured
    /// direction of the "way too fast" report. If this control ever starts
    /// failing, `ai_ground_speed` has stopped changing the value it exists to
    /// change.
    #[test]
    fn removing_the_ground_speed_conversion_reproduces_the_too_fast_bug() {
        let attribute = 0.25;
        assert!(
            ai_ground_speed(attribute) < attribute,
            "control: the converted ground speed must be lower than the bare \
             attribute value, or the subject assertions above prove nothing \
             about the conversion firing"
        );
    }

    /// A bred child inherits the correct baby shape through
    /// `resolve_breeding`'s existing `child.set_age(BABY_START_AGE)` call —
    /// no separate wiring needed, because [`SimMob::set_age`] itself now
    /// re-derives the shape. This is the island check: a shape fold that only
    /// ran for a hand-called `set_age` in a test, and never for the
    /// production breeding path, would still look finished from the unit
    /// tests above alone.
    #[test]
    fn a_bred_child_spawns_with_the_baby_shape_already_applied() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let a = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(7.0, 1.0, 8.0))
            .id();
        let _b = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(9.0, 1.0, 8.0))
            .id();

        // Exercises `resolve_breeding`'s own partner search and
        // `child.set_age(BABY_START_AGE)` call directly — the real
        // production path a breeding goal completing feeds through
        // `MobSim::tick`, without re-driving sixty ticks of love-mode timing
        // just to reach it.
        sim.resolve_breeding(vec![(
            a,
            Vec3::new(8.0, 1.0, 8.0),
            "minecraft:cow".parse().expect("valid key"),
        )]);

        let child = sim
            .mobs
            .iter()
            .find(|m| m.is_baby())
            .expect("a child was spawned and is a baby");
        assert_eq!(
            child.shape().width,
            0.45,
            "the bred calf's shape must already be the baby literal, not the adult default"
        );
    }
}

/// `MetadataField::Baby`'s producer-side species switch in
/// [`SimMob::snapshot`] — the eligible species must match exactly the union
/// [`baby_dimensions`]/[`baby_speed_multiplier`] already scope "grows a
/// baby" to, and the ineligible species (index 16's other claimants) must
/// never see the field at all.
#[cfg(test)]
mod baby_metadata_tests {
    use super::*;

    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for z in 0..16 {
            for x in 0..16 {
                world.set_block(x, 0, z, "minecraft:stone");
            }
        }
        world
    }

    fn above_floor() -> Vec3 {
        Vec3::new(8.0, 1.0, 8.0)
    }

    fn baby_field(metadata: &[MetadataField]) -> Option<bool> {
        metadata.iter().find_map(|f| match f {
            MetadataField::Baby(b) => Some(*b),
            _ => None,
        })
    }

    /// **Positive arm**: every species this sim scopes ageing to
    /// (vanilla's own ageable-mob breedable-animal set plus the zombie family — see
    /// `SimMob::snapshot`'s own comment for the mechanical derivation off
    /// `.cache/mc/26.2/src/`) must push `MetadataField::Baby(false)` as a
    /// freshly-spawned adult.
    ///
    /// **Negative control**: index 16's other real claimants — `creeper`
    /// (vanilla's own swell-direction metadata field, an `INT`, already the producer for a
    /// different variant at this same index), `ghast`
    /// (vanilla's own "is charging" metadata field) and `phantom` (vanilla's
    /// own size metadata field) — must
    /// push no `Baby` field at all. These are exactly the entities a shared
    /// "is baby" encoder would corrupt: a ghast told `Baby(false)` reads to
    /// a real client as "not charging", and a phantom's size becomes `0`.
    ///
    /// Collected rather than asserted per-iteration so one run reports every
    /// wrong species, not just the first.
    #[test]
    fn eligible_species_emit_baby_and_only_those_do() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let mut wrong: Vec<String> = Vec::new();

        for species in [
            "cow",
            "mooshroom",
            "sheep",
            "pig",
            "chicken",
            "rabbit",
            "wolf",
            "zombie",
            "husk",
            "zombie_villager",
            "drowned",
            "zombified_piglin",
        ] {
            let key = format!("minecraft:{species}").parse().expect("valid key");
            let id = sim.spawn_species(key, above_floor()).id();
            let metadata = sim.get(id).expect("spawned").snapshot().metadata;
            if baby_field(&metadata) != Some(false) {
                wrong.push(format!(
                    "{species}: expected Baby(false), metadata was {metadata:?}"
                ));
            }
        }

        for species in ["creeper", "ghast", "phantom"] {
            let key = format!("minecraft:{species}").parse().expect("valid key");
            let id = sim.spawn_species(key, above_floor()).id();
            let metadata = sim.get(id).expect("spawned").snapshot().metadata;
            if baby_field(&metadata).is_some() {
                wrong.push(format!(
                    "{species}: must emit no Baby field at all, metadata was {metadata:?}"
                ));
            }
        }

        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// **The grown-up transition.** A baby that matures must produce a
    /// snapshot whose `Baby` is `Some(false)`, not absent — an absent field
    /// leaves the client holding whatever `Baby(true)` it was sent on
    /// arrival, so the mob would stay a baby on screen forever. See
    /// `SimMob::snapshot`'s own doc comment for why this variant is pushed
    /// unconditionally rather than only while `is_baby()` is true.
    #[test]
    fn a_grown_up_baby_reports_baby_false_not_absent() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:zombie".parse().expect("valid key"), above_floor())
            .set_age(lodestone_entity::ai::navigating_mob::BABY_START_AGE)
            .id();
        let baby_metadata = sim.get(id).expect("spawned").snapshot().metadata;
        assert_eq!(
            baby_field(&baby_metadata),
            Some(true),
            "a freshly spawned baby zombie must report Baby(true), got {baby_metadata:?}"
        );

        sim.get_mut(id).expect("spawned").set_age(0);
        let adult_metadata = sim.get(id).expect("still spawned").snapshot().metadata;
        assert!(
            adult_metadata.iter().any(|f| matches!(f, MetadataField::Baby(_))),
            "the grown-up snapshot must still carry a Baby field, not omit it: {adult_metadata:?}"
        );
        assert_eq!(
            baby_field(&adult_metadata),
            Some(false),
            "after growing up the field must flip to Baby(false), got {adult_metadata:?}"
        );
    }
}

/// Lead attach/detach, the fence-knot re-parent, and the
/// distance-based pull/snap physics.
#[cfg(test)]
mod leash_tests {
    use super::*;

    /// A real floor makes living mobs fall when idle (see
    /// `NavigatingMob::advance`'s no-waypoint branch). `-8..=24`/`-8..=8` covers every
    /// coordinate this module's own tests spawn a mob at, with margin.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=24 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    fn player_at(uuid: Uuid, pos: Vec3) -> PerceivedPlayer {
        PerceivedPlayer {
            identity: Some(PlayerIdentity { uuid, entity_id: 99 }),
            perception: PlayerPerception {
                position: pos,
                held_item: None,
                view_direction: Vec3::new(0.0, 0.0, 1.0),
            },
        }
    }

    #[test]
    fn attaching_a_lead_to_a_leashable_mob_holds_it() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        sim.set_players(vec![player_at(holder, Vec3::new(0.0, 0.0, 0.0))]);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0))
            .id();

        let outcome = sim.try_leash(id, holder, true, false);
        assert_eq!(outcome, LeashOutcome::Attached);
        assert_eq!(
            sim.get(id).expect("spawned").leash_holder(),
            Some(LeashHolder::Player(holder))
        );
    }

    /// **Control: a hostile species refuses a lead** — vanilla's own
    /// generic "can be leashed" check is "not a hostile-tagged mob", so this is the
    /// discriminating input against "every mob accepts a lead".
    #[test]
    fn control_a_hostile_mob_refuses_a_lead() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        sim.set_players(vec![player_at(holder, Vec3::new(0.0, 0.0, 0.0))]);
        let id = sim
            .spawn_species(
                "minecraft:zombie".parse().expect("valid key"),
                Vec3::new(2.0, 0.0, 0.0),
            )
            .id();

        assert_eq!(sim.try_leash(id, holder, true, false), LeashOutcome::Refused);
        assert_eq!(sim.get(id).expect("spawned").leash_holder(), None);
    }

    #[test]
    fn detaching_returns_the_lead_unless_creative() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        sim.set_players(vec![player_at(holder, Vec3::new(0.0, 0.0, 0.0))]);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0))
            .id();
        assert_eq!(sim.try_leash(id, holder, true, false), LeashOutcome::Attached);

        assert_eq!(
            sim.try_leash(id, holder, false, false),
            LeashOutcome::Detached { dropped_lead: true },
            "survival mode must drop a lead item"
        );
        assert_eq!(sim.get(id).expect("still spawned").leash_holder(), None);
        assert_eq!(sim.item_count(), 1, "the reported drop must be a real item, not just a flag");

        assert_eq!(sim.try_leash(id, holder, true, false), LeashOutcome::Attached);
        assert_eq!(
            sim.try_leash(id, holder, false, true),
            LeashOutcome::Detached {
                dropped_lead: false
            },
            "creative mode (infinite materials) must not drop a lead item"
        );
        assert_eq!(
            sim.item_count(),
            1,
            "creative detach must not spawn a second item on top of the survival one"
        );
    }

    /// One player cannot steal another's already-leashed mob just by
    /// holding a lead — vanilla's `!(leashable.getLeashHolder() instanceof
    /// Player)` guard.
    #[test]
    fn a_different_players_lead_cannot_steal_an_already_leashed_mob() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let owner = Uuid::new_v4();
        let thief = Uuid::new_v4();
        sim.set_players(vec![
            player_at(owner, Vec3::new(0.0, 0.0, 0.0)),
            player_at(thief, Vec3::new(1.0, 0.0, 0.0)),
        ]);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0))
            .id();
        assert_eq!(sim.try_leash(id, owner, true, false), LeashOutcome::Attached);

        assert_eq!(sim.try_leash(id, thief, true, false), LeashOutcome::Refused);
        assert_eq!(
            sim.get(id).expect("spawned").leash_holder(),
            Some(LeashHolder::Player(owner)),
            "the mob must still belong to its original holder"
        );
    }

    /// **The exact pull vector, predicted, at a discriminating distance**
    /// (10 blocks: past `LEASH_ELASTIC_DIST` (6) and short of
    /// `LEASH_TOO_FAR_DIST` (12), so both thresholds are exercised by the
    /// same fixture in opposite directions). `excess = 10 - 6 = 4`, capped
    /// to `1.0`, times the `0.3` scale this port uses — `0.3` exactly, along
    /// `+x` since the holder is due east.
    #[test]
    fn a_leashed_mob_beyond_the_elastic_distance_is_pulled_toward_its_holder() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        sim.set_players(vec![player_at(holder, Vec3::new(10.0, 0.0, 0.0))]);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(id)
            .expect("spawned")
            .set_leash_holder(Some(LeashHolder::Player(holder)));

        sim.tick_leashes();

        let mob = sim.get(id).expect("still spawned");
        assert_eq!(
            mob.position(),
            Vec3::new(0.3, 0.0, 0.0),
            "pulled 0.3 blocks toward the holder, not teleported"
        );
        assert!(mob.is_leashed(), "still leashed — this is a pull, not a snap");
    }

    /// **Control: within the elastic distance, nothing happens at all.**
    /// Discriminates against a pull formula that fires unconditionally.
    #[test]
    fn control_a_leashed_mob_within_the_elastic_distance_is_not_pulled() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        sim.set_players(vec![player_at(holder, Vec3::new(3.0, 0.0, 0.0))]);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(id)
            .expect("spawned")
            .set_leash_holder(Some(LeashHolder::Player(holder)));

        sim.tick_leashes();

        assert_eq!(
            sim.get(id).expect("still spawned").position(),
            Vec3::new(0.0, 0.0, 0.0),
            "3 blocks is inside LEASH_ELASTIC_DIST (6) — no force at all"
        );
    }

    /// Past `LEASH_TOO_FAR_DIST` (12), the lead snaps: the mob is freed and
    /// a real `minecraft:lead` item appears at its position.
    #[test]
    fn a_leashed_mob_beyond_the_snap_distance_drops_its_lead() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        sim.set_players(vec![player_at(holder, Vec3::new(13.0, 0.0, 0.0))]);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(id)
            .expect("spawned")
            .set_leash_holder(Some(LeashHolder::Player(holder)));
        assert_eq!(sim.item_count(), 0, "control: nothing dropped yet");

        sim.tick_leashes();

        assert!(
            !sim.get(id).expect("still spawned").is_leashed(),
            "13 blocks is past LEASH_TOO_FAR_DIST (12) — the lead must snap"
        );
        assert_eq!(sim.item_count(), 1, "a lead item must be spawned on snap");
    }

    /// A leash holder that cannot be resolved (a disconnected player) drops
    /// the leash silently, with no item — the disclosed simplification
    /// `tick_leashes`'s own doc comment names.
    #[test]
    fn control_an_unresolvable_holder_drops_the_leash_without_an_item() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        // No `set_players` call — `holder` cannot be resolved to a position.
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(id)
            .expect("spawned")
            .set_leash_holder(Some(LeashHolder::Player(holder)));

        sim.tick_leashes();

        assert!(!sim.get(id).expect("still spawned").is_leashed());
        assert_eq!(sim.item_count(), 0, "an unresolvable holder must not drop an item");
    }

    /// Right-clicking a fence with a lead re-parents every mob leashed to
    /// the player onto that fence position — vanilla's own lead-item "bind player mobs" call.
    #[test]
    fn leashing_to_a_fence_reparents_every_mob_the_player_was_holding() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let holder = Uuid::new_v4();
        sim.set_players(vec![player_at(holder, Vec3::new(0.0, 0.0, 0.0))]);
        let a = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(1.0, 0.0, 0.0))
            .id();
        let b = sim
            .spawn_species("minecraft:sheep".parse().expect("valid key"), Vec3::new(1.0, 0.0, 1.0))
            .id();
        // A third mob leashed to someone else must not be swept up.
        let other_holder = Uuid::new_v4();
        let c = sim
            .spawn_species("minecraft:pig".parse().expect("valid key"), Vec3::new(1.0, 0.0, 2.0))
            .id();
        sim.get_mut(a).expect("spawned").set_leash_holder(Some(LeashHolder::Player(holder)));
        sim.get_mut(b).expect("spawned").set_leash_holder(Some(LeashHolder::Player(holder)));
        sim.get_mut(c)
            .expect("spawned")
            .set_leash_holder(Some(LeashHolder::Player(other_holder)));

        let fence_pos = BlockPos::new(5, 0, 5);
        let mut moved = sim.try_leash_to_fence(holder, fence_pos);
        moved.sort_unstable();
        let mut expected = vec![a, b];
        expected.sort_unstable();
        assert_eq!(moved, expected, "only the calling player's own leashed mobs move");

        assert_eq!(
            sim.get(a).expect("spawned").leash_holder(),
            Some(LeashHolder::Fence(fence_pos))
        );
        assert_eq!(
            sim.get(c).expect("spawned").leash_holder(),
            Some(LeashHolder::Player(other_holder)),
            "a mob leashed to a different player must be untouched"
        );
    }
}

/// Entity-spawn slice: the trader plus its leashed llama
/// escort. The spawn-cycle timing/POI search is out of scope here — see
/// `spawn_wandering_trader`'s own doc comment.
#[cfg(test)]
mod wandering_trader_tests {
    use super::*;

    /// A real floor — see `leash_tests::flat_world`'s own doc comment for
    /// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=16 {
            for z in -8..=16 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    #[test]
    fn spawning_a_wandering_trader_leashes_two_llamas_to_it() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);

        let (trader_id, llamas) = sim.spawn_wandering_trader(Vec3::new(10.0, 5.0, 10.0));

        assert_eq!(llamas.len(), 2, "vanilla attempts exactly two escorts");
        let trader = sim.get(trader_id).expect("trader spawned");
        assert_eq!(trader.entity_type().path(), "wandering_trader");

        for llama_id in llamas {
            let llama = sim.get(llama_id).expect("llama spawned");
            assert_eq!(llama.entity_type().path(), "trader_llama");
            assert_eq!(
                llama.leash_holder(),
                Some(LeashHolder::Mob(trader_id)),
                "each llama must be leashed to the trader, not merely placed near it"
            );
        }
    }

    /// **The leash is real, not cosmetic** — moving the trader and ticking
    /// leashes must pull an escort that has drifted past the elastic
    /// distance, exactly as it would for a player-held leash. This is the
    /// control that separates "the llama has a `leash_holder` field set" from
    /// "the llama is actually tethered".
    #[test]
    fn the_escort_leash_actually_pulls_when_the_trader_moves_away() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let (trader_id, llamas) = sim.spawn_wandering_trader(Vec3::new(0.0, 0.0, 0.0));
        let llama_id = llamas[0];

        // Drag the trader far enough that the escort (2 blocks from spawn,
        // at x=2) is past LEASH_ELASTIC_DIST (6) from it but still short of
        // LEASH_TOO_FAR_DIST (12), so this exercises the *pull* branch —
        // distance 8, not the snap branch a farther drag would hit instead.
        // There is no teleport API, so drive it through a knockback impulse
        // large enough to land at the target position deterministically.
        let trader_pos = sim.get(trader_id).expect("spawned").position();
        let target = Vec3::new(10.0, 0.0, 0.0);
        sim.get_mut(trader_id).expect("spawned").apply_knockback(Vec3::new(
            target.x - trader_pos.x,
            target.y - trader_pos.y,
            target.z - trader_pos.z,
        ));
        assert_eq!(sim.get(trader_id).expect("spawned").position(), target);

        let llama_before = sim.get(llama_id).expect("spawned").position();
        sim.tick_leashes();
        let llama_after = sim.get(llama_id).expect("still spawned").position();
        assert_ne!(
            llama_before, llama_after,
            "the llama must move toward its holder once the trader is far enough away"
        );
    }
}

/// `MobSim`'s periodic idle-vocalisation producer (`roll_ambient_sound`),
/// wired into [`MobSim::tick`], emits ambient sounds during ordinary
/// exploration in addition to hurt and death sounds.
#[cfg(test)]
mod ambient_sound_tests {
    use super::*;
    use crate::effects::WorldEffect;
    use lodestone_model::SoundCategory;

    /// A real floor — see `leash_tests::flat_world`'s own doc comment for
    /// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    /// **The wire is real, not merely the derivation.** A hermetic call to
    /// `effects::mob_ambient_sound` proves the string derivation; it proves
    /// nothing about whether anything in `MobSim::tick` ever calls it — the
    /// exact island shape this repo's own evidence standards warn about
    /// repeatedly. Ticking a real, freshly spawned cow through the real
    /// production `tick()` loop and draining `take_ambient_sounds()` is what
    /// proves the roll actually fires and actually reaches the queue
    /// `crate::tick::run_tick_loop` drains.
    ///
    /// `ambient_sound_time` starts at `0` and this cow's RNG stream is
    /// seeded deterministically from its id, so how many ticks the first
    /// firing takes is a fixed, measured number for this exact scenario, not
    /// a guess: measured at **tick 36** for this world/spawn order. The
    /// 400-tick loop bound is over 10x that measured value, not a round
    /// number reached for on its own — it exists only so a regression that
    /// pushed the firing tick out shows up as a clean failure rather than an
    /// infinite loop.
    #[test]
    fn a_real_mob_ticked_through_the_production_loop_eventually_vocalises() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0));

        let mut fired = Vec::new();
        for _ in 0..400 {
            sim.tick();
            fired.extend(sim.take_ambient_sounds());
            if !fired.is_empty() {
                break;
            }
        }

        assert_eq!(
            fired.len(),
            1,
            "exactly one ambient sound must have fired within the margin, got {fired:?}"
        );
        match &fired[0] {
            WorldEffect::Sound { sound, category, .. } => {
                assert_eq!(sound, "minecraft:entity.cow.ambient");
                assert_eq!(*category, SoundCategory::Neutral);
            }
            other => panic!("expected a Sound effect, got {other:?}"),
        }
    }

    /// **Control: the roll is load-bearing, not vacuous.** A dead mob
    /// (`health <= 0.0`) must never roll — vanilla's own guard is
    /// `isAlive() && …` — so ticking a mob whose health is forced to zero
    /// for the same window above must produce nothing at all. Without this,
    /// the subject test's "it fired" is not distinguishable from "everything
    /// always fires".
    #[test]
    fn a_dead_mob_never_rolls_an_ambient_sound() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:cow".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(id).expect("spawned").health = 0.0;

        let mut fired = Vec::new();
        for _ in 0..400 {
            sim.tick();
            fired.extend(sim.take_ambient_sounds());
        }
        assert!(
            fired.is_empty(),
            "a mob at zero health must never roll an ambient sound, got {fired:?}"
        );
    }

    /// A hostile species reports the `Hostile` sound category, matching
    /// `mob_vocalisation`'s own hurt/death split — checked with the same
    /// production-loop wiring as the subject test above, not just the
    /// hermetic derivation.
    #[test]
    fn a_hostile_mob_vocalises_on_the_hostile_category() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_species("minecraft:zombie".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0));

        let mut fired = Vec::new();
        for _ in 0..400 {
            sim.tick();
            fired.extend(sim.take_ambient_sounds());
            if !fired.is_empty() {
                break;
            }
        }
        assert_eq!(fired.len(), 1, "expected exactly one ambient sound, got {fired:?}");
        match &fired[0] {
            WorldEffect::Sound { sound, category, .. } => {
                assert_eq!(sound, "minecraft:entity.zombie.ambient");
                assert_eq!(*category, SoundCategory::Hostile);
            }
            other => panic!("expected a Sound effect, got {other:?}"),
        }
    }
}

/// Gossip, reputation and zombie-villager curing,
/// driven through real production entry points
/// (`MobSim::interact`/`MobSim::tick`/`MobSim::attack_from_player`) rather
/// than calling `villager::gossip`/`villager::reputation`/`villager::conversion`
/// directly — those modules' own test suites already cover the pure
/// arithmetic; what these gates prove is that the wiring actually reaches a
/// live [`SimMob`], the same "reaches pixels, not just a closed loop"
/// standard `ambient_sound_tests` above applies.
#[cfg(test)]
mod villager_gossip_reputation_and_curing_tests {
    use super::*;

    /// A real floor near the origin — see `leash_tests::flat_world`'s own
    /// doc comment for why a bare void `ChunkWorld` stopped being safe once
    /// idle mobs fall. This module's "distant villager" control spawns at
    /// `(500, 0, 500)`, deliberately left ungrounded: nothing in this module
    /// asserts that villager's own position, only that gossip/curing never
    /// reaches it, and x/z are untouched by falling regardless.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=20 {
            for z in -8..=20 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    fn alice() -> PlayerIdentity {
        PlayerIdentity {
            uuid: Uuid::from_u128(0xA11CE),
            entity_id: 4242,
        }
    }

    /// A golden apple on a zombie villager with no Weakness must do nothing
    /// at all — no conversion state, `Pass`, matching vanilla's own
    /// plain-success-no-reduction arm (disclosed as `Pass`,
    /// see `InteractOutcome::ZombieVillagerConversionStarted`'s own doc).
    #[test]
    fn a_golden_apple_on_an_unweakened_zombie_villager_does_nothing() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:zombie_villager".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();

        let outcome = sim.interact(
            id,
            alice(),
            Some(&"minecraft:golden_apple".parse().expect("valid key")),
        );
        assert_eq!(outcome, InteractOutcome::Pass);
        assert!(
            sim.get(id).expect("still alive").conversion.is_none(),
            "no conversion state must be started without Weakness"
        );
    }

    /// **The wire is real, not merely the derivation.** A golden apple used
    /// on a weakened zombie villager must: report
    /// `ZombieVillagerConversionStarted` (which consumes the item), start a
    /// real [`villager::conversion::ConversionState`] with the actor's uuid
    /// recorded, remove Weakness, add Strength, and publish the cure sound
    /// through the same [`MobSim::take_vocalisations`] queue
    /// `crate::tick::run_tick_loop` drains in production — not a hermetic
    /// call to `effects::zombie_villager_cure_sound` in isolation.
    #[test]
    fn a_golden_apple_on_a_weakened_zombie_villager_starts_a_real_conversion() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:zombie_villager".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();
        sim.get_mut(id)
            .expect("spawned")
            .apply_effect("minecraft:weakness", 1000, 0);

        let outcome = sim.interact(
            id,
            alice(),
            Some(&"minecraft:golden_apple".parse().expect("valid key")),
        );
        assert_eq!(outcome, InteractOutcome::ZombieVillagerConversionStarted);
        assert!(
            outcome.consumes_item(),
            "the golden apple must be consumed, matching itemStack.consume(1, player)"
        );

        let mob = sim.get(id).expect("still alive");
        let state = mob.conversion.expect("a conversion must have started");
        assert_eq!(state.starter, Some(alice().uuid));
        assert!(
            (villager::conversion::CONVERSION_WAIT_MIN..=villager::conversion::CONVERSION_WAIT_MAX)
                .contains(&state.remaining_ticks),
            "remaining_ticks must land in the real vanilla 3600-6000 range, got {}",
            state.remaining_ticks
        );
        assert!(
            mob.effects().amplifier_of("minecraft:weakness").is_none(),
            "Weakness must be removed"
        );
        assert!(
            mob.effects().amplifier_of("minecraft:strength").is_some(),
            "Strength must be applied"
        );

        let vocalisations = sim.take_vocalisations();
        assert_eq!(
            vocalisations.len(),
            1,
            "exactly one cure sound must have been queued, got {vocalisations:?}"
        );
        match &vocalisations[0] {
            crate::effects::WorldEffect::Sound { sound, .. } => {
                assert_eq!(sound, "minecraft:entity.zombie_villager.cure");
            }
            other => panic!("expected a Sound effect, got {other:?}"),
        }
    }

    /// **The whole timer, driven through the production `tick()` loop** rather
    /// than a direct call to `villager::conversion::conversion_progress`.
    /// The countdown uses a handful of ticks (private-field access, same crate)
    /// so this test does not need 3600+ iterations; the *mechanism* ticked is
    /// the same one production drives. A completed conversion must flip
    /// `entity_type` to `minecraft:villager`, seed gossip with the curer's
    /// `ZombieVillagerCured` entries, apply nausea (the "confusion" state), and publish
    /// a conversion-sound level event
    /// through the same queue production drains.
    #[test]
    fn a_completed_conversion_becomes_a_real_villager_with_seeded_gossip() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:zombie_villager".parse().expect("valid key"),
                Vec3::new(10.0, 0.0, 10.0),
            )
            .id();
        let curer = alice().uuid;
        sim.get_mut(id).expect("spawned").conversion = Some(villager::conversion::ConversionState {
            starter: Some(curer),
            remaining_ticks: 3,
        });

        let mut level_events = Vec::new();
        for _ in 0..10 {
            sim.tick();
            level_events.extend(sim.take_ambient_sounds());
            if sim
                .get(id)
                .is_some_and(|m| m.entity_type().path() == "villager")
            {
                break;
            }
        }

        let mob = sim.get(id).expect("still alive");
        assert_eq!(mob.entity_type().path(), "villager", "must have become a real villager");
        assert!(mob.conversion.is_none(), "conversion state must be cleared");
        assert_eq!(
            mob.gossip.reputation(curer),
            125,
            "ZombieVillagerCured's own predicted value (20*5 + 25*1), seeded onto the \
             new villager's own ledger"
        );
        assert!(
            mob.effects().amplifier_of("minecraft:nausea").is_some(),
            "the post-cure confusion state (Nausea) must be applied"
        );

        assert!(
            level_events.iter().any(|effect| matches!(
                effect,
                crate::effects::WorldEffect::LevelEvent { event, .. }
                    if *event == crate::effects::SOUND_ZOMBIE_CONVERTED
            )),
            "the SOUND_ZOMBIE_CONVERTED level event must reach the production queue, got {level_events:?}"
        );
    }

    /// `MobSim::record_reputation_event`/`villager_reputation` reach a real
    /// spawned villager's own ledger, not a hermetic `GossipContainer`.
    #[test]
    fn record_reputation_event_reaches_a_real_villagers_own_ledger() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        let player = alice().uuid;

        assert_eq!(sim.villager_reputation(id, player), 0);
        sim.record_reputation_event(id, villager::reputation::ReputationEventType::Trade, player);
        assert_eq!(sim.villager_reputation(id, player), 2, "trading grants 2 * weight(1) = 2");
    }

    /// `MobSim::attack_from_player`: hurting a real villager
    /// writes negative gossip onto **that villager's own** ledger about the
    /// attacker — driven through the real hit pipeline
    /// (`apply_damage`/`note_hurt`), not a direct `apply_reputation_event`
    /// call.
    #[test]
    fn hurting_a_real_villager_lowers_its_reputation_of_the_attacker() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        let attacker = alice();

        assert_eq!(sim.villager_reputation(id, attacker.uuid), 0);
        let outcome = sim.attack_from_player(
            id,
            Some(attacker),
            Vec3::new(1.0, 0.0, 0.0),
            1.0,
            DamageFlags::default(),
            0.0,
        );
        assert!(outcome.is_some(), "the villager must have been hit");
        assert_eq!(
            sim.villager_reputation(id, attacker.uuid),
            -25,
            "VillagerHurt's predicted value: 25 * minor_negative.weight()(-1)"
        );
    }

    /// **Control: a `None` attacker must write no gossip at all** — the
    /// disclosed "unidentified actor" skip, proven by actually driving it
    /// rather than merely asserting the branch exists.
    #[test]
    fn an_unidentified_attacker_writes_no_reputation_gossip() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();

        sim.attack_from_player(id, None, Vec3::new(1.0, 0.0, 0.0), 1.0, DamageFlags::default(), 0.0);
        assert!(
            sim.get(id).expect("still alive").gossip.is_empty(),
            "no attacker identity means no gossip write at all"
        );
    }

    /// Two villagers close enough to gossip exchange ledger entries through the
    /// real `tick()` loop's `spread_villager_gossip` pass, exercising the
    /// integrated producer and consumer path.
    #[test]
    fn two_nearby_villagers_spread_gossip_through_the_real_tick_loop() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let a = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        let b = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0))
            .id();
        let stranger = Uuid::from_u128(0xDEAD_BEEF);
        sim.get_mut(a)
            .expect("spawned")
            .gossip
            // `Trading`, not `MajorPositive`: `MajorPositive`'s own
            // `decay_per_transfer` (20) equals its own `max` (20), so it
            // can *never* survive a transfer (always decays to exactly 0,
            // below `DISCARD_THRESHOLD`) — a real vanilla quirk `gossip.rs`'s
            // own `a_transferred_entry_that_decays_below_threshold_is_dropped`
            // test already predicts. `Trading` at its own max (25) decays to
            // 5, which does survive.
            .add(stranger, villager::gossip::GossipType::Trading, 25);

        for _ in 0..(MobSim::GOSSIP_SPREAD_INTERVAL_TICKS + 1) {
            sim.tick();
        }

        assert!(
            sim.get(b)
                .expect("still alive")
                .gossip
                .entries_for(stranger)
                .is_some(),
            "villager b must have picked up some gossip about the stranger from villager a"
        );
    }

    /// Control: two villagers far apart never spread, even across many
    /// gossip-spread passes — otherwise the subject test above could pass
    /// under an implementation with no distance gate at all.
    #[test]
    fn distant_villagers_never_spread_gossip() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let a = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        let b = sim
            .spawn_species(
                "minecraft:villager".parse().expect("valid key"),
                Vec3::new(500.0, 0.0, 500.0),
            )
            .id();
        let stranger = Uuid::from_u128(0xDEAD_BEEF);
        sim.get_mut(a)
            .expect("spawned")
            .gossip
            // `Trading`, not `MajorPositive`: `MajorPositive`'s own
            // `decay_per_transfer` (20) equals its own `max` (20), so it
            // can *never* survive a transfer (always decays to exactly 0,
            // below `DISCARD_THRESHOLD`) — a real vanilla quirk `gossip.rs`'s
            // own `a_transferred_entry_that_decays_below_threshold_is_dropped`
            // test already predicts. `Trading` at its own max (25) decays to
            // 5, which does survive.
            .add(stranger, villager::gossip::GossipType::Trading, 25);

        for _ in 0..(MobSim::GOSSIP_SPREAD_INTERVAL_TICKS * 3) {
            sim.tick();
        }

        assert!(
            sim.get(b)
                .expect("still alive")
                .gossip
                .entries_for(stranger)
                .is_none(),
            "villagers 500 blocks apart must never spread gossip to each other"
        );
    }
}

#[cfg(test)]
mod allay_carrying_tests {
    use super::*;

    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    fn alice() -> PlayerIdentity {
        PlayerIdentity {
            uuid: Uuid::from_u128(0xA11CE),
            entity_id: 4242,
        }
    }

    /// The empty-handed allay "carrying" interaction path: an allay given an
    /// item must take it
    /// into its main hand, consume the item, and report `ItemGiven`.
    #[test]
    fn giving_an_empty_handed_allay_an_item_makes_it_hold_that_item() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:allay".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();
        assert!(
            sim.get(id).expect("spawned").mob.main_hand_item().is_none(),
            "a freshly spawned allay must start empty-handed"
        );

        let outcome = sim.interact(
            id,
            alice(),
            Some(&"minecraft:emerald".parse().expect("valid key")),
        );

        assert_eq!(outcome, InteractOutcome::ItemGiven);
        assert!(
            outcome.consumes_item(),
            "the given item must be consumed, matching itemStack.consume(1, player)"
        );
        assert_eq!(
            sim.get(id).expect("still alive").mob.main_hand_item(),
            Some("emerald"),
            "the allay must now be carrying exactly the item it was given"
        );
    }

    /// The negative control: an allay **already** carrying an item must
    /// refuse a second one — vanilla's own allay interaction override's gate is
    /// specifically "empty main hand", not "any interaction with an item".
    /// Without this, the positive gate above could be passing because every
    /// interaction overwrites the held item unconditionally rather than
    /// because the empty-hand gate is real.
    #[test]
    fn an_already_carrying_allay_refuses_a_second_item() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:allay".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();
        let first = sim.interact(
            id,
            alice(),
            Some(&"minecraft:emerald".parse().expect("valid key")),
        );
        assert_eq!(first, InteractOutcome::ItemGiven);

        let second = sim.interact(
            id,
            alice(),
            Some(&"minecraft:diamond".parse().expect("valid key")),
        );
        assert_eq!(
            second,
            InteractOutcome::Pass,
            "an allay already carrying an item must refuse a second one"
        );
        assert_eq!(
            sim.get(id).expect("still alive").mob.main_hand_item(),
            Some("emerald"),
            "the original item must still be held after the refused second gift"
        );
    }

    /// A second negative control: an empty-handed interaction (no item held
    /// by the actor) must never clear or otherwise touch an allay's hands.
    #[test]
    fn an_empty_hand_interaction_does_nothing_to_an_allay() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:allay".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();

        let outcome = sim.interact(id, alice(), None);
        assert_eq!(outcome, InteractOutcome::Pass);
        assert!(sim.get(id).expect("still alive").mob.main_hand_item().is_none());
    }

    /// An allay-specific pickup check: a carrying allay
    /// with a matching item dropped right next to it absorbs the whole
    /// stack into [`SimMob::allay_inventory_count`] and the ground item is
    /// gone, driven through the real production path (`MobSim::tick` →
    /// `allay_pick_up_items`).
    #[test]
    fn an_allay_picks_up_a_matching_dropped_item_nearby() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:allay".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.interact(id, alice(), Some(&"minecraft:stick".parse().expect("valid key")));
        let stick_id = sim.spawn_item(
            "minecraft:stick".parse().expect("valid key"),
            Vec3::new(0.5, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 0.0),
            ItemLifecycle::newly_dropped(3, lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE),
        );

        sim.tick();

        assert_eq!(
            sim.get(id).expect("alive").allay_inventory_count(),
            3,
            "the whole 3-stack must be absorbed"
        );
        assert!(
            sim.item_lifecycle(stick_id).is_none(),
            "the fully-absorbed ground stack must be removed, not left at count 0"
        );
    }

    /// **Control**: an emerald dropped next to a stick-carrying allay must
    /// never be picked up — `allayConsidersItemEqual`'s own item-identity
    /// gate, without which the positive test above could be passing because
    /// every nearby item is absorbed regardless of type.
    #[test]
    fn an_allay_ignores_a_dropped_item_of_a_different_type() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:allay".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.interact(id, alice(), Some(&"minecraft:stick".parse().expect("valid key")));
        sim.spawn_item(
            "minecraft:emerald".parse().expect("valid key"),
            Vec3::new(0.5, 0.0, 0.0),
            Vec3::new(0.0, 0.0, 0.0),
            ItemLifecycle::newly_dropped(1, lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE),
        );

        sim.tick();

        assert_eq!(
            sim.get(id).expect("alive").allay_inventory_count(),
            0,
            "a mismatched item type must never be picked up"
        );
        assert_eq!(sim.item_count(), 1, "the mismatched item must still be on the ground");
    }

    /// `GoAndGiveItemsToTarget`: a carrying allay standing at its own liked
    /// note block's `.above()` cell throws exactly one item there per tick
    /// — a real dropped [`crate::item_entity::ItemEntity`] a player could
    /// walk over, not a state flag. Drives `MobSim::tick` →
    /// `allay_deliver_items` directly against host state set the way
    /// `resolve_vibrations`' own `hearNoteblock` arm would have left it,
    /// isolating the *delivery* half from the *hearing* half already proven
    /// end-to-end in `crate::tick`'s own note-block gates.
    #[test]
    fn a_carrying_allay_at_its_liked_noteblock_delivers_one_item_per_tick() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:allay".parse().expect("valid key"),
                Vec3::new(0.0, 1.0, 0.0),
            )
            .id();
        sim.interact(id, alice(), Some(&"minecraft:stick".parse().expect("valid key")));
        {
            let mob = sim.get_mut(id).expect("alive");
            mob.allay_inventory_count = 2;
            mob.allay_liked_noteblock = Some((Vec3::new(0.0, 0.0, 0.0), 100));
        }

        sim.tick();

        assert_eq!(
            sim.get(id).expect("alive").allay_inventory_count(),
            1,
            "exactly one item must be thrown this tick"
        );
        assert_eq!(sim.item_count(), 1, "the thrown item must be a real ground entity");
    }

    /// **Control**: the identical fixture but far from any liked note block
    /// (`allay_liked_noteblock` left `None`) must never deliver — proving
    /// the arrival check above is a real gate, not unconditional draining.
    #[test]
    fn a_carrying_allay_with_no_liked_noteblock_never_delivers() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species(
                "minecraft:allay".parse().expect("valid key"),
                Vec3::new(0.0, 1.0, 0.0),
            )
            .id();
        sim.interact(id, alice(), Some(&"minecraft:stick".parse().expect("valid key")));
        sim.get_mut(id).expect("alive").allay_inventory_count = 2;

        sim.tick();

        assert_eq!(
            sim.get(id).expect("alive").allay_inventory_count(),
            2,
            "with nothing liked, nothing must be thrown"
        );
        assert_eq!(sim.item_count(), 0);
    }

    /// **The allay duplication arm, through the production path** — driven by
    /// `allay_liked_noteblock` as the dance signal. An amethyst shard on such an allay
    /// must spawn a second, real allay and consume the shard.
    #[test]
    fn an_amethyst_shard_duplicates_an_allay_that_recently_heard_a_noteblock() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:allay".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();
        sim.get_mut(id).expect("alive").allay_liked_noteblock = Some((Vec3::new(3.0, 0.0, 0.0), 100));

        let before = sim.snapshots().iter().filter(|s| s.entity_type.path() == "allay").count();
        let outcome = sim.interact(
            id,
            alice(),
            Some(&"minecraft:amethyst_shard".parse().expect("valid key")),
        );

        assert_eq!(outcome, InteractOutcome::AllayDuplicated);
        assert!(outcome.consumes_item(), "the shard must be consumed");
        let after = sim.snapshots().iter().filter(|s| s.entity_type.path() == "allay").count();
        assert_eq!(after, before + 1, "duplication must spawn exactly one real new allay");
        assert!(
            sim.get(id).expect("alive").allay_duplication_cooldown > 0,
            "the original allay must be put on cooldown too"
        );
        assert!(
            sim.take_vocalisations().iter().any(|effect| matches!(
                effect,
                crate::effects::WorldEffect::Particles { particle, .. } if particle == "minecraft:heart"
            )),
            "vanilla's own allay entity-event handler's status-18 heart burst \
             must reach the production queue too, not just the outcome's own \
             particle() classification"
        );
    }

    /// **Control**: the identical shard interaction against an allay that
    /// has never heard a note block must do nothing — proving the
    /// `isDancing()` substitute is a real gate, not one that always fires
    /// on an amethyst shard.
    #[test]
    fn an_amethyst_shard_does_nothing_to_an_allay_that_never_heard_a_noteblock() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:allay".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();

        let before = sim.snapshots().iter().filter(|s| s.entity_type.path() == "allay").count();
        let outcome = sim.interact(
            id,
            alice(),
            Some(&"minecraft:amethyst_shard".parse().expect("valid key")),
        );

        assert_ne!(
            outcome,
            InteractOutcome::AllayDuplicated,
            "an allay that never heard a note block must never duplicate"
        );
        let after = sim.snapshots().iter().filter(|s| s.entity_type.path() == "allay").count();
        assert_eq!(after, before, "no new allay must have spawned");
    }
}

/// Villager hurt or nearby-hostile conditions can summon an iron golem through
/// the integrated mob-simulation path.
#[cfg(test)]
mod golem_summon_tests {
    use super::*;

    /// A real floor near the origin — see `leash_tests::flat_world`'s own
    /// doc comment for why a bare void `ChunkWorld` stopped being safe once
    /// idle mobs fall. This module's `hurt_villager` calls at `x = ±50` are
    /// deliberately left ungrounded: no assertion here reads either
    /// villager's own position, only the resulting golem-summon count.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    fn hurt_villager(sim: &mut MobSim<'_>, pos: Vec3) -> i32 {
        let id = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), pos)
            .id();
        sim.get_mut(id)
            .expect("just spawned")
            .mob
            .note_hurt(Some(Vec3::new(pos.x + 1.0, pos.y, pos.z)));
        id
    }

    fn iron_golem_count(sim: &MobSim<'_>) -> usize {
        sim.mobs
            .iter()
            .filter(|m| m.entity_type.path() == "iron_golem")
            .count()
    }

    /// The headline case: three hurt villagers within the 10-block agreement
    /// box must produce exactly one iron golem, and every villager in the box
    /// (not only the triggering one) must be marked `golem_detected_until` so
    /// a second pass this same 100-tick window does not summon a second one.
    #[test]
    fn three_hurt_villagers_close_together_summon_exactly_one_golem() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        hurt_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));
        hurt_villager(&mut sim, Vec3::new(2.0, 0.0, 0.0));
        hurt_villager(&mut sim, Vec3::new(-2.0, 0.0, 0.0));

        sim.tick();

        assert_eq!(
            iron_golem_count(&sim),
            1,
            "three hurt villagers within the agreement box must summon exactly one golem"
        );
        assert!(
            sim.mobs
                .iter()
                .filter(|m| m.entity_type.path() == "villager")
                .all(|m| m.golem_detected_until.is_some()),
            "every villager in the agreement box must be marked golem-detected after a spawn"
        );
    }

    /// Below `GOLEM_VILLAGERS_NEEDED` (3): two hurt villagers must summon
    /// nothing — the discriminating floor, not merely "some villagers hurt
    /// summons a golem eventually".
    #[test]
    fn two_hurt_villagers_do_not_summon_a_golem() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        hurt_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));
        hurt_villager(&mut sim, Vec3::new(2.0, 0.0, 0.0));

        for _ in 0..150 {
            sim.tick();
        }

        assert_eq!(iron_golem_count(&sim), 0, "two hurt villagers must never reach the agreement floor");
    }

    /// Three villagers far enough apart that they never share an agreement
    /// box (each pairwise distance exceeds `GOLEM_AGREEMENT_RADIUS`) must not
    /// summon a golem even though each is individually hurt.
    #[test]
    fn three_hurt_villagers_too_far_apart_do_not_summon_a_golem() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        hurt_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));
        hurt_villager(&mut sim, Vec3::new(50.0, 0.0, 0.0));
        hurt_villager(&mut sim, Vec3::new(-50.0, 0.0, 0.0));

        sim.tick();

        assert_eq!(iron_golem_count(&sim), 0, "villagers outside one shared agreement box must not summon a golem");
    }

    /// A villager that is neither hurt nor near a hostile is not a candidate
    /// at all — three *unhurt* villagers standing together must never summon
    /// a golem.
    #[test]
    fn unhurt_villagers_with_no_hostile_nearby_never_summon() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0));
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0));
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(-2.0, 0.0, 0.0));

        for _ in 0..150 {
            sim.tick();
        }

        assert_eq!(iron_golem_count(&sim), 0, "villagers with nothing wrong must never summon a golem");
    }

    /// A hostile mob nearby is exactly as good as being hurt — three
    /// *unhurt* villagers next to a zombie must still summon, proving the
    /// `hurt || hostile_near` disjunction rather than only the hurt half.
    #[test]
    fn a_nearby_hostile_alone_is_enough_to_summon() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0));
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(2.0, 0.0, 0.0));
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(-2.0, 0.0, 0.0));
        sim.spawn_species("minecraft:zombie".parse().expect("valid key"), Vec3::new(1.0, 0.0, 1.0));

        sim.tick();

        assert_eq!(iron_golem_count(&sim), 1, "a nearby hostile alone must be enough to summon, with no villager hurt");
    }
}

/// Cat gift and parrot shoulder-ride requests are drained and resolved by the
/// real [`MobSim::tick`] loop, covering the production connection between
/// request producers and host consumers.
#[cfg(test)]
mod cat_gift_and_shoulder_tests {
    use super::*;

    /// A real floor — see `leash_tests::flat_world`'s own doc comment for
    /// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    fn cat_key() -> ResourceKey {
        "minecraft:cat".parse().expect("valid key")
    }

    fn parrot_key() -> ResourceKey {
        "minecraft:parrot".parse().expect("valid key")
    }

    /// `cat_gift_chance`'s two-keyframe step function, pinned against both
    /// hypotheses a rounding-off reading could produce: inside the
    /// pre-dawn window (`[23667, 24000)` and `[0, 362)`, wrapping) the
    /// chance is vanilla's own `0.7F`; everywhere else it is a hard `0.0`,
    /// not merely "lower".
    #[test]
    fn cat_gift_chance_matches_the_timeline_step_function() {
        assert_eq!(MobSim::cat_gift_chance(0), 0.7, "the very start of a day is still inside the wrapped window");
        assert_eq!(MobSim::cat_gift_chance(361), 0.7, "one tick before the low keyframe");
        assert_eq!(MobSim::cat_gift_chance(362), 0.0, "the low keyframe itself");
        assert_eq!(MobSim::cat_gift_chance(12_000), 0.0, "the middle of the day");
        assert_eq!(MobSim::cat_gift_chance(23_666), 0.0, "one tick before the high keyframe");
        assert_eq!(MobSim::cat_gift_chance(23_667), 0.7, "the high keyframe itself");
        assert_eq!(MobSim::cat_gift_chance(23_999), 0.7, "the last tick before wrapping");
        assert_eq!(
            MobSim::cat_gift_chance(24_000 + 12_000),
            0.0,
            "a second day's middle must read the same as the first's"
        );
    }

    /// A cat's [`MobController::request_gift`] call, drained through one real
    /// `MobSim::tick`, must reach an actual item entity when the gift chance
    /// is favourable — proving the production drain (`gift_requests` →
    /// `resolve_cat_gifts`), not merely that the resolver function works when
    /// called directly. Forty cats rather than one: at chance `0.7` the
    /// probability every single one fails is `0.3^40`, indistinguishable
    /// from zero, so this is not a flaky roll of the dice — it is the
    /// deterministic-in-practice discriminator against a wiring break that
    /// would make it fail **every** time instead.
    #[test]
    fn a_cats_gift_request_reaches_a_real_item_through_one_production_tick() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.day_time = 23_800; // inside the 0.7 window
        for i in 0..40 {
            let id = sim.spawn_species(cat_key(), Vec3::new(f64::from(i) * 8.0, 64.0, 0.0)).id();
            sim.get_mut(id).expect("just spawned").mob.request_gift();
        }
        assert_eq!(sim.item_count(), 0, "no item exists before the tick that drains the request");
        sim.tick();
        assert!(
            sim.item_count() > 0,
            "at a 0.7 gift chance across 40 requests, at least one real item entity must exist \
             after one production tick"
        );
    }

    /// The deterministic negative control for the same chain: outside the
    /// gift window the chance is a **hard** `0.0` (not merely lower), so no
    /// request — however many — can ever produce an item. This is what
    /// separates "the wiring reaches the resolver" from "the resolver always
    /// spawns something regardless of the roll".
    #[test]
    fn a_cats_gift_request_never_lands_outside_the_gift_window() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.day_time = 12_000; // dead centre of the 0.0 window
        for i in 0..40 {
            let id = sim.spawn_species(cat_key(), Vec3::new(f64::from(i) * 8.0, 64.0, 0.0)).id();
            sim.get_mut(id).expect("just spawned").mob.request_gift();
        }
        sim.tick();
        assert_eq!(sim.item_count(), 0, "a 0.0 gift chance must never spawn an item, however many cats request one");
    }

    /// A parrot's [`MobController::request_shoulder_ride`] call, drained
    /// through one real tick, removes the mob from the world and records a
    /// shoulder rider for its owner — the mount half. Then, once the owner
    /// is reported asleep and the 20-tick minimum ride has elapsed, the next
    /// several ticks must dismount it: the parrot reappears as a real mob
    /// again and the shoulder-rider slot clears. Both halves driven through
    /// production `MobSim::tick`, not through `resolve_shoulder_mounts`/
    /// `tick_shoulder_dismounts` called directly.
    #[test]
    fn a_parrots_shoulder_request_despawns_it_and_a_sleeping_owner_dismounts_it_back() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let owner_uuid = Uuid::new_v4();
        let owner_entity_id = 500;
        sim.set_players(vec![PerceivedPlayer {
            identity: Some(PlayerIdentity {
                uuid: owner_uuid,
                entity_id: owner_entity_id,
            }),
            perception: PlayerPerception {
                position: Vec3::new(0.0, 64.0, 0.0),
                held_item: None,
                view_direction: Vec3::new(0.0, 0.0, 1.0),
            },
        }]);
        let id = sim.spawn_species(parrot_key(), Vec3::new(0.0, 64.0, 0.0)).id();
        sim.get_mut(id).expect("just spawned").tame(MobOwner::Player(owner_uuid));
        sim.get_mut(id).expect("just spawned").mob.request_shoulder_ride();

        sim.tick();

        assert_eq!(
            sim.mobs.iter().filter(|m| m.entity_type.path() == "parrot").count(),
            0,
            "a mounted parrot must be removed from the world, matching vanilla's own \
             shoulder-mount setter's discard"
        );
        assert_eq!(sim.shoulder_riders.len(), 1, "the mount must be recorded for its owner");

        // The owner falls asleep. The dismount is gated on the 20-tick
        // minimum ride (vanilla's own per-player shoulder-mount timer plus 20), so this
        // must run well past that before asserting.
        let since = sim.tick_count;
        sim.set_sleeping_players(vec![(owner_entity_id, since)]);
        for _ in 0..30 {
            sim.tick();
        }

        assert_eq!(
            sim.mobs.iter().filter(|m| m.entity_type.path() == "parrot").count(),
            1,
            "a sleeping owner past the ride-cooldown grace period must dismount and respawn the parrot"
        );
        assert!(sim.shoulder_riders.is_empty(), "the shoulder slot must clear on dismount");
    }

    /// The 20-tick minimum-ride grace period, isolated: an owner reported
    /// asleep on the *same* tick the parrot mounts must not dismount it
    /// immediately — the `mounted_tick + 20 < game_tick` grace-period guard.
    #[test]
    fn a_freshly_mounted_parrot_does_not_dismount_before_the_grace_period() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let owner_uuid = Uuid::new_v4();
        let owner_entity_id = 501;
        sim.set_players(vec![PerceivedPlayer {
            identity: Some(PlayerIdentity {
                uuid: owner_uuid,
                entity_id: owner_entity_id,
            }),
            perception: PlayerPerception {
                position: Vec3::new(0.0, 64.0, 0.0),
                held_item: None,
                view_direction: Vec3::new(0.0, 0.0, 1.0),
            },
        }]);
        let id = sim.spawn_species(parrot_key(), Vec3::new(0.0, 64.0, 0.0)).id();
        sim.get_mut(id).expect("just spawned").tame(MobOwner::Player(owner_uuid));
        sim.get_mut(id).expect("just spawned").mob.request_shoulder_ride();
        sim.tick();
        assert_eq!(sim.shoulder_riders.len(), 1);

        let since = sim.tick_count;
        sim.set_sleeping_players(vec![(owner_entity_id, since)]);
        // Well under the 20-tick grace period.
        for _ in 0..5 {
            sim.tick();
        }
        assert_eq!(
            sim.shoulder_riders.len(),
            1,
            "a parrot inside its 20-tick minimum ride must not dismount just because the owner sleeps"
        );
    }
}

/// The cat block search (`MobSim::tick_cat_block_search`) uses a host-computed
/// candidate position.
#[cfg(test)]
mod cat_block_search_tests {
    use super::*;

    /// A real floor — see `leash_tests::flat_world`'s own doc comment for
    /// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
    /// Set at `y = -1` so it never collides with a search target block a
    /// test places at `y = 0`.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    fn spawn_cat(sim: &mut MobSim<'_>, pos: Vec3) -> i32 {
        sim.spawn_species("minecraft:cat".parse().expect("valid key"), pos).id()
    }

    /// The headline case: a chest three blocks away, with clear headroom,
    /// must be found and fed to the sit goal's seam — and the bed seam must
    /// stay empty, since nothing bed-shaped exists in this world.
    #[test]
    fn a_cat_finds_a_nearby_chest_as_its_sit_target() {
        let mut world = flat_world();
        world.set_block(3, 0, 0, "minecraft:chest");
        let mut sim = MobSim::new(&world);
        let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

        sim.tick();

        let cat = sim.get(id).expect("just spawned");
        let target = cat.mob.cat_sit_target();
        assert_eq!(
            target,
            Some(Vec3::new(3.5, 1.0, 0.5)),
            "the sit target must be the chest's stand-on point, got {target:?}"
        );
        assert_eq!(cat.mob.cat_bed_target(), None, "no bed exists in this world");
    }

    /// A bed's *foot* part must feed both seams: the sit goal accepts a bed
    /// foot (vanilla's own valid-target check's own third clause) and the
    /// lie goal accepts any bed part.
    #[test]
    fn a_cat_finds_a_nearby_bed_foot_for_both_seams() {
        let mut world = flat_world();
        world.set_block(0, 0, 2, "minecraft:red_bed[facing=north,part=foot]");
        let mut sim = MobSim::new(&world);
        let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

        sim.tick();

        let cat = sim.get(id).expect("just spawned");
        assert!(cat.mob.cat_sit_target().is_some(), "a bed foot is a valid sit target too");
        assert!(cat.mob.cat_bed_target().is_some(), "a bed foot is a valid lie target");
    }

    /// A bed's *head* part must be excluded from the sit seam
    /// (vanilla's own valid-target check's "not the head part" clause) but
    /// still accepted by the lie seam, which makes no part distinction at
    /// all (vanilla's own lie-goal valid-target check).
    #[test]
    fn a_beds_head_part_is_excluded_from_sitting_but_not_from_lying() {
        let mut world = flat_world();
        world.set_block(0, 0, 2, "minecraft:red_bed[facing=north,part=head]");
        let mut sim = MobSim::new(&world);
        let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

        sim.tick();

        let cat = sim.get(id).expect("just spawned");
        assert_eq!(cat.mob.cat_sit_target(), None, "a bed head must not be a sit target");
        assert!(cat.mob.cat_bed_target().is_some(), "a bed head is still a valid lie target");
    }

    /// An unlit furnace is not a valid sit target — only the lit furnace
    /// state qualifies (vanilla's own valid-target check's second clause).
    #[test]
    fn an_unlit_furnace_is_not_a_sit_target_but_a_lit_one_is() {
        let mut world = flat_world();
        world.set_block(0, 0, 2, "minecraft:furnace[facing=north,lit=false]");
        let mut sim = MobSim::new(&world);
        let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));
        sim.tick();
        assert_eq!(
            sim.get(id).expect("spawned").mob.cat_sit_target(),
            None,
            "an unlit furnace must not be a sit target"
        );

        let mut world2 = flat_world();
        world2.set_block(0, 0, 2, "minecraft:furnace[facing=north,lit=true]");
        let mut sim2 = MobSim::new(&world2);
        let id2 = spawn_cat(&mut sim2, Vec3::new(0.0, 0.0, 0.0));
        sim2.tick();
        assert!(
            sim2.get(id2).expect("spawned").mob.cat_sit_target().is_some(),
            "a lit furnace must be a sit target"
        );
    }

    /// A chest with no headroom (a solid block directly above it) must be
    /// rejected — vanilla's own valid-target check's
    /// "is empty block one above" clause.
    #[test]
    fn a_chest_with_no_headroom_is_rejected() {
        let mut world = flat_world();
        world.set_block(0, 0, 2, "minecraft:chest");
        world.set_block(0, 1, 2, "minecraft:stone");
        let mut sim = MobSim::new(&world);
        let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

        sim.tick();

        assert_eq!(
            sim.get(id).expect("spawned").mob.cat_sit_target(),
            None,
            "a chest with a solid block on top must not be a sit target"
        );
    }

    /// An empty world (no chest, furnace or bed anywhere in range) must
    /// leave both seams empty — the negative control.
    #[test]
    fn an_empty_world_finds_neither_target() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = spawn_cat(&mut sim, Vec3::new(0.0, 0.0, 0.0));

        sim.tick();

        let cat = sim.get(id).expect("just spawned");
        assert_eq!(cat.mob.cat_sit_target(), None);
        assert_eq!(cat.mob.cat_bed_target(), None);
    }

    /// A non-cat species (a villager, which also walks around chests every
    /// day) must never receive a cat block search feed — the species filter
    /// is load-bearing, not merely a cost optimisation.
    #[test]
    fn a_non_cat_species_never_receives_a_cat_block_search_feed() {
        let mut world = flat_world();
        world.set_block(1, 0, 0, "minecraft:chest");
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:villager".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();

        sim.tick();

        assert_eq!(sim.get(id).expect("spawned").mob.cat_sit_target(), None);
    }
}

/// Villager bed claiming runs through `tick_villager_beds` and
/// `occupied_homes_in_range` in the real per-tick [`MobSim`] loop, exercising
/// the integrated path rather than standalone helpers.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod villager_bed_claim_tests {
    use super::*;

    /// A real floor — see `leash_tests::flat_world`'s own doc comment for
    /// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    fn spawn_villager(sim: &mut MobSim<'_>, pos: Vec3) -> i32 {
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), pos).id()
    }

    /// The headline case: a villager standing near an unclaimed bed claims it
    /// over a real tick, and the claim is visible through the same live query
    /// the raid trigger needs (`occupied_homes_in_range`).
    #[test]
    fn a_villager_claims_a_nearby_bed_through_a_real_tick_and_it_becomes_findable() {
        let mut world = flat_world();
        world.set_block(3, 0, 0, "minecraft:red_bed[facing=north,occupied=false,part=foot]");
        let mut sim = MobSim::new(&world);
        let id = spawn_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));

        sim.tick();

        let claimed = sim.get(id).expect("just spawned").bed();
        assert_eq!(
            claimed,
            Some(BlockPos::new(3, 0, 0)),
            "the villager must claim the only nearby bed"
        );
        let found = sim.occupied_homes_in_range(BlockPos::new(0, 0, 0), 64);
        assert_eq!(
            found,
            vec![BlockPos::new(3, 0, 0)],
            "the raid trigger's live query must see the claim the same tick it happens"
        );
    }

    /// Two villagers, one bed — the discriminating shape every claim gate in
    /// this file uses: a single-villager test would pass under an
    /// implementation with no occupancy at all.
    #[test]
    fn a_second_villager_cannot_claim_an_already_claimed_bed_through_the_sim() {
        let mut world = flat_world();
        world.set_block(3, 0, 0, "minecraft:red_bed[facing=north,occupied=false,part=foot]");
        let mut sim = MobSim::new(&world);
        let first = spawn_villager(&mut sim, Vec3::new(0.0, 0.0, 0.0));
        let second = spawn_villager(&mut sim, Vec3::new(1.0, 0.0, 0.0));

        sim.tick();

        let first_bed = sim.get(first).expect("spawned").bed();
        let second_bed = sim.get(second).expect("spawned").bed();
        assert_eq!(first_bed, Some(BlockPos::new(3, 0, 0)));
        assert_eq!(
            second_bed, None,
            "the bed has one ticket and the closer villager already holds it"
        );
    }

    /// A non-villager species standing right next to a bed must never claim
    /// it — the species filter is load-bearing, the same control
    /// `cat_block_search_tests` already runs for its own search.
    #[test]
    fn a_non_villager_species_never_claims_a_bed() {
        let mut world = flat_world();
        world.set_block(1, 0, 0, "minecraft:red_bed[facing=north,occupied=false,part=foot]");
        let mut sim = MobSim::new(&world);
        let id = sim
            .spawn_species("minecraft:pig".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0))
            .id();

        sim.tick();

        assert_eq!(sim.get(id).expect("spawned").bed(), None);
        assert!(sim.occupied_homes_in_range(BlockPos::new(0, 0, 0), 64).is_empty());
    }
}

/// WORK/MEET/REST schedule: proves the chain claimed-POI ->
/// `MobSim::set_day_time` -> `crate::brain::roster::villager_brain`'s
/// schedule -> `WalkToPoi`/`MoveToTargetSink` -> a real position change
/// reaches a real, spawned villager through `MobSim::tick`, the same
/// not-an-island bar `villager_bed_claim_tests` already sets for bed
/// claiming and `vibration_substrate_tests` sets for the warden.
#[cfg(test)]
mod villager_schedule_tests {
    use super::*;

    /// A flat, walkable floor wide enough that pathfinding across it never
    /// runs off the edge — the same shape `a_grazing_mob_hands_its_eat_to_the_driver`
    /// already establishes for a real multi-tick walk.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -32..=32 {
            for z in -32..=32 {
                world.set_block(x, -1, z, "minecraft:grass_block");
            }
        }
        world
    }

    fn spawn_villager(sim: &mut MobSim<'_>, pos: Vec3) -> i32 {
        sim.spawn_species("minecraft:villager".parse().expect("valid key"), pos).id()
    }

    fn horizontal_distance(a: Vec3, b: Vec3) -> f64 {
        (a.x - b.x).hypot(a.z - b.z)
    }

    /// **The headline case.** A villager spawns 20 blocks from a composter
    /// (a real `farmer` job site), stays idle at a day time before `WORK`
    /// starts (`2000`, `VILLAGER_SCHEDULE`'s own keyframe), then the clock
    /// enters the `WORK` window and the villager visibly closes most of the
    /// distance to its claimed workstation over real ticks — not merely
    /// "the position changed", a magnitude check against the starting gap,
    /// so a villager that only ever random-strolls (and might coincidentally
    /// drift a block or two toward the composter) cannot pass this by luck.
    #[test]
    fn a_villager_walks_to_its_claimed_workstation_once_work_begins() {
        let mut world = flat_world();
        // Inside `villager::SEARCH_RADIUS` (16 blocks) so the villager's
        // bounded job search can actually find it, and far enough past
        // `WalkToPoi`'s own 9-block close-enough radius that "arrived" and
        // "started here" are unambiguously different distances.
        let composter = BlockPos::new(15, 0, 0);
        world.set_block(composter.x, composter.y, composter.z, "minecraft:composter");

        let mut sim = MobSim::new(&world);
        let id = spawn_villager(&mut sim, Vec3::new(0.5, 0.0, 0.5));

        // Before `WORK` (schedule keyframe `2000`): let the villager claim
        // the workstation (job search is unthrottled on its first tick) but
        // never let the clock enter `WORK`, so any position drift here is
        // attributable only to `IDLE`'s own random stroll, not to this
        // schedule.
        for _ in 0..5 {
            sim.set_day_time(500);
            sim.tick();
        }
        assert_eq!(
            sim.get(id).expect("just spawned").workstation(),
            Some(composter),
            "the villager must have claimed the only nearby composter before WORK ever starts"
        );

        let workstation_center = Vec3::new(
            f64::from(composter.x) + 0.5,
            f64::from(composter.y) + 0.5,
            f64::from(composter.z) + 0.5,
        );
        let initial_distance = horizontal_distance(sim.get(id).expect("spawned").position(), workstation_center);

        // Now enter the WORK window and let the schedule + WalkToPoi close
        // the gap over real ticks.
        for _ in 0..400 {
            sim.set_day_time(3000);
            sim.tick();
        }
        let final_distance = horizontal_distance(sim.get(id).expect("spawned").position(), workstation_center);

        // Two predictions, not just "it got closer": `WalkToPoi::new(JOB_SITE,
        // …, 9)` stops issuing a fresh walk target once within 9 blocks
        // (`MoveToTargetSink::reached`'s own `+ 0.5` tolerance), so a working
        // villager should end up **near that exact radius**, not merely
        // "somewhat closer" — which a lucky IDLE stroll could also produce.
        assert!(
            final_distance <= 10.5,
            "a villager working its claimed job site should stop within WalkToPoi's own \
             9-block close-enough radius (plus MoveToTargetSink's 0.5 tolerance): \
             started {initial_distance:.1} blocks away, ended {final_distance:.1}"
        );
        assert!(
            initial_distance - final_distance > 4.0,
            "WORK must walk the villager measurably closer to its claimed workstation: \
             started {initial_distance:.1} blocks away, ended {final_distance:.1}"
        );
    }

    /// [`a_villager_walks_to_its_claimed_workstation_once_work_begins`]'s own
    /// sibling for `MEET`/bells rather than `WORK`/workstations — proving the
    /// third POI (the one this session's own `BellClaims` adds) reaches the
    /// identical real chain: claim -> schedule -> `WalkToPoi` -> a real
    /// position change.
    #[test]
    fn a_villager_walks_to_its_claimed_bell_once_meet_begins() {
        let mut world = flat_world();
        let bell = BlockPos::new(15, 0, 0);
        world.set_block(bell.x, bell.y, bell.z, "minecraft:bell[attachment=floor,facing=south]");

        let mut sim = MobSim::new(&world);
        let id = spawn_villager(&mut sim, Vec3::new(0.5, 0.0, 0.5));

        // Before `MEET` (schedule keyframe `9000`): let the villager claim
        // the bell but keep the clock in `IDLE`'s own window.
        for _ in 0..5 {
            sim.set_day_time(500);
            sim.tick();
        }
        assert_eq!(
            sim.get(id).expect("just spawned").meeting_point(),
            Some(bell),
            "the villager must have claimed the only nearby bell before MEET ever starts"
        );

        let bell_center = Vec3::new(f64::from(bell.x) + 0.5, f64::from(bell.y) + 0.5, f64::from(bell.z) + 0.5);
        let initial_distance = horizontal_distance(sim.get(id).expect("spawned").position(), bell_center);

        for _ in 0..400 {
            sim.set_day_time(9500);
            sim.tick();
        }
        let final_distance = horizontal_distance(sim.get(id).expect("spawned").position(), bell_center);

        // `WalkToPoi::new(MEETING_POINT, …, 6)` — a tighter close-enough
        // radius than the job site's `9`, which is the meeting-point walk
        // target's range.
        assert!(
            final_distance <= 7.5,
            "a villager meeting at its claimed bell should stop within WalkToPoi's own \
             6-block close-enough radius (plus MoveToTargetSink's 0.5 tolerance): \
             started {initial_distance:.1} blocks away, ended {final_distance:.1}"
        );
        assert!(
            initial_distance - final_distance > 4.0,
            "MEET must walk the villager measurably closer to its claimed bell: \
             started {initial_distance:.1} blocks away, ended {final_distance:.1}"
        );
    }

    /// The schedule's own negative control: a villager with **no** claimed
    /// job site (nothing nearby to claim) never becomes `WORK`-eligible —
    /// the generic villager activity requirement that a job-site memory be
    /// present — so it stays wherever `IDLE`'s random stroll leaves
    /// it: never *reliably* walking toward a fixed faraway point regardless
    /// of the clock. Asserted as "never claims a workstation", the
    /// discriminating fact this control actually has available deterministically
    /// (a stroll's own endpoint is randomised and not itself a safe assertion).
    #[test]
    fn a_villager_with_no_nearby_job_site_never_claims_one_regardless_of_the_clock() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let id = spawn_villager(&mut sim, Vec3::new(0.5, 0.0, 0.5));

        for _ in 0..200 {
            sim.set_day_time(3000);
            sim.tick();
        }

        assert_eq!(
            sim.get(id).expect("spawned").workstation(),
            None,
            "with no workstation block anywhere nearby there is nothing to claim, \
             so WORK can never become eligible no matter how long the clock sits in its window"
        );
    }
}

/// Vibration events produced by `reap_dead` reach `resolve_vibrations` through
/// the real per-tick [`MobSim`] loop.
#[cfg(test)]
mod vibration_substrate_tests {
    use super::*;

    /// A real floor — see `leash_tests::flat_world`'s own doc comment for
    /// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
    /// `24` on X covers this module's own `16.1`-block "just outside the
    /// listener radius" control with margin.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=24 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    fn spawn(sim: &mut MobSim<'_>, species: &str, pos: Vec3) -> i32 {
        sim.spawn_species(format!("minecraft:{species}").parse().expect("valid key"), pos)
            .id()
    }

    /// Production-path proof for the door/float/malus shape fix: drives the
    /// real `MobSim::spawn_species` entry point (not `species_shape` in
    /// isolation) and reads back the `MobShape` a `NavigatingMob` would
    /// actually path with. A vindicator opening a door is the headline case
    /// from vanilla's own vindicator spawn-finalization's unconditional
    /// navigation "can open doors" setter, and vanilla's own villager constructor
    /// sets both `canOpenDoors` and `canFloat` unconditionally too.
    #[test]
    fn vindicator_and_villager_can_open_doors_and_float() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let vindicator = spawn(&mut sim, "vindicator", Vec3::new(0.0, 0.0, 0.0));
        let villager = spawn(&mut sim, "villager", Vec3::new(5.0, 0.0, 0.0));

        let vindicator_shape = sim.get(vindicator).expect("spawned").shape();
        assert!(vindicator_shape.can_open_doors);
        assert!(vindicator_shape.can_float);

        let villager_shape = sim.get(villager).expect("spawned").shape();
        assert!(villager_shape.can_open_doors);
        assert!(villager_shape.can_float);
    }

    /// Control: an ordinary land animal with no special-cased goals gets
    /// neither flag; the defaults are off for all species in this case.
    #[test]
    fn a_plain_animal_still_cannot_open_doors() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let pig = spawn(&mut sim, "pig", Vec3::new(0.0, 0.0, 0.0));
        let shape = sim.get(pig).expect("spawned").shape();
        assert!(!shape.can_open_doors);
        // Pigs retain floating behavior, so this one is `true`.
        assert!(shape.can_float);
    }

    /// Vanilla's own bee spawn-finalization's malus table (`WATER` -1, `FENCE` -1) is the
    /// path-malus behavior: `malus_overrides` has entries for
    /// `.insert` calls anywhere in the workspace, so every mob pathed as if
    /// nothing were dangerous. `PathType::malus`'s own default for `Water` is
    /// `8.0` (costly but passable) and for `Fence` is `-1.0` already, so
    /// `Water` is the discriminating field here — a bee must come back
    /// strictly more averse to water than the vanilla default, not merely
    /// non-zero.
    #[test]
    fn a_bee_s_malus_overrides_reach_the_navigating_shape() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let bee = spawn(&mut sim, "bee", Vec3::new(0.0, 0.0, 0.0));
        let shape = sim.get(bee).expect("spawned").shape();
        assert_eq!(shape.malus(PathType::Water), -1.0);
        assert_ne!(
            shape.malus(PathType::Water),
            PathType::Water.malus(),
            "bee must diverge from the un-overridden default, not coincide with it"
        );
        assert_eq!(shape.malus(PathType::Fence), -1.0);
    }

    /// Vanilla's own zombie spawn-finalization's door-breaking roll is a coin flip scaled by
    /// regional difficulty (`random.nextFloat() < difficultyModifier * 0.1F`),
    /// not a species constant — this is the control proving the roll is
    /// actually wired to `spawn_special_multiplier` rather than a fixed
    /// constant in either direction. At multiplier `0.0` the roll is
    /// deterministically `false` for every draw (`x < 0.0` never holds for
    /// `x` in `[0.0, 1.0)`), so this is exact, not statistical.
    #[test]
    fn zombie_door_roll_is_scaled_by_regional_difficulty_not_constant() {
        let world = flat_world();

        let mut off = MobSim::new(&world);
        off.set_spawn_difficulty(0.0, false);
        for i in 0..20 {
            let z = spawn(&mut off, "zombie", Vec3::new(i as f64 * 3.0, 0.0, 0.0));
            assert!(!off.get(z).expect("spawned").shape().can_open_doors);
        }

        // At the maximum multiplier every draw has a real (~10%) chance, so
        // spawning enough zombies must produce at least one `true` — the
        // reciprocal control to the all-`false` case above. `next_f32() <
        // 1.0 * 0.1` succeeds for roughly one in ten draws; 200 spawns makes
        // a run of all-`false` astronomically unlikely (`0.9^200 < 1e-9`)
        // without pinning to a specific seeded count.
        let mut on = MobSim::new(&world);
        on.set_spawn_difficulty(1.0, false);
        let ids: Vec<i32> = (0..200)
            .map(|i| spawn(&mut on, "husk", Vec3::new(i as f64 * 3.0, 0.0, 0.0)))
            .collect();
        let any_open = ids
            .iter()
            .any(|&id| on.get(id).expect("spawned").shape().can_open_doors);
        assert!(any_open, "expected at least one husk to roll door-breaking true at multiplier 1.0");
    }

    /// The roll must survive [`SimMob::set_age`]'s baby/adult shape refresh —
    /// The age transition must preserve the sampled roll rather than re-derive the
    /// static species default and discard a `true` value.
    #[test]
    fn a_zombie_s_door_roll_survives_growing_up() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.set_spawn_difficulty(1.0, false);
        let ids: Vec<i32> = (0..200)
            .map(|i| spawn(&mut sim, "zombie", Vec3::new(i as f64 * 3.0, 0.0, 0.0)))
            .collect();
        let id = *ids
            .iter()
            .find(|&&id| sim.get(id).expect("spawned").shape().can_open_doors)
            .expect("expected at least one zombie to roll door-breaking true at multiplier 1.0");

        sim.get_mut(id).expect("spawned").set_age(BABY_START_AGE);
        sim.get_mut(id).expect("spawned").set_age(0);

        assert!(
            sim.get(id).expect("spawned").shape().can_open_doors,
            "growing up must not reset a rolled-true door flag back to the static default"
        );
    }

    /// Zombie reinforcement: only the *roll* is this sim's job — see
    /// `ReinforcementCall` for the decide-here/place-there split. Hard
    /// difficulty, `spawn_mobs` enabled,
    /// and `reinforcement_chance` pinned to `1.0` (`next_f32() < 1.0` always
    /// holds in `[0.0, 1.0)`, so this is exact, not statistical) must queue
    /// exactly one call carrying the zombie's own type, position and — no AI
    /// target set on this mob — the attacking player's own entity id as the
    /// fallback when no live target is available.
    #[test]
    fn a_hurt_zombie_calls_a_reinforcement_when_the_roll_passes() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.set_spawn_difficulty(0.0, true);
        sim.set_spawn_monsters_enabled(true);
        let id = spawn(&mut sim, "zombie", Vec3::new(0.0, 0.0, 0.0));
        sim.get_mut(id).expect("spawned").reinforcement_chance = 1.0;
        let attacker = PlayerIdentity { uuid: Uuid::new_v4(), entity_id: 777 };

        let outcome = sim.attack_from_player(
            id,
            Some(attacker),
            Vec3::new(1.0, 0.0, 0.0),
            1.0,
            DamageFlags::default(),
            0.0,
        );
        assert!(outcome.is_some_and(|o| !o.killed), "one point of damage must not kill a zombie");

        let calls = sim.take_reinforcement_calls();
        assert_eq!(calls.len(), 1, "the roll was pinned to 1.0 — it must always fire");
        assert_eq!(calls[0].entity_type.path(), "zombie");
        // Near the spawn point, not exactly on it — the hit's own mandatory
        // knockback moves the zombie before this roll reads its position, the
        // same `dealDefaultKnockback` every landed hit applies.
        let dist_sqr = calls[0].position.x.powi(2) + calls[0].position.y.powi(2) + calls[0].position.z.powi(2);
        assert!(dist_sqr < 4.0, "expected the caller's position near its spawn point, got {:?}", calls[0].position);
        assert_eq!(calls[0].target_id, 777, "falls back to the attacker with no AI target set");
    }

    /// **Control:** the identical setup, but the mob is only skeleton-family
    /// — `reinforcement_chance` stays `0.0` for every species outside the
    /// zombie family, so even a Hard-difficulty hit queues nothing. The
    /// discriminating control against "the gate is difficulty alone".
    #[test]
    fn only_the_zombie_family_ever_calls_for_reinforcements() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.set_spawn_difficulty(1.0, true);
        sim.set_spawn_monsters_enabled(true);
        let id = spawn(&mut sim, "skeleton", Vec3::new(0.0, 0.0, 0.0));
        assert_eq!(
            sim.get(id).expect("spawned").reinforcement_chance(),
            0.0,
            "only the zombie family rolls a nonzero chance at spawn"
        );

        sim.attack_from_player(
            id,
            Some(PlayerIdentity { uuid: Uuid::new_v4(), entity_id: 777 }),
            Vec3::new(1.0, 0.0, 0.0),
            1.0,
            DamageFlags::default(),
            0.0,
        );
        assert!(sim.take_reinforcement_calls().is_empty());
    }

    /// **Control:** the identical zombie/roll setup below Hard difficulty
    /// must queue nothing — `level.getDifficulty() == Difficulty.HARD` is a
    /// hard gate in vanilla, not folded into the continuous chance roll, so
    /// a saturated `special_multiplier` (`1.0`, Normal/Easy's ceiling) must
    /// not substitute for it.
    #[test]
    fn a_hurt_zombie_calls_no_reinforcement_below_hard_difficulty() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.set_spawn_difficulty(1.0, false);
        sim.set_spawn_monsters_enabled(true);
        let id = spawn(&mut sim, "zombie", Vec3::new(0.0, 0.0, 0.0));
        sim.get_mut(id).expect("spawned").reinforcement_chance = 1.0;

        sim.attack_from_player(
            id,
            Some(PlayerIdentity { uuid: Uuid::new_v4(), entity_id: 777 }),
            Vec3::new(1.0, 0.0, 0.0),
            1.0,
            DamageFlags::default(),
            0.0,
        );
        assert!(
            sim.take_reinforcement_calls().is_empty(),
            "Hard is a hard gate, not part of the continuous chance roll"
        );
    }

    /// The headline case: a mob dies within 16 blocks of a warden, and the
    /// same tick's `resolve_vibrations` (run after `reap_dead` posts) hands
    /// the warden the death's own position as an `EntityDie` vibration.
    #[test]
    fn a_warden_hears_a_nearby_death_the_same_tick_it_happens() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
        let victim = spawn(&mut sim, "zombie", Vec3::new(10.0, 0.0, 0.0));
        sim.get_mut(victim).expect("spawned").health = 0.0;

        sim.tick();

        let heard = sim.get(warden).expect("spawned").nearest_vibration();
        assert_eq!(
            heard,
            Some(PostedVibration {
                position: Vec3::new(10.0, 0.0, 0.0),
                event: VibrationEvent::EntityDie,
                source: Some(victim),
            }),
            "the warden must hear the death at the victim's own position, the same tick"
        );
    }

    /// A death just outside the 16-block listener radius must not be heard —
    /// the discriminating control against "the warden hears everything".
    #[test]
    fn a_death_just_outside_the_listener_radius_is_not_heard() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
        let victim = spawn(&mut sim, "zombie", Vec3::new(16.1, 0.0, 0.0));
        sim.get_mut(victim).expect("spawned").health = 0.0;

        sim.tick();

        assert_eq!(
            sim.get(warden).expect("spawned").nearest_vibration(),
            None,
            "16.1 blocks away must not be audible at the warden's 16.0 radius"
        );
    }

    /// A non-listener species standing right next to the same death must
    /// never receive a vibration — the species filter is load-bearing, the
    /// same control every other search in this file runs for its own gate.
    #[test]
    fn a_non_listener_species_never_receives_a_vibration() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let bystander = spawn(&mut sim, "zombie", Vec3::new(0.0, 0.0, 0.0));
        let victim = spawn(&mut sim, "zombie", Vec3::new(1.0, 0.0, 0.0));
        sim.get_mut(victim).expect("spawned").health = 0.0;

        sim.tick();

        assert_eq!(sim.get(bystander).expect("spawned").nearest_vibration(), None);
    }

    /// The posted log must not leak into the next tick: a warden that hears
    /// a death on tick 1 must hear nothing new (and retain no stale answer)
    /// on tick 2, when nothing else has died.
    #[test]
    fn the_posted_log_does_not_leak_into_the_next_tick() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let warden = spawn(&mut sim, "warden", Vec3::new(0.0, 0.0, 0.0));
        let victim = spawn(&mut sim, "zombie", Vec3::new(5.0, 0.0, 0.0));
        sim.get_mut(victim).expect("spawned").health = 0.0;

        sim.tick();
        assert!(sim.get(warden).expect("spawned").nearest_vibration().is_some());

        sim.tick();
        assert_eq!(
            sim.get(warden).expect("spawned").nearest_vibration(),
            None,
            "a vibration from a prior tick must not persist once nothing new was posted"
        );
    }
}

/// The elder guardian's mining-fatigue aura,
/// vanilla's own elder-guardian AI step calling
/// its own "add effect to players around" helper.
#[cfg(test)]
mod elder_guardian_mining_fatigue_tests {
    use super::*;

    /// A real floor near the origin — see `leash_tests::flat_world`'s own
    /// doc comment for why a bare void `ChunkWorld` stopped being safe once
    /// idle mobs fall. This module's `x = 60` position is a *player*
    /// (`set_players`), not a spawned mob, so it is unaffected either way
    /// and is deliberately left outside the floor.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    fn player_at(uuid: Uuid, pos: Vec3) -> PerceivedPlayer {
        PerceivedPlayer {
            identity: Some(PlayerIdentity { uuid, entity_id: 99 }),
            perception: PlayerPerception {
                position: pos,
                held_item: None,
                view_direction: Vec3::new(0.0, 0.0, 1.0),
            },
        }
    }

    /// `(tickCount + getId()) % 1200 == 0`, with `tick_count` standing in for
    /// vanilla's own generic tick-count field (see [`ELDER_GUARDIAN_EFFECT_INTERVAL`]'s own doc).
    /// A freshly spawned elder guardian gets id `1`, so the trigger tick is
    /// `1200 - 1 = 1199`; [`MobSim::tick`] reads `self.tick_count` *before*
    /// incrementing it, so seeding `set_tick_count(1199)` and ticking once is
    /// the tick this pulse fires on.
    #[test]
    fn a_player_within_fifty_blocks_is_pulsed_on_the_interval_tick() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let guardian_id = sim
            .spawn_species(
                "minecraft:elder_guardian".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();
        assert_eq!(guardian_id, 1, "precondition: the trigger-tick arithmetic below assumes id 1");

        let alice = Uuid::from_u128(0xA11CE);
        sim.set_players(vec![player_at(alice, Vec3::new(40.0, 0.0, 0.0))]);
        sim.set_tick_count(1199);
        sim.tick();

        let pulses = sim.take_mining_fatigue_auras();
        assert_eq!(
            pulses,
            vec![MiningFatigueAura {
                target: PlayerIdentity {
                    uuid: alice,
                    entity_id: 99
                }
            }],
            "a player 40 blocks away (within the 50-block radius) must be pulsed on tick 1199, got {pulses:?}"
        );
    }

    /// The same setup, moved just past `EFFECT_RADIUS` — the spherical
    /// distance cut, not a box.
    #[test]
    fn a_player_beyond_fifty_blocks_is_not_pulsed() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let guardian_id = sim
            .spawn_species(
                "minecraft:elder_guardian".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();
        assert_eq!(guardian_id, 1, "precondition: the trigger-tick arithmetic below assumes id 1");

        let alice = Uuid::from_u128(0xA11CE);
        sim.set_players(vec![player_at(alice, Vec3::new(60.0, 0.0, 0.0))]);
        sim.set_tick_count(1199);
        sim.tick();

        assert!(
            sim.take_mining_fatigue_auras().is_empty(),
            "a player 60 blocks away is outside EFFECT_RADIUS and must not be pulsed"
        );
    }

    /// A tick that is not a multiple of the 1200-tick interval must pulse
    /// nobody, even with a player standing on top of the guardian.
    #[test]
    fn no_pulse_off_the_interval_tick() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        sim.spawn_species(
            "minecraft:elder_guardian".parse().expect("valid key"),
            Vec3::new(0.0, 0.0, 0.0),
        );

        let alice = Uuid::from_u128(0xA11CE);
        sim.set_players(vec![player_at(alice, Vec3::new(0.0, 0.0, 0.0))]);
        sim.set_tick_count(1198);
        sim.tick();

        assert!(
            sim.take_mining_fatigue_auras().is_empty(),
            "one tick before the interval must not fire"
        );
    }

    /// An ordinary guardian (not elder) must never pulse — the aura is
    /// elder-guardian-only in vanilla; the ordinary guardian's own AI step has no
    /// such call.
    #[test]
    fn an_ordinary_guardian_never_pulses() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let guardian_id = sim
            .spawn_species(
                "minecraft:guardian".parse().expect("valid key"),
                Vec3::new(0.0, 0.0, 0.0),
            )
            .id();
        assert_eq!(guardian_id, 1, "precondition: the trigger-tick arithmetic below assumes id 1");

        let alice = Uuid::from_u128(0xA11CE);
        sim.set_players(vec![player_at(alice, Vec3::new(0.0, 0.0, 0.0))]);
        sim.set_tick_count(1199);
        sim.tick();

        assert!(
            sim.take_mining_fatigue_auras().is_empty(),
            "an ordinary guardian must never emit a mining-fatigue pulse"
        );
    }

    /// The magnitude gate: the constants a driver applies must match
    /// vanilla's own elder-guardian effect-duration/effect-amplifier fields, not
    /// a plausible-looking round number.
    #[test]
    fn effect_constants_match_the_jar() {
        assert_eq!(ELDER_GUARDIAN_EFFECT_DURATION, 6000);
        assert_eq!(ELDER_GUARDIAN_EFFECT_AMPLIFIER, 2, "Mining Fatigue III is amplifier 2, zero-indexed");
        assert_eq!(ELDER_GUARDIAN_EFFECT_RADIUS, 50.0);
    }
}

/// Goat spawn-finalization's pre-broken-horn roll and the metadata field
/// ([`crate::protocol::MetadataField::GoatHorns`]) that reaches the client.
/// The test wires both through a real [`MobSim::spawn_species`] call.
#[cfg(test)]
mod goat_horn_tests {
    use super::*;

    /// A real floor — see `leash_tests::flat_world`'s own doc comment for
    /// why a bare void `ChunkWorld` stopped being safe once idle mobs fall.
    fn flat_world() -> ChunkWorld {
        let mut world = ChunkWorld::new(-64, 384);
        for x in -8..=8 {
            for z in -8..=8 {
                world.set_solid(x, -1, z, true);
            }
        }
        world
    }

    /// **Real arithmetic, not merely "sometimes true"**: over a large sample,
    /// the fraction of goats spawned missing a horn must land near
    /// vanilla's own goat spawn-finalization's own `0.1` roll — bounded generously (5%–15%
    /// over 2,000 trials) since this crate's `SpawnRng` is not a
    /// bit-identical port of `java.util.Random` (a disclosed approximation
    /// already established elsewhere in this crate, e.g. `raid::bonus_spawns`).
    #[test]
    fn about_one_in_ten_goats_spawn_missing_a_horn() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let trials = 2000;
        let mut missing = 0;
        for _ in 0..trials {
            let id = sim.spawn_species("minecraft:goat".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0)).id();
            let m = sim.get(id).expect("just spawned");
            if !(m.has_left_horn() && m.has_right_horn()) {
                missing += 1;
            }
        }
        let fraction = f64::from(missing) / f64::from(trials);
        assert!(
            (0.05..=0.15).contains(&fraction),
            "expected roughly 10% of {trials} goats missing a horn, got {missing} ({:.1}%)",
            fraction * 100.0
        );
    }

    /// The discriminating control: a goat that *does* lose a horn loses
    /// exactly one, never both — proves the roll picks a single horn rather
    /// than clearing the pair.
    #[test]
    fn a_goat_that_loses_a_horn_loses_exactly_one() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let mut saw_a_miss = false;
        for _ in 0..500 {
            let id = sim.spawn_species("minecraft:goat".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0)).id();
            let m = sim.get(id).expect("just spawned");
            let horn_count = i32::from(m.has_left_horn()) + i32::from(m.has_right_horn());
            assert!((1..=2).contains(&horn_count), "a goat must never lose both horns at spawn");
            if horn_count == 1 {
                saw_a_miss = true;
            }
        }
        assert!(saw_a_miss, "500 trials at a 10% roll must produce at least one miss");
    }

    /// A non-goat species is never touched by the roll — both accessors stay
    /// their default `true`, matching every other species' "meaningless"
    /// reading.
    #[test]
    fn a_non_goat_species_always_reports_both_horns() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        for _ in 0..20 {
            let id = sim.spawn_species("minecraft:pig".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0)).id();
            let m = sim.get(id).expect("just spawned");
            assert!(m.has_left_horn() && m.has_right_horn());
        }
    }

    /// The wiring proof: `SimMob::snapshot` pushes `MetadataField::GoatHorns`
    /// for a goat, carrying whatever [`SimMob::has_left_horn`]/
    /// [`SimMob::has_right_horn`] actually are — and pushes nothing for a
    /// species this field does not apply to, which is the control that rules
    /// out "always pushed regardless of species".
    #[test]
    fn snapshot_carries_goat_horns_for_a_goat_and_nothing_for_a_pig() {
        let world = flat_world();
        let mut sim = MobSim::new(&world);
        let goat = sim.spawn_species("minecraft:goat".parse().expect("valid key"), Vec3::new(0.0, 0.0, 0.0)).id();
        let pig = sim.spawn_species("minecraft:pig".parse().expect("valid key"), Vec3::new(1.0, 0.0, 0.0)).id();

        let goat_mob = sim.get_mut(goat).expect("spawned");
        goat_mob.has_left_horn = false;
        goat_mob.has_right_horn = true;
        let goat_snapshot = sim.get(goat).expect("spawned").snapshot();
        assert_eq!(
            goat_snapshot.metadata,
            vec![crate::protocol::MetadataField::GoatHorns { has_left: false, has_right: true }],
            "the snapshot must carry the mob's own current horn state, not the spawn default"
        );

        let pig_snapshot = sim.get(pig).expect("spawned").snapshot();
        assert!(
            !pig_snapshot.metadata.iter().any(|f| matches!(f, crate::protocol::MetadataField::GoatHorns { .. })),
            "a pig must never carry a GoatHorns field"
        );
    }
}
