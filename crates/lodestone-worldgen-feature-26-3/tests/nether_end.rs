//! Nether and End decoration against the real server (`DecorationOracle263`, settings `nether`
//! and `end`, chunk range 0..256). Each fixture is a greedy cover of a 36- or 64-chunk oracle
//! sweep over one biome layout, keeping chunks until every feature that changes blocks in the
//! sweep does so in at least two kept chunks (ten for the End spikes, one chunk per spike). The
//! oracle commands are
//!
//! ```text
//! run.sh DecorationOracle263 42 nether <layout>.txt '*' 0 256 <chunks>
//! run.sh DecorationOracle263 42 end <layout>.txt '*' 0 256 <chunks>
//! run.sh DecorationOracle263 42 <settings> <layout>.txt +force:<name>:<tries> 0 256 <chunks>
//! ```

mod common;

use common::*;

const NETHER: &[(&str, &str)] = &[
    (NETHER_BIOMES, include_str!("fixtures/nether-biomes-nether-42.txt")),
    ("crimson_forest", include_str!("fixtures/crimson_forest-only-nether-42.txt")),
    ("warped_forest", include_str!("fixtures/warped_forest-only-nether-42.txt")),
    ("soul_sand_valley", include_str!("fixtures/soul_sand_valley-only-nether-42.txt")),
    ("basalt_deltas", include_str!("fixtures/basalt_deltas-only-nether-42.txt")),
    ("nether_wastes", include_str!("fixtures/nether_wastes-only-nether-42.txt")),
];

const END: &[(&str, &str)] = &[
    ("the_end", include_str!("fixtures/the_end-only-end-42.txt")),
    (END_BIOMES, include_str!("fixtures/end-biomes-end-42.txt")),
];

const SPRING_DELTA: &str = include_str!("fixtures/force-spring_delta-nether-42.txt");
const END_ISLAND: &str = include_str!("fixtures/force-end_island_decorated-end-42.txt");
const END_GATEWAY: &str = include_str!("fixtures/force-end_gateway_return-end-42.txt");

#[test]
fn nether_layouts_match_the_server() {
    for (layout, fixture) in NETHER {
        check_dimension("nether", layout, 42, fixture, &["*"]);
    }
}

#[test]
fn end_layouts_match_the_server() {
    for (layout, fixture) in END {
        check_dimension("end", layout, 42, fixture, &["*"]);
    }
}

#[test]
fn a_basalt_delta_spring_matches_the_server() {
    check_dimension("nether", "basalt_deltas", 42, SPRING_DELTA, &["+force:spring_delta:64"]);
}

#[test]
fn an_end_island_matches_the_server() {
    check_dimension("end", "small_end_islands", 42, END_ISLAND, &["+force:end_island_decorated:16"]);
}

#[test]
fn an_end_gateway_matches_the_server() {
    check_dimension("end", "end_highlands", 42, END_GATEWAY, &["+force:end_gateway_return:3000"]);
}

/// Every Nether and End biome's features are ported: nothing on either list is a gap.
#[test]
fn no_nether_or_end_feature_is_a_gap() {
    for (settings, layout) in [("nether", NETHER_BIOMES), ("end", END_BIOMES)] {
        let world = World::new(settings, 42, layout);
        let gaps = world.decorator().gaps(env());
        assert!(gaps.is_empty(), "{settings}: {gaps:?}");
    }
}

/// Control: each fixture under a neighbouring seed must be rejected.
#[test]
fn control_wrong_seed_fails() {
    for (settings, fixtures) in [("nether", NETHER), ("end", END)] {
        for (layout, fixture) in fixtures {
            assert!(!reproduces_in(settings, layout, 43, fixture, &["*"]), "{settings} {}", &layout[..layout.len().min(20)]);
        }
    }
}

/// Control: the fixtures really exercise the ported features.
#[test]
fn fixtures_are_not_vacuous() {
    let mut changed: std::collections::BTreeMap<&str, u32> = std::collections::BTreeMap::new();
    let all = NETHER.iter().chain(END).map(|(_, f)| *f).chain([SPRING_DELTA, END_ISLAND, END_GATEWAY]);
    for fixture in all {
        for l in fixture.lines().filter(|l| l.starts_with("f ")) {
            let p: Vec<&str> = l.split(' ').collect();
            if p[7] != "0" {
                *changed.entry(p[3]).or_default() += 1;
            }
        }
    }
    for name in [
        "chorus_plant",
        "crimson_fungi",
        "warped_fungi",
        "delta",
        "basalt_blobs",
        "blackstone_blobs",
        "glowstone",
        "glowstone_extra",
        "end_spike",
        "end_platform",
        "end_island_decorated",
        "end_gateway_return",
        "spring_delta",
        "twisting_vines",
        "weeping_vines",
        "brown_mushroom_nether",
    ] {
        let key = format!("minecraft:{name}");
        assert!(changed.get(key.as_str()).copied().unwrap_or(0) > 0, "{name} never changes a block: {changed:?}");
    }
}
