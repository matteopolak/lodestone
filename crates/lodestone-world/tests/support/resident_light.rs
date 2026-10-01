use super::*;
use crate::{Neighbourhood, compute_column_light_with_neighbours};
use std::cell::Cell;
use std::collections::BTreeMap;

const AIR: u32 = 0;
const STONE: u32 = 1;
const GLOW: u32 = 2;
const TORCH: u32 = 3;
const CLEAR: u32 = 4;
const WATER: u32 = 5;

struct Props {
    sky: bool,
}

impl LightProperties for Props {
    fn has_skylight(&self) -> bool { self.sky }
    fn opacity(&self, state: u32) -> u8 {
        match state {
            STONE => 15,
            WATER => 1,
            _ => 0,
        }
    }
    fn emission(&self, state: u32) -> u8 {
        match state {
            GLOW => 15,
            TORCH => 14,
            _ => 0,
        }
    }
}

struct Volume {
    cells: Vec<u32>,
    reads: Cell<usize>,
}

impl Volume {
    fn new() -> Self {
        Self { cells: vec![AIR; 2 * 4096], reads: Cell::new(0) }
    }

    fn set(&mut self, x: usize, y: usize, z: usize, state: u32) {
        self.cells[y * 256 + z * 16 + x] = state;
    }

    fn roof(&mut self) {
        for z in 0..16 {
            for x in 0..16 {
                self.set(x, 8, z, STONE);
            }
        }
    }
}

impl BlockVolume for Volume {
    fn block(&self, x: usize, y: i32, z: usize) -> u32 {
        self.reads.set(self.reads.get() + 1);
        if (0..32).contains(&y) {
            self.cells[y as usize * 256 + z * 16 + x]
        } else {
            AIR
        }
    }
    fn min_y(&self) -> i32 { 0 }
    fn section_count(&self) -> usize { 2 }
}

fn empty(outputs: &[(i32, i32)]) -> BTreeMap<(i32, i32), Volume> {
    ResidentLightFootprint::new(outputs.iter().copied()).unwrap().inputs()
        .iter().copied().map(|position| (position, Volume::new())).collect()
}

fn inputs<'a>(
    volumes: &'a BTreeMap<(i32, i32), Volume>,
    outputs: &[(i32, i32)],
) -> ResidentLightInputs<'a, Volume> {
    let footprint = ResidentLightFootprint::new(outputs.iter().copied()).unwrap();
    let columns = footprint.inputs().iter().map(|&(cx, cz)| {
        (cx, cz, &volumes[&(cx, cz)])
    }).collect::<Vec<_>>();
    ResidentLightInputs::new(footprint, columns).unwrap()
}

fn solve(
    volumes: &BTreeMap<(i32, i32), Volume>,
    outputs: &[(i32, i32)],
    props: &Props,
) -> ResidentLightResult {
    ResidentLightJob::new(inputs(volumes, outputs), props).finish()
}

fn independent(
    volumes: &BTreeMap<(i32, i32), Volume>,
    target: (i32, i32),
    props: &Props,
) -> ColumnLight {
    let mut neighbourhood = Neighbourhood::new(&volumes[&target]);
    for dz in -1..=1 {
        for dx in -1..=1 {
            if (dx, dz) != (0, 0) {
                neighbourhood = neighbourhood.with(dx, dz, &volumes[&(target.0 + dx, target.1 + dz)]);
            }
        }
    }
    compute_column_light_with_neighbours(&neighbourhood, props)
}

fn at(light: &ColumnLight, x: usize, y: i32, z: usize, sky: bool) -> u8 {
    let y = (y + 16) as usize;
    let layer = if sky { light.sky(y / 16) } else { light.block(y / 16) };
    layer.get(NibbleArray::index(x, y % 16, z)).unwrap_or(0)
}

fn check_level(actual: u8, expected: u8) -> Result<u8, u8> {
    if actual == expected { Ok(actual) } else { Err(actual) }
}

fn compare(target: (i32, i32), actual: &ColumnLight, expected: &ColumnLight) {
    if actual != expected {
        let mut bounds: Option<((i32, i32, i32), (i32, i32, i32))> = None;
        for section in 0..actual.light_section_count() {
            for (ours, theirs) in [(actual.sky(section), expected.sky(section)),
                (actual.block(section), expected.block(section))]
            {
                for cell in 0..NibbleArray::LEN {
                    if ours.get(cell) != theirs.get(cell) {
                        let p = (target.0 * 16 + (cell % 16) as i32,
                            section as i32 * 16 - 16 + (cell / 256) as i32,
                            target.1 * 16 + ((cell / 16) % 16) as i32);
                        bounds = Some(match bounds {
                            None => (p, p),
                            Some((lo, hi)) => ((lo.0.min(p.0), lo.1.min(p.1), lo.2.min(p.2)),
                                (hi.0.max(p.0), hi.1.max(p.1), hi.2.max(p.2))),
                        });
                    }
                }
            }
        }
        assert_eq!(actual, expected, "target {target:?}; mismatch bounding box {bounds:?}");
    }
}

#[test]
fn footprint_requires_distinct_bounded_outputs_and_complete_inputs() {
    assert!(matches!(ResidentLightFootprint::new([]), Err(ResidentLightError::NoOutputs)));
    assert!(matches!(ResidentLightFootprint::new([(0, 0), (0, 0)]),
        Err(ResidentLightError::InvalidOutputs)));
    assert!(matches!(ResidentLightFootprint::new((0..10).map(|x| (x, 0))),
        Err(ResidentLightError::InvalidOutputs)));
    assert!(matches!(ResidentLightFootprint::new([(0, 0), (3, 0)]),
        Err(ResidentLightError::FootprintTooWide)));
    assert!(matches!(ResidentLightFootprint::new([(i32::MAX, 0)]),
        Err(ResidentLightError::CoordinateOverflow)));
    let outputs = (-1..=1).flat_map(|z| (-1..=1).map(move |x| (x, z))).collect::<Vec<_>>();
    let footprint = ResidentLightFootprint::new(outputs).unwrap();
    assert_eq!(footprint.outputs().len(), 9);
    assert_eq!(footprint.inputs().len(), 25);
    let volumes = empty(footprint.outputs());
    let incomplete = footprint.inputs().iter().filter(|&&p| p != (2, 0))
        .map(|&(x, z)| (x, z, &volumes[&(x, z)])).collect::<Vec<_>>();
    assert!(matches!(ResidentLightInputs::new(footprint, incomplete),
        Err(ResidentLightError::MissingInput)));
}

#[test]
fn every_output_matches_independent_halo_with_distinct_sky_trimming() {
    let outputs = (-1..=1).flat_map(|z| (-1..=1).map(move |x| (x, z))).collect::<Vec<_>>();
    let mut volumes = empty(&outputs);
    for (&(cx, cz), volume) in &mut volumes {
        for z in 0..16 {
            for x in 0..16 {
                volume.set(x, 0, z, STONE);
                if (x + 2 * z) % 5 != 0 {
                    volume.set(x, 8 + (cx + cz).rem_euclid(3) as usize, z, STONE);
                }
            }
        }
        volume.set(13, 5, 11, if (cx - cz).rem_euclid(2) == 0 { GLOW } else { TORCH });
        for y in 1..8 { volume.set(3, y, 7, WATER); }
        if (cx, cz) == (0, 0) { volume.set(7, 31, 8, CLEAR); }
    }
    for props in [Props { sky: true }, Props { sky: false }] {
        for selected in [outputs.as_slice(), &[(0, 0)][..], &[(-1, -1), (1, 1)][..]] {
            let footprint = ResidentLightFootprint::new(selected.iter().copied()).unwrap();
            let columns = footprint.inputs().iter().map(|&(x, z)| (x, z, &volumes[&(x, z)]))
                .collect::<Vec<_>>();
            let bound = ResidentLightInputs::new(footprint, columns).unwrap();
            let result = ResidentLightJob::new(bound, &props).finish();
            assert_eq!(result.columns.len(), selected.len());
            for (target, light) in &result.columns {
                compare(*target, light, &independent(&volumes, *target, &props));
            }
            if props.sky && selected.len() == 9 {
                let centre = &result.columns.iter().find(|(p, _)| *p == (0, 0)).unwrap().1;
                let west = &result.columns.iter().find(|(p, _)| *p == (-1, 0)).unwrap().1;
                assert_eq!(centre.sky(3), &LightData::Uniform(15));
                assert_eq!(west.sky(3), &LightData::Missing);
                assert_eq!(centre.block(0), &LightData::Uniform(0));
            }
            #[cfg(not(target_arch = "wasm32"))]
            if matches!(selected.len(), 1 | 9) {
                measure_outputs(&volumes, selected, &props);
            }
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
fn measure_outputs(
    volumes: &BTreeMap<(i32, i32), Volume>,
    selected: &[(i32, i32)],
    props: &Props,
) {
    let Ok(raw) = std::env::var("LODESTONE_RESIDENT_LIGHT_PERF_ITERATIONS") else { return };
    let iterations = raw.parse::<usize>().expect("integer performance iterations");
    assert!((1..=128).contains(&iterations), "performance iterations must be in 1..=128");
    let field = inputs(volumes, selected).field;
    let fixture_height = volumes[&selected[0]].section_count() * EDGE;
    let ordinary_input_columns = selected.len() * 9;
    let unified_input_columns = ResidentLightFootprint::new(selected.iter().copied()).unwrap()
        .inputs().len();
    let mut ordinary = std::time::Duration::ZERO;
    let mut resumable = std::time::Duration::ZERO;
    for iteration in 0..iterations {
        for new_solver in [iteration % 2 == 0, iteration % 2 != 0] {
            let started = std::time::Instant::now();
            if new_solver {
                let output = std::hint::black_box(solve(volumes, selected, props));
                resumable += started.elapsed();
                drop(output);
            } else {
                let output = std::hint::black_box(selected.iter().map(|&target| {
                    (target, independent(volumes, target, props))
                }).collect::<Vec<_>>());
                ordinary += started.elapsed();
                drop(output);
            }
        }
    }
    eprintln!("RESIDENT_LIGHT_PERF outputs={} fixture_height={} field_height={} sky={} calls={} ordinary_input_columns={} unified_input_columns={} ordinary_ns={} resumable_ns={}",
        selected.len(), fixture_height, field.height, props.sky, iterations,
        ordinary_input_columns, unified_input_columns, ordinary.as_nanos(), resumable.as_nanos());
}

#[test]
fn outer_halo_source_reaches_an_output_and_removal_clears_it() {
    let outputs = (-1..=1).flat_map(|z| (-1..=1).map(move |x| (x, z))).collect::<Vec<_>>();
    let mut volumes = empty(&outputs);
    let props = Props { sky: false };
    volumes.get_mut(&(2, 0)).unwrap().set(0, 4, 8, GLOW);
    let result = solve(&volumes, &outputs, &props);
    let light = &result.columns.iter().find(|(p, _)| *p == (1, 0)).unwrap().1;
    let accepted = check_level(at(light, 15, 4, 8, false), 14);
    assert_eq!(accepted, Ok(14));
    let without_outer = compute_column_light_with_neighbours(
        &Neighbourhood::new(&volumes[&(1, 0)]), &props);
    let rejected = check_level(at(&without_outer, 15, 4, 8, false), 14);
    assert_eq!(rejected, Err(0), "the same detector must reject the omitted outer source");
    eprintln!("resident outer-halo level control: complete={accepted:?}, omitted={rejected:?}");
    volumes.get_mut(&(2, 0)).unwrap().set(0, 4, 8, AIR);
    let removed = solve(&volumes, &outputs, &props);
    for (_, light) in &removed.columns {
        for section in 0..light.light_section_count() {
            assert_eq!(light.block(section), &LightData::Uniform(0));
        }
    }
}

#[test]
fn torch_crosses_a_seam_and_corner_at_arithmetic_levels() {
    let outputs = [(0, 0), (1, 0), (1, 1)];
    let mut volumes = empty(&outputs);
    volumes.get_mut(&(0, 0)).unwrap().set(15, 4, 15, TORCH);
    let result = solve(&volumes, &outputs, &Props { sky: false });
    let east = &result.columns.iter().find(|(p, _)| *p == (1, 0)).unwrap().1;
    let corner = &result.columns.iter().find(|(p, _)| *p == (1, 1)).unwrap().1;
    assert_eq!(at(east, 0, 4, 15, false), 13);
    assert_eq!(at(corner, 0, 4, 0, false), 12);
    assert_eq!(at(corner, 1, 4, 0, false), 11);
}

#[test]
fn sky_seam_and_opened_shaft_match_arithmetic_and_no_sky_control() {
    let outputs = [(0, 0), (1, 0)];
    let mut volumes = empty(&outputs);
    for volume in volumes.values_mut() { volume.roof(); }
    volumes.insert((-1, 0), Volume::new());
    let result = solve(&volumes, &outputs, &Props { sky: true });
    let centre = &result.columns[0].1;
    assert_eq!(at(centre, 1, 4, 8, true), 13);
    assert_eq!(at(centre, 5, 4, 8, true), 9);
    let dark = solve(&volumes, &outputs, &Props { sky: false });
    assert_eq!(at(&dark.columns[0].1, 1, 4, 8, true), 0);
    volumes.get_mut(&(-1, 0)).unwrap().roof();
    let sealed = solve(&volumes, &outputs, &Props { sky: true });
    assert_eq!(at(&sealed.columns[0].1, 1, 4, 8, true), 0);
    volumes.get_mut(&(0, 0)).unwrap().set(7, 8, 8, AIR);
    let opened = solve(&volumes, &outputs, &Props { sky: true });
    assert_eq!(at(&opened.columns[0].1, 7, 4, 8, true), 15);
}

#[test]
fn slices_charge_every_stage_and_do_not_read_terrain_ahead() {
    let outputs = [(0, 0)];
    let mut volumes = empty(&outputs);
    for volume in volumes.values_mut() { volume.roof(); }
    volumes.get_mut(&(0, 0)).unwrap().set(15, 4, 15, TORCH);
    volumes.insert((-1, 0), Volume::new());
    let props = Props { sky: true };
    let mut job = ResidentLightJob::new(inputs(&volumes, &outputs), &props);
    let field_cells = 48 * 48 * 64;
    assert_eq!(job.field_cells(), field_cells);
    assert_eq!(job.opacity.capacity(), field_cells);
    assert_eq!(job.sky.capacity(), field_cells);
    assert_eq!(job.block.capacity(), field_cells);
    assert!(volumes.values().all(|volume| volume.reads.get() == 0));
    let budget = NonZeroUsize::new(13).unwrap();
    loop {
        let before = job.work().total();
        let reads = volumes.values().map(|volume| volume.reads.get()).sum::<usize>();
        let progress = job.step(budget);
        let used = job.work().total() - before;
        let next_reads = volumes.values().map(|volume| volume.reads.get()).sum::<usize>();
        assert!(used <= budget.get());
        assert!(next_reads - reads <= used);
        if progress == ResidentLightProgress::Ready { break; }
        assert!(used > 0, "each pending slice makes progress");
    }
    let work = job.work();
    assert_eq!(work.scan_cells, field_cells);
    assert!(work.sky_seed_steps > 0 && work.sky_frontier_steps > 0);
    assert!(work.sky_flood_steps > 15 && work.block_flood_steps > 15);
    assert_eq!(work.packing_steps, 1 + 4 * (4096 + 1));
    let result = match job.into_result() {
        Ok(result) => result,
        Err(_) => panic!("all stages completed"),
    };
    compare((0, 0), &result.columns[0].1, &independent(&volumes, (0, 0), &props));
    assert_eq!(result.work, work);
}
