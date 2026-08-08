//! The winit `ApplicationHandler`: window lifecycle and raw event routing.
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

impl ApplicationHandler for WindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let attrs = Window::default_attributes()
            .with_title("Lodestone")
            .with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0));
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };

        let (gpu, target) = match attach_window(window.clone()) {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("failed to attach GPU to window: {e}");
                event_loop.exit();
                return;
            }
        };

        let (w, h) = target.size();
        let format = target.format();
        let mut render = RenderState::new(
            gpu.device(),
            gpu.queue(),
            format,
            w,
            h,
            self.sim.vanilla_atlas(),
        );
        // Size the sky fog to our real render distance so terrain fades into the
        // sky where chunks actually stop, not at the render crate's 8-chunk default.
        render.set_fog(sky_fog(self.config.render_distance), self.config.render_distance);
        // The F3 overlay's fixed adapter block (see `DebugStats::adapter`).
        // Resolved **once**, here, because `Adapter::get_info` is the only place
        // this is knowable and it never changes for the process — and because the
        // field is otherwise an island: it was added with a draw and no producer,
        // so the lines it exists to show reached zero pixels.
        self.sim.stats.adapter = adapter_lines(&gpu);
        // Upload the stitched particle sheet the emitter already resolves its
        // flame/smoke/crit UVs against (issue #45). `load_particle_atlas` is
        // memoised, so this is the **same** `ParticleAtlas` object `Sim` built
        // its `(Sheet, frame) -> UV` table from — not a second stitch that
        // happens to pack the same way. The bug being closed here is a UV table
        // addressing a different image than the one bound, and every counter
        // reads perfectly healthy while it is happening, so the identity is made
        // structural rather than assumed.
        if let Some(sheet) = crate::resources::load_particle_atlas() {
            render.install_particle_sheet_atlas(gpu.device(), gpu.queue(), sheet.atlas());
        }
        let mut hud = HudRenderer::new(gpu.device(), format);
        // Attach the vanilla GUI sprite atlas so the survival vitals draw from
        // real textures; on a jar-less run this is `None` and the HUD keeps its
        // procedural fallback.
        if let Some(gui) = crate::resources::load_gui_atlas() {
            hud.attach_gui(gpu.device(), gpu.queue(), format, gui);
        }
        // Load the real crafting-recipe corpus from `client.jar`, once. Feeds
        // the container screen's ghost-preview draw and the debug-overlay
        // counter; a jar-less run leaves this `None` and neither draws.
        //
        // Issue #148: the corpus is *adopted into* `lodestone_ecs::RecipeRegistry`
        // rather than assigned straight to the field, so every recipe a plugin
        // registered during `Plugin::build` — which ran long before this point —
        // is folded in before anything reads it. `self.recipe_book` is now a
        // revision-gated cache of that resource; see `Self::sync_recipe_book`.
        self.adopt_recipe_corpus(crate::resources::load_recipe_book());
        // Attach the flat item-sprite atlas so hotbar/container slots draw real
        // item icons; jar-less runs leave this `None` and slots stay empty wells.
        // Loaded once and shared: the container screen needs the same atlas, and
        // `ItemAtlas` is behind an `Arc` precisely so the second consumer is a
        // refcount bump rather than a second stitch of the whole item corpus.
        let item_atlas = crate::resources::load_item_atlas();
        // The 2-D GUI glint sheet, shared by the hotbar and the container screen.
        // Loaded once here rather than twice below; `None` on a jar-less run, in
        // which case enchanted icons draw without their shimmer.
        let glint_sheet = item_atlas
            .as_ref()
            .and_then(|_| crate::resources::load_glint_texture());
        if let Some(items) = item_atlas.clone() {
            hud.attach_items(gpu.device(), gpu.queue(), format, items);
            if let Some(img) = &glint_sheet {
                hud.attach_glint(gpu.device(), gpu.queue(), format, img);
            }
        }
        // Attach the 3-D block-item pass, which borrows the world renderer's own
        // block atlas, tint palette and animation slots rather than uploading a
        // second copy of any of them. Present only on the live vanilla path (the
        // demo world bakes no models), where block items would otherwise draw an
        // empty well.
        if let (Some(atlas_view), Some(atlas_sampler), Some(palette), Some(anim)) = (
            render.model_atlas_view(),
            render.model_atlas_sampler(),
            render.model_palette_buffer(),
            render.model_anim_buffer(),
        ) {
            hud.attach_item_models(
                gpu.device(),
                format,
                atlas_view,
                atlas_sampler,
                palette,
                anim,
            );
        }
        let effects = EffectsRenderer::new(gpu.device(), format);

        // The container screen draws real item icons through the *same* shared
        // pass the hotbar uses (`hud::item_icon`), so both must be attached or
        // slots fall back to hash-derived colour swatches. Without this the
        // capability is complete, gated and reaches zero pixels — the island
        // pattern this project has hit eleven times.
        let mut container = ContainerRenderer::new(gpu.device(), format);
        if let Some(items) = item_atlas {
            container.attach_items(gpu.device(), gpu.queue(), format, items);
            if let Some(img) = &glint_sheet {
                container.attach_glint(gpu.device(), gpu.queue(), format, img);
            }
        }
        if let (Some(atlas_view), Some(atlas_sampler), Some(palette), Some(anim)) = (
            render.model_atlas_view(),
            render.model_atlas_sampler(),
            render.model_palette_buffer(),
            render.model_anim_buffer(),
        ) {
            container.attach_item_models(
                gpu.device(),
                format,
                atlas_view,
                atlas_sampler,
                palette,
                anim,
            );
        }
        // Vanilla's real `container/*.png` panel art (issue #51). A jar-less
        // run leaves this `None` and the screen keeps its flat programmatic
        // fill — the same "is a thing attached" degradation as the two calls
        // above.
        if let Some(background) = crate::resources::load_container_background() {
            container.attach_background(gpu.device(), gpu.queue(), format, background);
        }
        // The inventory avatar — the player standing in the panel's recess with
        // their head following the cursor (player report: "right now theres just a
        // black box where the player should be"). This call is the whole
        // difference between the capability existing and reaching zero pixels:
        // `ContainerRenderer` starts with it detached and draws nothing.
        // `false` on a jar-less run, where the recess stays empty.
        if !container.attach_player_preview(gpu.device(), gpu.queue(), format) {
            tracing::info!(
                target: "assets",
                "no player skin sheet: the inventory avatar will not draw"
            );
        }

        // Upload whatever has already meshed; the rest streams in per frame.
        for meshed in self.sim.drain_meshes() {
            render.upload_section(gpu.device(), gpu.queue(), meshed.key, &meshed.mesh);
        }

        let menu = MenuRenderer::new(gpu.device(), format);

        // Choose the session per config. A connection target on the command line
        // dials it immediately (and shows a loading screen until login);
        // otherwise the window opens on the **main menu**, which is now the GUI
        // entry point. Singleplayer from the menu enters the local worldgen world
        // — *not* the integrated server, which isn't wired yet (see
        // `WindowApp::begin_singleplayer`).
        if requested_a_connection(&self.config) {
            self.ui.begin(SessionKind::Multiplayer);
            self.sim.connect(
                self.config.host.clone(),
                self.config.port,
                self.config.protocol,
            );
            let net = self
                .sim
                .net()
                .expect("Sim::connect always leaves a client attached");
            // Install the entity light sampler now, at connect time, not after
            // login: `set_entity_light_source` wants a `'static` closure
            // installed *once*, and the shared handle it needs is available
            // immediately (it is an `Arc<OnceLock<_>>` the net thread resolves
            // later — see `net::SharedHandle`). Waiting for `LoggedIn` would
            // just delay the install for no benefit, since the closure already
            // tolerates an unresolved handle (`entity_light_at` reads `None`
            // and the sampler falls back to full-bright, exactly matching the
            // "no world yet" state during connect). This has to happen before
            // `attach_net` moves `net` into `self.sim` — `NetClient` itself
            // isn't `Clone` and doesn't outlive this function, only the shared
            // handle inside it does.
            let entity_light_handle = net.shared_handle();
            // See `connect_to`: same clock for terrain and mobs, installed here
            // too because this is the second, independent connect path.
            let clock = net.shared_handle();
            // See `connect_to`: the sky pass's own clock, next to (but distinct
            // from) `set_sky_darken_source`'s already-derived factor.
            let sky_clock = net.shared_handle();
            // See `connect_to`: extrapolates between the ~1/sec `SET_TIME`
            // packets so the cloud scroll advances smoothly instead of
            // stepping once a second.
            let continuous_time_of_day = ContinuousTimeOfDay::new();
            // See `install_session_render_sources` for why weather rides the
            // `sky_darken` lane rather than getting its own uniform. Installed on
            // this path too, or a `--connect` launch renders a storm at full
            // daylight brightness while a menu-launched session does not: the
            // duplicated-source hazard this whole function's doc warns about.
            let weather = Arc::new(WeatherTracker::new(net.shared_weather()));
            self.weather = Some(weather.clone());
            render.set_sky_darken_source(move || {
                let base = clock.get().map(|h| {
                    lodestone_render::entity::sky_darken_for_time_of_day(h.world_time().1)
                })?;
                Some(lodestone_render::weather_sky_light_factor(
                    base,
                    &weather.state(),
                ))
            });
            // Same cell as `install_session_render_sources`, installed on this path
            // too for the reason that function's doc gives about duplicated
            // sources: a `--connect` launch that skipped it would black out mobs in
            // open air while a menu-launched session did not.
            let light_policy = net.shared_sky_default();
            render.set_entity_light_source(move |feet| {
                crate::net::entity_light_at(
                    &entity_light_handle,
                    feet.x.floor() as i32,
                    feet.y.floor() as i32,
                    feet.z.floor() as i32,
                    // Read per call, not captured: a portal changes this mid-session.
                    light_policy.get(),
                )
            });
            render.set_time_of_day_source(move || {
                sky_clock
                    .get()
                    .map(|h| continuous_time_of_day.advance(h.world_time().1))
            });
            // See `install_session_render_sources`: the sky pass itself, from the
            // GPU handles this path already has locally (`self.gpu`/`self.target`
            // are not set until the end of this function).
            if !render.has_sky()
                && let Some(sky) = crate::resources::load_sky(gpu.device(), gpu.queue(), format)
            {
                render.install_sky(sky);
            }
            // See `install_session_render_sources`: the overlay pass, from the
            // same local GPU handles.
            if !render.has_screen_effects()
                && let Some(fx) =
                    crate::resources::load_screen_effects(gpu.device(), gpu.queue(), format)
            {
                render.install_screen_effects(fx);
            }
            // See `install_session_render_sources`: the rain/snow pass, from the
            // same local GPU handles.
            if !render.has_weather()
                && let Some(textures) = crate::resources::load_weather_textures()
            {
                render.install_weather(gpu.device(), gpu.queue(), format, &textures);
            }
            // See `install_session_render_sources`: the enchantment-glint pass, from
            // the same local GPU handles. `install_glint` uploads the sheet as
            // `Rgba8Unorm` — see `gpu/glint.rs`'s module doc for why that is the
            // opposite of every diffuse loader and deliberate.
            if !render.has_glint()
                && let Some(img) = crate::resources::load_glint_texture()
            {
                render.install_glint(gpu.device(), gpu.queue(), format, &img);
            }
        }
        // No target requested: stay on `Screen::MainMenu`, which `UiState::new`
        // already put us on. Nothing else to do.

        self.window = Some(window);
        self.gpu = Some(gpu);
        self.target = Some(target);
        self.render = Some(render);
        // Now that `self.render` exists and `attach_net` has already run above,
        // the outline source can be installed on this path too.
        self.install_outline_source();
        // Debug lines need no connection at all (see the method doc), so this
        // is the one call that actually matters — the two above are just
        // keeping the three connect paths uniform.
        self.install_debug_lines_source();
        self.hud = Some(hud);
        self.effects = Some(effects);
        self.container = Some(container);
        self.menu = Some(menu);
        // Grab only if the chosen screen wants it (menus and loading: no).
        self.set_grab(self.ui.wants_cursor_grab());
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        match event {
            WindowEvent::CloseRequested => event_loop.exit(),
            WindowEvent::Resized(size) => {
                if let (Some(gpu), Some(target), Some(render)) = (
                    self.gpu.as_ref(),
                    self.target.as_mut(),
                    self.render.as_mut(),
                ) {
                    target.resize(gpu.device(), size.width, size.height);
                    render.resize(gpu.device(), size.width, size.height);
                }
            }
            WindowEvent::Focused(false) => {
                // Losing focus pauses (and releases the pointer) so we don't
                // keep grabbing the mouse of a backgrounded window. The *world*
                // is not paused by this: `Screen::Paused` is local UI state and
                // the sim keeps ticking (see `FramePacer`), which is what keeps
                // keep-alives and movement flowing to the server.
                self.ui.pause();
                self.set_grab(false);
                self.pacer.set_focused(false);
            }
            WindowEvent::Focused(true) => {
                // Presentation resumes at full rate. The pointer is *not*
                // re-grabbed here — the player clicks to resume, as before.
                self.pacer.set_focused(true);
            }
            WindowEvent::Occluded(occluded) => {
                // Fully covered or minimised: there is nothing on screen to
                // update and acquiring a drawable is what stalls, so drop
                // presentation entirely while continuing to tick.
                self.pacer.set_occluded(occluded);
            }
            // Hovering a menu row highlights it, so the mouse and the keyboard
            // drive one selection rather than two. `Screen::Paused` shares this
            // arm too even though it is not `owns_frame` — see `menu_row_at`'s
            // doc — because it has its own row navigation to hover just like
            // every screen this renderer owns.
            //
            // `routes_menu_input` and not the `owns_frame(..) || is_paused() ||
            // is_death()` this used to spell out: that expression was copied
            // here, into the click arm below, into `KeyGate::menu`, and a
            // fourth time into the test that was supposed to police it, so
            // `Screen::CommandBlockEdit` could be missing from all four at once
            // and nothing failed (#474). See that function's doc.
            // Advancements (#167) tracks the cursor for its hover *and* its
            // viewport pan. Its own arm before the menu one below, which would
            // otherwise try to hover a menu row on a screen that has none.
            WindowEvent::CursorMoved { position, .. } if self.ui.is_advancements() => {
                self.cursor = (position.x as f32, position.y as f32);
                if let Some((w, h)) = self.target.as_ref().map(RenderTarget::size) {
                    self.drag_advancements(w, h);
                }
            }
            WindowEvent::MouseInput { state, button, .. } if self.ui.is_advancements() => {
                if let (MouseButton::Left, Some((w, h))) =
                    (button, self.target.as_ref().map(RenderTarget::size))
                {
                    match state {
                        ElementState::Pressed => {
                            self.handle_advancements_click(w, h);
                        }
                        ElementState::Released => self.advancements_drag = None,
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } if self.ui.is_advancements() => {
                if let Some((w, h)) = self.target.as_ref().map(RenderTarget::size) {
                    // Vanilla's `mouseScrolled` passes `scrollY` straight into
                    // `AdvancementTab.scroll(0, scrollY * 16)`, so the notch count
                    // goes through verbatim.
                    self.scroll_advancements(wheel_notches(delta) as f32, w, h);
                }
            }
            WindowEvent::CursorMoved { position, .. }
                if crate::menu::nav::routes_menu_input(&self.ui) =>
            {
                self.cursor = (position.x as f32, position.y as f32);
                // A slider drag in progress owns the cursor: vanilla's
                // `AbstractSliderButton.onDrag` keeps calling
                // `setValueFromMouse` for as long as the button is held, whether
                // or not the cursor is still inside the widget. Hovering (and
                // therefore re-highlighting some other row) while dragging would
                // both move the keyboard cursor mid-drag and, worse, make the
                // slider stop following once you left its row.
                match self.menu_slider_drag {
                    Some(row) => {
                        if let Some(fraction) =
                            self.menu_slider_fraction(row, self.cursor.0, self.cursor.1)
                        {
                            self.nav.drag_slider(&self.ui, row, fraction);
                        }
                    }
                    None => {
                        if let Some(row) = self.menu_row_at(self.cursor.0, self.cursor.1) {
                            self.nav.hover(&self.ui, row);
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. }
                if crate::menu::nav::routes_menu_input(&self.ui) =>
            {
                if state == ElementState::Pressed {
                    // Issue #15's other capture half: a mouse-button rebind
                    // (vanilla defaults `key.attack` to the left button,
                    // `key.pickItem` to the middle one — real cases, not
                    // hypothetical) needs *any* button, not only Left, and must
                    // run before the "click acts on the row under the cursor"
                    // branch below — otherwise a capture would immediately
                    // consume its own confirming click as a hover-row
                    // activation instead of finishing the rebind.
                    if self.nav.awaiting_key_capture() {
                        self.nav.capture_binding(Binding::Mouse(button));
                    } else if button == MouseButton::Left {
                        // Only a click *on a row* activates: clicking the backdrop
                        // must not confirm whatever happens to be highlighted.
                        //
                        // `MenuNav::click` and not `hover` + `MenuKey::Enter`: that
                        // pair is still what happens on every screen with a single
                        // row cursor and a single meaning of Enter, and it was wrong
                        // on the settings screen, which had no cursor and gave each
                        // control its own key. There, a click on the GUI SCALE row
                        // arrived as `Enter` and therefore as "toggle View Bobbing" —
                        // issue #391, where the whole bob chain was working and the
                        // option had been silently persisted off by a click on an
                        // unrelated row. Issue #55 gave that screen 135 controls and
                        // a real cursor, so a click now resolves its row to that
                        // row's own control; `MenuNav::click`'s doc has the history.
                        if let Some(row) = self.menu_row_at(self.cursor.0, self.cursor.1) {
                            // A slider takes the drag path, and takes it on the
                            // *press*: vanilla's `onClick` calls
                            // `setValueFromMouse` too, so a click anywhere on
                            // the track jumps the handle there rather than
                            // nudging one step. That is the whole of the
                            // "sliders just move a tiny bit on click" report —
                            // every slider used to route through
                            // `SettingsOutcome::Cycle`, a single wrapping step.
                            let dragged = self.nav.slider_row(&self.ui, row)
                                && self
                                    .menu_slider_fraction(row, self.cursor.0, self.cursor.1)
                                    .is_some_and(|f| {
                                        self.nav.drag_slider(&self.ui, row, f)
                                    });
                            if dragged {
                                self.menu_slider_drag = Some(row);
                            } else {
                                let action = self.nav.click(&mut self.ui, row);
                                self.apply_menu_action(action);
                            }
                        }
                    }
                }
                if state == ElementState::Released && button == MouseButton::Left {
                    // `AbstractSliderButton.onRelease`: the drag ends. Also the
                    // safety net for a release outside the row, which is where a
                    // sticky drag would otherwise come from.
                    self.menu_slider_drag = None;
                }
                // Every `owns_frame` action handles its own grab (each of them
                // either stays on a menu screen, which never grabs, or moves to
                // Playing through a path that already calls `set_grab`).
                // `PauseButton::BackToGame` does not — `handle_menu_key` only
                // calls `MenuNav::key`, which flips `UiState` to `Playing` and
                // returns, with nothing here to notice. Without this a click on
                // Back to Game resumes play with the pointer still released:
                // visible but unusable.
                let want = self.ui.wants_cursor_grab();
                if want != self.grabbed {
                    self.set_grab(want);
                }
            }
            // Track the cursor and, mid-drag, the slots it paints while a
            // container screen is up. This is a separate arm from the menu one
            // above because `Screen::Container` is not `owns_frame` — the
            // container overlay draws over the world, it does not replace it.
            WindowEvent::CursorMoved { position, .. } if self.ui.is_container_open() => {
                self.cursor = (position.x as f32, position.y as f32);
                // The creative screen's scrollbar drag (issue #158). Checked
                // first and exclusively: on that screen there is no slot layout
                // to paint a quick-craft across, so the drag below has nothing
                // to do.
                if self.creative_screen_open() {
                    if let Some((w, h)) = self.target.as_ref().map(RenderTarget::size) {
                        self.drag_creative_scroll(w, h);
                    }
                } else if self.menu_input.is_dragging() {
                    if let (Some(menu), Some((w, h))) = (
                        self.active_container_menu(),
                        self.target.as_ref().map(RenderTarget::size),
                    ) {
                        let hit = crate::container::hit_test_with_book(
                            &menu,
                            self.nav.gui_scale(),
                            w,
                            h,
                            self.cursor.0,
                            self.cursor.1,
                            // An open recipe book **moves the panel** (`redraw` passes
                            // the same flag to `ContainerFrame::with_book_open`), so an
                            // unshifted hit-test paints slots the pointer is not over.
                            self.recipe_panel.open,
                        );
                        // `&menu` supplies the cursor stack and the slot rules
                        // vanilla's `shouldAddSlotToQuickCraft` gate needs — see
                        // `MenuInput::dragged`, and issue #378 part 1 for what an
                        // unfiltered paint set costs.
                        self.menu_input.dragged(hit, &menu);
                    }
                }
            }
            // The creative screen (issue #158) owns every click while it is up: it
            // *replaces* the inventory screen rather than overlaying it (see
            // `creative_screen_open`), so falling through to the slot path would
            // click a panel that is not on screen. Its own arm rather than a
            // first-refusal call inside the arm below, for that reason.
            WindowEvent::MouseInput { state, button, .. }
                if self.ui.is_container_open() && self.creative_screen_open() =>
            {
                if let (Some(MenuButton::Left), Some((w, h))) = (
                    menu_button_for(button),
                    self.target.as_ref().map(RenderTarget::size),
                ) {
                    match state {
                        ElementState::Pressed => {
                            self.handle_creative_click(w, h);
                        }
                        // The thumb drag ends on release, wherever the pointer
                        // is — vanilla's `mouseReleased` sets `scrolling = false`
                        // unconditionally (`:513`).
                        ElementState::Released => self.creative.scrolling = false,
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } if self.ui.is_container_open() => {
                if let Some(menu_button) = menu_button_for(button)
                    && let (Some(menu), Some((w, h))) = (
                        self.active_container_menu(),
                        self.target.as_ref().map(RenderTarget::size),
                    )
                {
                    // The recipe-book panel gets first refusal on the click
                    // (issue #163). It overlaps the main panel's left edge at
                    // narrow canvases by `container.rs`'s documented design, so
                    // testing it *after* the slot layout would make its own
                    // widgets unclickable there. Only a press is offered: a
                    // release landing on the panel must still reach
                    // `MenuInput::release` so an in-flight drag that started on
                    // a real slot can terminate.
                    // Deliberately not an early `return`: the tail of
                    // `window_event` latches `quit_requested`, and returning
                    // from here would skip it.
                    let consumed_by_recipe_panel = matches!(state, ElementState::Pressed)
                        && menu_button == MenuButton::Left
                        && self.handle_recipe_panel_click(&menu, w, h);
                    if !consumed_by_recipe_panel {
                        // **`hit_test_with_book`, not `hit_test_with_scale`.** This is
                        // the one click path that was still testing against an
                        // unshifted panel while `redraw` drew a shifted one
                        // (`ContainerFrame::with_book_open`) — the exact hazard that
                        // module's doc warns about, and with the book open it lands
                        // every click a panel-offset to the left of what is on screen.
                        // The swap/drop/pick-item paths in `container_input.rs` already
                        // passed the flag; only the mouse did not.
                        let hit = crate::container::hit_test_with_book(
                            &menu,
                            self.nav.gui_scale(),
                            w,
                            h,
                            self.cursor.0,
                            self.cursor.1,
                            self.recipe_panel.open,
                        );
                        let ctx = MenuContext {
                            cursor_loaded: menu.carried().is_some(),
                            // No game-mode plumbing exists on `Sim` to source this
                            // from yet — see the report on this change.
                            creative: false,
                        };
                        let clicks = match state {
                            ElementState::Pressed => {
                                let now = Instant::now();
                                let is_repeat = menu_button == MenuButton::Left
                                    && self.last_menu_click.is_some_and(|t| {
                                        now.duration_since(t) < DOUBLE_CLICK_WINDOW
                                    });
                                self.last_menu_click = Some(now);
                                self.menu_input
                                    .press(hit, menu_button, self.shift_held, ctx, is_repeat, &menu)
                            }
                            ElementState::Released => {
                                self.menu_input
                                    .release(hit, menu_button, self.shift_held, ctx, &menu)
                            }
                        };
                        for click in clicks {
                            self.send_menu_click(click);
                        }
                    }
                }
            }
            WindowEvent::MouseInput { state, button, .. } => {
                // `Screen::Paused` no longer reaches this catch-all at all — the
                // `owns_frame(...) || self.ui.is_paused()` arm above now handles
                // every click while paused (hover + activate the highlighted
                // pause-menu row, including Back to Game via `MenuKey::Enter`).
                if self.grabbed {
                    // `key.attack` mines (hold-to-mine on live; one-shot break on
                    // demo) and `key.use` uses/places against the targeted face.
                    // Both default to a mouse button — left and right
                    // respectively — which is exactly why `Binding` has to be
                    // able to hold a mouse button and not just a key.
                    match (mouse_action_for(&self.keybinds, button), state) {
                        (Some(InputAction::Attack), ElementState::Pressed) => {
                            self.sim.begin_attack();
                        }
                        (Some(InputAction::Attack), ElementState::Released) => {
                            self.sim.end_attack();
                        }
                        (Some(InputAction::Use), ElementState::Pressed) => {
                            self.sim.use_item();
                        }
                        (Some(InputAction::Use), ElementState::Released) => {
                            self.sim.end_use();
                        }
                        // Middle-click by default (`Options.java:669` binds
                        // `key.pickItem` to `Type.MOUSE, 2`), so unlike
                        // attack/use this is the *primary* route rather than the
                        // rebound one. Press-only: `pickBlockOrEntity` is a
                        // one-shot with no release edge.
                        (Some(InputAction::PickItem), ElementState::Pressed) => {
                            self.sim.pick_block_or_entity(self.ctrl_held);
                        }
                        // A movement action bound to a mouse button still drives
                        // the controller, on both edges.
                        (Some(action), _) => {
                            if let Some(movement) = action.movement() {
                                let held = state == ElementState::Pressed;
                                self.sim.input_mut(|i| i.set(movement, held));
                            }
                        }
                        _ => {}
                    }
                }
            }
            // Scroll cycles the hotbar (down = right, like vanilla) only
            // during active play; menus and the chat prompt ignore it. The
            // step is scaled by `mouseWheelSensitivity` (issue #203) through
            // the same fractional accumulator vanilla's `ScrollWheelHandler`
            // uses, so sensitivity below 1.0 can take more than one notch to
            // move a slot and sensitivity above 1.0 can cross several in one
            // notch — not just a threshold on the existing ±1 step.
            // The creative grid scrolls by whole rows (issue #158). Its own arm
            // and placed first, because none of the arms below can see it: the
            // hotbar's is gated on `accepts_gameplay_input`, which an open
            // container makes false, and `scroll_active_list` only knows about
            // `MenuNav`'s list screens. That is exactly the gap that left every
            // non-list screen ignoring the wheel entirely.
            WindowEvent::MouseWheel { delta, .. } if self.creative_screen_open() => {
                // Vanilla's `subtractInputFromScroll` takes the raw `scrollY`, so
                // the notch count goes through verbatim — no `accumulate_scroll`,
                // which exists for the hotbar's discrete-slot quantization.
                self.scroll_creative_screen(wheel_notches(delta) as f32);
            }
            WindowEvent::MouseWheel { delta, .. } if self.ui.accepts_gameplay_input() => {
                let dy = wheel_notches(delta);
                let scaled = scale_scroll(dy, self.nav.discrete_mouse_scroll(), self.nav.mouse_wheel_sensitivity());
                let step = accumulate_scroll(&mut self.scroll_accum, scaled);
                if step != 0 {
                    self.sim.cycle_slot(-step);
                }
            }
            // The multiplayer server list (issues #402, #445): the notch count
            // goes through **verbatim**, as vanilla's `scrollY`, and
            // `MenuNav::scroll_server_list` turns it into
            // `scrollY * scrollRate()` pixels — 18 px for a 36 px row
            // (`AbstractScrollArea.java:34`, `AbstractSelectionList.java:44`).
            //
            // **This used to collapse `dy` to `-1`/`0`/`+1` rows**, and that was
            // the owner's bug report: a list that jumps a whole 36 px entry per
            // notch instead of scrolling. The information was destroyed here, at
            // the input, not in the geometry — a row index cannot represent the
            // half-entry position vanilla lands on, so no amount of work
            // downstream could have recovered it. Passing the real `dy` also
            // makes a trackpad's fractional `PixelDelta` move proportionally
            // rather than snapping to a whole row.
            //
            // Deliberately not run through `accumulate_scroll`, which exists for
            // the hotbar's sub-notch *quantization* — the opposite problem:
            // `cycle_slot` takes a discrete slot step, so it has to accumulate
            // fractions until one whole step is due. A pixel offset needs no
            // accumulator, because a fraction of a notch is already a meaningful
            // number of pixels.
            //
            // Needs the *real* canvas height, which this handler has via
            // `RenderTarget::size` and `gui_scale`, unlike keyboard
            // scroll-into-view which uses the canvas-independent window estimate
            // (see `MenuNav::scroll_server_list`'s doc).
            // **One arm for every menu list, not one per screen.** This used to be
            // gated on `self.ui.screen() == Screen::ServerList`, which meant `app/`
            // contained exactly two `MouseWheel` arms — the hotbar's and the
            // multiplayer list's — and every other list screen ignored the wheel
            // completely. Not "jumped by a row": did not respond at all. The gate is
            // now `MenuNav::scroll_active_list`'s own answer, so a screen that
            // declares a `ListSpec` scrolls here for free and a screen that does not
            // falls through to the arms below unchanged.
            // Gated on `owns_frame` — the same predicate the click and hover arms
            // above use, so "the wheel reaches this screen" and "a click reaches this
            // screen" cannot drift apart. A screen inside that set with no list is
            // handled by `scroll_active_list` returning `false`, not by a second
            // predicate here.
            WindowEvent::MouseWheel { delta, .. }
                if crate::menu::render::owns_frame(self.ui.screen()) =>
            {
                let dy = wheel_notches(delta);
                // The same boundary transform the hotbar arm above uses, because
                // vanilla computes it **once** for both: `MouseHandler.onScroll`
                // (`MouseHandler.java:189-192`) hands one `scaledYOffset` to
                // `screen().mouseScrolled(..)` and to `ScrollWheelHandler` alike.
                // Deliberately **not** run through `accumulate_scroll`, which exists
                // for the hotbar's sub-notch *quantization*: `cycle_slot` takes a
                // discrete slot step so it must carry fractions until one is due,
                // while a pixel offset needs no accumulator — a fraction of a notch
                // is already a meaningful number of pixels.
                let dy = scale_scroll(dy, self.nav.discrete_mouse_scroll(), self.nav.mouse_wheel_sensitivity());
                if dy != 0.0
                    && let Some((fb_w, fb_h)) = self.target.as_ref().map(RenderTarget::size)
                {
                    let (_, canvas_h) =
                        crate::menu::render::logical_canvas(self.nav.gui_scale(), fb_w, fb_h);
                    self.nav
                        .scroll_active_list(&self.ui, dy as f32, canvas_h);
                }
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let pressed = event.state == ElementState::Pressed;

                // Tracked unconditionally (not gated on `accepts_gameplay_input`
                // like the movement bindings below): a container shift-click is a
                // `QuickMove`, not movement, and must still work while gameplay
                // input is not being accepted.
                //
                // **Deliberately still a literal key, and vanilla agrees**: it
                // checks `Screen.hasShiftDown()` — the raw modifier state — not
                // `options.keyShift`, so rebinding sneak does *not* move
                // shift-click. Same boundary as `menu_button_for`: container
                // gestures are UI chrome, not gameplay bindings. Both shifts
                // count, because this is asking "is a shift modifier down".
                if let PhysicalKey::Code(code) = event.physical_key
                    && matches!(code, KeyCode::ShiftLeft | KeyCode::ShiftRight)
                {
                    self.shift_held = pressed;
                }
                // Same tracking, for Control — `resolve_key`'s `ctrl` parameter.
                // Deliberately a running flag rather than read off this event:
                // `key.drop` is a different physical key from Control, so the
                // modifier's state has to outlive the keypress that changed it.
                if let PhysicalKey::Code(code) = event.physical_key
                    && matches!(code, KeyCode::ControlLeft | KeyCode::ControlRight)
                {
                    self.ctrl_held = pressed;
                }

                // Resolve *what this key means* before touching any state, then
                // perform the one side effect it names. The precedence lives in
                // [`resolve_key`] — a pure function, so the swallowing order can
                // be unit-tested without a window (see its docs and the tests at
                // the bottom of this file). This match is only the effects half.
                let gate = KeyGate {
                    // The same predicate the hover and click arms above use
                    // (#474). It was the same *expression* before, written out
                    // three times; a screen missing from one copy is a screen
                    // whose clicks or keys silently vanish, and that is what
                    // happened to `Screen::CommandBlockEdit` — the command box
                    // could not be typed into because this copy excluded it too.
                    menu: crate::menu::nav::routes_menu_input(&self.ui),
                    chat_open: self.ui.is_chat_open(),
                    // `active_container_menu`, **not** `ui.is_container_open()`.
                    // That flag only tracks the *locally* opened player inventory;
                    // a server-opened menu (crafting table, chest, furnace) lives
                    // in `sim.open_menu()` and leaves it `false`. Reading the flag
                    // meant the swallow arm never fired for a server menu, so the
                    // inventory binding could not close a crafting table and every
                    // gameplay key stayed live behind it. This is the same
                    // predicate `redraw` draws from, so hit-testing, drawing and
                    // key dispatch cannot disagree about what is on screen.
                    container_open: self.active_container_menu().is_some(),
                    gameplay: self.ui.accepts_gameplay_input(),
                    debug_held: self.debug_held,
                    // The recipe book's search box. **Gated on `open` as well as
                    // `search_focused`**: the focus flag is only cleared by a
                    // click, so closing the panel with the box focused would
                    // otherwise leave every key routed into an invisible field.
                    recipe_search: self.recipe_panel.open && self.recipe_panel.search_focused,
                    // Gated on the screen being up as well as the box being
                    // focused, for the same reason `recipe_search` is: the focus
                    // flag persists across close, so closing the screen with the
                    // box focused would otherwise leave every key routed into an
                    // invisible field.
                    creative_search: self.creative_search_active(),
                };
                let code = match event.physical_key {
                    PhysicalKey::Code(code) => Some(code),
                    _ => None,
                };
                // Resolved into a local first so the immutable borrow of
                // `self.keybinds` ends before the `&mut self` calls below.
                let outcome = resolve_key(&self.keybinds, gate, code, pressed, self.ctrl_held);
                match outcome {
                    Some(KeyOutcome::Menu) => {
                        // Issue #15's last hop: a bind button mid-capture needs the
                        // *next raw key*, not `menu_key_for`'s translation —
                        // `menu_key_for` silently drops any physical key with no
                        // printable `text` (F-keys, modifiers, arrows other than
                        // Up/Down), which is exactly the common rebind case
                        // (`docs/keybindings.md`'s "Wiring the Controls menu").
                        // Checked *before* calling `menu_key_for` at all, not only
                        // when it returns `None`: a capture target can be a
                        // printable key too, and `menu_key_for` would otherwise
                        // consume it as `MenuKey::Char` first.
                        if pressed && self.nav.awaiting_key_capture() {
                            match capture_key_for(event.physical_key) {
                                Some(CaptureKey::Cancel) => {
                                    self.handle_menu_key(MenuKey::Escape);
                                }
                                Some(CaptureKey::Bind(code)) => {
                                    self.nav.capture_binding(Binding::Key(code));
                                }
                                None => {}
                            }
                        } else if pressed
                            && let Some(key) = Self::menu_key_for(&event)
                        {
                            self.handle_menu_key(key);
                            // Entering the world grabs; leaving it releases.
                            let want = self.ui.wants_cursor_grab();
                            if want != self.grabbed {
                                self.set_grab(want);
                            }
                        }
                    }
                    Some(KeyOutcome::Chat) => {
                        if pressed {
                            self.handle_chat_key(&event);
                        }
                    }
                    Some(KeyOutcome::Pause) => {
                        // Escape on a container screen **closes the container and
                        // returns to gameplay** — it does not open the pause menu.
                        // That is `Screen.onClose()` in vanilla, and it is why this
                        // is an `else` rather than a close followed by `on_escape`:
                        // the old form paused *as well*, leaving the pause menu
                        // drawn over a menu that was still open server-side.
                        //
                        // Also note it must clear both halves. `close_open_menu`
                        // only releases the *server* menu; `close_container` clears
                        // the local inventory flag. Whichever one was showing, the
                        // other is already false and clearing it is a no-op.
                        if self.active_container_menu().is_some() {
                            self.sim.close_open_menu();
                            self.ui.close_container();
                        } else {
                            // Context-sensitive: Playing↔Paused, Error→menu, etc.
                            self.ui.on_escape();
                        }
                        self.set_grab(self.ui.wants_cursor_grab());
                    }
                    Some(KeyOutcome::CloseContainer) => {
                        self.sim.close_open_menu();
                        self.ui.close_container();
                        self.set_grab(self.ui.wants_cursor_grab());
                    }
                    Some(KeyOutcome::ToggleDebugOverlay) => {
                        // Toggle the debug instrument (§S4). Unlike older
                        // vanilla, 26.2 makes this a real `KeyMapping`, so it
                        // belongs in the table — see `keybinds`' module docs.
                        self.show_debug = !self.show_debug;
                    }
                    Some(KeyOutcome::DebugModifier(down)) => {
                        // Issue #197. Vanilla's
                        // `keyDebugModifier.setDown(!didDebugAction)`
                        // (`KeyboardHandler.java:554-555`): the overlay toggles
                        // on the **release**, and only if no chord consumed the
                        // hold. Without that, F3+B would both open the overlay
                        // and toggle hitboxes on one keystroke.
                        self.debug_held = down;
                        if down {
                            self.debug_chord_used = false;
                        } else if !self.debug_chord_used {
                            self.show_debug = !self.show_debug;
                        }
                    }
                    Some(KeyOutcome::ToggleHitboxes) => {
                        use std::sync::atomic::Ordering;
                        self.debug_chord_used = true;
                        let was = self.debug_hitboxes.load(Ordering::Relaxed);
                        self.debug_hitboxes.store(!was, Ordering::Relaxed);
                    }
                    Some(KeyOutcome::ToggleChunkBorders) => {
                        use std::sync::atomic::Ordering;
                        self.debug_chord_used = true;
                        let was = self.debug_chunk_borders.load(Ordering::Relaxed);
                        self.debug_chunk_borders.store(!was, Ordering::Relaxed);
                    }
                    Some(KeyOutcome::ToggleSpectator) => {
                        self.debug_chord_used = true;
                        self.toggle_spectator();
                    }
                    Some(KeyOutcome::CycleGameMode) => {
                        self.debug_chord_used = true;
                        self.cycle_game_mode();
                    }
                    Some(KeyOutcome::RecipeSearch) => {
                        // `RecipePanelState::search` had **no writer at all** —
                        // the field was read by `RecipeBook::browse` and (since
                        // the search box learned to draw) by the layout, and
                        // written by nothing. The owner's "I can't type in the
                        // search bar" is that island, one layer up from the
                        // missing draw.
                        //
                        // `event.text` rather than a `KeyCode` mapping: winit has
                        // already applied the keyboard layout, which is the same
                        // reason `menu_key_for` reads it. Control characters are
                        // filtered out because winit reports Backspace/Enter with
                        // `text` set on some platforms.
                        if code == Some(KeyCode::Backspace) {
                            self.recipe_panel.search.pop();
                            self.recipe_panel.page = 0;
                        } else if let Some(text) = event.text.as_deref() {
                            let mut typed = false;
                            for ch in text.chars().filter(|c| !c.is_control()) {
                                // `searchBox.setMaxLength(50)`
                                // (`RecipeBookComponent.java:126`).
                                if self.recipe_panel.search.chars().count() < RECIPE_SEARCH_MAX_LEN {
                                    self.recipe_panel.search.push(ch);
                                    typed = true;
                                }
                            }
                            if typed {
                                self.recipe_panel.page = 0;
                            }
                        }
                        // Back to page 0 on any edit, both branches:
                        // `checkSearchStringUpdate` calls `updateCollections`,
                        // which re-pages from the start. Without it, narrowing a
                        // search while parked on page 3 shows an empty grid —
                        // `recipe_book_panel_contents` would clamp it, but to the
                        // *last* page rather than the first, which is not where a
                        // player expects a fresh search to begin.
                    }
                    Some(KeyOutcome::CreativeSearch) => {
                        // Same shape as the recipe box above, through
                        // `edit_creative_search` so the max-length rule and the
                        // scroll reset live with the rest of the screen's state
                        // rather than in this driver arm.
                        if code == Some(KeyCode::Backspace) {
                            self.edit_creative_search(CreativeSearchEdit::Backspace);
                        } else if let Some(text) = event.text.as_deref() {
                            for ch in text.chars().filter(|c| !c.is_control()) {
                                self.edit_creative_search(CreativeSearchEdit::Char(ch));
                            }
                        }
                    }
                    Some(KeyOutcome::ToggleAdvancedTooltips) => {
                        // A persisted option, not a render flag — see the
                        // outcome's own doc. `MenuNav` owns `Options` and writes
                        // `options.json` eagerly on every mutation, so this
                        // survives a crash the way every settings row does.
                        self.debug_chord_used = true;
                        self.nav.toggle_advanced_item_tooltips();
                    }
                    Some(KeyOutcome::Screenshot) => {
                        // Arm it; `redraw()` services it after the frame is
                        // drawn. Capturing here would read an undrawn (or
                        // stale) swapchain image — see the field's own doc.
                        self.pending_screenshot = true;
                    }
                    Some(KeyOutcome::PlayerList(held)) => self.tab_held = held,
                    Some(KeyOutcome::OpenChat { command }) => {
                        // Release held movement so we don't walk while typing.
                        self.sim.input_mut(InputState::release_all);
                        let _ = self.chat_input.take();
                        if command {
                            self.chat_input.push_char('/');
                        }
                        self.ui.open_chat();
                        self.tab_held = false;
                        self.set_grab(false);
                    }
                    Some(KeyOutcome::OpenContainer) => {
                        self.sim.input_mut(InputState::release_all);
                        self.ui.open_container();
                        self.tab_held = false;
                        self.set_grab(false);
                    }
                    // Vanilla's own third-/first-person toggle.
                    Some(KeyOutcome::TogglePerspective) => self.sim.cycle_camera_type(),
                    Some(KeyOutcome::SelectSlot(slot)) => self.sim.select_slot(slot),
                    Some(KeyOutcome::ContainerSwap { button }) => {
                        self.send_container_swap(button);
                    }
                    Some(KeyOutcome::ContainerDrop { ctrl }) => {
                        self.send_container_drop(ctrl);
                    }
                    Some(KeyOutcome::ContainerPickItem) => self.send_container_pick_item(),
                    Some(KeyOutcome::Drop { ctrl }) => self.send_drop_selected(ctrl),
                    Some(KeyOutcome::PickItem { ctrl }) => self.sim.pick_block_or_entity(ctrl),
                    // The *other* off-hand route (#385): no screen, no slot, a
                    // bare `ServerboundPlayerAction`. Sent straight through
                    // `NetClient` rather than queued into `ActionQueue`, which is
                    // the sanctioned shape for a per-frame input-driven action —
                    // see `interact.rs`' module doc on why `end_attack`,
                    // `use_item_live` and `send_chat` do the same.
                    Some(KeyOutcome::SwapOffhand) => self.send_offhand_swap(),
                    Some(KeyOutcome::Attack(true)) => self.sim.begin_attack(),
                    Some(KeyOutcome::Attack(false)) => self.sim.end_attack(),
                    Some(KeyOutcome::Use(true)) => self.try_use(),
                    Some(KeyOutcome::Use(false)) => self.sim.end_use(),
                    Some(KeyOutcome::Movement(action, held)) => {
                        self.sim.input_mut(|i| i.set(action, held));
                    }
                    // Either nothing is bound to this key, or a screen above
                    // swallowed it. Both are "do nothing", deliberately.
                    None => {}
                }
            }
            WindowEvent::RedrawRequested => self.redraw(),
            _ => {}
        }

        // Clean shutdown path: any handler may latch a quit request.
        if self.ui.quit_requested() {
            event_loop.exit();
        }
    }

    fn device_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _device_id: DeviceId,
        event: DeviceEvent,
    ) {
        if let DeviceEvent::MouseMotion { delta } = event
            && self.ui.is_playing()
            && self.grabbed
        {
            self.sim
                .input_mut(|i| i.add_mouse(delta.0 as f32, delta.1 as f32));
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        // Spin while focused (vsync paces the loop); otherwise sleep in short
        // `BACKGROUND_POLL` slices so a backgrounded window stops burning a core
        // yet still wakes far more often than the 20 Hz tick needs.
        event_loop.set_control_flow(self.pacer.control_flow(Instant::now()));
    }
}

/// The recipe-corpus seam between `lodestone_ecs::RecipeRegistry` — which
/// plugins write to — and `WindowApp::recipe_book`, which the container screen
/// reads. Issue #148.
///
/// Two functions rather than one because they answer different questions.
/// [`WindowApp::adopt_recipe_corpus`] runs once, at GPU bring-up, and is the only
/// thing that *installs* `client.jar`'s corpus; `sync_recipe_book` runs every
/// frame and only notices that someone else changed it.
impl WindowApp {
    /// Hands a freshly loaded corpus to the registry and takes the merged book
    /// back, so plugin recipes registered during `Plugin::build` are present
    /// before the first frame draws.
    ///
    /// `None` (a jar-less run) is still adopted deliberately: a plugin's recipes
    /// must be craftable on a run with no `client.jar`, and refusing to adopt
    /// would leave the registry unadopted and its recipes pending forever.
    pub(super) fn adopt_recipe_corpus(&mut self, corpus: Option<RecipeBook>) {
        let corpus = corpus.unwrap_or_default();
        let (book, revision) = lodestone_ecs::hold_write(self.sim.ecs(), |world| {
            let mut registry = world.get_resource_or_insert_with(
                lodestone_ecs::RecipeRegistry::default,
            );
            registry.adopt_corpus(corpus);
            (registry.snapshot(), registry.revision())
        });
        self.recipe_book_revision = revision;
        // An empty book stays `None`, which is what every read site already
        // treats as "no corpus" — a jar-less run with no plugin recipes must
        // keep drawing exactly as it did before this seam existed.
        self.recipe_book = (!book.is_empty()).then_some(book);
    }

    /// Re-clones the corpus if the registry's revision has moved since the last
    /// clone — i.e. if a plugin registered or unregistered a recipe.
    ///
    /// The revision gate is what makes this callable per frame: the steady state
    /// is one `u64` read under a short read guard, with no clone and no
    /// allocation. Without it, a 1585-recipe clone every frame would be a real
    /// cost for a feature almost no session uses.
    pub(super) fn sync_recipe_book(&mut self) {
        let fresh = lodestone_ecs::hold_read(self.sim.ecs(), |world| {
            let registry = world.get_resource::<lodestone_ecs::RecipeRegistry>()?;
            (registry.revision() != self.recipe_book_revision)
                .then(|| (registry.snapshot(), registry.revision()))
        });
        if let Some((book, revision)) = fresh {
            self.recipe_book_revision = revision;
            self.recipe_book = (!book.is_empty()).then_some(book);
        }
    }
}
