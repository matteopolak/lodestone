//! Hearts, armour and the creative/spectator vitals rules.

use super::*;

#[test]
fn health_pips_scale_with_value() {
    let stats = DebugStats::default();
    let mut frame = HudFrame::new(&stats);
    frame.crosshair = false;
    frame.show_debug = false;
    frame.health = Some(0.0);
    let empty = HudGeometry::build(&frame, 640, 480);
    frame.health = Some(20.0);
    let full = HudGeometry::build(&frame, 640, 480);
    // Ten pips are always drawn (lit or dark), so the *count* is identical —
    // the gauge width reads regardless of value. This guards that a zero-HP
    // frame still renders the empty slots rather than nothing (which would
    // read as "no HUD" instead of "no health").
    assert_eq!(
        empty.vertex_count(),
        full.vertex_count(),
        "ten pip quads regardless of value"
    );
    assert_eq!(empty.vertex_count(), 10 * 6, "10 pips × 6 verts each");
    // …but the *colours* must differ: a full bar's lit pips can't share the
    // empty bar's dark colour, or the gauge would never actually read HP.
    assert_ne!(
        empty.verts, full.verts,
        "full vs empty must recolour the pips, not just redraw them"
    );
}

#[test]
fn regeneration_effect_projection_arms_only_the_wave() {
    let ordinary = crate::effects::HudEffectIcon {
        icon: "mob_effect/speed".to_owned(),
        background: crate::effects::HUD_EFFECT_BACKGROUND_SPRITE,
        alpha: 1.0,
        beneficial: true,
    };
    let regeneration = crate::effects::HudEffectIcon {
        icon: "mob_effect/regeneration".to_owned(),
        ..ordinary.clone()
    };
    assert!(!regeneration_active(Some(&[ordinary])));
    assert!(regeneration_active(Some(&[regeneration])));
    assert!(!regeneration_active(None));
}

#[test]
fn max_health_adds_a_second_heart_row_without_inferring_it_from_current_health() {
    let stats = DebugStats::default();
    let mut frame = HudFrame::new(&stats);
    frame.crosshair = false;
    frame.show_debug = false;
    // A hurt Health-Boosted player is below 20 but still needs 20 heart
    // containers. This distinguishes attribute-driven row count from a
    // tempting `health > 20` branch.
    frame.health = Some(19.0);
    frame.max_health = Some(24.0);
    let boosted = HudGeometry::build(&frame, 640, 480);

    frame.max_health = Some(20.0);
    let ordinary = HudGeometry::build(&frame, 640, 480);
    assert_eq!(heart_rows(Some(24.0)), 2);
    assert_eq!(heart_rows(Some(20.0)), 1);
    assert_eq!(boosted.vertex_count() - ordinary.vertex_count(), 10 * 6);

    // Malformed data cannot create an arbitrary number of rows.
    assert_eq!(heart_rows(Some(f32::INFINITY)), 52);
}

/// The discriminating input for the two readings of "hide the hearts in
/// creative": vanilla's own is-survival check on its game-type enum is `SURVIVAL || ADVENTURE`, so **spectator**
/// is the value where the mode-naming hypothesis (`mode == Creative`) and the real
/// predicate disagree. Adventure is the second such value in the other direction.
#[test]
fn can_hurt_player_is_survival_and_not_creative_test() {
    use lodestone_model::GameMode;
    assert!(can_hurt_player(Some(GameMode::Survival)));
    // `isSurvival()` returns true for ADVENTURE too — an adventure-mode player is
    // hurtable and keeps the whole column.
    assert!(can_hurt_player(Some(GameMode::Adventure)));
    assert!(!can_hurt_player(Some(GameMode::Creative)));
    // The one that separates the hypotheses.
    assert!(!can_hurt_player(Some(GameMode::Spectator)));
    // Pre-connect / pre-login: the survival layout, matching `HudFrame::new`.
    assert!(can_hurt_player(None));
}

/// [`armour_icon`] against `Hud.extractArmor`'s three `if`s, at inputs where the
/// **wrong** reading gives a different answer.
///
/// The wrong reading is the one anybody would write from the screenshot rather
/// than the record: `full = ceil(armour / 2)` with a half only on an odd
/// remainder — or equivalently the off-by-one `i * 2 < armour`. It agrees with
/// the real predicate on **every even input**, so a gate at 8 or 20 measures that
/// the code runs. The discriminating inputs are odd, and every one below is
/// checked against both hypotheses: at 15 the truth is 7 full + 1 half + 2 empty
/// and the wrong reading says 8 full + 0 half + 2 empty.
///
/// Asserted as the full ten-icon **sequence**, not as counts, because counts
/// alone cannot see a half drawn at the wrong index — and mismatches are
/// collected rather than asserted inside the loop, so a neuter reports every arm
/// instead of the first.
#[test]
fn armour_icons_follow_extract_armor_at_odd_values() {
    use ArmourIcon::{Empty, Full, Half};
    // Hand-expanded from `if (i * 2 + 1 </==/> armor)`, one row per input.
    // `armour = 1` is the smallest drawn row; 30 is the registry's clamp ceiling
    // and must saturate at ten rather than grow.
    let cases: [(i32, [ArmourIcon; 10]); 6] = [
        (1, [Half, Empty, Empty, Empty, Empty, Empty, Empty, Empty, Empty, Empty]),
        (7, [Full, Full, Full, Half, Empty, Empty, Empty, Empty, Empty, Empty]),
        (15, [Full, Full, Full, Full, Full, Full, Full, Half, Empty, Empty]),
        (19, [Full, Full, Full, Full, Full, Full, Full, Full, Full, Half]),
        (20, [Full; 10]),
        (30, [Full; 10]),
    ];
    let mut mismatches: Vec<String> = Vec::new();
    for (armour, expected) in cases {
        let got: Vec<ArmourIcon> = (0..10).map(|i| armour_icon(i, armour)).collect();
        if got != expected {
            mismatches.push(format!("armour {armour}: expected {expected:?}, got {got:?}"));
        }
        // The wrong hypothesis, evaluated at the same input so the test records
        // that the two really do differ here rather than asserting they do.
        let wrong: Vec<ArmourIcon> = (0..10)
            .map(|i| {
                if (i as i32) * 2 < armour {
                    Full
                } else {
                    Empty
                }
            })
            .collect();
        if armour % 2 == 1 && wrong == expected {
            mismatches.push(format!(
                "armour {armour} is not a discriminating input: the ceil()/off-by-one \
                 reading gives the same ten icons, so this row measures only that \
                 the function runs"
            ));
        }
    }
    // Zero draws nothing at the call site (`armour > 0`), but the predicate itself
    // must still be total — an all-empty row, never a panic or a stray half.
    if (0..10).map(|i| armour_icon(i, 0)).any(|c| c != Empty) {
        mismatches.push("armour 0 must be ten empty icons".to_string());
    }
    assert!(
        mismatches.is_empty(),
        "armour icon selection diverges from Hud.extractArmor:\n  {}",
        mismatches.join("\n  ")
    );
}

/// [`heart_fill`] against `Hud.extractHearts`, at the **half** healths where the
/// `Mth.ceil` reading and the float reading give different sprites.
///
/// Live player report: *"sometimes i get to 0 hearts but im still alive - im
/// assuming vanilla maybe rounds up while we just round either way"*. He was
/// right, and 0.5 is the input that proves it — but it is not the only one, which
/// is why this drives four healths rather than that one. Vanilla ceils **up**, so
/// the divergence runs in both directions: 0.5 gains a half heart the float
/// reading never drew, while 1.5 and 19.5 promote a *half* to a **full**.
///
/// Every expectation below is hand-expanded from `currentHealth = Mth.ceil(health)`
/// and `halves < currentHealth` / `halves + 1 == currentHealth`, and the float
/// reading this replaced is evaluated at the same input so each row *records* that
/// the two really differ instead of asserting they do. The two integer healths are
/// deliberately included as the coincident controls: they must agree, and a gate
/// written at 1.0 or 20.0 alone would measure only that the function runs.
///
/// Asserted as the ten-sprite **sequence** and collected rather than asserted
/// inside the loop, so a neuter reports every arm instead of the first — a count
/// of filled hearts cannot see 19.5, where both readings draw ten sprites and only
/// the tenth one's identity differs.
#[test]
fn heart_fill_follows_extract_hearts_at_half_healths() {
    use HeartFill::{Full, Half};
    // `None` is a container with nothing over it. Rows hand-expanded from the
    // record, not from this module.
    let cases: [(f32, [Option<HeartFill>; 10]); 6] = [
        // Dead: the only health at which the bar is legitimately empty.
        (0.0, [None; 10]),
        // The report. `ceil(0.5) == 1`, so `halves + 1 == 1` at i = 0: a half.
        (0.5, [Some(Half), None, None, None, None, None, None, None, None, None]),
        // Coincident control — both readings say one half heart.
        (1.0, [Some(Half), None, None, None, None, None, None, None, None, None]),
        // `ceil(1.5) == 2`, so i = 0 is `halves + 1 == 1 != 2`: a **full** heart.
        (1.5, [Some(Full), None, None, None, None, None, None, None, None, None]),
        // The top of the bar. `ceil(19.5) == 20`: ten full, no half at i = 9.
        (19.5, [Some(Full); 10]),
        // Coincident control at the top.
        (20.0, [Some(Full); 10]),
    ];
    let mut mismatches: Vec<String> = Vec::new();
    for (health, expected) in cases {
        let got: Vec<Option<HeartFill>> = (0..10).map(|i| heart_fill(i, health)).collect();
        if got != expected {
            mismatches.push(format!("health {health}: expected {expected:?}, got {got:?}"));
        }
        // The reading this replaced, evaluated here so a row that stops
        // discriminating says so rather than passing quietly.
        let float_reading: Vec<Option<HeartFill>> = (0..10)
            .map(|i| {
                let units = health.max(0.0) - i as f32 * 2.0;
                if units >= 2.0 {
                    Some(Full)
                } else if units >= 1.0 {
                    Some(Half)
                } else {
                    None
                }
            })
            .collect();
        let half_health = (health.fract() - 0.5).abs() < 1e-6;
        if half_health && float_reading == expected {
            mismatches.push(format!(
                "health {health} is not a discriminating input: the float \
                 `health - 2i` reading gives the same ten sprites, so this row \
                 measures only that the function runs"
            ));
        }
        if !half_health && float_reading != expected {
            mismatches.push(format!(
                "health {health} was chosen as a coincident control but the two \
                 readings disagree ({float_reading:?} vs {expected:?}) — the \
                 control's premise is false"
            ));
        }
    }
    // A negative health (hurt overshoot) must read as dead, not ceil to a heart.
    if heart_fill(0, -0.5).is_some() {
        mismatches.push("a negative health must fill no heart".to_string());
    }
    assert!(
        mismatches.is_empty(),
        "heart fill diverges from Hud.extractHearts:\n  {}",
        mismatches.join("\n  ")
    );
}

/// The armour row is gated by the **same** `canHurtPlayer()` flag as the hearts,
/// hunger and bubble rows — vanilla reaches all four through one
/// `extractPlayerHealth` call — and by vanilla's own `armor > 0`, which draws
/// **no** row rather than ten empty icons.
///
/// Counts are predicted from `Builder::pips`' own shape, which
/// `health_pips_scale_with_value` independently pins at ten quads of six vertices:
/// so one armour row is `10 * 6 = 60` vertices on top of an otherwise identical
/// frame. The three arms that must be identical to the baseline are the ones a
/// direction-only assertion would miss — `Some(0)` in particular, where the
/// tempting "draw the empty backing anyway" reading adds 60 and vanilla adds 0.
#[test]
fn the_armour_row_costs_one_pip_row_and_only_when_worn() {
    let stats = DebugStats::default();
    let build = |can_hurt: bool, armour: Option<i32>| {
        let mut frame = HudFrame::new(&stats);
        frame.crosshair = false;
        frame.show_debug = false;
        frame.can_hurt_player = can_hurt;
        frame.health = Some(20.0);
        frame.food = Some(20);
        frame.armour = armour;
        HudGeometry::build(&frame, 640, 480).vertex_count()
    };
    let baseline = build(true, None);
    let mut mismatches: Vec<String> = Vec::new();
    // `None` (never wired), `Some(0)` (live, wearing nothing) and creative all
    // draw exactly the baseline; a worn value adds one row and nothing else.
    for (label, can_hurt, armour, expected) in [
        ("not wired", true, None, baseline),
        ("live, unarmoured", true, Some(0), baseline),
        ("full diamond", true, Some(20), baseline + 60),
        ("half icon at 15", true, Some(15), baseline + 60),
        ("creative, armoured", false, Some(20), build(false, None)),
    ] {
        let got = build(can_hurt, armour);
        if got != expected {
            mismatches.push(format!(
                "{label}: expected {expected} vertices, got {got} (baseline {baseline})"
            ));
        }
    }
    assert!(
        mismatches.is_empty(),
        "armour row wiring diverges:\n  {}",
        mismatches.join("\n  ")
    );
}

/// Vanilla's gate is `canHurtPlayer()` — `SURVIVAL || ADVENTURE` — and one call
/// to `extractPlayerHealth` behind it draws hearts, hunger and the bubble row,
/// while `hasExperience()` (the same body) gates the XP bar.
///
/// The counts here are predicted, not observed: on the procedural branch (no GUI
/// atlas attached) `Builder::pips` emits exactly ten quads per row at six
/// vertices each, which `health_pips_scale_with_value` above independently pins at
/// `10 * 6`. So a survival frame carrying both rows is `2 * 10 * 6` and a creative
/// frame is `0`. The wrong hypothesis — "gate on `GameMode::Creative`" — would
/// leave the spectator arm at 120, so the spectator case below is the one that
/// separates the two readings; a creative-only test passes under either.
#[test]
fn creative_and_spectator_hide_the_whole_vitals_column() {
    let stats = DebugStats::default();
    // `HudFrame` is not `Copy` (it carries owned strings), so each arm builds its
    // own rather than cloning one.
    let build = |can_hurt: bool, vitals: bool, hotbar: Option<usize>| {
        let mut frame = HudFrame::new(&stats);
        frame.crosshair = false;
        frame.show_debug = false;
        frame.can_hurt_player = can_hurt;
        frame.xp = Some((7, 0.5));
        frame.hotbar = hotbar;
        if vitals {
            frame.health = Some(20.0);
            frame.food = Some(20);
        }
        HudGeometry::build(&frame, 640, 480).vertex_count()
    };

    // Survival / adventure: two pip rows on top of whatever the XP bar itself
    // costs — asserted by *subtracting* an XP-only frame rather than by predicting
    // the bar's own vertex budget, which is not what this test is about.
    assert_eq!(
        build(true, true, None) - build(true, false, None),
        2 * 10 * 6,
        "ten health pips and ten hunger pips, six vertices each"
    );

    // `can_hurt_player == false` must take the XP bar and its level number with
    // it, not just the two pip rows — so the whole cluster is gone, not merely
    // shortened. With no hotbar, zero is the honest total.
    assert_eq!(
        build(false, true, None),
        0,
        "no hearts, no hunger, no XP bar when the player cannot be hurt"
    );

    // The control: a `0` above would also be what a frame drawing nothing at all
    // reports, so prove the hotbar — which vanilla draws in every game mode, and
    // which this gate must not touch — still lands.
    assert!(
        build(false, true, Some(0)) > 0,
        "the hotbar is not behind canHurtPlayer(); vanilla draws it in creative"
    );
}

/// `extractSelectedItemName` places the held-item label at `guiHeight - 59`, then
/// `y += 14` when `!canHurtPlayer()`. Predicted as an exact delta rather than
/// "it moved": at 480 px tall with GUI scale forced to 1 the logical canvas is the
/// physical one, so the two y values are `421` and `435`.
#[test]
fn the_held_item_label_drops_exactly_fourteen_pixels_in_creative() {
    let stats = DebugStats::default();
    let lowest_y = |can_hurt: bool| {
        let mut frame = HudFrame::new(&stats);
        frame.crosshair = false;
        frame.show_debug = false;
        frame.can_hurt_player = can_hurt;
        frame.held_item = Some(("Diamond Sword".to_string(), 1.0));
        HudGeometry::build(&frame, 640, 480)
            .verts
            .chunks(FLOATS_PER_VERTEX)
            .map(|v| v[1])
            .fold(f32::NEG_INFINITY, f32::max)
    };
    let survival = lowest_y(true);
    let creative = lowest_y(false);
    // `verts` are clip space, y **up**, so a label lower on screen has the smaller
    // value. The expected delta is derived from the same `logical_canvas` the draw
    // lays out in rather than from a hardcoded scale: 14 logical px over a canvas
    // `h` tall spans `2 * 14 / h` of the `-1..=1` range.
    let (_, canvas_h) =
        crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, 640, 480);
    let expected = 2.0 * 14.0 / canvas_h;
    assert!(
        (survival - creative - expected).abs() < 1e-4,
        "expected a 14px drop ({expected} in clip space), got {survival} -> {creative}"
    );
}
