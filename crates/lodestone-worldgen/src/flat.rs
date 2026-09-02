//! Superflat world generation — this change's third missing generator, after
//! `WorldType::{Amplified,LargeBiomes}` landed as pure wiring onto
//! [`crate::overworld::OverworldGenerator`].
//!
//! Deliberately its own tiny [`FlatLevelSource`], not a degenerate
//! `OverworldGenerator`: a flat world has no noise router, no biome climate
//! sampling and no carvers (vanilla's own flat-level-source apply-carvers is
//! an empty
//! override in vanilla), so composing it out of the
//! overworld pipeline would carry stages that do not apply to a flat world
//! and could silently produce non-flat terrain under a flat world's name —
//! the exact trap this change's own doc names ("a selection that silently
//! produces ordinary terrain under a preset's name is worse than an absent
//! option").
//!
//! # What is ported, and what is a documented gap
//!
//! **Ported**: the layer stack (`FlatLayerInfo`/`FlatLevelGeneratorSettings`,
//! same package as `FlatLevelSource`) and the block field it produces, which
//! is a flat world's entire defining behaviour — vanilla's own fill and
//! base-column queries place exactly this stack and nothing else generates the
//! raw terrain. A flat world is therefore **fully determined** by its
//! settings: no seed, no RNG, the same column at every `(x, z)`.
//!
//! **Not ported (documented, not silently dropped)**: vanilla's own
//! adjust-generation-settings routine splices `features`/`lakes` into the *biome's
//! own* decoration list (grass, flowers, lava lakes — real biome-decoration
//! feature placement) and its own create-state routine filters world structure sets by
//! `structure_overrides`. Both are decoration layered *on top of* the
//! deterministic block field, not the field itself, so a preset with
//! `"features": true` (e.g. `overworld`, `desert`, `tunnelers_dream`) still
//! generates its correct, exact layer stack here; it does not yet grow
//! anything on top of it or restrict which structures could place into it.
//! [`FlatLevelGeneratorSettings`] parses and carries `features`/`lakes`/
//! `structure_overrides` anyway, for exactly this reason — so the day
//! biome-decoration or structure placement is wired to a flat world, the
//! settings are already there rather than a second parse.
//!
//! # Deterministic by construction
//!
//! [`FlatLevelSource::column`] takes `cx`/`cz` — matching every other
//! generator's `column` signature so a `ChunkSource`-style wrapper can stay
//! uniform — but does not consult them: a flat world's column is the same
//! stack everywhere by definition, so there is nothing coordinate-dependent
//! to compute.

use serde_json::Value;

/// One layer of a flat preset's `layers` list, before height-expansion —
/// vanilla's own per-layer record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatLayer {
    /// Registry id, e.g. `"minecraft:dirt"` — `FlatLayerInfo`'s `block`.
    pub block: String,
    /// Row count, e.g. `2` — `FlatLayerInfo::getHeight`.
    pub height: u32,
}

/// [`FlatLevelGeneratorSettings::structureOverrides`]'s three JSON shapes,
/// parsed rather than collapsed to a plain `Vec` so "field absent" (→
/// vanilla's own fallback: every registered structure set) stays
/// distinguishable from "field present and empty" (→ no structure sets at
/// all). `the_void.json`'s `"structure_overrides": []` is the second, not the
/// first — collapsing both to an empty `Vec` would make a void world and an
/// (absent-field) world indistinguishable at exactly the case that matters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructureOverrides {
    /// The field was absent from the document. Vanilla's own flat-level-source
    /// create-state routine
    /// then falls back to every element of the structure-sets registry — every registered
    /// structure set — which this crate does not carry the registry to
    /// enumerate; a consumer wiring structures into a flat world must supply
    /// that full set itself when it sees this variant.
    Default,
    /// The field was present: either a single structure-set id (a bare
    /// string, e.g. `"minecraft:villages"`) or a list of them, possibly
    /// empty. Both JSON shapes normalise to this one `Vec`.
    Explicit(Vec<String>),
}

/// A flat preset's settings — vanilla's own flat-level-generator-settings
/// record,
/// parsed straight off the bundled JSON shape (`{biome, features, lakes,
/// layers, structure_overrides}`) shared by both a
/// `flat_level_generator_preset/<id>`'s `.settings` object and a
/// `world_preset`'s embedded `dimensions.<dim>.generator.settings` object —
/// the two document families that both need this parser (see
/// `lodestone_server::worldgen_data`'s callers).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FlatLevelGeneratorSettings {
    /// Fallback/decoration biome — `FlatLevelGeneratorSettings::getBiome`.
    pub biome: String,
    /// `FlatLevelGeneratorSettings::decoration` — see the module doc's "not
    /// ported" section.
    pub features: bool,
    /// `FlatLevelGeneratorSettings::addLakes` — see the module doc's "not
    /// ported" section.
    pub lakes: bool,
    /// `FlatLevelGeneratorSettings::layersInfo`, in bottom-to-top order.
    pub layers: Vec<FlatLayer>,
    /// `FlatLevelGeneratorSettings::structureOverrides`.
    pub structure_overrides: StructureOverrides,
}

impl FlatLevelGeneratorSettings {
    /// Parses one `settings` document (the object at either a
    /// `flat_level_generator_preset/<id>.settings` key or a `world_preset`'s
    /// `generator.settings` key — both share this shape byte-for-byte).
    ///
    /// Missing `biome`/`features`/`lakes` take vanilla's own codec defaults
    /// (`minecraft:plains`, `false`, `false` —
    /// vanilla's own settings codec's own lenient-optional and
    /// always-present-optional field wrappers); a missing or malformed `layers` entry
    /// is dropped rather than panicking, matching this crate's
    /// `Resolver`-adjacent convention of "no data" over an abort — a caller
    /// that needs to know the parse was incomplete has
    /// [`Self::total_height`] and its own document to cross-check against.
    #[must_use]
    pub fn from_json(v: &Value) -> Self {
        let biome = v["biome"]
            .as_str()
            .unwrap_or("minecraft:plains")
            .to_string();
        let features = v["features"].as_bool().unwrap_or(false);
        let lakes = v["lakes"].as_bool().unwrap_or(false);
        let layers = v["layers"]
            .as_array()
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(|entry| {
                        let block = entry["block"].as_str()?.to_string();
                        let height = entry["height"].as_u64()?;
                        Some(FlatLayer {
                            block,
                            height: height as u32,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        let structure_overrides = match &v["structure_overrides"] {
            Value::String(id) => StructureOverrides::Explicit(vec![id.clone()]),
            Value::Array(entries) => StructureOverrides::Explicit(
                entries
                    .iter()
                    .filter_map(|e| e.as_str().map(str::to_string))
                    .collect(),
            ),
            // `Value::Null` covers both "key absent" (`serde_json::Value`'s
            // index returns `Null` for a missing key) and an explicit JSON
            // `null`, which the bundled data never writes — both mean
            // vanilla's own fallback.
            _ => StructureOverrides::Default,
        };
        Self {
            biome,
            features,
            lakes,
            layers,
            structure_overrides,
        }
    }

    /// Sum of every layer's height — `FlatLevelGeneratorSettings
    /// .validateHeight`'s left-hand side, checked there against
    /// `DimensionType.Y_SIZE`. Exposed so a caller can run the same check
    /// against whichever dimension height it is placing into, rather than
    /// this crate assuming one.
    #[must_use]
    pub fn total_height(&self) -> u32 {
        self.layers.iter().map(|l| l.height).sum()
    }
}

/// The canonical default-state string for a layer block —
/// `Block::defaultBlockState()`'s string form, for the finite set of blocks
/// the bundled flat presets actually name.
///
/// This crate has no `lodestone-data` dependency (see `Cargo.toml`), so
/// unlike `lodestone_server::worldgen_data::canonical_state` this cannot
/// resolve an arbitrary block id's default properties from the real
/// registry — it only needs to be right for the ids the bundled data names,
/// and the two with a non-empty default state already have their canonical
/// form fixed elsewhere in this crate as plain string literals
/// (`crate::carver::WATER`, `crate::feature::top_layer::SNOW_LAYER`, and the
/// `[snowy=false]` form `crate::feature::top_layer`'s own tests hardcode for
/// grass_block); reused here by literal rather than re-derived, so the two
/// cannot drift apart. Every other id in the bundled set (bedrock, dirt,
/// stone, sandstone, sand, deepslate, gravel, cobblestone, end_stone,
/// basalt, air, barrier) has no properties at all, so its bare id **is**
/// its default state.
fn canonical_default_state(block: &str) -> String {
    match block {
        "minecraft:grass_block" => "minecraft:grass_block[snowy=false]".to_string(),
        "minecraft:water" => "minecraft:water[level=0]".to_string(),
        "minecraft:snow" => crate::feature::top_layer::SNOW_LAYER.to_string(),
        other => other.to_string(),
    }
}

/// One flat world's generated column — the same for every `(x, z)`, so this
/// carries one Y-indexed row list rather than a 16×height×16 grid.
#[derive(Debug, Clone)]
pub struct FlatColumn {
    min_y: i32,
    height: i32,
    biome: String,
    /// Canonical state per Y row, index 0 == `min_y`. May be shorter than
    /// `height` — a preset's `layers` list is not padded, exactly like
    /// vanilla's own `FlatLevelGeneratorSettings::layers`; a row past the end
    /// is air.
    rows: std::sync::Arc<[String]>,
}

impl FlatColumn {
    /// World Y of the lowest row this column's `rows` covers (not
    /// necessarily where the layer stack ends — see the field's own doc).
    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    /// The dimension's full column height (not the layer stack's height).
    #[must_use]
    pub fn height(&self) -> i32 {
        self.height
    }

    /// The preset's biome id, uniform across the whole column — a flat world
    /// has one fixed biome by construction (`FixedBiomeSource`, wrapped by
    /// `FlatLevelSource`'s constructor around `settings.getBiome()`).
    #[must_use]
    pub fn biome(&self) -> &str {
        &self.biome
    }

    /// Canonical state at world `y`. `"minecraft:air"` above the layer stack
    /// or below `min_y` — mirrors vanilla's own flat-level-source
    /// base-column query's `null`
    /// → default-air-state substitution.
    #[must_use]
    pub fn block_state(&self, y: i32) -> &str {
        let row = y - self.min_y;
        if row < 0 {
            return "minecraft:air";
        }
        self.rows
            .get(row as usize)
            .map(String::as_str)
            .unwrap_or("minecraft:air")
    }

    /// Highest world Y whose block is not air, or `min_y - 1` for an
    /// all-air column (mirrors [`crate::overworld::GeneratedColumn::top_non_air_y`]'s
    /// contract, so a caller comparing the two generators' output can share
    /// one code path).
    #[must_use]
    pub fn top_non_air_y(&self) -> i32 {
        for (row, state) in self.rows.iter().enumerate().rev() {
            if state != "minecraft:air" {
                return self.min_y + row as i32;
            }
        }
        self.min_y - 1
    }

    /// The expanded per-row states, bottom to top — for a caller asserting
    /// the exact layer stack (a flat world is fully determined, so there is
    /// no excuse for a vague assertion — CLAUDE.md).
    #[must_use]
    pub fn rows(&self) -> &[String] {
        &self.rows
    }
}

/// A superflat generator built from one [`FlatLevelGeneratorSettings`] plus
/// the dimension's vertical bounds — vanilla's own flat-level-source type.
///
/// Unlike [`crate::overworld::OverworldGenerator`] this needs no seed and no
/// [`crate::density::Resolver`]: nothing about a flat world's raw terrain is
/// randomised or density-function-driven.
#[derive(Debug, Clone)]
pub struct FlatLevelSource {
    settings: FlatLevelGeneratorSettings,
    /// [`Self::settings`]'s layers expanded to one canonical state per row —
    /// computed once at construction (`FlatLevelGeneratorSettings
    /// ::updateLayers`'s equivalent) rather than per [`Self::column`] call,
    /// since every column is identical.
    rows: std::sync::Arc<[String]>,
    min_y: i32,
    height: i32,
}

impl FlatLevelSource {
    /// Builds the generator. `min_y`/`height` are the *dimension's* vertical
    /// bounds (e.g. -64/384 for the overworld), not a property of the
    /// settings — vanilla's own flat-level-source min-Y/gen-depth queries
    /// hardcode 0/384 as an unrelated `ChunkGenerator` abstract-method
    /// answer; the actual placement in vanilla's own fill and base-column
    /// queries is
    /// always relative to the height accessor (dimension) it is handed, which
    /// is why this constructor takes the bounds explicitly rather than
    /// guessing them from the settings.
    #[must_use]
    pub fn new(settings: FlatLevelGeneratorSettings, min_y: i32, height: i32) -> Self {
        let mut rows = Vec::with_capacity(settings.total_height() as usize);
        for layer in &settings.layers {
            let state = canonical_default_state(&layer.block);
            for _ in 0..layer.height {
                rows.push(state.clone());
            }
        }
        Self {
            settings,
            rows: rows.into(),
            min_y,
            height,
        }
    }

    #[must_use]
    pub fn settings(&self) -> &FlatLevelGeneratorSettings {
        &self.settings
    }

    #[must_use]
    pub fn min_y(&self) -> i32 {
        self.min_y
    }

    #[must_use]
    pub fn height(&self) -> i32 {
        self.height
    }

    /// Generates the column at chunk coordinates `(cx, cz)` — accepted but
    /// unused, see the module doc's "deterministic by construction" section.
    #[must_use]
    pub fn column(&self, _cx: i32, _cz: i32) -> FlatColumn {
        FlatColumn {
            min_y: self.min_y,
            height: self.height,
            biome: self.settings.biome.clone(),
            rows: std::sync::Arc::clone(&self.rows),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `classic_flat.json`'s `settings` object, transcribed verbatim from
    /// `crates/lodestone-server/assets/worldgen/flat_level_generator_preset/classic_flat.json`
    /// — vanilla's default flat layer stack (bedrock/dirt/grass_block).
    fn classic_flat_settings_json() -> Value {
        serde_json::json!({
            "biome": "minecraft:plains",
            "features": false,
            "lakes": false,
            "layers": [
                { "block": "minecraft:bedrock", "height": 1 },
                { "block": "minecraft:dirt", "height": 2 },
                { "block": "minecraft:grass_block", "height": 1 },
            ],
            "structure_overrides": "minecraft:villages",
        })
    }

    /// `the_void.json`'s `settings` object — the discriminating case for
    /// [`StructureOverrides::Explicit`] vs [`StructureOverrides::Default`]:
    /// an explicit empty list, not an absent field.
    fn the_void_settings_json() -> Value {
        serde_json::json!({
            "biome": "minecraft:the_void",
            "features": true,
            "lakes": false,
            "layers": [
                { "block": "minecraft:air", "height": 1 },
            ],
            "structure_overrides": [],
        })
    }

    /// `water_world.json`'s `settings` object — exercises the water-layer
    /// canonical-state exception and a taller, multi-block stack.
    fn water_world_settings_json() -> Value {
        serde_json::json!({
            "biome": "minecraft:deep_ocean",
            "features": false,
            "lakes": false,
            "layers": [
                { "block": "minecraft:bedrock", "height": 1 },
                { "block": "minecraft:deepslate", "height": 64 },
                { "block": "minecraft:stone", "height": 5 },
                { "block": "minecraft:dirt", "height": 5 },
                { "block": "minecraft:gravel", "height": 5 },
                { "block": "minecraft:water", "height": 90 },
            ],
            "structure_overrides": [
                "minecraft:ocean_ruins",
                "minecraft:shipwrecks",
                "minecraft:ocean_monuments",
            ],
        })
    }

    #[test]
    fn parses_classic_flat_exactly() {
        let settings = FlatLevelGeneratorSettings::from_json(&classic_flat_settings_json());
        assert_eq!(settings.biome, "minecraft:plains");
        assert!(!settings.features);
        assert!(!settings.lakes);
        assert_eq!(
            settings.layers,
            vec![
                FlatLayer {
                    block: "minecraft:bedrock".to_string(),
                    height: 1
                },
                FlatLayer {
                    block: "minecraft:dirt".to_string(),
                    height: 2
                },
                FlatLayer {
                    block: "minecraft:grass_block".to_string(),
                    height: 1
                },
            ]
        );
        assert_eq!(
            settings.structure_overrides,
            StructureOverrides::Explicit(vec!["minecraft:villages".to_string()])
        );
        assert_eq!(settings.total_height(), 4);
    }

    /// A bare string `structure_overrides` (one id, no array) must parse to a
    /// one-element `Explicit`, not be dropped or treated as `Default` — the
    /// shape `classic_flat`/`bottomless_pit` actually use.
    #[test]
    fn structure_overrides_bare_string_is_one_element_explicit() {
        let settings = FlatLevelGeneratorSettings::from_json(&classic_flat_settings_json());
        match settings.structure_overrides {
            StructureOverrides::Explicit(ids) => assert_eq!(ids, vec!["minecraft:villages"]),
            StructureOverrides::Default => panic!("a bare string must not parse as Default"),
        }
    }

    /// The discriminating case named in [`StructureOverrides`]'s own doc: an
    /// explicit empty array must stay `Explicit(vec![])`, not collapse to
    /// `Default` (which would silently hand a void world every structure set
    /// vanilla explicitly withholds from it).
    #[test]
    fn structure_overrides_empty_array_is_explicit_not_default() {
        let settings = FlatLevelGeneratorSettings::from_json(&the_void_settings_json());
        assert_eq!(
            settings.structure_overrides,
            StructureOverrides::Explicit(Vec::new()),
            "an explicit `[]` must not read the same as an absent field"
        );
    }

    /// A document with no `structure_overrides` key at all parses to
    /// `Default` — none of the 9 bundled presets actually omit it, so this is
    /// exercised directly against a hand-built minimal document rather than
    /// bundled data.
    #[test]
    fn structure_overrides_absent_field_is_default() {
        let doc = serde_json::json!({
            "biome": "minecraft:plains",
            "layers": [{ "block": "minecraft:stone", "height": 1 }],
        });
        let settings = FlatLevelGeneratorSettings::from_json(&doc);
        assert_eq!(settings.structure_overrides, StructureOverrides::Default);
        // Also exercises the `features`/`lakes` defaults, both absent here.
        assert!(!settings.features);
        assert!(!settings.lakes);
    }

    /// The load-bearing assertion for the whole module: given
    /// `classic_flat`'s settings and the overworld's real vertical bounds
    /// (-64/384), the generated column must be the *exact* predicted layer
    /// stack — bedrock at -64, dirt at -63..-61, grass_block at -60, air
    /// above and below — collected into one mismatch list and asserted once
    /// (CLAUDE.md: "collect mismatches and assert on the collection").
    #[test]
    fn classic_flat_column_matches_the_exact_predicted_layer_stack() {
        let settings = FlatLevelGeneratorSettings::from_json(&classic_flat_settings_json());
        let generator = FlatLevelSource::new(settings, -64, 384);
        let column = generator.column(7, -3);

        let mut expected: Vec<(i32, &str)> = vec![
            (-64, "minecraft:bedrock"),
            (-63, "minecraft:dirt"),
            (-62, "minecraft:dirt"),
            (-61, "minecraft:grass_block[snowy=false]"),
        ];
        // A wide bracket of "must be air" checks: one row below the stack (a
        // negative-index probe) and several rows above it, including the
        // dimension's own top row.
        for y in [-65, -60, -59, 0, 64, 130, 319] {
            expected.push((y, "minecraft:air"));
        }

        let mismatches: Vec<String> = expected
            .iter()
            .filter_map(|&(y, want)| {
                let got = column.block_state(y);
                (got != want).then(|| format!("y={y}: expected {want:?}, got {got:?}"))
            })
            .collect();
        assert!(
            mismatches.is_empty(),
            "classic_flat column mismatches at exact predicted rows: {mismatches:#?}"
        );

        assert_eq!(column.top_non_air_y(), -61, "grass_block is the topmost non-air row");
        assert_eq!(column.biome(), "minecraft:plains");
        assert_eq!(
            column.rows(),
            &[
                "minecraft:bedrock".to_string(),
                "minecraft:dirt".to_string(),
                "minecraft:dirt".to_string(),
                "minecraft:grass_block[snowy=false]".to_string(),
            ]
        );
    }

    /// `the_void`'s single `air` layer must generate a column that is air at
    /// every row this test samples — the layer is present (height 1) but
    /// canonicalises to the same string as "no layer at all", so this also
    /// guards against an off-by-one that would place a phantom block at
    /// `min_y`.
    #[test]
    fn the_void_column_is_air_everywhere_sampled() {
        let settings = FlatLevelGeneratorSettings::from_json(&the_void_settings_json());
        let generator = FlatLevelSource::new(settings, -64, 384);
        let column = generator.column(0, 0);

        let mismatches: Vec<i32> = [-64, -63, -1, 0, 63, 100, 319]
            .into_iter()
            .filter(|&y| column.block_state(y) != "minecraft:air")
            .collect();
        assert!(mismatches.is_empty(), "the_void must be air at rows {mismatches:?}");
        assert_eq!(column.top_non_air_y(), -65, "an all-air column reports min_y - 1");
    }

    /// `water_world`'s stack is 170 rows deep and exercises the
    /// `[level=0]` water canonicalisation plus a non-trivial multi-block
    /// interior — a second, independent fixture from `classic_flat` so a
    /// coincidence in one preset's numbers cannot hide a real bug (CLAUDE.md's
    /// "an input where both hypotheses coincide is not a test").
    #[test]
    fn water_world_column_matches_the_exact_predicted_layer_stack() {
        let settings = FlatLevelGeneratorSettings::from_json(&water_world_settings_json());
        assert_eq!(settings.total_height(), 170);
        let generator = FlatLevelSource::new(settings, -64, 384);
        let column = generator.column(1000, -1000);

        let mut expected: Vec<(i32, &str)> = vec![(-64, "minecraft:bedrock")];
        for y in -63..=0 {
            expected.push((y, "minecraft:deepslate"));
        }
        for y in 1..=5 {
            expected.push((y, "minecraft:stone"));
        }
        for y in 6..=10 {
            expected.push((y, "minecraft:dirt"));
        }
        for y in 11..=15 {
            expected.push((y, "minecraft:gravel"));
        }
        // Sample rather than enumerate all 90 water rows: first, middle, last.
        for y in [16, 60, 105] {
            expected.push((y, "minecraft:water[level=0]"));
        }
        expected.push((106, "minecraft:air")); // one past the 170-row stack

        let mismatches: Vec<String> = expected
            .iter()
            .filter_map(|&(y, want)| {
                let got = column.block_state(y);
                (got != want).then(|| format!("y={y}: expected {want:?}, got {got:?}"))
            })
            .collect();
        assert!(
            mismatches.is_empty(),
            "water_world column mismatches: {mismatches:#?}"
        );
        assert_eq!(column.top_non_air_y(), 105, "the last water row is the topmost non-air row");
    }

    /// A flat world's column must be identical at every `(cx, cz)` — the
    /// module doc's "deterministic by construction" claim, checked rather
    /// than assumed.
    #[test]
    fn column_is_identical_at_every_chunk_coordinate() {
        let settings = FlatLevelGeneratorSettings::from_json(&classic_flat_settings_json());
        let generator = FlatLevelSource::new(settings, -64, 384);
        let a = generator.column(0, 0);
        let b = generator.column(-500, 12345);
        assert_eq!(a.rows(), b.rows());
        assert_eq!(a.biome(), b.biome());
    }

    #[test]
    fn snow_layer_canonicalises_through_the_shared_top_layer_constant() {
        assert_eq!(
            canonical_default_state("minecraft:snow"),
            crate::feature::top_layer::SNOW_LAYER
        );
    }

    #[test]
    fn blocks_with_no_properties_canonicalise_to_their_bare_id() {
        for id in [
            "minecraft:bedrock",
            "minecraft:stone",
            "minecraft:sandstone",
            "minecraft:sand",
            "minecraft:cobblestone",
            "minecraft:end_stone",
            "minecraft:basalt",
            "minecraft:barrier",
            "minecraft:air",
        ] {
            assert_eq!(canonical_default_state(id), id);
        }
    }
}
