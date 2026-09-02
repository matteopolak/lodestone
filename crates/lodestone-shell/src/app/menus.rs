//! `WindowApp`'s menu keyboard/mouse routing and the `MenuAction` match.
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

/// Whether `modifiers` holds the platform's edit-shortcut key —
/// vanilla's own platform edit-shortcut-modifier split: Cmd (`SUPER`) on
/// macOS, Ctrl everywhere else.
/// Must agree with `focus::EDIT_SHORTCUT_MODIFIER`, which is what
/// `is_select_all`/`is_copy`/`is_cut`/`is_paste` test once the `MenuKey` this
/// produces reaches `KeyEvent::from_menu_key` — the two sides of the same
/// modifier can't be allowed to drift apart.
///
/// `is_macos` is a parameter rather than a `cfg!` read inline **so the macOS
/// branch is a unit test, not a fact about whichever machine runs the
/// suite** — `cfg!(target_os = "macos")` alone cannot be exercised from CI
/// running on a different platform, and getting this exact mapping wrong is
/// invisible on Linux and Windows (Ctrl already worked there) while breaking
/// every shortcut on a Mac. [`WindowApp::menu_key_for`] and
/// [`WindowApp::handle_chat_key_parts`] both call this with the real `cfg!`
/// value; the tests at the bottom of this file call it with both.
pub(super) fn shortcut_modifier_held(modifiers: ModifiersState, is_macos: bool) -> bool {
    if is_macos {
        modifiers.super_key()
    } else {
        modifiers.control_key()
    }
}

impl WindowApp {
    /// Translate one winit key event into a [`MenuKey`], or `None` if the menu
    /// has no use for it.
    ///
    /// Takes `physical_key`/`text` rather than a whole `&winit::event::KeyEvent`
    /// **on purpose**: that type's only public constructor is winit's own event
    /// pump (its `platform_specific` field is `pub(crate)` to winit), so a
    /// version taking the struct could never be driven from a unit test —
    /// which is exactly how this conversion went untested long enough to ship
    /// a menu that never received keyboard modifiers in the first place;
    /// `nav.rs`'s own module doc used to note "the winit mapping ... is the
    /// only untested part". Destructuring at the boundary is what makes the
    /// tests at the bottom of this file possible at all.
    ///
    /// `modifiers` is `self.modifiers` at the moment of the press — tracked
    /// from `WindowEvent::ModifiersChanged` in `app::lifecycle::window_event`.
    ///
    /// # Menu inputs never received keyboard modifiers
    ///
    /// Before `modifiers` existed here, every real `KeyEvent` reaching this
    /// function looked unmodified — winit reports modifier state as its own
    /// event, separate from the key press — so Cmd+A was indistinguishable
    /// from `a` and fell straight into the text arm below. This function is
    /// now the one place that tells the two apart.
    pub(super) fn menu_key_for(
        physical_key: PhysicalKey,
        text: Option<&str>,
        modifiers: ModifiersState,
    ) -> Option<MenuKey> {
        Self::menu_key_for_platform(
            physical_key,
            text,
            modifiers,
            cfg!(target_os = "macos"),
        )
    }

    /// [`Self::menu_key_for`] with the platform split supplied rather than
    /// read from `cfg!`.
    ///
    /// Same reasoning as [`shortcut_modifier_held`]'s own `is_macos`
    /// parameter, one layer up, and it is not theoretical: the shortcut tests
    /// at the bottom of `app::tests` drive `ModifiersState::SUPER` and expect
    /// `MenuKey::SelectAll`, which is true on macOS and false everywhere
    /// else. Reading `cfg!` inside made those five gates assertions about the
    /// machine rather than about the mapping — green on the dev Macs, red on
    /// every Linux CI runner, with nothing in the source to suggest it. With
    /// the flag passed in, each one asserts **both** platforms, which is
    /// strictly more than either arm could say before.
    pub(super) fn menu_key_for_platform(
        physical_key: PhysicalKey,
        text: Option<&str>,
        modifiers: ModifiersState,
        is_macos: bool,
    ) -> Option<MenuKey> {
        let shortcut_held = shortcut_modifier_held(modifiers, is_macos);
        if let PhysicalKey::Code(code) = physical_key {
            match code {
                KeyCode::ArrowUp => return Some(MenuKey::Up),
                KeyCode::ArrowDown => return Some(MenuKey::Down),
                KeyCode::Enter | KeyCode::NumpadEnter => return Some(MenuKey::Enter),
                KeyCode::Escape => return Some(MenuKey::Escape),
                KeyCode::Tab => return Some(MenuKey::Tab),
                KeyCode::Backspace => return Some(MenuKey::Backspace),
                KeyCode::Delete => return Some(MenuKey::Delete),
                // F5 refreshes the multiplayer list, which is
                // vanilla's own multiplayer-screen key-press handler's only
                // key. It has to be here
                // rather than falling through to the text path below: a function
                // key has no `text`, so without this it would reach nothing.
                KeyCode::F5 => return Some(MenuKey::Refresh),
                // Select-all/copy/cut/paste — gated on *exactly* the shortcut
                // modifier, matching `focus::KeyEvent::is_edit_shortcut`'s own
                // `!has_shift_down() && !has_alt_down()` guard, so Cmd+Shift+A
                // is not consumed here either (it falls through to nothing,
                // the same as it does in vanilla).
                KeyCode::KeyA if shortcut_held && !modifiers.shift_key() && !modifiers.alt_key() => {
                    return Some(MenuKey::SelectAll);
                }
                KeyCode::KeyC if shortcut_held && !modifiers.shift_key() && !modifiers.alt_key() => {
                    return Some(MenuKey::Copy);
                }
                KeyCode::KeyX if shortcut_held && !modifiers.shift_key() && !modifiers.alt_key() => {
                    return Some(MenuKey::Cut);
                }
                KeyCode::KeyV if shortcut_held && !modifiers.shift_key() && !modifiers.alt_key() => {
                    return Some(MenuKey::Paste);
                }
                // Caret motion, carried whole rather than abstracted, because
                // for these four the modifiers *are* the meaning — Left,
                // Shift+Left, Cmd/Ctrl+Left and both together are four
                // different edits. `super::input::text_key_event` is the same
                // translator the chat box uses, so the menu fields and the
                // chat line cannot drift apart in what a chord means.
                KeyCode::ArrowLeft | KeyCode::ArrowRight | KeyCode::Home | KeyCode::End => {
                    return super::input::text_key_event(physical_key, modifiers).map(MenuKey::Edit);
                }
                _ => {}
            }
        }
        // The other half of the fix: suppress printable insertion whenever the
        // shortcut modifier is held, independent of Shift/Alt and of whether
        // the letter above matched a known shortcut. Without this, a chord
        // this function does not recognise (or one held with an extra
        // modifier) would still fall into the text arm below and type —
        // which is the literal reported symptom, "it inserts a v"/"inserts an
        // a" — and a *recognised* shortcut would type its letter **alongside**
        // acting, since `event.text` is set on the same `KeyEvent` as
        // `physical_key`.
        if shortcut_held {
            return None;
        }
        // Anything else is text. `KeyEvent::text` is already the composed
        // character, so this is the path that makes non-US layouts type
        // correctly into the address field.
        text.and_then(|t| t.chars().next())
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
            // The leading `Entitlement` is the ownership gate's proof, and it is
            // consumed here rather than read: reaching this arm at all *is* the
            // evidence, because `MenuNav` cannot have built the variant without
            // a stored account that owns the game. Nothing downstream needs the
            // value, so it is bound and dropped rather than threaded on.
            MenuAction::Singleplayer(_owned, config) => {
                // A real integrated server, not the old offline demo
                // world. `Sim::new` no longer builds one (see its docs): a client
                // holds the server's world or none at all, and a demo world left
                // resident under a later multiplayer join is the two-worlds defect
                // this button used to be the entry point for. Singleplayer now
                // takes the *same* path a join does, so there is only ever one
                // world and it always came off the wire.
                //
                // The payload is a
                // `menu::nav::SingleplayerLaunch`, naming a **world directory**
                // that already exists. `Open` is `Screen::WorldSelect`'s Play
                // Selected World (the world's stored seed wins, so none travels);
                // `Created` is `Screen::CreateWorld`'s Create button, whose
                // directory the menu made and whose typed seed
                // `resolve_launch_seed` resolves. `begin_singleplayer` handles both
                // uniformly (see this file's `resolved_seeds_from_different_world_
                // creation_configs_generate_different_terrain`).
                //
                // This used to be an `Option<WorldCreationConfig>` where `None`
                // meant "the one implicit world" — which is why Create New World
                // could not create a second one. See `crate::saves`' module doc.
                self.begin_singleplayer(config);
            }
            // See the `Singleplayer` arm above for what the first field is.
            MenuAction::Connect(_owned, entry) => {
                // Set before dialing: `net::set_pending_server_pack_policy`'s
                // own doc explains why this is a one-shot global read at
                // `NetClient::connect_impl` rather than a `Sim::connect`
                // parameter — that fixed signature has no policy to carry
                // for a direct/quick connect or a singleplayer/LAN session,
                // only for a saved `ServerEntry`, which is exactly what this
                // arm (and only this arm) holds.
                crate::net::set_pending_server_pack_policy(entry.pack_status);
                self.connect_to(entry.host.clone(), entry.effective_port());
            }
            MenuAction::Quit => {}
            MenuAction::Reprobe(Some(entry)) => self.statuses.refresh_one(&entry),
            MenuAction::Reprobe(None) => {
                self.statuses.refresh(self.nav.list().entries());
            }
            // F5 or the Refresh button. `refresh_all`, not `refresh`:
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
            // Escape on the loading screen. Same teardown as
            // `MenuAction::QuitToTitle` below and deliberately so — a session
            // that is still dialling is exactly as much a live session as one
            // that finished, and `Sim::end_session` is what drops `NetClient`,
            // whose `Drop` sets the stop flag `net.rs` races the dial against.
            // Without this the screen would return to the server list while the
            // net thread carried on connecting behind it and then attached to a
            // `Sim` nobody was showing.
            //
            // `UiState::cancel_connect` has already moved the screen; unlike
            // `QuitToTitle` there is no pointer to release, because the loading
            // screen never grabbed it — `set_grab(false)` is kept anyway for the
            // same belt-and-braces reason that arm gives.
            MenuAction::CancelConnect => {
                self.sim.end_session();
                self.hosted_world = None;
                self.set_grab(false);
            }
            MenuAction::QuitToTitle => {
                // `UiState` has already moved to `MainMenu` — `nav.rs`'s
                // `key_paused` (and `key_death`) calls
                // `ui.quit_to_title()` before returning this action. What is
                // left is tearing down whatever live session is attached to
                // `Sim` so a fresh connect afterward starts clean; see
                // `Sim::end_session` for exactly what resets vs. persists.
                self.sim.end_session();
                // There is no hosted world any more, so Open to LAN
                // must stop claiming there is one. Cleared here rather than in
                // `end_session` because `Sim` does not know how the session was
                // obtained — that is exactly what this field records.
                self.hosted_world = None;
                // The pause/death screen already released the pointer on
                // entry, so this is normally a no-op; cheap insurance against
                // a future caller reaching `QuitToTitle` some other way.
                self.set_grab(false);
            }
            // The death screen's Respawn button: submit the
            // manual `ClientAction::Respawn` — `Sim::respawn` is a no-op
            // unless `Sim::is_dead` is still true, so a stray/duplicate call
            // (e.g. a double-click before the server's confirmation lands)
            // costs nothing. `UiState` stays on `Screen::Death` until
            // `net::NetUpdate::Respawned` arrives; see `drive_ui_from_session`.
            MenuAction::Respawn => self.sim.respawn(),
            // The command-block screen's Done button: vanilla's own
            // populate-and-send-packet routine.
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
            // `interact.rs` trigger." — true when written,
            // stale now: `crate::command_block_source` reads the payload the
            // chunk already carries, and the trigger is in `try_use` rather
            // than `interact.rs` for the reason that method's doc gives.
            MenuAction::SetCommandBlock(submit) => {
                if let Some(net) = self.sim.net() {
                    net.send_action(submit.into_action());
                }
            }
            // The sign-editing screen's Done row **or** Escape — both send,
            // see `Screen::SignEdit`'s own doc. `MenuNav::key_sign_edit`/
            // `activate_sign_edit_row` both take `SignEditState::to_submit`
            // before closing the screen, so by the time this arm runs the
            // widget state is already gone and this payload is all that is
            // left of it — the same shape as `MenuAction::SetCommandBlock`
            // immediately above.
            MenuAction::SignUpdate(submit) => {
                if let Some(net) = self.sim.net() {
                    net.send_action(submit.into_action());
                }
            }
            // The book-editing screen's Done or Finalize row — the same
            // shape as `MenuAction::SignUpdate` immediately above:
            // `MenuNav::activate_book_edit_row` takes `BookEditState::
            // to_save_action`/`to_sign_action` before closing the screen, so
            // this payload is all that is left of the widget state by the
            // time this arm runs. Sends [`ClientAction::EditBook`] —
            // producer for a packet that previously had none.
            MenuAction::EditBook(submit) => {
                if let Some(net) = self.sim.net() {
                    net.send_action(submit.into_action());
                }
            }
            MenuAction::ContainerButtonClick { window_id, button_id } => {
                // The reader already advanced optimistically. The lectern
                // menu validates the requested page and corrects us through
                // its next container-data update if the book changed.
                self.sim.send_container_button_click(window_id, button_id);
            }
            MenuAction::CloseContainer { window_id } => {
                // Do not close a replacement menu if the server swapped the
                // lectern out between the click and this app-side action.
                if self.sim.open_menu().is_some_and(|open| open.window_id == window_id) {
                    self.sim.close_open_menu();
                }
            }
            // The pause menu's Open to LAN. Native only: there is no
            // TCP listener to bind in a browser, which is the same reason
            // `Origin::Integrated`'s `lan_port` is `cfg`'d out there.
            #[cfg(not(target_arch = "wasm32"))]
            MenuAction::OpenToLan => self.open_current_world_to_lan(),
            #[cfg(target_arch = "wasm32")]
            MenuAction::OpenToLan => {}
            // The resource-pack prompt's Accept/Decline (`nav.rs`'s
            // `apply_resource_pack_prompt`). Goes out through `Sim`'s own
            // `NetClient`, not a `Sim` method — the same shape
            // `MenuAction::SetCommandBlock` uses and for the identical
            // reason: there is no state-dependent guard to enforce here
            // (the net thread's own `apply_pack_response` already handles a
            // stale/superseded id), only a value to hand across the
            // `Sim`/`NetClient` boundary this layer owns. `Sim::net()` is
            // `None` off a live session, so this is a no-op if the session
            // has already ended — which can only mean the prompt's own
            // answer no longer has anywhere to go, not a dropped answer for
            // a session that is still live.
            MenuAction::ResourcePackResponse { id, accept } => {
                if let Some(net) = self.sim.net() {
                    net.respond_to_resource_pack(id, accept);
                }
            }
            // The Spectator Menu's a player row was activated (`TeleportToEntity`
            // remainder — see
            // `crate::menu::spectator_menu`'s module doc): the screen has
            // already closed (`MenuNav::activate_spectator_menu_row`), and
            // this is the one send `ClientAction::SpectatorAction`'s own
            // producer (`Sim::begin_attack_live`) already proved a live
            // spectator session can reach.
            MenuAction::TeleportToEntity { target } => {
                if let Some(net) = self.sim.net() {
                    net.send_action(lodestone_model::ClientAction::TeleportToEntity { target });
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
        // Record the logical position as well as the row. The multiplayer list needs
        // the position itself — which quadrant of a row's favicon the cursor is in
        // decides whether a click joins or reorders — and this is the one place that
        // has already converted physical pixels to the canvas the draw uses, so
        // recording it here covers hover *and* click with no new plumbing at either
        // site. Recorded before the hit-test, so a cursor over the backdrop still
        // updates it.
        self.nav.set_menu_cursor(lx, ly, w, h);
        // **The hit-test itself is `render::menu_row_under`, not a copy of it here.**
        // Everything this function still owns is the physical-to-logical conversion
        // above; which row a logical point is over — including the band guard that
        // stops a scrolled-out list row stealing the footer button's clicks and hover
        // — belongs to the renderer, because `render::draw` needs the same answer to
        // decide whose tooltip to show. Two copies would have disagreed at exactly
        // that case: a tooltip on a row nothing can click.
        crate::menu::render::menu_row_under(&frame, (lx, ly), w, h)
    }

    /// The slider track fraction for `row` at physical cursor `(x, y)`.
    ///
    /// Vanilla's own slider-button "set value from mouse" routine, verbatim:
    /// the new fraction is `(cursor_x - (slider_x + 4)) / (width - 8)`.
    ///
    /// The `4` is `HANDLE_HALF_WIDTH` and the `8` is `HANDLE_WIDTH`: the handle's
    /// *centre* tracks the cursor, so the usable travel is the track minus one
    /// handle width and the origin is offset by half of it. Getting either wrong
    /// is the classic "the handle lags the cursor near the ends" slider bug.
    /// `setValue` clamps, which is why dragging past either edge pins rather than
    /// overshooting.
    ///
    /// Takes the row **index** rather than hit-testing, because a drag continues
    /// while the cursor is outside the row: once the drag has started, the row is
    /// fixed and only the x matters. That is exactly why this is a separate
    /// function from [`Self::menu_row_at`] rather than a field on its result.
    pub(super) fn menu_slider_fraction(&mut self, row: usize, x: f32, _y: f32) -> Option<f32> {
        let (fb_w, fb_h) = self.target.as_ref().map(RenderTarget::size)?;
        let frame = crate::menu::nav::on_screen_frame(
            &self.ui,
            &self.nav,
            self.sim.death_message(),
            &self.statuses,
            &mut self.favicons,
        )?;
        let (w, h) = crate::menu::render::logical_canvas(frame.gui_scale, fb_w, fb_h);
        let scale = crate::config::calculate_gui_scale(frame.gui_scale, fb_w, fb_h).max(1) as f32;
        let lx = x / scale;
        let (rx, _, rw, _) = crate::menu::render::row_rect(&frame.rows, row, w, h)?;
        let travel = rw - crate::menu::render::SLIDER_HANDLE_WIDTH;
        if travel <= 0.0 {
            return None;
        }
        let half = crate::menu::render::SLIDER_HANDLE_WIDTH * 0.5;
        Some(((lx - (rx + half)) / travel).clamp(0.0, 1.0))
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
            crate::platform::Instant::now(),
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
    ///
    /// # Order, which is the whole of the suggestion dropdown's key routing
    ///
    /// Vanilla's own chat-screen key-press handler gives its own command-suggestions
    /// key-press handler **first
    /// refusal** on every key, before Enter, Escape or anything else. That is
    /// what makes Escape close the popup rather than the box, and it is why the
    /// popup arm below sits above the `Escape` arm rather than beside it.
    ///
    /// The Up/Down arrows never arrive here — `handle_chat_history_key` in
    /// `lifecycle.rs` intercepts them one layer up, and it consults the popup
    /// first for the same reason.
    pub(super) fn handle_chat_key(&mut self, event: &winit::event::KeyEvent) {
        self.handle_chat_key_parts(event.physical_key, event.text.as_deref(), self.modifiers);
    }

    /// The testable half of [`Self::handle_chat_key`].
    ///
    /// Destructured at the boundary for exactly the reason
    /// [`Self::menu_key_for`] is: `winit::event::KeyEvent`'s
    /// `platform_specific` field is `pub(crate)` to winit, so nothing outside
    /// winit can construct one and a version taking the struct could only ever
    /// be driven by a real window. `apply_key_outcome`'s split gets a test as
    /// far as `KeyOutcome::Chat`; this one is what gets it the rest of the way,
    /// to the effect on the line.
    ///
    /// `modifiers` is `self.modifiers`, tracked from
    /// `WindowEvent::ModifiersChanged` — a real key event carries no modifier
    /// state of its own, which is why every chord here reads it from the
    /// window rather than from the press.
    pub(super) fn handle_chat_key_parts(
        &mut self,
        physical_key: PhysicalKey,
        text: Option<&str>,
        modifiers: ModifiersState,
    ) {
        // Land any `command_suggestion` reply that arrived since the last key.
        // See `pump_command_suggestions` for why this is here rather than only
        // in the frame loop.
        self.pump_command_suggestions();
        if let PhysicalKey::Code(code) = physical_key {
            // Vanilla's own command-suggestions key-press handler's first
            // refusal, above every arm
            // below. Only Escape has anything to consume here; Tab is handled in
            // its own arm because it also has a job with no popup up.
            if code == KeyCode::Escape && self.chat_input.suggestion_escape() {
                return;
            }
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
                // The point of the whole chain: the completion is computed
                // against the tree **the server sent** (`net::CommandTreeCell`),
                // not against anything local. With no tree yet — a server that
                // has sent no `minecraft:commands`, or any point before login
                // completes — `ChatInput::tab` offers nothing rather than an
                // empty list.
                //
                // `shift_held` is `event.hasShiftDown()`: Shift+Tab walks the
                // candidate list backwards.
                KeyCode::Tab => {
                    let tree = self.command_tree();
                    if let Some(action) = self.chat_input.tab(tree.as_deref(), self.shift_held)
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
        // Ordinary text editing — Backspace and Delete, caret motion (plain,
        // word-wise under the platform's edit modifier, and extending the
        // selection under Shift), Home/End, select-all, copy, cut and paste.
        //
        // **All of it is `EditBox::handle_key`'s existing port**, not a second
        // implementation beside it. The chat line used to be a bare `String`
        // edited only at its end, which is why none of the above worked and
        // why paste needed a bespoke arm here (deleted with this: `EditBox`
        // pastes over the selection, which is what the bespoke one could not
        // do). `ChatInput::handle_key` reports "consumed" and "edited"
        // separately — a caret that only moved must not fall through to the
        // text arm below, and must not re-ask the server for suggestions
        // either, which is `EditBox`'s own `onValueChange`-gated responder.
        if let Some(key_event) = super::input::text_key_event(physical_key, modifiers) {
            let result = self.chat_input.handle_key(key_event);
            if result.edited {
                self.refresh_command_suggestions();
            }
            if result.consumed {
                return;
            }
        }
        // Holding the shortcut modifier while typing must not insert the
        // letter, whether or not the box consumed it above: an unrecognised
        // chord (Cmd+B, say) should do nothing here rather than type `b`.
        if shortcut_modifier_held(modifiers, cfg!(target_os = "macos")) {
            return;
        }
        if let Some(text) = text {
            self.chat_input.push_str(text);
            self.refresh_command_suggestions();
        }
    }

    /// Vanilla's own chat-screen "on edited" — the `EditBox` responder, run
    /// after every edit to
    /// the line.
    ///
    /// **Every edit path must call this**, and it is the difference between a
    /// dropdown that appears while typing and one that only ever appears on Tab.
    /// The three callers are the printable-text and Backspace arms of
    /// [`Self::handle_chat_key`] and the paste path; a fourth edit site added
    /// without this call would silently go back to the old behaviour, which no
    /// `cargo check` can see.
    pub(super) fn refresh_command_suggestions(&mut self) {
        let tree = self.command_tree();
        if let Some(action) = self.chat_input.update_command_info(tree.as_deref())
            && let Some(net) = self.sim.net()
        {
            net.send_action(action);
        }
    }

    /// The suggestion row the pointer is over, or `None` when there is no popup
    /// or the pointer is outside it.
    ///
    /// The rect comes from [`crate::hud::HudRenderer::suggestion_layout`], i.e.
    /// from the same expression and the same font the draw resolves — the only
    /// way a click can be guaranteed to land on the row the player sees. Without
    /// a renderer or a render target there is no rect and therefore no hit, which
    /// is the right answer for a frame that has not been drawn.
    pub(super) fn suggestion_row_under_cursor(&self) -> Option<usize> {
        let list = self.chat_input.suggestion_list()?;
        let hud = self.hud.as_ref()?;
        let (w, h) = self.target.as_ref().map(lodestone_render::RenderTarget::size)?;
        let opts = self.nav.options();
        let popup = crate::hud::SuggestionPopup {
            line: self.chat_input.as_str(),
            start: list.start(),
            candidates: list.candidates(),
            selected: list.current(),
            offset: list.offset(),
            cursor: None,
        };
        let gui_scale = self.nav.gui_scale();
        let layout = hud.suggestion_layout(
            w,
            h,
            gui_scale,
            crate::hud::ChatDisplayOptions {
                scale: opts.chat_scale,
                width_pct: opts.chat_width,
                height_pct_unfocused: opts.chat_height_unfocused,
                height_pct_focused: opts.chat_height_focused,
                line_spacing: opts.chat_line_spacing,
                text_opacity: opts.chat_opacity,
                background_opacity: opts.chat_background_opacity,
                colors: opts.chat_colors,
            },
            &popup,
        );
        let (cx, cy) =
            crate::hud::HudRenderer::canvas_cursor(w, h, gui_scale, self.cursor);
        layout.row_at(cx, cy, list.offset(), list.candidates().len())
    }

    /// The `click_event`/`hover_event` under the pointer while chat is open,
    /// or `None` when nothing interactive is drawn there.
    ///
    /// [`crate::hud::HudRenderer::chat_interaction_at`]'s caller, following
    /// [`Self::suggestion_row_under_cursor`]'s own discipline immediately
    /// above: every input is the same renderer/target/options the chat draw
    /// itself resolves from, so a hit can never land somewhere the player
    /// does not see it.
    pub(super) fn chat_interaction(&self) -> Option<lodestone_game::text::InteractiveSpan> {
        let hud = self.hud.as_ref()?;
        let (w, h) = self.target.as_ref().map(lodestone_render::RenderTarget::size)?;
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
        // The same 100-entry cap the scroll-wheel handler already reads
        // through `Sim::recent_chat(100)` — `ChatFeed`'s own capacity, not a
        // windowed subset of it.
        let entries = self.sim.recent_chat_interactive(100);
        let hit = hud.chat_interaction_at(
            w,
            h,
            self.nav.gui_scale(),
            chat_opts,
            self.ui.is_chat_open(),
            &entries,
            self.chat_input.scroll().scrolled(),
            self.cursor,
        );
        hit.filter(|s| s.click.is_some() || s.hover.is_some())
    }

    /// Acts on a chat `click_event` under the pointer, if there is one — the
    /// dispatch half of [`Self::chat_interaction`].
    ///
    /// `run_command`/`suggest_command`/`copy_to_clipboard` act immediately,
    /// the same way vanilla's do: none has an effect outside this process a
    /// player cannot already see and act on (a bad command is not silent —
    /// it echoes the server's own "unknown command" reply; a clipboard write
    /// touches nothing but the OS clipboard). `open_url`/`open_file` are the
    /// opposite: vanilla itself gates `open_url` behind a confirmation
    /// screen (`ConfirmLinkScreen`) precisely because a chat message is
    /// server-supplied, untrusted content, so `open_url` enters the existing
    /// confirmation overlay and opens only after an explicit Yes. `open_file`
    /// remains unsupported and never receives an OS handoff.
    pub(super) fn dispatch_chat_click_under_cursor(&mut self) -> bool {
        let Some(click) = self.chat_interaction().and_then(|hit| hit.click) else {
            return false;
        };
        self.dispatch_click_action(&click);
        true
    }

    /// The pure action-dispatch half of [`Self::dispatch_chat_click_under_cursor`],
    /// split out so it is testable without a renderer or a render target —
    /// [`Self::chat_interaction`] needs both (the same requirement
    /// [`Self::suggestion_row_under_cursor`] already has), which would
    /// otherwise make this whole match a GPU-gated test just to prove the
    /// dispatch table itself is right.
    pub(super) fn dispatch_click_action(&mut self, click: &lodestone_model::text::ClickEvent) {
        use lodestone_model::text::ClickAction;
        match &click.action {
            ClickAction::RunCommand => {
                self.sim.send_chat(&click.value);
            }
            ClickAction::SuggestCommand => {
                self.chat_input.set(click.value.clone());
                self.refresh_command_suggestions();
            }
            ClickAction::CopyToClipboard => {
                #[cfg(not(target_arch = "wasm32"))]
                crate::menu::accounts::copy_to_clipboard(&click.value);
            }
            ClickAction::OpenUrl => {
                self.nav.server_links_mut().confirm_chat_url(click.value.clone());
                self.ui.open_server_links_from_chat();
            }
            ClickAction::OpenFile => {
                self.sim.push_local_chat(format!(
                    "Link received (not opened automatically): {}",
                    click.value
                ));
            }
            ClickAction::ChangePage | ClickAction::Other(_) => {}
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

    /// `key.use` in the world — vanilla's own start-use-item routine, plus the one
    /// block whose right-click is resolved **entirely client-side**.
    ///
    /// # Why the command block forks here rather than in `interact.rs`
    ///
    /// Every other right-click in this client is a packet: `drive_placement`
    /// resolves the intent, sends `UseItemOn`, and the server decides. A
    /// command block is different in vanilla too — vanilla's own
    /// use-without-item routine
    /// calls the local player's open-command-block hook, which is a no-op on
    /// the server and
    /// is overridden by the local player to open the screen locally. The data comes
    /// from the block entity the client already has, not from a response.
    ///
    /// So this is not a shortcut around `interact.rs`; it is the client-side
    /// half vanilla itself has. It also cannot live in `drive_placement`: that
    /// system returns `PlaceRejection::NothingPlaceableHeld` before it ever
    /// looks at the clicked block, so a right-click with an empty hand — the
    /// normal way to open a command block — never reaches its body.
    ///
    /// Closes last hop: `UiState::open_command_block` and
    /// `MenuNav::open_command_block` had **zero production callers**, so the
    /// screen, its layout and its completion were real, unit-tested and
    /// unreachable.
    pub(super) fn try_use(&mut self) {
        // Only from the world. `Screen::Playing` is `open_command_block`'s own
        // guard as well, so this is belt-and-braces rather than the only check
        // — but asking here keeps the ordinary use path from being skipped on a
        // screen where the open would be refused anyway.
        if self.ui.screen() == crate::menu::Screen::Playing
            && let Some(open) = self.sim.targeted_command_block()
        {
            // Vanilla returns a success result and sends no use
            // packet for this block, so the ordinary path is skipped, not run
            // as well: running both would place a block against the command
            // block behind the screen that just opened.
            self.sim.input_mut(InputState::release_all);
            // Hand the screen the tree the server actually
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
        // `EditBook` remainder: `minecraft:writable_book` opens
        // vanilla's own book-edit screen the instant it is used, entirely
        // client-side — no server round trip, the same shape the command
        // block fork immediately above has. See `crate::menu::book_edit`'s
        // module doc.
        if self.ui.screen() == crate::menu::Screen::Playing
            && let Some(open) = self.sim.writable_book_in_hand()
        {
            self.sim.input_mut(InputState::release_all);
            self.nav.open_book_edit(&mut self.ui, open);
            self.tab_held = false;
            self.set_grab(false);
            return;
        }
        // The signed book's half of the same fork: vanilla's own written-book
        // use routine
        // calls its own open-item-GUI hook and returns
        // a success result, so vanilla opens its own book-view screen
        // and never reaches the generic use either. Its
        // result is success with a client-side swing source,
        // and `generic_use_swings` lists `written_book` for exactly that
        // reason — but the return below means the swing is not reached from
        // here, matching vanilla, whose start-use-item routine for a book is
        // answered entirely by the screen opening. See
        // `crate::menu::book_view`'s module doc.
        if self.ui.screen() == crate::menu::Screen::Playing
            && let Some(open) = self.sim.written_book_in_hand()
        {
            self.sim.input_mut(InputState::release_all);
            self.nav.open_book_view(&mut self.ui, open);
            self.tab_held = false;
            self.set_grab(false);
            return;
        }
        self.sim.use_item();
    }
}
