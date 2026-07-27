//! Shared helpers for `lodestone-v770`'s live integration tests.
//!
//! This is compiled as an inline module (`mod common;`) into each test binary,
//! not as a separate crate, so it adds no Cargo dependency edge and keeps
//! `cargo xtask check-isolation` green. A sibling copy lives in
//! `lodestone-client`'s tests for that crate's own live joins; duplicating a
//! three-line helper across two test suites is cheaper than a shared
//! test-support crate that would create a real dependency edge.
pub use lodestone_testsupport::unique_username;
