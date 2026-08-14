//! The self-contained "write a region, read it back" round trip, through an
//! actual file on disk this time (`src/region.rs`'s unit tests cover the
//! in-memory byte-buffer version). This intentionally checks `decode(encode(x)) == x` against our
//! own writer, which per this repo's own standing rule proves our two
//! halves agree with each other and nothing more — see
//! `tests/region_real_world.rs` and `tests/level_dat_real_world.rs` for the
//! evidence that actually matters, reading files this crate never wrote.

use lodestone_anvil::compression::CompressionScheme;
use lodestone_anvil::region::{build_region_from_nbt, ChunkLocation, RegionFile};
use lodestone_core::Nbt;
use std::collections::BTreeMap;

fn chunk_nbt(x: i32, z: i32) -> Nbt {
    Nbt::Compound(vec![
        (
            "Level".to_string(),
            Nbt::Compound(vec![
                ("xPos".to_string(), Nbt::Int(x)),
                ("zPos".to_string(), Nbt::Int(z)),
                ("Status".to_string(), Nbt::String("full".to_string())),
            ]),
        ),
        ("DataVersion".to_string(), Nbt::Int(4903)),
    ])
}

#[test]
fn writes_a_region_to_disk_and_reads_it_back_across_a_negative_region_boundary() {
    // Chunks spanning region (-1,-1) through (0,0) around the origin, so
    // the round trip exercises `region_and_local`'s negative-coordinate
    // floor behaviour end to end, not just as an isolated arithmetic check.
    // All four chunks belong to region (-1,-1) here (chunk coords -1..-32
    // map to region -1's locals 31..0), which is why they're all written
    // into the same file — `build_region` is explicitly a single-region
    // primitive.
    let mut chunks = BTreeMap::new();
    for (x, z) in [(-1, -1), (-1, -2), (-2, -1), (-32, -32)] {
        chunks.insert((x, z), chunk_nbt(x, z));
    }

    let built = build_region_from_nbt(&chunks, CompressionScheme::Zlib, 1_700_000_000)
        .expect("builds a region file's bytes");
    assert!(
        built.external.is_empty(),
        "none of these tiny chunks should need external storage"
    );

    let dir = std::env::temp_dir().join(format!(
        "lodestone-anvil-region-container-test-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    let path = dir.join("r.-1.-1.mca");
    std::fs::write(&path, &built.bytes).expect("write region file to disk");

    let region = RegionFile::read_from_file(&path).expect("read region file back from disk");
    std::fs::remove_file(&path).ok();
    std::fs::remove_dir(&dir).ok();

    for (x, z) in [(-1, -1), (-1, -2), (-2, -1), (-32, -32)] {
        let local_x = (x & 31) as u8;
        let local_z = (z & 31) as u8;

        assert!(
            region.has_chunk(local_x, local_z).expect("in range"),
            "chunk ({x},{z}) -> local ({local_x},{local_z}) should be present after round trip"
        );
        assert_eq!(
            region.timestamp(local_x, local_z).expect("in range"),
            Some(1_700_000_000)
        );

        let raw = region
            .read_chunk_nbt_bytes(local_x, local_z)
            .expect("reads")
            .expect("present");
        let mut reader = lodestone_core::Reader::new(&raw);
        let (_, decoded) = lodestone_core::read_named_nbt(&mut reader).expect("decodes");
        assert_eq!(decoded, chunk_nbt(x, z), "round-tripped NBT for chunk ({x},{z})");
    }
}

#[test]
fn mixed_compression_schemes_in_one_file_each_decode_correctly() {
    // Nothing in the container format forbids per-chunk scheme mixing (each
    // chunk carries its own scheme byte) — a server that changes
    // `region-file-compression` mid-life produces exactly this. Build one
    // by hand via the lower-level `build_region`/`ChunkToWrite` API (rather
    // than `build_region_from_nbt`, which applies one scheme to everything)
    // to prove the reader doesn't assume a single scheme for the whole
    // file.
    use lodestone_anvil::region::{build_region, ChunkToWrite};

    let payload_a = {
        let mut w = lodestone_core::Writer::default();
        lodestone_core::write_named_nbt(&mut w, "", &chunk_nbt(0, 0)).expect("encodes");
        w.into_vec()
    };
    let payload_b = {
        let mut w = lodestone_core::Writer::default();
        lodestone_core::write_named_nbt(&mut w, "", &chunk_nbt(1, 0)).expect("encodes");
        w.into_vec()
    };

    let entries = vec![
        ChunkToWrite {
            chunk_x: 0,
            chunk_z: 0,
            compressed: CompressionScheme::Zlib.compress(&payload_a).expect("zlib compress"),
            scheme: CompressionScheme::Zlib,
            timestamp: 1,
        },
        ChunkToWrite {
            chunk_x: 1,
            chunk_z: 0,
            compressed: CompressionScheme::Gzip.compress(&payload_b).expect("gzip compress"),
            scheme: CompressionScheme::Gzip,
            timestamp: 2,
        },
    ];
    let built = build_region(&entries).expect("builds");
    let region = RegionFile::parse(&built.bytes).expect("parses");

    let raw_a = region
        .read_chunk_nbt_bytes(0, 0)
        .expect("reads")
        .expect("present");
    let mut reader_a = lodestone_core::Reader::new(&raw_a);
    let (_, decoded_a) = lodestone_core::read_named_nbt(&mut reader_a).expect("decodes");
    assert_eq!(decoded_a, chunk_nbt(0, 0));

    let raw_b = region
        .read_chunk_nbt_bytes(1, 0)
        .expect("reads")
        .expect("present");
    let mut reader_b = lodestone_core::Reader::new(&raw_b);
    let (_, decoded_b) = lodestone_core::read_named_nbt(&mut reader_b).expect("decodes");
    assert_eq!(decoded_b, chunk_nbt(1, 0));

    assert_eq!(
        region.locate_chunk(0, 0).expect("in range"),
        Some(ChunkLocation {
            sector_number: 2,
            sector_count: 1
        })
    );
    assert_eq!(
        region.locate_chunk(1, 0).expect("in range"),
        Some(ChunkLocation {
            sector_number: 3,
            sector_count: 1
        })
    );
}
