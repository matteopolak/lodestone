//! The pufferfish puff state machine.
//!
//! # What it is
//!
//! The 0 (small), 1 (mid), 2 (full) puff state a pufferfish shows, driven by
//! whether its puff goal is running.
//!
//! # How it works
//!
//! While inflating, the first tick goes to state 1 and the counter passing 40
//! goes to state 2. After the goal stops, a deflate timer passing 60 drops
//! state 2 to 1, and passing 100 drops state 1 to 0. A puffed fish stings a
//! player within 0.3 blocks of its box for `1 + state` damage and
//! `60 * state` ticks of poison.
//!
//! # How to change it
//!
//! `PuffState::tick` is the machine; `SimMob` owns one per pufferfish and
//! `MobSim::tick` calls it and emits the stings.

/// Ticks of inflating after which a mid puff becomes a full one.
const FULL_AFTER: i32 = 40;
/// Ticks of deflating after which a full puff drops to mid.
const DROP_FULL_AFTER: i32 = 60;
/// Ticks of deflating after which a mid puff drops to small.
const DROP_MID_AFTER: i32 = 100;

/// A pufferfish's puff.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PuffState {
    /// 0 small, 1 mid, 2 full.
    pub state: u8,
    inflate: i32,
    deflate: i32,
    was_inflating: bool,
}

impl PuffState {
    /// Advances one tick; `inflating` is whether the puff goal is running.
    pub fn tick(&mut self, inflating: bool) {
        if inflating && !self.was_inflating {
            self.inflate = 1;
            self.deflate = 0;
        } else if !inflating && self.was_inflating {
            self.inflate = 0;
        }
        self.was_inflating = inflating;
        if self.inflate > 0 {
            if self.state == 0 {
                self.state = 1;
            } else if self.inflate > FULL_AFTER && self.state == 1 {
                self.state = 2;
            }
            self.inflate += 1;
        } else if self.state != 0 {
            if self.deflate > DROP_FULL_AFTER && self.state == 2 {
                self.state = 1;
            } else if self.deflate > DROP_MID_AFTER && self.state == 1 {
                self.state = 0;
            }
            self.deflate += 1;
        }
    }

    /// The damage a sting deals: `1 + state`.
    #[must_use]
    pub fn sting_damage(self) -> f32 {
        1.0 + f32::from(self.state)
    }

    /// The poison a sting applies, in ticks: `60 * state`.
    #[must_use]
    pub fn sting_poison_ticks(self) -> i32 {
        60 * i32::from(self.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The state after each of the first ticks of one inflation, then of the
    /// deflation after it, counted by hand from the rule in the module docs.
    #[test]
    fn inflates_in_two_steps_and_deflates_in_two() {
        let mut puff = PuffState::default();
        puff.tick(true);
        assert_eq!(puff.state, 1, "first inflating tick");
        // Tick k tests a counter equal to k, so it exceeds 40 on tick 41.
        for _ in 0..39 {
            puff.tick(true);
        }
        assert_eq!(puff.state, 1, "40 ticks in");
        puff.tick(true);
        assert_eq!(puff.state, 2, "41st tick");
        // Stopping: deflate tick k tests a timer of k - 1, which passes 60 on
        // tick 62.
        let mut ticks = 0;
        while puff.state == 2 {
            puff.tick(false);
            ticks += 1;
        }
        assert_eq!(ticks, 62);
        let mut more = 0;
        while puff.state == 1 {
            puff.tick(false);
            more += 1;
        }
        // The timer reads 62 after that tick and must read 101: 40 more ticks.
        assert_eq!(more, 40);
        assert_eq!(puff.state, 0);
    }

    #[test]
    fn stings_scale_with_the_puff() {
        let mut puff = PuffState::default();
        puff.tick(true);
        assert_eq!((puff.sting_damage(), puff.sting_poison_ticks()), (2.0, 60));
        puff.state = 2;
        assert_eq!((puff.sting_damage(), puff.sting_poison_ticks()), (3.0, 120));
    }
}
