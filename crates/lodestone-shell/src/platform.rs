//! `crate::platform` — the shell's native/browser seam for the things a browser
//! does not have.
//!
//! The shell is one application compiled for two targets: a native `winit`/`wgpu`
//! window, and `wasm32-unknown-unknown` inside a browser tab (consumed by `web/`).
//! Almost all of it is portable already. This module holds the handful of
//! primitives that are *not*, so the rest of the crate names one symbol and stops
//! caring:
//!
//! | seam | native | browser |
//! |---|---|---|
//! | [`Instant`] | `std::time::Instant` | `web_time::Instant` (`performance.now()`) |
//! | [`epoch_duration`] | `SystemTime::now()` since the Unix epoch | `web_time::SystemTime` (`Date.now()`) |
//!
//! # Why a clock seam at all — the failure mode this exists to delete
//!
//! **`std::time::Instant::now()` compiles for `wasm32-unknown-unknown` and panics
//! when it runs.** So does `SystemTime::now()`. With `panic = "abort"` in the
//! browser profile that is not a recoverable error, it is the tab dying, and **no
//! `cargo check` at any feature setting can see it** — the call type-checks
//! perfectly. `scripts/wasm-check.sh`'s header is the long-form writeup; the short
//! version is that this is the archetype of the "compiles on wasm, dies at
//! runtime" family, and a confinement guard (grep the banned symbol everywhere
//! outside one gated file) is the only thing that catches a fresh one.
//!
//! `SystemTime::now()` is worth calling out separately because **it is a member of
//! that family that `wasm-check.sh`'s header does not list**: `std::fs`,
//! `Instant::now`, `std::thread::spawn` and `tokio::time` are named there, and
//! wall-clock time is not. The shell had 8 production `SystemTime::now()` sites
//! (seed derivation, the chat caret blink, the glint phase, the recipe-toast clock)
//! and every one of them would have aborted the tab.
//!
//! # Why `web_time` rather than a hand-rolled newtype
//!
//! The first version of this module was a `f64`-millisecond newtype wrapping
//! `performance.now()` by hand. **That was the wrong answer, and the compiler said
//! so:** `winit`'s wasm arm types `ControlFlow::WaitUntil` as
//! `web_time::Instant`, so `app::pacing`'s
//! `ControlFlow::WaitUntil(now + BACKGROUND_POLL)` did not type-check against a
//! private newtype — *"`browser::Instant` and `web_time::time::instant::Instant`
//! have similar names, but are actually distinct types"*. `web-time` was already in
//! the wasm dependency graph (`winit 0.30.13 -> web-time 1.1.0`), so:
//!
//! * it is **API-identical to `std::time::Instant`** — including `AddAssign`,
//!   `Ord`, `Sub<Instant>` and `checked_*`, several of which the hand-rolled
//!   version had to omit or fake (`Ord` over an `f64` cannot be honest);
//! * it costs **zero extra bytes** in the bundle, because winit already links it;
//! * it is the type winit *hands us*, so the pacer needs no conversion.
//!
//! The general lesson, which is the reusable half: **before writing a portability
//! shim, check whether a crate already in the graph is the one the platform layer
//! above you already speaks.** A shim that is merely *equivalent* to the one the
//! neighbouring crate uses is not interchangeable with it.
//!
//! # How to change this
//!
//! Both arms are **re-exports, not wrappers**, so neither target has a newtype,
//! an inlining question, or any behaviour change: `crate::platform::Instant` *is*
//! `std::time::Instant` on native and *is* `web_time::Instant` in a browser. If you
//! need something the browser arm lacks, reach for another `web_time` item — do not
//! reach back for `std::time`, because `wasm-check.sh`'s `lodestone-shell
//! instant-ban` rule greps for `Instant::now(` across the crate and will
//! (correctly) fail.

#[cfg(not(target_arch = "wasm32"))]
pub use std::time::Instant;

/// The browser clock, backed by `performance.now()` — specified monotonic, and
/// measured from the page's time origin rather than an arbitrary boot offset.
/// Nothing in the shell depends on the absolute value, only on differences.
#[cfg(target_arch = "wasm32")]
pub use web_time::Instant;

/// Time elapsed since the Unix epoch, i.e. wall-clock time.
///
/// Replaces `SystemTime::now().duration_since(UNIX_EPOCH)`, which panics on
/// wasm32. Returns [`Duration::ZERO`](std::time::Duration::ZERO) rather than a
/// `Result` because **every one of the shell's call sites already discarded the
/// error** with `map_or(0, …)`/`unwrap_or_default()` — the `SystemTimeError` case
/// is a clock set before 1970, and none of these consumers (a seed, a caret-blink
/// phase, a glint phase, a toast deadline) has anything better to do about it than
/// carry on. Folding that into the seam means the call sites get *shorter* rather
/// than acquiring a second error path.
#[must_use]
pub fn epoch_duration() -> std::time::Duration {
    #[cfg(not(target_arch = "wasm32"))]
    let now = std::time::SystemTime::now();
    // `Date.now()`, which is *not* monotonic — the user can change the system
    // clock, and a browser may coarsen it for fingerprinting resistance. That is
    // correct for a wall clock; use `Instant` for durations.
    #[cfg(target_arch = "wasm32")]
    let now = web_time::SystemTime::now();

    #[cfg(not(target_arch = "wasm32"))]
    let epoch = std::time::UNIX_EPOCH;
    #[cfg(target_arch = "wasm32")]
    let epoch = web_time::UNIX_EPOCH;

    now.duration_since(epoch).unwrap_or_default()
}
