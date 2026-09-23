//! Redstone-openable blocks: doors, trapdoors and fence gates
//! opening when redstone-powered and closing when unpowered.
//!
//! Every family in this module is a *passive* redstone consumer — it never
//! emits signal, so it is not part of `crate::redstone`'s source/conductor
//! model. What all three share is the same neighbor-changed shape: read
//! whether the block is redstone-powered, and if that differs from the
//! stored `powered` property, write both `open` and `powered` to the new
//! value. Nothing is scheduled — the real engine flips these inline in
//! the neighbor-changed hook (a client-update flag without a
//! neighbor-update flag),
//! unlike the torch/diode/observer delayed-recheck families.
//!
//! # Transcribed from the real openable blocks
//!
//! The real door's neighbor-changed hook, transcribed as the rule it
//! implements: the signal is whether this position has a neighbor signal, or
//! (for a door specifically) whether the *other* half of the door does — the
//! cell above if this is the lower half, or below if this is the upper half.
//! If the changed block is not itself a door, and that signal differs from
//! the stored powered property, play a sound if open/closed is about to
//! change, then write both `powered` and `open` to the new signal value.
//!
//! The real trapdoor's and fence gate's neighbor-changed hooks are the
//! same shape minus the two-high half: the signal is whether this position
//! has a neighbor signal; if that differs from the stored powered property,
//! write both `powered` and `open` to it.
//!
//! The real has-neighbor-signal check is the best-neighbor-signal query
//! being above zero — the [`redstone::best_neighbor_signal`] query
//! already ported in `crate::redstone`.
//!
//! # The door's two halves, and the one deviation
//!
//! A door occupies two cells (`HALF` = `lower`/`upper`), and the real engine
//! checks
//! both halves' neighbours when either one is notified. The halves stay in
//! sync through the real door's own shape-update hook, which
//! copies the *whole* neighbour half's state (`OPEN`, `POWERED`, `FACING`,
//! `HINGE`) over whenever the other half's cell changes shape — reached via
//! the real block-write function's shape-update pass, which runs
//! even for the client-update-only flag. **This crate has no shape-update
//! mechanism at all**
//! (`crate::random_tick`'s reaction dispatch is neighbor-changed-shaped and
//! nothing here implements the shape pass), so the half-sync the real
//! engine performs
//! there is done explicitly here: [`react_to_notification`]'s door arm writes
//! *both* halves when either is notified, and returns no cascade so the other
//! half is not re-notified — the same no-fan-out a client-update-only block
//! write has in
//! the real engine. [`other_door_half_pos`] is the pure piece the wiring uses for
//! that.
//!
//! # Named omissions
//!
//! - **Hand interaction** (the real "use without item" hook) is not modelled: this crate has
//!   no right-click-to-toggle path for these families (placement
//!   `placed_block_state` in `crate::server` only knows the three redstone
//!   directional families), and the redstone half of the issue does not need
//!   it. Iron blocks being power-only (no hand open) is a property of that
//!   omitted path, not of the redstone response — every family here responds
//!   to power identically.
//! - **The door's "changed block is not itself a door" guard** is omitted: its only purpose in
//!   the real engine is to stop a door half reacting to the other half's own
//!   flag-3 block-and-update write, and this crate's door arm writes both
//!   halves itself and returns no cascade, so the trigger it guards against
//!   cannot occur.
//! - **Sound/game-event emission** on open/close is out of scope (the crate
//!   has no such pipeline for block-state changes).
//!
//! # Family recognition
//!
//! The three families are recognized by base-name suffix (`_door`,
//! `_trapdoor`, `_fence_gate`) rather than an enumerated species list — the
//! same convention `crate::growth_tick`'s `is_sapling`/`is_leaves`
//! uses. All vanilla species (oak, iron,
//! acacia, mangrove, bamboo, crimson, …) share the same neighbor-changed shape,
//! so a suffix is the honest classifier and an enumerated list would rot as
//! new wood types land.

use crate::neighbor_update::Direction;
use crate::redstone::{best_neighbor_signal, get_bool_property, get_str_property, with_property};
#[cfg(test)]
use crate::redstone::WorldState;
use lodestone_data::block_properties::{BuiltinPropertyValue, PropertyKey, PropertyValue};
use lodestone_data::block_states::StateId;
use lodestone_model::BlockPos;

/// `true` for any of the three redstone-openable families — see this module's
/// doc comment for why suffix matching, not an enumerated species list.
#[must_use]
pub fn is_openable(state: StateId) -> bool {
    is_door(state) || is_trapdoor(state) || is_fence_gate(state)
}

#[must_use]
pub fn is_door(state: StateId) -> bool {
    let path = base_name(state).path();
    path.ends_with("_door") && !path.ends_with("_trapdoor")
}

#[must_use]
pub fn is_trapdoor(state: StateId) -> bool {
    base_name(state).path().ends_with("_trapdoor")
}

#[must_use]
pub fn is_fence_gate(state: StateId) -> bool {
    base_name(state).path().ends_with("_fence_gate")
}

fn base_name(state: StateId) -> lodestone_data::block::Block {
    state.block()
}

/// The door's `HALF` property (the real double-block-half enum, `lower`/`upper`). Defaults
/// to `lower` for the block's generated default state.
#[must_use]
pub fn door_half(state: StateId) -> BuiltinPropertyValue {
    get_str_property(state, PropertyKey::Half).unwrap_or(BuiltinPropertyValue::Lower)
}

/// The `POWERED` block-state property — the real shared power property.
/// Defaults to `false`, matching each
/// family's default state. This is the property every neighbor-changed hook
/// gates on (`signal != POWERED`); the `OPEN` property needs no standalone
/// reader because all three families write `open` and `powered` to the same
/// value and only ever *read* `powered` to decide — the real engine reads `OPEN`
/// solely for its sound-emission decision, which this crate omits.
#[must_use]
pub fn powered(state: StateId) -> bool {
    get_bool_property(state, PropertyKey::Powered).unwrap_or(false)
}

/// `state` with both `open` and `powered` set to `v`, every other property
/// preserved verbatim (via [`redstone::with_property`]) and appended when
/// absent. Setting both to the same value is the whole contract of all three
/// real neighbor-changed hook bodies.
#[must_use]
pub fn with_open_and_powered(state: StateId, v: bool) -> Option<StateId> {
    let value = PropertyValue::builtin(if v { BuiltinPropertyValue::True } else { BuiltinPropertyValue::False });
    let with_open = with_property(state, PropertyKey::Open, value)?;
    with_property(with_open, PropertyKey::Powered, value)
}

/// The position of the *other* half of a two-high door, or `None` for a
/// non-door (trapdoor/fence gate are single-block) — the same "lower half
/// looks up, upper half looks down" rule
/// from the real door's neighbor-changed hook, transcribed above.
#[must_use]
pub fn other_door_half_pos(pos: BlockPos, state: StateId) -> Option<BlockPos> {
    if !is_door(state) {
        return None;
    }
    let direction = if door_half(state) == BuiltinPropertyValue::Lower { Direction::Up } else { Direction::Down };
    Some(direction.relative(pos))
}

/// The real has-neighbor-signal check as the three
/// real neighbor-changed hook bodies read it: the best neighbour signal is positive.
/// For a door, both halves are checked (`hasNeighborSignal(pos) ||
/// hasNeighborSignal(otherHalf)`) — the "respond to power at either half"
/// property this module models; trapdoor/fence gate check only the one
/// position.
#[must_use]
pub fn has_neighbor_signal<F>(lookup: &F, pos: BlockPos, state: StateId) -> bool
where
    F: crate::redstone::RedstoneLookup + ?Sized,
{
    if best_neighbor_signal(lookup, pos, false) > 0 {
        return true;
    }
    match other_door_half_pos(pos, state) {
        Some(other) => best_neighbor_signal(lookup, other, false) > 0,
        None => false,
    }
}

/// The pure flip decision — the real `signal != POWERED` gate, shared by
/// the real door's, trapdoor's and fence gate's neighbor-changed hooks:
/// when the incoming signal differs from the stored `powered`, return the new
/// state with both `open` and `powered` set to `signal`; otherwise `None`.
///
/// Deliberately **immediate**: unlike the torch/diode/observer families there
/// is no scheduled tick anywhere in these three real neighbor-changed hook bodies, so
/// the caller writes the result right away (the way `crate::random_tick`'s
/// hopper arm already treats an immediate reaction).
#[must_use]
pub fn react(state: StateId, signal: bool) -> Option<StateId> {
    if signal != powered(state) {
        with_open_and_powered(state, signal)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::redstone::configured_state as state;
    use lodestone_data::block::Block;

    fn half_openable(block: Block, half: BuiltinPropertyValue, open: bool, powered: bool) -> StateId {
        state(
            block,
            &[
                (PropertyKey::Half, half),
                (PropertyKey::Open, bool_value(open)),
                (PropertyKey::Powered, bool_value(powered)),
            ],
        )
    }

    fn gate(block: Block, open: bool, powered: bool) -> StateId {
        state(block, &[(PropertyKey::Open, bool_value(open)), (PropertyKey::Powered, bool_value(powered))])
    }

    fn bool_value(value: bool) -> BuiltinPropertyValue {
        if value { BuiltinPropertyValue::True } else { BuiltinPropertyValue::False }
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
    fn is_openable_recognizes_all_three_families_and_rejects_non_openable_blocks() {
        assert!(is_openable(half_openable(Block::OakDoor, BuiltinPropertyValue::Lower, false, false)));
        assert!(is_openable(Block::IronDoor.default_state()));
        assert!(is_openable(half_openable(Block::OakTrapdoor, BuiltinPropertyValue::Bottom, false, false)));
        assert!(is_openable(Block::IronTrapdoor.default_state()));
        assert!(is_openable(gate(Block::OakFenceGate, false, false)));
        assert!(is_openable(Block::CrimsonFenceGate.default_state()));
        assert!(!is_openable(Block::Stone.default_state()));
        assert!(!is_openable(Block::RedstoneWire.default_state()));
        assert!(!is_openable(lodestone_data::block_states::air_state()));
    }

    #[test]
    fn each_family_has_its_own_predicate() {
        assert!(is_door(half_openable(Block::SpruceDoor, BuiltinPropertyValue::Upper, false, false)));
        assert!(!is_door(Block::SpruceTrapdoor.default_state()));
        assert!(is_trapdoor(Block::MangroveTrapdoor.default_state()));
        assert!(!is_trapdoor(Block::MangroveDoor.default_state()));
        assert!(is_fence_gate(Block::AcaciaFenceGate.default_state()));
        assert!(!is_fence_gate(Block::AcaciaDoor.default_state()));
    }

    #[test]
    fn powered_reads_the_property_with_a_sane_default() {
        let closed = half_openable(Block::OakDoor, BuiltinPropertyValue::Lower, false, false);
        assert!(!powered(closed));
        let open_door = half_openable(Block::OakDoor, BuiltinPropertyValue::Upper, true, true);
        assert!(powered(open_door));
        assert!(!powered(Block::OakDoor.default_state()));
        // The `open` property (written alongside `powered`, read nowhere in
        // production) is still surfaced through the shared property helper
        // `with_open_and_powered` produces and `react`'s decision consumes.
        assert_eq!(
            crate::redstone::get_bool_property(open_door, PropertyKey::Open),
            Some(true),
            "the written `open` property must be readable back through the crate's property helper"
        );
    }

    /// `with_open_and_powered` must preserve every other property verbatim —
    /// the same load-bearing property-preservation `redstone::with_property`
    /// exists for (see `crate::redstone`'s own doc comment on it): a rebuilt
    /// state that dropped `facing`/`half`/`hinge` would fall to the subset
    /// tier and hand the client a door pointing somewhere else.
    #[test]
    fn with_open_and_powered_preserves_unrelated_properties() {
        let closed_door = state(
            Block::OakDoor,
            &[
                (PropertyKey::Facing, BuiltinPropertyValue::East),
                (PropertyKey::Half, BuiltinPropertyValue::Lower),
                (PropertyKey::Hinge, BuiltinPropertyValue::Left),
                (PropertyKey::Open, BuiltinPropertyValue::False),
                (PropertyKey::Powered, BuiltinPropertyValue::False),
            ],
        );
        let opened = with_open_and_powered(closed_door, true);
        assert_eq!(
            opened,
            Some(state(
                Block::OakDoor,
                &[
                    (PropertyKey::Facing, BuiltinPropertyValue::East),
                    (PropertyKey::Half, BuiltinPropertyValue::Lower),
                    (PropertyKey::Hinge, BuiltinPropertyValue::Left),
                    (PropertyKey::Open, BuiltinPropertyValue::True),
                    (PropertyKey::Powered, BuiltinPropertyValue::True),
                ],
            ))
        );
        let closed = with_open_and_powered(opened.unwrap(), false);
        assert_eq!(
            closed,
            Some(closed_door)
        );
        assert_eq!(with_open_and_powered(Block::OakDoor.default_state(), true), Some(state(
            Block::OakDoor,
            &[
                (PropertyKey::Half, BuiltinPropertyValue::Lower),
                (PropertyKey::Open, BuiltinPropertyValue::True),
                (PropertyKey::Powered, BuiltinPropertyValue::True),
            ],
        )));
    }

    #[test]
    fn other_door_half_pos_pairs_lower_and_upper() {
        let bottom = half_openable(Block::OakDoor, BuiltinPropertyValue::Lower, false, false);
        assert_eq!(other_door_half_pos(pos(1, 5, 2), bottom), Some(pos(1, 6, 2)));
        let top = half_openable(Block::OakDoor, BuiltinPropertyValue::Upper, false, false);
        assert_eq!(other_door_half_pos(pos(1, 5, 2), top), Some(pos(1, 4, 2)));
        // Single-block families have no other half.
        assert_eq!(other_door_half_pos(pos(1, 5, 2), half_openable(Block::OakTrapdoor, BuiltinPropertyValue::Bottom, false, false)), None);
        assert_eq!(other_door_half_pos(pos(1, 5, 2), gate(Block::OakFenceGate, false, false)), None);
    }

    /// The pure flip decision: exactly the two `signal != powered` branches —
    /// a magnitude-style prediction, both the change and the no-change rows.
    #[test]
    fn react_flips_open_and_powered_exactly_when_signal_differs() {
        let closed = half_openable(Block::OakDoor, BuiltinPropertyValue::Lower, false, false);
        assert_eq!(react(closed, true), with_open_and_powered(closed, true));
        let opened = half_openable(Block::OakDoor, BuiltinPropertyValue::Lower, true, true);
        assert_eq!(react(opened, false), with_open_and_powered(opened, false));
        // Steady states are no-ops, both ways.
        assert_eq!(react(closed, false), None, "unpowered AND unsignaled: steady state");
        assert_eq!(react(opened, true), None, "powered AND signaled: steady state");
    }

    /// A trapdoor reads its own neighbours only (single block) — a lit torch
    /// one step west of it is a real signal.
    #[test]
    fn has_neighbor_signal_finds_an_adjacent_lit_torch_for_a_single_block_family() {
        let origin = pos(3, 5, 3);
        let torch_pos = Direction::West.relative(origin);
        let w = world(&[(torch_pos, crate::redstone_torch::set_standing_lit(true))]);
        let trapdoor_state = half_openable(Block::OakTrapdoor, BuiltinPropertyValue::Bottom, false, false);
        assert!(has_neighbor_signal(&w, origin, trapdoor_state));
        // Negative control: empty world reads no signal.
        assert!(!has_neighbor_signal(&world(&[]), origin, trapdoor_state));
    }

    /// The two-high door power check, end to end: a source adjacent to the
    /// *bottom* half must power the door, and a source adjacent to the *top*
    /// half must power it too — vanilla's `hasNeighborSignal(pos) ||
    /// hasNeighborSignal(otherHalf)`.
    #[test]
    fn a_door_powers_from_a_signal_adjacent_to_either_half() {
        let bottom = pos(3, 5, 3);
        let top = pos(3, 6, 3);
        let bottom_state = half_openable(Block::OakDoor, BuiltinPropertyValue::Lower, false, false);
        let top_state = half_openable(Block::OakDoor, BuiltinPropertyValue::Upper, false, false);
        // Source west of the BOTTOM half.
        let w_bottom = world(&[(Direction::West.relative(bottom), crate::redstone_torch::set_standing_lit(true))]);
        assert!(has_neighbor_signal(&w_bottom, bottom, bottom_state));
        assert!(has_neighbor_signal(&w_bottom, top, top_state), "the top half must read the bottom half's signal");
        // Source west of the TOP half.
        let w_top = world(&[(Direction::West.relative(top), crate::redstone_torch::set_standing_lit(true))]);
        assert!(has_neighbor_signal(&w_top, top, top_state));
        assert!(has_neighbor_signal(&w_top, bottom, bottom_state), "the bottom half must read the top half's signal");
        // Negative control: neither half is powered with no source.
        assert!(!has_neighbor_signal(&world(&[]), bottom, bottom_state));
        assert!(!has_neighbor_signal(&world(&[]), top, top_state));
    }

    /// The `signal != powered` gate must fire on POWERED transitions, not on
    /// every notification — the same discrimination the single-block test
    /// proves for the whole `react` decision, here for the query half.
    #[test]
    fn has_neighbor_signal_distinguishes_powered_from_unpowered_neighbourhoods() {
        let origin = pos(0, 0, 0);
        let w_powered = world(&[(Direction::East.relative(origin), crate::redstone_torch::set_standing_lit(true))]);
        let w_unpowered = world(&[(Direction::East.relative(origin), crate::redstone_torch::set_standing_lit(false))]);
        let gate_state = gate(Block::OakFenceGate, false, false);
        assert!(has_neighbor_signal(&w_powered, origin, gate_state));
        assert!(!has_neighbor_signal(&w_unpowered, origin, gate_state), "an unlit torch is not a signal");
    }
}
