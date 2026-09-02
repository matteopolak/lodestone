//! Tripwire hooks and tripwire string (`minecraft:tripwire_hook` /
//! `minecraft:tripwire`) — the second fixture in the note-block/tripwire-hook/
//! target issue. `crate::redstone` already carries the hook's *read* half
//! (`is_tripwire_hook`, `tripwire_hook_facing`, its `ownSignal`/
//! `getDirectSignal` arms) — this module is the producer that writes the
//! `attached`/`powered` properties those reads consume.
//!
//! # What it is
//!
//! Vanilla's own `TripWireHookBlock.calculateState` —
//! the up-to-41-cell scan along a hook's `FACING` that decides whether it has
//! found a matching hook facing back at it (`attached`), and whether the
//! string between them is armed and reporting powered (`powered`). Both
//! endpoints of a completed run get rewritten together, and every scanned
//! wire segment's own `attached` flag is kept in step with the hook's.
//!
//! **Deliberately not modelled: entity crossing.** `TripWireBlock.entityInside`
//! /`checkPressed` is what actually sets a wire segment's `powered=true` in
//! the common case (something standing on the string) — that needs an
//! entity-AABB census this crate does not have anywhere (the same gap
//! `crate::redstone`'s own module doc names for pressure plates and detector
//! rails). This module only ever reads whatever `powered` a wire segment's
//! state string already carries; nothing here decides it from a live entity.
//! The two triggers this module *does* cover — a hook or a wire segment being
//! **placed**, and (unwired, see below) a wire segment being **broken** — are
//! the "power when the string is broken" half of the issue body, not the
//! "power when an entity crosses it" half.
//!
//! # What this needs of the execution model (for issue #548)
//!
//! * **Trigger**: not a neighbour notification at all — vanilla drives this
//!   from **placement** (`setPlacedBy`/`onPlace`) and from a **periodic
//!   10-tick recheck** the scan itself schedules, never from
//!   `neighborChanged` (`TripWireHookBlock` overrides neither). So
//!   [`calculate_state`]/[`find_controlling_hooks`] are wired into
//!   `react_at_placement`, the existing seam for "the placed block owes
//!   itself a reaction the neighbour pass cannot deliver" (already used for
//!   the hopper `ENABLED` write and fire's first tick) — **not** into
//!   `react_to_notification`.
//! * **Propagation**: two positions can be rewritten from one scan (both
//!   hook endpoints), plus every wire segment between them — a wider blast
//!   radius than any other device in this crate except a piston's multi-cell
//!   move, and for the same reason: [`CalculatedState`] is a full write plan,
//!   not a single new state, mirroring `piston::Resolution`/`apply_move`'s
//!   shape rather than `redstone_diode`'s single-state one.
//! * **Scheduled tick**: yes, but conditionally — vanilla schedules the
//!   10-tick recheck (`RECHECK_PERIOD`) only on the branch where the scan was
//!   *itself* triggered by a specific wire position (`i == wireSource`), not
//!   on every call. [`CalculatedState::reschedule_recheck`] carries that
//!   condition out; a caller ignoring it would recheck a hook forever even
//!   after nothing nearby ever changes again.
//! * **What this crate does not have yet, named precisely rather than
//!   guessed**: the sound/game-event pair `emitState` plays on every
//!   attach/detach/power transition — a client-visible effect with no state
//!   write behind it, the same shape `redstone_note_block`'s pulse names as
//!   an unmodelled gap, and the same `tick.rs::publish_openable_sound`
//!   precedent is the seam to extend for both at once rather than inventing
//!   two different ones.
//!
//! **The block-removal hook now exists.** `crate::server::destroy_block`
//! captures a broken block's state before overwriting the cell (the same
//! `broken` binding every other post-break reaction there already reads) and
//! passes it to `crate::server::propagate_removal_with_entities` →
//! `crate::random_tick::react_at_removal`, which calls [`on_wire_removed`]
//! when the removed block was a tripwire and applies whatever
//! [`calculate_state`] returns for each hook it finds. Breaking a taut
//! tripwire string now fires the instant pulse the module doc above already
//! described as the point of [`on_wire_removed`].

use crate::neighbor_update::Direction;
use crate::redstone::{base_name, direction_to_str, get_bool_property, tripwire_hook_facing, with_property};
use lodestone_model::BlockPos;

pub const TRIPWIRE: &str = "minecraft:tripwire";
pub const TRIPWIRE_HOOK: &str = crate::redstone::TRIPWIRE_HOOK;

/// `TripWireHookBlock.WIRE_DIST_MAX` (`:40`) — the scan runs `1..42`, so the
/// farthest a receiving hook can sit is 41 cells away.
pub const WIRE_DIST_MAX: i32 = 42;

/// `TripWireHookBlock.RECHECK_PERIOD` (`:41`).
pub const RECHECK_DELAY: u32 = 10;

/// `redstone:tripwire_recheck` — the periodic-recheck scheduled-tick kind.
pub const TICK_TRIPWIRE_RECHECK: &str = "redstone:tripwire_recheck";

/// `pos.relative(direction, count)` — vanilla's `BlockPos.relative(Direction,
/// int)`, which [`crate::neighbor_update::Direction`] has no multi-step form
/// of.
fn step(pos: BlockPos, direction: Direction, count: i32) -> BlockPos {
    let mut p = pos;
    for _ in 0..count {
        p = direction.relative(p);
    }
    p
}

/// The one wire cell the scan treats specially — the position a placement or
/// removal is *about*, whose state the caller supplies directly rather than
/// asking `lookup` for it (vanilla's `wireSourceState`, which for a removal is
/// the destroyed block's last known state, not readable from the world
/// anymore).
#[derive(Debug, Clone)]
pub struct WireSource {
    /// Distance from the hook, `1..WIRE_DIST_MAX`.
    pub distance: i32,
    /// The wire state to use at that distance, overriding whatever `lookup`
    /// would answer.
    pub state: String,
}

/// The full write plan one `calculate_state` call produces — see this
/// module's own doc comment for why this is a plan rather than a single new
/// state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CalculatedState {
    /// The hook at the scanned position's own new state, unless the scan was
    /// itself a "this hook is being destroyed" call (vanilla's `!isBeingDestroyed`
    /// guard on the final `setBlock`).
    pub hook_write: Option<(BlockPos, String)>,
    /// The receiving hook's new state, if the scan found one
    /// (`receiver_pos > 0`) — always present together with a facing set to
    /// the *opposite* of the scanning hook's.
    pub receiver_write: Option<(BlockPos, String)>,
    /// Every scanned wire segment between the two hooks whose `attached`
    /// property must flip to match the freshly computed value — vanilla's
    /// `if (wasAttached != attached)` fan-out, already narrowed to "only
    /// cells the scan actually recorded as real wire", matching
    /// `wireStates[i] != null`.
    pub wire_writes: Vec<(BlockPos, String)>,
    /// Vanilla's `level.scheduleTick(pos, block, RECHECK_DELAY)` — `true`
    /// only when this scan was driven by a [`WireSource`] (`i == wireSource`
    /// in the jar).
    pub reschedule_recheck: bool,
    pub attached: bool,
    pub powered: bool,
}

/// Vanilla's own `TripWireHookBlock.calculateState`,
/// minus the sound/game-event pair (`emitState`) and the mid-call
/// self-removal branch — see this module's own doc comment for both.
///
/// `state` is the hook's own current state at `pos` (its `FACING` is read
/// from here, not passed separately, matching the jar reading
/// `state.getOptionalValue(FACING)`). `wire_source`, when present, is the one
/// scanned cell whose state the caller supplies directly rather than asking
/// `lookup` — see [`WireSource`].
#[must_use]
pub fn calculate_state<F>(lookup: &F, pos: BlockPos, state: &str, is_being_destroyed: bool, wire_source: Option<&WireSource>) -> CalculatedState
where
    F: Fn(BlockPos) -> String,
{
    let facing = tripwire_hook_facing(state);
    let was_attached = get_bool_property(state, "attached").unwrap_or(false);
    let mut attached = !is_being_destroyed;
    let mut powered = false;
    let mut receiver_pos: i32 = 0;
    // `(distance, position)` for every cell the scan recorded as a real wire
    // segment (`wireStates[i] != null` in the jar) — the candidate set for the
    // post-loop `attached` fan-out.
    let mut wire_cells: Vec<(i32, BlockPos)> = Vec::new();
    let mut reschedule_recheck = false;

    for i in 1..WIRE_DIST_MAX {
        let test_pos = step(pos, facing, i);
        let is_source = wire_source.is_some_and(|s| s.distance == i);
        let raw_state = if is_source {
            wire_source.unwrap().state.clone()
        } else {
            lookup(test_pos)
        };

        if base_name(&raw_state) == TRIPWIRE_HOOK {
            if tripwire_hook_facing(&raw_state) == facing.opposite() {
                receiver_pos = i;
            }
            break;
        }

        if base_name(&raw_state) != TRIPWIRE && !is_source {
            attached = false;
            continue;
        }

        let wire_armed = !get_bool_property(&raw_state, "disarmed").unwrap_or(false);
        let wire_powered = get_bool_property(&raw_state, "powered").unwrap_or(false);
        powered |= wire_armed && wire_powered;
        wire_cells.push((i, test_pos));
        if is_source {
            reschedule_recheck = true;
            attached &= wire_armed;
        }
    }

    attached &= receiver_pos > 1;
    powered &= attached;

    let self_new = hook_state(facing, attached, powered);
    let receiver_write = (receiver_pos > 0).then(|| {
        let receiver_pos_at = step(pos, facing, receiver_pos);
        (receiver_pos_at, hook_state(facing.opposite(), attached, powered))
    });

    // `if (wasAttached != attached) { for (i = 1; i < receiverPos; i++) ... }`
    // (vanilla's own hook-state calculation) — **both** the outer gate (only when
    // `attached` actually flipped) and the range (`1..receiverPos`, empty
    // whenever no receiver was found at all) are load-bearing; either one
    // missing turns a no-op recheck into a spurious rewrite of every wire
    // segment on the run.
    let mut wire_writes = Vec::new();
    if was_attached != attached {
        for (i, wire_pos) in &wire_cells {
            if *i >= receiver_pos {
                break;
            }
            let current = lookup(*wire_pos);
            if base_name(&current) == TRIPWIRE || base_name(&current) == TRIPWIRE_HOOK {
                wire_writes.push((*wire_pos, with_property(&current, "attached", if attached { "true" } else { "false" })));
            }
        }
    }

    CalculatedState {
        hook_write: (!is_being_destroyed).then(|| (pos, self_new)),
        receiver_write,
        wire_writes,
        reschedule_recheck,
        attached,
        powered,
    }
}

fn hook_state(facing: Direction, attached: bool, powered: bool) -> String {
    format!(
        "minecraft:tripwire_hook[facing={},attached={},powered={}]",
        direction_to_str(facing),
        attached,
        powered
    )
}

/// Vanilla's own `TripWireBlock.updateSource` — from a wire
/// segment at `pos`, scan **south and west only** (vanilla's own fixed pair;
/// the opposite two directions are covered because a hook facing this wire
/// runs its *own* [`calculate_state`] scan toward it) for a hook whose
/// `FACING` points back at `pos`. For every one found, returns the
/// `(hook_pos, WireSource)` pair a caller feeds into [`calculate_state`].
///
/// `wire_state` is the just-placed (or, for [`on_wire_removed`], the
/// synthetically-repowered) wire's own state — vanilla's `state` parameter,
/// threaded through as each found hook's `wireSourceState`.
#[must_use]
pub fn find_controlling_hooks<F>(lookup: &F, pos: BlockPos, wire_state: &str) -> Vec<(BlockPos, WireSource)>
where
    F: Fn(BlockPos) -> String,
{
    let mut found = Vec::new();
    for direction in [Direction::South, Direction::West] {
        for i in 1..WIRE_DIST_MAX {
            let test_pos = step(pos, direction, i);
            let block = lookup(test_pos);
            if base_name(&block) == TRIPWIRE_HOOK {
                if tripwire_hook_facing(&block) == direction.opposite() {
                    found.push((
                        test_pos,
                        WireSource {
                            distance: i,
                            state: wire_state.to_string(),
                        },
                    ));
                }
                break;
            }
            if base_name(&block) != TRIPWIRE {
                break;
            }
        }
    }
    found
}

/// [`find_controlling_hooks`] with `POWERED` forced to `true` on the wire
/// state passed to each found hook — vanilla's own `TripWireBlock
/// ::affectNeighborsAfterRemoval`'s `state.setValue(POWERED, true)`,
/// the "the string just broke" instantaneous
/// pulse. Called from `crate::random_tick::react_at_removal`, which
/// `crate::server::destroy_block` reaches through
/// `crate::server::propagate_removal_with_entities` — the block-removal hook
/// this module's own doc comment named as missing.
#[must_use]
pub fn on_wire_removed<F>(lookup: &F, pos: BlockPos, wire_state_before_removal: &str) -> Vec<(BlockPos, WireSource)>
where
    F: Fn(BlockPos) -> String,
{
    let forced = with_property(wire_state_before_removal, "powered", "true");
    find_controlling_hooks(lookup, pos, &forced)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn world(entries: &[(BlockPos, &str)]) -> impl Fn(BlockPos) -> String + use<> {
        let entries: Vec<(BlockPos, String)> = entries.iter().map(|(p, s)| (*p, (*s).to_string())).collect();
        move |p: BlockPos| {
            entries
                .iter()
                .find(|(pos, _)| *pos == p)
                .map(|(_, s)| s.clone())
                .unwrap_or_else(|| "minecraft:air".to_string())
        }
    }

    fn hook(facing: &str) -> String {
        format!("minecraft:tripwire_hook[facing={facing},attached=false,powered=false]")
    }

    /// Two hooks facing each other with three armed, powered wire segments
    /// between them: `attached` and `powered` both end up `true`, and **both**
    /// hooks get rewritten — the "one scan, two endpoints" claim this module's
    /// doc comment makes, not just the scanning hook.
    #[test]
    fn a_complete_powered_run_attaches_and_powers_both_hooks() {
        let origin = BlockPos::new(0, 64, 0);
        let receiver = BlockPos::new(4, 64, 0);
        let wire = "minecraft:tripwire[attached=false,powered=true,disarmed=false]";
        let w = world(&[
            (BlockPos::new(1, 64, 0), wire),
            (BlockPos::new(2, 64, 0), wire),
            (BlockPos::new(3, 64, 0), wire),
            (receiver, &hook("west")),
        ]);
        let result = calculate_state(&w, origin, &hook("east"), false, None);
        assert!(result.attached, "three unbroken wire cells and a facing hook must attach");
        assert!(result.powered, "every wire cell reports powered=true");
        assert_eq!(
            result.hook_write,
            Some((origin, "minecraft:tripwire_hook[facing=east,attached=true,powered=true]".to_string()))
        );
        assert_eq!(
            result.receiver_write,
            Some((receiver, "minecraft:tripwire_hook[facing=west,attached=true,powered=true]".to_string())),
            "the receiving hook's own FACING is untouched by the scanning hook's FACING"
        );
        assert_eq!(result.wire_writes.len(), 3, "all three wire cells must have their `attached` flag written");
    }

    /// **The discriminating case for the `receiverPos > 1` floor**: two hooks
    /// with nothing between them (adjacent) must NOT attach, even though a
    /// receiving hook was found — a version without that floor would attach
    /// here.
    #[test]
    fn two_adjacent_hooks_with_no_wire_between_them_do_not_attach() {
        let origin = BlockPos::new(0, 64, 0);
        let w = world(&[(BlockPos::new(1, 64, 0), &hook("west"))]);
        let result = calculate_state(&w, origin, &hook("east"), false, None);
        assert!(!result.attached, "receiverPos == 1 must fail the `> 1` floor");
        assert!(!result.powered, "powered is masked by attached regardless of anything else");
    }

    /// A gap in the run (air where a wire segment should be) breaks
    /// `attached` even though a hook is eventually found further out — proves
    /// the scan does not stop at the first gap, it keeps looking for the
    /// receiver but remembers the gap happened.
    #[test]
    fn a_gap_in_the_run_prevents_attachment_even_with_a_hook_beyond_it() {
        let origin = BlockPos::new(0, 64, 0);
        let wire = "minecraft:tripwire[attached=false,powered=false,disarmed=false]";
        let w = world(&[
            (BlockPos::new(1, 64, 0), wire),
            // (2,64,0) left as air: the gap.
            (BlockPos::new(3, 64, 0), wire),
            (BlockPos::new(4, 64, 0), &hook("west")),
        ]);
        let result = calculate_state(&w, origin, &hook("east"), false, None);
        assert!(!result.attached, "the air gap at distance 2 must break attachment");
    }

    /// A `disarmed` (sheared) wire segment cannot ever report `powered`, even
    /// while its own `powered` property is `true` — the conjunction
    /// `wireArmed && wirePowered`, and the discriminating input is a segment
    /// where the two disagree.
    #[test]
    fn a_disarmed_segment_cannot_power_the_run_even_if_marked_powered() {
        let origin = BlockPos::new(0, 64, 0);
        let disarmed_but_marked_powered = "minecraft:tripwire[attached=false,powered=true,disarmed=true]";
        let w = world(&[
            (BlockPos::new(1, 64, 0), disarmed_but_marked_powered),
            (BlockPos::new(2, 64, 0), &hook("west")),
        ]);
        let result = calculate_state(&w, origin, &hook("east"), false, None);
        assert!(result.attached, "the run itself is unbroken (one armed-check aside)");
        assert!(!result.powered, "a disarmed segment must not contribute power");
    }

    /// [`find_controlling_hooks`] finds a hook to the west facing east
    /// (back at the wire) and ignores one to the north — the fixed
    /// south/west scan pair, not all four directions.
    #[test]
    fn find_controlling_hooks_only_scans_south_and_west() {
        let wire_pos = BlockPos::new(5, 64, 5);
        let west_hook = BlockPos::new(2, 64, 5);
        let north_hook = BlockPos::new(5, 64, 2);
        let wire = "minecraft:tripwire[attached=false,powered=false,disarmed=false]";
        let w = world(&[
            (BlockPos::new(3, 64, 5), wire),
            (BlockPos::new(4, 64, 5), wire),
            (west_hook, &hook("east")),
            (BlockPos::new(5, 64, 3), wire),
            (BlockPos::new(5, 64, 4), wire),
            (north_hook, &hook("south")),
        ]);
        let found = find_controlling_hooks(&w, wire_pos, wire);
        let positions: Vec<BlockPos> = found.iter().map(|(p, _)| *p).collect();
        assert!(positions.contains(&west_hook), "a hook to the west facing east must be found");
        assert!(!positions.contains(&north_hook), "north is not one of the two scanned directions");
    }

    /// `on_wire_removed` forces `powered=true` onto the source cell
    /// regardless of the wire's last real value — the "breaking the string
    /// pulses power" behaviour, isolated from the (unwired) trigger that would
    /// call it in production.
    ///
    /// Layout: hook A faces east into the wire cell, which sits one cell
    /// before hook B (facing back west) — `find_controlling_hooks` scans
    /// **west** from the wire to find A (matching its own fixed south/west
    /// pair), and `receiverPos == 2` clears the `> 1` floor so `attached`
    /// (and therefore `powered`) is not masked to `false`.
    #[test]
    fn on_wire_removed_forces_powered_true_on_the_broken_cell() {
        let hook_a = BlockPos::new(0, 64, 0);
        let wire_pos = BlockPos::new(1, 64, 0);
        let hook_b = BlockPos::new(2, 64, 0);
        let unpowered_wire = "minecraft:tripwire[attached=true,powered=false,disarmed=false]";
        let w = world(&[(hook_a, &hook("east")), (hook_b, &hook("west"))]);

        let sources = on_wire_removed(&w, wire_pos, unpowered_wire);
        let (found_hook, source) = sources.into_iter().find(|(p, _)| *p == hook_a).expect("the west-scanning hook must be found");
        assert_eq!(found_hook, hook_a);
        let result = calculate_state(&w, hook_a, &hook("east"), false, Some(&source));
        assert!(result.attached, "a receiver two cells out clears the `receiverPos > 1` floor");
        assert!(result.powered, "the destroyed cell reports powered=true regardless of its stored value");
        assert!(result.reschedule_recheck, "the wire-source branch always schedules the 10-tick recheck");
    }
}
