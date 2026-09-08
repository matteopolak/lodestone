//! Strict serde models for the structure documents consumed by the generator.
//!
//! The resolver still exposes JSON values because it serves several registry
//! families.  These models are the narrow boundary for structure data: known
//! records are typed, discriminators are closed, and only genuinely polymorphic
//! registry payloads remain JSON values.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Weighted<T> {
    pub data: T,
    #[serde(default = "default_weight")]
    pub weight: i32,
}

fn default_weight() -> i32 {
    1
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum StringOrStrings {
    String(String),
    Strings(Vec<String>),
}

impl StringOrStrings {
    pub(crate) fn into_vec(self) -> Vec<String> {
        match self {
            Self::String(value) => vec![value],
            Self::Strings(values) => values,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ExclusionZone {
    pub other_set: String,
    pub chunk_count: i32,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum SpreadType {
    #[serde(alias = "minecraft:triangular")]
    Triangular,
    #[serde(alias = "minecraft:linear")]
    Linear,
}

impl Default for SpreadType {
    fn default() -> Self {
        Self::Linear
    }
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum FrequencyReduction {
    #[serde(rename = "default")]
    Default,
    #[serde(rename = "legacy_type_1")]
    LegacyType1,
    #[serde(rename = "legacy_type_2")]
    LegacyType2,
    #[serde(rename = "legacy_type_3")]
    LegacyType3,
}

impl Default for FrequencyReduction {
    fn default() -> Self {
        Self::Default
    }
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum PlacementDocument {
    #[serde(rename = "minecraft:random_spread", alias = "random_spread")]
    RandomSpread {
        #[serde(default)]
        locate_offset: Option<[i32; 3]>,
        #[serde(default)]
        frequency_reduction_method: FrequencyReduction,
        #[serde(default = "default_frequency")]
        frequency: f32,
        #[serde(default)]
        salt: i32,
        #[serde(default)]
        exclusion_zone: Option<ExclusionZone>,
        spacing: i32,
        separation: i32,
        #[serde(default)]
        spread_type: SpreadType,
    },
    #[serde(rename = "minecraft:concentric_rings", alias = "concentric_rings")]
    ConcentricRings {
        #[serde(default)]
        locate_offset: Option<[i32; 3]>,
        #[serde(default)]
        frequency_reduction_method: FrequencyReduction,
        #[serde(default = "default_frequency")]
        frequency: f32,
        #[serde(default)]
        salt: i32,
        #[serde(default)]
        exclusion_zone: Option<ExclusionZone>,
        distance: i32,
        #[serde(default = "default_spread")]
        spread: i32,
        #[serde(default = "default_count")]
        count: i32,
        preferred_biomes: StringOrStrings,
    },
}

fn default_frequency() -> f32 {
    1.0
}

fn default_spread() -> i32 {
    1
}

fn default_count() -> i32 {
    1
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub(crate) enum PoolAliasDocument {
    #[serde(rename = "minecraft:direct", alias = "direct")]
    Direct { alias: String, target: String },
    #[serde(rename = "minecraft:random", alias = "random")]
    Random {
        alias: String,
        targets: Vec<Weighted<String>>,
    },
    #[serde(rename = "minecraft:random_group", alias = "random_group")]
    RandomGroup {
        groups: Vec<Weighted<Vec<PoolAliasDocument>>>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum HeightDocument {
    Uniform(UniformHeightDocument),
    Constant(ConstantHeightDocument),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct UniformHeightDocument {
    #[serde(rename = "type")]
    pub kind: HeightKind,
    pub min_inclusive: AnchorDocument,
    pub max_inclusive: AnchorDocument,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ConstantHeightDocument {
    #[serde(default, rename = "type")]
    pub kind: Option<HeightKind>,
    #[serde(default)]
    pub value: Option<AnchorDocument>,
    #[serde(default)]
    pub absolute: Option<i32>,
    #[serde(default)]
    pub above_bottom: Option<i32>,
    #[serde(default)]
    pub below_top: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub(crate) enum HeightKind {
    #[serde(rename = "minecraft:constant", alias = "constant")]
    Constant,
    #[serde(rename = "minecraft:uniform", alias = "uniform")]
    Uniform,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AnchorDocument {
    #[serde(default)]
    pub absolute: Option<i32>,
    #[serde(default)]
    pub above_bottom: Option<i32>,
    #[serde(default)]
    pub below_top: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum IntOrObject {
    Int(i32),
    Object {
        #[serde(default)]
        horizontal: Option<i32>,
        #[serde(default)]
        vertical: Option<i32>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum PaddingDocument {
    Int(i32),
    Object {
        #[serde(default)]
        bottom: i32,
        #[serde(default)]
        top: i32,
    },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct JigsawDocument {
    #[serde(rename = "type")]
    pub kind: StructureType,
    #[serde(rename = "biomes")]
    pub _biomes: StringOrStrings,
    #[serde(default)]
    #[serde(rename = "spawn_overrides")]
    pub _spawn_overrides: serde_json::Value,
    #[serde(rename = "step")]
    pub _step: String,
    #[serde(default)]
    #[serde(rename = "terrain_adaptation")]
    pub _terrain_adaptation: Option<String>,
    pub start_pool: String,
    #[serde(default)]
    pub start_jigsaw_name: Option<String>,
    #[serde(default)]
    pub size: i32,
    pub start_height: HeightDocument,
    #[serde(default)]
    pub use_expansion_hack: bool,
    #[serde(default)]
    pub project_start_to_heightmap: Option<HeightmapDocument>,
    #[serde(default)]
    pub max_distance_from_center: Option<IntOrObject>,
    #[serde(default)]
    pub liquid_settings: Option<LiquidSettings>,
    #[serde(default)]
    pub dimension_padding: Option<PaddingDocument>,
    #[serde(default)]
    pub pool_aliases: Vec<PoolAliasDocument>,
}

#[derive(Debug, Deserialize)]
pub(crate) enum StructureType {
    #[serde(rename = "minecraft:jigsaw", alias = "jigsaw")]
    Jigsaw,
}

#[derive(Debug, Deserialize)]
pub(crate) enum HeightmapDocument {
    #[serde(rename = "WORLD_SURFACE_WG")]
    WorldSurfaceWg,
    #[serde(rename = "OCEAN_FLOOR_WG")]
    OceanFloorWg,
}

#[derive(Debug, Deserialize)]
pub(crate) enum LiquidSettings {
    #[serde(rename = "apply_waterlogging")]
    ApplyWaterlogging,
    #[serde(rename = "ignore_waterlogging")]
    IgnoreWaterlogging,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TemplatePoolDocument {
    #[serde(default = "default_fallback")]
    pub fallback: String,
    #[serde(default)]
    pub elements: Vec<PoolEntryDocument>,
}

fn default_fallback() -> String {
    "minecraft:empty".to_string()
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct PoolEntryDocument {
    #[serde(default = "default_weight")]
    pub weight: i32,
    pub element: PoolElementDocument,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "element_type", deny_unknown_fields)]
pub(crate) enum PoolElementDocument {
    #[serde(rename = "minecraft:empty_pool_element")]
    Empty,
    #[serde(rename = "minecraft:feature_pool_element")]
    Feature {
        projection: ProjectionDocument,
        feature: FeatureDocument,
    },
    #[serde(rename = "minecraft:list_pool_element")]
    List {
        projection: ProjectionDocument,
        elements: Vec<PoolElementDocument>,
    },
    #[serde(rename = "minecraft:single_pool_element")]
    Single {
        projection: ProjectionDocument,
        location: String,
        processors: ProcessorDocument,
        #[serde(default)]
        override_liquid_settings: Option<LiquidSettings>,
    },
    #[serde(rename = "minecraft:legacy_single_pool_element")]
    LegacySingle {
        projection: ProjectionDocument,
        location: String,
        processors: ProcessorDocument,
        #[serde(default)]
        override_liquid_settings: Option<LiquidSettings>,
    },
}

#[derive(Debug, Deserialize)]
pub(crate) enum ProjectionDocument {
    #[serde(rename = "rigid")]
    Rigid,
    #[serde(rename = "terrain_matching")]
    TerrainMatching,
}

/// A processor list is either a registry id or an inline processor document.
/// Processor variants are owned by the processor subsystem and intentionally
/// remain an opaque payload at this boundary.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ProcessorDocument {
    Reference(String),
    Inline(serde_json::Value),
}

/// A placed feature is a registry id or an inline registry body.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum FeatureDocument {
    Reference(String),
    Inline(serde_json::Value),
}

#[cfg(test)]
mod tests {
    use super::{JigsawDocument, PlacementDocument, PoolElementDocument};

    #[test]
    fn placement_rejects_unknown_discriminator_and_fields() {
        let error = serde_json::from_str::<PlacementDocument>(
            r#"{"type":"minecraft:future","spacing":1,"separation":0}"#,
        )
        .expect_err("an unknown placement type must not become a default");
        assert!(error.to_string().contains("unknown variant"), "{error}");

        let error = serde_json::from_str::<PlacementDocument>(
            r#"{"type":"minecraft:random_spread","spacing":1,"separation":0,"mystery":true}"#,
        )
        .expect_err("placement records must reject unknown fields");
        assert!(error.to_string().contains("unknown field"), "{error}");
    }

    #[test]
    fn jigsaw_rejects_unknown_fields_and_implicit_numeric_coercion() {
        let error = serde_json::from_str::<JigsawDocument>(
            r##"{
                "type":"minecraft:jigsaw", "biomes":"#x", "spawn_overrides":{},
                "step":"surface_structures", "start_pool":"minecraft:x",
                "start_height":{"absolute":0}, "size":6, "mystery":true
            }"##,
        )
        .expect_err("jigsaw records must reject unknown fields");
        assert!(error.to_string().contains("unknown field"), "{error}");

        let error = serde_json::from_str::<JigsawDocument>(
            r##"{
                "type":"minecraft:jigsaw", "biomes":"#x", "spawn_overrides":{},
                "step":"surface_structures", "start_pool":"minecraft:x",
                "start_height":{"absolute":"0"}, "size":6
            }"##,
        )
        .expect_err("numeric strings must not be coerced");
        assert!(
            error.to_string().contains("invalid type")
                || error.to_string().contains("untagged enum"),
            "{error}"
        );
    }

    #[test]
    fn pool_element_rejects_unknown_discriminator() {
        let error = serde_json::from_str::<PoolElementDocument>(
            r#"{"element_type":"minecraft:future_pool_element"}"#,
        )
        .expect_err("an unknown pool element must not be silently empty");
        assert!(error.to_string().contains("unknown variant"), "{error}");
    }
}
