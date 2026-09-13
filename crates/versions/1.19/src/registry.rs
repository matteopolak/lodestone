//! Version-local item and block registry lookups for protocol 762.
//!
//! The flattened-era `slot` and `block_action` packets carry numeric ids in
//! registries whose order is specific to the remote protocol. This module
//! reads the committed 1.19.4 jar report once and turns those ids into
//! canonical resource keys before they reach model consumers.

use std::collections::HashMap;
use std::sync::OnceLock;

use lodestone_model::ResourceKey;
use serde::Deserialize;

use crate::PROTOCOL_1_19_4;

const REGISTRIES: &str = include_str!("../tests/support/registries_1_19_4_jar.json");

#[derive(Default)]
struct Registries {
    items: HashMap<i32, ResourceKey>,
    item_ids: HashMap<ResourceKey, i32>,
    blocks: HashMap<i32, ResourceKey>,
    menus: HashMap<i32, ResourceKey>,
    menu_ids: HashMap<ResourceKey, i32>,
}

/// The committed jar report is a map of version-local registry names to a
/// shared record shape. Registry names and entry names remain strings because
/// the report includes many registries this adapter does not consume; ids are
/// typed so malformed requested entries fail at deserialization.
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
    static V762: OnceLock<Registries> = OnceLock::new();
    match protocol {
        PROTOCOL_1_19_4 => V762.get_or_init(|| parse(REGISTRIES)),
        other => panic!("protocol {other} is outside the v1-19 registry family"),
    }
}

/// Resolves a protocol-762 item registry id to its canonical key.
pub(crate) fn item(protocol: i32, id: i32) -> Option<ResourceKey> {
    for_protocol(protocol).items.get(&id).cloned()
}

/// Resolves a canonical item key to its protocol-762 numeric registry id.
pub(crate) fn item_id(protocol: i32, key: &ResourceKey) -> Option<i32> {
    for_protocol(protocol).item_ids.get(key).copied()
}

/// Resolves a protocol-762 menu registry id to its canonical menu key.
pub(crate) fn menu(protocol: i32, id: i32) -> Option<ResourceKey> {
    for_protocol(protocol).menus.get(&id).cloned()
}

/// Resolves a canonical menu key to its protocol-762 registry id.
pub(crate) fn menu_id(protocol: i32, key: &ResourceKey) -> Option<i32> {
    for_protocol(protocol).menu_ids.get(key).copied()
}

/// Resolves a protocol-762 block registry id to its canonical key.
pub(crate) fn block(protocol: i32, id: i32) -> Option<ResourceKey> {
    for_protocol(protocol).blocks.get(&id).cloned()
}

#[cfg(test)]
mod tests {
    use super::{block, item, item_id, menu, menu_id};
    use crate::PROTOCOL_1_19_4;

    #[test]
    fn jar_registry_ids_are_not_canonical_26_2_ids() {
        assert_eq!(
            item(PROTOCOL_1_19_4, 760).unwrap().to_string(),
            "minecraft:diamond"
        );
        assert_eq!(
            block(PROTOCOL_1_19_4, 101).unwrap().to_string(),
            "minecraft:note_block"
        );
        assert_eq!(
            item_id(PROTOCOL_1_19_4, &"minecraft:diamond".parse().unwrap()),
            Some(760)
        );
        assert_eq!(
            menu(PROTOCOL_1_19_4, 2).unwrap().to_string(),
            "minecraft:generic_9x3"
        );
        assert_eq!(
            menu_id(PROTOCOL_1_19_4, &"minecraft:generic_9x3".parse().unwrap()),
            Some(2)
        );
    }

    #[test]
    fn registry_report_rejects_missing_or_non_integer_ids() {
        for report in [
            r#"{"minecraft:item":{"entries":{"minecraft:stone":{}}}}"#,
            r#"{"minecraft:item":{"entries":{"minecraft:stone":{"protocol_id":"7"}}}}"#,
        ] {
            assert!(
                serde_json::from_str::<super::RegistryReport>(report).is_err(),
                "malformed registry report was accepted: {report}"
            );
        }
    }

    #[test]
    fn registry_report_keeps_unconsumed_registry_names_external() {
        let report = serde_json::from_str::<super::RegistryReport>(
            r#"{
                "minecraft:item":{"entries":{"minecraft:stone":{"protocol_id":7}}},
                "plugin:custom":{"entries":{"plugin:entry":{"protocol_id":0}}}
            }"#,
        )
        .expect("unconsumed registry names are still report data");
        assert!(report.0.contains_key("plugin:custom"));
    }
}
