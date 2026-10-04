//! Block tags: the bundled tag documents resolved into per-block bitsets.

use std::collections::HashMap;

use serde_json::Value;

use crate::blocks::{BlockId, BlockTable};

/// A set of blocks.
#[derive(Clone, Debug, Default)]
pub struct BlockSet {
    bits: Vec<u64>,
}

impl BlockSet {
    fn new(blocks: usize) -> Self {
        Self { bits: vec![0; blocks.div_ceil(64)] }
    }

    fn insert(&mut self, b: BlockId) {
        self.bits[b as usize / 64] |= 1 << (b % 64);
    }

    #[must_use]
    pub fn contains(&self, b: BlockId) -> bool {
        self.bits.get(b as usize / 64).is_some_and(|w| w & (1 << (b % 64)) != 0)
    }
}

/// Every bundled block tag, by name without namespace.
#[derive(Debug)]
pub struct BlockTags {
    sets: HashMap<String, BlockSet>,
}

impl BlockTags {
    /// Resolves every bundled tag (including nested `#tag` references).
    ///
    /// # Panics
    /// If a bundled tag document is malformed or names an unknown block.
    #[must_use]
    pub fn load(table: &BlockTable) -> Self {
        let docs: HashMap<&str, Value> = lodestone_worldgen_data_26_3::TAG_BLOCK
            .iter()
            .map(|(n, j)| (*n, serde_json::from_str(j).expect("tag json parses")))
            .collect();
        let mut sets: HashMap<String, BlockSet> = HashMap::new();
        for name in docs.keys() {
            resolve(name, &docs, table, &mut sets, &mut Vec::new());
        }
        Self { sets }
    }

    /// The set for `name` (`minecraft:` prefix optional); `None` for an unknown tag.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&BlockSet> {
        self.sets.get(name.strip_prefix("minecraft:").unwrap_or(name))
    }
}

fn resolve(name: &str, docs: &HashMap<&str, Value>, table: &BlockTable, sets: &mut HashMap<String, BlockSet>, stack: &mut Vec<String>) {
    if sets.contains_key(name) {
        return;
    }
    assert!(!stack.iter().any(|s| s == name), "tag cycle at {name}");
    stack.push(name.to_owned());
    let mut set = BlockSet::new(table.blocks.len());
    let doc = docs.get(name).unwrap_or_else(|| panic!("unknown tag {name}"));
    for v in doc.get("values").and_then(Value::as_array).expect("values") {
        let (entry, required) = match v {
            Value::String(s) => (s.as_str(), true),
            Value::Object(o) => (
                o.get("id").and_then(Value::as_str).expect("entry id"),
                o.get("required").and_then(Value::as_bool).unwrap_or(true),
            ),
            _ => panic!("tag entry"),
        };
        if let Some(inner) = entry.strip_prefix('#') {
            let inner = inner.strip_prefix("minecraft:").unwrap_or(inner);
            resolve(inner, docs, table, sets, stack);
            let inner_set = sets.get(inner).expect("resolved").clone();
            for (w, bits) in set.bits.iter_mut().zip(&inner_set.bits) {
                *w |= bits;
            }
        } else if let Some(b) = table.block_by_name(entry) {
            set.insert(b);
        } else {
            assert!(!required, "tag {name} names unknown block {entry}");
        }
    }
    stack.pop();
    sets.insert(name.to_owned(), set);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_tags_resolve() {
        let t = BlockTable::load();
        let tags = BlockTags::load(&t);
        let logs = tags.get("logs").unwrap();
        assert!(logs.contains(t.block_by_name("oak_log").unwrap()));
        assert!(!logs.contains(t.block_by_name("stone").unwrap()));
        let air = tags.get("minecraft:air").unwrap();
        assert!(air.contains(t.block_by_name("cave_air").unwrap()));
        assert!(tags.get("no_such_tag").is_none());
    }
}
