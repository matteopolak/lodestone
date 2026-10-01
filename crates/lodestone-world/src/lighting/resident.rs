//! Borrowed, bounded resident light solves with explicit output halos.

use std::num::NonZeroUsize;

use super::{BlockVolume, Buckets, EDGE, Field, LightProperties, MAX_LIGHT, neighbours};
use crate::{ColumnLight, LightData, NibbleArray};

const MAX_INPUTS: usize = 25;
const MAX_OUTPUTS: usize = 9;

/// An invalid resident light footprint or volume shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentLightError {
    /// At least one output column is required.
    NoOutputs,
    /// Outputs must be distinct and number at most nine.
    InvalidOutputs,
    /// The union of output halos must fit within five by five columns.
    FootprintTooWide,
    /// A halo coordinate cannot be represented by an `i32`.
    CoordinateOverflow,
    /// A required input column was not supplied.
    MissingInput,
    /// An input was duplicated or lies outside the required halo union.
    InvalidInput,
    /// Input columns disagree about minimum Y or section count.
    InconsistentShape,
    /// The vertical field or its queue indices cannot be represented.
    InvalidShape,
}

/// Distinct output coordinates and the complete union of their radius-one halos.
///
/// Coordinates are column coordinates. The halo is input only: its presence
/// does not make a column an output or authorize an owning source to settle it.
#[derive(Debug, Clone)]
pub struct ResidentLightFootprint {
    outputs: Vec<(i32, i32)>,
    inputs: Vec<(i32, i32)>,
    origin: (i32, i32),
    width: usize,
    depth: usize,
}

impl ResidentLightFootprint {
    /// Validates at most nine outputs whose complete halo fits a 5×5 field.
    pub fn new(
        outputs: impl IntoIterator<Item = (i32, i32)>,
    ) -> Result<Self, ResidentLightError> {
        let mut outputs = outputs.into_iter().take(MAX_OUTPUTS + 1).collect::<Vec<_>>();
        if outputs.is_empty() {
            return Err(ResidentLightError::NoOutputs);
        }
        outputs.sort_unstable();
        if outputs.len() > MAX_OUTPUTS || outputs.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(ResidentLightError::InvalidOutputs);
        }
        let mut inputs = Vec::with_capacity(MAX_INPUTS);
        for &(cx, cz) in &outputs {
            for dz in -1..=1 {
                for dx in -1..=1 {
                    inputs.push((
                        cx.checked_add(dx).ok_or(ResidentLightError::CoordinateOverflow)?,
                        cz.checked_add(dz).ok_or(ResidentLightError::CoordinateOverflow)?,
                    ));
                }
            }
        }
        inputs.sort_unstable();
        inputs.dedup();
        let min_x = inputs.iter().map(|p| p.0).min().expect("nonempty halos");
        let max_x = inputs.iter().map(|p| p.0).max().expect("nonempty halos");
        let min_z = inputs.iter().map(|p| p.1).min().expect("nonempty halos");
        let max_z = inputs.iter().map(|p| p.1).max().expect("nonempty halos");
        let width = i64::from(max_x) - i64::from(min_x) + 1;
        let depth = i64::from(max_z) - i64::from(min_z) + 1;
        if width > 5 || depth > 5 || inputs.len() > MAX_INPUTS {
            return Err(ResidentLightError::FootprintTooWide);
        }
        Ok(Self {
            outputs,
            inputs,
            origin: (min_x, min_z),
            width: width as usize,
            depth: depth as usize,
        })
    }

    /// The columns whose results will be packed, in coordinate order.
    pub fn outputs(&self) -> &[(i32, i32)] {
        &self.outputs
    }

    /// Every required input coordinate, including each output's complete halo.
    pub fn inputs(&self) -> &[(i32, i32)] {
        &self.inputs
    }

    fn slot(&self, coordinate: (i32, i32)) -> usize {
        let x = (i64::from(coordinate.0) - i64::from(self.origin.0)) as usize;
        let z = (i64::from(coordinate.1) - i64::from(self.origin.1)) as usize;
        z * self.width + x
    }
}

/// Validated borrowed volumes for a complete resident light footprint.
///
/// This type owns no terrain snapshots. The caller must keep these volumes
/// stable until the job completes, then validate its source revisions before
/// publishing the returned light.
pub struct ResidentLightInputs<'a, V: BlockVolume> {
    footprint: ResidentLightFootprint,
    columns: [Option<&'a V>; MAX_INPUTS],
    min_y: i32,
    section_count: usize,
    field: Field,
}

impl<V: BlockVolume> std::fmt::Debug for ResidentLightInputs<'_, V> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResidentLightInputs")
            .field("footprint", &self.footprint)
            .field("min_y", &self.min_y)
            .field("section_count", &self.section_count)
            .finish_non_exhaustive()
    }
}

impl<'a, V: BlockVolume> ResidentLightInputs<'a, V> {
    /// Binds exactly the required columns and validates their common shape.
    pub fn new(
        footprint: ResidentLightFootprint,
        columns: impl IntoIterator<Item = (i32, i32, &'a V)>,
    ) -> Result<Self, ResidentLightError> {
        let mut bound = [None; MAX_INPUTS];
        for (cx, cz, column) in columns {
            if footprint.inputs.binary_search(&(cx, cz)).is_err() {
                return Err(ResidentLightError::InvalidInput);
            }
            let slot = footprint.slot((cx, cz));
            if bound[slot].replace(column).is_some() {
                return Err(ResidentLightError::InvalidInput);
            }
        }
        if footprint.inputs.iter().any(|&position| bound[footprint.slot(position)].is_none()) {
            return Err(ResidentLightError::MissingInput);
        }
        let first = bound[footprint.slot(footprint.outputs[0])].expect("validated output");
        let min_y = first.min_y();
        let section_count = first.section_count();
        if bound.iter().flatten().any(|column| {
            column.min_y() != min_y || column.section_count() != section_count
        }) {
            return Err(ResidentLightError::InconsistentShape);
        }
        let height = section_count.checked_add(2)
            .and_then(|sections| sections.checked_mul(EDGE))
            .ok_or(ResidentLightError::InvalidShape)?;
        let field = Field {
            wx: footprint.width * EDGE,
            wz: footprint.depth * EDGE,
            height,
        };
        if field.area().checked_mul(height).is_none_or(|len| len > u32::MAX as usize) {
            return Err(ResidentLightError::InvalidShape);
        }
        let bottom = min_y.checked_sub(EDGE as i32).ok_or(ResidentLightError::InvalidShape)?;
        if i64::from(bottom) + height as i64 > i64::from(i32::MAX) {
            return Err(ResidentLightError::InvalidShape);
        }
        Ok(Self { footprint, columns: bound, min_y, section_count, field })
    }
}

/// Charged operations performed by a resident light job.
///
/// One flood operation pops one queue entry and visits at most six neighbours,
/// or finishes one empty level. Packing charges each cell and each section's
/// bounded finalization separately. These are work counts, not elapsed time.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ResidentLightWork {
    /// Field cells sampled and appended to the three light/opacity buffers.
    pub scan_cells: usize,
    /// Top-down sky seed cells examined.
    pub sky_seed_steps: usize,
    /// Horizontal frontier setup and candidate cells examined.
    pub sky_frontier_steps: usize,
    /// Sky queue pops or empty levels completed.
    pub sky_flood_steps: usize,
    /// Block queue pops or empty levels completed.
    pub block_flood_steps: usize,
    /// Output cells, section finalizations, and output initializations.
    pub packing_steps: usize,
}

impl ResidentLightWork {
    /// Total charged operations across every stage.
    pub fn total(self) -> usize {
        self.scan_cells + self.sky_seed_steps + self.sky_frontier_steps
            + self.sky_flood_steps + self.block_flood_steps + self.packing_steps
    }
}

/// Whether a budgeted step completed the solve.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentLightProgress {
    /// More charged operations remain.
    Pending,
    /// All selected outputs have been packed.
    Ready,
}

/// Fresh light results for the selected columns, with solve work counts.
#[derive(Debug)]
pub struct ResidentLightResult {
    /// Coordinate-sorted outputs; input-only halo columns are never returned.
    pub columns: Vec<((i32, i32), ColumnLight)>,
    /// Work consumed by this solve, including all resumptions.
    pub work: ResidentLightWork,
}

#[derive(Debug, Clone, Copy)]
enum Stage {
    Scan,
    SkySeed,
    SkyFrontier,
    SkyFlood,
    BlockFlood,
    Pack,
    Done,
}

/// A fresh multi-target flood that can run synchronously or in bounded slices.
///
/// Buffers belong only to this job. No retained light is used as a seed. A
/// call to `step` never samples more terrain cells than its operation budget;
/// sky seeding, queue work, and per-cell packing are charged too. Allocator
/// latency and host scheduling are outside an operation-count guarantee.
pub struct ResidentLightJob<'a, V: BlockVolume, P: LightProperties> {
    inputs: ResidentLightInputs<'a, V>,
    props: &'a P,
    has_skylight: bool,
    air_ceilings: [i32; MAX_INPUTS],
    output_at: [Option<usize>; MAX_INPUTS],
    highest: [Option<usize>; MAX_OUTPUTS],
    opacity: Vec<u8>,
    sky: Vec<u8>,
    block: Vec<u8>,
    sky_buckets: Buckets,
    block_buckets: Buckets,
    open_bottom: Vec<usize>,
    stage: Stage,
    scan: usize,
    scan_x: usize,
    scan_y: usize,
    scan_z: usize,
    sky_column: usize,
    sky_y: usize,
    frontier_column: usize,
    frontier_y: Option<usize>,
    frontier_end: usize,
    frontier_neighbours: [Option<usize>; 4],
    vertical_frontier: bool,
    level: u8,
    output: usize,
    section: usize,
    cell: usize,
    pack_x: usize,
    pack_z: usize,
    packed: Option<ColumnLight>,
    sky_bytes: [u8; 2048],
    block_bytes: [u8; 2048],
    first_sky: u8,
    first_block: u8,
    uniform_sky: bool,
    uniform_block: bool,
    sky_elide_after: Option<usize>,
    results: Vec<((i32, i32), ColumnLight)>,
    work: ResidentLightWork,
}

impl<V: BlockVolume, P: LightProperties> std::fmt::Debug for ResidentLightJob<'_, V, P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResidentLightJob")
            .field("inputs", &self.inputs)
            .field("stage", &self.stage)
            .field("work", &self.work)
            .field("completed_outputs", &self.results.len())
            .finish_non_exhaustive()
    }
}

impl<'a, V: BlockVolume, P: LightProperties> ResidentLightJob<'a, V, P> {
    /// Starts a solve without invoking `block` or cloning input columns.
    /// Each volume's conservative air-ceiling metadata is queried once.
    pub fn new(inputs: ResidentLightInputs<'a, V>, props: &'a P) -> Self {
        let len = inputs.field.len();
        let area = inputs.field.area();
        let height = inputs.field.height;
        let output_count = inputs.footprint.outputs.len();
        let has_skylight = props.has_skylight();
        let mut output_at = [None; MAX_INPUTS];
        for (index, &position) in inputs.footprint.outputs.iter().enumerate() {
            output_at[inputs.footprint.slot(position)] = Some(index);
        }
        let air_ceilings = inputs.columns.map(|column| {
            column.map_or(i32::MAX, BlockVolume::air_above_y)
        });
        Self {
            inputs,
            props,
            has_skylight,
            air_ceilings,
            output_at,
            highest: [None; MAX_OUTPUTS],
            opacity: Vec::with_capacity(len),
            sky: Vec::with_capacity(len),
            block: Vec::with_capacity(len),
            sky_buckets: Buckets::new(),
            block_buckets: Buckets::new(),
            open_bottom: if has_skylight { vec![0; area] } else { Vec::new() },
            stage: Stage::Scan,
            scan: 0,
            scan_x: 0,
            scan_y: 0,
            scan_z: 0,
            sky_column: 0,
            sky_y: height - 1,
            frontier_column: 0,
            frontier_y: None,
            frontier_end: 0,
            frontier_neighbours: [None; 4],
            vertical_frontier: false,
            level: MAX_LIGHT,
            output: 0,
            section: 0,
            cell: 0,
            pack_x: 0,
            pack_z: 0,
            packed: None,
            sky_bytes: [0; 2048],
            block_bytes: [0; 2048],
            first_sky: 0,
            first_block: 0,
            uniform_sky: true,
            uniform_block: true,
            sky_elide_after: None,
            results: Vec::with_capacity(output_count),
            work: ResidentLightWork::default(),
        }
    }

    /// Advances by at most `budget` charged operations.
    pub fn step(&mut self, budget: NonZeroUsize) -> ResidentLightProgress {
        let mut remaining = budget.get();
        while remaining > 0 && !matches!(self.stage, Stage::Done) {
            remaining -= self.advance_batch(remaining);
        }
        if matches!(self.stage, Stage::Done) {
            ResidentLightProgress::Ready
        } else {
            ResidentLightProgress::Pending
        }
    }

    /// Cumulative work performed so far.
    pub fn work(&self) -> ResidentLightWork {
        self.work
    }

    /// Number of cells in the bounded flood field.
    pub fn field_cells(&self) -> usize {
        self.inputs.field.len()
    }

    /// Takes a finished result, or returns the still-pending job unchanged.
    pub fn into_result(self) -> Result<ResidentLightResult, Self> {
        if matches!(self.stage, Stage::Done) {
            Ok(ResidentLightResult { columns: self.results, work: self.work })
        } else {
            Err(self)
        }
    }

    /// Drives the same state machine to completion on a synchronous worker.
    pub fn finish(mut self) -> ResidentLightResult {
        let budget = NonZeroUsize::new(4096).expect("nonzero solve slice");
        while self.step(budget) == ResidentLightProgress::Pending {}
        match self.into_result() {
            Ok(result) => result,
            Err(_) => unreachable!("a completed solve has a result"),
        }
    }

    fn advance_batch(&mut self, budget: usize) -> usize {
        loop {
            match self.stage {
                Stage::Scan if self.scan == self.inputs.field.len() => {
                    self.stage = if self.has_skylight {
                        Stage::SkySeed
                    } else {
                        Stage::BlockFlood
                    };
                }
                Stage::Scan => {
                    let count = budget.min(self.inputs.field.len() - self.scan);
                    self.scan_cells(count);
                    self.work.scan_cells += count;
                    return count;
                }
                Stage::SkySeed if self.sky_column == self.inputs.field.area() => {
                    self.stage = Stage::SkyFrontier;
                }
                Stage::SkySeed => {
                    let mut count = 0;
                    let area = self.inputs.field.area();
                    while count < budget && self.sky_column < area {
                        self.seed_sky_cell();
                        count += 1;
                    }
                    self.work.sky_seed_steps += count;
                    return count;
                }
                Stage::SkyFrontier if self.frontier_column == self.inputs.field.area() => {
                    self.stage = Stage::SkyFlood;
                }
                Stage::SkyFrontier => {
                    let mut count = 0;
                    let area = self.inputs.field.area();
                    while count < budget && self.frontier_column < area {
                        self.seed_frontier();
                        count += 1;
                    }
                    self.work.sky_frontier_steps += count;
                    return count;
                }
                Stage::SkyFlood if self.level == 0 => {
                    self.level = MAX_LIGHT;
                    self.stage = Stage::BlockFlood;
                }
                Stage::SkyFlood => {
                    let count = flood_batch(&self.inputs.field, &self.opacity, &mut self.sky,
                        &mut self.sky_buckets, &mut self.level, budget);
                    self.work.sky_flood_steps += count;
                    return count;
                }
                Stage::BlockFlood if self.level == 0 => self.stage = Stage::Pack,
                Stage::BlockFlood => {
                    let count = flood_batch(&self.inputs.field, &self.opacity, &mut self.block,
                        &mut self.block_buckets, &mut self.level, budget);
                    self.work.block_flood_steps += count;
                    return count;
                }
                Stage::Pack => {
                    let count = self.pack_batch(budget);
                    self.work.packing_steps += count;
                    return count;
                }
                Stage::Done => return 0,
            }
        }
    }

    fn scan_cells(&mut self, count: usize) {
        let field = self.inputs.field;
        let end = self.scan + count;
        while self.scan < end {
            let (x, y, z) = (self.scan_x, self.scan_y, self.scan_z);
            let slot = (z / EDGE) * self.inputs.footprint.width + x / EDGE;
            let world_y = self.inputs.min_y - EDGE as i32 + y as i32;
            let row_count = (EDGE - x % EDGE).min(end - self.scan);
            match self.inputs.columns[slot] {
                Some(column) => {
                    let air = column.air_state();
                    let above = world_y >= self.air_ceilings[slot];
                    let mut occupied = false;
                    for offset in 0..row_count {
                        let state = if above { air } else {
                            column.block(x % EDGE + offset, world_y, z % EDGE)
                        };
                        occupied |= state != air;
                        let opacity = self.props.opacity(state).min(MAX_LIGHT);
                        let emission = self.props.emission(state).min(MAX_LIGHT);
                        self.opacity.push(opacity);
                        self.block.push(emission);
                        self.sky.push(0);
                        if emission > 0 {
                            self.block_buckets.push(emission, (self.scan + offset) as u32);
                        }
                    }
                    if occupied {
                        if let Some(output) = self.output_at[slot] {
                            self.highest[output] = Some(y / EDGE);
                        }
                    }
                }
                None => {
                    self.opacity.resize(self.scan + row_count, MAX_LIGHT);
                    self.block.resize(self.scan + row_count, 0);
                    self.sky.resize(self.scan + row_count, 0);
                }
            }
            self.scan += row_count;
            self.scan_x += row_count;
            if self.scan_x == field.wx {
                self.scan_x = 0;
                self.scan_z += 1;
                if self.scan_z == field.wz {
                    self.scan_z = 0;
                    self.scan_y += 1;
                }
            }
        }
    }

    fn seed_sky_cell(&mut self) {
        let field = self.inputs.field;
        let idx = self.sky_y * field.area() + self.sky_column;
        if self.opacity[idx] != 0 {
            self.open_bottom[self.sky_column] = self.sky_y + 1;
            self.sky_column += 1;
            self.sky_y = field.height - 1;
        } else {
            self.sky[idx] = MAX_LIGHT;
            if self.sky_y == 0 {
                self.sky_column += 1;
                self.sky_y = field.height - 1;
            } else {
                self.sky_y -= 1;
            }
        }
    }

    fn seed_frontier(&mut self) {
        let field = self.inputs.field;
        let x = self.frontier_column % field.wx;
        let z = self.frontier_column / field.wx;
        let bottom = self.open_bottom[self.frontier_column];
        if self.frontier_y.is_none() {
            if bottom == field.height {
                self.frontier_column += 1;
                return;
            }
            self.frontier_neighbours = [
                (x > 0).then(|| self.open_bottom[self.frontier_column - 1]),
                (x + 1 < field.wx).then(|| self.open_bottom[self.frontier_column + 1]),
                (z > 0).then(|| self.open_bottom[self.frontier_column - field.wx]),
                (z + 1 < field.wz).then(|| self.open_bottom[self.frontier_column + field.wx]),
            ];
            self.vertical_frontier = bottom > 0
                && self.opacity[field.cell(x, bottom - 1, z)] < MAX_LIGHT;
            self.frontier_end = self.frontier_neighbours.iter().flatten().copied()
                .max().unwrap_or(bottom).max(bottom + usize::from(self.vertical_frontier));
            if bottom == self.frontier_end {
                self.frontier_column += 1;
            } else {
                self.frontier_y = Some(bottom);
            }
            return;
        }
        let y = self.frontier_y.expect("prepared frontier");
        let [west, east, north, south] = self.frontier_neighbours;
        let horizontal = west.is_some_and(|open| {
            open > y && self.opacity[field.cell(x - 1, y, z)] < MAX_LIGHT
        }) || east.is_some_and(|open| {
            open > y && self.opacity[field.cell(x + 1, y, z)] < MAX_LIGHT
        }) || north.is_some_and(|open| {
            open > y && self.opacity[field.cell(x, y, z - 1)] < MAX_LIGHT
        }) || south.is_some_and(|open| {
            open > y && self.opacity[field.cell(x, y, z + 1)] < MAX_LIGHT
        });
        if horizontal || (y == bottom && self.vertical_frontier) {
            self.sky_buckets.push(MAX_LIGHT, field.cell(x, y, z) as u32);
        }
        if y + 1 == self.frontier_end {
            self.frontier_column += 1;
            self.frontier_y = None;
        } else {
            self.frontier_y = Some(y + 1);
        }
    }

    fn pack_batch(&mut self, budget: usize) -> usize {
        let mut count = 0;
        while count < budget && !matches!(self.stage, Stage::Done) {
            if self.packed.is_none() {
                let position = self.inputs.footprint.outputs[self.output];
                let slot = self.inputs.footprint.slot(position);
                self.pack_x = (slot % self.inputs.footprint.width) * EDGE;
                self.pack_z = (slot / self.inputs.footprint.width) * EDGE;
                self.packed = Some(ColumnLight::new(self.inputs.section_count));
                count += 1;
            } else if self.cell == NibbleArray::LEN {
                self.finish_section();
                count += 1;
            } else {
                let cells = (budget - count).min(NibbleArray::LEN - self.cell);
                self.pack_cells(cells);
                count += cells;
            }
        }
        count
    }

    fn pack_cells(&mut self, count: usize) {
        for cell in self.cell..self.cell + count {
            let x = cell % EDGE;
            let z = (cell / EDGE) % EDGE;
            let y = self.section * EDGE + cell / (EDGE * EDGE);
            let idx = self.inputs.field.cell(self.pack_x + x, y, self.pack_z + z);
            let sky = self.sky[idx];
            let block = self.block[idx];
            if cell == 0 {
                self.first_sky = sky;
                self.first_block = block;
            } else {
                self.uniform_sky &= sky == self.first_sky;
                self.uniform_block &= block == self.first_block;
            }
            let shift = 4 * (cell & 1);
            self.sky_bytes[cell >> 1] |= sky << shift;
            self.block_bytes[cell >> 1] |= block << shift;
        }
        self.cell += count;
    }

    fn finish_section(&mut self) {
        let packed = self.packed.as_mut().expect("initialized output");
        if self.highest[self.output].is_some_and(|highest| self.section == highest + 1)
            && self.uniform_sky && self.first_sky == MAX_LIGHT
        {
            self.sky_elide_after = Some(self.section + 1);
        }
        *packed.sky_mut(self.section) = if self.sky_elide_after.is_some_and(|after| self.section >= after) {
            LightData::Missing
        } else {
            packed_layer(self.uniform_sky, self.first_sky, &self.sky_bytes)
        };
        *packed.block_mut(self.section) = packed_layer(self.uniform_block, self.first_block, &self.block_bytes);
        self.section += 1;
        self.cell = 0;
        self.sky_bytes.fill(0);
        self.block_bytes.fill(0);
        self.uniform_sky = true;
        self.uniform_block = true;
        if self.section == self.inputs.section_count + 2 {
            self.results.push((self.inputs.footprint.outputs[self.output],
                self.packed.take().expect("finished output")));
            self.output += 1;
            self.section = 0;
            self.sky_elide_after = None;
            if self.output == self.inputs.footprint.outputs.len() {
                self.stage = Stage::Done;
            }
        }
    }
}

fn packed_layer(uniform: bool, first: u8, bytes: &[u8; 2048]) -> LightData {
    if uniform {
        LightData::Uniform(first)
    } else {
        LightData::Values(NibbleArray::from_bytes(bytes).expect("one light section"))
    }
}

fn flood_batch(
    field: &Field,
    opacity: &[u8],
    light: &mut [u8],
    buckets: &mut Buckets,
    level: &mut u8,
    budget: usize,
) -> usize {
    let mut count = 0;
    while count < budget && *level > 0 {
        flood_one(field, opacity, light, buckets, level);
        count += 1;
    }
    count
}

fn flood_one(field: &Field, opacity: &[u8], light: &mut [u8], buckets: &mut Buckets, level: &mut u8) {
    let Some(idx) = buckets.pop(*level) else {
        *level -= 1;
        return;
    };
    let idx = idx as usize;
    if light[idx] != *level {
        return;
    }
    let (x, y, z) = field.uncell(idx);
    for (nx, ny, nz) in neighbours(field, x, y, z) {
        let next = field.cell(nx, ny, nz);
        let value = (*level).saturating_sub(opacity[next].max(1));
        if value > light[next] {
            light[next] = value;
            buckets.push(value, next as u32);
        }
    }
}

#[cfg(test)]
mod tests {
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
}
