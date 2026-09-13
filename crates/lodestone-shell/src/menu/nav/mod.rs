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

pub(super) use super::{book_edit, book_view, command_block, edit_box, servers, sign_edit, spectator_menu};
pub(super) use super::focus::{self, KeyEvent};
pub(super) use super::servers::{ServerList, servers_path};
pub(super) use super::widget;
pub(super) use super::options::LiveOption;
pub(super) use super::{render, server_links};
pub(super) use super::{Screen, SessionKind, UiState};
pub(super) use crate::config::{MAX_MANUAL_GUI_SCALE, Options};
pub(super) use lodestone_auth::Entitlement;

mod buttons;
mod form;
mod model;
mod construct;
mod state;
mod scroll;
mod session;
mod input;
mod keys;
mod settings;
mod session_input;
mod overlays;

pub use buttons::*;
pub use form::*;
pub use model::*;
pub use overlays::*;

#[derive(Debug)]
pub struct MenuNav {
    main: usize,
    /// Highlighted row on the ownership gate ([`OWNERSHIP_BUTTONS`]).
    ///
    /// Its own cursor rather than a reuse of `main`: the gate and the title
    /// screen can both be on screen across one transition (adding an account
    /// moves from one to the other), and a shared cursor would carry the gate's
    /// "Quit" row onto the title screen's fourth button.
    ownership: usize,
    server: usize,
    /// Highlighted row on the pause menu ([`PAUSE_BUTTONS`]).
    paused: usize,
    /// Highlighted row on the death screen ([`DEATH_BUTTONS`]).
    death: usize,
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
    /// The account list + sign-in flow. See
    /// [`crate::menu::accounts`].
    accounts: crate::menu::accounts::AccountsNav,
    /// The world-select screen's widgets and focus. Held here for
    /// [`EditForm`]'s reason: it owns real [`super::edit_box::EditBox`] state (a caret, a
    /// selection, a scroll offset) that cannot be rebuilt per frame.
    world_select: crate::menu::world_select::WorldSelectNav,
    /// The root every singleplayer world folder lives under — [`crate::saves`]'s
    /// `saves_dir()` in production, a temp directory in every test.
    ///
    /// **Held here rather than read from [`crate::saves::saves_dir`] at the call
    /// site**, and that is the mechanism that keeps the suite off the developer's
    /// real saves folder: it is derived from [`Self::path`]'s own directory
    /// exactly as `options_path`/`profiles_path`/`hidden_players_path` are, so a
    /// test that points `MenuNav` at a temp `servers.json` gets a temp `saves/`
    /// for free and cannot forget to. See `crate::saves`'s module doc on why a
    /// `cfg!(test)` early return would have been the wrong shape.
    saves_root: std::path::PathBuf,
    /// Which [`SERVER_LIST_BUTTONS`] entry the cursor is over, if any.
    ///
    /// Separate from [`Self::server`] because the two are different cursors that
    /// are visible at once: the selected *server* keeps its outline while a footer
    /// button under the mouse draws highlighted.
    list_button: Option<usize>,
    /// How far the multiplayer list is scrolled down, **in logical pixels** —
    /// vanilla's `AbstractScrollArea.scrollAmount`, which is a `double` and is
    /// subtracted straight from a row's y.
    ///
    /// **The offset is a `f32` pixel distance, not a `usize` row counter**: one wheel
    /// notch advances by
    /// `scrollY * scrollRate()` where `scrollRate = defaultEntryHeight / 2`
    /// (vanilla's own abstract scroll-area base, `:141-142`, vanilla's own abstract selection-list base
    /// via `defaultSettings`), i.e. **18 px** for a 36 px row — a value a row
    /// index structurally cannot hold, so the list jumped a whole entry per
    /// notch. See [`Self::scroll_server_list`], which now delegates to
    /// [`super::widget::ScrollList`] rather than reimplementing the clamp.
    ///
    /// The clip is required because a straddling row must not paint over the footer.
    /// `render.rs` wraps `draw_server_entry` in `Quads::with_clip`
    /// against the same band this offset is clamped against — which is the
    /// precondition that makes a pixel offset safe to draw.
    ///
    /// Not persisted and reset to `0.0` whenever the screen is (re)opened from
    /// the title, matching vanilla building a fresh `JoinMultiplayerScreen` —
    /// see [`Self::key_main`]'s `MainButton::Multiplayer` arm.
    server_scroll: f32,
    /// The last known mouse position in **logical** pixels, and the canvas it was
    /// measured in.
    ///
    /// `app.rs` already resolves the cursor to a logical position inside
    /// `menu_row_at`; this is where it records it, so the *menu* can answer
    /// position questions a row index cannot. There is exactly one such question
    /// so far and it is vanilla's: which quadrant of a server row's 32 px favicon
    /// the cursor is in decides whether a click joins, moves the row up, or moves
    /// it down.
    ///
    /// `None` until the first `CursorMoved`, which is the state a keyboard-only
    /// session is in — and the quadrant actions must then simply not fire, rather
    /// than behaving as if the cursor were at `(0, 0)`.
    menu_cursor: Option<(f32, f32, f32, f32)>,
    /// The settings tree's own cursor — which of the nine pages is showing,
    /// where the cursor is on it, and how far its `OptionsList` is scrolled.
    /// See [`super::options::SettingsNav`].
    ///
    /// Held here rather than in [`UiState`] because it is *navigation state*,
    /// like [`Self::main`] and [`Self::paused`]: `Screen::Settings` is one screen
    /// however deep the page stack is, and `UiState` models legal screen edges
    /// only.
    settings: crate::menu::options::SettingsNav,
    /// The Social Interactions screen's own cursor, roster snapshot and
    /// hidden-player choices. Held here for the same reason
    /// [`Self::settings`] is: `Screen::Social` is one screen regardless of how
    /// far its list is scrolled, and `UiState` models legal screen edges only.
    social: crate::menu::social::SocialNav,
    /// The Friends screen's credential-free view, focus and queued intents.
    /// The app refreshes the view; only it can forward intents to the worker.
    friends: crate::menu::friends::FriendsNav,
    /// The Statistics screen's own scroll cursor. No persisted
    /// state of its own — see [`crate::menu::stats::StatsNav`]'s doc.
    stats: crate::menu::stats::StatsNav,
    /// The counters the Statistics screen draws, refreshed once per frame from
    /// `lodestone_ecs::SessionStatistics` by `app::session`.
    ///
    /// Beside `StatsNav` rather than inside it, because `StatsNav` is `Copy` and a
    /// sparse counter map is not — and because the *lifetimes* differ: the scroll
    /// and focus reset when the screen opens, while the counters belong to the
    /// session. Empty is the honest default outside one.
    stats_snapshot: crate::menu::stats::StatsSnapshot,
    /// The Server Links screen's own view (list or confirmation) and hover
    /// cursor, plus the server's live link list — refreshed once per frame by
    /// `app::session`, [`Self::stats_snapshot`]'s exact shape and for the same
    /// reason: the screen and its data have different lifetimes (the view
    /// resets on entry, the links belong to the session).
    server_links: crate::menu::server_links::ServerLinksNav,
    /// The Advancements screen's selected tab and per-tab scroll.
    /// Held here for [`Self::stats`]' reason: `Screen::Advancements` is one screen
    /// however far its tree is panned, and `UiState` models legal screen edges
    /// only. Reset on every entry from the pause menu, matching vanilla's
    /// per-screen `AdvancementTab` lifetime.
    advancements: crate::menu::advancements::AdvancementsState,
    /// The World Creation screen's own widgets, focus and collected config.
    /// Held here for the same reason [`Self::form`] is: it owns
    /// real [`super::edit_box::EditBox`] state that cannot be rebuilt per frame.
    create_world: crate::menu::create_world::CreateWorldNav,
    /// The live confirmation screen's own widgets, focus and request. Held here
    /// because the widgets carry focus state that cannot be rebuilt per frame,
    /// and are **replaced** rather than mutated every time a confirmation is opened;
    /// this prevents stale focus or a stale target from surviving into the next one.
    /// A confirmation that
    /// remembered the last answer is the failure mode this rules out.
    confirm: crate::menu::confirm::ConfirmNav,
    /// The live resource-pack prompt's own widgets, focus and pack id, held
    /// for [`Self::command_block`]'s reason: it owns real widget focus state
    /// that cannot be rebuilt per frame, and there is no non-empty default
    /// to construct eagerly, since it is entirely server-driven (a
    /// `net::PendingResourcePackPrompt`, not a menu button). `None` whenever
    /// [`Screen::ResourcePackPrompt`](super::Screen::ResourcePackPrompt) is
    /// not showing — see [`Self::open_resource_pack_prompt`].
    resource_pack_prompt: Option<crate::menu::confirm::ResourcePackPromptNav>,
    /// The id of the resource-pack prompt this side last answered
    /// (Accept/Decline), kept until `app/session.rs`'s
    /// `drive_ui_from_session` observes the ground truth
    /// (`NetClient::pending_resource_pack_prompt`) catch up to `None`.
    ///
    /// [`Self::apply_resource_pack_prompt`] closes this screen the instant
    /// the player answers, but `NetClient::respond_to_resource_pack` only
    /// *queues* the answer for the net thread's own loop to drain — up to
    /// 15 ms later on native, and only on that loop's next iteration on
    /// wasm32 — so the shared cell a fresh reconcile reads is still `Some`
    /// with the *same* id for a little while after. Without this, the
    /// reconcile's own "not currently showing, but the ground truth says
    /// pending" edge re-triggers on that stale read and reopens the exact
    /// prompt just answered, which is indistinguishable from "Accept did
    /// nothing" — the owner's report. This field lets the reconcile tell
    /// "still the prompt I already answered" apart from "a new one", without
    /// changing which thread clears the shared cell or when.
    resource_pack_answered_id: Option<uuid::Uuid>,
    /// A double-click on a **selection-list row** activates it: a server row
    /// joins (vanilla's own server-selection list rendering, `if (doubleClick) join()`,
    /// unconditional on where in the row the click landed), an account row
    /// selects that account. The primitive is
    /// [`super::focus::DoubleClickTracker`].
    ///
    /// **Keyed by `(Screen, usize)` rather than by the row alone**, because it
    /// is shared by more than one screen and a bare row index is not a unique
    /// target across them: with a `usize` key, clicking server row 0 and then
    /// account row 0 inside the 250 ms threshold reads as a double-click on
    /// one row and fires an activation nobody asked for. The screen is part of
    /// what "the same thing was clicked twice" means.
    double_click: super::focus::DoubleClickTracker<(super::Screen, usize)>,
    /// The monotonic clock [`Self::double_click`] measures against. An
    /// `Instant` fixed at construction rather than reset per click — only
    /// the *differences* `DoubleClickTracker` computes matter, so nothing
    /// needs rearming.
    click_clock: crate::platform::Instant,
    /// The command block edit screen's widgets and toggles, held
    /// for the same reason [`Self::form`] is: it owns a real [`super::edit_box::EditBox`] that
    /// cannot be rebuilt per frame. `None` whenever
    /// [`Screen::CommandBlockEdit`](super::Screen::CommandBlockEdit) is not
    /// showing — unlike [`Self::form`], which always has *some* value because
    /// [`Screen::ServerEdit`](super::Screen::ServerEdit) is always reached
    /// through a button that seeds one first, this screen has no such
    /// producer yet (see [`command_block`]'s module doc), so there is no
    /// non-empty default to construct eagerly.
    command_block: Option<command_block::CommandBlockState>,
    /// The sign-editing screen's four line fields and active-line focus,
    /// held for the same reason [`Self::command_block`] is. `None` whenever
    /// [`Screen::SignEdit`] is not showing — this screen is server-driven
    /// (see its own doc), so there is no non-empty default to construct
    /// eagerly, exactly as for [`Self::command_block`].
    sign_edit: Option<sign_edit::SignEditState>,
    /// The book-editing screen's page/title widgets, held for the same
    /// reason [`Self::command_block`] is. `None` whenever
    /// [`Screen::BookEdit`](super::Screen::BookEdit) is not showing — this
    /// screen is client-local, the same as [`Self::command_block`] and
    /// unlike [`Self::sign_edit`], so there is equally no non-empty default
    /// to construct eagerly.
    book_edit: Option<book_edit::BookEditState>,
    /// The signed-book reading screen's page state, held for exactly the
    /// same reason [`Self::book_edit`] is, and `None` whenever
    /// [`Screen::BookView`](super::Screen::BookView) is not showing. A stack
    /// is either writable or written, so this and [`Self::book_edit`] are
    /// never both `Some`.
    book_view: Option<book_view::BookViewState>,
    /// The Spectator Menu's roster and expand/hover state (the
    /// `TeleportToEntity` remainder). **Not** an `Option` like
    /// [`Self::book_edit`] above — this screen's roster is live-refreshed
    /// every frame while connected, the same shape [`Self::social`] uses for
    /// its own roster, so there is a real non-empty default (an empty
    /// roster) rather than "not constructed yet". See
    /// [`spectator_menu`]'s module doc.
    spectator_menu: spectator_menu::SpectatorMenuState,
    /// The command tree the connected server sent, pushed
    /// down by `app`'s right-click handler off `net::CommandTreeCell` — this
    /// module is pure and holds no client handle, so it cannot pull it.
    ///
    /// `None` off a live session, or before the server's `minecraft:commands`
    /// arrives, and every consumer treats that as "offer no completions"
    /// rather than as an empty tree. An `Arc` because a real 26.2 server's tree
    /// is ~2,000 nodes: this is a shared read, never a copy.
    command_tree: Option<std::sync::Arc<lodestone_model::command_tree::CommandTree>>,
    /// Whether the hosted world is currently published to LAN — pushed in every frame from
    /// `Sim::is_lan_published` by
    /// `app::session::drive_ui_from_session`, the same shape
    /// [`Self::command_tree`] is pushed in from a live session. This module
    /// is pure and holds no `Sim`, so it cannot poll the real state itself.
    ///
    /// **`false` off a hosted session too** — that is the wire ground truth
    /// (`NetUpdate::LanOpened` can only ever arrive from *our own* integrated
    /// server), not a claim that multiplayer's pause menu should therefore
    /// look unpublished. This field alone cannot tell "singleplayer, not yet
    /// published" from "multiplayer, publishing meaningless" apart — that is
    /// [`Self::has_singleplayer_server`]'s job, and [`Self::open_to_lan_available`]
    /// is the one place the two combine. See [`Self::pause_buttons`], one of
    /// its two readers.
    lan_published: bool,
    /// Vanilla's own client-instance has-singleplayer-server accessor — whether this session
    /// is an **integrated** server at all, pushed in from
    /// `UiState::kind() == Some(SessionKind::Singleplayer)` by
    /// `app::session::drive_ui_from_session`, next to [`Self::lan_published`]'s
    /// own push. `false` by default (and reset alongside every other session
    /// field), which is the safe direction: Open to LAN starts hidden and is
    /// only offered once a live singleplayer session confirms it, rather than
    /// risking a stale `true` surviving into a multiplayer join.
    ///
    /// **The bug this exists to fix**: before this field, [`Self::pause_buttons`]
    /// read only [`Self::lan_published`], which is `false` on *both* an
    /// unpublished singleplayer world and a remote multiplayer server — so a
    /// multiplayer session's pause menu showed Open to LAN, a button whose
    /// only possible outcome there is nonsensical (there is no local world of
    /// ours to open). See [`Self::open_to_lan_available`].
    has_singleplayer_server: bool,
}

impl Default for MenuNav {
    fn default() -> Self {
        Self::new()
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use super::super::servers::MAX_NAME_CHARS;

    /// A nav whose list persists to a unique temporary file, so tests exercise
    /// the *real* save path without touching the developer's server list.
    fn nav_path(tag: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lodestone-nav-{}-{tag}/servers.json",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        path
    }

    /// Writes a `profiles.json` beside `path` holding one account, so the
    /// ownership gate is **open** for the `MenuNav` about to be built from it.
    ///
    /// Must run before construction: `AccountsNav` reads the roster once, in its
    /// constructor.
    fn grant_ownership(path: &std::path::Path) {
        let mut meta = lodestone_auth::AccountsMetadata::default();
        let id = uuid::Uuid::new_v4();
        meta.upsert(lodestone_auth::AccountProfile {
            profile_id: id,
            username: "OwnerAccount".to_owned(),
            skin_url: None,
            last_used: 1,
        });
        meta.selected = Some(id);
        meta.save_to(&path.parent().unwrap().join("profiles.json"))
            .expect("the temp roster must be writable");
    }

    /// A `MenuNav` on a fresh temp directory **with an account that owns the
    /// game**, i.e. past the ownership gate.
    ///
    /// Seeding is deliberate and is the premise almost every test in this module
    /// wants — they are about what a player who can play sees. It does mean the
    /// whole corpus is blind to the gate by construction, which is why the gate
    /// has its own tests built on [`unowned_nav`] rather than on this.
    fn nav(tag: &str) -> (MenuNav, std::path::PathBuf) {
        let path = nav_path(tag);
        grant_ownership(&path);
        (MenuNav::with_path(path.clone()), path)
    }

    /// [`nav`]'s twin with **no** account: the ownership gate is closed.
    fn unowned_nav(tag: &str) -> (MenuNav, std::path::PathBuf) {
        let path = nav_path(tag);
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
        // `Accounts` is appended after `Quit` (see `MAIN_BUTTONS`'s docs) and
        // is enabled, so it — not `Quit` — is now the last stop wrapping up
        // from the top reaches.
        assert_eq!(nav.main_button(), MainButton::Accounts, "up from the top wraps");
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
        // Singleplayer opens the world list through the menu's world-list route —
        // where it used to return `MenuAction::Singleplayer` and launch directly.
        // There is no action for the app to take at *this* button; the launch is
        // Play Selected World, one screen in.
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert_eq!(ui.screen(), Screen::WorldSelect);
        ui.on_escape();
        assert_eq!(ui.screen(), Screen::MainMenu, "escape unwinds to the title");
        assert_eq!(
            nav.main_button(),
            MainButton::Singleplayer,
            "and leaves the highlight where it was"
        );

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

        // `Accounts` is the last button now (see `MAIN_BUTTONS`'s docs), so
        // wrapping `Up` from the top lands there rather than on `Quit` — see
        // `main_menu_selection_wraps_both_ways`. Walk to `Quit` directly
        // instead, exercising a plain `Up` from the top of the vanilla run.
        nav.key(&mut ui, MenuKey::Up);
        assert_eq!(nav.main_button(), MainButton::Singleplayer);
        nav.key(&mut ui, MenuKey::Up);
        assert_eq!(nav.main_button(), MainButton::Accounts, "up from the top wraps");
        nav.key(&mut ui, MenuKey::Up);
        assert_eq!(nav.main_button(), MainButton::Quit);
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
        assert_eq!(nav.form().name(), "Home", "the form pre-fills");
        assert_eq!(nav.form().address(), "mc.example.com:25566");
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
    fn clicking_the_resource_pack_row_cycles_and_persists_the_choice() {
        use crate::menu::servers::ServerPackPolicy;
        let (mut nav, path) = nav("packrow");
        let mut ui = UiState::new();
        ui.open_server_list();
        nav.key(&mut ui, MenuKey::Char('a'));
        assert_eq!(ui.screen(), Screen::ServerEdit);
        assert_eq!(
            nav.form().pack_status(),
            ServerPackPolicy::Prompt,
            "a new entry defaults to Prompt, matching a freshly added vanilla server"
        );

        // Enabled -> Disabled -> Prompt -> Enabled, vanilla's declaration order.
        assert_eq!(nav.click(&mut ui, RESOURCE_PACK_ROW), MenuAction::None);
        assert_eq!(nav.form().pack_status(), ServerPackPolicy::Enabled);
        assert_eq!(nav.click(&mut ui, RESOURCE_PACK_ROW), MenuAction::None);
        assert_eq!(nav.form().pack_status(), ServerPackPolicy::Disabled);
        assert_eq!(nav.click(&mut ui, RESOURCE_PACK_ROW), MenuAction::None);
        assert_eq!(nav.form().pack_status(), ServerPackPolicy::Prompt);
        assert_eq!(nav.click(&mut ui, RESOURCE_PACK_ROW), MenuAction::None);
        assert_eq!(nav.form().pack_status(), ServerPackPolicy::Enabled);

        // Save, and the choice must have travelled with the entry — through
        // `to_entry`, through `ServerList::to_json`, and back out of a fresh
        // `MenuNav` reading the same file, not merely out of the live one.
        type_str(&mut nav, &mut ui, "Home");
        nav.key(&mut ui, MenuKey::Tab);
        type_str(&mut nav, &mut ui, "mc.example.com");
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(ui.screen(), Screen::ServerList);
        assert_eq!(nav.list().get(0).unwrap().pack_status, ServerPackPolicy::Enabled);
        assert_eq!(
            MenuNav::with_path(path.clone())
                .list()
                .get(0)
                .unwrap()
                .pack_status,
            ServerPackPolicy::Enabled,
            "the policy must be on disk, not only in the live list"
        );

        // Re-opening the edit form for this entry seeds the cycle button from
        // the saved value, not from `Prompt`.
        nav.key(&mut ui, MenuKey::Char('e'));
        assert_eq!(ui.screen(), Screen::ServerEdit);
        assert_eq!(nav.form().pack_status(), ServerPackPolicy::Enabled);
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
        assert_eq!(nav.form().address(), "aaa.example");
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
            MenuAction::Connect(_, e) => {
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
        assert!(form.name().is_empty(), "control chars must not enter a field");

        for _ in 0..1000 {
            form.push('x');
        }
        assert_eq!(form.name().chars().count(), MAX_NAME_CHARS);
        form.next_field();
        for _ in 0..1000 {
            form.push('y');
        }
        assert_eq!(form.address().chars().count(), MAX_ADDRESS_CHARS);
    }

    /// The exact focused-field sequence, as a sequence — not a property.
    ///
    /// `CLAUDE.md`'s `ClientEvent::BiomeVisuals` precedent: an ordering change
    /// has to fail *here*, and no `cargo check` can see one. The wrap on the
    /// fourth press is the interesting entry, because it is vanilla's
    /// `clearFocus()`-then-retry and not `(i + 1) % n` —
    /// see `super::focus`.
    #[test]
    fn tab_walks_the_form_fields_in_order_and_wraps() {
        let mut form = EditForm::adding();
        assert_eq!(form.field(), FormField::Name, "setInitialFocus lands here");
        let seen: Vec<FormField> = (0..5)
            .map(|_| {
                form.handle_key(MenuKey::Tab);
                form.field()
            })
            .collect();
        assert_eq!(
            seen,
            vec![
                FormField::Address,
                FormField::Name,
                FormField::Address,
                FormField::Name,
                FormField::Address,
            ]
        );
        // Shift is not routed by `app.rs`, so backward Tab is not reachable from
        // the keyboard yet — but the mechanism is, and it is the same walk.
        form.next_field();
        assert_eq!(form.field(), FormField::Name);
    }

    /// The ordering at this screen is deliberate: the focused field
    /// is offered the key **first**, so the keys it wants never become
    /// navigation, and the keys it declines do.
    #[test]
    fn the_focused_field_swallows_its_keys_before_they_can_move_focus() {
        let mut form = EditForm::adding();
        for c in "abc".chars() {
            form.push(c);
        }
        assert_eq!(form.name(), "abc");
        assert_eq!(form.field(), FormField::Name);

        // Backspace and Delete are the field's; focus must not budge.
        assert_eq!(form.handle_key(MenuKey::Backspace), FormOutcome::Handled);
        assert_eq!((form.name(), form.field()), ("ab", FormField::Name));
        assert_eq!(form.handle_key(MenuKey::Delete), FormOutcome::Handled);
        assert_eq!(
            (form.name(), form.field()),
            ("ab", FormField::Name),
            "Delete at the end of the value is consumed and changes nothing — \
             it used to fall through to the screen and mean nothing at all"
        );

        // Down is not the field's — `EditBox.keyPressed` lists 264 in its
        // `default:` group — so it reaches navigation and moves focus.
        assert_eq!(form.handle_key(MenuKey::Down), FormOutcome::Handled);
        assert_eq!(form.field(), FormField::Address);
        assert_eq!(form.handle_key(MenuKey::Up), FormOutcome::Handled);
        assert_eq!(form.field(), FormField::Name);
        // And arrow navigation does not wrap, unlike Tab: Up from the top field
        // stays put. (The old form toggled on Up/Down, so this is a deliberate
        // behaviour change *toward* vanilla, not a regression.)
        assert_eq!(form.handle_key(MenuKey::Up), FormOutcome::Handled);
        assert_eq!(form.field(), FormField::Name);
        assert_eq!(form.handle_key(MenuKey::Tab), FormOutcome::Handled);
        assert_eq!(form.field(), FormField::Address, "Tab still moves");

        // Escape and Enter are the screen's, and Escape is answered *before* the
        // field — a text field must never be able to trap the player.
        assert_eq!(form.handle_key(MenuKey::Escape), FormOutcome::Cancel);
        assert_eq!(form.handle_key(MenuKey::Enter), FormOutcome::Save);

        // The horizontal arrows are the field's, and this is the half `app.rs`
        // does not produce yet: asserted through the focus layer directly so the
        // capability is proved rather than assumed. See
        // `focus::KeyEvent::from_menu_key`.
        let mut form = EditForm::adding();
        for c in "abc".chars() {
            form.push(c);
        }
        assert_eq!(form.fields.name.cursor_position(), 3);
        assert_eq!(
            form.focus
                .screen_key_pressed(&mut form.fields, KeyEvent::new(focus::KEY_LEFT)),
            focus::KeyOutcome::Consumed,
            "Left is the caret's, not the focus layer's"
        );
        assert_eq!(form.fields.name.cursor_position(), 2);
        assert_eq!(form.field(), FormField::Name, "and focus did not move");
    }

    /// `EditForm`'s focus ids and `menu::render`'s row indices are the same
    /// numbers, and `app.rs` reports a click as a row index — so if they ever
    /// diverge, clicking the address field would focus the name one. Same shape
    /// as `the_settings_rows_are_in_the_order_click_assumes`, and the same bug
    /// it guards against.
    #[test]
    fn the_form_field_ids_are_the_row_indices_the_mouse_reports() {
        let (mut nav, _) = nav("form-ids");
        let mut ui = UiState::new();
        ui.open_server_list();
        nav.key(&mut ui, MenuKey::Char('a'));
        let frame = crate::menu::render::frame_for(
            &ui,
            &nav,
            &crate::menu::status::StatusCache::new(),
            &mut crate::menu::render::FaviconCache::new(),
        )
        .expect("the edit form owns its frame");
        // Two fields plus the framework-conversion's three button rows —
        // Resource Packs, Done, Cancel.
        assert_eq!(frame.rows.len(), 5);
        assert_eq!(frame.rows[NAME_FIELD].detail, "Server Name");
        assert_eq!(frame.rows[ADDRESS_FIELD].detail, "Server Address");
        // A hover on row 1 must **not** focus the address field — a player
        // report (2026-08-04) caught pure mouse motion granting real keyboard
        // focus, which vanilla's `ContainerEventHandler` only ever does from a
        // click or Tab. `the_form_field_ids_are_the_row_indices_the_mouse_
        // reports`'s own name is about hover *hit-testing* landing on the
        // right row, which is still true — it is `hover_row`'s reaction to
        // that row that changed.
        nav.hover(&ui, ADDRESS_FIELD);
        assert_eq!(
            nav.form().field(),
            FormField::Name,
            "hovering a field must not move keyboard focus"
        );
        // The control: a real click on the same row *does* focus it —
        // `MenuNav::click`'s `ServerEdit` arm calls `focus_row` directly and is
        // unaffected by the `hover_row` fix above.
        nav.click(&mut ui, ADDRESS_FIELD);
        assert_eq!(nav.form().field(), FormField::Address, "a click must still focus");
        let frame = crate::menu::render::frame_for(
            &ui,
            &nav,
            &crate::menu::status::StatusCache::new(),
            &mut crate::menu::render::FaviconCache::new(),
        )
        .unwrap();
        assert_eq!(frame.selected, ADDRESS_FIELD);
        // Tab must still advance focus — the other legitimate way in, besides a
        // click.
        nav.key(&mut ui, MenuKey::Tab);
        assert_eq!(
            nav.form().field(),
            FormField::Name,
            "Tab from Address must advance (wrapping back to Name)"
        );
        // A hover on the Done row must **not** steal focus from the name
        // field — it is a different question, carried on `hovered` (the same
        // shape averted a second way: a button hover must not silently move
        // the caret).
        nav.hover(&ui, DONE_ROW);
        assert_eq!(
            nav.form().field(),
            FormField::Name,
            "hovering a button must not move text focus"
        );
        assert_eq!(nav.form().hovered_button(), Some(DONE_ROW));
        // Out of range does nothing rather than clamping onto a real field.
        nav.hover(&ui, 7);
        assert_eq!(nav.form().field(), FormField::Name);
    }

    #[test]
    fn the_edit_form_fields_carry_vanillas_real_narration_and_our_own_hint() {
        // vanilla's own manage-server screen rendering: `manageServer.enterName`/
        // `manageServer.enterIp` (`en_us.json`: "Server Name"/"Server
        // Address") as each field's own message — those narration strings are
        // kept. The name field's hint is Lodestone's own text,
        // shown while the field is empty and unfocused; the IP field never
        // gets a hint in the jar either, and we match that.
        let form = EditForm::adding();
        assert_eq!(form.fields.name.widget.message, "Server Name");
        assert_eq!(form.fields.address.widget.message, "Server Address");
        assert_eq!(form.fields.name.hint.as_deref(), Some("My Server"));
        assert_eq!(
            form.fields.address.hint, None,
            "vanilla's ipEdit never gets a setHint call"
        );
    }

    /// Clicking a field must **focus** it, not activate the screen.
    ///
    /// `MenuNav::click` translates a click into `hover` + `Enter` for every screen
    /// that has a row cursor, and on this screen `Enter` means *save* — so
    /// clicking either field submitted the form. That is the same shape one
    /// screen over, and the same dispatch is what makes it visible:
    /// `ContainerEventHandler.mouseClicked` focuses the child it hit and calls its
    /// `onClick`; it never activates the screen.
    #[test]
    fn clicking_a_form_field_focuses_it_instead_of_saving() {
        let (mut nav, _) = nav("form-click");
        let mut ui = UiState::new();
        ui.open_server_list();
        nav.key(&mut ui, MenuKey::Char('a'));
        for c in "play.example".chars() {
            nav.key(&mut ui, MenuKey::Char(c));
        }
        nav.key(&mut ui, MenuKey::Tab);
        for c in "play.example".chars() {
            nav.key(&mut ui, MenuKey::Char(c));
        }
        // Premise: the form *is* saveable, so a stray `Enter` would really have
        // closed it — without this the test would pass on an invalid form for the
        // wrong reason.
        assert!(nav.form().is_valid());
        assert_eq!(
            nav.click(&mut ui, NAME_FIELD),
            MenuAction::None,
            "a click on a field must not produce a save action"
        );
        assert_eq!(
            ui.screen(),
            Screen::ServerEdit,
            "and must not close the form the player is still typing into"
        );
        assert_eq!(nav.form().field(), FormField::Name, "it focuses the field");
        assert!(nav.list().is_empty(), "and saves nothing");
        // The control: `Enter` on the same form *does* save, so the assertions
        // above are about the click and not about an unsaveable form.
        assert!(matches!(
            nav.key(&mut ui, MenuKey::Enter),
            MenuAction::Reprobe(_)
        ));
        assert_eq!(ui.screen(), Screen::ServerList);
        assert_eq!(nav.list().len(), 1);
    }

    /// The seed geometry and the draw geometry must be the same rects, because
    /// arrow navigation between the fields is *geometric*: a seed with both boxes
    /// at `(0, 0)` would make Up/Down silently stop working while every unit test
    /// that drives `next_field` still passed.
    #[test]
    fn the_seeded_field_geometry_is_the_layout_the_draw_uses() {
        let form = EditForm::adding();
        let [name_rect, address_rect] =
            crate::menu::render::field_row_rects(SEED_CANVAS.0, SEED_CANVAS.1);
        assert_eq!(
            (
                form.fields.name.widget.x,
                form.fields.name.widget.y,
                form.fields.name.widget.width,
                form.fields.name.widget.height
            ),
            name_rect
        );
        assert_eq!(
            (
                form.fields.address.widget.x,
                form.fields.address.widget.y,
                form.fields.address.widget.width,
                form.fields.address.widget.height
            ),
            address_rect
        );
        // The premise arrow navigation rests on: the address field is *below* the
        // name field and they share a column. Both halves matter — the strict
        // pass in `focus` requires the orthogonal overlap.
        assert!(
            address_rect.1 > name_rect.1 + name_rect.3,
            "the address field must be strictly below the name field: \
             {name_rect:?} then {address_rect:?}"
        );
        assert_eq!(address_rect.0, name_rect.0, "and in the same column");
        assert!(name_rect.2 > 0.0 && name_rect.3 > 0.0, "with a real size");
        // And the width the boxes scroll against is the drawn width, so a long
        // address does not scroll half a field early.
        assert!(
            form.fields.address.inner_width() > 0.0,
            "an inner width of zero makes every character invisible"
        );
    }

    #[test]
    fn a_save_failure_is_reported_rather_than_swallowed() {
        // A player who adds a server and sees it vanish deserves the reason.
        // `/dev/null/...` cannot be a directory on any Unix.
        // The roster lives on a *writable* temp path while the server list does
        // not: this test is about a failed server-list write, and an unwritable
        // roster would additionally close the ownership gate, which would stop
        // the keystrokes below ever reaching the edit form.
        let profiles = nav_path("savefail-profiles");
        grant_ownership(&profiles);
        let mut nav = MenuNav::with_paths(
            std::path::PathBuf::from("/dev/null/nope/servers.json"),
            std::path::PathBuf::from("/dev/null/nope/options.json"),
            profiles.parent().unwrap().join("profiles.json"),
        );
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
        // Singleplayer, Multiplayer, Language, Accessibility, Options, Quit,
        // in that order — inserting Options must not disturb Multiplayer's
        // index (existing wrap tests rely on it staying at 1) or Quit's
        // position as the last vanilla button. Language/Accessibility now sit
        // between Multiplayer and Options in the walk (see
        // `MainButton::Language`/`::Accessibility`'s own docs for why they
        // joined the enabled set) — this used to skip straight from
        // Multiplayer to Options in one `Down`.
        assert_eq!(nav.main_button(), MainButton::Singleplayer);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.main_button(), MainButton::Multiplayer);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.main_button(), MainButton::Language);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.main_button(), MainButton::Accessibility);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.main_button(), MainButton::Options);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.main_button(), MainButton::Quit);

        nav.key(&mut ui, MenuKey::Up);
        assert_eq!(nav.main_button(), MainButton::Options);
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert_eq!(ui.screen(), Screen::Settings);
    }

    /// The title screen's Language/Accessibility icons (`MainButton::Language`/
    /// `::Accessibility`) must each open `Screen::Settings` on their own page.
    /// The empty page stack supplied by `SettingsNav::open_at` makes Escape or
    /// Done return directly to the title instead of traversing the root grid.
    /// **Quit Game** is present-and-greyed in a browser tab and live
    /// everywhere else, and the whole row set is otherwise identical between
    /// the two hosts.
    ///
    /// Both arms are driven through [`MainButton::enabled_on`] rather than
    /// `enabled()`, because the native suite is the only suite: an inline
    /// `cfg!` would make the browser's answer unobservable here, and the arm
    /// nobody can run is the arm that rots. The assertion is on a **collected**
    /// difference set, not one `assert!` per row inside the loop — a loop-body
    /// assert aborts on the first mismatch, so a neuter proves exactly one row
    /// and leaves the rest as arguments rather than observations.
    #[test]
    fn quit_game_is_the_only_row_a_browser_disables() {
        let differing: Vec<MainButton> = MAIN_BUTTONS
            .iter()
            .copied()
            .filter(|b| b.enabled_on(true) != b.enabled_on(false))
            .collect();
        assert_eq!(
            differing,
            vec![MainButton::Quit],
            "exactly one row may depend on whether a process can be ended"
        );

        assert!(
            MainButton::Quit.enabled_on(true),
            "a native build must be able to quit"
        );
        assert!(
            !MainButton::Quit.enabled_on(false),
            "a browser tab has no process to end, so the row must be greyed \
             rather than latching a quit that only stops the event loop"
        );
        // Present, not removed: a button missing from its vanilla position is a
        // layout that reads wrong. This is what separates "disabled" from
        // "absent", and it is the half a `retain`-shaped fix would break.
        assert!(
            MAIN_BUTTONS.contains(&MainButton::Quit),
            "the row must still occupy its vanilla slot on every host"
        );
    }

    #[cfg(not(feature = "multiplayer"))]
    #[test]
    fn singleplayer_only_build_keeps_multiplayer_visible_but_disabled_with_an_explanation() {
        assert!(
            MAIN_BUTTONS.contains(&MainButton::Multiplayer),
            "the disabled control must retain its title-screen slot"
        );
        assert!(
            !MainButton::Multiplayer.enabled(),
            "a build without the multiplayer feature must not open the server list"
        );
        assert_eq!(
            MainButton::Multiplayer.tooltip(),
            Some("Multiplayer is disabled in this build of the game."),
            "hovering the disabled control must explain the build capability"
        );

        let (mut nav, _) = nav("singleplayer-only-entry");
        let mut ui = UiState::new();
        assert!(
            !nav.ownership_gate_blocks(&ui),
            "a build with no remote capability must not require an online account"
        );
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert_eq!(
            ui.screen(),
            Screen::WorldSelect,
            "the default Singleplayer button must reach the local world flow"
        );
    }

    #[test]
    fn language_and_accessibility_icons_open_their_page_directly_and_escape_is_one_step() {
        use crate::menu::options::SettingsPage;

        for (button, page) in [
            (MainButton::Language, SettingsPage::Language),
            (MainButton::Accessibility, SettingsPage::Accessibility),
        ] {
            let (mut nav, _) = nav("title-icon");
            let mut ui = UiState::new();
            assert!(button.enabled(), "{button:?} must be enabled");
            while nav.main_button() != button {
                nav.key(&mut ui, MenuKey::Down);
            }
            assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
            assert_eq!(ui.screen(), Screen::Settings, "{button:?} must open Settings");
            assert_eq!(
                nav.settings().page(),
                page,
                "{button:?} must land directly on its own page, not Root"
            );

            // One Escape, straight back to the title — never surfacing the
            // root grid first, which an empty page stack is what prevents.
            assert_eq!(nav.key(&mut ui, MenuKey::Escape), MenuAction::None);
            assert_eq!(
                ui.screen(),
                Screen::MainMenu,
                "{button:?}: Escape from a directly-opened page must leave \
                 Settings entirely in one step, matching vanilla's \
                 `lastScreen = this` — landing back on Root instead means the \
                 page stack was not empty"
            );
        }
    }

    /// Drives the settings cursor onto the control `pred` picks out, using only
    /// keys a player has, and returns its **visible row index** — the number
    /// `app.rs`'s hit-test reports for that row.
    ///
    /// Deliberately not a shortcut into [`crate::menu::options::SettingsNav`]'s
    /// private fields: reaching a row by pressing Down is what proves the row is
    /// reachable, which is the property this navigation test protects (117 of 135 controls
    /// are inactive, so a cursor that skipped them would leave most of the tree
    /// invisible).
    fn settings_row(
        nav: &mut MenuNav,
        ui: &mut UiState,
        pred: impl Fn(&crate::menu::options::Cell) -> bool,
    ) -> usize {
        let page = nav.settings().page();
        // `nav` was opened via `self::nav(...)` + `ui.open_settings()` in these
        // tests, never through `MainButton::Options`/`PauseButton::Options`, so
        // `SettingsNav::in_world` is still `new()`'s default (`false`) — match
        // it here rather than hand the census the wrong Root(2) cell.
        let controls = crate::menu::options::all_controls(page, false);
        let target = controls
            .iter()
            .position(|c| pred(c))
            .expect("no such control on this page");
        for _ in 0..=controls.len() {
            if nav.settings().cursor() == target {
                break;
            }
            nav.key(ui, MenuKey::Down);
        }
        assert_eq!(
            nav.settings().cursor(),
            target,
            "Down must reach every control on {page:?}"
        );
        nav.settings()
            .selected_row()
            .expect("the cursor must be inside the visible window")
    }

    /// Walks the root page's nav button for `page` and enters it.
    fn open_settings_page(
        nav: &mut MenuNav,
        ui: &mut UiState,
        page: crate::menu::options::SettingsPage,
    ) {
        use crate::menu::options::Cell;
        settings_row(nav, ui, |c| {
            matches!(c, Cell::Nav { page: Some(p), .. } if *p == page)
        });
        nav.key(ui, MenuKey::Enter);
        assert_eq!(nav.settings().page(), page);
    }

    #[test]
    fn online_allow_requests_route_reaches_the_account_scoped_friends_settings() {
        let (mut nav, _path) = nav("online-friends-settings");
        let mut ui = UiState::new();
        ui.open_settings();
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Online);
        settings_row(&mut nav, &mut ui, |cell| {
            matches!(
                cell,
                crate::menu::options::Cell::Act {
                    act: crate::menu::options::Action::OpenFriendsSettings,
                    ..
                }
            )
        });

        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert_eq!(ui.screen(), Screen::Friends);
        assert_eq!(nav.friends().tab(), crate::menu::friends::FriendsTab::Settings);
    }

    /// Matches the `OptionInstance` whose vanilla's own persisted-options declarations accessor is `name`.
    fn is_option(name: &str) -> impl Fn(&crate::menu::options::Cell) -> bool + '_ {
        move |c| matches!(c, crate::menu::options::Cell::Option(s) if s.accessor == name)
    }

    /// [`settings_row`]'s counterpart for the Key Binds screen:
    /// drives `KeyBindsNav`'s own cursor with nothing but Down, the same
    /// "reaching it this way proves it is reachable" reasoning that method's
    /// own doc gives. `nav.key` already routes to the right cursor by itself
    /// once `SettingsNav::page()` is `KeyBinds` — see `key_settings` guard
    /// — so this needs no separate key-sending path.
    fn key_binds_row(
        nav: &mut MenuNav,
        ui: &mut UiState,
        pred: impl Fn(&crate::menu::key_binds::KeyControl) -> bool,
    ) -> usize {
        let controls = crate::menu::key_binds::all_controls();
        let target = controls
            .iter()
            .position(|c| pred(c))
            .expect("no such control on the Key Binds screen");
        for _ in 0..=controls.len() {
            if nav.settings().key_binds().cursor() == target {
                break;
            }
            nav.key(ui, MenuKey::Down);
        }
        assert_eq!(
            nav.settings().key_binds().cursor(),
            target,
            "Down must reach every control on Key Binds"
        );
        nav.settings()
            .key_binds()
            .selected_row()
            .expect("the cursor must be inside the visible window")
    }

    #[test]
    fn enter_on_the_gui_scale_row_cycles_it_and_persists_through_a_real_file() {
        // This is the re-pointed `settings_up_down_cycles_the_gui_scale…`. The
        // *behaviour* it protected — a scale that cycles, wraps and reaches
        // `options.json` immediately rather than at exit — is unchanged; what
        // moved is the key. Up/Down are a cursor now, and the cycle is Enter on
        // the row, which is `CycleButton.onPress`.
        let (mut nav, path) = nav("settings-cycle");
        let mut ui = UiState::new();
        ui.open_settings();
        assert_eq!(nav.gui_scale(), 0, "starts at auto");

        // GUI Scale lives on the Video screen in vanilla, under the Display
        // header — not on the root, which is why this walks two levels.
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Video);
        settings_row(&mut nav, &mut ui, is_option("guiScale"));

        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert_eq!(nav.gui_scale(), 1);
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(nav.gui_scale(), 2);
        assert_eq!(nav.options_save_error(), None);

        // It is on disk *now*, not at exit.
        let options_path = path.parent().unwrap().join("options.json");
        assert_eq!(
            crate::config::Options::load_from(&options_path).gui_scale,
            2
        );

        // And it is a *cycle*, not a clamp: counting up to the ceiling and then
        // pressing once more lands back on auto. Six more presses from 2 reaches
        // `MAX_MANUAL_GUI_SCALE`, so the range is exclusive — an inclusive one
        // would overshoot by one and land on auto here instead of below.
        for _ in 2..crate::config::MAX_MANUAL_GUI_SCALE {
            nav.key(&mut ui, MenuKey::Enter);
        }
        assert_eq!(
            nav.gui_scale(),
            crate::config::MAX_MANUAL_GUI_SCALE,
            "counted up to the ceiling"
        );
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(nav.gui_scale(), 0, "and wraps back to auto");
        assert_eq!(
            ui.screen(),
            Screen::Settings,
            "cycling an option must not leave the screen"
        );
    }

    #[test]
    fn enter_on_the_view_bobbing_row_toggles_it_and_touches_nothing_else() {
        // Vanilla puts `bobView` on the **Accessibility** screen in 26.2, not on
        // Video — which is worth asserting, because "View Bobbing is a video
        // setting" is the intuitive and wrong answer.
        let (mut nav, path) = nav("settings-view-bobbing");
        let mut ui = UiState::new();
        ui.open_settings();
        let options_path = path.parent().unwrap().join("options.json");

        open_settings_page(
            &mut nav,
            &mut ui,
            crate::menu::options::SettingsPage::Accessibility,
        );
        settings_row(&mut nav, &mut ui, is_option("bobView"));

        assert!(nav.view_bobbing(), "vanilla's default is ON");
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert!(!nav.view_bobbing());
        // On disk immediately, same rule as the scale.
        assert!(!crate::config::Options::load_from(&options_path).view_bobbing);

        nav.key(&mut ui, MenuKey::Enter);
        assert!(nav.view_bobbing(), "Enter is a toggle, not a latch");
        assert!(crate::config::Options::load_from(&options_path).view_bobbing);
        assert_eq!(nav.gui_scale(), 0, "and must not reach the other live option");

        // The control that matters now that the cursor is shared: moving it onto
        // the *neighbouring* row and pressing Enter must not toggle the bob. Its
        // left-hand neighbour is `notificationDisplayTime`, which we do not
        // honour, so Enter there is a no-op.
        settings_row(&mut nav, &mut ui, is_option("notificationDisplayTime"));
        nav.key(&mut ui, MenuKey::Enter);
        assert!(
            nav.view_bobbing(),
            "Enter on an inactive row must do nothing at all"
        );
        assert_eq!(nav.options_save_error(), None);
    }

    /// The chat options' **consumed effect**, end to end: clicking the Width
    /// row on the Chat screen must move the pixel width of the chat box the HUD
    /// actually draws, to a number predicted from vanilla's own algebra.
    ///
    /// This is deliberately not a read-back of what was written. Before this
    /// wiring the eight chat fields were persisted, `app.rs` already copied
    /// them into `hud_frame.chat_options`, and `hud.rs` already had magnitude
    /// gates proving the draw honours them — and the rows were drawn **greyed**,
    /// so the whole chain reached zero pixels for want of a control. A test that
    /// asserted `options.chat_width == 0.1` would have passed on that dead
    /// version just as well; only driving the real widget and then measuring the
    /// real geometry can tell the difference.
    ///
    /// The predicted numbers come from outside this client:
    /// vanilla's own chat-component rendering's get-width accessor: `floor(pct * 280 + 40)`
    ///, so `1.0` is 320px and `0.0` is 40px, and
    /// `step_unit_double` wraps `1.0` straight to `0.0` — a 280px move on the
    /// very first click, which no rounding could fake.
    #[test]
    fn clicking_the_chat_width_row_resizes_the_chat_box_the_hud_draws() {
        use crate::hud::{ChatDisplayOptions, DebugStats, HudFrame, HudGeometry};

        // `logical_canvas(AUTO_GUI_SCALE, 640, 480) == (320, 240)`, so the
        // logical canvas is 320px wide and `b.w == 320` — the same canvas the
        // `hud.rs` width gate uses, and the reason a 320px box exactly fills it.
        const CANVAS_W: f32 = 320.0;
        let stats = DebugStats::default();
        let chat = [("hi", 0.0_f32)];

        // Vanilla's own `getWidth`, recomputed here from the published formula
        // rather than by calling this client's `chat_width_px` — so a shared
        // bug in that helper could not cancel itself out.
        let expect_px = |pct: f32| (f64::from(pct) * 280.0 + 40.0).floor() as f32;
        // The box width the HUD really draws, read back out of the vertex
        // buffer: row 0's background starts at `x == 0`, so its second vertex's
        // NDC x is `2 * w / b.w - 1` (`verts[6]`, as the `hud.rs` gate does).
        //
        // That rect is the chat *plate*, which is wider than the text column by
        // a fixed padding (`hud::CHAT_PLATE_PAD_PX`, scaled by the chat pose
        // scale) so a full-width wrapped line cannot overhang its own
        // background. Subtracting it here recovers the column width this gate
        // is actually about; the `pct * 280 + 40` slope above stays derived
        // independently of `chat_width_px`.
        let plate_pad =
            crate::hud::CHAT_PLATE_PAD_PX * crate::hud::chat_pose_scale(ChatDisplayOptions::default());
        let drawn_px = |opts: &Options| {
            let geo = HudGeometry::build(
                &HudFrame {
                    crosshair: false,
                    show_debug: false,
                    chat: &chat,
                    chat_options: ChatDisplayOptions {
                        width_pct: opts.chat_width,
                        ..ChatDisplayOptions::default()
                    },
                    ..HudFrame::new(&stats)
                },
                640,
                480,
            );
            (geo.verts[6] + 1.0) * CANVAS_W / 2.0 - plate_pad
        };

        let (mut nav, _path) = self::nav("settings-chat-width-consumed");
        let mut ui = UiState::new();
        ui.open_settings();
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Chat);
        let row = settings_row(&mut nav, &mut ui, is_option("chatWidth"));

        // Precondition, stated as a real assertion: vanilla's default is 1.0,
        // i.e. a box that fills the 320px canvas.
        assert_eq!(nav.options().chat_width, 1.0);
        assert!(
            (drawn_px(nav.options()) - 320.0).abs() < 1e-3,
            "premise: the default must draw a 320px box, got {}",
            drawn_px(nav.options())
        );

        // Click 1: `1.0` steps past the top and wraps to `0.0` → 40px.
        // A 280px collapse is far outside any tolerance.
        assert_eq!(nav.click(&mut ui, row), MenuAction::None);
        assert_eq!(nav.options().chat_width, 0.0, "1.0 + 0.1 wraps to 0.0");
        assert!(
            (drawn_px(nav.options()) - expect_px(0.0)).abs() < 1e-3,
            "expected {}px, the HUD drew {}px",
            expect_px(0.0),
            drawn_px(nav.options())
        );

        // Clicks 2 and 3: 0.1 → 68px and 0.2 → 96px. `floor` makes these exact
        // integers, so a predicate that merely checked "it went up" would pass
        // on a wrong slope while these do not.
        for (clicks, expected_pct, expected_px) in [(2, 0.1_f32, 68.0_f32), (3, 0.2, 96.0)] {
            assert_eq!(nav.click(&mut ui, row), MenuAction::None);
            let got = nav.options().chat_width;
            assert!(
                (got - expected_pct).abs() < 1e-6,
                "click {clicks}: expected pct {expected_pct}, got {got}"
            );
            assert!(
                (expect_px(expected_pct) - expected_px).abs() < 1e-6,
                "the prediction itself must match vanilla's formula"
            );
            assert!(
                (drawn_px(nav.options()) - expected_px).abs() < 1e-3,
                "click {clicks}: expected a {expected_px}px box, the HUD drew {}px",
                drawn_px(nav.options())
            );
        }

        // The control that makes the positive assertions mean something: the row
        // *beside* Width is `chatDelay`, which this client does not honour, so
        // clicking it must leave the drawn box exactly where it is. Without this,
        // "clicking Width changed the geometry" would pass on an implementation
        // that moved the width on any click at all.
        let before = drawn_px(nav.options());
        let inert = settings_row(&mut nav, &mut ui, is_option("chatDelay"));
        assert_ne!(inert, row, "premise: they are different rows");
        assert_eq!(nav.click(&mut ui, inert), MenuAction::None);
        assert!(
            (drawn_px(nav.options()) - before).abs() < 1e-6,
            "an inactive neighbour must not resize the chat box"
        );
        assert_eq!(nav.options_save_error(), None);
    }

    /// The anti-island control for every chat option: `app/redraw.rs` must still
    /// copy all eight fields out of `nav.options()` into
    /// `hud_frame.chat_options`.
    ///
    /// The gate above drives the real widget and measures the real
    /// `HudGeometry`, but it builds its `ChatDisplayOptions` itself — because
    /// `app.rs` is the frame loop and a unit test cannot run it. So if that one
    /// copy were deleted, the gate above would still pass while every chat
    /// option silently stopped reaching the screen. That is precisely this
    /// repo's dominant defect, and the seam is one grep wide, so it is checked
    /// here by reading the source.
    ///
    /// This asserts the **field reads**, not a line number, so ordinary edits to
    /// `app.rs` do not disturb it. If the copy legitimately moves elsewhere,
    /// point this at the new home rather than deleting it.
    #[test]
    fn app_rs_still_threads_every_chat_option_into_the_hud_frame() {
        let src = include_str!("../../app/redraw.rs");
        assert!(
            src.contains("hud_frame.chat_options"),
            "app/redraw.rs must still populate `hud_frame.chat_options`"
        );
        for field in [
            "chat_scale",
            "chat_width",
            "chat_height_unfocused",
            "chat_height_focused",
            "chat_line_spacing",
            "chat_opacity",
            "chat_background_opacity",
            "chat_colors",
        ] {
            assert!(
                src.contains(&format!("chat_opts.{field}")),
                "app/redraw.rs no longer reads `chat_opts.{field}` — the settings row for \
                 it is now an island, and no other test in this crate can see that"
            );
        }
        // The control: the detector must be able to report an absence. A field
        // that does not exist must fail the same `contains` check, so a typo in
        // the list above cannot make this vacuously green.
        assert!(
            !src.contains("chat_opts.chat_nonexistent_field"),
            "the detector must not match a field that is not there"
        );
    }

    /// **Damage Tilt is a working control, and the tilt it produces is
    /// predicted.**
    ///
    /// This option was the chat batch's exact inverse and worse: the field was
    /// persisted *and* `app/redraw.rs` already fed
    /// `MenuNav::damage_tilt_strength` to `RenderState::set_damage_tilt_strength`
    /// every frame, so the whole camera-tilt consumer was honoured — and the row
    /// drew from `UNIT_DOUBLE_DEFAULTS`' frozen `1.0`, so the only way to reach it
    /// was to hand-edit `options.json`. Links 1 and 5 present, 2–4 missing.
    ///
    /// **The expected values come from vanilla's formula, evaluated outside
    /// `BobFrame`.** `GameRenderer.bobHurt` is
    /// `-sin((hurt/duration)^4 * PI) * 14 * strength`, so at `hurt == 5` of a
    /// 10-tick window the shaped term is `sin(0.5^4 * PI) = sin(PI/16)` and the
    /// tilt is `-14 * sin(PI/16) * strength`. That is recomputed here from
    /// `HURT_DURATION_TICKS` and the literal 14, not read back out of
    /// `hurt_roll_degrees`.
    ///
    /// **`0.0` is the discriminating input, not a round number.** Clicking 1.0
    /// wraps to 0.0 (`step_unit_double`'s documented wrap), and the accessibility
    /// contract is that `0.0` genuinely *disables* the tilt rather than shrinking
    /// it — so this asserts exactly zero, which a "scale it down a bit"
    /// implementation fails. The second click lands on 0.1, where the correct
    /// hypothesis (`-14 sin(PI/16) * 0.1`) and the wrong one (the frozen table
    /// default `1.0`) differ by a factor of ten.
    #[test]
    fn the_damage_tilt_row_moves_the_option_and_the_tilt_it_produces() {
        use crate::camera_rig::{BobFrame, HURT_DURATION_TICKS};

        // Vanilla's magnitude, computed here rather than asked of the subject.
        let expected_tilt = |strength: f32| -> f32 {
            let t = 5.0 / HURT_DURATION_TICKS;
            -(t * t * t * t * std::f32::consts::PI).sin() * 14.0 * strength
        };
        let measured = |strength: f32| -> f32 {
            BobFrame {
                walk_phase: 0.0,
                bob: 0.0,
                hurt: 5.0,
                hurt_dir_degrees: 0.0,
                death_time: 0.0,
            }
            .hurt_roll_degrees(strength)
        };

        let (mut nav, path) = self::nav("settings-damage-tilt");
        let mut ui = UiState::new();
        ui.open_settings();
        open_settings_page(
            &mut nav,
            &mut ui,
            crate::menu::options::SettingsPage::Accessibility,
        );
        let row = settings_row(&mut nav, &mut ui, is_option("damageTiltStrength"));

        assert_eq!(
            nav.damage_tilt_strength(),
            1.0,
            "premise: vanilla's default is a full-strength tilt"
        );
        assert!(
            (measured(1.0) - expected_tilt(1.0)).abs() < 1e-5,
            "premise: the consumer must already match vanilla's formula at the \
             default — expected {}, got {}",
            expected_tilt(1.0),
            measured(1.0)
        );
        assert!(
            measured(1.0).abs() > 2.0,
            "premise: the default tilt must be large enough that zero is \
             distinguishable from it; it is {} degrees",
            measured(1.0)
        );

        // Click 1: 1.0 steps past the top and wraps to 0.0 — the accessibility
        // value, which must switch the tilt off completely.
        assert_eq!(nav.click(&mut ui, row), MenuAction::None);
        assert_eq!(nav.damage_tilt_strength(), 0.0, "1.0 + 0.1 wraps to 0.0");
        assert_eq!(
            measured(0.0), 0.0,
            "a strength of 0.0 must produce exactly no tilt, not a small one — \
             that is the accessibility contract"
        );

        // Click 2: 0.1. The correct and the frozen-default hypotheses differ by
        // 10x here, so this is a magnitude assertion rather than a direction one.
        assert_eq!(nav.click(&mut ui, row), MenuAction::None);
        let got = nav.damage_tilt_strength();
        assert!((got - 0.1).abs() < 1e-6, "expected 0.1, got {got}");
        assert!(
            (measured(0.1) - expected_tilt(0.1)).abs() < 1e-5,
            "expected {} degrees, the consumer produced {}",
            expected_tilt(0.1),
            measured(0.1)
        );
        assert!(
            (measured(0.1) - expected_tilt(1.0)).abs() > 1.0,
            "the wrong hypothesis (the frozen table default 1.0) must be far from \
             the measurement, or this passes either way"
        );

        // The label is vanilla's `percentValueOrOffLabel`, not the plain percent
        // its neighbours use, so OFF at zero and a percentage above it. The two
        // stringifiers differ **only** at zero, which is why that value is pinned.
        assert_eq!(
            crate::menu::options::live_value(
                crate::menu::options::LiveOption::DamageTiltStrength,
                nav.options()
            ),
            "10%"
        );
        let mut off = *nav.options();
        off.damage_tilt_strength = 0.0;
        assert_eq!(
            crate::menu::options::live_value(
                crate::menu::options::LiveOption::DamageTiltStrength,
                &off
            ),
            "OFF",
            "percentValueOrOffLabel prints OFF at zero; the plain percent \
             transcription its neighbours use would print 0%"
        );

        // Persisted eagerly, through a real file, like every other row here.
        let options_path = path.parent().unwrap().join("options.json");
        let saved = std::fs::read_to_string(&options_path).expect("options.json must exist");
        assert!(
            saved.contains("damage_tilt_strength"),
            "the value must reach disk on the click, not at exit: {saved}"
        );
        assert_eq!(nav.options_save_error(), None);
    }

    /// The eleven volume rows each write **their own** bus, at eleven distinct
    /// values.
    ///
    /// Distinct values rather than one repeated, because the failure an
    /// eleven-wide indexed array invites is a **transposed pair** — two rows
    /// wired to each other's slot — and a uniform value cannot see it: every
    /// assertion passes while two categories swap. The values are `(i + 1) / 16`,
    /// dyadic so the `f32` comparison is exact.
    ///
    /// Drives the **drag** path (`AbstractSliderButton.setValueFromMouse`), which
    /// is the one that reaches `LiveOption::unit_double_mut`'s index write with a
    /// value of the test's choosing; the click path is exercised separately at the
    /// end, because it goes through a different arm
    /// (`apply_settings` → `step_sound_volume`).
    #[test]
    fn each_volume_row_writes_its_own_bus_and_no_other() {
        let (mut nav, path) = self::nav("settings-sound-volumes");
        let mut ui = UiState::new();
        ui.open_settings();
        let options_path = path.parent().unwrap().join("options.json");

        assert_eq!(
            nav.options().sound_volumes,
            [1.0; 11],
            "premise: vanilla ships every bus at full volume"
        );

        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Sound);

        // The eleven target values, and the eleven accessors they belong to. The
        // accessor order is `config::SOUND_CATEGORY_NAMES`, i.e. `SoundSource`
        // declaration order, which is the outside source for the whole mapping.
        let mut expected = [0.0f32; 11];
        for (index, name) in crate::config::SOUND_CATEGORY_NAMES.iter().enumerate() {
            let accessor = format!("soundSource.{name}");
            let row = settings_row(&mut nav, &mut ui, is_option(&accessor));
            let value = (index as f32 + 1.0) / 16.0;
            assert!(
                nav.drag_slider(&ui, row, value),
                "{accessor} must be a draggable live slider"
            );
            expected[index] = value;
        }
        assert_eq!(
            nav.options().sound_volumes,
            expected,
            "each row must land on its own bus — a mismatch here is a transposed \
             pair, which a uniform test value cannot detect"
        );
        // The control for that assertion: the eleven values really are distinct,
        // so a swap would have to change the array. An `expected` full of one
        // number would make the check above vacuous.
        let mut distinct = expected;
        distinct.sort_unstable_by(f32::total_cmp);
        for pair in distinct.windows(2) {
            assert_ne!(pair[0], pair[1], "the eleven test values must be distinct");
        }

        // Now the click path, which is a different arm. Master is at 1/16 =
        // 0.0625, so one step of `UNIT_DOUBLE_STEP` lands on 0.1625 — not a round
        // number, and not reachable by any wrap.
        let master = settings_row(&mut nav, &mut ui, is_option("soundSource.master"));
        assert_eq!(nav.click(&mut ui, master), MenuAction::None);
        let got = nav.options().sound_volumes[0];
        assert!(
            (got - 0.1625).abs() < 1e-6,
            "expected 0.0625 + 0.1 = 0.1625, got {got}"
        );
        assert_eq!(
            nav.options().sound_volumes[1..],
            expected[1..],
            "clicking Master must not disturb the other ten buses"
        );

        // Persisted eagerly, one key per bus, under vanilla's **singular**
        // sound-category name spellings.
        let saved = std::fs::read_to_string(&options_path).expect("options.json must exist");
        for name in crate::config::SOUND_CATEGORY_NAMES {
            assert!(
                saved.contains(&format!("sound_volume_{name}")),
                "sound_volume_{name} must reach disk on the drag, not at exit: {saved}"
            );
        }
        assert!(
            !saved.contains("sound_volume_records"),
            "the file keys are singular (`record`), not the plural enum variant \
             names — the detector would match either, so this pins which"
        );
        let reloaded = crate::config::Options::load_from(&options_path);
        assert_eq!(
            reloaded.sound_volumes[1..],
            expected[1..],
            "and survive a reload in the same slots"
        );
        assert_eq!(nav.options_save_error(), None);
    }

    /// The root page's FOV row moves `fov`, wraps at 110, and its drag lands on
    /// vanilla's own bucket.
    ///
    /// **Every input here is a non-default**, and for a specific reason:
    /// `camera_rig::FOV_Y_DEGREES` *is* vanilla's 70, so at the default the
    /// "reads the option" and "still pinned to the constant" hypotheses are
    /// byte-identical and a gate there measures only that the code runs.
    #[test]
    fn the_root_fov_row_moves_the_option_and_wraps_at_the_maximum() {
        use crate::config::{DEFAULT_FOV, MAX_FOV, MIN_FOV};

        let (mut nav, path) = self::nav("settings-fov");
        let mut ui = UiState::new();
        ui.open_settings();
        let options_path = path.parent().unwrap().join("options.json");

        assert_eq!(
            nav.settings().page(),
            crate::menu::options::SettingsPage::Root,
            "premise: FOV lives on the root page's own header, not in a list"
        );
        assert_eq!(nav.options().fov, DEFAULT_FOV, "premise: vanilla's 70");

        // One click is one degree, and it must be 71 rather than a wrap or a
        // no-op: the row used to be inactive, so "nothing happened" is the
        // failure this is here to exclude.
        let row = settings_row(&mut nav, &mut ui, is_option("fov"));
        assert_eq!(nav.click(&mut ui, row), MenuAction::None);
        assert_eq!(nav.options().fov, 71);

        // The drag path, through vanilla's bucket map. `(90 + 0.5 - 30) / (110 + 1
        // - 30) = 60.5 / 81` is the fraction the handle draws at for 90, so
        // handing that fraction back must return 90 — and 90 is chosen because at
        // the default the bucket map and the naive endpoint span *coincide* (both
        // 0.5), so a drag gate at 70 would pass under either.
        assert!(nav.drag_slider(&ui, row, 60.5 / 81.0));
        assert_eq!(nav.options().fov, 90);

        // Both ends of the track land exactly on the bounds rather than one past.
        assert!(nav.drag_slider(&ui, row, 0.0));
        assert_eq!(nav.options().fov, MIN_FOV);
        assert!(nav.drag_slider(&ui, row, 1.0));
        assert_eq!(nav.options().fov, MAX_FOV);

        // And the wrap: a keyboard Enter is the only way down from the maximum,
        // so 110 must step to 30 rather than sticking. Vanilla saturates here
        // because it drags; this is the documented departure every `step_*` on
        // this tree shares.
        assert_eq!(nav.click(&mut ui, row), MenuAction::None);
        assert_eq!(nav.options().fov, MIN_FOV, "110 + 1 wraps to 30, not 111");

        let saved = std::fs::read_to_string(&options_path).expect("options.json must exist");
        assert!(
            saved.contains("\"fov\""),
            "the value must reach disk on the click: {saved}"
        );
        assert_eq!(crate::config::Options::load_from(&options_path).fov, MIN_FOV);
        assert_eq!(nav.options_save_error(), None);
    }

    /// The Video page's Mipmap Levels row moves `mipmap_levels`, wraps at the
    /// maximum, lands its drag on vanilla's own bucket, persists, and — the
    /// property this row exists to add over every other `IntRange` slider on
    /// this tree — pushes the change into
    /// `crate::resources::set_mipmap_levels`, the trigger the live atlas
    /// reload polls. `pack_generation` and `mipmap_levels` are process-global
    /// (see `crate::resources`' own `pack_generation_strictly_increases_on_every_selection_change`),
    /// so this asserts the *change*, never an absolute value another test in
    /// this binary could have already moved.
    #[test]
    fn the_mipmap_levels_row_moves_the_option_and_reaches_the_live_reload_trigger() {
        let (mut nav, path) = self::nav("settings-mipmap-levels");
        let mut ui = UiState::new();
        ui.open_settings();
        let options_path = path.parent().unwrap().join("options.json");

        assert_eq!(
            nav.options().mipmap_levels,
            lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS,
            "premise: vanilla's shipped default is the max, 4"
        );

        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Video);
        let row = settings_row(&mut nav, &mut ui, is_option("mipmapLevels"));

        // Parked at the maximum, so a click must wrap to 0 rather than sticking
        // — the same departure `the_root_fov_row_moves_the_option_and_wraps_at_the_maximum`
        // exercises, and the row this one starts on needs it immediately rather
        // than after four more clicks.
        let before_click = crate::resources::pack_generation();
        assert_eq!(nav.click(&mut ui, row), MenuAction::None);
        assert_eq!(nav.options().mipmap_levels, 0, "4 + 1 wraps to 0, not 5");
        assert!(
            crate::resources::pack_generation() > before_click,
            "the click must reach the live-reload trigger, not just the option"
        );
        assert_eq!(crate::resources::mipmap_levels(), 0);

        // The drag path, through vanilla's bucket map: `(2 + 0.5 - 0) / (4 + 1
        // - 0) = 0.5` is the fraction the handle draws at for 2.
        let before_drag = crate::resources::pack_generation();
        assert!(nav.drag_slider(&ui, row, 0.5));
        assert_eq!(nav.options().mipmap_levels, 2);
        assert!(crate::resources::pack_generation() > before_drag);
        assert_eq!(crate::resources::mipmap_levels(), 2);

        // Both ends of the track land exactly on the bounds rather than one past.
        assert!(nav.drag_slider(&ui, row, 0.0));
        assert_eq!(nav.options().mipmap_levels, 0);
        assert!(nav.drag_slider(&ui, row, 1.0));
        assert_eq!(
            nav.options().mipmap_levels,
            lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS
        );

        let saved = std::fs::read_to_string(&options_path).expect("options.json must exist");
        assert!(
            !saved.contains("\"mipmap_levels\""),
            "back at the shipped default, the key must not be written: {saved}"
        );
        assert!(nav.drag_slider(&ui, row, 0.0));
        let saved = std::fs::read_to_string(&options_path).expect("options.json must exist");
        assert!(
            saved.contains("\"mipmap_levels\""),
            "away from the default, the value must reach disk: {saved}"
        );
        assert_eq!(crate::config::Options::load_from(&options_path).mipmap_levels, 0);
        assert_eq!(nav.options_save_error(), None);
    }

    /// The glint pair and the Clouds cycle.
    ///
    /// `glint::DEFAULT_SPEED`/`DEFAULT_STRENGTH` are vanilla's shipped `0.5`/`0.75`
    /// and `CloudStatus::default()` is FANCY, so all three rows agree with their
    /// frozen constants at the default and every value asserted below is a
    /// non-default.
    #[test]
    fn the_glint_and_cloud_rows_move_their_own_options() {
        use lodestone_render::CloudStatus;

        let (mut nav, path) = self::nav("settings-glint-clouds");
        let mut ui = UiState::new();
        ui.open_settings();
        let options_path = path.parent().unwrap().join("options.json");

        assert_eq!(
            f64::from(nav.options().glint_speed),
            lodestone_render::glint::DEFAULT_SPEED,
            "premise: the field boots at the constant the consumer was pinned to"
        );
        assert_eq!(
            nav.options().glint_strength,
            lodestone_render::glint::DEFAULT_STRENGTH
        );

        open_settings_page(
            &mut nav,
            &mut ui,
            crate::menu::options::SettingsPage::Accessibility,
        );
        let speed = settings_row(&mut nav, &mut ui, is_option("glintSpeed"));
        assert!(nav.drag_slider(&ui, speed, 0.25));
        assert_eq!(nav.options().glint_speed, 0.25);
        assert_eq!(
            nav.options().glint_strength,
            lodestone_render::glint::DEFAULT_STRENGTH,
            "Glint Speed's row must not touch Glint Strength — they are adjacent \
             columns of one pair, which is where a mis-indexed row lands"
        );

        let strength = settings_row(&mut nav, &mut ui, is_option("glintStrength"));
        assert_ne!(strength, speed, "premise: two different rows");
        assert!(nav.drag_slider(&ui, strength, 0.375));
        assert_eq!(nav.options().glint_strength, 0.375);
        assert_eq!(nav.options().glint_speed, 0.25, "and not back the other way");

        // A zero on either is a real choice — a frozen shimmer and an invisible
        // one — so the row must be able to reach exactly 0.0 rather than a small
        // positive floor.
        assert!(nav.drag_slider(&ui, speed, 0.0));
        assert_eq!(nav.options().glint_speed, 0.0);

        // -- Clouds: three states in `CloudStatus.values()` order, wrapping.
        //
        // Back to the root first: `open_settings_page` walks a nav button on the
        // *current* page, and Accessibility's only one goes to Controls. Escape is
        // `OptionsSubScreen`'s own way back, so this is the route a player takes.
        nav.key(&mut ui, MenuKey::Escape);
        assert_eq!(
            nav.settings().page(),
            crate::menu::options::SettingsPage::Root
        );
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Video);
        let clouds = settings_row(&mut nav, &mut ui, is_option("cloudStatus"));
        assert_eq!(
            nav.options().cloud_status,
            CloudStatus::Fancy,
            "premise: vanilla's default, and what the sky pass drew unconditionally"
        );
        // FANCY is *last* in the enum, so the first click wraps to OFF. That order
        // is `CycleButton`'s, not a chosen one.
        for want in [CloudStatus::Off, CloudStatus::Fast, CloudStatus::Fancy] {
            assert_eq!(nav.click(&mut ui, clouds), MenuAction::None);
            assert_eq!(nav.options().cloud_status, want);
        }

        // The discriminating property, and the reason `Off` is a variant rather
        // than a skip in the caller: the two geometry predicates are
        // **non-complementary**, and `Off` must answer false to *both*. The natural
        // wrong reading — "not fancy, so draw the flat quad" — satisfies any gate
        // that merely requires the three states to differ, and it would draw FAST
        // clouds for a player who asked for none.
        assert!(!CloudStatus::Off.draws_flat_quad());
        assert!(!CloudStatus::Off.draws_extruded_cells());
        assert!(
            CloudStatus::Fast.draws_flat_quad(),
            "control: the flat-quad predicate is not stuck at false"
        );
        assert!(
            CloudStatus::Fancy.draws_extruded_cells(),
            "control: nor is the extruded one"
        );

        // Persisted **by name**. The ordinal would be the trap: `Off` is ordinal
        // 0, which is also what a missing key deserialises to under an ordinal
        // scheme, so "clouds off" and "no setting" would be indistinguishable.
        assert_eq!(nav.click(&mut ui, clouds), MenuAction::None);
        assert_eq!(nav.options().cloud_status, CloudStatus::Off);
        let saved = std::fs::read_to_string(&options_path).expect("options.json must exist");
        assert!(
            saved.contains("\"off\""),
            "cloud_status must be stored as vanilla's own name, not an ordinal: \
             {saved}"
        );
        let reloaded = crate::config::Options::load_from(&options_path);
        assert_eq!(reloaded.cloud_status, CloudStatus::Off);
        assert_eq!(reloaded.glint_speed, 0.0);
        assert_eq!(reloaded.glint_strength, 0.375);
        assert_eq!(nav.options_save_error(), None);
    }

    /// `app/redraw.rs` must still push the glint options to **all three** sites.
    ///
    /// The same instrument as
    /// [`app_rs_still_threads_every_chat_option_into_the_hud_frame`] and for the
    /// same reason: `redraw.rs` is the frame loop, no unit test in this crate can
    /// run it, and the third site is a *separate pipeline with its own uniform*
    /// — so an enchanted item can shimmer correctly in the world and in hand while
    /// a slot draws it at vanilla's default, with every other test green. That is
    /// exactly the state this batch found the GUI glint in: `set_glint_options`
    /// existed on `IconRenderer`, was read once per frame, and had **no caller**.
    ///
    /// Asserts the **calls**, not line numbers. If the pushes legitimately move,
    /// point this at their new home rather than deleting it.
    #[test]
    fn redraw_rs_still_pushes_the_glint_options_to_all_three_sites() {
        let src = include_str!("../../app/redraw.rs");
        for site in [
            "render.set_glint_options",
            "hud.set_glint_options",
            "container_renderer.set_glint_options",
        ] {
            assert!(
                src.contains(site),
                "app/redraw.rs no longer calls `{site}` — that glint site is back \
                 to vanilla's default constant and out of phase with the others"
            );
        }
        // The other three kind A pushes live in the same function, and each was an
        // island until it landed.
        for push in ["set_cloud_status", "set_sound_volumes", "set_fov_y_degrees"] {
            assert!(src.contains(push), "app/redraw.rs no longer calls `{push}`");
        }
        // The control: the detector must be able to report an absence, so a typo
        // in either list above cannot make this vacuously green.
        assert!(
            !src.contains("nonexistent_renderer.set_glint_options"),
            "the detector must not match a call that is not there"
        );
    }

    /// **Panorama Scroll Speed is a working control, and the value reaches the
    /// renderer.**
    ///
    /// The island here pointed the other way from Damage Tilt's:
    /// `panorama::PanoramaRenderer::set_speed` existed, was unit-tested, and had
    /// **zero callers**, so the title screen always span at `DEFAULT_SPIN_SPEED`
    /// whatever the option said.
    ///
    /// Two links are checked, because they fail independently:
    ///
    /// 1. `frame_for` stamps `MenuFrame::panorama_speed` from the live option, on
    ///    **every** screen — the panorama is drawn behind every non-overlay
    ///    screen, not only the title screen.
    /// 2. `render/renderer.rs` still hands that field to `set_speed`. That call
    ///    lives inside a `wgpu`-owning method a unit test cannot run, so it is
    ///    checked by reading the source — `app_rs_still_threads_every_chat_option_
    ///    into_the_hud_frame`'s mechanism, for the same reason.
    ///
    /// The rate arithmetic itself is `panorama`'s own
    /// `the_spin_rate_is_two_degrees_per_second_at_vanillas_default_speed`; this
    /// gate is about the value getting there.
    #[test]
    fn the_panorama_speed_row_reaches_the_frame_and_the_renderer() {
        let (mut nav, _path) = self::nav("settings-panorama-speed");
        let mut ui = UiState::new();
        ui.open_settings();
        open_settings_page(
            &mut nav,
            &mut ui,
            crate::menu::options::SettingsPage::Accessibility,
        );
        let row = settings_row(&mut nav, &mut ui, is_option("panoramaSpeed"));

        assert_eq!(
            nav.panorama_speed(),
            1.0,
            "premise: vanilla's default is full speed"
        );

        // Click 1 wraps 1.0 to 0.0 — a deliberately stationary panorama, which is
        // the whole point of the option and the value the frame's `Option` wrapper
        // exists to keep distinguishable from "nothing stamped this".
        assert_eq!(nav.click(&mut ui, row), MenuAction::None);
        assert_eq!(nav.panorama_speed(), 0.0);

        let statuses = crate::menu::status::StatusCache::with_probe(
            crate::menu::status::unavailable_probe(),
        );
        let mut favicons = crate::menu::render::FaviconCache::new();
        let frame = crate::menu::render::frame_for(&ui, &nav, &statuses, &mut favicons)
            .expect("the settings screen owns its frame");
        assert_eq!(
            frame.panorama_speed,
            Some(0.0),
            "frame_for must stamp the live value — `Some(0.0)`, not `None`, or the \
             renderer would keep its own default and the option would do nothing"
        );
        drop(frame);

        // And a second value, so this is not passing because the stamp is a
        // constant that happens to match.
        assert_eq!(nav.click(&mut ui, row), MenuAction::None);
        assert!((nav.panorama_speed() - 0.1).abs() < 1e-6);
        let mut favicons = crate::menu::render::FaviconCache::new();
        let frame = crate::menu::render::frame_for(&ui, &nav, &statuses, &mut favicons)
            .expect("the settings screen owns its frame");
        assert!(
            frame.panorama_speed.is_some_and(|s| (s - 0.1).abs() < 1e-6),
            "the stamp must track the option, got {:?}",
            frame.panorama_speed
        );
        drop(frame);

        assert_eq!(
            crate::menu::options::live_value(
                crate::menu::options::LiveOption::PanoramaSpeed,
                nav.options()
            ),
            "10%",
            "the plain percentValueLabel — unlike Damage Tilt beside it, zero here \
             prints 0% rather than OFF, because a still panorama is a value and \
             not an off state"
        );

        // Link 2, the one no unit test can execute.
        let src = include_str!("../render/renderer.rs");
        assert!(
            src.contains("frame.panorama_speed") && src.contains("set_speed"),
            "render/renderer.rs no longer hands `frame.panorama_speed` to \
             `PanoramaRenderer::set_speed` — the row is an island again, and \
             nothing else in this crate can see that"
        );
        // The control: the detector must be able to report an absence.
        assert!(
            !src.contains("frame.panorama_nonexistent_field"),
            "the detector must not match a field that is not there"
        );
    }

    /// A **click** on the settings screen must act on the row it
    /// landed on, not on whatever `MenuKey::Enter` means there.
    ///
    /// This is the whole bug. `app.rs` translated every menu click into
    /// `hover(row)` + `Enter`, which is right on the screens that have a row
    /// cursor and was wrong here, where there was none and `Enter` was hard-wired
    /// to View Bobbing. So a click on the GUI SCALE row — row 0, the one drawn
    /// `selected` — turned the option off and wrote it to disk. Nothing about the
    /// bob itself was broken; the reporter's `options.json` simply said `false`.
    ///
    /// A real cursor removes the cause rather than the symptom: the screen has a
    /// cursor and every row resolves to its own control. The assertion that
    /// matters is still the **negative** one, so it is still paired with a
    /// control — clicking the scale's row must cycle it, or "the click did not
    /// toggle the bob" would pass just as well on a `click` that did nothing.
    #[test]
    fn clicking_a_settings_row_acts_on_that_row_and_no_other() {
        let (mut nav, path) = self::nav("settings-click-rows");
        let mut ui = UiState::new();
        ui.open_settings();
        let options_path = path.parent().unwrap().join("options.json");

        assert!(nav.view_bobbing(), "precondition: the default is ON");
        assert_eq!(nav.gui_scale(), 0, "precondition: the scale starts at auto");

        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Video);
        let scale = settings_row(&mut nav, &mut ui, is_option("guiScale"));
        // Its own row cycles the scale…
        assert_eq!(nav.click(&mut ui, scale), MenuAction::None);
        assert_eq!(nav.gui_scale(), 1, "the clicked row must do its own job");
        assert!(
            nav.view_bobbing(),
            "and must not touch a setting on another screen entirely"
        );
        assert!(
            crate::config::Options::load_from(&options_path).view_bobbing,
            "nor persist it off — that is what survived the restart in #391"
        );

        // …and a still-inert row on the same page does nothing.
        // (`inactivityFpsLimit`, this test's former inert row, went live
        // alongside the rest of the video settings and is exercised by its own
        // gate now; `fullscreen` is still unwired.)
        let inert = settings_row(&mut nav, &mut ui, is_option("fullscreen"));
        assert_ne!(inert, scale, "premise: they are different rows");
        assert_eq!(nav.click(&mut ui, inert), MenuAction::None);
        assert_eq!(
            nav.gui_scale(),
            1,
            "an inactive row must not fall through to whatever Enter means"
        );

        // Control: the *active* row is still reachable by mouse, so the negative
        // assertion above is row-awareness and not a dead `click`.
        assert_eq!(nav.click(&mut ui, scale), MenuAction::None);
        assert_eq!(nav.gui_scale(), 2);

        // A hit-test that lands past the last row must do nothing rather than
        // fall through to the keyboard path.
        let past = nav.settings().visible().len() + 3;
        assert_eq!(nav.click(&mut ui, past), MenuAction::None);
        assert_eq!(nav.gui_scale(), 2);
        assert!(nav.view_bobbing());
        assert_eq!(ui.screen(), Screen::Settings);
    }

    /// Clicking Sneak/Sprint's rows on the Controls page toggles
    /// their hold/toggle mode and persists immediately, isolated from each
    /// other and from an inactive neighbour — same shape as
    /// [`clicking_a_settings_row_acts_on_that_row_and_no_other`], scoped to
    /// the live rows.
    #[test]
    fn clicking_sneak_or_sprint_toggles_only_that_ones_mode() {
        let (mut nav, path) = self::nav("settings-toggle-sneak-sprint");
        let mut ui = UiState::new();
        ui.open_settings();
        let options_path = path.parent().unwrap().join("options.json");

        assert!(!nav.toggle_sneak(), "vanilla's own default is hold");
        assert!(!nav.toggle_sprint());
        assert!(!nav.toggle_attack(), "the #444 rows share the hold default");

        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Controls);
        let sneak = settings_row(&mut nav, &mut ui, is_option("toggleCrouch"));
        assert_eq!(nav.click(&mut ui, sneak), MenuAction::None);
        assert!(nav.toggle_sneak(), "the clicked row must flip");
        assert!(!nav.toggle_sprint(), "and not its neighbour");
        assert!(!nav.toggle_attack());
        assert!(crate::config::Options::load_from(&options_path).toggle_sneak);
        assert!(!crate::config::Options::load_from(&options_path).toggle_sprint);

        let sprint = settings_row(&mut nav, &mut ui, is_option("toggleSprint"));
        assert_ne!(sprint, sneak);
        assert_eq!(nav.click(&mut ui, sprint), MenuAction::None);
        assert!(nav.toggle_sprint());
        assert!(nav.toggle_sneak(), "sprint's click must not un-flip sneak");
        assert!(!nav.toggle_attack());

        // Attack/Destroy is now a live row too: clicking it flips only
        // its own mode, leaving Sneak and Sprint untouched.
        let attack = settings_row(&mut nav, &mut ui, is_option("toggleAttack"));
        assert_eq!(nav.click(&mut ui, attack), MenuAction::None);
        assert!(nav.toggle_attack(), "the clicked row must flip");
        assert!(nav.toggle_sneak(), "and not its neighbours");
        assert!(nav.toggle_sprint());
        assert!(crate::config::Options::load_from(&options_path).toggle_attack);
    }

    /// Clicking the Mouse page's Scroll Sensitivity / Invert X / Invert
    /// Y rows mutates and persists only the clicked one.
    #[test]
    fn clicking_a_mouse_row_touches_only_that_row() {
        let (mut nav, path) = self::nav("settings-mouse-feel");
        let mut ui = UiState::new();
        ui.open_settings();
        let options_path = path.parent().unwrap().join("options.json");

        assert_eq!(nav.mouse_wheel_sensitivity(), 1.0, "vanilla's own default");
        assert!(!nav.invert_mouse_x());
        assert!(!nav.invert_mouse_y());

        // Mouse Settings is nested under Controls, not a root-level page
        // (`nav("Mouse Settings...", SettingsPage::Mouse)` lives inside
        // `CONTROLS`) — so reaching it is two hops, matching how a player
        // would actually navigate there.
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Controls);
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Mouse);

        let wheel = settings_row(&mut nav, &mut ui, is_option("mouseWheelSensitivity"));
        assert_eq!(nav.click(&mut ui, wheel), MenuAction::None);
        assert!(
            (nav.mouse_wheel_sensitivity() - 1.25).abs() < 1e-4,
            "one click is one MOUSE_WHEEL_SENSITIVITY_STEP; got {}",
            nav.mouse_wheel_sensitivity()
        );
        assert!(!nav.invert_mouse_x(), "must not touch an unrelated row");
        assert!(!nav.invert_mouse_y());
        assert!(
            (crate::config::Options::load_from(&options_path).mouse_wheel_sensitivity - 1.25).abs()
                < 1e-4
        );

        let inv_x = settings_row(&mut nav, &mut ui, is_option("invertMouseX"));
        assert_ne!(inv_x, wheel);
        assert_eq!(nav.click(&mut ui, inv_x), MenuAction::None);
        assert!(nav.invert_mouse_x());
        assert!(!nav.invert_mouse_y(), "invert X must not flip invert Y");
        assert!(
            (nav.mouse_wheel_sensitivity() - 1.25).abs() < 1e-4,
            "…nor touch the slider"
        );

        let inv_y = settings_row(&mut nav, &mut ui, is_option("invertMouseY"));
        assert_ne!(inv_y, inv_x);
        assert_eq!(nav.click(&mut ui, inv_y), MenuAction::None);
        assert!(nav.invert_mouse_y());
        assert!(nav.invert_mouse_x(), "invert Y's click must not un-flip X");

        // Sensitivity (look) is deliberately inactive — see the module docs.
        let look_sensitivity = settings_row(&mut nav, &mut ui, is_option("sensitivity"));
        assert_eq!(nav.click(&mut ui, look_sensitivity), MenuAction::None);
        assert!(nav.invert_mouse_x());
        assert!(nav.invert_mouse_y());
    }

    /// The Key Binds screen is reachable through Controls, its parent page, and
    /// Escape returns to Controls. This test exercises both navigation edges
    /// with the same two-hop path a player uses from the root settings grid.
    #[test]
    fn the_key_binds_screen_is_reachable_from_controls_and_escape_returns_there() {
        let (mut nav, _path) = self::nav("key-binds-reachable");
        let mut ui = UiState::new();
        ui.open_settings();

        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Controls);
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::KeyBinds);
        assert_eq!(
            nav.settings().page(),
            crate::menu::options::SettingsPage::KeyBinds
        );
        assert_eq!(ui.screen(), Screen::Settings, "still one Screen the whole way down");

        // Escape, with nothing being captured, leaves the page — back to
        // Controls, not the title (the page stack, not `UiState`).
        nav.key(&mut ui, MenuKey::Escape);
        assert_eq!(
            nav.settings().page(),
            crate::menu::options::SettingsPage::Controls
        );
        assert_eq!(ui.screen(), Screen::Settings, "Escape here must not leave Settings");
    }

    /// The last hop app.rs owns (see `MenuNav::capture_binding`'s doc):
    /// clicking a bind button starts capture entirely within this crate;
    /// finishing it needs the raw key/mouse event app.rs would forward. This
    /// drives both halves without a `WindowApp` by calling `capture_binding`
    /// directly, the same call `app.rs`'s patch is specified to make.
    #[test]
    fn clicking_a_bind_button_then_capturing_a_key_rebinds_and_persists() {
        use crate::keybinds::{Binding, InputAction};
        use crate::menu::key_binds::KeyControl;
        use winit::keyboard::KeyCode;

        let (mut nav, path) = self::nav("key-binds-capture");
        let mut ui = UiState::new();
        ui.open_settings();
        let options_path = path.parent().unwrap().join("options.json");

        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Controls);
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::KeyBinds);

        assert!(!nav.awaiting_key_capture());
        let bind_row = key_binds_row(&mut nav, &mut ui, |c| {
            *c == KeyControl::Bind(InputAction::Forward)
        });
        assert_eq!(nav.click(&mut ui, bind_row), MenuAction::None);
        assert!(
            nav.awaiting_key_capture(),
            "clicking the bind button alone must start capture"
        );
        // Nothing is persisted yet — starting capture is pure UI state.
        assert!(
            crate::config::Options::load_from(&options_path)
                .keybinds
                .is_default(InputAction::Forward)
        );

        // The forwarded raw key, exactly as `app.rs`'s patch is specified to
        // call it.
        nav.capture_binding(Binding::Key(KeyCode::KeyF.into()));
        assert!(!nav.awaiting_key_capture(), "the capture is consumed");
        assert_eq!(
            nav.settings().page(),
            crate::menu::options::SettingsPage::KeyBinds,
            "finishing a capture must not itself leave the page"
        );
        let persisted = crate::config::Options::load_from(&options_path);
        assert_eq!(
            persisted.keybinds.binding(InputAction::Forward),
            Binding::Key(KeyCode::KeyF.into()),
            "and it must reach the file immediately, not at exit"
        );
    }

    /// **The gap capture patch (`a6da3f6`) existed to close**:
    /// a key with *no printable text* — an F-key, here, per that commit's own
    /// choice of `F1` over `F5` so a currently-unbound key is exercised —
    /// must be bindable end to end, not just started.
    ///
    /// The test above already proves this shape for a printable key
    /// (`KeyF`); it proves nothing about `menu_key_for`'s no-text drop
    /// because a printable key never reaches that branch. `app.rs::
    /// capture_key_for_forwards_a_function_key` proves the physical-key
    /// half (`capture_key_for(F1) == Some(CaptureKey::Bind(F1))`) but reads
    /// no persisted string — this is the missing other half, driven with
    /// `Binding::Key(KeyCode::F1.into())`, exactly what `app.rs`'s
    /// `Some(CaptureKey::Bind(code)) => self.nav.capture_binding(Binding::
    /// Key(code))` arm forwards verbatim for any `KeyCode`, F1 included.
    ///
    /// Asserts vanilla's own persisted spelling (`key.keyboard.f1`,
    /// `keybinds.rs`'s own `KEY_NAMES` table) directly out of the file on
    /// disk, not `Binding::name()` called a second time — the same file a
    /// restart reads back.
    #[test]
    fn a_key_with_no_printable_text_binds_end_to_end_with_vanillas_real_name() {
        use crate::keybinds::{Binding, InputAction};
        use crate::menu::key_binds::KeyControl;
        use winit::keyboard::KeyCode;

        let (mut nav, path) = self::nav("key-binds-capture-f1");
        let mut ui = UiState::new();
        ui.open_settings();
        let options_path = path.parent().unwrap().join("options.json");

        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Controls);
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::KeyBinds);
        let bind_row = key_binds_row(&mut nav, &mut ui, |c| {
            *c == KeyControl::Bind(InputAction::Forward)
        });
        nav.click(&mut ui, bind_row);
        assert!(nav.awaiting_key_capture());

        // The exact call `app.rs`'s forwarding arm makes for `PhysicalKey::
        // Code(KeyCode::F1)` — no printable `text`, which is what
        // `menu_key_for` alone would have silently dropped before `a6da3f6`.
        nav.capture_binding(Binding::Key(KeyCode::F1.into()));
        assert!(!nav.awaiting_key_capture(), "the capture is consumed");
        assert_eq!(
            nav.options().keybinds.binding(InputAction::Forward),
            Binding::Key(KeyCode::F1.into())
        );

        let raw = std::fs::read_to_string(&options_path).expect("options.json must exist");
        let value: serde_json::Value = serde_json::from_str(&raw).expect("must be valid JSON");
        let persisted_name = value
            .get("keybinds")
            .and_then(|k| k.get(InputAction::Forward.name()))
            .and_then(serde_json::Value::as_str)
            .expect("key.forward must be a persisted string");
        assert_eq!(
            persisted_name, "key.keyboard.f1",
            "an F-key must persist under vanilla's own InputConstants spelling, \
             not a winit debug name or a dropped/blank binding"
        );

        // Reloading from disk must reproduce the same binding — the round
        // trip a restart performs, not just the in-memory `Keybinds`.
        let reloaded = crate::config::Options::load_from(&options_path);
        assert_eq!(
            reloaded.keybinds.binding(InputAction::Forward),
            Binding::Key(KeyCode::F1.into()),
            "the persisted name must parse back to the same binding on load"
        );
    }

    /// Escape while capturing cancels the capture and leaves the binding
    /// exactly as it was — vanilla's own `keyPressed` sets `UNKNOWN`
    /// unconditionally on Escape while capturing;
    /// this client does not, for the `Pause`-unbind hazard
    /// `MenuNav::capture_binding`'s doc names. The control is the *other*
    /// direction: a genuine key still rebinds, so this is not "Escape is
    /// broken", it is "Escape means cancel, not unbind".
    #[test]
    fn escape_while_capturing_cancels_without_changing_the_binding() {
        use crate::keybinds::InputAction;
        use crate::menu::key_binds::KeyControl;

        let (mut nav, path) = self::nav("key-binds-escape-cancels");
        let mut ui = UiState::new();
        ui.open_settings();
        let options_path = path.parent().unwrap().join("options.json");

        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Controls);
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::KeyBinds);
        let bind_row = key_binds_row(&mut nav, &mut ui, |c| {
            *c == KeyControl::Bind(InputAction::Forward)
        });
        nav.click(&mut ui, bind_row);
        assert!(nav.awaiting_key_capture());

        nav.key(&mut ui, MenuKey::Escape);
        assert!(!nav.awaiting_key_capture(), "cancelled");
        assert_eq!(
            nav.settings().page(),
            crate::menu::options::SettingsPage::KeyBinds,
            "cancelling a capture must not also leave the page"
        );
        assert!(
            crate::config::Options::load_from(&options_path)
                .keybinds
                .is_default(InputAction::Forward),
            "unchanged — nothing was ever persisted"
        );
    }

    /// The hazard `crate::keybinds::InputAction::Pause`'s own doc names,
    /// `docs/keybindings.md` records as unenforced, and
    /// `MenuNav::capture_binding` is the first place able to enforce it: a
    /// player who captures Pause and then presses Escape (which this client
    /// does *not* treat as "cancel" for a *literal* Escape key-press the way
    /// it does for the menu's own Escape — see the previous test's doc) must
    /// not end up with Pause unbound and no way back to the title screen.
    #[test]
    fn capturing_pause_refuses_to_leave_it_unbound() {
        use crate::keybinds::{Binding, InputAction};
        use crate::menu::key_binds::KeyControl;

        let (mut nav, path) = self::nav("key-binds-pause-hazard");
        let mut ui = UiState::new();
        ui.open_settings();
        let options_path = path.parent().unwrap().join("options.json");
        let default_pause = nav.options().keybinds.binding(InputAction::Pause);

        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Controls);
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::KeyBinds);
        let bind_row = key_binds_row(&mut nav, &mut ui, |c| {
            *c == KeyControl::Bind(InputAction::Pause)
        });
        nav.click(&mut ui, bind_row);
        assert!(nav.awaiting_key_capture());

        nav.capture_binding(Binding::Unbound);
        assert_eq!(
            nav.options().keybinds.binding(InputAction::Pause),
            default_pause,
            "refused: Pause must never be set to Unbound through capture"
        );
        assert!(!nav.awaiting_key_capture(), "the capture is still consumed");
        assert!(
            crate::config::Options::load_from(&options_path)
                .keybinds
                .is_default(InputAction::Pause)
        );

        // The control: capturing a real key for Pause still works — the
        // guard is specific to `Unbound`, not to `Pause` as a whole.
        let bind_row = key_binds_row(&mut nav, &mut ui, |c| {
            *c == KeyControl::Bind(InputAction::Pause)
        });
        nav.click(&mut ui, bind_row);
        nav.capture_binding(Binding::Key(winit::keyboard::KeyCode::KeyP.into()));
        assert_eq!(
            nav.options().keybinds.binding(InputAction::Pause),
            Binding::Key(winit::keyboard::KeyCode::KeyP.into())
        );
    }

    /// Per-row Reset and the footer's Reset Keys, both persisted
    /// immediately, both isolated from an untouched neighbour — the same
    /// shape every other live row in this tree already proves.
    #[test]
    fn resetting_one_action_and_reset_all_persist_through_a_real_file() {
        use crate::keybinds::{Binding, InputAction};
        use crate::menu::key_binds::KeyControl;
        use winit::keyboard::KeyCode;

        let (mut nav, path) = self::nav("key-binds-reset");
        let mut ui = UiState::new();
        ui.open_settings();
        let options_path = path.parent().unwrap().join("options.json");

        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Controls);
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::KeyBinds);

        // Change two actions so there is something to reset.
        for (action, key) in [
            (InputAction::Forward, KeyCode::KeyF),
            (InputAction::Back, KeyCode::KeyB),
        ] {
            let bind_row = key_binds_row(&mut nav, &mut ui, |c| *c == KeyControl::Bind(action));
            nav.click(&mut ui, bind_row);
            nav.capture_binding(Binding::Key(key.into()));
        }
        assert_eq!(
            nav.options().keybinds.binding(InputAction::Forward),
            Binding::Key(KeyCode::KeyF.into())
        );

        // Resetting Forward alone must not touch Back.
        let forward_reset =
            key_binds_row(&mut nav, &mut ui, |c| *c == KeyControl::Reset(InputAction::Forward));
        assert_eq!(nav.click(&mut ui, forward_reset), MenuAction::None);
        assert!(nav.options().keybinds.is_default(InputAction::Forward));
        assert_eq!(
            nav.options().keybinds.binding(InputAction::Back),
            Binding::Key(KeyCode::KeyB.into()),
            "an untouched neighbour must not reset"
        );
        assert!(
            crate::config::Options::load_from(&options_path)
                .keybinds
                .is_default(InputAction::Forward)
        );

        // Reset Keys resets everything, including Back.
        let reset_all = key_binds_row(&mut nav, &mut ui, |c| *c == KeyControl::ResetAll);
        assert_eq!(nav.click(&mut ui, reset_all), MenuAction::None);
        assert!(nav.options().keybinds.is_default(InputAction::Back));
        assert!(
            crate::config::Options::load_from(&options_path)
                .keybinds
                .is_default(InputAction::Back)
        );
    }

    /// The scroll-sensitivity click wraps at the configured slider bounds
    /// rather than running away, and steps by exactly one increment at a time
    /// — predicted from the constants, not just "it changed".
    #[test]
    fn mouse_wheel_sensitivity_cycles_and_wraps_at_vanillas_bounds() {
        use crate::config::{
            MAX_MOUSE_WHEEL_SENSITIVITY, MIN_MOUSE_WHEEL_SENSITIVITY, MOUSE_WHEEL_SENSITIVITY_STEP,
        };
        let (mut nav, _path) = self::nav("settings-wheel-wrap");
        let mut ui = UiState::new();
        ui.open_settings();
        // Mouse Settings is nested under Controls, not a root-level page
        // (`nav("Mouse Settings...", SettingsPage::Mouse)` lives inside
        // `CONTROLS`) — so reaching it is two hops, matching how a player
        // would actually navigate there.
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Controls);
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Mouse);
        let wheel = settings_row(&mut nav, &mut ui, is_option("mouseWheelSensitivity"));

        // A single-shot closed form (`MIN + (start_offset + n*STEP).rem_euclid(period)`,
        // with no clamp) is **not** what the mutator implements, and this used
        // to assert exactly that — measured wrong at click 77, predicting
        // `10.01` where the real value is `10.0`. `span` (`9.99`) is not a
        // multiple of `STEP` (`0.25`), so `period = span + STEP` (`10.24`)
        // leaves a dead zone of width `STEP - (period - span - STEP)` — i.e.
        // the last `0.01` of every period — where the raw modular position
        // is past `MAX` but has not yet wrapped past a full `period`.
        // `cycle_mouse_wheel_sensitivity` clamps there, and that clamp is
        // **lossy**: the next click's offset is read back from the clamped
        // value, not the discarded raw one, so every click after the first
        // one that lands in the dead zone is permanently shifted by however
        // much that click clamped away. A one-shot formula computed from `n`
        // alone cannot see this — it has to be the same per-click recurrence,
        // reproduced here from the documented constants (not by calling
        // `cycle_mouse_wheel_sensitivity` itself, which would make this
        // vacuous) so the test still predicts every value rather than just
        // asserting it changed. Checked at *every* click for 90 of them —
        // more than two full periods (`10.24 / 0.25 ≈ 41` steps/period) — so
        // this exercises more than one dead-zone clamp.
        let span = MAX_MOUSE_WHEEL_SENSITIVITY - MIN_MOUSE_WHEEL_SENSITIVITY;
        let period = span + MOUSE_WHEEL_SENSITIVITY_STEP;
        assert!(
            (nav.mouse_wheel_sensitivity() - 1.0).abs() < 1e-6,
            "precondition: starts at vanilla's default"
        );
        let mut expected = 1.0_f32; // vanilla's own default

        for n in 1..=90_i32 {
            nav.click(&mut ui, wheel);
            let offset = expected - MIN_MOUSE_WHEEL_SENSITIVITY;
            let wrapped = (offset + MOUSE_WHEEL_SENSITIVITY_STEP).rem_euclid(period);
            expected = (MIN_MOUSE_WHEEL_SENSITIVITY + wrapped)
                .clamp(MIN_MOUSE_WHEEL_SENSITIVITY, MAX_MOUSE_WHEEL_SENSITIVITY);
            let got = nav.mouse_wheel_sensitivity();
            assert!(
                (got - expected).abs() < 1e-4,
                "click {n}: expected {expected}, got {got}"
            );
            assert!(
                (MIN_MOUSE_WHEEL_SENSITIVITY - 1e-4..=MAX_MOUSE_WHEEL_SENSITIVITY + 1e-4)
                    .contains(&got),
                "click {n}: {got} left vanilla's own \
                 {MIN_MOUSE_WHEEL_SENSITIVITY}..={MAX_MOUSE_WHEEL_SENSITIVITY} range"
            );
        }
    }

    /// A settings row index is an index into a `rows` vector built in
    /// `menu::render`, a different file with no compile-time link to it — and
    /// it also depends on which page is showing and how far it is
    /// scrolled. If the two disagree the mouse acts on the wrong control, which
    /// is exactly the failure this coupling would cause.
    ///
    /// `options::tests::the_settings_rows_are_in_the_order_click_assumes` sweeps
    /// every page at every scroll position against `settings_frame` directly;
    /// this one checks the same agreement through the **real** `frame_for`, which
    /// is the path `app.rs` uses.
    #[test]
    fn the_settings_rows_are_in_the_order_click_assumes() {
        let (mut nav, _) = self::nav("settings-row-order");
        let mut ui = UiState::new();
        ui.open_settings();
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Video);
        let scale = settings_row(&mut nav, &mut ui, is_option("guiScale"));

        let mut favicons = crate::menu::render::FaviconCache::new();
        let frame = crate::menu::render::frame_for(
            &ui,
            &nav,
            &crate::menu::status::StatusCache::new(),
            &mut favicons,
        )
        .expect("the settings screen owns its frame");
        let visible = nav.settings().visible();
        assert_eq!(
            frame.rows.len(),
            visible.len(),
            "the frame and the control list must agree in length"
        );
        for (row, control) in visible.iter().enumerate() {
            assert_eq!(
                frame.rows[row].label,
                control.cell.label(nav.options()),
                "row {row}"
            );
        }
        assert_eq!(frame.rows[scale].label, "GUI Scale: Auto");
        assert_eq!(frame.selected, scale, "and the cursor draws on that row");

        // The label tracks the value, so a click's effect is visible.
        nav.click(&mut ui, scale);
        let frame = crate::menu::render::frame_for(
            &ui,
            &nav,
            &crate::menu::status::StatusCache::new(),
            &mut favicons,
        )
        .unwrap();
        assert_eq!(frame.rows[scale].label, "GUI Scale: 1");
    }

    /// The coupling `the_settings_rows_are_in_the_order_click_assumes`
    /// guards [`crate::menu::world_select`]'s focus ids, which are indices into a `rows`
    /// vector built in `menu::render`. If that vector is reordered, the mouse
    /// acts on a different control from the one under the pointer.
    #[test]
    fn the_world_select_rows_are_in_the_order_click_assumes() {
        use crate::menu::world_select::{SEARCH_FIELD, WORLD_SELECT_BUTTONS};
        let (mut nav, _) = self::nav("world-select-row-order");
        let mut ui = UiState::new();
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert_eq!(ui.screen(), Screen::WorldSelect, "Singleplayer opens it");
        let mut favicons = crate::menu::render::FaviconCache::new();
        let frame = crate::menu::render::frame_for(
            &ui,
            &nav,
            &crate::menu::status::StatusCache::new(),
            &mut favicons,
        )
        .expect("the world list owns its frame");
        assert_eq!(frame.rows.len(), 1 + WORLD_SELECT_BUTTONS.len());
        assert!(
            frame.rows[SEARCH_FIELD].edit.is_some(),
            "row {SEARCH_FIELD} must be the search box"
        );
        for button in WORLD_SELECT_BUTTONS {
            assert_eq!(
                frame.rows[button.row()].label,
                button.label(),
                "row {} is not {button:?}",
                button.row()
            );
        }
    }

    /// A click on the world list does what the label under it says — the third
    /// screen to need its own `click` arm rather than "hover then Enter".
    #[test]
    fn clicking_back_leaves_the_world_list_and_clicking_create_does_nothing() {
        use crate::menu::world_select::WorldSelectButton as B;
        let (mut nav, _) = self::nav("world-select-click");
        let mut ui = UiState::new();
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(ui.screen(), Screen::WorldSelect);

        // The disabled buttons first, so a stray activation would be visible as a
        // screen change before Back is ever pressed.
        for button in [B::Edit, B::Delete, B::ReCreate] {
            assert_eq!(nav.click(&mut ui, button.row()), MenuAction::None);
            assert_eq!(
                ui.screen(),
                Screen::WorldSelect,
                "clicking {button:?} must do nothing at all"
            );
        }
        // Create is live now and does do something: it opens
        // World Creation. Checked and reversed here rather than folded into
        // the disabled loop above.
        assert_eq!(nav.click(&mut ui, B::Create.row()), MenuAction::None);
        assert_eq!(ui.screen(), Screen::CreateWorld, "clicking Create must open it");
        assert_eq!(nav.key(&mut ui, MenuKey::Escape), MenuAction::None);
        assert_eq!(ui.screen(), Screen::WorldSelect, "Escape returns to the world list");
        // Clicking the search field must not activate the screen either — the
        // `ServerEdit` bug one screen over.
        assert_eq!(
            nav.click(&mut ui, crate::menu::world_select::SEARCH_FIELD),
            MenuAction::None
        );
        assert_eq!(ui.screen(), Screen::WorldSelect);

        // The control: Back does leave, so the assertions above are about which
        // row was clicked and not about a `click` that does nothing.
        assert_eq!(nav.click(&mut ui, B::Back.row()), MenuAction::None);
        assert_eq!(ui.screen(), Screen::MainMenu);
        assert!(!ui.quit_requested(), "Back is not a quit");
    }

    /// **Pressing Create reaches the app with the typed seed.** The world-list
    /// and integrated-server paths are described in that variant's doc. This
    /// drives the real screen flow — open World Creation, focus the Seed field,
    /// type a seed, click Create — and checks the action the app receives rather
    /// than reading `CreateWorldNav::config()` a second time.
    ///
    /// The screen must **not** change here, mirroring Play Selected World
    /// immediately below: `begin_singleplayer` is what moves to
    /// `Screen::Connecting`.
    #[test]
    fn creating_a_world_asks_the_app_to_start_singleplayer_with_the_typed_seed() {
        use crate::menu::create_world::{CREATE_ROW, SEED_FIELD, WORLD_TAB};
        use crate::menu::world_select::WorldSelectButton as B;

        let (mut nav, _) = self::nav("create-world-seed");
        let mut ui = UiState::new();
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(ui.screen(), Screen::WorldSelect, "premise");
        assert_eq!(nav.click(&mut ui, B::Create.row()), MenuAction::None);
        assert_eq!(ui.screen(), Screen::CreateWorld, "premise: World Creation is open");

        // Seed lives on the World tab — click the tab first, the
        // same two clicks a player makes.
        assert_eq!(nav.click(&mut ui, WORLD_TAB), MenuAction::None);
        let seed_row = nav
            .create_world()
            .frame_row_for_focus_id(SEED_FIELD)
            .expect("the Seed field is visible on the World tab");
        assert_eq!(
            nav.click(&mut ui, seed_row),
            MenuAction::None,
            "focusing the Seed field must not itself produce an action"
        );
        type_str(&mut nav, &mut ui, "777");

        let create_row = nav
            .create_world()
            .frame_row_for_focus_id(CREATE_ROW)
            .expect("Create is always visible, on every tab");
        let action = nav.click(&mut ui, create_row);
        let MenuAction::Singleplayer(_, SingleplayerLaunch::Created { world_dir, config }) = action
        else {
            panic!("expected MenuAction::Singleplayer(Created {{ .. }}), got {action:?}");
        };
        assert_eq!(config.seed, "777", "the typed seed must reach the action's payload");
        // Pressing Create really **creates**. The action carries a new directory
        // and the typed configuration, so each submission has an independent
        // destination and seed rather than reusing an existing world's metadata.
        assert!(world_dir.is_dir(), "Create must have made a directory: {world_dir:?}");
        assert!(
            world_dir.starts_with(nav.saves_root()),
            "and it must be under this nav's own saves root, not the real one: {world_dir:?}"
        );
        assert!(
            world_dir.join("level.dat").is_file(),
            "a world folder with no level.dat is not one vanilla will open"
        );
        // And **not** the seed's own file: `resolve_world_seed` creates that on
        // first open, which is what makes the typed seed win for a new world.
        assert!(
            !world_dir
                .join("data")
                .join("minecraft")
                .join("world_gen_settings.dat")
                .exists(),
            "the menu must not pre-write the seed file"
        );
        assert_eq!(
            ui.screen(),
            Screen::CreateWorld,
            "the nav layer must not leave the screen; begin_singleplayer does that"
        );
    }

    /// **Toggling an Experiments flag reaches a real `level.dat` on disk**
    /// (Experiments half stops being decorative).
    ///
    /// Before this, `WorldCreationConfig::experiments` was collected and
    /// discarded: nothing between here and the freshly created world's save
    /// data ever read it. This drives the exact screen flow a player uses —
    /// World Creation, the More tab, the Experiments row, one toggle, Done,
    /// Create — through `nav.click`, the same dispatch
    /// `creating_a_world_asks_the_app_to_start_singleplayer_with_the_typed_seed`
    /// uses for the Seed field, so it fails if `apply_create_world` or
    /// `saves::create_world_in` ever stops threading the flag through. The
    /// assertion reads the written file back with
    /// `lodestone_anvil::level_dat::read_from_file` — a decoder sharing no
    /// code with `LevelDat::with_enabled_features`, per this repo's own rule
    /// that `decode(encode(x)) == x` against one's own writer proves nothing.
    #[test]
    fn toggling_an_experiment_and_creating_writes_enabled_features_to_level_dat() {
        use crate::menu::create_world::{CREATE_ROW, EXPERIMENTS_ROW, MORE_TAB};
        use crate::menu::world_select::WorldSelectButton as B;

        let (mut nav, _) = self::nav("create-world-experiments");
        let mut ui = UiState::new();
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(ui.screen(), Screen::WorldSelect, "premise");
        assert_eq!(nav.click(&mut ui, B::Create.row()), MenuAction::None);
        assert_eq!(ui.screen(), Screen::CreateWorld, "premise: World Creation is open");

        // Experiments lives on the More tab (tab layout).
        assert_eq!(nav.click(&mut ui, MORE_TAB), MenuAction::None);
        let experiments_row = nav
            .create_world()
            .frame_row_for_focus_id(EXPERIMENTS_ROW)
            .expect("the Experiments row is visible on the More tab");
        assert_eq!(nav.click(&mut ui, experiments_row), MenuAction::None);
        assert!(
            nav.create_world().experiments_open(),
            "premise: the click opened the Experiments sub-screen"
        );
        // Toggle(1) == RedstoneExperiments (`ExperimentFlag::ALL`), then Done —
        // the sub-editor's own row space, exactly as `click_row` dispatches
        // while `CreateWorldMode::Experiments` is active.
        assert_eq!(nav.click(&mut ui, 1), MenuAction::None);
        // Done is the row right after the three toggles (`ALL_EXPERIMENT_CONTROLS`,
        // private to `create_world.rs`): `Toggle(0)`, `Toggle(1)`, `Toggle(2)`, `Done`.
        let done_row = crate::menu::create_world::ExperimentFlag::ALL.len();
        assert_eq!(nav.click(&mut ui, done_row), MenuAction::None);
        assert!(
            !nav.create_world().experiments_open(),
            "Done must leave the sub-editor, back onto the More tab"
        );

        let create_row = nav
            .create_world()
            .frame_row_for_focus_id(CREATE_ROW)
            .expect("Create is always visible, on every tab");
        let action = nav.click(&mut ui, create_row);
        let MenuAction::Singleplayer(_, SingleplayerLaunch::Created { world_dir, config }) = action
        else {
            panic!("expected MenuAction::Singleplayer(Created {{ .. }}), got {action:?}");
        };
        assert_eq!(
            config.experiments,
            vec!["redstone_experiments".to_string()],
            "the action's own config must carry the one toggled flag"
        );

        let level = lodestone_anvil::level_dat::read_from_file(
            &lodestone_anvil::level_dat::path_in(&world_dir),
        )
        .expect("apply_create_world must have written a decodable level.dat");
        assert_eq!(
            level.enabled_features(),
            vec![
                "minecraft:vanilla".to_string(),
                "minecraft:redstone_experiments".to_string(),
            ],
            "the real level.dat on disk must carry vanilla's own enabled_features \
             shape for the flag the player actually toggled — read back with the \
             crate's real decoder, not asserted against the writer's own input"
        );
    }

    /// **The "Customize Type" half, end to end at the nav layer**: the real
    /// screen flow (World tab → cycle to Flat → Customize → cycle the preset
    /// → Done → Create) reaches a decodable `world_gen_settings.dat` on disk,
    /// carrying the cycled preset's own real layers — not the vanilla
    /// default one more click back would have chosen, and not merely "some
    /// file exists". Mirrors
    /// `toggling_an_experiment_and_creating_writes_enabled_features_to_level_dat`'s
    /// own shape and reasoning, one file over: `nav.click` end to end,
    /// `world_gen_settings.dat` read back through the crate's real decoder.
    #[test]
    fn customizing_a_flat_world_and_creating_writes_the_layers_to_world_gen_settings_dat() {
        use crate::menu::create_world::{CREATE_ROW, CUSTOMIZE_ROW, WORLD_TAB, WORLD_TYPE_ROW};
        use crate::menu::world_select::WorldSelectButton as B;

        let (mut nav, _) = self::nav("create-world-customize");
        let mut ui = UiState::new();
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(ui.screen(), Screen::WorldSelect, "premise");
        assert_eq!(nav.click(&mut ui, B::Create.row()), MenuAction::None);
        assert_eq!(ui.screen(), Screen::CreateWorld, "premise: World Creation is open");

        assert_eq!(nav.click(&mut ui, WORLD_TAB), MenuAction::None);
        let world_type_row = nav
            .create_world()
            .frame_row_for_focus_id(WORLD_TYPE_ROW)
            .expect("World Type is visible on the World tab");
        // Normal -> LargeBiomes -> Amplified -> SingleBiomeSurface -> Flat.
        for _ in 0..4 {
            assert_eq!(nav.click(&mut ui, world_type_row), MenuAction::None);
        }
        assert_eq!(
            nav.create_world().config().world_type,
            crate::menu::create_world::WorldTypePreset::Flat,
            "premise: reached Flat"
        );

        let customize_row = nav
            .create_world()
            .frame_row_for_focus_id(CUSTOMIZE_ROW)
            .expect("Customize Type is visible on the World tab");
        assert_eq!(nav.click(&mut ui, customize_row), MenuAction::None);
        assert!(
            nav.create_world().customize_open(),
            "premise: the click opened the Customize sub-screen now that Flat is selected"
        );
        // Row 0 is the one Cycle control (`ALL_CUSTOMIZE_CONTROLS`, private to
        // `create_world.rs`): ClassicFlat -> TunnelersDream.
        assert_eq!(nav.click(&mut ui, 0), MenuAction::None);
        // Row 1 is Done.
        assert_eq!(nav.click(&mut ui, 1), MenuAction::None);
        assert!(!nav.create_world().customize_open(), "Done must leave the sub-editor");

        let create_row = nav
            .create_world()
            .frame_row_for_focus_id(CREATE_ROW)
            .expect("Create is always visible, on every tab");
        let action = nav.click(&mut ui, create_row);
        let MenuAction::Singleplayer(_, SingleplayerLaunch::Created { world_dir, config }) = action
        else {
            panic!("expected MenuAction::Singleplayer(Created {{ .. }}), got {action:?}");
        };
        assert_eq!(
            config.flat_layers,
            Some(crate::menu::create_world::FlatLayerPreset::TunnelersDream),
            "the action's own config must carry the cycled-to preset"
        );

        let settings = lodestone_anvil::world_gen_settings::read_from_file(
            &lodestone_anvil::world_gen_settings::path_in(&world_dir),
        )
        .expect("apply_create_world must have written a decodable world_gen_settings.dat");
        assert!(
            settings.has_dimensions(),
            "the customized generator must reach the real file on disk"
        );
        assert!(
            settings.seed().is_ok(),
            "the file must carry a real seed alongside the generator override — a \
             dimensions compound with no seed would make the world's first open fail"
        );
    }

    /// **The owner's report, end to end at the nav layer: Create New World twice
    /// makes two worlds, both are listed, and either can be opened.**
    ///
    /// This is the acceptance condition for reading (2) and the
    /// regression gate for the wart reading (1) shipped with — *"Using Create New
    /// World just joins me to the existing world"*. Every step is the real screen
    /// flow (title → list → create → list), so it fails if any hop is unwired
    /// rather than only if `saves.rs` is wrong.
    /// **The whole delete flow, driven through the real screens**:
    /// title -> world list -> Delete -> the confirmation -> its affirmative
    /// control -> the world is gone and the others are not.
    ///
    /// This is the anti-island gate for the feature. Every hop is a production
    /// call (`nav.key`/`nav.click` on a `UiState`), so it fails if any of them is
    /// unwired rather than only if `saves::delete_world_in` is wrong — which its
    /// own tests already cover from the inside.
    ///
    /// The fixture is three worlds plus a **non-world directory** and a stray
    /// **file**, asserted as a precondition, because "it deleted the right one" is
    /// not a question a one-world root can ask and "it left everything else alone"
    /// is not one a root with only worlds in it can ask.
    #[test]
    fn deleting_a_world_removes_that_world_and_nothing_else() {
        use crate::menu::confirm::{NO_ROW, YES_ROW};
        use crate::menu::world_select::{FIRST_WORLD_ROW, WorldSelectButton as B};

        let (mut nav, _) = self::nav("delete-flow");
        let root = nav.saves_root().to_path_buf();
        for name in ["alpha", "bravo", "charlie"] {
            plant_world(&nav, name);
        }
        std::fs::create_dir_all(root.join("notaworld")).expect("create the non-world dir");
        std::fs::write(root.join(".DS_Store"), b"\x00").expect("write the stray file");

        let mut ui = UiState::new();
        // Reached the way a player reaches it.
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(ui.screen(), Screen::WorldSelect);
        assert_eq!(
            nav.world_select().shown_len(),
            3,
            "premise: three worlds, so 'the right one' is a real question"
        );
        assert!(
            root.join("notaworld").is_dir() && root.join(".DS_Store").is_file(),
            "premise: the root also holds a non-world directory and a stray file"
        );

        // Select `bravo` (row 1 under `cmp_for_list`: `plant_world` writes them
        // with the same `LastPlayed`, so the tie-break is folder name ascending).
        nav.click(&mut ui, FIRST_WORLD_ROW + 1);
        assert_eq!(
            nav.world_select().selected().map(|w| w.dir_name.clone()),
            Some("bravo".to_string())
        );

        // Delete **opens the confirmation and deletes nothing.**
        assert_eq!(nav.click(&mut ui, B::Delete.row()), MenuAction::None);
        assert_eq!(ui.screen(), Screen::Confirm);
        assert!(
            root.join("bravo").is_dir(),
            "pressing Delete must not remove anything by itself"
        );
        assert!(
            nav.confirm().message().contains("bravo"),
            "the confirmation must name the world it will remove: {:?}",
            nav.confirm().message()
        );
        assert_eq!(
            nav.confirm().focused_row(),
            None,
            "nothing is focused, so Enter here presses nothing"
        );
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert!(root.join("bravo").is_dir(), "Enter with no focus deleted a world");

        // Only the affirmative control deletes.
        assert_eq!(nav.click(&mut ui, YES_ROW), MenuAction::None);
        assert_eq!(ui.screen(), Screen::WorldSelect, "and it returns to the list");
        assert!(!root.join("bravo").exists(), "bravo must be gone");
        for kept in ["alpha", "charlie"] {
            assert!(root.join(kept).is_dir(), "{kept} must survive");
        }
        assert!(root.join("notaworld").is_dir(), "the non-world folder survives");
        assert!(root.join(".DS_Store").is_file(), "the stray file survives");
        assert!(root.is_dir(), "and the saves root itself survives");
        // The list was **re-read**, so the screen reflects the disk.
        let dirs: Vec<String> = nav
            .world_select()
            .worlds()
            .iter()
            .map(|w| w.dir_name.clone())
            .collect();
        assert_eq!(dirs, vec!["alpha".to_string(), "charlie".to_string()]);
        assert_eq!(nav.world_select().error(), None, "and no failure was reported");

        // -- controls: the three ways of saying no ---------------------------
        // Each one is run, and each must leave the world intact — an assertion of
        // absence, so each needs the affirmative arm above as its own control,
        // which it has.
        for (what, cancel) in [
            ("cancel button", 0usize),
            ("escape", 1),
            ("a click on nothing, then escape", 2),
        ] {
            let (mut nav, _) = self::nav(&format!("delete-flow-no-{cancel}"));
            let root = nav.saves_root().to_path_buf();
            plant_world(&nav, "alpha");
            plant_world(&nav, "keepme");
            let mut ui = UiState::new();
            nav.key(&mut ui, MenuKey::Enter);
            nav.click(&mut ui, FIRST_WORLD_ROW + 1);
            assert_eq!(
                nav.world_select().selected().map(|w| w.dir_name.clone()),
                Some("keepme".to_string()),
                "{what}: premise — `keepme` is the selection"
            );
            nav.click(&mut ui, B::Delete.row());
            assert_eq!(ui.screen(), Screen::Confirm, "{what}: premise — it opened");
            match cancel {
                0 => {
                    nav.click(&mut ui, NO_ROW);
                }
                1 => {
                    nav.key(&mut ui, MenuKey::Escape);
                }
                _ => {
                    // A click on a row this screen does not have — "clicking
                    // elsewhere" — then Escape.
                    nav.click(&mut ui, 99);
                    assert_eq!(ui.screen(), Screen::Confirm, "{what}: still up");
                    nav.key(&mut ui, MenuKey::Escape);
                }
            }
            assert_eq!(ui.screen(), Screen::WorldSelect, "{what}: back to the list");
            assert!(
                root.join("keepme").is_dir(),
                "{what} must leave the world intact"
            );
            assert!(root.join("alpha").is_dir(), "{what}: and the other one");
        }
    }

    /// A **corrupt** world can be removed, which is the safety property this test
    /// need — and it stays non-playable throughout.
    ///
    /// The fixture is the point: a directory with a `level.dat` that is not gzip
    /// at all, asserted undecodable as a precondition, because a *readable* world
    /// cannot exercise any of this.
    #[test]
    fn a_corrupt_world_can_be_deleted_and_never_played() {
        use crate::menu::confirm::YES_ROW;
        use crate::menu::world_select::{FIRST_WORLD_ROW, WorldSelectButton as B};

        let (mut nav, _) = self::nav("delete-corrupt-flow");
        let root = nav.saves_root().to_path_buf();
        plant_world(&nav, "aaa-readable");
        let broken = root.join("zzz-broken");
        std::fs::create_dir_all(&broken).expect("create the corrupt world dir");
        std::fs::write(
            lodestone_anvil::level_dat::path_in(&broken),
            b"this is not gzip",
        )
        .expect("write the corrupt level.dat");
        assert!(
            lodestone_anvil::level_dat::read_from_file(&lodestone_anvil::level_dat::path_in(
                &broken
            ))
            .is_err(),
            "premise: the level.dat must genuinely fail to decode"
        );

        let mut ui = UiState::new();
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(nav.world_select().shown_len(), 2, "both are listed");
        // Row 1 is the corrupt one — same `LastPlayed`, so folder name ascending.
        nav.click(&mut ui, FIRST_WORLD_ROW + 1);
        let selected = nav.world_select().selected().expect("a selection");
        assert_eq!(selected.dir_name, "zzz-broken");
        assert!(!selected.readable, "premise: the corrupt world is selected");
        assert!(
            !nav.world_select().is_active(B::Play.row()),
            "Play must stay greyed for a corrupt world"
        );
        assert!(nav.world_select().is_active(B::Delete.row()), "Delete must not");
        // Play does nothing even if something reaches it — the second guard.
        assert_eq!(nav.click(&mut ui, B::Play.row()), MenuAction::None);
        assert_eq!(ui.screen(), Screen::WorldSelect, "no launch");

        nav.click(&mut ui, B::Delete.row());
        assert_eq!(ui.screen(), Screen::Confirm);
        nav.click(&mut ui, YES_ROW);
        assert!(!broken.exists(), "a corrupt world must be removable");
        assert!(root.join("aaa-readable").is_dir(), "the readable one survives");
        assert_eq!(nav.world_select().error(), None);
    }

    /// A delete that the filesystem refuses is **reported over the world list**,
    /// not swallowed — vanilla raises `SystemToast.onWorldDeleteFailure` and this
    /// shell has no toast layer, so the list's own error line is where it goes.
    ///
    /// Driven by removing the directory behind the confirmation's back, which is
    /// the real race (another process, or Finder) rather than a fault injected
    /// into our own code.
    #[test]
    fn a_delete_the_filesystem_refuses_is_reported_on_the_world_list() {
        use crate::menu::confirm::YES_ROW;
        use crate::menu::world_select::WorldSelectButton as B;

        let (mut nav, _) = self::nav("delete-refused");
        let root = nav.saves_root().to_path_buf();
        plant_world(&nav, "vanishing");
        let mut ui = UiState::new();
        nav.key(&mut ui, MenuKey::Enter);
        nav.click(&mut ui, B::Delete.row());
        assert_eq!(ui.screen(), Screen::Confirm, "premise: it opened");
        // Gone behind our back.
        std::fs::remove_dir_all(root.join("vanishing")).expect("remove it first");
        nav.click(&mut ui, YES_ROW);
        assert_eq!(ui.screen(), Screen::WorldSelect);
        let err = nav
            .world_select()
            .error()
            .expect("a refused delete must say so");
        assert!(
            err.starts_with("Could not delete the world:"),
            "unexpected message: {err:?}"
        );

        // -- control ---------------------------------------------------------
        // A delete that succeeds sets **no** error, so the assertion above is
        // about the failure and not about a screen that always shows one.
        let (mut nav, _) = self::nav("delete-refused-control");
        plant_world(&nav, "present");
        let mut ui = UiState::new();
        nav.key(&mut ui, MenuKey::Enter);
        nav.click(&mut ui, B::Delete.row());
        nav.click(&mut ui, YES_ROW);
        assert_eq!(nav.world_select().error(), None);
    }

    #[test]
    fn creating_two_worlds_lists_both_and_play_opens_the_selected_one() {
        use crate::menu::create_world::{CREATE_ROW, NAME_LABEL};
        use crate::menu::world_select::{FIRST_WORLD_ROW, WorldSelectButton as B};

        let (mut nav, _) = self::nav("two-worlds");
        let mut ui = UiState::new();

        // Two creations, each with its own typed name, through the real buttons.
        let mut created: Vec<std::path::PathBuf> = Vec::new();
        for name in ["First", "Second"] {
            nav.key(&mut ui, MenuKey::Enter);
            assert_eq!(ui.screen(), Screen::WorldSelect, "premise: the list is open");
            assert_eq!(nav.click(&mut ui, B::Create.row()), MenuAction::None);
            assert_eq!(ui.screen(), Screen::CreateWorld);
            // Clear the `New World` default and type a real name. Name lives
            // on the Game tab, which is where a fresh screen already starts.
            let name_row = nav
                .create_world()
                .frame_row_for_focus_id(crate::menu::create_world::NAME_FIELD)
                .expect("Name is visible on the Game tab, the default");
            nav.click(&mut ui, name_row);
            for _ in 0..NAME_LABEL.len() + crate::menu::create_world::DEFAULT_NAME.len() {
                nav.key(&mut ui, MenuKey::Backspace);
            }
            type_str(&mut nav, &mut ui, name);
            let create_row = nav
                .create_world()
                .frame_row_for_focus_id(CREATE_ROW)
                .expect("Create is always visible, on every tab");
            let action = nav.click(&mut ui, create_row);
            let MenuAction::Singleplayer(_, SingleplayerLaunch::Created { world_dir, .. }) = action
            else {
                panic!("expected Created, got {action:?}");
            };
            created.push(world_dir);
            // The app would take over here; simulate coming back to the title the
            // way quitting to it does.
            ui = UiState::new();
        }
        assert_eq!(created.len(), 2);
        assert_ne!(
            created[0], created[1],
            "the second Create must make a **different** directory — this is the \
             whole defect: with one implicit world it reopened the first"
        );
        assert_eq!(
            created[0].file_name().and_then(|n| n.to_str()),
            Some("First")
        );
        assert_eq!(
            created[1].file_name().and_then(|n| n.to_str()),
            Some("Second")
        );

        // Both are on the list, re-read from disk by opening the screen.
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(ui.screen(), Screen::WorldSelect);
        let listed: Vec<String> = nav
            .world_select()
            .worlds()
            .iter()
            .map(|w| w.display_name.clone())
            .collect();
        assert_eq!(
            listed.len(),
            2,
            "both worlds must be listed after Create; got {listed:?}"
        );
        assert!(listed.contains(&"First".to_string()), "{listed:?}");
        assert!(listed.contains(&"Second".to_string()), "{listed:?}");

        // And **either** can be opened: click each row and check Play resolves to
        // that row's own directory.
        for row in 0..2 {
            assert_eq!(nav.click(&mut ui, FIRST_WORLD_ROW + row), MenuAction::None);
            let expected = nav
                .world_select()
                .world_at(row)
                .expect("row exists")
                .dir_name
                .clone();
            let action = nav.click(&mut ui, B::Play.row());
            let MenuAction::Singleplayer(_, SingleplayerLaunch::Open(dir)) = action else {
                panic!("expected Open, got {action:?}");
            };
            assert_eq!(
                dir.file_name().and_then(|n| n.to_str()),
                Some(expected.as_str()),
                "Play must open the row that is selected, not a fixed world"
            );
        }
    }

    /// **An untouched Seed field reaches the app as an empty string, not a
    /// sentinel** (queued patch, the random-seed half).
    ///
    /// `app.rs::parse_seed` already proves empty text resolves to a fresh
    /// random `i64` (`empty_seed_is_random_not_a_fixed_fallback`) rather than
    /// zero or a panic — that half of the contract lives with `parse_seed`
    /// and is not re-derived here. What is this layer's job, and unproven
    /// before this test, is that the screen hands `parse_seed` the *empty*
    /// string it actually collected rather than some default numeral: a
    /// `WorldCreationConfig::default()` with a `"0"` seed would compile,
    /// look plausible, and turn every "random" world into the same one.
    #[test]
    fn an_empty_seed_field_reaches_the_action_as_empty_text_not_a_default_number() {
        use crate::menu::create_world::CREATE_ROW;
        use crate::menu::world_select::WorldSelectButton as B;

        let (mut nav, _) = self::nav("create-world-empty-seed");
        let mut ui = UiState::new();
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(nav.click(&mut ui, B::Create.row()), MenuAction::None);
        assert_eq!(ui.screen(), Screen::CreateWorld, "premise: World Creation is open");

        // No click into the Seed field, no typing — Create is pressed with
        // the field exactly as `CreateWorldNav::new` left it.
        let create_row = nav
            .create_world()
            .frame_row_for_focus_id(CREATE_ROW)
            .expect("Create is always visible, on every tab");
        let action = nav.click(&mut ui, create_row);
        let MenuAction::Singleplayer(_, SingleplayerLaunch::Created { config, .. }) = action else {
            panic!("expected MenuAction::Singleplayer(Created {{ .. }}), got {action:?}");
        };
        assert_eq!(
            config.seed, "",
            "an untouched Seed field must reach the action as an empty string, matching \
             parse_seed's own random-means-empty branch, not \"0\" or any other default"
        );
    }

    /// **Play Selected World reaches the app**.
    ///
    /// This is the link that turns `MenuAction::Singleplayer` from a variant
    /// nothing produced into a button: without it the launcher `app.rs` holds is
    /// unreachable, which is this repo's dominant defect class. It stops at the
    /// action deliberately — `app.rs`'s `apply_menu_action` arm is what starts a
    /// server, and `lodestone-shell`'s
    /// `pressing_play_reaches_a_running_integrated_server` carries it the rest of
    /// the way.
    ///
    /// The screen must **not** change here: `begin_singleplayer` is what moves to
    /// `Screen::Connecting`, and a launch that cannot proceed needs to fail onto
    /// a screen the player recognises.
    /// Plant a world under `nav`'s own saves root, through the same codec
    /// production reads — the fixture has to be a file
    /// `crate::saves::list_worlds_in` can actually parse.
    fn plant_world(nav: &MenuNav, dir_name: &str) {
        let dir = nav.saves_root().join(dir_name);
        std::fs::create_dir_all(&dir).expect("create world dir");
        let level = lodestone_anvil::level_dat::LevelDat::for_new_world(
            dir_name,
            &lodestone_anvil::level_dat::Spawn::default(),
            0,
        );
        lodestone_anvil::level_dat::write_to_file(
            &level,
            &lodestone_anvil::level_dat::path_in(&dir),
        )
        .expect("write level.dat");
    }

    #[test]
    fn play_selected_world_asks_the_app_to_start_singleplayer() {
        use crate::menu::world_select::WorldSelectButton as B;
        let (mut nav, _) = self::nav("world-select-play");
        // A world has to exist for Play to be live at all — with an empty
        // `saves/` it is greyed, which is `updateButtonStatus(null)` and is
        // covered by `world_select.rs`'s own gates.
        plant_world(&nav, "planted");
        let mut ui = UiState::new();
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(ui.screen(), Screen::WorldSelect, "premise: the list is open");
        assert_eq!(
            nav.world_select().shown_len(),
            1,
            "premise: opening the screen enumerated the planted world"
        );

        // Destructured rather than compared against a whole constructed value:
        // the action now carries an `Entitlement`, which has no public
        // constructor, so a test cannot build the expected variant — which is
        // the ownership gate working as designed.
        let action = nav.click(&mut ui, B::Play.row());
        let MenuAction::Singleplayer(_, SingleplayerLaunch::Open(dir)) = action else {
            panic!("expected MenuAction::Singleplayer(_, Open(..)), got {action:?}");
        };
        assert_eq!(
            dir,
            nav.saves_root().join("planted"),
            "Play Selected World must ask the app to launch that world's directory"
        );
        assert_eq!(
            ui.screen(),
            Screen::WorldSelect,
            "the nav layer must not leave the list; `begin_singleplayer` does that"
        );

        // The keyboard path is the same action, not a second implementation.
        // **Two Tabs now, not one**: registration order is header → contents →
        // footer, so the planted world's row comes between the search field and
        // Play — which is exactly what `FIRST_WORLD_ROW`'s doc says the *ids* do
        // not tell you.
        let (mut nav, _) = self::nav("world-select-play-keys");
        plant_world(&nav, "planted");
        let mut ui = UiState::new();
        nav.key(&mut ui, MenuKey::Enter);
        nav.key(&mut ui, MenuKey::Tab);
        assert_eq!(
            nav.world_select().focused_row(),
            Some(crate::menu::world_select::FIRST_WORLD_ROW)
        );
        nav.key(&mut ui, MenuKey::Tab);
        assert_eq!(nav.world_select().focused_row(), Some(B::Play.row()));
        let action = nav.key(&mut ui, MenuKey::Enter);
        let MenuAction::Singleplayer(_, SingleplayerLaunch::Open(dir)) = action else {
            panic!("expected MenuAction::Singleplayer(_, Open(..)), got {action:?}");
        };
        assert_eq!(dir, nav.saves_root().join("planted"));
    }

    /// Typing on the world list goes into the search box, and Escape leaves.
    #[test]
    fn the_world_list_search_field_takes_text_and_escape_returns_to_the_title() {
        let (mut nav, _) = self::nav("world-select-keys");
        let mut ui = UiState::new();
        nav.key(&mut ui, MenuKey::Enter);
        type_str(&mut nav, &mut ui, "flat");
        assert_eq!(nav.world_select().search().value(), "flat");
        assert_eq!(nav.key(&mut ui, MenuKey::Escape), MenuAction::None);
        assert_eq!(ui.screen(), Screen::MainMenu);
        assert!(!ui.quit_requested(), "escape from the list is not a quit");
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
        // The roster is seeded before construction, on a writable path: this
        // test is about a failed *options* write, and an empty roster would
        // additionally close the ownership gate, swallowing every key before
        // the settings tree sees one.
        let seeded = nav_path("settingsfail");
        grant_ownership(&seeded);
        let mut nav = MenuNav::with_paths(
            seeded.parent().unwrap().join("servers.json"),
            std::path::PathBuf::from("/dev/null/nope/options.json"),
            seeded.parent().unwrap().join("profiles.json"),
        );
        let mut ui = UiState::new();
        ui.open_settings();
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Video);
        settings_row(&mut nav, &mut ui, is_option("guiScale"));
        nav.key(&mut ui, MenuKey::Enter);
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
        // This walk visits `OpenToLan`, which only a singleplayer session
        // offers (`MenuNav::open_to_lan_available`) — `enter_dev_world`'s
        // `kind = None` carries no session kind of its own, so the test states
        // its premise explicitly rather than relying on a stale default.
        nav.set_has_singleplayer_server(true);
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
        // Advancements is now live, so it is the first stop below
        // Back to Game.
        assert_eq!(nav.pause_button(), PauseButton::Advancements);
        nav.key(&mut ui, MenuKey::Down);
        // Statistics is live too.
        assert_eq!(nav.pause_button(), PauseButton::Statistics);
        nav.key(&mut ui, MenuKey::Down);
        // Player Reporting is live, so it appears before Options.
        assert_eq!(nav.pause_button(), PauseButton::PlayerReporting);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.pause_button(), PauseButton::Options);
        nav.key(&mut ui, MenuKey::Down);
        // Options' half-width sibling is the singleplayer-only LAN control.
        assert_eq!(nav.pause_button(), PauseButton::OpenToLan);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.pause_button(), PauseButton::QuitToTitle);
    }

    /// The published-world counterpart to
    /// `pause_menu_selection_wraps_both_ways` above: once
    /// `set_lan_published(true)` runs, Open to LAN is unreachable by keyboard
    /// (it is not merely skipped as a disabled row — it is not in the list at
    /// all, so `PAUSE_BUTTONS_PUBLISHED.len()` rows exist, not
    /// `PAUSE_BUTTONS.len()`), and hover/click follow the same shorter list.
    ///
    /// Walked explicitly, one assert per `Down`, the same shape as the
    /// unpublished walk above rather than a generic loop with hand-derived
    /// modular arithmetic — that keeps a wrong stop visible immediately
    /// instead of only in a final aggregate.
    #[test]
    fn once_published_the_pause_menu_skips_open_to_lan_entirely() {
        let (mut nav, _) = nav("pause-published-skip");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.pause();
        nav.set_lan_published(true);
        assert_eq!(nav.pause_buttons(), PAUSE_BUTTONS_PUBLISHED.as_slice());
        assert_eq!(nav.pause_button(), PauseButton::BackToGame);

        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.pause_button(), PauseButton::Advancements);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.pause_button(), PauseButton::Statistics);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.pause_button(), PauseButton::PlayerReporting);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.pause_button(), PauseButton::Options);
        nav.key(&mut ui, MenuKey::Down);
        // The discriminating step: the unpublished walk stops at Open to LAN
        // here. Published, it is not in the list to stop at, so Down lands
        // straight on Disconnect.
        assert_eq!(nav.pause_button(), PauseButton::QuitToTitle);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.pause_button(), PauseButton::BackToGame, "wraps");

        // Hover follows the same shorter list: the old last-row index (9,
        // `PAUSE_BUTTONS.len() - 1`) is out of range once published and must
        // be ignored, and the new last index (8) is Disconnect.
        let stale_last_index = PAUSE_BUTTONS.len() - 1;
        nav.hover(&ui, stale_last_index);
        assert_ne!(
            nav.pause_index(),
            stale_last_index,
            "the unpublished list's last index is out of range once published"
        );
        nav.hover(&ui, PAUSE_BUTTONS_PUBLISHED.len() - 1);
        assert_eq!(nav.pause_button(), PauseButton::QuitToTitle);
    }

    /// **The owner's report**: "Open to LAN" is shown while on a multiplayer
    /// server. `MenuNav::lan_published` alone cannot tell "singleplayer, not
    /// yet published" from "multiplayer, has nothing to publish" apart — both
    /// read `false`, which is exactly the state a fresh `MenuNav` starts in
    /// and a multiplayer session never leaves. `open_to_lan_available` is the
    /// fix: vanilla's own `hasSingleplayerServer()` conjunct, pushed in by
    /// `app::session::drive_ui_from_session` from `UiState::kind()`.
    #[test]
    fn open_to_lan_is_absent_on_a_never_flagged_singleplayer_session() {
        let mut nav = nav("pause-multiplayer-shape").0;
        // Neither setter called — the exact state a multiplayer session's
        // `MenuNav` is in every frame, since `set_has_singleplayer_server`
        // only ever pushes `true` for `SessionKind::Singleplayer`.
        assert!(
            !nav.open_to_lan_available(),
            "a session never confirmed singleplayer must not offer Open to LAN"
        );
        assert_eq!(
            nav.pause_buttons(),
            PAUSE_BUTTONS_PUBLISHED.as_slice(),
            "the multiplayer pause menu must take the same collapsed, no-LAN-row \
             shape a published singleplayer world does"
        );
        assert!(
            !nav.pause_buttons().contains(&PauseButton::OpenToLan),
            "Open to LAN must not be reachable at all on a multiplayer session"
        );

        // The positive control: flagging the session singleplayer (and still
        // unpublished) is what actually turns the row back on — proving the
        // assertions above are not vacuously true for every `MenuNav`.
        nav.set_has_singleplayer_server(true);
        assert!(nav.open_to_lan_available());
        assert!(nav.pause_buttons().contains(&PauseButton::OpenToLan));
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
        // BackToGame -> Advancements -> Statistics -> Player Reporting -> Options
        // The three middle stops are live and therefore are included in the walk.
        for _ in 0..4 {
            nav.key(&mut ui, MenuKey::Down);
        }
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
        // The full ten-row (unpublished) list is what this walk exercises —
        // see `MenuNav::open_to_lan_available`'s own doc.
        nav.set_has_singleplayer_server(true);
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
        // The full ten-row (unpublished) list is what `last` below assumes —
        // see `MenuNav::open_to_lan_available`'s own doc.
        nav.set_has_singleplayer_server(true);

        // Select the real Disconnect button first, so a fall-through would be
        // observable as a session teardown.
        let last = PAUSE_BUTTONS.len() - 1;
        nav.hover(&ui, last);
        assert_eq!(nav.pause_button(), PauseButton::QuitToTitle);

        // Now click Report Bugs (index 3, disabled). This used to probe
        // Advancements at index 1 is live; the subject has to be a
        // button that is genuinely still disabled or the test proves nothing.
        nav.hover(&ui, 3);
        assert_eq!(
            nav.pause_button(),
            PauseButton::ReportBugs,
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
        // widget. Both screens carry several disabled rows (pause's own count
        // contains five rows when Player Reporting is live;
        // live), so without this the arrow keys would walk through dead rows.
        let (mut nav, _) = nav("skip-disabled");
        let mut ui = UiState::new();

        // Title screen: Singleplayer, Multiplayer, Language, Accessibility,
        // Options — Realms and Friends are stepped over in both directions.
        // Language/Accessibility joined the walk once they were flipped live
        // (see `MainButton::Language`/`::Accessibility`'s own docs); `Accounts`
        // is not vanilla (see `MainButton::Accounts`) but is enabled too, one
        // step further than this walk goes.
        let mut seen = vec![nav.main_button()];
        for _ in 0..4 {
            nav.key(&mut ui, MenuKey::Down);
            seen.push(nav.main_button());
        }
        assert_eq!(
            seen,
            vec![
                MainButton::Singleplayer,
                MainButton::Multiplayer,
                MainButton::Language,
                MainButton::Accessibility,
                MainButton::Options,
            ]
        );
        for _ in 0..9 {
            nav.key(&mut ui, MenuKey::Up);
            assert!(
                nav.main_button().enabled(),
                "Up landed on {:?}, which is disabled",
                nav.main_button()
            );
        }

        // Pause screen: Back to Game, Advancements, Statistics, Player Reporting,
        // Options, Open to LAN, Disconnect — the three icon buttons in the middle
        // are the disabled rows Down must step over; the live entries are
        // Advancements, Statistics and Player Reporting, while Open to LAN is
        // session-gated.
        ui.enter_dev_world();
        ui.pause();
        // This walk visits `OpenToLan`, which only a singleplayer session
        // offers — see `MenuNav::open_to_lan_available`'s own doc.
        nav.set_has_singleplayer_server(true);
        let mut seen = vec![nav.pause_button()];
        for _ in 0..6 {
            nav.key(&mut ui, MenuKey::Down);
            seen.push(nav.pause_button());
        }
        assert_eq!(
            seen,
            vec![
                PauseButton::BackToGame,
                PauseButton::Advancements,
                PauseButton::Statistics,
                PauseButton::PlayerReporting,
                PauseButton::Options,
                PauseButton::OpenToLan,
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

    // -- the death screen -------------------------------------

    fn dead(nav_tag: &str) -> (MenuNav, UiState) {
        let (nav, _) = nav(nav_tag);
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.die(Some(vec![lodestone_model::text::InteractiveTextSpan {
            text: "blew up".to_string(),
            style: lodestone_model::TextStyle::default(),
            click: None,
            hover: None,
            insertion: None,
        }]));
        assert_eq!(ui.screen(), Screen::Death, "test setup did not reach Death");
        (nav, ui)
    }

    #[test]
    fn hovering_a_death_row_moves_the_highlight() {
        let (mut nav, ui) = dead("death-hover");
        assert_eq!(nav.death_index(), 0);
        nav.hover(&ui, 1);
        assert_eq!(nav.death_button(), DeathButton::TitleScreen);
        // Out-of-range rows are ignored rather than clamped, matching every
        // other screen's `hover`.
        nav.hover(&ui, 99);
        assert_eq!(nav.death_button(), DeathButton::TitleScreen);
    }

    #[test]
    fn death_screen_keyboard_navigation_wraps_between_the_two_buttons() {
        let (mut nav, mut ui) = dead("death-wrap");
        assert_eq!(nav.death_button(), DeathButton::Respawn);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.death_button(), DeathButton::TitleScreen);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(
            nav.death_button(),
            DeathButton::Respawn,
            "Down from the last row must wrap to the first"
        );
        nav.key(&mut ui, MenuKey::Up);
        assert_eq!(
            nav.death_button(),
            DeathButton::TitleScreen,
            "Up from the first row must wrap to the last"
        );
    }

    #[test]
    fn enter_on_respawn_asks_the_app_to_respawn_and_stays_on_the_death_screen() {
        let (mut nav, mut ui) = dead("death-respawn");
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::Respawn);
        // `UiState` only leaves `Screen::Death` once the server confirms the
        // respawn (`UiState::respawn_confirmed`, driven by `Sim::is_dead`
        // going false) — activating the button must not jump the gun.
        assert_eq!(
            ui.screen(),
            Screen::Death,
            "the screen must wait for the server's confirmation, not the click"
        );
    }

    #[test]
    fn enter_on_title_screen_leaves_for_the_main_menu() {
        let (mut nav, mut ui) = dead("death-title");
        nav.hover(&ui, 1);
        assert_eq!(nav.death_button(), DeathButton::TitleScreen);
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::QuitToTitle);
        assert_eq!(ui.screen(), Screen::MainMenu);
    }

    #[test]
    fn escape_does_nothing_on_the_death_screen() {
        // Vanilla's own death-screen should-close-on-esc check returns `false`
        // — unlike every other screen in this
        // file, Escape here must not even unwind one level, let alone quit.
        let (mut nav, mut ui) = dead("death-escape");
        assert_eq!(nav.key(&mut ui, MenuKey::Escape), MenuAction::None);
        assert_eq!(ui.screen(), Screen::Death);
        assert!(!ui.quit_requested());
    }

    // -- the credits/end-poem screen ------------------------------------

    fn on_credits(nav_tag: &str) -> (MenuNav, UiState) {
        let (nav, _) = nav(nav_tag);
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.show_credits();
        assert_eq!(
            ui.screen(),
            Screen::Credits,
            "test setup did not reach Credits"
        );
        (nav, ui)
    }

    #[test]
    fn enter_on_credits_leaves_for_the_main_menu() {
        let (mut nav, mut ui) = on_credits("credits-enter");
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::QuitToTitle);
        assert_eq!(ui.screen(), Screen::MainMenu);
    }

    #[test]
    fn escape_also_leaves_the_credits_screen() {
        // Unlike `Screen::Death` above, this screen has nothing to cancel
        // back out of — Escape and Enter mean the same thing, matching every
        // *other* present-and-final screen in this tree (`Screen::Error`'s
        // own `Escape | Enter` arm is the direct precedent).
        let (mut nav, mut ui) = on_credits("credits-escape");
        assert_eq!(nav.key(&mut ui, MenuKey::Escape), MenuAction::QuitToTitle);
        assert_eq!(ui.screen(), Screen::MainMenu);
    }

    #[test]
    fn up_and_down_do_nothing_on_the_credits_screen() {
        // One control, no cursor to move — see `key_credits`'s own doc for
        // why this does not chase vanilla's "any key" dismissal.
        let (mut nav, mut ui) = on_credits("credits-updown");
        assert_eq!(nav.key(&mut ui, MenuKey::Up), MenuAction::None);
        assert_eq!(nav.key(&mut ui, MenuKey::Down), MenuAction::None);
        assert_eq!(ui.screen(), Screen::Credits, "still on the screen");
    }

    #[test]
    fn a_click_on_the_only_row_dismisses_it_through_the_generic_hover_then_enter_path() {
        // Credits has no explicit arm in `MenuNav::click` — it relies on the
        // generic `hover` (a no-op here) then `key(Enter)` fallback, and this
        // is the test that would fail if that fallback ever stopped covering
        // it (e.g. a future screen-specific `click` arm added above it by
        // mistake).
        let (mut nav, mut ui) = on_credits("credits-click");
        assert_eq!(nav.click(&mut ui, 0), MenuAction::QuitToTitle);
        assert_eq!(ui.screen(), Screen::MainMenu);
    }

    // -- Social Interactions --------------------------------------------

    fn on_social(nav_tag: &str) -> (MenuNav, UiState) {
        let (mut nav, _) = self::nav(nav_tag);
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.pause();
        // Step to Player Reporting and press it — reproduces exactly what a
        // player does, rather than calling `ui.open_social_from_pause()`
        // directly, so this also proves the button click chain end to end.
        while nav.pause_button() != PauseButton::PlayerReporting {
            nav.key(&mut ui, MenuKey::Down);
        }
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(
            ui.screen(),
            Screen::Social,
            "test setup did not reach Social via the real button"
        );
        (nav, ui)
    }

    #[test]
    fn pressing_player_reporting_opens_social_with_a_fresh_cursor() {
        let (mut nav, mut ui) = on_social("social-open");
        // Move the cursor, leave, come back through the button again — must
        // not resume scrolled/selected where it was left, mirroring
        // `SettingsNav::reset`'s rule.
        nav.key(&mut ui, MenuKey::Down);
        ui.close_social();
        while nav.pause_button() != PauseButton::PlayerReporting {
            nav.key(&mut ui, MenuKey::Down);
        }
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(ui.screen(), Screen::Social);
        assert_eq!(nav.social().selected_row(), Some(0), "cursor reset to the top");
    }

    #[test]
    fn escape_leaves_social_for_the_pause_menu_not_the_title() {
        let (mut nav, mut ui) = on_social("social-escape");
        assert_eq!(nav.key(&mut ui, MenuKey::Escape), MenuAction::None);
        assert_eq!(ui.screen(), Screen::Paused);
    }

    #[test]
    fn done_also_leaves_social_for_the_pause_menu() {
        let (mut nav, mut ui) = on_social("social-done");
        // With no players in the roster, the only control is Done, at the
        // cursor already.
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert_eq!(ui.screen(), Screen::Paused);
    }

    #[test]
    fn a_disconnect_while_on_the_social_screen_reaches_error() {
        // Same reasoning as the death-screen disconnect gate: a session that
        // ends while this screen is open must not silently strand the player
        // on a roster from a server that is no longer there.
        let (_nav, mut ui) = on_social("social-disconnect");
        ui.session_failed(crate::sim::SessionEnd::disconnected(
            lodestone_model::ResolvedText::literal("connection lost"),
        ));
        assert_eq!(ui.screen(), Screen::Error);
    }

    // -- the multiplayer list's footer and row actions -----------------

    /// A nav on the multiplayer screen with `n` saved servers, and a canvas
    /// recorded so the position-dependent paths are reachable.
    fn listing(tag: &str, n: usize) -> (MenuNav, UiState, std::path::PathBuf) {
        let (mut nav, path) = self::nav(tag);
        let mut ui = UiState::new();
        ui.open_server_list();
        for i in 0..n {
            nav.key(&mut ui, MenuKey::Char('a'));
            type_str(&mut nav, &mut ui, &format!("S{i}"));
            nav.key(&mut ui, MenuKey::Tab);
            type_str(&mut nav, &mut ui, &format!("h{i}.example"));
            nav.key(&mut ui, MenuKey::Enter);
        }
        assert_eq!(ui.screen(), Screen::ServerList, "premise: the list is up");
        assert_eq!(nav.list().len(), n);
        (nav, ui, path)
    }

    /// Puts the cursor at `(x, y)` logical pixels on an 854×480 canvas, the way
    /// `app.rs`'s `menu_row_at` does.
    fn point_at(nav: &mut MenuNav, x: f32, y: f32) {
        nav.set_menu_cursor(x, y, 854.0, 480.0);
    }

    /// The centre of a quadrant of row `row`'s favicon, in logical pixels,
    /// unscrolled.
    fn icon_point(row: usize, fx: f32, fy: f32) -> (f32, f32) {
        let (ix, iy, iw, ih) = crate::menu::render::server_entry_icon_rect(row, 854.0, 0.0);
        (ix + iw * fx, iy + ih * fy)
    }

    /// The row indices `click_list` derives from `list.len()` are the ones
    /// `render::server_list_frame` builds, in the order it builds them. Same guard
    /// shape as `the_settings_rows_are_in_the_order_click_assumes`, and the same
    /// The row-order bug it protects against: nothing in the compiler links the two files.
    #[test]
    fn the_server_list_rows_are_in_the_order_click_assumes() {
        let (nav, ui, _) = listing("list-row-order", 2);
        let mut favicons = crate::menu::render::FaviconCache::new();
        let frame = crate::menu::render::frame_for(
            &ui,
            &nav,
            &crate::menu::status::StatusCache::with_probe(
                crate::menu::status::unavailable_probe(),
            ),
            &mut favicons,
        )
        .expect("the multiplayer screen owns its frame");

        assert_eq!(frame.rows.len(), 2 + SERVER_LIST_BUTTONS.len());
        for (i, entry) in nav.list().entries().iter().enumerate() {
            assert_eq!(frame.rows[i].label, entry.name, "row {i} is not entry {i}");
            assert!(frame.rows[i].entry.is_some(), "row {i} must be a list entry");
        }
        for (i, button) in SERVER_LIST_BUTTONS.iter().enumerate() {
            let row = &frame.rows[2 + i];
            assert_eq!(row.label, button.label(), "footer row {i} is not {button:?}");
            assert!(
                row.entry.is_none() && row.slot.is_some(),
                "a footer row is a slotted button, not a list entry"
            );
        }
    }

    /// Arrowing past the bottom of the scroll window scrolls to keep the
    /// selection on screen, and — the hit-testing half the issue calls out by
    /// name — `row_rect` (the same function `app.rs`'s hit-test calls) refuses
    /// a row that has scrolled out of the band, rather than reporting a rect
    /// for a row nothing draws there.
    #[test]
    fn arrowing_past_the_window_scrolls_and_off_window_rows_are_not_hit_testable() {
        let window = crate::menu::render::server_list_window_rows();
        let n = window + 3; // guaranteed to overflow the window
        let (nav, ui, _) = listing("list-scroll-keyboard", n);
        // `listing` adds through the real add-form path, which leaves the
        // cursor on the row it just created.
        assert_eq!(nav.server_index(), n - 1, "precondition: cursor on the last row");
        assert!(
            nav.server_scroll() > 0.0,
            "selecting a row past the window must have scrolled to show it"
        );

        let mut favicons = crate::menu::render::FaviconCache::new();
        let frame = crate::menu::render::frame_for(
            &ui,
            &nav,
            &crate::menu::status::StatusCache::with_probe(
                crate::menu::status::unavailable_probe(),
            ),
            &mut favicons,
        )
        .expect("the multiplayer screen owns its frame");

        const V_W: f32 = 854.0;
        const V_H: f32 = 480.0;

        // The control: the detector can return `Some` at all, on the row that
        // actually is on screen. Without this, "returns `None`" below would be
        // satisfied just as well by a `row_rect` that always answers `None`.
        let visible = crate::menu::render::row_rect(&frame.rows, n - 1, V_W, V_H);
        assert!(
            visible.is_some(),
            "control: the selected, on-screen row must still have a rect"
        );

        // The bug itself: row 0's `MenuRow` still exists in `frame.rows` —
        // nothing is windowed out of the vec — but it has scrolled above the
        // band. A hit-test that still answered a rect for it is exactly how a
        // stale click coordinate could select whatever is now drawn at row 0's
        // old pixels.
        let scrolled_off = crate::menu::render::row_rect(&frame.rows, 0, V_W, V_H);
        assert_eq!(
            scrolled_off, None,
            "a row scrolled out of the band must not be hit-testable"
        );
    }

    /// The mouse wheel scrolls the list too, independently of the
    /// keyboard path above, and clamps at both ends rather than running past
    /// the list.
    ///
    /// **Sign convention:** `notches` is winit's `scrollY` verbatim, so
    /// **positive scrolls up** — the same sign vanilla's
    /// `setScrollAmount(scrollAmount() - scrollY * scrollRate())` uses
    ///. This is the *opposite* of the `rows`
    /// parameter it replaced, where positive meant down.
    #[test]
    fn the_mouse_wheel_scrolls_the_server_list_and_clamps() {
        // `server_list_max_scroll` is dynamic (the real canvas, not the
        // conservative keyboard window), so pick `n` large enough that even
        // the *reference* 854×480 canvas cannot show it all — otherwise the
        // wheel would legitimately have nothing to do (`max == 0`) and the
        // clamp assertions below would hold vacuously.
        const V_H: f32 = 480.0;
        let n = 15;
        let (mut nav, _ui, _) = listing("list-scroll-wheel", n);
        let max = crate::menu::render::server_list_max_scroll(n, V_H);
        assert!(
            max > 0.0,
            "precondition: {n} rows must overflow an 854x480 canvas"
        );

        // Scroll to the very top and past it — must clamp at 0, never negative.
        nav.scroll_server_list(1000.0, V_H);
        assert_eq!(nav.server_scroll(), 0.0, "wheel-up clamps at the top");

        // Scroll to the very bottom and past it — must clamp at
        // `server_list_max_scroll`, not run off the end of the list.
        nav.scroll_server_list(-1000.0, V_H);
        assert_eq!(
            nav.server_scroll(),
            max,
            "wheel-down clamps at the bottom"
        );

        // One notch back up moves by exactly one *scroll rate* — half a row,
        // 18 px — not a whole entry. See the dedicated gate below.
        nav.scroll_server_list(1.0, V_H);
        assert_eq!(nav.server_scroll(), max - SCROLL_RATE_PX);
    }

    /// `scrollRate = defaultEntryHeight / 2` for the 36 px server row —
    /// vanilla's own abstract scroll-area base's default-settings call applied
    /// to `defaultEntryHeight / 2`
    ///, read back by `scrollRate()`
    /// and applied by `mouseScrolled`
    /// (`:34`). Transcribed from `.cache/mc/26.2/client-src`, not guessed.
    const SCROLL_RATE_PX: f32 = 18.0;

    /// **The player-reported bug, as a value rather than a
    /// direction.** The owner: *"scrolling the server list should actually
    /// scroll — not jump by increments of the height of a server entry."*
    ///
    /// One notch must land on **18 px** and three on **54 px**. That second
    /// number is the load-bearing one: 54 is not a multiple of the 36 px row
    /// height, so **no row index can represent it** — the assertion is
    /// unsatisfiable by the implementation this replaced, whatever else that
    /// implementation got right. Asserting merely that "the offset increased"
    /// would have passed both, which is the *magnitude* species of vacuous test
    /// `CLAUDE.md` names.
    ///
    /// The negative control is
    /// [`a_row_quantized_wheel_cannot_reach_the_predicted_offset`], which runs
    /// the old model and observes it fail exactly this predicate.
    #[test]
    fn three_wheel_notches_land_on_fifty_four_pixels() {
        const V_H: f32 = 480.0;
        let n = 15;
        let (mut nav, _ui, _) = listing("list-scroll-notch", n);
        // A precondition, not decoration: with `max_scroll` below 54 px the
        // clamp would answer these assertions instead of the notch rate, and
        // the gate would be measuring the wrong thing.
        let max = crate::menu::render::server_list_max_scroll(n, V_H);
        assert!(
            max >= 3.0 * SCROLL_RATE_PX,
            "precondition: {n} rows must leave room for three notches ({max} px of travel)"
        );
        // **A load-bearing precondition, and it caught itself.** `listing` adds
        // through the real add-form path, which leaves the cursor on the last row
        // — and `scroll_server_to_show` has therefore *already* scrolled the list
        // to the bottom. Without this the first assertion below measured 157.0,
        // an offset that is neither 18 nor 36 and would have read as a defect in
        // the notch rate rather than as the wrong starting point. Start from a
        // known top so the numbers below are the notch's own.
        nav.scroll_server_list(1000.0, V_H);
        assert_eq!(
            nav.server_scroll(),
            0.0,
            "precondition: the measurement must start from the top of the list"
        );

        // Negative `notches` is down, per `mouseScrolled`'s own sign.
        nav.scroll_server_list(-1.0, V_H);
        assert_eq!(
            nav.server_scroll(),
            SCROLL_RATE_PX,
            "one notch is half an entry, not a whole one"
        );

        nav.scroll_server_list(-1.0, V_H);
        nav.scroll_server_list(-1.0, V_H);
        assert_eq!(
            nav.server_scroll(),
            3.0 * SCROLL_RATE_PX,
            "three notches must reach 54 px — a position no row index can hold"
        );
        // Stated separately so a failure names the impossibility directly.
        assert_ne!(
            nav.server_scroll() % crate::menu::render::SERVER_LIST_ITEM_H,
            0.0,
            "54 px must not be a whole number of rows, or this gate has stopped \
             discriminating against the row-index model"
        );
    }

    /// The row-quantized wheel this replaced, kept **executable** so the gate
    /// above is a control rather than a description of one — the same discipline
    /// `widget.rs`'s `RowIndexList` uses for the primitive.
    ///
    /// This is `scroll_server_list(rows: i32, …)` and `server_row_top`'s
    /// `scroll as f32 * ITEM_H` as they actually were: one notch, one row.
    /// Observed: it lands on 36.0 and 108.0 where the real implementation lands
    /// on 18.0 and 54.0, so it **fails** both predicted values.
    #[test]
    fn a_row_quantized_wheel_cannot_reach_the_predicted_offset() {
        struct RowQuantizedWheel {
            rows: i32,
        }
        impl RowQuantizedWheel {
            /// The old handler: `app.rs` collapsed `dy` to ±1 and `nav.rs`
            /// added it to a row counter.
            fn wheel(&mut self, dy: f32) {
                let rows = if dy > 0.0 {
                    -1
                } else if dy < 0.0 {
                    1
                } else {
                    0
                };
                self.rows = (self.rows + rows).max(0);
            }
            fn scroll_px(&self) -> f32 {
                self.rows as f32 * crate::menu::render::SERVER_LIST_ITEM_H
            }
        }

        const V_H: f32 = 480.0;
        let n = 15;
        let (mut nav, _ui, _) = listing("list-scroll-control", n);
        let mut old = RowQuantizedWheel { rows: 0 };
        // Both models must start from the same place, or the `assert_ne!`s below
        // pass on the offset rather than on the granularity — see the sibling
        // gate's note on `listing` leaving the list scrolled to the bottom.
        nav.scroll_server_list(1000.0, V_H);
        assert_eq!(nav.server_scroll(), 0.0, "precondition: both start at 0");

        nav.scroll_server_list(-1.0, V_H);
        old.wheel(-1.0);
        assert_eq!(old.scroll_px(), 36.0, "the old model lands on a whole entry");
        assert_ne!(
            old.scroll_px(),
            nav.server_scroll(),
            "control must FAIL the one-notch prediction: 36 != 18"
        );

        nav.scroll_server_list(-1.0, V_H);
        nav.scroll_server_list(-1.0, V_H);
        old.wheel(-1.0);
        old.wheel(-1.0);
        assert_eq!(old.scroll_px(), 108.0, "three notches, three whole entries");
        assert_ne!(
            old.scroll_px(),
            nav.server_scroll(),
            "control must FAIL the three-notch prediction: 108 != 54"
        );
        // And the reason it cannot be fixed by scaling: every offset a row
        // counter can express is a multiple of the row height, so 54 is not in
        // its range at all.
        assert_eq!(
            old.scroll_px() % crate::menu::render::SERVER_LIST_ITEM_H,
            0.0,
            "a row counter can only ever land on a multiple of the row height"
        );
    }

    /// The scrollbar thumb is placed from **the same number the rows are** —
    /// `ServerEntryView::scroll`, which is `MenuNav::server_scroll()`.
    ///
    /// A thumb computed from its own expression is how a bar and its rows
    /// desynchronise, so this asserts the join rather than the arithmetic: after
    /// a wheel notch, the offset `render::server_scroll_model` clamps and the
    /// offset every row carries are one value, and `server_row_top` moves by
    /// exactly that many pixels.
    #[test]
    fn the_scrollbar_and_the_rows_read_the_same_offset() {
        const V_H: f32 = 480.0;
        let n = 15;
        let (mut nav, ui, _) = listing("list-scroll-join", n);
        // `listing` leaves the cursor on the last row, which has already
        // scrolled; start from a known top so the numbers below are the notch's.
        nav.scroll_server_list(1000.0, V_H);
        assert_eq!(nav.server_scroll(), 0.0, "precondition: scrolled to the top");
        let top_before = crate::menu::render::server_row_top(0, nav.server_scroll());

        nav.scroll_server_list(-1.0, V_H);
        let offset = nav.server_scroll();
        assert_eq!(offset, SCROLL_RATE_PX, "one notch of travel");

        let mut favicons = crate::menu::render::FaviconCache::new();
        let frame = crate::menu::render::frame_for(
            &ui,
            &nav,
            &crate::menu::status::StatusCache::with_probe(
                crate::menu::status::unavailable_probe(),
            ),
            &mut favicons,
        )
        .expect("the multiplayer screen owns its frame");

        // Every entry in the frame carries the offset the wheel produced — this
        // is the value `server_scroll_list` hands `ScrollList::set_scroll`, so
        // the thumb cannot be reading anything else.
        let carried: Vec<f32> = frame
            .rows
            .iter()
            .filter_map(|r| r.entry.as_ref().map(|e| e.scroll))
            .collect();
        assert_eq!(carried.len(), n, "every row must carry the offset");
        assert!(
            carried.iter().all(|s| *s == offset),
            "the offset the rows draw from must be the offset the wheel set: \
             {offset} vs {carried:?}"
        );

        // And the rows actually moved by it — a carried-but-ignored offset would
        // pass the assertion above and change nothing on screen.
        let top_after = crate::menu::render::server_row_top(0, offset);
        assert_eq!(
            top_before - top_after,
            SCROLL_RATE_PX,
            "row 0 must rise by exactly the offset, not by a whole row"
        );
    }

    /// **Hovering a server row does not select it.** Reported by a player: the
    /// 1 px row outline followed the mouse, so a server could not stay selected
    /// while the cursor travelled down to the Join button.
    ///
    /// Vanilla reaches `AbstractSelectionList.setSelected` only from `setFocused`
    /// and the click paths, never from
    /// hover — so this asserts hover is inert on rows *and* that click still
    /// works, because "hover does nothing" is also satisfied by a screen where
    /// nothing works at all.
    #[test]
    fn hovering_a_server_row_does_not_move_the_selection() {
        let (mut nav, mut ui, _) = listing("list-hover", 3);
        // Establish a known selection by clicking, rather than assuming one:
        // `listing` adds each server through the real add path, which highlights
        // the row it just created, so a 3-entry list arrives selected on row 2.
        let (cx, cy) = icon_point(0, 3.0, 0.5);
        point_at(&mut nav, cx, cy);
        nav.click(&mut ui, 0);
        assert_eq!(nav.server_index(), 0, "precondition: row 0 is selected");

        // Sweep the cursor across every row, including back to the start. Under
        // the old `hover_list` each of these moved the selection.
        for row in [1_usize, 2, 0, 2, 1] {
            nav.hover(&ui, row);
            assert_eq!(
                nav.server_index(),
                0,
                "hovering row {row} moved the selection; on a selection list only \
                 a click may do that"
            );
        }

        // The control: the same rows, clicked, *do* move it — so the assertion
        // above is measuring hover-versus-click and not a dead screen.
        for row in [1_usize, 2, 0] {
            let (bx, by) = icon_point(row, 3.0, 0.5);
            point_at(&mut nav, bx, by);
            nav.click(&mut ui, row);
            assert_eq!(
                nav.server_index(),
                row,
                "the control failed: a click on row {row} must select it, so the \
                 hover assertion above proves nothing"
            );
        }

        // And a selection survives the cursor leaving the rows entirely for the
        // footer, which is the exact motion the report was about.
        nav.hover(&ui, 0);
        nav.hover(&ui, nav.list().len()); // first footer button
        assert_eq!(
            nav.server_index(),
            0,
            "reaching for a footer button must not disturb the selected server"
        );
    }

    /// A click on a row **selects**; only the favicon's right half joins. That is
    /// `OnlineServerEntry.mouseClicked`'s order,
    /// and it is also the `MenuNav::click` hazard documented by the direct-click rule:
    /// translating a click into `Enter` here would connect on any click on any row.
    #[test]
    fn a_click_selects_a_row_and_only_the_join_icon_connects() {
        let (mut nav, mut ui, _) = listing("list-click", 2);

        // The row body: selection moves, nothing else happens.
        let (bx, by) = icon_point(1, 3.0, 0.5); // well to the right of the icon
        point_at(&mut nav, bx, by);
        assert_eq!(nav.click(&mut ui, 1), MenuAction::None, "a row click selects");
        assert_eq!(nav.server_index(), 1);
        assert_eq!(ui.screen(), Screen::ServerList, "and does not connect");

        // The icon's right half joins, and it is the *selected* row that goes.
        let (jx, jy) = icon_point(0, 0.75, 0.5);
        point_at(&mut nav, jx, jy);
        match nav.click(&mut ui, 0) {
            MenuAction::Connect(_, entry) => assert_eq!(entry.host, "h0.example"),
            other => panic!("the join icon must connect, got {other:?}"),
        }
        assert_eq!(nav.server_index(), 0, "and it selects the row it joined");
        assert_eq!(ui.screen(), Screen::Connecting);

        // With no cursor recorded at all — a click that arrived before any mouse
        // movement, and every keyboard-only path — the quadrants must not fire.
        let (mut nav, mut ui, _) = listing("list-click-nocursor", 2);
        assert_eq!(nav.click(&mut ui, 0), MenuAction::None);
        assert_eq!(ui.screen(), Screen::ServerList, "no cursor, no join");
    }

    #[test]
    fn a_double_click_on_the_row_body_joins_it() {
        // Player report (2026-08-04): vanilla's `if (doubleClick) join()`
        // fires wherever on the row the click landed
        // — but `click_list` used to
        // return early from `entry_icon_cursor` returning `None`/missing
        // every quadrant, before the double-click check ever ran, unless the
        // click happened to be inside the 32 px favicon. This point is well
        // clear of it, same as the "row body" case in the test above.
        let (mut nav, mut ui, _) = listing("list-dblclick", 2);
        let (bx, by) = icon_point(0, 3.0, 0.5);
        point_at(&mut nav, bx, by);
        assert_eq!(
            nav.click(&mut ui, 0),
            MenuAction::None,
            "the first click only selects"
        );
        assert_eq!(ui.screen(), Screen::ServerList);
        match nav.click(&mut ui, 0) {
            MenuAction::Connect(_, entry) => assert_eq!(entry.host, "h0.example"),
            other => panic!("a double-click on the row body must join, got {other:?}"),
        }
        assert_eq!(ui.screen(), Screen::Connecting);

        // The control: `DoubleClickTracker` only pairs *consecutive* clicks on
        // the *same* target, so a click on a different row in between must not
        // let the next click on row 0 count as its pair.
        let (mut nav, mut ui, _) = listing("list-dblclick-interrupted", 2);
        point_at(&mut nav, bx, by);
        assert_eq!(nav.click(&mut ui, 0), MenuAction::None);
        assert_eq!(
            nav.click(&mut ui, 1),
            MenuAction::None,
            "a different row resets the pair"
        );
        assert_eq!(
            nav.click(&mut ui, 0),
            MenuAction::None,
            "row 0 again, but not consecutively — must not join"
        );
        assert_eq!(
            ui.screen(),
            Screen::ServerList,
            "no genuine consecutive pair means no join"
        );
    }

    /// The move quadrants reorder the list, persist it, and carry the selection
    /// with the row — and each is refused at the end it cannot move toward.
    #[test]
    fn the_move_quadrants_reorder_the_list_and_persist_it() {
        let (mut nav, mut ui, path) = listing("list-move", 3);
        let names = |nav: &MenuNav| -> Vec<String> {
            nav.list().entries().iter().map(|e| e.name.clone()).collect()
        };
        assert_eq!(names(&nav), ["S0", "S1", "S2"]);

        // Row 2's top-left quadrant moves it up.
        let (ux, uy) = icon_point(2, 0.25, 0.25);
        point_at(&mut nav, ux, uy);
        assert_eq!(nav.click(&mut ui, 2), MenuAction::None);
        assert_eq!(names(&nav), ["S0", "S2", "S1"]);
        assert_eq!(nav.server_index(), 1, "the selection follows the row");
        // Persisted immediately, like every other list mutation here.
        assert_eq!(
            ServerList::load_from(&path)
                .entries()
                .iter()
                .map(|e| e.name.clone())
                .collect::<Vec<_>>(),
            ["S0", "S2", "S1"],
            "a reorder must survive a restart"
        );

        // Row 0's bottom-left quadrant moves it down.
        let (dx, dy) = icon_point(0, 0.25, 0.75);
        point_at(&mut nav, dx, dy);
        assert_eq!(nav.click(&mut ui, 0), MenuAction::None);
        assert_eq!(names(&nav), ["S2", "S0", "S1"]);
        assert_eq!(nav.server_index(), 1);

        // The guards: row 0 cannot move up, the last row cannot move down. Both
        // must leave the list *untouched* rather than clamping into some other
        // reorder — and the control is that the opposite quadrant on the same row
        // still works, which the two clicks above already showed.
        let before = names(&nav);
        let (ux, uy) = icon_point(0, 0.25, 0.25);
        point_at(&mut nav, ux, uy);
        assert_eq!(nav.click(&mut ui, 0), MenuAction::None);
        assert_eq!(names(&nav), before, "row 0 has nowhere to move up to");
        let last = nav.list().len() - 1;
        let (dx, dy) = icon_point(last, 0.25, 0.75);
        point_at(&mut nav, dx, dy);
        assert_eq!(nav.click(&mut ui, last), MenuAction::None);
        assert_eq!(names(&nav), before, "the last row has nowhere to move down to");
    }

    /// Each footer button does what its label says, and the two that cannot are
    /// refused. The indices are `list.len() + button`, which is what
    /// `the_server_list_rows_are_in_the_order_click_assumes` pins to the frame.
    #[test]
    fn the_footer_buttons_do_what_their_labels_say() {
        let button_row = |nav: &MenuNav, b: ServerListButton| {
            nav.list().len()
                + SERVER_LIST_BUTTONS
                    .iter()
                    .position(|x| *x == b)
                    .expect("in the table")
        };

        // Add opens the form; Back leaves the screen.
        let (mut nav, mut ui, _) = listing("list-buttons", 1);
        let row = button_row(&nav, ServerListButton::Add);
        assert_eq!(nav.click(&mut ui, row), MenuAction::None);
        assert_eq!(ui.screen(), Screen::ServerEdit);
        assert!(nav.form().editing.is_none(), "Add is a fresh form");
        ui.on_escape();

        let row = button_row(&nav, ServerListButton::Edit);
        assert_eq!(nav.click(&mut ui, row), MenuAction::None);
        assert_eq!(ui.screen(), Screen::ServerEdit);
        assert_eq!(nav.form().editing, Some(0), "Edit carries the selection");
        ui.on_escape();

        let row = button_row(&nav, ServerListButton::Select);
        match nav.click(&mut ui, row) {
            MenuAction::Connect(_, entry) => assert_eq!(entry.host, "h0.example"),
            other => panic!("Join Server must connect, got {other:?}"),
        }
        assert_eq!(ui.screen(), Screen::Connecting);

        // Refresh re-pings everything. Not `Reprobe(None)`, which would skip every
        // row that already has a result and make the button do nothing.
        let (mut nav, mut ui, _) = listing("list-refresh", 1);
        let row = button_row(&nav, ServerListButton::Refresh);
        assert_eq!(nav.click(&mut ui, row), MenuAction::RefreshList);
        assert_eq!(ui.screen(), Screen::ServerList, "and stays on the screen");

        // Delete removes the row and asks the app to forget its cached status.
        let row = button_row(&nav, ServerListButton::Delete);
        match nav.click(&mut ui, row) {
            MenuAction::Forget(gone) => assert_eq!(gone.host, "h0.example"),
            other => panic!("Delete must forget the row's status, got {other:?}"),
        }
        assert!(nav.list().is_empty(), "Delete must remove the row");

        // With the list now empty, the three conditional buttons are inactive and
        // a click on one must do **nothing** — vanilla's inactive
        // `AbstractWidget.mouseClicked` returns false.
        for b in [
            ServerListButton::Select,
            ServerListButton::Edit,
            ServerListButton::Delete,
            ServerListButton::Direct,
        ] {
            let row = button_row(&nav, b);
            assert_eq!(nav.click(&mut ui, row), MenuAction::None, "{b:?}");
            assert_eq!(ui.screen(), Screen::ServerList, "{b:?} must not navigate");
        }
        // Control: Add is active on the same empty list, so the four assertions
        // above measure `enabled` and not a dead `click`.
        let row = button_row(&nav, ServerListButton::Add);
        assert_eq!(nav.click(&mut ui, row), MenuAction::None);
        assert_eq!(ui.screen(), Screen::ServerEdit, "Add is still active");
        ui.on_escape();

        let row = button_row(&nav, ServerListButton::Back);
        assert_eq!(nav.click(&mut ui, row), MenuAction::None);
        assert_eq!(ui.screen(), Screen::MainMenu, "Back leaves the screen");
    }

    /// F5 refreshes, and hovering the footer moves a **second** cursor rather than
    /// the selection — which is what lets a selected row stay outlined while a
    /// button under the mouse highlights.
    ///
    /// This test used to assert that hovering row 1 *selected* row 1, which was
    /// the defect a player reported rather than a property worth keeping; see
    /// `hovering_a_server_row_does_not_move_the_selection`. Only the row-hover
    /// assertions changed — everything about F5 and the footer cursor is as it
    /// was, including that a row hover still clears the button cursor.
    #[test]
    fn f5_refreshes_and_hovering_the_footer_leaves_the_selection_alone() {
        let (mut nav, mut ui, _) = listing("list-f5", 2);
        let selected = nav.server_index();
        nav.hover(&ui, 1);
        assert_eq!(
            nav.server_index(),
            selected,
            "a row hover must not move the selection"
        );
        assert_eq!(nav.list_button(), None, "a row hover clears the button cursor");

        assert_eq!(nav.key(&mut ui, MenuKey::Refresh), MenuAction::RefreshList);
        assert_eq!(ui.screen(), Screen::ServerList);

        // Hovering a footer button.
        nav.hover(&ui, 2 + 3); // the fourth button, Edit
        assert_eq!(nav.list_button(), Some(3));
        assert_eq!(
            nav.server_index(),
            selected,
            "hovering a button must not move the selected server"
        );
        // Back onto a row clears the button cursor and *still* leaves the
        // selection where the last click put it.
        nav.hover(&ui, 0);
        assert_eq!(nav.list_button(), None);
        assert_eq!(nav.server_index(), selected);
        // A row index past every button is ignored rather than clamped.
        nav.hover(&ui, 99);
        assert_eq!(nav.list_button(), None);
        assert_eq!(nav.server_index(), selected);
    }

    /// F5 must not reach the edit form as text — the trap `MenuKey::Refresh`
    /// exists to avoid. Typing `r` there is a real keystroke; F5 is not.
    #[test]
    fn f5_is_not_text_in_the_edit_form() {
        let (mut nav, mut ui, _) = listing("list-f5-form", 0);
        nav.key(&mut ui, MenuKey::Char('a'));
        assert_eq!(ui.screen(), Screen::ServerEdit, "premise: the form is open");
        type_str(&mut nav, &mut ui, "home");
        nav.key(&mut ui, MenuKey::Refresh);
        assert_eq!(nav.form().name(), "home", "F5 must not type anything");
        assert_eq!(ui.screen(), Screen::ServerEdit, "and must not navigate");
    }

    // -- the mouse click path, at real coordinates (two player reports) -------

    /// `app.rs::menu_row_at`'s hit-test scan, verbatim, at `gui_scale == 1`
    /// (where the logical canvas is the framebuffer, so no `/ scale` applies).
    ///
    /// Reproduced here rather than restated: the *rects* come from
    /// `render::row_rect`, which is the same function the draw and `menu_row_at`
    /// both call, so a click coordinate in these tests is derived from the
    /// expression that draws the row and never from a copied constant. Only the
    /// `find` loop is duplicated.
    fn hit_test(
        frame: &crate::menu::render::MenuFrame<'_>,
        x: f32,
        y: f32,
        w: f32,
        h: f32,
    ) -> Option<usize> {
        (0..frame.rows.len()).find(|&i| {
            crate::menu::render::row_rect(&frame.rows, i, w, h).is_some_and(|(rx, ry, rw, rh)| {
                x >= rx && x <= rx + rw && y >= ry && y <= ry + rh
            })
        })
    }

    /// The centre of row `row`'s own rect, in logical pixels.
    fn row_centre(
        frame: &crate::menu::render::MenuFrame<'_>,
        row: usize,
        w: f32,
        h: f32,
    ) -> (f32, f32) {
        let (rx, ry, rw, rh) = crate::menu::render::row_rect(&frame.rows, row, w, h)
            .expect("the row under test must have a rect to click in");
        (rx + rw * 0.5, ry + rh * 0.5)
    }

    fn empty_statuses() -> crate::menu::status::StatusCache {
        crate::menu::status::StatusCache::with_probe(crate::menu::status::unavailable_probe())
    }

    /// A canvas wide enough for the settings grid's two 150 px columns at their
    /// vanilla pitch, and tall enough that `HeaderAndFooterLayout` puts the
    /// footer below the content band — i.e. an ordinary window.
    const CLICK_W: f32 = 854.0;
    const CLICK_H: f32 = 480.0;

    /// The **positive control** for the in-world test below: on the title-screen
    /// options tree, a click at a named row's own coordinates activates that row
    /// and no other.
    ///
    /// This is what proves the machinery in [`hit_test`]/[`row_centre`] can
    /// resolve a click at all, which matters because the in-world test's whole
    /// content is that the *same* rows became unreachable. Without this control,
    /// a hit-test that answered `None` everywhere would make that test pass for
    /// the wrong reason once it was fixed by any means.
    ///
    /// It also measures the hypothesis the bug was first attributed to, and
    /// disproves it: the options screen keeps its own entry-index window
    /// (`options::LIST_WINDOW_PX`, `visible_entries`, `Placement::ListCell`'s
    /// `first`) and never adopted the shared pixel-scrolled `ScrollList`, so a
    /// units mismatch between the two was the obvious suspect. It is not one —
    /// these coordinates resolve exactly.
    #[test]
    fn clicking_an_options_row_at_its_own_coordinates_activates_that_row() {
        let (mut nav, _p) = self::nav("options-click-coords");
        let mut ui = UiState::new();
        ui.open_settings();
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Video);
        let scale = settings_row(&mut nav, &mut ui, is_option("guiScale"));
        assert_eq!(nav.gui_scale(), 0, "precondition: the scale starts at auto");

        let mut favicons = crate::menu::render::FaviconCache::default();
        let statuses = empty_statuses();
        let frame = on_screen_frame(&ui, &nav, None, &statuses, &mut favicons)
            .expect("the title-screen options frame must exist");
        assert!(
            frame.rows[scale].label.starts_with("GUI Scale"),
            "premise: row {scale} is the GUI Scale row, not {:?}",
            frame.rows[scale].label
        );

        let (cx, cy) = row_centre(&frame, scale, CLICK_W, CLICK_H);
        assert_eq!(
            hit_test(&frame, cx, cy, CLICK_W, CLICK_H),
            Some(scale),
            "a click inside the GUI Scale row must resolve to that row"
        );
        assert_eq!(nav.click(&mut ui, scale), MenuAction::None);
        assert_eq!(nav.gui_scale(), 1, "and must cycle that row's own option");

        // The negative half: a coordinate in a *different* named row must
        // resolve to that other row, so the assertion above is row-resolution
        // and not "every coordinate answers `scale`".
        let vsync = frame
            .rows
            .iter()
            .position(|r| r.label.starts_with("VSync"))
            .expect("premise: the Video page has a VSync row");
        assert_ne!(vsync, scale, "premise: they are different rows");
        let (vx, vy) = row_centre(&frame, vsync, CLICK_W, CLICK_H);
        assert_eq!(hit_test(&frame, vx, vy, CLICK_W, CLICK_H), Some(vsync));

        // And a coordinate in the gap above the first row is on no row at all.
        assert_eq!(
            hit_test(&frame, CLICK_W * 0.5, 1.0, CLICK_W, CLICK_H),
            None,
            "the backdrop must not resolve to a row"
        );
    }

    /// **The player report**: "i cant click anything in the options menu".
    ///
    /// Options opened from the **pause menu** — the in-world case — could not be
    /// clicked at all, while the identical rows on the title screen worked
    /// perfectly (the test above). The cause was not geometry: `d096de8` made
    /// `render::frame_for` answer `None` for `Screen::Settings` whenever
    /// [`UiState::settings_in_world`], so the screen would draw as an overlay
    /// over the still-rendering world instead of replacing it with the title
    /// screen's panorama. `app.rs::menu_row_at` consulted `frame_for` with a
    /// `?`, so from that commit on it had **no rows to hit-test** here and every
    /// click returned before reaching one.
    ///
    /// The assertion below is deliberately not "clicking does something": it
    /// names the GUI Scale row, derives the coordinate from that row's own rect,
    /// and requires *that* option to have cycled — a partial fix that resolved
    /// clicks to the wrong row would fail it.
    #[test]
    fn in_world_options_clicks_reach_the_row_they_land_on() {
        let (mut nav, _p) = self::nav("options-click-in-world");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.pause();
        ui.open_settings_from_pause();
        assert_eq!(ui.screen(), Screen::Settings, "precondition: options is up");
        assert!(
            ui.settings_in_world(),
            "precondition: this is the in-world screen, not the title one"
        );
        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Video);
        let scale = settings_row(&mut nav, &mut ui, is_option("guiScale"));
        assert_eq!(nav.gui_scale(), 0, "precondition: the scale starts at auto");

        let mut favicons = crate::menu::render::FaviconCache::default();
        let statuses = empty_statuses();

        // The observed pre-fix failure, kept in the test rather than described:
        // the frame source `menu_row_at` used before the fix is **empty** here,
        // so its `?` bailed and no coordinate could resolve. This is the control
        // for the assertion that follows — it proves the row below was genuinely
        // unreachable, not merely reachable by a different route.
        assert!(
            crate::menu::render::frame_for(&ui, &nav, &statuses, &mut favicons).is_none(),
            "premise: `frame_for` still answers `None` in-world (that is what \
             makes it an overlay screen); if this ever becomes `Some`, the \
             overlay draw in `app/redraw.rs` is drawing the screen twice"
        );

        let frame = on_screen_frame(&ui, &nav, None, &statuses, &mut favicons)
            .expect("the in-world options screen must have a frame to hit-test");
        assert!(
            frame.rows[scale].label.starts_with("GUI Scale"),
            "premise: row {scale} is the GUI Scale row, not {:?}",
            frame.rows[scale].label
        );

        let (cx, cy) = row_centre(&frame, scale, CLICK_W, CLICK_H);
        let hit = hit_test(&frame, cx, cy, CLICK_W, CLICK_H);
        assert_eq!(
            hit,
            Some(scale),
            "a click inside the in-world GUI Scale row must resolve to that row"
        );
        assert_eq!(nav.click(&mut ui, hit.unwrap()), MenuAction::None);
        assert_eq!(
            nav.gui_scale(),
            1,
            "and must cycle GUI Scale — the row the click actually landed in"
        );
        assert!(
            nav.view_bobbing(),
            "and must not fall through to whatever Enter means on this screen"
        );
    }

    /// Every screen the mouse is allowed to route to must have somewhere for its
    /// rows to come from.
    ///
    /// This is the invariant whose violation was the report above:
    /// `render::owns_frame` (plus the pause/death overlays) decides where
    /// `app.rs` *routes* a click, and [`on_screen_frame`] decides where the rows
    /// come from. When the two disagree, the screen is live to the mouse and has
    /// no rows — which is silent, because nothing panics and no pixel changes.
    ///
    /// Bounded to the screens a `UiState` can be driven into here; it cannot see
    /// a future overlay screen that is never listed. That is why the list is
    /// spelled out per screen with its own setup rather than derived — a new
    /// overlay screen has to be added here, and the report above is the argument
    /// for doing it.
    ///
    /// This gate must use the production routing rule.
    ///
    /// The `routable` premise below used to be a **hand-copy** of the driver's
    /// `owns_frame(..) || is_paused() || is_death()`, not a call to it. So it
    /// tested two things this file controls against each other, and could not
    /// see the driver at all: `Screen::CommandBlockEdit` was absent from
    /// `on_screen_frame` *and* from the copied premise, which is a screen that
    /// silently never appears in `cases` rather than a failure. It now calls
    /// [`routes_menu_input`] — the same function `app/lifecycle.rs` guards on —
    /// so the premise is the production rule and a screen the driver routes to
    /// with no frame is a red test.
    ///
    /// It still cannot see whether `app.rs` hit-tests the frame *correctly*;
    /// that is `app/tests.rs`'s
    /// `clicking_a_command_block_row_at_its_own_coordinates_activates_that_row`.
    #[test]
    fn every_mouse_routable_screen_has_a_frame_to_hit_test() {
        let mut favicons = crate::menu::render::FaviconCache::default();
        let statuses = empty_statuses();

        let cases: Vec<(&str, fn(&mut UiState, &mut MenuNav))> = vec![
            ("MainMenu", |_ui, _nav| {}),
            ("ServerList", |ui, _nav| ui.open_server_list()),
            ("Settings-title", |ui, _nav| ui.open_settings()),
            ("Paused", |ui, _nav| {
                ui.enter_dev_world();
                ui.pause();
            }),
            // The one that was broken in `0d0ae93`.
            ("Settings-in-world", |ui, _nav| {
                ui.enter_dev_world();
                ui.pause();
                ui.open_settings_from_pause();
            }),
            ("Statistics", |ui, _nav| {
                ui.enter_dev_world();
                ui.pause();
                ui.open_statistics_from_pause();
            }),
            // The confirmation needs the `nav` half too: the frame is
            // built from `MenuNav::confirm`, so this drives the world list's own
            // Delete button rather than calling `ui.open_confirm()` — which also
            // makes it an anti-island premise (if Delete no longer opens the
            // screen, the setup fails rather than the assertion).
            ("Confirm", |ui, nav| {
                plant_world(nav, "alpha");
                nav.open_world_list(ui);
                assert_eq!(
                    nav.world_select().shown_len(),
                    1,
                    "premise: the world list enumerated the planted world"
                );
                nav.click(ui, crate::menu::world_select::WorldSelectButton::Delete.row());
                assert_eq!(
                    ui.screen(),
                    Screen::Confirm,
                    "premise: the world list's Delete button opens the confirmation"
                );
            }),
            // The command-block confirmation needs the `nav` half too — the
            // frame is built from `MenuNav::command_block`, so a `UiState` on
            // this screen with no widget state is not the production state.
            ("CommandBlockEdit", |ui, nav| {
                ui.enter_dev_world();
                nav.open_command_block(ui, command_block::CommandBlockOpen::default());
            }),
            // Same shape and same reason: the frame is built from
            // `MenuNav::sign_edit`, so a `UiState` on this screen with no
            // widget state is not the production state either.
            ("SignEdit", |ui, nav| {
                ui.enter_dev_world();
                nav.open_sign_edit(ui, sign_edit::SignEditOpen::default());
            }),
            // Same shape again — `EditBook` remainder. The frame
            // is built from `MenuNav::book_edit`, so a bare `UiState` on this
            // screen is equally not the production state.
            ("BookEdit", |ui, nav| {
                ui.enter_dev_world();
                nav.open_book_edit(
                    ui,
                    book_edit::BookEditOpen {
                        slot: 0,
                        pages: vec![String::new()],
                        author: "Steve".to_string(),
                    },
                );
            }),
            // The signed-book reader is the editable book screen's sibling,
            // but it still owns mouse input while rendering over the world.
            // Keep it explicit: omitting it from `routes_menu_input` makes
            // every arrow and Done click fall through to gameplay.
            ("BookView", |ui, nav| {
                ui.enter_dev_world();
                nav.open_book_view(
                    ui,
                    book_view::BookViewOpen::from_pages(
                        "Notes".to_string(),
                        "Steve".to_string(),
                        0,
                        &[
                            lodestone_model::text::Text::literal("first"),
                            lodestone_model::text::Text::literal("second"),
                        ],
                                            &|_| None,
),
                );
            }),
            // The sixth overlay screen — the frame is built from
            // `MenuNav::resource_pack_prompt`, so this drives
            // `show_resource_pack_prompt` rather than a bare
            // `ui.open_resource_pack_prompt()`, the same "not the production
            // state otherwise" reason `CommandBlockEdit`/`SignEdit` above give.
            ("ResourcePackPrompt", |ui, nav| {
                ui.enter_dev_world();
                nav.show_resource_pack_prompt(
                    ui,
                    &crate::net::PendingResourcePackPrompt::for_test(
                        uuid::Uuid::from_u128(1),
                        false,
                    ),
                );
            }),
            // The seventh overlay screen — `owns_frame == false`
            // unconditionally (see `Screen::ServerLinks`'s own doc), so
            // without `server_links_overlay_frame` in `on_screen_frame` every
            // click on it would otherwise be dropped exactly as an unframed overlay
            // click on the command block editor.
            ("ServerLinks", |ui, nav| {
                ui.enter_dev_world();
                ui.pause();
                ui.open_server_links_from_pause();
                let _ = nav;
            }),
        ];

        for (what, setup) in cases {
            let (mut nav, _p) = self::nav(&format!("routable-{what}"));
            let mut ui = UiState::new();
            setup(&mut ui, &mut nav);
            assert!(
                routes_menu_input(&ui),
                "{what}: premise — the driver routes menu input to this screen"
            );
            assert!(
                on_screen_frame(&ui, &nav, None, &statuses, &mut favicons).is_some(),
                "{what}: the mouse routes clicks to this screen but `on_screen_frame` \
                 has no frame for it, so every click is dropped before it reaches a row"
            );
        }
    }

    /// The three book controls are overlays, so their mouse path is only live
    /// if both the routing predicate and the screen-specific hover state agree.
    /// A signed book has no editable field to mask either omission: hovering a
    /// page arrow must light that arrow, and clicking it must turn the page.
    #[test]
    fn book_reader_buttons_route_hover_and_click_through_the_overlay_frame() {
        let (mut nav, _) = nav("book-reader-mouse");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        nav.open_book_view(
            &mut ui,
            book_view::BookViewOpen::from_pages(
                "Notes".to_string(),
                "Steve".to_string(),
                0,
                &[
                    lodestone_model::text::Text::literal("first"),
                    lodestone_model::text::Text::literal("second"),
                ],
                                    &|_| None,
),
        );

        assert!(routes_menu_input(&ui), "the reader owns overlay mouse input");
        nav.hover(&ui, book_view::page_row::NEXT);
        assert_eq!(
            book_edit_overlay_frame(&ui, &nav).map(|frame| frame.hovered),
            Some(Some(book_view::page_row::NEXT)),
            "hovering the next-page rect reaches the frame the renderer draws"
        );
        assert_eq!(
            nav.click(&mut ui, book_view::page_row::NEXT),
            MenuAction::None,
            "a hand-held book page turn is local"
        );
        assert_eq!(nav.book_view().unwrap().page_indicator(), (2, 2));

        nav.hover(&ui, book_view::page_row::DONE);
        assert_eq!(
            book_edit_overlay_frame(&ui, &nav).map(|frame| frame.hovered),
            Some(Some(book_view::page_row::DONE)),
            "Done also renders a hover state"
        );
        assert_eq!(nav.click(&mut ui, book_view::page_row::DONE), MenuAction::None);
        assert_eq!(ui.screen(), Screen::Playing, "Done closes the reader");
    }

    /// The writable-book editor shares the reader's arrows and Done geometry,
    /// but keeps a distinct state object. Keep its hover path covered too so a
    /// future reader-only fix cannot strand the editable book's controls.
    #[test]
    fn writable_book_buttons_record_hover() {
        let (mut nav, _) = nav("book-editor-hover");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        nav.open_book_edit(
            &mut ui,
            book_edit::BookEditOpen {
                slot: 0,
                pages: vec![String::new()],
                author: "Steve".to_string(),
            },
        );

        nav.hover(&ui, book_edit::page_row::DONE);
        assert_eq!(
            book_edit_overlay_frame(&ui, &nav).map(|frame| frame.hovered),
            Some(Some(book_edit::page_row::DONE)),
            "Done in the editable-book frame is hovered"
        );
    }

    /// **The control for the gate above, run and observed.**
    ///
    /// The gate asserts an implication, and an implication is satisfied for
    /// free by a premise that is never true. If `routes_menu_input` answered
    /// `true` for *everything* — a plausible way to make the gate pass — it
    /// would be worthless, so this pins the other direction: `Screen::Playing`
    /// and `Screen::Container` are live gameplay screens, the mouse there is
    /// look/attack and a container's own hit-test, and neither may be routed to
    /// the menu row path.
    ///
    /// `Screen::Container` is the sharper half: it *is* a screen with clickable
    /// rows, drawn as an overlay, and it has its own `hit_test_with_scale`
    /// path in `app/lifecycle.rs`. Adding it to `routes_menu_input` "for
    /// symmetry" would break every slot click, so it is here to make that a
    /// test failure rather than a discovery.
    #[test]
    fn gameplay_screens_are_not_routed_to_the_menu_row_path() {
        let mut ui = UiState::new();
        ui.enter_dev_world();
        assert!(
            !routes_menu_input(&ui),
            "Playing: gameplay input must not be swallowed by the menu layer"
        );

        ui.open_container();
        assert_eq!(ui.screen(), Screen::Container, "premise: the container is up");
        assert!(
            !routes_menu_input(&ui),
            "Container: has its own `hit_test_with_scale` path — routing it here \
             would break every slot click"
        );

        // And the command block screen must go back to `false` once it closes,
        // so this is a property of the screen and not a latch.
        let (mut nav, _p) = self::nav("routable-control");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        nav.open_command_block(&mut ui, command_block::CommandBlockOpen::default());
        assert!(routes_menu_input(&ui), "premise: open routes input");
        nav.close_command_block(&mut ui);
        assert!(
            !routes_menu_input(&ui),
            "and closing it must hand the mouse back to gameplay"
        );
    }

    // -- Statistics: nothing is focused until Tab (player report) -------------

    /// **The player report**: "the Statistics menu always has the 'Done' button
    /// focused for some reason".
    ///
    /// `stats::frame` set `selected: 0` on a frame whose only row *is* Done, so
    /// it was drawn focused the moment the screen opened. Vanilla focuses
    /// nothing: `Screen.setInitialFocus` runs its whole
    /// body only `if (this.minecraft.getLastInputType().isKeyboard())`, and this
    /// screen is reached by clicking the pause menu's Statistics button.
    /// `StatsScreen` does not override `setInitialFocus`, and even if the last
    /// input *had* been a keyboard, `StatsScreen.init` puts Done in
    /// `setTabOrderGroup(1)` behind the tab bar, so Done is not the first tab
    /// stop either.
    ///
    /// `usize::MAX` rather than an arbitrary out-of-range index: it is
    /// `MenuFrame::selected`'s own documented "highlights nothing" value, the
    /// same one the command-block frame uses.
    #[test]
    fn opening_statistics_focuses_nothing_and_tab_then_focuses_done() {
        let (mut nav, _p) = self::nav("stats-initial-focus");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.pause();
        ui.open_statistics_from_pause();
        assert_eq!(ui.screen(), Screen::Statistics, "precondition");

        let snapshot = crate::menu::stats::StatsSnapshot::default();
        let frame = crate::menu::stats::frame(nav.stats(), &snapshot);
        // Premise, restated after the tab bar landed: this screen used to carry
        // Done alone, and the assertion said so. It now carries Done plus the
        // three tab rows, so the old `rows.len() == 1` was measuring the absence
        // of a feature rather than anything this test is about. It failed loudly
        // on the day the tabs arrived, which is the whole reason to assert a
        // premise rather than assume it.
        assert_eq!(
            frame.rows.len(),
            1 + crate::menu::stats::TAB_LABELS.len(),
            "premise: Done plus the three tabs"
        );
        assert_eq!(
            frame.rows[crate::menu::stats::DONE_ROW].label,
            "Done",
            "premise: and Done is still row 0, which the focus assertions below \
             read through `DONE_ROW`"
        );
        assert!(
            frame.rows[crate::menu::stats::DONE_ROW].tab.is_none(),
            "premise: row 0 is the button, not a tab row"
        );
        assert_eq!(
            frame.selected,
            usize::MAX,
            "on open, nothing may be focused — a `0` here is Done, which is \
             precisely the reported bug"
        );

        // Enter must therefore do nothing: vanilla routes it to the *focused*
        // widget, and there is none. Escape is the screen's own handler and is
        // deliberately still unconditional, so there is always a way out.
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert_eq!(
            ui.screen(),
            Screen::Statistics,
            "Enter with nothing focused must not close the screen"
        );

        // Tab is `Screen.keyPressed`'s TabNavigation, and this screen has one
        // focusable child for it to land on.
        //
        // **A known divergence, stated rather than asserted away.** Vanilla's
        // `MenuTabBar` is itself focusable and sits in tab-order group 0, ahead
        // of the Done button's group 1 — so real vanilla's first Tab lands on
        // the tab bar, not on Done. `StatsNav` models focus as a single flag and
        // the tab rows are not focusable widgets here, so our first Tab reaches
        // Done directly. That gap is why this assertion reads `DONE_ROW` rather
        // than "whatever Tab focused": when the tab bar becomes focusable, this
        // line is the one that should fail.
        nav.key(&mut ui, MenuKey::Tab);
        let focused = crate::menu::stats::frame(nav.stats(), &snapshot);
        assert_eq!(
            focused.selected,
            crate::menu::stats::DONE_ROW,
            "Tab must focus Done"
        );
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::None);
        assert_eq!(
            ui.screen(),
            Screen::Paused,
            "and Enter on a focused Done must close back to the pause menu"
        );
    }

    /// Hover must not focus, and a click must — `ContainerEventHandler.
    /// mouseClicked` focuses the child it hit and then calls its `onClick`,
    /// while hover touches focus on no screen (the server-list report).
    ///
    /// Without this, gating Enter on focus would have broken clicking Done: the
    /// shared `click` fall-through is `hover` + `Enter`, and hover grants no
    /// focus, so Enter would have found nothing focused and done nothing. That
    /// is why `Screen::Statistics` gained its own `click` arm.
    #[test]
    fn hovering_statistics_focuses_nothing_but_clicking_done_closes_it() {
        let (mut nav, _p) = self::nav("stats-hover-vs-click");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.pause();
        ui.open_statistics_from_pause();
        let snapshot = crate::menu::stats::StatsSnapshot::default();

        nav.hover(&ui, crate::menu::stats::DONE_ROW);
        assert_eq!(
            crate::menu::stats::frame(nav.stats(), &snapshot).selected,
            usize::MAX,
            "hovering Done must not focus it"
        );
        assert_eq!(ui.screen(), Screen::Statistics, "nor activate it");

        // A row this screen does not have does nothing at all, rather than
        // falling through to whatever Enter means.
        assert_eq!(nav.click(&mut ui, 7), MenuAction::None);
        assert_eq!(ui.screen(), Screen::Statistics);

        assert_eq!(
            nav.click(&mut ui, crate::menu::stats::DONE_ROW),
            MenuAction::None
        );
        assert_eq!(
            ui.screen(),
            Screen::Paused,
            "but clicking Done focuses it and closes the screen"
        );
    }

    /// Re-entering Statistics must not arrive with Done still focused from last
    /// time — vanilla builds a fresh `StatsScreen` on every entry, which is the
    /// same rule `PauseButton::Statistics` already applies to the scroll offset.
    #[test]
    fn re_entering_statistics_starts_unfocused_again() {
        let (mut nav, _p) = self::nav("stats-refocus");
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.pause();
        ui.open_statistics_from_pause();
        nav.key(&mut ui, MenuKey::Tab);
        let snapshot = crate::menu::stats::StatsSnapshot::default();
        assert_eq!(
            crate::menu::stats::frame(nav.stats(), &snapshot).selected,
            crate::menu::stats::DONE_ROW,
            "premise: Tab focused Done"
        );
        nav.key(&mut ui, MenuKey::Escape);
        assert_eq!(ui.screen(), Screen::Paused, "premise: back at the pause menu");

        // Through the real pause-menu path, which is what calls `reset`.
        let target = PAUSE_BUTTONS
            .iter()
            .position(|b| *b == PauseButton::Statistics)
            .expect("the pause menu has a Statistics button");
        for _ in 0..=PAUSE_BUTTONS.len() {
            if nav.pause_index() == target {
                break;
            }
            nav.key(&mut ui, MenuKey::Down);
        }
        assert_eq!(nav.pause_index(), target, "premise: the cursor reached it");
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(ui.screen(), Screen::Statistics, "premise: re-opened");
        assert_eq!(
            crate::menu::stats::frame(nav.stats(), &snapshot).selected,
            usize::MAX,
            "a fresh entry must focus nothing again"
        );
    }

    /// **The island gate for `click`'s account-row arm**, and the discriminator
    /// for the interaction change: driven through `click` — the function
    /// `app.rs`'s mouse handler actually calls — because `AccountsNav::click_row`
    /// and `select_focused` being unit-tested proves nothing about whether
    /// anything calls them.
    ///
    /// Without the arm, `click` falls through to `hover` + `Enter`, and `Enter`
    /// on a list row **selects**. So the assertion that carries the whole change
    /// is the negative one after the first click: under the old behaviour that
    /// account was already selected and `profiles.json` already written.
    #[test]
    fn a_single_click_on_an_account_focuses_it_and_a_second_selects_it() {
        use crate::menu::accounts::AccountRow;
        use lodestone_auth::metadata::{AccountProfile, AccountsMetadata};

        let (_, path) = nav("accounts-dblclick");
        let dir = path.parent().expect("the temp path has a parent");
        let profiles = dir.join("profiles.json");
        // Seed three real accounts *before* the nav reads them, so the list has
        // more than the lone offline row — with one row, "the cursor moved to
        // the row I clicked" is true of every implementation, including one that
        // never moved it at all.
        std::fs::create_dir_all(dir).expect("temp dir");
        let mut meta = AccountsMetadata::default();
        for i in 0..3u64 {
            meta.upsert(AccountProfile {
                profile_id: uuid::Uuid::from_u128(u128::from(i) + 1),
                username: format!("p{i}"),
                skin_url: None,
                last_used: i,
            });
        }
        meta.save_to(&profiles).expect("seed profiles.json");
        let mut nav = MenuNav::with_paths(path.clone(), dir.join("options.json"), profiles.clone());
        let mut ui = UiState::new();
        ui.open_accounts();

        let target = match nav.accounts().rows().get(2) {
            Some(AccountRow::Account(p)) => p.profile_id,
            other => panic!("row 2 of 3 accounts + offline must be an account: {other:?}"),
        };
        assert_eq!(nav.accounts().highlighted(), 0, "precondition: cursor at 0");
        assert!(!nav.accounts().is_selected(target), "precondition: nothing selected");

        assert_eq!(nav.click(&mut ui, 2), MenuAction::None);
        assert_eq!(
            nav.accounts().highlighted(),
            2,
            "the click must aim Select/Remove/Delete at the row it landed on"
        );
        assert!(
            !nav.accounts().is_selected(target),
            "a single click selected the account -- `click`'s account-row arm is \
             missing, so the click was translated into hover + Enter"
        );

        assert_eq!(nav.click(&mut ui, 2), MenuAction::None);
        assert!(
            nav.accounts().is_selected(target),
            "the control failed: a second click did not select the focused row, \
             so the negative assertion above proves nothing"
        );
        assert_eq!(ui.screen(), Screen::Accounts, "neither click leaves the screen");

        // The pair must be genuinely consecutive *on this row*, and the tracker
        // is now keyed by screen as well, so a stray click elsewhere in the list
        // resets it rather than arming the next one.
        assert_eq!(nav.click(&mut ui, 0), MenuAction::None);
        assert_eq!(nav.click(&mut ui, 2), MenuAction::None);
        assert_eq!(
            nav.accounts().highlighted(),
            2,
            "row 2 is focused again, but not by a consecutive pair"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    /// **The island gate for `click`'s `Screen::Accounts` arm.**
    ///
    /// `AccountsNav::click_name_edit_row` is unit-tested directly, which proves
    /// nothing about whether `MenuNav::click` ever calls it — without the arm,
    /// this file's fall-through translates a click into `hover` + `Enter`, and
    /// `Enter` on the open editor **commits**. So clicking the text field to fix
    /// a typo saved the name, which is the same direct-click shape on a sixth screen.
    ///
    /// Driven through `click`, the function `app.rs`'s mouse handler calls, with
    /// the field row and the Done row measured separately: a gate that only
    /// clicked Done would pass with the arm deleted.
    #[test]
    fn clicking_the_offline_name_field_does_not_save_but_clicking_done_does() {
        use crate::menu::accounts::{NAME_EDIT_DONE_ROW, NAME_EDIT_FIELD_ROW};
        use crate::offline_identity::OfflineIdentity;

        // `unowned_nav`, so the offline row really is row 0 — this test's own
        // stated premise. The account screen is exempt from the ownership gate
        // (it is the only way through it), so an empty roster is reachable here
        // exactly as a player who has just launched would find it.
        let (mut nav, path) = unowned_nav("offline-name-click");
        let dir = path.parent().expect("the temp path has a parent");
        let offline_file = dir.join("offline.json");
        let mut ui = UiState::new();
        ui.open_accounts();

        // Open the editor the way a player does: the offline row is row 0 with no
        // accounts, and the third footer button is the affordance.
        {
            let accounts = nav.accounts();
            let list_len = accounts.rows().len();
            accounts.hover(list_len + crate::menu::accounts::BUTTON_REMOVE);
        }
        nav.key(&mut ui, MenuKey::Enter);
        assert!(
            nav.accounts().is_editing_name(),
            "precondition: the editor must be open"
        );
        type_str(&mut nav, &mut ui, "Notch");
        assert_eq!(
            nav.accounts()
                .name_edit_view()
                .expect("still editing")
                .edit
                .value(),
            // The field was seeded with the persisted name and then typed into,
            // so this is the default plus what was typed — asserted so the test
            // is not silently measuring an empty field.
            format!("{}Notch", crate::offline_identity::DEFAULT_USERNAME),
        );

        assert_eq!(
            nav.click(&mut ui, NAME_EDIT_FIELD_ROW),
            MenuAction::None,
            "a click on the field must not act"
        );
        assert!(
            nav.accounts().is_editing_name(),
            "clicking the text field saved the name and closed the editor — \
             `click`'s Screen::Accounts arm is missing, so the click was \
             translated into hover + Enter"
        );
        assert!(
            !offline_file.exists(),
            "clicking the field wrote {}",
            offline_file.display()
        );

        // Now Done. This is the control for the assertions above: without it,
        // "nothing was saved" is equally consistent with a commit path that never
        // works at all.
        assert_eq!(nav.click(&mut ui, NAME_EDIT_DONE_ROW), MenuAction::None);
        assert!(
            !nav.accounts().is_editing_name(),
            "the control failed: clicking Done did not close the editor"
        );
        assert_eq!(
            OfflineIdentity::load_from(&offline_file).username(),
            format!("{}Notch", crate::offline_identity::DEFAULT_USERNAME),
            "clicking Done did not persist the name to {}",
            offline_file.display()
        );
        assert_eq!(
            ui.screen(),
            Screen::Accounts,
            "neither click may leave the screen"
        );
        let _ = std::fs::remove_dir_all(dir);
    }
}
