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
//! - `unihex` — a zip of GNU Unifont `.hex` files, rasterised here; see
//!   [`UnihexBitmap`] for the line format and [`UnihexGlyph`] for the trimming
//!   rule. This is the broad fallback that covers CJK, Thai, Arabic, Hangul and
//!   most of the BMP — 114,432 codepoints in vanilla's 26.2 `unifont.zip`
//!   against 2,414 from the three bitmap sheets, which is why every codepoint
//!   outside those sheets used to draw the missing-glyph box.
//! - `ttf` — a TrueType/OpenType face, rasterised here with [`fontdue`]. Ported
//!   from `TrueTypeGlyphProviderDefinition`/`TrueTypeGlyphProvider` in the 26.2
//!   client: every codepoint the face's own `cmap` covers (minus `skip`) gets a
//!   [`TtfGlyph`], rendered at `pixelsPerEm = round(size * oversample)` the same
//!   way vanilla sets FreeType's pixel size, with `shift` applied as a plain
//!   post-hoc bearing offset (`+shiftX` horizontally, `-shiftY` vertically —
//!   vertical is negated because glyph-space Y grows up and screen-space Y
//!   grows down). Vanilla's `default.json` declares no `ttf` provider, so this
//!   affects resource packs rather than the vanilla font.
//!
//! # Where `unifont.zip` comes from
//!
//! **Not from `client.jar`.** The jar ships a 29-byte
//! `assets/minecraft/font/include/unifont.json` **stub with an empty
//! `providers` array**, and the real 3,993-byte one lives in the launcher's
//! asset-object store, as does `font/unifont.zip` itself (1,559,654 bytes). A
//! [`ResourceManager`] built from the jar alone therefore resolves the stub,
//! loads zero unihex providers, and reports success — see
//! `lodestone_shell::asset_objects` for the store reader and the rule it
//! encodes (*for any name present in both, prefer the object store*). A missing
//! `hex_file` is a **soft skip**, not an error, so a store-less run degrades to
//! exactly the pre-unihex behaviour instead of losing all text; use
//! [`Font::unihex_count`] to tell the two apart, because nothing else can.
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

/// Vanilla draw-side metrics the renderer needs to reproduce the game's text
/// exactly. These are *render constants*, not per-pack data — the asset side
/// exposes them by name so the shell/renderer don't hardcode magic numbers that
/// silently drift from vanilla. All values are in logical (GUI) pixels and match
/// `net.minecraft.client.gui.Font` in the 26.2 client.
pub mod metrics {
    /// Baseline-to-baseline line height (`Font.lineHeight`). Chat, tab list and
    /// tooltips advance by this per line.
    pub const LINE_HEIGHT: f32 = 9.0;
    /// Drop-shadow offset: the shadow copy is drawn `+1` px right and down.
    pub const SHADOW_OFFSET: f32 = 1.0;
    /// Drop-shadow brightness: the shadow colour is the text colour scaled to
    /// 25% (`* 0.25`), alpha preserved.
    pub const SHADOW_BRIGHTNESS: f32 = 0.25;
    /// Bold extra advance: bold text advances `+1` px per glyph (the glyph is
    /// also drawn a second time offset by this to thicken it).
    pub const BOLD_OFFSET: f32 = 1.0;
    /// Italic shear intercept: `BakedSheetGlyph.shearTop`/`shearBottom`
    /// are both `1.0F - 0.25F * v`, where `v`
    /// is a glyph edge's logical-pixel offset from the line's top (the same
    /// quantity [`super::GlyphRaster::top`] returns for the glyph's top edge).
    /// This is the `1.0F` term; [`ITALIC_SHEAR_SLOPE`] is the `0.25F` term. For
    /// the ascii sheet (`up = 0`, `down = 8`) that resolves to the top edge
    /// shifting `+1` px and the bottom edge shifting `-1` px — a 2 px lean
    /// across an 8 px-tall glyph, not a 1 px one.
    pub const ITALIC_SHEAR: f32 = 1.0;
    /// Italic shear slope: the `0.25F` multiplying `v` in
    /// `BakedSheetGlyph.shearTop`/`shearBottom`.
    /// A row at logical-pixel offset `v` from the line's top shears by
    /// `ITALIC_SHEAR - ITALIC_SHEAR_SLOPE * v`.
    pub const ITALIC_SHEAR_SLOPE: f32 = 0.25;
    /// Strikethrough bar: a 1 px-tall bar whose **bottom** edge sits this many
    /// logical px below the line's top (`Font.PreparedTextBuilder.accept`'s
    /// `y + 4.5F - 1.0F` .. `y + 4.5F`).
    pub const STRIKETHROUGH_Y: f32 = 4.5;
    /// Underline bar: a 1 px-tall bar whose **bottom** edge sits this many
    /// logical px below the line's top (`Font.PreparedTextBuilder.accept`'s
    /// `y + 9.0F - 1.0F` .. `y + 9.0F`).
    pub const UNDERLINE_Y: f32 = 9.0;
    /// Thickness of the underline/strikethrough bar, in logical px
    /// (`Font.PreparedTextBuilder.accept`: the bar spans exactly `1.0F`).
    pub const EFFECT_THICKNESS: f32 = 1.0;
    /// The underline/strikethrough bar for the **first** glyph of a run starts
    /// this many logical px to the left of that glyph's pen position
    /// (`Font.PreparedTextBuilder.accept`'s `effectX0 = position == 0 ? this.x - 1.0F : this.x`).
    /// Every later glyph's bar starts exactly at its own pen position.
    pub const EFFECT_LEAD_IN: f32 = 1.0;
    /// The bearing-top a glyph is measured against when placing its bitmap:
    /// `GlyphBitmap.getTop() == 7.0 - bearingTop`, and a bitmap glyph's
    /// `bearingTop` is its provider's `ascent`. So the ascii sheet (`ascent: 7`)
    /// puts its 8×8 cell flush with the line's top edge, and the accented sheet
    /// (`ascent: 10`) hangs 3 px above it. Straight from
    /// `com.mojang.blaze3d.font.GlyphBitmap` in the 26.2 client.
    pub const BEARING_TOP_BASE: f32 = 7.0;
}

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
                match (FontOption::from_key(k), v.as_bool()) {
                    (Some(opt), Some(b)) => {
                        conditions.insert(opt, b);
                    }
                    _ => {
                        // Never silently drop a filter condition: an
                        // unrecognised key (a `FontOption` this client does
                        // not know) or a non-boolean value both currently
                        // vanished with no trace, and a provider gated on
                        // one would then be silently treated as
                        // always-active instead of correctly conditional.
                        eprintln!(
                            "lodestone-assets: font filter condition {k:?}={v:?} is not \
                             a recognised option (or not a boolean) -- ignoring it, so \
                             this provider may be active when it should not be"
                        );
                    }
                }
            }
        }
        FontFilter { conditions }
    }
}

/// Whether the per-glyph bitmap-font metrics dump is enabled
/// (`LODESTONE_FONT_METRICS`, any value turns it on). See
/// [`FontLoader::load_bitmap`] for what it prints and why: this is the tool
/// for "the spacing between some glyphs is wrong and I can't tell why" —
/// every bitmap glyph's declared `height`/`ascent`, its sheet cell, the
/// derived `pixel_scale`, the measured ink width and the resulting
/// advance/drawn extent, all on one stderr line, so a pack-font spacing bug
/// is a `grep` of captured output rather than a source-diving exercise.
/// Checked once and cached: this can fire thousands of times for a large
/// sheet, so it must not cost a syscall per glyph.
fn font_metrics_debug_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("LODESTONE_FONT_METRICS").is_some())
}

/// The codepoints `LODESTONE_FONT_TRACE` names (comma/whitespace-separated;
/// each token `0x7532`, `U+7532`, or a bare decimal). Empty when the env var
/// is unset or unparseable.
///
/// This is the direct answer to "which provider actually supplies this
/// codepoint, and what did each candidate compute for its advance": every
/// provider that reaches a traced codepoint, in the *same* priority order
/// [`FontLoader::flatten`] produced, prints one line -- WINS for the one
/// [`Font::advance`] actually returns, "shadowed" for every other provider
/// that also declares the codepoint but lost. Two providers can each be
/// individually correct (a pack's own bitmap panel *and* vanilla's own
/// unihex CJK fallback both computing the textbook-correct advance for their
/// own cell) and still produce the wrong glyph on screen if the wrong one
/// wins the precedence race -- this is the tool for reading that race
/// directly, rather than inferring it from a metrics dump that only shows
/// one provider's own numbers in isolation.
fn font_trace_codepoints() -> &'static HashSet<u32> {
    static TRACE: std::sync::OnceLock<HashSet<u32>> = std::sync::OnceLock::new();
    TRACE.get_or_init(|| {
        let Some(raw) = std::env::var_os("LODESTONE_FONT_TRACE") else {
            return HashSet::new();
        };
        let raw = raw.to_string_lossy();
        raw.split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .filter_map(|token| {
                let hex = token
                    .strip_prefix("0x")
                    .or_else(|| token.strip_prefix("0X"))
                    .or_else(|| token.strip_prefix("U+"))
                    .or_else(|| token.strip_prefix("u+"));
                match hex {
                    Some(h) => u32::from_str_radix(h, 16).ok(),
                    None => token
                        .parse::<u32>()
                        .ok()
                        .or_else(|| u32::from_str_radix(token, 16).ok()),
                }
            })
            .collect()
    })
}

/// Number of bitmap rows in every unihex glyph (`UnihexProvider.GLYPH_HEIGHT`).
/// Fixed: the HEX line's digit count varies the *width*, never the height.
pub const UNIHEX_GLYPH_HEIGHT: u32 = 16;

/// Source-texels-per-logical-pixel for a unihex glyph
/// (`GlyphBitmap.getOversample` on `UnihexProvider.Glyph`'s bitmap). A unihex
/// glyph is 16 rows tall and draws 8 logical pixels tall, which is what puts it
/// on the same baseline as the 8 px ascii sheet.
pub const UNIHEX_OVERSAMPLE: f32 = 2.0;

/// A unihex glyph's bearing-top, which `GlyphBitmap.getBearingTop`'s default
/// supplies (`UnihexProvider.Glyph`'s bitmap overrides neither bearing). With
/// [`metrics::BEARING_TOP_BASE`] also 7.0 this puts the glyph box's top edge
/// flush with the line's top, exactly like the ascii sheet's `ascent: 7`.
const UNIHEX_ASCENT: i32 = 7;

/// One unihex glyph's 16 rows of bits, as read from a GNU Unifont HEX line.
///
/// # The line format, from `UnihexProvider.readFromStream`
///
/// ```text
/// 2713:00000000010102024444282810100000
/// 4E2D:01000100010001003FF8210821082108210821083FF821080100010001000100
/// ```
///
/// A codepoint field of **4, 5 or 6** hex digits, a `:`, then the bitmap as hex
/// digits, then a newline. The bitmap's **digit count is the glyph's width** and
/// only four counts are legal — this is the fact that makes a fixed stride
/// wrong:
///
/// | digits | bit width | reader |
/// |---|---|---|
/// | 32 | 8 (half-width) | `ByteContents.read` |
/// | 64 | 16 (full-width) | `ShortContents.read` |
/// | 96 | 24 | `IntContents.read24` |
/// | 128 | 32 | `IntContents.read32` |
///
/// Every one of the four is 16 rows; `digits / 16` hex digits per row, most
/// significant digit first, so the row's leftmost pixel is its most significant
/// bit. Vanilla normalises all four to a 32-bit row **left-aligned in the
/// word** — `ByteContents.line` returns `contents[i] << 24`,
/// `ShortContents.line` `<< 16`, `read24` stores `v << 8`, `read32` stores `v` —
/// and [`rows`](Self::rows) holds exactly that, so bit 31 is always column 0
/// regardless of width. Keeping the alignment rather than the raw width is what
/// lets one trimming rule serve all four.
///
/// Vanilla's 26.2 `unifont.zip` uses only the 32- and 64-digit forms (12,582 and
/// 101,850 entries), so the 24- and 32-pixel arms are exercised by fixture
/// rather than by that file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnihexBitmap {
    rows: [u32; UNIHEX_GLYPH_HEIGHT as usize],
    bit_width: u32,
}

impl UnihexBitmap {
    /// The 16 rows, each left-aligned in its `u32` (bit 31 = column 0).
    #[must_use]
    pub fn rows(&self) -> &[u32; UNIHEX_GLYPH_HEIGHT as usize] {
        &self.rows
    }

    /// The declared bit width: 8, 16, 24 or 32 (`LineData::bitWidth`).
    #[must_use]
    pub fn bit_width(&self) -> u32 {
        self.bit_width
    }

    /// The OR of all 16 rows (`LineData::mask`) — every column that is ink
    /// anywhere in the glyph.
    #[must_use]
    pub fn mask(&self) -> u32 {
        self.rows.iter().fold(0, |acc, row| acc | row)
    }

    /// `(left, right)` column bounds derived from the ink, per
    /// `LineData::calculateWidth`.
    ///
    /// Both are **bit indices from the left**, not from the LSB: `left =
    /// numberOfLeadingZeros(mask)` and `right = 32 -
    /// numberOfTrailingZeros(mask) - 1`.
    ///
    /// The all-empty case is the one worth spelling out because it is *not*
    /// `(0, bit_width - 1)`: vanilla returns `left = 0, right = bitWidth`, one
    /// past the last real column, so a blank 8-wide glyph is 9 columns wide and
    /// advances 5.5 rather than 5.0. (Vanilla round-trips the pair through
    /// `Dimensions.pack`, which masks each to a byte and re-reads it *signed*;
    /// for the 0..=32 this can produce that is the identity, so the pack step is
    /// omitted here.)
    #[must_use]
    pub fn derived_bounds(&self) -> (i32, i32) {
        let mask = self.mask();
        if mask == 0 {
            (0, self.bit_width as i32)
        } else {
            (
                mask.leading_zeros() as i32,
                32 - mask.trailing_zeros() as i32 - 1,
            )
        }
    }
}

/// Reads a GNU Unifont `.hex` payload, calling `out` once per entry.
///
/// A faithful port of `UnihexProvider.readFromStream`, with two deliberate
/// relaxations: a `\r` before the newline is dropped (vanilla would count it as
/// a digit and throw), and a wholly blank line is skipped (vanilla would fold
/// its newline into the next entry's codepoint field and throw). Both make a
/// CRLF-mangled pack load instead of failing; neither can change a glyph.
///
/// # Errors
///
/// [`FontError::Hex`] for a codepoint field that is not 4–6 hex digits, a bitmap
/// field whose digit count is not 32/64/96/128, or any non-hex digit — the same
/// three rejections vanilla makes, and for the same reason: a mis-sized field
/// silently reinterprets the whole glyph.
pub fn read_hex_entries(
    bytes: &[u8],
    mut out: impl FnMut(u32, UnihexBitmap),
) -> Result<usize, FontError> {
    let mut count = 0usize;
    for (index, raw) in bytes.split(|b| *b == b'\n').enumerate() {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if line.is_empty() {
            continue;
        }
        let colon = line
            .iter()
            .position(|b| *b == b':')
            .ok_or_else(|| hex_err(index, "expected 4, 5 or 6 hex digits followed by a colon"))?;
        let (cp_digits, data) = (&line[..colon], &line[colon + 1..]);
        if !matches!(cp_digits.len(), 4 | 5 | 6) {
            return Err(hex_err(
                index,
                "expected 4, 5 or 6 hex digits followed by a colon",
            ));
        }
        let mut codepoint = 0u32;
        for b in cp_digits {
            codepoint = codepoint << 4 | u32::from(decode_hex(index, *b)?);
        }
        // `digits / 16` digits per row; the shift left-aligns the row in the
        // word so bit 31 is column 0 for every width.
        let (per_row, bit_width) = match data.len() {
            32 => (2u32, 8u32),
            64 => (4, 16),
            96 => (6, 24),
            128 => (8, 32),
            _ => {
                return Err(hex_err(
                    index,
                    "expected hex number describing (8,16,24,32) x 16 bitmap, \
                     followed by a new line",
                ));
            }
        };
        let shift = 32 - bit_width;
        let mut rows = [0u32; UNIHEX_GLYPH_HEIGHT as usize];
        let mut pos = 0usize;
        for row in &mut rows {
            let mut value = 0u32;
            for _ in 0..per_row {
                value = value << 4 | u32::from(decode_hex(index, data[pos])?);
                pos += 1;
            }
            *row = value << shift;
        }
        out(codepoint, UnihexBitmap { rows, bit_width });
        count += 1;
    }
    Ok(count)
}

fn hex_err(index: usize, reason: &str) -> FontError {
    FontError::Hex {
        reason: format!("invalid entry at entry {index}: {reason}"),
    }
}

fn decode_hex(index: usize, b: u8) -> Result<u8, FontError> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        // Vanilla's `decodeHex` accepts uppercase only; lowercase is accepted
        // here because it cannot be ambiguous and a hand-written pack may use
        // it. Vanilla's own files are uppercase.
        b'a'..=b'f' => Ok(b - b'a' + 10),
        other => Err(hex_err(
            index,
            &format!("expected hex digit, got {:?}", other as char),
        )),
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
    /// A TrueType/OpenType font, rasterised by [`FontLoader::load_ttf`].
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

/// A baked unihex glyph: its 16 rows plus the column bounds that trim it.
///
/// # The trimming rule, from `UnihexProvider.Glyph`
///
/// `left`/`right` are inclusive column bounds and come from **one of two
/// places**, never both:
///
/// * a `size_overrides` range containing the codepoint — applied *first*, in
///   declaration order, and vanilla `remove`s the codepoint from the pending map
///   as it goes, so the earliest matching range wins and a later one cannot
///   reclaim it;
/// * otherwise [`UnihexBitmap::derived_bounds`], i.e. the ink's own extent.
///
/// Everything else follows: `width = right - left + 1`, `advance = width / 2 +
/// 1`, the bitmap is `width` columns by 16 rows at oversample 2, and the drawn
/// box is therefore `width / 2` × 8 logical pixels with its top edge on the
/// line's top.
///
/// The override is not cosmetic. `U+4E2D` 中 has ink from column 2 to column 12,
/// so its *derived* advance would be 6.5; the `3200`–`9FFF` override forces
/// `left = 0, right = 15` and an advance of **9.0**, which is what makes CJK
/// monospaced against the 9 px full-width cell instead of ragged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnihexGlyph {
    /// The 16 rows of bits.
    pub bitmap: UnihexBitmap,
    /// Inclusive left column bound.
    pub left: i32,
    /// Inclusive right column bound.
    pub right: i32,
}

impl UnihexGlyph {
    /// Column count, `right - left + 1` (`UnihexProvider.Glyph.width`).
    #[must_use]
    pub fn width(&self) -> i32 {
        self.right - self.left + 1
    }

    /// Advance in logical pixels, `width / 2 + 1` — the anonymous
    /// `GlyphInfo.getAdvance` in `UnihexProvider.Glyph.info`. Half-integral for
    /// an odd width, which is why [`Glyph::advance`] is `f32` and not `i32`.
    #[must_use]
    pub fn advance(&self) -> f32 {
        self.width() as f32 / UNIHEX_OVERSAMPLE + 1.0
    }

    /// Whether the texel at `(tx, ty)` of the trimmed bitmap is ink.
    ///
    /// `tx` walks the *trimmed* columns, `0..width()`. Vanilla's
    /// `unpackBitsToBytes` emits one texel per bit index from `32 - left - 1`
    /// down to `32 - right - 1`, writing 0 for any index outside `0..32` — so a
    /// `size_overrides` bound wider than the source (or negative) pads with
    /// blank columns rather than reading a neighbour's bits. That guard is
    /// load-bearing for the CJK ranges: they force `right = 15` on glyphs whose
    /// own ink stops earlier, and on an 8-bit-wide row `right = 15` addresses
    /// bits that were never in the file.
    #[must_use]
    pub fn is_ink(&self, tx: u32, ty: u32) -> bool {
        if ty >= UNIHEX_GLYPH_HEIGHT || i64::from(tx) >= i64::from(self.width()) {
            return false;
        }
        let bit = 32 - self.left - 1 - tx as i32;
        if !(0..32).contains(&bit) {
            return false;
        }
        (self.bitmap.rows()[ty as usize] >> bit) & 1 != 0
    }
}

/// A baked `ttf` glyph: enough to place it and re-rasterise its bitmap on
/// demand from its face.
///
/// Faithful to `com.mojang.blaze3d.font.TrueTypeGlyphProvider`: FreeType (here,
/// [`fontdue`]) renders every glyph at `pixelsPerEm = round(size * oversample)`,
/// and every logical-pixel field below has already been divided by
/// `oversample` — and had `shift` folded in — the way
/// `TrueTypeGlyphProvider.Glyph`'s constructor computes `bearingX`/`bearingY`
/// from FreeType's `left`/`top` (`bearingX = left / oversample`, `bearingY =
/// top / oversample`, with the shift already baked into `left`/`top` by
/// FreeType's own `FT_Set_Transform`). This port applies the shift itself
/// instead, since [`fontdue`] has no transform hook: `bearing_left = xmin /
/// oversample + shift.x`, `bearing_top = (ymin + height) / oversample -
/// shift.y` — the `-shift.y` because FreeType's `transformY` is `-shiftY *
/// oversample` (glyph space grows up, screen space grows down).
#[derive(Debug, Clone, PartialEq)]
pub struct TtfGlyph {
    /// The source `.ttf`/`.otf` file, keying into [`RasterFont`]'s resident
    /// faces the same way [`BitmapGlyph::file`] keys into its sheets.
    pub file: ResourceLocation,
    /// This face's own glyph index (`FT_Get_Char_Index`'s result) — the key
    /// [`fontdue::Font::rasterize_indexed`] rasterises by, not the codepoint.
    pub glyph_index: u16,
    /// `round(size * oversample)` — the pixel size every rasterisation call
    /// for this glyph uses (`FT_Set_Pixel_Sizes`'s argument).
    pub pixels_per_em: f32,
    /// This provider's oversample factor (`GlyphBitmap.getOversample()`).
    pub oversample: f32,
    /// Advance in logical pixels (`scaledAdvance / oversample`).
    pub advance: f32,
    /// Left bearing in logical pixels — `GlyphBitmap.getBearingLeft()`.
    pub bearing_left: f32,
    /// Top bearing in logical pixels — `GlyphBitmap.getBearingTop()`, the
    /// quantity [`GlyphRaster::top`]'s `7.0 - bearingTop` formula consumes.
    pub bearing_top: f32,
    /// Rasterised bitmap width, in oversampled (source) pixels.
    pub width: u32,
    /// Rasterised bitmap height, in oversampled (source) pixels.
    pub height: u32,
}

/// A resolved glyph.
#[derive(Debug, Clone, PartialEq)]
pub enum Glyph {
    /// A bitmap glyph with atlas placement.
    Bitmap(BitmapGlyph),
    /// A unihex glyph carrying its own 16 rows of bits.
    Unihex(UnihexGlyph),
    /// A TrueType/OpenType glyph, rasterised on demand from its face.
    Ttf(TtfGlyph),
    /// A whitespace-only glyph carrying only an advance: either an explicit
    /// `space` provider entry, or a `ttf` glyph whose rasterised bitmap came
    /// back zero-area (`TrueTypeGlyphProvider.loadGlyph`'s `EmptyGlyph` case —
    /// vanilla still gives it a real advance, just no bitmap to bake).
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
            Glyph::Unihex(g) => g.advance(),
            Glyph::Ttf(g) => g.advance,
            Glyph::Space { advance } => *advance,
        }
    }

    /// The extra advance this glyph gains when bold, and the distance its bold
    /// second pass is offset by (`GlyphInfo.getBoldOffset`).
    ///
    /// **1 px for everything except a unihex glyph, which is 0.5.** Vanilla puts
    /// this on `GlyphInfo` per glyph, not on the font: a unihex glyph is drawn at
    /// oversample 2, so its bold pass shifts half a logical pixel — one source
    /// texel — where a sheet glyph's shifts a whole one. Reading
    /// [`metrics::BOLD_OFFSET`] for every codepoint instead would make bold CJK
    /// measure 0.5 px per glyph too wide.
    pub fn bold_offset(&self) -> f32 {
        match self {
            Glyph::Unihex(_) => 1.0 / UNIHEX_OVERSAMPLE,
            _ => metrics::BOLD_OFFSET,
        }
    }

    /// How far this glyph's drop shadow is offset (`GlyphInfo.getShadowOffset`):
    /// 1 px, or 0.5 for a unihex glyph, for the same oversample reason as
    /// [`bold_offset`](Self::bold_offset).
    pub fn shadow_offset(&self) -> f32 {
        match self {
            Glyph::Unihex(_) => 1.0 / UNIHEX_OVERSAMPLE,
            _ => metrics::SHADOW_OFFSET,
        }
    }
}

/// A fully resolved font: a codepoint→glyph map with the winning provider chosen.
#[derive(Debug, Clone)]
pub struct Font {
    glyphs: HashMap<u32, Glyph>,
    provider_count: usize,
    unihex_count: usize,
    ttf_count: usize,
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

    /// The TTF glyph for `codepoint`, if it is one.
    pub fn ttf_glyph(&self, codepoint: u32) -> Option<&TtfGlyph> {
        match self.glyphs.get(&codepoint) {
            Some(Glyph::Ttf(g)) => Some(g),
            _ => None,
        }
    }

    /// The advance of `codepoint`, or `None` if it is not in the font.
    pub fn advance(&self, codepoint: u32) -> Option<f32> {
        self.glyphs.get(&codepoint).map(Glyph::advance)
    }

    /// The advance of `codepoint`, adding its own [`Glyph::bold_offset`] when
    /// `bold` (`GlyphInfo.getAdvance(boolean)`). That is +1 for a sheet glyph
    /// and +0.5 for a unihex one — per glyph, not per font.
    pub fn advance_bold(&self, codepoint: u32, bold: bool) -> Option<f32> {
        self.glyphs.get(&codepoint).map(|g| {
            g.advance() + if bold { g.bold_offset() } else { 0.0 }
        })
    }

    /// The bold extra-advance for `codepoint`, or [`metrics::BOLD_OFFSET`] when
    /// this font does not cover it (the missing-glyph box is a sheet glyph in
    /// vanilla, so it takes the 1 px default).
    pub fn bold_offset(&self, codepoint: u32) -> f32 {
        self.glyphs
            .get(&codepoint)
            .map_or(metrics::BOLD_OFFSET, Glyph::bold_offset)
    }

    /// The drop-shadow offset for `codepoint`, or [`metrics::SHADOW_OFFSET`]
    /// when this font does not cover it.
    pub fn shadow_offset(&self, codepoint: u32) -> f32 {
        self.glyphs
            .get(&codepoint)
            .map_or(metrics::SHADOW_OFFSET, Glyph::shadow_offset)
    }

    /// The unihex glyph for `codepoint`, if it is one.
    pub fn unihex_glyph(&self, codepoint: u32) -> Option<&UnihexGlyph> {
        match self.glyphs.get(&codepoint) {
            Some(Glyph::Unihex(g)) => Some(g),
            _ => None,
        }
    }

    /// How many codepoints a `unihex` provider **won**.
    ///
    /// This exists because a missing `hex_file` is a soft skip: with the
    /// asset-object store absent, `font/include/unifont.json` resolves to the
    /// jar's empty stub and this is `0` while everything else about the load
    /// looks healthy. It is the one number that separates "unihex is off" from
    /// "unihex is on", so log it rather than the codepoint total — the total
    /// moves for a dozen other reasons.
    pub fn unihex_count(&self) -> usize {
        self.unihex_count
    }

    /// How many codepoints a `ttf` provider **won** (including zero-area
    /// glyphs that resolved to [`Glyph::Space`] — they still came from the
    /// face's own `cmap`, unlike an explicit `space` provider entry). A
    /// `ttf` file no pack supplies is a soft skip returning `0` from that
    /// provider, for the same reason as [`Font::unihex_count`]'s missing
    /// `hex_file`.
    pub fn ttf_count(&self) -> usize {
        self.ttf_count
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
    /// width; `§l` turns on bold (each glyph's own [`Glyph::bold_offset`] per
    /// subsequent glyph); a colour code or
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
                + if bold { self.bold_offset(c as u32) } else { 0.0 };
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
        let mut unihex_count = 0usize;
        let mut ttf_count = 0usize;
        let traced = font_trace_codepoints();
        for (provider_index, def) in active.iter().enumerate() {
            match def {
                ProviderDef::Unihex {
                    hex_file,
                    size_overrides,
                } => {
                    unihex_count += self.load_unihex(
                        hex_file,
                        size_overrides,
                        &mut glyphs,
                        provider_index,
                        traced,
                        id,
                    )?;
                }
                ProviderDef::Space { advances } => {
                    for (cp, adv) in advances {
                        let already = glyphs.contains_key(cp);
                        if traced.contains(cp) {
                            eprintln!(
                                "lodestone-assets: TRACE font={id} U+{cp:04X} provider[{provider_index}] \
                                 space advance={adv} -> {}",
                                if already {
                                    "shadowed (an earlier-declared provider already won this codepoint)"
                                } else {
                                    "WINS"
                                }
                            );
                        }
                        glyphs.entry(*cp).or_insert(Glyph::Space { advance: *adv });
                    }
                }
                ProviderDef::Bitmap {
                    file,
                    height,
                    ascent,
                    chars,
                } => {
                    self.load_bitmap(
                        file,
                        *height,
                        *ascent,
                        chars,
                        &mut glyphs,
                        provider_index,
                        traced,
                        id,
                    )?;
                }
                ProviderDef::Ttf {
                    file,
                    size,
                    oversample,
                    shift,
                    skip,
                } => {
                    ttf_count += self.load_ttf(
                        file,
                        *size,
                        *oversample,
                        *shift,
                        skip,
                        &mut glyphs,
                        provider_index,
                        traced,
                        id,
                    )?;
                }
                ProviderDef::Reference { .. } => {}
            }
        }

        Ok(Font {
            glyphs,
            provider_count,
            unihex_count,
            ttf_count,
            missing_advance: MISSING_ADVANCE,
        })
    }

    /// Reads a `unihex` provider's zip and inserts every glyph it wins.
    ///
    /// Returns how many codepoints this provider contributed — **not** how many
    /// the file holds. A codepoint an earlier (higher-priority) provider already
    /// supplied is skipped, so for vanilla's `default.json` this is
    /// `unifont.zip`'s entry count minus the 2,414 the three bitmap sheets and
    /// the `space` provider took first.
    ///
    /// A `hex_file` no pack supplies is a **soft skip returning 0**, not an
    /// error, and the reason is the failure mode on the other side: making it
    /// fatal turns a store-less install from "CJK draws the missing-glyph box,
    /// as it did before" into "`FontLoader::load` errors and the HUD falls back
    /// to a hand-drawn 5×7 font", i.e. every glyph in the game changes because
    /// one optional 1.5 MB download is absent. Malformed *contents* still fail
    /// loudly — that is a broken pack, not an absent one.
    fn load_unihex(
        &self,
        hex_file: &ResourceLocation,
        size_overrides: &[UnihexOverride],
        glyphs: &mut HashMap<u32, Glyph>,
        provider_index: usize,
        traced: &HashSet<u32>,
        font_id: &ResourceLocation,
    ) -> Result<usize, FontError> {
        let path = format!("assets/{}/{}", hex_file.namespace(), hex_file.path());
        let Some(bytes) = self.manager.read(&path) else {
            // A deliberate soft skip (see this method's own doc), but never
            // a *silent* one: without this the CJK/Thai/Arabic fallback
            // vanishing looks identical to it never having been declared.
            eprintln!(
                "lodestone-assets: unihex hex_file {path:?} not found -- skipping this \
                 provider (0 codepoints from it; its glyphs fall through to the next \
                 provider or the missing-glyph box)"
            );
            return Ok(0);
        };

        // Vanilla applies `size_overrides` first, `remove`-ing each codepoint
        // from the pending map, then derives bounds for whatever is left. Both
        // halves have to see the same pending map, so the bits are collected
        // first and resolved after — a one-pass "look up the override as I read"
        // would be equivalent only because the ranges here do not overlap, and
        // is exactly the kind of coincidence that stops holding when a pack
        // declares two ranges that do.
        let mut bits: BTreeMap<u32, UnihexBitmap> = BTreeMap::new();
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).map_err(|e| {
            FontError::HexArchive {
                file: hex_file.to_string(),
                reason: e.to_string(),
            }
        })?;
        for i in 0..archive.len() {
            let mut entry = archive
                .by_index(i)
                .map_err(|e| FontError::HexArchive {
                    file: hex_file.to_string(),
                    reason: e.to_string(),
                })?;
            if !entry.name().ends_with(".hex") {
                continue;
            }
            let mut raw = Vec::with_capacity(entry.size() as usize);
            std::io::Read::read_to_end(&mut entry, &mut raw).map_err(|e| {
                FontError::HexArchive {
                    file: hex_file.to_string(),
                    reason: e.to_string(),
                }
            })?;
            read_hex_entries(&raw, |cp, bitmap| {
                bits.insert(cp, bitmap);
            })?;
        }

        let mut inserted = 0usize;
        let mut insert = |cp: u32, glyph: UnihexGlyph, inserted: &mut usize| {
            let already = glyphs.contains_key(&cp);
            if traced.contains(&cp) {
                eprintln!(
                    "lodestone-assets: TRACE font={font_id} U+{cp:04X} provider[{provider_index}] unihex hex_file={hex_file} advance={:.3} -> {}",
                    glyph.advance(),
                    if already {
                        "shadowed (an earlier-declared provider already won this codepoint)"
                    } else {
                        "WINS"
                    }
                );
            }
            if already {
                return; // first-declared provider wins
            }
            glyphs.insert(cp, Glyph::Unihex(glyph));
            *inserted += 1;
        };
        for over in size_overrides {
            for cp in over.from..=over.to {
                if let Some(bitmap) = bits.remove(&cp) {
                    insert(
                        cp,
                        UnihexGlyph {
                            bitmap,
                            left: over.left,
                            right: over.right,
                        },
                        &mut inserted,
                    );
                }
            }
        }
        for (cp, bitmap) in bits {
            let (left, right) = bitmap.derived_bounds();
            insert(
                cp,
                UnihexGlyph {
                    bitmap,
                    left,
                    right,
                },
                &mut inserted,
            );
        }
        Ok(inserted)
    }

    /// Reads a `ttf` provider's font file and inserts every glyph its own
    /// `cmap` supplies (minus `skip`) that no earlier provider already won.
    ///
    /// Returns how many codepoints this provider contributed. A `file` no pack
    /// supplies is a **soft skip returning 0**, not an error — the same
    /// reasoning as [`FontLoader::load_unihex`]'s absent `hex_file`: a `ttf`
    /// provider is a resource-pack addition, never vanilla's own `default.json`,
    /// so an absent file should degrade to "this pack's extra glyphs are
    /// missing" rather than failing the whole font. Malformed *contents* (a
    /// file present but not a font [`fontdue`] can parse) still fail loudly.
    #[allow(clippy::too_many_arguments)]
    fn load_ttf(
        &self,
        file: &ResourceLocation,
        size: f32,
        oversample: f32,
        shift: [f32; 2],
        skip: &[u32],
        glyphs: &mut HashMap<u32, Glyph>,
        provider_index: usize,
        traced: &HashSet<u32>,
        font_id: &ResourceLocation,
    ) -> Result<usize, FontError> {
        // `TrueTypeGlyphProviderDefinition.load`: `resourceManager.open(this.location.withPrefix("font/"))`.
        let path = format!("assets/{}/font/{}", file.namespace(), file.path());
        let Some(bytes) = self.manager.read(&path) else {
            // Soft skip, deliberately (see this method's own doc) -- but
            // never silent: a pack referencing a `ttf` file that is not
            // actually present should be visible in the log, not just as
            // "those codepoints fell back to something else".
            eprintln!(
                "lodestone-assets: ttf file {path:?} not found -- skipping this provider \
                 (0 codepoints from it)"
            );
            return Ok(0);
        };
        let face = fontdue::Font::from_bytes(bytes.as_slice(), fontdue::FontSettings::default())
            .map_err(|reason| FontError::TtfFont {
                file: file.to_string(),
                reason: reason.to_string(),
            })?;

        let skip_set: HashSet<u32> = skip.iter().copied().collect();
        // `int pixelsPerEm = Math.round(size * oversample);` — an integral
        // pixel size, exactly what `FT_Set_Pixel_Sizes` takes.
        let pixels_per_em = (size * oversample).round();

        let mut inserted = 0usize;
        for (&ch, &glyph_id) in face.chars() {
            let cp = ch as u32;
            let skip_cp = skip_set.contains(&cp);
            let already = glyphs.contains_key(&cp);
            if traced.contains(&cp) {
                let reason = if skip_cp {
                    "skipped (this provider's own \"skip\" list)"
                } else if already {
                    "shadowed (an earlier-declared provider already won this codepoint)"
                } else {
                    "WINS"
                };
                eprintln!(
                    "lodestone-assets: TRACE font={font_id} U+{cp:04X} provider[{provider_index}] ttf file={file} -> {reason}"
                );
            }
            if skip_cp || already {
                continue; // pack-declared skip, or an earlier provider already won it
            }
            let metrics = face.metrics_indexed(glyph_id.get(), pixels_per_em);
            let glyph = if metrics.width == 0 || metrics.height == 0 {
                // `TrueTypeGlyphProvider.loadGlyph`'s `EmptyGlyph` case: still a
                // real advance, no bitmap to bake.
                Glyph::Space {
                    advance: metrics.advance_width / oversample,
                }
            } else {
                Glyph::Ttf(TtfGlyph {
                    file: file.clone(),
                    glyph_index: glyph_id.get(),
                    pixels_per_em,
                    oversample,
                    advance: metrics.advance_width / oversample,
                    bearing_left: metrics.xmin as f32 / oversample + shift[0],
                    bearing_top: (metrics.ymin + metrics.height as i32) as f32 / oversample
                        - shift[1],
                    width: metrics.width as u32,
                    height: metrics.height as u32,
                })
            };
            glyphs.insert(cp, glyph);
            inserted += 1;
        }
        Ok(inserted)
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
        // Vanilla stacks every active pack's own copy of a font definition
        // file for this id -- it is not a single-winner override the way an
        // ordinary texture reference is. `FontManager.prepare` reads via
        // `FONT_DEFINITIONS.listMatchingResourceStacks`, one entry per pack
        // that carries the path, and `loadResourceStack` (reverse per file)
        // plus `apply`'s final `Lists.reverse` combine to lay the merged
        // list out **highest-priority pack first, each pack's own JSON
        // order preserved** -- the exact shape `ResourceManager::read_stack`
        // already documents for language files ("a pack that ships its own
        // lang/en_us.json must not blank out the ~7,000 vanilla keys
        // underneath it"). A font id is the same shape: a pack that ships
        // its own `font/<id>.json` to add a handful of custom bitmap
        // providers must not silently delete every lower-priority pack's
        // (including the jar's) own providers for that id -- which is
        // exactly what a single-winner `read_asset` did here before this
        // fix, discarding the jar's `minecraft:default.json` outright
        // whenever a pack shipped its own file at the same path.
        let path = crate::manager::ResourceManager::asset_path(id, "font", "json");
        let layers = self.manager.read_stack(&path);
        if layers.is_empty() {
            return Err(FontError::NotFound { id: id.to_string() });
        }
        stack.push(id.clone());
        // `read_stack` returns lowest priority first; the higher-priority
        // pack's own providers must come first in the flattened,
        // priority-ordered list, so walk it highest-first.
        for bytes in layers.iter().rev() {
            let def = FontDefinition::parse(bytes)?;
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
        provider_index: usize,
        traced: &HashSet<u32>,
        font_id: &ResourceLocation,
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
        let debug = font_metrics_debug_enabled();
        if debug {
            eprintln!(
                "lodestone-assets: bitmap provider font={font_id} file={file} sheet={}x{} \
                 grid={cols}x{rows} cell={glyph_width}x{glyph_height}px declared_height={height} \
                 ascent={ascent} pixel_scale={pixel_scale}",
                img.width, img.height,
            );
        }

        // Codepoints this *provider's own* grid has already placed, tracked
        // separately from `glyphs` (which also carries every earlier
        // provider's winners). The two duplicate cases are opposite:
        // `BitmapProvider.Definition.load`'s `charMap.put` is a plain
        // overwrite within one provider's grid (the *last* declaration wins,
        // with a logged warning), but a codepoint an earlier provider in the
        // font's own priority list already won must never be reclaimed by a
        // later one. Checking `glyphs.contains_key` alone conflates the two
        // and made the *first* cell in this grid win a same-provider
        // collision instead of the last.
        let mut declared_here: HashSet<u32> = HashSet::new();
        for (slot_y, row) in chars.iter().enumerate() {
            for (slot_x, &cp) in row.iter().enumerate() {
                if cp == 0 {
                    continue; // null padding
                }
                let shadowed_by_earlier = glyphs.contains_key(&cp) && !declared_here.contains(&cp);
                let want_trace = traced.contains(&cp);
                if shadowed_by_earlier && !want_trace {
                    continue; // an earlier-declared provider already won this codepoint
                }
                // Computed for any *live* candidate, and also for a shadowed
                // one that is explicitly traced -- LODESTONE_FONT_TRACE's
                // whole point is showing what a losing provider *would have*
                // computed, not just the winner's own number.
                let actual = actual_glyph_width(
                    &img,
                    glyph_width,
                    glyph_height,
                    slot_x as u32,
                    slot_y as u32,
                );
                let advance = (0.5 + actual as f32 * pixel_scale) as i32 + 1;
                if want_trace {
                    eprintln!(
                        "lodestone-assets: TRACE font={font_id} U+{cp:04X} provider[{provider_index}] \
                         bitmap file={file} cell_pos=({slot_x},{slot_y}) advance={advance} -> {}",
                        if shadowed_by_earlier {
                            "shadowed (an earlier-declared provider already won this codepoint)"
                        } else {
                            "WINS"
                        }
                    );
                }
                if shadowed_by_earlier {
                    continue;
                }
                if !declared_here.insert(cp) {
                    eprintln!(
                        "lodestone-assets: codepoint U+{cp:04X} declared multiple times in \
                         {file} -- the later cell wins, matching \
                         BitmapProvider.Definition.load's charMap.put"
                    );
                }
                if debug {
                    // Everything a spacing bug needs to be diagnosed from one
                    // line: the declared metrics, the measured ink extent
                    // (`actual`, in the sheet's own physical texels), the
                    // advance that formula produces, and the *drawn* extent
                    // (`actual * pixel_scale`) -- the two a caller expects to
                    // agree modulo the vanilla "+1" gap. If `drawn_w` is
                    // ever close to or past `advance` for consecutive
                    // glyphs, that pair is the overlap.
                    eprintln!(
                        "lodestone-assets:   codepoint=U+{cp:04X} {:?} cell_pos=({slot_x},{slot_y}) \
                         ink_w={actual}px drawn_w={:.3} advance={advance}",
                        char::from_u32(cp),
                        actual as f32 * pixel_scale,
                    );
                }
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

/// A [`Font`] together with the decoded sheets its bitmap glyphs live in, so a
/// consumer can ask not just "how wide is this glyph" but "which texels of it
/// are ink".
///
/// [`Font`] alone is a *metrics* table: it knows each glyph's advance and which
/// cell of which sheet it occupies, but it holds no pixels, so nothing can draw
/// from it. That is exactly why the font machinery sat unconsumed — a renderer
/// that wants vanilla text needs coverage, and had to re-open and re-decode the
/// sheets itself. `RasterFont` closes that gap and is the type a HUD actually
/// holds.
///
/// It is deliberately **GPU-free**: coverage is exposed as a per-texel predicate
/// in *logical* (GUI) pixel space via [`GlyphRaster`], so a renderer can either
/// blit the sheet or emit quads, and a test can assert on it with no adapter.
#[derive(Debug)]
pub struct RasterFont {
    font: Font,
    sheets: HashMap<ResourceLocation, Image>,
    ttf_faces: HashMap<ResourceLocation, fontdue::Font>,
}

impl RasterFont {
    /// The metrics half. Advances, coverage counts and `§`-aware widths all live
    /// here; [`RasterFont`] adds only the pixels.
    pub fn font(&self) -> &Font {
        &self.font
    }

    /// The number of distinct bitmap sheets whose pixels are resident.
    pub fn sheet_count(&self) -> usize {
        self.sheets.len()
    }

    /// The number of distinct `ttf` faces whose bytes are resident and parsed.
    pub fn ttf_face_count(&self) -> usize {
        self.ttf_faces.len()
    }

    /// The advance of `codepoint` in logical pixels, or `None` when the font
    /// does not cover it.
    pub fn advance(&self, codepoint: u32) -> Option<f32> {
        self.font.advance(codepoint)
    }

    /// The width of a plain string in logical pixels (see [`Font::string_width`]).
    pub fn string_width(&self, s: &str) -> f32 {
        self.font.string_width(s)
    }

    /// The width of a `§`-coded string in logical pixels (see
    /// [`Font::legacy_width`]).
    pub fn legacy_width(&self, s: &str) -> f32 {
        self.font.legacy_width(s)
    }

    /// The drawable raster for `codepoint`, or `None` for a whitespace-only or
    /// uncovered glyph (or one whose sheet/face failed to decode/resolve).
    ///
    /// A unihex glyph needs no sheet — it carries its own bits — so it resolves
    /// here whether or not any PNG decoded. A `ttf` glyph is rasterised here,
    /// on demand, from its resident face (`TrueTypeGlyphProvider.bake`'s
    /// equivalent — vanilla also bakes lazily, on first draw, rather than
    /// eagerly for every codepoint the face's `cmap` supplies).
    pub fn raster(&self, codepoint: u32) -> Option<GlyphRaster<'_>> {
        match self.font.glyph(codepoint)? {
            Glyph::Bitmap(glyph) => {
                let image = self.sheets.get(&glyph.file)?;
                Some(GlyphRaster {
                    kind: RasterKind::Sheet { glyph, image },
                })
            }
            Glyph::Unihex(glyph) => Some(GlyphRaster {
                kind: RasterKind::Unihex(glyph),
            }),
            Glyph::Ttf(glyph) => {
                let face = self.ttf_faces.get(&glyph.file)?;
                let (_, bitmap) = face.rasterize_indexed(glyph.glyph_index, glyph.pixels_per_em);
                Some(GlyphRaster {
                    kind: RasterKind::Ttf { glyph, bitmap },
                })
            }
            Glyph::Space { .. } => None,
        }
    }

    /// How many codepoints came from a `unihex` provider — see
    /// [`Font::unihex_count`], which is the number to log.
    pub fn unihex_count(&self) -> usize {
        self.font.unihex_count()
    }

    /// How many codepoints came from a `ttf` provider — see
    /// [`Font::ttf_count`].
    pub fn ttf_count(&self) -> usize {
        self.font.ttf_count()
    }
}

/// One glyph's pixels, addressed in *cell texels* and measured in logical pixels.
///
/// A renderer walks `0..cell_width()` × `0..cell_height()` and asks
/// [`GlyphRaster::is_ink`]; each lit texel covers a
/// [`texel_size`](GlyphRaster::texel_size)-square of logical pixels at
/// `(x + tx * texel_size, y + top() + ty * texel_size)`, where `(x, y)` is the
/// pen position and `y` is the **top of the line** (not the baseline).
///
/// Three provider families that produce pixels resolve to this one type — a
/// bitmap sheet cell, a unihex glyph's own 16 rows of bits, and a rasterised
/// `ttf` glyph — so a renderer written against it needs no branch on provider
/// kind. See [`RasterKind`].
///
/// Not [`Copy`]: a `ttf` glyph's bitmap is rasterised fresh into an owned
/// buffer each time [`RasterFont::raster`] is called (see [`RasterKind::Ttf`]),
/// so cloning this type would re-rasterise rather than share pixels.
#[derive(Debug, Clone)]
pub struct GlyphRaster<'a> {
    kind: RasterKind<'a>,
}

/// Which kind of glyph a [`GlyphRaster`] is reading.
///
/// This is private on purpose: the whole point is that a caller walking
/// `cell_width` × `cell_height` and asking [`GlyphRaster::is_ink`] does not have
/// to know. The HUD's quad emitter was written against the sheet case and drew
/// unihex glyphs correctly with **no change**, because every quantity it uses
/// (`texel_size`, `top`, `advance`, `cell_*`) has a unihex answer here; the same
/// held for `ttf` once [`GlyphRaster::left`] was added for its (usually
/// nonzero) left bearing, which the other two kinds don't need.
#[derive(Debug, Clone)]
enum RasterKind<'a> {
    Sheet {
        glyph: &'a BitmapGlyph,
        image: &'a Image,
    },
    Unihex(&'a UnihexGlyph),
    /// `bitmap` is `fontdue::Font::rasterize_indexed`'s coverage output,
    /// rasterised once per [`RasterFont::raster`] call (not once per texel —
    /// `is_ink` below indexes straight into it) and owned here rather than
    /// borrowed, since nothing else keeps a rasterised `ttf` bitmap resident.
    Ttf {
        glyph: &'a TtfGlyph,
        bitmap: Vec<u8>,
    },
}

/// Coverage-to-ink threshold for a `ttf` glyph. [`fontdue`] returns
/// antialiased 0..=255 coverage, but this renderer intentionally exposes TTF
/// texels as binary white or transparent, so its edges are binarised at roughly
/// half coverage rather than blended. This reproduces the glyph's *shape*, not
/// vanilla's antialiasing.
const TTF_INK_THRESHOLD: u8 = 127;

impl GlyphRaster<'_> {
    /// Cell width in source texels — the sheet cell's width, a unihex glyph's
    /// trimmed column count, or a `ttf` glyph's rasterised bitmap width.
    pub fn cell_width(&self) -> u32 {
        match &self.kind {
            RasterKind::Sheet { glyph, .. } => glyph.cell[2],
            RasterKind::Unihex(g) => g.width().max(0) as u32,
            RasterKind::Ttf { glyph, .. } => glyph.width,
        }
    }

    /// Cell height in source texels ([`UNIHEX_GLYPH_HEIGHT`] for a unihex
    /// glyph, always; a `ttf` glyph's rasterised bitmap height otherwise).
    pub fn cell_height(&self) -> u32 {
        match &self.kind {
            RasterKind::Sheet { glyph, .. } => glyph.cell[3],
            RasterKind::Unihex(_) => UNIHEX_GLYPH_HEIGHT,
            RasterKind::Ttf { glyph, .. } => glyph.height,
        }
    }

    /// The logical size of one source texel (vanilla's `1 / oversample`) — so
    /// 0.5 for a unihex glyph, which is what makes its 16 rows draw 8 logical
    /// pixels tall next to the ascii sheet's 8×8 at 1.0. A caller that assumed
    /// 1.0 here would draw every CJK glyph at double size. A `ttf` glyph uses
    /// its own provider's oversample the same way.
    pub fn texel_size(&self) -> f32 {
        match &self.kind {
            RasterKind::Sheet { glyph, .. } => glyph.pixel_scale,
            RasterKind::Unihex(_) => 1.0 / UNIHEX_OVERSAMPLE,
            RasterKind::Ttf { glyph, .. } => 1.0 / glyph.oversample,
        }
    }

    /// The glyph box's left edge relative to the pen position, in logical
    /// pixels — `GlyphBitmap.getBearingLeft()`. Zero for a sheet or unihex
    /// glyph (neither overrides the interface default); a `ttf` glyph's own
    /// [`TtfGlyph::bearing_left`] otherwise, since a TrueType outline is not
    /// generally flush with its advance box the way a bitmap-sheet cell is.
    pub fn left(&self) -> f32 {
        match &self.kind {
            RasterKind::Ttf { glyph, .. } => glyph.bearing_left,
            RasterKind::Sheet { .. } | RasterKind::Unihex(_) => 0.0,
        }
    }

    /// The glyph box's top edge relative to the line's top, in logical pixels:
    /// `7 - bearingTop`, per `GlyphBitmap.getTop`. Negative for tall sheets,
    /// which is how accented capitals hang above the ascii line. A unihex
    /// glyph overrides no bearing, so its bearing-top is
    /// `GlyphBitmap.getBearingTop`'s 7.0 default and this is 0. A `ttf`
    /// glyph's own [`TtfGlyph::bearing_top`] is the FreeType-equivalent
    /// bearing this same formula was always meant to consume.
    pub fn top(&self) -> f32 {
        let bearing_top = match &self.kind {
            RasterKind::Sheet { glyph, .. } => glyph.ascent as f32,
            RasterKind::Unihex(_) => UNIHEX_ASCENT as f32,
            RasterKind::Ttf { glyph, .. } => glyph.bearing_top,
        };
        metrics::BEARING_TOP_BASE - bearing_top
    }

    /// This glyph's advance in logical pixels.
    pub fn advance(&self) -> f32 {
        match &self.kind {
            RasterKind::Sheet { glyph, .. } => glyph.advance as f32,
            RasterKind::Unihex(g) => g.advance(),
            RasterKind::Ttf { glyph, .. } => glyph.advance,
        }
    }

    /// The source RGBA of the texel at `(tx, ty)` within the cell.
    ///
    /// Bitmap providers preserve the decoded PNG's native RGBA8 values, which
    /// lets resource-pack fonts use coloured glyphs and partial alpha. Unihex
    /// remains its native binary mask — opaque white ink or transparent — and
    /// TTF retains the existing [`TTF_INK_THRESHOLD`] binary behaviour rather
    /// than introducing antialiasing as a side effect of this accessor.
    #[must_use]
    pub fn texel_rgba(&self, tx: u32, ty: u32) -> [f32; 4] {
        match &self.kind {
            RasterKind::Sheet { glyph, image } => {
                if tx >= self.cell_width() || ty >= self.cell_height() {
                    return [0.0; 4];
                }
                let x = glyph.cell[0] + tx;
                let y = glyph.cell[1] + ty;
                if x >= image.width || y >= image.height {
                    return [0.0; 4];
                }
                let idx = ((y * image.width + x) * 4) as usize;
                image.rgba.get(idx..idx + 4).map_or([0.0; 4], |rgba| {
                    [
                        rgba[0] as f32 / 255.0,
                        rgba[1] as f32 / 255.0,
                        rgba[2] as f32 / 255.0,
                        rgba[3] as f32 / 255.0,
                    ]
                })
            }
            RasterKind::Unihex(g) => {
                if g.is_ink(tx, ty) {
                    [1.0; 4]
                } else {
                    [0.0; 4]
                }
            }
            RasterKind::Ttf { glyph, bitmap } => {
                if tx >= glyph.width || ty >= glyph.height {
                    return [0.0; 4];
                }
                let idx = (ty * glyph.width + tx) as usize;
                if bitmap.get(idx).is_some_and(|&c| c > TTF_INK_THRESHOLD) {
                    [1.0; 4]
                } else {
                    [0.0; 4]
                }
            }
        }
    }

    /// Whether the texel at `(tx, ty)` within the cell is ink.
    ///
    /// For a sheet glyph, vanilla's `getActualGlyphWidth` tests
    /// `getLuminanceOrAlpha() != 0` on an image read as RGBA, i.e. the alpha
    /// channel — the same test used here, so coverage and advance agree by
    /// construction rather than by coincidence. For a unihex glyph it is
    /// [`UnihexGlyph::is_ink`], the bit `unpackBitsToBytes` would have written.
    /// For a `ttf` glyph it is the rasterised coverage byte against
    /// [`TTF_INK_THRESHOLD`] — see that constant for why this is a threshold
    /// and not a `!= 0` test.
    pub fn is_ink(&self, tx: u32, ty: u32) -> bool {
        self.texel_rgba(tx, ty)[3] != 0.0
    }
}

impl FontLoader<'_> {
    /// Loads `id` and decodes every bitmap sheet its glyphs reference, yielding
    /// a [`RasterFont`].
    ///
    /// Sheets are decoded once each, not once per glyph. A sheet that decodes
    /// here is by construction the same one [`FontLoader::load`] measured the
    /// advances from, so ink and advance can never disagree.
    pub fn load_raster(
        &self,
        id: &ResourceLocation,
        options: &FontOptions,
    ) -> Result<RasterFont, FontError> {
        let font = self.load(id, options)?;
        let mut files: Vec<ResourceLocation> = Vec::new();
        for cp in font.codepoints() {
            if let Some(g) = font.bitmap_glyph(cp)
                && !files.contains(&g.file)
            {
                files.push(g.file.clone());
            }
        }
        let mut sheets = HashMap::with_capacity(files.len());
        for file in files {
            let tex_path = format!("assets/{}/textures/{}", file.namespace(), file.path());
            let bytes = self
                .manager
                .read(&tex_path)
                .ok_or_else(|| FontError::MissingTexture {
                    file: file.to_string(),
                })?;
            sheets.insert(file, Image::decode_png(&bytes)?);
        }

        // Same "collect the distinct files, then load each once" shape as the
        // bitmap sheets above. Re-parsing the font here (rather than caching
        // the `fontdue::Font` from `load_ttf`'s metrics pass) mirrors how a
        // bitmap sheet is decoded once for [`FontLoader::load`]'s metrics and
        // again here for [`RasterFont`]'s resident pixels — an existing
        // redundancy, not a new one.
        let mut ttf_files: Vec<ResourceLocation> = Vec::new();
        for cp in font.codepoints() {
            if let Some(g) = font.ttf_glyph(cp)
                && !ttf_files.contains(&g.file)
            {
                ttf_files.push(g.file.clone());
            }
        }
        let mut ttf_faces = HashMap::with_capacity(ttf_files.len());
        for file in ttf_files {
            let path = format!("assets/{}/font/{}", file.namespace(), file.path());
            let bytes = self
                .manager
                .read(&path)
                .ok_or_else(|| FontError::TtfFont {
                    file: file.to_string(),
                    reason: "file present during metrics load is now missing".into(),
                })?;
            let face = fontdue::Font::from_bytes(bytes.as_slice(), fontdue::FontSettings::default())
                .map_err(|reason| FontError::TtfFont {
                    file: file.to_string(),
                    reason: reason.to_string(),
                })?;
            ttf_faces.insert(file, face);
        }

        Ok(RasterFont {
            font,
            sheets,
            ttf_faces,
        })
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
