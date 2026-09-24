use super::*;


// ---------------------------------------------------------------------------
// The disconnect screen keeps the server's own colours
// ---------------------------------------------------------------------------

/// A kicked player's reason is a styled component, and both the frame and the
/// draw must keep its colours.
///
/// # The input is discriminating three ways
///
/// The root carries `gold`, one `extra` child overrides it with `red`, and a
/// second child specifies nothing and must therefore inherit `gold`. Three
/// different readings of inheritance give three different answers:
///
/// | reading | spans |
/// |---|---|
/// | correct: a child inherits what it does not override | `[Red, Gold]` |
/// | "the root's colour wins" | `[Gold, Gold]` |
/// | "no inheritance; an unstyled child is unstyled" | `[Red, None]` |
///
/// One colour cannot tell those apart, which is why there are two children with
/// different styling rather than one coloured child.
///
/// The root's own `text` is `""` with all the content in `extra` — the shape a
/// real server's kick message takes, and the shape most likely to expose a
/// root-only reader. Nothing on the disconnect path exercised a nested `extra`
/// at all before this: the corpus covered it on the *MOTD* path
/// (`tests/text_colour.rs`) and on the server *encode* path
/// (`v770`'s `server_disconnect`), and the shell's own disconnect fixtures were
/// every one of them a flat single component.
///
/// # Expected values
///
/// Transcribed from the jar's `TextColor` decimal table by hand, not read from
/// `TextColor::rgb` — calling that would make this `decode(encode(x))`, green
/// under any self-consistent misunderstanding. `named("gold", 16755200)` is
/// `0xffaa00`; `named("red", 16733525)` is `0xff5555`.
#[test]
fn a_kick_reason_keeps_the_server_s_colours_through_frame_and_draw() {
    use lodestone_model::{Text, TextColor};

    const GOLD: u32 = 0x00ff_aa00;
    const RED: u32 = 0x00ff_5555;

    let reason = Text::from_json(
        r#"{"text":"","color":"gold","extra":[
            {"text":"kick","color":"red"},
            {"text":" reason"}
        ]}"#,
    );
    let mut ui = UiState::new();
    ui.begin(crate::menu::SessionKind::Multiplayer);
    ui.session_failed(crate::sim::SessionEnd::disconnected(reason.resolve(&|_| None)));
    assert_eq!(ui.screen(), Screen::Error);

    let frame = frame_for(
            &ui,
            &MenuNav::new(),
            &StatusCache::new(),
            &mut FaviconCache::new(),
        )
        .expect("the error screen owns a frame");
    let notice = frame.notice.as_ref().expect("the reason is a notice");

    assert_eq!(
        notice.text, "kick reason",
        "the wording must survive, and with no prefix of ours glued on"
    );
    assert!(
        !notice.text.contains("disconnected"),
        "the `disconnected: ` prefix was ours, not vanilla's — a DisconnectedScreen \
         puts its title in a separate widget above the reason: {}",
        notice.text
    );

    // Collected, not asserted in a loop: an `assert_eq!` per span aborts at the
    // first mismatch, so the "inherits" and "overrides" halves would never both
    // be reported.
    let got: Vec<Option<u32>> = notice
        .spans
        .iter()
        .map(|s| s.style.color.as_ref().map(TextColor::rgb))
        .collect();
    assert_eq!(
        got,
        vec![Some(RED), Some(GOLD)],
        "the overriding child must be red and the bare child must inherit the \
         root's gold; got {got:?} for spans {:?}",
        notice.spans
    );

    // And the draw actually uses them. `build` with no font takes
    // `text_spans`' fixed-advance fallback, which still emits one coloured quad
    // per glyph into the colour stream — so a red quad and a gold quad must both
    // be present. The single-colour text path would otherwise draw through
    // `b.text` with `FG_BAD`, so neither colour could appear.
    let geometry = build(&frame, None, None, V_W, V_H);
    let has = |rgb: u32| {
        let want = [
            ((rgb >> 16) & 0xff) as f32 / 255.0,
            ((rgb >> 8) & 0xff) as f32 / 255.0,
            (rgb & 0xff) as f32 / 255.0,
        ];
        geometry.colour.chunks_exact(STRIDE).any(|v| {
            (v[2] - want[0]).abs() < 1.0 / 255.0
                && (v[3] - want[1]).abs() < 1.0 / 255.0
                && (v[4] - want[2]).abs() < 1.0 / 255.0
        })
    };
    let mut missing = Vec::new();
    if !has(RED) {
        missing.push("red");
    }
    if !has(GOLD) {
        missing.push("gold");
    }
    assert!(
        missing.is_empty(),
        "the draw dropped these colours: {missing:?}"
    );
}
/// The control for the test above: a reason with **no** colour must draw in
/// `FG_BAD` and produce no gold or red quad, so the two `has(...)` assertions
/// there are not satisfied by a draw that emits every colour, or by an atlas
/// sprite that happens to be gold.
///
/// It also pins the fallback that every notice this shell authors itself relies
/// on — an unstyled notice must not change appearance because the styled path
/// exists.
#[test]
fn an_uncoloured_kick_reason_still_draws_in_the_error_colour() {
    let mut ui = UiState::new();
    ui.begin(crate::menu::SessionKind::Multiplayer);
    ui.session_failed(crate::sim::SessionEnd::disconnected(lodestone_model::ResolvedText::literal(
        "kick reason",
    )));
    let frame = frame_for(
            &ui,
            &MenuNav::new(),
            &StatusCache::new(),
            &mut FaviconCache::new(),
        )
        .expect("the error screen owns a frame");
    let notice = frame.notice.as_ref().expect("the reason is a notice");
    assert!(
        notice.spans.iter().all(|s| s.style.color.is_none()),
        "a literal with no style must produce no colours: {:?}",
        notice.spans
    );

    let geometry = build(&frame, None, None, V_W, V_H);
    let gold_quads = geometry
        .colour
        .chunks_exact(STRIDE)
        .filter(|v| {
            (v[2] - 1.0).abs() < 1.0 / 255.0
                && (v[3] - 170.0 / 255.0).abs() < 1.0 / 255.0
                && v[4].abs() < 1.0 / 255.0
        })
        .count();
    assert_eq!(
        gold_quads, 0,
        "an uncoloured reason must emit no gold quads — otherwise the styled \
         test's assertions prove nothing"
    );
    // The error colour is what it draws instead, so the block is still visible.
    let bad_quads = geometry
        .colour
        .chunks_exact(STRIDE)
        .filter(|v| {
            (v[2] - FG_BAD[0]).abs() < 1.0 / 255.0
                && (v[3] - FG_BAD[1]).abs() < 1.0 / 255.0
                && (v[4] - FG_BAD[2]).abs() < 1.0 / 255.0
        })
        .count();
    assert!(
        bad_quads > 0,
        "an uncoloured reason must still draw, in the error colour"
    );
}

/// A client-side failure and a server disconnect get vanilla's two different
/// titles, from [`crate::sim::SessionEndKind`] rather than from the wording.
///
/// Vanilla's own disconnected-screen type takes its `title` as a constructor argument;
/// vanilla's own common packet-listener's on-disconnect handler passes `disconnect.lost`
/// ("Connection Lost") and both its own handshake packet-listener's on-disconnect handler
/// and its own connect-screen's failed-title site pass `connect.failed` ("Failed to
/// connect to the server"). The English strings are vanilla's own `en_us.json`
/// entries for those keys, transcribed by hand.
#[test]
fn the_error_screen_titles_a_failure_differently_from_a_disconnect() {
    let title_for = |end: crate::sim::SessionEnd| {
        let mut ui = UiState::new();
        ui.begin(crate::menu::SessionKind::Multiplayer);
        ui.session_failed(end);
        let frame = frame_for(
            &ui,
            &MenuNav::new(),
            &StatusCache::new(),
            &mut FaviconCache::new(),
        )
            .expect("the error screen owns a frame");
        frame.labels[0].text.clone()
    };

    let mut wrong = Vec::new();
    let disconnected = title_for(crate::sim::SessionEnd::disconnected(lodestone_model::ResolvedText::literal("bye")));
    if disconnected != "Connection Lost" {
        wrong.push(format!("disconnected: {disconnected}"));
    }
    let failed = title_for(crate::sim::SessionEnd::failed(lodestone_model::ResolvedText::literal(
        "connect: connection refused",
    )));
    if failed != "Failed to connect to the server" {
        wrong.push(format!("failed: {failed}"));
    }
    assert!(missing_titles_empty(&wrong), "wrong titles: {wrong:?}");
}

/// The loading screen declares the panorama backdrop, the three in-world screens
/// declare the dim one, and the two decisions the old `overlay: bool` fused are
/// now independent.
///
/// # What this can and cannot see
///
/// It can see the **declaration** and the **backdrop quad's colour**, which is
/// exactly the coupling that produced the bug: one flag chose the quad colour
/// *and* suppressed the panorama, so no screen could ask for a wash over the sky.
/// With the two separated, `Panorama` keeps the *opaque* quad — its
/// no-panorama fallback — while `Dim` keeps the translucent one.
///
/// It **cannot** see whether the panorama reaches a pixel. `build` is pure and
/// emits the backdrop quad unconditionally; the decision to skip those vertices
/// and draw the cubemap instead lives in `MenuRenderer::draw`, which needs a GPU.
/// So this is the declaration half only, and
/// `menu_panorama_pixels::the_loading_screen_draws_the_panorama_under_the_menu_background_wash`
/// is the half that measures pixels. Do not read a green run here as evidence the
/// sky draws.
#[test]
fn the_loading_screen_asks_for_the_panorama_and_the_in_world_screens_do_not() {
    let mut wrong: Vec<String> = Vec::new();

    let loading = loading_frame("Joining world...");
    if loading.backdrop != MenuBackdrop::Panorama {
        wrong.push(format!("loading_frame: {:?}", loading.backdrop));
    }
    let bar = loading_frame_with_progress(
        "Loading terrain",
        crate::menu::loading::TerrainProgress {
            loaded: 3,
            expected: 9,
        },
    );
    if bar.backdrop != MenuBackdrop::Panorama {
        wrong.push(format!("loading_frame_with_progress: {:?}", bar.backdrop));
    }

    let mut nav = test_nav("backdrop-decl");
    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.pause();
    nav.hover(&ui, 0);
    if pause_frame(&nav).backdrop != MenuBackdrop::Dim {
        wrong.push(format!("pause_frame: {:?}", pause_frame(&nav).backdrop));
    }
    if death_frame(&nav, None).backdrop != MenuBackdrop::DeathGradient {
        wrong.push(format!("death_frame: {:?}", death_frame(&nav, None).backdrop));
    }

    // The two questions the old boolean answered with one bit, now independent.
    if MenuBackdrop::Panorama.is_translucent() {
        wrong.push(
            "MenuBackdrop::Panorama must keep the OPAQUE quad: that quad is the              no-panorama fallback, and a translucent one over a Clear pass is the              flat-fill the loading screen used to draw"
                .to_owned(),
        );
    }
    if !MenuBackdrop::Panorama.wants_panorama() {
        wrong.push("Panorama must want the panorama".to_owned());
    }
    if MenuBackdrop::Dim.wants_panorama() {
        wrong.push(
            "Dim must NOT want the panorama — it exists to leave a live world              visible, which a cubemap covering every pixel would not"
                .to_owned(),
        );
    }
    if !MenuBackdrop::Dim.is_translucent() {
        wrong.push("Dim must be translucent".to_owned());
    }

    // And the quad's colour follows the enum, read straight out of the first
    // vertex rather than through a vertex-sampling coverage helper — a probe that
    // counts vertices inside a rect is blind to a quad that *encloses* it, which
    // is precisely the shape of a full-screen backdrop.
    let colour_of = |frame: &MenuFrame<'_>| -> [f32; 4] {
        let geo = build(frame, None, None, 480.0, 320.0);
        [geo.colour[2], geo.colour[3], geo.colour[4], geo.colour[5]]
    };
    let loading_bg = colour_of(&loading);
    let pause_bg = colour_of(&pause_frame(&nav));
    if (loading_bg[3] - 1.0).abs() > 1e-6 {
        wrong.push(format!(
            "the loading screen's fallback quad must be opaque, alpha was {}",
            loading_bg[3]
        ));
    }
    if pause_bg[3] >= 1.0 {
        wrong.push(format!(
            "the pause screen's quad must be translucent, alpha was {}",
            pause_bg[3]
        ));
    }
    if loading_bg == pause_bg {
        wrong.push(
            "both screens emit the same backdrop colour, so the enum is not              reaching the draw at all"
                .to_owned(),
        );
    }

    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// **The reported bug** (2026-08-09): *"the main menu settings have the
/// header/footer, but if i go in game and open settings it doesnt have it. they
/// should be the exact same menu, not separate"* — and *"the main menu
/// implementation is the better one, it should fully replace the in-game one"*.
///
/// # What this measures, and why nothing caught it
///
/// The two screens really are one screen: one `Screen::Settings`, one
/// `options::settings_frame` that takes no in-world flag, and a `MenuNav::active_list`
/// arm keyed only on the page. So the *content* was never the difference. The
/// difference was that `render::frame_for` stamps the canvas facts — `gui_scale`,
/// `panorama_speed`, `list`, `cursor` — onto everything it returns, and answers
/// `None` for the overlay screens **by design**, so the in-world path built
/// `settings_frame` raw and reached none of them. `MenuFrame::list` is what the band
/// tint and both bevelled separator bars hang off (`ListSpec::chrome_rect`), so the
/// entire chrome silently vanished on one path of two.
///
/// Every render test in this file goes through `frame_for`, which is exactly the
/// path that worked — the blind spot was the *fixture set*, not any one assertion.
/// This gate therefore goes through `nav::settings_overlay_frame`, the expression
/// `app/redraw.rs`'s overlay draw and `nav::on_screen_frame`'s hit-test both use.
///
/// # Expected values
///
/// All from outside this crate, and identical to
/// `the_settings_band_carries_vanillas_separators_and_its_tint`'s: the four colours
/// are the decoded pixels of `textures/gui/{header,footer}_separator.png` and
/// `menu_list_background.png` from the 26.2 `client.jar` (whose `inworld_*` variants
/// are byte-identical, which is why one constant is faithful to both arms), and the
/// two y offsets are vanilla's own abstract-selection-list separator extraction's own
/// top-minus-2 and bottom accessors. The rects come from `ListSpec::chrome_rect`, the call the draw
/// makes.
///
/// # What else paints here
///
/// Asked first, because in-world adds a painter the main-menu arm does not have: the
/// `MenuBackdrop::Dim` wash. It is `OVERLAY_BG`, black at **64/255**, against
/// `LIST_BAND_TINT`'s black at **112/255** — distinct, so a probe cannot mistake the
/// wash for the band tint, and the negative assertions below are not satisfied by it.
/// The sample column is `x = 4`, in the left margin outside the 310 px row column,
/// clear of the centred title and the 200 px Done button.
///
/// # The grid page is asserted too
///
/// `SettingsPage::Root` is a widget grid, not vanilla's own options-list type — `Root.entries()` is
/// empty, so `active_list` answers `None` and it has no chrome by construction. That
/// is deliberate (a scrollbar beside a screen with no rows), and `packs.rs` has its
/// own deliberate `ListChrome::None`. This fix widens no condition, so the claim
/// asserted for Root is **agreement**: whatever the main menu does, in-world does the
/// same. Blessing "a grid page has no chrome" as correct is a separate question from
/// this report and is left alone.
#[test]
fn in_world_settings_carries_the_same_band_chrome_as_the_main_menu() {
    use crate::menu::options::SettingsPage;

    // The canvas the report was made against: short enough that Sound overflows
    // its band.
    const W: f32 = 320.0;
    const H: f32 = 240.0;

    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let mut wrong: Vec<String> = Vec::new();

    // -- the two frames, each by the path production actually uses -----------
    let (nav_out, ui_out) = settings_nav_on(SettingsPage::Sound);
    assert!(
        !ui_out.settings_in_world(),
        "premise: the title-screen walk is out of world"
    );
    let out = frame_for(&ui_out, &nav_out, &statuses, &mut fav)
        .expect("premise: the main-menu settings screen owns its frame");

    let (mut nav_in, ui_in) = settings_nav_in_world_on(SettingsPage::Sound);
    assert_eq!(
        nav_in.settings().page(),
        SettingsPage::Sound,
        "premise: the same page in both arms"
    );
    let inw = crate::menu::nav::settings_overlay_frame(&ui_in, &nav_in)
        .expect("premise: in-world settings declares an overlay frame");
    assert!(
        frame_for(&ui_in, &nav_in, &statuses, &mut fav).is_none(),
        "premise: in-world settings is an overlay, so `frame_for` must decline it — \
         if it stops declining, this gate is measuring the wrong path"
    );

    // -- the field claims, before anything that needs a rect ------------------
    // Deliberately ahead of the pixel work: the rect claims below report and stop
    // when there is no rect at all, so a neuter that removes the stamp would
    // otherwise leave every one of these as an argument rather than an observation.
    if inw.gui_scale != nav_in.gui_scale() {
        wrong.push(format!(
            "in-world settings carries gui_scale {} rather than the option's {} — the \
             GUI Scale setting does not reach the screen that edits it",
            inw.gui_scale,
            nav_in.gui_scale()
        ));
    }
    if inw.backdrop != MenuBackdrop::Dim {
        wrong.push(format!(
            "in-world settings declares backdrop {:?}, not Dim, so the panorama paints \
             over the paused world",
            inw.backdrop
        ));
    }
    if out.backdrop != MenuBackdrop::Panorama {
        wrong.push(format!(
            "the main-menu arm declares backdrop {:?}, not Panorama — the fork went the \
             wrong way",
            out.backdrop
        ));
    }
    // `cursor` on a **rebuilt** frame, so `inw` — the one the pixel probes below
    // rasterise — never carries a cursor and cannot grow a tooltip quad in the
    // probe column.
    nav_in.set_menu_cursor(311.0, 229.0, W, H);
    let with_cursor = crate::menu::nav::settings_overlay_frame(&ui_in, &nav_in)
        .expect("in-world settings still declares a frame with a cursor set");
    if with_cursor.cursor != nav_in.menu_cursor() {
        wrong.push(format!(
            "in-world settings carries cursor {:?} rather than the nav's {:?}, so hover \
             affordances and option tooltips are dead in world",
            with_cursor.cursor,
            nav_in.menu_cursor()
        ));
    }

    // -- the list spec, and that both arms agree on it -----------------------
    let out_rect = out
        .list
        .as_ref()
        .and_then(|spec| spec.model(H).map(|list| spec.chrome_rect(&list, W)))
        .flatten();
    let in_rect = inw
        .list
        .as_ref()
        .and_then(|spec| spec.model(H).map(|list| spec.chrome_rect(&list, W)))
        .flatten();
    if in_rect.is_none() {
        wrong.push(format!(
            "in-world Sound declares no chrome rect (frame.list is {}), so the band \
             tint and all four separator rows are skipped — this is the reported bug",
            if inw.list.is_some() { "Some" } else { "None" }
        ));
    }
    if out_rect != in_rect {
        wrong.push(format!(
            "the two arms disagree on the chrome rect: main menu {out_rect:?} vs \
             in-world {in_rect:?}; they are the same screen at the same page and \
             canvas, so these must be byte-identical"
        ));
    }

    // Everything below needs a rect; without one the pixel claims are moot rather
    // than false, so report and stop instead of unwrapping.
    let Some((cx, cy, cw, ch)) = in_rect else {
        assert!(wrong.is_empty(), "{wrong:#?}");
        unreachable!("in_rect is None but nothing was reported");
    };
    // The same 33 px-header / 33 px-footer band vanilla's own options-list type is sized
    // to, asserted here as well as in the main-menu gate so a change to one arm
    // cannot quietly move both.
    if (cx, cy, cw, ch) != (0.0, 33.0, W, H - 33.0 - 33.0) {
        wrong.push(format!(
            "the in-world chrome rect is {:?}, not the 33/33 band",
            (cx, cy, cw, ch)
        ));
    }

    // -- the chrome, by location and by exact colour, on the in-world frame ---
    let v = geometry(&inw, W, H);
    let probe_x = 4.0;
    let at = |verts: &[f32], y: f32| colour_at(verts, 2.0 * probe_x / W - 1.0, 1.0 - 2.0 * y / H);
    // `header_separator.png` is light over dark; `footer_separator.png` is the
    // mirror. Sampled at row centres, since a quad spans `y..y+1`. Collected
    // rather than asserted in place: an `assert!` here would prove one row and
    // leave the other three as arguments.
    for (y, want, what) in [
        (cy - SEPARATOR_H + 0.5, SEPARATOR_LIGHT, "header bar light row"),
        (cy - 1.0 + 0.5, SEPARATOR_DARK, "header bar dark row"),
        (cy + ch + 0.5, SEPARATOR_DARK, "footer bar dark row"),
        (cy + ch + 1.5, SEPARATOR_LIGHT, "footer bar light row"),
    ] {
        let got = at(&v, y);
        if !got.is_some_and(|c| c.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.002)) {
            wrong.push(format!(
                "in-world {what} at y={y} is {got:?}, not the decoded {want:?}"
            ));
        }
    }
    // And the tint, over the band's whole height in that column.
    let margin = (0.0, cy, options::row_left(W, 0), ch);
    let tinted = coverage_of(&v, W, H, margin, LIST_BAND_TINT);
    if tinted != 1.0 {
        wrong.push(format!(
            "the in-world band's left margin {margin:?} is only {tinted} tinted — the \
             black filter over the paused world does not cover the band"
        ));
    }
    // The tint **stops** at the band: one row above the header bar, one inside the
    // footer band. A tint that ran the whole canvas would satisfy the claim above
    // and still be wrong, and in-world that is a live risk because the `Dim` wash
    // really does cover everything.
    for (y, what) in [
        (cy - SEPARATOR_H - 1.5, "above the header bar"),
        (cy + ch + SEPARATOR_H + 1.5, "below the footer bar"),
    ] {
        let got = at(&v, y);
        if got.is_some_and(|c| c.iter().zip(LIST_BAND_TINT).all(|(a, b)| (a - b).abs() < 0.002)) {
            wrong.push(format!(
                "the in-world band tint leaked {what} (y={y}), so the content band is \
                 not fenced off"
            ));
        }
    }

    // -- the grid page: agreement, not a chrome claim -------------------------
    let (nav_root_in, ui_root_in) = settings_nav_in_world_on(SettingsPage::Root);
    let root_in = crate::menu::nav::settings_overlay_frame(&ui_root_in, &nav_root_in)
        .expect("in-world Root declares an overlay frame");
    let mut nav_root_out = test_nav("root-out-chrome");
    let mut ui_root_out = crate::menu::UiState::new();
    ui_root_out.open_settings();
    let root_out = frame_for(&ui_root_out, &nav_root_out, &statuses, &mut fav)
        .expect("premise: main-menu Root owns its frame");
    assert_eq!(
        nav_root_in.settings().page(),
        SettingsPage::Root,
        "premise: in-world settings opens on Root"
    );
    assert_eq!(
        nav_root_out.settings().page(),
        SettingsPage::Root,
        "premise: main-menu settings opens on Root"
    );
    if root_in.list.is_some() != root_out.list.is_some() {
        wrong.push(format!(
            "the two arms disagree on whether the Root grid has a list: main menu {} vs \
             in-world {}",
            root_out.list.is_some(),
            root_in.list.is_some()
        ));
    }
    // And the deliberate no-chrome case stays no-chrome: a grid page must not have
    // grown a scrollbar-bearing band on either path.
    for (frame, what) in [(&root_out, "main menu"), (&root_in, "in-world")] {
        if frame.list.is_some() {
            wrong.push(format!(
                "the {what} Root grid now declares a ListSpec, so a scrollbar hangs \
                 beside a screen with no rows"
            ));
        }
    }
    let _ = &mut nav_root_out;

    // -- control, executed ---------------------------------------------------
    // The **pre-fix construction**: `settings_frame` raw, exactly as
    // `app/redraw.rs` used to build it. It must produce no chrome at all, which is
    // what proves (a) the assertions above measure the chrome rather than something
    // else painting at those rows, and (b) the stamp is what supplies it. Observed
    // failing before the fix: this was the production frame.
    let raw = crate::menu::options::settings_frame(
        nav_in.settings(),
        nav_in.options(),
        nav_in.options_save_error(),
    );
    if raw.list.is_some() {
        wrong.push(
            "control premise broken: the raw `settings_frame` now carries a ListSpec of \
             its own, so this control no longer demonstrates that the stamp is what \
             supplies the chrome"
                .to_owned(),
        );
    }
    let rv = geometry(&raw, W, H);
    let raw_at = at(&rv, cy - SEPARATOR_H + 0.5);
    if raw_at.is_some() {
        wrong.push(format!(
            "control: the unstamped frame still painted {raw_at:?} where the header bar \
             goes, so the chrome is not what this gate measured"
        ));
    }
    let raw_tint = coverage_of(&rv, W, H, margin, LIST_BAND_TINT);
    if raw_tint != 0.0 {
        wrong.push(format!(
            "control: the unstamped frame still tinted {raw_tint} of the band"
        ));
    }

    assert!(wrong.is_empty(), "{wrong:#?}");
}

// -- the tab bar "meshes with the border" -----------------------------------
//
// Point-sampled, not vertex-counted: `colour_at` reads the *topmost quad
// covering an exact pixel*, which is what lets this tell a large enclosing
// fill (the merge panel, or `chrome`'s own full-width tint) apart from a
// thin 1 px separator drawn on top of it — a vertex-in-rect probe like
// `band_coverage` would read the merge fill's own large quad as "nothing
// here" for a small separator-sized probe rect, which is exactly the trap
// `CLAUDE.md` names. See `colour_at`/`coverage_of`'s own docs for why this
// codebase already prefers point sampling for this class of check
// (`the_selected_row_is_visibly_different_from_its_neighbours` is the
// precedent).

/// Statistics's own tab bar at the same 854×480 canvas
/// `tab_bar_geometry_matches_vanillas_own_arithmetic_at_854_wide` measures:
/// `start_x = 242`, `tab_width = 124`, so General is `242..366`, Items
/// `366..490`, Mobs `490..614`, and the flanks are `0..242`/`614..854`.
///
/// Three claims, all from the owner's report and none of them a size
/// change (vanilla's own menu-tab-bar height constant and `tab_bar_geometry` were already vanilla's
/// own constants):
///
/// 1. The header separator (vanilla's own screen base's header-separator constant) appears in the two
///    flanking margins, and **only** there — not under any tab, selected
///    or not. Before this fix a full-width bar ran under the whole strip.
/// 2. The selected tab's bottom edge **meshes with the content panel
///    below**: the merge fill (`LIST_BAND_TINT`, standing in for vanilla's
///    own screen base's menu-background constant, which this client has no tiled texture
///    for) paints under General but not under the unselected Items tab.
/// 3. That merge is not a guess at a gap — the selected tab's own bottom
///    edge (`layout::TAB_BAR_HEIGHT`) and the content band's own top
///    (`ListSpec::model(..).top()`) are asserted to be the **same
///    coordinate**, both read from the expressions the draw actually
///    uses rather than restated as a literal `24.0` twice.
#[test]
fn the_statistics_tab_bar_meshes_selected_tab_only_and_the_flanks_carry_the_header_separator() {
    let nav = test_nav("tab-mesh-stats");
    let mut ui = UiState::new();
    ui.enter_dev_world();
    ui.pause();
    ui.open_statistics_from_pause();
    assert_eq!(ui.screen(), Screen::Statistics, "premise");

    // Statistics is an overlay now (it always shows the paused world behind
    // it — see `Screen::Statistics`'s own doc), so its production frame comes
    // from `stats_overlay_frame`, not `frame_for` (which deliberately
    // answers `None` here).
    let frame = crate::menu::nav::stats_overlay_frame(&ui, &nav).expect("Statistics draws a frame");
    let (w, h) = (854.0_f32, 480.0_f32);
    let v = geometry(&frame, w, h);
    assert!(!v.is_empty(), "the screen must draw something");

    // Claim 3: the same coordinate, derived from both sides' own expressions.
    let chrome_top = frame
        .list
        .as_ref()
        .and_then(|spec| spec.model(h))
        .map(|list| list.top())
        .expect("Statistics has a content band");
    assert_eq!(
        chrome_top,
        layout::TAB_BAR_HEIGHT,
        "the content band's own top ({chrome_top}) must be the tab bar's own bottom \
         ({}) — a gap here is exactly what 'does not mesh' looks like",
        layout::TAB_BAR_HEIGHT
    );

    let (start_x, tab_width) = layout::tab_bar_geometry(w, 3);
    assert_eq!((start_x, tab_width), (242.0, 124.0), "premise: the known-good geometry");

    let at = |px: f32, py: f32| colour_at(&v, 2.0 * px / w - 1.0, 1.0 - 2.0 * py / h);
    let close = |c: Option<[f32; 4]>, want: [f32; 4]| {
        c.is_some_and(|c| c.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.01))
    };

    let mut wrong = Vec::new();

    // Claim 1: the flanks carry the separator, at the tab bar's own bottom
    // edge (`h - SEPARATOR_H .. h`), and interior points under any tab do
    // not — checked under the *selected* tab (General, 242..366) and an
    // *unselected* one (Items, 366..490), so a fix that only special-cased
    // the selected tab could not pass this by accident.
    for (x, y, want, what) in [
        (100.0, 22.5, SEPARATOR_LIGHT, "left flank, light row"),
        (100.0, 23.5, SEPARATOR_DARK, "left flank, dark row"),
        (700.0, 22.5, SEPARATOR_LIGHT, "right flank, light row"),
        (700.0, 23.5, SEPARATOR_DARK, "right flank, dark row"),
    ] {
        let got = at(x, y);
        if !close(got, want) {
            wrong.push(format!(
                "{what} at ({x}, {y}): expected {want:?}, got {got:?}"
            ));
        }
    }
    for (x, y, what) in [
        (300.0, 22.5, "under General (selected), light row"),
        (300.0, 23.5, "under General (selected), dark row"),
        (420.0, 22.5, "under Items (unselected), light row"),
        (420.0, 23.5, "under Items (unselected), dark row"),
    ] {
        let got = at(x, y);
        if close(got, SEPARATOR_LIGHT) || close(got, SEPARATOR_DARK) {
            wrong.push(format!(
                "{what} at ({x}, {y}) painted a header-separator colour ({got:?}) — vanilla's \
                 own menu-tab-bar type never draws one under any tab, only in the two flanking margins"
            ));
        }
    }

    // Claim 2: the merge fill under General's own inset body (`x+2, y+2` per
    // vanilla's own menu-tab-button background-render call), sampled 10 px in from the tab's
    // left edge and away from its centred label so a glyph quad cannot be
    // mistaken for (or hide) the fill. Absent under Items, which is not
    // selected and must not merge with anything.
    let general_merge = at(start_x + 10.0, 10.0);
    if !close(general_merge, LIST_BAND_TINT) {
        wrong.push(format!(
            "General's own inset body at ({}, 10) is {general_merge:?}, not the content \
             panel's own LIST_BAND_TINT — the selected tab does not mesh with the panel below it",
            start_x + 10.0
        ));
    }
    let items_x = start_x + tab_width;
    let items_merge = at(items_x + 10.0, 10.0);
    if close(items_merge, LIST_BAND_TINT) {
        wrong.push(format!(
            "Items (unselected) painted the merge fill at ({}, 10) — only the selected tab \
             may merge with the panel below",
            items_x + 10.0
        ));
    }

    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Create New World's own tab bar (second consumer) at the same
/// canvas: the flanking separators are this bar's own concern regardless of
/// whether a content band sits below it (unlike Statistics, this screen has
/// no `ListSpec` at all), but the merge fill must **never** appear here —
/// there is no panel to merge with, and painting one anyway would be a
/// worse regression than the pre-fix "no merge anywhere" state, since it
/// would show a tinted patch floating over the plain backdrop.
#[test]
fn create_worlds_tab_bar_gets_the_flanking_separators_but_never_the_merge_fill() {
    let nav = test_nav("tab-mesh-create-world");
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let mut ui = UiState::new();
    ui.open_world_select();
    ui.open_create_world();
    assert_eq!(ui.screen(), Screen::CreateWorld, "premise");

    let frame = frame_for(&ui, &nav, &statuses, &mut fav).expect("Create New World draws a frame");
    assert!(
        frame.list.is_none(),
        "premise: this screen has no ListSpec, which is exactly why its merge fill must stay off"
    );
    let (w, h) = (854.0_f32, 480.0_f32);
    let v = geometry(&frame, w, h);
    assert!(!v.is_empty());

    let (start_x, tab_width) = layout::tab_bar_geometry(w, create_world::TAB_LABELS.len());
    let at = |px: f32, py: f32| colour_at(&v, 2.0 * px / w - 1.0, 1.0 - 2.0 * py / h);
    let close = |c: Option<[f32; 4]>, want: [f32; 4]| {
        c.is_some_and(|c| c.iter().zip(want).all(|(a, b)| (a - b).abs() < 0.01))
    };

    let mut wrong = Vec::new();
    for (x, y, want, what) in [
        (100.0, 22.5, SEPARATOR_LIGHT, "left flank, light row"),
        (100.0, 23.5, SEPARATOR_DARK, "left flank, dark row"),
        (700.0, 22.5, SEPARATOR_LIGHT, "right flank, light row"),
        (700.0, 23.5, SEPARATOR_DARK, "right flank, dark row"),
    ] {
        let got = at(x, y);
        if !close(got, want) {
            wrong.push(format!("{what} at ({x}, {y}): expected {want:?}, got {got:?}"));
        }
    }
    // Game (index 0) is selected by default — sample its own inset body,
    // same offset the Statistics gate uses.
    let game_merge = at(start_x + 10.0, 10.0);
    if close(game_merge, LIST_BAND_TINT) {
        wrong.push(format!(
            "Game (selected, default tab) painted the merge fill at ({}, 10) even though this \
             screen has no content band to merge with",
            start_x + 10.0
        ));
    }
    let _ = tab_width;

    assert!(wrong.is_empty(), "{wrong:#?}");
}

/// Tab labels must be vertically centred within their controls. The selected
/// and unselected states use different top insets, then share the same
/// centring calculation between the control's top and bottom edges; drawing at
/// the raw top inset would leave both states too high. See
/// `widget::tab_label_top` for the formula.
///
/// Two arms, selected and unselected, on the **same label** ("More",
/// `create_world::MORE_TAB`) so a fixture-width or x-position difference
/// cannot be mistaken for the fix: clicking the More tab flips it from
/// unselected to selected without moving it, which is what makes this a
/// discriminating input rather than the *magnitude* species `CLAUDE.md`
/// warns about ("the label moved down" alone would pass on either wrong
/// hypothesis moving it *some* amount).
///
/// "More" is also not an incidental choice of label: the jar-less debug
/// font's `M` glyph (`font::glyph_rows`) lights up both edge columns in
/// *every* row, including row 0, so the emitted glyph quads' own
/// bounding-box top is exactly the `y` `Quads::text` was called with, with
/// no per-glyph offset to subtract first — the same trick
/// `the_search_box_draws_as_a_field_inside_its_own_slot` uses for the same
/// reason.
///
/// Predicted from `widget::tab_label_top`'s own formula at `y = 0`,
/// `height = layout::TAB_BAR_HEIGHT` (24), `line_height = LINE_H` (9):
/// unselected `(3 + 24 - 9) / 2 + 1 = 10`, selected `(0 + 24 - 9) / 2 + 1 =
/// 8`. The pre-fix code drew at `3`/`0` — both wrong, and wrong in the
/// direction the owner reported ("too high").
#[test]
fn the_tab_label_is_vertically_centred_not_flush_against_the_tabs_own_top() {
    let nav = test_nav("tab-label-y");
    let statuses = StatusCache::with_probe(unavailable_probe());
    let mut fav = FaviconCache::new();
    let mut ui = UiState::new();
    ui.open_world_select();
    ui.open_create_world();
    assert_eq!(ui.screen(), Screen::CreateWorld, "premise");

    let (w, h) = (854.0_f32, 480.0_f32);
    let (start_x, tab_width) = layout::tab_bar_geometry(w, create_world::TAB_LABELS.len());
    let more_x0 = start_x + tab_width * create_world::MORE_TAB as f32;
    let more_x1 = more_x0 + tab_width;

    // Restricted to both the More tab's own x-span and its own row's y-band
    // (`0..layout::TAB_BAR_HEIGHT`) — narrow enough that Game's and World's
    // own labels, sitting at a different x in the same y-band, cannot leak
    // into this bounding box.
    let more_label_top = |v: &[f32]| -> Option<f32> {
        let mut y0 = f32::MAX;
        let mut seen = false;
        for vert in v.chunks_exact(STRIDE) {
            if (2..6).any(|c| (vert[c] - widget::ACTIVE_LABEL[c - 2]).abs() > 1e-4) {
                continue;
            }
            let px = (vert[0] + 1.0) * 0.5 * w;
            let py = (1.0 - vert[1]) * 0.5 * h;
            if px < more_x0 || px > more_x1 || py < 0.0 || py > layout::TAB_BAR_HEIGHT {
                continue;
            }
            seen = true;
            y0 = y0.min(py);
        }
        seen.then_some(y0)
    };

    // Arm 1: unselected. Game (index 0) is selected by default (see the
    // control above), so More starts out unselected.
    let f = frame_for(&ui, &nav, &statuses, &mut fav).expect("Create New World draws a frame");
    let unselected_y = more_label_top(&geometry(&f, w, h))
        .expect("the More label must draw inside its own tab rect");
    assert!(
        (unselected_y - 10.0).abs() < 0.01,
        "unselected More's label top is {unselected_y}, want widget::tab_label_top(0, 24, \
         false, 9) == 10 — not vanilla's raw `top` local (3). The label must be vertically \
         centred, not flush against the tab's own top"
    );

    // Arm 2: selected, same label, same x — only the tab's selection state
    // changed.
    let mut nav = nav;
    assert_eq!(
        nav.click(&mut ui, create_world::MORE_TAB),
        crate::menu::nav::MenuAction::None,
        "clicking a tab switches content in place, it does not navigate"
    );
    let f = frame_for(&ui, &nav, &statuses, &mut fav).expect("Create New World draws a frame");
    let selected_y = more_label_top(&geometry(&f, w, h))
        .expect("the More label must draw inside its own tab rect");
    assert!(
        (selected_y - 8.0).abs() < 0.01,
        "selected More's label top is {selected_y}, want widget::tab_label_top(0, 24, true, 9) \
         == 8 — not vanilla's raw `top` local (0)"
    );

    assert_ne!(
        unselected_y, selected_y,
        "the selected/unselected offset must survive centring, or this gate could not tell a \
         centred-but-offset-collapsed regression from a correct draw"
    );
}
