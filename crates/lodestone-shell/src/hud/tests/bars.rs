//! Boss bar and locator bar geometry against synthetic atlases.

use super::*;

/// The boss bar's four vanilla clauses (`BossHealthOverlay.extractBar`,
/// `.cache/mc/26.2/client-src`), each pinned to the layer that actually
/// emits geometry (`HudGeometry::sprite_verts`, via a real
/// [`GuiAtlas`]) rather than to [`crate::overlay::BossBarView`]'s model —
/// the gap this whole fix closes was that every existing gate stopped one
/// layer above this and a flat `rect_px` reached the screen instead.
///
/// 1. background plate, full 182px, drawn even at zero progress
/// 2. background notch overlay, full 182px, only when the overlay style
///    is not `Progress`
/// 3. progress fill, **cropped** (not scaled) to
///    [`crate::overlay::lerp_discrete_width`] — checked at a half-full
///    bar (the brief's own discriminating case) and at `0.2`, which the
///    naive `round(progress * 182)` hypothesis gets wrong (36px, not 37)
/// 4. progress notch overlay, cropped the same way, only when the
///    overlay style is not `Progress`
#[test]
fn boss_bar_reaches_the_sprite_geometry_layer_not_just_the_model() {
    use crate::overlay::{BossBarView, lerp_discrete_width};
    use lodestone_game::bossbar::{BossBarColor, BossBarOverlay};

    let atlas = boss_bar_synthetic_atlas();
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    let bar_x = cw * 0.5 - BOSS_BAR_WIDTH * 0.5;
    let yo = BOSS_BAR_TOP;

    let render = |progress: f32, overlay: BossBarOverlay| -> Vec<(f32, f32, f32, f32)> {
        let bars = [BossBarView {
            title: crate::overlay::plain_spans("Ender Dragon"),
            progress,
            color: BossBarColor::Purple,
            overlay,
        }];
        let frame = HudFrame {
            boss_bars: &bars,
            crosshair: false,
            show_debug: false,
            ..HudFrame::new(&stats)
        };
        let geo = HudGeometry::build_with_gui(&frame, w, h, &atlas);
        quad_boxes(&geo.sprite_verts, cw, ch)
    };

    let mut wrong = Vec::new();
    let mut check = |name: String, got: f32, want: f32| {
        if (got - want).abs() > 0.5 {
            wrong.push(format!("{name}: got {got:.2}, want {want:.2}"));
        }
    };

    // -- clause 1: background, full width, drawn even at zero progress,
    // and clauses 3/4 correctly absent (no fill, overlay is Progress).
    let empty = render(0.0, BossBarOverlay::Progress);
    assert_eq!(
        empty.len(),
        1,
        "progress 0.0 with the Progress overlay must draw only the \
         background plate: got {empty:?}"
    );
    check("empty bg x0".into(), empty[0].0, bar_x);
    check("empty bg x1".into(), empty[0].2, bar_x + BOSS_BAR_WIDTH);
    check("empty bg y0".into(), empty[0].1, yo);
    check("empty bg y1".into(), empty[0].3, yo + BOSS_BAR_HEIGHT);

    // -- clause 3 at full progress: the fill spans the whole 182px too.
    let full = render(1.0, BossBarOverlay::Progress);
    assert_eq!(full.len(), 2, "full progress must draw background + fill: got {full:?}");
    check("full bg x0".into(), full[0].0, bar_x);
    check("full bg x1".into(), full[0].2, bar_x + BOSS_BAR_WIDTH);
    check("full fill x0".into(), full[1].0, bar_x);
    check("full fill x1".into(), full[1].2, bar_x + BOSS_BAR_WIDTH);

    // -- clause 3, the discriminating cases: a half-full bar's fill must
    // cover the *predicted partial* width — not zero, not full, and (at
    // 0.2) not the naive `progress * 182` scale either.
    for (progress, want_px) in [(0.5_f32, 91_i32), (0.2_f32, 37_i32)] {
        let quads = render(progress, BossBarOverlay::Progress);
        assert_eq!(
            quads.len(),
            2,
            "progress {progress} must draw background + fill: got {quads:?}"
        );
        let predicted = lerp_discrete_width(progress, BOSS_BAR_WIDTH as i32);
        assert_eq!(predicted, want_px, "lerp_discrete_width regressed for {progress}");
        let fill = quads[1];
        check(format!("progress {progress} fill x0"), fill.0, bar_x);
        check(format!("progress {progress} fill x1"), fill.2, bar_x + want_px as f32);
        check(format!("progress {progress} fill y0"), fill.1, yo);
        check(format!("progress {progress} fill y1"), fill.3, yo + BOSS_BAR_HEIGHT);
    }

    // -- clauses 2 + 4: the notch overlay draws only when the overlay
    // style is not `Progress`, doubling the quad count at both ends.
    let notched_empty = render(0.0, BossBarOverlay::Notched6);
    assert_eq!(
        notched_empty.len(),
        2,
        "zero progress with a notch overlay must draw background + \
         background-notch, no fill: got {notched_empty:?}"
    );
    let notched_full = render(1.0, BossBarOverlay::Notched6);
    assert_eq!(
        notched_full.len(),
        4,
        "full progress with a notch overlay must draw all four clauses: \
         got {notched_full:?}"
    );

    assert!(wrong.is_empty(), "{wrong:?}");
}

/// The locator bar's dots reach the sprite-geometry layer at their
/// predicted screen positions — the same class of gap
/// [`boss_bar_reaches_the_sprite_geometry_layer_not_just_the_model`]
/// closed for the boss bar, and the one `CLAUDE.md`'s `draw_tab`
/// incident names generally: a corpus that only ever asserts on the
/// *model* (here, [`locator::LocatorDot`]) cannot see a draw site that
/// never calls `b.sprite` at all. This one calls `HudGeometry::build_with_gui`
/// and reads the actual emitted quads.
///
/// Also the mutual-exclusion contract `sprite_vitals`'s own doc names:
/// a non-empty `locator` must draw *instead of* the XP bar, not
/// alongside it, even when `frame.xp` is `Some` — `ContextualBar` is one
/// slot in vanilla, never two bars stacked.
#[test]
fn locator_bar_reaches_the_sprite_geometry_layer_and_outranks_the_xp_bar() {
    let atlas = locator_bar_synthetic_atlas();
    let stats = DebugStats::default();
    let (w, h) = (640u32, 480u32);
    let (cw, ch) = crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
    // `ContextualBar::left`/`top`, the same expressions `sprite_vitals`
    // derives `hx`/`bar_top` from — not restated as an independent
    // constant, so a change to the hotbar's own layout moves this
    // prediction with it instead of silently drifting from it.
    let bar_x = (cw - 182.0) / 2.0;
    let bar_y = ch - 22.0 - 5.0 - 2.0;
    let dot = locator::dot_size() as f32;
    let screen_middle = ((cw - dot) / 2.0).ceil();

    let render = |dots: &[locator::LocatorDot], xp: Option<(i32, f32)>| -> Vec<(f32, f32, f32, f32)> {
        let frame = HudFrame {
            crosshair: false,
            show_debug: false,
            can_hurt_player: true,
            xp,
            locator: dots,
            ..HudFrame::new(&stats)
        };
        let geo = HudGeometry::build_with_gui(&frame, w, h, &atlas);
        quad_boxes(&geo.sprite_verts, cw, ch)
    };

    // -- No waypoints, XP present: only the XP bar draws (the pre-existing
    //    behaviour every XP gate elsewhere in this file already covers;
    //    this call is the mutual-exclusion control, not new coverage).
    let xp_only = render(&[], Some((3, 0.4)));
    assert!(
        !xp_only.is_empty(),
        "control: the XP bar must still draw when there are no waypoints"
    );

    // -- Two waypoints, no XP: exactly background + two dots, at the
    //    predicted screen rects.
    let dots = [
        locator::LocatorDot { offset: -40, color: [1.0, 0.0, 0.0, 1.0] },
        locator::LocatorDot { offset: 25, color: [0.0, 1.0, 0.0, 1.0] },
    ];
    let quads = render(&dots, None);
    assert_eq!(
        quads.len(),
        3,
        "two waypoints must draw background + two dots, no more, no fewer: got {quads:?}"
    );
    let check = |name: &str, got: f32, want: f32| {
        assert!(
            (got - want).abs() < 0.5,
            "{name}: got {got:.2}, want {want:.2} (all quads: {quads:?})"
        );
    };
    check("background x0", quads[0].0, bar_x);
    check("background x1", quads[0].2, bar_x + 182.0);
    check("background y0", quads[0].1, bar_y);
    check("background y1", quads[0].3, bar_y + 5.0);
    check("dot[0] (-40) x0", quads[1].0, screen_middle - 40.0);
    check("dot[0] (-40) x1", quads[1].2, screen_middle - 40.0 + dot);
    check("dot[1] (+25) x0", quads[2].0, screen_middle + 25.0);
    check("dot[1] (+25) x1", quads[2].2, screen_middle + 25.0 + dot);

    // -- Both present: locator wins outright — this is the assertion
    //    that fails if the mutual-exclusion `if`/`else if` in
    //    `sprite_vitals` is ever loosened back into two independent
    //    conditions, which would draw both bars stacked on one slot.
    let both = render(&dots, Some((3, 0.4)));
    assert_eq!(
        both.len(),
        3,
        "with waypoints present the XP bar must not draw at all, even though frame.xp is \
         Some: got {both:?}"
    );
}
