use std::sync::OnceLock;
use std::time::Duration;
#[cfg(any(target_arch = "wasm32", test))]
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionActivity {
    Select,
    TickUpdates,
    Relight,
    Vitals,
    JoinAdmission,
    JoinBegin,
    JoinColumn,
    JoinEnd,
    Dispatch,
    Publication,
}

impl ConnectionActivity {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Select => "select",
            Self::TickUpdates => "tick-updates",
            Self::Relight => "relight",
            Self::Vitals => "vitals",
            Self::JoinAdmission => "join-admission",
            Self::JoinBegin => "join-begin",
            Self::JoinColumn => "join-column",
            Self::JoinEnd => "join-end",
            Self::Dispatch => "dispatch",
            Self::Publication => "publication",
        }
    }
}

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
    pub activity: ConnectionActivity,
    pub activity_elapsed: Duration,
    pub target: Option<(i32, i32)>,
    pub packet_id: Option<i32>,
}

pub type ConnectionProgressSink = fn(ConnectionProgress);

static SINK: OnceLock<ConnectionProgressSink> = OnceLock::new();

pub fn install_sink(sink: ConnectionProgressSink) -> Result<(), ConnectionProgressSink> {
    SINK.set(sink)
}

#[cfg(any(target_arch = "wasm32", test))]
pub(crate) struct ConnectionProbe {
    shared: Arc<Mutex<ProbeState>>,
}

#[cfg(any(target_arch = "wasm32", test))]
struct ProbeState {
    sink: ConnectionProgressSink,
    started: lodestone_time::Instant,
    last_report: Option<Duration>,
    latest: Option<ConnectionProgress>,
    activity: ConnectionActivity,
    activity_started: Duration,
    target: Option<(i32, i32)>,
    packet_id: Option<i32>,
}

#[cfg(any(target_arch = "wasm32", test))]
impl ConnectionProbe {
    pub(crate) fn start() -> Option<Self> {
        let shared = Arc::new(Mutex::new(ProbeState {
            sink: *SINK.get()?,
            started: lodestone_time::Instant::now(),
            last_report: None,
            latest: None,
            activity: ConnectionActivity::Select,
            activity_started: Duration::ZERO,
            target: None,
            packet_id: None,
        }));
        #[cfg(target_arch = "wasm32")]
        {
            let weak = Arc::downgrade(&shared);
            wasm_bindgen_futures::spawn_local(async move {
                loop {
                    lodestone_time::browser_sleep(Duration::from_secs(1)).await;
                    let Some(shared) = weak.upgrade() else { break };
                    let report = {
                        let mut state = shared.lock().expect("connection probe lock poisoned");
                        let elapsed = state.started.elapsed();
                        state.report(elapsed)
                    };
                    if let Some((sink, progress)) = report {
                        sink(progress);
                    }
                }
            });
        }
        Some(Self { shared })
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn observe(&mut self, progress: ConnectionProgress) {
        let elapsed = self.shared.lock()
            .expect("connection probe lock poisoned").started.elapsed();
        self.record(progress, elapsed);
    }

    #[cfg(target_arch = "wasm32")]
    pub(crate) fn activity(
        &self,
        activity: ConnectionActivity,
        target: Option<(i32, i32)>,
        packet_id: Option<i32>,
    ) {
        let mut state = self.shared.lock().expect("connection probe lock poisoned");
        let elapsed = state.started.elapsed();
        state.set_activity(activity, target, packet_id, elapsed);
    }

    fn record(&mut self, mut progress: ConnectionProgress, elapsed: Duration) {
        let report = {
            let mut state = self.shared.lock().expect("connection probe lock poisoned");
            progress.passes = state.latest.map_or(1, |last| last.passes.saturating_add(1));
            state.latest = Some(progress);
            state.set_activity(ConnectionActivity::Select, None, None, elapsed);
            state.report(elapsed)
        };
        if let Some((sink, progress)) = report {
            sink(progress);
        }
    }
}

#[cfg(any(target_arch = "wasm32", test))]
impl ProbeState {
    fn set_activity(
        &mut self,
        activity: ConnectionActivity,
        target: Option<(i32, i32)>,
        packet_id: Option<i32>,
        elapsed: Duration,
    ) {
        self.activity = activity;
        self.activity_started = elapsed;
        self.target = target;
        self.packet_id = packet_id;
    }

    fn sample(&self, elapsed: Duration) -> Option<ConnectionProgress> {
        let mut progress = self.latest?;
        progress.running = true;
        progress.elapsed = elapsed;
        progress.activity = self.activity;
        progress.activity_elapsed = elapsed.saturating_sub(self.activity_started);
        progress.target = self.target;
        progress.packet_id = self.packet_id;
        Some(progress)
    }

    fn report(&mut self, elapsed: Duration) -> Option<(ConnectionProgressSink, ConnectionProgress)> {
        let progress = self.sample(elapsed)?;
        if self.last_report.is_none_or(|last| {
            elapsed.saturating_sub(last) >= Duration::from_secs(1)
        }) {
            self.last_report = Some(elapsed);
            Some((self.sink, progress))
        } else {
            None
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
            activity: ConnectionActivity::Select,
            activity_elapsed: Duration::ZERO,
            target: None,
            packet_id: None,
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
        {
            let mut state = probe.shared.lock().unwrap();
            state.set_activity(
                ConnectionActivity::JoinAdmission,
                Some((9, -2)),
                None,
                Duration::from_millis(1_002),
            );
            let (sink, progress) = state.report(Duration::from_secs(4)).unwrap();
            sink(progress);
        }
        drop(probe);
        let events = EVENTS.lock().unwrap();
        assert_eq!(events.len(), 4);
        assert!(events[0].running);
        assert!(!events[0].client_loaded);
        assert_eq!(events[0].passes, 1);
        assert_eq!(events[1].passes, 3);
        assert_eq!(events[1].delivered_columns, 169);
        assert_eq!(events[1].chunks_sent, 170);
        assert_eq!(events[1].remaining, 272);
        assert!(events[2].running);
        assert_eq!(events[2].passes, 4);
        assert_eq!(events[2].remaining, 0);
        assert_eq!(events[2].center, (-3, 7));
        assert_eq!(events[2].activity, ConnectionActivity::JoinAdmission);
        assert_eq!(events[2].activity_elapsed, Duration::from_millis(2_998));
        assert_eq!(events[2].target, Some((9, -2)));
        assert!(!events[3].running);
    }
}

#[cfg(any(target_arch = "wasm32", test))]
impl Drop for ConnectionProbe {
    fn drop(&mut self) {
        let report = {
            let state = self.shared.lock().expect("connection probe lock poisoned");
            state.sample(state.started.elapsed()).map(|progress| (state.sink, progress))
        };
        if let Some((sink, mut progress)) = report {
            progress.running = false;
            sink(progress);
        }
    }
}
