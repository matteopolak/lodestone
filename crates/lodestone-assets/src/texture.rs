//! PNG decoding and `*.png.mcmeta` metadata parsing.
//!
//! Decoding always normalises to RGBA8 so higher layers never have to branch on
//! the source colour type. Resource packs are untrusted, so malformed input is
//! reported as an error and never panics; the PNG decoder is also bounded by a
//! byte limit to resist decompression bombs.

use std::{collections::BTreeSet, fmt};

use serde::{
    Deserialize, Serialize,
    de::{self, IgnoredAny, MapAccess, Visitor, value::MapAccessDeserializer},
    ser::SerializeMap,
};
use std::marker::PhantomData;

use crate::error::TextureError;
use crate::mipmap::MipStrategy;

/// A decoded, RGBA8, row-major image.
///
/// `rgba` is `width * height * 4` bytes, four bytes per pixel in `R, G, B, A`
/// order, rows top to bottom.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// Image width in pixels.
    pub width: u32,
    /// Image height in pixels.
    pub height: u32,
    /// RGBA8 pixel data, `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

impl Image {
    /// Decodes a PNG from memory into RGBA8.
    ///
    /// Palette, grayscale, grayscale+alpha, RGB and RGBA inputs at bit depths
    /// 1/2/4/8/16 are all accepted and expanded to RGBA8 (`tRNS` transparency is
    /// honoured, 16-bit samples are scaled down to 8-bit). Returns
    /// [`TextureError`] on malformed or oversized input rather than panicking.
    pub fn decode_png(bytes: &[u8]) -> Result<Self, TextureError> {
        let mut decoder = png::Decoder::new(std::io::Cursor::new(bytes));
        // EXPAND: palette -> RGB, low-bit grayscale -> 8-bit, tRNS -> alpha.
        // ALPHA: palette -> include alpha. STRIP_16: 16-bit -> 8-bit.
        decoder.set_transformations(
            png::Transformations::EXPAND
                | png::Transformations::ALPHA
                | png::Transformations::STRIP_16,
        );
        let mut reader = decoder
            .read_info()
            .map_err(|e| TextureError::Decode(e.to_string()))?;
        let buf_size = reader
            .output_buffer_size()
            .ok_or_else(|| TextureError::Decode("image too large".to_string()))?;
        let mut buf = vec![0u8; buf_size];
        let info = reader
            .next_frame(&mut buf)
            .map_err(|e| TextureError::Decode(e.to_string()))?;
        buf.truncate(info.buffer_size());

        let width = info.width;
        let height = info.height;
        if width == 0 || height == 0 {
            return Err(TextureError::EmptyImage { width, height });
        }

        let pixels = (width as usize) * (height as usize);
        let rgba = match info.color_type {
            png::ColorType::Rgba => buf,
            png::ColorType::Rgb => expand(&buf, pixels, 3, |px, out| {
                out.extend_from_slice(&[px[0], px[1], px[2], 255]);
            }),
            png::ColorType::GrayscaleAlpha => expand(&buf, pixels, 2, |px, out| {
                out.extend_from_slice(&[px[0], px[0], px[0], px[1]]);
            }),
            png::ColorType::Grayscale => expand(&buf, pixels, 1, |px, out| {
                out.extend_from_slice(&[px[0], px[0], px[0], 255]);
            }),
            other => {
                return Err(TextureError::Decode(format!(
                    "unexpected post-transform colour type {other:?}"
                )));
            }
        };

        if rgba.len() != pixels * 4 {
            return Err(TextureError::Decode(format!(
                "decoded buffer size {} does not match {width}x{height} RGBA8",
                rgba.len()
            )));
        }
        Ok(Self {
            width,
            height,
            rgba,
        })
    }

    /// Returns the `[R, G, B, A]` pixel at `(x, y)`, or `[0; 4]` if out of range.
    pub fn pixel(&self, x: u32, y: u32) -> [u8; 4] {
        if x >= self.width || y >= self.height {
            return [0; 4];
        }
        let i = ((y as usize) * (self.width as usize) + (x as usize)) * 4;
        [
            self.rgba[i],
            self.rgba[i + 1],
            self.rgba[i + 2],
            self.rgba[i + 3],
        ]
    }

    /// Crops the top `frame_height`-tall slice off a vertically-stacked
    /// animation strip — frame `0` of a `*.png.mcmeta`-animated texture
    /// (`{"animation": {"height": frame_height, ...}}`), which the jar always
    /// stores as one tall PNG of `width × (width * frame_count)` (square
    /// frames) rather than as separate files.
    ///
    /// Returns `self` unchanged (not a copy with the same dimensions — the
    /// literal same [`Image`], no reallocation) when `frame_height` is `0` or
    /// at least the image's own height, so passing a non-animated texture's
    /// full height through this is a safe no-op rather than a special case a
    /// caller has to detect first.
    #[must_use]
    pub fn first_animation_frame(&self, frame_height: u32) -> Self {
        if frame_height == 0 || frame_height >= self.height {
            return self.clone();
        }
        let row_bytes = (self.width as usize) * 4;
        let take = (frame_height as usize) * row_bytes;
        Image {
            width: self.width,
            height: frame_height,
            rgba: self.rgba[..take.min(self.rgba.len())].to_vec(),
        }
    }
}

/// Expands a tightly packed `channels`-per-pixel buffer to RGBA8 via `f`.
fn expand(buf: &[u8], pixels: usize, channels: usize, f: impl Fn(&[u8], &mut Vec<u8>)) -> Vec<u8> {
    let mut out = Vec::with_capacity(pixels * 4);
    for px in buf.chunks_exact(channels) {
        f(px, &mut out);
    }
    out
}

/// Typed wire names for the texture metadata mipmap strategies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum MipmapStrategyDocument {
    Auto,
    Mean,
    Cutout,
    StrictCutout,
    DarkCutout,
}

impl Default for MipmapStrategyDocument {
    fn default() -> Self {
        Self::Auto
    }
}

impl From<MipmapStrategyDocument> for MipStrategy {
    fn from(strategy: MipmapStrategyDocument) -> Self {
        match strategy {
            MipmapStrategyDocument::Auto => Self::Auto,
            MipmapStrategyDocument::Mean => Self::Mean,
            MipmapStrategyDocument::Cutout => Self::Cutout,
            MipmapStrategyDocument::StrictCutout => Self::StrictCutout,
            MipmapStrategyDocument::DarkCutout => Self::DarkCutout,
        }
    }
}

/// A positive `u32` field in a texture metadata document.
///
/// Dimensions and durations are optional at the object level but, when
/// present, zero is not a valid value. Keeping that rule in the DTO means the
/// lowering step never has to inspect an untyped JSON number.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PositiveU32Document(u32);

impl<'de> Deserialize<'de> for PositiveU32Document {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        let value =
            u32::try_from(value).map_err(|_| de::Error::custom("value does not fit in a u32"))?;
        if value == 0 {
            return Err(de::Error::custom("value must be greater than zero"));
        }
        Ok(Self(value))
    }
}

impl Serialize for PositiveU32Document {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_u32(self.0)
    }
}

/// An optional positive `u32` that rejects explicit `null` while still using
/// the enclosing struct's default when the field is absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct OptionalPositiveU32Document(Option<u32>);

impl OptionalPositiveU32Document {
    fn is_none(&self) -> bool {
        self.0.is_none()
    }
}

impl<'de> Deserialize<'de> for OptionalPositiveU32Document {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(Self(Some(
            PositiveU32Document::deserialize(deserializer)?.0,
        )))
    }
}

impl Serialize for OptionalPositiveU32Document {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self.0 {
            Some(value) => serializer.serialize_u32(value),
            None => serializer.serialize_none(),
        }
    }
}

fn default_frametime_document() -> PositiveU32Document {
    PositiveU32Document(1)
}

/// Restricts a derived struct DTO to JSON objects. Serde's default struct
/// visitor also accepts positional sequences when every field has a default;
/// resource-pack sections are named objects, so the outer map check matters.
fn deserialize_object<'de, D, T>(deserializer: D) -> Result<T, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct ObjectVisitor<T>(PhantomData<T>);

    impl<'de, T> Visitor<'de> for ObjectVisitor<T>
    where
        T: Deserialize<'de>,
    {
        type Value = T;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("an object")
        }

        fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
        where
            M: MapAccess<'de>,
        {
            T::deserialize(MapAccessDeserializer::new(map))
        }
    }

    deserializer.deserialize_map(ObjectVisitor(PhantomData))
}

/// The closed `texture` section of a texture metadata document.
#[derive(Debug, Clone, PartialEq)]
struct TextureSectionDocument(TextureSectionDocumentFields);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct TextureSectionDocumentFields {
    #[serde(default)]
    blur: bool,
    #[serde(default)]
    clamp: bool,
    #[serde(default)]
    mipmap_strategy: MipmapStrategyDocument,
    #[serde(default)]
    alpha_cutoff_bias: f32,
}

impl Serialize for TextureSectionDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for TextureSectionDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserialize_object(deserializer).map(Self)
    }
}

impl From<TextureSectionDocument> for TextureSection {
    fn from(document: TextureSectionDocument) -> Self {
        Self {
            blur: document.0.blur,
            clamp: document.0.clamp,
            mipmap_strategy: document.0.mipmap_strategy.into(),
            alpha_cutoff_bias: document.0.alpha_cutoff_bias,
        }
    }
}

/// The closed object form of one animation frame.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnimationFrameObjectDocument {
    index: u32,
    #[serde(default, skip_serializing_if = "OptionalPositiveU32Document::is_none")]
    time: OptionalPositiveU32Document,
}

/// The two legal animation-frame shapes: a bare index or an object with an
/// optional duration override.
#[derive(Debug, Clone, PartialEq)]
enum AnimationFrameDocument {
    Index(u32),
    Object(AnimationFrameObjectDocument),
}

impl Serialize for AnimationFrameDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Index(index) => serializer.serialize_u32(*index),
            Self::Object(document) => document.serialize(serializer),
        }
    }
}

impl<'de> Deserialize<'de> for AnimationFrameDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct FrameVisitor;

        impl<'de> Visitor<'de> for FrameVisitor {
            type Value = AnimationFrameDocument;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("an animation frame index or object")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: de::Error,
            {
                let index = u32::try_from(value)
                    .map_err(|_| E::custom("frame index does not fit in a u32"))?;
                Ok(AnimationFrameDocument::Index(index))
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                AnimationFrameObjectDocument::deserialize(MapAccessDeserializer::new(map))
                    .map(AnimationFrameDocument::Object)
            }
        }

        deserializer.deserialize_any(FrameVisitor)
    }
}

impl From<AnimationFrameDocument> for AnimationFrame {
    fn from(document: AnimationFrameDocument) -> Self {
        match document {
            AnimationFrameDocument::Index(index) => Self { index, time: None },
            AnimationFrameDocument::Object(document) => Self {
                index: document.index,
                time: document.time.0,
            },
        }
    }
}

/// The closed `animation` section of a texture metadata document.
#[derive(Debug, Clone, PartialEq)]
struct AnimationDocument(AnimationDocumentFields);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct AnimationDocumentFields {
    #[serde(default = "default_frametime_document")]
    frametime: PositiveU32Document,
    #[serde(default)]
    interpolate: bool,
    #[serde(default, skip_serializing_if = "OptionalPositiveU32Document::is_none")]
    width: OptionalPositiveU32Document,
    #[serde(default, skip_serializing_if = "OptionalPositiveU32Document::is_none")]
    height: OptionalPositiveU32Document,
    #[serde(default)]
    frames: Vec<AnimationFrameDocument>,
}

impl Serialize for AnimationDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        self.0.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AnimationDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        deserialize_object(deserializer).map(Self)
    }
}

impl From<AnimationDocument> for AnimationMeta {
    fn from(document: AnimationDocument) -> Self {
        Self {
            frametime: document.0.frametime.0,
            interpolate: document.0.interpolate,
            frame_width: document.0.width.0,
            frame_height: document.0.height.0,
            frames: document
                .0
                .frames
                .into_iter()
                .map(AnimationFrame::from)
                .collect(),
        }
    }
}

/// Typed transport form of the metadata root.
///
/// Unknown top-level section values are deliberately consumed as
/// [`IgnoredAny`]. Their payloads are section-specific and not interpreted by
/// this crate; only their names are part of the public API's presence census.
#[derive(Debug, Clone, PartialEq)]
struct TextureMetaDocument {
    animation: Option<AnimationDocument>,
    texture: Option<TextureSectionDocument>,
    other_sections: BTreeSet<String>,
}

impl Serialize for TextureMetaDocument {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let unknown_count = self
            .other_sections
            .iter()
            .filter(|name| name.as_str() != "animation" && name.as_str() != "texture")
            .count();
        let field_count = usize::from(self.animation.is_some())
            + usize::from(self.texture.is_some())
            + unknown_count;
        let mut state = serializer.serialize_map(Some(field_count))?;
        if let Some(animation) = &self.animation {
            state.serialize_entry("animation", animation)?;
        }
        if let Some(texture) = &self.texture {
            state.serialize_entry("texture", texture)?;
        }
        // The payload is intentionally not retained. `null` is a valid JSON
        // placeholder that preserves the documented presence-only semantics.
        for name in &self.other_sections {
            if name != "animation" && name != "texture" {
                state.serialize_entry(name, &())?;
            }
        }
        state.end()
    }
}

impl<'de> Deserialize<'de> for TextureMetaDocument {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        struct TextureMetaVisitor;

        impl<'de> Visitor<'de> for TextureMetaVisitor {
            type Value = TextureMetaDocument;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a texture metadata object")
            }

            fn visit_map<M>(self, mut map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut animation: Option<AnimationDocument> = None;
                let mut texture: Option<TextureSectionDocument> = None;
                let mut other_sections = BTreeSet::new();
                while let Some(name) = map.next_key::<String>()? {
                    match name.as_str() {
                        "animation" => animation = Some(map.next_value()?),
                        "texture" => texture = Some(map.next_value()?),
                        _ => {
                            other_sections.insert(name);
                            let _: IgnoredAny = map.next_value()?;
                        }
                    }
                }
                Ok(TextureMetaDocument {
                    animation,
                    texture,
                    other_sections,
                })
            }
        }

        deserializer.deserialize_map(TextureMetaVisitor)
    }
}

/// The `texture` section of a `*.png.mcmeta`, mirroring vanilla's
/// `TextureMetadataSection` record.
///
/// The two fields that matter to the block atlas are
/// [`mipmap_strategy`](Self::mipmap_strategy) and
/// [`alpha_cutoff_bias`](Self::alpha_cutoff_bias): both are inputs to vanilla's
/// per-sprite mip generation, and leaving them at their defaults renders 45 of
/// the 26.2 block sprites through the wrong downsample. Every leaves texture
/// asks for `dark_cutout`, 27 flower/amethyst sprites ask for `strict_cutout`
/// (a `0.3` alpha-coverage reference rather than `0.5`), glass and the four
/// redstone-dust sprites ask for plain `mean`, and cactus, kelp and tripwire
/// carry a `0.1` cutoff bias. See [`crate::mipmap`] for what each does.
///
/// `blur` and `clamp` are parsed for completeness — they select the GPU
/// sampler in vanilla and this crate is GPU-free, so nothing here consumes
/// them yet.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextureSection {
    /// Vanilla's `blur`: sample this texture with a linear filter.
    pub blur: bool,
    /// Vanilla's `clamp`: clamp rather than repeat at the texture's edge.
    pub clamp: bool,
    /// How the mip chain is downsampled. Vanilla's default is
    /// [`MipStrategy::Auto`].
    pub mipmap_strategy: MipStrategy,
    /// Added to every texel's alpha after the coverage rescale, on top of
    /// vanilla's unconditional `0.025`. Default `0.0`.
    pub alpha_cutoff_bias: f32,
}

impl Default for TextureSection {
    fn default() -> Self {
        Self {
            blur: false,
            clamp: false,
            mipmap_strategy: MipStrategy::Auto,
            alpha_cutoff_bias: 0.0,
        }
    }
}

/// Parsed `*.png.mcmeta` texture metadata.
///
/// The `animation` and `texture` sections are modelled explicitly. Other
/// vanilla sections — `gui`, `villager` — are recognised and their presence
/// recorded, but they are not otherwise interpreted here.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TextureMeta {
    /// The animation section, if present.
    pub animation: Option<AnimationMeta>,
    /// The `texture` section, if present. Its contents are interpreted — see
    /// [`TextureSection`].
    pub texture: Option<TextureSection>,
    /// Names of every top-level section other than `animation` that was
    /// present, sorted and de-duplicated. This records *presence*, not lack of
    /// interpretation: `texture` appears here as well as in
    /// [`Self::texture`].
    pub other_sections: Vec<String>,
}

impl TextureMeta {
    /// Parses `*.png.mcmeta` bytes.
    ///
    /// A file with no `animation` section (for example a `gui`/`villager`/
    /// `texture` mcmeta) parses successfully with `animation == None`. Returns
    /// [`TextureError::MetaMalformed`] only when the bytes are not valid JSON or
    /// the `animation` section has an invalid shape.
    pub fn parse(bytes: &[u8]) -> Result<Self, TextureError> {
        // Lenient about *trailing* content, strict about the value — vanilla's
        // own JSON-helper parse step reads one value off a `JsonReader` and never
        // asserts end-of-document, so a pack whose `.mcmeta` carries a stray
        // extra closing brace renders normally in the real client. Rejecting it
        // here costs the whole texture, not just its animation, because
        // `AtlasBuilder::load` treats a metadata failure as a texture failure.
        // See `crate::json`.
        let document: TextureMetaDocument = crate::json::from_slice_lenient(bytes)
            .map_err(|e| TextureError::MetaMalformed(e.to_string()))?;
        let texture = document.texture.map(TextureSection::from);
        let animation = document.animation.map(AnimationMeta::from);
        let mut other_sections: Vec<String> = document.other_sections.into_iter().collect();
        if texture.is_some() {
            other_sections.push("texture".to_owned());
        }
        other_sections.sort();
        other_sections.dedup();
        Ok(Self {
            animation,
            texture,
            other_sections,
        })
    }
}

/// The `animation` section of a `*.png.mcmeta`.
///
/// The PNG is a vertical strip of equally sized frames. `frames` gives the
/// playback order (and optional per-frame timing); when it is empty the frames
/// play in natural top-to-bottom order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnimationMeta {
    /// Default frame duration in ticks (vanilla default `1`).
    pub frametime: u32,
    /// Whether the renderer should interpolate between frames.
    pub interpolate: bool,
    /// Explicit frame width in pixels, if overridden.
    pub frame_width: Option<u32>,
    /// Explicit frame height in pixels, if overridden.
    pub frame_height: Option<u32>,
    /// Explicit playback order; empty means natural order.
    pub frames: Vec<AnimationFrame>,
}

/// A single animation frame: an index into the strip and an optional per-frame
/// duration override.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnimationFrame {
    /// Zero-based index of the frame within the vertical strip.
    pub index: u32,
    /// Per-frame duration override in ticks, if given.
    pub time: Option<u32>,
}

#[cfg(test)]
mod tests {
    use super::TextureMetaDocument;

    #[test]
    fn typed_document_round_trips_known_sections_and_unknown_section_names() {
        let source = r#"{
            "animation":{"frametime":2,"interpolate":true,"width":16,
                "height":16,"frames":[0,{"index":1,"time":3}]},
            "texture":{"mipmap_strategy":"dark_cutout","blur":true},
            "gui":{"scaling":{"type":"nine_slice"}},
            "future_section":[1,2,3]
        }"#;
        let document: TextureMetaDocument = serde_json::from_str(source).unwrap();
        let encoded = serde_json::to_string(&document).unwrap();
        let decoded: TextureMetaDocument = serde_json::from_str(&encoded).unwrap();
        assert_eq!(document, decoded);
    }

    #[test]
    fn closed_nested_sections_reject_unknown_fields() {
        assert!(
            serde_json::from_str::<TextureMetaDocument>(
                r#"{"animation":{"frametime":1,"unexpected":true}}"#,
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<TextureMetaDocument>(
                r#"{"texture":{"blur":false,"unexpected":true}}"#,
            )
            .is_err()
        );
        assert!(
            serde_json::from_str::<TextureMetaDocument>(
                r#"{"animation":{"frames":[{"index":0,"unexpected":true}]}}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn positive_optional_fields_reject_zero_and_null() {
        assert!(
            serde_json::from_str::<TextureMetaDocument>(r#"{"animation":{"width":0}}"#,).is_err()
        );
        assert!(
            serde_json::from_str::<TextureMetaDocument>(r#"{"animation":{"height":null}}"#,)
                .is_err()
        );
        assert!(
            serde_json::from_str::<TextureMetaDocument>(
                r#"{"animation":{"frames":[{"index":0,"time":null}]}}"#,
            )
            .is_err()
        );
    }

    #[test]
    fn animation_array_is_not_a_document() {
        assert!(serde_json::from_str::<TextureMetaDocument>(r#"{"animation":[]}"#).is_err());
    }
}
