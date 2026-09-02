//! The `unihex` provider against vanilla's **real** `unifont.zip`.
//!
//! `#[ignore]`d and **fail-closed**: a missing fixture panics with the fetch
//! command rather than skipping, because a silent pass here repairs nothing.
//!
//! ```text
//! cargo test -p lodestone-assets --test unihex_vanilla_oracle -- --ignored --nocapture
//! ```
//!
//! # Where the fixture lives, and the trap in it
//!
//! `font/unifont.zip` is **not in `client.jar`** — it is an asset-object-store
//! object, and so is the only `font/include/unifont.json` that declares a unihex
//! provider at all. The jar ships a 29-byte stub of that file whose `providers`
//! array is **empty**. So a `ResourceManager` built from the jar alone loads a
//! font with 2,414 codepoints and no error, which is exactly the state that made
//! every non-Latin character draw the missing-glyph box. The store source is
//! stacked **above** the jar here for that reason, and
//! [`without_the_object_store_the_jar_stub_wins`] is the control that proves the
//! stacking is what did it.
//!
//! # Every expected number below came out of the files, not out of our code
//!
//! `unifont_all_no_pua-17.0.01.hex` holds 114,432 entries and is a **superset**
//! of the 2,414 codepoints the three bitmap sheets plus the `space` provider
//! supply, so the resolved font must cover exactly 114,432 codepoints of which
//! 112,018 come from unihex. The per-codepoint advances are hand-derived from the
//! glyph's own HEX line by the rule in `UnihexProvider`, and each case names what
//! the *wrong* reading would have produced.

use lodestone_assets::font::{FontLoader, FontOptions};
use lodestone_assets::{MemorySource, ResourceLocation, ResourceManager, ZipSource};
use std::path::PathBuf;

/// Entries in vanilla 26.2's `unifont.zip` (`wc -l` of its single `.hex`
/// member), and therefore the codepoint total of the resolved `minecraft:default`
/// font, since the file covers every bitmap-sheet codepoint too.
const UNIFONT_ENTRIES: usize = 114_432;
/// Codepoints the three bitmap sheets plus the `space` provider supply — the
/// total this font reported before unihex rasterised.
const BITMAP_CODEPOINTS: usize = 2_414;
/// Codepoints the unihex provider *wins*: everything in the file that no
/// higher-priority provider already claimed.
const UNIHEX_WON: usize = UNIFONT_ENTRIES - BITMAP_CODEPOINTS;

fn cache_root() -> Option<PathBuf> {
    Some(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()?
            .parent()?
            .join(".cache/mc/26.2"),
    )
}

/// Reads one asset-object-store object by its logical index name.
///
/// The name carries **no** `assets/` prefix; getting that wrong resolves nothing
/// and is the mistake that makes a store look empty.
fn object(name: &str) -> Option<Vec<u8>> {
    let dir = cache_root()?;
    let index = std::fs::read_dir(&dir).ok()?.flatten().find_map(|e| {
        let p = e.path();
        let file = p.file_name()?.to_str()?.to_string();
        (file.starts_with("asset-index") && file.ends_with(".json")).then_some(p)
    })?;
    let json: serde_json::Value = serde_json::from_slice(&std::fs::read(&index).ok()?).ok()?;
    let meta = json.get("objects")?.get(name)?;
    let hash = meta.get("hash")?.as_str()?;
    let want = meta.get("size")?.as_u64()?;
    let bytes = std::fs::read(dir.join("objects").join(&hash[0..2]).join(hash)).ok()?;
    // Length against the index, so a truncated object reads as absent rather
    // than reaching the parser as if it were the asset.
    (bytes.len() as u64 == want).then_some(bytes)
}

const FETCH_HINT: &str = "\
This gate needs two asset-object-store objects that are NOT in client.jar:\n\
  minecraft/font/include/unifont.json  (3993 B — the jar's copy is a 29 B stub \
with an EMPTY providers array)\n\
  minecraft/font/unifont.zip           (1559654 B — GNU Unifont HEX data)\n\
Fetch them with:  cargo run -p xtask -- fetch-assets --version 26.2\n\
An #[ignore]d test that was explicitly asked to run must FAIL on a missing \
fixture, never skip.";

/// The jar alone, which is what the shell used to build.
fn jar_only() -> ResourceManager {
    let jar = cache_root()
        .map(|r| r.join("client.jar"))
        .filter(|p| p.is_file())
        .unwrap_or_else(|| panic!("no .cache/mc/26.2/client.jar\n{FETCH_HINT}"));
    ResourceManager::new(vec![Box::new(ZipSource::open(&jar).expect("open the jar"))])
}

/// The jar with the asset-object store stacked above it, which is what the shell
/// builds now.
fn jar_plus_store() -> ResourceManager {
    let mut manager = jar_only();
    let mut store = MemorySource::new("asset-objects");
    for name in [
        "minecraft/font/include/unifont.json",
        "minecraft/font/unifont.zip",
    ] {
        let bytes = object(name).unwrap_or_else(|| panic!("missing object {name}\n{FETCH_HINT}"));
        store.insert(&format!("assets/{name}"), bytes);
    }
    manager.push(Box::new(store));
    manager
}

fn default_font(manager: &ResourceManager) -> lodestone_assets::font::Font {
    let id: ResourceLocation = "minecraft:default".parse().expect("valid id");
    FontLoader::new(manager)
        .load(&id, &FontOptions::none())
        .expect("minecraft:default loads")
}

/// The whole point, as a count: unihex takes the font from 2,414 codepoints to
/// 114,432.
#[test]
#[ignore = "requires client.jar plus the unifont.json/unifont.zip asset objects"]
fn the_real_unifont_zip_covers_every_codepoint_it_declares() {
    let font = default_font(&jar_plus_store());
    assert_eq!(
        font.codepoint_count(),
        UNIFONT_ENTRIES,
        "unifont.zip's {UNIFONT_ENTRIES} entries are a superset of the {BITMAP_CODEPOINTS} \
         bitmap-sheet codepoints, so the union is the file's own count"
    );
    assert_eq!(
        font.unihex_count(),
        UNIHEX_WON,
        "{BITMAP_CODEPOINTS} of them are claimed first by the sheets and the space provider"
    );
}

/// The control, run in the same process: with the object store removed and
/// nothing else changed, the jar's empty `unifont.json` stub wins and every
/// unihex codepoint is gone.
///
/// This is the arm that had to be observed failing before the fix, and it is what
/// makes the counts above attributable to the store push rather than to anything
/// else.
#[test]
#[ignore = "requires client.jar plus the unifont.json/unifont.zip asset objects"]
fn without_the_object_store_the_jar_stub_wins() {
    let jar = default_font(&jar_only());
    assert_eq!(
        jar.unihex_count(),
        0,
        "the jar's font/include/unifont.json has an empty providers array"
    );
    assert_eq!(
        jar.codepoint_count(),
        BITMAP_CODEPOINTS,
        "bitmap sheets plus the space provider, and nothing else"
    );
    let mut leaked: Vec<String> = Vec::new();
    for cp in [0x2713u32, 0x4E2D, 0x3042, 0xAC00, 0x0E01, 0x0627] {
        if jar.contains(cp) {
            leaked.push(format!("U+{cp:04X}"));
        }
    }
    assert!(
        leaked.is_empty(),
        "these must be uncovered without the store: {}",
        leaked.join(", ")
    );

    // And with the store they are all covered, which is the same detector
    // reporting the other answer.
    let both = default_font(&jar_plus_store());
    let missing: Vec<String> = [0x2713u32, 0x4E2D, 0x3042, 0xAC00, 0x0E01, 0x0627]
        .iter()
        .filter(|cp| !both.contains(**cp))
        .map(|cp| format!("U+{cp:04X}"))
        .collect();
    assert!(missing.is_empty(), "still uncovered: {}", missing.join(", "));
}

/// Exact advances for codepoints chosen so that each one separates a correct
/// implementation from a specific wrong one.
///
/// Mismatches are collected rather than asserted in the loop, so one wrong row
/// cannot hide the other nine.
#[test]
#[ignore = "requires client.jar plus the unifont.json/unifont.zip asset objects"]
fn real_glyph_advances_match_the_hand_derived_hex_lines() {
    let font = default_font(&jar_plus_store());
    // (codepoint, advance, is_unihex, why this row discriminates)
    let cases: &[(u32, f32, bool, &str)] = &[
        // U+2713 ✓  HEX 00000000010102024444282810100000 (32 digits => 8 wide).
        // mask 0x7F000000 => left 1, right 7, width 7, advance 4.5. NOT in any
        // bitmap sheet and in no size_overrides range: the plain half-width
        // derived case, and a codepoint that drew a square before this work.
        (0x2713, 4.5, true, "half-width, derived, unihex-only"),
        // U+2714 ✔  IS in nonlatin_european.png, ink to column 6, so the sheet
        // advance is (0.5 + 6) as i32 + 1 = 7. Its unihex line derives left 0,
        // right 14 => 8.5. The two providers disagree, so this row is a real
        // priority test: 8.5 here would mean unihex is overriding the sheets.
        (0x2714, 7.0, false, "in BOTH; the bitmap sheet must win, not 8.5"),
        // U+4E2D 中  ink spans columns 2..12 => derived 6.5, but the
        // 3200..9FFF override forces (0, 15) => 9.0.
        (0x4E2D, 9.0, true, "CJK override; 6.5 means size_overrides was ignored"),
        // U+FF5E ～  the LAST codepoint of the FF01..FF5E override range. Its own
        // ink derives (1, 13) => 7.5; the override gives 9.0.
        (0xFF5E, 9.0, true, "last codepoint INSIDE FF01..FF5E"),
        // U+FF5F ｟  ONE PAST that range. Derived (4, 12) => 5.5. A range applied
        // one codepoint too wide reads 9.0 here — invisible without this row.
        (0xFF5F, 5.5, true, "one PAST FF5E: a too-wide range reads 9.0"),
        // U+AC00 가  Hangul Syllables override is left 1, right 15 => width 15 =>
        // 8.5, where its own ink derives (2, 15) => 8.0. The only range in
        // vanilla's list whose `left` is not 0.
        (0xAC00, 8.5, true, "Hangul override has left=1, so 8.5 and not 8.0"),
        // U+0020 space. The `space` provider is declared FIRST in default.json and
        // gives 4. unifont's entry for it is all-zero, which by the mask == 0 rule
        // would give 5.5 — so this row pins provider order, not whitespace.
        (0x0020, 4.0, false, "space provider wins over unihex's blank 5.5"),
        // U+200C ZWNJ. Same story with a starker gap: the space provider says 0
        // and unifont ships a visible placeholder box that derives 9.0.
        (0x200C, 0.0, false, "space provider wins over unihex's 9.0 placeholder"),
        // U+0041 A. The ascii sheet, ink to column 5 => 6. Its unihex line derives
        // (1, 6) => width 6 => 4.0, so the two providers disagree here too.
        (0x0041, 6.0, false, "ascii sheet, not unihex's 4.0"),
        // U+0E01 ก Thai, 32-digit line, derived (1, 6) => width 6 => 4.0. Another
        // half-width codepoint outside every override range and every sheet.
        (0x0E01, 4.0, true, "Thai half-width, derived"),
    ];

    let mut bad: Vec<String> = Vec::new();
    for &(cp, advance, is_unihex, why) in cases {
        let got = font.advance(cp);
        if got.is_none_or(|g| (g - advance).abs() > 1e-6) {
            bad.push(format!("U+{cp:04X}: advance {got:?}, want {advance} — {why}"));
        }
        if font.unihex_glyph(cp).is_some() != is_unihex {
            bad.push(format!(
                "U+{cp:04X}: unihex={} but expected {is_unihex} — {why}",
                font.unihex_glyph(cp).is_some()
            ));
        }
    }
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

/// Half-width and full-width glyphs must differ in **cell width**, not only in
/// advance — a fixed 16-column stride would make ✓ 16 columns wide and still
/// produce a plausible-looking advance if the trimming happened elsewhere.
#[test]
#[ignore = "requires client.jar plus the unifont.json/unifont.zip asset objects"]
fn half_and_full_width_real_glyphs_have_different_strides() {
    let id: ResourceLocation = "minecraft:default".parse().expect("valid id");
    let manager = jar_plus_store();
    let raster = FontLoader::new(&manager)
        .load_raster(&id, &FontOptions::none())
        .expect("the real font rasters");

    let mut bad: Vec<String> = Vec::new();
    // (codepoint, cell width, bit width of the source line, ink texel count)
    for (cp, cell_w, ink) in [(0x2713u32, 7u32, 14usize), (0x4E2D, 16, 48)] {
        let r = raster
            .raster(cp)
            .unwrap_or_else(|| panic!("U+{cp:04X} must have a drawable raster"));
        if r.cell_width() != cell_w {
            bad.push(format!(
                "U+{cp:04X}: cell width {}, want {cell_w}",
                r.cell_width()
            ));
        }
        if r.cell_height() != 16 {
            bad.push(format!("U+{cp:04X}: cell height {}, want 16", r.cell_height()));
        }
        if (r.texel_size() - 0.5).abs() > 1e-6 {
            bad.push(format!("U+{cp:04X}: texel size {}, want 0.5", r.texel_size()));
        }
        let lit = (0..r.cell_height())
            .flat_map(|ty| (0..r.cell_width()).map(move |tx| (tx, ty)))
            .filter(|&(tx, ty)| r.is_ink(tx, ty))
            .count();
        if lit != ink {
            bad.push(format!("U+{cp:04X}: {lit} lit texels, want {ink}"));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));

    // A sheet glyph beside them, so "0.5 everywhere" cannot be what passed.
    let sheet = raster.raster(0x0041).expect("A rasters");
    assert!((sheet.texel_size() - 1.0).abs() < 1e-6);
    assert_eq!(sheet.cell_height(), 8);
}
