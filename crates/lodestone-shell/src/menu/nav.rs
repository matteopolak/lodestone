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

use super::book_edit;
use super::command_block;
use super::edit_box::EditBox;
use super::sign_edit;
use super::focus::{self, FocusChildren, FocusSet, FocusTarget, KeyEvent, KeyOutcome};
use super::servers::{MAX_NAME_CHARS, ServerEntry, ServerList, servers_path};
use super::widget;
use super::options::LiveOption;
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
    /// **F5** — refresh the multiplayer list (#396).
    ///
    /// Its own variant rather than a reuse of `Char('r')`, because it is a
    /// *function* key: on [`Screen::ServerEdit`] a `Char` is text, and mapping F5
    /// onto one would type an `r` into the address field. `focus::KEY_F5` is the
    /// GLFW code `JoinMultiplayerScreen.keyPressed` tests for (`:234`).
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
    /// whole reason creating a fresh directory is the right fix for #468's wart
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    /// Nothing to do; the menu handled it internally.
    None,
    /// Enter a singleplayer world: start the integrated server in-process
    /// against that world's directory and connect to it (issues #287, #468).
    ///
    /// Two producers, and the payload says which: [`Screen::WorldSelect`]'s
    /// **Play Selected World** ([`SingleplayerLaunch::Open`]) and
    /// [`Screen::CreateWorld`]'s **Create** ([`SingleplayerLaunch::Created`]).
    /// `app.rs`'s arm calls `begin_singleplayer`, which takes exactly what this
    /// variant carries.
    ///
    /// It used to carry `Option<WorldCreationConfig>` — `None` meaning "the one
    /// implicit world at `saves/world`". That was issue #468's reading (1) and it
    /// is why Create New World could not create a second world; see
    /// [`crate::saves`]'s module doc.
    ///
    /// Between #397 and #287 this variant had **no producer at all** and was
    /// kept as the seam the integrated server would land on. It is worth naming
    /// because "the variant exists and is matched" was true throughout and is
    /// exactly what an island looks like from the inside.
    Singleplayer(SingleplayerLaunch),
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
    /// The player asked for a **refresh** — F5 or the Refresh button (#396).
    ///
    /// Distinct from [`MenuAction::Reprobe`]`(None)`, and the distinction is
    /// load-bearing rather than tidy: `Reprobe(None)` means "make sure every row
    /// has been probed", which `StatusCache::refresh` answers by *skipping* every
    /// address it already has a result for. A refresh that skipped every row is a
    /// button that does nothing. This one discards the cached results first — which
    /// is also what vanilla does, by throwing the whole screen away and building a
    /// new one with a fresh `ServerList` (`JoinMultiplayerScreen.java`).
    RefreshList,
    /// The pause menu's "Quit to Title" was activated, or (issue #103) the
    /// death screen's "Title Screen" button was: [`UiState`] has already moved
    /// to [`Screen::MainMenu`] (see [`UiState::quit_to_title`]); the app must
    /// now tear down whatever live session (net connection and/or integrated
    /// server) is still attached to `Sim`, exactly as it would for an
    /// ordinary disconnect — nothing here does that on its own, since
    /// `MenuNav` holds no session state to tear down.
    QuitToTitle,
    /// The death screen's Respawn button was activated (issue #103): the app
    /// must call `Sim::respawn` to submit the manual `ClientAction::Respawn`
    /// — `MenuNav` holds no `Sim` to send it through. [`UiState`] stays on
    /// [`Screen::Death`] until the server confirms the respawn (see
    /// `net::NetUpdate::Respawned`), so a duplicate click before that lands
    /// just resubmits the same request — harmless, since `Sim::respawn` is a
    /// no-op once `Sim::is_dead` has already gone false.
    Respawn,
    /// The command-block screen's **Done** button was activated (issue #47):
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
    /// though this variant now has a real consumer. Tracked as
    /// [#442](https://github.com/matteopolak/lodestone/issues/442). What #47's
    /// half fixes is the *submit* path, which was the island: the Done button
    /// computed a fully-tested payload and dropped it on the floor.
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
    /// The pause menu's **Open to LAN** was activated (issue #535): the app must
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
}

/// Which field of the add/edit form has focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    /// The display label.
    Name,
    /// `host` or `host:port`.
    Address,
}

impl Default for FormField {
    fn default() -> Self {
        Self::Name
    }
}

/// [`EditForm`]'s name field, as a [`super::focus::FocusChildren`] id.
///
/// The ids double as the row indices [`super::render::frame_for`] builds and
/// `app.rs`'s hit-test reports, which is why they are `0`/`1` and not opaque —
/// `the_form_field_ids_are_the_row_indices_the_mouse_reports` asserts the two
/// still agree.
pub const NAME_FIELD: usize = 0;
/// [`EditForm`]'s address field. See [`NAME_FIELD`].
pub const ADDRESS_FIELD: usize = 1;

/// Row indices [`crate::menu::render::screens::sign_edit_frame`] builds and
/// this module's own hover/click routing agree on — the [`sign_edit`] screen's
/// version of [`NAME_FIELD`]/[`ADDRESS_FIELD`] above.
pub mod sign_edit_row {
    /// The four line fields, top to bottom.
    pub const LINES: std::ops::Range<usize> = 0..super::sign_edit::LINE_COUNT;
    /// The Done button — the only other row this screen draws.
    pub const DONE: usize = super::sign_edit::LINE_COUNT;
}
/// `ManageServerScreen`'s `manageServer.resourcePack` cycle button
/// (`ManageServerScreen.java`). **Live**: a click cycles
/// [`super::servers::ServerPackPolicy`] (`MenuNav::click`'s `ServerEdit`
/// arm), and the value is what a live join now reads to decide whether a
/// pushed resource pack is silently applied, silently declined, or prompted
/// — see `net.rs`'s resource-pack flow. This row used to be present and
/// permanently inactive, on the grounds that `ServerEntry` carried no
/// `pack_status` field to cycle; that gap is closed.
pub const RESOURCE_PACK_ROW: usize = 2;
/// `CommonComponents.GUI_DONE` (`ManageServerScreen.java`) — saves the
/// form. A real, clickable row alongside the existing Enter/Tab keyboard path
/// (see [`MenuNav::click`]'s `Screen::ServerEdit` arm).
pub const DONE_ROW: usize = 3;
/// `CommonComponents.GUI_CANCEL` (`ManageServerScreen.java`) — discards
/// the form. See [`DONE_ROW`].
pub const CANCEL_ROW: usize = 4;

/// The logical canvas [`EditForm`]'s boxes are seeded against.
///
/// It matters for exactly two things — the **relative** y order of the two
/// fields (which is what makes Up/Down move between them, since arrow
/// navigation is geometric) and the box **width** `displayPos` scrolls against.
/// `super::render::row_rect` centres the stack vertically, so the ordering holds
/// at every canvas, and it clamps the width to `ROW_W` at every canvas at least
/// `ROW_W + 2 * PAD` wide — so a seeded box is correct everywhere that is not a
/// pathologically narrow window.
///
/// It is a *seed*, not the draw geometry: `super::render::build` moves a
/// per-frame clone of each box into that frame's real rect (see
/// `super::render`'s `draw_edit_box`), which is `OptionsSubScreen`'s
/// reposition-don't-rebuild order. A `&mut MenuNav` per frame would let the
/// originals be repositioned instead, and `frame_for` takes `&MenuNav` — see
/// `docs/menu-focus.md` on why that is `app.rs`'s call to make.
const SEED_CANVAS: (f32, f32) = (854.0, 480.0);

/// The two [`EditBox`]es [`EditForm`] owns, as one struct so
/// [`super::focus::FocusSet`] can borrow them while `EditForm` borrows the set.
///
/// **This split is load-bearing, not cosmetic.** `FocusSet`'s methods take
/// `&mut dyn FocusChildren`, and a `FocusSet` living in the same struct as the
/// children it dispatches to could not be called at all — `&mut self.focus` and
/// `&mut self` are not disjoint. Vanilla has the same shape (a `Screen` holds
/// both) and no such rule.
#[derive(Debug, Clone, PartialEq)]
pub struct FormFields {
    /// The display label. Capped at [`MAX_NAME_CHARS`].
    pub name: EditBox,
    /// `host` or `host:port`. Capped at [`MAX_ADDRESS_CHARS`].
    pub address: EditBox,
}

impl FocusChildren for FormFields {
    fn get(&self, id: usize) -> Option<&dyn FocusTarget> {
        match id {
            NAME_FIELD => Some(&self.name as &dyn FocusTarget),
            ADDRESS_FIELD => Some(&self.address as &dyn FocusTarget),
            _ => None,
        }
    }

    fn get_mut(&mut self, id: usize) -> Option<&mut dyn FocusTarget> {
        match id {
            NAME_FIELD => Some(&mut self.name as &mut dyn FocusTarget),
            ADDRESS_FIELD => Some(&mut self.address as &mut dyn FocusTarget),
            _ => None,
        }
    }
}

/// What one key did to the form, from the screen's point of view.
///
/// Only [`Self::Save`] and [`Self::Cancel`] need the screen's cooperation; every
/// other keystroke was fully answered by the focused [`EditBox`] or by focus
/// navigation. This is the distinction [`super::focus::KeyOutcome`] exists to
/// preserve and vanilla's `boolean` throws away.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormOutcome {
    /// The field or the focus layer dealt with it.
    Handled,
    /// Escape: discard the form.
    Cancel,
    /// Enter, and nothing else wanted it: save.
    Save,
}

/// The add/edit form's contents: two real [`EditBox`]es and the focus that
/// decides which of them a keystroke reaches.
///
/// The address is held as the **single string the user typed** and split into
/// host/port only on save. Splitting per keystroke would make `mc.example.com:2`
/// unrepresentable halfway through typing `:25565`.
///
/// ## These widgets outlive a frame, and that is the point
///
/// Every other screen in this shell rebuilds its rows — labels included — in
/// [`super::render::frame_for`], every frame. That is fine for a button, whose
/// whole state is derivable, and impossible for a text field: rebuilding one
/// would reset the caret, the selection and the scroll offset sixty times a
/// second. `Screen.rebuildWidgets` has exactly this consequence in vanilla too
/// — it calls `clearFocus()` (`Screen.java`), so a rebuilt screen has no
/// focus by construction.
///
/// So this is the first menu state in the shell that is *widget* state rather
/// than derived state, and #394's note that `OptionsSubScreen`'s
/// build→reposition order "becomes the right one once a widget holds state" is
/// where it lands. The cost is one clone of each box per frame, in
/// [`super::render`]'s `draw_edit_box`, which is what stands in for the
/// reposition a `&mut` frame hook would do in place.
#[derive(Debug, Clone, PartialEq)]
pub struct EditForm {
    /// The two fields. Public so [`super::render`] can read a box's own
    /// geometry, value, caret and selection rather than re-deriving them.
    pub fields: FormFields,
    /// Which field has focus, and the Tab/arrow traversal between them.
    focus: FocusSet,
    /// Index being edited, or `None` when adding a new entry.
    pub editing: Option<usize>,
    /// Which of [`RESOURCE_PACK_ROW`]/[`DONE_ROW`]/[`CANCEL_ROW`] the mouse is
    /// over, if any — separate from [`Self::field`] the same way
    /// `WorldSelectNav::hovered` is separate from its own focus, and for the
    /// same reason: those three rows are buttons, not text fields, so a mouse
    /// hovering one must not steal keyboard focus out of whichever field it
    /// was in. See [`super::render::MenuFrame::hovered`].
    hovered: Option<usize>,
    /// The `ManageServerScreen`'s resource-pack `CycleButton` value — see
    /// [`super::servers::ServerPackPolicy`]. Seeded from the entry being
    /// edited, or [`super::servers::ServerPackPolicy::default`] (`Prompt`)
    /// for a new one, and carried into [`Self::to_entry`].
    pack_status: super::servers::ServerPackPolicy,
}

impl Default for EditForm {
    fn default() -> Self {
        Self::adding()
    }
}

impl EditForm {
    /// A blank form for a new entry, focused on the name field.
    ///
    /// The initial focus is set explicitly, which is
    /// `Screen.setInitialFocus(GuiEventListener)` (`Screen.java`) rather
    /// than the no-argument overload — that one is gated on
    /// `minecraft.getLastInputType().isKeyboard()`, a piece of state this shell
    /// does not track. Without it the form would open with **nothing** focused
    /// and the first keystroke would go nowhere, which is precisely the island
    /// this issue is about.
    #[must_use]
    pub fn adding() -> Self {
        let [name_rect, address_rect] =
            super::render::field_row_rects(SEED_CANVAS.0, SEED_CANVAS.1);
        // The narration text was "Name"/"Address" — plausible-looking and
        // wrong. Vanilla's are `manageServer.enterName`/`manageServer.enterIp`
        // (`ManageServerScreen.java`), whose `en_us.json` values are
        // "Server Name"/"Server Address" — which happen to already be what
        // `render.rs`'s (unrelated) `detail` line under each field shows, so
        // this was invisible on screen and only wrong to a screen reader.
        let mut name =
            EditBox::new(name_rect.0, name_rect.1, name_rect.2, name_rect.3, "Server Name")
                .with_max_length(MAX_NAME_CHARS);
        // `nameEdit.setHint(DEFAULT_SERVER_NAME)` (`ManageServerScreen.java`),
        // `selectServer.defaultName` = "Minecraft Server" — shown only while
        // the field is empty and unfocused (`EditBox.hint`'s own doc), so this
        // was missing entirely rather than merely mislabelled.
        name.hint = Some("Minecraft Server".to_string());
        let mut fields = FormFields {
            name,
            address: EditBox::new(
                address_rect.0,
                address_rect.1,
                address_rect.2,
                address_rect.3,
                "Server Address",
            )
            .with_max_length(MAX_ADDRESS_CHARS),
        };
        let mut focus = FocusSet::new();
        // `addRenderableWidget`, not `addWidget` or `addRenderableOnly`: these
        // are drawn *and* interactive *and* narrated. Getting this wrong is the
        // island `super::focus`'s docs describe — a field that renders and never
        // takes a keystroke, with nothing failing loudly.
        focus.add_renderable_widget(NAME_FIELD);
        focus.add_renderable_widget(ADDRESS_FIELD);
        focus.set_initial_focus(&mut fields, NAME_FIELD);
        Self {
            fields,
            focus,
            editing: None,
            hovered: None,
            pack_status: super::servers::ServerPackPolicy::default(),
        }
    }

    /// A form pre-filled from `entry`, editing the row at `index`.
    #[must_use]
    pub fn editing(index: usize, entry: &ServerEntry) -> Self {
        let mut form = Self::adding();
        form.fields.name.set_value(&entry.name);
        form.fields.address.set_value(entry.address_label());
        form.editing = Some(index);
        form.pack_status = entry.pack_status;
        form
    }

    /// The name field's text.
    #[must_use]
    pub fn name(&self) -> &str {
        self.fields.name.value()
    }

    /// The address field's text.
    #[must_use]
    pub fn address(&self) -> &str {
        self.fields.address.value()
    }

    /// Which field has focus, for [`super::render`]'s `selected` row.
    ///
    /// Derived from [`super::focus::FocusSet`] rather than stored: two sources of
    /// truth for "which field is being typed into" is how a caret ends up drawn
    /// on a row that is not receiving the keys.
    #[must_use]
    pub fn field(&self) -> FormField {
        match self.focus.focused() {
            Some(ADDRESS_FIELD) => FormField::Address,
            _ => FormField::Name,
        }
    }

    /// The focused field's box, or the name field's when nothing is focused.
    #[must_use]
    pub fn focused_box(&self) -> &EditBox {
        match self.field() {
            FormField::Name => &self.fields.name,
            FormField::Address => &self.fields.address,
        }
    }

    /// Whether the form can be saved. The label may be blank (it falls back to
    /// the host); the address may not.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        !self.address().trim().is_empty()
    }

    /// The entry this form would save.
    #[must_use]
    pub fn to_entry(&self) -> ServerEntry {
        let (host, port) = ServerEntry::split_host_port(self.address());
        let name = if self.name().trim().is_empty() {
            host.clone()
        } else {
            self.name().to_owned()
        };
        let mut entry = ServerEntry::new(name, host, port);
        entry.pack_status = self.pack_status;
        entry
    }

    /// The resource-pack `CycleButton`'s current value, for the row's label.
    #[must_use]
    pub fn pack_status(&self) -> super::servers::ServerPackPolicy {
        self.pack_status
    }

    /// Advances [`Self::pack_status`] — the `RESOURCE_PACK_ROW` click.
    pub fn cycle_pack_status(&mut self) {
        self.pack_status = self.pack_status.cycle();
    }

    /// One key, routed through vanilla's `Screen.keyPressed` order: Escape, then
    /// the focused field, then — only if it declined — Tab and the arrows as
    /// focus navigation, and only then the screen's own meaning for the key.
    ///
    /// **That order is why Up/Down move between fields while Left/Right move the
    /// caret**, with no rule anywhere saying so: `EditBox.keyPressed` handles
    /// 262/263 and declines 264/265 (`EditBox.java`), so the vertical
    /// pair falls through to navigation and the horizontal pair never gets there.
    pub fn handle_key(&mut self, key: MenuKey) -> FormOutcome {
        // A printable character is `charTyped`, a *different* callback in vanilla
        // — see `super::focus::KeyEvent::from_menu_key`. Routing it through
        // `keyPressed` would make the letter `a` and Ctrl+A the same event.
        if let MenuKey::Char(ch) = key {
            self.focus.char_typed(&mut self.fields, ch);
            return FormOutcome::Handled;
        }
        let Some(event) = KeyEvent::from_menu_key(key) else {
            return FormOutcome::Handled;
        };
        match self.focus.screen_key_pressed(&mut self.fields, event) {
            KeyOutcome::Close => FormOutcome::Cancel,
            KeyOutcome::Consumed | KeyOutcome::FocusMoved => FormOutcome::Handled,
            KeyOutcome::Declined if key == MenuKey::Enter => FormOutcome::Save,
            KeyOutcome::Declined => FormOutcome::Handled,
        }
    }

    /// Focus the field drawn at row `row`, as a mouse click or hover does.
    /// Out-of-range rows are ignored rather than clamped.
    pub fn focus_row(&mut self, row: usize) {
        if row == NAME_FIELD || row == ADDRESS_FIELD {
            self.focus.set_focused(&mut self.fields, Some(row));
        }
    }

    /// The mouse moved over row `row` (issue: the screen's framework
    /// conversion added three button rows this form has no `FocusTarget` for).
    ///
    /// **A field row does nothing here** — a player report (2026-08-04)
    /// caught that pure mouse motion over the name/address field was granting
    /// it real keyboard focus, with no click involved. Vanilla's
    /// `ContainerEventHandler` only moves focus from `mouseClicked` or Tab
    /// traversal; hovering a field is not one of those, and `EditBox` does
    /// not even highlight on hover (see [`super::widget::Widget::slider`]'s
    /// sibling asymmetry note in `edit_box.rs`). A button row
    /// ([`RESOURCE_PACK_ROW`]/[`DONE_ROW`]/[`CANCEL_ROW`]) still records
    /// *only* [`Self::hovered`] — that part was always right, since it never
    /// touched keyboard focus — which is what lets the mouse travel to Done
    /// without pulling the caret out of the address field the player is still
    /// typing into. A click (`MenuNav::click`'s `ServerEdit` arm) still calls
    /// [`Self::focus_row`] directly, unaffected.
    pub fn hover_row(&mut self, row: usize) {
        match row {
            NAME_FIELD | ADDRESS_FIELD => {}
            RESOURCE_PACK_ROW | DONE_ROW | CANCEL_ROW => self.hovered = Some(row),
            _ => {}
        }
    }

    /// The button row the mouse is over, for [`super::render::MenuFrame::hovered`].
    #[must_use]
    pub fn hovered_button(&self) -> Option<usize> {
        self.hovered
    }

    /// A click at logical `(x, y)`, dispatched through
    /// `ContainerEventHandler.mouseClicked` — so it both focuses the field it
    /// landed in and puts the caret at the clicked character.
    ///
    /// Not reachable from `app.rs` today, which routes a click as a *row index*
    /// (`MenuNav::click`) and therefore carries no x. See `docs/menu-focus.md`.
    pub fn click_at(&mut self, x: f32, y: f32) -> bool {
        self.focus.mouse_clicked(&mut self.fields, x, y)
    }

    /// Types one character into the focused field, refusing whatever
    /// [`super::edit_box::is_allowed_chat_character`] refuses.
    ///
    /// Kept as a named method because it is the form's whole text-entry API to
    /// its tests; the cap and the filter now live in [`EditBox`] rather than
    /// here, so `§` is still refused for vanilla's own reason (it is the legacy
    /// formatting-code introducer) rather than for one written twice.
    pub fn push(&mut self, ch: char) {
        self.focus.char_typed(&mut self.fields, ch);
    }

    /// Deletes the character before the caret in the focused field.
    ///
    /// Now genuinely "before the caret" rather than "off the end", which is what
    /// the pre-`EditBox` form did — it had no caret to be before.
    pub fn backspace(&mut self) {
        self.focus
            .key_pressed(&mut self.fields, KeyEvent::new(focus::KEY_BACKSPACE));
    }

    /// Moves focus to the other field, through real Tab traversal.
    ///
    /// With two children this is also the wrap, and the wrap is vanilla's
    /// `clearFocus()`-then-retry rather than modular arithmetic — see
    /// [`super::focus`].
    pub fn next_field(&mut self) {
        self.focus
            .screen_key_pressed(&mut self.fields, KeyEvent::new(focus::KEY_TAB));
    }
}

/// The title screen's widgets, in vanilla's own display order.
///
/// This is vanilla `TitleScreen.init`'s widget list
/// (`.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/TitleScreen.java`),
/// reproduced whole rather than trimmed to what this client implements.
/// [`MainButton::enabled`] is what marks the rest **present but greyed out**,
/// which is the faithful thing: a button missing from its vanilla position is a
/// layout that reads wrong, while a disabled one in the right position reads
/// exactly like vanilla with the feature unavailable (which is a state vanilla
/// itself ships — `Multiplayer` and `Minecraft Realms` are disabled for a
/// banned account, `TitleScreen.java`).
///
/// The three 20×20 icon buttons come from `CommonButtons`
/// (`TitleScreen.java`); vanilla positions them with
/// `getHorizontalPosition(i, 3, 20)` (`TitleScreen.java`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MainButton {
    /// Open the singleplayer world list ([`Screen::WorldSelect`], issue #397) —
    /// vanilla's own behaviour for this button. It used to return
    /// [`MenuAction::Singleplayer`] and launch directly, which vanilla never
    /// does; that action is now produced one screen in, by **Play Selected
    /// World** (issue #287).
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
    /// Vanilla's language icon button — `TitleScreen.java` constructs
    /// `LanguageSelectScreen` directly with `lastScreen = this` (the title),
    /// never through `OptionsScreen`. **Now live** (issue #415 built
    /// [`super::options::SettingsPage::Language`]): this doc used to say the
    /// shell had no language-selection screen at all, which stopped being
    /// true once that issue landed and this button was never revisited.
    /// Opens the same page the root grid's "Language..." row does, but with
    /// an empty page stack (see
    /// [`super::options::SettingsNav::open_at`]) so Escape/Done returns
    /// straight to the title, matching vanilla's `lastScreen`.
    Language,
    /// Vanilla's accessibility icon button —
    /// `TitleScreen.java`, same direct-construction shape as
    /// [`MainButton::Language`]. **Now live** (the Accessibility Settings
    /// page has existed since issue #55): this doc used to say there was no
    /// accessibility options screen, which was already false by the time
    /// anyone next read this comment. Same `open_at`-with-empty-stack
    /// treatment as `Language`.
    Accessibility,
    /// Open the settings screen.
    Options,
    /// Quit the game.
    Quit,
    /// Open the account list (issue #66). **Not a vanilla widget** — unlike
    /// every other row in this enum, there is no `TitleScreen.java` line to
    /// cite for it. Real Minecraft has no in-game account switcher at all:
    /// an account is chosen once, outside the game, by the separate
    /// Minecraft Launcher, and the game client just uses whatever it was
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

/// Whether this build can end its own process, which is what **Quit Game**
/// means. False in a browser tab — see [`MainButton::enabled_on`].
pub const CAN_EXIT_PROCESS: bool = !cfg!(target_arch = "wasm32");

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
            MainButton::Singleplayer
            | MainButton::Multiplayer
            | MainButton::Options
            | MainButton::Accounts
            // Both destination screens are built now — see the variants' own
            // docs.
            | MainButton::Language
            | MainButton::Accessibility => true,
            MainButton::Realms | MainButton::Friends => false,
        }
    }

    /// The GUI sprite drawn centred in the button instead of a label —
    /// vanilla's `SpriteIconButton.CenteredIcon`, 15×15 inside a 20×20 button
    /// (`CommonButtons.java`, `FriendsButton.java`).
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
/// (`.cache/mc/26.2/client-src/net/minecraft/client/gui/screens/PauseScreen.java`)
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
/// with no players to report, `PauseScreen.java`).
///
/// Which Options layout is reproduced is a real fork in vanilla:
/// `minecraft.hasSingleplayerServer()` splits the row into Options + Open to LAN
/// (`PauseScreen.java`), and only the `else` branch gives Options the
/// full 204 px width (`PauseScreen.java`). This client has no integrated
/// server at all (see the module docs), so `hasSingleplayerServer()` is
/// unconditionally false for it and the full-width branch is the correct one.
///
/// Vanilla's last button is labelled by
/// `CommonComponents.disconnectButtonLabel(isLocalServer)` — "Save and Quit to
/// Title" locally, "Disconnect" remotely (`CommonComponents.java`). This
/// client uses "Disconnect" for both, because [`SessionKind::Singleplayer`] is
/// currently the local dev world with no persistence: "Save and Quit" would
/// promise a save that does not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PauseButton {
    /// Resume play. Equivalent to Escape. Vanilla's `menu.returnToGame`.
    BackToGame,
    /// Vanilla's `gui.advancements` — opens [`super::Screen::Advancements`]
    /// (issue #167). **Live, and showing real progress**: this used to be
    /// present-and-disabled because nothing decoded `UPDATE_ADVANCEMENTS`, and
    /// both halves of that wire have since landed. See [`super::advancements`].
    Advancements,
    /// Vanilla's `gui.stats` — opens [`super::Screen::Statistics`] (issue
    /// #188). **Now live.** What used to be this button's whole disabled
    /// reason ("nothing decodes the `award_stats` packet") is still true,
    /// and still matters: it is why every value the screen shows reads zero.
    /// It no longer has to keep the *button* off, because the screen behind
    /// it is real and a zero-everywhere state is what it honestly shows —
    /// see [`super::stats`]'s module docs for why that is not a
    /// fabrication. If a decoder for that packet lands, this is the doc to
    /// point at it, not a reason the button needed flipping again.
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
    /// [`super::Screen::Social`] (issue #189), vanilla's
    /// `SocialInteractionsScreen`. **Now live**, not present-and-disabled:
    /// the screen itself (an online-player list with a Hide/Show-in-Chat
    /// toggle) needs nothing this button's own disabled reason used to name.
    /// What is *still* gated is one control **inside** that screen — every
    /// row's Report button, because that needs the chat-signature/secure
    /// chat-signing context this client does not have (see
    /// [`super::social`]'s module docs). If secure chat signing lands, that
    /// is the doc to update, not this one — this comment used to be the only
    /// place the dependency was written down, and issue #189's own tracking
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
    /// vanilla's own `!serverLinks.isEmpty()` gate, reproduced as an
    /// *omission* rather than a disabled row, matching
    /// [`Self::OpenToLan`]'s own precedent for a row with nothing to offer.
    ServerLinks,
    /// Open the settings screen (reuses [`super::Screen::Settings`] — see
    /// [`super::UiState::open_settings_from_pause`]).
    Options,
    /// Vanilla's `menu.multiplayerOptions.button`, whose `en_us` value really is
    /// **"Open to LAN"** — the half-width sibling of [`Self::Options`] that
    /// `PauseScreen.createPauseMenu`'s `hasSingleplayerServer()` branch adds
    /// (`PauseScreen.java`). Issue #535's scope 1.
    ///
    /// **Conditionally present, since issue #535's scope 2 — but not for the
    /// reason a first read of vanilla suggests.** `PauseScreen` in the 26.2
    /// decompile (`PauseScreen.java`) shows this row whenever
    /// `hasSingleplayerServer()` is true **regardless of publish state**: it
    /// is vanilla's `MultiplayerOptionsScreen` behind the button that changes,
    /// an on/off `CycleButton` seeded from `IntegratedServer.isPublished()`
    /// (`MultiplayerOptionsScreen.java`) — vanilla never re-presses a "publish"
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
    /// Vanilla opens a `MultiplayerOptionsScreen` — a form with a LAN/online
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
/// the world is published (issue #535's scope 2). See that variant's own doc
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
                // Issue #189: the screen behind this button is built. See the
                // variant's own doc for what is and is not wired inside it.
                | PauseButton::PlayerReporting
                // Issue #188: likewise.
                | PauseButton::Statistics
                // Issue #167: the Advancements screen exists, and since the
                // `UPDATE_ADVANCEMENTS` decode landed it shows real progress —
                // see `menu::advancements`' module docs.
                | PauseButton::Advancements
                // Issue #535: `IntegratedServer::open_to_lan` has a caller. Always
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
    /// inside a 20×20 button (`PauseScreen.java`).
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

/// The multiplayer screen's title — `multiplayer.title`'s `en_us` string
/// (`JoinMultiplayerScreen.java`), which
/// `HeaderAndFooterLayout.addTitleHeader` centres in the header band.
pub const SERVER_LIST_TITLE: &str = "Play Multiplayer";

/// `JoinMultiplayerScreen`'s seven footer buttons (#396), in the order they are
/// added to the two footer rows (`JoinMultiplayerScreen.java`) — which is
/// also the order [`super::render::server_list_footer_slot`] reads out of the
/// arranged layout, and the order the rows appear in after the server entries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServerListButton {
    /// `selectServer.select` — join the selected server. Inactive with nothing
    /// selected.
    Select,
    /// `selectServer.direct` — connect to an address without saving it.
    ///
    /// **Present and inactive.** It opens `DirectJoinServerScreen`, a second
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
    /// Vanilla's `en_us.json` strings verbatim.
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

    /// Vanilla's declared width: 100 for the top row, 74 for the lower one
    /// (`JoinMultiplayerScreen.java`).
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

    /// `JoinMultiplayerScreen.onSelectedChange` (`:246-257`): Join, Edit and
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

/// The death screen's two widgets (issue #103), vanilla's
/// `DeathScreen.init` (`DeathScreen.java`). Both live; unlike
/// [`MainButton`]/[`PauseButton`] there is nothing present-and-disabled here
/// — vanilla itself only ever shows these two.
///
/// No hardcore variant: this client has no hardcore mode (nothing decodes a
/// client-visible hardcore flag), so vanilla's fork —
/// `deathScreen.spectate`/no-confirm-dialog when hardcore, `deathScreen.respawn`
/// otherwise — always takes the non-hardcore branch. See [`super::render::death_frame`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeathButton {
    /// `deathScreen.respawn` ("Respawn"): submit a manual `ClientAction::Respawn`.
    Respawn,
    /// `deathScreen.titleScreen` ("Title Screen"): leave for the main menu.
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
    /// Vanilla's `en_us.json` strings verbatim.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            DeathButton::Respawn => "Respawn",
            DeathButton::TitleScreen => "Title Screen",
        }
    }
}

/// Selection state and the saved server list.
///
/// No longer `Clone`: [`accounts`](MenuNav::accounts) holds a live channel
/// receiver for an in-flight sign-in ([`accounts::AccountsNav`]), and
/// `mpsc::Receiver` is not `Clone`. Nothing in the tree ever cloned a
/// `MenuNav` (it is held once, behind `&mut self`, in `app.rs`'s window
/// struct), so dropping the derive costs nothing.
#[derive(Debug)]
pub struct MenuNav {
    main: usize,
    server: usize,
    /// Highlighted row on the pause menu ([`PAUSE_BUTTONS`]).
    paused: usize,
    /// Highlighted row on the death screen ([`DEATH_BUTTONS`], issue #103).
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
    /// The account list + sign-in flow (issue #66). See
    /// [`crate::menu::accounts`].
    accounts: crate::menu::accounts::AccountsNav,
    /// The world-select screen's widgets and focus (issue #397). Held here for
    /// [`EditForm`]'s reason: it owns real [`EditBox`] state (a caret, a
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
    /// Which [`SERVER_LIST_BUTTONS`] entry the cursor is over, if any (#396).
    ///
    /// Separate from [`Self::server`] because the two are different cursors that
    /// are visible at once: the selected *server* keeps its outline while a footer
    /// button under the mouse draws highlighted.
    list_button: Option<usize>,
    /// How far the multiplayer list is scrolled down, **in logical pixels** —
    /// vanilla's `AbstractScrollArea.scrollAmount`, which is a `double` and is
    /// subtracted straight from a row's y (`AbstractSelectionList.java`).
    ///
    /// **This was a `usize` row counter until issue #445**, and that was the
    /// whole of the owner's bug report: one wheel notch is
    /// `scrollY * scrollRate()` where `scrollRate = defaultEntryHeight / 2`
    /// (`AbstractScrollArea.java`, `:141-142`, `AbstractSelectionList.java`
    /// via `defaultSettings`), i.e. **18 px** for a 36 px row — a value a row
    /// index structurally cannot hold, so the list jumped a whole entry per
    /// notch. See [`Self::scroll_server_list`], which now delegates to
    /// [`super::widget::ScrollList`] rather than reimplementing the clamp.
    ///
    /// Row-quantization was not arbitrary when it landed: this pipeline had no
    /// scissor, so a straddling row would have painted over the footer. It has
    /// one now — `render.rs` wraps `draw_server_entry` in `Quads::with_clip`
    /// against the same band this offset is clamped against — which is the
    /// precondition that makes a pixel offset safe to draw.
    ///
    /// Not persisted and reset to `0.0` whenever the screen is (re)opened from
    /// the title, matching vanilla building a fresh `JoinMultiplayerScreen` —
    /// see [`Self::key_main`]'s `MainButton::Multiplayer` arm.
    server_scroll: f32,
    /// The last known mouse position in **logical** pixels, and the canvas it was
    /// measured in (#396).
    ///
    /// `app.rs` already resolves the cursor to a logical position inside
    /// `menu_row_at`; this is where it records it, so the *menu* can answer
    /// position questions a row index cannot. There is exactly one such question
    /// so far and it is vanilla's: which quadrant of a server row's 32 px favicon
    /// the cursor is in decides whether a click joins, moves the row up, or moves
    /// it down (`ServerSelectionList.java`).
    ///
    /// `None` until the first `CursorMoved`, which is the state a keyboard-only
    /// session is in — and the quadrant actions must then simply not fire, rather
    /// than behaving as if the cursor were at `(0, 0)`.
    menu_cursor: Option<(f32, f32, f32, f32)>,
    /// The settings tree's own cursor — which of the nine pages is showing,
    /// where the cursor is on it, and how far its `OptionsList` is scrolled
    /// (issue #55). See [`super::options::SettingsNav`].
    ///
    /// Held here rather than in [`UiState`] because it is *navigation state*,
    /// like [`Self::main`] and [`Self::paused`]: `Screen::Settings` is one screen
    /// however deep the page stack is, and `UiState` models legal screen edges
    /// only.
    settings: crate::menu::options::SettingsNav,
    /// The Social Interactions screen's own cursor, roster snapshot and
    /// hidden-player choices (issue #189). Held here for the same reason
    /// [`Self::settings`] is: `Screen::Social` is one screen regardless of how
    /// far its list is scrolled, and `UiState` models legal screen edges only.
    social: crate::menu::social::SocialNav,
    /// The Statistics screen's own scroll cursor (issue #188). No persisted
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
    /// The Advancements screen's selected tab and per-tab scroll (issue #167).
    /// Held here for [`Self::stats`]' reason: `Screen::Advancements` is one screen
    /// however far its tree is panned, and `UiState` models legal screen edges
    /// only. Reset on every entry from the pause menu, matching vanilla's
    /// per-screen `AdvancementTab` lifetime.
    advancements: crate::menu::advancements::AdvancementsState,
    /// The World Creation screen's own widgets, focus and collected config
    /// (issue #190). Held here for the same reason [`Self::form`] is: it owns
    /// real [`EditBox`] state that cannot be rebuilt per frame.
    create_world: crate::menu::create_world::CreateWorldNav,
    /// The live confirmation screen's own widgets, focus and request (issue
    /// #540). Held here for [`Self::create_world`]'s reason — the widgets carry
    /// focus state that cannot be rebuilt per frame — and **replaced** rather
    /// than mutated every time a confirmation is opened, so a stale focus or a
    /// stale target can never survive into the next one. A confirmation that
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
    /// A double-click on a server row joins it — vanilla's
    /// `ServerSelectionList.java`, `if (doubleClick) join()`,
    /// unconditional on where in the row the click landed. The primitive is
    /// [`super::focus::DoubleClickTracker`]; this is `click_list`'s only
    /// caller of it.
    double_click: super::focus::DoubleClickTracker<usize>,
    /// The monotonic clock [`Self::double_click`] measures against. An
    /// `Instant` fixed at construction rather than reset per click — only
    /// the *differences* `DoubleClickTracker` computes matter, so nothing
    /// needs rearming.
    click_clock: crate::platform::Instant,
    /// The command block edit screen's widgets and toggles (issue #47), held
    /// for the same reason [`Self::form`] is: it owns a real [`EditBox`] that
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
    /// The command tree the connected server sent (issue #471 step 2), pushed
    /// down by `app`'s right-click handler off `net::CommandTreeCell` — this
    /// module is pure and holds no client handle, so it cannot pull it.
    ///
    /// `None` off a live session, or before the server's `minecraft:commands`
    /// arrives, and every consumer treats that as "offer no completions"
    /// rather than as an empty tree. An `Arc` because a real 26.2 server's tree
    /// is ~2,000 nodes: this is a shared read, never a copy.
    command_tree: Option<std::sync::Arc<lodestone_model::command_tree::CommandTree>>,
    /// Whether the hosted world is currently published to LAN (issue #535's
    /// scope 2) — pushed in every frame from `Sim::is_lan_published` by
    /// `app::session::drive_ui_from_session`, the same shape
    /// [`Self::command_tree`] is pushed in from a live session. This module
    /// is pure and holds no `Sim`, so it cannot poll the real state itself.
    /// `false` off a hosted session too, which is what keeps a multiplayer
    /// join's pause menu identical to a fresh singleplayer one's. See
    /// [`Self::pause_buttons`], the one reader.
    lan_published: bool,
}

impl Default for MenuNav {
    fn default() -> Self {
        Self::new()
    }
}

impl MenuNav {
    /// Loads the saved server list, options and account metadata from their
    /// real locations.
    #[must_use]
    pub fn new() -> Self {
        Self::with_paths(
            servers_path(),
            crate::config::options_path(),
            lodestone_auth::paths::profiles_path(),
        )
    }

    /// Loads the server list from `path`. Missing or corrupt is an empty list.
    /// The options and account-metadata files are derived from the same
    /// directory (`options.json`/`profiles.json` beside it) so existing
    /// callers of this constructor keep working unchanged — see
    /// [`MenuNav::with_paths`] to point all three explicitly.
    #[must_use]
    pub fn with_path(path: std::path::PathBuf) -> Self {
        let options_path = path
            .parent()
            .map(|d| d.join("options.json"))
            .unwrap_or_else(|| std::path::PathBuf::from("options.json"));
        let profiles_path = path
            .parent()
            .map(|d| d.join("profiles.json"))
            .unwrap_or_else(|| std::path::PathBuf::from("profiles.json"));
        Self::with_paths(path, options_path, profiles_path)
    }

    /// Loads the server list from `path`, the options from `options_path` and
    /// account metadata from `profiles_path`. Missing or corrupt is an empty
    /// list / the default options / no known accounts respectively, never an
    /// error — a corrupt file must not stop the game from launching.
    #[must_use]
    pub fn with_paths(
        path: std::path::PathBuf,
        options_path: std::path::PathBuf,
        profiles_path: std::path::PathBuf,
    ) -> Self {
        // Derived from `path`'s directory the same way `Self::with_path`
        // already derives `options_path`/`profiles_path` when only the list
        // path is given — not a fourth constructor parameter, so every
        // existing three-argument caller (there are many, across this file's
        // own tests) keeps working unchanged.
        let hidden_players_path = path
            .parent()
            .map(|d| d.join("hidden_players.json"))
            .unwrap_or_else(|| std::path::PathBuf::from("hidden_players.json"));
        // Same derivation, and load-bearing for a different reason: this one is a
        // *directory tree* the game writes worlds into, so a test that inherited
        // the real one would be creating and listing worlds in the developer's own
        // saves folder. See [`Self::saves_root`].
        let saves_root = path
            .parent()
            .map(|d| d.join(crate::saves::SAVES_DIR))
            .unwrap_or_else(|| std::path::PathBuf::from(crate::saves::SAVES_DIR));
        Self {
            main: 0,
            server: 0,
            paused: 0,
            death: 0,
            form: EditForm::adding(),
            list: ServerList::load_from(&path),
            path,
            save_error: None,
            options: Options::load_from(&options_path),
            options_path,
            options_save_error: None,
            accounts: crate::menu::accounts::AccountsNav::with_path(profiles_path),
            // Empty on construction, deliberately: `MenuNav::new()` runs at
            // startup and in hundreds of tests, and enumerating the filesystem
            // from a constructor is the OS-side-effect-in-a-test shape §12.44
            // records. The list is read when the screen is *opened* — see
            // `open_world_list`, which is also what makes a just-created world
            // appear.
            world_select: crate::menu::world_select::WorldSelectNav::new(),
            saves_root,
            list_button: None,
            server_scroll: 0.0,
            menu_cursor: None,
            settings: crate::menu::options::SettingsNav::new(),
            social: crate::menu::social::SocialNav::with_path(hidden_players_path),
            stats: crate::menu::stats::StatsNav::default(),
            stats_snapshot: crate::menu::stats::StatsSnapshot::default(),
            server_links: crate::menu::server_links::ServerLinksNav::default(),
            advancements: crate::menu::advancements::AdvancementsState::default(),
            create_world: crate::menu::create_world::CreateWorldNav::new(),
            // A placeholder: nothing reads it until `Screen::Confirm` is
            // reached, and `apply_world_select`'s Delete arm builds the real one
            // in the same statement that opens the screen.
            confirm: crate::menu::confirm::ConfirmNav::default(),
            double_click: super::focus::DoubleClickTracker::new(),
            click_clock: crate::platform::Instant::now(),
            command_block: None,
            sign_edit: None,
            book_edit: None,
            resource_pack_prompt: None,
            resource_pack_answered_id: None,
            command_tree: None,
            lan_published: false,
        }
    }

    /// The saved servers.
    #[must_use]
    pub fn list(&self) -> &ServerList {
        &self.list
    }

    /// Where singleplayer worlds live for *this* `MenuNav`. See
    /// [`Self::saves_root`].
    #[must_use]
    pub fn saves_root(&self) -> &std::path::Path {
        &self.saves_root
    }

    /// Open [`Screen::WorldSelect`], re-reading `saves/` first.
    ///
    /// **The re-read is the point**, and it is vanilla's own behaviour rather than
    /// a cache invalidation bolted on: `TitleScreen` constructs a brand-new
    /// `SelectWorldScreen` on every press, whose `WorldSelectionList` calls
    /// `loadLevels()` in its constructor. Without it, a world created a moment ago
    /// would be absent from the list the player is returned to — which is exactly
    /// the "Create New World did nothing" report all over again, one layer up.
    ///
    /// Every entry point to the screen goes through here for that reason: the
    /// title-screen button, and the return from `CreateWorld`'s Cancel.
    pub fn open_world_list(&mut self, ui: &mut UiState) {
        self.world_select = crate::menu::world_select::WorldSelectNav::with_worlds(
            crate::saves::list_worlds_in(&self.saves_root),
        );
        ui.open_world_select();
    }

    /// The persisted `gui_scale` option ([`crate::config::AUTO_GUI_SCALE`] or
    /// an explicit ceiling) — never a pixel count, see
    /// [`crate::config::calculate_gui_scale`].
    #[must_use]
    pub fn gui_scale(&self) -> u32 {
        self.options.gui_scale
    }

    /// Vanilla's **Panorama Scroll Speed** option — see
    /// [`crate::config::Options::panorama_speed`]. Read by
    /// `render::frame_for`, which stamps it onto every frame beside
    /// [`Self::gui_scale`]; `MenuRenderer`'s panorama block is the consumer.
    #[must_use]
    pub fn panorama_speed(&self) -> f32 {
        self.options.panorama_speed
    }

    /// Vanilla's **View Bobbing** option — see
    /// [`crate::config::Options::view_bobbing`]. Read once per presented frame
    /// by `app.rs` and handed to `Sim::set_view_bobbing`.
    #[must_use]
    pub fn view_bobbing(&self) -> bool {
        self.options.view_bobbing
    }

    /// Vanilla's **Damage Tilt** accessibility option — see
    /// [`crate::config::Options::damage_tilt_strength`]. Read once per presented
    /// frame by `app.rs` and handed to `Sim::set_damage_tilt_strength`, exactly
    /// like [`MenuNav::view_bobbing`], because the two are the halves of one
    /// vanilla split: View Bobbing gates the walk bob and this scales the damage
    /// tilt, and `GameRenderer.renderLevel` applies the second whether or not the
    /// first is on.
    #[must_use]
    pub fn damage_tilt_strength(&self) -> f32 {
        self.options.damage_tilt_strength
    }

    /// Vanilla's `key.sneak` hold/toggle option (issue #202) — see
    /// [`crate::config::Options::toggle_sneak`]. Read every tick and handed to
    /// `InputState::set_toggle_modes`.
    #[must_use]
    pub fn toggle_sneak(&self) -> bool {
        self.options.toggle_sneak
    }

    /// As [`MenuNav::toggle_sneak`], for `key.sprint`.
    #[must_use]
    pub fn toggle_sprint(&self) -> bool {
        self.options.toggle_sprint
    }

    /// As [`MenuNav::toggle_sneak`], for `key.attack` (issue #444).
    #[must_use]
    pub fn toggle_attack(&self) -> bool {
        self.options.toggle_attack
    }

    /// As [`MenuNav::toggle_sneak`], for `key.use` (issue #444).
    #[must_use]
    pub fn toggle_use(&self) -> bool {
        self.options.toggle_use
    }

    /// Vanilla's `options.autoJump` (issue #444) — see
    /// [`crate::config::Options::auto_jump`]. Pushed into `Sim` once per frame
    /// by `app/redraw.rs`, the same way [`Self::view_bobbing`] is, so a change
    /// in Controls applies on the very next tick's auto-jump gate rather than
    /// at the next launch.
    #[must_use]
    pub fn auto_jump(&self) -> bool {
        self.options.auto_jump
    }

    /// Vanilla's `options.sprintWindow` (issue #444) — the double-tap-forward
    /// window in 20 Hz ticks. See [`crate::config::Options::sprint_window_ticks`].
    /// Pushed into `Sim` once per frame by `app/redraw.rs` and forwarded to
    /// the live `InputState`, so a change in Controls applies on the very next
    /// tick.
    #[must_use]
    pub fn sprint_window_ticks(&self) -> u8 {
        self.options.sprint_window_ticks
    }

    /// Vanilla's `options.sensitivity` (issue #443) — see
    /// [`crate::config::Options::sensitivity`]. Pushed into `Sim` once per
    /// frame by `app/redraw.rs`, the same way [`Self::invert_mouse_x`] is,
    /// so a change in Options → Mouse applies on the very next tick rather
    /// than at the next launch. Without this accessor `apply_mouse` had no
    /// route to the *persisted* option at all and read the argv-derived
    /// `Config::sensitivity`, which is fixed for the process's lifetime.
    #[must_use]
    pub fn sensitivity(&self) -> f32 {
        self.options.sensitivity
    }

    /// Vanilla's `options.renderDistance` in chunks — see
    /// [`crate::config::Options::render_distance`].
    ///
    /// Polled once per frame by `app/redraw.rs` and committed on vanilla's
    /// 600 ms delay (`WindowApp::render_distance_apply_at`), **not** pushed
    /// straight through like [`Self::sensitivity`]: this is the one option whose
    /// `IntRange` vanilla builds with `applyValueImmediately == false`, because
    /// applying it reloads chunks.
    #[must_use]
    pub fn render_distance(&self) -> u32 {
        self.options.render_distance
    }

    /// [`crate::config::Options::advanced_item_tooltips`] — read by the container
    /// tooltip builder, written only by [`Self::toggle_advanced_item_tooltips`].
    #[must_use]
    pub fn advanced_item_tooltips(&self) -> bool {
        self.options.advanced_item_tooltips
    }

    /// F3+H. Persists eagerly, like every other option mutation here.
    ///
    /// On `MenuNav` rather than on `WindowApp` because this is the type that owns
    /// `Options` and knows how to write `options.json`; the driver's F3 chord arm
    /// is the caller. See the option's own doc for why it is persisted at all
    /// when its two sibling chords are not.
    pub fn toggle_advanced_item_tooltips(&mut self) {
        self.options.advanced_item_tooltips = !self.options.advanced_item_tooltips;
        self.persist_options();
    }

    /// [`crate::config::Options::pause_on_lost_focus`] — read by
    /// `WindowEvent::Focused(false)` in `app/lifecycle.rs`, written only by
    /// [`Self::toggle_pause_on_lost_focus`].
    #[must_use]
    pub fn pause_on_lost_focus(&self) -> bool {
        self.options.pause_on_lost_focus
    }

    /// F3+P. Persists eagerly, the same shape as
    /// [`Self::toggle_advanced_item_tooltips`] and for the same vanilla reason
    /// (`options.pauseOnLostFocus = !options.pauseOnLostFocus; options.save();`
    /// in `KeyboardHandler.java`).
    pub fn toggle_pause_on_lost_focus(&mut self) {
        self.options.pause_on_lost_focus = !self.options.pause_on_lost_focus;
        self.persist_options();
    }

    /// Vanilla's `options.invertMouseX` (issue #203) — see
    /// [`crate::config::Options::invert_mouse_x`]. Read per look-integration
    /// call and handed to `apply_look_inverted`.
    #[must_use]
    pub fn invert_mouse_x(&self) -> bool {
        self.options.invert_mouse_x
    }

    /// [`crate::config::Options::discrete_mouse_scroll`] (issue #444), read by
    /// `app/lifecycle.rs`'s two wheel arms — see that field's doc for why both.
    #[must_use]
    pub fn discrete_mouse_scroll(&self) -> bool {
        self.options.discrete_mouse_scroll
    }

    /// As [`MenuNav::invert_mouse_x`], for Y.
    #[must_use]
    pub fn invert_mouse_y(&self) -> bool {
        self.options.invert_mouse_y
    }

    /// Vanilla's `options.mouseWheelSensitivity` (issue #203) — see
    /// [`crate::config::Options::mouse_wheel_sensitivity`]. Read by the
    /// hotbar scroll handler.
    #[must_use]
    pub fn mouse_wheel_sensitivity(&self) -> f32 {
        self.options.mouse_wheel_sensitivity
    }

    /// The last options-save failure, if any.
    #[must_use]
    pub fn options_save_error(&self) -> Option<&str> {
        self.options_save_error.as_deref()
    }

    /// The persisted options, whole.
    ///
    /// The settings tree needs *all* of them to label its live rows, and adding
    /// a third `fn <name>()` per option as [`Self::gui_scale`] and
    /// [`Self::view_bobbing`] did would grow one accessor per option forever.
    /// Those two stay because `app.rs` reads them by name on the hot path.
    #[must_use]
    pub fn options(&self) -> &Options {
        &self.options
    }

    /// The settings tree's cursor (issue #55) — which page, which control, how
    /// far scrolled. See [`super::options::SettingsNav`].
    #[must_use]
    pub fn settings(&self) -> &crate::menu::options::SettingsNav {
        &self.settings
    }

    /// Rescans the packs folder and refreshes the Resource Packs screen's
    /// Available column, **if** that page is the one currently active —
    /// a no-op otherwise, so a caller (`WindowApp::window_event`'s
    /// `WindowEvent::Focused(true)` arm) can call this unconditionally on
    /// every window-focus regain rather than threading a "is this screen
    /// open" check through from `UiState`.
    ///
    /// See [`super::packs::PacksNav::refresh_available`] for why this is not
    /// [`super::packs::PacksNav::reset`]: this must never revert the user's
    /// own in-progress (uncommitted) reordering back to the persisted
    /// selection just because the window regained focus.
    pub fn refresh_open_resource_packs_screen(&mut self) {
        if self.settings.page() == crate::menu::options::SettingsPage::ResourcePacks {
            self.settings.packs_mut().refresh_available();
        }
    }

    /// The Social Interactions screen's own state (issue #189).
    #[must_use]
    pub fn social(&self) -> &crate::menu::social::SocialNav {
        &self.social
    }

    /// Replaces the online-player snapshot the Social Interactions screen
    /// shows — see [`crate::menu::social::entries_from_tablist`]'s doc for
    /// who is meant to call this. Exposed here rather than requiring a caller
    /// to reach through `social_mut()` (there is none) because this is the
    /// one piece of live-session data this screen needs from outside, the
    /// same shape [`Self::settings`]'s sibling accessors take for granted
    /// everything else is either persisted or pure.
    pub fn refresh_social(&mut self, entries: Vec<crate::menu::social::SocialEntry>) {
        self.social.refresh(entries);
    }

    /// Replaces the counters the Statistics screen shows — the same shape, and
    /// for the same reason, as [`Self::refresh_social`] above: `menu::render`'s
    /// dispatcher cannot reach the session world, so the live data is pushed in
    /// from `app::session`'s per-frame reconciliation.
    ///
    /// Before this the screen drew `StatsSnapshot::default()` unconditionally and
    /// so showed zeros forever, which was correct while nothing decoded
    /// `award_stats` and became an island the moment something did.
    pub fn refresh_stats(&mut self, snapshot: crate::menu::stats::StatsSnapshot) {
        self.stats_snapshot = snapshot;
    }

    /// The counters the Statistics screen should draw. See
    /// [`Self::refresh_stats`].
    #[must_use]
    pub fn stats_snapshot(&self) -> &crate::menu::stats::StatsSnapshot {
        &self.stats_snapshot
    }

    /// Replaces the live link list the Server Links screen — and the pause
    /// menu's own row gate, [`Self::pause_buttons`] — read. The same shape
    /// and reason as [`Self::refresh_stats`]: `menu::render`'s dispatcher
    /// cannot reach the session world, so `app::session`'s per-frame
    /// reconciliation pushes this in.
    pub fn refresh_server_links(&mut self, links: Vec<lodestone_model::event::ServerLink>) {
        self.server_links.set_links(links);
    }

    /// The Server Links screen's own state (list/confirm view, hover, the
    /// live link list). Public for the same reason [`Self::stats`] is: the
    /// draw and the hit-test both need it, and duplicating a second read
    /// would be a second source of truth.
    #[must_use]
    pub fn server_links(&self) -> &crate::menu::server_links::ServerLinksNav {
        &self.server_links
    }

    /// The Statistics screen's own state (issue #188).
    #[must_use]
    /// The Advancements screen's own tab/scroll state (issue #167), for the draw
    /// and hit-test paths in `app`.
    #[must_use]
    pub fn advancements(&self) -> &crate::menu::advancements::AdvancementsState {
        &self.advancements
    }

    /// [`advancements`](Self::advancements), mutably — the layout centres a tab on
    /// first read, so even the *draw* needs `&mut` here (vanilla's own `centered`
    /// latch does the same).
    pub fn advancements_mut(&mut self) -> &mut crate::menu::advancements::AdvancementsState {
        &mut self.advancements
    }

    pub fn stats(&self) -> &crate::menu::stats::StatsNav {
        &self.stats
    }

    /// The World Creation screen's own state (issue #190).
    #[must_use]
    pub fn create_world(&self) -> &crate::menu::create_world::CreateWorldNav {
        &self.create_world
    }

    /// Whether a Key Binds bind button is mid-capture (issue #15) — a click
    /// or Enter on it already latched
    /// [`super::key_binds::KeyBindsNav::awaiting`], entirely within this
    /// crate. `app.rs` reads this **before** translating a raw `KeyEvent`/
    /// `MouseButton` into a [`MenuKey`] and, while it is `true`, must route
    /// the *next* one to [`Self::capture_binding`] instead — the one hop this
    /// crate cannot take on its own, because rebinding to a key with no
    /// printable `text` (an F-key, a modifier, an arrow other than Up/Down)
    /// needs the physical [`winit::keyboard::KeyCode`] `menu_key_for` throws
    /// away today. See `docs/keybindings.md`'s "Wiring the Controls menu"
    /// section for the exact patch.
    #[must_use]
    pub fn awaiting_key_capture(&self) -> bool {
        self.settings.key_binds().awaiting().is_some()
    }

    /// Finishes a pending Key Binds capture: sets the action's binding and
    /// persists immediately, the same eager-persistence rule every other live
    /// row in this tree follows. A no-op if nothing is awaiting (harmless —
    /// `app.rs` is expected to guard this on [`Self::awaiting_key_capture`],
    /// but a stray call costing nothing is cheaper than a debug assertion
    /// that could panic in the field).
    ///
    /// **The `Pause` hazard, enforced here rather than left as a comment.**
    /// `crate::keybinds::InputAction::Pause`'s own doc names it: unbinding the
    /// only gameplay route to the pause screen (and so to Quit to Title)
    /// strands a session with no way out but the window's close button.
    /// Vanilla's own `KeyBindsScreen.keyPressed` sets `InputConstants.UNKNOWN`
    /// unconditionally on Escape while capturing (`:73-74`) — `Pause` is not a
    /// real vanilla `KeyMapping`, so vanilla never has this hazard to guard.
    /// Escape while capturing `Pause` here instead cancels the capture with
    /// its *old* binding intact ([`super::key_binds::KeyBindsNav::escape`]
    /// already does that for every action); this method additionally refuses
    /// to *set* `Pause` to [`crate::keybinds::Binding::Unbound`] even if a
    /// future caller reaches one some other way (a mouse click has no
    /// "Escape" of its own to fall back on).
    pub fn capture_binding(&mut self, binding: crate::keybinds::Binding) {
        use crate::keybinds::{Binding, InputAction};
        let Some(action) = self.settings.key_binds_mut().take_awaiting() else {
            return;
        };
        if action == InputAction::Pause && binding == Binding::Unbound {
            return;
        }
        self.options.keybinds.set(action, binding);
        self.persist_options();
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

    /// Which [`SERVER_LIST_BUTTONS`] entry the cursor is over, if any (#396).
    #[must_use]
    pub fn list_button(&self) -> Option<usize> {
        self.list_button
    }

    /// How far the multiplayer list is scrolled down, **in logical pixels**
    /// (issues #402, #445). See [`Self::server_scroll`]'s field doc.
    #[must_use]
    pub fn server_scroll(&self) -> f32 {
        self.server_scroll
    }

    /// The scrolling list on the screen `ui` is showing, or `None` when that screen
    /// has none.
    ///
    /// ## Why this is one function and not a field per screen
    ///
    /// This is the **generic hook** the scrollbar draw and the mouse wheel both ask.
    /// Before it existed, `render::draw` called `server_scroll_list` by name and
    /// `app`'s wheel arm was gated on `Screen::ServerList`, so exactly one screen
    /// could have a bar or respond to the wheel — and a second screen adopting
    /// `ScrollList` would have had correct geometry, green tests and zero pixels.
    /// Both consumers now go through here, so *declaring* a list is all a screen has
    /// to do.
    ///
    /// Each arm delegates to the screen's own `*_list_spec`, which derives the band
    /// and the pitch from the same constants that screen's draw uses. This function
    /// therefore holds no geometry of its own — it is a router, and the thing it is
    /// routing is the answer to "which screen is up".
    ///
    /// ## How to add a screen
    ///
    /// Add an arm, and make sure the screen's offset is stored in **pixels**. A
    /// screen whose offset is a row index cannot be added honestly: it would report a
    /// `scroll` that is always a multiple of the row height, which is exactly the
    /// snap-to-row stepping the wheel work removed. `menu/stats.rs`,
    /// `menu/social.rs`, `menu/language.rs`, `menu/key_binds.rs` and
    /// `menu/options.rs` all still hold a `first: usize` entry index and are
    /// therefore **not** here yet; converting the field is the prerequisite, not an
    /// afterthought.
    #[must_use]
    pub fn active_list(&self, ui: &super::UiState) -> Option<super::widget::ListSpec> {
        match ui.screen() {
            super::Screen::ServerList => Some(super::render::server_list_spec(
                self.list.len(),
                self.server_scroll,
            )),
            // Only the idle frame has a list at all: the sign-in and failure frames
            // draw one wide button over a text notice, and reporting a list there
            // would hang a scrollbar beside a screen with no rows.
            super::Screen::Accounts => {
                let accounts = self.accounts();
                if matches!(
                    accounts.sign_in_view(),
                    super::accounts::SignInView::Idle
                ) {
                    Some(super::render::accounts_list_spec(
                        accounts.rows().len(),
                        accounts.scroll(),
                    ))
                } else {
                    None
                }
            }
            // The singleplayer save list (issue #541). Its length is the
            // **post-filter** row count, so typing in the search box shortens the
            // bar instead of leaving a thumb sized for the whole of `saves/` —
            // `WorldSelectNav::shown_len` is the one expression that decides, and
            // the row draw reads the same one.
            super::Screen::WorldSelect => {
                let ws = self.world_select();
                Some(super::render::world_list_spec(ws.shown_len(), ws.scroll()))
            }
            // Issue #445's first adoption. Its offset is pixels, which is the
            // prerequisite this arm exists to assert — see `ListSpec`'s doc.
            super::Screen::Statistics => Some(super::stats::list_spec(
                super::stats::GENERAL_STATS.len(),
                self.stats.scroll(),
            )),
            // Issue #445's second adoption. **Keyed on the settings *page*, not
            // just the screen**: `Screen::Settings` is one screen with a dozen
            // pages and only some of them are lists, so an arm on the bare screen
            // would hang a scrollbar beside the root page's two-column grid. The
            // page's own `KeyBindsNav` owns the offset, in pixels.
            super::Screen::Settings
                if self.settings.page() == crate::menu::options::SettingsPage::KeyBinds =>
            {
                Some(super::key_binds::list_spec(
                    self.settings.key_binds().scroll(),
                ))
            }
            // Language (#445's third adoption). Its length is the **post-filter**
            // entry count, so typing in the search box shortens the bar instead of
            // leaving a thumb sized for the full list — `LanguageNav::model` is
            // the one expression that decides, and this arm calls the same
            // `list_spec` it does.
            super::Screen::Settings
                if self.settings.page() == crate::menu::options::SettingsPage::Language =>
            {
                let lang = self.settings.language();
                Some(super::language::list_spec(
                    lang.entries().len(),
                    lang.scroll(),
                ))
            }
            // Resource Packs. Its two columns share **one** vertical band, so one
            // spec is the right clip rect for both; the length and offset are
            // `PacksNav::focused_list`'s column, which is the one under the
            // pointer (falling back to the cursor's) and is also the column the
            // wheel acts on. See `packs`'s module doc on why the thumb reflects one
            // column rather than both.
            super::Screen::Settings
                if self.settings.page() == crate::menu::options::SettingsPage::ResourcePacks =>
            {
                let packs = self.settings.packs();
                Some(super::packs::list_spec(
                    packs.focused_len(),
                    packs.scroll(),
                ))
            }
            // Social Interactions (#445's fourth and last adoption, and the only
            // user of `RowBand::Inset` — its rows are full-width, so no constant
            // `row_w` could place its scrollbar; see `social::list_spec`).
            super::Screen::Social => Some(super::social::list_spec(
                self.social.entries().len(),
                self.social.scroll(),
            )),
            // Every other settings page (#445's last adoption). Its entry heights
            // are **non-uniform** — a header is taller than a control row — so
            // this is the one arm that goes through `ListSpec::with_heights`.
            // Placed after the KeyBinds/Language arms, which are their own
            // geometry rather than `OptionsList`'s.
            //
            // **`None` for a page with no list.** `SettingsPage::Root` is an
            // arranged widget grid, not an `OptionsList` — `Root.entries()` is
            // empty — so reporting a spec there would hang a scrollbar beside a
            // screen with no rows. `ListSpec::model` would reject it anyway, but
            // saying so here is the same explicitness the `Accounts` arm above
            // uses for its sign-in views, and it keeps `active_list`'s answer
            // meaning "this screen has a list".
            super::Screen::Settings => {
                let page = self.settings.page();
                if page.entries().is_empty() {
                    None
                } else {
                    Some(crate::menu::options::list_spec(page, self.settings.scroll()))
                }
            }
            // Create New World's Game Rules sub-screen (issue #592's More
            // tab). Gated on the nested mode, not the bare screen — the
            // ordinary Game/World/More tabs have no list at all, so an
            // unconditional arm here would hang a scrollbar beside them.
            super::Screen::CreateWorld if self.create_world.game_rules_open() => Some(
                crate::menu::create_world::game_rules_list_spec(self.create_world.game_rules_scroll()),
            ),
            // Create New World's Data Packs sub-screen (issue #592's More
            // tab). Same guard shape as the Game Rules arm immediately
            // above, gated on its own nested mode — the length is not a
            // constant the way `GAME_RULES.len()` is, since it comes from a
            // real directory scan.
            super::Screen::CreateWorld if self.create_world.data_packs_open() => {
                Some(crate::menu::create_world::data_packs_list_spec(
                    self.create_world.data_packs_len(),
                    self.create_world.data_packs_scroll(),
                ))
            }
            _ => None,
        }
    }

    /// Scroll whichever list [`Self::active_list`] reports by `notches` of mouse
    /// wheel, at a `canvas_height`-tall canvas — vanilla's
    /// `AbstractScrollArea::mouseScrolled` on the active screen.
    ///
    /// The write-back half of the hook, and the reason `app` needs exactly **one**
    /// `MouseWheel` arm for the whole menu rather than one per screen. Returns
    /// whether anything moved, so the caller can tell "no list here" from "the list
    /// is already at its clamp" if it ever needs to.
    ///
    /// The arithmetic is [`super::widget::ScrollList`]'s in every arm — this only
    /// decides *which* offset field the result lands in.
    pub fn scroll_active_list(
        &mut self,
        ui: &super::UiState,
        notches: f32,
        canvas_height: f32,
    ) -> bool {
        match ui.screen() {
            super::Screen::ServerList => {
                let before = self.server_scroll;
                self.scroll_server_list(notches, canvas_height);
                self.server_scroll != before
            }
            super::Screen::Accounts => {
                let accounts = self.accounts();
                let before = accounts.scroll();
                accounts.scroll_by(notches, canvas_height);
                accounts.scroll() != before
            }
            // The save list (issue #541). Same screen as `active_list`'s arm — the
            // two sets must agree, or the wheel scrolls a screen with no bar or a
            // bar sits beside a screen the wheel does not reach.
            super::Screen::WorldSelect => {
                let before = self.world_select.scroll();
                self.world_select.scroll_by(notches, canvas_height);
                self.world_select.scroll() != before
            }
            super::Screen::Statistics => {
                let before = self.stats.scroll();
                self.stats.scroll_by(notches, canvas_height);
                self.stats.scroll() != before
            }
            // Key Binds (#445). Same page guard as `active_list`'s arm — the two
            // sets must agree, or the wheel scrolls a screen with no bar or a bar
            // sits beside a screen the wheel does not reach.
            super::Screen::Settings
                if self.settings.page() == crate::menu::options::SettingsPage::KeyBinds =>
            {
                let binds = self.settings.key_binds_mut();
                let before = binds.scroll();
                binds.scroll_by(notches, canvas_height);
                binds.scroll() != before
            }
            // Language (#445). Same page guard as `active_list`'s arm.
            super::Screen::Settings
                if self.settings.page() == crate::menu::options::SettingsPage::Language =>
            {
                let lang = self.settings.language_mut();
                let before = lang.scroll();
                lang.scroll_by(notches, canvas_height);
                lang.scroll() != before
            }
            // Resource Packs (issue #415). Same page guard as `active_list`'s
            // arm; the wheel moves whichever column the cursor is in.
            super::Screen::Settings
                if self.settings.page() == crate::menu::options::SettingsPage::ResourcePacks =>
            {
                let packs = self.settings.packs_mut();
                let before = packs.scroll();
                packs.scroll_by(notches, canvas_height);
                packs.scroll() != before
            }
            // Social Interactions (#445). Same screen as `active_list`'s arm.
            super::Screen::Social => {
                let before = self.social.scroll();
                self.social.scroll_by(notches, canvas_height);
                self.social.scroll() != before
            }
            // Every other settings page (#445). Same ordering as `active_list`.
            super::Screen::Settings => {
                let before = self.settings.scroll();
                self.settings.scroll_by(notches, canvas_height);
                self.settings.scroll() != before
            }
            // Create New World's Game Rules sub-screen. Same guard as
            // `active_list`'s own arm — the two sets must agree.
            super::Screen::CreateWorld if self.create_world.game_rules_open() => {
                let before = self.create_world.game_rules_scroll();
                self.create_world
                    .scroll_game_rules_by(notches, canvas_height);
                self.create_world.game_rules_scroll() != before
            }
            // Create New World's Data Packs sub-screen. Same guard as
            // `active_list`'s own arm — the two sets must agree.
            super::Screen::CreateWorld if self.create_world.data_packs_open() => {
                let before = self.create_world.data_packs_scroll();
                self.create_world
                    .scroll_data_packs_by(notches, canvas_height);
                self.create_world.data_packs_scroll() != before
            }
            _ => false,
        }
    }

    /// Scrolls the multiplayer list by `notches` of mouse wheel — vanilla's
    /// `AbstractScrollArea::mouseScrolled`,
    /// `setScrollAmount(scrollAmount() - scrollY * scrollRate())`
    /// (`AbstractScrollArea.java`).
    ///
    /// `notches` is winit's `scrollY` verbatim, so **positive scrolls up**
    /// (toward entry 0), matching vanilla's sign — the negation lives in
    /// [`super::widget::ScrollList::mouse_scrolled`], not here, so there is
    /// exactly one place the sign can be got wrong.
    ///
    /// **Delegates to [`super::widget::ScrollList`] rather than reimplementing
    /// the arithmetic**, which is what makes one notch land on 18 px rather than
    /// a whole 36 px entry: that type owns `scrollRate = defaultEntryHeight / 2`
    /// and `setScrollAmount`'s `Mth.clamp`, both already gated against the jar.
    /// A fractional notch (a trackpad `PixelDelta`) therefore moves a
    /// proportional number of pixels instead of being rounded to a row, and
    /// three notches reach 54 px — a position the old `usize` model could not
    /// represent at all.
    ///
    /// Takes the real canvas height because the mouse wheel is the one call site
    /// that has it (`app.rs` already resolves the framebuffer to a logical
    /// canvas for every cursor event) — unlike keyboard scroll-into-view, which
    /// runs on every arrow press and uses the canvas-independent
    /// [`super::render::server_list_window_rows`] instead so it needs no new
    /// plumbing from `app.rs`. A no-op on an empty list, where there is no band
    /// to clamp against.
    pub fn scroll_server_list(&mut self, notches: f32, canvas_height: f32) {
        let Some(mut list) =
            super::render::server_scroll_model(self.list.len(), canvas_height)
        else {
            return;
        };
        list.set_scroll(self.server_scroll);
        list.mouse_scrolled(notches);
        self.server_scroll = list.scroll();
    }

    /// Keeps [`Self::server`] inside the scrolled window — vanilla's
    /// `AbstractSelectionList.scrollToEntry` (`:251-261`), in pixels, modelled on
    /// [`super::accounts`]'s `scroll_to_show`. Uses the canvas-independent
    /// [`super::render::server_list_window_rows`] rather than a real canvas
    /// height, so a keyboard press needs no new plumbing from `app.rs` — see
    /// that function's doc on why the result is safe (never wrong direction)
    /// even when it under-uses a larger canvas.
    ///
    /// Both deltas are measured against the *current* offset and applied in
    /// order, exactly as `scrollToEntry` does, so this is the minimum move that
    /// brings the row fully into the band — an arrow press off the bottom edge
    /// advances by one row's 36 px, not by a whole window.
    fn scroll_server_to_show(&mut self) {
        let row_h = super::render::SERVER_LIST_ITEM_H;
        let window_px = super::render::server_list_window_rows() as f32 * row_h;
        let row_top = self.server as f32 * row_h;
        if row_top < self.server_scroll {
            self.server_scroll = row_top;
        } else if row_top + row_h > self.server_scroll + window_px {
            self.server_scroll = row_top + row_h - window_px;
        }
        // Never leave the window scrolled past the point where it has nothing
        // left to reveal, e.g. right after a delete shrinks the list.
        let max = (self.list.len() as f32 * row_h - window_px).max(0.0);
        self.server_scroll = self.server_scroll.clamp(0.0, max);
    }

    /// Records the mouse position in logical pixels, and the canvas it was
    /// measured in (#396).
    ///
    /// Called from `app.rs`'s `menu_row_at`, which already computes both — so it
    /// runs before every hover *and* every click, and needs no new plumbing at the
    /// click site. See [`Self::menu_cursor`] for why a row index is not enough.
    pub fn set_menu_cursor(&mut self, x: f32, y: f32, canvas_width: f32, canvas_height: f32) {
        self.menu_cursor = Some((x, y, canvas_width, canvas_height));
    }

    /// The last known logical mouse position, for a frame builder that needs the
    /// position itself rather than a row index — see [`MenuNav::set_menu_cursor`]
    /// and [`super::render::MenuFrame::cursor`].
    #[must_use]
    pub fn menu_cursor(&self) -> Option<(f32, f32)> {
        self.menu_cursor.map(|(x, y, _, _)| (x, y))
    }

    /// The cursor's position **relative to the favicon** of list row `row`, or
    /// `None` when there is no cursor yet or it is outside that row.
    ///
    /// This is `relX`/`relY` in `OnlineServerEntry.mouseClicked` — `event.x() -
    /// getContentX()` (`ServerSelectionList.java`) — and it is derived
    /// through [`super::render::server_row_content_rect`], the same expression the
    /// draw uses, rather than restating the row geometry here. That is what keeps
    /// the highlighted quadrant and the quadrant that acts from drifting apart.
    /// Returns `(rel_x, rel_y, size)`, so the caller passes the same `size` the
    /// draw blits at rather than restating vanilla's 32.
    #[must_use]
    fn entry_icon_cursor(&self, row: usize) -> Option<(f32, f32, f32)> {
        let (x, y, canvas_w, canvas_h) = self.menu_cursor?;
        // A canvas is only known once a frame has been hit-tested; a zero one
        // would put every row at the same place.
        if canvas_w <= 0.0 || canvas_h <= 0.0 {
            return None;
        }
        let (ix, iy, iw, _) = super::render::server_entry_icon_rect(row, canvas_w, self.server_scroll);
        Some((x - ix, y - iy, iw))
    }

    /// The highlighted pause-menu button.
    #[must_use]
    pub fn pause_button(&self) -> PauseButton {
        let buttons = self.pause_buttons();
        buttons[self.paused.min(buttons.len() - 1)]
    }

    /// Index of the highlighted pause-menu button.
    #[must_use]
    pub fn pause_index(&self) -> usize {
        self.paused
    }

    /// The highlighted death-screen button (issue #103).
    #[must_use]
    pub fn death_button(&self) -> DeathButton {
        DEATH_BUTTONS[self.death.min(DEATH_BUTTONS.len() - 1)]
    }

    /// Index of the highlighted death-screen button.
    #[must_use]
    pub fn death_index(&self) -> usize {
        self.death
    }

    /// The add/edit form.
    #[must_use]
    pub fn form(&self) -> &EditForm {
        &self.form
    }

    /// The command block edit screen's state (issue #47), or `None` when
    /// [`Screen::CommandBlockEdit`] is not showing — see [`Self::command_block`]'s
    /// own field doc for why this is the one screen-state field that is not
    /// eagerly non-empty.
    #[must_use]
    pub fn command_block(&self) -> Option<&command_block::CommandBlockState> {
        self.command_block.as_ref()
    }

    /// The book-editing screen's state, or `None` when [`Screen::BookEdit`]
    /// is not showing — see [`Self::book_edit`]'s own field doc.
    #[must_use]
    pub fn book_edit(&self) -> Option<&book_edit::BookEditState> {
        self.book_edit.as_ref()
    }

    /// The sign-editing screen's state, or `None` when [`Screen::SignEdit`] is
    /// not showing — see [`Self::sign_edit`]'s own field doc.
    #[must_use]
    pub fn sign_edit(&self) -> Option<&sign_edit::SignEditState> {
        self.sign_edit.as_ref()
    }

    /// The server's command tree, for the screens that complete against it —
    /// see [`Self::command_tree`]'s own field doc. `None` means "offer no
    /// completions", never "an empty tree".
    #[must_use]
    pub fn command_tree(&self) -> Option<&lodestone_model::command_tree::CommandTree> {
        self.command_tree.as_deref()
    }

    /// Push the server's command tree down from `app` (issue #471 step 2).
    /// Idempotent and cheap — an `Arc` clone — so a caller that has one may
    /// call this every time it opens a screen rather than tracking whether the
    /// tree has changed. Passing `None` (no live session, or no
    /// `minecraft:commands` yet) clears it, which is the honest state: a stale
    /// tree from a previous server is worse than none.
    pub fn set_command_tree(
        &mut self,
        tree: Option<std::sync::Arc<lodestone_model::command_tree::CommandTree>>,
    ) {
        self.command_tree = tree;
    }

    /// Pushes the session's real publish state in, from
    /// `Sim::is_lan_published` — see [`Self::lan_published`]'s own field doc.
    pub fn set_lan_published(&mut self, published: bool) {
        self.lan_published = published;
    }

    /// The last-pushed publish state — what [`Self::pause_buttons`] keys its
    /// row list on, and what `render::pause_frame` reads to pick the matching
    /// grid arrangement for each row's rect.
    #[must_use]
    pub fn is_lan_published(&self) -> bool {
        self.lan_published
    }

    /// The pause menu's active row list: [`PAUSE_BUTTONS_PUBLISHED`] once the
    /// hosted world is published, [`PAUSE_BUTTONS`] otherwise. Every internal
    /// user of a pause-screen row — [`Self::pause_button`], the hover/click
    /// hit test, and the keyboard walk — reads through this rather than the
    /// two constants directly, so they cannot drift onto different lists.
    ///
    /// **A `Vec`, not `&'static [PauseButton]` any more.** [`PauseButton::
    /// ServerLinks`] is appended here rather than living in
    /// [`PAUSE_BUTTONS`]/[`PAUSE_BUTTONS_PUBLISHED`], because its presence
    /// depends on session data (whether the server announced any links) that
    /// those two `const` arrays cannot express — see that variant's own doc.
    #[must_use]
    pub fn pause_buttons(&self) -> Vec<PauseButton> {
        let base: &[PauseButton] = if self.lan_published {
            &PAUSE_BUTTONS_PUBLISHED
        } else {
            &PAUSE_BUTTONS
        };
        let mut buttons = base.to_vec();
        if self.server_links.has_links() {
            buttons.push(PauseButton::ServerLinks);
        }
        buttons
    }

    /// Opens the command block edit screen (issue #47) with `open`'s data —
    /// which a right-click handler would read off the block entity's NBT; see
    /// [`command_block`]'s module doc for why nothing does that yet. Only from
    /// [`Screen::Playing`], matching [`UiState::open_command_block`]'s own
    /// guard (this method drives both: the widget state here, the screen
    /// there, in that order — mirroring [`Self::form`]'s
    /// `EditForm::adding`-then-`ui.open_server_edit()` pairing at every one of
    /// its call sites).
    pub fn open_command_block(&mut self, ui: &mut UiState, open: command_block::CommandBlockOpen) {
        if ui.screen() == Screen::Playing {
            self.command_block = Some(command_block::CommandBlockState::new(open));
            ui.open_command_block();
        }
    }

    /// Closes the command block edit screen without sending anything — Cancel,
    /// or Escape. [`Self::activate_command_block_row`]'s `Done` arm is the
    /// other way out, and it sends [`command_block::CommandBlockState::
    /// to_action`] before calling this same method.
    pub fn close_command_block(&mut self, ui: &mut UiState) {
        self.command_block = None;
        ui.close_command_block();
    }

    /// Opens the sign-editing screen with `open`'s data — read off the sign's
    /// already-synced block-entity NBT by whatever consumes
    /// `ClientEvent::SignEditorOpened` (see [`sign_edit`]'s module doc). Only
    /// from [`Screen::Playing`], matching [`Self::open_command_block`]'s own
    /// guard and driving both the widget state here and the screen there, in
    /// that order.
    pub fn open_sign_edit(&mut self, ui: &mut UiState, open: sign_edit::SignEditOpen) {
        if ui.screen() == Screen::Playing {
            self.sign_edit = Some(sign_edit::SignEditState::new(open));
            ui.open_sign_edit();
        }
    }

    /// Closes the sign-editing screen. **Callers must take
    /// [`sign_edit::SignEditState::to_action`] before calling this** — it
    /// drops the state — matching [`Self::activate_sign_edit_row`] and
    /// [`Self::key_sign_edit`]'s own Escape arm, both of which do exactly
    /// that. See [`Screen::SignEdit`]'s own doc on why, unlike
    /// [`Self::close_command_block`], there is no "close without sending"
    /// caller.
    pub fn close_sign_edit(&mut self, ui: &mut UiState) {
        self.sign_edit = None;
        ui.close_sign_edit();
    }

    /// Opens the book-editing screen with `open`'s data — the draft's current
    /// pages, read off the item stack in hand. Only from [`Screen::Playing`],
    /// matching [`Self::open_command_block`]'s own guard: this screen is
    /// client-local, not server-driven, the same shape as the command block.
    pub fn open_book_edit(&mut self, ui: &mut UiState, open: book_edit::BookEditOpen) {
        if ui.screen() == Screen::Playing {
            self.book_edit = Some(book_edit::BookEditState::new(open));
            ui.open_book_edit();
        }
    }

    /// Closes the book-editing screen, whether Done, Finalize, or Escape
    /// triggered it — matching [`Self::close_command_block`]'s own "either
    /// way" phrasing, since which one happened decided *whether a packet was
    /// sent*, not which screen comes next. Callers that mean to send take
    /// [`book_edit::BookEditState::to_save_action`]/[`to_sign_action`
    /// ](book_edit::BookEditState::to_sign_action) **before** calling this —
    /// it drops the state.
    pub fn close_book_edit(&mut self, ui: &mut UiState) {
        self.book_edit = None;
        ui.close_book_edit();
    }

    /// The last persistence failure, if any.
    #[must_use]
    pub fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// The account list + sign-in flow state (issue #66).
    #[must_use]
    pub fn accounts(&self) -> &crate::menu::accounts::AccountsNav {
        &self.accounts
    }

    /// The world-select screen's widgets and focus (issue #397).
    #[must_use]
    pub fn world_select(&self) -> &crate::menu::world_select::WorldSelectNav {
        &self.world_select
    }

    /// The live confirmation screen (issue #540) — what it asks and what it will
    /// do if answered affirmatively.
    #[must_use]
    pub fn confirm(&self) -> &crate::menu::confirm::ConfirmNav {
        &self.confirm
    }

    /// The live resource-pack prompt, if [`Screen::ResourcePackPrompt`] is
    /// showing — what it asks and which pack it answers for. `None` off that
    /// screen, the same shape [`Self::sign_edit`]/[`Self::command_block`]
    /// use.
    #[must_use]
    pub fn resource_pack_prompt(&self) -> Option<&crate::menu::confirm::ResourcePackPromptNav> {
        self.resource_pack_prompt.as_ref()
    }

    /// Opens the resource-pack prompt for `prompt` — `app/session.rs`'s
    /// `drive_ui_from_session` calls this once per frame while
    /// `NetClient::pending_resource_pack_prompt` is `Some` and the prompt is
    /// not already showing (mirroring how it reconciles
    /// `Sim::is_dead`/`Sim::has_won` into their own screens). A **new**
    /// `ResourcePackPromptNav` is built every call rather than reused, so a
    /// second push cannot inherit a stale focus or a stale pack id — the
    /// same replace-not-mutate discipline [`Self::confirm`]'s own doc
    /// describes for [`Screen::WorldSelect`]'s Delete confirmation.
    pub fn show_resource_pack_prompt(
        &mut self,
        ui: &mut UiState,
        prompt: &crate::net::PendingResourcePackPrompt,
    ) {
        self.resource_pack_prompt = Some(crate::menu::confirm::ResourcePackPromptNav::new(prompt));
        ui.open_resource_pack_prompt();
    }

    /// Moves the highlight to row `row` of the current screen, as a mouse hover
    /// would. Out-of-range rows are ignored rather than clamped: the caller
    /// hit-tests against the rendered rects, so "no row here" must not silently
    /// move the selection to a different one.
    ///
    /// A **disabled** row is still hovered, matching vanilla exactly:
    /// `AbstractWidget::extractRenderState` sets `isHovered` from geometry alone
    /// and never consults `active` (`AbstractWidget.java`), while
    /// `WidgetSprites::get(active, focused)` returns `button_disabled` whichever
    /// way `focused` went (`WidgetSprites.java`) — so a greyed-out button
    /// under the cursor looks greyed-out, not highlighted. The half that matters
    /// is the *click*: `key_main`/`key_paused` refuse Enter on a disabled button,
    /// which is why moving the highlight onto one here is safe.
    pub fn hover(&mut self, ui: &UiState, row: usize) {
        match ui.screen() {
            Screen::MainMenu if row < MAIN_BUTTONS.len() => self.main = row,
            // Two cursors on one screen (#396), and hover drives only one of
            // them: the seven rows above `list.len()` are footer buttons and move
            // a button highlight, while the server entries below it move
            // **nothing**. That is what lets a selected server stay outlined
            // while the cursor travels to Join — see `hover_list`.
            Screen::ServerList => self.hover_list(row),
            Screen::Paused if row < self.pause_buttons().len() => self.paused = row,
            Screen::Death if row < DEATH_BUTTONS.len() => self.death = row,
            Screen::Accounts => self.accounts.hover(row),
            // The one screen where hover is **not** the row cursor: it records
            // hover alone and leaves focus where it is, or dragging the mouse
            // across the footer would pull the keyboard out of the search field.
            // See `world_select::WorldSelectNav::hovered`.
            Screen::WorldSelect => self.world_select.hover(row),
            // The confirmation screen (issue #540) — hover is not focus here for
            // a sharper reason than on the world list: a hover that moved focus
            // onto the affirmative button would arm the *next* Enter to delete.
            // See `confirm::ConfirmNav::hover`.
            Screen::Confirm => self.confirm.hover(row),
            // Same reasoning as `Screen::Confirm` immediately above.
            Screen::ResourcePackPrompt => {
                if let Some(prompt) = &mut self.resource_pack_prompt {
                    prompt.hover(row);
                }
            }
            // `hover_row` is `ContainerEventHandler.setFocused(child)` for the
            // two text fields — real focus, not a highlight index, because the
            // row indices and `EditForm`'s focus ids are the same numbers (see
            // [`NAME_FIELD`]) — and plain hover tracking for the three button
            // rows the screen's framework conversion added (see
            // [`EditForm::hover_row`]).
            Screen::ServerEdit => self.form.hover_row(row),
            // Create New World tracks hover the same way `EditForm` does, and
            // for the same reason: its text fields take real focus while its
            // buttons take a highlight only. Its absence from this match is
            // exactly why none of that screen's buttons drew a hover outline —
            // the screen already reached `stamp_canvas_facts` through
            // `render::frame_for`, so every canvas fact was present and only
            // `MenuFrame::hovered` stayed `None`, every frame.
            Screen::CreateWorld => self.create_world.hover_row(row),
            // Statistics (issues #564/#567's audit): this screen's only real
            // control is Done, and this arm's own absence was exactly the
            // Create New World bug above, one screen over — `StatsNav::
            // hover_row` records nothing for anything but `DONE_ROW`, since
            // the tab bar's own hover is derived from `MenuFrame::cursor` at
            // draw time (see `stats::hover_row`'s own doc).
            Screen::Statistics => self.stats.hover_row(row),
            Screen::ServerLinks => self.server_links.hover(row),
            // The settings tree now *has* a cursor (issue #55), so hover moves
            // it — this arm's absence is what let #391 happen, because a screen
            // with no hover arm had to route a click through `Enter`. Row indices
            // are indices into `SettingsNav::visible`, which is also what
            // `render::frame_for` builds its rows from.
            //
            // Key Binds (issue #15) is a sub-page of this same `Screen`, and it
            // is not an `OptionsList` page — see `SettingsPage::KeyBinds`'s own
            // doc — so its row indices are `KeyBindsNav::visible`'s, a
            // different list from `SettingsNav::visible`. Guarded ahead of the
            // plain arm below rather than inside it, matching how `hover_list`
            // and `world_select.hover` already get their own arms instead of a
            // branch buried in a shared method.
            Screen::Settings if self.settings.page() == crate::menu::options::SettingsPage::KeyBinds => {
                self.settings.key_binds_mut().hover_row(row);
            }
            // Language (issue #415) — same reasoning, one row index space
            // over: row 0 is the search field (always focused, never
            // hovered — see `menu::language::frame`'s doc), so only rows
            // past it move the cursor.
            Screen::Settings if self.settings.page() == crate::menu::options::SettingsPage::Language => {
                if let Some(row) = row.checked_sub(1) {
                    self.settings.language_mut().hover_row(row);
                }
            }
            // Telemetry (issue #415) — no search field here, so (unlike
            // Language) row indices need no offset.
            Screen::Settings if self.settings.page() == crate::menu::options::SettingsPage::Telemetry => {
                self.settings.telemetry_mut().hover_row(row);
            }
            // Resource Packs (issue #415) — same reasoning as Telemetry, no
            // search field, no offset.
            Screen::Settings if self.settings.page() == crate::menu::options::SettingsPage::ResourcePacks => {
                self.settings.packs_mut().hover_row(row);
            }
            Screen::Settings => self.settings.hover_row(row),
            // Social Interactions (#189) — same reasoning as the Settings
            // arm above: without this, a click would have to route through
            // `Enter`, which is #391's exact trap one screen further.
            Screen::Social => self.social.hover_row(row),
            // The command block edit screen (issue #47) — plain hover
            // tracking, like `Screen::Paused`/`Screen::Death` above: this
            // screen has no keyboard-focus cursor to move (see
            // `command_block::CommandBlockState`'s own doc), only a mouse
            // highlight.
            Screen::CommandBlockEdit => {
                if let Some(state) = self.command_block.as_mut() {
                    state.hovered = Some(row);
                }
            }
            // The sign-editing screen — same shape as `CommandBlockEdit`
            // immediately above, narrower: its only hoverable row is Done
            // (row index [`sign_edit_row::DONE`]), so anything else clears the
            // highlight rather than recording a row that draws no hover state.
            Screen::SignEdit => {
                if let Some(state) = self.sign_edit.as_mut() {
                    state.done_hovered = row == sign_edit_row::DONE;
                }
            }
            // The book-editing screen — same shape as `CommandBlockEdit`
            // above: plain mouse-highlight tracking, no keyboard row cursor.
            Screen::BookEdit => {
                if let Some(state) = self.book_edit.as_mut() {
                    state.hovered = Some(row);
                }
            }
            _ => {}
        }
    }

    /// A left-click that landed on row `row` of the current screen.
    ///
    /// # Why this is not just `hover` then `Enter`
    ///
    /// That *is* what it does for every screen with a row cursor, and it is what
    /// `app.rs` used to inline. The **settings** screen is the exception, and it
    /// is the exception that was issue #391.
    ///
    /// That screen used to have no cursor at all — each control owned its own
    /// key instead — so [`MenuNav::hover`] had no `Screen::Settings` arm and a
    /// click could not move a highlight even in principle. The old translation
    /// therefore turned a click on *any* row into `MenuKey::Enter`, and on that
    /// screen `Enter` meant "toggle View Bobbing" unconditionally. So clicking
    /// the **GUI SCALE** row — row 0, the one `render.rs` marked `selected`, the
    /// natural thing to click — silently turned View Bobbing off and persisted
    /// it.
    ///
    /// That is the whole of #391: the reporter's `options.json` carried
    /// `"view_bobbing": false`, written six minutes before the report, and every
    /// hop of the render chain underneath it was working. The bug was never in
    /// the bob; it was a mouse that did the opposite of the label under it.
    ///
    /// Issue #55 gave that screen 135 controls and a real cursor, which removes
    /// the *cause* rather than patching the symptom: a click now resolves its row
    /// to that row's own [`super::options::Control`] and acts on it, and there is
    /// no shared per-screen meaning of `Enter` left to mis-apply.
    ///
    /// # The row indices are a coupling, and it is guarded
    ///
    /// A row index here means whatever `menu::render::frame_for` put in
    /// `Screen::Settings`'s `rows`, which is a different file — and, since #55,
    /// depends on which page is showing and how far it is scrolled.
    /// `options::tests::the_settings_rows_are_in_the_order_click_assumes` walks
    /// every page at every scroll position and asserts the two agree, so a table
    /// edit fails a test instead of silently rebinding the mouse to the wrong
    /// control.
    /// Whether visible `row` is a live slider the mouse can drag.
    ///
    /// The app asks this on mouse-down to decide between the drag path and the
    /// ordinary click path; only the settings tree has sliders.
    #[must_use]
    pub fn slider_row(&self, ui: &UiState, row: usize) -> bool {
        ui.screen() == Screen::Settings && self.settings.slider_row_option(row).is_some()
    }

    /// Set the slider at visible `row` from a track `fraction` — vanilla's
    /// `AbstractSliderButton.setValueFromMouse`, reached from both the initial
    /// click and every subsequent drag position.
    ///
    /// Returns `true` when it was applied. `false` means "not a draggable
    /// slider", and the app then falls back to [`Self::click`] so nothing that
    /// used to work stops working.
    ///
    /// The row is put under the cursor first ([`super::options::SettingsNav::hover_row`]),
    /// so a drag also moves the keyboard cursor — matching vanilla, where
    /// clicking a widget focuses it.
    pub fn drag_slider(&mut self, ui: &UiState, row: usize, fraction: f32) -> bool {
        if ui.screen() != Screen::Settings {
            return false;
        }
        let Some(live) = self.settings.slider_row_option(row) else {
            return false;
        };
        self.settings.hover_row(row);
        self.set_live_slider(live, fraction)
    }

    pub fn click(&mut self, ui: &mut UiState, row: usize) -> MenuAction {
        // The edit form is the second screen where "hover then Enter" is wrong,
        // and #395 is what makes it visible. `ContainerEventHandler.mouseClicked`
        // focuses the child it hit and calls *its* `onClick`; it does not activate
        // the screen. Translating a click into `Enter` here meant **clicking
        // either address field tried to save the form** — the same shape as #391,
        // one screen over: with a valid address it closed the form the player was
        // still typing into, and without one it flashed "AN ADDRESS IS REQUIRED"
        // at someone who had just clicked the field to fix that.
        if ui.screen() == Screen::ServerEdit {
            return match row {
                NAME_FIELD | ADDRESS_FIELD => {
                    self.form.focus_row(row);
                    MenuAction::None
                }
                // Vanilla's Done/Cancel (`ManageServerScreen.java`), now
                // real clickable rows since the screen's framework conversion
                // — see `save_entry`/`cancel_edit`, also reached by
                // Enter/Escape so the two paths cannot disagree.
                DONE_ROW => self.save_entry(ui),
                CANCEL_ROW => self.cancel_edit(ui),
                // `ManageServerScreen`'s `manageServer.resourcePack`
                // `CycleButton` — see `RESOURCE_PACK_ROW`'s doc.
                RESOURCE_PACK_ROW => {
                    self.form.cycle_pack_status();
                    MenuAction::None
                }
                // Anything past the five rows this screen has: a click does
                // nothing, same as every other inactive control.
                _ => MenuAction::None,
            };
        }
        // The third screen where it is wrong, and the reason the parent issue
        // insists every cursorless screen gets its own arm (issue #397): here a
        // click means "focus this field" *or* "press this button", never both,
        // and a click on one of the four disabled buttons means nothing at all.
        // Play Selected World is the one that does something — it launches (#287).
        if ui.screen() == Screen::WorldSelect {
            let outcome = self.world_select.click_row(row);
            return self.apply_world_select(ui, outcome);
        }
        // The confirmation screen (issue #540). Its own arm for the same reason,
        // and here the "hover then Enter" translation would be actively
        // destructive rather than merely wrong: the row a hover had highlighted
        // would be the row Enter pressed.
        if ui.screen() == Screen::Confirm {
            let outcome = self.confirm.click_row(row);
            return self.apply_confirm(ui, outcome);
        }
        // The resource-pack prompt. Same reasoning as `Screen::Confirm`
        // immediately above.
        if ui.screen() == Screen::ResourcePackPrompt {
            let Some(prompt) = &mut self.resource_pack_prompt else {
                return MenuAction::None;
            };
            let outcome = prompt.click_row(row);
            return self.apply_resource_pack_prompt(ui, outcome);
        }
        // World Creation (issue #190) — #391's shape again: a click focuses a
        // field or presses a button, never "hover then Enter".
        if ui.screen() == Screen::CreateWorld {
            let outcome = self.create_world.click_row(row);
            return self.apply_create_world(ui, outcome);
        }
        if ui.screen() == Screen::Settings {
            // Key Binds (issue #15) again — see `hover`'s matching guard.
            if self.settings.page() == crate::menu::options::SettingsPage::KeyBinds {
                let outcome = self
                    .settings
                    .key_binds_mut()
                    .click_row(row, &self.options.keybinds);
                return self.apply_key_binds(ui, outcome);
            }
            // Language (issue #415) again — row 0 is the always-focused
            // search field, so a click there is a no-op (there is nothing to
            // move focus *to* — see `hover`'s matching guard).
            if self.settings.page() == crate::menu::options::SettingsPage::Language {
                let outcome = match row.checked_sub(1) {
                    Some(row) => self.settings.language_mut().click_row(row),
                    None => crate::menu::language::LanguageOutcome::None,
                };
                return self.apply_language(ui, outcome);
            }
            // Telemetry (issue #415) again — no search field, no offset.
            if self.settings.page() == crate::menu::options::SettingsPage::Telemetry {
                let outcome = self.settings.telemetry_mut().click_row(row);
                return self.apply_telemetry(ui, outcome);
            }
            // Resource Packs (issue #415) again.
            if self.settings.page() == crate::menu::options::SettingsPage::ResourcePacks {
                let outcome = self.settings.packs_mut().click_row(row);
                return self.apply_packs(ui, outcome);
            }
            // A click that hit-tested onto a row this page does not have does
            // nothing at all (`SettingsNav::click_row` returns `None` for an
            // out-of-range row), rather than falling through to the keyboard
            // path — which is the other half of #391's fix.
            let outcome = self.settings.click_row(row);
            return self.apply_settings(ui, outcome);
        }
        // Social Interactions (#189) — #391's fix, one screen further: a
        // click resolves directly to the row it hit, never through Enter.
        if ui.screen() == Screen::Social {
            let outcome = self.social.click_row(row);
            return self.apply_social(ui, outcome);
        }
        // The fourth (#396). A click on a *row* here is
        // `AbstractSelectionList.mouseClicked` — it selects, and only the favicon's
        // quadrants act — while a click above the rows is one of seven buttons.
        // Routing it as `hover` + `Enter` would join a server on any click on its
        // row, which vanilla reserves for the join icon and the double-click.
        if ui.screen() == Screen::ServerList {
            return self.click_list(ui, row);
        }
        // The command block edit screen (issue #47) — #391's fix once more: a
        // click on `CommandBlockRow::Command` is caret placement (a no-op
        // here, see `activate_command_block_row`'s own doc), and a click on
        // any other row is a button press, never routed through `Enter`.
        if ui.screen() == Screen::CommandBlockEdit {
            return self.activate_command_block_row(ui, row);
        }
        // The sign-editing screen — same #391 shape: a click on a line field
        // is caret placement (`app.rs`'s to translate, like the command
        // block's own field), a click on Done is activation, never routed
        // through `Enter` (which this screen repurposes for line navigation).
        if ui.screen() == Screen::SignEdit {
            return self.activate_sign_edit_row(ui, row);
        }
        // The book-editing screen — same #391 shape as `SignEdit` above: a
        // click on the title field (while signing) is caret placement, a
        // click on any other row is a button press.
        if ui.screen() == Screen::BookEdit {
            return self.activate_book_edit_row(ui, row);
        }
        // Statistics (issue #188) — the newest instance of #391's shape, and it
        // became *necessary* rather than merely tidy when Enter there stopped
        // being unconditional: see `click_statistics`.
        if ui.screen() == Screen::Statistics {
            return self.click_statistics(ui, row);
        }
        // Server Links — every row on both its views is a real button (a
        // link row, Back, or Yes/No), never a field, so the same #391 shape
        // applies: a click resolves directly to the row it hit.
        if ui.screen() == Screen::ServerLinks {
            return self.click_server_links(ui, row);
        }
        // #391's shape once more, and only while the account screen's offline-name
        // editor is open. The *list* screen still wants the `hover` + `Enter`
        // translation below (a click on an account row selects it — see
        // `AccountsNav::handle_key_with`'s `Enter` arm, which says so at length),
        // so this is deliberately narrower than the arms above: it fires on the
        // editor's own frame, where row 0 is an always-focused field with nothing
        // to move focus *to* (the world list's search row, exactly) and only the
        // Done button acts. Without it, clicking the field to fix a typo saved the
        // name instead.
        if ui.screen() == Screen::Accounts && self.accounts.is_editing_name() {
            use crate::menu::accounts::AccountsSignal;
            match self.accounts.click_name_edit_row(row) {
                AccountsSignal::Back => ui.close_accounts(),
                AccountsSignal::None => {}
            }
            return MenuAction::None;
        }
        self.hover(ui, row);
        self.key(ui, MenuKey::Enter)
    }

    /// [`Self::hover`]'s multiplayer arm. A footer row moves the button
    /// highlight; a **server row does nothing but clear it**, because on a
    /// selection list hover is not selection.
    ///
    /// This used to set `self.server`, so the 1 px row outline followed the mouse
    /// and a server could not stay selected while the cursor travelled to Join. A
    /// player reported it immediately. Vanilla reaches
    /// `AbstractSelectionList.setSelected` only from `setFocused`
    /// (`AbstractSelectionList.java`) and the click paths — never from
    /// hover; `ServerSelectionList.java` shows what hover *does* draw,
    /// which is a `fill(…, -1601138544)` scrim over the 32 px favicon plus the
    /// join / move-up / move-down sprite for the quadrant under the cursor.
    ///
    /// **Nothing is recorded for the row**, and that is deliberate rather than an
    /// omission: both of those visuals are driven by `MenuFrame::cursor` in
    /// `render.rs`, which bounds-tests the logical cursor against the row rect it
    /// is about to draw into. A `hovered` row index here would have no consumer —
    /// see `super::world_select::WorldSelectNav::hovered`, which *does* need one,
    /// because on that screen a hovered row must not steal focus from the search
    /// field.
    fn hover_list(&mut self, row: usize) {
        if row < self.list.len() {
            // Moving from the footer onto a row must put the button highlight
            // out, or it stays burnt in on whichever button was last crossed.
            self.list_button = None;
        } else if row - self.list.len() < SERVER_LIST_BUTTONS.len() {
            self.list_button = Some(row - self.list.len());
        }
    }

    /// [`Self::click`]'s multiplayer arm (#396).
    ///
    /// The row half is `OnlineServerEntry.mouseClicked`
    /// (`ServerSelectionList.java`) in vanilla's own order: the join
    /// quadrant first, then the two move quadrants with their index guards, and
    /// **selection last** — a plain click selects and does not join.
    ///
    /// The one piece of vanilla's ordering that is missing is the double-click
    /// (`if (doubleClick) this.join()`), because `app.rs` reports one click at a
    /// time with no interval. Joining is still one click away on the icon's right
    /// half, and one keypress away with Enter or the Join Server button.
    fn click_list(&mut self, ui: &mut UiState, row: usize) -> MenuAction {
        if row < self.list.len() {
            self.server = row;
            self.list_button = None;
            // The icon-quadrant checks, run only when the click actually
            // landed in the 32 px favicon (`entry_icon_cursor` is `Some`).
            // Unlike before, a click that misses the icon — or hits a
            // quadrant this row's position blocks, e.g. "move up" on row 0 —
            // no longer returns early here: it falls through to the
            // double-click check below instead of stopping dead, which is
            // the bug a player report (2026-08-04) traced to this function.
            if let Some((rx, ry, size)) = self.entry_icon_cursor(row) {
                if widget::over_right_half(rx, ry, size) {
                    return match self.list.get(row) {
                        Some(entry) => {
                            let entry = entry.clone();
                            ui.begin(SessionKind::Multiplayer);
                            MenuAction::Connect(entry)
                        }
                        None => MenuAction::None,
                    };
                }
                if row > 0 && widget::over_top_left_quarter(rx, ry, size) {
                    return self.swap_rows(row, row - 1);
                }
                if row + 1 < self.list.len() && widget::over_bottom_left_quarter(rx, ry, size) {
                    return self.swap_rows(row, row + 1);
                }
            }
            // Vanilla's own order (`ServerSelectionList.java`): after
            // the icon-quadrant checks above, **unconditionally**,
            // `if (doubleClick) join()` — it fires wherever on the row the
            // click landed, icon or not. `entry_icon_cursor` played no part
            // in reaching this before, and must not gate it now either.
            let now_ms = self.click_clock.elapsed().as_millis() as u64;
            if self.double_click.click(now_ms, row) {
                return match self.list.get(row) {
                    Some(entry) => {
                        let entry = entry.clone();
                        ui.begin(SessionKind::Multiplayer);
                        MenuAction::Connect(entry)
                    }
                    None => MenuAction::None,
                };
            }
            return MenuAction::None;
        }
        let Some(button) = SERVER_LIST_BUTTONS.get(row - self.list.len()).copied() else {
            return MenuAction::None;
        };
        self.list_button = Some(row - self.list.len());
        // `AbstractWidget.mouseClicked` returns false for an inactive widget, so
        // an inactive button swallows the click — the same rule `key_main` applies
        // to a disabled title-screen button.
        if !button.enabled(!self.list.is_empty()) {
            return MenuAction::None;
        }
        self.activate_list_button(ui, button)
    }

    /// Reorders the list and persists it — vanilla's
    /// `OnlineServerEntry.swap`, which is `servers.swap` then `servers.save`
    /// (`ServerSelectionList.java`, `:434-436`).
    ///
    /// The selection **follows the row**, matching vanilla's
    /// `scrollToEntry(children.get(newIndex))`: the entry the player grabbed stays
    /// the selected one, so a second click on the same arrow keeps moving it.
    fn swap_rows(&mut self, from: usize, to: usize) -> MenuAction {
        if !self.list.swap(from, to) {
            return MenuAction::None;
        }
        self.server = to;
        // #402: the swap can carry the selection to the edge of the scrolled
        // window (repeated clicks on the move-up/down arrow), matching
        // vanilla's own `scrollToEntry` call right after the swap.
        self.scroll_server_to_show();
        self.persist();
        MenuAction::None
    }

    /// What one footer button does. Each one is the mouse's route to something the
    /// keyboard can already do, except Direct Connection, which is inactive.
    fn activate_list_button(&mut self, ui: &mut UiState, button: ServerListButton) -> MenuAction {
        match button {
            ServerListButton::Select => match self.list.get(self.server) {
                Some(entry) => {
                    let entry = entry.clone();
                    ui.begin(SessionKind::Multiplayer);
                    MenuAction::Connect(entry)
                }
                None => MenuAction::None,
            },
            ServerListButton::Add => {
                self.form = EditForm::adding();
                ui.open_server_edit();
                MenuAction::None
            }
            ServerListButton::Edit => match self.list.get(self.server) {
                Some(entry) => {
                    self.form = EditForm::editing(self.server, entry);
                    ui.open_server_edit();
                    MenuAction::None
                }
                None => MenuAction::None,
            },
            ServerListButton::Delete => self.delete_selected(),
            ServerListButton::Refresh => MenuAction::RefreshList,
            ServerListButton::Back => {
                ui.on_escape();
                MenuAction::None
            }
            // Inactive, so `click_list` has already returned; spelled out rather
            // than `_` so making it active without giving it an action is a
            // compile error instead of a dead button.
            ServerListButton::Direct => MenuAction::None,
        }
    }

    /// Handles one key for the current screen, mutating `ui` for navigation and
    /// returning the action the app must perform.
    pub fn key(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match ui.screen() {
            Screen::MainMenu => self.key_main(ui, key),
            Screen::ServerList => self.key_list(ui, key),
            Screen::ServerEdit => self.key_edit(ui, key),
            Screen::WorldSelect => self.key_world_select(ui, key),
            // World Creation (issue #190) — same reasoning as
            // `Screen::WorldSelect`'s own arm above.
            Screen::CreateWorld => self.key_create_world(ui, key),
            // The confirmation screen (issue #540). Escape here is the *negative
            // answer* rather than a bare unwind, which is why it needs an arm of
            // its own and cannot fall through to `UiState::on_escape`.
            Screen::Confirm => self.key_confirm(ui, key),
            // The resource-pack prompt. Same reasoning as `Screen::Confirm`
            // immediately above — Escape is Decline, not a bare unwind.
            Screen::ResourcePackPrompt => self.key_resource_pack_prompt(ui, key),
            Screen::Settings => self.key_settings(ui, key),
            Screen::Accounts => self.key_accounts(ui, key),
            // Unlike the other arms above, the pause menu is not an
            // `owns_frame` screen — see `render::pause_frame`'s docs — but it
            // still owns its own row navigation exactly like they do.
            Screen::Paused => self.key_paused(ui, key),
            // Same reasoning as `Screen::Paused` — not `owns_frame`, still
            // owns its own row navigation. Its own arm rather than falling
            // through to the catch-all below (issue #103): that catch-all
            // routes Escape through `UiState::on_escape`, but the death
            // screen must swallow Escape entirely (vanilla's
            // `shouldCloseOnEsc() == false`), which `key_death` does by
            // simply never calling `on_escape`.
            Screen::Death => self.key_death(ui, key),
            // The error screen has exactly one affordance — go back — reachable
            // with Escape or by activating its single row.
            Screen::Error if matches!(key, MenuKey::Escape | MenuKey::Enter) => {
                ui.dismiss_error();
                MenuAction::None
            }
            // The credits/end-poem screen (#192) — also exactly one
            // affordance, its own arm for the same reason `Screen::Error`'s
            // is: routing through the catch-all below would call
            // `UiState::on_escape` on Escape, which is the wrong exit (this
            // screen leaves through `quit_to_title`, matching
            // `PauseButton::QuitToTitle`/`DeathButton::TitleScreen`, not
            // through the ordinary menu-stack unwind).
            Screen::Credits => self.key_credits(ui, key),
            // Social Interactions (#189) has a real cursor and a real
            // "back", unlike `Screen::Credits` — its own arm rather than the
            // catch-all below for the same reason `Screen::Settings`'s is:
            // that catch-all's Escape goes through `UiState::on_escape`,
            // which would work here too (its `Screen::Social` arm calls
            // `close_social`), but routing every key through `key_social`
            // keeps Up/Down/Enter and Escape's screen-specific meaning in one
            // place instead of splitting it across two functions.
            Screen::Social => self.key_social(ui, key),
            // Statistics (#188) — its own arm for the same reason
            // `Screen::Social`'s is: routing Escape through the catch-all's
            // `UiState::on_escape` would also work (its `Screen::Statistics`
            // arm calls `close_statistics`), but keeping Up/Down/Escape in
            // one function is the established pattern here.
            Screen::Statistics => self.key_statistics(ui, key),
            // Server Links — its own arm for the reason `Screen::Statistics`'s
            // above gives, sharpened: Escape here is two-step (back out of a
            // link's confirmation to the list, *then* close), which the
            // catch-all's single `UiState::on_escape` call cannot express —
            // see `key_server_links`.
            Screen::ServerLinks => self.key_server_links(ui, key),
            // The command block edit screen (issue #47) — its own arm for the
            // same reason `Screen::ServerEdit`'s is: a text field needs every
            // keystroke routed to it, which the catch-all below (Escape only)
            // cannot do.
            Screen::CommandBlockEdit => self.key_command_block(ui, key),
            // The sign-editing screen — its own arm for the same reason
            // `Screen::CommandBlockEdit`'s is: every keystroke must reach one
            // of its four line fields, which the catch-all below (Escape only)
            // cannot do.
            Screen::SignEdit => self.key_sign_edit(ui, key),
            // The book-editing screen — its own arm for the same reason
            // `Screen::SignEdit`'s is: every keystroke must reach the page or
            // the title field, which the catch-all below (Escape only)
            // cannot do.
            Screen::BookEdit => self.key_book_edit(ui, key),
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
                    // Vanilla's `TitleScreen` opens `SelectWorldScreen` here; it
                    // does not start a world (issue #397).
                    MainButton::Singleplayer => {
                        // `open_world_list`, not `ui.open_world_select()`: the save
                        // list has to be re-read here. See that method.
                        self.open_world_list(ui);
                        MenuAction::None
                    }
                    MainButton::Multiplayer => {
                        ui.open_server_list();
                        // Vanilla builds a fresh `JoinMultiplayerScreen`
                        // (`scrollAmount` starts at 0) every time this is
                        // pressed; `clamp_server` below then re-derives the
                        // window from wherever `self.server` already points,
                        // matching a fresh screen whose selection just happens
                        // to already be scrolled to (#402).
                        self.server_scroll = 0.0;
                        self.clamp_server();
                        MenuAction::Reprobe(None)
                    }
                    MainButton::Options => {
                        // Vanilla builds a **new** `OptionsScreen` every time
                        // (`TitleScreen.java`'s `setScreen(new OptionsScreen(…))`),
                        // so re-entering Options never resumes three pages deep.
                        // Opened from the title, so `inWorld` is false — the
                        // root's Online button is live (`SettingsPage::Online`),
                        // not the permanently-absent World Options fork.
                        self.settings.reset(false);
                        ui.open_settings();
                        MenuAction::None
                    }
                    MainButton::Quit => {
                        ui.request_quit();
                        MenuAction::Quit
                    }
                    MainButton::Accounts => {
                        ui.open_accounts();
                        MenuAction::None
                    }
                    // Vanilla constructs `LanguageSelectScreen`/
                    // `AccessibilityOptionsScreen` directly from the title
                    // (`TitleScreen.java`), with `lastScreen = this`
                    // — never through `OptionsScreen`. `open_at` lands on the
                    // page with an empty stack so Escape/Done leaves straight
                    // back to the title (one Escape, not two through the root
                    // grid) — see `SettingsNav::open_at`'s own doc.
                    MainButton::Language => {
                        self.settings.open_at(false, crate::menu::options::SettingsPage::Language);
                        ui.open_settings();
                        MenuAction::None
                    }
                    MainButton::Accessibility => {
                        self.settings.open_at(false, crate::menu::options::SettingsPage::Accessibility);
                        ui.open_settings();
                        MenuAction::None
                    }
                    // Unreachable — every variant below is disabled above.
                    // Spelled out instead of `_` so making one of them *enabled*
                    // without giving it an action is a compile-visible mistake
                    // rather than a silently dead button.
                    MainButton::Realms | MainButton::Friends => MenuAction::None,
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
                // #402: without this, arrowing past the bottom of the visible
                // window moved the selection but drew nothing new — the
                // outline vanished off-screen with no sign anything happened.
                self.scroll_server_to_show();
                MenuAction::None
            }
            MenuKey::Down => {
                self.server = wrap_next(self.server, self.list.len());
                self.scroll_server_to_show();
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
            // F5, `JoinMultiplayerScreen.keyPressed`'s `event.key() == 294`
            // (`:231-239`). Every row, not the selected one: vanilla's refresh
            // replaces the whole screen.
            MenuKey::Refresh => MenuAction::RefreshList,
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

    /// The add/edit form. **Every key goes through [`EditForm::handle_key`]**,
    /// which is vanilla's `Screen.keyPressed` order — so this arm only decides
    /// what "cancel" and "save" *mean*, not which key is which.
    ///
    /// That replaced a flat `match key`, and the difference is worth naming: the
    /// old arm mapped `Tab | Up | Down` to "toggle field" and swallowed
    /// `Delete`. Now the focused [`EditBox`] is offered the key first, so
    /// Backspace/Delete edit at the caret, Tab and the vertical arrows fall
    /// through to real focus traversal, and the horizontal arrows would move the
    /// caret if `app.rs` produced them (it does not yet — see
    /// [`focus::KeyEvent::from_menu_key`]).
    fn key_edit(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match self.form.handle_key(key) {
            FormOutcome::Handled => MenuAction::None,
            FormOutcome::Cancel => self.cancel_edit(ui),
            FormOutcome::Save => self.save_entry(ui),
        }
    }

    /// Discards the form; the list is untouched. Vanilla's `CommonComponents.GUI_CANCEL`
    /// (`ManageServerScreen.java`) and Escape's own meaning on this
    /// screen ([`FormOutcome::Cancel`]) — shared by [`key_edit`](Self::key_edit)
    /// and [`Self::click`]'s [`CANCEL_ROW`] arm so the button and the key do
    /// the exact same thing rather than two copies that could drift apart.
    fn cancel_edit(&mut self, ui: &mut UiState) -> MenuAction {
        ui.close_server_edit();
        MenuAction::None
    }

    /// Validates and saves the form, exactly as `Enter` does
    /// ([`FormOutcome::Save`]) — shared with [`Self::click`]'s [`DONE_ROW`]
    /// arm (vanilla's `CommonComponents.GUI_DONE`,
    /// `ManageServerScreen.java`) for the same reason
    /// [`Self::cancel_edit`] is shared.
    fn save_entry(&mut self, ui: &mut UiState) -> MenuAction {
        if !self.form.is_valid() {
            // Refuse rather than saving a row that cannot be dialed. Vanilla
            // reaches the same outcome by disabling the Done button instead
            // (`ManageServerScreen.java`); this screen has no per-row
            // `active` flag to disable it with, so refusing on activation is
            // the equivalent it can express.
            return MenuAction::None;
        }
        let entry = self.form.to_entry();
        let previous = self.form.editing.and_then(|i| self.list.get(i)).cloned();
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
        if let Some(old) = previous.filter(|p| p.host != entry.host || p.port != entry.port) {
            return MenuAction::Forget(old);
        }
        MenuAction::Reprobe(Some(entry))
    }

    /// The command block edit screen (issue #47).
    ///
    /// Unlike [`Self::key_edit`], this does **not** route every key through a
    /// shared `handle_key` first: the screen has exactly one keyboard focus
    /// target (see [`command_block::CommandBlockState`]'s own doc on why
    /// "Previous Output" is not a second one), so there is no focus layer to
    /// arbitrate — every key already knows where it goes. Left/Right/Home/End
    /// are not handled for the same reason [`Self::key_edit`]'s doc already
    /// names: `app.rs` does not produce them from `MenuKey` yet.
    fn key_command_block(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let Some(state) = self.command_block.as_mut() else {
            return MenuAction::None;
        };
        match key {
            // Vanilla's `AbstractCommandBlockEditScreen.keyPressed`: Escape
            // is `Screen`'s own `shouldCloseOnEsc()` path (`:129`, unguarded —
            // unlike `Screen::Death`'s override), which is Cancel here.
            MenuKey::Escape => {
                self.close_command_block(ui);
                MenuAction::None
            }
            // `event.isConfirmation() -> this.onDone()` (`:134-136`), reached
            // only when the (currently always-empty, see the module doc)
            // suggestion list did not consume Enter first.
            MenuKey::Enter => self.activate_command_block_row(
                ui,
                command_block::CommandBlockRow::Done as usize,
            ),
            MenuKey::Char(ch) => {
                state.handle_char(ch);
                MenuAction::None
            }
            MenuKey::Backspace => {
                state.handle_key(KeyEvent::new(focus::KEY_BACKSPACE));
                MenuAction::None
            }
            MenuKey::Delete => {
                state.handle_key(KeyEvent::new(focus::KEY_DELETE));
                MenuAction::None
            }
            // Vanilla cycles the suggestion list with Tab/Up/Down
            // (`CommandSuggestions.SuggestionsList.keyPressed`). This comment
            // used to end "With no command tree ever reaching this client yet
            // … there is nothing to cycle" — true when written, stale since
            // #470/#471: the tree the server sent is in `self.command_tree`,
            // so Tab now completes against it (see
            // `CommandBlockState::apply_completion`, including why it commits
            // rather than cycles). Up/Down stay no-ops: they move a popup
            // *selection* that is not modelled.
            MenuKey::Tab => {
                // `self.command_tree` is a disjoint field from the
                // `self.command_block` `state` above, so this reads the tree
                // without a second `&mut self`.
                state.apply_completion(self.command_tree.as_deref());
                MenuAction::None
            }
            // Select-all/copy/cut/paste on the command field — `from_menu_key`
            // is the one place that knows the GLFW key + modifier each of
            // these stands for, so this forwards rather than re-deriving it.
            MenuKey::SelectAll | MenuKey::Copy | MenuKey::Cut | MenuKey::Paste => {
                if let Some(event) = KeyEvent::from_menu_key(key) {
                    state.handle_key(event);
                }
                MenuAction::None
            }
            MenuKey::Up | MenuKey::Down | MenuKey::Refresh => MenuAction::None,
        }
    }

    /// What one [`command_block::CommandBlockRow`] does when clicked or
    /// activated by Enter — shared by [`Self::click`]'s `CommandBlockEdit`
    /// arm and [`Self::key_command_block`]'s `Enter` arm, matching
    /// [`Self::save_entry`]/[`Self::cancel_edit`]'s "button and key do the
    /// same thing" rule.
    fn activate_command_block_row(&mut self, ui: &mut UiState, row: usize) -> MenuAction {
        use command_block::CommandBlockRow;
        let Some(cb_row) = command_block::COMMAND_BLOCK_ROWS.get(row).copied() else {
            // `PREVIOUS_OUTPUT_ROW` and anything past it: not a control, see
            // `command_block`'s module doc on why "Previous Output" is
            // display-only.
            return MenuAction::None;
        };
        let Some(state) = self.command_block.as_mut() else {
            return MenuAction::None;
        };
        match cb_row {
            // The command field itself is not a button; a click on it is
            // caret placement, which — like `Screen::ServerEdit`'s own two
            // fields — is `app.rs`'s to translate from a physical click
            // position via `EditBox::click_at`, not something `Enter`/this
            // method can express as "activation".
            CommandBlockRow::Command => MenuAction::None,
            CommandBlockRow::TrackOutput => {
                state.toggle_track_output();
                MenuAction::None
            }
            CommandBlockRow::Mode => {
                state.cycle_mode();
                MenuAction::None
            }
            CommandBlockRow::Conditional => {
                state.toggle_conditional();
                MenuAction::None
            }
            CommandBlockRow::Automatic => {
                state.toggle_automatic();
                MenuAction::None
            }
            CommandBlockRow::Done => {
                // `populateAndSendPacket(); this.onClose();`
                // (`CommandBlockEditScreen.java`) — vanilla sends first
                // and closes second, and the order matters here for the same
                // reason: `close_command_block` drops `self.command_block`, so
                // the payload has to be taken off `state` before it goes.
                let submit = state.to_submit();
                self.close_command_block(ui);
                MenuAction::SetCommandBlock(submit)
            }
            CommandBlockRow::Cancel => {
                self.close_command_block(ui);
                MenuAction::None
            }
        }
    }

    /// The sign-editing screen.
    ///
    /// Unlike [`Self::key_command_block`], Up/Down are **not** no-ops here —
    /// vanilla's `AbstractSignEditScreen.keyPressed` uses them (plus Enter) to
    /// switch which line is focused, and that is this screen's only keyboard
    /// navigation: there is no suggestion popup, no toggle row, nothing Tab
    /// would do. See [`sign_edit::SignEditState::next_line`]/[`previous_line`
    /// ](sign_edit::SignEditState::previous_line).
    fn key_sign_edit(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let Some(state) = self.sign_edit.as_mut() else {
            return MenuAction::None;
        };
        match key {
            // `onClose()` → `onDone()` → `removed()`, which sends
            // unconditionally (`AbstractSignEditScreen.java`) — see the module
            // doc on why this screen has no Cancel that skips the send.
            MenuKey::Escape => {
                let submit = state.to_submit();
                self.close_sign_edit(ui);
                MenuAction::SignUpdate(submit)
            }
            // `event.isUp()`: the *previous* line, cursor parked at its end.
            MenuKey::Up => {
                state.previous_line();
                MenuAction::None
            }
            // `event.isDown() || event.isConfirmation()`: Enter behaves exactly
            // like Down here — it does **not** activate Done. Only a real click
            // on the Done row (or Escape) closes this screen.
            MenuKey::Down | MenuKey::Enter => {
                state.next_line();
                MenuAction::None
            }
            MenuKey::Char(ch) => {
                state.handle_char(ch);
                MenuAction::None
            }
            MenuKey::Backspace => {
                state.handle_key(KeyEvent::new(focus::KEY_BACKSPACE));
                MenuAction::None
            }
            MenuKey::Delete => {
                state.handle_key(KeyEvent::new(focus::KEY_DELETE));
                MenuAction::None
            }
            MenuKey::SelectAll | MenuKey::Copy | MenuKey::Cut | MenuKey::Paste => {
                if let Some(event) = KeyEvent::from_menu_key(key) {
                    state.handle_key(event);
                }
                MenuAction::None
            }
            MenuKey::Tab | MenuKey::Refresh => MenuAction::None,
        }
    }

    /// What clicking the sign-editing screen's Done row does — the only
    /// activation this screen has (a click on a line field is caret placement,
    /// like [`CommandBlockRow::Command`] above, not something this method
    /// expresses). Shared by [`Self::click`]'s `SignEdit` arm.
    fn activate_sign_edit_row(&mut self, ui: &mut UiState, row: usize) -> MenuAction {
        if row != sign_edit_row::DONE {
            return MenuAction::None;
        }
        let Some(state) = self.sign_edit.as_mut() else {
            return MenuAction::None;
        };
        let submit = state.to_submit();
        self.close_sign_edit(ui);
        MenuAction::SignUpdate(submit)
    }

    /// The book-editing screen. Up/Down move the page's caret between visual
    /// lines (`TextArea::seek_cursor_line`) — this screen's only keyboard
    /// navigation, the same reduced shape `key_sign_edit`'s own doc names:
    /// no Tab traversal is needed because neither layout has more than one
    /// focusable field (see [`book_edit::BookEditState::handle_key`]'s own
    /// doc). Enter inserts a newline in the page rather than acting as
    /// "Done" — vanilla's `MultilineTextField.keyPressed`'s `case 257` — and
    /// does nothing while signing (there is no multi-line field there to
    /// insert into; Finalize is a click-only affordance, like `SignEdit`'s
    /// own Done).
    fn key_book_edit(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let Some(state) = self.book_edit.as_mut() else {
            return MenuAction::None;
        };
        match key {
            // Escape discards unconditionally, from either layout — see
            // `Screen::BookEdit`'s own doc on why this differs from
            // `key_sign_edit`'s Escape arm.
            MenuKey::Escape => {
                self.close_book_edit(ui);
                MenuAction::None
            }
            MenuKey::Up if !state.signing => {
                state.page.seek_cursor_line(-1, false);
                MenuAction::None
            }
            MenuKey::Down if !state.signing => {
                state.page.seek_cursor_line(1, false);
                MenuAction::None
            }
            MenuKey::Enter if !state.signing => {
                state.page.handle_key(KeyEvent::new(focus::KEY_ENTER));
                MenuAction::None
            }
            MenuKey::Char(ch) => {
                state.handle_char(ch);
                MenuAction::None
            }
            MenuKey::Backspace => {
                state.handle_key(KeyEvent::new(focus::KEY_BACKSPACE));
                MenuAction::None
            }
            MenuKey::Delete => {
                state.handle_key(KeyEvent::new(focus::KEY_DELETE));
                MenuAction::None
            }
            MenuKey::SelectAll | MenuKey::Copy | MenuKey::Cut | MenuKey::Paste => {
                if let Some(event) = KeyEvent::from_menu_key(key) {
                    state.handle_key(event);
                }
                MenuAction::None
            }
            MenuKey::Up | MenuKey::Down | MenuKey::Enter | MenuKey::Tab | MenuKey::Refresh => {
                MenuAction::None
            }
        }
    }

    /// What clicking a row on the book-editing screen does — dispatched by
    /// [`book_edit::BookEditState::signing`], since the two layouts use
    /// disjoint row tables (`page_row`/`sign_row`). Shared by [`Self::click`]'s
    /// `BookEdit` arm.
    fn activate_book_edit_row(&mut self, ui: &mut UiState, row: usize) -> MenuAction {
        let Some(state) = self.book_edit.as_mut() else {
            return MenuAction::None;
        };
        if state.signing {
            match row {
                // The title field: caret placement, not something this method
                // expresses — see the module doc's "What is deliberately
                // simplified".
                book_edit::sign_row::TITLE => MenuAction::None,
                book_edit::sign_row::FINALIZE => {
                    if !state.can_finalize() {
                        return MenuAction::None;
                    }
                    let action = state.to_sign_action();
                    self.close_book_edit(ui);
                    MenuAction::EditBook(action)
                }
                book_edit::sign_row::CANCEL => {
                    state.cancel_sign();
                    MenuAction::None
                }
                _ => MenuAction::None,
            }
        } else {
            match row {
                book_edit::page_row::BACK => {
                    state.page_back();
                    MenuAction::None
                }
                book_edit::page_row::FORWARD => {
                    state.page_forward();
                    MenuAction::None
                }
                book_edit::page_row::SIGN => {
                    state.begin_sign();
                    MenuAction::None
                }
                book_edit::page_row::DONE => {
                    let action = state.to_save_action();
                    self.close_book_edit(ui);
                    MenuAction::EditBook(action)
                }
                _ => MenuAction::None,
            }
        }
    }

    /// The world-select screen (issue #397). **Every key goes through
    /// [`super::world_select::WorldSelectNav::handle_key`]**, which is vanilla's
    /// `Screen.keyPressed` order, so this arm only decides what "close" means.
    ///
    /// Same shape as [`key_edit`](Self::key_edit) and for the same reason: the
    /// screen holds real focus over a text field and six buttons, so the order —
    /// Escape, then the focused widget, then Tab and the arrows as navigation —
    /// is what makes the search box coexist with keyboard traversal rather than
    /// fight it.
    fn key_world_select(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let outcome = self.world_select.handle_key(key);
        self.apply_world_select(ui, outcome)
    }

    /// The one thing a [`super::world_select::WorldSelectOutcome`] can ask of
    /// the screen. Used to be an associated function that touched no
    /// `MenuNav` state; issue #190's `CreateWorld` arm needs to reset
    /// [`Self::create_world`] on entry (the same "fresh screen, not a
    /// resumed one" rule every other `open_*`/`reset` pair in this file
    /// follows), so it is a method now.
    fn apply_world_select(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::world_select::WorldSelectOutcome,
    ) -> MenuAction {
        use crate::menu::world_select::WorldSelectOutcome;
        match outcome {
            WorldSelectOutcome::Handled => MenuAction::None,
            // Vanilla's `onClose()`/Back: `setScreen(this.lastScreen)`, the title.
            WorldSelectOutcome::Close => {
                ui.close_world_select();
                MenuAction::None
            }
            // Vanilla's `loadSelectedWorld()`. The screen is left *by the app*,
            // not here: `begin_singleplayer` calls `ui.begin(Singleplayer)`,
            // which moves to `Screen::Connecting` — and it must stay on the world
            // list until then, because a launch that fails (no version family
            // compiled in) has to be able to show its error over a screen the
            // player recognises rather than over a blank one.
            //
            // The folder name is resolved against **this** `MenuNav`'s saves root
            // through [`crate::saves::world_dir_in`], which is also the containment
            // check: a `dir_name` that is not one plain path component answers
            // `None` and the press does nothing rather than opening the saves root
            // itself as a world.
            WorldSelectOutcome::Play(dir_name) => {
                match crate::saves::world_dir_in(&self.saves_root, &dir_name) {
                    Some(dir) => MenuAction::Singleplayer(SingleplayerLaunch::Open(dir)),
                    None => {
                        self.world_select
                            .set_error(format!("{dir_name:?} is not a world folder"));
                        MenuAction::None
                    }
                }
            }
            // Issue #190.
            WorldSelectOutcome::CreateWorld => {
                self.create_world = crate::menu::create_world::CreateWorldNav::new();
                ui.open_create_world();
                MenuAction::None
            }
            // Issue #540. **Nothing is deleted here.** This arm only opens the
            // confirmation, carrying the folder the player had selected — the
            // removal happens in [`Self::apply_confirm`]'s `Yes` arm and nowhere
            // else, which is the property that makes the Delete button safe to
            // press. The whole `ConfirmNav` is rebuilt rather than reused, so a
            // previous confirmation's focus and target cannot leak in.
            WorldSelectOutcome::DeleteWorld {
                dir_name,
                display_name,
            } => {
                self.confirm =
                    crate::menu::confirm::ConfirmNav::delete_world(&dir_name, &display_name);
                ui.open_confirm();
                MenuAction::None
            }
        }
    }

    /// The confirmation screen (issue #540). Every key goes through
    /// [`crate::menu::confirm::ConfirmNav::handle_key`], which is vanilla's
    /// `ConfirmScreen.keyPressed` order — including its Escape branch, which is
    /// `callback.accept(false)` rather than `onClose`.
    fn key_confirm(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let outcome = self.confirm.handle_key(key);
        self.apply_confirm(ui, outcome)
    }

    /// What an answer to the confirmation means.
    ///
    /// **This is the only place in the shell that deletes a world**, and it is
    /// here for `apply_create_world`'s reason: this layer knows the saves root, so
    /// the containment check and the removal happen where the root is rather than
    /// somewhere a folder name has been carried to.
    ///
    /// Both answers `close_confirm` **and re-read the list** —
    /// `WorldSelectionList.deleteWorld`'s callback calls `returnToScreen()`
    /// outside its own `if (result)` (`WorldSelectionList.java`) — so the
    /// screen the player lands on always reflects the disk rather than what was
    /// enumerated before the confirmation opened. That matters even for a cancel:
    /// another process may have removed the world in the meantime.
    ///
    /// The `match` on [`crate::menu::confirm::ConfirmRequest`] is exhaustive so a
    /// second kind of confirmation cannot be opened and then silently do nothing
    /// when the player says yes — the island shape, in the one place where the
    /// island would be a *missing destructive action* rather than an unused one.
    fn apply_confirm(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::confirm::ConfirmOutcome,
    ) -> MenuAction {
        use crate::menu::confirm::{ConfirmOutcome, ConfirmRequest};
        match outcome {
            ConfirmOutcome::Handled => MenuAction::None,
            ConfirmOutcome::No => {
                ui.close_confirm();
                self.open_world_list(ui);
                MenuAction::None
            }
            ConfirmOutcome::Yes => {
                // Cloned out of the request before anything moves the screen: the
                // list rebuild below replaces `world_select`, and reading the
                // target after that would be reading it from a screen that has
                // already forgotten which row was selected.
                let ConfirmRequest::DeleteWorld { dir_name, .. } = self.confirm.request().clone();
                let result = crate::saves::delete_world_in(&self.saves_root, &dir_name);
                ui.close_confirm();
                self.open_world_list(ui);
                // Reported over a screen the player recognises rather than
                // swallowed — vanilla logs it and raises `SystemToast
                // .onWorldDeleteFailure` (`WorldSelectionList.java`), and
                // this shell has no toast layer, so the world list's own error
                // line is where it goes (the same place a failed create goes).
                if let Err(e) = result {
                    self.world_select
                        .set_error(format!("Could not delete the world: {e}"));
                }
                MenuAction::None
            }
        }
    }

    /// The resource-pack prompt. Every key goes through
    /// [`crate::menu::confirm::ResourcePackPromptNav::handle_key`], which
    /// follows [`crate::menu::confirm::ConfirmNav::handle_key`]'s own order —
    /// including its Escape branch, which answers Decline rather than a bare
    /// close.
    fn key_resource_pack_prompt(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let Some(prompt) = &mut self.resource_pack_prompt else {
            return MenuAction::None;
        };
        let outcome = prompt.handle_key(key);
        self.apply_resource_pack_prompt(ui, outcome)
    }

    /// What an answer to the resource-pack prompt means: close the overlay
    /// (back to whatever live screen it opened over) and hand the app the
    /// [`MenuAction::ResourcePackResponse`] to submit — `MenuNav` holds no
    /// `Sim`/`NetClient` to send it through itself, [`MenuAction::Respawn`]'s
    /// own division of labour.
    ///
    /// `self.resource_pack_prompt` is taken (`Option::take`), not merely
    /// read, so a second answer to an already-closed prompt (a double-click
    /// racing the overlay's own close) cannot resubmit it — the same
    /// "already handled" shape [`Screen::Death`]'s respawn button relies on
    /// `Sim::respawn`'s own idempotence for, done here at the source instead.
    fn apply_resource_pack_prompt(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::confirm::ResourcePackPromptOutcome,
    ) -> MenuAction {
        use crate::menu::confirm::ResourcePackPromptOutcome;
        match outcome {
            ResourcePackPromptOutcome::Handled => MenuAction::None,
            ResourcePackPromptOutcome::Accept | ResourcePackPromptOutcome::Decline => {
                let Some(prompt) = self.resource_pack_prompt.take() else {
                    return MenuAction::None;
                };
                ui.close_resource_pack_prompt();
                // Record *before* returning: `app/session.rs`'s reconcile can
                // run again as soon as this frame (see
                // `Self::resource_pack_answered_id`'s own doc for why the
                // shared cell this compares against is not cleared yet), so
                // the flag has to be set the instant we decide to close, not
                // after the caller gets around to sending the response.
                self.resource_pack_answered_id = Some(prompt.id());
                MenuAction::ResourcePackResponse {
                    id: prompt.id(),
                    accept: outcome == ResourcePackPromptOutcome::Accept,
                }
            }
        }
    }

    /// Whether `id` is the resource-pack prompt this side already answered —
    /// see [`Self::resource_pack_answered_id`]'s own doc. `app/session.rs`'s
    /// reconcile is the one caller.
    #[must_use]
    pub fn resource_pack_already_answered(&self, id: uuid::Uuid) -> bool {
        self.resource_pack_answered_id == Some(id)
    }

    /// Forgets the last-answered id once the ground truth
    /// (`NetClient::pending_resource_pack_prompt`) catches up to `None` — see
    /// [`Self::resource_pack_answered_id`]'s own doc. Idempotent, so calling
    /// it every frame the ground truth is empty (as `app/session.rs` does) is
    /// cheap and safe.
    pub fn clear_resource_pack_answered(&mut self) {
        self.resource_pack_answered_id = None;
    }

    /// The World Creation screen (issue #190). Every key is routed through
    /// [`crate::menu::create_world::CreateWorldNav::handle_key`], which
    /// already implements vanilla's `Screen.keyPressed` order (Escape, then
    /// the focused field, then Tab/arrow navigation, then Enter on whatever
    /// is focused) — this arm only decides what leaving the screen means.
    fn key_create_world(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let outcome = self.create_world.handle_key(key);
        self.apply_create_world(ui, outcome)
    }

    /// What a [`crate::menu::create_world::CreateWorldOutcome`] means at the
    /// `UiState` level.
    ///
    /// `Create` (issue #190's queued patch): the screen is left *by the app*,
    /// not here — mirroring [`Self::apply_world_select`]'s `Play` arm above,
    /// for the identical reason: `begin_singleplayer` must stay able to show
    /// a launch failure over a screen the player recognises rather than over
    /// a screen that has already navigated away.
    fn apply_create_world(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::create_world::CreateWorldOutcome,
    ) -> MenuAction {
        use crate::menu::create_world::CreateWorldOutcome;
        match outcome {
            CreateWorldOutcome::Handled => MenuAction::None,
            // Back to the world list, **re-read**: the player may have cancelled
            // after a create that failed, and the list they return to must be
            // what is on disk rather than what was there when they left.
            CreateWorldOutcome::Cancel => {
                ui.close_create_world();
                self.open_world_list(ui);
                MenuAction::None
            }
            // **This is where a world is actually created** (issue #468's reading
            // 2), and it is here rather than in `app.rs` because this is the layer
            // that knows the saves root — the same reason `ServerList::save_to` is
            // called from this file.
            //
            // `game_type` is the one `WorldCreationConfig` field that reaches disk:
            // it lands in `level.dat`'s `GameType`, so the list row says Creative
            // for a creative world. Hardcore maps to survival's `0` because
            // `LevelDat::for_new_world` writes `hardcore: 0` and this layer has no
            // business hand-editing that compound — so a Hardcore world is created
            // as Survival, which is the same gap `create_world.rs`'s own
            // "decorative" list already records for difficulty, structures, bonus
            // chest and cheats.
            CreateWorldOutcome::Create(config) => {
                let game_type = match config.game_mode {
                    crate::menu::create_world::WorldGameMode::Creative => 1,
                    crate::menu::create_world::WorldGameMode::Survival
                    | crate::menu::create_world::WorldGameMode::Hardcore => 0,
                };
                // Browser: no directory, no `level.dat`, straight to the launch.
                //
                // `saves::create_world_in` deliberately *refuses* on wasm32 — a page
                // has no `saves/` to write into — so routing through it made Create
                // New World a button that did nothing: it returned to the world list
                // with "Could not create the world", which was correct and useless.
                // A browser world is real, it is simply **in memory**:
                // `IntegratedServer::open_in_memory` needs no directory and no
                // `level.dat`, and everything downstream of here already handled its
                // absence under the same `cfg`.
                //
                // The typed **name** is the one thing lost with the directory — it
                // normally lands in `level.dat` — and nothing in a browser session
                // reads it back, because the world it would label cannot be re-opened.
                // The seed still travels, in `config`, and is still honoured: a fresh
                // in-memory world has no stored settings to override it, which is the
                // same reason the native `Created` arm honours it.
                #[cfg(target_arch = "wasm32")]
                {
                    let _ = game_type;
                    tracing::info!(
                        target: "saves",
                        name = %config.name,
                        "creating an in-memory browser world (nothing is written to disk; \
                         it is lost when the tab closes)"
                    );
                    return MenuAction::Singleplayer(SingleplayerLaunch::Created { config });
                }
                #[cfg(not(target_arch = "wasm32"))]
                match crate::saves::create_world_in(&self.saves_root, &config.name, game_type) {
                    Ok(world_dir) => {
                        MenuAction::Singleplayer(SingleplayerLaunch::Created { world_dir, config })
                    }
                    // Reported over a screen the player recognises, never routed
                    // around: a failed `create_dir` means the data directory is
                    // unwritable, and silently opening *some other* world would be
                    // the worst possible answer.
                    Err(e) => {
                        ui.close_create_world();
                        self.open_world_list(ui);
                        self.world_select
                            .set_error(format!("Could not create the world: {e}"));
                        MenuAction::None
                    }
                }
            }
        }
    }

    /// The settings tree (issue #55). Up/Down move the cursor, Enter activates
    /// what it is on, Escape unwinds one page.
    ///
    /// **This is the re-pointing the previous version of this comment predicted.**
    /// It used to say: the screen has no row highlight, each control owns its own
    /// key (Up/Down stepped the GUI scale, Enter toggled View Bobbing), and "when
    /// a third control lands, that is the point to introduce a real highlight and
    /// vanilla's own `OptionsScreen` list and re-point those tests once, on
    /// purpose." A hundred and thirty-third control landed; this is that.
    ///
    /// So Up/Down no longer change a value — they move a cursor, like every other
    /// screen in this shell — and the GUI scale is cycled by pressing Enter on
    /// **its own row**, which is vanilla's `CycleButton.onPress`. The tests that
    /// asserted the old binding were rewritten rather than deleted: the behaviour
    /// they protected (a scale that cycles and reaches `options.json`) is still
    /// asserted, through the new path.
    fn key_settings(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        // Key Binds (issue #15) has its own cursor and its own outcome type —
        // see `hover`'s matching guard for why this is a separate arm rather
        // than a branch inside the match below.
        if self.settings.page() == crate::menu::options::SettingsPage::KeyBinds {
            return self.key_key_binds(ui, key);
        }
        // Language (issue #415) — same reasoning.
        if self.settings.page() == crate::menu::options::SettingsPage::Language {
            return self.key_language(ui, key);
        }
        // Telemetry (issue #415) — same reasoning.
        if self.settings.page() == crate::menu::options::SettingsPage::Telemetry {
            return self.key_telemetry(ui, key);
        }
        // Resource Packs (issue #415) — same reasoning.
        if self.settings.page() == crate::menu::options::SettingsPage::ResourcePacks {
            return self.key_packs(ui, key);
        }
        let outcome = match key {
            MenuKey::Up => {
                self.settings.step(false);
                return MenuAction::None;
            }
            MenuKey::Down => {
                self.settings.step(true);
                return MenuAction::None;
            }
            MenuKey::Enter => self.settings.enter(),
            MenuKey::Escape => self.settings.escape(),
            _ => return MenuAction::None,
        };
        self.apply_settings(ui, outcome)
    }

    /// [`Self::key_settings`]'s Key Binds half.
    fn key_key_binds(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let outcome = match key {
            MenuKey::Up => {
                self.settings.key_binds_mut().step(false);
                return MenuAction::None;
            }
            MenuKey::Down => {
                self.settings.key_binds_mut().step(true);
                return MenuAction::None;
            }
            MenuKey::Enter => self
                .settings
                .key_binds_mut()
                .enter(&self.options.keybinds),
            MenuKey::Escape => self.settings.key_binds_mut().escape(),
            _ => return MenuAction::None,
        };
        self.apply_key_binds(ui, outcome)
    }

    /// What a [`super::options::key_binds::KeyBindsOutcome`] asks of the shell.
    /// Mirrors [`Self::apply_settings`]'s reason for living here: this owns
    /// [`Options`] and the file it persists to, and a rebind or a reset that
    /// only saved on exit would be the one a crash loses — the same eager-
    /// persistence rule every other live row in this tree already follows.
    ///
    /// Takes `ui` even though most arms do not touch it, rather than
    /// fabricating a throwaway [`UiState`] for the one arm
    /// ([`KeyBindsOutcome::Back`]) that can: `SettingsNav::leave_key_binds`
    /// asks to close the whole tree if its page stack is ever unexpectedly
    /// empty, and that has to reach the *real* `ui.close_settings()` or the
    /// fallback would silently do nothing to a state nobody can see.
    fn apply_key_binds(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::key_binds::KeyBindsOutcome,
    ) -> MenuAction {
        use crate::menu::key_binds::KeyBindsOutcome;
        match outcome {
            KeyBindsOutcome::None => MenuAction::None,
            // Back to Controls — `leave_key_binds` pops `SettingsNav`'s own
            // page stack (always back to Controls in practice; see its doc)
            // and its `SettingsOutcome` is routed through `apply_settings`
            // rather than discarded, for the reason this method's own doc
            // gives.
            KeyBindsOutcome::Back => {
                let outcome = self.settings.leave_key_binds();
                self.apply_settings(ui, outcome)
            }
            KeyBindsOutcome::ResetOne(action) => {
                self.options.keybinds.reset(action);
                self.persist_options();
                MenuAction::None
            }
            KeyBindsOutcome::ResetAll => {
                self.options.keybinds.reset_all();
                self.persist_options();
                MenuAction::None
            }
        }
    }

    /// [`Self::key_settings`]'s Language half (issue #415). Up/Down/Enter
    /// move the list+footer cursor; typed characters always go to the search
    /// box regardless of where that cursor is — see
    /// [`crate::menu::language::LanguageNav`]'s module doc on why the two are
    /// independent.
    fn key_language(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let outcome = match key {
            MenuKey::Up => {
                self.settings.language_mut().step(false);
                return MenuAction::None;
            }
            MenuKey::Down => {
                self.settings.language_mut().step(true);
                return MenuAction::None;
            }
            MenuKey::Enter => self.settings.language_mut().enter(),
            MenuKey::Escape => self.settings.language_mut().escape(),
            // The search box is always the keyboard's text target on this
            // page (see `LanguageNav`'s doc) — routed here rather than
            // falling into the catch-all below, which is exactly the island
            // this would otherwise be: `LanguageNav::type_char`/`backspace`
            // would compile, be unit-tested, and never run.
            MenuKey::Char(ch) => {
                self.settings.language_mut().type_char(ch);
                return MenuAction::None;
            }
            MenuKey::Backspace => {
                self.settings.language_mut().backspace();
                return MenuAction::None;
            }
            _ => return MenuAction::None,
        };
        self.apply_language(ui, outcome)
    }

    /// What a [`crate::menu::language::LanguageOutcome`] asks of the shell —
    /// mirrors [`Self::apply_key_binds`]'s reason for living here (it can
    /// reach the real `ui.close_settings()` fallback [`SettingsNav::
    /// leave_language`]'s doc names, not a throwaway one).
    fn apply_language(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::language::LanguageOutcome,
    ) -> MenuAction {
        use crate::menu::language::LanguageOutcome;
        match outcome {
            LanguageOutcome::None => MenuAction::None,
            LanguageOutcome::Back => {
                let outcome = self.settings.leave_language();
                self.apply_settings(ui, outcome)
            }
        }
    }

    /// [`Self::key_settings`]'s Telemetry half (issue #415). Up/Down/Enter/
    /// Escape only — no text field on this page, unlike Language.
    fn key_telemetry(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let outcome = match key {
            MenuKey::Up => {
                self.settings.telemetry_mut().step(false);
                return MenuAction::None;
            }
            MenuKey::Down => {
                self.settings.telemetry_mut().step(true);
                return MenuAction::None;
            }
            MenuKey::Enter => self.settings.telemetry_mut().enter(),
            MenuKey::Escape => self.settings.telemetry_mut().escape(),
            _ => return MenuAction::None,
        };
        self.apply_telemetry(ui, outcome)
    }

    /// What a [`crate::menu::telemetry::TelemetryOutcome`] asks of the shell
    /// — mirrors [`Self::apply_language`]. Opening a URL is not one of
    /// these outcomes: `TelemetryNav::activate` performs it directly (see
    /// that module's own doc), so the only thing this ever asks for is
    /// leaving the page.
    fn apply_telemetry(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::telemetry::TelemetryOutcome,
    ) -> MenuAction {
        use crate::menu::telemetry::TelemetryOutcome;
        match outcome {
            TelemetryOutcome::None => MenuAction::None,
            TelemetryOutcome::Back => {
                let outcome = self.settings.leave_telemetry();
                self.apply_settings(ui, outcome)
            }
        }
    }

    /// [`Self::key_settings`]'s Resource Packs half (issue #415). Up/Down/
    /// Enter/Escape only — no text field, same shape as
    /// [`Self::key_telemetry`].
    fn key_packs(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        let outcome = match key {
            MenuKey::Up => {
                self.settings.packs_mut().step(false);
                return MenuAction::None;
            }
            MenuKey::Down => {
                self.settings.packs_mut().step(true);
                return MenuAction::None;
            }
            MenuKey::Enter => self.settings.packs_mut().enter(),
            MenuKey::Escape => self.settings.packs_mut().escape(),
            _ => return MenuAction::None,
        };
        self.apply_packs(ui, outcome)
    }

    /// What a [`crate::menu::packs::PacksOutcome`] asks of the shell —
    /// mirrors [`Self::apply_telemetry`].
    fn apply_packs(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::packs::PacksOutcome,
    ) -> MenuAction {
        use crate::menu::packs::PacksOutcome;
        match outcome {
            PacksOutcome::None => MenuAction::None,
            PacksOutcome::Back => {
                // **This is the call that makes the screen do anything** (issue
                // #415): it installs the column's order into
                // `resources::selected_packs` and persists it. It has to happen
                // *before* `leave_packs`, which resets the nav — vanilla commits
                // in `PackSelectionScreen.onClose` for the same reason, and
                // Escape comes through here too, so leaving is never a cancel.
                crate::menu::packs::commit(self.settings.packs());
                let outcome = self.settings.leave_packs();
                self.apply_settings(ui, outcome)
            }
        }
    }

    /// The two things a [`super::options::SettingsOutcome`] can ask of the shell.
    ///
    /// The mutation lives here rather than in [`super::options`] because this is
    /// what owns the [`Options`] and the file it is written to — and because the
    /// **eager persistence** rule is a `MenuNav` rule (see the module docs): a
    /// setting that only saved on exit is the setting a crash loses.
    fn apply_settings(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::options::SettingsOutcome,
    ) -> MenuAction {
        use crate::menu::options::{LiveOption, SettingsOutcome};
        match outcome {
            SettingsOutcome::None => MenuAction::None,
            // The root page's Done, or Escape from it. `close_settings` is what
            // knows whether that means the title screen or the pause menu.
            SettingsOutcome::Close => {
                ui.close_settings();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::GuiScale) => {
                self.cycle_gui_scale(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ViewBobbing) => {
                self.toggle_view_bobbing();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ShowSubtitles) => {
                self.toggle_show_subtitles();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ToggleSneak) => {
                self.toggle_toggle_sneak();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ToggleSprint) => {
                self.toggle_toggle_sprint();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ToggleAttack) => {
                self.toggle_toggle_attack();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ToggleUse) => {
                self.toggle_toggle_use();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::AutoJump) => {
                self.toggle_auto_jump();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::SprintWindow) => {
                self.step_sprint_window(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::InvertMouseX) => {
                self.toggle_invert_mouse_x();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::DiscreteMouseScroll) => {
                self.toggle_discrete_mouse_scroll();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::InvertMouseY) => {
                self.toggle_invert_mouse_y();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::MouseWheelSensitivity) => {
                self.cycle_mouse_wheel_sensitivity(1);
                MenuAction::None
            }
            // The eight chat/text-background options. Each one steps its
            // `UnitDouble` by [`crate::config::UNIT_DOUBLE_STEP`] and persists,
            // and `app.rs` already copies all eight into
            // `hud_frame.chat_options` from `self.nav.options()` every frame —
            // so no threading is needed beyond the mutation here.
            SettingsOutcome::Cycle(LiveOption::ChatScale) => {
                self.step_unit_double_option(|o| &mut o.chat_scale, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatWidth) => {
                self.step_unit_double_option(|o| &mut o.chat_width, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatHeightFocused) => {
                self.step_unit_double_option(|o| &mut o.chat_height_focused, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatHeightUnfocused) => {
                self.step_unit_double_option(|o| &mut o.chat_height_unfocused, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatLineSpacing) => {
                self.step_unit_double_option(|o| &mut o.chat_line_spacing, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatOpacity) => {
                self.step_unit_double_option(|o| &mut o.chat_opacity, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::TextBackgroundOpacity) => {
                self.step_unit_double_option(|o| &mut o.chat_background_opacity, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatColors) => {
                self.toggle_chat_colors();
                MenuAction::None
            }
            // Issue #443's two migrated options. Both persist eagerly like
            // every arm above; unlike the chat eight, neither takes effect in
            // the *current* session, because their consumers read
            // `config::Config` and `Config::resolve_persisted` folds
            // `options.json` in at launch. That is vanilla's own behaviour for
            // `renderDistance` (`applyValueImmediately = false`) and a
            // documented departure for `sensitivity` — see
            // `Config::resolve_persisted`.
            SettingsOutcome::Cycle(LiveOption::Sensitivity) => {
                self.step_unit_double_option(|o| &mut o.sensitivity, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::RenderDistance) => {
                self.step_render_distance(1);
                MenuAction::None
            }
            // The two Accessibility-page sliders whose consumers were already
            // live. Neither needs threading beyond the mutation here:
            // `app/redraw.rs` already reads `MenuNav::damage_tilt_strength` every
            // frame, and `render::frame_for` stamps
            // `MenuFrame::panorama_speed` onto every frame beside `gui_scale`.
            SettingsOutcome::Cycle(LiveOption::DamageTiltStrength) => {
                self.step_unit_double_option(|o| &mut o.damage_tilt_strength, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::PanoramaSpeed) => {
                self.step_unit_double_option(|o| &mut o.panorama_speed, 1);
                MenuAction::None
            }
            // The fifteen rows whose consumers already ran every frame against a
            // hardcoded constant: eleven mixer buses, the projection FOV, the two
            // glint parameters and the cloud geometry. Nothing needs threading
            // beyond the mutation here — `app/redraw.rs` reads all four of
            // `Sim::set_sound_volumes`, `Sim::set_fov_y_degrees`,
            // `RenderState::set_glint_options` and `RenderState::set_cloud_status`
            // off `MenuNav::options` once per presented frame.
            SettingsOutcome::Cycle(LiveOption::SoundVolume(index)) => {
                self.step_sound_volume(index, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::Fov) => {
                self.step_fov(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::GlintSpeed) => {
                self.step_unit_double_option(|o| &mut o.glint_speed, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::GlintStrength) => {
                self.step_unit_double_option(|o| &mut o.glint_strength, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::CloudStatus) => {
                self.cycle_cloud_status(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::FramerateLimit) => {
                self.step_framerate_limit(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::EnableVsync) => {
                self.toggle_enable_vsync();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::InactivityFpsLimit) => {
                self.cycle_inactivity_fps_limit(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::GraphicsPreset) => {
                self.step_graphics_preset(1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::CutoutLeaves) => {
                self.toggle_cutout_leaves();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::MipmapLevels) => {
                self.step_mipmap_levels(1);
                MenuAction::None
            }
        }
    }

    /// Steps one of the eleven `soundSource.*` volumes and persists it eagerly.
    ///
    /// Goes through [`Self::step_unit_double_option`] rather than writing the
    /// wrap out again — the eleven are `UnitDouble`s like the chat sliders, and
    /// the only thing that varies is the array slot.
    ///
    /// An out-of-range index is a **no-op**, not a panic: the index arrives from
    /// a `const` cell on the Sound page, so a bad one is an authoring mistake in
    /// `menu::options::SOUND` and is caught by
    /// `sound_rows_index_the_category_they_name`, not something a player can
    /// provoke mid-session.
    fn step_sound_volume(&mut self, index: u8, delta: i32) {
        let slot = index as usize;
        if slot >= self.options.sound_volumes.len() {
            return;
        }
        self.step_unit_double_option(move |o| &mut o.sound_volumes[slot], delta);
    }

    /// Steps `fov` by one degree and wraps, then persists.
    ///
    /// **Wraps rather than saturating**, for the reason
    /// [`Self::step_render_distance`] records at length: a keyboard Enter is a
    /// click, and a value parked at 110 has to be able to come back down. A
    /// *mouse* click on the track goes through [`Self::set_live_slider`] instead
    /// and lands wherever the cursor is, so the 81-degree span is not 81 clicks
    /// for a mouse user.
    ///
    /// The bounds are `config`'s [`crate::config::MIN_FOV`]/`MAX_FOV`, which are
    /// vanilla's `IntRange(30, 110)` — the same pair
    /// `menu::options::INT_RANGE_SLIDERS` places the handle with, so the value a
    /// click can reach and the track it draws on cannot disagree.
    fn step_fov(&mut self, delta: i32) {
        use crate::config::{MAX_FOV, MIN_FOV};
        let span = (MAX_FOV - MIN_FOV + 1) as i32;
        let offset = self.options.fov as i32 - MIN_FOV as i32;
        let wrapped = (offset + delta).rem_euclid(span);
        self.options.fov = MIN_FOV + wrapped as u32;
        self.persist_options();
    }

    /// Cycles Clouds through `CloudStatus.values()`' own declaration order — OFF,
    /// FAST, FANCY — and wraps, then persists.
    ///
    /// **Three states, and the order is the enum's rather than a chosen one**,
    /// because that is what `CycleButton` visits. A hand-picked order would put
    /// FANCY (the default) somewhere other than where vanilla's third click
    /// leaves it.
    ///
    /// A `cloud_status` that is somehow not in the list restarts at OFF rather
    /// than sticking, which cannot happen through
    /// [`crate::config::cloud_status_from_name`] but keeps the `position` lookup
    /// honest about being fallible.
    fn cycle_cloud_status(&mut self, delta: i32) {
        use lodestone_render::CloudStatus;
        const ORDER: [CloudStatus; 3] = [CloudStatus::Off, CloudStatus::Fast, CloudStatus::Fancy];
        let index = ORDER
            .iter()
            .position(|s| *s == self.options.cloud_status)
            .unwrap_or(0) as i32;
        let next = (index + delta).rem_euclid(ORDER.len() as i32) as usize;
        self.options.cloud_status = ORDER[next];
        self.persist_options();
    }

    /// Steps `framerateLimit` by one bucket (10 fps) and wraps, then persists.
    ///
    /// Wraps in the `[10, 260]` domain rather than saturating, for
    /// [`Self::step_render_distance`]'s reason: this is a click-only control
    /// (a *drag* goes through [`Self::set_live_slider`] instead), and a value
    /// parked at "Unlimited" has to be able to come back down.
    fn step_framerate_limit(&mut self, delta: i32) {
        use crate::config::{MIN_FRAMERATE_LIMIT, UNLIMITED_FRAMERATE_CUTOFF};
        let buckets = (UNLIMITED_FRAMERATE_CUTOFF - MIN_FRAMERATE_LIMIT) / 10 + 1;
        let offset = (self.options.framerate_limit - MIN_FRAMERATE_LIMIT) / 10;
        let wrapped = (offset as i32 + delta).rem_euclid(buckets as i32) as u32;
        self.options.framerate_limit = MIN_FRAMERATE_LIMIT + wrapped * 10;
        self.persist_options();
    }

    /// Steps `mipmapLevels` by one and wraps, then persists — vanilla's own
    /// `IntRange(0, 4)` (`menu::options::INT_RANGE_SLIDERS`'s `"mipmapLevels"`
    /// row), the same click-only wrap shape as [`Self::step_fov`]: a *drag*
    /// goes through [`Self::set_live_slider`] instead, and a value parked at
    /// the maximum has to be able to come back down through a click alone.
    ///
    /// Also pushes the new depth into `crate::resources::set_mipmap_levels`,
    /// which is what actually rebuilds the atlas — this function only owns
    /// the menu-side value and its wrap, exactly as
    /// [`Self::set_live_slider`]'s own `MipmapLevels` arm does for a drag.
    fn step_mipmap_levels(&mut self, delta: i32) {
        let max = lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS as i32;
        let span = max + 1;
        let offset = self.options.mipmap_levels as i32;
        let wrapped = (offset + delta).rem_euclid(span);
        self.options.mipmap_levels = wrapped as u32;
        crate::resources::set_mipmap_levels(self.options.mipmap_levels);
        self.persist_options();
    }

    /// Flips `options.vsync` and saves immediately, same eager-persistence
    /// rule as [`Self::toggle_chat_colors`]. The live consumer is
    /// `WindowApp::sync_vsync_present_mode`, which polls this field every
    /// presented frame rather than being pushed from here — see that method's
    /// doc for why a poll is the safe shape against a GPU setter.
    fn toggle_enable_vsync(&mut self) {
        self.options.enable_vsync = !self.options.enable_vsync;
        self.persist_options();
    }

    /// Cycles `inactivityFpsLimit` through its two declared states
    /// (`Minimized`, `Afk`) and wraps, then persists. Same shape as
    /// [`Self::cycle_cloud_status`], two states instead of three.
    fn cycle_inactivity_fps_limit(&mut self, delta: i32) {
        use crate::config::InactivityFpsLimit;
        const ORDER: [InactivityFpsLimit; 2] =
            [InactivityFpsLimit::Minimized, InactivityFpsLimit::Afk];
        let index = ORDER
            .iter()
            .position(|s| *s == self.options.inactivity_fps_limit)
            .unwrap_or(0) as i32;
        let next = (index + delta).rem_euclid(ORDER.len() as i32) as usize;
        self.options.inactivity_fps_limit = ORDER[next];
        self.persist_options();
    }

    /// Steps `graphicsPreset` through `GraphicsPreset::ORDER` and wraps, then
    /// applies it — [`Self::apply_graphics_preset`] — and persists.
    ///
    /// The apply happens **every** step, `Custom` included, matching vanilla:
    /// `Options::applyGraphicsPreset` calls `value.apply(minecraft)`
    /// unconditionally, and `GraphicsPreset.apply`'s `switch` simply has no
    /// `CUSTOM` case, so applying `Custom` is a real call that writes nothing
    /// — not a skipped call. [`Self::apply_graphics_preset`] mirrors that
    /// shape rather than special-casing `Custom` at this call site.
    fn step_graphics_preset(&mut self, delta: i32) {
        use crate::config::GraphicsPreset;
        let index = GraphicsPreset::ORDER
            .iter()
            .position(|p| *p == self.options.graphics_preset)
            .unwrap_or(0) as i32;
        let next = (index + delta).rem_euclid(GraphicsPreset::ORDER.len() as i32) as usize;
        self.options.graphics_preset = GraphicsPreset::ORDER[next];
        self.apply_graphics_preset();
        self.persist_options();
    }

    /// Vanilla's `GraphicsPreset::apply` (`GraphicsPreset.java`), the
    /// three fields of its seventeen this client has real consumers for.
    ///
    /// | preset | `renderDistance` | `cloudStatus` | `cutoutLeaves` |
    /// |---|---|---|---|
    /// | `Fast` | 8 | `Fast` | `false` |
    /// | `Fancy` | 16 | `Fancy` | `true` |
    /// | `Fabulous` | 32 | `Fancy` | `true` |
    /// | `Custom` | — | — | — |
    ///
    /// `Custom` writes nothing, matching vanilla's own `switch` (no `CUSTOM`
    /// arm — see [`Self::step_graphics_preset`]'s doc). The fourteen fields
    /// this client does not have a consumer for at all
    /// (`biomeBlendRadius`, `simulationDistance`, `particles`,
    /// `mipmapLevels`, `entityShadows`, `menuBackgroundBlurriness`,
    /// `cloudRange`, `improvedTransparency`, `weatherRadius`,
    /// `maxAnisotropyBit`, `textureFiltering`, `prioritizeChunkUpdates`,
    /// `entityDistanceScaling`, `ambientOcclusion`) are left alone — writing
    /// them would move a settings-row *label* with nothing behind it to
    /// consume the new value, the exact fabrication `docs/`'s "departure 1"
    /// exists to name rather than hide.
    ///
    /// Never resets a hand-picked `render_distance`/`cloud_status`/
    /// `cutout_leaves` back to `Custom` on its own: vanilla's
    /// `setGraphicsPresetToCustom` (called from each of those options'
    /// individual `onChange`) has no counterpart here yet, so choosing FAST
    /// and then hand-tweaking Render Distance leaves the Preset row reading
    /// "Fast" even though the value it placed has moved — a known,
    /// documented gap rather than a silent one.
    fn apply_graphics_preset(&mut self) {
        use crate::config::GraphicsPreset;
        use lodestone_render::CloudStatus;
        match self.options.graphics_preset {
            GraphicsPreset::Fast => {
                self.options.render_distance = 8;
                self.options.cloud_status = CloudStatus::Fast;
                self.options.cutout_leaves = false;
            }
            GraphicsPreset::Fancy => {
                self.options.render_distance = 16;
                self.options.cloud_status = CloudStatus::Fancy;
                self.options.cutout_leaves = true;
            }
            GraphicsPreset::Fabulous => {
                self.options.render_distance = 32;
                self.options.cloud_status = CloudStatus::Fancy;
                self.options.cutout_leaves = true;
            }
            GraphicsPreset::Custom => {}
        }
    }

    /// Flips `options.cutoutLeaves` and saves immediately, same eager-persistence
    /// rule as [`Self::toggle_chat_colors`]. See
    /// [`crate::config::Options::cutout_leaves`]'s doc for the render-side
    /// consumer and why toggling it forces a remesh.
    fn toggle_cutout_leaves(&mut self) {
        self.options.cutout_leaves = !self.options.cutout_leaves;
        self.persist_options();
    }

    /// Steps `renderDistance` by one chunk and wraps, then persists.
    ///
    /// **Wraps rather than saturating**, matching every other live control on
    /// this tree (`cycle_gui_scale`'s `rem_euclid`, `cycle_mouse_wheel_sensitivity`'s
    /// period): a click is the only way to move these rows, so a value parked at
    /// the maximum has to be able to come back down. Vanilla drags instead and
    /// therefore needs no wrap at all — this is a consequence of departure 1, not
    /// a transcription of `IntRangeBase::next` (`OptionInstance.java`),
    /// which really does saturate.
    ///
    /// The bounds are `config`'s, which are vanilla's `IntRange(2, 32)` — the same
    /// pair `menu::options::INT_RANGE_SLIDERS` places the handle with, so the
    /// value a click can reach and the track it draws on cannot disagree.
    fn step_render_distance(&mut self, delta: i32) {
        use crate::config::{MAX_RENDER_DISTANCE, MIN_RENDER_DISTANCE};
        let span = (MAX_RENDER_DISTANCE - MIN_RENDER_DISTANCE + 1) as i32;
        let offset = self.options.render_distance as i32 - MIN_RENDER_DISTANCE as i32;
        let wrapped = (offset + delta).rem_euclid(span);
        self.options.render_distance = MIN_RENDER_DISTANCE + wrapped as u32;
        self.persist_options();
    }

    /// Set a live slider's value from a track fraction — the drag half of
    /// vanilla's `AbstractSliderButton` (`onClick`/`onDrag` both call
    /// `setValueFromMouse`).
    ///
    /// Returns `true` when the fraction was applied, `false` for a
    /// [`LiveOption`] that is not slider-shaped (every toggle, and the two the
    /// ranges below do not cover) — the caller falls back to the click-step
    /// path on `false` rather than swallowing the click, so a control this does
    /// not understand keeps working exactly as it did.
    ///
    /// **This is the one place the departure recorded on
    /// [`Self::step_render_distance`] is lifted.** That doc says "a click is the
    /// only way to move these rows, so a value parked at the maximum has to be
    /// able to come back down", which is why every `step_*` wraps. With a real
    /// drag the wrap is no longer load-bearing for reachability — but the
    /// `step_*` functions keep it, because they are still what a *keyboard*
    /// Enter uses and that has no other way down.
    ///
    /// The two conversions both come from the tables the *handle draw* uses
    /// (`LiveOption::unit_double_mut`, `LiveOption::int_range`), never from a
    /// restated range: a slider whose drag and whose handle disagreed about the
    /// bounds would land the handle somewhere the value cannot be.
    fn set_live_slider(&mut self, live: LiveOption, fraction: f32) -> bool {
        let f = fraction.clamp(0.0, 1.0);
        // The eight `UnitDouble` options plus `sensitivity`: the fraction *is*
        // the value, so this needs no conversion at all.
        if let Some(slot) = live.unit_double_mut(&mut self.options) {
            *slot = f;
            self.persist_options();
            return true;
        }
        // The `IntRange` options, through vanilla's own bucket map.
        if let Some(range) = live.int_range() {
            let value = range.from_slider_value(f);
            match live {
                LiveOption::RenderDistance => {
                    self.options.render_distance = value.max(0) as u32;
                }
                LiveOption::SprintWindow => {
                    self.options.sprint_window_ticks = value.clamp(0, 255) as u8;
                }
                // The clamp is `config`'s own bounds rather than `0..`: an FOV of
                // zero is a degenerate projection matrix, and `from_slider_value`
                // is only guaranteed in range for a fraction in `[0, 1]`.
                LiveOption::Fov => {
                    self.options.fov = value.clamp(
                        crate::config::MIN_FOV as i32,
                        crate::config::MAX_FOV as i32,
                    ) as u32;
                }
                // `framerateLimit`'s bucket is `fps / 10` (`INT_RANGE_SLIDERS`'s
                // own row), so the value this bucket map returns has to be
                // multiplied back before it is a real fps.
                LiveOption::FramerateLimit => {
                    self.options.framerate_limit = (value.max(1) as u32 * 10).clamp(
                        crate::config::MIN_FRAMERATE_LIMIT,
                        crate::config::UNLIMITED_FRAMERATE_CUTOFF,
                    );
                }
                // `mipmapLevels`' bucket is the depth itself (`INT_RANGE_SLIDERS`'s
                // own row is `IntRange(0, 4)` with no xmap), so unlike
                // `FramerateLimit` above nothing has to be scaled back. Pushes the
                // new depth into `crate::resources::set_mipmap_levels`, which is
                // what actually rebuilds the atlas — see that function's doc.
                LiveOption::MipmapLevels => {
                    self.options.mipmap_levels = value
                        .clamp(0, lodestone_render::texture::BLOCK_ATLAS_MIP_LEVELS as i32)
                        as u32;
                    crate::resources::set_mipmap_levels(self.options.mipmap_levels);
                }
                // `int_range` only answers for the five above; a sixth would
                // have to add its own write here, and falling through to
                // `false` is the honest result until it does.
                _ => return false,
            }
            self.persist_options();
            return true;
        }
        // `graphicsPreset`'s `SliderableEnum`, the third shape alongside
        // `UnitDouble` and `IntRange` above — see
        // `menu::options::graphics_preset_from_fraction`.
        if live == LiveOption::GraphicsPreset {
            self.options.graphics_preset = crate::menu::options::graphics_preset_from_fraction(f);
            self.apply_graphics_preset();
            self.persist_options();
            return true;
        }
        false
    }

    /// Steps one `UnitDouble`-backed option and persists it eagerly.
    ///
    /// Takes a field selector rather than being written out once per option:
    /// every one of these has an identical `[0, 1]` domain and an identical wrap,
    /// so the only thing that varies is which field is being moved. The
    /// per-option *semantics* (the pixel and percent mappings, the OFF caption)
    /// live in `menu::options::live_value`, where the vanilla stringifier they
    /// come from is cited.
    ///
    /// **Was `step_chat_option`**, and the rename is the point rather than
    /// tidying: it now carries the Damage Tilt and Panorama Scroll Speed rows on
    /// the Accessibility page too, and a name claiming otherwise is how the next
    /// reader concludes there is no generic stepper and writes a second one.
    fn step_unit_double_option(
        &mut self,
        field: impl FnOnce(&mut Options) -> &mut f32,
        delta: i32,
    ) {
        let slot = field(&mut self.options);
        *slot = crate::config::step_unit_double(*slot, delta);
        self.persist_options();
    }

    /// Flips `options.chat.color`, the one non-slider chat option.
    fn toggle_chat_colors(&mut self) {
        self.options.chat_colors = !self.options.chat_colors;
        self.persist_options();
    }

    /// The account list: entirely delegated to [`accounts::AccountsNav`],
    /// which owns the row highlight, the scroll window and the sign-in state
    /// machine. This arm's only job is translating its
    /// [`accounts::AccountsSignal::Back`] into leaving the screen — every
    /// other outcome (selecting an account, starting/cancelling a sign-in,
    /// removing an account) is a self-contained mutation `AccountsNav`
    /// already applied by the time this returns.
    fn key_accounts(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        use crate::menu::accounts::AccountsSignal;
        match self.accounts.handle_key(key) {
            AccountsSignal::Back => ui.close_accounts(),
            AccountsSignal::None => {}
        }
        MenuAction::None
    }

    /// The pause menu: Up/Down move the highlight, Enter activates the
    /// highlighted button, Escape resumes play (same as [`UiState::on_escape`]
    /// from [`Screen::Paused`] — spelled out here too rather than falling
    /// through to a catch-all, now that this screen has its own arm).
    fn key_paused(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match key {
            MenuKey::Up => {
                let buttons = self.pause_buttons();
                self.paused = step_enabled(self.paused, buttons.len(), false, &|i| {
                    buttons[i].enabled()
                });
                MenuAction::None
            }
            MenuKey::Down => {
                let buttons = self.pause_buttons();
                self.paused = step_enabled(self.paused, buttons.len(), true, &|i| {
                    buttons[i].enabled()
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
                        // See `MainButton::Options` — a fresh screen, not a
                        // resumed one. Opened from the pause menu, so
                        // `inWorld` is true: the root's header button is the
                        // (unbuilt) World Options fork, same as before.
                        self.settings.reset(true);
                        ui.open_settings_from_pause();
                        MenuAction::None
                    }
                    PauseButton::QuitToTitle => {
                        ui.quit_to_title();
                        MenuAction::QuitToTitle
                    }
                    // Issue #189: a fresh screen, not a resumed one — the
                    // same "reset on every entry" rule `PauseButton::Options`
                    // follows above, so re-opening it never resumes scrolled
                    // down onto a stale roster.
                    PauseButton::PlayerReporting => {
                        self.social.reset();
                        ui.open_social_from_pause();
                        MenuAction::None
                    }
                    // Issue #188 — same "reset on every entry" rule as
                    // `PauseButton::PlayerReporting` immediately above.
                    PauseButton::Statistics => {
                        self.stats.reset();
                        ui.open_statistics_from_pause();
                        MenuAction::None
                    }
                    // Same "reset on every entry" rule as `PauseButton::
                    // Statistics` immediately above — a re-opened screen must
                    // never still be sitting on the confirmation for whatever
                    // link the player last looked at.
                    PauseButton::ServerLinks => {
                        self.server_links.reset();
                        ui.open_server_links_from_pause();
                        MenuAction::None
                    }
                    // Issue #167 — same shape as the two above. `advancements`
                    // is reset on entry so a reopened screen starts on the
                    // default tab with each tab freshly centred, matching
                    // vanilla's per-screen `AdvancementTab` lifetime.
                    PauseButton::Advancements => {
                        self.advancements = crate::menu::advancements::AdvancementsState::default();
                        ui.open_advancements_from_pause();
                        MenuAction::None
                    }
                    // Issue #535. `MenuNav` holds no `Sim` and no world path, so
                    // the publish itself is the app's — same division of labour
                    // as `Respawn` and `Singleplayer`.
                    PauseButton::OpenToLan => MenuAction::OpenToLan,
                    PauseButton::ReportBugs
                    | PauseButton::Feedback
                    | PauseButton::Friends => MenuAction::None,
                }
            }
            MenuKey::Escape => {
                ui.on_escape();
                MenuAction::None
            }
            _ => MenuAction::None,
        }
    }

    /// The death screen (issue #103): Up/Down move the highlight between the
    /// two widgets, Enter activates the highlighted one. Both are always
    /// enabled (see [`DeathButton`]'s docs), so this wraps with
    /// [`wrap_prev`]/[`wrap_next`] rather than [`step_enabled`] — there is no
    /// disabled row to step over, unlike [`key_main`](Self::key_main)/
    /// [`key_paused`](Self::key_paused).
    ///
    /// **Escape is deliberately absent from this match** — it falls to `_`,
    /// which does nothing. Vanilla's `DeathScreen.shouldCloseOnEsc()` returns
    /// `false` (`DeathScreen.java`): the only way off this screen is a
    /// click. Every sibling `key_*` above calls `ui.on_escape()` for
    /// `MenuKey::Escape`; this one is the one screen that must not.
    fn key_death(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match key {
            MenuKey::Up => {
                self.death = wrap_prev(self.death, DEATH_BUTTONS.len());
                MenuAction::None
            }
            MenuKey::Down => {
                self.death = wrap_next(self.death, DEATH_BUTTONS.len());
                MenuAction::None
            }
            MenuKey::Enter => match self.death_button() {
                DeathButton::Respawn => MenuAction::Respawn,
                DeathButton::TitleScreen => {
                    ui.quit_to_title();
                    MenuAction::QuitToTitle
                }
            },
            _ => MenuAction::None,
        }
    }

    /// The credits/end-poem screen (issue #192). One control (Done), no
    /// cursor to move — Up/Down are no-ops, matching [`DeathButton`]'s own
    /// "nothing else to select" screens when they have only one live row, and
    /// unlike vanilla's real `WinScreen`, which dismisses on **any** key. That
    /// "any key" behaviour is a deliberate simplification: every other screen
    /// in this tree distinguishes Enter/Escape from navigation, and this one
    /// stays consistent with that rather than adding the one exception — see
    /// [`super::render::credits_frame`]'s module doc for the fuller reasoning
    /// (this screen's content is a short placeholder, not vanilla's real
    /// auto-scrolling poem, so there is no long scroll a stray keypress needs
    /// to skip past).
    fn key_credits(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match key {
            MenuKey::Enter | MenuKey::Escape => {
                ui.quit_to_title();
                MenuAction::QuitToTitle
            }
            _ => MenuAction::None,
        }
    }

    /// The Social Interactions screen (issue #189). Up/Down/Enter mirror
    /// [`Self::key_settings`]'s shape one screen over (this screen has its
    /// own [`crate::menu::social::SocialNav`], same reason `SettingsNav` gets
    /// one); Escape always leaves for the pause menu, since nothing on this
    /// screen has a "cancel a pending state" step the way a Key Binds capture
    /// does.
    fn key_social(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match key {
            MenuKey::Up => {
                self.social.step(false);
                MenuAction::None
            }
            MenuKey::Down => {
                self.social.step(true);
                MenuAction::None
            }
            MenuKey::Enter => {
                let outcome = self.social.enter();
                self.apply_social(ui, outcome)
            }
            MenuKey::Escape => {
                ui.close_social();
                MenuAction::None
            }
            _ => MenuAction::None,
        }
    }

    /// What a [`crate::menu::social::SocialOutcome`] means at the `UiState`
    /// level — mirrors [`Self::apply_key_binds`]'s shape.
    fn apply_social(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::social::SocialOutcome,
    ) -> MenuAction {
        if outcome == crate::menu::social::SocialOutcome::Back {
            ui.close_social();
        }
        MenuAction::None
    }

    /// The Statistics screen (issue #188). No selection/activation at all on
    /// the General list — it is not clickable in vanilla either (only
    /// narrated), so Up/Down just scroll.
    ///
    /// **Enter is gated on focus, and Tab is what grants it.** A player report
    /// (2026-08-04, "the Statistics menu always has the 'Done' button focused
    /// for some reason") traced to `stats::frame` hard-coding `selected: 0` on
    /// a frame whose only row is Done; see
    /// [`crate::menu::stats::StatsNav::focused`] for what the jar says. Enter
    /// used to close unconditionally, which is `Screen.keyPressed` with a
    /// focused widget — correct behaviour reached from a premise (something is
    /// focused) that is false on open. With nothing focused, vanilla's Enter
    /// does nothing.
    ///
    /// Escape is deliberately **not** gated: `shouldCloseOnEsc()` is true here
    /// and Escape is handled by the screen itself, not by a focused child, so
    /// it is unconditional — which also means there is always a keyboard way
    /// out even before the first Tab.
    fn key_statistics(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match key {
            MenuKey::Up => {
                self.stats.step(false);
                MenuAction::None
            }
            MenuKey::Down => {
                self.stats.step(true);
                MenuAction::None
            }
            MenuKey::Tab => {
                self.stats.focus_next();
                MenuAction::None
            }
            MenuKey::Enter => {
                if self.stats.focused() {
                    ui.close_statistics();
                }
                MenuAction::None
            }
            MenuKey::Escape => {
                ui.close_statistics();
                MenuAction::None
            }
            _ => MenuAction::None,
        }
    }

    /// [`Self::click`]'s Statistics arm — `ContainerEventHandler.mouseClicked`:
    /// focus the child that was hit, *then* call its `onClick`.
    ///
    /// Its own arm rather than the shared `hover` + `Enter` fall-through, for
    /// #391's reason one screen further: with Enter now gated on focus (see
    /// [`Self::key_statistics`]) that pair would need `hover` to grant focus,
    /// and hover granting focus is itself a bug this repo has already fixed
    /// once on the server list. So the click grants focus directly and hover
    /// still grants none.
    fn click_statistics(&mut self, ui: &mut UiState, row: usize) -> MenuAction {
        if row != crate::menu::stats::DONE_ROW {
            return MenuAction::None;
        }
        self.stats.focus_done();
        ui.close_statistics();
        MenuAction::None
    }

    /// A click on the Server Links screen, in whichever view it is showing —
    /// [`crate::menu::server_links::ServerLinksNav::click_row`] decides what
    /// the row *means*; this turns that answer into a [`MenuAction`], the
    /// same split [`Self::click_list`]/[`Self::apply_confirm`] already make.
    fn click_server_links(&mut self, ui: &mut UiState, row: usize) -> MenuAction {
        let outcome = self.server_links.click_row(row);
        self.apply_server_links(ui, outcome)
    }

    fn key_server_links(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match key {
            MenuKey::Escape => {
                let outcome = self.server_links.escape();
                self.apply_server_links(ui, outcome)
            }
            _ => MenuAction::None,
        }
    }

    /// [`crate::menu::server_links::ServerLinksOutcome`] to [`MenuAction`] —
    /// the one place that answer is turned into a screen change or a browser
    /// open, so [`Self::click_server_links`] and [`Self::key_server_links`]
    /// cannot disagree about what a given outcome does.
    fn apply_server_links(
        &mut self,
        ui: &mut UiState,
        outcome: crate::menu::server_links::ServerLinksOutcome,
    ) -> MenuAction {
        use crate::menu::server_links::ServerLinksOutcome;
        match outcome {
            ServerLinksOutcome::Handled => MenuAction::None,
            ServerLinksOutcome::Close => {
                ui.close_server_links();
                MenuAction::None
            }
            // Vanilla's `ConfirmLinkScreen` always returns to the screen it
            // was opened over regardless of which button answered — see
            // `clickUrlAction` — so opening the link also closes this screen,
            // not just the confirmation sub-view.
            ServerLinksOutcome::OpenUrl(url) => {
                crate::menu::accounts::open_in_browser(&url);
                self.server_links.reset();
                ui.close_server_links();
                MenuAction::None
            }
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

    /// Flips vanilla's View Bobbing option and saves immediately, same
    /// eager-persistence rule as [`MenuNav::cycle_gui_scale`].
    fn toggle_view_bobbing(&mut self) {
        self.options.view_bobbing = !self.options.view_bobbing;
        self.persist_options();
    }

    /// Flips `options.showSubtitles` (issue #198) and saves immediately, same
    /// eager-persistence rule as [`MenuNav::toggle_view_bobbing`].
    fn toggle_show_subtitles(&mut self) {
        self.options.show_subtitles = !self.options.show_subtitles;
        self.persist_options();
    }

    /// Flips `key.sneak`'s hold/toggle mode (issue #202) and saves
    /// immediately, same eager-persistence rule as
    /// [`MenuNav::cycle_gui_scale`].
    fn toggle_toggle_sneak(&mut self) {
        self.options.toggle_sneak = !self.options.toggle_sneak;
        self.persist_options();
    }

    /// As [`MenuNav::toggle_toggle_sneak`], for `key.sprint`.
    fn toggle_toggle_sprint(&mut self) {
        self.options.toggle_sprint = !self.options.toggle_sprint;
        self.persist_options();
    }

    /// As [`MenuNav::toggle_toggle_sneak`], for `key.attack` (issue #444).
    fn toggle_toggle_attack(&mut self) {
        self.options.toggle_attack = !self.options.toggle_attack;
        self.persist_options();
    }

    /// As [`MenuNav::toggle_toggle_sneak`], for `key.use` (issue #444).
    fn toggle_toggle_use(&mut self) {
        self.options.toggle_use = !self.options.toggle_use;
        self.persist_options();
    }

    /// Flips `options.autoJump` (issue #444) and saves immediately, same
    /// eager-persistence rule as [`MenuNav::toggle_toggle_sneak`].
    fn toggle_auto_jump(&mut self) {
        self.options.auto_jump = !self.options.auto_jump;
        self.persist_options();
    }

    /// Steps `options.sprintWindow` by one 20 Hz tick and wraps between `0`
    /// and `10` inclusive — vanilla's `IntRange(0, 10)` (`Options.java`),
    /// the same bounds `menu::options::INT_RANGE_SLIDERS` places the handle
    /// with, so the value a click can reach and the track it draws on cannot
    /// disagree. `0` is the "OFF" endpoint (double-tap sprint disabled).
    fn step_sprint_window(&mut self, delta: i32) {
        const MIN: u8 = 0;
        const MAX: u8 = 10;
        let span = (MAX - MIN + 1) as i32;
        let offset = self.options.sprint_window_ticks as i32 - MIN as i32;
        let wrapped = (offset + delta).rem_euclid(span);
        self.options.sprint_window_ticks = MIN + wrapped as u8;
        self.persist_options();
    }

    /// Flips `options.invertMouseX` (issue #203) and saves immediately.
    fn toggle_invert_mouse_x(&mut self) {
        self.options.invert_mouse_x = !self.options.invert_mouse_x;
        self.persist_options();
    }

    /// As [`MenuNav::toggle_invert_mouse_x`], for Y.
    fn toggle_invert_mouse_y(&mut self) {
        self.options.invert_mouse_y = !self.options.invert_mouse_y;
        self.persist_options();
    }

    /// Flips `options.discreteMouseScroll` (issue #444) and saves immediately.
    fn toggle_discrete_mouse_scroll(&mut self) {
        self.options.discrete_mouse_scroll = !self.options.discrete_mouse_scroll;
        self.persist_options();
    }

    /// Steps `mouseWheelSensitivity` by `delta` clicks of
    /// [`crate::config::MOUSE_WHEEL_SENSITIVITY_STEP`], wrapping between
    /// [`crate::config::MIN_MOUSE_WHEEL_SENSITIVITY`] and
    /// [`crate::config::MAX_MOUSE_WHEEL_SENSITIVITY`] inclusive (issue #203),
    /// and saves immediately.
    fn cycle_mouse_wheel_sensitivity(&mut self, delta: i32) {
        use crate::config::{
            MAX_MOUSE_WHEEL_SENSITIVITY, MIN_MOUSE_WHEEL_SENSITIVITY, MOUSE_WHEEL_SENSITIVITY_STEP,
        };
        // Additive, on the *continuous* value — not a round-trip through a
        // quantized step index. Rounding to the nearest step and back would
        // drift the value toward whatever grid the rounding implies (e.g. a
        // starting `1.0` is not itself a multiple of `STEP` away from `MIN`,
        // so round-tripping it would silently move it to the nearest one
        // that is), which is both surprising and, once `sensitivity` no
        // longer sits exactly on the grid, a source of accumulating error
        // across repeated clicks.
        let span = MAX_MOUSE_WHEEL_SENSITIVITY - MIN_MOUSE_WHEEL_SENSITIVITY;
        let period = span + MOUSE_WHEEL_SENSITIVITY_STEP;
        let offset = self.options.mouse_wheel_sensitivity - MIN_MOUSE_WHEEL_SENSITIVITY;
        let wrapped = (offset + delta as f32 * MOUSE_WHEEL_SENSITIVITY_STEP).rem_euclid(period);
        self.options.mouse_wheel_sensitivity =
            (MIN_MOUSE_WHEEL_SENSITIVITY + wrapped).clamp(MIN_MOUSE_WHEEL_SENSITIVITY, MAX_MOUSE_WHEEL_SENSITIVITY);
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

    /// Keeps the highlight inside the list after an add or a delete, and
    /// (#402) keeps the scroll window consistent with wherever that leaves it —
    /// a delete can otherwise strand the scroll offset past the new, shorter
    /// list's end.
    fn clamp_server(&mut self) {
        if self.server >= self.list.len() {
            self.server = self.list.len().saturating_sub(1);
        }
        self.scroll_server_to_show();
    }

    /// Writes the list to disk, recording (not swallowing) any failure.
    fn persist(&mut self) {
        self.save_error = match self.list.save_to(&self.path) {
            Ok(()) => None,
            Err(e) => Some(format!("could not save {}: {e}", self.path.display())),
        };
    }
}

/// The frame that is **actually on screen**, however it got there — the one
/// source `app.rs`'s mouse hit-test (`menu_row_at`) may consult.
///
/// # Why this exists rather than a call to `render::frame_for`
///
/// A player report (2026-08-04, "i cant click anything in the options menu")
/// was exactly this function's absence. `render::frame_for` is the authority on
/// which screens the *menu renderer owns* — screens it draws with a `Clear`
/// pass, replacing the world — and it deliberately answers `None` for the
/// screens that draw as an **overlay** over a still-rendering world. There are
/// now three of those: `Screen::Paused`, `Screen::Death`, and — since
/// `d096de8` — `Screen::Settings` when [`UiState::settings_in_world`], which
/// was made an overlay so that in-world Options stopped drawing the title
/// screen's panorama behind itself.
///
/// `menu_row_at` consulted `frame_for` with a `?`. Pause and death had each
/// been given their own branch there when they became overlays; the third was
/// not, so in-world Options had **no frame to hit-test against at all** and
/// every click returned `None` before it reached a row. Nothing was wrong with
/// the options screen's own geometry: the title-screen copy of the very same
/// rows hit-tests correctly (`clicking_an_options_row_at_its_own_coordinates_
/// activates_that_row` measures it), which is why the geometry was the wrong
/// place to look.
///
/// So the fix is not a fourth branch — it is putting the branch set *somewhere a
/// test can reach*, because three `if`s inlined in a private `app.rs` method
/// cannot be enumerated from anywhere, which is why the third one could go
/// missing silently. [`crate::menu::render::owns_frame`] says which screens
/// route mouse input; this says where their rows come from; and
/// `every_mouse_routable_screen_has_a_frame_to_hit_test` asserts the two sets
/// agree, so the *next* overlay screen fails a test instead of losing its
/// clicks.
///
/// Returns `None` only when no menu-ish screen is up at all — the same meaning
/// `frame_for`'s `None` had at the call site.
#[must_use]
pub fn on_screen_frame<'a>(
    ui: &UiState,
    nav: &MenuNav,
    death_message: Option<&str>,
    statuses: &super::status::StatusCache,
    favicons: &mut super::render::FaviconCache,
) -> Option<super::render::MenuFrame<'a>> {
    if ui.is_paused() {
        return Some(super::render::pause_frame(nav));
    }
    if ui.is_death() {
        return Some(super::render::death_frame(nav, death_message));
    }
    // The third overlay screen, and the one whose absence was the bug. Built
    // from exactly the call `app.rs`'s redraw uses to *draw* it, so the frame
    // the click hit-tests against is the frame on the glass — a second
    // construction here is how a click lands on a row the draw put elsewhere.
    if let Some(frame) = settings_overlay_frame(ui, nav) {
        return Some(frame);
    }
    // The Statistics screen — always reached from the pause menu (see
    // `stats_overlay_frame`'s own doc), so an overlay unconditionally.
    if let Some(frame) = stats_overlay_frame(ui, nav) {
        return Some(frame);
    }
    // The Server Links screen — always reached from the pause menu, so
    // (unlike in-world Settings) it has no out-of-world case at all and is an
    // overlay unconditionally. See `server_links_overlay_frame`'s own doc.
    if let Some(frame) = server_links_overlay_frame(ui, nav) {
        return Some(frame);
    }
    // The fourth overlay screen (#474), and the second instance of the exact
    // shape above. `command_block_overlay_frame` is the *same call* the draw
    // path in `app/redraw.rs` makes — see its own doc for why it is a function
    // rather than a second construction here.
    if let Some(frame) = command_block_overlay_frame(ui, nav) {
        return Some(frame);
    }
    // The fifth overlay screen, same shape as the fourth immediately above.
    if let Some(frame) = sign_edit_overlay_frame(ui, nav) {
        return Some(frame);
    }
    // The sixth overlay screen, same shape again.
    if let Some(frame) = resource_pack_prompt_overlay_frame(ui, nav) {
        return Some(frame);
    }
    // The seventh overlay screen (issue #613's `EditBook`), same shape again.
    if let Some(frame) = book_edit_overlay_frame(ui, nav) {
        return Some(frame);
    }
    super::render::frame_for(ui, nav, statuses, favicons)
}

/// The sign-editing screen's overlay frame, or `None` when that screen is not
/// up — one expression with two consumers, exactly [`command_block_overlay_frame`]'s
/// shape and for the same reason: [`on_screen_frame`] hit-tests a click
/// against this, and `app/redraw.rs`'s overlay block draws it, so a second
/// construction anywhere would be free to disagree with it.
#[must_use]
pub fn sign_edit_overlay_frame<'a>(ui: &UiState, nav: &MenuNav) -> Option<super::render::MenuFrame<'a>> {
    if !ui.is_sign_edit_open() {
        return None;
    }
    let state = nav.sign_edit()?;
    Some(super::render::sign_edit_frame(state))
}

/// The book-editing screen's overlay frame, or `None` when that screen is not
/// up — [`sign_edit_overlay_frame`]'s exact shape and for the same reason:
/// [`on_screen_frame`] hit-tests a click against this, and `app/redraw.rs`'s
/// overlay block draws it, so a second construction anywhere would be free
/// to disagree with it.
#[must_use]
pub fn book_edit_overlay_frame<'a>(ui: &UiState, nav: &MenuNav) -> Option<super::render::MenuFrame<'a>> {
    if !ui.is_book_edit_open() {
        return None;
    }
    let state = nav.book_edit()?;
    Some(super::render::book_edit_frame(state))
}

/// The resource-pack prompt's overlay frame, or `None` when it is not up —
/// the sixth overlay screen, [`sign_edit_overlay_frame`]'s exact shape and
/// for the same reason: a second construction in `app/redraw.rs`'s draw
/// block would be free to disagree with what this hit-tests against.
///
/// **Two bugs this used to carry, found auditing it alongside the Social fix
/// above.** [`crate::menu::confirm::resource_pack_prompt_frame`] builds its
/// `MenuFrame` with `..Default::default()`, so — unlike every other overlay
/// builder in this file — nothing here called [`super::render::stamp_canvas_facts`]
/// or touched `backdrop`, which left `backdrop` at `MenuFrame::default()`'s
/// `MenuBackdrop::Panorama`. `Panorama.wants_panorama()` is `true`, so
/// `MenuRenderer::draw` drew vanilla's cubemap over whatever `render_overlay`'s
/// `Load` op had already put in `view` — the paused world, when the prompt
/// opened from [`Screen::Playing`]/[`Screen::Chat`]/[`Screen::Container`]/
/// [`Screen::Paused`] — the exact defect class `stats_overlay_frame`'s own
/// doc records, and the general sweep in `render/tests.rs` could not catch
/// it: this screen is reached only through `ui.begin(SessionKind::Multiplayer)`
/// (the *no*-world case, where `Panorama` happens to be correct), never
/// through a live world, so `owns_frame_agrees_with_frame_for_on_every_screen`
/// walked straight past the broken case.
///
/// The record settles which is right: `Screen.extractBackground` forks on
/// `this.minecraft.level == null` — panorama with no level, the in-world
/// wash otherwise — exactly [`UiState::settings_in_world`]'s own fork, so
/// this now mirrors [`settings_overlay_frame`] instead of
/// [`sign_edit_overlay_frame`]: `Dim` (plus the blur — `PackConfirmScreen`
/// does not override `isInGameUi()`) when
/// [`UiState::resource_pack_prompt_in_world`], the untouched `Panorama`
/// default otherwise (`Screen::Connecting` has no level, matching vanilla's
/// `level == null` arm). Vanilla actually blurs there too — the `blur`
/// call in `extractBackground` is unconditional once `isInGameUi()` is
/// ruled out, panorama or not — but this port scopes the blur pass to
/// [`MenuRenderer::render_overlay`] frames only (see `render::blur`'s module
/// doc), so the Connecting-screen panorama stays unblurred, a stated cut
/// rather than an oversight.
#[must_use]
pub fn resource_pack_prompt_overlay_frame<'a>(
    ui: &UiState,
    nav: &MenuNav,
) -> Option<super::render::MenuFrame<'a>> {
    if !ui.is_resource_pack_prompt() {
        return None;
    }
    let prompt = nav.resource_pack_prompt()?;
    let mut frame = crate::menu::confirm::resource_pack_prompt_frame(prompt);
    super::render::stamp_canvas_facts(&mut frame, ui, nav);
    if ui.resource_pack_prompt_in_world() {
        frame.backdrop = super::render::MenuBackdrop::Dim;
        frame.blur = true;
    }
    Some(frame)
}

/// The **in-world** settings screen's overlay frame, or `None` when settings is not
/// up in a world — one expression with three consumers ([`on_screen_frame`]'s
/// hit-test, `app/redraw.rs`'s overlay draw, and the gate that measures it).
///
/// # Why this exists
///
/// A player report (2026-08-09): *"the main menu settings have the header/footer,
/// but if i go in game and open settings it doesnt have it. they should be the
/// exact same menu, not separate"*. They **are** the same screen — one
/// `Screen::Settings`, one [`crate::menu::options::settings_frame`] taking no
/// in-world flag, and an `active_list` arm keyed only on the page — so nothing
/// about the *content* differed. What differed is that
/// [`crate::menu::render::frame_for`] stamps the canvas facts onto everything it
/// returns and answers `None` for the overlay screens by design, so the in-world
/// path built `settings_frame` raw and never got them.
///
/// Measured on `SettingsPage::Sound` at 320×240 — the page and canvas the report
/// names — the raw call yields `list: None` where `frame_for` yields `Some`, and the
/// band tint and both bevelled separator bars are gated on
/// `ListSpec::chrome_rect`, so the whole chrome silently vanished. `cursor` was
/// dropped the same way, which is the in-world hover tooltips.
///
/// The draw path was **not** the difference and is worth recording as a ruled-out
/// hypothesis: `MenuRenderer::render` and `render_overlay` share one `draw` body
/// and differ only in the pass's load op, so both emit the chrome identically.
///
/// # The one thing that is legitimately context-dependent
///
/// [`crate::menu::render::MenuBackdrop::Dim`], and it is set here rather than in
/// `settings_frame`. Out of a world the settings tree sits on the panorama; in one
/// it must leave the paused world visible, which is vanilla's own fork
/// (`OptionsScreen` over the level vs over the title). `settings_frame` defaults to
/// `Panorama` and nothing was overriding it, so in-world Options drew the panorama
/// *over* the paused world — the same 2026-08-04 report that made this an overlay
/// in the first place, still live because routing the frame to `render_overlay`
/// changed the load op and not the frame's own backdrop declaration. `pause_frame`
/// and `death_frame` — the sibling overlays — set `Dim` by hand; this is the third.
///
/// The root page's `World Options...` row is **kept** context-dependent on purpose:
/// that is vanilla's `inWorld` header fork and it is about rows, not chrome.
#[must_use]
pub fn settings_overlay_frame<'a>(
    ui: &UiState,
    nav: &MenuNav,
) -> Option<super::render::MenuFrame<'a>> {
    if !(ui.is_settings() && ui.settings_in_world()) {
        return None;
    }
    let mut frame = crate::menu::options::settings_frame(
        nav.settings(),
        nav.options(),
        nav.options_save_error(),
    );
    super::render::stamp_canvas_facts(&mut frame, ui, nav);
    frame.backdrop = super::render::MenuBackdrop::Dim;
    // `OptionsScreen` does not override `isInGameUi()` either, so vanilla
    // blurs behind in-world Options — see `MenuFrame::blur`'s own doc.
    frame.blur = true;
    Some(frame)
}

/// The Statistics screen's overlay frame, or `None` when it is not up —
/// [`settings_overlay_frame`]'s exact shape, but for a screen with no
/// out-of-world case at all: `UiState::open_statistics_from_pause` only
/// opens `Screen::Statistics` from `Screen::Paused`, and there is no
/// title-screen entry point (see that variant's own doc), so this is
/// unconditional rather than gated on an `..._in_world()` predicate the way
/// [`settings_overlay_frame`] is.
///
/// `Dim`, not the default `Panorama` — the fix for the defect
/// [`super::render::dispatch::frame_for`]'s `Screen::Statistics` arm
/// documents at length: a frame built outside `frame_for`'s own `Some` arm
/// never receives its stamp, so the in-world backdrop has to be set here by
/// hand, the same one line [`settings_overlay_frame`]/[`pause_frame`]/
/// [`death_frame`] already carry.
#[must_use]
pub fn stats_overlay_frame<'a>(ui: &UiState, nav: &MenuNav) -> Option<super::render::MenuFrame<'a>> {
    if ui.screen() != Screen::Statistics {
        return None;
    }
    let mut frame = crate::menu::stats::frame(nav.stats(), nav.stats_snapshot());
    super::render::stamp_canvas_facts(&mut frame, ui, nav);
    frame.backdrop = super::render::MenuBackdrop::Dim;
    // `StatsScreen` does not override `isInGameUi()` — see `MenuFrame::blur`'s
    // own doc.
    frame.blur = true;
    Some(frame)
}

/// The Social Interactions screen's overlay frame, or `None` when it is not
/// up — [`stats_overlay_frame`]'s exact shape and for the identical reason:
/// [`UiState::open_social_from_pause`] only opens [`Screen::Social`] from
/// [`Screen::Paused`] and there is no title-screen entry point (see that
/// variant's own doc), so this is unconditional too.
///
/// A player report (2026-08-15) caught this for Statistics; Social has the
/// same defect for the same underlying reason — [`super::render::dispatch::frame_for`]'s
/// old `Screen::Social` arm built `super::social::frame(..)` unconditionally,
/// which routes through `draw_menu`'s `Clear` pass and by construction never
/// renders the world that frame, so no backdrop value that arm's frame ever
/// carried could have shown the paused world behind it. `Dim`, not the
/// default `Panorama`, and stamped with the same canvas facts
/// [`stats_overlay_frame`] carries — a frame built outside `frame_for`'s own
/// `Some` arm gets neither for free.
#[must_use]
pub fn social_overlay_frame<'a>(ui: &UiState, nav: &MenuNav) -> Option<super::render::MenuFrame<'a>> {
    if ui.screen() != Screen::Social {
        return None;
    }
    let mut frame = crate::menu::social::frame(nav.social(), ui.kind());
    super::render::stamp_canvas_facts(&mut frame, ui, nav);
    frame.backdrop = super::render::MenuBackdrop::Dim;
    // `SocialInteractionsScreen` does not override `isInGameUi()` either.
    frame.blur = true;
    Some(frame)
}

/// The Server Links screen's overlay frame, or `None` when it is not up —
/// one expression with two consumers, [`settings_overlay_frame`]'s exact
/// shape and for the same underlying reason: this screen can only ever be
/// reached from the pause menu (see [`super::Screen::ServerLinks`]'s own
/// doc), so it is an overlay unconditionally rather than conditionally like
/// in-world Settings. That is also why [`super::render::frame_for`] carries
/// no `Screen::ServerLinks` arm at all — every case is the overlay case,
/// so there is nothing for that dispatcher to build.
///
/// `Dim`, not the default `Panorama` — the same fix
/// [`settings_overlay_frame`]'s own doc explains at length: a frame built
/// without going through `frame_for`'s stamp has to set the in-world
/// backdrop by hand, or the paused world it must leave visible gets replaced
/// by the panorama that belongs to the *main menu* only.
#[must_use]
pub fn server_links_overlay_frame<'a>(
    ui: &UiState,
    nav: &MenuNav,
) -> Option<super::render::MenuFrame<'a>> {
    if ui.screen() != Screen::ServerLinks {
        return None;
    }
    let mut frame = crate::menu::server_links::frame(&nav.server_links);
    super::render::stamp_canvas_facts(&mut frame, ui, nav);
    frame.backdrop = super::render::MenuBackdrop::Dim;
    // This client has no dedicated `ServerLinksScreen` in vanilla to check —
    // it stands in for `Dialogs.SERVER_LINKS`, a dialog over the pause
    // screen, which inherits `PauseScreen`'s own non-`isInGameUi` fork. See
    // `MenuFrame::blur`'s own doc.
    frame.blur = true;
    Some(frame)
}

/// The command block edit screen's overlay frame, or `None` when that screen is
/// not up — **one expression with two consumers**.
///
/// [`on_screen_frame`] hit-tests a click against this, and `app/redraw.rs`'s
/// overlay block draws it. That is the whole reason it is a function: issue
/// #474's draw half (`0948f59`) put `render::command_block_frame(state,
/// nav.command_tree())` inline in `redraw.rs`, and a *second* construction here
/// would be free to disagree with it — which is a click landing on a row the
/// draw put somewhere else, the failure mode that is invisible in a screenshot
/// because both halves look individually correct.
///
/// The in-world Settings arm above still constructs `settings_frame` twice for
/// historical reasons; this one does not, and is the shape to copy for the next
/// overlay screen.
///
/// `nav.command_tree()` rather than `None`: the suggestion popup is fed by the
/// tree the server actually sent (#470 decodes it, #471 routes it here), and
/// the popup is part of the frame the click has to hit-test against.
#[must_use]
pub fn command_block_overlay_frame<'a>(
    ui: &UiState,
    nav: &MenuNav,
) -> Option<super::render::MenuFrame<'a>> {
    if !ui.is_command_block_open() {
        return None;
    }
    let state = nav.command_block()?;
    Some(super::render::command_block_frame(
        state,
        nav.command_tree(),
    ))
}

/// Whether the mouse and keyboard are routed to the menu layer rather than to
/// gameplay — the predicate `app/lifecycle.rs` guards its `CursorMoved`,
/// `MouseInput` and `KeyGate::menu` arms on.
///
/// # Why this is a function and not three copies of one expression
///
/// It was three copies. `render::owns_frame(screen) || ui.is_paused() ||
/// ui.is_death()` appeared literally in the hover guard, the click guard and
/// the `KeyGate` construction, and a *fourth* copy appeared in
/// `every_mouse_routable_screen_has_a_frame_to_hit_test` — which is precisely
/// why that gate could not see issue #474. The gate re-derived the routing rule
/// instead of calling it, so it asserted a self-consistent pair of its own
/// making: `Screen::CommandBlockEdit` was not in the gate's copy either, and
/// adding a frame for it would not have made the gate fail, nor would omitting
/// one.
///
/// With the rule named once, the gate's `routable` premise and the production
/// guard are the same code, so a screen the driver routes to and
/// [`on_screen_frame`] has no frame for is a test failure rather than a silent
/// dropped click.
///
/// `Screen::CommandBlockEdit` is in the set for the same reason `Paused` and
/// `Death` are: it is an overlay ([`render::owns_frame`](super::render::
/// owns_frame) is `false` for it, deliberately — the world keeps rendering
/// behind it, matching vanilla's `isInGameUi() == true`) with its own rows to
/// hover, click and type into. Without it the screen opened, and neither a
/// click nor a keystroke ever reached it.
///
/// Not a `UiState` method: `owns_frame` lives in `render`, so putting this on
/// `UiState` would make `menu.rs` depend on the renderer to answer an input
/// question.
#[must_use]
pub fn routes_menu_input(ui: &UiState) -> bool {
    super::render::owns_frame(ui.screen())
        || ui.is_paused()
        || ui.is_death()
        || ui.is_command_block_open()
        || ui.is_sign_edit_open()
        // Same reasoning as `is_command_block_open`/`is_sign_edit_open`
        // immediately above: `Screen::BookEdit` is `owns_frame == false`
        // (see [`book_edit_overlay_frame`]'s own doc) with its own rows to
        // hover, click and type into. Without this arm the screen would open
        // and never receive a single keystroke or click.
        || ui.is_book_edit_open()
        // Issue #167. Advancements is not `owns_frame` (it is an overlay drawn
        // through `ContainerRenderer`), but Escape has to close it, and the
        // `_` arm of `MenuNav::key` routes exactly that through
        // `UiState::on_escape`.
        || ui.is_advancements()
        // Same reasoning as `is_command_block_open`/`is_sign_edit_open`
        // immediately above: without this arm a click or keystroke while the
        // prompt is up would fall through to gameplay input (mining,
        // movement) instead of answering the dialog — the screen would open,
        // draw, and never receive a single Accept/Decline.
        || ui.is_resource_pack_prompt()
        // Server Links (like the `Screen::ResourcePackPrompt` arm above) is
        // `owns_frame == false` unconditionally — it is never routed through
        // the Clear pass, only ever drawn as an overlay — so without this arm
        // every click and keystroke on it would fall straight through to
        // gameplay input instead of reaching the screen at all.
        || ui.screen() == Screen::ServerLinks
}

/// Steps `i` one row in `forward`'s direction, wrapping, and keeps stepping
/// while the row it lands on is disabled.
///
/// This is vanilla's own focus rule: `AbstractWidget::nextFocusPath` returns
/// `null` for an inactive widget (`AbstractWidget.java`), so keyboard
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
        // Issue #397: Singleplayer opens the world list — vanilla's own wiring —
        // where it used to return `MenuAction::Singleplayer` and launch directly.
        // There is no action for the app to take at *this* button; the launch is
        // Play Selected World, one screen in (#287).
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
    /// `clearFocus()`-then-retry (`Screen.java`) and not `(i + 1) % n` —
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

    /// The ordering that is the whole of #395 at this screen: the focused field
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
            KeyOutcome::Consumed,
            "Left is the caret's, not the focus layer's"
        );
        assert_eq!(form.fields.name.cursor_position(), 2);
        assert_eq!(form.field(), FormField::Name, "and focus did not move");
    }

    /// `EditForm`'s focus ids and `menu::render`'s row indices are the same
    /// numbers, and `app.rs` reports a click as a row index — so if they ever
    /// diverge, clicking the address field would focus the name one. Same shape
    /// as `the_settings_rows_are_in_the_order_click_assumes`, and the same bug
    /// (#391) it guards against.
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
        // Resource Packs, Done, Cancel (`ManageServerScreen.java`).
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
        // field — it is a different question, carried on `hovered` (#391's
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
    fn the_edit_form_fields_carry_vanillas_real_narration_and_hint() {
        // `ManageServerScreen.java`: `manageServer.enterName`/
        // `manageServer.enterIp` (`en_us.json`: "Server Name"/"Server
        // Address") as each field's own message, and `nameEdit.setHint(
        // selectServer.defaultName)` = "Minecraft Server" on the name field
        // only — the IP field never gets a hint in the jar either.
        let form = EditForm::adding();
        assert_eq!(form.fields.name.widget.message, "Server Name");
        assert_eq!(form.fields.address.widget.message, "Server Address");
        assert_eq!(form.fields.name.hint.as_deref(), Some("Minecraft Server"));
        assert_eq!(
            form.fields.address.hint, None,
            "vanilla's ipEdit never gets a setHint call"
        );
    }

    /// Clicking a field must **focus** it, not activate the screen.
    ///
    /// `MenuNav::click` translates a click into `hover` + `Enter` for every screen
    /// that has a row cursor, and on this screen `Enter` means *save* — so
    /// clicking either field used to submit the form. That is #391's shape one
    /// screen over, and #395's dispatch is what makes it visible:
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
    /// `::Accessibility`) used to be present-and-disabled with a stale reason
    /// ("no language-selection screen" / "no accessibility options screen") —
    /// both destination pages have existed since #415 and #55 respectively,
    /// and nothing ever revisited the button. This is the structural-liveness
    /// finding this test pins: each icon must open `Screen::Settings` on
    /// *its own* page directly (vanilla's `TitleScreen.java`
    /// constructs `LanguageSelectScreen`/`AccessibilityOptionsScreen` with
    /// `lastScreen = this`, never through `OptionsScreen`), and Escape from
    /// there must leave in **one** step, straight back to the title — not
    /// two, via the root grid — which is what an empty page stack
    /// (`SettingsNav::open_at`) buys over the grid button's push-from-Root.
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
    /// reachable, which is the property issue #55 turns on (117 of 135 controls
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

    /// Matches the `OptionInstance` whose `Options.java` accessor is `name`.
    fn is_option(name: &str) -> impl Fn(&crate::menu::options::Cell) -> bool + '_ {
        move |c| matches!(c, crate::menu::options::Cell::Option(s) if s.accessor == name)
    }

    /// [`settings_row`]'s counterpart for the Key Binds screen (issue #15):
    /// drives `KeyBindsNav`'s own cursor with nothing but Down, the same
    /// "reaching it this way proves it is reachable" reasoning that method's
    /// own doc gives. `nav.key` already routes to the right cursor by itself
    /// once `SettingsNav::page()` is `KeyBinds` — see `key_settings`'s guard
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
    /// `ChatComponent.getWidth(pct) = floor(pct * 280 + 40)`
    /// (`ChatComponent.java`), so `1.0` is 320px and `0.0` is 40px, and
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
            (geo.verts[6] + 1.0) * CANVAS_W / 2.0
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
        let src = include_str!("../app/redraw.rs");
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
        // `SoundSource.getName()` spellings.
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
        let src = include_str!("../app/redraw.rs");
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
        let src = include_str!("render/renderer.rs");
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

    /// Issue #391. A **click** on the settings screen must act on the row it
    /// landed on, not on whatever `MenuKey::Enter` means there.
    ///
    /// This is the whole bug. `app.rs` translated every menu click into
    /// `hover(row)` + `Enter`, which is right on the screens that have a row
    /// cursor and was wrong here, where there was none and `Enter` was hard-wired
    /// to View Bobbing. So a click on the GUI SCALE row — row 0, the one drawn
    /// `selected` — turned the option off and wrote it to disk. Nothing about the
    /// bob itself was broken; the reporter's `options.json` simply said `false`.
    ///
    /// #55 removed the cause rather than the symptom: the screen has a real
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

    /// #202/#444: clicking Sneak/Sprint's rows on the Controls page toggles
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

        // Attack/Destroy is now a live row too (#444): clicking it flips only
        // its own mode, leaving Sneak and Sprint untouched.
        let attack = settings_row(&mut nav, &mut ui, is_option("toggleAttack"));
        assert_eq!(nav.click(&mut ui, attack), MenuAction::None);
        assert!(nav.toggle_attack(), "the clicked row must flip");
        assert!(nav.toggle_sneak(), "and not its neighbours");
        assert!(nav.toggle_sprint());
        assert!(crate::config::Options::load_from(&options_path).toggle_attack);
    }

    /// #203: clicking the Mouse page's Scroll Sensitivity / Invert X / Invert
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

    /// Issue #15: the screen the rebindable layer has been sitting behind
    /// with no way to reach it since it landed. Two hops, matching how a
    /// player would actually navigate there — Controls is not a root-level
    /// page, and Key Binds is nested one level under that.
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
        nav.capture_binding(Binding::Key(KeyCode::KeyF));
        assert!(!nav.awaiting_key_capture(), "the capture is consumed");
        assert_eq!(
            nav.settings().page(),
            crate::menu::options::SettingsPage::KeyBinds,
            "finishing a capture must not itself leave the page"
        );
        let persisted = crate::config::Options::load_from(&options_path);
        assert_eq!(
            persisted.keybinds.binding(InputAction::Forward),
            Binding::Key(KeyCode::KeyF),
            "and it must reach the file immediately, not at exit"
        );
    }

    /// **The gap issue #15's capture patch (`a6da3f6`) existed to close**:
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
    /// `Binding::Key(KeyCode::F1)`, exactly what `app.rs`'s
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
        nav.capture_binding(Binding::Key(KeyCode::F1));
        assert!(!nav.awaiting_key_capture(), "the capture is consumed");
        assert_eq!(
            nav.options().keybinds.binding(InputAction::Forward),
            Binding::Key(KeyCode::F1)
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
            Binding::Key(KeyCode::F1),
            "the persisted name must parse back to the same binding on load"
        );
    }

    /// Escape while capturing cancels the capture and leaves the binding
    /// exactly as it was — vanilla's own `keyPressed` sets `UNKNOWN`
    /// unconditionally on Escape while capturing (`KeyBindsScreen.java`);
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
        nav.capture_binding(Binding::Key(winit::keyboard::KeyCode::KeyP));
        assert_eq!(
            nav.options().keybinds.binding(InputAction::Pause),
            Binding::Key(winit::keyboard::KeyCode::KeyP)
        );
    }

    /// Per-row Reset and the footer's Reset Keys (#15), both persisted
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
            nav.capture_binding(Binding::Key(key));
        }
        assert_eq!(
            nav.options().keybinds.binding(InputAction::Forward),
            Binding::Key(KeyCode::KeyF)
        );

        // Resetting Forward alone must not touch Back.
        let forward_reset =
            key_binds_row(&mut nav, &mut ui, |c| *c == KeyControl::Reset(InputAction::Forward));
        assert_eq!(nav.click(&mut ui, forward_reset), MenuAction::None);
        assert!(nav.options().keybinds.is_default(InputAction::Forward));
        assert_eq!(
            nav.options().keybinds.binding(InputAction::Back),
            Binding::Key(KeyCode::KeyB),
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

    /// #203: the scroll-sensitivity click wraps at vanilla's own slider bounds
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
    /// since #55 it also depends on which page is showing and how far it is
    /// scrolled. If the two disagree the mouse acts on the wrong control, which
    /// is exactly the failure #391 was.
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

    /// Issue #397, and the same coupling `the_settings_rows_are_in_the_order_click_assumes`
    /// guards: [`crate::menu::world_select`]'s focus ids are indices into a `rows`
    /// vector built in `menu::render`, a different file with no compile-time link
    /// to them. If that vector is reordered, the mouse acts on the wrong control
    /// — which is what #391 was.
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
        // Create is live now (issue #190) and does do something: it opens
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

    /// **Pressing Create reaches the app with the typed seed** (issue #190's
    /// queued patch).
    ///
    /// Before this, `apply_create_world` returned `MenuAction::None`
    /// unconditionally — pressing Create updated `CreateWorldNav`'s own
    /// in-memory config and nothing else happened, the same "collected but
    /// read nowhere" shape `MenuAction::Singleplayer` itself was in between
    /// #397 and #287 (see that variant's doc). This drives the real screen
    /// flow — open World Creation, focus the Seed field the same way a click
    /// would, type a seed, click Create — and checks the *action* the app
    /// receives, not `CreateWorldNav::config()` a second time (already
    /// covered by `create_world.rs`'s own
    /// `create_carries_the_typed_name_and_seed`).
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

        // Seed lives on the World tab (issue #567) — click the tab first, the
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
        let MenuAction::Singleplayer(SingleplayerLaunch::Created { world_dir, config }) = action
        else {
            panic!("expected MenuAction::Singleplayer(Created {{ .. }}), got {action:?}");
        };
        assert_eq!(config.seed, "777", "the typed seed must reach the action's payload");
        // Issue #468's reading (2): pressing Create really **creates**. Before
        // this, the action carried a config and no directory, and
        // `begin_singleplayer` opened `saves/world` — so a second Create reopened
        // the first world and the typed seed was silently discarded by
        // `resolve_world_seed`. The directory and its `level.dat` are the proof
        // that stopped being possible.
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

    /// **The owner's report, end to end at the nav layer: Create New World twice
    /// makes two worlds, both are listed, and either can be opened.**
    ///
    /// This is the acceptance condition for issue #468's reading (2) and the
    /// regression gate for the wart reading (1) shipped with — *"Using Create New
    /// World just joins me to the existing world"*. Every step is the real screen
    /// flow (title → list → create → list), so it fails if any hop is unwired
    /// rather than only if `saves.rs` is wrong.
    /// **The whole delete flow, driven through the real screens** (issue #540):
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

    /// A **corrupt** world can be removed, which is the one #540 says you most
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
            let MenuAction::Singleplayer(SingleplayerLaunch::Created { world_dir, .. }) = action
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
            let MenuAction::Singleplayer(SingleplayerLaunch::Open(dir)) = action else {
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
    /// sentinel** (issue #190's queued patch, the random-seed half).
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
        let MenuAction::Singleplayer(SingleplayerLaunch::Created { config, .. }) = action else {
            panic!("expected MenuAction::Singleplayer(Created {{ .. }}), got {action:?}");
        };
        assert_eq!(
            config.seed, "",
            "an untouched Seed field must reach the action as an empty string, matching \
             parse_seed's own random-means-empty branch, not \"0\" or any other default"
        );
    }

    /// **Play Selected World reaches the app** (issue #287).
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

        assert_eq!(
            nav.click(&mut ui, B::Play.row()),
            MenuAction::Singleplayer(SingleplayerLaunch::Open(
                nav.saves_root().join("planted")
            )),
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
        assert_eq!(
            nav.key(&mut ui, MenuKey::Enter),
            MenuAction::Singleplayer(SingleplayerLaunch::Open(
                nav.saves_root().join("planted")
            ))
        );
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
        let mut nav = MenuNav::with_paths(
            std::env::temp_dir().join(format!(
                "lodestone-nav-{}-settingsfail/servers.json",
                std::process::id()
            )),
            std::path::PathBuf::from("/dev/null/nope/options.json"),
            std::env::temp_dir().join(format!(
                "lodestone-nav-{}-settingsfail/profiles.json",
                std::process::id()
            )),
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
        // Issue #167: Advancements is now live, so it is the first stop below
        // Back to Game.
        assert_eq!(nav.pause_button(), PauseButton::Advancements);
        nav.key(&mut ui, MenuKey::Down);
        // Issue #188: Statistics is live too.
        assert_eq!(nav.pause_button(), PauseButton::Statistics);
        nav.key(&mut ui, MenuKey::Down);
        // Issue #189: likewise, Player Reporting rather than Options.
        assert_eq!(nav.pause_button(), PauseButton::PlayerReporting);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.pause_button(), PauseButton::Options);
        nav.key(&mut ui, MenuKey::Down);
        // Issue #535: Options' half-width sibling, vanilla's own singleplayer
        // branch of `createPauseMenu`.
        assert_eq!(nav.pause_button(), PauseButton::OpenToLan);
        nav.key(&mut ui, MenuKey::Down);
        assert_eq!(nav.pause_button(), PauseButton::QuitToTitle);
    }

    /// Issue #535's scope 2, the counterpart to
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
        // (#167/#188/#189 made the three middle stops live).
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

        // Now click Report Bugs (index 3, disabled). This used to probe
        // Advancements at index 1, which #167 made live — the subject has to be a
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
        // dropped from six to five when issue #189 made Player Reporting
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
        // are the disabled rows Down must step over (issues #188/#189 made
        // Statistics and Player Reporting live, #167 Advancements, #535 Open to
        // LAN).
        ui.enter_dev_world();
        ui.pause();
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

    // -- the death screen (issue #103) -------------------------------------

    fn dead(nav_tag: &str) -> (MenuNav, UiState) {
        let (nav, _) = nav(nav_tag);
        let mut ui = UiState::new();
        ui.enter_dev_world();
        ui.die(Some("blew up".to_string()));
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
        // Vanilla's `DeathScreen.shouldCloseOnEsc()` returns `false`
        // (`DeathScreen.java`) — unlike every other screen in this
        // file, Escape here must not even unwind one level, let alone quit.
        let (mut nav, mut ui) = dead("death-escape");
        assert_eq!(nav.key(&mut ui, MenuKey::Escape), MenuAction::None);
        assert_eq!(ui.screen(), Screen::Death);
        assert!(!ui.quit_requested());
    }

    // -- the credits/end-poem screen (#192) ------------------------------------

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

    // -- Social Interactions (#189) --------------------------------------------

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
            lodestone_model::Text::literal("connection lost"),
        ));
        assert_eq!(ui.screen(), Screen::Error);
    }

    // -- the multiplayer list's footer and row actions (#396) -----------------

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
    /// #391 bug it protects against: nothing in the compiler links the two files.
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

    /// #402: arrowing past the bottom of the scroll window scrolls to keep the
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

    /// #402: the mouse wheel scrolls the list too, independently of the
    /// keyboard path above, and clamps at both ends rather than running past
    /// the list.
    ///
    /// **Sign convention (#445):** `notches` is winit's `scrollY` verbatim, so
    /// **positive scrolls up** — the same sign vanilla's
    /// `setScrollAmount(scrollAmount() - scrollY * scrollRate())` uses
    /// (`AbstractScrollArea.java`). This is the *opposite* of the `rows`
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
    /// `AbstractScrollArea.defaultSettings(defaultEntryHeight / 2)`
    /// (`AbstractSelectionList.java`), read back by `scrollRate()`
    /// (`AbstractScrollArea.java`) and applied by `mouseScrolled`
    /// (`:34`). Transcribed from `.cache/mc/26.2/client-src`, not guessed.
    const SCROLL_RATE_PX: f32 = 18.0;

    /// **The player-reported bug (issue #445), as a value rather than a
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
        const V_W: f32 = 854.0;
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
    /// (`AbstractSelectionList.java`) and the click paths, never from
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
    /// `OnlineServerEntry.mouseClicked`'s order (`ServerSelectionList.java`),
    /// and it is also the `MenuNav::click` hazard #395 recorded from the other side:
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
            MenuAction::Connect(entry) => assert_eq!(entry.host, "h0.example"),
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
        // (`ServerSelectionList.java`) — but `click_list` used to
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
            MenuAction::Connect(entry) => assert_eq!(entry.host, "h0.example"),
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
            MenuAction::Connect(entry) => assert_eq!(entry.host, "h0.example"),
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
    /// # This gate was itself vacuous until #474
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
            // Issue #540's confirmation. Needs the `nav` half too: the frame is
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
            // The one that was broken in #474. Needs the `nav` half too — the
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
            // Same shape again — issue #613's `EditBook` remainder. The frame
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
            // click on it would be dropped exactly as #474 dropped every
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
    /// nothing: `Screen.setInitialFocus` (`Screen.java`) runs its whole
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

    /// **The island gate for `click`'s `Screen::Accounts` arm.**
    ///
    /// `AccountsNav::click_name_edit_row` is unit-tested directly, which proves
    /// nothing about whether `MenuNav::click` ever calls it — without the arm,
    /// this file's fall-through translates a click into `hover` + `Enter`, and
    /// `Enter` on the open editor **commits**. So clicking the text field to fix
    /// a typo saved the name, which is #391's shape on a sixth screen.
    ///
    /// Driven through `click`, the function `app.rs`'s mouse handler calls, with
    /// the field row and the Done row measured separately: a gate that only
    /// clicked Done would pass with the arm deleted.
    #[test]
    fn clicking_the_offline_name_field_does_not_save_but_clicking_done_does() {
        use crate::menu::accounts::{NAME_EDIT_DONE_ROW, NAME_EDIT_FIELD_ROW};
        use crate::offline_identity::OfflineIdentity;

        let (mut nav, path) = nav("offline-name-click");
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
