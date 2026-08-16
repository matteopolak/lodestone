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

    /// Whether the anvil rename box has focus this instant — `AnvilScreen`'s
    /// `setCanLoseFocus(false)` plus `slotChanged`'s `setEditable(!itemStack.
    /// isEmpty())`, collapsed into one predicate since this box has no
    /// separate focus flag to track (see [`KeyGate::anvil_rename_active`]'s
    /// own doc): active exactly when the anvil screen is open and its input
    /// slot (slot 0) is occupied.
    pub(super) fn anvil_rename_active(&self) -> bool {
        self.active_container_menu().is_some_and(|menu| {
            menu.special_layout() == Some(lodestone_game::menu::SpecialLayout::Anvil)
                && menu.slot_item(0).is_some()
        })
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
        // A plugin-opened local menu has no server container, so its
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
            recipe_panel_contents(self.recipe_book.as_ref(), &self.recipe_panel, menu, book_type);
        let layout = recipe_panel_layout(
            &self.recipe_panel,
            menu,
            self.nav.gui_scale(),
            w,
            h,
            tab_count,
            total_pages,
            // Icons only, and the hit-test reads no icons — see
            // `recipe_panel_layout`'s own doc.
            &[],
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
                self.send_recipe_book_settings(book_type);
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
            // Vanilla's All/Craftable cycle-button
            // (`RecipeBookComponent.java`). Cycling it re-browses the
            // corpus through `craftable_in` and swaps the button art, so this
            // arm is two visible changes, not a flag.
            Hit::FilterButton => {
                self.recipe_panel.filtering = !self.recipe_panel.filtering;
                // A narrower set can leave `page` past the end. The contents
                // query clamps on read, but resetting here keeps the *arrows*
                // honest on the very next frame rather than one frame late.
                self.recipe_panel.page = 0;
                self.recipe_panel.search_focused = false;
                self.send_recipe_book_settings(book_type);
            }
            // A click on the panel body is still *consumed*, so it does not
            // fall through and click the container slot behind the panel.
            Hit::Panel => self.recipe_panel.search_focused = false,
        }
        true
    }

    /// Resolve a click at the current cursor against the merchant screen's
    /// trade-list buttons and act on it, returning whether it **consumed**
    /// the click — vanilla's `postButtonClick`
    /// (`MerchantScreen.java`): select the row, remember it, and
    /// tell the server (that fix's UI half).
    ///
    /// Given first refusal the same way
    /// [`Self::handle_recipe_panel_click`] is, and for the same reason: the
    /// trade buttons sit at local `x = 5..93`, well clear of the real payment
    /// slots (`x = 136..`), so there is no real overlap today, but a screen
    /// this contended is exactly where "one path forgets to check" bugs live,
    /// and the first-refusal shape is what lets a caller add a click surface
    /// without re-deriving the slot precedence rule each time.
    pub(super) fn handle_merchant_click(&mut self, menu: &Menu, w: u32, h: u32) -> bool {
        if menu.special_layout() != Some(lodestone_game::menu::SpecialLayout::Merchant) {
            return false;
        }
        let offer_count = self.sim.trades().offers().len();
        let Some(index) = crate::container::merchant::button_hit_test(
            menu,
            offer_count,
            self.nav.gui_scale(),
            w,
            h,
            self.cursor.0,
            self.cursor.1,
        ) else {
            return false;
        };
        self.merchant_selected = index;
        self.sim.send_select_trade(
            i32::try_from(index).expect("a trade row index is always in i32 range"),
        );
        true
    }

    /// Resolves a click against the beacon screen's power buttons and
    /// confirm/cancel controls (issue #613's `SetBeaconEffects` remainder),
    /// given first refusal for the same reason
    /// [`Self::handle_merchant_click`]'s own doc gives — this screen's
    /// buttons never overlap a real slot either, but a contended screen is
    /// exactly where a click path that forgets to check gets away with it.
    ///
    /// A power-button hit only ever updates local pending state
    /// ([`crate::container::beacon::BeaconSelection`]); the wire send
    /// happens on confirm, gated on
    /// [`crate::container::beacon::BeaconSelection::can_confirm`] the same
    /// way vanilla's own `BeaconConfirmButton.active` gates the press —
    /// `updateEffects` never runs server-side for an inactive confirm
    /// either, so a click that lands there while it is disabled is simply
    /// consumed and does nothing, matching what the player sees.
    pub(super) fn handle_beacon_click(&mut self, menu: &Menu, w: u32, h: u32) -> bool {
        if menu.special_layout() != Some(lodestone_game::menu::SpecialLayout::Beacon) {
            return false;
        }
        let levels = self
            .sim
            .open_menu()
            .and_then(|open| open.data.iter().find(|(p, _)| *p == 0).map(|(_, v)| *v))
            .unwrap_or(0);
        let Some(hit) = crate::container::beacon::button_hit_test(
            menu,
            self.nav.gui_scale(),
            w,
            h,
            self.cursor.0,
            self.cursor.1,
            levels,
            self.beacon_selection.primary.as_ref(),
        ) else {
            return false;
        };
        match hit {
            crate::container::beacon::BeaconHit::Power {
                is_primary: true,
                effect,
            } => self.beacon_selection.select_primary(effect),
            crate::container::beacon::BeaconHit::Power {
                is_primary: false,
                effect,
            } => self.beacon_selection.select_secondary(effect),
            crate::container::beacon::BeaconHit::Confirm => {
                let has_payment = menu.slot_item(0).is_some();
                if self.beacon_selection.can_confirm(has_payment) {
                    self.sim.send_set_beacon_effects(
                        self.beacon_selection.primary.clone(),
                        self.beacon_selection.secondary.clone(),
                    );
                    self.sim.close_open_menu();
                }
            }
            crate::container::beacon::BeaconHit::Cancel => self.sim.close_open_menu(),
        }
        true
    }

    /// Report this panel's open/filter state for `book_type` to the server —
    /// vanilla's `ServerboundRecipeBookChangeSettingsPacket`, sent from
    /// `RecipeBookComponent`'s own toggle and filter handlers.
    ///
    /// This is the **producer** half of a round trip whose two other thirds
    /// already existed: every protocol family encodes
    /// [`ClientAction::SetRecipeBookSettings`] and nothing in the shell ever
    /// constructed one (an outbound island, the `ClientAction::SetFlying`
    /// shape), while the inbound `RECIPE_BOOK_SETTINGS` fold landed in
    /// `fd53995` and nothing read it. That fix.
    ///
    /// Sent directly rather than queued through `ActionQueue`, like
    /// `Sim::send_selected_slot`: this is a discrete click, not a per-tick
    /// state, and `ActionQueue` only drains inside the tick loop.
    ///
    /// Deliberately *not* guarded on "changed": both call sites have already
    /// flipped the value they report, so every call is a real change.
    /// `restore_recipe_book_settings` is the one path that writes these fields
    /// without reporting, which is what stops a restore echoing straight back
    /// at the server that sent it.
    pub(super) fn send_recipe_book_settings(&mut self, book_type: lodestone_model::RecipeBookType) {
        self.sim.send_recipe_book_settings(
            book_type,
            self.recipe_panel.open,
            self.recipe_panel.filtering,
        );
    }

    /// Auto-fill the crafting grid for `id` (that fix's "click a recipe to
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
    /// (part 3).
    ///
    /// Vanilla's `AbstractContainerScreen.checkHotbarKeyPressed`
    /// (`AbstractContainerScreen.java`) guards on exactly two pieces of
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
        // The creative screen replaces the inventory screen rather than overlaying it,
        // and its slot clicks never become a `container_click` — see
        // `container/creative.rs`'s `CreativeEffect`. Vanilla's own
        // `CreativeModeInventoryScreen.keyPressed` reaches `checkHotbarKeyPressed`
        // through its overridden `slotClicked`, which is what this routes to. The
        // carried-empty guard below is vanilla's too, so it still applies.
        if self.creative_screen_open() {
            if self.sim.player_menu().carried().is_none() {
                self.handle_creative_key(button, lodestone_game::click::ContainerInput::Swap);
            }
            return;
        }
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
        let hit = crate::container::hit_test_with_book(
            &menu,
            self.nav.gui_scale(),
            w,
            h,
            self.cursor.0,
            self.cursor.1,
            // The same flag `redraw` gives `ContainerFrame::with_book_open`; an
            // open book shifts the panel, so an unshifted hit-test would send
            // every click one panel-offset to the left.
            self.recipe_panel.open,
        );
        let MenuHit::Slot(index) = hit else { return };
        // Vanilla's `40` is the off-hand button and `do_swap`'s `button == 40` arm
        // handles it. Since that fix freed `F` the off-hand binding does reach here;
        // note this is the **container** route only — the no-screen route is
        // `send_offhand_swap` below, a different packet entirely.
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
    /// (`AbstractContainerScreen.java`) and the `PickItem`/`Drop`
    /// `else if` — duplicating either here would be a second copy that can
    /// drift from the one `container.rs` already tests. `Click::drop_one`/
    /// `drop_stack` and `do_throw` (`lodestone-game`) were built and tested
    /// under that fix with zero producers before this; this is the first caller.
    pub(super) fn send_container_drop(&mut self, ctrl: bool) {
        // Same interception as `send_container_swap`. Vanilla's raw button number for a
        // throw is `0` for one item and `1` for the whole stack, which is what `ctrl`
        // selects (`AbstractContainerScreen.keyPressed`'s `hasControlDown()`).
        if self.creative_screen_open() {
            let button = i32::from(ctrl);
            self.handle_creative_key(button, lodestone_game::click::ContainerInput::Throw);
            return;
        }
        let (Some(menu), Some((w, h))) = (
            self.active_container_menu(),
            self.target.as_ref().map(RenderTarget::size),
        ) else {
            return;
        };
        let hit = crate::container::hit_test_with_book(
            &menu,
            self.nav.gui_scale(),
            w,
            h,
            self.cursor.0,
            self.cursor.1,
            // The same flag `redraw` gives `ContainerFrame::with_book_open`; an
            // open book shifts the panel, so an unshifted hit-test would send
            // every click one panel-offset to the left.
            self.recipe_panel.open,
        );
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
    /// against the hovered slot (`AbstractContainerScreen.java`).
    ///
    /// Identical in shape to [`Self::send_container_drop`] except that there is
    /// no modifier variant to carry: vanilla's clone click has no `ctrl` form.
    /// The same `creative: false` gap applies — no game-mode plumbing exists on
    /// `Sim` yet, which matters more here than for drop, because vanilla's clone
    /// click is *creative-only*; until that lands this resolves and then produces
    /// no clicks, which is the honest degradation rather than a fabricated one.
    pub(super) fn send_container_pick_item(&mut self) {
        // Same interception as `send_container_swap`. This is the one click type the
        // creative screen makes *reachable*: `AbstractContainerScreen` gates
        // `ContainerInput::CLONE` on `player.hasInfiniteMaterials()`, so on the ordinary
        // container path below it still resolves to nothing.
        if self.creative_screen_open() {
            self.handle_creative_key(0, lodestone_game::click::ContainerInput::Clone);
            return;
        }
        let (Some(menu), Some((w, h))) = (
            self.active_container_menu(),
            self.target.as_ref().map(RenderTarget::size),
        ) else {
            return;
        };
        let hit = crate::container::hit_test_with_book(
            &menu,
            self.nav.gui_scale(),
            w,
            h,
            self.cursor.0,
            self.cursor.1,
            // The same flag `redraw` gives `ContainerFrame::with_book_open`; an
            // open book shifts the panel, so an unshifted hit-test would send
            // every click one panel-offset to the left.
            self.recipe_panel.open,
        );
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
    /// # The local prediction is not optional
    ///
    /// This used to be `send_action` and nothing else, and an owner reported the
    /// consequence: *"throwing out items with Q doesn't update the count in my
    /// inventory or hotbar, but it does work properly otherwise."* `DROP_ITEM` /
    /// `DROP_ALL_ITEMS` are the one inventory change a vanilla server applies
    /// **silently** — `ServerGamePacketListenerImpl.java` calls
    /// `player.drop(…)` and returns with no slot or content packet — so an
    /// unpredicted drop leaves the count wrong *forever*, not briefly.
    ///
    /// Two things about the shape below are deliberate:
    ///
    /// * **Order.** Predict, then send, which is vanilla's own order inside
    ///   `LocalPlayer.removeFromSelected`.
    /// * **Inside the `if let`.** [`drop_selected_action`] already returns `None`
    ///   for a spectator, so putting the prediction here gives it that gate for
    ///   free rather than duplicating the game-mode check — a spectator predicts
    ///   nothing and sends nothing, decided once.
    ///
    /// The prediction itself is `lodestone_game::menus::Menus::drop_selected`, a
    /// port of `Inventory.removeFromSelected`; see `docs/container-clicks.md` for
    /// why the container-screen `Q` ([`Self::send_container_drop`]) never had this
    /// bug.
    pub(super) fn send_drop_selected(&mut self, ctrl: bool) {
        let Some(net) = self.sim.net() else { return };
        let game_mode = net
            .shared_handle()
            .get()
            .cloned()
            .and_then(|handle| handle.game_mode());
        let Some(action) = drop_selected_action(game_mode, ctrl) else {
            return;
        };
        // Vanilla's `Minecraft.handleKeybinds` swings the main hand only when
        // `Player.drop` reports it actually dropped something, so an empty slot
        // is silent. `predict_drop_selected` is our answer to the same question,
        // which is why the swing hangs off its return value rather than off the
        // keypress: pressing the key on an empty hotbar slot must not animate.
        let dropped = net.predict_drop_selected(self.sim.selected_slot(), ctrl);
        net.send_action(action);
        if dropped {
            self.sim.swing_hand();
        }
    }

    /// The off-hand key pressed in normal gameplay.
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
