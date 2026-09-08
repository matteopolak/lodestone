//! Manual large-grid parity-manifest gate. It intentionally remains ignored:
//! a full 501² run is an external oracle job, not a regular unit test.
mod support { pub mod large_parity_manifest; }

use std::{collections::{BTreeMap, BTreeSet}, fs::File, io::{BufReader, Read, Seek, SeekFrom}, path::{Path, PathBuf}, time::Instant};
use lodestone_core::{Reader, Writer};
use lodestone_server::{
    ChunkColumn, ChunkSource, ServerDirective, ServerProtocol,
    end_chunk_source,
    nether_chunk_source, overworld_chunk_source, retained_chunk_source_for_view_radius,
};
use lodestone_server::dimension::Dimension as ServerDimension;
use lodestone_server::region_source::RegionChunkSource;
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};
use lodestone_worldgen_parity::lifecycle::{
    LifecycleCompletion, LifecycleMaterializer, LifecycleReplayEvent, LifecycleReplayPlan,
    LifecycleWorldgenSource,
};
use support::large_parity_manifest::{
    Dimension, Header as ManifestHeader, IncrementalSha256, HEADER_BYTES, PACKET_AUDIT_RECORD_BYTES, RAW_PACKET_HASH_BYTES,
    canonical_nbt, payload_digest_from_header,
    raw_packet_full_digest, read_header, read_packet_audit_header,
    semantic_digest, semantic_digest_for_dimension, semantic_digest_v5_for_dimension,
    semantic_record, semantic_record_for_dimension, semantic_record_v5_for_dimension,
    validate_packet_audit_header, verify_payload, verify_manifest_payload,
    verify_raw_packet_audit_pair,
};

type ChunkPos = (i32, i32);

const MAX_RAW_DIAGNOSTIC_EXAMPLES: usize = 32;
const MAX_RAW_DIAGNOSTIC_GROUPS: usize = 64;
const PERSISTED_WORLD_ROOT_ENV: &str = "LODESTONE_LARGE_PARITY_FROZEN_WORLD_ROOT";
const PERSISTED_BATCH_SIZE_ENV: &str = "LODESTONE_LARGE_PARITY_PERSISTED_BATCH_SIZE";
const PERSISTED_BATCH_SIZE: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RawPacketMismatch {
    target: ChunkPos,
    index: u64,
    expected_prefix: [u8; RAW_PACKET_HASH_BYTES],
    actual_prefix: [u8; RAW_PACKET_HASH_BYTES],
    expected_full: [u8; PACKET_AUDIT_RECORD_BYTES],
    actual_full: [u8; 32],
    payload_bytes: usize,
}

impl RawPacketMismatch {
    fn collision(self) -> bool {
        self.expected_prefix == self.actual_prefix && self.expected_full != self.actual_full
    }
}

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

/// The lifecycle comparator normally consumes a manifest prefix.  A single
/// target is a separate mode because its digest is not the first payload row
/// and its replay can use the target-specific dependency closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleTargetSelection {
    Prefix { limit: u64 },
    Single { index: usize },
}

fn lifecycle_target_selection(
    count: u64,
    max_chunks: Option<u64>,
    target_index: Option<usize>,
    scan_all: bool,
) -> Result<LifecycleTargetSelection, String> {
    if let Some(index) = target_index {
        if scan_all {
            return Err("LODESTONE_LARGE_PARITY_TARGET_INDEX cannot be combined with LODESTONE_LARGE_PARITY_SCAN_ALL".to_owned());
        }
        if u64::try_from(index).ok().is_none_or(|index| index >= count) {
            return Err(format!("LODESTONE_LARGE_PARITY_TARGET_INDEX={index} is outside the {count}-digest manifest"));
        }
        if max_chunks.is_some_and(|limit| limit != 1) {
            return Err("LODESTONE_LARGE_PARITY_TARGET_INDEX requires LODESTONE_LARGE_PARITY_MAX_CHUNKS to be unset or exactly 1".to_owned());
        }
        return Ok(LifecycleTargetSelection::Single { index });
    }

    Ok(LifecycleTargetSelection::Prefix {
        limit: max_chunks.unwrap_or(count).min(count),
    })
}

fn read_manifest_record_at<R: Read + Seek>(reader: &mut R, index: usize, width: usize) -> std::io::Result<Vec<u8>> {
    let width = u64::try_from(width)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "manifest record width overflows u64"))?;
    let offset = (HEADER_BYTES as u64)
        .checked_add(
            u64::try_from(index)
                .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "manifest record index overflows u64"))?
                .checked_mul(width)
                .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "manifest record offset overflows u64"))?,
        )
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "manifest record offset overflows u64"))?;
    reader.seek(SeekFrom::Start(offset))?;
    let width = usize::try_from(width)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "manifest record width does not fit usize"))?;
    let mut record = vec![0u8; width];
    reader.read_exact(&mut record)?;
    Ok(record)
}

fn read_manifest_digest_at<R: Read + Seek>(reader: &mut R, index: usize) -> std::io::Result<[u8; 32]> {
    read_manifest_record_at(reader, index, 32)?
        .try_into()
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidData, "manifest semantic digest has the wrong width"))
}

fn raw_packet_audit_path(manifest: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os("LODESTONE_LARGE_PARITY_PACKET_AUDIT") {
        return PathBuf::from(path);
    }
    let mut path = manifest.as_os_str().to_os_string();
    path.push(".packet-audit");
    PathBuf::from(path)
}

fn load_raw_packet_audit(
    manifest: &Path,
    main_header: &support::large_parity_manifest::Header,
) -> (PathBuf, support::large_parity_manifest::PacketAuditHeader) {
    let path = raw_packet_audit_path(manifest);
    let mut file = File::open(&path)
        .unwrap_or_else(|error| panic!("open v6 packet-audit sidecar {}: {error}", path.display()));
    let mut raw = [0u8; HEADER_BYTES];
    file.read_exact(&mut raw)
        .unwrap_or_else(|error| panic!("read v6 packet-audit sidecar {} header: {error}", path.display()));
    let header = read_packet_audit_header(&raw[..]).unwrap_or_else(|error| {
        panic!("invalid v6 packet-audit sidecar {}: {error}", path.display())
    });
    validate_packet_audit_header(main_header, &header).unwrap_or_else(|error| {
        panic!("v6 packet-audit sidecar {} does not match manifest: {error}", path.display())
    });
    (path, header)
}

fn verify_raw_packet_audit_files(
    manifest: &Path,
    raw_header: &[u8; HEADER_BYTES],
    main_header: &support::large_parity_manifest::Header,
    audit_path: &Path,
    audit_header: &support::large_parity_manifest::PacketAuditHeader,
) {
    let mut main = File::open(manifest).unwrap_or_else(|error| panic!("reopen v6 manifest {}: {error}", manifest.display()));
    main.seek(SeekFrom::Start(HEADER_BYTES as u64)).unwrap_or_else(|error| panic!("seek v6 manifest payload: {error}"));
    let mut audit = File::open(audit_path).unwrap_or_else(|error| panic!("reopen packet-audit sidecar {}: {error}", audit_path.display()));
    audit.seek(SeekFrom::Start(HEADER_BYTES as u64)).unwrap_or_else(|error| panic!("seek packet-audit payload: {error}"));
    verify_raw_packet_audit_pair(
        BufReader::new(main),
        BufReader::new(audit),
        main_header.count,
        payload_digest_from_header(raw_header),
        audit_header.payload_digest,
    )
    .unwrap_or_else(|error| panic!("v6 manifest/packet-audit payload authentication failed: {error}"));
}

fn persisted_world_freeze_stamp(header: &ManifestHeader) -> String {
    let contract = if header.semantic_version == 6 { "v6" } else { "v2" };
    let dimension = match header.dimension {
        Dimension::Overworld => "overworld",
        Dimension::Nether => "nether",
        Dimension::End => "end",
    };
    format!("lodestone-large-parity-materialization-{contract}-{dimension}.freeze.sha256")
}

fn collect_persisted_world_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<(String, PathBuf)>,
) -> std::io::Result<()> {
    let mut entries = std::fs::read_dir(current)
        .map_err(|error| std::io::Error::new(error.kind(), format!("read {}: {error}", current.display())))?
        .collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        let metadata = std::fs::metadata(&path)?;
        if metadata.is_dir() {
            collect_persisted_world_files(root, &path, files)?;
        } else if metadata.is_file() {
            let relative = path
                .strip_prefix(root)
                .expect("walked path must be below frozen root")
                .to_string_lossy()
                .replace(std::path::MAIN_SEPARATOR, "/");
            files.push((relative, path));
        }
    }
    Ok(())
}

/// Reproduces the oracle's authenticated tree digest without loading all
/// region bytes at once. The selected freeze stamp is excluded exactly as the
/// external exporter excludes it before sealing the world.
fn persisted_world_tree_digest(root: &Path, freeze_stamp: &str) -> std::io::Result<[u8; 32]> {
    let mut files = Vec::new();
    collect_persisted_world_files(root, root, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut digest = IncrementalSha256::new();
    let mut buffer = [0u8; 64 * 1024];
    for (relative, path) in files {
        if relative == freeze_stamp {
            continue;
        }
        let name = relative.as_bytes();
        let name_len = i32::try_from(name.len()).map_err(|_| {
            std::io::Error::new(std::io::ErrorKind::InvalidData, "frozen-world path is too long")
        })?;
        digest.update(&name_len.to_be_bytes());
        digest.update(name);
        let size = std::fs::metadata(&path)?.len();
        digest.update(&size.to_be_bytes());
        let mut file = File::open(&path)?;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            digest.update(&buffer[..read]);
        }
    }
    Ok(digest.finish())
}

fn parse_persisted_world_digest(value: &str, path: &Path) -> [u8; 32] {
    let value = value.trim();
    assert_eq!(value.len(), 64, "frozen-world seal {} must contain 64 hex characters", path.display());
    let mut digest = [0u8; 32];
    for (index, byte) in digest.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .unwrap_or_else(|error| panic!("frozen-world seal {} contains invalid hex: {error}", path.display()));
    }
    digest
}

fn validated_persisted_world_root(header: &ManifestHeader) -> PathBuf {
    let root = std::env::var_os(PERSISTED_WORLD_ROOT_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| panic!("persisted End parity requires {PERSISTED_WORLD_ROOT_ENV}=/absolute/path/to/validated-frozen-world"));
    assert!(root.is_dir(), "persisted End parity root {} is not a directory", root.display());
    let freeze_stamp = persisted_world_freeze_stamp(header);
    let stamp = root.join(&freeze_stamp);
    let sealed = parse_persisted_world_digest(
        &std::fs::read_to_string(&stamp)
            .unwrap_or_else(|error| panic!("read frozen-world seal {}: {error}", stamp.display())),
        &stamp,
    );
    let actual = persisted_world_tree_digest(&root, &freeze_stamp)
        .unwrap_or_else(|error| panic!("digest frozen-world root {}: {error}", root.display()));
    assert_eq!(actual, sealed, "frozen-world tree differs from its seal {}", stamp.display());
    assert_eq!(actual, header.frozen_world, "frozen-world seal {} differs from manifest identity", stamp.display());
    root
}

fn persisted_batch_size() -> usize {
    let value = std::env::var(PERSISTED_BATCH_SIZE_ENV).unwrap_or_else(|_| PERSISTED_BATCH_SIZE.to_string());
    let parsed = value.parse::<usize>().unwrap_or_else(|error| {
        panic!("{PERSISTED_BATCH_SIZE_ENV} must be a positive integer: {error}")
    });
    assert!(parsed > 0, "{PERSISTED_BATCH_SIZE_ENV} must be a positive integer");
    parsed
}

fn persisted_export_coordinate(header: &ManifestHeader, index: usize) -> ChunkPos {
    let width = usize::try_from(i64::from(header.cx1) - i64::from(header.cx0) + 1)
        .expect("persisted export width fits usize");
    (
        header.cx0 + i32::try_from(index % width).expect("persisted export x offset fits i32"),
        header.cz0 + i32::try_from(index / width).expect("persisted export z offset fits i32"),
    )
}

fn persisted_batch_ranges(limit: usize, batch_size: usize) -> Vec<(usize, usize)> {
    assert!(batch_size > 0, "persisted batch size must be positive");
    (0..limit)
        .step_by(batch_size)
        .map(|start| (start, (start + batch_size).min(limit)))
        .collect()
}

fn optional_usize_env(name: &str) -> Result<Option<usize>, String> {
    std::env::var_os(name)
        .map(|value| {
            value
                .into_string()
                .map_err(|_| format!("{name} must be valid UTF-8"))?
                .parse::<usize>()
                .map_err(|error| format!("{name} must be a non-negative integer: {error}"))
        })
        .transpose()
}

fn optional_u64_env(name: &str) -> Result<Option<u64>, String> {
    std::env::var_os(name)
        .map(|value| {
            value
                .into_string()
                .map_err(|_| format!("{name} must be valid UTF-8"))?
                .parse::<u64>()
                .map_err(|error| format!("{name} must be a non-negative integer: {error}"))
        })
        .transpose()
}

/// Selects a bounded comparison run.  A batch is explicitly scan-all: a
/// multi-thousand-chunk run that stops at its first defect cannot provide the
/// independent issue inventory that makes the expensive export useful.
fn parity_batch_limit(
    count: u64,
    max_chunks: Option<u64>,
    batch_size: Option<u64>,
    target_index: Option<usize>,
    scan_all: bool,
) -> Result<u64, String> {
    if let Some(size) = batch_size {
        if target_index.is_some() {
            return Err("LODESTONE_LARGE_PARITY_BATCH_SIZE cannot be combined with LODESTONE_LARGE_PARITY_TARGET_INDEX".to_owned());
        }
        if !scan_all {
            return Err("LODESTONE_LARGE_PARITY_BATCH_SIZE requires LODESTONE_LARGE_PARITY_SCAN_ALL=1".to_owned());
        }
        if size < 2_000 {
            return Err(format!("LODESTONE_LARGE_PARITY_BATCH_SIZE must be at least 2000, got {size}"));
        }
        if count < size {
            return Err(format!("LODESTONE_LARGE_PARITY_BATCH_SIZE={size} exceeds the authenticated manifest count {count}"));
        }
        if max_chunks.is_some_and(|limit| limit != size) {
            return Err("LODESTONE_LARGE_PARITY_BATCH_SIZE and LODESTONE_LARGE_PARITY_MAX_CHUNKS must agree when both are set".to_owned());
        }
        return Ok(size);
    }

    let selection = lifecycle_target_selection(count, max_chunks, target_index, scan_all)?;
    Ok(match selection {
        LifecycleTargetSelection::Prefix { limit } => limit,
        LifecycleTargetSelection::Single { .. } => 1,
    })
}

fn is_partial_lifecycle_manifest(header: &support::large_parity_manifest::Header) -> bool {
    header.semantic_version != 6
        && header.dimension != Dimension::End
        && header.count == 256
        && i64::from(header.cx1) - i64::from(header.cx0) == 15
        && i64::from(header.cz1) - i64::from(header.cz0) == 15
}

fn raw_packet_targets(
    header: &support::large_parity_manifest::Header,
    limit: u64,
) -> Vec<ChunkPos> {
    let width = u64::try_from(i64::from(header.cx1) - i64::from(header.cx0) + 1)
        .expect("authenticated manifest coordinate width fits u64");
    (0..limit)
        .map(|index| {
            (
                header.cx0 + i32::try_from(index % width).expect("raw target x offset fits i32"),
                header.cz0 + i32::try_from(index / width).expect("raw target z offset fits i32"),
            )
        })
        .collect()
}

/// The first packet is the bootstrap admission and has no previously adopted
/// neighbour light. Later admissions adopt the cardinal source columns before
/// their initial packet is encoded; the full 3x3 footprint is still supplied
/// to the encoder for packet construction.
const INITIAL_CARDINAL_NEIGHBOUR_OFFSETS: [(i32, i32); 4] =
    [(-1, 0), (0, -1), (1, 0), (0, 1)];
const END_LIGHT_NEIGHBOUR_OFFSETS: [(i32, i32); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

fn initial_light_admission_neighbour_offsets(record_index: u64) -> &'static [(i32, i32)] {
    if record_index == 0 {
        &[]
    } else {
        &INITIAL_CARDINAL_NEIGHBOUR_OFFSETS
    }
}

fn initial_light_snapshot_for_admission(
    proto: &V770ServerProtocol,
    centre: &ChunkColumn,
    admitted_neighbours: &[(i32, i32, ChunkColumn)],
    dimension: ServerDimension,
) -> ChunkColumn {
    let mut settled = centre.clone();
    if proto.retains_initial_column_light()
        && let Some(light) = proto.compute_initial_column_light_with_neighbours_in_dimension(
            centre,
            admitted_neighbours,
            dimension,
        )
    {
        settled.set_retained_light(light);
    }
    settled
}

/// Replays the serial materializer order used to seal the raw-packet world.
///
/// The frozen-world export order is x-fastest/z, but the light-engine state was
/// produced tile-z/tile-x/z/x. Keeping those orders separate is essential: an
/// export prefix is not an admission prefix.
fn end_materialization_positions(h: &ManifestHeader) -> Vec<ChunkPos> {
    const TILE_SIDE: i32 = 16;
    let min_x = h.cx0 - 1;
    let max_x = h.cx1 + 1;
    let min_z = h.cz0 - 1;
    let max_z = h.cz1 + 1;
    let tiles_x = (max_x - min_x) / TILE_SIDE + 1;
    let tiles_z = (max_z - min_z) / TILE_SIDE + 1;
    let mut positions = Vec::new();
    for tile_z in 0..tiles_z {
        let z0 = min_z + tile_z * TILE_SIDE;
        for tile_x in 0..tiles_x {
            let x0 = min_x + tile_x * TILE_SIDE;
            for cz in z0..=max_z.min(z0 + TILE_SIDE - 1) {
                for cx in x0..=max_x.min(x0 + TILE_SIDE - 1) {
                    positions.push((cx, cz));
                }
            }
        }
    }
    positions
}

/// Returns the exclusive admission prefix needed to export the requested
/// x-fastest/z prefix.  An admission settles the centre and all columns in
/// `dependency_offsets`, so the final retained state of a target is fixed by
/// the last admission whose footprint contains that target.  Admissions after
/// that point cannot write the target and are causally irrelevant to its
/// packet.  The mapping is deliberately independent of the materializer so it
/// can be checked against small synthetic grids.
fn materialization_prefix_for_export_prefix(
    admissions: &[ChunkPos],
    export_bounds: (i32, i32, i32, i32),
    dependency_offsets: &[(i32, i32)],
    export_limit: usize,
) -> usize {
    let (min_x, max_x, min_z, max_z) = export_bounds;
    let width = usize::try_from(max_x - min_x + 1).expect("export width fits usize");
    let height = usize::try_from(max_z - min_z + 1).expect("export height fits usize");
    let export_count = width.checked_mul(height).expect("export count fits usize");
    assert!(export_limit <= export_count, "export prefix exceeds authenticated bounds");

    let admission_indices = admissions
        .iter()
        .enumerate()
        .map(|(index, &chunk)| (chunk, index))
        .collect::<BTreeMap<_, _>>();
    let mut bound = 0;
    for export_index in 0..export_limit {
        let cx = min_x + i32::try_from(export_index % width).expect("export x offset fits i32");
        let cz = min_z + i32::try_from(export_index / width).expect("export z offset fits i32");
        let mut last = *admission_indices
            .get(&(cx, cz))
            .expect("export target must be in the materialization admissions")
            + 1;
        for &(dx, dz) in dependency_offsets {
            let writer = (cx - dx, cz - dz);
            let admission = *admission_indices
                .get(&writer)
                .expect("export dependency writer must be in the materialization admissions")
                + 1;
            last = last.max(admission);
        }
        bound = bound.max(last);
    }
    bound
}

/// Compares against the externally sealed End world without replaying
/// admissions. This is intentionally a diagnostic import/encoder arm: its
/// mismatches never enter the generated-world acceptance vectors below.
fn compare_end_raw_from_persisted_world(
    source: &dyn ChunkSource,
    manifest: &Path,
    h: &ManifestHeader,
    limit: u64,
    scan_all: bool,
    reference_packets: &BTreeMap<ChunkPos, PathBuf>,
    batch_size: usize,
) -> Vec<RawPacketMismatch> {
    let mut expected = BufReader::new(
        File::open(manifest).unwrap_or_else(|error| panic!("open persisted-world manifest {}: {error}", manifest.display())),
    );
    expected
        .seek(SeekFrom::Start(HEADER_BYTES as u64))
        .unwrap_or_else(|error| panic!("seek persisted-world manifest payload: {error}"));
    let audit_path = raw_packet_audit_path(manifest);
    let mut expected_audit = BufReader::new(
        File::open(&audit_path)
            .unwrap_or_else(|error| panic!("open persisted-world packet audit {}: {error}", audit_path.display())),
    );
    expected_audit
        .seek(SeekFrom::Start(HEADER_BYTES as u64))
        .unwrap_or_else(|error| panic!("seek persisted-world packet audit payload: {error}"));

    let mut mismatches = Vec::new();
    let mut reference_mismatches = Vec::new();
    let mut retained_target_lights = 0usize;
    let total = usize::try_from(limit).expect("persisted export limit fits usize");
    for (batch_start, batch_end) in persisted_batch_ranges(total, batch_size) {
        let mut records = Vec::with_capacity(batch_end - batch_start);
        for index in batch_start..batch_end {
            let mut prefix = [0u8; RAW_PACKET_HASH_BYTES];
            expected
                .read_exact(&mut prefix)
                .unwrap_or_else(|error| panic!("read persisted-world manifest record {index}: {error}"));
            let mut full = [0u8; PACKET_AUDIT_RECORD_BYTES];
            expected_audit
                .read_exact(&mut full)
                .unwrap_or_else(|error| panic!("read persisted-world packet audit record {index}: {error}"));
            let index = u64::try_from(index).expect("persisted export index fits u64");
            let (cx, cz) = persisted_export_coordinate(h, usize::try_from(index).expect("persisted export index fits usize"));
            records.push(((cx, cz), prefix, full, index));
        }

        // Keep the batch boundary explicit even though RegionChunkSource owns
        // immutable persisted state rather than a ticket graph. The source is
        // opened once, matching the oracle's one-server sequential batches;
        // load the complete batch before any packet capture, then retain those
        // owned inputs until every capture in the batch finishes.
        let mut resident = Vec::with_capacity(records.len());
        for ((cx, cz), expected_prefix, expected_full, index) in records {
            let settled = source.column(cx, cz);
            // A fresh reopen legitimately has no persisted light layers for a
            // dark first target. The production encoder's fallback then emits
            // empty masks; later targets use the persisted light graph. Do not
            // recompute or settle a target here, and do not infer masks from
            // block occupancy.
            if settled.retained_light().is_some() {
                retained_target_lights += 1;
            }
            let mut neighbours = Vec::with_capacity(8);
            for dz in -1..=1 {
                for dx in -1..=1 {
                    if (dx, dz) != (0, 0) {
                        neighbours.push((dx, dz, source.column(cx + dx, cz + dz)));
                    }
                }
            }
            resident.push(((cx, cz), expected_prefix, expected_full, index, settled, neighbours));
        }
        for ((cx, cz), expected_prefix, expected_full, index, settled, neighbours) in resident {
            let directive = V770ServerProtocol
                .try_encode_chunk_with_neighbours_in_dimension(
                    cx,
                    cz,
                    &settled,
                    &neighbours,
                    ServerDimension::End,
                )
                .expect("production neighbour-aware chunk encoder for persisted End");
            let payload = match directive {
                ServerDirective::Send { packet_id, payload } => {
                    assert_eq!(packet_id, lodestone_v26_2::packet_ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT);
                    payload
                }
                other => panic!("production chunk encoder returned {other:?} at persisted ({cx},{cz})"),
            };
            let actual_full = raw_packet_full_digest(&payload);
            let actual_prefix = [actual_full[0], actual_full[1]];
            if actual_prefix != expected_prefix || actual_full != expected_full {
                let mismatch = RawPacketMismatch {
                    target: (cx, cz),
                    index,
                    expected_prefix,
                    actual_prefix,
                    expected_full,
                    actual_full,
                    payload_bytes: payload.len(),
                };
                if !scan_all {
                    panic!(
                        "persisted-world import/encoder parity mismatch at ({cx},{cz}) after {index} matching chunks: expected prefix {}, actual prefix {}, expected full SHA-256 {}, actual full SHA-256 {}, collision={}, payload bytes={}",
                        hex(&mismatch.expected_prefix),
                        hex(&mismatch.actual_prefix),
                        hex(&mismatch.expected_full),
                        hex(&mismatch.actual_full),
                        mismatch.collision(),
                        mismatch.payload_bytes,
                    );
                }
                mismatches.push(mismatch);
            }
            if let Some(reference_path) = reference_packets.get(&(cx, cz)) {
                let reference = std::fs::read(reference_path)
                    .unwrap_or_else(|error| panic!("read persisted-world reference {}: {error}", reference_path.display()));
                let reference_full = raw_packet_full_digest(&reference);
                if reference_full != actual_full {
                    reference_mismatches.push(((cx, cz), reference_path.clone(), reference_full, actual_full));
                }
            }
        }
        eprintln!(
            "large persisted-world import/encoder parity: compared {}..{} of {} targets (batch size {batch_size}, retained target lights {retained_target_lights})",
            batch_start,
            batch_end,
            total,
        );
    }
    if !reference_mismatches.is_empty() {
        eprintln!(
            "large persisted-world import/encoder reference mismatches: {:?}",
            reference_mismatches
                .iter()
                .map(|(target, path, expected, actual)| (*target, path, hex(expected), hex(actual)))
                .collect::<Vec<_>>(),
        );
    }
    mismatches
}

fn encode_end_packet_with_source(
    source: &dyn ChunkSource,
    cx: i32,
    cz: i32,
    column: &ChunkColumn,
    phase: &str,
) -> Vec<u8> {
    let directive = lodestone_server::encode_chunk_with_source(
        &V770ServerProtocol,
        source,
        cx,
        cz,
        column,
    )
    .unwrap_or_else(|error| panic!("{phase} End production source-aware encoder at ({cx},{cz}): {error}"));
    match directive {
        ServerDirective::Send { packet_id, payload } => {
            assert_eq!(
                packet_id,
                lodestone_v26_2::packet_ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT,
            );
            payload
        }
        other => panic!("{phase} End production source-aware encoder returned {other:?} at ({cx},{cz})"),
    }
}

/// Materializes generated End columns through the live source-aware encoder,
/// flushes the source's real Anvil save handle, then compares a fresh reopen.
/// This deliberately leaves retention to `RegionChunkSource`: the parity arm
/// must exercise the same persistence owner that production uses.
fn compare_end_raw_after_generated_save_reopen<R: Read, A: Read>(
    world_dir: &Path,
    expected: &mut R,
    expected_audit: &mut A,
    h: &ManifestHeader,
    server_dimension: ServerDimension,
    limit: u64,
    scan_all: bool,
    reference_packets: &BTreeMap<ChunkPos, PathBuf>,
    component_reports: &mut Vec<(ChunkPos, PacketComponentReport)>,
) -> Vec<RawPacketMismatch> {
    let width = u64::try_from(i64::from(h.cx1) - i64::from(h.cx0) + 1)
        .expect("authenticated manifest coordinate width fits u64");
    let mut records = Vec::with_capacity(usize::try_from(limit).expect("parity prefix fits memory"));
    for index in 0..limit {
        let mut prefix = [0u8; RAW_PACKET_HASH_BYTES];
        expected.read_exact(&mut prefix).expect("manifest raw packet hash prefix");
        let mut full = [0u8; PACKET_AUDIT_RECORD_BYTES];
        expected_audit.read_exact(&mut full).expect("packet-audit full packet digest");
        let cx = h.cx0 + (index % width) as i32;
        let cz = h.cz0 + (index / width) as i32;
        records.push(((cx, cz), prefix, full, index));
    }

    let source = RegionChunkSource::new(
        end_chunk_source(42),
        world_dir,
        server_dimension,
        0,
        256,
    )
    .unwrap_or_else(|error| panic!("open generated End persistence source {}: {error}", world_dir.display()));
    let admissions = end_materialization_positions(h);
    let admission_limit = materialization_prefix_for_export_prefix(
        &admissions,
        (h.cx0, h.cx1, h.cz0, h.cz1),
        &END_LIGHT_NEIGHBOUR_OFFSETS,
        usize::try_from(limit).expect("export prefix fits usize"),
    );
    eprintln!(
        "large generated save/reopen parity: replaying {admission_limit}/{} End materialization admissions before {} export records",
        admissions.len(),
        limit,
    );
    for (admission_index, &(cx, cz)) in admissions.iter().take(admission_limit).enumerate() {
        let column = source.column(cx, cz);
        let _ = encode_end_packet_with_source(
            &source,
            cx,
            cz,
            &column,
            "generated materialization",
        );
        if (admission_index + 1) % 256 == 0 || admission_index + 1 == admission_limit {
            eprintln!(
                "large generated save/reopen parity: materialized {}/{} End admissions",
                admission_index + 1,
                admissions.len(),
            );
        }
    }

    let save_handle: lodestone_server::region_source::WorldSaveHandle = source.save_handle();
    let written = save_handle
        .save()
        .unwrap_or_else(|error| panic!("save generated End materialization through WorldSaveHandle: {error}"));
    assert!(written > 0, "generated End materialization must write its settled centres");
    drop(save_handle);
    drop(source);

    let reopened = RegionChunkSource::new(
        end_chunk_source(42),
        world_dir,
        server_dimension,
        0,
        256,
    )
    .unwrap_or_else(|error| panic!("reopen generated End persistence source {}: {error}", world_dir.display()));

    let mut mismatches = Vec::new();
    for (batch_start, batch_end) in persisted_batch_ranges(
        records.len(),
        persisted_batch_size(),
    ) {
        let batch = &records[batch_start..batch_end];
        let mut halo = BTreeSet::new();
        for &((cx, cz), _, _, _) in batch {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    halo.insert((cx + dx, cz + dz));
                }
            }
        }
        for &(cx, cz) in &halo {
            let _ = reopened.column(cx, cz);
        }
        for &((cx, cz), expected_prefix, expected_full, index) in batch {
            let column = reopened.column(cx, cz);
            let payload = encode_end_packet_with_source(
                &reopened,
                cx,
                cz,
                &column,
                "generated save/reopen capture",
            );
            let actual_full = raw_packet_full_digest(&payload);
            let actual_prefix = [actual_full[0], actual_full[1]];
            if actual_prefix != expected_prefix || actual_full != expected_full {
                let mismatch = RawPacketMismatch {
                    target: (cx, cz),
                    index,
                    expected_prefix,
                    actual_prefix,
                    expected_full,
                    actual_full,
                    payload_bytes: payload.len(),
                };
                if !scan_all {
                    panic!(
                        "large generated save/reopen End raw-packet parity mismatch at ({cx},{cz}) after {index} export records: expected prefix {}, actual prefix {}, expected full SHA-256 {}, actual full SHA-256 {}, collision={}, payload bytes={}",
                        hex(&mismatch.expected_prefix),
                        hex(&mismatch.actual_prefix),
                        hex(&mismatch.expected_full),
                        hex(&mismatch.actual_full),
                        mismatch.collision(),
                        mismatch.payload_bytes,
                    );
                }
                mismatches.push(mismatch);
            }
            if let Some(reference_path) = reference_packets.get(&(cx, cz)) {
                let reference_packet = std::fs::read(reference_path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", reference_path.display()));
                component_reports.push((
                    (cx, cz),
                    packet_component_difference(&reference_packet, &payload, Dimension::End),
                ));
            }
        }
        eprintln!(
            "large generated save/reopen parity: compared {}..{} of {} targets (batch size {})",
            batch_start,
            batch_end,
            records.len(),
            persisted_batch_size(),
        );
    }
    let generated_delta = reopened
        .stats()
        .generated
        .load(std::sync::atomic::Ordering::Relaxed);
    let loaded_from_disk = reopened
        .stats()
        .loaded_from_disk
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(generated_delta, 0, "generated save/reopen parity fell back to generated columns");
    assert!(loaded_from_disk > 0, "generated save/reopen parity loaded no columns from disk");
    eprintln!(
        "large generated save/reopen parity: loaded_from_disk_delta={loaded_from_disk} generated_delta={generated_delta} packet_mismatches={}",
        mismatches.len(),
    );
    mismatches
}

#[test]
fn persisted_end_export_mapping_is_x_fastest_and_batch_generic() {
    let header = ManifestHeader {
        semantic_version: 6,
        cx0: -25,
        cx1: 25,
        cz0: -25,
        cz1: 25,
        count: 51 * 51,
        frozen_world: [1; 32],
        dimension: Dimension::End,
        record_width: RAW_PACKET_HASH_BYTES as u16,
        kind: 2,
    };
    assert_eq!(persisted_export_coordinate(&header, 0), (-25, -25));
    assert_eq!(persisted_export_coordinate(&header, 890), (-2, -8));
    assert_eq!(persisted_export_coordinate(&header, 1050), (5, -5));
    assert_eq!(
        persisted_batch_ranges(1051, PERSISTED_BATCH_SIZE),
        vec![(0, 256), (256, 512), (512, 768), (768, 1024), (1024, 1051)],
    );
}

#[test]
fn generated_end_save_reopen_loads_centre_and_eight_dependencies() {
    let world_dir = std::env::temp_dir().join(format!(
        "lodestone-end-save-reopen-control-{}",
        std::process::id(),
    ));
    let _ = std::fs::remove_dir_all(&world_dir);
    let source = RegionChunkSource::new(
        end_chunk_source(42),
        &world_dir,
        ServerDimension::End,
        0,
        256,
    )
    .expect("open generated End persistence control source");
    for cz in -1..=1 {
        for cx in -1..=1 {
            let column = source.column(cx, cz);
            let _ = encode_end_packet_with_source(
                &source,
                cx,
                cz,
                &column,
                "generated persistence control",
            );
        }
    }
    let save_handle: lodestone_server::region_source::WorldSaveHandle = source.save_handle();
    assert!(
        save_handle
            .save()
            .expect("save generated End persistence control")
            >= 9,
        "centre and eight dependencies must be written",
    );
    drop(save_handle);
    drop(source);

    let reopened = RegionChunkSource::new(
        end_chunk_source(42),
        &world_dir,
        ServerDimension::End,
        0,
        256,
    )
    .expect("reopen generated End persistence control source");
    for cz in -1..=1 {
        for cx in -1..=1 {
            let _ = reopened.column(cx, cz);
        }
    }
    let generated_delta = reopened
        .stats()
        .generated
        .load(std::sync::atomic::Ordering::Relaxed);
    let loaded_from_disk = reopened
        .stats()
        .loaded_from_disk
        .load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(generated_delta, 0, "reopened centre and dependencies must not regenerate");
    assert!(loaded_from_disk > 0, "reopened centre and dependencies must load from disk");
    eprintln!(
        "focused generated End save/reopen persistence: loaded_from_disk_delta={loaded_from_disk} generated_delta={generated_delta}",
    );
    drop(reopened);
    let _ = std::fs::remove_dir_all(world_dir);
}

#[test]
fn persisted_end_freeze_stamp_uses_raw_materialization_contract() {
    let header = ManifestHeader {
        semantic_version: 6,
        cx0: -500,
        cx1: 500,
        cz0: -500,
        cz1: 500,
        count: 1001 * 1001,
        frozen_world: [1; 32],
        dimension: Dimension::End,
        record_width: RAW_PACKET_HASH_BYTES as u16,
        kind: 2,
    };
    assert_eq!(
        persisted_world_freeze_stamp(&header),
        "lodestone-large-parity-materialization-v6-end.freeze.sha256",
    );
}

#[test]
fn lifecycle_target_selection_rejects_ambiguous_single_target_requests() {
    assert_eq!(
        lifecycle_target_selection(256, None, Some(7), false),
        Ok(LifecycleTargetSelection::Single { index: 7 }),
    );
    assert_eq!(
        lifecycle_target_selection(256, Some(1), Some(7), false),
        Ok(LifecycleTargetSelection::Single { index: 7 }),
    );
    assert!(lifecycle_target_selection(256, None, Some(256), false).is_err());
    assert!(lifecycle_target_selection(256, None, Some(7), true).is_err());
    assert!(lifecycle_target_selection(256, Some(2), Some(7), false).is_err());
    assert_eq!(
        lifecycle_target_selection(256, Some(2), None, false),
        Ok(LifecycleTargetSelection::Prefix { limit: 2 }),
    );
}

#[test]
fn initial_light_admission_has_bootstrap_then_cardinal_sources() {
    assert!(initial_light_admission_neighbour_offsets(0).is_empty());
    assert_eq!(
        initial_light_admission_neighbour_offsets(1),
        INITIAL_CARDINAL_NEIGHBOUR_OFFSETS.as_slice(),
    );
    assert_eq!(
        initial_light_admission_neighbour_offsets(31),
        INITIAL_CARDINAL_NEIGHBOUR_OFFSETS.as_slice(),
    );
}

#[test]
fn manifest_record_reader_seeks_using_the_authenticated_record_width() {
    let mut bytes = vec![0u8; HEADER_BYTES];
    bytes.extend([0x10, 0x11, 0x20, 0x21, 0x30, 0x31]);
    let mut reader = std::io::Cursor::new(bytes);
    assert_eq!(read_manifest_record_at(&mut reader, 1, RAW_PACKET_HASH_BYTES).unwrap(), vec![0x20, 0x21]);
    assert_eq!(reader.position(), HEADER_BYTES as u64 + 2 * RAW_PACKET_HASH_BYTES as u64);
}

#[test]
fn raw_v6_16x16_shards_never_enter_lifecycle_replay() {
    let header = support::large_parity_manifest::Header {
        semantic_version: 6,
        cx0: 0,
        cx1: 15,
        cz0: 0,
        cz1: 15,
        count: 256,
        frozen_world: [1; 32],
        dimension: Dimension::Overworld,
        record_width: RAW_PACKET_HASH_BYTES as u16,
        kind: 2,
    };
    assert!(!is_partial_lifecycle_manifest(&header));
}

#[test]
fn end_replay_bound_preserves_the_required_light_prefix_for_export_records() {
    let header = support::large_parity_manifest::Header {
        semantic_version: 6,
        cx0: -25,
        cx1: 25,
        cz0: -25,
        cz1: 25,
        count: 51 * 51,
        frozen_world: [0; 32],
        dimension: Dimension::End,
        record_width: RAW_PACKET_HASH_BYTES as u16,
        kind: 2,
    };
    let admissions = end_materialization_positions(&header);
    assert_eq!(admissions.len(), 2_809);
    assert_eq!(
        materialization_prefix_for_export_prefix(
            &admissions,
            (header.cx0, header.cx1, header.cz0, header.cz1),
            &END_LIGHT_NEIGHBOUR_OFFSETS,
            891,
        ),
        1_631,
        "a prefix ending at export index 890 must retain every earlier target's light",
    );
    assert_eq!(
        materialization_prefix_for_export_prefix(
            &admissions,
            (header.cx0, header.cx1, header.cz0, header.cz1),
            &END_LIGHT_NEIGHBOUR_OFFSETS,
            1_051,
        ),
        1_646,
        "a prefix ending at export index 1050 must retain every earlier target's light",
    );
    assert_eq!(
        materialization_prefix_for_export_prefix(
            &admissions,
            (header.cx0, header.cx1, header.cz0, header.cz1),
            &END_LIGHT_NEIGHBOUR_OFFSETS,
            header.count as usize,
        ),
        admissions.len(),
        "the complete export still requires the complete admission stream",
    );
}

#[test]
fn materialization_prefix_bound_uses_x_fast_export_rows_and_supplied_dependencies() {
    let admissions = (0..=3)
        .flat_map(|z| (-1..=2).map(move |x| (x, z - 1)))
        .collect::<Vec<_>>();
    let dependencies = [(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)];

    assert_eq!(
        materialization_prefix_for_export_prefix(&admissions, (0, 1, 0, 1), &dependencies, 1),
        11,
        "the first x-fast export record uses the full dependency footprint",
    );
    assert_eq!(
        materialization_prefix_for_export_prefix(&admissions, (0, 1, 0, 1), &dependencies, 2),
        12,
        "the second x-fast export record advances within the same z row",
    );
    assert_eq!(
        materialization_prefix_for_export_prefix(&admissions, (0, 1, 0, 1), &dependencies, 3),
        15,
        "the third export record starts the next z row",
    );
    assert_eq!(
        materialization_prefix_for_export_prefix(&admissions, (0, 1, 0, 1), &dependencies, 0),
        0,
        "an empty export prefix requires no admissions",
    );
}

#[test]
fn raw_packet_mismatch_marks_same_prefix_different_full_digest_as_collision() {
    let expected_full = [0x11; PACKET_AUDIT_RECORD_BYTES];
    let mut actual_full = expected_full;
    actual_full[PACKET_AUDIT_RECORD_BYTES - 1] ^= 1;
    let collision = RawPacketMismatch {
        target: (3, 4),
        index: 17,
        expected_prefix: [0x11, 0x22],
        actual_prefix: [0x11, 0x22],
        expected_full,
        actual_full,
        payload_bytes: 123,
    };
    assert!(collision.collision());
    let prefix_mismatch = RawPacketMismatch {
        actual_prefix: [0x11, 0x23],
        ..collision
    };
    assert!(!prefix_mismatch.collision());
    let summary = format_diagnostic_summary(true, 1, 1, &[], &[collision], &[]);
    assert!(summary.contains("raw-packet mismatches=1 collisions=1"));
}

#[test]
fn parity_batch_selection_requires_scan_all_and_two_thousand_targets() {
    assert_eq!(
        parity_batch_limit(4_000, Some(2_000), Some(2_000), None, true),
        Ok(2_000),
    );
    assert!(parity_batch_limit(4_000, None, Some(1_999), None, true).is_err());
    assert!(parity_batch_limit(4_000, None, Some(2_000), None, false).is_err());
    assert!(parity_batch_limit(1_999, None, Some(2_000), None, true).is_err());
    assert!(parity_batch_limit(4_000, None, Some(2_000), Some(3), true).is_err());
    assert!(parity_batch_limit(4_000, Some(256), Some(2_000), None, true).is_err());
    assert_eq!(
        parity_batch_limit(4_000, Some(2), None, None, false),
        Ok(2),
    );
}

#[test]
fn lifecycle_target_digest_reader_seeks_to_only_the_selected_payload_row() {
    let mut bytes = vec![0u8; HEADER_BYTES];
    bytes.extend((0..4).flat_map(|index| [index as u8; 32]));
    let mut reader = std::io::Cursor::new(bytes);
    let digest = read_manifest_digest_at(&mut reader, 2).expect("selected manifest digest");
    assert_eq!(digest, [2u8; 32]);
    assert_eq!(reader.position(), HEADER_BYTES as u64 + 2 * 32 + 32);
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

/// Cross-language control for P06: the Rust comparator hashes the exact packet
/// body emitted by the independent exporter, then checks both the two-byte
/// manifest record and its full-digest audit sidecar.
#[test]
#[ignore = "requires a one-chunk Java v6 raw export; see docs/worldgen-large-parity.md"]
fn java_and_rust_raw_packet_digests_agree() {
    let packet_path = std::env::var("LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_PACKET")
        .expect("set LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_PACKET to Java's one-chunk packet body");
    let manifest_path = std::env::var("LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_MANIFEST")
        .expect("set LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_MANIFEST to Java's one-chunk v6 manifest");
    let audit_path = std::env::var("LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_PACKET_AUDIT")
        .unwrap_or_else(|_| format!("{manifest_path}.packet-audit"));
    let manifest_bytes = std::fs::read(&manifest_path).expect("read Java v6 manifest");
    assert!(manifest_bytes.len() >= HEADER_BYTES + RAW_PACKET_HASH_BYTES);
    let mut raw_header = [0u8; HEADER_BYTES];
    raw_header.copy_from_slice(&manifest_bytes[..HEADER_BYTES]);
    let header = read_header(&raw_header[..]).expect("validate Java v6 manifest header");
    assert_eq!(header.semantic_version, 6, "raw packet control requires P06");
    assert_eq!(header.count, 1, "raw packet control must use exactly one chunk");
    let expected_prefix: [u8; RAW_PACKET_HASH_BYTES] = manifest_bytes
        [HEADER_BYTES..HEADER_BYTES + RAW_PACKET_HASH_BYTES]
        .try_into()
        .expect("v6 manifest prefix width");

    let audit_bytes = std::fs::read(&audit_path).expect("read Java v6 packet-audit sidecar");
    assert!(audit_bytes.len() >= HEADER_BYTES + PACKET_AUDIT_RECORD_BYTES);
    let mut audit_raw_header = [0u8; HEADER_BYTES];
    audit_raw_header.copy_from_slice(&audit_bytes[..HEADER_BYTES]);
    let audit = read_packet_audit_header(&audit_raw_header[..])
        .expect("validate Java v6 packet-audit header");
    validate_packet_audit_header(&header, &audit).expect("sidecar identity must match manifest");
    let expected_full: [u8; PACKET_AUDIT_RECORD_BYTES] = audit_bytes
        [HEADER_BYTES..HEADER_BYTES + PACKET_AUDIT_RECORD_BYTES]
        .try_into()
        .expect("packet-audit digest width");
    verify_raw_packet_audit_pair(
        std::io::Cursor::new(expected_prefix),
        std::io::Cursor::new(expected_full),
        1,
        payload_digest_from_header(&raw_header),
        audit.payload_digest,
    )
    .expect("Java manifest and packet-audit records must authenticate together");

    let packet = std::fs::read(packet_path).expect("read Java packet body");
    let actual_full = raw_packet_full_digest(&packet);
    assert_eq!(actual_full, expected_full, "Rust SHA-256 of Java's exact body must match audit digest");
    assert_eq!([actual_full[0], actual_full[1]], expected_prefix, "Rust raw prefix must match Java manifest");
}

/// Reads any authenticated frozen-world shard strictly sequentially and uses
/// only one full semantic digest at a time.
#[test]
#[ignore = "manual external oracle comparison; see docs/worldgen-large-parity.md"]
fn parity_manifest_streams_before_rust_comparison() {
    let path = std::env::var("LODESTONE_LARGE_PARITY_MANIFEST").expect("set LODESTONE_LARGE_PARITY_MANIFEST=/absolute/path/to/merged.lwp");
    let mut raw_header = [0; HEADER_BYTES]; let mut f = File::open(&path).expect("open manifest"); f.read_exact(&mut raw_header).expect("read header");
    let h = read_header(&raw_header[..]).expect("valid parity shard header");
    let raw_packet = h.semantic_version == 6;
    if std::env::var_os("LODESTONE_LARGE_PARITY_REQUIRE_FULL_GRID").is_some() {
        let (grid_min, grid_max, grid_count) = if raw_packet {
            (
                support::large_parity_manifest::RAW_GRID_MIN,
                support::large_parity_manifest::RAW_GRID_MAX,
                support::large_parity_manifest::RAW_GRID_COUNT,
            )
        } else {
            (
                support::large_parity_manifest::GRID_MIN,
                support::large_parity_manifest::GRID_MAX,
                support::large_parity_manifest::GRID_COUNT,
            )
        };
        assert_eq!((h.cx0,h.cx1,h.cz0,h.cz1,h.count), (
            grid_min, grid_max, grid_min, grid_max, grid_count,
        ));
    }
    let audit = if raw_packet {
        Some(load_raw_packet_audit(Path::new(&path), &h))
    } else {
        None
    };
    if let Some((audit_path, audit_header)) = &audit {
        verify_raw_packet_audit_files(
            Path::new(&path),
            &raw_header,
            &h,
            audit_path,
            audit_header,
        );
    } else {
        let mut payload_file = File::open(&path).expect("reopen manifest");
        payload_file.seek(SeekFrom::Start(HEADER_BYTES as u64)).expect("seek payload");
        verify_manifest_payload(
            BufReader::new(payload_file),
            &h,
            payload_digest_from_header(&raw_header),
        )
        .expect("payload integrity");
    }
    let mut payload_file = File::open(&path).expect("reopen manifest payload");
    payload_file.seek(SeekFrom::Start(HEADER_BYTES as u64)).expect("seek payload");
    let mut expected = BufReader::new(payload_file);
    let mut expected_audit = audit.as_ref().map(|(audit_path, _)| {
        let mut file = File::open(audit_path)
            .unwrap_or_else(|error| panic!("reopen packet-audit sidecar {}: {error}", audit_path.display()));
        file.seek(SeekFrom::Start(HEADER_BYTES as u64))
            .unwrap_or_else(|error| panic!("seek packet-audit payload {}: {error}", audit_path.display()));
        BufReader::new(file)
    });
    let dimension = h.dimension;
    let server_dimension = match dimension {
        Dimension::Overworld => ServerDimension::Overworld,
        Dimension::Nether => ServerDimension::Nether,
        Dimension::End => ServerDimension::End,
    };
    let max_chunks = match std::env::var("LODESTONE_LARGE_PARITY_MAX_CHUNKS") {
        Ok(value) => Some(value.parse::<u64>().unwrap_or_else(|error| {
            panic!("LODESTONE_LARGE_PARITY_MAX_CHUNKS must be a non-negative integer: {error}")
        })),
        Err(std::env::VarError::NotPresent) => None,
        Err(error) => panic!("could not read LODESTONE_LARGE_PARITY_MAX_CHUNKS: {error}"),
    };
    let scan_all = std::env::var_os("LODESTONE_LARGE_PARITY_SCAN_ALL").is_some();
    let target_index = optional_usize_env("LODESTONE_LARGE_PARITY_TARGET_INDEX")
        .unwrap_or_else(|error| panic!("invalid lifecycle target selection: {error}"));
    if raw_packet && target_index.is_some() {
        panic!("LODESTONE_LARGE_PARITY_TARGET_INDEX is only supported for semantic lifecycle manifests");
    }
    let batch_size = optional_u64_env("LODESTONE_LARGE_PARITY_BATCH_SIZE")
        .unwrap_or_else(|error| panic!("invalid parity batch selection: {error}"));
    let limit = parity_batch_limit(h.count, max_chunks, batch_size, target_index, scan_all)
        .unwrap_or_else(|error| panic!("invalid parity batch selection: {error}"));
    if batch_size.is_some() {
        eprintln!(
            "large {} parity: authenticated scan-all batch selected ({limit} targets)",
            if raw_packet { "raw-packet" } else { "semantic" },
        );
    }
    let reference_packets = if scan_all {
        load_reference_packets(dimension, h.cx0, h.cx1, h.cz0, h.cz1, limit)
    } else {
        BTreeMap::new()
    };
    if raw_packet && dimension == Dimension::End && std::env::var_os(PERSISTED_WORLD_ROOT_ENV).is_some() {
        let root = validated_persisted_world_root(&h);
        eprintln!(
            "large persisted-world import/encoder parity: opening validated End root {} (diagnostic only; generated replay remains acceptance authority)",
            root.display(),
        );
        let persisted = RegionChunkSource::new(
            end_chunk_source(42),
            &root,
            ServerDimension::End,
            0,
            256,
        )
        .unwrap_or_else(|error| panic!("open validated persisted End root {}: {error}", root.display()));
        let generated_before = persisted
            .stats()
            .generated
            .load(std::sync::atomic::Ordering::Relaxed);
        let loaded_before = persisted
            .stats()
            .loaded_from_disk
            .load(std::sync::atomic::Ordering::Relaxed);
        let persisted_mismatches = compare_end_raw_from_persisted_world(
            &persisted,
            Path::new(&path),
            &h,
            limit,
            scan_all,
            &reference_packets,
            persisted_batch_size(),
        );
        let generated_after = persisted
            .stats()
            .generated
            .load(std::sync::atomic::Ordering::Relaxed);
        let loaded_after = persisted
            .stats()
            .loaded_from_disk
            .load(std::sync::atomic::Ordering::Relaxed);
        assert_eq!(
            generated_after - generated_before,
            0,
            "persisted End diagnostic fell back to generated columns",
        );
        assert!(
            loaded_after > loaded_before,
            "persisted End diagnostic loaded no columns from disk",
        );
        eprintln!(
            "large persisted-world import/encoder parity: loaded_from_disk_delta={} generated_delta={} packet_mismatches={}",
            loaded_after - loaded_before,
            generated_after - generated_before,
            persisted_mismatches.len(),
        );
    }
    let mut digest_mismatches = Vec::new();
    let mut raw_mismatches = Vec::new();
    let mut component_reports = Vec::new();
    let partial_lifecycle = is_partial_lifecycle_manifest(&h);
    if target_index.is_some() {
        assert!(
            partial_lifecycle,
            "LODESTONE_LARGE_PARITY_TARGET_INDEX requires an authenticated 16x16 Overworld or Nether lifecycle manifest",
        );
    }
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
                target_index,
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
                target_index,
                &reference_packets,
                &mut digest_mismatches,
                &mut component_reports,
            ),
            Dimension::End => panic!("partial lifecycle capture is only available for Overworld and Nether"),
        }
    } else {
        let width = u64::try_from(i64::from(h.cx1) - i64::from(h.cx0) + 1)
            .expect("authenticated manifest coordinate width fits u64");
        let prepared_raw_targets = if raw_packet && dimension == Dimension::Nether {
            Some(raw_packet_targets(&h, limit))
        } else {
            None
        };
        let end_persistence_dir = (dimension == Dimension::End).then(|| {
            let dir = std::env::temp_dir().join(format!(
                "lodestone-end-large-parity-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            dir
        });
        if raw_packet && dimension == Dimension::End {
            let mut audit = expected_audit
                .take()
                .expect("v6 End raw-packet comparison has an audit reader");
            let dir = end_persistence_dir
                .as_deref()
                .expect("End raw replay must have a persistence directory");
            raw_mismatches.extend(compare_end_raw_after_generated_save_reopen(
                dir,
                &mut expected,
                &mut audit,
                &h,
                server_dimension,
                limit,
                scan_all,
                &reference_packets,
                &mut component_reports,
            ));
        } else {
        let source: Box<dyn ChunkSource> = match dimension {
            // A frozen external world is a retained, settled lifecycle result.
            // Keep the comparator on the server's retained-source path rather than
            // regenerating an isolated column for every packet request.
            Dimension::Overworld => Box::new(retained_chunk_source_for_view_radius(overworld_chunk_source(42), 8)),
            Dimension::Nether => {
                let source = nether_chunk_source(42);
                if let Some(targets) = prepared_raw_targets.as_deref() {
                    let capacity = source.generator().prepare_packet_replay(targets);
                    eprintln!(
                        "large raw-packet parity: prepared Nether immutable prefix for {} targets (capacity={capacity})",
                        targets.len(),
                    );
                }
                Box::new(retained_chunk_source_for_view_radius(source, 8))
            }
            Dimension::End => {
                let dir = end_persistence_dir
                    .as_deref()
                    .expect("End raw replay must have a persistence directory");
                let persistent = RegionChunkSource::new(
                    end_chunk_source(42),
                    dir,
                    server_dimension,
                    0,
                    256,
                )
                .unwrap_or_else(|error| {
                    panic!("open temporary End persistence source {}: {error}", dir.display())
                });
                Box::new(persistent)
            }
        };
        let column_for = |cx, cz| -> ChunkColumn { source.column(cx, cz) };
        let mut expected_digest = [0u8; 32];
        for index in 0..limit {
            let (expected_prefix, expected_full) = if raw_packet {
                let mut prefix = [0u8; RAW_PACKET_HASH_BYTES];
                expected.read_exact(&mut prefix).expect("manifest raw packet hash prefix");
                let mut full = [0u8; PACKET_AUDIT_RECORD_BYTES];
                expected_audit
                    .as_mut()
                    .expect("v6 raw-packet comparison has an audit reader")
                    .read_exact(&mut full)
                    .expect("packet-audit full packet digest");
                (Some(prefix), Some(full))
            } else {
                expected.read_exact(&mut expected_digest).expect("manifest semantic digest");
                (None, None)
            };
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
            let settled_column = if raw_packet && dimension == Dimension::Nether {
                let admitted_neighbour_offsets =
                    initial_light_admission_neighbour_offsets(index);
                let admitted_neighbours = admitted_neighbour_offsets
                    .iter()
                    .map(|&(dx, dz)| (dx, dz, column_for(cx + dx, cz + dz)))
                    .collect::<Vec<_>>();
                initial_light_snapshot_for_admission(
                    &V770ServerProtocol,
                    &column,
                    &admitted_neighbours,
                    server_dimension,
                )
            } else {
                column.clone()
            };
            let directive = V770ServerProtocol
                .try_encode_chunk_with_neighbours_in_dimension(
                    cx,
                    cz,
                    &settled_column,
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
            if raw_packet {
                let expected_prefix = expected_prefix.expect("raw prefix selected");
                let expected_full = expected_full.expect("raw full digest selected");
                let actual_full = raw_packet_full_digest(&payload);
                let actual_prefix = [actual_full[0], actual_full[1]];
                if actual_prefix != expected_prefix || actual_full != expected_full {
                    let mismatch = RawPacketMismatch {
                        target: (cx, cz),
                        index,
                        expected_prefix,
                        actual_prefix,
                        expected_full,
                        actual_full,
                        payload_bytes: payload.len(),
                    };
                    if !scan_all {
                        let packet_summary = std::env::var_os("LODESTONE_LARGE_PARITY_REFERENCE_PACKET")
                            .map(|path| packet_difference_summary(&std::fs::read(path).expect("read authoritative packet capture"), &payload, dimension));
                        panic!(
                            "large raw-packet parity mismatch at ({cx},{cz}) after {index} matching chunks: expected prefix {}, actual prefix {}, expected full SHA-256 {}, actual full SHA-256 {}, collision={}, payload bytes={}{}",
                            hex(&mismatch.expected_prefix),
                            hex(&mismatch.actual_prefix),
                            hex(&mismatch.expected_full),
                            hex(&mismatch.actual_full),
                            mismatch.collision(),
                            mismatch.payload_bytes,
                            packet_summary.as_deref().unwrap_or(""),
                        );
                    }
                    raw_mismatches.push(mismatch);
                }
            } else {
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
            }
            if let Some(reference_path) = reference_packets.get(&(cx, cz)) {
                let reference_packet = std::fs::read(reference_path)
                    .unwrap_or_else(|error| panic!("read {}: {error}", reference_path.display()));
                let report = packet_component_difference(&reference_packet, &payload, dimension);
                component_reports.push(((cx, cz), report));
            }
            if (index + 1) % 256 == 0 || index + 1 == limit {
                eprintln!(
                    "large {} parity: compared {}/{} chunks (batch boundary at ({cx},{cz}))",
                    if raw_packet { "raw-packet" } else { "semantic" },
                    index + 1,
                    limit,
                );
            }
        }
        drop(source);
        }
        if let Some(dir) = end_persistence_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    if limit < h.count {
        eprintln!(
            "large {} parity: bounded pilot completed successfully at {} chunks; full grid remains pending",
            if raw_packet { "raw-packet" } else { "semantic" },
            limit,
        );
    }
    let diagnostic = format_diagnostic_report(
        raw_packet,
        limit,
        h.count,
        &digest_mismatches,
        &raw_mismatches,
        &component_reports,
    );
    let has_component_mismatches = component_reports.iter().any(|(_, report)| report.has_mismatch());
    let has_failing_component_mismatches = component_reports.iter().any(|(_, report)| report.has_failing_mismatch());
    if !digest_mismatches.is_empty() || !raw_mismatches.is_empty() || has_component_mismatches {
        if let Some(path) = std::env::var_os("LODESTONE_LARGE_PARITY_DIAGNOSTIC_OUT") {
            std::fs::write(path, &diagnostic).expect("write parity diagnostic report");
        }
    }
    if !digest_mismatches.is_empty() || !raw_mismatches.is_empty() || has_failing_component_mismatches {
        panic!(
            "{}",
            format_diagnostic_summary(
                raw_packet,
                limit,
                h.count,
                &digest_mismatches,
                &raw_mismatches,
                &component_reports,
            )
        );
    }
}

fn lifecycle_replay_events(capture: &LifecycleCapture) -> Vec<LifecycleReplayEvent> {
    capture.feature_events.iter().map(|event| LifecycleReplayEvent {
        source: event.source,
        stage: event.stage,
        sequence: event.completion_sequence,
    }).collect()
}

fn replay_full_capture<S: LifecycleWorldgenSource>(
    mut materializer: LifecycleMaterializer<S>,
    capture: &LifecycleCapture,
) -> LifecycleMaterializer<S> {
    for admission in &capture.admissions { materializer.admit(admission.chunk); }
    for event in &capture.feature_events {
        materializer.complete(event.source, event.stage, event.completion_sequence);
    }
    materializer
}

fn replay_plan_without_preparation<S: LifecycleWorldgenSource>(
    mut materializer: LifecycleMaterializer<S>,
    plan: &LifecycleReplayPlan,
) -> LifecycleMaterializer<S> {
    for &admission in plan.admissions() {
        materializer.admit(admission);
    }
    for event in plan.feature_events() {
        materializer.complete(event.source, event.stage, event.sequence);
    }
    materializer
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LifecycleReplayMode {
    Full,
    Pruned,
}

fn lifecycle_replay_mode(limit: u64, scan_all: bool) -> LifecycleReplayMode {
    if limit == 1 && !scan_all {
        LifecycleReplayMode::Pruned
    } else {
        LifecycleReplayMode::Full
    }
}

#[test]
fn lifecycle_replay_mode_only_prunes_single_fail_fast_target() {
    assert_eq!(lifecycle_replay_mode(1, false), LifecycleReplayMode::Pruned);
    assert_eq!(lifecycle_replay_mode(2, false), LifecycleReplayMode::Full);
    assert_eq!(lifecycle_replay_mode(1, true), LifecycleReplayMode::Full);
    assert_eq!(lifecycle_replay_mode(256, true), LifecycleReplayMode::Full);
}

fn lifecycle_packet_payload<S: LifecycleWorldgenSource>(
    materializer: &LifecycleMaterializer<S>,
    target: ChunkPos,
    dimension: ServerDimension,
) -> Vec<u8> {
    let column = materializer.snapshot_for_packet(target);
    let mut neighbours = Vec::with_capacity(8);
    for dz in -1..=1 {
        for dx in -1..=1 {
            if (dx, dz) == (0, 0) { continue; }
            let neighbour = (target.0 + dx, target.1 + dz);
            let column = materializer.resident_column(neighbour).unwrap_or_else(|| {
                panic!("target {target:?} is missing admitted packet-light neighbour {neighbour:?}")
            });
            neighbours.push((dx, dz, column.clone()));
        }
    }
    let directive = V770ServerProtocol
        .try_encode_chunk_with_neighbours_in_dimension(target.0, target.1, &column, &neighbours, dimension)
        .expect("production neighbour-aware chunk encoder");
    match directive {
        ServerDirective::Send { packet_id, payload } => {
            assert_eq!(packet_id, lodestone_v26_2::packet_ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT);
            payload
        }
        other => panic!("production chunk encoder returned {other:?} at {target:?}"),
    }
}

fn nether_packet_payload<S: ChunkSource>(source: &S, target: ChunkPos) -> Vec<u8> {
    let column = source.column(target.0, target.1);
    let mut neighbours = Vec::with_capacity(8);
    for dz in -1..=1 {
        for dx in -1..=1 {
            if (dx, dz) != (0, 0) {
                neighbours.push((dx, dz, source.column(target.0 + dx, target.1 + dz)));
            }
        }
    }
    let directive = V770ServerProtocol
        .try_encode_chunk_with_neighbours_in_dimension(
            target.0,
            target.1,
            &column,
            &neighbours,
            ServerDimension::Nether,
        )
        .expect("production neighbour-aware chunk encoder");
    match directive {
        ServerDirective::Send { packet_id, payload } => {
            assert_eq!(packet_id, lodestone_v26_2::packet_ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT);
            payload
        }
        other => panic!("production chunk encoder returned {other:?} at {target:?}"),
    }
}

/// Full replay remains the acceptance authority; this checks the static
/// target optimisation against its raw packet bytes at the audited corner.
#[test]
#[ignore = "requires an authenticated lifecycle capture and accepted manifest; see docs/worldgen-large-parity.md"]
fn full_and_pruned_lifecycle_replay_have_identical_target_packet_bytes() {
    let path = std::env::var("LODESTONE_LARGE_PARITY_MANIFEST")
        .expect("set LODESTONE_LARGE_PARITY_MANIFEST to an accepted 16x16 manifest");
    assert_full_and_pruned_lifecycle_packet_bytes(Path::new(&path), (-8, -8), true);
}

/// The immutable FEATURES context is an execution optimisation only. This
/// control replays the same authenticated target closure with and without the
/// prepared slots and compares the exact production packet bytes. The timings
/// are diagnostic: context preparation must not alter source order, RNG state,
/// resident writes, or the encoded target.
#[test]
#[ignore = "requires an authenticated lifecycle capture and accepted manifest; see docs/worldgen-large-parity.md"]
fn prepared_overworld_replay_preserves_authenticated_target_packet_bytes() {
    let path = std::env::var("LODESTONE_LARGE_PARITY_MANIFEST")
        .expect("set LODESTONE_LARGE_PARITY_MANIFEST to an accepted 16x16 Overworld manifest");
    let mut raw_header = [0; HEADER_BYTES];
    let mut manifest = File::open(&path).expect("open accepted manifest");
    manifest.read_exact(&mut raw_header).expect("read manifest header");
    let header = read_header(&raw_header[..]).expect("validate accepted manifest header");
    assert_eq!(header.dimension, Dimension::Overworld, "the context control is Overworld-specific");
    let capture = LifecycleCapture::load(header.dimension, &header, Path::new(&path));
    let target = (-8, -8);
    let admissions = capture
        .admissions
        .iter()
        .map(|admission| admission.chunk)
        .collect::<Vec<_>>();
    let events = lifecycle_replay_events(&capture);
    let plan = LifecycleReplayPlan::for_target(target, &admissions, &events)
        .unwrap_or_else(|error| panic!("static target replay plan rejected authenticated capture: {error}"));
    assert_eq!(plan.target(), target);
    assert_eq!(plan.admissions().len(), 105, "audited target closure changed");
    assert_eq!(plan.feature_events().len(), 59, "audited target event closure changed");

    let unprepared_start = Instant::now();
    let unprepared = replay_plan_without_preparation(
        LifecycleMaterializer::new(overworld_chunk_source(42)),
        &plan,
    );
    let unprepared_us = unprepared_start.elapsed().as_micros();

    let prepared_start = Instant::now();
    let mut prepared = LifecycleMaterializer::new(overworld_chunk_source(42));
    prepared.prepare_lifecycle_replay(plan.admissions());
    let prepared = replay_plan_without_preparation(prepared, &plan);
    let prepared_us = prepared_start.elapsed().as_micros();

    let unprepared_packet = lifecycle_packet_payload(
        &unprepared,
        target,
        ServerDimension::Overworld,
    );
    let prepared_packet = lifecycle_packet_payload(
        &prepared,
        target,
        ServerDimension::Overworld,
    );
    assert_eq!(
        unprepared_packet, prepared_packet,
        "prepared immutable replay context changed authenticated target packet bytes"
    );
    println!(
        "AUTH_REPLAY_CONTEXT_BENCH target={target:?} admissions={} events={} unprepared_us={} prepared_us={}",
        plan.admissions().len(),
        plan.feature_events().len(),
        unprepared_us,
        prepared_us,
    );
}

/// Raw-byte identity control for the Nether target-index pilot. This is kept
/// separate from the streaming gate because the full side intentionally
/// replays every captured admission and is expensive.
#[test]
#[ignore = "requires an authenticated lifecycle capture and accepted manifest; run only after reviewing the full replay cost"]
fn nether_target_index_7_full_and_pruned_packet_bytes_match() {
    let path = std::env::var("LODESTONE_LARGE_PARITY_MANIFEST")
        .expect("set LODESTONE_LARGE_PARITY_MANIFEST to an accepted 16x16 Nether manifest");
    let mut raw_header = [0; HEADER_BYTES];
    let mut manifest = File::open(&path).expect("open accepted manifest");
    manifest.read_exact(&mut raw_header).expect("read manifest header");
    let header = read_header(&raw_header[..]).expect("validate accepted manifest header");
    assert_eq!(header.dimension, Dimension::Nether, "target-index-7 raw control is Nether-specific");
    let capture = LifecycleCapture::load(header.dimension, &header, Path::new(&path));
    assert_eq!(capture.target_order.get(7), Some(&(-1, -8)), "authenticated target order changed for index 7");
    assert_full_and_pruned_lifecycle_packet_bytes(Path::new(&path), (-1, -8), false);
}

/// Cache retention is an optimisation only: the exact packet bytes must be
/// unchanged when the packet closure is retained instead of demand-evicted.
#[test]
#[ignore = "long-running raw-byte identity control; see docs/worldgen-large-parity.md"]
fn nether_packet_replay_cache_preserves_raw_packet_bytes() {
    let target = (0, 0);
    let baseline_source = nether_chunk_source(42);
    let baseline = nether_packet_payload(&baseline_source, target);
    assert_eq!(baseline_source.generator().pre_decoration_computations(), 217);
    assert_eq!(baseline_source.generator().pre_decoration_evictions(), 6);

    let prepared_source = nether_chunk_source(42);
    let capacity = prepared_source.generator().prepare_packet_replay(&[target]);
    assert_eq!(capacity, 49, "one packet's 3x3 targets have a 7x7 prefix closure");
    let prepared = nether_packet_payload(&prepared_source, target);

    assert_eq!(baseline, prepared, "immutable-stage retention must not alter packet bytes");
    assert_eq!(prepared_source.generator().pre_decoration_computations(), 49);
    assert_eq!(prepared_source.generator().pre_decoration_evictions(), 0);
    assert!(baseline_source.generator().pre_decoration_computations() > capacity);
    assert!(baseline_source.generator().pre_decoration_evictions() > 0);
}

fn assert_full_and_pruned_lifecycle_packet_bytes(path: &Path, target: ChunkPos, audit_corner_counts: bool) {
    let mut raw_header = [0; HEADER_BYTES];
    let mut manifest = File::open(path).expect("open accepted manifest");
    manifest.read_exact(&mut raw_header).expect("read manifest header");
    let header = read_header(&raw_header[..]).expect("validate accepted manifest header");
    assert_eq!(header.count, 256, "the lifecycle byte control requires the accepted 16x16 manifest");
    let capture = LifecycleCapture::load(header.dimension, &header, path);
    assert!(capture.target_order.contains(&target), "target {target:?} is absent from the authenticated target order");
    let admissions = capture.admissions.iter().map(|admission| admission.chunk).collect::<Vec<_>>();
    let events = lifecycle_replay_events(&capture);
    let plan = LifecycleReplayPlan::for_target(target, &admissions, &events)
        .unwrap_or_else(|error| panic!("static target replay plan rejected authenticated capture: {error}"));
    assert_eq!(plan.target(), target);
    if audit_corner_counts {
        assert_eq!(target, (-8, -8));
        assert_eq!(plan.feature_events().len(), 59, "audited tiled target closure must retain 59 FEATURES events");
        assert_eq!(plan.admissions().len(), 105, "audited target closure must admit 105 resident destinations");
    }
    let server_dimension = match header.dimension {
        Dimension::Overworld => ServerDimension::Overworld,
        Dimension::Nether => ServerDimension::Nether,
        Dimension::End => panic!("lifecycle byte control has no End capture"),
    };
    match header.dimension {
        Dimension::Overworld => {
            let full = replay_full_capture(LifecycleMaterializer::new(overworld_chunk_source(42)), &capture);
            let mut pruned = LifecycleMaterializer::new(overworld_chunk_source(42));
            pruned.replay_plan(&plan);
            assert_eq!(lifecycle_packet_payload(&full, target, server_dimension), lifecycle_packet_payload(&pruned, target, server_dimension));
        }
        Dimension::Nether => {
            let mut full_source = LifecycleMaterializer::new(nether_chunk_source(42));
            full_source.prepare_lifecycle_replay(&admissions);
            let full = replay_full_capture(full_source, &capture);
            let mut pruned = LifecycleMaterializer::new(nether_chunk_source(42));
            pruned.prepare_lifecycle_replay(plan.admissions());
            pruned.replay_plan(&plan);
            assert_eq!(lifecycle_packet_payload(&full, target, server_dimension), lifecycle_packet_payload(&pruned, target, server_dimension));
        }
        Dimension::End => unreachable!(),
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
    target_index: Option<usize>,
    reference_packets: &BTreeMap<ChunkPos, PathBuf>,
    digest_mismatches: &mut Vec<(i32, i32, [u8; 32], [u8; 32])>,
    component_reports: &mut Vec<((i32, i32), PacketComponentReport)>,
) {
    let replay_mode = lifecycle_replay_mode(limit, scan_all);
    let full_admissions = capture.admissions.iter().map(|admission| admission.chunk).collect::<Vec<_>>();
    // The external rows repeat the same source completion once per target that
    // observes it. The one-target, fail-fast pilot uses the authenticated
    // static dependency closure; every multi-target or scan-all run remains a
    // complete replay and therefore the acceptance authority. FULL/fence
    // telemetry is diagnostic only: the accepted manifest is a sealed final
    // world state, not a set of target-fence snapshots.
    match replay_mode {
        LifecycleReplayMode::Pruned => {
            let selected_index = target_index.unwrap_or(0);
            let target = capture
                .target_order
                .get(selected_index)
                .copied()
                .unwrap_or_else(|| panic!("lifecycle target index {selected_index} is outside the authenticated target order"));
            let events = lifecycle_replay_events(capture);
            let plan = LifecycleReplayPlan::for_target(target, &full_admissions, &events)
                .unwrap_or_else(|error| panic!("static target replay plan rejected authenticated capture: {error}"));
            assert_eq!(plan.target(), target);
            materializer.prepare_lifecycle_replay(plan.admissions());
            materializer.replay_plan(&plan);
            eprintln!("large lifecycle replay: pruned target index {selected_index} {target:?}, admitted {} destinations, applied {} FEATURES events", plan.admissions().len(), plan.feature_events().len());
        }
        LifecycleReplayMode::Full => {
            materializer.prepare_lifecycle_replay(&full_admissions);
            for &admission in &full_admissions {
                materializer.admit(admission);
            }
            eprintln!(
                "large lifecycle replay: admitted {} halo columns, applying {} FEATURES events; ignoring {} FULL telemetry events",
                full_admissions.len(),
                capture.feature_events.len(),
                capture.full_event_count,
            );
            for event in &capture.feature_events {
                materializer.complete(event.source, event.stage, event.completion_sequence);
            }
        }
    }
    if let Some(computations) = materializer.lifecycle_pre_decoration_computations() {
        // The full 18x18 capture's 5x5 source context is a 22x22 closure. The
        // A pruned target has an irregular clipped closure, so its count is
        // target-dependent and is telemetry rather than an acceptance
        // invariant. Full replay retains the fixed authenticated geometry.
        if replay_mode == LifecycleReplayMode::Full {
            assert_eq!(computations, 484, "Nether full lifecycle replay must cover its 22x22 source closure");
        }
        eprintln!("large lifecycle replay: Nether pre-decoration computations={computations} ({replay_mode:?} replay)");
    }

    let mut expected_digest = [0u8; 32];
    for index in 0..limit {
        let manifest_index = target_index.unwrap_or_else(|| usize::try_from(index).expect("manifest prefix index fits usize"));
        if target_index.is_some() {
            expected_digest = read_manifest_digest_at(expected, manifest_index)
                .expect("selected manifest semantic digest");
        } else {
            expected.read_exact(&mut expected_digest).expect("manifest semantic digest");
        }
        let target = capture
            .target_order
            .get(manifest_index)
            .copied()
            .expect("capture target order must cover the selected manifest digest");
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
        if let Some(reference_path) = reference_packets.get(&target) {
            let reference_packet = std::fs::read(reference_path)
                .unwrap_or_else(|error| panic!("read {}: {error}", reference_path.display()));
            component_reports.push((
                target,
                packet_component_difference(&reference_packet, &payload, dimension),
            ));
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
/// Keep component-state inventory bounded even when a batch contains many
/// unrelated bad cells.  The digest mismatch list remains exact for every
/// target; this cap applies only to the optional per-cell signature census.
const MAX_COMPONENT_SIGNATURES: usize = 32;
const MAX_REFERENCE_PACKET_BYTES: u64 = 8 * 1024 * 1024;
const MAX_REFERENCE_PACKET_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Default)]
struct ComponentDiff {
    total: usize,
    examples: Vec<String>,
    signatures: BTreeMap<String, usize>,
    signature_overflow: usize,
}

impl ComponentDiff {
    fn push(&mut self, signature: String, value: String) {
        self.total += 1;
        if let Some(count) = self.signatures.get_mut(&signature) {
            *count += 1;
        } else if self.signatures.len() < MAX_COMPONENT_SIGNATURES {
            self.signatures.insert(signature, 1);
        } else {
            self.signature_overflow += 1;
        }
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

#[test]
fn component_inventory_groups_signatures_without_unbounded_examples() {
    let mut first = PacketComponentReport::default();
    first
        .terrain
        .push("minecraft:air -> minecraft:stone".to_owned(), "cell a".to_owned());
    first
        .terrain
        .push("minecraft:air -> minecraft:stone".to_owned(), "cell b".to_owned());
    for index in 0..(MAX_COMPONENT_SIGNATURES + 1) {
        first
            .biomes
            .push(format!("{index} -> {}", index + 1), format!("cell {index}"));
    }
    let second = PacketComponentReport::default();
    let reports = vec![((3, 4), first), ((5, 6), second)];
    let groups = component_signature_groups(&reports);
    let terrain = groups
        .get(&("terrain", "minecraft:air -> minecraft:stone".to_owned()))
        .expect("repeated terrain signature is retained");
    assert_eq!(terrain.0, 2);
    assert_eq!(terrain.1, BTreeSet::from([(3, 4)]));
    assert_eq!(reports[0].1.biomes.total, MAX_COMPONENT_SIGNATURES + 1);
    assert_eq!(reports[0].1.biomes.signature_overflow, 1);
    assert_eq!(reports[0].1.biomes.examples.len(), MAX_COMPONENT_EXAMPLES);
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
) -> BTreeMap<ChunkPos, PathBuf> {
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
    let mut total_bytes = 0u64;
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
        total_bytes = total_bytes
            .checked_add(size)
            .expect("reference packet diagnostic byte count overflow");
        assert!(
            total_bytes <= MAX_REFERENCE_PACKET_TOTAL_BYTES,
            "reference packet inputs total {total_bytes} bytes, over the {MAX_REFERENCE_PACKET_TOTAL_BYTES}-byte diagnostic bound",
        );
        if packets.insert(coordinate, path.to_path_buf()).is_some() {
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
                let from = state_label(left);
                let to = state_label(right);
                report.terrain.push(
                    format!("{from} -> {to}"),
                    format!("({x},{y},{z}) {from} -> {to}"),
                );
            }
        }
        for cell in 0..64 {
            let left = reference_section.map_or(0, |value| value.biomes().get(cell));
            let right = actual_section.map_or(0, |value| value.biomes().get(cell));
            if left != right {
                let x = cell % 4;
                let z = (cell / 4) % 4;
                let y = cell / 16;
                report.biomes.push(
                    format!("{left} -> {right}"),
                    format!("section {section} ({x},{y},{z}) {left} -> {right}"),
                );
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
                if a != b {
                    report.heightmaps.push(
                        format!("id {id}: {a:?} -> {b:?}"),
                        format!("id {id} ({x},{z}) {a:?} -> {b:?}"),
                    );
                }
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
            let signature = format!("type {:?} -> {:?}", a.map(|entity| entity.type_id), b.map(|entity| entity.type_id));
            report.block_entities.push(signature, format!("index {index} at {position:?}"));
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
            if a != b {
                report.sky_light.push(
                    format!("{a} -> {b}"),
                    format!("section {section} ({x},{y},{z}) {a} -> {b}"),
                );
            }
            let a = left.block_at(x, y, z);
            let b = right.block_at(x, y, z);
            if a != b {
                report.block_light.push(
                    format!("{a} -> {b}"),
                    format!("section {section} ({x},{y},{z}) {a} -> {b}"),
                );
            }
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
            if tag(a) != tag(b) {
                report.masks.push(
                    format!("{layer}: {} -> {}", tag(a), tag(b)),
                    format!("{layer} section {section}: {} -> {}", tag(a), tag(b)),
                );
            }
        }
    }
    report
}

fn canonical_entity_key(entity: &lodestone_world::BlockEntity) -> (u8, i16, u8, u32, Vec<u8>) {
    let mut writer = Writer::default();
    canonical_nbt(&mut writer, &entity.nbt);
    (entity.rel_x, entity.y, entity.rel_z, entity.type_id, writer.into_vec())
}

fn add_component_signature_groups(
    groups: &mut BTreeMap<(&'static str, String), (usize, BTreeSet<ChunkPos>)>,
    component: &'static str,
    target: ChunkPos,
    diff: &ComponentDiff,
) {
    for (signature, count) in &diff.signatures {
        let entry = groups
            .entry((component, signature.clone()))
            .or_insert_with(|| (0, BTreeSet::new()));
        entry.0 += *count;
        entry.1.insert(target);
    }
}

fn component_signature_groups(
    components: &[((i32, i32), PacketComponentReport)],
) -> BTreeMap<(&'static str, String), (usize, BTreeSet<ChunkPos>)> {
    let mut groups = BTreeMap::new();
    for &(target, ref report) in components {
        add_component_signature_groups(&mut groups, "terrain", target, &report.terrain);
        add_component_signature_groups(&mut groups, "biomes", target, &report.biomes);
        add_component_signature_groups(&mut groups, "heightmaps", target, &report.heightmaps);
        add_component_signature_groups(&mut groups, "block_entities", target, &report.block_entities);
        add_component_signature_groups(&mut groups, "sky_light", target, &report.sky_light);
        add_component_signature_groups(&mut groups, "block_light", target, &report.block_light);
        add_component_signature_groups(&mut groups, "masks", target, &report.masks);
    }
    groups
}

fn raw_packet_diagnostic_groups(
    mismatches: &[RawPacketMismatch],
) -> BTreeMap<(String, String, String, String, bool), (usize, Vec<ChunkPos>)> {
    let mut groups = BTreeMap::new();
    for mismatch in mismatches {
        let key = (
            hex(&mismatch.expected_prefix),
            hex(&mismatch.actual_prefix),
            hex(&mismatch.expected_full),
            hex(&mismatch.actual_full),
            mismatch.collision(),
        );
        if !groups.contains_key(&key) && groups.len() >= MAX_RAW_DIAGNOSTIC_GROUPS {
            continue;
        }
        let entry = groups.entry(key).or_insert_with(|| (0, Vec::new()));
        entry.0 += 1;
        if entry.1.len() < MAX_RAW_DIAGNOSTIC_EXAMPLES {
            entry.1.push(mismatch.target);
        }
    }
    groups
}

fn format_diagnostic_report(
    raw_packet: bool,
    limit: u64,
    total: u64,
    digest: &[(i32, i32, [u8; 32], [u8; 32])],
    raw: &[RawPacketMismatch],
    components: &[((i32, i32), PacketComponentReport)],
) -> String {
    let label = if raw_packet { "raw-packet" } else { "semantic" };
    let raw_collisions = raw.iter().filter(|mismatch| mismatch.collision()).count();
    let mut output = format!(
        "large {label} parity diagnostic: compared {limit}/{total} chunks; digest mismatches={}; raw-packet mismatches={} collisions={} coordinates={:?}\n",
        digest.len(),
        raw.len(),
        raw_collisions,
        raw.iter().take(MAX_RAW_DIAGNOSTIC_EXAMPLES).map(|mismatch| (mismatch.index, mismatch.target)).collect::<Vec<_>>(),
    );
    let mut digest_groups = BTreeMap::<(String, String), BTreeSet<ChunkPos>>::new();
    for &(x, z, expected, actual) in digest {
        digest_groups
            .entry((hex(&expected), hex(&actual)))
            .or_default()
            .insert((x, z));
    }
    for ((expected, actual), coordinates) in digest_groups {
        output.push_str(&format!(
            "digest group expected={expected} actual={actual} chunks={coordinates:?}\n"
        ));
    }
    for ((expected_prefix, actual_prefix, expected_full, actual_full, collision), (count, coordinates)) in raw_packet_diagnostic_groups(raw) {
        output.push_str(&format!(
            "raw packet group expected_prefix={expected_prefix} actual_prefix={actual_prefix} expected_full={expected_full} actual_full={actual_full} collision={collision} mismatches={count} example_coordinates={coordinates:?}\n"
        ));
    }
    if (!digest.is_empty() || !raw.is_empty()) && components.is_empty() {
        output.push_str("component reports unavailable: set LODESTONE_LARGE_PARITY_REFERENCE_PACKET to an authoritative raw packet (or LODESTONE_LARGE_PARITY_REFERENCE_PACKET_DIR to reference*.packet files)\n");
    }
    for ((x, z), report) in components {
        output.push_str(&format!("packet ({x},{z}) component mismatches: terrain={} {:?} (signature_overflow={}); biomes={} {:?} (signature_overflow={}); heightmaps={} {:?} (signature_overflow={}); block_entities={} {:?} (signature_overflow={}); sky_light={} {:?} (signature_overflow={}); block_light={} {:?} (signature_overflow={}); masks={} {:?} (signature_overflow={})\n", report.terrain.total, report.terrain.examples, report.terrain.signature_overflow, report.biomes.total, report.biomes.examples, report.biomes.signature_overflow, report.heightmaps.total, report.heightmaps.examples, report.heightmaps.signature_overflow, report.block_entities.total, report.block_entities.examples, report.block_entities.signature_overflow, report.sky_light.total, report.sky_light.examples, report.sky_light.signature_overflow, report.block_light.total, report.block_light.examples, report.block_light.signature_overflow, report.masks.total, report.masks.examples, report.masks.signature_overflow));
    }
    if !components.is_empty() {
        output.push_str("grouped component signatures (chunk coordinates are exact for retained signatures):\n");
        for ((component, signature), (cells, coordinates)) in component_signature_groups(components) {
            output.push_str(&format!(
                "component={component} signature={signature:?} differing_cells={cells} chunks={coordinates:?}\n"
            ));
        }
    }
    output
}

fn format_diagnostic_summary(
    raw_packet: bool,
    limit: u64,
    total: u64,
    digest: &[(i32, i32, [u8; 32], [u8; 32])],
    raw: &[RawPacketMismatch],
    components: &[((i32, i32), PacketComponentReport)],
) -> String {
    let label = if raw_packet { "raw-packet" } else { "semantic" };
    let raw_collisions = raw.iter().filter(|mismatch| mismatch.collision()).count();
    let mut output = format!(
        "large {label} parity diagnostic: compared {limit}/{total} chunks; digest mismatches={} coordinates={:?}; raw-packet mismatches={} collisions={} coordinates={:?}",
        digest.len(),
        digest.iter().map(|(x, z, _, _)| (*x, *z)).collect::<Vec<_>>(),
        raw.len(),
        raw_collisions,
        raw.iter().take(MAX_RAW_DIAGNOSTIC_EXAMPLES).map(|mismatch| (mismatch.index, mismatch.target)).collect::<Vec<_>>(),
    );
    if (!digest.is_empty() || !raw.is_empty()) && components.is_empty() {
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
