//! Manual million-chunk parity-manifest gate. It intentionally remains ignored:
//! a full 1001² run is an external oracle job, not a regular unit test.
mod support { pub mod large_parity_manifest; }

use std::{fs::File, io::{BufReader, Read, Seek, SeekFrom}};
use lodestone_core::Reader;
use lodestone_server::{ChunkSource, ServerDirective, ServerProtocol, overworld_chunk_source};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};
use support::large_parity_manifest::{HEADER_BYTES, read_header, payload_digest_from_header, verify_payload};

#[test]
fn sha256_control_and_bit_flip_are_detected() {
    use support::large_parity_manifest::sha256;
    assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "known external SHA-256 control");
    let payload = [1u8,2]; let good = sha256(&payload);
    verify_payload(&payload[..], 1, good).expect("control payload must authenticate");
    let mut corrupt = payload; corrupt[1] ^= 1;
    assert!(verify_payload(&corrupt[..], 1, good).is_err(), "one changed fingerprint bit must be detected");
}

#[test]
fn bounded_shard_header_control_is_accepted() {
    let mut raw = [0u8; HEADER_BYTES];
    raw[..8].copy_from_slice(b"LWP26P02");
    raw[8..10].copy_from_slice(&2u16.to_be_bytes());
    raw[10..12].copy_from_slice(&(HEADER_BYTES as u16).to_be_bytes());
    raw[12..14].copy_from_slice(&1u16.to_be_bytes());
    raw[14..16].copy_from_slice(&2u16.to_be_bytes());
    raw[16..20].copy_from_slice(&776u32.to_be_bytes());
    raw[20..28].copy_from_slice(&42i64.to_be_bytes());
    for (offset, value) in [(28, -500i32), (32, 500), (36, -500), (40, 500), (44, -2), (48, 1), (52, 7), (56, 8)] {
        raw[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    raw[60..68].copy_from_slice(&8u64.to_be_bytes());
    raw[68..100].copy_from_slice(&support::large_parity_manifest::sha256(
        b"lodestone.worldgen.large-parity.manifest/v2",
    ));
    let h = read_header(&raw[..]).expect("valid bounded shard header");
    assert_eq!((h.cx0, h.cx1, h.cz0, h.cz1, h.count), (-2, 1, 7, 8, 8));
}

/// Reads any authenticated shard strictly sequentially and uses only one 2-byte
/// expected fingerprint at a time. Set `LODESTONE_LARGE_PARITY_MANIFEST` to a
/// shard or merged 1001² manifest after the production packet encoder has a
/// comparison seam.
#[test]
#[ignore = "manual external oracle comparison; see docs/worldgen-large-parity.md"]
fn parity_manifest_streams_before_rust_comparison() {
    let path = std::env::var("LODESTONE_LARGE_PARITY_MANIFEST").expect("set LODESTONE_LARGE_PARITY_MANIFEST=/absolute/path/to/merged.lwp");
    let mut raw_header = [0; HEADER_BYTES]; let mut f = File::open(&path).expect("open manifest"); f.read_exact(&mut raw_header).expect("read header");
    let h = read_header(&raw_header[..]).expect("valid parity shard header");
    if std::env::var_os("LODESTONE_LARGE_PARITY_REQUIRE_FULL_GRID").is_some() {
        assert_eq!((h.cx0,h.cx1,h.cz0,h.cz1,h.count), (-500,500,-500,500,1_002_001));
    }
    let mut payload_file = File::open(&path).expect("reopen manifest"); payload_file.seek(SeekFrom::Start(HEADER_BYTES as u64)).expect("seek payload");
    verify_payload(BufReader::new(payload_file), h.count, payload_digest_from_header(&raw_header)).expect("payload integrity");
    let mut payload_file = File::open(&path).expect("reopen manifest payload");
    payload_file.seek(SeekFrom::Start(HEADER_BYTES as u64)).expect("seek payload");
    let mut expected = BufReader::new(payload_file);
    let source = overworld_chunk_source(42);
    let max_chunks = std::env::var("LODESTONE_LARGE_PARITY_MAX_CHUNKS")
        .ok().and_then(|value| value.parse::<u64>().ok()).unwrap_or(h.count);
    let limit = max_chunks.min(h.count);
    let width = (h.cx1 - h.cx0 + 1) as u64;
    let mut fingerprint = [0u8; 2];
    for index in 0..limit {
        expected.read_exact(&mut fingerprint).expect("manifest fingerprint");
        let cx = h.cx0 + (index % width) as i32;
        let cz = h.cz0 + (index / width) as i32;
        let directive = V770ServerProtocol.encode_chunk(cx, cz, &source.column(cx, cz));
        let payload = match directive {
            ServerDirective::Send { packet_id, payload } => {
                assert_eq!(packet_id, lodestone_v26_2::packet_ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT);
                payload
            }
            other => panic!("production chunk encoder returned {other:?} at ({cx},{cz})"),
        };
        if index == 0 {
            if let Some(path) = std::env::var_os("LODESTONE_LARGE_PARITY_PACKET_OUT") {
                std::fs::write(&path, &payload).expect("write requested Lodestone packet capture");
            }
        }
        let full = support::large_parity_manifest::sha256(&payload);
        if full[..2] != fingerprint {
            let packet_summary = std::env::var_os("LODESTONE_LARGE_PARITY_REFERENCE_PACKET")
                .map(|path| packet_difference_summary(&std::fs::read(path).expect("read authoritative packet capture"), &payload));
            panic!(
                "large packet parity mismatch at ({cx},{cz}) after {index} matching chunks: reference fingerprint {:02x}{:02x}, Lodestone SHA-256 {}{}",
                fingerprint[0], fingerprint[1], hex(&full), packet_summary.as_deref().unwrap_or(""),
            );
        }
        if (index + 1) % 256 == 0 || index + 1 == limit {
            eprintln!("large packet parity: compared {}/{} chunks (batch boundary at ({cx},{cz}))", index + 1, limit);
        }
    }
    if limit < h.count {
        eprintln!("large packet parity: bounded pilot completed successfully at {} chunks; full grid remains pending", limit);
    }
}

/// Reads two independently emitted packet bodies through the production client
/// decoder and makes the first oracle mismatch actionable without retaining a
/// full corpus of reference packets.
fn packet_difference_summary(reference: &[u8], actual: &[u8]) -> String {
    let reference_len = reference.len();
    let actual_len = actual.len();
    fn decode(bytes: &[u8]) -> LevelChunkWithLight {
        let mut r = Reader::new(bytes);
        let decoded = LevelChunkWithLight::decode(&mut r, &ChunkShape::overworld_1_21())
            .expect("packet capture must decode with the production client codec");
        r.ensure_empty().expect("packet capture must have no trailing bytes");
        decoded
    }
    let reference = decode(reference);
    let actual = decode(actual);
    let mut differing_blocks = 0usize;
    let mut differing_biomes = 0usize;
    let mut non_air_reference = 0usize;
    let mut non_air_actual = 0usize;
    for section in 0..24 {
        let reference_section = reference.column.section(section);
        let actual_section = actual.column.section(section);
        for cell in 0..4096 {
            let reference_block = reference_section.map_or(0, |s| s.block_states().get(cell));
            let actual_block = actual_section.map_or(0, |s| s.block_states().get(cell));
            non_air_reference += usize::from(reference_block != 0);
            non_air_actual += usize::from(actual_block != 0);
            differing_blocks += usize::from(reference_block != actual_block);
        }
        for cell in 0..64 {
            let reference_biome = reference_section.map_or(0, |s| s.biomes().get(cell));
            let actual_biome = actual_section.map_or(0, |s| s.biomes().get(cell));
            differing_biomes += usize::from(reference_biome != actual_biome);
        }
    }
    format!(
        "; captured-packet diagnosis: reference={} bytes, Lodestone={} bytes, block cells differ={differing_blocks}, biome cells differ={differing_biomes}, non-air reference={non_air_reference}, Lodestone={non_air_actual}",
        reference_len, actual_len,
    )
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
