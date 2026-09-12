use super::*;

impl MenuNav {
    pub fn hover(&mut self, ui: &UiState, row: usize) {
        // The gate takes `&UiState`, so unlike `key`/`click` this cannot
        // reconcile the screen — it aims the gate's own cursor instead. Without
        // it, hovering during the window between the first drawn frame and the
        // first key or click (the gate is drawn from `frame_for`, which cannot
        // move `UiState` either) would move the *title screen's* cursor under a
        // gate the player is looking at.
        if self.ownership_gate_blocks(ui) || ui.screen() == Screen::Ownership {
            if row < OWNERSHIP_BUTTONS.len() {
                self.ownership = row;
            }
            return;
        }
        match ui.screen() {
            Screen::MainMenu if row < MAIN_BUTTONS.len() => self.main = row,
            // Two cursors on one screen, and hover drives only one of
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
            // The confirmation screen — hover is not focus here for
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
            // `hover_row` is vanilla's own container-event-handler set-focused call for the
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
            // Statistics (the ordering audit): this screen's only real
            // control is Done, and this arm's own absence was exactly the
            // Create New World bug above, one screen over — `StatsNav::
            // hover_row` records nothing for anything but `DONE_ROW`, since
            // the tab bar's own hover is derived from `MenuFrame::cursor` at
            // draw time (see `stats::hover_row`'s own doc).
            Screen::Statistics => self.stats.hover_row(row),
            Screen::ServerLinks => self.server_links.hover(row),
            // The settings tree now *has* a cursor, so hover moves
            // it — without this arm, a screen
            // with no hover arm had to route a click through `Enter`. Row indices
            // are indices into `SettingsNav::visible`, which is also what
            // `render::frame_for` builds its rows from.
            //
            // Key Binds is a sub-page of this same `Screen`, and it
            // is not an `OptionsList` page — see `SettingsPage::KeyBinds`'s own
            // doc — so its row indices are `KeyBindsNav::visible`'s, a
            // different list from `SettingsNav::visible`. Guarded ahead of the
            // plain arm below rather than inside it, matching how `hover_list`
            // and `world_select.hover` already get their own arms instead of a
            // branch buried in a shared method.
            Screen::Settings if self.settings.page() == crate::menu::options::SettingsPage::KeyBinds => {
                self.settings.key_binds_mut().hover_row(row);
            }
            // Language — same reasoning, one row index space
            // over: row 0 is the search field (always focused, never
            // hovered — see `menu::language::frame`'s doc), so only rows
            // past it move the cursor.
            Screen::Settings if self.settings.page() == crate::menu::options::SettingsPage::Language => {
                if let Some(row) = row.checked_sub(1) {
                    self.settings.language_mut().hover_row(row);
                }
            }
            // Telemetry — no search field here, so (unlike
            // Language) row indices need no offset.
            Screen::Settings if self.settings.page() == crate::menu::options::SettingsPage::Telemetry => {
                self.settings.telemetry_mut().hover_row(row);
            }
            // Resource Packs — same reasoning as Telemetry, no
            // search field, no offset.
            Screen::Settings if self.settings.page() == crate::menu::options::SettingsPage::ResourcePacks => {
                self.settings.packs_mut().hover_row(row);
            }
            Screen::Settings => self.settings.hover_row(row),
            // Social Interactions — the same direct-row rule as the Settings
            // arm above: without this, a click would have to route through
            // `Enter`, which would activate whichever row was previously focused.
            Screen::Social => self.social.hover_row(row),
            Screen::Friends => self.friends.hover_row(row),
            // The command block edit screen — plain hover
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
            // The reading screen — same shape as `BookEdit` above.
            Screen::BookView => {
                if let Some(state) = self.book_view.as_mut() {
                    state.hovered = Some(row);
                }
            }
            // The Spectator Menu — same shape as `BookEdit`/`CommandBlockEdit`
            // above: plain mouse-highlight tracking, no keyboard row cursor.
            Screen::SpectatorMenu => {
                self.spectator_menu.hovered = Some(row);
            }
            _ => {}
        }
    }

    /// A left-click that landed on row `row` of the current screen.
    ///
    /// # Why this is not just `hover` then `Enter`
    ///
    /// That translation is correct for screens whose row cursor is the intended
    /// target, but not for Settings: a click must resolve the row under the
    /// pointer rather than reuse the keyboard meaning of `Enter`.
    ///
    /// Settings owns a cursor over its visible controls. A click updates that
    /// cursor and activates the matching [`super::options::Control`], so GUI
    /// Scale, View Bobbing and every other row receives only its own action.
    ///
    /// # The row indices are a coupling, and it is guarded
    ///
    /// A row index here means whatever `menu::render::frame_for` put in
    /// `Screen::Settings`'s `rows`, which is a different file, and
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
        // The gate reconcile — [`Self::key`]'s, for the other of the two paths
        // every input takes. A click that arrives while the gate is closed must
        // land on the gate, not on whatever row the screen behind it had there.
        if self.ownership_gate_blocks(ui) {
            let was = ui.screen();
            ui.open_ownership_gate();
            // [`Self::key`]'s reasoning: a click aimed at the screen the gate
            // just replaced must not activate a gate button under the cursor.
            if ui.screen() != was {
                self.ownership = 0;
                return MenuAction::None;
            }
        }
        if ui.screen() == Screen::Ownership {
            return self.click_ownership(ui, row);
        }
        // The edit form is the second screen where "hover then Enter" is wrong,
        // and the widget dispatch is what makes it visible. The child click handler
        // focuses the child it hit and calls *its* `onClick`; it does not activate
        // the screen. Translating a click into `Enter` here meant **clicking
        // either address field tried to save the form** — the same shape as the settings
        // one screen over: with a valid address it closed the form the player was
        // still typing into, and without one it flashed "AN ADDRESS IS REQUIRED"
        // at someone who had just clicked the field to fix that.
        if ui.screen() == Screen::ServerEdit {
            return match row {
                NAME_FIELD | ADDRESS_FIELD => {
                    self.form.focus_row(row);
                    MenuAction::None
                }
                // Vanilla's Done/Cancel, now
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
        // insists every cursorless screen gets its own arm: here a
        // click means "focus this field" *or* "press this button", never both,
        // and a click on one of the four disabled buttons means nothing at all.
        // Play Selected World is the one that does something — it launches.
        if ui.screen() == Screen::WorldSelect {
            let outcome = self.world_select.click_row(row);
            return self.apply_world_select(ui, outcome);
        }
        // The confirmation screen. Its own arm for the same reason,
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
        // World Creation — the same shape again: a click focuses a
        // field or presses a button, never "hover then Enter".
        if ui.screen() == Screen::CreateWorld {
            let outcome = self.create_world.click_row(row);
            return self.apply_create_world(ui, outcome);
        }
        if ui.screen() == Screen::Settings {
            // Key Binds again — see `hover`'s matching guard.
            if self.settings.page() == crate::menu::options::SettingsPage::KeyBinds {
                let outcome = self
                    .settings
                    .key_binds_mut()
                    .click_row(row, &self.options.keybinds);
                return self.apply_key_binds(ui, outcome);
            }
            // Language again — row 0 is the always-focused
            // search field, so a click there is a no-op (there is nothing to
            // move focus *to* — see `hover`'s matching guard).
            if self.settings.page() == crate::menu::options::SettingsPage::Language {
                let outcome = match row.checked_sub(1) {
                    Some(row) => self.settings.language_mut().click_row(row),
                    None => crate::menu::language::LanguageOutcome::None,
                };
                return self.apply_language(ui, outcome);
            }
            // Telemetry again — no search field, no offset.
            if self.settings.page() == crate::menu::options::SettingsPage::Telemetry {
                let outcome = self.settings.telemetry_mut().click_row(row);
                return self.apply_telemetry(ui, outcome);
            }
            // Resource Packs again.
            if self.settings.page() == crate::menu::options::SettingsPage::ResourcePacks {
                let outcome = self.settings.packs_mut().click_row(row);
                return self.apply_packs(ui, outcome);
            }
            // A click that hit-tested onto a row this page does not have does
            // nothing at all (`SettingsNav::click_row` returns `None` for an
            // out-of-range row), rather than falling through to the keyboard
            // path — the other half of the direct-click rule.
            let outcome = self.settings.click_row(row);
            return self.apply_settings(ui, outcome);
        }
        // Social Interactions — direct row activation applies here too: a
        // click resolves directly to the row it hit, never through Enter.
        if ui.screen() == Screen::Social {
            let outcome = self.social.click_row(row);
            return self.apply_social(ui, outcome);
        }
        if ui.screen() == Screen::Friends {
            if self.friends.click_row(row) {
                ui.close_friends();
            }
            return MenuAction::None;
        }
        // The fourth. A click on a *row* here is
        // `AbstractSelectionList.mouseClicked` — it selects, and only the favicon's
        // quadrants act — while a click above the rows is one of seven buttons.
        // Routing it as `hover` + `Enter` would join a server on any click on its
        // row, which vanilla reserves for the join icon and the double-click.
        if ui.screen() == Screen::ServerList {
            return self.click_list(ui, row);
        }
        // The command block edit screen — the same fix once more: a
        // click on `CommandBlockRow::Command` is caret placement (a no-op
        // here, see `activate_command_block_row`'s own doc), and a click on
        // any other row is a button press, never routed through `Enter`.
        if ui.screen() == Screen::CommandBlockEdit {
            return self.activate_command_block_row(ui, row);
        }
        // The sign-editing screen follows the same direct-click rule: a click on a line field
        // is caret placement (`app.rs`'s to translate, like the command
        // block's own field), a click on Done is activation, never routed
        // through `Enter` (which this screen repurposes for line navigation).
        if ui.screen() == Screen::SignEdit {
            return self.activate_sign_edit_row(ui, row);
        }
        // The book-editing screen follows the same direct-click rule as `SignEdit`: a
        // click on the title field (while signing) is caret placement, a
        // click on any other row is a button press.
        if ui.screen() == Screen::BookEdit {
            return self.activate_book_edit_row(ui, row);
        }
        // The reading screen — every row is a button (page back, page
        // forward, Done); there is no field on it at all.
        if ui.screen() == Screen::BookView {
            return self.activate_book_view_row(ui, row);
        }
        // The Spectator Menu (`TeleportToEntity` remainder) —
        // Every row is a button (a team category, a
        // player, or Back), never a field.
        if ui.screen() == Screen::SpectatorMenu {
            return self.activate_spectator_menu_row(ui, row);
        }
        // Statistics — the newest instance of the same shape, and it
        // became *necessary* rather than merely tidy when Enter there stopped
        // being unconditional: see `click_statistics`.
        if ui.screen() == Screen::Statistics {
            return self.click_statistics(ui, row);
        }
        // Server Links — every row on both its views is a real button (a
        // link row, Back, or Yes/No), never a field, so direct row activation
        // applies: a click resolves directly to the row it hit.
        if ui.screen() == Screen::ServerLinks {
            return self.click_server_links(ui, row);
        }
        // The same direct-row shape applies only while the account screen's offline-name
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
                AccountsSignal::Back => self.leave_accounts(ui),
                AccountsSignal::None => {}
            }
            return MenuAction::None;
        }
        if ui.screen() == Screen::Accounts {
            return self.click_accounts(ui, row);
        }
        self.hover(ui, row);
        self.key(ui, MenuKey::Enter)
    }

    /// [`Self::click`]'s accounts arm: **a single click focuses a row, a double
    /// click selects it** — the server list's model, reached through the same
    /// [`Self::double_click`] tracker rather than a second one.
    ///
    /// Before this, `Screen::Accounts` had no arm at all and fell through to
    /// `hover` + `Enter`, so one click ran `select` — committing the account
    /// switch and writing `profiles.json` on a click that may only have been
    /// aiming Remove at a row. Focus and activation were the same event, which
    /// is exactly what the server list and the world list both avoid.
    ///
    /// Button rows keep single-click activation, matching the server list's
    /// footer: `click_row` returns `false` for them (and for the sign-in and
    /// name-editor frames, which draw no list), and the fall-through below is
    /// the unchanged path.
    fn click_accounts(&mut self, ui: &mut UiState, row: usize) -> MenuAction {
        if !self.accounts.click_row(row) {
            self.hover(ui, row);
            return self.key(ui, MenuKey::Enter);
        }
        let now_ms = self.click_clock.elapsed().as_millis() as u64;
        if self.double_click.click(now_ms, (Screen::Accounts, row)) {
            self.accounts.select_focused();
        }
        MenuAction::None
    }

    /// [`Self::hover`]'s multiplayer arm. A footer row moves the button
    /// highlight; a **server row does nothing but clear it**, because on a
    /// selection list hover is not selection.
    ///
    /// This used to set `self.server`, so the 1 px row outline followed the mouse
    /// and a server could not stay selected while the cursor travelled to Join. A
    /// player reported it immediately. Vanilla reaches
    /// `AbstractSelectionList.setSelected` only from `setFocused`
    /// and the click paths — never from
    /// hover; vanilla's own server-selection list rendering shows what hover *does* draw,
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

    /// [`Self::click`]'s multiplayer arm.
    ///
    /// The row half is `OnlineServerEntry.mouseClicked`
    /// in vanilla's own order: the join
    /// quadrant first, then the two move quadrants with their index guards, and
    /// **selection last** — a plain click selects and does not join.
    ///
    /// The double-click (`if (doubleClick) this.join()`) is implemented, last in
    /// that order and ungated by the icon quadrants. This doc used to say it was
    /// missing "because `app.rs` reports one click at a time with no interval";
    /// [`super::focus::DoubleClickTracker`] supplies the interval, and the claim
    /// outlived the gap by longer than it was true.
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
                    let Some(auth) = self.entitlement() else {
                        return self.refuse_unowned(ui);
                    };
                    return match self.list.get(row) {
                        Some(entry) => {
                            let entry = entry.clone();
                            ui.begin(SessionKind::Multiplayer);
                            MenuAction::Connect(auth, entry)
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
            // Vanilla's own order: after
            // the icon-quadrant checks above, **unconditionally**,
            // `if (doubleClick) join()` — it fires wherever on the row the
            // click landed, icon or not. `entry_icon_cursor` played no part
            // in reaching this before, and must not gate it now either.
            let now_ms = self.click_clock.elapsed().as_millis() as u64;
            if self.double_click.click(now_ms, (Screen::ServerList, row)) {
                let Some(auth) = self.entitlement() else {
                    return self.refuse_unowned(ui);
                };
                return match self.list.get(row) {
                    Some(entry) => {
                        let entry = entry.clone();
                        ui.begin(SessionKind::Multiplayer);
                        MenuAction::Connect(auth, entry)
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
    /// (vanilla's own server-selection list rendering, `:434-436`).
    ///
    /// The selection **follows the row**, matching vanilla's
    /// `scrollToEntry(children.get(newIndex))`: the entry the player grabbed stays
    /// the selected one, so a second click on the same arrow keeps moving it.
    fn swap_rows(&mut self, from: usize, to: usize) -> MenuAction {
        if !self.list.swap(from, to) {
            return MenuAction::None;
        }
        self.server = to;
        // A swap can carry the selection to the edge of the scrolled
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
            ServerListButton::Select => {
                let Some(auth) = self.entitlement() else {
                    return self.refuse_unowned(ui);
                };
                match self.list.get(self.server) {
                    Some(entry) => {
                        let entry = entry.clone();
                        ui.begin(SessionKind::Multiplayer);
                        MenuAction::Connect(auth, entry)
                    }
                    None => MenuAction::None,
                }
            }
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
}
