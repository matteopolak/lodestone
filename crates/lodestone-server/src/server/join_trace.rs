//! Opt-in timing for a connection's initial chunk stream (`LODESTONE_JOIN_TRACE`).

use super::*;

/// Portable monotonic clock for join-path measurements.
#[derive(Clone, Copy)]
pub(crate) struct JoinStopwatch {
    pub(super) started: Instant,
}

impl std::fmt::Debug for JoinStopwatch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // The platform clock is intentionally opaque here: a stopwatch's
        // starting instant is useful only through `elapsed`, and exposing it
        // would make the debug representation platform-specific.
        f.debug_struct("JoinStopwatch").finish_non_exhaustive()
    }
}

impl JoinStopwatch {
    pub(crate) fn now() -> Self {
        Self { started: Instant::now() }
    }

    pub(crate) fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }
}

/// Optional, low-overhead timing for one connection's initial chunk stream.
///
/// The trace is deliberately opt-in through `LODESTONE_JOIN_TRACE=1`; normal
/// joins do not allocate this state, clone it into workers, or pay a clock read
/// for every column. When enabled, one event is emitted for each stage and the
/// first event in each stage carries `first=true`, making time-to-first
/// generated/encoded/delivered columns visible without dumping payloads or
/// shader/source text.
#[derive(Debug)]
pub(crate) struct JoinTrace {
    pub(super) started: JoinStopwatch,
    pub(super) generated_seen: AtomicBool,
    pub(super) encoded_seen: AtomicBool,
    pub(super) delivered_seen: AtomicBool,
}

impl JoinTrace {
    /// Build a trace only when the operator explicitly requested it and the
    /// subscriber accepts the dedicated target. `None` is the normal path.
    pub(crate) fn new() -> Option<Arc<Self>> {
        #[cfg(not(target_arch = "wasm32"))]
        let requested = std::env::var_os("LODESTONE_JOIN_TRACE")
            .is_some_and(|value| !value.is_empty() && value != "0");
        #[cfg(target_arch = "wasm32")]
        let requested = false;
        if !requested || !tracing::enabled!(target: "lodestone_join_trace", tracing::Level::INFO) {
            return None;
        }
        Some(Arc::new(Self {
            started: JoinStopwatch::now(),
            generated_seen: AtomicBool::new(false),
            encoded_seen: AtomicBool::new(false),
            delivered_seen: AtomicBool::new(false),
        }))
    }

    /// Record one stage transition for a chunk. The stage names are stable so
    /// a capture can be grouped without parsing free-form log messages.
    pub(crate) fn mark(&self, stage: &'static str, cx: i32, cz: i32) {
        let first = match stage {
            "generated" => self.generated_seen.swap(true, Ordering::Relaxed),
            "encoded" => self.encoded_seen.swap(true, Ordering::Relaxed),
            "delivered" => self.delivered_seen.swap(true, Ordering::Relaxed),
            _ => true,
        };
        tracing::info!(
            target: "lodestone_join_trace",
            stage,
            cx,
            cz,
            first = !first,
            elapsed_millis = self.started.elapsed().as_millis() as u64,
            "join chunk stage"
        );
    }
}
