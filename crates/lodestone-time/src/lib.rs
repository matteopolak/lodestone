//! `lodestone-time` — the workspace's one portable clock seam.
//!
//! This crate used to be three things at once: `crates/lodestone-shell/src/platform.rs`
//! (which carried this exact reasoning, the `Instant` re-export and `epoch_duration`),
//! plus improvised, uncommented copies of the same three lines in `lodestone-net`'s
//! ping timer and `lodestone-particle`'s entropy seed. All three made the identical
//! choice — depend on `web-time` unconditionally rather than fork on `cfg` — because
//! it is the right choice, but three independent copies of a rule is three places for
//! the rule to be forgotten. This crate is the one place it now lives.
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
//! outside this one crate) is the only thing that catches a fresh one.
//!
//! `SystemTime::now()` is worth calling out separately because it is a member of
//! that family that is easy to miss: `std::fs`, `Instant::now`,
//! `std::thread::spawn` and `tokio::time` are the ones everyone remembers, and wall
//! -clock time is not. `lodestone-shell` alone had 8 production `SystemTime::now()`
//! sites (seed derivation, the chat caret blink, the glint phase, the recipe-toast
//! clock) before this seam existed, and every one of them would have aborted the
//! tab.
//!
//! # Why `web_time` rather than a hand-rolled newtype
//!
//! The first version of this seam (when it lived in `lodestone-shell::platform`)
//! was a `f64`-millisecond newtype wrapping `performance.now()` by hand. **That was
//! the wrong answer, and the compiler said so:** `winit`'s wasm arm types
//! `ControlFlow::WaitUntil` as `web_time::Instant`, so the shell's own frame pacer
//! did not type-check against a private newtype — *"`browser::Instant` and
//! `web_time::time::instant::Instant` have similar names, but are actually distinct
//! types"*. `web-time` was already in the wasm dependency graph
//! (`winit 0.30.13 -> web-time 1.1.0`), so:
//!
//! * it is **API-identical to `std::time::Instant`** — including `AddAssign`,
//!   `Ord`, `Sub<Instant>` and `checked_*`, several of which the hand-rolled
//!   version had to omit or fake (`Ord` over an `f64` cannot be honest);
//! * it costs **zero extra bytes** in the bundle, because winit already links it;
//! * it is the type winit *hands us*, so a frame pacer needs no conversion.
//!
//! The general lesson, which is the reusable half: **before writing a portability
//! shim, check whether a crate already in the graph is the one the platform layer
//! above you already speaks.** A shim that is merely *equivalent* to the one the
//! neighbouring crate uses is not interchangeable with it.
//!
//! # Why this is a real crate rather than a re-exported convention
//!
//! `web-time` already does the platform switching; nothing here reimplements it.
//! The reason for a dedicated crate is **enforcement**: with exactly one sanctioned
//! path to a clock, `scripts/wasm-check.sh`'s confinement rules collapse from "grep
//! `std::time::Instant`/`SystemTime` out of every crate's `src`, separately, with a
//! separately-maintained allowlist" into one rule per hazard, scoped to every
//! wasm-linked crate, with this crate as the sole legitimate depender on `web-time`.
//! Fifty-five source comments across this workspace told readers not to reach for
//! `std::time` directly; that is the "prose instead of a guard" pattern at scale,
//! and a comment is not a rule until something checks it.
//!
//! # How to change this
//!
//! There is **no `cfg` fork here at all** — `web_time` already is one, and its
//! non-wasm arm is `pub use std::time::*`, so [`Instant`] is `std::time::Instant`
//! on native, the same type rather than a wrapper over it. If you need something
//! this crate lacks, reach for another `web_time` item and re-export it here — do
//! not reach for `std::time` directly in *any* crate, including this one:
//! `scripts/wasm-check.sh`'s per-crate `instant-ban`/`systemtime-ban` rules grep
//! for the `std::time::` paths across every wasm-linked crate and will (correctly)
//! fail. This crate itself is held to the same rule, with an empty allowlist —
//! it has no special exemption, it simply never needs one, because everything it
//! re-exports comes from `web_time`.

/// A monotonic instant.
///
/// **Unconditional, with no `cfg` fork, and that is a property of `web_time` rather
/// than a shortcut.** Its non-wasm arm is literally `pub use std::time::*`, so on
/// native `lodestone_time::Instant` *is* `std::time::Instant` — the same type, not
/// a newtype over it, so native has no wrapper, no conversion and provably no
/// behaviour change. On wasm32 it is `performance.now()`: specified monotonic, and
/// measured from the page's time origin rather than an arbitrary boot offset.
/// Nothing here depends on the absolute value, only on differences.
///
/// The practical consequence is worth stating because it is what made every port
/// into this crate tractable: any crate with an `Instant` in a **public signature**
/// can switch to this type as a no-op on native, and no call site needs a `cfg`.
pub use web_time::Instant;

/// Time elapsed since the Unix epoch, i.e. wall-clock time.
///
/// Replaces `SystemTime::now().duration_since(UNIX_EPOCH)`, which panics on
/// wasm32. Returns [`Duration::ZERO`](std::time::Duration::ZERO) rather than a
/// `Result` because every known call site already discarded the error
/// (`map_or(0, …)`/`unwrap_or_default()`) — the `SystemTimeError` case is a clock
/// set before 1970, and none of these consumers (a seed, a caret-blink phase, a
/// glint phase, a toast deadline) has anything better to do about it than carry
/// on. Folding that into the seam means call sites get *shorter* rather than
/// acquiring a second error path.
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

#[cfg(test)]
mod tests {
    use super::*;

    /// `Instant` differences are non-negative and the type round-trips through
    /// arithmetic the way `std::time::Instant` does — a smoke test that this is
    /// really the API it claims to be, not a claim about wasm behaviour (which no
    /// native test run can observe).
    #[test]
    fn instant_elapsed_is_non_negative_and_monotonic_between_two_reads() {
        let first = Instant::now();
        let second = Instant::now();
        assert!(second >= first);
        assert!(first.elapsed() >= std::time::Duration::ZERO);
    }

    /// `epoch_duration` never panics and returns a plausible "now" — well past the
    /// epoch, since this test runs long after 1970. Guards against the `unwrap_or_default`
    /// fallback silently masking a real clock failure by always returning `ZERO`.
    #[test]
    fn epoch_duration_is_well_past_the_unix_epoch() {
        let d = epoch_duration();
        // Any real clock reading built after this crate existed is comfortably
        // past 2020-01-01T00:00:00Z (1_577_836_800s), which is the discriminating
        // property: a broken seam collapsing to ZERO would fail this, while a
        // working one on any machine with a sane clock will not.
        assert!(
            d.as_secs() > 1_577_836_800,
            "epoch_duration() returned {d:?}, which looks like the ZERO fallback \
             rather than a real wall-clock reading"
        );
    }

    /// `Duration` is shared between `std::time` and `web_time` (the latter simply
    /// re-exports the former), so a `Duration` produced by this crate's [`Instant`]
    /// interoperates with one from plain `std::time::Duration` arithmetic with no
    /// conversion. This is the property every downstream crate relies on when it
    /// writes `std::time::Duration` in a signature next to `lodestone_time::Instant`.
    #[test]
    fn duration_interoperates_with_std_duration_with_no_conversion() {
        let start = Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(1));
        let elapsed: std::time::Duration = start.elapsed();
        let doubled: std::time::Duration = elapsed + std::time::Duration::from_millis(1);
        assert!(doubled > elapsed);
    }
}
