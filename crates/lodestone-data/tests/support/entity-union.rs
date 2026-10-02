use std::collections::BTreeSet;

pub const LATEST_DUMP: &str = include_str!("entity_census_26_3_jvm.txt");

#[derive(Clone, Debug)]
pub struct LatestEntity {
    pub wire_id: usize,
    pub name: String,
    pub living: bool,
    pub mob: bool,
    pub pushes: bool,
    pub collidable: bool,
    pub width_bits: u32,
    pub height_bits: u32,
}

pub fn latest_rows() -> Vec<LatestEntity> {
    let mut names = BTreeSet::new();
    let mut rows = Vec::new();
    for line in LATEST_DUMP.lines() {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let tokens: Vec<_> = line.split_whitespace().collect();
        assert_eq!(tokens.len(), 8, "invalid latest entity row: {line}");
        let row = LatestEntity {
            wire_id: tokens[0].parse().expect("latest wire id"),
            name: tokens[1].to_owned(),
            living: tokens[2].parse().expect("latest living flag"),
            mob: tokens[3].parse().expect("latest mob flag"),
            pushes: tokens[4].parse().expect("latest crowd-push flag"),
            collidable: tokens[5].parse().expect("latest hard-collision flag"),
            width_bits: u32::from_str_radix(tokens[6], 16).expect("latest width bits"),
            height_bits: u32::from_str_radix(tokens[7], 16).expect("latest height bits"),
        };
        assert_eq!(row.wire_id, rows.len(), "latest wire ids must be dense");
        assert!(names.insert(row.name.clone()), "duplicate latest entity name");
        assert!(!row.mob || row.living, "a mob must be living");
        assert!(!row.pushes || row.living, "ordinary crowd push requires living");
        for bits in [row.width_bits, row.height_bits] {
            let dimension = f32::from_bits(bits);
            assert!(dimension.is_finite() && dimension >= 0.0, "invalid captured dimension");
        }
        rows.push(row);
    }
    assert_eq!(rows.len(), 161, "complete latest entity capture required");
    assert!(rows.iter().any(|row| row.collidable), "hard-collision positive control");
    assert!(rows.iter().any(|row| !row.collidable), "hard-collision negative control");
    let dimensions: Vec<Vec<_>> = include_str!("entity_dimensions_26_3_jvm.txt").lines()
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(|line| line.split_whitespace().collect()).collect();
    assert_eq!(dimensions.len(), rows.len(), "independent capture must be complete");
    for (row, captured) in rows.iter().zip(dimensions) {
        assert_eq!(captured.len(), 4, "independent dimensions columns");
        assert_eq!(captured[0].parse::<usize>().expect("dimension wire id"), row.wire_id);
        assert_eq!(captured[1], row.name, "independent resource identity");
        assert_eq!(u32::from_str_radix(captured[2], 16).expect("independent width"), row.width_bits);
        assert_eq!(u32::from_str_radix(captured[3], 16).expect("independent height"), row.height_bits);
    }
    rows
}

pub fn canonical_names(base: &[String]) -> Vec<String> {
    assert_eq!(base.len(), 158, "canonical base prefix must remain stable");
    let latest = latest_rows();
    for name in base {
        assert!(latest.iter().any(|row| &row.name == name), "missing latest entity {name}");
    }
    let mut names = base.to_vec();
    names.extend(latest.into_iter().filter_map(|row| {
        (!base.contains(&row.name)).then_some(row.name)
    }));
    assert_eq!(names.len(), 161, "append-only canonical entity census");
    assert_eq!(&names[158..], ["minecraft:cushion", "minecraft:poplar_boat", "minecraft:poplar_chest_boat"]);
    names
}
