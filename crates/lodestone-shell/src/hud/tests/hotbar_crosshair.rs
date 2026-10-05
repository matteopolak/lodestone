//! Crosshair span, hotbar slots, item counts and cooldown veils.

use super::*;

#[test]
fn geometry_has_crosshair_and_text() {
    let stats = DebugStats {
        position: [1.0, 64.0, 2.0],
        status: "local world".into(),
        ..Default::default()
    };
    let geo = HudGeometry::build(&HudFrame::new(&stats), 320, 240);
    // Crosshair alone is 2 quads = 12 verts; text adds far more.
    assert!(geo.vertex_count() > 100, "expected glyphs + crosshair");
    assert_eq!(geo.verts.len() % FLOATS_PER_VERTEX, 0);
}

#[test]
fn empty_string_advances_without_panicking() {
    let stats = DebugStats::default();
    let _ = HudGeometry::build(&HudFrame::new(&stats), 1, 1);
}

#[test]
fn hotbar_items_draw_count_on_colour_stream_without_atlas() {
    // With `hotbar_items` populated but no item atlas attached, the flat
    // icons cannot draw (item_verts stays empty), but the stack-count number
    // still renders to the colour stream and nothing panics.
    let stats = DebugStats::default();
    let base = HudGeometry::build(&HudFrame::new(&stats), 640, 480).vertex_count();

    let slots = [
        Some(ItemIcon {
            item: ResourceLocation::parse("minecraft:stone").unwrap(),
            count: 64,
            damage: None,
            max_damage: None,
            enchanted: false,
            custom_model_data: None,
            dyed_color: None,
            potion_color: None,
            banner_patterns: Vec::new(),
            base_color: None,
            skin: None,
        }),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    ];
    let mut frame = HudFrame::new(&stats);
    frame.hotbar = Some(0);
    frame.hotbar_items = Some(&slots);
    let geo = HudGeometry::build(&frame, 640, 480);

    assert!(
        geo.item_verts.is_empty(),
        "no item atlas attached, so no item-sprite geometry"
    );
    assert!(
        geo.vertex_count() > base,
        "the '64' stack count must add colour-stream verts"
    );
}

#[test]
fn item_cooldown_veil_draws_only_for_an_occupied_positive_fraction_slot() {
    let stats = DebugStats::default();
    let slots = [
        Some(ItemIcon {
            item: ResourceLocation::parse("minecraft:ender_pearl").unwrap(),
            count: 1,
            damage: None,
            max_damage: None,
            enchanted: false,
            custom_model_data: None,
            dyed_color: None,
            potion_color: None,
            banner_patterns: Vec::new(),
            base_color: None,
            skin: None,
        }),
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
    ];
    let mut frame = HudFrame::new(&stats);
    frame.hotbar = Some(0);
    frame.hotbar_items = Some(&slots);

    let zero_cooldown = [0.0];
    let half_cooldown = [0.5];
    let full_cooldown = [1.0];
    let no_cooldowns: [f32; 0] = [];
    frame.hotbar_cooldowns = &zero_cooldown;
    let zero = HudGeometry::build(&frame, 640, 480).vertex_count();
    frame.hotbar_cooldowns = &half_cooldown;
    let half = HudGeometry::build(&frame, 640, 480).vertex_count();
    frame.hotbar_cooldowns = &full_cooldown;
    let full = HudGeometry::build(&frame, 640, 480).vertex_count();

    assert_eq!(half, zero + 6, "one positive cooldown must add one veil quad");
    assert_eq!(full, half, "fraction changes veil height, not its quad count");

    // Negative control: the same positive fraction must not paint an empty
    // slot merely because the parallel cooldown slice names its index.
    let empty_slots: [Option<ItemIcon>; 9] = Default::default();
    frame.hotbar_items = Some(&empty_slots);
    let empty = HudGeometry::build(&frame, 640, 480).vertex_count();
    frame.hotbar_cooldowns = &no_cooldowns;
    let empty_control = HudGeometry::build(&frame, 640, 480).vertex_count();
    assert_eq!(empty, empty_control, "an empty slot must not receive a cooldown veil");
}

/// Pins the crosshair to vanilla's real ink, not its sprite's bounding box —
/// the draw site's own doc has the pixel-by-pixel read of
/// `hud/crosshair.png`. Two hypotheses, computed from outside constants
/// rather than guessed: vanilla's real 9px-long, 1px-thick "+" (correct), and
/// this draw's own pre-fix 16px/2px-thick bar (the bug this test would have
/// caught). At `gui_scale == 1` — the floor `320x240` this repo already uses
/// elsewhere for that reason — physical and logical pixels coincide, so the
/// measured span is the real on-screen footprint, not a scaled derivative.
#[test]
fn crosshair_span_matches_vanillas_real_ink_not_the_old_wrong_hypothesis() {
    let stats = DebugStats::default();
    let mut frame = HudFrame::new(&stats);
    frame.show_debug = false;
    let (w, h) = (320.0_f32, 240.0_f32);
    let geo = HudGeometry::build(&frame, w as u32, h as u32);
    assert_eq!(geo.vertex_count(), 12, "precondition: only the crosshair draws");

    let (min_x, max_x, min_y, max_y) = ndc_bounds(&geo.verts, w, h);
    let (span_x, span_y) = (max_x - min_x, max_y - min_y);
    let (correct, wrong) = (9.0_f32, 16.0_f32);
    let mut mismatches = Vec::new();
    if (span_x - correct).abs() >= 0.01 {
        mismatches.push(format!("horizontal span {span_x} (want {correct})"));
    }
    if (span_y - correct).abs() >= 0.01 {
        mismatches.push(format!("vertical span {span_y} (want {correct})"));
    }
    assert!(
        mismatches.is_empty(),
        "crosshair does not match vanilla's real ink: {mismatches:?} — \
         note the wrong hypothesis this used to draw was {wrong}px"
    );
    // The wrong hypothesis is a real, distinct number — if `correct` and
    // `wrong` ever coincided this assertion would be measuring nothing.
    assert!((correct - wrong).abs() > 1.0);

    // Still centred on the canvas — only the size should have changed.
    let (cx, cy) = ((min_x + max_x) * 0.5, (min_y + max_y) * 0.5);
    assert!((cx - w * 0.5).abs() < 0.01, "crosshair should stay x-centred");
    assert!((cy - h * 0.5).abs() < 0.01, "crosshair should stay y-centred");
}

/// The same shape one GUI-scale step up: vanilla's own scale-calculation picks 3
/// for a 1280x720 framebuffer at its automatic GUI-scale setting (height-bound:
/// `720/(3+1) < 240`, `720/(3+0) >= 240` at the previous step — see
/// `calculate_gui_scale`'s own doc), so every logical-pixel constant this
/// draw site uses should come out scaled by exactly 3, including the
/// crosshair's real 9px span becoming 27.
#[test]
fn crosshair_span_scales_with_gui_scale_not_just_at_the_floor() {
    let stats = DebugStats::default();
    let mut frame = HudFrame::new(&stats);
    frame.show_debug = false;
    let (w, h) = (1280.0_f32, 720.0_f32);
    assert_eq!(
        crate::config::calculate_gui_scale(crate::config::AUTO_GUI_SCALE, w as u32, h as u32),
        3,
        "precondition: this framebuffer must resolve to gui_scale 3"
    );
    let geo = HudGeometry::build(&frame, w as u32, h as u32);
    let (min_x, max_x, min_y, max_y) = ndc_bounds(&geo.verts, w, h);
    let (span_x, span_y) = (max_x - min_x, max_y - min_y);
    assert!(
        (span_x - 27.0).abs() < 0.05,
        "crosshair horizontal span should scale to 27px at gui_scale 3, got {span_x}"
    );
    assert!(
        (span_y - 27.0).abs() < 0.05,
        "crosshair vertical span should scale to 27px at gui_scale 3, got {span_y}"
    );
}

#[test]
fn hotbar_draws_and_selection_moves_the_highlight() {
    let stats = DebugStats::default();
    let mut frame = HudFrame::new(&stats);
    frame.crosshair = false;
    frame.show_debug = false;

    // No hotbar → no hotbar geometry.
    frame.hotbar = None;
    let none = HudGeometry::build(&frame, 640, 480).vertex_count();

    // A hotbar adds a real run of geometry (panel + 9 cells + a 4-edge ring).
    frame.hotbar = Some(0);
    let sel0 = HudGeometry::build(&frame, 640, 480);
    assert!(
        sel0.vertex_count() > none,
        "an on-screen hotbar must add geometry, got {} vs {none}",
        sel0.vertex_count()
    );

    // Moving the selection keeps the vertex *count* identical (same panel,
    // 9 cells, 4-edge ring) but must move the ring — so the bytes differ. A
    // selection that never relocates the highlight would render as a hotbar
    // that ignores the held slot.
    frame.hotbar = Some(4);
    let sel4 = HudGeometry::build(&frame, 640, 480);
    assert_eq!(
        sel0.vertex_count(),
        sel4.vertex_count(),
        "selecting a different slot must not change the vertex count"
    );
    assert_ne!(
        sel0.verts, sel4.verts,
        "the selection ring must move to the newly-selected slot"
    );
}
