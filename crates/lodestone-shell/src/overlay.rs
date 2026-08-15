//! Pure folding of live client state (the scoreboard sidebar's *shape*, boss
//! bars) into flat, version-free view structs the HUD can draw.
//!
//! Kept separate from [`crate::hud`] geometry and from the [`crate::net`] /
//! [`crate::sim`] wiring so the *interpretation* of scoreboard/boss-bar state is
//! unit-testable with no GPU and no server — which is where the "built, tested,
//! wired to nothing" gap (§12.24) actually closes: these types are the last mile
//! between modelled game state and pixels, and their tests assert on the exact
//! rows a player would read.
//!
//! ## What Stage 3 removed from here
//!
//! This module used to carry a *second* sidebar projection, `sidebar_from` /
//! `sidebar_view`, over the deleted `lodestone_client::Scoreboard`. It was
//! reachable only through `NetClient::sidebar()`, which **nothing called** — the
//! HUD has drawn [`crate::scoreboard::sidebar_from`] (over `lodestone-game`'s
//! aggregate, with `translate` resolution) for as long as that function has
//! existed. Two projections of one thing, one of them unreachable, was the same
//! defect one layer up from the double fold `docs/bevy-migration.md` §1.1
//! measured. [`Sidebar`] / [`SidebarLine`] stay here because they are the HUD's
//! vocabulary, not the fold's.
//!
//! ## The same defect, found a second time
//!
//! `NetClient::boss_bars()` was the boss-bar twin of exactly that shape, and it
//! survived Stage 3 because nothing about it looked wrong on inspection: a
//! `pub fn` with a plausible doc comment, folding the right component, next to
//! sibling accessors that *are* live. Nothing called it — the HUD reaches
//! [`Sidebar`]'s counterpart through [`crate::sim::Sim::boss_bars`], which reads
//! the same `lodestone_ecs::SessionBossBars` component directly.
//!
//! It was **not** merely redundant. It passed a null translator (`&|_| None`)
//! into [`boss_bars_from`], so anything that had started calling it would have
//! rendered boss-bar titles as raw keys like `entity.minecraft.ender_dragon` —
//! and it only carried that null translator because the *live* path was given a
//! real one while fixing exactly that bug, and this one had no language table to
//! fix it with. A dead projection does not stay merely dead: it drifts, silently,
//! away from the live one, and it is the version a future caller will find first.
//!
//! The lesson for the next one: **two projections of one thing is the defect**,
//! whether or not the dead half currently misbehaves. Grep for a second folder
//! whenever one is touched.

use lodestone_game::bossbar::{BossBarColor, BossBarOverlay, BossBarSet};
use lodestone_model::text::TextSpan;

/// A ready-to-draw scoreboard sidebar: a title plus up to 15 rows, each a label
/// and its score.
///
/// Every field is a **styled span list**, not a `String`. These used to be
/// `String`s, and that was where the sidebar's colour died: the projection
/// called `Text::to_plain_string()`, so a server that coloured its objective
/// title or a holder's name — which is most of what a scoreboard sidebar is
/// *for* — had that colour discarded one layer before the HUD, and the HUD then
/// painted every row in a hardcoded constant. Keeping spans here means the
/// vocabulary between fold and pixels can express what the wire already carried.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Sidebar {
    /// The objective's display name, shown centred at the top.
    pub title: Vec<TextSpan>,
    /// The score rows, top-to-bottom in render order.
    pub lines: Vec<SidebarLine>,
}

/// One sidebar row: the holder's label and its score value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarLine {
    /// Left-aligned holder label (per-score display override, else the holder).
    pub label: Vec<TextSpan>,
    /// Right-aligned score. Vanilla's default is red, which the HUD supplies as
    /// the *base* colour; a `NumberFormat::Styled`/`Fixed` colour from the server
    /// overrides it per span.
    pub score: Vec<TextSpan>,
}

/// One uncoloured span: the "no server styling" case.
///
/// For demo/test fixtures and for any caller that has a plain string and wants
/// the sidebar's span vocabulary. An uncoloured span renders in whatever base
/// colour the surface passes, so this is behaviour-preserving for text that was
/// never styled to begin with.
#[must_use]
pub fn plain_spans(text: impl Into<String>) -> Vec<TextSpan> {
    let text = text.into();
    if text.is_empty() {
        return Vec::new();
    }
    vec![TextSpan {
        text,
        style: lodestone_model::text::TextStyle::default(),
    }]
}

/// The concatenated plain text of a span list, style discarded.
///
/// For assertions about **wording** and for the few consumers that genuinely
/// need a string (a window title, a log line). Note what this cannot do: a test
/// that only ever calls this is blind to colour by construction, so a colour
/// assertion has to read `span.style.color` per span, or measure pixels.
#[must_use]
pub fn spans_text(spans: &[TextSpan]) -> String {
    spans.iter().map(|s| s.text.as_str()).collect()
}

/// Split a span list at every `\n`, keeping each run's style on both sides of the
/// break.
///
/// A server writes a multi-line tab-list banner as literal newlines *inside* one
/// component, so a surface that draws one line per row has to break the span list
/// rather than the string — the whole reason this exists instead of
/// `spans_text(...).split('\n')`, which would throw away the colour that is the
/// point of carrying spans at all. A break inside a single span yields two spans
/// sharing that span's style.
///
/// Empty runs are dropped, so a line consisting only of a break is an **empty
/// line** (an empty `Vec`) rather than a line holding one empty span — the shape
/// the width measurement and the plate height both already assume. A span list
/// with no newline yields exactly one line; an empty input yields **no** lines,
/// because a caller drawing one row per element must draw nothing for nothing.
#[must_use]
pub fn spans_lines(spans: &[TextSpan]) -> Vec<Vec<TextSpan>> {
    let mut lines: Vec<Vec<TextSpan>> = Vec::new();
    let mut current: Vec<TextSpan> = Vec::new();
    let mut any = false;
    for span in spans {
        any = true;
        // `split('\n')` on "a\nb" gives ["a", "b"], and on "a\n" gives ["a", ""] —
        // so the number of pieces is one more than the number of breaks, and
        // pushing `current` between pieces is exactly one push per break.
        let mut pieces = span.text.split('\n');
        if let Some(first) = pieces.next() {
            if !first.is_empty() {
                current.push(TextSpan {
                    text: first.to_owned(),
                    style: span.style,
                });
            }
        }
        for piece in pieces {
            lines.push(std::mem::take(&mut current));
            if !piece.is_empty() {
                current.push(TextSpan {
                    text: piece.to_owned(),
                    style: span.style,
                });
            }
        }
    }
    if any {
        lines.push(current);
    }
    lines
}

/// A ready-to-draw boss bar: a styled title, a clamped progress fraction, and
/// the colour/overlay enums that select vanilla's real per-colour sprite art.
///
/// Earlier this carried a `color: [f32; 3]` RGB tint instead, on the
/// (unchecked) assumption that vanilla paints one greyscale bar and tints it.
/// `BossHealthOverlay.BAR_BACKGROUND_SPRITES`/`BAR_PROGRESS_SPRITES`
/// (`.cache/mc/26.2/client-src`) say otherwise: seven **distinct** sprite
/// files per colour, blitted with `color = -1` (opaque white, no tint at
/// all). A tint field could not have driven that draw no matter what the HUD
/// did with it — the fold had already thrown away the one piece of
/// information (*which* colour) a sprite-based draw needs, and manufactured a
/// value (an approximate RGB) that vanilla's own draw call never uses.
#[derive(Debug, Clone, PartialEq)]
pub struct BossBarView {
    /// The title as **styled spans**, for the same reason [`Sidebar`]'s fields
    /// are. This was a `String` filled by `resolve_to_string`, which flattens
    /// through [`lodestone_model::Text::to_plain_string`] — so a boss bar whose
    /// title carried a hex colour arrived here uncoloured. A legacy `§` code
    /// survived a `String` (the font layer applies codes at draw time); a
    /// [`lodestone_model::text::TextColor::Rgb`] has no legacy code and so could
    /// not, which is why the vocabulary here has to be spans rather than a
    /// better-flattened string.
    pub title: Vec<TextSpan>,
    /// Progress in `0.0..=1.0`.
    pub progress: f32,
    /// Bar colour — selects the background/progress sprite pair via
    /// [`BossBarColor::background_sprite_id`]/[`BossBarColor::progress_sprite_id`].
    pub color: BossBarColor,
    /// Division/overlay style — selects the notch sprite pair (or none, for
    /// [`BossBarOverlay::Progress`]) via
    /// [`BossBarOverlay::background_sprite_id`]/[`BossBarOverlay::progress_sprite_id`].
    pub overlay: BossBarOverlay,
}

/// Vanilla's `Mth.lerpDiscrete(alpha, p0, p1)` (`.cache/mc/26.2/client-src`):
/// an integer interpolation between `p0` and `p1` that is `0` only at
/// `alpha == 0.0` and `p1` only at `alpha >= 1.0`, otherwise
/// `p0 + floor(alpha * (p1 - p0 - 1)) + 1`. `BossHealthOverlay.extractBar`
/// feeds this `(progress, 0, 182)` to get the progress sprite's pixel width —
/// **not** `progress * 182` rounded or truncated, which disagrees with this
/// formula at most fractions (they coincide at `0.0`, `1.0`, and `0.5`, which
/// is exactly why a fixture corpus that only ever tries a half-full bar can't
/// tell the two apart).
#[must_use]
pub fn lerp_discrete_width(alpha: f32, native_width: i32) -> i32 {
    if alpha <= 0.0 {
        return 0;
    }
    let delta = native_width;
    let mut w = (alpha * (delta - 1) as f32).floor() as i32 + 1;
    if w > native_width {
        w = native_width;
    }
    w
}

/// Fold the active boss bars into drawable views, preserving server (render)
/// order. Progress is clamped defensively in case a server sends out of range.
///
/// Takes the folded [`BossBarSet`] — one of the three implementations of this
/// event family that Stage 3 collapsed to one. `BossBarSet::iter` is what
/// carries insertion order; a `HashMap` iteration would shuffle the stack every
/// frame.
///
/// `translate` is the language table (an `assets::Language` becomes one via
/// `Language::translator`). It is **not** optional decoration: a vanilla boss
/// bar's title is the boss entity's display name, i.e.
/// `translate("entity.minecraft.ender_dragon")` — flattening it without a table
/// puts the raw key on screen. Same shape as [`crate::scoreboard::sidebar_from`]
/// and [`crate::tablist::tab_list_view`], deliberately.
#[must_use]
pub fn boss_bars_from(
    bars: &BossBarSet,
    translate: &dyn Fn(&str) -> Option<String>,
) -> Vec<BossBarView> {
    bars.iter()
        .map(|(_, b)| BossBarView {
            // `resolve` then `to_spans`, exactly as `crate::scoreboard`'s own
            // `spans` helper does: resolving to literals first means the trailing
            // flatten never consults the model's stub table, and `to_spans`
            // applies `TextStyle::inherit` down the tree so a nested run with no
            // colour of its own arrives carrying its parent's.
            title: lodestone_game::text::resolve(&b.title, translate).to_spans(),
            progress: b.progress.clamp(0.0, 1.0),
            color: b.color,
            overlay: b.overlay,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::bossbar::{BossBar, BossBarOverlay};
    use lodestone_model::Text;

    fn boss_bar(title: &str, progress: f32, color: BossBarColor) -> BossBar {
        BossBar {
            title: Text::literal(title),
            // Deliberately assigned rather than via `set_progress`, which
            // clamps: the clamp under test is this module's own defensive one,
            // and going through the setter would make the assertion vacuous.
            progress,
            color,
            overlay: BossBarOverlay::Progress,
            darken_screen: false,
            play_music: false,
            create_fog: false,
        }
    }

    /// A tiny table so these tests depend on no real asset. Deliberately does
    /// **not** contain the chat/death keys `Text`'s built-in stub table carries,
    /// so a fold that ignored this closure would be visible.
    fn tr(key: &str) -> Option<String> {
        match key {
            "entity.minecraft.ender_dragon" => Some("Ender Dragon".to_owned()),
            _ => None,
        }
    }

    #[test]
    fn titles_resolve_translate_components_through_the_translator() {
        // A vanilla boss bar's title is the boss's display name — a `translate`
        // node, not prose. That fix's defect class: flattening it without the
        // language table renders `entity.minecraft.ender_dragon`.
        let mut bars = BossBarSet::new();
        bars.add(
            uuid::Uuid::from_u128(7),
            BossBar {
                title: Text::translate("entity.minecraft.ender_dragon", vec![]),
                progress: 1.0,
                color: BossBarColor::Purple,
                overlay: BossBarOverlay::Progress,
                darken_screen: false,
                play_music: false,
                create_fog: false,
            },
        );

        let views = boss_bars_from(&bars, &tr);
        assert_eq!(spans_text(&views[0].title), "Ender Dragon");

        // -- negative control -------------------------------------------------
        // The same fold against a table that knows nothing must surface the raw
        // key, proving the assertion above is reading the translator and not a
        // built-in table that happens to agree.
        let empty = boss_bars_from(&bars, &|_| None);
        assert_eq!(spans_text(&empty[0].title), "entity.minecraft.ender_dragon");
    }

    /// [`spans_lines`] breaks on `\n` **and keeps each side's style**.
    ///
    /// The discriminating input is a break *inside* one span whose two halves must
    /// both stay coloured: a `spans_text(...).split('\n')` implementation gets the
    /// wording right and the colour wrong, so an assertion on the text alone
    /// cannot separate the two. Mismatches are collected and asserted on the
    /// collection rather than inside the loop, so a failure reports every arm.
    #[test]
    fn spans_lines_breaks_on_newlines_and_carries_style_across_the_break() {
        use lodestone_model::text::{TextColor, TextSpan, TextStyle};

        let styled = |text: &str, color: TextColor| TextSpan {
            text: text.to_owned(),
            style: TextStyle {
                color: Some(color),
                ..TextStyle::default()
            },
        };

        assert!(
            spans_lines(&[]).is_empty(),
            "no spans means no lines, so a caller drawing one row per line draws none"
        );
        assert_eq!(
            spans_lines(&plain_spans("one"))
                .iter()
                .map(|l| spans_text(l))
                .collect::<Vec<_>>(),
            vec!["one".to_owned()],
            "a list with no break is one line"
        );

        // Gold "A\nB" then a separate aqua "\nC": three lines, the middle one made
        // of the gold tail and the aqua head, and a leading break in the second
        // span must not merge into the first line.
        let input = vec![styled("A\nB", TextColor::Gold), styled("\nC", TextColor::Aqua)];
        let lines = spans_lines(&input);
        let mut wrong = Vec::new();
        let want: [(&str, &[Option<TextColor>]); 3] = [
            ("A", &[Some(TextColor::Gold)]),
            ("B", &[Some(TextColor::Gold)]),
            ("C", &[Some(TextColor::Aqua)]),
        ];
        if lines.len() != want.len() {
            wrong.push(format!(
                "line count: want {}, got {} ({:?})",
                want.len(),
                lines.len(),
                lines.iter().map(|l| spans_text(l)).collect::<Vec<_>>()
            ));
        }
        for (i, (text, colours)) in want.iter().enumerate() {
            let Some(line) = lines.get(i) else { continue };
            if spans_text(line) != *text {
                wrong.push(format!("line {i} text: want {text:?}, got {:?}", spans_text(line)));
            }
            let got: Vec<Option<TextColor>> = line.iter().map(|s| s.style.color).collect();
            if got.as_slice() != *colours {
                wrong.push(format!("line {i} colours: want {colours:?}, got {got:?}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// A **hex** title colour survives the fold.
    ///
    /// The discriminating input: a named colour cannot separate a working span
    /// path from the old `String` one, because the font layer applies `§` codes at
    /// draw time and `to_legacy_string`/`to_plain_string` both keep enough for a
    /// named colour to be recoverable downstream. `TextColor::Rgb` has no legacy
    /// code at all, so it is the only input on which the two hypotheses differ.
    #[test]
    fn a_hex_title_colour_survives_the_fold() {
        // Not a multiple of 0x11 and not near any named colour, so a fallback
        // would be obvious rather than plausible.
        const HEX: u32 = 0x001f_2e3d;
        let mut bars = BossBarSet::new();
        bars.add(
            uuid::Uuid::from_u128(9),
            BossBar {
                title: Text {
                    style: lodestone_model::TextStyle {
                        color: Some(lodestone_model::TextColor::Rgb(HEX)),
                        ..lodestone_model::TextStyle::default()
                    },
                    ..Text::literal("Boss")
                },
                progress: 1.0,
                color: BossBarColor::White,
                overlay: BossBarOverlay::Progress,
                darken_screen: false,
                play_music: false,
                create_fog: false,
            },
        );

        let views = boss_bars_from(&bars, &tr);
        assert_eq!(spans_text(&views[0].title), "Boss", "wording must survive");
        assert_eq!(
            views[0].title[0].style.color,
            Some(lodestone_model::TextColor::Rgb(HEX)),
            "a hex title colour must reach the view; a flatten to String cannot \
             carry it, which is the whole reason this field is spans"
        );
    }

    #[test]
    fn boss_bars_fold_title_progress_and_clamp_in_insertion_order() {
        let mut bars = BossBarSet::new();
        bars.add(
            uuid::Uuid::from_u128(1),
            boss_bar("Ender Dragon", 0.5, BossBarColor::Purple),
        );
        // Out of range on purpose.
        bars.add(
            uuid::Uuid::from_u128(2),
            boss_bar("Overshoot", 2.0, BossBarColor::Red),
        );

        let views = boss_bars_from(&bars, &tr);
        assert_eq!(views.len(), 2, "one view per active bar");
        assert_eq!(
            spans_text(&views[0].title),
            "Ender Dragon",
            "insertion order is render order"
        );
        assert!((views[0].progress - 0.5).abs() < 1e-6);
        assert_eq!(spans_text(&views[1].title), "Overshoot");
        assert!(
            (views[1].progress - 1.0).abs() < 1e-6,
            "progress must clamp to 1.0, got {}",
            views[1].progress
        );
        assert_eq!(views[0].color, BossBarColor::Purple, "colour must fold through");
        assert_eq!(views[1].color, BossBarColor::Red, "colour must fold through");
        assert_ne!(
            views[0].color.background_sprite_id(),
            views[1].color.background_sprite_id(),
            "purple and red must resolve to distinct sprite ids"
        );
    }

    /// [`lerp_discrete_width`] against vanilla's own `Mth.lerpDiscrete`
    /// formula, hand-expanded — the exact source of the boss bar's clipped
    /// progress-fill width, not a naive `progress * native_width` round or
    /// truncation. `0.0`/`1.0`/`0.5` are where the naive version happens to
    /// agree (this module's own doc comment names why that makes them
    /// non-discriminating); `0.3`/`0.61`/`0.99` do not.
    #[test]
    fn lerp_discrete_width_matches_vanillas_formula_not_a_naive_scale() {
        let cases: [(f32, i32); 8] = [
            (0.0, 0),
            (1.0, 182),
            (0.5, 91),
            (0.1, 19),  // floor(0.1 * 181) + 1 = floor(18.1) + 1 = 19; naive round(0.1 * 182) = 18
            (0.2, 37),  // floor(0.2 * 181) + 1 = floor(36.2) + 1 = 37; naive round(0.2 * 182) = 36
            (0.8, 145), // floor(0.8 * 181) + 1 = floor(144.8) + 1 = 145; naive round(0.8 * 182) = 146
            (0.9, 163), // floor(0.9 * 181) + 1 = floor(162.9) + 1 = 163; naive round(0.9 * 182) = 164
            (2.0, 182), // out-of-range progress must still clamp to the bar
        ];
        let mut wrong = Vec::new();
        for (alpha, want) in cases {
            let got = lerp_discrete_width(alpha, 182);
            if got != want {
                wrong.push(format!("alpha {alpha}: want {want}, got {got}"));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");

        // Negative control: the naive `round(alpha * 182)` hypothesis must
        // disagree at the discriminating inputs above, or this test would not
        // be able to tell the two implementations apart.
        let mut naive_agrees_everywhere = true;
        for (alpha, want) in cases {
            let naive = (alpha.clamp(0.0, 1.0) * 182.0).round() as i32;
            if naive != want {
                naive_agrees_everywhere = false;
            }
        }
        assert!(
            !naive_agrees_everywhere,
            "fixture is not discriminating: the naive scale matches every case"
        );
    }
}
