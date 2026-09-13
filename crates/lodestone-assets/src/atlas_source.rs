//! Atlas **source lists** — `assets/<ns>/atlases/<id>.json`.
//!
//! Vanilla does not stitch a texture atlas from an implicit directory scan.
//! Each atlas (`blocks`, `chests`, `shulker_boxes`, `banner_patterns`,
//! `armor_trims`, …) is described by a JSON *source list* that enumerates
//! exactly which textures belong on the sheet and under what sprite id. This is
//! the authority for "what goes on the block-entity atlases" that the renderer
//! needs — chests, signs, beds, banners and shulker boxes are all `directory`
//! sources here.
//!
//! This module is the **data** half: it parses the source list and resolves the
//! two dominant source kinds (`directory`, `single`) against a
//! [`ResourceManager`] into concrete `(sprite id, texture path)` pairs. The
//! `paletted_permutations` kind (armor trims, banner-pattern recolours) is
//! parsed into a typed variant and its derived sprite ids are enumerated, but
//! the actual palette-swap pixel generation is a bake step and is intentionally
//! left to the atlas-baking layer — this crate only reports what it will
//! produce.

use std::{collections::BTreeMap, fmt};

use serde::{
    Deserialize, Serialize,
    de::{self, IgnoredAny, MapAccess, Visitor},
    ser::SerializeStruct,
};

use crate::{ResourceLocation, ResourceManager, error::AtlasSourceError};

/// The closed top-level shape of an atlas source-list document.
///
/// `sources` is optional in the transport DTO so a missing field can retain
/// the public [`AtlasSourceError::MissingSources`] diagnostic. The successful
/// path always lowers a present list into [`AtlasDefinition`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AtlasDefinitionDocument {
    #[serde(default)]
    sources: Option<Vec<AtlasSourceDocument>>,
}

/// The closed shape of a directory source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DirectorySourceDocument {
    source: String,
    prefix: String,
}

/// The closed shape of a single-texture source.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SingleSourceDocument {
    resource: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sprite: Option<String>,
}

/// The closed shape of a paletted-permutations source.
///
/// `permutations` is intentionally a dynamic map: its keys are pack-authored
/// variant names, while every value is still constrained to a resource string.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PalettedPermutationsDocument {
    textures: Vec<String>,
    palette_key: String,
    permutations: BTreeMap<String, String>,
    #[serde(default = "default_separator")]
    separator: String,
}

fn default_separator() -> String {
    "_".to_owned()
}

/// Typed transport form for one atlas source.
///
/// The custom deserializer has one intentional escape hatch: an unknown
/// `type` is preserved as [`AtlasSource::Unknown`]. This is part of the asset
/// boundary contract because newer packs may add source kinds before this
/// crate learns how to resolve them.
#[derive(Debug, Clone, PartialEq)]
enum AtlasSourceDocument {
    Directory(DirectorySourceDocument),
    Single(SingleSourceDocument),
    PalettedPermutations(PalettedPermutationsDocument),
    Unknown { kind: String },
}

impl Serialize for AtlasSourceDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Directory(document) => {
                let mut state = serializer.serialize_struct("DirectorySourceDocument", 3)?;
                state.serialize_field("type", "minecraft:directory")?;
                state.serialize_field("source", &document.source)?;
                state.serialize_field("prefix", &document.prefix)?;
                state.end()
            }
            Self::Single(document) => {
                let field_count = if document.sprite.is_some() { 3 } else { 2 };
                let mut state = serializer.serialize_struct("SingleSourceDocument", field_count)?;
                state.serialize_field("type", "minecraft:single")?;
                state.serialize_field("resource", &document.resource)?;
                if let Some(sprite) = &document.sprite {
                    state.serialize_field("sprite", sprite)?;
                }
                state.end()
            }
            Self::PalettedPermutations(document) => {
                let mut state = serializer.serialize_struct("PalettedPermutationsDocument", 5)?;
                state.serialize_field("type", "minecraft:paletted_permutations")?;
                state.serialize_field("textures", &document.textures)?;
                state.serialize_field("palette_key", &document.palette_key)?;
                state.serialize_field("permutations", &document.permutations)?;
                state.serialize_field("separator", &document.separator)?;
                state.end()
            }
            Self::Unknown { kind } => {
                let mut state = serializer.serialize_struct("UnknownAtlasSourceDocument", 1)?;
                state.serialize_field("type", kind)?;
                state.end()
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceField {
    Type,
    Source,
    Prefix,
    Resource,
    Sprite,
    Textures,
    PaletteKey,
    Permutations,
    Separator,
}

impl SourceField {
    const ALL: [Self; 9] = [
        Self::Type,
        Self::Source,
        Self::Prefix,
        Self::Resource,
        Self::Sprite,
        Self::Textures,
        Self::PaletteKey,
        Self::Permutations,
        Self::Separator,
    ];

    fn index(self) -> usize {
        match self {
            Self::Type => 0,
            Self::Source => 1,
            Self::Prefix => 2,
            Self::Resource => 3,
            Self::Sprite => 4,
            Self::Textures => 5,
            Self::PaletteKey => 6,
            Self::Permutations => 7,
            Self::Separator => 8,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Type => "type",
            Self::Source => "source",
            Self::Prefix => "prefix",
            Self::Resource => "resource",
            Self::Sprite => "sprite",
            Self::Textures => "textures",
            Self::PaletteKey => "palette_key",
            Self::Permutations => "permutations",
            Self::Separator => "separator",
        }
    }
}

struct AtlasSourceFields {
    kind: Option<String>,
    source: Option<String>,
    prefix: Option<String>,
    resource: Option<String>,
    sprite: Option<String>,
    textures: Option<Vec<String>>,
    palette_key: Option<String>,
    permutations: Option<BTreeMap<String, String>>,
    separator: Option<String>,
    seen: [bool; SourceField::ALL.len()],
    unknown: Vec<String>,
}

impl AtlasSourceFields {
    fn new() -> Self {
        Self {
            kind: None,
            source: None,
            prefix: None,
            resource: None,
            sprite: None,
            textures: None,
            palette_key: None,
            permutations: None,
            separator: None,
            seen: [false; SourceField::ALL.len()],
            unknown: Vec::new(),
        }
    }

    fn mark(&mut self, field: SourceField) {
        self.seen[field.index()] = true;
    }

    fn validate<E: de::Error>(&self, kind: &str, allowed: &[SourceField]) -> Result<(), E> {
        if let Some(field) = self.unknown.first() {
            return Err(E::custom(format!(
                "unknown field {field:?} for atlas source type {kind:?}"
            )));
        }
        for field in SourceField::ALL {
            if self.seen[field.index()] && !allowed.contains(&field) {
                return Err(E::custom(format!(
                    "field {:?} is not valid for atlas source type {kind:?}",
                    field.name()
                )));
            }
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for AtlasSourceDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct AtlasSourceVisitor;

        impl<'de> Visitor<'de> for AtlasSourceVisitor {
            type Value = AtlasSourceDocument;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an atlas source object")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut fields = AtlasSourceFields::new();
                while let Some(name) = map.next_key::<String>()? {
                    match name.as_str() {
                        "type" => {
                            fields.mark(SourceField::Type);
                            fields.kind = Some(map.next_value()?);
                        }
                        "source" => {
                            fields.mark(SourceField::Source);
                            fields.source = map.next_value()?;
                        }
                        "prefix" => {
                            fields.mark(SourceField::Prefix);
                            fields.prefix = map.next_value()?;
                        }
                        "resource" => {
                            fields.mark(SourceField::Resource);
                            fields.resource = map.next_value()?;
                        }
                        "sprite" => {
                            fields.mark(SourceField::Sprite);
                            fields.sprite = map.next_value()?;
                        }
                        "textures" => {
                            fields.mark(SourceField::Textures);
                            fields.textures = map.next_value()?;
                        }
                        "palette_key" => {
                            fields.mark(SourceField::PaletteKey);
                            fields.palette_key = map.next_value()?;
                        }
                        "permutations" => {
                            fields.mark(SourceField::Permutations);
                            fields.permutations = map.next_value()?;
                        }
                        "separator" => {
                            fields.mark(SourceField::Separator);
                            fields.separator = map.next_value()?;
                        }
                        _ => {
                            fields.unknown.push(name);
                            let _: IgnoredAny = map.next_value()?;
                        }
                    }
                }

                let kind = fields
                    .kind
                    .as_deref()
                    .ok_or_else(|| de::Error::missing_field("type"))?;
                let kind = strip_ns(kind);
                match kind {
                    "directory" => {
                        fields.validate(
                            kind,
                            &[SourceField::Type, SourceField::Source, SourceField::Prefix],
                        )?;
                        Ok(AtlasSourceDocument::Directory(DirectorySourceDocument {
                            source: fields
                                .source
                                .ok_or_else(|| de::Error::missing_field("source"))?,
                            prefix: fields
                                .prefix
                                .ok_or_else(|| de::Error::missing_field("prefix"))?,
                        }))
                    }
                    "single" => {
                        fields.validate(
                            kind,
                            &[
                                SourceField::Type,
                                SourceField::Resource,
                                SourceField::Sprite,
                            ],
                        )?;
                        Ok(AtlasSourceDocument::Single(SingleSourceDocument {
                            resource: fields
                                .resource
                                .ok_or_else(|| de::Error::missing_field("resource"))?,
                            sprite: fields.sprite,
                        }))
                    }
                    "paletted_permutations" => {
                        fields.validate(
                            kind,
                            &[
                                SourceField::Type,
                                SourceField::Textures,
                                SourceField::PaletteKey,
                                SourceField::Permutations,
                                SourceField::Separator,
                            ],
                        )?;
                        Ok(AtlasSourceDocument::PalettedPermutations(
                            PalettedPermutationsDocument {
                                textures: fields
                                    .textures
                                    .ok_or_else(|| de::Error::missing_field("textures"))?,
                                palette_key: fields
                                    .palette_key
                                    .ok_or_else(|| de::Error::missing_field("palette_key"))?,
                                permutations: fields
                                    .permutations
                                    .ok_or_else(|| de::Error::missing_field("permutations"))?,
                                separator: fields.separator.unwrap_or_else(default_separator),
                            },
                        ))
                    }
                    other => Ok(AtlasSourceDocument::Unknown {
                        kind: other.to_owned(),
                    }),
                }
            }
        }

        deserializer.deserialize_map(AtlasSourceVisitor)
    }
}

/// A parsed `atlases/<id>.json` document: an ordered list of sources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtlasDefinition {
    /// The sources, in file order. Later sources may add or override sprites.
    pub sources: Vec<AtlasSource>,
}

/// One entry in an atlas source list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AtlasSource {
    /// `minecraft:directory` — every `.png` under `textures/<source>/` becomes a
    /// sprite named `<prefix><relative-path-without-extension>`.
    Directory {
        /// The subdirectory of `textures/` to scan, e.g. `entity/chest`.
        source: String,
        /// Prepended to each discovered sprite id, e.g. `entity/chest/`.
        prefix: String,
    },
    /// `minecraft:single` — one explicit texture, optionally renamed.
    Single {
        /// The texture resource (`textures/<path>.png`).
        resource: ResourceLocation,
        /// The sprite id it is stitched under. Defaults to `resource`.
        sprite: ResourceLocation,
    },
    /// `minecraft:paletted_permutations` — recoloured variants of base textures.
    ///
    /// For each `texture` × each `permutations` entry, vanilla generates a
    /// recoloured sprite `<texture><separator><permutation-key>` by remapping
    /// the `palette_key` palette to the permutation's palette. The recolour is a
    /// bake step; this variant only carries the inputs.
    PalettedPermutations {
        /// Base greyscale textures to recolour.
        textures: Vec<ResourceLocation>,
        /// The source palette every base texture is keyed against.
        palette_key: ResourceLocation,
        /// Suffix-key → replacement palette.
        permutations: BTreeMap<String, ResourceLocation>,
        /// Separator between texture id and permutation key (default `_`).
        separator: String,
    },
    /// A source type this loader does not (yet) understand. Preserved rather
    /// than rejected so an unknown type never fails a whole atlas.
    Unknown {
        /// The namespace-stripped `type` value.
        kind: String,
    },
}

/// A concrete sprite produced by resolving an atlas source against a manager.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtlasSpriteEntry {
    /// The sprite id used to address the sprite on the atlas.
    pub sprite: ResourceLocation,
    /// The full in-pack path of the backing texture, e.g.
    /// `assets/minecraft/textures/entity/chest/normal.png`.
    pub texture_path: String,
}

fn strip_ns(kind: &str) -> &str {
    kind.strip_prefix("minecraft:").unwrap_or(kind)
}

impl AtlasDefinition {
    /// Parses an `atlases/<id>.json` document.
    pub fn parse(bytes: &[u8]) -> Result<Self, AtlasSourceError> {
        let document: AtlasDefinitionDocument =
            serde_json::from_slice(bytes).map_err(|e| AtlasSourceError::Json(e.to_string()))?;
        let documents = document.sources.ok_or(AtlasSourceError::MissingSources)?;
        let sources = documents
            .into_iter()
            .map(source_from_document)
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self { sources })
    }

    /// Builds the implicit pre-1.13 terrain atlas for a version that has no
    /// declarative `atlases/*.json` index.
    ///
    /// Before the flattening, "the block atlas" was simply every texture under
    /// `textures/<block_texture_dir>/`, addressed by the same
    /// `<block_texture_dir>/<name>` id that models reference. This synthesizes a
    /// single `directory` source equivalent to what 1.13+ writes out explicitly,
    /// so [`crate::AssetProfile::uses_atlas_index`] is the only version knob the
    /// loader consults — the resolver itself never learns a version.
    ///
    /// Pass [`crate::AssetProfile::block_texture_dir`] (`"blocks"` for ≤1.12,
    /// `"block"` for 1.13+).
    pub fn implicit_terrain(block_texture_dir: &str) -> Self {
        Self {
            sources: vec![AtlasSource::Directory {
                source: block_texture_dir.to_string(),
                prefix: format!("{block_texture_dir}/"),
            }],
        }
    }

    /// Resolves the `directory` and `single` sources against the manager into
    /// concrete `(sprite, texture path)` pairs.
    ///
    /// `paletted_permutations` is skipped here — its sprites are generated at
    /// bake time; use [`AtlasSource::derived_sprite_ids`] to enumerate the ids
    /// it will produce. `unknown` sources contribute nothing.
    ///
    /// When two sources name the same sprite id, the **later** source wins —
    /// vanilla's own sprite-source-list "list" step's own output-add step is a plain
    /// `Map<Identifier, …>.put`, so a source later in [`Self::sources`]
    /// (whether a second entry in one file, or a higher-priority pack's
    /// descriptor appended by [`Self::load_stacked`]) silently replaces an
    /// earlier source's entry for that id rather than being shadowed by it.
    pub fn resolve(&self, manager: &ResourceManager) -> Vec<AtlasSpriteEntry> {
        let mut out: Vec<AtlasSpriteEntry> = Vec::new();
        let mut index: std::collections::HashMap<String, usize> = std::collections::HashMap::new();
        for source in &self.sources {
            for entry in source.resolve(manager) {
                let key = entry.sprite.to_string();
                if let Some(&i) = index.get(&key) {
                    out[i] = entry;
                } else {
                    index.insert(key, out.len());
                    out.push(entry);
                }
            }
        }
        out
    }

    /// Loads and merges **every pack layer's own copy** of an
    /// `atlases/<id>.json` descriptor at `atlas_path`, the shape
    /// vanilla's own sprite-source-list "load" step requires: it iterates
    /// its own resource-manager "get resource stack" accessor — every pack that
    /// carries the path, lowest priority first — parsing each layer and
    /// accumulating their sources into one combined list, rather than
    /// its own resource-manager "get resource" single-winner accessor. A pack that ships its
    /// own `atlases/armor_trims.json` (or `banner_patterns.json`,
    /// `shield_patterns.json`, …) therefore **extends** the source list
    /// underneath it — most commonly the jar's own `directory`/
    /// `paletted_permutations` source — rather than replacing it outright.
    ///
    /// A layer that fails to parse is skipped rather than failing the whole
    /// load, matching vanilla's own catch-and-log-per-entry behaviour. Returns `None` only when [`ResourceManager::read_stack`]
    /// finds the path in **no** layer at all — the caller's existing
    /// "descriptor missing" error, unchanged from the single-winner form this
    /// replaces.
    #[must_use]
    pub fn load_stacked(manager: &ResourceManager, atlas_path: &str) -> Option<Self> {
        let layers = manager.read_stack(atlas_path);
        if layers.is_empty() {
            return None;
        }
        let mut sources = Vec::new();
        for bytes in layers {
            if let Ok(def) = Self::parse(&bytes) {
                sources.extend(def.sources);
            }
        }
        Some(Self { sources })
    }
}

fn source_from_document(document: AtlasSourceDocument) -> Result<AtlasSource, AtlasSourceError> {
    match document {
        AtlasSourceDocument::Directory(document) => Ok(AtlasSource::Directory {
            source: document.source,
            prefix: document.prefix,
        }),
        AtlasSourceDocument::Single(document) => {
            let resource = ResourceLocation::parse(&document.resource)?;
            let sprite = document
                .sprite
                .as_deref()
                .map(ResourceLocation::parse)
                .transpose()?
                .unwrap_or_else(|| resource.clone());
            Ok(AtlasSource::Single { resource, sprite })
        }
        AtlasSourceDocument::PalettedPermutations(document) => {
            let textures = document
                .textures
                .iter()
                .map(|texture| ResourceLocation::parse(texture))
                .collect::<Result<Vec<_>, _>>()?;
            let palette_key = ResourceLocation::parse(&document.palette_key)?;
            let permutations = document
                .permutations
                .into_iter()
                .map(|(key, value)| ResourceLocation::parse(&value).map(|location| (key, location)))
                .collect::<Result<BTreeMap<_, _>, _>>()?;
            Ok(AtlasSource::PalettedPermutations {
                textures,
                palette_key,
                permutations,
                separator: document.separator,
            })
        }
        AtlasSourceDocument::Unknown { kind } => Ok(AtlasSource::Unknown { kind }),
    }
}

impl AtlasSource {
    /// Resolves this single source against the manager. `paletted_permutations`
    /// and `unknown` return nothing (see [`AtlasDefinition::resolve`]).
    pub fn resolve(&self, manager: &ResourceManager) -> Vec<AtlasSpriteEntry> {
        match self {
            AtlasSource::Directory { source, prefix } => {
                let mut out = Vec::new();
                // A directory source scans `textures/<source>/` in every
                // namespace present in the stack. The leading slash anchors the
                // namespace boundary so `<ns>` is exactly the segment before it.
                let infix = format!("/textures/{source}/");
                for path in manager.list("assets/") {
                    let Some(png) = path.strip_suffix(".png") else {
                        continue;
                    };
                    // png = assets/<ns>/textures/<source>/<rest>
                    let Some(rest_of) = png.strip_prefix("assets/") else {
                        continue;
                    };
                    let Some(idx) = rest_of.find(&infix) else {
                        continue;
                    };
                    // Namespace is the segment before "/textures/...".
                    let ns = &rest_of[..idx];
                    if ns.is_empty() || ns.contains('/') {
                        continue;
                    }
                    let rel = &rest_of[idx + infix.len()..];
                    let Ok(sprite) = ResourceLocation::parse(&format!("{ns}:{prefix}{rel}")) else {
                        continue;
                    };
                    out.push(AtlasSpriteEntry {
                        sprite,
                        texture_path: path.clone(),
                    });
                }
                out.sort_by_key(|e| e.sprite.to_string());
                out
            }
            AtlasSource::Single { resource, sprite } => {
                let texture_path = ResourceManager::asset_path(resource, "textures", "png");
                vec![AtlasSpriteEntry {
                    sprite: sprite.clone(),
                    texture_path,
                }]
            }
            AtlasSource::PalettedPermutations { .. } | AtlasSource::Unknown { .. } => Vec::new(),
        }
    }

    /// For a `paletted_permutations` source, enumerates the sprite ids it will
    /// generate (`<texture><separator><permutation-key>`). Empty for other
    /// kinds. The pixels are produced by the atlas-baking layer; this is the
    /// data the renderer needs to size the sheet.
    pub fn derived_sprite_ids(&self) -> Vec<ResourceLocation> {
        match self {
            AtlasSource::PalettedPermutations {
                textures,
                permutations,
                separator,
                ..
            } => {
                let mut out = Vec::new();
                for texture in textures {
                    for key in permutations.keys() {
                        if let Ok(loc) = ResourceLocation::parse(&format!(
                            "{}:{}{separator}{key}",
                            texture.namespace(),
                            texture.path()
                        )) {
                            out.push(loc);
                        }
                    }
                }
                out
            }
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::AtlasDefinitionDocument;

    #[test]
    fn typed_document_round_trips_all_known_source_shapes() {
        let source = r#"{
            "sources":[
                {"type":"minecraft:directory","source":"entity/chest","prefix":"entity/chest/"},
                {"type":"minecraft:single","resource":"minecraft:gui/empty_slot","sprite":"minecraft:gui/empty"},
                {"type":"minecraft:paletted_permutations","textures":["minecraft:trims/entity/humanoid/sentry"],
                 "palette_key":"minecraft:trims/color_palettes/trim_palette",
                 "permutations":{"gold":"minecraft:trims/color_palettes/gold"}}
            ]
        }"#;
        let document: AtlasDefinitionDocument = serde_json::from_str(source).unwrap();
        let encoded = serde_json::to_string(&document).unwrap();
        let decoded: AtlasDefinitionDocument = serde_json::from_str(&encoded).unwrap();
        assert_eq!(document, decoded);
    }

    #[test]
    fn unknown_source_type_survives_typed_deserialization() {
        let document: AtlasDefinitionDocument = serde_json::from_str(
            r#"{"sources":[{"type":"minecraft:future_source","payload":{"x":1}}]}"#,
        )
        .unwrap();
        let encoded = serde_json::to_string(&document).unwrap();
        assert!(encoded.contains("future_source"));
        let decoded: AtlasDefinitionDocument = serde_json::from_str(&encoded).unwrap();
        assert_eq!(document, decoded);
    }

    #[test]
    fn closed_known_source_rejects_an_unexpected_field() {
        assert!(serde_json::from_str::<AtlasDefinitionDocument>(
            r#"{"sources":[{"type":"minecraft:directory","source":"block","prefix":"","typo":true}]}"#,
        )
        .is_err());
    }
}
