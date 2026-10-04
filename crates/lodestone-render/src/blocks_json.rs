//! Typed `blocks.json` report ingestion. [`BlocksJsonRegistry`] preserves report
//! IDs for report consumers; [`BlocksJsonRegistry::into_canonical`] moves exact
//! built-in identities into the bounded registry used for runtime model baking.

use std::collections::{BTreeMap, BTreeSet};
use std::marker::PhantomData;

use lodestone_data::block_states::{STATE_COUNT, StateId};
use lodestone_model::{BlockStateRegistry, Identifier, ResolvedBlockState};
use serde::de::{MapAccess, Visitor};
use serde::{Deserialize, Deserializer};

type StateEntry = (Identifier, BTreeMap<String, String>);

struct UniqueMap<T>(BTreeMap<String, T>);

impl<T> Default for UniqueMap<T> {
    fn default() -> Self {
        Self(BTreeMap::new())
    }
}

impl<'de, T: Deserialize<'de>> Deserialize<'de> for UniqueMap<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct MapVisitor<T>(PhantomData<T>);

        impl<'de, T: Deserialize<'de>> Visitor<'de> for MapVisitor<T> {
            type Value = UniqueMap<T>;

            fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                formatter.write_str("an object with unique keys")
            }

            fn visit_map<M: MapAccess<'de>>(self, mut input: M) -> Result<Self::Value, M::Error> {
                let mut values = BTreeMap::new();
                while let Some((key, value)) = input.next_entry::<String, T>()? {
                    if values.insert(key.clone(), value).is_some() {
                        return Err(serde::de::Error::custom(format!("duplicate key {key:?}")));
                    }
                }
                Ok(UniqueMap(values))
            }
        }

        deserializer.deserialize_map(MapVisitor(PhantomData))
    }
}

#[derive(Deserialize)]
struct BlockDocument {
    states: Vec<StateDocument>,
}

#[derive(Deserialize)]
struct StateDocument {
    id: u32,
    #[serde(default)]
    properties: UniqueMap<String>,
    #[serde(default, rename = "default")]
    _default: Option<bool>,
}

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

/// A parsed report registry indexed by the report's own numeric state IDs.
///
/// Sparse storage scales with actual entries, but `state_count` remains the
/// maximum report ID plus one. Raw consumers that scan that range inherit its
/// size; runtime bakers should consume [`Self::into_canonical`] instead.
#[derive(Debug)]
pub struct BlocksJsonRegistry {
    entries: BTreeMap<u32, StateEntry>,
    state_count: u32,
}

/// Report entries indexed only by this build's canonical state census.
#[derive(Debug)]
pub struct CanonicalBlocksJsonRegistry {
    entries: Vec<Option<StateEntry>>,
    report: CanonicalizationReport,
}

impl CanonicalBlocksJsonRegistry {
    /// How the report's identities mapped onto this build's canonical census.
    #[must_use]
    pub fn report(&self) -> &CanonicalizationReport {
        &self.report
    }
}

/// Cold-load accounting for identities accepted or left unsupported.
#[derive(Debug, Default, Clone)]
pub struct CanonicalizationReport {
    /// Number of distinct states supplied by the report.
    pub input_states: usize,
    /// Number of states matched by exact canonical identity.
    pub matched_states: usize,
    /// Number of states outside this build's canonical census.
    pub unsupported_states: usize,
    /// At most four unsupported identities in report-ID order.
    pub unsupported_examples: Vec<String>,
}

impl BlocksJsonRegistry {
    /// Parses a registry from the raw bytes of a `blocks.json` report.
    ///
    /// Requires namespaced block names, unsigned IDs, and string property
    /// values. Duplicate object keys, IDs, or identities and an empty report
    /// are rejected. Report IDs retain their original meaning.
    ///
    /// # Errors
    /// Returns [`BlocksJsonError::Json`] if the bytes are not valid JSON, or
    /// [`BlocksJsonError::Malformed`] if the JSON is not a blocks report.
    pub fn from_slice(bytes: &[u8]) -> Result<Self, BlocksJsonError> {
        let document: UniqueMap<BlockDocument> = serde_json::from_slice(bytes).map_err(|error| {
            if error.is_data() {
                BlocksJsonError::Malformed(error.to_string())
            } else {
                BlocksJsonError::Json(error)
            }
        })?;
        let mut entries = BTreeMap::new();
        for (name, block) in document.0 {
            let (namespace, path) = name.split_once(':')
                .ok_or_else(|| BlocksJsonError::Malformed(format!("block name is not namespaced: {name:?}")))?;
            let identifier = Identifier::new_borrowed(namespace, path)
                .map_err(|_| BlocksJsonError::Malformed(format!("bad block name {name:?}")))?;
            if block.states.is_empty() {
                return Err(BlocksJsonError::Malformed(format!("block {name:?} has no states")));
            }
            let mut identities = BTreeSet::new();
            for state in &block.states {
                if !identities.insert(&state.properties.0) {
                    return Err(BlocksJsonError::Malformed(format!("duplicate state identity for {name:?}")));
                }
            }
            drop(identities);
            for state in block.states {
                if entries.insert(state.id, (identifier.clone(), state.properties.0)).is_some() {
                    return Err(BlocksJsonError::Malformed(format!("duplicate state id {}", state.id)));
                }
            }
        }
        let max_id = entries.last_key_value().map(|(&id, _)| id)
            .ok_or_else(|| BlocksJsonError::Malformed("report contained no block states".into()))?;
        let state_count = max_id.checked_add(1)
            .ok_or_else(|| BlocksJsonError::Malformed("maximum state id overflows state_count".into()))?;
        Ok(Self { entries, state_count })
    }

    /// Moves exact built-in identities into a fixed canonical state-ID vector.
    ///
    /// Unsupported identities leave no entry, regardless of their report IDs.
    /// Missing canonical identities remain holes; no content fallback applies.
    #[must_use]
    pub fn into_canonical(self) -> (CanonicalBlocksJsonRegistry, CanonicalizationReport) {
        let mut entries = vec![None; STATE_COUNT as usize];
        let mut report = CanonicalizationReport {
            input_states: self.entries.len(),
            ..CanonicalizationReport::default()
        };
        for (block, properties) in self.entries.into_values() {
            if let Some(state) = StateId::from_exact_parts(&block.to_string(), &properties) {
                entries[state.index()] = Some((block, properties));
                report.matched_states += 1;
            } else {
                report.unsupported_states += 1;
                if report.unsupported_examples.len() < 4 {
                    let mut example = block.to_string();
                    if !properties.is_empty() {
                        example.push('[');
                        for (index, (key, value)) in properties.iter().enumerate() {
                            if index != 0 {
                                example.push(',');
                            }
                            example.push_str(key);
                            example.push('=');
                            example.push_str(value);
                        }
                        example.push(']');
                    }
                    report.unsupported_examples.push(example);
                }
            }
        }
        (CanonicalBlocksJsonRegistry { entries, report: report.clone() }, report)
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
        let (block, properties) = self.entries.get(&id)?;
        Some(ResolvedBlockState { block, properties })
    }

    fn state_count(&self) -> u32 {
        self.state_count
    }
}

impl BlockStateRegistry for CanonicalBlocksJsonRegistry {
    fn resolve(&self, id: u32) -> Option<ResolvedBlockState<'_>> {
        let (block, properties) = self.entries.get(id as usize)?.as_ref()?;
        Some(ResolvedBlockState { block, properties })
    }

    fn state_count(&self) -> u32 {
        STATE_COUNT
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

    // Identities and numeric IDs captured from the official 26.3 blocks report.
    const SHIFTED_SAMPLE: &[u8] = br#"{
        "minecraft:water": {"states": [{"id":89,"properties":{"level":"0"}}]},
        "minecraft:oak_log": {"states": [{"id":140,"properties":{"axis":"y"}}]},
        "minecraft:acacia_button": {"states": [{"id":12514,"properties":{
            "powered":"true","facing":"north","face":"floor"
        }}]},
        "minecraft:poplar_planks": {"states": [{"id":27}]},
        "minecraft:bamboo_planks": {"states": [{"id":28}]},
        "minecraft:unreleased_future_block": {"states": [{"id":99999}]}
    }"#;

    fn has_official_canonical_witnesses(registry: &impl BlockStateRegistry) -> bool {
        let witnesses: &[(u32, &str, &[(&str, &str)])] = &[
            (27, "minecraft:bamboo_planks", &[]),
            (86, "minecraft:water", &[("level", "0")]),
            (137, "minecraft:oak_log", &[("axis", "y")]),
            (10771, "minecraft:acacia_button", &[
                ("face", "floor"), ("facing", "north"), ("powered", "true")
            ]),
        ];
        witnesses.iter().all(|&(id, name, properties)| {
            registry.resolve(id).is_some_and(|resolved| {
                resolved.block.to_string() == name
                    && resolved.properties.len() == properties.len()
                    && properties.iter().all(|&(key, value)| {
                        resolved.properties.get(key).map(String::as_str) == Some(value)
                    })
            })
        })
    }

    #[test]
    fn canonical_ingress_remaps_official_witnesses_without_aliasing_new_content() {
        let raw = BlocksJsonRegistry::from_slice(SHIFTED_SAMPLE).expect("parse shifted report");
        assert!(!has_official_canonical_witnesses(&raw), "raw report must fail the canonical detector");
        assert_eq!(raw.resolve(27).unwrap().block.to_string(), "minecraft:poplar_planks");
        assert_eq!(raw.resolve(89).unwrap().properties.get("level").map(String::as_str), Some("0"));

        let (canonical, report) = raw.into_canonical();
        assert!(has_official_canonical_witnesses(&canonical));
        // The canonical census is the append-only union of the 26.2 and 26.3
        // block-state tables: 35,723 states, with the 26.2 ids unchanged.
        assert_eq!(canonical.state_count(), 35723);
        assert_eq!(report.input_states, 6);
        assert_eq!(report.matched_states, 5);
        assert_eq!(report.unsupported_states, 1);
        assert_eq!(report.unsupported_examples, ["minecraft:unreleased_future_block"]);
        assert!(canonical.resolve(28).is_none(), "report slot 28 must not retain bamboo");
        assert!(canonical.resolve(35723).is_none());
        // A 26.3-only block lands after the 26.2 prefix (32,366 states) and
        // never displaces a 26.2 slot, whatever id the report gave it.
        let poplar = (0..35723u32)
            .find(|&id| canonical.resolve(id).is_some_and(|s| s.block.to_string() == "minecraft:poplar_planks"))
            .expect("poplar planks is canonical");
        assert!(poplar >= 32366, "26.3-only content is appended, got slot {poplar}");
        assert!(canonical.resolve(27).is_some_and(|s| s.block.to_string() == "minecraft:bamboo_planks"));

        let sorted: serde_json::Value = serde_json::from_slice(SHIFTED_SAMPLE).unwrap();
        let reordered = serde_json::to_vec(&sorted).unwrap();
        assert_ne!(reordered.as_slice(), SHIFTED_SAMPLE);
        let (permuted, reordered_report) = BlocksJsonRegistry::from_slice(&reordered).unwrap().into_canonical();
        for id in 0..STATE_COUNT {
            let identity = |registry: &CanonicalBlocksJsonRegistry| {
                registry.resolve(id).map(|state| (state.block.clone(), state.properties.clone()))
            };
            assert_eq!(identity(&canonical), identity(&permuted), "canonical slot {id}");
        }
        assert_eq!(reordered_report.unsupported_examples, report.unsupported_examples);
    }

    #[test]
    fn malformed_fields_and_duplicate_identities_are_rejected() {
        for input in [
            r#"{"stone":{"states":[{"id":1}]}}"#,
            r#"{"minecraft:stone":null}"#,
            r#"{"minecraft:stone":{}}"#,
            r#"{"minecraft:stone":{"states":{}}}"#,
            r#"{"minecraft:stone":{"states":[{}]}}"#,
            r#"{"minecraft:stone":{"states":[{"id":-1}]}}"#,
            r#"{"minecraft:stone":{"states":[{"id":1.5}]}}"#,
            r#"{"minecraft:stone":{"states":[{"id":"1"}]}}"#,
            r#"{"minecraft:stone":{"states":[{"id":4294967296}]}}"#,
            r#"{"minecraft:stone":{"states":[{"id":4294967295}]}}"#,
            r#"{"minecraft:stone":{"states":[{"id":1,"properties":null}]}}"#,
            r#"{"minecraft:stone":{"states":[{"id":1,"properties":[]}]}}"#,
            r#"{"minecraft:oak_log":{"states":[{"id":1,"properties":{"axis":true}}]}}"#,
            r#"{"minecraft:oak_log":{"states":[{"id":1,"properties":{"axis":2}}]}}"#,
            r#"{"minecraft:oak_log":{"states":[{"id":1,"properties":{"axis":"x","axis":"y"}}]}}"#,
            r#"{"minecraft:stone":{"states":[{"id":1}]},"minecraft:dirt":{"states":[{"id":1}]}}"#,
            r#"{"minecraft:stone":{"states":[{"id":1},{"id":2}]}}"#,
            r#"{"minecraft:stone":{"states":[{"id":1}]},"minecraft:stone":{"states":[{"id":2}]}}"#,
        ] {
            assert!(
                matches!(BlocksJsonRegistry::from_slice(input.as_bytes()), Err(BlocksJsonError::Malformed(_))),
                "accepted malformed report: {input}"
            );
        }
    }

    #[test]
    fn sparse_report_storage_and_unsupported_examples_stay_bounded() {
        let raw = BlocksJsonRegistry::from_slice(
            br#"{"minecraft:stone":{"states":[{"id":4294967294}]}}"#
        ).unwrap();
        assert_eq!(raw.state_count(), u32::MAX);
        assert_eq!(raw.resolve(u32::MAX - 1).unwrap().block.to_string(), "minecraft:stone");
        let (canonical, report) = raw.into_canonical();
        assert_eq!(canonical.state_count(), STATE_COUNT);
        assert_eq!(canonical.resolve(1).unwrap().block.to_string(), "minecraft:stone");
        assert_eq!(report.matched_states, 1);

        let (canonical, report) = BlocksJsonRegistry::from_slice(br#"{
            "minecraft:oak_log":{"states":[
                {"id":0},{"id":1,"properties":{"axis":"invalid"}},
                {"id":2,"properties":{"axis":"y","snowy":"false"}}
            ]},
            "other:stone":{"states":[{"id":3}]},
            "minecraft:poplar_planks":{"states":[{"id":4}]}
        }"#).unwrap().into_canonical();
        assert_eq!(report.input_states, 5);
        // Only the 26.3-only poplar planks is a canonical identity; the invalid
        // axis, the extra property and the foreign namespace are not.
        assert_eq!(report.matched_states, 1);
        assert_eq!(report.unsupported_states, 4);
        assert_eq!(report.unsupported_examples.len(), 4);
        assert!(canonical.resolve(137).is_none());
        assert!(canonical.resolve(27).is_none());
    }

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
