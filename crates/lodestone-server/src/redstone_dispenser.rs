//! Dispensers and droppers (`minecraft:dispenser` / `minecraft:dropper`).
//!
//! # What it is
//!
//! `DispenserBlock`/`DropperBlock` differ in **two** methods, not the one a
//! surface read of `getDispenseMethod` suggests. A dispenser's
//! `getDispenseMethod` looks the held item's `Item` up in
//! `DISPENSER_REGISTRY` (populated once, in `DispenseItemBehavior.bootStrap`)
//! and falls back to a plain toss only when nothing is registered for it —
//! that half is modelled here ([`plain_toss`], the fallback). A dropper
//! hardcodes the plain-toss behaviour for `getDispenseMethod`, **but also
//! overrides `dispenseFrom` itself** (`DropperBlock.dispenseFrom`, not just the
//! method-selection hook `DispenserBlock.getDispenseMethod` names) to check the
//! block directly ahead of it first: if that cell is a real container
//! (`HopperBlockEntity.getContainerAt`), the randomly-picked item is pushed
//! into it via `HopperBlockEntity.addItem` and **never becomes an item
//! entity at all** — only when there is no container ahead does a dropper
//! fall through to the same plain toss a dispenser's fallback uses. **This
//! module now models that container-push check** ([`is_pushable_container`],
//! wired from `tick.rs`'s `TICK_DISPENSER_FIRE` arm through
//! [`crate::hopper::try_move_item_into`]): a dropper facing a real container
//! pushes one item into it and tosses nothing at all; only with no container
//! ahead does it fall through to the plain toss below. A full container is its
//! own third outcome — see [`is_pushable_container`]'s own doc comment for why
//! that is neither a push nor a toss. Getting the *toss* boundary backwards (a
//! dropper that consults the dispenser's item-behaviour table, or a dispenser
//! that never does) is invisible in the overwhelmingly common case — a stack of
//! cobblestone dispenses identically either way — and only shows up the moment
//! someone loads an arrow or points a dropper at a chest.
//!
//! # The behaviour table, derived from the registrations
//!
//! Every entry below is a real `DispenserBlock.registerBehavior`/
//! `registerProjectileBehavior` call inside `DispenseItemBehavior.bootStrap`,
//! or one of the three implicit defaults `DispenserBlock.getDefaultDispenseMethod`
//! falls back to when no explicit registration matches — not a memory-derived
//! guess:
//!
//! | items | behaviour | modelled here |
//! |---|---|---|
//! | arrow, tipped/spectral arrow, egg (+2 chicken-colour variants), snowball, experience bottle, splash/lingering potion, firework rocket, fire charge, wind charge | fires as a projectile entity | **no** — `crate::mobs::MobSim::spawn_projectile_from` exists for a *shooter's* projectile, but `ProjectileItem.createDispenseConfig`'s per-item power/uncertainty/pickup-type table is unported, and the potion-carrying items additionally need a potion-contents item component `lodestone_model::ItemComponents` does not have |
//! | armor stand | spawns one, facing the dispenser's own facing | **no** — `MobSim::spawn_species` is built for `Mob`-shaped entities with goals/AI; [`crate::boat`]'s own doc names the exact hazard for a non-`Mob` `LivingEntity` routed through it ("gives a boat a mob's component set, produces a boat that *wanders*") and an armor stand is not a `Mob` either, so this is left unmodelled rather than risking a wandering prop |
//! | chest | fills a nearby saddled chest-carrying animal, else plain-tosses | no — entity query |
//! | every boat/chest-boat/raft (18 items) | places a riding entity just outside the dispenser's own face, on the water surface ahead (or on the ground beneath, if the cell ahead is air over water) | **yes** ([`boat_dispense`]) |
//! | lava/water/powder-snow/fish/axolotl/sulfur-cube/tadpole bucket | empties into the world ahead | **no** — no item ever triggers a fluid *placement* anywhere in this crate; `crate::fluid` only ticks a fluid already in the world, it has no place/pickup entry point at all |
//! | bucket | picks a fluid up | **no** — same reason |
//! | flint and steel | ignites the block ahead (`FlintAndSteelDispenseItemBehavior`) | **partial** ([`flint_and_steel_ignite`]) — the fire-placement arm only; the sulfur-cube-entity-priming arm needs an entity query this crate has none of, and the TNT-block-priming arm needs a primed-TNT entity (see the TNT row) |
//! | bone meal | grows the crop/water plant ahead (`BoneMealItem.growCrop`) | **yes**, via `crate::bone_meal::apply_bone_meal` — `growWaterPlant` (seagrass/coral) is that module's own pre-existing named gap, not a new one this wiring introduces |
//! | TNT | spawns a primed-TNT entity | **no** — this crate has no primed-TNT entity at all; `crate::fire`'s own module doc names the identical gap for `TntBlock::prime` |
//! | wither skeleton skull | places the skull block, then a `BlockPattern` match for the full wither shape | **no** — needs multi-block shape matching this crate has nowhere |
//! | carved pumpkin | places the block, then a `BlockPattern` match for snow/iron/copper golem shapes | **no** — same missing subsystem |
//! | shulker box (+16 colours) | places the block entity | no |
//! | glass bottle | takes water/honey from ahead | no |
//! | glowstone | charges a respawn anchor | no — this crate hosts the overworld only |
//! | shears | shears a sheep/mooshroom ahead, or a beehive's honey | **no** — no shearing/wool-state mechanism exists anywhere in this crate (not even for a player's own direct use), and no entity query either |
//! | brush | brushes an armadillo ahead | no — entity query |
//! | honeycomb | waxes a copper block ahead | no |
//! | potion (water only, on mud-convertible ground) | places mud | no |
//! | minecart family (6 items) | places a riding entity with real rail-following physics | **no** — this crate has no minecart entity or vehicle physics *at all*; `crate::mobs::TrackedVehicle`/`MobSim::spawn_vehicle` model only `AbstractBoat`, and `crate::redstone_rail`'s own module doc already names "this crate has none" of minecarts |
//! | every armor/trims/saddle/horse-armor/carpet/mob-head-banner item (`getDefaultDispenseMethod`'s `EQUIPPABLE` default, also the fallback when a wither skull or carved pumpkin's spawn check fails) | equips the first eligible `LivingEntity` standing on the cell ahead | **no** — needs an entity spatial query this crate has nowhere, *and* `crate::mobs::SimMob` carries no equipment-slot state to write into even if one were found; equipping a bystanding *player* instead would additionally need a player-position registry, which is not reachable from `tick.rs`'s scheduled-tick drain |
//! | *(implicit default)* spawn egg (`itemStack.has(DataComponents.ENTITY_DATA)`, true for every real spawn egg) | spawns the named mob just outside the dispenser's own face | **yes** ([`spawn_egg_position`], reusing `crate::spawn_egg::entity_type_for_egg`/`y_offset`) |
//! | *everything else* | `DefaultDispenseItemBehavior` — plain toss | **yes** ([`plain_toss`]) |
//!
//! So this module now models the shared mechanics (the `TRIGGERED` redstone
//! state machine, the plain-toss math, and the dropper's container push) plus
//! five of the ~35 special behaviours (spawn eggs, boats, bone meal, the
//! fire-placement half of flint and steel, and — via `crate::hopper` — a
//! dropper's container push); every skip above names its own missing
//! mechanism rather than leaving "no" unexplained, per this issue's own trap
//! about not treating "dispenser ejects an item" as done until the table is
//! at least enumerated.
//!
//! # What this needs of the execution model
//!
//! * **Trigger**: `neighborChanged`, immediate — `hasNeighborSignal(pos) ||
//!   hasNeighborSignal(pos.above())` (the `pos.above()` half is easy to miss:
//!   a comparator or repeater sitting directly on **top** of a dispenser can
//!   fire it, not only one beside it). Wired into `react_to_notification`.
//! * **Scheduled tick**: yes, a fixed 4-tick one-shot on the *rising* edge
//!   only (`TRIGGER_DURATION`) — [`on_neighbor_changed`]'s
//!   `schedule_fire` flag. Unlike a diode's delay this one never reschedules
//!   itself and never changes with any block-state property.
//! * **The dispense is wired**: `tick.rs`'s scheduled-tick drain has a
//!   `TICK_DISPENSER_FIRE` arm that reads the live container through the
//!   `BlockEntityHandle` it already carries, picks a slot with
//!   [`random_slot`], and dispatches on the item and on [`is_dropper`] — a
//!   dropper always either pushes into a container ahead
//!   ([`is_pushable_container`], `crate::hopper::try_move_item_into`) or
//!   plain-tosses, **never** consulting the behaviour table below, matching
//!   `DropperBlock.getDispenseMethod`'s own hardcoded `DefaultDispenseItemBehavior`.
//!   A dispenser instead matches the item against [`spawn_egg_position`],
//!   [`boat_dispense`], `crate::bone_meal::apply_bone_meal` and
//!   [`flint_and_steel_ignite`] in turn, falling to [`plain_toss`] through
//!   `MobSim::spawn_item` — the same entry point `crate::block_drops` uses for
//!   a broken block's loot — when none matches or a matched behaviour reports
//!   no effect. An empty container (`random_slot` returns `None`) is a silent
//!   no-op: vanilla plays a click sound instead, which this crate does not
//!   model sound effects for yet.

use lodestone_data::{block_states, collision_shapes};
use lodestone_model::{BlockPos, Vec3};

use crate::chunk::ChunkSource;
use crate::neighbor_update::Direction;
use crate::redstone::{base_name, direction_from_str, direction_to_str, get_bool_property, get_str_property, with_property};

pub const DISPENSER: &str = "minecraft:dispenser";
pub const DROPPER: &str = "minecraft:dropper";

/// `DispenserBlock.TRIGGER_DURATION` (`:56`).
pub const TRIGGER_DURATION: u32 = 4;

/// `redstone:dispenser_fire` — the scheduled-tick kind `tick.rs`'s drain
/// dispatches on (see this module's own doc comment).
pub const TICK_DISPENSER_FIRE: &str = "redstone:dispenser_fire";

/// The seed for `tick.rs`'s per-world dispenser RNG — [`random_slot`]'s pick
/// and [`plain_toss`]'s toss draw from the one stream, matching vanilla's
/// single per-level `RandomSource`. Explicit rather than drawn, matching
/// every other `_BEHAVIOR_SEED` in this crate (`crate::fire::FIRE_BEHAVIOR_SEED`,
/// `crate::explosion_blocks::EXPLOSION_BEHAVIOR_SEED`).
pub const DISPENSER_BEHAVIOR_SEED: u64 = 0xD15E_5EED;

/// `DispenserBlock.getDispensePosition`'s own default `scale` (`:161-163`,
/// the zero-argument overload).
pub const DISPENSE_SCALE: f64 = 0.7;

#[must_use]
pub fn is_dispenser_family(state: &str) -> bool {
    matches!(base_name(state), DISPENSER | DROPPER)
}

/// `true` only for `minecraft:dropper` — the one predicate `tick.rs`'s
/// `TICK_DISPENSER_FIRE` arm needs to pick the container-push-then-toss path
/// ([`is_pushable_container`], [`crate::hopper::try_move_item_into`])
/// unconditionally rather than consulting the dispenser behaviour table.
#[must_use]
pub fn is_dropper(state: &str) -> bool {
    base_name(state) == DROPPER
}

/// `DropperBlock.dispenseFrom`'s container check — whether `menu_name` (from
/// [`crate::block_entities::BlockEntity::menu_name`], read at the cell
/// directly ahead of the dropper) names a container this crate can push a
/// single item into blind, with no face or slot-kind rule.
///
/// That is the same simplification `crate::hopper::try_move_one_item`'s own
/// doc comment already accepts for hopper adjacency — this crate has no real
/// container-kind registry to restrict against yet. A furnace is deliberately
/// **excluded** even though it has a real menu: vanilla only ever reaches its
/// three slots through `WorldlyContainer.getSlotsForFace` (fuel through the
/// side, nothing through the top or the output), and a blind push would land
/// an item in whichever of `[input, fuel, output]` happens to be first —
/// silently wrong rather than merely unmodelled, which is worse than refusing
/// and falling through to a plain toss. `"minecraft:generic_3x3"` covers a
/// dispenser or dropper ahead, matching vanilla's own `getContainerAt`, which
/// does not exclude the dispenser/dropper family either.
#[must_use]
pub fn is_pushable_container(menu_name: &str) -> bool {
    matches!(
        menu_name,
        "minecraft:hopper" | "minecraft:generic_3x3" | "minecraft:generic_9x3"
    )
}

#[must_use]
pub fn facing(state: &str) -> Direction {
    get_str_property(state, "facing").map(direction_from_str).unwrap_or(Direction::North)
}

#[must_use]
pub fn triggered(state: &str) -> bool {
    get_bool_property(state, "triggered").unwrap_or(false)
}

/// The result of a neighbour notification reaching a dispenser or dropper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NeighborReaction {
    pub new_state: String,
    /// `true` only on the rising edge — vanilla schedules the 4-tick fire
    /// tick exactly once per `false -> true` transition, never on the way
    /// back down and never while already `true`.
    pub schedule_fire: bool,
}

/// `DispenserBlock.neighborChanged` (`DispenserBlock.java:127-139`).
/// `should_trigger` is vanilla's `hasNeighborSignal(pos) ||
/// hasNeighborSignal(pos.above())` — the caller computes both `best_neighbor_signal`
/// reads (see this module's own doc comment on why the `above` half matters).
/// `None` when `state` is not this family, or when `should_trigger` already
/// matches the stored `TRIGGERED` (nothing to write).
#[must_use]
pub fn on_neighbor_changed(state: &str, should_trigger: bool) -> Option<NeighborReaction> {
    if !is_dispenser_family(state) {
        return None;
    }
    let is_triggered = triggered(state);
    if should_trigger && !is_triggered {
        Some(NeighborReaction {
            new_state: with_property(state, "triggered", "true"),
            schedule_fire: true,
        })
    } else if !should_trigger && is_triggered {
        Some(NeighborReaction {
            new_state: with_property(state, "triggered", "false"),
            schedule_fire: false,
        })
    } else {
        None
    }
}

/// `DispenserBlockEntity.getRandomSlot` (`DispenserBlockEntity.java:34-46`) —
/// reservoir sampling over whichever slots `occupied` marks non-empty,
/// `None` for an entirely empty container (vanilla's own `-1`, which callers
/// read as "play the empty click sound instead").
///
/// `next_int` mirrors `RandomSource.nextInt(bound)`: given `bound`, a value in
/// `0..bound`. The reservoir property this exists to test is that **every**
/// occupied slot has an equal `1/n` chance of being the final answer, which
/// is exactly what incrementing `replace_odds` once per occupied slot (not
/// once per slot overall) achieves — the discriminating case is an empty slot
/// sitting *between* two occupied ones, which must not consume a draw.
#[must_use]
pub fn random_slot(occupied: &[bool], mut next_int: impl FnMut(u32) -> u32) -> Option<usize> {
    let mut replace_slot = None;
    let mut replace_odds: u32 = 1;
    for (i, is_occupied) in occupied.iter().enumerate() {
        if *is_occupied {
            if next_int(replace_odds) == 0 {
                replace_slot = Some(i);
            }
            replace_odds += 1;
        }
    }
    replace_slot
}

/// `DispenserBlock.getDispensePosition` (`DispenserBlock.java:161-169`), the
/// zero-offset overload — the world-space point [`DISPENSE_SCALE`] of a block
/// out from `center` (the dispenser's own centre, `pos + 0.5` on every axis)
/// in the direction it faces.
#[must_use]
pub fn dispense_position(center: (f64, f64, f64), face: Direction) -> (f64, f64, f64) {
    let (dx, dy, dz) = step(face);
    (
        center.0 + DISPENSE_SCALE * dx,
        center.1 + DISPENSE_SCALE * dy,
        center.2 + DISPENSE_SCALE * dz,
    )
}

fn step(d: Direction) -> (f64, f64, f64) {
    match d {
        Direction::Down => (0.0, -1.0, 0.0),
        Direction::Up => (0.0, 1.0, 0.0),
        Direction::North => (0.0, 0.0, -1.0),
        Direction::South => (0.0, 0.0, 1.0),
        Direction::West => (-1.0, 0.0, 0.0),
        Direction::East => (1.0, 0.0, 0.0),
    }
}

/// `DefaultDispenseItemBehavior.execute`'s velocity/position math is owned by
/// `crate::block_drops`'s item-entity constants where this crate already
/// models one (see that module's own doc comment); this function is only the
/// facing lookup [`dispense_position`] needs, kept here so `direction_to_str`
/// round-trips through the same helper every other family in this crate uses.
#[allow(dead_code)]
#[must_use]
pub fn facing_name(state: &str) -> &'static str {
    direction_to_str(facing(state))
}

/// `RandomSource.triangle(mean, spread)` (`RandomSource.java:59-61`):
/// `mean + spread * (next() - next())`. Two draws, always in this order —
/// [`plain_toss`]'s own doc comment names why draw order matters here as much
/// as everywhere else in this crate's RNG-threaded code.
fn triangle(mean: f64, spread: f64, next_f64: &mut impl FnMut() -> f64) -> f64 {
    mean + spread * (next_f64() - next_f64())
}

/// `DefaultDispenseItemBehavior.DEFAULT_ACCURACY` (`:12`) — the deviation
/// [`plain_toss`]'s three [`triangle`] draws share, before the
/// `0.0172275` scale `spawnItem` multiplies it by (`:44-46`).
const DEFAULT_ACCURACY: f64 = 6.0;

/// The world-space feet position and velocity of a plain-tossed item —
/// `DefaultDispenseItemBehavior.execute` → `spawnItem`
/// (`DefaultDispenseItemBehavior.java:22-49`). Every dropper dispense, and
/// every dispenser item this module has no special behaviour for (the
/// `*everything else*` row of this module's own table), uses this.
///
/// `next_f64` is threaded rather than captured, matching
/// `crate::block_drops::pop_resource_placement`'s own convention — a test can
/// pin an exact draw sequence this way. **Not** byte-parity with vanilla's
/// Xoroshiro stream: vanilla's `ItemEntity` four-argument constructor draws
/// two numbers for a default velocity that `spawnItem` immediately
/// overwrites one line later, so this function skips those two wasted draws
/// — the same class of divergence `crate::block_drops`'s own module doc
/// records for its own RNG stream.
#[must_use]
pub fn plain_toss(
    center: (f64, f64, f64),
    face: Direction,
    next_f64: &mut impl FnMut() -> f64,
) -> ((f64, f64, f64), (f64, f64, f64)) {
    let (px, py, pz) = dispense_position(center, face);
    // `spawnItem`'s own axis split (`:34-38`): a straight up/down eject sits
    // closer to the dispenser's own centre than a sideways one does.
    let y_shift = if matches!(face, Direction::Up | Direction::Down) {
        0.125
    } else {
        0.156_25
    };
    let position = (px, py - y_shift, pz);

    let (step_x, _step_y, step_z) = step(face);
    // Draw 1: the forward push's magnitude, uniform in `[0.2, 0.3)`.
    let pow = next_f64() * 0.1 + 0.2;
    let deviation = 0.0172275 * DEFAULT_ACCURACY;
    // Draws 2-7: x then y then z, matching the argument-evaluation order
    // inside vanilla's one `setDeltaMovement(...)` call.
    let velocity = (
        triangle(step_x * pow, deviation, next_f64),
        triangle(0.2, deviation, next_f64),
        triangle(step_z * pow, deviation, next_f64),
    );
    (position, velocity)
}

/// The collision boxes' highest top, block-local — the one piece
/// [`spawn_egg_position`] needs and [`crate::spawn_egg`]/[`crate::boat`] each
/// already duplicate locally rather than share (both modules' own doc
/// comments give the same reason: resolution is `block_state_id` then
/// `block_states::state_id`, **never** `_or_default`, because a bare name's
/// default answer is its *lowest* state id, not its default state).
fn collision_top(state: &str) -> Option<f64> {
    let id = crate::mobs::block_state_id(state).or_else(|| block_states::state_id(state));
    let boxes = id.and_then(collision_shapes::collision_boxes).unwrap_or(&[]);
    boxes
        .iter()
        .map(|b| f64::from(b.max[1]))
        .fold(None, |acc: Option<f64>, v| Some(acc.map_or(v, |a| a.max(v))))
}

/// `SpawnEggItemBehavior.execute`'s placement half —
/// `EntityType.create`/`getYOffset`, specialised to a dispenser: `tryMoveDown`
/// is `direction != Direction.UP` and `movedUp` is always `false` (only a
/// clicked *top face* can set it, and a dispenser has no clicked face at all).
/// [`crate::spawn_egg::y_offset`] is vanilla's own `getYOffset` re-expression
/// with `moved_up` already threaded through; this only supplies the
/// dispenser-specific inputs — the cell directly ahead, not a clicked one —
/// and the `direction == UP` short-circuit `y_offset` cannot see on its own
/// (`tryMoveDown = false` skips the sweep entirely rather than sweeping with a
/// zero budget).
///
/// The caller resolves the entity type with
/// [`crate::spawn_egg::entity_type_for_egg`] first; this only answers *where*.
#[must_use]
pub fn spawn_egg_position(origin: BlockPos, face: Direction, block_state: &dyn Fn(BlockPos) -> String) -> Vec3 {
    let target = face.relative(origin);
    let y_off = if matches!(face, Direction::Up) {
        0.0
    } else {
        crate::spawn_egg::y_offset(collision_top(&block_state(target)), false)
    };
    Vec3::new(f64::from(target.x) + 0.5, f64::from(target.y) + y_off, f64::from(target.z) + 0.5)
}

/// `Direction.toYRot()` — `(data2d & 3) * 90`. Vanilla's `data2d` is `-1` for
/// the two vertical directions and `0..=3` for south/west/north/east in that
/// order (`Direction.java`'s own per-variant field table); [`boat_dispense`]
/// calls this unconditionally, even for a dispenser facing up or down, so the
/// vertical case is included rather than treated as unreachable.
fn to_y_rot(face: Direction) -> f32 {
    let data2d: i32 = match face {
        Direction::South => 0,
        Direction::West => 1,
        Direction::North => 2,
        Direction::East => 3,
        Direction::Up | Direction::Down => -1,
    };
    ((data2d & 3) * 90) as f32
}

/// `true` for a block-state string carrying **water** (not lava) —
/// [`boat_dispense`]'s own `FluidTags.WATER` check, via
/// [`crate::fluid::fluid_state_of`].
fn is_water(state: &str) -> bool {
    crate::fluid::fluid_state_of(state).is_some_and(|f| f.kind == crate::fluid::FluidKind::Water)
}

/// [`boat_dispense`]'s outcome.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum BoatDispense {
    /// `boat.setInitialPos(...)` plus `setYRot` — the caller spawns through
    /// `crate::mobs::MobSim::spawn_vehicle`, the same entry point
    /// [`crate::boat::apply_boat_item`] uses for a player-placed one.
    Place { position: Vec3, yaw: f32 },
    /// Neither the water-ahead nor the air-over-water case matched — vanilla's
    /// own fallback is `defaultDispenseItemBehavior.dispense`, i.e.
    /// [`plain_toss`].
    Fallback,
}

/// `BoatDispenseItemBehavior.execute` — placement is **not**
/// [`crate::boat`]'s player-raytrace rule at all (that module's own doc
/// comment is explicit that a boat is placed at the exact hit *point* of the
/// *player's* view ray). A dispensed boat instead lands just outside the
/// dispenser's own face, riding the water surface directly ahead, or resting
/// on the ground one cell down when the cell ahead is air directly over
/// water. `boat_width` is [`crate::boat::BOAT_WIDTH`] — every boat, chest boat
/// and raft shares one width, so the caller does not need to know which of
/// the twenty it is dispensing to compute this.
#[must_use]
pub fn boat_dispense(origin: BlockPos, face: Direction, boat_width: f64, block_state: &dyn Fn(BlockPos) -> String) -> BoatDispense {
    let (sx, sy, sz) = step(face);
    // `0.5625` here is `AbstractBoat`'s own bounding-box half-thickness
    // constant, numerically identical to `BOAT_HEIGHT` but a *different*
    // vanilla field (`justOutsideDispenser`'s addend, not the box height) —
    // stated because the coincidence is easy to mistake for a typo.
    let just_outside = 0.5625 + boat_width / 2.0;
    let spawn_x = f64::from(origin.x) + 0.5 + sx * just_outside;
    let spawn_y = f64::from(origin.y) + 0.5 + sy * 1.125;
    let spawn_z = f64::from(origin.z) + 0.5 + sz * just_outside;

    let front = face.relative(origin);
    let front_state = block_state(front);
    let y_offset = if is_water(&front_state) {
        1.0
    } else if crate::random_tick::is_air_variant(&front_state) {
        let below = BlockPos::new(front.x, front.y - 1, front.z);
        if is_water(&block_state(below)) {
            0.0
        } else {
            return BoatDispense::Fallback;
        }
    } else {
        return BoatDispense::Fallback;
    };
    BoatDispense::Place {
        position: Vec3::new(spawn_x, spawn_y + y_offset, spawn_z),
        yaw: to_y_rot(face),
    }
}

/// `FlintAndSteelDispenseItemBehavior.execute`'s block-ignition arm only.
///
/// Two of its three arms are deliberately absent, each for a reason this
/// crate cannot currently close: `tryIgniteExplosiveEntities` needs an entity
/// query this crate has nowhere (so this behaves exactly as if no such entity
/// were ever standing ahead — correct whenever one is not, which is every
/// case this crate could tell apart anyway), and priming a `TntBlock` ahead
/// needs a primed-TNT entity this crate has none of at all (`crate::fire`'s
/// own module doc names the identical gap for `TntBlock::prime`). Campfire and
/// candle re-lighting are absent too — smaller, same shape: this crate has no
/// modelled `lit` re-toggle path for either family reachable from an item
/// use. `None` for all four; `Some((target, new_state))` only for the
/// fire-placement arm this crate can actually do, mirroring
/// `BaseFireBlock::canBePlacedAt` (target is air, and the *fire's own*
/// `canSurvive` — a sturdy floor or a burnable neighbour — holds); the portal
/// clause inside `canBePlacedAt` is not modelled either, since this crate has
/// no portal-frame detection.
#[must_use]
pub fn flint_and_steel_ignite<S: ChunkSource + ?Sized>(
    world: &S,
    env: crate::fire::FireEnv,
    origin: BlockPos,
    face: Direction,
) -> Option<(BlockPos, String)> {
    let target = face.relative(origin);
    if !crate::random_tick::is_air_variant(&crate::fire::block_at(world, env, target)) {
        return None;
    }
    if !crate::fire::can_survive(world, env, target) {
        return None;
    }
    Some((target, crate::fire::state_at(world, env, target)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dispenser(facing: &str, triggered: bool) -> String {
        format!("minecraft:dispenser[facing={facing},triggered={triggered}]")
    }

    #[test]
    fn rising_edge_triggers_and_schedules_the_fire_tick() {
        let out = on_neighbor_changed(&dispenser("north", false), true).expect("rising edge");
        assert_eq!(out.new_state, dispenser("north", true));
        assert!(out.schedule_fire);
    }

    #[test]
    fn falling_edge_untriggers_without_scheduling_anything() {
        let out = on_neighbor_changed(&dispenser("north", true), false).expect("falling edge");
        assert_eq!(out.new_state, dispenser("north", false));
        assert!(!out.schedule_fire, "vanilla never schedules on the way down");
    }

    #[test]
    fn steady_state_is_a_no_op_in_both_directions() {
        assert_eq!(on_neighbor_changed(&dispenser("north", false), false), None);
        assert_eq!(on_neighbor_changed(&dispenser("north", true), true), None);
    }

    /// The reservoir-sampling property: an empty slot between two occupied
    /// ones must not consume a draw. Checked directly against the sequence of
    /// `bound` values `random_slot` actually passes to `next_int` — `1, 2, 3`
    /// for the three occupied slots, never incrementing (and never being
    /// called at all) for the four empty ones. A version that consumed a draw
    /// per slot overall would instead pass `1, 3, 5` (the 1-based overall
    /// index) or call `next_int` seven times instead of three.
    #[test]
    fn random_slot_skips_empty_slots_without_consuming_their_draw() {
        let occupied = [false, true, false, false, true, false, true];
        let mut bounds_seen = Vec::new();
        let _ = random_slot(&occupied, |bound| {
            bounds_seen.push(bound);
            1 // always "miss": never replace, so the very first hit (forced below) is the only way to pin a winner.
        });
        assert_eq!(bounds_seen, vec![1, 2, 3], "exactly one draw per occupied slot, with odds incrementing only across occupied slots");
    }

    /// The companion to the count check above: forcing every draw to "hit"
    /// (`next_int` always `0`) must leave the **last** occupied slot standing
    /// — proves the reservoir keeps overwriting its answer rather than
    /// latching the first hit, and that the winner is drawn from the
    /// occupied set (index 6), not a plain slot count (which would be 7).
    #[test]
    fn random_slot_keeps_replacing_and_lands_on_the_last_occupied_slot_when_every_draw_hits() {
        let occupied = [false, true, false, false, true, false, true];
        let picked = random_slot(&occupied, |_bound| 0);
        assert_eq!(picked, Some(6));
    }

    #[test]
    fn random_slot_reports_none_for_an_entirely_empty_container() {
        assert_eq!(random_slot(&[false, false, false], |_| 0), None);
    }

    /// A single occupied slot is chosen unconditionally, on the very first
    /// draw (`replace_odds` starts at 1, so `next_int(1)` is always `0`) —
    /// the discriminating case that a reservoir sample of size 1 needs no
    /// randomness to resolve.
    #[test]
    fn a_single_occupied_slot_is_always_chosen() {
        let mut calls = 0;
        let picked = random_slot(&[false, false, true, false], |bound| {
            calls += 1;
            assert_eq!(bound, 1);
            0
        });
        assert_eq!(picked, Some(2));
        assert_eq!(calls, 1, "exactly one draw for exactly one occupied slot");
    }

    /// `dispense_position` for each of the six facings, pinned against the
    /// jar's own `0.7` scale rather than a rounded `0.5`/`1.0` guess.
    #[test]
    fn dispense_position_offsets_by_the_jars_own_scale_in_every_direction() {
        let centre = (8.5, 65.5, 8.5);
        assert_eq!(dispense_position(centre, Direction::East), (9.2, 65.5, 8.5));
        assert_eq!(dispense_position(centre, Direction::West), (7.8, 65.5, 8.5));
        assert_eq!(dispense_position(centre, Direction::Up), (8.5, 66.2, 8.5));
        assert_eq!(dispense_position(centre, Direction::Down), (8.5, 64.8, 8.5));
        assert_eq!(dispense_position(centre, Direction::South), (8.5, 65.5, 9.2));
        assert_eq!(dispense_position(centre, Direction::North), (8.5, 65.5, 7.8));
    }

    #[test]
    fn is_dropper_distinguishes_the_two_registrations() {
        assert!(is_dropper("minecraft:dropper[facing=up,triggered=false]"));
        assert!(!is_dropper("minecraft:dispenser[facing=up,triggered=false]"));
    }

    /// A small helper so a test can hand `plain_toss` a fixed draw sequence —
    /// the same "predict the exact sequence" approach
    /// `crate::block_drops::pop_resource_placement`'s own tests use.
    fn fixed_draws(values: &'static [f64]) -> impl FnMut() -> f64 {
        let mut it = values.iter().copied();
        move || it.next().expect("test supplied enough draws")
    }

    /// **`plain_toss`'s sideways case, predicted from the jar's own
    /// constants** (`DEFAULT_ACCURACY = 6`, `0.0172275` deviation scale,
    /// `spawnItem`'s `0.15625` off-axis y-shift) rather than a re-derivation
    /// through the function itself. East is not the Y axis, so the y-shift
    /// takes the `0.15625` branch and `step_z` is `0.0`, which is the
    /// discriminating half against the vertical case below.
    #[test]
    fn plain_toss_sideways_matches_the_jars_own_formula() {
        let mut next = fixed_draws(&[0.5, 0.25, 0.75, 0.125, 0.875, 0.375, 0.625]);
        let (position, velocity) =
            plain_toss((8.5, 65.5, 8.5), Direction::East, &mut next);
        assert_eq!(position, (9.2, 65.34375, 8.5), "0.15625 off-axis y-shift");

        let expected = (0.198_317_5, 0.122_476_25, -0.025_841_25);
        assert!(
            (velocity.0 - expected.0).abs() < 1e-9
                && (velocity.1 - expected.1).abs() < 1e-9
                && (velocity.2 - expected.2).abs() < 1e-9,
            "velocity {velocity:?} does not match the predicted {expected:?}"
        );
    }

    /// **The vertical counterpart**, a different centre and a different draw
    /// sequence so the pair cannot pass by coincidence: `Down`'s y-shift is
    /// `0.125` (not `0.15625`) and both `step_x`/`step_z` are `0.0`, so the x
    /// and z velocities carry no forward push at all — only the triangular
    /// spread around a mean of zero.
    #[test]
    fn plain_toss_vertical_matches_the_jars_own_formula() {
        let mut next = fixed_draws(&[0.375, 0.625, 0.875, 0.125, 0.25, 0.75, 0.5]);
        let (position, velocity) =
            plain_toss((2.5, 70.5, -3.5), Direction::Down, &mut next);
        assert_eq!(position, (2.5, 69.675, -3.5), "0.125 on-axis y-shift");

        let expected = (-0.025_841_25, 0.187_079_375, 0.025_841_25);
        assert!(
            (velocity.0 - expected.0).abs() < 1e-9
                && (velocity.1 - expected.1).abs() < 1e-9
                && (velocity.2 - expected.2).abs() < 1e-9,
            "velocity {velocity:?} does not match the predicted {expected:?}"
        );
    }

    #[test]
    fn is_pushable_container_accepts_hopper_and_both_generic_menus_and_refuses_a_furnace() {
        assert!(is_pushable_container("minecraft:hopper"));
        assert!(is_pushable_container("minecraft:generic_3x3"));
        assert!(is_pushable_container("minecraft:generic_9x3"));
        // The discriminating refusal: a furnace has a real menu but face
        // restrictions this crate does not model, so a blind push must not
        // treat it as pushable.
        assert!(!is_pushable_container("minecraft:furnace"));
        assert!(!is_pushable_container("minecraft:smoker"));
        assert!(!is_pushable_container("minecraft:blast_furnace"));
    }

    /// **`spawn_egg_position`, a side facing.** East from `(0, 64, 0)` targets
    /// `(1, 64, 0)`; air there gives `y_off = 0.0` (the reservoir/collide
    /// formula's floor), landing the mob's feet exactly on the target cell's
    /// own floor.
    #[test]
    fn spawn_egg_position_over_air_lands_at_the_target_cells_own_floor() {
        let pos = spawn_egg_position(BlockPos::new(0, 64, 0), Direction::East, &|_| "minecraft:air".to_owned());
        assert_eq!(pos, Vec3::new(1.5, 64.0, 0.5));
    }

    /// **The discriminating case**: a *solid* block occupying the target cell
    /// itself (not a clicked-face neighbour — a dispenser has no clicked face)
    /// pushes the mob up onto its top, `y_off = 1.0`. A hardcoded `0.0` cannot
    /// tell these two apart.
    #[test]
    fn spawn_egg_position_over_solid_stone_stands_on_top_of_it() {
        let pos = spawn_egg_position(BlockPos::new(0, 64, 0), Direction::East, &|_| "minecraft:stone".to_owned());
        assert_eq!(pos, Vec3::new(1.5, 65.0, 0.5));
    }

    /// **Facing `Up` never sweeps at all** (`tryMoveDown = direction !=
    /// Direction.UP`), so a solid block occupying the target cell still gives
    /// `y_off = 0.0` — the opposite answer a same-cell-collision rule would
    /// give for every other facing, which is exactly why this needs its own
    /// gate rather than trusting the East case to generalise.
    #[test]
    fn spawn_egg_position_facing_up_never_computes_a_collision_offset() {
        let pos = spawn_egg_position(BlockPos::new(0, 64, 0), Direction::Up, &|_| "minecraft:stone".to_owned());
        assert_eq!(pos, Vec3::new(0.5, 65.0, 0.5));
    }

    /// A world of water at `y == 63` (source, air above) for `x >= 1`, stone
    /// floor elsewhere, air above `y == 63` everywhere — enough surface
    /// variety to hit every [`boat_dispense`] branch.
    fn boat_world(overrides: Vec<(BlockPos, String)>) -> impl Fn(BlockPos) -> String {
        move |p: BlockPos| {
            if let Some((_, name)) = overrides.iter().find(|(at, _)| *at == p) {
                return name.clone();
            }
            if p.y == 63 && p.x >= 1 {
                return "minecraft:water[level=0]".to_owned();
            }
            if p.y <= 62 {
                return "minecraft:stone".to_owned();
            }
            "minecraft:air".to_owned()
        }
    }

    /// **Water directly ahead**: `y_offset = 1.0`, riding the source's own
    /// surface centre — pairwise-distinct origin coordinates so a transposed
    /// axis cannot pass by coincidence.
    #[test]
    fn boat_dispense_over_water_rides_the_surface() {
        let origin = BlockPos::new(0, 63, 4);
        let out = boat_dispense(origin, Direction::East, 1.375, &boat_world(vec![]));
        let BoatDispense::Place { position, yaw } = out else {
            panic!("water directly ahead must place: {out:?}");
        };
        // justOutsideDispenser = 0.5625 + 1.375/2 = 1.25; spawnX = 0.5 + 1.25 = 1.75.
        assert!((position.x - 1.75).abs() < 1e-9, "{position:?}");
        // East's step_y is 0, so the `1.125` vertical term never applies —
        // spawnY = 63.5 + 0.0 + yOffset(1.0) = 64.5.
        assert!((position.y - 64.5).abs() < 1e-9, "{position:?}");
        assert!((position.z - 4.5).abs() < 1e-9, "{position:?}");
        assert!((yaw - 270.0).abs() < f32::EPSILON, "East's toYRot is 270: {yaw}");
    }

    /// **Air over water**: `y_offset = 0.0`, the boat settles a cell lower
    /// than the water-ahead case above — the discriminating pair this
    /// function's whole reason for existing rests on.
    #[test]
    fn boat_dispense_air_over_water_settles_one_cell_lower() {
        // The same origin as `boat_dispense_over_water_rides_the_surface`
        // above, so the two results are directly comparable: there, the front
        // cell itself is water and the boat lands at `y = 64.5`; here the
        // front cell is overridden to air with water only in the cell below
        // it, and the boat must land exactly one block lower.
        let origin = BlockPos::new(0, 63, 4);
        let world = boat_world(vec![
            (BlockPos::new(1, 63, 4), "minecraft:air".to_owned()),
            (BlockPos::new(1, 62, 4), "minecraft:water[level=0]".to_owned()),
        ]);
        let out = boat_dispense(origin, Direction::East, 1.375, &world);
        let BoatDispense::Place { position, .. } = out else {
            panic!("air over water must place: {out:?}");
        };
        // spawnY = 63.5 + 0.0 + yOffset(0.0) = 63.5 — one block below the
        // water-directly-ahead case's 64.5.
        assert!((position.y - 63.5).abs() < 1e-9, "{position:?}");
    }

    /// **Neither condition holds** (solid stone ahead): falls to
    /// [`BoatDispense::Fallback`], the caller's cue to plain-toss instead.
    #[test]
    fn boat_dispense_into_solid_stone_falls_back() {
        let origin = BlockPos::new(0, 63, -4);
        let front = Direction::East.relative(origin);
        let world = boat_world(vec![(front, "minecraft:stone".to_owned())]);
        let out = boat_dispense(origin, Direction::East, 1.375, &world);
        assert_eq!(out, BoatDispense::Fallback);
    }

    /// A `ChunkSource` test double for [`flint_and_steel_ignite`], the same
    /// minimal shape `crate::fire`'s own test module uses: a solid floor at
    /// `y <= floor_y`, air above, with per-cell overrides.
    struct FireRig {
        floor_y: i32,
        overrides: Vec<(BlockPos, &'static str)>,
    }

    impl crate::chunk::ChunkSource for FireRig {
        fn column(&self, cx: i32, cz: i32) -> crate::chunk::ChunkColumn {
            let mut col = crate::chunk::ChunkColumn::new(-64, 384);
            for x in 0..16 {
                for z in 0..16 {
                    for y in -64..=self.floor_y {
                        col.set_block(x, y, z, "minecraft:stone");
                    }
                }
            }
            let _ = (cx, cz);
            col
        }

        fn block_state(&self, x: i32, y: i32, z: i32) -> String {
            let pos = BlockPos::new(x, y, z);
            if let Some((_, name)) = self.overrides.iter().find(|(at, _)| *at == pos) {
                return (*name).to_owned();
            }
            if y <= self.floor_y {
                "minecraft:stone".to_owned()
            } else {
                "minecraft:air".to_owned()
            }
        }

        fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
    }

    fn fire_env() -> crate::fire::FireEnv {
        crate::fire::FireEnv::overworld_in(-64, 384, lodestone_model::Difficulty::Normal, false)
    }

    /// **A sturdy floor ahead**: fire can survive there, so this places
    /// ordinary fire.
    #[test]
    fn flint_and_steel_ignite_over_a_sturdy_floor_places_fire() {
        // `floor_y = 63` fills every cell up to and including `y = 63`, so the
        // origin must sit one cell *above* the floor for its own target cell
        // to be air at all.
        let rig = FireRig { floor_y: 63, overrides: vec![] };
        let out = flint_and_steel_ignite(&rig, fire_env(), BlockPos::new(0, 64, 0), Direction::East);
        let (target, state) = out.expect("air over a sturdy floor must ignite");
        assert_eq!(target, BlockPos::new(1, 64, 0));
        assert!(state.starts_with("minecraft:fire["), "{state}");
    }

    /// **Control: no floor and no burnable neighbour** — `canSurvive` fails,
    /// so nothing ignites. Without this, the test above could pass merely
    /// because the target cell is air, regardless of support.
    #[test]
    fn flint_and_steel_ignite_with_nothing_to_stand_on_refuses() {
        let rig = FireRig { floor_y: -65, overrides: vec![] };
        assert_eq!(
            flint_and_steel_ignite(&rig, fire_env(), BlockPos::new(0, 63, 0), Direction::East),
            None
        );
    }

    /// **Control: the target cell is not air at all** — a facing into solid
    /// stone must refuse regardless of what is beneath it.
    #[test]
    fn flint_and_steel_ignite_into_a_solid_block_refuses() {
        let rig = FireRig {
            floor_y: 63,
            overrides: vec![(BlockPos::new(1, 64, 0), "minecraft:stone")],
        };
        assert_eq!(
            flint_and_steel_ignite(&rig, fire_env(), BlockPos::new(0, 64, 0), Direction::East),
            None,
            "the target cell (1, 64, 0) is overridden solid, not air"
        );
    }
}
