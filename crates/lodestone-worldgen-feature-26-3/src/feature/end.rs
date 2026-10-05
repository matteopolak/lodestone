//! End features: `end_spike`, `end_platform`, `end_gateway`, `end_island` and `chorus_plant`.

use lodestone_worldgen_core::rng::{LegacyRandomSource, RandomSource};
use serde_json::Value;

use crate::blocks::{Dir, State};
use crate::json::{Res, array, boolean, int};
use crate::level::{Level, PlacedBlockEntity};
use crate::pos::{Pos, Rng};

/// One obsidian pillar of the central island.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EndSpike {
    pub center_x: i32,
    pub center_z: i32,
    pub radius: i32,
    pub height: i32,
    pub guarded: bool,
}

/// The ten pillars a world seed gets: sizes `0..10` shuffled by a legacy random seeded with the
/// low 16 bits of the seed's first long, laid on a circle of radius 42 starting due west.
#[must_use]
pub fn spikes_for_seed(seed: i64) -> [EndSpike; 10] {
    let key = LegacyRandomSource::new(seed).next_long() & 0xFFFF;
    let mut random = LegacyRandomSource::new(key);
    let mut sizes: [i32; 10] = std::array::from_fn(|i| i as i32);
    for i in (2..=10).rev() {
        let to = random.next_int_bounded(i as i32) as usize;
        sizes.swap(i - 1, to);
    }
    std::array::from_fn(|i| {
        let angle = 2.0 * (-std::f64::consts::PI + std::f64::consts::PI / 10.0 * i as f64);
        let size = sizes[i];
        EndSpike {
            center_x: (42.0 * angle.cos()).floor() as i32,
            center_z: (42.0 * angle.sin()).floor() as i32,
            radius: 2 + size / 3,
            height: 76 + size * 3,
            guarded: size == 1 || size == 2,
        }
    })
}

#[derive(Clone, Debug)]
pub struct SpikeConfig {
    /// Explicit pillars; empty means the seed's own ten.
    pub spikes: Vec<EndSpike>,
}

impl SpikeConfig {
    pub fn parse(v: &Value, ctx: &str) -> Res<Self> {
        let mut spikes = Vec::new();
        for s in array(v, "spikes", ctx)? {
            let i = |k: &str| -> Res<i32> { s.get(k).map_or(Ok(0), |_| int(s, k, ctx)) };
            spikes.push(EndSpike {
                center_x: i("centerX")?,
                center_z: i("centerZ")?,
                radius: i("radius")?,
                height: i("height")?,
                guarded: boolean(s, "guarded", false, ctx)?,
            });
        }
        Ok(Self { spikes })
    }
}

/// Builds every pillar whose centre lies in the origin's chunk: an obsidian cylinder cleared of
/// anything above y 65 around it, an iron-bar cage on the guarded ones, and the crystal's
/// bedrock base and fire. The crystal entity itself is the server's to spawn; its yaw draw is
/// still taken here.
pub fn place_spikes(cfg: &SpikeConfig, level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let seeded;
    let spikes = if cfg.spikes.is_empty() {
        seeded = spikes_for_seed(level.seed);
        &seeded[..]
    } else {
        &cfg.spikes[..]
    };
    for s in spikes {
        if origin.x >> 4 == s.center_x >> 4 && origin.z >> 4 == s.center_z >> 4 {
            place_spike(level, rng, s);
        }
    }
    true
}

fn place_spike(level: &mut Level<'_>, rng: &mut Rng, s: &EndSpike) {
    let env = level.env;
    let blocks = &env.blocks;
    let by_name = |n: &str| blocks.default_state(blocks.block_by_name(n).expect("end block"));
    let obsidian = by_name("obsidian");
    let r = s.radius;
    for z in s.center_z - r..=s.center_z + r {
        for y in level.min_y..=s.height + 10 {
            for x in s.center_x - r..=s.center_x + r {
                let (dx, dz) = (f64::from(x - s.center_x), f64::from(z - s.center_z));
                if dx * dx + dz * dz <= f64::from(r * r + 1) && y < s.height {
                    level.set(x, y, z, obsidian);
                } else if y > 65 {
                    level.set(x, y, z, env.known.air);
                }
            }
        }
    }
    if s.guarded {
        let bars = by_name("iron_bars");
        let flag = |b: bool| if b { "true" } else { "false" };
        for dx in -2..=2 {
            for dz in -2..=2 {
                for dy in 0..=3 {
                    let side_x = dx == -2 || dx == 2;
                    let side_z = dz == -2 || dz == 2;
                    let top = dy == 3;
                    if !(side_x || side_z || top) {
                        continue;
                    }
                    let edge_x = side_x || top;
                    let edge_z = side_z || top;
                    let mut st = bars;
                    for (prop, on) in [("north", edge_x && dz != -2), ("south", edge_x && dz != 2), ("west", edge_z && dx != -2), ("east", edge_z && dx != 2)] {
                        st = blocks.with(st, prop, flag(on)).expect("iron bars side");
                    }
                    level.set(s.center_x + dx, s.height + dy, s.center_z + dz, st);
                }
            }
        }
    }
    let _yaw = rng.next_float();
    let (cx, cy, cz) = (s.center_x, s.height + 1, s.center_z);
    level.set(cx, cy - 1, cz, env.known.bedrock);
    // Bedrock is not a soul-fire base and has a sturdy top, so the fire takes its plain state.
    level.set(cx, cy, cz, by_name("fire"));
}

/// The obsidian landing platform: a 5x5 obsidian floor under three layers of air.
pub fn place_platform(level: &mut Level<'_>, origin: Pos) -> bool {
    let blocks = &level.env.blocks;
    let obsidian = blocks.default_state(blocks.block_by_name("obsidian").expect("obsidian"));
    let air = level.env.known.air;
    for dz in -2..=2 {
        for dx in -2..=2 {
            for dy in -1..3 {
                let want = if dy == -1 { obsidian } else { air };
                let (x, y, z) = (origin.x + dx, origin.y + dy, origin.z + dz);
                if blocks.block_of(level.get(x, y, z)) != blocks.block_of(want) {
                    level.set(x, y, z, want);
                }
            }
        }
    }
    true
}

#[derive(Clone, Debug)]
pub struct GatewayConfig {
    pub exit: Option<Pos>,
    pub exact: bool,
}

impl GatewayConfig {
    pub fn parse(v: &Value, ctx: &str) -> Res<Self> {
        let exit = match v.get("exit") {
            Some(Value::Array(a)) if a.len() == 3 => {
                let c = |i: usize| a[i].as_i64().map(|n| n as i32).ok_or_else(|| format!("{ctx}: exit coordinate"));
                Some(Pos::new(c(0)?, c(1)?, c(2)?))
            }
            Some(_) => return Err(format!("{ctx}: exit must be three integers")),
            None => None,
        };
        Ok(Self { exit, exact: boolean(v, "exact", false, ctx)? })
    }
}

/// A gateway block in a bedrock frame: bedrock above and below the gateway and on the four
/// vertical edges between them, air in the gateway's own layer and in the remaining cells.
pub fn place_gateway(cfg: &GatewayConfig, level: &mut Level<'_>, origin: Pos) -> bool {
    let env = level.env;
    let blocks = &env.blocks;
    let gateway = blocks.default_state(blocks.block_by_name("end_gateway").expect("end_gateway"));
    for dz in -1..=1 {
        for dy in -2i32..=2 {
            for dx in -1..=1 {
                let (same_x, same_y, same_z) = (dx == 0, dy == 0, dz == 0);
                let end = dy.abs() == 2;
                let (x, y, z) = (origin.x + dx, origin.y + dy, origin.z + dz);
                let s: State = if same_x && same_y && same_z {
                    gateway
                } else if same_y {
                    env.known.air
                } else if (end && same_x && same_z) || ((same_x || same_z) && !end) {
                    env.known.bedrock
                } else {
                    env.known.air
                };
                level.set(x, y, z, s);
            }
        }
    }
    if let Some(exit) = cfg.exit {
        level.attach_block_entity(PlacedBlockEntity::EndGateway { x: origin.x, y: origin.y, z: origin.z, exit, exact: cfg.exact });
    }
    true
}

/// A small floating island: end-stone discs shrinking by 0.5 or 1.5 per layer downward from
/// a radius of 4 to 6.
pub fn place_island(level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let blocks = &level.env.blocks;
    let end_stone = blocks.default_state(blocks.block_by_name("end_stone").expect("end_stone"));
    let mut size = rng.next_int_bounded(3) as f32 + 4.0;
    let mut y = 0;
    while size > 0.5 {
        for x in (-size).floor() as i32..=size.ceil() as i32 {
            for z in (-size).floor() as i32..=size.ceil() as i32 {
                if ((x * x + z * z) as f32) <= (size + 1.0) * (size + 1.0) {
                    level.set(origin.x + x, origin.y + y, origin.z + z, end_stone);
                }
            }
        }
        size -= rng.next_int_bounded(2) as f32 + 0.5;
        y -= 1;
    }
    true
}

/// A chorus plant grown on a supporting block (end stone), at most eight blocks out sideways.
pub fn place_chorus(level: &mut Level<'_>, rng: &mut Rng, origin: Pos) -> bool {
    let env = level.env;
    let blocks = &env.blocks;
    let below = blocks.block_of(level.get(origin.x, origin.y - 1, origin.z));
    if !blocks.is_air(level.get(origin.x, origin.y, origin.z)) || !env.tags.get("supports_chorus_plant").expect("tag").contains(below) {
        return false;
    }
    let plant = Chorus::new(level);
    plant.set_connected(level, origin);
    plant.grow(level, rng, origin, origin, 8, 0);
    true
}

struct Chorus {
    plant: State,
    flower_aged: State,
}

impl Chorus {
    fn new(level: &Level<'_>) -> Self {
        let blocks = &level.env.blocks;
        let flower = blocks.default_state(blocks.block_by_name("chorus_flower").expect("chorus_flower"));
        Self {
            plant: blocks.default_state(blocks.block_by_name("chorus_plant").expect("chorus_plant")),
            flower_aged: blocks.with(flower, "age", "5").expect("flower age"),
        }
    }

    fn empty(level: &Level<'_>, p: Pos) -> bool {
        level.env.blocks.is_air(level.get(p.x, p.y, p.z))
    }

    fn horizontal_empty(level: &Level<'_>, p: Pos, ignore: Option<Dir>) -> bool {
        Dir::HORIZONTAL.iter().all(|&d| Some(d) == ignore || Self::empty(level, p.relative(d)))
    }

    /// A plant block connected to each neighbouring plant or flower (and downward to its support).
    fn set_connected(&self, level: &mut Level<'_>, p: Pos) {
        let env = level.env;
        let blocks = &env.blocks;
        let plant = blocks.block_of(self.plant);
        let flower = blocks.block_of(self.flower_aged);
        let support = env.tags.get("supports_chorus_plant").expect("tag");
        let mut s = self.plant;
        for d in Dir::ALL {
            let n = p.relative(d);
            let b = blocks.block_of(level.get(n.x, n.y, n.z));
            let on = b == plant || b == flower || (d == Dir::Down && support.contains(b));
            s = blocks.with(s, d.name(), if on { "true" } else { "false" }).expect("chorus side");
        }
        level.set(p.x, p.y, p.z, s);
    }

    fn grow(&self, level: &mut Level<'_>, rng: &mut Rng, current: Pos, start: Pos, spread: i32, depth: i32) {
        let mut height = rng.next_int_bounded(4) + 1;
        if depth == 0 {
            height += 1;
        }
        for i in 0..height {
            let t = current.offset(0, i + 1, 0);
            if !Self::horizontal_empty(level, t, None) {
                return;
            }
            self.set_connected(level, t);
            self.set_connected(level, t.below());
        }
        let mut stem = false;
        if depth < 4 {
            let mut stems = rng.next_int_bounded(4);
            if depth == 0 {
                stems += 1;
            }
            for _ in 0..stems {
                let d = Dir::HORIZONTAL[rng.next_int_bounded(4) as usize];
                let t = current.offset(0, height, 0).relative(d);
                if (t.x - start.x).abs() < spread
                    && (t.z - start.z).abs() < spread
                    && Self::empty(level, t)
                    && Self::empty(level, t.below())
                    && Self::horizontal_empty(level, t, Some(d.opposite()))
                {
                    stem = true;
                    self.set_connected(level, t);
                    self.set_connected(level, t.relative(d.opposite()));
                    self.grow(level, rng, t, start, spread, depth + 1);
                }
            }
        }
        if !stem {
            let p = current.offset(0, height, 0);
            level.set(p.x, p.y, p.z, self.flower_aged);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pillar sits on the radius-42 circle at its own tenth of a turn, and the sizes are a
    /// permutation of 0..10 (radius `2 + size / 3`, height `76 + 3 * size`).
    #[test]
    fn spikes_are_a_permutation_on_the_circle() {
        let spikes = spikes_for_seed(42);
        let mut sizes: Vec<i32> = spikes.iter().map(|s| (s.height - 76) / 3).collect();
        for s in &spikes {
            let size = (s.height - 76) / 3;
            assert_eq!(s.radius, 2 + size / 3);
            assert_eq!(s.guarded, size == 1 || size == 2);
        }
        assert_eq!((spikes[0].center_x, spikes[0].center_z), (42, 0));
        sizes.sort_unstable();
        assert_eq!(sizes, (0..10).collect::<Vec<_>>());
    }
}
