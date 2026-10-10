//! Shared measurement for the map decoration pixel gates: where a decoration
//! sprite should land on a map, predicted from the sprite's own PNG and the
//! reference placement rules, compared with the pixels a render changed.
//!
//! The rules, written out again here independently of the renderer: wire units
//! are half map pixels from the map centre; the sprite is 8x8 pixels with its top
//! row at the lower edge, shifted half a pixel left and down; rotation is
//! clockwise sixteenths of a turn.

#![allow(dead_code)]

use lodestone_assets::{Image, ResourceManager, ResourceSource, ZipSource};

/// A render target's size, for turning pixel indices into coordinates.
#[derive(Debug, Clone, Copy)]
pub struct Target {
    pub w: u32,
    pub h: u32,
}

/// The decoration sprite PNG, from the jar.
pub fn sprite(name: &str) -> Image {
    let jar = lodestone_mc_cache::client_jar().expect("no client.jar under .cache/mc");
    let bytes = std::fs::read(&jar).unwrap_or_else(|e| panic!("read {}: {e}", jar.display()));
    let zip = ZipSource::from_bytes(bytes).expect("open jar");
    let manager = ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>]);
    let png = manager
        .read(&format!("assets/minecraft/textures/map/decorations/{name}.png"))
        .unwrap_or_else(|| panic!("{name}.png is in the jar"));
    Image::decode_png(&png).expect("decode sprite")
}

fn is_opaque(image: &Image, col: u32, row: u32) -> bool {
    image.rgba[((row * image.width + col) * 4 + 3) as usize] != 0
}

/// Map-pixel position of a point `(sx, sy)` in sprite space (pixels from the
/// sprite centre, y down) for a sprite at wire `(x, y)` with `rotation`.
fn to_map(sx: f32, sy: f32, x: i8, y: i8, rotation: u8) -> (f32, f32) {
    let angle = f32::from(rotation) * 22.5_f32.to_radians();
    let (sin, cos) = angle.sin_cos();
    let (px, py) = (sx - 0.5, sy + 0.5);
    (
        f32::from(x) / 2.0 + 64.0 + px * cos - py * sin,
        f32::from(y) / 2.0 + 64.0 + px * sin + py * cos,
    )
}

/// Map-pixel rectangle `[min_x, min_y, max_x, max_y]` covered by the opaque texels
/// of `image` placed at wire `(x, y)` with `rotation`.
pub fn expected_map_rect(image: &Image, x: i8, y: i8, rotation: u8) -> [f32; 4] {
    let mut rect = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
    for row in 0..image.height {
        for col in 0..image.width {
            if !is_opaque(image, col, row) {
                continue;
            }
            // Texel (col, row) covers sprite x in [col - 4, col - 3] and, with the
            // top row at the lower edge, y in [3 - row, 4 - row].
            for (sx, sy) in [
                (col as f32 - 4.0, 3.0 - row as f32),
                (col as f32 - 3.0, 3.0 - row as f32),
                (col as f32 - 4.0, 4.0 - row as f32),
                (col as f32 - 3.0, 4.0 - row as f32),
            ] {
                let (mx, my) = to_map(sx, sy, x, y, rotation);
                rect = [rect[0].min(mx), rect[1].min(my), rect[2].max(mx), rect[3].max(my)];
            }
        }
    }
    rect
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Bounds {
    pub count: usize,
    pub min_x: u32,
    pub max_x: u32,
    pub min_y: u32,
    pub max_y: u32,
}

impl Bounds {
    /// Screen position of map-pixel `(mx, my)` when this box is the whole 128x128
    /// picture seen face-on.
    pub fn at(&self, mx: f32, my: f32) -> (f32, f32) {
        let width = (self.max_x - self.min_x + 1) as f32;
        let height = (self.max_y - self.min_y + 1) as f32;
        (
            self.min_x as f32 + mx / 128.0 * width,
            self.min_y as f32 + my / 128.0 * height,
        )
    }
}

pub fn differs(a: &[u8], b: &[u8]) -> bool {
    (0..3).map(|c| (i32::from(a[c]) - i32::from(b[c])).abs()).sum::<i32>() > 8
}

/// Pixels in `keep` that differ from `reference`.
pub fn changed(target: Target, subject: &[u8], reference: &[u8], keep: impl Fn(u32, u32) -> bool) -> Option<Bounds> {
    let mut out: Option<Bounds> = None;
    for (i, (a, b)) in subject.chunks_exact(4).zip(reference.chunks_exact(4)).enumerate() {
        let (x, y) = ((i as u32) % target.w, (i as u32) / target.w);
        if !differs(a, b) || !keep(x, y) {
            continue;
        }
        let c = out.get_or_insert(Bounds { count: 0, min_x: x, max_x: x, min_y: y, max_y: y });
        c.count += 1;
        c.min_x = c.min_x.min(x);
        c.max_x = c.max_x.max(x);
        c.min_y = c.min_y.min(y);
        c.max_y = c.max_y.max(y);
    }
    out
}

/// Where `rect` (map pixels) falls on screen given the picture's screen box.
pub fn to_screen(picture: Bounds, rect: [f32; 4]) -> [f32; 4] {
    let (x0, y0) = picture.at(rect[0], rect[1]);
    let (x1, y1) = picture.at(rect[2], rect[3]);
    [x0, y0, x1, y1]
}

/// The changed pixels sit where the placement rules predict, within a couple of
/// pixels.
pub fn assert_box(label: &str, got: Bounds, want: [f32; 4]) {
    let tolerance = 2.5;
    let ok = (got.min_x as f32 - want[0]).abs() <= tolerance
        && (got.min_y as f32 - want[1]).abs() <= tolerance
        && ((got.max_x + 1) as f32 - want[2]).abs() <= tolerance
        && ((got.max_y + 1) as f32 - want[3]).abs() <= tolerance;
    assert!(
        ok,
        "{label}: icon pixels {got:?} but the placement rules predict x {}..{}, y {}..{}",
        want[0], want[2], want[1], want[3]
    );
}

/// How well the sprite's opaque and transparent texels line up with the pixels
/// that changed: the fraction of opaque texel centres that changed minus the
/// fraction of transparent ones that did.
fn texel_score(
    target: Target,
    image: &Image,
    placement: (i8, i8, u8),
    picture: Bounds,
    subject: &[u8],
    reference: &[u8],
) -> f32 {
    let (x, y, rotation) = placement;
    let (mut opaque, mut opaque_changed, mut clear, mut clear_changed) = (0u32, 0u32, 0u32, 0u32);
    for row in 0..image.height {
        for col in 0..image.width {
            let (mx, my) = to_map(col as f32 + 0.5 - 4.0, 3.5 - row as f32, x, y, rotation);
            let (sx, sy) = picture.at(mx, my);
            let i = ((sy.floor() as u32 * target.w + sx.floor() as u32) * 4) as usize;
            let moved = differs(&subject[i..i + 4], &reference[i..i + 4]);
            if is_opaque(image, col, row) {
                opaque += 1;
                opaque_changed += u32::from(moved);
            } else {
                clear += 1;
                clear_changed += u32::from(moved);
            }
        }
    }
    opaque_changed as f32 / opaque.max(1) as f32 - clear_changed as f32 / clear.max(1) as f32
}

fn flipped_vertically(image: &Image) -> Image {
    let row_bytes = (image.width * 4) as usize;
    let rgba = image.rgba.chunks_exact(row_bytes).rev().flatten().copied().collect();
    Image { width: image.width, height: image.height, rgba }
}

/// The picture shows the sprite as placed: its overlay score is high, and clearly
/// higher than the same sprite upside down. Every sprite used is asymmetric top
/// to bottom, so the upside-down reading is a different shape.
pub fn assert_orientation(
    target: Target,
    label: &str,
    name: &str,
    placement: (i8, i8, u8),
    picture: Bounds,
    subject: &[u8],
    reference: &[u8],
) {
    let image = sprite(name);
    let right = texel_score(target, &image, placement, picture, subject, reference);
    let upside_down = texel_score(target, &flipped_vertically(&image), placement, picture, subject, reference);
    eprintln!("{label}: overlay score {right:.2}, upside down {upside_down:.2}");
    assert!(
        right >= 0.45 && right >= upside_down + 0.12,
        "{label}: overlay score {right:.2} against {upside_down:.2} for the upside-down sprite; \
         the sprite is drawn flipped or misplaced"
    );
}
