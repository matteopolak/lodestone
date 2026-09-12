use super::*;

impl MenuNav {
    pub fn key(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        // **The gate reconcile**, and it is here rather than at each play verb
        // because this is one of the two places every keystroke passes through.
        // Sampled before the dispatch below, so a key pressed on a screen the
        // gate blocks lands on the gate rather than on the screen it was aimed
        // at. See `ownership_gate_blocks` for the two exempt screens.
        if self.ownership_gate_blocks(ui) {
            let was = ui.screen();
            ui.open_ownership_gate();
            // **Swallow the keystroke that moved the screen.** It was aimed at
            // whatever was showing before, and letting it through would make the
            // very first Enter after launch both reveal the gate and press its
            // first button — a screen the player never saw and never chose.
            // A key pressed while the gate is *already* up falls through.
            if ui.screen() != was {
                self.ownership = 0;
                return MenuAction::None;
            }
        }
        match ui.screen() {
            Screen::Ownership => self.key_ownership(ui, key),
            Screen::MainMenu => self.key_main(ui, key),
            Screen::ServerList => self.key_list(ui, key),
            Screen::ServerEdit => self.key_edit(ui, key),
            Screen::WorldSelect => self.key_world_select(ui, key),
            // World Creation — same reasoning as
            // `Screen::WorldSelect`'s own arm above.
            Screen::CreateWorld => self.key_create_world(ui, key),
            // The confirmation screen. Escape here is the *negative
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
            // through to the catch-all below: that catch-all
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
            // The credits/end-poem screen — also exactly one
            // affordance, its own arm for the same reason `Screen::Error`'s
            // is: routing through the catch-all below would call
            // `UiState::on_escape` on Escape, which is the wrong exit (this
            // screen leaves through `quit_to_title`, matching
            // `PauseButton::QuitToTitle`/`DeathButton::TitleScreen`, not
            // through the ordinary menu-stack unwind).
            Screen::Credits => self.key_credits(ui, key),
            // Social Interactions has a real cursor and a real
            // "back", unlike `Screen::Credits` — its own arm rather than the
            // catch-all below for the same reason `Screen::Settings`'s is:
            // that catch-all's Escape goes through `UiState::on_escape`,
            // which would work here too (its `Screen::Social` arm calls
            // `close_social`), but routing every key through `key_social`
            // keeps Up/Down/Enter and Escape's screen-specific meaning in one
            // place instead of splitting it across two functions.
            Screen::Social => self.key_social(ui, key),
            Screen::Friends => self.key_friends(ui, key),
            // Statistics — its own arm for the same reason
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
            // The command block edit screen — its own arm for the
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
            // The reading screen — its own arm because it binds two keys to
            // page turns, which the catch-all below, being Escape-only,
            // would drop.
            Screen::BookView => self.key_book_view(ui, key),
            // The Spectator Menu (`TeleportToEntity`
            // remainder) — its own arm for the same reason as its siblings
            // above: the catch-all's `UiState::on_escape` would work too
            // (its `Screen::SpectatorMenu` arm calls
            // `close_spectator_menu`), but routing Escape through here keeps
            // it in one place with the row-click path.
            Screen::SpectatorMenu => self.key_spectator_menu(ui, key),
            // Escape is the only menu key that means anything on the world and
            // loading screens, and `UiState` already owns it.
            _ => {
                if key == MenuKey::Escape {
                    // Sampled *before* `on_escape`, which is what moves the
                    // screen: afterwards there is no way to tell a cancelled
                    // connect from any other unwind, and only that one leaves a
                    // half-open session for the app to tear down.
                    let was_connecting = ui.screen() == Screen::Connecting;
                    ui.on_escape();
                    if ui.quit_requested() {
                        return MenuAction::Quit;
                    }
                    // Guarded on the screen having actually moved, so a future
                    // `cancel_connect` guard that declines cannot make the app
                    // tear down a session that is still live.
                    if was_connecting && ui.screen() != Screen::Connecting {
                        return MenuAction::CancelConnect;
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
                    // does not start a world.
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
                        // to already be scrolled to.
                        self.server_scroll = 0.0;
                        self.clamp_server();
                        MenuAction::Reprobe(None)
                    }
                    MainButton::Options => {
                        // Vanilla builds a **new** `OptionsScreen` every time
                        // (vanilla's own title-screen rendering's `setScreen(new OptionsScreen(…))`),
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
                    //, with `lastScreen = this`
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
                    MainButton::Friends => {
                        self.friends.reset();
                        ui.open_friends_from_title();
                        MenuAction::None
                    }
                    // Unreachable — every variant below is disabled above.
                    // Spelled out instead of `_` so making one of them *enabled*
                    // without giving it an action is a compile-visible mistake
                    // rather than a silently dead button.
                    MainButton::Realms => MenuAction::None,
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
                // Without this, arrowing past the bottom of the visible
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
                    let Some(auth) = self.entitlement() else {
                        return self.refuse_unowned(ui);
                    };
                    let entry = entry.clone();
                    ui.begin(SessionKind::Multiplayer);
                    MenuAction::Connect(auth, entry)
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
    /// and Escape's own meaning on this
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
    /// vanilla's own manage-server screen rendering) for the same reason
    /// [`Self::cancel_edit`] is shared.
    fn save_entry(&mut self, ui: &mut UiState) -> MenuAction {
        if !self.form.is_valid() {
            // Refuse rather than saving a row that cannot be dialed. Vanilla
            // reaches the same outcome by disabling the Done button instead
            //; this screen has no per-row
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

    /// The command block edit screen.
    ///
    /// Unlike [`Self::key_edit`], this does **not** route every key through a
    /// shared `handle_key` first: the screen has exactly one keyboard focus
    /// target (see [`command_block::CommandBlockState`]'s own doc on why
    /// "Previous Output" is not a second one), so there is no focus layer to
    /// arbitrate — every key already knows where it goes. Left/Right/Home/End
    /// arrive as [`MenuKey::Edit`] and are forwarded with the rest of the
    /// edit keys, in the same arm, since `from_menu_key` hands that variant's
    /// event straight back.
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
            // The tree the server sent is in `self.command_tree`,
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
            MenuKey::SelectAll
            | MenuKey::Copy
            | MenuKey::Cut
            | MenuKey::Paste
            | MenuKey::Edit(_) => {
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
                // — vanilla sends first
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
            // unconditionally — see the module
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
            MenuKey::SelectAll
            | MenuKey::Copy
            | MenuKey::Cut
            | MenuKey::Paste
            | MenuKey::Edit(_) => {
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

    /// The Spectator Menu (`TeleportToEntity` remainder). No
    /// keyboard row cursor — see [`spectator_menu::SpectatorMenuState::hovered`]'s
    /// own doc for why this is mouse-only, the same shape
    /// [`Self::key_book_edit`]'s doc names for its own screen's simplest
    /// layout. Escape closes without sending anything, matching
    /// `Screen.keyPressed`'s un-overridden default for every vanilla screen
    /// that has no explicit `onClose` — same reasoning
    /// [`Screen::BookEdit`]'s own doc gives.
    fn key_spectator_menu(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        if key == MenuKey::Escape {
            self.close_spectator_menu(ui);
        }
        MenuAction::None
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
            MenuKey::SelectAll
            | MenuKey::Copy
            | MenuKey::Cut
            | MenuKey::Paste
            | MenuKey::Edit(_) => {
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

    /// Keys on the signed-book reading screen. Escape and Done close it;
    /// when it belongs to a lectern the close is also sent to the server.
    /// Up/Down turn the page. Every other key is inert: there is no field on
    /// this screen to type into.
    ///
    /// **Up/Down are an approximation, and the divergence is named rather
    /// than hidden**: `BookViewScreen.keyPressed` binds GLFW `266`/`267` —
    /// Page Up and Page Down — to the back and forward buttons, and
    /// [`MenuKey`] carries no variant for either. (Left/Right/Home/End used
    /// to be the same gap and no longer are — they travel as
    /// [`MenuKey::Edit`]; Page Up/Down could go the same way, but only this
    /// screen wants them and no text field does.)
    /// The arrow keys are the nearest thing this shell can express and are
    /// otherwise unused on this screen, so nothing is shadowed by taking
    /// them. Wiring the real pair means adding them to [`MenuKey`], which is
    /// a keyboard-layer change and not this screen's to make.
    fn key_book_view(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match key {
            MenuKey::Escape => {
                let window_id = self.book_view.as_ref().and_then(book_view::BookViewState::lectern_window_id);
                self.close_book_view(ui);
                window_id.map_or(MenuAction::None, |window_id| MenuAction::CloseContainer { window_id })
            }
            MenuKey::Up => {
                if let Some(state) = self.book_view.as_mut() {
                    if !state.can_page_back() {
                        return MenuAction::None;
                    }
                    state.page_back();
                    return state.lectern_page_action().map_or(MenuAction::None, |(window_id, button_id)| {
                        MenuAction::ContainerButtonClick { window_id, button_id }
                    });
                }
                MenuAction::None
            }
            MenuKey::Down => {
                if let Some(state) = self.book_view.as_mut() {
                    if !state.can_page_forward() {
                        return MenuAction::None;
                    }
                    state.page_forward();
                    return state.lectern_page_action().map_or(MenuAction::None, |(window_id, button_id)| {
                        MenuAction::ContainerButtonClick { window_id, button_id }
                    });
                }
                MenuAction::None
            }
            _ => MenuAction::None,
        }
    }

    /// What a `change_page` click event on a book page does — the click-event
    /// twin of [`Self::activate_book_view_row`]'s two page rows, and the same
    /// lectern fork: a hand-held book turns its page locally, while a lectern
    /// reader's page belongs to the server and so reports a button click.
    ///
    /// The page number is 1-based; see
    /// [`book_view::BookViewState::force_page`].
    pub fn book_view_change_page(&mut self, page: i32) -> MenuAction {
        let Some(state) = self.book_view.as_mut() else {
            return MenuAction::None;
        };
        if !state.force_page(page) {
            return MenuAction::None;
        }
        state.lectern_page_action().map_or(MenuAction::None, |(window_id, button_id)| {
            MenuAction::ContainerButtonClick { window_id, button_id }
        })
    }

    /// Mutable access to the reading screen's state, for the app's own
    /// pointer plumbing — the cursor the page hit-test needs is recorded on
    /// the state, not on this type (see
    /// [`book_view::BookViewState::set_page_cursor`]).
    pub fn book_view_mut(&mut self) -> Option<&mut book_view::BookViewState> {
        self.book_view.as_mut()
    }

    /// What clicking a row on the signed-book reading screen does. Shared by
    /// [`Self::click`]'s `BookView` arm.
    ///
    /// The two page rows are guarded on the state's own
    /// `can_page_back`/`can_page_forward` rather than only on the frame's
    /// `enabled` flag, so a click that arrives against a stale frame cannot
    /// walk off either end — `BookViewScreen` achieves the same by hiding
    /// the buttons outright (`updateButtonVisibility`).
    fn activate_book_view_row(&mut self, ui: &mut UiState, row: usize) -> MenuAction {
        let Some(state) = self.book_view.as_mut() else {
            return MenuAction::None;
        };
        match row {
            book_view::page_row::PREVIOUS if state.can_page_back() => {
                state.page_back();
                return state.lectern_page_action().map_or(MenuAction::None, |(window_id, button_id)| {
                    MenuAction::ContainerButtonClick { window_id, button_id }
                });
            }
            book_view::page_row::NEXT if state.can_page_forward() => {
                state.page_forward();
                return state.lectern_page_action().map_or(MenuAction::None, |(window_id, button_id)| {
                    MenuAction::ContainerButtonClick { window_id, button_id }
                });
            }
            book_view::page_row::DONE => {
                let window_id = state.lectern_window_id();
                self.close_book_view(ui);
                return window_id.map_or(MenuAction::None, |window_id| MenuAction::CloseContainer { window_id });
            }
            _ => {}
        }
        MenuAction::None
    }

    /// What clicking a row on the Spectator Menu does (the target is selected by
    /// `TeleportToEntity` remainder) — dispatched entirely by
    /// [`spectator_menu::SpectatorMenuState::activate`], which already knows
    /// whether `row` means "expand a category", "go back", or "teleport".
    /// Shared by [`Self::click`]'s `SpectatorMenu` arm.
    fn activate_spectator_menu_row(&mut self, ui: &mut UiState, row: usize) -> MenuAction {
        match self.spectator_menu.activate(row) {
            spectator_menu::SpectatorMenuOutcome::None => MenuAction::None,
            spectator_menu::SpectatorMenuOutcome::Teleport(target) => {
                self.close_spectator_menu(ui);
                MenuAction::TeleportToEntity { target }
            }
        }
    }

    /// The world-select screen. **Every key goes through
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
    /// `MenuNav` state; `CreateWorld` arm needs to reset
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
                let Some(auth) = self.singleplayer_permit() else {
                    return self.refuse_unowned(ui);
                };
                match crate::saves::world_dir_in(&self.saves_root, &dir_name) {
                    Some(dir) => MenuAction::Singleplayer(auth, SingleplayerLaunch::Open(dir)),
                    None => {
                        self.world_select
                            .set_error(format!("{dir_name:?} is not a world folder"));
                        MenuAction::None
                    }
                }
            }
            // World creation starts from a fresh navigation state.
            WorldSelectOutcome::CreateWorld => {
                self.create_world = crate::menu::create_world::CreateWorldNav::new();
                ui.open_create_world();
                MenuAction::None
            }
            // **Nothing is deleted here.** This arm only opens the
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

    /// The confirmation screen. Every key goes through
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
    /// outside its own `if (result)` — so the
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
                // .onWorldDeleteFailure`, and
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

    /// The World Creation screen. Every key is routed through
    /// [`crate::menu::create_world::CreateWorldNav::handle_key`], which
    /// already implements vanilla's `Screen.keyPressed` order (Escape, then
    /// the focused field, then Tab/arrow navigation, then Enter on whatever
    /// is focused) — this arm only decides what leaving the screen means.
}

