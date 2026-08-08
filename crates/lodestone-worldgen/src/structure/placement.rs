//! `StructurePlacement`: which chunks a structure *set* may start a structure in.
//!
//! Ported from the record definitions under
//! `.cache/mc/26.2/src/net/minecraft/world/level/levelgen/structure/placement/`
//! (`StructurePlacement`, `RandomSpreadStructurePlacement`, `RandomSpreadType`,
//! `ConcentricRingsStructurePlacement`) plus
//! `chunk/ChunkGeneratorStructureState.generateRingPositions`.
//!
//! # RNG draw order and count are the specification
//!
//! Every predicate here is a *seeded* decision, and the seed derivation is
//! [`WorldgenRandom::set_large_feature_with_salt`] — see that method for the
//! two different argument orders vanilla itself uses. A placement that draws the
//! same *number* of values in a different *order* is a different world at the
//! same seed, and nothing about the result looks wrong: structures simply appear
//! somewhere else. So:
//!
//! * `getPotentialStructureChunk` draws **exactly two** spread values, X then Z,
//!   from one salt-seeded legacy source. `triangular` spends **two** draws per
//!   axis (four total), `linear` one (two total) — [`RandomSpreadType::evaluate`].
//! * The four `frequency_reduction_method`s are four *different* derivations, not
//!   one derivation with four thresholds, and three of them are legacy shims kept
//!   for pre-1.18 worlds. `legacy_type_1` (pillager outposts) does not use
//!   `set_large_feature_with_salt` at all and burns a `next_int()` before its
//!   real draw; `legacy_type_3` (mineshafts) draws a `nextDouble` — two
//!   `next_bits` calls — where the others draw a `nextFloat`.
//! * A `frequency` of `1.0` skips the reducer entirely
//!   (`applyAdditionalChunkRestrictions`'s `!(frequency < 1.0F)`), so 18 of the
//!   20 bundled sets never draw for frequency at all. Do not "simplify" that to
//!   an unconditional draw-then-compare: it would consume RNG that vanilla does
//!   not, but only for the sets that currently skip it, and the effect is
//!   invisible because each reducer re-seeds from scratch.

use lodestone_worldgen_core::rng::{LegacyRandomSource, RandomSource, WorldgenRandom};
use serde_json::Value;

/// `RandomSpreadType` — how the in-cell offset is drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadType {
    /// `random.nextInt(limit)`.
    Linear,
    /// `(random.nextInt(limit) + random.nextInt(limit)) / 2` — **two** draws.
    Triangular,
}

impl SpreadType {
    fn parse(value: &Value) -> Self {
        match value.as_str() {
            Some("triangular") => Self::Triangular,
            _ => Self::Linear,
        }
    }

    fn evaluate(self, random: &mut WorldgenRandom<LegacyRandomSource>, limit: i32) -> i32 {
        match self {
            Self::Linear => random.next_int_bounded(limit),
            Self::Triangular => {
                (random.next_int_bounded(limit) + random.next_int_bounded(limit)) / 2
            }
        }
    }
}

/// `StructurePlacement.FrequencyReductionMethod` — four distinct seed
/// derivations, one per historical era of vanilla structure placement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FrequencyReduction {
    /// `default` — `setLargeFeatureWithSalt(seed, salt, sourceX, sourceZ)`
    /// (note the argument order) then `nextFloat() < probability`.
    Default,
    /// `legacy_type_1` — the pillager-outpost reducer. Seeds from
    /// `cx ^ cz << 4 ^ seed` where `cx = sourceX >> 4`, **discards one
    /// `nextInt()`**, then tests `nextInt(1/probability) == 0`.
    LegacyType1,
    /// `legacy_type_2` — `setLargeFeatureWithSalt(seed, sourceX, sourceZ, 10387320)`
    /// with vanilla's `HIGHLY_ARBITRARY_RANDOM_SALT`, ignoring the set's own salt.
    LegacyType2,
    /// `legacy_type_3` — `setLargeFeatureSeed` then `nextDouble() < probability`.
    LegacyType3,
}

impl FrequencyReduction {
    fn parse(value: &Value) -> Self {
        match value.as_str() {
            Some("legacy_type_1") => Self::LegacyType1,
            Some("legacy_type_2") => Self::LegacyType2,
            Some("legacy_type_3") => Self::LegacyType3,
            _ => Self::Default,
        }
    }

    /// `HIGHLY_ARBITRARY_RANDOM_SALT` — `StructurePlacement.java:27`.
    const ARBITRARY_SALT: i32 = 10_387_320;

    fn should_generate(
        self,
        seed: i64,
        salt: i32,
        source_x: i32,
        source_z: i32,
        probability: f32,
    ) -> bool {
        let mut random = WorldgenRandom::new(LegacyRandomSource::new(0));
        match self {
            Self::Default => {
                // Vanilla's own argument order: salt into `x`, chunk X into `z`,
                // chunk Z into `blend`. Not a transcription slip — see the
                // module doc.
                random.set_large_feature_with_salt(seed, salt, source_x, source_z);
                random.next_float() < probability
            }
            Self::LegacyType1 => {
                let cx = source_x >> 4;
                let cz = source_z >> 4;
                random.set_seed(i64::from(cx) ^ (i64::from(cz) << 4) ^ seed);
                let _ = random.next_int();
                random.next_int_bounded((1.0 / probability) as i32) == 0
            }
            Self::LegacyType2 => {
                random.set_large_feature_with_salt(
                    seed,
                    source_x,
                    source_z,
                    Self::ARBITRARY_SALT,
                );
                random.next_float() < probability
            }
            Self::LegacyType3 => {
                random.set_large_feature_seed(seed, source_x, source_z);
                random.next_double() < f64::from(probability)
            }
        }
    }
}

/// `StructurePlacement.ExclusionZone` — "not within `chunk_count` chunks of a
/// placement chunk of `other_set`".
#[derive(Debug, Clone)]
pub struct ExclusionZone {
    /// The structure-set id this zone excludes around.
    pub other_set: String,
    /// Chebyshev chunk radius, 1..=16.
    pub chunk_count: i32,
}

/// The parsed placement half of one structure set.
#[derive(Debug, Clone)]
pub struct Placement {
    /// `locate_offset`, the block offset `/locate` reports. Carried because the
    /// data has it (buried treasure's `[9, 0, 9]`); nothing in generation reads it.
    pub locate_offset: [i32; 3],
    /// `frequency_reduction_method`.
    pub frequency_reduction: FrequencyReduction,
    /// `frequency`, `0.0..=1.0`. `1.0` (the default) skips the reducer entirely.
    pub frequency: f32,
    /// `salt`.
    pub salt: i32,
    /// `exclusion_zone`, if any. Only `pillager_outposts` carries one in 26.2.
    pub exclusion_zone: Option<ExclusionZone>,
    /// The placement-type-specific half.
    pub kind: PlacementKind,
}

/// The two placement types vanilla registers.
#[derive(Debug, Clone)]
pub enum PlacementKind {
    /// `minecraft:random_spread` — a jittered grid.
    RandomSpread {
        /// Grid pitch in chunks.
        spacing: i32,
        /// Chunks of the cell reserved as a gutter; the jitter range is
        /// `spacing - separation`.
        separation: i32,
        /// Linear or triangular jitter.
        spread_type: SpreadType,
    },
    /// `minecraft:concentric_rings` — the stronghold rings.
    ConcentricRings {
        /// `distance`, in chunks, of the innermost ring.
        distance: i32,
        /// `spread`: positions in the first ring.
        spread: i32,
        /// `count`: total ring positions.
        count: i32,
        /// `preferred_biomes` — a biome-tag reference or inline list, resolved by
        /// the registry into a name set.
        preferred_biomes: Vec<String>,
    },
    /// A placement `type` this engine does not implement. Placement never fires,
    /// and the registry names it in its unsupported ledger rather than silently
    /// dropping it.
    Unsupported(String),
}

impl Placement {
    /// Parses one structure set's `placement` object.
    #[must_use]
    pub fn parse(value: &Value) -> Self {
        let locate_offset = value["locate_offset"]
            .as_array()
            .and_then(|a| {
                Some([
                    a.first()?.as_i64()? as i32,
                    a.get(1)?.as_i64()? as i32,
                    a.get(2)?.as_i64()? as i32,
                ])
            })
            .unwrap_or([0, 0, 0]);
        let exclusion_zone = value["exclusion_zone"].as_object().and_then(|z| {
            Some(ExclusionZone {
                other_set: z.get("other_set")?.as_str()?.to_string(),
                chunk_count: z.get("chunk_count")?.as_i64()? as i32,
            })
        });
        let kind = match value["type"].as_str().unwrap_or_default() {
            "minecraft:random_spread" | "random_spread" => PlacementKind::RandomSpread {
                spacing: value["spacing"].as_i64().unwrap_or(1) as i32,
                separation: value["separation"].as_i64().unwrap_or(0) as i32,
                spread_type: SpreadType::parse(&value["spread_type"]),
            },
            "minecraft:concentric_rings" | "concentric_rings" => {
                PlacementKind::ConcentricRings {
                    distance: value["distance"].as_i64().unwrap_or(0) as i32,
                    spread: value["spread"].as_i64().unwrap_or(1) as i32,
                    count: value["count"].as_i64().unwrap_or(1) as i32,
                    // Left as the raw reference; `StructureRegistry` resolves it
                    // through `Resolver::biome_tag` because only it has the
                    // resolver.
                    preferred_biomes: match &value["preferred_biomes"] {
                        Value::String(s) => vec![s.clone()],
                        Value::Array(a) => a
                            .iter()
                            .filter_map(|e| e.as_str().map(str::to_owned))
                            .collect(),
                        _ => Vec::new(),
                    },
                }
            }
            other => PlacementKind::Unsupported(other.to_string()),
        };
        Self {
            locate_offset,
            frequency_reduction: FrequencyReduction::parse(&value["frequency_reduction_method"]),
            frequency: value["frequency"].as_f64().unwrap_or(1.0) as f32,
            salt: value["salt"].as_i64().unwrap_or(0) as i32,
            exclusion_zone,
            kind,
        }
    }

    /// `RandomSpreadStructurePlacement.getPotentialStructureChunk` — the one
    /// chunk of the grid cell containing `(source_x, source_z)` that this set may
    /// start in. `None` for a non-random-spread placement.
    #[must_use]
    pub fn potential_structure_chunk(
        &self,
        seed: i64,
        source_x: i32,
        source_z: i32,
    ) -> Option<(i32, i32)> {
        let PlacementKind::RandomSpread {
            spacing,
            separation,
            spread_type,
        } = self.kind
        else {
            return None;
        };
        let grid_x = source_x.div_euclid(spacing);
        let grid_z = source_z.div_euclid(spacing);
        let mut random = WorldgenRandom::new(LegacyRandomSource::new(0));
        random.set_large_feature_with_salt(seed, grid_x, grid_z, self.salt);
        let limit = spacing - separation;
        let spread_x = spread_type.evaluate(&mut random, limit);
        let spread_z = spread_type.evaluate(&mut random, limit);
        Some((grid_x * spacing + spread_x, grid_z * spacing + spread_z))
    }

    /// `applyAdditionalChunkRestrictions` — the `frequency` gate, skipped
    /// entirely (no draw) when `frequency >= 1.0`.
    #[must_use]
    pub fn passes_frequency(&self, seed: i64, source_x: i32, source_z: i32) -> bool {
        if self.frequency >= 1.0 {
            return true;
        }
        self.frequency_reduction
            .should_generate(seed, self.salt, source_x, source_z, self.frequency)
    }

    /// `isPlacementChunk` for the grid placements this engine implements, with
    /// **no** frequency or exclusion-zone gate — those are
    /// [`Self::passes_frequency`] and the registry's exclusion walk.
    ///
    /// `concentric_rings` needs the pre-computed ring list and so is answered by
    /// the registry, not here.
    #[must_use]
    pub fn is_placement_chunk(&self, seed: i64, source_x: i32, source_z: i32) -> bool {
        match self.potential_structure_chunk(seed, source_x, source_z) {
            Some((x, z)) => x == source_x && z == source_z,
            None => false,
        }
    }
}

/// `ChunkGeneratorStructureState.generateRingPositions` — the stronghold ring
/// positions, in vanilla's own draw order.
///
/// **`biome_pick` is called once per position, in position order**, and is handed
/// the *forked* generator vanilla forks per position plus the ring-centre chunk
/// coordinates; returning `None` keeps vanilla's fallback (`new ChunkPos(initialX,
/// initialZ)` — note that is a *chunk* position built from what were section
/// coordinates, which is vanilla's own behaviour and not a unit slip).
///
/// The draw order, which is the specification: one `nextDouble` for the initial
/// angle, then per position one `nextDouble` for the distance jitter and one
/// `fork()`, and one extra `nextDouble` each time a ring completes. The source is
/// **xoroshiro** (`RandomSource.create()`), not the legacy LCG every other
/// placement decision uses.
///
/// # Not verified against vanilla output
///
/// The oracle world at `.cache/mc/survival` contains no stronghold (the nearest
/// ring sits ~1,280 blocks outside its generated area), so this function is
/// gated by its record definition only. Do not report stronghold placement as
/// verified until the oracle world is extended.
pub fn ring_positions<F>(
    concentric_rings_seed: i64,
    distance: i32,
    spread: i32,
    count: i32,
    mut biome_pick: F,
) -> Vec<(i32, i32)>
where
    F: FnMut(&mut lodestone_worldgen_core::rng::XoroshiroRandomSource, i32, i32) -> Option<(i32, i32)>,
{
    use lodestone_worldgen_core::rng::XoroshiroRandomSource;

    if count == 0 {
        return Vec::new();
    }
    let mut random = XoroshiroRandomSource::new(0);
    random.set_seed(concentric_rings_seed);
    let mut angle = random.next_double() * std::f64::consts::PI * 2.0;
    let mut position_in_circle = 0;
    let mut circle = 0i32;
    let mut spread = spread;
    let mut out = Vec::with_capacity(count as usize);

    for i in 0..count {
        let dist = 4.0 * f64::from(distance)
            + f64::from(distance) * f64::from(circle) * 6.0
            + (random.next_double() - 0.5) * f64::from(distance) * 2.5;
        let initial_x = (angle.cos() * dist).round() as i32;
        let initial_z = (angle.sin() * dist).round() as i32;
        // `RandomSource.fork()` for xoroshiro is
        // `new XoroshiroRandomSource(nextLong(), nextLong())` — the raw
        // `(lo, hi)` constructor with no seed upgrade, hence `from_128bit`
        // rather than `new`. Spelled out here rather than added to the
        // `RandomSource` trait, which no other caller needs.
        let mut forked = XoroshiroRandomSource::from_128bit(
            random.next_long(),
            random.next_long(),
        );
        out.push(
            biome_pick(&mut forked, initial_x, initial_z)
                .unwrap_or((initial_x, initial_z)),
        );
        angle += std::f64::consts::PI * 2.0 / f64::from(spread);
        position_in_circle += 1;
        if position_in_circle == spread {
            circle += 1;
            position_in_circle = 0;
            spread += 2 * spread / (circle + 1);
            spread = spread.min(count - i);
            angle += random.next_double() * std::f64::consts::PI * 2.0;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The bundled `shipwrecks` placement, parsed. Fixture values come from the
    /// asset, not from this parser.
    #[test]
    fn parses_the_bundled_shipwreck_placement() {
        let p = Placement::parse(&serde_json::json!({
            "type": "minecraft:random_spread",
            "salt": 165_745_295,
            "separation": 4,
            "spacing": 24
        }));
        assert_eq!(p.salt, 165_745_295);
        assert_eq!(p.frequency, 1.0);
        assert_eq!(p.frequency_reduction, FrequencyReduction::Default);
        assert!(p.exclusion_zone.is_none());
        match p.kind {
            PlacementKind::RandomSpread {
                spacing,
                separation,
                spread_type,
            } => {
                assert_eq!((spacing, separation), (24, 4));
                assert_eq!(spread_type, SpreadType::Linear);
            }
            other => panic!("wrong kind: {other:?}"),
        }
    }

    /// `getPotentialStructureChunk` lands inside its own cell, is constant across
    /// the whole cell, and is reached from a negative source chunk by
    /// **floor** division — `-1 / 24 == 0` in Rust, which would put chunk -1 in
    /// cell 0 and place two structures in adjacent cells near the origin.
    #[test]
    fn potential_chunk_is_cell_constant_and_floor_divided() {
        let p = Placement::parse(&serde_json::json!({
            "type": "minecraft:random_spread", "salt": 165_745_295,
            "separation": 4, "spacing": 24
        }));
        let seed = -195_764_831i64;
        let a = p.potential_structure_chunk(seed, 5, 7).unwrap();
        for (x, z) in [(0, 0), (23, 23), (12, 3)] {
            assert_eq!(p.potential_structure_chunk(seed, x, z).unwrap(), a);
        }
        assert!((0..24).contains(&a.0) && (0..24).contains(&a.1), "{a:?}");
        // Cell -1 is a *different* cell from cell 0.
        let b = p.potential_structure_chunk(seed, -1, -1).unwrap();
        assert!((-24..0).contains(&b.0) && (-24..0).contains(&b.1), "{b:?}");
    }

    /// A `frequency` of 1.0 draws nothing; a low frequency rejects most chunks.
    /// The magnitude matters: `legacy_type_3` at 0.004 (mineshafts) must accept
    /// roughly 0.4% of chunks, not ~50% (which is what comparing a `nextFloat`
    /// against the wrong side of the inequality gives) and not 0%.
    #[test]
    fn mineshaft_frequency_reduction_has_the_right_magnitude() {
        let p = Placement::parse(&serde_json::json!({
            "type": "minecraft:random_spread", "frequency": 0.004,
            "frequency_reduction_method": "legacy_type_3", "salt": 0,
            "separation": 0, "spacing": 1
        }));
        let seed = -195_764_831i64;
        let hits = (0..200).flat_map(|x| (0..200).map(move |z| (x, z)))
            .filter(|&(x, z)| p.passes_frequency(seed, x, z))
            .count();
        // 40,000 chunks at p = 0.004 -> expect ~160.
        assert!((60..400).contains(&hits), "{hits} of 40000");
    }
}
