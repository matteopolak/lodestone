//! Manual large-grid parity-manifest gate. It intentionally remains ignored:
//! a full 501² run is an external oracle job, not a regular unit test.
mod support { pub mod large_parity_manifest; }

use std::{collections::{BTreeMap, BTreeSet}, fs::File, io::{BufReader, Read, Seek, SeekFrom}, path::{Path, PathBuf}};
use lodestone_core::{Reader, Writer};
use lodestone_server::{
    ChunkColumn, ChunkSource, ServerDirective, ServerProtocol,
    end_chunk_source,
    nether_chunk_source, overworld_chunk_source, retained_chunk_source_for_view_radius,
};
use lodestone_server::dimension::Dimension as ServerDimension;
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};
use lodestone_worldgen_parity::lifecycle::{
    LifecycleCompletion, LifecycleMaterializer, LifecycleWorldgenSource,
};
use support::large_parity_manifest::{Dimension, HEADER_BYTES, canonical_nbt, read_header, payload_digest_from_header, semantic_digest, semantic_digest_for_dimension, semantic_digest_v5_for_dimension, semantic_record, semantic_record_for_dimension, semantic_record_v5_for_dimension, verify_payload};

type ChunkPos = (i32, i32);

/// One coordinate admission from the independent lifecycle capture.
#[derive(Debug, Clone, Copy)]
struct LifecycleAdmission {
    replay_index: usize,
    chunk: ChunkPos,
}

/// One globally unique completion event, derived from the repeated per-target
/// observations in `replay-completion-order.tsv`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LifecycleEvent {
    source: ChunkPos,
    stage: LifecycleCompletion,
    completion_sequence: u64,
    admission_index: usize,
}

/// Authenticated, compact input to the partial-manifest materializer.
#[derive(Debug)]
struct LifecycleCapture {
    admissions: Vec<LifecycleAdmission>,
    target_order: Vec<ChunkPos>,
    /// FEATURES events in replay admission order. Their captured completion
    /// sequence is retained for diagnostics/authentication, but admission
    /// order is the only order allowed to drive effects.
    feature_events: Vec<LifecycleEvent>,
    /// Number of unique FULL events in the telemetry. FULL is retained as a
    /// diagnostic fact and deliberately has no effect on sealed replay state.
    full_event_count: usize,
}

/// Checks that the independent capture's FEATURES completion sequence agrees
/// with replay admission order. The served replay uses admission order because
/// that is the deterministic source order; an externally reordered callback
/// stream must fail authentication rather than change the materialized world.
fn validate_feature_event_order(
    feature_events_by_sequence: &[LifecycleEvent],
    admissions: &[LifecycleAdmission],
) -> Result<(), String> {
    if feature_events_by_sequence.len() != admissions.len() {
        return Err(format!(
            "expected one FEATURES event per admission, got {} events for {} admissions",
            feature_events_by_sequence.len(),
            admissions.len(),
        ));
    }
    let mut seen_sources = BTreeSet::new();
    for (rank, event) in feature_events_by_sequence.iter().enumerate() {
        if event.stage != LifecycleCompletion::Features {
            return Err(format!(
                "global event at completion sequence {} is not FEATURES",
                event.completion_sequence,
            ));
        }
        if event.admission_index != rank {
            return Err(format!(
                "FEATURES completion sequence rank {rank} maps to admission {}, expected {rank}",
                event.admission_index,
            ));
        }
        let admission = admissions.get(event.admission_index).ok_or_else(|| {
            format!(
                "FEATURES event for {:?} points outside the {}-entry admission replay",
                event.source,
                admissions.len(),
            )
        })?;
        if admission.chunk != event.source {
            return Err(format!(
                "FEATURES event source {:?} disagrees with replay admission {} {:?}",
                event.source, event.admission_index, admission.chunk,
            ));
        }
        if !seen_sources.insert(event.source) {
            return Err(format!(
                "FEATURES source {:?} appears more than once",
                event.source,
            ));
        }
    }
    if seen_sources.len() != admissions.len() {
        return Err(format!(
            "FEATURES source set has {} entries for {} admissions",
            seen_sources.len(),
            admissions.len(),
        ));
    }
    Ok(())
}

const LIFECYCLE_CAPTURE_SCHEMA: &str = "lodestone-worldgen-lifecycle-capture-v1";
const LIFECYCLE_REPLAY_SEQUENCE_SHA256: &str =
    "4e2eeb0217c06e7ed68e3976df04ebef5648ff1b14516141c49095d8ef15498f";
const LIFECYCLE_CAPTURE_SOURCE_SHA256: &str =
    "b47516b6f47ec74aba6201cd8d54401deb12edf94cc4272c0dd9c2b52845f9a3";
const LIFECYCLE_ACCEPTED_ROOT_SHA256_OVERWORLD: &str =
    "ade151a2bd6a5840c0548d70dd763a3f5060b301fbf2b2cf043af5de365ea4e8";
const LIFECYCLE_ACCEPTED_ROOT_SHA256_NETHER: &str =
    "c56e42d8ac751d348ff8461b4284c783f24702437039b682b37dca426b497048";
const LIFECYCLE_ACCEPTED_MANIFEST_SHA256_OVERWORLD: &str =
    "54f3a7e62ed8dbd0d976a27eefef64f6d11152d3e26b94e162e2561192f81071";
const LIFECYCLE_ACCEPTED_MANIFEST_SHA256_NETHER: &str =
    "cb4d341f6826ebf7bce49ee195618d48e314729b6bd4251b3b97124e4f948789";
const LIFECYCLE_REPLAY_FILE_SHA256: &str =
    "b8678c8a8b847a94d82bba31c9250aeace0d43e02fc309c923838e327c209986";
const LIFECYCLE_COMPLETION_FILE_SHA256: &str =
    "4af54ab035bc7febad74da7a6dffdd9c79a6b9e10c90a164523d98c5e995b1e0";
const LIFECYCLE_COMPLETION_FILE_SHA256_NETHER: &str =
    "7115c80a42320ed2ca7c3b8fe7160ea4516cdc6436a10633e212f405f2f0b52a";

impl LifecycleCapture {
    /// Loads and authenticates one external full-run capture.  The capture is
    /// intentionally not copied into the repository: its provenance and file
    /// digests bind the replay to the independently generated artifact.
    fn load(dimension: Dimension, header: &support::large_parity_manifest::Header, manifest: &Path) -> Self {
        let directory = lifecycle_capture_directory(dimension);
        let provenance_path = directory.join("provenance.txt");
        let provenance = std::fs::read_to_string(&provenance_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", provenance_path.display()));
        let fields = provenance
            .lines()
            .filter_map(|line| line.split_once('='))
            .collect::<std::collections::HashMap<_, _>>();
        let field = |name: &str| {
            fields
                .get(name)
                .copied()
                .unwrap_or_else(|| panic!("{} has no {name}", provenance_path.display()))
        };
        assert_eq!(field("capture_schema"), LIFECYCLE_CAPTURE_SCHEMA);
        assert_eq!(field("mode"), "full", "partial comparator requires the full 324-coordinate capture");
        assert_eq!(field("seed"), "42");
        assert_eq!(field("dimension"), dimension_capture_name(dimension));
        assert_eq!(field("replay_entries"), "324");
        assert_eq!(field("coordinate_order"), "tile_z-major,tile_x-major,z-major,x-major;tile_side=16");
        assert_eq!(field("capture_source_sha256"), LIFECYCLE_CAPTURE_SOURCE_SHA256);
        assert_eq!(field("replay_sequence_sha256"), LIFECYCLE_REPLAY_SEQUENCE_SHA256);
        let accepted_root_sha256 = match dimension {
            Dimension::Overworld => LIFECYCLE_ACCEPTED_ROOT_SHA256_OVERWORLD,
            Dimension::Nether => LIFECYCLE_ACCEPTED_ROOT_SHA256_NETHER,
            Dimension::End => unreachable!("the partial lifecycle gate rejects End below"),
        };
        assert_eq!(field("accepted_root_tree_sha256"), accepted_root_sha256);
        assert_eq!(field("accepted_freeze_text"), accepted_root_sha256);
        assert_eq!(field("accepted_roots_were_not_opened_for_generation"), "true");
        let accepted_manifest_sha256 = match dimension {
            Dimension::Overworld => LIFECYCLE_ACCEPTED_MANIFEST_SHA256_OVERWORLD,
            Dimension::Nether => LIFECYCLE_ACCEPTED_MANIFEST_SHA256_NETHER,
            Dimension::End => unreachable!("the partial lifecycle gate rejects End below"),
        };
        assert_eq!(field("accepted_manifest_sha256"), accepted_manifest_sha256);
        assert_eq!(file_sha256(manifest), accepted_manifest_sha256);
        assert_eq!(field("accepted_manifest_header"), format_manifest_header(header));
        assert_eq!(field("target_grid_x"), format!("{}..{}", header.cx0, header.cx1));
        assert_eq!(field("target_grid_z"), format!("{}..{}", header.cz0, header.cz1));
        let provenance_canonical = format!(
            "capture_schema={LIFECYCLE_CAPTURE_SCHEMA}\nmode=full\ndimension={}\nseed=42\naccepted_root_tree_sha256={accepted_root_sha256}\naccepted_manifest_sha256={}\naccepted_freeze_text={accepted_root_sha256}\ntarget_grid=-8..7\nmaterializer_halo=-9..8\ncoordinate_order=tile_z-major,tile_x-major,z-major,x-major;tile_side=16\nreplay_sequence_sha256={LIFECYCLE_REPLAY_SEQUENCE_SHA256}\n",
            dimension_capture_name(dimension),
            field("accepted_manifest_sha256"),
        );
        assert_eq!(
            field("provenance_sha256"),
            hex(&support::large_parity_manifest::sha256(provenance_canonical.as_bytes())),
            "capture provenance hash does not authenticate its canonical fields",
        );

        let replay_path = directory.join("replay.tsv");
        assert_eq!(file_sha256(&replay_path), LIFECYCLE_REPLAY_FILE_SHA256);
        let completion_path = directory.join("replay-completion-order.tsv");
        let completion_sha = if dimension == Dimension::Nether {
            LIFECYCLE_COMPLETION_FILE_SHA256_NETHER
        } else {
            LIFECYCLE_COMPLETION_FILE_SHA256
        };
        assert_eq!(file_sha256(&completion_path), completion_sha);

        let replay_text = std::fs::read_to_string(&replay_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", replay_path.display()));
        let replay_lines = replay_text.lines().collect::<Vec<_>>();
        assert_eq!(
            replay_lines.first().copied(),
            Some("replay_index\ttile\ttile_x\ttile_z\tx\tz\ttarget"),
            "{} has an unexpected header",
            replay_path.display()
        );
        let width = i64::from(header.cx1 - header.cx0 + 1);
        let height = i64::from(header.cz1 - header.cz0 + 1);
        assert_eq!((width, height, header.count), (16, 16, 256));
        assert_eq!(replay_lines.len(), 325);
        let mut admissions = Vec::with_capacity(324);
        let mut seen = BTreeSet::new();
        let mut captured_targets = BTreeSet::new();
        let mut canonical = String::new();
        for (line_index, line) in replay_lines.iter().skip(1).enumerate() {
            let fields = line.split('\t').collect::<Vec<_>>();
            assert_eq!(fields.len(), 7, "replay row {line_index} has {} fields", fields.len());
            let replay_index = parse_usize(fields[0], "replay index");
            assert_eq!(replay_index, line_index, "replay indices must be contiguous");
            let tile = parse_usize(fields[1], "tile");
            let tile_x = parse_usize(fields[2], "tile x");
            let tile_z = parse_usize(fields[3], "tile z");
            let x = parse_i32(fields[4], "replay x");
            let z = parse_i32(fields[5], "replay z");
            let target = parse_bool(fields[6], "replay target");
            let expected_target = header.cx0 <= x && x <= header.cx1 && header.cz0 <= z && z <= header.cz1;
            assert_eq!(target, expected_target, "replay target flag disagrees at ({x},{z})");
            assert_eq!(tile_x, usize::from(x >= header.cx1));
            assert_eq!(tile_z, usize::from(z >= header.cz1));
            assert_eq!(tile, tile_x + tile_z * 2);
            assert!(
                (header.cx0 - 1..=header.cx1 + 1).contains(&x)
                    && (header.cz0 - 1..=header.cz1 + 1).contains(&z),
                "replay coordinate ({x},{z}) is outside the one-chunk halo"
            );
            assert!(seen.insert((x, z)), "replay admits ({x},{z}) more than once");
            canonical.push_str(&format!("{replay_index}\t{x}\t{z}\n"));
            admissions.push(LifecycleAdmission {
                replay_index,
                chunk: (x, z),
            });
            if target {
                captured_targets.insert((x, z));
            }
        }
        assert_eq!(seen.len(), 324);
        let target_order = (header.cz0..=header.cz1)
            .flat_map(|z| (header.cx0..=header.cx1).map(move |x| (x, z)))
            .collect::<Vec<_>>();
        assert_eq!(captured_targets.len(), header.count as usize);
        assert!(target_order.iter().all(|target| captured_targets.contains(target)));
        assert_eq!(hex(&support::large_parity_manifest::sha256(canonical.as_bytes())), LIFECYCLE_REPLAY_SEQUENCE_SHA256);

        let completion_text = std::fs::read_to_string(&completion_path)
            .unwrap_or_else(|error| panic!("read {}: {error}", completion_path.display()));
        let completion_lines = completion_text.lines().collect::<Vec<_>>();
        assert_eq!(
            completion_lines.first().copied(),
            Some("target_index\ttarget_x\ttarget_z\tstatus\tsource_x\tsource_z\tcompletion_seq\tcompletion_state\tbefore_fence\tcenter_admission_index"),
            "{} has an unexpected header",
            completion_path.display()
        );
        assert_eq!(completion_lines.len(), 4609);
        let admission_by_chunk = admissions
            .iter()
            .map(|entry| (entry.chunk, entry.replay_index))
            .collect::<BTreeMap<_, _>>();
        let replay_by_chunk = admission_by_chunk.clone();
        let mut completions = BTreeMap::<ChunkPos, BTreeSet<(ChunkPos, LifecycleCompletion)>>::new();
        let mut events_by_key = BTreeMap::<(u64, ChunkPos, LifecycleCompletion), LifecycleEvent>::new();
        let mut event_by_sequence = BTreeMap::<u64, (ChunkPos, LifecycleCompletion, usize)>::new();
        for (line_index, line) in completion_lines.iter().skip(1).enumerate() {
            let fields = line.split('\t').collect::<Vec<_>>();
            assert_eq!(fields.len(), 10, "completion row {line_index} has {} fields", fields.len());
            let target_index = parse_usize(fields[0], "completion target index");
            let target = (parse_i32(fields[1], "completion target x"), parse_i32(fields[2], "completion target z"));
            let expected_replay_index = replay_by_chunk
                .get(&target)
                .copied()
                .unwrap_or_else(|| panic!("completion target {target:?} was not admitted"));
            assert_eq!(target_index, expected_replay_index, "completion target index must identify its replay admission");
            let stage = match fields[3] {
                "FEATURES" => LifecycleCompletion::Features,
                "FULL" => LifecycleCompletion::Full,
                other => panic!("unknown lifecycle completion {other:?}"),
            };
            let source = (parse_i32(fields[4], "completion source x"), parse_i32(fields[5], "completion source z"));
            assert!((target.0 - source.0).abs() <= 1 && (target.1 - source.1).abs() <= 1, "completion source {source:?} is outside target {target:?}'s 3x3");
            let completion_sequence = parse_u64(fields[6], "completion sequence");
            assert_eq!(fields[7], "success=true", "only successful captured completions may drive replay");
            // This is an observation about one target's packet-time fence,
            // not an instruction to replay a prefix. Final manifests are
            // sealed after the complete halo settles, so the relation is
            // intentionally parsed for schema validation and then ignored.
            let _before_fence = parse_bool(fields[8], "completion fence flag");
            let admission_index = parse_usize(fields[9], "completion admission index");
            assert!(admissions.get(admission_index).is_some(), "completion admission index {admission_index} is outside the replay");
            assert_eq!(admission_by_chunk[&source], admission_index, "completion source and admission index disagree");
            assert!(
                completions.entry(target).or_default().insert((source, stage)),
                "target {target:?} repeats the {:?} completion for {source:?}",
                stage,
            );
            let event = LifecycleEvent {
                source,
                stage,
                completion_sequence,
                admission_index,
            };
            match events_by_key.entry((completion_sequence, source, stage)) {
                std::collections::btree_map::Entry::Vacant(slot) => {
                    slot.insert(event);
                }
                std::collections::btree_map::Entry::Occupied(slot) => {
                    assert_eq!(*slot.get(), event, "repeated completion rows must describe one global event");
                }
            }
            if let Some(previous) = event_by_sequence.insert(completion_sequence, (source, stage, admission_index)) {
                assert_eq!(previous, (source, stage, admission_index), "completion sequence {completion_sequence} identifies more than one global event");
            }
        }
        assert_eq!(completions.len(), target_order.len());
        for target in &target_order {
            let rows = completions.get(target).expect("every target needs completion rows");
            assert_eq!(rows.len(), 18, "target {target:?} must have 9 FEATURES/FULL source pairs");
        }
        let events = events_by_key.into_values().collect::<Vec<_>>();
        assert_eq!(events.len(), event_by_sequence.len(), "global completion sequence must be one-to-one");
        assert_eq!(events.len(), admissions.len() * 2, "every admitted source must contribute one global FEATURES and FULL event");
        assert!(events.windows(2).all(|pair| pair[0].completion_sequence < pair[1].completion_sequence));
        let feature_events = events
            .iter()
            .filter(|event| event.stage == LifecycleCompletion::Features)
            .copied()
            .collect::<Vec<_>>();
        let full_event_count = events
            .iter()
            .filter(|event| event.stage == LifecycleCompletion::Full)
            .count();
        validate_feature_event_order(&feature_events, &admissions)
            .unwrap_or_else(|error| panic!("invalid FEATURES completion order: {error}"));
        let mut feature_events = feature_events;
        feature_events.sort_by_key(|event| event.admission_index);

        Self {
            admissions,
            target_order,
            feature_events,
            full_event_count,
        }
    }
}

fn lifecycle_capture_directory(dimension: Dimension) -> PathBuf {
    if let Some(path) = std::env::var_os("LODESTONE_LARGE_PARITY_LIFECYCLE_CAPTURE") {
        return PathBuf::from(path);
    }
    let root = std::env::var_os("LODESTONE_LARGE_PARITY_LIFECYCLE_CAPTURE_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            panic!("set LODESTONE_LARGE_PARITY_LIFECYCLE_CAPTURE to the external {} capture directory (or set LODESTONE_LARGE_PARITY_LIFECYCLE_CAPTURE_ROOT)", dimension_capture_name(dimension))
        });
    root.join(format!("{}-full-accepted-sequence", dimension_capture_name(dimension)))
}

fn dimension_capture_name(dimension: Dimension) -> &'static str {
    match dimension {
        Dimension::Overworld => "overworld",
        Dimension::Nether => "nether",
        Dimension::End => "end",
    }
}

fn format_manifest_header(header: &support::large_parity_manifest::Header) -> String {
    format!(
        "magic=LWP26P04;version={};header_bytes=256;coordinate_order=2;schema={};protocol=776;seed=42;grid_x=-250..250;grid_z=-250..250;target_x={}..{};target_z={}..{};count={}",
        header.semantic_version,
        header.semantic_version,
        header.cx0,
        header.cx1,
        header.cz0,
        header.cz1,
        header.count,
    )
}

fn file_sha256(path: &Path) -> String {
    let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("read {} for SHA-256: {error}", path.display()));
    hex(&support::large_parity_manifest::sha256(&bytes))
}

fn parse_usize(value: &str, what: &str) -> usize {
    value.parse().unwrap_or_else(|error| panic!("invalid {what} {value:?}: {error}"))
}

fn parse_u64(value: &str, what: &str) -> u64 {
    value.parse().unwrap_or_else(|error| panic!("invalid {what} {value:?}: {error}"))
}

fn parse_i32(value: &str, what: &str) -> i32 {
    value.parse().unwrap_or_else(|error| panic!("invalid {what} {value:?}: {error}"))
}

fn parse_bool(value: &str, what: &str) -> bool {
    value.parse().unwrap_or_else(|error| panic!("invalid {what} {value:?}: {error}"))
}

#[test]
fn canonical_grid_has_the_requested_square_and_one_chunk_halo() {
    use support::large_parity_manifest::{GRID_COUNT, GRID_MAX, GRID_MIN, GRID_SIDE};

    assert_eq!(GRID_SIDE, 501);
    assert_eq!(GRID_COUNT, 251_001);
    assert_eq!((GRID_MIN - 1, GRID_MAX + 1), (-251, 251));
    let halo_side = (GRID_MAX - GRID_MIN + 3) as u64;
    assert_eq!(halo_side * halo_side, 253_009);
}

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
fn lifecycle_rejects_adjacent_feature_sequence_inversion() {
    let admissions = vec![
        LifecycleAdmission { replay_index: 0, chunk: (0, 0) },
        LifecycleAdmission { replay_index: 1, chunk: (1, 0) },
    ];
    let mut swapped = vec![
        LifecycleEvent {
            source: (0, 0),
            stage: LifecycleCompletion::Features,
            completion_sequence: 10,
            admission_index: 0,
        },
        LifecycleEvent {
            source: (1, 0),
            stage: LifecycleCompletion::Features,
            completion_sequence: 20,
            admission_index: 1,
        },
    ];
    assert!(validate_feature_event_order(&swapped, &admissions).is_ok());
    // Simulate the smallest capture corruption: only adjacent callback
    // sequence numbers are exchanged. Sorting by the captured sequence now
    // exposes an admission inversion, which the loader must reject.
    (swapped[0].completion_sequence, swapped[1].completion_sequence) =
        (swapped[1].completion_sequence, swapped[0].completion_sequence);
    swapped.sort_by_key(|event| event.completion_sequence);
    assert!(
        validate_feature_event_order(&swapped, &admissions).is_err(),
        "an adjacent FEATURES sequence inversion must fail the structural gate",
    );
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
    for (offset, value) in [
        (28, support::large_parity_manifest::GRID_MIN),
        (32, support::large_parity_manifest::GRID_MAX),
        (36, support::large_parity_manifest::GRID_MIN),
        (40, support::large_parity_manifest::GRID_MAX),
        (44, -2), (48, 1), (52, 7), (56, 8),
    ] {
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
fn dimension_manifest_header_authenticates_the_resource_location() {
    let mut raw = [0u8; HEADER_BYTES];
    raw[..8].copy_from_slice(b"LWP26P04");
    raw[8..10].copy_from_slice(&4u16.to_be_bytes());
    raw[10..12].copy_from_slice(&(HEADER_BYTES as u16).to_be_bytes());
    raw[12..14].copy_from_slice(&2u16.to_be_bytes());
    raw[14..16].copy_from_slice(&4u16.to_be_bytes());
    raw[16..20].copy_from_slice(&776u32.to_be_bytes());
    raw[20..28].copy_from_slice(&42i64.to_be_bytes());
    for (offset, value) in [
        (28, support::large_parity_manifest::GRID_MIN),
        (32, support::large_parity_manifest::GRID_MAX),
        (36, support::large_parity_manifest::GRID_MIN),
        (40, support::large_parity_manifest::GRID_MAX),
        (44, 0), (48, 0), (52, 0), (56, 0),
    ] { raw[offset..offset + 4].copy_from_slice(&value.to_be_bytes()); }
    raw[60..68].copy_from_slice(&1u64.to_be_bytes());
    raw[68..70].copy_from_slice(&32u16.to_be_bytes());
    raw[72..104].copy_from_slice(&support::large_parity_manifest::sha256(
        b"lodestone.worldgen.large-parity.manifest/v4/semantic",
    ));
    raw[104..136].copy_from_slice(&[7; 32]);
    raw[168..200].copy_from_slice(&support::large_parity_manifest::sha256(b"minecraft:the_nether"));
    let h = read_header(&raw[..]).expect("valid dimension manifest header");
    assert_eq!(h.dimension, Dimension::Nether);
    raw[168] ^= 1;
    assert!(read_header(&raw[..]).is_err(), "a changed dimension identity must not be accepted");
}

#[test]
fn v2_raw_packet_manifest_is_rejected_not_reinterpreted() {
    let mut raw = [0u8; HEADER_BYTES];
    raw[..8].copy_from_slice(b"LWP26P02");
    let error = read_header(&raw[..]).expect_err("raw v2 fingerprints must not enter the semantic gate");
    assert!(error.to_string().contains("v2"), "the migration failure must name the rejected format: {error}");
}

#[test]
fn dimension_identity_is_an_external_value_and_not_a_shape_guess() {
    assert_eq!(Dimension::Overworld.name(), "minecraft:overworld");
    assert_eq!(Dimension::Nether.name(), "minecraft:the_nether");
    assert_eq!(Dimension::End.name(), "minecraft:the_end");
    assert_ne!(Dimension::Nether, Dimension::End, "equal 0..256 windows do not make dimensions interchangeable");
}

#[test]
fn dimension_packet_shapes_match_the_published_level_windows() {
    let overworld = lodestone_v26_2::packets::chunk::ChunkShape::overworld_1_21();
    let non_overworld = lodestone_v26_2::packets::chunk::ChunkShape::nether_or_end_1_21();
    assert_eq!((overworld.min_y, overworld.section_count, overworld.world_height), (-64, 24, 384));
    assert_eq!((non_overworld.min_y, non_overworld.section_count, non_overworld.world_height), (0, 16, 256));
}

/// The full-grid gate must use the same neighbour-bearing initial-packet path
/// as normal chunk sends. A north-column emitter reaches the centre's north
/// edge; encoding the centre alone deliberately cannot reproduce that light.
#[test]
fn north_neighbour_light_requires_neighbour_aware_initial_encoding() {
    let shape = ChunkShape::overworld_1_21();
    let centre = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    let mut north = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    north.set_block(8, 0, 15, "minecraft:glowstone");
    let proto = V770ServerProtocol;
    let decode = |directive: ServerDirective| {
        let ServerDirective::Send { payload, .. } = directive else {
            panic!("chunk encoder must send a packet");
        };
        packet_decode_for_dimension(&payload, Dimension::Overworld)
    };
    let isolated = decode(ServerProtocol::encode_chunk(&proto, 0, 0, &centre));
    let neighbour_aware = decode(
        ServerProtocol::try_encode_chunk_with_neighbours_in_dimension(
            &proto,
            0,
            0,
            &centre,
            &[(0, -1, north)],
            ServerDimension::Overworld,
        )
        .expect("neighbour-aware initial encoding"),
    );

    assert_eq!(
        isolated.light.section_light(5).block_at(8, 0, 0),
        0,
        "control: a one-column encoder cannot see the north-column source"
    );
    assert_eq!(
        neighbour_aware.light.section_light(5).block_at(8, 0, 0),
        14,
        "the source crosses one north-border air cell into the centre"
    );
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
        .expect("set LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_MANIFEST to Java's one-chunk parity manifest");
    let java_record = std::fs::read(record_path).expect("read Java canonical record");
    let manifest_bytes = std::fs::read(&manifest_path).expect("read Java manifest");
    let mut manifest_header = [0; HEADER_BYTES]; manifest_header.copy_from_slice(&manifest_bytes[..HEADER_BYTES]);
    let manifest_header = read_header(&manifest_header[..]).expect("validate Java manifest header");
    let decoded = packet_decode_for_dimension(&std::fs::read(packet_path).expect("read Java packet body"), manifest_header.dimension);
    let rust_record = match manifest_header.semantic_version {
        3 => semantic_record(&decoded),
        4 => semantic_record_for_dimension(&decoded, manifest_header.dimension),
        5 => semantic_record_v5_for_dimension(&decoded, manifest_header.dimension),
        version => panic!("unsupported parity semantic version {version}"),
    };
    if rust_record != java_record {
        let first = rust_record.iter().zip(&java_record).position(|(left, right)| left != right).unwrap_or(rust_record.len().min(java_record.len()));
        panic!("canonical semantic bytes differ at offset {first}: Java length {}, Rust length {}, Java byte {:?}, Rust byte {:?}", java_record.len(), rust_record.len(), java_record.get(first), rust_record.get(first));
    }
    let mut manifest = File::open(manifest_path).expect("open Java parity manifest");
    let mut raw_header = [0; HEADER_BYTES]; manifest.read_exact(&mut raw_header).expect("read Java manifest header");
    let header = read_header(&raw_header[..]).expect("validate Java manifest header");
    assert_eq!(header.count, 1, "cross-language control must use exactly one chunk");
    let mut java_digest = [0; 32]; manifest.read_exact(&mut java_digest).expect("read Java semantic digest");
    assert_eq!(support::large_parity_manifest::sha256(&java_record), java_digest, "Java manifest digest must authenticate Java canonical bytes");
    let rust_digest = match header.semantic_version {
        3 => semantic_digest(&decoded),
        4 => semantic_digest_for_dimension(&decoded, header.dimension),
        5 => semantic_digest_v5_for_dimension(&decoded, header.dimension),
        version => panic!("unsupported parity semantic version {version}"),
    };
    assert_eq!(rust_digest, java_digest, "Rust digest must authenticate the same canonical bytes");
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
        assert_eq!((h.cx0,h.cx1,h.cz0,h.cz1,h.count), (
            support::large_parity_manifest::GRID_MIN,
            support::large_parity_manifest::GRID_MAX,
            support::large_parity_manifest::GRID_MIN,
            support::large_parity_manifest::GRID_MAX,
            support::large_parity_manifest::GRID_COUNT,
        ));
    }
    let mut payload_file = File::open(&path).expect("reopen manifest"); payload_file.seek(SeekFrom::Start(HEADER_BYTES as u64)).expect("seek payload");
    verify_payload(BufReader::new(payload_file), h.count, payload_digest_from_header(&raw_header)).expect("payload integrity");
    let mut payload_file = File::open(&path).expect("reopen manifest payload");
    payload_file.seek(SeekFrom::Start(HEADER_BYTES as u64)).expect("seek payload");
    let mut expected = BufReader::new(payload_file);
    let dimension = h.dimension;
    let server_dimension = match dimension {
        Dimension::Overworld => ServerDimension::Overworld,
        Dimension::Nether => ServerDimension::Nether,
        Dimension::End => ServerDimension::End,
    };
    let max_chunks = std::env::var("LODESTONE_LARGE_PARITY_MAX_CHUNKS")
        .ok().and_then(|value| value.parse::<u64>().ok()).unwrap_or(h.count);
    let limit = max_chunks.min(h.count);
    let scan_all = std::env::var_os("LODESTONE_LARGE_PARITY_SCAN_ALL").is_some();
    let reference_packets = if scan_all {
        load_reference_packets(dimension, h.cx0, h.cx1, h.cz0, h.cz1, limit)
    } else {
        BTreeMap::new()
    };
    let mut digest_mismatches = Vec::new();
    let mut component_reports = Vec::new();
    let partial_lifecycle = h.count == 256
        && h.cx1 - h.cx0 == 15
        && h.cz1 - h.cz0 == 15;
    if partial_lifecycle {
        let capture = LifecycleCapture::load(dimension, &h, Path::new(&path));
        match dimension {
            Dimension::Overworld => compare_lifecycle_manifest(
                LifecycleMaterializer::new(overworld_chunk_source(42)),
                &capture,
                &mut expected,
                &h,
                dimension,
                server_dimension,
                limit,
                scan_all,
                &reference_packets,
                &mut digest_mismatches,
                &mut component_reports,
            ),
            Dimension::Nether => compare_lifecycle_manifest(
                LifecycleMaterializer::new(nether_chunk_source(42)),
                &capture,
                &mut expected,
                &h,
                dimension,
                server_dimension,
                limit,
                scan_all,
                &reference_packets,
                &mut digest_mismatches,
                &mut component_reports,
            ),
            Dimension::End => panic!("partial lifecycle capture is only available for Overworld and Nether"),
        }
    } else {
        let source: Box<dyn ChunkSource> = match dimension {
            // A frozen external world is a retained, settled lifecycle result.
            // Keep the comparator on the server's retained-source path rather than
            // regenerating an isolated column for every packet request.
            Dimension::Overworld => Box::new(retained_chunk_source_for_view_radius(overworld_chunk_source(42), 8)),
            Dimension::Nether => Box::new(retained_chunk_source_for_view_radius(nether_chunk_source(42), 8)),
            Dimension::End => Box::new(retained_chunk_source_for_view_radius(end_chunk_source(42), 8)),
        };
        let column_for = |cx, cz| -> ChunkColumn { source.column(cx, cz) };
        let width = (h.cx1 - h.cx0 + 1) as u64;
        let mut expected_digest = [0u8; 32];
        for index in 0..limit {
            expected.read_exact(&mut expected_digest).expect("manifest semantic digest");
            let cx = h.cx0 + (index % width) as i32;
            let cz = h.cz0 + (index / width) as i32;
            let column = column_for(cx, cz);
            let mut neighbours = Vec::with_capacity(8);
            for dz in -1..=1 {
                for dx in -1..=1 {
                    if (dx, dz) != (0, 0) {
                        neighbours.push((dx, dz, column_for(cx + dx, cz + dz)));
                    }
                }
            }
            let directive = V770ServerProtocol
                .try_encode_chunk_with_neighbours_in_dimension(
                    cx,
                    cz,
                    &column,
                    &neighbours,
                    server_dimension,
                )
                .expect("production neighbour-aware chunk encoder");
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
            let decoded = packet_decode_for_dimension(&payload, dimension);
            let full = match h.semantic_version {
                3 => semantic_digest(&decoded),
                4 => semantic_digest_for_dimension(&decoded, dimension),
                5 => semantic_digest_v5_for_dimension(&decoded, dimension),
                version => panic!("unsupported parity semantic version {version}"),
            };
            if full != expected_digest {
                if !scan_all {
                    let packet_summary = std::env::var_os("LODESTONE_LARGE_PARITY_REFERENCE_PACKET")
                        .map(|path| packet_difference_summary(&std::fs::read(path).expect("read authoritative packet capture"), &payload, dimension));
                    panic!(
                        "large semantic parity mismatch at ({cx},{cz}) after {index} matching chunks: reference SHA-256 {}, Lodestone SHA-256 {}{}",
                        hex(&expected_digest), hex(&full), packet_summary.as_deref().unwrap_or(""),
                    );
                }
                digest_mismatches.push((cx, cz, expected_digest, full));
            }
            if let Some(reference_packet) = reference_packets.get(&(cx, cz)) {
                let report = packet_component_difference(reference_packet, &payload, dimension);
                component_reports.push(((cx, cz), report));
            }
            if (index + 1) % 256 == 0 || index + 1 == limit {
                eprintln!("large semantic parity: compared {}/{} chunks (batch boundary at ({cx},{cz}))", index + 1, limit);
            }
        }
    }
    if limit < h.count {
        eprintln!("large semantic parity: bounded pilot completed successfully at {} chunks; full grid remains pending", limit);
    }
    let diagnostic = format_diagnostic_report(limit, h.count, &digest_mismatches, &component_reports);
    let has_component_mismatches = component_reports.iter().any(|(_, report)| report.has_mismatch());
    let has_failing_component_mismatches = component_reports.iter().any(|(_, report)| report.has_failing_mismatch());
    if !digest_mismatches.is_empty() || has_component_mismatches {
        if let Some(path) = std::env::var_os("LODESTONE_LARGE_PARITY_DIAGNOSTIC_OUT") {
            std::fs::write(path, &diagnostic).expect("write parity diagnostic report");
        }
    }
    if !digest_mismatches.is_empty() || has_failing_component_mismatches {
        panic!("{}", format_diagnostic_summary(limit, h.count, &digest_mismatches, &component_reports));
    }
}

fn compare_lifecycle_manifest<S: LifecycleWorldgenSource>(
    mut materializer: LifecycleMaterializer<S>,
    capture: &LifecycleCapture,
    expected: &mut BufReader<File>,
    header: &support::large_parity_manifest::Header,
    dimension: Dimension,
    server_dimension: ServerDimension,
    limit: u64,
    scan_all: bool,
    reference_packets: &BTreeMap<ChunkPos, Vec<u8>>,
    digest_mismatches: &mut Vec<(i32, i32, [u8; 32], [u8; 32])>,
    component_reports: &mut Vec<((i32, i32), PacketComponentReport)>,
) {
    // The external rows repeat the same source completion once per target that
    // observes it. Admit the complete authenticated 18x18 halo first, then
    // run each source's FEATURES body exactly once in replay.tsv admission
    // order. FULL/fence telemetry is diagnostic only: the accepted manifest is
    // a sealed final-world state, not a set of target-fence snapshots.
    for admission in &capture.admissions {
        materializer.admit(admission.chunk);
    }
    eprintln!(
        "large lifecycle replay: admitted {} halo columns, applying {} FEATURES events; ignoring {} FULL telemetry events",
        capture.admissions.len(),
        capture.feature_events.len(),
        capture.full_event_count,
    );
    for event in &capture.feature_events {
        materializer.complete(event.source, event.stage, event.completion_sequence);
    }

    let mut expected_digest = [0u8; 32];
    for index in 0..limit {
        expected.read_exact(&mut expected_digest).expect("manifest semantic digest");
        let target = capture
            .target_order
            .get(index as usize)
            .copied()
            .expect("capture target order must cover the manifest prefix");
        let column = materializer.snapshot_for_packet(target);
        let mut neighbours = Vec::with_capacity(8);
        for dz in -1..=1 {
            for dx in -1..=1 {
                if (dx, dz) != (0, 0) {
                    let neighbour = (target.0 + dx, target.1 + dz);
                    if let Some(column) = materializer.resident_column(neighbour) {
                        neighbours.push((dx, dz, column.clone()));
                    }
                }
            }
        }
        let directive = V770ServerProtocol
            .try_encode_chunk_with_neighbours_in_dimension(
                target.0,
                target.1,
                &column,
                &neighbours,
                server_dimension,
            )
            .expect("production neighbour-aware chunk encoder");
        let payload = match directive {
            ServerDirective::Send { packet_id, payload } => {
                assert_eq!(packet_id, lodestone_v26_2::packet_ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT);
                payload
            }
            other => panic!("production chunk encoder returned {other:?} at {target:?}"),
        };
        if index == 0 {
            if let Some(path) = std::env::var_os("LODESTONE_LARGE_PARITY_PACKET_OUT") {
                std::fs::write(&path, &payload).expect("write requested Lodestone packet capture");
            }
        }
        let decoded = packet_decode_for_dimension(&payload, dimension);
        let full = match header.semantic_version {
            3 => semantic_digest(&decoded),
            4 => semantic_digest_for_dimension(&decoded, dimension),
            5 => semantic_digest_v5_for_dimension(&decoded, dimension),
            version => panic!("unsupported parity semantic version {version}"),
        };
        if full != expected_digest {
            if !scan_all {
                let packet_summary = std::env::var_os("LODESTONE_LARGE_PARITY_REFERENCE_PACKET")
                    .map(|path| packet_difference_summary(&std::fs::read(path).expect("read authoritative packet capture"), &payload, dimension));
                panic!(
                    "large semantic parity mismatch at {:?} after {index} matching chunks: reference SHA-256 {}, Lodestone SHA-256 {}{}",
                    target, hex(&expected_digest), hex(&full), packet_summary.as_deref().unwrap_or(""),
                );
            }
            digest_mismatches.push((target.0, target.1, expected_digest, full));
        }
        if let Some(reference_packet) = reference_packets.get(&target) {
            component_reports.push((target, packet_component_difference(reference_packet, &payload, dimension)));
        }
        if (index + 1) % 256 == 0 || index + 1 == limit {
            eprintln!("large lifecycle parity: compared {}/{} targets (batch boundary at {:?})", index + 1, limit, target);
        }
    }
}

fn packet_decode_for_dimension(bytes: &[u8], dimension: Dimension) -> LevelChunkWithLight {
    let mut reader = Reader::new(bytes);
    let shape = match dimension { Dimension::Overworld => ChunkShape::overworld_1_21(), Dimension::Nether | Dimension::End => ChunkShape::nether_or_end_1_21() };
    let decoded = LevelChunkWithLight::decode(&mut reader, &shape)
        .expect("packet capture must decode with the production client codec");
    reader.ensure_empty().expect("packet capture must have no trailing bytes");
    decoded
}

/// Reads two independently emitted packet bodies through the production client
/// decoder and makes the first oracle mismatch actionable without retaining a
/// full corpus of reference packets.
fn packet_difference_summary(reference: &[u8], actual: &[u8], dimension: Dimension) -> String {
    let reference_len = reference.len();
    let actual_len = actual.len();
    let reference = packet_decode_for_dimension(reference, dimension);
    let actual = packet_decode_for_dimension(actual, dimension);
    if (reference.x, reference.z) != (actual.x, actual.z) {
        return format!(
            "; captured-packet diagnosis unavailable: reference packet is for ({},{}), current mismatch is ({},{})",
            reference.x, reference.z, actual.x, actual.z,
        );
    }
    let mut differing_blocks = 0usize;
    let mut differing_biomes = 0usize;
    let mut non_air_reference = 0usize;
    let mut non_air_actual = 0usize;
    let mut first_block_difference = None;
    let mut block_difference_positions = Vec::new();
    let mut differing_state_pairs = std::collections::HashMap::<(u32, u32), usize>::new();
    let heightmaps_equal = reference.heightmaps == actual.heightmaps;
    let mut heightmap_differences = Vec::new();
    let mut heightmap_ids = reference
        .heightmaps
        .iter()
        .map(|(id, _)| id)
        .chain(actual.heightmaps.iter().map(|(id, _)| id))
        .collect::<Vec<_>>();
    heightmap_ids.sort_unstable();
    heightmap_ids.dedup();
    for id in heightmap_ids {
        let reference_map = reference.heightmaps.get(id);
        let actual_map = actual.heightmaps.get(id);
        let mut differing = 0usize;
        let mut first = None;
        for z in 0..16 {
            for x in 0..16 {
                let reference_height = reference_map.map(|map| map.get(x, z));
                let actual_height = actual_map.map(|map| map.get(x, z));
                if reference_height != actual_height {
                    differing += 1;
                    first.get_or_insert((x, z, reference_height, actual_height));
                }
            }
        }
        if differing != 0 {
            heightmap_differences.push(format!("id {id}: {differing} cells, first {first:?}"));
        }
    }
    let block_entities_equal = reference.block_entities == actual.block_entities;
    let reference_stored_sky = (0..reference.light.light_section_count())
        .filter(|&section| !matches!(reference.light.sky(section), lodestone_world::LightData::Missing))
        .collect::<Vec<_>>();
    let reference_stored_block = (0..reference.light.light_section_count())
        .filter(|&section| !matches!(reference.light.block(section), lodestone_world::LightData::Missing))
        .collect::<Vec<_>>();
    let mut differing_sky_light_cells = 0usize;
    let mut differing_block_light_cells = 0usize;
    let mut differing_sky_light_sections = Vec::new();
    let mut differing_block_light_sections = Vec::new();
    let mut first_sky_light_differences = Vec::new();
    let mut sky_light_difference_extents = Vec::new();
    let mut light_representations = Vec::new();
    for section in 0..reference.column.section_count() {
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
                if block_difference_positions.len() < 96 {
                    let x = cell % 16;
                    let z = (cell / 16) % 16;
                    let y = reference.column.min_y() + section as i32 * 16 + (cell / 256) as i32;
                    block_difference_positions.push(format!(
                        "({x},{y},{z}) {} vs {}",
                        state_label(reference_block),
                        state_label(actual_block),
                    ));
                }
            }
            if first_block_difference.is_none() && reference_block != actual_block {
                let x = cell % 16;
                let z = (cell / 16) % 16;
                let y = reference.column.min_y() + section as i32 * 16 + (cell / 256) as i32;
                first_block_difference = Some((x, y, z, reference_block, actual_block));
            }
        }
        for cell in 0..64 {
            let reference_biome = reference_section.map_or(0, |s| s.biomes().get(cell));
            let actual_biome = actual_section.map_or(0, |s| s.biomes().get(cell));
            differing_biomes += usize::from(reference_biome != actual_biome);
        }
    }
    for section in 0..reference.light.light_section_count() {
        if reference.light.sky(section) != actual.light.sky(section) {
            differing_sky_light_sections.push(section);
            let mut first = None;
            let mut min = (usize::MAX, usize::MAX, usize::MAX);
            let mut max = (0usize, 0usize, 0usize);
            let mut reference_brighter = 0usize;
            let mut actual_brighter = 0usize;
            for cell in 0..4096 {
                let x = cell % 16;
                let z = (cell / 16) % 16;
                let y = cell / 256;
                let reference_value = reference.light.section_light(section).sky_at(x, y, z);
                let actual_value = actual.light.section_light(section).sky_at(x, y, z);
                if reference_value != actual_value {
                    first.get_or_insert((x, y, z, reference_value, actual_value));
                    min.0 = min.0.min(x);
                    min.1 = min.1.min(y);
                    min.2 = min.2.min(z);
                    max.0 = max.0.max(x);
                    max.1 = max.1.max(y);
                    max.2 = max.2.max(z);
                    reference_brighter += usize::from(reference_value > actual_value);
                    actual_brighter += usize::from(actual_value > reference_value);
                }
            }
            first_sky_light_differences.push((section, first));
            sky_light_difference_extents.push((section, min, max, reference_brighter, actual_brighter));
        }
        if reference.light.block(section) != actual.light.block(section) {
            differing_block_light_sections.push(section);
        }
        if reference.light.sky(section) != actual.light.sky(section)
            || reference.light.block(section) != actual.light.block(section)
        {
            light_representations.push(format!(
                "{section}: sky {:?} vs {:?}, block {:?} vs {:?}",
                reference.light.sky(section),
                actual.light.sky(section),
                reference.light.block(section),
                actual.light.block(section),
            ));
        }
        for cell in 0..4096 {
            let x = cell % 16;
            let z = (cell / 16) % 16;
            let y = cell / 256;
            differing_sky_light_cells += usize::from(
                reference.light.section_light(section).sky_at(x, y, z)
                    != actual.light.section_light(section).sky_at(x, y, z),
            );
            differing_block_light_cells += usize::from(
                reference.light.section_light(section).block_at(x, y, z)
                    != actual.light.section_light(section).block_at(x, y, z),
            );
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
    let block_difference_positions = block_difference_positions.join("; ");
    format!(
        "; captured-packet diagnosis: reference={} bytes, Lodestone={} bytes, block cells differ={differing_blocks}, biome cells differ={differing_biomes}, heightmaps equal={heightmaps_equal} ({heightmap_differences:?}), block entities equal={block_entities_equal}, reference stored sky={reference_stored_sky:?}, block={reference_stored_block:?}, sky light cells differ={differing_sky_light_cells} in sections {differing_sky_light_sections:?}, first sky differences={first_sky_light_differences:?}, sky difference extents={sky_light_difference_extents:?}, block light cells differ={differing_block_light_cells} in sections {differing_block_light_sections:?}, representations={light_representations:?}, non-air reference={non_air_reference}, Lodestone={non_air_actual}, first block difference={first_block_difference}, common state pairs={common_state_pairs}, differing blocks={block_difference_positions}",
        reference_len, actual_len,
    )
}

const MAX_COMPONENT_EXAMPLES: usize = 32;
const MAX_REFERENCE_PACKET_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Default)]
struct ComponentDiff {
    total: usize,
    examples: Vec<String>,
}

impl ComponentDiff {
    fn push(&mut self, value: String) {
        self.total += 1;
        if self.examples.len() < MAX_COMPONENT_EXAMPLES {
            self.examples.push(value);
        }
    }
}

#[derive(Default)]
struct PacketComponentReport {
    terrain: ComponentDiff,
    biomes: ComponentDiff,
    heightmaps: ComponentDiff,
    block_entities: ComponentDiff,
    sky_light: ComponentDiff,
    block_light: ComponentDiff,
    masks: ComponentDiff,
}

impl PacketComponentReport {
    fn has_mismatch(&self) -> bool {
        self.terrain.total != 0
            || self.biomes.total != 0
            || self.heightmaps.total != 0
            || self.block_entities.total != 0
            || self.sky_light.total != 0
            || self.block_light.total != 0
            || self.masks.total != 0
    }

    fn has_failing_mismatch(&self) -> bool {
        self.terrain.total != 0
            || self.biomes.total != 0
            || self.heightmaps.total != 0
            || self.block_entities.total != 0
            || self.sky_light.total != 0
            || self.block_light.total != 0
    }
}

fn load_reference_packets(
    dimension: Dimension,
    cx0: i32,
    cx1: i32,
    cz0: i32,
    cz1: i32,
    limit: u64,
) -> BTreeMap<ChunkPos, Vec<u8>> {
    let mut paths = Vec::new();
    if let Some(path) = std::env::var_os("LODESTONE_LARGE_PARITY_REFERENCE_PACKET") {
        paths.push(path.into());
    }
    if let Some(directory) = std::env::var_os("LODESTONE_LARGE_PARITY_REFERENCE_PACKET_DIR") {
        for entry in std::fs::read_dir(directory).expect("read reference packet directory") {
            let path = entry.expect("read reference packet directory entry").path();
            let name = path.file_name().and_then(|name| name.to_str()).unwrap_or_default();
            if name.starts_with("reference")
                && path.extension().is_some_and(|ext| ext == "packet")
                && path.is_file()
            {
                paths.push(path.into_os_string());
            }
        }
    }
    let packet_limit = usize::try_from(limit).unwrap_or(usize::MAX);
    assert!(paths.len() <= packet_limit, "reference packet inputs ({}) exceed the selected manifest prefix ({limit} chunks)", paths.len());
    let mut packets = BTreeMap::new();
    for path in paths {
        let path = Path::new(&path);
        let size = std::fs::metadata(path).unwrap_or_else(|error| panic!("stat {}: {error}", path.display())).len();
        assert!(size <= MAX_REFERENCE_PACKET_BYTES, "reference packet {} is {size} bytes, over the {MAX_REFERENCE_PACKET_BYTES}-byte diagnostic bound", path.display());
        let bytes = std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
        let packet = packet_decode_for_dimension(&bytes, dimension);
        let coordinate = (packet.x, packet.z);
        let width = i64::from(cx1 - cx0 + 1);
        let index = i64::from(coordinate.1 - cz0) * width + i64::from(coordinate.0 - cx0);
        assert!(coordinate.0 >= cx0 && coordinate.0 <= cx1 && coordinate.1 >= cz0 && coordinate.1 <= cz1 && index >= 0 && u64::try_from(index).is_ok_and(|index| index < limit), "reference packet {} decodes to {coordinate:?}, outside the selected manifest prefix", path.display());
        if packets.insert(coordinate, bytes).is_some() {
            panic!("duplicate reference packet for {coordinate:?}");
        }
    }
    packets
}

fn packet_component_difference(reference_bytes: &[u8], actual_bytes: &[u8], dimension: Dimension) -> PacketComponentReport {
    let reference = packet_decode_for_dimension(reference_bytes, dimension);
    let actual = packet_decode_for_dimension(actual_bytes, dimension);
    assert_eq!((reference.x, reference.z), (actual.x, actual.z), "reference/current packet coordinates differ");
    let mut report = PacketComponentReport::default();

    for section in 0..reference.column.section_count() {
        let reference_section = reference.column.section(section);
        let actual_section = actual.column.section(section);
        for cell in 0..4096 {
            let left = reference_section.map_or(0, |value| value.block_states().get(cell));
            let right = actual_section.map_or(0, |value| value.block_states().get(cell));
            if left != right {
                let x = cell % 16;
                let z = (cell / 16) % 16;
                let y = reference.column.min_y() + section as i32 * 16 + (cell / 256) as i32;
                report.terrain.push(format!("({x},{y},{z}) {} -> {}", state_label(left), state_label(right)));
            }
        }
        for cell in 0..64 {
            let left = reference_section.map_or(0, |value| value.biomes().get(cell));
            let right = actual_section.map_or(0, |value| value.biomes().get(cell));
            if left != right {
                let x = cell % 4;
                let z = (cell / 4) % 4;
                let y = cell / 16;
                report.biomes.push(format!("section {section} ({x},{y},{z}) {left} -> {right}"));
            }
        }
    }

    let mut ids = reference.heightmaps.iter().map(|(id, _)| id).chain(actual.heightmaps.iter().map(|(id, _)| id)).collect::<Vec<_>>();
    ids.sort_unstable();
    ids.dedup();
    for id in ids {
        let left = reference.heightmaps.get(id);
        let right = actual.heightmaps.get(id);
        for z in 0..16 {
            for x in 0..16 {
                let a = left.map(|value| value.get(x, z));
                let b = right.map(|value| value.get(x, z));
                if a != b { report.heightmaps.push(format!("id {id} ({x},{z}) {a:?} -> {b:?}")); }
            }
        }
    }

    let mut left = reference.block_entities.clone();
    let mut right = actual.block_entities.clone();
    left.sort_unstable_by_key(canonical_entity_key);
    right.sort_unstable_by_key(canonical_entity_key);
    let max = left.len().max(right.len());
    for index in 0..max {
        let a = left.get(index);
        let b = right.get(index);
        if a.map(canonical_entity_key) != b.map(canonical_entity_key) {
            let position = a.or(b).map(|entity| (entity.rel_x, entity.y, entity.rel_z, entity.type_id));
            report.block_entities.push(format!("index {index} at {position:?}"));
        }
    }

    for section in 0..reference.light.light_section_count() {
        let left = reference.light.section_light(section);
        let right = actual.light.section_light(section);
        for cell in 0..4096 {
            let x = cell % 16;
            let z = (cell / 16) % 16;
            let y = cell / 256;
            let a = left.sky_at(x, y, z);
            let b = right.sky_at(x, y, z);
            if a != b { report.sky_light.push(format!("section {section} ({x},{y},{z}) {a} -> {b}")); }
            let a = left.block_at(x, y, z);
            let b = right.block_at(x, y, z);
            if a != b { report.block_light.push(format!("section {section} ({x},{y},{z}) {a} -> {b}")); }
        }
        let masks = [
            ("sky", reference.light.sky(section), actual.light.sky(section)),
            ("block", reference.light.block(section), actual.light.block(section)),
        ];
        for (layer, a, b) in masks {
            let tag = |value: &lodestone_world::LightData| match value {
                lodestone_world::LightData::Missing => "missing",
                lodestone_world::LightData::Uniform(0) => "empty",
                lodestone_world::LightData::Uniform(_) | lodestone_world::LightData::Values(_) => "present",
            };
            if tag(a) != tag(b) { report.masks.push(format!("{layer} section {section}: {} -> {}", tag(a), tag(b))); }
        }
    }
    report
}

fn canonical_entity_key(entity: &lodestone_world::BlockEntity) -> (u8, i16, u8, u32, Vec<u8>) {
    let mut writer = Writer::default();
    canonical_nbt(&mut writer, &entity.nbt);
    (entity.rel_x, entity.y, entity.rel_z, entity.type_id, writer.into_vec())
}

fn format_diagnostic_report(limit: u64, total: u64, digest: &[(i32, i32, [u8; 32], [u8; 32])], components: &[((i32, i32), PacketComponentReport)]) -> String {
    let mut output = format!("large semantic parity diagnostic: compared {limit}/{total} chunks; digest mismatches={} coordinates={:?}\n", digest.len(), digest.iter().map(|(x, z, _, _)| (*x, *z)).collect::<Vec<_>>());
    if !digest.is_empty() && components.is_empty() {
        output.push_str("component reports unavailable: set LODESTONE_LARGE_PARITY_REFERENCE_PACKET to an authoritative raw packet (or LODESTONE_LARGE_PARITY_REFERENCE_PACKET_DIR to reference*.packet files)\n");
    }
    for ((x, z), report) in components {
        output.push_str(&format!("packet ({x},{z}) component mismatches: terrain={} {:?}; biomes={} {:?}; heightmaps={} {:?}; block_entities={} {:?}; sky_light={} {:?}; block_light={} {:?}; masks={} {:?}\n", report.terrain.total, report.terrain.examples, report.biomes.total, report.biomes.examples, report.heightmaps.total, report.heightmaps.examples, report.block_entities.total, report.block_entities.examples, report.sky_light.total, report.sky_light.examples, report.block_light.total, report.block_light.examples, report.masks.total, report.masks.examples));
    }
    output
}

fn format_diagnostic_summary(limit: u64, total: u64, digest: &[(i32, i32, [u8; 32], [u8; 32])], components: &[((i32, i32), PacketComponentReport)]) -> String {
    let mut output = format!("large semantic parity diagnostic: compared {limit}/{total} chunks; digest mismatches={} coordinates={:?}", digest.len(), digest.iter().map(|(x, z, _, _)| (*x, *z)).collect::<Vec<_>>());
    if !digest.is_empty() && components.is_empty() {
        output.push_str("; component reports unavailable: set LODESTONE_LARGE_PARITY_REFERENCE_PACKET to an authoritative raw packet (or LODESTONE_LARGE_PARITY_REFERENCE_PACKET_DIR to reference*.packet files)");
    }
    for ((x, z), report) in components {
        output.push_str(&format!("\npacket ({x},{z}) counts: terrain={}, biomes={}, heightmaps={}, block_entities={}, sky_light={}, block_light={}, masks={}", report.terrain.total, report.biomes.total, report.heightmaps.total, report.block_entities.total, report.sky_light.total, report.block_light.total, report.masks.total));
    }
    output
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
