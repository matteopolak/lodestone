//! Native-only filesystem loader for [`BlocksJsonRegistry`].
//!
//! Confined to its own wholly-gated file so `std::fs` cannot leak onto the wasm
//! path, matching the `frame_native.rs` convention. Parsing itself lives in
//! [`BlocksJsonRegistry::from_slice`], which is platform-free — a wasm caller
//! fetches the bytes however it likes and parses them with that.

use std::path::Path;

use crate::blocks_json::{BlocksJsonError, BlocksJsonRegistry, CanonicalBlocksJsonRegistry};

/// Loads a `blocks.json` file from disk and canonicalizes its identities, so the
/// state ids it hands out are the ids every table and model in this build is
/// keyed by. Parse the report with [`BlocksJsonRegistry::from_slice`] directly
/// to keep the report's own ids.
///
/// # Errors
/// Returns [`BlocksJsonError::Read`] if the file cannot be read, or a parse
/// error from [`BlocksJsonRegistry::from_slice`].
pub fn blocks_json_registry(path: &Path) -> Result<CanonicalBlocksJsonRegistry, BlocksJsonError> {
    let bytes = std::fs::read(path).map_err(|source| BlocksJsonError::Read {
        path: path.display().to_string(),
        source,
    })?;
    Ok(BlocksJsonRegistry::from_slice(&bytes)?.into_canonical().0)
}
