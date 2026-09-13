//! Browser-only asset helpers with host-runnable validation tests.
//!
//! The executable remains the page bootstrap in `main.rs`; this small library
//! exists so manifest validation can run as ordinary native unit tests without
//! trying to link the browser-only asset installation path.

pub mod client_jar;
