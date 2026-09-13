//! Shared helpers for `lodestone-client`'s live integration tests.
//!
//! This is compiled as an inline module (`mod common;`) into each test binary,
//! not as a separate crate, so it adds no Cargo dependency edge and keeps
//! `cargo xtask check-isolation` green.
pub use lodestone_testsupport::unique_username;
