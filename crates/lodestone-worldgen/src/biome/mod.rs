//! Multi-noise overworld biome assignment (issue #405).
//!
//! Vanilla assigns each column's biome by evaluating six climate density
//! functions (temperature, humidity, continentalness, erosion, depth,
//! weirdness — already computed by the same, already-JVM-proven [`Density`]
//! interpreter [`crate::overworld`] uses for shape) and finding the nearest
//! point, by squared distance, in a ~7.6k-row `(ParameterPoint, Biome)` table
//! (`net/minecraft/world/level/biome/Climate.java`,
//! `MultiNoiseBiomeSourceParameterList`). This module is that search plus the
//! quantization glue; the table itself is **data**, not code (see below).
//!
//! # The table is bigger than expected — measured, not assumed
//!
//! The scoping plan estimated "~700 rows" from `OverworldBiomeBuilder.java`'s
//! nested-loop structure. The oracle dump
//! (`scripts/worldgen-oracle/BiomeOracle.java`, mode `table`) says **7594**.
//! The finding that made this cheap still holds — no part of the 1124-line
//! builder needed transliterating, only its resolved output, reachable with
//! zero bootstrap via `MultiNoiseBiomeSourceParameterList.knownPresets()` —
//! but the row count itself was a guess from reading Java control flow, not a
//! measurement, and the guess was off by 10x. A brute-force search over 7594
//! points is still trivial (a few thousand squared-distance comparisons per
//! quart column), so this doesn't change the cost story, only the "~700"
//! number quoted in the issue and epic.
//!
//! # The y = 0 trap
//!
//! An early version of this module's oracle fixtures sampled climate at a
//! fixed `y = 0` for every column, reasoning that `y = 0` is quart-aligned
//! (`0 % 4 == 0`) and simple. That produced almost exclusively cave and
//! deep-ocean biomes (`lush_caves`, `dripstone_caves`, `deep_dark`,
//! `deep_ocean`) at ordinary overworld surface coordinates — measured via
//! `BiomeOracle sample`, not assumed correct. The cause: `depth`'s density
//! function (`overworld/depth.json`) is `y_clamped_gradient(from_y: -64,
//! from_value: 1.5, to_y: 320, to_value: -1.5) + offset`, so at `y = 0` the
//! gradient term alone is already ≈ +1.0 — solidly in "deep underground"
//! climate-space, since vanilla's real per-quart 3-D biome assignment expects
//! `depth ≈ 0` only *near a column's own terrain surface*, not at a global
//! height. **This module samples at each quart's own generated surface
//! height** (the `heights[]` array [`crate::overworld::OverworldGenerator`]'s
//! fluid-fill stage already computes), not a constant — confirmed to recover
//! plausible surface biomes (plains, forest, savanna, beach, swamp, ocean
//! variants) at the same coordinates.
//!
//! # Resolution: one biome per quart column, not per quart cube
//!
//! Vanilla's real biome assignment is fully 3-D — every `(quartX, quartY,
//! quartZ)` gets its own sample, so a single `(x, z)` column can carry
//! different biomes at different depths (surface vs. a deep-dark cave
//! pocket). This port computes **one climate sample per horizontal quart
//! `(qx, qz)`** (16 per chunk, at that quart's own surface height) and
//! broadcasts it to every `y` in that quart column. This is deliberately
//! scoped to what Phase 1 needs to be observable and testable — "the biome a
//! player sees while exploring the surface" — and is *cheaper*, not just
//! simpler, than full 3-D: caves are not composed into
//! [`crate::overworld::OverworldGenerator`] yet (issue #295 / Phase 2), so
//! there is no cave volume for a vertically-varying biome to describe today.
//! Revisiting this is the natural first step of Phase 2, once caves exist to
//! carry `dripstone_caves`/`lush_caves`/`deep_dark` into.
//!
//! # Three biomes this port could not surface, until now
//!
//! `minecraft:badlands`, `minecraft:eroded_badlands` and
//! `minecraft:wooded_badlands` all reach `SurfaceRules.Bandlands` in the
//! overworld surface rule tree (confirmed by walking the JSON: both
//! `bandlands` nodes sit under a `condition{biome_is:[badlands,
//! eroded_badlands, wooded_badlands]}` guard, nothing else). Vanilla's
//! `Bandlands` rule delegates to `SurfaceSystem.getBand` — **now ported**
//! (`crate::surface`'s `Rule::Bandlands`/`BandBlocks`/`generate_bands`, issue
//! #295's carried-over gap): its own noise (`clay_bands_offset`) and the
//! banded-terracotta-column generator (`SurfaceSystem.java:332+`,
//! `generateBands`/`makeBands`/`getBand`) are reproduced from the documented
//! algorithm, checked against the running server.
//!
//! Before this module existed those three biomes were unreachable (the world
//! ran under a single fixed `minecraft:plains`), so `Rule::Bandlands`'s old
//! panic was dead code; once real biome variety landed, reaching it would
//! have crashed chunk generation the moment a player's world contained
//! badlands, so [`usable_overworld_table`] excluded exactly these three from
//! the searchable table as a deliberate, documented Phase 1 gap. That
//! exclusion is now removed — [`usable_overworld_table`] is a pass-through —
//! so the nearest-neighbour search can select any of the three again.
//! [`UNSUPPORTED_SURFACE_BIOMES`] itself is kept (not deleted): it is a
//! public item another crate imports by name (see its own doc comment).
//!
//! **Not ported by this increment**: `SurfaceSystem.erodedBadlandsExtension`
//! (the separate stone-pillar height extension unconditionally applied to
//! every `eroded_badlands` column, unrelated to `getBand`'s terracotta
//! banding) and `frozenOceanExtension` (a different biome pair entirely).
//! Neither is reachable through `Rule::Bandlands`, and un-filtering the
//! three names above does not require either — see
//! `docs/worldgen-parity.md` for what was and wasn't measured here.

use std::collections::HashMap;

use serde_json::Value;

use crate::density::{Builder, Context, Density};

/// The three biomes [`usable_overworld_table`] used to exclude before
/// `SurfaceSystem.getBand` was ported — see the module doc's "Three biomes
/// this port could not surface, until now" section. The name is now a
/// historical artifact, not a current filter: kept (rather than deleted or
/// renamed) because `lodestone_server::worldgen_data`'s
/// `served_columns_never_carry_an_unported_badlands_variant` test imports it
/// by this name, and that crate is outside this session's edit scope (see
/// this crate's own `CLAUDE.md` file-ownership note) — that test's own
/// premise is now stale and needs an update in its owning crate, not
/// something fixable from here.
pub const UNSUPPORTED_SURFACE_BIOMES: [&str; 3] = [
    "minecraft:badlands",
    "minecraft:eroded_badlands",
    "minecraft:wooded_badlands",
];

/// One climate axis's quantized `[min, max]` span — `Climate.Parameter`'s
/// internal representation (`(coord * 10000.0f) as i64`, already applied
/// before this type is ever constructed; see [`quantize_coord`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Parameter {
    pub min: i64,
    pub max: i64,
}

impl Parameter {
    /// `Climate.Parameter.distance(long)`.
    #[must_use]
    fn distance(&self, target: i64) -> i64 {
        let above = target - self.max;
        let below = self.min - target;
        if above > 0 { above } else { below.max(0) }
    }
}

/// A biome's climate cell: 7 quantized spans in vanilla's fixed axis order
/// (temperature, humidity, continentalness, erosion, depth, weirdness,
/// offset). `offset` is stored as a degenerate `[o, o]` span so it folds into
/// the same generic distance formula as the other six — exactly what
/// `Climate.ParameterPoint.parameterSpace()` does internally (it appends
/// `Parameter(offset, offset)` as element 7 before handing the array to the
/// RTree/brute-force search), so this is not a simplification, it is the
/// same representation vanilla uses.
#[derive(Debug, Clone)]
pub struct BiomeParameterPoint {
    /// `[temperature, humidity, continentalness, erosion, depth, weirdness, offset]`.
    pub params: [Parameter; 7],
    pub biome: String,
}

impl BiomeParameterPoint {
    /// `Climate.ParameterPoint.fitness(TargetPoint)`. `target`'s 7th slot
    /// (offset) is always `0` for a real climate sample — only a biome's own
    /// parameter point ever carries a nonzero offset span.
    #[must_use]
    fn fitness(&self, target: &[i64; 7]) -> i64 {
        let mut sum = 0i64;
        for i in 0..7 {
            let d = self.params[i].distance(target[i]);
            sum += d * d;
        }
        sum
    }
}

/// Quantizes a climate coordinate exactly as `Climate.quantizeCoord` does:
/// `(long)(coord * 10000.0F)`. The multiplication happens in **`f32`**
/// precision in vanilla (the value is cast to `float` before this point, in
/// `Climate.Sampler.sample`'s `(float)this.temperature.compute(context)`), so
/// this casts to `f32` first — not a `f64` quantization rounded afterward —
/// to reproduce the exact same truncation vanilla gets.
#[must_use]
pub fn quantize_coord(v: f64) -> i64 {
    ((v as f32) * 10000.0_f32) as i64
}

/// Finds the nearest biome by squared climate distance —
/// `Climate.ParameterList.findValueBruteForce` (`Climate.java:182`), vanilla's
/// own un-optimized reference search sitting next to the RTree it also ships.
/// The RTree is purely a lookup-speed optimization over the same table (plan
/// §2/§7: "same nearest point, different lookup structure"), safe to skip —
/// this port never builds one, since a few thousand squared-distance
/// comparisons per quart column is already fast.
///
/// Matches vanilla's tie-break exactly: ties keep the **earlier** table
/// entry (`if (fitness < bestFitness)`, strict `<`), so `table`'s order must
/// match the oracle dump's order — [`parse_table`] preserves JSON array
/// order for exactly this reason.
///
/// # Panics
/// Panics if `table` is empty.
#[must_use]
pub fn nearest_biome<'a>(table: &'a [BiomeParameterPoint], target: &[i64; 7]) -> &'a str {
    // Diagnostic D5. Both numbers matter and neither implies the other: the
    // search *count* is what U9's memoisation reduces, while `table.len()` rows
    // per search is what the RTree port reduces. A single "searches" counter
    // would make an RTree that searched just as often look like no improvement.
    crate::counters::bump_biome_search(table.len() as u64);
    let mut best = &table[0];
    let mut best_fitness = best.fitness(target);
    for entry in &table[1..] {
        let fitness = entry.fitness(target);
        if fitness < best_fitness {
            best_fitness = fitness;
            best = entry;
        }
    }
    &best.biome
}

/// Parses the embedded overworld biome-parameter table dumped by
/// `BiomeOracle`'s `table` mode. Schema: a JSON array of 14-element rows,
/// `[tMin,tMax,hMin,hMax,cMin,cMax,eMin,eMax,dMin,dMax,wMin,wMax,offset,"biome"]`
/// — the same order [`BiomeParameterPoint::params`] uses, and the raw
/// quantized `long`s Java's own `Climate.Parameter` carries internally (not
/// re-derived from decimal floats), so parsing never round-trips through a
/// second float parse.
///
/// # Panics
/// Panics on any row that isn't exactly 13 numbers followed by a string.
#[must_use]
pub fn parse_table(value: &Value) -> Vec<BiomeParameterPoint> {
    value
        .as_array()
        .expect("biome parameter table must be a JSON array")
        .iter()
        .map(|row| {
            let row = row.as_array().expect("biome parameter row must be an array");
            assert_eq!(
                row.len(),
                14,
                "biome parameter row must have 13 numbers + 1 biome name, got {}",
                row.len()
            );
            let n = |i: usize| {
                row[i]
                    .as_i64()
                    .unwrap_or_else(|| panic!("biome parameter row[{i}] is not an integer"))
            };
            let point = |lo: usize, hi: usize| Parameter {
                min: n(lo),
                max: n(hi),
            };
            let offset = n(12);
            let params = [
                point(0, 1),
                point(2, 3),
                point(4, 5),
                point(6, 7),
                point(8, 9),
                point(10, 11),
                Parameter {
                    min: offset,
                    max: offset,
                },
            ];
            let biome = row[13]
                .as_str()
                .expect("biome parameter row[13] must be a biome id string")
                .to_string();
            BiomeParameterPoint { params, biome }
        })
        .collect()
}

/// Used to drop [`UNSUPPORTED_SURFACE_BIOMES`] from a parsed table before
/// `SurfaceSystem.getBand` was ported (`crate::surface::Rule::Bandlands`) —
/// see the module doc's "Three biomes this port could not surface, until
/// now" section. Now a pass-through: every biome in the parsed table,
/// including the three formerly-excluded badlands variants, is searchable.
/// Kept as a named function (not inlined away at call sites) so
/// [`crate::overworld::OverworldGenerator::new`] doesn't need to change, and
/// so a future exclusion has an obvious place to live again.
#[must_use]
pub fn usable_overworld_table(table: Vec<BiomeParameterPoint>) -> Vec<BiomeParameterPoint> {
    table
}

/// Parses the embedded per-biome `temperature` map (`{"minecraft:plains":
/// 0.8, ...}`, sourced directly from vanilla's own `data/minecraft/worldgen/
/// biome/*.json` files — Mojang's own generated data, CLAUDE.md's data-source
/// #1, no oracle needed since this field needs no runtime evaluation).
///
/// # Panics
/// Panics if `value` is not a JSON object of biome-id -> number.
#[must_use]
pub fn parse_temperatures(value: &Value) -> HashMap<String, f32> {
    value
        .as_object()
        .expect("biome temperature table must be a JSON object")
        .iter()
        .map(|(k, v)| {
            let t = v
                .as_f64()
                .unwrap_or_else(|| panic!("biome temperature for {k} is not a number"))
                as f32;
            (k.clone(), t)
        })
        .collect()
}

/// Approximates `Biome.warmEnoughToRain`/`coldEnoughToSnow`'s `< 0.15`
/// threshold from the biome's *declared* `temperature` field, ignoring the
/// per-block height adjustment (`Biome.getHeightAdjustedTemperature`, a noise
/// + `(y - seaLevel - 17) * 0.05/40` correction above `seaLevel + 17`) and any
/// `temperature_modifier` (e.g. `frozen`, which lowers the effective value
/// for a handful of ocean biomes). This is not a new simplification: before
/// this module existed, `cold_enough_to_snow` was already a single fixed
/// bool for the whole world (`worldgen_data::DEFAULT_BIOME_SNOWS`); this just
/// computes that same kind of answer per selected biome instead of once
/// globally. Revisiting the height adjustment is a small, independent
/// follow-up if a snow-line seam near `sea_level + 17` ever needs it.
#[must_use]
pub fn cold_enough_to_snow(temperatures: &HashMap<String, f32>, biome: &str) -> bool {
    temperatures.get(biome).is_none_or(|&t| t < 0.15)
}

/// Evaluates the six named climate channels (`noise_router.{temperature,
/// vegetation, continents, erosion, depth, ridges}` — `vegetation` is
/// vanilla's field name for humidity, `ridges` for weirdness) at a block
/// position, quantizing exactly as `Climate.Sampler.sample`/`Climate.target`
/// do. Built once per generator (like [`crate::overworld::OverworldGenerator`]'s
/// `final_density`), reusing the same [`Density`] interpreter that
/// `region_parity`'s whole-region test already proves bit-exact against the
/// JVM for these exact six outputs (`RegionOracle.java` dumps
/// `continents`/`erosion`/`ridges`/`temperature`/`vegetation`/`depth`
/// directly) — so nothing new needs re-verifying here except the
/// quantization and the search.
#[allow(missing_debug_implementations)]
pub struct ClimateSampler {
    temperature: Density,
    humidity: Density,
    continentalness: Density,
    erosion: Density,
    depth: Density,
    weirdness: Density,
}

impl ClimateSampler {
    #[must_use]
    pub fn new(settings: &Value, builder: &Builder) -> Self {
        let router = &settings["noise_router"];
        Self {
            temperature: builder.build(&router["temperature"]),
            humidity: builder.build(&router["vegetation"]),
            continentalness: builder.build(&router["continents"]),
            erosion: builder.build(&router["erosion"]),
            depth: builder.build(&router["depth"]),
            weirdness: builder.build(&router["ridges"]),
        }
    }

    /// `Climate.Sampler.sample`'s quantized target, at an exact block
    /// position (the caller is responsible for quart-aligning `x`/`z` and
    /// picking `y`; see the module doc's "y = 0 trap" section for why `y`
    /// must be the column's own surface height, not a constant). The 7th
    /// slot (offset) is always `0`: a *target* point never carries an
    /// offset, only a biome's own [`BiomeParameterPoint`] does.
    #[must_use]
    pub fn target(&self, x: i32, y: i32, z: i32) -> [i64; 7] {
        let ctx = Context::new(x, y, z);
        [
            quantize_coord(self.temperature.compute(ctx)),
            quantize_coord(self.humidity.compute(ctx)),
            quantize_coord(self.continentalness.compute(ctx)),
            quantize_coord(self.erosion.compute(ctx)),
            quantize_coord(self.depth.compute(ctx)),
            quantize_coord(self.weirdness.compute(ctx)),
            0,
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tiny_table() -> Vec<BiomeParameterPoint> {
        // Two points on the temperature axis only, everything else spans the
        // full [-10000, 10000] range so only temperature discriminates.
        let full = || Parameter {
            min: -10000,
            max: 10000,
        };
        vec![
            BiomeParameterPoint {
                params: [
                    Parameter {
                        min: -10000,
                        max: -5000,
                    },
                    full(),
                    full(),
                    full(),
                    full(),
                    full(),
                    Parameter { min: 0, max: 0 },
                ],
                biome: "minecraft:cold".to_string(),
            },
            BiomeParameterPoint {
                params: [
                    Parameter {
                        min: 5000,
                        max: 10000,
                    },
                    full(),
                    full(),
                    full(),
                    full(),
                    full(),
                    Parameter { min: 0, max: 0 },
                ],
                biome: "minecraft:hot".to_string(),
            },
        ]
    }

    #[test]
    fn nearest_biome_picks_the_closer_temperature_band() {
        let table = tiny_table();
        assert_eq!(
            nearest_biome(&table, &[-9000, 0, 0, 0, 0, 0, 0]),
            "minecraft:cold"
        );
        assert_eq!(
            nearest_biome(&table, &[9000, 0, 0, 0, 0, 0, 0]),
            "minecraft:hot"
        );
        // Exactly equidistant (target 0 is 5000 from both spans) must keep
        // the *earlier* table entry — vanilla's strict `<` tie-break.
        assert_eq!(nearest_biome(&table, &[0, 0, 0, 0, 0, 0, 0]), "minecraft:cold");
    }

    #[test]
    fn quantize_matches_java_float_truncation() {
        // (long)(0.8f * 10000.0f) == 8000, exact.
        assert_eq!(quantize_coord(0.8), 8000);
        // Negative truncates toward zero, not floor.
        assert_eq!(quantize_coord(-0.15), -1500);
    }

    #[test]
    fn parse_table_round_trips_row_order_and_fields() {
        let json: Value = serde_json::from_str(
            r#"[[-10000,10000,-10000,10000,-12000,-10500,-10000,10000,0,0,-10000,10000,0,"minecraft:mushroom_fields"],
                [-10000,-4500,-10000,10000,-10500,-4550,-10000,10000,10000,10000,-10000,10000,7,"minecraft:deep_frozen_ocean"]]"#,
        )
        .unwrap();
        let table = parse_table(&json);
        assert_eq!(table.len(), 2);
        assert_eq!(table[0].biome, "minecraft:mushroom_fields");
        assert_eq!(table[0].params[2], Parameter { min: -12000, max: -10500 }, "continentalness");
        assert_eq!(table[0].params[6], Parameter { min: 0, max: 0 }, "offset");
        assert_eq!(table[1].biome, "minecraft:deep_frozen_ocean");
        assert_eq!(table[1].params[6], Parameter { min: 7, max: 7 }, "offset");
    }

    /// `usable_overworld_table` used to filter `UNSUPPORTED_SURFACE_BIOMES`
    /// out because `SurfaceSystem.getBand` (`crate::surface::Rule::Bandlands`)
    /// was unported and would panic if a column ever resolved to one of the
    /// three. Now that `getBand` is ported, the exclusion is gone — this test
    /// used to assert the *old* filtering behaviour (named
    /// `usable_table_excludes_the_three_unported_badlands_variants`); it now
    /// asserts the opposite, as a real control rather than a renamed no-op:
    /// badlands entering the table must actually change what the nearest
    /// search returns, not merely survive being present in a `Vec`.
    #[test]
    fn usable_table_no_longer_excludes_the_three_formerly_unported_badlands_variants() {
        let json: Value = serde_json::from_str(
            r#"[[-10000,10000,-10000,10000,-10000,10000,-10000,10000,-10000,10000,-10000,10000,0,"minecraft:badlands"],
                [-10000,10000,-10000,10000,-10000,10000,-10000,10000,-10000,10000,-10000,10000,0,"minecraft:plains"]]"#,
        )
        .unwrap();
        let table = usable_overworld_table(parse_table(&json));
        assert_eq!(table.len(), 2, "usable_overworld_table must no longer drop any row");
        assert!(
            table.iter().any(|p| p.biome == "minecraft:badlands"),
            "badlands must survive usable_overworld_table now that getBand is ported"
        );
        // The control: with only two rows sharing identical climate spans but
        // different biome names, `nearest_biome` breaks the tie by table
        // order (first element wins ties in `fitness`'s strict `<`
        // comparison — see `nearest_biome`'s own loop). Since `parse_table`
        // preserves JSON row order and badlands is row 0 here, every target
        // must resolve to badlands — proving the search can actually select
        // it, not just that it's present in the `Vec`.
        for target in [
            [-10000, -10000, -10000, -10000, -10000, -10000, 0],
            [10000, 10000, 10000, 10000, 10000, 10000, 0],
            [0, 0, 0, 0, 0, 0, 0],
        ] {
            assert_eq!(nearest_biome(&table, &target), "minecraft:badlands");
        }
    }

    #[test]
    fn cold_enough_to_snow_matches_known_biomes() {
        let mut temps = HashMap::new();
        temps.insert("minecraft:plains".to_string(), 0.8_f32);
        temps.insert("minecraft:snowy_taiga".to_string(), -0.5_f32);
        assert!(!cold_enough_to_snow(&temps, "minecraft:plains"));
        assert!(cold_enough_to_snow(&temps, "minecraft:snowy_taiga"));
        // Unknown biome: fail safe toward "cold" (matches the pre-existing
        // global default before this biome existed, `DEFAULT_BIOME_SNOWS`'s
        // sibling constant's own conservative choice is `false`, but an
        // *unknown* biome name is a data bug worth being loud about via snow
        // rather than silently matching the common case).
        assert!(cold_enough_to_snow(&temps, "minecraft:not_a_real_biome"));
    }
}
