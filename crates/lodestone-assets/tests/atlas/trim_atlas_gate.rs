//! Jar-backed gate for the real armour-trim atlas (`crate::trim`,
//! `crate::palette_bake`) — `#[ignore]`d, run with
//! `cargo test -p lodestone-assets --test trim_atlas_gate -- --ignored --nocapture`
//! once a jar has been fetched to `.cache/mc/<version>/client.jar`.
//!
//! Everything here is checked against real bytes out of `client.jar`, not
//! against this crate's own hand-derived expectations — `docs/armour-rendering.md`'s
//! "Trims" section and `crate::trim`'s module docs record the specific
//! pixel positions and colours quoted below as having been read directly out
//! of the real `sentry.png`/`iron.png`/`iron_darker.png`/`trim_palette.png`
//! (via Pillow, outside this crate, while investigating the feature).

use lodestone_assets::{ResourceManager, TrimAtlas, ZipSource, equipment::ArmourLayerType, trim};
use std::path::PathBuf;

fn client_jar() -> Option<PathBuf> {
    let cache = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()?
        .parent()?
        .join(".cache/mc");
    let preferred = cache.join("26.2").join("client.jar");
    if preferred.is_file() {
        return Some(preferred);
    }
    let entries = std::fs::read_dir(&cache).ok()?;
    for entry in entries.flatten() {
        let candidate = entry.path().join("client.jar");
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn manager() -> ResourceManager {
    let jar = client_jar().expect("no client.jar under .cache/mc/<version>/; fetch it first");
    let source = ZipSource::open(&jar).expect("open client.jar");
    ResourceManager::new(vec![Box::new(source)])
}

/// Every sprite the descriptor promises bakes cleanly, with no missing
/// texture, decode error, or palette error against the real jar — 18
/// patterns x 16 suffixes x 2 layer types = 576.
#[test]
#[ignore = "needs a fetched client.jar"]
fn every_trim_sprite_bakes_cleanly_against_the_real_jar() {
    let manager = manager();
    let (atlas, report) = TrimAtlas::load_reported(&manager).expect("descriptor present");

    assert_eq!(
        report.bake.reference_palette_error, None,
        "trim_palette.png must load"
    );
    assert!(
        report.bake.missing_base_textures.is_empty(),
        "missing base textures: {:?}",
        report.bake.missing_base_textures
    );
    assert!(
        report.bake.decode_errors.is_empty(),
        "decode errors: {:?}",
        report.bake.decode_errors
    );
    assert!(
        report.bake.palette_errors.is_empty(),
        "palette errors: {:?}",
        report.bake.palette_errors
    );
    assert_eq!(
        report.bake.loaded,
        trim::TRIM_PATTERNS.len() * 16 * 2,
        "18 patterns x 16 suffixes (11 materials + 5 _darker overrides) x 2 layer types"
    );
    assert_eq!(atlas.len(), report.bake.loaded);
}

/// A hand-verified pixel, end to end: `sentry.png`'s pixel `(11, 0)` is the
/// reference palette's index-0 grey (`224,224,224,255`), so the baked
/// `sentry_iron` sprite must carry iron's own index-0 colour
/// (`197,210,212,255`, `trims/color_palettes/iron.png`'s first entry) at that
/// exact offset.
#[test]
#[ignore = "needs a fetched client.jar"]
fn a_hand_verified_pixel_recolours_to_irons_own_first_palette_entry() {
    let manager = manager();
    let atlas = TrimAtlas::load(&manager).expect("atlas loads");

    let sentry = trim::trim_pattern("sentry").expect("sentry exists");
    let iron = trim::trim_material("iron").expect("iron exists");

    // Wearer is *not* iron, so no override fires — plain `iron` suffix.
    let sprite = atlas
        .sprite_for(sentry, iron, ArmourLayerType::Humanoid, "diamond")
        .expect("sentry_iron sprite baked");
    assert_eq!(sprite.width, 64, "trim sprites are 64x32, matching the armour sheet");
    assert_eq!(sprite.height, 32);

    let (x, y) = (11u32, 0u32);
    let offset = ((y * sprite.width + x) * 4) as usize;
    let px = &sprite.rgba[offset..offset + 4];
    assert_eq!(
        px,
        &[197, 210, 212, 255],
        "pixel (11,0) must recolour to iron's own index-0 palette entry"
    );
}

/// The wearer-aware override, end to end: the *same* pixel differs between a
/// diamond-worn (plain `iron`) and an iron-worn (`iron_darker`) resolution —
/// proving the override genuinely changes which baked sprite is selected,
/// not merely that a table entry exists.
#[test]
#[ignore = "needs a fetched client.jar"]
fn the_same_pixel_differs_between_the_overridden_and_plain_suffix() {
    let manager = manager();
    let atlas = TrimAtlas::load(&manager).expect("atlas loads");

    let sentry = trim::trim_pattern("sentry").expect("sentry exists");
    let iron = trim::trim_material("iron").expect("iron exists");

    let on_diamond = atlas
        .sprite_for(sentry, iron, ArmourLayerType::Humanoid, "diamond")
        .expect("plain iron sprite baked");
    let on_iron = atlas
        .sprite_for(sentry, iron, ArmourLayerType::Humanoid, "iron")
        .expect("iron_darker sprite baked");

    let (x, y) = (11u32, 0u32);
    let offset = ((y * on_diamond.width + x) * 4) as usize;
    let plain_px = &on_diamond.rgba[offset..offset + 4];
    let darker_px = &on_iron.rgba[offset..offset + 4];

    assert_eq!(plain_px, &[197, 210, 212, 255]);
    // iron_darker's own index-0 palette entry, read directly off
    // `trims/color_palettes/iron_darker.png`.
    assert_eq!(darker_px, &[162, 176, 179, 255]);
    assert_ne!(
        plain_px, darker_px,
        "wearing iron armour must select the darker override sprite, not the plain one"
    );
}

/// The always-transparent background must stay transparent through the
/// recolour — a control that the palette swap does not accidentally paint
/// the whole sheet opaque.
#[test]
#[ignore = "needs a fetched client.jar"]
fn background_pixels_outside_the_pattern_stay_fully_transparent() {
    let manager = manager();
    let atlas = TrimAtlas::load(&manager).expect("atlas loads");
    let sentry = trim::trim_pattern("sentry").expect("sentry exists");
    let iron = trim::trim_material("iron").expect("iron exists");
    let sprite = atlas
        .sprite_for(sentry, iron, ArmourLayerType::Humanoid, "diamond")
        .expect("sprite baked");

    // (0,0) is background on every 64x32 humanoid armour-style sheet's
    // corner — measured directly: `sentry.png` has 1956 fully transparent
    // pixels out of 2048, so a corner pixel is transparent with overwhelming
    // probability, and (0,0) specifically was confirmed transparent while
    // building the pixel census this gate's other cases use.
    let px = &sprite.rgba[0..4];
    assert_eq!(px[3], 0, "corner pixel must stay transparent after recolour");
}
