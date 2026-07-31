//! Error types for the asset layer.

/// Error returned when a [`crate::ResourceLocation`] fails to parse or validate.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResourceLocationError {
    /// The namespace or path was empty.
    #[error("resource location has an empty {part}")]
    Empty {
        /// Which part was empty (`"namespace"` or `"path"`).
        part: &'static str,
    },
    /// A character outside the allowed set was found.
    #[error("invalid character {ch:?} in {part} of resource location {input:?}")]
    InvalidCharacter {
        /// Which part contained the bad character (`"namespace"` or `"path"`).
        part: &'static str,
        /// The offending character.
        ch: char,
        /// The full input that was rejected.
        input: String,
    },
    /// The input contained more than one `:` separator.
    #[error("resource location {input:?} contains more than one ':' separator")]
    TooManySeparators {
        /// The full input that was rejected.
        input: String,
    },
}

/// Errors produced while loading resources or pack metadata.
#[derive(Debug, thiserror::Error)]
pub enum AssetError {
    /// An underlying I/O error.
    #[error("i/o error for {path:?}: {source}")]
    Io {
        /// The path being accessed when the error occurred.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
    /// A zip/jar archive could not be opened or read.
    #[error("zip error for {path:?}: {source}")]
    Zip {
        /// The archive path.
        path: String,
        /// The underlying zip error.
        #[source]
        source: zip::result::ZipError,
    },
    /// `pack.mcmeta` was missing.
    #[error("pack metadata (pack.mcmeta) not found")]
    MetaMissing,
    /// `pack.mcmeta` was present but could not be parsed.
    #[error("malformed pack.mcmeta: {0}")]
    MetaMalformed(String),
    /// A language file was present but was not a valid flat JSON object.
    #[error("malformed language file: {0}")]
    LangMalformed(String),
}

/// Errors produced while parsing blockstate JSON.
#[derive(Debug, thiserror::Error)]
pub enum BlockStateError {
    /// The blockstate JSON was not valid JSON.
    #[error("invalid blockstate json: {0}")]
    Json(String),
    /// The document had neither a `variants` nor a `multipart` key.
    #[error("blockstate has neither \"variants\" nor \"multipart\"")]
    MissingDefinition,
    /// A field had an unexpected shape.
    #[error("invalid blockstate field {field:?}: {reason}")]
    InvalidField {
        /// The offending field.
        field: &'static str,
        /// Why it was rejected.
        reason: String,
    },
    /// A model reference was not a valid resource location.
    #[error("invalid model reference: {0}")]
    BadModel(#[from] ResourceLocationError),
}

/// Errors produced while parsing or resolving block models.
#[derive(Debug, thiserror::Error)]
pub enum ModelError {
    /// A referenced model file was not present in any pack.
    #[error("model not found: {location}")]
    NotFound {
        /// The missing model.
        location: String,
    },
    /// A model's JSON was invalid.
    #[error("invalid model json for {location}: {reason}")]
    Json {
        /// The model being parsed.
        location: String,
        /// The parse error.
        reason: String,
    },
    /// The parent chain formed a cycle.
    #[error("parent cycle detected while resolving {location}")]
    ParentCycle {
        /// The model whose chain looped.
        location: String,
    },
    /// The parent chain exceeded the maximum depth.
    #[error("parent chain too deep while resolving {location}")]
    MaxDepthExceeded {
        /// The model whose chain was too deep.
        location: String,
    },
}

/// Errors produced while parsing an item definition (`items/<id>.json`).
#[derive(Debug, thiserror::Error)]
pub enum ItemModelError {
    /// The definition was not valid JSON.
    #[error("invalid item definition json: {0}")]
    Json(String),
    /// The document had no top-level `model` field.
    #[error("item definition missing \"model\"")]
    MissingModel,
    /// A required key was absent.
    #[error("item definition missing key {0:?}")]
    MissingKey(&'static str),
    /// A field had an unexpected shape.
    #[error("invalid item definition field: {0}")]
    BadField(String),
    /// A model reference was not a valid resource location.
    #[error("invalid item model reference: {0}")]
    BadModel(#[from] ResourceLocationError),
}

/// Errors produced while building an item's inventory [`crate::ItemIcon`].
#[derive(Debug, thiserror::Error)]
pub enum IconError {
    /// No `items/<id>.json` definition exists for the item.
    #[error("item definition not found: {0}")]
    DefinitionMissing(String),
    /// The item definition failed to parse.
    #[error("item definition error: {0}")]
    Definition(#[from] ItemModelError),
    /// A model the definition references failed to resolve.
    #[error("model error: {0}")]
    Model(#[from] ModelError),
}

/// Errors produced while parsing an `atlases/<id>.json` source list.
#[derive(Debug, thiserror::Error)]
pub enum AtlasSourceError {
    /// The definition was not valid JSON.
    #[error("invalid atlas definition json: {0}")]
    Json(String),
    /// The document had no top-level `sources` array.
    #[error("atlas definition missing \"sources\" array")]
    MissingSources,
    /// A required key was absent for a given source type.
    #[error("atlas source missing key {0:?}")]
    MissingKey(&'static str),
    /// A field had an unexpected shape.
    #[error("invalid atlas source field: {0}")]
    BadField(String),
    /// A resource reference was not a valid resource location.
    #[error("invalid atlas resource reference: {0}")]
    BadResource(#[from] ResourceLocationError),
}

/// Errors produced while decoding textures or parsing texture metadata.
#[derive(Debug, thiserror::Error)]
pub enum TextureError {
    /// The PNG data could not be decoded (malformed, unsupported, or truncated).
    #[error("failed to decode png: {0}")]
    Decode(String),
    /// The decoded image had zero width or height.
    #[error("decoded image has zero area ({width}x{height})")]
    EmptyImage {
        /// Decoded width.
        width: u32,
        /// Decoded height.
        height: u32,
    },
    /// A `*.png.mcmeta` file was present but could not be parsed.
    #[error("malformed texture metadata (.mcmeta): {0}")]
    MetaMalformed(String),
}

/// Errors produced while building a texture atlas.
#[derive(Debug, thiserror::Error)]
pub enum AtlasError {
    /// A texture referenced by a model was not present in any pack.
    #[error("texture not found: {location}")]
    TextureMissing {
        /// The missing texture.
        location: String,
    },
    /// A texture failed to decode or its metadata failed to parse.
    #[error("texture {location}: {source}")]
    Texture {
        /// The texture being processed.
        location: String,
        /// The underlying texture error.
        #[source]
        source: TextureError,
    },
    /// An animation strip's frame height does not divide the image height.
    #[error(
        "animation frame height {frame_height} does not divide image height {image_height} for {location}"
    )]
    BadAnimationStrip {
        /// The texture being processed.
        location: String,
        /// The declared/derived frame height.
        frame_height: u32,
        /// The full image height.
        image_height: u32,
    },
    /// No sprites were supplied to the atlas builder.
    #[error("cannot build an atlas from zero sprites")]
    Empty,
}

/// Errors produced while building the flat item-sprite atlas.
#[derive(Debug, thiserror::Error)]
pub enum ItemAtlasError {
    /// The underlying sprite atlas failed to stitch.
    #[error("item atlas: {0}")]
    Atlas(#[from] AtlasError),
}

/// Errors produced while building the particle-sprite atlas.
#[derive(Debug, thiserror::Error)]
pub enum ParticleAtlasError {
    /// The underlying sprite atlas failed to stitch.
    #[error("particle atlas: {0}")]
    Atlas(#[from] AtlasError),
}

/// Errors produced while loading sky assets: the celestial (sun + moon phase)
/// atlas and the cloud texture.
#[derive(Debug, thiserror::Error)]
pub enum SkyAssetError {
    /// A required sky texture (sun, a moon phase, or the cloud map) was not
    /// present in any pack.
    #[error("sky texture not found: {location}")]
    Missing {
        /// The missing texture's location.
        location: String,
    },
    /// A sky texture failed to decode.
    #[error("sky texture {location}: {source}")]
    Texture {
        /// The texture being processed.
        location: String,
        /// The underlying decode error.
        #[source]
        source: TextureError,
    },
    /// The underlying celestial-atlas stitch failed for a reason other than a
    /// missing texture.
    #[error("celestial atlas: {0}")]
    Atlas(#[from] AtlasError),
}

/// Errors produced while baking a resolved model into renderer-ready geometry.
#[derive(Debug, thiserror::Error)]
pub enum BakeError {
    /// A face referenced a texture variable that did not resolve to a location.
    #[error("unresolved texture variable {variable:?} while baking a face")]
    UnresolvedTexture {
        /// The `#variable` (or bare name) that failed to resolve.
        variable: String,
    },
    /// A face's resolved texture had no sprite in the atlas.
    #[error("no atlas sprite for texture {location}")]
    SpriteMissing {
        /// The texture location with no atlas sprite.
        location: String,
    },
    /// The block state id was not present in the registry.
    #[error("unknown block state id {id}")]
    UnknownState {
        /// The unresolved numeric block state id.
        id: u32,
    },
    /// The blockstate definition for a block could not be read or parsed.
    #[error("blockstate for {block}: {reason}")]
    Blockstate {
        /// The block whose blockstate failed.
        block: String,
        /// The underlying reason.
        reason: String,
    },
    /// A referenced model failed to resolve.
    #[error("model {location}: {source}")]
    Model {
        /// The model being baked.
        location: String,
        /// The underlying model error.
        #[source]
        source: ModelError,
    },
}

/// Errors produced while loading biome colormaps for tint resolution.
#[derive(Debug, thiserror::Error)]
pub enum TintError {
    /// A colormap texture was not present in the pack stack.
    #[error("colormap texture not found: {name}")]
    MissingColormap {
        /// The colormap name (e.g. `grass`, `foliage`, `dry_foliage`).
        name: String,
    },
    /// A colormap image had zero area.
    #[error("colormap image has zero area")]
    EmptyColormap,
    /// A colormap texture failed to decode.
    #[error("colormap decode failed: {0}")]
    Texture(#[from] TextureError),
}
/// Errors produced while loading and resolving fonts.
#[derive(Debug, thiserror::Error)]
pub enum FontError {
    /// The font definition was not valid JSON, or a field had the wrong shape.
    #[error("invalid font json: {0}")]
    Json(String),
    /// A referenced font (or a font requested directly) was not present.
    #[error("font not found: {id}")]
    NotFound {
        /// The font id that could not be located.
        id: String,
    },
    /// A `reference` provider chain formed a cycle.
    #[error("font reference cycle detected at {id}")]
    ReferenceCycle {
        /// The font id where the cycle closed.
        id: String,
    },
    /// A bitmap provider's `chars` grid was empty or had uneven row lengths.
    #[error("invalid bitmap font grid: {reason}")]
    InvalidGrid {
        /// Why the grid was rejected.
        reason: String,
    },
    /// A bitmap provider referenced a texture that no pack supplied.
    #[error("bitmap font texture not found: {file}")]
    MissingTexture {
        /// The texture resource location.
        file: String,
    },
    /// A bitmap provider's texture failed to decode.
    #[error("bitmap font texture decode failed: {0}")]
    Texture(#[from] TextureError),
    /// A resource location inside the font definition was malformed.
    #[error("invalid resource location in font: {0}")]
    Location(#[from] ResourceLocationError),
}

/// Errors produced while parsing GUI sprite scaling metadata (`gui.scaling`).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GuiError {
    /// The metadata was not valid JSON, or a field had the wrong shape.
    #[error("invalid gui metadata json: {0}")]
    Json(String),
    /// An unknown `scaling.type` was encountered.
    #[error("unknown gui scaling type: {0:?}")]
    UnknownType(String),
    /// A dimension or border field was zero, negative, or otherwise invalid.
    #[error("invalid gui scaling field: {0}")]
    InvalidField(String),
    /// A nine-slice border leaves no center slice in one axis.
    #[error("nine-slice has no center slice: {0}")]
    NoCenter(String),
}

/// Errors produced while parsing or resolving `sounds.json` sound events.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SoundError {
    /// The registry was not valid JSON, or a field had the wrong shape.
    #[error("invalid sounds.json: {0}")]
    Json(String),
    /// A numeric field was outside its allowed range (volume/pitch > 0, weight > 0).
    #[error("invalid sound field: {0}")]
    InvalidField(String),
    /// An unknown sound entry `type` was encountered (expected `file` or `event`).
    #[error("unknown sound type: {0:?}")]
    UnknownType(String),
    /// A `type: event` reference chain formed a cycle or exceeded the depth bound.
    #[error("sound event reference cycle or overflow at {id}")]
    ReferenceCycle {
        /// The event id where the cycle or overflow was detected.
        id: String,
    },
    /// A resource location inside the registry was malformed.
    #[error("invalid resource location in sounds.json: {0}")]
    Location(#[from] ResourceLocationError),
}

/// Errors produced while parsing particle definition JSON.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParticleError {
    /// The definition was not valid JSON, or a field had the wrong shape.
    #[error("invalid particle json: {0}")]
    Json(String),
    /// A resource location inside the definition was malformed.
    #[error("invalid resource location in particle: {0}")]
    Location(#[from] ResourceLocationError),
}
