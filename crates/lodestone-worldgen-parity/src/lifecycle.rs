//! Compatibility re-export for the shared world-generation lifecycle core.
//!
//! Capture authentication and manifest parsing remain in the parity harness;
//! the reusable materializer and production source adapters live with the
//! server that owns the `ChunkSource` boundary.

pub use lodestone_server::worldgen_lifecycle::*;
