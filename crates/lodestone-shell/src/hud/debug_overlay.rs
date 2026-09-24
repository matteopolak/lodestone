//! Row layout for the F3 debug overlay's text columns.
//!
//! ## What it is
//!
//! The F3 overlay used to place a line by measuring it and subtracting that
//! width from the canvas edge, which places a line correctly and says nothing
//! about whether it *fits*. It did not: `world_encode_submit`'s line carries a
//! bracketed sub-phase breakdown (four `world.*` timings plus the section
//! counts), and at a real GUI scale that single string measures several times
//! the logical canvas width — so most of it, including the `world.submit`
//! reading it exists to show, rendered past the right edge where nobody could
//! read it.
//!
//! Shortening one label would have fixed that one line and nothing else. This
//! module is the structural form instead: **every** row the overlay draws is
//! measured and broken to fit `canvas_w - 2 * margin` before it is positioned,
//! so a sub-phase somebody adds next month cannot reintroduce the defect.
//!
//! ## How it works
//!
//! [`fit_line`] is a greedy break with a preference order — a `", "` boundary
//! first (which is what puts one `world.*` sub-phase on each row, because that
//! is the separator the profiler joins them with), then any space, then a hard
//! character break so a single unbroken token cannot escape. Continuation rows
//! carry [`CONTINUATION_INDENT`] so a wrapped row reads as belonging to the one
//! above rather than as a new entry.
//!
//! [`layout_columns`] then flows the two columns into positioned
//! [`OverlayRow`]s. A blank input line consumes a row slot and emits nothing —
//! vanilla's own `""` group spacer, which draws as a gap rather than as an
//! empty plate.
//!
//! Everything here is in the **logical canvas** (`menu::render::logical_canvas`,
//! vanilla's `guiScaledWidth`), never device pixels, and every width comes from
//! [`measure`], which is the same function `Builder::text` advances by. A
//! restated constant would disagree the moment the vanilla font is or is not
//! loaded.
//!
//! ## How to change it
//!
//! Adding a line to the overlay needs nothing here — route it through
//! [`layout_columns`] like the rest and it is fitted automatically. Changing
//! the *anchor* rules is the risky edit: the guarantee callers rely on is that
//! for every returned row, `x - 1.0 >= 0.0` and `x + width + 1.0 <= canvas_w`
//! (the `±1` is the plate, which is two pixels wider than its text), and
//! `crates/lodestone-shell/tests/debug_overlay_lines_fit_the_canvas.rs` asserts
//! exactly that at several GUI scales with a control that fails when the fit is
//! bypassed.
//!
//! ## Dependencies
//!
//! `super::measure_text` for widths — nothing else. No GPU, no atlas, no jar.

use super::vanilla_font::VanillaFont;

/// The scale the overlay draws its text at.
///
/// `1.0`, because `HudGeometry::build_with_gui` already lays out in the
/// `gui_scale`-divided logical canvas and `DebugScreenOverlay` draws with no
/// pose scale of its own. Named here so the measure a caller passes to
/// [`layout_columns`] and the scale the draw hands `Builder::text` cannot
/// drift apart.
pub const DEBUG_SCALE: f32 = 1.0;

/// The hanging indent prefixed to every continuation row [`fit_line`] emits.
///
/// Two spaces: enough that a wrapped `world.*` sub-phase reads as a child of
/// the `world_encode_submit:` row above it, cheap enough that it costs almost
/// nothing of the width the wrap is trying to reclaim.
pub const CONTINUATION_INDENT: &str = "  ";

/// Which canvas edge a column anchors to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Anchor {
    /// Left-aligned at `margin`. Long content grows rightwards, so this is
    /// where a wide block (the frame profile) belongs.
    Left,
    /// Right-aligned at `canvas_w - margin - width`, vanilla's
    /// `guiWidth() - 2 - font.width(line)`.
    Right,
}

/// One overlay row, already broken to fit and positioned in the logical canvas.
///
/// `width` is carried rather than re-measured at the draw site so the plate and
/// the glyphs cannot resolve two different numbers from two calls to the same
/// measure.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayRow {
    pub text: String,
    pub x: f32,
    pub y: f32,
    pub width: f32,
}

/// The overlay's own text measure: exactly what `Builder::text` will advance
/// by, at the overlay's own [`DEBUG_SCALE`].
///
/// Exposed so a gate can drive the real layout with the real metrics instead of
/// restating a per-glyph advance — the restatement is what lets a gate pass
/// while the screen is wrong.
#[must_use]
pub fn measure(font: Option<&VanillaFont>, s: &str) -> f32 {
    super::measure_text(font, s, DEBUG_SCALE)
}

/// Break `text` into rows that each measure at most `max_width`.
///
/// Never returns an empty vector: an empty `text` yields one empty row, and a
/// non-positive `max_width` yields the input unbroken rather than looping
/// forever trying to shrink it.
///
/// The preference order is `", "`, then any space, then a hard character break.
/// The comma rule is what makes a profiler detail string read as a list — the
/// sub-phases are joined with `", "`, so breaking there puts one per row — and
/// it degrades to plain word wrap for any line that has no commas, which is
/// every other line on this screen.
#[must_use]
pub fn fit_line(text: &str, max_width: f32, measure: &dyn Fn(&str) -> f32) -> Vec<String> {
    if text.is_empty() {
        return vec![String::new()];
    }
    if max_width <= 0.0 || measure(text) <= max_width {
        return vec![text.to_string()];
    }
    let mut rows: Vec<String> = Vec::new();
    let mut rest = text;
    let mut indent = "";
    while !rest.is_empty() {
        let whole = format!("{indent}{rest}");
        if measure(&whole) <= max_width {
            rows.push(whole);
            break;
        }
        let cut = choose_cut(rest, indent, max_width, measure);
        let (head, tail) = rest.split_at(cut);
        rows.push(format!("{indent}{}", head.trim_end()));
        rest = tail.trim_start();
        indent = CONTINUATION_INDENT;
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

/// The byte index in `rest` to end this row at — always `>= 1`, so
/// [`fit_line`]'s loop cannot spin on a zero-length cut.
fn choose_cut(rest: &str, indent: &str, max_width: f32, measure: &dyn Fn(&str) -> f32) -> usize {
    let fits = |end: usize| {
        measure(&format!("{indent}{}", rest[..end].trim_end())) <= max_width
    };
    // Widths grow monotonically with `end`, so the first opportunity that does
    // not fit ends the search: everything after it is wider still.
    let mut last_space = None;
    let mut last_comma = None;
    for (i, ch) in rest.char_indices() {
        if ch != ' ' {
            continue;
        }
        let end = i + 1;
        if end >= rest.len() {
            break;
        }
        if !fits(end) {
            break;
        }
        last_space = Some(end);
        if i > 0 && rest.as_bytes()[i - 1] == b',' {
            last_comma = Some(end);
        }
    }
    if let Some(end) = last_comma {
        return end;
    }
    if let Some(end) = last_space {
        return end;
    }
    // No break opportunity fits: hard-break at the widest character boundary
    // that does, and failing that at the first one, so a single token wider
    // than the whole canvas still terminates.
    let mut last = 0usize;
    for (i, _) in rest.char_indices().skip(1) {
        if fits(i) {
            last = i;
        } else {
            break;
        }
    }
    if last > 0 {
        last
    } else {
        rest.char_indices()
            .nth(1)
            .map_or(rest.len(), |(i, _)| i)
    }
}

/// Flow two columns of overlay lines into positioned, already-fitting rows.
///
/// Both columns start at `y = margin` and advance by `line_h` per row. An empty
/// input line consumes a row slot and emits nothing (vanilla's `""` spacer);
/// every other line is [`fit_line`]d, and each piece takes its own slot — so a
/// wrapped line pushes the rest of *its* column down, exactly as an extra line
/// would.
///
/// The returned order is every left row, then every right row. Callers draw the
/// plates for all of them before any of the text, which is vanilla's own
/// two-pass order (`DebugScreenOverlay.extractLines`) and is what stops a later
/// line's plate covering an earlier line's glyphs.
///
/// **The guarantee**: for every row, `x >= margin` and
/// `x + width <= canvas_w - margin`, so the plate (`x - 1` to `x + width + 1`)
/// stays inside the canvas.
#[must_use]
pub fn layout_columns(
    canvas_w: f32,
    margin: f32,
    line_h: f32,
    left: &[String],
    right: &[String],
    measure: &dyn Fn(&str) -> f32,
) -> Vec<OverlayRow> {
    let max_width = (canvas_w - margin * 2.0).max(0.0);
    let mut out = Vec::new();
    for (anchor, lines) in [(Anchor::Left, left), (Anchor::Right, right)] {
        let mut row = 0usize;
        for line in lines {
            if line.is_empty() {
                row += 1;
                continue;
            }
            for piece in fit_line(line, max_width, measure) {
                let width = measure(&piece);
                let x = match anchor {
                    Anchor::Left => margin,
                    // `max(margin)` is belt-and-braces for a canvas so narrow
                    // that `max_width` collapsed to zero and `fit_line`
                    // returned the line unbroken.
                    Anchor::Right => (canvas_w - margin - width).max(margin),
                };
                out.push(OverlayRow {
                    text: piece,
                    x,
                    y: margin + row as f32 * line_h,
                    width,
                });
                row += 1;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A stand-in metric: one unit per character. Keeps the wrap *decision*
    /// testable without a jar, exactly as `wrap_legacy_with`'s own tests do.
    fn chars(s: &str) -> f32 {
        s.chars().count() as f32
    }

    /// The comma preference, at a width where it is **discriminating**: a plain
    /// greedy word wrap and the comma rule give different answers, so landing
    /// on the comma one is evidence rather than coincidence.
    ///
    /// At 50 units the last *space* that fits ends after `2.00` (49 chars) and
    /// the last *comma* that fits ends after `1.00 ms,` (41). A width where the
    /// two agree would pass either way — which is exactly what the earlier
    /// version of this test picked, and why it failed on a fixture whose first
    /// row had no comma boundary available at all.
    #[test]
    fn a_comma_separated_detail_breaks_one_item_to_a_row() {
        let line = "world_encode_submit: 1.00 ms [a: 1.00 ms, b: 2.00 ms, c: 3.00 ms]";
        let rows = fit_line(line, 50.0, &chars);
        assert_eq!(
            rows[0], "world_encode_submit: 1.00 ms [a: 1.00 ms,",
            "the first row must break at the comma, not at the later space a \
             greedy word wrap would have taken: {rows:#?}"
        );
        let greedy_word_wrap_hypothesis = "world_encode_submit: 1.00 ms [a: 1.00 ms, b: 2.00";
        assert_ne!(
            rows[0], greedy_word_wrap_hypothesis,
            "the two hypotheses must not coincide at this width or the test \
             measures nothing"
        );
        assert!(
            chars(greedy_word_wrap_hypothesis) <= 50.0,
            "the wrong hypothesis has to be one the fit would actually have \
             accepted, or preferring the comma proves nothing"
        );
        for row in &rows {
            assert!(chars(row) <= 50.0, "row {row:?} measures {} > 50", chars(row));
        }
        assert!(
            rows.iter().skip(1).all(|r| r.starts_with(CONTINUATION_INDENT)),
            "every continuation row carries the hanging indent: {rows:#?}"
        );
    }

    /// And the fallback: at a width too narrow for *any* comma boundary to fit
    /// on the first row, the break degrades to a plain space and every row
    /// still fits. The comma rule is a preference, not a requirement, and a
    /// line with no usable comma must not be left unbroken.
    #[test]
    fn a_width_too_narrow_for_a_comma_falls_back_to_a_space_and_still_fits() {
        let line = "world_encode_submit: 1.00 ms [a: 1.00 ms, b: 2.00 ms, c: 3.00 ms]";
        let rows = fit_line(line, 34.0, &chars);
        assert!(rows.len() > 1, "the line must still break: {rows:#?}");
        for row in &rows {
            assert!(chars(row) <= 34.0, "row {row:?} measures {} > 34", chars(row));
        }
    }

    #[test]
    fn a_single_token_wider_than_the_canvas_is_hard_broken() {
        let line = "x".repeat(200);
        let rows = fit_line(&line, 10.0, &chars);
        assert!(rows.len() > 1, "a 200-char token must break");
        for row in &rows {
            assert!(chars(row) <= 10.0, "row {row:?} escaped the width");
        }
        // Nothing may be lost to the break.
        let rejoined: String = rows
            .iter()
            .map(|r| r.trim_start_matches(CONTINUATION_INDENT))
            .collect();
        assert_eq!(rejoined, line, "a hard break must not drop characters");
    }

    #[test]
    fn a_line_that_already_fits_is_returned_whole() {
        assert_eq!(fit_line("short", 100.0, &chars), vec!["short".to_string()]);
        assert_eq!(fit_line("", 100.0, &chars), vec![String::new()]);
        // A degenerate width returns the input rather than looping.
        assert_eq!(fit_line("abc", 0.0, &chars), vec!["abc".to_string()]);
    }

    #[test]
    fn a_blank_input_line_consumes_a_row_slot_and_draws_nothing() {
        let left = vec!["a".to_string(), String::new(), "b".to_string()];
        let rows = layout_columns(100.0, 2.0, 9.0, &left, &[], &chars);
        assert_eq!(rows.len(), 2, "the spacer must not emit a row: {rows:#?}");
        assert_eq!(rows[0].y, 2.0);
        assert_eq!(rows[1].y, 2.0 + 2.0 * 9.0, "the spacer still costs a slot");
    }

    #[test]
    fn a_wrapped_line_pushes_the_rest_of_its_own_column_down() {
        let left = vec!["aaaaaaaaaaaaaaaaaaaa".to_string(), "b".to_string()];
        let rows = layout_columns(14.0, 2.0, 9.0, &left, &[], &chars);
        let b = rows.iter().find(|r| r.text == "b").expect("the second line");
        assert!(
            b.y > 2.0 + 9.0,
            "the first line wrapped, so `b` cannot still be on row 1: {rows:#?}"
        );
    }
}
