// -- the scrolling-list clip, in the router ----------------------------------
//
// A framebuffer whose **auto** GUI scale is exactly 1, so a logical pixel is a
// physical pixel and the vanilla-derived coordinates below need no conversion.
//
// `calculate_gui_scale`'s loop stops when `fb / (scale + 1)` drops below
// `320x240`, and `479 / 2 == 239 < 240` — so this is 854x479 rather than the
// 854x480 reference canvas every hermetic geometry test uses, precisely because
// 480 would auto-scale to 2. Asserted in the tests rather than assumed.

//! Tests for server-list layout, join hit testing, and display scaling.

use super::*;

const LIST_FB_W: u32 = 854;
const LIST_FB_H: u32 = 479;

/// The *Join Server* button's rect, hand-derived from vanilla's own numbers.
///
/// Nothing in here is read back out of our arranged layout — that is the point.
/// Vanilla's own multiplayer-screen init builds a header-and-footer layout with
/// a 33 px header and a 60 px footer,
/// and fills the footer with a vertical stack (4 px spacing) holding two
/// horizontal rows (4 px spacing): three 100 px-wide
/// buttons, then four 74 px-wide ones. Every button is
/// 20 px tall, and the footer frame
/// aligns its child at `(0.5, 0.5)`.
///
/// So: the child column measures `3*100 + 2*4 == 308` wide (the lower row is
/// `4*74 + 3*4 == 308` too) and `20 + 4 + 20 == 44` tall; the footer band is the
/// bottom 60 px; centring 44 in 60 puts the column 8 px below the band's top, and
/// centring 308 in the canvas puts it at `(854 - 308) / 2`. *Join Server* is the
/// top row's first cell.
///
/// `the_derived_join_server_rect_is_the_one_the_layout_arranges` checks this
/// against `server_list_footer_slot`, so a divergence is a named failure rather
/// than a silently wrong probe point.
fn join_server_rect() -> (f32, f32, f32, f32) {
    let w = LIST_FB_W as f32;
    let h = LIST_FB_H as f32;
    let footer_band_top = h - SERVER_LIST_FOOTER_H_VANILLA;
    let column_h = 20.0 + 4.0 + 20.0;
    let column_w = 3.0 * 100.0 + 2.0 * 4.0;
    (
        ((w - column_w) / 2.0).floor(),
        footer_band_top + ((SERVER_LIST_FOOTER_H_VANILLA - column_h) / 2.0).floor(),
        100.0,
        20.0,
    )
}
/// Vanilla's own header-and-footer layout's footer height, and the same value
/// `server_row_visible` subtracts from the canvas to find the list's bottom.
const SERVER_LIST_FOOTER_H_VANILLA: f32 = 60.0;
/// The same layout's header height, which is where the content band starts (the
/// content frame is clamped up against the footer, so it never gets the 30 px
/// preferred gap on this screen).
const SERVER_LIST_HEADER_H_VANILLA: f32 = 33.0;
/// Vanilla's own server-selection list row height.
const SERVER_LIST_ITEM_H_VANILLA: f32 = 36.0;
/// Vanilla's own selection-list first-entry offset: the list's own y plus 2.
const SERVER_LIST_FIRST_ENTRY_Y_VANILLA: f32 = 2.0;

/// A `WindowApp` on the multiplayer screen with `n` servers in the list, added
/// through the real Add Server form.
///
/// The nav is replaced with one pointing at a throwaway directory **before** any
/// server is added: `MenuNav::new` reads and writes the player's own
/// `servers.json`, and a test that added rows through it would rewrite the real
/// file. Hostnames are RFC 2606 `.invalid`, so nothing here can resolve even if a
/// probe were ever spawned.
fn app_with_servers(tag: &str, n: usize) -> WindowApp {
    use crate::menu::nav::{MenuKey, MenuNav};

    let mut app = WindowApp::new(Config {
        mode: Mode::Headless,
        ..Config::default()
    });
    let dir = std::env::temp_dir().join(format!(
        "lodestone-listclip-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    seed_owning_account(&dir);
    app.nav = MenuNav::with_path(dir.join("servers.json"));
    app.ui.open_server_list();
    for i in 0..n {
        app.nav.key(&mut app.ui, MenuKey::Char('a'));
        for c in format!("S{i}").chars() {
            app.nav.key(&mut app.ui, MenuKey::Char(c));
        }
        app.nav.key(&mut app.ui, MenuKey::Tab);
        for c in format!("h{i}.invalid").chars() {
            app.nav.key(&mut app.ui, MenuKey::Char(c));
        }
        app.nav.key(&mut app.ui, MenuKey::Enter);
    }
    assert_eq!(
        app.ui.screen(),
        crate::menu::Screen::ServerList,
        "premise: the multiplayer screen is up after adding {n} servers"
    );
    assert_eq!(app.nav.list().len(), n, "premise: {n} rows are in the list");
    // Adding leaves the cursor on the row just created, which scrolled the list to
    // show it. Wind it back to the top so the sweep below starts at 0.
    app.nav
        .scroll_server_list(1000.0, LIST_FB_H as f32);
    assert_eq!(
        app.nav.server_scroll(),
        0.0,
        "premise: the sweep starts from an unscrolled list"
    );
    app
}

/// The hand derivation above is the rect the screen really arranges.
///
/// Without this the probe point in the sweep could be inside no widget at all,
/// and "the hit-test answers the footer button" would be untestable in the
/// direction that matters. Kept separate from the sweep so a layout change
/// reports itself here by name instead of as a mysterious routing failure.
#[test]
fn the_derived_join_server_rect_is_the_one_the_layout_arranges() {
    use crate::menu::nav::ServerListButton;

    let arranged = crate::menu::render::server_list_footer_slot(ServerListButton::Select)
        .resolve(LIST_FB_W as f32, LIST_FB_H as f32);
    assert_eq!(
        arranged,
        join_server_rect(),
        "the arranged Join Server rect and the one derived from \
         JoinMultiplayerScreen's own constants disagree"
    );
}

/// **A scrolling list must not steal the footer's clicks — or its hover.**
///
/// The player report (2026-08-07): *"on the server list ui … if i try to press
/// 'Join Server' when im not scrolled to the bottom (ie there is a server behind
/// the button) it doesnt highlight the 'Join Server' button (despite my cursor
/// being on it) and instead presses the server entry"*. Hover and click are one
/// path — [`WindowApp::menu_row_at`] answers both — so one assertion covers both
/// symptoms.
///
/// # The invariant, and why it is swept
///
/// For a cursor inside a footer button's own rect, the hit-test must return that
/// button **at every scroll offset of the list**. The bug only exists at offsets
/// where a row has scrolled under the footer: at 0 the first row is at the band's
/// top, and at the maximum the last row ends inside the band, so a gate that
/// probed either end would pass against the broken code. That is `CLAUDE.md`'s
/// *world* species — the input cannot exercise the defect — and it is the specific
/// trap that would let this ship again, which is why the sweep walks every
/// pixel offset rather than sampling.
///
/// # Why the expected value is not round-tripped
///
/// The probe point comes from [`join_server_rect`], hand-derived from
/// screen layout constants, and the expected answer is the
/// row index `SERVER_LIST_BUTTONS`' order fixes — `n + 0`, since Join Server is
/// its first entry and the footer follows the entries in one flat index space.
/// Asking `row_rect` where the button is and clicking there would pass for any
/// self-consistent geometry, including one that never routes to a footer button
/// at all.
///
/// # The premise that makes the sweep non-vacuous, measured rather than assumed
///
/// At least one offset in the sweep must put a *list row's* rect over the probe
/// point — otherwise there is nothing for the row to steal and the assertion is
/// satisfied by geometry that never overlaps. That is checked with the same
/// `row_rect` the broken scan read, and its count is reported on failure.
#[test]
fn the_join_server_button_wins_the_cursor_at_every_scroll_offset() {
    use crate::menu::nav::SERVER_LIST_BUTTONS;

    assert_eq!(
        crate::config::calculate_gui_scale(0, LIST_FB_W, LIST_FB_H),
        1,
        "premise: at this framebuffer a logical pixel is a physical pixel, so the \
         vanilla-derived coordinates need no scale conversion"
    );
    assert_eq!(
        SERVER_LIST_BUTTONS[0],
        crate::menu::nav::ServerListButton::Select,
        "premise: Join Server is the first footer row, so its index is `n + 0`"
    );

    // 16 rows overflow the 386 px band at this canvas by 190 px, which is what
    // makes the offsets in the middle of the sweep reachable at all.
    const N: usize = 16;
    let mut app = app_with_servers("join-sweep", N);
    let expected = Some(N);

    let (bx, by, bw, bh) = join_server_rect();
    let (probe_x, probe_y) = (bx + bw * 0.5, by + bh * 0.5);
    let band_bottom = LIST_FB_H as f32 - SERVER_LIST_FOOTER_H_VANILLA;
    assert!(
        probe_y > band_bottom,
        "premise: the probe at y {probe_y} must be below the list band's bottom \
         ({band_bottom}) — otherwise it is inside the list and the footer has no \
         claim on it"
    );

    let max_scroll =
        crate::menu::render::server_list_max_scroll(N, LIST_FB_H as f32);
    assert!(
        max_scroll >= 30.0,
        "premise: the list must scroll at least 30 px for a row to reach the \
         footer strip; it scrolls {max_scroll}"
    );

    // How many offsets put a row's rect over the probe point. This is the
    // detector's own evidence: a zero here means the sweep measured nothing.
    let mut offsets_with_a_row_under_the_button = 0usize;
    let mut steps = 0usize;
    loop {
        let scroll = app.nav.server_scroll();
        let frame = crate::menu::nav::on_screen_frame(
            &app.ui,
            &app.nav,
            app.sim.death_message(),
            &app.statuses,
            &mut app.favicons,
        )
        .expect("the multiplayer screen owns its frame");
        let row_under = (0..N).any(|i| {
            crate::menu::render::row_rect(
                &frame.rows,
                i,
                LIST_FB_W as f32,
                LIST_FB_H as f32,
            )
            .is_some_and(|(rx, ry, rw, rh)| {
                probe_x >= rx
                    && probe_x <= rx + rw
                    && probe_y >= ry
                    && probe_y <= ry + rh
            })
        });
        if row_under {
            offsets_with_a_row_under_the_button += 1;
        }
        drop(frame);

        assert_eq!(
            app.menu_row_at_in(probe_x, probe_y, LIST_FB_W, LIST_FB_H),
            expected,
            "at scroll {scroll} px a cursor at ({probe_x}, {probe_y}) — inside Join \
             Server's own rect — resolved to something else. A list row is \
             overhanging the band and winning the flat index scan, which is both \
             the missing highlight and the wrong press."
        );

        if scroll >= max_scroll {
            break;
        }
        // One logical pixel: `mouse_scrolled` moves `notches * scroll_rate` and the
        // rate is half a 36 px entry. Negative scrolls *down*, matching winit.
        app.nav
            .scroll_server_list(-1.0 / 18.0, LIST_FB_H as f32);
        steps += 1;
        assert!(
            steps < 4000,
            "the sweep is not advancing: stuck at {scroll} of {max_scroll}"
        );
    }

    assert!(
        offsets_with_a_row_under_the_button > 0,
        "premise failed: no scroll offset in the sweep put a list row's rect over \
         Join Server, so the assertion above measured nothing at all. The band \
         bottom is {band_bottom} and the probe is at y {probe_y}."
    );
}

/// **The control for the sweep above, run and observed.**
///
/// Two ways that gate could pass for the wrong reason, and both are the shape
/// `CLAUDE.md` warns about — an absence assertion with no evidence the detector
/// fires, and a control whose premise is false before the fix existed:
///
/// 1. If the clip rejected list rows *everywhere* rather than outside the band,
///    "the footer wins below the band" would hold while the list itself became
///    unclickable. So a cursor genuinely **inside** the band, over a drawn row,
///    must return that row and not a footer button.
/// 2. If `menu_row_at_in` answered `None` on this screen, every equality in the
///    sweep would be comparing two `None`s. It does not: both probes here answer
///    `Some`, from the same framebuffer, differing only in `y`.
///
/// **Observed to fail against an inverted fix**: dropping the `ly < top || ly >
/// bottom` bound from `menu_row_at_in`'s guard — i.e. rejecting a list row at
/// every position — leaves the sweep green and makes this test fail with
/// `None` where row 0 is expected. Restoring the bound makes both pass. That is
/// the polarity a control needs, and it is why the two live side by side.
#[test]
fn a_cursor_inside_the_band_still_resolves_to_the_list_row_under_it() {
    const N: usize = 16;
    let mut app = app_with_servers("band-control", N);

    // Row 0's centre at scroll 0, from the layout arithmetic:
    // the first-entry offset plus the row index times row height below the
    // content band's top, with the fixed 305 px row width centred horizontally.
    let row_top = SERVER_LIST_HEADER_H_VANILLA + SERVER_LIST_FIRST_ENTRY_Y_VANILLA;
    let probe_y = row_top + SERVER_LIST_ITEM_H_VANILLA * 0.5;
    let row_w: f32 = 305.0;
    let row_left = (LIST_FB_W as f32 * 0.5).floor() - (row_w * 0.5).floor();
    let probe_x = row_left + row_w * 0.5;

    let band_bottom = LIST_FB_H as f32 - SERVER_LIST_FOOTER_H_VANILLA;
    assert!(
        probe_y > SERVER_LIST_HEADER_H_VANILLA && probe_y < band_bottom,
        "premise: {probe_y} must be inside the band {SERVER_LIST_HEADER_H_VANILLA}\
         ..{band_bottom}, or this control is measuring the footer strip again"
    );

    assert_eq!(
        app.menu_row_at_in(probe_x, probe_y, LIST_FB_W, LIST_FB_H),
        Some(0),
        "a cursor over row 0, inside the list's band, must resolve to row 0 — a \
         clip that rejected every list row would satisfy the footer gate and \
         break the list"
    );

    // And the second premise: the two answers differ, from one framebuffer, so
    // the sweep above is not comparing two `None`s.
    let (bx, by, bw, bh) = join_server_rect();
    assert_eq!(
        app.menu_row_at_in(bx + bw * 0.5, by + bh * 0.5, LIST_FB_W, LIST_FB_H),
        Some(N),
        "premise: the footer probe answers the footer row at scroll 0 as well"
    );
}

/// [`crate::app::lifecycle::dpr_scaled_size`] — the pure CSS-box-times-`devicePixelRatio`
/// arithmetic behind the browser build's initial canvas sizing (see that function's doc and
/// `finish_bring_up`'s call site). Deliberately not `#[cfg(target_arch = "wasm32")]`: the
/// DOM-touching caller is, so this is the only part of that fix a native `cargo test` run
/// ever exercises — a test living inside the `wasm32`-gated function itself would never run
/// under any check `just health` performs.
///
/// Every case predicts an exact `(width, height)` from outside the function under test
/// (plain `f64` arithmetic on the input), collects every mismatch rather than asserting
/// inside the loop (CLAUDE.md's *magnitude* species — a `for`-loop `assert!` would only ever
/// report the first failing case), and every input is pairwise-distinct so a transposed
/// width/height cannot survive unnoticed.
#[test]
fn dpr_scaled_size_matches_predicted_physical_pixels() {
    use crate::app::lifecycle::dpr_scaled_size;

    struct Case {
        label: &'static str,
        client_width: i32,
        client_height: i32,
        dpr: f64,
        expected: Option<(u32, u32)>,
    }

    let cases = [
        // DPR 1 anchor: no scaling, so this alone cannot tell "scaled" from
        // "unscaled" — the other cases below carry that weight.
        Case {
            label: "dpr 1.0, exact",
            client_width: 900,
            client_height: 600,
            dpr: 1.0,
            expected: Some((900, 600)),
        },
        // The "4x fragment count at retina" case named in `dpr_scaled_size`'s
        // caller's doc: dpr 2.0 doubles *both* dimensions, so the fragment count
        // — width * height — is 4x, not 2x.
        Case {
            label: "dpr 2.0, retina",
            client_width: 960,
            client_height: 540,
            dpr: 2.0,
            expected: Some((1920, 1080)),
        },
        // A real iPhone-class dpr (3.0), and large enough that a `u32` truncation
        // bug in an intermediate would be visible.
        Case {
            label: "dpr 3.0, phone-class",
            client_width: 375,
            client_height: 812,
            dpr: 3.0,
            expected: Some((1125, 2436)),
        },
        // The rounding discriminator: 667 * 1.5 = 1000.5 and 421 * 1.5 = 631.5,
        // both exact half-pixels. `.round()` (round-half-away-from-zero) gives
        // 1001 and 632; a truncating cast (`as u32` with no `.round()` — the
        // plausible neuter for this function, and a real bug shape: forgetting
        // the round and letting the float-to-int cast truncate) would instead
        // give 1000 and 631. The two hypotheses differ on both fields, so a
        // width/height transposition could not accidentally pass this either.
        Case {
            label: "dpr 1.5, half-pixel rounding",
            client_width: 667,
            client_height: 421,
            dpr: 1.5,
            expected: Some((1001, 632)),
        },
        // Zero-guard, width: `client_width` reads 0 when the canvas has not been
        // laid out yet (or is `display: none`) — must not resize the surface to
        // a degenerate 0-wide target.
        Case {
            label: "zero width guards to None",
            client_width: 0,
            client_height: 600,
            dpr: 1.0,
            expected: None,
        },
        // Zero-guard, height — a non-1.0 dpr here so this is not merely the
        // same guard exercised twice under the same scale factor.
        Case {
            label: "zero height guards to None",
            client_width: 900,
            client_height: 0,
            dpr: 2.0,
            expected: None,
        },
        // A negative `client_width` is a real (if pathological) input shape the
        // DOM can hand back; the guard compares the unscaled `f64` product
        // against `1.0` *before* any cast, so this must not become a huge
        // `u32` via a wrapping negative-float-to-unsigned cast.
        Case {
            label: "negative width guards to None",
            client_width: -5,
            client_height: 600,
            dpr: 1.0,
            expected: None,
        },
    ];

    let mismatches: Vec<String> = cases
        .iter()
        .filter_map(|case| {
            let actual = dpr_scaled_size(case.client_width, case.client_height, case.dpr);
            (actual != case.expected).then(|| {
                format!(
                    "{}: dpr_scaled_size({}, {}, {}) = {:?}, expected {:?}",
                    case.label, case.client_width, case.client_height, case.dpr, actual, case.expected
                )
            })
        })
        .collect();

    assert!(
        mismatches.is_empty(),
        "{} of {} cases mismatched:\n{}",
        mismatches.len(),
        cases.len(),
        mismatches.join("\n")
    );
}
