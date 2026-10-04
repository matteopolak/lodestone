use super::*;

/// Build only the F3 text/plate layer for the renderer's persistent buffer.
/// The public [`HudGeometry`] constructors keep that layer inline for their
/// pure geometry tests; the live renderer asks the main build to omit it and
/// draws this separately so unchanged vertices do not need to be regenerated
/// and uploaded on every presented frame.
pub(super) fn build_debug_vertices(
    frame: &HudFrame<'_>,
    width: u32,
    height: u32,
    gui_scale: u32,
    font: Option<&VanillaFont>,
) -> Vec<f32> {
    let (w, h) = crate::menu::render::logical_canvas(gui_scale, width, height);
    let mut b = Builder::new(w, h, None, None, None, font);
    draw_debug_overlay(&mut b, frame);
    b.verts
}

/// Emit the F3 overlay into whichever colour stream owns it: the inline pure
/// geometry path or the live renderer's refreshable cache.
pub(super) fn draw_debug_overlay(b: &mut Builder<'_>, frame: &HudFrame<'_>) {
    if !frame.show_debug {
        return;
    }
    let debug_scale = debug_overlay::DEBUG_SCALE;
    let debug_margin = DEBUG_MARGIN;
    let debug_line_h = DEBUG_LINE_H;
    let mut left = frame.stats.left_lines();
    let mut right = frame.stats.right_lines();
    // The four conditional diagnostics live on the frame rather than on
    // `DebugStats`, so they cannot be part of either column function. Each
    // opens with a spacer so it reads as its own `addToGroup` block.
    let mut left_group_open = false;
    let mut right_group_open = false;
    let open = |lines: &mut Vec<String>, opened: &mut bool| {
        if !*opened {
            lines.push(String::new());
            *opened = true;
        }
    };
    if let Some((recipes, tags)) = frame.recipe_stats {
        open(&mut right, &mut right_group_open);
        right.push(format!("Recipes: {recipes}, tags: {tags}"));
    }
    if let Some((dist, warn_at, strength)) = frame.border_debug {
        open(&mut right, &mut right_group_open);
        right.push(format!(
            "Border: {dist:.1} away, warns at {warn_at:.1} ({strength:.2})"
        ));
    }
    if let Some((count, explored)) = frame.map_debug {
        open(&mut right, &mut right_group_open);
        right.push(format!("Maps: {count}, {:.0}% explored", explored * 100.0));
    }
    if let Some(spawn) = frame.spawn_debug {
        open(&mut left, &mut left_group_open);
        left.push(format!("Spawn: {} {} {}", spawn.x, spawn.y, spawn.z));
    }
    // The frame-profile block flows below the left column and carries its own
    // leading spacer so it reads as a distinct group.
    left.extend(frame.stats.profile_lines());
    let rows = debug_overlay::layout_columns(
        b.w,
        debug_margin,
        debug_line_h,
        &left,
        &right,
        &|s: &str| measure_text(b.font, s, debug_scale),
    );
    // All plates precede all text, matching `DebugScreenOverlay.extractLines`.
    for row in &rows {
        b.rect_px(
            row.x - 1.0,
            row.y - 1.0,
            row.width + 2.0,
            debug_line_h,
            DEBUG_LINE_BG,
        );
    }
    for row in &rows {
        b.text(&row.text, row.x, row.y, debug_scale, DEBUG_LINE_INK);
    }
}

/// Wedge radius, in logical-canvas pixels — small enough to sit clear of the
/// F3 text columns at any window size, matching those columns' own `debug_scale
/// = 1.0` convention (this canvas is already gui-scale-divided; see
/// `HudGeometry::build_with_gui`'s own note on that).
pub(super) const PROFILER_CHART_RADIUS: f32 = 28.0;
/// Full-circle wedge substep count — the pie is subdivided at this angular
/// resolution regardless of how many slices there are, so one wide slice and
/// eight thin ones read equally round. 48 keeps a single-slice detail-view
/// circle visibly smooth without pushing meaningful vertex counts (at most
/// `PROFILER_CHART_STEPS` triangles per frame this draws at all).
pub(super) const PROFILER_CHART_STEPS: usize = 48;
/// Multiplier applied to a wedge triangle whose midpoint falls in the lower
/// half of the circle — vanilla's `Minecraft.renderFpsMeter` shades its own
/// pie's lower half darker for a pseudo-3D read; re-derived here as a flat
/// multiplier rather than transliterated, since vanilla's own shading is a
/// fixed second colour per section rather than one darkening factor.
pub(super) const PROFILER_CHART_LOWER_HALF_SHADE: f32 = 0.7;

/// One flat colour per root-level wedge, indexed by
/// `app::frame_profile::FramePhase`'s own position in `FramePhase::ALL` —
/// **not** stored on [`ProfilerChartSlice`] (see that type's doc): vanilla
/// assigns a section's pie colour by hashing its identity
/// (`ProfileResults.getPreferredColor` in spirit), so a fixed palette keyed by
/// position is this instrument's equivalent of "a stable colour per section
/// across frames" without needing a hash at all, since the eight phases are a
/// fixed, ordered set.
pub(super) const PROFILER_CHART_COLORS: [[f32; 4]; 8] = [
    [0.85, 0.25, 0.25, 0.92], // setup
    [0.90, 0.55, 0.15, 0.92], // sim_tick
    [0.90, 0.80, 0.20, 0.92], // mesh_upload
    [0.35, 0.75, 0.30, 0.92], // acquire
    [0.20, 0.70, 0.70, 0.92], // prepare
    [0.25, 0.50, 0.90, 0.92], // world_encode_submit
    [0.55, 0.35, 0.85, 0.92], // hud_ui_encode_submit
    [0.85, 0.35, 0.65, 0.92], // present
];
/// Neutral grey drawn instead of a wedge fan when the profiler's own total is
/// `0.0` — the ring buffers have not produced a sample yet (the first frame
/// or two of a session). Never divides by that zero to fabricate a slice.
pub(super) const PROFILER_CHART_EMPTY: [f32; 4] = [0.4, 0.4, 0.4, 0.5];

/// Draws the F3+Shift profiler pie chart: vanilla's shape
/// (`Minecraft.renderFpsMeter`) — a filled pie with a darker lower half and a
/// legend — re-derived rather than transliterated, plus this instrument's own
/// additions (GPU segments, skip counts) noted where they appear. See
/// `docs/frame-profiling.md`'s "Pie chart" section for the full picture and
/// [`DebugStats::profiler_chart`] for what feeds this.
///
/// Bottom-right corner, clear of the F3 text columns
/// (`HudGeometry::build_with_gui`'s own debug-line block occupies the top two
/// corners). `selected` (`ProfilerChart::selected`) switches between the root
/// 8-wedge pie plus a full legend, and a single-wedge detail view for one
/// phase plus its own mean/p95/p99/samples/skip readout — vanilla's own
/// number-key drill/`0`-back navigation, as an F3 chord
/// (`app::input::KeyOutcome::ProfilerChartSelect`).
pub(super) fn draw_profiler_chart(b: &mut Builder, chart: &ProfilerChart) {
    let r = PROFILER_CHART_RADIUS;
    let cx = b.w - DEBUG_MARGIN - r - 2.0;
    let cy = b.h - DEBUG_MARGIN - r - 2.0;

    // A slice's screen offset at angle `a` (0 = straight up, increasing
    // clockwise — vanilla's own pie starts at 12 o'clock and sweeps
    // clockwise too).
    let point = |a: f32| (cx + r * a.sin(), cy - r * a.cos());
    let shade = |c: [f32; 4], mid_angle: f32| {
        // Lower half is where the offset's y component is positive, i.e.
        // `cos(mid_angle) < 0` — see `PROFILER_CHART_LOWER_HALF_SHADE`'s doc.
        if mid_angle.cos() < 0.0 {
            [
                c[0] * PROFILER_CHART_LOWER_HALF_SHADE,
                c[1] * PROFILER_CHART_LOWER_HALF_SHADE,
                c[2] * PROFILER_CHART_LOWER_HALF_SHADE,
                c[3],
            ]
        } else {
            c
        }
    };
    let mut wedge = |a0: f32, a1: f32, c: [f32; 4]| {
        let span = a1 - a0;
        if span <= 0.0 {
            return;
        }
        let steps = ((span / std::f32::consts::TAU) * PROFILER_CHART_STEPS as f32)
            .ceil()
            .max(1.0) as usize;
        for i in 0..steps {
            let t0 = a0 + span * (i as f32 / steps as f32);
            let t1 = a0 + span * ((i + 1) as f32 / steps as f32);
            let colour = shade(c, (t0 + t1) * 0.5);
            b.colour().triangle((cx, cy), point(t0), point(t1), colour);
        }
    };

    let legend_right = cx - r - 8.0;
    let row = |b: &mut Builder, i: usize, swatch: [f32; 4], text: &str| {
        let y = cy - r + i as f32 * DEBUG_LINE_H;
        let tw = b.text_width(text, 1.0);
        // Right-aligned, but clamped so a long legend row cannot run off the
        // left edge — the same defect the text columns had, one draw site over.
        // The floor leaves room for the swatch, which is drawn a line-height to
        // the *left* of the text origin.
        let x = (legend_right - tw).max(DEBUG_MARGIN + DEBUG_LINE_H);
        b.rect_px(x - 1.0, y - 1.0, tw + 2.0, DEBUG_LINE_H, DEBUG_LINE_BG);
        b.rect_px(x - DEBUG_LINE_H, y, DEBUG_LINE_H - 2.0, DEBUG_LINE_H - 2.0, swatch);
        b.text(text, x, y, 1.0, DEBUG_LINE_INK);
    };

    match chart.selected {
        None => {
            let total = chart.total_mean_ms();
            if total <= 0.0 {
                b.colour().triangle(
                    (cx, cy),
                    point(0.0),
                    point(std::f32::consts::TAU * 0.9999),
                    PROFILER_CHART_EMPTY,
                );
            } else {
                let mut a = 0.0f32;
                for (i, slice) in chart.slices.iter().enumerate() {
                    let frac = (slice.mean_ms / total).max(0.0);
                    let span = frac * std::f32::consts::TAU;
                    wedge(a, a + span, PROFILER_CHART_COLORS[i % PROFILER_CHART_COLORS.len()]);
                    a += span;
                }
            }
            let mut i = 0usize;
            for (idx, slice) in chart.slices.iter().enumerate() {
                let pct = if total > 0.0 {
                    slice.mean_ms / total * 100.0
                } else {
                    0.0
                };
                let text = format!(
                    "[{}] {}: {pct:4.1}% {:.2} ms",
                    idx + 1,
                    slice.name,
                    slice.mean_ms
                );
                row(b, i, PROFILER_CHART_COLORS[idx % PROFILER_CHART_COLORS.len()], &text);
                i += 1;
            }
            if chart.gpu_unavailable {
                row(b, i, [0.5, 0.5, 0.5, 0.9], "gpu timing: unavailable");
                i += 1;
            } else {
                for &(name, ms) in &chart.gpu {
                    let text = match ms {
                        Some(ms) => format!("gpu {name}: {ms:.2} ms"),
                        None => format!("gpu {name}: <no reading yet>"),
                    };
                    row(b, i, [0.7, 0.7, 0.75, 0.9], &text);
                    i += 1;
                }
            }
            if chart.gpu_stalled_frames > 0 {
                row(
                    b,
                    i,
                    [0.9, 0.2, 0.2, 0.9],
                    &format!("gpu timer stalled_frames: {}", chart.gpu_stalled_frames),
                );
            }
        }
        Some(sel) if sel < chart.slices.len() => {
            let slice = chart.slices[sel];
            wedge(
                0.0,
                std::f32::consts::TAU * 0.9999,
                PROFILER_CHART_COLORS[sel % PROFILER_CHART_COLORS.len()],
            );
            let lines = [
                slice.name.to_string(),
                format!("mean {:.2} ms", slice.mean_ms),
                format!("p95  {:.2} ms", slice.p95_ms),
                format!("p99  {:.2} ms", slice.p99_ms),
                format!("samples {}/{}", slice.samples, slice.window),
                format!("skipped {}", slice.skipped),
                "[0] back to overview".to_string(),
            ];
            for (i, text) in lines.iter().enumerate() {
                row(
                    b,
                    i,
                    PROFILER_CHART_COLORS[sel % PROFILER_CHART_COLORS.len()],
                    text,
                );
            }
        }
        Some(_) => {
            // A stale selection past the current slice count (should not
            // happen — `slices.len()` is fixed at `PHASE_COUNT` — but drawing
            // nothing rather than panicking on an out-of-range index is the
            // honest degrade for a debug overlay.
        }
    }
}
