//! Gate: server-authored **text colour** must survive from the wire to the
//! emitted vertex, byte-exact, on every surface that draws a chat component.
//!
//! # Why this measures vertices rather than pixels
//!
//! [`HudGeometry::verts`] is flat `[x, y, r, g, b, a]`, and it is the last point
//! where a colour is a number this test chose rather than a number the GPU
//! decided. That matters here specifically: on this Metal backend, through
//! `ALPHA_BLENDING` with an `Rgba8UnormSrgb` target, the *effective* blend alpha
//! is a real, repeatable, non-trivial function of the raw fragment alpha which is
//! neither the identity nor `linear_to_srgb(a)` nor any single power law — so an
//! exact-byte prediction downstream of the blend cannot be stated honestly. The
//! quad colour is upstream of all of that.
//!
//! It also means this gate runs in the ordinary `cargo test --workspace`, with no
//! adapter and no `client.jar`, so a colour regression is caught by the default
//! health check rather than by an `#[ignore]`d gate nobody runs.
//!
//! # What this file does NOT cover, and how that was found
//!
//! **`HudGeometry::build` attaches no `VanillaFont`** — the font lives on
//! `HudRenderer`, not on the geometry builder — so every draw below takes
//! `Builder::text_spans`' jar-less fixed-advance fallback. That is a real surface
//! (it is what a jar-less run renders), but it is *not* the path a player with a
//! jar sees.
//!
//! This was not deduced by reading the code. Replacing `spans_run`'s per-span
//! colour with the base colour — the exact pre-fix defect — left all nine tests
//! here **green**, because the neutered function was never called. A `client.jar`
//! was present the whole time; the harness could not reach the code it was written
//! to check. That is the *world* species of vacuous test, and no amount of reading
//! this file would have revealed it.
//!
//! The vanilla path is gated in-crate instead, where a font can be attached and a
//! `ColourStream` constructed: `hud::vanilla_font::span_colour_tests`
//! (`--ignored`, fail-closed on a missing jar). The same neuter fails *that* gate
//! on both colour-carrying assertions.
//!
//! # Where the expected values come from
//!
//! [`VANILLA`] is transcribed **by hand from the decompiled jar**, not read from
//! the code under test: `TextColor.java` in `.cache/mc/26.2/src`, where
//! vanilla writes them in decimal (`named("gold", 16755200)`). Calling
//! `TextColor::rgb()` to build the expectation would make this
//! `decode(encode(x))` — satisfied by any self-consistent misunderstanding,
//! including the one where all sixteen values are wrong together.
//!
//! Note the jar's `ChatFormatting` is **not** the source: in 26.2 that enum
//! carries only the `§` code character and no colour at all.
//!
//! # The hypotheses this discriminates
//!
//! Vanilla is not colour-managed: a text colour is written to the framebuffer as
//! the sRGB byte it is, and the drop shadow is `ARGB.scaleRGB(color, 0.25F)` — a
//! quarter taken in **gamma** space (`Font.PreparedTextBuilder.getShadowColor`,
//! `ARGB.java`). The plausible-but-wrong implementation converts to
//! linear first. Both hypotheses are computed below and the measurement is
//! required to land on the right one; see [`gold_is_written_in_gamma_space`] and
//! [`shadow_is_a_gamma_space_quarter`].

use lodestone::hud::{DebugStats, HudFrame, HudGeometry};
use lodestone::overlay::{Sidebar, plain_spans};
use lodestone_model::text::{Text, TextColor, TextSpan, TextStyle};

const W: u32 = 640;
const H: u32 = 480;

/// Vanilla's sixteen named colours, `(name, 0xrrggbb)`.
///
/// Hand-transcribed from `TextColor.java` (26.2). The decimal vanilla
/// actually writes is in the trailing comment, so a reviewer can check the
/// conversion without opening the jar.
const VANILLA: [(&str, u32); 16] = [
    ("black", 0x0000_0000),       // named("black", 0)
    ("dark_blue", 0x0000_00aa),   // named("dark_blue", 170)
    ("dark_green", 0x0000_aa00),  // named("dark_green", 43520)
    ("dark_aqua", 0x0000_aaaa),   // named("dark_aqua", 43690)
    ("dark_red", 0x00aa_0000),    // named("dark_red", 11141120)
    ("dark_purple", 0x00aa_00aa), // named("dark_purple", 11141290)
    ("gold", 0x00ff_aa00),        // named("gold", 16755200)
    ("gray", 0x00aa_aaaa),        // named("gray", 11184810)
    ("dark_gray", 0x0055_5555),   // named("dark_gray", 5592405)
    ("blue", 0x0055_55ff),        // named("blue", 5592575)
    ("green", 0x0055_ff55),       // named("green", 5635925)
    ("aqua", 0x0055_ffff),        // named("aqua", 5636095)
    ("red", 0x00ff_5555),         // named("red", 16733525)
    ("light_purple", 0x00ff_55ff), // named("light_purple", 16733695)
    ("yellow", 0x00ff_ff55),      // named("yellow", 16777045)
    ("white", 0x00ff_ffff),       // named("white", 16777215)
];

/// The sixteen colours in `§`-code order, as [`TextColor`] variants. Order must
/// match [`VANILLA`]; `code_order_matches_vanilla_table` asserts it does.
const ORDER: [TextColor; 16] = [
    TextColor::Black,
    TextColor::DarkBlue,
    TextColor::DarkGreen,
    TextColor::DarkAqua,
    TextColor::DarkRed,
    TextColor::DarkPurple,
    TextColor::Gold,
    TextColor::Gray,
    TextColor::DarkGray,
    TextColor::Blue,
    TextColor::Green,
    TextColor::Aqua,
    TextColor::Red,
    TextColor::LightPurple,
    TextColor::Yellow,
    TextColor::White,
];

/// One ink vertex: its position and its colour as bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Ink {
    x: i32,
    y: i32,
    rgb: (u8, u8, u8),
}

/// Quantise a colour float to the byte it represents. `round`, not `floor`:
/// `170.0 / 255.0 * 255.0` is `169.99999` in binary floating point, and a
/// truncating conversion would turn every `0xAA` channel into `0xA9` and make
/// every assertion below fail for a reason that has nothing to do with colour.
fn as_byte(v: f32) -> u8 {
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

/// Every fully-opaque vertex in the geometry, as `(x, y, rgb)`.
///
/// The `alpha == 1.0` filter is the "what else already paints here" answer: the
/// sidebar panel behind the text is a translucent black rect (`alpha 0.55`), so
/// without this filter its vertices would be indistinguishable from black text.
/// Glyph ink and its shadow are both emitted at full alpha and are both wanted —
/// no named colour can be mistaken for a shadow, because a shadow is a quarter of
/// its colour and none of `0x00`/`0x55`/`0xAA`/`0xFF` is a quarter of another
/// (`0xAA/4 = 0x2A`, `0xFF/4 = 0x3F`, `0x55/4 = 0x15`). Black is its own shadow,
/// which is harmless for a presence test.
///
/// **`x`/`y` are pixel coordinates, converted back from the NDC
/// [`ColourStream`] actually writes** — `HudGeometry::verts` is `[x, y, r, g, b,
/// a]` with the position pair in `[-1, 1]` clip space
/// (`(2*px/w - 1, 1 - 2*py/h)`), the inverse of the transform every `rect`/
/// `glyph` call applies. Casting an NDC float straight to `i32` — the previous
/// version of this function — truncates almost every on-screen vertex to `0`,
/// so [`bbox`] always reported `(0, 0, 0, 0)` regardless of where the ink
/// actually was: not "found at the origin", but the diagnostic never having a
/// real coordinate to report at all. Presence/absence (`bbox(..).is_some()`)
/// was never affected — that only reads `rgb` — but the printed box was
/// useless for localising a mismatch, which is the one thing this repo's own
/// rule ("make failure output print a bounding box") requires it to do.
fn opaque_ink(geo: &HudGeometry) -> Vec<Ink> {
    let to_px = |ndc_x: f32, ndc_y: f32| {
        (
            ((ndc_x + 1.0) * W as f32 / 2.0).round() as i32,
            ((1.0 - ndc_y) * H as f32 / 2.0).round() as i32,
        )
    };
    geo.verts
        .chunks_exact(6)
        .filter(|v| (v[5] - 1.0).abs() < 1e-6)
        .map(|v| {
            let (x, y) = to_px(v[0], v[1]);
            Ink {
                x,
                y,
                rgb: (as_byte(v[2]), as_byte(v[3]), as_byte(v[4])),
            }
        })
        .collect()
}

/// The bounding box of every vertex carrying `rgb`, or `None` if none do.
///
/// Failure output prints a **box**, not a percentage: a gate that reports only a
/// fraction cannot tell a uniform-but-wrong frame from a localised blob, and the
/// two need different fixes.
fn bbox(ink: &[Ink], rgb: (u8, u8, u8)) -> Option<(i32, i32, i32, i32)> {
    let hits: Vec<&Ink> = ink.iter().filter(|i| i.rgb == rgb).collect();
    if hits.is_empty() {
        return None;
    }
    let x0 = hits.iter().map(|i| i.x).min()?;
    let x1 = hits.iter().map(|i| i.x).max()?;
    let y0 = hits.iter().map(|i| i.y).min()?;
    let y1 = hits.iter().map(|i| i.y).max()?;
    Some((x0, y0, x1, y1))
}

fn unpack(hex: u32) -> (u8, u8, u8) {
    (
        ((hex >> 16) & 0xff) as u8,
        ((hex >> 8) & 0xff) as u8,
        (hex & 0xff) as u8,
    )
}

fn span(text: &str, color: Option<TextColor>) -> TextSpan {
    TextSpan {
        text: text.to_string(),
        style: TextStyle {
            color,
            ..TextStyle::default()
        },
    }
}

/// Build a sidebar whose title is `spans`, and return the emitted geometry.
///
/// The sidebar is the subject because it is a real surface with a real defect
/// history, not a synthetic harness: its projection called `to_plain_string()`
/// and its draw passed three hardcoded `[f32; 4]` constants, so *no* server
/// colour could reach it by construction.
///
/// **No score rows.** This used to add one fixed `SidebarLine` — `"row"`/`"1"`,
/// both uncoloured — for realism. Both fields are drawn with a *base* colour
/// the row cannot opt out of by being uncoloured: the label's base is
/// `[1.0, 1.0, 1.0]` (white) and the score's is `SIDEBAR_SCORE_DEFAULT`, which
/// is vanilla's own `0xFF5555` — byte-identical to [`VANILLA`]'s `"red"`. So
/// every geometry this function built also always painted white and red,
/// regardless of `spans`' own colour: two callers below assert the *absence*
/// of every named colour but the one under test, and that fixed row made both
/// assertions false on their face — "found red" and "white is present" were
/// reports about the row, not about `spans`. This is the control-premise-false
/// species: the row's own drawing is correct (vanilla really does default a
/// score to red and a label to white), the confound is that an isolation test
/// shared a fixture with content it does not control. Row rendering already
/// has its own coverage in `hud.rs`'s `sidebar_draws_title_and_scored_rows`;
/// this fixture only needs to isolate the title.
fn geometry_for_title(spans: Vec<TextSpan>) -> HudGeometry {
    let side = Sidebar {
        title: spans,
        lines: vec![],
    };
    let stats = DebugStats::default();
    let frame = HudFrame {
        sidebar: Some(&side),
        crosshair: false,
        show_debug: false,
        ..HudFrame::new(&stats)
    };
    HudGeometry::build(&frame, W, H)
}

/// Guards [`ORDER`] against [`VANILLA`]: the two tables are indexed together
/// everywhere below, so a silent reordering of either would make every other test
/// in this file assert the wrong pairing while still passing.
#[test]
fn code_order_matches_vanilla_table() {
    for (i, (name, _)) in VANILLA.iter().enumerate() {
        assert_eq!(
            &ORDER[i].name(),
            name,
            "ORDER[{i}] must be the same colour VANILLA[{i}] names"
        );
    }
}

/// **The gate.** All sixteen named colours must reach the vertex stream at
/// vanilla's exact bytes.
#[test]
fn sixteen_named_colours_reach_the_vertex_stream_byte_exact() {
    let spans: Vec<TextSpan> = ORDER
        .iter()
        .map(|c| span("M", Some(*c)))
        .collect();
    let geo = geometry_for_title(spans);
    let ink = opaque_ink(&geo);
    assert!(
        !ink.is_empty(),
        "no opaque vertices at all: the sidebar title drew nothing, so this gate \
         would pass vacuously for any colour"
    );

    let mut missing = Vec::new();
    for (name, hex) in VANILLA {
        let want = unpack(hex);
        match bbox(&ink, want) {
            Some(_) => {}
            None => missing.push(format!("{name} #{hex:06x} {want:?}")),
        }
    }
    assert!(
        missing.is_empty(),
        "these vanilla colours never reached a vertex: {missing:?}\n\
         colours actually emitted: {:?}",
        distinct(&ink)
    );
}

/// The distinct colours present, for failure output.
fn distinct(ink: &[Ink]) -> Vec<(u8, u8, u8)> {
    let mut seen: Vec<(u8, u8, u8)> = ink.iter().map(|i| i.rgb).collect();
    seen.sort_unstable();
    seen.dedup();
    seen
}

/// **Negative control, executed.** Strip the colour and the assertion above must
/// fail. This is the same geometry builder, the same detector and the same
/// sixteen expectations, with the only difference being that the spans carry no
/// colour.
///
/// Without this, `sixteen_named_colours_reach_the_vertex_stream_byte_exact`
/// proves nothing: a detector that reported every colour present regardless of
/// input would satisfy it.
#[test]
fn control_uncoloured_spans_fail_the_sixteen_colour_assertion() {
    let geo = geometry_for_title(plain_spans("MMMMMMMMMMMMMMMM"));
    let ink = opaque_ink(&geo);
    assert!(
        !ink.is_empty(),
        "the control must still draw text — otherwise it fails for the wrong reason"
    );

    let found: Vec<&str> = VANILLA
        .iter()
        .filter(|(_, hex)| bbox(&ink, unpack(*hex)).is_some())
        .map(|(name, _)| *name)
        .collect();

    // The uncoloured title draws in the sidebar's base white, so `white` is
    // legitimately present and so is its shadow. Every *other* named colour must
    // be absent — that is the detector demonstrating it can say "no".
    assert!(
        found.len() < VANILLA.len(),
        "the control found all sixteen colours, so the subject's pass is vacuous"
    );
    let unexpected: Vec<&&str> = found.iter().filter(|n| **n != "white").collect();
    assert!(
        unexpected.is_empty(),
        "uncoloured text must not produce named colours, but found {unexpected:?}"
    );
}

/// A hex colour must arrive at its exact bytes.
///
/// This is the case the pre-existing renderer **structurally could not** draw: its
/// only bridge from a model colour to a pixel colour was
/// `TextColor::legacy_code()` → a `§`-keyed table, and a hex colour has no legacy
/// code, so `legacy_code()` returned `None` and the run silently fell back to the
/// surface's base colour. A gate built only from the sixteen named colours cannot
/// see that.
#[test]
fn a_hex_colour_reaches_the_vertex_stream_byte_exact() {
    // Deliberately not a multiple of 0x11 and not near any named colour, so a
    // fallback to the base white or to a nearest-named-colour rounding would be
    // obvious rather than plausible.
    let want = (0x1f, 0x2e, 0x3d);
    let hex = 0x001f_2e3d;
    let geo = geometry_for_title(vec![span("M", Some(TextColor::Rgb(hex)))]);
    let ink = opaque_ink(&geo);
    assert!(
        bbox(&ink, want).is_some(),
        "hex #{hex:06x} {want:?} never reached a vertex; emitted: {:?}",
        distinct(&ink)
    );
}

/// **The producer gate.** A hex colour must survive the *fold* on every HUD
/// surface whose producer used to flatten to a string, not just the sidebar.
///
/// # Why the sidebar tests above could not see this
///
/// Everything above drives `HudFrame::sidebar`, which has carried spans since the
/// sidebar's own fix. The title, subtitle, action bar, boss bar title and tab-list
/// banners were still `String`s filled by `Text::to_legacy_string` /
/// `to_plain_string` one layer *above* the renderer, so the renderer's span support
/// was reachable on exactly one of six surfaces and a hex colour on the other five
/// was discarded before `HudGeometry::build` ever ran. The fix is in the producers;
/// this is the gate that they are all wired.
///
/// # The discriminating input, and why each surface gets its own value
///
/// Hex, because a **named** colour cannot separate a working span path from the old
/// string one: the sixteen named colours have `§` codes and the font layer applies
/// codes at draw time, so `to_legacy_string` smuggled them through. `TextColor::Rgb`
/// has no code and could not be smuggled — it is the only input on which the two
/// hypotheses differ.
///
/// The six values are **pairwise distinct** (and none is a multiple of `0x11` or
/// near a named colour) so that a surface drawing another surface's text — the
/// silent-transposition failure two adjacent same-typed fields invite — cannot pass.
/// Mismatches are collected and asserted on the collection, so one failing surface
/// does not hide the other five.
#[test]
fn a_hex_colour_reaches_every_hud_surface_whose_producer_flattened() {
    let mut missing = Vec::new();
    for (name, hex, geo) in hex_surfaces() {
        let ink = opaque_ink(&geo);
        if bbox(&ink, unpack(hex)).is_none() {
            missing.push(format!(
                "{name}: #{hex:06x} {:?} never reached a vertex; emitted {:?}",
                unpack(hex),
                distinct(&ink)
            ));
        }
    }
    assert!(missing.is_empty(), "{missing:#?}");
}

/// **Negative control, executed.** The same six surfaces with the colour stripped
/// must produce none of the six hex values.
///
/// Without this, the gate above is satisfied by a detector that reports any colour
/// present — and, more to the point here, by a renderer that happened to paint one
/// of these values for an unrelated reason. Each control frame still has to *draw*,
/// or it would fail for the wrong reason.
#[test]
fn control_uncoloured_spans_reach_no_hex_on_any_hud_surface() {
    let coloured = hex_surfaces();
    let mut wrong = Vec::new();
    for ((name, hex, _), (_, _, geo)) in coloured.iter().zip(hex_surfaces_uncoloured()) {
        let ink = opaque_ink(&geo);
        if ink.is_empty() {
            wrong.push(format!(
                "{name}: the control frame drew nothing at all, so it would pass for \
                 any renderer"
            ));
            continue;
        }
        if bbox(&ink, unpack(*hex)).is_some() {
            wrong.push(format!(
                "{name}: uncoloured spans still produced #{hex:06x}, so the subject's \
                 pass says nothing"
            ));
        }
    }
    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// The six surfaces under test, each carrying its own hex colour.
///
/// Returns owned geometry rather than frames because `HudFrame` borrows its
/// sub-views; building and consuming each frame inside this function keeps the
/// borrows local.
fn hex_surfaces() -> Vec<(&'static str, u32, HudGeometry)> {
    hex_surfaces_with(true)
}

/// The same six with `style.color` left `None` — the executed control.
fn hex_surfaces_uncoloured() -> Vec<(&'static str, u32, HudGeometry)> {
    hex_surfaces_with(false)
}

/// Six pairwise-distinct hex colours, one per surface. Deliberately not multiples
/// of `0x11` and not near any of [`VANILLA`]'s sixteen, so a fallback to a base
/// colour or a rounding to a named one would be obvious rather than plausible.
const SURFACE_HEX: [(&str, u32); 6] = [
    ("title", 0x001f_2e3d),
    ("subtitle", 0x004a_6b8c),
    ("action_bar", 0x00c4_7b19),
    ("boss_bar_title", 0x0093_2af6),
    ("tab_header", 0x0026_d17e),
    ("tab_footer", 0x00e3_5c41),
];

fn hex_surfaces_with(coloured: bool) -> Vec<(&'static str, u32, HudGeometry)> {
    use lodestone::hud::HudFrame as Frame;
    use lodestone::overlay::BossBarView;
    use lodestone::tablist::{TabListRow, TabListView, ping_sprite};

    let hex = |i: usize| -> Vec<TextSpan> {
        let colour = coloured.then(|| TextColor::Rgb(SURFACE_HEX[i].1));
        // "M" on every surface: a glyph with ink in both the vanilla font and the
        // jar-less fallback, so no arm can fail merely for being blank.
        vec![span("M", colour)]
    };
    let stats = DebugStats::default();
    // Nothing else may paint, or a stray quad could carry one of these values by
    // coincidence — the "what else already paints here" check.
    fn quiet<'a>(stats: &'a DebugStats) -> Frame<'a> {
        Frame {
            crosshair: false,
            show_debug: false,
            ..Frame::new(stats)
        }
    }

    let mut out = Vec::new();

    // Title and subtitle share one `Option` and one draw block but are two
    // separate call sites at two pose scales, which is exactly how one gets fixed
    // and the other does not.
    let frame = Frame {
        title: Some((hex(0), Some(hex(1)), 1.0)),
        ..quiet(&stats)
    };
    let geo = HudGeometry::build(&frame, W, H);
    out.push((SURFACE_HEX[0].0, SURFACE_HEX[0].1, geo));
    let frame = Frame {
        title: Some((hex(0), Some(hex(1)), 1.0)),
        ..quiet(&stats)
    };
    out.push((
        SURFACE_HEX[1].0,
        SURFACE_HEX[1].1,
        HudGeometry::build(&frame, W, H),
    ));

    let frame = Frame {
        action_bar: Some((hex(2), 1.0)),
        ..quiet(&stats)
    };
    out.push((
        SURFACE_HEX[2].0,
        SURFACE_HEX[2].1,
        HudGeometry::build(&frame, W, H),
    ));

    let bars = [BossBarView {
        title: hex(3),
        progress: 0.5,
        color: [1.0, 0.0, 1.0],
    }];
    let frame = Frame {
        boss_bars: &bars,
        ..quiet(&stats)
    };
    out.push((
        SURFACE_HEX[3].0,
        SURFACE_HEX[3].1,
        HudGeometry::build(&frame, W, H),
    ));

    // One row, so the overlay lays out as it does live; the row's own name is
    // uncoloured so it cannot supply either banner's value.
    let view = TabListView {
        rows: vec![TabListRow {
            name: plain_spans("row"),
            ping_sprite: ping_sprite(10),
            spectator: false,
        }],
        header: vec![hex(4)],
        footer: vec![hex(5)],
        show_head: false,
    };
    let frame = Frame {
        players: Some(&view),
        ..quiet(&stats)
    };
    let geo = HudGeometry::build(&frame, W, H);
    out.push((SURFACE_HEX[4].0, SURFACE_HEX[4].1, geo));
    let frame = Frame {
        players: Some(&view),
        ..quiet(&stats)
    };
    out.push((
        SURFACE_HEX[5].0,
        SURFACE_HEX[5].1,
        HudGeometry::build(&frame, W, H),
    ));

    out
}

/// Style **inheritance**: a nested component with no colour of its own must
/// render its parent's colour.
///
/// The input data is the point of this test, which is why it builds a real nested
/// [`Text`] rather than a flat span list. A suite that only ever feeds
/// single-colour flat strings is blind to inheritance by construction — the test
/// source looks exemplary and the flaw is in what it was pointed at.
#[test]
fn a_nested_component_inherits_its_parents_colour() {
    let tree = Text {
        style: TextStyle {
            color: Some(TextColor::Gold),
            ..TextStyle::default()
        },
        // The child specifies *no* colour. `None` means "inherit", which is the
        // distinction under test — `Some(false)`-style explicit values would be a
        // different case entirely.
        extra: vec![Text::literal("child")],
        ..Text::literal("parent")
    };
    let spans = tree.to_spans();

    // First, at the model layer: the child span must carry gold.
    let child = spans
        .iter()
        .find(|s| s.text == "child")
        .expect("the child run must survive flattening");
    assert_eq!(
        child.style.color,
        Some(TextColor::Gold),
        "a child with no colour of its own must inherit the parent's"
    );

    // Then, that it reaches a vertex: gold ink must exist, and the *only* named
    // colour present must be gold — if inheritance were dropped the child would
    // draw in the sidebar's base white instead.
    let geo = geometry_for_title(spans);
    let ink = opaque_ink(&geo);
    let gold = unpack(0x00ff_aa00);
    assert!(
        bbox(&ink, gold).is_some(),
        "gold never reached a vertex; emitted: {:?}",
        distinct(&ink)
    );
    let white = unpack(0x00ff_ffff);
    assert!(
        bbox(&ink, white).is_none(),
        "base white is present at {:?}, so some run lost its inherited colour",
        bbox(&ink, white)
    );
}

/// Colour is written in **gamma** space, not converted to linear.
///
/// Gold is `0xFFAA00`: red saturated, green at `170/255`. The two hypotheses put
/// the green channel in different places, and the ratio `G/R` needs no knowledge
/// of the subject's own colours to state:
///
/// * **gamma** (vanilla, correct): `G/R == 170/255 == 0.6667`.
/// * **linear** (the plausible mistake): `srgb_to_linear(0.6667) == 0.4019`, so
///   `G/R == 0.4019`.
///
/// A direction-only assertion ("green is less than red") is satisfied identically
/// by both, which is exactly the vacuous shape that shipped a 70 %-red hurt
/// overlay here where vanilla renders 30 %.
#[test]
fn gold_is_written_in_gamma_space() {
    const GAMMA: f32 = 170.0 / 255.0;
    let linear = srgb_to_linear(GAMMA);
    assert!(
        (GAMMA - linear).abs() > 0.2,
        "the two hypotheses must be far apart for this test to mean anything: \
         gamma {GAMMA}, linear {linear}"
    );

    let geo = geometry_for_title(vec![span("M", Some(TextColor::Gold))]);
    // The full-strength gold run: red exactly 1.0 rules out the shadow copy,
    // which is a quarter of every channel.
    let g = geo
        .verts
        .chunks_exact(6)
        .filter(|v| (v[5] - 1.0).abs() < 1e-6 && (v[2] - 1.0).abs() < 1e-6)
        .map(|v| v[3])
        .next()
        .expect("a full-strength gold vertex must exist");

    let d_gamma = (g - GAMMA).abs();
    let d_linear = (g - linear).abs();
    assert!(
        d_gamma < d_linear,
        "gold's green channel measured {g}: closer to the LINEAR hypothesis \
         ({linear}, d={d_linear}) than the gamma one ({GAMMA}, d={d_gamma}). \
         Text colour is being converted to linear somewhere."
    );
    assert!(
        d_gamma < 1e-5,
        "gold's green channel must be exactly {GAMMA}, measured {g}"
    );
}

/// The drop shadow is a **gamma-space** quarter, per
/// `ARGB.scaleRGB(color, 0.25F)`.
///
/// Same two-hypothesis shape as [`gold_is_written_in_gamma_space`], on the one
/// place a multiply actually happens. For a saturated channel:
///
/// * **gamma** (correct): `0.25`.
/// * **linear** (the mistake): `linear_to_srgb(srgb_to_linear(1.0) * 0.25)`
///   `== 0.5372` — a visible grey outline rather than vanilla's near-black one,
///   more than twice as bright.
///
/// ## Why this reads `shadow_of` rather than the vertex stream
///
/// This test originally looked for the shadow copy in [`HudGeometry::verts`], and
/// **it failed** — correctly, and for a reason worth recording. The drop shadow
/// only exists on the `VanillaFont` path; the jar-less fixed-advance fallback that
/// every other test in this file exercises draws no shadow at all, exactly as
/// `Builder::text_legacy`'s fallback does not. So on a machine with no
/// `client.jar` there was no shadow to find.
///
/// Gating it behind "if a jar is present" would be the *precondition* species of
/// vacuous test: it would silently assert nothing on the machines that most need
/// checking. Instead this asserts the pure function that decides the value, which
/// is available unconditionally, and the *on-screen* shadow stays gated by
/// `tests/vanilla_font_pixels.rs` — which renders to a deliberately
/// **`Rgba8Unorm`** target so the floats land verbatim, and which asserts
/// `HudRenderer::font_attached` before measuring anything so a missing jar is a
/// failure there rather than a silent degrade.
#[test]
fn shadow_is_a_gamma_space_quarter() {
    const GAMMA: f32 = 0.25;
    let linear = linear_to_srgb(srgb_to_linear(1.0) * 0.25);
    assert!(
        (GAMMA - linear).abs() > 0.2,
        "hypotheses too close to discriminate: gamma {GAMMA}, linear {linear}"
    );

    // White at full alpha: every channel saturated, so the scale is visible in
    // each and alpha preservation is checkable in the same call.
    let s = lodestone::hud::vanilla_font::shadow_of([1.0, 1.0, 1.0, 1.0]);
    for (i, channel) in s[..3].iter().enumerate() {
        let d_gamma = (channel - GAMMA).abs();
        let d_linear = (channel - linear).abs();
        assert!(
            d_gamma < d_linear,
            "shadow channel {i} measured {channel}: closer to the LINEAR \
             hypothesis ({linear}, d={d_linear}) than the gamma one ({GAMMA}, \
             d={d_gamma}). The shadow scale is being applied in linear space."
        );
        assert!(
            d_gamma < 1e-6,
            "shadow channel {i} must be exactly {GAMMA}, measured {channel}"
        );
    }
    assert!(
        (s[3] - 1.0).abs() < 1e-6,
        "alpha must be preserved, not scaled: ARGB.scaleRGB keeps alpha(color); \
         measured {}",
        s[3]
    );

    // And that the quarter is taken on the colour as given, for a channel that is
    // not 1.0 — where a linear round trip and a plain multiply differ by more than
    // a constant factor. Gold's green is 170/255; a gamma quarter is 42.5/255.
    let gold = lodestone::hud::vanilla_font::shadow_of([1.0, 170.0 / 255.0, 0.0, 1.0]);
    let want = (170.0 / 255.0) * 0.25;
    assert!(
        (gold[1] - want).abs() < 1e-6,
        "gold's shadow green must be {want}, measured {}",
        gold[1]
    );
}

/// sRGB transfer function, for stating the *wrong* hypothesis explicitly.
fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.040_45 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

fn linear_to_srgb(c: f32) -> f32 {
    if c <= 0.003_130_8 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// A `NumberFormat::Styled` score must carry the server's colour rather than the
/// HUD's default red.
///
/// The old code matched this variant as `Styled(_)` next to `Default`, binding
/// the colour to a wildcard and discarding it — the one thing that variant exists
/// to express.
#[test]
fn a_styled_score_number_carries_the_servers_colour() {
    use lodestone_game::scoreboard::{DisplaySlot, NumberFormat, Objective, Scoreboard};

    let mut scoreboard = Scoreboard::new();
    scoreboard.add_objective(Objective::new("obj", "", Text::literal("Obj")));
    scoreboard.set_display(DisplaySlot::Sidebar, Some("obj"));
    scoreboard.set_score_entry(
        "obj",
        "Alice",
        lodestone_game::scoreboard::ScoreEntry {
            value: 42,
            display_name: None,
            number_format: NumberFormat::Styled(TextColor::Aqua),
        },
    );

    let side = lodestone::scoreboard::sidebar_from(&scoreboard, &|_: &str| None)
        .expect("sidebar visible");
    let score = &side.lines[0].score;
    assert_eq!(
        score.iter().map(|s| s.text.as_str()).collect::<String>(),
        "42",
        "the score's wording must survive"
    );
    assert_eq!(
        score[0].style.color,
        Some(TextColor::Aqua),
        "NumberFormat::Styled's colour must reach the view, not be dropped"
    );

    // And that it beats the HUD's hardcoded red rather than merely existing.
    let stats = DebugStats::default();
    let frame = HudFrame {
        sidebar: Some(&side),
        crosshair: false,
        show_debug: false,
        ..HudFrame::new(&stats)
    };
    let ink = opaque_ink(&HudGeometry::build(&frame, W, H));
    let aqua = unpack(0x0055_ffff);
    assert!(
        bbox(&ink, aqua).is_some(),
        "aqua never reached a vertex; emitted: {:?}",
        distinct(&ink)
    );
}

/// A server-list MOTD must keep its colour through the status decoder, in both
/// conventions a real `description` arrives in.
#[test]
fn motd_keeps_colour_from_json_and_from_legacy_codes() {
    // Modern: component `color` keys, including a nested child that inherits.
    //
    // `r##"…"##`, not `r#"…"#`: a hex colour is written `"#1f2e3d"`, and the `"#`
    // inside it terminates a single-hash raw string. The resulting errors point at
    // the JSON *after* the break and say nothing about quoting.
    let modern = r##"{"text":"","extra":[
        {"color":"gold","text":"Lode","extra":[{"text":"stone"}]},
        {"color":"#1f2e3d","text":" hex"}
    ]}"##;
    let s = lodestone_net::parse_status_json(
        &format!(r#"{{"description":{modern}}}"#),
        None,
    )
    .expect("valid status json");
    assert_eq!(s.motd, "Lodestone hex", "the wording must survive");
    let colours: Vec<Option<TextColor>> = s.motd_spans.iter().map(|x| x.style.color).collect();
    assert!(
        colours.contains(&Some(TextColor::Gold)),
        "the gold run must survive the decoder, got {colours:?}"
    );
    assert!(
        colours.contains(&Some(TextColor::Rgb(0x001f_2e3d))),
        "the hex run must survive the decoder, got {colours:?}"
    );
    // Inheritance: "stone" is a child of the gold node with no colour of its own.
    let stone = s
        .motd_spans
        .iter()
        .find(|x| x.text == "stone")
        .expect("the nested child run must survive");
    assert_eq!(
        stone.style.color,
        Some(TextColor::Gold),
        "a nested MOTD run must inherit its parent's colour"
    );

    // Legacy: a bare string full of `§` codes, which is what a great many real
    // servers send. The old decoder deleted every one of these pairs.
    let legacy = lodestone_net::parse_status_json(
        "{\"description\":\"\u{a7}cRed \u{a7}9Blue\"}",
        None,
    )
    .expect("valid status json");
    assert_eq!(
        legacy.motd, "Red Blue",
        "the plain form must still be code-free"
    );
    let legacy_colours: Vec<Option<TextColor>> =
        legacy.motd_spans.iter().map(|x| x.style.color).collect();
    assert!(
        legacy_colours.contains(&Some(TextColor::Red))
            && legacy_colours.contains(&Some(TextColor::Blue)),
        "legacy §-coded MOTD colours must survive, got {legacy_colours:?}"
    );

    // Control: the same words with no colour at all must yield no colours, so the
    // two assertions above are not satisfied by a decoder that invents them.
    let plain =
        lodestone_net::parse_status_json(r#"{"description":"Red Blue"}"#, None)
            .expect("valid status json");
    assert!(
        plain.motd_spans.iter().all(|x| x.style.color.is_none()),
        "an uncoloured MOTD must produce uncoloured spans"
    );
}
