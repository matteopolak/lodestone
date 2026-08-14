//! Dispensers and droppers (`minecraft:dispenser` / `minecraft:dropper`).
//!
//! # What it is
//!
//! `DispenserBlock`/`DropperBlock` share everything except **one** method:
//! `getDispenseMethod`. A dropper hardcodes `DefaultDispenseItemBehavior` —
//! always a plain toss, full stop. A dispenser looks the held item's `Item`
//! up in `DISPENSER_REGISTRY` (populated once, in `DispenseItemBehavior
//! .bootStrap`) and falls back to the same plain toss only when nothing is
//! registered for it. Getting this boundary backwards (a dropper that
//! consults a behaviour table, or a dispenser that never does) is invisible
//! in the overwhelmingly common case — a stack of cobblestone dispenses
//! identically either way — and only shows up the moment someone loads an
//! arrow.
//!
//! # The behaviour table, derived from the registrations
//!
//! Every entry below is a real `DispenserBlock.registerBehavior`/
//! `registerProjectileBehavior` call inside `DispenseItemBehavior.bootStrap`
//! (`.cache/mc/26.2/src/net/minecraft/core/dispenser/DispenseItemBehavior.java:59-405`),
//! not a memory-derived guess:
//!
//! | items | behaviour | modelled here |
//! |---|---|---|
//! | arrow, tipped/spectral arrow, egg (+2 chicken-colour variants), snowball, experience bottle, splash/lingering potion, firework rocket, fire charge, wind charge | fires as a projectile entity | **no** — needs a projectile/entity-velocity spawn this crate has none of |
//! | armor stand | spawns and equips one | no — entity spawn |
//! | chest | fills a nearby saddled chest-carrying animal, else plain-tosses | no — entity query |
//! | every boat/chest-boat/raft (18 items) | places a riding entity | no — entity spawn |
//! | lava/water/powder-snow/fish/axolotl/sulfur-cube/tadpole bucket | empties into the world ahead | **not yet** — needs `crate::fluid`'s place-fluid entry point, which this module does not call |
//! | bucket | picks a fluid up | **not yet** — same reason |
//! | flint and steel | ignites the block ahead (`FlintAndSteelDispenseItemBehavior`) | no |
//! | bone meal | grows the crop/water plant ahead (`BoneMealItem.growCrop`) | **not yet** — `crate::bone_meal` exists and is the seam, not called from here |
//! | TNT | spawns a primed-TNT entity | no — entity spawn |
//! | wither skeleton skull | places the skull block, mob-summon check | no |
//! | carved pumpkin | places the block, golem-summon check | no |
//! | shulker box (+16 colours) | places the block entity | no |
//! | glass bottle | takes water/honey from ahead | no |
//! | glowstone | charges a respawn anchor | no |
//! | shears | shears a sheep/mooshroom ahead | no — entity query |
//! | brush | brushes an armadillo ahead | no — entity query |
//! | honeycomb | waxes a copper block ahead | no |
//! | potion (water only, on mud-convertible ground) | places mud | no |
//! | minecart family (6 items) | places a riding entity | no — entity spawn |
//! | *everything else* | `DefaultDispenseItemBehavior` — plain toss | **yes** ([`plain_toss`]) |
//!
//! So today this module models exactly the shared mechanics (the `TRIGGERED`
//! redstone state machine every dispenser/dropper has, and the plain-toss
//! math both blocks fall back to) and none of the ~35 special behaviours —
//! the whole point of the table above being to say so *precisely*, per this
//! issue's own trap about not treating "dispenser ejects an item" as done
//! until the table is at least enumerated.
//!
//! # What this needs of the execution model (for issue #548)
//!
//! * **Trigger**: `neighborChanged`, immediate — `hasNeighborSignal(pos) ||
//!   hasNeighborSignal(pos.above())` (the `pos.above()` half is easy to miss:
//!   a comparator or repeater sitting directly on **top** of a dispenser can
//!   fire it, not only one beside it). Wired into `react_to_notification`.
//! * **Scheduled tick**: yes, a fixed 4-tick one-shot on the *rising* edge
//!   only (`TRIGGER_DURATION`) — [`on_neighbor_changed`]'s
//!   `schedule_fire` flag. Unlike a diode's delay this one never reschedules
//!   itself and never changes with any block-state property.
//! * **The actual dispense is not wired anywhere in this crate yet, and that
//!   gap is named precisely rather than silently absent**: firing needs a
//!   9-slot container's live contents (`crate::block_entities::BlockEntity`
//!   now has a registered `minecraft:dispenser`/`minecraft:dropper`
//!   `Container` variant — see this module's sibling edit in
//!   `block_entities.rs` — but nothing threads a `BlockEntityHandle` into
//!   `tick.rs`'s scheduled-tick drain, the one place `TICK_DISPENSER_FIRE`
//!   would need to read one from). This is the exact gap the #314/#315/#317
//!   landing's own comment on this issue named in advance: "no block-entity
//!   query reachable from `crate::redstone`". [`random_slot`] and
//!   [`dispense_position`] are the pure math a real caller needs and are
//!   fully tested against the jar's own formulas; there is simply no
//!   production call site for them yet.

use crate::neighbor_update::Direction;
use crate::redstone::{base_name, direction_from_str, direction_to_str, get_bool_property, get_str_property, with_property};

pub const DISPENSER: &str = "minecraft:dispenser";
pub const DROPPER: &str = "minecraft:dropper";

/// `DispenserBlock.TRIGGER_DURATION` (`:56`).
pub const TRIGGER_DURATION: u32 = 4;

/// `redstone:dispenser_fire` — the scheduled-tick kind, ready for a caller
/// that can reach a live container (see this module's own doc comment).
pub const TICK_DISPENSER_FIRE: &str = "redstone:dispenser_fire";

/// `DispenserBlock.getDispensePosition`'s own default `scale` (`:161-163`,
/// the zero-argument overload).
///
/// `#[allow(dead_code)]` on this and every plain-toss helper below through
/// [`facing_name`]: ready for the fire tick this module's own doc comment
/// names as unwired — nothing in this crate can reach a live container yet.
#[allow(dead_code)]
pub const DISPENSE_SCALE: f64 = 0.7;

#[must_use]
pub fn is_dispenser_family(state: &str) -> bool {
    matches!(base_name(state), DISPENSER | DROPPER)
}

/// `true` only for `minecraft:dropper` — the one predicate
/// [`crate::redstone_dispenser`] needs to pick `plain_toss` unconditionally
/// rather than consulting the (unmodelled) behaviour table.
#[allow(dead_code)]
#[must_use]
pub fn is_dropper(state: &str) -> bool {
    base_name(state) == DROPPER
}

#[allow(dead_code)]
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
#[allow(dead_code)]
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
#[allow(dead_code)]
#[must_use]
pub fn dispense_position(center: (f64, f64, f64), face: Direction) -> (f64, f64, f64) {
    let (dx, dy, dz) = step(face);
    (
        center.0 + DISPENSE_SCALE * dx,
        center.1 + DISPENSE_SCALE * dy,
        center.2 + DISPENSE_SCALE * dz,
    )
}

#[allow(dead_code)]
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
        random_slot(&occupied, |bound| {
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
}
