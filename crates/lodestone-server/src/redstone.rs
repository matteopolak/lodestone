//! Redstone signal computation (issue #314's parent scope, shared by every
//! sub-issue: dust/torches #314, repeaters/comparators #315, observers #317)
//! — the "how much signal touches this position, and from which side" query
//! layer every redstone component reads from before deciding what to do.
//! This module has **no notify/cascade logic of its own** — that lives in
//! `crate::random_tick`'s reaction dispatch, which calls
//! `crate::neighbor_update::NeighborPropagator` (issue #308) exactly the way
//! `crate::gravity_tick` already does. This module is pure queries plus a
//! handful of per-family state-string helpers.
//!
//! # Where this comes from in the jar
//!
//! `net.minecraft.world.level.SignalGetter` (`SignalGetter.java`) is
//! vanilla's own query layer, and every function below is a direct,
//! citation-per-function port of it:
//!
//! - [`weak_signal`] ~ `BlockState.getSignal` (each block's own override —
//!   see the per-family jar citations inline below).
//! - [`direct_signal`] ~ `BlockState.getDirectSignal`.
//! - [`direct_signal_to`] ~ `SignalGetter.getDirectSignalTo`
//!   (`SignalGetter.java:17-46`).
//! - [`signal_at`] ~ `SignalGetter.getSignal` (`:65-69`): a redstone
//!   *conductor* additionally carries whatever direct/strong signal touches
//!   any of its six faces (`getDirectSignalTo`), which is how a lever on the
//!   side of a stone block can power a wire sitting on top of that same
//!   block — this crate models it for torches, since torches are the one
//!   source #314 asks for.
//! - [`best_neighbor_signal`] ~ `SignalGetter.getBestNeighborSignal`
//!   (`:90-105`).
//! - [`control_input_signal`] ~ `SignalGetter.getControlInputSignal`
//!   (`:48-59`) — repeaters/comparators' own side-input read (#315).
//!
//! `SignalGetter.DIRECTIONS` (`:11`, `Direction.values()`) is iterated by
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
//! [`is_redstone_conductor`] cannot check `BlockState.isRedstoneConductor`'s
//! real definition (a full-cube collision shape). The honest reduction used
//! here: **anything that is not air/fluid and not itself a redstone
//! component is a conductor** — every ordinary solid block (stone, dirt,
//! log, ...) is a conductor, exactly as in vanilla, and the only place this
//! reduction could disagree with vanilla is a non-full-cube solid block
//! (e.g. a slab or stair) that vanilla does *not* treat as a conductor. This
//! crate's worldgen has no such partial blocks in the positions redstone
//! would touch today (`crate::chunk`'s own module doc), so the reduction is
//! unexercised in the direction it could be wrong, the same "reduction, not
//! invented" framing `crate::gravity_tick::is_free` gives for its own
//! `isFree` narrowing.
//!
//! # What sources exist today
//!
//! Only **lit redstone torches** (standing and wall) are power *sources* in
//! this landing — the literal scope #314 asks for ("what counts as a power
//! source versus a conductor"). Levers, buttons, daylight sensors, and
//! `minecraft:redstone_block` are not modeled: none of them exist anywhere
//! else in this crate yet (no placement, no item, no block-state constant),
//! so adding source recognition for them now would be exactly the kind of
//! correct-in-isolation code with no producer this repo's own "islands" rule
//! warns against. Repeaters/comparators (#315, when `powered`) and observers
//! (#317, when `powered`) become sources too — see `crate::redstone_diode`/
//! `crate::redstone_observer`.

use crate::neighbor_update::{Direction, ALL_DIRECTIONS};
use lodestone_model::BlockPos;

pub const WIRE: &str = "minecraft:redstone_wire";
pub const TORCH: &str = "minecraft:redstone_torch";
pub const WALL_TORCH: &str = "minecraft:redstone_wall_torch";
pub const REPEATER: &str = "minecraft:repeater";
pub const COMPARATOR: &str = "minecraft:comparator";
pub const OBSERVER: &str = "minecraft:observer";

/// Scheduled-tick `kind` strings (issue #308's `ScheduledTickQueue<T>` is
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

/// A hopper's `ENABLED` block-state property — `true` (transferring) when the
/// hopper is **not** redstone-powered (issue #321).
///
/// Defaults to `true` for a state that does not name it, matching vanilla's
/// `registerDefaultState(... ENABLED, true)` (`HopperBlock.java:55`) and giving
/// a bare `minecraft:hopper` (which is what placement writes today — see #475)
/// the correct unlocked initial value.
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
/// client a hopper pointing somewhere else — the same class of defect as
/// `8f2d912` and #476. Property *order* does not matter, because that resolver
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

/// `DiodeBlock.isDiode` (`DiodeBlock.java:196-198`).
#[must_use]
pub fn is_diode(state: &str) -> bool {
    is_repeater(state) || is_comparator(state)
}
#[must_use]
pub fn is_observer(state: &str) -> bool {
    base_name(state) == OBSERVER
}
/// Any block this crate models signal-carrying behaviour for.
#[must_use]
pub fn is_redstone_component(state: &str) -> bool {
    is_wire(state) || is_torch(state) || is_diode(state) || is_observer(state)
}

/// The reduced conductor predicate — see this module's own doc comment for
/// the full citation and named gap.
#[must_use]
pub fn is_redstone_conductor(state: &str) -> bool {
    !crate::chunk::is_air_or_fluid(state) && !is_redstone_component(state)
}

/// Dust's own `POWER` property (`RedStoneWireBlock.POWER`,
/// `BlockStateProperties.POWER`, `0..=15`). `0` for anything that is not
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
/// direction pointing *into* the block it's attached to), vanilla's
/// `HorizontalDirectionalBlock.FACING` (`RedstoneWallTorchBlock.FACING`).
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
/// `RepeaterBlock.DELAY`, `1..=4` (vanilla default `1`,
/// `RepeaterBlock.java:35`).
#[must_use]
pub fn repeater_delay_ticks(state: &str) -> u32 {
    get_u32_property(state, "delay").unwrap_or(1).clamp(1, 4)
}
#[must_use]
pub fn comparator_mode_subtract(state: &str) -> bool {
    get_str_property(state, "mode") == Some("subtract")
}
/// The comparator's last-computed analog output, `0..=15` — vanilla stores
/// this in a `ComparatorBlockEntity` (`ComparatorBlock.java:67-69`); this
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

/// A diode's own output value while powered — `DiodeBlock.getOutputSignal`
/// defaults to `15` (`DiodeBlock.java:192-194`, unmodified by
/// `RepeaterBlock`); `ComparatorBlock` overrides it to read its stored
/// analog output (`ComparatorBlock.java:67-69`) — see
/// [`comparator_output`]'s own doc comment for where that value lives here.
#[must_use]
fn diode_output_signal(state: &str) -> u8 {
    if is_comparator(state) {
        comparator_output(state)
    } else {
        15
    }
}

/// `BlockState.getOwnSignal`/each block's `ownSignal` override: wire ->
/// `POWER`; torch -> `15` if lit else `0`
/// (`RedstoneTorchBlock.ownSignal`, `:109-111`); diode -> its own output
/// signal if `POWERED` else `0` (`DiodeBlock.ownSignal`, `:142-144`);
/// observer -> `15` if `POWERED` else `0` (`ObserverBlock.ownSignal`,
/// `:100-102`).
#[must_use]
pub fn own_signal(state: &str) -> u8 {
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
    } else {
        0
    }
}

/// `true` for anything `BlockState.isSignalSource()` reports true for among
/// the families this crate models: torches always
/// (`RedstoneTorchBlock.isSignalSource`, `:104-106`, unconditional — a torch
/// is a source whether lit or not, the *value* just happens to be `0`
/// unlit), diodes always (`DiodeBlock.isSignalSource`, `:137-139`), observers
/// always (`ObserverBlock.isSignalSource`, `:95-97`). Dust is deliberately
/// excluded here: `RedStoneWireBlock.isSignalSource` returns its own
/// `shouldSignal` flag, which the general query path always sees as `true`
/// — `getControlInputSignal` special-cases wire before ever reaching this
/// check (`SignalGetter.java:54-55`), so wire never needs this predicate.
#[must_use]
pub fn is_signal_source(state: &str) -> bool {
    is_torch(state) || is_diode(state) || is_observer(state)
}

/// `BlockState.getSignal`'s per-block override — the *weak* signal a `state`
/// contributes toward a querier that reached it by travelling `direction`
/// (i.e. `querier.relative(direction) == the position holding `state``, the
/// same "direction travelled from the querier" convention every function in
/// this module and `crate::neighbor_update::Notification` shares).
///
/// `ignore_wire`: when `true`, a wire's own contribution is forced to `0` —
/// mirrors `RedStoneWireBlock.getBlockSignal` toggling its private
/// `shouldSignal` flag off for the duration of its own
/// `getBestNeighborSignal` call (`RedStoneWireBlock.java:285-290`), so a
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
    } else {
        0
    }
}

/// `BlockState.getDirectSignal`'s per-block override — the *strong* signal
/// `state` sends into a conductor it touches, same direction convention as
/// [`weak_signal`].
///
/// `ignore_wire` — see [`weak_signal`]'s own doc comment for the full
/// citation of vanilla's `shouldSignal` trick, and why it must reach here
/// too, not just the weak-signal path: `RedStoneWireBlock.getDirectSignal`
/// (`:363-365`) is `shouldSignal ? getSignal(...) : 0`, and `shouldSignal` is
/// a field on the **one shared `RedStoneWireBlock` instance** every wire in
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
        // this unmodified (no override in `RedstoneWallTorchBlock.java`).
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
        // DiodeBlock.getDirectSignal (`:147-149`) / ObserverBlock.getDirectSignal
        // (`:105-107`): both delegate straight to `getSignal`.
        weak_signal(state, direction, false)
    } else {
        0
    }
}

/// `SignalGetter.getDirectSignalTo` (`SignalGetter.java:17-46`): the
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

/// `SignalGetter.getControlInputSignal` (`:48-59`) — a repeater/comparator's
/// own side-input read (issue #315). `minecraft:redstone_block` (vanilla's
/// unconditional-`15` branch, `:52-53`) is not modeled: this crate has no
/// such block anywhere (see this module's own doc comment on sources).
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
/// repeaters (`RepeaterBlock.java:88-90`, only another diode's *output* can
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

/// `DiodeBlock.getInputSignal` (`:113-123`), reduced: vanilla additionally
/// reads a two-away block's analog output signal (a hopper/chest's fill
/// level via `BlockState.getAnalogOutputSignal`) and an item frame's
/// rotation when the immediate target is a redstone conductor
/// (`:104-115`) — this crate has no block-entity/analog-output query
/// reachable from this module (see `crate::redstone_diode`'s own doc
/// comment for the full citation of this exact trap, called out by name in
/// issue #315's own brief). What *is* implemented is the base case every
/// circuit not touching a container needs: the direct signal facing into the
/// diode, maxed with an immediately-adjacent wire's own power (vanilla's own
/// belt-and-suspenders read, `:121-122`, since `getSignal` for a wire in a
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
    /// = North (`Direction.getClockWise`, `Direction.java:195-203`). Uses a
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
}
