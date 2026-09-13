//! Whole-corpus assertions against Mojang's generated reports and the community
//! `minecraft-data` reference.
//!
//! The project's hard-won lesson is that hand-picked fixtures pass while being
//! wrong; coverage *numbers* over the entire corpus are what catch drift. These
//! tests are `#[ignore]`d because they read the (gitignored) `.cache` jar report
//! and `vendor/` community data, neither of which is available in a hermetic
//! checkout. Run them with `--ignored` when the caches are present.
//!
//! What each detector actually catches:
//! * The attribute set drifting between the report and our table (a new
//!   attribute added upstream, or one we invented) — an exact set comparison.
//! * A wrong default/min/max — cross-checked against an independent source.

use lodestone_entity::attribute::{default_def, known_attribute_paths};
use lodestone_model::Identifier;
use std::path::PathBuf;
use std::str::FromStr;

fn repo_root() -> PathBuf {
    // CARGO_MANIFEST_DIR = <root>/crates/lodestone-entity
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|p| p.parent())
        .expect("repo root")
        .to_path_buf()
}

fn read_json(rel: &str) -> Option<serde_json::Value> {
    let path = repo_root().join(rel);
    let text = std::fs::read_to_string(&path).ok()?;
    serde_json::from_str(&text).ok()
}

#[test]
#[ignore = "reads the gitignored .cache jar report"]
fn attribute_set_matches_generated_report_exactly() {
    let report = read_json(".cache/mc/26.2/generated/reports/registries.json")
        .expect("generated registries.json present under .cache");
    let entries = report["minecraft:attribute"]["entries"]
        .as_object()
        .expect("attribute entries");

    // Every attribute the game declares must resolve in our table.
    let mut unresolved = Vec::new();
    for id in entries.keys() {
        let key = Identifier::from_str(id).expect("valid attribute id");
        if default_def(&key).is_none() {
            unresolved.push(id.clone());
        }
    }
    assert!(
        unresolved.is_empty(),
        "attributes in the report with no default_def: {unresolved:?}"
    );

    // And we must not invent attributes the game does not have.
    let report_paths: std::collections::BTreeSet<String> = entries
        .keys()
        .map(|id| id.trim_start_matches("minecraft:").to_string())
        .collect();
    let ours: std::collections::BTreeSet<String> = known_attribute_paths()
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    assert_eq!(
        ours, report_paths,
        "our attribute set diverges from the generated report"
    );

    // Coverage number, not a fixture count.
    assert_eq!(
        entries.len(),
        known_attribute_paths().len(),
        "attribute coverage must be whole-corpus"
    );
}

#[test]
#[ignore = "reads the gitignored vendor/minecraft-data reference"]
fn attribute_ranges_cross_check_vendor() {
    // An independent source for default/min/max. minecraft-data lags the latest
    // snapshot, so we only cross-check the attributes it *does* list, and report
    // any disagreement rather than trusting either blindly.
    let data = read_json("vendor/minecraft-data/data/pc/1.21.5/attributes.json")
        .expect("vendor attributes.json present");
    let list = data.as_array().expect("array of attributes");

    // minecraft-data lags the latest snapshot. Known, source-verified drift
    // where the decompiled 26.2 source disagrees with vendor 1.21.5:
    //   knockback_resistance: 26.2 widened min to -2.0; vendor still says 0.0.
    // We exclude these so the detector stays live for *unexpected* drift.
    let known_vendor_lag = ["minecraft:knockback_resistance"];

    let mut mismatches = Vec::new();
    let mut checked = 0usize;
    for entry in list {
        let resource = entry["resource"].as_str().unwrap_or_default();
        if known_vendor_lag.contains(&resource) {
            continue;
        }
        let Ok(key) = Identifier::from_str(resource) else {
            continue;
        };
        let Some(def) = default_def(&key) else {
            continue;
        };
        checked += 1;
        let want_default = entry["default"].as_f64().unwrap_or(f64::NAN);
        let want_min = entry["min"].as_f64().unwrap_or(f64::NAN);
        let want_max = entry["max"].as_f64().unwrap_or(f64::NAN);
        let approx = |a: f64, b: f64| (a - b).abs() <= 1e-6 * b.abs().max(1.0);
        if !approx(def.default, want_default)
            || !approx(def.min, want_min)
            || !approx(def.max, want_max)
        {
            mismatches.push(format!(
                "{resource}: ours=({}, {}..={}) vendor=({want_default}, {want_min}..={want_max})",
                def.default, def.min, def.max
            ));
        }
    }
    // Anti-vacuity guard: an empty `mismatches` only means "no disagreement" if
    // we actually compared a meaningful number of attributes. Without this, a
    // future rename that made every `default_def` lookup miss would `continue`
    // past every entry and pass green having asserted nothing. Vendor 1.21.5
    // lists 31 attributes we can resolve; require most of them.
    assert!(
        checked >= 25,
        "cross-checked only {checked} attributes — the corpus loop is not exercising \
         the comparison (expected ~31 from vendor 1.21.5). Refusing to pass vacuously."
    );
    assert!(
        mismatches.is_empty(),
        "attribute range disagreements with vendor minecraft-data:\n{}",
        mismatches.join("\n")
    );
}
