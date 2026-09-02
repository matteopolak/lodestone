//! The account list screen's brain: which Microsoft accounts
//! [`lodestone_auth::metadata::AccountsMetadata`] knows about, the synthetic
//! offline entry, and the device-code sign-in flow that adds a new one.
//!
//! ## What it is
//!
//! Mirrors [`super::servers`]/[`super::status`]'s split for the multiplayer
//! list: this module owns the *data* (the loaded metadata, which row is
//! highlighted, the in-flight sign-in state machine) and a background thread
//! per sign-in attempt; [`super::render`] turns it into a [`super::render::MenuFrame`]
//! and [`super::nav::MenuNav`] wires it into the screen state machine.
//!
//! ## Why interior mutability
//!
//! [`super::render::frame_for`] is called by `app.rs` **every frame** with a
//! plain `&MenuNav` — that is the one call site guaranteed to run regardless
//! of whether a key was pressed, which is what a live "waiting for you to
//! sign in…" poll needs to advance without a keystroke. `app.rs` is held by
//! another agent in this session, so a new per-frame hook there (the way
//! `app.rs` calls `StatusCache::pump` today) is not an option here.
//! [`AccountsNav::pump`] is instead written to work through a shared
//! reference (`RefCell` inside), so it can run from that existing call site
//! with **no `app.rs` change at all**.
//!
//! ## This screen is the ownership gate's only exit
//!
//! Nothing else in the client is reachable until [`AccountsNav::entitlement`]
//! answers `Some` — see `docs/accounts-and-join.md`. That makes this screen
//! load-bearing in a way it was not before: it is deliberately **exempt** from
//! the gate (blocking it would make the gate unopenable), so it is also the one
//! screen a player can be sitting on at the moment they become *un*entitled, by
//! removing their last account. [`super::nav::MenuNav::leave_accounts`] is what
//! handles that edge; both exits from this screen go through it.
//!
//! ## The offline entry
//!
//! [`lodestone_auth::metadata::AccountsMetadata::selected`] is `Option<Uuid>`
//! and cannot be extended with an "offline" sentinel without editing
//! `lodestone-auth` (owned elsewhere this session). This screen therefore
//! treats "no Microsoft account selected" as offline mode's own selected
//! state: the offline row shows the selection marker exactly when
//! `selected.is_none()`, and choosing it sets `selected = None` and saves.
//! That is indistinguishable from "nothing has ever been chosen" (a fresh
//! install), which is fine because both mean the same thing operationally —
//! play without a Microsoft session — but it does mean a future consumer that
//! wants to tell "never asked" apart from "explicitly offline" needs a schema
//! change in `lodestone-auth` this change does not make. See `docs/accounts.md`.
//!
//! ## The offline *name*, and why the third footer button changes identity
//!
//! [`crate::offline_identity`] landed the model, the persistence and the derived
//! UUID with **no editor** — `accounts_idle_frame` hardcoded the string
//! `"Play offline"`, so the one name every join in this client actually uses was
//! unreachable from the UI. This module now owns both halves: the offline row's
//! label *is* [`AccountsNav::offline_username`], and [`ThirdButton::EditName`]
//! opens a real [`EditBox`] over the screen.
//!
//! **The editor is not a fifth footer button, and that is a measurement rather
//! than a preference.** `render::ACCOUNTS_BUTTON_W` is 74 px with `spacing(4)`,
//! so four buttons measure `4 * 74 + 3 * 4 = 308` — which fits
//! [`crate::config::MIN_SCALED_WIDTH`]'s 320. A fifth would measure
//! `5 * 74 + 4 * 4 = 386` and hang 33 px off *each* edge at the smallest
//! supported GUI scale. Instead the third slot changes what it is: the offline
//! row **cannot be removed** (`remove_highlighted` refuses, and the button was
//! already drawn inactive whenever the cursor sat on it), so that slot is dead
//! space for exactly the row that needs an Edit affordance.
//! [`AccountsNav::third_button`] is the single expression both the label and
//! `activate_button` read, so the two cannot drift — the `BUTTON_*` ordering
//! coupling `accounts_idle_frame` documents applies to this as well.
//!
//! **Validation is not re-implemented here.** `set_username` is the validating
//! door and it guarantees the old name stays live when it refuses, so a rejected
//! edit shows [`crate::offline_identity::NameError`]'s own `Display` and keeps
//! the field open. Re-deriving the rule locally would be a second copy of
//! vanilla's `StringUtil.isValidPlayerName` to drift from the server's.
//!
//! ## Credentials never touch this screen
//!
//! The device-code flow's `user_code`/`verification_uri` are the only strings
//! this module ever displays; no field here can hold a password, and nothing
//! in [`SignIn`] or the worker message type carries one. Sign-in happens on
//! Microsoft's own page, in the user's browser.

use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::time::{SystemTime, UNIX_EPOCH};

use lodestone_auth::metadata::{AccountProfile, AccountsMetadata};
use uuid::Uuid;

use super::edit_box::EditBox;
use super::focus::KeyEvent;
use super::nav::MenuKey;
use crate::offline_identity::{OfflineIdentity, offline_uuid};

/// Rows of the account list visible at once. The list scrolls past this,
/// rather than the server list's current unbounded stack — see
/// `docs/main-menu.md`'s "left for polish" list, item 3, for why that one
/// still doesn't and why fixing it here first (a new screen, not shared
/// code) does not fix it there too.
///
/// **A count, not a measurement**, and that is the residual gap that fix records:
/// this module has no canvas, so it cannot ask how many 36 px rows actually fit
/// between the header and the footer. `render::accounts_row_visible` is the
/// second half — it refuses to *draw* a row that would overlap the footer band,
/// so a short canvas truncates the window rather than painting over the
/// buttons. What that leaves is bounded and the same shape the server list
/// already documents: `render::row_rect` still answers for a skipped row, so a
/// click there selects it and nothing else. Raising this number is only safe
/// once the window itself is canvas-derived.
pub const VISIBLE_ROWS: usize = 5;

/// One row of the account list: a real Microsoft account, or the synthetic
/// offline entry appended after them.
#[derive(Debug, Clone, PartialEq)]
pub enum AccountRow {
    /// A locally-known Microsoft account.
    Account(AccountProfile),
    /// The always-present offline entry (see the module docs).
    Offline,
}

/// The device-code sign-in flow's current state.
enum SignIn {
    /// Nothing in flight.
    Idle,
    /// A device code was requested; waiting for Microsoft to answer with a
    /// user code and verification URL.
    Requesting {
        rx: Receiver<WorkerMsg>,
        cancel: Arc<AtomicBool>,
    },
    /// Showing the user code and URL, polling for the user to finish in their
    /// browser.
    Waiting {
        user_code: String,
        verification_uri: String,
        rx: Receiver<WorkerMsg>,
        cancel: Arc<AtomicBool>,
    },
    /// The flow ended in an error; shown until dismissed.
    Failed { message: String },
}

impl std::fmt::Debug for SignIn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SignIn::Idle => write!(f, "Idle"),
            SignIn::Requesting { .. } => write!(f, "Requesting"),
            SignIn::Waiting {
                user_code,
                verification_uri,
                ..
            } => f
                .debug_struct("Waiting")
                .field("user_code", user_code)
                .field("verification_uri", verification_uri)
                .finish_non_exhaustive(),
            SignIn::Failed { message } => f.debug_struct("Failed").field("message", message).finish(),
        }
    }
}

/// A read-only snapshot of [`SignIn`] for the renderer — cheap to clone,
/// carries no channel or cancellation handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInView {
    /// Nothing in flight; the ordinary list + button screen shows.
    Idle,
    /// Waiting on Microsoft for the very first response.
    Requesting,
    /// Show the code and URL; sign-in is in progress in the user's browser.
    Waiting {
        /// The short code the user types at `verification_uri`.
        user_code: String,
        /// The URL the user visits to enter `user_code`.
        verification_uri: String,
    },
    /// The flow failed; `message` is the reason, already rendered to plain
    /// text by [`describe_auth_error`].
    Failed {
        /// Human-readable failure reason.
        message: String,
    },
}

/// What the background worker reports back over the channel.
enum WorkerMsg {
    /// Microsoft answered with a prompt to show the user.
    Prompt {
        user_code: String,
        verification_uri: String,
    },
    /// The full chain completed; the profile is ready to fold into metadata.
    /// The keychain save already happened on the worker thread (see
    /// [`run_browser_login`]/[`finish_ms_token`]) — only the metadata write is
    /// left, and that happens in [`AccountsNav::pump`] on the render thread,
    /// so every `profiles.json` write goes through one place.
    SignedIn(AccountProfile),
    /// A step in the chain failed. Already rendered to plain text.
    Failed(String),
    /// The user cancelled before completion.
    Cancelled,
}

/// A spawner for the background sign-in worker: real code hands in one that
/// starts a genuine OS thread against live Microsoft endpoints; tests hand in
/// one that just returns a channel they control by hand. Keeping this as an
/// injected closure is what makes [`AccountsNav::handle_key`]'s state
/// machine testable without a network.
type Spawn = Box<dyn FnOnce() -> (Receiver<WorkerMsg>, Arc<AtomicBool>)>;

/// Mutable state behind [`AccountsNav`]'s `RefCell` — see the module docs for
/// why this needs to be reachable through a shared reference.
struct State {
    metadata: AccountsMetadata,
    /// Index into the logical row list (`0..accounts.len()` are real
    /// accounts in [`AccountsNav::ordered`]'s order, `accounts.len()` is the
    /// offline entry) that Select/Remove act on. Distinct from `focus` so a
    /// mouse hovering a button does not forget which account was last
    /// highlighted — see [`AccountsNav::hover`].
    highlighted: usize,
    /// Index into the logical row list **plus** the four trailing button
    /// slots (`list_len + 0..=3`, see the `BUTTON_*` constants) that
    /// currently draws as hovered/selected. Reset to `highlighted` by every
    /// keyboard navigation, so the buttons are mouse-only for focus purposes
    /// — matching the server list's own letter-command buttons, which have
    /// no visual focus state at all today.
    focus: usize,
    /// The list's scroll offset, in **logical pixels** — `AbstractScrollArea`'s
    /// `scrollAmount`, exactly as [`super::nav::MenuNav::server_scroll`] carries
    /// the multiplayer list's.
    ///
    /// **Was a `usize` row index**, which is why this screen jumped a whole 36 px
    /// entry per keyboard step and could not have a continuous scrollbar thumb at
    /// all: a row counter can only ever land on a multiple of the row height, so
    /// no work downstream could recover an intermediate position. Every writer now
    /// goes through [`super::widget::ScrollList`], which owns the clamp and the
    /// `scrollRate = defaultEntryHeight / 2` notch.
    scroll: f32,
    save_error: Option<String>,
    sign_in: SignIn,
    /// The persisted "Play offline" identity — the name the offline row shows
    /// and the editor writes. Held here rather than re-read per frame because
    /// `frame_for` runs every frame and a file read per frame to draw one label
    /// is the shape `docs/accounts.md` already forbids for the keychain.
    identity: OfflineIdentity,
    /// The in-flight name edit, if any. `None` is the ordinary list screen.
    name_edit: Option<NameEdit>,
}

/// The offline-name editor while it is open.
struct NameEdit {
    /// The live widget — a real [`EditBox`], so the caret, the selection and the
    /// horizontal scroll are `edit_box.rs`'s arithmetic rather than restated.
    edit: EditBox,
    /// The last refusal from `OfflineIdentity::set_username`, already rendered
    /// through [`crate::offline_identity::NameError`]'s `Display`. `Some` means
    /// the *old* name is still the live one.
    error: Option<String>,
}

impl std::fmt::Debug for NameEdit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NameEdit")
            .field("value", &self.edit.value())
            .field("error", &self.error)
            .finish()
    }
}

/// A read-only snapshot of the name editor for the renderer.
#[derive(Debug, Clone, PartialEq)]
pub struct NameEditView {
    /// The live widget, cloned for the frame — `draw_edit_box` repositions the
    /// clone into the row's rect (see [`super::render::MenuRow::edit`]).
    pub edit: EditBox,
    /// Why the last attempt was refused, if it was.
    pub error: Option<String>,
    /// The UUID the **typed** name would join under, so the identity visibly
    /// changes with the name rather than only after a save. Derived, never
    /// stored — [`crate::offline_identity::offline_uuid`].
    pub uuid: Uuid,
}

/// What the account screen's **third** footer slot does right now.
///
/// Two labels for one slot, for the reason the module docs give: a fifth 74 px
/// button overflows [`crate::config::MIN_SCALED_WIDTH`], and the offline row can
/// never be removed, so `Remove` is dead for exactly the row `EditName` serves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThirdButton {
    /// Remove the highlighted Microsoft account.
    Remove,
    /// Open the offline-name editor (the highlighted row is the offline entry).
    EditName,
}

impl ThirdButton {
    /// The button's caption.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            ThirdButton::Remove => "Remove",
            ThirdButton::EditName => "Edit Name",
        }
    }
}

/// Account list + sign-in flow state for [`Screen::Accounts`](super::Screen).
pub struct AccountsNav {
    path: PathBuf,
    /// Where the offline identity is read and written — `offline.json` beside
    /// `path`'s `profiles.json`, so a test that points `path` at a temp
    /// directory cannot reach the developer's real file even by accident. This
    /// is the same structural defence `MenuNav` uses for its saves root.
    offline_path: PathBuf,
    state: RefCell<State>,
}

impl std::fmt::Debug for AccountsNav {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = self.state.borrow();
        f.debug_struct("AccountsNav")
            .field("path", &self.path)
            .field("accounts", &st.metadata.profiles.len())
            .field("highlighted", &st.highlighted)
            .field("sign_in", &st.sign_in)
            .field("offline_username", &st.identity.username())
            .field("name_edit", &st.name_edit)
            .finish()
    }
}

/// Index of the "Add account" row, relative to the end of the logical list
/// (real accounts + the offline entry).
pub const BUTTON_ADD: usize = 0;
/// Index of the "Select" row. See [`BUTTON_ADD`].
pub const BUTTON_SELECT: usize = 1;
/// Index of the "Remove" row. See [`BUTTON_ADD`].
pub const BUTTON_REMOVE: usize = 2;
/// Index of the "Cancel" row. See [`BUTTON_ADD`].
pub const BUTTON_CANCEL: usize = 3;
/// Number of trailing button rows after the account list.
pub const BUTTON_COUNT: usize = 4;

/// The name editor's text field, row 0 of `render::accounts_name_edit_frame`.
///
/// The two rows are constants for [`BUTTON_ADD`]'s reason: the frame's order and
/// what a click on each row does are two lists that must not drift.
pub const NAME_EDIT_FIELD_ROW: usize = 0;
/// The name editor's Done button, row 1. See [`NAME_EDIT_FIELD_ROW`].
pub const NAME_EDIT_DONE_ROW: usize = 1;

/// The longest offline name a server will accept — vanilla's
/// `StringUtil.isValidPlayerName` cap, the same 16 `char`s
/// [`crate::offline_identity::validate_username`] enforces.
///
/// Set on the [`EditBox`] as well as checked on commit, and the duplication is
/// deliberate: the cap stops the 17th keystroke rather than letting a player type
/// a long name and only learn on Done that it was refused. The commit check stays
/// because the box cap cannot see the *character* rule at all.
const NAME_MAX_LENGTH: usize = 16;

/// What [`AccountsNav::handle_key`] asks the screen-level caller to do. Kept
/// tiny and free of `UiState`/`MenuAction` so this module does not need to
/// know about either — `nav.rs`'s `key_accounts` is the only reader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountsSignal {
    /// Nothing to do beyond the internal state change already applied.
    None,
    /// Leave the screen (Cancel, or Escape with nothing in flight).
    Back,
}

impl AccountsNav {
    /// Loads metadata from the real on-disk location.
    #[must_use]
    pub fn new() -> Self {
        Self::with_path(lodestone_auth::paths::profiles_path())
    }

    /// Loads metadata from `path` — for tests, so nothing touches a
    /// developer's real `profiles.json`.
    ///
    /// The offline identity is loaded from `offline.json` **in `path`'s own
    /// directory**, never from [`crate::offline_identity::offline_identity_path`]:
    /// a test that hands in a temp `profiles.json` gets a temp `offline.json`
    /// with it, so the two cannot disagree about which root this screen is on.
    /// A `path` with no parent (a bare file name) falls back to the current
    /// directory, which is what `Path::parent` reports as `Some("")`.
    #[must_use]
    pub fn with_path(path: PathBuf) -> Self {
        let offline_path = path
            .parent()
            .unwrap_or(Path::new(""))
            .join("offline.json");
        Self {
            state: RefCell::new(State {
                metadata: AccountsMetadata::load_from(&path),
                highlighted: 0,
                focus: 0,
                scroll: 0.0,
                save_error: None,
                sign_in: SignIn::Idle,
                identity: OfflineIdentity::load_from(&offline_path),
                name_edit: None,
            }),
            path,
            offline_path,
        }
    }

    /// Every account, most-recently-used first — the order both the renderer
    /// and the index math in this module use, so they cannot drift apart.
    #[must_use]
    pub fn ordered(&self) -> Vec<AccountProfile> {
        ordered_profiles(&self.state.borrow().metadata)
    }

    /// The full logical row list: every account (see [`Self::ordered`]) plus
    /// the trailing offline entry.
    #[must_use]
    pub fn rows(&self) -> Vec<AccountRow> {
        let mut rows: Vec<AccountRow> = self.ordered().into_iter().map(AccountRow::Account).collect();
        rows.push(AccountRow::Offline);
        rows
    }

    /// The ownership proof this screen's roster supports, or `None` when no
    /// account has been added yet.
    ///
    /// This screen owns the loaded roster, so it is the one place that can
    /// answer, and every consumer of the answer goes through here rather than
    /// re-reading `profiles.json`: two readers of the same file are two answers
    /// that can disagree while an account is being added or removed.
    ///
    /// Note the value is **not** cached. Adding an account (through
    /// [`Self::pump`]) and removing one both mutate the roster in place, and a
    /// cached token would keep the gate open past the removal of the last
    /// account — which is precisely the state the gate exists to catch.
    #[must_use]
    pub fn entitlement(&self) -> Option<lodestone_auth::Entitlement> {
        lodestone_auth::Entitlement::from_metadata(&self.state.borrow().metadata)
    }

    /// Whether `selected` (the metadata's own field) points at nothing, i.e.
    /// offline mode is active — see the module docs.
    #[must_use]
    pub fn offline_selected(&self) -> bool {
        self.state.borrow().metadata.selected.is_none()
    }

    /// The persisted offline name — the offline row's own label.
    ///
    /// **A `String`, not a `&str`.** The brief that specified this asked for a
    /// borrow; the state lives behind a [`RefCell`] (see the module docs on why),
    /// and a reference cannot outlive the `Ref` guard, so a borrow here is not
    /// expressible without leaking the guard into the signature. Every sibling
    /// accessor on this type has the same shape for the same reason.
    #[must_use]
    pub fn offline_username(&self) -> String {
        self.state.borrow().identity.username().to_owned()
    }

    /// The UUID the persisted offline name joins under — the *saved* name, not a
    /// name being typed (that one is [`NameEditView::uuid`]).
    #[must_use]
    pub fn offline_uuid(&self) -> Uuid {
        self.state.borrow().identity.uuid()
    }

    /// What the third footer slot means for the row the cursor is on — see
    /// [`ThirdButton`] and the module docs.
    #[must_use]
    pub fn third_button(&self) -> ThirdButton {
        third_button(&self.state.borrow())
    }

    /// The open name editor, or `None` on the ordinary list screen.
    #[must_use]
    pub fn name_edit_view(&self) -> Option<NameEditView> {
        let st = self.state.borrow();
        st.name_edit.as_ref().map(|e| NameEditView {
            edit: e.edit.clone(),
            error: e.error.clone(),
            uuid: offline_uuid(e.edit.value()),
        })
    }

    /// Whether the name editor is open — the predicate `nav.rs` routes a click
    /// on this screen through, so a click on the field is caret placement rather
    /// than "save the form" (that fix's shape).
    #[must_use]
    pub fn is_editing_name(&self) -> bool {
        self.state.borrow().name_edit.is_some()
    }

    /// A click on rendered row `row` of the **name editor**'s frame.
    ///
    /// Only meaningful while [`Self::is_editing_name`]; the ordinary list screen
    /// still goes through `hover` + `Enter`. Row 0 is the field, which is always
    /// focused and therefore has nothing to move focus *to* — a no-op, exactly
    /// like the world list's search row. Row 1 is Done.
    pub fn click_name_edit_row(&self, row: usize) -> AccountsSignal {
        if row == NAME_EDIT_DONE_ROW {
            let mut st = self.state.borrow_mut();
            commit_name_edit(&mut st, &self.offline_path);
        }
        AccountsSignal::None
    }

    /// Whether `id` is the metadata's currently-selected account.
    #[must_use]
    pub fn is_selected(&self, id: Uuid) -> bool {
        self.state.borrow().metadata.selected == Some(id)
    }

    /// Index into [`Self::rows`] currently acted on by Select/Remove.
    #[must_use]
    pub fn highlighted(&self) -> usize {
        self.state.borrow().highlighted
    }

    /// Index into [`Self::rows`] **plus** the four button rows
    /// (`rows().len() + 0..=3`) currently drawn as hovered/focused.
    #[must_use]
    pub fn focus(&self) -> usize {
        self.state.borrow().focus
    }

    /// The list's scroll offset in **logical pixels** — see the field's own doc for
    /// why this is not a row index any more.
    #[must_use]
    pub fn scroll(&self) -> f32 {
        self.state.borrow().scroll
    }

    /// Scroll the list by `notches` of mouse wheel, at a `canvas_height`-tall
    /// canvas — vanilla's `AbstractScrollArea::mouseScrolled`.
    ///
    /// Delegates to [`super::widget::ScrollList::mouse_scrolled`] through
    /// [`super::render::accounts_list_spec`], the *same* expression the scrollbar
    /// draw and the keyboard cursor-follow go through, so one notch lands on
    /// `floor(36 / 2) = 18` px and the thumb cannot disagree with the rows.
    pub fn scroll_by(&self, notches: f32, canvas_height: f32) {
        let mut st = self.state.borrow_mut();
        let Some(mut list) =
            super::render::accounts_list_spec(list_len(&st), st.scroll).model(canvas_height)
        else {
            return;
        };
        list.mouse_scrolled(notches);
        st.scroll = list.scroll();
    }

    /// The last save failure, if any.
    #[must_use]
    pub fn save_error(&self) -> Option<String> {
        self.state.borrow().save_error.clone()
    }

    /// A read-only snapshot of the sign-in flow, for the renderer.
    #[must_use]
    pub fn sign_in_view(&self) -> SignInView {
        match &self.state.borrow().sign_in {
            SignIn::Idle => SignInView::Idle,
            SignIn::Requesting { .. } => SignInView::Requesting,
            SignIn::Waiting {
                user_code,
                verification_uri,
                ..
            } => SignInView::Waiting {
                user_code: user_code.clone(),
                verification_uri: verification_uri.clone(),
            },
            SignIn::Failed { message } => SignInView::Failed {
                message: message.clone(),
            },
        }
    }

    /// Drains any finished worker message and advances the sign-in state.
    /// Must be called every frame regardless of input — see the module docs
    /// on why this takes `&self`. Idempotent when nothing has arrived.
    pub fn pump(&self) {
        let mut st = self.state.borrow_mut();
        let current = std::mem::replace(&mut st.sign_in, SignIn::Idle);
        let (next, effect) = pump_locked(current);
        if let Some((url, profile)) = effect {
            if let Some(url) = url {
                open_in_browser(&url);
            }
            if let Some(profile) = profile {
                st.metadata.upsert(profile.clone());
                st.metadata.selected = Some(profile.profile_id);
                st.save_error = st.metadata.save_to(&self.path).err().map(|e| e.to_string());
                st.highlighted = 0;
                st.focus = 0;
                st.scroll = 0.0;
            }
        }
        st.sign_in = next;
    }

    /// Mouse hover over rendered row `rendered_row` (an index into whatever
    /// [`super::render::frame_for`] just built — i.e. **after** the scroll
    /// window and button rows are laid out, not [`Self::rows`]'s full list).
    pub fn hover(&self, rendered_row: usize) {
        let mut st = self.state.borrow_mut();
        // The sign-in and failure states draw a *different* frame — one wide
        // button, no list rows — so a row index there means nothing to the
        // mapping below, and applying it anyway would move the account cursor
        // when the player clicked "Cancel". `render::accounts_flow_frame` and
        // `accounts_failed_frame` are the frames in question.
        if !matches!(st.sign_in, SignIn::Idle) {
            return;
        }
        // The name editor draws a *different* frame too (a field and one wide
        // button, no list rows), so the same reasoning applies verbatim: a row
        // index there means nothing to the mapping below. `click_name_edit_row`
        // is that frame's click path.
        if st.name_edit.is_some() {
            return;
        }
        let list_len = list_len(&st);
        // **`rendered_row` *is* the logical row now, and this mapping is gone rather
        // than converted.** `accounts_idle_frame` emits every logical row and places
        // it by pixel offset instead of slicing `rows[scroll..scroll + shown]`, so
        // there is no window to map back through — which also removes the class of
        // bug that mapping could have: a `scroll` and a `shown` computed here from a
        // different canvas than the frame used would have silently aimed the hover one
        // row off. `render::row_rect` refuses a rect for a row outside the band, so
        // `menu_row_at` cannot hand us an off-screen row in the first place.
        if rendered_row < list_len {
            let logical = rendered_row;
            // **Only `focus`.** `focus` is what draws highlighted;
            // `highlighted` is what Select/Remove act on — vanilla's `hovered`
            // and `selected` (`:40`), which are
            // separate fields that nothing ever copies between.
            //
            // This line used to also write `st.highlighted = logical`, and that
            // was the reported bug: moving the mouse across the list silently
            // re-aimed Select and Remove at whatever the cursor last passed over,
            // so a player who highlighted an account with the keyboard and then
            // moved the mouse would sign in as a different one. Nothing about the
            // assignment looked wrong in isolation, which is why the guard is now
            // a test that reads *both* fields after a hover
            // (`hovering_an_account_does_not_change_what_select_acts_on`) —
            // a single assertion cannot tell "hover works" from "hover selected it".
            //
            // `super::widget::ScrollList` makes this structural rather than a
            // remembered rule: `set_hovered` has no path to `selected` at all.
            st.focus = logical;
        } else {
            let button = rendered_row - list_len;
            if button < BUTTON_COUNT {
                st.focus = list_len + button;
            }
        }
    }

    /// A **single** mouse click on rendered row `rendered_row`: move the
    /// cursor, and nothing else. Returns whether that row was a list row, so
    /// the caller knows whether a double-click is even meaningful for it.
    ///
    /// This is the "focus" half of the server list's interaction model, which
    /// this screen now shares: a click aims Select/Remove/Delete at the row you
    /// clicked, and **committing** the account switch takes a second click.
    /// Before this, `MenuNav::click` fell through to `hover` + `Enter` for this
    /// screen, so one click both moved the cursor *and* ran [`select`] —
    /// writing `profiles.json` on every stray click, and switching account on a
    /// click that was only meant to aim Remove.
    ///
    /// # Why it writes `highlighted` where [`Self::hover`] deliberately does not
    ///
    /// They are different events answering different questions. Hover must not
    /// touch `highlighted`, because moving the mouse across the list would
    /// silently re-aim Select and Remove at whatever the cursor last passed
    /// over — the reported bug that `hovering_an_account_does_not_change_what_select_acts_on`
    /// guards. A click is the opposite: vanilla's
    /// `AbstractSelectionList.mouseClicked` ends in `setSelected`, so a click
    /// *is* how the cursor moves. Leaving `highlighted` behind here would aim
    /// Remove at the keyboard's last row while the click visibly moved the
    /// highlight somewhere else.
    ///
    /// `false` for a button row and for the sign-in and name-editor frames,
    /// which draw no list at all — the caller keeps single-click activation for
    /// those, exactly as the server list's footer does.
    pub fn click_row(&self, rendered_row: usize) -> bool {
        let mut st = self.state.borrow_mut();
        // The same two guards [`Self::hover`] applies, for the same reason: the
        // sign-in, failure and name-editor frames draw one wide button and no
        // list rows, so a row index means nothing to the mapping below.
        if !matches!(st.sign_in, SignIn::Idle) || st.name_edit.is_some() {
            return false;
        }
        let list_len = list_len(&st);
        if rendered_row < list_len {
            st.focus = rendered_row;
            st.highlighted = rendered_row;
            return true;
        }
        let button = rendered_row - list_len;
        if button < BUTTON_COUNT {
            st.focus = list_len + button;
        }
        false
    }

    /// The **double**-click action: commit the focused row as the selected
    /// account and persist `profiles.json`.
    ///
    /// The same [`select`] the Enter key and the Select button reach, so the
    /// three cannot disagree about what selecting means or about which row it
    /// acts on. Silently does nothing for a non-list row, which cannot happen
    /// through the caller ([`Self::click_row`] returns `false` there) but keeps
    /// this safe to call on its own.
    pub fn select_focused(&self) {
        let mut st = self.state.borrow_mut();
        if !matches!(st.sign_in, SignIn::Idle) || st.name_edit.is_some() {
            return;
        }
        let list_len = list_len(&st);
        if st.focus >= list_len {
            return;
        }
        let logical = st.focus;
        st.highlighted = logical;
        select(&mut st, logical, &self.path);
    }

    /// Handles one key with a real worker spawn (a genuine background thread
    /// against live Microsoft endpoints). See [`Self::handle_key_with`] for
    /// the seam tests use instead.
    pub fn handle_key(&self, key: MenuKey) -> AccountsSignal {
        #[cfg(not(target_arch = "wasm32"))]
        let spawn: Spawn = Box::new(|| {
            let (tx, rx) = channel();
            let cancel = Arc::new(AtomicBool::new(false));
            let worker_cancel = Arc::clone(&cancel);
            // The **loopback** flow: it opens the real Microsoft login in the
            // user's browser and needs no code typed. There is no device-code
            // fallback for a headless host or a failed browser launch — a
            // prior `run_device_code_login` implemented that chain but had no
            // caller (its own doc claimed it was "kept as the fallback" while
            // nothing ever selected it), so it was dead code rather than a
            // real fallback and was removed. `open_in_browser`'s native arm
            // also silently discards a failed `spawn`, so wiring a real
            // fallback needs that failure surfaced first.
            std::thread::spawn(move || run_browser_login(tx, worker_cancel));
            (rx, cancel)
        });

        // Browser: there is no sign-in worker to spawn. Rather than gate the
        // keypress — which would make "Add account" do nothing at all, the
        // indistinguishable-from-broken outcome this screen is careful to avoid
        // everywhere else — feed the **real** state machine a pre-failed channel.
        // It transitions to `SignIn::Failed { message }`, which the account screen
        // already knows how to draw, so the player gets a sentence explaining why
        // instead of a dead button. This is the injected-`Spawn` seam being used
        // for exactly what its doc says it is for.
        //
        // Three things are missing and none is a shim away: `std::thread::spawn`
        // traps on wasm32, the flow needs a blocking `current_thread` runtime on the
        // one thread the browser paints with, and `lodestone_auth`'s `flow` /
        // `browser_login` / `store` modules are all `reqwest`- and keychain-based
        // and gated at their own crate. A browser sign-in would be a
        // `spawn_local` + `fetch` reimplementation of the whole chain, with
        // somewhere other than an OS keychain to put the refresh token.
        #[cfg(target_arch = "wasm32")]
        let spawn: Spawn = Box::new(|| {
            let (tx, rx) = channel();
            let _ = tx.send(WorkerMsg::Failed(
                "Microsoft sign-in is not available in the browser build: it needs \
                 an OS keychain for the refresh token and a blocking HTTP client. \
                 Play offline, or use the native client."
                    .to_owned(),
            ));
            (rx, Arc::new(AtomicBool::new(false)))
        });

        self.handle_key_with(key, spawn)
    }

    /// The real state machine, parameterised over how "Add account" spawns
    /// its worker — see [`Spawn`]. Kept as a normal (non-pub) method so
    /// production code always goes through [`Self::handle_key`]; tests reach
    /// it directly with a hand-fed channel.
    fn handle_key_with(&self, key: MenuKey, spawn: Spawn) -> AccountsSignal {
        let mut st = self.state.borrow_mut();
        let list_len = st.metadata.profiles.len() + 1;

        // **The name editor comes first, and it swallows every key.** Without
        // this ordering `Delete` would reach `remove_highlighted` while the
        // player was deleting a character, and `Up`/`Down` would move the list
        // cursor out from under a field they cannot see.
        if st.name_edit.is_some() {
            return handle_key_editing_name(&mut st, key, &self.offline_path);
        }
        if matches!(st.sign_in, SignIn::Requesting { .. } | SignIn::Waiting { .. }) {
            return handle_key_mid_flow(&mut st, key);
        }
        if matches!(st.sign_in, SignIn::Failed { .. }) {
            if matches!(key, MenuKey::Escape | MenuKey::Enter) {
                st.sign_in = SignIn::Idle;
            }
            return AccountsSignal::None;
        }

        match key {
            MenuKey::Up => {
                st.highlighted = wrap_prev(st.highlighted, list_len);
                st.focus = st.highlighted;
                scroll_to_show(&mut st);
                AccountsSignal::None
            }
            MenuKey::Down => {
                st.highlighted = wrap_next(st.highlighted, list_len);
                st.focus = st.highlighted;
                scroll_to_show(&mut st);
                AccountsSignal::None
            }
            MenuKey::Enter => {
                if st.focus < list_len {
                    let logical = st.focus;
                    // A click **does** select — `AbstractSelectionList.mouseClicked`
                    // ends in `setSelected` (`ObjectSelectionList.java` plus
                    // `AbstractSelectionList.java`). Only *hover* does not.
                    //
                    // `highlighted` has to move with it, or the hover fix opens a
                    // new gap in the other direction: a click reached here through
                    // `MenuNav::click`'s `hover` + `Enter` fall-through,
                    // so with hover no longer writing
                    // `highlighted`, clicking account 3 would sign in as 3 while
                    // leaving Remove and the Delete key aimed at whatever the
                    // keyboard last highlighted. That is the same class of bug as
                    // the one being fixed, so it is closed in the same change.
                    st.highlighted = logical;
                    select(&mut st, logical, &self.path);
                    AccountsSignal::None
                } else {
                    let button = st.focus - list_len;
                    activate_button(&mut st, button, &self.path, spawn)
                }
            }
            MenuKey::Delete => {
                remove_highlighted(&mut st, &self.path);
                AccountsSignal::None
            }
            MenuKey::Escape => AccountsSignal::Back,
            _ => AccountsSignal::None,
        }
    }
}

impl Default for AccountsNav {
    fn default() -> Self {
        Self::new()
    }
}

fn ordered_profiles(meta: &AccountsMetadata) -> Vec<AccountProfile> {
    let mut v = meta.profiles.clone();
    v.sort_by(|a, b| b.last_used.cmp(&a.last_used));
    v
}

fn select(st: &mut State, logical: usize, path: &Path) {
    let accounts_len = st.metadata.profiles.len();
    let ordered = ordered_profiles(&st.metadata);
    st.metadata.selected = if logical < accounts_len {
        ordered.get(logical).map(|p| p.profile_id)
    } else {
        None
    };
    st.save_error = st.metadata.save_to(path).err().map(|e| e.to_string());
}

fn remove_highlighted(st: &mut State, path: &Path) {
    let accounts_len = st.metadata.profiles.len();
    if st.highlighted >= accounts_len {
        // The offline entry cannot be removed.
        return;
    }
    let ordered = ordered_profiles(&st.metadata);
    let Some(profile) = ordered.get(st.highlighted) else {
        return;
    };
    let id = profile.profile_id;
    // Deleting the keychain entry is a real, explicit user action (not a
    // per-frame render), so it is allowed to touch the keychain — see
    // `docs/accounts.md`'s rule this screen otherwise follows by never
    // reaching `AccountSecrets` just to draw a row.
    // Native-only: `AccountSecrets` is the OS keychain, gated at
    // `lodestone-auth`. A browser has no keychain and therefore no stored refresh
    // token to delete — and no way to have acquired one, since the sign-in workers
    // below are native-only too. Removing the `profiles.json` row (immediately
    // after) is the whole operation there, which is correct rather than partial.
    #[cfg(not(target_arch = "wasm32"))]
    {
    let secrets = lodestone_auth::AccountSecrets::open();
    if let Err(e) = secrets.delete_refresh_token(id) {
        st.save_error = Some(format!("could not remove the stored credential: {e}"));
        return;
    }
    // The cached Minecraft session (`lodestone_auth::store::CachedSession`)
    // lives under a separate keychain entry from the refresh token — see
    // that module's doc for why — so removing an account has to clear it
    // explicitly too, or a re-added account that happens to land on the same
    // profile UUID would start from a stale (if still unexpired) cached
    // session belonging to the removed one. Best-effort, same as the
    // refresh-token delete above: `delete_session` is idempotent, and not
    // blocking the account removal on this succeeding matches how the rest
    // of this crate treats the session cache as an optimisation, never a
    // correctness dependency.
    if let Err(e) = secrets.delete_session(id) {
        tracing::warn!(profile = %id, error = %e, "could not remove the cached session for this account");
    }
    }
    st.metadata.remove(id);
    st.save_error = st.metadata.save_to(path).err().map(|e| e.to_string());
    let list_len = st.metadata.profiles.len() + 1;
    st.highlighted = st.highlighted.min(list_len - 1);
    st.focus = st.highlighted;
    scroll_to_show(st);
}

/// `path` is `profiles.json`. **No `offline.json` path is taken**, deliberately:
/// opening the editor writes nothing, so the only place `offline_path` is needed
/// is [`commit_name_edit`] — one writer, reached from exactly two call sites
/// (Enter and the Done click), both of which hold `AccountsNav`'s own field.
fn activate_button(st: &mut State, button: usize, path: &Path, spawn: Spawn) -> AccountsSignal {
    match button {
        BUTTON_ADD => {
            let (rx, cancel) = spawn();
            st.sign_in = SignIn::Requesting { rx, cancel };
            AccountsSignal::None
        }
        BUTTON_SELECT => {
            let h = st.highlighted;
            select(st, h, path);
            AccountsSignal::None
        }
        // **One slot, two verbs** — see [`third_button`], which is the single
        // expression the label and this arm share.
        BUTTON_REMOVE => {
            match third_button(st) {
                ThirdButton::Remove => remove_highlighted(st, path),
                ThirdButton::EditName => begin_name_edit(st),
            }
            AccountsSignal::None
        }
        BUTTON_CANCEL => AccountsSignal::Back,
        _ => AccountsSignal::None,
    }
}

/// What the third footer slot means for the currently-highlighted row.
///
/// The **one** expression both the button's caption
/// (`render::accounts_idle_frame`) and [`activate_button`]'s `BUTTON_REMOVE` arm
/// read. `highlighted >= profiles.len()` is the offline row — the same test
/// [`remove_highlighted`] refuses on and the same one that used to draw the
/// button inactive, so the button that was dead for that row is now the one that
/// does the useful thing.
fn third_button(st: &State) -> ThirdButton {
    if st.highlighted >= st.metadata.profiles.len() {
        ThirdButton::EditName
    } else {
        ThirdButton::Remove
    }
}

/// Opens the editor on the *persisted* name, caret at the end.
///
/// Seeded from the stored value rather than left empty: an editor that starts
/// blank reads as "your name has been cleared", and the overwhelmingly common
/// edit is a change to an existing name rather than a fresh one.
fn begin_name_edit(st: &mut State) {
    // Geometry is a placeholder: `draw_edit_box` repositions its clone into the
    // row's `Slot` before reading any of it (`OptionsSubScreen.init`'s
    // build-then-reposition order), so seeding real numbers here would be a
    // second, unread source of truth for the field's rect.
    let mut edit = EditBox::default_sized("Offline name");
    edit.set_max_length(NAME_MAX_LENGTH);
    edit.set_value(st.identity.username());
    edit.move_cursor_to_end(false);
    // Always focused: this frame has exactly one widget that takes text and no
    // Tab traversal, so a caret must be visible from the first frame.
    edit.widget.focused = true;
    st.name_edit = Some(NameEdit { edit, error: None });
}

/// Commits whatever is typed, or reports why it was refused and stays open.
///
/// Both halves matter. `set_username` **leaves the old name live** on `Err`
/// (`offline_identity`'s `set_username_leaves_the_old_name_live_when_it_refuses`
/// is that guarantee's own gate), so a refusal here cannot lose the name the
/// player is already joining under — which is why the error is shown in place
/// rather than closing the editor.
///
/// A *write* failure is different from a *validation* failure and is reported
/// differently: the name is already live in memory, so the editor closes and the
/// filesystem error goes to `save_error`, the same notice a failed
/// `profiles.json` write uses.
fn commit_name_edit(st: &mut State, offline_path: &Path) {
    let Some(pending) = st.name_edit.as_ref() else {
        return;
    };
    let typed = pending.edit.value().to_owned();
    match st.identity.set_username(&typed) {
        Ok(()) => {
            st.save_error = st
                .identity
                .save_to(offline_path)
                .err()
                .map(|e| format!("could not save the offline name: {e}"));
            st.name_edit = None;
        }
        Err(e) => {
            if let Some(pending) = st.name_edit.as_mut() {
                pending.error = Some(e.to_string());
            }
        }
    }
}

/// The name editor's key handling. Escape abandons, Enter commits, everything
/// else is the [`EditBox`]'s.
///
/// `MenuKey::Char` before `KeyEvent::from_menu_key`, matching
/// `create_world::handle_key`: `from_menu_key` returns `None` for a `Char`, and
/// treating that `None` as "unhandled" would silently drop every keystroke.
fn handle_key_editing_name(st: &mut State, key: MenuKey, offline_path: &Path) -> AccountsSignal {
    match key {
        // Abandon: the stored name is untouched (nothing has been written yet),
        // so this needs no restore step. `AccountsSignal::None`, not `Back` —
        // Escape closes the editor, not the screen.
        MenuKey::Escape => {
            st.name_edit = None;
            AccountsSignal::None
        }
        MenuKey::Enter => {
            commit_name_edit(st, offline_path);
            AccountsSignal::None
        }
        MenuKey::Char(ch) => {
            if let Some(pending) = st.name_edit.as_mut() {
                pending.edit.handle_char(ch);
                // A keystroke invalidates the previous refusal: leaving it up
                // would report "no spaces" at a name that no longer has one.
                pending.error = None;
            }
            AccountsSignal::None
        }
        other => {
            if let Some(event) = KeyEvent::from_menu_key(other)
                && let Some(pending) = st.name_edit.as_mut()
            {
                pending.edit.handle_key(event);
                pending.error = None;
            }
            AccountsSignal::None
        }
    }
}

fn handle_key_mid_flow(st: &mut State, key: MenuKey) -> AccountsSignal {
    match key {
        // `Enter` as well as `Escape`, because the sign-in screen now *has* a
        // Cancel button and a click on it arrives here as `hover` + `Enter`
        // (`MenuNav::click`'s default translation). Cancel is the only control on
        // the screen while a sign-in is in flight, so "activate the focused
        // widget" and "cancel" are the same verb — without this the button would
        // draw, highlight, and do nothing, which is that fix's shape.
        MenuKey::Escape | MenuKey::Enter => {
            if let SignIn::Requesting { cancel, .. } | SignIn::Waiting { cancel, .. } = &st.sign_in {
                cancel.store(true, Ordering::Relaxed);
            }
            AccountsSignal::None
        }
        MenuKey::Char('o' | 'O') => {
            if let SignIn::Waiting { verification_uri, .. } = &st.sign_in {
                open_in_browser(verification_uri);
            }
            AccountsSignal::None
        }
        MenuKey::Char('c' | 'C') => {
            // Native-only: `copy_to_clipboard` shells out to `pbcopy`/`clip`/`xclip`.
            // A browser has `navigator.clipboard.writeText`, but it is `async` and
            // permission-gated, so it is a different function rather than a swap —
            // and it is unreachable anyway, because `SignIn::Waiting` is only
            // produced by the sign-in workers, which do not exist on wasm32. The
            // code is still on screen for the player to copy by hand.
            #[cfg(not(target_arch = "wasm32"))]
            if let SignIn::Waiting { user_code, .. } = &st.sign_in {
                copy_to_clipboard(user_code);
            }
            AccountsSignal::None
        }
        _ => AccountsSignal::None,
    }
}

/// The logical row count: every account plus the trailing synthetic offline entry,
/// i.e. `rows().len()` computed without building the `Vec`.
///
/// Named rather than inlined because three call sites need it while `state` is
/// already mutably borrowed, so [`AccountsNav::rows`] is unreachable from them.
fn list_len(st: &State) -> usize {
    st.metadata.profiles.len() + 1
}

/// Keep [`State::highlighted`] inside the scrolled band — vanilla's
/// `AbstractSelectionList.scrollToEntry` (`:251-261`).
///
/// **Delegates to [`super::widget::ScrollList::scroll_to_entry`] rather than
/// restating the clamp**, which is what makes an arrow press move the minimum
/// number of *pixels* rather than snapping the window to a whole row. The previous
/// body was a two-branch row-index clamp against [`VISIBLE_ROWS`]; it is gone
/// because a row index cannot express an intermediate offset, and keeping it
/// alongside a pixel field would have meant two writers with different ideas of
/// what the offset means.
///
/// Uses [`crate::config::MIN_SCALED_HEIGHT`] rather than a real canvas for exactly
/// `MenuNav::scroll_server_to_show`'s reason: a keyboard press has no canvas to
/// hand, and the smallest supported canvas is the *conservative* choice — it can
/// only ever under-use a taller one, never scroll a row off-screen.
fn scroll_to_show(st: &mut State) {
    let Some(mut list) =
        super::render::accounts_list_spec(list_len(st), st.scroll)
            .model(crate::config::MIN_SCALED_HEIGHT as f32)
    else {
        return;
    };
    list.scroll_to_entry(st.highlighted);
    st.scroll = list.scroll();
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

/// Pure transition function: given the current [`SignIn`] state, drains at
/// most one message and returns the next state plus any side effect the
/// caller ([`AccountsNav::pump`]) must perform (opening the browser, and/or
/// folding a freshly-signed-in profile into `AccountsMetadata`).
///
/// Kept free of any actual I/O so it is unit-testable with a hand-fed
/// channel and no real network, browser, or keychain.
fn pump_locked(sign_in: SignIn) -> (SignIn, Option<(Option<String>, Option<AccountProfile>)>) {
    match sign_in {
        SignIn::Requesting { rx, cancel } => match rx.try_recv() {
            Ok(WorkerMsg::Prompt {
                user_code,
                verification_uri,
            }) => {
                // **A `Prompt` that arrives after Cancel must open nothing.**
                // `run_browser_login` sends its `Prompt` *before* the loop that
                // first checks the flag, so cancelling in the window between
                // "Add account" and the worker's first poll used to still
                // launch a browser window the user had just asked not to see —
                // a second, smaller instance of the unrequested-window symptom.
                // The worker notices the flag on its next sleep and follows
                // with `Cancelled`, which is what returns this to `Idle`.
                let effect = (!cancel.load(Ordering::Relaxed))
                    .then(|| (Some(verification_uri.clone()), None));
                (
                    SignIn::Waiting {
                        user_code,
                        verification_uri,
                        rx,
                        cancel,
                    },
                    effect,
                )
            }
            Ok(WorkerMsg::Failed(message)) => (SignIn::Failed { message }, None),
            Ok(WorkerMsg::Cancelled) => (SignIn::Idle, None),
            Ok(WorkerMsg::SignedIn(profile)) => (SignIn::Idle, Some((None, Some(profile)))),
            Err(TryRecvError::Empty) => (SignIn::Requesting { rx, cancel }, None),
            Err(TryRecvError::Disconnected) => (
                SignIn::Failed {
                    message: "the sign-in worker stopped unexpectedly".to_string(),
                },
                None,
            ),
        },
        SignIn::Waiting {
            user_code,
            verification_uri,
            rx,
            cancel,
        } => match rx.try_recv() {
            Ok(WorkerMsg::SignedIn(profile)) => (SignIn::Idle, Some((None, Some(profile)))),
            Ok(WorkerMsg::Failed(message)) => (SignIn::Failed { message }, None),
            Ok(WorkerMsg::Cancelled) => (SignIn::Idle, None),
            // A stale duplicate prompt; ignore and keep waiting.
            Ok(WorkerMsg::Prompt { .. }) => (
                SignIn::Waiting {
                    user_code,
                    verification_uri,
                    rx,
                    cancel,
                },
                None,
            ),
            Err(TryRecvError::Empty) => (
                SignIn::Waiting {
                    user_code,
                    verification_uri,
                    rx,
                    cancel,
                },
                None,
            ),
            Err(TryRecvError::Disconnected) => (
                SignIn::Failed {
                    message: "the sign-in worker stopped unexpectedly".to_string(),
                },
                None,
            ),
        },
        other @ (SignIn::Idle | SignIn::Failed { .. }) => (other, None),
    }
}

/// Renders an [`lodestone_auth::AuthError`] as a plain, user-facing message.
///
/// Matches the variants that exist today for a specific, friendlier message
/// and falls back to the error's own `Display` (via `#[error(...)]`) for
/// everything else. `AuthError` is `#[non_exhaustive]`, so this **must**
/// have a wildcard arm regardless — which is exactly what made it safe to
/// write *before* that fix landed [`lodestone_auth::AuthError::Xsts`] and
/// [`lodestone_auth::XstsErrorKind`] concurrently in this same session: this
/// function did not need to know their names to render something reasonable
/// in the meantime, and giving `Xsts` its own arm now that it exists is a
/// pure addition — consuming what the error type exposes, per the brief,
/// rather than string-matching Microsoft's response text.
#[must_use]
pub fn describe_auth_error(e: &lodestone_auth::AuthError) -> String {
    use lodestone_auth::AuthError as E;
    match e {
        E::AuthorizationDeclined => "Sign-in was declined.".to_string(),
        E::DeviceCodeExpired => "The sign-in code expired before it was used. Try again.".to_string(),
        E::NoMinecraftProfile => "No Minecraft profile was found for this Microsoft account.".to_string(),
        // `kind.describe()` is the short, user-facing text; `message` is
        // Microsoft's own raw (English, developer-oriented) response body —
        // see the variant's own doc comment on why the UI wants the former.
        E::Xsts { kind, .. } => kind.describe().to_string(),
        other => other.to_string(),
    }
}

/// Describes a failure from [`lodestone_auth::login::finish_interactive`],
/// which now does what `run_device_code_login`/`finish_ms_token` used to
/// hand-roll as two separate calls (deriving the session, then saving the
/// refresh token) — see that fix and `docs/accounts.md`. Keeping the same
/// two distinct messages those two calls used to produce, rather than
/// collapsing to one, because `secrets.save_refresh_token` can only ever fail
/// with [`lodestone_auth::AuthError::Keychain`]/[`lodestone_auth::AuthError::Cache`]
/// (a filesystem/keychain error), and every other variant can only have come
/// from deriving the session itself — so the variant alone tells us which
/// step failed, with no need to keep the two calls separate to distinguish
/// them.
#[must_use]
#[cfg(not(target_arch = "wasm32"))]
fn describe_finish_interactive_failure(e: &lodestone_auth::AuthError) -> String {
    use lodestone_auth::AuthError as E;
    match e {
        E::Keychain(_) | E::Cache(_) => {
            format!("signed in, but could not save the credential: {e}")
        }
        other => describe_auth_error(other),
    }
}

/// Runs the **loopback** sign-in: the real Microsoft login page in the user's
/// browser, no code to type. This is what Add Account uses.
///
/// A device-code → Xbox Live → XSTS → Minecraft-services worker
/// (`run_device_code_login`) used to live beside this one, sharing everything
/// from the `MsToken` onward through [`finish_ms_token`]. It was deleted: it
/// had no caller (`handle_key` only ever spawns this loopback worker) and its
/// own inline copy of the post-token steps had drifted from
/// [`finish_ms_token`]'s — it was missing the `tracing::warn!` on a
/// session-derivation failure. If a real headless/no-browser fallback is
/// wanted, rebuild it from `lodestone_auth::flow::PendingLogin` and call
/// [`finish_ms_token`] rather than re-inlining its body.
///
/// Same keychain save here on the worker thread, same `SignedIn` message with the
/// metadata write left to [`AccountsNav::pump`]. Only how the authorization code
/// arrives differs, which is the whole reason
/// [`lodestone_auth::browser_login`] was shaped to mirror
/// `flow::PendingLogin`'s `poll_once`.
///
/// The URL still goes to the screen as [`WorkerMsg::Prompt`]'s
/// `verification_uri`, with an **empty** `user_code`: there is no code in this
/// flow, and the URL is the copy-paste fallback for when the browser cannot be
/// launched. `render.rs` renders an empty code as "no code to show".
#[cfg(not(target_arch = "wasm32"))]
fn run_browser_login(tx: Sender<WorkerMsg>, cancel: Arc<AtomicBool>) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Failed(format!("could not start a runtime: {e}")));
            return;
        }
    };
    rt.block_on(async move {
        // Deliberately never `flow::MOJANG_CLIENT_ID`, the *official
        // launcher's* registration — see `lodestone_auth::login`'s docs.
        let client_id = match lodestone_auth::login::resolve_client_id() {
            Ok(id) => id,
            Err(e) => {
                let _ = tx.send(WorkerMsg::Failed(describe_auth_error(&e)));
                return;
            }
        };
        // That fix, as above: `rustls-no-provider` makes this a runtime panic
        // without an installed provider.
        lodestone_auth::install_crypto_provider();
        let client = reqwest::Client::new();
        let mut pending = match lodestone_auth::browser_login::LoopbackLogin::begin(&client_id).await
        {
            Ok(p) => p,
            Err(e) => {
                let _ = tx.send(WorkerMsg::Failed(describe_auth_error(&e)));
                return;
            }
        };

        // `WorkerMsg::Prompt` **already opens the browser**: `pump` turns it into an
        // effect carrying the URI, and the render thread calls `open_in_browser` on
        // it (the device-code flow's auto-open). So this must not open it too —
        // doing both launched the browser twice, reported from play.
        //
        // Worth stating because reusing this variant is what caused it: it was
        // chosen to avoid touching a contended `render.rs`, and it turned out to
        // carry a side effect. A message that *does* something is not a plain
        // carrier, and the second mechanism was invisible from the send site.
        let _ = tx.send(WorkerMsg::Prompt {
            user_code: String::new(),
            verification_uri: pending.authorize_url().to_owned(),
        });

        loop {
            // 100ms rather than the device flow's server-dictated interval: this
            // polls our own listener, not Microsoft, so there is no rate limit to
            // respect and a tighter loop makes sign-in feel immediate.
            if cancellable_sleep_ms(100, &cancel).await {
                let _ = tx.send(WorkerMsg::Cancelled);
                return;
            }
            if pending.is_expired() {
                let _ = tx.send(WorkerMsg::Failed(
                    "Sign-in timed out waiting for the browser. Try again.".to_owned(),
                ));
                return;
            }
            match pending.poll_once(&client, &client_id).await {
                Ok(None) => continue,
                Ok(Some(ms_token)) => {
                    finish_ms_token(&tx, &client, ms_token).await;
                    return;
                }
                Err(e) => {
                    // Log as well as show: the on-screen string is transient and the
                    // user cannot copy it, so a failed sign-in left no evidence at
                    // all. `AuthError`'s Debug carries the step and status that
                    // `describe_auth_error` flattens for display.
                    tracing::warn!(target: "auth", error = ?e, "browser sign-in failed");
                    let _ = tx.send(WorkerMsg::Failed(describe_auth_error(&e)));
                    return;
                }
            }
        }
    });
}

/// The half of a sign-in that is identical for both flows: an `MsToken` becomes a
/// session, a saved refresh token and a [`WorkerMsg::SignedIn`].
///
/// Extracted so the two workers cannot drift. The keychain write happens here, on
/// the worker thread; the `profiles.json` write deliberately does not — it stays
/// in [`AccountsNav::pump`] so every metadata write funnels through one place
/// rather than racing a foreground Remove.
#[cfg(not(target_arch = "wasm32"))]
async fn finish_ms_token(
    tx: &Sender<WorkerMsg>,
    client: &reqwest::Client,
    ms_token: lodestone_auth::flow::MsToken,
) {
    // Was two hand-rolled calls (`session_from_ms_token` then
    // `secrets.save_refresh_token`) duplicating `login::finish_interactive`'s
    // own composition — That fix. `scratch` is discarded: this thread's real
    // metadata lives on the render thread and is written back through
    // `AccountsNav::pump`, never here.
    let secrets = lodestone_auth::AccountSecrets::open();
    let mut scratch = AccountsMetadata::default();
    let session =
        match lodestone_auth::login::finish_interactive(client, &ms_token, &secrets, &mut scratch).await {
            Ok(s) => s,
            Err(e) => {
                // **This is the arm a real sign-in failure takes**, and its
                // silence used to be why a failed attempt left no log line for
                // the failing step: `run_browser_login`'s `poll_once` arm logs,
                // but that one only fires before the browser hands the code
                // back. `AuthError`'s `Debug` carries the step and the
                // untruncated response body that `describe_finish_interactive_failure`
                // flattens to one sentence for display, and the on-screen
                // string is transient and uncopyable — so this line is the
                // only thing that makes a session-derivation failure
                // diagnosable after the fact. A credential-*save* failure
                // (`AuthError::Keychain`/`AuthError::Cache`) does not warn here,
                // matching the pre-fix behaviour where that step had no log
                // line of its own.
                use lodestone_auth::AuthError as E;
                if !matches!(e, E::Keychain(_) | E::Cache(_)) {
                    tracing::warn!(target: "auth", error = ?e, "sign-in failed after the browser step");
                }
                let _ = tx.send(WorkerMsg::Failed(describe_finish_interactive_failure(&e)));
                return;
            }
        };
    // The profile now carries its skin, so fetch it here — this is the only
    // place in the process with both the services profile and an HTTP
    // client. Never fatal.
    crate::skin_fetch::fetch_own_skin(client, &session.profile).await;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = tx.send(WorkerMsg::SignedIn(AccountProfile {
        profile_id: session.profile.id,
        username: session.profile.name.clone(),
        skin_url: session.profile.skin.as_ref().map(|s| s.url.clone()),
        last_used: now,
    }));
}

/// Sleeps up to `millis` milliseconds, checking `cancel` every poll so an
/// interactive Cancel keypress is felt quickly rather than after a whole
/// sleep. Returns `true` if cancelled mid-sleep. Millis rather than a
/// `Duration`: the loopback flow's 100ms poll is a literal at the call site,
/// not a value that flows in from anywhere that would want a richer type.
#[cfg(not(target_arch = "wasm32"))]
async fn cancellable_sleep_ms(millis: u64, cancel: &AtomicBool) -> bool {
    if cancel.load(Ordering::Relaxed) {
        return true;
    }
    tokio::time::sleep(std::time::Duration::from_millis(millis)).await;
    cancel.load(Ordering::Relaxed)
}

/// Best-effort: opens `url` in the system's default browser. Never blocks
/// (the OS handoff command returns immediately) and never panics — a failure
/// just means the user has to open the URL themselves, which the screen
/// always shows as text anyway.
///
/// No `open`/`webbrowser` crate dependency: three one-line OS commands cover
/// the three desktop platforms this client targets without adding to the
/// dependency graph for a single call site.
///
/// `pub(crate)` since that fix: `super::telemetry`'s Privacy Statement/Give
/// Feedback buttons reuse this rather than duplicating it, since opening a
/// URL has nothing account-specific about it.
/// **A unit test must never reach the OS handoff, and one did — it reached a
/// player.** `add_account_button_starts_the_flow_and_a_prompt_message_shows_it`
/// fed the state machine a [`WorkerMsg::Prompt`] carrying the literal
/// `https://microsoft.com/link` and then called [`AccountsNav::pump`], which
/// performs the open as an *effect*. So every `cargo test -p lodestone-shell`
/// run — several agents run that suite continuously — spawned `open` on
/// Microsoft's device-code page, which 301s to
/// `https://login.live.com/oauth20_remoteconnect.srf`. The owner, playing the
/// game, saw OAuth windows appear from nowhere and reported it twice; the flow
/// they were attributed to (Add Account) had not been touched. Measured with a
/// PATH shim standing in for `open`: one call per lib-test run,
/// `OPEN_CALLED https://microsoft.com/link`.
///
/// The interception below is a `cfg` **fork**, not a `cfg!(test)` early return,
/// for two reasons: the `Command::spawn` is then not even compiled into a test
/// binary, and a test can *assert* the interception is live
/// (`the_real_browser_handoff_is_unreachable_from_a_unit_test`) rather than
/// trust a silent skip. `super::telemetry`'s Privacy Statement / Give Feedback
/// buttons come through here too, so they are covered by the same fork — a
/// latent copy of this bug that had not fired only because no telemetry test
/// activates rows 0 or 1.
#[cfg(all(not(test), not(target_arch = "wasm32")))]
pub(crate) fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// The browser build's [`open_in_browser`]: `window.open`, in a new tab.
///
/// **This is the one native-only capability on this screen that a browser does
/// better rather than worse** — "hand a URL to the platform's browser" is the
/// platform's whole job here, so this is a real implementation, not a gate. It is
/// only ever called from a key handler, i.e. inside a user gesture, which is what
/// keeps a popup blocker from swallowing it. A blocked or refused open is ignored
/// for the same reason the native arm ignores a failed `spawn`: every caller also
/// shows the URL on screen as text.
#[cfg(all(not(test), target_arch = "wasm32"))]
pub(crate) fn open_in_browser(url: &str) {
    if let Some(win) = web_sys::window() {
        let _ = win.open_with_url_and_target(url, "_blank");
    }
}

/// The test build's [`open_in_browser`]: records the URL instead of handing it
/// to the OS. See the `cfg(not(test))` sibling above for the incident.
#[cfg(test)]
pub(crate) fn open_in_browser(url: &str) {
    test_browser_opens::record(url);
}

/// Per-thread record of what [`open_in_browser`] was asked to open, in test
/// builds only.
///
/// Thread-local rather than a global counter because the test harness runs each
/// `#[test]` on its own thread, so this isolates concurrently-running tests
/// from each other with no lock and no ordering assumption. Every consumer is
/// on the same thread as the `pump` it is observing.
#[cfg(test)]
pub(crate) mod test_browser_opens {
    use std::cell::RefCell;

    thread_local! {
        static OPENS: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn record(url: &str) {
        OPENS.with(|o| o.borrow_mut().push(url.to_owned()));
    }

    /// Everything recorded so far, clearing the record. Taking rather than
    /// peeking so a test's assertions are about *its own* interval.
    pub(crate) fn taken() -> Vec<String> {
        OPENS.with(|o| std::mem::take(&mut *o.borrow_mut()))
    }

    pub(crate) fn count() -> usize {
        OPENS.with(|o| o.borrow().len())
    }
}

/// Best-effort: copies `text` to the system clipboard via the same
/// no-new-dependency OS-command approach as [`open_in_browser`].
///
/// `pub(crate)` since that fix: `crate::chat`'s `copy_to_clipboard`
/// `click_event` reuses this rather than duplicating it — the effect
/// (`pbcopy`/`clip`/`xclip`) has nothing account-specific about it, the same
/// reasoning [`open_in_browser`] is already `pub(crate)` for.
///
/// **Forked on `#[cfg(test)]` for the same reason [`open_in_browser`] is,
/// and found the same way that incident's own doc says to look: grep for the
/// effect, not the feature.** This call site was reachable from a real key
/// handler (`MenuKey::Char('c')` while `SignIn::Waiting`) with no test
/// interception at all — a second, latent instance of the exact incident
/// `open_in_browser`'s doc records, sitting undiscovered only because no
/// existing test happens to press 'c' from that state. Fixed here rather
/// than left for the day one does.
#[cfg(all(not(test), not(target_arch = "wasm32")))]
pub(crate) fn copy_to_clipboard(text: &str) {
    use std::io::Write;
    #[cfg(target_os = "macos")]
    let cmd = std::process::Command::new("pbcopy").stdin(std::process::Stdio::piped()).spawn();
    #[cfg(target_os = "windows")]
    let cmd = std::process::Command::new("clip").stdin(std::process::Stdio::piped()).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let cmd = std::process::Command::new("xclip")
        .args(["-selection", "clipboard"])
        .stdin(std::process::Stdio::piped())
        .spawn();
    if let Ok(mut child) = cmd
        && let Some(stdin) = child.stdin.as_mut()
    {
        let _ = stdin.write_all(text.as_bytes());
    }
}

/// The test build's [`copy_to_clipboard`]: records the text instead of
/// shelling out to the OS clipboard. See the `cfg(not(test))` sibling above
/// for the incident this forestalls.
#[cfg(all(test, not(target_arch = "wasm32")))]
pub(crate) fn copy_to_clipboard(text: &str) {
    test_clipboard::record(text);
}

/// Per-thread record of what [`copy_to_clipboard`] was asked to copy, in
/// test builds only — the clipboard sibling of `test_browser_opens`, same
/// thread-local reasoning (one `#[test]` per thread, no lock, no ordering
/// assumption).
#[cfg(test)]
pub(crate) mod test_clipboard {
    use std::cell::RefCell;

    thread_local! {
        static COPIES: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    }

    pub(crate) fn record(text: &str) {
        COPIES.with(|c| c.borrow_mut().push(text.to_owned()));
    }

    /// Everything recorded so far, clearing the record — taking rather than
    /// peeking so a test's assertions are about its own interval.
    pub(crate) fn taken() -> Vec<String> {
        COPIES.with(|c| std::mem::take(&mut *c.borrow_mut()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path(tag: &str) -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "lodestone-accounts-nav-{}-{tag}/profiles.json",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        path
    }

    fn profile(name: &str, last_used: u64) -> AccountProfile {
        AccountProfile {
            profile_id: Uuid::new_v4(),
            username: name.to_string(),
            skin_url: None,
            last_used,
        }
    }

    fn spawn_stub(rx: Receiver<WorkerMsg>, cancel: Arc<AtomicBool>) -> Spawn {
        Box::new(move || (rx, cancel))
    }

    /// Fixture URLs use RFC 2606's reserved `.invalid` TLD, **never a real
    /// endpoint**. This is defence in depth behind `open_in_browser`'s
    /// `cfg(test)` fork: these strings used to be `https://microsoft.com/link`,
    /// and `pump` handed that to the OS on every lib-test run. If the fork is
    /// ever removed, a regression opens a tab that cannot resolve rather than a
    /// live Microsoft OAuth page.
    const FIXTURE_URI: &str = "https://example.invalid/device-login";
    const FIXTURE_URI_2: &str = "https://example.invalid/device-login-again";

    #[test]
    fn offline_is_always_present_and_selected_by_default() {
        let nav = AccountsNav::with_path(temp_path("offline-default"));
        assert_eq!(nav.rows(), vec![AccountRow::Offline]);
        assert!(nav.offline_selected());
    }

    #[test]
    fn accounts_sort_most_recently_used_first() {
        let path = temp_path("sort");
        let mut meta = AccountsMetadata::default();
        let old = profile("Old", 1);
        let new = profile("New", 100);
        meta.upsert(old.clone());
        meta.upsert(new.clone());
        meta.save_to(&path).unwrap();

        let nav = AccountsNav::with_path(path.clone());
        assert_eq!(
            nav.rows(),
            vec![AccountRow::Account(new), AccountRow::Account(old), AccountRow::Offline]
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn up_down_wraps_across_accounts_and_the_offline_row() {
        let path = temp_path("wrap");
        let mut meta = AccountsMetadata::default();
        meta.upsert(profile("A", 2));
        meta.upsert(profile("B", 1));
        meta.save_to(&path).unwrap();
        let nav = AccountsNav::with_path(path.clone());

        assert_eq!(nav.highlighted(), 0);
        nav.handle_key(MenuKey::Up);
        assert_eq!(nav.highlighted(), 2, "up from the top wraps to the offline row");
        nav.handle_key(MenuKey::Down);
        assert_eq!(nav.highlighted(), 0);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn selecting_offline_clears_the_metadata_selection() {
        let path = temp_path("select-offline");
        let mut meta = AccountsMetadata::default();
        let a = profile("A", 1);
        meta.upsert(a.clone());
        meta.selected = Some(a.profile_id);
        meta.save_to(&path).unwrap();
        let nav = AccountsNav::with_path(path.clone());

        assert!(!nav.offline_selected());
        nav.handle_key(MenuKey::Down); // A -> offline
        assert_eq!(nav.highlighted(), 1);
        nav.handle_key(MenuKey::Enter);
        assert!(nav.offline_selected(), "selecting offline must clear `selected`");
        assert_eq!(
            AccountsMetadata::load_from(&path).selected,
            None,
            "must be saved, not just held in memory"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn selecting_a_real_account_persists_it() {
        let path = temp_path("select-real");
        let mut meta = AccountsMetadata::default();
        meta.upsert(profile("A", 1));
        meta.save_to(&path).unwrap();
        let nav = AccountsNav::with_path(path.clone());
        let id = match &nav.rows()[0] {
            AccountRow::Account(p) => p.profile_id,
            AccountRow::Offline => panic!("expected an account row"),
        };

        nav.handle_key(MenuKey::Enter);
        assert!(nav.is_selected(id));
        assert_eq!(AccountsMetadata::load_from(&path).selected, Some(id));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn remove_cannot_touch_the_offline_row() {
        let path = temp_path("remove-offline-guard");
        let nav = AccountsNav::with_path(path.clone());
        assert_eq!(nav.highlighted(), 0);
        nav.handle_key(MenuKey::Delete);
        assert_eq!(nav.rows(), vec![AccountRow::Offline], "offline must survive");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Hover takes a **logical** row index, and the offset it is read against is
    /// pixels.
    ///
    /// This replaces `hover_maps_a_rendered_row_back_through_the_scroll_offset`,
    /// whose subject no longer exists: `accounts_idle_frame` emits every logical row
    /// rather than a `rows[scroll..scroll + VISIBLE_ROWS]` slice, so there is no
    /// window for `hover` to map back through. Deleting the old assertion without
    /// replacing it would have removed the only check that `hover` and the frame
    /// agree about what a row index *means*, so the invariant is restated in the new
    /// terms: hovering row `n` focuses row `n`, whatever the list is scrolled to.
    ///
    /// It is asserted on `focus`, not `highlighted` — see
    /// `hovering_an_account_does_not_change_what_select_acts_on`. The old version of
    /// this test read `highlighted()` and thereby locked in the reported bug: test
    /// and code agreed because both came from the same wrong assumption.
    #[test]
    fn hover_takes_a_logical_row_and_the_offset_is_pixels() {
        let path = temp_path("hover-scroll");
        let mut meta = AccountsMetadata::default();
        for i in 0..8u64 {
            meta.upsert(profile(&format!("p{i}"), i));
        }
        meta.save_to(&path).unwrap();
        let nav = AccountsNav::with_path(path.clone());
        // 8 accounts + 1 offline = 9 logical rows; the band at MIN_SCALED_HEIGHT
        // holds five 36 px rows, so reaching row 7 must have scrolled.
        for _ in 0..7 {
            nav.handle_key(MenuKey::Down);
        }
        assert_eq!(nav.highlighted(), 7);
        let scroll = nav.scroll();
        assert!(
            scroll > 0.0,
            "highlighting row 7 with five rows of band must have scrolled, got {scroll}"
        );
        // A *pixel* offset, so it is expressible in units a row index cannot reach —
        // and the value is predicted from vanilla's own arithmetic rather than read
        // off the implementation. `scrollToEntry`'s bottom branch
        // solves
        // `bottom() - row_top(7) - 36 - CONTENT_PADDING = 0`, i.e.
        // `scroll = row_offset(8) + 2 * CONTENT_PADDING - band`:
        //
        //   band   = MIN_SCALED_HEIGHT - ACCOUNTS_FOOTER_H - content_top
        //          = 240 - 60 - 33                                       = 147
        //   scroll = 8 * 36 + 2 * 2 - 147                                = 145
        //
        // **145 is not a multiple of 36**, which is the whole claim: the old `usize`
        // field could only ever hold 0, 36, 72, … so this position was unreachable,
        // and a keyboard step therefore had to jump a whole entry.
        assert_eq!(
            scroll, 145.0,
            "the minimum move that brings row 7 fully into a 147 px band"
        );
        assert_ne!(
            scroll % 36.0,
            0.0,
            "a row-index offset could have expressed {scroll}, so this proves nothing"
        );

        // Hovering row 0 focuses row 0. Under the old window model this call meant
        // "the topmost row currently drawn", which at this offset was row 3.
        nav.hover(0);
        assert_eq!(nav.focus(), 0, "hover takes the logical row, not a window slot");
        nav.hover(7);
        assert_eq!(nav.focus(), 7, "and again at the other end of the list");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn hovering_an_account_does_not_change_what_select_acts_on() {
        // The owner's report: "hovering an account shouldn't focus it (it should
        // work the same as the server list)".
        //
        // Both pieces of state are read after every move, because one assertion
        // cannot distinguish "the hover highlight works" from "the hover selected
        // it" — the two facts are `focus` (drawn) and `highlighted` (acted on).
        let path = temp_path("hover-not-select");
        let mut meta = AccountsMetadata::default();
        for i in 0..3u64 {
            meta.upsert(profile(&format!("p{i}"), i));
        }
        meta.save_to(&path).unwrap();
        let nav = AccountsNav::with_path(path.clone());

        // Keyboard-highlight row 1, so there is a selection to steal.
        nav.handle_key(MenuKey::Down);
        assert_eq!(nav.highlighted(), 1);
        assert_eq!(nav.focus(), 1);

        // Now hover a *different* row. The highlight must follow the mouse and
        // the selection must not move.
        nav.hover(3);
        assert_eq!(nav.focus(), 3, "the hover highlight must follow the mouse");
        assert_eq!(
            nav.highlighted(),
            1,
            "hover must not re-aim Select/Remove — this is the reported bug"
        );

        // Sweeping across every row leaves the selection where it was.
        for row in 0..4 {
            nav.hover(row);
            assert_eq!(
                nav.highlighted(),
                1,
                "hovering row {row} must not move the selection"
            );
        }

        // Control: the selection *can* still be moved, so the assertions above
        // are not passing merely because `highlighted` is stuck. A click is what
        // selects, and the keyboard is what selects — hover is neither.
        nav.handle_key(MenuKey::Down);
        assert_eq!(nav.highlighted(), 2, "the keyboard must still select");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn clicking_an_account_selects_it_and_moves_what_remove_acts_on() {
        // The other direction of the hover fix, and the reason it is not just a
        // deleted line: now that `hover` no longer writes `highlighted`, `Enter`
        // must, or Remove and Delete stay aimed at a row the player is no longer
        // looking at.
        //
        // This is the **keyboard** path. `MenuNav::click` used to reach a list
        // row this same way — `hover` then `Enter` — which is why the two were
        // once one test; a click now goes through `click_row`/`select_focused`
        // instead, and is covered by
        // `a_single_click_focuses_an_account_and_a_second_click_selects_it`.
        let path = temp_path("click-selects");
        let mut meta = AccountsMetadata::default();
        for i in 0..3u64 {
            meta.upsert(profile(&format!("p{i}"), i));
        }
        meta.save_to(&path).unwrap();
        let nav = AccountsNav::with_path(path.clone());
        assert_eq!(nav.highlighted(), 0);

        // Simulate a click on rendered row 2: hover then Enter, exactly as
        // `MenuNav::click` does it.
        nav.hover(2);
        nav.handle_key(MenuKey::Enter);

        assert_eq!(nav.focus(), 2);
        assert_eq!(
            nav.highlighted(),
            2,
            "a click must move the sticky highlight, unlike a hover"
        );
        // And it really did select that account, not merely highlight it.
        let ordered = nav.rows();
        if let Some(AccountRow::Account(p)) = ordered.get(2) {
            assert!(nav.is_selected(p.profile_id));
        } else {
            panic!("row 2 of 3 accounts + offline should be an account: {ordered:?}");
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// The interaction model this screen now shares with the server list and
    /// the world list: **one click focuses, two clicks select**.
    ///
    /// The discriminating assertion is the *negative* one in the middle — after
    /// the first click the row must be highlighted and **not** selected. A gate
    /// that only checked the end state would pass under the old behaviour too,
    /// because the old single click also ended with row 2 highlighted and
    /// selected; the whole change is that those two stopped happening together.
    ///
    /// It matters beyond tidiness: `select` writes `profiles.json`, so under
    /// the old model every stray click on the list committed an account switch
    /// to disk — including a click that was only aiming Remove at a row.
    #[test]
    fn a_single_click_focuses_an_account_and_a_second_click_selects_it() {
        let path = temp_path("click-focus-then-select");
        let mut meta = AccountsMetadata::default();
        for i in 0..3u64 {
            meta.upsert(profile(&format!("p{i}"), i));
        }
        meta.save_to(&path).unwrap();
        let nav = AccountsNav::with_path(path.clone());

        let target = match nav.rows().get(2) {
            Some(AccountRow::Account(p)) => p.profile_id,
            other => panic!("row 2 of 3 accounts + offline must be an account: {other:?}"),
        };
        assert!(!nav.is_selected(target), "nothing is selected to begin with");

        assert!(nav.click_row(2), "row 2 is a list row");
        assert_eq!(nav.focus(), 2);
        assert_eq!(
            nav.highlighted(),
            2,
            "a click aims Select/Remove/Delete at the row it landed on"
        );
        assert!(
            !nav.is_selected(target),
            "a single click must NOT commit the account switch -- that is the \
             whole difference from the old hover-plus-Enter fall-through"
        );

        nav.select_focused();
        assert!(
            nav.is_selected(target),
            "the second click selects the focused row"
        );

        // A button row is not a list row: `click_row` says so, which is what
        // keeps single-click activation on the footer (the server list's rule
        // too), and it must not disturb the account cursor on its way past.
        let buttons_start = nav.rows().len();
        assert!(
            !nav.click_row(buttons_start),
            "a button row must report itself as not a list row"
        );
        assert_eq!(
            nav.highlighted(),
            2,
            "focusing a button must not re-aim Remove"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn hover_on_a_button_row_does_not_move_the_account_highlight() {
        let path = temp_path("hover-button");
        let mut meta = AccountsMetadata::default();
        meta.upsert(profile("A", 1));
        meta.save_to(&path).unwrap();
        let nav = AccountsNav::with_path(path.clone());
        assert_eq!(nav.highlighted(), 0);
        // 2 logical rows (A + offline) are both shown (< VISIBLE_ROWS), so
        // rendered row 2 is the first button ("Add account").
        nav.hover(2);
        assert_eq!(nav.highlighted(), 0, "the sticky highlight must not move");
        assert_eq!(nav.focus(), 2 + BUTTON_ADD, "but focus follows the mouse");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn add_account_button_starts_the_flow_and_a_prompt_message_shows_it() {
        let path = temp_path("add-flow");
        let nav = AccountsNav::with_path(path.clone());
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        // Empty metadata: 1 logical row (offline) is shown, so the first
        // button ("Add account") renders at index 1.
        nav.hover(1 + BUTTON_ADD);
        assert_eq!(nav.handle_key_with(MenuKey::Enter, spawn_stub(rx, cancel)), AccountsSignal::None);
        assert_eq!(nav.sign_in_view(), SignInView::Requesting);

        tx.send(WorkerMsg::Prompt {
            user_code: "ABCD-EFGH".to_string(),
            verification_uri: FIXTURE_URI.to_string(),
        })
        .unwrap();
        nav.pump();
        assert_eq!(
            nav.sign_in_view(),
            SignInView::Waiting {
                user_code: "ABCD-EFGH".to_string(),
                verification_uri: FIXTURE_URI.to_string(),
            }
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// **The gate that would have caught the unrequested-browser report.**
    ///
    /// `pump` performs the browser open as an *effect*, so a test that feeds the
    /// state machine a `Prompt` and pumps is indistinguishable, from the OS's
    /// point of view, from a player pressing "Add account" — and the test above
    /// did exactly that with `https://microsoft.com/link`, which 301s to
    /// `login.live.com/oauth20_remoteconnect.srf`. Measured before the fix with
    /// a PATH shim in place of `open`: one real `open` per lib-test run.
    ///
    /// This asserts the `cfg(test)` fork in [`open_in_browser`] is the arm that
    /// got compiled. If it is ever deleted, the `Command::spawn` comes back and
    /// this fails — it cannot pass by accident, because nothing else populates
    /// the recorder.
    #[test]
    fn the_real_browser_handoff_is_unreachable_from_a_unit_test() {
        let _ = test_browser_opens::taken();
        open_in_browser("https://example.invalid/probe");
        assert_eq!(
            test_browser_opens::taken(),
            vec!["https://example.invalid/probe".to_string()],
            "the cfg(test) interception is not live — a unit test just handed a URL to the OS"
        );
    }

    /// [`copy_to_clipboard`]'s own sibling of the gate above — the second
    /// latent instance the incident's own lesson (grep for the effect, not
    /// the feature) found. Before this fork existed, a test that pressed 'c'
    /// while `SignIn::Waiting` would have shelled out to `pbcopy`/`clip`/
    /// `xclip` for real; nothing currently does, which is exactly why it
    /// went unnoticed rather than being caught the way the browser one was.
    #[test]
    fn the_real_clipboard_handoff_is_unreachable_from_a_unit_test() {
        let _ = test_clipboard::taken();
        copy_to_clipboard("probe-code");
        assert_eq!(
            test_clipboard::taken(),
            vec!["probe-code".to_string()],
            "the cfg(test) interception is not live — a unit test just shelled out to the OS clipboard"
        );
    }

    /// **The invariant the report violated: at most one browser open per user
    /// action.** `pump` runs every frame the screen is showing, so "opens once"
    /// and "opens sixty times a second" are the same code path with a different
    /// guard, and only a count across many frames can tell them apart.
    ///
    /// Both controls matter. The first (20 empty pumps → 0) rules out "every
    /// pump opens", which would make the later count meaningless. The second (a
    /// *second* Add is allowed a second open) proves the recorder can move, so
    /// "still exactly one" is not the vacuous pass a permanently-dead recorder
    /// would also produce.
    #[test]
    fn the_browser_opens_at_most_once_per_add_account_action() {
        let path = temp_path("open-once-per-action");
        let nav = AccountsNav::with_path(path.clone());
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let _ = test_browser_opens::taken();

        nav.hover(1 + BUTTON_ADD);
        nav.handle_key_with(MenuKey::Enter, spawn_stub(rx, Arc::clone(&cancel)));
        for _ in 0..20 {
            nav.pump();
        }
        assert_eq!(
            test_browser_opens::count(),
            0,
            "control: an in-flight flow with nothing to report must open nothing"
        );

        tx.send(WorkerMsg::Prompt {
            user_code: String::new(),
            verification_uri: FIXTURE_URI.to_string(),
        })
        .unwrap();
        for _ in 0..50 {
            nav.pump();
        }
        assert_eq!(
            test_browser_opens::taken(),
            vec![FIXTURE_URI.to_string()],
            "one user action must produce exactly one open, across 50 frames"
        );

        // Back to Idle, then a second, genuinely separate user action.
        tx.send(WorkerMsg::Cancelled).unwrap();
        nav.pump();
        assert_eq!(nav.sign_in_view(), SignInView::Idle);

        let (tx2, rx2) = channel();
        nav.handle_key_with(
            MenuKey::Enter,
            spawn_stub(rx2, Arc::new(AtomicBool::new(false))),
        );
        tx2.send(WorkerMsg::Prompt {
            user_code: String::new(),
            verification_uri: FIXTURE_URI_2.to_string(),
        })
        .unwrap();
        for _ in 0..10 {
            nav.pump();
        }
        assert_eq!(
            test_browser_opens::taken(),
            vec![FIXTURE_URI_2.to_string()],
            "control: the recorder can move — a second action gets its own single open"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    /// Cancelling in the window between "Add account" and the worker's first
    /// poll must not still launch a browser. `run_browser_login` sends its
    /// `Prompt` *before* the loop that checks the cancel flag, so the message is
    /// already on its way when the user changes their mind.
    ///
    /// The positive control lives in
    /// `the_browser_opens_at_most_once_per_add_account_action` above: the same
    /// `Prompt`, without the cancel, does open exactly once — so this is not
    /// asserting an absence the recorder could never have observed.
    #[test]
    fn a_prompt_that_arrives_after_cancel_opens_nothing() {
        let path = temp_path("cancel-before-prompt");
        let nav = AccountsNav::with_path(path.clone());
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let _ = test_browser_opens::taken();

        nav.hover(1 + BUTTON_ADD);
        nav.handle_key_with(MenuKey::Enter, spawn_stub(rx, Arc::clone(&cancel)));
        nav.handle_key(MenuKey::Escape);
        assert!(cancel.load(Ordering::Relaxed), "Escape must have set the flag");

        tx.send(WorkerMsg::Prompt {
            user_code: String::new(),
            verification_uri: FIXTURE_URI.to_string(),
        })
        .unwrap();
        for _ in 0..10 {
            nav.pump();
        }
        assert_eq!(
            test_browser_opens::count(),
            0,
            "a prompt racing a cancel must not open a window the user just refused"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn cancel_during_the_flow_returns_to_idle_without_a_screen_change() {
        let path = temp_path("cancel-flow");
        let nav = AccountsNav::with_path(path.clone());
        let (_tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        nav.hover(1 + BUTTON_ADD);
        nav.handle_key_with(MenuKey::Enter, spawn_stub(rx, Arc::clone(&cancel)));
        assert_eq!(nav.sign_in_view(), SignInView::Requesting);

        assert_eq!(nav.handle_key(MenuKey::Escape), AccountsSignal::None);
        assert!(cancel.load(Ordering::Relaxed), "the flag must be set for the worker to see");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_successful_sign_in_lands_in_the_list_and_on_disk() {
        let path = temp_path("signed-in");
        let nav = AccountsNav::with_path(path.clone());
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        nav.hover(1 + BUTTON_ADD);
        nav.handle_key_with(MenuKey::Enter, spawn_stub(rx, cancel));

        let steve = profile("Steve", 42);
        tx.send(WorkerMsg::SignedIn(steve.clone())).unwrap();
        nav.pump();

        assert_eq!(nav.sign_in_view(), SignInView::Idle);
        assert!(nav.rows().contains(&AccountRow::Account(steve.clone())));
        assert!(nav.is_selected(steve.profile_id));
        assert_eq!(AccountsMetadata::load_from(&path).selected, Some(steve.profile_id));
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    // -- the ownership gate ------------------------------------------------
    //
    // The half `tests/session/ownership_gate.rs` structurally cannot reach: it
    // drives the menu from outside the crate and so cannot inject a sign-in
    // worker, while `handle_key_with` — the seam that makes the real state
    // machine drivable without a network — is private to this module.

    /// Plants a world directory under `root`, so **Play Selected World** is live
    /// at all: with an empty `saves/` the button is greyed and a walk to it
    /// would stop for a reason that has nothing to do with the ownership gate.
    fn plant_world(root: &Path, dir_name: &str) {
        let dir = root.join(dir_name);
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

    /// Walks a freshly-loaded `MenuNav` on `dir` from the title screen to a
    /// singleplayer launch, and reports whether it got there.
    ///
    /// A **fresh** nav is the point: it re-reads `profiles.json` from disk, so
    /// this answers "would the next launch let this player play", which is the
    /// question the gate is actually about.
    fn a_fresh_launch_can_start_a_world(dir: &Path) -> bool {
        use crate::menu::nav::{MenuAction, MenuNav};
        use crate::menu::world_select::WorldSelectButton;
        let mut nav = MenuNav::with_path(dir.join("servers.json"));
        plant_world(nav.saves_root(), "planted");
        let mut ui = crate::menu::UiState::new();
        // Re-open the world list after planting, exactly as a player pressing
        // Singleplayer does — the list is read when the screen opens.
        let opened = nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(opened, MenuAction::None, "Singleplayer never launches directly");
        matches!(
            nav.click(&mut ui, WorldSelectButton::Play.row()),
            MenuAction::Singleplayer(..)
        )
    }

    /// **An account that authenticates but does not own the game leaves the gate
    /// closed.**
    ///
    /// The failure is the one the real chain produces for exactly that case —
    /// `AuthError::NoMinecraftProfile`, rendered through the same
    /// `describe_finish_interactive_failure` the worker calls — rather than a
    /// hand-written string, so the message this screen shows and the message
    /// production shows cannot drift apart.
    ///
    /// Note there is deliberately no "not entitled" flag on the stored account:
    /// such an account never produces an `AccountProfile` at all, because the
    /// roster is keyed on a Minecraft profile UUID it does not have. "A row
    /// exists" and "that account owned the game" are the same statement, and
    /// this test is what holds that equivalence up.
    #[test]
    fn an_account_that_does_not_own_the_game_never_reaches_the_roster_or_opens_the_gate() {
        let path = temp_path("not-entitled");
        let dir = path.parent().expect("the temp path has a parent").to_owned();
        let nav = AccountsNav::with_path(path.clone());
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        nav.hover(1 + BUTTON_ADD);
        nav.handle_key_with(MenuKey::Enter, spawn_stub(rx, cancel));

        let refusal =
            describe_finish_interactive_failure(&lodestone_auth::AuthError::NoMinecraftProfile);
        tx.send(WorkerMsg::Failed(refusal.clone())).unwrap();
        nav.pump();

        assert_eq!(
            nav.sign_in_view(),
            SignInView::Failed { message: refusal },
            "the screen must say the account has no Minecraft profile, not show a \
             generic error"
        );
        assert_eq!(
            nav.rows(),
            vec![AccountRow::Offline],
            "no account row may be added for an account that does not own the game"
        );
        assert!(
            nav.entitlement().is_none(),
            "a refused sign-in must not produce an ownership proof"
        );
        assert!(
            AccountsMetadata::load_from(&path).profiles.is_empty(),
            "nothing may be written to profiles.json"
        );
        assert!(
            !a_fresh_launch_can_start_a_world(&dir),
            "the next launch must still be gated after a sign-in that authenticated \
             but did not own the game"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// **The other arm of the same comparison**: the identical flow, ending in
    /// the success message instead, adds the account to the switcher *and* lets
    /// the next launch start a world.
    ///
    /// The two tests differ in one thing — which `WorkerMsg` the worker sends —
    /// so together they say the outcome tracks ownership rather than tracking
    /// "a sign-in was attempted".
    #[test]
    fn a_sign_in_that_owns_the_game_lands_in_the_switcher_and_opens_the_gate() {
        let path = temp_path("entitled");
        let dir = path.parent().expect("the temp path has a parent").to_owned();
        let nav = AccountsNav::with_path(path.clone());
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        nav.hover(1 + BUTTON_ADD);
        nav.handle_key_with(MenuKey::Enter, spawn_stub(rx, cancel));

        let steve = profile("Steve", 42);
        tx.send(WorkerMsg::SignedIn(steve.clone())).unwrap();
        nav.pump();

        // In the switcher, selected, and on disk — the same store the title
        // screen's Accounts row reads, not a parallel one.
        assert!(
            nav.rows().contains(&AccountRow::Account(steve.clone())),
            "the added account must appear in the account switcher's own list"
        );
        assert!(nav.is_selected(steve.profile_id));

        let entitlement = nav
            .entitlement()
            .expect("a completed sign-in must produce an ownership proof");
        assert_eq!(entitlement.profile_id(), steve.profile_id);
        assert_eq!(entitlement.username(), "Steve");

        assert!(
            a_fresh_launch_can_start_a_world(&dir),
            "the next launch must be able to start a singleplayer world"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_sign_in_shows_the_message_and_dismisses_on_enter() {
        let path = temp_path("failed-flow");
        let nav = AccountsNav::with_path(path.clone());
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        nav.hover(1 + BUTTON_ADD);
        nav.handle_key_with(MenuKey::Enter, spawn_stub(rx, cancel));
        tx.send(WorkerMsg::Failed(
            "device code expired before sign-in completed".to_string(),
        ))
        .unwrap();
        nav.pump();
        assert_eq!(
            nav.sign_in_view(),
            SignInView::Failed {
                message: "device code expired before sign-in completed".to_string()
            }
        );
        nav.handle_key(MenuKey::Enter);
        assert_eq!(nav.sign_in_view(), SignInView::Idle, "Enter dismisses the failure");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn describe_auth_error_never_needs_an_exhaustive_match() {
        // `AuthError` is `#[non_exhaustive]`; this just pins that the known
        // variants get a friendlier message than their raw `Display`.
        let e = lodestone_auth::AuthError::AuthorizationDeclined;
        assert_eq!(describe_auth_error(&e), "Sign-in was declined.");
        let e = lodestone_auth::AuthError::NoMinecraftProfile;
        assert!(describe_auth_error(&e).contains("No Minecraft profile was found"));
        // A variant with no special-case arm still renders something, via
        // `Display` — not a panic and not an empty string.
        let e = lodestone_auth::AuthError::Service {
            step: "xsts",
            message: "12345".to_string(),
        };
        assert!(describe_auth_error(&e).contains("xsts"));
    }

    #[test]
    fn a_typed_xsts_failure_shows_its_kind_specific_description_not_the_raw_body() {
        let e = lodestone_auth::AuthError::Xsts {
            kind: lodestone_auth::XstsErrorKind::ChildAccountNeedsFamily,
            message: "XErr=2148916238;some raw microsoft json".to_string(),
        };
        let shown = describe_auth_error(&e);
        assert!(shown.contains("child account"), "unhelpful message: {shown}");
        assert!(
            !shown.contains("XErr"),
            "the raw Microsoft response body must not leak into the UI text: {shown}"
        );
    }

    // -- the offline-name editor ---------------------------------------------

    /// A nav on a temp root, with `accounts` Microsoft profiles and `offline`
    /// already persisted as the offline name.
    ///
    /// Both files land in the *same* temp directory, which is the property the
    /// production `with_path` derivation is under test here: nothing in these
    /// tests names `offline_identity_path()`, so nothing can reach the
    /// developer's real `offline.json`.
    fn nav_with_offline(tag: &str, accounts: &[&str], offline: Option<&str>) -> (AccountsNav, PathBuf) {
        let path = temp_path(tag);
        let dir = path.parent().expect("temp_path always has a parent");
        std::fs::create_dir_all(dir).expect("temp dir");
        let mut meta = AccountsMetadata::default();
        for (i, name) in accounts.iter().enumerate() {
            meta.upsert(profile(name, (accounts.len() - i) as u64));
        }
        meta.save_to(&path).expect("temp profiles must be writable");
        if let Some(name) = offline {
            let mut id = OfflineIdentity::default();
            id.set_username(name).expect("fixture name must be valid");
            id.save_to(&dir.join("offline.json")).expect("temp offline must be writable");
        }
        let nav = AccountsNav::with_path(path.clone());
        (nav, path)
    }

    /// Drives the real production path: highlight the offline row, press the
    /// third footer button, type `typed`, press Enter.
    ///
    /// Every step is a `handle_key` through `handle_key_with`'s state machine —
    /// no direct field pokes — so this exercises the same code a keyboard does.
    /// `spawn_stub` is never reached (nothing here presses Add Account) but a
    /// `Spawn` has to be supplied, so it is fed a dead channel.
    fn edit_offline_name(nav: &AccountsNav, typed: &str) {
        let offline_row = nav.rows().len() - 1;
        while nav.highlighted() != offline_row {
            nav.handle_key(MenuKey::Down);
        }
        // Focus the third footer button and press it. `focus` past the end of
        // the list is how a mouse reaches a button, and `hover` is the only
        // writer of it — the same path `MenuNav::click` uses.
        nav.hover(nav.rows().len() + BUTTON_REMOVE);
        nav.handle_key(MenuKey::Enter);
        assert!(nav.is_editing_name(), "the third button did not open the editor");
        // Clear the seeded value the way a player would, then type.
        for _ in 0..NAME_MAX_LENGTH + 1 {
            nav.handle_key(MenuKey::Backspace);
        }
        assert_eq!(
            nav.name_edit_view().expect("still editing").edit.value(),
            "",
            "Backspace did not reach the field"
        );
        for ch in typed.chars() {
            nav.handle_key(MenuKey::Char(ch));
        }
        nav.handle_key(MenuKey::Enter);
    }

    #[test]
    fn the_offline_row_label_is_the_persisted_name_not_a_literal() {
        // The island this closes: `offline_identity` stored, validated and
        // UUID-derived a name that **nothing displayed**, because
        // `accounts_idle_frame` hardcoded `"Play offline"`.
        let (nav, path) = nav_with_offline("label", &[], Some("Steve"));
        assert_eq!(nav.offline_username(), "Steve");
        assert_eq!(nav.offline_uuid(), offline_uuid("Steve"));
        // Control: a fresh root shows the *default*, and in particular not the
        // pre-fix literal. Without this, "the label is Steve" is equally
        // consistent with a label that happens to echo the fixture.
        let (fresh, fresh_path) = nav_with_offline("label-default", &[], None);
        assert_eq!(fresh.offline_username(), crate::offline_identity::DEFAULT_USERNAME);
        assert_ne!(fresh.offline_username(), "Play offline");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
        let _ = std::fs::remove_dir_all(fresh_path.parent().unwrap());
    }

    #[test]
    fn the_third_button_is_remove_for_an_account_and_edit_name_for_the_offline_row() {
        // Both arms observed in one test, because a predicate that always
        // answered `EditName` would satisfy every other test in this section.
        let (nav, path) = nav_with_offline("third", &["Alex"], None);
        assert_eq!(nav.highlighted(), 0, "row 0 is the one account");
        assert_eq!(nav.third_button(), ThirdButton::Remove);
        assert_eq!(nav.third_button().label(), "Remove");
        nav.handle_key(MenuKey::Down);
        assert_eq!(nav.highlighted(), 1, "row 1 is the offline entry");
        assert_eq!(nav.third_button(), ThirdButton::EditName);
        assert_eq!(nav.third_button().label(), "Edit Name");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn editing_the_name_persists_it_and_the_row_follows() {
        let (nav, path) = nav_with_offline("edit", &[], Some("Steve"));
        edit_offline_name(&nav, "Notch");

        assert!(!nav.is_editing_name(), "a valid name must close the editor");
        assert_eq!(nav.offline_username(), "Notch", "the live name did not change");
        assert_eq!(nav.save_error(), None, "a temp dir write must not fail");
        // **The expected UUID comes from outside this module**: it is one of
        // `offline_identity`'s externally-computed vectors (CPython's
        // `hashlib.md5` plus the documented `nameUUIDFromBytes` stamping), not
        // `offline_uuid("Notch")` re-derived here.
        assert_eq!(
            nav.offline_uuid(),
            Uuid::parse_str("b50ad385-829d-3141-a216-7e7d7539ba7f").unwrap(),
            "the derived identity must follow the name"
        );
        // And it reached the file, read back through the loader production uses.
        let offline_file = path.parent().unwrap().join("offline.json");
        assert_eq!(
            OfflineIdentity::load_from(&offline_file).username(),
            "Notch",
            "the name was not persisted to {}",
            offline_file.display()
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_refused_name_keeps_the_old_one_live_shows_why_and_writes_nothing() {
        let (nav, path) = nav_with_offline("refused", &[], Some("Steve"));
        let offline_file = path.parent().unwrap().join("offline.json");
        edit_offline_name(&nav, "has space");

        assert!(
            nav.is_editing_name(),
            "a refused name must leave the editor open so it can be corrected"
        );
        let view = nav.name_edit_view().expect("still editing");
        // The message is `NameError`'s own `Display`, not one written twice.
        assert_eq!(
            view.error.as_deref(),
            Some(
                crate::offline_identity::NameError::IllegalCharacter
                    .to_string()
                    .as_str()
            ),
            "the refusal reason is not the validator's own text"
        );
        assert_eq!(view.edit.value(), "has space", "the typed text must survive the refusal");
        assert_eq!(nav.offline_username(), "Steve", "the old name must stay live");
        assert_eq!(
            OfflineIdentity::load_from(&offline_file).username(),
            "Steve",
            "a refused name must not reach the file"
        );

        // Control: the same editor *can* still commit, so the assertions above
        // are not passing because commit is broken outright.
        for _ in 0..NAME_MAX_LENGTH + 1 {
            nav.handle_key(MenuKey::Backspace);
        }
        for ch in "Dev".chars() {
            nav.handle_key(MenuKey::Char(ch));
        }
        nav.handle_key(MenuKey::Enter);
        assert!(!nav.is_editing_name(), "a corrected name must commit");
        assert_eq!(
            OfflineIdentity::load_from(&offline_file).username(),
            "Dev",
            "the corrected name did not reach the file"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_length_cap_stops_the_seventeenth_character_rather_than_refusing_on_done() {
        let (nav, path) = nav_with_offline("cap", &[], None);
        // 17 characters typed; the box must hold 16 and the commit must succeed,
        // rather than the player learning on Done that the name was too long.
        edit_offline_name(&nav, "0123456789abcdefg");
        assert!(!nav.is_editing_name(), "a capped name is a valid name");
        assert_eq!(nav.offline_username(), "0123456789abcdef");
        assert_eq!(nav.offline_username().chars().count(), NAME_MAX_LENGTH);
        // Control: the *validator* really would have refused the untruncated
        // string, so the cap is doing work rather than the name being short.
        assert_eq!(
            crate::offline_identity::validate_username("0123456789abcdefg"),
            Err(crate::offline_identity::NameError::TooLong)
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn escape_abandons_the_edit_and_writes_nothing() {
        let (nav, path) = nav_with_offline("escape", &[], Some("Steve"));
        let offline_file = path.parent().unwrap().join("offline.json");
        let offline_row = nav.rows().len() - 1;
        while nav.highlighted() != offline_row {
            nav.handle_key(MenuKey::Down);
        }
        nav.hover(nav.rows().len() + BUTTON_REMOVE);
        nav.handle_key(MenuKey::Enter);
        for ch in "Notch".chars() {
            nav.handle_key(MenuKey::Char(ch));
        }
        assert_eq!(
            nav.handle_key(MenuKey::Escape),
            AccountsSignal::None,
            "Escape closes the editor, not the screen"
        );
        assert!(!nav.is_editing_name());
        assert_eq!(nav.offline_username(), "Steve");
        assert_eq!(OfflineIdentity::load_from(&offline_file).username(), "Steve");
        // ...and the screen is still there to leave, which is what makes the
        // `AccountsSignal::None` above meaningful.
        assert_eq!(nav.handle_key(MenuKey::Escape), AccountsSignal::Back);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn the_shown_uuid_follows_the_typed_name_not_the_saved_one() {
        // The editor's whole reason for showing a UUID: the name *is* the
        // identity, so the consequence must be visible before Done.
        let (nav, path) = nav_with_offline("uuid", &[], Some("Steve"));
        let offline_row = nav.rows().len() - 1;
        while nav.highlighted() != offline_row {
            nav.handle_key(MenuKey::Down);
        }
        nav.hover(nav.rows().len() + BUTTON_REMOVE);
        nav.handle_key(MenuKey::Enter);
        // Seeded from the stored name, so it starts equal to the saved identity.
        assert_eq!(nav.name_edit_view().unwrap().uuid, nav.offline_uuid());
        for _ in 0..NAME_MAX_LENGTH + 1 {
            nav.handle_key(MenuKey::Backspace);
        }
        for ch in "Notch".chars() {
            nav.handle_key(MenuKey::Char(ch));
        }
        let view = nav.name_edit_view().unwrap();
        // External vector again, and it differs from the still-saved one — which
        // is the assertion a view reading `identity.uuid()` would fail.
        assert_eq!(
            view.uuid,
            Uuid::parse_str("b50ad385-829d-3141-a216-7e7d7539ba7f").unwrap()
        );
        assert_ne!(
            view.uuid,
            nav.offline_uuid(),
            "mid-edit the shown UUID must be the typed name's, not the saved name's"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_key_that_means_something_to_the_list_is_swallowed_by_the_open_editor() {
        // **The layered-guard control.** `Delete` on this screen removes the
        // highlighted account; while a name is being edited it must delete a
        // *character*. The editor is only reachable with the offline row
        // highlighted, so `remove_highlighted` would refuse anyway — which would
        // make a test driven purely through the UI vacuous. The state is
        // therefore poked directly: an account row highlighted **and** the editor
        // open, a combination the UI cannot reach, so that the assertion is about
        // `handle_key_with`'s ordering and nothing else.
        let (nav, path) = nav_with_offline("swallow", &["Alex", "Steve"], None);
        {
            let mut st = nav.state.borrow_mut();
            st.highlighted = 0;
            begin_name_edit(&mut st);
            st.name_edit.as_mut().unwrap().edit.set_value("abc");
            // `move_cursor_to`, **not** `set_cursor_position`: the latter moves
            // the caret and leaves `highlight_pos` at the end, so "abc" is a live
            // *selection* and `delete_text` deletes all of it. Measured — the
            // first draft of this test asserted `"bc"` and got `""`.
            st.name_edit.as_mut().unwrap().edit.move_cursor_to(0, false);
        }
        assert_eq!(nav.ordered().len(), 2, "precondition: two accounts to lose");
        nav.handle_key(MenuKey::Delete);
        assert_eq!(
            nav.ordered().len(),
            2,
            "Delete while editing removed an account — the editor's branch does \
             not come first in `handle_key_with`"
        );
        assert_eq!(
            nav.name_edit_view().unwrap().edit.value(),
            "bc",
            "Delete did not reach the field either, so this test proves nothing"
        );

        // The control: with the editor **closed**, the very same key really does
        // remove that account. Without this, "two accounts survived" is equally
        // consistent with a `Delete` that never removes anything.
        nav.handle_key(MenuKey::Escape);
        assert!(!nav.is_editing_name());
        nav.handle_key(MenuKey::Delete);
        assert_eq!(
            nav.ordered().len(),
            1,
            "the control failed: Delete does not remove an account even with the \
             editor closed, so the assertion above measures nothing"
        );
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn a_click_on_the_field_does_not_save_but_a_click_on_done_does() {
        // The two rows of the editor's frame, through the path `MenuNav::click`
        // routes to (`nav.rs`'s `Screen::Accounts` arm). Row 0 is an
        // always-focused field: clicking it used to arrive as `hover` + `Enter`
        // and therefore *save*, which is that fix's shape on a sixth screen.
        let (nav, path) = nav_with_offline("click", &[], Some("Steve"));
        let offline_file = path.parent().unwrap().join("offline.json");
        let offline_row = nav.rows().len() - 1;
        while nav.highlighted() != offline_row {
            nav.handle_key(MenuKey::Down);
        }
        nav.hover(nav.rows().len() + BUTTON_REMOVE);
        nav.handle_key(MenuKey::Enter);
        for _ in 0..NAME_MAX_LENGTH + 1 {
            nav.handle_key(MenuKey::Backspace);
        }
        for ch in "Notch".chars() {
            nav.handle_key(MenuKey::Char(ch));
        }

        nav.click_name_edit_row(NAME_EDIT_FIELD_ROW);
        assert!(nav.is_editing_name(), "a click on the field must not save");
        assert_eq!(
            OfflineIdentity::load_from(&offline_file).username(),
            "Steve",
            "a click on the field wrote the file"
        );
        // And a hover while the editor is open must move neither cursor: the
        // editor's frame has no list rows, so a row index there means nothing —
        // the same argument `hover`'s sign-in guard already makes.
        let (highlighted, focus) = (nav.highlighted(), nav.focus());
        nav.hover(0);
        assert_eq!(nav.highlighted(), highlighted, "hover moved the selection");
        assert_eq!(nav.focus(), focus, "hover moved the button focus");

        nav.click_name_edit_row(NAME_EDIT_DONE_ROW);
        assert!(!nav.is_editing_name(), "a click on Done must save");
        assert_eq!(OfflineIdentity::load_from(&offline_file).username(), "Notch");
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
