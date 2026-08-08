//! The End: `TheEndBiomeSource`, plus a precise statement of the one thing still
//! between this engine and End terrain.
//!
//! # What it is
//!
//! [`EndBiomeSource`] is the complete port of vanilla's `TheEndBiomeSource`
//! (`TheEndBiomeSource.java:60-81`) — the End's whole biome layout, and it works
//! **today**, without the density interpreter, because the only thing it samples is
//! the router's `erosion` channel and for the End that channel is exactly
//! `cache_2d(end_islands)` (`NoiseRouterData.java:433,443`; confirmed against the
//! bundled `noise_settings/end.json`, whose `erosion` is literally
//! `{"type": "minecraft:cache_2d", "argument": {"type": "minecraft:end_islands"}}`).
//! So it is built straight on [`crate::noise::EndIslandNoise`].
//!
//! # What is missing, exactly
//!
//! **End *terrain* needs one thing: a `minecraft:end_islands` leaf in the density
//! interpreter.** `noise_settings/end.json`'s `final_density` reaches
//! `density_function/end/sloped_cheese.json`, which is
//! `add(end_islands, end/base_3d_noise)`, so `density::Builder::build` panics on the
//! unknown type and there is no `EndGenerator` here. Everything *else* the End needs
//! already landed with the Nether:
//!
//! | need | state |
//! |---|---|
//! | `legacy_random_source` | [`crate::rng::Algorithm`], landed |
//! | `aquifers_enabled: false` | [`crate::aquifer::AquiferSystem::disabled`], landed |
//! | `default_fluid: air` | `aquifer::fluid_from_settings`, landed |
//! | 8-wide/4-tall cells (`size_horizontal 2, size_vertical 1`) | `aquifer::cell_geometry`, landed |
//! | surface rule | a single `minecraft:block` end_stone rule — the engine's simplest case |
//! | `TheEndBiomeSource` | **here** |
//! | `end_islands` as a `Density` leaf | **not landed** — the density interpreter is another cluster's file |
//! | `end_islands` the algorithm | [`crate::noise::EndIslandNoise`], landed and gated |
//!
//! The leaf patch is three lines of `Density`/`OpKind` plumbing plus a `Builder`
//! arm; see `docs/worldgen-end.md` for the exact shape.
//!
//! # How it works
//!
//! ```text
//! chunkX² + chunkZ² <= 4096  ->  the_end                       (radius 64, the main island)
//! erosion >  0.25            ->  end_highlands
//! erosion >= -0.0625         ->  end_midlands
//! erosion <  -0.21875        ->  small_end_islands
//! otherwise                  ->  end_barrens
//! ```
//!
//! Two details that are easy to get wrong and are load-bearing:
//!
//! * **The sample position is not the quart's own block position.** Vanilla builds
//!   `weirdBlockX = (chunkX * 2 + 1) * 8`, i.e. `chunkX * 16 + 8` — the *chunk
//!   centre*, so all 16 quarts of a chunk share one erosion sample. The variable is
//!   called `weird` in the decompiled source for a reason; sampling at the quart
//!   would give a finer-grained and wrong biome map.
//! * **The 4096 gate is `i64`** and matches `end_islands`' own centre hole exactly,
//!   which is why `the_end` covers precisely the region that can never carry an
//!   island.
//!
//! `blockY` is passed to the erosion sample and never read (the channel is
//! `cache_2d`, i.e. xz-only), so End biomes are y-invariant just as the Nether's
//! are.
//!
//! # How to change it
//!
//! The five biome ids are **not** data: `TheEndBiomeSource` serialises to an empty
//! object and its five holders come from the registry (`TheEndBiomeSource.java:14-23`),
//! so they are constants here too rather than a resolver lookup.
//!
//! # Dependencies
//!
//! [`crate::noise::EndIslandNoise`] only. Nothing version-specific, no resolver.

use crate::noise::EndIslandNoise;

/// `Biomes.THE_END`.
pub const THE_END: &str = "minecraft:the_end";
/// `Biomes.END_HIGHLANDS`.
pub const END_HIGHLANDS: &str = "minecraft:end_highlands";
/// `Biomes.END_MIDLANDS`.
pub const END_MIDLANDS: &str = "minecraft:end_midlands";
/// `Biomes.SMALL_END_ISLANDS`.
pub const SMALL_END_ISLANDS: &str = "minecraft:small_end_islands";
/// `Biomes.END_BARRENS`.
pub const END_BARRENS: &str = "minecraft:end_barrens";

/// The main island's chunk radius, squared — `chunkX² + chunkZ² <= 4096` is
/// radius 64 chunks, and it is the same constant `end_islands`' own centre hole
/// uses.
const MAIN_ISLAND_CHUNKS_SQUARED: i64 = 4096;

/// `TheEndBiomeSource`.
#[derive(Debug, Clone)]
pub struct EndBiomeSource {
    islands: EndIslandNoise,
}

impl EndBiomeSource {
    /// Builds the source for `seed`. Constructs its own [`EndIslandNoise`] because
    /// vanilla's `erosion` channel is `cache2d(endIslands(seed))` and nothing else
    /// feeds it.
    #[must_use]
    pub fn new(seed: i64) -> Self {
        Self {
            islands: EndIslandNoise::new(seed),
        }
    }

    /// The five biomes this source can return, in `collectPossibleBiomes` order.
    #[must_use]
    pub fn possible_biomes() -> [&'static str; 5] {
        [
            THE_END,
            END_HIGHLANDS,
            END_MIDLANDS,
            SMALL_END_ISLANDS,
            END_BARRENS,
        ]
    }

    /// `getNoiseBiome(quartX, quartY, quartZ, sampler)`.
    ///
    /// `quart_y` is accepted and unused, exactly as in vanilla: it reaches the
    /// erosion sample's context and the `cache_2d(end_islands)` channel never reads
    /// a `y`. Keeping the parameter means a caller writes the same call it would
    /// write for a multi-noise dimension.
    #[must_use]
    pub fn biome_at_quart(&self, quart_x: i32, _quart_y: i32, quart_z: i32) -> &'static str {
        let block_x = quart_x * 4;
        let block_z = quart_z * 4;
        let chunk_x = block_x >> 4;
        let chunk_z = block_z >> 4;
        if i64::from(chunk_x) * i64::from(chunk_x) + i64::from(chunk_z) * i64::from(chunk_z)
            <= MAIN_ISLAND_CHUNKS_SQUARED
        {
            return THE_END;
        }
        // `weirdBlockX` — the chunk *centre*, not the quart's own position, so all
        // 16 quarts of a chunk share one sample.
        let weird_block_x = (chunk_x * 2 + 1) * 8;
        let weird_block_z = (chunk_z * 2 + 1) * 8;
        let height = self.islands.compute(weird_block_x, weird_block_z);
        if height > 0.25 {
            END_HIGHLANDS
        } else if height >= -0.0625 {
            END_MIDLANDS
        } else if height < -0.21875 {
            SMALL_END_ISLANDS
        } else {
            END_BARRENS
        }
    }

    /// The 16 horizontal quart biomes of chunk `(cx, cz)`.
    ///
    /// All 16 are equal for any chunk, because the erosion sample is taken at the
    /// chunk centre — that is vanilla's behaviour, not a simplification, and the
    /// array shape is kept so a caller building a biome container writes the same
    /// loop it would for the Nether.
    #[must_use]
    pub fn chunk_quarts(&self, cx: i32, cz: i32) -> [&'static str; 16] {
        std::array::from_fn(|i| {
            self.biome_at_quart(cx * 4 + (i % 4) as i32, 0, cz * 4 + (i / 4) as i32)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The main island covers exactly the chunks the density function's own centre
    /// hole covers, and nothing outside it. The expectation is the geometric
    /// predicate itself, evaluated independently of the branch under test.
    #[test]
    fn the_main_island_is_exactly_chunk_radius_64() {
        let source = EndBiomeSource::new(-195_764_831);
        let mut inside = 0usize;
        let mut outside = 0usize;
        for cx in -70..=70 {
            for cz in -70..=70 {
                let want_end = i64::from(cx) * i64::from(cx) + i64::from(cz) * i64::from(cz) <= 4096;
                let got = source.biome_at_quart(cx * 4, 0, cz * 4);
                if want_end {
                    assert_eq!(got, THE_END, "chunk ({cx},{cz}) is inside radius 64");
                    inside += 1;
                } else {
                    assert_ne!(got, THE_END, "chunk ({cx},{cz}) is outside radius 64");
                    outside += 1;
                }
            }
        }
        // Both arms must be exercised, or the equality above proves nothing.
        assert!(inside > 1_000 && outside > 1_000, "{inside} / {outside}");
    }

    /// All 16 quarts of a chunk agree, because the sample is at the chunk centre.
    /// A port that used the quart's own block position would fail this.
    #[test]
    fn every_quart_of_a_chunk_shares_the_chunk_centre_sample() {
        let source = EndBiomeSource::new(-195_764_831);
        for (cx, cz) in [(100, 100), (-137, 244), (65, 0), (-2000, 1500)] {
            let quarts = source.chunk_quarts(cx, cz);
            assert!(
                quarts.iter().all(|b| *b == quarts[0]),
                "chunk ({cx},{cz}) is not uniform: {quarts:?}"
            );
        }
    }

    /// Outside the main island all four outer biomes must actually be reachable.
    /// Without this the threshold ladder could be collapsed to one arm and every
    /// other test here would still pass — the island species of vacuous test,
    /// applied to a `match`.
    #[test]
    fn all_five_biomes_are_reachable() {
        let source = EndBiomeSource::new(-195_764_831);
        let mut seen: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        seen.insert(source.biome_at_quart(0, 0, 0));
        for cx in (-400..400).step_by(11) {
            for cz in (-400..400).step_by(13) {
                seen.insert(source.biome_at_quart(cx * 4, 0, cz * 4));
            }
        }
        let mut expected: std::collections::BTreeSet<&str> = std::collections::BTreeSet::new();
        expected.extend(EndBiomeSource::possible_biomes());
        assert_eq!(seen, expected, "not every End biome is reachable");
    }

    /// The thresholds are read off the erosion value, so the mapping must agree with
    /// the ladder re-derived from the density function directly — which is the
    /// independent construction, not a restatement: it reads
    /// [`EndIslandNoise::compute`] at the same position and applies the four
    /// constants transcribed from `TheEndBiomeSource.java` by hand.
    #[test]
    fn the_threshold_ladder_matches_the_erosion_value() {
        let source = EndBiomeSource::new(42);
        let islands = EndIslandNoise::new(42);
        let mut counts = std::collections::BTreeMap::new();
        for cx in (65..400).step_by(3) {
            for cz in (-400..400).step_by(7) {
                let h = islands.compute((cx * 2 + 1) * 8, (cz * 2 + 1) * 8);
                let want = if h > 0.25 {
                    END_HIGHLANDS
                } else if h >= -0.0625 {
                    END_MIDLANDS
                } else if h < -0.21875 {
                    SMALL_END_ISLANDS
                } else {
                    END_BARRENS
                };
                assert_eq!(source.biome_at_quart(cx * 4, 0, cz * 4), want, "({cx},{cz})");
                *counts.entry(want).or_insert(0usize) += 1;
            }
        }
        assert_eq!(counts.len(), 4, "only {counts:?} of the four outer arms fired");
    }

    /// End biomes do not depend on `y`, for the same structural reason the Nether's
    /// do not: the channel is `cache_2d`.
    #[test]
    fn end_biomes_do_not_vary_with_y() {
        let source = EndBiomeSource::new(-195_764_831);
        for (qx, qz) in [(0, 0), (400, -900), (-3000, 3000)] {
            let at_zero = source.biome_at_quart(qx, 0, qz);
            for qy in [-16, 1, 8, 31] {
                assert_eq!(source.biome_at_quart(qx, qy, qz), at_zero);
            }
        }
    }
}
