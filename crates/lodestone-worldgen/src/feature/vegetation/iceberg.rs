//! Iceberg configured-feature placement.
//!
//! The iceberg body grows a packed-ice or blue-ice mass around sea level, then
//! smooths unsupported over-water blocks and optionally carves an air/water
//! cavity. The configured state is the only data-dependent part; the geometry
//! and random draw order are fixed by the feature family.

use crate::feature::BlockPos;
use crate::rng::RandomSource;

use super::grid::VegGrid;

/// Parsed state for one iceberg configured feature.
#[derive(Clone, Debug)]
pub struct IcebergCfg {
    pub(super) state: String,
}

const SEA_LEVEL: i32 = super::features::SEA_LEVEL;
const AIR: &str = "minecraft:air";
const WATER: &str = "minecraft:water";
const SNOW_BLOCK: &str = "minecraft:snow_block";
const PACKED_ICE: &str = "minecraft:packed_ice";
const BLUE_ICE: &str = "minecraft:blue_ice";
const ICE: &str = "minecraft:ice";
const SNOW: &str = "minecraft:snow";

fn base_at(grid: &VegGrid, x: i32, y: i32, z: i32) -> &str {
    super::base_id(grid.get(x, y, z))
}

fn iceberg_state(state: &str) -> bool {
    matches!(state, PACKED_ICE | SNOW_BLOCK | BLUE_ICE)
}

fn circle_distance(x: i32, z: i32, radius: i32, random: &mut impl RandomSource) -> f64 {
    let offset = 10.0 * f64::from(random.next_float().clamp(0.2, 0.8)) / f64::from(radius);
    offset + f64::from(x * x + z * z) - f64::from(radius * radius)
}

fn ellipse_distance(x: i32, z: i32, a: i32, c: i32, angle: f64) -> f64 {
    let (sin, cos) = angle.sin_cos();
    let xr = (f64::from(x) * cos - f64::from(z) * sin) / f64::from(a);
    let zr = (f64::from(x) * sin + f64::from(z) * cos) / f64::from(c);
    xr * xr + zr * zr - 1.0
}

fn ellipse_c(y: i32, height: i32, shape_c: i32) -> i32 {
    let mut c = shape_c;
    if y > 0 && height - y <= 3 {
        c -= 4 - (height - y);
    }
    c
}

fn round_radius(random: &mut impl RandomSource, y: i32, height: i32, width: i32) -> i32 {
    let k = 3.5 - random.next_float();
    let mut scale = (1.0 - (y * y) as f32 / (height as f32 * k)) * width as f32;
    if height > 15 + random.next_int_bounded(5) {
        let temporary_y = if y < 3 + random.next_int_bounded(6) { y / 2 } else { y };
        scale = (1.0 - temporary_y as f32 / (height as f32 * k * 0.4)) * width as f32;
    }
    (scale / 2.0).ceil() as i32
}

fn ellipse_radius(y: i32, height: i32, width: i32) -> i32 {
    (((1.0 - (y * y) as f32 / height as f32) * width as f32) / 2.0).ceil() as i32
}

fn steep_radius(random: &mut impl RandomSource, y: i32, height: i32, width: i32) -> i32 {
    let k = 1.0 + random.next_float() / 2.0;
    (((1.0 - y as f32 / (height as f32 * k)) * width as f32) / 2.0).ceil() as i32
}

fn set_block(grid: &mut VegGrid, x: i32, y: i32, z: i32, state: &str) {
    let _ = grid.set_if_in_bounds(x, y, z, state.to_owned());
}

fn set_iceberg_block(
    grid: &mut VegGrid,
    random: &mut impl RandomSource,
    pos: BlockPos,
    h_diff: i32,
    height: i32,
    ellipse: bool,
    snow_on_top: bool,
    main_state: &str,
) {
    let current = base_at(grid, pos.x, pos.y, pos.z);
    if !matches!(current, AIR | SNOW_BLOCK | ICE | WATER) {
        return;
    }
    let randomness = !ellipse || random.next_double() > 0.05;
    let divisor = if ellipse { 3 } else { 2 };
    if snow_on_top
        && current != WATER
        && f64::from(h_diff) <= f64::from(random.next_int_bounded((height / divisor).max(1)))
            + f64::from(height) * 0.6
        && randomness
    {
        set_block(grid, pos.x, pos.y, pos.z, SNOW_BLOCK);
    } else {
        set_block(grid, pos.x, pos.y, pos.z, main_state);
    }
}

fn generate_block(
    grid: &mut VegGrid,
    random: &mut impl RandomSource,
    origin: BlockPos,
    height: i32,
    x: i32,
    y: i32,
    z: i32,
    radius: i32,
    a: i32,
    ellipse: bool,
    shape_c: i32,
    angle: f64,
    snow_on_top: bool,
    main_state: &str,
) {
    let signed = if ellipse {
        ellipse_distance(x, z, a, ellipse_c(y, height, shape_c), angle)
    } else {
        circle_distance(x, z, radius, random)
    };
    if signed >= 0.0 {
        return;
    }
    let compare = if ellipse {
        -0.5
    } else {
        -6.0 - f64::from(random.next_int_bounded(3))
    };
    if signed > compare && random.next_double() > 0.9 {
        return;
    }
    set_iceberg_block(
        grid,
        random,
        BlockPos { x: origin.x + x, y: origin.y + y, z: origin.z + z },
        height - y,
        height,
        ellipse,
        snow_on_top,
        main_state,
    );
}

fn smooth(grid: &mut VegGrid, origin: BlockPos, width: i32, height: i32, ellipse: bool, ellipse_a: i32) {
    let a = if ellipse { ellipse_a } else { width / 2 };
    for x in -a..=a {
        for z in -a..=a {
            for y in 0..=height {
                let pos = BlockPos { x: origin.x + x, y: origin.y + y, z: origin.z + z };
                let current = base_at(grid, pos.x, pos.y, pos.z);
                if !iceberg_state(current) && current != SNOW {
                    continue;
                }
                if base_at(grid, pos.x, pos.y - 1, pos.z) == AIR {
                    set_block(grid, pos.x, pos.y, pos.z, AIR);
                    set_block(grid, pos.x, pos.y + 1, pos.z, AIR);
                    continue;
                }
                if !iceberg_state(current) {
                    continue;
                }
                let sides = [
                    base_at(grid, pos.x - 1, pos.y, pos.z),
                    base_at(grid, pos.x + 1, pos.y, pos.z),
                    base_at(grid, pos.x, pos.y, pos.z - 1),
                    base_at(grid, pos.x, pos.y, pos.z + 1),
                ];
                if sides.iter().filter(|state| !iceberg_state(state)).count() >= 3 {
                    set_block(grid, pos.x, pos.y, pos.z, AIR);
                }
            }
        }
    }
}

fn carve(
    grid: &mut VegGrid,
    radius: i32,
    y: i32,
    under_water: bool,
    angle: f64,
    local_origin: (i32, i32),
    ellipse_a: i32,
    ellipse_c_value: i32,
    global_origin: BlockPos,
) {
    let a = radius + 1 + ellipse_a / 3;
    let c = (radius - 3).min(3) + ellipse_c_value / 2 - 1;
    for x in -a..a {
        for z in -a..a {
            let signed = ellipse_distance(x - local_origin.0, z - local_origin.1, a, c, angle);
            if signed >= 0.0 {
                continue;
            }
            let pos = BlockPos { x: global_origin.x + x, y: global_origin.y + y, z: global_origin.z + z };
            let current = base_at(grid, pos.x, pos.y, pos.z);
            if iceberg_state(current) || current == SNOW_BLOCK {
                set_block(grid, pos.x, pos.y, pos.z, if under_water { WATER } else { AIR });
                if !under_water && base_at(grid, pos.x, pos.y + 1, pos.z) == SNOW {
                    set_block(grid, pos.x, pos.y + 1, pos.z, AIR);
                }
            }
        }
    }
}

fn cut_out(
    grid: &mut VegGrid,
    random: &mut impl RandomSource,
    width: i32,
    height: i32,
    origin: BlockPos,
    ellipse: bool,
    ellipse_a: i32,
    angle: f64,
    ellipse_c_value: i32,
) {
    let sign_x = if random.next_bool() { -1 } else { 1 };
    let sign_z = if random.next_bool() { -1 } else { 1 };
    let mut x = random.next_int_bounded((width / 2 - 2).max(1));
    if random.next_bool() {
        x = width / 2 + 1 - random.next_int_bounded((width - width / 2 - 1).max(1));
    }
    let mut z = random.next_int_bounded((width / 2 - 2).max(1));
    if random.next_bool() {
        z = width / 2 + 1 - random.next_int_bounded((width - width / 2 - 1).max(1));
    }
    if ellipse {
        x = random.next_int_bounded((ellipse_a - 5).max(1));
        z = x;
    }
    let local_origin = (sign_x * x, sign_z * z);
    let cut_angle = if ellipse { angle + std::f64::consts::FRAC_PI_2 } else { random.next_double() * std::f64::consts::TAU };
    for y in 0..height - 3 {
        let radius = round_radius(random, y, height, width);
        carve(grid, radius, y, false, cut_angle, local_origin, ellipse_a, ellipse_c_value, origin);
    }
    for y in -1..(-height + random.next_int_bounded(5)).min(0) {
        let radius = steep_radius(random, -y, height, width);
        carve(grid, radius, y, true, cut_angle, local_origin, ellipse_a, ellipse_c_value, origin);
    }
}

/// Places one iceberg at the overworld sea level.
pub(super) fn place_iceberg<R: RandomSource>(random: &mut R, origin: BlockPos, cfg: &IcebergCfg, grid: &mut VegGrid) {
    let origin = BlockPos { x: origin.x, y: SEA_LEVEL, z: origin.z };
    let snow_on_top = random.next_double() > 0.7;
    let angle = random.next_double() * std::f64::consts::TAU;
    let ellipse_a = 11 - random.next_int_bounded(5);
    let ellipse_c_value = 3 + random.next_int_bounded(3);
    let ellipse = random.next_double() > 0.7;
    let over_height = if ellipse { random.next_int_bounded(6) + 6 } else { random.next_int_bounded(15) + 3 };
    let over_height = if !ellipse && random.next_double() > 0.9 {
        over_height + random.next_int_bounded(19) + 7
    } else { over_height };
    let under_height = (over_height + random.next_int_bounded(11)).min(18);
    let width = (over_height + random.next_int_bounded(7) - random.next_int_bounded(5)).min(11);
    let a = if ellipse { ellipse_a } else { 11 };

    for x in -a..a {
        for z in -a..a {
            for y in 0..over_height {
                let radius = if ellipse { ellipse_radius(y, over_height, width) } else { round_radius(random, y, over_height, width) };
                if ellipse || x < radius {
                    generate_block(grid, random, origin, over_height, x, y, z, radius, a, ellipse, ellipse_c_value, angle, snow_on_top, &cfg.state);
                }
            }
        }
    }
    smooth(grid, origin, width, over_height, ellipse, ellipse_a);
    for x in -a..a {
        for z in -a..a {
            for y in -1..-under_height {
                let new_a = if ellipse {
                    ((a as f32 * (1.0 - (y * y) as f32 / (under_height as f32 * 8.0))).ceil()) as i32
                } else { a };
                let radius = steep_radius(random, -y, under_height, width);
                if x < radius {
                    generate_block(grid, random, origin, under_height, x, y, z, radius, new_a, ellipse, ellipse_c_value, angle, snow_on_top, &cfg.state);
                }
            }
        }
    }
    let do_cutout = if ellipse { random.next_double() > 0.1 } else { random.next_double() > 0.7 };
    if do_cutout {
        cut_out(grid, random, width, over_height, origin, ellipse, ellipse_a, angle, ellipse_c_value);
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::{place_iceberg, IcebergCfg, SEA_LEVEL};
    use crate::feature::BlockPos;
    use crate::feature::vegetation::grid::VegGrid;
    use crate::rng::{WorldgenRandom, XoroshiroRandomSource};

    #[test]
    fn packed_ice_matches_the_independent_compiled_server_fixture() {
        let mut grid = VegGrid::new(-64, 384, 0, 0);
        for x in -16..=16 {
            for z in -16..=16 {
                for y in -32..=100 {
                    grid.seed(x, y, z, if y <= SEA_LEVEL { "minecraft:water" } else { "minecraft:air" }.to_owned());
                }
            }
        }
        let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
        place_iceberg(
            &mut random,
            BlockPos { x: 0, y: 0, z: 0 },
            &IcebergCfg { state: "minecraft:packed_ice".to_owned() },
            &mut grid,
        );

        let mut expected = BTreeMap::new();
        for line in include_str!("../../../tests/support/iceberg_feature_jvm.txt").lines() {
            let mut parts = line.split_whitespace();
            let position = parts.next().expect("fixture position");
            let state = parts.next().expect("fixture state");
            let mut coords = position.split(',');
            let x = coords.next().unwrap().parse::<i32>().unwrap();
            let y = coords.next().unwrap().parse::<i32>().unwrap();
            let z = coords.next().unwrap().parse::<i32>().unwrap();
            expected.insert((x, y, z), state.to_owned());
        }

        let mut actual = BTreeMap::new();
        for x in -16..=16 {
            for z in -16..=16 {
                for y in -32..=100 {
                    let baseline = if y <= SEA_LEVEL { "minecraft:water" } else { "minecraft:air" };
                    let state = grid.get(x, y, z);
                    if state != baseline {
                        actual.insert((x, y, z), state.to_owned());
                    }
                }
            }
        }
        assert_eq!(actual, expected, "iceberg geometry or random draw order diverged from the external fixture");
        assert_eq!(actual.len(), 699, "fixture must remain non-vacuous");
    }
}
