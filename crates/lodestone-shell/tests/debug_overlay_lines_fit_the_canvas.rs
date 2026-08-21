//! Every row the F3 debug overlay draws stays inside the logical canvas.
//!
//! The owner's report: *"the line that contains `world.submit` is way too long
//! and goes off the screen so i cant see it"*. The frame profiler formats
//! `world_encode_submit` as a base timing plus a bracketed breakdown of four
//! `world.*` sub-phases and the section counts, and that single string is
//! several times the logical canvas width at any real GUI scale. Right-aligning
//! it — which is what the overlay did, faithfully porting
//! `DebugScreenOverlay.extractLines` — places the line correctly and says
//! nothing at all about whether it fits.
//!
//! Shortening one label would fix one line. This gate asserts the structural
//! property instead: **for every row, at every GUI scale, the plate stays
//! inside the canvas**, so a sub-phase added later cannot reintroduce the bug.
//!
//! ## Which half this verifies
//!
//! The left and right columns come from the **real producers**
//! (`DebugStats::left_lines` / `right_lines` / `profile_lines`) and the widths
//! from the **real measure** (`hud::debug_overlay::measure`, the same function
//! `Builder::text` advances by), so this is not a gate that invented its own
//! layout input.
//!
//! The one thing it *does* transcribe is the profiler's own string. That lives
//! in `app::frame_profile::PhaseSummary::line`, which is `pub(crate)` and so
//! unreachable from an integration test; [`WORLD_LINE`] below is a copy of its
//! shape, and if the profiler's format changes this gate keeps passing against
//! the old one. It would still catch the defect it exists for — the assertion
//! is about width, not about content — but it is worth stating plainly: this
//! verifies the *overlay's* fit, not the *profiler's* formatting.
//!
//! ## The controls
//!
//! Two, because an "everything fits" assertion is an assertion of an absence
//! and needs evidence the detector could have fired:
//!
//! - [`the_unwrapped_profile_line_really_does_overflow`] measures the raw input
//!   and requires it to be *too wide*. Without it, a gate over a corpus of
//!   short lines passes vacuously and reports nothing about wrapping.
//! - [`a_deliberately_overlong_line_is_still_fitted`] adds a line far wider
//!   than any real one and requires the layout to absorb it — the same shape as
//!   the future sub-phase this gate is defending against.

use lodestone::hud::debug_overlay::{self, OverlayRow};
use lodestone::hud::{DEBUG_LINE_H, DEBUG_MARGIN, DebugStats};
use lodestone::menu::render::logical_canvas;

/// A transcription of what `app::frame_profile::PhaseSummary::line` produces
/// for `world_encode_submit` once every sub-phase window has a reading — the
/// line the owner could not read. See the module doc for why this is copied
/// rather than called.
const WORLD_LINE: &str = "world_encode_submit: 4.71/8.02/9.63 ms (240/240, 0 skip) \
[world.prepare_buffers: 0.42/0.71/0.88 ms, world.terrain_cull_draw: 3.05/5.90/7.11 ms, \
world.other_draws: 0.71/1.02/1.30 ms, world.submit: 0.53/0.79/0.94 ms, \
sections visited: 1284 packed + 96 model]";

/// The GUI scales a player can actually select, plus `1` — the overlay must fit
/// at every one of them, and the narrow ones are where it fails first, because
/// the logical canvas shrinks while the font does not.
const GUI_SCALES: [u32; 4] = [1, 2, 3, 4];

/// The scales at which [`WORLD_LINE`] genuinely does not fit [`FRAMEBUFFER`].
///
/// **Not scale 1**, and that is the useful part rather than an inconvenience:
/// at scale 1 the logical canvas *is* the framebuffer, 2560 px, and the line
/// measures 1536, so it fits with room to spare. The defect is scale-dependent
/// — which is why "the `world.submit` line runs off the screen" and "the
/// overlay looks fine" can both be honest reports from two players, and why a
/// gate that only ever measured one scale would have proved whichever of them
/// it happened to pick.
const OVERFLOWING_SCALES: [u32; 3] = [2, 3, 4];

/// A framebuffer wide enough that scale 1 is not itself the interesting case.
const FRAMEBUFFER: (u32, u32) = (2560, 1440);

/// A stats record with every optional block populated, so no column is
/// accidentally empty: a gate whose right column had no adapter group would be
/// measuring half the screen.
fn realistic_stats() -> DebugStats {
    DebugStats {
        position: [1234.567, 71.937_5, -8765.432],
        yaw: 137.5,
        pitch: -22.25,
        fps: 143.0,
        frame_ms: 6.31,
        chunk_count: 1024,
        live_columns: 961,
        mesh_drops: 0,
        section_count: 412,
        quads: 1_204_996,
        vram_bytes: 67_108_864,
        vram_reserved_bytes: 75_497_472,
        rss_bytes: 2_147_483_648,
        world_bytes: 402_653_184,
        frames_per_tick: 0.42,
        target: Some([1234, 70, -8766]),
        entities_drawn: 37,
        occlusion_graph_sections: 1911,
        sections_culled_occlusion: 132,
        sections_occlusion_shadow: 88,
        occlusion_active: true,
        occlusion_walks: 4127,
        particles_alive: 512,
        particles_drawn: 498,
        particles_unresolved: 0,
        weather_columns: 121,
        weather_rain_columns: 121,
        status: "local world".into(),
        difficulty: Some((lodestone_model::Difficulty::Normal, false)),
        light: Some((15, 4)),
        adapter: vec![
            "Apple M5 (IntegratedGpu)".into(),
            "Metal, driver: Metal 4.0".into(),
            "max_bind_groups: 8, max_texture_dimension_2d: 16384".into(),
        ],
        dimension: Some("minecraft:overworld".into()),
        hitboxes_shown: false,
        chunk_borders_shown: true,
        frame_profile: vec![
            "setup: 0.12/0.30/0.41 ms (240/240, 0 skip)".into(),
            "sim_tick: 1.84/3.10/4.02 ms (240/240, 0 skip)".into(),
            "mesh_upload: 0.66/2.41/3.90 ms (240/240, 0 skip)".into(),
            "acquire: 0.09/0.22/0.31 ms (240/240, 0 skip)".into(),
            "prepare: 0.98/1.55/1.91 ms (240/240, 0 skip)".into(),
            WORLD_LINE.into(),
            "hud_ui_encode_submit: 0.77/1.20/1.44 ms (240/240, 0 skip)".into(),
            "present: 0.31/0.55/0.72 ms (240/240, 0 skip)".into(),
            "gpu terrain: 3.90 ms".into(),
            "gpu entities: 0.62 ms".into(),
            "gpu hud: 0.21 ms".into(),
        ],
        profiler_chart: None,
    }
}

/// The three real producers, in the order the draw flows them: the left column
/// with the profile block appended, and the right column.
fn columns(stats: &DebugStats) -> (Vec<String>, Vec<String>) {
    let mut left = stats.left_lines();
    left.extend(stats.profile_lines());
    (left, stats.right_lines())
}

/// Run the real layout at one GUI scale, with the real measure and no jar
/// attached (the fixed-advance debug font — see `hud::measure_text`).
fn rows_at(gui_scale: u32, left: &[String], right: &[String]) -> (f32, Vec<OverlayRow>) {
    let (w, _h) = logical_canvas(gui_scale, FRAMEBUFFER.0, FRAMEBUFFER.1);
    let rows = debug_overlay::layout_columns(
        w,
        DEBUG_MARGIN,
        DEBUG_LINE_H,
        left,
        right,
        &|s: &str| debug_overlay::measure(None, s),
    );
    (w, rows)
}

/// Every row's **plate** — `x - 1` to `x + width + 1`, which is the rectangle
/// the draw actually fills — inside `0 ..= canvas_w`, at every GUI scale.
///
/// Failures are collected rather than asserted inside the loop, so one bad row
/// cannot hide the rest: a gate that aborts on the first mismatch proves one
/// arm and leaves the others as arguments.
#[test]
fn every_overlay_row_fits_the_canvas_at_every_gui_scale() {
    let stats = realistic_stats();
    let (left, right) = columns(&stats);
    let mut overflows: Vec<String> = Vec::new();
    let mut checked = 0usize;

    for scale in GUI_SCALES {
        let (w, rows) = rows_at(scale, &left, &right);
        assert!(
            !rows.is_empty(),
            "scale {scale} produced no rows at all — the layout ran on nothing, \
             which would make every assertion below vacuous"
        );
        for row in &rows {
            checked += 1;
            let plate_left = row.x - 1.0;
            let plate_right = row.x + row.width + 1.0;
            if plate_left < 0.0 || plate_right > w {
                overflows.push(format!(
                    "scale {scale} (canvas {w}): plate [{plate_left}, {plate_right}] \
                     escapes for {:?}",
                    row.text
                ));
            }
        }
    }

    assert!(
        overflows.is_empty(),
        "{} of {checked} rows ran off the canvas:\n{}",
        overflows.len(),
        overflows.join("\n")
    );
}

/// The detector control: the raw `world_encode_submit` line really is wider
/// than the canvas it has to fit, at every scale under test.
///
/// Without this, the test above is satisfied by a corpus of short lines and
/// says nothing about wrapping — the vacuous-by-input species. It is also the
/// direct reproduction of the report: this is the measurement that says the
/// line the owner could not read was genuinely too wide, not merely long.
#[test]
fn the_unwrapped_profile_line_really_does_overflow() {
    let raw = debug_overlay::measure(None, WORLD_LINE);
    for scale in OVERFLOWING_SCALES {
        let (w, _h) = logical_canvas(scale, FRAMEBUFFER.0, FRAMEBUFFER.1);
        let budget = w - DEBUG_MARGIN * 2.0;
        assert!(
            raw > budget,
            "at gui scale {scale} the canvas is {w} wide (budget {budget}) and the \
             raw line measures {raw} — if this ever stops overflowing, pick a \
             narrower canvas or a longer fixture, because the fit test has \
             stopped testing the fit"
        );
    }

    // The other side of the same measurement, asserted so that a future
    // fixture change cannot quietly move this line into the fitting case at
    // every scale and leave the wrap untested: see `OVERFLOWING_SCALES`.
    let (w1, _h) = logical_canvas(1, FRAMEBUFFER.0, FRAMEBUFFER.1);
    assert!(
        raw < w1 - DEBUG_MARGIN * 2.0,
        "scale 1 on a {}px framebuffer is expected to be the case that fits \
         ({raw} vs {w1}); if the fixture grew past it, update the comment on \
         OVERFLOWING_SCALES rather than deleting this assertion",
        FRAMEBUFFER.0
    );

    // And the layout really does break it into more rows than it was given —
    // the positive half of the same claim.
    let stats = realistic_stats();
    let (left, right) = columns(&stats);
    let input_rows = left.iter().chain(right.iter()).filter(|l| !l.is_empty()).count();
    let (_w, rows) = rows_at(3, &left, &right);
    assert!(
        rows.len() > input_rows,
        "the layout emitted {} rows for {input_rows} non-blank input lines — \
         nothing wrapped, so nothing was fitted",
        rows.len()
    );
}

/// The future-sub-phase control: a line far wider than any the profiler emits
/// today is still absorbed, in **both** columns.
///
/// Both, because the two anchors are different arithmetic: the left column
/// pins `x` and lets the width run, the right column derives `x` *from* the
/// width. A fit rule that covered only one of them would be exactly half a
/// guard, and the right column is the one the profile lines used to live in.
#[test]
fn a_deliberately_overlong_line_is_still_fitted() {
    let monster: String = (0..40)
        .map(|i| format!("world.some_future_subphase_{i}: 1.23/4.56/7.89 ms"))
        .collect::<Vec<_>>()
        .join(", ");
    // A single unbroken token as well: word wrap alone cannot place this one,
    // so it exercises the hard-break fallback rather than the comma rule.
    let unbroken = "x".repeat(4096);

    for (name, line) in [("comma-separated", &monster), ("unbroken", &unbroken)] {
        let raw = debug_overlay::measure(None, line);
        for scale in GUI_SCALES {
            let (w, _h) = logical_canvas(scale, FRAMEBUFFER.0, FRAMEBUFFER.1);
            assert!(
                raw > w,
                "the {name} control must be wider than the scale-{scale} canvas \
                 ({raw} vs {w}) or it controls for nothing"
            );
            for (side, left, right) in [
                ("left", vec![line.clone()], Vec::new()),
                ("right", Vec::new(), vec![line.clone()]),
            ] {
                let rows = debug_overlay::layout_columns(
                    w,
                    DEBUG_MARGIN,
                    DEBUG_LINE_H,
                    &left,
                    &right,
                    &|s: &str| debug_overlay::measure(None, s),
                );
                let escaped: Vec<&OverlayRow> = rows
                    .iter()
                    .filter(|r| r.x - 1.0 < 0.0 || r.x + r.width + 1.0 > w)
                    .collect();
                assert!(
                    escaped.is_empty(),
                    "{name} in the {side} column at scale {scale}: {} of {} rows \
                     escaped the canvas",
                    escaped.len(),
                    rows.len()
                );
            }
        }
    }
}
