//! GUI sprite scaling (`gui.scaling`) — parsing and geometry.
//!
//! Modern Minecraft GUI sprites (`assets/<ns>/textures/gui/sprites/**`) carry a
//! sibling `<name>.png.mcmeta` describing how the sprite is scaled to a target
//! rectangle. Three modes exist ([`GuiScaling`]):
//!
//! * `stretch` (the default) — the whole sprite is stretched to fill.
//! * `tile` — the sprite is repeated at its native size, cropping the last
//!   partial row/column.
//! * `nine_slice` — the four corners stay fixed-size, the four edges scale along
//!   one axis, and the center fills the middle. Edges/center are either tiled
//!   (default) or stretched (`stretch_inner: true`).
//!
//! This module is GPU-free: [`GuiScaling::geometry`] returns a renderer-agnostic
//! list of [`GuiQuad`]s, each mapping a destination rectangle (in target pixels)
//! to a source rectangle (in native sprite pixels). The renderer turns source
//! rects into atlas UVs. Geometry mirrors vanilla's `GuiGraphics` blit path
//! (corner/edge/center decomposition, half-size border clamping, per-tile
//! cropping) so packs render identically.

use serde_json::Value;

use crate::error::GuiError;

/// The four border insets of a nine-slice sprite, in native sprite pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Border {
    /// Inset from the left edge.
    pub left: u32,
    /// Inset from the top edge.
    pub top: u32,
    /// Inset from the right edge.
    pub right: u32,
    /// Inset from the bottom edge.
    pub bottom: u32,
}

impl Border {
    /// A uniform border with the same inset on all four sides.
    pub fn uniform(size: u32) -> Self {
        Self {
            left: size,
            top: size,
            right: size,
            bottom: size,
        }
    }
}

/// How a GUI sprite is scaled to fill a target rectangle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuiScaling {
    /// Stretch the whole sprite to the target (the vanilla default).
    Stretch,
    /// Repeat the sprite at a fixed native tile size, cropping the last tiles.
    Tile {
        /// Native tile width in sprite pixels.
        width: u32,
        /// Native tile height in sprite pixels.
        height: u32,
    },
    /// Nine-slice scaling: fixed corners, axis-scaled edges, filled center.
    NineSlice {
        /// The native width the border insets are measured against.
        width: u32,
        /// The native height the border insets are measured against.
        height: u32,
        /// The four border insets.
        border: Border,
        /// If `true`, edges/center are stretched; otherwise they are tiled.
        stretch_inner: bool,
    },
}

/// One piece of scaled GUI geometry: a destination rect fed by a source rect.
///
/// `dst` is `[x, y, w, h]` in target pixels, relative to the sprite's origin
/// (the renderer translates by the draw position). `src` is `[x, y, w, h]` in
/// native sprite pixels; fractional values arise only when a tile is cropped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GuiQuad {
    /// Destination rectangle `[x, y, w, h]` in target pixels.
    pub dst: [i32; 4],
    /// Source rectangle `[x, y, w, h]` in native sprite pixels.
    pub src: [f32; 4],
}

/// The parsed `gui` metadata section of a sprite `.mcmeta`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuiMeta {
    /// The sprite's scaling mode.
    pub scaling: GuiScaling,
}

impl GuiMeta {
    /// Parses a sprite `.mcmeta` document. A missing `gui` or `gui.scaling`
    /// section defaults to [`GuiScaling::Stretch`], matching vanilla.
    pub fn parse(bytes: &[u8]) -> Result<Self, GuiError> {
        let root: Value =
            serde_json::from_slice(bytes).map_err(|e| GuiError::Json(e.to_string()))?;
        let scaling = match root.get("gui").and_then(|g| g.get("scaling")) {
            Some(v) => GuiScaling::parse_value(v)?,
            None => GuiScaling::Stretch,
        };
        Ok(Self { scaling })
    }
}

fn positive_u32(v: &Value, field: &str) -> Result<u32, GuiError> {
    let n = v
        .get(field)
        .and_then(Value::as_u64)
        .ok_or_else(|| GuiError::InvalidField(format!("{field} must be a positive integer")))?;
    if n == 0 || n > u32::MAX as u64 {
        return Err(GuiError::InvalidField(format!("{field} out of range: {n}")));
    }
    Ok(n as u32)
}

fn non_negative_u32(v: &Value, field: &str) -> Result<u32, GuiError> {
    let n = v
        .as_u64()
        .ok_or_else(|| GuiError::InvalidField(format!("{field} must be a non-negative integer")))?;
    if n > u32::MAX as u64 {
        return Err(GuiError::InvalidField(format!("{field} out of range: {n}")));
    }
    Ok(n as u32)
}

impl GuiScaling {
    /// Parses a `scaling` object (the value of `gui.scaling`).
    pub fn parse_value(v: &Value) -> Result<Self, GuiError> {
        let kind = v
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| GuiError::InvalidField("scaling.type is required".into()))?;
        match kind {
            "stretch" => Ok(GuiScaling::Stretch),
            "tile" => Ok(GuiScaling::Tile {
                width: positive_u32(v, "width")?,
                height: positive_u32(v, "height")?,
            }),
            "nine_slice" => {
                let width = positive_u32(v, "width")?;
                let height = positive_u32(v, "height")?;
                let border = parse_border(v.get("border"))?;
                let stretch_inner = v
                    .get("stretch_inner")
                    .and_then(Value::as_bool)
                    .unwrap_or(false);
                if border.left + border.right >= width {
                    return Err(GuiError::NoCenter(format!(
                        "{} + {} >= {width}",
                        border.left, border.right
                    )));
                }
                if border.top + border.bottom >= height {
                    return Err(GuiError::NoCenter(format!(
                        "{} + {} >= {height}",
                        border.top, border.bottom
                    )));
                }
                Ok(GuiScaling::NineSlice {
                    width,
                    height,
                    border,
                    stretch_inner,
                })
            }
            other => Err(GuiError::UnknownType(other.to_string())),
        }
    }

    /// Builds the geometry mapping this sprite onto a `dst_w` x `dst_h` target.
    ///
    /// `sprite_w`/`sprite_h` are the sprite's real pixel dimensions (used by the
    /// stretch and tile modes; nine-slice measures against its own `width`/
    /// `height`). Returns quads whose destination rectangles tile the target
    /// exactly, with no gaps or overlap.
    pub fn geometry(&self, sprite_w: u32, sprite_h: u32, dst_w: u32, dst_h: u32) -> Vec<GuiQuad> {
        match self {
            GuiScaling::Stretch => vec![GuiQuad {
                dst: [0, 0, dst_w as i32, dst_h as i32],
                src: [0.0, 0.0, sprite_w as f32, sprite_h as f32],
            }],
            GuiScaling::Tile { width, height } => {
                // Each full tile shows the entire sprite at native tile size.
                let mut out = Vec::new();
                tile_region(
                    &mut out,
                    0,
                    0,
                    dst_w,
                    dst_h,
                    0.0,
                    0.0,
                    *width,
                    *height,
                    sprite_w as f32,
                    sprite_h as f32,
                );
                out
            }
            GuiScaling::NineSlice {
                width,
                height,
                border,
                stretch_inner,
            } => nine_slice_geometry(*width, *height, *border, *stretch_inner, dst_w, dst_h),
        }
    }
}

fn parse_border(v: Option<&Value>) -> Result<Border, GuiError> {
    let v = v.ok_or_else(|| GuiError::InvalidField("nine_slice.border is required".into()))?;
    if let Some(size) = v.as_u64() {
        if size > u32::MAX as u64 {
            return Err(GuiError::InvalidField(format!(
                "border out of range: {size}"
            )));
        }
        return Ok(Border::uniform(size as u32));
    }
    if v.is_object() {
        return Ok(Border {
            left: non_negative_u32(&v["left"], "border.left")?,
            top: non_negative_u32(&v["top"], "border.top")?,
            right: non_negative_u32(&v["right"], "border.right")?,
            bottom: non_negative_u32(&v["bottom"], "border.bottom")?,
        });
    }
    Err(GuiError::InvalidField(
        "border must be an integer or an object".into(),
    ))
}

/// Fills a destination area by repeating a source tile, cropping partial edges.
///
/// A full tile occupies `tile_w_dst` x `tile_h_dst` destination pixels and maps
/// to a `src_tile_w` x `src_tile_h` source rectangle at `(src_x, src_y)`. Partial
/// edge tiles crop both destination and source proportionally.
#[allow(clippy::too_many_arguments)]
fn tile_region(
    out: &mut Vec<GuiQuad>,
    dst_x: u32,
    dst_y: u32,
    dst_w: u32,
    dst_h: u32,
    src_x: f32,
    src_y: f32,
    tile_w_dst: u32,
    tile_h_dst: u32,
    src_tile_w: f32,
    src_tile_h: f32,
) {
    if dst_w == 0 || dst_h == 0 || tile_w_dst == 0 || tile_h_dst == 0 {
        return;
    }
    let mut y = 0;
    while y < dst_h {
        let ph = (dst_h - y).min(tile_h_dst);
        let frac_h = ph as f32 / tile_h_dst as f32;
        let mut x = 0;
        while x < dst_w {
            let pw = (dst_w - x).min(tile_w_dst);
            let frac_w = pw as f32 / tile_w_dst as f32;
            out.push(GuiQuad {
                dst: [(dst_x + x) as i32, (dst_y + y) as i32, pw as i32, ph as i32],
                src: [src_x, src_y, src_tile_w * frac_w, src_tile_h * frac_h],
            });
            x += tile_w_dst;
        }
        y += tile_h_dst;
    }
}

/// Emits one inner segment (edge or center): stretched to a single quad or
/// tiled at its native size.
#[allow(clippy::too_many_arguments)]
fn inner_segment(
    out: &mut Vec<GuiQuad>,
    stretch_inner: bool,
    dst_x: u32,
    dst_y: u32,
    dst_w: u32,
    dst_h: u32,
    src_x: u32,
    src_y: u32,
    src_w: u32,
    src_h: u32,
) {
    if dst_w == 0 || dst_h == 0 || src_w == 0 || src_h == 0 {
        return;
    }
    if stretch_inner {
        out.push(GuiQuad {
            dst: [dst_x as i32, dst_y as i32, dst_w as i32, dst_h as i32],
            src: [src_x as f32, src_y as f32, src_w as f32, src_h as f32],
        });
    } else {
        tile_region(
            out,
            dst_x,
            dst_y,
            dst_w,
            dst_h,
            src_x as f32,
            src_y as f32,
            src_w,
            src_h,
            src_w as f32,
            src_h as f32,
        );
    }
}

/// Emits a fixed-size (unscaled) corner quad.
fn corner(out: &mut Vec<GuiQuad>, dst_x: u32, dst_y: u32, src_x: u32, src_y: u32, w: u32, h: u32) {
    if w == 0 || h == 0 {
        return;
    }
    out.push(GuiQuad {
        dst: [dst_x as i32, dst_y as i32, w as i32, h as i32],
        src: [src_x as f32, src_y as f32, w as f32, h as f32],
    });
}

fn nine_slice_geometry(
    nw: u32,
    nh: u32,
    border: Border,
    stretch_inner: bool,
    dst_w: u32,
    dst_h: u32,
) -> Vec<GuiQuad> {
    // Borders never exceed half the target, so opposing regions never overlap.
    let bl = border.left.min(dst_w / 2);
    let br = border.right.min(dst_w / 2);
    let bt = border.top.min(dst_h / 2);
    let bb = border.bottom.min(dst_h / 2);
    let cw = dst_w - bl - br; // center/edge destination width
    let ch = dst_h - bt - bb; // center/edge destination height
    // Source insets use the same clamped borders (matches vanilla).
    let scw = nw - bl - br; // center/edge source width
    let sch = nh - bt - bb; // center/edge source height

    let mut out = Vec::new();
    // Corners (fixed size, unscaled).
    corner(&mut out, 0, 0, 0, 0, bl, bt);
    corner(&mut out, dst_w - br, 0, nw - br, 0, br, bt);
    corner(&mut out, 0, dst_h - bb, 0, nh - bb, bl, bb);
    corner(&mut out, dst_w - br, dst_h - bb, nw - br, nh - bb, br, bb);
    // Edges (scale along one axis).
    inner_segment(&mut out, stretch_inner, bl, 0, cw, bt, bl, 0, scw, bt); // top
    inner_segment(
        &mut out,
        stretch_inner,
        bl,
        dst_h - bb,
        cw,
        bb,
        bl,
        nh - bb,
        scw,
        bb,
    ); // bottom
    inner_segment(&mut out, stretch_inner, 0, bt, bl, ch, 0, bt, bl, sch); // left
    inner_segment(
        &mut out,
        stretch_inner,
        dst_w - br,
        bt,
        br,
        ch,
        nw - br,
        bt,
        br,
        sch,
    ); // right
    // Center.
    inner_segment(&mut out, stretch_inner, bl, bt, cw, ch, bl, bt, scw, sch);
    out
}
