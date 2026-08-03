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

use super::nav::MenuKey;

/// Rows of the account list visible at once. The list scrolls past this,
/// rather than the server list's current unbounded stack — see
/// `docs/main-menu.md`'s "left for polish" list, item 3, for why that one
/// still doesn't and why fixing it here first (a new screen, not shared
/// code) does not fix it there too.
///
/// **A count, not a measurement**, and that is the residual gap #402 records:
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
    /// [`run_device_code_login`]) — only the metadata write is left, and that
    /// happens in [`AccountsNav::pump`] on the render thread, so every
    /// `profiles.json` write goes through one place.
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
    /// First visible logical row index of the scrolling window.
    scroll: usize,
    save_error: Option<String>,
    sign_in: SignIn,
}

/// Account list + sign-in flow state for [`Screen::Accounts`](super::Screen).
pub struct AccountsNav {
    path: PathBuf,
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
    #[must_use]
    pub fn with_path(path: PathBuf) -> Self {
        Self {
            state: RefCell::new(State {
                metadata: AccountsMetadata::load_from(&path),
                highlighted: 0,
                focus: 0,
                scroll: 0,
                save_error: None,
                sign_in: SignIn::Idle,
            }),
            path,
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

    /// Whether `selected` (the metadata's own field) points at nothing, i.e.
    /// offline mode is active — see the module docs.
    #[must_use]
    pub fn offline_selected(&self) -> bool {
        self.state.borrow().metadata.selected.is_none()
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

    /// First visible row of the scrolling window.
    #[must_use]
    pub fn scroll(&self) -> usize {
        self.state.borrow().scroll
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
                st.scroll = 0;
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
        let list_len = st.metadata.profiles.len() + 1;
        let shown = list_len.saturating_sub(st.scroll).min(VISIBLE_ROWS);
        if rendered_row < shown {
            let logical = st.scroll + rendered_row;
            st.highlighted = logical;
            st.focus = logical;
        } else {
            let button = rendered_row - shown;
            if button < BUTTON_COUNT {
                st.focus = list_len + button;
            }
        }
    }

    /// Handles one key with a real worker spawn (a genuine background thread
    /// against live Microsoft endpoints). See [`Self::handle_key_with`] for
    /// the seam tests use instead.
    pub fn handle_key(&self, key: MenuKey) -> AccountsSignal {
        self.handle_key_with(
            key,
            Box::new(|| {
                let (tx, rx) = channel();
                let cancel = Arc::new(AtomicBool::new(false));
                let worker_cancel = Arc::clone(&cancel);
                // The **loopback** flow, not the device-code one: it opens the real
                // Microsoft login in the user's browser and needs no code typed.
                // `run_device_code_login` is kept beside it and still compiled —
                // it is the only option on a headless host, and it is the fallback
                // if the browser cannot be launched.
                std::thread::spawn(move || run_browser_login(tx, worker_cancel));
                (rx, cancel)
            }),
        )
    }

    /// The real state machine, parameterised over how "Add account" spawns
    /// its worker — see [`Spawn`]. Kept as a normal (non-pub) method so
    /// production code always goes through [`Self::handle_key`]; tests reach
    /// it directly with a hand-fed channel.
    fn handle_key_with(&self, key: MenuKey, spawn: Spawn) -> AccountsSignal {
        let mut st = self.state.borrow_mut();
        let list_len = st.metadata.profiles.len() + 1;

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
    let secrets = lodestone_auth::AccountSecrets::open();
    if let Err(e) = secrets.delete_refresh_token(id) {
        st.save_error = Some(format!("could not remove the stored credential: {e}"));
        return;
    }
    st.metadata.remove(id);
    st.save_error = st.metadata.save_to(path).err().map(|e| e.to_string());
    let list_len = st.metadata.profiles.len() + 1;
    st.highlighted = st.highlighted.min(list_len - 1);
    st.focus = st.highlighted;
    scroll_to_show(st);
}

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
        BUTTON_REMOVE => {
            remove_highlighted(st, path);
            AccountsSignal::None
        }
        BUTTON_CANCEL => AccountsSignal::Back,
        _ => AccountsSignal::None,
    }
}

fn handle_key_mid_flow(st: &mut State, key: MenuKey) -> AccountsSignal {
    match key {
        // `Enter` as well as `Escape`, because the sign-in screen now *has* a
        // Cancel button and a click on it arrives here as `hover` + `Enter`
        // (`MenuNav::click`'s default translation). Cancel is the only control on
        // the screen while a sign-in is in flight, so "activate the focused
        // widget" and "cancel" are the same verb — without this the button would
        // draw, highlight, and do nothing, which is #391's shape.
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
            if let SignIn::Waiting { user_code, .. } = &st.sign_in {
                copy_to_clipboard(user_code);
            }
            AccountsSignal::None
        }
        _ => AccountsSignal::None,
    }
}

fn scroll_to_show(st: &mut State) {
    if st.highlighted < st.scroll {
        st.scroll = st.highlighted;
    } else if st.highlighted >= st.scroll + VISIBLE_ROWS {
        st.scroll = st.highlighted + 1 - VISIBLE_ROWS;
    }
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
                let effect = Some((Some(verification_uri.clone()), None));
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
/// write *before* issue #65 landed [`lodestone_auth::AuthError::Xsts`] and
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
        E::NoMinecraftProfile => "This Microsoft account does not own Minecraft.".to_string(),
        // `kind.describe()` is the short, user-facing text; `message` is
        // Microsoft's own raw (English, developer-oriented) response body —
        // see the variant's own doc comment on why the UI wants the former.
        E::Xsts { kind, .. } => kind.describe().to_string(),
        other => other.to_string(),
    }
}

/// Runs the full device-code → Xbox Live → XSTS → Minecraft-services chain on
/// its own thread with its own single-threaded runtime, mirroring
/// `menu/status.rs`'s per-probe thread. The keychain save happens here, on
/// the worker thread, because it is this thread that holds the refresh
/// token; the metadata write happens back on the render thread inside
/// [`AccountsNav::pump`], so every `profiles.json` write funnels through one
/// place rather than racing a foreground Remove.
fn run_device_code_login(tx: Sender<WorkerMsg>, cancel: Arc<AtomicBool>) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Failed(format!("could not start a runtime: {e}")));
            return;
        }
    };
    rt.block_on(async move {
        // Not `flow::MOJANG_CLIENT_ID`: that is the *official launcher's*
        // registered Azure application id, and `lodestone_auth::login`'s own
        // docs are explicit that using it "would misrepresent this client to
        // Microsoft, not just violate a style rule". `resolve_client_id`
        // reads `LODESTONE_MS_CLIENT_ID`, typed-erroring
        // (`AuthError::MissingClientId`) rather than silently falling back —
        // see `docs/accounts.md`'s configuration section.
        let client_id = match lodestone_auth::login::resolve_client_id() {
            Ok(id) => id,
            Err(e) => {
                let _ = tx.send(WorkerMsg::Failed(describe_auth_error(&e)));
                return;
            }
        };
        let client = reqwest::Client::new();
        let mut pending = match lodestone_auth::flow::PendingLogin::begin(&client, &client_id).await {
            Ok(p) => p,
            Err(e) => {
                let _ = tx.send(WorkerMsg::Failed(describe_auth_error(&e)));
                return;
            }
        };
        let prompt = pending.prompt();
        let _ = tx.send(WorkerMsg::Prompt {
            user_code: prompt.user_code.clone(),
            verification_uri: prompt.verification_uri.clone(),
        });

        loop {
            if cancellable_sleep(pending.interval(), &cancel).await {
                let _ = tx.send(WorkerMsg::Cancelled);
                return;
            }
            match pending.poll_once(&client, &client_id).await {
                Ok(None) => continue,
                Ok(Some(ms_token)) => {
                    let session =
                        match lodestone_auth::flow::session_from_ms_token(&client, &ms_token.access_token).await {
                            Ok(s) => s,
                            Err(e) => {
                                let _ = tx.send(WorkerMsg::Failed(describe_auth_error(&e)));
                                return;
                            }
                        };
                    let secrets = lodestone_auth::AccountSecrets::open();
                    if let Err(e) = secrets.save_refresh_token(session.profile.id, &ms_token.refresh_token) {
                        let _ = tx.send(WorkerMsg::Failed(format!(
                            "signed in, but could not save the credential: {e}"
                        )));
                        return;
                    }
                    let now = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .map(|d| d.as_secs())
                        .unwrap_or(0);
                    let profile = AccountProfile {
                        profile_id: session.profile.id,
                        username: session.profile.name.clone(),
                        skin_url: None,
                        last_used: now,
                    };
                    let _ = tx.send(WorkerMsg::SignedIn(profile));
                    return;
                }
                Err(e) => {
                    let _ = tx.send(WorkerMsg::Failed(describe_auth_error(&e)));
                    return;
                }
            }
        }
    });
}

/// Runs the **loopback** sign-in: the real Microsoft login page in the user's
/// browser, no code to type. This is what Add Account uses.
///
/// Everything from the `MsToken` onward is identical to
/// [`run_device_code_login`] — same Xbox Live → XSTS → Minecraft-services chain,
/// same keychain save here on the worker thread, same `SignedIn` message with the
/// metadata write left to [`AccountsNav::pump`]. Only how the authorization code
/// arrives differs, which is the whole reason
/// [`lodestone_auth::browser_login`] was shaped to mirror
/// `flow::PendingLogin`'s `poll_once`.
///
/// The URL still goes to the screen as [`WorkerMsg::Prompt`]'s
/// `verification_uri`, with an **empty** `user_code`: there is no code in this
/// flow, and the URL is the copy-paste fallback for when the browser cannot be
/// launched. `render.rs` renders an empty code as "no code to show".
fn run_browser_login(tx: Sender<WorkerMsg>, cancel: Arc<AtomicBool>) {
    let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
        Ok(rt) => rt,
        Err(e) => {
            let _ = tx.send(WorkerMsg::Failed(format!("could not start a runtime: {e}")));
            return;
        }
    };
    rt.block_on(async move {
        // Same refusal to fall back to the official launcher's id as the
        // device-code path — see `run_device_code_login`'s comment and
        // `lodestone_auth::login`'s docs.
        let client_id = match lodestone_auth::login::resolve_client_id() {
            Ok(id) => id,
            Err(e) => {
                let _ = tx.send(WorkerMsg::Failed(describe_auth_error(&e)));
                return;
            }
        };
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
async fn finish_ms_token(
    tx: &Sender<WorkerMsg>,
    client: &reqwest::Client,
    ms_token: lodestone_auth::flow::MsToken,
) {
    let session =
        match lodestone_auth::flow::session_from_ms_token(client, &ms_token.access_token).await {
            Ok(s) => s,
            Err(e) => {
                // **This is the arm a real sign-in failure takes**, and its
                // silence is why a failed attempt left no log line for the
                // failing step: `run_browser_login`'s `poll_once` arm logs, but
                // that one only fires before the browser hands the code back.
                // `AuthError`'s `Debug` carries the step and the untruncated
                // response body that `describe_auth_error` flattens to one
                // sentence for display, and the on-screen string is transient
                // and uncopyable — so this line is the only thing that makes a
                // failure diagnosable after the fact.
                tracing::warn!(target: "auth", error = ?e, "sign-in failed after the browser step");
                let _ = tx.send(WorkerMsg::Failed(describe_auth_error(&e)));
                return;
            }
        };
    let secrets = lodestone_auth::AccountSecrets::open();
    if let Err(e) = secrets.save_refresh_token(session.profile.id, &ms_token.refresh_token) {
        let _ = tx.send(WorkerMsg::Failed(format!(
            "signed in, but could not save the credential: {e}"
        )));
        return;
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let _ = tx.send(WorkerMsg::SignedIn(AccountProfile {
        profile_id: session.profile.id,
        username: session.profile.name.clone(),
        skin_url: None,
        last_used: now,
    }));
}

/// [`cancellable_sleep`] in milliseconds, for the loopback flow's tighter poll.
///
/// Separate rather than making the existing function take millis: that one's
/// `secs` argument comes straight from Microsoft's `interval` field, and widening
/// it would invite passing a millisecond value where a second value is meant.
async fn cancellable_sleep_ms(millis: u64, cancel: &AtomicBool) -> bool {
    if cancel.load(Ordering::Relaxed) {
        return true;
    }
    tokio::time::sleep(std::time::Duration::from_millis(millis)).await;
    cancel.load(Ordering::Relaxed)
}

/// Sleeps up to `secs` seconds, checking `cancel` every 100ms so an
/// interactive Cancel keypress is felt quickly rather than after a whole
/// multi-second poll interval. Returns `true` if cancelled mid-sleep.
async fn cancellable_sleep(secs: u64, cancel: &AtomicBool) -> bool {
    let mut remaining = std::time::Duration::from_secs(secs);
    let step = std::time::Duration::from_millis(100);
    while remaining > std::time::Duration::ZERO {
        if cancel.load(Ordering::Relaxed) {
            return true;
        }
        let this_step = remaining.min(step);
        tokio::time::sleep(this_step).await;
        remaining -= this_step;
    }
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
fn open_in_browser(url: &str) {
    #[cfg(target_os = "macos")]
    let _ = std::process::Command::new("open").arg(url).spawn();
    #[cfg(target_os = "windows")]
    let _ = std::process::Command::new("cmd").args(["/C", "start", "", url]).spawn();
    #[cfg(all(unix, not(target_os = "macos")))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// Best-effort: copies `text` to the system clipboard via the same
/// no-new-dependency OS-command approach as [`open_in_browser`].
fn copy_to_clipboard(text: &str) {
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

    #[test]
    fn hover_maps_a_rendered_row_back_through_the_scroll_offset() {
        let path = temp_path("hover-scroll");
        let mut meta = AccountsMetadata::default();
        for i in 0..8u64 {
            meta.upsert(profile(&format!("p{i}"), i));
        }
        meta.save_to(&path).unwrap();
        let nav = AccountsNav::with_path(path.clone());
        // 8 accounts + 1 offline = 9 logical rows, VISIBLE_ROWS = 5.
        for _ in 0..7 {
            nav.handle_key(MenuKey::Down);
        }
        assert_eq!(nav.highlighted(), 7);
        let scroll = nav.scroll();
        assert!(scroll > 0, "highlighting row 7 with only 5 visible must have scrolled");
        nav.hover(0);
        assert_eq!(nav.highlighted(), scroll, "rendered row 0 on screen is `scroll` logically");
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
            verification_uri: "https://microsoft.com/link".to_string(),
        })
        .unwrap();
        nav.pump();
        assert_eq!(
            nav.sign_in_view(),
            SignInView::Waiting {
                user_code: "ABCD-EFGH".to_string(),
                verification_uri: "https://microsoft.com/link".to_string(),
            }
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
        assert!(describe_auth_error(&e).contains("does not own Minecraft"));
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
}
