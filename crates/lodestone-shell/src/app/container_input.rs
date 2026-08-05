//! `WindowApp`'s container-screen gestures: clicks, swaps, drops, pick-item.
//!
//! Split out of `app.rs`; see that module's own header for the layout.

use super::*;

impl WindowApp {
    /// The menu currently drawn as the container screen — the open non-player
    /// menu if the server has one open, else the player inventory while `E`
    /// has it up — or `None` when no container UI is showing.
    ///
    /// Mirrors the `container_menu` selection `redraw` makes for drawing, so
    /// hit-testing and drawing never disagree about which menu is on screen
    /// (see the layout module's own warning about that class of bug).
    pub(super) fn active_container_menu(&self) -> Option<Menu> {
        if let Some(open) = self.sim.open_menu() {
            Some(open.menu)
        } else if self.ui.is_container_open() {
            Some(self.sim.player_menu())
        } else {
            None
        }
    }

    /// Predicts a container click against the live client state and submits
    /// it to the server.
    ///
    /// This goes straight to [`lodestone_client::ClientHandle::menu_click`]
    /// rather than through `Sim`/`NetClient`'s `send_action` queue, and
    /// deliberately so: the prediction has to run inside the read-model the
    /// live `Menus` session lives in (see the doc comment on
    /// `ClientHandle::menu_click`), and `NetClient::send_action` only ever
    /// forwards an *already-built* [`lodestone_model::ClientAction`] — it has
    /// no menu to predict a click against. `NetClient::shared_handle()` is
    /// the existing, already-public seam onto that same live handle (used
    /// today for the sky-darken and entity-light samplers), so this needs no
    /// change to `net.rs` or `sim.rs`.
    ///
    /// Silently drops the click if there is no live connection yet (matches
    /// every other best-effort send in this app, e.g. `NetClient::send_action`
    /// itself).
    pub(super) fn send_menu_click(&mut self, click: Click) {
        // Issue #145: a plugin-opened local menu has no server container, so its
        // clicks are applied locally and **nothing is sent**. Checked before the
        // connection check on purpose — a local menu works with no connection at
        // all, so bailing on `net()` first would make plugin menus dead at the
        // title screen.
        if self.sim.click_local_menu(click) {
            return;
        }
        let Some(net) = self.sim.net() else { return };
        // Named separately from its `.get()` below rather than chained: the
        // `Arc<OnceLock<_>>` `shared_handle()` returns is an owned value, and
        // `.get()` borrows from it — keeping it in a binding of its own avoids
        // relying on let-else's temporary-scope-extension rules to keep that
        // borrow valid.
        let shared = net.shared_handle();
        let Some(handle) = shared.get() else { return };
        // `Sim` has no game-mode accessor to source a real `PlayerCtx` from
        // (see the report on this change) — hardcoded survival, matching the
        // only existing production-shaped precedent
        // (`container.rs`'s own click-driving tests use `PlayerCtx::survival()`
        // /`::creative()` explicitly rather than reading one off anything).
        let _ = handle.menu_click(click, PlayerCtx::survival());
    }

    /// Resolve a click at the current cursor against the recipe-book panel and
    /// act on it, returning whether the panel **consumed** the click.
    ///
    /// Called before the container's own `hit_test_with_scale` so the panel —
    /// which overlaps the main panel's left edge at narrow canvases, by
    /// `container.rs`'s documented design — wins over the slot underneath it.
    /// Returning `false` leaves the click to the normal slot path untouched.
    pub(super) fn handle_recipe_panel_click(&mut self, menu: &Menu, w: u32, h: u32) -> bool {
        let Some(book_type) = recipe_book_type_for(menu) else {
            return false;
        };
        let (tab_count, total_pages, page_ids) =
            recipe_panel_contents(self.recipe_book.as_ref(), &self.recipe_panel, book_type);
        let layout = recipe_panel_layout(
            &self.recipe_panel,
            menu,
            self.nav.gui_scale(),
            w,
            h,
            tab_count,
            total_pages,
        );
        let Some(hit) = crate::container::recipe_book_panel_hit_test_with_scale(
            &layout,
            self.recipe_panel.open,
            self.nav.gui_scale(),
            w,
            h,
            self.cursor.0,
            self.cursor.1,
        ) else {
            return false;
        };

        use crate::container::RecipeBookPanelHit as Hit;
        match hit {
            Hit::Toggle => {
                self.recipe_panel.open = !self.recipe_panel.open;
                self.recipe_panel.search_focused = false;
            }
            Hit::SearchBox => self.recipe_panel.search_focused = true,
            Hit::Tab(i) => {
                // Clicking the selected tab again clears the filter, so there is
                // always a way back to all categories without a dedicated
                // "all" tab (this client's tab list has none — see
                // `recipe_book_panel_contents`).
                self.recipe_panel.tab = if self.recipe_panel.tab == Some(i) {
                    None
                } else {
                    Some(i)
                };
                self.recipe_panel.page = 0;
                self.recipe_panel.search_focused = false;
            }
            Hit::PageForward => {
                if self.recipe_panel.page + 1 < total_pages {
                    self.recipe_panel.page += 1;
                }
                self.recipe_panel.search_focused = false;
            }
            Hit::PageBack => {
                self.recipe_panel.page = self.recipe_panel.page.saturating_sub(1);
                self.recipe_panel.search_focused = false;
            }
            Hit::Recipe(i) => {
                self.recipe_panel.search_focused = false;
                // A cell can be empty on a short final page — `page_ids` is the
                // authority on which of the 20 fixed cells is populated, exactly
                // as `RecipeBookPanelHit::Recipe`'s own doc requires.
                if let Some(id) = page_ids.get(i).cloned() {
                    self.auto_fill_recipe(menu, &id);
                }
            }
            // A click on the panel body or the unimplemented All/Craftable
            // filter is still *consumed*, so it does not fall through and
            // click the container slot behind the panel.
            Hit::FilterButton | Hit::Panel => self.recipe_panel.search_focused = false,
        }
        true
    }

    /// Auto-fill the crafting grid for `id` (issue #163's "click a recipe to
    /// fill the grid").
    ///
    /// Every click goes out through [`Self::send_menu_click`], i.e. the **same**
    /// per-click predict-then-send path a manual `MenuInput::press`/`release`
    /// takes. That is deliberate and load-bearing: a second dispatch path would
    /// diverge from `container.rs`'s vanilla-exact click semantics, and the
    /// prediction has to see each click in order for the next one's `ctx` to be
    /// right.
    fn auto_fill_recipe(&mut self, menu: &Menu, id: &lodestone_model::Identifier) {
        let Some(book) = self.recipe_book.as_ref() else {
            return;
        };
        let Some(recipe) = book.get(id) else { return };
        let Some(steps) = menu.plan_recipe_auto_fill(recipe, book.tags()) else {
            return;
        };
        for click in auto_fill_clicks(&steps) {
            self.send_menu_click(click);
        }
    }

    /// A number-key / off-hand-key `SWAP` against the slot under the cursor
    /// (issue #378 part 3).
    ///
    /// Vanilla's `AbstractContainerScreen.checkHotbarKeyPressed`
    /// (`AbstractContainerScreen.java:506-522`) guards on exactly two pieces of
    /// **state**: `menu.getCarried().isEmpty()` and `hoveredSlot != null`. Both
    /// are checked here rather than in `resolve_key`, which only knows about keys.
    /// Failing either does nothing — the same thing an open container did with
    /// these keys before this landed, so a miss is not a new dead end.
    ///
    /// The hover is resolved through the identical
    /// `active_container_menu` + `hit_test_with_scale` pair the mouse path uses,
    /// so the key and the mouse can never disagree about which slot is under the
    /// pointer (the layout module's own warning about that class of bug).
    pub(super) fn send_container_swap(&mut self, button: i32) {
        let (Some(menu), Some((w, h))) = (
            self.active_container_menu(),
            self.target.as_ref().map(RenderTarget::size),
        ) else {
            return;
        };
        // An occupied cursor is vanilla's first guard, and it is not arbitrary: a
        // swap with something already in hand has no defined meaning, so vanilla
        // lets the key fall through to nothing.
        if menu.carried().is_some() {
            return;
        }
        let hit = hit_test_with_scale(&menu, self.nav.gui_scale(), w, h, self.cursor.0, self.cursor.1);
        let MenuHit::Slot(index) = hit else { return };
        // Vanilla's `40` is the off-hand button and `do_swap`'s `button == 40` arm
        // handles it. Since #382 freed `F` the off-hand binding does reach here;
        // note this is the **container** route only — the no-screen route is
        // `send_offhand_swap` below, a different packet entirely (#385).
        let click = if button == OFFHAND_SWAP_BUTTON {
            Click::offhand_swap(index)
        } else if let Ok(hotbar) = u8::try_from(button) {
            Click::hotbar_swap(index, hotbar)
        } else {
            return;
        };
        self.send_menu_click(click);
    }

    /// `key.drop` pressed with a container screen open (the container half of
    /// the drop-key island pair).
    ///
    /// Goes through [`MenuInput::key_pressed`] rather than building the
    /// `Click` directly the way [`Self::send_container_swap`] does, because
    /// `key_pressed` already carries vanilla's `hoveredSlot.hasItem()` guard
    /// (`AbstractContainerScreen.java:495`) and the `PickItem`/`Drop`
    /// `else if` — duplicating either here would be a second copy that can
    /// drift from the one `container.rs` already tests. `Click::drop_one`/
    /// `drop_stack` and `do_throw` (`lodestone-game`) were built and tested
    /// under #27 with zero producers before this; this is the first caller.
    pub(super) fn send_container_drop(&mut self, ctrl: bool) {
        let (Some(menu), Some((w, h))) = (
            self.active_container_menu(),
            self.target.as_ref().map(RenderTarget::size),
        ) else {
            return;
        };
        let hit = hit_test_with_scale(&menu, self.nav.gui_scale(), w, h, self.cursor.0, self.cursor.1);
        let ctx = MenuContext {
            cursor_loaded: menu.carried().is_some(),
            // Same gap `send_container_swap`'s own click construction has: no
            // game-mode plumbing exists on `Sim` yet, and `key_pressed`'s
            // `Drop` arm does not read `ctx` regardless (see its doc comment).
            creative: false,
        };
        for click in self.menu_input.key_pressed(hit, ContainerMenuKey::Drop { ctrl }, ctx, &menu) {
            self.send_menu_click(click);
        }
    }

    /// `key.pickItem` pressed with a container screen open — `ClickType::CLONE`
    /// against the hovered slot (`AbstractContainerScreen.java:495-501`).
    ///
    /// Identical in shape to [`Self::send_container_drop`] except that there is
    /// no modifier variant to carry: vanilla's clone click has no `ctrl` form.
    /// The same `creative: false` gap applies — no game-mode plumbing exists on
    /// `Sim` yet, which matters more here than for drop, because vanilla's clone
    /// click is *creative-only*; until that lands this resolves and then produces
    /// no clicks, which is the honest degradation rather than a fabricated one.
    pub(super) fn send_container_pick_item(&mut self) {
        let (Some(menu), Some((w, h))) = (
            self.active_container_menu(),
            self.target.as_ref().map(RenderTarget::size),
        ) else {
            return;
        };
        let hit = hit_test_with_scale(&menu, self.nav.gui_scale(), w, h, self.cursor.0, self.cursor.1);
        let ctx = MenuContext {
            cursor_loaded: menu.carried().is_some(),
            creative: false,
        };
        for click in self.menu_input.key_pressed(hit, ContainerMenuKey::PickItem, ctx, &menu) {
            self.send_menu_click(click);
        }
    }

    /// `key.drop` pressed in normal gameplay (no screen open) — the gameplay
    /// half of the drop-key island pair. `ClientAction::DropSelectedItem`/
    /// `DropSelectedItemStack` were encoded and round-trip tested with zero
    /// producers anywhere in `lodestone-shell` before this; this is the first
    /// caller. Thin by design, like [`Self::send_offhand_swap`]: everything
    /// decidable is in [`drop_selected_action`], testable without a window, a
    /// GPU or a live `Sim`.
    ///
    /// # The local prediction is not optional (issue #436)
    ///
    /// This used to be `send_action` and nothing else, and an owner reported the
    /// consequence: *"throwing out items with Q doesn't update the count in my
    /// inventory or hotbar, but it does work properly otherwise."* `DROP_ITEM` /
    /// `DROP_ALL_ITEMS` are the one inventory change a vanilla server applies
    /// **silently** — `ServerGamePacketListenerImpl.java:1303-1314` calls
    /// `player.drop(…)` and returns with no slot or content packet — so an
    /// unpredicted drop leaves the count wrong *forever*, not briefly.
    ///
    /// Two things about the shape below are deliberate:
    ///
    /// * **Order.** Predict, then send, which is vanilla's
    ///   (`LocalPlayer.java:316-317`: `removeFromSelected` on line 316, the packet
    ///   on 317).
    /// * **Inside the `if let`.** [`drop_selected_action`] already returns `None`
    ///   for a spectator, so putting the prediction here gives it that gate for
    ///   free rather than duplicating the game-mode check — a spectator predicts
    ///   nothing and sends nothing, decided once.
    ///
    /// The prediction itself is `lodestone_game::menus::Menus::drop_selected`, a
    /// port of `Inventory.removeFromSelected`; see `docs/container-clicks.md` for
    /// why the container-screen `Q` ([`Self::send_container_drop`]) never had this
    /// bug.
    pub(super) fn send_drop_selected(&self, ctrl: bool) {
        let Some(net) = self.sim.net() else { return };
        let game_mode = net
            .shared_handle()
            .get()
            .cloned()
            .and_then(|handle| handle.game_mode());
        if let Some(action) = drop_selected_action(game_mode, ctrl) {
            net.predict_drop_selected(self.sim.selected_slot(), ctrl);
            net.send_action(action);
        }
    }

    /// The off-hand key pressed in normal gameplay (issue #385).
    ///
    /// Thin by design: everything decidable is in [`offhand_swap_action`], which
    /// takes the game mode rather than reading it off `self`, so the whole
    /// decision is testable without a window, a GPU or a live `Sim`.
    pub(super) fn send_offhand_swap(&self) {
        let Some(net) = self.sim.net() else { return };
        // Same read the fire/underwater overlay pass uses for `spectator`.
        let game_mode = net
            .shared_handle()
            .get()
            .cloned()
            .and_then(|handle| handle.game_mode());
        if let Some(action) = offhand_swap_action(game_mode) {
            net.send_action(action);
        }
    }
}
