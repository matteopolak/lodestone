//! The shell's screen / session **state machine**, and the menu built on it.
//!
//! This file is structure only — which screen is showing and every legal edge
//! between them. The rest of the menu lives in the submodules:
//!
//! | module | what it owns |
//! |---|---|
//! | [`nav`] | selection, the add/edit form, what a keypress means |
//! | [`options`] | the whole settings tree, unsupported controls disabled |
//! | [`render`] | layout + a self-contained GPU pipeline |
//! | [`servers`] | the saved server list and its on-disk JSON |
//! | [`status`] | background status pings and their cache |
//! | [`widget`] | vanilla's widget contract, and the disabled render path |
//! | [`world_select`] | the singleplayer world list, with creation disabled |
//!
//! The lifecycle is the actual hard part: choosing a session, establishing it,
//! playing, pausing, and every way it can fail. A menu that can only succeed is
//! the same class of defect as a test that can only pass, so the failure edges
//! (connection refused, server-start failure, disconnect mid-game, quit while
//! still loading) are modelled and tested first-class here.
//!
//! This owns cursor-grab intent, whether gameplay input is consumed, and the
//! clean-shutdown latch; [`crate::app`] maps those queries onto winit and drives
//! the transitions from real session signals. The type is pure and `Clone`, so
//! every edge is unit-testable without a window, a GPU, or a server.
//!
//! ## Singleplayer vs multiplayer (the entry point)
//!
//! [`SessionKind::Singleplayer`] means vanilla's architecture: start an
//! integrated server in-process and connect to it over a local transport — the
//! *same* client and dispatch as multiplayer, a different transport.
//! [`SessionKind::Multiplayer`] connects to a remote address. Both are now real
//! (issue #287): `app.rs`'s `launch_singleplayer` starts
//! `lodestone_server::IntegratedServer` on an in-memory duplex and the same
//! client speaks to it, so there is one client and one dispatch, differing only
//! in transport. A build with no version family compiled in has no server
//! protocol to run and fails loudly into [`Screen::Error`] rather than silently
//! doing nothing.

pub mod accounts;
pub mod create_world;
pub mod edit_box;
pub mod focus;
pub mod key_binds;
pub mod layout;
pub mod nav;
pub mod options;
pub mod panorama;
pub mod render;
pub mod servers;
pub mod social;
pub mod stats;
pub mod status;
pub mod widget;
pub mod world_select;

/// What the player chose to start from the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    /// Local integrated server (vanilla's singleplayer): start a server
    /// in-process and connect to it over a local transport.
    Singleplayer,
    /// Connect to a remote address.
    Multiplayer,
}

/// Which screen the shell is showing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screen {
    /// Title screen: choose Singleplayer / Multiplayer / Quit.
    MainMenu,
    /// The multiplayer server list: pick a saved server, or add/edit/delete one.
    /// Reached from [`Screen::MainMenu`]; Escape returns there.
    ServerList,
    /// The add/edit form for one server entry. Reached from
    /// [`Screen::ServerList`]; Escape returns there **without** saving.
    ServerEdit,
    /// The singleplayer world list (issue #397): vanilla's `SelectWorldScreen`,
    /// with **world creation present and disabled**. Reached from
    /// [`Screen::MainMenu`] via [`nav::MainButton::Singleplayer`] — which is
    /// vanilla's own wiring, where the title screen's Singleplayer button opens
    /// this screen rather than launching anything; Escape (or the screen's own
    /// Back button) returns there.
    ///
    /// The list holds **exactly one** world (#287's
    /// [`world_select::BUNDLED_WORLD`]): a fixed seed the integrated server
    /// regenerates on every launch, never written to disk. There is still no
    /// world storage in this client and no world creation (#190), so one is the
    /// honest count — a second row would have to be invented. Its **Play
    /// Selected World** button is live and starts a real session; see
    /// [`world_select`] for the row's geometry and the launch chain.
    WorldSelect,
    /// The account list (issue #66): saved Microsoft accounts plus the
    /// always-present offline entry, and the device-code sign-in flow that
    /// adds a new account. Reached from [`Screen::MainMenu`] via
    /// [`nav::MainButton::Accounts`]; Escape (or the screen's own Cancel
    /// button) returns there. Not a vanilla screen at all — see
    /// [`nav::MainButton::Accounts`]'s docs for why real Minecraft has
    /// nothing equivalent to reproduce.
    Accounts,
    /// The settings screen — vanilla's whole `OptionsScreen` tree (issue #55),
    /// nine pages of it, with every control present and the majority this
    /// client does not honour drawn inactive (118 of 143 outside a world, 119
    /// inside one — the root's Online button is the one row whose liveness
    /// depends on it). Which page is showing lives in
    /// [`options::SettingsNav`], **not** here: the pages are a graph rather than
    /// screen states (Accessibility links to Controls, which the root also
    /// links to), and Escape unwinds a history stack instead of a
    /// [`Screen`] edge.
    ///
    /// Reached from [`Screen::MainMenu`] (the title's Options button) or from
    /// [`Screen::Paused`] (the pause menu's Options button, mid-session);
    /// Escape from the *root* page returns to whichever it was opened from —
    /// see [`UiState::close_settings`] and
    /// [`UiState::settings_in_world`]. Changes persist immediately (see
    /// [`crate::config::Options`]), not on exit.
    Settings,
    /// A session is being established — integrated-server startup and/or the
    /// connect handshake. Nothing is playable yet; the pointer is free so the
    /// user can still cancel (quit) while it loads.
    Connecting,
    /// In the world: pointer grabbed, keyboard/mouse drive the player.
    Playing,
    /// The chat box is open over the world: the pointer is released and gameplay
    /// input is frozen (typed keys go to the chat line, not the player), but the
    /// world behind keeps rendering and ticking. A sub-mode of [`Playing`] rather
    /// than a full screen — Escape or submit returns straight to it.
    Chat,
    /// A container or the player inventory is open over the world: pointer
    /// released and gameplay input frozen while the world keeps rendering.
    Container,
    /// Paused overlay: pointer released, player input frozen. The world behind
    /// keeps rendering and — on a live server — keeps ticking; pausing is a
    /// *local* UI state, not a world stop. Reachable from [`Screen::Playing`]
    /// (Escape or a focus loss) and from [`Screen::Chat`]/[`Screen::Container`]
    /// (a focus loss only — Escape from those cancels back to `Playing`
    /// instead). Draws its own button rows (Back to Game, Options, Quit to
    /// Title) as an overlay over the still-rendering world — see
    /// [`render::pause_frame`] and [`render::MenuRenderer::render_overlay`];
    /// deliberately **not** an [`render::owns_frame`] screen, because that set
    /// clears the frame and would stop the world rendering behind it.
    Paused,
    /// The local player died (issue #103): vanilla's `DeathScreen` — the "You
    /// Died!" title, the server's death message, a score line, and two
    /// buttons (Respawn / Title Screen). Reachable from every live gameplay
    /// screen (`Playing`, `Chat`, `Container`, `Paused` — vanilla replaces
    /// whatever screen was open the instant the death packet lands, see
    /// [`die`](Self::die)) on [`crate::sim::Sim::is_dead`] going true, and
    /// left only by [`respawn_confirmed`](Self::respawn_confirmed) once the
    /// server confirms the respawn the Respawn button asked for
    /// (`Sim::respawn`) — **not** by [`on_escape`](Self::on_escape), which is
    /// a deliberate no-op here: vanilla's `DeathScreen.shouldCloseOnEsc()`
    /// returns `false`, so Escape does not dismiss it.
    ///
    /// Drawn the same way [`Screen::Paused`] is — an overlay over the
    /// still-rendering, still-ticking world (`render::death_frame` +
    /// `MenuRenderer::render_overlay`), **not** an [`render::owns_frame`]
    /// screen — for the same reason: a live server holds a dead player with
    /// no chunk stream until it respawns (see `CLAUDE.md`'s dead-player note),
    /// and this screen must not itself go blank or stop the session ticking
    /// while that holds. See `docs/pause-menu.md`'s note on this screen.
    Death,
    /// A session failed to establish or ended unexpectedly. `error()` carries the
    /// human-readable reason; the only ways forward are back to the menu or quit.
    Error,
    /// The end-poem/credits roll (issue #192): vanilla's `WinScreen`, shown
    /// after the dragon fight and exiting the End through the exit portal.
    ///
    /// **What this is not**: vanilla's `WinScreen` auto-scrolls a ~1500-word
    /// poem plus a real Mojang employee credits roll, driven by elapsed time
    /// (`WinScreen.java`'s own tick counter). Two things rule that out here —
    /// see [`render::credits_frame`] for the full reasoning:
    /// 1. [`render::frame_for`] is a pure function of [`UiState`]/[`nav::MenuNav`]
    ///    with no elapsed-time input, so a real auto-scroll needs a per-frame
    ///    tick reaching this state machine, which nothing here provides yet
    ///    (`app.rs`'s `pano.advance(Instant::now())` is the one place this
    ///    codebase already does that, and it lives outside this crate's frame
    ///    model).
    /// 2. Mojang's end-poem text and its own credited employee list are not
    ///    this project's to reproduce or to relabel as Lodestone's own — see
    ///    the module docs on [`render::credits_frame`].
    ///
    /// So this screen shows a short, Lodestone-authored placeholder message
    /// instead of vanilla's real scroll, dismissed by Enter/Escape/its own
    /// Done button — reachable through [`Self::show_credits`], which nothing
    /// calls yet: the real trigger (dragon defeat, exit portal) is
    /// server/ECS-driven and outside this crate (`sim.rs`/`app.rs`), so this
    /// is an island until that patch lands. See issue #192's own tracking
    /// comment for the exact hook.
    Credits,
    /// The Social Interactions screen (issue #189): vanilla's
    /// `SocialInteractionsScreen`, an online-player list with a per-player
    /// Hide/Show-in-Chat toggle and a Report button. Reached from the pause
    /// menu's Player Reporting icon button
    /// ([`nav::PauseButton::PlayerReporting`]); Escape or the screen's own
    /// Done button returns to [`Screen::Paused`].
    ///
    /// Vanilla itself shows this screen's real list only in a multiplayer
    /// session (`multiplayer.socialInteractions.not_available`) — see
    /// [`social::available_for`] for the fork, which this client's own
    /// [`SessionKind`] already carries. The Report button stays permanently
    /// inactive regardless of session kind: it needs secure chat signing,
    /// which does not exist here (see [`social`]'s module docs).
    Social,
    /// The Statistics screen (issue #188): vanilla's `StatsScreen`. Reached
    /// from the pause menu's Statistics button
    /// ([`nav::PauseButton::Statistics`], now live); Escape or the screen's
    /// own Done button returns to [`Screen::Paused`].
    ///
    /// Only the General tab (vanilla's 77 fixed stats) is a real list; Items
    /// and Mobs are present-and-inactive, which is not an approximation —
    /// see [`stats`]'s module docs for why vanilla's own screen would show
    /// exactly that given the same (zero) data. Every value on every tab
    /// reads zero: nothing in this workspace decodes the statistics packet,
    /// so [`stats::StatsSnapshot::default`] is not a placeholder, it is the
    /// only data that has ever existed here.
    Statistics,
    /// The World Creation screen (issue #190): vanilla's `CreateWorldScreen`,
    /// reduced to one flat hand-placed list (see [`create_world`]'s module
    /// docs for why). Reached from [`Screen::WorldSelect`]'s "Create New
    /// World" button — issue #397 left it present-and-disabled for exactly
    /// this issue; Escape or the screen's own Cancel button returns to
    /// [`Screen::WorldSelect`].
    ///
    /// Collecting a name/seed/game-mode/difficulty/structures/bonus-chest/
    /// allow-cheats config is real; **nothing downstream reads it yet** — the
    /// integrated server still launches [`world_select::BUNDLED_WORLD`]'s
    /// fixed seed regardless of what this screen collected. See
    /// [`create_world`]'s module docs for the queued patch.
    CreateWorld,
}

impl Screen {
    /// Every variant, in declaration order.
    ///
    /// Exists so a test that has to walk *all* screens iterates the enum instead
    /// of restating a count.
    /// `render::tests::owns_frame_agrees_with_frame_for_on_every_screen` asserted
    /// a literal `12` and went red the moment [`Screen::WorldSelect`] landed
    /// (#397) — which is `CLAUDE.md`'s staleness class, in the one place it is
    /// most annoying: a test that is *about* completeness, made incomplete by the
    /// thing it was meant to notice.
    ///
    /// **What keeps this complete.** The length is written next to the list, so
    /// adding an entry without bumping `13` (or bumping it without adding one) is
    /// a compile error; `screen_all_lists_each_variant_once` rules out the
    /// copy-paste that keeps the length right and silently drops a screen.
    ///
    /// **What does not.** Rust cannot enumerate an enum's variants without a
    /// derive macro, so a variant added to `Screen` and *not* added here is
    /// caught only by the exhaustive `match` inside whatever loop consumes this —
    /// which forces an arm to be written, but cannot force the array entry. That
    /// residue is real; it is stated rather than papered over. If a third
    /// consumer ever needs this, a derive is the fix, not another hand-written
    /// list.
    pub const ALL: [Screen; 17] = [
        Screen::MainMenu,
        Screen::ServerList,
        Screen::ServerEdit,
        Screen::WorldSelect,
        Screen::Accounts,
        Screen::Settings,
        Screen::Connecting,
        Screen::Playing,
        Screen::Chat,
        Screen::Container,
        Screen::Paused,
        Screen::Death,
        Screen::Error,
        Screen::Credits,
        Screen::Social,
        Screen::Statistics,
        Screen::CreateWorld,
    ];
}

/// The shell's top-level UI state. One instance lives in the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiState {
    screen: Screen,
    kind: Option<SessionKind>,
    error: Option<String>,
    quit_requested: bool,
    /// Where Escape (or the settings screen's own back action) returns to from
    /// [`Screen::Settings`] — [`Screen::MainMenu`] or [`Screen::Paused`],
    /// whichever opened it. See [`UiState::open_settings`],
    /// [`UiState::open_settings_from_pause`] and [`UiState::close_settings`].
    settings_return: Screen,
    /// The current death's message, populated only on [`Screen::Death`] — see
    /// [`Self::die`]. Mirrors how [`Self::error`] carries `Screen::Error`'s
    /// reason.
    death_message: Option<String>,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            screen: Screen::MainMenu,
            kind: None,
            error: None,
            quit_requested: false,
            settings_return: Screen::MainMenu,
            death_message: None,
        }
    }
}

impl UiState {
    /// A fresh state at the title screen.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    // -- queries ----------------------------------------------------------

    /// The current screen.
    #[must_use]
    pub fn screen(&self) -> Screen {
        self.screen
    }

    /// The session kind being established or played, if any.
    #[must_use]
    pub fn kind(&self) -> Option<SessionKind> {
        self.kind
    }

    /// The failure reason, populated only in [`Screen::Error`].
    #[must_use]
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether the world is being actively played.
    #[must_use]
    pub fn is_playing(&self) -> bool {
        self.screen == Screen::Playing
    }

    /// Whether the pause overlay is up.
    #[must_use]
    pub fn is_paused(&self) -> bool {
        self.screen == Screen::Paused
    }

    /// Whether the death screen is up.
    #[must_use]
    pub fn is_death(&self) -> bool {
        self.screen == Screen::Death
    }

    /// The current death's message, populated only on [`Screen::Death`].
    #[must_use]
    pub fn death_message(&self) -> Option<&str> {
        self.death_message.as_deref()
    }

    /// Whether the chat box is open over the world.
    #[must_use]
    pub fn is_chat_open(&self) -> bool {
        self.screen == Screen::Chat
    }

    /// Whether a container/inventory screen is open over the world.
    #[must_use]
    pub fn is_container_open(&self) -> bool {
        self.screen == Screen::Container
    }

    /// Whether a session is currently being established.
    #[must_use]
    pub fn is_connecting(&self) -> bool {
        self.screen == Screen::Connecting
    }

    /// Whether the multiplayer server list is showing.
    #[must_use]
    pub fn is_server_list(&self) -> bool {
        self.screen == Screen::ServerList
    }

    /// Whether the add/edit form is showing.
    #[must_use]
    pub fn is_server_edit(&self) -> bool {
        self.screen == Screen::ServerEdit
    }

    /// Whether the settings screen is showing.
    #[must_use]
    pub fn is_settings(&self) -> bool {
        self.screen == Screen::Settings
    }

    /// Whether the shell is on any pre-session menu screen, i.e. no world is
    /// loaded and the menu renderer owns the frame.
    ///
    /// [`Screen::Settings`] is included even though it is reachable
    /// mid-session too (from [`Screen::Paused`]'s Options button, see
    /// [`open_settings_from_pause`](Self::open_settings_from_pause)): it is
    /// still an [`render::owns_frame`] screen either way, so the world's
    /// *rendering* pauses for as long as Settings is up regardless of how it
    /// was reached — only its ticking and networking are guaranteed to
    /// continue (see the module docs' note on gating input, not the network).
    #[must_use]
    pub fn is_menu(&self) -> bool {
        matches!(
            self.screen,
            Screen::MainMenu
                | Screen::ServerList
                | Screen::ServerEdit
                | Screen::WorldSelect
                | Screen::Settings
                | Screen::Accounts
        )
    }

    /// Whether the singleplayer world list is showing (issue #397).
    #[must_use]
    pub fn is_world_select(&self) -> bool {
        self.screen == Screen::WorldSelect
    }

    /// Whether the account list is showing.
    #[must_use]
    pub fn is_accounts(&self) -> bool {
        self.screen == Screen::Accounts
    }

    /// The pointer should be grabbed **exactly** when playing. The app calls
    /// `set_grab(ui.wants_cursor_grab())` after every transition rather than
    /// tracking grab separately.
    #[must_use]
    pub fn wants_cursor_grab(&self) -> bool {
        self.screen == Screen::Playing
    }

    /// Gameplay keyboard/mouse is fed to the player **exactly** when playing.
    #[must_use]
    pub fn accepts_gameplay_input(&self) -> bool {
        self.screen == Screen::Playing
    }

    /// Whether a clean shutdown has been requested.
    #[must_use]
    pub fn quit_requested(&self) -> bool {
        self.quit_requested
    }

    // -- session lifecycle ------------------------------------------------

    /// Chose a session kind from the menu; begin establishing it. Clears any
    /// stale error and moves to [`Screen::Connecting`].
    pub fn begin(&mut self, kind: SessionKind) {
        self.kind = Some(kind);
        self.error = None;
        self.screen = Screen::Connecting;
    }

    /// Enter the local dev world directly (the shell's `worldgen` stand-in).
    /// Distinct from [`SessionKind::Singleplayer`], which starts a real
    /// integrated server (#287); this is a no-session Playing state with no
    /// server behind it, reached only from a `--headless`/dev entry point rather
    /// than from any menu button.
    pub fn enter_dev_world(&mut self) {
        self.kind = None;
        self.error = None;
        self.screen = Screen::Playing;
    }

    /// The session is established; enter the world. Only meaningful while
    /// connecting, so a late/duplicate signal from a torn-down session can't
    /// yank the player out of a menu or error screen.
    pub fn session_ready(&mut self) {
        if self.screen == Screen::Connecting {
            self.screen = Screen::Playing;
        }
    }

    /// The session failed to establish or ended unexpectedly. Valid from any
    /// live screen (connecting, playing, paused) — a mid-game disconnect and a
    /// refused connection land in the same place with a reason.
    ///
    /// [`Screen::Settings`] is included **conditionally**: only when it was
    /// opened from the pause menu (`settings_return == Paused`), i.e. the
    /// player is mid-session tweaking GUI scale. Settings opened from the
    /// title screen is deliberately excluded, same as `MainMenu` itself —
    /// otherwise a stray disconnect signal from an abandoned connection
    /// attempt could reach in and yank the player out of the pre-session
    /// Options screen for no session they were ever in.
    pub fn session_failed(&mut self, reason: impl Into<String>) {
        let mid_session_settings =
            self.screen == Screen::Settings && self.settings_return == Screen::Paused;
        // Ignore failures once we've already left for the menu, so a trailing
        // error from a shutting-down session doesn't resurrect the error screen.
        //
        // `Screen::Death` is included (issue #103): a live server holds a dead
        // player with no chunk stream until it respawns, so a disconnect while
        // the death screen is up is a real failure the player needs to see,
        // not something that silently strands them on a screen whose Respawn
        // button will now never get an answer — exactly the "held on the
        // death screen, silent total chunk blackout" symptom `CLAUDE.md`
        // warns about, here from a different cause (a genuine disconnect
        // rather than the offline-mode UUID collision that note names).
        if mid_session_settings
            || matches!(
                self.screen,
                Screen::Connecting
                    | Screen::Playing
                    | Screen::Chat
                    | Screen::Container
                    | Screen::Paused
                    | Screen::Death
                    | Screen::Error
                    // Same reasoning as `Screen::Death` above: a disconnect
                    // while Social Interactions is open (#189) must not
                    // silently strand the player on a screen backed by a
                    // session that no longer exists.
                    | Screen::Social
                    // Same reasoning, for Statistics (#188).
                    | Screen::Statistics
            )
        {
            self.death_message = None;
            self.error = Some(reason.into());
            self.screen = Screen::Error;
        }
    }

    /// Dismiss the error screen back to the title.
    pub fn dismiss_error(&mut self) {
        if self.screen == Screen::Error {
            self.error = None;
            self.kind = None;
            self.screen = Screen::MainMenu;
        }
    }

    // -- menu navigation --------------------------------------------------

    /// Open the multiplayer server list. Only from the title screen, so a stray
    /// call cannot pull the player out of a world.
    pub fn open_server_list(&mut self) {
        if self.screen == Screen::MainMenu {
            self.screen = Screen::ServerList;
        }
    }

    /// Back to the title screen from the server list.
    pub fn close_server_list(&mut self) {
        if self.screen == Screen::ServerList {
            self.screen = Screen::MainMenu;
        }
    }

    /// Open the add/edit form. Only from the server list.
    pub fn open_server_edit(&mut self) {
        if self.screen == Screen::ServerList {
            self.screen = Screen::ServerEdit;
        }
    }

    /// Leave the add/edit form, whether the entry was saved or cancelled.
    pub fn close_server_edit(&mut self) {
        if self.screen == Screen::ServerEdit {
            self.screen = Screen::ServerList;
        }
    }

    /// Open the singleplayer world list (issue #397). Only from the title
    /// screen, matching [`open_server_list`](Self::open_server_list)'s
    /// reasoning: a stray call must never pull the player out of a world.
    ///
    /// This is what the title screen's Singleplayer button does, and it is
    /// vanilla's own wiring — `TitleScreen`'s Singleplayer button opens
    /// `SelectWorldScreen`; nothing in vanilla starts a world straight off the
    /// title. The launch is one screen further in: **Play Selected World**
    /// produces [`nav::MenuAction::Singleplayer`], which `app.rs` turns into a
    /// real integrated-server session (#287). Between #397 and #287 that action
    /// had no producer at all and was kept as exactly this seam.
    pub fn open_world_select(&mut self) {
        if self.screen == Screen::MainMenu {
            self.screen = Screen::WorldSelect;
        }
    }

    /// Back to the title screen from the world list — vanilla's
    /// `SelectWorldScreen.onClose()`, which is `setScreen(this.lastScreen)`
    /// (`SelectWorldScreen.java:154-157`), and also what its Back button does
    /// (`:106`).
    pub fn close_world_select(&mut self) {
        if self.screen == Screen::WorldSelect {
            self.screen = Screen::MainMenu;
        }
    }

    /// Open the account list. Only from the title screen, matching
    /// [`open_server_list`](Self::open_server_list)'s reasoning: a stray call
    /// must never pull the player out of a world.
    pub fn open_accounts(&mut self) {
        if self.screen == Screen::MainMenu {
            self.screen = Screen::Accounts;
        }
    }

    /// Back to the title screen from the account list.
    pub fn close_accounts(&mut self) {
        if self.screen == Screen::Accounts {
            self.screen = Screen::MainMenu;
        }
    }

    /// Open the settings screen from the title. Only from the title screen,
    /// matching [`open_server_list`](Self::open_server_list)'s reasoning: a
    /// stray call must never pull the player out of a world.
    pub fn open_settings(&mut self) {
        if self.screen == Screen::MainMenu {
            self.settings_return = Screen::MainMenu;
            self.screen = Screen::Settings;
        }
    }

    /// Open the settings screen from the pause menu's Options button. Only
    /// from [`Screen::Paused`] — the mid-session counterpart to
    /// [`open_settings`](Self::open_settings), so Escape (or the equivalent
    /// Back action) returns to the paused world instead of skipping past it to
    /// the title screen. See [`close_settings`](Self::close_settings).
    pub fn open_settings_from_pause(&mut self) {
        if self.screen == Screen::Paused {
            self.settings_return = Screen::Paused;
            self.screen = Screen::Settings;
        }
    }

    /// Back to whichever screen opened settings — the title screen or the
    /// pause menu. This is what makes Escape from Options a genuine *stack*:
    /// Options opened from the pause menu must return to the pause menu, not
    /// fall all the way through to the title and drop a live session.
    pub fn close_settings(&mut self) {
        if self.screen == Screen::Settings {
            self.screen = self.settings_return;
        }
    }

    /// Whether the settings screen was opened **from inside a world**, i.e. from
    /// the pause menu rather than the title.
    ///
    /// This is vanilla's `inWorld` flag on `OptionsScreen`
    /// (`OptionsScreen.java:41-47`), and it picks between two mutually exclusive
    /// buttons in the root screen's header: `options.worldOptions.button` when a
    /// level is loaded, `options.online` when not (`:56-66`). It reads
    /// [`Self::settings_return`], which is already the exact fact — see
    /// [`open_settings_from_pause`](Self::open_settings_from_pause) — so this is
    /// an accessor and not a second piece of state to keep in step.
    #[must_use]
    pub fn settings_in_world(&self) -> bool {
        self.settings_return == Screen::Paused
    }

    /// Open the Social Interactions screen (issue #189) from the pause
    /// menu's Player Reporting button. Only from [`Screen::Paused`] — vanilla
    /// has no title-screen entry point for it at all (there is no session to
    /// list players from before one exists), so unlike
    /// [`Self::open_settings`]/[`open_settings_from_pause`](Self::open_settings_from_pause)
    /// this needs no return-fork: it always came from the pause menu, so
    /// [`Self::close_social`] always goes back there.
    pub fn open_social_from_pause(&mut self) {
        if self.screen == Screen::Paused {
            self.screen = Screen::Social;
        }
    }

    /// Back to the pause menu from Social Interactions.
    pub fn close_social(&mut self) {
        if self.screen == Screen::Social {
            self.screen = Screen::Paused;
        }
    }

    /// Open the Statistics screen (issue #188) from the pause menu's
    /// Statistics button. Only from [`Screen::Paused`] — same reasoning as
    /// [`Self::open_social_from_pause`]: vanilla has no title-screen entry
    /// point, there being no session to have accrued any stats in before one
    /// exists.
    pub fn open_statistics_from_pause(&mut self) {
        if self.screen == Screen::Paused {
            self.screen = Screen::Statistics;
        }
    }

    /// Back to the pause menu from Statistics.
    pub fn close_statistics(&mut self) {
        if self.screen == Screen::Statistics {
            self.screen = Screen::Paused;
        }
    }

    /// Open the World Creation screen (issue #190) from the world list's
    /// "Create New World" button. Only from [`Screen::WorldSelect`] — same
    /// reasoning as every other `open_*_from_*`: a stray call must not pull
    /// the player out of wherever they actually are.
    pub fn open_create_world(&mut self) {
        if self.screen == Screen::WorldSelect {
            self.screen = Screen::CreateWorld;
        }
    }

    /// Back to the world list from World Creation — Escape or Cancel, and
    /// (today) Create too, since nothing yet launches a world from the
    /// collected config (see [`create_world`]'s module docs).
    pub fn close_create_world(&mut self) {
        if self.screen == Screen::CreateWorld {
            self.screen = Screen::WorldSelect;
        }
    }

    // -- input-driven transitions ----------------------------------------

    /// Open the chat box over the world. Only from [`Screen::Playing`]; opening
    /// releases the pointer and freezes gameplay input (both keyed off
    /// `is_playing`), so typed keys reach the chat line instead of the player.
    pub fn open_chat(&mut self) {
        if self.screen == Screen::Playing {
            self.screen = Screen::Chat;
        }
    }

    /// Close the chat box back to the world, whether the line was sent or
    /// cancelled. Only from [`Screen::Chat`]; never resurrects a menu/error.
    pub fn close_chat(&mut self) {
        if self.screen == Screen::Chat {
            self.screen = Screen::Playing;
        }
    }

    /// Open a container or the player inventory over the world.
    pub fn open_container(&mut self) {
        if self.screen == Screen::Playing {
            self.screen = Screen::Container;
        }
    }

    /// Close the container/inventory screen back to the world.
    pub fn close_container(&mut self) {
        if self.screen == Screen::Container {
            self.screen = Screen::Playing;
        }
    }

    /// Escape, interpreted by screen:
    /// - Playing → Paused, Paused → Playing
    /// - Chat → Playing (cancel the line)
    /// - Error → back to the menu (dismiss)
    /// - ServerEdit → ServerList (cancel the edit)
    /// - ServerList → MainMenu
    /// - WorldSelect → MainMenu
    /// - Settings → MainMenu, **or** Paused if that is where it was opened
    ///   from (see [`close_settings`](Self::close_settings))
    /// - MainMenu → request a clean quit (Escape on the title exits)
    /// - Connecting → no-op (can't pause mid-connect; the app offers quit-to-cancel)
    ///
    /// Note the menu screens unwind **one level at a time**: Escape from the
    /// edit form must not skip past the list and quit the game, and — the same
    /// rule applied mid-session — Escape from Options opened out of the pause
    /// menu must not skip past the pause menu and drop straight into play.
    pub fn on_escape(&mut self) {
        match self.screen {
            Screen::Playing => self.screen = Screen::Paused,
            Screen::Paused => self.screen = Screen::Playing,
            Screen::Chat | Screen::Container => self.screen = Screen::Playing,
            Screen::Error => self.dismiss_error(),
            Screen::ServerEdit => self.screen = Screen::ServerList,
            Screen::ServerList => self.screen = Screen::MainMenu,
            // As with `Screen::Accounts` below, `MenuNav::key_world_select`
            // normally answers Escape before this is reached (it routes every key
            // through vanilla's `Screen.keyPressed` order, whose Escape branch is
            // `onClose()`). This arm keeps the match exhaustive and unwinds one
            // level, which is the same thing.
            Screen::WorldSelect => self.close_world_select(),
            // In practice `MenuNav::key_accounts` intercepts Escape before
            // `UiState::on_escape` is ever reached from this screen (a
            // sign-in in progress must cancel the flow, not leave the
            // screen) — this arm exists so the match stays exhaustive and so
            // a caller that *does* reach here unwinds one level, matching
            // every other menu screen's rule.
            Screen::Accounts => self.close_accounts(),
            Screen::Settings => self.close_settings(),
            Screen::MainMenu => self.request_quit(),
            Screen::Connecting => {}
            // Deliberately a no-op, not "unwind one level" like every screen
            // above: vanilla's `DeathScreen.shouldCloseOnEsc()` returns
            // `false` (`DeathScreen.java:64-66`), so Escape does not dismiss
            // it. `MenuNav::key_death` mirrors this — it does not call
            // `on_escape` for `MenuKey::Escape` the way every other screen's
            // key handler does — so this arm exists only so the match stays
            // exhaustive against a caller that reaches here some other way.
            Screen::Death => {}
            // In practice `MenuNav::key_credits` intercepts Escape before
            // this is reached (same reasoning as `Screen::Accounts` above),
            // and it leaves through `quit_to_title` rather than an ordinary
            // one-level unwind — there is no "back" from the end-poem screen
            // in vanilla either, only "return to title". This arm keeps the
            // match exhaustive and does the same thing for a caller that
            // reaches here some other way.
            Screen::Credits => self.quit_to_title(),
            // In practice `MenuNav::key_social` intercepts Escape before this
            // is reached (same reasoning as `Screen::Accounts` above) — this
            // arm exists so the match stays exhaustive and unwinds one level
            // like every ordinary sub-screen (unlike `Screen::Credits`, this
            // one has a real "back", the pause menu it was opened from).
            Screen::Social => self.close_social(),
            // Same reasoning as `Screen::Social` immediately above.
            Screen::Statistics => self.close_statistics(),
            // Same reasoning again — back to the world list.
            Screen::CreateWorld => self.close_create_world(),
        }
    }

    /// Force the pause overlay up (e.g. the window lost focus). Meaningful while
    /// playing or with chat open (a focus loss must not leave input captured);
    /// a focus loss on a menu/loading/error screen is a no-op.
    pub fn pause(&mut self) {
        if matches!(
            self.screen,
            Screen::Playing | Screen::Chat | Screen::Container
        ) {
            self.screen = Screen::Paused;
        }
    }

    /// Return to the world from the pause overlay (e.g. a click). Only from
    /// paused — never resurrects a failed or loading session.
    pub fn resume(&mut self) {
        if self.screen == Screen::Paused {
            self.screen = Screen::Playing;
        }
    }

    /// Leave a live session for the title screen — the pause menu's "Quit to
    /// Title" button, and (issue #103) the death screen's "Title Screen"
    /// button. Only from [`Screen::Paused`] or [`Screen::Death`], matching
    /// every other "leave a screen" guard here: a stray call must never cut a
    /// live session out from under the player who didn't ask for it.
    ///
    /// Deliberately **not** routed through [`session_failed`](Self::session_failed):
    /// this is not an error, so no `error` is set and the title screen shows
    /// plainly rather than with a disconnect reason that never happened. The
    /// app still owns tearing down the actual network/session resources in
    /// reaction to the [`nav::MenuAction`] this produces — this method only
    /// moves the screen.
    pub fn quit_to_title(&mut self) {
        if matches!(self.screen, Screen::Paused | Screen::Death | Screen::Credits) {
            self.kind = None;
            self.error = None;
            self.death_message = None;
            self.screen = Screen::MainMenu;
        }
    }

    /// Show the end-poem/credits screen (issue #192): vanilla's `WinScreen`,
    /// reached by exiting the End through the exit portal after the dragon
    /// fight. Valid from the same live-gameplay screens as [`Self::die`] —
    /// mirroring that guard rather than inventing a different one, since both
    /// are "the server just ended this session's world for a story reason"
    /// events.
    ///
    /// **Nothing calls this yet.** The real trigger is server/ECS-driven (a
    /// dimension-change flag on exiting the End) and lives outside this
    /// crate's ownership for this batch of work — see [`Screen::Credits`]'s
    /// own doc. Until that patch lands this is reachable only from a test,
    /// which is this method's whole risk: an island by this project's own
    /// definition, kept as exactly the seam the trigger will land on (the
    /// same shape [`nav::MenuAction::Singleplayer`] was between #397 and #287).
    pub fn show_credits(&mut self) {
        if matches!(
            self.screen,
            Screen::Playing | Screen::Chat | Screen::Container | Screen::Paused
        ) {
            self.screen = Screen::Credits;
        }
    }

    /// The local player died (issue #103): show the death screen. Valid from
    /// every live gameplay screen — `Playing`, `Chat`, `Container`, `Paused`
    /// — matching vanilla, which replaces whatever screen is open the instant
    /// the death packet lands (`ClientPacketListener` sets the death screen
    /// unconditionally, not only from `Playing`). `message` is the server's
    /// death message, already flattened to plain text — see
    /// `net::NetUpdate::Death` and `Sim::death_message`.
    ///
    /// Called once per death from `app.rs`'s per-frame reconciliation, guarded
    /// there on `!ui.is_death()` so a death that is still being processed
    /// (`Sim::is_dead()` staying `true` across many frames while the screen
    /// waits for a click) does not re-latch every frame — harmless here either
    /// way since the guard below is idempotent, but the caller's guard is what
    /// keeps `message` from being overwritten by a later, unrelated call.
    pub fn die(&mut self, message: Option<String>) {
        if matches!(
            self.screen,
            Screen::Playing | Screen::Chat | Screen::Container | Screen::Paused
        ) {
            self.death_message = message;
            self.screen = Screen::Death;
        }
    }

    /// The server confirmed the respawn the death screen's Respawn button
    /// asked for (`Sim::respawn`): leave the death screen for the world. Only
    /// from [`Screen::Death`] — never resurrects a menu/error/loading screen.
    pub fn respawn_confirmed(&mut self) {
        if self.screen == Screen::Death {
            self.death_message = None;
            self.screen = Screen::Playing;
        }
    }

    /// Ask the shell to shut down cleanly. Latches — the app polls
    /// [`quit_requested`](Self::quit_requested) after handling each event and
    /// exits the loop when set, letting `Drop` tear down the net thread (and,
    /// when it exists, the integrated server) with no leak, even mid-load.
    pub fn request_quit(&mut self) {
        self.quit_requested = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn starts_at_the_title_with_cursor_free_and_input_off() {
        let ui = UiState::new();
        assert_eq!(ui.screen(), Screen::MainMenu);
        assert!(!ui.wants_cursor_grab(), "no world yet, no grab");
        assert!(!ui.accepts_gameplay_input());
        assert!(ui.kind().is_none());
        assert!(ui.error().is_none());
        assert!(!ui.quit_requested());
    }

    #[test]
    fn dev_world_enters_play_directly() {
        let mut ui = UiState::new();
        ui.enter_dev_world();
        assert_eq!(ui.screen(), Screen::Playing);
        assert!(ui.wants_cursor_grab());
        assert!(ui.accepts_gameplay_input());
        assert!(ui.kind().is_none(), "dev world is not a real session kind");
    }

    #[test]
    fn happy_path_singleplayer_and_multiplayer_reach_play() {
        for kind in [SessionKind::Singleplayer, SessionKind::Multiplayer] {
            let mut ui = UiState::new();
            ui.begin(kind);
            assert_eq!(ui.screen(), Screen::Connecting);
            assert!(!ui.wants_cursor_grab(), "no grab while loading");
            assert_eq!(ui.kind(), Some(kind));
            ui.session_ready();
            assert_eq!(ui.screen(), Screen::Playing);
            assert!(ui.wants_cursor_grab());
        }
    }

    #[test]
    fn connection_refused_goes_to_error_with_reason() {
        let mut ui = UiState::new();
        ui.begin(SessionKind::Multiplayer);
        ui.session_failed("connection refused (os error 61)");
        assert_eq!(ui.screen(), Screen::Error);
        assert_eq!(ui.error(), Some("connection refused (os error 61)"));
        assert!(!ui.wants_cursor_grab());
        assert!(!ui.accepts_gameplay_input());
    }

    #[test]
    fn server_startup_failure_goes_to_error() {
        let mut ui = UiState::new();
        ui.begin(SessionKind::Singleplayer);
        ui.session_failed("integrated server failed to start: bind :25565 in use");
        assert_eq!(ui.screen(), Screen::Error);
        assert!(ui.error().unwrap().contains("failed to start"));
    }

    #[test]
    fn disconnect_mid_game_goes_to_error_from_playing_and_paused() {
        for pause_first in [false, true] {
            let mut ui = UiState::new();
            ui.begin(SessionKind::Multiplayer);
            ui.session_ready();
            if pause_first {
                ui.pause();
                assert!(ui.is_paused());
            }
            ui.session_failed("disconnected: Server closed");
            assert_eq!(ui.screen(), Screen::Error, "pause_first={pause_first}");
            assert_eq!(ui.error(), Some("disconnected: Server closed"));
        }
    }

    #[test]
    fn quit_while_loading_latches_without_leaving_connecting() {
        // Shutdown mid-load: the state stays Connecting (so the app knows a
        // launch is in flight to tear down) while quit latches.
        let mut ui = UiState::new();
        ui.begin(SessionKind::Singleplayer);
        ui.request_quit();
        assert!(ui.quit_requested());
        assert_eq!(ui.screen(), Screen::Connecting);
    }

    #[test]
    fn error_dismisses_back_to_menu_and_clears_state() {
        let mut ui = UiState::new();
        ui.begin(SessionKind::Multiplayer);
        ui.session_failed("boom");
        ui.dismiss_error();
        assert_eq!(ui.screen(), Screen::MainMenu);
        assert!(ui.error().is_none());
        assert!(ui.kind().is_none());
    }

    #[test]
    fn escape_is_context_sensitive() {
        // Playing <-> Paused
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.on_escape();
        assert!(ui.is_paused());
        ui.on_escape();
        assert!(ui.is_playing());

        // Error -> MainMenu
        let mut ui = UiState::new();
        ui.begin(SessionKind::Multiplayer);
        ui.session_failed("x");
        ui.on_escape();
        assert_eq!(ui.screen(), Screen::MainMenu);

        // MainMenu -> quit
        let mut ui = UiState::new();
        ui.on_escape();
        assert!(ui.quit_requested());

        // Connecting -> no-op (can't pause mid-connect)
        let mut ui = UiState::new();
        ui.begin(SessionKind::Singleplayer);
        ui.on_escape();
        assert_eq!(ui.screen(), Screen::Connecting);
        assert!(!ui.quit_requested());
    }

    #[test]
    fn stale_ready_signal_cannot_yank_a_menu_into_play() {
        // A session_ready arriving after we've bailed to the error screen must
        // not put us back in the world.
        let mut ui = UiState::new();
        ui.begin(SessionKind::Multiplayer);
        ui.session_failed("dropped");
        ui.session_ready();
        assert_eq!(
            ui.screen(),
            Screen::Error,
            "ready only fires from Connecting"
        );
    }

    #[test]
    fn chat_opens_and_closes_over_playing_without_grab_or_input() {
        let mut ui = UiState::new();
        ui.enter_dev_world();
        assert!(ui.is_playing());

        ui.open_chat();
        assert!(ui.is_chat_open());
        assert_eq!(ui.screen(), Screen::Chat);
        assert!(!ui.wants_cursor_grab(), "chat releases the pointer");
        assert!(!ui.accepts_gameplay_input(), "keys go to the chat line");

        ui.close_chat();
        assert!(ui.is_playing(), "close returns to the world");
        assert!(ui.wants_cursor_grab());
    }

    #[test]
    fn chat_only_opens_from_playing() {
        // Not from a menu, loading, error, or paused screen.
        let mut ui = UiState::new();
        ui.open_chat();
        assert_eq!(ui.screen(), Screen::MainMenu, "no chat outside the world");

        ui.enter_dev_world();
        ui.pause();
        ui.open_chat();
        assert_eq!(
            ui.screen(),
            Screen::Paused,
            "no chat from the pause overlay"
        );
    }

    #[test]
    fn escape_cancels_chat_back_to_playing() {
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.open_chat();
        ui.on_escape();
        assert!(
            ui.is_playing(),
            "escape cancels the line and returns to play"
        );
    }

    #[test]
    fn focus_loss_while_typing_pauses_rather_than_leaving_input_captured() {
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.open_chat();
        ui.pause();
        assert_eq!(ui.screen(), Screen::Paused);
        assert!(!ui.accepts_gameplay_input());
    }

    #[test]
    fn multiplayer_drills_down_to_the_list_and_the_edit_form() {
        let mut ui = UiState::new();
        ui.open_server_list();
        assert_eq!(ui.screen(), Screen::ServerList);
        assert!(ui.is_server_list() && ui.is_menu());

        ui.open_server_edit();
        assert_eq!(ui.screen(), Screen::ServerEdit);
        assert!(ui.is_server_edit() && ui.is_menu());

        ui.close_server_edit();
        assert_eq!(ui.screen(), Screen::ServerList);
        ui.close_server_list();
        assert_eq!(ui.screen(), Screen::MainMenu);
    }

    #[test]
    fn escape_unwinds_the_menu_one_level_at_a_time() {
        // The bug this guards: Escape from the edit form falling through to
        // MainMenu's "quit the game" arm and exiting mid-edit.
        let mut ui = UiState::new();
        ui.open_server_list();
        ui.open_server_edit();

        ui.on_escape();
        assert_eq!(ui.screen(), Screen::ServerList);
        assert!(!ui.quit_requested(), "must not quit from the edit form");

        ui.on_escape();
        assert_eq!(ui.screen(), Screen::MainMenu);
        assert!(!ui.quit_requested(), "must not quit from the list");

        ui.on_escape();
        assert!(ui.quit_requested(), "only the title screen quits");
    }

    #[test]
    fn menu_screens_never_grab_the_cursor_or_take_gameplay_input() {
        for screen in [
            Screen::MainMenu,
            Screen::ServerList,
            Screen::ServerEdit,
            Screen::WorldSelect,
            Screen::Settings,
            Screen::Accounts,
        ] {
            let mut ui = UiState::new();
            match screen {
                Screen::ServerList => ui.open_server_list(),
                Screen::ServerEdit => {
                    ui.open_server_list();
                    ui.open_server_edit();
                }
                Screen::WorldSelect => ui.open_world_select(),
                Screen::Settings => ui.open_settings(),
                Screen::Accounts => ui.open_accounts(),
                _ => {}
            }
            assert_eq!(ui.screen(), screen);
            assert!(!ui.wants_cursor_grab(), "{screen:?} must not grab");
            assert!(!ui.accepts_gameplay_input(), "{screen:?} must not take input");
            assert!(ui.is_menu(), "{screen:?} should count as a menu screen");
        }
        // And the world screens must not be mistaken for menus, or the menu
        // renderer would paint over live gameplay.
        let mut ui = UiState::new();
        ui.enter_dev_world();
        assert!(!ui.is_menu());
    }

    #[test]
    fn the_list_only_opens_from_the_title_screen() {
        // A stray call must never pull the player out of a world.
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.open_server_list();
        assert_eq!(ui.screen(), Screen::Playing);

        ui.pause();
        ui.open_server_list();
        assert_eq!(ui.screen(), Screen::Paused);

        // Nor may the edit form open from anywhere but the list.
        let mut ui = UiState::new();
        ui.open_server_edit();
        assert_eq!(ui.screen(), Screen::MainMenu);
    }

    /// [`Screen::ALL`]'s length is checked by the compiler; what it cannot check
    /// is a copy-pasted line that keeps the length right and lists one variant
    /// twice, dropping another screen from every walk that iterates it.
    #[test]
    fn screen_all_lists_each_variant_once() {
        for (i, a) in Screen::ALL.iter().enumerate() {
            for b in &Screen::ALL[i + 1..] {
                assert_ne!(a, b, "Screen::ALL lists {a:?} twice");
            }
        }
        // And the screens this file's own `is_menu` set names must all be in it,
        // which is the cheapest available check that the list is not merely
        // internally consistent.
        for screen in [
            Screen::MainMenu,
            Screen::ServerList,
            Screen::ServerEdit,
            Screen::WorldSelect,
            Screen::Settings,
            Screen::Accounts,
        ] {
            assert!(
                Screen::ALL.contains(&screen),
                "{screen:?} is a menu screen missing from Screen::ALL"
            );
        }
    }

    /// Issue #397. The world list opens only from the title and unwinds back to
    /// it — one level, like every other menu screen.
    #[test]
    fn the_world_list_opens_from_the_title_and_unwinds_to_it() {
        let mut ui = UiState::new();
        ui.open_world_select();
        assert_eq!(ui.screen(), Screen::WorldSelect);
        assert!(ui.is_world_select());
        assert!(ui.is_menu(), "it is a pre-session menu screen");
        assert!(!ui.wants_cursor_grab());

        ui.on_escape();
        assert_eq!(ui.screen(), Screen::MainMenu, "one level, not a quit");
        assert!(!ui.quit_requested());

        // And a stray call must never pull the player out of a world — the same
        // guard `the_list_only_opens_from_the_title_screen` makes for the server
        // list.
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.open_world_select();
        assert_eq!(ui.screen(), Screen::Playing);
        ui.pause();
        ui.open_world_select();
        assert_eq!(ui.screen(), Screen::Paused);
        // Nor may `close` resurrect the title from a live session.
        ui.close_world_select();
        assert_eq!(ui.screen(), Screen::Paused);
    }

    #[test]
    fn singleplayer_from_the_menu_enters_the_offline_world() {
        // The dev-world entry point, which is **not** a menu button: the menu's
        // Singleplayer button opens the world list, and Play Selected World
        // there starts a real integrated server (#287). This asserts only that
        // `enter_dev_world` still lands in a session-less Playing state.
        let mut ui = UiState::new();
        ui.enter_dev_world();
        assert_eq!(ui.screen(), Screen::Playing);
        assert!(ui.wants_cursor_grab());
    }

    #[test]
    fn cursor_and_input_are_live_in_exactly_one_screen() {
        let mut ui = UiState::new();
        // Walk every screen and assert grab/input == is_playing throughout.
        let check = |u: &UiState| {
            assert_eq!(u.wants_cursor_grab(), u.is_playing());
            assert_eq!(u.accepts_gameplay_input(), u.is_playing());
        };
        check(&ui); // MainMenu
        ui.begin(SessionKind::Multiplayer);
        check(&ui); // Connecting
        ui.session_ready();
        check(&ui); // Playing
        ui.open_chat();
        check(&ui); // Chat
        ui.close_chat();
        ui.pause();
        check(&ui); // Paused
        ui.session_failed("end");
        check(&ui); // Error
    }

    #[test]
    fn settings_opens_from_the_title_and_escape_returns_to_it() {
        let mut ui = UiState::new();
        ui.open_settings();
        assert_eq!(ui.screen(), Screen::Settings);
        assert!(ui.is_settings() && ui.is_menu());
        assert!(!ui.wants_cursor_grab());
        assert!(!ui.accepts_gameplay_input());

        ui.on_escape();
        assert_eq!(ui.screen(), Screen::MainMenu, "escape unwinds to the title");
    }

    #[test]
    fn settings_only_opens_from_the_title_screen() {
        // A stray call must never pull the player out of a world, matching the
        // server list's own guard.
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.open_settings();
        assert_eq!(ui.screen(), Screen::Playing);

        ui.pause();
        ui.open_settings();
        assert_eq!(ui.screen(), Screen::Paused);
    }

    #[test]
    fn close_settings_is_a_no_op_off_screen() {
        let mut ui = UiState::new();
        ui.close_settings();
        assert_eq!(ui.screen(), Screen::MainMenu, "nothing to close");
    }

    #[test]
    fn accounts_opens_from_the_title_and_escape_returns_to_it() {
        let mut ui = UiState::new();
        ui.open_accounts();
        assert_eq!(ui.screen(), Screen::Accounts);
        assert!(ui.is_accounts() && ui.is_menu());
        assert!(!ui.wants_cursor_grab());
        assert!(!ui.accepts_gameplay_input());

        ui.on_escape();
        assert_eq!(ui.screen(), Screen::MainMenu, "escape unwinds to the title");
    }

    #[test]
    fn accounts_only_opens_from_the_title_screen() {
        // A stray call must never pull the player out of a world, matching
        // the server list's and settings screen's own guards.
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.open_accounts();
        assert_eq!(ui.screen(), Screen::Playing);

        ui.pause();
        ui.open_accounts();
        assert_eq!(ui.screen(), Screen::Paused);
    }

    #[test]
    fn close_accounts_is_a_no_op_off_screen() {
        let mut ui = UiState::new();
        ui.close_accounts();
        assert_eq!(ui.screen(), Screen::MainMenu, "nothing to close");
    }

    #[test]
    fn options_opened_from_pause_returns_to_pause_not_the_title() {
        // The bug this guards: Escape from Options always falling through to
        // MainMenu, which would drop a live session out from under the player
        // just for opening the pause menu's Options button.
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.on_escape();
        assert!(ui.is_paused());

        ui.open_settings_from_pause();
        assert_eq!(ui.screen(), Screen::Settings);
        assert!(!ui.wants_cursor_grab());
        assert!(!ui.accepts_gameplay_input());

        ui.on_escape();
        assert!(
            ui.is_paused(),
            "settings opened from pause must unwind back to pause, not the title"
        );

        // And the ordinary title-screen path is unaffected: still MainMenu.
        let mut ui = UiState::new();
        ui.open_settings();
        ui.on_escape();
        assert_eq!(ui.screen(), Screen::MainMenu);
    }

    #[test]
    fn open_settings_from_pause_only_opens_from_the_pause_screen() {
        // Mirrors `settings_only_opens_from_the_title_screen`'s guard, for the
        // new entry point: a stray call must never pull the player out of
        // wherever they actually are.
        let mut ui = UiState::new();
        ui.open_settings_from_pause();
        assert_eq!(ui.screen(), Screen::MainMenu, "not paused, so no-op");

        ui.enter_dev_world();
        ui.open_settings_from_pause();
        assert_eq!(
            ui.screen(),
            Screen::Playing,
            "playing, not paused, so no-op"
        );
    }

    #[test]
    fn escape_still_quits_from_the_title_after_a_pause_settings_round_trip() {
        // The `settings_return` bookkeeping must not leak into the ordinary
        // title-screen Escape path once the player is back there for real.
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.on_escape(); // -> Paused
        ui.open_settings_from_pause();
        ui.on_escape(); // -> Paused
        ui.on_escape(); // -> Playing
        assert!(ui.is_playing());
    }

    #[test]
    fn quit_to_title_only_leaves_from_pause_and_clears_session_state() {
        let mut ui = UiState::new();
        ui.begin(SessionKind::Multiplayer);
        ui.session_ready();
        ui.pause();
        assert!(ui.is_paused());

        ui.quit_to_title();
        assert_eq!(ui.screen(), Screen::MainMenu);
        assert!(
            ui.kind().is_none(),
            "leaving must not remember the old session"
        );
        assert!(ui.error().is_none(), "this is not a failure, so no reason");
        assert!(!ui.wants_cursor_grab());

        // A stray call from anywhere else must be a no-op — same guard as
        // every other "leave a screen" method in this file.
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.quit_to_title();
        assert_eq!(
            ui.screen(),
            Screen::Playing,
            "playing, not paused, so no-op"
        );
    }

    #[test]
    fn a_disconnect_while_tweaking_pause_settings_reaches_the_error_screen() {
        // Settings opened from the title must NOT be reachable by a stray
        // disconnect (there is no session to have disconnected from), but
        // settings opened from the pause menu is genuinely mid-session and
        // must report a real one.
        let mut ui = UiState::new();
        ui.begin(SessionKind::Multiplayer);
        ui.session_ready();
        ui.pause();
        ui.open_settings_from_pause();
        assert_eq!(ui.screen(), Screen::Settings);

        ui.session_failed("disconnected: Server closed");
        assert_eq!(
            ui.screen(),
            Screen::Error,
            "a real disconnect must reach the player even while they're in \
             the pause menu's options"
        );
        assert_eq!(ui.error(), Some("disconnected: Server closed"));
    }

    #[test]
    fn a_stray_failure_does_not_reach_settings_opened_from_the_title() {
        let mut ui = UiState::new();
        ui.open_settings();
        ui.session_failed("connection refused (os error 61)");
        assert_eq!(
            ui.screen(),
            Screen::Settings,
            "no session was ever started from the title's Options button"
        );
        assert!(ui.error().is_none());
    }

    // -- the death screen (issue #103) -------------------------------------

    #[test]
    fn die_reaches_the_death_screen_from_every_live_gameplay_screen_and_carries_the_message() {
        for setup in [
            (|ui: &mut UiState| ui.enter_dev_world()) as fn(&mut UiState),
            |ui| {
                ui.enter_dev_world();
                ui.open_chat();
            },
            |ui| {
                ui.enter_dev_world();
                ui.open_container();
            },
            |ui| {
                ui.enter_dev_world();
                ui.pause();
            },
        ] {
            let mut ui = UiState::new();
            setup(&mut ui);
            ui.die(Some("hit the ground too hard".to_string()));
            assert_eq!(ui.screen(), Screen::Death);
            assert_eq!(ui.death_message(), Some("hit the ground too hard"));
        }

        // A stray call from anywhere it cannot happen from (no live session)
        // must be a no-op — same guard as every other "leave a screen" method.
        let mut ui = UiState::new();
        ui.die(Some("should not apply".to_string()));
        assert_eq!(ui.screen(), Screen::MainMenu, "no world, so no-op");
        assert!(ui.death_message().is_none());
    }

    #[test]
    fn respawn_confirmed_only_leaves_from_death_and_clears_the_message() {
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.die(Some("burned to death".to_string()));

        ui.respawn_confirmed();
        assert_eq!(ui.screen(), Screen::Playing);
        assert!(
            ui.death_message().is_none(),
            "the old death's message must not survive into the new session"
        );

        // A stray call from anywhere else is a no-op.
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.respawn_confirmed();
        assert_eq!(ui.screen(), Screen::Playing, "not dead, so no-op");
    }

    #[test]
    fn escape_does_not_leave_the_death_screen() {
        // Vanilla's `DeathScreen.shouldCloseOnEsc()` is `false` — this is the
        // one screen in this file where `on_escape` must be a pure no-op.
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.die(None);
        ui.on_escape();
        assert_eq!(ui.screen(), Screen::Death);
        assert!(!ui.quit_requested());
    }

    #[test]
    fn quit_to_title_from_the_death_screen_leaves_for_the_main_menu() {
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.die(Some("slain by Zombie".to_string()));

        ui.quit_to_title();
        assert_eq!(ui.screen(), Screen::MainMenu);
        assert!(ui.death_message().is_none());
        assert!(ui.error().is_none(), "this is not a failure, so no reason");
    }

    /// Issue #192: `show_credits` reaches `Screen::Credits` from every live
    /// gameplay screen `die` also reaches from, and a stray call from
    /// anywhere else (the menu, an error screen) is a no-op — same shape as
    /// every other `open_*`/`show_*` guard in this file.
    #[test]
    fn show_credits_only_leaves_from_live_gameplay_screens() {
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.show_credits();
        assert_eq!(ui.screen(), Screen::Credits);

        let mut ui = UiState::new();
        ui.show_credits();
        assert_eq!(
            ui.screen(),
            Screen::MainMenu,
            "a stray call off the title screen must not do anything"
        );
    }

    /// `quit_to_title` from the credits screen is the same teardown as from
    /// pause/death — reused rather than a fourth copy of "clear session state
    /// and go to the title", per issue #192's own scope (the trigger is out
    /// of this crate's ownership; the exit is not, and it should not need a
    /// new mechanism).
    #[test]
    fn quit_to_title_from_the_credits_screen_leaves_for_the_main_menu() {
        let mut ui = UiState::new();
        ui.begin(SessionKind::Multiplayer);
        ui.session_ready();
        ui.show_credits();
        assert_eq!(ui.screen(), Screen::Credits);

        ui.quit_to_title();
        assert_eq!(ui.screen(), Screen::MainMenu);
        assert!(ui.kind().is_none(), "leaving must not remember the old session");
    }

    /// `on_escape` from the credits screen behaves like every screen whose
    /// key handler intercepts Escape before `UiState` ever sees it
    /// (`Screen::Accounts`, `Screen::WorldSelect`) — this only exercises the
    /// fallback a caller reaching here some other way would hit, and it must
    /// still leave for the title rather than doing nothing.
    #[test]
    fn on_escape_from_credits_leaves_for_the_title() {
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.show_credits();
        ui.on_escape();
        assert_eq!(ui.screen(), Screen::MainMenu);
    }

    #[test]
    fn a_disconnect_while_dead_reaches_the_error_screen() {
        // The hazard `CLAUDE.md` names: a live server holds a dead player with
        // no chunk stream until it respawns. If a genuine disconnect while the
        // death screen is up were swallowed here, the player would be stuck
        // looking at a Respawn button that can never get an answer — the same
        // *symptom* as the dead-player chunk-blackout bug, from a different
        // cause. `session_failed` must reach through to `Screen::Error`.
        let mut ui = UiState::new();
        ui.begin(SessionKind::Multiplayer);
        ui.session_ready();
        ui.die(Some("fell out of the world".to_string()));
        assert_eq!(ui.screen(), Screen::Death);

        ui.session_failed("disconnected: Server closed");
        assert_eq!(ui.screen(), Screen::Error);
        assert_eq!(ui.error(), Some("disconnected: Server closed"));
        assert!(
            ui.death_message().is_none(),
            "the stale death message must not leak onto the error screen"
        );
    }
}
