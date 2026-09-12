use super::*;

impl MenuNav {
    fn key_accounts(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        use crate::menu::accounts::AccountsSignal;
        match self.accounts.handle_key(key) {
            AccountsSignal::Back => self.leave_accounts(ui),
            AccountsSignal::None => {}
        }
        MenuAction::None
    }

    /// Leave [`Screen::Accounts`] to whichever screen is legal to leave to.
    ///
    /// The account screen is the one screen the ownership gate exempts, so it is
    /// also the one screen that can be left into a *closed* gate — a player who
    /// removed their last account has to land back on the gate, not on a title
    /// screen the reconcile would only bounce them off at the next keystroke.
    ///
    /// Both exits (Escape and the Cancel button) go through here, so they cannot
    /// disagree about where "back" is.
    fn leave_accounts(&mut self, ui: &mut UiState) {
        ui.close_accounts();
        if self.ownership_gate_blocks(ui) {
            self.ownership = 0;
            ui.open_ownership_gate();
        }
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
                    // Player Reporting opens a fresh screen, not a resumed one — the
                    // same "reset on every entry" rule `PauseButton::Options`
                    // follows above, so re-opening it never resumes scrolled
                    // down onto a stale roster.
                    PauseButton::PlayerReporting => {
                        self.social.reset();
                        ui.open_social_from_pause();
                        MenuAction::None
                    }
                    PauseButton::Friends => {
                        self.friends.reset();
                        ui.open_friends_from_pause();
                        MenuAction::None
                    }
                    // Statistics follows the same "reset on every entry" rule as
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
                    // Advancements follows the same shape as the two above. Its state
                    // is reset on entry so a reopened screen starts on the
                    // default tab with each tab freshly centred, matching
                    // vanilla's per-screen `AdvancementTab` lifetime.
                    PauseButton::Advancements => {
                        self.advancements = crate::menu::advancements::AdvancementsState::default();
                        ui.open_advancements_from_pause();
                        MenuAction::None
                    }
                    // `MenuNav` holds no `Sim` and no world path, so
                    // the publish itself is the app's — same division of labour
                    // as `Respawn` and `Singleplayer`.
                    PauseButton::OpenToLan => MenuAction::OpenToLan,
                    PauseButton::ReportBugs
                    | PauseButton::Feedback => MenuAction::None,
                }
            }
            MenuKey::Escape => {
                ui.on_escape();
                MenuAction::None
            }
            _ => MenuAction::None,
        }
    }

    /// The death screen: Up/Down move the highlight between the
    /// two widgets, Enter activates the highlighted one. Both are always
    /// enabled (see [`DeathButton`]'s docs), so this wraps with
    /// [`wrap_prev`]/[`wrap_next`] rather than [`step_enabled`] — there is no
    /// disabled row to step over, unlike [`key_main`](Self::key_main)/
    /// [`key_paused`](Self::key_paused).
    ///
    /// **Escape is deliberately absent from this match** — it falls to `_`,
    /// which does nothing. Vanilla's own death-screen should-close-on-esc check returns
    /// `false`: the only way off this screen is a
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

    /// The credits/end-poem screen. One control (Done), no
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

    /// The Social Interactions screen. Up/Down/Enter mirror
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

    /// Friends is a credential-free menu consumer. It only queues a refresh or
    /// an already-supported relationship change; the app owns forwarding those
    /// intents to the private service worker. While the profile-name prompt is
    /// active, it receives printable keys before screen navigation so the
    /// existing `FriendMutation::SendByName` service path has a real producer.
    fn key_friends(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        if self.friends.is_adding() {
            self.friends.handle_add_key(key);
            return MenuAction::None;
        }
        match key {
            MenuKey::Up => self.friends.step(false),
            MenuKey::Down | MenuKey::Tab => self.friends.step(true),
            MenuKey::Enter => {
                if self.friends.enter() {
                    ui.close_friends();
                }
            }
            MenuKey::Escape => ui.close_friends(),
            _ => {}
        }
        MenuAction::None
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

    /// The Statistics screen. No selection/activation at all on
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
    /// the same reason one screen further: with Enter now gated on focus (see
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
                if self.server_links.returns_to_chat() {
                    self.server_links.reset();
                    ui.close_server_links_to_chat();
                } else {
                    ui.close_server_links();
                }
                MenuAction::None
            }
            // Vanilla's `ConfirmLinkScreen` always returns to the screen it
            // was opened over regardless of which button answered — see
            // `clickUrlAction` — so opening the link also closes this screen,
            // not just the confirmation sub-view.
            ServerLinksOutcome::OpenUrl(url) => {
                crate::menu::accounts::open_in_browser(url.as_str());
                let returns_to_chat = self.server_links.returns_to_chat();
                self.server_links.reset();
                if returns_to_chat {
                    ui.close_server_links_to_chat();
                } else {
                    ui.close_server_links();
                }
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

    /// Flips `options.showSubtitles` and saves immediately, same
    /// eager-persistence rule as [`MenuNav::toggle_view_bobbing`].
    fn toggle_show_subtitles(&mut self) {
        self.options.show_subtitles = !self.options.show_subtitles;
        self.persist_options();
    }

    /// Flips `key.sneak`'s hold/toggle mode and saves
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

    /// As [`MenuNav::toggle_toggle_sneak`], for `key.attack`.
    fn toggle_toggle_attack(&mut self) {
        self.options.toggle_attack = !self.options.toggle_attack;
        self.persist_options();
    }

    /// As [`MenuNav::toggle_toggle_sneak`], for `key.use`.
    fn toggle_toggle_use(&mut self) {
        self.options.toggle_use = !self.options.toggle_use;
        self.persist_options();
    }

    /// Flips `options.autoJump` and saves immediately, same
    /// eager-persistence rule as [`MenuNav::toggle_toggle_sneak`].
    fn toggle_auto_jump(&mut self) {
        self.options.auto_jump = !self.options.auto_jump;
        self.persist_options();
    }

    /// Steps `options.sprintWindow` by one 20 Hz tick and wraps between `0`
    /// and `10` inclusive — vanilla's `IntRange(0, 10)`,
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

    /// Flips `options.invertMouseX` and saves immediately.
    fn toggle_invert_mouse_x(&mut self) {
        self.options.invert_mouse_x = !self.options.invert_mouse_x;
        self.persist_options();
    }

    /// As [`MenuNav::toggle_invert_mouse_x`], for Y.
    fn toggle_invert_mouse_y(&mut self) {
        self.options.invert_mouse_y = !self.options.invert_mouse_y;
        self.persist_options();
    }

    /// Flips `options.discreteMouseScroll` and saves immediately.
    fn toggle_discrete_mouse_scroll(&mut self) {
        self.options.discrete_mouse_scroll = !self.options.discrete_mouse_scroll;
        self.persist_options();
    }

    /// Steps `mouseWheelSensitivity` by `delta` clicks of
    /// [`crate::config::MOUSE_WHEEL_SENSITIVITY_STEP`], wrapping between
    /// [`crate::config::MIN_MOUSE_WHEEL_SENSITIVITY`] and
    /// [`crate::config::MAX_MOUSE_WHEEL_SENSITIVITY`] inclusive,
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
    /// keeps the scroll window consistent with wherever that leaves it —
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

