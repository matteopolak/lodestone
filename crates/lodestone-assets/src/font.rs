//! Fonts and text metrics: parsing `assets/<ns>/font/*.json` providers and
//! deriving per-codepoint glyph metrics (advance widths + atlas cell rects).
//!
//! This is the **metrics and glyph-atlas** half of text rendering. It does not
//! parse text components or legacy-formatted strings into runs — that belongs to
//! the text layer (`lodestone-text`/`lodestone-model`). The seam is
//! [`TextStyle`]-shaped: the text layer decomposes a component tree (or a
//! `§`-coded legacy string) into a sequence of `(codepoint, style)` and asks this
//! module for each glyph's advance and sprite. Only **bold** affects advance
//! (+1 px); colour/italic/obfuscation do not change width. A convenience
//! [`Font::legacy_width`] is provided for measuring raw `§`-coded strings without
//! pulling in the component model.
//!
//! # Provider kinds
//!
//! Vanilla font definitions are an ordered list of providers, each optionally
//! gated by a [`FontFilter`] on [`FontOption`]s (e.g. "force unicode font"):
//! - `bitmap` — a PNG grid; a glyph's advance is derived from the **rightmost
//!   non-transparent column** of its cell, not the cell width (verified against
//!   `BitmapProvider.getActualGlyphWidth` in the 26.2 client).
//! - `space` — explicit per-codepoint advances (this is how the space character
//!   gets its width; its ascii cell is blank).
//! - `reference` — includes another font's providers in place.
//! - `ttf` / `unihex` — parsed (so packs using them don't fail to load) but not
//!   rasterised here; they contribute no bitmap glyphs.
//!
//! # Priority
//!
//! Providers are listed in order of **decreasing** priority: the first provider
//! that supplies a codepoint wins. This is why the `space` provider (declared
//! before the ascii bitmap in vanilla's `default.json`) sets the space width even
//! though the ascii sheet also has a (blank) space cell.

use crate::error::FontError;
use crate::location::ResourceLocation;
use crate::manager::ResourceManager;
use crate::texture::Image;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

/// The advance of vanilla's "missing glyph" (a 5px hollow box, `width + 1`).
pub const MISSING_ADVANCE: f32 = 6.0;

/// A font option that can gate a conditional provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum FontOption {
    /// "Force Unicode Font" is enabled.
    Uniform,
    /// Japanese glyph variants are enabled.
    JapaneseVariants,
}

impl FontOption {
    fn from_key(key: &str) -> Option<Self> {
        match key {
            "uniform" => Some(Self::Uniform),
            "jp" => Some(Self::JapaneseVariants),
            _ => None,
        }
    }
}

/// The set of active font options (default: none active).
#[derive(Debug, Clone, Default)]
pub struct FontOptions {
    active: HashSet<FontOption>,
}

impl FontOptions {
    /// No options active — the normal default configuration.
    pub fn none() -> Self {
        Self::default()
    }

    /// Returns a copy with `option` activated.
    pub fn with(mut self, option: FontOption) -> Self {
        self.active.insert(option);
        self
    }

    fn contains(&self, option: FontOption) -> bool {
        self.active.contains(&option)
    }
}

/// A provider's activation condition: a map from font option to the required
/// active state. A provider is active iff every condition matches.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FontFilter {
    conditions: BTreeMap<FontOption, bool>,
}

impl FontFilter {
    /// A filter that always passes.
    pub fn always() -> Self {
        Self::default()
    }

    /// Whether this filter passes for the given active options.
    pub fn passes(&self, options: &FontOptions) -> bool {
        self.conditions
            .iter()
            .all(|(opt, expected)| options.contains(*opt) == *expected)
    }

    /// Returns this filter with `override_with`'s conditions layered on top
    /// (matching vanilla's `mergeFilters`, where the referencing font's filter
    /// overrides the referenced provider's on conflict).
    fn with_override(&self, override_with: &FontFilter) -> FontFilter {
        let mut conditions = self.conditions.clone();
        for (k, v) in &override_with.conditions {
            conditions.insert(*k, *v);
        }
        FontFilter { conditions }
    }

    fn parse(value: Option<&Value>) -> Self {
        let mut conditions = BTreeMap::new();
        if let Some(Value::Object(map)) = value {
            for (k, v) in map {
                if let (Some(opt), Some(b)) = (FontOption::from_key(k), v.as_bool()) {
                    conditions.insert(opt, b);
                }
            }
        }
        FontFilter { conditions }
    }
}

/// A single unihex size override range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnihexOverride {
    /// First codepoint (inclusive).
    pub from: u32,
    /// Last codepoint (inclusive).
    pub to: u32,
    /// Left column bound.
    pub left: i32,
    /// Right column bound.
    pub right: i32,
}

/// A parsed glyph-provider definition.
#[derive(Debug, Clone, PartialEq)]
pub enum ProviderDef {
    /// A bitmap sheet.
    Bitmap {
        /// The texture location. Vanilla's `file` field already carries the
        /// `.png` extension; the loader prepends `assets/<ns>/textures/`.
        file: ResourceLocation,
        /// Declared logical height (default 8).
        height: i32,
        /// Baseline ascent.
        ascent: i32,
        /// Codepoint grid, row-major.
        chars: Vec<Vec<u32>>,
    },
    /// Explicit advances per codepoint.
    Space {
        /// Codepoint -> advance width.
        advances: BTreeMap<u32, f32>,
    },
    /// A TrueType font (parsed, not rasterised here).
    Ttf {
        /// The `.ttf`/`.otf` file.
        file: ResourceLocation,
        /// Point size.
        size: f32,
        /// Oversampling factor.
        oversample: f32,
        /// `[x, y]` shift.
        shift: [f32; 2],
        /// Codepoints to skip.
        skip: Vec<u32>,
    },
    /// A unihex provider (parsed, not rasterised here).
    Unihex {
        /// The zip of `.hex` files.
        hex_file: ResourceLocation,
        /// Per-range size overrides.
        size_overrides: Vec<UnihexOverride>,
    },
    /// Includes another font's providers in place.
    Reference {
        /// The referenced font id.
        id: ResourceLocation,
    },
}

/// A provider paired with its activation filter.
#[derive(Debug, Clone, PartialEq)]
pub struct ConditionalProvider {
    /// The provider definition.
    pub def: ProviderDef,
    /// Its activation filter.
    pub filter: FontFilter,
}

/// A parsed font definition file (`font/*.json`).
#[derive(Debug, Clone, PartialEq)]
pub struct FontDefinition {
    /// The ordered provider list (decreasing priority).
    pub providers: Vec<ConditionalProvider>,
}

impl FontDefinition {
    /// Parses a font definition from JSON bytes.
    pub fn parse(bytes: &[u8]) -> Result<Self, FontError> {
        let root: Value =
            serde_json::from_slice(bytes).map_err(|e| FontError::Json(e.to_string()))?;
        let providers = root
            .get("providers")
            .and_then(Value::as_array)
            .ok_or_else(|| FontError::Json("missing \"providers\" array".into()))?;
        let mut out = Vec::with_capacity(providers.len());
        for p in providers {
            out.push(parse_provider(p)?);
        }
        Ok(FontDefinition { providers: out })
    }
}

fn parse_provider(value: &Value) -> Result<ConditionalProvider, FontError> {
    let obj = value
        .as_object()
        .ok_or_else(|| FontError::Json("provider is not an object".into()))?;
    let ty = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| FontError::Json("provider missing \"type\"".into()))?;
    let filter = FontFilter::parse(obj.get("filter"));
    let def = match ty {
        "bitmap" => {
            let file = parse_loc(obj.get("file"), "bitmap.file")?;
            let height = obj.get("height").and_then(Value::as_i64).unwrap_or(8) as i32;
            let ascent = obj
                .get("ascent")
                .and_then(Value::as_i64)
                .ok_or_else(|| FontError::Json("bitmap missing \"ascent\"".into()))?
                as i32;
            let chars_val = obj
                .get("chars")
                .and_then(Value::as_array)
                .ok_or_else(|| FontError::Json("bitmap missing \"chars\"".into()))?;
            let mut chars = Vec::with_capacity(chars_val.len());
            for row in chars_val {
                let s = row.as_str().ok_or_else(|| {
                    FontError::Json("bitmap \"chars\" row is not a string".into())
                })?;
                chars.push(s.chars().map(|c| c as u32).collect::<Vec<u32>>());
            }
            ProviderDef::Bitmap {
                file,
                height,
                ascent,
                chars,
            }
        }
        "space" => {
            let advances_val = obj
                .get("advances")
                .and_then(Value::as_object)
                .ok_or_else(|| FontError::Json("space missing \"advances\"".into()))?;
            let mut advances = BTreeMap::new();
            for (k, v) in advances_val {
                if let Some(cp) = k.chars().next() {
                    let adv = v
                        .as_f64()
                        .ok_or_else(|| FontError::Json("space advance is not a number".into()))?;
                    advances.insert(cp as u32, adv as f32);
                }
            }
            ProviderDef::Space { advances }
        }
        "reference" => ProviderDef::Reference {
            id: parse_loc(obj.get("id"), "reference.id")?,
        },
        "ttf" => {
            let file = parse_loc(obj.get("file"), "ttf.file")?;
            let size = obj.get("size").and_then(Value::as_f64).unwrap_or(11.0) as f32;
            let oversample = obj.get("oversample").and_then(Value::as_f64).unwrap_or(1.0) as f32;
            let shift = obj
                .get("shift")
                .and_then(Value::as_array)
                .map(|a| {
                    let x = a.first().and_then(Value::as_f64).unwrap_or(0.0) as f32;
                    let y = a.get(1).and_then(Value::as_f64).unwrap_or(0.0) as f32;
                    [x, y]
                })
                .unwrap_or([0.0, 0.0]);
            let skip = match obj.get("skip") {
                Some(Value::String(s)) => s.chars().map(|c| c as u32).collect(),
                Some(Value::Array(a)) => a
                    .iter()
                    .filter_map(Value::as_str)
                    .flat_map(|s| s.chars().map(|c| c as u32).collect::<Vec<_>>())
                    .collect(),
                _ => Vec::new(),
            };
            ProviderDef::Ttf {
                file,
                size,
                oversample,
                shift,
                skip,
            }
        }
        "unihex" => {
            let hex_file = parse_loc(obj.get("hex_file"), "unihex.hex_file")?;
            let mut size_overrides = Vec::new();
            if let Some(arr) = obj.get("size_overrides").and_then(Value::as_array) {
                for o in arr {
                    let from = o
                        .get("from")
                        .and_then(Value::as_str)
                        .and_then(|s| s.chars().next())
                        .map(|c| c as u32);
                    let to = o
                        .get("to")
                        .and_then(Value::as_str)
                        .and_then(|s| s.chars().next())
                        .map(|c| c as u32);
                    let left = o.get("left").and_then(Value::as_i64);
                    let right = o.get("right").and_then(Value::as_i64);
                    if let (Some(from), Some(to), Some(left), Some(right)) = (from, to, left, right)
                    {
                        size_overrides.push(UnihexOverride {
                            from,
                            to,
                            left: left as i32,
                            right: right as i32,
                        });
                    }
                }
            }
            ProviderDef::Unihex {
                hex_file,
                size_overrides,
            }
        }
        other => return Err(FontError::Json(format!("unknown provider type {other:?}"))),
    };
    Ok(ConditionalProvider { def, filter })
}

fn parse_loc(value: Option<&Value>, field: &str) -> Result<ResourceLocation, FontError> {
    let s = value
        .and_then(Value::as_str)
        .ok_or_else(|| FontError::Json(format!("missing/invalid \"{field}\"")))?;
    Ok(ResourceLocation::parse(s)?)
}

/// A baked bitmap glyph: its advance plus where it lives in its source sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct BitmapGlyph {
    /// Advance width in logical pixels.
    pub advance: i32,
    /// Baseline ascent in logical pixels.
    pub ascent: i32,
    /// The source sheet texture (e.g. `minecraft:font/ascii.png`).
    pub file: ResourceLocation,
    /// The glyph cell within the sheet, `[x, y, w, h]` in physical pixels.
    pub cell: [u32; 4],
    /// Logical-to-physical scale (`height / cell_height`).
    pub pixel_scale: f32,
}

/// A resolved glyph.
#[derive(Debug, Clone, PartialEq)]
pub enum Glyph {
    /// A bitmap glyph with atlas placement.
    Bitmap(BitmapGlyph),
    /// A whitespace glyph carrying only an advance.
    Space {
        /// Advance width.
        advance: f32,
    },
}

impl Glyph {
    /// This glyph's advance width in logical pixels.
    pub fn advance(&self) -> f32 {
        match self {
            Glyph::Bitmap(g) => g.advance as f32,
            Glyph::Space { advance } => *advance,
        }
    }
}

/// A fully resolved font: a codepoint→glyph map with the winning provider chosen.
#[derive(Debug, Clone)]
pub struct Font {
    glyphs: HashMap<u32, Glyph>,
    provider_count: usize,
    missing_advance: f32,
}

impl Font {
    /// The glyph for `codepoint`, if any provider supplied it.
    pub fn glyph(&self, codepoint: u32) -> Option<&Glyph> {
        self.glyphs.get(&codepoint)
    }

    /// The bitmap glyph for `codepoint`, if it is a bitmap glyph.
    pub fn bitmap_glyph(&self, codepoint: u32) -> Option<&BitmapGlyph> {
        match self.glyphs.get(&codepoint) {
            Some(Glyph::Bitmap(g)) => Some(g),
            _ => None,
        }
    }

    /// The advance of `codepoint`, or `None` if it is not in the font.
    pub fn advance(&self, codepoint: u32) -> Option<f32> {
        self.glyphs.get(&codepoint).map(Glyph::advance)
    }

    /// The advance of `codepoint`, adding +1 when `bold`.
    pub fn advance_bold(&self, codepoint: u32, bold: bool) -> Option<f32> {
        self.advance(codepoint)
            .map(|a| a + if bold { 1.0 } else { 0.0 })
    }

    /// Whether the font contains `codepoint`.
    pub fn contains(&self, codepoint: u32) -> bool {
        self.glyphs.contains_key(&codepoint)
    }

    /// The number of distinct codepoints covered.
    pub fn codepoint_count(&self) -> usize {
        self.glyphs.len()
    }

    /// The number of active (filtered, non-reference) providers that
    /// contributed to this font.
    pub fn provider_count(&self) -> usize {
        self.provider_count
    }

    /// Iterates the covered codepoints.
    pub fn codepoints(&self) -> impl Iterator<Item = u32> + '_ {
        self.glyphs.keys().copied()
    }

    /// The width of a plain string (no formatting codes): the sum of glyph
    /// advances, using the missing-glyph advance for uncovered codepoints.
    pub fn string_width(&self, s: &str) -> f32 {
        s.chars()
            .map(|c| self.advance(c as u32).unwrap_or(self.missing_advance))
            .sum()
    }

    /// The width of a legacy `§`-coded string. `§`+code pairs contribute zero
    /// width; `§l` turns on bold (+1 per subsequent glyph); a colour code or
    /// `§r` resets bold. This is a convenience for measuring already-formatted
    /// legacy strings — structured component measurement should decompose to
    /// `(codepoint, bold)` and call [`Font::advance_bold`].
    pub fn legacy_width(&self, s: &str) -> f32 {
        let mut width = 0.0;
        let mut bold = false;
        let mut chars = s.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\u{00a7}' {
                if let Some(code) = chars.next() {
                    match code.to_ascii_lowercase() {
                        'l' => bold = true,
                        // Colours (0-9, a-f) and reset clear formatting.
                        '0'..='9' | 'a'..='f' | 'r' => bold = false,
                        // k (obfuscated), m/n/o (strike/underline/italic) don't
                        // change advance.
                        _ => {}
                    }
                }
                continue;
            }
            width += self.advance(c as u32).unwrap_or(self.missing_advance)
                + if bold { 1.0 } else { 0.0 };
        }
        width
    }
}

/// Loads and resolves fonts from a [`ResourceManager`].
#[derive(Debug)]
pub struct FontLoader<'a> {
    manager: &'a ResourceManager,
}

impl<'a> FontLoader<'a> {
    /// Creates a loader over the given pack stack.
    pub fn new(manager: &'a ResourceManager) -> Self {
        Self { manager }
    }

    /// Loads and resolves the font `id` under the given `options`.
    pub fn load(&self, id: &ResourceLocation, options: &FontOptions) -> Result<Font, FontError> {
        let mut active: Vec<ProviderDef> = Vec::new();
        let mut stack: Vec<ResourceLocation> = Vec::new();
        self.flatten(id, &FontFilter::always(), options, &mut stack, &mut active)?;

        let provider_count = active.len();
        let mut glyphs: HashMap<u32, Glyph> = HashMap::new();
        for def in &active {
            match def {
                ProviderDef::Space { advances } => {
                    for (cp, adv) in advances {
                        glyphs.entry(*cp).or_insert(Glyph::Space { advance: *adv });
                    }
                }
                ProviderDef::Bitmap {
                    file,
                    height,
                    ascent,
                    chars,
                } => {
                    self.load_bitmap(file, *height, *ascent, chars, &mut glyphs)?;
                }
                // Not rasterised here; they contribute no bitmap glyphs.
                ProviderDef::Ttf { .. } | ProviderDef::Unihex { .. } => {}
                ProviderDef::Reference { .. } => {}
            }
        }

        Ok(Font {
            glyphs,
            provider_count,
            missing_advance: MISSING_ADVANCE,
        })
    }

    fn flatten(
        &self,
        id: &ResourceLocation,
        inherited: &FontFilter,
        options: &FontOptions,
        stack: &mut Vec<ResourceLocation>,
        out: &mut Vec<ProviderDef>,
    ) -> Result<(), FontError> {
        if stack.contains(id) {
            return Err(FontError::ReferenceCycle { id: id.to_string() });
        }
        let bytes = self
            .manager
            .read_asset(id, "font", "json")
            .ok_or_else(|| FontError::NotFound { id: id.to_string() })?;
        let def = FontDefinition::parse(&bytes)?;
        stack.push(id.clone());
        for provider in def.providers {
            let effective = provider.filter.with_override(inherited);
            match provider.def {
                ProviderDef::Reference { id: ref_id } => {
                    self.flatten(&ref_id, &effective, options, stack, out)?;
                }
                other => {
                    if effective.passes(options) {
                        out.push(other);
                    }
                }
            }
        }
        stack.pop();
        Ok(())
    }

    fn load_bitmap(
        &self,
        file: &ResourceLocation,
        height: i32,
        ascent: i32,
        chars: &[Vec<u32>],
        glyphs: &mut HashMap<u32, Glyph>,
    ) -> Result<(), FontError> {
        let rows = chars.len();
        if rows == 0 || chars[0].is_empty() {
            return Err(FontError::InvalidGrid {
                reason: "empty codepoint grid".into(),
            });
        }
        let cols = chars[0].len();
        for row in chars {
            if row.len() != cols {
                return Err(FontError::InvalidGrid {
                    reason: "codepoint grid rows have differing lengths".into(),
                });
            }
        }
        let tex_path = format!("assets/{}/textures/{}", file.namespace(), file.path());
        let bytes = self
            .manager
            .read(&tex_path)
            .ok_or_else(|| FontError::MissingTexture {
                file: file.to_string(),
            })?;
        let img = Image::decode_png(&bytes)?;
        let glyph_width = img.width / cols as u32;
        let glyph_height = img.height / rows as u32;
        if glyph_width == 0 || glyph_height == 0 {
            return Err(FontError::InvalidGrid {
                reason: format!(
                    "sheet {}x{} too small for {cols}x{rows} grid",
                    img.width, img.height
                ),
            });
        }
        let pixel_scale = height as f32 / glyph_height as f32;

        for (slot_y, row) in chars.iter().enumerate() {
            for (slot_x, &cp) in row.iter().enumerate() {
                if cp == 0 {
                    continue; // null padding
                }
                if glyphs.contains_key(&cp) {
                    continue; // first-declared provider wins
                }
                let actual = actual_glyph_width(
                    &img,
                    glyph_width,
                    glyph_height,
                    slot_x as u32,
                    slot_y as u32,
                );
                let advance = (0.5 + actual as f32 * pixel_scale) as i32 + 1;
                glyphs.insert(
                    cp,
                    Glyph::Bitmap(BitmapGlyph {
                        advance,
                        ascent,
                        file: file.clone(),
                        cell: [
                            slot_x as u32 * glyph_width,
                            slot_y as u32 * glyph_height,
                            glyph_width,
                            glyph_height,
                        ],
                        pixel_scale,
                    }),
                );
            }
        }
        Ok(())
    }
}

/// Returns the width, in physical pixels, of the rightmost non-transparent
/// column of the glyph cell at `(slot_x, slot_y)` plus one (0 for a blank cell),
/// matching `BitmapProvider.getActualGlyphWidth`.
fn actual_glyph_width(
    img: &Image,
    glyph_width: u32,
    glyph_height: u32,
    slot_x: u32,
    slot_y: u32,
) -> u32 {
    for col in (0..glyph_width).rev() {
        let x = slot_x * glyph_width + col;
        for y in 0..glyph_height {
            let py = slot_y * glyph_height + y;
            if alpha_at(img, x, py) != 0 {
                return col + 1;
            }
        }
    }
    0
}

fn alpha_at(img: &Image, x: u32, y: u32) -> u8 {
    if x >= img.width || y >= img.height {
        return 0;
    }
    let idx = ((y * img.width + x) * 4 + 3) as usize;
    img.rgba[idx]
}
