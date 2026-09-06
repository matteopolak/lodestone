use super::{Screen, SessionKind};

/// The title screen's widgets, in vanilla's own display order.
///
/// This is vanilla's own title-screen init's widget list,
/// reproduced whole rather than trimmed to what this client implements.
/// [`MainButton::enabled`] is what marks the rest **present but greyed out**,
/// which is the faithful thing: a button missing from its vanilla position is a
/// layout that reads wrong, while a disabled one in the right position reads
/// exactly like vanilla with the feature unavailable (which is a state vanilla
/// itself ships — multiplayer and Realms are disabled for a
/// banned account, vanilla's own title-screen rendering).
///
/// The three 20×20 icon buttons use the shared icon-button row;
/// the layout positions them evenly across the row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainButton {
    /// Open the singleplayer world list ([`Screen::WorldSelect`]) —
    /// vanilla's own behaviour for this button. It used to return
    /// [`MenuAction::Singleplayer`] and launch directly, which vanilla never
    /// does; that action is now produced one screen in, by **Play Selected
    /// World**.
    Singleplayer,
    /// Open the server list.
    Multiplayer,
    /// Vanilla's `menu.online` row. Present and disabled: Realms is a paid
    /// Mojang-hosted service with its own authenticated HTTP API, none of which
    /// exists here and none of which is on the roadmap.
    Realms,
    /// The friends icon button. Present and
    /// disabled: it needs a Microsoft-account social graph.
    Friends,
    /// The language icon button opens the language selector directly from the
    /// title screen,
    /// never through the root options screen. **Live** through
    /// [`super::options::SettingsPage::Language`]; the icon opens that page.
    /// This keeps the title-screen entry point separate from the root-grid row.
    /// Opens the same page the root grid's "Language..." row does, but with
    /// an empty page stack (see
    /// [`super::options::SettingsNav::open_at`]) so Escape/Done returns
    /// straight to the title, matching the direct title-screen entry path.
    Language,
    /// The accessibility icon opens [`super::options::SettingsPage::Accessibility`]
    /// directly, using an empty page stack so Escape or Done returns to the title
    /// screen just as it does for the language icon.
    Accessibility,
    /// Open the settings screen.
    Options,
    /// Quit the game.
    Quit,
    /// Open the account list. **Not a vanilla widget** — unlike
    /// every other row in this enum, there is no vanilla's own title-screen rendering line to
    /// cite for it. The game has no in-game account switcher at all:
    /// an account is chosen once, outside the game, by the separate
    /// separate launcher, and the game client just uses whatever it was
    /// handed. Lodestone has no separate launcher, so the game itself has to
    /// own this. [`title_slot`] places it below vanilla's own four rows
    /// rather than inserting it into their grid, so it cannot be mistaken
    /// for a reproduced vanilla rect.
    Accounts,
}
/// Every title-screen widget, in vanilla's display order. Indices are the one
/// index space shared by keyboard selection, mouse hover, hit-testing and the
/// renderer — see [`super::render::title_slot`].
pub const MAIN_BUTTONS: [MainButton; 9] = [
    MainButton::Singleplayer,
    MainButton::Multiplayer,
    MainButton::Realms,
    MainButton::Friends,
    MainButton::Language,
    MainButton::Accessibility,
    MainButton::Options,
    MainButton::Quit,
    // Not part of vanilla's own eight — see `MainButton::Accounts`'s docs.
    // Appended last rather than inserted into the vanilla run so every
    // vanilla row keeps its original index.
    MainButton::Accounts,
];

/// The two widgets on the ownership gate ([`Screen::Ownership`]).
///
/// Deliberately only two, and deliberately not a title screen with everything
/// greyed: a greyed row invites a click and reads as "temporarily unavailable",
/// while a screen with one thing to do reads as "do this". The gate is not a
/// nag, so there is no "Continue anyway".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OwnershipButton {
    /// Open [`Screen::Accounts`] — the *same* account switcher the title screen
    /// reaches, writing the *same* roster. Adding an account here therefore adds
    /// it to the switcher by construction; there is no second store and no
    /// second sign-in path to keep in sync.
    AddAccount,
    /// Quit the game. Present on every host, unlike the title screen's own Quit
    /// (see [`MainButton::enabled_on`]): a browser tab cannot end its process,
    /// but a gate whose only two rows are "add an account" and one that does
    /// nothing is worse than one row.
    Quit,
}

impl OwnershipButton {
    /// The button's caption.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            OwnershipButton::AddAccount => "Add Account",
            OwnershipButton::Quit => "Quit Game",
        }
    }
}

/// The ownership gate's widgets, in display order. As with [`MAIN_BUTTONS`],
/// these indices are the one index space shared by keyboard selection, mouse
/// hover, hit-testing and the renderer.
pub const OWNERSHIP_BUTTONS: [OwnershipButton; 2] =
    [OwnershipButton::AddAccount, OwnershipButton::Quit];

/// Whether this build can end its own process, which is what **Quit Game**
/// means. False in a browser tab — see [`MainButton::enabled_on`].
pub const CAN_EXIT_PROCESS: bool = !cfg!(target_arch = "wasm32");

/// Whether this build is allowed to initiate a remote multiplayer session.
///
/// This is a build capability, not a temporary server-list failure: without the
/// `multiplayer` feature the button remains in its vanilla position but cannot
/// open a screen that could send traffic through a remote server or browser
/// relay.
pub const MULTIPLAYER_ENABLED: bool = cfg!(feature = "multiplayer");

impl MainButton {
    /// The label drawn on the button, or narrated for an icon-only one.
    ///
    /// The English language-table labels for these controls. Mixed case now,
    /// not upper — the title screen draws through the real vanilla font, which
    /// has lower-case glyphs.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            MainButton::Singleplayer => "Singleplayer",
            MainButton::Multiplayer => "Multiplayer",
            MainButton::Realms => "Minecraft Realms",
            MainButton::Friends => "Friends",
            MainButton::Language => "Language...",
            MainButton::Accessibility => "Accessibility Settings...",
            MainButton::Options => "Options...",
            MainButton::Quit => "Quit Game",
            MainButton::Accounts => "Accounts",
        }
    }

    /// Whether the button can be activated. A `false` here is what draws
    /// vanilla's `widget/button_disabled` sprite with a dimmed label and makes
    /// keyboard navigation step over the row — see [`MainButton`]'s docs for why
    /// each disabled one is still present.
    ///
    /// Delegates to [`Self::enabled_on`] with this build's real answer for
    /// whether a process exists to end.
    #[must_use]
    pub fn enabled(self) -> bool {
        self.enabled_on(CAN_EXIT_PROCESS)
    }

    /// [`Self::enabled`] with the host capability passed in rather than read
    /// from `cfg!`, so **both** arms are reachable from a test on either
    /// target. A `cfg!` read inline here would make the browser's answer
    /// unobservable from the native suite, which is the whole corpus — and a
    /// rule no gate can exercise is documentation of intent, not a guard.
    #[must_use]
    pub fn enabled_on(self, can_exit_process: bool) -> bool {
        match self {
            // A browser tab has no process to end. `event_loop.exit()` — what
            // `quit_requested` latches into — stops the loop and leaves a dead
            // canvas rather than closing anything, and `window.close()` is
            // refused for any page the script did not itself open. So the row
            // is present and greyed, exactly as `Realms` and `Friends` are:
            // the state vanilla itself ships for a feature that is unavailable
            // rather than absent.
            MainButton::Quit => can_exit_process,
            MainButton::Multiplayer => MULTIPLAYER_ENABLED,
            MainButton::Singleplayer
            | MainButton::Options
            | MainButton::Accounts
            // Both destination screens are built now — see the variants' own
            // docs.
            | MainButton::Language
            | MainButton::Accessibility => true,
            MainButton::Realms => false,
            MainButton::Friends => true,
        }
    }

    /// A capability explanation for an otherwise-present disabled button.
    #[must_use]
    pub fn tooltip(self) -> Option<&'static str> {
        match self {
            MainButton::Multiplayer if !MULTIPLAYER_ENABLED => {
                Some("Multiplayer is disabled in this build of the game.")
            }
            _ => None,
        }
    }

    /// The GUI sprite drawn centred in the button instead of a label —
    /// A centered icon, 15×15 inside a 20×20 button.
    #[must_use]
    pub fn icon(self) -> Option<&'static str> {
        match self {
            MainButton::Friends => Some("friends/friends"),
            MainButton::Language => Some("icon/language"),
            MainButton::Accessibility => Some("icon/accessibility"),
            _ => None,
        }
    }
}

/// The pause screen's widgets, in vanilla's own display order.
///
/// This is vanilla's own pause-screen "create pause menu" grid
/// reproduced whole, and it is **not** the three-button stack people remember:
/// a full-width Back to Game, then a two-column Advancements / Statistics row,
/// then a centred row of four 20×20 icon buttons, then Options, then
/// Disconnect. The exact rects are in [`super::render::pause_slot`].
///
/// An earlier version of this file *omitted* Advancements and Statistics on the
/// grounds that neither has a client-side subsystem to open onto, so either
/// button would reach zero pixels. That reasoning still holds for the
/// *action* — which is why they are [`PauseButton::enabled`]-`false` — but it
/// does not hold for the *position*: a greyed-out button where the reference
/// client puts one is faithful UI, and it greys these out (the player-reporting icon
/// with no players to report, vanilla's own pause-screen rendering).
///
/// Which Options layout is reproduced is a real fork in vanilla:
/// integrated-server availability splits the row into Options + Open to LAN
///, and only the `else` branch gives Options the
/// full 204 px width. This client has no integrated
/// server at all (see the module docs), so that availability is
/// unconditionally false for it and the full-width branch is the correct one.
///
/// The reference client labels its last button from the local-session state — "Save and Quit to
/// Title" locally, "Disconnect" remotely. This
/// client uses "Disconnect" for both, because [`SessionKind::Singleplayer`] is
/// currently the local dev world with no persistence: "Save and Quit" would
/// promise a save that does not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseButton {
    /// Resume play. Equivalent to Escape. Vanilla's `menu.returnToGame`.
    BackToGame,
    /// Vanilla's `gui.advancements` — opens [`super::Screen::Advancements`].
    /// **Live, and showing real progress**: this used to be
    /// present-and-disabled because nothing decoded `UPDATE_ADVANCEMENTS`, and
    /// both halves of that wire have since landed. See [`super::advancements`].
    Advancements,
    /// The Statistics entry — opens [`super::Screen::Statistics`]. **Live.**
    /// `award_stats` is decoded into `lodestone_ecs::SessionStatistics`, and
    /// `app::session` refreshes the snapshot through [`MenuNav::refresh_stats`]
    /// once per frame.
    /// The default snapshot remains zero-valued until the session supplies data.
    Statistics,
    /// Vanilla's `menu.reportBugs` icon button. Present and disabled: it opens
    /// an external Mojang bug tracker through a link-confirmation screen.
    ReportBugs,
    /// Vanilla's `menu.sendFeedback` icon button. Present and disabled: same,
    /// an external Mojang link.
    Feedback,
    /// Vanilla's friends icon button. Present and disabled, as on the title
    /// screen: it needs a Microsoft-account social graph.
    Friends,
    /// Vanilla's `menu.playerReporting` icon button — opens
    /// [`super::Screen::Social`], vanilla's
    /// the social interactions screen. **Now live**, not present-and-disabled:
    /// the screen itself (an online-player list with a Hide/Show-in-Chat
    /// toggle) needs nothing this button's own disabled reason used to name.
    /// What is *still* gated is one control **inside** that screen — every
    /// row's Report button, because that needs the chat-signature/secure
    /// chat-signing context this client does not have (see
    /// [`super::social`]'s module docs). If secure chat signing lands, that
    /// is the doc to update, not this one — this comment used to be the only
    /// place the dependency was written down, and own tracking
    /// note flagged that as a trap because comments drift; it no longer needs
    /// to be, now that `super::social`'s module docs carry it instead.
    PlayerReporting,
    /// Vanilla's `menu.server_links` — opens [`super::Screen::ServerLinks`].
    /// See that variant's own doc and [`super::server_links`]'s module doc
    /// for the vanilla dialog this reproduces as a dedicated screen.
    ///
    /// **Not in [`PAUSE_BUTTONS`]/[`PAUSE_BUTTONS_PUBLISHED`]** — those two
    /// arrays are the pause **grid**'s own membership, and this row is
    /// deliberately drawn *outside* the arranged grid (see
    /// [`super::render::pause_slot`]'s `ServerLinks` arm), the same
    /// "outside the arranged tree" shape [`MainButton::Accounts`] already
    /// uses on the title screen. [`MenuNav::pause_buttons`] appends it
    /// dynamically, and only when the server actually announced a link —
    /// the reference client's non-empty-links gate, reproduced as an
    /// *omission* rather than a disabled row, matching
    /// [`Self::OpenToLan`]'s own precedent for a row with nothing to offer.
    ServerLinks,
    /// Open the settings screen (reuses [`super::Screen::Settings`] — see
    /// [`super::UiState::open_settings_from_pause`]).
    Options,
    /// Vanilla's `menu.multiplayerOptions.button`, whose `en_us` value really is
    /// **"Open to LAN"** — the half-width sibling of [`Self::Options`] that
    /// The pause grid includes this half-width action only when an integrated
    /// server is available.
    ///
    /// **Conditionally present, since scope 2 — but not for the
    /// reason a first read of the reference client suggests.** The pause screen
    /// decompile shows this row whenever
    /// integrated-server availability is true **regardless of publish state**: it
    /// is the options form behind the button that changes,
    /// an on/off cycle control seeded from the integrated-server publish state
    /// — vanilla never re-presses a "publish"
    /// action against an already-published world because the same button
    /// re-opens a form that can also *unpublish*. This client has no such
    /// form — [`MenuAction::OpenToLan`]'s consumer is a single-shot publish
    /// with no toggle-off — so once the world *is* published there is nothing
    /// left for this row to honestly offer, and [`MenuNav::pause_buttons`]
    /// omits it, matching the *shape* of vanilla's non-singleplayer branch
    /// (`Options` alone, full width, `:160-164`) applied to a different
    /// condition (published, not "no integrated server"). See
    /// `crates/lodestone-shell/src/net.rs`'s `NetUpdate::LanPublishError` for
    /// what happens if a stale render still sends a publish anyway — it must
    /// never be able to disconnect the session, so the omission here is a
    /// polish fix, not the only guard.
    ///
    /// The reference client opens an options form with a LAN/online
    /// toggle, a port field, a game mode and allow-commands. This publishes
    /// straight away on [`crate::net::LAN_DEFAULT_PORT`] with commands on; the
    /// form is a screen of its own and the action it would submit is this one.
    OpenToLan,
    /// Leave the session for the title screen.
    QuitToTitle,
}

/// Every pause-screen widget, in vanilla's display order, for a session that
/// is not publishing anything. As with [`MAIN_BUTTONS`], these indices are
/// the one index space keyboard selection, mouse hover, hit-testing and the
/// renderer all share **while this is the active list** — see
/// [`MenuNav::pause_buttons`], which is what every internal user of a pause
/// row list actually calls; this constant and
/// [`PAUSE_BUTTONS_PUBLISHED`] are its two possible answers.
pub const PAUSE_BUTTONS: [PauseButton; 10] = [
    PauseButton::BackToGame,
    PauseButton::Advancements,
    PauseButton::Statistics,
    PauseButton::ReportBugs,
    PauseButton::Feedback,
    PauseButton::Friends,
    PauseButton::PlayerReporting,
    PauseButton::Options,
    PauseButton::OpenToLan,
    PauseButton::QuitToTitle,
];

/// [`PAUSE_BUTTONS`], minus [`PauseButton::OpenToLan`] — the row list once
/// the world is published (scope 2). See that variant's own doc
/// for why this is an *omission* rather than a disabled row: this client's
/// Open to LAN has no unpublish/toggle form behind it, unlike vanilla's, so a
/// published world has nothing left for the row to do.
pub const PAUSE_BUTTONS_PUBLISHED: [PauseButton; 9] = [
    PauseButton::BackToGame,
    PauseButton::Advancements,
    PauseButton::Statistics,
    PauseButton::ReportBugs,
    PauseButton::Feedback,
    PauseButton::Friends,
    PauseButton::PlayerReporting,
    PauseButton::Options,
    PauseButton::QuitToTitle,
];

impl PauseButton {
    /// The label drawn on the button, or narrated for an icon-only one.
    /// The English language-table labels.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            PauseButton::BackToGame => "Back to Game",
            PauseButton::Advancements => "Advancements",
            PauseButton::Statistics => "Statistics",
            PauseButton::ReportBugs => "Report Bugs",
            PauseButton::Feedback => "Give Feedback",
            PauseButton::Friends => "Friends",
            PauseButton::PlayerReporting => "Player Reporting",
            PauseButton::ServerLinks => super::server_links::ROW_LABEL,
            PauseButton::Options => "Options...",
            PauseButton::OpenToLan => "Open to LAN",
            PauseButton::QuitToTitle => "Disconnect",
        }
    }

    /// Whether the button can be activated — see [`MainButton::enabled`].
    #[must_use]
    pub fn enabled(self) -> bool {
        matches!(
            self,
            PauseButton::BackToGame
                | PauseButton::Options
                | PauseButton::QuitToTitle
                // Player Reporting screen is built; see the
                // variant's own doc for what is and is not wired inside it.
                | PauseButton::PlayerReporting
                // Statistics has a built screen as well.
                | PauseButton::Statistics
                // The Advancements screen exists, and its
                // `UPDATE_ADVANCEMENTS` decode shows real progress —
                // see `menu::advancements`' module docs.
                | PauseButton::Advancements
                | PauseButton::Friends
                // hosted-world opener has a caller. Always
                // enabled rather than session-aware, because `enabled` is a pure
                // function of the variant at every call site — see the variant's
                // own doc for what happens outside a hosted world.
                | PauseButton::OpenToLan
                // This screen is real (see `super::server_links`), and the
                // row itself is never present without a real link to show —
                // see `PauseButton::ServerLinks`'s own doc.
                | PauseButton::ServerLinks
        )
    }

    /// The GUI sprite drawn centred in the button instead of a label, 15×15
    /// inside a 20×20 button.
    #[must_use]
    pub fn icon(self) -> Option<&'static str> {
        match self {
            PauseButton::ReportBugs => Some("pause_menu/bug"),
            PauseButton::Feedback => Some("pause_menu/social_interactions"),
            PauseButton::Friends => Some("friends/friends"),
            PauseButton::PlayerReporting => Some("pause_menu/player_reporting"),
            _ => None,
        }
    }
}

/// The multiplayer screen's title, centred in the header band.
pub const SERVER_LIST_TITLE: &str = "Play Multiplayer";

/// The multiplayer screen's seven footer buttons, in the order they are
/// added to the two footer rows — which is
/// also the order [`super::render::server_list_footer_slot`] reads out of the
/// arranged layout, and the order the rows appear in after the server entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerListButton {
    /// `selectServer.select` — join the selected server. Inactive with nothing
    /// selected.
    Select,
    /// `selectServer.direct` — connect to an address without saving it.
    ///
    /// **Present and inactive.** It would open a second
    /// address form this shell does not have; the add form
    /// ([`Screen::ServerEdit`]) is the affordance it would duplicate, minus the
    /// "do not save it" part. Greyed out rather than omitted, which is this
    /// repo's rule for a vanilla control it cannot honour yet — see
    /// `docs/menu-widgets.md`.
    Direct,
    /// `selectServer.add` — open the add form.
    Add,
    /// `selectServer.edit` — open the edit form for the selection.
    Edit,
    /// `selectServer.delete` — remove the selection.
    Delete,
    /// `selectServer.refresh` — re-ping every row.
    Refresh,
    /// `gui.back` — leave the screen.
    Back,
}

/// Every multiplayer footer button, in vanilla's own order. As with
/// [`MAIN_BUTTONS`], the indices are one index space — here offset by the number
/// of server rows above them, which is what
/// [`MenuNav::click`]/[`MenuNav::hover`] translate.
pub const SERVER_LIST_BUTTONS: [ServerListButton; 7] = [
    ServerListButton::Select,
    ServerListButton::Direct,
    ServerListButton::Add,
    ServerListButton::Edit,
    ServerListButton::Delete,
    ServerListButton::Refresh,
    ServerListButton::Back,
];

impl ServerListButton {
    /// The English language-table labels.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ServerListButton::Select => "Join Server",
            ServerListButton::Direct => "Direct Connection",
            ServerListButton::Add => "Add Server",
            ServerListButton::Edit => "Edit",
            ServerListButton::Delete => "Delete",
            ServerListButton::Refresh => "Refresh",
            ServerListButton::Back => "Back",
        }
    }

    /// Vanilla's declared width: 100 for the top row, 74 for the lower one.
    ///
    /// The **draw** does not read this — [`super::render::server_list_footer_slot`]
    /// returns the width the arranged layout produced, which is the number that
    /// reaches pixels. It is here so a test can assert the two agree, which is
    /// what would catch a footer built with the rows swapped.
    #[must_use]
    pub fn width(self) -> f32 {
        match self {
            ServerListButton::Select | ServerListButton::Direct | ServerListButton::Add => 100.0,
            ServerListButton::Edit
            | ServerListButton::Delete
            | ServerListButton::Refresh
            | ServerListButton::Back => 74.0,
        }
    }

    /// The server-list selection rule: Join, Edit and
    /// Delete all start `false`, a selection enables Join, and only an
    /// `OnlineServerEntry` also enables Edit and Delete.
    ///
    /// **Two deviations, both because this shell's list is narrower than
    /// vanilla's, not because the rule was simplified:**
    ///
    /// - Vanilla's selection starts as **null** even with a non-empty list, so
    ///   the three buttons are inactive until the player clicks or arrows onto a
    ///   row. This shell has a keyboard row cursor that always points at a row
    ///   when there is one (that is what `MenuNav::server_index` is), and no
    ///   "nothing selected" state to model — so `has_selection` is
    ///   `!list.is_empty()`. The disabled path is therefore reached by an *empty*
    ///   list rather than by a fresh one.
    /// - Vanilla's Edit/Delete are inactive for a **LAN** entry, which is neither
    ///   editable nor deletable. There is no LAN discovery here
    ///   (`LanServerDetection` has no port), so every row is the equivalent of an
    ///   `OnlineServerEntry` and the two conditions collapse into one. If LAN
    ///   rows ever land, this is the function that has to split them apart again.
    #[must_use]
    pub fn enabled(self, has_selection: bool) -> bool {
        match self {
            ServerListButton::Select | ServerListButton::Edit | ServerListButton::Delete => {
                has_selection
            }
            // Nothing to point at; see the variant's own doc.
            ServerListButton::Direct => false,
            ServerListButton::Add | ServerListButton::Refresh | ServerListButton::Back => true,
        }
    }
}

/// The death screen's two widgets, vanilla's
/// the death screen's two controls. Both live; unlike
/// [`MainButton`]/[`PauseButton`] there is nothing present-and-disabled here
/// — vanilla itself only ever shows these two.
///
/// No hardcore variant: this client has no hardcore mode (nothing decodes a
/// client-visible hardcore flag), so vanilla's fork —
/// the alternate spectator/no-confirm branch when hardcore, and the respawn
/// otherwise — always takes the non-hardcore branch. See [`super::render::death_frame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeathButton {
    /// The respawn control ("Respawn"): submit a manual `ClientAction::Respawn`.
    Respawn,
    /// The title-screen control ("Title Screen"): leave for the main menu.
    /// Vanilla pops a confirm dialog first (skipped, non-hardcore only) or
    /// disconnects straight away (hardcore) — this client always takes the
    /// pause menu's own simplification of skipping the confirm, the same
    /// scope cut named for `PauseButton::QuitToTitle`.
    TitleScreen,
}

/// Every death-screen widget, in vanilla's display order — the one index
/// space keyboard selection, mouse hover, hit-testing and the renderer share,
/// same as [`MAIN_BUTTONS`]/[`PAUSE_BUTTONS`].
pub const DEATH_BUTTONS: [DeathButton; 2] = [DeathButton::Respawn, DeathButton::TitleScreen];

impl DeathButton {
    /// The English language-table labels.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            DeathButton::Respawn => "Respawn",
            DeathButton::TitleScreen => "Title Screen",
        }
    }
}
