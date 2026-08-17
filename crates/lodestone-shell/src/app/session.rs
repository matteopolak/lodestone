//! `WindowApp` construction, cursor grab, and session start/teardown.
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

impl WindowApp {
    pub(super) fn new(config: Config) -> Self {
        let sim = Sim::new(config.clone());
        // Matches the sky fog set at render bring-up, so the fog reconciliation's
        // first above-water frame is a no-op rather than a redundant upload.
        let applied_fog = Some(crate::sim::fog_for_render_distance(config.render_distance));
        // Read once for both the keybinds and the Render Distance edge detector
        // below. **The seed is the *persisted* value, not `config`'s**, because
        // `--render-distance` on argv wins for the run (`Config::resolve_persisted`)
        // — seeding from `config` would make frame one see a "change" back to the
        // stored value and quietly undo the flag 600 ms in.
        let persisted = crate::config::Options::load();
        Self {
            config,
            sim,
            window: None,
            gpu: None,
            target: None,
            render: None,
            hud: None,
            effects: None,
            container: None,
            grabbed: false,
            pacer: FramePacer::new(Instant::now()),
            ui: UiState::new(),
            nav: MenuNav::new(),
            statuses: StatusCache::new(),
            menu: None,
            favicons: crate::menu::render::FaviconCache::new(),
            cursor: (0.0, 0.0),
            show_debug: false,
            debug_held: false,
            debug_chord_used: false,
            debug_hitboxes: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            debug_chunk_borders: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            menu_slider_drag: None,
            render_distance_seen: persisted.render_distance,
            render_distance_apply_at: None,
            tab_held: false,
            pending_screenshot: false,
            // Read from `options.json` via the same loader the menu uses.
            // Missing, partial or corrupt is vanilla's defaults, never an error
            // — see `Keybinds::from_json_value`.
            keybinds: persisted.keybinds,
            chat_input: ChatInput::new(),
            chat_wrap: crate::hud::ChatWrapCache::default(),
            menu_input: MenuInput::new(),
            shift_held: false,
            ctrl_held: false,
            modifiers: winit::keyboard::ModifiersState::empty(),
            scroll_accum: 0.0,
            last_menu_click: None,
            last_log: Instant::now(),
            applied_fog,
            recipe_book: None,
            recipe_book_revision: 0,
            recipe_panel: RecipePanelState::default(),
            recipe_toasts: lodestone_game::recipe::RecipeToastQueue::new(),
            recipe_toast_seen: std::collections::HashSet::new(),
            recipe_toast_synced: false,
            recipe_book_seen: std::collections::HashSet::new(),
            bundle_selection: None,
            // No session yet, so no weather cell to read; see
            // `install_session_render_sources`.
            weather: None,
            creative: crate::container::CreativeState::default(),
            advancements_drag: None,
            advancement_feed: super::advancements_screen::AdvancementsFeed::default(),
            hosted_world: None,
            merchant_selected: 0,
            anvil_rename: crate::container::AnvilRenameState::new(),
            beacon_selection: crate::container::beacon::BeaconSelection::new(),
            pending_game_rules: None,
            last_ping_request: None,
        }
    }

    pub(super) fn set_grab(&mut self, grabbed: bool) {
        let Some(window) = &self.window else { return };
        if grabbed {
            let locked = window
                .set_cursor_grab(CursorGrabMode::Locked)
                .or_else(|_| window.set_cursor_grab(CursorGrabMode::Confined));
            if locked.is_ok() {
                window.set_cursor_visible(false);
                self.grabbed = true;
            }
        } else {
            let _ = window.set_cursor_grab(CursorGrabMode::None);
            window.set_cursor_visible(true);
            self.grabbed = false;
            self.sim.input_mut(InputState::release_all);
            // Releasing the pointer also ends any held dig, so mining does not
            // continue while the player is in a menu or the window is unfocused.
            self.sim.end_attack();
        }
    }

    /// Reconcile the menu state machine with the session's real phase, then keep
    /// the cursor grab in sync with whatever screen we ended up on. Called each
    /// frame so the loading screen is never a lie: it clears the moment the
    /// server logs us in, and flips to Error the moment the session ends.
    pub(super) fn drive_ui_from_session(&mut self) {
        use crate::sim::SessionPhase;
        // Issue #449: the loading screen's *label* comes from here, not from the
        // coarse `SessionPhase` below — see `crate::menu::loading::ConnectPhase`
        // for why they are two different questions. Pushed every frame rather
        // than on transition, because `Sim` is the only thing the net thread
        // reaches and `UiState` is the only thing `frame_for` reads.
        self.ui.set_connect_phase(self.sim.connect_phase());
        match self.sim.session_phase() {
            // LocalOnly never drives the menu — the dev world is already Playing.
            SessionPhase::LocalOnly | SessionPhase::Connecting => {}
            SessionPhase::Connected => {
                self.ui.session_ready();
                // Issue #592's Game Rules half: the integrated server only
                // starts inside `begin_singleplayer`, so there was nothing to
                // send the overrides to any earlier than the session's own
                // first `Connected` frame. `take()` means a later frame
                // (this arm runs every frame the session stays `Connected`,
                // not just the transition into it) sends nothing a second
                // time.
                if let Some(entries) = self.pending_game_rules.take() {
                    self.sim.send_set_game_rules(entries);
                }
            }
            SessionPhase::Ended(end) => {
                // Only transition in once; re-setting every frame would keep
                // re-latching the same reason (harmless but wasteful).
                //
                // The whole `SessionEnd` crosses, not a formatted string: its
                // `kind` is what picks the screen's title (a server disconnect
                // and a failure to reach the server are different screens in
                // vanilla) and its `reason` is still a styled `Text`, so the
                // server's own colours survive to the draw.
                if self.ui.screen() != crate::menu::Screen::Error {
                    self.ui.session_failed(*end);
                }
            }
        }
        // The death screen (issue #103): `net::run` now builds the client
        // with `RespawnPolicy::Manual`, so nothing auto-respawns any more —
        // `Sim::is_dead` is the ground truth for whether the screen should be
        // up, reconciled here the same way `SessionPhase` is reconciled into
        // `UiState` above. The `!self.ui.is_death()` guard makes `die` fire
        // exactly once per death rather than re-latching (and re-cloning) the
        // message every frame the screen stays up; the `respawn_confirmed`
        // side needs no such guard — it is already a no-op off `Screen::Death`.
        //
        // `doImmediateRespawn` (issue #436's `SessionGameRules` island) forks
        // this: vanilla's `ClientPacketListener.handleRespawn` never puts the
        // death screen up at all when the rule is on, it respawns on the spot.
        // That is the rule's entire user-visible meaning, and it is the reason
        // the fold had a reader worth writing — `SessionGameRules` was folded,
        // reset on quit-to-title, gated through the real `SharedState::apply`
        // path, and read by nothing.
        if self.sim.is_dead() {
            if self.sim.game_rules().immediate_respawn() == Some(true) {
                // No screen, ever — not "open it and close it next frame",
                // which would flash the death screen for one frame at 60 Hz.
                // `Sim::respawn` is already a no-op unless `is_dead`, so this
                // cannot fire twice for one death: the second frame sees the
                // server's confirmation and `is_dead` is false.
                self.sim.respawn();
            } else if !self.ui.is_death() {
                self.ui.die(self.sim.death_message().map(str::to_string));
            }
        } else if self.ui.is_death() {
            self.ui.respawn_confirmed();
        }
        self.restore_recipe_book_settings();
        self.sync_recipe_toasts();
        self.sync_recipe_book_seen();
        // The credits screen (issue #192): `Sim::has_won()` is the ground
        // truth `NetUpdate::WinGame` sets in `poll_net`, reconciled here the
        // same way `is_dead()` is reconciled above. The `!= Screen::Credits`
        // guard mirrors the `!self.ui.is_death()` one: `show_credits` is
        // already idempotent (it only moves the screen from a live-gameplay
        // screen), but this avoids re-latching every frame the screen stays
        // up. No "un-won" transition is needed on the other side — unlike
        // death, winning has no server-confirmed reversal to reconcile
        // against, and `Sim::end_session` clears the flag for the next
        // session.
        if self.sim.has_won() && self.ui.screen() != crate::menu::Screen::Credits {
            self.ui.show_credits();
        }
        // The sign-editing screen: `Sim::take_pending_sign_edit` is the
        // ground truth a real `NetUpdate::SignEditorOpened` sets, reconciled
        // here every frame the same way `has_won`/`is_dead` are above. Unlike
        // those two this is a one-shot **take**, not a latched flag — see
        // `Sim::pending_sign_edit`'s own doc — so there is no "un-open"
        // branch to reconcile on the other side; the screen closes itself
        // through `MenuNav::close_sign_edit` when the player is done.
        //
        // `MenuNav::open_sign_edit` converts `Sim`'s menu-agnostic
        // `PendingSignEdit` into `menu::sign_edit::SignEditOpen` and guards
        // on `Screen::Playing`, matching `open_command_block`'s own two-step
        // (widget state here, screen there).
        if let Some(request) = self.sim.take_pending_sign_edit() {
            self.nav.open_sign_edit(
                &mut self.ui,
                crate::menu::sign_edit::SignEditOpen {
                    pos: request.pos,
                    is_front_text: request.is_front_text,
                    lines: request.lines,
                },
            );
        }
        // Issue #535's scope 2: the pause menu stops offering Open to LAN
        // once there is nothing left for it to do. `Sim::is_lan_published`
        // is the ground truth (set from the real `NetUpdate::LanOpened`, not
        // from `hosted_world` — a multiplayer session is never published
        // either), reconciled here every frame the same way `has_won`/
        // `is_dead` are above, so a menu already open catches up the instant
        // the server confirms the bind rather than needing to be reopened.
        self.nav.set_lan_published(self.sim.is_lan_published());
        // Vanilla's other half of the same gate, `hasSingleplayerServer()` —
        // without this a multiplayer session read the same `false` an
        // unpublished singleplayer world does and showed Open to LAN with
        // nothing local to publish. `MenuNav` holds no `Sim`/`UiState` of its
        // own, so `SessionKind` is pushed in here too, next to the flag it
        // combines with (`MenuNav::open_to_lan_available`).
        self.nav.set_has_singleplayer_server(
            self.ui.kind() == Some(crate::menu::SessionKind::Singleplayer),
        );
        // A server-initiated container close: `Sim::open_menu` is the same
        // ground truth `redraw` reads a few lines below to *open*
        // `Screen::Container` (`Sim::open_menu().is_some() && is_playing()`),
        // reconciled the other way here. `ClientEvent::ScreenClosed` already
        // resets the *menu* model (`lodestone_game::menus::Menus::apply`,
        // folded through `lodestone-ecs`'s session ingest) the instant the
        // server sends `CONTAINER_CLOSE`, but nothing reset the *screen* —
        // see `UiState::reconcile_server_menu_window`'s own doc for the full
        // vanilla chain (`clientSideCloseContainer`'s two clauses) and why
        // this has to be edge-triggered on the window id rather than a level
        // check on "no window right now", which would also fire for the
        // player's own `E`-opened inventory.
        self.ui
            .reconcile_server_menu_window(self.sim.open_menu().map(|open| open.window_id));
        // The resource-pack prompt: `NetClient::pending_resource_pack_prompt`
        // is the ground truth, reconciled here every frame the same way
        // `has_won`/`is_dead`/`is_lan_published` are above. `show_resource_pack_prompt`
        // rebuilds `MenuNav`'s own dialog state unconditionally (a second
        // push must not inherit a stale focus or pack id — see that
        // method's own doc), so it is only called on the **edge** into
        // "something is pending" via `!self.ui.is_resource_pack_prompt()`.
        //
        // That edge alone used to be the owner's exact report ("accepting
        // the custom resource pack didn't do anything, and it kept the
        // choice menu open"): `apply_resource_pack_prompt` closes the screen
        // the moment the player answers, but `respond_to_resource_pack` only
        // *queues* the answer for the net thread's own loop to drain — up to
        // 15 ms later, never "the instant" a doc comment here used to claim
        // — so this reconcile, which can run again the very same frame (the
        // click handler and `redraw` share one winit dispatch), still reads
        // the *same* pending prompt back from the still-uncleared shared
        // cell and reopens it right away, indistinguishable from the click
        // having done nothing. `MenuNav::resource_pack_already_answered`
        // is the fix: it remembers the id this side already answered so the
        // edge does not re-fire for it, and is forgotten once the ground
        // truth itself reports nothing pending (the net thread has, by then,
        // actually cleared its cell).
        if let Some(net) = self.sim.net() {
            match net.pending_resource_pack_prompt() {
                Some(prompt) => {
                    if !self.ui.is_resource_pack_prompt()
                        && !self.nav.resource_pack_already_answered(prompt.id)
                    {
                        self.nav.show_resource_pack_prompt(&mut self.ui, &prompt);
                    }
                }
                None => self.nav.clear_resource_pack_answered(),
            }
        }
        // A transition may have changed grab intent (Connected → Playing grabs;
        // Ended/Death → menu-owned screens release). Only touch the OS grab
        // when it disagrees.
        let want = self.ui.wants_cursor_grab();
        if want != self.grabbed {
            self.set_grab(want);
        }

        // Issue #189: keep the Social Interactions roster live.
        // `social::entries_from_tablist` was pure and tested with **no
        // production caller** — this is the queued call
        // `docs/social-interactions.md`'s "How to change it" names. Only
        // `Screen::Social` ever reads `MenuNav::social()`, but this runs every
        // frame regardless of which screen is open (matching every other
        // reconciliation in this function) rather than gating on the screen:
        // a `TabList` clone plus a short `Vec` build is cheap, and refreshing
        // only-while-open would mean the roster the player sees the instant
        // they open it is one frame stale.
        if self.sim.session_phase() == crate::sim::SessionPhase::Connected {
            let tab_list = self.sim.tab_list();
            let entries =
                crate::menu::social::entries_from_tablist(&tab_list, self.sim.local_uuid());
            self.nav.refresh_social(entries);

            // The Statistics screen (#188), for exactly the same reason and in
            // exactly the same shape. `award_stats` is decoded and folded into
            // `lodestone_ecs::SessionStatistics`, and `menu::render::dispatch`
            // passed `StatsSnapshot::default()` — a literal — into the frame, so
            // every counter read zero no matter what the server sent. This is the
            // read that was missing.
            //
            // Every frame rather than only while the screen is open, matching the
            // roster above: the projection walks the screen's fixed 77 ids against
            // a sparse map, and refreshing only-while-open would show one stale
            // frame on open.
            let stats = self.sim.statistics();
            self.nav
                .refresh_stats(crate::menu::stats::StatsSnapshot::from_statistics(&stats));

            // The pause menu's Server Links row and the screen it opens, for
            // exactly the same reason: `SERVER_LINKS` decodes into
            // `ClientEvent::ServerLinksReceived` and folds into
            // `lodestone_ecs::session::SessionServerInfo`, and nothing read
            // it — the row never had a live link list to gate its own
            // presence on. Every frame rather than only while the screen is
            // open, matching the roster and the counters above.
            self.nav.refresh_server_links(self.sim.server_links());
        }
    }

    /// Apply the server's `RECIPE_BOOK_SETTINGS` (76) to the recipe-book panel,
    /// once per book type per session — issue #436's `SessionRecipeBookSettings`
    /// island.
    ///
    /// Before this, the panel always started closed and unfiltered no matter
    /// what the server said, so a player who had left their book open came back
    /// to it shut. The fold landed in `fd53995` and had **no reader**; this is
    /// it.
    ///
    /// Three guards, each load-bearing:
    ///
    /// * `settings.reported` — an unreported record is all-`false`, which is
    ///   indistinguishable from "the server wants it closed". Restoring on an
    ///   unreported record would be restoring *our own default*, a wire that
    ///   looks connected and carries nothing.
    /// * `restored_type != Some(book_type)` — the settings are per book type
    ///   while the panel state is one shared instance, so this re-restores when
    ///   the player opens a furnace after a crafting table, and does **not**
    ///   re-restore every frame (which would fight the user's own clicks).
    /// * an open menu with a recipe book at all — `recipe_book_type_for`
    ///   returns `None` for a chest, and there is nothing to restore into.
    ///
    /// Deliberately does **not** call `send_recipe_book_settings`: this is the
    /// server's own value coming back, and echoing it would be a write loop.
    /// That asymmetry is why the two click arms report and this does not.
    pub(super) fn restore_recipe_book_settings(&mut self) {
        let Some(menu) = self.active_container_menu() else {
            return;
        };
        let Some(book_type) = super::recipe_panel::recipe_book_type_for(&menu) else {
            return;
        };
        if self.recipe_panel.restored_type == Some(book_type) {
            return;
        }
        let settings = self.sim.recipe_book_settings();
        if !settings.reported {
            return;
        }
        let per_type = settings.for_type(book_type);
        self.recipe_panel.open = per_type.open;
        self.recipe_panel.filtering = per_type.filtering;
        self.recipe_panel.page = 0;
        self.recipe_panel.restored_type = Some(book_type);
    }

    /// Diff the server's recipe-unlock sync against what has already been
    /// toasted, and push every newly-unlocked, notifying recipe into
    /// [`Self::recipe_toasts`] — the missing hop between `SessionRecipeBook`
    /// (`lodestone-ecs`, folded and read but never dispatched) and the toast
    /// queue (built and drawn, but never fed).
    ///
    /// Same shape as [`Self::restore_recipe_book_settings`]: a plain per-frame
    /// diff against `Sim::known_recipes()`, run from
    /// [`Self::drive_ui_from_session`] so it keeps up the instant a session
    /// exists, not gated on any screen being open (a toast can fire with no
    /// menu on screen at all).
    ///
    /// The **first** sync — `known_recipes().has_data()` true for the first
    /// time this session — seeds [`Self::recipe_toast_seen`] from the whole
    /// current known set and toasts nothing: vanilla does not toast a fresh
    /// join's entire unlock history, only genuinely new unlocks after that.
    /// Every later frame toasts exactly the display ids not already in the
    /// seen set, which both first-sync seeding and a real toast insert into,
    /// so nothing is ever toasted twice.
    ///
    /// A recipe whose result or station item id does not resolve through
    /// `lodestone_data::items::item_name` (an id outside the generated
    /// census) is marked seen but never toasted — the same "draw nothing
    /// rather than a wrong icon" contract `container::merchant::cost_item_stack`
    /// documents for the same table.
    pub(super) fn sync_recipe_toasts(&mut self) {
        let sync = self.sim.known_recipes();
        if !sync.has_data() {
            // Off a server, or before the first recipe-book sync packet has
            // landed this session — nothing to diff against yet.
            return;
        }
        if !self.recipe_toast_synced {
            self.recipe_toast_seen = sync.known().keys().copied().collect();
            self.recipe_toast_synced = true;
            return;
        }
        let now = recipe_toast_now_ms();
        for (&display_id, recipe) in sync.known() {
            if !self.recipe_toast_seen.insert(display_id) {
                // Already toasted, or already folded into the first-sync seed.
                continue;
            }
            if !recipe.notification {
                continue;
            }
            let Some(unlocked) = recipe
                .result_items
                .first()
                .copied()
                .and_then(recipe_item_identifier)
            else {
                continue;
            };
            let Some(station) = recipe
                .station_items
                .first()
                .copied()
                .and_then(recipe_item_identifier)
            else {
                continue;
            };
            self.recipe_toasts.push(station, unlocked, now);
        }
    }

    /// Report vanilla's "seen recipe" signal for every highlighted, unseen
    /// recipe currently placed on the recipe-book panel's visible page —
    /// vanilla's `RecipeButton::init` → `RecipeBookPage::recipeShown` →
    /// `RecipeBookComponent::recipeShown` → `LocalPlayer::removeRecipeHighlight`
    /// chain, which fires the instant a highlighted recipe's button is
    /// populated onto a page the player can see, not on a click.
    ///
    /// Walked every frame the panel is open, the same shape as
    /// [`Self::sync_recipe_toasts`] and [`Self::restore_recipe_book_settings`]:
    /// only while `recipe_panel.open`, because vanilla only ever populates
    /// `RecipeButton`s — and therefore only ever fires this — for a page
    /// actually on screen, never for the whole corpus at once.
    ///
    /// [`Self::recipe_book_seen`] is this method's own "already reported" set,
    /// separate from [`Self::recipe_toast_seen`]: a recipe can be seen (its
    /// tab highlight cleared) without ever having raised a toast (highlight
    /// and notification are independent `flags` bits), and the toast queue's
    /// own dedup must not be reused to gate a differently-timed signal.
    pub(super) fn sync_recipe_book_seen(&mut self) {
        if !self.recipe_panel.open {
            return;
        }
        let Some(menu) = self.active_container_menu() else {
            return;
        };
        let Some(book_type) = super::recipe_panel::recipe_book_type_for(&menu) else {
            return;
        };
        let (_, _, page_ids) = super::recipe_panel::recipe_panel_contents(
            self.recipe_book.as_ref(),
            &self.recipe_panel,
            &menu,
            book_type,
        );
        let sync = self.sim.known_recipes();
        for id in &page_ids {
            let Some(item_reg_id) = lodestone_data::items::item_id(&id.to_string()) else {
                continue;
            };
            for (display_id, recipe) in sync.unlocked_producing(item_reg_id) {
                if !recipe.highlight {
                    continue;
                }
                if self.recipe_book_seen.insert(display_id) {
                    self.sim.send_recipe_book_seen_recipe(display_id);
                }
            }
        }
    }

    /// Staged Singleplayer entry point. Vanilla's singleplayer starts an
    /// integrated server in-process and connects to it over a local transport;
    /// that server (`impl-worldgen`'s `lodestone-server`, via a future
    /// `IntegratedServer::start`) is not wired yet. Rather than fork a second
    /// launch path or silently do nothing, this drives the honest failure path:
    /// the menu shows an Error explaining the feature is staged. Kept here so the
    /// wiring is a one-call swap once the seam lands.
    /// Install the block-outline source, which needs a live `Sim` — it reads the
    /// version adapter's per-state outline census through the shared handle.
    ///
    /// Must run *after* `attach_net`: `Sim::outline_shape_source` returns `None`
    /// without a net client. Until this is installed the selection box falls back
    /// to a unit cube, which is wrong for roughly nine block states in ten — only
    /// 3,328 of 32,366 have a full-cube outline.
    ///
    /// Note the outline census is deliberately *not* the collision census: they
    /// are different vanilla shape families and disagree for over half of all
    /// states, so a slab's box and a slab's collider are not the same box.
    pub(super) fn install_outline_source(&mut self) {
        if let (Some(render), Some(f)) = (self.render.as_mut(), self.sim.outline_shape_source()) {
            render.set_outline_shape_source(f);
        }
    }

    /// Install the debug-lines source: the render half of `ExtractSet::Debug`
    /// (`docs/plugin-api.md`), the channel a plugin (e.g. a navigator) uses to
    /// push world-space line geometry onto screen via
    /// `lodestone_ecs::player::DebugLines`. `RenderState::set_debug_lines_source`
    /// and the line pipeline it drives already existed with no caller —
    /// `gpu.rs`'s own `DebugLinesSource` doc names this as "the one wire this
    /// crate cannot lay itself."
    ///
    /// Unlike [`install_outline_source`](Self::install_outline_source), this
    /// needs no live connection: `Sim::new`/`Sim::with_demo_world` always add
    /// `LocalPlayerPlugin` (`crates/lodestone-ecs/src/player.rs`), which
    /// `init_resource`s `DebugLines` on the one `World` regardless of session
    /// kind, so `self.sim.ecs()` is enough. Callable — and safe to call
    /// repeatedly, since it only replaces the closure with an equivalent one —
    /// the moment `self.render` exists.
    pub(super) fn install_debug_lines_source(&mut self) {
        let Some(render) = self.render.as_mut() else {
            return;
        };
        let ecs = self.sim.ecs().clone();
        // Issue #197's two F3 sub-modes ride this same channel rather than
        // getting a pass of their own: they are world-space coloured segments,
        // which is exactly what `DebugLineRenderer` already draws, and it draws
        // last in the world pass so they read over everything real.
        let hitboxes = std::sync::Arc::clone(&self.debug_hitboxes);
        let borders = std::sync::Arc::clone(&self.debug_chunk_borders);
        // The world column, resolved **now** rather than assumed in the closure:
        // a nether or custom-height dimension has a different range, and a
        // hardcoded `-64..320` would silently draw the wrong box there. `None`
        // (no session yet) falls back to the overworld column, which is what the
        // dev world is.
        let (min_y, height) = self
            .sim
            .net()
            .and_then(crate::net::NetClient::world_dimensions)
            .map_or((-64, 384), |d| (d.min_y, d.height));
        let local = self.sim.local_entity();
        render.set_debug_lines_source(move || {
            use std::sync::atomic::Ordering;
            lodestone_ecs::hold_read(&ecs, |world| {
                let mut out = crate::gpu::debug_line_vertices(
                    &world.resource::<lodestone_ecs::DebugLines>().0,
                );
                if hitboxes.load(Ordering::Relaxed) {
                    out.extend(crate::gpu::entity_hitbox_vertices(
                        &crate::entities::extracted_entity_draws(world),
                    ));
                }
                if borders.load(Ordering::Relaxed) {
                    let p = world
                        .get::<lodestone_ecs::player::PhysicsState>(local)
                        .map_or([0.0, 0.0, 0.0], |s| {
                            [s.0.position.x, s.0.position.y, s.0.position.z]
                        });
                    out.extend(crate::gpu::chunk_border_vertices(p, min_y, height));
                }
                out
            })
        });
    }

    /// Install the plugin-billboard source: the render half of issue #161's
    /// `ExtractSet::Debug` billboard channel (`docs/plugin-api.md`), the
    /// channel a plugin (a waypoint, a hologram, a minimap overlay) uses to
    /// push textured/billboard world-space geometry onto screen via
    /// `lodestone_ecs::PluginBillboards`. `RenderState::set_plugin_billboards_source`
    /// and the pipeline it drives already exist with no caller — same shape
    /// as [`install_debug_lines_source`](Self::install_debug_lines_source),
    /// whose doc this one mirrors.
    ///
    /// Needs no live connection, for the identical reason
    /// `install_debug_lines_source` does not: `Sim::new`/`Sim::with_demo_world`
    /// always add `LocalPlayerPlugin`, which `init_resource`s
    /// `PluginBillboards` on the one `World` regardless of session kind, so
    /// `self.sim.ecs()` is enough. Callable — and safe to call repeatedly,
    /// since it only replaces the closure with an equivalent one — the moment
    /// `self.render` exists.
    pub(super) fn install_plugin_billboards_source(&mut self) {
        let Some(render) = self.render.as_mut() else {
            return;
        };
        let ecs = self.sim.ecs().clone();
        // Resolved **now**, not inside the closure: the atlas table is a
        // snapshot of whatever `RenderState` uploaded at connect time, and
        // capturing it once here is what lets the closure stay a plain `Fn`
        // with no borrow back into `render`.
        let atlas = render.plugin_atlas_sprites();
        render.set_plugin_billboards_source(move || {
            lodestone_ecs::hold_read(&ecs, |world| {
                crate::gpu::plugin_billboard_vertices(
                    &world.resource::<lodestone_ecs::PluginBillboards>().0,
                    &atlas,
                )
            })
        });
    }

    /// Arm and service the deferred Render Distance commit — vanilla's
    /// `OptionInstance.OptionInstanceSliderButton` `delayedApplyAt` /
    /// `applyUnsavedValue` pair (`OptionInstance.java, 429-435`).
    ///
    /// Called once per frame from `app/redraw.rs`. The deadline is **re-armed by
    /// every change**, so a drag that crosses ten values commits once, 600 ms
    /// after it stops.
    ///
    /// # Edge detection, not difference
    ///
    /// The trigger is `nav`'s value *changing*, not `nav` disagreeing with
    /// `config`. A difference test would re-arm on every frame of the 600 ms
    /// window and the commit would never fire — and it would also fight
    /// `--render-distance` on argv, which is deliberately allowed to disagree
    /// with the persisted value for the whole run.
    pub(super) fn tick_render_distance(&mut self, now: Instant) {
        let wanted = self.nav.render_distance();
        if wanted != self.render_distance_seen {
            self.render_distance_seen = wanted;
            self.render_distance_apply_at = Some(now + RENDER_DISTANCE_APPLY_DELAY);
        }
        if self.render_distance_apply_at.is_none_or(|at| now < at) {
            return;
        }
        self.render_distance_apply_at = None;
        if wanted == self.config.render_distance {
            return;
        }
        // Both copies. `self.config` is what the fog upload and the render
        // bring-up read; `self.sim.config` is what the camera's far plane
        // (`sim/camera.rs`) and the fog helpers read. Leaving either behind is
        // the "the slider appears to do nothing" report with one symptom fixed.
        self.config.render_distance = wanted;
        self.sim.config.render_distance = wanted;
        // The server side. Vanilla's `Options.broadcastOptions` sends
        // `ServerboundClientInformationPacket` whenever an option in it changes,
        // and `viewDistance` is in it — without this the server keeps streaming
        // the square we asked for at join and the extra rings simply never
        // arrive, so fog and the far plane would open onto empty space.
        //
        // `+ 1` for the mesher's buffer ring, the same reason
        // `start_singleplayer`'s `view_radius` adds one: the outermost streamed
        // ring can never be meshed, so asking for exactly `render_distance`
        // loses the last visible ring.
        //
        // # Raising it mid-session works, since #545
        //
        // This used to be capped: `dispatch_play_packet`'s
        // `ClientInformationChanged` arm clamped against *this connection's own*
        // `serve_connection` radius — the `render_distance + 1` the shell asked
        // for at join — so a decrease took effect and an increase past the launch
        // value was silently clamped back. `0c09f576` separated the join view
        // radius from the permitted maximum, so the clamp is now against the
        // server's configured ceiling and the chunk store's capacity follows a
        // live raise (grow-only, a session high-water mark). Both directions
        // reach the stream now, and nothing on this side needs to compensate.
        let radius = wanted.saturating_add(1);
        if let Some(net) = self.sim.net() {
            net.send_action(lodestone_model::action::ClientAction::SetClientSettings(
                self.client_settings(radius),
            ));
        }
        // Deliberately **not** `Sim::set_view_radius`: that is the loading
        // screen's progress denominator (#449), not the streaming radius, and
        // re-declaring it mid-session would re-baseline a bar for a load that
        // already finished. Nothing client-side gates chunk *retention* on the
        // radius — the camera's far plane and the fog do the work, and both read
        // `config` above.
    }

    /// The pause menu's **Open to LAN** (issue #535's scope 1): publish the
    /// world this process is hosting on a TCP port, so other machines can join it.
    ///
    /// # Publishes the live handle in place — issue #562
    ///
    /// This used to call `Sim::end_session` and reopen the same launch through
    /// `NetClient::open_to_lan`, which **rebuilt** the world: a fresh
    /// `ChunkStore`, a fresh tick loop, and the local player rejoining over
    /// loopback like a stranger — a real loading screen for a button that is
    /// supposed to be invisible if you are not the one joining. Vanilla's own
    /// `Minecraft.getSingleplayerServer().publishServer` adds a listener to
    /// the world already running; nothing about it is torn down. This now does
    /// the same: `NetClient::publish_to_lan` asks the net thread to call
    /// `IntegratedServer::publish` on the handle it already holds, so every
    /// entity, loaded chunk and this player's own position are exactly what
    /// they were the instant before the button was pressed — see that
    /// method's own doc comment for what state a publish-time joiner shares.
    ///
    /// `0` — an OS-assigned port (issue #559) — rather than a fixed one:
    /// vanilla's `/publish` defaults to `HttpUtil.getAvailablePort()`, and the
    /// actual bound port comes back through `NetUpdate::LanOpened`, which
    /// `Sim::apply` already turns into the "Local game hosted on port N" chat
    /// line — unchanged by this fix, since it already read the *reported*
    /// port rather than the requested one.
    ///
    /// Off a hosted world, or before a net session exists at all, this says so
    /// in chat instead of doing nothing — the caller only omits the button
    /// once it has *itself* learned the world is published (see
    /// `MenuNav::pause_buttons`), so a stale render (this frame's menu built
    /// from last frame's `MenuNav` state) can still land here between the
    /// press and that catch-up, and this is where "there is nothing of ours
    /// to publish" gets stated for it.
    ///
    /// A publish that fails server-side (already published, or a bind error)
    /// reports through `NetUpdate::LanPublishError` — **not**
    /// `NetUpdate::Error`, which ends the session — so a race here reads as
    /// one more chat line, never a kick. See `NetClient::publish_to_lan`'s own
    /// doc, and `NetUpdate::Error`'s for why the two must stay distinct: they
    /// used to be one variant, and every "already published" reply rode in on
    /// the session-ending one, which is exactly the disconnect this doc
    /// used to (wrongly) say could not happen.
    #[cfg(not(target_arch = "wasm32"))]
    pub(super) fn open_current_world_to_lan(&mut self) {
        if self.hosted_world.is_none() {
            self.sim
                .push_local_chat("Only a world you are hosting can be opened to LAN");
            return;
        }
        let Some(net) = self.sim.net() else {
            self.sim
                .push_local_chat("Only a world you are hosting can be opened to LAN");
            return;
        };
        net.publish_to_lan(0);
    }

    /// Vanilla's `key.debug.spectate` (F3+N): drop into spectator, or come back
    /// out of it (`KeyboardHandler.java`).
    ///
    /// **The first producer of `ClientAction::ChangeGameMode` anywhere outside
    /// `crates/protocol/`** — the variant was encoded by two families and sent by
    /// nothing, the outbound-island shape `ClientAction::SetFlying` was caught in,
    /// and the reason the server's own `ServerBound::ChangeGameMode` arm (live
    /// since `226ac517`) could never fire.
    ///
    /// **Coming back always lands in Creative, never in whatever you left.**
    /// Vanilla reads `gameMode.getPreviousPlayerMode()` and falls back to
    /// `GameType.CREATIVE` with `firstNonNull`; this client tracks no previous
    /// mode, so it takes that fallback every time. The server is authoritative
    /// either way — it answers with the mode it applied plus fresh abilities — so
    /// the worst case is one extra chord, not a desync.
    pub(super) fn toggle_spectator(&self) {
        use lodestone_model::GameMode;
        let Some(net) = self.sim.net() else { return };
        let current = net
            .shared_handle()
            .get()
            .cloned()
            .and_then(|handle| handle.game_mode());
        let wanted = if current == Some(GameMode::Spectator) {
            GameMode::Creative
        } else {
            GameMode::Spectator
        };
        net.send_action(lodestone_model::action::ClientAction::ChangeGameMode { mode: wanted });
    }

    /// Vanilla's `key.debug.switchGameMode` (F3+F4), cycling instead of opening a
    /// radial picker.
    ///
    /// `KeyboardHandler.java` shows a `GameModeSwitcherScreen` — a
    /// four-slot hover picker with its own hotbar-style art. Cycling is the
    /// honest subset: it reaches every mode with the same chord and needs no new
    /// screen, and a player holding F3 and tapping F4 four times sees exactly the
    /// same four modes the picker offers. Whoever wants the picker adds a
    /// `Screen` variant; the action this sends does not change.
    ///
    /// **No permission gate.** Vanilla checks `canSwitchGameMode()` and
    /// `GameModeCommand.PERMISSION_CHECK`; this client tracks no op level, and the
    /// server rejects an unauthorised request as it rejects every other
    /// optimistic action — see `targeted_command_block`'s doc for the same call.
    pub(super) fn cycle_game_mode(&self) {
        use lodestone_model::GameMode;
        let Some(net) = self.sim.net() else { return };
        let current = net
            .shared_handle()
            .get()
            .cloned()
            .and_then(|handle| handle.game_mode());
        net.send_action(lodestone_model::action::ClientAction::ChangeGameMode {
            mode: next_game_mode(current),
        });
    }

    /// The [`lodestone_model::action::ClientSettings`] this client would send
    /// now, with `view_distance` overridden to `radius` chunks.
    ///
    /// Split out because it is the *only* producer of
    /// [`lodestone_model::action::ClientAction::SetClientSettings`] in the
    /// workspace — the variant was encoded by four adapters and sent by nothing,
    /// which is `CLAUDE.md`'s outbound-island shape. Everything but
    /// `view_distance` and `chat_colors` is vanilla's `ClientInformation`
    /// default, because this client has no option for it yet; a fabricated value
    /// would be worse than the default it would replace.
    fn client_settings(&self, radius: u32) -> lodestone_model::action::ClientSettings {
        use lodestone_model::action::{
            ChatMode, ClientSettings, DisplayedSkinParts, MainHand, ParticleStatus,
        };
        ClientSettings {
            locale: "en_us".to_string(),
            // `ServerboundClientInformationPacket` carries a byte; vanilla clamps
            // to `2..=32` before sending. Saturating rather than wrapping, or a
            // radius past 127 would arrive as a negative distance.
            view_distance: i8::try_from(radius.clamp(2, 32)).unwrap_or(i8::MAX),
            chat_mode: ChatMode::Full,
            chat_colors: self.nav.options().chat_colors,
            skin_parts: DisplayedSkinParts {
                cape: true,
                jacket: true,
                left_sleeve: true,
                right_sleeve: true,
                left_pants_leg: true,
                right_pants_leg: true,
                hat: true,
            },
            main_hand: MainHand::Right,
            text_filtering: false,
            allow_server_listing: true,
            particle_status: ParticleStatus::All,
        }
    }

    /// Start singleplayer and show the loading screen (issue #287).
    ///
    /// The multiplayer twin of this is [`Self::connect_to`], and after the
    /// session is attached the two are *the same function*: both call
    /// [`Self::install_session_render_sources`], because the sky, fog clock,
    /// entity light sampler and screen-effect passes are properties of having a
    /// session, not of how it was obtained. That sharing is the point — a
    /// singleplayer path with its own render wiring is how one of the two ends up
    /// silently missing a pass.
    ///
    /// `attach_net` rather than a `Sim::connect`-style helper because the client
    /// is already built *with* this `Sim`'s `World` and local entity: that is what
    /// [`launch_singleplayer`]'s `session` argument is, threaded through
    /// `NetClient::open_singleplayer` into `ClientBuilder::ecs` (§4.1(c)).
    /// Attaching without it is the silent failure `Sim::connect`'s docs warn
    /// about — every HUD accessor would read an empty default.
    pub(super) fn begin_singleplayer(&mut self, launch: crate::menu::nav::SingleplayerLaunch) {
        use crate::menu::nav::SingleplayerLaunch;
        let launch_for_lan = launch.clone();
        self.ui.begin(crate::menu::SessionKind::Singleplayer);
        // Issue #468's reading (2): the world is a **directory the menu chose**,
        // and the two arms differ only in whether a typed seed is honoured.
        //
        // `Open` resolves through `resolve_launch_seed(None)` and the value is then
        // *discarded* by `resolve_world_seed`, which reads the world's stored seed
        // — a requested seed is a creation parameter for an existing world. That is
        // not a wire carrying the wrong value, it is a parameter with no effect on
        // this path, and it is why `SingleplayerLaunch::Open` carries no seed of
        // its own to imply otherwise. The one case where it *does* matter is a
        // directory whose `world_gen_settings.dat` is missing, which
        // `resolve_world_seed` then creates from it — see
        // `world_select::BUNDLED_WORLD`.
        //
        // `Created`'s directory already exists (the menu made it, with the player's
        // typed name in its `level.dat`) and has **no** settings file yet, so this
        // is the arm where `config.seed` reaches the generator.
        #[cfg(not(target_arch = "wasm32"))]
        let world_dir = Some(match &launch {
            SingleplayerLaunch::Open(dir) => dir.clone(),
            SingleplayerLaunch::Created { world_dir, .. } => world_dir.clone(),
        });
        let seed = match &launch {
            SingleplayerLaunch::Open(_) => resolve_launch_seed(None),
            SingleplayerLaunch::Created { config, .. } => resolve_launch_seed(Some(config)),
        };
        // Issue #592's items 1 and 2, same rule as `seed` immediately above:
        // only `Created` carries a `WorldCreationConfig` to read a chosen
        // preset from, and only a **new** world's generator is ever built
        // from this — an existing `Open`ed world has no stored world-type
        // field to override either, so it takes the same `Normal` default the
        // unconditional `overworld_chunk_source(seed)` call used before this
        // was threaded. Not cfg-gated to native, unlike `online_mode` below:
        // `launch_singleplayer` needs this on wasm32 too.
        //
        // The full `WorldTypePreset` is threaded now, not just its
        // `lodestone_server::WorldType` projection (issue #592's item 1) —
        // `net.rs`'s `preset_chunk_source` is what resolves the other four
        // presets (`SingleBiomeSurface`/`Flat`/`FlatAllDimensions`/
        // `DebugAllBlockStates`) once `lodestone-server`'s `lib.rs` re-exports
        // their entry points (issue #592's item 2, already landed), and it
        // needs the preset itself to pick among four different generator
        // constructors, not a three-way `WorldType`.
        let world_type = match &launch {
            SingleplayerLaunch::Open(_) => crate::menu::create_world::WorldTypePreset::Normal,
            SingleplayerLaunch::Created { config, .. } => config.world_type,
        };
        // Issue #592's Game Rules half, same rule as `world_type` immediately
        // above: only a **new** world carries a `WorldCreationConfig` to read
        // overrides from, and an empty `Vec` (nothing touched) is left as
        // `None` so `drive_ui_from_session` has nothing to send.
        self.pending_game_rules = match &launch {
            SingleplayerLaunch::Open(_) => None,
            SingleplayerLaunch::Created { config, .. } if !config.game_rules.is_empty() => {
                Some(config.game_rules.clone())
            }
            SingleplayerLaunch::Created { .. } => None,
        };
        // Issue #273's shell-side control: only `SingleplayerLaunch::Created`
        // carries a `WorldCreationConfig` to hold this on — `Open` (Play
        // Selected World) has none, so an existing world always takes the
        // ordinary offline path below. See
        // `WorldCreationConfig::online_mode`'s own doc for the full picture.
        #[cfg(not(target_arch = "wasm32"))]
        let online_mode = matches!(
            &launch,
            SingleplayerLaunch::Created { config, .. } if config.online_mode
        );
        let session = Some((self.sim.ecs().clone(), self.sim.local_player()));
        // Vanilla streams `simulationDistance`/`viewDistance` chunks around the
        // player; ours is the same number the camera's far plane and the mesher
        // already use, so the server never sends a column the renderer would
        // discard and never withholds one it wants.
        //
        // **Plus one, and the `+ 1` is not slack — it is the buffer ring the
        // mesher's invariant requires.** Vanilla's own server tracks
        // `center + viewDistance + 1` (`ChunkTrackingView.java, 96`), and it has
        // to: a section is only meshed once all its neighbours are resident, so the
        // outermost ring of a radius-`n` stream permanently lacks a neighbour and
        // **never draws**. Streaming exactly `render_distance` made singleplayer
        // silently lose its last ring of chunks — reported as "some water far away
        // is blocky", because a large flat surface is where a missing outer ring
        // reads as a hard step rather than as absent scenery.
        //
        // This does not widen the view: fog and the far plane read
        // `config.render_distance` directly, not this value.
        let view_radius = i32::try_from(self.config.render_distance)
            .unwrap_or(i32::MAX)
            .saturating_add(1);
        #[cfg(not(target_arch = "wasm32"))]
        let launch_result = if online_mode {
            launch_open_to_lan_online(
                self.config.protocol,
                view_radius,
                session,
                seed,
                world_type,
                world_dir,
            )
        } else {
            launch_singleplayer(
                self.config.protocol,
                view_radius,
                session,
                seed,
                world_type,
                world_dir,
            )
        };
        #[cfg(target_arch = "wasm32")]
        let launch_result = launch_singleplayer(
            self.config.protocol,
            view_radius,
            session,
            seed,
            world_type,
        );
        match launch_result {
            Ok(net) => {
                self.sim.attach_net(net);
                // Issue #449: the loading screen's denominator. Declared only
                // for singleplayer, because here we *asked* for this view radius
                // and the integrated server streams exactly the square it
                // implies. A multiplayer server clamps our requested view
                // distance to its own, so the same number would be an upper
                // bound there — a bar that stalls at 70% and reads as a hang.
                // Better no bar than a wrong one.
                self.sim
                    .set_view_radius(u32::try_from(view_radius).unwrap_or(0));
                self.install_session_render_sources();
            }
            // Reported, never routed around: the only cause is a build with no
            // hostable version family, and telling the player that is strictly
            // better than a world that silently never loads.
            Err(e) => self
                .ui
                .session_failed(crate::sim::SessionEnd::failed(lodestone_model::Text::literal(e.to_string()))),
        }
        // Remembered for Open to LAN (issue #535), which republishes this exact
        // launch on a TCP port. Recorded even on the error arm above: a failed
        // launch left no session, and the field is only ever read behind one.
        self.hosted_world = Some(launch_for_lan);
    }

    /// Open a live connection to `host:port` and show the loading screen.
    ///
    /// Factored out of `resumed` because the menu's Join button needs the exact
    /// same sequence, including the entity light sampler — which must be
    /// installed at connect time, not after login (see the long note at the
    /// `resumed` call site for why).
    pub(super) fn connect_to(&mut self, host: String, port: u16) {
        // Issue #449: leave the menu for the `Connecting` screen *before*
        // dialing, mirroring `begin_singleplayer`. Without this, multiplayer
        // never shows a loading screen for the handshake/configuration phase —
        // the screen stays on the server list until `session_ready()` flips it
        // straight to Playing.
        self.ui.begin(crate::menu::SessionKind::Multiplayer);
        // §4.1(c): `Sim::connect` builds the client *with* the shell's one `World`
        // and attaches it, so the render sources below are installed from the
        // already-attached client's shared handle rather than from a `NetClient`
        // this function still owns. `shared_handle` survives the move either way
        // (it is an `Arc<OnceLock<_>>` the net thread publishes into).
        self.sim.connect(host, port, self.config.protocol);
        self.install_session_render_sources();
    }

    /// Install every render source a live session feeds, for **either** session
    /// kind: the fog/sky clock, the entity light sampler, the sky pass and the
    /// screen-effect overlays, plus the outline and debug-line sources.
    ///
    /// Shared by [`Self::connect_to`] and [`Self::begin_singleplayer`] (issue
    /// #287) rather than duplicated, because a source installed for one session
    /// kind and not the other is invisible until someone plays the other one —
    /// and the two differ *only* in transport (see `net.rs`'s `Origin`). A no-op
    /// when there is no session or no GPU yet, so it is safe to call from either
    /// path unconditionally.
    fn install_session_render_sources(&mut self) {
        // `sky_clock.get().map(|h| h.world_time().1)` used to be handed to
        // `set_time_of_day_source` directly. `WorldTime` is a flat snapshot the
        // network thread only overwrites on a decoded `SET_TIME`
        // (`ClientEvent::TimeChanged` — `lodestone-client/src/state.rs`), and the
        // server sends that roughly once per second
        // (`docs/served-session-liveness.md`'s `TIME_SYNC_INTERVAL`), so the raw
        // value steps once/sec instead of advancing per frame. That produced the
        // reported once-a-second cloud "teleport" (`sky.rs::cloud_plane_geometry`'s
        // `scroll_x` is `time_of_day * CLOUD_SCROLL_BLOCKS_PER_TICK`, so a
        // once/sec step is a visible ~0.6-block jump).
        //
        // `ContinuousTimeOfDay::advance` wraps the same raw value with a local,
        // wall-clock extrapolation between packets — the same trick vanilla's own
        // client-side day-time prediction uses, and it keeps `sky.rs` itself
        // clock-agnostic per its own module docs ("there is deliberately no
        // second clock... anywhere in this module"): the extrapolation lives here,
        // at the render-source boundary, not inside the sky module.
        //
        // The handle comes from the already-attached client rather than from a
        // `NetClient` a caller still owns; `shared_handle` survives the move
        // either way (it is an `Arc<OnceLock<_>>` the net thread publishes into).
        let Some(net_handle) = self.sim.net().map(crate::net::NetClient::shared_handle) else {
            return;
        };
        // The weather cell, cloned out for the same reason `shared_handle` is: the
        // `NetClient` is moved into `Sim::attach_net` and the closures below outlive
        // it. Re-created on every connect so a new session starts clear.
        let weather = self
            .sim
            .net()
            .map(|net| Arc::new(WeatherTracker::new(net.shared_weather())));
        self.weather = weather.clone();
        // The dimension's absent-sky-light policy, cloned out for the same reason as
        // the two above. The entity-light closure is installed **once** and must
        // still be right after a portal, so it reads the policy per call from this
        // cell rather than capturing today's value — `Sim::refresh_mesh_policy`
        // publishes into it. See `net::SkyDefaultCell`.
        let sky_policy = self
            .sim
            .net()
            .map(crate::net::NetClient::shared_sky_default);
        if let Some(render) = self.render.as_mut() {
            let handle = net_handle.clone();
            let light_policy = sky_policy.clone();
            // Terrain and mobs must read the same clock: `RenderState` folds this
            // factor into the fog lane both the model and entity passes sample.
            // Installing it for one and not the other makes mobs darker than the
            // blocks they stand on at midnight.
            let clock = net_handle.clone();
            // The dimension's own ambient floor rides beside the darken lane
            // rather than inside it: `sky_darken` is a time-of-day curve that
            // weather modifies, while this is a constant of wherever the player
            // currently is. Folding them would make one a second writer of the
            // other's value.
            let ambient_handle = net_handle.clone();
            // The sky pass's own clock — see `set_time_of_day_source`'s doc for
            // why it needs the raw tick rather than `set_sky_darken_source`'s
            // already-derived factor.
            let sky_clock = net_handle;
            let continuous_time_of_day = ContinuousTimeOfDay::new();
            // Weather rides *this* lane rather than getting one of its own.
            // `EnvironmentAttributes.SKY_LIGHT_FACTOR` is a single attribute in
            // vanilla too: the time-of-day curve is its base and
            // `WeatherAttributes`' two layers modify it
            // (`WeatherAttributes.java`, `:30`), so a separate uniform would be
            // a second writer of one value and the two would drift. This is the
            // exact `sky_darken` `lodestone_render::light`'s module doc derives,
            // and terrain, mobs and the first-person arm all read it through the
            // same fog lane — so one line here darkens all three under a storm.
            let darken_weather = weather.clone();
            render.set_sky_darken_source(move || {
                let base = clock.get().map(|h| {
                    lodestone_render::entity::sky_darken_for_time_of_day(h.world_time().1)
                })?;
                Some(match &darken_weather {
                    Some(w) => lodestone_render::weather_sky_light_factor(base, &w.state()),
                    None => base,
                })
            });
            // Without this the Nether renders the overworld's own ambient floor
            // and reads far darker than vanilla — the shaders carried the
            // overworld grey as a constant until the wire started supplying the
            // real per-dimension colour. `None` is the honest answer before the
            // dimension type is known, and the source is polled every frame, so
            // there is nothing to wait for.
            render.set_ambient_light_source(move || {
                let dim = ambient_handle.get()?.player().dimension_type?;
                Some(match dim.ambient_light_color {
                    Some(packed) => lodestone_render::light::rgb24_to_channels(packed),
                    None => lodestone_render::light::OVERWORLD_AMBIENT_LIGHT,
                })
            });
            render.set_entity_light_source(move |feet| {
                crate::net::entity_light_at(
                    &handle,
                    feet.x.floor() as i32,
                    feet.y.floor() as i32,
                    feet.z.floor() as i32,
                    // Read per call, not captured: a portal changes this mid-session.
                    light_policy.as_ref().map_or(
                        lodestone_render::SkyDefault::Full,
                        |cell| cell.get(),
                    ),
                )
            });
            render.set_time_of_day_source(move || {
                sky_clock
                    .get()
                    .map(|h| continuous_time_of_day.advance(h.world_time().1))
            });
        }
        // The sky pass itself needs GPU handles `RenderState::set_*_source`'s
        // closures don't (it uploads the celestial atlas + cloud texture
        // immediately, via `crate::resources::load_sky`), so it is installed
        // from a separate `self.gpu`/`self.target` borrow rather than folded
        // into the block above. `has_sky` guards a re-connect from re-loading
        // and re-uploading the same jar's textures a second time.
        if let (Some(gpu), Some(target)) = (self.gpu.as_ref(), self.target.as_ref()) {
            let (device, queue, format) = (gpu.device(), gpu.queue(), target.format());
            if let Some(render) = self.render.as_mut()
                && !render.has_sky()
                && let Some(sky) = crate::resources::load_sky(device, queue, format)
            {
                render.install_sky(sky);
            }
            // The underwater/fire overlay pass (issues #108, #112): same
            // shape and same reason as the sky install just above (needs GPU
            // handles immediately, so it is loaded here rather than folded
            // into a `set_*_source` closure). `has_screen_effects` guards a
            // re-connect the same way `has_sky` does.
            if let Some(render) = self.render.as_mut()
                && !render.has_screen_effects()
                && let Some(fx) = crate::resources::load_screen_effects(device, queue, format)
            {
                render.install_screen_effects(fx);
            }
            // The rain/snow pass: same shape and same `has_*` re-connect guard as
            // the two above. Note this is only the *droplets* — a jar-less run
            // still darkens correctly, because that half went in through
            // `set_sky_darken_source` and `set_fog` above.
            if let Some(render) = self.render.as_mut()
                && !render.has_weather()
                && let Some(textures) = crate::resources::load_weather_textures()
            {
                render.install_weather(device, queue, format, &textures);
            }
        }
        self.install_outline_source();
        self.install_debug_lines_source();
        self.install_plugin_billboards_source();
    }
}

/// The next game mode in F3+F4's cycle: survival → creative → adventure →
/// spectator → survival.
///
/// A free function so the cycle is testable without a window, a GPU or a live
/// session — the same split [`crate::app::offhand_swap_action`] makes. An unknown
/// current mode (no session, or a server that has not reported one) starts at
/// creative, which is where a host reaching for this chord is going.
pub(super) fn next_game_mode(current: Option<lodestone_model::GameMode>) -> lodestone_model::GameMode {
    use lodestone_model::GameMode;
    match current {
        Some(GameMode::Survival) => GameMode::Creative,
        Some(GameMode::Creative) => GameMode::Adventure,
        Some(GameMode::Adventure) => GameMode::Spectator,
        Some(GameMode::Spectator) => GameMode::Survival,
        None => GameMode::Creative,
    }
}
