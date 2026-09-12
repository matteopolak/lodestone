use super::*;

impl MenuNav {
    pub fn open_to_lan_available(&self) -> bool {
        MULTIPLAYER_ENABLED && self.has_singleplayer_server && !self.lan_published
    }

    /// The pause menu's active row list: [`PAUSE_BUTTONS_PUBLISHED`] once
    /// [`Self::open_to_lan_available`] is `false` (published, or not a
    /// singleplayer session at all), [`PAUSE_BUTTONS`] otherwise. Every
    /// internal user of a pause-screen row — [`Self::pause_button`], the
    /// hover/click hit test, and the keyboard walk — reads through this
    /// rather than the two constants directly, so they cannot drift onto
    /// different lists.
    ///
    /// **A `Vec`, not `&'static [PauseButton]` any more.** [`PauseButton::
    /// ServerLinks`] is appended here rather than living in
    /// [`PAUSE_BUTTONS`]/[`PAUSE_BUTTONS_PUBLISHED`], because its presence
    /// depends on session data (whether the server announced any links) that
    /// those two `const` arrays cannot express — see that variant's own doc.
    #[must_use]
    pub fn pause_buttons(&self) -> Vec<PauseButton> {
        let base: &[PauseButton] = if self.open_to_lan_available() {
            &PAUSE_BUTTONS
        } else {
            &PAUSE_BUTTONS_PUBLISHED
        };
        let mut buttons = base.to_vec();
        if self.server_links.has_links() {
            buttons.push(PauseButton::ServerLinks);
        }
        buttons
    }

    /// Opens the command block edit screen with `open`'s data —
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

    /// Opens the signed-book reading screen with `open`'s pages, read off
    /// the `minecraft:written_book` in hand. Only from [`Screen::Playing`],
    /// matching [`Self::open_book_edit`]'s own guard: this screen is
    /// client-local too, and reached by the other branch of the same fork.
    pub fn open_book_view(&mut self, ui: &mut UiState, open: book_view::BookViewOpen) {
        if ui.screen() == Screen::Playing {
            self.book_view = Some(book_view::BookViewState::new(open));
            ui.open_book_view();
        }
    }

    /// Opens the server-owned lectern reader. Its page is supplied by the
    /// lectern menu's first data property; page turns therefore return
    /// [`MenuAction::ContainerButtonClick`] rather than being purely local.
    pub fn open_lectern_book_view(
        &mut self,
        ui: &mut UiState,
        window_id: i32,
        open: book_view::BookViewOpen,
        page: i32,
    ) {
        if ui.screen() == Screen::Playing {
            self.book_view = Some(book_view::BookViewState::lectern(open, window_id, page));
            ui.open_book_view();
        }
    }

    /// Closes the signed-book reading screen. Unlike
    /// [`Self::close_book_edit`] there is nothing for a caller to take
    /// first: no exit from this screen sends anything.
    pub fn close_book_view(&mut self, ui: &mut UiState) {
        self.book_view = None;
        ui.close_book_view();
    }

    /// Opens the Spectator Menu at its root view. Only from
    /// [`Screen::Playing`], matching [`Self::open_book_edit`]'s own guard.
    /// Unlike `open_book_edit` this needs no payload from the caller — the
    /// roster is already live via [`Self::refresh_spectator_menu`] — but it
    /// does reset the view (see [`spectator_menu::SpectatorMenuState::reset_view`])
    /// so a menu closed mid-category-browse does not reopen already-expanded.
    pub fn open_spectator_menu(&mut self, ui: &mut UiState) {
        if ui.screen() == Screen::Playing {
            self.spectator_menu.reset_view();
            ui.open_spectator_menu();
        }
    }

    /// Closes the Spectator Menu, whether Escape or a real teleport
    /// selection triggered it — matching [`Self::close_book_edit`]'s own
    /// "either way" phrasing. The roster itself is **not** cleared (unlike
    /// [`Self::close_book_edit`]'s state drop): it stays live-refreshed so
    /// the next open is never stale, see [`Self::spectator_menu`]'s own
    /// field doc.
    pub fn close_spectator_menu(&mut self, ui: &mut UiState) {
        ui.close_spectator_menu();
    }

    /// The last persistence failure, if any.
    #[must_use]
    pub fn save_error(&self) -> Option<&str> {
        self.save_error.as_deref()
    }

    /// The account list + sign-in flow state.
    #[must_use]
    pub fn accounts(&self) -> &crate::menu::accounts::AccountsNav {
        &self.accounts
    }

    /// The world-select screen's widgets and focus.
    #[must_use]
    pub fn world_select(&self) -> &crate::menu::world_select::WorldSelectNav {
        &self.world_select
    }

    /// The live confirmation screen — what it asks and what it will
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
    /// and never consults `active`, while
    /// `WidgetSprites::get(active, focused)` returns `button_disabled` whichever
    /// way `focused` went — so a greyed-out button
    /// under the cursor looks greyed-out, not highlighted. The half that matters
    /// is the *click*: `key_main`/`key_paused` refuse Enter on a disabled button,
    /// which is why moving the highlight onto one here is safe.
}

