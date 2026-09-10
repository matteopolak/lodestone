//! Version-local item and block registry lookups for protocols 756 and 758.
//!
//! The flattened-era `slot` and `block_action` packets carry numeric ids in
//! registries whose order is specific to the remote protocol. This module
//! reads the committed jar reports once per protocol and turns those ids into
//! canonical resource keys before they can reach model consumers.

use std::collections::HashMap;
use std::sync::OnceLock;

use lodestone_model::ResourceKey;
use serde::Deserialize;

use crate::{PROTOCOL_1_17_1, PROTOCOL_1_18_2};

const REGISTRIES_756: &str = include_str!("../tests/support/registries_1_17_1_jar.json");
const REGISTRIES_758: &str = include_str!("../tests/support/registries_1_18_2_jar.json");

#[derive(Default)]
struct Registries {
    items: HashMap<i32, ResourceKey>,
    item_ids: HashMap<ResourceKey, i32>,
    blocks: HashMap<i32, ResourceKey>,
    menus: HashMap<i32, ResourceKey>,
    menu_ids: HashMap<ResourceKey, i32>,
}

/// The jar report is a map of registry names to the same small record shape.
///
/// Registry names and entry names are intentionally strings: the report is a
/// version-local external fixture and contains many registries that this
/// adapter does not consume. The fields we do consume are typed, so malformed
/// ids fail while unrelated registries remain forward-compatible.
#[derive(Debug, Deserialize)]
#[serde(transparent)]
struct RegistryReport(HashMap<String, RegistryDefinition>);

#[derive(Debug, Deserialize)]
struct RegistryDefinition {
    #[serde(default)]
    entries: Option<HashMap<String, RegistryEntry>>,
}

#[derive(Debug, Deserialize)]
struct RegistryEntry {
    protocol_id: i32,
}

fn registry(report: &'static str, registry: &str) -> HashMap<i32, ResourceKey> {
    let report: RegistryReport = serde_json::from_str(report)
        .expect("the committed jar registry report must remain valid JSON");
    report
        .0
        .get(registry)
        .and_then(|registry| registry.entries.as_ref())
        .expect("the committed jar registry report must contain the requested registry entries")
        .iter()
        .map(|(name, entry)| {
            let key = name
                .parse()
                .expect("a jar registry entry must be a valid resource key");
            (entry.protocol_id, key)
        })
        .collect()
}

fn parse(report: &'static str) -> Registries {
    let items = registry(report, "minecraft:item");
    let menus = registry(report, "minecraft:menu");
    Registries {
        item_ids: items.iter().map(|(id, key)| (key.clone(), *id)).collect(),
        items,
        blocks: registry(report, "minecraft:block"),
        menu_ids: menus.iter().map(|(id, key)| (key.clone(), *id)).collect(),
        menus,
    }
}

fn for_protocol(protocol: i32) -> &'static Registries {
    static V756: OnceLock<Registries> = OnceLock::new();
    static V758: OnceLock<Registries> = OnceLock::new();
    match protocol {
        PROTOCOL_1_17_1 => V756.get_or_init(|| parse(REGISTRIES_756)),
        PROTOCOL_1_18_2 => V758.get_or_init(|| parse(REGISTRIES_758)),
        other => panic!("protocol {other} is outside the v1-17 registry family"),
    }
}

/// Resolves a protocol-local item registry id to its canonical key.
pub(crate) fn item(protocol: i32, id: i32) -> Option<ResourceKey> {
    for_protocol(protocol).items.get(&id).cloned()
}

/// Resolves a canonical item key to the protocol-local flattened item id.
pub(crate) fn item_id(protocol: i32, key: &ResourceKey) -> Option<i32> {
    for_protocol(protocol).item_ids.get(key).copied()
}

/// Resolves a protocol-local menu registry id to its canonical key.
pub(crate) fn menu(protocol: i32, id: i32) -> Option<ResourceKey> {
    for_protocol(protocol).menus.get(&id).cloned()
}

/// Resolves a canonical menu key to the protocol-local menu registry id.
pub(crate) fn menu_id(protocol: i32, key: &ResourceKey) -> Option<i32> {
    for_protocol(protocol).menu_ids.get(key).copied()
}

/// Resolves a protocol-local block registry id to its canonical key.
pub(crate) fn block(protocol: i32, id: i32) -> Option<ResourceKey> {
    for_protocol(protocol).blocks.get(&id).cloned()
}

#[cfg(test)]
mod tests {
    use super::RegistryReport;

    #[test]
    fn registry_report_rejects_missing_or_non_integer_ids() {
        for report in [
            r#"{"minecraft:item":{"entries":{"minecraft:stone":{}}}}"#,
            r#"{"minecraft:item":{"entries":{"minecraft:stone":{"protocol_id":"7"}}}}"#,
        ] {
            assert!(
                serde_json::from_str::<RegistryReport>(report).is_err(),
                "malformed registry report was accepted: {report}"
            );
        }
    }

    #[test]
    fn registry_report_keeps_unconsumed_registry_names_external() {
        let report = serde_json::from_str::<RegistryReport>(
            r#"{
                "minecraft:item":{"entries":{"minecraft:stone":{"protocol_id":7}}},
                "plugin:custom":{"entries":{"plugin:entry":{"protocol_id":0}}}
            }"#,
        )
        .expect("unconsumed registry names are still report data");
        assert!(report.0.contains_key("plugin:custom"));
    }
}
