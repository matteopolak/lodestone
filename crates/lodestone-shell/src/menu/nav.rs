//! The menu's *brain*: selection, the add/edit form, and what a keypress means
//! on each screen.
//!
//! ## What it is
//!
//! [`super::UiState`] models which screen is showing; this models everything
//! else the menu needs to be usable — which row is highlighted, what is typed
//! into the edit form, and which of those keys means "connect to this server".
//! It returns a [`MenuAction`] describing the one thing the app must then do
//! (start a session, quit, re-ping a row), so `app.rs` contains no menu logic
//! beyond translating winit keys and acting on the returned verb.
//!
//! ## Why it does not touch winit
//!
//! Input arrives as [`MenuKey`], a tiny abstract key set. That is what makes the
//! whole menu — every navigation edge, every text-entry rule, add/edit/delete
//! and persistence — unit-testable with no window, no GPU and no server. The
//! winit mapping is four lines in `app.rs` and is the only untested part.
//!
//! ## How to change it
//!
//! Adding a screen means a variant in [`super::Screen`], an arm in
//! [`MenuNav::key`], and rows in [`super::render`]. Adding an *action* means a
//! [`MenuAction`] variant — deliberately an enum rather than a callback so the
//! exhaustive `match` in `app.rs` fails to compile when a new one is added,
//! rather than silently doing nothing (this repo's dominant defect).
//!
//! Persistence is written **eagerly**, on every mutation, rather than on exit:
//! the shell has no guaranteed clean-shutdown hook (a GPU crash or a `SIGKILL`
//! skips `Drop`), and a server list that survives only a graceful quit is one
//! that silently loses the entry the player just added.

use super::servers::{MAX_NAME_CHARS, ServerEntry, ServerList, servers_path};
use super::{Screen, SessionKind, UiState};
use crate::config::{MAX_MANUAL_GUI_SCALE, Options};

/// Longest accepted address string in the edit form, in characters. A hostname
/// is capped at 253 by DNS; the extra room is for `:port`.
pub const MAX_ADDRESS_CHARS: usize = 260;

/// The abstract keys the menu understands. `app.rs` maps winit's `KeyCode` and
/// text onto these; nothing here knows what a scancode is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKey {
    /// Move the highlight up one row (wraps).
    Up,
    /// Move the highlight down one row (wraps).
    Down,
    /// Activate the highlighted row / save the form.
    Enter,
    /// Back out one level. Handled by [`UiState::on_escape`].
    Escape,
    /// Move between fields in the edit form.
    Tab,
    /// Delete the character before the cursor (edit form only).
    Backspace,
    /// Delete the highlighted server (list only).
    Delete,
    /// A printable character: a command on the list, text in the form.
    Char(char),
}

/// The one thing the app must do as a result of a keypress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    /// Nothing to do; the menu handled it internally.
    None,
    /// Enter the singleplayer world.
    Singleplayer,
    /// Connect to this server (the app opens the session and shows Connecting).
    Connect(ServerEntry),
    /// Shut the game down cleanly.
    Quit,
    /// The list changed or a re-ping was asked for: the app should refresh
    /// statuses. Carries the entry to (re-)probe, or `None` for "all of them".
    Reprobe(Option<ServerEntry>),
    /// A row was removed; drop its cached status. Carried separately from
    /// [`MenuAction::Reprobe`] so the app does not start a probe for an address
    /// that is no longer in the list.
    Forget(ServerEntry),
    /// The pause menu's "Quit to Title" was activated: [`UiState`] has already
    /// moved to [`Screen::MainMenu`] (see [`UiState::quit_to_title`]); the app
    /// must now tear down whatever live session (net connection and/or
    /// integrated server) is still attached to `Sim`, exactly as it would for
    /// an ordinary disconnect — nothing here does that on its own, since
    /// `MenuNav` holds no session state to tear down.
    QuitToTitle,
}

/// Which field of the add/edit form has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    /// The display label.
    Name,
    /// `host` or `host:port`.
    Address,
}

/// The add/edit form's contents.
///
/// The address is held as the **single string the user typed** and split into
/// host/port only on save. Splitting per keystroke would make `mc.example.com:2`
/// unrepresentable halfway through typing `:25565`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EditForm {
    /// Display label being typed.
    pub name: String,
    /// Address being typed, `host` or `host:port`.
    pub address: String,
    /// Which field has focus.
    pub field: FormField,
    /// Index being edited, or `None` when adding a new entry.
    pub editing: Option<usize>,
}

impl Default for FormField {
    fn default() -> Self {
        Self::Name
    }
}

impl EditForm {
    /// A blank form for a new entry.
    #[must_use]
    pub fn adding() -> Self {
        Self::default()
    }

    /// A form pre-filled from `entry`, editing the row at `index`.
    #[must_use]
    pub fn editing(index: usize, entry: &ServerEntry) -> Self {
        Self {
            name: entry.name.clone(),
            address: entry.address_label(),
            field: FormField::Name,
            editing: Some(index),
        }
    }

    /// Whether the form can be saved. The label may be blank (it falls back to
    /// the host); the address may not.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.address.trim().is_empty()
    }

    /// The entry this form would save.
    #[must_use]
    pub fn to_entry(&self) -> ServerEntry {
        let (host, port) = ServerEntry::split_host_port(&self.address);
        let name = if self.name.trim().is_empty() {
            host.clone()
        } else {
            self.name.clone()
        };
        ServerEntry::new(name, host, port)
    }

    fn active_mut(&mut self) -> (&mut String, usize) {
        match self.field {
            FormField::Name => (&mut self.name, MAX_NAME_CHARS),
            FormField::Address => (&mut self.address, MAX_ADDRESS_CHARS),
        }
    }

    /// Appends a printable character to the focused field, respecting its cap.
    /// Control characters and `§` are refused — `§` because it is the legacy
    /// formatting-code introducer and has no business in a hostname or a label.
    pub fn push(&mut self, ch: char) {
        if ch.is_control() || ch == '\u{a7}' {
            return;
        }
        let (buf, cap) = self.active_mut();
        if buf.chars().count() < cap {
            buf.push(ch);
        }
    }

    /// Removes the last character of the focused field.
    pub fn backspace(&mut self) {
        let (buf, _) = self.active_mut();
        buf.pop();
    }

    /// Moves focus to the other field.
    pub fn next_field(&mut self) {
        self.field = match self.field {
            FormField::Name => FormField::Address,
            FormField::Address => FormField::Name,
        };
    }
}

/// The title screen's widgets, in vanilla's own display order.
///
/// This is vanilla `TitleScreen.init`'s widget list
/// (`.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/TitleScreen.java:105-168`),
/// reproduced whole rather than trimmed to what this client implements.
/// [`MainButton::enabled`] is what marks the rest **present but greyed out**,
/// which is the faithful thing: a button missing from its vanilla position is a
/// layout that reads wrong, while a disabled one in the right position reads
/// exactly like vanilla with the feature unavailable (which is a state vanilla
/// itself ships — `Multiplayer` and `Minecraft Realms` are disabled for a
/// banned account, `TitleScreen.java:189-203`).
///
/// The three 20×20 icon buttons come from `CommonButtons`
/// (`TitleScreen.java:123-140`); vanilla positions them with
/// `getHorizontalPosition(i, 3, 20)` (`TitleScreen.java:170-173`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainButton {
    /// Enter the local world.
    Singleplayer,
    /// Open the server list.
    Multiplayer,
    /// Vanilla's `menu.online` row. Present and disabled: Realms is a paid
    /// Mojang-hosted service with its own authenticated HTTP API, none of which
    /// exists here and none of which is on the roadmap.
    Realms,
    /// Vanilla's friends icon button (`CommonButtons.friends`). Present and
    /// disabled: it needs a Microsoft-account social graph.
    Friends,
    /// Vanilla's language icon button. Present and disabled: the shell loads
    /// exactly one language table (`en_us.json`, see `resources.rs`) and has no
    /// language-selection screen.
    Language,
    /// Vanilla's accessibility icon button. Present and disabled: there is no
    /// accessibility options screen.
    Accessibility,
    /// Open the settings screen.
    Options,
    /// Quit the game.
    Quit,
}

/// Every title-screen widget, in vanilla's display order. Indices are the one
/// index space shared by keyboard selection, mouse hover, hit-testing and the
/// renderer — see [`super::render::title_slot`].
pub const MAIN_BUTTONS: [MainButton; 8] = [
    MainButton::Singleplayer,
    MainButton::Multiplayer,
    MainButton::Realms,
    MainButton::Friends,
    MainButton::Language,
    MainButton::Accessibility,
    MainButton::Options,
    MainButton::Quit,
];

impl MainButton {
    /// The label drawn on the button, or narrated for an icon-only one.
    ///
    /// Vanilla's own `en_us.json` strings, verbatim: `menu.singleplayer`,
    /// `menu.multiplayer`, `menu.online`, `options.language`,
    /// `options.accessibility`, `menu.options`, `menu.quit`. Mixed case now,
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
        }
    }

    /// Whether the button can be activated. A `false` here is what draws
    /// vanilla's `widget/button_disabled` sprite with a dimmed label and makes
    /// keyboard navigation step over the row — see [`MainButton`]'s docs for why
    /// each disabled one is still present.
    #[must_use]
    pub fn enabled(self) -> bool {
        matches!(
            self,
            MainButton::Singleplayer
                | MainButton::Multiplayer
                | MainButton::Options
                | MainButton::Quit
        )
    }

    /// The GUI sprite drawn centred in the button instead of a label —
    /// vanilla's `SpriteIconButton.CenteredIcon`, 15×15 inside a 20×20 button
    /// (`CommonButtons.java:10,21`, `FriendsButton.java:34`).
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
/// This is vanilla `PauseScreen.createPauseMenu`'s grid
/// (`.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/PauseScreen.java:91-183`)
/// reproduced whole, and it is **not** the three-button stack people remember:
/// a full-width Back to Game, then a two-column Advancements / Statistics row,
/// then a centred row of four 20×20 icon buttons, then Options, then
/// Disconnect. The exact rects are in [`super::render::pause_slot`].
///
/// An earlier version of this file *omitted* Advancements and Statistics on the
/// grounds that neither has a client-side subsystem to open onto, so either
/// button would reach zero pixels. That reasoning still holds for the
/// *action* — which is why they are [`PauseButton::enabled`]-`false` — but it
/// does not hold for the *position*: a greyed-out button where vanilla puts one
/// is faithful UI, and vanilla itself greys these out (`playerReportingButton`
/// with no players to report, `PauseScreen.java:148-151`).
///
/// Which Options layout is reproduced is a real fork in vanilla:
/// `minecraft.hasSingleplayerServer()` splits the row into Options + Open to LAN
/// (`PauseScreen.java:157-159`), and only the `else` branch gives Options the
/// full 204 px width (`PauseScreen.java:161-163`). This client has no integrated
/// server at all (see the module docs), so `hasSingleplayerServer()` is
/// unconditionally false for it and the full-width branch is the correct one.
///
/// Vanilla's last button is labelled by
/// `CommonComponents.disconnectButtonLabel(isLocalServer)` — "Save and Quit to
/// Title" locally, "Disconnect" remotely (`CommonComponents.java:53-55`). This
/// client uses "Disconnect" for both, because [`SessionKind::Singleplayer`] is
/// currently the local dev world with no persistence: "Save and Quit" would
/// promise a save that does not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseButton {
    /// Resume play. Equivalent to Escape. Vanilla's `menu.returnToGame`.
    BackToGame,
    /// Vanilla's `gui.advancements`. Present and disabled: nothing in this
    /// workspace decodes the `update_advancements` packet, so there is no
    /// advancement tree to open.
    Advancements,
    /// Vanilla's `gui.stats`. Present and disabled: nothing decodes the
    /// `award_stats` packet, so there are no statistics to show.
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
    /// Vanilla's `menu.playerReporting` icon button. Present and disabled: it
    /// needs the chat-signature reporting context.
    PlayerReporting,
    /// Open the settings screen (reuses [`super::Screen::Settings`] — see
    /// [`super::UiState::open_settings_from_pause`]).
    Options,
    /// Leave the session for the title screen.
    QuitToTitle,
}

/// Every pause-screen widget, in vanilla's display order. As with
/// [`MAIN_BUTTONS`], these indices are the one index space keyboard selection,
/// mouse hover, hit-testing and the renderer all share.
pub const PAUSE_BUTTONS: [PauseButton; 9] = [
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
    /// Vanilla's `en_us.json` strings verbatim.
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
            PauseButton::Options => "Options...",
            PauseButton::QuitToTitle => "Disconnect",
        }
    }

    /// Whether the button can be activated — see [`MainButton::enabled`].
    #[must_use]
    pub fn enabled(self) -> bool {
        matches!(
            self,
            PauseButton::BackToGame | PauseButton::Options | PauseButton::QuitToTitle
        )
    }

    /// The GUI sprite drawn centred in the button instead of a label, 15×15
    /// inside a 20×20 button (`PauseScreen.java:104,115,134`).
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

/// Selection state and the saved server list.
#[derive(Debug, Clone)]
pub struct MenuNav {
    main: usize,
    server: usize,
    /// Highlighted row on the pause menu ([`PAUSE_BUTTONS`]).
    paused: usize,
    form: EditForm,
    list: ServerList,
    /// Where the list is persisted. Held rather than recomputed so a test can
    /// point one at a temporary file.
    path: std::path::PathBuf,
    /// The last save error, surfaced on the list screen. A silent write failure
    /// is how a player loses an entry and never learns why.
    save_error: Option<String>,
    /// The persisted user options (currently just GUI scale).
    options: Options,
    /// Where `options` is persisted. Held separately from `path` so tests can
    /// point each file at its own temporary location.
    options_path: std::path::PathBuf,
    /// The last options-save error, surfaced on the settings screen.
    options_save_error: Option<String>,
}

impl Default for MenuNav {
    fn default() -> Self {
        Self::new()
    }
}

impl MenuNav {
    /// Loads the saved server list and options from their real locations.
    #[must_use]
    pub fn new() -> Self {
        Self::with_paths(servers_path(), crate::config::options_path())
    }

    /// Loads the server list from `path`. Missing or corrupt is an empty list.
    /// The options file is derived from the same directory (`options.json`
    /// beside it) so existing callers of this constructor keep working
    /// unchanged — see [`MenuNav::with_paths`] to point both explicitly.
    #[must_use]
    pub fn with_path(path: std::path::PathBuf) -> Self {
        let options_path = path
            .parent()
            .map(|d| d.join("options.json"))
            .unwrap_or_else(|| std::path::PathBuf::from("options.json"));
        Self::with_paths(path, options_path)
    }

    /// Loads the server list from `path` and the options from `options_path`.
    /// Missing or corrupt is an empty list / the default options respectively,
    /// never an error — a corrupt file must not stop the game from launching.
    #[must_use]
    pub fn with_paths(path: std::path::PathBuf, options_path: std::path::PathBuf) -> Self {
        Self {
            main: 0,
            server: 0,
            paused: 0,
            form: EditForm::adding(),
            list: ServerList::load_from(&path),
            path,
            save_error: None,
            options: Options::load_from(&options_path),
            options_path,
            options_save_error: None,
        }
    }

    /// The saved servers.
    #[must_use]
    pub fn list(&self) -> &ServerList {
        &self.list
    }

    /// The persisted `gui_scale` option ([`crate::config::AUTO_GUI_SCALE`] or
    /// an explicit ceiling) — never a pixel count, see
    /// [`crate::config::calculate_gui_scale`].
    #[must_use]
    pub fn gui_scale(&self) -> u32 {
        self.options.gui_scale
    }

    /// The last options-save failure, if any.
    #[must_use]
    pub fn options_save_error(&self) -> Option<&str> {
        self.options_save_error.as_deref()
    }

    /// The highlighted main-menu button.
    #[must_use]
    pub fn main_button(&self) -> MainButton {
        MAIN_BUTTONS[self.main.min(MAIN_BUTTONS.len() - 1)]
    }

    /// Index of the highlighted main-menu button.
    #[must_use]
    pub fn main_index(&self) -> usize {
        self.main
    }

    /// Index of the highlighted server row.
    #[must_use]
    pub fn server_index(&self) -> usize {
        self.server
    }

    /// The highlighted pause-menu button.
    #[must_use]
    pub fn pause_button(&self) -> PauseButton {
        PAUSE_BUTTONS[self.paused.min(PAUSE_BUTTONS.len() - 1)]
    }

    /// Index of the highlighted pause-menu button.
    #[must_use]
    pub fn pause_index(&self) -> usize {
        self.paused
    }

    /// The add/edit form.
    #[must_use]
    pub fn form(&self) -> &EditForm {
        &self.form
    }

    /// The last persistence failure, if any.
    #[must_use]
    pub fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// Moves the highlight to row `row` of the current screen, as a mouse hover
    /// would. Out-of-range rows are ignored rather than clamped: the caller
    /// hit-tests against the rendered rects, so "no row here" must not silently
    /// move the selection to a different one.
    ///
    /// A **disabled** row is still hovered, matching vanilla exactly:
    /// `AbstractWidget::extractRenderState` sets `isHovered` from geometry alone
    /// and never consults `active` (`AbstractWidget.java:56-62`), while
    /// `WidgetSprites::get(active, focused)` returns `button_disabled` whichever
    /// way `focused` went (`WidgetSprites.java:19-25`) — so a greyed-out button
    /// under the cursor looks greyed-out, not highlighted. The half that matters
    /// is the *click*: `key_main`/`key_paused` refuse Enter on a disabled button,
    /// which is why moving the highlight onto one here is safe.
    pub fn hover(&mut self, ui: &UiState, row: usize) {
        match ui.screen() {
            Screen::MainMenu if row < MAIN_BUTTONS.len() => self.main = row,
            Screen::ServerList if row < self.list.len() => self.server = row,
            Screen::Paused if row < PAUSE_BUTTONS.len() => self.paused = row,
            Screen::ServerEdit => {
                self.form.field = match row {
                    0 => FormField::Name,
                    1 => FormField::Address,
                    _ => self.form.field,
                };
            }
            _ => {}
        }
    }

    /// Handles one key for the current screen, mutating `ui` for navigation and
    /// returning the action the app must perform.
    pub fn key(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match ui.screen() {
            Screen::MainMenu => self.key_main(ui, key),
            Screen::ServerList => self.key_list(ui, key),
            Screen::ServerEdit => self.key_edit(ui, key),
            Screen::Settings => self.key_settings(ui, key),
            // Unlike the other arms above, the pause menu is not an
            // `owns_frame` screen — see `render::pause_frame`'s docs — but it
            // still owns its own row navigation exactly like they do.
            Screen::Paused => self.key_paused(ui, key),
            // The error screen has exactly one affordance — go back — reachable
            // with Escape or by activating its single row.
            Screen::Error if matches!(key, MenuKey::Escape | MenuKey::Enter) => {
                ui.dismiss_error();
                MenuAction::None
            }
            // Escape is the only menu key that means anything on the world and
            // loading screens, and `UiState` already owns it.
            _ => {
                if key == MenuKey::Escape {
                    ui.on_escape();
                    if ui.quit_requested() {
                        return MenuAction::Quit;
                    }
                }
                MenuAction::None
            }
        }
    }

    fn key_main(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match key {
            MenuKey::Up => {
                self.main = step_enabled(self.main, MAIN_BUTTONS.len(), false, &|i| {
                    MAIN_BUTTONS[i].enabled()
                });
                MenuAction::None
            }
            MenuKey::Down => {
                self.main = step_enabled(self.main, MAIN_BUTTONS.len(), true, &|i| {
                    MAIN_BUTTONS[i].enabled()
                });
                MenuAction::None
            }
            MenuKey::Enter => {
                let button = self.main_button();
                if !button.enabled() {
                    // A click landed on a present-but-disabled vanilla button
                    // (the mouse *can* highlight one — see `hover` — even though
                    // the keyboard steps over it). Vanilla's
                    // `AbstractWidget.mouseClicked` returns false for an inactive
                    // widget, so nothing happens. Returning here rather than
                    // leaving the highlight where it was is what stops a click on
                    // Advancements activating whatever *used* to be selected.
                    return MenuAction::None;
                }
                match button {
                    MainButton::Singleplayer => MenuAction::Singleplayer,
                    MainButton::Multiplayer => {
                        ui.open_server_list();
                        self.clamp_server();
                        MenuAction::Reprobe(None)
                    }
                    MainButton::Options => {
                        ui.open_settings();
                        MenuAction::None
                    }
                    MainButton::Quit => {
                        ui.request_quit();
                        MenuAction::Quit
                    }
                    // Unreachable — every variant below is disabled above.
                    // Spelled out instead of `_` so making one of them *enabled*
                    // without giving it an action is a compile-visible mistake
                    // rather than a silently dead button.
                    MainButton::Realms
                    | MainButton::Friends
                    | MainButton::Language
                    | MainButton::Accessibility => MenuAction::None,
                }
            }
            MenuKey::Escape => {
                ui.on_escape();
                MenuAction::Quit
            }
            _ => MenuAction::None,
        }
    }

    fn key_list(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match key {
            MenuKey::Up => {
                self.server = wrap_prev(self.server, self.list.len());
                MenuAction::None
            }
            MenuKey::Down => {
                self.server = wrap_next(self.server, self.list.len());
                MenuAction::None
            }
            MenuKey::Enter => match self.list.get(self.server) {
                Some(entry) => {
                    let entry = entry.clone();
                    ui.begin(SessionKind::Multiplayer);
                    MenuAction::Connect(entry)
                }
                // An empty list must not silently swallow Enter; open the add
                // form, which is the only useful thing to do here.
                None => {
                    self.form = EditForm::adding();
                    ui.open_server_edit();
                    MenuAction::None
                }
            },
            MenuKey::Delete => self.delete_selected(),
            MenuKey::Escape => {
                ui.on_escape();
                MenuAction::None
            }
            MenuKey::Char(c) => match c.to_ascii_lowercase() {
                'a' => {
                    self.form = EditForm::adding();
                    ui.open_server_edit();
                    MenuAction::None
                }
                'e' => match self.list.get(self.server) {
                    Some(entry) => {
                        self.form = EditForm::editing(self.server, entry);
                        ui.open_server_edit();
                        MenuAction::None
                    }
                    None => MenuAction::None,
                },
                'd' => self.delete_selected(),
                'r' => MenuAction::Reprobe(self.list.get(self.server).cloned()),
                _ => MenuAction::None,
            },
            _ => MenuAction::None,
        }
    }

    fn key_edit(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match key {
            MenuKey::Escape => {
                // Cancel: the form is discarded, the list is untouched.
                ui.close_server_edit();
                MenuAction::None
            }
            MenuKey::Tab | MenuKey::Up | MenuKey::Down => {
                self.form.next_field();
                MenuAction::None
            }
            MenuKey::Backspace => {
                self.form.backspace();
                MenuAction::None
            }
            MenuKey::Char(c) => {
                self.form.push(c);
                MenuAction::None
            }
            MenuKey::Enter => {
                if !self.form.is_valid() {
                    // Refuse rather than saving a row that cannot be dialed.
                    return MenuAction::None;
                }
                let entry = self.form.to_entry();
                let previous = self
                    .form
                    .editing
                    .and_then(|i| self.list.get(i))
                    .cloned();
                match self.form.editing {
                    Some(i) => {
                        self.list.update(i, entry.clone());
                    }
                    None => {
                        if let Some(i) = self.list.add(entry.clone()) {
                            self.server = i;
                        }
                    }
                }
                self.persist();
                ui.close_server_edit();
                self.clamp_server();
                // An edit that changed the address orphans the old row's cached
                // status; the app forgets it, then probes the new address.
                if let Some(old) = previous.filter(|p| p.host != entry.host || p.port != entry.port)
                {
                    return MenuAction::Forget(old);
                }
                MenuAction::Reprobe(Some(entry))
            }
            MenuKey::Delete => MenuAction::None,
        }
    }

    /// The settings screen has one interactive control today (GUI scale), so
    /// Up/Down step its value directly rather than moving a highlight between
    /// rows — there is nothing else to highlight yet.
    fn key_settings(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match key {
            MenuKey::Up => {
                self.cycle_gui_scale(1);
                MenuAction::None
            }
            MenuKey::Down => {
                self.cycle_gui_scale(-1);
                MenuAction::None
            }
            MenuKey::Escape => {
                ui.on_escape();
                MenuAction::None
            }
            _ => MenuAction::None,
        }
    }

    /// The pause menu: Up/Down move the highlight, Enter activates the
    /// highlighted button, Escape resumes play (same as [`UiState::on_escape`]
    /// from [`Screen::Paused`] — spelled out here too rather than falling
    /// through to a catch-all, now that this screen has its own arm).
    fn key_paused(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match key {
            MenuKey::Up => {
                self.paused = step_enabled(self.paused, PAUSE_BUTTONS.len(), false, &|i| {
                    PAUSE_BUTTONS[i].enabled()
                });
                MenuAction::None
            }
            MenuKey::Down => {
                self.paused = step_enabled(self.paused, PAUSE_BUTTONS.len(), true, &|i| {
                    PAUSE_BUTTONS[i].enabled()
                });
                MenuAction::None
            }
            MenuKey::Enter => {
                let button = self.pause_button();
                if !button.enabled() {
                    // See `key_main`'s equivalent guard.
                    return MenuAction::None;
                }
                match button {
                    PauseButton::BackToGame => {
                        ui.resume();
                        MenuAction::None
                    }
                    PauseButton::Options => {
                        ui.open_settings_from_pause();
                        MenuAction::None
                    }
                    PauseButton::QuitToTitle => {
                        ui.quit_to_title();
                        MenuAction::QuitToTitle
                    }
                    PauseButton::Advancements
                    | PauseButton::Statistics
                    | PauseButton::ReportBugs
                    | PauseButton::Feedback
                    | PauseButton::Friends
                    | PauseButton::PlayerReporting => MenuAction::None,
                }
            }
            MenuKey::Escape => {
                ui.on_escape();
                MenuAction::None
            }
            _ => MenuAction::None,
        }
    }

    /// Steps the persisted `gui_scale` option by `delta`, wrapping between
    /// `crate::config::AUTO_GUI_SCALE` and [`MAX_MANUAL_GUI_SCALE`] inclusive,
    /// and saves immediately — the same eager-persistence rule as the server
    /// list (see the module docs): there is no guaranteed clean-shutdown hook,
    /// so a setting that only saved on exit would be the setting a crash loses.
    fn cycle_gui_scale(&mut self, delta: i32) {
        // The cycle is `AUTO_GUI_SCALE..=MAX_MANUAL_GUI_SCALE`; `AUTO_GUI_SCALE`
        // is `0`, so `rem_euclid` already lands there without naming it.
        let span = MAX_MANUAL_GUI_SCALE as i32 + 1;
        let current = self.options.gui_scale as i32;
        let next = (current + delta).rem_euclid(span);
        self.options.gui_scale = next as u32;
        self.persist_options();
    }

    /// Writes the options to disk, recording (not swallowing) any failure —
    /// mirrors [`MenuNav::persist`].
    fn persist_options(&mut self) {
        self.options_save_error = match self.options.save_to(&self.options_path) {
            Ok(()) => None,
            Err(e) => Some(format!(
                "could not save {}: {e}",
                self.options_path.display()
            )),
        };
    }

    fn delete_selected(&mut self) -> MenuAction {
        match self.list.remove(self.server) {
            Some(gone) => {
                self.persist();
                self.clamp_server();
                MenuAction::Forget(gone)
            }
            None => MenuAction::None,
        }
    }

    /// Keeps the highlight inside the list after an add or a delete.
    fn clamp_server(&mut self) {
        if self.server >= self.list.len() {
            self.server = self.list.len().saturating_sub(1);
        }
    }

    /// Writes the list to disk, recording (not swallowing) any failure.
    fn persist(&mut self) {
        self.save_error = match self.list.save_to(&self.path) {
            Ok(()) => None,
            Err(e) => Some(format!("could not save {}: {e}", self.path.display())),
        };
    }
}

/// Steps `i` one row in `forward`'s direction, wrapping, and keeps stepping
/// while the row it lands on is disabled.
///
/// This is vanilla's own focus rule: `AbstractWidget::nextFocusPath` returns
/// `null` for an inactive widget (`AbstractWidget.java:152-158`), so keyboard
/// navigation never *lands* on a greyed-out button — which is what makes it safe
/// to reproduce vanilla's full widget list with most of it disabled without the
/// arrow keys walking through five dead rows.
///
/// Returns `i` unchanged when nothing in `0..len` is enabled. Neither real
/// button set can be in that state, but the loop bound is what keeps a future
/// all-disabled set from spinning forever rather than being a latent hang.
fn step_enabled(i: usize, len: usize, forward: bool, enabled: &dyn Fn(usize) -> bool) -> usize {
    if len == 0 {
        return 0;
    }
    let mut next = i.min(len - 1);
    for _ in 0..len {
        next = if forward {
            wrap_next(next, len)
        } else {
            wrap_prev(next, len)
        };
        if enabled(next) {
            return next;
        }
    }
    i
}

fn wrap_next(i: usize, len: usize) -> usize {
    if len == 0 { 0 } else { (i + 1) % len }
}

fn wrap_prev(i: usize, len: usize) -> usize {
    if len == 0 {
        0
    } else if i == 0 {
        len - 1
    } else {
        i - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A nav whose list persists to a unique temporary file, so tests exercise
    /// the *real* save path without touching the developer's server list.
    fn nav(tag: &str) -> (MenuNav, std::path::PathBuf) {
        let path = std::env::temp_dir().join(format!(
            "lodestone-nav-{}-{tag}/servers.json",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        (MenuNav::with_path(path.clone()), path)
    }

    fn type_str(nav: &mut MenuNav, ui: &mut UiState, s: &str) {
        for c in s.chars() {
            nav.key(ui, MenuKey::Char(c));
        }
    }

    #[test]
    fn main_menu_selection_wraps_both_ways() {
        let (mut nav, _) = nav("wrap");
        let mut ui = UiState::new();
        assert_eq!(nav.main_button(), MainButton::Singleplayer);
        nav.key(&mut ui, MenuKey::Up);
        assert_eq!(nav.main_button(), MainButton::Quit, "up from the top wraps");
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.main_button(), MainButton::Singleplayer);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.main_button(), MainButton::Multiplayer);
    }

    #[test]
    fn the_main_menu_buttons_do_what_they_say() {
        // This test's key sequence was wrong from the commit that introduced
        // it (e6fd783): it pressed `Up` only once after landing on
        // Multiplayer and expected to land on `Quit`, which is only true if
        // `Up` wraps forward — it does not (`main_menu_selection_wraps_both_ways`
        // pins `Up` from the *top* row wrapping to the *last* one, i.e.
        // backwards through the list). One agent's fix attempt blamed the
        // pause-menu `Options` insertion for reordering `MAIN_BUTTONS`, but
        // replaying this exact sequence against the commit that introduced
        // the test — three buttons, no `Options` in existence yet — fails
        // identically. The test, not `MAIN_BUTTONS`, was wrong.
        let (mut nav, _) = nav("buttons");
        let mut ui = UiState::new();
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::Singleplayer);
        assert_eq!(ui.screen(), Screen::MainMenu, "the app drives the world");

        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.main_button(), MainButton::Multiplayer);
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::Reprobe(None));
        assert_eq!(ui.screen(), Screen::ServerList);

        ui.on_escape();
        assert_eq!(ui.screen(), Screen::MainMenu);
        assert_eq!(
            nav.main_button(),
            MainButton::Multiplayer,
            "escape returns to the title without moving the highlight"
        );

        // Quit is always the last button: wrapping `Up` from the first row
        // reaches it regardless of how many rows sit in between (that
        // invariant is what `main_menu_selection_wraps_both_ways` pins down
        // in isolation). Walk there for real, exercising the same edges.
        nav.key(&mut ui, MenuKey::Up);
        assert_eq!(nav.main_button(), MainButton::Singleplayer);
        nav.key(&mut ui, MenuKey::Up);
        assert_eq!(nav.main_button(), MainButton::Quit, "up from the top wraps");
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::Quit);
        assert!(ui.quit_requested());
    }

    #[test]
    fn add_edit_delete_round_trips_through_a_real_file() {
        // The end-to-end persistence path, driven only by keys — the same calls
        // the window makes.
        let (mut nav, path) = nav("crud");
        let mut ui = UiState::new();
        ui.open_server_list();

        // Add: 'a', type a name, Tab, type an address, Enter.
        nav.key(&mut ui, MenuKey::Char('a'));
        assert_eq!(ui.screen(), Screen::ServerEdit);
        type_str(&mut nav, &mut ui, "Home");
        nav.key(&mut ui, MenuKey::Tab);
        type_str(&mut nav, &mut ui, "mc.example.com:25566");
        let action = nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(ui.screen(), Screen::ServerList, "saving returns to the list");
        assert_eq!(nav.list().len(), 1);
        assert_eq!(nav.list().get(0).unwrap().name, "Home");
        assert_eq!(nav.list().get(0).unwrap().host, "mc.example.com");
        assert_eq!(nav.list().get(0).unwrap().port, Some(25566));
        assert!(
            matches!(action, MenuAction::Reprobe(Some(_))),
            "a saved entry should be probed: {action:?}"
        );
        assert_eq!(nav.save_error(), None, "the save must have succeeded");

        // It is on disk *now*, not at exit: a fresh nav sees it.
        assert_eq!(MenuNav::with_path(path.clone()).list().len(), 1);

        // Edit: 'e', clear the name, retype, Enter.
        nav.key(&mut ui, MenuKey::Char('e'));
        assert_eq!(ui.screen(), Screen::ServerEdit);
        assert_eq!(nav.form().name, "Home", "the form pre-fills");
        assert_eq!(nav.form().address, "mc.example.com:25566");
        for _ in 0..8 {
            nav.key(&mut ui, MenuKey::Backspace);
        }
        type_str(&mut nav, &mut ui, "Away");
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(nav.list().get(0).unwrap().name, "Away");
        assert_eq!(MenuNav::with_path(path.clone()).list().get(0).unwrap().name, "Away");

        // Delete.
        let action = nav.key(&mut ui, MenuKey::Delete);
        assert!(matches!(action, MenuAction::Forget(_)), "{action:?}");
        assert!(nav.list().is_empty());
        assert!(MenuNav::with_path(path.clone()).list().is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn cancelling_the_form_leaves_the_list_untouched() {
        // The bug this guards: Escape from the edit form saving anyway.
        let (mut nav, _) = nav("cancel");
        let mut ui = UiState::new();
        ui.open_server_list();
        nav.key(&mut ui, MenuKey::Char('a'));
        type_str(&mut nav, &mut ui, "ghost");
        nav.key(&mut ui, MenuKey::Tab);
        type_str(&mut nav, &mut ui, "nowhere.example");
        nav.key(&mut ui, MenuKey::Escape);
        assert_eq!(ui.screen(), Screen::ServerList);
        assert!(nav.list().is_empty(), "a cancelled form must save nothing");
    }

    #[test]
    fn an_addressless_form_refuses_to_save() {
        let (mut nav, _) = nav("empty");
        let mut ui = UiState::new();
        ui.open_server_list();
        nav.key(&mut ui, MenuKey::Char('a'));
        type_str(&mut nav, &mut ui, "just a label");
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert_eq!(
            ui.screen(),
            Screen::ServerEdit,
            "an invalid form must stay open rather than silently dropping input"
        );
        assert!(nav.list().is_empty());
    }

    #[test]
    fn a_nameless_entry_falls_back_to_its_host() {
        let (mut nav, _) = nav("noname");
        let mut ui = UiState::new();
        ui.open_server_list();
        nav.key(&mut ui, MenuKey::Char('a'));
        nav.key(&mut ui, MenuKey::Tab);
        type_str(&mut nav, &mut ui, "bare.example");
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(nav.list().get(0).unwrap().name, "bare.example");
    }

    #[test]
    fn typing_in_the_list_is_a_command_and_typing_in_the_form_is_text() {
        // 'a' must add a server from the list and type an 'a' in the form. Get
        // this backwards and the list is unusable or the form cannot spell
        // "australia.example.com".
        let (mut nav, _) = nav("modal");
        let mut ui = UiState::new();
        ui.open_server_list();
        nav.key(&mut ui, MenuKey::Char('a'));
        assert_eq!(ui.screen(), Screen::ServerEdit);
        nav.key(&mut ui, MenuKey::Tab);
        type_str(&mut nav, &mut ui, "aaa.example");
        assert_eq!(nav.form().address, "aaa.example");
        assert_eq!(ui.screen(), Screen::ServerEdit, "text must not navigate");
    }

    #[test]
    fn enter_on_a_row_connects_and_shows_the_loading_screen() {
        let (mut nav, _) = nav("connect");
        let mut ui = UiState::new();
        ui.open_server_list();
        nav.key(&mut ui, MenuKey::Char('a'));
        nav.key(&mut ui, MenuKey::Tab);
        type_str(&mut nav, &mut ui, "play.example");
        nav.key(&mut ui, MenuKey::Enter);

        match nav.key(&mut ui, MenuKey::Enter) {
            MenuAction::Connect(e) => {
                assert_eq!(e.host, "play.example");
                assert_eq!(e.effective_port(), super::super::servers::DEFAULT_PORT);
            }
            other => panic!("expected Connect, got {other:?}"),
        }
        assert!(ui.is_connecting(), "the app must show a loading screen");
        assert!(!ui.wants_cursor_grab());
    }

    #[test]
    fn enter_on_an_empty_list_opens_the_add_form_instead_of_doing_nothing() {
        let (mut nav, _) = nav("emptyenter");
        let mut ui = UiState::new();
        ui.open_server_list();
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert_eq!(ui.screen(), Screen::ServerEdit);
    }

    #[test]
    fn navigation_on_an_empty_list_cannot_panic_or_point_off_the_end() {
        let (mut nav, _) = nav("emptynav");
        let mut ui = UiState::new();
        ui.open_server_list();
        for _ in 0..5 {
            nav.key(&mut ui, MenuKey::Up);
            nav.key(&mut ui, MenuKey::Down);
        }
        assert_eq!(nav.server_index(), 0);
        assert_eq!(nav.key(&mut ui, MenuKey::Delete), MenuAction::None);
        assert_eq!(nav.key(&mut ui, MenuKey::Char('e')), MenuAction::None);
    }

    #[test]
    fn deleting_the_last_row_moves_the_highlight_back_onto_the_list() {
        // The bug this guards: an index left one past the end, which the
        // renderer would then read as a missing row (or panic on `[]`).
        let (mut nav, path) = nav("clamp");
        let mut ui = UiState::new();
        ui.open_server_list();
        for host in ["a.example", "b.example", "c.example"] {
            nav.key(&mut ui, MenuKey::Char('a'));
            nav.key(&mut ui, MenuKey::Tab);
            type_str(&mut nav, &mut ui, host);
            nav.key(&mut ui, MenuKey::Enter);
        }
        assert_eq!(nav.list().len(), 3);
        assert_eq!(nav.server_index(), 2, "adding highlights the new row");

        nav.key(&mut ui, MenuKey::Delete);
        assert_eq!(nav.server_index(), 1);
        assert!(nav.list().get(nav.server_index()).is_some());
        nav.key(&mut ui, MenuKey::Delete);
        nav.key(&mut ui, MenuKey::Delete);
        assert_eq!(nav.server_index(), 0);
        assert!(nav.list().is_empty());
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn changing_a_rows_address_forgets_the_old_cached_status() {
        // Otherwise the row keeps showing the MOTD of the server it used to be.
        let (mut nav, _) = nav("readdress");
        let mut ui = UiState::new();
        ui.open_server_list();
        nav.key(&mut ui, MenuKey::Char('a'));
        nav.key(&mut ui, MenuKey::Tab);
        type_str(&mut nav, &mut ui, "old.example");
        nav.key(&mut ui, MenuKey::Enter);

        nav.key(&mut ui, MenuKey::Char('e'));
        nav.key(&mut ui, MenuKey::Tab);
        for _ in 0..40 {
            nav.key(&mut ui, MenuKey::Backspace);
        }
        type_str(&mut nav, &mut ui, "new.example");
        match nav.key(&mut ui, MenuKey::Enter) {
            MenuAction::Forget(old) => assert_eq!(old.host, "old.example"),
            other => panic!("expected Forget(old), got {other:?}"),
        }

        // Renaming *without* changing the address keeps the cached status.
        nav.key(&mut ui, MenuKey::Char('e'));
        type_str(&mut nav, &mut ui, "!");
        match nav.key(&mut ui, MenuKey::Enter) {
            MenuAction::Reprobe(Some(e)) => assert_eq!(e.host, "new.example"),
            other => panic!("a rename should re-probe, not forget: {other:?}"),
        }
    }

    #[test]
    fn r_refreshes_the_highlighted_row() {
        let (mut nav, _) = nav("refresh");
        let mut ui = UiState::new();
        ui.open_server_list();
        nav.key(&mut ui, MenuKey::Char('a'));
        nav.key(&mut ui, MenuKey::Tab);
        type_str(&mut nav, &mut ui, "r.example");
        nav.key(&mut ui, MenuKey::Enter);
        match nav.key(&mut ui, MenuKey::Char('r')) {
            MenuAction::Reprobe(Some(e)) => assert_eq!(e.host, "r.example"),
            other => panic!("expected a single-row reprobe, got {other:?}"),
        }
    }

    #[test]
    fn text_fields_reject_control_characters_and_respect_their_caps() {
        let mut form = EditForm::adding();
        form.push('\n');
        form.push('\u{a7}');
        form.push('\t');
        assert!(form.name.is_empty(), "control chars must not enter a field");

        for _ in 0..1000 {
            form.push('x');
        }
        assert_eq!(form.name.chars().count(), MAX_NAME_CHARS);
        form.next_field();
        for _ in 0..1000 {
            form.push('y');
        }
        assert_eq!(form.address.chars().count(), MAX_ADDRESS_CHARS);
    }

    #[test]
    fn a_save_failure_is_reported_rather_than_swallowed() {
        // A player who adds a server and sees it vanish deserves the reason.
        // `/dev/null/...` cannot be a directory on any Unix.
        let mut nav = MenuNav::with_path(std::path::PathBuf::from("/dev/null/nope/servers.json"));
        let mut ui = UiState::new();
        ui.open_server_list();
        nav.key(&mut ui, MenuKey::Char('a'));
        nav.key(&mut ui, MenuKey::Tab);
        type_str(&mut nav, &mut ui, "x.example");
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(nav.list().len(), 1, "the in-memory list still updates");
        let err = nav.save_error().expect("a failed write must be reported");
        assert!(err.contains("servers.json"), "unhelpful message: {err}");
    }

    #[test]
    fn escape_from_the_edit_form_never_quits_the_game() {
        // Escape must unwind one level at a time all the way out.
        let (mut nav, _) = nav("unwind");
        let mut ui = UiState::new();
        ui.open_server_list();
        nav.key(&mut ui, MenuKey::Char('a'));
        assert_eq!(nav.key(&mut ui, MenuKey::Escape), MenuAction::None);
        assert!(!ui.quit_requested());
        assert_eq!(nav.key(&mut ui, MenuKey::Escape), MenuAction::None);
        assert_eq!(ui.screen(), Screen::MainMenu);
        assert!(!ui.quit_requested());
        assert_eq!(nav.key(&mut ui, MenuKey::Escape), MenuAction::Quit);
        assert!(ui.quit_requested());
    }

    #[test]
    fn options_button_sits_between_multiplayer_and_quit_and_opens_settings() {
        let (mut nav, _) = nav("options-button");
        let mut ui = UiState::new();
        // Singleplayer, Multiplayer, Options, Quit, in that order — inserting
        // Options must not disturb Multiplayer's index (existing wrap tests
        // rely on it staying at 1) or Quit's position as the last button.
        assert_eq!(nav.main_button(), MainButton::Singleplayer);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.main_button(), MainButton::Multiplayer);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.main_button(), MainButton::Options);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.main_button(), MainButton::Quit);

        nav.key(&mut ui, MenuKey::Up);
        assert_eq!(nav.main_button(), MainButton::Options);
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert_eq!(ui.screen(), Screen::Settings);
    }

    #[test]
    fn settings_up_down_cycles_the_gui_scale_and_persists_through_a_real_file() {
        let (mut nav, path) = nav("settings-cycle");
        let mut ui = UiState::new();
        ui.open_settings();
        assert_eq!(nav.gui_scale(), 0, "starts at auto");

        nav.key(&mut ui, MenuKey::Up);
        assert_eq!(nav.gui_scale(), 1);
        nav.key(&mut ui, MenuKey::Up);
        assert_eq!(nav.gui_scale(), 2);
        assert_eq!(nav.options_save_error(), None);

        // It is on disk *now*, not at exit.
        let options_path = path.parent().unwrap().join("options.json");
        assert_eq!(
            crate::config::Options::load_from(&options_path).gui_scale,
            2
        );

        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.gui_scale(), 1, "down steps back");

        // Down from auto wraps to the top, and up from the top wraps back to
        // auto — this is what makes the control a *cycle*, not a clamp.
        // `nav` is shadowed by the `MenuNav` local above (functions and
        // locals share Rust's value namespace), so reach the helper through
        // its module path instead of renaming the well-established `nav`
        // binding used throughout this test.
        let (mut wrap_nav, _) = self::nav("settings-cycle-wrap");
        let mut wrap_ui = UiState::new();
        wrap_ui.open_settings();
        assert_eq!(wrap_nav.gui_scale(), 0);
        wrap_nav.key(&mut wrap_ui, MenuKey::Down);
        assert_eq!(
            wrap_nav.gui_scale(),
            crate::config::MAX_MANUAL_GUI_SCALE,
            "down from auto wraps to the top"
        );
        wrap_nav.key(&mut wrap_ui, MenuKey::Up);
        assert_eq!(wrap_nav.gui_scale(), 0, "up from the top wraps back to auto");
    }

    #[test]
    fn escape_from_settings_returns_to_the_main_menu_without_quitting() {
        let (mut nav, _) = nav("settings-escape");
        let mut ui = UiState::new();
        ui.open_settings();
        assert_eq!(nav.key(&mut ui, MenuKey::Escape), MenuAction::None);
        assert_eq!(ui.screen(), Screen::MainMenu);
        assert!(!ui.quit_requested());
    }

    #[test]
    fn a_settings_save_failure_is_reported_rather_than_swallowed() {
        let mut nav = MenuNav::with_paths(
            std::env::temp_dir().join(format!(
                "lodestone-nav-{}-settingsfail/servers.json",
                std::process::id()
            )),
            std::path::PathBuf::from("/dev/null/nope/options.json"),
        );
        let mut ui = UiState::new();
        ui.open_settings();
        nav.key(&mut ui, MenuKey::Up);
        assert_eq!(nav.gui_scale(), 1, "the in-memory option still updates");
        let err = nav
            .options_save_error()
            .expect("a failed write must be reported");
        assert!(err.contains("options.json"), "unhelpful message: {err}");
    }

    #[test]
    fn pause_menu_selection_wraps_both_ways() {
        let (mut nav, _) = nav("pause-wrap");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.pause();
        assert_eq!(nav.pause_button(), PauseButton::BackToGame);

        nav.key(&mut ui, MenuKey::Up);
        assert_eq!(
            nav.pause_button(),
            PauseButton::QuitToTitle,
            "up from the top wraps"
        );
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.pause_button(), PauseButton::BackToGame);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.pause_button(), PauseButton::Options);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.pause_button(), PauseButton::QuitToTitle);
    }

    #[test]
    fn back_to_game_resumes_play() {
        let (mut nav, _) = nav("pause-resume");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.pause();
        assert_eq!(nav.pause_button(), PauseButton::BackToGame);
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert!(
            ui.is_playing(),
            "the highlighted Back to Game button resumed"
        );
    }

    #[test]
    fn pause_options_opens_settings_and_escape_returns_to_pause() {
        let (mut nav, _) = nav("pause-options");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.pause();
        nav.key(&mut ui, MenuKey::Down); // BackToGame -> Options
        assert_eq!(nav.pause_button(), PauseButton::Options);

        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert_eq!(ui.screen(), Screen::Settings);
        assert!(!ui.wants_cursor_grab());

        assert_eq!(nav.key(&mut ui, MenuKey::Escape), MenuAction::None);
        assert!(
            ui.is_paused(),
            "escape from options opened out of the pause menu must return \
             there, not skip past it into play or the title"
        );
    }

    #[test]
    fn quit_to_title_from_the_pause_menu_leaves_for_the_main_menu() {
        let (mut nav, _) = nav("pause-quit");
        let mut ui = UiState::new();
        ui.begin(SessionKind::Multiplayer);
        ui.session_ready();
        ui.pause();
        nav.key(&mut ui, MenuKey::Up); // BackToGame -> QuitToTitle (wraps)
        assert_eq!(nav.pause_button(), PauseButton::QuitToTitle);

        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::QuitToTitle);
        assert_eq!(
            ui.screen(),
            Screen::MainMenu,
            "the ui state has already left, independent of the app's teardown"
        );
        assert!(ui.kind().is_none());
    }

    #[test]
    fn pause_menu_escape_resumes_play() {
        let (mut nav, _) = nav("pause-escape");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.pause();
        assert_eq!(nav.key(&mut ui, MenuKey::Escape), MenuAction::None);
        assert!(ui.is_playing());
    }

    #[test]
    fn hovering_a_pause_row_moves_the_highlight() {
        let (mut nav, _) = nav("pause-hover");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.pause();
        assert_eq!(nav.pause_index(), 0);
        // Disconnect is the last of vanilla's nine pause widgets, not the third
        // of three — this index moved when the screen gained vanilla's full
        // structure (Advancements, Statistics and the four icon buttons).
        let last = PAUSE_BUTTONS.len() - 1;
        nav.hover(&ui, last);
        assert_eq!(nav.pause_button(), PauseButton::QuitToTitle);
        // Out-of-range rows are ignored rather than clamped.
        nav.hover(&ui, 99);
        assert_eq!(nav.pause_button(), PauseButton::QuitToTitle);
    }

    #[test]
    fn a_disabled_button_is_hoverable_but_cannot_be_activated() {
        // The specific regression: `app.rs` turns a click into `hover(row)` then
        // `MenuKey::Enter`. If `hover` refused a disabled row, the Enter would
        // fall through and activate whatever was highlighted *before* — clicking
        // the greyed-out Advancements button would disconnect you. And if Enter
        // did not refuse, a disabled button would act.
        let (mut nav, _) = nav("disabled-click");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.pause();

        // Select the real Disconnect button first, so a fall-through would be
        // observable as a session teardown.
        let last = PAUSE_BUTTONS.len() - 1;
        nav.hover(&ui, last);
        assert_eq!(nav.pause_button(), PauseButton::QuitToTitle);

        // Now click Advancements (index 1, disabled).
        nav.hover(&ui, 1);
        assert_eq!(
            nav.pause_button(),
            PauseButton::Advancements,
            "a disabled button is still hovered, exactly as in vanilla"
        );
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert!(
            ui.is_paused(),
            "clicking a disabled button must neither act nor fall through to the \
             previously highlighted one"
        );

        // The positive control: the same click sequence on an *enabled* button
        // does act, so the assertion above is not passing because clicks are
        // broken generally.
        nav.hover(&ui, last);
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::QuitToTitle);
        assert_eq!(ui.screen(), Screen::MainMenu);
    }

    #[test]
    fn keyboard_navigation_steps_over_every_disabled_button() {
        // Vanilla's own focus rule: arrow keys never land on a greyed-out
        // widget. Both screens carry five/six disabled rows now, so without this
        // the arrow keys would walk through dead rows.
        let (mut nav, _) = nav("skip-disabled");
        let mut ui = UiState::new();

        // Title screen: Singleplayer, Multiplayer, Options, Quit — Realms and
        // the three icon buttons are stepped over in both directions.
        let mut seen = vec![nav.main_button()];
        for _ in 0..3 {
            nav.key(&mut ui, MenuKey::Down);
            seen.push(nav.main_button());
        }
        assert_eq!(
            seen,
            vec![
                MainButton::Singleplayer,
                MainButton::Multiplayer,
                MainButton::Options,
                MainButton::Quit
            ]
        );
        for _ in 0..8 {
            nav.key(&mut ui, MenuKey::Up);
            assert!(
                nav.main_button().enabled(),
                "Up landed on {:?}, which is disabled",
                nav.main_button()
            );
        }

        // Pause screen: Back to Game, Options, Disconnect.
        ui.enter_dev_world();
        ui.pause();
        let mut seen = vec![nav.pause_button()];
        for _ in 0..2 {
            nav.key(&mut ui, MenuKey::Down);
            seen.push(nav.pause_button());
        }
        assert_eq!(
            seen,
            vec![
                PauseButton::BackToGame,
                PauseButton::Options,
                PauseButton::QuitToTitle
            ]
        );
        for _ in 0..9 {
            nav.key(&mut ui, MenuKey::Up);
            assert!(
                nav.pause_button().enabled(),
                "Up landed on {:?}, which is disabled",
                nav.pause_button()
            );
        }

        // The negative control the two loops above need: the sets really do
        // contain disabled buttons, so "every landing was enabled" is a
        // measurement and not a tautology.
        assert!(
            MAIN_BUTTONS.iter().any(|b| !b.enabled()),
            "no disabled title-screen button to step over"
        );
        assert!(
            PAUSE_BUTTONS.iter().any(|b| !b.enabled()),
            "no disabled pause-screen button to step over"
        );
    }

    #[test]
    fn menu_keys_do_nothing_on_the_world_screens() {
        // A stray key from the menu mapping must never mutate the world's state.
        let (mut nav, _) = nav("world");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        for key in [
            MenuKey::Up,
            MenuKey::Down,
            MenuKey::Enter,
            MenuKey::Delete,
            MenuKey::Char('d'),
        ] {
            assert_eq!(nav.key(&mut ui, key), MenuAction::None, "{key:?}");
            assert!(ui.is_playing(), "{key:?} left the world");
        }
    }
}
