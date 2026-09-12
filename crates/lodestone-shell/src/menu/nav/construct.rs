use super::{MenuNav, *};

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
            ownership: 0,
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
            friends: crate::menu::friends::FriendsNav::default(),
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
            book_view: None,
            spectator_menu: spectator_menu::SpectatorMenuState::default(),
            resource_pack_prompt: None,
            resource_pack_answered_id: None,
            command_tree: None,
            lan_published: false,
            has_singleplayer_server: false,
        }
    }

    /// **The ownership gate's whole question**: proof that a locally stored
    /// account owns the game, or `None`.
    ///
    /// Delegates to the account screen, which owns the loaded roster — not a
    /// second read of `profiles.json`, because two readers of one file are two
    /// answers that can disagree the moment an account is added or removed.
    ///
    /// Deliberately recomputed per call rather than cached at construction:
    /// removing the last account has to close the gate again in the same frame,
    /// and a cached token is exactly how it would not.
    #[must_use]
    pub fn entitlement(&self) -> Option<Entitlement> {
        self.accounts.entitlement()
    }

    /// Authorization for a local world. A multiplayer-capable build keeps the
    /// account ownership proof; a deliberately confined build has no remote
    /// join surface and can therefore play locally without online credentials.
    #[must_use]
    pub(super) fn singleplayer_permit(&self) -> Option<SingleplayerPermit> {
        #[cfg(feature = "multiplayer")]
        {
            self.entitlement().map(SingleplayerPermit::Entitled)
        }
        #[cfg(not(feature = "multiplayer"))]
        {
            Some(SingleplayerPermit::LocalBuild)
        }
    }

    /// Whether the ownership gate must be showing instead of whatever `ui` is
    /// on.
    ///
    /// One expression with four callers — [`Self::key`], [`Self::click`],
    /// [`Self::hover`] and `render::frame_for` — because a gate whose input
    /// routing and whose drawing disagree about being closed is a gate you can
    /// click through.
    ///
    /// Takes the whole [`UiState`] rather than a [`Screen`] because one screen
    /// cannot answer on its own: [`Screen::Settings`] is reached both from the
    /// title *and* from the pause menu, and the in-world one sits over a live
    /// session. `settings_in_world` is the only thing that tells them apart.
    ///
    /// Exempt, each for its own reason:
    ///
    /// * [`Screen::Accounts`] is the *only* way out of the gate. Blocking it
    ///   would make the gate unopenable.
    /// * [`Screen::Error`] carries a failure the app has already decided to
    ///   show (a build with no version family, for instance). Painting the gate
    ///   over it would replace a real diagnosis with an unrelated screen.
    /// * Every **in-session** screen — see [`Screen::in_session`] for why the
    ///   gate must never paint over a live world, and why that fork is an
    ///   exhaustive match rather than a list here — plus in-world Settings,
    ///   which `in_session` cannot classify without this extra bit.
    ///
    /// [`Screen::Ownership`] itself is not exempt and does not need to be:
    /// reconciling the gate onto the gate is a no-op.
    #[must_use]
    pub fn ownership_gate_blocks(&self, ui: &UiState) -> bool {
        if !MULTIPLAYER_ENABLED {
            return false;
        }
        let screen = ui.screen();
        if matches!(screen, Screen::Accounts | Screen::Error)
            || screen.in_session()
            || (screen == Screen::Settings && ui.settings_in_world())
        {
            return false;
        }
        self.entitlement().is_none()
    }

    /// Refuse a play verb that was reached without an [`Entitlement`], landing
    /// the player back on the gate.
    ///
    /// **Unreachable through the UI as it stands** — [`Self::key`] and
    /// [`Self::click`] reconcile onto [`Screen::Ownership`] before any screen
    /// that can produce a play verb gets to dispatch — and that is exactly why
    /// it exists rather than an `unreachable!()`: the whole point of the gate is
    /// to survive an entry path nobody has written yet, and a refusal that
    /// visibly lands on the gate is the right answer for one, where a panic and
    /// a silent `MenuAction::None` are both wrong.
    pub(super) fn refuse_unowned(&mut self, ui: &mut UiState) -> MenuAction {
        tracing::warn!(
            target: "auth",
            screen = ?ui.screen(),
            "a play action was reached with no account that owns the game; \
             returning to the ownership gate"
        );
        self.ownership = 0;
        ui.open_ownership_gate();
        MenuAction::None
    }

    /// The gate's highlighted widget.
    #[must_use]
    pub fn ownership_button(&self) -> OwnershipButton {
        OWNERSHIP_BUTTONS[self.ownership.min(OWNERSHIP_BUTTONS.len() - 1)]
    }

    /// The gate's highlighted row index, for the renderer.
    #[must_use]
    pub fn ownership_index(&self) -> usize {
        self.ownership.min(OWNERSHIP_BUTTONS.len() - 1)
    }

    /// One key on [`Screen::Ownership`].
    ///
    /// Escape is **Quit**, not an unwind: there is no screen behind the gate to
    /// back out to, and an Escape that did nothing would read as a frozen game.
    pub(super) fn key_ownership(&mut self, ui: &mut UiState, key: MenuKey) -> MenuAction {
        match key {
            MenuKey::Up => {
                self.ownership = wrap_prev(self.ownership, OWNERSHIP_BUTTONS.len());
                MenuAction::None
            }
            MenuKey::Down => {
                self.ownership = wrap_next(self.ownership, OWNERSHIP_BUTTONS.len());
                MenuAction::None
            }
            MenuKey::Enter => match self.ownership_button() {
                OwnershipButton::AddAccount => {
                    ui.open_accounts();
                    MenuAction::None
                }
                OwnershipButton::Quit => {
                    ui.request_quit();
                    MenuAction::Quit
                }
            },
            MenuKey::Escape => {
                ui.request_quit();
                MenuAction::Quit
            }
            _ => MenuAction::None,
        }
    }

    /// A click on rendered row `row` of [`Screen::Ownership`].
    pub(super) fn click_ownership(&mut self, ui: &mut UiState, row: usize) -> MenuAction {
        if row >= OWNERSHIP_BUTTONS.len() {
            return MenuAction::None;
        }
        self.ownership = row;
        self.key_ownership(ui, MenuKey::Enter)
    }

}
