//! [`FaviconMosaic`]: a server favicon or a player head reduced to a small
//! grid of coloured cells, drawn as quads on the pipeline that is already
//! here rather than through a texture and a second bind group.
//!
//! Split out of `menu/render.rs` verbatim: a pure move by line range.

use super::*;

/// A favicon reduced to a small grid of colours, ready to draw as quads.
#[derive(Debug, Clone, PartialEq)]
pub struct FaviconMosaic {
    /// Cells per side.
    pub size: usize,
    /// `size * size` RGBA cells, row-major, top-left first.
    pub cells: Vec<[f32; 4]>,
}

/// Decodes `png` and box-filters it to a [`MOSAIC`]×[`MOSAIC`] colour grid.
///
/// Returns `None` if the bytes are not a decodable PNG — a server with a broken
/// icon still gets its MOTD, which is the whole reason `lodestone-net` decodes
/// the favicon as `Option` rather than failing the status.
#[must_use]
pub fn favicon_mosaic(png: &[u8]) -> Option<FaviconMosaic> {
    let img = Image::decode_png(png).ok()?;
    rgba_mosaic(&img.rgba, img.width as usize, img.height as usize)
}

/// Box-filters a small player-head icon down to a [`MOSAIC`]×[`MOSAIC`]
/// colour grid, from **already-decoded** RGBA bytes rather than a PNG file —
/// see [`default_head_icon`] for why that is the parameter this screen needs.
///
/// Shares [`rgba_mosaic`]'s box filter with [`favicon_mosaic`] rather than
/// re-deriving it: a head and a favicon are the same kind of drawable (a
/// small square texture reduced to coloured cells), so there is one filter,
/// not two that could silently drift apart.
#[must_use]
pub fn head_mosaic(rgba: &[u8], width: usize, height: usize) -> Option<FaviconMosaic> {
    rgba_mosaic(rgba, width, height)
}

/// The 8×8 **face** of a decoded skin sheet, box-filtered to a mosaic the
/// account list can draw — the per-account avatar, in place of the one
/// hand-authored head every row used to share.
///
/// # The face is two layers, not one
///
/// Vanilla's `PlayerFaceRenderer` blits the head's front face from `(8, 8)`
/// **and then the hat layer from `(40, 8)` over it**, so a skin whose
/// character is defined by the hat — a helmet, hair, a mask — is
/// unrecognisable from the base layer alone. Both are 8×8 and both live in
/// the top half of the sheet, so this works for a legacy 64×32 sheet as well
/// as a 64×64 one.
///
/// The composite is a plain source-over on straight alpha. The hat layer is
/// authored as fully opaque or fully transparent in practice, but blending
/// properly costs nothing and avoids a hard cutoff choosing wrong on a skin
/// that does use partial alpha.
///
/// `None` for a sheet too small to contain the face rect, which is the only
/// way this can fail — a caller falls back to [`default_head_icon`].
#[must_use]
pub fn face_mosaic(sheet: &Image) -> Option<FaviconMosaic> {
    /// Left edge of both the base face and the hat layer.
    const BASE_X: usize = 8;
    /// Top edge of both.
    const Y: usize = 8;
    /// Left edge of the hat layer.
    const HAT_X: usize = 40;

    let (w, h) = (sheet.width as usize, sheet.height as usize);
    if w < HAT_X + HEAD_SIZE || h < Y + HEAD_SIZE {
        return None;
    }
    let texel = |x: usize, y: usize| -> [u8; 4] {
        let i = (y * w + x) * 4;
        [
            sheet.rgba[i],
            sheet.rgba[i + 1],
            sheet.rgba[i + 2],
            sheet.rgba[i + 3],
        ]
    };

    let mut face = [0u8; HEAD_SIZE * HEAD_SIZE * 4];
    for y in 0..HEAD_SIZE {
        for x in 0..HEAD_SIZE {
            let base = texel(BASE_X + x, Y + y);
            let hat = texel(HAT_X + x, Y + y);
            let a = f32::from(hat[3]) / 255.0;
            let out = (y * HEAD_SIZE + x) * 4;
            for c in 0..3 {
                let over = f32::from(hat[c]).mul_add(a, f32::from(base[c]) * (1.0 - a));
                face[out + c] = over.round().clamp(0.0, 255.0) as u8;
            }
            // The base face is opaque on every well-formed skin; keeping its
            // own alpha rather than forcing 255 means a malformed sheet draws
            // transparent instead of drawing a lie.
            face[out + 3] = base[3].max(hat[3]);
        }
    }
    head_mosaic(&face, HEAD_SIZE, HEAD_SIZE)
}

/// A placeholder head icon used until skins are implemented: a
/// flat skin-tone square with a darker hairline band across the top eighth
/// and two single-pixel eyes, at [`HEAD_SIZE`]×[`HEAD_SIZE`].
///
/// **The texture is the parameter, not the constant.** [`head_mosaic`] does
/// not know or care that [`DEFAULT_HEAD_RGBA`] is hand-authored pixels rather
/// than a downloaded skin — it is exactly the same call a real skin's decoded
/// face region would go through. Swapping this default out for
/// `head_mosaic(&decoded_skin_face, 8, 8)` once that fix lands a skin
/// fetch is the entire change; nothing in [`MenuRow`], [`draw_widget`]'s
/// icon-drawing branch, or the geometry builder needs to move.
#[must_use]
pub fn default_head_icon() -> FaviconMosaic {
    head_mosaic(&DEFAULT_HEAD_RGBA, HEAD_SIZE, HEAD_SIZE).expect("the embedded default head is a valid 8x8 RGBA grid")
}

/// Side length, in pixels, of [`DEFAULT_HEAD_RGBA`].
const HEAD_SIZE: usize = 8;

/// An 8×8 RGBA placeholder head: skin tone (`0xC8, 0x96, 0x64`) with a
/// darker top row (hair) and two single-pixel dark eyes on row 4. Hand-authored
/// pixels, not art — see [`default_head_icon`]'s docs on why that is fine.
const DEFAULT_HEAD_RGBA: [u8; HEAD_SIZE * HEAD_SIZE * 4] = build_default_head();

const fn build_default_head() -> [u8; HEAD_SIZE * HEAD_SIZE * 4] {
    const SKIN: [u8; 4] = [0xC8, 0x96, 0x64, 0xFF];
    const HAIR: [u8; 4] = [0x4A, 0x30, 0x1E, 0xFF];
    const EYE: [u8; 4] = [0x20, 0x20, 0x20, 0xFF];
    let mut out = [0u8; HEAD_SIZE * HEAD_SIZE * 4];
    let mut y = 0;
    while y < HEAD_SIZE {
        let mut x = 0;
        while x < HEAD_SIZE {
            let px = if y == 0 {
                HAIR
            } else if y == 3 && (x == 2 || x == 5) {
                EYE
            } else {
                SKIN
            };
            let i = (y * HEAD_SIZE + x) * 4;
            out[i] = px[0];
            out[i + 1] = px[1];
            out[i + 2] = px[2];
            out[i + 3] = px[3];
            x += 1;
        }
        y += 1;
    }
    out
}

/// The box filter shared by [`favicon_mosaic`] and [`head_mosaic`]: reduces
/// `width`×`height` RGBA pixels to [`MOSAIC`]×[`MOSAIC`] cells, averaging each
/// cell's source rect. Returns `None` for a zero-sized image.
#[must_use]
fn rgba_mosaic(rgba: &[u8], width: usize, height: usize) -> Option<FaviconMosaic> {
    if width == 0 || height == 0 {
        return None;
    }
    let (iw, ih) = (width, height);
    let mut cells = Vec::with_capacity(MOSAIC * MOSAIC);
    for cy in 0..MOSAIC {
        for cx in 0..MOSAIC {
            // Source rect for this cell. Each bound is forced to span at least
            // one pixel: for an icon *smaller* than the mosaic, plain integer
            // division gives `x0 == x1` for most cells, which would average
            // nothing and leave transparent (invisible) holes.
            let x0 = (cx * iw / MOSAIC).min(iw - 1);
            let x1 = ((cx + 1) * iw).div_ceil(MOSAIC).clamp(x0 + 1, iw);
            let y0 = (cy * ih / MOSAIC).min(ih - 1);
            let y1 = ((cy + 1) * ih).div_ceil(MOSAIC).clamp(y0 + 1, ih);
            let mut acc = [0f32; 4];
            let mut n = 0f32;
            for y in y0..y1 {
                for x in x0..x1 {
                    let i = (y * iw + x) * 4;
                    if i + 3 >= rgba.len() {
                        continue;
                    }
                    for (c, a) in acc.iter_mut().enumerate() {
                        *a += f32::from(rgba[i + c]);
                    }
                    n += 1.0;
                }
            }
            if n == 0.0 {
                cells.push([0.0, 0.0, 0.0, 0.0]);
            } else {
                cells.push([
                    acc[0] / n / 255.0,
                    acc[1] / n / 255.0,
                    acc[2] / n / 255.0,
                    acc[3] / n / 255.0,
                ]);
            }
        }
    }
    Some(FaviconMosaic {
        size: MOSAIC,
        cells,
    })
}

