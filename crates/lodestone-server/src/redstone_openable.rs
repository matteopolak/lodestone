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
use lodestone_model::BlockPos;

/// `true` for any of the three redstone-openable families — see this module's
/// doc comment for why suffix matching, not an enumerated species list.
#[must_use]
pub fn is_openable(state: &str) -> bool {
    is_door(state) || is_trapdoor(state) || is_fence_gate(state)
}

#[must_use]
pub fn is_door(state: &str) -> bool {
    base_name(state).ends_with("_door")
}

#[must_use]
pub fn is_trapdoor(state: &str) -> bool {
    base_name(state).ends_with("_trapdoor")
}

#[must_use]
pub fn is_fence_gate(state: &str) -> bool {
    base_name(state).ends_with("_fence_gate")
}

/// Strips a `[...]` block-state property suffix — the same local convention
/// every other per-family module in this crate duplicates rather than sharing
/// (see `crate::redstone`'s own `base_name` doc comment).
fn base_name(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

/// The door's `HALF` property (the real double-block-half enum, `lower`/`upper`). Defaults
/// to `"lower"` for a state that does not name it — a bare
/// `minecraft:oak_door` (the only form worldgen/placement could write today,
/// see the module doc's placement note) is a bottom half by that default,
/// matching the real door block's own constructor registering its default
/// state with the lower half.
#[must_use]
pub fn door_half(state: &str) -> &str {
    get_str_property(state, "half").unwrap_or("lower")
}

/// The `POWERED` block-state property — the real shared power property.
/// Defaults to `false`, matching each
/// family's default state. This is the property every neighbor-changed hook
/// gates on (`signal != POWERED`); the `OPEN` property needs no standalone
/// reader because all three families write `open` and `powered` to the same
/// value and only ever *read* `powered` to decide — the real engine reads `OPEN`
/// solely for its sound-emission decision, which this crate omits.
#[must_use]
pub fn powered(state: &str) -> bool {
    get_bool_property(state, "powered").unwrap_or(false)
}

/// `state` with both `open` and `powered` set to `v`, every other property
/// preserved verbatim (via [`redstone::with_property`]) and appended when
/// absent. Setting both to the same value is the whole contract of all three
/// real neighbor-changed hook bodies.
#[must_use]
pub fn with_open_and_powered(state: &str, v: bool) -> String {
    let value = if v { "true" } else { "false" };
    let with_open = with_property(state, "open", value);
    with_property(&with_open, "powered", value)
}

/// The position of the *other* half of a two-high door, or `None` for a
/// non-door (trapdoor/fence gate are single-block) — the same "lower half
/// looks up, upper half looks down" rule
/// from the real door's neighbor-changed hook, transcribed above.
#[must_use]
pub fn other_door_half_pos(pos: BlockPos, state: &str) -> Option<BlockPos> {
    if !is_door(state) {
        return None;
    }
    let direction = if door_half(state) == "lower" { Direction::Up } else { Direction::Down };
    Some(direction.relative(pos))
}

/// The real has-neighbor-signal check as the three
/// real neighbor-changed hook bodies read it: the best neighbour signal is positive.
/// For a door, both halves are checked (`hasNeighborSignal(pos) ||
/// hasNeighborSignal(otherHalf)`) — the "respond to power at either half"
/// property this module models; trapdoor/fence gate check only the one
/// position.
#[must_use]
pub fn has_neighbor_signal<F>(lookup: &F, pos: BlockPos, state: &str) -> bool
where
    F: Fn(BlockPos) -> String,
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
pub fn react(state: &str, signal: bool) -> Option<String> {
    if signal != powered(state) {
        Some(with_open_and_powered(state, signal))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A tiny fake world: an explicit map from position to block-state
    /// string, air everywhere unset — the same "pure decision, fake world
    /// via closure" style every redstone module in this crate uses.
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

    /// Both a bare name and a full property set must classify — the crate's
    /// own state strings carry only the properties it models (see
    /// `v770::resolve_state_id`'s tier-2 fallback), so a door is recognised
    /// however its state string was written.
    #[test]
    fn is_openable_recognizes_all_three_families_and_rejects_non_openable_blocks() {
        assert!(is_openable("minecraft:oak_door[half=lower,open=false,powered=false]"));
        assert!(is_openable("minecraft:iron_door"));
        assert!(is_openable("minecraft:oak_trapdoor[half=bottom,open=false,powered=false,waterlogged=false]"));
        assert!(is_openable("minecraft:iron_trapdoor"));
        assert!(is_openable("minecraft:oak_fence_gate[open=false,powered=false,in_wall=false]"));
        assert!(is_openable("minecraft:crimson_fence_gate"));
        assert!(!is_openable("minecraft:stone"));
        assert!(!is_openable("minecraft:redstone_wire[power=15]"));
        assert!(!is_openable("minecraft:air"));
        assert!(!is_openable("minecraft:oak_doorway"), "a `_door`-suffixed lookalike must not classify");
    }

    #[test]
    fn each_family_has_its_own_predicate() {
        assert!(is_door("minecraft:spruce_door[half=upper]"));
        assert!(!is_door("minecraft:spruce_trapdoor"));
        assert!(is_trapdoor("minecraft:mangrove_trapdoor"));
        assert!(!is_trapdoor("minecraft:mangrove_door"));
        assert!(is_fence_gate("minecraft:acacia_fence_gate"));
        assert!(!is_fence_gate("minecraft:acacia_door"));
    }

    #[test]
    fn powered_reads_the_property_with_a_sane_default() {
        let closed = "minecraft:oak_door[half=lower,open=false,powered=false]";
        assert!(!powered(closed));
        let open_door = "minecraft:oak_door[half=upper,open=true,powered=true]";
        assert!(powered(open_door));
        // A state that does not name the property reads its default.
        assert!(!powered("minecraft:oak_door"));
        // The `open` property (written alongside `powered`, read nowhere in
        // production) is still surfaced through the shared property helper
        // `with_open_and_powered` produces and `react`'s decision consumes.
        assert_eq!(
            crate::redstone::get_bool_property(&open_door, "open"),
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
        let door = "minecraft:oak_door[facing=east,half=lower,hinge=left,open=false,powered=false]";
        let opened = with_open_and_powered(door, true);
        assert_eq!(
            opened,
            "minecraft:oak_door[facing=east,half=lower,hinge=left,open=true,powered=true]"
        );
        let closed = with_open_and_powered(&opened, false);
        assert_eq!(
            closed,
            "minecraft:oak_door[facing=east,half=lower,hinge=left,open=false,powered=false]"
        );
        // Bare name: both properties appended.
        assert_eq!(with_open_and_powered("minecraft:oak_door", true), "minecraft:oak_door[open=true,powered=true]");
    }

    #[test]
    fn other_door_half_pos_pairs_lower_and_upper() {
        let bottom = "minecraft:oak_door[half=lower,open=false,powered=false]";
        assert_eq!(other_door_half_pos(pos(1, 5, 2), bottom), Some(pos(1, 6, 2)));
        let top = "minecraft:oak_door[half=upper,open=false,powered=false]";
        assert_eq!(other_door_half_pos(pos(1, 5, 2), top), Some(pos(1, 4, 2)));
        // Single-block families have no other half.
        assert_eq!(other_door_half_pos(pos(1, 5, 2), "minecraft:oak_trapdoor[open=false,powered=false]"), None);
        assert_eq!(other_door_half_pos(pos(1, 5, 2), "minecraft:oak_fence_gate[open=false,powered=false]"), None);
    }

    /// The pure flip decision: exactly the two `signal != powered` branches —
    /// a magnitude-style prediction, both the change and the no-change rows.
    #[test]
    fn react_flips_open_and_powered_exactly_when_signal_differs() {
        let closed = "minecraft:oak_door[half=lower,open=false,powered=false]";
        assert_eq!(react(closed, true), Some(with_open_and_powered(closed, true)));
        let opened = "minecraft:oak_door[half=lower,open=true,powered=true]";
        assert_eq!(react(&opened, false), Some(with_open_and_powered(&opened, false)));
        // Steady states are no-ops, both ways.
        assert_eq!(react(closed, false), None, "unpowered AND unsignaled: steady state");
        assert_eq!(react(&opened, true), None, "powered AND signaled: steady state");
    }

    /// A trapdoor reads its own neighbours only (single block) — a lit torch
    /// one step west of it is a real signal.
    #[test]
    fn has_neighbor_signal_finds_an_adjacent_lit_torch_for_a_single_block_family() {
        let origin = pos(3, 5, 3);
        let torch_pos = Direction::West.relative(origin);
        let w = world(&[(torch_pos, "minecraft:redstone_torch[lit=true]")]);
        let state = "minecraft:oak_trapdoor[half=bottom,open=false,powered=false]";
        assert!(has_neighbor_signal(&w, origin, state));
        // Negative control: empty world reads no signal.
        assert!(!has_neighbor_signal(&world(&[]), origin, state));
    }

    /// The two-high door power check, end to end: a source adjacent to the
    /// *bottom* half must power the door, and a source adjacent to the *top*
    /// half must power it too — vanilla's `hasNeighborSignal(pos) ||
    /// hasNeighborSignal(otherHalf)`.
    #[test]
    fn a_door_powers_from_a_signal_adjacent_to_either_half() {
        let bottom = pos(3, 5, 3);
        let top = pos(3, 6, 3);
        let bottom_state = "minecraft:oak_door[half=lower,open=false,powered=false]";
        let top_state = "minecraft:oak_door[half=upper,open=false,powered=false]";
        // Source west of the BOTTOM half.
        let w_bottom = world(&[(Direction::West.relative(bottom), "minecraft:redstone_torch[lit=true]")]);
        assert!(has_neighbor_signal(&w_bottom, bottom, bottom_state));
        assert!(has_neighbor_signal(&w_bottom, top, top_state), "the top half must read the bottom half's signal");
        // Source west of the TOP half.
        let w_top = world(&[(Direction::West.relative(top), "minecraft:redstone_torch[lit=true]")]);
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
        let w_powered = world(&[(Direction::East.relative(origin), "minecraft:redstone_torch[lit=true]")]);
        let w_unpowered = world(&[(Direction::East.relative(origin), "minecraft:redstone_torch[lit=false]")]);
        let state = "minecraft:oak_fence_gate[open=false,powered=false]";
        assert!(has_neighbor_signal(&w_powered, origin, state));
        assert!(!has_neighbor_signal(&w_unpowered, origin, state), "an unlit torch is not a signal");
    }
}
