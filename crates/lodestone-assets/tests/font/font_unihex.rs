//! Hermetic gates for the `unihex` glyph provider
//! ([`lodestone_assets::font::read_hex_entries`] and the loader path that turns
//! its output into [`lodestone_assets::font::Glyph::Unihex`]).
//!
//! # Why this file exists, and what the corpus was missing before it
//!
//! Every font gate in this crate before this one measured a codepoint the three
//! vanilla **bitmap sheets** already cover: `tests/font.rs` uses synthetic cells
//! plus `§` (U+00A7) and `—` (U+2014); `tests/vanilla_font_metrics.rs` adds `Á`
//! (U+00C1) and `—`; the shell's `hud/vanilla_font.rs` gates use `§`, `×`
//! (U+00D7) and `—`. All of them are inside the 2,414 codepoints the sheets
//! supply, so **no assertion in the corpus could distinguish a font with a
//! working unihex provider from one with none** — the bitmap providers are
//! declared first and win either way. That is the blind spot this file closes,
//! and it is why "the squares" survived a green suite.
//!
//! # The expected values come from the record, not from the code
//!
//! Every advance and bound below is hand-derived from `UnihexProvider` in
//! `.cache/mc/26.2/client-src`, and each case quotes the arithmetic. Two of the
//! HEX lines are copied verbatim out of vanilla's own `unifont.zip`
//! (`unifont_all_no_pua-17.0.01.hex`), so the *input* is vanilla's too.
//!
//! Where a value could be produced by two readings of the record, the input is
//! chosen so the two readings **disagree**, and the test says what the wrong one
//! would give. A codepoint whose override and derived bounds coincided would pass
//! with `size_overrides` ignored entirely.

use lodestone_assets::font::{Font, FontLoader, FontOptions, Glyph, read_hex_entries};
use lodestone_assets::{MemorySource, ResourceLocation, ResourceManager};
use std::io::Write;

/// Builds a zip holding one `.hex` member, the way `unifont.zip` is shaped.
fn hex_zip(body: &str) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut w = zip::ZipWriter::new(std::io::Cursor::new(&mut out));
        w.start_file("fixture.hex", zip::write::SimpleFileOptions::default())
            .expect("start hex member");
        w.write_all(body.as_bytes()).expect("write hex member");
        // A member the loader must ignore: only `.hex` entries are read
        // (`UnihexProvider.Definition.loadData`'s `name.endsWith(".hex")`), and
        // a real `unifont.zip` does ship a `LICENSE.txt` beside the data. If
        // this were read, the whole load would fail on a non-hex digit.
        w.start_file("LICENSE.txt", zip::write::SimpleFileOptions::default())
            .expect("start license");
        w.write_all(b"not hex data at all\n").expect("write license");
        w.finish().expect("finish zip");
    }
    out
}

/// The fixture's HEX payload, one entry per line.
///
/// | codepoint | digits | width | why it is here |
/// |---|---|---|---|
/// | U+0041 `A` | 32 | 8 | also an **inked** bitmap cell — priority |
/// | U+0042 `B` | 32 | 8 | also a **blank** bitmap cell — coverage, not ink, is what wins |
/// | U+2713 `✓` | 32 | 8 | verbatim from vanilla's `unifont.zip`; half-width |
/// | U+4E2D `中` | 64 | 16 | verbatim from vanilla's `unifont.zip`; full-width, inside an override |
/// | U+1234 | 96 | 24 | the `read24` arm, which vanilla's own file never exercises |
/// | U+1235 | 128 | 32 | the `read32` arm, likewise |
/// | U+2000 | 32 | 8 | all-empty, the `mask == 0` branch |
/// | U+30FF | 64 | 16 | last codepoint of the 3001..30FF override |
/// | U+3100 | 64 | 16 | **one past** it, with a byte-identical bitmap |
/// | U+5005 | 64 | 16 | inside two overlapping override ranges |
/// | U+2500 | 64 | 16 | inside no override range, so derived |
const HEX: &str = concat!(
    // 32 digits: 16 rows of one byte. Rows 14 and 15 are 0xFF.
    "0041:0000000000000000000000000000FFFF\n",
    "0042:0000000000000000000000000000FFFF\n",
    "2713:00000000010102024444282810100000\n",
    // 64 digits: 16 rows of one short.
    "4E2D:01000100010001003FF8210821082108210821083FF821080100010001000100\n",
    // 96 digits: 16 rows of six digits; row 5 is 018000, the rest 000000.
    "1234:000000000000000000000000000000018000000000000000000000000000",
    "000000000000000000000000000000000000\n",
    // 128 digits: 16 rows of eight digits; row 0 is 80000001, the rest zero.
    "1235:800000010000000000000000000000000000000000000000000000000000",
    "00000000000000000000000000000000000000000000000000000000000000000000\n",
    "2000:00000000000000000000000000000000\n",
    // 64 digits, row 0 = 0400, rest zero. Four codepoints, one bitmap.
    "30FF:0400000000000000000000000000000000000000000000000000000000000000\n",
    "3100:0400000000000000000000000000000000000000000000000000000000000000\n",
    "5005:0400000000000000000000000000000000000000000000000000000000000000\n",
    "2500:0400000000000000000000000000000000000000000000000000000000000000\n",
);

/// How many entries [`HEX`] holds.
const HEX_ENTRIES: usize = 11;
/// How many of them the unihex provider *wins*: all but `A` and `B`, which the
/// bitmap sheet declares first.
const UNIHEX_WON: usize = HEX_ENTRIES - 2;

/// One `size_overrides` range, as JSON.
///
/// `from`/`to` are built from `char::from_u32` rather than written as literals so
/// the boundaries are unambiguous in the source and cannot be mangled by an
/// editor or a `\u` escape that some tool decides to expand.
fn range(from: u32, to: u32, left: i32, right: i32) -> String {
    let esc = |cp: u32| {
        let ch = char::from_u32(cp).expect("test range boundary is a valid char");
        serde_json::to_string(&ch.to_string()).expect("string encodes")
    };
    format!(
        "{{ \"from\": {}, \"to\": {}, \"left\": {left}, \"right\": {right} }}",
        esc(from),
        esc(to)
    )
}

/// The font definition: a `space` provider, then a bitmap provider, then the
/// unihex provider — vanilla's own `default.json` order (decreasing priority),
/// which is what makes the priority assertions below mean anything.
fn font_json(with_unihex: bool) -> String {
    let space = char::from_u32(0x20).expect("space");
    let zwnj = char::from_u32(0x200C).expect("zwnj");
    let mut providers = vec![
        format!(
            "{{ \"type\": \"space\", \"advances\": {{ {}: 4, {}: 0 }} }}",
            serde_json::to_string(&space.to_string()).expect("encodes"),
            serde_json::to_string(&zwnj.to_string()).expect("encodes"),
        ),
        r#"{ "type": "bitmap", "file": "minecraft:font/sheet.png", "ascent": 7,
             "chars": ["AB"] }"#
            .to_string(),
    ];
    if with_unihex {
        providers.push(format!(
            r#"{{ "type": "unihex", "hex_file": "minecraft:font/unifont.zip",
                  "size_overrides": [{}, {}, {}, {}, {}] }}"#,
            // The boundary range: U+30FF is its last member, U+3100 is one past.
            range(0x3001, 0x30FF, 0, 15),
            // Two identical ranges with different bounds, to pin which wins.
            range(0x5000, 0x5010, 0, 3),
            range(0x5000, 0x5010, 0, 7),
            // The CJK range that makes 中 full-width.
            range(0x4E00, 0x9FFF, 0, 15),
            // A range naming codepoints the payload does not contain.
            range(0xE000, 0xE010, 0, 15),
        ));
    }
    format!("{{ \"providers\": [{}] }}", providers.join(","))
}

/// A 2-cell 8×8 sheet: cell 0 (`A`) has ink out to column 4, cell 1 (`B`) is
/// blank. So the bitmap advance for `A` is `(0.5 + 5 * 1.0) as i32 + 1 = 6` and
/// for `B` it is `0 + 1 = 1` — neither reachable from this fixture's unihex arm,
/// so "which provider won" is readable straight off the advance.
fn sheet_png() -> Vec<u8> {
    let (w, h) = (16u32, 8u32);
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..=4u32 {
            let i = ((y * w + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&[255, 255, 255, 255]);
        }
    }
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, w, h);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().expect("png header");
        writer.write_image_data(&rgba).expect("png data");
    }
    out
}

/// The fixture pack. `with_zip` false omits `unifont.zip` entirely, which is the
/// store-less install.
fn fixture(with_unihex: bool, with_zip: bool) -> ResourceManager {
    let mut src = MemorySource::new("unihex-fixture");
    src.insert(
        "assets/minecraft/font/default.json",
        font_json(with_unihex).into_bytes(),
    );
    src.insert("assets/minecraft/textures/font/sheet.png", sheet_png());
    if with_zip {
        src.insert("assets/minecraft/font/unifont.zip", hex_zip(HEX));
    }
    ResourceManager::new(vec![Box::new(src)])
}

fn load(with_unihex: bool) -> Font {
    let manager = fixture(with_unihex, true);
    let id: ResourceLocation = "minecraft:default".parse().expect("valid id");
    FontLoader::new(&manager)
        .load(&id, &FontOptions::none())
        .expect("the fixture font loads")
}

/// A `unihex` glyph's advance is `width / 2 + 1` with `width = right - left + 1`
/// (`UnihexProvider.Glyph.width` plus the anonymous `GlyphInfo` in
/// `UnihexProvider.Glyph.info`), and the bounds come from the ink's own extent
/// (`LineData.calculateWidth`) unless a `size_overrides` range replaces them.
///
/// Mismatches are **collected**, not asserted in the loop: an `assert!` inside a
/// `for` stops at the first failure, so one wrong arm would leave every later row
/// unmeasured and its correctness a guess.
#[test]
fn unihex_metrics_match_the_hand_derived_record() {
    let font = load(true);
    // (codepoint, left, right, advance, why this row discriminates)
    let cases: &[(u32, i32, i32, f32, &str)] = &[
        // 32 digits => 8-bit rows stored `byte << 24`. Vanilla's own line for ✓:
        // bytes 00 00 00 00 01 01 02 02 44 44 28 28 10 10 00 00, so
        // mask = (0x01|0x02|0x44|0x28|0x10) << 24 = 0x7F000000.
        // leading_zeros 1 => left 1; trailing_zeros 24 => right = 32-24-1 = 7.
        // width 7, advance 7/2 + 1 = 4.5.
        (
            0x2713,
            1,
            7,
            4.5,
            "half-width: a fixed 16-wide stride reads 4 digits per row and mis-masks",
        ),
        // 64 digits => 16-bit rows stored `short << 16`. 中's ink spans columns
        // 2..12 (mask 0x3FF80000), so the DERIVED advance would be 11/2 + 1 =
        // 6.5. The 4E00..9FFF override forces (0, 15) and 16/2 + 1 = 9.0 — the
        // two readings differ by 2.5 px, which is what makes this a test of
        // `size_overrides` and not of arithmetic.
        (
            0x4E2D,
            0,
            15,
            9.0,
            "full-width + override; ignoring size_overrides gives 6.5",
        ),
        // 96 digits => 24-bit rows stored `v << 8`. Row 5 is 0x018000, so
        // mask = 0x01800000: leading_zeros 7 => left 7, trailing_zeros 23 =>
        // right = 32-23-1 = 8. width 2, advance 2/2 + 1 = 2.0.
        (0x1234, 7, 8, 2.0, "the read24 arm"),
        // 128 digits => 32-bit rows stored as-is. Row 0 is 0x80000001, so the
        // mask has bit 31 and bit 0: left 0, right 31, width 32,
        // advance 32/2 + 1 = 17.0.
        (0x1235, 0, 31, 17.0, "the read32 arm"),
        // mask == 0 takes `left = 0, right = bitWidth` — NOT `bitWidth - 1`. A
        // blank 8-wide glyph is therefore 9 columns and advances 9/2 + 1 = 5.5;
        // the off-by-one reading gives 5.0.
        (
            0x2000,
            0,
            8,
            5.5,
            "all-empty: right = bitWidth, so 5.5 and not 5.0",
        ),
        // The override-boundary pair. Both bitmaps are byte-identical (row 0 =
        // 0x0400 => mask 0x04000000 => left 5, right 5, width 1, advance 1.5),
        // so the only difference between these two rows is whether the
        // 3001..30FF range contains the codepoint.
        (0x30FF, 0, 15, 9.0, "last codepoint INSIDE 3001..30FF"),
        (
            0x3100,
            5,
            5,
            1.5,
            "one PAST 30FF: a range applied one codepoint too wide reads 9.0 here",
        ),
        // Two identical ranges, different bounds. Vanilla `remove`s the
        // codepoint as it applies the first, so the first declaration wins:
        // right 3 => width 4 => advance 3.0. Last-wins would give 5.0.
        (
            0x5005,
            0,
            3,
            3.0,
            "overlapping ranges: first declared wins, so 3.0 and not 5.0",
        ),
        // Inside no range, to prove the derived path is still live for a 16-wide
        // glyph rather than everything landing in an override.
        (0x2500, 5, 5, 1.5, "16-wide, inside no range, so derived"),
    ];

    let mut bad: Vec<String> = Vec::new();
    for &(cp, left, right, advance, why) in cases {
        match font.unihex_glyph(cp) {
            None => bad.push(format!(
                "U+{cp:04X}: expected a unihex glyph ({why}), got {:?}",
                font.glyph(cp)
            )),
            Some(g) => {
                if g.left != left || g.right != right {
                    bad.push(format!(
                        "U+{cp:04X}: bounds ({}, {}), want ({left}, {right}) — {why}",
                        g.left, g.right
                    ));
                }
                let got = g.advance();
                if (got - advance).abs() > 1e-6 {
                    bad.push(format!("U+{cp:04X}: advance {got}, want {advance} — {why}"));
                }
                if g.width() != right - left + 1 {
                    bad.push(format!(
                        "U+{cp:04X}: width {} disagrees with its own bounds",
                        g.width()
                    ));
                }
            }
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// The wrong hypothesis, evaluated: 中's own ink spans columns 2..12, so a loader
/// that dropped `size_overrides` would measure 6.5 rather than 9.0.
///
/// Asserting this is what proves the override row above is not passing by
/// coincidence — an input where the two readings agreed would make it vacuous.
#[test]
fn the_override_and_derived_readings_of_a_cjk_glyph_really_do_differ() {
    let font = load(true);
    let g = font.unihex_glyph(0x4E2D).expect("中 is a unihex glyph");
    assert_eq!(
        g.bitmap.derived_bounds(),
        (2, 12),
        "中's ink spans columns 2..12; (0, 15) here would mean the fixture lost its ink"
    );
    let derived_advance = f32::from(12i16 - 2 + 1) / 2.0 + 1.0;
    assert!(
        (derived_advance - 6.5).abs() < 1e-6,
        "the wrong hypothesis is 6.5, got {derived_advance}"
    );
    assert!(
        (g.advance() - 9.0).abs() < 1e-6,
        "and the override hypothesis is 9.0, got {}",
        g.advance()
    );
}

/// A higher-priority provider keeps a codepoint the unihex file also supplies —
/// and **coverage, not ink, is what a provider contributes**.
///
/// `A` and `B` are both in the `.hex` payload and both declared by the bitmap
/// sheet; `B`'s cell is blank. Vanilla still counts that blank cell as the sheet
/// supplying `B` (`BitmapProvider` maps every non-null `chars` slot), so `B`
/// advances 1, not the 5.0 its unihex entry would give. That is the trap in the
/// priority rule and the reason this fixture draws one cell and not the other.
#[test]
fn a_bitmap_provider_declared_first_beats_the_unihex_file() {
    let font = load(true);
    let mut bad: Vec<String> = Vec::new();
    for (cp, want, why) in [
        (0x0041u32, 6.0f32, "inked sheet cell, not unihex's 5.0"),
        (0x0042, 1.0, "BLANK sheet cell still wins, so 1.0 and not 5.0"),
        (0x2713, 4.5, "unihex-only, so the unihex advance"),
    ] {
        // `unwrap_or(f32::NAN)` was here and made this test vacuous: every
        // comparison against NaN is false, so an *absent* glyph passed. Caught by
        // neutering the loader and watching this arm stay green. Compare the
        // `Option` itself.
        match font.advance(cp) {
            None => bad.push(format!("U+{cp:04X}: uncovered, want {want} ({why})")),
            Some(got) if (got - want).abs() > 1e-6 => {
                bad.push(format!("U+{cp:04X}: advance {got}, want {want} ({why})"));
            }
            Some(_) => {}
        }
    }
    for cp in [0x0041u32, 0x0042] {
        if font.bitmap_glyph(cp).is_none() {
            bad.push(format!("U+{cp:04X} must stay a sheet glyph"));
        }
        if font.unihex_glyph(cp).is_some() {
            bad.push(format!("U+{cp:04X} must not be won by unihex"));
        }
    }
    // And the `space` provider, declared before both, keeps U+0020 and U+200C.
    for (cp, want) in [(0x0020u32, 4.0f32), (0x200C, 0.0)] {
        if font.advance(cp) != Some(want) {
            bad.push(format!(
                "U+{cp:04X}: space provider must win with {want}, got {:?}",
                font.advance(cp)
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// The control: with the `unihex` provider removed and **nothing else changed**,
/// every codepoint only it supplied must be absent.
///
/// This is what makes the positive arms attributable to the unihex provider
/// rather than to anything else in the fixture.
#[test]
fn without_the_unihex_provider_those_codepoints_are_uncovered() {
    let with = load(true);
    let without = load(false);

    assert!(without.contains(0x0041), "the sheet must still supply A");
    assert_eq!(without.advance(0x0020), Some(4.0), "space is unaffected");

    let mut wrong: Vec<String> = Vec::new();
    for cp in [
        0x2713u32, 0x4E2D, 0x1234, 0x1235, 0x2000, 0x30FF, 0x3100, 0x5005, 0x2500,
    ] {
        if without.contains(cp) {
            wrong.push(format!("U+{cp:04X} present with unihex OFF"));
        }
        if !with.contains(cp) {
            wrong.push(format!("U+{cp:04X} absent with unihex ON"));
        }
    }
    assert!(wrong.is_empty(), "{}", wrong.join(", "));
    assert_eq!(without.unihex_count(), 0);
    assert_eq!(
        with.unihex_count(),
        UNIHEX_WON,
        "{UNIHEX_WON} of the {HEX_ENTRIES} hex entries win; A and B go to the sheet"
    );
    // The whole point, stated as a count: the unihex provider is where the extra
    // coverage comes from.
    assert_eq!(
        with.codepoint_count() - without.codepoint_count(),
        UNIHEX_WON
    );
}

/// A `size_overrides` range naming codepoints the `.hex` file does not contain
/// contributes nothing (`UnihexProvider.Definition.loadData`'s
/// `if (codepointBits != null)`) rather than inventing blank glyphs across the
/// range. The fixture declares E000..E010 for exactly this.
#[test]
fn an_override_range_does_not_invent_glyphs_the_hex_file_lacks() {
    let font = load(true);
    let present: Vec<String> = (0xE000u32..=0xE010)
        .filter(|cp| font.contains(*cp))
        .map(|cp| format!("U+{cp:04X}"))
        .collect();
    assert!(
        present.is_empty(),
        "E000..E010 is an override range with no hex data; got {}",
        present.join(", ")
    );
    // Control: a range whose codepoints *are* in the payload does produce
    // glyphs, so the emptiness above is the missing data and not the override
    // machinery being inert. Without this the assertion passes even when no
    // unihex glyph loads at all.
    assert!(
        font.unihex_glyph(0x30FF).is_some(),
        "3001..30FF is an override range WITH hex data and must produce a glyph"
    );
}

/// A bound a `size_overrides` range pushes outside the source row's width pads
/// with blank columns, not with a neighbour's ink
/// (`unpackBitsToBytes`'s `i < 32 && i >= 0`).
///
/// U+30FF has ink in exactly one column and is forced to 16. Asserting the exact
/// set of lit texels rather than "some ink exists" is what separates a correct
/// pad from one that smears the row.
#[test]
fn an_override_wider_than_the_ink_pads_with_blank_columns() {
    let font = load(true);
    let g = font.unihex_glyph(0x30FF).expect("30FF is unihex");
    assert_eq!((g.left, g.right, g.width()), (0, 15, 16));
    let lit: Vec<(u32, u32)> = (0..16)
        .flat_map(|ty| (0..16).map(move |tx| (tx, ty)))
        .filter(|&(tx, ty)| g.is_ink(tx, ty))
        .collect();
    // Row 0 is 0x0400 => bit 26 of `0x0400 << 16`, i.e. column 5 with left = 0.
    assert_eq!(
        lit,
        vec![(5, 0)],
        "exactly one lit texel, at column 5 of row 0"
    );
}

/// The ink of a real vanilla glyph, read off its HEX line by hand.
///
/// `2713`'s bytes are `00 00 00 00 01 01 02 02 44 44 28 28 10 10 00 00` — 14 bits
/// set in total. With `left = 1`, texel column `tx` is bit `32 - 1 - 1 - tx =
/// 30 - tx`, and a byte occupies bits 31..24, so row 8 (`0x44` = bits 30 and 26)
/// is ink at `tx = 0` and `tx = 4` and nowhere else.
#[test]
fn a_real_unifont_glyph_lights_the_texels_its_hex_line_names() {
    let font = load(true);
    let g = font.unihex_glyph(0x2713).expect("✓ is unihex-only here");
    assert_eq!((g.left, g.right), (1, 7));
    let lit: Vec<(u32, u32)> = (0..16u32)
        .flat_map(|ty| (0..g.width() as u32).map(move |tx| (tx, ty)))
        .filter(|&(tx, ty)| g.is_ink(tx, ty))
        .collect();
    assert_eq!(lit.len(), 14, "14 bits are set across the line: {lit:?}");
    let row8: Vec<u32> = lit
        .iter()
        .filter(|(_, ty)| *ty == 8)
        .map(|(tx, _)| *tx)
        .collect();
    assert_eq!(row8, vec![0, 4], "row 8 is 0x44 => columns 0 and 4");
    assert!(
        !lit.iter().any(|(_, ty)| *ty < 4),
        "rows 0..3 are 0x00: {lit:?}"
    );
}

/// The three rejections `UnihexProvider.readFromStream` and `decodeHex` make.
///
/// Each malformed input is paired with the well-formed line it was derived from,
/// which is the control: if the accepting case did not parse, an `Err` here would
/// prove nothing about *why*.
#[test]
fn a_malformed_hex_entry_is_an_error_not_a_dropped_glyph() {
    let good = "2713:00000000010102024444282810100000\n";
    assert_eq!(
        read_hex_entries(good.as_bytes(), |_, _| {}).expect("control parses"),
        1
    );

    let cases: &[(&str, &str)] = &[
        ("271:00000000010102024444282810100000\n", "3-digit codepoint"),
        (
            "2713000:00000000010102024444282810100000\n",
            "7-digit codepoint",
        ),
        ("271300000000010102024444282810100000\n", "no colon"),
        ("2713:000000000101020244442828101000\n", "30 data digits"),
        (
            "2713:0000000001010202444428281010000000\n",
            "34 data digits",
        ),
        (
            "2713:0000000001010202444428281010000G\n",
            "non-hex digit in the bitmap",
        ),
        (
            "27G3:00000000010102024444282810100000\n",
            "non-hex codepoint",
        ),
    ];
    let accepted: Vec<&str> = cases
        .iter()
        .filter(|(body, _)| read_hex_entries(body.as_bytes(), |_, _| {}).is_ok())
        .map(|(_, why)| *why)
        .collect();
    assert!(
        accepted.is_empty(),
        "these must be rejected: {}",
        accepted.join(", ")
    );
}

/// A `unihex` provider whose `hex_file` no pack supplies is a soft skip.
///
/// Deliberate and load-bearing: vanilla's `font/unifont.zip` lives in the
/// launcher's asset-object store, not in `client.jar`, so a store-less install
/// resolves nothing here. Making it fatal would take that install from "CJK draws
/// the missing-glyph box, as it always did" to "the font fails to load and every
/// glyph in the game changes".
#[test]
fn a_missing_hex_file_leaves_the_rest_of_the_font_intact() {
    let manager = fixture(true, false);
    let id: ResourceLocation = "minecraft:default".parse().expect("valid id");
    let font = FontLoader::new(&manager)
        .load(&id, &FontOptions::none())
        .expect("a missing hex_file must not fail the load");
    assert_eq!(font.unihex_count(), 0);
    assert_eq!(font.advance(0x0041), Some(6.0), "the sheet still works");
    assert!(!font.contains(0x4E2D), "and unihex contributed nothing");

    // Control: the same definition *with* the zip present does cover it, so the
    // absence above is the missing file and not the provider being ignored.
    assert!(load(true).contains(0x4E2D));
}

/// A unihex glyph's bold offset is **0.5**, not 1
/// (`UnihexProvider.Glyph.info`'s `getBoldOffset`), because it draws at
/// oversample 2. A sheet glyph keeps 1.
///
/// The discriminating quantity is the *difference* between the bold and plain
/// advances: asserting only that bold is wider passes under either reading.
#[test]
fn bold_widens_a_unihex_glyph_by_half_a_pixel_and_a_sheet_glyph_by_one() {
    let font = load(true);
    let mut bad: Vec<String> = Vec::new();
    for (cp, want) in [(0x0041u32, 1.0f32), (0x4E2D, 0.5)] {
        let plain = font.advance_bold(cp, false).expect("covered");
        let bold = font.advance_bold(cp, true).expect("covered");
        let delta = bold - plain;
        if (delta - want).abs() > 1e-6 {
            bad.push(format!("U+{cp:04X}: bold delta {delta}, want {want}"));
        }
        if (font.bold_offset(cp) - want).abs() > 1e-6 {
            bad.push(format!(
                "U+{cp:04X}: bold_offset {}, want {want}",
                font.bold_offset(cp)
            ));
        }
        if (font.shadow_offset(cp) - want).abs() > 1e-6 {
            bad.push(format!(
                "U+{cp:04X}: shadow_offset {}, want {want}",
                font.shadow_offset(cp)
            ));
        }
    }
    // `legacy_width` must use the per-glyph offset too, or bold CJK measures
    // 0.5 px per glyph too wide. `§l` then one 中: 9.0 + 0.5 = 9.5.
    let bold_cjk = font.legacy_width("\u{00a7}l\u{4e2d}");
    assert!(
        (bold_cjk - 9.5).abs() < 1e-6,
        "bold 中 is 9.5, not {bold_cjk} (10.0 means a font-wide +1)"
    );
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// `Glyph::Unihex` reaches the drawable-raster API with the geometry a renderer
/// needs, and specifically with `texel_size` 0.5 rather than 1.0.
///
/// That is the number a renderer written for the 8×8 sheets gets wrong: at 1.0
/// every CJK glyph draws 16 logical pixels tall — double height — while still
/// satisfying every advance-only assertion.
#[test]
fn a_unihex_glyph_rasters_at_half_a_logical_pixel_per_texel() {
    let manager = fixture(true, true);
    let id: ResourceLocation = "minecraft:default".parse().expect("valid id");
    let raster = FontLoader::new(&manager)
        .load_raster(&id, &FontOptions::none())
        .expect("the fixture font rasters");

    let cjk = raster.raster(0x4E2D).expect("中 has a drawable raster");
    assert_eq!(cjk.cell_width(), 16, "16 trimmed columns");
    assert_eq!(cjk.cell_height(), 16, "always 16 rows");
    assert!(
        (cjk.texel_size() - 0.5).abs() < 1e-6,
        "oversample 2 => 0.5 logical px per texel, got {}",
        cjk.texel_size()
    );
    assert!(
        (cjk.top() - 0.0).abs() < 1e-6,
        "bearingTop 7 against BEARING_TOP_BASE 7 => top 0, got {}",
        cjk.top()
    );
    assert!((cjk.advance() - 9.0).abs() < 1e-6);
    // 16 rows at 0.5 logical px = 8 logical px tall, the ascii sheet's height.
    let drawn_height = cjk.cell_height() as f32 * cjk.texel_size();
    assert!((drawn_height - 8.0).abs() < 1e-6, "got {drawn_height}");
    // Half-width: 7 columns at 0.5 = 3.5 logical px of ink, advance 4.5. A fixed
    // 16-column stride would report 8.0 here.
    let half = raster.raster(0x2713).expect("✓ has a drawable raster");
    assert_eq!(half.cell_width(), 7);
    let half_ink_width = half.cell_width() as f32 * half.texel_size();
    assert!((half_ink_width - 3.5).abs() < 1e-6, "got {half_ink_width}");

    // Control: the sheet glyph beside it is 1.0, so the 0.5 above is a property
    // of the unihex arm and not of this API.
    let sheet = raster.raster(0x0041).expect("A has a drawable raster");
    assert!((sheet.texel_size() - 1.0).abs() < 1e-6);
    assert_eq!(sheet.cell_height(), 8);

    // And a space is still not drawable, which is what `RasterFont::raster`'s
    // `None` means to the HUD.
    assert!(raster.raster(0x0020).is_none());
    assert_eq!(raster.unihex_count(), UNIHEX_WON);
}

/// Every glyph kind reports an advance, and the census says how many of each the
/// fixture produced — so a fifth provider kind has one obvious place that
/// names what changed.
#[test]
fn the_glyph_census_accounts_for_every_covered_codepoint() {
    let font = load(true);
    let mut kinds = (0usize, 0usize, 0usize, 0usize);
    for cp in font.codepoints().collect::<Vec<_>>() {
        match font.glyph(cp).expect("codepoints() only yields covered ones") {
            Glyph::Bitmap(_) => kinds.0 += 1,
            Glyph::Unihex(_) => kinds.1 += 1,
            Glyph::Space { .. } => kinds.2 += 1,
            Glyph::Ttf(_) => kinds.3 += 1,
        }
    }
    assert_eq!(
        kinds,
        (2, UNIHEX_WON, 2, 0),
        "2 sheet cells, {UNIHEX_WON} unihex, 2 space advances, 0 ttf (this fixture declares none)"
    );
    assert_eq!(
        kinds.0 + kinds.1 + kinds.2 + kinds.3,
        font.codepoint_count()
    );
}
