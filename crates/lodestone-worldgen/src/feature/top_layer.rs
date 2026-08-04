//! `TOP_LAYER_MODIFICATION` — vanilla's `freeze_top_layer`
//! (`SnowAndFreezeFeature`): snow layers and surface ice, issue #404's U2.
//!
//! This is the **eleventh and last** decoration step
//! (`GenerationStep.Decoration.TOP_LAYER_MODIFICATION.ordinal() == 10`), and it
//! is in **every** biome's step list — `BiomeDefaultFeatures.java:413` adds it
//! unconditionally from `addDefaultOverworldLandMesaFeatures`' shared tail, so
//! the step self-gates on temperature rather than on biome membership. That is
//! why its absence showed up everywhere cold at once: before this module,
//! nothing in this engine ever wrote `minecraft:snow` or surface ice.
//!
//! ## What vanilla does, line by line
//!
//! `SnowAndFreezeFeature.place` (`SnowAndFreezeFeature.java:20-49`) walks the
//! chunk's 16×16 columns, `dx` outer and `dz` inner, and per column:
//!
//! 1. `int y = level.getHeight(MOTION_BLOCKING, x, z)` — the first **free** Y
//!    above the column's top motion-blocking-or-fluid block. See
//!    [`motion_blocking_first_free`] for the two cancelling `±1`s behind that.
//! 2. `topPos = (x, y, z)`, `belowPos = topPos.below()`.
//! 3. `biome = level.getBiome(topPos)`.
//! 4. **Ice**: `if biome.shouldFreeze(level, belowPos, false) → setBlock(belowPos,
//!    ICE)`. Note `checkNeighbors = false`, so the four-way `isWaterAt` test in
//!    `Biome.shouldFreeze` (`Biome.java:158-165`) never runs during world
//!    generation — it is a *live-world* freeze concern only.
//! 5. **Snow**: `if biome.shouldSnow(level, topPos) → setBlock(topPos, SNOW)`,
//!    then if the block at `belowPos` has the `snowy` property, set it `true`.
//!
//! Step 4 happens **before** step 5 and writes into the same column, which is
//! load-bearing: on a frozen ocean the ice written at `belowPos` is what step 5
//! then reads for `canSurvive`, and `minecraft:ice` is in
//! `cannot_support_snow_layer`. So frozen oceans get bare ice, never
//! snow-on-ice. Reordering the two produces a snow blanket over every frozen
//! ocean, and no unit test of either predicate alone would notice.
//!
//! ## RNG: none, at all
//!
//! `SnowAndFreezeFeature.java` does not contain the string `random` — not as a
//! field, a parameter, or an unused import. Its placed feature
//! (`placed_feature/freeze_top_layer.json`) is `[{"type": "minecraft:biome"}]`
//! and nothing else, and `BiomeFilter.shouldPlace` never touches the
//! `RandomSource` it is handed (`BiomeFilter.java:20-26`,
//! `PlacementFilter.java:8-11`). So this step consumes **zero draws**, and the
//! `TrapezoidInt`-as-`Uniform` draw-count desync that broke vegetation parity
//! (`crate::feature::IntProvider::Trapezoid`'s doc comment) has no analogue
//! here. Adding it also cannot desynchronise any earlier step: vanilla reseeds
//! per feature with `setFeatureSeed(decorationSeed, globalFeatureIndex,
//! stepIndex)` (`ChunkGenerator.java:389`), and this step's index is 10, past
//! every step this engine composes.
//!
//! ## The temperature source, which is the whole trap
//!
//! `Biome.warmEnoughToRain` is `getTemperature(pos, seaLevel) >= 0.15F`
//! (`Biome.java:175-177`), and `getTemperature` is **height-adjusted**
//! (`Biome.getHeightAdjustedTemperature`, `Biome.java:112-121`): above
//! `seaLevel + 17` it subtracts a noise-perturbed lapse rate. Using the flat
//! biome `temperature` field instead — which is exactly what this crate's
//! pre-existing [`crate::biome::cold_enough_to_snow`] does for *surface rules* —
//! is a plausible-looking error with a very visible signature:
//!
//! * `windswept_hills` has `temperature = 0.2`, comfortably above the `0.15`
//!   threshold, so a flat reading says "never snows". The height-adjusted
//!   reading crosses the threshold at roughly `y = 120`, which is why vanilla's
//!   windswept hills have bare stone low down and a snow cap on top. A flat port
//!   deletes the snow cap.
//! * Conversely `snowy_plains` (`temperature = 0.0`) snows at every altitude
//!   under either reading, so **a fixture in a snowy biome cannot tell the two
//!   apart**. `tests/top_layer_parity.rs`'s `windswept_hills` fixture exists
//!   specifically to discriminate them.
//!
//! [`crate::biome::cold_enough_to_snow`] is *not* wrong for its own caller
//! (surface rules ask a different question at a different Y), and this module
//! deliberately does not reuse it. See [`height_adjusted_temperature`].
//!
//! ## Approximations, named
//!
//! * **Block light is not modelled, and does not need to be.** Both predicates
//!   gate on `level.getBrightness(LightLayer.BLOCK, pos) < 10`
//!   (`Biome.java:150,188`). At the `features` chunk status that value is
//!   **always 0**: `initialize_light` runs strictly after `features`
//!   (`ChunkStatus.java:28-30`), and `BlockLightSectionStorage.getLightValue`
//!   returns `0` for a section with no `DataLayer`
//!   (`BlockLightSectionStorage.java:16-26`). So the gate is unconditionally
//!   satisfied in vanilla too. This is agreement, not a shortcut — do not
//!   "improve" it by supplying real light.
//! * **Biome is per-quart-column, not per block.** `level.getBiome(topPos)` is a
//!   3-D lookup; this engine's biome stage is 2-D (one sample per horizontal
//!   quart, broadcast down the column — see `crate::biome`'s module doc and
//!   census row 2 of `docs/plans/worldgen-parity.md`). Since `topPos` is at the
//!   surface, which is the Y that stage already samples at, this is the closest
//!   available answer rather than a new approximation, and it becomes exact when
//!   U7 lands 3-D biomes.
//! * **`Biome.getTemperature`'s 1024-entry `Long2FloatLinkedOpenHashMap`**
//!   (`Biome.java:123-139`) is a pure memo keyed by `pos.asLong()`. Omitted: it
//!   cannot change a value, only how often one is computed.
//! * **Collision shapes are read with no neighbours.** `canSurvive`'s
//!   `isFaceFull` answer arrives pre-computed through [`SnowSupport`] from
//!   `lodestone_data::snow_support`, whose dump calls the same two-argument
//!   `getCollisionShape` overload vanilla's `canSurvive` does. See that module's
//!   "Known scope".

use std::collections::{HashMap, HashSet};

use serde_json::Value;

use crate::dense_grid::DenseBlockGrid;
use crate::noise::ClimateNoise;

/// `GenerationStep.Decoration.TOP_LAYER_MODIFICATION.ordinal()` — the eleventh
/// and last decoration step. One past `VEGETAL_DECORATION`
/// ([`super::STEP_VEGETAL_DECORATION`], 9).
pub const STEP_TOP_LAYER_MODIFICATION: i32 = 10;

/// The block state `SnowAndFreezeFeature` writes at `topPos`:
/// `Blocks.SNOW.defaultBlockState()`, i.e. one layer
/// (`SnowLayerBlock` registers `LAYERS = 1` as its default,
/// `SnowLayerBlock.java:38`).
pub const SNOW_LAYER: &str = "minecraft:snow[layers=1]";

/// The block state written at `belowPos` when a column freezes:
/// `Blocks.ICE.defaultBlockState()`. `IceBlock` has no properties.
pub const ICE: &str = "minecraft:ice";

/// `Biome.warmEnoughToRain`'s threshold (`Biome.java:176`). A column snows when
/// its height-adjusted temperature is **strictly below** this.
pub const RAIN_TEMPERATURE_THRESHOLD: f32 = 0.15;

/// `Biome.getHeightAdjustedTemperature`'s `snowLevel` offset above sea level
/// (`Biome.java:114`). Below this Y the biome's declared temperature is used
/// unmodified; above it the lapse rate applies.
pub const SNOW_LEVEL_ABOVE_SEA: i32 = 17;

/// `Biome.TemperatureModifier` (`Biome.java:388-420`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum TemperatureModifier {
    /// `NONE` — returns the biome's declared temperature unchanged.
    #[default]
    None,
    /// `FROZEN` — the ice-patch noise blend used by `frozen_ocean` and
    /// `deep_frozen_ocean`, and by nothing else in 26.2 (verified across
    /// `data/minecraft/worldgen/biome/*.json`). See
    /// [`height_adjusted_temperature`] for the ported formula.
    Frozen,
}

impl TemperatureModifier {
    /// Parses a biome document's `temperature_modifier` field. Absent means
    /// `NONE` (`Biome.ClimateSettings.CODEC`'s
    /// `optionalFieldOf("temperature_modifier", NONE)`, `Biome.java:363`).
    ///
    /// # Panics
    /// Panics on an unrecognised value rather than silently falling back to
    /// `NONE`: a new modifier in a future version changes where snow lands, and
    /// a silent fallback would look like a subtle parity residual instead of a
    /// missing port.
    #[must_use]
    pub fn parse(value: Option<&str>) -> Self {
        match value.map(|v| v.strip_prefix("minecraft:").unwrap_or(v)) {
            None | Some("none") => TemperatureModifier::None,
            Some("frozen") => TemperatureModifier::Frozen,
            Some(other) => panic!("unsupported temperature_modifier: {other}"),
        }
    }
}

/// One biome's `Biome.ClimateSettings` (`Biome.java:358`), minus `downfall`
/// (which no generation step reads — it is a client visual and a grass-colour
/// input).
#[derive(Clone, Copy, Debug)]
pub struct BiomeClimate {
    /// `hasPrecipitation`. Gates **snow only**: `Biome.shouldSnow` goes through
    /// `getPrecipitationAt`, which returns `NONE` without it
    /// (`Biome.java:104-110`), while `Biome.shouldFreeze` does **not** consult it
    /// at all (`Biome.java:145-169`). So a `has_precipitation: false` biome that
    /// is nonetheless cold enough would still form ice — vanilla's behaviour,
    /// preserved here even though no overworld biome reaches it (the coldest such
    /// biome is `temperature = 0.5`, which needs `y > ~360` to cross the
    /// threshold, above the overworld's `maxY` of 319).
    pub has_precipitation: bool,
    /// `temperature`, the declared field — **not** the value the predicates use.
    /// See [`height_adjusted_temperature`].
    pub temperature: f32,
    /// `temperature_modifier`.
    pub temperature_modifier: TemperatureModifier,
}

/// The per-block-state facts `freeze_top_layer` needs, resolved once per
/// generator into string-keyed lookups.
///
/// Every field originates in a jar dump, never in this crate: the four
/// per-state predicates come from `lodestone_data::snow_support` (whose own
/// module doc records the four hand guesses the dump contradicted) and the two
/// tags from vanilla's `tags/block/*.json` through
/// [`crate::density::Resolver::block_tag`].
///
/// # Why lookups are two-level
///
/// The generator's block field holds canonical state strings, but it emits
/// fluids **without** the `level` property (`docs/worldgen-parity.md`'s "Known
/// representation gap") — so a column's water reads as `minecraft:water`, not
/// `minecraft:water[level=0]`. Since `is_water_source_liquid_block` is true for
/// exactly one water state, an exact-string lookup would silently stop every
/// ocean from freezing. [`StatePredicate::test`] therefore falls back from the
/// exact state to the block's **default state**'s answer, which is what a
/// property-less name means.
#[derive(Clone, Debug, Default)]
pub struct SnowSupport {
    /// `BlockState.blocksMotion()`, from `lodestone_data::block_solidity`.
    pub blocks_motion: StatePredicate,
    /// `!BlockState.getFluidState().isEmpty()`.
    pub has_fluid_state: StatePredicate,
    /// `getFluidState().is(Fluids.WATER) && block instanceof LiquidBlock`.
    pub water_source: StatePredicate,
    /// `Block.isFaceFull(collisionShape, UP)`.
    pub face_full_up: StatePredicate,
    /// `BlockState.hasProperty(BlockStateProperties.SNOWY)`.
    pub snowy_property: StatePredicate,
    /// `BlockTags.CANNOT_SUPPORT_SNOW_LAYER`, by block base name.
    pub cannot_support_snow_layer: HashSet<String>,
    /// `BlockTags.SUPPORT_OVERRIDE_SNOW_LAYER`, by block base name.
    pub support_override_snow_layer: HashSet<String>,
}

/// A per-block-state boolean, stored as "the answer for each block's default
/// state" plus an override for every state that disagrees with its own default.
///
/// This is the compaction the resolver JSON uses: 26.2 has 32,366 states across
/// 1,196 blocks, and for most blocks every state agrees, so a per-block answer
/// plus a short override list is two orders of magnitude smaller than a
/// per-state map — while staying **exact**, because the overrides are complete
/// rather than a curated subset.
#[derive(Clone, Debug, Default)]
pub struct StatePredicate {
    /// Answer for each block's default state, keyed by base name
    /// (`minecraft:water`). Absent means `false`.
    by_block_default: HashSet<String>,
    /// Every state whose answer differs from its block's default, keyed by full
    /// canonical state string (`minecraft:water[level=0]`).
    by_state: HashMap<String, bool>,
}

impl StatePredicate {
    /// Builds from the two halves. `by_state` must list **every** disagreeing
    /// state; a partial list is silently wrong, which is why it is produced by a
    /// full walk of the state registry rather than by hand.
    #[must_use]
    pub fn new(by_block_default: HashSet<String>, by_state: HashMap<String, bool>) -> Self {
        Self {
            by_block_default,
            by_state,
        }
    }

    /// The answer for a canonical block-state string.
    ///
    /// Exact state first, then the block's default-state answer — see
    /// [`SnowSupport`]'s "Why lookups are two-level".
    #[must_use]
    pub fn test(&self, state: &str) -> bool {
        if let Some(&answer) = self.by_state.get(state) {
            return answer;
        }
        self.by_block_default.contains(base_id(state))
    }

    /// `true` when nothing was supplied — the "no data supplied" convention
    /// every other resolver-fed table in this crate follows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.by_block_default.is_empty() && self.by_state.is_empty()
    }

    /// Parses one column of the resolver's `block_freeze_facts` document:
    /// `{"default": ["minecraft:stone", ...], "states": {"minecraft:snow[layers=8]": false, ...}}`.
    ///
    /// # Panics
    /// Panics on a malformed document — this is embedded, generated data, so a
    /// shape error is a build-time defect rather than untrusted input.
    #[must_use]
    pub fn parse(value: &Value) -> Self {
        if value.is_null() {
            return Self::default();
        }
        let by_block_default = value
            .get("default")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .map(|v| v.as_str().expect("default entry is a string").to_owned())
                    .collect()
            })
            .unwrap_or_default();
        let by_state = value
            .get("states")
            .and_then(Value::as_object)
            .map(|o| {
                o.iter()
                    .map(|(k, v)| {
                        (
                            k.clone(),
                            v.as_bool().expect("states entry is a boolean"),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self::new(by_block_default, by_state)
    }
}

impl SnowSupport {
    /// Parses the whole `block_freeze_facts` document plus the two already-
    /// resolved tag sets.
    #[must_use]
    pub fn parse(
        facts: &Value,
        cannot_support_snow_layer: HashSet<String>,
        support_override_snow_layer: HashSet<String>,
    ) -> Self {
        Self {
            blocks_motion: StatePredicate::parse(&facts["blocks_motion"]),
            has_fluid_state: StatePredicate::parse(&facts["has_fluid_state"]),
            water_source: StatePredicate::parse(&facts["water_source"]),
            face_full_up: StatePredicate::parse(&facts["face_full_up"]),
            snowy_property: StatePredicate::parse(&facts["snowy_property"]),
            cannot_support_snow_layer,
            support_override_snow_layer,
        }
    }

    /// `true` when no per-state facts were supplied at all, i.e. this engine has
    /// no data to run on and [`apply_freeze_top_layer`] must be a no-op. The two
    /// tags are deliberately **not** part of this test: both are legitimately
    /// small, and an empty `cannot_support_snow_layer` is a plausible (if wrong)
    /// datapack rather than "no data".
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.blocks_motion.is_empty()
            || self.has_fluid_state.is_empty()
            || self.face_full_up.is_empty()
    }

    /// `Heightmap.Types.MOTION_BLOCKING`'s predicate (`Heightmap.java:151`):
    /// `input.blocksMotion() || !input.getFluidState().isEmpty()`.
    #[must_use]
    pub fn motion_blocking(&self, state: &str) -> bool {
        self.blocks_motion.test(state) || self.has_fluid_state.test(state)
    }

    /// `SnowLayerBlock.canSurvive` (`SnowLayerBlock.java:76-86`), where
    /// `below_state` is the state at `pos.below()`.
    ///
    /// The two tag checks run **before** the geometry check, and both directions
    /// are load-bearing (measured in `lodestone-data`'s `tests/snow_support.rs`):
    /// `ice`/`packed_ice` have a full UP face and are only kept snow-free by
    /// `cannot_support_snow_layer`, while `mud`/`honey_block`/`soul_sand` have no
    /// full UP face and only support snow via `support_override_snow_layer`.
    ///
    /// The trailing `layers == 8` clause is not redundant: **no** snow state has
    /// a full UP collision face (a full snow layer is 14/16 tall), so without
    /// that clause snow could never stack on a full layer.
    #[must_use]
    pub fn snow_can_survive(&self, below_state: &str) -> bool {
        let base = base_id(below_state);
        if self.cannot_support_snow_layer.contains(base) {
            return false;
        }
        if self.support_override_snow_layer.contains(base) {
            return true;
        }
        self.face_full_up.test(below_state)
            || (base == "minecraft:snow" && snow_layers(below_state) == Some(8))
    }
}

/// Resolves a [`SnowSupport`] from a [`crate::density::Resolver`]: the five
/// per-state columns from
/// [`Resolver::block_freeze_facts`](crate::density::Resolver::block_freeze_facts)
/// and the two tags from
/// [`Resolver::block_tag`](crate::density::Resolver::block_tag).
///
/// Empty sets (never a panic) when the resolver has no data — matching
/// [`crate::feature::vegetation::build_veg_tags`] and every other
/// #295/#406/#427 resolver convention, so every existing `Resolver` in this
/// crate's tests and benches keeps compiling and keeps generating exactly the
/// snow-free world it did before.
#[must_use]
pub fn build_snow_support(resolver: &dyn crate::density::Resolver) -> SnowSupport {
    let resolve = |id: &str| {
        let mut out = HashSet::new();
        let mut seen = HashSet::new();
        crate::compose::resolve_block_tag(resolver, id, &mut out, &mut seen);
        out
    };
    SnowSupport::parse(
        &resolver.block_freeze_facts(),
        resolve("minecraft:cannot_support_snow_layer"),
        resolve("minecraft:support_override_snow_layer"),
    )
}

/// Parses one biome document's `ClimateSettings` — `has_precipitation`,
/// `temperature` and `temperature_modifier`, straight out of
/// [`Resolver::biome_document`](crate::density::Resolver::biome_document).
///
/// **No new resolver method is needed for climate**: vanilla's own
/// `worldgen/biome/*.json` carries all three fields, the embedded assets are
/// verbatim copies of them, and `biome_document` already returns the whole
/// document (issue #295 added it for `carvers` and the per-step `features`
/// lists). This crate's pre-existing
/// [`Resolver::biome_temperatures`](crate::density::Resolver::biome_temperatures)
/// carries `temperature` alone and is deliberately left to its own caller —
/// surface rules ask a *different* question, at a different Y, and answering it
/// with the height-adjusted value would change composed surface output.
///
/// Returns `None` for a document with no `temperature` field (including
/// `Value::Null`, the no-data-supplied case), which
/// [`apply_freeze_top_layer`] treats as "this biome does not freeze" rather than
/// defaulting.
///
/// Defaults follow `Biome.ClimateSettings.CODEC` (`Biome.java:359-366`):
/// `has_precipitation` is required in vanilla's schema but defaulted to `true`
/// here for robustness against a trimmed asset, and `temperature_modifier`
/// defaults to `none`.
#[must_use]
pub fn parse_biome_climate(document: &Value) -> Option<BiomeClimate> {
    let temperature = document.get("temperature")?.as_f64()? as f32;
    Some(BiomeClimate {
        has_precipitation: document
            .get("has_precipitation")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        temperature,
        temperature_modifier: TemperatureModifier::parse(
            document
                .get("temperature_modifier")
                .and_then(Value::as_str),
        ),
    })
}

/// The base block id of a canonical state string: `minecraft:snow[layers=2]` ->
/// `minecraft:snow`.
#[must_use]
pub fn base_id(state: &str) -> &str {
    state.split('[').next().unwrap_or(state)
}

/// The `layers` property of a `minecraft:snow[...]` state, if present.
fn snow_layers(state: &str) -> Option<u32> {
    let props = state.split_once('[')?.1.strip_suffix(']')?;
    props
        .split(',')
        .find_map(|kv| kv.strip_prefix("layers="))
        .and_then(|v| v.parse().ok())
}

/// `Biome.getHeightAdjustedTemperature(pos, seaLevel)` (`Biome.java:112-121`),
/// which is what `Biome.getTemperature` returns once its memo is stripped.
///
/// ```text
/// float adjusted = temperatureModifier.modifyTemperature(pos, baseTemperature);
/// int snowLevel = seaLevel + 17;
/// if (pos.getY() > snowLevel) {
///     float v = (float)(TEMPERATURE_NOISE.getValue(pos.getX() / 8.0F, pos.getZ() / 8.0F, false) * 8.0);
///     return adjusted - (v + pos.getY() - snowLevel) * 0.05F / 40.0F;
/// }
/// return adjusted;
/// ```
///
/// # Float typing is not incidental
///
/// Every arithmetic step above is Java `float`, and the noise *input* is a
/// `float` division widened to `double`: `pos.getX() / 8.0F` is `(double)((float)
/// x / 8.0f)`, **not** `(double) x / 8.0`. Those differ for large coordinates,
/// and computing the whole expression in `f64` shifts the snow line by a
/// fraction of a block near the threshold — enough to move which columns snow.
/// This port keeps `f32` throughout and widens only at the noise call, matching
/// the JVM bit for bit.
///
/// # The `FROZEN` modifier
///
/// `TemperatureModifier.FROZEN.modifyTemperature` (`Biome.java:395-409`) blends
/// three noise reads into an ice-patch mask and clamps the result to `0.2`
/// (above the `0.15` rain threshold, i.e. *warmer*) inside a patch — so a frozen
/// ocean is mostly ice with warmer, unfrozen gaps. Its `pos.getX() * 0.05`
/// inputs are `int * double`, genuine `f64`, unlike the `/ 8.0F` above.
#[must_use]
pub fn height_adjusted_temperature(
    climate: &BiomeClimate,
    noise: &ClimateNoise,
    x: i32,
    y: i32,
    z: i32,
    sea_level: i32,
) -> f32 {
    let adjusted = match climate.temperature_modifier {
        TemperatureModifier::None => climate.temperature,
        TemperatureModifier::Frozen => {
            let large_variation = noise.frozen_temperature(f64::from(x) * 0.05, f64::from(z) * 0.05)
                * 7.0;
            let edge_variation = noise.biome_info(f64::from(x) * 0.2, f64::from(z) * 0.2);
            let ice_patches = large_variation + edge_variation;
            if ice_patches < 0.3 {
                let small_variation =
                    noise.biome_info(f64::from(x) * 0.09, f64::from(z) * 0.09);
                if small_variation < 0.8 {
                    0.2
                } else {
                    climate.temperature
                }
            } else {
                climate.temperature
            }
        }
    };
    let snow_level = sea_level + SNOW_LEVEL_ABOVE_SEA;
    if y > snow_level {
        // `pos.getX() / 8.0F`: a float divide, then widened for `getValue`.
        let nx = f64::from(x as f32 / 8.0);
        let nz = f64::from(z as f32 / 8.0);
        let v = (noise.temperature(nx, nz) * 8.0) as f32;
        adjusted - (v + y as f32 - snow_level as f32) * 0.05 / 40.0
    } else {
        adjusted
    }
}

/// `Biome.warmEnoughToRain` (`Biome.java:175-177`).
#[must_use]
pub fn warm_enough_to_rain(
    climate: &BiomeClimate,
    noise: &ClimateNoise,
    x: i32,
    y: i32,
    z: i32,
    sea_level: i32,
) -> bool {
    height_adjusted_temperature(climate, noise, x, y, z, sea_level) >= RAIN_TEMPERATURE_THRESHOLD
}

/// `level.getHeight(Heightmap.Types.MOTION_BLOCKING, x, z)` as
/// `SnowAndFreezeFeature` sees it: the first **free** Y above the column's
/// topmost motion-blocking-or-fluid block, or `min_y` for an entirely empty
/// column.
///
/// # The two `±1`s that cancel
///
/// `Heightmap` stores `topMatchingY + 1` (`Heightmap.primeHeightmaps` does
/// `setHeight(x, z, y + 1)`, `Heightmap.java:64`), so
/// `Heightmap.getFirstAvailable` is the first free Y.
/// `Heightmap.getHighestTaken` subtracts one (`Heightmap.java:114`) and
/// `ChunkAccess.getHeight` routes to it — then `WorldGenRegion.getHeight` adds
/// one back (`WorldGenRegion.java:435`). Net: the feature receives the first
/// free Y, so `topPos` is where snow goes and `belowPos` is the top solid or
/// fluid block. Dropping either `±1` puts every snow layer one block out.
///
/// # Why recomputing is equivalent to vanilla's incremental heightmap
///
/// Vanilla primes these heightmaps once at the start of the `features` status
/// (`ChunkStatusTasks.java:134-138`) and then maintains them through
/// `Heightmap.update` as each feature places a block. `update` is an incremental
/// form of exactly this scan: it early-outs below `firstAvailable - 2`, raises
/// the height for an opaque block at or above it, and rescans downward when a
/// non-opaque block replaces the current top (`Heightmap.java:81-108`). A fresh
/// top-down scan of the finished field lands on the same answer, and this step
/// runs after every other feature, so there is nothing left to place.
#[must_use]
pub fn motion_blocking_first_free(
    grid: &DenseBlockGrid,
    support: &SnowSupport,
    x: i32,
    z: i32,
    min_y: i32,
    height: i32,
) -> i32 {
    let mut y = min_y + height - 1;
    while y >= min_y {
        if support.motion_blocking(grid.get(x, y, z)) {
            return y + 1;
        }
        y -= 1;
    }
    min_y
}

/// What one [`apply_freeze_top_layer`] pass did, for gates that need to assert a
/// count rather than scan a chunk.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FreezeCounts {
    /// Columns where `minecraft:ice` replaced a water source.
    pub ice: usize,
    /// Columns where a `minecraft:snow[layers=1]` was placed.
    pub snow: usize,
    /// Snow placements that additionally flipped a `snowy` property below.
    pub snowy_flips: usize,
}

impl FreezeCounts {
    /// `true` when the pass wrote nothing at all.
    #[must_use]
    pub fn is_empty(self) -> bool {
        self.ice == 0 && self.snow == 0 && self.snowy_flips == 0
    }
}

/// Runs the whole `TOP_LAYER_MODIFICATION` step for one chunk, in place.
///
/// `grid` must cover at least the chunk's own 16×16×`height` box in **absolute**
/// coordinates; `biome_at(local_x, local_z)` supplies the biome id for a column
/// (the caller resolves quart rounding — see this module's "Approximations,
/// named"), and `climates` maps biome id to its `ClimateSettings`.
///
/// **This step never writes outside its own chunk.** `SnowAndFreezeFeature`'s
/// loops are `dx`/`dz` in `0..16` from the chunk origin and every write is at
/// `(x, y, z)` or `(x, y - 1, z)` of that same column, so unlike ores
/// ([`super::apply_ore_step_3x3`]) and vegetation there is no
/// `blockStateWriteRadius(1)` spill to model and no 3×3 driver: a centre-only
/// pass **is** vanilla's full behaviour here. That is why this function takes one
/// chunk's grid rather than a stitched region.
///
/// A biome missing from `climates` is skipped, not defaulted — a defaulted
/// climate would place snow (or refuse to) on a guess, and this engine's whole
/// convention is that missing data means "do nothing", never "assume".
#[allow(clippy::too_many_arguments)]
pub fn apply_freeze_top_layer<'b>(
    grid: &mut DenseBlockGrid,
    chunk_x: i32,
    chunk_z: i32,
    min_y: i32,
    height: i32,
    sea_level: i32,
    biome_at: &dyn Fn(i32, i32) -> &'b str,
    climates: &HashMap<String, BiomeClimate>,
    support: &SnowSupport,
    noise: &ClimateNoise,
) -> FreezeCounts {
    let mut counts = FreezeCounts::default();
    if support.is_empty() {
        return counts;
    }
    let base_x = chunk_x * 16;
    let base_z = chunk_z * 16;
    let max_y = min_y + height - 1;
    // `dx` outer, `dz` inner — `SnowAndFreezeFeature.java:26-27`. Iteration
    // order cannot matter here (no column reads another), but it is kept
    // vanilla's so a future reader does not have to prove that again.
    for dx in 0..16 {
        for dz in 0..16 {
            let x = base_x + dx;
            let z = base_z + dz;
            let Some(climate) = climates.get(biome_at(dx, dz)) else {
                continue;
            };
            let top_y = motion_blocking_first_free(grid, support, x, z, min_y, height);
            let below_y = top_y - 1;

            // --- ice: Biome.shouldFreeze(level, belowPos, checkNeighbors=false)
            let inside_below = below_y >= min_y && below_y <= max_y;
            if inside_below && !warm_enough_to_rain(climate, noise, x, below_y, z, sea_level) {
                // The block-light gate (`< 10`) is unconditionally true during
                // worldgen — see this module's "Approximations, named".
                if support.water_source.test(grid.get(x, below_y, z)) {
                    grid.set(x, below_y, z, ICE);
                    counts.ice += 1;
                }
            }

            // --- snow: Biome.shouldSnow(level, topPos)
            if !climate.has_precipitation {
                continue;
            }
            let inside_top = top_y >= min_y && top_y <= max_y;
            if !inside_top || warm_enough_to_rain(climate, noise, x, top_y, z, sea_level) {
                continue;
            }
            let top_state = grid.get(x, top_y, z).to_owned();
            let top_base = base_id(&top_state);
            // `state.isAir() || state.is(Blocks.SNOW)` (`Biome.java:190`).
            if !(is_air(top_base) || top_base == "minecraft:snow") {
                continue;
            }
            // `canSurvive` reads the block BELOW topPos — which the ice write
            // above may just have changed. Reading it here rather than earlier is
            // what makes frozen oceans bare ice instead of snow-covered ice.
            let below_state = if inside_below {
                grid.get(x, below_y, z).to_owned()
            } else {
                "minecraft:air".to_owned()
            };
            if !support.snow_can_survive(&below_state) {
                continue;
            }
            grid.set(x, top_y, z, SNOW_LAYER);
            counts.snow += 1;
            if inside_below && support.snowy_property.test(&below_state) {
                grid.set(x, below_y, z, &with_snowy_true(&below_state));
                counts.snowy_flips += 1;
            }
        }
    }
    counts
}

/// `belowState.setValue(SnowyBlock.SNOWY, true)` on a canonical state string.
///
/// Property order in a canonical string is alphabetical (see
/// [`super::canon_state`]), and `snowy` is only ever carried by `grass_block`,
/// `podzol` and `mycelium` — each of which has `snowy` as its **only** property
/// (measured in `lodestone-data`'s `tests/snow_support.rs`: six states, three
/// blocks, two states each). The general rewrite is still done properly rather
/// than special-casing those three, so a future block with `snowy` alongside
/// other properties keeps working.
#[must_use]
fn with_snowy_true(state: &str) -> String {
    let Some((base, rest)) = state.split_once('[') else {
        return format!("{state}[snowy=true]");
    };
    let props = rest.strip_suffix(']').unwrap_or(rest);
    let mut kv: Vec<(&str, &str)> = props
        .split(',')
        .filter_map(|p| p.split_once('='))
        .map(|(k, v)| if k == "snowy" { (k, "true") } else { (k, v) })
        .collect();
    if !kv.iter().any(|(k, _)| *k == "snowy") {
        kv.push(("snowy", "true"));
    }
    kv.sort_by(|a, b| a.0.cmp(b.0));
    let body: Vec<String> = kv.iter().map(|(k, v)| format!("{k}={v}")).collect();
    format!("{base}[{}]", body.join(","))
}

fn is_air(base: &str) -> bool {
    matches!(
        base,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn climate(temperature: f32) -> BiomeClimate {
        BiomeClimate {
            has_precipitation: true,
            temperature,
            temperature_modifier: TemperatureModifier::None,
        }
    }

    #[test]
    fn snowy_rewrite_sets_the_property_and_keeps_canonical_order() {
        assert_eq!(
            with_snowy_true("minecraft:grass_block[snowy=false]"),
            "minecraft:grass_block[snowy=true]"
        );
        assert_eq!(
            with_snowy_true("minecraft:grass_block[snowy=true]"),
            "minecraft:grass_block[snowy=true]"
        );
        // Property-less form (the generator's shortest canonical spelling).
        assert_eq!(
            with_snowy_true("minecraft:podzol"),
            "minecraft:podzol[snowy=true]"
        );
        // A hypothetical multi-property block must stay alphabetically sorted.
        assert_eq!(
            with_snowy_true("minecraft:x[waterlogged=false,snowy=false,axis=y]"),
            "minecraft:x[axis=y,snowy=true,waterlogged=false]"
        );
    }

    #[test]
    fn snow_layers_parses_only_the_layers_property() {
        assert_eq!(snow_layers("minecraft:snow[layers=8]"), Some(8));
        assert_eq!(snow_layers("minecraft:snow[layers=1]"), Some(1));
        assert_eq!(snow_layers("minecraft:snow"), None);
        assert_eq!(snow_layers("minecraft:stone"), None);
    }

    /// The two-level lookup: an exact state wins, and a property-less name falls
    /// back to the block's default-state answer. This is what keeps the
    /// generator's `level`-less `minecraft:water` freezing.
    #[test]
    fn state_predicate_falls_back_from_state_to_block_default() {
        let mut by_state = HashMap::new();
        by_state.insert("minecraft:water[level=1]".to_owned(), false);
        let mut default = HashSet::new();
        default.insert("minecraft:water".to_owned());
        let p = StatePredicate::new(default, by_state);

        assert!(p.test("minecraft:water"), "property-less name uses the default state");
        assert!(
            p.test("minecraft:water[level=0]"),
            "the default state itself is not in `states`, so it falls through to the default"
        );
        assert!(!p.test("minecraft:water[level=1]"), "an override wins");
        assert!(!p.test("minecraft:stone"), "an unknown block is false");
    }

    /// The height adjustment must be inert at or below `seaLevel + 17` and active
    /// above it — the branch `Biome.java:115` takes.
    #[test]
    fn height_adjustment_is_inert_at_or_below_snow_level() {
        let noise = ClimateNoise::new();
        let c = climate(0.2);
        for y in [-64, 0, 63, 79, 80] {
            let t = height_adjusted_temperature(&c, &noise, 100, y, 200, 63);
            assert_eq!(
                t.to_bits(),
                0.2_f32.to_bits(),
                "y={y} is at or below seaLevel+17=80 and must be unadjusted"
            );
        }
        let above = height_adjusted_temperature(&c, &noise, 100, 81, 200, 63);
        assert_ne!(
            above.to_bits(),
            0.2_f32.to_bits(),
            "y=81 is above snowLevel and must be adjusted"
        );
    }

    /// The predicted-value gate on the lapse rate, not a direction check.
    ///
    /// At `y = snowLevel + d` the adjustment is `-(v + d) * 0.05 / 40`, where
    /// `v = noise * 8` is bounded by `±8` (a single-octave simplex field is in
    /// `[-1, 1]`). So the drop at `d` blocks above snow level lies in
    /// `[(d - 8) / 800, (d + 8) / 800]` — an interval that comes from vanilla's
    /// own constants and the noise field's range, not from this implementation.
    /// `windswept_hills` (`temperature = 0.2`) must therefore cross the `0.15`
    /// threshold somewhere in `d ∈ [32, 48]`, i.e. `y ∈ [112, 128]`.
    #[test]
    fn lapse_rate_lands_in_the_interval_vanilla_constants_predict() {
        let noise = ClimateNoise::new();
        let c = climate(0.2);
        let sea_level = 63;
        let snow_level = sea_level + SNOW_LEVEL_ABOVE_SEA;
        for d in [1, 10, 40, 100, 239] {
            let y = snow_level + d;
            let t = height_adjusted_temperature(&c, &noise, 1234, y, -567, sea_level);
            let drop = 0.2_f32 - t;
            let lo = (d as f32 - 8.0) / 800.0;
            let hi = (d as f32 + 8.0) / 800.0;
            assert!(
                (lo..=hi).contains(&drop),
                "d={d}: drop {drop} outside the [{lo}, {hi}] interval vanilla's \
                 0.05/40 lapse rate and the ±8 noise term allow"
            );
        }
        // The discriminating claim: a windswept-hills column snows high up and
        // does not snow low down. A flat-temperature port fails the first of
        // these (0.2 >= 0.15 everywhere), which is exactly the trap.
        assert!(
            !warm_enough_to_rain(&c, &noise, 1234, 200, -567, sea_level),
            "windswept_hills at y=200 must be cold enough to snow"
        );
        assert!(
            warm_enough_to_rain(&c, &noise, 1234, 70, -567, sea_level),
            "windswept_hills at y=70 must be too warm to snow"
        );
    }

    /// `snowy_plains` (`temperature = 0.0`) snows at every altitude under both a
    /// flat and a height-adjusted reading — the control proving the fixture
    /// choice in `tests/top_layer_parity.rs` matters. A gate built only on a
    /// snowy biome cannot detect the flat-temperature error.
    #[test]
    fn a_snowy_biome_cannot_discriminate_the_temperature_source() {
        let noise = ClimateNoise::new();
        let c = climate(0.0);
        for y in [-60, 0, 63, 80, 120, 200, 319] {
            assert!(
                !warm_enough_to_rain(&c, &noise, 55, y, -99, 63),
                "snowy_plains snows at y={y} regardless of the temperature source"
            );
        }
    }

    /// `canSurvive`'s three branches, in order, against the jar-measured facts.
    #[test]
    fn snow_survival_respects_both_tags_before_the_geometry() {
        let mut face_full = HashSet::new();
        // `ice` genuinely HAS a full up face (measured) — only the tag stops it.
        face_full.insert("minecraft:ice".to_owned());
        face_full.insert("minecraft:grass_block".to_owned());
        let support = SnowSupport {
            face_full_up: StatePredicate::new(face_full, HashMap::new()),
            cannot_support_snow_layer: ["minecraft:ice".to_owned()].into_iter().collect(),
            support_override_snow_layer: ["minecraft:mud".to_owned()].into_iter().collect(),
            ..SnowSupport::default()
        };
        assert!(support.snow_can_survive("minecraft:grass_block[snowy=false]"));
        assert!(
            !support.snow_can_survive("minecraft:ice"),
            "cannot_support_snow_layer must beat a full up face"
        );
        assert!(
            support.snow_can_survive("minecraft:mud"),
            "support_override_snow_layer must beat a missing up face"
        );
        assert!(!support.snow_can_survive("minecraft:short_grass"));
        // The `layers == 8` clause, which no geometry satisfies.
        assert!(support.snow_can_survive("minecraft:snow[layers=8]"));
        assert!(!support.snow_can_survive("minecraft:snow[layers=7]"));
    }

    #[test]
    fn temperature_modifier_parses_and_rejects_the_unknown() {
        assert_eq!(TemperatureModifier::parse(None), TemperatureModifier::None);
        assert_eq!(
            TemperatureModifier::parse(Some("none")),
            TemperatureModifier::None
        );
        assert_eq!(
            TemperatureModifier::parse(Some("frozen")),
            TemperatureModifier::Frozen
        );
        assert_eq!(
            TemperatureModifier::parse(Some("minecraft:frozen")),
            TemperatureModifier::Frozen
        );
    }

    #[test]
    #[should_panic(expected = "unsupported temperature_modifier")]
    fn an_unknown_temperature_modifier_is_a_hard_stop() {
        let _ = TemperatureModifier::parse(Some("scorching"));
    }

    /// The `FROZEN` modifier must actually change the answer somewhere, and it
    /// must clamp *upward* to `0.2` (warmer than the `0.15` threshold) inside an
    /// ice patch — `frozen_ocean`'s declared temperature is `0.0`, so the patches
    /// are the parts that DON'T freeze.
    #[test]
    fn frozen_modifier_produces_warm_patches_in_a_cold_ocean() {
        let noise = ClimateNoise::new();
        let plain = BiomeClimate {
            has_precipitation: true,
            temperature: 0.0,
            temperature_modifier: TemperatureModifier::None,
        };
        let frozen = BiomeClimate {
            temperature_modifier: TemperatureModifier::Frozen,
            ..plain
        };
        let mut patched = 0;
        let mut cold = 0;
        for x in 0..64 {
            for z in 0..64 {
                let t = height_adjusted_temperature(&frozen, &noise, x, 63, z, 63);
                assert_eq!(
                    height_adjusted_temperature(&plain, &noise, x, 63, z, 63).to_bits(),
                    0.0_f32.to_bits(),
                    "the unmodified biome is flat 0.0 at or below snow level"
                );
                if t == 0.2 {
                    patched += 1;
                } else {
                    assert_eq!(t.to_bits(), 0.0_f32.to_bits());
                    cold += 1;
                }
            }
        }
        assert!(
            patched > 0,
            "the FROZEN modifier never fired across 4096 positions, so its noise blend is \
             not being evaluated"
        );
        assert!(
            cold > 0,
            "the FROZEN modifier fired everywhere, so a frozen ocean would never freeze"
        );
        println!("FROZEN warm patches: {patched}/4096, cold: {cold}/4096");
    }
}
