use std::time::Duration;

use super::{MILLIS_PER_TICK, TICK_PERIOD};

const MAX_DEBT: Duration = Duration::from_secs(2);
const RECOVERY_TICKS: u8 = 2;
const RECOVERY_BUDGET: Duration = Duration::from_millis(8);

pub(super) struct TickSchedule {
    next: Duration,
    recovery_ticks: u8,
    recovery_started: Option<Duration>,
}

impl TickSchedule {
    pub(super) fn new() -> Self {
        Self { next: TICK_PERIOD, recovery_ticks: 0, recovery_started: None }
    }

    pub(super) fn deadline(&self) -> Duration {
        self.next
    }

    pub(super) fn reset(&mut self, now: Duration) {
        self.next = now + TICK_PERIOD;
        self.yielded();
    }

    pub(super) fn needs_yield(&mut self, now: Duration) -> bool {
        if now < self.next {
            self.yielded();
            return false;
        }
        self.recovery_ticks >= RECOVERY_TICKS
            || self.recovery_started.is_some_and(|start| {
                now.saturating_sub(start) >= RECOVERY_BUDGET
            })
    }

    pub(super) fn yielded(&mut self) {
        self.recovery_ticks = 0;
        self.recovery_started = None;
    }

    pub(super) fn shed_debt(&mut self, now: Duration) -> u64 {
        let behind = now.saturating_sub(self.next);
        if behind <= MAX_DEBT {
            return 0;
        }
        let ticks = u64::try_from(behind.as_millis() / u128::from(MILLIS_PER_TICK))
            .expect("tick debt fits the host clock");
        self.next += Duration::from_millis(ticks * MILLIS_PER_TICK);
        self.yielded();
        ticks
    }

    pub(super) fn admit(&mut self, resumed: Duration, paused: bool) {
        if paused {
            self.reset(resumed);
            return;
        }
        if resumed > self.next {
            self.recovery_ticks = self.recovery_ticks.saturating_add(1);
            self.recovery_started.get_or_insert(resumed);
        } else {
            self.yielded();
        }
        self.next += TICK_PERIOD;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn replay(first_service_ms: u64, work_ms: u64) -> (Vec<u64>, usize) {
        let mut schedule = TickSchedule::new();
        let mut now = Duration::ZERO;
        let mut starts = Vec::new();
        let mut yields = 0;
        while schedule.deadline() <= Duration::from_millis(1005) {
            if schedule.needs_yield(now) {
                yields += 1;
                now += Duration::from_millis(1);
                schedule.yielded();
            }
            assert_eq!(schedule.shed_debt(now), 0);
            now = now.max(schedule.deadline());
            if starts.is_empty() {
                now = now.max(Duration::from_millis(first_service_ms));
            }
            if now > Duration::from_millis(1005) {
                break;
            }
            starts.push(now.as_millis() as u64);
            schedule.admit(now, false);
            now += Duration::from_millis(work_ms);
        }
        (starts, yields)
    }

    #[test]
    fn late_service_keeps_twenty_anchored_ticks_instead_of_eighteen_delayed_ticks() {
        let (starts, _) = replay(130, 2);
        assert_eq!(&starts[..4], &[130, 132, 150, 200]);
        assert_eq!(starts.len(), 20);
        assert_eq!(starts.last(), Some(&1000));
        let delayed = (130..=1005).step_by(50).collect::<Vec<_>>();
        assert_eq!(delayed.len(), 18);
        assert_ne!(starts, delayed);
        let (on_time, yields) = replay(50, 2);
        assert_eq!(on_time, (50..=1000).step_by(50).collect::<Vec<_>>());
        assert_eq!(yields, 0);
    }

    #[test]
    fn recovery_yields_after_two_ticks_or_eight_milliseconds() {
        let mut schedule = TickSchedule::new();
        schedule.admit(Duration::from_millis(250), false);
        assert!(!schedule.needs_yield(Duration::from_millis(252)));
        schedule.admit(Duration::from_millis(252), false);
        assert!(schedule.needs_yield(Duration::from_millis(254)));
        schedule.yielded();
        assert!(!schedule.needs_yield(Duration::from_millis(255)));
        schedule.admit(Duration::from_millis(255), false);
        assert!(schedule.needs_yield(Duration::from_millis(263)));
        let (slow, yields) = replay(50, 60);
        assert!(slow.len() < 20);
        assert!(yields > 0);
    }

    #[test]
    fn excessive_debt_sheds_whole_ticks_without_changing_phase() {
        let mut schedule = TickSchedule::new();
        assert_eq!(schedule.shed_debt(Duration::from_millis(2050)), 0);
        assert_eq!(schedule.shed_debt(Duration::from_millis(2051)), 40);
        assert_eq!(schedule.deadline(), Duration::from_millis(2050));
        schedule.admit(Duration::from_millis(2051), false);
        assert_eq!(schedule.shed_debt(Duration::from_millis(4101)), 40);
        assert_eq!(schedule.deadline(), Duration::from_millis(4100));
    }

    #[test]
    fn paused_time_cannot_be_replayed_after_reset() {
        let mut schedule = TickSchedule::new();
        schedule.admit(Duration::from_millis(250), false);
        schedule.reset(Duration::from_secs(60));
        assert_eq!(schedule.deadline(), Duration::from_millis(60050));
        assert_eq!(schedule.shed_debt(Duration::from_secs(60)), 0);
        assert!(!schedule.needs_yield(Duration::from_secs(60)));
        schedule.admit(Duration::from_millis(63000), true);
        assert_eq!(schedule.deadline(), Duration::from_millis(63050));
        assert_eq!(schedule.shed_debt(Duration::from_millis(63000)), 0);
        assert!(!schedule.needs_yield(Duration::from_millis(63000)));
    }
}
