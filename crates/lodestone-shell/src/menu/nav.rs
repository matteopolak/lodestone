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

use super::command_block;
use super::edit_box::EditBox;
use super::focus::{self, FocusChildren, FocusSet, FocusTarget, KeyEvent, KeyOutcome};
use super::servers::{MAX_NAME_CHARS, ServerEntry, ServerList, servers_path};
use super::widget;
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
    /// A printable character: a command on the list, text in the form.
    Char(char),
}

/// The one thing the app must do as a result of a keypress.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuAction {
    /// Nothing to do; the menu handled it internally.
    None,
    /// Enter the singleplayer world: start the integrated server in-process and
    /// connect to it (issue #287).
    ///
    /// Two producers: [`Screen::WorldSelect`]'s **Play Selected World** button
    /// (`None` — the bundled world's own seed) and, since issue #190,
    /// [`Screen::CreateWorld`]'s **Create** button (`Some(config)` — the
    /// collected [`crate::menu::create_world::WorldCreationConfig`], whose
    /// `seed` field `app.rs`'s `resolve_launch_seed` resolves ahead of the
    /// bundled world's). `app.rs`'s arm calls `begin_singleplayer`, which
    /// takes the same `Option` this variant carries.
    ///
    /// Between #397 and #287 this variant had **no producer at all** and was
    /// kept as the seam the integrated server would land on. It is worth naming
    /// because "the variant exists and is matched" was true throughout and is
    /// exactly what an island looks like from the inside.
    Singleplayer(Option<crate::menu::create_world::WorldCreationConfig>),
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
    /// new one with a fresh `ServerList` (`JoinMultiplayerScreen.java:167-169`).
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
/// `ManageServerScreen`'s `manageServer.resourcePack` cycle button
/// (`ManageServerScreen.java:43-54`). **Present and inactive**: `ServerEntry`
/// has no `pack_status` field to cycle (see that struct's docs), so wiring
/// this row would be exactly the fabricated persistence `CLAUDE.md`'s
/// evidence standards warn against — same rule as every inactive settings row.
pub const RESOURCE_PACK_ROW: usize = 2;
/// `CommonComponents.GUI_DONE` (`ManageServerScreen.java:55-57`) — saves the
/// form. A real, clickable row alongside the existing Enter/Tab keyboard path
/// (see [`MenuNav::click`]'s `Screen::ServerEdit` arm).
pub const DONE_ROW: usize = 3;
/// `CommonComponents.GUI_CANCEL` (`ManageServerScreen.java:58-62`) — discards
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
/// — it calls `clearFocus()` (`Screen.java:342-347`), so a rebuilt screen has no
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
    /// `Screen.setInitialFocus(GuiEventListener)` (`Screen.java:171-176`) rather
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
        // (`ManageServerScreen.java:14-15`), whose `en_us.json` values are
        // "Server Name"/"Server Address" — which happen to already be what
        // `render.rs`'s (unrelated) `detail` line under each field shows, so
        // this was invisible on screen and only wrong to a screen reader.
        let mut name =
            EditBox::new(name_rect.0, name_rect.1, name_rect.2, name_rect.3, "Server Name")
                .with_max_length(MAX_NAME_CHARS);
        // `nameEdit.setHint(DEFAULT_SERVER_NAME)` (`ManageServerScreen.java:35`),
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
        }
    }

    /// A form pre-filled from `entry`, editing the row at `index`.
    #[must_use]
    pub fn editing(index: usize, entry: &ServerEntry) -> Self {
        let mut form = Self::adding();
        form.fields.name.set_value(&entry.name);
        form.fields.address.set_value(entry.address_label());
        form.editing = Some(index);
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
        ServerEntry::new(name, host, port)
    }

    /// One key, routed through vanilla's `Screen.keyPressed` order: Escape, then
    /// the focused field, then — only if it declined — Tab and the arrows as
    /// focus navigation, and only then the screen's own meaning for the key.
    ///
    /// **That order is why Up/Down move between fields while Left/Right move the
    /// caret**, with no rule anywhere saying so: `EditBox.keyPressed` handles
    /// 262/263 and declines 264/265 (`EditBox.java:279-284`), so the vertical
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
    /// Vanilla's language icon button — `TitleScreen.java:131-136` constructs
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
    /// `TitleScreen.java:137-139`, same direct-construction shape as
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
    #[must_use]
    pub fn enabled(self) -> bool {
        matches!(
            self,
            MainButton::Singleplayer
                | MainButton::Multiplayer
                | MainButton::Options
                | MainButton::Quit
                | MainButton::Accounts
                // Issue #415/#55: both destination screens are built now —
                // see the variants' own docs.
                | MainButton::Language
                | MainButton::Accessibility
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
            PauseButton::BackToGame
                | PauseButton::Options
                | PauseButton::QuitToTitle
                // Issue #189: the screen behind this button is built. See the
                // variant's own doc for what is and is not wired inside it.
                | PauseButton::PlayerReporting
                // Issue #188: likewise.
                | PauseButton::Statistics
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

/// The multiplayer screen's title — `multiplayer.title`'s `en_us` string
/// (`JoinMultiplayerScreen.java:43`), which
/// `HeaderAndFooterLayout.addTitleHeader` centres in the header band.
pub const SERVER_LIST_TITLE: &str = "Play Multiplayer";

/// `JoinMultiplayerScreen`'s seven footer buttons (#396), in the order they are
/// added to the two footer rows (`JoinMultiplayerScreen.java:68-125`) — which is
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
    /// (`JoinMultiplayerScreen.java:28-29,73,89,108,123-125`).
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
/// `DeathScreen.init` (`DeathScreen.java:42-60`). Both live; unlike
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
    /// Which [`SERVER_LIST_BUTTONS`] entry the cursor is over, if any (#396).
    ///
    /// Separate from [`Self::server`] because the two are different cursors that
    /// are visible at once: the selected *server* keeps its outline while a footer
    /// button under the mouse draws highlighted.
    list_button: Option<usize>,
    /// How far the multiplayer list is scrolled down, **in logical pixels** —
    /// vanilla's `AbstractScrollArea.scrollAmount`, which is a `double` and is
    /// subtracted straight from a row's y (`AbstractSelectionList.java:143-150`).
    ///
    /// **This was a `usize` row counter until issue #445**, and that was the
    /// whole of the owner's bug report: one wheel notch is
    /// `scrollY * scrollRate()` where `scrollRate = defaultEntryHeight / 2`
    /// (`AbstractScrollArea.java:34`, `:141-142`, `AbstractSelectionList.java:44`
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
    /// it down (`ServerSelectionList.java:490-515`).
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
    /// The World Creation screen's own widgets, focus and collected config
    /// (issue #190). Held here for the same reason [`Self::form`] is: it owns
    /// real [`EditBox`] state that cannot be rebuilt per frame.
    create_world: crate::menu::create_world::CreateWorldNav,
    /// A double-click on a server row joins it — vanilla's
    /// `ServerSelectionList.java:513-514`, `if (doubleClick) join()`,
    /// unconditional on where in the row the click landed. The primitive is
    /// [`super::focus::DoubleClickTracker`]; this is `click_list`'s only
    /// caller of it.
    double_click: super::focus::DoubleClickTracker<usize>,
    /// The monotonic clock [`Self::double_click`] measures against. An
    /// `Instant` fixed at construction rather than reset per click — only
    /// the *differences* `DoubleClickTracker` computes matter, so nothing
    /// needs rearming.
    click_clock: std::time::Instant,
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
    /// The command tree the connected server sent (issue #471 step 2), pushed
    /// down by `app`'s right-click handler off `net::CommandTreeCell` — this
    /// module is pure and holds no client handle, so it cannot pull it.
    ///
    /// `None` off a live session, or before the server's `minecraft:commands`
    /// arrives, and every consumer treats that as "offer no completions"
    /// rather than as an empty tree. An `Arc` because a real 26.2 server's tree
    /// is ~2,000 nodes: this is a shared read, never a copy.
    command_tree: Option<std::sync::Arc<lodestone_model::command_tree::CommandTree>>,
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
            world_select: crate::menu::world_select::WorldSelectNav::new(),
            list_button: None,
            server_scroll: 0.0,
            menu_cursor: None,
            settings: crate::menu::options::SettingsNav::new(),
            social: crate::menu::social::SocialNav::with_path(hidden_players_path),
            stats: crate::menu::stats::StatsNav::default(),
            create_world: crate::menu::create_world::CreateWorldNav::new(),
            double_click: super::focus::DoubleClickTracker::new(),
            click_clock: std::time::Instant::now(),
            command_block: None,
            command_tree: None,
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

    /// Vanilla's **View Bobbing** option — see
    /// [`crate::config::Options::view_bobbing`]. Read once per presented frame
    /// by `app.rs` and handed to `Sim::set_view_bobbing`.
    #[must_use]
    pub fn view_bobbing(&self) -> bool {
        self.options.view_bobbing
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

    /// The Statistics screen's own state (issue #188).
    #[must_use]
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
            // Issue #445's first adoption. Its offset is pixels, which is the
            // prerequisite this arm exists to assert — see `ListSpec`'s doc.
            super::Screen::Statistics => Some(super::stats::list_spec(
                super::stats::GENERAL_STATS.len(),
                self.stats.scroll(),
            )),
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
            super::Screen::Statistics => {
                let before = self.stats.scroll();
                self.stats.scroll_by(notches, canvas_height);
                self.stats.scroll() != before
            }
            _ => false,
        }
    }

    /// Scrolls the multiplayer list by `notches` of mouse wheel — vanilla's
    /// `AbstractScrollArea::mouseScrolled`,
    /// `setScrollAmount(scrollAmount() - scrollY * scrollRate())`
    /// (`AbstractScrollArea.java:34`).
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
    /// getContentX()` (`ServerSelectionList.java:492-493`) — and it is derived
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
        PAUSE_BUTTONS[self.paused.min(PAUSE_BUTTONS.len() - 1)]
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
            // Two cursors on one screen (#396), and hover drives only one of
            // them: the seven rows above `list.len()` are footer buttons and move
            // a button highlight, while the server entries below it move
            // **nothing**. That is what lets a selected server stay outlined
            // while the cursor travels to Join — see `hover_list`.
            Screen::ServerList => self.hover_list(row),
            Screen::Paused if row < PAUSE_BUTTONS.len() => self.paused = row,
            Screen::Death if row < DEATH_BUTTONS.len() => self.death = row,
            Screen::Accounts => self.accounts.hover(row),
            // The one screen where hover is **not** the row cursor: it records
            // hover alone and leaves focus where it is, or dragging the mouse
            // across the footer would pull the keyboard out of the search field.
            // See `world_select::WorldSelectNav::hovered`.
            Screen::WorldSelect => self.world_select.hover(row),
            // `hover_row` is `ContainerEventHandler.setFocused(child)` for the
            // two text fields — real focus, not a highlight index, because the
            // row indices and `EditForm`'s focus ids are the same numbers (see
            // [`NAME_FIELD`]) — and plain hover tracking for the three button
            // rows the screen's framework conversion added (see
            // [`EditForm::hover_row`]).
            Screen::ServerEdit => self.form.hover_row(row),
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
                // Vanilla's Done/Cancel (`ManageServerScreen.java:55-62`), now
                // real clickable rows since the screen's framework conversion
                // — see `save_entry`/`cancel_edit`, also reached by
                // Enter/Escape so the two paths cannot disagree.
                DONE_ROW => self.save_entry(ui),
                CANCEL_ROW => self.cancel_edit(ui),
                // `RESOURCE_PACK_ROW` (present, inactive — see its doc) and
                // anything past the five rows this screen has: a click does
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
        // Statistics (issue #188) — the newest instance of #391's shape, and it
        // became *necessary* rather than merely tidy when Enter there stopped
        // being unconditional: see `click_statistics`.
        if ui.screen() == Screen::Statistics {
            return self.click_statistics(ui, row);
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
    /// (`AbstractSelectionList.java:298-311`) and the click paths — never from
    /// hover; `ServerSelectionList.java:364-382` shows what hover *does* draw,
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
    /// (`ServerSelectionList.java:490-515`) in vanilla's own order: the join
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
            // Vanilla's own order (`ServerSelectionList.java:490-514`): after
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
    /// (`ServerSelectionList.java:485-488`, `:434-436`).
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
            // The command block edit screen (issue #47) — its own arm for the
            // same reason `Screen::ServerEdit`'s is: a text field needs every
            // keystroke routed to it, which the catch-all below (Escape only)
            // cannot do.
            Screen::CommandBlockEdit => self.key_command_block(ui, key),
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
                        ui.open_world_select();
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
                    // (`TitleScreen.java:131-139`), with `lastScreen = this`
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
    /// (`ManageServerScreen.java:58-62`) and Escape's own meaning on this
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
    /// `ManageServerScreen.java:55-57`) for the same reason
    /// [`Self::cancel_edit`] is shared.
    fn save_entry(&mut self, ui: &mut UiState) -> MenuAction {
        if !self.form.is_valid() {
            // Refuse rather than saving a row that cannot be dialed. Vanilla
            // reaches the same outcome by disabling the Done button instead
            // (`ManageServerScreen.java:92-93`); this screen has no per-row
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
                // (`CommandBlockEditScreen.java:111-114`) — vanilla sends first
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
            WorldSelectOutcome::Play => MenuAction::Singleplayer(None),
            // Issue #190.
            WorldSelectOutcome::CreateWorld => {
                self.create_world = crate::menu::create_world::CreateWorldNav::new();
                ui.open_create_world();
                MenuAction::None
            }
        }
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
            CreateWorldOutcome::Cancel => {
                ui.close_create_world();
                MenuAction::None
            }
            CreateWorldOutcome::Create(config) => MenuAction::Singleplayer(Some(config)),
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
            SettingsOutcome::Cycle(LiveOption::ToggleSneak) => {
                self.toggle_toggle_sneak();
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ToggleSprint) => {
                self.toggle_toggle_sprint();
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
                self.step_chat_option(|o| &mut o.chat_scale, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatWidth) => {
                self.step_chat_option(|o| &mut o.chat_width, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatHeightFocused) => {
                self.step_chat_option(|o| &mut o.chat_height_focused, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatHeightUnfocused) => {
                self.step_chat_option(|o| &mut o.chat_height_unfocused, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatLineSpacing) => {
                self.step_chat_option(|o| &mut o.chat_line_spacing, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::ChatOpacity) => {
                self.step_chat_option(|o| &mut o.chat_opacity, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::TextBackgroundOpacity) => {
                self.step_chat_option(|o| &mut o.chat_background_opacity, 1);
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
                self.step_chat_option(|o| &mut o.sensitivity, 1);
                MenuAction::None
            }
            SettingsOutcome::Cycle(LiveOption::RenderDistance) => {
                self.step_render_distance(1);
                MenuAction::None
            }
        }
    }

    /// Steps `renderDistance` by one chunk and wraps, then persists.
    ///
    /// **Wraps rather than saturating**, matching every other live control on
    /// this tree (`cycle_gui_scale`'s `rem_euclid`, `cycle_mouse_wheel_sensitivity`'s
    /// period): a click is the only way to move these rows, so a value parked at
    /// the maximum has to be able to come back down. Vanilla drags instead and
    /// therefore needs no wrap at all — this is a consequence of departure 1, not
    /// a transcription of `IntRangeBase::next` (`OptionInstance.java:287-289`),
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

    /// Steps one `UnitDouble`-backed chat option and persists it eagerly.
    ///
    /// Takes a field selector rather than being written out eight times: every
    /// one of these options has an identical `[0, 1]` domain and an identical
    /// wrap, so the only thing that varies is which field is being moved. The
    /// per-option *semantics* (the pixel and percent mappings, the OFF caption)
    /// live in `menu::options::live_value`, where the vanilla stringifier they
    /// come from is cited.
    fn step_chat_option(
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
                    PauseButton::Advancements
                    | PauseButton::ReportBugs
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
    /// `false` (`DeathScreen.java:64-66`): the only way off this screen is a
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
    if ui.is_settings() && ui.settings_in_world() {
        return Some(crate::menu::options::settings_frame(
            nav.settings(),
            nav.options(),
            nav.options_save_error(),
        ));
    }
    // The fourth overlay screen (#474), and the second instance of the exact
    // shape above. `command_block_overlay_frame` is the *same call* the draw
    // path in `app/redraw.rs` makes — see its own doc for why it is a function
    // rather than a second construction here.
    if let Some(frame) = command_block_overlay_frame(ui, nav) {
        return Some(frame);
    }
    super::render::frame_for(ui, nav, statuses, favicons)
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
    /// `clearFocus()`-then-retry (`Screen.java:139-143`) and not `(i + 1) % n` —
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
        // Resource Packs, Done, Cancel (`ManageServerScreen.java:43-62`).
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
        // `ManageServerScreen.java:14-16,33-38`: `manageServer.enterName`/
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
    /// *its own* page directly (vanilla's `TitleScreen.java:131-139`
    /// constructs `LanguageSelectScreen`/`AccessibilityOptionsScreen` with
    /// `lastScreen = this`, never through `OptionsScreen`), and Escape from
    /// there must leave in **one** step, straight back to the title — not
    /// two, via the root grid — which is what an empty page stack
    /// (`SettingsNav::open_at`) buys over the grid button's push-from-Root.
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
    /// (`ChatComponent.java:416-420`), so `1.0` is 320px and `0.0` is 40px, and
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

        // …and the row beside it, which we do not honour, does nothing.
        let inert = settings_row(&mut nav, &mut ui, is_option("inactivityFpsLimit"));
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

    /// #202: clicking Sneak/Sprint's rows on the Controls page toggles their
    /// hold/toggle mode and persists immediately, isolated from each other
    /// and from an inactive neighbour — same shape as
    /// [`clicking_a_settings_row_acts_on_that_row_and_no_other`], scoped to
    /// the two new live rows.
    #[test]
    fn clicking_sneak_or_sprint_toggles_only_that_ones_mode() {
        let (mut nav, path) = self::nav("settings-toggle-sneak-sprint");
        let mut ui = UiState::new();
        ui.open_settings();
        let options_path = path.parent().unwrap().join("options.json");

        assert!(!nav.toggle_sneak(), "vanilla's own default is hold");
        assert!(!nav.toggle_sprint());

        open_settings_page(&mut nav, &mut ui, crate::menu::options::SettingsPage::Controls);
        let sneak = settings_row(&mut nav, &mut ui, is_option("toggleCrouch"));
        assert_eq!(nav.click(&mut ui, sneak), MenuAction::None);
        assert!(nav.toggle_sneak(), "the clicked row must flip");
        assert!(!nav.toggle_sprint(), "and not its neighbour");
        assert!(crate::config::Options::load_from(&options_path).toggle_sneak);
        assert!(!crate::config::Options::load_from(&options_path).toggle_sprint);

        let sprint = settings_row(&mut nav, &mut ui, is_option("toggleSprint"));
        assert_ne!(sprint, sneak);
        assert_eq!(nav.click(&mut ui, sprint), MenuAction::None);
        assert!(nav.toggle_sprint());
        assert!(nav.toggle_sneak(), "sprint's click must not un-flip sneak");

        // An inactive neighbour (Attack/Destroy) does nothing.
        let attack = settings_row(&mut nav, &mut ui, is_option("toggleAttack"));
        assert_eq!(nav.click(&mut ui, attack), MenuAction::None);
        assert!(nav.toggle_sneak());
        assert!(nav.toggle_sprint());
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
    /// unconditionally on Escape while capturing (`KeyBindsScreen.java:73-74`);
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
        use crate::menu::create_world::{CREATE_ROW, SEED_FIELD};
        use crate::menu::world_select::WorldSelectButton as B;

        let (mut nav, _) = self::nav("create-world-seed");
        let mut ui = UiState::new();
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(ui.screen(), Screen::WorldSelect, "premise");
        assert_eq!(nav.click(&mut ui, B::Create.row()), MenuAction::None);
        assert_eq!(ui.screen(), Screen::CreateWorld, "premise: World Creation is open");

        assert_eq!(
            nav.click(&mut ui, SEED_FIELD),
            MenuAction::None,
            "focusing the Seed field must not itself produce an action"
        );
        type_str(&mut nav, &mut ui, "777");

        let action = nav.click(&mut ui, CREATE_ROW);
        let MenuAction::Singleplayer(Some(config)) = action else {
            panic!("expected MenuAction::Singleplayer(Some(config)), got {action:?}");
        };
        assert_eq!(config.seed, "777", "the typed seed must reach the action's payload");
        assert_eq!(
            ui.screen(),
            Screen::CreateWorld,
            "the nav layer must not leave the screen; begin_singleplayer does that"
        );
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
        let action = nav.click(&mut ui, CREATE_ROW);
        let MenuAction::Singleplayer(Some(config)) = action else {
            panic!("expected MenuAction::Singleplayer(Some(config)), got {action:?}");
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
    #[test]
    fn play_selected_world_asks_the_app_to_start_singleplayer() {
        use crate::menu::world_select::WorldSelectButton as B;
        let (mut nav, _) = self::nav("world-select-play");
        let mut ui = UiState::new();
        nav.key(&mut ui, MenuKey::Enter);
        assert_eq!(ui.screen(), Screen::WorldSelect, "premise: the list is open");

        assert_eq!(
            nav.click(&mut ui, B::Play.row()),
            MenuAction::Singleplayer(None),
            "Play Selected World must ask the app to launch"
        );
        assert_eq!(
            ui.screen(),
            Screen::WorldSelect,
            "the nav layer must not leave the list; `begin_singleplayer` does that"
        );

        // The keyboard path is the same action, not a second implementation:
        // Tab off the search field lands on Play (registration order), and Enter
        // presses it.
        let (mut nav, _) = self::nav("world-select-play-keys");
        let mut ui = UiState::new();
        nav.key(&mut ui, MenuKey::Enter);
        nav.key(&mut ui, MenuKey::Tab);
        assert_eq!(nav.world_select().focused_row(), Some(B::Play.row()));
        assert_eq!(nav.key(&mut ui, MenuKey::Enter), MenuAction::Singleplayer(None));
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
        // Issue #188: Statistics is now live, so it is the next stop rather
        // than Player Reporting.
        assert_eq!(nav.pause_button(), PauseButton::Statistics);
        nav.key(&mut ui, MenuKey::Down);
        // Issue #189: likewise, Player Reporting rather than Options.
        assert_eq!(nav.pause_button(), PauseButton::PlayerReporting);
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
        // BackToGame -> Statistics -> Player Reporting -> Options (#188/#189
        // made the two middle stops live).
        nav.key(&mut ui, MenuKey::Down);
        nav.key(&mut ui, MenuKey::Down);
        nav.key(&mut ui, MenuKey::Down);
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

        // Pause screen: Back to Game, Statistics, Player Reporting, Options,
        // Disconnect (issues #188/#189 made Statistics and Player Reporting
        // live).
        ui.enter_dev_world();
        ui.pause();
        let mut seen = vec![nav.pause_button()];
        for _ in 0..4 {
            nav.key(&mut ui, MenuKey::Down);
            seen.push(nav.pause_button());
        }
        assert_eq!(
            seen,
            vec![
                PauseButton::BackToGame,
                PauseButton::Statistics,
                PauseButton::PlayerReporting,
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
        // (`DeathScreen.java:64-66`) — unlike every other screen in this
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
        ui.session_failed("connection lost");
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
    /// (`AbstractScrollArea.java:34`). This is the *opposite* of the `rows`
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
    /// (`AbstractSelectionList.java:44`), read back by `scrollRate()`
    /// (`AbstractScrollArea.java:141-142`) and applied by `mouseScrolled`
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
    /// (`AbstractSelectionList.java:298-311`) and the click paths, never from
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
    /// `OnlineServerEntry.mouseClicked`'s order (`ServerSelectionList.java:490-515`),
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
        // (`ServerSelectionList.java:490-514`) — but `click_list` used to
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
            // The one that was broken in #474. Needs the `nav` half too — the
            // frame is built from `MenuNav::command_block`, so a `UiState` on
            // this screen with no widget state is not the production state.
            ("CommandBlockEdit", |ui, nav| {
                ui.enter_dev_world();
                nav.open_command_block(ui, command_block::CommandBlockOpen::default());
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
    /// nothing: `Screen.setInitialFocus` (`Screen.java:161-169`) runs its whole
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
        assert_eq!(frame.rows.len(), 1, "premise: Done is the only control");
        assert_eq!(frame.rows[0].label, "Done", "premise: and it is row 0");
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
}
