//! `crate::poi_storage` against **vanilla's own bytes** (issue
//! [#303](https://github.com/matteopolak/lodestone/issues/303)'s second half).
//!
//! # Why this file is the load-bearing one
//!
//! `poi_persistence_round_trip.rs` proves `decode(encode(x)) == x` through this
//! crate's own writer, which — per this repo's own evidence standard — two
//! symmetric misunderstandings satisfy identically. So the expected values here
//! come from **outside this repo**: `.cache/mc/survival/world`, a world a real
//! 26.2 server wrote (seed −195764831), read with a foreign parser — Python's
//! stdlib `struct`, sharing no line of code with anything in this workspace.
//! Every number below was printed by that parser before a line of
//! `poi_storage.rs` existed.
//!
//! # The census the foreign reader produced
//!
//! Overworld `dimensions/minecraft/overworld/poi/`: **29** region files (many
//! legitimately empty — a 0-byte file is vanilla's own "no POI in this region
//! yet", not corruption), **124** chunks carrying a `Sections` compound, **150**
//! sections, **210** records. `DataVersion` is `4903` throughout, matching
//! [`lodestone_anvil::level_dat::DATA_VERSION_26_2`]. Per-type:
//!
//! | type | count |
//! |---|---|
//! | `minecraft:fisherman` | 127 |
//! | `minecraft:home` | 43 |
//! | `minecraft:bee_nest` | 23 |
//! | `minecraft:nether_portal` | 6 |
//! | `minecraft:farmer` | 4 |
//! | `minecraft:meeting` | 3 |
//! | `minecraft:shepherd` | 3 |
//! | `minecraft:cartographer` | 1 |
//!
//! `free_tickets` distribution: absent (reads as `0`) **44** times, explicit
//! `1` **163** times, explicit `28` **2** times, explicit `29` **1** time — real
//! villagers mid-claim on a real bell (`minecraft:meeting`, `maxTickets 32`),
//! not a synthetic fixture. `the_nether/poi/`: **4** region files, **1** chunk,
//! **1** section, **6** records, all `minecraft:nether_portal`, all with
//! `free_tickets` absent.
//!
//! # `#[ignore]`d, and why that is not a hole
//!
//! It needs `.cache/mc/survival/world`, which is **not repo state**. Same
//! treatment as `entity_nbt_vanilla_oracle.rs`, its direct precedent. Run it
//! with:
//! ```text
//! cargo test -p lodestone-server --test poi_vanilla_oracle -- --ignored --nocapture
//! ```

use std::collections::BTreeMap;
use std::path::PathBuf;

use lodestone_anvil::region::RegionFile;
use lodestone_core::{Nbt, Reader, read_named_nbt};
use lodestone_server::dimension::Dimension;
use lodestone_server::poi_storage::PoiStorage;

fn oracle_world_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.cache/mc/survival/world")
}

/// Every `(local pos, root NBT)` in `dir`'s region set, read through
/// `lodestone-anvil`'s container — the one piece shared with the code under
/// test, and it is pinned separately against real `.mca` files by that
/// crate's own tests. The *schema*, which this file is about, is not shared
/// with anything here.
///
/// Skips a region file under 8 KiB: a real 26.2 server writes a legitimate
/// 0-byte file for "no POI recorded in this region yet" rather than omitting
/// it, and that is not the same thing as a corrupt file — `RegionFile::parse`
/// would (correctly) reject it, so this survey steps around files this small
/// rather than trying to parse them.
fn oracle_chunks(dir: &std::path::Path) -> Vec<Nbt> {
    let mut out = Vec::new();
    let entries =
        std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    let mut paths: Vec<PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|x| x.to_str()) == Some("mca"))
        .collect();
    paths.sort();
    for path in paths {
        let bytes = std::fs::read(&path).expect("read region file");
        if bytes.len() < 8192 {
            continue;
        }
        let region = RegionFile::parse(&bytes).expect("a real vanilla poi region parses");
        for local_z in 0..32u8 {
            for local_x in 0..32u8 {
                let Some(raw) = region
                    .read_chunk_nbt_bytes(local_x, local_z)
                    .expect("chunk envelope")
                else {
                    continue;
                };
                let mut reader = Reader::new(&raw);
                let (_, nbt) = read_named_nbt(&mut reader).expect("chunk NBT decodes");
                out.push(nbt);
            }
        }
    }
    out
}

fn field<'a>(nbt: &'a Nbt, key: &str) -> Option<&'a Nbt> {
    match nbt {
        Nbt::Compound(fields) => fields
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, value)| value),
        _ => None,
    }
}

/// **The gate.** Our decoder reads every POI section a real 26.2 server wrote,
/// and the per-type census matches the foreign reader's exactly.
#[test]
#[ignore = "requires .cache/mc/survival/world, a real 26.2 world this repo did not write"]
fn reads_every_poi_record_a_real_vanilla_server_wrote() {
    let dir = oracle_world_dir()
        .join("dimensions/minecraft/overworld/poi");
    let chunks = oracle_chunks(&dir);
    let with_sections = chunks
        .iter()
        .filter(|c| matches!(field(c, "Sections"), Some(Nbt::Compound(fields)) if !fields.is_empty()))
        .count();
    assert_eq!(
        with_sections, 124,
        "expected 124 chunks carrying Sections (the foreign reader's count); got {with_sections}"
    );

    let mut section_count = 0usize;
    let mut census: BTreeMap<String, usize> = BTreeMap::new();
    let mut free_tickets_absent = 0usize;
    let mut free_tickets_explicit: BTreeMap<i32, usize> = BTreeMap::new();
    let mut total_records = 0usize;

    for chunk in &chunks {
        let Some(Nbt::Compound(sections)) = field(chunk, "Sections") else {
            continue;
        };
        for (_, section) in sections {
            section_count += 1;
            let Some(Nbt::List { elements, .. }) = field(section, "Records") else {
                continue;
            };
            for record in elements {
                total_records += 1;
                let Some(Nbt::String(ty)) = field(record, "type") else {
                    panic!("a real vanilla POI record had no string type: {record:?}");
                };
                *census.entry(ty.clone()).or_default() += 1;
                match field(record, "free_tickets") {
                    None => free_tickets_absent += 1,
                    Some(Nbt::Int(v)) => *free_tickets_explicit.entry(*v).or_default() += 1,
                    other => panic!("free_tickets was {other:?}, not absent or an Int"),
                }
            }
        }
    }

    assert_eq!(
        section_count, 150,
        "expected 150 sections (the foreign reader's count); got {section_count}"
    );
    assert_eq!(
        total_records, 210,
        "the container handed us {total_records} POI records; the foreign reader found 210"
    );

    for (id, expected) in [
        ("minecraft:fisherman", 127usize),
        ("minecraft:home", 43),
        ("minecraft:bee_nest", 23),
        ("minecraft:nether_portal", 6),
        ("minecraft:farmer", 4),
        ("minecraft:meeting", 3),
        ("minecraft:shepherd", 3),
        ("minecraft:cartographer", 1),
    ] {
        assert_eq!(
            census.get(id).copied().unwrap_or(0),
            expected,
            "{id}: our decode disagrees with the foreign reader"
        );
    }

    assert_eq!(
        free_tickets_absent, 44,
        "expected 44 records with an absent free_tickets field"
    );
    assert_eq!(free_tickets_explicit.get(&1).copied().unwrap_or(0), 163);
    assert_eq!(free_tickets_explicit.get(&28).copied().unwrap_or(0), 2);
    assert_eq!(free_tickets_explicit.get(&29).copied().unwrap_or(0), 1);
}

/// The Nether's own POI set — small enough to assert every field of, and the
/// only oracle evidence this repo has that a *second* dimension's `poi/`
/// directory is laid out the way [`PoiStorage::new`] predicts
/// (`dimensions/minecraft/the_nether/poi`, not a hand-guessed path).
#[test]
#[ignore = "requires .cache/mc/survival/world, a real 26.2 world this repo did not write"]
fn the_nether_poi_set_is_six_unclaimed_portal_cells() {
    let dir = oracle_world_dir().join("dimensions/minecraft/the_nether/poi");
    let chunks = oracle_chunks(&dir);
    let mut total_records = 0usize;
    for chunk in &chunks {
        let Some(Nbt::Compound(sections)) = field(chunk, "Sections") else {
            continue;
        };
        for (_, section) in sections {
            let Some(Nbt::List { elements, .. }) = field(section, "Records") else {
                continue;
            };
            for record in elements {
                total_records += 1;
                assert_eq!(
                    field(record, "type"),
                    Some(&Nbt::String("minecraft:nether_portal".to_owned()))
                );
                assert!(
                    field(record, "free_tickets").is_none(),
                    "a nether_portal record must never carry an explicit free_tickets"
                );
            }
        }
    }
    assert_eq!(total_records, 6);
}

/// Our own container + [`PoiStorage`] schema reads the same oracle files
/// without dropping a record, and refuses none of them as an unreadable
/// `DataVersion` — the two symmetric ways a "we read some POI" claim could be
/// hollow.
#[test]
#[ignore = "requires .cache/mc/survival/world, a real 26.2 world this repo did not write"]
fn poi_storage_reads_the_full_oracle_area_without_dropping_a_record() {
    let world_dir = oracle_world_dir();
    let storage = PoiStorage::new(&world_dir, Dimension::Overworld).expect("open store");
    // Wide enough to cover every region file under the oracle's overworld
    // `poi/` directory (region indices ranged roughly -3..=10 on both axes,
    // i.e. chunk coordinates roughly -96..=352).
    let chunks = storage
        .load_area(-100..=360, -100..=360)
        .expect("load area");
    let total_records: usize = chunks
        .values()
        .flat_map(|c| c.sections.values())
        .map(|s| s.records.len())
        .sum();
    assert_eq!(
        total_records, 210,
        "PoiStorage disagrees with the foreign reader's 210-record census"
    );
}
