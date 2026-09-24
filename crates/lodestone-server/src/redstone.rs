//! Redstone signal computation — the "how much signal touches this position,
//! and from which side" query layer every redstone component reads from before
//! deciding what to do.
//! This module has **no notify/cascade logic of its own** — that lives in
//! `crate::random_tick`'s reaction dispatch, which calls
//! `crate::neighbor_update::NeighborPropagator` exactly the way
//! `crate::gravity_tick` already does. This module is pure queries plus a
//! handful of per-family state helpers.
//!
//! # Where this comes from in the jar
//!
//! Vanilla's own `SignalGetter` interface is
//! its own query layer, and every function below is a direct,
//! citation-per-function port of it:
//!
//! - [`weak_signal`] ~ vanilla's own per-block weak-signal override (each block's own override —
//!   see the per-family jar citations inline below).
//! - [`direct_signal`] ~ vanilla's own per-block direct-signal override.
//! - [`direct_signal_to`] ~ vanilla's own signal-getter direct-signal-to routine.
//! - [`signal_at`] ~ vanilla's own signal-getter get-signal routine: a redstone *conductor*
//!   additionally carries whatever direct/strong signal touches any of its six
//!   faces (`getDirectSignalTo`), which is how a lever on the side of a stone
//!   block powers a wire sitting on top of that same block. That circuit is
//!   gated by `a_lever_on_the_side_of_a_conductor_powers_a_wire_on_top_of_it`,
//!   and it is the case that separates the weak path from the strong one.
//! - [`best_neighbor_signal`] ~ vanilla's own signal-getter best-neighbor-signal routine.
//! - [`control_input_signal`] ~ vanilla's own signal-getter control-input-signal routine —
//!   repeaters'/comparators' own side-input read.
//!
//! Vanilla's own signal-getter direction list (`Direction.values()`) is iterated by
//! [`best_neighbor_signal`]/[`direct_signal_to`], but **only ever through a
//! commutative `max`** — unlike [`crate::neighbor_update::UPDATE_ORDER`],
//! where the fan-out order is itself the observable behaviour, no query
//! function in this module has an order-sensitive result. This is *not* an
//! oversight of CLAUDE.md's "ordering is the behaviour" rule: it is exactly
//! why that rule does not apply to a pure `max` reduction, and
//! [`crate::neighbor_update::ALL_DIRECTIONS`]'s own doc comment says so
//! explicitly.
//!
//! # The reduced conductor/source model
//!
//! This crate has no collision-shape system (`crate::gravity_tick`'s own doc
//! comment already names this gap for `isFree`/`canBeReplaced`), so
//! [`is_redstone_conductor`] cannot check vanilla's own is-redstone-conductor check's
//! real definition (a full-cube collision shape). The honest reduction used
//! here: **anything that is not air/fluid and not one of the modelled
//! non-full-cube redstone blocks is a conductor** — every ordinary solid block
//! (stone, dirt, log, ...) is a conductor, exactly as in vanilla, and the only
//! place this reduction could disagree with vanilla is a non-full-cube solid
//! block (e.g. a slab or stair) that vanilla does *not* treat as a conductor.
//! [`is_redstone_component`] is that exclusion list, and its doc comment
//! carries the reason `minecraft:target` and `minecraft:redstone_block` are
//! signal sources that stay *on* the conductor side of it. This
//! crate's worldgen has no such partial blocks in the positions redstone
//! would touch today (`crate::chunk`'s own module doc), so the reduction is
//! unexercised in the direction it could be wrong, the same "reduction, not
//! invented" framing `crate::gravity_tick::is_free` gives for its own
//! `isFree` narrowing.
//!
//! # What sources exist today
//!
//! **Relaying** families: lit redstone torches (standing and wall), repeaters
//! and comparators when `powered`, and observers when `powered` — see
//! `crate::redstone_diode`/`crate::redstone_observer`.
//!
//! **Input** families, the ones a player reaches for first, are
//! [`is_input_source`]'s nine. They emit these values, and the differences
//! between the rows are the point:
//!
//! | family | signal while active | strong power reaches |
//! |---|---|---|
//! | lever, button | 15 | the surface it is attached to (`getConnectedDirection`) |
//! | pressure plate | 15 | the block below it |
//! | weighted pressure plate | its own `power`, `0..=15` | the block below it |
//! | tripwire hook | 15 | the wall it faces |
//! | detector rail | 15 | the block below it |
//! | target | its own `power`, `0..=15` | nothing |
//! | daylight detector | its own `power`, `0..=15` | nothing |
//! | redstone block | 15 | nothing (but see [`control_input_signal`]) |
//!
//! **Collapsing any of those to a boolean 15 is a defect**, and so is assuming
//! a family with no `getDirectSignal` override sends strong power. Every value
//! above comes from the named block class's own `ownSignal`/`getDirectSignal`
//! in the jar, cited at each arm of [`own_signal`]/[`direct_signal`].
//!
//! # The half of each input family that is *not* modelled here
//!
//! This module answers "how much signal does this state emit". It does not
//! *produce* the states: something else has to write the `powered`/`power`
//! property, and that producer exists for only some of the nine.
//!
//! * **lever, button** — `crate::hand_use` flips `powered` from a right-click,
//!   and `crate::server`'s use-item-on path fans the change out to neighbours,
//!   so these two work end to end.
//! * **pressure plate, weighted pressure plate, detector rail** — need an
//!   entity-AABB-versus-block census (vanilla's own base-pressure-plate-block
//!   signal-strength getter counts entities inside `TOUCH_AABB`), which this crate has no collision
//!   system for. The read below is correct and its producer is missing, so
//!   these stay at their default `0` until something writes the property.
//! * **tripwire hook** — needs `minecraft:tripwire` string state and the
//!   two-hook span search in vanilla's own tripwire-hook-block calculate-state routine.
//! * **target** — needs projectile-hit dispatch plus the decay tick.
//! * **daylight detector** — needs a sky-light read at the detector's own
//!   position.
//! * **redstone block** — needs nothing; it is a constant and works as soon as
//!   one is placed.
//!
//! Wiring the read first is deliberate rather than an island: the read is what
//! every one of those producers would otherwise have to be written against, and
//! it was measurably the blocking half — a piston gate driven by a `powered`
//! lever scheduled zero commits while the identical gate driven by a redstone
//! torch passed.

use crate::neighbor_update::{Direction, ALL_DIRECTIONS};
use lodestone_data::block::Block;
use lodestone_data::block_properties::{BuiltinPropertyValue, Properties, PropertyKey, PropertyValue};
use lodestone_data::block_states::StateId;
use lodestone_model::BlockPos;

/// The validated global state read by redstone queries and mutations.
pub type WorldState = StateId;

/// A world read with comparator analog output attached to its position.
pub trait RedstoneLookup {
    fn state_at(&self, pos: BlockPos) -> WorldState;
    fn comparator_output(&self, pos: BlockPos) -> u8;
}

impl<F> RedstoneLookup for F
where
    F: Fn(BlockPos) -> WorldState,
{
    fn state_at(&self, pos: BlockPos) -> WorldState {
        self(pos)
    }

    fn comparator_output(&self, _pos: BlockPos) -> u8 {
        0
    }
}

#[cfg(test)]
pub(crate) fn configured_state(
    block: Block,
    values: &[(PropertyKey, BuiltinPropertyValue)],
) -> StateId {
    let properties = values.iter().fold(
        Properties::from_state_id(block.default_state()),
        |properties, &(key, value)| {
            properties
                .with_builtin(key, value)
                .expect("fixture property must be valid for its block")
        },
    );
    Properties::state_for_block(block, &properties)
        .expect("fixture state must exist in the generated state table")
}

pub(crate) fn property(state: StateId, key: PropertyKey) -> Option<BuiltinPropertyValue> {
    Properties::from_state_id(state).get(key)?.builtin_value()
}

pub(crate) fn property_bool(state: StateId, key: PropertyKey) -> Option<bool> {
    match property(state, key)? {
        BuiltinPropertyValue::True => Some(true),
        BuiltinPropertyValue::False => Some(false),
        _ => None,
    }
}

pub(crate) fn property_u8(state: StateId, key: PropertyKey) -> Option<u8> {
    match property(state, key)? {
        BuiltinPropertyValue::Value0 => Some(0),
        BuiltinPropertyValue::Value1 => Some(1),
        BuiltinPropertyValue::Value2 => Some(2),
        BuiltinPropertyValue::Value3 => Some(3),
        BuiltinPropertyValue::Value4 => Some(4),
        BuiltinPropertyValue::Value5 => Some(5),
        BuiltinPropertyValue::Value6 => Some(6),
        BuiltinPropertyValue::Value7 => Some(7),
        BuiltinPropertyValue::Value8 => Some(8),
        BuiltinPropertyValue::Value9 => Some(9),
        BuiltinPropertyValue::Value10 => Some(10),
        BuiltinPropertyValue::Value11 => Some(11),
        BuiltinPropertyValue::Value12 => Some(12),
        BuiltinPropertyValue::Value13 => Some(13),
        BuiltinPropertyValue::Value14 => Some(14),
        BuiltinPropertyValue::Value15 => Some(15),
        BuiltinPropertyValue::Value16 => Some(16),
        BuiltinPropertyValue::Value17 => Some(17),
        BuiltinPropertyValue::Value18 => Some(18),
        BuiltinPropertyValue::Value19 => Some(19),
        BuiltinPropertyValue::Value20 => Some(20),
        BuiltinPropertyValue::Value21 => Some(21),
        BuiltinPropertyValue::Value22 => Some(22),
        BuiltinPropertyValue::Value23 => Some(23),
        BuiltinPropertyValue::Value24 => Some(24),
        BuiltinPropertyValue::Value25 => Some(25),
        _ => None,
    }
}

pub(crate) fn numeric_property_value(value: u8) -> Option<BuiltinPropertyValue> {
    Some(match value {
        0 => BuiltinPropertyValue::Value0,
        1 => BuiltinPropertyValue::Value1,
        2 => BuiltinPropertyValue::Value2,
        3 => BuiltinPropertyValue::Value3,
        4 => BuiltinPropertyValue::Value4,
        5 => BuiltinPropertyValue::Value5,
        6 => BuiltinPropertyValue::Value6,
        7 => BuiltinPropertyValue::Value7,
        8 => BuiltinPropertyValue::Value8,
        9 => BuiltinPropertyValue::Value9,
        10 => BuiltinPropertyValue::Value10,
        11 => BuiltinPropertyValue::Value11,
        12 => BuiltinPropertyValue::Value12,
        13 => BuiltinPropertyValue::Value13,
        14 => BuiltinPropertyValue::Value14,
        15 => BuiltinPropertyValue::Value15,
        16 => BuiltinPropertyValue::Value16,
        17 => BuiltinPropertyValue::Value17,
        18 => BuiltinPropertyValue::Value18,
        19 => BuiltinPropertyValue::Value19,
        20 => BuiltinPropertyValue::Value20,
        21 => BuiltinPropertyValue::Value21,
        22 => BuiltinPropertyValue::Value22,
        23 => BuiltinPropertyValue::Value23,
        24 => BuiltinPropertyValue::Value24,
        25 => BuiltinPropertyValue::Value25,
        _ => return None,
    })
}

pub(crate) fn property_direction(state: StateId, key: PropertyKey) -> Option<Direction> {
    Some(match property(state, key)? {
        BuiltinPropertyValue::Down => Direction::Down,
        BuiltinPropertyValue::Up => Direction::Up,
        BuiltinPropertyValue::North => Direction::North,
        BuiltinPropertyValue::South => Direction::South,
        BuiltinPropertyValue::West => Direction::West,
        BuiltinPropertyValue::East => Direction::East,
        _ => return None,
    })
}

pub(crate) fn with_property(
    state: StateId,
    key: PropertyKey,
    value: PropertyValue,
) -> Option<StateId> {
    let properties = Properties::from_state_id(state)
        .with_builtin(key, value.builtin_value()?)
        .ok()?;
    Properties::state_for_block(state.block(), &properties)
}

pub const WIRE: Block = Block::RedstoneWire;
pub const TORCH: Block = Block::RedstoneTorch;
pub const WALL_TORCH: Block = Block::RedstoneWallTorch;
pub const REPEATER: Block = Block::Repeater;
pub const COMPARATOR: Block = Block::Comparator;
pub const OBSERVER: Block = Block::Observer;
pub const LEVER: Block = Block::Lever;
pub const TRIPWIRE_HOOK: Block = Block::TripwireHook;
pub const DETECTOR_RAIL: Block = Block::DetectorRail;
pub const TARGET: Block = Block::Target;
pub const DAYLIGHT_DETECTOR: Block = Block::DaylightDetector;
pub const REDSTONE_BLOCK: Block = Block::RedstoneBlock;

/// Scheduled-tick `kind` strings (`ScheduledTickQueue<T>` is
/// keyed by `T = String` in this crate — see `scheduled_tick.rs`'s own doc
/// comment for why a canonical name, not a `Block`/`Fluid` registry object,
/// is the faithful key here). One constant per block family that schedules a
/// delayed tick, matching vanilla dedup-by-`(pos, Block instance)`: a given
/// position holds at most one block, so a fixed string per family is exactly
/// as unique as vanilla's own `Block` singleton.
pub const TICK_TORCH: &str = "redstone:torch";
pub const TICK_REPEATER: &str = "redstone:repeater";
pub const TICK_COMPARATOR: &str = "redstone:comparator";
pub const TICK_OBSERVER: &str = "redstone:observer";

pub(crate) fn base_name(state: StateId) -> Block {
    state.block()
}

pub(crate) fn get_u32_property(state: StateId, key: PropertyKey) -> Option<u32> {
    property_u8(state, key).map(u32::from)
}

pub(crate) fn get_bool_property(state: StateId, key: PropertyKey) -> Option<bool> {
    property_bool(state, key)
}

pub(crate) fn get_str_property(state: StateId, key: PropertyKey) -> Option<BuiltinPropertyValue> {
    property(state, key)
}

pub(crate) fn direction_from_property(value: BuiltinPropertyValue) -> Option<Direction> {
    Some(match value {
        BuiltinPropertyValue::Down => Direction::Down,
        BuiltinPropertyValue::Up => Direction::Up,
        BuiltinPropertyValue::North => Direction::North,
        BuiltinPropertyValue::South => Direction::South,
        BuiltinPropertyValue::West => Direction::West,
        BuiltinPropertyValue::East => Direction::East,
        _ => return None,
    })
}

pub(crate) fn direction_property(d: Direction) -> BuiltinPropertyValue {
    match d {
        Direction::Down => BuiltinPropertyValue::Down,
        Direction::Up => BuiltinPropertyValue::Up,
        Direction::North => BuiltinPropertyValue::North,
        Direction::South => BuiltinPropertyValue::South,
        Direction::West => BuiltinPropertyValue::West,
        Direction::East => BuiltinPropertyValue::East,
    }
}

#[must_use]
pub fn is_wire(state: StateId) -> bool {
    base_name(state) == WIRE
}
#[must_use]
pub fn is_standing_torch(state: StateId) -> bool {
    base_name(state) == TORCH
}
#[must_use]
pub fn is_wall_torch(state: StateId) -> bool {
    base_name(state) == WALL_TORCH
}
#[must_use]
pub fn is_torch(state: StateId) -> bool {
    is_standing_torch(state) || is_wall_torch(state)
}
#[must_use]
pub fn is_repeater(state: StateId) -> bool {
    base_name(state) == REPEATER
}
#[must_use]
pub fn is_comparator(state: StateId) -> bool {
    base_name(state) == COMPARATOR
}
#[must_use]
pub fn is_hopper(state: StateId) -> bool {
    base_name(state) == Block::Hopper
}

/// `minecraft:lever`.
///
/// `crate::hand_use` carries its own copy of this predicate for the *interaction*
/// half (which `powered` property a right-click cycles); this one is the
/// *query* half. They are deliberately independent — `hand_use` already depends
/// on this module, so the reverse edge would be a cycle.
#[must_use]
pub fn is_lever(state: StateId) -> bool {
    base_name(state) == LEVER
}

/// Any of the fourteen button blocks — `stone`, `polished_blackstone` and the
/// twelve wooden species. Matched by suffix rather than by an enumerated table
/// for the same reason `is_weighted_pressure_plate` is not: a new wood species
/// adds a button and would otherwise silently emit no signal.
#[must_use]
pub fn is_button(state: StateId) -> bool {
    matches!(
        base_name(state),
        Block::StoneButton
            | Block::PolishedBlackstoneButton
            | Block::OakButton
            | Block::SpruceButton
            | Block::BirchButton
            | Block::JungleButton
            | Block::AcaciaButton
            | Block::CherryButton
            | Block::DarkOakButton
            | Block::PaleOakButton
            | Block::MangroveButton
            | Block::BambooButton
            | Block::CrimsonButton
            | Block::WarpedButton
    )
}

/// The two `WeightedPressurePlateBlock` registrations, which carry an analog
/// `POWER` rather than a boolean `POWERED`.
///
/// Checked **before** [`is_pressure_plate`], because both of these also end in
/// `_pressure_plate` and reading `powered` off one would always find nothing and
/// report `0`.
#[must_use]
pub fn is_weighted_pressure_plate(state: StateId) -> bool {
    matches!(base_name(state), Block::LightWeightedPressurePlate | Block::HeavyWeightedPressurePlate)
}

/// A boolean (`PressurePlateBlock`) pressure plate — every `*_pressure_plate`
/// that is not one of the two weighted ones.
#[must_use]
pub fn is_pressure_plate(state: StateId) -> bool {
    !is_weighted_pressure_plate(state)
        && matches!(
            base_name(state),
            Block::StonePressurePlate
                | Block::OakPressurePlate
                | Block::SprucePressurePlate
                | Block::BirchPressurePlate
                | Block::JunglePressurePlate
                | Block::AcaciaPressurePlate
                | Block::CherryPressurePlate
                | Block::DarkOakPressurePlate
                | Block::PaleOakPressurePlate
                | Block::MangrovePressurePlate
                | Block::BambooPressurePlate
                | Block::PolishedBlackstonePressurePlate
                | Block::CrimsonPressurePlate
                | Block::WarpedPressurePlate
        )
}

#[must_use]
pub fn is_tripwire_hook(state: StateId) -> bool {
    base_name(state) == TRIPWIRE_HOOK
}
#[must_use]
pub fn is_detector_rail(state: StateId) -> bool {
    base_name(state) == DETECTOR_RAIL
}
#[must_use]
pub fn is_target(state: StateId) -> bool {
    base_name(state) == TARGET
}
#[must_use]
pub fn is_daylight_detector(state: StateId) -> bool {
    base_name(state) == DAYLIGHT_DETECTOR
}
#[must_use]
pub fn is_redstone_block(state: StateId) -> bool {
    base_name(state) == REDSTONE_BLOCK
}

/// The `POWERED` property shared by lever, button, boolean pressure plate,
/// tripwire hook and detector rail. `false` for a state that does not name it,
/// matching every one of those families' `registerDefaultState(... POWERED,
/// false)`.
#[must_use]
pub fn powered_property(state: StateId) -> bool {
    get_bool_property(state, PropertyKey::Powered).unwrap_or(false)
}

/// The analog `POWER` property (vanilla's own block-state-properties registration, `0..=15`) carried
/// by a weighted pressure plate, a target and a daylight detector — the same
/// property dust uses, on blocks that are not dust.
#[must_use]
pub fn analog_power(state: StateId) -> u8 {
    get_u32_property(state, PropertyKey::Power).unwrap_or(0).min(15) as u8
}

/// Vanilla's own face-attached-horizontal-directional-block connected-direction getter — the
/// direction a lever or button *points*, i.e. away from the surface it is
/// attached to: `Down` for a ceiling mount, `Up` for a floor mount, and the
/// block's own `FACING` for a wall mount.
///
/// This is the one direction in which such a block sends **strong** power, and
/// (because the same expression is `getOpposite`d by `canSurvive`) the block
/// receiving it is exactly the one the lever is stuck to. Defaults to the wall
/// reading for a state naming no `face`, matching vanilla's own wall-attach-face being the
/// default in both registrations.
#[must_use]
pub fn attached_connected_direction(state: StateId) -> Direction {
    match get_str_property(state, PropertyKey::Face) {
        Some(BuiltinPropertyValue::Ceiling) => Direction::Down,
        Some(BuiltinPropertyValue::Floor) => Direction::Up,
        _ => get_str_property(state, PropertyKey::Facing)
            .and_then(direction_from_property)
            .unwrap_or(Direction::North),
    }
}

/// Vanilla's own tripwire-hook-block `FACING` — the direction the hook points away from its
/// wall, and the one direction it strongly powers.
#[must_use]
pub fn tripwire_hook_facing(state: StateId) -> Direction {
    get_str_property(state, PropertyKey::Facing)
        .and_then(direction_from_property)
        .unwrap_or(Direction::North)
}

/// Vanilla's own base-pressure-plate-block signal-for-state getter for both plate families:
/// `PressurePlateBlock` reads its boolean `POWERED` as 15-or-0, while
/// `WeightedPressurePlateBlock` reads its analog `POWER` directly.
///
/// **The two are not interchangeable.** A weighted plate's value comes from
/// `getSignalStrength`, `ceil(min(maxWeight, count) / maxWeight * 15)` with
/// `maxWeight` 15 for light and **150** for heavy — so one entity on a heavy
/// plate is `1`, not `15`, and ten entities are still `1`. Collapsing either
/// family to a boolean is the shape of defect this whole module was added to
/// fix. Computing that value needs an entity census this crate cannot run from
/// here (see this module's own doc comment on which halves are modelled); what
/// is read here is whatever the property already holds.
#[must_use]
pub fn pressure_plate_signal(state: StateId) -> u8 {
    if is_weighted_pressure_plate(state) {
        analog_power(state)
    } else if powered_property(state) {
        15
    } else {
        0
    }
}

/// The primary redstone *input* devices — every family whose whole job is to
/// turn a player or a world condition into a signal, as opposed to relaying one.
///
/// Grouped because they share a property no relaying family has: none of them
/// overrides `getSignal`, so each emits its own signal in **all six**
/// directions weakly (see [`weak_signal`]), and each restricts only its
/// *strong* output.
#[must_use]
pub fn is_input_source(state: StateId) -> bool {
    is_lever(state)
        || is_button(state)
        || is_pressure_plate(state)
        || is_weighted_pressure_plate(state)
        || is_tripwire_hook(state)
        || is_detector_rail(state)
        || is_target(state)
        || is_daylight_detector(state)
        || is_redstone_block(state)
}

/// A hopper's `ENABLED` block-state property — `true` (transferring) when the
/// hopper is **not** redstone-powered.
///
/// Defaults to `true` for a state that does not name it, matching vanilla's
/// `HopperBlock`'s own `registerDefaultState(... ENABLED, true)` and giving
/// a bare `minecraft:hopper` (which is what placement writes today) the
/// correct unlocked initial value.
#[must_use]
pub fn hopper_enabled(state: StateId) -> bool {
    get_bool_property(state, PropertyKey::Enabled).unwrap_or(true)
}

/// `state` with one property replaced, every other property preserved
/// verbatim, and the property appended if it was absent.
///
/// **Replaces in place rather than rebuilding from known properties**, and that
/// is load-bearing for delivery. `enabled` is a *real* property of
/// `minecraft:hopper`, so a state that keeps its whole property set intact still
/// matches `v770::resolve_state_id`'s exact tier and is delivered precisely. A
/// rebuild that dropped `facing` would fall to the subset tier and hand the
/// client a hopper pointing somewhere else. Property *order* does not matter, because that resolver
/// sorts before comparing.
/// Vanilla's own diode-block is-diode check.
#[must_use]
pub fn is_diode(state: StateId) -> bool {
    is_repeater(state) || is_comparator(state)
}
#[must_use]
pub fn is_observer(state: StateId) -> bool {
    base_name(state) == OBSERVER
}
/// The blocks this crate models signal-carrying behaviour for that are **not
/// full cubes**, and so are not redstone conductors.
///
/// The full-cube qualifier is the whole content of this predicate, because
/// [`is_redstone_conductor`] is its only caller and vanilla's
/// `isRedstoneConductor` default is `isCollisionShapeFullBlock`. Wire, torches,
/// diodes, observers, levers, buttons, plates, tripwire hooks, detector rails
/// and daylight detectors all fail that test in the jar.
///
/// [`is_target`] and [`is_redstone_block`] are deliberately **absent** even
/// though both are signal sources: both register with a plain full collision
/// cube, so vanilla treats them as conductors and so must we. Listing them here
/// would stop a `minecraft:redstone_block` relaying strong power through
/// [`signal_at`]'s conductor wrap, which is how a block of redstone under a
/// wire-topped stone block works at all.
#[must_use]
pub fn is_redstone_component(state: StateId) -> bool {
    is_wire(state)
        || is_torch(state)
        || is_diode(state)
        || is_observer(state)
        || is_lever(state)
        || is_button(state)
        || is_pressure_plate(state)
        || is_weighted_pressure_plate(state)
        || is_tripwire_hook(state)
        || is_detector_rail(state)
        || is_daylight_detector(state)
}

/// The reduced conductor predicate — see this module's own doc comment for
/// the full citation and named gap.
#[must_use]
pub fn is_redstone_conductor(state: StateId) -> bool {
    !matches!(base_name(state), Block::Air | Block::CaveAir | Block::VoidAir | Block::Water | Block::Lava)
        && !is_redstone_component(state)
}

/// Dust's own `POWER` property (vanilla's own redstone-wire-block registration,
/// `0..=15`). `0` for anything that is not
/// wire — callers gate on [`is_wire`] first when the distinction matters.
#[must_use]
pub fn wire_power(state: StateId) -> u8 {
    if !is_wire(state) {
        return 0;
    }
    get_u32_property(state, PropertyKey::Power).unwrap_or(0).min(15) as u8
}

#[must_use]
pub fn torch_lit(state: StateId) -> bool {
    get_bool_property(state, PropertyKey::Lit).unwrap_or(true)
}

/// The wall torch's `FACING` — the face it is mounted against (the
/// direction pointing *into* the block it's attached to), vanilla's own
/// horizontal-directional-block `FACING` (its own wall-torch-block registration).
#[must_use]
pub fn wall_torch_facing(state: StateId) -> Direction {
    get_str_property(state, PropertyKey::Facing)
        .and_then(direction_from_property)
        .unwrap_or(Direction::North)
}

#[must_use]
pub fn diode_facing(state: StateId) -> Direction {
    get_str_property(state, PropertyKey::Facing)
        .and_then(direction_from_property)
        .unwrap_or(Direction::North)
}
#[must_use]
pub fn diode_powered(state: StateId) -> bool {
    get_bool_property(state, PropertyKey::Powered).unwrap_or(false)
}
#[must_use]
pub fn repeater_locked(state: StateId) -> bool {
    get_bool_property(state, PropertyKey::Locked).unwrap_or(false)
}
/// Vanilla's own repeater-block `DELAY`, `1..=4` (vanilla default `1`).
#[must_use]
pub fn repeater_delay_ticks(state: StateId) -> u32 {
    get_u32_property(state, PropertyKey::Delay).unwrap_or(1).clamp(1, 4)
}
#[must_use]
pub fn comparator_mode_subtract(state: StateId) -> bool {
    get_str_property(state, PropertyKey::Mode) == Some(BuiltinPropertyValue::Subtract)
}
#[must_use]
pub fn observer_facing(state: StateId) -> Direction {
    get_str_property(state, PropertyKey::Facing)
        .and_then(direction_from_property)
        .unwrap_or(Direction::South)
}
#[must_use]
pub fn observer_powered(state: StateId) -> bool {
    get_bool_property(state, PropertyKey::Powered).unwrap_or(false)
}

/// A diode's own output value while powered — vanilla's own diode-block
/// output-signal getter
/// defaults to `15` (unmodified by
/// the repeater block); the comparator block overrides it to read its stored
/// analog output from the coordinate-aware lookup.
#[must_use]
fn diode_output_signal(state: StateId, comparator_output: u8) -> u8 {
    if is_comparator(state) {
        comparator_output.min(15)
    } else {
        15
    }
}

/// Vanilla's own `getOwnSignal`/each block's `ownSignal` override: wire ->
/// `POWER`; torch -> `15` if lit else `0`
/// (its own redstone-torch-block override); diode -> its own output
/// signal if `POWERED` else `0` (its own diode-block override);
/// observer -> `15` if `POWERED` else `0` (its own observer-block override).
#[must_use]
#[cfg(test)]
pub fn own_signal(state: StateId) -> u8 {
    own_signal_with_comparator_output(state, 0)
}

fn own_signal_with_comparator_output(state: StateId, comparator_output: u8) -> u8 {
    crate::redstone_counters::bump_state_parse();
    if is_wire(state) {
        wire_power(state)
    } else if is_torch(state) {
        if torch_lit(state) {
            15
        } else {
            0
        }
    } else if is_diode(state) {
        if diode_powered(state) {
            diode_output_signal(state, comparator_output)
        } else {
            0
        }
    } else if is_observer(state) {
        if observer_powered(state) {
            15
        } else {
            0
        }
    } else if is_lever(state) || is_button(state) || is_tripwire_hook(state) || is_detector_rail(state) {
        // Vanilla's own lever-block/button-block/
        // tripwire-hook-block/detector-rail-block own-signal overrides — all
        // four are `POWERED ? 15 : 0`.
        if powered_property(state) {
            15
        } else {
            0
        }
    } else if is_pressure_plate(state) || is_weighted_pressure_plate(state) {
        // Vanilla's own base-pressure-plate-block own-signal override delegates to `getSignalForState`.
        pressure_plate_signal(state)
    } else if is_target(state) || is_daylight_detector(state) {
        // Vanilla's own target-block/daylight-detector-block own-signal overrides — both
        // read their own analog `POWER`, so neither is a flat 15. A target
        // decays back to `0` on a scheduled tick after a projectile hit and a
        // daylight detector tracks sky light, and both of those producers are
        // separate from this read.
        analog_power(state)
    } else if is_redstone_block(state) {
        // Vanilla's own powered-block own-signal override — the unconditional constant source.
        15
    } else {
        0
    }
}

/// `true` for anything vanilla's own is-signal-source check reports true for among
/// the families this crate models. Every one of them returns an unconditional
/// `true` in the jar — a source is a source whether or not it is currently
/// emitting, and the *value* is what goes to zero: the redstone-torch block,
/// `DiodeBlock`, `ObserverBlock`, `LeverBlock`, `ButtonBlock`,
/// `BasePressurePlateBlock` (both plate families), `TripWireHookBlock`,
/// `DetectorRailBlock`, `TargetBlock`, `DaylightDetectorBlock` and
/// `PoweredBlock`.
///
/// Dust is deliberately excluded here: vanilla's own wire-block is-signal-source override
/// returns its own `shouldSignal` flag, which the general query path always
/// sees as `true` — vanilla's own signal-getter control-input-signal routine special-cases wire
/// before ever reaching this check, so wire never needs this predicate.
#[must_use]
pub fn is_signal_source(state: StateId) -> bool {
    is_torch(state) || is_diode(state) || is_observer(state) || is_input_source(state)
}

/// Vanilla's own `getSignal`'s per-block override — the *weak* signal a `state`
/// contributes toward a querier that reached it by travelling `direction`
/// (i.e. `querier.relative(direction) == the position holding `state``, the
/// same "direction travelled from the querier" convention every function in
/// this module and `crate::neighbor_update::Notification` shares).
///
/// `ignore_wire`: when `true`, a wire's own contribution is forced to `0` —
/// mirrors vanilla's own wire-block block-signal getter toggling its private
/// `shouldSignal` flag off for the duration of its own
/// `getBestNeighborSignal` call, so a
/// wire recomputing its target strength never counts an adjacent wire's
/// power as if it were a power *source* (that contribution is handled
/// separately, and with a `-1` decay, by
/// [`crate::redstone_wire::incoming_wire_signal`]).
#[must_use]
#[cfg(test)]
pub fn weak_signal(state: StateId, direction: Direction, ignore_wire: bool) -> u8 {
    weak_signal_with_comparator_output(state, direction, ignore_wire, 0)
}

fn weak_signal_with_comparator_output(
    state: StateId,
    direction: Direction,
    ignore_wire: bool,
    comparator_output: u8,
) -> u8 {
    if is_wire(state) {
        if ignore_wire || direction == Direction::Down {
            0
        } else {
            wire_power(state)
        }
    } else if is_standing_torch(state) {
        // RedstoneTorchBlock.getSignal (`:114-116`): every direction except UP.
        if direction == Direction::Up {
            0
        } else if torch_lit(state) {
            15
        } else {
            0
        }
    } else if is_wall_torch(state) {
        // RedstoneWallTorchBlock.getSignal (`:88-90`): every direction except
        // the one it's mounted against.
        if wall_torch_facing(state) == direction {
            0
        } else if torch_lit(state) {
            15
        } else {
            0
        }
    } else if is_diode(state) {
        // DiodeBlock.getSignal (`:152-154`): only in its own FACING direction.
        if diode_facing(state) == direction {
            own_signal_with_comparator_output(state, comparator_output)
        } else {
            0
        }
    } else if is_observer(state) {
        // ObserverBlock.getSignal (`:110-112`): only in its own FACING direction.
        if observer_facing(state) == direction {
            own_signal_with_comparator_output(state, comparator_output)
        } else {
            0
        }
    } else if is_input_source(state) {
        // **None of the nine input families overrides `getSignal`.** They stop
        // at `ownSignal`, so vanilla's own base block-behaviour get-signal's own body —
        // `return this.ownSignal(state, level, pos)` — applies, and the value is
        // the same in all six directions.
        //
        // That is worth stating rather than assuming, because every *relaying*
        // family above excludes at least one direction and the obvious guess is
        // that these do too. A lever really does weakly power a wire directly
        // above it, and the direction-restricted half of a lever lives entirely
        // in [`direct_signal`].
        own_signal_with_comparator_output(state, comparator_output)
    } else {
        0
    }
}

/// Vanilla's own `getDirectSignal`'s per-block override — the *strong* signal
/// `state` sends into a conductor it touches, same direction convention as
/// [`weak_signal`].
///
/// `ignore_wire` — see [`weak_signal`]'s own doc comment for the full
/// citation of vanilla's `shouldSignal` trick, and why it must reach here
/// too, not just the weak-signal path: vanilla's own wire-block direct-signal
/// getter is `shouldSignal ? getSignal(...) : 0`, and `shouldSignal` is
/// a field on the **one shared wire-block instance** every wire in
/// the world resolves to — so a wire recomputing its own target strength
/// suppresses *every* wire's direct signal for that call's duration, not
/// only its own. Missing this let a wire sitting on a conductor relay a
/// second wire's *current* power straight through the `getSignal`
/// conductor-wrap (below), bypassing `getIncomingWireSignal`'s own `-1`
/// decay entirely — caught by
/// `crate::redstone_wire::a_wire_reads_a_higher_wire_across_a_one_block_conductor_step`
/// initially reading `15` instead of the predicted `14`.
#[must_use]
#[cfg(test)]
pub fn direct_signal(state: StateId, direction: Direction, ignore_wire: bool) -> u8 {
    direct_signal_with_comparator_output(state, direction, ignore_wire, 0)
}

fn direct_signal_with_comparator_output(
    state: StateId,
    direction: Direction,
    ignore_wire: bool,
    comparator_output: u8,
) -> u8 {
    if is_torch(state) {
        // RedstoneTorchBlock.getDirectSignal (`:99-101`): only straight DOWN
        // from the querier's perspective — i.e. only the block directly
        // ABOVE a torch receives strong power from it. Wall torches inherit
        // this unmodified (no override in vanilla's own wall-torch block).
        if direction == Direction::Down {
            own_signal_with_comparator_output(state, comparator_output)
        } else {
            0
        }
    } else if is_wire(state) {
        if ignore_wire {
            0
        } else {
            weak_signal_with_comparator_output(state, direction, false, comparator_output)
        }
    } else if is_diode(state) || is_observer(state) {
        // Vanilla's own diode-block/observer-block direct-signal getters:
        // both delegate straight to `getSignal`.
        weak_signal_with_comparator_output(state, direction, false, comparator_output)
    } else if is_lever(state) || is_button(state) {
        // Vanilla's own lever-block/button-block direct-signal getters:
        // `POWERED && getConnectedDirection(state) == direction ? 15 : 0`.
        //
        // This is the arm that makes a lever on the *side* of a block power a
        // wire on *top* of that block, via `getDirectSignalTo`'s six-face scan.
        // With only the weak path wired, that circuit reads zero — the wire's
        // own neighbour is the stone, whose weak signal is 0.
        if powered_property(state) && attached_connected_direction(state) == direction {
            15
        } else {
            0
        }
    } else if is_pressure_plate(state) || is_weighted_pressure_plate(state) {
        // Vanilla's own base-pressure-plate-block direct-signal getter: `direction == UP` only, i.e.
        // only the block a plate is standing on receives strong power from it.
        if direction == Direction::Up {
            pressure_plate_signal(state)
        } else {
            0
        }
    } else if is_tripwire_hook(state) {
        // Vanilla's own tripwire-hook-block direct-signal getter: its own `FACING` only.
        if powered_property(state) && tripwire_hook_facing(state) == direction {
            15
        } else {
            0
        }
    } else if is_detector_rail(state) {
        // Vanilla's own detector-rail-block direct-signal getter: `direction == UP` only.
        if powered_property(state) && direction == Direction::Up {
            15
        } else {
            0
        }
    } else {
        // The target, daylight-detector and powered blocks override
        // neither `getSignal` nor `getDirectSignal`, so they keep
        // vanilla's own base block-behaviour direct-signal getter's `return 0` and send **no** strong
        // power at all. A block of redstone reaches a wire across a conductor
        // only through vanilla's own signal-getter control-input-signal routine's own
        // `is(Blocks.REDSTONE_BLOCK)` special case — see
        // [`control_input_signal`].
        0
    }
}

/// Vanilla's own signal-getter direct-signal-to routine: the
/// strongest direct/strong signal touching any of `pos`'s six faces.
/// `lookup` reads a block state at an absolute world position; see
/// `crate::random_tick`'s call sites for why it returns air rather than
/// erroring outside the currently-loaded chunk column (the same cross-chunk
/// limitation `crate::gravity_tick`'s own trigger surface already accepts).
#[must_use]
pub fn direct_signal_to<L>(lookup: &L, pos: BlockPos, ignore_wire: bool) -> u8
where
    L: RedstoneLookup + ?Sized,
{
    let mut best = 0u8;
    for direction in ALL_DIRECTIONS {
        let neighbor_pos = direction.relative(pos);
        let neighbor_state = lookup.state_at(neighbor_pos);
        let output = if is_comparator(neighbor_state) {
            lookup.comparator_output(neighbor_pos)
        } else {
            0
        };
        let signal = direct_signal_with_comparator_output(neighbor_state, direction, ignore_wire, output);
        if signal > best {
            best = signal;
        }
        if best >= 15 {
            return 15;
        }
    }
    best
}

/// Vanilla's own signal-getter get-signal routine: a redstone conductor additionally
/// carries the strongest signal touching *any* of its own six faces, not
/// just the one facing the querier — see this module's own doc comment for
/// why that is what lets a lever on the side of a block power a wire
/// sitting on top of it.
#[must_use]
pub fn signal_at<L>(lookup: &L, pos: BlockPos, direction: Direction, ignore_wire: bool) -> u8
where
    L: RedstoneLookup + ?Sized,
{
    let state = lookup.state_at(pos);
    let output = if is_comparator(state) { lookup.comparator_output(pos) } else { 0 };
    let weak = weak_signal_with_comparator_output(state, direction, ignore_wire, output);
    if is_redstone_conductor(state) {
        weak.max(direct_signal_to(lookup, pos, ignore_wire))
    } else {
        weak
    }
}

/// Vanilla's own signal-getter best-neighbor-signal routine: the strongest signal
/// any of `pos`'s six neighbours presents back at it.
#[must_use]
pub fn best_neighbor_signal<L>(lookup: &L, pos: BlockPos, ignore_wire: bool) -> u8
where
    L: RedstoneLookup + ?Sized,
{
    crate::redstone_counters::bump_signal_query();
    let mut best = 0u8;
    for direction in ALL_DIRECTIONS {
        let signal = signal_at(lookup, direction.relative(pos), direction, ignore_wire);
        if signal > best {
            best = signal;
        }
        if best >= 15 {
            return 15;
        }
    }
    best
}

/// Vanilla's own signal-getter control-input-signal routine — a repeater/comparator's own
/// side-input read.
///
/// **The `minecraft:redstone_block` arm is load-bearing and not a shortcut.**
/// The powered block overrides no `getDirectSignal`, so the generic
/// `isSignalSource() ? getDirectSignal(...) : 0` tail below returns `0` for a
/// block of redstone in every direction. Without vanilla's own explicit
/// `is(Blocks.REDSTONE_BLOCK) -> 15` branch — placed *before* the wire check —
/// a block of redstone beside a comparator supplies no side input at all, which
/// looks like a comparator bug rather than a missing table row.
#[must_use]
pub fn control_input_signal<L>(lookup: &L, pos: BlockPos, direction: Direction, only_diodes: bool) -> u8
where
    L: RedstoneLookup + ?Sized,
{
    let state = lookup.state_at(pos);
    let output = if is_comparator(state) { lookup.comparator_output(pos) } else { 0 };
    if only_diodes {
        if is_diode(state) {
            direct_signal_with_comparator_output(state, direction, false, output)
        } else {
            0
        }
    } else if is_redstone_block(state) {
        15
    } else if is_wire(state) {
        wire_power(state)
    } else if is_signal_source(state) {
        direct_signal_with_comparator_output(state, direction, false, output)
    } else {
        0
    }
}

/// Vanilla's own diode-block alternate-signal getter — the stronger of a diode's
/// two side inputs (its `FACING`'s clockwise/counterclockwise neighbours).
/// `side_input_diodes_only` is vanilla's own side-input-diodes-only check: `true` for
/// repeaters (only another diode's *output* can
/// lock a repeater), `false` for comparators (any signal source counts as a
/// side input).
#[must_use]
pub fn alternate_signal<L>(lookup: &L, pos: BlockPos, facing: Direction, side_input_diodes_only: bool) -> u8
where
    L: RedstoneLookup + ?Sized,
{
    let cw = facing.clockwise();
    let ccw = facing.counterclockwise();
    control_input_signal(lookup, cw.relative(pos), cw, side_input_diodes_only)
        .max(control_input_signal(lookup, ccw.relative(pos), ccw, side_input_diodes_only))
}

/// Vanilla's own diode-block input-signal getter, reduced: vanilla additionally
/// reads a two-away block's analog output signal (a hopper/chest's fill
/// level via its own get-analog-output-signal routine) and an item frame's
/// rotation when the immediate target is a redstone conductor — this crate
/// has no block-entity/analog-output query reachable from this module (see
/// `crate::redstone_diode`'s own doc comment for the full citation of this
/// exact trap). What *is* implemented is the base case every
/// circuit not touching a container needs: the direct signal facing into the
/// diode, maxed with an immediately-adjacent wire's own power (vanilla's own
/// belt-and-suspenders read, since `getSignal` for a wire in a
/// horizontal direction already returns the same value).
#[must_use]
pub fn input_signal<L>(lookup: &L, pos: BlockPos, facing: Direction) -> u8
where
    L: RedstoneLookup + ?Sized,
{
    let target_pos = facing.relative(pos);
    let signal = signal_at(lookup, target_pos, facing, false);
    if signal >= 15 {
        return signal;
    }
    let target_state = lookup.state_at(target_pos);
    signal.max(wire_power(target_state))
}

/// Builds a `Fn(BlockPos) -> WorldState` reading through `column`, the shared
/// shape every query function in this module (and `crate::redstone_wire`/
/// `crate::redstone_torch`/`crate::redstone_diode`/`crate::redstone_observer`)
/// takes as `lookup`. Positions outside `column`'s own 16×16×height footprint
/// read as air — the same cross-chunk-neighbour limitation
/// `crate::gravity_tick`'s own trigger surface already accepts (`tick_chunk`
/// has no neighbouring-column access), stated once here rather than at every
/// call site. Callers must not hold the returned closure alive across a
/// `column.set_block` call on the same column (it borrows `column`
/// immutably) — every call site in `crate::random_tick`/`crate::tick`
/// constructs a fresh one per query rather than reusing one across a
/// mutation, for exactly this reason.
#[cfg(test)]
pub fn make_lookup(column: &crate::chunk::ChunkColumn, min_x: i32, min_z: i32) -> impl Fn(BlockPos) -> WorldState + '_ {
    move |p: BlockPos| -> WorldState {
        let lx = p.x - min_x;
        let lz = p.z - min_z;
        if !(0..16).contains(&lx) || !(0..16).contains(&lz) || p.y < column.min_y || p.y >= column.min_y + column.height {
            return lodestone_data::block_states::air_state();
        }
        crate::redstone_counters::bump_cell_read();
        column.block_state_id(lx, p.y, lz)
    }
}

/// [`make_lookup`], backed by a [`crate::random_tick::RedstoneColumns`]
/// multi-column cache instead of a single column — the read half of
/// cross-chunk propagation. A position beyond the home
/// column's own 16×16 footprint is answered from whichever already-loaded
/// neighbour column it falls in, rather than unconditionally air; a
/// position whose own chunk is not resident still answers air, matching
/// vanilla's own boundary (a circuit does not propagate into a chunk nobody
/// is simulating) — this only extends reach to chunks the server is
/// already ticking, never generates one to answer a redstone read.
#[must_use]
pub fn make_columns_lookup<'a>(columns: &'a crate::random_tick::RedstoneColumns<'_, '_>) -> impl Fn(BlockPos) -> WorldState + 'a {
    move |p: BlockPos| columns.state(p)
}

#[cfg(test)]
mod tests {
    use super::*;

    macro_rules! state {
        ($block:ident $(, $key:ident = $value:ident)* $(,)?) => {
            configured_state(
                Block::$block,
                &[$((PropertyKey::$key, BuiltinPropertyValue::$value)),*],
            )
        };
    }

    fn world(entries: &[(BlockPos, StateId)]) -> impl Fn(BlockPos) -> WorldState + use<> {
        let entries = entries.to_vec();
        move |p: BlockPos| {
            entries
                .iter()
                .find(|(pos, _)| *pos == p)
                .map(|(_, s)| *s)
                .unwrap_or_else(lodestone_data::block_states::air_state)
        }
    }

    fn pos(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[test]
    fn conductor_predicate_excludes_air_and_every_redstone_component() {
        assert!(is_redstone_conductor(state!(Stone)));
        assert!(!is_redstone_conductor(state!(Air)));
        assert!(!is_redstone_conductor(state!(Water, Level = Value0)));
        assert!(!is_redstone_conductor(WIRE.default_state()));
        assert!(!is_redstone_conductor(state!(RedstoneTorch, Lit = True)));
        assert!(!is_redstone_conductor(state!(
            Repeater,
            Facing = North,
            Delay = Value1,
            Locked = False,
            Powered = False
        )));
        assert!(!is_redstone_conductor(state!(Observer, Facing = South, Powered = False)));
    }

    /// A lit standing torch gives weak signal 15 to every horizontal
    /// neighbour and to the block below it, but NOT upward (the block it's
    /// resting on top of, from the torch's own perspective, is queried with
    /// `direction = Up`).
    #[test]
    fn lit_standing_torch_signals_every_direction_except_up() {
        let torch = state!(RedstoneTorch, Lit = True);
        assert_eq!(weak_signal(torch, Direction::Up, false), 0, "control: UP must be the one excluded direction");
        for d in [Direction::Down, Direction::North, Direction::South, Direction::East, Direction::West] {
            assert_eq!(weak_signal(torch, d, false), 15, "direction {d:?} must carry full signal");
        }
    }

    #[test]
    fn unlit_torch_signals_nothing() {
        let torch = state!(RedstoneTorch, Lit = False);
        for d in [Direction::Down, Direction::North, Direction::South, Direction::East, Direction::West] {
            assert_eq!(weak_signal(torch, d, false), 0);
        }
    }

    /// The strong-power path: a lit torch sitting directly below `pos`
    /// gives the (conductor) block at `pos` a direct signal of 15 — cited
    /// from vanilla's own torch-block direct-signal getter, `direction == DOWN`.
    #[test]
    fn a_lit_torch_gives_direct_signal_to_the_block_directly_above_it() {
        let torch = state!(RedstoneTorch, Lit = True);
        assert_eq!(direct_signal(torch, Direction::Down, false), 15);
        // Negative control: every other direction gives zero direct signal.
        for d in [Direction::Up, Direction::North, Direction::South, Direction::East, Direction::West] {
            assert_eq!(direct_signal(torch, d, false), 0, "control failed: direction {d:?} must give zero direct signal");
        }
    }

    /// End-to-end `best_neighbor_signal`: a lit torch one step west of `pos`
    /// is the only source nearby — predicted value 15, not merely nonzero.
    #[test]
    fn best_neighbor_signal_finds_an_adjacent_lit_torch() {
        let origin = pos(0, 0, 0);
        let torch_pos = Direction::West.relative(origin);
        let w = world(&[(torch_pos, state!(RedstoneTorch, Lit = True))]);
        assert_eq!(best_neighbor_signal(&w, origin, false), 15);
    }

    /// Negative control: with no source anywhere nearby, `best_neighbor_signal`
    /// must read exactly zero, not some nonzero default.
    #[test]
    fn best_neighbor_signal_is_zero_with_no_source_nearby() {
        let w = world(&[]);
        assert_eq!(best_neighbor_signal(&w, pos(0, 0, 0), false), 0);
    }

    /// The strong-power case wired all the way through `signal_at`: a lit
    /// torch sits directly below a stone block (a conductor); a wire
    /// sitting on a *different* side of that same stone block must see the
    /// stone as carrying signal 15, via `getDirectSignalTo`'s "check all six
    /// faces" — not merely the face facing the torch.
    #[test]
    fn a_conductor_relays_strong_power_from_a_torch_below_it_to_every_other_face() {
        let stone_pos = pos(0, 1, 0);
        let torch_pos = pos(0, 0, 0); // directly below the stone
        let w = world(&[
            (stone_pos, state!(Stone)),
            (torch_pos, state!(RedstoneTorch, Lit = True)),
        ]);
        // Querying the stone's signal as seen from the EAST (i.e. a wire
        // sitting east of the stone, at the same height) must still read 15
        // — the torch is on the opposite (south... actually below) face,
        // proving the "any face" contract, not just "the face it happens to
        // be touching".
        assert_eq!(signal_at(&w, stone_pos, Direction::East, false), 15);
    }

    /// `ignore_wire` zeroes out an adjacent wire's own weak signal — the
    /// `shouldSignal = false` trick a wire's own recompute uses so it never
    /// double-counts a neighbouring wire's power as a *source*.
    #[test]
    fn ignore_wire_suppresses_an_adjacent_wires_weak_signal_but_not_a_torchs() {
        let wire_pos = pos(1, 0, 0);
        let torch_pos = pos(-1, 0, 0);
        let w = world(&[
            (wire_pos, state!(RedstoneWire, Power = Value10)),
            (torch_pos, state!(RedstoneTorch, Lit = True)),
        ]);
        assert_eq!(
            best_neighbor_signal(&w, pos(0, 0, 0), true),
            15,
            "the torch must still be found even while wire is ignored"
        );
        // Negative control: with the torch removed, the wire-ignoring scan
        // must read zero — proving the suppression actually discriminates
        // rather than the torch case being a coincidence.
        let w2 = world(&[(wire_pos, state!(RedstoneWire, Power = Value10))]);
        assert_eq!(best_neighbor_signal(&w2, pos(0, 0, 0), true), 0);
        // And WITHOUT ignore_wire, the same setup must see the wire's power.
        assert_eq!(best_neighbor_signal(&w2, pos(0, 0, 0), false), 10);
    }

    #[test]
    fn wall_torch_signals_every_direction_except_its_own_mount_face() {
        let torch = state!(RedstoneWallTorch, Facing = North, Lit = True);
        assert_eq!(weak_signal(torch, Direction::North, false), 0, "the mount face must be excluded");
        for d in [Direction::South, Direction::East, Direction::West, Direction::Up, Direction::Down] {
            assert_eq!(weak_signal(torch, d, false), 15);
        }
    }

    #[test]
    fn diode_signals_only_in_its_own_facing_direction() {
        let repeater = state!(Repeater, Facing = East, Delay = Value1, Locked = False, Powered = True);
        assert_eq!(weak_signal(repeater, Direction::East, false), 15);
        for d in [Direction::West, Direction::North, Direction::South, Direction::Up, Direction::Down] {
            assert_eq!(weak_signal(repeater, d, false), 0, "control failed: direction {d:?} must be silent");
        }
    }

    #[test]
    fn unpowered_diode_signals_nothing_even_in_its_own_direction() {
        let repeater = state!(Repeater, Facing = East, Delay = Value1, Locked = False, Powered = False);
        assert_eq!(weak_signal(repeater, Direction::East, false), 0);
    }

    /// `alternate_signal` reads the clockwise/counterclockwise neighbours of
    /// a diode facing `East` — clockwise(East) = South, counterclockwise(East)
    /// = North (vanilla's own clockwise-direction getter). Uses a
    /// WIRE as the side source rather than a torch: `control_input_signal`'s
    /// `!only_diodes` branch reads a wire's `POWER` directly regardless of
    /// direction, whereas a torch only ever contributes through
    /// `getDirectSignal`, which (per vanilla's own torch-block direct-signal
    /// getter) is nonzero **only** when `direction == DOWN` — a torch
    /// sitting to the *side* of a diode can never supply a side input at
    /// all, direct or otherwise. An earlier version of this test placed a
    /// torch here and asserted `15`; it was wrong, not the code — caught by
    /// this same test failing against the real implementation.
    #[test]
    fn alternate_signal_reads_the_clockwise_and_counterclockwise_neighbours() {
        let origin = pos(0, 0, 0);
        let south_pos = Direction::South.relative(origin);
        let w = world(&[(south_pos, state!(RedstoneWire, Power = Value10))]);
        // side_input_diodes_only = false (comparator-style): a wire counts.
        assert_eq!(alternate_signal(&w, origin, Direction::East, false), 10);
        // side_input_diodes_only = true (repeater-style): a bare wire does
        // NOT count as a lock source (only another diode's *output* can lock
        // a repeater) — must read zero, proving the flag actually
        // discriminates rather than being decorative.
        assert_eq!(alternate_signal(&w, origin, Direction::East, true), 0);
    }

    #[test]
    fn alternate_signal_with_diodes_only_accepts_a_diode_side_input() {
        let origin = pos(0, 0, 0);
        let north_pos = Direction::North.relative(origin);
        // `direct_signal` for a diode fires when its own `FACING` equals the
        // direction travelled FROM the querier TO the diode (the same
        // convention every function in this module shares — see
        // `weak_signal`'s own doc comment). From `origin`, the neighbour at
        // `north_pos` is reached by travelling North, so the repeater there
        // must have `FACING = north` for this to register — not `south`, an
        // earlier version of this fixture's mistake, corrected after this
        // test caught it failing (`0`, not the predicted `15`).
        let w = world(&[(
            north_pos,
            state!(Repeater, Facing = North, Delay = Value1, Locked = False, Powered = True),
        )]);
        assert_eq!(alternate_signal(&w, origin, Direction::East, true), 15);
    }

    #[test]
    fn input_signal_reads_a_lit_torch_facing_into_the_diode() {
        let origin = pos(0, 0, 0);
        let torch_pos = Direction::East.relative(origin);
        let w = world(&[(torch_pos, state!(RedstoneTorch, Lit = True))]);
        assert_eq!(input_signal(&w, origin, Direction::East), 15);
    }

    #[test]
    fn input_signal_is_zero_with_nothing_facing_into_the_diode() {
        let w = world(&[]);
        assert_eq!(input_signal(&w, pos(0, 0, 0), Direction::East), 0);
    }

    #[test]
    fn observer_signals_only_in_its_own_facing_direction_when_powered() {
        let observer = state!(Observer, Facing = North, Powered = True);
        assert_eq!(weak_signal(observer, Direction::North, false), 15);
        for d in [Direction::South, Direction::East, Direction::West, Direction::Up, Direction::Down] {
            assert_eq!(weak_signal(observer, d, false), 0);
        }
    }

    /// `control_input_signal`'s `!only_diodes` branch reaches
    /// `direct_signal`, and a torch's own `getDirectSignal` is nonzero only
    /// for `direction == Down` (vanilla's own torch-block direct-signal getter
    /// — see `alternate_signal_reads_the_clockwise_and_counterclockwise_neighbours`
    /// above for the same fact stated for the side-input case).
    /// `dir = Down` here is what actually exercises the accepting branch;
    /// an earlier version of this test used `East` and got `0` where it
    /// predicted `15` — the same "torch can't side-signal horizontally"
    /// mistake, caught the same way.
    #[test]
    fn control_input_signal_with_only_diodes_rejects_a_torch() {
        let origin = pos(0, 0, 0);
        let torch = state!(RedstoneTorch, Lit = True);
        let dir = Direction::Down;
        // control_input_signal's own `pos` parameter IS the neighbour being
        // queried (see `control_input_signal`'s own doc comment / the jar's
        // `getControlInputSignal(pos, direction, ...)` signature) — the
        // torch itself, not `origin`.
        let torch_pos = dir.relative(origin);
        assert_eq!(control_input_signal(&world(&[(torch_pos, torch)]), torch_pos, dir, true), 0);
        assert_eq!(control_input_signal(&world(&[(torch_pos, torch)]), torch_pos, dir, false), 15);
    }

    // -----------------------------------------------------------------------
    // The primary input devices
    //
    // Every expectation below is the value a named block class's own
    // `ownSignal`/`getDirectSignal` returns in the 26.2 decompile, hand-expanded
    // from the record rather than from any behaviour of this crate. The tables
    // are written so that a wrong-but-plausible implementation lands on a
    // different number, not merely on a different sign: see
    // `every_input_source_emits_its_own_exact_value_and_not_a_boolean_15` for the
    // two hypotheses each row is built to separate.
    // -----------------------------------------------------------------------

    /// The six directions, in `ALL_DIRECTIONS` order, for the "same in every
    /// direction" sweeps below.
    const EVERY_DIRECTION: [Direction; 6] = [
        Direction::Down,
        Direction::Up,
        Direction::North,
        Direction::South,
        Direction::West,
        Direction::East,
    ];

    /// `(state, its own signal)` for every input family, in both an emitting and
    /// a silent configuration.
    ///
    /// **The analog rows are the discriminating inputs.** A weighted plate at
    /// `power=3`, a target at `power=7` and a daylight detector at `power=11` all
    /// answer `15` under the wrong hypothesis that an active source is a boolean
    /// — which is exactly the shape this whole module landing corrects — and `0`
    /// under the wrong hypothesis that the family is unmodelled. Picking
    /// `power=15` for any of them would make the row pass under the boolean
    /// hypothesis too, so no row does.
    fn input_source_own_signal_table() -> Vec<(StateId, u8)> {
        vec![
            // LeverBlock.ownSignal / ButtonBlock.ownSignal: POWERED ? 15 : 0.
            (state!(Lever, Face = Wall, Facing = North, Powered = True), 15),
            (state!(Lever, Face = Wall, Facing = North, Powered = False), 0),
            (state!(StoneButton, Face = Wall, Facing = East, Powered = True), 15),
            (state!(OakButton, Face = Floor, Facing = East, Powered = True), 15),
            (state!(StoneButton, Face = Wall, Facing = East, Powered = False), 0),
            // PressurePlateBlock.getSignalForState: POWERED ? 15 : 0.
            (state!(StonePressurePlate, Powered = True), 15),
            (state!(OakPressurePlate, Powered = False), 0),
            // WeightedPressurePlateBlock.getSignalForState: the analog POWER.
            (state!(LightWeightedPressurePlate, Power = Value4), 4),
            (state!(HeavyWeightedPressurePlate, Power = Value3), 3),
            (state!(HeavyWeightedPressurePlate, Power = Value0), 0),
            // TripWireHookBlock.ownSignal / DetectorRailBlock.ownSignal.
            (state!(TripwireHook, Facing = West, Attached = True, Powered = True), 15),
            (state!(TripwireHook, Facing = West, Attached = True, Powered = False), 0),
            (state!(DetectorRail, Shape = NorthSouth, Powered = True), 15),
            (state!(DetectorRail, Shape = NorthSouth, Powered = False), 0),
            // TargetBlock.ownSignal / DaylightDetectorBlock.ownSignal: analog.
            (state!(Target, Power = Value7), 7),
            (state!(Target, Power = Value0), 0),
            (state!(DaylightDetector, Inverted = False, Power = Value11), 11),
            (state!(DaylightDetector, Inverted = True, Power = Value0), 0),
            // PoweredBlock.ownSignal: the unconditional constant.
            (state!(RedstoneBlock), 15),
        ]
    }

    /// Every input family's own signal is the value its jar class computes, and
    /// specifically **not** a boolean 15.
    ///
    /// Mismatches are collected rather than asserted inside the loop: an
    /// `assert_eq!` in the body reports the first bad row and leaves the other
    /// nineteen as arguments, so a neuter would demonstrate one family instead of
    /// all of them.
    #[test]
    fn every_input_source_emits_its_own_exact_value_and_not_a_boolean_15() {
        let mut wrong: Vec<String> = Vec::new();
        for (state, expected) in input_source_own_signal_table() {
            let got = own_signal(state);
            if got != expected {
                wrong.push(format!("{} -> own_signal {got}, expected {expected}", state.canonical_state()));
            }
            // Every one of them is also a signal *source* in the jar,
            // unconditionally — including the silent configurations, whose value
            // is 0 while the predicate stays true.
            if !is_signal_source(state) {
                wrong.push(format!("{} -> is_signal_source false, expected true", state.canonical_state()));
            }
        }
        assert!(
            wrong.is_empty(),
            "{} of {} input-source readings disagree with the jar's own record:\n  {}",
            wrong.len(),
            input_source_own_signal_table().len(),
            wrong.join("\n  ")
        );

        // The wrong hypothesis, evaluated: a boolean collapse would answer 15
        // for every active row. Three rows are analog and non-15, so the table
        // above is known to separate the two models rather than merely to agree
        // with one.
        let analog_rows: Vec<(StateId, u8)> = input_source_own_signal_table()
            .into_iter()
            .filter(|(_, v)| *v != 0 && *v != 15)
            .collect();
        assert_eq!(
            analog_rows.len(),
            4,
            "the table must carry rows whose correct value is neither 0 nor 15, or a \
             boolean-collapse implementation passes it; got {analog_rows:?}"
        );
    }

    /// **None of the nine input families overrides `getSignal`**, so each emits
    /// its own signal weakly in all six directions.
    ///
    /// The wrong hypothesis this separates is the one a reader of this module
    /// would most naturally reach for: every *relaying* family above excludes at
    /// least one direction (a standing torch excludes `Up`, a diode emits only
    /// along its `FACING`), so copying that shape would give `0` in at least one
    /// direction here. A lever really does weakly power a wire directly above it.
    #[test]
    fn an_input_sources_weak_signal_is_identical_in_all_six_directions() {
        let table = input_source_own_signal_table();
        let mut wrong: Vec<String> = Vec::new();
        let mut checked = 0usize;
        for (state, expected) in &table {
            for direction in EVERY_DIRECTION {
                checked += 1;
                let got = weak_signal(*state, direction, false);
                if got != *expected {
                    wrong.push(format!("{} toward {direction:?} -> {got}, expected {expected}", state.canonical_state()));
                }
            }
        }
        // Derived from the table rather than restated as a literal: the count is
        // 19 rows today and every row is a family/configuration someone may add
        // to, so a hardcoded product would fail on the next addition for no
        // reason a reader could act on. The floor is what keeps it non-vacuous.
        assert_eq!(
            checked,
            table.len() * EVERY_DIRECTION.len(),
            "the sweep must cover every family in every direction"
        );
        assert!(table.len() >= 15, "the table shrank to {} rows", table.len());
        assert!(
            wrong.is_empty(),
            "{} of {checked} weak-signal readings are direction-dependent, but no input \
             family overrides `getSignal`:\n  {}",
            wrong.len(),
            wrong.join("\n  ")
        );
    }

    /// `(state, the one direction that carries strong power, its value)`.
    ///
    /// Strong power is where the families differ from each other and from their
    /// own weak output, so this is the table that a "weak path only"
    /// implementation cannot satisfy. `attached_connected_direction` is what
    /// makes the three lever rows differ: a floor lever powers the block below
    /// it, a ceiling lever the block above, and a wall lever the wall.
    fn input_source_direct_signal_table() -> Vec<(StateId, Direction, u8)> {
        vec![
            // LeverBlock.getDirectSignal: getConnectedDirection(state) only.
            (state!(Lever, Face = Wall, Facing = North, Powered = True), Direction::North, 15),
            (state!(Lever, Face = Wall, Facing = East, Powered = True), Direction::East, 15),
            (state!(Lever, Face = Floor, Facing = North, Powered = True), Direction::Up, 15),
            (state!(Lever, Face = Ceiling, Facing = North, Powered = True), Direction::Down, 15),
            (state!(StoneButton, Face = Wall, Facing = South, Powered = True), Direction::South, 15),
            (state!(OakButton, Face = Floor, Facing = West, Powered = True), Direction::Up, 15),
            // BasePressurePlateBlock.getDirectSignal: UP only.
            (state!(StonePressurePlate, Powered = True), Direction::Up, 15),
            (state!(HeavyWeightedPressurePlate, Power = Value3), Direction::Up, 3),
            // TripWireHookBlock.getDirectSignal: its own FACING only.
            (state!(TripwireHook, Facing = West, Attached = True, Powered = True), Direction::West, 15),
            // DetectorRailBlock.getDirectSignal: UP only.
            (state!(DetectorRail, Shape = NorthSouth, Powered = True), Direction::Up, 15),
        ]
    }

    /// Strong power leaves each input family in **exactly one** direction, and
    /// the value there is the family's own — with `heavy_weighted_pressure_plate`
    /// carrying `3` rather than `15`, so the analog path is exercised on the
    /// strong side too and not only on the weak one.
    #[test]
    fn strong_power_from_an_input_source_leaves_in_exactly_one_direction() {
        let mut wrong: Vec<String> = Vec::new();
        for (state, carrying, value) in input_source_direct_signal_table() {
            let got = direct_signal(state, carrying, false);
            if got != value {
                wrong.push(format!("{} toward {carrying:?} -> {got}, expected {value}", state.canonical_state()));
            }
            for direction in EVERY_DIRECTION.into_iter().filter(|d| *d != carrying) {
                let silent = direct_signal(state, direction, false);
                if silent != 0 {
                    wrong.push(format!(
                        "{} toward {direction:?} -> {silent}, expected 0 (only {carrying:?} carries strong power)",
                        state.canonical_state()
                    ));
                }
            }
        }
        assert!(
            wrong.is_empty(),
            "{} strong-power reading(s) disagree with the jar:\n  {}",
            wrong.len(),
            wrong.join("\n  ")
        );
    }

    /// An unpowered lever, button, hook or rail sends **no** strong power even in
    /// the direction that would otherwise carry it — the `POWERED &&` half of
    /// each `getDirectSignal`, which a version reading only the direction would
    /// drop.
    #[test]
    fn an_inactive_input_source_sends_no_strong_power_in_its_own_direction() {
        let mut wrong: Vec<String> = Vec::new();
        for (state, carrying) in [
            (state!(Lever, Face = Wall, Facing = North, Powered = False), Direction::North),
            (state!(Lever, Face = Floor, Facing = North, Powered = False), Direction::Up),
            (state!(StoneButton, Face = Wall, Facing = South, Powered = False), Direction::South),
            (state!(StonePressurePlate, Powered = False), Direction::Up),
            (state!(HeavyWeightedPressurePlate, Power = Value0), Direction::Up),
            (state!(TripwireHook, Facing = West, Attached = False, Powered = False), Direction::West),
            (state!(DetectorRail, Shape = NorthSouth, Powered = False), Direction::Up),
        ] {
            let got = direct_signal(state, carrying, false);
            if got != 0 {
                wrong.push(format!("{} toward {carrying:?} -> {got}, expected 0", state.canonical_state()));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n  "));
    }

    /// **`target`, `daylight_detector` and `redstone_block` send no strong power
    /// at all**, in any direction, while still emitting weakly.
    ///
    /// None of the three overrides `getDirectSignal`, so each keeps
    /// vanilla's own base block-behaviour direct-signal getter's `return 0`. This is the row that would
    /// be got wrong by assuming "a source with signal 15 must strongly power
    /// something", and getting it wrong is invisible until a specific
    /// through-a-conductor contraption fails.
    #[test]
    fn the_three_full_cube_and_flat_sources_send_no_strong_power_in_any_direction() {
        let mut wrong: Vec<String> = Vec::new();
        for (state, weak) in [
            (state!(Target, Power = Value7), 7u8),
            (state!(DaylightDetector, Inverted = False, Power = Value11), 11),
            (state!(RedstoneBlock), 15),
        ] {
            // The premise: each one really is emitting, so a zero below is a
            // statement about the strong path and not about a silent block.
            if own_signal(state) != weak {
                wrong.push(format!("premise failed: {} own_signal {} != {weak}", state.canonical_state(), own_signal(state)));
            }
            for direction in EVERY_DIRECTION {
                let got = direct_signal(state, direction, false);
                if got != 0 {
                    wrong.push(format!("{} toward {direction:?} -> {got}, expected 0", state.canonical_state()));
                }
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n  "));
    }

    /// **The weak/strong discriminator: a lever on the *side* of a stone block
    /// powers a wire sitting on *top* of that block.**
    ///
    /// This is the circuit `signal_at`'s conductor wrap exists for, and it is the
    /// one case a weak-only implementation cannot satisfy — the wire's own
    /// neighbour is the stone, whose weak signal is `0`. The value has to arrive
    /// through `getDirectSignalTo`'s six-face scan, so the assertion below names
    /// both halves separately rather than only the composite.
    #[test]
    fn a_lever_on_the_side_of_a_conductor_powers_a_wire_on_top_of_it() {
        let stone_pos = pos(0, 1, 0);
        let wire_pos = pos(0, 2, 0);
        // The lever is north of the stone. A wall lever attaches to
        // `pos.relative(getConnectedDirection().getOpposite())`, so a lever at
        // `stone.north()` stuck to the stone has `facing=north` — not `south`.
        let lever_pos = Direction::North.relative(stone_pos);
        let w = world(&[
            (stone_pos, state!(Stone)),
            (wire_pos, state!(RedstoneWire, Power = Value0)),
            (lever_pos, state!(Lever, Face = Wall, Facing = North, Powered = True)),
        ]);

        // The weak half alone gives nothing: the stone is not a source.
        assert_eq!(
            weak_signal(state!(Stone), Direction::Down, false),
            0,
            "premise: a stone block has no weak signal of its own, so anything the wire \
             reads must have come through the strong path"
        );
        // The strong half is what supplies it.
        assert_eq!(
            direct_signal_to(&w, stone_pos, false),
            15,
            "the lever's `getDirectSignal` must reach the stone through the six-face scan"
        );
        // And composed: the wire above reads the stone by travelling Down.
        assert_eq!(signal_at(&w, stone_pos, Direction::Down, false), 15);
        assert_eq!(
            best_neighbor_signal(&w, wire_pos, true),
            15,
            "the wire's own recompute must see 15"
        );

        // **Control, and it must fail the same assertion.** The identical rig
        // with the lever facing EAST strongly powers a different block, so the
        // stone gets nothing and the wire reads exactly 0 — not merely "less".
        let elsewhere = world(&[
            (stone_pos, state!(Stone)),
            (wire_pos, state!(RedstoneWire, Power = Value0)),
            (lever_pos, state!(Lever, Face = Wall, Facing = East, Powered = True)),
        ]);
        assert_eq!(
            direct_signal_to(&elsewhere, stone_pos, false),
            0,
            "an east-facing lever must not strongly power the block to its south"
        );
        assert_eq!(best_neighbor_signal(&elsewhere, wire_pos, true), 0);

        // Second control: the same north-facing lever, unpowered.
        let off = world(&[
            (stone_pos, state!(Stone)),
            (wire_pos, state!(RedstoneWire, Power = Value0)),
            (lever_pos, state!(Lever, Face = Wall, Facing = North, Powered = False)),
        ]);
        assert_eq!(best_neighbor_signal(&off, wire_pos, true), 0);
    }

    /// A pressure plate strongly powers the block **below** it, so a wire beside
    /// that block reads 15 — the plate's own analogue of the lever case above,
    /// and the direction (`Up` from the querier's view) most easily got backwards.
    #[test]
    fn a_pressure_plate_strongly_powers_the_block_it_stands_on() {
        let stone_pos = pos(0, 0, 0);
        let plate_pos = Direction::Up.relative(stone_pos);
        let w = world(&[
            (stone_pos, state!(Stone)),
            (plate_pos, state!(StonePressurePlate, Powered = True)),
        ]);
        assert_eq!(direct_signal_to(&w, stone_pos, false), 15);
        // A wire east of the stone reads the stone by travelling East.
        assert_eq!(signal_at(&w, stone_pos, Direction::East, false), 15);

        // The weighted plate's analog value survives the same path — 3, not 15,
        // which a boolean strong path would give.
        let weighted = world(&[
            (stone_pos, state!(Stone)),
            (plate_pos, state!(HeavyWeightedPressurePlate, Power = Value3)),
        ]);
        assert_eq!(direct_signal_to(&weighted, stone_pos, false), 3);

        // Control: unpressed plate, same geometry, exactly zero.
        let off = world(&[
            (stone_pos, state!(Stone)),
            (plate_pos, state!(StonePressurePlate, Powered = False)),
        ]);
        assert_eq!(direct_signal_to(&off, stone_pos, false), 0);
    }

    /// A block of redstone reaches a comparator's side input only through
    /// `getControlInputSignal`'s own `is(Blocks.REDSTONE_BLOCK)` branch.
    ///
    /// The generic `isSignalSource() ? getDirectSignal(...) : 0` tail cannot do
    /// it, because `PoweredBlock` overrides no `getDirectSignal` — so this gate
    /// asserts the direct signal is `0` *and* the control input is `15`, which
    /// together pin the value to that one branch rather than to the tail.
    #[test]
    fn a_redstone_block_supplies_a_side_input_only_through_the_explicit_branch() {
        let origin = pos(0, 0, 0);
        let dir = Direction::East;
        let block_pos = dir.relative(origin);
        let w = world(&[(block_pos, state!(RedstoneBlock))]);

        assert_eq!(
            direct_signal(state!(RedstoneBlock), dir, false),
            0,
            "premise: a block of redstone has no direct signal, so the 15 below cannot have \
             come from the generic signal-source tail"
        );
        assert_eq!(control_input_signal(&w, block_pos, dir, false), 15);
        // `only_diodes` (a repeater's lock read) must still reject it: a block of
        // redstone is not a diode, and only a diode's output can lock a repeater.
        assert_eq!(control_input_signal(&w, block_pos, dir, true), 0);

        // And the comparator side-input path end to end: clockwise(East) = South.
        let south = Direction::South.relative(origin);
        let side = world(&[(south, state!(RedstoneBlock))]);
        assert_eq!(alternate_signal(&side, origin, Direction::East, false), 15);
        assert_eq!(alternate_signal(&side, origin, Direction::East, true), 0);
    }

    /// The conductor split across the new families: the non-full-cube ones are
    /// excluded, and `target`/`redstone_block` — both full cubes in the jar, and
    /// both signal sources — stay conductors.
    ///
    /// Getting `redstone_block` wrong here is not cosmetic: a non-conductor does
    /// not get `signal_at`'s `getDirectSignalTo` wrap, so a block of redstone
    /// under a wire-topped stone block would stop working.
    #[test]
    fn the_conductor_split_follows_the_full_cube_shape_not_the_source_predicate() {
        let mut wrong: Vec<String> = Vec::new();
        for (state, want_conductor) in [
            (state!(Lever, Face = Wall, Facing = North, Powered = True), false),
            (state!(StoneButton, Face = Wall, Facing = East, Powered = True), false),
            (state!(OakButton, Face = Floor, Facing = East, Powered = False), false),
            (state!(StonePressurePlate, Powered = True), false),
            (state!(LightWeightedPressurePlate, Power = Value4), false),
            (state!(HeavyWeightedPressurePlate, Power = Value0), false),
            (state!(TripwireHook, Facing = West, Attached = True, Powered = True), false),
            (state!(DetectorRail, Shape = NorthSouth, Powered = True), false),
            (state!(DaylightDetector, Inverted = False, Power = Value11), false),
            // Full collision cubes in `Blocks`, so conductors — and both are
            // signal sources, which is the coincidence this row exists to break.
            (state!(Target, Power = Value7), true),
            (state!(RedstoneBlock), true),
            (state!(Stone), true),
        ] {
            if is_redstone_conductor(state) != want_conductor {
                wrong.push(format!(
                    "{} -> is_redstone_conductor {}, expected {want_conductor}",
                    state.canonical_state(),
                    is_redstone_conductor(state)
                ));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n  "));
    }

    /// The two weighted plates must not be read as boolean plates, and vice
    /// versa — both families end in `_pressure_plate`, so a suffix test alone
    /// puts a weighted plate on the `powered` path where it would always read 0.
    #[test]
    fn the_weighted_plates_are_split_out_from_the_boolean_ones() {
        for weighted in [
            state!(LightWeightedPressurePlate, Power = Value4),
            state!(HeavyWeightedPressurePlate, Power = Value3),
        ] {
            assert!(is_weighted_pressure_plate(weighted), "{weighted:?}");
            assert!(!is_pressure_plate(weighted), "{weighted:?} must not take the boolean path");
        }
        for boolean in [
            state!(StonePressurePlate, Powered = True),
            state!(OakPressurePlate, Powered = True),
            state!(PolishedBlackstonePressurePlate, Powered = True),
        ] {
            assert!(is_pressure_plate(boolean), "{boolean:?}");
            assert!(!is_weighted_pressure_plate(boolean), "{boolean:?}");
        }
        // A weighted plate read through the boolean path would answer 0 at
        // power=3, which is the failure this split prevents.
        assert_eq!(pressure_plate_signal(state!(HeavyWeightedPressurePlate, Power = Value3)), 3);
        assert!(!powered_property(state!(HeavyWeightedPressurePlate, Power = Value3)));
    }

    /// `attached_connected_direction` for each `AttachFace`, plus the wall
    /// default for a state naming no `face`.
    #[test]
    fn the_attached_connected_direction_follows_the_attach_face() {
        assert_eq!(
            attached_connected_direction(state!(Lever, Face = Floor, Facing = North, Powered = False)),
            Direction::Up
        );
        assert_eq!(
            attached_connected_direction(state!(Lever, Face = Ceiling, Facing = North, Powered = False)),
            Direction::Down
        );
        assert_eq!(
            attached_connected_direction(state!(Lever, Face = Wall, Facing = South, Powered = False)),
            Direction::South
        );
        // No `face` at all falls back to the wall reading, matching
        // vanilla's own wall attach-face being the registered default.
        assert_eq!(
            attached_connected_direction(state!(Lever, Facing = West)),
            Direction::West
        );
    }

    /// The name predicates must not be so loose that an unrelated block becomes
    /// a power source — the two suffix matches (`_button`, `_pressure_plate`) are
    /// the risk, so this is their negative control.
    ///
    /// The rail and tripwire rows are the ones worth having: `powered_rail` and
    /// `activator_rail` both carry a `powered` property and are *not* signal
    /// sources in the jar (they consume power rather than produce it), and
    /// `minecraft:tripwire` is a different block from `minecraft:tripwire_hook`
    /// with the same prefix.
    #[test]
    fn nothing_unrelated_is_mistaken_for_an_input_source() {
        let mut wrong: Vec<String> = Vec::new();
        for state in [
            state!(Stone),
            state!(Air),
            state!(Water, Level = Value0),
            state!(OakPlanks),
            state!(Rail, Shape = NorthSouth),
            state!(PoweredRail, Shape = NorthSouth, Powered = True),
            state!(ActivatorRail, Shape = NorthSouth, Powered = True),
            state!(Tripwire, Attached = True, Powered = True),
            state!(Chest, Facing = North),
        ] {
            if is_input_source(state) {
                wrong.push(format!("{} is_input_source true, expected false", state.canonical_state()));
            }
            if own_signal(state) != 0 {
                wrong.push(format!("{} own_signal {}, expected 0", state.canonical_state(), own_signal(state)));
            }
        }
        // Wire and torches are excluded from `is_input_source` but *do* emit,
        // so they belong to the predicate half of this control and not to the
        // "emits nothing" half. Asserting `own_signal == 0` for them would be
        // wrong about the code, which is how this row was first written.
        for relaying in [state!(RedstoneWire, Power = Value15), state!(RedstoneTorch, Lit = True)] {
            if is_input_source(relaying) {
                wrong.push(format!("{} is_input_source true, expected false", relaying.canonical_state()));
            }
            if own_signal(relaying) != 15 {
                wrong.push(format!("{} own_signal {}, expected 15", relaying.canonical_state(), own_signal(relaying)));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n  "));
    }
}
