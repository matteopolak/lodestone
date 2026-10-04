//! Structure-terrain density: the term that lifts terrain under and around
//! structure pieces so villages sit on flat ground and strongholds are buried.
//!
//! Pure geometry in `f32`: nothing here reads noise or a seed. A caller selects
//! which pieces are in reach of a chunk (those within 12 blocks) and hands them
//! over as [`Rigid`] boxes and [`Junction`]s; this type reproduces the
//! reference's arithmetic and accumulation order exactly (rigid pieces first,
//! then junctions, each added in `f32`).

use std::sync::OnceLock;

use super::sampler::BeardifierSource;
use super::volume::Volume;

/// How a structure reshapes the terrain around its pieces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TerrainAdjustment {
    None,
    Bury,
    BeardThin,
    BeardBox,
    Encapsulate,
}

/// An inclusive axis-aligned box, `[x, y, z]` corners.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BoundingBox {
    pub min: [i32; 3],
    pub max: [i32; 3],
}

impl BoundingBox {
    fn union(self, o: Self) -> Self {
        Self {
            min: [self.min[0].min(o.min[0]), self.min[1].min(o.min[1]), self.min[2].min(o.min[2])],
            max: [self.max[0].max(o.max[0]), self.max[1].max(o.max[1]), self.max[2].max(o.max[2])],
        }
    }

    fn inflated(self, by: i32) -> Self {
        Self { min: self.min.map(|v| v - by), max: self.max.map(|v| v + by) }
    }

    fn contains(&self, x: i32, y: i32, z: i32) -> bool {
        x >= self.min[0] && x <= self.max[0] && y >= self.min[1] && y <= self.max[1] && z >= self.min[2] && z <= self.max[2]
    }
}

/// A rigid structure piece: its box, its structure's adjustment, and how far
/// above the box bottom its floor sits.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rigid {
    pub bounds: BoundingBox,
    pub adjustment: TerrainAdjustment,
    pub ground_level_delta: i32,
}

/// A jigsaw junction: a soft beard around the point where two pieces meet.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Junction {
    pub source_x: i32,
    pub source_ground_y: i32,
    pub source_z: i32,
}

const KERNEL_RADIUS: i32 = 12;
const KERNEL_SIZE: i32 = 24;

/// `exp(-|(dx, dy + 0.5, dz)|^2 / 16)` over the 24^3 offset cube, narrowed to
/// `f32`, indexed `[zi * 576 + xi * 24 + yi]` (Y innermost).
fn kernel() -> &'static [f32; 13824] {
    static KERNEL: OnceLock<Box<[f32; 13824]>> = OnceLock::new();
    KERNEL.get_or_init(|| {
        let mut k = Box::new([0.0f32; 13824]);
        for zi in 0..KERNEL_SIZE {
            for xi in 0..KERNEL_SIZE {
                for yi in 0..KERNEL_SIZE {
                    let (dx, dy, dz) = (f64::from(xi - KERNEL_RADIUS), f64::from(yi - KERNEL_RADIUS) + 0.5, f64::from(zi - KERNEL_RADIUS));
                    let d2 = dx * dx + dy * dy + dz * dz;
                    k[(zi * KERNEL_SIZE * KERNEL_SIZE + xi * KERNEL_SIZE + yi) as usize] = std::f64::consts::E.powf(-d2 / 16.0) as f32;
                }
            }
        }
        k
    })
}

fn length_squared(x: f32, y: f32, z: f32) -> f32 {
    x * x + y * y + z * z
}

/// One Newton step from the classic magic-constant seed, in `f64`.
fn fast_inv_sqrt(x: f64) -> f64 {
    let xhalf = 0.5 * x;
    let i = 6_910_469_410_427_058_090_i64.wrapping_sub((x.to_bits() as i64) >> 1);
    let y = f64::from_bits(i as u64);
    y * (1.5 - xhalf * y * y)
}

fn bury(dx: f32, dy: f32, dz: f32) -> f32 {
    let d2 = length_squared(dx, dy, dz);
    if d2 >= 36.0 { 0.0 } else { 1.0 - d2.sqrt() / 6.0 }
}

fn beard(dx: i32, dy: i32, dz: i32, y_to_ground: i32) -> f32 {
    let (xi, yi, zi) = (dx + KERNEL_RADIUS, dy + KERNEL_RADIUS, dz + KERNEL_RADIUS);
    let in_range = |i: i32| (0..KERNEL_SIZE).contains(&i);
    if !(in_range(xi) && in_range(yi) && in_range(zi)) {
        return 0.0;
    }
    let dy_off = y_to_ground as f32 + 0.5;
    let d2 = length_squared(dx as f32, dy_off, dz as f32);
    let inv = fast_inv_sqrt(f64::from(d2 / 2.0)) as f32;
    let value = -dy_off * inv / 2.0;
    value * kernel()[(zi * KERNEL_SIZE * KERNEL_SIZE + xi * KERNEL_SIZE + yi) as usize]
}

/// The structure-terrain term for one chunk's worth of in-reach pieces.
#[derive(Clone, Debug, Default)]
pub struct Beardifier {
    pieces: Vec<Rigid>,
    junctions: Vec<Junction>,
    affected: Option<BoundingBox>,
}

impl Beardifier {
    /// Builds the term. The affected box is the union of every piece box and
    /// junction point, inflated by 24; with no pieces the term is zero everywhere.
    pub fn new(pieces: Vec<Rigid>, junctions: Vec<Junction>) -> Self {
        let mut any: Option<BoundingBox> = None;
        let mut include = |b: BoundingBox| any = Some(any.map_or(b, |a| a.union(b)));
        for p in &pieces {
            include(p.bounds);
        }
        for j in &junctions {
            let at = [j.source_x, j.source_ground_y, j.source_z];
            include(BoundingBox { min: at, max: at });
        }
        Self { pieces, junctions, affected: any.map(|b| b.inflated(24)) }
    }

    pub fn is_empty(&self) -> bool {
        self.affected.is_none()
    }

    fn unchecked(&self, x: i32, y: i32, z: i32) -> f32 {
        let mut noise = 0.0f32;
        for r in &self.pieces {
            let b = r.bounds;
            let dx = 0.max((b.min[0] - x).max(x - b.max[0]));
            let dz = 0.max((b.min[2] - z).max(z - b.max[2]));
            let ground_y = b.min[1] + r.ground_level_delta;
            let dy_to_ground = y - ground_y;
            let dy = match r.adjustment {
                TerrainAdjustment::None => 0,
                TerrainAdjustment::Bury | TerrainAdjustment::BeardThin => dy_to_ground,
                TerrainAdjustment::BeardBox => 0.max((ground_y - y).max(y - b.max[1])),
                TerrainAdjustment::Encapsulate => 0.max((b.min[1] - y).max(y - b.max[1])),
            };
            noise += match r.adjustment {
                TerrainAdjustment::None => 0.0,
                TerrainAdjustment::Bury => bury(dx as f32, dy as f32 / 2.0, dz as f32),
                TerrainAdjustment::BeardThin | TerrainAdjustment::BeardBox => beard(dx, dy, dz, dy_to_ground) * 0.8,
                TerrainAdjustment::Encapsulate => bury(dx as f32 / 2.0, dy as f32 / 2.0, dz as f32 / 2.0) * 0.8,
            };
        }
        for j in &self.junctions {
            let (dx, dy, dz) = (x - j.source_x, y - j.source_ground_y, z - j.source_z);
            noise += beard(dx, dy, dz, dy) * 0.4;
        }
        noise
    }
}

impl BeardifierSource for Beardifier {
    fn value(&self, x: i32, y: i32, z: i32) -> f32 {
        match self.affected {
            Some(b) if b.contains(x, y, z) => self.unchecked(x, y, z),
            _ => 0.0,
        }
    }

    fn fill_volume(&self, out: &mut [f32], v: &Volume) {
        out.fill(0.0);
        let Some(b) = self.affected else { return };
        // Index range of lattice points whose cell can overlap the affected box.
        let hi = |axis: usize| (v.size[axis] - 1).min((b.max[axis] - v.min[axis]).div_euclid(v.step[axis]));
        let lo = |axis: usize| 0.max(b.min[axis] - v.min[axis]).div_euclid(v.step[axis]);
        // The box must also intersect the volume's block extent.
        for axis in 0..3 {
            if b.max[axis] < v.min[axis] || b.min[axis] > v.max_block(axis) {
                return;
            }
        }
        for z in lo(2)..=hi(2) {
            let bz = v.block_z(z);
            for x in lo(0)..=hi(0) {
                let bx = v.block_x(x);
                for y in lo(1)..=hi(1) {
                    out[v.index(x, y, z)] = self.unchecked(bx, v.block_y(y), bz);
                }
            }
        }
    }
}
