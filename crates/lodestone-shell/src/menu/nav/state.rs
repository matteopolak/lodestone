use super::{MenuNav, *};

impl MenuNav {
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

    /// Vanilla's `key.sneak` hold/toggle option — see
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

    /// As [`MenuNav::toggle_sneak`], for `key.attack`.
    #[must_use]
    pub fn toggle_attack(&self) -> bool {
        self.options.toggle_attack
    }

    /// As [`MenuNav::toggle_sneak`], for `key.use`.
    #[must_use]
    pub fn toggle_use(&self) -> bool {
        self.options.toggle_use
    }

    /// Vanilla's `options.autoJump` — see
    /// [`crate::config::Options::auto_jump`]. Pushed into `Sim` once per frame
    /// by `app/redraw.rs`, the same way [`Self::view_bobbing`] is, so a change
    /// in Controls applies on the very next tick's auto-jump gate rather than
    /// at the next launch.
    #[must_use]
    pub fn auto_jump(&self) -> bool {
        self.options.auto_jump
    }

    /// Vanilla's `options.sprintWindow` — the double-tap-forward
    /// window in 20 Hz ticks. See [`crate::config::Options::sprint_window_ticks`].
    /// Pushed into `Sim` once per frame by `app/redraw.rs` and forwarded to
    /// the live `InputState`, so a change in Controls applies on the very next
    /// tick.
    #[must_use]
    pub fn sprint_window_ticks(&self) -> u8 {
        self.options.sprint_window_ticks
    }

    /// Vanilla's `options.sensitivity` — see
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
    /// in vanilla's own keyboard-handler type).
    pub fn toggle_pause_on_lost_focus(&mut self) {
        self.options.pause_on_lost_focus = !self.options.pause_on_lost_focus;
        self.persist_options();
    }

    /// Vanilla's `options.invertMouseX` — see
    /// [`crate::config::Options::invert_mouse_x`]. Read per look-integration
    /// call and handed to `apply_look_inverted`.
    #[must_use]
    pub fn invert_mouse_x(&self) -> bool {
        self.options.invert_mouse_x
    }

    /// [`crate::config::Options::discrete_mouse_scroll`], read by
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

    /// Vanilla's `options.mouseWheelSensitivity` — see
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

    /// The settings tree's cursor — which page, which control, how
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

    /// The Social Interactions screen's own state.
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

    /// Replace Friends' credential-free presentation state. The app is the
    /// only source: menu code cannot reach a session, token, or service.
    pub fn refresh_friends_view(&mut self, view: crate::friends_runtime::FriendsView) {
        self.friends.refresh(view);
    }

    #[must_use]
    pub fn friends(&self) -> &crate::menu::friends::FriendsNav {
        &self.friends
    }

    /// Drain user gestures for the app to forward to its private Friends worker.
    pub fn take_friends_intents(&mut self) -> Vec<crate::menu::friends::FriendsIntent> {
        self.friends.take_intents()
    }

    /// Replaces the Spectator Menu's roster — the same shape
    /// [`Self::refresh_social`] immediately above has, for the identical
    /// reason: `app/session.rs`'s per-frame reconciliation is what can reach
    /// the live [`crate::sim::Sim`]; this module cannot. Called every frame
    /// while connected regardless of which screen is open, matching
    /// `refresh_social`'s own reasoning — the list the player sees the
    /// instant they open the menu must not be a frame stale.
    pub fn refresh_spectator_menu(&mut self, root: Vec<spectator_menu::SpectatorMenuEntry>) {
        self.spectator_menu.refresh(root);
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

    pub fn server_links_mut(&mut self) -> &mut crate::menu::server_links::ServerLinksNav {
        &mut self.server_links
    }

    /// The Advancements screen's own tab/scroll state, for the draw
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

    /// The Statistics screen's own state.
    pub fn stats(&self) -> &crate::menu::stats::StatsNav {
        &self.stats
    }

    /// The World Creation screen's own state.
    #[must_use]
    pub fn create_world(&self) -> &crate::menu::create_world::CreateWorldNav {
        &self.create_world
    }

    /// Whether a Key Binds bind button is mid-capture — a click
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

    /// Which [`SERVER_LIST_BUTTONS`] entry the cursor is over, if any.
    #[must_use]
    pub fn list_button(&self) -> Option<usize> {
        self.list_button
    }

    /// How far the multiplayer list is scrolled down, **in logical pixels**
    /// See [`Self::server_scroll`]'s field doc.
    #[must_use]
    pub fn server_scroll(&self) -> f32 {
        self.server_scroll
    }

}
