//! Geode configured-feature placement.
//!
//! A geode is a bounded, noise-shaped shell around a small set of sampled
//! points.  The shell writes four configured layers, optionally cuts a crack,
//! and finally grows one of the configured crystal states from air or water
//! beside budding amethyst.  This module keeps that body separate from the
//! ordinary vegetation placers because its world-seed noise and its
//! replace/invalid-block rules are unlike the surface feature families.

use std::collections::HashSet;

use serde_json::Value;

use crate::density::Resolver;
use crate::feature::{BlockPos, IntProvider};
use crate::interner::StateId;
use crate::noise::NormalNoise;
use crate::rng::{LegacyRandomSource, RandomSource, WorldgenRandom};

use super::base_id;
use super::config::{
    BlockStateProvider, VegTags, is_air, resolve_block_set, try_parse_int_provider,
};
use super::grid::VegGrid;

/// Parsed configuration for the geode body used by 26.2's cave decoration.
///
/// The five providers are retained as providers instead of eagerly reducing
/// them to state ids: provider evaluation is part of the feature's random
/// stream, even when the current bundled geode uses only simple providers.
#[derive(Clone, Debug)]
pub struct GeodeCfg {
    pub(super) filling_provider: BlockStateProvider,
    pub(super) inner_layer_provider: BlockStateProvider,
    pub(super) alternate_inner_layer_provider: BlockStateProvider,
    pub(super) middle_layer_provider: BlockStateProvider,
    pub(super) outer_layer_provider: BlockStateProvider,
    pub(super) inner_placements: Vec<String>,
    pub(super) cannot_replace: HashSet<String>,
    pub(super) invalid_blocks: HashSet<String>,
    pub(super) filling: f64,
    pub(super) inner_layer: f64,
    pub(super) middle_layer: f64,
    pub(super) outer_layer: f64,
    pub(super) generate_crack_chance: f64,
    pub(super) base_crack_size: f64,
    pub(super) crack_point_offset: i32,
    pub(super) use_potential_placements_chance: f64,
    pub(super) use_alternate_layer0_chance: f64,
    pub(super) placements_require_layer0_alternate: bool,
    pub(super) outer_wall_distance: IntProvider,
    pub(super) outer_wall_distance_max: i32,
    pub(super) distribution_points: IntProvider,
    pub(super) point_offset: IntProvider,
    pub(super) min_gen_offset: i32,
    pub(super) max_gen_offset: i32,
    pub(super) noise_multiplier: f64,
    pub(super) invalid_blocks_threshold: i32,
}

impl GeodeCfg {
    /// Parses the configured feature with the codec defaults used by the
    /// bundled data. A malformed provider or holder set leaves the enclosing
    /// configured feature unsupported instead of guessing a shape.
    pub(super) fn try_parse(resolver: &dyn Resolver, c: &Value) -> Option<Self> {
        let blocks = c.get("blocks")?;
        let filling_provider = BlockStateProvider::try_parse(&blocks["filling_provider"])?;
        let inner_layer_provider = BlockStateProvider::try_parse(&blocks["inner_layer_provider"])?;
        let alternate_inner_layer_provider =
            BlockStateProvider::try_parse(&blocks["alternate_inner_layer_provider"])?;
        let middle_layer_provider = BlockStateProvider::try_parse(&blocks["middle_layer_provider"])?;
        let outer_layer_provider = BlockStateProvider::try_parse(&blocks["outer_layer_provider"])?;
        let inner_placements = blocks["inner_placements"]
            .as_array()?
            .iter()
            .map(|state| state["Name"].as_str().map(|_| super::config::canon_state(state)))
            .collect::<Option<Vec<_>>>()?;
        if inner_placements.is_empty() {
            return None;
        }
        let cannot_replace = resolve_block_set(resolver, &blocks["cannot_replace"])?;
        let invalid_blocks = resolve_block_set(resolver, &blocks["invalid_blocks"])?;

        let layers = c.get("layers")?;
        let filling = layers.get("filling").and_then(Value::as_f64).unwrap_or(1.7);
        let inner_layer = layers
            .get("inner_layer")
            .and_then(Value::as_f64)
            .unwrap_or(2.2);
        let middle_layer = layers
            .get("middle_layer")
            .and_then(Value::as_f64)
            .unwrap_or(3.2);
        let outer_layer = layers
            .get("outer_layer")
            .and_then(Value::as_f64)
            .unwrap_or(4.2);
        if [filling, inner_layer, middle_layer, outer_layer]
            .iter()
            .any(|value| !value.is_finite() || *value <= 0.0)
        {
            return None;
        }

        let crack = c.get("crack")?;
        let generate_crack_chance = crack
            .get("generate_crack_chance")
            .and_then(Value::as_f64)
            .unwrap_or(1.0);
        let base_crack_size = crack
            .get("base_crack_size")
            .and_then(Value::as_f64)
            .unwrap_or(2.0);
        let crack_point_offset = crack
            .get("crack_point_offset")
            .and_then(Value::as_i64)
            .unwrap_or(2) as i32;
        if !(0.0..=1.0).contains(&generate_crack_chance)
            || !base_crack_size.is_finite()
            || base_crack_size < 0.0
            || !(0..=10).contains(&crack_point_offset)
        {
            return None;
        }

        let outer_wall_distance = parse_int_or_default(
            c.get("outer_wall_distance"),
            IntProvider::Uniform { min: 4, max: 5 },
        )?;
        let distribution_points = parse_int_or_default(
            c.get("distribution_points"),
            IntProvider::Uniform { min: 3, max: 4 },
        )?;
        let point_offset = parse_int_or_default(
            c.get("point_offset"),
            IntProvider::Uniform { min: 1, max: 2 },
        )?;
        let outer_wall_distance_max = max_inclusive(&outer_wall_distance);
        if outer_wall_distance_max <= 0
            || !provider_within_bounds(&outer_wall_distance, 1, 20)
            || !provider_within_bounds(&distribution_points, 1, 20)
            || !provider_within_bounds(&point_offset, 0, 10)
        {
            return None;
        }

        let use_potential_placements_chance = c
            .get("use_potential_placements_chance")
            .and_then(Value::as_f64)
            .unwrap_or(0.35);
        let use_alternate_layer0_chance = c
            .get("use_alternate_layer0_chance")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if !(0.0..=1.0).contains(&use_potential_placements_chance)
            || !(0.0..=1.0).contains(&use_alternate_layer0_chance)
        {
            return None;
        }
        let min_gen_offset = c
            .get("min_gen_offset")
            .and_then(Value::as_i64)
            .unwrap_or(-16) as i32;
        let max_gen_offset = c
            .get("max_gen_offset")
            .and_then(Value::as_i64)
            .unwrap_or(16) as i32;
        let noise_multiplier = c
            .get("noise_multiplier")
            .and_then(Value::as_f64)
            .unwrap_or(0.05);
        if min_gen_offset > max_gen_offset
            || !(0.0..=1.0).contains(&noise_multiplier)
            || !noise_multiplier.is_finite()
        {
            return None;
        }

        Some(Self {
            filling_provider,
            inner_layer_provider,
            alternate_inner_layer_provider,
            middle_layer_provider,
            outer_layer_provider,
            inner_placements,
            cannot_replace,
            invalid_blocks,
            filling,
            inner_layer,
            middle_layer,
            outer_layer,
            generate_crack_chance,
            base_crack_size,
            crack_point_offset,
            use_potential_placements_chance,
            use_alternate_layer0_chance,
            placements_require_layer0_alternate: c
                .get("placements_require_layer0_alternate")
                .and_then(Value::as_bool)
                .unwrap_or(true),
            outer_wall_distance,
            outer_wall_distance_max,
            distribution_points,
            point_offset,
            min_gen_offset,
            max_gen_offset,
            noise_multiplier,
            invalid_blocks_threshold: c.get("invalid_blocks_threshold")?.as_i64()? as i32,
        })
    }
}

fn parse_int_or_default(value: Option<&Value>, default: IntProvider) -> Option<IntProvider> {
    value.map_or(Some(default), try_parse_int_provider)
}

fn max_inclusive(provider: &IntProvider) -> i32 {
    match provider {
        IntProvider::Constant(value) => *value,
        IntProvider::Uniform { max, .. }
        | IntProvider::BiasedToBottom { max, .. }
        | IntProvider::Trapezoid { max, .. }
        | IntProvider::ClampedNormal { max, .. } => *max,
        IntProvider::WeightedList(entries) => entries.iter().map(|(value, _)| *value).max().unwrap_or(0),
        IntProvider::WeightedProviders(entries) => entries
            .iter()
            .map(|(provider, _)| max_inclusive(provider))
            .max()
            .unwrap_or(0),
    }
}

fn provider_bounds(provider: &IntProvider) -> Option<(i32, i32)> {
    match provider {
        IntProvider::Constant(value) => Some((*value, *value)),
        IntProvider::Uniform { min, max }
        | IntProvider::BiasedToBottom { min, max }
        | IntProvider::Trapezoid { min, max, .. }
        | IntProvider::ClampedNormal { min, max, .. } => Some((*min, *max)),
        IntProvider::WeightedList(entries) => Some((
            entries.iter().map(|(value, _)| *value).min()?,
            entries.iter().map(|(value, _)| *value).max()?,
        )),
        IntProvider::WeightedProviders(entries) => Some((
            entries
                .iter()
                .map(|(provider, _)| provider_bounds(provider).map(|(min, _)| min))
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .min()?,
            entries
                .iter()
                .map(|(provider, _)| provider_bounds(provider).map(|(_, max)| max))
                .collect::<Option<Vec<_>>>()?
                .into_iter()
                .max()?,
        )),
    }
}

fn provider_within_bounds(provider: &IntProvider, min: i32, max: i32) -> bool {
    provider_bounds(provider).is_some_and(|(actual_min, actual_max)| {
        actual_min <= actual_max && actual_min >= min && actual_max <= max
    })
}

/// Places one geode with the caller's world seed. The seed is deliberately a
/// separate argument from the feature RNG: the normal-noise field is rebuilt
/// from the world seed and does not consume the feature's placement stream.
pub(super) fn place_geode<R: RandomSource>(
    random: &mut R,
    world_seed: i64,
    origin: BlockPos,
    cfg: &GeodeCfg,
    grid: &mut VegGrid,
    tags: &VegTags,
) -> bool {
    let num_points = cfg.distribution_points.sample(random);
    let mut noise_random = WorldgenRandom::new(LegacyRandomSource::new(world_seed));
    let noise = NormalNoise::create(&mut noise_random, -4, &[1.0]);
    let crack_size_adjustment = f64::from(num_points) / f64::from(cfg.outer_wall_distance_max);
    let inner_air = 1.0 / cfg.filling.sqrt();
    let innermost_block_layer = 1.0 / (cfg.inner_layer + crack_size_adjustment).sqrt();
    let inner_crust = 1.0 / (cfg.middle_layer + crack_size_adjustment).sqrt();
    let outer_crust = 1.0 / (cfg.outer_layer + crack_size_adjustment).sqrt();
    let crack_size = 1.0
        / (cfg.base_crack_size
            + random.next_double() / 2.0
            + if num_points > 3 {
                crack_size_adjustment
            } else {
                0.0
            })
        .sqrt();
    let should_generate_crack = f64::from(random.next_float()) < cfg.generate_crack_chance;

    let mut points = Vec::new();
    let mut invalid_points = 0;
    for _ in 0..num_points {
        let x = cfg.outer_wall_distance.sample(random);
        let y = cfg.outer_wall_distance.sample(random);
        let z = cfg.outer_wall_distance.sample(random);
        let pos = BlockPos {
            x: origin.x + x,
            y: origin.y + y,
            z: origin.z + z,
        };
        let base = base_id(grid.get(pos.x, pos.y, pos.z));
        if is_air(base) || cfg.invalid_blocks.contains(base) {
            invalid_points += 1;
            if invalid_points > cfg.invalid_blocks_threshold {
                return false;
            }
        }
        points.push((pos, cfg.point_offset.sample(random)));
    }

    let mut crack_points = Vec::new();
    if should_generate_crack {
        let offset_index = random.next_int_bounded(4);
        let crack_offset = num_points * 2 + 1;
        let (x, z) = match offset_index {
            0 => (crack_offset, 0),
            1 => (0, crack_offset),
            2 => (crack_offset, crack_offset),
            _ => (0, 0),
        };
        for y in [7, 5, 1] {
            crack_points.push(BlockPos {
                x: origin.x + x,
                y: origin.y + y,
                z: origin.z + z,
            });
        }
    }

    let mut potential_crystal_placements = Vec::new();
    for z in origin.z + cfg.min_gen_offset..=origin.z + cfg.max_gen_offset {
        for y in origin.y + cfg.min_gen_offset..=origin.y + cfg.max_gen_offset {
            for x in origin.x + cfg.min_gen_offset..=origin.x + cfg.max_gen_offset {
                let noise_offset = noise.get_value(f64::from(x), f64::from(y), f64::from(z))
                    * cfg.noise_multiplier;
                let dist_sum_shell = points
                    .iter()
                    .fold(0.0, |sum, (point, offset)| {
                        sum + (inv_sqrt(
                            distance_squared(BlockPos { x, y, z }, *point) + *offset as f64,
                        ) + noise_offset)
                    });
                if dist_sum_shell < outer_crust {
                    continue;
                }
                let dist_sum_crack = crack_points
                    .iter()
                    .fold(0.0, |sum, point| {
                        sum + (inv_sqrt(
                            distance_squared(BlockPos { x, y, z }, *point)
                                + cfg.crack_point_offset as f64,
                        ) + noise_offset)
                    });
                let pos = BlockPos { x, y, z };
                if should_generate_crack && dist_sum_crack >= crack_size && dist_sum_shell < inner_air {
                    safe_set_state(grid, &cfg.cannot_replace, pos, grid.interner().id_of("minecraft:air"));
                } else if dist_sum_shell >= inner_air {
                    safe_set_provider(
                        random,
                        grid,
                        tags,
                        &cfg.cannot_replace,
                        pos,
                        &cfg.filling_provider,
                    );
                } else if dist_sum_shell >= innermost_block_layer {
                    let use_alternate =
                        f64::from(random.next_float()) < cfg.use_alternate_layer0_chance;
                    let provider = if use_alternate {
                        &cfg.alternate_inner_layer_provider
                    } else {
                        &cfg.inner_layer_provider
                    };
                    safe_set_provider(random, grid, tags, &cfg.cannot_replace, pos, provider);
                    if (!cfg.placements_require_layer0_alternate || use_alternate)
                        && f64::from(random.next_float()) < cfg.use_potential_placements_chance
                    {
                        potential_crystal_placements.push(pos);
                    }
                } else if dist_sum_shell >= inner_crust {
                    safe_set_provider(
                        random,
                        grid,
                        tags,
                        &cfg.cannot_replace,
                        pos,
                        &cfg.middle_layer_provider,
                    );
                } else {
                    safe_set_provider(
                        random,
                        grid,
                        tags,
                        &cfg.cannot_replace,
                        pos,
                        &cfg.outer_layer_provider,
                    );
                }
            }
        }
    }

    for crystal_pos in potential_crystal_placements {
        let index = random.next_int_bounded(cfg.inner_placements.len() as i32) as usize;
        let base_state = &cfg.inner_placements[index];
        for (direction, (dx, dy, dz)) in DIRECTIONS {
            let mut state = replace_state_property(base_state, "facing", direction);
            let place_pos = BlockPos {
                x: crystal_pos.x + dx,
                y: crystal_pos.y + dy,
                z: crystal_pos.z + dz,
            };
            let place_state = grid.get(place_pos.x, place_pos.y, place_pos.z);
            let waterlogged = is_source_water_state(place_state);
            state = replace_state_property(state.as_str(), "waterlogged", if waterlogged { "true" } else { "false" });
            if can_cluster_grow_at_state(place_state) {
                safe_set_state(
                    grid,
                    &cfg.cannot_replace,
                    place_pos,
                    grid.interner().id_of(&state),
                );
                break;
            }
        }
    }

    true
}

const DIRECTIONS: [(&str, (i32, i32, i32)); 6] = [
    ("down", (0, -1, 0)),
    ("up", (0, 1, 0)),
    ("north", (0, 0, -1)),
    ("south", (0, 0, 1)),
    ("west", (-1, 0, 0)),
    ("east", (1, 0, 0)),
];

fn distance_squared(a: BlockPos, b: BlockPos) -> f64 {
    let dx = f64::from(a.x - b.x);
    let dy = f64::from(a.y - b.y);
    let dz = f64::from(a.z - b.z);
    dx * dx + dy * dy + dz * dz
}

fn inv_sqrt(value: f64) -> f64 {
    1.0 / value.sqrt()
}

fn safe_set_provider<R: RandomSource>(
    random: &mut R,
    grid: &mut VegGrid,
    tags: &VegTags,
    cannot_replace: &HashSet<String>,
    pos: BlockPos,
    provider: &BlockStateProvider,
) -> bool {
    // Evaluate the provider before the replaceability check. This preserves
    // provider RNG draws even when a protected block rejects the write.
    let Some(state) = provider.get_state_id(grid, tags, random, pos) else {
        return false;
    };
    safe_set_state(grid, cannot_replace, pos, state)
}

fn safe_set_state(
    grid: &mut VegGrid,
    cannot_replace: &HashSet<String>,
    pos: BlockPos,
    state: StateId,
) -> bool {
    let current = grid.interner().name_of(grid.get_id(pos.x, pos.y, pos.z));
    if cannot_replace.contains(base_id(current)) {
        return false;
    }
    grid.set_id_if_in_bounds(pos.x, pos.y, pos.z, state)
}

fn can_cluster_grow_at_state(state: &str) -> bool {
    is_air(base_id(state)) || is_source_water_state(state)
}

fn is_source_water_state(state: &str) -> bool {
    if base_id(state) != "minecraft:water" {
        return false;
    }
    let Some(level_start) = state.find("level=") else {
        return true;
    };
    let value_start = level_start + "level=".len();
    let value_end = state[value_start..]
        .find([',', ']'])
        .map_or(state.len(), |offset| value_start + offset);
    &state[value_start..value_end] == "0"
}

fn replace_state_property(state: &str, property: &str, value: &str) -> String {
    let needle = format!("{property}=");
    let Some(start) = state.find(&needle) else {
        return state.to_string();
    };
    let value_start = start + needle.len();
    let value_end = state[value_start..]
        .find([',', ']'])
        .map_or(state.len(), |offset| value_start + offset);
    let mut out = state.to_string();
    out.replace_range(value_start..value_end, value);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rng::LegacyRandomSource;

    fn cfg() -> GeodeCfg {
        GeodeCfg {
            filling_provider: BlockStateProvider::Simple("minecraft:air".into()),
            inner_layer_provider: BlockStateProvider::Simple("minecraft:amethyst_block".into()),
            alternate_inner_layer_provider:
                BlockStateProvider::Simple("minecraft:budding_amethyst".into()),
            middle_layer_provider: BlockStateProvider::Simple("minecraft:calcite".into()),
            outer_layer_provider: BlockStateProvider::Simple("minecraft:smooth_basalt".into()),
            inner_placements: vec![
                "minecraft:amethyst_cluster[facing=up,waterlogged=false]".into(),
            ],
            cannot_replace: HashSet::new(),
            invalid_blocks: HashSet::new(),
            filling: 1.7,
            inner_layer: 2.2,
            middle_layer: 3.2,
            outer_layer: 4.2,
            generate_crack_chance: 0.0,
            base_crack_size: 2.0,
            crack_point_offset: 2,
            use_potential_placements_chance: 1.0,
            use_alternate_layer0_chance: 1.0,
            placements_require_layer0_alternate: true,
            outer_wall_distance: IntProvider::Constant(4),
            outer_wall_distance_max: 4,
            distribution_points: IntProvider::Constant(3),
            point_offset: IntProvider::Constant(1),
            min_gen_offset: -8,
            max_gen_offset: 8,
            noise_multiplier: 0.0,
            invalid_blocks_threshold: 1,
        }
    }

    #[test]
    fn shell_writes_material_layers_and_preserves_protected_blocks() {
        let mut grid = VegGrid::new(0, 32, 0, 0);
        for x in 0..16 {
            for y in 0..32 {
                for z in 0..16 {
                    grid.seed(x, y, z, "minecraft:air".into());
                }
            }
        }
        let mut cfg = cfg();
        cfg.invalid_blocks_threshold = 10;
        cfg.cannot_replace.insert("minecraft:stone".into());
        grid.seed(0, 12, 0, "minecraft:stone".into());
        let tags = VegTags::default();
        let mut random = LegacyRandomSource::new(7);
        assert!(place_geode(
            &mut random,
            42,
            BlockPos { x: 0, y: 12, z: 0 },
            &cfg,
            &mut grid,
            &tags,
        ));
        assert_eq!(base_id(grid.get(0, 12, 0)), "minecraft:stone");
        assert!(grid.dirty_cells().next().is_some());
    }

    #[test]
    fn crystal_state_reorients_and_tracks_waterlogged_property() {
        assert_eq!(
            replace_state_property(
                "minecraft:amethyst_cluster[facing=up,waterlogged=false]",
                "facing",
                "north",
            ),
            "minecraft:amethyst_cluster[facing=north,waterlogged=false]"
        );
        assert_eq!(
            replace_state_property(
                "minecraft:amethyst_cluster[facing=up,waterlogged=false]",
                "waterlogged",
                "true",
            ),
            "minecraft:amethyst_cluster[facing=up,waterlogged=true]"
        );
        assert!(can_cluster_grow_at_state("minecraft:air"));
        assert!(can_cluster_grow_at_state("minecraft:water[level=0]"));
        assert!(!can_cluster_grow_at_state("minecraft:water[level=1]"));
        assert!(!can_cluster_grow_at_state("minecraft:lava"));
    }

    #[test]
    fn neighbor_source_spill_reaches_both_geode_footprint_edges() {
        let local_lo = crate::feature::REGION_MIN - super::super::GEODE_PADDING;
        let local_hi = crate::feature::REGION_MAX + super::super::GEODE_PADDING;
        assert_eq!((local_lo, local_hi), (-32, 48));

        let mut grid = VegGrid::with_footprint(0, 64, 0, 0, local_lo, local_hi);
        grid.seed(0, 48, 0, "minecraft:stone".into());
        grid.seed(47, 48, 47, "minecraft:stone".into());

        let mut cfg = cfg();
        cfg.distribution_points = IntProvider::Constant(20);
        cfg.outer_wall_distance = IntProvider::Constant(16);
        cfg.outer_wall_distance_max = 16;
        cfg.point_offset = IntProvider::Constant(0);
        cfg.min_gen_offset = -16;
        cfg.max_gen_offset = 16;
        cfg.outer_layer = 50.0;
        cfg.generate_crack_chance = 0.0;
        cfg.use_potential_placements_chance = 0.0;
        cfg.invalid_blocks_threshold = 1;
        cfg.filling_provider = BlockStateProvider::Simple("minecraft:amethyst_block".into());
        cfg.inner_layer_provider = BlockStateProvider::Simple("minecraft:amethyst_block".into());
        cfg.alternate_inner_layer_provider =
            BlockStateProvider::Simple("minecraft:budding_amethyst".into());

        super::super::census::reset();
        let mut random = LegacyRandomSource::new(7);
        assert!(place_geode(
            &mut random,
            42,
            BlockPos { x: -16, y: 32, z: -16 },
            &cfg,
            &mut grid,
            &VegTags::default(),
        ));
        assert!(place_geode(
            &mut random,
            42,
            BlockPos { x: 31, y: 32, z: 31 },
            &cfg,
            &mut grid,
            &VegTags::default(),
        ));

        assert_eq!(base_id(grid.get(-32, 32, -32)), "minecraft:smooth_basalt");
        assert_eq!(base_id(grid.get(47, 32, 47)), "minecraft:amethyst_block");
        let census = super::super::census::snapshot();
        assert_eq!(census.writes_rejected, 0, "geode edge spills must fit the widened footprint");
        assert!(census.writes > 0);
    }
}
