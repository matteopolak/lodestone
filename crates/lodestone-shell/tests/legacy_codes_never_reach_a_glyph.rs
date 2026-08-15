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

//! # What changed when these two surfaces started carrying spans
//!
//! The title/subtitle overlay and the boss bar title used to be `String` fields
//! and reached `Builder::text`, so "is the `§` consumed?" was a question about the
//! *font* layer. Both now carry `Vec<TextSpan>`, because
//! `Text::to_legacy_string`/`to_plain_string` cannot express a hex colour and a
//! server's hex title was arriving white — see `HudFrame::title`.
//!
//! The rule these tests exist for is unchanged and the input is the same coded
//! string; what moved is *where* the code is consumed. A producer now calls
//! `Text::to_spans`, which expands the pair into a coloured run, so the assertion
//! gets **stronger**: the pair must contribute no glyph and no advance (positions
//! byte-identical to the plain arm) *and* the colour it named must actually reach
//! the vertex. The old buffer-equality assertion cannot be kept as written — a
//! correctly *applied* `§c` changes every vertex's colour, which is the point.

use lodestone::hud::{DebugStats, HudFrame, HudGeometry};
use lodestone::overlay::{BossBarView, plain_spans};
use lodestone_model::Text;
use lodestone_model::text::TextSpan;

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

/// Vanilla's `red`, `TextColor.java`'s `named("red", 16733525)` — the colour `§c`
/// names, hand-transcribed rather than read back from our own table.
const RED: (u8, u8, u8) = (0xff, 0x55, 0x55);

/// The spans a producer really hands over for a `§`-coded server component: the
/// component parsed from the legacy string, then `to_spans`, which is the expanding
/// pass. Building `TextSpan`s by hand here would bypass the expansion under test.
fn coded_spans(s: &str) -> Vec<TextSpan> {
    Text::from_legacy(s).to_spans()
}

/// Every vertex's `(x, y)`, quantised to the byte grid the canvas uses. Positions
/// alone, so a *correctly applied* colour code does not make the geometry
/// comparison fail for the wrong reason.
fn positions(geo: &HudGeometry) -> Vec<(i32, i32)> {
    geo.verts
        .chunks_exact(6)
        .map(|v| (v[0].to_bits() as i32, v[1].to_bits() as i32))
        .collect()
}

/// Whether any fully-opaque vertex carries `rgb`.
fn has_colour(geo: &HudGeometry, rgb: (u8, u8, u8)) -> bool {
    let byte = |v: f32| (v * 255.0).round().clamp(0.0, 255.0) as u8;
    geo.verts
        .chunks_exact(6)
        .any(|v| (byte(v[2]), byte(v[3]), byte(v[4])) == rgb)
}

/// The title/subtitle overlay. Its spans arrive from `Sim::title_overlay`'s
/// `to_spans()`, so a `§`-coded server component reaches here already expanded.
#[test]
fn a_coded_title_draws_exactly_the_visible_text() {
    let stats = DebugStats::default();
    let build = |title: Vec<TextSpan>, subtitle: Vec<TextSpan>| {
        HudGeometry::build(
            &HudFrame {
                crosshair: false,
                show_debug: false,
                // Both a title and a subtitle: they are drawn at different pose
                // scales through two separate call sites, and one being fixed
                // while the other is not is exactly the per-call-site outcome
                // this change exists to avoid.
                title: Some((title, Some(subtitle), 1.0)),
                ..HudFrame::new(&stats)
            },
            W,
            H,
        )
    };

    let plain = build(plain_spans(PLAIN), plain_spans(PLAIN));
    let coded = build(coded_spans(CODED), coded_spans(CODED));
    let literal = build(plain_spans(LITERAL), plain_spans(LITERAL));

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
        positions(&coded),
        positions(&plain),
        "a `§c` prefix must contribute no glyph and no advance: coded emitted {} \
         vertices, plain {}, and the 'codes are ordinary characters' reading would \
         emit at least the {} of the literal arm. Positions (not just the count) are \
         compared because the measure is centred — if the width still counted the \
         code pair, every vertex would shift in x.",
        coded.vertex_count(),
        plain.vertex_count(),
        literal.vertex_count(),
    );
    // And the colour the code named must actually be applied, which the old
    // `String` path could only do inside the font and this one has to do through
    // the span pipeline.
    assert!(
        has_colour(&coded, RED),
        "`§c` must recolour the run to vanilla red {RED:?}; it was consumed but its \
         colour never reached a vertex"
    );
    assert!(
        !has_colour(&plain, RED),
        "control: the uncoloured arm must not contain red, or the assertion above is \
         satisfied by something else on this surface"
    );
}

/// Boss bar titles. A separate surface with its own centring arithmetic, drawn from
/// `overlay::BossBarView::title` — the spans `boss_bars_from` fills straight from
/// the server's component.
#[test]
fn a_coded_boss_bar_title_draws_exactly_the_visible_text() {
    let stats = DebugStats::default();
    let build = |title: Vec<TextSpan>| {
        let bars = [BossBarView {
            title,
            progress: 0.5,
            color: lodestone_game::bossbar::BossBarColor::Purple,
            overlay: lodestone_game::bossbar::BossBarOverlay::Progress,
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

    let plain = build(plain_spans(PLAIN));
    let coded = build(coded_spans(CODED));
    let literal = build(plain_spans(LITERAL));

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
        positions(&coded),
        positions(&plain),
        "a boss bar title's `§c` must be consumed: coded {} vertices, plain {}, literal {}",
        coded.vertex_count(),
        plain.vertex_count(),
        literal.vertex_count(),
    );
    assert!(
        has_colour(&coded, RED),
        "`§c` must recolour a boss bar title to vanilla red {RED:?}"
    );
}
