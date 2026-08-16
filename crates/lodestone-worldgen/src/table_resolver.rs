//! A [`Resolver`] driven entirely by a static, id-sorted table of embedded
//! JSON text, plus an optional table of binary structure-template bytes.
//!
//! # Why this exists
//!
//! Every embedding site that wants real terrain (not a hand-rolled test
//! fixture) ends up writing the same ~150 lines: a `build.rs` that walks
//! `assets/worldgen/` into a sorted `&'static [(&str, &str)]` table keyed by
//! path-with-extension-stripped, and a private `Resolver` impl that does
//! `strip_prefix("minecraft:")` + `binary_search_by` + `serde_json::from_str`
//! for every category (`density_function/`, `noise/`, `biome/`,
//! `configured_carver/`, `configured_feature/`, `placed_feature/`,
//! `tags/block/`, `structure_set/`, `structure/`, `tags/worldgen/biome/`,
//! `template_pool/`, `processor_list/`). `lodestone-server`'s
//! `EmbeddedResolver` was the first of these; a pending version-seam migration
//! moves 26.2's actual JSON *data* out of this crate's dependents into
//! whichever version crate ships it, and a second embedding site is exactly
//! when hand-rolling this a second time would go unnoticed as duplication.
//! This type is the shared half: supply a table, get a full [`Resolver`].
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
//! [`Resolver::block_freeze_facts`] is deliberately absent: it is a census of
//! the game's *compiled* behaviour (collision, fluid state), sourced from
//! `lodestone_data::{block_solidity, snow_support}`, not from a JSON asset —
//! and this crate must stay version-free, so it cannot depend on
//! `lodestone-data`. An embedder that wants it wraps [`TableResolver`] in a
//! newtype and overrides just that one method, the same way
//! `lodestone_server::worldgen_data::NetherResolver` wraps `EmbeddedResolver`
//! to override just `biome_parameters` today.

use serde_json::Value;

use crate::density::{NoiseParams, Resolver};

/// See the [module docs](self).
#[derive(Debug, Clone, Copy)]
pub struct TableResolver<'a> {
    json: &'a [(&'a str, &'a str)],
    structure_templates: &'a [(&'a str, &'a [u8])],
    biome_parameters_key: &'a str,
    biome_temperatures_key: &'a str,
}

/// The keys [`EmbeddedResolver`](../../lodestone_server/struct.EmbeddedResolver.html)
/// (and every other overworld embedder so far) has used for the two
/// dimension-scoped singleton documents. A resolver for a different
/// dimension (e.g. the Nether) overrides these via
/// [`TableResolver::with_biome_parameters_key`] /
/// [`TableResolver::with_biome_temperatures_key`] rather than needing a
/// wrapper type for this one difference.
const DEFAULT_BIOME_PARAMETERS_KEY: &str = "biome_parameters/overworld";
const DEFAULT_BIOME_TEMPERATURES_KEY: &str = "biome_parameters/overworld_temperature";

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
            biome_parameters_key: DEFAULT_BIOME_PARAMETERS_KEY,
            biome_temperatures_key: DEFAULT_BIOME_TEMPERATURES_KEY,
        }
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

    /// Overrides the table id [`Resolver::biome_parameters`] looks up.
    /// Default: `"biome_parameters/overworld"`.
    #[must_use]
    pub const fn with_biome_parameters_key(mut self, key: &'a str) -> Self {
        self.biome_parameters_key = key;
        self
    }

    /// Overrides the table id [`Resolver::biome_temperatures`] looks up.
    /// Default: `"biome_parameters/overworld_temperature"`.
    #[must_use]
    pub const fn with_biome_temperatures_key(mut self, key: &'a str) -> Self {
        self.biome_temperatures_key = key;
        self
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
        self.try_raw(key).map_or(Value::Null, |raw| {
            serde_json::from_str(raw).unwrap_or_else(|e| panic!("parsing embedded '{key}': {e}"))
        })
    }
}

impl Resolver for TableResolver<'_> {
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

    fn biome_parameters(&self) -> Value {
        // NOT `try_json`: a missing key must resolve to the trait's own
        // documented default (`Value::Array(Vec::new())`), because
        // `crate::biome::parse_table` calls `.as_array().expect(..)` on
        // whatever this returns — `Value::Null` would panic instead of
        // taking the "no real biome variety supplied" fallback path.
        self.try_raw(self.biome_parameters_key).map_or_else(
            || Value::Array(Vec::new()),
            |raw| {
                serde_json::from_str(raw)
                    .unwrap_or_else(|e| panic!("parsing embedded '{}': {e}", self.biome_parameters_key))
            },
        )
    }

    fn biome_temperatures(&self) -> Value {
        // Same reasoning as `biome_parameters`: `crate::biome::parse_temperatures`
        // calls `.as_object().expect(..)`, so the empty default must be an
        // object, not `Null`.
        self.try_raw(self.biome_temperatures_key).map_or_else(
            || Value::Object(serde_json::Map::new()),
            |raw| {
                serde_json::from_str(raw).unwrap_or_else(|e| {
                    panic!("parsing embedded '{}': {e}", self.biome_temperatures_key)
                })
            },
        )
    }

    fn biome_document(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("biome/{name}"))
    }

    fn configured_carver(&self, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        self.try_json(&format!("configured_carver/{name}"))
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
}

#[cfg(test)]
mod tests {
    use super::*;

    const JSON: &[(&str, &str)] = &[
        ("biome/plains", r#"{"carvers": ["minecraft:cave"]}"#),
        (
            "biome_parameters/overworld",
            r#"[{"biome": "minecraft:plains"}]"#,
        ),
        (
            "biome_parameters/overworld_temperature",
            r#"{"minecraft:plains": 0.8}"#,
        ),
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
        assert_eq!(r.configured_carver("minecraft:cave"), Value::Null);
        assert_eq!(r.block_tag("minecraft:whatever"), Value::Null);
        assert_eq!(r.structure("minecraft:mineshaft"), Value::Null);
        // block_freeze_facts is not overridden — the trait default holds.
        assert_eq!(r.block_freeze_facts(), Value::Null);
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
    fn biome_parameter_keys_use_the_overworld_default() {
        let r = TableResolver::new(JSON);
        assert_eq!(r.biome_parameters()[0]["biome"], "minecraft:plains");
        assert_eq!(r.biome_temperatures()["minecraft:plains"], 0.8);
    }

    #[test]
    fn biome_parameter_keys_are_overridable() {
        const NETHER_JSON: &[(&str, &str)] = &[(
            "biome_parameters/nether",
            r#"[{"biome": "minecraft:nether_wastes"}]"#,
        )];
        let r = TableResolver::new(NETHER_JSON).with_biome_parameters_key("biome_parameters/nether");
        assert_eq!(
            r.biome_parameters()[0]["biome"],
            "minecraft:nether_wastes"
        );
        // No temperature table for the Nether: still resolves to the
        // trait's own empty-object default (never `Null`, which
        // `crate::biome::parse_temperatures` would panic on) rather than
        // panicking.
        assert_eq!(r.biome_temperatures(), Value::Object(serde_json::Map::new()));
        let _ = crate::biome::parse_temperatures(&r.biome_temperatures());
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
    fn empty_table_is_a_valid_all_defaults_resolver() {
        let r = TableResolver::new(&[]);
        // Both must be the trait's typed empty defaults, not `Value::Null` —
        // `crate::biome::parse_table`/`parse_temperatures` panic on `Null`
        // (`.as_array()`/`.as_object()` return `None` for it), and
        // `crate::overworld::OverworldGenerator::new` calls
        // `parse_table(&resolver.biome_parameters())` unconditionally, before
        // checking whether the result is empty.
        assert_eq!(r.biome_parameters(), Value::Array(Vec::new()));
        assert_eq!(r.biome_temperatures(), Value::Object(serde_json::Map::new()));
        let _ = crate::biome::parse_table(&r.biome_parameters());
        let _ = crate::biome::parse_temperatures(&r.biome_temperatures());
        assert_eq!(r.structure_set_ids(), Vec::<String>::new());
        assert_eq!(r.structure_template("minecraft:anything"), None);
    }
}
