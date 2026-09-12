//! Manual large-grid parity-manifest gate. It intentionally remains ignored:
//! a full 501² run is an external oracle job, not a regular unit test.
mod support { pub mod large_parity_manifest; }

use std::{collections::{BTreeMap, BTreeSet}, fs::File, io::{BufReader, Cursor, Read, Seek, SeekFrom}, path::{Path, PathBuf}, time::{Duration, Instant}};
use lodestone_core::{Reader, Writer};
use lodestone_server::{
    ChunkColumn, ChunkSource, RetainedLightStatus, ServerDirective, ServerProtocol,
    end_chunk_source,
    nether_chunk_source, overworld_chunk_source, retained_chunk_source_for_view_radius,
};
use lodestone_server::dimension::Dimension as ServerDimension;
use lodestone_server::region_source::{PersistenceStats, RegionChunkSource};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};
use lodestone_world::{ColumnLight, LightData};
use lodestone_worldgen_parity::lifecycle::{
    LifecycleCompletion, LifecycleMaterializer, LifecycleReplayEvent, LifecycleReplayPlan,
    LifecycleWorldgenSource, FEATURES_WRITE_RADIUS,
};
use lodestone_worldgen::stage_schedule::{ChunkRequest, NETHER_FEATURE_WRITE_RADIUS};
use support::large_parity_manifest::{
    Dimension, Header as ManifestHeader, IncrementalSha256, HEADER_BYTES, PACKET_AUDIT_RECORD_BYTES, RAW_PACKET_HASH_BYTES,
    LIGHT_FREE_AUDIT_RECORD_BYTES,
    canonical_nbt, payload_digest_from_header, sha256,
    light_free_record,
    raw_packet_full_digest, read_header, read_packet_audit_header,
    read_light_free_audit_header,
    semantic_digest, semantic_digest_for_dimension, semantic_digest_v5_for_dimension,
    semantic_record, semantic_record_for_dimension, semantic_record_v5_for_dimension,
    validate_light_free_audit_header, validate_packet_audit_header,
    verify_light_free_audit_pair, verify_payload, verify_manifest_payload,
    verify_raw_packet_audit_pair,
};

type ChunkPos = (i32, i32);

const MAX_RAW_DIAGNOSTIC_EXAMPLES: usize = 32;
const MAX_RAW_DIAGNOSTIC_GROUPS: usize = 64;
/// Keep the Nether immutable replay closure bounded while preserving the
/// manifest's z-major, x-fastest target order.
const NETHER_PACKET_REPLAY_WINDOW_ROWS: u64 = 1;
const PERSISTED_WORLD_ROOT_ENV: &str = "LODESTONE_LARGE_PARITY_FROZEN_WORLD_ROOT";
const PERSISTED_BATCH_SIZE_ENV: &str = "LODESTONE_LARGE_PARITY_PERSISTED_BATCH_SIZE";
const PERSISTED_ONLY_ENV: &str = "LODESTONE_LARGE_PARITY_PERSISTED_ONLY";
const TRACE_COLUMN_ENV: &str = "LODESTONE_LARGE_PARITY_TRACE_COLUMN";
const TRACE_OUT_ENV: &str = "LODESTONE_LARGE_PARITY_TRACE_OUT";
const PHASE_PROFILE_ENV: &str = "LODESTONE_LARGE_PARITY_PHASE_PROFILE";
const CONTENT_ONLY_ENV: &str = "LODESTONE_LARGE_PARITY_CONTENT_ONLY";
const PERSISTED_PACKET_OUT_ENV: &str = "LODESTONE_LARGE_PARITY_PERSISTED_PACKET_OUT";
const START_INDEX_ENV: &str = "LODESTONE_LARGE_PARITY_START_INDEX";
const MISMATCH_INVENTORY_ENV: &str = "LODESTONE_LARGE_PARITY_MISMATCH_OUT";
const MISMATCH_PACKET_DIR_ENV: &str = "LODESTONE_LARGE_PARITY_MISMATCH_PACKET_DIR";
const MISMATCH_REFERENCE_PACKET_DIR_ENV: &str = "LODESTONE_LARGE_PARITY_MISMATCH_REFERENCE_PACKET_DIR";
const MISMATCH_COMPONENT_REPORT_ENV: &str = "LODESTONE_LARGE_PARITY_MISMATCH_COMPONENT_REPORT";
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

#[derive(Default)]
struct PhaseProfile {
    enabled: bool,
    phases: BTreeMap<&'static str, PhaseProfileEntry>,
}

#[derive(Default)]
struct PhaseProfileEntry {
    elapsed: Duration,
    calls: u64,
    coordinates: BTreeSet<ChunkPos>,
    cache_hits: u64,
    cache_misses: u64,
    source_generated: u64,
    source_loaded_from_disk: u64,
}

impl PhaseProfile {
    fn from_env() -> Self {
        Self {
            enabled: std::env::var_os(PHASE_PROFILE_ENV).is_some(),
            phases: BTreeMap::new(),
        }
    }

    fn call<T>(&mut self, phase: &'static str, coordinate: Option<ChunkPos>, work: impl FnOnce() -> T) -> T {
        if !self.enabled {
            return work();
        }
        let started = Instant::now();
        let result = work();
        self.record(phase, coordinate, started.elapsed(), None);
        result
    }

    fn call_with_source_stats<T>(
        &mut self,
        phase: &'static str,
        coordinate: Option<ChunkPos>,
        stats: &PersistenceStats,
        work: impl FnOnce() -> T,
    ) -> T {
        if !self.enabled {
            return work();
        }
        let generated_before = stats
            .generated
            .load(std::sync::atomic::Ordering::Relaxed);
        let loaded_before = stats
            .loaded_from_disk
            .load(std::sync::atomic::Ordering::Relaxed);
        let result = self.call(phase, coordinate, work);
        let generated = stats
            .generated
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_sub(generated_before);
        let loaded_from_disk = stats
            .loaded_from_disk
            .load(std::sync::atomic::Ordering::Relaxed)
            .saturating_sub(loaded_before);
        let entry = self.phases.get_mut(phase).expect("profile phase exists");
        entry.source_generated += generated;
        entry.source_loaded_from_disk += loaded_from_disk;
        let misses = generated + loaded_from_disk;
        entry.cache_misses += misses;
        if misses == 0 {
            entry.cache_hits += 1;
        }
        result
    }

    fn source_column(
        &mut self,
        phase: &'static str,
        source: &dyn ChunkSource,
        stats: &PersistenceStats,
        coordinate: ChunkPos,
    ) -> ChunkColumn {
        if !self.enabled {
            return source.column(coordinate.0, coordinate.1);
        }
        let loaded_before = stats
            .loaded_from_disk
            .load(std::sync::atomic::Ordering::Relaxed);
        let generated_before = stats
            .generated
            .load(std::sync::atomic::Ordering::Relaxed);
        let started = Instant::now();
        let column = source.column(coordinate.0, coordinate.1);
        let loaded_after = stats
            .loaded_from_disk
            .load(std::sync::atomic::Ordering::Relaxed);
        let generated_after = stats
            .generated
            .load(std::sync::atomic::Ordering::Relaxed);
        let loaded_delta = loaded_after.saturating_sub(loaded_before);
        let generated_delta = generated_after.saturating_sub(generated_before);
        let cache_miss = loaded_delta != 0 || generated_delta != 0;
        self.record(phase, Some(coordinate), started.elapsed(), Some(!cache_miss));
        let entry = self.phases.get_mut(phase).expect("profile phase exists");
        entry.source_generated += generated_delta;
        entry.source_loaded_from_disk += loaded_delta;
        column
    }

    fn record(
        &mut self,
        phase: &'static str,
        coordinate: Option<ChunkPos>,
        elapsed: Duration,
        cache_hit: Option<bool>,
    ) {
        if !self.enabled {
            return;
        }
        let entry = self.phases.entry(phase).or_default();
        entry.elapsed += elapsed;
        entry.calls += 1;
        if let Some(coordinate) = coordinate {
            entry.coordinates.insert(coordinate);
        }
        match cache_hit {
            Some(true) => entry.cache_hits += 1,
            Some(false) => entry.cache_misses += 1,
            None => {}
        }
    }

    fn report(&self) {
        if !self.enabled {
            return;
        }
        for (phase, entry) in &self.phases {
            eprintln!(
                "large generated phase profile: phase={phase} wall_ms={:.3} calls={} unique_coordinates={} cache_hits={} cache_misses={} source_generated={} source_loaded_from_disk={}",
                entry.elapsed.as_secs_f64() * 1000.0,
                entry.calls,
                entry.coordinates.len(),
                entry.cache_hits,
                entry.cache_misses,
                entry.source_generated,
                entry.source_loaded_from_disk,
            );
        }
    }
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
        let expected_admissions = ChunkRequest::new(
            header.cx0,
            header.cx1,
            header.cz0,
            header.cz1,
            1,
        )
        .admission_order();
        assert_eq!(
            admissions.iter().map(|admission| admission.chunk).collect::<Vec<_>>(),
            expected_admissions,
            "external lifecycle admissions must match the named request wavefront",
        );
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

enum ManifestAuditHeader {
    Raw(support::large_parity_manifest::PacketAuditHeader),
    LightFree(support::large_parity_manifest::LightFreeAuditHeader),
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

fn light_free_audit_path(manifest: &Path) -> PathBuf {
    if let Some(path) = std::env::var_os("LODESTONE_LARGE_PARITY_LIGHT_FREE_AUDIT") {
        return PathBuf::from(path);
    }
    let mut path = manifest.as_os_str().to_os_string();
    path.push(".light-free-audit");
    PathBuf::from(path)
}

fn load_light_free_audit(
    manifest: &Path,
    main_header: &support::large_parity_manifest::Header,
) -> (PathBuf, support::large_parity_manifest::LightFreeAuditHeader) {
    let path = light_free_audit_path(manifest);
    let mut file = File::open(&path)
        .unwrap_or_else(|error| panic!("open v7 light-free audit sidecar {}: {error}", path.display()));
    let mut raw = [0u8; HEADER_BYTES];
    file.read_exact(&mut raw)
        .unwrap_or_else(|error| panic!("read v7 light-free audit sidecar {} header: {error}", path.display()));
    let header = read_light_free_audit_header(&raw[..]).unwrap_or_else(|error| {
        panic!("invalid v7 light-free audit sidecar {}: {error}", path.display())
    });
    validate_light_free_audit_header(main_header, &header).unwrap_or_else(|error| {
        panic!("v7 light-free audit sidecar {} does not match manifest: {error}", path.display())
    });
    (path, header)
}

fn verify_light_free_audit_files(
    manifest: &Path,
    raw_header: &[u8; HEADER_BYTES],
    main_header: &support::large_parity_manifest::Header,
    audit_path: &Path,
    audit_header: &support::large_parity_manifest::LightFreeAuditHeader,
) {
    let mut main = File::open(manifest).unwrap_or_else(|error| panic!("reopen v7 manifest {}: {error}", manifest.display()));
    main.seek(SeekFrom::Start(HEADER_BYTES as u64)).unwrap_or_else(|error| panic!("seek v7 manifest payload: {error}"));
    let mut audit = File::open(audit_path).unwrap_or_else(|error| panic!("reopen light-free audit sidecar {}: {error}", audit_path.display()));
    audit.seek(SeekFrom::Start(HEADER_BYTES as u64)).unwrap_or_else(|error| panic!("seek light-free audit payload: {error}"));
    verify_light_free_audit_pair(
        BufReader::new(main),
        BufReader::new(audit),
        main_header.count,
        payload_digest_from_header(raw_header),
        audit_header.payload_digest,
    )
    .unwrap_or_else(|error| panic!("v7 manifest/light-free audit payload authentication failed: {error}"));
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

/// Select a contiguous export range while retaining the complete causal
/// replay prefix. `max_chunks`/`batch_size` remain the range length; the
/// optional start index only moves the comparison window forward.
fn parity_batch_range(
    count: u64,
    max_chunks: Option<u64>,
    batch_size: Option<u64>,
    target_index: Option<usize>,
    start_index: Option<usize>,
    scan_all: bool,
) -> Result<(u64, u64), String> {
    if start_index.is_some() && target_index.is_some() {
        return Err(format!("{START_INDEX_ENV} cannot be combined with LODESTONE_LARGE_PARITY_TARGET_INDEX"));
    }
    let start = u64::try_from(start_index.unwrap_or(0)).map_err(|_| "start index does not fit u64".to_owned())?;
    if start > count {
        return Err(format!("{START_INDEX_ENV}={start} exceeds the authenticated manifest count {count}"));
    }
    if start_index.is_some() && max_chunks.is_none() && batch_size.is_none() {
        return Err(format!("{START_INDEX_ENV} requires LODESTONE_LARGE_PARITY_MAX_CHUNKS or LODESTONE_LARGE_PARITY_BATCH_SIZE"));
    }
    let length = parity_batch_limit(count, max_chunks, batch_size, target_index, scan_all)?;
    let end = start
        .checked_add(length)
        .ok_or_else(|| "parity comparison range overflows u64".to_owned())?;
    if end > count {
        return Err(format!("parity comparison range {start}..{end} exceeds the authenticated manifest count {count}"));
    }
    Ok((start, end))
}

fn is_partial_lifecycle_manifest(header: &support::large_parity_manifest::Header) -> bool {
    header.semantic_version != 6 && header.semantic_version != 7
        && header.dimension != Dimension::End
        && header.count == 256
        && i64::from(header.cx1) - i64::from(header.cx0) == 15
        && i64::from(header.cz1) - i64::from(header.cz0) == 15
}

fn raw_packet_target(
    header: &support::large_parity_manifest::Header,
    index: u64,
    width: u64,
) -> ChunkPos {
    (
        header.cx0 + i32::try_from(index % width).expect("raw target x offset fits i32"),
        header.cz0 + i32::try_from(index / width).expect("raw target z offset fits i32"),
    )
}

fn nether_packet_replay_window_end(index: u64, width: u64, limit: u64) -> u64 {
    let window_size = width
        .checked_mul(NETHER_PACKET_REPLAY_WINDOW_ROWS)
        .expect("Nether replay window size fits u64");
    let window_start = (index / window_size) * window_size;
    (window_start + window_size).min(limit)
}

fn raw_packet_targets_for_window(
    header: &support::large_parity_manifest::Header,
    start: u64,
    end: u64,
    width: u64,
) -> Vec<ChunkPos> {
    (start..end)
        .map(|index| raw_packet_target(header, index, width))
        .collect()
}

/// The sealed-world exporter reads the saved light snapshot produced when a
/// column was first admitted. Materialization is z-major and x-major within
/// each row, so only the north row and west cell can have been admitted when
/// a target's initial snapshot is computed. Future east and south columns
/// must not leak into that first fallback; a later live relight may use them.
const INITIAL_ADMISSION_NEIGHBOUR_OFFSETS: [(i32, i32); 4] =
    [(-1, -1), (0, -1), (1, -1), (-1, 0)];

fn initial_admission_neighbour_offsets() -> &'static [(i32, i32)] {
    &INITIAL_ADMISSION_NEIGHBOUR_OFFSETS
}

fn raw_packet_targets_range(
    header: &support::large_parity_manifest::Header,
    start: u64,
    end: u64,
) -> Vec<ChunkPos> {
    let width = u64::try_from(i64::from(header.cx1) - i64::from(header.cx0) + 1)
        .expect("authenticated manifest coordinate width fits u64");
    (start..end)
        .map(|index| raw_packet_target(header, index, width))
        .collect()
}
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

fn nether_packet_replay_capacity_bound(width: u64, rows: u64) -> usize {
    let halo = 1_u64
        .checked_add(
            u64::try_from(lodestone_worldgen::feature::region_view::WIDE_RADIUS)
                .expect("Nether replay radius fits u64"),
        )
        .expect("Nether replay halo fits u64");
    usize::try_from(
        width
            .checked_add(halo * 2)
            .expect("Nether replay width fits u64")
            .checked_mul(
                rows.checked_add(halo * 2)
                    .expect("Nether replay height fits u64"),
            )
            .expect("Nether replay closure fits u64"),
    )
    .expect("Nether replay closure fits usize")
}

#[test]
fn nether_packet_replay_windows_preserve_order_and_retention_bound() {
    let header = support::large_parity_manifest::Header {
        semantic_version: 6,
        cx0: -500,
        cx1: 500,
        cz0: -500,
        cz1: 500,
        count: 1_002_001,
        frozen_world: [0; 32],
        dimension: Dimension::Nether,
        record_width: RAW_PACKET_HASH_BYTES as u16,
        kind: 2,
    };
    let width = 1_001;
    let first_end = nether_packet_replay_window_end(0, width, header.count);
    let second_end = nether_packet_replay_window_end(first_end, width, header.count);
    let first = raw_packet_targets_for_window(&header, 0, first_end, width);
    let second = raw_packet_targets_for_window(&header, first_end, second_end, width);

    assert_eq!(first.len(), width as usize);
    assert_eq!(second.len(), width as usize);
    assert_eq!(first[0], (-500, -500));
    assert_eq!(first[width as usize - 1], (500, -500));
    assert_eq!(second[0], (-500, -499));
    assert_eq!(
        first
            .iter()
            .chain(second.iter())
            .copied()
            .collect::<Vec<_>>(),
        (0..second_end)
            .map(|index| raw_packet_target(&header, index, width))
            .collect::<Vec<_>>(),
        "windowing must not reorder or skip manifest targets",
    );

    let window_capacity = nether_packet_replay_capacity_bound(width, NETHER_PACKET_REPLAY_WINDOW_ROWS);
    let full_capacity = nether_packet_replay_capacity_bound(width, width);
    assert_eq!(window_capacity, 7_049, "the 1001-wide one-row closure is bounded");
    assert!(window_capacity < full_capacity / 100, "retention must not scale with the full grid");
}

#[test]
fn nether_packet_replay_generator_capacity_is_row_bounded() {
    let source = nether_chunk_source(42);
    let targets = (-500..=500).map(|x| (x, -500)).collect::<Vec<_>>();
    let capacity = source.generator().prepare_packet_replay(&targets);
    assert_eq!(
        capacity,
        nether_packet_replay_capacity_bound(1_001, NETHER_PACKET_REPLAY_WINDOW_ROWS),
        "the generator must retain one row's target-plus-halo closure only",
    );
    source.generator().reset_packet_replay();
}

/// Replays one initial admission's saved-light boundary. The packet still
/// carries all eight terrain neighbours, but the retained centre light is
/// computed from the columns that existed at admission time.
fn initial_light_snapshot_for_admission<P: ServerProtocol>(
    proto: &P,
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

/// A radius-zero admission sees the complete immediate 3x3 footprint. Keep
/// this order explicit because the serial light lifecycle is part of the raw
/// packet contract; it is not an export batching knob.
const PACKET_LIGHT_NEIGHBOUR_OFFSETS: [(i32, i32); 8] = [
    (-1, -1),
    (0, -1),
    (1, -1),
    (-1, 0),
    (1, 0),
    (-1, 1),
    (0, 1),
    (1, 1),
];

/// The admission trace is deliberately a small diagnostic window. It is
/// opt-in because hashing every light nibble is useful for an oracle run but
/// would be needless work in the ordinary parity gate.
const MAX_LIGHT_TRACE_ADMISSIONS: usize = 64;
const LIGHT_TRACE_ADMISSIONS_ENV: &str = "LODESTONE_LARGE_PARITY_LIGHT_TRACE_ADMISSIONS";
const LIGHT_TRACE_OUT_ENV: &str = "LODESTONE_LARGE_PARITY_LIGHT_TRACE_OUT";

struct NetherLightTrace {
    next_admission: usize,
    limit: usize,
    output: Option<PathBuf>,
    lines: Vec<String>,
}

impl NetherLightTrace {
    fn from_env() -> Option<Self> {
        let raw_limit = std::env::var(LIGHT_TRACE_ADMISSIONS_ENV).ok()?;
        let limit = raw_limit.parse::<usize>().unwrap_or_else(|error| {
            panic!(
                "invalid {LIGHT_TRACE_ADMISSIONS_ENV} value {raw_limit:?}: {error}"
            )
        });
        if limit == 0 {
            return None;
        }
        assert!(
            limit <= MAX_LIGHT_TRACE_ADMISSIONS,
            "{LIGHT_TRACE_ADMISSIONS_ENV} must be at most {MAX_LIGHT_TRACE_ADMISSIONS}, got {limit}"
        );
        Some(Self {
            next_admission: 0,
            limit,
            output: std::env::var_os(LIGHT_TRACE_OUT_ENV).map(PathBuf::from),
            lines: Vec::new(),
        })
    }

    fn begin_admission(&mut self) -> Option<usize> {
        if self.next_admission >= self.limit {
            return None;
        }
        let index = self.next_admission;
        self.next_admission += 1;
        Some(index)
    }

    fn record_source_phase(
        &mut self,
        admission: usize,
        phase: &str,
        center: ChunkPos,
        source: &dyn ChunkSource,
    ) {
        self.lines.push(format!(
            "admission={admission} phase={phase} center_x={} center_z={}",
            center.0, center.1
        ));
        for (dx, dz) in trace_offsets() {
            let cx = center.0 + dx;
            let cz = center.1 + dz;
            match source.resident_column(cx, cz) {
                Some(column) => self.record_column(
                    admission,
                    phase,
                    (dx, dz),
                    (cx, cz),
                    column.retained_light_status(),
                    column.retained_light(),
                ),
                None => self.lines.push(format!(
                    "admission={} phase={} slot_dx={} slot_dz={} coordinate_x={} coordinate_z={} status=absent light=absent",
                    admission, phase, dx, dz, cx, cz
                )),
            }
        }
    }

    fn record_returned_settlement(
        &mut self,
        admission: usize,
        center: ChunkPos,
        settlement: &lodestone_server::ColumnLightSettlement,
    ) {
        let phase = "returned_settlement";
        self.lines.push(format!(
            "admission={admission} phase={phase} center_x={} center_z={}",
            center.0, center.1
        ));
        self.record_light(
            admission,
            phase,
            (0, 0),
            center,
            "centre_returned",
            settlement.centre_light(),
        );
        for ((dx, dz), light) in settlement.dependency_lights() {
            self.record_light(
                admission,
                phase,
                (dx, dz),
                (center.0 + dx, center.1 + dz),
                "dependency_returned",
                light,
            );
        }
    }

    fn record_column(
        &mut self,
        admission: usize,
        phase: &str,
        offset: (i32, i32),
        coordinate: ChunkPos,
        status: Option<RetainedLightStatus>,
        light: Option<&lodestone_world::ColumnLight>,
    ) {
        let status = status.map(retained_light_status_name).unwrap_or("none");
        match light {
            Some(light) => self.record_light(
                admission, phase, offset, coordinate, status, light,
            ),
            None => self.lines.push(format!(
                "admission={} phase={} slot_dx={} slot_dz={} coordinate_x={} coordinate_z={} status={} light=absent",
                admission,
                phase,
                offset.0,
                offset.1,
                coordinate.0,
                coordinate.1,
                status,
            )),
        }
    }

    fn record_light(
        &mut self,
        admission: usize,
        phase: &str,
        offset: (i32, i32),
        coordinate: ChunkPos,
        status: &str,
        light: &lodestone_world::ColumnLight,
    ) {
        self.lines.push(format!(
            "admission={} phase={} slot_dx={} slot_dz={} coordinate_x={} coordinate_z={} status={} light=present sections={}",
            admission,
            phase,
            offset.0,
            offset.1,
            coordinate.0,
            coordinate.1,
            status,
            light.light_section_count(),
        ));
        let storage = light.storage();
        for section in 0..light.light_section_count() {
            let sky = light_layer_summary(light.sky(section));
            let block = light_layer_summary(light.block(section));
            let (allocated, light_only, block_data) = storage.map_or(
                (None, None, None),
                |storage| (
                    Some(storage.is_allocated(section)),
                    Some(storage.is_light_only(section)),
                    Some(storage.has_block_data(section)),
                ),
            );
            self.lines.push(format!(
                "admission={} phase={} slot_dx={} slot_dz={} section_y={} sky_state={} sky_sha256={} sky_nonzero={} sky_max={} block_state={} block_sha256={} block_nonzero={} block_max={} storage_allocated={:?} storage_light_only={:?} storage_block_data={:?}",
                admission,
                phase,
                offset.0,
                offset.1,
                section as isize - 1,
                sky.0,
                sky.1,
                sky.2,
                sky.3,
                block.0,
                block.1,
                block.2,
                block.3,
                allocated,
                light_only,
                block_data,
            ));
        }
    }

    fn finish(self) {
        if self.lines.is_empty() {
            return;
        }
        let mut output = self.lines.join("\n");
        output.push('\n');
        if let Some(path) = self.output {
            std::fs::write(&path, output).unwrap_or_else(|error| {
                panic!("write Nether light trace {}: {error}", path.display())
            });
        } else {
            eprint!("{output}");
        }
    }
}

fn trace_offsets() -> impl Iterator<Item = (i32, i32)> {
    std::iter::once((0, 0)).chain(PACKET_LIGHT_NEIGHBOUR_OFFSETS.iter().copied())
}

fn retained_light_status_name(status: RetainedLightStatus) -> &'static str {
    match status {
        RetainedLightStatus::DependencyInitialized => "dependency_initialized",
        RetainedLightStatus::CentreSettled => "centre_settled",
    }
}



fn light_layer_summary(data: &lodestone_world::LightData) -> (&'static str, String, usize, u8) {
    match data {
        lodestone_world::LightData::Missing => {
            ("missing", "none".to_owned(), 0, 0)
        }
        lodestone_world::LightData::Uniform(value) => {
            let value = value & 0x0f;
            let byte = value | (value << 4);
            let bytes = [byte; 2048];
            (
                "uniform",
                hex(&support::large_parity_manifest::sha256(&bytes)),
                if value == 0 { 0 } else { 4096 },
                value,
            )
        }
        lodestone_world::LightData::Values(values) => {
            let mut nonzero = 0;
            let mut max = 0;
            for index in 0..lodestone_world::NibbleArray::LEN {
                let value = values.get(index);
                nonzero += usize::from(value != 0);
                max = max.max(value);
            }
            (
                "values",
                hex(&support::large_parity_manifest::sha256(values.as_bytes())),
                nonzero,
                max,
            )
        }
    }
}

/// Builds the production persistent source used by the Nether raw-packet
/// comparator. The bounded cache is still the serving layer, while the region
/// source below it owns settled light after an admission is evicted. This is
/// deliberately a fresh directory per process: the comparator must never
/// consume a stale world or make the checked-in generator depend on one.
#[cfg(not(target_arch = "wasm32"))]
fn nether_parity_source(seed: i64, root: &Path) -> Box<dyn ChunkSource> {
    let persistent = RegionChunkSource::new(
        nether_chunk_source(seed),
        root,
        ServerDimension::Nether,
        ServerDimension::Nether.min_y(),
        ServerDimension::Nether.height(),
    )
    .unwrap_or_else(|error| panic!("open Nether parity source at {}: {error}", root.display()));
    Box::new(retained_chunk_source_for_view_radius(persistent, 8))
}

#[cfg(target_arch = "wasm32")]
fn nether_parity_source(seed: i64, _root: &Path) -> Box<dyn ChunkSource> {
    Box::new(retained_chunk_source_for_view_radius(nether_chunk_source(seed), 8))
}

/// Serially admits one Nether centre through the production source lifecycle.
///
/// The source owns both generated terrain and retained light. Each admission
/// captures the complete 3×3 footprint, calls the version adapter's plural
/// initial-light hook, and commits the centre plus dependency snapshots through
/// one `ChunkSource` transaction. This wrapper only keeps the admission order;
/// it deliberately has no parallel terrain or light map of its own.
struct NetherSerialLightStore<'a> {
    source: &'a dyn ChunkSource,
    trace: Option<NetherLightTrace>,
}

impl<'a> NetherSerialLightStore<'a> {
    fn new(source: &'a dyn ChunkSource, trace: Option<NetherLightTrace>) -> Self {
        Self { source, trace }
    }

    fn admit(&mut self, center: ChunkPos) {
        let source = self.source;
        let trace_admission = self
            .trace
            .as_mut()
            .and_then(NetherLightTrace::begin_admission);
        if let Some(admission) = trace_admission {
            self.trace
                .as_mut()
                .expect("trace admission was allocated")
                .record_source_phase(admission, "pre_callback", center, source);
        }
        let fallback = source
            .resident_column(center.0, center.1)
            .unwrap_or_else(|| source.column(center.0, center.1));
        let trace = &mut self.trace;
        let mut compute = |column: &ChunkColumn,
                           neighbours: &[(i32, i32, ChunkColumn)]| {
            let settlement = V770ServerProtocol
                .compute_initial_column_lights_with_neighbours_in_dimension(
                    column,
                    neighbours,
                    ServerDimension::Nether,
                );
            if let (Some(admission), Some(trace), Some(settlement)) =
                (trace_admission, trace.as_mut(), settlement.as_ref())
            {
                trace.record_returned_settlement(admission, center, settlement);
            }
            settlement
        };
        source
            .settle_resident_column_lights_with_neighbours(
                center.0,
                center.1,
                &fallback,
                &PACKET_LIGHT_NEIGHBOUR_OFFSETS,
                false,
                false,
                true,
                &mut compute,
            )
            .unwrap_or_else(|error| {
                panic!("production Nether light admission failed at {center:?}: {error:?}")
            });
        if let Some(admission) = trace_admission {
            trace
                .as_mut()
                .expect("trace admission was allocated")
                .record_source_phase(admission, "committed", center, source);
        }
    }

    fn materialize_until(&mut self, order: &[ChunkPos], targets: &BTreeSet<ChunkPos>) {
        for &center in order {
            self.admit(center);
            if targets.iter().all(|target| self.is_centre_settled(*target)) {
                return;
            }
        }
        assert!(
            targets.iter().all(|target| self.is_centre_settled(*target)),
            "serial materialization order ended before every requested target was retained",
        );
    }

    fn finish_trace(&mut self) {
        if let Some(trace) = self.trace.take() {
            trace.finish();
        }
    }

    fn is_centre_settled(&self, target: ChunkPos) -> bool {
        self.source
            .resident_column(target.0, target.1)
            .is_some_and(|column| {
                column.retained_light_status() == Some(RetainedLightStatus::CentreSettled)
            })
    }

    fn packet_inputs(
        &self,
        target: ChunkPos,
    ) -> (ChunkColumn, Vec<(i32, i32, ChunkColumn)>) {
        let column = self
            .source
            .resident_column(target.0, target.1)
            .unwrap_or_else(|| self.source.column(target.0, target.1));
        assert!(
            column.retained_light_status() == Some(RetainedLightStatus::CentreSettled),
            "target {target:?} must have a settled production light snapshot",
        );
        let neighbours = PACKET_LIGHT_NEIGHBOUR_OFFSETS
            .iter()
            .map(|&(dx, dz)| {
                let column = self
                    .source
                    .resident_column(target.0 + dx, target.1 + dz)
                    .unwrap_or_else(|| self.source.column(target.0 + dx, target.1 + dz));
                (dx, dz, column)
            })
            .collect();
        (column, neighbours)
    }
}

fn nether_materialization_order(
    header: &support::large_parity_manifest::Header,
) -> Vec<ChunkPos> {
    // The target rectangle is the admission sequence. Its one-chunk halo is
    // generated as dependency terrain by each centre admission, but halo
    // columns are not themselves admitted as centres before the first packet.
    ChunkRequest::new(header.cx0, header.cx1, header.cz0, header.cz1, 0)
        .admission_order()
}

/// Replays the serial materializer order used to seal the raw-packet world.
///
/// The frozen-world export order is x-fastest/z, but the light-engine state was
/// produced tile-z/tile-x/z/x. Keeping those orders separate is essential: an
/// export prefix is not an admission prefix.
fn end_materialization_positions(h: &ManifestHeader) -> Vec<ChunkPos> {
    ChunkRequest::new(h.cx0, h.cx1, h.cz0, h.cz1, 1)
        .admission_order()
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
    start: u64,
    limit: u64,
    scan_all: bool,
    reference_packets: &BTreeMap<ChunkPos, PathBuf>,
    batch_size: usize,
) -> Vec<RawPacketMismatch> {
    let mut expected = BufReader::new(
        File::open(manifest).unwrap_or_else(|error| panic!("open persisted-world manifest {}: {error}", manifest.display())),
    );
    expected
        .seek(SeekFrom::Start(
            HEADER_BYTES as u64 + start * u64::try_from(RAW_PACKET_HASH_BYTES).expect("manifest record width fits u64"),
        ))
        .unwrap_or_else(|error| panic!("seek persisted-world manifest payload: {error}"));
    let audit_path = raw_packet_audit_path(manifest);
    let mut expected_audit = BufReader::new(
        File::open(&audit_path)
            .unwrap_or_else(|error| panic!("open persisted-world packet audit {}: {error}", audit_path.display())),
    );
    expected_audit
        .seek(SeekFrom::Start(
            HEADER_BYTES as u64 + start * u64::try_from(PACKET_AUDIT_RECORD_BYTES).expect("manifest audit width fits u64"),
        ))
        .unwrap_or_else(|error| panic!("seek persisted-world packet audit payload: {error}"));

    let mut mismatches = Vec::new();
    let mut reference_mismatches = Vec::new();
    let mut retained_target_lights = 0usize;
    let total = usize::try_from(limit - start).expect("persisted export batch fits usize");
    for (batch_start, batch_end) in persisted_batch_ranges(total, batch_size) {
        let mut records = Vec::with_capacity(batch_end - batch_start);
        for relative_index in batch_start..batch_end {
            let index = relative_index + usize::try_from(start).expect("persisted export start fits usize");
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
                if mismatches.is_empty() {
                    if let Some(path) = std::env::var_os(PERSISTED_PACKET_OUT_ENV) {
                        std::fs::write(&path, &payload).unwrap_or_else(|error| {
                            panic!("write persisted-world packet capture {}: {error}", Path::new(&path).display())
                        });
                        eprintln!(
                            "large persisted-world import/encoder parity: wrote first mismatch packet to {}",
                            Path::new(&path).display(),
                        );
                    }
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

fn trace_column_from_env() -> Option<ChunkPos> {
    let Some(raw) = std::env::var_os(TRACE_COLUMN_ENV) else {
        return None;
    };
    let raw = raw.to_string_lossy();
    let (cx, cz) = raw
        .split_once(',')
        .unwrap_or_else(|| panic!("{TRACE_COLUMN_ENV} must be formatted as cx,cz, got {raw:?}"));
    let cx = cx
        .trim()
        .parse::<i32>()
        .unwrap_or_else(|error| panic!("invalid trace x coordinate {cx:?}: {error}"));
    let cz = cz
        .trim()
        .parse::<i32>()
        .unwrap_or_else(|error| panic!("invalid trace z coordinate {cz:?}: {error}"));
    Some((cx, cz))
}

fn non_air_sections(column: &ChunkColumn) -> Vec<usize> {
    let air_id = lodestone_data::block_states::air_state_id();
    (0..column.section_count())
        .filter(|&section| {
            let base_y = column.min_y + (section * 16) as i32;
            (0..16).any(|y| {
                (0..16).any(|z| {
                    (0..16).any(|x| column.block_state_id(x, base_y + y, z) != air_id)
                })
            })
        })
        .collect()
}

fn light_mask(light: Option<&ColumnLight>, sky: bool, empty: bool) -> Vec<usize> {
    let Some(light) = light else {
        return Vec::new();
    };
    (0..light.light_section_count())
        .filter(|&section| {
            let data = if sky {
                light.sky(section)
            } else {
                light.block(section)
            };
            if empty {
                matches!(data, LightData::Uniform(0))
            } else {
                !matches!(data, LightData::Missing)
            }
        })
        .collect()
}

fn retained_trace(
    light: Option<&ColumnLight>,
    status: Option<lodestone_server::RetainedLightStatus>,
) -> String {
    format!(
        "retained={} status={status:?} sky_present={:?} sky_empty={:?} block_present={:?} block_empty={:?}",
        light.is_some(),
        light_mask(light, true, false),
        light_mask(light, true, true),
        light_mask(light, false, false),
        light_mask(light, false, true),
    )
}

struct EndTrace {
    target: ChunkPos,
    output: Option<PathBuf>,
    lines: Vec<String>,
    first_creation_recorded: bool,
}

impl EndTrace {
    fn from_env() -> Option<Self> {
        trace_column_from_env().map(|target| Self {
            target,
            output: std::env::var_os(TRACE_OUT_ENV).map(PathBuf::from),
            lines: vec![format!(
                "trace_schema=lodestone-end-generated-admission-v1 target={target:?}"
            )],
            first_creation_recorded: false,
        })
    }

    fn includes(&self, cx: i32, cz: i32) -> bool {
        (self.target.0 - cx).abs() <= 1 && (self.target.1 - cz).abs() <= 1
    }

    fn record(
        &mut self,
        admission_index: usize,
        centre: ChunkPos,
        pre: &ChunkColumn,
        post: &ChunkColumn,
        footprint: &[(ChunkPos, Vec<usize>)],
    ) {
        let slot = (self.target.0 - centre.0, self.target.1 - centre.1);
        let pre_light = pre.retained_light();
        let post_light = post.retained_light();
        self.lines.push(format!(
            "admission_index={admission_index} centre={centre:?} target={:?} target_slot={slot:?} pre_{} post_{} target_non_air_sections={:?} footprint_non_air_sections={footprint:?}",
            self.target,
            retained_trace(pre_light, pre.retained_light_status()),
            retained_trace(post_light, post.retained_light_status()),
            non_air_sections(post),
        ));
        if !self.first_creation_recorded && pre_light.is_none() && post_light.is_some() {
            self.first_creation_recorded = true;
            self.lines.push(format!(
                "first_target_snapshot_transition=admission_index:{admission_index} centre:{centre:?} target:{:?} target_slot:{slot:?} pre_{} post_{}",
                self.target,
                retained_trace(pre_light, pre.retained_light_status()),
                retained_trace(post_light, post.retained_light_status()),
            ));
        }
    }

    fn finish(self) {
        let contents = format!("{}\n", self.lines.join("\n"));
        if let Some(path) = self.output {
            std::fs::write(&path, contents)
                .unwrap_or_else(|error| panic!("write End admission trace {}: {error}", path.display()));
        } else {
            eprintln!("{contents}");
        }
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
    start: u64,
    limit: u64,
    scan_all: bool,
    reference_packets: &BTreeMap<ChunkPos, PathBuf>,
    raw_packet_output: &mut Option<RawPacketOutput>,
    component_reports: &mut Vec<(ChunkPos, PacketComponentReport)>,
    profile: &mut PhaseProfile,
) -> Vec<RawPacketMismatch> {
    let width = u64::try_from(i64::from(h.cx1) - i64::from(h.cx0) + 1)
        .expect("authenticated manifest coordinate width fits u64");
    let mut records = Vec::with_capacity(usize::try_from(limit - start).expect("parity batch fits memory"));
    profile.call("manifest_io", None, || {
        for index in start..limit {
            let mut prefix = [0u8; RAW_PACKET_HASH_BYTES];
            expected.read_exact(&mut prefix).expect("manifest raw packet hash prefix");
            let mut full = [0u8; PACKET_AUDIT_RECORD_BYTES];
            expected_audit.read_exact(&mut full).expect("packet-audit full packet digest");
            let cx = h.cx0 + (index % width) as i32;
            let cz = h.cz0 + (index / width) as i32;
            records.push(((cx, cz), prefix, full, index));
        }
    });

    let source = RegionChunkSource::new(
        end_chunk_source(42),
        world_dir,
        server_dimension,
        0,
        256,
    )
    .unwrap_or_else(|error| panic!("open generated End persistence source {}: {error}", world_dir.display()));
    let admissions = end_materialization_positions(h);
    let source_stats = source.save_handle();
    let content_only = std::env::var_os(CONTENT_ONLY_ENV).is_some();
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
    let mut trace = EndTrace::from_env();
    for (admission_index, &(cx, cz)) in admissions.iter().take(admission_limit).enumerate() {
        let column = profile.source_column(
            "materialization",
            &source,
            source_stats.stats(),
            (cx, cz),
        );
        let mut trace_pre = None;
        let mut trace_footprint = Vec::new();
        if trace.as_ref().is_some_and(|trace| trace.includes(cx, cz)) {
            let target = trace.as_ref().expect("trace configuration").target;
            trace_pre = Some(profile.source_column(
                "trace_source_reads",
                &source,
                source_stats.stats(),
                target,
            ));
            for dz in -1..=1 {
                for dx in -1..=1 {
                    let position = (cx + dx, cz + dz);
                    trace_footprint.push((
                        position,
                        non_air_sections(&profile.source_column(
                            "trace_source_reads",
                            &source,
                            source_stats.stats(),
                            position,
                        )),
                    ));
                }
            }
        }
        if !content_only {
            let _ = profile.call_with_source_stats(
                "light_settlement_and_packet_encode",
                Some((cx, cz)),
                source_stats.stats(),
                || {
                    encode_end_packet_with_source(
                        &source,
                        cx,
                        cz,
                        &column,
                        "generated materialization",
                    )
                },
            );
            if let Some(pre) = trace_pre.as_ref() {
                let target = trace.as_ref().expect("trace configuration").target;
                let post = profile.source_column(
                    "trace_source_reads",
                    &source,
                    source_stats.stats(),
                    target,
                );
                trace
                    .as_mut()
                    .expect("trace configuration")
                    .record(admission_index, (cx, cz), pre, &post, &trace_footprint);
            }
        }
        if (admission_index + 1) % 256 == 0 || admission_index + 1 == admission_limit {
            eprintln!(
                "large generated save/reopen parity: materialized {}/{} End admissions",
                admission_index + 1,
                admissions.len(),
            );
        }
    }
    if let Some(trace) = trace.take() {
        trace.finish();
    }
    if content_only {
        eprintln!(
            "large generated content-only profile: materialized {admission_limit} End admissions; skipped light settlement, packet encode, save, reopen, and packet comparison",
        );
        return Vec::new();
    }

    let save_handle: lodestone_server::region_source::WorldSaveHandle = source.save_handle();
    let written = profile.call("save", None, || {
        save_handle
            .save()
            .unwrap_or_else(|error| panic!("save generated End materialization through WorldSaveHandle: {error}"))
    });
    assert!(written > 0, "generated End materialization must write its settled centres");
    drop(save_handle);
    drop(source);

    let reopened = profile.call("reopen", None, || {
        RegionChunkSource::new(
            end_chunk_source(42),
            world_dir,
            server_dimension,
            0,
            256,
        )
        .unwrap_or_else(|error| panic!("reopen generated End persistence source {}: {error}", world_dir.display()))
    });
    let reopened_stats = reopened.save_handle();

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
            let _ = profile.source_column(
                "batch_halo_load",
                &reopened,
                reopened_stats.stats(),
                (cx, cz),
            );
        }
        for &((cx, cz), expected_prefix, expected_full, index) in batch {
            let column = profile.source_column(
                "batch_target_load",
                &reopened,
                reopened_stats.stats(),
                (cx, cz),
            );
            let payload = profile.call_with_source_stats(
                "light_settlement_and_packet_encode",
                Some((cx, cz)),
                reopened_stats.stats(),
                || {
                    encode_end_packet_with_source(
                        &reopened,
                        cx,
                        cz,
                        &column,
                        "generated save/reopen capture",
                    )
                },
            );
            if let Some(output) = raw_packet_output.as_mut() {
                output.write((cx, cz), &payload);
            }
            let actual_full = profile.call("packet_hash", Some((cx, cz)), || raw_packet_full_digest(&payload));
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
                let reference_packet = profile.call("reference_packet_io", Some((cx, cz)), || {
                    std::fs::read(reference_path)
                        .unwrap_or_else(|error| panic!("read {}: {error}", reference_path.display()))
                });
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
    let reopened_save: lodestone_server::region_source::WorldSaveHandle = reopened.save_handle();
    let generated_delta = reopened_save
        .stats()
        .generated
        .load(std::sync::atomic::Ordering::Relaxed);
    let loaded_from_disk = reopened_save
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
    let reopened_save: lodestone_server::region_source::WorldSaveHandle = reopened.save_handle();
    let generated_delta = reopened_save
        .stats()
        .generated
        .load(std::sync::atomic::Ordering::Relaxed);
    let loaded_from_disk = reopened_save
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
fn initial_light_admission_excludes_future_neighbours() {
    assert_eq!(
        initial_admission_neighbour_offsets(),
        &INITIAL_ADMISSION_NEIGHBOUR_OFFSETS,
    );

    let mut centre = ChunkColumn::new(0, 256);
    for z in 0..16 {
        for x in 0..16 {
            centre.set_block(x, 0, z, "minecraft:end_stone");
        }
    }
    let empty = ChunkColumn::new(0, 256);
    let mut east = ChunkColumn::new(0, 256);
    for z in 0..16 {
        for x in 0..16 {
            east.set_block(x, 0, z, "minecraft:end_stone");
        }
    }
    let all_neighbours = (-1..=1)
        .flat_map(|dz| (-1..=1).map(move |dx| (dx, dz)))
        .filter(|&(dx, dz)| (dx, dz) != (0, 0))
        .map(|(dx, dz)| {
            let column = if (dx, dz) == (1, 0) {
                east.clone()
            } else {
                empty.clone()
            };
            (dx, dz, column)
        })
        .collect::<Vec<_>>();
    let admitted_offsets = initial_admission_neighbour_offsets();
    let admitted_neighbours = all_neighbours
        .iter()
        .filter(|(dx, dz, _)| admitted_offsets.contains(&(*dx, *dz)))
        .map(|&(dx, dz, ref column)| (dx, dz, column.clone()))
        .collect::<Vec<_>>();
    let proto = V770ServerProtocol;
    let settled = initial_light_snapshot_for_admission(
        &proto,
        &centre,
        &admitted_neighbours,
        ServerDimension::End,
    );
    let replayed = proto
        .try_encode_chunk_with_neighbours_in_dimension(
            0,
            0,
            &settled,
            &all_neighbours,
            ServerDimension::End,
        )
        .expect("the synthetic End packet must encode");
    let admitted_fallback = proto
        .try_encode_chunk_with_neighbours_in_dimension(
            0,
            0,
            &centre,
            &admitted_neighbours,
            ServerDimension::End,
        )
        .expect("the admitted-footprint End packet must encode");
    let full_fallback = proto
        .try_encode_chunk_with_neighbours_in_dimension(
            0,
            0,
            &centre,
            &all_neighbours,
            ServerDimension::End,
        )
        .expect("the full-footprint End packet must encode");
    assert_eq!(replayed, admitted_fallback);
    assert_ne!(replayed, full_fallback);
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
fn packet_light_admission_uses_a_serial_3x3_footprint() {
    assert_eq!(PACKET_LIGHT_NEIGHBOUR_OFFSETS.len(), 8);
}

#[test]
fn nether_materialization_order_keeps_halo_as_dependency_terrain() {
    let header = support::large_parity_manifest::Header {
        semantic_version: 6,
        cx0: -25,
        cx1: 25,
        cz0: -25,
        cz1: 25,
        count: 2_601,
        frozen_world: [0; 32],
        dimension: Dimension::Nether,
        record_width: RAW_PACKET_HASH_BYTES as u16,
        kind: 2,
    };
    let order = nether_materialization_order(&header);
    assert_eq!(order.len(), 51 * 51);
    assert_eq!(
        &order[..18],
        &[
            (-25, -25),
            (-24, -25),
            (-23, -25),
            (-22, -25),
            (-21, -25),
            (-20, -25),
            (-19, -25),
            (-18, -25),
            (-17, -25),
            (-16, -25),
            (-15, -25),
            (-14, -25),
            (-13, -25),
            (-12, -25),
            (-11, -25),
            (-10, -25),
            (-25, -24),
            (-24, -24),
        ]
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
fn parity_batch_range_selects_a_contiguous_window_and_rejects_overrun() {
    assert_eq!(
        parity_batch_range(2_601, Some(256), None, None, Some(512), false),
        Ok((512, 768)),
    );
    assert_eq!(
        parity_batch_range(2_601, Some(2_000), Some(2_000), None, Some(601), true),
        Ok((601, 2_601)),
    );
    assert!(parity_batch_range(2_601, None, None, None, Some(1), false).is_err());
    assert!(parity_batch_range(2_601, Some(2_000), None, None, Some(1_000), false).is_err());
    assert!(parity_batch_range(2_601, Some(1), None, Some(0), Some(1), false).is_err());
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

/// Cross-language control for P07. Both sides serialize the generated column
/// directly; neither constructs a light-bearing packet or settles light.
#[test]
#[ignore = "requires a one-chunk Java v7 light-free export; see docs/worldgen-large-parity.md"]
fn java_and_rust_light_free_records_agree() {
    const EXTERNAL_UPPER_SPILL: &str = include_str!("fixtures/nether_p07_upper_spill_external.txt");
    assert!(EXTERNAL_UPPER_SPILL.contains("target_chunk=-14,-25"));
    assert!(EXTERNAL_UPPER_SPILL.contains("source_chunk=-14,-26"));
    assert!(EXTERNAL_UPPER_SPILL.contains("world_position=-216,128,-400"));
    assert!(EXTERNAL_UPPER_SPILL.contains("state=minecraft:brown_mushroom"));
    assert!(EXTERNAL_UPPER_SPILL.contains("heightmap_id=1"));
    assert!(EXTERNAL_UPPER_SPILL.contains("heightmap_local=8,0"));
    assert!(EXTERNAL_UPPER_SPILL.contains("heightmap_first_available=129"));
    let record_path = std::env::var("LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_RECORD")
        .expect("set LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_RECORD to Java's light-free record");
    let manifest_path = std::env::var("LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_MANIFEST")
        .expect("set LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_MANIFEST to Java's one-chunk v7 manifest");
    let audit_path = std::env::var("LODESTONE_LARGE_PARITY_CROSS_LANGUAGE_LIGHT_FREE_AUDIT")
        .unwrap_or_else(|_| format!("{manifest_path}.light-free-audit"));
    let java_record = std::fs::read(&record_path).expect("read Java light-free record");
    let manifest_bytes = std::fs::read(&manifest_path).expect("read Java v7 manifest");
    assert!(manifest_bytes.len() >= HEADER_BYTES + RAW_PACKET_HASH_BYTES);
    let mut raw_header = [0u8; HEADER_BYTES]; raw_header.copy_from_slice(&manifest_bytes[..HEADER_BYTES]);
    let header = read_header(&raw_header[..]).expect("validate Java v7 manifest header");
    assert_eq!(header.semantic_version, 7, "light-free control requires P07");
    assert_eq!(header.count, 1, "light-free control must use exactly one chunk");
    let expected_prefix: [u8; RAW_PACKET_HASH_BYTES] = manifest_bytes[HEADER_BYTES..HEADER_BYTES + RAW_PACKET_HASH_BYTES].try_into().unwrap();
    let audit_bytes = std::fs::read(&audit_path).expect("read Java v7 light-free audit sidecar");
    assert!(audit_bytes.len() >= HEADER_BYTES + LIGHT_FREE_AUDIT_RECORD_BYTES);
    let mut audit_raw_header = [0u8; HEADER_BYTES]; audit_raw_header.copy_from_slice(&audit_bytes[..HEADER_BYTES]);
    let audit = read_light_free_audit_header(&audit_raw_header[..]).expect("validate Java v7 light-free audit header");
    validate_light_free_audit_header(&header, &audit).expect("sidecar identity must match manifest");
    let expected_full: [u8; LIGHT_FREE_AUDIT_RECORD_BYTES] = audit_bytes[HEADER_BYTES..HEADER_BYTES + LIGHT_FREE_AUDIT_RECORD_BYTES].try_into().unwrap();
    verify_light_free_audit_pair(
        Cursor::new(expected_prefix),
        Cursor::new(expected_full),
        1,
        payload_digest_from_header(&raw_header),
        audit.payload_digest,
    ).expect("Java manifest and light-free sidecar must authenticate together");
    assert_eq!(sha256(&java_record), expected_full, "Java sidecar must authenticate Java light-free bytes");

    let source: Box<dyn ChunkSource> = match header.dimension {
        Dimension::Overworld => Box::new(overworld_chunk_source(42)),
        Dimension::Nether => Box::new(nether_chunk_source(42)),
        Dimension::End => Box::new(end_chunk_source(42)),
    };
    let column = source.column(header.cx0, header.cz0);
    if header.dimension == Dimension::Nether && header.cx0 == 50 && header.cz0 == -50 {
        eprintln!("P07_TARGET_STATE state={} id={}", column.block_state(14, 7, 1), column.block_state_id(14, 7, 1));
    }
    let rust_record = light_free_record(&column, header.cx0, header.cz0, header.dimension);
    if let Some(path) = std::env::var_os("LODESTONE_LARGE_PARITY_RUST_RECORD_OUT") {
        std::fs::write(&path, &rust_record)
            .unwrap_or_else(|error| panic!("write Rust light-free record {}: {error}", Path::new(&path).display()));
    }
    if rust_record != java_record {
        let first = rust_record.iter().zip(&java_record).position(|(left, right)| left != right).unwrap_or(rust_record.len().min(java_record.len()));
        panic!("light-free content bytes differ at offset {first}: Java length {}, Rust length {}, Java byte {:?}, Rust byte {:?}", java_record.len(), rust_record.len(), java_record.get(first), rust_record.get(first));
    }
    assert_eq!(sha256(&rust_record), expected_full, "Rust light-free digest must match Java sidecar");
    assert_eq!([expected_full[0], expected_full[1]], expected_prefix, "Rust light-free prefix must match Java manifest");
}

/// Replays one P07 target through the serial tiled lifecycle that produced the
/// frozen world. This isolates mutable neighbour writes from light and packet
/// encoding while keeping the replay bounded to the selected target's causal
/// dependency closure.
#[test]
#[ignore = "requires an authenticated P07 manifest; see docs/worldgen-large-parity.md"]
fn light_free_target_matches_serial_lifecycle() {
    let path = std::env::var("LODESTONE_LARGE_PARITY_MANIFEST")
        .expect("set LODESTONE_LARGE_PARITY_MANIFEST to an authenticated P07 manifest");
    let target_index = optional_usize_env("LODESTONE_LARGE_PARITY_TARGET_INDEX")
        .expect("valid P07 target index")
        .expect("set LODESTONE_LARGE_PARITY_TARGET_INDEX to one zero-based target index");
    let mut raw_header = [0; HEADER_BYTES];
    let mut manifest = File::open(&path).expect("open P07 manifest");
    manifest.read_exact(&mut raw_header).expect("read P07 header");
    let header = read_header(&raw_header[..]).expect("valid P07 header");
    assert_eq!(header.semantic_version, 7, "target lifecycle control requires P07");
    assert!(target_index < header.count as usize, "P07 target index is out of range");
    let (audit_path, audit_header) = load_light_free_audit(Path::new(&path), &header);
    verify_light_free_audit_files(
        Path::new(&path),
        &raw_header,
        &header,
        &audit_path,
        &audit_header,
    );

    let offset = HEADER_BYTES as u64 + target_index as u64 * RAW_PACKET_HASH_BYTES as u64;
    manifest.seek(SeekFrom::Start(offset)).expect("seek P07 target prefix");
    let mut expected_prefix = [0; RAW_PACKET_HASH_BYTES];
    manifest.read_exact(&mut expected_prefix).expect("read P07 target prefix");
    let mut audit = File::open(&audit_path).expect("open P07 light-free audit");
    audit
        .seek(SeekFrom::Start(
            HEADER_BYTES as u64 + target_index as u64 * LIGHT_FREE_AUDIT_RECORD_BYTES as u64,
        ))
        .expect("seek P07 target digest");
    let mut expected_full = [0; LIGHT_FREE_AUDIT_RECORD_BYTES];
    audit.read_exact(&mut expected_full).expect("read P07 target digest");

    let width = usize::try_from(header.cx1 - header.cx0 + 1).expect("P07 width");
    let target = raw_packet_target(&header, target_index as u64, width as u64);
    let admissions = match header.dimension {
        Dimension::Nether => nether_lifecycle_halo_order(&header),
        _ => tiled_materialization_halo_order(&header),
    };
    let events = admissions
        .iter()
        .enumerate()
        .map(|(sequence, &source)| LifecycleReplayEvent {
            source,
            stage: LifecycleCompletion::Features,
            sequence: sequence as u64,
            resident_transitions: Vec::new(),
        })
        .collect::<Vec<_>>();
    let plan = LifecycleReplayPlan::for_target(target, &admissions, &events)
        .expect("P07 target must have a complete lifecycle closure");
    let column = match header.dimension {
        Dimension::Overworld => materialize_light_free_target(
            overworld_chunk_source(42),
            &plan,
            target,
        ),
        Dimension::Nether => materialize_light_free_target(
            nether_chunk_source(42),
            &plan,
            target,
        ),
        Dimension::End => end_chunk_source(42).column(target.0, target.1),
    };
    let record = light_free_record(&column, target.0, target.1, header.dimension);
    if let Some(path) = std::env::var_os("LODESTONE_LARGE_PARITY_RUST_RECORD_OUT") {
        std::fs::write(&path, &record).unwrap_or_else(|error| {
            panic!("write Rust lifecycle record {}: {error}", Path::new(&path).display())
        });
    }
    let actual_full = sha256(&record);
    assert_eq!(actual_full, expected_full, "P07 lifecycle digest differs at {target:?}");
    assert_eq!([actual_full[0], actual_full[1]], expected_prefix);
}

fn materialize_light_free_target<S: LifecycleWorldgenSource>(
    source: S,
    plan: &LifecycleReplayPlan,
    target: ChunkPos,
) -> ChunkColumn {
    let mut materializer = LifecycleMaterializer::new(source);
    materializer.prepare_lifecycle_replay(plan.admissions());
    for &admission in plan.admissions() {
        materializer.admit(admission);
    }
    for event in plan.feature_events() {
        materializer.complete_observing(
            event.source,
            event.stage,
            event.sequence,
            |_| {},
        );
    }
    materializer.snapshot_for_packet(target)
}

/// Compares every Nether target in a bounded P07 manifest after replaying the
/// complete sealed-world feature lifecycle once. This is intentionally a
/// bounded development gate: the million-column acceptance path uses the same
/// order with row eviction rather than retaining the complete grid.
#[test]
#[ignore = "requires a bounded authenticated Nether P07 manifest"]
fn bounded_nether_light_free_manifest_matches_serial_lifecycle() {
    let path = std::env::var("LODESTONE_LARGE_PARITY_MANIFEST")
        .expect("set LODESTONE_LARGE_PARITY_MANIFEST to a bounded Nether P07 manifest");
    let mut raw_header = [0; HEADER_BYTES];
    let mut manifest = File::open(&path).expect("open Nether P07 manifest");
    manifest.read_exact(&mut raw_header).expect("read Nether P07 header");
    let header = read_header(&raw_header[..]).expect("valid Nether P07 header");
    assert_eq!(header.semantic_version, 7, "bounded lifecycle control requires P07");
    assert_eq!(header.dimension, Dimension::Nether, "bounded lifecycle control requires Nether");
    assert!(header.count <= 10_000, "bounded lifecycle control refuses more than 10,000 targets");
    let (audit_path, audit_header) = load_light_free_audit(Path::new(&path), &header);
    verify_light_free_audit_files(
        Path::new(&path),
        &raw_header,
        &header,
        &audit_path,
        &audit_header,
    );

    let admissions = match header.dimension {
        Dimension::Nether => nether_lifecycle_halo_order(&header),
        _ => tiled_materialization_halo_order(&header),
    };
    let mut materializer = LifecycleMaterializer::new(nether_chunk_source(42));
    materializer.prepare_lifecycle_replay(&admissions);
    for &source in &admissions {
        materializer.admit(source);
    }
    for (sequence, &source) in admissions.iter().enumerate() {
        materializer.complete(source, LifecycleCompletion::Features, sequence as u64);
    }

    manifest.seek(SeekFrom::Start(HEADER_BYTES as u64)).expect("seek Nether P07 payload");
    let mut audit = File::open(&audit_path).expect("open Nether P07 audit");
    audit.seek(SeekFrom::Start(HEADER_BYTES as u64)).expect("seek Nether P07 audit payload");
    let width = u64::try_from(header.cx1 - header.cx0 + 1).expect("Nether P07 width");
    for index in 0..header.count {
        let mut expected_prefix = [0; RAW_PACKET_HASH_BYTES];
        manifest.read_exact(&mut expected_prefix).expect("read Nether P07 prefix");
        let mut expected_full = [0; LIGHT_FREE_AUDIT_RECORD_BYTES];
        audit.read_exact(&mut expected_full).expect("read Nether P07 digest");
        let target = raw_packet_target(&header, index, width);
        let column = materializer.snapshot_for_packet(target);
        let actual_full = sha256(&light_free_record(
            &column,
            target.0,
            target.1,
            Dimension::Nether,
        ));
        assert_eq!(actual_full, expected_full, "Nether P07 lifecycle digest differs at {target:?} after {index} matches");
        assert_eq!([actual_full[0], actual_full[1]], expected_prefix);
        if (index + 1) % 256 == 0 || index + 1 == header.count {
            eprintln!("bounded Nether P07 lifecycle: compared {}/{} chunks", index + 1, header.count);
        }
    }
}

fn tiled_materialization_halo_order(header: &ManifestHeader) -> Vec<ChunkPos> {
    tiled_materialization_halo_order_with_radius(header, 1)
}

fn tiled_materialization_halo_order_with_radius(
    header: &ManifestHeader,
    radius: i32,
) -> Vec<ChunkPos> {
    ChunkRequest::new(
        header.cx0,
        header.cx1,
        header.cz0,
        header.cz1,
        radius,
    )
    .admission_order()
}

/// The Nether source dispatcher can write into a two-chunk destination halo.
/// Admit that complete source halo in the same tile-z/tile-x/z/x order as the
/// sealed materialization, while callers still emit only authenticated target
/// records from the manifest rectangle.
fn nether_lifecycle_halo_order(header: &ManifestHeader) -> Vec<ChunkPos> {
    tiled_materialization_halo_order_with_radius(header, NETHER_FEATURE_WRITE_RADIUS)
}

#[test]
fn nether_lifecycle_halo_keeps_first_row_source_effects_outside_output() {
    let header = ManifestHeader {
        semantic_version: 7,
        cx0: 0,
        cx1: 1,
        cz0: 0,
        cz1: 1,
        count: 4,
        frozen_world: [1; 32],
        dimension: Dimension::Nether,
        record_width: RAW_PACKET_HASH_BYTES as u16,
        kind: 2,
    };
    let admissions = nether_lifecycle_halo_order(&header);
    let source = (0, -2);
    let target = (0, 0);
    assert!(
        admissions.contains(&source),
        "the first target row needs the complete two-chunk feature source halo",
    );
    assert!(
        admissions.iter().position(|&chunk| chunk == source)
            < admissions.iter().position(|&chunk| chunk == target),
        "the preceding source row must be admitted before the first requested row",
    );
    let events = admissions
        .iter()
        .enumerate()
        .map(|(sequence, &source)| LifecycleReplayEvent {
            source,
            stage: LifecycleCompletion::Features,
            sequence: sequence as u64,
            resident_transitions: Vec::new(),
        })
        .collect::<Vec<_>>();
    let plan = LifecycleReplayPlan::for_target(target, &admissions, &events)
        .expect("the synthetic two-chunk halo must form a valid target plan");
    assert!(
        plan.feature_events().iter().any(|event| event.source == source),
        "the z=-2 source effect must survive target-plan pruning",
    );
    let outputs = (0..header.count as usize)
        .map(|index| raw_packet_target(&header, index as u64, 2))
        .collect::<Vec<_>>();
    assert_eq!(outputs, vec![(0, 0), (1, 0), (0, 1), (1, 1)]);
    assert!(
        !outputs.contains(&source),
        "the admission halo is lifecycle input, never an extra emitted record",
    );
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
    let light_free = h.semantic_version == 7;
    if std::env::var_os("LODESTONE_LARGE_PARITY_REQUIRE_FULL_GRID").is_some() {
        let (grid_min, grid_max, grid_count) = if raw_packet || light_free {
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
        let (path, header) = load_raw_packet_audit(Path::new(&path), &h);
        Some((path, ManifestAuditHeader::Raw(header)))
    } else if light_free {
        let (path, header) = load_light_free_audit(Path::new(&path), &h);
        Some((path, ManifestAuditHeader::LightFree(header)))
    } else {
        None
    };
    if let Some((audit_path, audit_header)) = &audit {
        match audit_header {
            ManifestAuditHeader::Raw(header) => verify_raw_packet_audit_files(Path::new(&path), &raw_header, &h, audit_path, header),
            ManifestAuditHeader::LightFree(header) => verify_light_free_audit_files(Path::new(&path), &raw_header, &h, audit_path, header),
        }
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
    if (raw_packet || light_free) && target_index.is_some() {
        panic!("LODESTONE_LARGE_PARITY_TARGET_INDEX is only supported for semantic lifecycle manifests");
    }
    let batch_size = optional_u64_env("LODESTONE_LARGE_PARITY_BATCH_SIZE")
        .unwrap_or_else(|error| panic!("invalid parity batch selection: {error}"));
    let start_index = optional_usize_env(START_INDEX_ENV)
        .unwrap_or_else(|error| panic!("invalid parity batch selection: {error}"));
    let (start, limit) = parity_batch_range(
        h.count,
        max_chunks,
        batch_size,
        target_index,
        start_index,
        scan_all,
    )
    .unwrap_or_else(|error| panic!("invalid parity batch selection: {error}"));
    assert!(
        !(start > 0 && is_partial_lifecycle_manifest(&h)),
        "{START_INDEX_ENV} is only supported for full-grid manifests"
    );
    let selected_count = limit - start;
    if start > 0 {
        let payload_offset = HEADER_BYTES as u64
            + start
                * u64::try_from(if raw_packet || light_free {
                    RAW_PACKET_HASH_BYTES
                } else {
                    32
                })
                .expect("manifest record width fits u64");
        expected
            .seek(SeekFrom::Start(payload_offset))
            .expect("seek manifest payload to selected batch start");
        if let Some(audit) = expected_audit.as_mut() {
            let audit_offset = HEADER_BYTES as u64
                + start * u64::try_from(if raw_packet {
                    PACKET_AUDIT_RECORD_BYTES
                } else {
                    LIGHT_FREE_AUDIT_RECORD_BYTES
                })
                .expect("manifest audit width fits u64");
            audit
                .seek(SeekFrom::Start(audit_offset))
                .expect("seek manifest audit to selected batch start");
        }
    }
    let mut raw_packet_output = prepare_raw_packet_output(raw_packet, selected_count);
    if batch_size.is_some() {
        eprintln!(
            "large {} parity: authenticated scan-all batch selected (start={start}, {selected_count} targets)",
            if raw_packet { "raw-packet" } else if light_free { "light-free" } else { "semantic" },
        );
    }
    let reference_packets = if scan_all && !light_free {
        load_reference_packets(dimension, h.cx0, h.cx1, h.cz0, h.cz1, start, limit)
    } else {
        BTreeMap::new()
    };
    let mut phase_profile = PhaseProfile::from_env();
    if raw_packet && dimension == Dimension::End && std::env::var_os(PERSISTED_WORLD_ROOT_ENV).is_some() {
        let root = validated_persisted_world_root(&h);
        eprintln!(
            "large persisted-world import/encoder parity: opening validated End root {} (diagnostic only; generated replay remains acceptance authority)",
            root.display(),
        );
        let world = root.join("world");
        assert!(
            world.is_dir(),
            "persisted End parity root {} is missing its world directory",
            root.display(),
        );
        let persisted = RegionChunkSource::new(
            end_chunk_source(42),
            &world,
            ServerDimension::End,
            0,
            256,
        )
        .unwrap_or_else(|error| panic!("open validated persisted End root {}: {error}", root.display()));
        let persisted_save = persisted.save_handle();
        let generated_before = persisted_save
            .stats()
            .generated
            .load(std::sync::atomic::Ordering::Relaxed);
        let loaded_before = persisted_save
            .stats()
            .loaded_from_disk
            .load(std::sync::atomic::Ordering::Relaxed);
        let persisted_mismatches = compare_end_raw_from_persisted_world(
            &persisted,
            Path::new(&path),
            &h,
            start,
            limit,
            scan_all,
            &reference_packets,
            persisted_batch_size(),
        );
        let generated_after = persisted_save
            .stats()
            .generated
            .load(std::sync::atomic::Ordering::Relaxed);
        let loaded_after = persisted_save
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
        if std::env::var_os(PERSISTED_ONLY_ENV).is_some() {
            eprintln!(
                "large persisted-world import/encoder parity: {PERSISTED_ONLY_ENV}=1; stopping before generated replay",
            );
            return;
        }
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
        let end_persistence_dir = (dimension == Dimension::End).then(|| {
            let dir = std::env::temp_dir().join(format!(
                "lodestone-end-large-parity-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            dir
        });
        let nether_persistence_dir = (raw_packet && dimension == Dimension::Nether).then(|| {
            let dir = std::env::temp_dir().join(format!(
                "lodestone-nether-large-parity-{}",
                std::process::id()
            ));
            let _ = std::fs::remove_dir_all(&dir);
            dir
        });
        let source: Box<dyn ChunkSource> = if light_free {
            match dimension {
                Dimension::Overworld => Box::new(overworld_chunk_source(42)),
                Dimension::Nether => Box::new(nether_chunk_source(42)),
                Dimension::End => Box::new(end_chunk_source(42)),
            }
        } else {
            match dimension {
                // A frozen external world is a retained, settled lifecycle result.
                // Keep the comparator on the server's retained-source path rather than
                // regenerating an isolated column for every packet request.
                Dimension::Overworld => Box::new(retained_chunk_source_for_view_radius(
                    overworld_chunk_source(42),
                    8,
                )),
                Dimension::Nether if raw_packet => nether_parity_source(
                    42,
                    nether_persistence_dir
                        .as_deref()
                        .expect("Nether raw replay must have a persistence directory"),
                ),
                Dimension::Nether => Box::new(retained_chunk_source_for_view_radius(
                    nether_chunk_source(42),
                    8,
                )),
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
                    Box::new(retained_chunk_source_for_view_radius(persistent, 8))
                }
            }
        };
        let column_for = |cx, cz| -> ChunkColumn { source.column(cx, cz) };
        let mut nether_light = if raw_packet && dimension == Dimension::Nether {
            let targets = raw_packet_targets_range(&h, 0, limit)
                .into_iter()
                .collect::<BTreeSet<_>>();
            let order = nether_materialization_order(&h);
            let mut store = NetherSerialLightStore::new(&*source, NetherLightTrace::from_env());
            store.materialize_until(&order, &targets);
            store.finish_trace();
            eprintln!(
                "large raw-packet parity: settled serial Nether admissions for {} targets through the production source",
                targets.len(),
            );
            Some(store)
        } else {
            None
        };
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
                start,
                limit,
                scan_all,
                &reference_packets,
                &mut raw_packet_output,
                &mut component_reports,
                &mut phase_profile,
            ));
        } else {
            let mut expected_digest = [0u8; 32];
        let mut nether_replay_window_end = 0;
        for index in 0..limit {
            if index < start {
                continue;
            }
            if light_free {
                let mut expected_prefix = [0u8; RAW_PACKET_HASH_BYTES];
                expected.read_exact(&mut expected_prefix).expect("manifest light-free hash prefix");
                let mut expected_full = [0u8; LIGHT_FREE_AUDIT_RECORD_BYTES];
                expected_audit
                    .as_mut()
                    .expect("v7 light-free comparison has an audit reader")
                    .read_exact(&mut expected_full)
                    .expect("light-free audit full content digest");
                let (cx, cz) = raw_packet_target(&h, index, width);
                let column = column_for(cx, cz);
                let record = light_free_record(&column, cx, cz, dimension);
                let actual_full = sha256(&record);
                let actual_prefix = [actual_full[0], actual_full[1]];
                if actual_prefix != expected_prefix || actual_full != expected_full {
                    let mismatch = RawPacketMismatch {
                        target: (cx, cz),
                        index,
                        expected_prefix,
                        actual_prefix,
                        expected_full,
                        actual_full,
                        payload_bytes: record.len(),
                    };
                    if !scan_all {
                        panic!(
                            "large light-free parity mismatch at ({cx},{cz}) after {index} matching chunks: expected prefix {}, actual prefix {}, expected full SHA-256 {}, actual full SHA-256 {}, collision={}",
                            hex(&mismatch.expected_prefix),
                            hex(&mismatch.actual_prefix),
                            hex(&mismatch.expected_full),
                            hex(&mismatch.actual_full),
                            mismatch.collision(),
                        );
                    }
                    raw_mismatches.push(mismatch);
                }
                if (index + 1) % 256 == 0 || index + 1 == limit {
                    eprintln!("large light-free parity: compared {}/{} chunks (batch boundary at ({cx},{cz}))", index + 1, limit);
                }
                continue;
            }
            if raw_packet && dimension == Dimension::Nether && index >= nether_replay_window_end {
                let window_end = nether_packet_replay_window_end(index, width, limit);
                let targets = raw_packet_targets_for_window(&h, index, window_end, width);
                let capacity = source
                    .prepare_packet_replay(&targets)
                    .expect("Nether source exposes the packet replay seam");
                let bound = nether_packet_replay_capacity_bound(
                    width,
                    NETHER_PACKET_REPLAY_WINDOW_ROWS,
                );
                assert!(
                    capacity <= bound,
                    "Nether replay cache capacity {capacity} exceeds window bound {bound}"
                );
                eprintln!(
                    "large raw-packet parity: prepared Nether immutable window {}..{} ({} targets, capacity={capacity})",
                    index,
                    window_end,
                    targets.len(),
                );
                nether_replay_window_end = window_end;
            }
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
            let (settled_column, neighbours) = if let Some(store) = nether_light.as_mut() {
                store.packet_inputs((cx, cz))
            } else {
                let column = column_for(cx, cz);
                let mut neighbours = Vec::with_capacity(8);
                for dz in -1..=1 {
                    for dx in -1..=1 {
                        if (dx, dz) != (0, 0) {
                            neighbours.push((dx, dz, column_for(cx + dx, cz + dz)));
                        }
                    }
                }
                (column, neighbours)
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
            if let Some(output) = raw_packet_output.as_mut() {
                output.write((cx, cz), &payload);
            }
            if index == start {
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
            if raw_packet
                && dimension == Dimension::Nether
                && index + 1 == nether_replay_window_end
            {
                source.reset_packet_replay();
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
        }
        drop(nether_light);
        drop(source);
        if let Some(dir) = end_persistence_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
        if let Some(dir) = nether_persistence_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
    phase_profile.report();
    if start > 0 || limit < h.count {
        eprintln!(
            "large {} parity: bounded pilot completed successfully for export indices {start}..{limit} ({} chunks); full grid remains pending",
            if raw_packet { "raw-packet" } else { "semantic" },
            selected_count,
        );
    }
    let diagnostic = format_diagnostic_report(
        raw_packet,
        selected_count,
        h.count,
        &digest_mismatches,
        &raw_mismatches,
        &component_reports,
    );
    if let Some(inventory_path) = std::env::var_os(MISMATCH_INVENTORY_ENV) {
        write_mismatch_inventory(
            Path::new(&path),
            Path::new(&inventory_path),
            &h,
            &digest_mismatches,
            &raw_mismatches,
        );
    }
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
                selected_count,
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
        resident_transitions: Vec::new(),
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
    assert_eq!(baseline_source.generator().pre_decoration_computations(), 49);
    assert_eq!(baseline_source.generator().pre_decoration_evictions(), 0);

    let prepared_source = nether_chunk_source(42);
    let capacity = prepared_source.generator().prepare_packet_replay(&[target]);
    assert_eq!(capacity, 49, "one packet's 3x3 targets have a 7x7 prefix closure");
    let prepared = nether_packet_payload(&prepared_source, target);

    assert_eq!(baseline, prepared, "immutable-stage retention must not alter packet bytes");
    assert_eq!(prepared_source.generator().pre_decoration_computations(), 49);
    assert_eq!(prepared_source.generator().pre_decoration_evictions(), 0);
    assert_eq!(baseline_source.generator().pre_decoration_computations(), capacity);
    assert_eq!(baseline_source.generator().pre_decoration_evictions(), 0);
}

/// Independent seam control for the row-window implementation: packet bytes
/// and their full digests must match one-shot preparation when a row boundary
/// releases and rebuilds the immutable pre-decoration cache.
#[test]
#[ignore = "long-running raw-byte identity control; see docs/worldgen-large-parity.md"]
fn nether_packet_replay_window_seam_preserves_raw_packet_bytes_and_digests() {
    let targets = [(0, 0), (1, 0), (0, 1), (1, 1)];
    let one_shot_source = nether_chunk_source(42);
    one_shot_source.generator().prepare_packet_replay(&targets);
    let one_shot = targets
        .iter()
        .map(|&target| nether_packet_payload(&one_shot_source, target))
        .collect::<Vec<_>>();

    let windowed_source = nether_chunk_source(42);
    let first_row = [targets[0], targets[1]];
    windowed_source.generator().prepare_packet_replay(&first_row);
    let mut windowed = first_row
        .iter()
        .map(|&target| nether_packet_payload(&windowed_source, target))
        .collect::<Vec<_>>();
    windowed_source.generator().reset_packet_replay();
    let second_row = [targets[2], targets[3]];
    windowed_source.generator().prepare_packet_replay(&second_row);
    windowed.extend(
        second_row
            .iter()
            .map(|&target| nether_packet_payload(&windowed_source, target)),
    );

    assert_eq!(one_shot, windowed, "row-window preparation changed packet bytes");
    let one_shot_digests = one_shot.iter().map(|payload| raw_packet_full_digest(payload)).collect::<Vec<_>>();
    let windowed_digests = windowed.iter().map(|payload| raw_packet_full_digest(payload)).collect::<Vec<_>>();
    assert_eq!(one_shot_digests, windowed_digests, "row-window preparation changed packet digests");
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

fn compare_lifecycle_manifest<S: LifecycleWorldgenSource + Sync>(
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
            materializer.admit_many_parallel(plan.admissions());
            for event in plan.feature_events() {
                materializer.complete(event.source, event.stage, event.sequence);
            }
            eprintln!("large lifecycle replay: pruned target index {selected_index} {target:?}, admitted {} destinations, applied {} FEATURES events", plan.admissions().len(), plan.feature_events().len());
        }
        LifecycleReplayMode::Full => {
            materializer.prepare_lifecycle_replay(&full_admissions);
            materializer.admit_many_parallel(&full_admissions);
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
    let mut first_block_light_differences = Vec::new();
    let mut block_light_difference_extents = Vec::new();
    let mut light_representations = Vec::new();
    let mut local_emissive_blocks = Vec::new();
    for block_section in 0..reference.column.section_count() {
        let Some(section_data) = reference.column.section(block_section) else {
            continue;
        };
        for cell in 0..4096 {
            let state = section_data.block_states().get(cell);
            let Some(state_id) = lodestone_data::block_states::StateId::new(state) else {
                continue;
            };
            let emission = lodestone_data::light_props::emission(state_id);
            if emission != 0 {
                local_emissive_blocks.push((
                    cell % 16,
                    reference.column.min_y() + block_section as i32 * 16 + (cell / 256) as i32,
                    (cell / 16) % 16,
                    state,
                    emission,
                ));
            }
        }
    }
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
            let mut first = None;
            let mut min = (usize::MAX, usize::MAX, usize::MAX);
            let mut max = (0usize, 0usize, 0usize);
            let mut reference_brighter = 0usize;
            let mut actual_brighter = 0usize;
            for cell in 0..4096 {
                let x = cell % 16;
                let z = (cell / 16) % 16;
                let y = cell / 256;
                let reference_value = reference.light.section_light(section).block_at(x, y, z);
                let actual_value = actual.light.section_light(section).block_at(x, y, z);
                if reference_value != actual_value {
                    let world_y = reference.column.min_y()
                        + section.saturating_sub(1) as i32 * 16
                        + y as i32;
                    first.get_or_insert((x, world_y, z, reference_value, actual_value));
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
            let first_source = first.and_then(|(x, world_y, z, _, _)| {
                local_emissive_blocks
                    .iter()
                    .min_by_key(|(source_x, source_y, source_z, _, _)| {
                        source_x.abs_diff(x)
                            + source_y.abs_diff(world_y) as usize
                            + source_z.abs_diff(z)
                    })
                    .map(|&(source_x, source_y, source_z, state, emission)| {
                        let distance = source_x.abs_diff(x)
                            + source_y.abs_diff(world_y) as usize
                            + source_z.abs_diff(z);
                        format!(
                            "local ({source_x},{source_y},{source_z}) {} emission {emission} distance {distance}",
                            state_label(state),
                        )
                    })
            });
            first_block_light_differences.push((section, first, first_source));
            let extent = first.is_some().then_some((min, max));
            block_light_difference_extents.push((section, extent, reference_brighter, actual_brighter));
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
        "; captured-packet diagnosis: reference={} bytes, Lodestone={} bytes, block cells differ={differing_blocks}, biome cells differ={differing_biomes}, heightmaps equal={heightmaps_equal} ({heightmap_differences:?}), block entities equal={block_entities_equal}, reference stored sky={reference_stored_sky:?}, block={reference_stored_block:?}, sky light cells differ={differing_sky_light_cells} in sections {differing_sky_light_sections:?}, first sky differences={first_sky_light_differences:?}, sky difference extents={sky_light_difference_extents:?}, block light cells differ={differing_block_light_cells} in sections {differing_block_light_sections:?}, first block differences={first_block_light_differences:?}, block difference extents={block_light_difference_extents:?}, local emissive blocks={} (nearest centre-column source per first difference), representations={light_representations:?}, non-air reference={non_air_reference}, Lodestone={non_air_actual}, first block difference={first_block_difference}, common state pairs={common_state_pairs}, differing blocks={block_difference_positions}",
        reference_len, actual_len,
        local_emissive_blocks.len(),
    )
}

const MAX_COMPONENT_EXAMPLES: usize = 32;
/// Keep component-state inventory bounded even when a batch contains many
/// unrelated bad cells.  The digest mismatch list remains exact for every
/// target; this cap applies only to the optional per-cell signature census.
const MAX_COMPONENT_SIGNATURES: usize = 32;
const MAX_REFERENCE_PACKET_BYTES: u64 = 8 * 1024 * 1024;
const MAX_REFERENCE_PACKET_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
const MAX_RAW_PACKET_OUTPUT_PACKETS: u64 = 4_096;
const MAX_RAW_PACKET_OUTPUT_BYTES: u64 = 64 * 1024 * 1024;

struct RawPacketOutput {
    directory: PathBuf,
    packets: u64,
    bytes: u64,
}

fn validate_raw_packet_output_limit(limit: u64) -> Result<(), String> {
    if limit > MAX_RAW_PACKET_OUTPUT_PACKETS {
        return Err(format!(
            "LODESTONE_LARGE_PARITY_PACKET_OUT_DIR supports at most {MAX_RAW_PACKET_OUTPUT_PACKETS} packets, got {limit}"
        ));
    }
    Ok(())
}

fn raw_packet_output_path(directory: &Path, target: ChunkPos) -> PathBuf {
    directory.join(format!("x{}_z{}.packet", target.0, target.1))
}

fn prepare_raw_packet_output(raw_packet: bool, limit: u64) -> Option<RawPacketOutput> {
    let path = std::env::var_os("LODESTONE_LARGE_PARITY_PACKET_OUT_DIR")?;
    assert!(
        raw_packet,
        "LODESTONE_LARGE_PARITY_PACKET_OUT_DIR is only supported for raw-packet manifests"
    );
    validate_raw_packet_output_limit(limit)
        .unwrap_or_else(|error| panic!("invalid raw packet output directory: {error}"));
    let directory = PathBuf::from(path);
    std::fs::create_dir_all(&directory).unwrap_or_else(|error| {
        panic!(
            "create raw packet output directory {}: {error}",
            directory.display()
        )
    });
    let mut entries = std::fs::read_dir(&directory).unwrap_or_else(|error| {
        panic!(
            "read raw packet output directory {}: {error}",
            directory.display()
        )
    });
    if let Some(entry) = entries.next() {
        let entry = entry.unwrap_or_else(|error| {
            panic!(
                "read raw packet output directory {} entry: {error}",
                directory.display()
            )
        });
        panic!(
            "raw packet output directory {} must be empty; found {}",
            directory.display(),
            entry.path().display()
        );
    }
    Some(RawPacketOutput {
        directory,
        packets: 0,
        bytes: 0,
    })
}

impl RawPacketOutput {
    fn write(&mut self, target: ChunkPos, payload: &[u8]) {
        assert!(
            self.packets < MAX_RAW_PACKET_OUTPUT_PACKETS,
            "raw packet output exceeded the {MAX_RAW_PACKET_OUTPUT_PACKETS}-packet bound"
        );
        let packet_bytes = u64::try_from(payload.len()).expect("raw packet length fits u64");
        assert!(
            packet_bytes <= MAX_REFERENCE_PACKET_BYTES,
            "raw packet at {target:?} is {packet_bytes} bytes, over the {MAX_REFERENCE_PACKET_BYTES}-byte diagnostic bound"
        );
        let total = self
            .bytes
            .checked_add(packet_bytes)
            .expect("raw packet output byte count overflow");
        assert!(
            total <= MAX_RAW_PACKET_OUTPUT_BYTES,
            "raw packet output exceeded the {MAX_RAW_PACKET_OUTPUT_BYTES}-byte bound"
        );
        let path = raw_packet_output_path(&self.directory, target);
        assert!(
            !path.exists(),
            "raw packet output path already exists for {target:?}: {}",
            path.display()
        );
        std::fs::write(&path, payload).unwrap_or_else(|error| {
            panic!("write raw packet output {}: {error}", path.display())
        });
        self.packets += 1;
        self.bytes = total;
    }
}

#[test]
fn raw_packet_output_directory_is_bounded_and_coordinate_keyed() {
    assert!(validate_raw_packet_output_limit(0).is_ok());
    assert!(validate_raw_packet_output_limit(MAX_RAW_PACKET_OUTPUT_PACKETS).is_ok());
    assert!(validate_raw_packet_output_limit(MAX_RAW_PACKET_OUTPUT_PACKETS + 1).is_err());
    assert_eq!(
        raw_packet_output_path(Path::new("/tmp/raw-packets"), (-24, -25)),
        PathBuf::from("/tmp/raw-packets/x-24_z-25.packet")
    );
}

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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ComponentComparison {
    Exact,
    LightOnly,
    ContentMismatch,
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

    fn comparison(&self) -> ComponentComparison {
        if self.terrain.total != 0
            || self.biomes.total != 0
            || self.heightmaps.total != 0
            || self.block_entities.total != 0
        {
            ComponentComparison::ContentMismatch
        } else if self.sky_light.total != 0
            || self.block_light.total != 0
            || self.masks.total != 0
        {
            ComponentComparison::LightOnly
        } else {
            ComponentComparison::Exact
        }
    }

    fn has_failing_mismatch(&self) -> bool {
        self.comparison() == ComponentComparison::ContentMismatch
    }
}

#[test]
fn packet_component_policy_fails_for_terrain_mismatches() {
    let mut report = PacketComponentReport::default();
    report
        .terrain
        .push("minecraft:air -> minecraft:stone".to_owned(), "cell".to_owned());

    assert_eq!(report.comparison(), ComponentComparison::ContentMismatch);
    assert!(report.has_failing_mismatch());
}

#[test]
fn packet_component_policy_records_light_without_failing() {
    let mut report = PacketComponentReport::default();
    report.sky_light.push("15 -> 14".to_owned(), "cell".to_owned());
    report
        .masks
        .push("sky: present -> missing".to_owned(), "section".to_owned());

    assert!(report.has_mismatch());
    assert_eq!(report.comparison(), ComponentComparison::LightOnly);
    assert!(!report.has_failing_mismatch());
}

fn load_reference_packets(
    dimension: Dimension,
    cx0: i32,
    cx1: i32,
    cz0: i32,
    cz1: i32,
    start: u64,
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
    let packet_limit = usize::try_from(limit - start).unwrap_or(usize::MAX);
    assert!(paths.len() <= packet_limit, "reference packet inputs ({}) exceed the selected manifest batch ({start}..{limit}, {packet_limit} chunks)", paths.len());
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
        assert!(coordinate.0 >= cx0 && coordinate.0 <= cx1 && coordinate.1 >= cz0 && coordinate.1 <= cz1 && index >= 0 && u64::try_from(index).is_ok_and(|index| index >= start && index < limit), "reference packet {} decodes to {coordinate:?}, outside the selected manifest batch", path.display());
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

/// Writes a machine-readable mismatch inventory for the follow-up oracle
/// diagnostic.  The inventory is evidence, not acceptance input: every row
/// repeats the authenticated manifest identity and the expected full digest,
/// allowing the reference-export step to reject stale or hand-edited rows
/// before it asks the external oracle for packet bodies.
fn write_mismatch_inventory(
    path: &Path,
    manifest: &Path,
    header: &ManifestHeader,
    digest: &[(i32, i32, [u8; 32], [u8; 32])],
    raw: &[RawPacketMismatch],
) {
    let kind = match header.semantic_version {
        6 => "raw-packet",
        7 => "light-free",
        _ => "semantic",
    };
    let sidecar = match header.semantic_version {
        6 => Some(raw_packet_audit_path(manifest)),
        7 => Some(light_free_audit_path(manifest)),
        _ => None,
    };
    let width = i64::from(header.cx1 - header.cx0 + 1);
    let mut rows = Vec::with_capacity(digest.len() + raw.len());
    for &(cx, cz, expected, actual) in digest {
        let index = i64::from(cz - header.cz0)
            .checked_mul(width)
            .and_then(|value| value.checked_add(i64::from(cx - header.cx0)))
            .expect("mismatch inventory index fits i64");
        rows.push((index as u64, (cx, cz), expected, actual));
    }
    rows.extend(raw.iter().map(|mismatch| {
        (
            mismatch.index,
            mismatch.target,
            mismatch.expected_full,
            mismatch.actual_full,
        )
    }));
    rows.sort_unstable_by_key(|row| row.0);

    let mut output = String::new();
    output.push_str("schema=lodestone-large-parity-mismatch-v1\n");
    output.push_str(&format!("manifest_sha256={}\n", file_sha256(manifest)));
    if let Some(sidecar) = sidecar {
        output.push_str(&format!("sidecar_sha256={}\n", file_sha256(&sidecar)));
    }
    output.push_str(&format!(
        "semantic_version={}\ndimension={}\nfrozen_world_sha256={}\ngeometry={}..{}:{}..{}\nrecord_width={}\nkind={}\nmismatch_count={}\n",
        header.semantic_version,
        dimension_capture_name(header.dimension),
        hex(&header.frozen_world),
        header.cx0,
        header.cx1,
        header.cz0,
        header.cz1,
        header.record_width,
        kind,
        rows.len(),
    ));
    output.push_str("index\tcx\tcz\texpected_full_sha256\tactual_full_sha256\n");
    for (index, (cx, cz), expected, actual) in rows {
        output.push_str(&format!(
            "{index}\t{cx}\t{cz}\t{}\t{}\n",
            hex(&expected),
            hex(&actual),
        ));
    }
    std::fs::write(path, output).unwrap_or_else(|error| {
        panic!("write mismatch inventory {}: {error}", path.display())
    });
}

fn decode_digest32(value: &str, what: &str) -> [u8; 32] {
    assert_eq!(value.len(), 64, "{what} must contain 64 hexadecimal characters");
    let mut result = [0u8; 32];
    for (index, byte) in result.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .unwrap_or_else(|error| panic!("invalid {what} at byte {index}: {error}"));
    }
    result
}

/// Reads the inventory after re-authenticating the manifest and its v6
/// sidecar.  The file is intentionally small and line-oriented so shell
/// orchestration can move it between the Rust comparator and the JVM oracle
/// without inventing a second binary format.
fn load_mismatch_inventory(
    inventory: &Path,
    manifest: &Path,
    header: &ManifestHeader,
    sidecar: &Path,
) -> Vec<RawPacketMismatch> {
    assert_eq!(header.semantic_version, 6, "packet component diagnostics require a v6 manifest");
    let lines = std::fs::read_to_string(inventory)
        .unwrap_or_else(|error| panic!("read mismatch inventory {}: {error}", inventory.display()))
        .lines()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(lines.first().map(String::as_str), Some("schema=lodestone-large-parity-mismatch-v1"), "mismatch inventory schema differs");
    let mut fields = BTreeMap::new();
    let row_start = lines
        .iter()
        .position(|line| line == "index\tcx\tcz\texpected_full_sha256\tactual_full_sha256")
        .unwrap_or_else(|| panic!("mismatch inventory has no row header"));
    for line in &lines[1..row_start] {
        let (key, value) = line
            .split_once('=')
            .unwrap_or_else(|| panic!("malformed mismatch inventory header: {line}"));
        assert!(fields.insert(key, value).is_none(), "duplicate mismatch inventory field {key}");
    }
    let manifest_sha = file_sha256(manifest);
    let sidecar_sha = file_sha256(sidecar);
    assert_eq!(fields.get("manifest_sha256").copied(), Some(manifest_sha.as_str()), "mismatch inventory manifest identity differs");
    assert_eq!(fields.get("sidecar_sha256").copied(), Some(sidecar_sha.as_str()), "mismatch inventory sidecar identity differs");
    assert_eq!(fields.get("kind").copied(), Some("raw-packet"), "mismatch inventory kind differs");
    assert_eq!(fields.get("semantic_version").copied(), Some("6"), "mismatch inventory version differs");
    assert_eq!(fields.get("dimension").copied(), Some(dimension_capture_name(header.dimension)), "mismatch inventory dimension differs");
    let frozen_sha = hex(&header.frozen_world);
    assert_eq!(fields.get("frozen_world_sha256").copied(), Some(frozen_sha.as_str()), "mismatch inventory frozen-world identity differs");
    let expected_count = fields
        .get("mismatch_count")
        .unwrap_or_else(|| panic!("mismatch inventory has no mismatch_count"))
        .parse::<usize>()
        .expect("mismatch inventory count is an integer");
    assert!(expected_count <= 4_096, "mismatch inventory exceeds the bounded 4096-row diagnostic limit");
    assert_eq!(lines.len() - row_start - 1, expected_count, "mismatch inventory count differs from its rows");

    let width = i64::from(header.cx1 - header.cx0 + 1);
    let mut rows = Vec::with_capacity(expected_count);
    let mut audit = File::open(sidecar).unwrap_or_else(|error| panic!("open packet-audit sidecar: {error}"));
    for line in &lines[row_start + 1..] {
        let fields = line.split('\t').collect::<Vec<_>>();
        assert_eq!(fields.len(), 5, "mismatch inventory row has the wrong number of fields");
        let index = fields[0].parse::<u64>().expect("mismatch index is an integer");
        let cx = fields[1].parse::<i32>().expect("mismatch x is an integer");
        let cz = fields[2].parse::<i32>().expect("mismatch z is an integer");
        assert!(index < header.count, "mismatch index is outside the manifest");
        let expected_index = i64::from(cz - header.cz0)
            .checked_mul(width)
            .and_then(|value| value.checked_add(i64::from(cx - header.cx0)))
            .expect("mismatch coordinate index fits i64");
        assert_eq!(u64::try_from(expected_index).expect("mismatch index is non-negative"), index, "mismatch coordinate disagrees with index");
        let expected = decode_digest32(fields[3], "expected mismatch digest");
        let actual = decode_digest32(fields[4], "actual mismatch digest");
        assert_ne!(expected, actual, "mismatch inventory contains an equal digest");
        let authenticated: [u8; PACKET_AUDIT_RECORD_BYTES] = read_manifest_record_at(&mut audit, index as usize, PACKET_AUDIT_RECORD_BYTES)
            .expect("read authenticated mismatch digest")
            .try_into()
            .expect("authenticated mismatch digest width");
        assert_eq!(expected, authenticated, "mismatch inventory expected digest differs from sidecar");
        rows.push(RawPacketMismatch {
            target: (cx, cz),
            index,
            expected_prefix: [expected[0], expected[1]],
            actual_prefix: [actual[0], actual[1]],
            expected_full: expected,
            actual_full: actual,
            payload_bytes: 0,
        });
    }
    rows
}

/// File-only second phase of the bounded mismatch workflow.  The expensive
/// generated-world scan writes actual packets and an authenticated inventory;
/// the JVM oracle then writes reference packets only for those coordinates.
/// This test compares those files without regenerating any chunk, and retains
/// the existing exact component/signature census.
#[test]
#[ignore = "requires an authenticated mismatch inventory and packet directories"]
fn parity_mismatch_packet_files_report_components() {
    let manifest_path = std::env::var(MISMATCH_INVENTORY_ENV)
        .expect("set LODESTONE_LARGE_PARITY_MISMATCH_OUT");
    let inventory = Path::new(&manifest_path);
    let manifest = std::env::var("LODESTONE_LARGE_PARITY_MANIFEST")
        .expect("set LODESTONE_LARGE_PARITY_MANIFEST");
    let manifest = Path::new(&manifest);
    let mut raw_header = [0; HEADER_BYTES];
    File::open(manifest)
        .unwrap_or_else(|error| panic!("open manifest {}: {error}", manifest.display()))
        .read_exact(&mut raw_header)
        .expect("read manifest header");
    let header = read_header(&raw_header[..]).expect("valid manifest header");
    let (sidecar, sidecar_header) = load_raw_packet_audit(manifest, &header);
    verify_raw_packet_audit_files(manifest, &raw_header, &header, &sidecar, &sidecar_header);
    let mismatches = load_mismatch_inventory(inventory, manifest, &header, &sidecar);
    let actual_dir = PathBuf::from(std::env::var(MISMATCH_PACKET_DIR_ENV).expect("set LODESTONE_LARGE_PARITY_MISMATCH_PACKET_DIR"));
    let reference_dir = PathBuf::from(std::env::var(MISMATCH_REFERENCE_PACKET_DIR_ENV).expect("set LODESTONE_LARGE_PARITY_MISMATCH_REFERENCE_PACKET_DIR"));
    let mut components = Vec::with_capacity(mismatches.len());
    for mismatch in &mismatches {
        let name = format!("x{}_z{}.packet", mismatch.target.0, mismatch.target.1);
        let actual_path = actual_dir.join(&name);
        let reference_path = reference_dir.join(&name);
        let actual = std::fs::read(&actual_path).unwrap_or_else(|error| panic!("read actual packet {}: {error}", actual_path.display()));
        let reference = std::fs::read(&reference_path).unwrap_or_else(|error| panic!("read reference packet {}: {error}", reference_path.display()));
        assert_eq!(raw_packet_full_digest(&actual), mismatch.actual_full, "actual packet digest differs from the inventory at {:?}", mismatch.target);
        assert_eq!(raw_packet_full_digest(&reference), mismatch.expected_full, "reference packet digest differs from the authenticated sidecar at {:?}", mismatch.target);
        components.push((mismatch.target, packet_component_difference(&reference, &actual, header.dimension)));
    }
    let report = format_diagnostic_report(true, mismatches.len() as u64, header.count, &[], &mismatches, &components);
    if let Some(path) = std::env::var_os(MISMATCH_COMPONENT_REPORT_ENV) {
        std::fs::write(&path, &report).unwrap_or_else(|error| panic!("write component report {}: {error}", Path::new(&path).display()));
    }
    print!("{report}");
    assert!(components.iter().all(|(_, report)| !report.has_failing_mismatch()), "component mismatch report contains failing terrain/content differences");
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
