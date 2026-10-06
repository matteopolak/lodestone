//! A [`Resolver`] driven entirely by a static, id-sorted table of embedded
//! JSON text, plus an optional table of binary structure-template bytes.
//!
//! # Why this exists
//!
//! Every embedding site that wants real terrain (not a hand-rolled test
//! fixture) ends up writing the same table plus private resolver: a `build.rs`
//! that walks
//! `assets/worldgen/` into a sorted `&'static [(&str, &str)]` table keyed by
//! path-with-extension-stripped, and a private `Resolver` impl that does
//! `strip_prefix("minecraft:")` + `binary_search_by` + `serde_json::from_str`
//! for every category (`density_function/`, `noise/`, `biome/`,
//! `configured_feature/`, `placed_feature/`,
//! `tags/block/`, `structure_set/`, `structure/`, `tags/worldgen/biome/`,
//! `template_pool/`, `processor_list/`). `lodestone-server` uses this type for
//! its live bundled 26.2 data, so a future embedding site reuses the production
//! lookup path rather than copying it. This type is the shared half: supply a
//! table, get a full [`Resolver`].
//!
//! The `build.rs` directory-scan itself still belongs to each embedding
//! crate (it needs `OUT_DIR`, which build-time codegen shared across crates
//! cannot cleanly express) — only the *lookup* logic is shared here.
//!
//! # Id scheme
//!
//! Table entries are keyed exactly as `lodestone-server`'s `build.rs` derives
//! them: the file's path under `assets/worldgen/`, forward-slashed,
//! extension stripped — e.g. `"density_function/overworld/final_density"`,
//! `"noise/continentalness"`, `"biome/plains"`, `"structure_set/villages"`.
//! This mirrors vanilla's own `data/minecraft/worldgen/...` layout, so a
//! second embedder that copies vanilla's directory structure verbatim needs
//! no translation step.
//!
//! # What is NOT covered
//!
//! [`Resolver::block_freeze_facts`] and [`Resolver::block_survival_facts`] are not JSON lookups: they are censuses of
//! the game's *compiled* behaviour (collision, fluid state), sourced from
//! `lodestone_data::{block_solidity, snow_support}`, not from a JSON asset —
//! and this crate must stay version-free, so it cannot depend on
//! `lodestone-data`. An embedder that wants it supplies a census factory to
//! [`TableResolver::with_block_freeze_facts`] and
//! [`TableResolver::with_block_survival_facts`].

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::density::{NoiseParams, Resolver};

/// See the [module docs](self).
#[derive(Debug, Clone)]
pub struct TableResolver<'a> {
    json: &'a [(&'a str, &'a str)],
    structure_templates: &'a [(&'a str, &'a [u8])],
    block_freeze_facts: Option<fn() -> &'static Value>,
    block_survival_facts: Option<fn() -> &'static Value>,
    json_cache: Option<Arc<JsonCache>>,
}

/// Parsed documents shared by resolvers over one immutable asset table.
///
/// The cache is intentionally opt-in: arbitrary datapack resolvers remain
/// uncached, while an embedder can retain parsed documents across generators.
/// The fingerprint prevents a cache built for one asset bundle from being
/// attached to another bundle by mistake.
#[derive(Debug)]
pub struct JsonCache {
    fingerprint: u64,
    documents: Mutex<HashMap<String, Value>>,
}

impl<'a> TableResolver<'a> {
    /// Builds a resolver over `json` (sorted by id — see the [module docs](self))
    /// with no structure templates. Use
    /// [`with_structure_templates`](Self::with_structure_templates) to add
    /// them.
    #[must_use]
    pub const fn new(json: &'a [(&'a str, &'a str)]) -> Self {
        Self {
            json,
            structure_templates: &[],
            block_freeze_facts: None,
            block_survival_facts: None,
            json_cache: None,
        }
    }

    /// Creates an empty parsed-document cache tied to this resolver's assets.
    #[must_use]
    pub fn json_cache(&self) -> Arc<JsonCache> {
        Arc::new(JsonCache {
            fingerprint: self.fingerprint(),
            documents: Mutex::new(HashMap::new()),
        })
    }

    /// Reuses parsed documents from a resolver over the same immutable assets.
    ///
    /// A cache made from a different JSON or template table is ignored, which
    /// leaves this resolver on its uncached fallback path.
    #[must_use]
    pub fn with_json_cache(mut self, cache: Arc<JsonCache>) -> Self {
        if cache.fingerprint == self.fingerprint() {
            self.json_cache = Some(cache);
        }
        self
    }

    /// Stable within the process and sensitive to every embedded asset byte.
    /// Dynamic resolvers do not expose this value and therefore do not enter
    /// any shared production cache.
    #[must_use]
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        self.json.len().hash(&mut hasher);
        for &(key, value) in self.json {
            key.hash(&mut hasher);
            value.hash(&mut hasher);
        }
        self.structure_templates.len().hash(&mut hasher);
        for &(key, value) in self.structure_templates {
            key.hash(&mut hasher);
            value.hash(&mut hasher);
        }
        hasher.finish()
    }

    /// Attaches a table of raw `structure/<path>.nbt` bytes (sorted by id,
    /// `minecraft:` prefix stripped — e.g. `"shipwreck/with_mast"`), served
    /// by [`Resolver::structure_template`]. Without this, every
    /// template-driven structure demotes to `Unsupported`.
    #[must_use]
    pub const fn with_structure_templates(mut self, templates: &'a [(&'a str, &'a [u8])]) -> Self {
        self.structure_templates = templates;
        self
    }

    /// Supplies the version-specific block-state census required by
    /// [`Resolver::block_freeze_facts`]. The table itself stays version-free:
    /// only the embedding crate knows how to build this document.
    #[must_use]
    pub const fn with_block_freeze_facts(mut self, facts: fn() -> &'static Value) -> Self {
        self.block_freeze_facts = Some(facts);
        self
    }

    /// Supplies the version-specific census required by
    /// [`Resolver::block_survival_facts`].
    #[must_use]
    pub const fn with_block_survival_facts(mut self, facts: fn() -> &'static Value) -> Self {
        self.block_survival_facts = Some(facts);
        self
    }

    /// Parses one required document by its table key. Embedders use this for
    /// non-`Resolver` documents such as `noise_settings/*` and `world_preset/*`.
    /// A missing document is an embedded-data bug and panics naming the key.
    #[must_use]
    pub fn document(&self, key: &str) -> Value {
        self.json_at(key)
    }

    /// Looks up `key` in the JSON table, panicking if absent. For the two
    /// fields [`Resolver`] requires rather than defaults
    /// (`density_function`, `noise`) — a missing required entry is a data
    /// bug in the embedded bundle, not a "no data supplied" case.
    fn raw(&self, key: &str) -> &'a str {
        self.json
            .binary_search_by(|(id, _)| (*id).cmp(key))
            .map(|i| self.json[i].1)
            .unwrap_or_else(|_| panic!("embedded worldgen table missing '{key}'"))
    }

    fn json_at(&self, key: &str) -> Value {
        if let Some(cache) = &self.json_cache {
            let mut documents = cache.documents.lock().expect("json cache lock poisoned");
            return documents
                .entry(key.to_owned())
                .or_insert_with(|| {
                    serde_json::from_str(self.raw(key))
                        .unwrap_or_else(|e| panic!("parsing embedded '{key}': {e}"))
                })
                .clone();
        }
        serde_json::from_str(self.raw(key))
            .unwrap_or_else(|e| panic!("parsing embedded '{key}': {e}"))
    }

    /// Like [`Self::raw`], but a missing key returns `None` — the
    /// "no data supplied" convention every optional [`Resolver`] method
    /// documents (see `crate::density::Resolver`'s trait docs).
    fn try_raw(&self, key: &str) -> Option<&'a str> {
        self.json
            .binary_search_by(|(id, _)| (*id).cmp(key))
            .ok()
            .map(|i| self.json[i].1)
    }

    fn try_json(&self, key: &str) -> Value {
        if let Some(cache) = &self.json_cache {
            let mut documents = cache.documents.lock().expect("json cache lock poisoned");
            return documents
                .entry(key.to_owned())
                .or_insert_with(|| {
                    self.try_raw(key)
                        .map(|raw| {
                            serde_json::from_str(raw)
                                .unwrap_or_else(|e| panic!("parsing embedded '{key}': {e}"))
                        })
                        .unwrap_or(Value::Null)
                })
                .clone();
        }
        self.try_raw(key).map_or(Value::Null, |raw| {
            serde_json::from_str(raw).unwrap_or_else(|e| panic!("parsing embedded '{key}': {e}"))
        })
    }
}

impl Resolver for TableResolver<'_> {
    fn asset_fingerprint(&self) -> Option<u64> {
        Some(self.fingerprint())
    }

    fn density_function(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.json_at(&format!("density_function/{name}"))
    }

    fn noise(&self, id: &str) -> NoiseParams {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let v = self.json_at(&format!("noise/{name}"));
        NoiseParams {
            first_octave: v["firstOctave"]
                .as_i64()
                .unwrap_or_else(|| panic!("noise '{name}' missing firstOctave"))
                as i32,
            amplitudes: v["amplitudes"]
                .as_array()
                .unwrap_or_else(|| panic!("noise '{name}' missing amplitudes"))
                .iter()
                .map(|a| a.as_f64().expect("amplitude"))
                .collect(),
        }
    }

    fn biome_document(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("biome/{name}"))
    }

    fn configured_feature(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("configured_feature/{name}"))
    }

    fn placed_feature(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("placed_feature/{name}"))
    }

    fn block_tag(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("tags/block/{name}"))
    }

    fn structure_set_ids(&self) -> Vec<String> {
        self.json
            .iter()
            .filter_map(|(id, _)| id.strip_prefix("structure_set/"))
            .map(|name| format!("minecraft:{name}"))
            .collect()
    }

    fn structure_set(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("structure_set/{name}"))
    }

    fn structure(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("structure/{name}"))
    }

    fn structure_template(&self, id: &str) -> Option<Vec<u8>> {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.structure_templates
            .binary_search_by(|(key, _)| (*key).cmp(name))
            .ok()
            .map(|i| self.structure_templates[i].1.to_vec())
    }

    fn template_pool(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("template_pool/{name}"))
    }

    fn processor_list(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("processor_list/{name}"))
    }

    fn biome_tag(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("tags/worldgen/biome/{name}"))
    }

    fn block_freeze_facts(&self) -> Value {
        self.block_freeze_facts
            .map_or(Value::Null, |facts| facts().clone())
    }

    fn block_survival_facts(&self) -> Value {
        self.block_survival_facts
            .map_or(Value::Null, |facts| facts().clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &[(&str, &str)] = &[
        ("biome/plains", r#"{"carvers": ["minecraft:cave"]}"#),
        (
            "density_function/overworld/final_density",
            r#"{"type": "minecraft:constant", "argument": 0.0}"#,
        ),
        (
            "noise/continentalness",
            r#"{"firstOctave": -9, "amplitudes": [1.0, 1.0, 2.0]}"#,
        ),
        ("structure_set/villages", r#"{"placement": {}}"#),
    ];

    const TEMPLATES: &[(&str, &[u8])] = &[("shipwreck/with_mast", b"\x1f\x8b\x00fake")];

    #[test]
    fn required_fields_resolve_with_or_without_prefix() {
        let r = TableResolver::new(JSON);
        assert_eq!(
            r.density_function("minecraft:overworld/final_density")["type"],
            "minecraft:constant"
        );
        assert_eq!(
            r.density_function("overworld/final_density")["type"],
            "minecraft:constant"
        );
        let noise = r.noise("minecraft:continentalness");
        assert_eq!(noise.first_octave, -9);
        assert_eq!(noise.amplitudes, vec![1.0, 1.0, 2.0]);
    }

    #[test]
    #[should_panic(expected = "missing 'noise/nonexistent'")]
    fn missing_required_field_panics_naming_the_key() {
        TableResolver::new(JSON).noise("minecraft:nonexistent");
    }

    #[test]
    fn optional_fields_default_to_no_data_convention() {
        let r = TableResolver::new(JSON);
        assert_eq!(r.configured_feature("minecraft:cave"), Value::Null);
        assert_eq!(r.block_tag("minecraft:whatever"), Value::Null);
        assert_eq!(r.structure("minecraft:mineshaft"), Value::Null);
        // block_freeze_facts is not overridden — the trait default holds.
        assert_eq!(r.block_freeze_facts(), Value::Null);
        assert_eq!(r.block_survival_facts(), Value::Null);
    }

    #[test]
    fn optional_fields_resolve_when_present() {
        let r = TableResolver::new(JSON);
        assert_eq!(
            r.biome_document("minecraft:plains")["carvers"][0],
            "minecraft:cave"
        );
        assert_eq!(r.structure_set("minecraft:villages")["placement"], serde_json::json!({}));
    }

    #[test]
    fn structure_set_ids_are_derived_from_the_table_not_hand_listed() {
        let r = TableResolver::new(JSON);
        assert_eq!(r.structure_set_ids(), vec!["minecraft:villages".to_owned()]);
    }

    #[test]
    fn structure_templates_resolve_from_the_separate_byte_table() {
        let r = TableResolver::new(JSON).with_structure_templates(TEMPLATES);
        assert_eq!(
            r.structure_template("minecraft:shipwreck/with_mast"),
            Some(b"\x1f\x8b\x00fake".to_vec())
        );
        assert_eq!(r.structure_template("minecraft:nonexistent"), None);
    }

    #[test]
    fn parsed_documents_are_reused_by_an_attached_cache() {
        let cache = TableResolver::new(JSON).json_cache();
        let r = TableResolver::new(JSON).with_json_cache(Arc::clone(&cache));
        assert_eq!(r.biome_document("minecraft:plains")["carvers"][0], "minecraft:cave");
        assert_eq!(r.biome_document("minecraft:plains")["carvers"][0], "minecraft:cave");
        assert_eq!(cache.documents.lock().unwrap().len(), 1);
    }

    #[test]
    fn cache_rejects_a_different_asset_table_without_using_stale_data() {
        let cache = TableResolver::new(JSON).json_cache();
        let other = [("biome/plains", r#"{"carvers": []}"#)];
        let resolver = TableResolver::new(&other).with_json_cache(cache);
        assert_eq!(resolver.biome_document("minecraft:plains")["carvers"], serde_json::json!([]));
    }
}
