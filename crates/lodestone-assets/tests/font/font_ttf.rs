//! Hermetic gates for the `ttf` glyph provider
//! ([`lodestone_assets::font::FontLoader::load`]'s `ProviderDef::Ttf` arm and
//! the [`lodestone_assets::font::Glyph::Ttf`]/[`TtfGlyph`] it produces), the
//! last box in "full Unicode text" scope — `unihex` and bidi
//! already landed.
//!
//! # Why the fixture is hand-built rather than a real font file
//!
//! Vanilla's own `default.json` declares no `ttf` provider (it is a
//! resource-pack addition, not something the base game ships), so there is no
//! committed vanilla font to gate against the way the unihex tests gate
//! against `unifont.zip`. Every glyph and table below is instead a minimal,
//! hand-assembled `sfnt` (`build_font`), which buys two things a downloaded
//! font would not: every advance and outline is a number *we* chose (so the
//! spec-derived expected value in each assertion is arithmetic, not a
//! transcription of someone else's font), and the fixture is committed inline
//! rather than as an opaque binary blob nobody can diff.
//!
//! # The expected values come from outside `lodestone_assets::font`
//!
//! Two independent sources, per assertion:
//!
//! - **The font's own tables, read by [`ttf_parser`]** — a different crate
//!   than the [`fontdue`] the production loader uses — for `hmtx` advance
//!   widths and `glyf` bounding boxes, combined with the OpenType spec's own
//!   `scale = px / unitsPerEm` formula (also independently confirmed against
//!   `fontdue::Font::metrics_indexed`'s own `scale_factor`, which is exactly
//!   that formula — see `fontdue::Font::scale_factor`). This is the "font
//!   file's own tables read independently" option from the evidence
//!   standards, not `decode(encode(x)) == x`.
//! - **The outline's own geometry**, for orientation: a glyph built with ink
//!   in only the *upper* half of its bounding box must rasterise ink only in
//!   the *top* rows of the bitmap, and no ink in the bottom rows — true by
//!   construction regardless of any rasteriser's internal rounding, so it
//!   catches a Y-axis flip (glyph space grows up; screen/bitmap space grows
//!   down) that a purely numeric oracle keyed to the same formula the
//!   production code uses could not.
//!
//! Mismatches are **collected**, not asserted inside the loop that finds them
//! — an `assert!` inside a `for` stops at the first failure and leaves every
//! later row unmeasured.

use lodestone_assets::font::{Font, FontLoader, FontOptions, Glyph, RasterFont};
use lodestone_assets::{MemorySource, ResourceLocation, ResourceManager};

// --- A minimal hand-built sfnt -------------------------------------------

fn be16(v: u16) -> [u8; 2] {
    v.to_be_bytes()
}
fn be16i(v: i16) -> [u8; 2] {
    v.to_be_bytes()
}
fn be32(v: u32) -> [u8; 4] {
    v.to_be_bytes()
}

fn pad4(buf: &mut Vec<u8>) {
    while buf.len() % 4 != 0 {
        buf.push(0);
    }
}

fn checksum(data: &[u8]) -> u32 {
    let mut sum: u32 = 0;
    for c in data.chunks(4) {
        let mut arr = [0u8; 4];
        arr[..c.len()].copy_from_slice(c);
        sum = sum.wrapping_add(u32::from_be_bytes(arr));
    }
    sum
}

/// One glyph to bake into the fixture face.
struct GlyphSpec {
    ch: char,
    /// `hmtx` advance width, in font units.
    advance: u16,
    /// Zero or more closed, all-on-curve contours (font-unit coordinates).
    /// Zero contours means a glyph with no outline at all (`loca[n] ==
    /// loca[n+1]`, the standard encoding for a blank glyph like space).
    /// **More than one contour matters here, not just for letterforms with
    /// holes**: a single filled rectangle's rasterised cell is *always*
    /// entirely ink, because the cell is the tight bounding box of the ink
    /// itself — so it cannot discriminate "ink in the top of the cell" from
    /// "ink in the bottom", which is exactly the property the orientation
    /// gate needs. Two disjoint contours (a dense block, a sparse one, with a
    /// real gap between them) give the cell internal structure a single
    /// rectangle cannot.
    outline: Vec<Vec<(i16, i16)>>,
}

/// Encodes one simple TrueType glyph: one or more contours, every point
/// on-curve.
fn simple_glyph_bytes(contours: &[Vec<(i16, i16)>]) -> Vec<u8> {
    let all_points: Vec<(i16, i16)> = contours.iter().flatten().copied().collect();
    if all_points.is_empty() {
        return Vec::new();
    }
    let (mut xmin, mut ymin, mut xmax, mut ymax) = (i16::MAX, i16::MAX, i16::MIN, i16::MIN);
    for &(x, y) in &all_points {
        xmin = xmin.min(x);
        ymin = ymin.min(y);
        xmax = xmax.max(x);
        ymax = ymax.max(y);
    }
    let mut g = Vec::new();
    g.extend_from_slice(&be16i(contours.len() as i16)); // numberOfContours
    g.extend_from_slice(&be16i(xmin));
    g.extend_from_slice(&be16i(ymin));
    g.extend_from_slice(&be16i(xmax));
    g.extend_from_slice(&be16i(ymax));
    // endPtsOfContours: the cumulative point index (0-based) of each
    // contour's last point.
    let mut running = 0i32;
    for contour in contours {
        running += contour.len() as i32;
        g.extend_from_slice(&be16((running - 1) as u16));
    }
    g.extend_from_slice(&be16(0)); // instructionLength
    for _ in &all_points {
        g.push(0x01); // ON_CURVE_POINT, full-width deltas (no repeat/short flags)
    }
    // Deltas chain across contour boundaries too — the spec encodes every
    // point's coordinate as a delta from the *immediately preceding* point,
    // full stop, regardless of which contour either belongs to.
    let mut prev = 0i16;
    for &(x, _) in &all_points {
        g.extend_from_slice(&be16i(x - prev));
        prev = x;
    }
    let mut prev = 0i16;
    for &(_, y) in &all_points {
        g.extend_from_slice(&be16i(y - prev));
        prev = y;
    }
    g
}

/// Builds a minimal valid `sfnt` (TrueType outlines, `cmap` format 6 covering
/// one contiguous code range) holding `.notdef` plus every glyph in `specs`,
/// in order. `.notdef` itself is always empty.
///
/// Required tables only: `cmap`, `glyf`, `head`, `hhea`, `hmtx`, `loca`,
/// `maxp` — no `name`/`post`/`OS/2`. `ttf-parser` and `fontdue` both parse
/// this; a real vanilla/resource-pack font naturally carries more tables, but
/// none of them affect glyph outlines or `hmtx` advances.
fn build_font(units_per_em: u16, specs: &[GlyphSpec]) -> Vec<u8> {
    assert!(!specs.is_empty());
    let codes: Vec<u32> = specs.iter().map(|s| s.ch as u32).collect();
    let first_code = *codes.iter().min().unwrap();
    let last_code = *codes.iter().max().unwrap();

    let mut glyf = Vec::new();
    // `loca` needs `numGlyphs + 1` entries (a start *and* end offset per
    // glyph, chained). Glyph 0 (`.notdef`) contributes zero bytes, so it
    // needs its own explicit start-and-end pair before the loop — folding it
    // into the loop's first push would silently alias `.notdef`'s span onto
    // the first real glyph's, off by one for every glyph after it.
    let mut loca_offsets = vec![0u32, 0u32];
    for spec in specs {
        glyf.extend_from_slice(&simple_glyph_bytes(&spec.outline));
        pad4(&mut glyf);
        loca_offsets.push(glyf.len() as u32);
    }
    let num_glyphs = (specs.len() + 1) as u16;

    let mut loca = Vec::new();
    for off in &loca_offsets {
        loca.extend_from_slice(&be16((off / 2) as u16));
    }

    let mut head = Vec::new();
    head.extend_from_slice(&be16(1));
    head.extend_from_slice(&be16(0));
    head.extend_from_slice(&be32(0x00010000));
    head.extend_from_slice(&be32(0));
    head.extend_from_slice(&be32(0x5F0F3CF5));
    head.extend_from_slice(&be16(0b0000_0000_0000_0011));
    head.extend_from_slice(&be16(units_per_em));
    head.extend_from_slice(&[0u8; 8]);
    head.extend_from_slice(&[0u8; 8]);
    head.extend_from_slice(&be16i(0));
    head.extend_from_slice(&be16i(0));
    head.extend_from_slice(&be16i(units_per_em as i16));
    head.extend_from_slice(&be16i(units_per_em as i16));
    head.extend_from_slice(&be16(0));
    head.extend_from_slice(&be16(8));
    head.extend_from_slice(&be16i(2));
    head.extend_from_slice(&be16i(0)); // indexToLocFormat: short
    head.extend_from_slice(&be16i(0));

    let advance_max = specs.iter().map(|s| s.advance).max().unwrap_or(0);
    let mut hhea = Vec::new();
    hhea.extend_from_slice(&be16(1));
    hhea.extend_from_slice(&be16(0));
    hhea.extend_from_slice(&be16i(units_per_em as i16));
    hhea.extend_from_slice(&be16i(-(units_per_em as i16) / 4));
    hhea.extend_from_slice(&be16i(0));
    hhea.extend_from_slice(&be16(advance_max));
    hhea.extend_from_slice(&be16i(0));
    hhea.extend_from_slice(&be16i(0));
    hhea.extend_from_slice(&be16i(units_per_em as i16));
    hhea.extend_from_slice(&be16i(1));
    hhea.extend_from_slice(&be16i(0));
    hhea.extend_from_slice(&be16i(0));
    hhea.extend_from_slice(&[0u8; 8]);
    hhea.extend_from_slice(&be16i(0));
    hhea.extend_from_slice(&be16(num_glyphs)); // numberOfHMetrics = every glyph has its own

    let mut hmtx = Vec::new();
    hmtx.extend_from_slice(&be16(0)); // .notdef advance
    hmtx.extend_from_slice(&be16i(0));
    for spec in specs {
        hmtx.extend_from_slice(&be16(spec.advance));
        hmtx.extend_from_slice(&be16i(0));
    }

    let mut maxp = Vec::new();
    maxp.extend_from_slice(&be32(0x00010000));
    maxp.extend_from_slice(&be16(num_glyphs));
    maxp.extend_from_slice(&be16(16)); // maxPoints (generous)
    maxp.extend_from_slice(&be16(4)); // maxContours (generous — A uses 2)
    maxp.extend_from_slice(&be16(0));
    maxp.extend_from_slice(&be16(0));
    maxp.extend_from_slice(&be16(2));
    maxp.extend_from_slice(&be16(0));
    maxp.extend_from_slice(&be16(0));
    maxp.extend_from_slice(&be16(0));
    maxp.extend_from_slice(&be16(0));
    maxp.extend_from_slice(&be16(0));
    maxp.extend_from_slice(&be16(0));
    maxp.extend_from_slice(&be16(0));
    maxp.extend_from_slice(&be16(0));

    // cmap format 6 (trimmed table), platform 3 (Windows) encoding 1
    // (Unicode BMP) — a single contiguous range covering every fixture
    // codepoint, glyph id 0 (missing) for anything in the range this fixture
    // does not use.
    let entry_count = (last_code - first_code + 1) as u16;
    let mut glyph_ids = vec![0u16; entry_count as usize];
    for (i, spec) in specs.iter().enumerate() {
        glyph_ids[(spec.ch as u32 - first_code) as usize] = (i + 1) as u16;
    }
    let mut cmap_sub = Vec::new();
    cmap_sub.extend_from_slice(&be16(6));
    cmap_sub.extend_from_slice(&be16(10 + entry_count * 2));
    cmap_sub.extend_from_slice(&be16(0));
    cmap_sub.extend_from_slice(&be16(first_code as u16));
    cmap_sub.extend_from_slice(&be16(entry_count));
    for id in glyph_ids {
        cmap_sub.extend_from_slice(&be16(id));
    }
    let mut cmap = Vec::new();
    cmap.extend_from_slice(&be16(0));
    cmap.extend_from_slice(&be16(1));
    cmap.extend_from_slice(&be16(3));
    cmap.extend_from_slice(&be16(1));
    cmap.extend_from_slice(&be32(12));
    cmap.extend_from_slice(&cmap_sub);

    let mut tables = vec![
        (*b"cmap", cmap),
        (*b"glyf", glyf),
        (*b"head", head),
        (*b"hhea", hhea),
        (*b"hmtx", hmtx),
        (*b"loca", loca),
        (*b"maxp", maxp),
    ];
    tables.sort_by_key(|(tag, _)| *tag);

    let num_tables = tables.len() as u16;
    let mut out = Vec::new();
    out.extend_from_slice(&be32(0x00010000));
    out.extend_from_slice(&be16(num_tables));
    let search_range = {
        let mut p = 1u16;
        while (p * 2) as u32 <= num_tables as u32 {
            p *= 2;
        }
        p * 16
    };
    out.extend_from_slice(&be16(search_range));
    out.extend_from_slice(&be16((num_tables as f32).log2().floor() as u16));
    out.extend_from_slice(&be16(num_tables * 16 - search_range));

    let header_len = 12 + 16 * num_tables as usize;
    let mut offset = header_len;
    let mut dir = Vec::new();
    let mut body = Vec::new();
    for (tag, data) in &tables {
        let mut padded = data.clone();
        pad4(&mut padded);
        dir.extend_from_slice(tag);
        dir.extend_from_slice(&be32(checksum(&padded)));
        dir.extend_from_slice(&be32(offset as u32));
        dir.extend_from_slice(&be32(data.len() as u32));
        offset += padded.len();
        body.extend_from_slice(&padded);
    }
    out.extend_from_slice(&dir);
    out.extend_from_slice(&body);
    out
}

/// The fixture face: 1000 units/em, four real codepoints plus `.notdef`.
///
/// | glyph | codepoint | outline | why |
/// |---|---|---|---|
/// | `A` | U+0041 | two contours: a wide dense block in the top of the box (y 500..700), a narrow sparse one at the very bottom (y 0..100), a real gap between | orientation + metrics |
/// | `B` | U+0042 | full square, different bounds/advance from `A` | also declared in a `bitmap` sheet — priority, and a discriminating decoy |
/// | `C` | U+0043 | none (0 contours) | `TrueTypeGlyphProvider`'s `EmptyGlyph` case |
/// | `S` | U+0053 | full square | listed in the pack's `skip` |
///
/// `A` is two contours rather than one rectangle on purpose: a single filled
/// rectangle's rasterised cell is *always* entirely ink (the cell is the
/// tight bounding box of the ink itself), so it cannot show "more ink near
/// the top than the bottom" — there is no way for a filled rectangle's own
/// bounding box to contain a part of itself with less ink. Two disjoint
/// blocks of very different size, with a real empty band between them, do.
fn fixture_specs() -> Vec<GlyphSpec> {
    vec![
        GlyphSpec {
            ch: 'A',
            advance: 800,
            outline: vec![
                // Dense block: the top 200 units of the 700-unit box, full width.
                vec![(100, 500), (900, 500), (900, 700), (100, 700)],
                // Sparse block: the bottom 100 units, a narrow sliver — present
                // only so the glyph's bounding box reaches down to y=0 without
                // filling anywhere near as much of the cell as the top block.
                vec![(480, 0), (520, 0), (520, 100), (480, 100)],
            ],
        },
        GlyphSpec {
            // Deliberately different advance/x-range from `A`'s: if a glyph
            // table bug ever aliased `A`'s data onto `B`'s (or vice versa —
            // this is exactly the "loca" off-by-one this fixture builder
            // itself once had), the metrics test would catch it too, not
            // just the orientation test.
            ch: 'B',
            advance: 640,
            outline: vec![vec![(50, 0), (950, 0), (950, 700), (50, 700)]],
        },
        GlyphSpec {
            ch: 'C',
            advance: 300,
            outline: Vec::new(),
        },
        GlyphSpec {
            ch: 'S',
            advance: 500,
            outline: vec![vec![(200, 0), (800, 0), (800, 700), (200, 700)]],
        },
    ]
}

const UNITS_PER_EM: u16 = 1000;

fn fixture_font_bytes() -> Vec<u8> {
    build_font(UNITS_PER_EM, &fixture_specs())
}

/// A 2-cell 8×8 bitmap sheet: cell 0 is an unrelated filler codepoint (`X`,
/// used by nothing else in this fixture), cell 1 is `B` — declared ahead of
/// the `ttf` provider, so `B` must resolve to [`Glyph::Bitmap`] and never
/// reach the face at all. `A` is deliberately absent from this sheet so it
/// stays exclusively a `ttf` glyph.
fn sheet_png() -> Vec<u8> {
    let (w, h) = (16u32, 8u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..4u32 {
            let i = ((y * w + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, w, h);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().expect("png header");
    writer.write_image_data(&rgba).expect("png data");
    drop(writer);
    out
}

/// The font definition: a `space` advance, a bitmap sheet (declaring `A` and
/// `B`), then the `ttf` provider — decreasing priority, vanilla's own order —
/// with `size`/`oversample` chosen so `pixels_per_em = round(size *
/// oversample)` is easy to hand-check (20 * 2 = 40), and `S` in `skip`.
fn font_json() -> String {
    format!(
        r#"{{ "providers": [
            {{ "type": "space", "advances": {{ " ": 4 }} }},
            {{ "type": "bitmap", "file": "minecraft:font/sheet.png", "ascent": 7,
               "chars": ["XB"] }},
            {{ "type": "ttf", "file": "minecraft:fixture.ttf", "size": {SIZE},
               "oversample": {OVERSAMPLE}, "shift": [{SHIFT_X}, {SHIFT_Y}],
               "skip": "S" }}
        ] }}"#
    )
}

const SIZE: f32 = 20.0;
const OVERSAMPLE: f32 = 2.0;
const SHIFT_X: f32 = 1.0;
const SHIFT_Y: f32 = -0.5;

fn fixture(with_ttf_bytes: bool) -> ResourceManager {
    let mut src = MemorySource::new("ttf-fixture");
    src.insert("assets/minecraft/font/default.json", font_json().into_bytes());
    src.insert("assets/minecraft/textures/font/sheet.png", sheet_png());
    if with_ttf_bytes {
        // Vanilla's own true-type-glyph-provider-definition load step: `resourceManager.open(this.location.withPrefix("font/"))`
        // — the `file` field itself carries no `font/` prefix, unlike `unihex`'s `hex_file`.
        src.insert("assets/minecraft/font/fixture.ttf", fixture_font_bytes());
    }
    ResourceManager::new(vec![Box::new(src)])
}

fn load(with_ttf_bytes: bool) -> Font {
    let manager = fixture(with_ttf_bytes);
    let id: ResourceLocation = "minecraft:default".parse().expect("valid id");
    FontLoader::new(&manager)
        .load(&id, &FontOptions::none())
        .expect("the fixture font loads")
}

fn load_raster() -> RasterFont {
    let manager = fixture(true);
    let id: ResourceLocation = "minecraft:default".parse().expect("valid id");
    FontLoader::new(&manager)
        .load_raster(&id, &FontOptions::none())
        .expect("the fixture font loads")
}

/// Independent oracle: `hmtx` advance (read by `ttf-parser`, not `fontdue`)
/// scaled by the OpenType spec's own `px / unitsPerEm`, then divided by
/// oversample — the same arithmetic vanilla's own true-type-glyph-provider Java
/// does with
/// FreeType's own scaled-advance value, just performed by hand here.
fn expected_advance(ch: char) -> f32 {
    let bytes = fixture_font_bytes();
    let face = ttf_parser::Face::parse(&bytes, 0).expect("ttf-parser parses the fixture");
    let gid = face.glyph_index(ch).expect("fixture face maps this char");
    let units = face
        .glyph_hor_advance(gid)
        .expect("fixture glyph has hmtx") as f32;
    let pixels_per_em = (SIZE * OVERSAMPLE).round();
    units * pixels_per_em / face.units_per_em() as f32 / OVERSAMPLE
}

#[test]
fn ttf_metrics_match_the_font_tables_read_independently() {
    let font = load(true);
    let glyph = font
        .ttf_glyph('A' as u32)
        .expect("A is a rasterised ttf glyph, not bitmap/space");

    let mut mismatches = Vec::new();
    let expected_adv = expected_advance('A');
    if (glyph.advance - expected_adv).abs() > 0.05 {
        mismatches.push(format!(
            "advance: got {}, expected {expected_adv} (from ttf-parser's hmtx + spec scale)",
            glyph.advance
        ));
    }
    // pixels_per_em = round(20 * 2) = 40, exactly representable.
    if (glyph.pixels_per_em - 40.0).abs() > f32::EPSILON {
        mismatches.push(format!(
            "pixels_per_em: got {}, expected 40 = round(size * oversample)",
            glyph.pixels_per_em
        ));
    }
    // shift.x = +1.0 folds straight into bearing_left; shift.y = -0.5 folds
    // in negated (glyph space grows up, screen space grows down), matching
    // `TrueTypeGlyphProvider`'s `transformY = -shiftY * oversample`. Both
    // hand-derived from the outline's own corners (xmin 100, ymin 350, ymax
    // 700 font units) at scale 40/1000 = 0.04, which happens to land on
    // exact integers at every rounding step, so the tolerance here is tight:
    // bearing_left = floor(100 * 0.04)/2 + 1.0 = 2.0 + 1.0 = 3.0;
    // bearing_top = (floor(350*0.04) + ceil((700-350)*0.04))/2 - (-0.5)
    //             = (14 + 14)/2 + 0.5 = 14.5.
    if (glyph.bearing_left - 3.0).abs() > 0.01 {
        mismatches.push(format!(
            "bearing_left: got {}, expected 3.0 (2.0 from the outline's xmin + shift.x 1.0)",
            glyph.bearing_left
        ));
    }
    if (glyph.bearing_top - 14.5).abs() > 0.01 {
        mismatches.push(format!(
            "bearing_top: got {}, expected 14.5",
            glyph.bearing_top
        ));
    }
    assert!(
        mismatches.is_empty(),
        "ttf glyph metrics diverged from the independent oracle:\n{}",
        mismatches.join("\n")
    );
}

/// Orientation control: `A`'s outline is a wide, dense block in the top 200
/// units of its 700-unit bounding box, a narrow sparse block in the bottom
/// 100 units, and a genuine 200-unit gap of zero ink between them. If the
/// rasterised bitmap's row order or the [`GlyphRaster::top`] bearing were
/// flipped end-to-end, the *dense* block would land in the bottom rows and
/// the *sparse* one in the top — this is a class of bug no purely numeric
/// oracle keyed to the same formula as the code under test could catch (the
/// same reasoning `DESIGN.md` gives for why `decode(encode(x)) == x` is
/// worthless here). A single filled rectangle cannot make this assertion at
/// all: its rasterised cell is always entirely ink by construction, so
/// nothing about the fixture must be a solid rectangle in order to be able to
/// say "less ink over here than over there".
#[test]
fn ttf_glyph_orientation_puts_the_dense_block_in_the_top_rows() {
    let raster = load_raster();
    let r = raster
        .raster('A' as u32)
        .expect("A rasterises to a drawable ttf glyph");
    let h = r.cell_height();
    let w = r.cell_width();
    assert!(h >= 8 && w >= 8, "fixture glyph should be several px across");

    let mut rows_with_ink: Vec<u32> = Vec::new();
    let mut top_ink = 0usize;
    let mut bottom_ink = 0usize;
    for ty in 0..h {
        let mut row_ink = 0usize;
        for tx in 0..w {
            if r.is_ink(tx, ty) {
                row_ink += 1;
            }
        }
        if row_ink > 0 {
            rows_with_ink.push(ty);
        }
        if ty < h / 2 {
            top_ink += row_ink;
        } else {
            bottom_ink += row_ink;
        }
    }

    assert!(top_ink > 0, "expected ink in the top half (the dense block)");
    assert!(bottom_ink > 0, "expected ink in the bottom half (the sparse block)");
    assert!(
        top_ink > bottom_ink * 4,
        "the dense (wide) block sits above the sparse (narrow) one in font space, so the \
         top half must carry far more ink than the bottom half: got top={top_ink}, bottom={bottom_ink}"
    );

    // The real gap between the two blocks: some contiguous band of rows near
    // the middle must carry no ink at all.
    let min_inked = *rows_with_ink.iter().min().expect("some row has ink");
    let max_inked = *rows_with_ink.iter().max().expect("some row has ink");
    let gap_rows = (min_inked..=max_inked)
        .filter(|ty| !rows_with_ink.contains(ty))
        .count();
    assert!(
        gap_rows > 0,
        "expected an empty band of rows between the two blocks, found none \
         (inked rows span {min_inked}..={max_inked} with no gap)"
    );
}

#[test]
fn ttf_glyph_loses_priority_to_an_earlier_bitmap_provider() {
    let font = load(true);
    match font
        .glyph('B' as u32)
        .expect("B is covered by both the bitmap sheet and the ttf face")
    {
        Glyph::Bitmap(_) => {}
        other => panic!("B should resolve to the earlier-declared bitmap provider, got {other:?}"),
    }
    assert!(
        font.ttf_glyph('B' as u32).is_none(),
        "the ttf provider's own B must have been skipped as already-won"
    );
}

#[test]
fn ttf_zero_area_glyph_is_an_advance_only_space_not_a_missing_box() {
    let font = load(true);
    match font.glyph('C' as u32).expect("C is in the face's cmap") {
        Glyph::Space { advance } => {
            let expected = expected_advance('C');
            assert!(
                (*advance - expected).abs() < 0.05,
                "C's advance-only glyph: got {advance}, expected {expected}"
            );
        }
        other => panic!("a zero-contour ttf glyph should fall back to Glyph::Space, got {other:?}"),
    }
    let raster = load_raster();
    assert!(
        raster.raster('C' as u32).is_none(),
        "an advance-only glyph draws no pixels, same as an explicit space provider entry"
    );
}

#[test]
fn ttf_skip_list_excludes_a_codepoint_the_face_actually_maps() {
    // Control: prove the face really does map 'S' to a real glyph, so the
    // absence below is the skip list working and not an absent-anyway
    // codepoint.
    let bytes = fixture_font_bytes();
    let face = ttf_parser::Face::parse(&bytes, 0).expect("parses");
    assert!(
        face.glyph_index('S').is_some_and(|g| g.0 != 0),
        "control: the fixture face must map S to a real glyph for the skip test to mean anything"
    );

    let font = load(true);
    assert!(
        !font.contains('S' as u32),
        "S is listed in the ttf provider's \"skip\" and no other provider supplies it"
    );
}

#[test]
fn ttf_provider_with_no_font_file_is_a_soft_skip() {
    // The pack declares the ttf provider but the referenced file is absent —
    // same soft-skip contract as unihex's missing hex_file: the load still
    // succeeds, just without that provider's glyphs.
    let font = load(false);
    assert_eq!(font.ttf_count(), 0);
    // The bitmap sheet's own glyphs (declared earlier) are unaffected.
    assert!(matches!(font.glyph('B' as u32), Some(Glyph::Bitmap(_))));
    // Nothing else supplies A, C or S, so they are genuinely absent — not a
    // silent fallback to some other provider that would mask the soft skip.
    assert!(!font.contains('A' as u32));
    assert!(!font.contains('C' as u32));
}

/// End-to-end: pack definition -> `ttf` provider -> [`RasterFont`] -> ink a
/// consumer can walk, the same chain `docs/ttf-font-glyphs.md` traces. This is
/// the assertion that closes the "reaches no pixels" island: a provider that
/// merely parsed would leave `raster.raster('A' as u32)` at `None` forever.
#[test]
fn ttf_reaches_a_drawable_raster_end_to_end() {
    let raster = load_raster();
    assert_eq!(raster.ttf_face_count(), 1);
    // The face's cmap covers A, B, C, S. B loses to the earlier bitmap
    // provider; S is skip-listed. That leaves A (drawable) and C
    // (zero-area, still "won" — see `Font::ttf_count`'s doc) = 2.
    assert_eq!(raster.ttf_count(), 2);
    let r = raster.raster('A' as u32).expect("A is drawable");
    assert!(r.advance() > 0.0);
    assert!((0..r.cell_width()).any(|tx| (0..r.cell_height()).any(|ty| r.is_ink(tx, ty))));
}
