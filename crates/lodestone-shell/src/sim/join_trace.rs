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
    received_seen: bool,
    remesh_queued_seen: bool,
    remeshed_seen: bool,
}

impl Default for JoinTrace {
    fn default() -> Self {
        Self::new()
    }
}

impl JoinTrace {
    /// Create a disabled trace unless the operator explicitly requests the
    /// dedicated target. Browser builds cannot read a host environment and
    /// therefore keep this path disabled.
    pub(crate) fn new() -> Self {
        #[cfg(not(target_arch = "wasm32"))]
        let enabled = std::env::var_os("LODESTONE_JOIN_TRACE")
            .is_some_and(|value| !value.is_empty() && value != "0")
            && tracing::enabled!(target: "lodestone_join_trace", tracing::Level::INFO);
        #[cfg(target_arch = "wasm32")]
        let enabled = false;
        Self {
            enabled,
            started: Instant::now(),
            received_seen: false,
            remesh_queued_seen: false,
            remeshed_seen: false,
        }
    }

    /// Restart the timeline at the beginning of a new network session.
    pub(crate) fn restart(&mut self) {
        self.started = Instant::now();
        self.received_seen = false;
        self.remesh_queued_seen = false;
        self.remeshed_seen = false;
    }

    /// Record a client-side stage for one chunk. The first event in each stage
    /// is marked with `first=true`, so the join's critical path is visible in a
    /// mixed server/client log without dumping packet bodies.
    pub(crate) fn mark(&mut self, stage: &'static str, cx: i32, cz: i32) {
        if !self.enabled {
            return;
        }
        let first = match stage {
            "received" => {
                let previous = self.received_seen;
                self.received_seen = true;
                !previous
            }
            "remesh_queued" => {
                let previous = self.remesh_queued_seen;
                self.remesh_queued_seen = true;
                !previous
            }
            "remeshed" => {
                let previous = self.remeshed_seen;
                self.remeshed_seen = true;
                !previous
            }
            _ => false,
        };
        tracing::info!(
            target: "lodestone_join_trace",
            stage,
            cx,
            cz,
            first,
            elapsed_millis = self.started.elapsed().as_millis() as u64,
            "join chunk stage"
        );
    }
}

