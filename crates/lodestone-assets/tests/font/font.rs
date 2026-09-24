//! Tests for the font/text-metrics layer ([`lodestone_assets::font`]).
//!
//! Fixtures are hermetic: bitmap sheets are encoded to PNG in-memory and served
//! through a [`MemorySource`]-backed [`ResourceManager`], so nothing here needs a
//! real `client.jar`.

use lodestone_assets::font::{
    Font, FontDefinition, FontLoader, FontOption, FontOptions, ProviderDef,
};
use lodestone_assets::{MemorySource, ResourceLocation, ResourceManager};

/// Encodes an RGBA8 buffer to PNG bytes.
fn encode_png(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    {
        let mut enc = png::Encoder::new(&mut out, width, height);
        enc.set_color(png::ColorType::Rgba);
        enc.set_depth(png::BitDepth::Eight);
        let mut w = enc.write_header().unwrap();
        w.write_image_data(rgba).unwrap();
    }
    out
}

/// Builds a single-row bitmap sheet of `n` cells, each `cell` px square. For
/// each glyph, a single opaque pixel is placed at column `rightmost[i]` (its
/// rightmost non-transparent column), row 0. Everything else is transparent.
fn sheet(cell: u32, rightmost: &[Option<u32>]) -> Vec<u8> {
    let n = rightmost.len() as u32;
    let w = cell * n;
    let h = cell;
    let mut rgba = vec![0u8; (w * h * 4) as usize];
    for (i, r) in rightmost.iter().enumerate() {
        if let Some(col) = r {
            let x = i as u32 * cell + col;
            let y = 0;
            let idx = ((y * w + x) * 4) as usize;
            rgba[idx] = 255;
            rgba[idx + 1] = 255;
            rgba[idx + 2] = 255;
            rgba[idx + 3] = 255; // opaque
        }
    }
    encode_png(w, h, &rgba)
}

fn manager(entries: Vec<(&str, Vec<u8>)>) -> ResourceManager {
    let mut s = MemorySource::new("test");
    for (path, bytes) in entries {
        s.insert(path, bytes);
    }
    ResourceManager::new(vec![Box::new(s)])
}

fn loc(s: &str) -> ResourceLocation {
    ResourceLocation::parse(s).unwrap()
}

// --- parsing -------------------------------------------------------------

#[test]
fn parses_all_provider_kinds() {
    let json = br#"{
        "providers": [
            {"type":"reference","id":"minecraft:include/space"},
            {"type":"space","advances":{" ":4,"\u200c":0}},
            {"type":"bitmap","file":"minecraft:font/ascii.png","ascent":7,"height":8,
             "chars":["AB","CD"]},
            {"type":"ttf","file":"minecraft:font/x.ttf","size":11,"oversample":2,
             "shift":[0,1],"skip":"ab"},
            {"type":"unihex","hex_file":"minecraft:font/u.zip",
             "size_overrides":[{"from":"\u3000","to":"\u3002","left":0,"right":15}]}
        ]
    }"#;
    let def = FontDefinition::parse(json).expect("parse");
    assert_eq!(def.providers.len(), 5);
    assert!(matches!(
        def.providers[0].def,
        ProviderDef::Reference { .. }
    ));
    assert!(matches!(def.providers[1].def, ProviderDef::Space { .. }));
    assert!(matches!(def.providers[2].def, ProviderDef::Bitmap { .. }));
    assert!(matches!(def.providers[3].def, ProviderDef::Ttf { .. }));
    assert!(matches!(def.providers[4].def, ProviderDef::Unihex { .. }));

    if let ProviderDef::Bitmap {
        chars,
        height,
        ascent,
        ..
    } = &def.providers[2].def
    {
        assert_eq!(*height, 8);
        assert_eq!(*ascent, 7);
        // "AB","CD" -> two rows of two codepoints.
        assert_eq!(chars.len(), 2);
        assert_eq!(chars[0], vec![b'A' as u32, b'B' as u32]);
        assert_eq!(chars[1], vec![b'C' as u32, b'D' as u32]);
    } else {
        panic!("expected bitmap");
    }
}

#[test]
fn bitmap_height_defaults_to_eight() {
    let json = br#"{"providers":[{"type":"bitmap","file":"minecraft:font/a.png",
        "ascent":7,"chars":["A"]}]}"#;
    let def = FontDefinition::parse(json).unwrap();
    if let ProviderDef::Bitmap { height, .. } = &def.providers[0].def {
        assert_eq!(*height, 8);
    } else {
        panic!();
    }
}

#[test]
fn malformed_font_json_is_rejected() {
    assert!(FontDefinition::parse(b"not json").is_err());
    assert!(FontDefinition::parse(br#"{"providers":[{"type":"bitmap"}]}"#).is_err());
}

// --- bitmap advance derivation (the thing that must be right) -------------

#[test]
fn advance_is_rightmost_nontransparent_column_plus_one() {
    // 8px cells, scale 1 (height 8). 'A' rightmost col 4 -> actual 5 -> advance 6.
    // 'B' rightmost col 2 -> actual 3 -> advance 4. 'C' blank -> advance 1.
    let png = sheet(8, &[Some(4), Some(2), None]);
    let json = br#"{"providers":[{"type":"bitmap","file":"minecraft:font/t.png",
        "ascent":7,"height":8,"chars":["ABC"]}]}"#;
    let mgr = manager(vec![
        ("assets/minecraft/textures/font/t.png", png),
        ("assets/minecraft/font/default.json", json.to_vec()),
    ]);
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .expect("load font");
    assert_eq!(font.advance(b'A' as u32), Some(6.0));
    assert_eq!(font.advance(b'B' as u32), Some(4.0));
    assert_eq!(font.advance(b'C' as u32), Some(1.0));
}

#[test]
fn height_override_scales_advance() {
    // 8px cells but declared height 16 -> pixelScale 2. 'A' rightmost col 3
    // -> actual 4 -> advance round(4*2)+1 = 9.
    let png = sheet(8, &[Some(3)]);
    let json = br#"{"providers":[{"type":"bitmap","file":"minecraft:font/t.png",
        "ascent":14,"height":16,"chars":["A"]}]}"#;
    let mgr = manager(vec![
        ("assets/minecraft/textures/font/t.png", png),
        ("assets/minecraft/font/default.json", json.to_vec()),
    ]);
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    assert_eq!(font.advance(b'A' as u32), Some(9.0));
}

#[test]
fn duplicate_codepoint_within_one_providers_grid_uses_the_last_cell() {
    // 'A' declared twice in the same grid: slot 0 (rightmost col 1 -> actual
    // 2 -> advance 3) and slot 1 (rightmost col 6 -> actual 7 -> advance 8).
    // Vanilla's own bitmap-provider-definition load step's `charMap.put` is a
    // plain overwrite
    // as it walks the grid, so the *last* declaration must win, not the
    // first.
    let png = sheet(8, &[Some(1), Some(6)]);
    let json = br#"{"providers":[{"type":"bitmap","file":"minecraft:font/t.png",
        "ascent":7,"height":8,"chars":["AA"]}]}"#;
    let mgr = manager(vec![
        ("assets/minecraft/textures/font/t.png", png),
        ("assets/minecraft/font/default.json", json.to_vec()),
    ]);
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    assert_eq!(
        font.advance(b'A' as u32),
        Some(8.0),
        "the second (slot 1) declaration of 'A' must win, matching vanilla's plain-overwrite Map.put"
    );
    let g = font.bitmap_glyph(b'A' as u32).unwrap();
    assert_eq!(g.cell, [8, 0, 8, 8], "must carry slot 1's cell, not slot 0's");
}

#[test]
fn bitmap_glyph_carries_cell_rect() {
    // Two cells; 'B' is slot 1 -> cell x=8.
    let png = sheet(8, &[Some(4), Some(2)]);
    let json = br#"{"providers":[{"type":"bitmap","file":"minecraft:font/t.png",
        "ascent":7,"height":8,"chars":["AB"]}]}"#;
    let mgr = manager(vec![
        ("assets/minecraft/textures/font/t.png", png),
        ("assets/minecraft/font/default.json", json.to_vec()),
    ]);
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    let g = font.bitmap_glyph(b'B' as u32).expect("B is a bitmap glyph");
    assert_eq!(g.cell, [8, 0, 8, 8]);
    assert_eq!(g.ascent, 7);
    assert_eq!(g.file, loc("minecraft:font/t.png"));
}

#[test]
fn bitmap_raster_preserves_native_rgba_including_partial_alpha() {
    // A resource-pack glyph is not necessarily vanilla's white opaque mask:
    // its grey RGB and 50%-ish alpha must reach the HUD unchanged. The second
    // texel has RGB despite zero alpha, proving alpha still decides ink.
    let png = encode_png(
        2,
        1,
        &[
            64, 96, 128, 127, // coloured, partially transparent ink
            250, 240, 230, 0, // transparent, never drawable
        ],
    );
    let json = br#"{"providers":[{"type":"bitmap","file":"minecraft:font/t.png",
        "ascent":7,"height":1,"chars":["A"]}]}"#;
    let mgr = manager(vec![
        ("assets/minecraft/textures/font/t.png", png),
        ("assets/minecraft/font/default.json", json.to_vec()),
    ]);
    let raster = FontLoader::new(&mgr)
        .load_raster(&loc("minecraft:default"), &FontOptions::none())
        .expect("load raster font");
    let glyph = raster.raster('A' as u32).expect("A has a bitmap raster");

    assert_eq!(
        glyph.texel_rgba(0, 0),
        [64.0 / 255.0, 96.0 / 255.0, 128.0 / 255.0, 127.0 / 255.0]
    );
    assert!(glyph.is_ink(0, 0), "partial alpha is still drawable ink");
    assert_eq!(glyph.texel_rgba(1, 0), [250.0 / 255.0, 240.0 / 255.0, 230.0 / 255.0, 0.0]);
    assert!(!glyph.is_ink(1, 0), "zero alpha is never drawable ink");
}

// --- space provider ------------------------------------------------------

#[test]
fn space_provider_supplies_advances() {
    let json = br#"{"providers":[{"type":"space","advances":{" ":4,"\u200c":0}}]}"#;
    let mgr = manager(vec![("assets/minecraft/font/default.json", json.to_vec())]);
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    assert_eq!(font.advance(b' ' as u32), Some(4.0));
    assert_eq!(font.advance(0x200c), Some(0.0));
}

// --- priority: first-declared provider wins ------------------------------

#[test]
fn first_declared_provider_wins_on_overlap() {
    // Provider order: space (advance 4 for ' '), then a bitmap that ALSO defines
    // ' ' as a blank cell (advance 1). First declared (space) must win -> 4.
    // This mirrors vanilla's default font, where the space provider beats
    // ascii.png's blank space cell.
    let png = sheet(8, &[None]); // blank ' '
    let json = br#"{"providers":[
        {"type":"space","advances":{" ":4}},
        {"type":"bitmap","file":"minecraft:font/t.png","ascent":7,"height":8,"chars":[" "]}
    ]}"#;
    let mgr = manager(vec![
        ("assets/minecraft/textures/font/t.png", png),
        ("assets/minecraft/font/default.json", json.to_vec()),
    ]);
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    assert_eq!(font.advance(b' ' as u32), Some(4.0));
}

/// The discriminating arm the previous test cannot provide: **bitmap**
/// declared first, **space** declared second, for the same codepoint, with
/// deliberately different advances (bitmap 3, space 40 -- equal advances
/// cannot distinguish "the right provider won" from "the right *value*
/// happened to come out"). Declaration order is what vanilla's
/// `FontManager`'s double-reversal composes down to (the reversal exists to
/// make cross-*pack* priority work while preserving each pack's own JSON
/// order, not to bias one provider *type* over another) -- so the earlier
/// declaration wins here regardless of its kind, exactly as the previous
/// test's space-before-bitmap arm won regardless of *its* kind. Proves both
/// that the winner is bitmap's 3, and -- the coordinator's own second arm --
/// that the loser's value (40) is not what comes out.
#[test]
fn bitmap_declared_before_space_wins_over_a_later_space_entry() {
    let png = sheet(8, &[Some(1)]); // 'C': rightmost col 1 -> actual 2 -> advance 3
    let json = br#"{"providers":[
        {"type":"bitmap","file":"minecraft:font/t.png","ascent":7,"height":8,"chars":["C"]},
        {"type":"space","advances":{"C":40}}
    ]}"#;
    let mgr = manager(vec![
        ("assets/minecraft/textures/font/t.png", png),
        ("assets/minecraft/font/default.json", json.to_vec()),
    ]);
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    assert_eq!(
        font.advance(b'C' as u32),
        Some(3.0),
        "the earlier-declared bitmap provider must win -- got the later space \
         provider's 40 instead of the bitmap's 3, or something else entirely"
    );
    assert!(
        font.bitmap_glyph(b'C' as u32).is_some(),
        "the winning glyph must actually be the bitmap one, not merely a \
         coincidentally-equal advance from the space provider"
    );
}

/// The same discriminating shape, but through a `reference` -- the actual
/// structure a pack like the one that motivated this test uses: a root font
/// declares a bitmap provider directly, then references a second file that
/// declares the conflicting space entry, mirroring `nameplates:default.json`
/// declaring its own `background/space_split.png` bitmap ahead of a
/// `reference` to a shared "base" font carrying the pack's real gap-width
/// table. `FontLoader::flatten` recurses into the reference at its declared
/// position, so this must resolve identically to the flat, single-file case
/// above: the bitmap (declared first, before the reference) still wins.
#[test]
fn bitmap_before_a_referenced_spaces_wins_the_same_way_as_the_flat_case() {
    let png = sheet(8, &[Some(1)]); // 'C': actual 2 -> advance 3, same as above
    let base_json = br#"{"providers":[{"type":"space","advances":{"C":40}}]}"#;
    let root_json = br#"{"providers":[
        {"type":"bitmap","file":"minecraft:font/t.png","ascent":7,"height":8,"chars":["C"]},
        {"type":"reference","id":"minecraft:include/base"}
    ]}"#;
    let mgr = manager(vec![
        ("assets/minecraft/textures/font/t.png", png),
        ("assets/minecraft/font/include/base.json", base_json.to_vec()),
        ("assets/minecraft/font/default.json", root_json.to_vec()),
    ]);
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    assert_eq!(
        font.advance(b'C' as u32),
        Some(3.0),
        "a directly-declared bitmap provider ahead of a `reference` must still \
         win over that reference's own space entry -- referencing must not \
         silently promote the referenced font's providers ahead of ones \
         declared earlier in the referencing file"
    );
}

/// And the mirror of the reference case: the `reference` declared **first**,
/// so the referenced font's space entry should win over a bitmap declared
/// after it in the referencing file -- proving the reference's expanded
/// providers really do take the *position* the `reference` line occupies,
/// not always-first or always-last regardless of where it was written.
#[test]
fn a_referenced_space_declared_before_a_local_bitmap_wins() {
    let png = sheet(8, &[Some(1)]);
    let base_json = br#"{"providers":[{"type":"space","advances":{"C":40}}]}"#;
    let root_json = br#"{"providers":[
        {"type":"reference","id":"minecraft:include/base"},
        {"type":"bitmap","file":"minecraft:font/t.png","ascent":7,"height":8,"chars":["C"]}
    ]}"#;
    let mgr = manager(vec![
        ("assets/minecraft/textures/font/t.png", png),
        ("assets/minecraft/font/include/base.json", base_json.to_vec()),
        ("assets/minecraft/font/default.json", root_json.to_vec()),
    ]);
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    assert_eq!(
        font.advance(b'C' as u32),
        Some(40.0),
        "the reference, declared before the bitmap, must expand in place and \
         win with its space entry's 40 -- got the bitmap's 3 instead"
    );
}

// --- multi-pack font stacking (vanilla merges, never overrides) ----------

/// Two packs, both declaring `assets/minecraft/font/default.json`: the
/// lower-priority one (stands in for the jar) covers 'A' and 'Z', the
/// higher-priority one covers only 'A'. Vanilla's own font manager stacks every
/// active pack's own copy of a font id's definition file rather than letting
/// the top one win outright (its own "prepare" step reads via
/// its own "list matching resource stacks" step, not a single-winner "get
/// resource" accessor -- the
/// same shape `ResourceManager::read_stack` already documents for language
/// files). So the higher-priority pack's own 'A' must win, **and** the
/// lower-priority pack's 'Z' -- which the top pack never mentions -- must
/// still resolve, proving the bottom layer was merged in rather than
/// discarded wholesale.
#[test]
fn a_higher_priority_packs_font_definition_merges_with_a_lower_ones_rather_than_replacing_it() {
    let jar_a = sheet(8, &[Some(2), None]); // 'A' col2->advance4, 'Z' blank->advance1
    let jar_json = br#"{"providers":[{"type":"bitmap","file":"minecraft:font/jar.png",
        "ascent":7,"height":8,"chars":["AZ"]}]}"#;
    let pack_a = sheet(8, &[Some(6)]); // 'A' col6->advance8, distinct from the jar's
    let pack_json = br#"{"providers":[{"type":"bitmap","file":"minecraft:font/pack.png",
        "ascent":7,"height":8,"chars":["A"]}]}"#;

    let mut jar_source = MemorySource::new("jar");
    jar_source.insert("assets/minecraft/textures/font/jar.png", jar_a);
    jar_source.insert("assets/minecraft/font/default.json", jar_json.to_vec());

    let mut pack_source = MemorySource::new("pack");
    pack_source.insert("assets/minecraft/textures/font/pack.png", pack_a);
    pack_source.insert("assets/minecraft/font/default.json", pack_json.to_vec());

    // Lowest priority first: jar at the bottom, pack on top.
    let mgr = ResourceManager::new(vec![Box::new(jar_source), Box::new(pack_source)]);
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .expect("load font");

    let a = font.bitmap_glyph(b'A' as u32).expect("A resolves");
    assert_eq!(
        a.file,
        loc("minecraft:font/pack.png"),
        "the higher-priority pack's own 'A' must win"
    );
    assert_eq!(font.advance(b'A' as u32), Some(8.0));

    let z = font
        .bitmap_glyph(b'Z' as u32)
        .expect("Z must still resolve from the lower-priority (jar) layer -- it must not be silently discarded because a higher-priority pack also ships a default.json");
    assert_eq!(z.file, loc("minecraft:font/jar.png"));
    assert_eq!(font.advance(b'Z' as u32), Some(1.0));
}

// --- references, filters, cycles -----------------------------------------

#[test]
fn references_expand_in_declaration_order() {
    let inc = br#"{"providers":[{"type":"space","advances":{"A":7}}]}"#;
    let root = br#"{"providers":[{"type":"reference","id":"minecraft:include/x"}]}"#;
    let mgr = manager(vec![
        ("assets/minecraft/font/include/x.json", inc.to_vec()),
        ("assets/minecraft/font/default.json", root.to_vec()),
    ]);
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    assert_eq!(font.advance(b'A' as u32), Some(7.0));
}

#[test]
fn filter_excludes_provider_when_option_mismatches() {
    // Provider gated on {uniform:true} is only active when Uniform is on.
    let json = br#"{"providers":[
        {"type":"space","advances":{"A":3},"filter":{"uniform":true}}
    ]}"#;
    let mgr = manager(vec![("assets/minecraft/font/default.json", json.to_vec())]);
    let loader = FontLoader::new(&mgr);
    // Default options: uniform off -> provider excluded -> no glyph.
    let off = loader
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    assert_eq!(off.advance(b'A' as u32), None);
    // Uniform on -> provider active.
    let on = loader
        .load(
            &loc("minecraft:default"),
            &FontOptions::none().with(FontOption::Uniform),
        )
        .unwrap();
    assert_eq!(on.advance(b'A' as u32), Some(3.0));
}

#[test]
fn reference_filter_merges_onto_inner() {
    // The default font references include/default with filter {uniform:false}.
    // With uniform ON, that whole reference is filtered out.
    let inc = br#"{"providers":[{"type":"space","advances":{"A":5}}]}"#;
    let root = br#"{"providers":[
        {"type":"reference","id":"minecraft:include/x","filter":{"uniform":false}}
    ]}"#;
    let mgr = manager(vec![
        ("assets/minecraft/font/include/x.json", inc.to_vec()),
        ("assets/minecraft/font/default.json", root.to_vec()),
    ]);
    let loader = FontLoader::new(&mgr);
    assert_eq!(
        loader
            .load(&loc("minecraft:default"), &FontOptions::none())
            .unwrap()
            .advance(b'A' as u32),
        Some(5.0)
    );
    assert_eq!(
        loader
            .load(
                &loc("minecraft:default"),
                &FontOptions::none().with(FontOption::Uniform)
            )
            .unwrap()
            .advance(b'A' as u32),
        None
    );
}

#[test]
fn reference_cycle_is_rejected_not_infinite() {
    let a = br#"{"providers":[{"type":"reference","id":"minecraft:b"}]}"#;
    let b = br#"{"providers":[{"type":"reference","id":"minecraft:a"}]}"#;
    let mgr = manager(vec![
        ("assets/minecraft/font/a.json", a.to_vec()),
        ("assets/minecraft/font/b.json", b.to_vec()),
    ]);
    let err = FontLoader::new(&mgr).load(&loc("minecraft:a"), &FontOptions::none());
    assert!(err.is_err(), "cycle must be an error");
}

#[test]
fn missing_font_is_an_error() {
    let mgr = manager(vec![]);
    assert!(
        FontLoader::new(&mgr)
            .load(&loc("minecraft:nope"), &FontOptions::none())
            .is_err()
    );
}

// --- string measurement + legacy codes -----------------------------------

fn ascii_font() -> ResourceManager {
    // 'A'=6, 'B'=4, 'i'=2, ' '=4 (space provider).
    let png = sheet(8, &[Some(4), Some(2), Some(0)]); // A,B,i
    let json = br#"{"providers":[
        {"type":"space","advances":{" ":4}},
        {"type":"bitmap","file":"minecraft:font/t.png","ascent":7,"height":8,"chars":["ABi"]}
    ]}"#;
    manager(vec![
        ("assets/minecraft/textures/font/t.png", png),
        ("assets/minecraft/font/default.json", json.to_vec()),
    ])
}

#[test]
fn string_width_sums_advances() {
    let mgr = ascii_font();
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    // "AB i" -> 6 + 4 + 4 + 2 = 16
    assert_eq!(font.string_width("AB i"), 16.0);
    // string_width must agree exactly with an independent sum over the public
    // per-glyph `advance()` API — the drawing side measures the same way.
    let independent: f32 = "AB i"
        .chars()
        .map(|c| font.advance(c as u32).unwrap())
        .sum();
    assert_eq!(font.string_width("AB i"), independent);
}

/// NEGATIVE CONTROL — the defect this whole subsystem exists to prevent.
///
/// The shell's legacy debug font used a **fixed advance** for every glyph, which
/// is why "was slain by Spider" rendered with the wrong spacing even though every
/// character was correct. A string-level `assert_eq!` on the decoded text cannot
/// see that defect; only a width check can. This proves the gate has teeth: a
/// fixed-advance model diverges from the sum of the real per-glyph advances, so
/// the day someone regresses `actual_glyph_width` to a cell-width constant this
/// fails. ('A'=6, 'B'=4, 'i'=2 — genuinely different widths.)
#[test]
fn negative_control_fixed_advance_diverges_from_true_width() {
    let mgr = ascii_font();
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    let true_width = font.string_width("ABi"); // 6 + 4 + 2 = 12
    let fixed_width = 3.0 * 6.0; // fixed 6px cell advance -> 18
    assert_eq!(true_width, 12.0);
    assert_ne!(
        true_width, fixed_width,
        "proportional advances must differ from a fixed-advance model"
    );
}

#[test]
fn missing_codepoint_uses_missing_advance() {
    let mgr = ascii_font();
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    // 'Z' is not in the font -> vanilla missing glyph advance is 6.
    assert_eq!(font.advance(b'Z' as u32), None);
    assert_eq!(font.string_width("Z"), 6.0);
}

#[test]
fn legacy_section_codes_are_zero_width_and_bold_adds_one() {
    let mgr = ascii_font();
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    // Plain "AB" = 10.
    assert_eq!(font.legacy_width("AB"), 10.0);
    // "§cAB": colour code is 2 chars, 0 width -> still 10.
    assert_eq!(font.legacy_width("§cAB"), 10.0);
    // "§lAB": bold adds +1 per glyph -> 6+1 + 4+1 = 12.
    assert_eq!(font.legacy_width("§lAB"), 12.0);
    // "§lA§rB": reset clears bold before B -> (6+1) + 4 = 11.
    assert_eq!(font.legacy_width("§lA§rB"), 11.0);
}

#[test]
fn bold_style_adds_one_to_advance() {
    let mgr = ascii_font();
    let font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    assert_eq!(font.advance_bold(b'A' as u32, false), Some(6.0));
    assert_eq!(font.advance_bold(b'A' as u32, true), Some(7.0));
}

// --- provider census (for coverage reporting) ----------------------------

#[test]
fn font_reports_active_provider_kinds() {
    let mgr = ascii_font();
    let font: Font = FontLoader::new(&mgr)
        .load(&loc("minecraft:default"), &FontOptions::none())
        .unwrap();
    // space + bitmap active.
    assert_eq!(font.provider_count(), 2);
    assert!(font.codepoint_count() >= 4); // A,B,i,space
}
