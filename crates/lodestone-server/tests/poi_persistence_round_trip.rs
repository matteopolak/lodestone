//! `crate::poi_storage` against the real save path, plus its consumer
//! `crate::portal::PortalIndex`.
//!
//! `poi_storage`'s unit tests cover NBT round-tripping and the occupancy
//! predicate in isolation. This file additionally proves that `PortalIndex`
//! produces and consumes these records through the region-file save path,
//! rather than an in-memory shortcut. World open/shutdown integration belongs
//! to `crate::integrated`.

use std::collections::HashMap;
use std::path::PathBuf;

use lodestone_model::BlockPos;
use lodestone_server::dimension::Dimension;
use lodestone_server::poi_storage::{PoiChunk, PoiRecord, PoiSection, PoiStorage};
use lodestone_server::portal::{PortalIndex, poi_records_for_index, restore_index_from_poi};

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-poi-round-trip-h4n1-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

/// Buckets records into the chunk/section map [`PoiStorage::save`] wants,
/// preserving each record's own `free_tickets` via
/// [`PoiSection::insert_record`] rather than resetting it.
fn chunks_from_records(records: Vec<PoiRecord>) -> HashMap<(i32, i32), PoiChunk> {
    let mut out: HashMap<(i32, i32), PoiChunk> = HashMap::new();
    for record in records {
        let chunk_pos = record.pos.chunk_pos();
        let section_y = record.pos.section_pos().y;
        let chunk = out.entry((chunk_pos.x, chunk_pos.z)).or_default();
        chunk
            .sections
            .entry(section_y)
            .or_insert_with(PoiSection::new)
            .insert_record(record);
    }
    out
}

/// The end-to-end property: an index built from a live game session, saved
/// through the real container, reloaded in a fresh process, and rebuilt —
/// comes back holding the same cells, per dimension.
///
/// Pairwise-distinct block positions across both dimensions and across chunk
/// **and** section boundaries, so a transposition of chunk-x/chunk-z or
/// section-y cannot survive unnoticed — the same standard
/// `entity_nbt_vanilla_oracle.rs` holds itself to.
#[test]
fn a_portal_index_round_trips_through_the_poi_store() {
    let overworld_cells = [
        BlockPos::new(-385, 71, -897),
        BlockPos::new(68, 103, 26),
        BlockPos::new(4001, -40, -19),
    ];
    let nether_cells = [BlockPos::new(-56, 74, -107), BlockPos::new(12, 90, 900)];

    let index = PortalIndex::new();
    index.extend(Dimension::Overworld, overworld_cells);
    index.extend(Dimension::Nether, nether_cells);

    let overworld_dir = tempdir("overworld");
    let nether_dir = tempdir("nether");
    let overworld_storage =
        PoiStorage::new(&overworld_dir, Dimension::Overworld).expect("create overworld store");
    let nether_storage =
        PoiStorage::new(&nether_dir, Dimension::Nether).expect("create nether store");

    let overworld_records = poi_records_for_index(&index, Dimension::Overworld);
    let nether_records = poi_records_for_index(&index, Dimension::Nether);
    assert_eq!(overworld_records.len(), overworld_cells.len());
    assert_eq!(nether_records.len(), nether_cells.len());
    // Portal records have no free tickets, so every converted record starts
    // fully occupied in the ticket sense.
    for record in overworld_records.iter().chain(nether_records.iter()) {
        assert_eq!(record.free_tickets, 0);
        assert_eq!(record.poi_type.path(), "nether_portal");
    }

    let written_overworld = overworld_storage
        .save(&chunks_from_records(overworld_records))
        .expect("save overworld");
    let written_nether = nether_storage
        .save(&chunks_from_records(nether_records))
        .expect("save nether");
    assert_eq!(written_overworld, overworld_cells.len());
    assert_eq!(written_nether, nether_cells.len());

    // Reopen the stores fresh — proves the save actually reached disk rather
    // than the same in-memory handle simply remembering what it wrote.
    let reopened_overworld =
        PoiStorage::new(&overworld_dir, Dimension::Overworld).expect("reopen overworld store");
    let reopened_nether =
        PoiStorage::new(&nether_dir, Dimension::Nether).expect("reopen nether store");

    let cx_span = -260..=260;
    let cz_span = -260..=260;
    let overworld_chunks = reopened_overworld
        .load_area(cx_span.clone(), cz_span.clone())
        .expect("load overworld area");
    let nether_chunks = reopened_nether
        .load_area(cx_span, cz_span)
        .expect("load nether area");

    let rebuilt_overworld = restore_index_from_poi(
        Dimension::Overworld,
        overworld_chunks.values().flat_map(|c| c.sections.values()),
    );
    let rebuilt_nether = restore_index_from_poi(
        Dimension::Nether,
        nether_chunks.values().flat_map(|c| c.sections.values()),
    );

    let mut got_overworld = rebuilt_overworld.cells(Dimension::Overworld);
    got_overworld.sort_by_key(|p| (p.x, p.y, p.z));
    let mut want_overworld = overworld_cells.to_vec();
    want_overworld.sort_by_key(|p| (p.x, p.y, p.z));
    assert_eq!(got_overworld, want_overworld);

    let mut got_nether = rebuilt_nether.cells(Dimension::Nether);
    got_nether.sort_by_key(|p| (p.x, p.y, p.z));
    let mut want_nether = nether_cells.to_vec();
    want_nether.sort_by_key(|p| (p.x, p.y, p.z));
    assert_eq!(got_nether, want_nether);

    // Dimensions must not cross-contaminate: a portal indexed for the Nether
    // must not surface when rebuilding the overworld's own index.
    assert!(rebuilt_overworld.cells(Dimension::Nether).is_empty());
    assert!(rebuilt_nether.cells(Dimension::Overworld).is_empty());

    let _ = std::fs::remove_dir_all(&overworld_dir);
    let _ = std::fs::remove_dir_all(&nether_dir);
}

/// [`restore_index_from_poi`] must take only `nether_portal`-typed records —
/// a POI store may (once something populates it) also hold workstation or
/// bed records, none of which belong in a portal index. Without this filter
/// every villager's bed would misroute a nether trip.
#[test]
fn restoring_a_portal_index_ignores_non_portal_poi_types() {
    let mut section = PoiSection::new();
    let portal_pos = BlockPos::new(10, 70, 10);
    let home_pos = BlockPos::new(20, 70, 20);
    section.add(portal_pos, "minecraft:nether_portal".parse().expect("valid"));
    section.add(home_pos, "minecraft:home".parse().expect("valid"));

    let index = restore_index_from_poi(Dimension::Overworld, std::iter::once(&section));
    assert_eq!(index.cells(Dimension::Overworld), vec![portal_pos]);
}
