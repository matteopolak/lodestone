//! The vanilla 26.3 worldgen data bundle.
//!
//! JSON text only; parsing is the consumer's job. Each registry is a sorted
//! `(name, json)` table whose names are relative to the registry directory with
//! the `.json` stripped (`overworld/final_density`), i.e. the resource path
//! without its `minecraft:` namespace.
//!
//! Regenerate with `scripts/regen-worldgen-data-26-3.sh`; see
//! `docs/worldgen-data-bundle-26-3.md`.

include!(concat!(env!("OUT_DIR"), "/embedded.rs"));

/// Looks a document up in a sorted table. Accepts the `minecraft:` namespace.
#[must_use]
pub fn find(table: &'static [(&'static str, &'static str)], name: &str) -> Option<&'static str> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    table.binary_search_by(|(n, _)| (*n).cmp(name)).ok().map(|i| table[i].1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_are_sorted_and_populated() {
        for table in [DENSITY_FUNCTION, NOISE, NOISE_SETTINGS] {
            assert!(!table.is_empty());
            assert!(table.windows(2).all(|w| w[0].0 < w[1].0));
        }
    }

    #[test]
    fn finds_by_namespaced_and_bare_name() {
        let a = find(DENSITY_FUNCTION, "minecraft:overworld/final_density");
        assert!(a.is_some());
        assert_eq!(a, find(DENSITY_FUNCTION, "overworld/final_density"));
        assert!(find(DENSITY_FUNCTION, "overworld/absent").is_none());
    }
}
