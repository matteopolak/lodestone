//! Manual million-chunk parity-manifest gate. It intentionally remains ignored:
//! a full 1001² run is an external oracle job, not a regular unit test.
mod support { pub mod large_parity_manifest; }

use std::{fs::File, io::{BufReader, Read, Seek, SeekFrom}};
use lodestone_core::Reader;
use lodestone_server::{ChunkSource, ServerDirective, ServerProtocol, overworld_chunk_source};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};
use support::large_parity_manifest::{HEADER_BYTES, read_header, payload_digest_from_header, semantic_digest, semantic_record, verify_payload};

#[test]
fn sha256_control_and_bit_flip_are_detected() {
    use support::large_parity_manifest::sha256;
    assert_eq!(hex(&sha256(b"abc")), "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad", "known external SHA-256 control");
    let payload = [1u8; 32]; let good = sha256(&payload);
    verify_payload(&payload[..], 1, good).expect("control payload must authenticate");
    let mut corrupt = payload; corrupt[1] ^= 1;
    assert!(verify_payload(&corrupt[..], 1, good).is_err(), "one changed fingerprint bit must be detected");
}

#[test]
fn bounded_shard_header_control_is_accepted() {
    let mut raw = [0u8; HEADER_BYTES];
    raw[..8].copy_from_slice(b"LWP26P03");
    raw[8..10].copy_from_slice(&3u16.to_be_bytes());
    raw[10..12].copy_from_slice(&(HEADER_BYTES as u16).to_be_bytes());
    raw[12..14].copy_from_slice(&2u16.to_be_bytes());
    raw[14..16].copy_from_slice(&3u16.to_be_bytes());
    raw[16..20].copy_from_slice(&776u32.to_be_bytes());
    raw[20..28].copy_from_slice(&42i64.to_be_bytes());
    for (offset, value) in [(28, -500i32), (32, 500), (36, -500), (40, 500), (44, -2), (48, 1), (52, 7), (56, 8)] {
        raw[offset..offset + 4].copy_from_slice(&value.to_be_bytes());
    }
    raw[60..68].copy_from_slice(&8u64.to_be_bytes());
    raw[68..70].copy_from_slice(&32u16.to_be_bytes());
    raw[72..104].copy_from_slice(&support::large_parity_manifest::sha256(
        b"lodestone.worldgen.large-parity.manifest/v3/semantic",
    ));
    raw[104..136].copy_from_slice(&[7; 32]);
    let h = read_header(&raw[..]).expect("valid bounded shard header");
    assert_eq!((h.cx0, h.cx1, h.cz0, h.cz1, h.count), (-2, 1, 7, 8, 8));
    assert_eq!(h.frozen_world, [7; 32], "the validated frozen-world identity remains available to the gate");
}

#[test]
fn v2_raw_packet_manifest_is_rejected_not_reinterpreted() {
    let mut raw = [0u8; HEADER_BYTES];
    raw[..8].copy_from_slice(b"LWP26P02");
    let error = read_header(&raw[..]).expect_err("raw v2 fingerprints must not enter the semantic gate");
    assert!(error.to_string().contains("v2"), "the migration failure must name the rejected format: {error}");
}

/// Cross-language control: Java emits both the authoritative packet body and
/// its canonical semantic bytes for one frozen chunk. Rust must decode that
/// same body into byte-identical canonical bytes before their SHA-256 can be
/// admitted to a v3 manifest.
#[test]
#[ignore = "requires a one-chunk Java frozen-world export; see docs/worldgen-large-parity.md"]
fn java_and_rust_canonical_records_agree() {
    let packet_path = std::env::var("LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_PACKET")
        .expect("set LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_PACKET to Java's one-chunk packet body");
    let record_path = std::env::var("LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_RECORD")
        .expect("set LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_RECORD to Java's canonical record");
    let manifest_path = std::env::var("LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_MANIFEST")
        .expect("set LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_MANIFEST to Java's one-chunk v3 manifest");
    let java_record = std::fs::read(record_path).expect("read Java canonical record");
    let decoded = packet_decode(&std::fs::read(packet_path).expect("read Java packet body"));
    let rust_record = semantic_record(&decoded);
    if rust_record != java_record {
        let first = rust_record.iter().zip(&java_record).position(|(left, right)| left != right).unwrap_or(rust_record.len().min(java_record.len()));
        panic!("canonical semantic bytes differ at offset {first}: Java length {}, Rust length {}, Java byte {:?}, Rust byte {:?}", java_record.len(), rust_record.len(), java_record.get(first), rust_record.get(first));
    }
    let mut manifest = File::open(manifest_path).expect("open Java v3 manifest");
    let mut raw_header = [0; HEADER_BYTES]; manifest.read_exact(&mut raw_header).expect("read Java manifest header");
    let header = read_header(&raw_header[..]).expect("validate Java manifest header");
    assert_eq!(header.count, 1, "cross-language control must use exactly one chunk");
    let mut java_digest = [0; 32]; manifest.read_exact(&mut java_digest).expect("read Java semantic digest");
    assert_eq!(support::large_parity_manifest::sha256(&java_record), java_digest, "Java manifest digest must authenticate Java canonical bytes");
    assert_eq!(semantic_digest(&decoded), java_digest, "Rust digest must authenticate the same canonical bytes");
}

/// Reads any authenticated frozen-world shard strictly sequentially and uses
/// only one full semantic digest at a time.
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
    let mut expected_digest = [0u8; 32];
    for index in 0..limit {
        expected.read_exact(&mut expected_digest).expect("manifest semantic digest");
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
        let decoded = packet_decode(&payload);
        let full = semantic_digest(&decoded);
        if full != expected_digest {
            let packet_summary = std::env::var_os("LODESTONE_LARGE_PARITY_REFERENCE_PACKET")
                .map(|path| packet_difference_summary(&std::fs::read(path).expect("read authoritative packet capture"), &payload));
            panic!(
                "large semantic parity mismatch at ({cx},{cz}) after {index} matching chunks: reference SHA-256 {}, Lodestone SHA-256 {}{}",
                hex(&expected_digest), hex(&full), packet_summary.as_deref().unwrap_or(""),
            );
        }
        if (index + 1) % 256 == 0 || index + 1 == limit {
            eprintln!("large semantic parity: compared {}/{} chunks (batch boundary at ({cx},{cz}))", index + 1, limit);
        }
    }
    if limit < h.count {
        eprintln!("large semantic parity: bounded pilot completed successfully at {} chunks; full grid remains pending", limit);
    }
}

fn packet_decode(bytes: &[u8]) -> LevelChunkWithLight {
    let mut reader = Reader::new(bytes);
    let decoded = LevelChunkWithLight::decode(&mut reader, &ChunkShape::overworld_1_21())
        .expect("packet capture must decode with the production client codec");
    reader.ensure_empty().expect("packet capture must have no trailing bytes");
    decoded
}

/// Reads two independently emitted packet bodies through the production client
/// decoder and makes the first oracle mismatch actionable without retaining a
/// full corpus of reference packets.
fn packet_difference_summary(reference: &[u8], actual: &[u8]) -> String {
    let reference_len = reference.len();
    let actual_len = actual.len();
    let reference = packet_decode(reference);
    let actual = packet_decode(actual);
    let mut differing_blocks = 0usize;
    let mut differing_biomes = 0usize;
    let mut non_air_reference = 0usize;
    let mut non_air_actual = 0usize;
    let mut first_block_difference = None;
    let mut differing_state_pairs = std::collections::HashMap::<(u32, u32), usize>::new();
    for section in 0..24 {
        let reference_section = reference.column.section(section);
        let actual_section = actual.column.section(section);
        for cell in 0..4096 {
            let reference_block = reference_section.map_or(0, |s| s.block_states().get(cell));
            let actual_block = actual_section.map_or(0, |s| s.block_states().get(cell));
            non_air_reference += usize::from(reference_block != 0);
            non_air_actual += usize::from(actual_block != 0);
            differing_blocks += usize::from(reference_block != actual_block);
            if reference_block != actual_block {
                *differing_state_pairs.entry((reference_block, actual_block)).or_default() += 1;
            }
            if first_block_difference.is_none() && reference_block != actual_block {
                let x = cell % 16;
                let z = (cell / 16) % 16;
                let y = -64 + section as i32 * 16 + (cell / 256) as i32;
                first_block_difference = Some((x, y, z, reference_block, actual_block));
            }
        }
        for cell in 0..64 {
            let reference_biome = reference_section.map_or(0, |s| s.biomes().get(cell));
            let actual_biome = actual_section.map_or(0, |s| s.biomes().get(cell));
            differing_biomes += usize::from(reference_biome != actual_biome);
        }
    }
    let first_block_difference = first_block_difference.map_or_else(
        || "none".to_owned(),
        |(x, y, z, reference, actual)| format!(
            "local ({x},{y},{z}): {} ({reference}) vs {} ({actual})",
            state_label(reference),
            state_label(actual),
        ),
    );
    let mut common_state_pairs = differing_state_pairs.into_iter().collect::<Vec<_>>();
    common_state_pairs.sort_unstable_by(|left, right| right.1.cmp(&left.1));
    let common_state_pairs = common_state_pairs
        .into_iter()
        .take(4)
        .map(|((reference, actual), count)| format!("{count}× {} vs {}", state_label(reference), state_label(actual)))
        .collect::<Vec<_>>()
        .join("; ");
    format!(
        "; captured-packet diagnosis: reference={} bytes, Lodestone={} bytes, block cells differ={differing_blocks}, biome cells differ={differing_biomes}, non-air reference={non_air_reference}, Lodestone={non_air_actual}, first block difference={first_block_difference}, common state pairs={common_state_pairs}",
        reference_len, actual_len,
    )
}

fn state_label(id: u32) -> String {
    let name = lodestone_data::block_states::block_name(id).unwrap_or("unknown");
    let properties = lodestone_data::block_states::properties(id).unwrap_or_default();
    if properties.is_empty() {
        name.to_owned()
    } else {
        let properties = properties
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join(",");
        format!("{name}[{properties}]")
    }
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
