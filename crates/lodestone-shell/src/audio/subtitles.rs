//! Sound-subtitle captions (issue #198): vanilla's accessibility overlay that
//! names each sound as it plays, bottom-right, with a `<`/`>` arrow when the
//! source is behind you.
//!
//! # What it is
//!
//! A port of `SubtitleOverlay` (`SubtitleOverlay.java`). One [`SubtitleQueue`]
//! lives on [`crate::audio::ShellAudio`] — the single choke point every sound in
//! the client passes through — so a caption can never disagree with what is
//! actually audible. `views` turns the queue plus a listener transform into the
//! drawable [`SubtitleCaption`] rows the HUD renders.
//!
//! # Two things vanilla does that are easy to get backwards
//!
//! **It fades brightness, not alpha.** `SubtitleOverlay.java` lerps the text's
//! RGB from `255` down to `75` over the display window and leaves alpha at `255`
//! throughout, while the background stays at a constant opacity. Fading alpha
//! instead makes an old caption translucent over the world rather than grey on its
//! own plate, which reads as a different effect entirely.
//!
//! **A repeat refreshes an existing row rather than adding one.** Captions are
//! keyed on their *text*, and a second `block.stone.break` updates that row's
//! timestamp and position list instead of stacking a duplicate. Otherwise walking
//! on grass produces a wall of identical lines.
//!
//! # The one deliberate simplification
//!
//! Vanilla also drops a caption whose sound is further away than that sound's own
//! attenuation range (`Subtitle.isAudibleFrom`). We do not carry the resolved
//! range out to the caption hook — `ShellAudio::play_sound` knows the event name
//! and position, not the entry's `attenuation_distance` — so every caption here is
//! treated as audible. In practice the two agree: the sound was actually submitted
//! to the mixer, which is a stronger audibility signal than a range check, and the
//! 3-second window expires it anyway.

use glam::Vec3;

/// `SubtitleOverlay.DISPLAY_TIME` (`SubtitleOverlay.java:23`), in milliseconds.
/// Vanilla multiplies this by the `notificationDisplayTime` option, which this
/// client does not model; at its default of `1.0` the product is this constant.
pub(crate) const DISPLAY_MS: u64 = 3000;

/// The brightness a caption's text starts at — `255`, i.e. white.
const BRIGHTNESS_NEW: f32 = 255.0;

/// The brightness it decays to by the end of the window — `75`, a mid grey.
/// **Not zero**: an expiring caption goes grey and then vanishes, it does not
/// fade out.
const BRIGHTNESS_OLD: f32 = 75.0;

/// `forwards.dot(delta) > 0.5` — the cone within which vanilla considers the
/// sound to be *in view* and draws no arrow (`SubtitleOverlay.java:86`).
const IN_VIEW_DOT: f64 = 0.5;

/// One recorded play of a caption's sound: where, and when.
#[derive(Debug, Clone, Copy, PartialEq)]
struct PlayedAt {
    pos: Vec3,
    at_ms: u64,
}

/// One caption text plus every recent position its sound played at.
///
/// The position *list* rather than one position is vanilla's own shape
/// (`Subtitle.playedAt`): the arrow points at whichever instance is **closest to
/// the listener**, so a footstep on both sides of you points at the near one.
#[derive(Debug, Clone)]
struct Subtitle {
    text: String,
    played_at: Vec<PlayedAt>,
}

/// Which side an off-screen sound came from, when vanilla draws an arrow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubtitleArrow {
    /// `<` — to the listener's left.
    Left,
    /// `>` — to the listener's right.
    Right,
}

/// One drawable caption row. The returned slice is **oldest first**, which is
/// vanilla's own draw order with row 0 at the bottom — so a new caption appears
/// above the ones already showing rather than shunting them.
#[derive(Debug, Clone, PartialEq)]
pub struct SubtitleCaption {
    /// The translated caption text.
    pub text: String,
    /// The text ink's per-channel brightness in `0.0..=1.0`, lerped from white to
    /// mid grey across the display window. Alpha is always opaque — see the module
    /// doc.
    pub brightness: f32,
    /// The direction arrow, or `None` when the sound is within the forward cone.
    pub arrow: Option<SubtitleArrow>,
}

/// The live caption set. Fed by [`crate::audio::ShellAudio`]'s two play methods,
/// drained per frame by [`Self::views`].
#[derive(Debug, Default)]
pub struct SubtitleQueue {
    /// Insertion order, oldest first — which is also vanilla's *draw* order, with
    /// row 0 at the bottom, so the oldest caption sits lowest and new ones push up.
    subtitles: Vec<Subtitle>,
}

impl SubtitleQueue {
    /// Record that `text`'s sound just played at `pos`.
    ///
    /// Refreshes an existing row when the text already appears, replacing any
    /// entry at the identical position first — `Subtitle.refresh`
    /// (`SubtitleOverlay.java:158-161`), which is what stops a looping sound at a
    /// fixed point growing an unbounded position list.
    pub fn push(&mut self, text: &str, pos: Vec3, now_ms: u64) {
        if let Some(existing) = self.subtitles.iter_mut().find(|s| s.text == text) {
            existing.played_at.retain(|p| p.pos != pos);
            existing.played_at.push(PlayedAt { pos, at_ms: now_ms });
            return;
        }
        self.subtitles.push(Subtitle {
            text: text.to_string(),
            played_at: vec![PlayedAt { pos, at_ms: now_ms }],
        });
    }

    /// Drop every play older than the display window, and every caption left with
    /// no plays — `purgeOldInstances` + `isStillActive`.
    pub fn purge(&mut self, now_ms: u64) {
        for s in &mut self.subtitles {
            s.played_at
                .retain(|p| now_ms.saturating_sub(p.at_ms) <= DISPLAY_MS);
        }
        self.subtitles.retain(|s| !s.played_at.is_empty());
    }

    /// Whether nothing is live. Cheap enough for a caller to branch on before
    /// building a listener basis.
    pub fn is_empty(&self) -> bool {
        self.subtitles.is_empty()
    }

    /// How many captions are live.
    pub fn len(&self) -> usize {
        self.subtitles.len()
    }

    /// This frame's drawable rows against a listener at `pos` looking along
    /// `forward` with `right` to its right. Purges first, so a caller need not.
    pub fn views(
        &mut self,
        pos: Vec3,
        forward: Vec3,
        right: Vec3,
        now_ms: u64,
    ) -> Vec<SubtitleCaption> {
        self.purge(now_ms);
        self.subtitles
            .iter()
            .filter_map(|s| {
                // The closest recent play, `Subtitle.getClosest` — both the arrow
                // direction and the fade clock come from that one instance, so
                // they can never describe two different plays.
                let closest = s.played_at.iter().min_by(|a, b| {
                    a.pos
                        .distance_squared(pos)
                        .total_cmp(&b.pos.distance_squared(pos))
                })?;
                let age = now_ms.saturating_sub(closest.at_ms) as f32;
                let t = (age / DISPLAY_MS as f32).clamp(0.0, 1.0);
                let brightness = (BRIGHTNESS_NEW + (BRIGHTNESS_OLD - BRIGHTNESS_NEW) * t) / 255.0;
                let delta = (closest.pos - pos).normalize_or_zero();
                let forwardness = f64::from(forward.dot(delta));
                let rightness = f64::from(right.dot(delta));
                let arrow = if forwardness > IN_VIEW_DOT || rightness == 0.0 {
                    None
                } else if rightness > 0.0 {
                    Some(SubtitleArrow::Right)
                } else {
                    Some(SubtitleArrow::Left)
                };
                Some(SubtitleCaption {
                    text: s.text.clone(),
                    brightness,
                    arrow,
                })
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repeat_refreshes_the_row_instead_of_stacking_one() {
        let mut q = SubtitleQueue::default();
        q.push("Stone breaks", Vec3::new(1.0, 0.0, 0.0), 0);
        q.push("Stone breaks", Vec3::new(9.0, 0.0, 0.0), 1000);
        assert_eq!(q.len(), 1, "one row, two recorded positions");

        // The older play expires first, and the row survives on the newer one.
        q.purge(DISPLAY_MS + 500);
        assert_eq!(q.len(), 1);
        q.purge(DISPLAY_MS + 1500);
        assert_eq!(q.len(), 0, "the window expires the row entirely");
    }

    /// The arrow is `None` in front, and flips side behind — with the fade
    /// predicted rather than merely asserted to decrease.
    #[test]
    fn arrows_and_fade_follow_the_listener_basis() {
        let mut q = SubtitleQueue::default();
        let forward = Vec3::NEG_Z;
        let right = Vec3::X;

        q.push("Ahead", Vec3::new(0.0, 0.0, -10.0), 0);
        let v = q.views(Vec3::ZERO, forward, right, 0);
        assert_eq!(v[0].arrow, None, "a sound in the forward cone gets no arrow");
        assert!((v[0].brightness - 1.0).abs() < 1e-6, "a fresh caption is white");

        let mut q = SubtitleQueue::default();
        q.push("Behind right", Vec3::new(10.0, 0.0, 5.0), 0);
        let v = q.views(Vec3::ZERO, forward, right, DISPLAY_MS);
        assert_eq!(v[0].arrow, Some(SubtitleArrow::Right));
        // Full window elapsed: exactly `75/255`, not merely "less than 1".
        assert!((v[0].brightness - 75.0 / 255.0).abs() < 1e-4, "{v:?}");

        let mut q = SubtitleQueue::default();
        q.push("Behind left", Vec3::new(-10.0, 0.0, 5.0), 0);
        let v = q.views(Vec3::ZERO, forward, right, 0);
        assert_eq!(v[0].arrow, Some(SubtitleArrow::Left));
    }
}
