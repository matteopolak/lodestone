//! A legacy `§` code must never become a glyph on a HUD string surface.
//!
//! # What this gates, and why it is not a pixel test
//!
//! `Text::to_spans`' expansion is gated in `lodestone-model` against the
//! `StringDecomposer` semantics, and the allowlist on the non-expanding flatten is
//! gated by that crate's `legacy_expansion_guard`. Neither of those can tell you a
//! **surface** changed: the model tests are a closed loop around one function, and
//! the guard only proves nobody names the wrong one. The defect this whole change
//! fixes lived in the *font layer* — `VanillaFont::draw` emitting `§` and `7` as
//! two glyphs — and a `String`-carrying surface never touches `to_spans` at all.
//!
//! So this drives `HudGeometry::build`, the real geometry builder, on two surfaces
//! that were broken (the title/subtitle overlay and boss bar titles) and asserts on
//! the emitted vertex buffer. No GPU, no `client.jar`: with no `VanillaFont`
//! attached the builder falls back to the fixed-advance 5×7 debug font, whose draw
//! (`hud::item_icon::ColourStream::text`) and measure (`text_w`) were fixed in the
//! same change, so the fallback exercises the same rule the proportional path does.
//!
//! # The discriminating input
//!
//! Three arms per surface, because "the coded string draws less than the literal
//! one" is satisfied by any amount of undercounting:
//!
//! | arm | string | correct hypothesis | "codes are ordinary characters" |
//! |---|---|---|---|
//! | plain | `hi` | baseline | baseline |
//! | coded | `§chi` | **byte-identical to plain** | plain + ink for `§` and `c` |
//! | literal | `chi` | strictly more than plain | equals the coded arm |
//!
//! `coded == plain` is an equality on the whole buffer, not a count, so it also
//! covers the *measurement* half — a centred title measured over the raw string
//! lands at a different `x`, and every vertex moves. `literal != plain` is the
//! control that the two hypotheses actually differ on this input: if `c` had no ink
//! in this font, `coded == plain` would hold under **both** readings and the test
//! would be measuring nothing.

use lodestone::hud::{DebugStats, HudFrame, HudGeometry};
use lodestone::overlay::BossBarView;

/// Canvas size; anything is fine, both arms share it.
const W: u32 = 640;
const H: u32 = 480;

/// `§c` — a colour code, so it exercises the branch that also has to clear the
/// formatting flags, rather than a bare `§r`.
const CODED: &str = "\u{00a7}chi";
/// The same string with the `§` removed: what "a code is two ordinary characters"
/// would very nearly draw, and the upper bound of that hypothesis' error.
const LITERAL: &str = "chi";
/// The visible text `CODED` must reduce to.
const PLAIN: &str = "hi";

/// The title/subtitle overlay. Its string arrives from
/// `Sim::title_overlay`'s `to_legacy_string()`, so it is `§`-coded by
/// construction — and it was drawn through `Builder::text`, the plain path.
#[test]
fn a_coded_title_draws_exactly_the_visible_text() {
    let stats = DebugStats::default();
    let build = |title: &str, subtitle: &str| {
        HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                // Both a title and a subtitle: they are drawn at different pose
                // scales through two separate call sites, and one being fixed
                // while the other is not is exactly the per-call-site outcome
                // this change exists to avoid.
                title: Some((title.to_owned(), Some(subtitle.to_owned()), 1.0)),
                ..HudFrame::new(&stats)
            },
            W,
            H,
        )
    };

    let plain = build(PLAIN, PLAIN);
    let coded = build(CODED, CODED);
    let literal = build(LITERAL, LITERAL);

    assert!(
        plain.vertex_count() > 0,
        "sanity: the title must draw something at all, or every equality below is vacuous"
    );
    assert_ne!(
        literal.vertex_count(),
        plain.vertex_count(),
        "control failed: `c` contributes no ink in this font, so the correct and the \
         wrong hypothesis coincide on this input and the assertion below proves nothing"
    );
    assert_eq!(
        coded.verts, plain.verts,
        "a `§c` prefix must be consumed, not drawn: coded emitted {} vertices, plain {}, \
         and the 'codes are ordinary characters' reading would emit at least the {} of \
         the literal arm. Buffer equality (not just the count) is asserted because the \
         measure is centred — if the width still counted the code pair, every vertex \
         would shift in x.",
        coded.vertex_count(),
        plain.vertex_count(),
        literal.vertex_count(),
    );
}

/// Boss bar titles. A separate surface with its own centring arithmetic, drawn from
/// `overlay::BossBarView::title` — a plain `String` that `resolve_to_string` fills
/// straight from the server's component, `§` codes and all.
#[test]
fn a_coded_boss_bar_title_draws_exactly_the_visible_text() {
    let stats = DebugStats::default();
    let build = |title: &str| {
        let bars = [BossBarView {
            title: title.to_owned(),
            progress: 0.5,
            color: [1.0, 0.0, 1.0],
        }];
        HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                boss_bars: &bars,
                ..HudFrame::new(&stats)
            },
            W,
            H,
        )
    };

    let plain = build(PLAIN);
    let coded = build(CODED);
    let literal = build(LITERAL);

    assert!(
        plain.vertex_count() > 0,
        "sanity: a boss bar must draw something at all"
    );
    assert_ne!(
        literal.vertex_count(),
        plain.vertex_count(),
        "control failed: the two hypotheses coincide on this input"
    );
    assert_eq!(
        coded.verts, plain.verts,
        "a boss bar title's `§c` must be consumed: coded {} vertices, plain {}, literal {}",
        coded.vertex_count(),
        plain.vertex_count(),
        literal.vertex_count(),
    );
}
