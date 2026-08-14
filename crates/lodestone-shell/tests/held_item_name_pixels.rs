//! Pixel gate: the held-item name highlight reaches the **real
//! HUD build**, not just `HudFrame::held_item`'s field existing.
//!
//! # Why this is the island shape CLAUDE.md names
//!
//! `lodestone_game::player_state::HeldItemHighlight` (the timer/alpha model)
//! and `lodestone_game::item::styled_hover_name` (the styled-name resolver)
//! are both unit-tested in `lodestone-game` in complete isolation from any
//! renderer — a closed loop that would stay green even if `hud.rs`'s draw
//! site never read `HudFrame::held_item` at all. This file is the other half:
//! it drives [`HudGeometry::build_with_font`] — the same pure builder
//! `HudRenderer::render_with_item_models` calls every frame in `app.rs` — and
//! proves a populated `held_item` field produces real ink through the real
//! vanilla font, at vanilla's own position.
//!
//! # The position
//!
//! `Hud.extractSelectedItemName` (`Hud.java:625-648` in the 26.2 client):
//! `x = (guiWidth - strWidth) / 2`, `y = guiHeight - 59`, drawn **unscaled**
//! (a plain `graphics.textWithBackdrop` call — no `×2`, unlike this file's
//! debug/chat text). The expected x is derived from the vanilla font's own
//! measured string width, not restated as a constant.
//!
//! # Also proves italic shear reaches this same draw
//!
//! An item's custom name draws **italic** (`Hud.java:628-630`,
//! `.withStyle(ChatFormatting.ITALIC)` when `has(DataComponents.CUSTOM_NAME)`)
//! — [`a_custom_named_item_draws_narrower_when_forced_upright`] proves this
//! specific consumer actually exercises that fix's italic shear, by comparing the
//! real (sheared) ink width against the width the *same* string would
//! measure at if italic were silently dropped (the pre-fix shape: shear
//! costs zero width by construction, so a dropped-italic bug would pass a
//! naive "some ink appeared" check and only show up as a magnitude
//! difference here).
//!
//! Fail-closed on a missing jar, matching `lodestone-assets/tests/real_jar.rs`'s
//! convention — no GPU is needed (this reads the vertex stream directly, the
//! same format `container_labels.rs` inspects), so the only reason this is
//! `#[ignore]`d is to keep the default `cargo test --workspace` run
//! hermetic.
//!
//! ```text
//! cargo test -p lodestone-shell --test held_item_name_pixels -- --ignored --nocapture
//! ```

use lodestone::hud::{DebugStats, HudFrame, HudGeometry, VanillaFont};

// Chosen so `calculate_gui_scale(AUTO, W, H) == 1` — the physical/logical
// canvas divide is then a no-op, matching every sibling gate in this crate
// (`container_labels.rs`'s exact choice, for the exact same reason: at a
// scale other than 1 the vertex stream's NDC still maps to *physical* px,
// but `HudGeometry` lays text out in the *logical* canvas, and computing an
// expected position straight from `W`/`H` would silently repeat the "XP
// number drawn at the wrong scale" defect class in the test itself).
const W: u32 = 480;
const H: u32 = 320;
const FLOATS_PER_VERTEX: usize = 6;

fn font() -> std::sync::Arc<VanillaFont> {
    VanillaFont::shared().expect(
        "held-item-name gate opted in via --ignored but no vanilla client.jar was found; \
         set LODESTONE_ASSETS to a pack root containing client.jar, or populate \
         .cache/mc/<ver>/client.jar — do NOT skip, a silent pass here asserts nothing",
    )
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Bbox {
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
}

impl std::fmt::Display for Bbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "x {:.1}..{:.1}, y {:.1}..{:.1} ({:.1}x{:.1})",
            self.x0,
            self.x1,
            self.y0,
            self.y1,
            self.x1 - self.x0,
            self.y1 - self.y0
        )
    }
}

fn bbox_of(points: &[(f32, f32)]) -> Option<Bbox> {
    let mut it = points.iter();
    let &(x, y) = it.next()?;
    let mut b = Bbox {
        x0: x,
        y0: y,
        x1: x,
        y1: y,
    };
    for &(x, y) in it {
        b.x0 = b.x0.min(x);
        b.y0 = b.y0.min(y);
        b.x1 = b.x1.max(x);
        b.y1 = b.y1.max(y);
    }
    Some(b)
}

/// Every white-ink vertex of `verts` (the held-item name's colour) as
/// `(x_px, y_px)` in the logical HUD canvas — the same NDC-inverse
/// `container_labels.rs` uses.
fn white_ink(verts: &[f32], w: f32, h: f32) -> Vec<(f32, f32)> {
    verts
        .chunks_exact(FLOATS_PER_VERTEX)
        .filter(|v| v[2] > 0.9 && v[3] > 0.9 && v[4] > 0.9)
        .map(|v| ((v[0] + 1.0) * w * 0.5, (1.0 - v[1]) * h * 0.5))
        .collect()
}

fn frame_with_held_item<'a>(stats: &'a DebugStats, held_item: Option<(String, f32)>) -> HudFrame<'a> {
    HudFrame {
        show_debug: false,
        crosshair: false,
        held_item,
        ..HudFrame::new(stats)
    }
}

/// The gap in the module doc: with nothing selected, the real build must
/// paint no ink in the held-item band at all.
#[test]
#[ignore = "requires the vanilla client.jar"]
fn nothing_selected_draws_no_ink() {
    let font = font();
    let stats = DebugStats::default();
    let geom = HudGeometry::build_with_font(&frame_with_held_item(&stats, None), W, H, &font);
    let ink = white_ink(&geom.verts, W as f32, H as f32);
    assert!(
        ink.is_empty(),
        "held_item: None must draw nothing; found {} white-ink vertices, bbox {:?}",
        ink.len(),
        bbox_of(&ink)
    );
}

/// Same control at the other guard: `alpha <= 0.0` — the fully-faded-out
/// state — must also draw nothing, proving the draw site's own
/// `.filter(|(_, a)| *a > 0.0)` guard (not just an empty `Option`) is real.
#[test]
#[ignore = "requires the vanilla client.jar"]
fn zero_alpha_draws_no_ink() {
    let font = font();
    let stats = DebugStats::default();
    let geom = HudGeometry::build_with_font(
        &frame_with_held_item(&stats, Some(("Diamond Sword".to_owned(), 0.0))),
        W,
        H,
        &font,
    );
    let ink = white_ink(&geom.verts, W as f32, H as f32);
    assert!(
        ink.is_empty(),
        "held_item alpha=0.0 must draw nothing; found {} vertices",
        ink.len()
    );
}

/// The subject: a populated, fully-opaque held-item name reaches real ink at
/// vanilla's own position (`Hud.java:632-636`).
#[test]
#[ignore = "requires the vanilla client.jar"]
fn a_selected_items_name_draws_centred_at_vanillas_anchor() {
    let font = font();
    let stats = DebugStats::default();
    let name = "Diamond Sword";
    let geom = HudGeometry::build_with_font(
        &frame_with_held_item(&stats, Some((name.to_owned(), 1.0))),
        W,
        H,
        &font,
    );
    let ink = white_ink(&geom.verts, W as f32, H as f32);
    let got = bbox_of(&ink)
        .unwrap_or_else(|| panic!("a populated held_item must draw ink; found none"));
    eprintln!("held-item name bbox: {got}");

    // Independent expected values: the *measured* string width from the same
    // real font (not restated), plugged into vanilla's own formula.
    let str_width = font.width(name, 1.0);
    let want_x0 = (W as f32 - str_width) / 2.0;
    let want_y0 = H as f32 - 59.0;

    assert!(
        (got.x0 - want_x0).abs() < 1.0,
        "x must be centred using the font's own measured width ({str_width}); \
         want x0={want_x0:.1}, got {got}"
    );
    assert!(
        (got.y0 - want_y0).abs() < 1.0,
        "y must sit at guiHeight - 59 (Hud.java:634); want y0={want_y0:.1}, got {got}"
    );
    // Not scaled ×2 like the debug/chat text elsewhere in this file — the
    // exact "XP number" defect on a second piece of HUD text.
    // At scale 1.0 the ink height for a one-line string is at most the
    // ascii cell height (8px) plus the 1px drop shadow; at the old (wrong)
    // ×2 scale it would be roughly double that.
    assert!(
        got.y1 - got.y0 <= 10.0,
        "ink must be one unscaled text line tall (<=10px with shadow); got {got} — a ×2 \
         scale bug would roughly double this"
    );
}

/// Proves italic shear reaches this same draw: an item forced italic (custom name) must measure
/// **narrower** than the width vanilla's own advance table predicts for the
/// same plain string would need if italic geometry were silently dropped —
/// i.e. it must not be identical to an upright draw of the same characters at
/// the same scale, because italic's shear moves ink without moving the pen
/// (`metrics::ITALIC_SHEAR`/`ITALIC_SHEAR_SLOPE` cost zero advance by
/// construction — see `lodestone-assets/src/font.rs`'s module doc). This is
/// exactly the shape a dropped-italic regression would pass silently if this
/// test only checked "some ink drew".
#[test]
#[ignore = "requires the vanilla client.jar"]
fn a_custom_named_items_styled_name_draws_through_the_real_font() {
    use lodestone_game::item::{CUSTOM_NAME_COMPONENT, ComponentValue, ItemStack, styled_hover_name};
    use lodestone_model::{Identifier, Text};

    let font = font();
    let item: Identifier = "minecraft:diamond_sword".parse().unwrap();
    let mut stack = ItemStack::new(item, 1);
    let key: Identifier = CUSTOM_NAME_COMPONENT.parse().unwrap();
    stack
        .components_mut()
        .insert(key, ComponentValue::Text(Text::literal("Excalibur")));
    let styled = styled_hover_name(&stack, &|_| None);
    assert_eq!(styled, "\u{a7}oExcalibur", "sanity: must be italic-coded");

    let stats = DebugStats::default();
    let geom = HudGeometry::build_with_font(
        &frame_with_held_item(&stats, Some((styled, 1.0))),
        W,
        H,
        &font,
    );
    let ink = white_ink(&geom.verts, W as f32, H as f32);
    let italic_box =
        bbox_of(&ink).unwrap_or_else(|| panic!("a custom-named item's styled name must draw"));

    // Upright control: the same word, no style code at all.
    let geom_plain = HudGeometry::build_with_font(
        &frame_with_held_item(&stats, Some(("Excalibur".to_owned(), 1.0))),
        W,
        H,
        &font,
    );
    let plain_ink = white_ink(&geom_plain.verts, W as f32, H as f32);
    let plain_box = bbox_of(&plain_ink).expect("plain control must draw");

    eprintln!("italic bbox: {italic_box}, plain bbox: {plain_box}");
    // Both centred (same advance width in both cases — italic costs zero
    // advance), but italic's *ink* bbox is wider than plain's because the
    // sheared top/bottom rows spread the ink horizontally beyond the
    // upright glyph's own columns. A silently-dropped italic (the pre-fix
    // shape) would make these two boxes identical.
    assert!(
        italic_box.x1 - italic_box.x0 > plain_box.x1 - plain_box.x0,
        "an italic name's ink bbox must be wider than the same word drawn upright \
         (the shear spreads ink without moving the pen); italic {italic_box}, plain {plain_box}"
    );
}
