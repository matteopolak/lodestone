//! Tab-list display projection.
//!
//! ## What it is
//!
//! The authoritative fold lives in [`lodestone_game::tablist::TabList`]. This
//! module lowers that state into a [`TabListView`] — the shape `hud.rs` draws
//! while Tab is held, laid out to vanilla's own tab overlay.
//!
//! ## How it works
//!
//! [`tab_list_view`] does four things the fold deliberately does not:
//!
//! * resolves each entry's `display_name` (or its plain profile name) into
//!   **styled spans**, so a server that colours a name gets a coloured row;
//! * when an entry carries **no** explicit `display_name`, runs the plain
//!   profile name through the player's scoreboard team —
//!   vanilla's own get-name-for-display's other half,
//!   its own format-name-for-team routine — via
//!   [`Scoreboard::display_name_of`](lodestone_game::scoreboard::Scoreboard::display_name_of).
//!   This is the more common source of a coloured tab-list name in practice: a
//!   server that runs `/team modify <team> color` never sets a display name at
//!   all, and a `tab_list_view` that only checked `display_name` would show
//!   every one of its players in plain white;
//! * applies vanilla's `limit(80)` — vanilla's own get-player-infos routine caps the
//!   overlay at 80 entries, after sorting, and a 200-player server therefore
//!   shows the first 80 in comparator order rather than 200 rows off the bottom
//!   of the screen;
//! * turns the raw latency into the sprite id of one of vanilla's five discrete
//!   signal-bar icons ([`ping_sprite`]).
//!
//! ## How to change it, and the gotchas
//!
//! **The header and footer render only when the server sent them.** A vanilla
//! server sends neither unless a plugin or a datapack sets one, which is why
//! vanilla's own tab list shows neither; [`banner_lines`] returns an empty `Vec`
//! for absent, empty and whitespace-only banners, and the draw skips the whole
//! plate rather than drawing an empty gap. Do not synthesise either to fill
//! space.
//!
//! **This client draws no player head, and that is vanilla's own behaviour on
//! every server we can host.** Vanilla's own overlay render-state extract routine gates the
//! 8×8 face on `showHead = this.minecraft.getConnection().onlineMode()`, which
//! comes from the LOGIN packet's `onlineMode` field. Our own server writes
//! `false` there (`v770`'s `server_protocol`), so vanilla joined to it would
//! draw no head either, and the layout below — which reserves the 9 px only when
//! [`TabListView::show_head`] is set — is exactly vanilla's no-head layout.
//! Turning heads on needs two things this module cannot supply: the client-side
//! decode of that `onlineMode` field, and a texture path in the HUD pass (the
//! HUD has a colour pipeline and a single GUI-atlas sprite pipeline; a per-player
//! skin needs a third).
//!
//! ## Dependencies
//!
//! [`lodestone_game::tablist`] for the state, `lodestone_game::text` for
//! `translate` resolution, and the `gui/sprites/icon/ping_*` sprites out of the
//! GUI atlas for the bars.

use lodestone_game::scoreboard::Scoreboard;
use lodestone_game::tablist::{PlayerListEntry, TabList};
use lodestone_model::Text;
use lodestone_model::text::TextSpan;

/// Vanilla's cap on how many rows the overlay shows —
/// vanilla's own get-player-infos routine's `.limit(80L)`, applied **after** the
/// comparator so which 80 you see is well defined.
pub const MAX_TAB_ROWS: usize = 80;

/// One row of the tab overlay.
#[derive(Debug, Clone, PartialEq)]
pub struct TabListRow {
    /// The name to draw, as styled spans — vanilla's own get-name-for-display routine,
    /// which prefers `getTabListDisplayName()` and falls back to the plain
    /// profile name.
    pub name: Vec<TextSpan>,
    /// The GUI-atlas sprite id for this row's signal bars — see [`ping_sprite`].
    pub ping_sprite: &'static str,
    /// Whether this player is a spectator. Vanilla draws a spectator's name in
    /// `0x90FFFFFF` rather than opaque white (`extractRenderState`'s
    /// `info.getGameMode() == GameType.SPECTATOR ? -1862270977 : -1`) *and*
    /// italicises it in `decorateName`; only the alpha is modelled here, because
    /// this font has no italic variant and a fabricated slant would be worse
    /// than the dimming alone.
    pub spectator: bool,
}

/// Everything the tab overlay draws for one frame.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TabListView {
    /// The rows, already sorted and capped at [`MAX_TAB_ROWS`].
    pub rows: Vec<TabListRow>,
    /// The server's header, one entry per line, each line **styled spans**. Empty
    /// when it sent none.
    ///
    /// These were `Vec<String>` filled through `to_plain_string`, which is where a
    /// header's colour died. A legacy `§` code would have survived a `String` —
    /// the font layer applies codes at draw time — but a hex colour has no legacy
    /// code, so the flatten was lossy in a way no better string could fix. See
    /// [`banner_lines`].
    pub header: Vec<Vec<TextSpan>>,
    /// The server's footer, same shape and same rule.
    pub footer: Vec<Vec<TextSpan>>,
    /// Whether each row reserves 9 px for an 8×8 player face — vanilla's
    /// `showHead = connection.onlineMode()`. Always `false` here; see the module
    /// doc for what it would take to turn on, and why an offline-mode server
    /// makes `false` the *correct* answer rather than a placeholder.
    pub show_head: bool,
}

impl TabListView {
    /// How many rows there are — the `slots` count vanilla's column split works
    /// from.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// Whether there is nothing to draw.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }
}

/// The signal-bar sprite for a latency in milliseconds —
/// vanilla's own ping-icon extract routine, transcribed.
///
/// The bands are **not** evenly spaced and the strongest is the widest: anything
/// under 150 ms gets all five bars, and it takes a full second to fall to one.
/// A negative latency (the "not reported yet" value the wire uses) is
/// `ping_unknown`, which is the crossed-out icon and not zero bars.
#[must_use]
pub fn ping_sprite(latency: i32) -> &'static str {
    if latency < 0 {
        "icon/ping_unknown"
    } else if latency < 150 {
        "icon/ping_5"
    } else if latency < 300 {
        "icon/ping_4"
    } else if latency < 600 {
        "icon/ping_3"
    } else if latency < 1000 {
        "icon/ping_2"
    } else {
        "icon/ping_1"
    }
}

/// Lowers the folded tab list into the view the HUD draws.
///
/// `scoreboard` is the same fold [`crate::sim::session::Sim::sidebar`] already
/// reads through [`Scoreboard::display_name_of`] — `None` off a session with
/// no scoreboard data yet, which degrades to the plain-name half of
/// [`name_for_display`] exactly as vanilla does with no team.
#[must_use]
pub fn tab_list_view(
    tab_list: &TabList,
    scoreboard: Option<&Scoreboard>,
    translate: &dyn Fn(&str) -> Option<String>,
) -> TabListView {
    let rows = tab_list
        .ordered()
        .into_iter()
        .take(MAX_TAB_ROWS)
        .map(|entry| TabListRow {
            name: lodestone_game::text::resolve(&name_for_display(entry, scoreboard), translate)
                .to_spans(),
            ping_sprite: ping_sprite(entry.latency),
            spectator: entry.game_mode == lodestone_model::GameMode::Spectator,
        })
        .collect();
    TabListView {
        rows,
        header: banner_lines(tab_list.header.as_ref(), translate),
        footer: banner_lines(tab_list.footer.as_ref(), translate),
        // See the module doc: vanilla's own gate is `onlineMode()`, which our
        // server reports as `false` and our client does not yet decode.
        show_head: false,
    }
}

/// vanilla's own get-name-for-display routine: an explicit tab-list display name
/// wins outright (`entry.effective_name()` already does that half); with none,
/// the plain profile name is run through the player's scoreboard team
/// (vanilla's own format-name-for-team routine) rather than left bare.
///
/// This is the fold [`PlayerListEntry::effective_name`] deliberately does not
/// do, because it lives in `lodestone-game`'s per-entry state and has no
/// `Scoreboard` to consult — this is the view-layer half, alongside the
/// styled-span resolution the module doc already claims.
#[must_use]
fn name_for_display(entry: &PlayerListEntry, scoreboard: Option<&Scoreboard>) -> Text {
    if entry.display_name.is_some() {
        return entry.effective_name();
    }
    match scoreboard {
        Some(board) => board.display_name_of(&entry.profile.name),
        None => Text::literal(entry.profile.name.clone()),
    }
}

/// Lowers a tab-list header or footer into the centred lines the HUD draws
/// above and below the player rows.
///
/// A `Text` is a *tree*, and a server writes a multi-line banner as literal
/// `\n` inside it — so resolving and splitting on the breaks is the whole job.
/// An absent, empty or whitespace-only banner yields **no lines**, which
/// is what makes `Option`-ing the result at the call site unnecessary: vanilla
/// draws nothing for an empty header rather than an empty gap
/// (vanilla's own overlay render routine, which only measures a non-null header).
///
/// Trailing empties are dropped but interior blank lines are kept: a server
/// separating two banner halves with a blank line means it.
///
/// The split runs over **spans**, via [`crate::overlay::spans_lines`], not over a
/// flattened string. Flattening first would be simpler and would silently discard
/// every colour the banner carried — including hex, which has no legacy `§` code
/// and therefore cannot be smuggled through a `String` the way the sixteen named
/// colours can.
#[must_use]
pub fn banner_lines(
    banner: Option<&lodestone_model::Text>,
    translate: &dyn Fn(&str) -> Option<String>,
) -> Vec<Vec<TextSpan>> {
    let Some(banner) = banner else {
        return Vec::new();
    };
    let spans = lodestone_game::text::resolve(banner, translate).to_spans();
    // The whitespace test is on the *wording*: a banner of nothing but spaces and
    // breaks draws nothing however it is coloured.
    if crate::overlay::spans_text(&spans).trim().is_empty() {
        return Vec::new();
    }
    let mut lines = crate::overlay::spans_lines(&spans);
    while lines
        .last()
        .is_some_and(|l| crate::overlay::spans_text(l).trim().is_empty())
    {
        lines.pop();
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::tablist::{GameProfile, PlayerListEntry};
    use lodestone_model::{GameMode, Text};
    use uuid::Uuid;

    fn entry(id: u128, name: &str, latency: i32, mode: GameMode) -> PlayerListEntry {
        let mut entry = PlayerListEntry::new(GameProfile::new(Uuid::from_u128(id), name));
        entry.latency = latency;
        entry.game_mode = mode;
        entry
    }

    /// A translator that resolves nothing (the demo-palette case).
    fn no_tr(_: &str) -> Option<String> {
        None
    }

    /// Plain names of the rows, in draw order.
    fn names(view: &TabListView) -> Vec<String> {
        view.rows
            .iter()
            .map(|row| crate::overlay::spans_text(&row.name))
            .collect()
    }

    #[test]
    fn rows_use_game_tablist_order_and_display_names() {
        let mut tabs = TabList::new();
        let mut bob = entry(2, "Bob", 30, GameMode::Spectator);
        bob.display_name = Some(Text::literal("Bob AFK"));
        tabs.insert(bob);
        tabs.insert(entry(1, "Alice", 12, GameMode::Survival));

        let view = tab_list_view(&tabs, None, &no_tr);
        // Alice first because Bob is a spectator, and spectators sort last —
        // `PLAYER_COMPARATOR`'s second key. Alphabetically Alice would come first
        // anyway, so the *spectator flag* is the discriminating assertion here.
        assert_eq!(names(&view), vec!["Alice AFK".replace(" AFK", ""), "Bob AFK".to_string()]);
        assert_eq!(view.rows[0].spectator, false);
        assert_eq!(view.rows[1].spectator, true);
    }

    /// The spectator key really does outrank the name, which the test above cannot
    /// show because its alphabetical order happens to agree.
    ///
    /// `Aaa` is a spectator and `Zzz` is not, so a name-only comparator answers
    /// `[Aaa, Zzz]` and the real one answers `[Zzz, Aaa]`.
    #[test]
    fn a_spectator_sorts_after_a_later_name() {
        let mut tabs = TabList::new();
        tabs.insert(entry(1, "Aaa", 10, GameMode::Spectator));
        tabs.insert(entry(2, "Zzz", 10, GameMode::Survival));
        assert_eq!(
            names(&tab_list_view(&tabs, None, &no_tr)),
            vec!["Zzz".to_string(), "Aaa".to_string()]
        );
    }

    #[test]
    fn rows_resolve_translate_display_names_through_the_translator() {
        let mut tabs = TabList::new();
        let mut e = entry(1, "Steve", 20, GameMode::Survival);
        e.display_name = Some(Text::translate("entity.minecraft.spider", vec![]));
        tabs.insert(e);

        let tr = |key: &str| (key == "entity.minecraft.spider").then(|| "Spider".to_string());
        assert_eq!(names(&tab_list_view(&tabs, None, &tr)), vec!["Spider".to_string()]);
        // Negative control: no table leaks the raw key.
        assert_eq!(
            names(&tab_list_view(&tabs, None, &no_tr)),
            vec!["entity.minecraft.spider".to_string()]
        );
    }

    /// A **hex** colour on an explicit per-player display name survives to the
    /// row, the same discriminating shape as
    /// [`a_hex_coloured_banner_keeps_its_colour_on_both_sides_of_a_break`] but
    /// for [`TabListRow::name`] rather than the header/footer — nothing in this
    /// crate's corpus checked the row path on its own before this, only the
    /// banner, so a regression specific to `PlayerListEntry::display_name`
    /// could have shipped unnoticed even with the banner gate green.
    #[test]
    fn a_players_hex_coloured_display_name_survives_to_the_row() {
        use lodestone_model::TextColor;

        const HEX: u32 = 0x00ab_cdef;
        let mut tabs = TabList::new();
        let mut e = entry(1, "Steve", 10, GameMode::Survival);
        e.display_name = Some(Text {
            style: lodestone_model::TextStyle {
                font: None,
                color: Some(TextColor::Rgb(HEX)),
                ..lodestone_model::TextStyle::default()
            },
            ..Text::literal("Nicked")
        });
        tabs.insert(e);

        let view = tab_list_view(&tabs, None, &no_tr);
        assert_eq!(names(&view), vec!["Nicked".to_string()]);
        let got: Vec<Option<TextColor>> =
            view.rows[0].name.iter().map(|s| s.style.color).collect();
        assert_eq!(
            got,
            vec![Some(TextColor::Rgb(HEX))],
            "a hex TextColor::Rgb on the display name must reach the row's spans, \
             got {got:?}"
        );
    }

    /// vanilla's own get-name-for-display routine's other half: a player with **no**
    /// explicit display name is still coloured, through their scoreboard team —
    /// vanilla's own format-name-for-team routine. This is the common case a server that
    /// only runs `/team modify <team> color` hits, with no display-name
    /// component ever sent, so a `tab_list_view` that only checked
    /// `display_name` would show every one of these players in plain white.
    ///
    /// The prefix and suffix are asserted too, not just the colour: vanilla
    /// wraps the whole `prefix + name + suffix` run in the team colour, and a
    /// gate that only checked the colour could pass against an implementation
    /// that dropped the prefix/suffix text entirely.
    #[test]
    fn a_player_with_no_display_name_is_coloured_by_their_scoreboard_team() {
        use lodestone_game::scoreboard::{Scoreboard, Team, TeamColor};
        use lodestone_model::TextColor;

        let mut tabs = TabList::new();
        tabs.insert(entry(1, "Notch", 10, GameMode::Survival));

        let mut board = Scoreboard::new();
        let mut red = Team::new("red");
        red.color = Some(TeamColor::Red);
        red.prefix = Text::literal("[R] ");
        red.members.push("Notch".to_string());
        board.add_team(red);

        let with_team = tab_list_view(&tabs, Some(&board), &no_tr);
        assert_eq!(
            names(&with_team),
            vec!["[R] Notch".to_string()],
            "the team prefix must reach the row alongside the plain name"
        );
        assert_eq!(
            with_team.rows[0].name[0].style.color,
            Some(TeamColor::Red.as_text_color()),
            "the team's colour must reach the row"
        );

        // Negative control: with no scoreboard at all, the same player draws
        // plain — proving the colour above came from the team and not from
        // some other default.
        let without_team = tab_list_view(&tabs, None, &no_tr);
        assert_eq!(names(&without_team), vec!["Notch".to_string()]);
        assert_eq!(without_team.rows[0].name[0].style.color, None);

        // An explicit display name still wins outright over the team, exactly
        // as `getNameForDisplay` reads: `getTabListDisplayName() != null` short-
        // circuits before the team branch is ever reached.
        let mut with_display = TabList::new();
        let mut e = entry(1, "Notch", 10, GameMode::Survival);
        e.display_name = Some(Text::literal("Explicit"));
        with_display.insert(e);
        let overridden = tab_list_view(&with_display, Some(&board), &no_tr);
        assert_eq!(
            names(&overridden),
            vec!["Explicit".to_string()],
            "an explicit display name must win over team formatting entirely"
        );
        assert_eq!(overridden.rows[0].name[0].style.color, None);
    }

    /// The ping bands, **at their boundaries**.
    ///
    /// `extractPingIcon`'s ladder is `< 150 / < 300 / < 600 / < 1000`, so every
    /// threshold is exclusive and the strongest band is by far the widest. Testing
    /// only the midpoints (say 100 / 200 / 400) would pass under a `<=` reading
    /// too, so each pair below straddles one boundary.
    #[test]
    fn the_ping_bands_are_vanillas_own_exclusive_thresholds() {
        assert_eq!(ping_sprite(-1), "icon/ping_unknown");
        assert_eq!(ping_sprite(0), "icon/ping_5");
        assert_eq!(ping_sprite(149), "icon/ping_5");
        assert_eq!(ping_sprite(150), "icon/ping_4");
        assert_eq!(ping_sprite(299), "icon/ping_4");
        assert_eq!(ping_sprite(300), "icon/ping_3");
        assert_eq!(ping_sprite(599), "icon/ping_3");
        assert_eq!(ping_sprite(600), "icon/ping_2");
        assert_eq!(ping_sprite(999), "icon/ping_2");
        assert_eq!(ping_sprite(1000), "icon/ping_1");
    }

    /// The overlay is capped at [`MAX_TAB_ROWS`], **after** sorting.
    ///
    /// 100 players go in named `p000..p099`; 80 come out, and they are the
    /// alphabetically first 80 rather than an arbitrary 80 — which is what
    /// `.sorted().limit(80)` means and what a `.limit(80).sorted()` reading would
    /// get wrong (a `HashMap` iteration order would make the last row anything at
    /// all).
    #[test]
    fn the_view_caps_at_vanillas_eighty_rows_after_sorting() {
        let mut tabs = TabList::new();
        for i in 0..100u128 {
            tabs.insert(entry(i + 1, &format!("p{i:03}"), 10, GameMode::Survival));
        }
        let view = tab_list_view(&tabs, None, &no_tr);
        assert_eq!(view.len(), MAX_TAB_ROWS);
        let names = names(&view);
        assert_eq!(names[0], "p000");
        assert_eq!(names[MAX_TAB_ROWS - 1], "p079");
    }

    /// An unlisted player is in the state and not in the overlay — vanilla's
    /// `getListedOnlinePlayers()`. Tab-completion reads the *unfiltered* set, which
    /// is why the two must not share one projection.
    #[test]
    fn an_unlisted_player_is_folded_but_not_drawn() {
        let mut tabs = TabList::new();
        let mut hidden = entry(1, "Ghost", 10, GameMode::Survival);
        hidden.listed = false;
        tabs.insert(hidden);
        tabs.insert(entry(2, "Seen", 10, GameMode::Survival));
        assert_eq!(tabs.len(), 2, "both entries stay in the fold");
        assert_eq!(names(&tab_list_view(&tabs, None, &no_tr)), vec!["Seen".to_string()]);
    }

    /// The wording of each banner line, for assertions about the split alone.
    fn banner_text(banner: Option<&Text>, tr: &dyn Fn(&str) -> Option<String>) -> Vec<String> {
        banner_lines(banner, tr)
            .iter()
            .map(|l| crate::overlay::spans_text(l))
            .collect()
    }

    #[test]
    fn a_banner_splits_on_newlines_and_an_absent_one_yields_no_lines() {
        assert_eq!(banner_text(None, &no_tr), Vec::<String>::new());
        assert_eq!(
            banner_text(Some(&Text::literal("Welcome")), &no_tr),
            vec!["Welcome".to_string()]
        );
        assert_eq!(
            banner_text(Some(&Text::literal("Top\nMiddle\nBottom")), &no_tr),
            vec![
                "Top".to_string(),
                "Middle".to_string(),
                "Bottom".to_string()
            ]
        );
        // An empty banner draws nothing rather than an empty gap — the reason
        // the HUD field is a possibly-empty slice and not an `Option`.
        assert_eq!(banner_text(Some(&Text::literal("")), &no_tr), Vec::<String>::new());
        assert_eq!(
            banner_text(Some(&Text::literal("   \n  ")), &no_tr),
            Vec::<String>::new()
        );
        // A trailing blank line is dropped; an *interior* one is kept, because a
        // server separating two banner halves with a gap means it.
        assert_eq!(
            banner_text(Some(&Text::literal("A\n\nB\n\n")), &no_tr),
            vec!["A".to_string(), String::new(), "B".to_string()]
        );
    }

    #[test]
    fn a_banner_resolves_translate_components_through_the_translator() {
        let banner = Text::translate("multiplayer.title", vec![]);
        let tr = |key: &str| (key == "multiplayer.title").then(|| "Servers".to_string());
        assert_eq!(
            banner_text(Some(&banner), &tr),
            vec!["Servers".to_string()]
        );
        // Negative control: with no table the raw key leaks, so the assertion
        // above is really measuring the translator and not a literal.
        assert_eq!(
            banner_text(Some(&banner), &no_tr),
            vec!["multiplayer.title".to_string()]
        );
    }

    /// A **hex** banner colour survives, on both sides of a line break.
    ///
    /// The discriminating input twice over: hex is the only colour a flattened
    /// `String` cannot carry (the sixteen named ones have `§` codes the font layer
    /// applies at draw time), and the break is *inside* the coloured component, so
    /// a splitter that dropped style on the second half would still get the
    /// wording right. The two lines are given **different** hex values so a
    /// transposition of the two cannot survive.
    #[test]
    fn a_hex_coloured_banner_keeps_its_colour_on_both_sides_of_a_break() {
        use lodestone_model::{TextColor, TextStyle};

        const TOP: u32 = 0x001f_2e3d;
        const BOTTOM: u32 = 0x00c4_7b19;
        let banner = Text {
            style: TextStyle {
                font: None,
                color: Some(TextColor::Rgb(TOP)),
                ..TextStyle::default()
            },
            extra: vec![Text {
                style: TextStyle {
                    font: None,
                    color: Some(TextColor::Rgb(BOTTOM)),
                    ..TextStyle::default()
                },
                ..Text::literal("Bottom")
            }],
            ..Text::literal("Top\n")
        };

        let lines = banner_lines(Some(&banner), &no_tr);
        let mut wrong = Vec::new();
        let want = [("Top", TOP), ("Bottom", BOTTOM)];
        if lines.len() != want.len() {
            wrong.push(format!(
                "line count: want {}, got {}",
                want.len(),
                lines.len()
            ));
        }
        for (i, (text, hex)) in want.iter().enumerate() {
            let Some(line) = lines.get(i) else { continue };
            let got_text = crate::overlay::spans_text(line);
            if got_text != *text {
                wrong.push(format!("line {i}: want text {text:?}, got {got_text:?}"));
            }
            let got: Vec<Option<TextColor>> = line.iter().map(|s| s.style.color).collect();
            if got != vec![Some(TextColor::Rgb(*hex))] {
                wrong.push(format!(
                    "line {i}: want colour Rgb(#{hex:06x}) throughout, got {got:?}"
                ));
            }
        }
        assert!(wrong.is_empty(), "{wrong:?}");
    }

    /// **Measures by location, not by frame average**, and the location it
    /// measures is *vertical*.
    ///
    /// The horizontal question this gate used to ask — "is the header centred
    /// rather than left-aligned?" — is no longer answerable, and that is a fact
    /// about vanilla rather than a weakening. A single column is only as wide as
    /// its own content and is itself centred on the screen
    /// (`xxo = screenWidth / 2 - blockWidth / 2`), so a centred banner and a
    /// left-aligned row land within a few pixels of each other. The old gate got
    /// its discrimination from the `"PLAYERS (n)"` caption, which was this
    /// client's own invention and is gone.
    ///
    /// What *is* separable, and is what the layout actually has to get right: the
    /// header occupies the band **above** the rows, the footer the band **below**
    /// them, and both bands are blank when the server sent no banner. Every band
    /// comes from [`crate::hud::TabPanel`] — the same value the draw lays out from
    /// — so this cannot drift into passing against an overlay that has moved.
    ///
    /// The blank-band arm is the control: it is the executed proof that the
    /// detector would have fired, and it is also the common case, since a vanilla
    /// server sends neither banner unless something sets one.
    #[test]
    #[ignore = "requires a GPU adapter"]
    fn the_header_draws_above_the_rows_and_the_footer_below_them() {
        use crate::hud::{DebugStats, HudFrame, HudRenderer, TabPanel};
        use lodestone_render::{HeadlessTarget, RenderTarget};

        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in but no adapter is available; do not treat this as a pass",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (640u32, 480u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let mut hud = HudRenderer::new(device, format);
        let stats = DebugStats::default();

        let mut tabs = TabList::new();
        tabs.insert(entry(1, "Alice", 12, GameMode::Survival));
        tabs.insert(entry(2, "Bob", 30, GameMode::Survival));
        let bare = tab_list_view(&tabs, None, &no_tr);
        // **Three** lines each, not one, and the reason is a control that failed:
        // with a one-line header the header band is `y = 10..19`, which is exactly
        // where the *rows* sit in the no-banner frame (`yyo = 10`), so the
        // no-banner arm measured 228 lit pixels of player name and read them as a
        // fabricated header. Asking "what else already paints here" is the whole
        // check. Three lines push the row block to `10 + 27 + 1 = 38`, clear of
        // everything the bare frame draws, which ends at `10 + 2 * 9 = 28`.
        tabs.header = Some(Text::literal("H1\nH2\nH3"));
        tabs.footer = Some(Text::literal("F1\nF2\nF3"));
        let banner = tab_list_view(&tabs, None, &no_tr);
        assert_eq!(banner.header.len(), 3, "the fixture must supply three header lines");
        assert_eq!(banner.footer.len(), 3, "the fixture must supply three footer lines");

        let mut render = |view: &TabListView| -> Vec<u8> {
            let frame = target.acquire().expect("headless acquire");
            clear(device, queue, frame.view());
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: false,
                players: Some(view),
                ..HudFrame::new(&stats)
            };
            hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
            target.read_texels(device, queue)
        };

        let with_banner = render(&banner);
        let without = render(&bare);

        // The canvas the HUD actually lays out into is the *logical* one, not the
        // framebuffer — reading `b.w`'s source rather than assuming 640x480 is the
        // difference between a gate and a coincidence.
        let (cw, logical_h) =
            crate::menu::render::logical_canvas(crate::config::AUTO_GUI_SCALE, w, h);
        let sy = f64::from(h) / f64::from(logical_h).max(1.0);

        // Both layouts, from the same constructor the draw calls. `max_name_width`
        // and the banner width are only needed for *horizontal* geometry, which
        // this gate does not measure, so the bands below are unaffected by passing
        // a nominal width — the y ladder is a pure function of the line counts.
        let panel_b = TabPanel::new(cw, banner.len(), false, 40.0, 3, 3, 40.0);
        let panel_n = TabPanel::new(cw, bare.len(), false, 40.0, 0, 0, 0.0);

        // Count text-bright pixels in a logical scanline band, across the whole
        // width. The overlay's own backdrop is the 0x80 black plate over the grey
        // 128 clear, then a 0x20 white row fill on top — at most ~90 per channel,
        // so a 200 threshold is text and nothing else.
        let band_bright = |px: &[u8], y0: f32, y1: f32| -> usize {
            let y_start = (f64::from(y0) * sy).round().max(0.0) as u32;
            let y_end = ((f64::from(y1) * sy).round() as u32).min(h);
            let mut n = 0usize;
            for y in y_start..y_end {
                for x in 0..w {
                    let i = ((y * w + x) * 4) as usize;
                    if px[i] > 200 && px[i + 1] > 200 && px[i + 2] > 200 {
                        n += 1;
                    }
                }
            }
            n
        };

        let hdr_band = (panel_b.header_y(0), panel_b.header_y(1));
        let ftr_band = (panel_b.footer_y(0), panel_b.footer_y(1));
        let hdr_lit = band_bright(&with_banner, hdr_band.0, hdr_band.1);
        let ftr_lit = band_bright(&with_banner, ftr_band.0, ftr_band.1);
        eprintln!(
            "header band y={hdr_band:?} lit={hdr_lit}; footer band y={ftr_band:?} lit={ftr_lit}"
        );

        assert!(
            hdr_lit > 0,
            "no header text lit in the band the layout puts it in, y={hdr_band:?}: \
             the field is folded and nothing draws it"
        );
        assert!(
            ftr_lit > 0,
            "no footer text lit in the band the layout puts it in, y={ftr_band:?}"
        );

        // The header really is *above* the rows: it pushes the row block down by its
        // own height plus vanilla's bare `yyo++`.
        assert_eq!(
            panel_b.rows_top,
            panel_n.rows_top + 3.0 * crate::hud::TAB_LINE_H + 1.0,
            "a three-line header must push the rows down 27 + 1 logical pixels"
        );
        // …and the footer below them.
        assert!(panel_b.footer_top > panel_b.rows_top);

        // **The control, in pixels, on bands the bare frame provably cannot reach.**
        //
        // The banner frame's first *row* sits at `rows_top = 38` and its footer at
        // `57`; the bare frame's whole overlay ends at `rows_top + rows * 9 = 28`.
        // So both bands must be dark in the bare frame — which is the executed
        // proof that "lit > 0" above is not satisfied by an overlay that paints
        // text everywhere, *and* the proof in pixels that the header really shifted
        // the rows rather than the layout merely claiming so.
        //
        // An earlier version of this control used the header's own band and was
        // premise-false: at one header line that band is where the bare frame's
        // rows are.
        let row_band = (panel_b.rows_top, panel_b.rows_top + crate::hud::TAB_LINE_H);
        let bare_in_row_band = band_bright(&without, row_band.0, row_band.1);
        let bare_in_ftr_band = band_bright(&without, ftr_band.0, ftr_band.1);
        eprintln!(
            "no-banner frame: shifted-row band y={row_band:?} lit={bare_in_row_band}, \
             footer band lit={bare_in_ftr_band}"
        );
        assert_eq!(
            bare_in_row_band, 0,
            "with no header the rows must still be at yyo = 10, so the band the \
             banner frame's first row occupies has to be blank here"
        );
        assert_eq!(
            bare_in_ftr_band, 0,
            "with no footer the footer band must be blank — a fabricated banner is \
             exactly what this overlay must not draw"
        );
        // …and the banner frame really does light the shifted row band, so the two
        // assertions above are measuring a difference rather than two empty rects.
        let banner_in_row_band = band_bright(&with_banner, row_band.0, row_band.1);
        assert!(
            banner_in_row_band > 0,
            "the banner frame's first row must be lit at y={row_band:?}, or the \
             control above proves nothing"
        );
    }

    #[test]
    #[ignore = "requires a GPU adapter"]
    fn tab_overlay_reaches_pixels_inside_panel_rect() {
        use crate::hud::{DebugStats, HudFrame, HudRenderer};
        use lodestone_render::{HeadlessTarget, RenderTarget};

        let ctx = lodestone_render::GpuContext::new_headless_blocking().expect(
            "headless GPU test opted in but no adapter is available; do not treat this as a pass",
        );
        let device = ctx.device();
        let queue = ctx.queue();
        let format = wgpu::TextureFormat::Rgba8Unorm;
        let (w, h) = (640u32, 480u32);
        let mut target = HeadlessTarget::new(device, w, h, format);
        let mut hud = HudRenderer::new(device, format);
        let stats = DebugStats::default();

        let mut tabs = TabList::new();
        tabs.insert(entry(1, "Alice", 12, GameMode::Survival));
        tabs.insert(entry(2, "Bob", 30, GameMode::Survival));
        let view = tab_list_view(&tabs, None, &no_tr);

        // The overlay is anchored at the **top** of the screen (`yyo = 10`), not
        // vertically centred, so the sampled rect starts at the top edge. A rect
        // centred on the canvas — which is what this gate used to sample — would
        // now measure empty space and read as a regression.
        let mut render = |players: Option<&TabListView>| -> usize {
            let frame = target.acquire().expect("headless acquire");
            clear(device, queue, frame.view());
            let hud_frame = HudFrame {
                show_debug: false,
                crosshair: false,
                players,
                ..HudFrame::new(&stats)
            };
            hud.render(device, queue, frame.view(), frame.view(), &hud_frame, w, h);
            let pixels = target.read_texels(device, queue);
            count_changed(&pixels, w, h, w / 4, 0, w / 2, h / 4)
        };

        let blank = render(None);
        let populated = render(Some(&view));
        assert_eq!(blank, 0, "empty tab overlay region must be untouched");
        assert!(
            populated > 500,
            "tab rows must paint plate/text pixels in the overlay rect, got {populated}"
        );
    }

    fn clear(device: &wgpu::Device, queue: &wgpu::Queue, view: &wgpu::TextureView) {
        let mut encoder = device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("tablist-clear"),
        });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("tablist-clear-pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 128.0 / 255.0,
                            g: 128.0 / 255.0,
                            b: 128.0 / 255.0,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
                multiview_mask: None,
            });
        }
        queue.submit(std::iter::once(encoder.finish()));
    }

    fn count_changed(
        pixels: &[u8],
        width: u32,
        _height: u32,
        x0: u32,
        y0: u32,
        rw: u32,
        rh: u32,
    ) -> usize {
        let mut changed = 0;
        for y in y0..(y0 + rh) {
            for x in x0..(x0 + rw) {
                let i = ((y * width + x) * 4) as usize;
                let d = (i32::from(pixels[i]) - 128).abs()
                    + (i32::from(pixels[i + 1]) - 128).abs()
                    + (i32::from(pixels[i + 2]) - 128).abs();
                if d > 25 {
                    changed += 1;
                }
            }
        }
        changed
    }
}
