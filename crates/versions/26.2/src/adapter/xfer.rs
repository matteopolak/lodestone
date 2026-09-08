//! The `transfer` tracing target records the ordering around a server-issued
//! player-position correction and a backend switch.
//!
//! # What it is
//!
//! The sequence counter lets logs from the driver task and the simulation
//! thread be read in causal order. The shell emits the correction-side records:
//! the event entering the simulation channel, movement being queued, and the
//! corrected pose being adopted. This module emits the connection-side records
//! for configuration, transfer, and login boundaries.
//!
//! A correction is handled by ordering, not by estimating whether two positions
//! are "close enough": the shell records a fully absolute position target when
//! it forwards the event, rewrites stale movement claims while that target is
//! in flight, and closes the window after the simulation adopts it. A packet
//! whose mask contains only relative yaw and pitch (decimal `24`) still carries
//! an absolute x/y/z target. A relative x, y, or z component stays unresolved
//! until the simulation can apply it against its current pose.
//!
//! The adapter's movement selector remains responsible only for choosing the
//! packet shape and its ordinary dirty tracking. It does not compare movement
//! against a remembered teleport or rewrite based on distance, elapsed time, or
//! a movement-history guess. That keeps a deliberate long move indistinguishable
//! from any other valid movement to this layer.
//!
//! To collect the trace:
//!
//! ```text
//! RUST_LOG=info,transfer=debug cargo run --release -p lodestone-shell --bin lodestone
//! ```
//!
//! # How to change it
//!
//! Keep every message prefixed `xfer:`. Add connection-boundary records beside
//! the state transition that emits them, and add correction records in
//! `crates/lodestone-shell/src/net.rs`, `sim/net_apply.rs`, or `sim/step.rs`
//! where the corresponding state transition occurs. Do not add a distance or
//! timing threshold to decide whether a movement claim is stale; the correction
//! window already has an explicit forwarded/adopted boundary.
//!
//! # Configuration and dependencies
//!
//! The target is enabled by the normal `tracing` subscriber; no feature or
//! environment variable is required beyond the optional `RUST_LOG` filter above.
//! [`next_seq`] is the only state in this module. The ordering-window state is
//! owned by the shell and is shared with its `NetClient` action relay.

use std::sync::atomic::{AtomicU64, Ordering};

/// Process-wide ordering counter for the `transfer` target. See the module doc.
static SEQ: AtomicU64 = AtomicU64::new(0);

/// Returns the next `transfer`-target sequence number.
///
/// `Relaxed` is deliberate and sufficient: the counter's only job is to totally
/// order the lines *within* the log, and each value is read once, into the line
/// that consumes it. Nothing else synchronises on it.
pub(crate) fn next_seq() -> u64 {
    SEQ.fetch_add(1, Ordering::Relaxed)
}
