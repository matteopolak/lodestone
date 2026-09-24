use super::{MenuNav, *};

impl MenuNav {
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
                    crate::menu::accounts::SignInView::Idle
                ) {
                    Some(super::render::accounts_list_spec(
                        accounts.rows().len(),
                        accounts.scroll(),
                    ))
                } else {
                    None
                }
            }
            // The singleplayer save list. Its length is the
            // **post-filter** row count, so typing in the search box shortens the
            // bar instead of leaving a thumb sized for the whole of `saves/` —
            // `WorldSelectNav::shown_len` is the one expression that decides, and
            // the row draw reads the same one.
            super::Screen::WorldSelect => {
                let ws = self.world_select();
                Some(super::render::world_list_spec(ws.shown_len(), ws.scroll()))
            }
            // Statistics uses a pixel offset, which is the
            // prerequisite this arm exists to assert — see `ListSpec`'s doc.
            super::Screen::Statistics => Some(crate::menu::stats::list_spec(
                crate::menu::stats::GENERAL_STATS.len(),
                self.stats.scroll(),
            )),
            // Key Binds uses a pixel offset. **Keyed on the settings *page*, not
            // just the screen**: `Screen::Settings` is one screen with a dozen
            // pages and only some of them are lists, so an arm on the bare screen
            // would hang a scrollbar beside the root page's two-column grid. The
            // page's own `KeyBindsNav` owns the offset, in pixels.
            super::Screen::Settings
                if self.settings.page() == crate::menu::options::SettingsPage::KeyBinds =>
            {
                Some(crate::menu::key_binds::list_spec(
                    self.settings.key_binds().scroll(),
                ))
            }
            // Language uses a post-filter length. Its length is the **post-filter**
            // entry count, so typing in the search box shortens the bar instead of
            // leaving a thumb sized for the full list — `LanguageNav::model` is
            // the one expression that decides, and this arm calls the same
            // `list_spec` it does.
            super::Screen::Settings
                if self.settings.page() == crate::menu::options::SettingsPage::Language =>
            {
                let lang = self.settings.language();
                Some(crate::menu::language::list_spec(
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
                Some(crate::menu::packs::list_spec(
                    packs.focused_len(),
                    packs.scroll(),
                ))
            }
            // Social Interactions is the only
            // user of `RowBand::Inset` — its rows are full-width, so no constant
            // `row_w` could place its scrollbar; see `social::list_spec`).
            super::Screen::Social => Some(crate::menu::social::list_spec(
                self.social.entries().len(),
                self.social.scroll(),
            )),
            super::Screen::Friends => Some(crate::menu::friends::list_spec(
                self.friends.view().snapshot.as_ref().map_or(0, |snapshot| match self.friends.tab() {
                    crate::menu::friends::FriendsTab::Friends => snapshot.friends.len(),
                    crate::menu::friends::FriendsTab::Pending => snapshot.incoming.len() + snapshot.outgoing.len(),
                    crate::menu::friends::FriendsTab::Settings => 0,
                }),
                self.friends.scroll(),
            )),
            // Every other settings page uses entry heights that
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
            // Create New World's Game Rules sub-screen (More
            // tab). Gated on the nested mode, not the bare screen — the
            // ordinary Game/World/More tabs have no list at all, so an
            // unconditional arm here would hang a scrollbar beside them.
            super::Screen::CreateWorld if self.create_world.game_rules_open() => Some(
                crate::menu::create_world::game_rules_list_spec(self.create_world.game_rules_scroll()),
            ),
            // Create New World's Data Packs sub-screen (More
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
            // The save list. Same screen as `active_list`'s arm — the
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
            // Key Binds. Same page guard as `active_list`'s arm — the two
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
            // Language. Same page guard as `active_list`'s arm.
            super::Screen::Settings
                if self.settings.page() == crate::menu::options::SettingsPage::Language =>
            {
                let lang = self.settings.language_mut();
                let before = lang.scroll();
                lang.scroll_by(notches, canvas_height);
                lang.scroll() != before
            }
            // Resource Packs. Same page guard as `active_list`'s
            // arm; the wheel moves whichever column the cursor is in.
            super::Screen::Settings
                if self.settings.page() == crate::menu::options::SettingsPage::ResourcePacks =>
            {
                let packs = self.settings.packs_mut();
                let before = packs.scroll();
                packs.scroll_by(notches, canvas_height);
                packs.scroll() != before
            }
            // Social Interactions. Same screen as `active_list`'s arm.
            super::Screen::Social => {
                let before = self.social.scroll();
                self.social.scroll_by(notches, canvas_height);
                self.social.scroll() != before
            }
            super::Screen::Friends => {
                let before = self.friends.scroll();
                self.friends.scroll_by(notches, canvas_height);
                self.friends.scroll() != before
            }
            // Every other settings page. Same ordering as `active_list`.
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
    /// `setScrollAmount(scrollAmount() - scrollY * scrollRate())`.
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
    pub(super) fn scroll_server_to_show(&mut self) {
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
    /// measured in.
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
    /// getContentX()` — and it is derived
    /// through [`super::render::server_row_content_rect`], the same expression the
    /// draw uses, rather than restating the row geometry here. That is what keeps
    /// the highlighted quadrant and the quadrant that acts from drifting apart.
    /// Returns `(rel_x, rel_y, size)`, so the caller passes the same `size` the
    /// draw blits at rather than restating vanilla's 32.
    #[must_use]
    pub(super) fn entry_icon_cursor(&self, row: usize) -> Option<(f32, f32, f32)> {
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

    /// The highlighted death-screen button.
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

    /// The command block edit screen's state, or `None` when
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

    /// The signed-book reading screen's state, or `None` when
    /// [`Screen::BookView`] is not showing — see [`Self::book_view`]'s own
    /// field doc.
    #[must_use]
    pub fn book_view(&self) -> Option<&book_view::BookViewState> {
        self.book_view.as_ref()
    }

    /// The Spectator Menu's live state — never `None` (see
    /// [`Self::spectator_menu`]'s own field doc), drawn only while
    /// [`Screen::SpectatorMenu`] is up.
    #[must_use]
    pub fn spectator_menu(&self) -> &spectator_menu::SpectatorMenuState {
        &self.spectator_menu
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

    /// Push the server's command tree down from `app`.
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

    /// The last-pushed publish state — the raw wire ground truth, `false` on
    /// both an unpublished singleplayer world and any multiplayer session.
    /// Nothing outside this module currently needs this distinguished from
    /// [`Self::open_to_lan_available`]; kept as its own accessor because it is
    /// a real, independently meaningful fact even though today's one caller
    /// wants the combined one.
    #[must_use]
    pub fn is_lan_published(&self) -> bool {
        self.lan_published
    }

    /// Pushes vanilla's `hasSingleplayerServer()` in, from
    /// `UiState::kind() == Some(SessionKind::Singleplayer)` — see
    /// [`Self::has_singleplayer_server`]'s own field doc.
    pub fn set_has_singleplayer_server(&mut self, value: bool) {
        self.has_singleplayer_server = value;
    }

}
