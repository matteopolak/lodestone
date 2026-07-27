//! Native runtime timers, confined to this one `cfg(not(wasm32))`-gated module.
//!
//! `tokio::time::{timeout, sleep}` compile cleanly for `wasm32-unknown-unknown`
//! and then panic at runtime for want of a timer-enabled runtime — the same
//! "compiles green, dies in the browser" family as a wall-clock read or a
//! filesystem call, which `scripts/wasm-check.sh`'s compile pass is structurally
//! blind to. Keeping every runtime-timer call in this single module lets that
//! script enforce, by grep, that none leaks into code reachable from wasm: the
//! `lodestone-client time-confinement` rule bans the symbol everywhere except
//! this file. Callers are themselves `cfg(not(wasm32))`-gated; this module is
//! the single audited exception, so the invariant is checked rather than merely
//! promised.

use std::future::Future;
use std::time::Duration;

/// Marker returned when a [`timeout`] deadline elapses. Callers only need to
/// know the deadline passed, not carry the underlying timer error type.
pub(crate) struct Elapsed;

/// Runs `future` with a deadline. Mirrors `tokio::time::timeout`, but keeps that
/// symbol out of every other file so the confinement guard can enforce it.
pub(crate) async fn timeout<F>(duration: Duration, future: F) -> Result<F::Output, Elapsed>
where
    F: Future,
{
    tokio::time::timeout(duration, future)
        .await
        .map_err(|_| Elapsed)
}

/// Sleeps for `duration`. Mirrors `tokio::time::sleep`.
pub(crate) async fn sleep(duration: Duration) {
    tokio::time::sleep(duration).await;
}
