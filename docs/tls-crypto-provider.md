# TLS crypto provider (`ring`, not `aws-lc-rs`)

## What it is

Lodestone's HTTPS stack is `reqwest` → `rustls`, and rustls requires a
*`CryptoProvider`* to be chosen. We choose **`ring`**, installed once per process
by `lodestone_auth::install_crypto_provider()`, so that `aws-lc-rs` — and with it
`aws-lc-sys`'s roughly 1,500 vendored C translation units — stays out of the
dependency graph entirely (issue #446).

## How it works

`reqwest` is declared in the workspace manifest with the **`rustls-no-provider`**
feature, not `rustls`:

```toml
reqwest = { version = "0.13", default-features = false, features = [
    "json", "form", "rustls-no-provider", "http2", "charset",
] }
rustls = { version = "0.23", default-features = false, features = [
    "ring", "std", "tls12",
] }
```

That is forced, not preference. `reqwest`'s own manifest defines

```toml
rustls            = ["__rustls-aws-lc-rs", "dep:rustls-platform-verifier", "__rustls"]
rustls-no-provider = [                      "dep:rustls-platform-verifier", "__rustls"]
```

— the `rustls` feature **hard-wires** the provider to `aws-lc-rs` and there is no
`rustls-ring` feature to pick instead. `rustls-no-provider` enables the identical
TLS stack (`hyper-rustls`, `tokio-rustls`, `rustls`, `rustls-platform-verifier`)
and only declines to choose a provider, leaving that to the application.

At runtime `reqwest`'s `ClientBuilder::build()` calls
`rustls::crypto::CryptoProvider::get_default()` and, on `None`, falls through to
its own `default_rustls_crypto_provider()` — which under
`#[cfg(not(feature = "__rustls-aws-lc-rs"))]` is a bare `panic!`. It then passes
whatever provider it got to *both* `ClientConfig::builder_with_provider` and
`rustls_platform_verifier::Verifier::new(provider)`, which answers the question
that looks like a second problem: **the platform verifier needs no provider of
its own.** It is handed ours. (Its manifest confirms this from the other side: it
depends on `rustls` with `default-features = false, features = ["std"]` and
selects no provider. Its `rustls/ring` reference lives in the Android-only
`ffi-testing` feature.)

So one install, before the first client is built, is sufficient — and
`lodestone_auth::install_crypto_provider()` is it:

```rust
pub fn install_crypto_provider() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        if rustls::crypto::ring::default_provider().install_default().is_err() {
            tracing::debug!("a rustls CryptoProvider was already installed; leaving it in place");
        }
    });
}
```

Lives in `crates/lodestone-auth/src/tls.rs`, re-exported at the crate root, and
gated `cfg(not(target_arch = "wasm32"))` — reqwest's rustls dependencies are
themselves target-gated that way, so a browser build has no TLS stack to
configure and must not grow one. (`lodestone-client` is in
`scripts/wasm-check.sh`'s crate list and depends on `lodestone-auth`, so an
ungated `rustls` dependency here would break that guard.)

### Where it is called

Every site in the workspace that constructs a `reqwest::Client`, immediately
before the construction — **not** in `main()`. That placement is deliberate:
a `main()`-only install leaves every test binary panicking, and there is no
ordering to reason about when the call sits next to the thing it protects.

| file | site |
|---|---|
| `crates/lodestone-client/src/driver.rs` | `Driver::new` (the session-server `join` client) |
| `crates/lodestone-shell/src/menu/accounts.rs` | the device-code and loopback sign-in workers |
| `crates/lodestone-auth/src/{login,migrate,browser_login}.rs` | `#[cfg(test)]` clients |
| `crates/lodestone-auth/tests/device_code_live.rs` | the `#[ignore]`d live-OAuth client |
| `crates/lodestone-auth/tests/tls_provider.rs` | the gate |

## How to change it, and the gotchas

**The failure mode is a runtime panic, not a compile error.** This is the whole
hazard of the arrangement and it deserves restating: `cargo check --workspace
--all-targets`, `--all-features --all-targets`, and the `--no-default-features`
version seam all stay **green** with the install missing or a call site
forgotten. The first symptom would be a panic on the first HTTPS request a player
makes, in the auth path.

What catches it instead:

- `crates/lodestone-auth/tests/tls_provider.rs` — four **non-`#[ignore]`d**,
  zero-egress tests, plus one `#[ignore]`d real handshake. Read that file's
  header for what each one does and does not prove.
- `cargo test -p lodestone-client` is an incidental gate: its `driver.rs` and
  `read_model.rs` tests construct a `Driver`, which unconditionally builds a
  `reqwest::Client`.
- Three `#[tokio::test]`s in `lodestone-auth` (`login.rs` ×2, `migrate.rs`,
  `browser_login.rs`) are the same kind of canary.

Verified negative control (2026-08-04): neutering `install_crypto_provider` with
an early `return` made all four non-ignored gates *and* the handshake gate fail,
exit 101, with reqwest's own message — `No rustls crypto provider is configured`
from `reqwest`'s `default_rustls_crypto_provider` (`reqwest-0.13.4/src/async_impl/client.rs`).

Other traps:

- **Never enable rustls' default features.** They are
  `["aws_lc_rs", "logging", "prefer-post-quantum", "std", "tls12"]`, and
  `prefer-post-quantum` *itself* enables `aws_lc_rs`. Either one, anywhere in the
  graph, drags the whole C build back in through feature unification. This is why
  the workspace entry is `default-features = false`.
- **Do not call `install_default()` at a call site.** It returns `Err` once a
  provider exists, so an `expect` on it is a time bomb in any process that builds
  two clients — and in any test binary that runs more than one test. Go through
  the idempotent helper.
- **Adding a new `reqwest::Client` anywhere requires a call.** Grep
  `reqwest::Client::new()`, `reqwest::Client::builder()` and
  `reqwest::ClientBuilder` before assuming you are done.
- Enabling reqwest's `http3` feature would re-enable `quinn`, whose `quinn-proto`
  depends on `aws-lc-rs` directly. Removing the `rustls` feature already dropped
  `quinn`/`quinn-proto`/`quinn-udp` from the lock as a side effect; turning
  HTTP/3 on would need that edge re-checked.

## Verification (2026-08-04)

- `cargo tree -i aws-lc-sys --workspace` and `cargo tree -i aws-lc-rs
  --workspace` both report `package ID specification … did not match any
  packages`.
- `grep aws-lc Cargo.lock` → no matches. The lock diff was **5 insertions, 124
  deletions**: `aws-lc-rs`, `aws-lc-sys`, its build-only deps `dunce` and
  `fs_extra`, and the `quinn`/`quinn-proto`/`quinn-udp`/`lru-slab`/`rand_pcg`
  cluster.
- No `aws-lc-sys-*` directory regrew in a clean target dir after a full
  `--all-features --all-targets` check (`ring-*` fingerprints are present, which
  is the control that the search would have found something).
- `just wasm-check` verdict **unchanged** — the same two pre-existing failures
  (`lodestone-v770`, `lodestone-web`, both `getrandom 0.2` without `js`).

## Configuration

None at runtime. The provider is a compile-time choice expressed entirely in
`Cargo.toml` feature lists plus the one `rustls::crypto::ring` reference in
`crates/lodestone-auth/src/tls.rs`. To swap providers, change both.

## Dependencies

- `rustls` 0.23 with `default-features = false, features = ["ring", "std",
  "tls12"]` — `std`/`tls12` mirror what reqwest itself requests, so `ring` in
  place of `aws-lc-rs` is the only delta.
- `ring` 0.17, pulled in by that feature; it was already in `Cargo.lock` before
  this change.
- `reqwest` 0.13 with `rustls-no-provider`; `rustls-platform-verifier` 0.7 comes
  along with it and supplies OS trust-store verification.
