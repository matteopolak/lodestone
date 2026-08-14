//! Are that fix's sprites reachable? (the asset question, answered by measurement)
//!
//! That fix needs two sprite families: the empty-slot placeholders
//! `container/slot/*` and the hover-highlight pair
//! `container/slot_highlight_{back,front}`. The record said **"neither family is in
//! any atlas this client builds today"**, which reads as an asset-loading job.
//!
//! That was written from `ContainerBackground`, which stitches the three
//! `gui/container/*.png` panel sheets and nothing else — true of *that* atlas. But
//! `lodestone_render::GuiAtlas::build` enumerates **every**
//! `assets/<ns>/textures/gui/sprites/**.png` in the pack, parses each sibling
//! `.png.mcmeta`, and `crate::resources::load_gui_atlas()` already builds one that
//! `HudRenderer::attach_gui` consumes for the survival vitals. So the sprites are
//! stitched, sized and nine-slice-aware today.
//!
//! `CLAUDE.md`: *re-verify before routing around "X doesn't exist yet"* — and
//! *grep for the producer across the whole tree, not for the consumer in one named
//! file*. This file is that re-verification, kept as a test so the next person does
//! not have to redo it, and so the day a pack drops one of these sprites the
//! failure names the sprite instead of showing an empty cell.
//!
//! What is genuinely missing is **reachability, not loading**: `ContainerRenderer`
//! binds no GUI-sprite atlas, so `container.rs` cannot ask for a quad. That is the
//! remaining work, and it is a pipeline/bind-group job rather than an asset one.
//!
//! ```text
//! cargo test -p lodestone-shell --test container_slot_sprites -- --ignored --nocapture
//! ```

use lodestone_game::menu::{
    EMPTY_ARMOR_SLOT_BOOTS, EMPTY_ARMOR_SLOT_CHESTPLATE, EMPTY_ARMOR_SLOT_HELMET,
    EMPTY_ARMOR_SLOT_LEGGINGS, EMPTY_ARMOR_SLOT_SHIELD,
};

/// The highlight pair, from `AbstractContainerScreen.java:29-30`.
const HIGHLIGHT_BACK: &str = "container/slot_highlight_back";
const HIGHLIGHT_FRONT: &str = "container/slot_highlight_front";

#[test]
#[ignore = "requires the vanilla client.jar"]
fn issue_376s_sprites_are_already_in_the_gui_atlas() {
    let gui = crate_gui_atlas();

    eprintln!("=== #376 sprite reachability ===");
    eprintln!("GUI atlas holds {} sprites", gui.sprite_count());

    let mut failures: Vec<String> = Vec::new();

    // The five the player screen declares, all 16x16 (`InventoryMenu.java:29-33`).
    for id in [
        EMPTY_ARMOR_SLOT_HELMET,
        EMPTY_ARMOR_SLOT_CHESTPLATE,
        EMPTY_ARMOR_SLOT_LEGGINGS,
        EMPTY_ARMOR_SLOT_BOOTS,
        EMPTY_ARMOR_SLOT_SHIELD,
    ] {
        match gui.native_size(id) {
            None => failures.push(format!("{id} is not in the GUI atlas")),
            Some((w, h)) => {
                eprintln!("  {id:42} {w}x{h}");
                if (w, h) != (16, 16) {
                    failures.push(format!("{id} is {w}x{h}, expected 16x16"));
                }
            }
        }
    }

    // The highlight pair, 24x24, drawn at `(slot.x - 4, slot.y - 4)`.
    for id in [HIGHLIGHT_BACK, HIGHLIGHT_FRONT] {
        match gui.native_size(id) {
            None => failures.push(format!("{id} is not in the GUI atlas")),
            Some((w, h)) => {
                eprintln!("  {id:42} {w}x{h}");
                if (w, h) != (24, 24) {
                    failures.push(format!("{id} is {w}x{h}, expected 24x24"));
                }
            }
        }
    }

    // Nine-slice, not a plain quad. `GuiAtlas` already applied the `.mcmeta`, so the
    // proof is that asking for a *larger* rect yields more quads — a stretched
    // sprite yields exactly one at any size.
    //
    // One correction to the record while measuring this: the note that at the native
    // 24x24 the nine-slice "degenerates to a 1:1 draw" is true of the *image* and
    // false of the *geometry* — it is **9** quads at 24x24 (and 25 at 48x48, the
    // inner sections tiling), not one. So a consumer must size its vertex buffer for
    // the decomposition, not for one quad per sprite.
    for id in [HIGHLIGHT_BACK, HIGHLIGHT_FRONT] {
        let native = gui.geometry(id, 0.0, 0.0, 24.0, 24.0);
        let grown = gui.geometry(id, 0.0, 0.0, 48.0, 48.0);
        eprintln!("  {id:42} quads: 24x24 -> {}, 48x48 -> {}", native.len(), grown.len());
        if grown.len() <= 1 {
            failures.push(format!(
                "{id} decomposed into {} quad(s) at 48x48 — its `.mcmeta` nine-slice \
                 (border 4) was not applied, so it is being treated as a stretch",
                grown.len()
            ));
        }
    }

    // The argument against keying on slot index, as a measurement rather than a
    // claim: the family is far larger than the player screen's five.
    let family = [
        "container/slot/horse_armor",
        "container/slot/llama_armor",
        "container/slot/saddle",
        "container/slot/potion",
        "container/slot/brewing_fuel",
        "container/slot/banner_pattern",
        "container/slot/smithing_template_netherite_upgrade",
    ];
    let present = family.iter().filter(|id| gui.contains(id)).count();
    eprintln!("  {present}/{} of the wider `container/slot/*` family present", family.len());
    if present != family.len() {
        failures.push(format!(
            "only {present} of {} sampled `container/slot/*` sprites are present — the \
             'key on the slot's declared icon' design assumes they are free once the \
             atlas is reachable",
            family.len()
        ));
    }

    assert!(
        failures.is_empty(),
        "#376 sprite reachability:\n  {}",
        failures.join("\n  ")
    );
}

/// Fail-closed: a missing jar is a failure, never a skip.
fn crate_gui_atlas() -> std::sync::Arc<lodestone_render::GuiAtlas> {
    lodestone::resources::load_gui_atlas().expect(
        "the GUI sprite atlas must build from client.jar; set LODESTONE_ASSETS to a pack \
         root — do NOT treat a skip as a pass",
    )
}
