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
        let furnace_input_items = self.active_container_menu().and_then(|menu| {
            let key = furnace_input_property_key(&menu)?;
            self.sim
                .recipe_property_set(&key)
                .map(|item_ids| furnace_input_items(&item_ids))
        });
        let ctx = furnace_input_items.map_or_else(PlayerCtx::survival, |items| {
            PlayerCtx::survival().with_furnace_input_items(items)
        });
        // `Sim` has no game-mode accessor to source a real `PlayerCtx` from
        // at this call site, so use the conservative survival context. This
        // matches the only existing production-shaped precedent
        // (`container.rs`'s own click-driving tests use `PlayerCtx::survival()`
        // /`::creative()` explicitly rather than reading one off anything).
        let _ = handle.menu_click(click, ctx);
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
            //. Cycling it re-browses the
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
    ///: select the row, remember it, and
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
    /// confirm/cancel controls (`SetBeaconEffects` remainder),
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

    /// The crafter's own click override (`SetContainerSlotState`
    /// remainder): a plain click on an empty, non-spectator crafter slot
    /// toggles that slot's enabled/disabled state — `CrafterScreen.slotClicked`
    /// (`.cache/mc/26.2/client-src`)'s `PICKUP` case: re-enable a disabled
    /// slot unconditionally, or disable an enabled one only when nothing is
    /// carried (so placing an item there still works normally).
    ///
    /// **Unlike [`Self::handle_beacon_click`]/[`Self::handle_enchant_click`],
    /// this never *consumes* the click** — vanilla's own override still calls
    /// `super.slotClicked(...)` unconditionally right after its toggle check,
    /// so the ordinary (here, effectively a no-op — the slot is empty and the
    /// cursor usually is too) container click still goes out alongside this
    /// one. Callers should invoke this as a side effect beside the normal
    /// click dispatch, not as part of the `consumed_by_*` first-refusal
    /// chain.
    ///
    /// **Named simplifications**: only a plain click (`ContainerInput::PICKUP`)
    /// is handled — vanilla's `SWAP` case (pressing a hotbar number over a
    /// disabled slot holding a matching item) is not. And there is no local,
    /// optimistic `containerData` mutation the way `CrafterMenu.setSlotState`
    /// gives vanilla's own client — the toggle only becomes visible once the
    /// server's `container_set_data` echoes it back into
    /// [`lodestone_client::OpenMenuSnapshot::data`] (already threaded end to
    /// end; see `docs/container-cost-screens.md`). No render support for the
    /// disabled-slot sprite exists yet either — this lands the wire half
    /// only, the same scope [`Self::handle_beacon_click`]'s own remainder
    /// once was.
    pub(super) fn maybe_toggle_crafter_slot(&mut self, menu: &Menu, hit: MenuHit) {
        let MenuHit::Slot(index) = hit else { return };
        if index >= 9 {
            return;
        }
        let Some(open) = self.sim.open_menu() else {
            return;
        };
        if open.menu_type.namespace() != "minecraft" || open.menu_type.path() != "crafter_3x3" {
            return;
        }
        if menu.slot_item(index).is_some() {
            return;
        }
        #[allow(clippy::cast_possible_wrap)]
        let slot_id = index as i32;
        let disabled = open
            .data
            .iter()
            .find(|(id, _)| *id == slot_id)
            .is_some_and(|(_, value)| *value == 1);
        let Some(new_state) =
            crate::container::crafter::toggle_decision(disabled, menu.carried().is_some())
        else {
            return;
        };
        let window_id = open.window_id;
        self.sim
            .send_set_container_slot_state(slot_id, window_id, new_state);
    }

    /// One `MouseWheel` notch over a slot holding a bundle: scroll-selects
    /// which of its contents is highlighted and reports the new selection to
    /// the server (the bundle-item selection action — see
    /// `crate::container::bundle`'s module
    /// doc for the algorithm and why the tracked selection lives on
    /// `WindowApp` rather than mutated into the stack itself). Returns
    /// whether the notch was consumed, the same "did this surface claim it"
    /// shape [`Self::handle_beacon_click`]/[`Self::handle_enchant_click`]
    /// already use, so a caller can skip falling through to any other scroll
    /// handling for the same event.
    ///
    /// **Not wired to vanilla's `onStopHovering`/`onSlotClicked` reset**:
    /// this only ever resets the tracked selection when a later notch lands
    /// on a different (or no longer scrollable) slot, not the instant the
    /// cursor merely leaves the bundle slot with no further scroll, and not
    /// on a quick-move/swap click. The send is purely advisory (only a later
    /// right-click removal reads it server-side, and nothing broadcasts it
    /// back — see [`crate::sim::Sim::send_select_bundle_item`]'s doc), so a
    /// stale local highlight costs nothing beyond an occasional redundant or
    /// slightly-late send; a full per-frame hover tracker was judged not
    /// worth the plumbing for that.
    pub(super) fn handle_bundle_scroll(&mut self, wheel: f64, w: u32, h: u32) -> bool {
        let Some(menu) = self.active_container_menu() else {
            self.bundle_selection = None;
            return false;
        };
        let hit = crate::container::hit_test_with_book(
            &menu,
            self.nav.gui_scale(),
            w,
            h,
            self.cursor.0,
            self.cursor.1,
            self.recipe_panel.open,
        );
        let MenuHit::Slot(index) = hit else {
            self.bundle_selection = None;
            return false;
        };
        let Some(stack) = menu.slot_item(index) else {
            self.bundle_selection = None;
            return false;
        };
        let window_id = self.sim.open_menu().map_or(0, |open| open.window_id);
        #[allow(clippy::cast_possible_wrap)]
        let slot = index as i32;
        let Some(selection) = crate::container::bundle::bundle_slot_scrolled(
            window_id,
            slot,
            stack,
            wheel,
            self.bundle_selection,
        ) else {
            self.bundle_selection = None;
            return false;
        };
        self.bundle_selection = Some(selection);
        self.sim
            .send_select_bundle_item(selection.slot, selection.selected);
        true
    }

    /// The enchanting table's three enchant-offer rows. Unlike the beacon's
    /// power buttons,
    /// there is no local pending state to update here — a hit *is* the send,
    /// gated the same way [`crate::container::enchant::offer_clickable`]
    /// gates it client-side: it checks the local menu mirror, never mutates
    /// it, and only then sends the action. The screen stays open afterwards;
    /// pressing an offer never closes it.
    pub(super) fn handle_enchant_click(&mut self, menu: &Menu, w: u32, h: u32) -> bool {
        if menu.special_layout() != Some(lodestone_game::menu::SpecialLayout::Enchanting) {
            return false;
        }
        let Some(open) = self.sim.open_menu() else { return false };
        let mut costs = [0i32; 3];
        for (property, value) in &open.data {
            if let Ok(index) = usize::try_from(*property)
                && index < 3
            {
                costs[index] = *value;
            }
        }
        // `EnchantmentMenu`'s lapis slot is menu index 1 — the same constant
        // `container::geometry::draw_enchanting_costs` reads.
        const LAPIS_SLOT: usize = 1;
        let lapis_count = menu.slot_item(LAPIS_SLOT).map_or(0, lodestone_game::item::ItemStack::count);
        let xp_level = self.sim.xp().map_or(0, |(level, _)| level);
        let has_infinite_materials = self.sim.has_infinite_materials();
        let Some(row) = crate::container::enchant::button_hit_test(
            menu,
            self.nav.gui_scale(),
            w,
            h,
            self.cursor.0,
            self.cursor.1,
            costs,
            lapis_count,
            xp_level,
            has_infinite_materials,
        ) else {
            return false;
        };
        self.sim.send_container_button_click(open.window_id, row);
        true
    }

    /// The stonecutter's recipe-selection grid (`ContainerButtonClick`'s
    /// remainder for this screen — see
    /// [`crate::container::stonecutter`]'s module doc). Same shape as
    /// [`Self::handle_enchant_click`]: a hit *is* the send, pre-validated
    /// client-side the same way vanilla's own `StonecutterMenu` mirror gates
    /// `clickMenuButton` before ever reaching the network.
    ///
    /// `start_index` reads the persisted [`Self::stonecutter_scroll`] offset
    /// through [`crate::container::stonecutter::start_index_for_scroll`] —
    /// **stale, corrected**: this used to be pinned at `0` with no scroll
    /// input wired anywhere, which was true when written and is not any
    /// more (see [`Self::scroll_stonecutter`]). The screen stays open
    /// afterwards, matching `StonecutterScreen`: selecting a recipe never
    /// closes it.
    pub(super) fn handle_stonecutter_click(&mut self, menu: &Menu, w: u32, h: u32) -> bool {
        if menu.special_layout() != Some(lodestone_game::menu::SpecialLayout::Stonecutter) {
            return false;
        }
        let Some(open) = self.sim.open_menu() else { return false };
        let results = crate::container::stonecutter::server_results_for_menu(
            menu,
            &self.sim.known_recipes(),
        );
        let recipe_count = results.len();
        if recipe_count == 0 {
            return false;
        }
        let start_index =
            crate::container::stonecutter::start_index_for_scroll(self.stonecutter_scroll, recipe_count);
        let Some(index) = crate::container::stonecutter::button_hit_test(
            menu,
            self.nav.gui_scale(),
            w,
            h,
            self.cursor.0,
            self.cursor.1,
            recipe_count,
            start_index,
        ) else {
            return false;
        };
        self.sim.send_container_button_click(open.window_id, index);
        true
    }

    /// One `MouseWheel` notch over an open stonecutter screen: advances
    /// [`Self::stonecutter_scroll`] through
    /// [`crate::container::stonecutter::scroll_offset_after_wheel`] —
    /// `StonecutterScreen.mouseScrolled`'s own step. Returns whether the
    /// notch was consumed (the stonecutter screen is open and has a
    /// non-empty match list), the same "did this surface claim it" shape
    /// [`Self::handle_bundle_scroll`] uses, so a caller can gate any further
    /// scroll handling on it.
    ///
    /// Unlike vanilla's `mouseScrolled`, this does not check the cursor
    /// position — vanilla does not either: `StonecutterScreen` overrides
    /// `mouseScrolled` with no bounds check at all, since the whole screen
    /// has exactly one scrollable region.
    pub(super) fn scroll_stonecutter(&mut self, notches: f64) -> bool {
        let Some(menu) = self.active_container_menu() else { return false };
        if menu.special_layout() != Some(lodestone_game::menu::SpecialLayout::Stonecutter) {
            return false;
        }
        let results = crate::container::stonecutter::server_results_for_menu(
            &menu,
            &self.sim.known_recipes(),
        );
        let recipe_count = results.len();
        if recipe_count == 0 {
            return false;
        }
        self.stonecutter_scroll = crate::container::stonecutter::scroll_offset_after_wheel(
            self.stonecutter_scroll,
            notches as f32,
            recipe_count,
        );
        true
    }

    /// The loom's 32-pattern grid (`ContainerButtonClick`'s remainder for
    /// this screen — see [`crate::container::loom`]'s module doc). Same
    /// shape as [`Self::handle_stonecutter_click`]: a hit *is* the send,
    /// pre-validated client-side against
    /// [`crate::container::loom::display_patterns`]/
    /// [`crate::container::loom::selectable_pattern_count`] the same way
    /// vanilla's own `LoomMenu` mirror gates `clickMenuButton` before ever
    /// reaching the network. The screen stays open afterwards, matching
    /// `LoomScreen`: selecting a pattern never closes it.
    pub(super) fn handle_loom_click(&mut self, menu: &Menu, w: u32, h: u32) -> bool {
        if menu.special_layout() != Some(lodestone_game::menu::SpecialLayout::Loom) {
            return false;
        }
        let Some(open) = self.sim.open_menu() else { return false };
        let banner = menu.slot_item(crate::container::loom::BANNER_SLOT);
        let dye = menu.slot_item(crate::container::loom::DYE_SLOT);
        let pattern_item = menu.slot_item(crate::container::loom::PATTERN_SLOT);
        if !crate::container::loom::display_patterns(banner, dye, pattern_item) {
            return false;
        }
        let pattern_count = crate::container::loom::selectable_pattern_count(pattern_item);
        let start_row =
            crate::container::loom::start_row_for_scroll(self.loom_scroll, pattern_count);
        let Some(index) = crate::container::loom::button_hit_test(
            menu,
            self.nav.gui_scale(),
            w,
            h,
            self.cursor.0,
            self.cursor.1,
            pattern_count,
            start_row,
        ) else {
            return false;
        };
        self.sim.send_container_button_click(open.window_id, index);
        true
    }

    /// One `MouseWheel` notch over an open loom screen — the same shape as
    /// [`Self::scroll_stonecutter`], advancing [`Self::loom_scroll`] through
    /// [`crate::container::loom::scroll_offset_after_wheel`]
    /// (`LoomScreen.mouseScrolled`'s own step, also with no cursor-position
    /// check, matching vanilla).
    pub(super) fn scroll_loom(&mut self, notches: f64) -> bool {
        let Some(menu) = self.active_container_menu() else { return false };
        if menu.special_layout() != Some(lodestone_game::menu::SpecialLayout::Loom) {
            return false;
        }
        let banner = menu.slot_item(crate::container::loom::BANNER_SLOT);
        let dye = menu.slot_item(crate::container::loom::DYE_SLOT);
        let pattern_item = menu.slot_item(crate::container::loom::PATTERN_SLOT);
        if !crate::container::loom::display_patterns(banner, dye, pattern_item) {
            return false;
        }
        let pattern_count = crate::container::loom::selectable_pattern_count(pattern_item);
        self.loom_scroll = crate::container::loom::scroll_offset_after_wheel(
            self.loom_scroll,
            notches as f32,
            pattern_count,
        );
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
    /// Vanilla's hotbar-swap key handling
    /// guards on exactly two pieces of
    /// **state**: the cursor stack must be empty, and a slot must be hovered. Both
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
        // `container/creative.rs`'s `CreativeEffect`. Vanilla's own creative screen
        // reaches the same hotbar-swap key handling through its own overridden
        // slot-click routing, which is what this routes to. The
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
    /// `key_pressed` already carries vanilla's guard that the hovered slot must
    /// hold an item, and the `PickItem`/`Drop`
    /// `else if` — duplicating either here would be a second copy that can
    /// drift from the one `container.rs` already tests. `Click::drop_one`/
    /// `drop_stack` and `do_throw` (`lodestone-game`) were built and tested
    /// under that fix with zero producers before this; this is the first caller.
    pub(super) fn send_container_drop(&mut self, ctrl: bool) {
        // Same interception as `send_container_swap`. Vanilla's raw button number for a
        // throw is `0` for one item and `1` for the whole stack, which is what `ctrl`
        // selects (the container screen's drop-key handler reads the control-key
        // modifier to pick between them).
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

    /// `key.pickItem` pressed with a container screen open — vanilla's clone
    /// click against the hovered slot.
    ///
    /// Identical in shape to [`Self::send_container_drop`] except that there is
    /// no modifier variant to carry: vanilla's clone click has no `ctrl` form.
    /// The same `creative: false` gap applies — no game-mode plumbing exists on
    /// `Sim` yet, which matters more here than for drop, because vanilla's clone
    /// click is *creative-only*; until that lands this resolves and then produces
    /// no clicks, which is the honest degradation rather than a fabricated one.
    pub(super) fn send_container_pick_item(&mut self) {
        // Same interception as `send_container_swap`. This is the one click type the
        // creative screen makes *reachable*: vanilla's container-screen click handling
        // gates the clone click on the player being in creative mode, so on the ordinary
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
    /// **silently** — the server drops the item and returns with no slot or
    /// content packet — so an unpredicted drop leaves the count wrong
    /// *forever*, not briefly.
    ///
    /// Two things about the shape below are deliberate:
    ///
    /// * **Order.** Predict, then send, which is vanilla's own order: the
    ///   client-side slot is cleared before the drop is sent to the server.
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

/// The server property-set key that belongs to this furnace-family `menu`.
///
/// This gate deliberately runs before the session read in
/// [`WindowApp::send_menu_click`], so every other container click avoids even
/// copying a property-set member.
fn furnace_input_property_key(menu: &Menu) -> Option<lodestone_model::Identifier> {
    let key = match menu.special_layout() {
        Some(lodestone_game::menu::SpecialLayout::Furnace) => "minecraft:furnace_input",
        Some(lodestone_game::menu::SpecialLayout::BlastFurnace) => {
            "minecraft:blast_furnace_input"
        }
        Some(lodestone_game::menu::SpecialLayout::Smoker) => "minecraft:smoker_input",
        _ => return None,
    };
    Some(key.parse().expect("the static property-set key is valid"))
}

/// Resolves a server numeric cooking-input property set into the identifier
/// representation the version-free click predictor owns.
///
/// This is deliberately a shell-boundary conversion: recipe sync keeps wire
/// registry ids, while `ItemStack` in `lodestone-game` keeps identifiers.
/// Malformed ids are ignored rather than becoming invented item identities.
fn furnace_input_items(item_ids: &[i32]) -> Vec<lodestone_model::Identifier> {
    item_ids
        .iter()
        .filter_map(|item_id| lodestone_data::items::item_name(*item_id))
        .filter_map(|item_name| item_name.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_game::{menu::SpecialLayout, recipe_sync::RecipeBookSync};
    use lodestone_model::event::ClientEvent;

    #[test]
    fn furnace_input_property_set_resolves_numeric_ids_at_the_shell_boundary() {
        let mut recipe_sync = RecipeBookSync::new();
        // `crates/lodestone-data/tests/support/item_prototype_jvm.txt` records
        // protocol item id 931 as `minecraft:raw_iron` independently of this resolver.
        const RAW_IRON: i32 = 931;
        recipe_sync.apply(&ClientEvent::RecipePropertySetsUpdated {
            item_sets: vec![(("minecraft:furnace_input").parse().unwrap(), vec![RAW_IRON])],
            stonecutter_results: Vec::new(),
        });

        let menu = Menu::furnace(SpecialLayout::Furnace);
        let key = furnace_input_property_key(&menu)
            .expect("a furnace must request its cooking-input property set");
        let input_items = furnace_input_items(
            recipe_sync
                .property_set(&key)
                .expect("the declared property set must be carried into click prediction"),
        );

        assert_eq!(
            input_items,
            vec![("minecraft:raw_iron").parse().unwrap()],
            "the numeric property-set member resolves to the stack identifier"
        );
        assert!(
            furnace_input_property_key(&Menu::generic(9)).is_none(),
            "unrelated menus must not read recipe-book property data"
        );
    }

    #[test]
    fn furnace_family_uses_each_screen_specific_property_key() {
        // The same captured item-prototype fixture fixes 931 as raw iron and
        // 932 as iron ingot. An off-by-one registry mapping produces two
        // different identifiers.
        const RAW_IRON: i32 = 931;
        const IRON_INGOT: i32 = 932;

        for (layout, property_key) in [
            (SpecialLayout::Furnace, "minecraft:furnace_input"),
            (SpecialLayout::BlastFurnace, "minecraft:blast_furnace_input"),
            (SpecialLayout::Smoker, "minecraft:smoker_input"),
        ] {
            let mut recipe_sync = RecipeBookSync::new();
            recipe_sync.apply(&ClientEvent::RecipePropertySetsUpdated {
                item_sets: vec![(property_key.parse().unwrap(), vec![RAW_IRON, IRON_INGOT])],
                stonecutter_results: Vec::new(),
            });

            let key = furnace_input_property_key(&Menu::furnace(layout))
                .expect("each furnace-family layout selects a property key");
            assert_eq!(key, property_key.parse().unwrap());
            assert_eq!(
                furnace_input_items(
                    recipe_sync
                        .property_set(&key)
                        .expect("the menu-specific set must be selected"),
                ),
                vec![
                    ("minecraft:raw_iron").parse().unwrap(),
                    ("minecraft:iron_ingot").parse().unwrap(),
                ],
                "{property_key} must reach its matching furnace-family layout"
            );
        }
    }
}
