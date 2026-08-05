//! `WindowApp`'s menu keyboard/mouse routing and the `MenuAction` match.
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

impl WindowApp {
    /// Translate one winit key event into a [`MenuKey`], or `None` if the menu
    /// has no use for it.
    pub(super) fn menu_key_for(event: &winit::event::KeyEvent) -> Option<MenuKey> {
        if let PhysicalKey::Code(code) = event.physical_key {
            match code {
                KeyCode::ArrowUp => return Some(MenuKey::Up),
                KeyCode::ArrowDown => return Some(MenuKey::Down),
                KeyCode::Enter | KeyCode::NumpadEnter => return Some(MenuKey::Enter),
                KeyCode::Escape => return Some(MenuKey::Escape),
                KeyCode::Tab => return Some(MenuKey::Tab),
                KeyCode::Backspace => return Some(MenuKey::Backspace),
                KeyCode::Delete => return Some(MenuKey::Delete),
                // F5 refreshes the multiplayer list (#396), which is
                // `JoinMultiplayerScreen.keyPressed`'s only key. It has to be here
                // rather than falling through to the text path below: a function
                // key has no `text`, so without this it would reach nothing.
                KeyCode::F5 => return Some(MenuKey::Refresh),
                _ => {}
            }
        }
        // Anything else is text. `KeyEvent::text` is already the composed
        // character, so this is the path that makes non-US layouts type
        // correctly into the address field.
        event
            .text
            .as_ref()
            .and_then(|t| t.chars().next())
            .filter(|c| !c.is_control())
            .map(MenuKey::Char)
    }

    /// Feed one menu key through the navigator and act on what it asks for.
    pub(super) fn handle_menu_key(&mut self, key: MenuKey) {
        let action = self.nav.key(&mut self.ui, key);
        self.apply_menu_action(action);
    }

    /// Perform the one side effect a [`MenuAction`] names. Exhaustive on purpose:
    /// a new variant must fail to compile here rather than silently do nothing.
    pub(super) fn apply_menu_action(&mut self, action: MenuAction) {
        match action {
            MenuAction::None => {}
            MenuAction::Singleplayer(config) => {
                // A real integrated server (#287), not the old offline demo
                // world. `Sim::new` no longer builds one (see its docs): a client
                // holds the server's world or none at all, and a demo world left
                // resident under a later multiplayer join is the two-worlds defect
                // this button used to be the entry point for. Singleplayer now
                // takes the *same* path a join does, so there is only ever one
                // world and it always came off the wire.
                //
                // `None` when `Screen::WorldSelect`'s Play Selected World produced
                // the action (no seed of its own, so this resolves to
                // `BUNDLED_WORLD.seed` via `resolve_launch_seed`); `Some(config)`
                // when `Screen::CreateWorld`'s Create button did (issue #190,
                // `menu/nav.rs`'s `apply_create_world`). `begin_singleplayer`,
                // `resolve_launch_seed` and `launch_singleplayer` handle both
                // uniformly (see this file's `resolved_seeds_from_different_world_
                // creation_configs_generate_different_terrain`).
                self.begin_singleplayer(config);
            }
            MenuAction::Connect(entry) => {
                self.connect_to(entry.host.clone(), entry.effective_port());
            }
            MenuAction::Quit => {}
            MenuAction::Reprobe(Some(entry)) => self.statuses.refresh_one(&entry),
            MenuAction::Reprobe(None) => {
                self.statuses.refresh(self.nav.list().entries());
            }
            // F5 or the Refresh button (#396). `refresh_all`, not `refresh`:
            // `refresh` skips any address it already has a result for, so it would
            // make the button do nothing at all.
            MenuAction::RefreshList => {
                let entries = self.nav.list().entries().to_vec();
                self.statuses.refresh_all(&entries);
            }
            MenuAction::Forget(entry) => {
                self.statuses.forget(&entry);
                // A delete or re-address changes the row set; probe whatever is
                // now in the list (idempotent, so this costs nothing per frame).
                self.statuses.refresh(self.nav.list().entries());
            }
            MenuAction::QuitToTitle => {
                // `UiState` has already moved to `MainMenu` — `nav.rs`'s
                // `key_paused` (and, issue #103, `key_death`) calls
                // `ui.quit_to_title()` before returning this action. What is
                // left is tearing down whatever live session is attached to
                // `Sim` so a fresh connect afterward starts clean; see
                // `Sim::end_session` for exactly what resets vs. persists.
                self.sim.end_session();
                // The pause/death screen already released the pointer on
                // entry, so this is normally a no-op; cheap insurance against
                // a future caller reaching `QuitToTitle` some other way.
                self.set_grab(false);
            }
            // The death screen's Respawn button (issue #103): submit the
            // manual `ClientAction::Respawn` — `Sim::respawn` is a no-op
            // unless `Sim::is_dead` is still true, so a stray/duplicate call
            // (e.g. a double-click before the server's confirmation lands)
            // costs nothing. `UiState` stays on `Screen::Death` until
            // `net::NetUpdate::Respawned` arrives; see `drive_ui_from_session`.
            MenuAction::Respawn => self.sim.respawn(),
            // The command-block screen's Done button (issue #47):
            // `populateAndSendPacket` (`CommandBlockEditScreen.java:96-114`).
            //
            // `into_action` is the one step `MenuAction`'s `Eq` derive cannot
            // cross — `ClientAction` holds a float in a sibling variant, so
            // `nav.rs` carries the `Eq`-able `CommandBlockSubmit` and rebuilds
            // the real action here. See `command_block::CommandBlockSubmit`.
            //
            // Goes out through `Sim`'s own `NetClient`, not a `Sim` method:
            // there is no `Sim::set_command_block` to add, and unlike
            // `Sim::respawn` there is no state-dependent guard to enforce (a
            // command block edit is unconditional — the server validates op
            // level). `Sim::net()` is `None` off a live session, so this is a
            // no-op in single-player-menu or pre-join states rather than a
            // panic.
            //
            // **This makes the screen submit; it does not make it reachable.**
            // Nothing opens `Screen::CommandBlock` from a real interaction yet —
            // no command-block block-entity NBT decode, no `interact.rs`
            // trigger. That is issue #442, deliberately not fixed here.
            MenuAction::SetCommandBlock(submit) => {
                if let Some(net) = self.sim.net() {
                    net.send_action(submit.into_action());
                }
            }
        }
    }

    /// Route a mouse position (physical pixels) to a menu row, if it is over one.
    ///
    /// The frame comes from [`crate::menu::nav::on_screen_frame`] and **not**
    /// from [`crate::menu::render::frame_for`]. The distinction is a player
    /// report (2026-08-04, "i cant click anything in the options menu"):
    /// `frame_for` is the authority on which screens the menu renderer *owns* —
    /// the ones it draws with a `Clear` pass, replacing the world — and it
    /// answers `None` for the three that draw as an **overlay** over a live
    /// world: `Screen::Paused`, `Screen::Death`, and `Screen::Settings` when
    /// [`crate::menu::UiState::settings_in_world`].
    ///
    /// This function used to inline a branch per overlay screen and end with
    /// `frame_for(…)?`. Pause and death got theirs when they became overlays;
    /// in-world Options became the third and did not, so that screen was live to
    /// the mouse ([`crate::menu::render::owns_frame`] stays `true` for it,
    /// deliberately) with **no rows to hit-test** — every click returned at the
    /// `?` before reaching one. Three `if`s in a private method here could not be
    /// enumerated from any test, which is exactly why one could go missing
    /// silently; `on_screen_frame` is that set in one place, and
    /// `nav::tests::every_mouse_routable_screen_has_a_frame_to_hit_test` asserts
    /// it covers every screen the mouse routes to.
    ///
    /// The physical framebuffer size and cursor are then converted down to the
    /// same logical canvas [`MenuRenderer::render`]/`render_overlay` actually
    /// draw into (via [`crate::menu::render::logical_canvas`]) before calling
    /// [`crate::menu::render::row_rect`] — mirroring
    /// `container::hit_test_with_scale`'s own `x / scale` pattern. Skipping
    /// this (as this function used to) is exactly the "clicks land one slot
    /// off, invisible in any screenshot" bug that module warns about: it is
    /// only invisible at `gui_scale == 1`, which is why it went unnoticed.
    pub(super) fn menu_row_at(&mut self, x: f32, y: f32) -> Option<usize> {
        let frame = crate::menu::nav::on_screen_frame(
            &self.ui,
            &self.nav,
            self.sim.death_message(),
            &self.statuses,
            &mut self.favicons,
        )?;
        let (fb_w, fb_h) = self.target.as_ref().map(RenderTarget::size)?;
        let (w, h) = crate::menu::render::logical_canvas(frame.gui_scale, fb_w, fb_h);
        let scale = crate::config::calculate_gui_scale(frame.gui_scale, fb_w, fb_h).max(1) as f32;
        let (lx, ly) = (x / scale, y / scale);
        // Record the logical position as well as the row (#396). The multiplayer
        // list needs the position itself — which quadrant of a row's favicon the
        // cursor is in decides whether a click joins or reorders — and this is the
        // one place that has already converted physical pixels to the canvas the
        // draw uses, so recording it here covers hover *and* click with no new
        // plumbing at either site. Recorded before the hit-test, so a cursor over
        // the backdrop still updates it.
        self.nav.set_menu_cursor(lx, ly, w, h);
        (0..frame.rows.len()).find(|&i| {
            crate::menu::render::row_rect(&frame.rows, i, w, h)
                .is_some_and(|(rx, ry, rw, rh)| {
                    lx >= rx && lx <= rx + rw && ly >= ry && ly <= ry + rh
                })
        })
    }

    /// Draw one menu screen. Returns `false` when the current screen is not a
    /// menu, so the caller falls through to the world path.
    pub(super) fn draw_menu(&mut self) -> bool {
        // Land any finished status pings before building the frame, or a row
        // shows "PINGING" for one frame longer than it needs to.
        self.statuses.pump();
        // `frame_for` is the authority on which screens this renderer owns — it
        // covers the three menu screens *and* the error screen. Asking it,
        // rather than re-deriving the set here, is what keeps the two from
        // drifting apart into a screen that is drawn twice or not at all.
        let Some(frame) = crate::menu::render::frame_for(
            &self.ui,
            &self.nav,
            &self.statuses,
            &mut self.favicons,
        ) else {
            return false;
        };
        let (Some(gpu), Some(target), Some(menu)) = (
            self.gpu.as_ref(),
            self.target.as_mut(),
            self.menu.as_mut(),
        ) else {
            // GPU not up yet; still report the screen as handled so the world
            // path does not run for a menu.
            return true;
        };
        let (w, h) = target.size();
        let device = gpu.device();
        let queue = gpu.queue();
        let surface = match target.acquire() {
            Ok(f) => f,
            Err(e) => {
                if e.needs_reconfigure() {
                    target.reconfigure(device);
                }
                return true;
            }
        };
        menu.render(device, queue, surface.view(), &frame, w, h);
        if let Some(window) = &self.window {
            window.pre_present_notify();
        }
        surface.present(queue);
        true
    }

    /// Route one key press to the open chat prompt. Enter sends the line through
    /// the client's chat/command seam, Escape cancels, Backspace edits, and any
    /// printable text is appended (control chars and `§` are filtered by
    /// [`ChatInput`]). Both Enter and Escape close the prompt and re-grab.
    pub(super) fn handle_chat_key(&mut self, event: &winit::event::KeyEvent) {
        if let PhysicalKey::Code(code) = event.physical_key {
            match code {
                KeyCode::Escape => {
                    let _ = self.chat_input.take();
                    self.ui.close_chat();
                    self.set_grab(self.ui.wants_cursor_grab());
                    return;
                }
                KeyCode::Enter | KeyCode::NumpadEnter => {
                    let line = self.chat_input.take();
                    self.sim.send_chat(&line);
                    self.ui.close_chat();
                    self.set_grab(self.ui.wants_cursor_grab());
                    return;
                }
                KeyCode::Backspace => {
                    self.chat_input.backspace();
                    return;
                }
                _ => {}
            }
        }
        if let Some(text) = &event.text {
            self.chat_input.push_str(text.as_str());
        }
    }
}
