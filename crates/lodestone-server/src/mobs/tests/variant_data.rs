//! The biome rules in `appearance` checked against the data files of the
//! pinned release (`data/minecraft/{*_variant,tags/worldgen/biome,worldgen/biome}`),
//! read through `lodestone_mc_cache`. The expected variant of every biome is
//! derived from the files here, never written out: a biome tag that changes in
//! a new release fails a test instead of silently diverging.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde_json::Value;
use uuid::Uuid;

use super::appearance::{CAT_VARIANTS, DYE_NAMES, MobVariant, choose_variant, mix_dyes};

fn data_root() -> Option<PathBuf> {
    let root = lodestone_mc_cache::version_root(&lodestone_mc_cache::current_version())
        .join("src/data/minecraft");
    if root.is_dir() {
        Some(root)
    } else {
        eprintln!("SKIP: {} is absent (no decompiled tree for the current release)", root.display());
        None
    }
}

fn json(root: &std::path::Path, rel: &str) -> Value {
    let path = root.join(rel);
    let raw = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()));
    serde_json::from_str(&raw).unwrap_or_else(|e| panic!("parsing {}: {e}", path.display()))
}

fn stem_names(root: &std::path::Path, dir: &str) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root.join(dir))
        .unwrap_or_else(|e| panic!("listing {dir}: {e}"))
        .filter_map(|entry| {
            let name = entry.ok()?.file_name().into_string().ok()?;
            name.strip_suffix(".json").map(str::to_owned)
        })
        .collect();
    names.sort();
    names
}

/// The biomes a `biomes` field names: a single key, a `#tag`, or a list of either.
fn resolve(root: &std::path::Path, spec: &Value, out: &mut BTreeSet<String>) {
    match spec {
        Value::String(text) => match text.strip_prefix("#minecraft:") {
            Some(tag) => {
                let doc = json(root, &format!("tags/worldgen/biome/{tag}.json"));
                for value in doc["values"].as_array().expect("tag values") {
                    resolve(root, value, out);
                }
            }
            None => {
                out.insert(text.strip_prefix("minecraft:").unwrap_or(text).to_owned());
            }
        },
        Value::Array(items) => items.iter().for_each(|item| resolve(root, item, out)),
        other => panic!("unexpected biome spec {other}"),
    }
}

fn tag(root: &std::path::Path, name: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    resolve(root, &Value::String(format!("#minecraft:{name}")), &mut out);
    out
}

/// The variant the data files assign to `biome`: the highest-priority matching
/// condition, which for these registries is unique per biome.
fn data_variant(root: &std::path::Path, registry: &str, biome: &str) -> String {
    let mut best: Option<(i64, String)> = None;
    for name in stem_names(root, registry) {
        let doc = json(root, &format!("{registry}/{name}.json"));
        for entry in doc["spawn_conditions"].as_array().expect("spawn_conditions") {
            let matches = match entry.get("condition") {
                None => true,
                Some(cond) if cond["type"] == "minecraft:biome" => {
                    let mut set = BTreeSet::new();
                    resolve(root, &cond["biomes"], &mut set);
                    set.contains(biome)
                }
                Some(_) => false,
            };
            if !matches {
                continue;
            }
            let priority = entry["priority"].as_i64().expect("priority");
            match &best {
                Some((p, other)) if *p == priority => {
                    panic!("{registry}: {other} and {name} tie at priority {priority} for {biome}")
                }
                Some((p, _)) if *p > priority => {}
                _ => best = Some((priority, format!("minecraft:{name}"))),
            }
        }
    }
    best.unwrap_or_else(|| panic!("{registry}: nothing matches {biome}")).1
}

fn name_of(variant: Option<MobVariant>) -> String {
    match variant {
        Some(MobVariant::Name(name)) => name,
        other => panic!("expected a keyed variant, got {other:?}"),
    }
}

#[test]
fn temperature_and_wolf_variants_match_the_data_files_in_every_biome() {
    let Some(root) = data_root() else { return };
    let biomes = stem_names(&root, "worldgen/biome");
    assert!(biomes.len() > 50, "the biome universe was read: {}", biomes.len());
    let uuid = Uuid::from_u128(0x5EED);
    let mut warm = 0;
    for biome in &biomes {
        for (species, registry) in [
            ("cow", "cow_variant"),
            ("pig", "pig_variant"),
            ("chicken", "chicken_variant"),
            ("frog", "frog_variant"),
            ("wolf", "wolf_variant"),
        ] {
            let got = name_of(choose_variant(species, &format!("minecraft:{biome}"), uuid));
            let want = data_variant(&root, registry, biome);
            assert_eq!(got, want, "{species} in {biome}");
            warm += usize::from(got == "minecraft:warm");
        }
    }
    // A control that the comparison can see a non-default answer at all.
    assert!(warm > 5, "warm variants were exercised: {warm}");
}

#[test]
fn fox_and_rabbit_biome_sets_match_the_tags() {
    let Some(root) = data_root() else { return };
    let snow_foxes = tag(&root, "spawns_snow_foxes");
    let white = tag(&root, "spawns_white_rabbits");
    let gold = tag(&root, "spawns_gold_rabbits");
    assert!(!snow_foxes.is_empty() && !white.is_empty() && !gold.is_empty());
    for biome in stem_names(&root, "worldgen/biome") {
        let full = format!("minecraft:{biome}");
        for seed in 0..64u128 {
            let uuid = Uuid::from_u128(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
            let fox = name_of(choose_variant("fox", &full, uuid));
            assert_eq!(fox == "snow", snow_foxes.contains(&biome), "fox in {biome}");
            let Some(MobVariant::Int(rabbit)) = choose_variant("rabbit", &full, uuid) else {
                panic!("rabbit variant is an int");
            };
            let want: &[i32] = if white.contains(&biome) {
                &[1, 3]
            } else if gold.contains(&biome) {
                &[4]
            } else {
                &[0, 2, 5]
            };
            assert!(want.contains(&rabbit), "rabbit {rabbit} in {biome}, want one of {want:?}");
        }
    }
}

#[test]
fn rolled_cat_variants_are_the_unconditional_entries_of_the_cat_registry() {
    let Some(root) = data_root() else { return };
    let unconditional: BTreeSet<String> = stem_names(&root, "cat_variant")
        .into_iter()
        .filter(|name| {
            let doc = json(&root, &format!("cat_variant/{name}.json"));
            doc["spawn_conditions"].as_array().is_some_and(|conds| {
                conds.iter().all(|c| c.get("condition").is_none())
            })
        })
        .collect();
    let modeled: BTreeSet<String> = CAT_VARIANTS.iter().map(|s| (*s).to_owned()).collect();
    assert_eq!(modeled, unconditional);
    // Every modeled variant is actually reachable from the roll.
    let seen: BTreeSet<String> = (0..400u128)
        .map(|seed| {
            let uuid = Uuid::from_u128(seed.wrapping_mul(0xD6E8_FEB8_6659_FD93) | 1);
            name_of(choose_variant("cat", "minecraft:plains", uuid))
        })
        .map(|name| name.strip_prefix("minecraft:").unwrap().to_owned())
        .collect();
    assert_eq!(seen, modeled);
}

/// Every pair of dye colours mixes exactly as the two-ingredient shapeless dye
/// recipes say, and no other pair mixes.
#[test]
fn dye_mixing_matches_the_two_ingredient_recipes() {
    let Some(root) = data_root() else { return };
    let dye = |item: &str| {
        let name = item.strip_prefix("minecraft:")?.strip_suffix("_dye")?;
        DYE_NAMES.iter().position(|d| *d == name).map(|i| i as u8)
    };
    let mut want = std::collections::BTreeMap::new();
    for name in stem_names(&root, "recipe") {
        let doc = json(&root, &format!("recipe/{name}.json"));
        if doc["type"] != "minecraft:crafting_shapeless" {
            continue;
        }
        let Some(ingredients) = doc["ingredients"].as_array().filter(|i| i.len() == 2) else { continue };
        let colors: Vec<Option<u8>> = ingredients.iter().map(|i| i.as_str().and_then(dye)).collect();
        let (Some(Some(a)), Some(Some(b))) = (colors.first(), colors.get(1)) else { continue };
        let Some(result) = doc["result"]["id"].as_str().and_then(dye) else { continue };
        want.insert((*a.min(b), *a.max(b)), result);
    }
    assert_eq!(want.len(), 9, "the recipes the data files hold: {want:?}");
    for a in 0..16u8 {
        for b in 0..16u8 {
            assert_eq!(
                mix_dyes(a, b),
                want.get(&(a.min(b), a.max(b))).copied(),
                "{} + {}",
                DYE_NAMES[a as usize],
                DYE_NAMES[b as usize]
            );
        }
    }
}
