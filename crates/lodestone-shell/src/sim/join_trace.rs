//! Optional client-side timing for the initial chunk stream.
//!
//! `LODESTONE_JOIN_TRACE=1` enables the same `lodestone_join_trace` target the
//! integrated server uses. The normal client keeps this disabled, so chunk
//! arrival and meshing do not pay a clock read or emit a line per column.
//! Events stop at `remeshed`: that is the boundary this simulation owns, while
//! GPU upload and presentation remain in the window driver.

use crate::platform::Instant;

/// Client-side half of the opt-in join timeline.
#[derive(Debug)]
pub(crate) struct JoinTrace {
    enabled: bool,
    started: Instant,
    received_count: u32,
    remesh_queued_count: u32,
    remeshed_count: u32,
}

impl Default for JoinTrace {
    fn default() -> Self {
        Self::new()
    }
}

impl JoinTrace {
    /// Create a disabled trace unless the operator explicitly requests it.
    pub(crate) fn new() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let enabled = std::env::var_os("LODESTONE_JOIN_TRACE")
            .is_some_and(|value| !value.is_empty() && value != "0")
            && tracing::enabled!(target: "lodestone_join_trace", tracing::Level::INFO);
        #[cfg(target_arch = "wasm32")]
        let enabled = tracing::enabled!(target: "lodestone_join_trace", tracing::Level::INFO);
        Self {
            enabled,
            started: Instant::now(),
            received_count: 0,
            remesh_queued_count: 0,
            remeshed_count: 0,
        }
    }

    /// Restart the timeline at the beginning of a new network session.
    pub(crate) fn restart(&mut self) {
        self.started = Instant::now();
        self.received_count = 0;
        self.remesh_queued_count = 0;
        self.remeshed_count = 0;
    }

    /// Record a client-side stage for one chunk. The first event in each stage
    /// is marked with `first=true`, so the join's critical path is visible in a
    /// mixed server/client log without dumping packet bodies.
    pub(crate) fn mark(&mut self, stage: &'static str, cx: i32, cz: i32) {
        if !self.enabled {
            return;
        }
        let count = match stage {
            "received" => &mut self.received_count,
            "remesh_queued" => &mut self.remesh_queued_count,
            "remeshed" => &mut self.remeshed_count,
            _ => return,
        };
        *count = count.saturating_add(1);
        let first = *count == 1;
        #[cfg(target_arch = "wasm32")]
        if !first && !count.is_multiple_of(16) {
            return;
        }
        tracing::info!(
            target: "lodestone_join_trace",
            stage,
            cx,
            cz,
            first,
            count = *count,
            elapsed_millis = self.started.elapsed().as_millis() as u64,
            "join chunk stage"
        );
    }
}
