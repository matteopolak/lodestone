//! PNG decoding and `*.png.mcmeta` metadata parsing.
//!
//! Decoding always normalises to RGBA8 so higher layers never have to branch on
//! the source colour type. Resource packs are untrusted, so malformed input is
//! reported as an error and never panics; the PNG decoder is also bounded by a
//! byte limit to resist decompression bombs.

use crate::error::TextureError;
use serde_json::Value;

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

/// Parsed `*.png.mcmeta` texture metadata.
///
/// Only the parts relevant to the block atlas are modelled explicitly (the
/// `animation` section). Other vanilla sections — `texture`, `gui`, `villager` —
/// are recognised and their presence recorded, but they are not otherwise
/// interpreted here.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct TextureMeta {
    /// The animation section, if present.
    pub animation: Option<AnimationMeta>,
    /// Names of other top-level sections that were present but not interpreted
    /// (for example `gui`, `villager`, `texture`), sorted and de-duplicated.
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
        let value: Value = serde_json::from_slice(bytes)
            .map_err(|e| TextureError::MetaMalformed(e.to_string()))?;
        let obj = value
            .as_object()
            .ok_or_else(|| TextureError::MetaMalformed("root is not an object".to_string()))?;

        let animation = match obj.get("animation") {
            Some(a) => Some(AnimationMeta::from_value(a)?),
            None => None,
        };
        let mut other_sections: Vec<String> = obj
            .keys()
            .filter(|k| k.as_str() != "animation")
            .cloned()
            .collect();
        other_sections.sort();
        other_sections.dedup();
        Ok(Self {
            animation,
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

impl AnimationMeta {
    fn from_value(value: &Value) -> Result<Self, TextureError> {
        let obj = value
            .as_object()
            .ok_or_else(|| TextureError::MetaMalformed("\"animation\" is not an object".into()))?;
        let frametime = obj
            .get("frametime")
            .map(|v| {
                v.as_u64()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| TextureError::MetaMalformed("invalid \"frametime\"".into()))
            })
            .transpose()?
            .unwrap_or(1) as u32;
        let interpolate = obj
            .get("interpolate")
            .map(|v| {
                v.as_bool()
                    .ok_or_else(|| TextureError::MetaMalformed("invalid \"interpolate\"".into()))
            })
            .transpose()?
            .unwrap_or(false);
        let frame_width = parse_opt_dim(obj.get("width"), "width")?;
        let frame_height = parse_opt_dim(obj.get("height"), "height")?;

        let frames = match obj.get("frames") {
            None => Vec::new(),
            Some(Value::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for item in items {
                    out.push(AnimationFrame::from_value(item)?);
                }
                out
            }
            Some(_) => {
                return Err(TextureError::MetaMalformed(
                    "\"frames\" is not an array".into(),
                ));
            }
        };
        Ok(Self {
            frametime,
            interpolate,
            frame_width,
            frame_height,
            frames,
        })
    }
}

fn parse_opt_dim(value: Option<&Value>, field: &str) -> Result<Option<u32>, TextureError> {
    match value {
        None => Ok(None),
        Some(v) => v
            .as_u64()
            .filter(|&n| n > 0 && n <= u32::MAX as u64)
            .map(|n| Some(n as u32))
            .ok_or_else(|| TextureError::MetaMalformed(format!("invalid \"{field}\""))),
    }
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

impl AnimationFrame {
    fn from_value(value: &Value) -> Result<Self, TextureError> {
        // Bare index form.
        if let Some(index) = value.as_u64() {
            return Ok(Self {
                index: index as u32,
                time: None,
            });
        }
        // { "index": N, "time": M } form.
        let obj = value.as_object().ok_or_else(|| {
            TextureError::MetaMalformed("frame is neither an index nor an object".into())
        })?;
        let index =
            obj.get("index").and_then(Value::as_u64).ok_or_else(|| {
                TextureError::MetaMalformed("frame missing valid \"index\"".into())
            })? as u32;
        let time = obj
            .get("time")
            .map(|v| {
                v.as_u64()
                    .filter(|&n| n > 0)
                    .ok_or_else(|| TextureError::MetaMalformed("invalid frame \"time\"".into()))
            })
            .transpose()?
            .map(|n| n as u32);
        Ok(Self { index, time })
    }
}
