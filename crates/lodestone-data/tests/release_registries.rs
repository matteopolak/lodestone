//! Drift guards that pin the committed attribute, menu, mob-effect and potion
//! name tables to the current release's own `registries.json`.
//!
//! The report is Mojang's generator output under the gitignored cache, so the
//! checks are `#[ignore]`d; run them after any release bump:
//!
//! ```text
//! cargo test -p lodestone-data --test release_registries -- --ignored --nocapture
//! ```

use lodestone_data::attribute_types::{self, AttributeId, ATTRIBUTE_COUNT};
use lodestone_data::menus::{self, MenuId, MENU_COUNT};
use lodestone_data::mob_effects::{self, MobEffectId};
use lodestone_data::potion::{self, PotionId};

/// `names[protocol_id]` for one registry of a parsed `registries.json`.
fn names_by_protocol_id(doc: &serde_json::Value, registry: &str) -> Vec<String> {
    let entries = doc
        .get(registry)
        .and_then(|reg| reg.get("entries"))
        .and_then(serde_json::Value::as_object)
        .unwrap_or_else(|| panic!("registries.json has {registry}.entries"));
    let mut names = vec![None; entries.len()];
    for (name, value) in entries {
        let id = value
            .get("protocol_id")
            .and_then(serde_json::Value::as_u64)
            .expect("entry has a protocol_id") as usize;
        assert!(id < names.len(), "{registry} protocol_id {id} is not dense");
        assert!(names[id].replace(name.clone()).is_none(), "{registry} id {id} repeats");
    }
    names
        .into_iter()
        .map(|name| name.expect("dense id space"))
        .collect()
}

fn report() -> serde_json::Value {
    let path = lodestone_mc_cache::cache_root()
        .expect("current-release cache directory is present")
        .join("generated/reports/registries.json");
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    serde_json::from_str(&raw).expect("registries.json parses")
}

fn assert_table_matches(registry: &str, expected: &[String], committed: &[String]) {
    assert_eq!(
        expected.len(),
        committed.len(),
        "{registry}: the committed table has a different entry count"
    );
    for (id, (want, have)) in expected.iter().zip(committed).enumerate() {
        assert_eq!(want, have, "{registry}: id {id} drifted");
    }
}

#[test]
#[ignore = "reads the gitignored registries.json; run explicitly after a release bump"]
fn name_tables_match_the_current_release_report() {
    let doc = report();

    let attributes: Vec<String> = (0..ATTRIBUTE_COUNT as i32)
        .map(|id| {
            attribute_types::attribute_name(AttributeId::new(id).expect("in range")).to_owned()
        })
        .collect();
    assert_table_matches("minecraft:attribute", &names_by_protocol_id(&doc, "minecraft:attribute"), &attributes);

    let menu_names: Vec<String> = (0..MENU_COUNT as i32)
        .map(|id| menus::menu_name(MenuId::new(id).expect("in range")).to_owned())
        .collect();
    assert_table_matches("minecraft:menu", &names_by_protocol_id(&doc, "minecraft:menu"), &menu_names);

    let effects = names_by_protocol_id(&doc, "minecraft:mob_effect");
    let committed_effects: Vec<String> = (0..effects.len() as i32)
        .map(|id| {
            mob_effects::mob_effect_name_for(MobEffectId::from_registry_id(id).expect("in range"))
                .to_owned()
        })
        .collect();
    assert_table_matches("minecraft:mob_effect", &effects, &committed_effects);
    assert!(
        MobEffectId::from_registry_id(effects.len() as i32).is_none(),
        "the committed mob-effect table is longer than the report"
    );

    let potions = names_by_protocol_id(&doc, "minecraft:potion");
    let committed_potions: Vec<String> = (0..potions.len() as i32)
        .map(|id| potion::potion_name(PotionId::from_registry_id(id).expect("in range")).to_owned())
        .collect();
    assert_table_matches("minecraft:potion", &potions, &committed_potions);
    assert!(
        PotionId::from_registry_id(potions.len() as i32).is_none(),
        "the committed potion table is longer than the report"
    );
}

/// The comparison itself must fail on a transposed pair, or a green drift
/// check proves nothing.
#[test]
#[should_panic(expected = "id 0 drifted")]
fn comparison_detects_a_transposed_pair() {
    let expected = vec!["minecraft:a".to_owned(), "minecraft:b".to_owned()];
    let transposed = vec!["minecraft:b".to_owned(), "minecraft:a".to_owned()];
    assert_table_matches("control", &expected, &transposed);
}
