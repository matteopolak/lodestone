//! A version-free [`BlockStateRegistry`] parsed from Mojang's data-generator
//! `blocks.json` report.
//!
//! This is the seam a host (e.g. the shell) uses to obtain a registry *without*
//! reaching around the library into a version crate. Given the bytes or path of
//! a `blocks.json` (as shipped next to a fetched `client.jar`), it yields the
//! **real vanilla global state ids** that both [`BlockAtlas::build`] and
//! [`BlockAtlas::state_id_of`] index. Loading is fallible on purpose so callers
//! keep a loud fallback when the report is absent — a missing registry must
//! never silently degrade to a plausible-but-wrong id space.
//!
//! [`BlockAtlas::build`]: crate::BlockAtlas::build
//! [`BlockAtlas::state_id_of`]: crate::BlockAtlas::state_id_of

use std::collections::BTreeMap;

use lodestone_model::{BlockStateRegistry, Identifier, ResolvedBlockState};

/// Why loading a `blocks.json` registry failed. Every variant names the fix so a
/// caller's fallback banner can be actionable rather than a bare `None`.
#[derive(Debug, thiserror::Error)]
pub enum BlocksJsonError {
    /// The report file could not be read from disk.
    #[error("could not read blocks.json at {path}: {source}")]
    Read {
        /// The path that was attempted.
        path: String,
        /// The underlying I/O error.
        source: std::io::Error,
    },
    /// The bytes were not valid JSON.
    #[error("blocks.json is not valid JSON: {0}")]
    Json(#[from] serde_json::Error),
    /// The JSON parsed but is not shaped like a vanilla blocks report (or it
    /// carried a malformed block name / state id). The message names the
    /// offending fragment.
    #[error("blocks.json is not a vanilla blocks report: {0}")]
    Malformed(String),
}

/// A [`BlockStateRegistry`] backed by a parsed `blocks.json`.
///
/// Reverse lookup only — `id -> (name, properties)` — matching the model's
/// [`BlockStateRegistry`] contract. The forward direction (`name -> id`) lives
/// on [`BlockAtlas`](crate::BlockAtlas), which inverts this at build time.
#[derive(Debug)]
pub struct BlocksJsonRegistry {
    /// Indexed by global state id; `None` marks an id no block claims (holes are
    /// possible if the report is sparse, though vanilla's is dense).
    entries: Vec<Option<(Identifier, BTreeMap<String, String>)>>,
}

impl BlocksJsonRegistry {
    /// Parses a registry from the raw bytes of a `blocks.json` report.
    ///
    /// Fails **closed**: any structural surprise — a non-object root, a bad
    /// block name, a state without an integer id, or a report with no states at
    /// all — is a [`BlocksJsonError`], never a silently-empty registry.
    ///
    /// # Errors
    /// Returns [`BlocksJsonError::Json`] if the bytes are not valid JSON, or
    /// [`BlocksJsonError::Malformed`] if the JSON is not a blocks report.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, BlocksJsonError> {
        let root: serde_json::Value = serde_json::from_slice(bytes)?;
        let obj = root
            .as_object()
            .ok_or_else(|| BlocksJsonError::Malformed("top level is not an object".into()))?;

        let mut states: Vec<(u32, Identifier, BTreeMap<String, String>)> = Vec::new();
        let mut max_id = 0u32;
        for (name, block) in obj {
            let id: Identifier = name
                .parse()
                .map_err(|_| BlocksJsonError::Malformed(format!("bad block name {name:?}")))?;
            let Some(arr) = block.get("states").and_then(|s| s.as_array()) else {
                continue;
            };
            for state in arr {
                let sid = state
                    .get("id")
                    .and_then(serde_json::Value::as_u64)
                    .ok_or_else(|| {
                        BlocksJsonError::Malformed(format!("a state of {name:?} has no integer id"))
                    })? as u32;
                let mut props = BTreeMap::new();
                if let Some(p) = state.get("properties").and_then(|p| p.as_object()) {
                    for (k, v) in p {
                        if let Some(v) = v.as_str() {
                            props.insert(k.clone(), v.to_string());
                        }
                    }
                }
                max_id = max_id.max(sid);
                states.push((sid, id.clone(), props));
            }
        }

        if states.is_empty() {
            return Err(BlocksJsonError::Malformed(
                "report contained no block states".into(),
            ));
        }

        let mut entries = vec![None; max_id as usize + 1];
        for (sid, id, props) in states {
            entries[sid as usize] = Some((id, props));
        }
        Ok(Self { entries })
    }
}

/// Native-only disk loader, confined to its own wholly-gated file so `std::fs`
/// cannot leak onto the wasm path. Re-exported below on non-wasm targets.
#[cfg(not(target_arch = "wasm32"))]
#[path = "blocks_json_native.rs"]
mod native;
#[cfg(not(target_arch = "wasm32"))]
pub use native::blocks_json_registry;

impl BlockStateRegistry for BlocksJsonRegistry {
    fn resolve(&self, id: u32) -> Option<ResolvedBlockState<'_>> {
        let (block, properties) = self.entries.get(id as usize)?.as_ref()?;
        Some(ResolvedBlockState { block, properties })
    }

    fn state_count(&self) -> u32 {
        self.entries.len() as u32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A miniature but structurally faithful `blocks.json`: sparse property sets,
    /// a multi-state block whose `default` is *not* the lowest id, and extra
    /// fields (`default`) the parser must tolerate and ignore.
    const SAMPLE: &[u8] = br#"{
        "minecraft:air":   { "states": [ { "id": 0, "default": true } ] },
        "minecraft:stone": { "states": [ { "id": 1, "default": true } ] },
        "minecraft:oak_log": {
            "states": [
                { "id": 2, "properties": { "axis": "x" } },
                { "id": 3, "properties": { "axis": "y" }, "default": true },
                { "id": 4, "properties": { "axis": "z" } }
            ]
        }
    }"#;

    #[test]
    fn from_slice_indexes_states_by_real_global_id() {
        let reg = BlocksJsonRegistry::from_slice(SAMPLE).expect("parse sample report");
        assert_eq!(reg.state_count(), 5, "ids 0..=4 span five slots");

        let air = reg.resolve(0).expect("air resolves");
        assert_eq!(air.block.to_string(), "minecraft:air");
        assert!(air.properties.is_empty(), "air has no properties");

        // The id is the report's own `id`, not positional — oak_log[axis=y] is 3.
        let oak_y = reg.resolve(3).expect("oak_log[axis=y] resolves");
        assert_eq!(oak_y.block.to_string(), "minecraft:oak_log");
        assert_eq!(oak_y.properties.get("axis").map(String::as_str), Some("y"));
    }

    #[test]
    fn malformed_reports_fail_closed_rather_than_empty() {
        assert!(
            matches!(
                BlocksJsonRegistry::from_slice(b"not json at all"),
                Err(BlocksJsonError::Json(_))
            ),
            "non-JSON bytes are a Json error"
        );
        assert!(
            matches!(
                BlocksJsonRegistry::from_slice(b"[]"),
                Err(BlocksJsonError::Malformed(_))
            ),
            "a JSON array is not a blocks report"
        );
        assert!(
            matches!(
                BlocksJsonRegistry::from_slice(br#"{"minecraft:stone":{"states":[]}}"#),
                Err(BlocksJsonError::Malformed(_))
            ),
            "a report with zero states is malformed, not a silently-empty registry"
        );
    }
}
