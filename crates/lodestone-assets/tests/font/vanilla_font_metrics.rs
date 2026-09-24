//! Metrics gate: the resolved `minecraft:default` font must reproduce vanilla's
//! **per-glyph proportional advances**, and its rasterised ink must agree with
//! the advance it derived.
//!
//! # Why an advance table and not a round-trip
//!
//! `decode(encode(x)) == x` is satisfied by two symmetric misunderstandings, and
//! so is "the advance equals what our own `actual_glyph_width` returned". The
//! expected values below are therefore **hand-authored from outside this crate**:
//! they are the published widths of Minecraft's default font, the ones every
//! chat-wrapping and GUI-centring implementation has used for a decade — `i` is
//! 2 px, `l` is 3, `I` and `t` are 4, `f` and `k` are 5, the great majority are
//! 6, and the space character's 4 comes from a `space` provider rather than the
//! sheet (its ascii cell is blank). If our loader disagrees with this table, the
//! loader is wrong; the table was not derived from it.
//!
//! # What this cannot see
//!
//! Nothing here proves a pixel changed. A font with perfect metrics that nothing
//! draws with is the exact island this repo keeps producing — the on-screen half
//! is `lodestone-shell`'s `tests/vanilla_font_pixels.rs`, which measures the same
//! advances as distances between lit columns.
//!
//! ```text
//! cargo test -p lodestone-assets --test vanilla_font_metrics -- --ignored --nocapture
//! ```

use std::path::PathBuf;

use lodestone_assets::font::{FontLoader, FontOptions, metrics};
use lodestone_assets::{ResourceLocation, ResourceManager, ResourceSource, ZipSource};

/// Vanilla's default-font advances, hand-authored (see the module docs). Every
/// entry not listed is 6, which is asserted separately.
const VANILLA_ADVANCES: &[(char, f32)] = &[
    // The `space` provider, not the ascii sheet.
    (' ', 4.0),
    // The narrow set.
    ('!', 2.0),
    ('.', 2.0),
    (',', 2.0),
    (':', 2.0),
    (';', 2.0),
    ('i', 2.0),
    ('|', 2.0),
    ('\'', 2.0),
    ('l', 3.0),
    ('`', 3.0),
    ('I', 4.0),
    ('t', 4.0),
    ('[', 4.0),
    (']', 4.0),
    ('(', 4.0),
    (')', 4.0),
    ('"', 4.0),
    ('{', 4.0),
    ('}', 4.0),
    ('*', 4.0),
    ('f', 5.0),
    ('k', 5.0),
    ('<', 5.0),
    ('>', 5.0),
    // The wide outlier.
    ('~', 7.0),
    // Representative full-width glyphs, so the table is not all exceptions.
    ('W', 6.0),
    ('M', 6.0),
    ('m', 6.0),
    ('A', 6.0),
    ('a', 6.0),
    ('0', 6.0),
];

/// Characters whose advance is the common 6.
const FULL_WIDTH: &str = "ABCDEFGHJKLNOPQRSTUVXYZbcdeghjnopqrsuvwxyz123456789";

fn client_jar() -> Option<PathBuf> {
    let mut cwd = std::env::current_dir().ok()?;
    loop {
        let cache = cwd.join(".cache/mc");
        if cache.is_dir() {
            let mut roots: Vec<PathBuf> = std::fs::read_dir(&cache)
                .ok()?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.join("client.jar").is_file())
                .collect();
            roots.sort();
            if let Some(root) = roots.pop() {
                return Some(root.join("client.jar"));
            }
        }
        if !cwd.pop() {
            return None;
        }
    }
}

fn manager() -> ResourceManager {
    let jar = client_jar().expect(
        "no client.jar under .cache/mc/<version>/; this gate is opted in via --ignored and \
         fails closed — fetch the assets, do not treat a missing jar as a pass",
    );
    let zip = ZipSource::open(&jar).expect("open client.jar");
    ResourceManager::new(vec![Box::new(zip) as Box<dyn ResourceSource>])
}

#[test]
#[ignore = "requires a fetched vanilla client.jar in .cache/mc/<version>/"]
fn default_font_advances_are_vanillas_proportional_widths() {
    let manager = manager();
    let id: ResourceLocation = "minecraft:default".parse().expect("valid id");
    let raster = FontLoader::new(&manager)
        .load_raster(&id, &FontOptions::none())
        .expect("the vanilla default font must resolve from client.jar");
    let font = raster.font();

    eprintln!("=== vanilla default font metrics ===");
    eprintln!("codepoints  = {}", font.codepoint_count());
    eprintln!("sheets      = {}", raster.sheet_count());

    assert!(
        raster.sheet_count() >= 3,
        "the default font's bitmap providers are ascii, accented and \
         nonlatin_european; got {} decoded sheets",
        raster.sheet_count()
    );

    let mut wrong = Vec::new();
    for &(ch, want) in VANILLA_ADVANCES {
        let got = font.advance(ch as u32);
        if got != Some(want) {
            wrong.push(format!("{ch:?}: want {want}, got {got:?}"));
        }
    }
    for ch in FULL_WIDTH.chars() {
        let got = font.advance(ch as u32);
        if got != Some(6.0) {
            wrong.push(format!("{ch:?}: want 6, got {got:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "advances disagree with vanilla's published widths:\n  {}",
        wrong.join("\n  ")
    );

    // The load-bearing property, stated as the thing a fixed-advance font cannot
    // satisfy: these three must all differ.
    let (i, l, w) = (
        font.advance('i' as u32).unwrap(),
        font.advance('l' as u32).unwrap(),
        font.advance('W' as u32).unwrap(),
    );
    eprintln!("i / l / W   = {i} / {l} / {w}");
    assert!(
        i < l && l < w,
        "the font must be proportional: i={i} l={l} W={w}. A fixed-advance font \
         makes all three equal and still passes every assertion on the source string"
    );

    // A whole string, so the aggregate is pinned too. "Lodestone" is
    // 6+6+6+6+6+4+6+6+6 = 52 px proportional (only `t` is narrow) against 54 at
    // a flat 6; "lodestone" swaps the leading `L` for a 3 px `l` and lands at 49.
    for (word, want) in [("Lodestone", 52.0), ("lodestone", 49.0)] {
        let prop = font.string_width(word);
        let flat = word.chars().count() as f32 * 6.0;
        eprintln!("width({word:?}) = {prop} proportional vs {flat} at a flat 6 px");
        assert_eq!(prop, want, "vanilla's width for {word:?}");
        assert!(prop < flat);
    }
}

#[test]
#[ignore = "requires a fetched vanilla client.jar in .cache/mc/<version>/"]
fn glyph_ink_agrees_with_the_advance_it_produced() {
    let manager = manager();
    let id: ResourceLocation = "minecraft:default".parse().expect("valid id");
    let raster = FontLoader::new(&manager)
        .load_raster(&id, &FontOptions::none())
        .expect("the vanilla default font must resolve from client.jar");

    // For a 1:1 sheet (ascii is 8x8 cells at height 8) vanilla's advance is
    // `rightmost ink column + 1 + 1`. Measuring it back off the *pixels* is what
    // proves `RasterFont` handed out the same cell the metrics were taken from —
    // a coverage table pointing one cell to the left would still produce a
    // plausible proportional font.
    let mut checked = 0usize;
    for ch in "iltIWMm.aAz0".chars() {
        let r = raster
            .raster(ch as u32)
            .unwrap_or_else(|| panic!("{ch:?} must have drawable ink"));
        assert_eq!(r.texel_size(), 1.0, "{ch:?} is expected off the 1:1 ascii sheet");
        let mut rightmost: i32 = -1;
        for ty in 0..r.cell_height() {
            for tx in 0..r.cell_width() {
                if r.is_ink(tx, ty) {
                    rightmost = rightmost.max(tx as i32);
                }
            }
        }
        assert!(rightmost >= 0, "{ch:?} must have at least one ink texel");
        assert_eq!(
            r.advance(),
            rightmost as f32 + 2.0,
            "{ch:?}: advance {} does not match its own ink (rightmost column {rightmost})",
            r.advance()
        );
        // Nothing may sit beyond the advance: that is what would overlap the
        // next glyph.
        for ty in 0..r.cell_height() {
            for tx in (r.advance() as u32 - 1)..r.cell_width() {
                assert!(
                    !r.is_ink(tx, ty),
                    "{ch:?} has ink at column {tx}, past its {} px advance",
                    r.advance()
                );
            }
        }
        checked += 1;
    }
    eprintln!("ink/advance agreement checked for {checked} glyphs");

    // Placement: the ascii sheet declares `ascent: 7`, so its cell sits flush
    // with the line's top. Getting this backwards drops every line by 7 px.
    let a = raster.raster('A' as u32).expect("A has ink");
    eprintln!(
        "A: cell {}x{}, top {} (7 - ascent), advance {}",
        a.cell_width(),
        a.cell_height(),
        a.top(),
        a.advance()
    );
    assert_eq!(a.top(), 0.0, "ascii ascent 7 puts the cell top at 7 - 7 = 0");
    assert_eq!(metrics::BEARING_TOP_BASE, 7.0);

    // The accented sheet is height 12 / ascent 10, so its glyphs hang above the
    // line — the control proving `top()` is read from the provider and not a
    // constant zero.
    let acc = raster
        .raster('\u{00c1}' as u32)
        .expect("A-acute comes off the accented sheet");
    eprintln!(
        "A-acute: cell {}x{}, texel {}, top {}",
        acc.cell_width(),
        acc.cell_height(),
        acc.texel_size(),
        acc.top()
    );
    assert!(
        acc.top() < 0.0,
        "an ascent-10 sheet must hang above the line; got top {}",
        acc.top()
    );
}
