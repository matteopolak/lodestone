//! Real-jar acceptance gate for the GUI sprite atlas producer
//! ([`GuiAtlas`]).
//!
//! Where the hermetic unit tests in `gui_atlas.rs` prove the enumeration,
//! scaling, and UV math against a synthetic `MemorySource`, this gate proves the
//! producer against the **actual vanilla `client.jar`**: that it discovers the
//! real modern sprite tree, places the exact HUD sprites the shell's HUD wires,
//! reports their real native sizes, and honours a real nine-slice `.png.mcmeta`
//! by decomposing where a stretch sprite would not.
//!
//! ## Why this is `#[ignore]`d and fails *closed*
//!
//! It needs a fetched `client.jar`, which CI and a fresh checkout do not have.
//! So it is `#[ignore]`d: running it is an **explicit** request for the jar, and
//! [`manager`] panics (never silently passes) when the jar is absent — a GUI
//! atlas gate with no jar is not evidence.

use std::path::PathBuf;

use lodestone_assets::{ResourceManager, ZipSource};
use lodestone_render::GuiAtlas;

fn cache_root() -> Option<PathBuf> {
    Some(
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .parent()?
            .parent()?
            .join(".cache/mc"),
    )
}

/// Prefers 26.2 explicitly so a fetched legacy jar can never silently swap the
/// sprite corpus out from under a gate that expects the modern layout.
fn client_jar() -> Option<PathBuf> {
    let cache = cache_root()?;
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

/// A resource manager over the real `client.jar`. Fails **closed**.
fn manager() -> ResourceManager {
    let jar = client_jar().unwrap_or_else(|| {
        panic!(
            "no client.jar under .cache/mc/<version>/ — fetch it first \
             (cargo run -p xtask -- fetch-assets). A GUI atlas gate with no jar is not evidence."
        )
    });
    let source = ZipSource::open(&jar).expect("open client.jar");
    ResourceManager::new(vec![Box::new(source)])
}

/// The HUD sprites the shell actually wires, with their known native sizes in
/// vanilla 26.2. If a future jar renames or resizes one of these, the HUD wire
/// silently loses that element — so pin them here.
const EXPECTED_HUD_SPRITES: &[(&str, u32, u32)] = &[
    ("hud/heart/full", 9, 9),
    ("hud/heart/half", 9, 9),
    ("hud/heart/container", 9, 9),
    ("hud/food_full", 9, 9),
    ("hud/food_empty", 9, 9),
    ("hud/hotbar", 182, 22),
    ("hud/hotbar_selection", 24, 23),
    ("hud/experience_bar_background", 182, 5),
    ("hud/experience_bar_progress", 182, 5),
];

#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn gui_atlas_covers_the_real_hud_sprites() {
    let atlas = GuiAtlas::build(&manager()).expect("build gui atlas from real jar");

    // The modern layout carries hundreds of sprites; a handful would mean the
    // enumeration missed the tree. Vanilla 26.2 has ~466.
    assert!(
        atlas.sprite_count() > 100,
        "only {} gui sprites stitched — enumeration likely missed the tree",
        atlas.sprite_count()
    );
    eprintln!("=== gui atlas: {} sprites ===", atlas.sprite_count());

    for &(id, w, h) in EXPECTED_HUD_SPRITES {
        assert!(atlas.contains(id), "missing HUD sprite {id}");
        assert_eq!(
            atlas.native_size(id),
            Some((w, h)),
            "unexpected native size for {id}"
        );
    }
}

#[test]
#[ignore = "requires a fetched vanilla client.jar"]
fn real_nine_slice_sprite_decomposes() {
    let atlas = GuiAtlas::build(&manager()).expect("build gui atlas from real jar");

    // `container/inventory/effect_background` is a real 32x32 nine-slice panel
    // (border 4). Drawn larger than native it must decompose into many pieces —
    // proof the producer read and applied its `.png.mcmeta`, not defaulted to
    // stretch. A stretch sprite would yield exactly one quad here.
    let id = "container/inventory/effect_background";
    assert!(atlas.contains(id), "nine-slice test sprite {id} missing");
    let quads = atlas.geometry(id, 0.0, 0.0, 120.0, 64.0);
    assert!(
        quads.len() >= 9,
        "nine-slice sprite {id} produced {} quads — mcmeta not applied?",
        quads.len()
    );

    // A plain HUD sprite (no mcmeta) must stay a single stretched quad, so the
    // two paths are demonstrably different on the same atlas.
    let stretch = atlas.geometry("hud/heart/full", 0.0, 0.0, 18.0, 18.0);
    assert_eq!(
        stretch.len(),
        1,
        "a no-mcmeta sprite must stretch as one quad"
    );
    eprintln!(
        "=== nine-slice {} → {} quads; stretch heart → 1 quad ===",
        id,
        quads.len()
    );
}
