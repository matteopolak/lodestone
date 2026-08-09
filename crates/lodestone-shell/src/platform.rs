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
//! There is **no `cfg` fork here at all** — `web_time` already is one, and its
//! non-wasm arm is `pub use std::time::*`, so `crate::platform::Instant` is
//! `std::time::Instant` on native, the same type rather than a wrapper over it. If
//! you need something this module lacks, reach for another `web_time` item — do not
//! reach back for `std::time`, because `wasm-check.sh`'s `lodestone-shell
//! instant-ban` rule greps for `Instant::now(` across the crate and will
//! (correctly) fail.

/// A monotonic instant.
///
/// **Unconditional, with no `cfg` fork, and that is a property of `web_time` rather
/// than a shortcut.** Its non-wasm arm is literally `pub use std::time::*`, so on
/// native `crate::platform::Instant` *is* `std::time::Instant` — the same type, not
/// a newtype over it, so native has no wrapper, no conversion and provably no
/// behaviour change. On wasm32 it is `performance.now()`: specified monotonic, and
/// measured from the page's time origin rather than an arbitrary boot offset.
/// Nothing here depends on the absolute value, only on differences.
///
/// The practical consequence is worth stating because it is what made the port
/// tractable: any crate with an `Instant` in a **public signature** can switch to
/// `web_time::Instant` as a no-op on native, and no call site needs a `cfg`.
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
///
/// `web_time::SystemTime` for the same reason as [`Instant`]: it is `std`'s on
/// native and `Date.now()` in a browser. Note `Date.now()` is *not* monotonic — the
/// user can change the system clock, and a browser may coarsen it for
/// fingerprinting resistance. That is correct for a wall clock; use [`Instant`] for
/// durations.
#[must_use]
pub fn epoch_duration() -> std::time::Duration {
    web_time::SystemTime::now()
        .duration_since(web_time::UNIX_EPOCH)
        .unwrap_or_default()
}

/// The browser's asset byte source.
///
/// # What it is
///
/// Native resolves assets by *path*: `resources.rs` walks for a pack root and
/// `std::fs::read`s `client.jar` and `generated/reports/blocks.json` out of it. A
/// browser has no filesystem — measured: `std::fs::read` there returns
/// `Err(Unsupported)`, so the native path does not crash, it just reports "no
/// vanilla pack found" and falls back to the demo palette. That fallback is
/// *visible* (it banners), which is why this seam is an addition rather than a
/// rescue.
///
/// The important observation is that **only the byte acquisition differs**.
/// `lodestone-assets`' `ResourceSource` is a synchronous, byte-based trait, and
/// `ZipSource::from_bytes` builds a fully in-memory pack; `lodestone-render`'s
/// `BlocksJsonRegistry::from_slice` is likewise ungated. So the browser crosses the
/// filesystem wall exactly once, asynchronously, at the byte source — and every
/// parser, atlas builder and model baker downstream runs unchanged. That is the
/// same shape `web/`'s earlier feasibility spike proved with a trimmed pack.
///
/// # How it works
///
/// `web/` `fetch`es the bytes before it starts the app, calls [`install`] once, and
/// `resources.rs` reads them back through [`bundle`]. It is a `OnceLock` rather than
/// a parameter threaded through `Config` because the consumers are ~20 lazily-called
/// `load_*` functions spread across `resources.rs`, each of which independently
/// re-resolves the pack root today; giving them a process-wide byte source is a
/// strictly smaller change than giving all of them a new argument, and it matches
/// what the native side already does (`SELECTED_PACKS` is a process-wide `RwLock`
/// for the same reason).
///
/// # How to change it
///
/// If you need a third asset blob, add a field — do **not** add a second
/// `OnceLock`, or the "were the assets installed?" question stops having one
/// answer. [`install`] deliberately reports whether it won the race instead of
/// silently ignoring a second call: two different bundles installed in one session
/// is a bug in the caller, and one that would otherwise present as "the textures
/// are from the wrong pack".
#[cfg(target_arch = "wasm32")]
pub mod assets {
    use std::sync::OnceLock;

    /// The asset blobs a browser session needs, as raw bytes.
    ///
    /// These are the two files `resources.rs`' native `try_vanilla` reads off
    /// disk, in the same roles: the jar is the `ResourceSource` (textures, models,
    /// blockstates, lang), and the report is the block-state id table the atlas and
    /// the model baker are built against.
    #[derive(Debug)]
    pub struct Bundle {
        /// `client.jar` — the renderable corpus, consumed by `ZipSource::from_bytes`.
        pub client_jar: Vec<u8>,
        /// `generated/reports/blocks.json`, consumed by
        /// `BlocksJsonRegistry::from_slice`.
        pub blocks_report: Vec<u8>,
    }

    static BUNDLE: OnceLock<Bundle> = OnceLock::new();

    /// Installs the session's asset bytes. Returns `Err` with the already-installed
    /// bundle's sizes if one was installed first.
    ///
    /// # Errors
    /// Returns `Err(String)` when a bundle is already installed — a caller bug,
    /// reported rather than swallowed, because the symptom of swallowing it is a
    /// world rendered from the wrong pack with nothing in the log.
    pub fn install(bundle: Bundle) -> Result<(), String> {
        let jar = bundle.client_jar.len();
        let report = bundle.blocks_report.len();
        BUNDLE.set(bundle).map_err(|_| {
            let live = BUNDLE.get().expect("set failed, so one is installed");
            format!(
                "asset bundle already installed ({} B jar, {} B report); \
                 refused to replace it with {jar} B / {report} B",
                live.client_jar.len(),
                live.blocks_report.len(),
            )
        })
    }

    /// The installed bytes, or `None` when `web/` has not called [`install`] yet.
    #[must_use]
    pub fn bundle() -> Option<&'static Bundle> {
        BUNDLE.get()
    }
}
