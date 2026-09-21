//! Live external-oracle parity comparison.
//!
//! The shell launcher connects this test to `LargeParityOracle --mode stream`
//! with an ephemeral FIFO. Content records are compared as they arrive; the
//! opt-in scan-all mode writes bounded diagnostic artifacts, never a resume
//! ledger.

use std::fs::{self, File};
use std::collections::{BTreeMap, BTreeSet};
use std::io::{BufReader, Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use lodestone_core::Reader;
use lodestone_server::{ChunkColumn, EndChunkSource, NetherChunkSource, OverworldChunkSource, end_chunk_source, nether_chunk_source, overworld_chunk_source};
use lodestone_world::{ChunkColumn as WorldChunkColumn, Heightmaps};
use lodestone_worldgen::stage_schedule::{
    NETHER_FEATURE_SOURCE_RADIUS, NETHER_FEATURE_WRITE_RADIUS,
};
use lodestone_worldgen_parity::lifecycle::{
    LifecycleCompletion, LifecycleFeatureDispatch, LifecycleFeatureResult, LifecycleMaterializer,
    LifecycleReplayEvent, LifecycleResidentStage, LifecycleResidentTransition,
    LifecycleWorldgenSource,
};
use lodestone_server::{ServerDirective, ServerProtocol};
use lodestone_server::dimension::Dimension as ServerDimension;
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::packets::chunk::{ChunkShape, LevelChunkWithLight};

#[allow(dead_code)]
mod support { pub mod large_parity_manifest; }

const HEADER_BYTES: usize = 256;
const DIGEST_BYTES: usize = 32;
const STREAM_MAGIC: &[u8; 8] = b"LWS26S01";
const STREAM_DOMAIN: &[u8] = b"lodestone.worldgen.streaming-parity/v2/light-free";
const END_STREAM_DOMAIN: &[u8] = b"lodestone.worldgen.streaming-parity/v3/end-p06-lifecycle";
const END_STREAM_EVENT_DOMAIN: &[u8] = b"lodestone.worldgen.streaming-parity/end-p06-lifecycle-event/v1";
const PROTOCOL: u32 = 776;
const SEED: i64 = 42;
const MAX_FRAME_BYTES: u32 = 16 * 1024 * 1024;
const STREAM_FORMAT_LIGHT_FREE: u16 = 7;
const STREAM_FORMAT_END_P06_LIFECYCLE: u16 = 8;
const DEFAULT_ORDERED_STREAM_BATCH_SIZE: usize = 1;
const DEFAULT_END_STREAM_BATCH_SIZE: usize = 64;
const MAX_SCAN_COORDINATE_PAIRS: usize = 4_096;
const MAX_SCAN_EVIDENCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_SCAN_SIGNATURE_GROUPS: usize = 64;
const MAX_SCAN_EXAMPLES_PER_GROUP: usize = 32;
const SCAN_EVIDENCE_MAGIC: &[u8; 8] = b"LWS26E01";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StreamDimension { Overworld, Nether, End }

impl StreamDimension {
    fn parse() -> Self {
        match std::env::var("LODESTONE_LARGE_PARITY_STREAM_DIMENSION").as_deref() {
            Ok("overworld") | Err(std::env::VarError::NotPresent) => Self::Overworld,
            Ok("nether") => Self::Nether,
            Ok("end") => Self::End,
            Ok(other) => panic!("unsupported stream dimension {other:?}"),
            Err(error) => panic!("read stream dimension: {error}"),
        }
    }

    fn name(self) -> &'static [u8] {
        match self {
            Self::Overworld => b"minecraft:overworld",
            Self::Nether => b"minecraft:the_nether",
            Self::End => b"minecraft:the_end",
        }
    }

    fn manifest_dimension(self) -> support::large_parity_manifest::Dimension {
        match self {
            Self::Overworld => support::large_parity_manifest::Dimension::Overworld,
            Self::Nether => support::large_parity_manifest::Dimension::Nether,
            Self::End => support::large_parity_manifest::Dimension::End,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct StreamHeader {
    cx0: i32,
    cx1: i32,
    cz0: i32,
    cz1: i32,
    count: u64,
    start: u64,
    format: u16,
    dimension: StreamDimension,
    payload_digest: [u8; DIGEST_BYTES],
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 { u16::from_be_bytes(bytes[offset..offset + 2].try_into().unwrap()) }
fn u32_at(bytes: &[u8], offset: usize) -> u32 { u32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) }
fn i32_at(bytes: &[u8], offset: usize) -> i32 { i32::from_be_bytes(bytes[offset..offset + 4].try_into().unwrap()) }
fn i64_at(bytes: &[u8], offset: usize) -> i64 { i64::from_be_bytes(bytes[offset..offset + 8].try_into().unwrap()) }
fn u64_at(bytes: &[u8], offset: usize) -> u64 { u64::from_be_bytes(bytes[offset..offset + 8].try_into().unwrap()) }

fn parse_stream_header(raw: &[u8]) -> StreamHeader {
    assert_eq!(raw.len(), HEADER_BYTES, "stream header width");
    assert_eq!(&raw[..8], STREAM_MAGIC, "stream magic");
    assert_eq!(u16_at(raw, 8), 1, "stream version");
    assert_eq!(u16_at(raw, 10), HEADER_BYTES as u16, "stream header size");
    assert_eq!(u16_at(raw, 12), 2, "stream digest algorithm");
    assert_eq!(u16_at(raw, 14), 1, "stream schema");
    assert_eq!(u32_at(raw, 16), PROTOCOL, "stream protocol");
    assert_eq!(i64_at(raw, 20), SEED, "stream seed");
    let cx0 = i32_at(raw, 28);
    let cx1 = i32_at(raw, 32);
    let cz0 = i32_at(raw, 36);
    let cz1 = i32_at(raw, 40);
    assert!(cx0 <= cx1 && cz0 <= cz1, "stream bounds are ordered");
    let count = u64_at(raw, 44);
    let expected_count = u64::try_from(i64::from(cx1) - i64::from(cx0) + 1).unwrap()
        * u64::try_from(i64::from(cz1) - i64::from(cz0) + 1).unwrap();
    assert_eq!(count, expected_count, "stream count matches bounds");
    assert_eq!(u16_at(raw, 52), DIGEST_BYTES as u16, "stream digest width");
    let format = u16_at(raw, 54);
    let domain = match format {
        STREAM_FORMAT_LIGHT_FREE => STREAM_DOMAIN,
        STREAM_FORMAT_END_P06_LIFECYCLE => END_STREAM_DOMAIN,
        other => panic!("unsupported stream format {other}"),
    };
    assert_eq!(&raw[56..88], support::large_parity_manifest::sha256(domain).as_slice(), "stream domain");
    let dimension_digest = &raw[88..120];
    let dimension = [StreamDimension::Overworld, StreamDimension::Nether, StreamDimension::End]
        .into_iter()
        .find(|candidate| support::large_parity_manifest::sha256(candidate.name()).as_slice() == dimension_digest)
        .expect("stream dimension identity");
    let start = u64_at(raw, 184);
    let payload_digest = raw[192..224].try_into().unwrap();
    assert!(raw[224..].iter().all(|byte| *byte == 0), "stream header reserved bytes are non-zero");
    assert!(start <= count, "stream start is in bounds");
    if format == STREAM_FORMAT_END_P06_LIFECYCLE {
        assert_eq!(dimension, StreamDimension::End, "End P06 lifecycle format must name the End dimension");
    } else {
        assert_ne!(dimension, StreamDimension::End, "End streams must carry P06 lifecycle transitions");
    }
    StreamHeader { cx0, cx1, cz0, cz1, count, start, format, dimension, payload_digest }
}

struct Frame {
    index: u64,
    cx: i32,
    cz: i32,
    digest: [u8; DIGEST_BYTES],
    record: Vec<u8>,
    lifecycle_events: Vec<LifecycleReplayEvent>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, PartialOrd, Ord)]
struct DifferenceBounds {
    x0: Option<i32>,
    x1: Option<i32>,
    y0: Option<i32>,
    y1: Option<i32>,
    z0: Option<i32>,
    z1: Option<i32>,
}

impl DifferenceBounds {
    fn observe(&mut self, x: i32, y: i32, z: i32) {
        self.x0 = Some(self.x0.map_or(x, |value| value.min(x)));
        self.x1 = Some(self.x1.map_or(x, |value| value.max(x)));
        self.y0 = Some(self.y0.map_or(y, |value| value.min(y)));
        self.y1 = Some(self.y1.map_or(y, |value| value.max(y)));
        self.z0 = Some(self.z0.map_or(z, |value| value.min(z)));
        self.z1 = Some(self.z1.map_or(z, |value| value.max(z)));
    }

    fn observe_xz(&mut self, x: i32, z: i32) {
        self.x0 = Some(self.x0.map_or(x, |value| value.min(x)));
        self.x1 = Some(self.x1.map_or(x, |value| value.max(x)));
        self.z0 = Some(self.z0.map_or(z, |value| value.min(z)));
        self.z1 = Some(self.z1.map_or(z, |value| value.max(z)));
    }

    fn observe_y(&mut self, y: i32) {
        self.y0 = Some(self.y0.map_or(y, |value| value.min(y)));
        self.y1 = Some(self.y1.map_or(y, |value| value.max(y)));
    }

    fn text(&self) -> String {
        let value = |value: Option<i32>| value.map_or_else(|| "-".to_owned(), |value| value.to_string());
        format!("{}..{}:{}..{}:{}..{}", value(self.x0), value(self.x1), value(self.y0), value(self.y1), value(self.z0), value(self.z1))
    }
}

#[derive(Debug, Clone)]
struct ComponentDifference {
    component: &'static str,
    expected_identity: String,
    actual_identity: String,
    count: u64,
    bounds: DifferenceBounds,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MismatchSignature {
    component: &'static str,
    expected_identity: String,
    actual_identity: String,
    count: u64,
    bounds: DifferenceBounds,
}

#[derive(Debug)]
struct SignatureGroup {
    count: u64,
    examples: usize,
}

struct ScanArtifacts {
    dir: PathBuf,
    inventory: std::io::BufWriter<File>,
    provenance: std::io::BufWriter<File>,
    representatives: std::io::BufWriter<File>,
    evidence: std::io::BufWriter<File>,
    groups: BTreeMap<MismatchSignature, SignatureGroup>,
    component_counts: BTreeMap<&'static str, u64>,
    total_mismatches: u64,
    retained_coordinates: usize,
    retained_evidence_bytes: usize,
    omitted_group_records: u64,
    signature_records: u64,
}

impl ScanArtifacts {
    fn new(dir: PathBuf, raw_header: &[u8; HEADER_BYTES], header: StreamHeader) -> Result<Self, String> {
        match fs::metadata(&dir) {
            Ok(metadata) if !metadata.is_dir() => {
                return Err(format!("stream parity artifact path is not a directory: {}", dir.display()));
            }
            Ok(_) => {
                let mut entries = fs::read_dir(&dir).map_err(|error| {
                    format!("read stream parity artifact directory {}: {error}", dir.display())
                })?;
                if entries.next().is_some() {
                    return Err(format!("stream parity artifact directory is not empty: {}", dir.display()));
                }
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                fs::create_dir_all(&dir).map_err(|create_error| {
                    format!("create stream parity artifact directory {}: {create_error}", dir.display())
                })?;
            }
            Err(error) => return Err(format!("inspect stream parity artifact directory {}: {error}", dir.display())),
        }
        let mut inventory = std::io::BufWriter::new(create_artifact(&dir, "mismatch-inventory.tsv"));
        let mut provenance = std::io::BufWriter::new(create_artifact(&dir, "provenance.tsv"));
        let mut representatives = std::io::BufWriter::new(create_artifact(&dir, "representatives.tsv"));
        let mut evidence = std::io::BufWriter::new(create_artifact(&dir, "representative-records.bin"));
        evidence.write_all(SCAN_EVIDENCE_MAGIC).expect("write scan evidence header");
        let header_path = dir.join("stream-header.bin");
        fs::write(&header_path, raw_header).map_err(|error| {
            format!("write stream parity header {}: {error}", header_path.display())
        })?;
        let header_digest = support::large_parity_manifest::sha256(raw_header);
        let metadata = format!(
            "format=lodestone-stream-parity-scan/v1\nheader_sha256={}\ndimension={}\nformat_id={}\ncoordinates={}..{} x {}..{}\ncount={}\nseed={}\n",
            hex(&header_digest),
            String::from_utf8_lossy(header.dimension.name()),
            header.format,
            header.cx0,
            header.cx1,
            header.cz0,
            header.cz1,
            header.count,
            SEED,
        );
        fs::write(dir.join("stream-header.txt"), metadata)
            .expect("write stream parity header metadata");
        writeln!(
            &mut inventory,
            "index\tcx\tcz\tcomponent\texpected_record_digest\tactual_record_digest\texpected_identity\tactual_identity\tdifference_count\tbounds",
        )
        .expect("write scan inventory header");
        writeln!(&mut provenance, "index\tcx\tcz\tevent\tsource\tsequence\tstage\tresidents")
            .expect("write scan provenance header");
        writeln!(&mut representatives, "component\tindex\tcx\tcz\texpected_identity\tactual_identity\tdifference_count\tbounds\tevidence_offset\texpected_bytes\tactual_bytes\tevidence_end")
            .expect("write scan representative header");
        Ok(Self {
            dir,
            inventory,
            provenance,
            representatives,
            evidence,
            groups: BTreeMap::new(),
            component_counts: BTreeMap::new(),
            total_mismatches: 0,
            retained_coordinates: 0,
            retained_evidence_bytes: SCAN_EVIDENCE_MAGIC.len(),
            omitted_group_records: 0,
            signature_records: 0,
        })
    }

    fn record_pair(&mut self, frame: &Frame, actual: &[u8], differences: &[ComponentDifference]) {
        self.total_mismatches += 1;
        let actual_digest = support::large_parity_manifest::sha256(actual);
        for difference in differences {
            self.signature_records += 1;
            *self.component_counts.entry(difference.component).or_default() += 1;
            writeln!(
                self.inventory,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                frame.index,
                frame.cx,
                frame.cz,
                difference.component,
                hex(&frame.digest),
                hex(&actual_digest),
                difference.expected_identity,
                difference.actual_identity,
                difference.count,
                difference.bounds.text(),
            )
            .expect("write scan mismatch inventory");

            let signature = MismatchSignature {
                component: difference.component,
                expected_identity: difference.expected_identity.clone(),
                actual_identity: difference.actual_identity.clone(),
                count: difference.count,
                bounds: difference.bounds.clone(),
            };
            let should_retain_example = if let Some(group) = self.groups.get_mut(&signature) {
                group.count += 1;
                group.examples < MAX_SCAN_EXAMPLES_PER_GROUP
                    && self.retained_coordinates < MAX_SCAN_COORDINATE_PAIRS
            } else if self.groups.len() < MAX_SCAN_SIGNATURE_GROUPS {
                self.groups.insert(signature.clone(), SignatureGroup { count: 1, examples: 0 });
                self.retained_coordinates < MAX_SCAN_COORDINATE_PAIRS
            } else {
                self.omitted_group_records += 1;
                false
            };
            if should_retain_example && self.retain_example(frame, actual, difference) {
                self.groups
                    .get_mut(&signature)
                    .expect("retained scan signature")
                    .examples += 1;
            }
        }

        if self.retained_coordinates < MAX_SCAN_COORDINATE_PAIRS {
            self.retained_coordinates += 1;
            if frame.lifecycle_events.is_empty() {
                writeln!(
                    self.provenance,
                    "{}\t{}\t{}\t-\t-\t-\t-\t0",
                    frame.index, frame.cx, frame.cz,
                )
                .expect("write empty scan provenance");
            } else {
                for event in &frame.lifecycle_events {
                    writeln!(
                        self.provenance,
                        "{}\t{}\t{}\t{}\t({}, {})\t{}\t{:?}\t{}",
                        frame.index,
                        frame.cx,
                        frame.cz,
                        event.sequence,
                        event.source.0,
                        event.source.1,
                        event.sequence,
                        event.stage,
                        event.resident_transitions.len(),
                    )
                    .expect("write scan provenance");
                }
            }
        }
    }

    fn retain_example(&mut self, frame: &Frame, actual: &[u8], difference: &ComponentDifference) -> bool {
        let header_bytes: usize = 8 + 4 + 4 + 4 + 4;
        let bytes = header_bytes
            .checked_add(frame.record.len())
            .and_then(|bytes| bytes.checked_add(actual.len()))
            .expect("scan evidence size overflow");
        let start = self.retained_evidence_bytes;
        if self.retained_evidence_bytes.checked_add(bytes).is_none()
            || self.retained_evidence_bytes + bytes > MAX_SCAN_EVIDENCE_BYTES
        {
            return false;
        }
        self.evidence.write_all(&frame.index.to_be_bytes()).expect("write scan evidence index");
        self.evidence.write_all(&frame.cx.to_be_bytes()).expect("write scan evidence x");
        self.evidence.write_all(&frame.cz.to_be_bytes()).expect("write scan evidence z");
        self.evidence.write_all(&(frame.record.len() as u32).to_be_bytes()).expect("write scan evidence expected length");
        self.evidence.write_all(&(actual.len() as u32).to_be_bytes()).expect("write scan evidence actual length");
        self.evidence.write_all(&frame.record).expect("write expected scan evidence");
        self.evidence.write_all(actual).expect("write actual scan evidence");
        self.retained_evidence_bytes += bytes;
        writeln!(
            self.representatives,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            difference.component,
            frame.index,
            frame.cx,
            frame.cz,
            difference.expected_identity,
            difference.actual_identity,
            difference.count,
            difference.bounds.text(),
            start,
            frame.record.len(),
            actual.len(),
            self.retained_evidence_bytes,
        )
        .expect("write scan representative");
        true
    }

    fn finish(mut self, compared: u64) {
        self.inventory.flush().expect("flush scan inventory");
        self.provenance.flush().expect("flush scan provenance");
        self.representatives.flush().expect("flush scan representatives");
        self.evidence.flush().expect("flush scan evidence");
        let mut groups = std::io::BufWriter::new(create_artifact(&self.dir, "signature-groups.tsv"));
        writeln!(groups, "component\texpected_identity\tactual_identity\tdifference_count\tbounds\toccurrences\texamples").expect("write scan group header");
        for (signature, group) in &self.groups {
            writeln!(
                groups,
                "{}\t{}\t{}\t{}\t{}\t{}\t{}",
                signature.component,
                signature.expected_identity,
                signature.actual_identity,
                signature.count,
                signature.bounds.text(),
                group.count,
                group.examples,
            )
            .expect("write scan signature group");
        }
        groups.flush().expect("flush scan signature groups");
        let mut summary = String::new();
        summary.push_str("format=lodestone-stream-parity-scan/v1\n");
        summary.push_str(&format!("compared={}\n", compared));
        summary.push_str(&format!("mismatches={}\n", self.total_mismatches));
        for (component, count) in &self.component_counts {
            summary.push_str(&format!("component.{}={}\n", component, count));
        }
        summary.push_str(&format!("signature_groups={}\n", self.groups.len()));
        summary.push_str(&format!("signature_records={}\n", self.signature_records));
        summary.push_str(&format!("omitted_signature_group_records={}\n", self.omitted_group_records));
        summary.push_str(&format!("retained_coordinate_pairs={}\n", self.retained_coordinates));
        summary.push_str(&format!("retained_evidence_bytes={}\n", self.retained_evidence_bytes));
        summary.push_str(&format!("evidence_cap_bytes={}\n", MAX_SCAN_EVIDENCE_BYTES));
        summary.push_str(&format!("coordinate_cap={}\n", MAX_SCAN_COORDINATE_PAIRS));
        summary.push_str(&format!("signature_group_cap={}\n", MAX_SCAN_SIGNATURE_GROUPS));
        summary.push_str(&format!("examples_per_group_cap={}\n", MAX_SCAN_EXAMPLES_PER_GROUP));
        fs::write(self.dir.join("summary.txt"), summary).expect("write scan summary");
    }
}

fn create_artifact(dir: &std::path::Path, name: &str) -> File {
    File::create(dir.join(name)).unwrap_or_else(|error| {
        panic!("create stream parity artifact {}: {error}", dir.join(name).display())
    })
}

fn selected_scan_artifact_dir() -> PathBuf {
    if let Some(path) = std::env::var_os("LODESTONE_LARGE_PARITY_STREAM_ARTIFACT_DIR")
        .filter(|path| !path.is_empty())
    {
        return PathBuf::from(path);
    }
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("scan artifact clock")
        .as_nanos();
    PathBuf::from(".cache/worldgen-parity/runs")
        .join(format!("stream-{}-{stamp}", std::process::id()))
}

/// Decode the P06 resident sidecar before it reaches the materializer.  The
/// sidecar is authenticated by the frame digest and carries the raw resident
/// heightmap values observed at each FEATURES boundary; no final terrain scan
/// is involved.
fn parse_end_p06_lifecycle_events(
    bytes: &[u8],
    target_x: i32,
    target_z: i32,
) -> Vec<LifecycleReplayEvent> {
    fn take<'a>(bytes: &'a [u8], cursor: &mut usize, width: usize) -> &'a [u8] {
        let end = cursor.checked_add(width).expect("P06 lifecycle cursor overflow");
        let slice = bytes
            .get(*cursor..end)
            .expect("truncated P06 lifecycle event payload");
        *cursor = end;
        slice
    }
    fn read_u8(bytes: &[u8], cursor: &mut usize) -> u8 { take(bytes, cursor, 1)[0] }
    fn read_u32(bytes: &[u8], cursor: &mut usize) -> u32 {
        u32::from_be_bytes(take(bytes, cursor, 4).try_into().unwrap())
    }
    fn read_i32(bytes: &[u8], cursor: &mut usize) -> i32 {
        i32::from_be_bytes(take(bytes, cursor, 4).try_into().unwrap())
    }
    fn read_u64(bytes: &[u8], cursor: &mut usize) -> u64 {
        u64::from_be_bytes(take(bytes, cursor, 8).try_into().unwrap())
    }

    assert!(bytes.starts_with(END_STREAM_EVENT_DOMAIN), "P06 lifecycle event domain");
    let mut cursor = END_STREAM_EVENT_DOMAIN.len();
    let event_count = read_u32(bytes, &mut cursor) as usize;
    assert_eq!(event_count, 9, "End P06 lifecycle event count");
    let expected_sources = (-1..=1)
        .flat_map(|x| (-1..=1).map(move |z| (target_x + x, target_z + z)))
        .collect::<BTreeSet<_>>();
    let mut seen_sources = BTreeSet::new();
    let mut previous_sequence = None;
    let mut events = Vec::with_capacity(event_count);
    for event_index in 0..event_count {
        let sequence = read_u64(bytes, &mut cursor);
        if let Some(previous) = previous_sequence {
            assert!(previous < sequence, "P06 lifecycle event sequence is not increasing");
        }
        previous_sequence = Some(sequence);
        let source = (read_i32(bytes, &mut cursor), read_i32(bytes, &mut cursor));
        let expected_source = (
            target_x + (event_index / 3) as i32 - 1,
            target_z + (event_index % 3) as i32 - 1,
        );
        assert_eq!(source, expected_source, "End P06 lifecycle source order");
        assert!(expected_sources.contains(&source), "P06 source outside target wavefront");
        assert!(seen_sources.insert(source), "P06 lifecycle source is repeated");
        assert_eq!(read_u8(bytes, &mut cursor), 1, "P06 lifecycle completion stage");
        let transition_count = read_u32(bytes, &mut cursor) as usize;
        let expected_transition_count = usize::from(source != (target_x, target_z)) + 1;
        assert_eq!(transition_count, expected_transition_count, "P06 lifecycle transition count");
        let mut seen_residents = BTreeSet::new();
        let mut resident_transitions = Vec::with_capacity(transition_count);
        let mut saw_source = false;
        let mut saw_target = false;
        for _ in 0..transition_count {
            let resident = (read_i32(bytes, &mut cursor), read_i32(bytes, &mut cursor));
            assert!(
                (resident.0 - target_x).abs() <= 2 && (resident.1 - target_z).abs() <= 2,
                "P06 resident outside target admission halo",
            );
            assert!(seen_residents.insert(resident), "P06 resident is repeated in one event");
            saw_source |= resident == source;
            saw_target |= resident == (target_x, target_z);
            let stage = match read_u8(bytes, &mut cursor) {
                0 => LifecycleResidentStage::Carvers,
                1 => LifecycleResidentStage::Features,
                2 => LifecycleResidentStage::Full,
                other => panic!("unsupported P06 lifecycle resident stage {other}"),
            };
            let client_heightmaps = match read_u8(bytes, &mut cursor) {
                0 => None,
                1 => {
                    let mut maps = [[0u16; 256]; 3];
                    for map in &mut maps {
                        for cell in map {
                            let value = u16_at(take(bytes, &mut cursor, 2), 0);
                            assert!(value <= 256, "P06 lifecycle map cell exceeds End height");
                            *cell = value;
                        }
                    }
                    Some(maps)
                }
                other => panic!("unsupported P06 lifecycle map presence {other}"),
            };
            resident_transitions.push(LifecycleResidentTransition {
                resident,
                stage,
                client_heightmaps,
            });
        }
        assert!(saw_source, "P06 lifecycle event omits its source resident");
        assert!(saw_target, "P06 lifecycle event omits its target resident");
        assert!(
            resident_transitions.iter().any(|transition| {
                transition.resident == source
                    && transition.stage >= LifecycleResidentStage::Features
            }),
            "P06 source did not reach FEATURES",
        );
        events.push(LifecycleReplayEvent {
            source,
            stage: LifecycleCompletion::Features,
            sequence,
            resident_transitions,
        });
    }
    assert_eq!(seen_sources, expected_sources, "P06 lifecycle stream omits a source");
    assert_eq!(cursor, bytes.len(), "P06 lifecycle event payload has trailing bytes");
    events
}

fn read_frame(
    reader: &mut BufReader<File>,
    complete: &std::path::Path,
    format: u16,
) -> Option<Frame> {
    fn read_when_available(reader: &mut BufReader<File>, bytes: &mut [u8], complete: &std::path::Path) {
        let mut filled = 0;
        while filled < bytes.len() {
            match reader.read(&mut bytes[filled..]) {
                Ok(0) if complete.exists() => panic!("stream completed with a truncated frame"),
                Ok(0) => thread::sleep(Duration::from_millis(10)),
                Ok(count) => filled += count,
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => panic!("read stream frame: {error}"),
            }
        }
    }

    let mut length = [0u8; 4];
    read_when_available(reader, &mut length, complete);
    let length = u32::from_be_bytes(length);
    if length == 0 { return None; }
    assert!(length <= MAX_FRAME_BYTES, "stream frame is too large: {length}");
    let minimum = if format == STREAM_FORMAT_END_P06_LIFECYCLE {
        8 + 4 + 4 + 4 + 4 + DIGEST_BYTES + DIGEST_BYTES
    } else {
        8 + 4 + 4 + 4 + DIGEST_BYTES
    };
    assert!(length as usize >= minimum, "stream frame is truncated");
    let mut body = vec![0u8; length as usize];
    read_when_available(reader, &mut body, complete);
    let (record, digest, lifecycle_events) = if format == STREAM_FORMAT_END_P06_LIFECYCLE {
        let event_len = u32_at(&body, 16) as usize;
        let record_len = u32_at(&body, 20) as usize;
        let payload_start = 88usize;
        let expected_len = payload_start
            .checked_add(event_len)
            .and_then(|offset| offset.checked_add(record_len))
            .expect("P06 lifecycle frame length overflow");
        assert_eq!(body.len(), expected_len, "P06 lifecycle frame lengths");
        let event_digest: [u8; DIGEST_BYTES] = body[24..56].try_into().unwrap();
        let digest: [u8; DIGEST_BYTES] = body[56..88].try_into().unwrap();
        let event_payload = &body[payload_start..payload_start + event_len];
        let record = body[payload_start + event_len..].to_vec();
        assert_eq!(
            support::large_parity_manifest::sha256(event_payload),
            event_digest,
            "P06 lifecycle event digest",
        );
        assert_eq!(
            support::large_parity_manifest::sha256(&record),
            digest,
            "P06 lifecycle frame record digest",
        );
        (
            record,
            digest,
            parse_end_p06_lifecycle_events(event_payload, i32_at(&body, 8), i32_at(&body, 12)),
        )
    } else {
        let record_len = u32_at(&body, 16) as usize;
        assert_eq!(
            body.len(),
            8 + 4 + 4 + 4 + DIGEST_BYTES + record_len,
            "stream frame record length",
        );
        let digest: [u8; DIGEST_BYTES] = body[20..52].try_into().unwrap();
        let record = body[52..].to_vec();
        assert_eq!(
            support::large_parity_manifest::sha256(&record),
            digest,
            "stream frame record digest",
        );
        (record, digest, Vec::new())
    };
    Some(Frame {
        index: u64_at(&body, 0),
        cx: i32_at(&body, 8),
        cz: i32_at(&body, 12),
        digest,
        record,
        lifecycle_events,
    })
}

enum OrderedLifecycleMaterializer {
    Overworld(LifecycleMaterializer<OverworldChunkSource>),
    Nether(LifecycleMaterializer<NetherChunkSource>),
    End(LifecycleMaterializer<lodestone_server::EndChunkSource>),
}

fn lifecycle_admissions(targets: &[(i32, i32)]) -> Vec<(i32, i32)> {
    let min_x = targets.iter().map(|&(x, _)| x).min().expect("non-empty target batch");
    let max_x = targets.iter().map(|&(x, _)| x).max().expect("non-empty target batch");
    let min_z = targets.iter().map(|&(_, z)| z).min().expect("non-empty target batch");
    let max_z = targets.iter().map(|&(_, z)| z).max().expect("non-empty target batch");
    (min_z - NETHER_FEATURE_WRITE_RADIUS..=max_z + NETHER_FEATURE_WRITE_RADIUS)
        .flat_map(|z| {
            (min_x - NETHER_FEATURE_WRITE_RADIUS..=max_x + NETHER_FEATURE_WRITE_RADIUS)
                .map(move |x| (x, z))
        })
        .collect()
}

/// Source-ordered fallback completions follow each requested chunk's dependency
/// square with x changing by column and z changing fastest. This is distinct
/// from the z-major/x-fastest order used to emit packet records.
fn lifecycle_completion_wavefront(targets: &[(i32, i32)]) -> Vec<((i32, i32), (i32, i32))> {
    let mut order = Vec::new();
    for &(target_x, target_z) in targets {
        for source_x in
            target_x - NETHER_FEATURE_SOURCE_RADIUS..=target_x + NETHER_FEATURE_SOURCE_RADIUS
        {
            for source_z in
                target_z - NETHER_FEATURE_SOURCE_RADIUS..=target_z + NETHER_FEATURE_SOURCE_RADIUS
            {
                order.push(((target_x, target_z), (source_x, source_z)));
            }
        }
    }
    order
}

fn stream_feature_completion_order(
    dispatch: LifecycleFeatureDispatch,
    targets: &[(i32, i32)],
) -> Vec<((i32, i32), (i32, i32))> {
    match dispatch {
        LifecycleFeatureDispatch::TargetOwned => targets
            .iter()
            .copied()
            .map(|target| (target, target))
            .collect(),
        LifecycleFeatureDispatch::SourceOrdered => lifecycle_completion_wavefront(targets),
    }
}

#[derive(Default)]
struct StreamLifecycleState {
    /// The live oracle leaves dependency chunks resident after removing the
    /// requested centre ticket. Keep that state across bounded frame batches.
    admitted: BTreeSet<(i32, i32)>,
    /// Source-ordered fallback bodies are retained after their first completion
    /// when a later target reaches the same source through an overlapping halo.
    completed: BTreeSet<(i32, i32)>,
}

type EndP06LifecycleEvents = BTreeMap<(i32, i32), Vec<LifecycleReplayEvent>>;

fn lifecycle_columns(
    dimension: StreamDimension,
    targets: &[(i32, i32)],
    materializer: Option<&mut OrderedLifecycleMaterializer>,
    state: &mut StreamLifecycleState,
    end_events: Option<&EndP06LifecycleEvents>,
) -> Vec<ChunkColumn> {
    let timings = matches!(std::env::var("LODESTONE_LARGE_PARITY_STREAM_TIMINGS").as_deref(), Ok("1" | "true" | "yes" | "on"));
    let started = timings.then(Instant::now);
    fn run<S: LifecycleWorldgenSource + Sync>(
        materializer: &mut LifecycleMaterializer<S>,
        dimension: StreamDimension,
        targets: &[(i32, i32)],
        timings: bool,
        persistent: bool,
        state: &mut StreamLifecycleState,
        end_events: Option<&EndP06LifecycleEvents>,
    ) -> Vec<ChunkColumn> {
        let admissions = lifecycle_admissions(targets);
        if !persistent {
            materializer.reset_for_lifecycle_replay();
            state.admitted.clear();
            state.completed.clear();
        }
        let prepare_started = Instant::now();
        materializer.prepare_lifecycle_replay(&admissions);
        let prepare_ms = prepare_started.elapsed().as_millis();
        let admit_started = Instant::now();
        let new_admissions = admissions
            .iter()
            .copied()
            .filter(|chunk| state.admitted.insert(*chunk))
            .collect::<Vec<_>>();
        materializer.admit_many_parallel(&new_admissions);
        let admit_ms = admit_started.elapsed().as_millis();
        let complete_started = Instant::now();
        let dispatch = materializer.feature_dispatch();
        if dispatch == LifecycleFeatureDispatch::TargetOwned {
            assert_ne!(dimension, StreamDimension::End, "End lifecycle events are source-ordered");
            for (sequence, (target, source)) in
                stream_feature_completion_order(dispatch, targets).into_iter().enumerate()
            {
                assert_eq!(target, source, "target-owned FEATURES must name their target");
                materializer.complete_target_features_observing(target, sequence as u64, |_| {});
                materializer.finish_target(target);
            }
        } else {
            let completion_order = if dimension == StreamDimension::End {
                targets
                    .iter()
                    .flat_map(|target| {
                        end_events
                            .and_then(|events| events.get(target))
                            .unwrap_or_else(|| panic!("P06 End stream has no lifecycle events for target {target:?}"))
                            .iter()
                            .map(move |event| (*target, event.source))
                    })
                    .collect::<Vec<_>>()
            } else {
                stream_feature_completion_order(dispatch, targets)
            };
            let target_scoped = dimension != StreamDimension::End;
            let mut active_target = None;
            for (sequence, (target, source)) in completion_order.into_iter().enumerate() {
                if target_scoped && active_target != Some(target) {
                    if let Some(previous) = active_target {
                        materializer.finish_target(previous);
                    }
                    materializer.begin_target(target);
                    active_target = Some(target);
                }
                let should_complete = state.completed.insert(source);
                if should_complete {
                    if dimension == StreamDimension::End {
                        let event = end_events
                            .and_then(|events| events.get(&target))
                            .and_then(|events| events.iter().find(|event| event.source == source))
                            .unwrap_or_else(|| panic!("P06 End stream has no event for target {target:?}, source {source:?}"));
                        materializer.complete_observing_with_residents(
                            event.source,
                            event.stage,
                            event.sequence,
                            &event.resident_transitions,
                            |_| {},
                        );
                    } else {
                        materializer.complete_for_target(
                            target,
                            source,
                            LifecycleCompletion::Features,
                            sequence as u64,
                        );
                    }
                }
            }
            if target_scoped {
                if let Some(target) = active_target {
                    materializer.finish_target(target);
                }
            }
        }
        let complete_ms = complete_started.elapsed().as_millis();
        let columns: Vec<ChunkColumn> = targets
            .iter()
            .copied()
            .map(|target| materializer.snapshot_for_packet(target))
            .collect();
        if timings { eprintln!("stream timings: prepare={}ms admit={}ms complete={}ms", prepare_ms, admit_ms, complete_ms); }
        columns
    }

    let materializer = materializer.expect("ordered lifecycle materializer for non-End stream");
    // The external stream keeps generated columns resident after a centre
    // ticket is removed. End batches remain independent because their packet
    // stream uses a wider batch and its captured events own the replay state.
    let persistent = dimension != StreamDimension::End;
    let columns = match materializer {
        OrderedLifecycleMaterializer::Overworld(materializer) => {
            run(materializer, dimension, targets, timings, persistent, state, end_events)
        }
        OrderedLifecycleMaterializer::Nether(materializer) => {
            run(materializer, dimension, targets, timings, persistent, state, end_events)
        }
        OrderedLifecycleMaterializer::End(materializer) => {
            run(materializer, dimension, targets, timings, false, state, end_events)
        }
    };
    if let Some(started) = started { eprintln!("stream timings: {:?} batch={}ms", dimension, started.elapsed().as_millis()); }
    columns
}

fn end_p06_packet_payload(
    materializer: &mut LifecycleMaterializer<EndChunkSource>,
    target: (i32, i32),
) -> Vec<u8> {
    let column = materializer.snapshot_for_packet(target);
    // The external P06 capture requests each target with a radius-zero ticket.
    // Its initial packet therefore contains the target's own light field; the
    // neighbouring CARVERS columns admitted for lifecycle replay are not light
    // inputs until their own packets or a later seam update.
    let neighbours: [(i32, i32, ChunkColumn); 0] = [];
    let directive = V770ServerProtocol
        .try_encode_chunk_with_neighbours_in_dimension(
            target.0,
            target.1,
            &column,
            &neighbours,
            ServerDimension::End,
        )
        .expect("End P06 source-aware packet encoding");
    match directive {
        ServerDirective::Send { packet_id, payload } => {
            assert_eq!(
                packet_id,
                lodestone_v26_2::packet_ids::play::clientbound::LEVEL_CHUNK_WITH_LIGHT,
            );
            payload
        }
        other => panic!("End P06 packet encoder returned {other:?} at {target:?}"),
    }
}

#[test]
fn ordered_stream_admits_the_complete_feature_halo_but_emits_targets_only() {
    let targets = [(0, 0), (1, 0), (0, 1), (1, 1)];
    let admissions = lifecycle_admissions(&targets);
    assert_eq!(admissions.first(), Some(&(-2, -2)));
    assert_eq!(admissions.last(), Some(&(3, 3)));
    assert_eq!(admissions.len(), 36);
    assert!(admissions.iter().position(|&chunk| chunk == (0, -2))
        < admissions.iter().position(|&chunk| chunk == (0, 0)));
    assert_eq!(targets.len(), 4);
}

#[test]
fn completion_wavefront_matches_the_independent_three_by_three_fixture() {
    let order = lifecycle_completion_wavefront(&[(0, -1)]);
    let expected = (-1..=1)
        .flat_map(|x| (-2..=0).map(move |z| ((0, -1), (x, z))))
        .collect::<Vec<_>>();
    assert_eq!(order, expected);

    assert!(order.iter().position(|&(_, source)| source == (0, -1))
        < order.iter().position(|&(_, source)| source == (0, 0)));
}

#[test]
fn completion_wavefront_reuses_dependencies_between_adjacent_requests() {
    let order = lifecycle_completion_wavefront(&[(0, 0), (1, 0)]);
    assert_eq!(order.len(), 18);
    assert_eq!(&order[..3], &[((0, 0), (-1, -1)), ((0, 0), (-1, 0)), ((0, 0), (-1, 1))]);
    assert_eq!(&order[9..12], &[((1, 0), (0, -1)), ((1, 0), (0, 0)), ((1, 0), (0, 1))]);
}

#[test]
fn stream_feature_dispatch_routes_target_owned_and_source_ordered_fallback() {
    let targets = [(0, 0), (1, 0)];
    assert_eq!(
        stream_feature_completion_order(LifecycleFeatureDispatch::TargetOwned, &targets),
        vec![((0, 0), (0, 0)), ((1, 0), (1, 0))],
    );
    assert_eq!(
        stream_feature_completion_order(LifecycleFeatureDispatch::SourceOrdered, &targets),
        lifecycle_completion_wavefront(&targets),
    );
    assert_eq!(
        LifecycleMaterializer::new(overworld_chunk_source(SEED)).feature_dispatch(),
        LifecycleFeatureDispatch::TargetOwned,
    );
    assert_eq!(
        LifecycleMaterializer::new(nether_chunk_source(SEED)).feature_dispatch(),
        LifecycleFeatureDispatch::SourceOrdered,
    );
    assert_eq!(
        LifecycleMaterializer::new(end_chunk_source(SEED)).feature_dispatch(),
        LifecycleFeatureDispatch::SourceOrdered,
    );
}

#[test]
fn nether_target_completion_replays_the_captured_source_wavefront() {
    let target = (380, 380);
    let mut materializer = LifecycleMaterializer::new(nether_chunk_source(SEED));
    for admission in lifecycle_admissions(&[target]) {
        materializer.admit(admission);
    }
    for (sequence, (target, source)) in lifecycle_completion_wavefront(&[target]).into_iter().enumerate() {
        materializer.complete_for_target(target, source, LifecycleCompletion::Features, sequence as u64);
    }
    materializer.finish_target(target);

    assert_eq!(
        materializer
            .snapshot_for_packet(target)
            .block_state_id(8, 50, 1)
            .name(),
        "minecraft:crimson_roots",
        "the captured source wavefront must retain the target's external root witness",
    );
    assert_eq!(
        materializer
            .snapshot_for_packet(target)
            .block_state_id(15, 77, 5)
            .name(),
        "minecraft:netherrack",
        "the first captured packet must not inherit the later east-neighbour root",
    );
}

#[test]
fn stream_state_deduplicates_completed_targets() {
    let mut state = StreamLifecycleState::default();
    let first = lifecycle_completion_wavefront(&[(0, 0)]);
    for &(_, source) in &first {
        assert!(state.completed.insert(source));
    }
    let second = lifecycle_completion_wavefront(&[(1, 0)]);
    let unseen = second
        .into_iter()
        .filter(|&(_, source)| state.completed.insert(source))
        .collect::<Vec<_>>();
    assert_eq!(
        unseen,
        vec![
            ((1, 0), (2, -1)),
            ((1, 0), (2, 0)),
            ((1, 0), (2, 1)),
        ],
    );
}

#[test]
fn nether_stream_replays_prior_targets_before_magma_gravel_target() {
    let targets = (2..=3)
        .flat_map(|z| (2..=5).map(move |x| (x, z)))
        .collect::<Vec<_>>();
    let mut materializer = Some(OrderedLifecycleMaterializer::Nether(
        LifecycleMaterializer::new(nether_chunk_source(SEED)),
    ));
    let mut state = StreamLifecycleState::default();
    let mut observed = None;
    for target in targets {
        let columns = lifecycle_columns(
            StreamDimension::Nether,
            &[target],
            materializer.as_mut(),
            &mut state,
            None,
        );
        if target == (5, 3) {
            observed = Some(columns[0].block_state_id(15, 32, 15).name());
        }
    }

    assert_eq!(
        observed.as_deref(),
        Some("minecraft:magma_block"),
        "the eighth row-major target must retain the magma spill from its earlier source completion",
    );
}

fn diagnostics_enabled() -> bool {
    matches!(std::env::var("LODESTONE_LARGE_PARITY_STREAM_DIAGNOSTICS").as_deref(), Ok("1" | "true" | "yes" | "on"))
}

fn defer_heightmaps() -> bool {
    matches!(std::env::var("LODESTONE_LARGE_PARITY_STREAM_DEFER_HEIGHTMAPS").as_deref(), Ok("1" | "true" | "yes" | "on"))
}

fn scan_all_enabled() -> bool {
    matches!(std::env::var("LODESTONE_LARGE_PARITY_STREAM_SCAN_ALL").as_deref(), Ok("1" | "true" | "yes" | "on"))
        || std::env::var_os("LODESTONE_LARGE_PARITY_STREAM_ARTIFACT_DIR")
            .is_some_and(|path| !path.is_empty())
}

fn first_difference_offset(expected: &[u8], actual: &[u8]) -> Option<usize> {
    let common = expected.len().min(actual.len());
    expected[..common]
        .iter()
        .zip(&actual[..common])
        .position(|(left, right)| left != right)
        .or_else(|| (expected.len() != actual.len()).then_some(common))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LightFreeRecordLayout {
    heightmaps: std::ops::Range<usize>,
    terrain: std::ops::Range<usize>,
    biomes: std::ops::Range<usize>,
    block_entities: std::ops::RangeFrom<usize>,
    heightmap_count: usize,
    section_count: usize,
}

fn checked_u32_at(bytes: &[u8], offset: usize) -> Option<u32> {
    Some(u32::from_be_bytes(bytes.get(offset..offset.checked_add(4)?)?.try_into().ok()?))
}

fn light_free_record_layout(record: &[u8], dimension: StreamDimension) -> Option<LightFreeRecordLayout> {
    let domain = b"lodestone.worldgen.large-parity.chunk/v7/light-free";
    let mut cursor = domain.len();
    if record.get(..cursor)? != domain { return None; }
    cursor = cursor.checked_add(8)?;
    let dimension_name = dimension.name();
    let dimension_end = cursor.checked_add(dimension_name.len())?;
    if record.get(cursor..dimension_end)? != dimension_name { return None; }
    cursor = dimension_end;

    let heightmap_count = checked_u32_at(record, cursor)? as usize;
    cursor = cursor.checked_add(4)?;
    let heightmaps_start = cursor;
    cursor = cursor.checked_add(heightmap_count.checked_mul(4 + 256 * 4)?)?;
    if cursor > record.len() { return None; }
    let heightmaps = heightmaps_start..cursor;

    let section_count = checked_u32_at(record, cursor)? as usize;
    cursor = cursor.checked_add(4)?;
    let section_payload_start = cursor;
    let terrain_bytes = section_count.checked_mul(4096 * 4)?;
    let biome_bytes = section_count.checked_mul(64 * 4)?;
    cursor = cursor.checked_add(terrain_bytes)?.checked_add(biome_bytes)?;
    if cursor > record.len() { return None; }
    // Each section stores 4096 states immediately followed by its 64 biomes,
    // so these ranges describe the whole interleaved section payload. Semantic
    // cell helpers apply the per-section stride rather than treating all
    // terrain and all biomes as separate contiguous arrays.
    let terrain = section_payload_start..cursor;
    let biomes = section_payload_start..cursor;
    checked_u32_at(record, cursor)?;

    Some(LightFreeRecordLayout {
        heightmaps,
        terrain,
        biomes,
        block_entities: cursor..,
        heightmap_count,
        section_count,
    })
}

fn records_match_without_heightmaps(expected: &[u8], actual: &[u8], dimension: StreamDimension) -> bool {
    let (Some(expected_layout), Some(actual_layout)) = (
        light_free_record_layout(expected, dimension),
        light_free_record_layout(actual, dimension),
    ) else { return false; };
    expected_layout.heightmap_count == actual_layout.heightmap_count
        && expected.get(..expected_layout.heightmaps.start) == actual.get(..actual_layout.heightmaps.start)
        && expected.get(expected_layout.heightmaps.end..) == actual.get(actual_layout.heightmaps.end..)
}

fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

fn diagnose_end_raw_packet(expected: &[u8], actual: &[u8]) {
    let decode = |payload: &[u8]| {
        let mut reader = Reader::new(payload);
        let packet = LevelChunkWithLight::decode(
            &mut reader,
            &ChunkShape::nether_or_end_1_21(),
        )
        .expect("raw End packet must decode for diagnostics");
        reader.ensure_empty().expect("raw End packet has no trailing bytes");
        packet
    };
    let expected = decode(expected);
    let actual = decode(actual);
    eprintln!(
        "P06 decoded packet coords: expected=({}, {}) actual=({}, {})",
        expected.x, expected.z, actual.x, actual.z,
    );
    for type_id in [1, 4, 5] {
        let expected_map = expected
            .heightmaps
            .get(type_id)
            .expect("expected End packet heightmap");
        let actual_map = actual
            .heightmaps
            .get(type_id)
            .expect("actual End packet heightmap");
        let first = (0..16).flat_map(|z| (0..16).map(move |x| (x, z))).find_map(|(x, z)| {
            let left = expected_map.get(x, z);
            let right = actual_map.get(x, z);
            (left != right).then_some((x, z, left, right))
        });
        eprintln!("P06 heightmap type={type_id} first_diff={first:?}");
    }
    let mut block_differences = 0usize;
    let mut first_block_difference = None;
    for y in 0..256 {
        for z in 0..16 {
            for x in 0..16 {
                let left = expected.column.get_block(x, y, z);
                let right = actual.column.get_block(x, y, z);
                if left != right {
                    block_differences += 1;
                    first_block_difference.get_or_insert((x, y, z, left, right));
                }
            }
        }
    }
    let mut biome_differences = 0usize;
    let mut first_biome_difference = None;
    for y in (0..256).step_by(4) {
        for z in 0..4 {
            for x in 0..4 {
                let left = expected.column.get_biome(x, y, z);
                let right = actual.column.get_biome(x, y, z);
                if left != right {
                    biome_differences += 1;
                    first_biome_difference.get_or_insert((x, y, z, left, right));
                }
            }
        }
    }
    eprintln!(
        "P06 decoded columns: block_differences={block_differences} first_block={first_block_difference:?} biome_differences={biome_differences} first_biome={first_biome_difference:?} block_entities=({}, {}) light_equal={}",
        expected.block_entities.len(),
        actual.block_entities.len(),
        expected.light == actual.light,
    );
    let light_shape = |data: &lodestone_world::LightData| match data {
        lodestone_world::LightData::Missing => "missing".to_owned(),
        lodestone_world::LightData::Uniform(value) => format!("uniform({value})"),
        lodestone_world::LightData::Values(values) => {
            let first = values.get(0);
            let nonzero = values.as_bytes().iter().filter(|&&byte| byte != 0).count();
            format!("values(first={first}, nonzero_bytes={nonzero})")
        }
    };
    for section in 0..expected.light.light_section_count() {
        let sky_expected = light_shape(expected.light.sky(section));
        let sky_actual = light_shape(actual.light.sky(section));
        let block_expected = light_shape(expected.light.block(section));
        let block_actual = light_shape(actual.light.block(section));
        if sky_expected != sky_actual || block_expected != block_actual {
            eprintln!(
                "P06 light section={section}: sky expected={sky_expected} actual={sky_actual}; block expected={block_expected} actual={block_actual}"
            );
            if expected.light.sky(section) != actual.light.sky(section) {
                let mut shown = 0;
                for cell in 0..4096 {
                    let x = cell % 16;
                    let z = (cell / 16) % 16;
                    let y = cell / 256;
                    let left = expected.light.section_light(section).sky_at(x, y, z);
                    let right = actual.light.section_light(section).sky_at(x, y, z);
                    if left != right {
                        eprintln!("P06 sky diff section={section} local=({x},{y},{z}) expected={left} actual={right}");
                        shown += 1;
                        if shown == 8 { break; }
                    }
                }
            }
            for (kind, data) in [("sky", actual.light.sky(section)), ("block", actual.light.block(section))] {
                let Some(values) = (match data { lodestone_world::LightData::Values(values) => Some(values), _ => None }) else { continue };
                let mut shown = 0;
                for cell in 0..4096 {
                    let value = values.get(cell);
                    if value == 0 { continue; }
                    let x = cell % 16;
                    let z = (cell / 16) % 16;
                    let y = cell / 256;
                    let world_y = (section.saturating_sub(1) * 16 + y) as i32;
                    let state = actual.column.get_block(x, world_y, z);
                    let emission = lodestone_data::block_states::StateId::new(state)
                        .map(lodestone_data::light_props::emission);
                    let state_name = lodestone_data::block_states::StateId::new(state)
                        .map(|state| state.canonical_state())
                        .unwrap_or_else(|| "unknown".to_owned());
                    eprintln!("P06 light nonzero kind={kind} section={section} local=({x},{y},{z}) world_y={world_y} value={value} state={state_name} emission={emission:?}");
                    shown += 1;
                    if shown == 4 { break; }
                }
            }
        }
    }
}

fn mismatch_component(record: &[u8], offset: usize, dimension: StreamDimension) -> &'static str {
    let Some(layout) = light_free_record_layout(record, dimension) else { return "malformed"; };
    if offset < layout.heightmaps.start { "header" }
    else if offset < layout.heightmaps.end { "heightmaps" }
    else if offset < layout.terrain.start { "section_count" }
    else if offset < layout.terrain.end {
        let within_section = (offset - layout.terrain.start) % ((4096 + 64) * 4);
        if within_section < 4096 * 4 { "terrain" } else { "biomes" }
    }
    else { "block_entities" }
}

fn packet_difference(expected: &[u8], actual: &[u8]) -> ComponentDifference {
    let offset = first_difference_offset(expected, actual).unwrap_or(0);
    let identity = |bytes: &[u8]| {
        bytes
            .get(offset)
            .map_or_else(|| "eof".to_owned(), |value| format!("byte:{:02x}", *value))
    };
    ComponentDifference {
        component: "packet",
        expected_identity: format!("offset:{offset}:{}:len:{}", identity(expected), expected.len()),
        actual_identity: format!("offset:{offset}:{}:len:{}", identity(actual), actual.len()),
        count: u64::from(first_difference_offset(expected, actual).is_some()),
        bounds: DifferenceBounds::default(),
    }
}

fn heightmap_difference(expected: &Heightmaps, actual: &Heightmaps) -> Option<ComponentDifference> {
    let ids = expected
        .iter()
        .map(|(id, _)| id)
        .chain(actual.iter().map(|(id, _)| id))
        .collect::<BTreeSet<_>>();
    let mut count = 0u64;
    let mut bounds = DifferenceBounds::default();
    let mut identities = None;
    for id in ids {
        match (expected.get(id), actual.get(id)) {
            (Some(left), Some(right)) => {
                for z in 0..16 {
                    for x in 0..16 {
                        let expected_value = left.get(x, z);
                        let actual_value = right.get(x, z);
                        if expected_value == actual_value {
                            continue;
                        }
                        count += 1;
                        bounds.observe_xz(x as i32, z as i32);
                        identities.get_or_insert((
                            format!("map:{id}:height:{expected_value}"),
                            format!("map:{id}:height:{actual_value}"),
                        ));
                    }
                }
            }
            (Some(_), None) => {
                count += 256;
                bounds.observe_xz(0, 0);
                bounds.observe_xz(15, 15);
                identities.get_or_insert((format!("map:{id}:present"), format!("map:{id}:missing")));
            }
            (None, Some(_)) => {
                count += 256;
                bounds.observe_xz(0, 0);
                bounds.observe_xz(15, 15);
                identities.get_or_insert((format!("map:{id}:missing"), format!("map:{id}:present")));
            }
            (None, None) => unreachable!("heightmap union contains an absent key"),
        }
    }
    let (expected_identity, actual_identity) = identities?;
    Some(ComponentDifference {
        component: "heightmap",
        expected_identity,
        actual_identity,
        count,
        bounds,
    })
}

fn column_differences(expected: &WorldChunkColumn, actual: &WorldChunkColumn) -> Vec<ComponentDifference> {
    let mut differences = Vec::new();
    let min_y = expected.min_y();
    let max_y = expected.max_y().min(actual.max_y());
    let mut count = 0u64;
    let mut bounds = DifferenceBounds::default();
    let mut identities = None;
    for y in min_y..max_y {
        for z in 0..16 {
            for x in 0..16 {
                let expected_value = expected.get_block(x, y, z);
                let actual_value = actual.get_block(x, y, z);
                if expected_value == actual_value {
                    continue;
                }
                count += 1;
                bounds.observe(x as i32, y, z as i32);
                identities.get_or_insert((
                    format!("state:{expected_value}"),
                    format!("state:{actual_value}"),
                ));
            }
        }
    }
    if let Some((expected_identity, actual_identity)) = identities {
        differences.push(ComponentDifference {
            component: "terrain",
            expected_identity,
            actual_identity,
            count,
            bounds,
        });
    }

    let mut count = 0u64;
    let mut bounds = DifferenceBounds::default();
    let mut identities = None;
    for y in (min_y..max_y).step_by(4) {
        for z in 0..4 {
            for x in 0..4 {
                let expected_value = expected.get_biome(x, y, z);
                let actual_value = actual.get_biome(x, y, z);
                if expected_value == actual_value {
                    continue;
                }
                count += 1;
                bounds.observe(x as i32, y, z as i32);
                identities.get_or_insert((
                    format!("biome:{expected_value}"),
                    format!("biome:{actual_value}"),
                ));
            }
        }
    }
    if let Some((expected_identity, actual_identity)) = identities {
        differences.push(ComponentDifference {
            component: "biome",
            expected_identity,
            actual_identity,
            count,
            bounds,
        });
    }
    differences
}

fn block_entity_difference(expected: &[lodestone_world::BlockEntity], actual: &[lodestone_world::BlockEntity]) -> ComponentDifference {
    let count = expected
        .iter()
        .zip(actual)
        .filter(|(left, right)| left != right)
        .count()
        + expected.len().abs_diff(actual.len());
    let mut bounds = DifferenceBounds::default();
    for (left, right) in expected.iter().zip(actual) {
        if left != right {
            bounds.observe(left.rel_x as i32, left.y as i32, left.rel_z as i32);
            bounds.observe(right.rel_x as i32, right.y as i32, right.rel_z as i32);
        }
    }
    if expected.len() > actual.len() {
        for entity in &expected[actual.len()..] {
            bounds.observe(entity.rel_x as i32, entity.y as i32, entity.rel_z as i32);
        }
    } else if actual.len() > expected.len() {
        for entity in &actual[expected.len()..] {
            bounds.observe(entity.rel_x as i32, entity.y as i32, entity.rel_z as i32);
        }
    }
    ComponentDifference {
        component: "block-entity",
        expected_identity: format!("digest:{}", hex(&support::large_parity_manifest::sha256(format!("{:?}", expected).as_bytes()))),
        actual_identity: format!("digest:{}", hex(&support::large_parity_manifest::sha256(format!("{:?}", actual).as_bytes()))),
        count: count as u64,
        bounds,
    }
}

fn light_data_identity(data: &lodestone_world::LightData) -> String {
    match data {
        lodestone_world::LightData::Missing => "missing".to_owned(),
        lodestone_world::LightData::Uniform(value) => format!("uniform:{value}"),
        lodestone_world::LightData::Values(values) => format!(
            "values:{}",
            hex(&support::large_parity_manifest::sha256(values.as_bytes())),
        ),
    }
}

fn light_data_at(column: &lodestone_world::ColumnLight, section: usize, sky: bool) -> Option<&lodestone_world::LightData> {
    (section < column.light_section_count()).then(|| if sky { column.sky(section) } else { column.block(section) })
}

fn optional_light_identity(data: Option<&lodestone_world::LightData>) -> String {
    data.map_or_else(|| "missing".to_owned(), light_data_identity)
}

fn sidecar_difference(expected: &lodestone_world::ColumnLight, actual: &lodestone_world::ColumnLight) -> ComponentDifference {
    let mut count = 0u64;
    let mut bounds = DifferenceBounds::default();
    let mut identities = None;
    let section_count = expected.light_section_count().max(actual.light_section_count());
    for section in 0..section_count {
        let expected_sky = light_data_at(expected, section, true);
        let actual_sky = light_data_at(actual, section, true);
        if expected_sky != actual_sky {
            count += 1;
            bounds.observe_y(section as i32);
            identities.get_or_insert((
                format!("sky:{}", optional_light_identity(expected_sky)),
                format!("sky:{}", optional_light_identity(actual_sky)),
            ));
        }
        let expected_block = light_data_at(expected, section, false);
        let actual_block = light_data_at(actual, section, false);
        if expected_block != actual_block {
            count += 1;
            bounds.observe_y(section as i32);
            identities.get_or_insert((
                format!("block:{}", optional_light_identity(expected_block)),
                format!("block:{}", optional_light_identity(actual_block)),
            ));
        }
    }
    if count == 0 && expected.storage() != actual.storage() {
        count = 1;
        identities = Some((
            format!("storage:{:?}", expected.storage()),
            format!("storage:{:?}", actual.storage()),
        ));
    }
    let (expected_identity, actual_identity) = identities.unwrap_or_else(|| ("unknown".to_owned(), "unknown".to_owned()));
    ComponentDifference {
        component: "sidecar",
        expected_identity,
        actual_identity,
        count,
        bounds,
    }
}

fn light_free_differences(expected: &[u8], actual: &[u8], dimension: StreamDimension) -> Vec<ComponentDifference> {
    let (Some(expected_layout), Some(actual_layout)) = (
        light_free_record_layout(expected, dimension),
        light_free_record_layout(actual, dimension),
    ) else {
        return vec![packet_difference(expected, actual)];
    };
    let mut differences = Vec::new();
    if expected.get(..expected_layout.heightmaps.start) != actual.get(..actual_layout.heightmaps.start) {
        differences.push(packet_difference(expected, actual));
    }

    let expected_heightmaps = &expected[expected_layout.heightmaps.clone()];
    let actual_heightmaps = &actual[actual_layout.heightmaps.clone()];
    if expected_heightmaps != actual_heightmaps {
        let mut count = 0u64;
        let mut bounds = DifferenceBounds::default();
        let mut identities = None;
        for entry in 0..expected_layout.heightmap_count.min(actual_layout.heightmap_count) {
            let expected_offset = expected_layout.heightmaps.start + entry * (4 + 256 * 4);
            let actual_offset = actual_layout.heightmaps.start + entry * (4 + 256 * 4);
            let expected_id = u32_at(expected, expected_offset);
            let actual_id = u32_at(actual, actual_offset);
            for cell in 0..256 {
                let expected_value = u32_at(expected, expected_offset + 4 + cell * 4);
                let actual_value = u32_at(actual, actual_offset + 4 + cell * 4);
                if expected_value == actual_value && expected_id == actual_id {
                    continue;
                }
                count += 1;
                bounds.observe_xz((cell % 16) as i32, (cell / 16) as i32);
                identities.get_or_insert((
                    format!("map:{expected_id}:height:{expected_value}"),
                    format!("map:{actual_id}:height:{actual_value}"),
                ));
            }
        }
        count += (expected_layout.heightmap_count.abs_diff(actual_layout.heightmap_count) * 256) as u64;
        if count > 0 {
            if expected_layout.heightmap_count != actual_layout.heightmap_count {
                bounds.observe_xz(0, 0);
                bounds.observe_xz(15, 15);
            }
            let (expected_identity, actual_identity) = identities.unwrap_or_else(|| (
                format!("maps:{}", expected_layout.heightmap_count),
                format!("maps:{}", actual_layout.heightmap_count),
            ));
            differences.push(ComponentDifference {
                component: "heightmap",
                expected_identity,
                actual_identity,
                count,
                bounds,
            });
        }
    }

    if expected_layout.section_count != actual_layout.section_count {
        differences.push(ComponentDifference {
            component: "packet",
            expected_identity: format!("sections:{}", expected_layout.section_count),
            actual_identity: format!("sections:{}", actual_layout.section_count),
            count: 1,
            bounds: DifferenceBounds::default(),
        });
    } else if expected_layout.terrain.start == actual_layout.terrain.start {
        let min_y = match dimension {
            StreamDimension::Overworld => -64,
            StreamDimension::Nether | StreamDimension::End => 0,
        };
        let expected_base = expected_layout.terrain.start;
        let actual_base = actual_layout.terrain.start;
        let mut terrain_count = 0u64;
        let mut terrain_bounds = DifferenceBounds::default();
        let mut terrain_identities = None;
        let mut biome_count = 0u64;
        let mut biome_bounds = DifferenceBounds::default();
        let mut biome_identities = None;
        for section in 0..expected_layout.section_count {
            let expected_section = expected_base + section * (4096 + 64) * 4;
            let actual_section = actual_base + section * (4096 + 64) * 4;
            for cell in 0..4096 {
                let expected_value = u32_at(expected, expected_section + cell * 4);
                let actual_value = u32_at(actual, actual_section + cell * 4);
                if expected_value == actual_value {
                    continue;
                }
                terrain_count += 1;
                terrain_bounds.observe(
                    (cell % 16) as i32,
                    min_y + (section * 16 + cell / 256) as i32,
                    (cell / 16 % 16) as i32,
                );
                terrain_identities.get_or_insert((
                    format!("state:{expected_value}"),
                    format!("state:{actual_value}"),
                ));
            }
            for cell in 0..64 {
                let expected_value = u32_at(expected, expected_section + (4096 + cell) * 4);
                let actual_value = u32_at(actual, actual_section + (4096 + cell) * 4);
                if expected_value == actual_value {
                    continue;
                }
                biome_count += 1;
                biome_bounds.observe(
                    (cell % 4) as i32,
                    min_y + (section * 16 + cell / 16 * 4) as i32,
                    (cell / 16) as i32,
                );
                biome_identities.get_or_insert((
                    format!("biome:{expected_value}"),
                    format!("biome:{actual_value}"),
                ));
            }
        }
        if let Some((expected_identity, actual_identity)) = terrain_identities {
            differences.push(ComponentDifference {
                component: "terrain",
                expected_identity,
                actual_identity,
                count: terrain_count,
                bounds: terrain_bounds,
            });
        }
        if let Some((expected_identity, actual_identity)) = biome_identities {
            differences.push(ComponentDifference {
                component: "biome",
                expected_identity,
                actual_identity,
                count: biome_count,
                bounds: biome_bounds,
            });
        }

        if expected.get(expected_layout.block_entities.start..)
            != actual.get(actual_layout.block_entities.start..)
        {
            let expected_entities = &expected[expected_layout.block_entities.start..];
            let actual_entities = &actual[actual_layout.block_entities.start..];
            differences.push(ComponentDifference {
                component: "block-entity",
                expected_identity: format!("digest:{}", hex(&support::large_parity_manifest::sha256(expected_entities))),
                actual_identity: format!("digest:{}", hex(&support::large_parity_manifest::sha256(actual_entities))),
                count: 1,
                bounds: DifferenceBounds::default(),
            });
        }
    } else {
        differences.push(packet_difference(expected, actual));
    }
    if differences.is_empty() {
        differences.push(packet_difference(expected, actual));
    }
    differences
}

fn raw_packet_differences(expected_bytes: &[u8], actual_bytes: &[u8]) -> Vec<ComponentDifference> {
    let decode = |payload: &[u8]| {
        let mut reader = Reader::new(payload);
        let packet = LevelChunkWithLight::decode(
            &mut reader,
            &ChunkShape::nether_or_end_1_21(),
        )
        .ok()?;
        reader.ensure_empty().ok()?;
        Some(packet)
    };
    let (Some(expected), Some(actual)) = (decode(expected_bytes), decode(actual_bytes)) else {
        return vec![packet_difference(expected_bytes, actual_bytes)];
    };
    let mut differences = Vec::new();
    if expected.x != actual.x || expected.z != actual.z {
        differences.push(ComponentDifference {
            component: "packet",
            expected_identity: format!("coordinate:({}, {})", expected.x, expected.z),
            actual_identity: format!("coordinate:({}, {})", actual.x, actual.z),
            count: 1,
            bounds: DifferenceBounds::default(),
        });
    }
    if let Some(difference) = heightmap_difference(&expected.heightmaps, &actual.heightmaps) {
        differences.push(difference);
    } else if expected.heightmaps != actual.heightmaps {
        differences.push(packet_difference(expected_bytes, actual_bytes));
    }
    if expected.column != actual.column {
        let column_differences = column_differences(&expected.column, &actual.column);
        if column_differences.is_empty() {
            differences.push(packet_difference(expected_bytes, actual_bytes));
        } else {
            differences.extend(column_differences);
        }
    }
    if expected.block_entities != actual.block_entities {
        differences.push(block_entity_difference(&expected.block_entities, &actual.block_entities));
    }
    if expected.light != actual.light {
        differences.push(sidecar_difference(&expected.light, &actual.light));
    }
    if differences.is_empty() {
        differences.push(packet_difference(expected_bytes, actual_bytes));
    }
    differences
}

fn mismatch_differences(
    format: u16,
    expected: &[u8],
    actual: &[u8],
    dimension: StreamDimension,
) -> Vec<ComponentDifference> {
    if format == STREAM_FORMAT_END_P06_LIFECYCLE {
        raw_packet_differences(expected, actual)
    } else {
        light_free_differences(expected, actual, dimension)
    }
}

fn mismatch_category(
    format: u16,
    expected: &[u8],
    actual: &[u8],
    dimension: StreamDimension,
) -> &'static str {
    mismatch_differences(format, expected, actual, dimension)
        .first()
        .map_or("packet", |difference| difference.component)
}

fn first_terrain_difference(
    expected: &[u8],
    actual: &[u8],
    dimension: StreamDimension,
) -> Option<(usize, i32, i32, i32, u32, u32)> {
    let expected_layout = light_free_record_layout(expected, dimension)?;
    let actual_layout = light_free_record_layout(actual, dimension)?;
    if expected_layout.section_count != actual_layout.section_count { return None; }
    let min_y = match dimension {
        StreamDimension::Overworld => -64,
        StreamDimension::Nether | StreamDimension::End => 0,
    };
    for cell in 0..expected_layout.section_count * 4096 {
        let section = cell / 4096;
        let section_cell = cell % 4096;
        let expected_offset = expected_layout.terrain.start + section * (4096 + 64) * 4 + section_cell * 4;
        let actual_offset = actual_layout.terrain.start + section * (4096 + 64) * 4 + section_cell * 4;
        let expected_id = u32_at(expected, expected_offset);
        let actual_id = u32_at(actual, actual_offset);
        if expected_id == actual_id { continue; }
        let y = min_y + (cell / 4096 * 16 + section_cell / 256) as i32;
        let z = (section_cell / 16 % 16) as i32;
        let x = (section_cell % 16) as i32;
        return Some((expected_offset, x, y, z, expected_id, actual_id));
    }
    None
}

fn first_terrain_difference_from_column(
    expected: &[u8],
    column: &ChunkColumn,
    dimension: StreamDimension,
) -> Option<(usize, i32, i32, i32, u32, u32)> {
    let expected_layout = light_free_record_layout(expected, dimension)?;
    if expected_layout.section_count != column.section_count() { return None; }
    let min_y = match dimension {
        StreamDimension::Overworld => -64,
        StreamDimension::Nether | StreamDimension::End => 0,
    };
    for cell in 0..expected_layout.section_count * 4096 {
        let section = cell / 4096;
        let section_cell = cell % 4096;
        let expected_offset = expected_layout.terrain.start + section * (4096 + 64) * 4 + section_cell * 4;
        let expected_id = u32_at(expected, expected_offset);
        let y = min_y + (cell / 4096 * 16 + section_cell / 256) as i32;
        let z = (section_cell / 16 % 16) as i32;
        let x = (section_cell % 16) as i32;
        let actual_id = column.block_state_id(x, y, z).raw();
        if expected_id != actual_id {
            return Some((expected_offset, x, y, z, expected_id, actual_id));
        }
    }
    None
}

fn first_heightmap_difference(expected: &[u8], actual: &[u8], dimension: StreamDimension)
    -> Option<(usize, u32, i32, i32, u32, u32)>
{
    let expected_layout = light_free_record_layout(expected, dimension)?;
    let actual_layout = light_free_record_layout(actual, dimension)?;
    if expected_layout.heightmap_count != actual_layout.heightmap_count { return None; }
    let mut expected_cursor = expected_layout.heightmaps.start;
    let mut actual_cursor = actual_layout.heightmaps.start;
    for _ in 0..expected_layout.heightmap_count {
        let type_id = u32_at(expected, expected_cursor);
        let actual_type_id = u32_at(actual, actual_cursor);
        if type_id != actual_type_id {
            return Some((expected_cursor, type_id, -1, -1, type_id, actual_type_id));
        }
        expected_cursor += 4;
        actual_cursor += 4;
        for cell in 0..256usize {
            let expected_offset = expected_cursor + cell * 4;
            let actual_offset = actual_cursor + cell * 4;
            let expected_height = u32_at(expected, expected_offset);
            let actual_height = u32_at(actual, actual_offset);
            if expected_height != actual_height {
                return Some((expected_offset, type_id, (cell % 16) as i32, (cell / 16) as i32, expected_height, actual_height));
            }
        }
        expected_cursor += 256 * 4;
        actual_cursor += 256 * 4;
    }
    None
}

#[cfg(test)]
fn literal_v7_record(
    dimension: StreamDimension,
    heightmap_ids: &[u32],
    section_count: usize,
) -> Vec<u8> {
    let mut record = Vec::new();
    record.extend_from_slice(b"lodestone.worldgen.large-parity.chunk/v7/light-free");
    record.extend_from_slice(&17i32.to_be_bytes());
    record.extend_from_slice(&(-23i32).to_be_bytes());
    record.extend_from_slice(dimension.name());
    record.extend_from_slice(&(heightmap_ids.len() as u32).to_be_bytes());
    for &id in heightmap_ids {
        record.extend_from_slice(&id.to_be_bytes());
        for cell in 0..256u32 { record.extend_from_slice(&(cell + id * 1_000).to_be_bytes()); }
    }
    record.extend_from_slice(&(section_count as u32).to_be_bytes());
    for section in 0..section_count {
        for y in 0..16u32 {
            for z in 0..16u32 {
                for x in 0..16u32 {
                    let cell = (y * 16 + z) * 16 + x;
                    record.extend_from_slice(&(100_000 + section as u32 * 4096 + cell).to_be_bytes());
                }
            }
        }
        for cell in 0..64u32 {
            record.extend_from_slice(&(200_000 + section as u32 * 64 + cell).to_be_bytes());
        }
    }
    record.extend_from_slice(&0u32.to_be_bytes());
    record
}

#[test]
fn v7_diagnostics_parse_each_literal_record_independently() {
    for dimension in [StreamDimension::Overworld, StreamDimension::Nether, StreamDimension::End] {
        let expected = literal_v7_record(dimension, &[1, 4, 5], 3);
        let mut actual = literal_v7_record(dimension, &[1, 4, 5], 3);
        let layout = light_free_record_layout(&expected, dimension).expect("literal v7 layout");
        assert_eq!(
            layout.heightmaps.start,
            b"lodestone.worldgen.large-parity.chunk/v7/light-free".len()
                + 8
                + dimension.name().len()
                + 4,
        );
        assert_eq!(layout.terrain.len(), 3 * (4096 + 64) * 4);
        assert_eq!(layout.biomes, layout.terrain);
        assert_eq!(layout.block_entities.start + 4, expected.len());

        let cell = 2 * 4096 + 7 * 256 + 11 * 16 + 13;
        let actual_offset = layout.terrain.start + 2 * (4096 + 64) * 4 + (cell % 4096) * 4;
        actual[actual_offset..actual_offset + 4].copy_from_slice(&999_999u32.to_be_bytes());
        assert_eq!(
            first_terrain_difference(&expected, &actual, dimension),
            Some((actual_offset, 13, match dimension { StreamDimension::Overworld => -25, _ => 39 }, 11, 100_000 + cell as u32, 999_999)),
        );
        assert_eq!(mismatch_component(&expected, actual_offset, dimension), "terrain");
        assert_eq!(mismatch_category(STREAM_FORMAT_LIGHT_FREE, &expected, &actual, dimension), "terrain");
        let differences = mismatch_differences(STREAM_FORMAT_LIGHT_FREE, &expected, &actual, dimension);
        assert_eq!(differences.len(), 1);
        assert_eq!(differences[0].component, "terrain");
        assert_eq!(differences[0].count, 1);
        assert_eq!(differences[0].expected_identity, format!("state:{}", 100_000 + cell as u32));
        assert_eq!(differences[0].actual_identity, "state:999999");
        assert_eq!(differences[0].bounds.text(), match dimension {
            StreamDimension::Overworld => "13..13:-25..-25:11..11",
            StreamDimension::Nether | StreamDimension::End => "13..13:39..39:11..11",
        });
    }

    let expected = literal_v7_record(StreamDimension::Nether, &[1], 2);
    let mut actual = literal_v7_record(StreamDimension::Nether, &[1, 4], 2);
    let actual_layout = light_free_record_layout(&actual, StreamDimension::Nether).unwrap();
    let cell = 3 * 256 + 2 * 16 + 1;
    let offset = actual_layout.terrain.start + cell * 4;
    actual[offset..offset + 4].copy_from_slice(&888_888u32.to_be_bytes());
    assert_eq!(
        first_terrain_difference(&expected, &actual, StreamDimension::Nether),
        Some((light_free_record_layout(&expected, StreamDimension::Nether).unwrap().terrain.start + cell * 4, 1, 3, 2, 100_000 + cell as u32, 888_888)),
    );
    assert!(!records_match_without_heightmaps(&expected, &actual, StreamDimension::Nether));
}

#[test]
fn light_free_scan_classifies_all_changed_components_in_one_pair() {
    let expected = literal_v7_record(StreamDimension::Nether, &[1], 1);
    let mut actual = expected.clone();
    let layout = light_free_record_layout(&expected, StreamDimension::Nether).expect("literal layout");
    actual[layout.heightmaps.start + 4..layout.heightmaps.start + 8]
        .copy_from_slice(&999_999u32.to_be_bytes());
    actual[layout.terrain.start..layout.terrain.start + 4]
        .copy_from_slice(&888_888u32.to_be_bytes());
    actual[layout.terrain.start + 4096 * 4..layout.terrain.start + 4096 * 4 + 4]
        .copy_from_slice(&777_777u32.to_be_bytes());
    actual.extend_from_slice(&[1, 2, 3]);

    let differences = light_free_differences(&expected, &actual, StreamDimension::Nether);
    let components = differences.iter().map(|difference| difference.component).collect::<BTreeSet<_>>();
    assert_eq!(components, BTreeSet::from(["terrain", "biome", "heightmap", "block-entity"]));
    assert!(differences.iter().all(|difference| difference.count > 0));
}

fn scan_test_difference(component: &'static str, expected: &str, actual: &str, count: u64) -> ComponentDifference {
    let mut bounds = DifferenceBounds::default();
    bounds.observe(2, 7, 3);
    ComponentDifference {
        component,
        expected_identity: expected.to_owned(),
        actual_identity: actual.to_owned(),
        count,
        bounds,
    }
}

static SCAN_TEST_SEQUENCE: AtomicU64 = AtomicU64::new(0);

fn scan_test_dir() -> PathBuf {
    std::env::temp_dir().join(format!(
        "lodestone-stream-scan-{}-{}-{}",
        std::process::id(),
        SCAN_TEST_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("test clock")
            .as_nanos(),
    ))
}

#[test]
fn scan_artifacts_group_causal_signatures_and_count_each_component() {
    let dir = scan_test_dir();
    let header = StreamHeader {
        cx0: 0,
        cx1: 2,
        cz0: 0,
        cz1: 0,
        count: 3,
        start: 0,
        format: STREAM_FORMAT_LIGHT_FREE,
        dimension: StreamDimension::Nether,
        payload_digest: [0; DIGEST_BYTES],
    };
    let mut artifacts = ScanArtifacts::new(dir.clone(), &[0; HEADER_BYTES], header).expect("scan artifacts");
    let terrain = scan_test_difference("terrain", "state:1", "state:2", 1);
    for index in 0..2u64 {
        let frame = Frame {
            index,
            cx: index as i32,
            cz: 0,
            digest: support::large_parity_manifest::sha256(b"expected"),
            record: b"expected".to_vec(),
            lifecycle_events: Vec::new(),
        };
        artifacts.record_pair(&frame, b"actual", std::slice::from_ref(&terrain));
    }
    let frame = Frame {
        index: 2,
        cx: 2,
        cz: 0,
        digest: support::large_parity_manifest::sha256(b"expected-2"),
        record: b"expected-2".to_vec(),
        lifecycle_events: Vec::new(),
    };
    let biome = scan_test_difference("biome", "biome:3", "biome:4", 2);
    artifacts.record_pair(&frame, b"actual-2", &[terrain, biome]);
    artifacts.finish(3);

    let summary = fs::read_to_string(dir.join("summary.txt")).expect("scan summary");
    assert!(summary.contains("mismatches=3\n"));
    assert!(summary.contains("component.terrain=3\n"));
    assert!(summary.contains("component.biome=1\n"));
    assert!(summary.contains("signature_groups=2\n"));
    let groups = fs::read_to_string(dir.join("signature-groups.tsv")).expect("scan groups");
    assert!(groups.contains("terrain\tstate:1\tstate:2\t1\t2..2:7..7:3..3\t3\t3\n"));
    assert!(groups.contains("biome\tbiome:3\tbiome:4\t2\t2..2:7..7:3..3\t1\t1\n"));
    let inventory = fs::read_to_string(dir.join("mismatch-inventory.tsv")).expect("scan inventory");
    assert!(inventory.contains("terrain\t"));
    assert!(inventory.contains("biome\t"));
    assert!(inventory.contains("2..2:7..7:3..3"));
    fs::remove_dir_all(dir).expect("remove scan test directory");
}

#[test]
fn scan_artifacts_keep_exact_counts_after_retention_caps() {
    let dir = scan_test_dir();
    let total = MAX_SCAN_COORDINATE_PAIRS + 10;
    let header = StreamHeader {
        cx0: 0,
        cx1: total as i32 - 1,
        cz0: 0,
        cz1: 0,
        count: total as u64,
        start: 0,
        format: STREAM_FORMAT_LIGHT_FREE,
        dimension: StreamDimension::Nether,
        payload_digest: [0; DIGEST_BYTES],
    };
    let mut artifacts = ScanArtifacts::new(dir.clone(), &[0; HEADER_BYTES], header).expect("scan artifacts");
    for index in 0..total as u64 {
        let frame = Frame {
            index,
            cx: index as i32,
            cz: 0,
            digest: support::large_parity_manifest::sha256(b"expected"),
            record: b"expected".to_vec(),
            lifecycle_events: Vec::new(),
        };
        let actual = format!("actual-{index}").into_bytes();
        let difference = scan_test_difference(
            "terrain",
            &format!("state:{index}"),
            &format!("state:{}", index + 10_000),
            1,
        );
        artifacts.record_pair(&frame, &actual, std::slice::from_ref(&difference));
    }
    artifacts.finish(total as u64);

    let summary = fs::read_to_string(dir.join("summary.txt")).expect("scan summary");
    assert!(summary.contains(&format!("compared={total}\n")));
    assert!(summary.contains(&format!("mismatches={total}\n")));
    assert!(summary.contains(&format!("component.terrain={total}\n")));
    assert!(summary.contains("signature_groups=64\n"));
    assert!(summary.contains(&format!("signature_records={total}\n")));
    assert!(summary.contains(&format!(
        "omitted_signature_group_records={}\n",
        total - MAX_SCAN_SIGNATURE_GROUPS,
    )));
    assert!(summary.contains(&format!("retained_coordinate_pairs={}\n", MAX_SCAN_COORDINATE_PAIRS)));
    assert_eq!(fs::read(dir.join("stream-header.bin")).expect("scan header"), vec![0; HEADER_BYTES]);
    assert!(fs::read(dir.join("representative-records.bin")).expect("scan evidence").starts_with(SCAN_EVIDENCE_MAGIC));
    assert_eq!(fs::read_to_string(dir.join("mismatch-inventory.tsv")).expect("scan inventory").lines().count(), total + 1);
    fs::remove_dir_all(dir).expect("remove scan test directory");
}

#[test]
fn scan_artifacts_refuse_nonempty_explicit_directory() {
    let dir = scan_test_dir();
    fs::create_dir_all(&dir).expect("create explicit scan directory");
    fs::write(dir.join("prior-run"), b"retained evidence").expect("seed explicit scan directory");
    let header = StreamHeader {
        cx0: 0,
        cx1: 0,
        cz0: 0,
        cz1: 0,
        count: 1,
        start: 0,
        format: STREAM_FORMAT_LIGHT_FREE,
        dimension: StreamDimension::Nether,
        payload_digest: [0; DIGEST_BYTES],
    };
    let result = ScanArtifacts::new(dir.clone(), &[0; HEADER_BYTES], header);
    assert!(result.is_err(), "existing explicit evidence must not be overwritten");
    assert_eq!(fs::read(dir.join("prior-run")).expect("retained evidence"), b"retained evidence");
    fs::remove_dir_all(dir).expect("remove explicit scan directory");
}

struct EndP06ControlSource;

impl LifecycleWorldgenSource for EndP06ControlSource {
    type ReplayContext = ();

    fn lifecycle_replay_context(
        &self,
        _target: (i32, i32),
    ) -> std::sync::Arc<Self::ReplayContext> {
        std::sync::Arc::new(())
    }

    fn shaped_column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        ChunkColumn::new(0, 256)
    }

    fn feature_result(
        &self,
        _source: (i32, i32),
        _overrides: &BTreeMap<(i32, i32, i32), lodestone_data::block_states::StateId>,
        _resident: &BTreeMap<(i32, i32), ChunkColumn>,
    ) -> LifecycleFeatureResult {
        LifecycleFeatureResult::default()
    }
}

fn p06_put_i32(bytes: &mut Vec<u8>, value: i32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn p06_put_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn p06_put_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn p06_put_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_be_bytes());
}

fn literal_end_p06_lifecycle_payload(
    target: (i32, i32),
    target_focus: [u16; 3],
    reverse: bool,
) -> Vec<u8> {
    let mut sources = (-1..=1)
        .flat_map(|x| (-1..=1).map(move |z| (target.0 + x, target.1 + z)))
        .collect::<Vec<_>>();
    if reverse {
        sources.reverse();
    }
    let mut bytes = END_STREAM_EVENT_DOMAIN.to_vec();
    p06_put_u32(&mut bytes, sources.len() as u32);
    for (sequence, source) in sources.into_iter().enumerate() {
        p06_put_u64(&mut bytes, sequence as u64);
        p06_put_i32(&mut bytes, source.0);
        p06_put_i32(&mut bytes, source.1);
        bytes.push(1); // FEATURES completion
        p06_put_u32(&mut bytes, u32::from(source != target) + 1);
        let mut write_transition = |resident: (i32, i32), values: [u16; 3]| {
            p06_put_i32(&mut bytes, resident.0);
            p06_put_i32(&mut bytes, resident.1);
            bytes.push(1); // FEATURES resident status
            bytes.push(1); // map payload present
            for map in 0..3 {
                for cell in 0..256 {
                    let value = if resident == target && cell == 2 {
                        values[map]
                    } else {
                        58
                    };
                    p06_put_u16(&mut bytes, value);
                }
            }
        };
        write_transition(source, if source == target { target_focus } else { [58; 3] });
        if source != target {
            write_transition(target, target_focus);
        }
    }
    bytes
}

fn replay_literal_end_p06_events(
    events: &[LifecycleReplayEvent],
) -> (u32, u32, u32) {
    let target = (280, 78);
    let mut materializer = LifecycleMaterializer::new(EndP06ControlSource);
    for z in target.1 - 2..=target.1 + 2 {
        for x in target.0 - 2..=target.0 + 2 {
            materializer.admit((x, z));
        }
    }
    for event in events {
        materializer.complete_observing_with_residents(
            event.source,
            event.stage,
            event.sequence,
            &event.resident_transitions,
            |_| {},
        );
    }
    let maps = materializer
        .resident_column(target)
        .expect("P06 control target admission")
        .client_heightmaps()
        .expect("P06 control target maps");
    (
        maps.get(1).expect("WORLD_SURFACE map").get(2, 0),
        maps.get(4).expect("MOTION_BLOCKING map").get(2, 0),
        maps.get(5).expect("MOTION_BLOCKING_NO_LEAVES map").get(2, 0),
    )
}

#[test]
fn end_p06_parser_replays_authenticated_canonical_56_and_negative_58_maps() {
    let target = (280, 78);
    let canonical = literal_end_p06_lifecycle_payload(target, [68, 56, 56], false);
    let canonical_events = parse_end_p06_lifecycle_events(&canonical, target.0, target.1);
    assert_eq!(
        replay_literal_end_p06_events(&canonical_events),
        (68, 56, 56),
        "canonical P06 resident transition keeps raw motion heightmaps at 56",
    );

    let reverse_value = literal_end_p06_lifecycle_payload(target, [68, 58, 58], false);
    let reverse_value_events = parse_end_p06_lifecycle_events(&reverse_value, target.0, target.1);
    assert_eq!(
        replay_literal_end_p06_events(&reverse_value_events),
        (68, 58, 58),
        "the negative control preserves an authenticated raw 58 value rather than reconstructing it",
    );
}

#[test]
#[should_panic(expected = "End P06 lifecycle source order")]
fn end_p06_parser_rejects_reverse_resident_admission_order() {
    let target = (280, 78);
    let reversed = literal_end_p06_lifecycle_payload(target, [68, 58, 58], true);
    parse_end_p06_lifecycle_events(&reversed, target.0, target.1);
}

#[test]
#[ignore = "requires scripts/worldgen-oracle/stream-parity.sh and the external 26.2 oracle"]
fn stream_external_oracle_matches_lodestone() {
    let expected_dimension = StreamDimension::parse();
    let source_started = Instant::now();
    let mut materializer = match expected_dimension {
        StreamDimension::Overworld => Some(OrderedLifecycleMaterializer::Overworld(LifecycleMaterializer::new(overworld_chunk_source(SEED)))),
        StreamDimension::Nether => Some(OrderedLifecycleMaterializer::Nether(LifecycleMaterializer::new(nether_chunk_source(SEED)))),
        StreamDimension::End => Some(OrderedLifecycleMaterializer::End(LifecycleMaterializer::new(end_chunk_source(SEED)))),
    };
    if matches!(std::env::var("LODESTONE_LARGE_PARITY_STREAM_TIMINGS").as_deref(), Ok("1" | "true" | "yes" | "on")) {
        eprintln!("stream timings: source_init={}ms", source_started.elapsed().as_millis());
    }
    let fifo = std::env::var_os("LODESTONE_LARGE_PARITY_STREAM_FIFO").expect("set LODESTONE_LARGE_PARITY_STREAM_FIFO");
    let fifo = PathBuf::from(fifo);
    let complete = PathBuf::from(
        std::env::var_os("LODESTONE_LARGE_PARITY_STREAM_COMPLETE")
            .expect("set LODESTONE_LARGE_PARITY_STREAM_COMPLETE"),
    );
    let stream_file = File::open(&fifo).unwrap_or_else(|error| panic!("open stream {}: {error}", fifo.display()));
    let mut reader = BufReader::new(stream_file);
    let mut raw_header = [0u8; HEADER_BYTES];
    reader.read_exact(&mut raw_header).expect("read stream provenance header");
    let stream_header = parse_stream_header(&raw_header);
    assert_eq!(stream_header.dimension, expected_dimension, "stream dimension selection");
    assert_eq!(
        stream_header.format,
        if stream_header.dimension == StreamDimension::End {
            STREAM_FORMAT_END_P06_LIFECYCLE
        } else {
            STREAM_FORMAT_LIGHT_FREE
        },
        "stream format for selected dimension",
    );
    assert_eq!(stream_header.start, 0, "ephemeral stream has no persistent resume cursor");
    assert_eq!(stream_header.payload_digest, [0; DIGEST_BYTES], "stream payload checksum is reserved");
    let width = u64::try_from(i64::from(stream_header.cx1) - i64::from(stream_header.cx0) + 1).unwrap();
    assert_eq!(stream_header.cz0 + ((stream_header.count - 1) / width) as i32, stream_header.cz1, "stream height matches count");
    let scan_all = scan_all_enabled();
    let mut artifacts = scan_all.then(|| {
        ScanArtifacts::new(selected_scan_artifact_dir(), &raw_header, stream_header)
            .unwrap_or_else(|error| panic!("initialize scan artifacts: {error}"))
    });
    let mut compared = 0u64;
    let mut mismatch_count = 0u64;
    let mut lifecycle_state = StreamLifecycleState::default();
    let default_batch_size = match stream_header.dimension {
        StreamDimension::End => DEFAULT_END_STREAM_BATCH_SIZE,
        StreamDimension::Overworld | StreamDimension::Nether => DEFAULT_ORDERED_STREAM_BATCH_SIZE,
    };
    let batch_size = std::env::var("LODESTONE_LARGE_PARITY_STREAM_BATCH_SIZE")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .filter(|&value| value != 0)
        .unwrap_or(default_batch_size);
    let mut pending = Vec::with_capacity(batch_size);
    loop {
        match read_frame(&mut reader, &complete, stream_header.format) {
            Some(frame) => pending.push(frame),
            None => {
                if pending.is_empty() { break; }
            }
        }
        if pending.len() < batch_size && compared + (pending.len() as u64) < stream_header.count { continue; }
        let coordinates = pending.iter().map(|frame| (frame.cx, frame.cz)).collect::<Vec<_>>();
        let end_events = if stream_header.dimension == StreamDimension::End {
            let mut events = EndP06LifecycleEvents::new();
            for frame in &pending {
                assert!(
                    events.insert((frame.cx, frame.cz), frame.lifecycle_events.clone()).is_none(),
                    "duplicate P06 End lifecycle frame coordinate",
                );
            }
            Some(events)
        } else {
            None
        };
        let actual_columns = lifecycle_columns(
            stream_header.dimension,
            &coordinates,
            materializer.as_mut(),
            &mut lifecycle_state,
            end_events.as_ref(),
        );
        for (frame, column) in pending.drain(..).zip(actual_columns) {
            let index = compared;
            assert_eq!(frame.index, index, "stream frame index");
            let expected_cx = stream_header.cx0 + (index % width) as i32;
            let expected_cz = stream_header.cz0 + (index / width) as i32;
            assert_eq!((frame.cx, frame.cz), (expected_cx, expected_cz), "stream coordinate order");
            let actual = if stream_header.format == STREAM_FORMAT_END_P06_LIFECYCLE {
                let end_materializer = match materializer.as_mut().expect("End materializer") {
                    OrderedLifecycleMaterializer::End(materializer) => materializer,
                    _ => panic!("P06 End stream selected a non-End materializer"),
                };
                end_p06_packet_payload(end_materializer, (frame.cx, frame.cz))
            } else {
                support::large_parity_manifest::light_free_record(
                    &column,
                    frame.cx,
                    frame.cz,
                    stream_header.dimension.manifest_dimension(),
                )
            };
            let actual_digest = support::large_parity_manifest::sha256(&actual);
            if actual_digest != frame.digest {
                mismatch_count += 1;
                let differences = (scan_all || diagnostics_enabled()).then(|| {
                    mismatch_differences(
                        stream_header.format,
                        &frame.record,
                        &actual,
                        stream_header.dimension,
                    )
                });
                let component = differences
                    .as_ref()
                    .and_then(|differences| differences.first())
                    .map_or("packet", |difference| difference.component);
                if let Some(artifacts) = artifacts.as_mut() {
                    artifacts.record_pair(
                        &frame,
                        &actual,
                        differences.as_deref().expect("scan mismatch differences"),
                    );
                }
                if scan_all {
                    if stream_header.format == STREAM_FORMAT_LIGHT_FREE
                        && defer_heightmaps()
                        && records_match_without_heightmaps(&frame.record, &actual, stream_header.dimension)
                    {
                        eprintln!("stream parity: deferred heightmap-only mismatch at ({},{})", frame.cx, frame.cz);
                    } else {
                        eprintln!(
                            "stream parity: mismatch {} at ({},{}) component={} expected={} actual={}",
                            mismatch_count,
                            frame.cx,
                            frame.cz,
                            component,
                            hex(&frame.digest),
                            hex(&actual_digest),
                        );
                    }
                    compared += 1;
                    continue;
                }
                if stream_header.format == STREAM_FORMAT_LIGHT_FREE
                    && defer_heightmaps()
                    && records_match_without_heightmaps(&frame.record, &actual, stream_header.dimension)
                {
                    eprintln!("stream parity: deferred heightmap-only mismatch at ({},{})", frame.cx, frame.cz);
                    compared += 1;
                    continue;
                }
                if diagnostics_enabled() {
                    let first = frame.record.iter().zip(&actual).position(|(left, right)| left != right);
                    let kind = if stream_header.format == STREAM_FORMAT_END_P06_LIFECYCLE {
                        "P06 packet"
                    } else {
                        "stream payload"
                    };
                    if stream_header.format == STREAM_FORMAT_END_P06_LIFECYCLE {
                        eprintln!(
                            "P06 packet mismatch at ({},{}): expected_len={} actual_len={} first_diff={first:?} expected={} actual={}",
                            frame.cx,
                            frame.cz,
                            frame.record.len(),
                            actual.len(),
                            hex(&frame.digest),
                            hex(&actual_digest),
                        );
                    }
                    if stream_header.format != STREAM_FORMAT_LIGHT_FREE {
                        eprintln!(
                            "{kind} first differing bytes: expected={:?} actual={:?}",
                            frame.record.get(first.unwrap_or(0)..first.unwrap_or(0).saturating_add(16)),
                            actual.get(first.unwrap_or(0)..first.unwrap_or(0).saturating_add(16)),
                        );
                    }
                    if stream_header.format != STREAM_FORMAT_LIGHT_FREE {
                        // The packet path has no light-free component parser;
                        // keep the authenticated byte offset visible for a
                        // bounded raw-packet control without dumping shaders
                        // or the complete payload.
                        if let Some(first) = first {
                            eprintln!("{kind} first differing byte offset={first}");
                        }
                        if stream_header.format == STREAM_FORMAT_END_P06_LIFECYCLE {
                            diagnose_end_raw_packet(&frame.record, &actual);
                        }
                    }
                    eprintln!("stream mismatch at ({},{}), component={component}, first differing byte {:?}, expected={}, actual={}", frame.cx, frame.cz, first, hex(&frame.digest), hex(&actual_digest));
                    if component == "terrain" {
                        eprintln!("first terrain cell: {:?}", first_terrain_difference(&frame.record, &actual, stream_header.dimension));
                        eprintln!("first terrain cell from column: {:?}", first_terrain_difference_from_column(&frame.record, &column, stream_header.dimension));
                        if let Some((_, _, _, _, expected_id, actual_id)) =
                            first_terrain_difference_from_column(&frame.record, &column, stream_header.dimension)
                        {
                            eprintln!(
                                "first terrain names: expected={:?} actual={:?}",
                                lodestone_data::block_states::StateId::new(expected_id)
                                    .map(lodestone_data::block_states::StateId::canonical_state),
                                lodestone_data::block_states::StateId::new(actual_id)
                                    .map(lodestone_data::block_states::StateId::canonical_state),
                            );
                        }
                    }
                    if component == "heightmaps" {
                        eprintln!("first heightmap cell: {:?}", first_heightmap_difference(&frame.record, &actual, stream_header.dimension));
                        if let Some((_, _, x, z, expected_height, actual_height)) =
                            first_heightmap_difference(&frame.record, &actual, stream_header.dimension)
                        {
                            let low = expected_height.min(actual_height).saturating_sub(1) as i32;
                            let high = expected_height.max(actual_height) as i32;
                            let states = (low..=high)
                                .map(|y| (y, column.block_state_id(x, y, z).canonical_state()))
                                .collect::<Vec<_>>();
                            eprintln!("Lodestone heightmap column states: {states:?}");
                        }
                    }
                    if component == "block_entities" {
                        eprintln!("Lodestone block entities: {:?}", column.block_entities());
                    }
                    if component == "heightmaps" {
                        eprintln!("first terrain cell beyond heightmaps: {:?}", first_terrain_difference(&frame.record, &actual, stream_header.dimension));
                        eprintln!("first terrain cell from column: {:?}", first_terrain_difference_from_column(&frame.record, &column, stream_header.dimension));
                    }
                }
                let kind = if stream_header.format == STREAM_FORMAT_END_P06_LIFECYCLE {
                    "P06 packet"
                } else {
                    "light-free content"
                };
                panic!("stream {kind} parity mismatch at ({},{}) index {}", frame.cx, frame.cz, index);
            }
            compared += 1;
            if compared % 256 == 0 || compared == stream_header.count { eprintln!("stream parity: compared {compared}/{} chunks", stream_header.count); }
        }
        if compared == stream_header.count { break; }
    }
    assert_eq!(compared, stream_header.count, "stream ended before target count");
    if let Some(artifacts) = artifacts {
        artifacts.finish(compared);
    }
    assert_eq!(mismatch_count, 0, "stream parity scan found {mismatch_count} mismatching chunks");
}
