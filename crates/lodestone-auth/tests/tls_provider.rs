//! Gate for the `rustls-no-provider` + `ring` switch.
//!
//! ## What this is for
//!
//! `reqwest` is built with `rustls-no-provider` so that `aws-lc-rs` — and with it
//! `aws-lc-sys`'s ~1,500 vendored C translation units — stays out of the
//! dependency graph. The cost of that choice is that provider selection becomes
//! the application's job, and **getting it wrong is a runtime panic, not a
//! compile error**: reqwest's own `default_rustls_crypto_provider()` is a bare
//! `panic!` under `#[cfg(not(feature = "__rustls-aws-lc-rs"))]`, fired from
//! `ClientBuilder::build()`. Every health check in CLAUDE.md — including
//! `--all-features --all-targets` — stays green with the install missing. So the
//! gate has to *run* something, and this file is it.
//!
//! ## The split, and why
//!
//! The three tests below are **not** `#[ignore]`d and make **no** network
//! request. The handshake test at the bottom **is** `#[ignore]`d because it
//! reaches the internet. That split is deliberate and follows a real defect in
//! this repo: an accounts-screen unit test used to `Command::new("open")` a
//! Microsoft OAuth URL in the owner's browser on every `cargo test -p
//! lodestone-shell` run — a user-visible side effect that no health check could
//! see, because the suite passed. Anything network-shaped here is opt-in, and it
//! deliberately hits a neutral IANA-reserved host rather than Microsoft's OAuth
//! endpoints (`tests/device_code_live.rs` owns those, and its responses carry
//! live credentials).
//!
//! ## What each test actually proves
//!
//! Honest accounting, because "a provider is installed" and "TLS works" are
//! different claims:
//!
//! | test | egress | proves |
//! |---|---|---|
//! | `installed_provider_is_ring_and_not_a_post_quantum_aws_lc_build` | no | a provider is installed, and it is ring's |
//! | `client_builds_through_the_production_tls_path` | no | the provider is *usable*: `builder_with_provider` + the platform verifier accept it |
//! | `both_tls_versions_have_cipher_suites_under_ring` | no | ring supplies suites for TLS 1.2 *and* 1.3 |
//! | `real_https_handshake_succeeds_with_ring` | **yes** | an end-to-end handshake against a real server |
//!
//! The middle two are the load-bearing non-ignored ones. `install_crypto_provider`
//! could install a provider that is present but unusable — one with no suites for
//! a requested protocol version makes `with_protocol_versions` return `Err` — and
//! a bare `get_default().is_some()` assertion cannot tell those apart.

use rustls::crypto::CryptoProvider;

/// A provider is installed after the call, and it is ring's.
///
/// Two assertions with different sources. The first compares the installed
/// provider's cipher-suite and key-exchange-group names against
/// `rustls::crypto::ring::default_provider()` — i.e. against ring's own crate,
/// not against anything this repo authored — so it fails if the helper installs
/// some other or hand-rolled provider.
///
/// The second is the discriminator that does not depend on that comparison:
/// rustls gates its post-quantum key exchange behind `prefer-post-quantum`,
/// which *itself* enables `aws_lc_rs` (see rustls' manifest). So an installed
/// provider offering a `MLKEM`/`Kyber` group is proof that aws-lc-rs is in the
/// process, whatever else agrees. Asserting its absence is the runtime half of
/// the `cargo tree -i aws-lc-sys` proof.
#[test]
fn installed_provider_is_ring_and_not_a_post_quantum_aws_lc_build() {
    lodestone_auth::install_crypto_provider();

    let installed = CryptoProvider::get_default()
        .expect("install_crypto_provider must leave a process-default provider installed");

    let ring = rustls::crypto::ring::default_provider();

    let names = |p: &CryptoProvider| {
        let suites: Vec<String> = p
            .cipher_suites
            .iter()
            .map(|s| format!("{:?}", s.suite()))
            .collect();
        let groups: Vec<String> = p
            .kx_groups
            .iter()
            .map(|g| format!("{:?}", g.name()))
            .collect();
        (suites, groups)
    };

    let (installed_suites, installed_groups) = names(installed);
    let (ring_suites, ring_groups) = names(&ring);

    assert_eq!(
        installed_suites, ring_suites,
        "the installed provider's cipher suites must be exactly ring's"
    );
    assert_eq!(
        installed_groups, ring_groups,
        "the installed provider's key-exchange groups must be exactly ring's"
    );

    // The provider-independent discriminator. Post-quantum key exchange in rustls
    // only exists under `prefer-post-quantum`, which enables `aws_lc_rs`.
    for group in &installed_groups {
        let upper = group.to_uppercase();
        assert!(
            !upper.contains("MLKEM") && !upper.contains("KYBER"),
            "installed provider offers post-quantum group {group}, which rustls only \
             provides via `prefer-post-quantum` -> `aws_lc_rs`: aws-lc is back in the graph"
        );
    }
}

/// The provider is not merely present but *usable* by the code path production
/// uses.
///
/// This is the test that catches the real risk of a missing provider install:
/// a default `reqwest::Client` build walks exactly the branch the panic lives
/// on:
/// `CryptoProvider::get_default()`, then `ClientConfig::builder_with_provider`,
/// then `rustls_platform_verifier::Verifier::new(provider)` — so it also settles
/// the open question of whether the platform verifier needs a provider of its own
/// (it does not; reqwest hands it ours).
///
/// No egress: building a client opens no socket. It does read the OS trust store,
/// which is a local read, not a network request.
#[tokio::test]
async fn client_builds_through_the_production_tls_path() {
    lodestone_auth::install_crypto_provider();

    let client = reqwest::Client::builder()
        .user_agent("lodestone-tls-provider-gate")
        .build();

    assert!(
        client.is_ok(),
        "a default reqwest client must build with ring installed; \
         err = {:?}",
        client.err()
    );
}

/// Ring supplies cipher suites for both protocol versions we allow.
///
/// `with_protocol_versions` returns `Err` when the provider has no suites for a
/// requested version, so pinning each version in turn is a real capability probe
/// rather than a restatement of the test above — a provider trimmed to TLS 1.3
/// only would pass `client_builds_through_the_production_tls_path` and fail here.
#[tokio::test]
async fn both_tls_versions_have_cipher_suites_under_ring() {
    lodestone_auth::install_crypto_provider();

    for (label, version) in [
        ("TLS 1.2", reqwest::tls::Version::TLS_1_2),
        ("TLS 1.3", reqwest::tls::Version::TLS_1_3),
    ] {
        let built = reqwest::Client::builder()
            .min_tls_version(version)
            .max_tls_version(version)
            .build();
        assert!(
            built.is_ok(),
            "ring must supply cipher suites for {label}; err = {:?}",
            built.err()
        );
    }
}

/// Idempotence. `CryptoProvider::install_default` returns `Err` once a provider
/// exists, so a helper that `expect`ed it would panic on the second call — which
/// in a test binary means the second *test*, an ordering-dependent failure that is
/// miserable to diagnose.
#[test]
fn installing_twice_is_not_an_error() {
    lodestone_auth::install_crypto_provider();
    lodestone_auth::install_crypto_provider();
    assert!(CryptoProvider::get_default().is_some());
}

/// A real TLS handshake, end to end, through ring and the platform verifier.
///
/// `#[ignore]`d: it reaches the internet, and per this repo's history a test with
/// an unrequested side effect is a defect even when it passes. Run it explicitly:
///
/// ```text
/// cargo test -p lodestone-auth --test tls_provider -- --ignored --nocapture
/// ```
///
/// `example.com` on purpose: IANA-reserved (RFC 2606), no credentials, no
/// tracking, and served from a commercially-issued certificate chaining to a
/// system root — so a pass exercises `rustls-platform-verifier` against the real
/// OS trust store, which is the half of the change least visible to a unit test.
/// **Not** a Microsoft OAuth endpoint: those responses carry live credentials and
/// belong to `tests/device_code_live.rs`.
#[tokio::test]
#[ignore = "performs a real HTTPS request to example.com"]
async fn real_https_handshake_succeeds_with_ring() {
    lodestone_auth::install_crypto_provider();

    let client = reqwest::Client::builder()
        .user_agent("lodestone-tls-provider-gate")
        .build()
        .expect("client must build with ring installed");

    let response = client
        .get("https://example.com/")
        .send()
        .await
        .expect("the TLS handshake and request must succeed with ring installed");

    // Status and version only — never the body. Keeping this habit here, where the
    // body is a public page, is what keeps it in place in the auth tests where the
    // body is a credential.
    assert!(
        response.status().is_success(),
        "expected a successful status, got {}",
        response.status()
    );
    assert!(
        response.content_length().is_none_or(|len| len > 0),
        "a successful HTTPS response should not be empty"
    );
}
