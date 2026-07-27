//! Online-mode authentication for Lodestone.
//!
//! This crate turns a Microsoft account into everything the encryption
//! handshake needs to join an online-mode vanilla server:
//!
//! * [`server_hash`] — the non-standard SHA-1 the client sends to the session
//!   server (pure, fully tested against Mojang's published vectors);
//! * the Microsoft device-code OAuth flow and the Xbox Live → XSTS → Minecraft
//!   services token chain ([`flow`]);
//! * the session-server [`join_server`] call that proves ownership of the shared
//!   secret;
//! * an on-disk refresh-token [`cache`] (native only).
//!
//! The actual cipher (AES-128-CFB8), shared-secret generation and RSA wrapping
//! of the secret live in `lodestone-net`, because they sit in the sans-IO codec
//! so every transport (including the browser) inherits them. This crate is
//! purely the *identity* half: who you are and how the session server is told.
//!
//! ## What is and isn't verified
//!
//! [`server_hash`] is checked against externally-reproduced vectors. The token
//! chain cannot be exercised without a real Microsoft account, so it is written
//! to the documented protocol but is unverified end-to-end; its tests cover only
//! the JSON shapes it parses.

mod error;
mod hash;

pub use error::{AuthError, Result};
pub use hash::server_hash;

#[cfg(not(target_arch = "wasm32"))]
pub mod cache;
#[cfg(not(target_arch = "wasm32"))]
pub mod flow;

#[cfg(not(target_arch = "wasm32"))]
pub use flow::{
    DeviceCodePrompt, MOJANG_CLIENT_ID, MsToken, PendingLogin, Profile, Session,
    authenticate_with_device_code, join_server, poll_token, refresh_token, request_device_code,
    session_from_ms_token,
};
