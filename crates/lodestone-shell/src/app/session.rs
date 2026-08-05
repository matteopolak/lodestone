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
        let mut ecs = lodestone_ecs::app::App::new();
        ecs.add_plugins(lodestone_ecs::CorePlugin);
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
            tab_held: false,
            // Read from `options.json` via the same loader the menu uses.
            // Missing, partial or corrupt is vanilla's defaults, never an error
            // — see `Keybinds::from_json_value`.
            keybinds: crate::config::Options::load().keybinds,
            chat_input: ChatInput::new(),
            menu_input: MenuInput::new(),
            shift_held: false,
            ctrl_held: false,
            scroll_accum: 0.0,
            last_menu_click: None,
            fps_ema: 0.0,
            last_log: Instant::now(),
            applied_fog,
            ecs,
            recipe_book: None,
            recipe_book_revision: 0,
            recipe_panel: RecipePanelState::default(),
            recipe_toasts: lodestone_game::recipe::RecipeToastQueue::new(),
            // No session yet, so no weather cell to read; see
            // `install_session_render_sources`.
            weather: None,
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
        match self.sim.session_phase() {
            // LocalOnly never drives the menu — the dev world is already Playing.
            SessionPhase::LocalOnly | SessionPhase::Connecting => {}
            SessionPhase::Connected => self.ui.session_ready(),
            SessionPhase::Ended(reason) => {
                // Only transition in once; re-setting every frame would keep
                // re-latching the same reason (harmless but wasteful).
                if self.ui.screen() != crate::menu::Screen::Error {
                    self.ui.session_failed(reason.clone());
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
        if self.sim.is_dead() {
            if !self.ui.is_death() {
                self.ui.die(self.sim.death_message().map(str::to_string));
            }
        } else if self.ui.is_death() {
            self.ui.respawn_confirmed();
        }
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
        render.set_debug_lines_source(move || {
            lodestone_ecs::hold_read(&ecs, |world| {
                crate::gpu::debug_line_vertices(&world.resource::<lodestone_ecs::DebugLines>().0)
            })
        });
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
    pub(super) fn begin_singleplayer(&mut self, config: Option<crate::menu::create_world::WorldCreationConfig>) {
        self.ui.begin(crate::menu::SessionKind::Singleplayer);
        let seed = resolve_launch_seed(config.as_ref());
        let session = Some((self.sim.ecs().clone(), self.sim.local_player()));
        // Vanilla streams `simulationDistance`/`viewDistance` chunks around the
        // player; ours is the same number the camera's far plane and the mesher
        // already use, so the server never sends a column the renderer would
        // discard and never withholds one it wants.
        //
        // **Plus one, and the `+ 1` is not slack — it is the buffer ring the
        // mesher's invariant requires.** Vanilla's own server tracks
        // `center + viewDistance + 1` (`ChunkTrackingView.java:92, 96`), and it has
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
        match launch_singleplayer(self.config.protocol, view_radius, session, seed) {
            Ok(net) => {
                self.sim.attach_net(net);
                self.install_session_render_sources();
            }
            // Reported, never routed around: the only cause is a build with no
            // hostable version family, and telling the player that is strictly
            // better than a world that silently never loads.
            Err(e) => self.ui.session_failed(e.to_string()),
        }
    }

    /// Open a live connection to `host:port` and show the loading screen.
    ///
    /// Factored out of `resumed` because the menu's Join button needs the exact
    /// same sequence, including the entity light sampler — which must be
    /// installed at connect time, not after login (see the long note at the
    /// `resumed` call site for why).
    pub(super) fn connect_to(&mut self, host: String, port: u16) {
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
            // The sky pass's own clock — see `set_time_of_day_source`'s doc for
            // why it needs the raw tick rather than `set_sky_darken_source`'s
            // already-derived factor.
            let sky_clock = net_handle;
            let continuous_time_of_day = ContinuousTimeOfDay::new();
            // Weather rides *this* lane rather than getting one of its own.
            // `EnvironmentAttributes.SKY_LIGHT_FACTOR` is a single attribute in
            // vanilla too: the time-of-day curve is its base and
            // `WeatherAttributes`' two layers modify it
            // (`WeatherAttributes.java:19`, `:30`), so a separate uniform would be
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
    }
}
