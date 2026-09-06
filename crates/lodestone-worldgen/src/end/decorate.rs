//! End-only biome-decoration entries that can be represented by an
//! [`super::EndColumn`].
//!
//! The common vegetation decoder deliberately does not own these feature types:
//! their placement rules either have End-specific geometry or need data that a
//! block column cannot carry.  Keeping the small supported subset here makes the
//! boundary explicit instead of silently treating an End document as an
//! Overworld vegetation document.

use serde_json::Value;

use crate::dense_grid::DenseBlockGrid;
use crate::density::Resolver;
use crate::rng::{RandomSource, WorldgenRandom, XoroshiroRandomSource};

use super::{END_HIGHLANDS, SMALL_END_ISLANDS, THE_END};

/// The exit metadata attached to a generated return gateway.  The block itself
/// belongs in the palette; this record is the data the gateway block entity
/// needs in order to be functional.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EndGateway {
    /// Gateway block position.
    pub pos: (i32, i32, i32),
    /// Exact destination declared by the configured feature.
    pub exit: (i32, i32, i32),
    /// Whether the exit bypasses a destination search.
    pub exact: bool,
}

/// One fixed platform origin read from the `end_platform` placed-feature data.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PlatformOrigin {
    x: i32,
    y: i32,
    z: i32,
}

/// End decoration with a block-column representation.
///
/// The fixed platform, outer islands, chorus plants, and return-gateway blocks
/// all write into the three-by-three region. A return gateway also creates an
/// [`EndGateway`] sidecar; spikes create crystals and remain gameplay-driven.
#[derive(Debug, Clone, Default)]
pub(crate) struct EndDecoration {
    platforms: Vec<PlatformOrigin>,
    outer_islands: bool,
    chorus: bool,
    gateway_return: bool,
}

impl EndDecoration {
    pub(crate) fn from_resolver(resolver: &dyn Resolver) -> Self {
        let document = resolver.biome_document(THE_END);
        let Some(entries) = document
            .get("features")
            .and_then(Value::as_array)
            .and_then(|steps| steps.get(10))
            .and_then(Value::as_array)
        else {
            return Self::default();
        };

        let mut platforms = Vec::new();
        for entry in entries {
            let Some(id) = entry.as_str() else {
                continue;
            };
            let placed = resolver.placed_feature(id);
            let Some(configured_id) = placed.get("feature").and_then(Value::as_str) else {
                continue;
            };
            if resolver
                .configured_feature(configured_id)
                .get("type")
                .and_then(Value::as_str)
                != Some("minecraft:end_platform")
            {
                continue;
            }
            let Some(placement) = placed.get("placement").and_then(Value::as_array) else {
                continue;
            };
            for modifier in placement {
                if modifier.get("type").and_then(Value::as_str) != Some("minecraft:fixed_placement") {
                    continue;
                }
                let Some(positions) = modifier.get("positions").and_then(Value::as_array) else {
                    continue;
                };
                for position in positions {
                    let Some(position) = position.as_array() else {
                        continue;
                    };
                    let [x, y, z] = position.as_slice() else {
                        continue;
                    };
                    let (Some(x), Some(y), Some(z)) = (x.as_i64(), y.as_i64(), z.as_i64()) else {
                        continue;
                    };
                    let (Ok(x), Ok(y), Ok(z)) = (i32::try_from(x), i32::try_from(y), i32::try_from(z)) else {
                        continue;
                    };
                    platforms.push(PlatformOrigin { x, y, z });
                }
            }
        }
        Self {
            platforms,
            outer_islands: feature_in_step(resolver, SMALL_END_ISLANDS, 0, "minecraft:end_island"),
            chorus: feature_in_step(resolver, END_HIGHLANDS, 9, "minecraft:chorus_plant"),
            gateway_return: feature_in_step(resolver, END_HIGHLANDS, 4, "minecraft:end_gateway"),
        }
    }

    /// Applies every fixed platform which intersects this materialized column.
    ///
    /// Feature execution may be requested from any main-island chunk, but every
    /// invocation targets the same fixed coordinate.  Restricting writes to the
    /// materialized column makes independently requested columns converge on the
    /// same world state without a shared cache.
    pub(crate) fn apply(&self, world: &mut DenseBlockGrid) {
        for origin in &self.platforms {
            for dz in -2..=2 {
                for dx in -2..=2 {
                    world.set(origin.x + dx, origin.y - 1, origin.z + dz, "minecraft:obsidian");
                    for dy in 0..3 {
                        world.set(origin.x + dx, origin.y + dy, origin.z + dz, "minecraft:air");
                    }
                }
            }
        }
    }

    /// Executes the End features whose source chunks may write into `cx,cz`.
    /// The 3×3 source window is the feature write radius: an outer island's
    /// disc and a chorus plant may cross one chunk boundary.
    pub(crate) fn apply_region(
        &self,
        seed: i64,
        cx: i32,
        cz: i32,
        world: &mut DenseBlockGrid,
        biome_at_chunk: impl Fn(i32, i32) -> &'static str,
    ) -> Vec<EndGateway> {
        self.apply(world);
        let mut gateways = Vec::new();
        for source_x in cx - 1..=cx + 1 {
            for source_z in cz - 1..=cz + 1 {
                let biome = biome_at_chunk(source_x, source_z);
                let mut random = WorldgenRandom::new(XoroshiroRandomSource::new(0));
                let decoration_seed = random.set_decoration_seed(seed, source_x * 16, source_z * 16);
                if biome == SMALL_END_ISLANDS && self.outer_islands {
                    random.set_feature_seed(decoration_seed, 0, 0);
                    if random.next_float() < 1.0 / 14.0 {
                        let count = if random.next_int_bounded(4) < 3 { 1 } else { 2 };
                        for _ in 0..count {
                            let x = source_x * 16 + random.next_int_bounded(16);
                            let z = source_z * 16 + random.next_int_bounded(16);
                            let y = 55 + random.next_int_bounded(16);
                            place_outer_island(world, &mut random, (x, y, z));
                        }
                    }
                }
                if biome == END_HIGHLANDS && self.gateway_return {
                    random.set_feature_seed(decoration_seed, 0, 4);
                    if random.next_float() < 1.0 / 700.0 {
                        let x = source_x * 16 + random.next_int_bounded(16);
                        let z = source_z * 16 + random.next_int_bounded(16);
                        let y = surface_y(world, x, z) + 3 + random.next_int_bounded(7);
                        write_gateway(world, (x, y, z));
                        if x.div_euclid(16) == cx && z.div_euclid(16) == cz {
                            gateways.push(EndGateway { pos: (x, y, z), exit: (100, 50, 0), exact: true });
                        }
                    }
                }
                if biome == END_HIGHLANDS && self.chorus {
                    random.set_feature_seed(decoration_seed, 0, 9);
                    for _ in 0..random.next_int_bounded(5) {
                        let x = source_x * 16 + random.next_int_bounded(16);
                        let z = source_z * 16 + random.next_int_bounded(16);
                        let y = surface_y(world, x, z);
                        if world.get(x, y, z) == "minecraft:air" && world.get(x, y - 1, z) == "minecraft:end_stone" {
                            grow_chorus(world, &mut random, (x, y, z), (x, y, z), 0);
                        }
                    }
                }
            }
        }
        gateways
    }

}

fn is_chorus(state: &str) -> bool {
    state.starts_with("minecraft:chorus_plant") || state.starts_with("minecraft:chorus_flower")
}

fn chorus_plant_state(world: &DenseBlockGrid, pos: (i32, i32, i32)) -> String {
    let chorus_at = |x, y, z| is_chorus(world.get(x, y, z));
    format!(
        "minecraft:chorus_plant[down={},east={},north={},south={},up={},west={}]",
        chorus_at(pos.0, pos.1 - 1, pos.2),
        chorus_at(pos.0 + 1, pos.1, pos.2),
        chorus_at(pos.0, pos.1, pos.2 - 1),
        chorus_at(pos.0, pos.1, pos.2 + 1),
        chorus_at(pos.0, pos.1 + 1, pos.2),
        chorus_at(pos.0 - 1, pos.1, pos.2),
    )
}

fn set_chorus_plant(world: &mut DenseBlockGrid, pos: (i32, i32, i32)) {
    world.set(pos.0, pos.1, pos.2, &chorus_plant_state(world, pos));
}

fn horizontally_empty(world: &DenseBlockGrid, pos: (i32, i32, i32), ignore: Option<(i32, i32)>) -> bool {
    [(-1, 0), (1, 0), (0, -1), (0, 1)].into_iter().all(|(dx, dz)| {
        ignore == Some((dx, dz)) || world.get(pos.0 + dx, pos.1, pos.2 + dz) == "minecraft:air"
    })
}

fn grow_chorus<R: RandomSource>(
    world: &mut DenseBlockGrid,
    random: &mut R,
    current: (i32, i32, i32),
    start: (i32, i32, i32),
    depth: i32,
) {
    set_chorus_plant(world, current);
    let height = random.next_int_bounded(4) + 1 + i32::from(depth == 0);
    for i in 0..height {
        let target = (current.0, current.1 + i + 1, current.2);
        if !horizontally_empty(world, target, None) { return; }
        set_chorus_plant(world, target);
        set_chorus_plant(world, (target.0, target.1 - 1, target.2));
    }
    let mut branched = false;
    if depth < 4 {
        let stems = random.next_int_bounded(4) + i32::from(depth == 0);
        for _ in 0..stems {
            let (dx, dz) = [(0, -1), (1, 0), (0, 1), (-1, 0)][random.next_int_bounded(4) as usize];
            let target = (current.0 + dx, current.1 + height, current.2 + dz);
            if (target.0 - start.0).abs() < 8
                && (target.2 - start.2).abs() < 8
                && world.get(target.0, target.1, target.2) == "minecraft:air"
                && world.get(target.0, target.1 - 1, target.2) == "minecraft:air"
                && horizontally_empty(world, target, Some((-dx, -dz)))
            {
                branched = true;
                set_chorus_plant(world, target);
                set_chorus_plant(world, (target.0 - dx, target.1, target.2 - dz));
                grow_chorus(world, random, target, start, depth + 1);
            }
        }
    }
    if !branched {
        world.set(current.0, current.1 + height, current.2, "minecraft:chorus_flower[age=5]");
    }
}

fn feature_in_step(resolver: &dyn Resolver, biome: &str, step: usize, kind: &str) -> bool {
    resolver
        .biome_document(biome)
        .get("features")
        .and_then(Value::as_array)
        .and_then(|steps| steps.get(step))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .any(|id| {
            let placed = resolver.placed_feature(id);
            placed
                .get("feature")
                .and_then(Value::as_str)
                .is_some_and(|configured| resolver.configured_feature(configured).get("type").and_then(Value::as_str) == Some(kind))
        })
}

fn surface_y(world: &DenseBlockGrid, x: i32, z: i32) -> i32 {
    for y in (0..128).rev() {
        if world.get(x, y, z) != "minecraft:air" {
            return y + 1;
        }
    }
    0
}

fn write_gateway(world: &mut DenseBlockGrid, origin: (i32, i32, i32)) {
    for y in origin.1 - 2..=origin.1 + 2 {
        for x in origin.0 - 1..=origin.0 + 1 {
            for z in origin.2 - 1..=origin.2 + 1 {
                let same_x = x == origin.0;
                let same_y = y == origin.1;
                let same_z = z == origin.2;
                let end = (y - origin.1).abs() == 2;
                let state = if same_x && same_y && same_z {
                    "minecraft:end_gateway"
                } else if same_y {
                    "minecraft:air"
                } else if (end && same_x && same_z) || ((same_x || same_z) && !end) {
                    "minecraft:bedrock"
                } else {
                    "minecraft:air"
                };
                world.set(x, y, z, state);
            }
        }
    }
}

/// Writes one outer-island feature at its already-selected origin.
///
/// The region driver performs its rarity/count/in-square selection once per
/// surrounding source chunk, then this primitive writes the resulting shape.
pub(crate) fn place_outer_island<R: RandomSource>(
    world: &mut DenseBlockGrid,
    random: &mut R,
    origin: (i32, i32, i32),
) {
    let mut radius = random.next_int_bounded(3) as f32 + 4.0;
    let mut y_offset = 0;
    while radius > 0.5 {
        let lower = (-radius).floor() as i32;
        let upper = radius.ceil() as i32;
        for x_offset in lower..=upper {
            for z_offset in lower..=upper {
                if (x_offset * x_offset + z_offset * z_offset) as f32 <= (radius + 1.0) * (radius + 1.0) {
                    world.set(
                        origin.0 + x_offset,
                        origin.1 + y_offset,
                        origin.2 + z_offset,
                        "minecraft:end_stone",
                    );
                }
            }
        }
        radius -= random.next_int_bounded(2) as f32 + 0.5;
        y_offset -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::LegacyRandomSource;

    /// The native feature fixture is intentionally a direct feature invocation:
    /// placement modifiers belong to the future three-by-three region driver.
    #[test]
    fn outer_island_shape_matches_the_independent_feature_fixture() {
        let mut world = DenseBlockGrid::new(-32, 0, -32, 64, 128, 64, "minecraft:air");
        let mut random = LegacyRandomSource::new(918_273);
        place_outer_island(&mut world, &mut random, (24, 70, 24));

        let mut writes = 0usize;
        for line in include_str!("../../tests/support/end_decoration_jvm.txt").lines() {
            let mut words = line.split_whitespace();
            if words.next() != Some("island") {
                continue;
            }
            let mut position = words.next().expect("island position").split(',');
            let x: i32 = position.next().expect("x").parse().expect("integer x");
            let y: i32 = position.next().expect("y").parse().expect("integer y");
            let z: i32 = position.next().expect("z").parse().expect("integer z");
            assert!(position.next().is_none(), "extra island coordinate: {line}");
            let state = words.next().expect("island state");
            assert!(words.next().is_none(), "trailing island fixture data: {line}");
            assert_eq!(world.get(x, y, z), state, "island fixture write ({x},{y},{z})");
            writes += 1;
        }
        assert_eq!(writes, 402, "fixture must exercise a non-trivial island");
    }

    #[test]
    fn chorus_shape_matches_the_independent_feature_fixture() {
        let mut world = DenseBlockGrid::new(-32, 0, -32, 64, 128, 64, "minecraft:air");
        world.set(0, 64, 0, "minecraft:end_stone");
        let mut random = LegacyRandomSource::new(12_345);
        grow_chorus(&mut world, &mut random, (0, 65, 0), (0, 65, 0), 0);

        let mut writes = 0usize;
        for line in include_str!("../../tests/support/end_decoration_jvm.txt").lines() {
            let mut words = line.split_whitespace();
            if words.next() != Some("chorus") {
                continue;
            }
            let mut position = words.next().expect("chorus coordinate").split(',');
            let x: i32 = position.next().expect("x").parse().expect("integer x");
            let y: i32 = position.next().expect("y").parse().expect("integer y");
            let z: i32 = position.next().expect("z").parse().expect("integer z");
            assert!(position.next().is_none(), "extra coordinate: {line}");
            let state = words.next().expect("chorus state");
            assert!(words.next().is_none(), "trailing chorus fixture data: {line}");
            assert_eq!(world.get(x, y, z), state, "chorus write ({x}, {y}, {z})");
            writes += 1;
        }
        assert_eq!(writes, 19, "fixture must include every native chorus write");
    }

    #[test]
    fn return_gateway_shape_and_exit_match_the_independent_feature_fixture() {
        let mut world = DenseBlockGrid::new(0, 0, 0, 128, 128, 128, "minecraft:air");
        write_gateway(&mut world, (50, 70, 50));

        let mut writes = 0usize;
        let mut exit = None;
        for line in include_str!("../../tests/support/end_decoration_jvm.txt").lines() {
            let mut words = line.split_whitespace();
            match words.next() {
                Some("gateway") => {
                    let mut position = words.next().expect("gateway coordinate").split(',');
                    let x: i32 = position.next().expect("x").parse().expect("integer x");
                    let y: i32 = position.next().expect("y").parse().expect("integer y");
                    let z: i32 = position.next().expect("z").parse().expect("integer z");
                    assert!(position.next().is_none(), "extra coordinate: {line}");
                    let state = words.next().expect("gateway state");
                    assert!(words.next().is_none(), "trailing gateway fixture data: {line}");
                    assert_eq!(world.get(x, y, z), state, "gateway write ({x}, {y}, {z})");
                    writes += 1;
                }
                Some("gateway_exit") => {
                    let mut position = words.next().expect("gateway exit").split(',');
                    let x: i32 = position.next().expect("x").parse().expect("integer x");
                    let y: i32 = position.next().expect("y").parse().expect("integer y");
                    let z: i32 = position.next().expect("z").parse().expect("integer z");
                    assert!(position.next().is_none(), "extra coordinate: {line}");
                    assert_eq!(words.next(), Some("exact=true"), "gateway exit setting: {line}");
                    assert!(words.next().is_none(), "trailing gateway exit data: {line}");
                    exit = Some(EndGateway { pos: (50, 70, 50), exit: (x, y, z), exact: true });
                }
                _ => {}
            }
        }
        assert_eq!(writes, 45, "fixture must include the complete gateway box");
        assert_eq!(exit, Some(EndGateway { pos: (50, 70, 50), exit: (100, 50, 0), exact: true }));
    }
}
