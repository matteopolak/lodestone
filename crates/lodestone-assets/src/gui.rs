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
    /// `sprite_w`/`sprite_h` are the sprite's real pixel dimensions, and **all
    /// three modes need them, nine-slice included**. Vanilla's
    /// `GuiSpriteScaling.NineSlice` treats its declared `width`/`height` as the
    /// size the border insets were *authored* against, not as the size of the
    /// texture on disk: a 32x pack ships a sprite twice the declared dimensions,
    /// and every source coordinate is therefore a fraction of the declared size
    /// applied across the sprite's real UV span. Measuring against the declared
    /// numbers alone samples only the top-left quadrant of a high-resolution
    /// sprite, which drops the right and bottom borders — the visible symptom
    /// that this parameter list exists to prevent.
    ///
    /// `Tile` is immune by construction rather than by care, since its tile size
    /// equals the sprite size; `Stretch` maps the whole sprite either way.
    ///
    /// Returns quads whose destination rectangles tile the target exactly, with
    /// no gaps or overlap.
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
            } => nine_slice_geometry(
                *width,
                *height,
                sprite_w,
                sprite_h,
                *border,
                *stretch_inner,
                dst_w,
                dst_h,
            ),
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
///
/// `dst_w`/`dst_h` and `tile_step_w`/`tile_step_h` are in *declared* (GUI-pixel)
/// units — the on-screen rect and, for a tiled segment, the repeat spacing,
/// both resolution-independent. `src_x`/`src_y`/`src_w`/`src_h` are already in
/// *real* sprite pixels (the caller has scaled them); this function never
/// touches the real/declared ratio itself.
#[allow(clippy::too_many_arguments)]
fn inner_segment(
    out: &mut Vec<GuiQuad>,
    stretch_inner: bool,
    dst_x: u32,
    dst_y: u32,
    dst_w: u32,
    dst_h: u32,
    tile_step_w: u32,
    tile_step_h: u32,
    src_x: f32,
    src_y: f32,
    src_w: f32,
    src_h: f32,
) {
    if dst_w == 0 || dst_h == 0 || src_w <= 0.0 || src_h <= 0.0 {
        return;
    }
    if stretch_inner {
        out.push(GuiQuad {
            dst: [dst_x as i32, dst_y as i32, dst_w as i32, dst_h as i32],
            src: [src_x, src_y, src_w, src_h],
        });
    } else {
        tile_region(
            out,
            dst_x,
            dst_y,
            dst_w,
            dst_h,
            src_x,
            src_y,
            tile_step_w,
            tile_step_h,
            src_w,
            src_h,
        );
    }
}

/// Emits a fixed-size-on-screen corner quad. `dst_w`/`dst_h` are the
/// destination size in declared (GUI-pixel) units; `src_x`/`src_y`/`src_w`/
/// `src_h` are already scaled to real sprite pixels by the caller.
#[allow(clippy::too_many_arguments)]
fn corner(
    out: &mut Vec<GuiQuad>,
    dst_x: u32,
    dst_y: u32,
    dst_w: u32,
    dst_h: u32,
    src_x: f32,
    src_y: f32,
    src_w: f32,
    src_h: f32,
) {
    if dst_w == 0 || dst_h == 0 {
        return;
    }
    out.push(GuiQuad {
        dst: [dst_x as i32, dst_y as i32, dst_w as i32, dst_h as i32],
        src: [src_x, src_y, src_w, src_h],
    });
}

/// `sprite_w`/`sprite_h` are the sprite's **real** pixel dimensions; `nw`/`nh`
/// are the `.mcmeta`-declared dimensions the border insets are measured
/// against, which need not match (a resource pack can ship a higher-resolution
/// PNG under metadata inherited from the base game).
///
/// # Why source rects are rescaled and destination rects are not
///
/// Vanilla (`GuiGraphicsExtractor.blitNineSlicedSprite` —
/// `AbstractBoatRenderer` is unrelated)
/// never computes an absolute source pixel offset. Every corner/edge call
/// passes `nineSlice.width()`/`height()` as `spriteWidth`/`spriteHeight` and a
/// *declared*-space offset as `textureX`/`textureY`, and the actual sample is
/// `sprite.getU((float) textureX / spriteWidth)` — a **fraction** of the
/// declared size, applied against the atlas UV span that spans the sprite's
/// real pixel width. A fraction is resolution-independent, so vanilla's
/// nine-slice source math implicitly rescales to whatever the real texture
/// turns out to be; only the *destination* geometry (`x`, `y`, `width`,
/// `height`, `borderLeft`, …) is ever in raw, unscaled pixels, because a
/// nine-slice border is always drawn at a fixed GUI-pixel thickness regardless
/// of how many real texture pixels back it.
///
/// This function reproduces that split explicitly: `bl`/`br`/`bt`/`bb`/`cw`/
/// `ch` (declared units, clamped to half the target exactly like vanilla's own
/// `Math.min(border.left(), width / 2)`) drive every **destination** rect and
/// the **tile step** of a tiled edge/center, unchanged from before this fix.
/// Every **source** rect is the same declared-space quantity multiplied by
/// `sprite_w / nw` (or `sprite_h / nh`) — vanilla's fraction, applied up
/// front instead of deferred to a shader, since [`GuiQuad::src`] is already
/// documented as real sprite pixels, and `lodestone_render::GuiAtlas::geometry`
/// turns it into atlas UVs by adding the sprite's atlas offset and dividing by
/// the atlas's real pixel size — never by `nw`/`nh`. At a 1× pack (real ==
/// declared) the ratio is `1.0`
/// and every quad is byte-identical to before; at Faithful 32× (400×40 real
/// under inherited 200×20 metadata) the ratio is `2.0` and the right/bottom
/// source rects land on the real border pixels instead of the texture's
/// middle.
#[allow(clippy::too_many_arguments)]
fn nine_slice_geometry(
    nw: u32,
    nh: u32,
    sprite_w: u32,
    sprite_h: u32,
    border: Border,
    stretch_inner: bool,
    dst_w: u32,
    dst_h: u32,
) -> Vec<GuiQuad> {
    // Borders never exceed half the target, so opposing regions never
    // overlap. Declared/GUI-pixel units — destination-only.
    let bl = border.left.min(dst_w / 2);
    let br = border.right.min(dst_w / 2);
    let bt = border.top.min(dst_h / 2);
    let bb = border.bottom.min(dst_h / 2);
    let cw = dst_w - bl - br; // center/edge destination width
    let ch = dst_h - bt - bb; // center/edge destination height
    // One inner tile's declared size — the destination repeat step for a
    // tiled (non-stretched) edge/center. Still declared units: vanilla derives
    // this the same way (`nineSlice.width() - borderRight - borderLeft`)
    // before it ever touches the real texture.
    let scw = nw - bl - br;
    let sch = nh - bt - bb;

    // The real-to-declared ratio: `1.0` for a pack whose PNG matches its own
    // metadata, `> 1.0` for a higher-resolution PNG under stale/inherited
    // metadata. Every source rect below is a declared-space quantity times
    // this ratio, mirroring vanilla's `textureX / spriteWidth` fraction.
    let rx = sprite_w as f32 / nw as f32;
    let ry = sprite_h as f32 / nh as f32;
    let s_bl = bl as f32 * rx;
    let s_br = br as f32 * rx;
    let s_bt = bt as f32 * ry;
    let s_bb = bb as f32 * ry;
    let s_w = sprite_w as f32;
    let s_h = sprite_h as f32;
    let s_scw = scw as f32 * rx;
    let s_sch = sch as f32 * ry;

    let mut out = Vec::new();
    // Corners (fixed destination size, real-pixel source size).
    corner(&mut out, 0, 0, bl, bt, 0.0, 0.0, s_bl, s_bt);
    corner(&mut out, dst_w - br, 0, br, bt, s_w - s_br, 0.0, s_br, s_bt);
    corner(&mut out, 0, dst_h - bb, bl, bb, 0.0, s_h - s_bb, s_bl, s_bb);
    corner(
        &mut out,
        dst_w - br,
        dst_h - bb,
        br,
        bb,
        s_w - s_br,
        s_h - s_bb,
        s_br,
        s_bb,
    );
    // Edges (scale along one axis).
    inner_segment(
        &mut out, stretch_inner, bl, 0, cw, bt, scw, bt, s_bl, 0.0, s_scw, s_bt,
    ); // top
    inner_segment(
        &mut out,
        stretch_inner,
        bl,
        dst_h - bb,
        cw,
        bb,
        scw,
        bb,
        s_bl,
        s_h - s_bb,
        s_scw,
        s_bb,
    ); // bottom
    inner_segment(
        &mut out, stretch_inner, 0, bt, bl, ch, bl, sch, 0.0, s_bt, s_bl, s_sch,
    ); // left
    inner_segment(
        &mut out,
        stretch_inner,
        dst_w - br,
        bt,
        br,
        ch,
        br,
        sch,
        s_w - s_br,
        s_bt,
        s_br,
        s_sch,
    ); // right
    // Center.
    inner_segment(
        &mut out, stretch_inner, bl, bt, cw, ch, scw, sch, s_bl, s_bt, s_scw, s_sch,
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A 20×20 nine-slice with a uniform 4px border, drawn at its own declared
    /// size and looked up by `(dst_x, dst_y)` — the shape of every quad this
    /// scaling can produce, regardless of resolution.
    fn find<'a>(quads: &'a [GuiQuad], dst_x: i32, dst_y: i32) -> &'a GuiQuad {
        quads
            .iter()
            .find(|q| q.dst[0] == dst_x && q.dst[1] == dst_y)
            .unwrap_or_else(|| panic!("no quad at dst ({dst_x}, {dst_y}) in {quads:#?}"))
    }

    /// The regression control: a 1× pack (real pixels == declared metadata) is
    /// the case that shipped and passed for years, so this fix must not move a
    /// single byte of it. Every source rect here is hand-derived from the
    /// scaling alone (no border scaling applies at ratio 1.0).
    #[test]
    fn a_1x_pack_is_unchanged_by_the_real_declared_split() {
        let border = Border::uniform(4);
        let quads = nine_slice_geometry(20, 20, 20, 20, border, false, 20, 20);
        assert_eq!(quads.len(), 9, "4 corners + 4 edges + 1 center");

        let top_left = find(&quads, 0, 0);
        assert_eq!(top_left.dst, [0, 0, 4, 4]);
        assert_eq!(top_left.src, [0.0, 0.0, 4.0, 4.0]);

        let bottom_right = find(&quads, 16, 16);
        assert_eq!(bottom_right.dst, [16, 16, 4, 4]);
        assert_eq!(bottom_right.src, [16.0, 16.0, 4.0, 4.0]);

        let center = find(&quads, 4, 4);
        assert_eq!(center.dst, [4, 4, 12, 12]);
        assert_eq!(center.src, [4.0, 4.0, 12.0, 12.0]);
    }

    /// The discriminating input the bug needed and never got: a real sprite
    /// **twice** the pixel size its `.mcmeta` declares — exactly Faithful 32×
    /// (400×40 real) under inherited 200×20 metadata, scaled down to a small
    /// fixture. The destinations are identical to the 1× case above (a
    /// nine-slice border is always the same number of *GUI* pixels); only the
    /// **source** rects move, to the real border pixels rather than the
    /// texture's middle.
    ///
    /// This is the exact regression `GuiScaling::geometry` had: it received
    /// `sprite_w`/`sprite_h` and silently dropped them for the `NineSlice` arm,
    /// so `bottom_right.src` before this fix was `[16.0, 16.0, 4.0, 4.0]` — the
    /// dead centre of a 40×40 texture — instead of the real bottom-right corner.
    #[test]
    fn a_higher_resolution_pack_scales_source_rects_not_destinations() {
        let border = Border::uniform(4);
        let quads = nine_slice_geometry(20, 20, 40, 40, border, false, 20, 20);
        assert_eq!(quads.len(), 9);

        // Destinations: byte-identical to the 1x case — always GUI-pixel sized.
        let top_left = find(&quads, 0, 0);
        assert_eq!(top_left.dst, [0, 0, 4, 4]);
        let top_right = find(&quads, 16, 0);
        assert_eq!(top_right.dst, [16, 0, 4, 4]);
        let bottom_right = find(&quads, 16, 16);
        assert_eq!(bottom_right.dst, [16, 16, 4, 4]);
        let center = find(&quads, 4, 4);
        assert_eq!(center.dst, [4, 4, 12, 12]);

        // Sources: every declared-pixel quantity times the real/declared ratio
        // of 2.0 — landing on the real texture's own border pixels.
        assert_eq!(
            top_left.src,
            [0.0, 0.0, 8.0, 8.0],
            "top-left corner source must double with the resolution"
        );
        assert_eq!(
            top_right.src,
            [32.0, 0.0, 8.0, 8.0],
            "top-right corner source must sample near the real right edge \
             (x=32 of 40), not the middle of the texture"
        );
        assert_eq!(
            bottom_right.src,
            [32.0, 32.0, 8.0, 8.0],
            "the exact regression: this used to read [16.0, 16.0, 4.0, 4.0], \
             sampling the dead centre of the 40x40 texture instead of its \
             bottom-right corner"
        );
        assert_eq!(
            center.src,
            [8.0, 8.0, 24.0, 24.0],
            "the tiled center's source rect must also scale"
        );
    }

    /// The same discriminating input, but `stretch_inner: true` — the edges and
    /// center are emitted as one quad each rather than tiled, a different code
    /// path through [`inner_segment`] that needs its own coverage.
    #[test]
    fn stretch_inner_also_scales_source_rects_under_a_resolution_mismatch() {
        let border = Border::uniform(4);
        let quads = nine_slice_geometry(20, 20, 40, 40, border, true, 20, 20);
        assert_eq!(quads.len(), 9);

        let center = find(&quads, 4, 4);
        assert_eq!(center.dst, [4, 4, 12, 12]);
        assert_eq!(
            center.src,
            [8.0, 8.0, 24.0, 24.0],
            "a stretched center must scale its source rect exactly like a \
             tiled one — this is a different branch of `inner_segment`"
        );
    }

    /// [`GuiScaling::geometry`] is the real entry point every caller uses; this
    /// pins that it actually threads `sprite_w`/`sprite_h` into the `NineSlice`
    /// arm rather than discarding them (the bug lived exactly here — the
    /// function received both real dimensions and a working [`nine_slice_geometry`]
    /// to hand them to, and just never made the call with them).
    #[test]
    fn gui_scaling_geometry_forwards_real_dimensions_to_nine_slice() {
        let scaling = GuiScaling::NineSlice {
            width: 20,
            height: 20,
            border: Border::uniform(4),
            stretch_inner: false,
        };
        let quads = scaling.geometry(40, 40, 20, 20);
        let bottom_right = find(&quads, 16, 16);
        assert_eq!(
            bottom_right.src,
            [32.0, 32.0, 8.0, 8.0],
            "GuiScaling::geometry must forward its real sprite_w/sprite_h into \
             nine_slice_geometry, not just the declared width/height"
        );
    }
}
