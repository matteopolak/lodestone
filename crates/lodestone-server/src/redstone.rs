//! Redstone signal computation — the "how much signal touches this position,
//! and from which side" query layer every redstone component reads from before
//! deciding what to do.
//! This module has **no notify/cascade logic of its own** — that lives in
//! `crate::random_tick`'s reaction dispatch, which calls
//! `crate::neighbor_update::NeighborPropagator` exactly the way
//! `crate::gravity_tick` already does. This module is pure queries plus a
//! handful of per-family state-string helpers.
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
use lodestone_model::BlockPos;

pub const WIRE: &str = "minecraft:redstone_wire";
pub const TORCH: &str = "minecraft:redstone_torch";
pub const WALL_TORCH: &str = "minecraft:redstone_wall_torch";
pub const REPEATER: &str = "minecraft:repeater";
pub const COMPARATOR: &str = "minecraft:comparator";
pub const OBSERVER: &str = "minecraft:observer";
pub const LEVER: &str = "minecraft:lever";
pub const TRIPWIRE_HOOK: &str = "minecraft:tripwire_hook";
pub const DETECTOR_RAIL: &str = "minecraft:detector_rail";
pub const TARGET: &str = "minecraft:target";
pub const DAYLIGHT_DETECTOR: &str = "minecraft:daylight_detector";
pub const REDSTONE_BLOCK: &str = "minecraft:redstone_block";

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

/// Strips a `[...]` block-state property suffix — the same convention every
/// other per-family module in this crate duplicates locally rather than
/// sharing (see `growth_tick.rs`'s own doc comment on `base_name` for why:
/// no dependency between per-family modules, only on the shared primitives
/// in `chunk.rs`/`neighbor_update.rs`).
pub(crate) fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

pub(crate) fn get_u32_property(state: &str, key: &str) -> Option<u32> {
    let (_, props) = state.split_once('[')?;
    let props = props.strip_suffix(']').unwrap_or(props);
    for pair in props.split(',') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return v.parse().ok();
            }
        }
    }
    None
}

pub(crate) fn get_bool_property(state: &str, key: &str) -> Option<bool> {
    let (_, props) = state.split_once('[')?;
    let props = props.strip_suffix(']').unwrap_or(props);
    for pair in props.split(',') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return match v {
                    "true" => Some(true),
                    "false" => Some(false),
                    _ => None,
                };
            }
        }
    }
    None
}

pub(crate) fn get_str_property<'a>(state: &'a str, key: &str) -> Option<&'a str> {
    let (_, props) = state.split_once('[')?;
    let props = props.strip_suffix(']').unwrap_or(props);
    for pair in props.split(',') {
        if let Some((k, v)) = pair.split_once('=') {
            if k == key {
                return Some(v);
            }
        }
    }
    None
}

pub(crate) fn direction_from_str(s: &str) -> Direction {
    match s {
        "down" => Direction::Down,
        "up" => Direction::Up,
        "north" => Direction::North,
        "south" => Direction::South,
        "west" => Direction::West,
        _ => Direction::East,
    }
}

pub(crate) fn direction_to_str(d: Direction) -> &'static str {
    match d {
        Direction::Down => "down",
        Direction::Up => "up",
        Direction::North => "north",
        Direction::South => "south",
        Direction::West => "west",
        Direction::East => "east",
    }
}

#[must_use]
pub fn is_wire(state: &str) -> bool {
    base_name(state) == WIRE
}
#[must_use]
pub fn is_standing_torch(state: &str) -> bool {
    base_name(state) == TORCH
}
#[must_use]
pub fn is_wall_torch(state: &str) -> bool {
    base_name(state) == WALL_TORCH
}
#[must_use]
pub fn is_torch(state: &str) -> bool {
    is_standing_torch(state) || is_wall_torch(state)
}
#[must_use]
pub fn is_repeater(state: &str) -> bool {
    base_name(state) == REPEATER
}
#[must_use]
pub fn is_comparator(state: &str) -> bool {
    base_name(state) == COMPARATOR
}
#[must_use]
pub fn is_hopper(state: &str) -> bool {
    base_name(state) == "minecraft:hopper"
}

/// `minecraft:lever`.
///
/// `crate::hand_use` carries its own copy of this predicate for the *interaction*
/// half (which `powered` property a right-click cycles); this one is the
/// *query* half. They are deliberately independent — `hand_use` already depends
/// on this module, so the reverse edge would be a cycle.
#[must_use]
pub fn is_lever(state: &str) -> bool {
    base_name(state) == LEVER
}

/// Any of the fourteen button blocks — `stone`, `polished_blackstone` and the
/// twelve wooden species. Matched by suffix rather than by an enumerated table
/// for the same reason `is_weighted_pressure_plate` is not: a new wood species
/// adds a button and would otherwise silently emit no signal.
#[must_use]
pub fn is_button(state: &str) -> bool {
    base_name(state)
        .strip_suffix("_button")
        .is_some_and(|rest| rest.len() > "minecraft:".len() && rest.starts_with("minecraft:"))
}

/// The two `WeightedPressurePlateBlock` registrations, which carry an analog
/// `POWER` rather than a boolean `POWERED`.
///
/// Checked **before** [`is_pressure_plate`], because both of these also end in
/// `_pressure_plate` and reading `powered` off one would always find nothing and
/// report `0`.
#[must_use]
pub fn is_weighted_pressure_plate(state: &str) -> bool {
    matches!(
        base_name(state),
        "minecraft:light_weighted_pressure_plate" | "minecraft:heavy_weighted_pressure_plate"
    )
}

/// A boolean (`PressurePlateBlock`) pressure plate — every `*_pressure_plate`
/// that is not one of the two weighted ones.
#[must_use]
pub fn is_pressure_plate(state: &str) -> bool {
    !is_weighted_pressure_plate(state)
        && base_name(state)
            .strip_suffix("_pressure_plate")
            .is_some_and(|rest| rest.len() > "minecraft:".len() && rest.starts_with("minecraft:"))
}

#[must_use]
pub fn is_tripwire_hook(state: &str) -> bool {
    base_name(state) == TRIPWIRE_HOOK
}
#[must_use]
pub fn is_detector_rail(state: &str) -> bool {
    base_name(state) == DETECTOR_RAIL
}
#[must_use]
pub fn is_target(state: &str) -> bool {
    base_name(state) == TARGET
}
#[must_use]
pub fn is_daylight_detector(state: &str) -> bool {
    base_name(state) == DAYLIGHT_DETECTOR
}
#[must_use]
pub fn is_redstone_block(state: &str) -> bool {
    base_name(state) == REDSTONE_BLOCK
}

/// The `POWERED` property shared by lever, button, boolean pressure plate,
/// tripwire hook and detector rail. `false` for a state that does not name it,
/// matching every one of those families' `registerDefaultState(... POWERED,
/// false)`.
#[must_use]
pub fn powered_property(state: &str) -> bool {
    get_bool_property(state, "powered").unwrap_or(false)
}

/// The analog `POWER` property (vanilla's own block-state-properties registration, `0..=15`) carried
/// by a weighted pressure plate, a target and a daylight detector — the same
/// property dust uses, on blocks that are not dust.
#[must_use]
pub fn analog_power(state: &str) -> u8 {
    get_u32_property(state, "power").unwrap_or(0).min(15) as u8
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
pub fn attached_connected_direction(state: &str) -> Direction {
    match get_str_property(state, "face") {
        Some("ceiling") => Direction::Down,
        Some("floor") => Direction::Up,
        _ => get_str_property(state, "facing").map(direction_from_str).unwrap_or(Direction::North),
    }
}

/// Vanilla's own tripwire-hook-block `FACING` — the direction the hook points away from its
/// wall, and the one direction it strongly powers.
#[must_use]
pub fn tripwire_hook_facing(state: &str) -> Direction {
    get_str_property(state, "facing").map(direction_from_str).unwrap_or(Direction::North)
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
pub fn pressure_plate_signal(state: &str) -> u8 {
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
pub fn is_input_source(state: &str) -> bool {
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
pub fn hopper_enabled(state: &str) -> bool {
    get_bool_property(state, "enabled").unwrap_or(true)
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
#[must_use]
pub(crate) fn with_property(state: &str, key: &str, value: &str) -> String {
    let Some((name, rest)) = state.split_once('[') else {
        return format!("{state}[{key}={value}]");
    };
    let mut parts: Vec<String> = Vec::new();
    let mut replaced = false;
    for kv in rest.trim_end_matches(']').split(',') {
        match kv.split_once('=') {
            Some((k, _)) if k == key => {
                parts.push(format!("{key}={value}"));
                replaced = true;
            }
            _ => parts.push(kv.to_owned()),
        }
    }
    if !replaced {
        parts.push(format!("{key}={value}"));
    }
    format!("{name}[{}]", parts.join(","))
}

/// Vanilla's own diode-block is-diode check.
#[must_use]
pub fn is_diode(state: &str) -> bool {
    is_repeater(state) || is_comparator(state)
}
#[must_use]
pub fn is_observer(state: &str) -> bool {
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
pub fn is_redstone_component(state: &str) -> bool {
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
pub fn is_redstone_conductor(state: &str) -> bool {
    !crate::chunk::is_air_or_fluid(state) && !is_redstone_component(state)
}

/// Dust's own `POWER` property (vanilla's own redstone-wire-block registration,
/// `0..=15`). `0` for anything that is not
/// wire — callers gate on [`is_wire`] first when the distinction matters.
#[must_use]
pub fn wire_power(state: &str) -> u8 {
    if !is_wire(state) {
        return 0;
    }
    get_u32_property(state, "power").unwrap_or(0).min(15) as u8
}

#[must_use]
pub fn torch_lit(state: &str) -> bool {
    get_bool_property(state, "lit").unwrap_or(true)
}

/// The wall torch's `FACING` — the face it is mounted against (the
/// direction pointing *into* the block it's attached to), vanilla's own
/// horizontal-directional-block `FACING` (its own wall-torch-block registration).
#[must_use]
pub fn wall_torch_facing(state: &str) -> Direction {
    get_str_property(state, "facing").map(direction_from_str).unwrap_or(Direction::North)
}

#[must_use]
pub fn diode_facing(state: &str) -> Direction {
    get_str_property(state, "facing").map(direction_from_str).unwrap_or(Direction::North)
}
#[must_use]
pub fn diode_powered(state: &str) -> bool {
    get_bool_property(state, "powered").unwrap_or(false)
}
#[must_use]
pub fn repeater_locked(state: &str) -> bool {
    get_bool_property(state, "locked").unwrap_or(false)
}
/// Vanilla's own repeater-block `DELAY`, `1..=4` (vanilla default `1`).
#[must_use]
pub fn repeater_delay_ticks(state: &str) -> u32 {
    get_u32_property(state, "delay").unwrap_or(1).clamp(1, 4)
}
#[must_use]
pub fn comparator_mode_subtract(state: &str) -> bool {
    get_str_property(state, "mode") == Some("subtract")
}
/// The comparator's last-computed analog output, `0..=15` — vanilla stores
/// this in a `ComparatorBlockEntity`; this
/// crate has no block-entity storage plumbed into this module (`ChunkColumn`
/// has no block-entity registry access from here — see
/// `crate::redstone_diode`'s own module doc for the full citation), so it is
/// encoded as an ordinary block-state property instead, `output=N`. This
/// changes *where the value lives*, not what it means or how it is computed
/// — every value this property ever holds is exactly
/// [`crate::redstone_diode::calculate_comparator_output`]'s own return
/// value, the same function a block-entity-backed version would call too.
#[must_use]
pub fn comparator_output(state: &str) -> u8 {
    get_u32_property(state, "output").unwrap_or(0).min(15) as u8
}

#[must_use]
pub fn observer_facing(state: &str) -> Direction {
    get_str_property(state, "facing").map(direction_from_str).unwrap_or(Direction::South)
}
#[must_use]
pub fn observer_powered(state: &str) -> bool {
    get_bool_property(state, "powered").unwrap_or(false)
}

/// A diode's own output value while powered — vanilla's own diode-block
/// output-signal getter
/// defaults to `15` (unmodified by
/// the repeater block); the comparator block overrides it to read its stored
/// analog output — see
/// [`comparator_output`]'s own doc comment for where that value lives here.
#[must_use]
fn diode_output_signal(state: &str) -> u8 {
    if is_comparator(state) {
        comparator_output(state)
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
pub fn own_signal(state: &str) -> u8 {
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
            diode_output_signal(state)
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
pub fn is_signal_source(state: &str) -> bool {
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
pub fn weak_signal(state: &str, direction: Direction, ignore_wire: bool) -> u8 {
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
            own_signal(state)
        } else {
            0
        }
    } else if is_observer(state) {
        // ObserverBlock.getSignal (`:110-112`): only in its own FACING direction.
        if observer_facing(state) == direction {
            own_signal(state)
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
        own_signal(state)
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
pub fn direct_signal(state: &str, direction: Direction, ignore_wire: bool) -> u8 {
    if is_torch(state) {
        // RedstoneTorchBlock.getDirectSignal (`:99-101`): only straight DOWN
        // from the querier's perspective — i.e. only the block directly
        // ABOVE a torch receives strong power from it. Wall torches inherit
        // this unmodified (no override in vanilla's own wall-torch block).
        if direction == Direction::Down {
            own_signal(state)
        } else {
            0
        }
    } else if is_wire(state) {
        if ignore_wire {
            0
        } else {
            weak_signal(state, direction, false)
        }
    } else if is_diode(state) || is_observer(state) {
        // Vanilla's own diode-block/observer-block direct-signal getters:
        // both delegate straight to `getSignal`.
        weak_signal(state, direction, false)
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

/// Vanilla's own `SignalGetter.getDirectSignalTo`: the
/// strongest direct/strong signal touching any of `pos`'s six faces.
/// `lookup` reads a block state at an absolute world position; see
/// `crate::random_tick`'s call sites for why it returns air rather than
/// erroring outside the currently-loaded chunk column (the same cross-chunk
/// limitation `crate::gravity_tick`'s own trigger surface already accepts).
#[must_use]
pub fn direct_signal_to<F>(lookup: &F, pos: BlockPos, ignore_wire: bool) -> u8
where
    F: Fn(BlockPos) -> String,
{
    let mut best = 0u8;
    for direction in ALL_DIRECTIONS {
        let neighbor_state = lookup(direction.relative(pos));
        let signal = direct_signal(&neighbor_state, direction, ignore_wire);
        if signal > best {
            best = signal;
        }
        if best >= 15 {
            return 15;
        }
    }
    best
}

/// `SignalGetter.getSignal` (`:65-69`): a redstone conductor additionally
/// carries the strongest signal touching *any* of its own six faces, not
/// just the one facing the querier — see this module's own doc comment for
/// why that is what lets a lever on the side of a block power a wire
/// sitting on top of it.
#[must_use]
pub fn signal_at<F>(lookup: &F, pos: BlockPos, direction: Direction, ignore_wire: bool) -> u8
where
    F: Fn(BlockPos) -> String,
{
    let state = lookup(pos);
    let weak = weak_signal(&state, direction, ignore_wire);
    if is_redstone_conductor(&state) {
        weak.max(direct_signal_to(lookup, pos, ignore_wire))
    } else {
        weak
    }
}

/// `SignalGetter.getBestNeighborSignal` (`:90-105`): the strongest signal
/// any of `pos`'s six neighbours presents back at it.
#[must_use]
pub fn best_neighbor_signal<F>(lookup: &F, pos: BlockPos, ignore_wire: bool) -> u8
where
    F: Fn(BlockPos) -> String,
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

/// `SignalGetter.getControlInputSignal` — a repeater/comparator's own
/// side-input read.
///
/// **The `minecraft:redstone_block` arm is load-bearing and not a shortcut.**
/// `PoweredBlock` overrides no `getDirectSignal`, so the generic
/// `isSignalSource() ? getDirectSignal(...) : 0` tail below returns `0` for a
/// block of redstone in every direction. Without vanilla's own explicit
/// `is(Blocks.REDSTONE_BLOCK) -> 15` branch — placed *before* the wire check —
/// a block of redstone beside a comparator supplies no side input at all, which
/// looks like a comparator bug rather than a missing table row.
#[must_use]
pub fn control_input_signal<F>(lookup: &F, pos: BlockPos, direction: Direction, only_diodes: bool) -> u8
where
    F: Fn(BlockPos) -> String,
{
    let state = lookup(pos);
    if only_diodes {
        if is_diode(&state) {
            direct_signal(&state, direction, false)
        } else {
            0
        }
    } else if is_redstone_block(&state) {
        15
    } else if is_wire(&state) {
        wire_power(&state)
    } else if is_signal_source(&state) {
        direct_signal(&state, direction, false)
    } else {
        0
    }
}

/// `DiodeBlock.getAlternateSignal` (`:125-134`) — the stronger of a diode's
/// two side inputs (its `FACING`'s clockwise/counterclockwise neighbours).
/// `side_input_diodes_only` is `DiodeBlock.sideInputDiodesOnly()`: `true` for
/// repeaters (only another diode's *output* can
/// lock a repeater), `false` for comparators (any signal source counts as a
/// side input).
#[must_use]
pub fn alternate_signal<F>(lookup: &F, pos: BlockPos, facing: Direction, side_input_diodes_only: bool) -> u8
where
    F: Fn(BlockPos) -> String,
{
    let cw = facing.clockwise();
    let ccw = facing.counterclockwise();
    control_input_signal(lookup, cw.relative(pos), cw, side_input_diodes_only)
        .max(control_input_signal(lookup, ccw.relative(pos), ccw, side_input_diodes_only))
}

/// `DiodeBlock.getInputSignal`, reduced: vanilla additionally
/// reads a two-away block's analog output signal (a hopper/chest's fill
/// level via `BlockState.getAnalogOutputSignal`) and an item frame's
/// rotation when the immediate target is a redstone conductor — this crate
/// has no block-entity/analog-output query reachable from this module (see
/// `crate::redstone_diode`'s own doc comment for the full citation of this
/// exact trap). What *is* implemented is the base case every
/// circuit not touching a container needs: the direct signal facing into the
/// diode, maxed with an immediately-adjacent wire's own power (vanilla's own
/// belt-and-suspenders read, since `getSignal` for a wire in a
/// horizontal direction already returns the same value).
#[must_use]
pub fn input_signal<F>(lookup: &F, pos: BlockPos, facing: Direction) -> u8
where
    F: Fn(BlockPos) -> String,
{
    let target_pos = facing.relative(pos);
    let signal = signal_at(lookup, target_pos, facing, false);
    if signal >= 15 {
        return signal;
    }
    let target_state = lookup(target_pos);
    signal.max(wire_power(&target_state))
}

/// Builds a `Fn(BlockPos) -> String` reading through `column`, the shared
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
#[must_use]
pub fn make_lookup(column: &crate::chunk::ChunkColumn, min_x: i32, min_z: i32) -> impl Fn(BlockPos) -> String + '_ {
    move |p: BlockPos| -> String {
        let lx = p.x - min_x;
        let lz = p.z - min_z;
        if !(0..16).contains(&lx) || !(0..16).contains(&lz) || p.y < column.min_y || p.y >= column.min_y + column.height {
            return "minecraft:air".to_string();
        }
        crate::redstone_counters::bump_cell_read();
        column.block_state(lx, p.y, lz).to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny fake world: an explicit map from position to block-state
    /// string, air everywhere unset — enough to build every fixture below
    /// with no `ChunkColumn` in scope, matching `crate::gravity_tick`'s own
    /// "pure decision, fake world via closure" testing style.
    fn world(entries: &[(BlockPos, &str)]) -> impl Fn(BlockPos) -> String + use<> {
        let entries: Vec<(BlockPos, String)> = entries.iter().map(|(p, s)| (*p, s.to_string())).collect();
        move |p: BlockPos| {
            entries
                .iter()
                .find(|(pos, _)| *pos == p)
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| "minecraft:air".to_string())
        }
    }

    fn pos(x: i32, y: i32, z: i32) -> BlockPos {
        BlockPos::new(x, y, z)
    }

    #[test]
    fn conductor_predicate_excludes_air_and_every_redstone_component() {
        assert!(is_redstone_conductor("minecraft:stone"));
        assert!(!is_redstone_conductor("minecraft:air"));
        assert!(!is_redstone_conductor("minecraft:water[level=0]"));
        assert!(!is_redstone_conductor(WIRE));
        assert!(!is_redstone_conductor("minecraft:redstone_torch[lit=true]"));
        assert!(!is_redstone_conductor("minecraft:repeater[facing=north,delay=1,locked=false,powered=false]"));
        assert!(!is_redstone_conductor("minecraft:observer[facing=south,powered=false]"));
    }

    /// A lit standing torch gives weak signal 15 to every horizontal
    /// neighbour and to the block below it, but NOT upward (the block it's
    /// resting on top of, from the torch's own perspective, is queried with
    /// `direction = Up`).
    #[test]
    fn lit_standing_torch_signals_every_direction_except_up() {
        let torch = "minecraft:redstone_torch[lit=true]";
        assert_eq!(weak_signal(torch, Direction::Up, false), 0, "control: UP must be the one excluded direction");
        for d in [Direction::Down, Direction::North, Direction::South, Direction::East, Direction::West] {
            assert_eq!(weak_signal(torch, d, false), 15, "direction {d:?} must carry full signal");
        }
    }

    #[test]
    fn unlit_torch_signals_nothing() {
        let torch = "minecraft:redstone_torch[lit=false]";
        for d in [Direction::Down, Direction::North, Direction::South, Direction::East, Direction::West] {
            assert_eq!(weak_signal(torch, d, false), 0);
        }
    }

    /// The strong-power path: a lit torch sitting directly below `pos`
    /// gives the (conductor) block at `pos` a direct signal of 15 — cited
    /// from `RedstoneTorchBlock.getDirectSignal`, `direction == DOWN`.
    #[test]
    fn a_lit_torch_gives_direct_signal_to_the_block_directly_above_it() {
        let torch = "minecraft:redstone_torch[lit=true]";
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
        let w = world(&[(torch_pos, "minecraft:redstone_torch[lit=true]")]);
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
        let w = world(&[(stone_pos, "minecraft:stone"), (torch_pos, "minecraft:redstone_torch[lit=true]")]);
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
            (wire_pos, "minecraft:redstone_wire[power=10]"),
            (torch_pos, "minecraft:redstone_torch[lit=true]"),
        ]);
        assert_eq!(
            best_neighbor_signal(&w, pos(0, 0, 0), true),
            15,
            "the torch must still be found even while wire is ignored"
        );
        // Negative control: with the torch removed, the wire-ignoring scan
        // must read zero — proving the suppression actually discriminates
        // rather than the torch case being a coincidence.
        let w2 = world(&[(wire_pos, "minecraft:redstone_wire[power=10]")]);
        assert_eq!(best_neighbor_signal(&w2, pos(0, 0, 0), true), 0);
        // And WITHOUT ignore_wire, the same setup must see the wire's power.
        assert_eq!(best_neighbor_signal(&w2, pos(0, 0, 0), false), 10);
    }

    #[test]
    fn wall_torch_signals_every_direction_except_its_own_mount_face() {
        let torch = "minecraft:redstone_wall_torch[facing=north,lit=true]";
        assert_eq!(weak_signal(torch, Direction::North, false), 0, "the mount face must be excluded");
        for d in [Direction::South, Direction::East, Direction::West, Direction::Up, Direction::Down] {
            assert_eq!(weak_signal(torch, d, false), 15);
        }
    }

    #[test]
    fn diode_signals_only_in_its_own_facing_direction() {
        let repeater = "minecraft:repeater[facing=east,delay=1,locked=false,powered=true]";
        assert_eq!(weak_signal(repeater, Direction::East, false), 15);
        for d in [Direction::West, Direction::North, Direction::South, Direction::Up, Direction::Down] {
            assert_eq!(weak_signal(repeater, d, false), 0, "control failed: direction {d:?} must be silent");
        }
    }

    #[test]
    fn unpowered_diode_signals_nothing_even_in_its_own_direction() {
        let repeater = "minecraft:repeater[facing=east,delay=1,locked=false,powered=false]";
        assert_eq!(weak_signal(repeater, Direction::East, false), 0);
    }

    /// `alternate_signal` reads the clockwise/counterclockwise neighbours of
    /// a diode facing `East` — clockwise(East) = South, counterclockwise(East)
    /// = North (vanilla's own `Direction.getClockWise`). Uses a
    /// WIRE as the side source rather than a torch: `control_input_signal`'s
    /// `!only_diodes` branch reads a wire's `POWER` directly regardless of
    /// direction, whereas a torch only ever contributes through
    /// `getDirectSignal`, which (per `RedstoneTorchBlock.getDirectSignal`,
    /// `:99-101`) is nonzero **only** when `direction == DOWN` — a torch
    /// sitting to the *side* of a diode can never supply a side input at
    /// all, direct or otherwise. An earlier version of this test placed a
    /// torch here and asserted `15`; it was wrong, not the code — caught by
    /// this same test failing against the real implementation.
    #[test]
    fn alternate_signal_reads_the_clockwise_and_counterclockwise_neighbours() {
        let origin = pos(0, 0, 0);
        let south_pos = Direction::South.relative(origin);
        let w = world(&[(south_pos, &redstone_wire_power_fixture(10))]);
        // side_input_diodes_only = false (comparator-style): a wire counts.
        assert_eq!(alternate_signal(&w, origin, Direction::East, false), 10);
        // side_input_diodes_only = true (repeater-style): a bare wire does
        // NOT count as a lock source (only another diode's *output* can lock
        // a repeater) — must read zero, proving the flag actually
        // discriminates rather than being decorative.
        assert_eq!(alternate_signal(&w, origin, Direction::East, true), 0);
    }

    /// Builds a `minecraft:redstone_wire[power=N]` fixture without pulling in
    /// `crate::redstone_wire` (this module has no dependency on it — the
    /// reverse dependency direction, `redstone_wire` depends on `redstone`).
    fn redstone_wire_power_fixture(power: u8) -> String {
        format!("{WIRE}[power={power}]")
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
        let w = world(&[(north_pos, "minecraft:repeater[facing=north,delay=1,locked=false,powered=true]")]);
        assert_eq!(alternate_signal(&w, origin, Direction::East, true), 15);
    }

    #[test]
    fn input_signal_reads_a_lit_torch_facing_into_the_diode() {
        let origin = pos(0, 0, 0);
        let torch_pos = Direction::East.relative(origin);
        let w = world(&[(torch_pos, "minecraft:redstone_torch[lit=true]")]);
        assert_eq!(input_signal(&w, origin, Direction::East), 15);
    }

    #[test]
    fn input_signal_is_zero_with_nothing_facing_into_the_diode() {
        let w = world(&[]);
        assert_eq!(input_signal(&w, pos(0, 0, 0), Direction::East), 0);
    }

    #[test]
    fn observer_signals_only_in_its_own_facing_direction_when_powered() {
        let observer = "minecraft:observer[facing=north,powered=true]";
        assert_eq!(weak_signal(observer, Direction::North, false), 15);
        for d in [Direction::South, Direction::East, Direction::West, Direction::Up, Direction::Down] {
            assert_eq!(weak_signal(observer, d, false), 0);
        }
    }

    /// `control_input_signal`'s `!only_diodes` branch reaches
    /// `direct_signal`, and a torch's own `getDirectSignal` is nonzero only
    /// for `direction == Down` (`RedstoneTorchBlock.getDirectSignal`,
    /// `:99-101` — see `alternate_signal_reads_the_clockwise_and_counterclockwise_neighbours`
    /// above for the same fact stated for the side-input case).
    /// `dir = Down` here is what actually exercises the accepting branch;
    /// an earlier version of this test used `East` and got `0` where it
    /// predicted `15` — the same "torch can't side-signal horizontally"
    /// mistake, caught the same way.
    #[test]
    fn control_input_signal_with_only_diodes_rejects_a_torch() {
        let origin = pos(0, 0, 0);
        let torch = "minecraft:redstone_torch[lit=true]";
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
    fn input_source_own_signal_table() -> Vec<(&'static str, u8)> {
        vec![
            // LeverBlock.ownSignal / ButtonBlock.ownSignal: POWERED ? 15 : 0.
            ("minecraft:lever[face=wall,facing=north,powered=true]", 15),
            ("minecraft:lever[face=wall,facing=north,powered=false]", 0),
            ("minecraft:stone_button[face=wall,facing=east,powered=true]", 15),
            ("minecraft:oak_button[face=floor,facing=east,powered=true]", 15),
            ("minecraft:stone_button[face=wall,facing=east,powered=false]", 0),
            // PressurePlateBlock.getSignalForState: POWERED ? 15 : 0.
            ("minecraft:stone_pressure_plate[powered=true]", 15),
            ("minecraft:oak_pressure_plate[powered=false]", 0),
            // WeightedPressurePlateBlock.getSignalForState: the analog POWER.
            ("minecraft:light_weighted_pressure_plate[power=4]", 4),
            ("minecraft:heavy_weighted_pressure_plate[power=3]", 3),
            ("minecraft:heavy_weighted_pressure_plate[power=0]", 0),
            // TripWireHookBlock.ownSignal / DetectorRailBlock.ownSignal.
            ("minecraft:tripwire_hook[facing=west,attached=true,powered=true]", 15),
            ("minecraft:tripwire_hook[facing=west,attached=true,powered=false]", 0),
            ("minecraft:detector_rail[shape=north_south,powered=true]", 15),
            ("minecraft:detector_rail[shape=north_south,powered=false]", 0),
            // TargetBlock.ownSignal / DaylightDetectorBlock.ownSignal: analog.
            ("minecraft:target[power=7]", 7),
            ("minecraft:target[power=0]", 0),
            ("minecraft:daylight_detector[inverted=false,power=11]", 11),
            ("minecraft:daylight_detector[inverted=true,power=0]", 0),
            // PoweredBlock.ownSignal: the unconditional constant.
            ("minecraft:redstone_block", 15),
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
                wrong.push(format!("{state} -> own_signal {got}, expected {expected}"));
            }
            // Every one of them is also a signal *source* in the jar,
            // unconditionally — including the silent configurations, whose value
            // is 0 while the predicate stays true.
            if !is_signal_source(state) {
                wrong.push(format!("{state} -> is_signal_source false, expected true"));
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
        let analog_rows: Vec<(&str, u8)> = input_source_own_signal_table()
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
                let got = weak_signal(state, direction, false);
                if got != *expected {
                    wrong.push(format!("{state} toward {direction:?} -> {got}, expected {expected}"));
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
    fn input_source_direct_signal_table() -> Vec<(&'static str, Direction, u8)> {
        vec![
            // LeverBlock.getDirectSignal: getConnectedDirection(state) only.
            ("minecraft:lever[face=wall,facing=north,powered=true]", Direction::North, 15),
            ("minecraft:lever[face=wall,facing=east,powered=true]", Direction::East, 15),
            ("minecraft:lever[face=floor,facing=north,powered=true]", Direction::Up, 15),
            ("minecraft:lever[face=ceiling,facing=north,powered=true]", Direction::Down, 15),
            ("minecraft:stone_button[face=wall,facing=south,powered=true]", Direction::South, 15),
            ("minecraft:oak_button[face=floor,facing=west,powered=true]", Direction::Up, 15),
            // BasePressurePlateBlock.getDirectSignal: UP only.
            ("minecraft:stone_pressure_plate[powered=true]", Direction::Up, 15),
            ("minecraft:heavy_weighted_pressure_plate[power=3]", Direction::Up, 3),
            // TripWireHookBlock.getDirectSignal: its own FACING only.
            ("minecraft:tripwire_hook[facing=west,attached=true,powered=true]", Direction::West, 15),
            // DetectorRailBlock.getDirectSignal: UP only.
            ("minecraft:detector_rail[shape=north_south,powered=true]", Direction::Up, 15),
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
                wrong.push(format!("{state} toward {carrying:?} -> {got}, expected {value}"));
            }
            for direction in EVERY_DIRECTION.into_iter().filter(|d| *d != carrying) {
                let silent = direct_signal(state, direction, false);
                if silent != 0 {
                    wrong.push(format!(
                        "{state} toward {direction:?} -> {silent}, expected 0 (only {carrying:?} \
                         carries strong power)"
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
            ("minecraft:lever[face=wall,facing=north,powered=false]", Direction::North),
            ("minecraft:lever[face=floor,facing=north,powered=false]", Direction::Up),
            ("minecraft:stone_button[face=wall,facing=south,powered=false]", Direction::South),
            ("minecraft:stone_pressure_plate[powered=false]", Direction::Up),
            ("minecraft:heavy_weighted_pressure_plate[power=0]", Direction::Up),
            ("minecraft:tripwire_hook[facing=west,attached=false,powered=false]", Direction::West),
            ("minecraft:detector_rail[shape=north_south,powered=false]", Direction::Up),
        ] {
            let got = direct_signal(state, carrying, false);
            if got != 0 {
                wrong.push(format!("{state} toward {carrying:?} -> {got}, expected 0"));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n  "));
    }

    /// **`target`, `daylight_detector` and `redstone_block` send no strong power
    /// at all**, in any direction, while still emitting weakly.
    ///
    /// None of the three overrides `getDirectSignal`, so each keeps
    /// `BlockBehaviour.getDirectSignal`'s `return 0`. This is the row that would
    /// be got wrong by assuming "a source with signal 15 must strongly power
    /// something", and getting it wrong is invisible until a specific
    /// through-a-conductor contraption fails.
    #[test]
    fn the_three_full_cube_and_flat_sources_send_no_strong_power_in_any_direction() {
        let mut wrong: Vec<String> = Vec::new();
        for (state, weak) in [
            ("minecraft:target[power=7]", 7u8),
            ("minecraft:daylight_detector[inverted=false,power=11]", 11),
            ("minecraft:redstone_block", 15),
        ] {
            // The premise: each one really is emitting, so a zero below is a
            // statement about the strong path and not about a silent block.
            if own_signal(state) != weak {
                wrong.push(format!("premise failed: {state} own_signal {} != {weak}", own_signal(state)));
            }
            for direction in EVERY_DIRECTION {
                let got = direct_signal(state, direction, false);
                if got != 0 {
                    wrong.push(format!("{state} toward {direction:?} -> {got}, expected 0"));
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
            (stone_pos, "minecraft:stone"),
            (wire_pos, "minecraft:redstone_wire[power=0]"),
            (lever_pos, "minecraft:lever[face=wall,facing=north,powered=true]"),
        ]);

        // The weak half alone gives nothing: the stone is not a source.
        assert_eq!(
            weak_signal("minecraft:stone", Direction::Down, false),
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
            (stone_pos, "minecraft:stone"),
            (wire_pos, "minecraft:redstone_wire[power=0]"),
            (lever_pos, "minecraft:lever[face=wall,facing=east,powered=true]"),
        ]);
        assert_eq!(
            direct_signal_to(&elsewhere, stone_pos, false),
            0,
            "an east-facing lever must not strongly power the block to its south"
        );
        assert_eq!(best_neighbor_signal(&elsewhere, wire_pos, true), 0);

        // Second control: the same north-facing lever, unpowered.
        let off = world(&[
            (stone_pos, "minecraft:stone"),
            (wire_pos, "minecraft:redstone_wire[power=0]"),
            (lever_pos, "minecraft:lever[face=wall,facing=north,powered=false]"),
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
            (stone_pos, "minecraft:stone"),
            (plate_pos, "minecraft:stone_pressure_plate[powered=true]"),
        ]);
        assert_eq!(direct_signal_to(&w, stone_pos, false), 15);
        // A wire east of the stone reads the stone by travelling East.
        assert_eq!(signal_at(&w, stone_pos, Direction::East, false), 15);

        // The weighted plate's analog value survives the same path — 3, not 15,
        // which a boolean strong path would give.
        let weighted = world(&[
            (stone_pos, "minecraft:stone"),
            (plate_pos, "minecraft:heavy_weighted_pressure_plate[power=3]"),
        ]);
        assert_eq!(direct_signal_to(&weighted, stone_pos, false), 3);

        // Control: unpressed plate, same geometry, exactly zero.
        let off = world(&[
            (stone_pos, "minecraft:stone"),
            (plate_pos, "minecraft:stone_pressure_plate[powered=false]"),
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
        let w = world(&[(block_pos, "minecraft:redstone_block")]);

        assert_eq!(
            direct_signal("minecraft:redstone_block", dir, false),
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
        let side = world(&[(south, "minecraft:redstone_block")]);
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
            ("minecraft:lever[face=wall,facing=north,powered=true]", false),
            ("minecraft:stone_button[face=wall,facing=east,powered=true]", false),
            ("minecraft:oak_button[face=floor,facing=east,powered=false]", false),
            ("minecraft:stone_pressure_plate[powered=true]", false),
            ("minecraft:light_weighted_pressure_plate[power=4]", false),
            ("minecraft:heavy_weighted_pressure_plate[power=0]", false),
            ("minecraft:tripwire_hook[facing=west,attached=true,powered=true]", false),
            ("minecraft:detector_rail[shape=north_south,powered=true]", false),
            ("minecraft:daylight_detector[inverted=false,power=11]", false),
            // Full collision cubes in `Blocks`, so conductors — and both are
            // signal sources, which is the coincidence this row exists to break.
            ("minecraft:target[power=7]", true),
            ("minecraft:redstone_block", true),
            ("minecraft:stone", true),
        ] {
            if is_redstone_conductor(state) != want_conductor {
                wrong.push(format!(
                    "{state} -> is_redstone_conductor {}, expected {want_conductor}",
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
            "minecraft:light_weighted_pressure_plate[power=4]",
            "minecraft:heavy_weighted_pressure_plate[power=3]",
        ] {
            assert!(is_weighted_pressure_plate(weighted), "{weighted}");
            assert!(!is_pressure_plate(weighted), "{weighted} must not take the boolean path");
        }
        for boolean in [
            "minecraft:stone_pressure_plate[powered=true]",
            "minecraft:oak_pressure_plate[powered=true]",
            "minecraft:polished_blackstone_pressure_plate[powered=true]",
        ] {
            assert!(is_pressure_plate(boolean), "{boolean}");
            assert!(!is_weighted_pressure_plate(boolean), "{boolean}");
        }
        // A weighted plate read through the boolean path would answer 0 at
        // power=3, which is the failure this split prevents.
        assert_eq!(pressure_plate_signal("minecraft:heavy_weighted_pressure_plate[power=3]"), 3);
        assert!(!powered_property("minecraft:heavy_weighted_pressure_plate[power=3]"));
    }

    /// `attached_connected_direction` for each `AttachFace`, plus the wall
    /// default for a state naming no `face`.
    #[test]
    fn the_attached_connected_direction_follows_the_attach_face() {
        assert_eq!(
            attached_connected_direction("minecraft:lever[face=floor,facing=north,powered=false]"),
            Direction::Up
        );
        assert_eq!(
            attached_connected_direction("minecraft:lever[face=ceiling,facing=north,powered=false]"),
            Direction::Down
        );
        assert_eq!(
            attached_connected_direction("minecraft:lever[face=wall,facing=south,powered=false]"),
            Direction::South
        );
        // No `face` at all falls back to the wall reading, matching
        // `AttachFace.WALL` being the registered default.
        assert_eq!(
            attached_connected_direction("minecraft:lever[facing=west]"),
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
            "minecraft:stone",
            "minecraft:air",
            "minecraft:water[level=0]",
            "minecraft:oak_planks",
            "minecraft:rail[shape=north_south]",
            "minecraft:powered_rail[shape=north_south,powered=true]",
            "minecraft:activator_rail[shape=north_south,powered=true]",
            "minecraft:tripwire[attached=true,powered=true]",
            "minecraft:chest[facing=north]",
        ] {
            if is_input_source(state) {
                wrong.push(format!("{state} is_input_source true, expected false"));
            }
            if own_signal(state) != 0 {
                wrong.push(format!("{state} own_signal {}, expected 0", own_signal(state)));
            }
        }
        // Wire and torches are excluded from `is_input_source` but *do* emit,
        // so they belong to the predicate half of this control and not to the
        // "emits nothing" half. Asserting `own_signal == 0` for them would be
        // wrong about the code, which is how this row was first written.
        for relaying in ["minecraft:redstone_wire[power=15]", "minecraft:redstone_torch[lit=true]"] {
            if is_input_source(relaying) {
                wrong.push(format!("{relaying} is_input_source true, expected false"));
            }
            if own_signal(relaying) != 15 {
                wrong.push(format!("{relaying} own_signal {}, expected 15", own_signal(relaying)));
            }
        }
        assert!(wrong.is_empty(), "{}", wrong.join("\n  "));
    }
}
