use super::*;

/// Draw one recipe-unlock toast. Geometry and colours are cited on
/// [`RecipeToastView`]; this function is only the transcription.
///
/// The background prefers the real `toast/recipe` sprite and falls back to a
/// flat fill when no GUI atlas is attached — the same jar-less degradation
/// every other element in this module uses, and the reason a coverage gate can
/// run headless.
/// Vanilla's own subtitle-overlay layout constants, all in logical GUI pixels.
mod subtitle_layout {
    /// The row's text height; `halfHeight` is the integer half of it.
    pub(super) const ROW_H: f32 = 9.0;
    /// `guiHeight - 35` for row 0 — the bottom row's centre line.
    pub(super) const BOTTOM_INSET: f32 = 35.0;
    /// `row * (height + 1)`: rows stack upward 10px apart.
    pub(super) const ROW_STEP: f32 = ROW_H + 1.0;
    /// `guiWidth - halfWidth - 2`: the block's right edge sits 2px in.
    pub(super) const RIGHT_INSET: f32 = 2.0;
    /// The background plate's 1px bleed on every side.
    pub(super) const PLATE_PAD: f32 = 1.0;
}

/// Vanilla's sound-subtitle overlay: one right-aligned plate per live caption,
/// stacked upward from just above the hotbar, oldest at the bottom.
///
/// Ported from the subtitle overlay's extract render state
///. Two details are load-bearing:
///
/// * **Every row is the same width**, `max(text widths)` plus the width of
///   `"<"`, `">"` and two spaces — so the arrow columns exist on every plate,
///   and a row with no arrow does not shrink. Sizing each plate to its own text
///   makes a ragged stack that looks like a layout bug.
/// * **The text is centred inside that width**, not left-aligned — the plate is
///   right-aligned, its contents are not.
pub(super) fn draw_sound_subtitles(b: &mut Builder, captions: &[crate::audio::subtitles::SubtitleCaption]) {
    use crate::audio::subtitles::SubtitleArrow;
    use subtitle_layout::{BOTTOM_INSET, PLATE_PAD, RIGHT_INSET, ROW_H, ROW_STEP};

    // Vanilla draws at a fixed 1.0 scale under the GUI transform; `b`'s canvas is
    // already the logical (gui-scaled) one, so "1 logical pixel" here is exactly
    // vanilla's own unit and no extra factor belongs in this function.
    let scale = 1.0;
    let arrow_w = b.text_width("<", scale) + b.text_width(">", scale) + b.text_width("  ", scale);
    let width = captions
        .iter()
        .map(|c| b.text_width(&c.text, scale))
        .fold(0.0f32, f32::max)
        + arrow_w;
    let half_w = (width / 2.0).floor();
    let half_h = (ROW_H / 2.0).floor();
    let cx = b.w - half_w - RIGHT_INSET;

    for (row, caption) in captions.iter().enumerate() {
        let cy = b.h - BOTTOM_INSET - row as f32 * ROW_STEP;
        // get background color: black at 80%, the non-chat text plate.
        b.rect_px(
            cx - half_w - PLATE_PAD,
            cy - half_h - PLATE_PAD,
            half_w * 2.0 + PLATE_PAD * 2.0,
            ROW_H + PLATE_PAD * 2.0,
            [0.0, 0.0, 0.0, 0.8],
        );
        // Brightness fades, alpha does not — see `audio::subtitles`' module doc.
        let ink = [
            caption.brightness,
            caption.brightness,
            caption.brightness,
            1.0,
        ];
        let text_y = cy - half_h;
        match caption.arrow {
            Some(SubtitleArrow::Right) => {
                let w = b.text_width(">", scale);
                b.text(">", cx + half_w - w, text_y, scale, ink);
            }
            Some(SubtitleArrow::Left) => {
                b.text("<", cx - half_w, text_y, scale, ink);
            }
            None => {}
        }
        let tw = b.text_width(&caption.text, scale);
        b.text(&caption.text, cx - (tw / 2.0).floor(), text_y, scale, ink);
    }
}
