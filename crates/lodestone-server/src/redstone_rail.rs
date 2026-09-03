//! Powered and activator rails (`minecraft:powered_rail` /
//! `minecraft:activator_rail`) — the "powered rail" half of the rail issue.
//! `minecraft:detector_rail`'s own `POWERED` *read* already landed in
//! `crate::redstone` (`is_detector_rail`'s `ownSignal`/`getDirectSignal`
//! arms); its *producer* remains unbuilt — see this module's own doc comment
//! below for exactly why, and `crate::redstone_target`'s doc comment for the
//! matching shape of gap on a different device.
//!
//! # What it is
//!
//! Both `minecraft:powered_rail` and `minecraft:activator_rail` are the same
//! Java class, `PoweredRailBlock`
//! (`Blocks.ACTIVATOR_RAIL = register(..., PoweredRailBlock::new, ...)`,
//! confirmed against the jar rather than assumed) — one `POWERED` boolean
//! that tracks whether **direct redstone signal touches this rail, or a
//! chain of same-orientation powered rails up to 8 cells away (through
//! boosters) eventually reaches one that does**
//! (vanilla's own `PoweredRailBlock.findPoweredRailSignal`/`isSameRailWithPower`).
//! Activator rail's own *activation* effect
//! — `AbstractMinecart.activateMinecart` (a plain minecart ejects its rider,
//! a TNT minecart primes) — is now modelled, in
//! `crate::mobs::minecart::apply_activation`; this module's own `POWERED`
//! tracking is what feeds it the rail's activation state each tick. This
//! module remains the `POWERED` tracking only.
//!
//! Neither block is a redstone **signal source** — `PoweredRailBlock`
//! overrides no `ownSignal`, so it never appears in
//! `crate::redstone::is_signal_source`. It only *consumes* signal to decide
//! its own `POWERED`, the same shape a door or fence gate has in
//! `crate::redstone_openable`.
//!
//! # What is deliberately not modelled: connectivity (`SHAPE`)
//!
//! `BaseRailBlock`'s own generic curve/straight connection algorithm
//! (`updateDir`/`RailState.place`) decides
//! which of the (for `PoweredRailBlock`, six non-curved) `SHAPE` values a rail
//! settles into from its neighbours — a placement/shape-pipeline concern
//! shared with the plain, non-redstone `minecraft:rail`, and out of this
//! module's scope. [`update_state`] below reads whatever `SHAPE` a rail
//! already carries (even a naively-placed default) and answers only
//! `POWERED` from it — exactly the boundary `crate::redstone.rs`'s own module
//! doc draws between "the read is right, the producer is separate" for six of
//! its nine input families.
//!
//! # What this needs of the execution model (for issue #548)
//!
//! * **Trigger**: a neighbour notification, reacting through the same
//!   `PoweredRailBlock.updateState` override `BaseRailBlock.neighborChanged`
//!   calls — wired into `react_to_notification`
//!   exactly like the hopper `ENABLED`/note-block `POWERED` arms, no
//!   scheduled tick.
//! * **Propagation is *not* a plain six-direction fan-out.** Vanilla calls
//!   `updateNeighborsAt(pos.below(), this)` unconditionally on a `POWERED`
//!   flip, and *additionally* `updateNeighborsAt(pos.above(), this)` only
//!   when the rail's own `SHAPE` is a slope (vanilla's own powered-rail block).
//!   [`NeighborFanOut`] carries exactly that pair so a caller does not
//!   over-notify a flat rail's cell above it.
//! * **Scheduled tick**: none — this is a same-tick decision, like the note
//!   block.
//! * **Reads state another device can change the same tick**: yes, and this
//!   is the sharpest requirement in the family — [`find_powered_rail_signal`]
//!   recurses up to 8 cells through **other rails of the same block type**,
//!   reading each one's own current `POWERED`. A dependency-graph rework
//!   (issue #548) has to treat a chain of powered rails as one connected unit
//!   for invalidation purposes: flipping the direct-signal source at one end
//!   can flip every rail in the chain, and re-deriving that from a single
//!   position's own six neighbours (the shape every other device in this
//!   family needs) is not enough here — the read genuinely walks up to 8
//!   cells outward in each of two directions before it is done.
//! * **Ordering**: none of `UPDATE_ORDER`'s quirks apply beyond the ordinary
//!   fan-out; the recursive rail walk has no cross-device interaction to get
//!   wrong the way (for example) a diode's lock/unlock ordering does.

use crate::neighbor_update::Direction;
use crate::redstone::{base_name, get_bool_property, get_str_property, with_property, WorldState};
use lodestone_model::BlockPos;

pub const POWERED_RAIL: &str = "minecraft:powered_rail";
pub const ACTIVATOR_RAIL: &str = "minecraft:activator_rail";

/// `PoweredRailBlock.findPoweredRailSignal`'s own recursion cap
/// (`searchDepth >= 8`).
pub const MAX_SEARCH_DEPTH: i32 = 8;

/// `RailShape`, narrowed to the six values `PoweredRailBlock.SHAPE`
/// (`BlockStateProperties.RAIL_SHAPE_STRAIGHT`) actually allows — a powered
/// or activator rail can never curve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RailShape {
    NorthSouth,
    EastWest,
    AscendingNorth,
    AscendingSouth,
    AscendingEast,
    AscendingWest,
}

impl RailShape {
    #[must_use]
    pub fn from_str(s: &str) -> Option<Self> {
        Some(match s {
            "north_south" => RailShape::NorthSouth,
            "east_west" => RailShape::EastWest,
            "ascending_north" => RailShape::AscendingNorth,
            "ascending_south" => RailShape::AscendingSouth,
            "ascending_east" => RailShape::AscendingEast,
            "ascending_west" => RailShape::AscendingWest,
            _ => return None,
        })
    }

    /// `RailShape.isSlope()` for the six straight-only values.
    #[must_use]
    pub fn is_slope(self) -> bool {
        matches!(
            self,
            RailShape::AscendingNorth | RailShape::AscendingSouth | RailShape::AscendingEast | RailShape::AscendingWest
        )
    }
}

fn rail_shape(state: &str) -> Option<RailShape> {
    get_str_property(state, "shape").and_then(RailShape::from_str)
}

/// Public wrapper over [`rail_shape`] — the `SHAPE` a caller needs after
/// [`update_state`] to compute [`extra_notifications`], since a `POWERED`
/// flip never changes `SHAPE`.
#[must_use]
pub fn shape_of(state: &str) -> Option<RailShape> {
    rail_shape(state)
}

fn rail_powered(state: &str) -> bool {
    get_bool_property(state, "powered").unwrap_or(false)
}

/// `true` for either block this module covers — the one class,
/// `PoweredRailBlock`, registered twice (see this module's own doc comment).
#[must_use]
pub fn is_powered_rail_family(state: &str) -> bool {
    matches!(base_name(state), POWERED_RAIL | ACTIVATOR_RAIL)
}

/// The type/orientation half of `PoweredRailBlock.isSameRailWithPower`
/// (`:105-125`) — is the rail at `pos` the *same* block as the one searching,
/// oriented so it can actually pass power along (excludes a perpendicular
/// straight rail; a slope in the perpendicular axis is fine, matching the
/// jar's own `dir` comparison), and currently `POWERED`? [`is_same_rail_at`]
/// is the full port — this is the half with no recursion in it, split out so
/// each half has one job.
fn is_candidate_rail(state: &str, family: &str, dir: RailShape) -> bool {
    if base_name(state) != family {
        return false;
    }
    let Some(my_shape) = rail_shape(state) else { return false };

    // `dir != EAST_WEST || myShape not in {NORTH_SOUTH, ASCENDING_NORTH,
    // ASCENDING_SOUTH}` — i.e. reject when the search direction is
    // perpendicular to this rail's own orientation. Named as the rejection
    // condition, not the acceptance one, because that is the shape the jar's
    // own double-negative reads most faithfully as — inverting this comparison
    // is the exact mistake that made a same-direction chain look perpendicular
    // to itself (§ this module's own oracle gate, discovered from a straight
    // run failing to propagate at all).
    let perpendicular_mismatch = match dir {
        RailShape::EastWest => matches!(my_shape, RailShape::NorthSouth | RailShape::AscendingNorth | RailShape::AscendingSouth),
        RailShape::NorthSouth => matches!(my_shape, RailShape::EastWest | RailShape::AscendingEast | RailShape::AscendingWest),
        _ => false,
    };
    !perpendicular_mismatch && rail_powered(state)
}

/// `PoweredRailBlock.findPoweredRailSignal` (`:30-103`) — walks up to
/// [`MAX_SEARCH_DEPTH`] cells `forward` (or backward) along `shape`,
/// following ascending slopes up or down a Y level as vanilla's own `switch`
/// does, and asks whether the rail found there is itself powered (directly,
/// via `has_neighbor_signal`, or recursively via its own further search).
///
/// `has_neighbor_signal` is vanilla's `level.hasNeighborSignal(pos)` at each
/// visited rail — supplied by the caller (typically
/// `crate::redstone::best_neighbor_signal(lookup, pos, false) > 0`) rather
/// than recomputed here, keeping this module's only dependency on the query
/// layer at the call site.
#[must_use]
pub fn find_powered_rail_signal<F, S>(lookup: &F, has_neighbor_signal: &S, family: &str, pos: BlockPos, shape: RailShape, forward: bool, search_depth: i32) -> bool
where
    F: Fn(BlockPos) -> WorldState,
    S: Fn(BlockPos) -> bool,
{
    if search_depth >= MAX_SEARCH_DEPTH {
        return false;
    }

    let BlockPos { mut x, mut y, mut z } = pos;
    let mut check_below = true;
    let mut effective_shape = shape;

    match shape {
        RailShape::NorthSouth => {
            if forward {
                z += 1;
            } else {
                z -= 1;
            }
        }
        RailShape::EastWest => {
            if forward {
                x -= 1;
            } else {
                x += 1;
            }
        }
        RailShape::AscendingEast => {
            if forward {
                x -= 1;
            } else {
                x += 1;
                y += 1;
                check_below = false;
            }
            effective_shape = RailShape::EastWest;
        }
        RailShape::AscendingWest => {
            if forward {
                x -= 1;
                y += 1;
                check_below = false;
            } else {
                x += 1;
            }
            effective_shape = RailShape::EastWest;
        }
        RailShape::AscendingNorth => {
            if forward {
                z += 1;
            } else {
                z -= 1;
                y += 1;
                check_below = false;
            }
            effective_shape = RailShape::NorthSouth;
        }
        RailShape::AscendingSouth => {
            if forward {
                z += 1;
                y += 1;
                check_below = false;
            } else {
                z -= 1;
            }
            effective_shape = RailShape::NorthSouth;
        }
    }

    let candidate = BlockPos::new(x, y, z);
    if is_same_rail_at(lookup, has_neighbor_signal, family, candidate, forward, search_depth, effective_shape) {
        return true;
    }
    check_below
        && is_same_rail_at(
            lookup,
            has_neighbor_signal,
            family,
            BlockPos::new(x, y - 1, z),
            forward,
            search_depth,
            effective_shape,
        )
}

/// `PoweredRailBlock.isSameRailWithPower` (`:105-125`), in full: is this a
/// candidate rail ([`is_candidate_rail`]), and if so does it carry power —
/// either a direct neighbour signal of its own, or, failing that, its own
/// further [`find_powered_rail_signal`] recursion one level deeper?
fn is_same_rail_at<F, S>(lookup: &F, has_neighbor_signal: &S, family: &str, pos: BlockPos, forward: bool, search_depth: i32, dir: RailShape) -> bool
where
    F: Fn(BlockPos) -> WorldState,
    S: Fn(BlockPos) -> bool,
{
    let state = lookup(pos);
    if !is_candidate_rail(&state, family, dir) {
        return false;
    }
    if has_neighbor_signal(pos) {
        return true;
    }
    let Some(shape) = rail_shape(&state) else { return false };
    find_powered_rail_signal(lookup, has_neighbor_signal, family, pos, shape, forward, search_depth + 1)
}

/// `pos.below()`, plus `pos.above()` only when `shape.is_slope()` —
/// `PoweredRailBlock.updateState`'s own extra `updateNeighborsAt` calls
/// (`:134-138`), returned as [`Notification`]s ready to feed back into
/// `NeighborPropagator`'s cascade, the same shape a piston move's own fan-out
/// already returns.
#[must_use]
pub fn extra_notifications(pos: BlockPos, shape: RailShape) -> Vec<crate::neighbor_update::Notification> {
    let mut out = vec![crate::neighbor_update::Notification {
        pos: Direction::Down.relative(pos),
        from: Direction::Up,
    }];
    if shape.is_slope() {
        out.push(crate::neighbor_update::Notification {
            pos: Direction::Up.relative(pos),
            from: Direction::Down,
        });
    }
    out
}

/// `PoweredRailBlock.updateState` (`:127-140`) — `None` when nothing changes
/// (vanilla's own `if (shouldPower != isPowered)` guard).
#[must_use]
pub fn update_state<F, S>(lookup: &F, has_neighbor_signal: &S, pos: BlockPos, state: &str) -> Option<String>
where
    F: Fn(BlockPos) -> WorldState,
    S: Fn(BlockPos) -> bool,
{
    let family = base_name(state);
    if !is_powered_rail_family(state) {
        return None;
    }
    let Some(shape) = rail_shape(state) else { return None };
    let is_powered = rail_powered(state);
    let should_power = has_neighbor_signal(pos)
        || find_powered_rail_signal(lookup, has_neighbor_signal, family, pos, shape, true, 0)
        || find_powered_rail_signal(lookup, has_neighbor_signal, family, pos, shape, false, 0);
    if should_power == is_powered {
        return None;
    }
    Some(with_property(state, "powered", if should_power { "true" } else { "false" }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world(entries: &[(BlockPos, &str)]) -> impl Fn(BlockPos) -> WorldState + use<> {
        let entries: Vec<(BlockPos, WorldState)> = entries.iter().map(|(p, s)| (*p, WorldState::from(*s))).collect();
        move |p: BlockPos| {
            entries
                .iter()
                .find(|(pos, _)| *pos == p)
                .map(|(_, s)| s.clone())
                .unwrap_or_else(crate::chunk::air_state_arc)
        }
    }

    fn rail(shape: &str, powered: bool) -> String {
        format!("minecraft:powered_rail[shape={shape},powered={powered}]")
    }

    /// Direct signal alone (no chain at all) powers the rail — the base case
    /// `hasNeighborSignal(pos)` covers on its own.
    #[test]
    fn direct_signal_powers_a_rail_with_no_neighbours() {
        let pos = BlockPos::new(0, 64, 0);
        let w = world(&[(pos, &rail("north_south", false))]);
        let has_signal = |p: BlockPos| p == pos;
        let out = update_state(&w, &has_signal, pos, &rail("north_south", false));
        assert_eq!(out, Some(rail("north_south", true)));
    }

    /// **The chain case, two hops out.** `isSameRailWithPower` only ever
    /// extends the search through a cell that is *itself* already
    /// `POWERED` (vanilla's `if (!state.getValue(POWERED)) return false`,
    /// [`is_candidate_rail`]'s own gate) — being marked powered is what makes
    /// a rail a valid relay candidate, and the search then asks whether
    /// *that* rail has a direct signal of its own or must recurse again. So
    /// both B and C carry `powered=true` here (as if each had already
    /// converged from its own prior `update_state`), and **only C** carries a
    /// genuine external signal — the search must walk through B (powered but
    /// with no direct signal of its own) to reach C's signal two cells out.
    #[test]
    fn a_powered_rail_two_hops_out_powers_the_whole_straight_run() {
        let a = BlockPos::new(0, 64, 0);
        let b = BlockPos::new(0, 64, 1);
        let c = BlockPos::new(0, 64, 2);
        let w = world(&[
            (a, &rail("north_south", false)),
            (b, &rail("north_south", true)),
            (c, &rail("north_south", true)),
        ]);
        let has_signal = |p: BlockPos| p == c;
        let out = update_state(&w, &has_signal, a, &rail("north_south", false));
        assert_eq!(out, Some(rail("north_south", true)), "A must inherit power by walking through B to C's signal");
    }

    /// **The gating half of the case above, isolated.** If B is *not* already
    /// `powered=true` in the world, the search must not walk through it at
    /// all — even though C, two cells out, has a real direct signal. This is
    /// the discriminating input for [`is_candidate_rail`]'s own `POWERED`
    /// gate: dropping it would make this fixture (byte-identical to the one
    /// above except for B's own `powered` property) pass anyway.
    #[test]
    fn an_unpowered_intermediate_rail_blocks_the_chain_even_with_a_real_signal_beyond_it() {
        let a = BlockPos::new(0, 64, 0);
        let b = BlockPos::new(0, 64, 1);
        let c = BlockPos::new(0, 64, 2);
        let w = world(&[
            (a, &rail("north_south", false)),
            (b, &rail("north_south", false)),
            (c, &rail("north_south", true)),
        ]);
        let has_signal = |p: BlockPos| p == c;
        let out = update_state(&w, &has_signal, a, &rail("north_south", false));
        assert_eq!(out, None, "B is not itself powered, so the search must not reach past it to C");
    }

    /// **The 8-cell cap, the discriminating case.** A chain of nine already-
    /// powered rails must NOT reach the far end — `search_depth >= 8` cuts it
    /// off one short of nine, so the ninth rail's `has_neighbor_signal` (true)
    /// must not reach the origin.
    #[test]
    fn the_search_does_not_reach_past_eight_cells() {
        let origin = BlockPos::new(0, 64, 0);
        let mut entries = Vec::new();
        for i in 0..=9 {
            entries.push((BlockPos::new(0, 64, i), rail("north_south", true)));
        }
        let entries_ref: Vec<(BlockPos, &str)> = entries.iter().map(|(p, s)| (*p, s.as_str())).collect();
        let w = world(&entries_ref);
        let far_end = BlockPos::new(0, 64, 9);
        let has_signal = move |p: BlockPos| p == far_end;
        let out = update_state(&w, &has_signal, origin, &rail("north_south", false));
        assert_eq!(out, None, "a signal 9 cells away must not power the origin through the cap");
    }

    /// A perpendicular straight rail must not relay power across a junction —
    /// `isSameRailWithPower`'s own axis check, the discriminating input being
    /// an east/west rail sitting where a north/south search is looking.
    #[test]
    fn a_perpendicular_rail_does_not_relay_power() {
        let a = BlockPos::new(0, 64, 0);
        let b = BlockPos::new(0, 64, 1);
        let w = world(&[(a, &rail("north_south", false)), (b, &rail("east_west", true))]);
        let has_signal = |p: BlockPos| p == b;
        let out = update_state(&w, &has_signal, a, &rail("north_south", false));
        assert_eq!(out, None, "an east/west rail must not carry a north/south search's power");
    }

    /// No-op guard: a rail already at the value the search would compute
    /// changes nothing — proves `update_state` is not "always write".
    #[test]
    fn no_change_when_already_at_the_computed_value() {
        let pos = BlockPos::new(0, 64, 0);
        let w = world(&[(pos, &rail("north_south", true))]);
        let has_signal = |p: BlockPos| p == pos;
        assert_eq!(update_state(&w, &has_signal, pos, &rail("north_south", true)), None);
    }

    /// [`extra_notifications`]: flat rail notifies only below; a slope also
    /// notifies above — the conjunction this module's own doc comment
    /// singles out.
    #[test]
    fn extra_notifications_include_above_only_for_a_slope() {
        let pos = BlockPos::new(0, 64, 0);
        let flat = extra_notifications(pos, RailShape::NorthSouth);
        assert_eq!(flat.len(), 1, "a flat rail notifies only below");
        assert_eq!(flat[0].pos, Direction::Down.relative(pos));

        let slope = extra_notifications(pos, RailShape::AscendingNorth);
        assert_eq!(slope.len(), 2, "a slope also notifies above");
        assert!(slope.iter().any(|n| n.pos == Direction::Up.relative(pos)));
    }

    /// `is_powered_rail_family` covers both registrations of the one class —
    /// the fact this module's own doc comment cites as verified against the
    /// jar rather than assumed.
    #[test]
    fn both_powered_and_activator_rail_are_the_same_family() {
        assert!(is_powered_rail_family("minecraft:powered_rail[shape=north_south,powered=false]"));
        assert!(is_powered_rail_family("minecraft:activator_rail[shape=north_south,powered=false]"));
        assert!(!is_powered_rail_family("minecraft:rail[shape=north_south]"));
        assert!(!is_powered_rail_family("minecraft:detector_rail[shape=north_south,powered=false]"));
    }
}
