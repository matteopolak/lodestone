//! Non-hermetic live check of the Microsoft device-code request shape.
//!
//! `#[ignore]`d because it talks to Microsoft's real OAuth servers. Run with:
//! `cargo test -p lodestone-auth --test device_code_live -- --ignored --nocapture`.
//!
//! We have no Microsoft account and no registered Azure application, so a full
//! device-code login cannot complete. But the *request shape* can still be
//! validated against an external authority we did not write — Microsoft's own
//! server:
//!
//! * With the default (public, but unregistered-on-the-v2-consumers-tenant)
//!   Mojang client id, a **correctly formed** request is answered with
//!   `unauthorized_client` (AADSTS700016 — "application not found"), whereas a
//!   **malformed** request is answered with `invalid_request` (AADSTS900144).
//!   Reaching `unauthorized_client` therefore proves our URL, method, headers,
//!   content type and form encoding are all correct and that we got as far as
//!   Microsoft looking up the client id — the chain stops precisely at
//!   client-id registration, which needs an Azure tenant we don't have.
//!
//! * If you *do* have a registered public client id, pass it via
//!   `LODESTONE_MS_CLIENT_ID` and the test instead asserts a real device code is
//!   issued and that an immediate poll returns "authorization pending".

use lodestone_auth::{AuthError, MOJANG_CLIENT_ID, PendingLogin, request_device_code};

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .user_agent("lodestone-auth-test")
        .build()
        .expect("build reqwest client")
}

#[tokio::test]
#[ignore = "hits Microsoft's live OAuth servers"]
async fn device_code_request_shape_is_accepted_by_microsoft() {
    let http = client();

    match std::env::var("LODESTONE_MS_CLIENT_ID") {
        Ok(client_id) if !client_id.is_empty() => {
            // Real registered client id: expect a genuine device code, then an
            // immediate poll should say "still pending" (nobody has authorized).
            let mut pending = PendingLogin::begin(&http, &client_id)
                .await
                .expect("begin device-code login with a registered client id");
            let prompt = pending.prompt();
            eprintln!(
                "user_code={} verification_uri={} interval={}s expires_in={}s",
                prompt.user_code,
                prompt.verification_uri,
                prompt.interval(),
                prompt.expires_in()
            );
            assert!(!prompt.user_code.is_empty(), "expected a user code");
            assert!(
                prompt.verification_uri.starts_with("https://"),
                "expected an https verification uri"
            );
            let polled = pending
                .poll_once(&http, &client_id)
                .await
                .expect("first poll should not hard-error");
            assert!(
                polled.is_none(),
                "expected authorization to still be pending on the first poll"
            );
        }
        _ => {
            // Default path: prove the request is well-formed by reaching the
            // client-id-specific rejection rather than a format rejection.
            let err = request_device_code(&http, MOJANG_CLIENT_ID)
                .await
                .expect_err("the unregistered Mojang client id must be rejected");
            let AuthError::Service { step, message } = &err else {
                panic!("expected a structured OAuth service error, got: {err:?}");
            };
            assert_eq!(*step, "device_code");
            eprintln!("device-code endpoint returned: {message}");
            assert!(
                message.contains("unauthorized_client"),
                "expected `unauthorized_client` (proving the request shape was accepted and only \
                 the client id was rejected), got: {message}"
            );
            assert!(
                !message.contains("invalid_request"),
                "a malformed request would return `invalid_request`; we must not see it"
            );
        }
    }
}
