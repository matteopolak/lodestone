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

use std::collections::BTreeMap;

use serde_json::Value;

use crate::{ResourceLocation, ResourceManager, error::AtlasSourceError};

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

fn str_field<'a>(obj: &'a Value, key: &'static str) -> Result<&'a str, AtlasSourceError> {
    obj.get(key)
        .and_then(Value::as_str)
        .ok_or(AtlasSourceError::MissingKey(key))
}

impl AtlasDefinition {
    /// Parses an `atlases/<id>.json` document.
    pub fn parse(bytes: &[u8]) -> Result<Self, AtlasSourceError> {
        let root: Value =
            serde_json::from_slice(bytes).map_err(|e| AtlasSourceError::Json(e.to_string()))?;
        let arr = root
            .get("sources")
            .and_then(Value::as_array)
            .ok_or(AtlasSourceError::MissingSources)?;

        let mut sources = Vec::with_capacity(arr.len());
        for entry in arr {
            sources.push(AtlasSource::parse(entry)?);
        }
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

impl AtlasSource {
    fn parse(value: &Value) -> Result<Self, AtlasSourceError> {
        let obj = value
            .as_object()
            .ok_or_else(|| AtlasSourceError::BadField("source must be an object".into()))?;
        let kind = obj
            .get("type")
            .and_then(Value::as_str)
            .ok_or(AtlasSourceError::MissingKey("type"))?;

        match strip_ns(kind) {
            "directory" => Ok(AtlasSource::Directory {
                source: str_field(value, "source")?.to_string(),
                prefix: str_field(value, "prefix")?.to_string(),
            }),
            "single" => {
                let resource = ResourceLocation::parse(str_field(value, "resource")?)?;
                let sprite = match obj.get("sprite").and_then(Value::as_str) {
                    Some(s) => ResourceLocation::parse(s)?,
                    None => resource.clone(),
                };
                Ok(AtlasSource::Single { resource, sprite })
            }
            "paletted_permutations" => {
                let textures = obj
                    .get("textures")
                    .and_then(Value::as_array)
                    .ok_or(AtlasSourceError::MissingKey("textures"))?
                    .iter()
                    .map(|t| {
                        t.as_str()
                            .ok_or_else(|| {
                                AtlasSourceError::BadField("texture must be a string".into())
                            })
                            .and_then(|s| ResourceLocation::parse(s).map_err(Into::into))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let palette_key = ResourceLocation::parse(str_field(value, "palette_key")?)?;
                let perms = obj
                    .get("permutations")
                    .and_then(Value::as_object)
                    .ok_or(AtlasSourceError::MissingKey("permutations"))?;
                let mut permutations = BTreeMap::new();
                for (k, v) in perms {
                    let loc = v
                        .as_str()
                        .ok_or_else(|| {
                            AtlasSourceError::BadField("permutation must be a string".into())
                        })
                        .and_then(|s| ResourceLocation::parse(s).map_err(Into::into))?;
                    permutations.insert(k.clone(), loc);
                }
                let separator = obj
                    .get("separator")
                    .and_then(Value::as_str)
                    .unwrap_or("_")
                    .to_string();
                Ok(AtlasSource::PalettedPermutations {
                    textures,
                    palette_key,
                    permutations,
                    separator,
                })
            }
            other => Ok(AtlasSource::Unknown {
                kind: other.to_string(),
            }),
        }
    }

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
