//! Initial Nether light keeps its admission boundary explicit.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use lodestone_server::dimension::Dimension;
use lodestone_server::{
    retained_chunk_source_for_view_radius, ChunkColumn, ChunkSource, RetainedLightStatus,
    ServerProtocol,
};
use lodestone_v26_2::packets::chunk::ChunkShape;
use lodestone_v26_2::V770ServerProtocol;
use lodestone_world::{ColumnLight, LightData, LightStorage, NibbleArray};

fn block_at(light: &lodestone_world::ColumnLight, y: i32, x: usize, z: usize) -> u8 {
    let section = ((y + 16) / 16) as usize;
    let local_y = (y + 16).rem_euclid(16) as usize;
    light.section_light(section).block_at(x, local_y, z)
}

#[test]
fn initial_nether_admits_cardinal_sources_but_defers_diagonal_sources() {
    let shape = ChunkShape::nether_or_end_1_21();
    let center = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    let proto = V770ServerProtocol;

    let mut west = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    west.set_block(15, 82, 13, "minecraft:glowstone");
    let with_west = proto
        .compute_initial_column_light_with_neighbours_in_dimension(
            &center,
            &[(-1, 0, west)],
            Dimension::Nether,
        )
        .expect("initial Nether light with cardinal source");

    let without_west = proto
        .compute_initial_column_light_with_neighbours_in_dimension(
            &center,
            &[],
            Dimension::Nether,
        )
        .expect("initial Nether light without cardinal source");
    assert_eq!(block_at(&without_west, 82, 0, 13), 0);
    assert_eq!(block_at(&with_west, 82, 0, 13), 14);

    let mut north_west = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    north_west.set_block(15, 82, 15, "minecraft:glowstone");
    let with_diagonal = proto
        .compute_initial_column_light_with_neighbours_in_dimension(
            &center,
            &[(-1, -1, north_west)],
            Dimension::Nether,
        )
        .expect("initial Nether light with diagonal source");
    assert_eq!(block_at(&with_diagonal, 82, 0, 0), 0);
}

#[test]
fn retained_diagonal_light_crosses_only_admitted_bridges() {
    let shape = ChunkShape::nether_or_end_1_21();
    let center = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    let north_west = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    let north = ChunkColumn::new(shape.min_y, shape.world_height as i32);
    let proto = V770ServerProtocol;
    let with_bridge = proto.compute_initial_column_light_with_neighbours_seeded(
        &center,
        &[(-1, -1, north_west), (0, -1, north)],
        Dimension::Nether,
        |dx, dz, x, y, z| {
            if (dx, dz, x, y, z) == (-1, -1, 15, 82, 15) {
                15
            } else {
                0
            }
        },
    );
    assert_eq!(
        block_at(&with_bridge, 82, 0, 0),
        13,
        "retained light must cross only the columns admitted to this snapshot"
    );

    let without_bridge = proto.compute_initial_column_light_with_neighbours_seeded(
        &center,
        &[(-1, -1, ChunkColumn::new(shape.min_y, shape.world_height as i32))],
        Dimension::Nether,
        |dx, dz, x, y, z| {
            if (dx, dz, x, y, z) == (-1, -1, 15, 82, 15) {
                15
            } else {
                0
            }
        },
    );
    assert_eq!(
        block_at(&without_bridge, 82, 0, 0),
        0,
        "a missing bridge must remain an opaque seam"
    );
}

#[test]
fn plural_admission_commits_center_and_dependency_readiness_atomically() {
    let source = retained_chunk_source_for_view_radius(lodestone_server::nether_chunk_source(42), 8);
    let center = source.column(0, 0);
    let protocol = V770ServerProtocol;
    let mut compute = |centre: &ChunkColumn,
                       neighbours: &[(i32, i32, ChunkColumn)]| {
        protocol.compute_initial_column_lights_with_neighbours_in_dimension(
            centre,
            neighbours,
            Dimension::Nether,
        )
    };
    source
        .settle_resident_column_lights_with_neighbours(
            0,
            0,
            &center,
            &[(-1, -1), (0, -1), (1, -1), (-1, 0), (1, 0), (-1, 1), (0, 1), (1, 1)],
            false,
            false,
            true,
            &mut compute,
        )
        .expect("production plural light admission");

    for &(dx, dz) in &[
        (0, 0),
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ] {
        let column = source
            .resident_column(dx, dz)
            .expect("atomic admission retains every footprint column");
        let expected = if (dx, dz) == (0, 0) {
            RetainedLightStatus::CentreSettled
        } else {
            RetainedLightStatus::DependencyInitialized
        };
        assert_eq!(
            column.retained_light_status(),
            Some(expected),
            "admitted column ({dx},{dz}) must carry its typed lifecycle stage even when zero-valued"
        );
    }

    let dependency = source
        .resident_column(1, 0)
        .expect("the east dependency remains resident for its own admission");
    let mut centre_admission_calls = 0;
    let mut centre_compute = |centre: &ChunkColumn,
                              neighbours: &[(i32, i32, ChunkColumn)]| {
        centre_admission_calls += 1;
        protocol.compute_initial_column_lights_with_neighbours_in_dimension(
            centre,
            neighbours,
            Dimension::Nether,
        )
    };
    let settled = source
        .settle_resident_column_lights_with_neighbours(
            1,
            0,
            &dependency,
            &[
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ],
            false,
            false,
            true,
            &mut centre_compute,
        )
        .expect("a populated dependency must run its own centre admission");
    assert_eq!(centre_admission_calls, 1);
    assert_eq!(
        settled.retained_light_status(),
        Some(RetainedLightStatus::CentreSettled)
    );

    let mut skipped_calls = 0;
    let mut skipped_compute = |centre: &ChunkColumn,
                               neighbours: &[(i32, i32, ChunkColumn)]| {
        skipped_calls += 1;
        protocol.compute_initial_column_lights_with_neighbours_in_dimension(
            centre,
            neighbours,
            Dimension::Nether,
        )
    };
    source
        .settle_resident_column_lights_with_neighbours(
            1,
            0,
            &settled,
            &[
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ],
            false,
            false,
            true,
            &mut skipped_compute,
        )
        .expect("a centre-settled snapshot must use the fast path");
    assert_eq!(skipped_calls, 0);
}

#[test]
fn dependency_centre_admission_preserves_values_and_center_storage_shape() {
    let shape = ChunkShape::nether_or_end_1_21();
    let section_count = shape.section_count;
    let light_section_count = section_count + 2;
    let source = retained_chunk_source_for_view_radius(lodestone_server::nether_chunk_source(42), 8);
    let mut retained = ColumnLight::new(section_count);
    let mut block_values = NibbleArray::filled(11);
    block_values.set(NibbleArray::index(3, 5, 7), 2);
    *retained.sky_mut(1) = LightData::Uniform(4);
    *retained.block_mut(6) = LightData::Values(block_values);
    let mut allocated = vec![false; light_section_count];
    allocated[1] = true;
    allocated[6] = true;
    let mut light_and_data = vec![false; light_section_count];
    light_and_data[6] = true;
    let storage = LightStorage::from_masks(allocated, light_and_data);

    let mut centre = source.column(0, 0);
    centre.set_block(8, 82, 8, "minecraft:netherrack");
    retained.set_storage(storage.clone());
    centre.set_retained_light_with_status(
        retained.clone(),
        RetainedLightStatus::DependencyInitialized,
    );
    assert!(source.store_resident_column(0, 0, &centre));

    let mut new_dependency = source.column(1, 0);
    new_dependency.set_block(0, 82, 0, "minecraft:glowstone");
    assert!(source.store_resident_column(1, 0, &new_dependency));
    assert!(new_dependency.retained_light().is_none());

    let neighbour_offsets = vec![
        (-1, -1),
        (0, -1),
        (1, -1),
        (-1, 0),
        (1, 0),
        (-1, 1),
        (0, 1),
        (1, 1),
    ];
    let neighbours = neighbour_offsets
        .iter()
        .map(|&(dx, dz)| {
            let column = if (dx, dz) == (1, 0) {
                new_dependency.clone()
            } else {
                source.column(dx, dz)
            };
            (dx, dz, column)
        })
        .collect::<Vec<_>>();
    let proto = V770ServerProtocol;
    let fresh_centre = {
        let mut column = centre.clone();
        column.clear_retained_light();
        proto
            .compute_initial_column_light_with_neighbours_in_dimension(
                &column,
                &neighbours,
                Dimension::Nether,
            )
            .expect("fresh Nether centre computation")
    };
    assert_ne!(
        fresh_centre, retained,
        "the retained fixture must differ from a fresh computation"
    );

    let settled = source
        .settle_resident_column_lights_with_neighbours(
            0,
            0,
            &centre,
            &neighbour_offsets,
            false,
            false,
            true,
            &mut |centre, neighbours| {
                proto.compute_initial_column_lights_with_neighbours_in_dimension(
                    centre,
                    neighbours,
                    Dimension::Nether,
                )
            },
        )
        .expect("Nether dependency centre admission");
    assert_eq!(settled.retained_light_status(), Some(RetainedLightStatus::CentreSettled));
    let settled_light = settled
        .retained_light()
        .expect("the centre receives a settled light snapshot");
    assert_eq!(settled_light.block(6), retained.block(6));
    assert_eq!(
        settled_light.sky(1),
        &LightData::Missing,
        "the centre keeps its normalized Nether sky representation"
    );
    let settled_storage = settled_light
        .storage()
        .expect("the centre receives its own storage classification");
    assert!(settled_storage.is_allocated(6));
    assert_ne!(settled_storage, &storage);

    let dependency = source
        .resident_column(1, 0)
        .expect("new dependency remains resident");
    let dependency_storage = dependency
        .retained_light()
        .and_then(|light| light.storage())
        .expect("new dependency receives allocated light storage");
    assert_eq!(dependency_storage.section_count(), light_section_count);
    assert!(
        (0..dependency_storage.section_count())
            .any(|section| dependency_storage.is_allocated(section)),
        "the emitter dependency must retain at least one allocated section"
    );
    assert!(
        dependency
            .retained_light()
            .is_some_and(ColumnLight::has_nonzero_values),
        "the newly admitted emitter dependency must retain its computed light"
    );
    assert_eq!(
        dependency.retained_light_status(),
        Some(RetainedLightStatus::DependencyInitialized)
    );
}

const PROBE_Y: i32 = 82;
const PROBE_X: usize = 8;
const PROBE_Z: usize = 15;
const PROBE_MARKER_LEVEL: u8 = 13;

/// A small source whose complete-column retention can be inspected at every
/// boundary of one admission. The coordinates are deliberately relative to
/// the two admissions below; no production world coordinate or packet is
/// involved in this control.
#[derive(Clone, Default)]
struct LifecycleProbeSource {
    columns: Arc<Mutex<BTreeMap<(i32, i32), ChunkColumn>>>,
}

impl LifecycleProbeSource {
    fn blank() -> ChunkColumn {
        ChunkColumn::new(0, 256)
    }

    fn with_emitter() -> Self {
        let source = Self::default();
        let mut dependency = Self::blank();
        dependency.set_block(8, PROBE_Y, 0, "minecraft:glowstone");
        source.insert((0, 1), dependency);
        source
    }

    fn insert(&self, coordinate: (i32, i32), column: ChunkColumn) {
        self.columns.lock().expect("probe source lock").insert(coordinate, column);
    }
}

impl ChunkSource for LifecycleProbeSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.columns
            .lock()
            .expect("probe source lock")
            .get(&(cx, cz))
            .cloned()
            .unwrap_or_else(Self::blank)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .block_state(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_owned()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_owned()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, name: &str) {
        let coordinate = (x.div_euclid(16), z.div_euclid(16));
        let mut columns = self.columns.lock().expect("probe source lock");
        let column = columns.entry(coordinate).or_insert_with(Self::blank);
        column.set_block(x.rem_euclid(16), y, z.rem_euclid(16), name);
    }

    fn resident_column(&self, cx: i32, cz: i32) -> Option<ChunkColumn> {
        self.columns
            .lock()
            .expect("probe source lock")
            .get(&(cx, cz))
            .cloned()
    }

    fn store_resident_column(&self, cx: i32, cz: i32, column: &ChunkColumn) -> bool {
        self.insert((cx, cz), column.clone());
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LightSummary {
    hash: u64,
    nonzero: usize,
    max: u8,
    marker: u8,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LightObservation {
    status: Option<RetainedLightStatus>,
    light: Option<LightSummary>,
}

#[derive(Debug, Clone)]
struct AdmissionTrace {
    centre: (i32, i32),
    pre_centre: LightObservation,
    pre_plus_z: LightObservation,
    returned_centre: LightSummary,
    post_centre: LightObservation,
    post_plus_z: LightObservation,
}

fn mix_probe_hash(hash: &mut u64, byte: u8) {
    *hash ^= u64::from(byte);
    *hash = hash.wrapping_mul(1_099_511_628_211);
}

fn summarize_light_data(data: &LightData, hash: &mut u64) -> (usize, u8) {
    match data {
        LightData::Missing => {
            mix_probe_hash(hash, 0);
            (0, 0)
        }
        LightData::Uniform(value) => {
            mix_probe_hash(hash, 1);
            mix_probe_hash(hash, *value);
            if *value == 0 {
                (0, 0)
            } else {
                (NibbleArray::LEN, *value)
            }
        }
        LightData::Values(values) => {
            mix_probe_hash(hash, 2);
            let mut nonzero = 0;
            let mut max = 0;
            for &byte in values.as_bytes() {
                mix_probe_hash(hash, byte);
                let low = byte & 0x0f;
                let high = byte >> 4;
                nonzero += usize::from(low != 0) + usize::from(high != 0);
                max = max.max(low).max(high);
            }
            (nonzero, max)
        }
    }
}

fn summarize_light(light: &ColumnLight) -> LightSummary {
    let mut hash = 14_695_981_039_346_656_037u64;
    let mut nonzero = 0;
    let mut max = 0;
    for section in 0..light.light_section_count() {
        let (sky_nonzero, sky_max) = summarize_light_data(light.sky(section), &mut hash);
        let (block_nonzero, block_max) = summarize_light_data(light.block(section), &mut hash);
        nonzero += sky_nonzero + block_nonzero;
        max = max.max(sky_max).max(block_max);
    }
    if let Some(storage) = light.storage() {
        mix_probe_hash(&mut hash, 1);
        for section in 0..storage.section_count() {
            mix_probe_hash(&mut hash, u8::from(storage.is_allocated(section)));
            mix_probe_hash(&mut hash, u8::from(storage.is_light_only(section)));
            mix_probe_hash(&mut hash, u8::from(storage.has_block_data(section)));
        }
    } else {
        mix_probe_hash(&mut hash, 0);
    }
    LightSummary {
        hash,
        nonzero,
        max,
        marker: block_at(light, PROBE_Y, PROBE_X, PROBE_Z),
    }
}

fn observe_column(column: &ChunkColumn) -> LightObservation {
    LightObservation {
        status: column.retained_light_status(),
        light: column.retained_light().map(summarize_light),
    }
}

fn marker_light(section_count: usize) -> ColumnLight {
    let mut marker = ColumnLight::new(section_count);
    let mut values = NibbleArray::filled(0);
    values.set(NibbleArray::index(PROBE_X, 2, PROBE_Z), PROBE_MARKER_LEVEL);
    *marker.block_mut(6) = LightData::Values(values);
    marker
}

fn trace_admission<S: ChunkSource>(
    source: &S,
    protocol: &V770ServerProtocol,
    centre: (i32, i32),
    replace_existing: bool,
    traces: &mut Vec<AdmissionTrace>,
) -> ChunkColumn {
    let fallback = source.column(centre.0, centre.1);
    let mut returned = None;
    let mut pre_centre = None;
    let mut pre_plus_z = None;
    let mut compute = |current: &ChunkColumn, neighbours: &[(i32, i32, ChunkColumn)]| {
        pre_centre = Some(observe_column(current));
        pre_plus_z = Some(
            neighbours
                .iter()
                .find(|(dx, dz, _)| (*dx, *dz) == (0, 1))
                .map(|(_, _, column)| observe_column(column))
                .unwrap_or(LightObservation {
                    status: None,
                    light: None,
                }),
        );
        let settlement = protocol.compute_initial_column_lights_with_neighbours_in_dimension(
            current,
            neighbours,
            Dimension::Nether,
        )?;
        returned = Some(summarize_light(settlement.centre_light()));
        Some(settlement)
    };
    let settled = source
        .settle_resident_column_lights_with_neighbours(
            centre.0,
            centre.1,
            &fallback,
            &[
                (-1, -1),
                (0, -1),
                (1, -1),
                (-1, 0),
                (1, 0),
                (-1, 1),
                (0, 1),
                (1, 1),
            ],
            false,
            replace_existing,
            true,
            &mut compute,
        )
        .expect("probe admission must commit");
    let post_centre = source
        .resident_column(centre.0, centre.1)
        .map(|column| observe_column(&column))
        .expect("probe centre must remain resident");
    let post_plus_z = source
        .resident_column(centre.0, centre.1 + 1)
        .map(|column| observe_column(&column))
        .expect("probe +Z dependency must remain resident");
    let returned_centre = returned.expect("probe callback must return a settlement");
    let trace = AdmissionTrace {
        centre,
        pre_centre: pre_centre.expect("probe callback must observe centre"),
        pre_plus_z: pre_plus_z.expect("probe callback must observe +Z dependency"),
        returned_centre,
        post_centre,
        post_plus_z,
    };
    traces.push(trace);
    settled
}

#[test]
fn retained_light_lifecycle_control_distinguishes_initialization_seed_and_commit() {
    let shape = ChunkShape::nether_or_end_1_21();
    let protocol = V770ServerProtocol;
    let source = retained_chunk_source_for_view_radius(
        Arc::new(LifecycleProbeSource::with_emitter()),
        8,
    );
    let mut traces = Vec::new();
    trace_admission(&source, &protocol, (0, 0), false, &mut traces);
    let first = traces.first().expect("first admission trace");
    assert_eq!(first.centre, (0, 0));
    assert_eq!(first.pre_plus_z.status, None);
    assert_eq!(
        first.post_plus_z.status,
        Some(RetainedLightStatus::DependencyInitialized),
        "A: a queued dependency must receive a typed retained snapshot"
    );
    assert!(
        first.post_plus_z.light.as_ref().is_some_and(|light| light.nonzero > 0),
        "A: the emitter dependency must retain a nonzero light field"
    );

    let dependency = source
        .resident_column(0, 1)
        .expect("the first admission must retain its +Z dependency");
    let mut marked = dependency;
    marked.set_retained_light_with_status(
        marker_light(shape.section_count),
        RetainedLightStatus::DependencyInitialized,
    );
    assert!(source.store_resident_column(0, 1, &marked));

    trace_admission(&source, &protocol, (0, 1), true, &mut traces);
    let second = traces.get(1).expect("second admission trace");
    assert_eq!(second.pre_centre.status, Some(RetainedLightStatus::DependencyInitialized));
    assert_eq!(
        second.pre_centre.light.as_ref().map(|light| light.marker),
        Some(PROBE_MARKER_LEVEL),
        "B: the later centre admission must see the retained dependency values"
    );
    assert_eq!(
        second.returned_centre.marker, PROBE_MARKER_LEVEL,
        "B: the callback must preserve the retained centre value"
    );
    assert_eq!(
        second.post_centre.light.as_ref().map(|light| light.marker),
        Some(PROBE_MARKER_LEVEL),
        "C: commit must preserve the callback's centre value"
    );
    assert_eq!(
        second.post_centre.light.as_ref().map(|light| light.hash),
        Some(second.returned_centre.hash),
        "C: post-commit light bytes must match the returned settlement"
    );

    let negative_source = retained_chunk_source_for_view_radius(
        Arc::new(LifecycleProbeSource::with_emitter()),
        8,
    );
    let mut negative_traces = Vec::new();
    trace_admission(
        &negative_source,
        &protocol,
        (0, 0),
        false,
        &mut negative_traces,
    );
    let mut cleared = negative_source
        .resident_column(0, 1)
        .expect("negative control dependency");
    cleared.clear_retained_light();
    assert!(negative_source.store_resident_column(0, 1, &cleared));
    trace_admission(
        &negative_source,
        &protocol,
        (0, 1),
        true,
        &mut negative_traces,
    );
    let negative = negative_traces.get(1).expect("negative admission trace");
    assert_eq!(negative.pre_centre.status, None);
    assert_eq!(negative.pre_centre.light, None);
    assert_ne!(
        negative.returned_centre.marker, PROBE_MARKER_LEVEL,
        "negative detector: removing retained state must change the returned centre"
    );
    assert_eq!(
        negative.post_centre.light.as_ref().map(|light| light.hash),
        Some(negative.returned_centre.hash),
        "negative detector: commit still records the fresh callback result"
    );

    for trace in traces.iter().chain(&negative_traces) {
        eprintln!(
            "nether lifecycle probe centre=({},{}) pre={:?} pre_plus_z={:?} returned={:?} post={:?} post_plus_z={:?}",
            trace.centre.0,
            trace.centre.1,
            trace.pre_centre,
            trace.pre_plus_z,
            trace.returned_centre,
            trace.post_centre,
            trace.post_plus_z,
        );
    }
}
