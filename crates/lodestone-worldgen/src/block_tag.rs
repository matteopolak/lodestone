//! Block-tag closure: expands a tag id read through a [`Resolver`] into the
//! set of base block names it names, following nested tag references.

use std::collections::HashSet;

use serde_json::Value;

use crate::resolver::Resolver;

/// Recursively resolves a block tag's closure into a set of base block names.
/// Sub-tag references (`"#minecraft:..."`) recurse; plain ids are added
/// directly, and `seen` stops a cyclic reference.
///
/// A tag id with no data (`Resolver::block_tag`'s default `Value::Null`, or
/// a tag absent from the version's data) resolves to no members rather than
/// panicking: a `Resolver` that ships only part of the data is common.
pub(crate) fn resolve_block_tag(
    resolver: &dyn Resolver,
    id: &str,
    out: &mut HashSet<String>,
    seen: &mut HashSet<String>,
) {
    if !seen.insert(id.to_string()) {
        return;
    }
    let doc = resolver.block_tag(id);
    let Some(values) = doc.get("values").and_then(Value::as_array) else {
        return;
    };
    for entry in values {
        let s = match entry {
            Value::String(s) => s.as_str(),
            Value::Object(o) => o.get("id").and_then(Value::as_str).unwrap_or_default(),
            _ => continue,
        };
        if let Some(sub) = s.strip_prefix('#') {
            resolve_block_tag(resolver, sub, out, seen);
        } else if !s.is_empty() {
            out.insert(s.to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    struct FakeResolver {
        tags: HashMap<&'static str, Value>,
        biomes: HashMap<&'static str, Value>,
        features: HashMap<&'static str, Value>,
        placed: HashMap<&'static str, Value>,
    }

    impl Resolver for FakeResolver {
        fn block_tag(&self, id: &str) -> Value {
            self.tags.get(id).cloned().unwrap_or(Value::Null)
        }
        fn biome_document(&self, id: &str) -> Value {
            self.biomes.get(id).cloned().unwrap_or(Value::Null)
        }
        fn configured_feature(&self, id: &str) -> Value {
            self.features.get(id).cloned().unwrap_or(Value::Null)
        }
        fn placed_feature(&self, id: &str) -> Value {
            self.placed.get(id).cloned().unwrap_or(Value::Null)
        }
    }

    #[test]
    fn resolve_block_tag_follows_subtag_references() {
        let mut tags = HashMap::new();
        tags.insert(
            "minecraft:leaf",
            serde_json::json!({"values": ["minecraft:oak_log", "#minecraft:sub"]}),
        );
        tags.insert(
            "minecraft:sub",
            serde_json::json!({"values": ["minecraft:stone"]}),
        );
        let resolver = FakeResolver {
            tags,
            biomes: HashMap::new(),
            features: HashMap::new(),
            placed: HashMap::new(),
        };
        let mut out = HashSet::new();
        let mut seen = HashSet::new();
        resolve_block_tag(&resolver, "minecraft:leaf", &mut out, &mut seen);
        assert_eq!(
            out,
            HashSet::from([
                "minecraft:oak_log".to_string(),
                "minecraft:stone".to_string()
            ])
        );
    }

    #[test]
    fn resolve_block_tag_missing_id_is_empty_not_panic() {
        let resolver = FakeResolver {
            tags: HashMap::new(),
            biomes: HashMap::new(),
            features: HashMap::new(),
            placed: HashMap::new(),
        };
        let mut out = HashSet::new();
        let mut seen = HashSet::new();
        resolve_block_tag(&resolver, "minecraft:does_not_exist", &mut out, &mut seen);
        assert!(out.is_empty());
    }
}
