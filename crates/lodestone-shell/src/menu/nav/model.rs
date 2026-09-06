use super::book_edit;
use super::command_block;
use super::focus;
use super::servers::ServerEntry;
use super::sign_edit;
use super::{Screen, UiState};
use lodestone_auth::Entitlement;

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
    /// **F5** — refresh the multiplayer list.
    ///
    /// Its own variant rather than a reuse of `Char('r')`, because it is a
    /// *function* key: on [`Screen::ServerEdit`] a `Char` is text, and mapping F5
    /// onto one would type an `r` into the address field. `focus::KEY_F5` is the
    /// GLFW refresh code used by the multiplayer list.
    Refresh,
    /// Ctrl/Cmd+A — select all text in the focused field
    /// (`focus::EDIT_SHORTCUT_MODIFIER` already picks Cmd on macOS, Ctrl
    /// elsewhere; see [`focus::KeyEvent::is_select_all`]).
    SelectAll,
    /// Ctrl/Cmd+C — copy the focused field's selection to the clipboard.
    Copy,
    /// Ctrl/Cmd+X — cut the focused field's selection to the clipboard.
    Cut,
    /// Ctrl/Cmd+V — paste the clipboard into the focused field, replacing any
    /// selection. `app.rs` produces this only when the shortcut modifier is
    /// held, so it can never collide with a plain `v` (see [`MenuKey::Char`]).
    Paste,
    /// A key the focused text field acts on that this enum has no abstract
    /// name for — caret motion (Left/Right/Home/End), with whatever modifiers
    /// were held.
    ///
    /// Every other variant here is an *abstraction*: the winit key is thrown
    /// away at [`super::super::app::menus`]'s boundary and rebuilt in
    /// [`focus::KeyEvent::from_menu_key`]. That is right for a key whose
    /// meaning depends on the screen (`Delete` deletes a server on the list
    /// and a character in a form), and it is exactly wrong for caret motion,
    /// because the modifiers *are* the meaning: Left, Shift+Left,
    /// Cmd/Ctrl+Left and Cmd/Ctrl+Shift+Left are four different edits and an
    /// abstract `Left` could only carry one of them. Vanilla has no
    /// abstraction here either — the text-field key handler switches on the raw
    /// GLFW code and reads the shift/control modifiers from the same event.
    ///
    /// So this variant carries the [`focus::KeyEvent`] whole, and
    /// `from_menu_key` hands it straight back. Only the screens that own a
    /// text field act on it; a list screen ignores it, which is what
    /// `EditBox`-less vanilla screens do with an arrow key that no widget
    /// consumed.
    Edit(focus::KeyEvent),
    /// A printable character: a command on the list, text in the form.
    Char(char),
}
/// The one thing the app must do as a result of a keypress.
/// Which world [`MenuAction::Singleplayer`] is about, and how it got there.
///
/// Both arms carry a **directory that already exists on disk**, which is the
/// property that makes this seam narrow: the menu owns "where is `saves/`" and
/// "make a folder called that" (it is the only layer that knows the root — see
/// [`crate::saves`]'s note on why tests get a temp one), and `app.rs` owns "start
/// a server against this directory". Neither has to know the other's rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SingleplayerLaunch {
    /// **Play Selected World**: open this existing world directory.
    ///
    /// No seed travels with it, and that is not an omission:
    /// `lodestone_server::region_source::resolve_world_seed` reads the world's
    /// **stored** seed and a requested one is a *creation* parameter it ignores.
    /// Passing one here would be a value that looks connected and is discarded —
    /// worse than none, because a reader would believe it.
    Open(std::path::PathBuf),
    /// **Create New World**: `world_dir` was just created by the menu (so the
    /// player's typed name is already in its `level.dat`), and `config` carries
    /// the typed **seed** for `app`'s `resolve_launch_seed`.
    ///
    /// The seed does travel here, and here it is honoured, because the directory
    /// is new and therefore has no `world_gen_settings.dat` yet — which is the
    /// which is why creating a fresh directory is the right fix for stale directory metadata
    /// rather than forcing a seed onto an existing world.
    Created {
        /// The directory the menu created.
        ///
        /// **Native-only.** A browser world is created by
        /// `IntegratedServer::open_in_memory` and has no directory, no `level.dat`
        /// and no `world_gen_settings.dat` — so there is nothing for this field to
        /// hold, and gating it is what lets the browser reach this variant at all.
        /// `app::session::begin_singleplayer` already computed `world_dir` under the
        /// same `cfg` before this existed, and `app::launch::launch_singleplayer`
        /// already took it as a `cfg`-gated parameter; this was the one link in that
        /// chain still demanding a path.
        #[cfg(not(target_arch = "wasm32"))]
        world_dir: std::path::PathBuf,
        /// What the player typed on the creation screen.
        config: crate::menu::create_world::WorldCreationConfig,
    },
}

/// Proof that this build may start its bundled singleplayer server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SingleplayerPermit {
    /// The normal build's proof that the local roster contains an owning
    /// account. Keeping the token in the action prevents another native entry
    /// path from accidentally bypassing the ownership gate.
    #[cfg(feature = "multiplayer")]
    Entitled(Entitlement),
    /// A build that cannot contact arbitrary servers may start its bundled
    /// in-memory server without asking for an online account.
    #[cfg(not(feature = "multiplayer"))]
    LocalBuild,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    /// Nothing to do; the menu handled it internally.
    None,
    /// Enter a singleplayer world: start the integrated server in-process
    /// against that world's directory and connect to it.
    ///
    /// Two producers, and the payload says which: [`Screen::WorldSelect`]'s
    /// **Play Selected World** ([`SingleplayerLaunch::Open`]) and
    /// [`Screen::CreateWorld`]'s **Create** ([`SingleplayerLaunch::Created`]).
    /// `app.rs`'s arm calls `begin_singleplayer`, which takes exactly what this
    /// variant carries.
    ///
    /// It used to carry `Option<WorldCreationConfig>` — `None` meaning "the one
    /// implicit world at `saves/world`". That was reading (1) and it
    /// is why Create New World could not create a second world; see
    /// [`crate::saves`]'s module doc.
    ///
    /// The world-list path supplies an existing directory for `Open`; the world-creation
    /// path supplies a fresh directory and configuration for `Created`. Keeping these
    /// payloads in the action makes the app's launch hand-off explicit.
    ///
    /// The leading [`SingleplayerPermit`] preserves the normal ownership gate
    /// while allowing a build with no multiplayer capability to enter its
    /// bundled local server without online credentials.
    Singleplayer(SingleplayerPermit, SingleplayerLaunch),
    /// Connect to this server (the app opens the session and shows Connecting).
    ///
    /// Carries an [`Entitlement`] because every remote session requires an
    /// owning account. This variant has no local-only authorization arm.
    Connect(Entitlement, ServerEntry),
    /// Shut the game down cleanly.
    Quit,
    /// The list changed or a re-ping was asked for: the app should refresh
    /// statuses. Carries the entry to (re-)probe, or `None` for "all of them".
    Reprobe(Option<ServerEntry>),
    /// A row was removed; drop its cached status. Carried separately from
    /// [`MenuAction::Reprobe`] so the app does not start a probe for an address
    /// that is no longer in the list.
    Forget(ServerEntry),
    /// The player asked for a **refresh** — F5 or the Refresh button.
    ///
    /// Distinct from [`MenuAction::Reprobe`]`(None)`, and the distinction is
    /// load-bearing rather than tidy: `Reprobe(None)` means "make sure every row
    /// has been probed", which `StatusCache::refresh` answers by *skipping* every
    /// address it already has a result for. A refresh that skipped every row is a
    /// button that does nothing. This one discards the cached results first — which
    /// is also what vanilla does, by throwing the whole screen away and building a
    /// new one with a fresh `ServerList`.
    RefreshList,
    /// The pause menu's "Quit to Title" was activated, or the
    /// death screen's "Title Screen" button was: [`UiState`] has already moved
    /// to [`Screen::MainMenu`] (see [`UiState::quit_to_title`]); the app must
    /// now tear down whatever live session (net connection and/or integrated
    /// server) is still attached to `Sim`, exactly as it would for an
    /// ordinary disconnect — nothing here does that on its own, since
    /// `MenuNav` holds no session state to tear down.
    QuitToTitle,
    /// Escape on [`Screen::Connecting`]: abandon a session that is still
    /// being established. Distinct from [`Self::QuitToTitle`], which leaves a
    /// session that is fully up — the screen each unwinds to differs, and this
    /// one can fire while the net thread is still inside its dial, so the app's
    /// teardown is what actually interrupts it.
    CancelConnect,
    /// The death screen's Respawn button was activated: the app
    /// must call `Sim::respawn` to submit the manual `ClientAction::Respawn`
    /// — `MenuNav` holds no `Sim` to send it through. [`UiState`] stays on
    /// [`Screen::Death`] until the server confirms the respawn (see
    /// `net::NetUpdate::Respawned`), so a duplicate click before that lands
    /// just resubmits the same request — harmless, since `Sim::respawn` is a
    /// no-op once `Sim::is_dead` has already gone false.
    Respawn,
    /// The command-block screen's **Done** button was activated:
    /// `app.rs` must send the `ClientAction::SetCommandBlock` this payload
    /// rebuilds. `MenuNav` holds no session to send it through, the same
    /// division of labour [`MenuAction::Respawn`] has.
    ///
    /// Carries a [`command_block::CommandBlockSubmit`] rather than a
    /// [`lodestone_client::ClientAction`] directly because this enum derives
    /// `Eq` and `ClientAction` cannot (a sibling variant holds a float) — see
    /// that struct's own doc. `app.rs`'s arm calls
    /// [`command_block::CommandBlockSubmit::into_action`] to cross back.
    ///
    /// **Nothing can open this screen from a real interaction yet**, and that is
    /// worth naming here rather than leaving for someone to rediscover: there is
    /// no command-block block-entity NBT decode in the workspace and no
    /// `interact.rs` trigger, so the screen has no production producer even
    /// though this variant now has a real consumer. The Done button computes a tested
    /// payload, and the app consumes it at the action boundary; retaining that boundary
    /// keeps command text from being collected and then dropped.
    SetCommandBlock(command_block::CommandBlockSubmit),
    /// The sign-editing screen closed — Done **or** Escape, both of which send
    /// (see [`Screen::SignEdit`]'s own doc): `app.rs` must submit the
    /// `ClientAction::SignUpdate` this payload rebuilds, the same division of
    /// labour [`MenuAction::SetCommandBlock`] has.
    ///
    /// Carries a [`sign_edit::SignEditSubmit`] rather than a `ClientAction`
    /// directly for the identical `Eq`-derive reason
    /// [`MenuAction::SetCommandBlock`]'s own doc gives. `app.rs`'s arm calls
    /// [`sign_edit::SignEditSubmit::into_action`] to cross back.
    SignUpdate(sign_edit::SignEditSubmit),
    /// The book-editing screen closed with something to send — Done (draft
    /// save, `title: None`) or Finalize (sign, `title: Some(..)`); Cancel and
    /// Escape never reach this variant (see [`Screen::BookEdit`]'s own doc).
    /// `app.rs` must submit the `ClientAction::EditBook` this payload
    /// rebuilds. Carries a [`book_edit::BookEditSubmit`] rather than a
    /// [`lodestone_model::ClientAction`] directly for the identical
    /// `Eq`-derive reason [`MenuAction::SetCommandBlock`]'s own doc gives.
    EditBook(book_edit::BookEditSubmit),
    /// Tell a server-owned lectern to show `button_id` as its selected page.
    /// The action only comes from a successful turn in [`Screen::BookView`],
    /// never from a hand-held book.
    ContainerButtonClick {
        /// The lectern's open container id.
        window_id: i32,
        /// The new zero-based page index.
        button_id: i32,
    },
    /// Close the server-owned lectern after Done or Escape. Hand-held books
    /// never produce this action because they have no container to close.
    CloseContainer {
        /// The open lectern container id this close applies to.
        window_id: i32,
    },
    /// The pause menu's **Open to LAN** was activated: the app must
    /// republish the world it is in on a TCP port so other machines can join.
    ///
    /// `MenuNav` cannot do it — it holds no `Sim` and no world path — which is the
    /// same division of labour [`MenuAction::Respawn`] and
    /// [`MenuAction::Singleplayer`] have. Carries nothing: the world to publish is
    /// whichever one the app already has open.
    OpenToLan,
    /// The resource-pack prompt was answered ([`Screen::ResourcePackPrompt`]).
    /// The app must call `NetClient::respond_to_resource_pack(id, accept)`
    /// through `Sim::net()` — `MenuNav` holds no `Sim`/`NetClient` to send it
    /// through, the same division of labour [`MenuAction::Respawn`] has.
    /// `accept = false` on a `required` pack additionally ends the session,
    /// but that decision is the net thread's own (see
    /// `net::NetClient::respond_to_resource_pack`'s doc) — this variant only
    /// carries the player's answer, not the consequence.
    ResourcePackResponse {
        /// The pack id this answers.
        id: uuid::Uuid,
        /// `true` for Accept, `false` for Decline.
        accept: bool,
    },
    /// The Spectator Menu reports a player-row activation (the teleport target is a
    /// `TeleportToEntity` value — see [`spectator_menu`]'s module doc):
    /// the app must send `ClientAction::TeleportToEntity { target }`.
    /// `MenuNav` holds no `Sim`/`NetClient` to send it through, the same
    /// division of labour [`MenuAction::ResourcePackResponse`] has. The
    /// screen has already closed by the time this is returned (see
    /// [`MenuNav::activate_spectator_menu_row`]).
    TeleportToEntity {
        /// Uuid of the player to teleport to.
        target: uuid::Uuid,
    },
}
