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
            // This makes the screen *submit*; [`WindowApp::try_use`] is what
            // makes it reachable. The two landed separately, and this comment
            // used to end "Nothing opens `Screen::CommandBlock` from a real
            // interaction yet — no command-block block-entity NBT decode, no
            // `interact.rs` trigger. That is issue #442." — true when written,
            // stale now: `crate::command_block_source` reads the payload the
            // chunk already carries, and the trigger is in `try_use` rather
            // than `interact.rs` for the reason that method's doc gives.
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
        let (fb_w, fb_h) = self.target.as_ref().map(RenderTarget::size)?;
        self.menu_row_at_in(x, y, fb_w, fb_h)
    }

    /// [`Self::menu_row_at`]'s whole body, with the framebuffer size passed in
    /// rather than read off the swapchain.
    ///
    /// The split exists so a gate can drive **this exact code** — the same
    /// `on_screen_frame` call, the same scale conversion, the same `row_rect`
    /// loop — without a GPU. `self.target` is `None` in any test that has not
    /// brought up a real window, so `menu_row_at` returns `None` at its first
    /// line there and a test calling it measures nothing at all: it would pass
    /// identically against a screen with no frame, which is the vacuous-
    /// *precondition* species. Everything the hit-test can get wrong lives
    /// below this line, so the seam costs no coverage.
    ///
    /// Not `#[cfg(test)]`: `menu_row_at` is its only production caller and the
    /// two must not be allowed to become different code, which a test-only
    /// duplicate would invite.
    pub(super) fn menu_row_at_in(
        &mut self,
        x: f32,
        y: f32,
        fb_w: u32,
        fb_h: u32,
    ) -> Option<usize> {
        let frame = crate::menu::nav::on_screen_frame(
            &self.ui,
            &self.nav,
            self.sim.death_message(),
            &self.statuses,
            &mut self.favicons,
        )?;
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
        // Same shape, for the same reason, and this is the *frame* half of the
        // suggestion poll: this method is called once per rendered frame from
        // `redraw`, before the early return for a screen it does not own, so a
        // `command_suggestion` reply lands on the next frame rather than
        // waiting for the player to press another key. `handle_chat_key` pumps
        // too, which is what covers a driver that never renders.
        self.pump_command_suggestions();
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
        // Menu music. `frame_for` returning `Some` *is* the "a menu screen is up"
        // predicate — the same expression that decides this function draws at all —
        // so the music cannot drift out of step with the screen, which asking a
        // second source would allow. Placed before the GPU guard below on purpose:
        // vanilla's title-screen music plays while the window is still coming up,
        // and gating it on a live swapchain would make music depend on rendering.
        //
        // `in_world: false` is what selects `musics::MENU` (20/600 ticks,
        // `replaceCurrentMusic` set, so it interrupts rather than queues) — see
        // `MusicSituation::situational_music`.
        self.sim.tick_music(
            std::time::Instant::now(),
            &crate::audio::music::menu_situation(),
        );
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
    /// the client's chat/command seam, Escape cancels, Backspace edits, Tab
    /// completes against the server's own command tree, and any printable text
    /// is appended (control chars and `§` are filtered by [`ChatInput`]). Both
    /// Enter and Escape close the prompt and re-grab.
    ///
    /// Tab reaches here at all because `input::resolve_key` short-circuits on
    /// `gate.chat_open` before any gameplay binding — the player-list binding is
    /// on the same physical key and would otherwise eat it.
    pub(super) fn handle_chat_key(&mut self, event: &winit::event::KeyEvent) {
        // Land any `command_suggestion` reply that arrived since the last key.
        // See `pump_command_suggestions` for why this is here rather than only
        // in the frame loop.
        self.pump_command_suggestions();
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
                // Issue #471 step 3, and the point of the whole chain: the
                // completion is computed against the tree **the server sent**
                // (`net::CommandTreeCell`), not against anything local. With no
                // tree yet — a server that has sent no `minecraft:commands`, or
                // any point before login completes — `ChatInput::tab` offers
                // nothing rather than an empty list.
                KeyCode::Tab => {
                    let tree = self.command_tree();
                    if let Some(action) = self.chat_input.tab(tree.as_deref())
                        && let Some(net) = self.sim.net()
                    {
                        // A `Completion::NeedsServer` position: the answer
                        // arrives asynchronously and is applied by
                        // `pump_command_suggestions`.
                        net.send_action(action);
                    }
                    return;
                }
                _ => {}
            }
        }
        if let Some(text) = &event.text {
            self.chat_input.push_str(text.as_str());
        }
    }

    /// The command tree the connected server sent, if any.
    ///
    /// Read straight off the shared cell rather than cached: the tree is
    /// replaced whole whenever the server re-sends `minecraft:commands` (an op
    /// level change does), and an `Arc` clone is the whole cost — the tree
    /// itself is ~2,000 nodes and is never copied here.
    pub(super) fn command_tree(
        &self,
    ) -> Option<std::sync::Arc<lodestone_model::command_tree::CommandTree>> {
        self.sim.net()?.shared_command_tree().tree()
    }

    /// Apply whatever `command_suggestion` reply the net thread has folded into
    /// the shared cell.
    ///
    /// Safe to call as often as you like: [`ChatInput::apply_suggestions`]
    /// honours a reply only once — the transaction-id match consumes the
    /// pending request, so a second call with the same (still-latest) response
    /// is stale by construction and returns `false`. That is what lets this be
    /// a poll rather than a queue, matching how every other `net` cell here is
    /// read.
    pub(super) fn pump_command_suggestions(&mut self) {
        let Some(response) = self
            .sim
            .net()
            .and_then(|net| net.shared_command_tree().suggestions())
        else {
            return;
        };
        let _ = self.chat_input.apply_suggestions(&response);
    }

    /// `key.use` in the world — vanilla's `Minecraft.startUseItem`, plus the one
    /// block whose right-click is resolved **entirely client-side**.
    ///
    /// # Why the command block forks here rather than in `interact.rs`
    ///
    /// Every other right-click in this client is a packet: `drive_placement`
    /// resolves the intent, sends `UseItemOn`, and the server decides. A
    /// command block is different in vanilla too — `CommandBlock.useWithoutItem`
    /// calls `player.openCommandBlock(be)`, which is a no-op on the server and
    /// is overridden by `LocalPlayer` to open the screen locally
    /// (`CommandBlock.java`, `LocalPlayer.openCommandBlock`). The data comes
    /// from the block entity the client already has, not from a response.
    ///
    /// So this is not a shortcut around `interact.rs`; it is the client-side
    /// half vanilla itself has. It also cannot live in `drive_placement`: that
    /// system returns `PlaceRejection::NothingPlaceableHeld` before it ever
    /// looks at the clicked block, so a right-click with an empty hand — the
    /// normal way to open a command block — never reaches its body.
    ///
    /// Closes issue #47's last hop: `UiState::open_command_block` and
    /// `MenuNav::open_command_block` had **zero production callers**, so the
    /// screen, its layout and its completion were real, unit-tested and
    /// unreachable. Tracked on #436.
    pub(super) fn try_use(&mut self) {
        // Only from the world. `Screen::Playing` is `open_command_block`'s own
        // guard as well, so this is belt-and-braces rather than the only check
        // — but asking here keeps the ordinary use path from being skipped on a
        // screen where the open would be refused anyway.
        if self.ui.screen() == crate::menu::Screen::Playing
            && let Some(open) = self.sim.targeted_command_block()
        {
            // Vanilla returns `InteractionResult.SUCCESS` and sends no use
            // packet for this block, so the ordinary path is skipped, not run
            // as well: running both would place a block against the command
            // block behind the screen that just opened.
            self.sim.input_mut(InputState::release_all);
            // Issue #471 step 2: hand the screen the tree the server actually
            // sent, so its Tab key and its suggestion popup are computed from
            // real data. `MenuNav` is pure and holds no client handle, so the
            // push has to happen here, where one is in scope; doing it at open
            // time (rather than caching) means a re-sent `minecraft:commands`
            // is picked up the next time the screen opens.
            self.nav.set_command_tree(self.command_tree());
            self.nav.open_command_block(&mut self.ui, open);
            self.tab_held = false;
            self.set_grab(false);
            return;
        }
        self.sim.use_item();
    }
}
