use std::sync::OnceLock;
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ConnectionProgress {
    pub running: bool,
    pub elapsed: Duration,
    pub passes: u64,
    pub client_loaded: bool,
    pub center: (i32, i32),
    pub radius: i32,
    pub owed_columns: usize,
    pub delivered_columns: usize,
    pub chunks_sent: usize,
    pub remaining: usize,
}

pub type ConnectionProgressSink = fn(ConnectionProgress);

static SINK: OnceLock<ConnectionProgressSink> = OnceLock::new();

pub fn install_sink(sink: ConnectionProgressSink) -> Result<(), ConnectionProgressSink> {
    SINK.set(sink)
}

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) struct ConnectionProbe {
    sink: ConnectionProgressSink,
    started: lodestone_time::Instant,
    last_report: Option<Duration>,
    latest: Option<ConnectionProgress>,
}

#[cfg(any(target_arch = "wasm32", test))]
impl ConnectionProbe {
    pub(crate) fn start() -> Option<Self> {
        Some(Self {
            sink: *SINK.get()?,
            started: lodestone_time::Instant::now(),
            last_report: None,
            latest: None,
        })
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn observe(&mut self, progress: ConnectionProgress) {
        self.record(progress, self.started.elapsed());
    }

    fn record(&mut self, mut progress: ConnectionProgress, elapsed: Duration) {
        progress.running = true;
        progress.elapsed = elapsed;
        progress.passes = self.latest.map_or(1, |last| last.passes.saturating_add(1));
        self.latest = Some(progress);
        if self.last_report.is_none_or(|last| {
            progress.elapsed.saturating_sub(last) >= Duration::from_secs(1)
        }) {
            self.last_report = Some(progress.elapsed);
            (self.sink)(progress);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    static EVENTS: Mutex<Vec<ConnectionProgress>> = Mutex::new(Vec::new());

    #[test]
    fn probe_samples_live_passes_and_always_reports_latest_scope_exit() {
        install_sink(|event| EVENTS.lock().unwrap().push(event)).unwrap();
        let mut probe = ConnectionProbe::start().unwrap();
        let mut progress = ConnectionProgress {
            running: false,
            elapsed: Duration::ZERO,
            passes: 0,
            client_loaded: false,
            center: (-3, 7),
            radius: 10,
            owed_columns: 441,
            delivered_columns: 1,
            chunks_sent: 1,
            remaining: 440,
        };
        probe.record(progress, Duration::ZERO);
        progress.client_loaded = true;
        progress.delivered_columns = 169;
        progress.chunks_sent = 170;
        progress.remaining = 272;
        probe.record(progress, Duration::from_millis(999));
        probe.record(progress, Duration::from_secs(1));
        progress.remaining = 0;
        probe.record(progress, Duration::from_millis(1_001));
        drop(probe);
        let events = EVENTS.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert!(events[0].running);
        assert!(!events[0].client_loaded);
        assert_eq!(events[0].passes, 1);
        assert_eq!(events[1].passes, 3);
        assert_eq!(events[1].delivered_columns, 169);
        assert_eq!(events[1].chunks_sent, 170);
        assert_eq!(events[1].remaining, 272);
        assert!(!events[2].running);
        assert_eq!(events[2].passes, 4);
        assert_eq!(events[2].remaining, 0);
        assert_eq!(events[2].center, (-3, 7));
    }
}

#[cfg(any(target_arch = "wasm32", test))]
impl Drop for ConnectionProbe {
    fn drop(&mut self) {
        if let Some(mut progress) = self.latest {
            progress.running = false;
            progress.elapsed = self.started.elapsed();
            (self.sink)(progress);
        }
    }
}
