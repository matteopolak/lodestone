//! The Server Links screen, reached from the pause menu's Server Links row.
//!
//! ## What it is
//!
//! Vanilla surfaces a server's advertised links (`SERVER_LINKS`, decoded into
//! [`lodestone_model::event::ClientEvent::ServerLinksReceived`] and folded by
//! [`lodestone_game::serverinfo::ServerInfoStore`]) through the generic Dialog
//! system: `PauseScreen.getCustomAdditions` adds a button labelled
//! `menu.server_links` ("Server Links...") whenever `!serverLinks.isEmpty()`
//! and no server-defined `PAUSE_SCREEN_ADDITIONS` dialog pre-empts it, and
//! that button opens `Dialogs.SERVER_LINKS` — a `ServerLinksDialogScreen`,
//! one row per link, each a `ClickEvent.OpenUrl` action.
//!
//! This client has no generic dialog-registry renderer (`serverinfo`'s own
//! module doc: parsing an inline dialog is a renderer's job that does not
//! exist yet), so this is a dedicated screen instead: the same flat-list
//! departure [`super::stats`]/[`super::create_world`] already make for their
//! own vanilla screens. Two views live under one [`Screen::ServerLinks`]
//! ([`super::Screen::ServerLinks`]) — a list, and a link-open confirmation —
//! the same "nest the sub-state" shape [`super::accounts`]'s sign-in flow and
//! name editor already use, rather than a second overlay screen.
//!
//! ## The clauses this reproduces
//!
//! - **The row only exists when there is something to show it.** Vanilla's own
//!   gate is `!serverLinks.isEmpty()`; [`ServerLinksNav::links`] is what
//!   [`super::nav::MenuNav::pause_buttons`] checks to decide whether
//!   [`super::nav::PauseButton::ServerLinks`] is even in the row list — not
//!   merely disabled, matching how `PauseButton::OpenToLan` is *omitted*
//!   rather than greyed out once published (see that variant's own doc).
//! - **Known links get vanilla's own captions.** `ServerLinks.KnownLinkType`'s
//!   ten `known_server_link.<name>` strings, transcribed in [`known_caption`]
//!   — vanilla's own by-id-map continuous helper with an out-of-bounds-strategy of zero means an id outside
//!   `0..=9` resolves to id `0`'s caption rather than erroring, which
//!   [`known_caption`] reproduces.
//! - **Custom links show the server's own label, and it cannot break the
//!   layout.** [`link_label`] takes `Text::to_plain_string()` — no styled
//!   spans, no embedded control characters reaching the draw — and every row
//!   is a [`MenuRow`], whose label vanilla-style rows already clip to their
//!   own rect (see `render/tests.rs`'s `long_labels_are_clipped_instead_of_
//!   overrunning_the_row`), so an oversized custom label degrades to a
//!   clipped string rather than overhanging the screen.
//! - **Opening a link asks first, naming the full URL, with vanilla's
//!   untrusted-link warning.** `Screen.clickUrlAction` opens every server
//!   link through `ConfirmLinkScreen(.., trusted: false)`: title
//!   `chat.link.confirm` ("Are you sure you want to open the following
//!   website?"), the literal URL as the message, and — because
//!   `showWarning = !trusted` — `chat.link.warning` ("Never open links from
//!   people that you don't trust!") in [`WARNING_COLOUR`] (vanilla's
//!   `-13108`, decoded — see that constant's own doc). [`LinksView::Confirm`]
//!   is that screen; [`ServerLinksOutcome::OpenUrl`] is its Yes answer, which
//!   the caller hands to
//!   [`super::accounts::open_in_browser`] — **reused, not duplicated**: that
//!   function already carries the `#[cfg(test)]` interception this repo's own
//!   record says a URL-opening call site must have (a unit test opened
//!   `login.live.com` in the owner's browser once already), so a second,
//!   independent OS-command fork here would be the same hazard reintroduced
//!   under a different name.
//!
//! ## What is deliberately not built
//!
//! - **No "Copy to Clipboard" button.** Vanilla's `ConfirmLinkScreen` has
//!   three buttons for an untrusted link (Yes / Copy to Clipboard / No); this
//!   screen has two. The URL is still shown as plain text on the confirm
//!   view, so a player who wants it can still read and retype it — copying
//!   is a convenience, not the safety property this screen exists for.
//! - **No keyboard row-stepping.** Every row is reachable by mouse (hover
//!   then click, or a direct click); Escape backs out of the confirm view or
//!   closes the screen, matching [`ConfirmNav::handle_key`](super::confirm::
//!   ConfirmNav::handle_key)'s own Escape rule. Up/Down arrow traversal is
//!   not wired — keyboard traversal is outside this screen's current input
//!   model, while mouse hover and activation remain available.
//! - **No scrolling.** A server with more links than fit the canvas simply
//!   has its lowest rows run off the bottom, uncautioned — the same
//!   deliberate flat-list departure [`super::create_world`]'s own module doc
//!   argues for. Most servers announce a handful of links (bug report,
//!   website, a rules page); wiring a real [`super::widget::ListSpec`] is a
//!   follow-up once one is observed to need it.
//!
//! ## Dependencies
//!
//! [`lodestone_model::event::{ServerLink, ServerLinkKind}`], [`super::render`]
//! for the frame types, [`super::options::Placement`] for the footer slot, and
//! [`super::accounts::open_in_browser`] for the one side effect this screen
//! can cause.

use lodestone_model::event::{ServerLink, ServerLinkKind};

use super::options::{self, Placement};
use super::render::{Align, MenuFrame, MenuLabel, MenuRow, Origin, Slot};
use super::widget;

/// `menu.server_links` (`en_us.json`) — the pause-menu row label. Vanilla's
/// own trailing ellipsis, matching every other row that opens a screen rather
/// than acting immediately (`Options...`).
pub const ROW_LABEL: &str = "Server Links...";
/// `menu.server_links.title` — this screen's own title label.
pub const TITLE: &str = "Server Links";
/// `Dialogs.SERVER_LINKS`'s own back button — `CommonComponents.GUI_BACK`
/// (vanilla's own dialogs declarations' `DEFAULT_BACK_BUTTON`), not `gui.done`: this is the one
/// vanilla screen in this cluster whose footer button says "Back".
pub const BACK_LABEL: &str = "Back";
/// `chat.link.confirm` — the untrusted-link confirmation's title. Every
/// server link takes vanilla's `trusted: false` path
/// (`Screen.clickUrlAction`), so this is the only wording this screen needs;
/// `chat.link.confirmTrusted` never applies here.
pub const CONFIRM_TITLE: &str = "Are you sure you want to open the following website?";
/// `chat.link.warning` — shown because `showWarning = !trusted` is always
/// true for a server-supplied link.
pub const CONFIRM_WARNING: &str = "Never open links from people that you don't trust!";
/// `CommonComponents.GUI_YES` — `ConfirmLinkScreen`'s affirmative label for an
/// untrusted link (`GUI_OPEN_IN_BROWSER` is the *trusted* wording only).
pub const YES_LABEL: &str = "Yes";
/// `CommonComponents.GUI_NO` — the untrusted-link negative label (a *trusted*
/// link says "Cancel" instead; never reached here).
pub const NO_LABEL: &str = "No";

/// `ConfirmLinkScreen`'s `WARNING_TEXT` colour, `-13108` as a signed ARGB
/// int with no alpha channel set (Java packs it as 24-bit RGB and the sign
/// bit is `Component.withColor`'s own encoding artefact, not a real alpha):
/// masking to the low 24 bits gives `0xFFCCCC` — R 255, G 204, B 204, a pale
/// warning red. sRGB channel values written verbatim, this shell's own
/// convention for GUI text (see `docs/vanilla-hud-text.md`); GUI text is not
/// colour-managed the way block tint/shade is.
const WARNING_COLOUR: [f32; 4] = [1.0, 204.0 / 255.0, 204.0 / 255.0, 1.0];

/// `ServerLinks.KnownLinkType`'s ten `known_server_link.<name>` captions
/// (`en_us.json`), in `KnownLinkType`'s own declaration order — which is also
/// its wire id order (vanilla's own server-links declarations: `BUG_REPORT(0, ..)` through
/// `ANNOUNCEMENTS(9, ..)`).
const KNOWN_CAPTIONS: [&str; 10] = [
    "Report Server Bug",
    "Community Guidelines",
    "Support",
    "Status",
    "Feedback",
    "Community",
    "Website",
    "Forums",
    "News",
    "Announcements",
];

/// A known link type's caption, vanilla's own by-id-map continuous helper
/// with an out-of-bounds-strategy of zero rule applied to [`KNOWN_CAPTIONS`]: an id
/// outside `0..=9` — which cannot come off a well-formed wire, but a
/// malicious or buggy server can send anything — decodes as id `0` rather
/// than panicking or dropping the row.
#[must_use]
fn known_caption(id: i32) -> &'static str {
    usize::try_from(id)
        .ok()
        .and_then(|i| KNOWN_CAPTIONS.get(i))
        .copied()
        .unwrap_or(KNOWN_CAPTIONS[0])
}

/// The label to draw for `link` — `ServerLinks.Entry::displayName`'s
/// `Either::map(KnownLinkType::displayName, identity)`, ported.
///
/// A [`ServerLinkKind::Custom`] label goes through
/// [`lodestone_model::Text::to_plain_string`] rather than a styled-span
/// render: it is server-authored and untrusted (see the module doc), and a
/// plain string clips like any other [`MenuRow::label`] instead of opening a
/// second, unbounded formatting surface.
#[must_use]
pub fn link_label(link: &ServerLink) -> String {
    match &link.kind {
        ServerLinkKind::Known(id) => known_caption(*id).to_string(),
        ServerLinkKind::Custom(text) => text.to_plain_string(),
    }
}

/// Which sub-screen [`ServerLinksNav`] is showing.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum LinksView {
    /// The flat list of links plus Back.
    #[default]
    List,
    /// The untrusted-link confirmation for one link.
    Confirm(ServerLink),
}

/// What a click on this screen resolved to.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerLinksOutcome {
    /// Internal state changed (a view switch, a no-op click); nothing for the
    /// caller to do.
    Handled,
    /// Back/Escape from the list view — the caller returns to
    /// [`super::Screen::Paused`].
    Close,
    /// The player confirmed opening this URL — the caller hands it to
    /// [`super::accounts::open_in_browser`] and then also returns to
    /// [`super::Screen::Paused`] (vanilla's `ConfirmLinkScreen` always closes
    /// back to the screen it was opened over, whichever button was pressed).
    OpenUrl(String),
}

/// This screen's own state: the server's links (pushed in every frame by
/// `app::session`'s reconciliation, the same shape
/// [`super::stats::StatsSnapshot`]/[`super::social::SocialNav`] already take
/// live session data), which view is showing, and the row the mouse is over.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ServerLinksNav {
    links: Vec<ServerLink>,
    view: LinksView,
    hovered: Option<usize>,
    chat_link: bool,
}

impl ServerLinksNav {
    /// Back to the list view with nothing hovered — the state this screen
    /// should open in every time, so a stale "which link was I confirming"
    /// never survives a re-open.
    pub fn reset(&mut self) {
        self.view = LinksView::default();
        self.hovered = None;
        self.chat_link = false;
    }

    /// Opens vanilla's untrusted-link confirmation directly for a chat URL.
    pub fn confirm_chat_url(&mut self, url: String) {
        self.view = LinksView::Confirm(ServerLink {
            kind: ServerLinkKind::Custom(lodestone_model::Text::literal("Open link")),
            url,
        });
        self.hovered = None;
        self.chat_link = true;
    }

    #[must_use]
    pub fn returns_to_chat(&self) -> bool {
        self.chat_link
    }

    /// Replaces the live link list. See the struct doc for who calls this.
    pub fn set_links(&mut self, links: Vec<ServerLink>) {
        self.links = links;
    }

    /// The server's links, in the order the packet carried them.
    #[must_use]
    pub fn links(&self) -> &[ServerLink] {
        &self.links
    }

    /// Whether the pause-menu row should even be offered — vanilla's own
    /// `!serverLinks.isEmpty()` gate.
    #[must_use]
    pub fn has_links(&self) -> bool {
        !self.links.is_empty()
    }

    /// The view currently showing.
    #[must_use]
    pub fn view(&self) -> &LinksView {
        &self.view
    }

    /// The row the mouse is over, for [`super::render::MenuFrame::hovered`].
    #[must_use]
    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    /// The mouse moved onto row `row`, in whichever view is current.
    pub fn hover(&mut self, row: usize) {
        self.hovered = Some(row);
    }

    /// A click on row `row`.
    ///
    /// List view: a row `< links.len()` opens the confirmation for that link;
    /// the row *at* `links.len()` is Back. Confirm view: row `0` is Yes, row
    /// `1` is No — [`YES_ROW`]/[`NO_ROW`].
    pub fn click_row(&mut self, row: usize) -> ServerLinksOutcome {
        match self.view.clone() {
            LinksView::List => {
                if row < self.links.len() {
                    self.view = LinksView::Confirm(self.links[row].clone());
                    self.hovered = None;
                    ServerLinksOutcome::Handled
                } else if row == self.links.len() {
                    ServerLinksOutcome::Close
                } else {
                    ServerLinksOutcome::Handled
                }
            }
            LinksView::Confirm(link) => match row {
                YES_ROW => ServerLinksOutcome::OpenUrl(link.url.clone()),
                NO_ROW => {
                    if self.chat_link {
                        return ServerLinksOutcome::Close;
                    }
                    self.view = LinksView::List;
                    self.hovered = None;
                    ServerLinksOutcome::Handled
                }
                _ => ServerLinksOutcome::Handled,
            },
        }
    }

    /// Escape: back out of the confirmation to the list, or close the whole
    /// screen from the list — vanilla's `ConfirmScreen`
    /// (`shouldCloseOnEsc() == false`, so the callback runs with `false`
    /// rather than a bare close) applied one screen up, the same rule
    /// [`super::confirm::ConfirmNav::handle_key`] follows for world deletion.
    pub fn escape(&mut self) -> ServerLinksOutcome {
        match &self.view {
            LinksView::List => ServerLinksOutcome::Close,
            LinksView::Confirm(_) => {
                if self.chat_link {
                    return ServerLinksOutcome::Close;
                }
                self.view = LinksView::List;
                self.hovered = None;
                ServerLinksOutcome::Handled
            }
        }
    }
}

/// The confirmation's affirmative row — see [`ServerLinksNav::click_row`].
pub const YES_ROW: usize = 0;
/// The confirmation's negative row.
pub const NO_ROW: usize = 1;

// -- geometry: a flat, unscrolled list, the same departure `create_world.rs`
// already documents for its own vanilla screen -----------------------------

/// Vanilla's `Dialogs.SERVER_LINKS`' own button width (vanilla's own dialogs declarations,
/// `ServerLinksDialog(.., 1, 310)`'s last argument).
const ROW_W: f32 = 310.0;
const ROW_H: f32 = options::WIDGET_H;
/// Vertical gap between two link rows.
const ROW_GAP: f32 = 4.0;
const TITLE_Y: f32 = 12.0;
const WARNING_Y: f32 = 26.0;
const ROWS_TOP: f32 = 40.0;

/// One link row's [`Slot`] — centred, stacked top to bottom.
#[must_use]
fn link_row_slot(row: usize) -> Slot {
    Slot {
        origin: Origin::ScreenTop,
        dx: -(ROW_W * 0.5),
        dy: ROWS_TOP + row as f32 * (ROW_H + ROW_GAP),
        w: ROW_W,
        h: ROW_H,
    }
}

/// The confirmation's two buttons, side by side and centred, in the
/// footer band — the same [`Placement::Footer`] every other screen's Done
/// row uses, so this frame stays inside the header/footer chrome geometry
/// like its siblings.
#[must_use]
fn confirm_button_slot(row: usize) -> Slot {
    Slot {
        origin: Origin::Settings(Placement::Footer {
            index: u8::try_from(row).unwrap_or(0),
            count: 2,
        }),
        dx: 0.0,
        dy: 0.0,
        w: options::SMALL_BUTTON_WIDTH,
        h: options::WIDGET_H,
    }
}

/// Builds the whole Server Links frame — the list view or the confirmation,
/// whichever [`ServerLinksNav::view`] says.
#[must_use]
pub fn frame(nav: &ServerLinksNav) -> MenuFrame<'static> {
    match nav.view() {
        LinksView::List => list_frame(nav),
        LinksView::Confirm(link) => confirm_frame(nav, link),
    }
}

fn list_frame(nav: &ServerLinksNav) -> MenuFrame<'static> {
    let mut rows: Vec<MenuRow> = nav
        .links()
        .iter()
        .enumerate()
        .map(|(i, link)| MenuRow {
            label: link_label(link),
            enabled: true,
            slot: Some(link_row_slot(i)),
            ..Default::default()
        })
        .collect();
    // Back sits at `links().len()` — the row right after the last link, and
    // exactly the index `ServerLinksNav::click_row`'s List arm checks against.
    rows.push(MenuRow {
        label: BACK_LABEL.to_string(),
        enabled: true,
        slot: Some(Slot {
            origin: Origin::Settings(Placement::Footer { index: 0, count: 1 }),
            dx: 0.0,
            dy: 0.0,
            w: options::SMALL_BUTTON_WIDTH,
            h: options::WIDGET_H,
        }),
        ..Default::default()
    });
    MenuFrame {
        rows,
        selected: usize::MAX,
        hovered: nav.hovered(),
        vanilla: true,
        labels: vec![
            MenuLabel {
                text: TITLE.to_string(),
                origin: Origin::ScreenTop,
                dx: 0.0,
                dy: TITLE_Y,
                align: Align::Centre,
                colour: widget::ACTIVE_LABEL,
                scale: 1.0,
            },
            // `menu.custom_options.tooltip` — vanilla draws this as a hover
            // tooltip on the pause-menu row (see the module doc); this shell
            // has no tooltip renderer (`widget.rs`'s own doc says so), so the
            // warning is relocated to a static label on the screen the row
            // leads to, where it is still read before a link can be picked.
            MenuLabel {
                text: "Note: Custom options are provided by third-party \
                       servers and/or content. Handle with care!"
                    .to_string(),
                origin: Origin::ScreenTop,
                dx: 0.0,
                dy: WARNING_Y,
                align: Align::Centre,
                colour: WARNING_COLOUR,
                scale: 1.0,
            },
        ],
        message: nav
            .links()
            .is_empty()
            .then(|| "This server announced no links.".to_string()),
        ..Default::default()
    }
}

fn confirm_frame(nav: &ServerLinksNav, link: &ServerLink) -> MenuFrame<'static> {
    let rows = vec![
        MenuRow {
            label: YES_LABEL.to_string(),
            enabled: true,
            slot: Some(confirm_button_slot(YES_ROW)),
            ..Default::default()
        },
        MenuRow {
            label: NO_LABEL.to_string(),
            enabled: true,
            slot: Some(confirm_button_slot(NO_ROW)),
            ..Default::default()
        },
    ];
    let line = |text: String, dy: f32, colour: [f32; 4]| MenuLabel {
        text,
        origin: Origin::ScreenTop,
        dx: 0.0,
        dy,
        align: Align::Centre,
        colour,
        scale: 1.0,
    };
    MenuFrame {
        rows,
        selected: usize::MAX,
        hovered: nav.hovered(),
        vanilla: true,
        labels: vec![
            line(CONFIRM_TITLE.to_string(), TITLE_Y, widget::ACTIVE_LABEL),
            line(link.url.clone(), TITLE_Y + 16.0, widget::ACTIVE_LABEL),
            line(CONFIRM_WARNING.to_string(), TITLE_Y + 32.0, WARNING_COLOUR),
        ],
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(id: i32, url: &str) -> ServerLink {
        ServerLink { kind: ServerLinkKind::Known(id), url: url.to_string() }
    }

    fn custom_link(label: &str, url: &str) -> ServerLink {
        ServerLink {
            kind: ServerLinkKind::Custom(lodestone_model::Text::literal(label)),
            url: url.to_string(),
        }
    }

    // -- known-type captions, against vanilla's own table -------------------

    #[test]
    fn known_captions_match_vanillas_en_us_strings_in_wire_id_order() {
        assert_eq!(known_caption(0), "Report Server Bug");
        assert_eq!(known_caption(1), "Community Guidelines");
        assert_eq!(known_caption(2), "Support");
        assert_eq!(known_caption(3), "Status");
        assert_eq!(known_caption(4), "Feedback");
        assert_eq!(known_caption(5), "Community");
        assert_eq!(known_caption(6), "Website");
        assert_eq!(known_caption(7), "Forums");
        assert_eq!(known_caption(8), "News");
        assert_eq!(known_caption(9), "Announcements");
    }

    /// Vanilla's own by-id-map continuous helper with an out-of-bounds-strategy of zero: an id outside
    /// `0..=9` must resolve to id 0's caption, not panic and not some other
    /// row's text.
    #[test]
    fn an_out_of_range_known_id_falls_back_to_id_zero_not_a_panic() {
        assert_eq!(known_caption(10), known_caption(0));
        assert_eq!(known_caption(-1), known_caption(0));
        assert_eq!(known_caption(i32::MAX), known_caption(0));
    }

    #[test]
    fn a_known_link_shows_its_caption_a_custom_link_shows_its_own_label() {
        assert_eq!(link_label(&link(6, "https://example.invalid")), "Website");
        assert_eq!(
            link_label(&custom_link("Our Discord", "https://example.invalid")),
            "Our Discord"
        );
    }

    // -- gating: the row only exists with real links -------------------------

    #[test]
    fn has_links_is_false_until_the_server_announces_some() {
        let mut nav = ServerLinksNav::default();
        assert!(!nav.has_links());
        nav.set_links(vec![link(6, "https://example.invalid")]);
        assert!(nav.has_links());
    }

    // -- view transitions ------------------------------------------------

    #[test]
    fn clicking_a_link_row_opens_its_confirmation_naming_that_link() {
        let mut nav = ServerLinksNav::default();
        nav.set_links(vec![
            link(6, "https://example.invalid/site"),
            link(0, "https://example.invalid/bugs"),
        ]);
        assert_eq!(nav.click_row(1), ServerLinksOutcome::Handled);
        assert_eq!(nav.view(), &LinksView::Confirm(link(0, "https://example.invalid/bugs")));
    }

    #[test]
    fn a_chat_url_enters_confirmation_and_no_or_escape_close_it() {
        let mut nav = ServerLinksNav::default();
        nav.confirm_chat_url("https://example.invalid/chat".to_string());
        assert!(nav.returns_to_chat());
        assert!(matches!(nav.view(), LinksView::Confirm(link) if link.url == "https://example.invalid/chat"));
        assert_eq!(nav.click_row(NO_ROW), ServerLinksOutcome::Close);

        nav.confirm_chat_url("https://example.invalid/escape".to_string());
        assert_eq!(nav.escape(), ServerLinksOutcome::Close);
    }

    #[test]
    fn confirming_a_chat_url_yields_only_that_url() {
        let mut nav = ServerLinksNav::default();
        nav.confirm_chat_url("https://example.invalid/confirmed".to_string());
        assert_eq!(
            nav.click_row(YES_ROW),
            ServerLinksOutcome::OpenUrl("https://example.invalid/confirmed".to_string())
        );
    }

    #[test]
    fn clicking_back_in_the_list_view_closes_the_screen() {
        let mut nav = ServerLinksNav::default();
        nav.set_links(vec![link(6, "https://example.invalid")]);
        // Back is the row right after the last link.
        assert_eq!(nav.click_row(1), ServerLinksOutcome::Close);
        assert_eq!(nav.view(), &LinksView::List, "closing must not change the view");
    }

    #[test]
    fn a_row_past_back_does_nothing() {
        let mut nav = ServerLinksNav::default();
        nav.set_links(vec![link(6, "https://example.invalid")]);
        assert_eq!(nav.click_row(5), ServerLinksOutcome::Handled);
        assert_eq!(nav.view(), &LinksView::List);
    }

    #[test]
    fn yes_answers_with_the_confirmed_links_own_url_not_a_different_one() {
        let mut nav = ServerLinksNav::default();
        // Pairwise-distinct URLs (CLAUDE.md's own rule): a transposition
        // between the two links must be visible.
        nav.set_links(vec![
            link(6, "https://example.invalid/eleven"),
            link(0, "https://example.invalid/four"),
        ]);
        nav.click_row(0);
        assert_eq!(
            nav.click_row(YES_ROW),
            ServerLinksOutcome::OpenUrl("https://example.invalid/eleven".to_string())
        );
    }

    #[test]
    fn no_returns_to_the_list_without_opening_anything() {
        let mut nav = ServerLinksNav::default();
        nav.set_links(vec![link(6, "https://example.invalid")]);
        nav.click_row(0);
        assert_eq!(nav.click_row(NO_ROW), ServerLinksOutcome::Handled);
        assert_eq!(nav.view(), &LinksView::List);
    }

    #[test]
    fn escape_backs_out_of_the_confirmation_first_then_closes() {
        let mut nav = ServerLinksNav::default();
        nav.set_links(vec![link(6, "https://example.invalid")]);
        nav.click_row(0);
        assert!(matches!(nav.view(), LinksView::Confirm(_)));
        assert_eq!(nav.escape(), ServerLinksOutcome::Handled);
        assert_eq!(nav.view(), &LinksView::List, "first Escape backs out, not closes");
        assert_eq!(nav.escape(), ServerLinksOutcome::Close, "second Escape closes");
    }

    #[test]
    fn reset_returns_to_the_list_with_nothing_hovered() {
        let mut nav = ServerLinksNav::default();
        nav.set_links(vec![link(6, "https://example.invalid")]);
        nav.click_row(0);
        nav.hover(YES_ROW);
        nav.reset();
        assert_eq!(nav.view(), &LinksView::List);
        assert_eq!(nav.hovered(), None);
    }

    // -- hover --------------------------------------------------------------

    #[test]
    fn hover_reaches_the_frame() {
        let mut nav = ServerLinksNav::default();
        nav.set_links(vec![link(6, "https://example.invalid")]);
        assert_eq!(frame(&nav).hovered, None);
        nav.hover(0);
        assert_eq!(frame(&nav).hovered, Some(0));
    }

    // -- the frame ------------------------------------------------------

    #[test]
    fn the_list_frame_has_one_row_per_link_plus_back_and_the_warning_label() {
        let mut nav = ServerLinksNav::default();
        nav.set_links(vec![
            link(6, "https://example.invalid/a"),
            custom_link("Rules", "https://example.invalid/b"),
        ]);
        let f = frame(&nav);
        assert_eq!(f.rows.len(), 3, "two links plus Back");
        assert_eq!(f.rows[0].label, "Website");
        assert_eq!(f.rows[1].label, "Rules");
        assert_eq!(f.rows[2].label, BACK_LABEL);
        assert!(
            f.labels.iter().any(|l| l.text.contains("Handle with care")),
            "the custom-options warning must reach the frame: {:?}",
            f.labels
        );
        assert!(f.labels.iter().any(|l| l.text == TITLE));
        // No row highlighted until something is hovered — the same
        // "opens with nothing focused" rule `StatsNav`/`ConfirmNav` follow.
        assert_eq!(f.selected, usize::MAX);
    }

    #[test]
    fn the_confirm_frame_names_the_full_url_and_carries_the_warning() {
        let mut nav = ServerLinksNav::default();
        nav.set_links(vec![link(6, "https://example.invalid/exact-url")]);
        nav.click_row(0);
        let f = frame(&nav);
        assert_eq!(f.rows.len(), 2, "Yes and No");
        assert_eq!(f.rows[YES_ROW].label, YES_LABEL);
        assert_eq!(f.rows[NO_ROW].label, NO_LABEL);
        assert!(
            f.labels.iter().any(|l| l.text == "https://example.invalid/exact-url"),
            "the confirmation must show the exact URL, not a truncated or \
             re-derived one: {:?}",
            f.labels
        );
        assert!(f.labels.iter().any(|l| l.text == CONFIRM_WARNING));
        // The control: the warning's colour must actually differ from plain
        // white, or a gate reading `labels` could not tell it apart from the
        // title/URL lines.
        let warning = f.labels.iter().find(|l| l.text == CONFIRM_WARNING).unwrap();
        assert_ne!(warning.colour, widget::ACTIVE_LABEL);
        assert_eq!(warning.colour, WARNING_COLOUR);
    }

    #[test]
    fn link_rows_are_stacked_top_to_bottom_with_no_overlap() {
        let mut prev_bottom = f32::MIN;
        for row in 0..5 {
            let (_, y, _, h) = link_row_slot(row).resolve(854.0, 480.0);
            assert!(y >= prev_bottom, "row {row} at y={y} overlaps the row above it");
            prev_bottom = y + h;
        }
    }
}
