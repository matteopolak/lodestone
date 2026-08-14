//! Target blocks (`minecraft:target`) — the third fixture grouped into the
//! note-block/tripwire-hook/target issue.
//!
//! # What it is
//!
//! `crate::redstone::own_signal`/`is_signal_source` already read a target's
//! analog `power` property (landed alongside tripwire hook and detector rail —
//! see that module's own doc comment). This module is the producer: vanilla's
//! `TargetBlock.getRedstoneStrength` (a projectile hit's distance from centre
//! turned into `1..=15`) and `TargetBlock.tick` (the decay back to `0`).
//!
//! # What this needs of the execution model (for issue #548's incremental
//! rework)
//!
//! * **Trigger**: an external event this crate does not produce yet — a
//!   projectile hitting the block (`TargetBlock.onProjectileHit`). Nothing
//!   here schedules anything on its own; [`apply_hit`] is a pure function
//!   waiting for a caller. The seam is intentionally the same shape as every
//!   other device in this family (`redstone_diode::run_scheduled_tick`,
//!   `redstone_torch::run_scheduled_tick`): a pure decision, called from one
//!   named place, not spread across the dispatch.
//! * **Propagation**: none beyond the ordinary neighbour fan-out an analog
//!   power *change* already gets for free through `propagate_and_react` — a
//!   target has no special-cased neighbours the way a piston or tripwire hook
//!   does.
//! * **Scheduled tick**: yes, one-shot, `TickPriority::Normal`, at a duration
//!   that depends on *what* hit it (20 ticks for an arrow, 8 for anything
//!   else — [`activation_duration`]) — mirroring vanilla's own
//!   `ACTIVATION_TICKS_ARROWS`/`ACTIVATION_TICKS_OTHER`. A second hit while a
//!   decay is already pending does not reschedule or change the displayed
//!   power (`updateRedstoneOutput`'s `hasScheduledTick` guard, threaded
//!   through [`apply_hit`]'s `has_pending_decay` parameter) — the engine must
//!   expose "is a tick of this kind already queued at this position" for that
//!   guard to be checkable at all, which is exactly what
//!   `ScheduledTickQueue::has_scheduled` already answers for every other
//!   family here.
//! * **Reads state another device can change the same tick**: no — a target's
//!   own `power` property is written only by [`apply_hit`] and
//!   [`run_scheduled_tick`], both keyed to this block's own position.
//! * **Ordering**: none of vanilla's `UPDATE_ORDER` quirks apply; a target's
//!   `power` write is a plain `setBlock` with no directional fan-out beyond
//!   the standard one.

use crate::redstone::{analog_power, base_name, with_property};

pub const TARGET: &str = "minecraft:target";

/// `redstone:target` — the decay-to-zero scheduled tick's kind string, for
/// `ScheduledTickQueue<String>`.
pub const TICK_TARGET_DECAY: &str = "redstone:target_decay";

/// `Direction.Axis` narrowed to the three values [`redstone_strength`] needs —
/// which axis the hit **face** lies on, not which way the projectile was
/// travelling.
///
/// `#[allow(dead_code)]` on this and everything below through [`apply_hit`]:
/// ready for a projectile-hit producer this crate does not have yet — see
/// this module's own doc comment.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HitAxis {
    X,
    Y,
    Z,
}

/// `TargetBlock.getRedstoneStrength` (`TargetBlock.java:61-77`), given the
/// axis of the hit face and the hit point's position within the cell
/// (`Mth.frac(hitLocation.{x,y,z})`, i.e. already reduced to `[0, 1)`).
///
/// The two fractional coordinates **not** on the hit axis measure how far off
/// centre the hit landed (`0.0` = dead centre of that face, `0.5` = the
/// face's edge); the result is `ceil(15 * clamp((0.5 - distance) / 0.5, 0, 1))`,
/// floored at `1` so a hit that never returns `0` — an arrow that grazes the
/// very edge of the target still lights it, if only to level `1`.
#[allow(dead_code)]
#[must_use]
pub fn redstone_strength(hit_axis: HitAxis, frac_x: f64, frac_y: f64, frac_z: f64) -> u8 {
    let dist_x = (frac_x - 0.5).abs();
    let dist_y = (frac_y - 0.5).abs();
    let dist_z = (frac_z - 0.5).abs();
    let distance = match hit_axis {
        HitAxis::Y => dist_x.max(dist_z),
        HitAxis::Z => dist_x.max(dist_y),
        HitAxis::X => dist_y.max(dist_z),
    };
    let scaled = 15.0 * ((0.5 - distance) / 0.5).clamp(0.0, 1.0);
    (scaled.ceil() as i64).max(1).min(15) as u8
}

/// `TargetBlock.ACTIVATION_TICKS_ARROWS` (20) / `ACTIVATION_TICKS_OTHER` (8) —
/// how long [`apply_hit`]'s written power stays up before
/// [`run_scheduled_tick`] decays it back to `0`.
#[allow(dead_code)]
#[must_use]
pub fn activation_duration(is_arrow: bool) -> u32 {
    if is_arrow {
        20
    } else {
        8
    }
}

/// What a projectile hit resolved to — the state to write and the decay tick
/// to schedule after it, or `None` when vanilla's own guard suppresses the
/// write entirely (a decay is already pending).
#[allow(dead_code)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HitOutcome {
    pub new_state: String,
    pub delay: u32,
}

/// `TargetBlock.updateRedstoneOutput` + `setOutputPower`
/// (`TargetBlock.java:51-59,79-82`) — `state` must already be a
/// `minecraft:target` (checked by [`crate::redstone::is_target`] upstream;
/// this function trusts its caller the same way every other `run_*`/`apply_*`
/// function in this family does).
///
/// `has_pending_decay` is vanilla's `level.getBlockTicks().hasScheduledTick(pos,
/// state.getBlock())` — the caller answers it from
/// `ScheduledTickQueue::has_scheduled(pos, TICK_TARGET_DECAY)`. When true, the
/// hit still counts for advancement/stat purposes (vanilla returns
/// `redstoneStrength` from `updateRedstoneOutput` either way) but the block's
/// own `power` is left untouched and no new decay is scheduled — this
/// function returns `None` in that case since a caller only wanted to know
/// "how to change the world", not the advancement side-channel.
#[allow(dead_code)]
#[must_use]
pub fn apply_hit(state: &str, strength: u8, is_arrow: bool, has_pending_decay: bool) -> Option<HitOutcome> {
    if base_name(state) != TARGET || has_pending_decay {
        return None;
    }
    Some(HitOutcome {
        new_state: with_property(state, "power", &strength.to_string()),
        delay: activation_duration(is_arrow),
    })
}

/// `TargetBlock.tick` (`TargetBlock.java:85-89`) — decay `power` to `0` if it
/// is not already. `None` when there is nothing to change (mirrors every
/// other `run_scheduled_tick` in this family returning `None` for "no
/// mutation").
#[must_use]
pub fn run_scheduled_tick(state: &str) -> Option<String> {
    if base_name(state) != TARGET || analog_power(state) == 0 {
        return None;
    }
    Some(with_property(state, "power", "0"))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dead-centre hit on every face axis is distance `0`, so the formula's
    /// own `ceil(15 * 1.0)` gives the maximum, `15` — not a guessed round
    /// number, the arithmetic's actual output at the discriminating input.
    #[test]
    fn a_dead_centre_hit_reads_fifteen_on_every_axis() {
        assert_eq!(redstone_strength(HitAxis::Y, 0.5, 0.5, 0.5), 15);
        assert_eq!(redstone_strength(HitAxis::Z, 0.5, 0.5, 0.5), 15);
        assert_eq!(redstone_strength(HitAxis::X, 0.5, 0.5, 0.5), 15);
    }

    /// A hit at the very edge of the face (`distance` at its max of `0.5`)
    /// clamps the scaled term to `0.0`, and the `max(1, …)` floor is what
    /// keeps that a `1` rather than a `0` — the discriminating case for that
    /// floor, since a naive port without it would read `0` here.
    #[test]
    fn an_edge_hit_floors_at_one_rather_than_zero() {
        assert_eq!(redstone_strength(HitAxis::Y, 0.0, 0.5, 0.5), 1);
        assert_eq!(redstone_strength(HitAxis::Y, 0.5, 0.5, 1.0), 1);
    }

    /// The axis selection: a hit on the Y-axis face (top/bottom) reads X/Z,
    /// so moving the hit off-centre along Y must **not** move the reading —
    /// the control that the wrong pair of axes was not swapped in.
    #[test]
    fn the_hit_axis_selects_which_two_coordinates_matter() {
        let centred_xz = redstone_strength(HitAxis::Y, 0.5, 0.9, 0.5);
        assert_eq!(centred_xz, 15, "Y is off-centre but Y-axis hits ignore Y entirely");

        let off_centre_x = redstone_strength(HitAxis::Y, 0.9, 0.5, 0.5);
        assert_ne!(off_centre_x, 15, "X is one of the two axes a Y-axis hit reads");
    }

    /// A quarter-of-the-way-off-centre hit is the value worth pinning exactly,
    /// derived from the formula rather than guessed: `distance = 0.25`, so
    /// `scaled = 15 * (0.25/0.5) = 7.5`, and `ceil(7.5) = 8`.
    #[test]
    fn a_quarter_offset_hit_derives_to_eight_not_a_round_number() {
        assert_eq!(redstone_strength(HitAxis::Y, 0.75, 0.5, 0.5), 8);
    }

    #[test]
    fn arrows_stay_lit_five_times_longer_than_anything_else() {
        assert_eq!(activation_duration(true), 20);
        assert_eq!(activation_duration(false), 8);
    }

    #[test]
    fn apply_hit_writes_power_and_schedules_the_matching_duration() {
        let outcome = apply_hit("minecraft:target[power=0]", 12, false, false)
            .expect("no pending decay, so the hit must apply");
        assert_eq!(outcome.new_state, "minecraft:target[power=12]");
        assert_eq!(outcome.delay, 8);

        let arrow_outcome = apply_hit("minecraft:target[power=0]", 12, true, false)
            .expect("no pending decay, so the hit must apply");
        assert_eq!(arrow_outcome.delay, 20);
    }

    /// Vanilla's own guard: a hit landing while a decay is already scheduled
    /// changes nothing about the block. Without this arm a rapid-fire target
    /// farm would flicker its displayed power on every arrow instead of
    /// holding vanilla's steady value until the first decay actually fires.
    #[test]
    fn a_hit_during_a_pending_decay_changes_nothing() {
        assert_eq!(apply_hit("minecraft:target[power=9]", 3, false, true), None);
    }

    #[test]
    fn scheduled_tick_decays_a_lit_target_and_leaves_an_unlit_one_alone() {
        assert_eq!(
            run_scheduled_tick("minecraft:target[power=7]"),
            Some("minecraft:target[power=0]".to_string())
        );
        assert_eq!(run_scheduled_tick("minecraft:target[power=0]"), None);
    }
}
