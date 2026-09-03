//! The winit `ApplicationHandler`: window lifecycle and raw event routing.
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

impl ApplicationHandler<ShellEvent> for WindowApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        // A session started through `run_headless_session` wants
        // no window at all until an `AppEvent::AttachPresentation` arrives —
        // see `WindowApp::presentation_desired`'s own doc. Every existing
        // caller (`WindowApp::new`) seeds this `true`, so this guard is a
        // no-op for them and `resumed` creates a window exactly as it always
        // has.
        #[cfg(feature = "runtime-presentation")]
        if !self.presentation_desired {
            return;
        }
        let mut attrs = window_attributes(&self.config);
        if self.config.benchmark.is_some() {
            let Some(monitor) = benchmark_builtin_monitor(event_loop) else {
                eprintln!("benchmark requires a discoverable built-in laptop display");
                event_loop.exit();
                return;
            };
            tracing::info!(
                target: "frame_benchmark",
                native_id = ?monitor_native_id(&monitor),
                name = ?monitor.name(),
                position = ?monitor.position(),
                size = ?monitor.size(),
                "selected hardware built-in display for fullscreen benchmark"
            );
            attrs = attrs.with_fullscreen(Some(Fullscreen::Borderless(Some(monitor))));
            #[cfg(target_os = "macos")]
            {
                use winit::platform::macos::WindowAttributesExtMacOS;

                // Unlike ordinary borderless fullscreen, this also hides the
                // menu bar and Dock instead of letting either overlay the
                // laptop's fullscreen drawable.
                attrs = attrs.with_borderless_game(true);
            }
        }
        // Native: adapter/device selection blocks, so the whole bring-up finishes
        // inside this one callback exactly as it always did.
        //
        // `create_and_attach_window` (`app::session`) is the shared window +
        // GPU + `RenderState` bring-up factored out so a runtime
        // attach (`WindowApp::attach_presentation`) is not a second,
        // slightly-different copy of this. Startup keeps its own failure
        // handling (`event_loop.exit()`, unlike a runtime attach, which stays
        // headless) since a window this app cannot draw into is fatal here in
        // a way it is not for an already-running headless session.
        #[cfg(not(target_arch = "wasm32"))]
        if !self.create_and_attach_window(event_loop, attrs) {
            event_loop.exit();
        }
        #[cfg(target_arch = "wasm32")]
        let window = match event_loop.create_window(attrs) {
            Ok(w) => Arc::new(w),
            Err(e) => {
                eprintln!("failed to create window: {e}");
                event_loop.exit();
                return;
            }
        };

        // Browser: `resumed` is a *synchronous* winit callback, and adapter/device
        // selection is genuinely asynchronous — `pollster::block_on` on a browser main
        // thread cannot finish, because there is no other thread to make the future
        // progress. So the bring-up is split at exactly that seam: kick the attach off
        // on the microtask queue and let a later `about_to_wait` finish it.
        //
        // The window is parked here, before the GPU exists, so this callback's own
        // `self.window.is_some()` guard stops a second `resumed` creating a second
        // window and a second attach. Every field the deferred half fills is already
        // `Option` and every consumer already reads them as such — `redraw` starts with
        // `self.gpu.as_ref()` — so "no GPU for a frame or two" is a state this app
        // could already represent. That is what makes the split cheap rather than a
        // rewrite.
        #[cfg(target_arch = "wasm32")]
        {
            self.window = Some(window.clone());
            wasm_bindgen_futures::spawn_local(async move {
                match lodestone_render::window::attach_window_async(window).await {
                    Ok(pair) => PENDING_GPU.with_borrow_mut(|slot| *slot = Some(pair)),
                    // Not `event_loop.exit()`: we are outside the callback and have no
                    // `ActiveEventLoop`. Nothing else can draw, so say why loudly and
                    // leave the page up — a blank canvas with an explanation beats a
                    // silently dead tab.
                    Err(e) => tracing::error!(
                        target: "gpu",
                        "failed to attach GPU to the canvas: {e}. This build needs WebGPU."
                    ),
                }
            });
        }
    }

    /// The runtime toggle: everything that lets a caller outside
    /// this event loop attach or detach presentation on a running session —
    /// the mechanism a runtime switch needs ("attach things with the bevy
    /// systems at runtime when switching ... as long as we can also remove
    /// them"). Delivered through winit's own `user_event` callback, which
    /// (like every `ApplicationHandler` method) carries a live
    /// `&ActiveEventLoop`, so a window can be created here exactly as
    /// `resumed` creates one — see `WindowApp::attach_presentation`.
    ///
    /// Native-only in practice: `AppEvent` only exists behind
    /// `runtime-presentation`, and the one producer today
    /// (`app::runners::run_headless_session`) is itself
    /// `cfg(not(target_arch = "wasm32"))` — see that function's own doc for
    /// why. The browser target still compiles this impl (the trait method is
    /// generic over `ShellEvent`), it simply never receives one.
    // Native-only, matching `WindowApp::attach_presentation`/
    // `detach_presentation` (`app::session`), the two methods every non-`Quit`
    // arm below reaches: a wasm32 build with the feature on still needs to
    // compile, and those two are themselves target-gated because a browser's
    // bring-up is the async `attach_window_async` path, not this synchronous
    // one. Nothing on wasm32 constructs an `AppEvent` either way — the one
    // producer, `app::runners::run_headless_session`, is itself native-only.
    #[cfg(all(not(target_arch = "wasm32"), feature = "runtime-presentation"))]
    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: AppEvent) {
        match event {
            AppEvent::AttachPresentation { enable_input } => {
                self.attach_presentation(event_loop, enable_input);
            }
            AppEvent::ArmInput(armed) => {
                self.input_armed = armed;
                tracing::info!(target: "presentation", armed, "input arm state changed");
            }
            AppEvent::DetachPresentation => self.detach_presentation(),
            AppEvent::Quit => event_loop.exit(),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        _window_id: WindowId,
        event: WindowEvent,
    ) {
        // Vanilla's own framerate-limit tracker resets its AFK clock on input,
        // called from the keyboard and mouse handlers
        // (key press, mouse press, scroll) — deliberately **not**
        // `CursorMoved`, which vanilla never routes through it. Resets the AFK
        // clock the inactivity FPS limit reads (`app::pacing::effective_target_fps`).
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        ) {
            self.pacer.record_input(Instant::now());
        }
        // Browser: an `AudioContext` created before any user gesture starts
        // `suspended` and stays that way with **no error** — the identical
        // failure shape this crate already measured for a gesture-less
        // `request_pointer_lock()` (see the `WindowEvent::MouseInput` arm below
        // that re-issues the grab from inside a real click for that exact
        // reason). `Sim::resume_audio_on_gesture` is a no-op on native and,
        // once the context is already running, an idempotent one here too —
        // `AudioContext::resume()` on a running context just resolves
        // immediately — so this can run on every press with nothing to guard.
        //
        // A key or mouse press is unambiguously a real user gesture per the
        // HTML spec's "sticky activation" rules; a wheel event is deliberately
        // excluded from this check even though it shares the pacer's input
        // above — the spec does not count it as activating, so calling
        // `resume()` from one would be asking the browser to honour a gesture
        // it never granted.
        if matches!(event, WindowEvent::MouseInput { state: ElementState::Pressed, .. })
            || matches!(
                event,
                WindowEvent::KeyboardInput {
                    event: winit::event::KeyEvent { state: ElementState::Pressed, .. },
                    ..
                }
            )
        {
            self.sim.resume_audio_on_gesture();
        }
        // The resolved open question on input while a window is attached at
        // runtime: input is inert on an attached
        // window by default, with explicit opt-in. A window
        // `AppEvent::AttachPresentation` created on a previously headless
        // session starts with `input_armed: false` (see that field's own
        // doc), so a script driving the client does not suddenly start
        // receiving an operator's keystrokes just because someone attached a
        // window to watch it — only an explicit `AppEvent::ArmInput(true)`
        // lets these four kinds reach gameplay. Ordinary startup
        // (`WindowApp::new`) seeds `input_armed: true`, so this is a no-op
        // there and play is unaffected. Window management (resize, focus,
        // close, redraw, modifier tracking) is untouched below — an unarmed
        // window still behaves like a window, it just cannot act on
        // keyboard/mouse.
        #[cfg(feature = "runtime-presentation")]
        if !self.input_armed
            && matches!(
                event,
                WindowEvent::KeyboardInput { .. }
                    | WindowEvent::MouseInput { .. }
                    | WindowEvent::MouseWheel { .. }
                    | WindowEvent::CursorMoved { .. }
            )
        {
            return;
        }
        match event {
            // Winit reports modifier state as its own event rather than
            // attaching it to every `KeyboardInput`, and nothing in this
            // crate tracked it before now — every real `winit::event::KeyEvent`
            // reaching `menu_key_for`/`handle_chat_key` therefore looked
            // unmodified, so Cmd+A was indistinguishable from `a` (menu inputs
            // typed the shortcut's letter instead of acting on it).
            // `self.modifiers` is the one place that state now lives;
            // `mods.state()` is winit's own post-`Modifiers` accessor (the
            // struct also exposes a raw `ModifiersKeyState` per key, which
            // nothing here needs).
            WindowEvent::ModifiersChanged(mods) => {
                self.modifiers = mods.state();
            }
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
                //
                // Gated on the pause-on-lost-focus option (F3+P) — vanilla's
                // own lost-focus handling pauses only when the
                // option is on; the pointer release and the
                // pacer's background throttle are a different mechanism (mouse
                // capture, frame rate). The opt-in benchmark keeps foreground
                // pacing so an incidental focus notification cannot change the
                // measured workload; ordinary play keeps the existing option.
                if should_background_pace(&self.config) && self.nav.pause_on_lost_focus() {
                    self.ui.pause();
                }
                self.set_grab(false);
                if should_background_pace(&self.config) {
                    self.pacer.set_focused(false);
                }
            }
            WindowEvent::Focused(true) => {
                // Presentation resumes at full rate. The pointer is *not*
                // re-grabbed here — the player clicks to resume, as before.
                self.pacer.set_focused(true);
                // The cheap half of folder-watch request: if the
                // Resource Packs screen is open, rescan the folder now — the
                // shape of "extract or drop a pack in with a file manager,
                // alt-tab back". See `MenuNav::refresh_open_resource_packs_screen`
                // for why this is not a real filesystem watcher and why that
                // is the deliberate choice here, not a shortcut.
                self.nav.refresh_open_resource_packs_screen();
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
            // The command-suggestion dropdown's pointer half — vanilla's
            // own click and scroll handling for the popup, plus the
            // hover tracking inside its list-rendering pass.
            //
            // Its own three arms rather than a branch inside the menu ones
            // because `Screen::Chat` is not `routes_menu_input`: the chat box
            // draws over a live world with the pointer released, so none of the
            // menu row machinery applies. They come first for the same reason
            // `handle_chat_key` gives the popup first refusal on keys.
            WindowEvent::CursorMoved { position, .. } if self.ui.is_chat_open() => {
                self.cursor = (position.x as f32, position.y as f32);
                // Hover **only when the pointer actually moved**, which is what
                // this arm firing already means (vanilla's own mouse-moved
                // handling gates on the position actually changing). Without that gate the row
                // under a stationary pointer would fight the arrow keys for the
                // selection every frame.
                if let Some(row) = self.suggestion_row_under_cursor() {
                    self.chat_input.suggestion_hover(row);
                }
            }
            WindowEvent::MouseInput { state, button, .. } if self.ui.is_chat_open() => {
                if state == ElementState::Pressed && button == MouseButton::Left {
                    if let Some(row) = self.suggestion_row_under_cursor() {
                        // The suggestion popup gets first refusal, matching
                        // vanilla's own click-handling precedence
                        // over the scrollback beneath it.
                        self.chat_input.suggestion_click(row);
                    } else {
                        // A click on a scrollback line's own click_event —
                        // see `dispatch_chat_click_under_cursor`'s doc for
                        // which actions run immediately and which do not.
                        self.dispatch_chat_click_under_cursor();
                    }
                }
            }
            WindowEvent::MouseWheel { delta, .. } if self.ui.is_chat_open() => {
                // Vanilla's own mouse-wheel handling requires the pointer to be inside the rect —
                // resolving a row is a stricter version of the same test, and the
                // one place both come from.
                if self.suggestion_row_under_cursor().is_some() {
                    self.chat_input.suggestion_scroll(wheel_notches(delta) as i32);
                } else {
                    // Vanilla's chat-screen scroll handling: the popup gets first refusal
                    // (the arm above, mirroring the command-suggestion popup's own
                    // scroll handling returning "handled"); everything else
                    // scrolls the scrollback itself. Vanilla clamps the raw
                    // notch to a single unit *before* applying the ×7 "not
                    // holding shift" multiplier, so a precise
                    // trackpad gesture is not amplified sevenfold — only a
                    // whole mouse-wheel click is.
                    let notch = wheel_notches(delta).clamp(-1.0, 1.0);
                    let notch = if self.shift_held { notch } else { notch * 7.0 };
                    let opts = self.nav.options();
                    let chat_opts = crate::hud::ChatDisplayOptions {
                        scale: opts.chat_scale,
                        width_pct: opts.chat_width,
                        height_pct_unfocused: opts.chat_height_unfocused,
                        height_pct_focused: opts.chat_height_focused,
                        line_spacing: opts.chat_line_spacing,
                        text_opacity: opts.chat_opacity,
                        background_opacity: opts.chat_background_opacity,
                        colors: opts.chat_colors,
                    };
                    let rows_per_page = crate::hud::chat_lines_per_page(
                        chat_opts,
                        crate::hud::chat_pose_scale(chat_opts),
                        true,
                    );
                    // The same 100-entry cap the per-frame sync/window fetch
                    // uses (`app/redraw.rs`) — `ChatFeed`'s own capacity, so
                    // this is the true total, not a windowed subset of it.
                    // `recent_chat_spans`, not `recent_chat`: only the count is
                    // read here, but the legacy `to_legacy_string`
                    // path has no remaining production reason to run at all.
                    let total = self.sim.recent_chat_spans(100).len();
                    self.chat_input.scroll_mut().scroll(notch as i32, total, rows_per_page);
                }
            }
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
                    // Vanilla's mouse-wheel handling passes the scroll delta straight into
                    // the advancement tab's own scroll, multiplied by 16, so the notch count
                    // goes through verbatim.
                    self.scroll_advancements(wheel_notches(delta) as f32, w, h);
                }
            }
            WindowEvent::CursorMoved { position, .. }
                if crate::menu::nav::routes_menu_input(&self.ui) =>
            {
                self.cursor = (position.x as f32, position.y as f32);
                // A slider drag in progress owns the cursor: vanilla's own
                // slider widget keeps updating its
                // value from the mouse position for as long as the button is held, whether
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
                        self.nav.capture_binding(Binding::Mouse(button.into()));
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
                                // Vanilla's own widget click handling: the click
                                // sound fires unconditionally before the click action, on
                                // every activating click — a slider is the one
                                // exception (vanilla's slider widget overrides
                                // the click sound to a no-op here and plays it on
                                // *release* instead, handled below), which is
                                // exactly why this arm is reached only on the
                                // non-`dragged` branch. This was the owner's
                                // literal report ("clicking menu buttons"): no
                                // call site anywhere in the shell played
                                // `ui.button.click` before this.
                                self.sim.play_ui_click_sound();
                                let action = self.nav.click(&mut self.ui, row);
                                self.apply_menu_action(action);
                            }
                        }
                    }
                }
                if state == ElementState::Released && button == MouseButton::Left {
                    // Vanilla's own slider-release handling: the drag ends, and
                    // *this* is where a slider's click sound plays (its
                    // click-sound override is a no-op on press — see the
                    // press arm above) — only when a drag was actually live,
                    // matching vanilla's release handling being a widget method that
                    // fires only for the widget that started the drag.
                    if self.menu_slider_drag.take().is_some() {
                        self.sim.play_ui_click_sound();
                    }
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
                // The creative screen's scrollbar drag. Checked
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
            // The creative screen owns every click while it is up: it
            // *replaces* the inventory screen rather than overlaying it (see
            // `creative_screen_open`), so falling through to the slot path would
            // click a panel that is not on screen. Its own arm rather than a
            // first-refusal call inside the arm below, for that reason.
            WindowEvent::MouseInput { state, button, .. }
                if self.ui.is_container_open() && self.creative_screen_open() =>
            {
                if let (Some(menu_button), Some((w, h))) = (
                    menu_button_for(button),
                    self.target.as_ref().map(RenderTarget::size),
                ) {
                    match state {
                        ElementState::Pressed => {
                            // Vanilla's own container-screen click handling three-way:
                            // the pick-item button clones (and it is *only* a clone for
                            // a player with instant-build permission, which this screen already
                            // guarantees), shift quick-moves, everything else picks up.
                            // Vanilla's raw button number is 0 for left and 1 for
                            // right, and the clone arm passes whichever button was used.
                            let (raw, input) = match menu_button {
                                MenuButton::Pick => (0, lodestone_game::click::ContainerInput::Clone),
                                MenuButton::Left if self.shift_held => {
                                    (0, lodestone_game::click::ContainerInput::QuickMove)
                                }
                                MenuButton::Right if self.shift_held => {
                                    (1, lodestone_game::click::ContainerInput::QuickMove)
                                }
                                MenuButton::Left => (0, lodestone_game::click::ContainerInput::Pickup),
                                MenuButton::Right => (1, lodestone_game::click::ContainerInput::Pickup),
                            };
                            self.handle_creative_click(raw, input, w, h);
                        }
                        // The thumb drag ends on release, wherever the pointer
                        // is — vanilla's `mouseReleased` sets `scrolling = false`
                        // unconditionally.
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
                    // The merchant trade-list buttons get first refusal, then
                    // the beacon's power/confirm/cancel buttons,
                    // then the recipe-book panel. The panel
                    // overlaps the main panel's left edge at narrow canvases by
                    // `container.rs`'s documented design, so testing it
                    // *after* the slot layout would make its own widgets
                    // unclickable there; the merchant and beacon buttons never
                    // overlap a real slot (nor, by construction, each other —
                    // each only ever fires on its own `special_layout`) but
                    // follow the same first-refusal shape for the reason
                    // `handle_merchant_click`'s own doc gives. Only a press is
                    // offered to any of these: a release landing there must
                    // still reach `MenuInput::release` so an in-flight drag
                    // that started on a real slot can terminate.
                    // Deliberately not an early `return`: the tail of
                    // `window_event` latches `quit_requested`, and returning
                    // from here would skip it.
                    let is_left_press =
                        matches!(state, ElementState::Pressed) && menu_button == MenuButton::Left;
                    let consumed_by_merchant =
                        is_left_press && self.handle_merchant_click(&menu, w, h);
                    let consumed_by_beacon = !consumed_by_merchant
                        && is_left_press
                        && self.handle_beacon_click(&menu, w, h);
                    // The enchanting table's three offer rows, same
                    // first-refusal shape as the beacon buttons just above
                    // (`ContainerButtonClick` remainder): never
                    // overlaps a real slot, and by construction never overlaps
                    // the beacon buttons either, since each only ever fires on
                    // its own `special_layout`.
                    let consumed_by_enchant = !consumed_by_merchant
                        && !consumed_by_beacon
                        && is_left_press
                        && self.handle_enchant_click(&menu, w, h);
                    // The stonecutter's recipe grid, same first-refusal shape
                    // and the same "never overlaps a real slot, never overlaps
                    // another special screen's own buttons" reasoning as
                    // `consumed_by_enchant` just above.
                    let consumed_by_stonecutter = !consumed_by_merchant
                        && !consumed_by_beacon
                        && !consumed_by_enchant
                        && is_left_press
                        && self.handle_stonecutter_click(&menu, w, h);
                    // The loom's pattern grid, same first-refusal shape as
                    // `consumed_by_stonecutter` just above (never overlaps a
                    // real slot, never overlaps another special screen's own
                    // buttons — each fires on its own `special_layout`).
                    let consumed_by_loom = !consumed_by_merchant
                        && !consumed_by_beacon
                        && !consumed_by_enchant
                        && !consumed_by_stonecutter
                        && is_left_press
                        && self.handle_loom_click(&menu, w, h);
                    let consumed_by_recipe_panel = !consumed_by_merchant
                        && !consumed_by_beacon
                        && !consumed_by_enchant
                        && !consumed_by_stonecutter
                        && !consumed_by_loom
                        && is_left_press
                        && self.handle_recipe_panel_click(&menu, w, h);
                    if !consumed_by_merchant
                        && !consumed_by_beacon
                        && !consumed_by_enchant
                        && !consumed_by_stonecutter
                        && !consumed_by_loom
                        && !consumed_by_recipe_panel
                    {
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
                        // The crafter's slot-disable toggle (issue #613's
                        // `SetContainerSlotState`) — a side effect alongside the
                        // ordinary click below, not part of the `consumed_by_*`
                        // chain above it; see `maybe_toggle_crafter_slot`'s own
                        // doc for why it must not consume. Gated on a plain
                        // (non-shift) press, matching `ContainerInput::PICKUP`.
                        if matches!(state, ElementState::Pressed) && !self.shift_held {
                            self.maybe_toggle_crafter_slot(&menu, hit);
                        }
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
                if self.pointer_really_locked() {
                    // `key.attack` mines (hold-to-mine on live; one-shot break on
                    // demo) and `key.use` uses/places against the targeted face.
                    // Both default to a mouse button — left and right
                    // respectively — which is exactly why `Binding` has to be
                    // able to hold a mouse button and not just a key.
                    match (mouse_action_for(&self.keybinds(), button), state) {
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
                        // Middle-click by default (vanilla's default options
                        // bind `key.pickItem` to the middle mouse button), so unlike
                        // attack/use this is the *primary* route rather than the
                        // rebound one. Press-only: vanilla's pick-block-or-entity is a
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
                } else if self.ui.is_playing() && state == ElementState::Pressed {
                    // Browser only: the world-entry grab in `drive_ui_from_session`
                    // fires from the per-frame reconciliation the instant
                    // `SessionPhase` becomes `Connected`, not from a user gesture, so
                    // the Pointer Lock API silently refuses it (see
                    // `pointer_really_locked`'s doc). This press *is* a real
                    // gesture — the one place on the gameplay screen guaranteed to
                    // run inside one — so re-issue the request here. `set_grab` is
                    // idempotent (`self.grabbed` is already true from the doomed
                    // automatic request), so this only ever *adds* a second, this
                    // time gesture-backed, `request_pointer_lock()` call; it is a
                    // pure no-op on native, where `pointer_really_locked()` already
                    // agreed with `self.grabbed` and this branch is unreachable.
                    self.set_grab(true);
                }
            }
            // Scroll cycles the hotbar (down = right, like vanilla) only
            // during active play; menus and the chat prompt ignore it. The
            // step is scaled by `mouseWheelSensitivity` through
            // the same fractional accumulator vanilla's `ScrollWheelHandler`
            // uses, so sensitivity below 1.0 can take more than one notch to
            // move a slot.
            //
            // **`accumulate_scroll`'s magnitude is not the slot count** (issue
            // #597): `getNextScrollWheelSelection`
            // collapses it to its sign, so the hotbar always advances exactly
            // one slot per scroll event no matter how many whole notches that
            // event's accumulator crossed — see `hotbar_scroll_step`'s own
            // docs. Passing the raw magnitude through was the owner's "scroll
            // a bit, nothing; scroll more, jumps six slots" report: a single
            // large trackpad `PixelDelta` event can cross several whole
            // notches at once.
            // The creative grid scrolls by whole rows. Its own arm
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
            // A bundle's scroll-to-select highlight (issue #616's
            // `BUNDLE_ITEM_SELECTED` / #613's `SelectBundleItem` remainder).
            // Gated the same way the container click arm above is
            // (`is_container_open`, not `active_container_menu().is_some()`
            // directly) so a bundle slot and a real click can never disagree
            // about whether a container screen is showing. Falls through to
            // nothing else when the hovered slot is not a scrollable bundle —
            // `handle_bundle_scroll` returns `false` and this arm does not
            // forward the notch anywhere, matching vanilla: an ordinary
            // container has no other use for the wheel.
            //
            // Falls through, in order, to the stonecutter's recipe grid and
            // the loom's pattern grid — `scroll_stonecutter`/`scroll_loom`
            // each return whether their own screen is even open, the same
            // "did this surface claim it" shape `handle_bundle_scroll` uses,
            // so a wheel notch over an *ordinary* container (no bundle slot
            // hovered, no stonecutter/loom open) still reaches nothing,
            // matching vanilla.
            WindowEvent::MouseWheel { delta, .. } if self.ui.is_container_open() => {
                let notches = wheel_notches(delta);
                let consumed_by_bundle = self
                    .target
                    .as_ref()
                    .map(RenderTarget::size)
                    .is_some_and(|(w, h)| self.handle_bundle_scroll(notches, w, h));
                if !consumed_by_bundle {
                    let _ = self.scroll_stonecutter(notches) || self.scroll_loom(notches);
                }
            }
            WindowEvent::MouseWheel { delta, .. } if self.ui.accepts_gameplay_input() => {
                let dy = wheel_notches(delta);
                let scaled = scale_scroll(dy, self.nav.discrete_mouse_scroll(), self.nav.mouse_wheel_sensitivity());
                let step = hotbar_scroll_step(accumulate_scroll(&mut self.scroll_accum, scaled));
                if step != 0 {
                    self.sim.cycle_slot(-step);
                }
            }
            // The multiplayer server list (issues #402, #445): the notch count
            // goes through **verbatim**, as vanilla's own scroll delta, and
            // `MenuNav::scroll_server_list` turns it into
            // delta times a per-list scroll rate, in pixels — 18 px for a 36 px row
            // (vanilla's own scroll-area and selection-list scrolling).
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
                // vanilla computes it **once** for both: its own mouse-scroll
                // handling scales the raw offset once and
                // hands the same scaled value to the screen's scroll handling and
                // to the hotbar-cycling path alike.
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
            WindowEvent::KeyboardInput { event, .. } => self.handle_keyboard_input(event),
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
        // `pointer_really_locked`, not the raw `self.grabbed` flag: on the browser
        // build `self.grabbed` can be true while the OS pointer was never actually
        // captured (a rejected, gesture-less lock request — see that method's doc),
        // and feeding an un-locked, edge-bounded `movementX`/`Y` through as look-delta
        // is exactly the "stops registering at the edge of the page" report. A no-op
        // change on native, where the two are the same value.
        #[cfg(feature = "runtime-presentation")]
        if !self.input_armed {
            // Same "inert on attach by default" contract as `window_event`'s
            // guard — raw look-delta is input too.
            return;
        }
        if let DeviceEvent::MouseMotion { delta } = event
            && self.ui.is_playing()
            && self.pointer_really_locked()
        {
            self.sim
                .input_mut(|i| i.add_mouse(delta.0 as f32, delta.1 as f32));
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        // Browser: collect the deferred GPU attach `resumed` kicked off, the first time
        // it is ready. This is the other half of the split — see `resumed` — and this is
        // the right place for it because it runs every loop turn and has `&mut self`,
        // which the `async` block that produced the pair could not hold.
        //
        // Ordered *before* `request_redraw` deliberately: the frame this enables should
        // be the one we then ask for, rather than the one after it.
        #[cfg(target_arch = "wasm32")]
        if self.gpu.is_none() {
            let pending = PENDING_GPU.with_borrow_mut(Option::take);
            if let Some((gpu, target)) = pending {
                // `resumed` parked the window before spawning the attach, so it is
                // present here. If it somehow is not, drop the pair rather than
                // inventing a window: `finish_bring_up` needs the real one the surface
                // was created from, and a second `resumed` will retry cleanly.
                if let Some(window) = self.window.clone() {
                    tracing::info!(target: "gpu", "GPU attached; finishing bring-up");
                    self.finish_bring_up(window, gpu, target);
                } else {
                    tracing::warn!(
                        target: "gpu",
                        "GPU attach landed with no window parked; discarding it"
                    );
                }
            }
        }
        // Browser: react to a `pointerlockchange` DOM event the listener recorded
        // since the last turn — see `reconcile_browser_pointer_lock_change`'s doc
        // for why this is Escape's *other* half on this target. Registering the
        // listener is idempotent and cheap, so it is simplest to just make sure it
        // exists every turn rather than threading a "did bring-up run yet" flag
        // through this function.
        #[cfg(target_arch = "wasm32")]
        {
            ensure_pointer_lock_change_listener();
            if POINTER_LOCK_CHANGED.with(|flag| flag.replace(false)) {
                self.reconcile_browser_pointer_lock_change();
            }
        }
        if let Some(window) = &self.window {
            window.request_redraw();
        }
        // No window means no `RedrawRequested` will ever fire, so
        // without this a headless session (never attached, or just detached)
        // would never tick — the sim would sit frozen instead of continuing
        // to connect/persist/keep-alive. `redraw()` already ticks the sim and
        // reconciles menu/session state **before** its GPU-readiness guard
        // (`app/redraw.rs`'s own comment: "Simulation must never be
        // conditional on a swapchain image") and returns as soon as it
        // reaches that guard with nothing to draw into — so calling it here
        // with no window is exactly the tick-with-no-presentation this mode
        // needs, reusing the same pacing/catch-up logic a windowed frame
        // uses rather than a second, divergent tick loop.
        #[cfg(feature = "runtime-presentation")]
        if self.window.is_none() {
            self.redraw();
        }
        // Spin while focused and uncapped (vsync paces the loop); sleep until the
        // next scheduled deadline while `framerateLimit`/`inactivityFpsLimit`
        // cap a focused window (see `FramePacer::control_flow`'s doc — this is
        // what keeps a low cap from becoming a busy-wait); otherwise sleep in
        // short `BACKGROUND_POLL` slices so a backgrounded window stops burning
        // a core yet still wakes far more often than the 20 Hz tick needs.
        let now = Instant::now();
        event_loop.set_control_flow(
            self.pacer
                .control_flow(now, self.current_target_fps(now)),
        );
    }
}

impl WindowApp {
    /// The physical-key handling body of `window_event`'s
    /// `WindowEvent::KeyboardInput` arm, factored out so a test can drive it
    /// directly with no `ActiveEventLoop` involved — this arm never touches
    /// one (the only `event_loop` use in `window_event` is `CloseRequested`
    /// and the post-match quit check, both outside this arm). `resolve_key`'s
    /// own tests are a *pure-function* check of `KeyOutcome` routing; this is
    /// the layer above it — the atomics, `push_local_chat`, and every other
    /// side effect a `KeyOutcome` match arm performs — which a resolver-level
    /// assertion cannot see.
    fn handle_keyboard_input(&mut self, event: winit::event::KeyEvent) {
        let pressed = event.state == ElementState::Pressed;

        // Tracked unconditionally (not gated on `accepts_gameplay_input`
        // like the movement bindings below): a container shift-click is a
        // `QuickMove`, not movement, and must still work while gameplay
        // input is not being accepted.
        //
        // **Deliberately still a literal key, and vanilla agrees**: it
        // checks the raw shift-modifier state — not
        // the sneak key binding, so rebinding sneak does *not* move
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
            anvil_rename_active: self.anvil_rename_active(),
            // Issue #613's `TeleportToEntity` remainder — see
            // `KeyGate::spectator`'s own doc.
            spectator: self.sim.is_spectator(),
        };
        let code = match event.physical_key {
            PhysicalKey::Code(code) => Some(code),
            _ => None,
        };
        // Issue #162: a plugin's claim on this physical key, read
        // once through a short ECS guard before `resolve_key` (a pure
        // function of plain data — see its own doc) runs its
        // precedence chain. `None` on the overwhelmingly common path
        // where no plugin has registered anything at all, without
        // even taking the guard.
        let plugin_key = code.map(|c| lodestone_ecs::PhysicalKey::named(format!("{c:?}")));
        let plugin_mode = plugin_key
            .as_ref()
            .and_then(|key| self.sim.plugin_key_intercept_mode(key));
        // Read from `MenuNav`'s live `Options` on every event, **not** from a
        // copy taken at startup — see `WindowApp::keybinds`. Resolved into a
        // local first so the immutable borrow of `self` ends before the
        // `&mut self` calls below.
        let binds = self.keybinds();
        let outcome = resolve_key(
            &binds,
            gate,
            code,
            pressed,
            self.ctrl_held,
            plugin_mode,
        );
        // Deliver the raw transition to the plugin regardless of
        // which `KeyOutcome` this resolved to — `Observe` mode wants
        // it exactly as much as `Consume` does; only whether
        // gameplay *also* sees the key depends on the outcome above.
        if plugin_mode.is_some()
            && let Some(key) = plugin_key.clone()
        {
            self.sim.queue_plugin_key_event(key, pressed);
        }
        // One line per real key event, behind `RUST_LOG=debug_keys=debug`.
        // Deliberately **not** gated on `debug_held`, on `pressed`, or on
        // which outcome resulted, because its whole value is covering the
        // seam no test in this crate can reach: whether an event arrived at
        // all. `resolve_key`'s own tests prove which `KeyOutcome` a given
        // `code`/`gate` pair produces, and `apply_key_outcome`'s real-path
        // test proves the effect side runs given that outcome — but neither
        // can see an event that the window never received.
        //
        // That distinction is what this log was built for and it paid off
        // immediately. A report that F3+B/F3+G did nothing while F3+H worked
        // looked like a resolve failure; the log showed `Code(F3)` and
        // `Code(KeyH)` resolving normally and **no line at all** for G. An
        // absent line means the event never reached this function, so the
        // cause was below us: that keyboard needs `fn` held to produce F3,
        // and `fn`+G is not a combination it reports, so the chord never
        // left the hardware. Keep this ungated — a missing line is the
        // signal, and any condition here can only turn a real absence into
        // an ambiguous one.
        tracing::debug!(
            target: "debug_keys",
            physical_key = ?event.physical_key,
            ?code,
            pressed,
            debug_held = gate.debug_held,
            ?outcome,
            "key event resolved",
        );
        self.apply_key_outcome(outcome, pressed, code, Some(&event));
    }

    /// The `KeyOutcome` → effect half of `handle_keyboard_input`, split out
    /// so a test can drive it with a resolved `KeyOutcome` directly — the
    /// gap the resolver-only tests in `app::tests` cannot see, since those
    /// only assert which `KeyOutcome` `resolve_key` *returns*, never that its
    /// side effects (an `Arc<AtomicBool>` store, `Sim::push_local_chat`, …)
    /// actually ran. `raw` is `Some` for a real keyboard event and carries
    /// the handful of outcomes (`Menu`/`Chat`/`RecipeSearch`/`CreativeSearch`/
    /// `AnvilRename`) that need the platform `KeyEvent` for its `text`/
    /// `physical_key` — winit's `KeyEvent` has a private `platform_specific`
    /// field, so nothing outside winit can construct one, which is exactly
    /// why no test before this one reached past `resolve_key`'s pure output.
    /// `pressed`/`code` are threaded through separately rather than
    /// re-derived from `raw`, because most of this match's arms (including
    /// every debug chord) need them and `raw` is exactly the thing a test
    /// cannot supply. A test driving one of the outcomes that need no raw
    /// event at all passes `raw: None` and never touches the `expect` calls
    /// below.
    pub(super) fn apply_key_outcome(
        &mut self,
        outcome: Option<KeyOutcome>,
        pressed: bool,
        code: Option<KeyCode>,
        raw: Option<&winit::event::KeyEvent>,
    ) {
        match outcome {
            Some(KeyOutcome::Menu) => {
                let key_event = raw.expect("KeyOutcome::Menu needs the raw KeyEvent");
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
                    match capture_key_for(key_event.physical_key) {
                        Some(CaptureKey::Cancel) => {
                            self.handle_menu_key(MenuKey::Escape);
                        }
                        Some(CaptureKey::Bind(code)) => {
                            self.nav.capture_binding(Binding::Key(code.into()));
                        }
                        None => {}
                    }
                } else if pressed
                    && let Some(key) = Self::menu_key_for(
                        key_event.physical_key,
                        key_event.text.as_deref(),
                        self.modifiers,
                    )
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
                let key_event = raw.expect("KeyOutcome::Chat needs the raw KeyEvent");
                if pressed && !self.handle_chat_history_key(key_event) {
                    self.handle_chat_key(key_event);
                }
            }
            Some(KeyOutcome::Pause) => {
                // Escape on a container screen **closes the container and
                // returns to gameplay** — it does not open the pause menu.
                // That is a screen's own close handling in vanilla, and it is why this
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
            Some(KeyOutcome::DebugModifier(down)) => {
                // Issue #197. Vanilla's
                // `keyDebugModifier.setDown(!didDebugAction)`
                //: the overlay toggles
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
                let now = !was;
                self.debug_hitboxes.store(now, Ordering::Relaxed);
                // `debug.show_hitboxes.on`/`.off` — the owner's own
                // report: F3+H had this and F3+B/F3+G did not.
                self.sim.push_local_chat(debug_shown_feedback(
                    "Hitboxes",
                    now,
                ));
            }
            Some(KeyOutcome::ToggleChunkBorders) => {
                use std::sync::atomic::Ordering;
                self.debug_chord_used = true;
                let was = self.debug_chunk_borders.load(Ordering::Relaxed);
                let now = !was;
                self.debug_chunk_borders.store(now, Ordering::Relaxed);
                // `debug.chunk_boundaries.on`/`.off`.
                self.sim.push_local_chat(debug_shown_feedback(
                    "Chunk borders",
                    now,
                ));
            }
            Some(KeyOutcome::ToggleProfilerChart) => {
                self.debug_chord_used = true;
                self.show_profiler_chart = !self.show_profiler_chart;
                // Landing on the root every time the chart is shown
                // again is the honest default — a stale drill-in from
                // a previous session (or from before it was hidden)
                // is not a state the player asked to return to.
                if self.show_profiler_chart {
                    self.profiler_chart_selected = None;
                }
            }
            Some(KeyOutcome::ProfilerChartSelect(selection)) => {
                self.debug_chord_used = true;
                // Only meaningful while the chart is actually shown —
                // otherwise this chord falls through with no visible
                // effect, matching vanilla's own number-key handling
                // (its debug-overlay key handling no-ops when the
                // profiler chart is not up).
                if self.show_profiler_chart {
                    self.profiler_chart_selected = selection;
                }
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
                } else if let Some(text) = raw.expect("KeyOutcome::RecipeSearch needs the raw KeyEvent").text.as_deref() {
                    let mut typed = false;
                    for ch in text.chars().filter(|c| !c.is_control()) {
                        // `searchBox.setMaxLength(50)`
                        //.
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
                } else if let Some(text) = raw.expect("KeyOutcome::CreativeSearch needs the raw KeyEvent").text.as_deref() {
                    for ch in text.chars().filter(|c| !c.is_control()) {
                        self.edit_creative_search(CreativeSearchEdit::Char(ch));
                    }
                }
            }
            Some(KeyOutcome::AnvilRename) => {
                // Same shape as the two search boxes above, but this one
                // also has a *responder*: vanilla calls `onNameChanged`
                // after every edit (`EditBox::setResponder`), which is
                // what actually produces `ClientAction::RenameItem` —
                // this arm is whole fix, closing the send
                // side of the island the issue names (`RenameItem` was
                // modelled, encoded and consumed server-side with zero
                // producers anywhere in `lodestone-shell`).
                let mut changed = false;
                if code == Some(KeyCode::Backspace) {
                    self.anvil_rename.backspace();
                    changed = true;
                } else if let Some(text) = raw.expect("KeyOutcome::AnvilRename needs the raw KeyEvent").text.as_deref() {
                    for ch in text.chars().filter(|c| !c.is_control()) {
                        self.anvil_rename.push_char(ch);
                        changed = true;
                    }
                }
                if changed && let Some(name) = self.anvil_rename.resolve_rename() {
                    if let Some(net) = self.sim.net() {
                        net.send_action(lodestone_model::ClientAction::RenameItem { name });
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
                // `debug.advanced_tooltips.on`/`.off` — the owner's
                // named gap: the toggle already worked, only this line
                // was missing.
                let now = self.nav.advanced_item_tooltips();
                self.sim.push_local_chat(debug_shown_feedback(
                    "Advanced tooltips",
                    now,
                ));
            }
            Some(KeyOutcome::TogglePauseOnLostFocus) => {
                // `debug.pause_focus.on`/`.off` — vanilla's
                // `keyDebugFocusPause` arm: toggle, persist, then
                // feedback, same order as `toggle_advanced_item_tooltips`.
                self.debug_chord_used = true;
                self.nav.toggle_pause_on_lost_focus();
                let now = self.nav.pause_on_lost_focus();
                self.sim.push_local_chat(debug_enabled_feedback(
                    "Pause on lost focus",
                    now,
                ));
            }
            Some(KeyOutcome::CopyLocation) => {
                // `debug.copy_location.message` — vanilla's
                // `keyDebugCopyLocation` arm builds `/execute in <dim>
                // run tp @s x y z yaw pitch` from the local player's own
                // state and writes it to the clipboard, unconditionally
                // (no on/off — a location either copies or, off a
                // pre-login `dimension`, is a no-op, matching vanilla's
                // own `this.minecraft.player != null` guard).
                self.debug_chord_used = true;
                if let Some(dimension) = self.sim.stats.dimension.clone() {
                    let command = copy_location_command(
                        &dimension,
                        self.sim.stats.position,
                        self.sim.stats.yaw,
                        self.sim.stats.pitch,
                    );
                    clipboard_seam::set(&command);
                    self.sim.push_local_chat(debug_feedback(
                        "Copied location to clipboard",
                    ));
                }
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
            // Issue #613's `TeleportToEntity` remainder — the
            // Spectator Menu (`crate::menu::spectator_menu`), same
            // release-and-open dance as `OpenContainer` above.
            Some(KeyOutcome::OpenSpectatorMenu) => {
                self.sim.input_mut(InputState::release_all);
                self.nav.open_spectator_menu(&mut self.ui);
                self.tab_held = false;
                self.set_grab(false);
            }
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
            // A plugin's `Consume` claim already got the raw event
            // above, unconditionally; there is nothing else to do —
            // that is the entire point of the outcome existing.
            Some(KeyOutcome::PluginConsumed) => {}
            // Either nothing is bound to this key, or a screen above
            // swallowed it. Both are "do nothing", deliberately.
            None => {}
        }
    }
}

/// The two chat-box keys `handle_chat_key` does not own: the history arrows, and
/// the refresh that makes Tab complete player names.
///
/// # Why this intercepts rather than living inside `handle_chat_key`
///
/// Vanilla splits the same way. Its own chat-screen key handling offers the event to
/// the command-suggestion popup **first** and only then reaches its own key-code
/// switch for the history arrows, so the arrows are a distinct layer above ordinary text entry.
/// Here that layer is the routing site: `handle_chat_key` is the text-entry and
/// submit path, and these keys are handled before it sees them.
impl WindowApp {
    /// Whether the pointer is **actually** captured, as opposed to `self.grabbed`,
    /// which only tracks whether we *asked* — see `browser_pointer_locked`'s doc for
    /// why the two can disagree on `wasm32`. Native's `CursorGrabMode::Locked`
    /// request is synchronous and the platform has no user-gesture requirement, so
    /// `self.grabbed` is already ground truth there and this is a plain passthrough
    /// — every native call site that gated on `self.grabbed` before this method
    /// existed sees byte-identical behaviour through it.
    pub(super) fn pointer_really_locked(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            self.grabbed && browser_pointer_locked()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.grabbed
        }
    }

    /// Folds a **browser-initiated** Pointer Lock release into the same pause
    /// action a keyboard Escape triggers — called from `about_to_wait` once the
    /// `pointerlockchange` listener (see [`ensure_pointer_lock_change_listener`])
    /// reports that something changed.
    ///
    /// # The bug this closes
    ///
    /// Escape is the browser's own gesture for exiting Pointer Lock (the Pointer
    /// Lock spec reserves it), and it consumes the keypress: winit's web backend
    /// delivers no `WindowEvent::KeyboardInput` for it while locked (see
    /// `browser_pointer_locked`'s doc for the same "winit registers no
    /// `pointerlockchange`/`pointerlockerror` listener" gap). So the first Escape
    /// silently released the cursor and the `KeyOutcome::Pause` arm in
    /// `window_event` never ran — only a second, now-unlocked Escape reached it.
    /// This reacts to the release itself instead of waiting for a keypress that
    /// never arrives.
    ///
    /// # Distinguishing "we asked for this" from "the browser did this on its own"
    ///
    /// No new flag: `set_grab(false)` already sets `self.grabbed = false`
    /// **synchronously**, before the browser's own asynchronous unlock (or a
    /// `pointerlockchange` event) can possibly be observed here. So by the time
    /// this runs, `self.grabbed` already reads `false` for every release *we*
    /// initiated (closing a menu, losing focus, …) and `true` only when we did
    /// not — meaning the browser ended the lock on its own, which on the
    /// gameplay screen only ever happens via this one gesture. If a keydown
    /// *were* also delivered for the same Escape (contradicting the report but
    /// not ruled out on every browser), whichever handler runs first already
    /// flips `self.grabbed` to `false`, so the second is a no-op rather than a
    /// double pause/unpause — this needs no ordering guarantee between the two.
    #[cfg(target_arch = "wasm32")]
    fn reconcile_browser_pointer_lock_change(&mut self) {
        if !self.grabbed || browser_pointer_locked() {
            return;
        }
        // Mirrors the `KeyOutcome::Pause` arm in `window_event` exactly: a
        // container screen closes rather than opening the pause menu over it.
        if self.active_container_menu().is_some() {
            self.sim.close_open_menu();
            self.ui.close_container();
        } else {
            self.ui.on_escape();
        }
        self.set_grab(self.ui.wants_cursor_grab());
    }

    /// The synchronous half of window bring-up: everything after the GPU exists.
    ///
    /// Extracted from `resumed` when the browser arm landed. **One body, both
    /// targets** — the native path calls it inline, the browser path calls it from
    /// `about_to_wait` once the deferred attach lands. A forked copy would be ~270
    /// lines of render-state construction drifting silently, and the symptom would be a
    /// browser missing one atlas or one uniform rather than a build failure.
    // `pub(super)`, not private: `WindowApp::create_and_attach_window`
    // (`app::session`) — the bring-up shared by ordinary startup and a
    // runtime attach — calls this too, not only `resumed`/`about_to_wait` in
    // this file.
    pub(super) fn finish_bring_up(
        &mut self,
        window: Arc<Window>,
        gpu: GpuContext,
        #[cfg_attr(not(target_arch = "wasm32"), allow(unused_mut))] mut target: lodestone_render::SurfaceTarget<'static>,
    ) {
        // Browser only: `attach_window_async` sized `target` from `window.inner_size()`,
        // read from inside an async task racing winit's own canvas `ResizeObserver` — see
        // `window_attributes`'s doc on why nothing seeds that observer's tracked size
        // synchronously. When the observer has not delivered yet, `window.inner_size()`
        // reads `(0, 0)` and `SurfaceTarget::new` clamps that to a 1x1 surface rather than
        // failing, so `target` can be sized wrong the instant it exists. The canvas's CSS
        // box itself needs no observer to be correct, though: bring-up only starts after
        // `boot()`'s multi-second asset fetch, so the page has been laid out for a long
        // time by the time this runs, and `clientWidth`/`clientHeight` are authoritative
        // the instant they are read. Measuring directly here — before the depth buffer and
        // first frame are ever sized from `target` — fixes the initial frame outright
        // instead of relying on a `Resized` event to correct it a moment later.
        #[cfg(target_arch = "wasm32")]
        if let Some((mw, mh)) = measured_canvas_physical_size()
            && (mw, mh) != target.size()
        {
            target.resize(gpu.device(), mw, mh);
        }

        let (w, h) = target.size();
        if self.config.benchmark.is_some() {
            tracing::info!(
                target: "frame_benchmark",
                framebuffer_width = w,
                framebuffer_height = h,
                fullscreen = window.fullscreen().is_some(),
                monitor = ?window.current_monitor().and_then(|monitor| monitor.name()),
                outer_position = ?window.outer_position().ok(),
                render_distance = self.config.render_distance,
                present_mode = ?wgpu::PresentMode::AutoNoVsync,
                "benchmark window ready"
            );
        }
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
        // flame/smoke/crit UVs against. `load_particle_atlas` is
        // memoised, so this is the **same** `ParticleAtlas` object `Sim` built
        // its `(Sheet, frame) -> UV` table from — not a second stitch that
        // happens to pack the same way. The bug being closed here is a UV table
        // addressing a different image than the one bound, and every counter
        // reads perfectly healthy while it is happening, so the identity is made
        // structural rather than assumed.
        if let Some(sheet) = crate::resources::load_particle_atlas() {
            render.install_particle_sheet_atlas(gpu.device(), gpu.queue(), sheet.atlas());
        }
        // The **raw** (non-sRGB) sibling of `format`, and only for this one
        // pipeline: `HudRenderer::new` builds nothing but the flat-colour
        // stream (`hud.wgsl` — text, stack counts, durability bars, the
        // chat/tab-list/scoreboard plates), which draws into its own pass on a
        // raw view of the same swapchain texture. Vanilla's 2-D GUI blending is
        // not colour-managed — it composites straight on gamma bytes — so a
        // low-alpha white plate blended in linear light comes out far too
        // light. See `docs/tab-list.md` for the measured sweep.
        //
        // The `attach_*` calls below deliberately keep `format`: their
        // pipelines draw into `view`, not the raw view. An earlier attempt at
        // this changed `new` while those passes were still *shared*, so the
        // item pipelines could not draw into the pass at all and inventory
        // icons and air bubbles vanished. `HudRenderer` now runs the
        // flat-colour draws as their own passes, which is what makes the two
        // formats able to coexist — see `render_with_item_models`' pass list.
        let mut hud = HudRenderer::new(gpu.device(), target.raw_view_format());
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
        // Vanilla's real `container/*.png` panel art. A jar-less
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
            // Same cell as `install_shadow_ground_source`, installed on this
            // path too for `set_entity_light_source`'s own reason just above:
            // a `--connect` launch that skipped it would show no entity
            // shadows at all while a menu-launched session did.
            let shadow_ground_handle = net.shared_handle();
            render.set_shadow_ground_source(move |[x, y, z]| {
                shadow_ground_handle
                    .get()?
                    .block_at(lodestone_client::BlockPos::new(x, y, z))
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
        // Same reasoning as the debug-line install immediately above, for the
        // billboard channel.
        self.install_plugin_billboards_source();
        self.hud = Some(hud);
        self.container = Some(container);
        self.menu = Some(menu);
        // Grab only if the chosen screen wants it (menus and loading: no).
        self.set_grab(self.ui.wants_cursor_grab());
    }

    /// Returns `true` when the key was consumed, so the caller must **not** fall
    /// through to `handle_chat_key`.
    ///
    /// Three keys are touched and only one is consumed:
    ///
    /// * `ArrowUp`/`ArrowDown` — vanilla's own chat-history navigation. Consumed, and
    ///   consumed even when the line does not change: vanilla's key-handling arm
    ///   reports the key as handled regardless of what the history move decided, so an arrow
    ///   at either end of the list must not fall through and be typed.
    /// * `Tab` — **not** consumed. It only refreshes the name list the chat
    ///   input completes against, then lets the existing Tab arm run, so there
    ///   is exactly one Tab implementation rather than two that can drift. The
    ///   refresh happens per keystroke rather than at open time because a player
    ///   can join while the chat box is up, and vanilla recomputes
    ///   `getCustomTabSuggestions()` on every keystroke for the same reason.
    ///
    /// Note the Tab key reaches chat at all only because `input::resolve_key`
    /// short-circuits on `gate.chat_open` before any gameplay binding —
    /// `handle_chat_key`'s own doc records that. It is what keeps completion and
    /// the in-world player-list overlay, which share the physical key, from
    /// stealing each other: the overlay is `KeyOutcome::PlayerList`, reached only
    /// when the chat box is shut.
    pub(super) fn handle_chat_history_key(&mut self, event: &winit::event::KeyEvent) -> bool {
        let PhysicalKey::Code(code) = event.physical_key else {
            return false;
        };
        match code {
            // The suggestion popup gets first refusal on both arrows —
            // vanilla's suggestion-list key handling runs its up/down arms before
            // the chat screen's own history-arrow switch, so while the dropdown is up
            // the arrows browse it and the history is unreachable. Both return
            // `true` either way, so the key is consumed regardless of which
            // layer answered it.
            KeyCode::ArrowUp => {
                if !self.chat_input.suggestion_up() {
                    self.chat_input.history_up();
                }
                true
            }
            KeyCode::ArrowDown => {
                if !self.chat_input.suggestion_down() {
                    self.chat_input.history_down();
                }
                true
            }
            KeyCode::Enter | KeyCode::NumpadEnter => {
                // Record before falling through, because `handle_chat_key`'s
                // Enter arm *consumes* the line with `ChatInput::take`. Reading
                // it here rather than teaching `take` to record is the whole
                // point: `take` is on the **Escape** path too, and a cancelled
                // line is not part of the history — vanilla only records under
                // `handleChatInput(msg, addToRecent = true)`, which Escape never
                // reaches.
                let line = self.chat_input.as_str().to_owned();
                self.chat_input.record_sent(&line);
                false
            }
            KeyCode::Tab => {
                // `getOnlinePlayers()`, not `getListedOnlinePlayers()`: vanilla's
                // suggestion provider offers every entry, including a player the
                // server has hidden from the tab overlay.
                self.chat_input.set_online_players(
                    self.sim
                        .tab_list()
                        .iter()
                        .map(|entry| entry.profile.name.clone())
                        .collect(),
                );
                false
            }
            _ => false,
        }
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

/// The window `resumed` asks winit for.
///
/// A free function so both targets build the identical attributes and only the
/// browser-specific part is forked.
///
/// `pub(super)` rather than private:
/// `WindowApp::attach_presentation` (`app::session`) needs the identical
/// attributes for a runtime attach's window, not a second, potentially
/// drifting copy.
pub(super) fn window_attributes(config: &Config) -> winit::window::WindowAttributes {
    let attrs = Window::default_attributes().with_title("Lodestone");

    // Native only: a concrete starting size for a freestanding OS window, which has no
    // other source of truth for its initial size. **Deliberately not applied on
    // `wasm32`** — see the browser arm below for why setting this there is actively
    // wrong rather than merely redundant.
    #[cfg(not(target_arch = "wasm32"))]
    let attrs = match window_physical_size(config) {
        Some((width, height)) => {
            attrs.with_inner_size(winit::dpi::PhysicalSize::new(width, height))
        }
        None => attrs.with_inner_size(winit::dpi::LogicalSize::new(1280.0, 720.0)),
    };

    // Browser: bind winit to the page's own `<canvas id="lodestone">` instead of
    // letting it create one. Two reasons, and the second is the one that bites:
    // winit does create a canvas when given none, but it does **not** insert it into
    // the DOM, so the app runs, renders, reports success and is invisible — the island
    // failure with a GPU attached. Taking the canvas from the page also lets
    // `web/index.html` own layout and sizing, which is where they belong.
    //
    // A missing element falls through to winit's own canvas rather than panicking, so a
    // page that forgot the element gets the "renders nowhere" behaviour and a warning,
    // not a dead tab.
    //
    // **No `.with_inner_size(..)` call on this path — this is not an oversight.**
    // Winit's web backend does not treat `inner_size` as a mere initial hint: `Canvas::create`
    // spends it by writing an *inline* `style.width`/`style.height` (in px) onto the canvas
    // element (see `winit::platform_impl::web::web_sys::set_canvas_size`), and an inline style
    // outranks the stylesheet rule `#lodestone { width: 100vw; height: 100vh; }` in
    // `web/index.html`. Calling it here — even with a value that happens to match the
    // viewport at that instant — pins the canvas's CSS box at that fixed pixel size forever;
    // nothing else in winit's web backend ever rewrites that inline style. That was the whole
    // bug this comment replaces: the canvas rendered at a permanent 1280x720 and was then
    // stretched by nothing (its own inline style *was* the layout), which is
    // indistinguishable on screen from "not resizing to the viewport" because that is
    // exactly what was happening. Leaving `inner_size` unset lets the stylesheet own the box,
    // and winit's own `ResizeObserver` on the canvas element (already wired up regardless of
    // this call, in `web_sys::resize_scaling`) reports every subsequent box change — initial
    // layout included — as a `WindowEvent::Resized`, which the shared `Resized` arm below
    // already forwards to the surface reconfigure and the depth buffer resize. Sizes it
    // reports are already DPR-scaled (`devicePixelContentBoxSize` where supported), so this
    // renders at native `devicePixelRatio`, matching what desktop winit already does and
    // costing the same `dpr²` fragment multiplier a retina display always costs — e.g. 4x
    // fragments at `dpr = 2`. Downscaling to CSS pixels and letting the browser upscale
    // would trade that cost for a soft/blurry image; this build takes the sharp, expensive
    // option to match native rather than picking silently.
    #[cfg(target_arch = "wasm32")]
    {
        use winit::platform::web::WindowAttributesExtWebSys;
        match browser_canvas() {
            Some(canvas) => attrs.with_canvas(Some(canvas)),
            None => {
                tracing::warn!(
                    target: "gpu",
                    "no <canvas id=\"{CANVAS_ID}\"> in the page: winit will create its own, \
                     which is NOT inserted into the DOM, so nothing will be visible"
                );
                attrs
            }
        }
    }
    #[cfg(not(target_arch = "wasm32"))]
    attrs
}

/// The id of the canvas element `web/index.html` provides.
#[cfg(target_arch = "wasm32")]
const CANVAS_ID: &str = "lodestone";

/// The page's canvas, if it has one.
#[cfg(target_arch = "wasm32")]
fn browser_canvas() -> Option<web_sys::HtmlCanvasElement> {
    use wasm_bindgen::JsCast;
    web_sys::window()?
        .document()?
        .get_element_by_id(CANVAS_ID)?
        .dyn_into::<web_sys::HtmlCanvasElement>()
        .ok()
}

/// Whether the browser's Pointer Lock API has **genuinely** engaged on our canvas —
/// the ground truth `WindowApp::grabbed` cannot provide on its own here, and trusting
/// it anyway is the whole of the "pointer doesn't get locked" report.
///
/// `winit::window::Window::set_cursor_grab(CursorGrabMode::Locked)` on this platform
/// (`platform_impl::web::web_sys::canvas::Canvas::set_cursor_lock`) calls the
/// fire-and-forget `Element::request_pointer_lock()` and returns `Ok(())`
/// *unconditionally* — it never inspects a result, and winit 0.30 registers no
/// `pointerlockchange`/`pointerlockerror` listener anywhere in its web backend, so a
/// rejected request is invisible to it. The browser rejects any such request that is
/// not the direct result of a user gesture (a click/keydown handler), and
/// `app::session::drive_ui_from_session` — the call site that actually flips grab the
/// instant `SessionPhase` becomes `Connected` — runs from the per-frame render loop,
/// not from one. So the very first grab of a session is silently refused: `grabbed`
/// becomes (falsely) `true`, the cursor is hidden by CSS, and `device_event` starts
/// feeding ordinary, edge-bounded `movementX`/`Y` through as if it were an unbounded
/// locked delta — exactly the "cursor doesn't get locked / goes off the page and
/// stops registering" report, because the OS cursor was never actually captured.
///
/// Reads `Document::pointer_lock_element()` and compares it against our own canvas by
/// identity (`===` via `JsValue` equality) rather than merely checking `is_some()`, so
/// a lock briefly held by some other element (there is only ever one canvas here, but
/// nothing enforces that structurally) cannot read as ours.
#[cfg(target_arch = "wasm32")]
fn browser_pointer_locked() -> bool {
    use wasm_bindgen::JsValue;
    let Some(canvas) = browser_canvas() else {
        return false;
    };
    web_sys::window()
        .and_then(|w| w.document())
        .and_then(|d| d.pointer_lock_element())
        .is_some_and(|el| JsValue::from(el) == JsValue::from(canvas))
}

thread_local! {
    /// Set by the `pointerlockchange` listener [`ensure_pointer_lock_change_listener`]
    /// registers, polled and cleared once per turn in `about_to_wait`.
    ///
    /// A bare `Cell<bool>` rather than anything richer: the listener fires on
    /// *every* lock change (ours and the browser's own, acquired and released
    /// alike), and the poll side always re-derives the ground truth from
    /// [`browser_pointer_locked`] and `WindowApp::grabbed` rather than trusting a
    /// value smuggled out of the DOM callback — "something changed, go look" is
    /// all this needs to carry. Same shape as `PENDING_GPU` above it: a JS
    /// callback has no way back into the live `WindowApp` winit's wasm
    /// `spawn_app` owns, so the callback can only leave a note for the next
    /// `about_to_wait` to read.
    #[cfg(target_arch = "wasm32")]
    static POINTER_LOCK_CHANGED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
}

/// Registers the page's one `pointerlockchange` listener, the first time this is
/// called; every later call is a no-op.
///
/// Exists because nothing else registers one: `browser_pointer_locked`'s doc
/// already notes winit 0.30's web backend adds no
/// `pointerlockchange`/`pointerlockerror` listener of its own, and that gap is
/// exactly what stops this app from ever learning that Escape released the
/// pointer — the DOM event is the only signal there is.
///
/// The closure is intentionally leaked (`Closure::forget`): it has to outlive
/// every possible lock change for the rest of the page's life, which is the
/// app's entire lifetime, so there is no earlier point at which dropping it
/// would be correct. Matches winit's own web backend, which leaks its DOM
/// closures for the same reason.
#[cfg(target_arch = "wasm32")]
fn ensure_pointer_lock_change_listener() {
    thread_local! {
        static REGISTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) };
    }
    REGISTERED.with(|registered| {
        if registered.get() {
            return;
        }
        let Some(document) = web_sys::window().and_then(|w| w.document()) else {
            return;
        };
        use wasm_bindgen::JsCast;
        let closure = wasm_bindgen::closure::Closure::<dyn FnMut()>::new(|| {
            POINTER_LOCK_CHANGED.with(|flag| flag.set(true));
        });
        if document
            .add_event_listener_with_callback("pointerlockchange", closure.as_ref().unchecked_ref())
            .is_ok()
        {
            registered.set(true);
        }
        closure.forget();
    });
}

/// The canvas's real backing-store size in physical pixels, read directly from its live CSS
/// box rather than from winit's own tracked `Window::inner_size` — see `finish_bring_up`'s
/// call site for why the two can disagree at browser bring-up. `clientWidth`/`clientHeight`
/// need no `ResizeObserver` delivery to be correct; they reflect the box the browser has
/// already laid out. Scaled by `devicePixelRatio` to match what `SurfaceTarget`/`RenderState`
/// expect everywhere else — see `window_attributes`'s doc on rendering at native DPR.
///
/// `None` if there is no canvas (mirrors `browser_canvas`) or it currently measures to zero
/// (e.g. `display: none`), in which case the caller keeps whatever `target` already has
/// rather than resizing to a degenerate surface.
#[cfg(target_arch = "wasm32")]
fn measured_canvas_physical_size() -> Option<(u32, u32)> {
    let canvas = browser_canvas()?;
    let dpr = web_sys::window()?.device_pixel_ratio();
    dpr_scaled_size(canvas.client_width(), canvas.client_height(), dpr)
}

/// Scales a CSS-pixel box by `dpr` into a rounded physical-pixel size, or `None` if either
/// scaled dimension rounds to less than one physical pixel.
///
/// Pulled out of [`measured_canvas_physical_size`] as a plain function with no `web_sys`/DOM
/// dependency and **no `wasm32` gate**, specifically so it is exercised by the workspace's
/// ordinary native `cargo test` run (see `app::tests`) rather than only by a `wasm32` target
/// nothing in `just health` builds for — a `#[cfg(test)]` block inside the `wasm32`-gated
/// function above would never run under any check this repo actually runs.
pub(crate) fn dpr_scaled_size(client_width: i32, client_height: i32, dpr: f64) -> Option<(u32, u32)> {
    let w = (f64::from(client_width) * dpr).round();
    let h = (f64::from(client_height) * dpr).round();
    (w >= 1.0 && h >= 1.0).then_some((w as u32, h as u32))
}

thread_local! {
    /// Where the deferred browser GPU attach parks its result for `about_to_wait` to
    /// collect.
    ///
    /// A `thread_local` because there is no way to reach back into the `WindowApp`:
    /// winit's wasm `spawn_app` takes ownership of it, and the `async` block cannot
    /// hold `&mut self` across an `await` regardless. Single-threaded by construction,
    /// so there is no race to lose — the browser event loop is one thread, which is the
    /// same fact that made the mesher's pool removable.
    #[cfg(target_arch = "wasm32")]
    static PENDING_GPU: std::cell::RefCell<
        Option<(GpuContext, lodestone_render::SurfaceTarget<'static>)>,
    > = const { std::cell::RefCell::new(None) };
}
