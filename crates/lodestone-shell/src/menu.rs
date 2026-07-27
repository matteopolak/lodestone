//! The shell's screen / session **state machine** — structure only.
//!
//! There is deliberately no text or widget rendering here: `impl-assets` owns
//! fonts and the GUI has no glyph source yet. What *is* independent of drawing
//! text — and is the actual hard part — is the lifecycle: choosing a session,
//! establishing it, playing, pausing, and every way it can fail. A menu that can
//! only succeed is the same class of defect as a test that can only pass, so the
//! failure edges (connection refused, server-start failure, disconnect mid-game,
//! quit while still loading) are modelled and tested first-class here.
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
//! [`SessionKind::Multiplayer`] connects to a remote address. The integrated
//! server (`impl-worldgen`'s `lodestone-server`) is not yet wired, so the app
//! does not build a second launch path for it: selecting it fails loudly into
//! [`Screen::Error`] rather than silently doing nothing (see
//! [`crate::app`]'s staged launcher).

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
    /// *local* UI state, not a world stop.
    Paused,
    /// A session failed to establish or ended unexpectedly. `error()` carries the
    /// human-readable reason; the only ways forward are back to the menu or quit.
    Error,
}

/// The shell's top-level UI state. One instance lives in the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiState {
    screen: Screen,
    kind: Option<SessionKind>,
    error: Option<String>,
    quit_requested: bool,
}

impl Default for UiState {
    fn default() -> Self {
        Self {
            screen: Screen::MainMenu,
            kind: None,
            error: None,
            quit_requested: false,
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

    /// Enter the local dev world directly (the shell's `worldgen` stand-in used
    /// before the integrated server exists). Distinct from
    /// [`SessionKind::Singleplayer`], which will start a real server; this is a
    /// no-session Playing state so we don't fake a launch path that isn't built.
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
    pub fn session_failed(&mut self, reason: impl Into<String>) {
        // Ignore failures once we've already left for the menu, so a trailing
        // error from a shutting-down session doesn't resurrect the error screen.
        if matches!(
            self.screen,
            Screen::Connecting
                | Screen::Playing
                | Screen::Chat
                | Screen::Container
                | Screen::Paused
                | Screen::Error
        ) {
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
    /// - MainMenu → request a clean quit (Escape on the title exits)
    /// - Connecting → no-op (can't pause mid-connect; the app offers quit-to-cancel)
    pub fn on_escape(&mut self) {
        match self.screen {
            Screen::Playing => self.screen = Screen::Paused,
            Screen::Paused => self.screen = Screen::Playing,
            Screen::Chat | Screen::Container => self.screen = Screen::Playing,
            Screen::Error => self.dismiss_error(),
            Screen::MainMenu => self.request_quit(),
            Screen::Connecting => {}
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
}
