//! Trees (every trunk, foliage and decorator type that is ported) against the real server
//! (`DecorationOracle263`), over a forest-heavy biome layout. Every placed feature without a
//! known gap runs on both sides; regenerate the fixture whenever a port closes a gap:
//! run the ignored `print_oracle_args` test and feed its output to the oracle with
//! `tree-biomes.txt` as the layout.

mod common;

use common::*;

const FIXTURE: &str = include_str!("fixtures/trees-overworld-42.txt");

fn world() -> World {
    World::new("overworld", 42, TREE_BIOMES)
}

fn only() -> Vec<String> {
    all_ported(&world().decorator())
}

#[test]
#[ignore = "prints the oracle arguments"]
fn print_oracle_args() {
    println!("ARGS {}", only().join(","));
}

#[test]
#[ignore = "prints the remaining gaps"]
fn print_gaps() {
    let mut by_gap: std::collections::BTreeMap<String, Vec<String>> = std::collections::BTreeMap::new();
    for (name, gaps) in world().decorator().gaps(env()) {
        for g in gaps {
            by_gap.entry(g).or_default().push(name.clone());
        }
    }
    for (gap, names) in by_gap {
        println!("{gap}: {} ({})", names.len(), names.iter().take(8).cloned().collect::<Vec<_>>().join(", "));
    }
}

#[test]
fn trees_overworld_42() {
    let only = only();
    let refs: Vec<&str> = only.iter().map(String::as_str).collect();
    check_in(TREE_BIOMES, 42, FIXTURE, &refs);
}

/// Control: the same fixture under a neighbouring seed must be rejected.
#[test]
fn control_wrong_seed_fails() {
    let only = only();
    let result = std::panic::catch_unwind(|| {
        let refs: Vec<&str> = only.iter().map(String::as_str).collect();
        check_in(TREE_BIOMES, 43, FIXTURE, &refs)
    });
    assert!(result.is_err(), "a different seed must not reproduce the oracle's chunks");
}

/// Control: the tree features really place blocks in the fixture.
#[test]
fn fixture_is_not_vacuous() {
    let mut changed: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
    for l in FIXTURE.lines().filter(|l| l.starts_with("f ")) {
        let f: Vec<&str> = l.split(' ').collect();
        *changed.entry(f[3]).or_default() += f[7].parse::<u32>().unwrap();
    }
    for name in [
        "minecraft:trees_birch",
        "minecraft:trees_grove",
        "minecraft:trees_swamp",
        "minecraft:trees_cherry",
        "minecraft:trees_taiga",
        "minecraft:trees_old_growth_pine_taiga",
        "minecraft:trees_old_growth_spruce_taiga",
        "minecraft:trees_savanna",
        "minecraft:trees_jungle",
        "minecraft:trees_flower_forest",
        "minecraft:trees_windswept_savanna",
        "minecraft:dark_forest_vegetation",
        "minecraft:mushroom_island_vegetation",
    ] {
        assert!(changed.get(name).copied().unwrap_or(0) > 0, "{name} never changes a block in the fixture");
    }
}

/// Prints the cells one placed feature changes in one chunk, in the oracle's `d` format
/// (`LODESTONE_DUMP=<placed name>,<cx>,<cz>`), for diffing against the oracle's `+dump:name`.
#[test]
#[ignore = "debugging aid"]
fn dump_feature() {
    let spec = std::env::var("LODESTONE_DUMP").expect("LODESTONE_DUMP=name,cx,cz");
    let f: Vec<&str> = spec.split(',').collect();
    let (name, cx, cz): (&str, i32, i32) = (f[0], f[1].parse().unwrap(), f[2].parse().unwrap());
    let mut world = world();
    let decorator = world.decorator();
    let only = only();
    let refs: Vec<&str> = only.iter().map(String::as_str).collect();
    let mut level = world.level(cx, cz);
    let present = world.present(cx, cz);
    let blocks = &env().blocks;
    decorator.decorate(&mut level, cx, cz, &present, Some(&refs), |r| {
        if decorator.features.placed[r.placed].name.ends_with(name) {
            for (x, y, z, s) in &r.changed {
                println!("d {x} {y} {z} {}", blocks.full_key(*s));
            }
            println!("f draws {} changed {}", r.draws, r.changed.len());
        }
    });
}
