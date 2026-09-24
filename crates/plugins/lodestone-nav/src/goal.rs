//! Goals (`docs/baritone-port.md` §4.7).
//!
//! Two contract lines, both load-bearing:
//!
//! * [`Goal::heuristic`] **must be admissible** — never over-estimate the true
//!   remaining cost — or the search silently returns bad paths;
//! * [`Goal::satisfied`] **must imply `heuristic == 0`**, or the search steps past
//!   its own goal.
//!
//! Both are `debug_assert`ed inside the search, which is nearly free and catches
//! every user-written goal bug at the point of use.
//!
//! # Deviation from the design, recorded
//!
//! `docs/baritone-port.md` §4.7 spells the signature `heuristic(&self, x, y, z) ->
//! Ticks`, with the per-block rate baked into each goal. It is passed in as [`Rates`]
//! here instead, because the admissible rate is an *output of the template table*
//! (`cheapest_ticks_per_block`, deflated 1.5%) and a goal that carried its own copy
//! would be a second place for it to be wrong. Goals stay rate-free; the search owns
//! the rate.

use crate::ticks::Ticks;

/// The admissible per-block rates a heuristic scales distance by, derived from the
/// simulated template table rather than written down.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Rates {
    /// Ticks per block of horizontal travel, deflated for strict admissibility.
    pub per_block: f64,
    /// Ticks per block of *upward* travel. Descent contributes zero: falling is free
    /// (and often faster than walking), so charging for it would over-estimate.
    pub per_block_up: f64,
}

impl Rates {
    /// Scale a block distance into ticks.
    #[must_use]
    pub fn cost(&self, blocks: f64) -> Ticks {
        Ticks::from_f64(blocks * self.per_block)
    }
}

/// What the search is trying to reach.
pub trait Goal: std::fmt::Debug + Send + Sync {
    /// An **admissible** lower bound on the cost from `(x, y, z)` to satisfaction.
    fn heuristic(&self, x: i32, y: i32, z: i32, rates: &Rates) -> Ticks;

    /// Whether a feet cell satisfies the goal. Must imply `heuristic == 0`.
    fn satisfied(&self, x: i32, y: i32, z: i32) -> bool;

    /// A short description, for the status surface and the log.
    fn describe(&self) -> String {
        format!("{self:?}")
    }
}

/// Octile distance in blocks: the cheapest 8-connected path length.
///
/// Under-estimates a 4-connected graph (M1 has no diagonals), which keeps the
/// heuristic admissible with slack to spare, and is exact once `WalkDiagonal` lands
/// in M2.
#[must_use]
pub fn octile(dx: i32, dz: i32) -> f64 {
    let (a, b) = (dx.abs().min(dz.abs()), dx.abs().max(dz.abs()));
    f64::from(b - a) + f64::from(a) * std::f64::consts::SQRT_2
}

/// Stand in one specific cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtBlock {
    /// Target feet cell `x`.
    pub x: i32,
    /// Target feet cell `y`.
    pub y: i32,
    /// Target feet cell `z`.
    pub z: i32,
}

impl Goal for AtBlock {
    fn heuristic(&self, x: i32, y: i32, z: i32, rates: &Rates) -> Ticks {
        let horizontal = rates.cost(octile(self.x - x, self.z - z));
        let up = Ticks::from_f64(f64::from((self.y - y).max(0)) * rates.per_block_up);
        horizontal.saturating_add(up)
    }

    fn satisfied(&self, x: i32, y: i32, z: i32) -> bool {
        (x, y, z) == (self.x, self.y, self.z)
    }

    fn describe(&self) -> String {
        format!("at block {} {} {}", self.x, self.y, self.z)
    }
}

/// Stand anywhere in one column, at any height.
///
/// The **zero vertical term** is what makes long-distance travel not care about
/// terrain height it cannot know yet — and it is what a positional goal in an
/// unloaded region should be rewritten to (`docs/baritone-port.md` §4.7's goal
/// simplification at range), because effort spent optimising an unknowable `y` is
/// wasted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AtColumn {
    /// Target column `x`.
    pub x: i32,
    /// Target column `z`.
    pub z: i32,
}

impl Goal for AtColumn {
    fn heuristic(&self, x: i32, _y: i32, z: i32, rates: &Rates) -> Ticks {
        rates.cost(octile(self.x - x, self.z - z))
    }

    fn satisfied(&self, x: i32, _y: i32, z: i32) -> bool {
        (x, z) == (self.x, self.z)
    }

    fn describe(&self) -> String {
        format!("at column {} {}", self.x, self.z)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rates() -> Rates {
        Rates {
            per_block: 3.5,
            per_block_up: 12.0,
        }
    }

    /// The contract the search `debug_assert`s. Checked here too, so a goal added
    /// later has an obvious place to be checked.
    #[test]
    fn satisfied_implies_a_zero_heuristic() {
        let r = rates();
        let block = AtBlock { x: 4, y: 64, z: -7 };
        assert!(block.satisfied(4, 64, -7));
        assert_eq!(block.heuristic(4, 64, -7, &r), Ticks::ZERO);

        let column = AtColumn { x: 4, z: -7 };
        assert!(column.satisfied(4, 999, -7));
        assert_eq!(column.heuristic(4, 999, -7, &r), Ticks::ZERO);
    }

    /// Descent contributes nothing. Charging for it would over-estimate, because
    /// falling really is free.
    #[test]
    fn descent_is_free_in_the_heuristic() {
        let r = rates();
        let goal = AtBlock { x: 0, y: 0, z: 0 };
        assert_eq!(goal.heuristic(0, 100, 0, &r), Ticks::ZERO);
        assert!(goal.heuristic(0, -100, 0, &r) > Ticks::ZERO);
    }

    /// Under-estimating a 4-connected graph is what keeps the heuristic admissible.
    #[test]
    fn octile_under_estimates_manhattan() {
        assert!(octile(3, 4) < 7.0);
        assert_eq!(octile(0, 5), 5.0);
        assert!((octile(5, 5) - 5.0 * std::f64::consts::SQRT_2).abs() < 1e-12);
    }

    #[test]
    fn a_column_goal_ignores_height_entirely() {
        let r = rates();
        let goal = AtColumn { x: 10, z: 0 };
        assert_eq!(
            goal.heuristic(0, 0, 0, &r),
            goal.heuristic(0, 300, 0, &r),
            "a column goal that cared about y would refuse to path over hills"
        );
    }
}
