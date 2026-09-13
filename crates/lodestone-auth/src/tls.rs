//! The process-default rustls crypto provider.
//!
//! ## What this is
//!
//! One idempotent function, [`install_crypto_provider`], that installs
//! [`ring`](rustls::crypto::ring) as this process's default rustls
//! `CryptoProvider`. Every place in the workspace that builds a
//! `reqwest::Client` must call it first.
//!
//! ## Why it has to exist
//!
//! `reqwest`'s `rustls` feature hard-wires the provider to
//! `aws-lc-rs`, whose `aws-lc-sys` vendors roughly 1,500 C translation units and
//! is compiled by a build script — so `sccache` (which wraps rustc, not a build
//! script's C toolchain) cannot cache it, and it is rebuilt from scratch in
//! every target directory. Twenty-five concurrent copies were counted across
//! per-agent target dirs. `reqwest` offers no `rustls-ring` feature; the only
//! escape is `rustls-no-provider`, which enables the same TLS stack and leaves
//! provider selection to the application. This module *is* that selection.
//!
//! ## The gotcha, which is the whole risk of the change
//!
//! **A missing install is a runtime panic, not a compile error.** With
//! `rustls-no-provider`, `reqwest`'s internal `default_rustls_crypto_provider()`
//! is a bare `panic!` guarded by `#[cfg(not(feature = "__rustls-aws-lc-rs"))]`,
//! and it fires from `ClientBuilder::build()` — i.e. from
//! `reqwest::Client::new()`. So no `cargo check`, at any feature setting, can
//! see a construction site that forgot to call this. What catches it:
//!
//! * every `reqwest::Client` built anywhere in the workspace is preceded by a
//!   call to [`install_crypto_provider`] — grep for `Client::new` and
//!   `Client::builder` if you add one;
//! * `tests/tls_provider.rs` builds a client through the production code path
//!   and asserts it succeeds, with no network egress, and is **not**
//!   `#[ignore]`d;
//! * the three existing `#[tokio::test]`s in `login.rs`, `migrate.rs` and
//!   `browser_login.rs` that construct a client are incidental canaries for the
//!   same thing — they panic outright if the install is gone.
//!
//! ## How to change it
//!
//! To swap providers, change the one `ring` reference below *and* the `rustls`
//! feature list in the workspace manifest. Do not reach for `install_default`
//! directly at a call site: it returns `Err` when a provider is already
//! installed, so an `expect` on it is a time bomb in any process that builds
//! two clients, and in every test binary that runs more than one test.
//!
//! ## Dependencies
//!
//! `rustls` with `default-features = false, features = ["ring", "std",
//! "tls12"]`. Turning rustls' default features back on would re-enable both
//! `aws_lc_rs` and `prefer-post-quantum` (which itself enables `aws_lc_rs`) and
//! silently reintroduce the `aws-lc-sys` build through feature unification.

use std::sync::Once;

/// Install `ring` as this process's default rustls `CryptoProvider`.
///
/// Idempotent and cheap to call repeatedly: the work happens once per process
/// behind a [`Once`], and an already-installed provider is accepted rather than
/// treated as an error. Call it immediately before building a
/// `reqwest::Client` — there is no ordering subtlety to reason about if the call
/// sits next to the construction it protects.
///
/// Deliberately infallible. `CryptoProvider::install_default` returns `Err` if
/// *any* provider is already installed, which is a benign race between two
/// clients rather than a fault, so surfacing it would only invite an `expect`
/// that panics under exactly the conditions this function exists to survive.
pub fn install_crypto_provider() {
    static ONCE: Once = Once::new();

    ONCE.call_once(|| {
        // `is_err()` means another provider won the race, or something else in
        // the process installed one first. Either way a provider is now present,
        // which is the postcondition callers depend on.
        if rustls::crypto::ring::default_provider()
            .install_default()
            .is_err()
        {
            tracing::debug!(
                "a rustls CryptoProvider was already installed; leaving it in place"
            );
        }
    });
}
