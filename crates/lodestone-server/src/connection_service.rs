use std::future::Future;
use std::sync::Mutex;
use std::time::Duration;

use crate::worldgen_progress::{PhaseTimer, WorldgenTimingPhase};

const SERVICE_BUDGET: Duration = Duration::from_millis(8);
const PASS_BUDGET: u32 = 64;

pub(crate) fn pin_future<F: Future>(create: impl FnOnce() -> F) -> std::pin::Pin<Box<F>> {
    Box::pin(create())
}

#[derive(Default)]
struct ServiceWindow {
    active: Option<Duration>,
    occupied: Duration,
    passes: u32,
}

impl ServiceWindow {
    fn begin_poll(&mut self, now: Duration) {
        self.active = Some(now);
    }

    fn end_poll(&mut self, now: Duration) {
        if let Some(started) = self.active.take() {
            self.occupied += now.saturating_sub(started);
        }
    }

    fn exhausted(&self, now: Duration) -> bool {
        let active = self.active.map_or(Duration::ZERO, |start| now.saturating_sub(start));
        self.occupied + active >= SERVICE_BUDGET || self.passes >= PASS_BUDGET
    }

    fn yielded(&mut self, now: Duration) {
        self.active = self.active.map(|_| now);
        self.occupied = Duration::ZERO;
        self.passes = 0;
    }
}

pub(crate) struct ConnectionService {
    started: lodestone_time::Instant,
    window: Mutex<ServiceWindow>,
}

impl ConnectionService {
    pub(crate) fn new() -> Self {
        Self {
            started: lodestone_time::Instant::now(),
            window: Mutex::new(ServiceWindow::default()),
        }
    }

    pub(crate) async fn run<F: Future>(&self, future: F) -> F::Output {
        let mut future = std::pin::pin!(future);
        std::future::poll_fn(|cx| {
            let _timing = PhaseTimer::start(WorldgenTimingPhase::ConnectionPoll, 1);
            self.window.lock().expect("connection service poisoned")
                .begin_poll(self.started.elapsed());
            let result = future.as_mut().poll(cx);
            self.window.lock().expect("connection service poisoned")
                .end_poll(self.started.elapsed());
            result
        }).await
    }

    pub(crate) async fn admit_pass(&self) {
        let exhausted = self.window.lock().expect("connection service poisoned")
            .exhausted(self.started.elapsed());
        if exhausted {
            #[cfg(target_arch = "wasm32")]
            lodestone_time::browser_yield().await;
            #[cfg(not(target_arch = "wasm32"))]
            tokio::task::yield_now().await;
            self.window.lock().expect("connection service poisoned")
                .yielded(self.started.elapsed());
        }
        self.window.lock().expect("connection service poisoned").passes += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occupied_budget_excludes_suspension_and_survives_repolling() {
        let mut window = ServiceWindow::default();
        window.begin_poll(Duration::ZERO);
        window.end_poll(Duration::from_millis(3));
        assert!(!window.exhausted(Duration::from_secs(100)));
        window.begin_poll(Duration::from_secs(100));
        assert!(!window.exhausted(Duration::from_millis(100_004)));
        assert!(window.exhausted(Duration::from_millis(100_005)));
        window.end_poll(Duration::from_millis(100_005));
        window.begin_poll(Duration::from_secs(200));
        assert!(window.exhausted(Duration::from_secs(200)));
        window.yielded(Duration::from_secs(200));
        assert!(!window.exhausted(Duration::from_secs(200)));
        window.end_poll(Duration::from_millis(200_003));
        assert_eq!(window.occupied, Duration::from_millis(3));
    }

    #[test]
    fn pass_budget_bounds_clock_free_ready_bursts() {
        let mut window = ServiceWindow::default();
        window.begin_poll(Duration::ZERO);
        for _ in 0..PASS_BUDGET {
            assert!(!window.exhausted(Duration::ZERO));
            window.passes += 1;
        }
        assert!(window.exhausted(Duration::ZERO));
        window.end_poll(Duration::ZERO);
        window.begin_poll(Duration::from_secs(1));
        assert!(window.exhausted(Duration::from_secs(1)));
        window.yielded(Duration::from_secs(1));
        assert!(!window.exhausted(Duration::from_secs(1)));
    }
}
