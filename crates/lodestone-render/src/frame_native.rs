//! Native-only frame-timing implementation, wholly gated off `wasm32`.
//!
//! This file is the **single** permitted home for `std::time::Instant` in the
//! crate. `Instant::now()` compiles on `wasm32-unknown-unknown` but traps at
//! runtime, so it lives here behind `#[cfg(not(target_arch = "wasm32"))]` and
//! nowhere else. The `no_wasm_trap_symbols_are_confined` guard in `frame.rs`
//! enforces that confinement in CI — a fresh ungated `Instant::now()` (or
//! `std::fs`, `std::thread::spawn`, `tokio::time`) added anywhere else fails the
//! test, naming the offending file and line, rather than compiling green and
//! trapping in a browser later.

use std::time::Duration;
use std::time::Instant;

use crate::frame::TimeSource;

/// The native monotonic clock, backed by [`std::time::Instant`].
///
/// Not available on `wasm32` — a browser build supplies a `performance.now()`
/// backed [`TimeSource`] instead, because `Instant::now()` traps there.
#[derive(Debug, Clone)]
pub struct SystemClock {
    origin: Instant,
}

impl SystemClock {
    /// Create a clock whose origin is the moment of construction.
    #[must_use]
    pub fn new() -> Self {
        Self {
            origin: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl TimeSource for SystemClock {
    fn now(&self) -> Duration {
        self.origin.elapsed()
    }
}
