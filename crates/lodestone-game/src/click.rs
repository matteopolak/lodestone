//! The container click state machine.
//!
//! Every inventory interaction the client sends is one `container_click` packet:
//! a slot index, a button number, and a mode. This module re-implements the
//! server-side interpretation of those packets — vanilla
//! `AbstractContainerMenu.doClick` — as an original, version-free predictor over
//! a [`Menu`]. The client runs exactly this locally to predict the result of a
//! click before the server confirms it (see [`crate::reconcile`]).
//!
//! The seven modes and their button meanings:
//!
//! | mode | [`ContainerInput`] | button semantics |
//! |------|--------------------|------------------|
//! | 0 | `Pickup`     | 0 = left (whole), 1 = right (half/one); slot −999 drops the cursor |
//! | 1 | `QuickMove`  | 0/1 = shift-click quick transfer |
//! | 2 | `Swap`       | 0–8 = hotbar key, 40 = off-hand key |
//! | 3 | `Clone`      | middle-click, creative only |
//! | 4 | `Throw`      | 0 = drop one, 1 = drop stack (with cursor empty) |
//! | 5 | `QuickCraft` | the multi-stage drag sequence (start / add / end) |
//! | 6 | `PickupAll`  | double-click gather |
//!
//! The **drag** protocol (mode 5) is the multi-packet sequence that trips up
//! most implementations: a *start* packet arms a drag of a given type, one *add*
//! packet per painted slot records it, and an *end* packet distributes the
//! cursor across the recorded slots — an even split for a left-drag, one item
//! each for a right-drag, and a full stack each for a creative middle-drag. The
//! button number packs a 2-bit header (start/add/end) and 2-bit type; see
//! [`quick_craft_mask`].

use std::sync::Arc;

use lodestone_model::Identifier;

use crate::{
    item::ItemStack,
    menu::{Menu, OFFHAND_NATIVE, OUTSIDE_SLOT},
};

/// A container-click mode (`ContainerInput` in vanilla).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContainerInput {
    /// Left/right pickup and place (mode 0).
    Pickup,
    /// Shift-click quick transfer (mode 1).
    QuickMove,
    /// Hotbar / off-hand key swap (mode 2).
    Swap,
    /// Creative middle-click clone (mode 3).
    Clone,
    /// Drop from a slot (mode 4).
    Throw,
    /// Drag distribute, multi-stage (mode 5).
    QuickCraft,
    /// Double-click gather (mode 6).
    PickupAll,
}

/// The three phases of a drag (`quickcraftHeader`).
pub mod drag_header {
    /// Arms a drag of a given type.
    pub const START: i32 = 0;
    /// Records a painted slot.
    pub const ADD: i32 = 1;
    /// Distributes the cursor across recorded slots.
    pub const END: i32 = 2;
}

/// The three drag distribution types (`quickcraftType`).
pub mod drag_type {
    /// Left-drag: even split across slots.
    pub const EVEN: i32 = 0;
    /// Right-drag: one item per slot.
    pub const ONE: i32 = 1;
    /// Creative middle-drag: a full stack per slot.
    pub const CLONE: i32 = 2;
}

/// The closed set of drag-distribution semantics carried by a quick-craft
/// button. Raw button values are accepted only at the click boundary; the
/// menu state and all split arithmetic use this type so a header bit cannot
/// accidentally be treated as a distribution mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum QuickCraftType {
    /// Divide the carried stack evenly across the painted slots.
    Even,
    /// Place one item into each painted slot.
    One,
    /// Place a full stack into each painted slot (creative only).
    Clone,
}

impl QuickCraftType {
    /// Validates the two-bit wire value from a quick-craft button.
    #[must_use]
    pub const fn from_raw(raw: i32) -> Option<Self> {
        match raw {
            drag_type::EVEN => Some(Self::Even),
            drag_type::ONE => Some(Self::One),
            drag_type::CLONE => Some(Self::Clone),
            _ => None,
        }
    }

    /// Returns the two-bit value used in a container-click button.
    #[must_use]
    pub const fn raw(self) -> i32 {
        match self {
            Self::Even => drag_type::EVEN,
            Self::One => drag_type::ONE,
            Self::Clone => drag_type::CLONE,
        }
    }

    /// Whether this distribution mode is available to the player.
    #[must_use]
    pub const fn allowed_for(self, infinite_materials: bool) -> bool {
        !matches!(self, Self::Clone) || infinite_materials
    }
}

/// Packs a drag header and type into a click button number.
#[must_use]
pub fn quick_craft_mask(header: i32, kind: i32) -> i32 {
    (header & 3) | ((kind & 3) << 2)
}

/// Extracts the drag header from a button number.
#[must_use]
pub fn quick_craft_header(mask: i32) -> i32 {
    mask & 3
}

/// Extracts the drag type from a button number.
#[must_use]
pub fn quick_craft_type(mask: i32) -> i32 {
    (mask >> 2) & 3
}

/// Player state a click depends on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlayerCtx {
    /// Whether the player has infinite materials (creative): enables clone and
    /// the creative drag type.
    pub infinite_materials: bool,
    /// Whether the player may drop items.
    pub can_drop: bool,
    /// The currently selected hotbar slot (0–8), a native player-inventory
    /// index — vanilla's own selected-slot field. Consulted by
    /// the swap-overflow path's give-back-to-inventory scan, which tries this
    /// slot before the off-hand and before the general scan (vanilla's own
    /// slot-with-remaining-space search). Defaults to `0`,
    /// which is also every production caller's current value — nothing yet
    /// reads the live selected hotbar slot into a `PlayerCtx` (see
    /// `lodestone-shell`'s `app.rs::send_menu_click`, which hardcodes
    /// `PlayerCtx::survival()` for the same reason it hardcodes game mode).
    pub selected_hotbar_slot: usize,
    /// The current furnace-family input property set, resolved from the
    /// server's numeric item ids at the application boundary. `None` means the
    /// server has not declared one, so quick-move keeps its generic behavior.
    pub furnace_input_items: Option<Arc<[Identifier]>>,
}

impl Default for PlayerCtx {
    fn default() -> Self {
        Self {
            infinite_materials: false,
            can_drop: true,
            selected_hotbar_slot: 0,
            furnace_input_items: None,
        }
    }
}

impl PlayerCtx {
    /// A survival-mode player context.
    #[must_use]
    pub fn survival() -> Self {
        Self::default()
    }

    /// A creative-mode player context.
    #[must_use]
    pub fn creative() -> Self {
        Self {
            infinite_materials: true,
            can_drop: true,
            selected_hotbar_slot: 0,
            furnace_input_items: None,
        }
    }

    /// Attaches the server-declared furnace-family input items to this click.
    #[must_use]
    pub fn with_furnace_input_items(
        mut self,
        items: impl Into<Arc<[Identifier]>>,
    ) -> Self {
        self.furnace_input_items = Some(items.into());
        self
    }
}

/// Side effects of a click that leave the menu.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClickOutcome {
    /// Stacks thrown into the world by this click (drops).
    pub dropped: Vec<ItemStack>,
}

/// One painted cell's provisional contents partway through a drag — what the
/// release would put there, and what is already there.
///
/// Produced by [`Menu::quick_craft_plan`], consumed both by the release path and
/// by the container screen's live preview. See that method for why the two share
/// this rather than each deriving the split.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuickCraftCell {
    /// The painted menu-slot index.
    pub menu_index: usize,
    /// The **total** count the cell would hold on release — the per-slot share
    /// plus whatever the cell already has, clamped to the cell's cap. Not the
    /// amount added.
    pub count: i32,
    /// What the cell already holds (`0` when empty). `count - existing` is the
    /// amount this cell takes off the cursor.
    pub existing: i32,
    /// Whether [`count`](Self::count) was cut by the cell's cap. Vanilla draws a
    /// clamped preview count in **yellow** (vanilla's own container-screen
    /// extract-slot step), which is the whole reason this flag is carried out rather than recomputed.
    pub clamped: bool,
}

/// A distinct click action, primary (left) or secondary (right).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ClickAction {
    Primary,
    Secondary,
}

impl Menu {
    /// Applies one raw container-click to the menu, mutating slots and the
    /// cursor exactly as the server would, and returns any world-side effects.
    ///
    /// `slot_index` may be [`OUTSIDE_SLOT`] (−999) for a click outside the
    /// window. This is the low-level entry point; ergonomic constructors are on
    /// [`Click`].
    pub fn do_click(
        &mut self,
        slot_index: i32,
        button: i32,
        input: ContainerInput,
        ctx: PlayerCtx,
    ) -> ClickOutcome {
        let mut outcome = ClickOutcome::default();
        self.bump_state();

        if input == ContainerInput::QuickCraft {
            self.do_quick_craft(slot_index, button, ctx, &mut outcome);
            return outcome;
        }

        // Any non-drag click while a drag is armed cancels the drag.
        if self.quick_craft_status() != 0 {
            self.reset_quick_craft();
            return outcome;
        }

        match input {
            ContainerInput::Pickup | ContainerInput::QuickMove if button == 0 || button == 1 => {
                let action = if button == 0 {
                    ClickAction::Primary
                } else {
                    ClickAction::Secondary
                };
                if slot_index == OUTSIDE_SLOT {
                    self.do_drop_cursor(action, &mut outcome);
                } else if input == ContainerInput::QuickMove {
                    self.do_quick_move(slot_index, ctx.furnace_input_items.as_deref());
                } else {
                    self.do_pickup(slot_index, action);
                }
            }
            ContainerInput::Swap if (0..9).contains(&button) || button == 40 => {
                self.do_swap(slot_index, button, ctx, &mut outcome);
            }
            ContainerInput::Clone
                if ctx.infinite_materials && self.carried().is_none() && slot_index >= 0 =>
            {
                self.do_clone(slot_index);
            }
            ContainerInput::Throw if self.carried().is_none() && slot_index >= 0 => {
                self.do_throw(slot_index, button, ctx, &mut outcome);
            }
            ContainerInput::PickupAll if slot_index >= 0 => {
                self.do_pickup_all(slot_index, button);
            }
            _ => {}
        }

        outcome
    }

    fn do_drop_cursor(&mut self, action: ClickAction, outcome: &mut ClickOutcome) {
        let Some(mut carried) = self.carried().cloned() else {
            return;
        };
        match action {
            ClickAction::Primary => {
                outcome.dropped.push(carried);
                self.set_carried(None);
            }
            ClickAction::Secondary => {
                let one = carried.split(1);
                outcome.dropped.push(one);
                self.set_carried(crate::item::normalize(carried));
            }
        }
    }

    fn do_quick_move(&mut self, slot_index: i32, furnace_input_items: Option<&[Identifier]>) {
        let Ok(index) = usize::try_from(slot_index) else {
            return;
        };
        if !self.may_pickup(index) {
            return;
        }
        // Vanilla's repeat loop, verbatim: keep quick-moving while the slot
        // still holds the same item it held before the move.
        //
        // On a **result slot** this is what makes one shift-click craft a whole
        // stack — but only where something refills the result between
        // iterations, and that is the server. A client's `CraftingMenu` is
        // built with a null level access, so its `slotsChanged` never
        // recomputes the recipe and the result slot stays empty after the first
        // take; the loop then exits after exactly one craft. That is precisely
        // what vanilla's client predicts too. The server runs this same loop
        // over a menu that *does* refill, crafts until the grid runs out, and
        // pushes the difference back as `container_set_slot`s, which
        // `ClientMenu::reconcile` folds in. Predicting more than one craft here
        // would mean matching the recipe locally — a guess overwriting the one
        // slot the server owns outright.
        while let Some(template) = self.quick_move_with_furnace_input_items(index, furnace_input_items) {
            match self.slot_item(index) {
                Some(current) if ItemStack::is_same_item(current, &template) => continue,
                _ => break,
            }
        }
    }

    fn do_pickup(&mut self, slot_index: i32, action: ClickAction) {
        let Ok(index) = usize::try_from(slot_index) else {
            return;
        };
        let clicked = self.slot_item_cloned(index);
        let carried = self.carried().cloned();

        match (clicked, carried) {
            // Empty slot: place from cursor.
            (None, Some(carried)) => {
                let amount = match action {
                    ClickAction::Primary => carried.count(),
                    ClickAction::Secondary => 1,
                };
                let leftover = self.safe_insert(index, carried, amount);
                self.set_carried(leftover);
            }
            (None, None) => {}
            // Occupied slot, empty cursor: pick up.
            (Some(clicked), None) => {
                if !self.may_pickup(index) {
                    return;
                }
                let amount = match action {
                    ClickAction::Primary => clicked.count(),
                    ClickAction::Secondary => (clicked.count() + 1) / 2,
                };
                if let Some(taken) = self.try_remove(index, amount, i32::MAX) {
                    self.set_carried(Some(taken));
                    self.on_take(index);
                }
            }
            // Both occupied.
            (Some(clicked), Some(mut carried)) => {
                if !self.may_pickup(index) {
                    return;
                }
                if self.may_place(index, &carried) {
                    if ItemStack::is_same_item_same_components(&clicked, &carried) {
                        // Deposit onto the matching slot.
                        let amount = match action {
                            ClickAction::Primary => carried.count(),
                            ClickAction::Secondary => 1,
                        };
                        let leftover = self.safe_insert(index, carried, amount);
                        self.set_carried(leftover);
                    } else if carried.count() <= self.effective_max(index, &carried) {
                        // Swap cursor and slot.
                        self.set_slot_item(index, Some(carried));
                        self.set_carried(Some(clicked));
                    }
                } else if ItemStack::is_same_item_same_components(&clicked, &carried) {
                    // Slot rejects placement but same item: pull into cursor.
                    // This is *the* result-slot path — left-clicking the output
                    // while already holding a stack of the same result — so the
                    // take hook here is what crafts the second and later items.
                    let room = carried.max_stack_size() - carried.count();
                    if let Some(taken) = self.try_remove(index, clicked.count(), room) {
                        carried.grow(taken.count());
                        self.set_carried(Some(carried));
                        self.on_take(index);
                    }
                }
            }
        }
    }

    fn do_swap(&mut self, slot_index: i32, button: i32, ctx: PlayerCtx, outcome: &mut ClickOutcome) {
        let Ok(index) = usize::try_from(slot_index) else {
            return;
        };
        let native = usize::try_from(button).unwrap_or(0);
        let source = self.player_native(native).cloned();
        let target = self.slot_item_cloned(index);

        if source.is_none() && target.is_none() {
            return;
        }

        match (source, target) {
            (None, Some(target)) => {
                if self.may_pickup(index) {
                    self.set_player_native(native, Some(target));
                    self.set_slot_item(index, None);
                    self.on_take(index);
                }
            }
            (Some(mut source), None) => {
                if self.may_place(index, &source) {
                    let cap = self.effective_max(index, &source);
                    if source.count() > cap {
                        let placed = source.split(cap);
                        self.set_slot_item(index, Some(placed));
                        self.set_player_native(native, crate::item::normalize(source));
                    } else {
                        self.set_player_native(native, None);
                        self.set_slot_item(index, Some(source));
                    }
                }
            }
            (Some(mut source), Some(target)) => {
                if self.may_pickup(index) && self.may_place(index, &source) {
                    let cap = self.effective_max(index, &source);
                    if source.count() > cap {
                        let placed = source.split(cap);
                        self.set_slot_item(index, Some(placed));
                        // Vanilla's `source` here is the *live* object backing
                        // its own inventory-item lookup — vanilla's own stack-split
                        // step calls `this.shrink(...)`,
                        // mutating it in place, so by the time
                        // its own inventory-add step runs
                        // the native slot
                        // already shows the reduced remainder and can absorb the
                        // overflow via a same-slot merge. Our `ItemStack` isn't
                        // aliased, so the native slot must be written back
                        // *before* `give_to_player` scans it, or the overflow
                        // lands in the first empty slot instead of merging back
                        // into the slot it just came from.
                        self.set_player_native(native, crate::item::normalize(source));
                        // Overflow target back into inventory or drop it.
                        if !self.give_to_player(target.clone(), ctx.selected_hotbar_slot) {
                            outcome.dropped.push(target);
                        }
                        self.on_take(index);
                    } else {
                        self.set_player_native(native, Some(target));
                        self.set_slot_item(index, Some(source));
                        self.on_take(index);
                    }
                }
            }
            (None, None) => unreachable!(),
        }
    }

    fn do_clone(&mut self, slot_index: i32) {
        let Ok(index) = usize::try_from(slot_index) else {
            return;
        };
        if let Some(item) = self.slot_item_cloned(index) {
            let mut clone = item;
            clone.set_count(clone.max_stack_size());
            self.set_carried(Some(clone));
        }
    }

    fn do_throw(
        &mut self,
        slot_index: i32,
        button: i32,
        ctx: PlayerCtx,
        outcome: &mut ClickOutcome,
    ) {
        let Ok(index) = usize::try_from(slot_index) else {
            return;
        };
        if !ctx.can_drop {
            return;
        }
        let drop_whole = button == 1;
        let amount = if button == 0 {
            1
        } else {
            self.slot_item(index).map_or(0, ItemStack::count)
        };
        if let Some(taken) = self.try_remove(index, amount, i32::MAX) {
            outcome.dropped.push(taken);
            // `Slot.safeTake` = `tryRemove` + `onTake`; dropping the result of a
            // craft with `Q` consumes the grid exactly like picking it up does.
            self.on_take(index);
        }
        if drop_whole {
            // Drop-stack (button 1) empties the slot in one go; already handled
            // because amount was the full count.
        }
    }

    fn do_pickup_all(&mut self, slot_index: i32, button: i32) {
        let Ok(index) = usize::try_from(slot_index) else {
            return;
        };
        let Some(mut carried) = self.carried().cloned() else {
            return;
        };
        let slot_blocks = self
            .slot_item(index)
            .is_some_and(|_| self.may_pickup(index));
        if slot_blocks {
            return;
        }
        let count = self.slot_count();
        let backwards = button != 0;
        for pass in 0..2 {
            let indices: Vec<usize> = if backwards {
                (0..count).rev().collect()
            } else {
                (0..count).collect()
            };
            for i in indices {
                if carried.count() >= carried.max_stack_size() {
                    break;
                }
                let Some(target) = self.slot_item_cloned(i) else {
                    continue;
                };
                if !self.may_pickup(i)
                    || !self.can_take_for_pick_all(i)
                    || !ItemStack::is_same_item_same_components(&carried, &target)
                {
                    continue;
                }
                let is_full = target.count() == target.max_stack_size();
                if pass == 0 && is_full {
                    continue;
                }
                let room = carried.max_stack_size() - carried.count();
                if let Some(removed) = self.try_remove(i, target.count(), room) {
                    carried.grow(removed.count());
                }
            }
        }
        self.set_carried(Some(carried));
    }

    /// Vanilla `AbstractContainerMenu.canTakeItemForPickAll`: `true` by default,
    /// but **every** result-bearing menu overrides it to exclude its own result
    /// container — `CraftingMenu`, `InventoryMenu`, `SmithingMenu`,
    /// `StonecutterMenu` and `CartographyTableMenu` all carry the identical
    /// `target.container != this.resultSlots` line.
    ///
    /// Without it a double-click gather in a crafting screen vacuums the result
    /// slot along with everything else, which would craft an item the player
    /// never asked for — and since `on_take` now charges the grid for a take,
    /// it would silently eat the ingredients too.
    fn can_take_for_pick_all(&self, menu_index: usize) -> bool {
        self.craft_layout()
            .is_none_or(|layout| menu_index != layout.result_slot)
    }

    // --- Drag (quick-craft) ---

    fn do_quick_craft(
        &mut self,
        slot_index: i32,
        button: i32,
        ctx: PlayerCtx,
        outcome: &mut ClickOutcome,
    ) {
        let expected = self.quick_craft_status();
        let header = quick_craft_header(button);
        self.set_quick_craft_status(header);

        // A header that does not advance the expected sequence resets the drag,
        // except the start→end shortcut that vanilla tolerates.
        if (expected != drag_header::ADD || header != drag_header::END) && expected != header {
            self.reset_quick_craft();
            return;
        }
        if self.carried().is_none() {
            self.reset_quick_craft();
            return;
        }

        match header {
            drag_header::START => {
                let Some(kind) = QuickCraftType::from_raw(quick_craft_type(button)) else {
                    self.reset_quick_craft();
                    return;
                };
                if kind.allowed_for(ctx.infinite_materials) {
                    self.set_quick_craft_status(drag_header::ADD);
                    self.set_quick_craft_type(kind);
                    // slots already cleared by a prior reset/new drag.
                } else {
                    self.reset_quick_craft();
                }
            }
            drag_header::ADD => {
                if let Ok(index) = usize::try_from(slot_index) {
                    let carried = self.carried().cloned();
                    if let Some(carried) = carried
                        && self.can_drag_place(index, &carried)
                    {
                        self.push_quick_craft_slot(index);
                    }
                }
            }
            drag_header::END => {
                self.finish_quick_craft(ctx, outcome);
                self.reset_quick_craft();
            }
            _ => {
                self.reset_quick_craft();
            }
        }
    }

    /// Whether a slot is a legal drag target for the current cursor, against this
    /// menu's own live drag state. Thin wrapper over
    /// [`can_drag_place_at`](Self::can_drag_place_at) — the `ADD` arm's view of it.
    fn can_drag_place(&self, index: usize, carried: &ItemStack) -> bool {
        self.can_drag_place_at_typed(
            index,
            carried,
            self.quick_craft_type(),
            self.quick_craft_slots().len() as i32,
        )
    }

    /// Whether a drag of `kind` holding `carried` may **paint** `index` when
    /// `painted_count` slots are already painted: the cell is empty or holds the
    /// same item with room, the slot accepts the item, and the cursor still has
    /// more items than cells painted so far (a creative `CLONE` drag ignores that
    /// last part).
    ///
    /// Vanilla writes these three conditions twice — server-side in its own
    /// click-handler's
    /// `ADD` arm and client-side in
    /// its own should-add-slot-to-quick-craft step — and it
    /// matters a great deal that they agree, because **the screen's paint set is
    /// the divisor for the preview and the menu's is the divisor for the
    /// distribution** (see [`quick_craft_plan`](Self::quick_craft_plan)). Two
    /// copies that drifted would show the player a split the release does not
    /// produce.
    ///
    /// So there is one copy, here, and `MenuInput::dragged` in the shell calls it
    /// rather than restating it. That makes the two sets equal by construction
    /// instead of by argument.
    ///
    /// The raw `kind` is retained for the shell boundary; an unknown value is
    /// rejected before the predicate is evaluated.
    #[must_use]
    pub fn can_drag_place_at(
        &self,
        index: usize,
        carried: &ItemStack,
        kind: i32,
        painted_count: i32,
    ) -> bool {
        let Some(kind) = QuickCraftType::from_raw(kind) else {
            return false;
        };
        self.can_drag_place_at_typed(index, carried, kind, painted_count)
    }

    /// Typed implementation of [`Self::can_drag_place_at`].
    fn can_drag_place_at_typed(
        &self,
        index: usize,
        carried: &ItemStack,
        kind: QuickCraftType,
        painted_count: i32,
    ) -> bool {
        let enough = matches!(kind, QuickCraftType::Clone) || carried.count() > painted_count;
        can_item_quick_replace(self.slot_item(index), carried, true)
            && self.may_place(index, carried)
            && enough
    }

    fn finish_quick_craft(&mut self, ctx: PlayerCtx, outcome: &mut ClickOutcome) {
        let painted = self.quick_craft_slots().to_vec();
        if painted.is_empty() {
            return;
        }
        // A single painted slot degrades to an ordinary pickup/place click,
        // exactly as vanilla re-dispatches it.
        if painted.len() == 1 {
            let only = painted[0];
            let kind = self.quick_craft_type();
            self.reset_quick_craft();
            self.do_click(only as i32, kind.raw(), ContainerInput::Pickup, ctx);
            let _ = outcome; // pickup produces no drops here
            return;
        }

        let Some(source) = self.carried().cloned() else {
            return;
        };
        let kind = self.quick_craft_type();
        // **The split is computed by `quick_craft_plan`, not here.** The on-screen
        // preview calls the same function against the same painted set, so a
        // preview that disagreed with the outcome would have to be a different
        // *input*, never a different formula. See its doc comment.
        let plan = self.quick_craft_plan_typed(&painted, kind, &source);
        let mut remaining = source.count();
        for cell in plan {
            remaining -= cell.count - cell.existing;
            let mut placed = source.clone();
            placed.set_count(cell.count);
            self.set_slot_item(cell.menu_index, Some(placed));
        }

        let mut leftover = source;
        leftover.set_count(remaining);
        self.set_carried(crate::item::normalize(leftover));
    }

    /// What a drag would put in each painted cell if it were released now.
    ///
    /// **This is the single source of the split arithmetic.** Both the release
    /// path ([`finish_quick_craft`](Self::finish_quick_craft), vanilla's
    /// own quick-craft commit step) and the container screen's live
    /// preview (vanilla's own extract-slot step) go
    /// through it. Vanilla itself has the formula written out twice, in those two
    /// places plus a third time in its own recalculate-quick-craft-remaining
    /// step;
    /// keeping one copy here is what makes "the preview equals the outcome" a
    /// property of the code rather than a thing a test hopes for.
    ///
    /// `painted` is the accumulated paint set in paint order, `kind` a
    /// [`drag_type`], `source` the carried stack. Cells the release would refuse
    /// ([`can_drag_place_end`](Self::can_drag_place_end)) are **absent** from the
    /// result rather than present with a zero count, mirroring vanilla's `continue`.
    /// An unknown raw `kind` returns an empty plan.
    ///
    /// # `painted` must be the *filtered* paint set
    ///
    /// `painted.len()` is the divisor for an even split — vanilla's
    /// `getQuickCraftPlaceCount(this.quickcraftSlots.size(), …)` (`:386`), the size
    /// of the set that survived `ADD`, not of everything the pointer crossed. A
    /// caller that passes an unfiltered set divides by too large a number and
    /// under-previews every cell.
    ///
    /// Both real callers satisfy this by construction and it is worth knowing why,
    /// because the two sets are built in different places:
    ///
    /// * the machine's set is grown one `ADD` at a time through
    ///   [`can_drag_place`](Self::can_drag_place);
    /// * the screen's set is grown by `MenuInput::dragged` through vanilla's
    ///   `shouldAddSlotToQuickCraft`.
    ///
    /// Those two predicates are the *same three conditions*, evaluated against the
    /// same menu, over sets grown in the same order — so they stay equal at every
    /// step by induction, which is what lets the screen preview a drag the machine
    /// has not been told about yet. `container.rs`'s
    /// `the_screens_paint_set_and_the_machines_stay_identical` asserts the equality
    /// rather than leaving it as an argument.
    ///
    /// Reading every cell up front is equivalent to vanilla's interleaved loop
    /// because that loop never mutates what it reads: it re-reads `getCarried()`
    /// per iteration (`:378`) but only calls `setCarried` *after* the loop
    /// (`:393`), and the painted set is deduplicated
    /// ([`push_quick_craft_slot`](Self::push_quick_craft_slot)) so no two
    /// iterations touch the same slot.
    #[must_use]
    pub fn quick_craft_plan(
        &self,
        painted: &[usize],
        kind: i32,
        source: &ItemStack,
    ) -> Vec<QuickCraftCell> {
        let Some(kind) = QuickCraftType::from_raw(kind) else {
            return Vec::new();
        };
        self.quick_craft_plan_typed(painted, kind, source)
    }

    /// Typed implementation of [`Self::quick_craft_plan`].
    fn quick_craft_plan_typed(
        &self,
        painted: &[usize],
        kind: QuickCraftType,
        source: &ItemStack,
    ) -> Vec<QuickCraftCell> {
        let painted_count = painted.len() as i32;
        painted
            .iter()
            .copied()
            .filter(|&index| self.can_drag_place_end(index, source, painted_count, kind))
            .map(|index| {
                let existing = self.slot_item(index).map_or(0, ItemStack::count);
                let slot_cap = self.effective_max(index, source);
                let max_size = source.max_stack_size().min(slot_cap);
                let want = quick_craft_place_count_typed(painted_count, kind, source) + existing;
                QuickCraftCell {
                    menu_index: index,
                    count: want.min(max_size),
                    existing,
                    clamped: want > max_size,
                }
            })
            .collect()
    }

    /// What the cursor would be left holding if the drag painted onto `painted`
    /// were released now — vanilla's `quickCraftingRemainder`
    /// (`AbstractContainerScreen.recalculateQuickCraftRemaining`, `:248-267`).
    ///
    /// Derived from [`quick_craft_plan`](Self::quick_craft_plan) rather than
    /// recomputed, for the same reason: vanilla's third copy of the formula lives
    /// here, and a remainder that disagreed with the per-cell counts would show
    /// the player a cursor total that does not add up.
    ///
    /// Note the `CLONE` short-circuit: a creative stack-per-slot drag leaves the
    /// cursor at a full stack regardless of what it distributes (`:251-252`), so
    /// it is not the sum of anything.
    /// An unknown raw `kind` returns zero rather than entering a distribution arm.
    #[must_use]
    pub fn quick_craft_remainder(&self, painted: &[usize], kind: i32, source: &ItemStack) -> i32 {
        let Some(kind) = QuickCraftType::from_raw(kind) else {
            return 0;
        };
        self.quick_craft_remainder_typed(painted, kind, source)
    }

    /// Typed implementation of [`Self::quick_craft_remainder`].
    fn quick_craft_remainder_typed(
        &self,
        painted: &[usize],
        kind: QuickCraftType,
        source: &ItemStack,
    ) -> i32 {
        if matches!(kind, QuickCraftType::Clone) {
            return source.max_stack_size();
        }
        self.quick_craft_plan_typed(painted, kind, source)
            .iter()
            .fold(source.count(), |acc, cell| acc - (cell.count - cell.existing))
    }

    /// Whether the drag's **end** would fill `index`, mirroring
    /// vanilla's own quick-craft commit step. Note `>=` here where
    /// [`can_drag_place`](Self::can_drag_place)'s paint-time check has `>`: at
    /// paint time the slot is not yet in the set, at end time it is.
    ///
    /// `kind` is passed in rather than read from
    /// [`quick_craft_type`](Self::quick_craft_type), and that is load-bearing for
    /// the **preview** caller. The screen previews a drag the machine has not
    /// been told about yet — nothing is sent until the mouse is released — so
    /// mid-drag the menu's stored type is whatever the *previous* drag left
    /// (`reset_quick_craft` deliberately does not clear it) or `0` on a fresh
    /// menu. Reading it here made a creative `CLONE` preview evaluate its
    /// `enough` arm as `EVEN` and refuse every cell: the plan came back empty
    /// where the release filled five. Caught by
    /// `tests/drag_preview_agreement.rs`, which is the whole reason that file
    /// compares against the real release rather than against a second formula.
    pub(crate) fn can_drag_place_end(
        &self,
        index: usize,
        carried: &ItemStack,
        painted_count: i32,
        kind: QuickCraftType,
    ) -> bool {
        let enough = matches!(kind, QuickCraftType::Clone) || carried.count() >= painted_count;
        can_item_quick_replace(self.slot_item(index), carried, true)
            && self.may_place(index, carried)
            && enough
    }

    // --- Slot primitives (Slot.safeInsert / tryRemove) ---

    /// Inserts up to `increment` items from `stack` into `menu_index`, returning
    /// the leftover cursor. Mirrors vanilla `Slot.safeInsert`.
    fn safe_insert(
        &mut self,
        menu_index: usize,
        mut stack: ItemStack,
        increment: i32,
    ) -> Option<ItemStack> {
        if stack.is_empty() || !self.may_place(menu_index, &stack) {
            return crate::item::normalize(stack);
        }
        let existing = self.slot_item_cloned(menu_index);
        let existing_count = existing.as_ref().map_or(0, ItemStack::count);
        let cap = self.effective_max(menu_index, &stack);
        let to_insert = increment.min(stack.count()).min(cap - existing_count);
        if to_insert <= 0 {
            return crate::item::normalize(stack);
        }
        match existing {
            None => {
                let placed = stack.split(to_insert);
                self.set_slot_item(menu_index, Some(placed));
            }
            Some(mut existing) if ItemStack::is_same_item_same_components(&existing, &stack) => {
                stack.shrink(to_insert);
                existing.grow(to_insert);
                self.set_slot_item(menu_index, Some(existing));
            }
            Some(_) => {}
        }
        crate::item::normalize(stack)
    }

    /// Removes up to `min(amount, max_take)` items from `menu_index`, honouring
    /// the slot's modification rules. Mirrors vanilla `Slot.tryRemove`.
    fn try_remove(&mut self, menu_index: usize, amount: i32, max_take: i32) -> Option<ItemStack> {
        if !self.may_pickup(menu_index) {
            return None;
        }
        let current = self.slot_item_cloned(menu_index)?;
        // allowModification = mayPickup && mayPlace(item); when false, a partial
        // take (max_take < count) is refused.
        let allow_modification = self.may_place(menu_index, &current);
        if !allow_modification && max_take < current.count() {
            return None;
        }
        let take = amount.min(max_take);
        if take <= 0 {
            return None;
        }
        let mut current = current;
        let removed = current.split(take);
        if removed.is_empty() {
            return None;
        }
        self.set_slot_item(menu_index, crate::item::normalize(current));
        Some(removed)
    }

    /// Attempts to merge a stack back into the player inventory (main+hotbar+
    /// off-hand), returning whether it was fully absorbed. Used by swap
    /// overflow.
    ///
    /// Mirrors vanilla's own inventory add/add-resource/slot-with-remaining-space/
    /// free-slot steps. Each iteration picks
    /// **one** target slot via vanilla's own slot-with-remaining-space priority — the
    /// *selected* hotbar slot first, then the off-hand (native
    /// [`OFFHAND_NATIVE`]), then a merge-only linear scan across natives
    /// `0..36` — and only when *none* of those already hold a mergeable stack
    /// does it fall back to vanilla's own free-slot search, the first **empty** slot in `0..36`.
    /// The off-hand is never used as an empty-slot fallback: vanilla's
    /// own free-slot search scans only its own item list, which it sizes at exactly 36;
    /// slot 40 only ever participates via the merge
    /// check. The loop re-derives the target from scratch every pass (as
    /// vanilla's `add` does by calling `addResource` repeatedly) so a stack
    /// bigger than one cap can spread across several slots.
    fn give_to_player(&mut self, mut stack: ItemStack, selected_hotbar_slot: usize) -> bool {
        loop {
            if stack.is_empty() {
                return true;
            }
            let merge_target = self
                .mergeable_native(selected_hotbar_slot, &stack)
                .or_else(|| self.mergeable_native(OFFHAND_NATIVE, &stack))
                .or_else(|| (0..36).find(|&i| self.mergeable_native(i, &stack).is_some()));

            let target = match merge_target {
                Some(i) => i,
                None => match (0..36).find(|&i| self.player_native(i).is_none()) {
                    Some(i) => i,
                    // No merge target and no free slot: no progress possible.
                    None => return stack.is_empty(),
                },
            };

            let existing = self.player_native(target).cloned();
            let cap = existing
                .as_ref()
                .map_or_else(|| stack.max_stack_size(), ItemStack::max_stack_size);
            let existing_count = existing.as_ref().map_or(0, ItemStack::count);
            let give = (cap - existing_count).min(stack.count());
            if give <= 0 {
                return stack.is_empty();
            }
            match existing {
                Some(mut existing) => {
                    existing.grow(give);
                    stack.shrink(give);
                    self.set_player_native(target, Some(existing));
                }
                None => {
                    let placed = stack.split(give);
                    self.set_player_native(target, Some(placed));
                }
            }
        }
    }

    /// Returns `Some(native_index)` when the native slot already holds a
    /// same-item-same-components, stackable stack with spare room for `stack`.
    /// Mirrors vanilla `Inventory.hasRemainingSpaceForItem` (`Inventory.
    /// java:95-100`), which is what `getSlotWithRemainingSpace` calls at each
    /// candidate slot.
    fn mergeable_native(&self, native_index: usize, stack: &ItemStack) -> Option<usize> {
        let existing = self.player_native(native_index)?;
        (ItemStack::is_same_item_same_components(existing, stack)
            && existing.is_stackable()
            && existing.count() < existing.max_stack_size())
        .then_some(native_index)
    }

    /// Runs a full drag as a start/add.../end packet sequence and applies it.
    ///
    /// `kind` is one of [`drag_type`]; `slots` are the painted menu indices.
    /// Unknown raw kinds are rejected without starting a packet sequence.
    pub fn perform_drag(&mut self, kind: i32, slots: &[usize], ctx: PlayerCtx) -> ClickOutcome {
        let Some(kind) = QuickCraftType::from_raw(kind) else {
            return ClickOutcome::default();
        };
        self.perform_drag_typed(kind, slots, ctx)
    }

    /// Runs a validated drag as a start/add…/end packet sequence.
    fn perform_drag_typed(
        &mut self,
        kind: QuickCraftType,
        slots: &[usize],
        ctx: PlayerCtx,
    ) -> ClickOutcome {
        let mut outcome = self.do_click(
            OUTSIDE_SLOT,
            quick_craft_mask_typed(drag_header::START, kind),
            ContainerInput::QuickCraft,
            ctx.clone(),
        );
        for &slot in slots {
            let add = self.do_click(
                slot as i32,
                quick_craft_mask_typed(drag_header::ADD, kind),
                ContainerInput::QuickCraft,
                ctx.clone(),
            );
            outcome.dropped.extend(add.dropped);
        }
        let end = self.do_click(
            OUTSIDE_SLOT,
            quick_craft_mask_typed(drag_header::END, kind),
            ContainerInput::QuickCraft,
            ctx,
        );
        outcome.dropped.extend(end.dropped);
        outcome
    }
}

/// Returns whether `type` is legal for a player: even and one always are; clone
/// requires infinite materials.
#[must_use]
pub fn is_valid_quick_craft_type(kind: i32, infinite_materials: bool) -> bool {
    QuickCraftType::from_raw(kind).is_some_and(|kind| kind.allowed_for(infinite_materials))
}

/// Per-slot amount a drag places, before adding what is already there. Mirrors
/// vanilla `getQuickCraftPlaceCount`.
#[must_use]
pub fn quick_craft_place_count(slots: i32, kind: i32, stack: &ItemStack) -> i32 {
    let Some(kind) = QuickCraftType::from_raw(kind) else {
        return 0;
    };
    quick_craft_place_count_typed(slots, kind, stack)
}

/// Typed implementation of [`quick_craft_place_count`].
fn quick_craft_place_count_typed(slots: i32, kind: QuickCraftType, stack: &ItemStack) -> i32 {
    match kind {
        QuickCraftType::Even => {
            if slots <= 0 {
                0
            } else {
                stack.count() / slots
            }
        }
        QuickCraftType::One => 1,
        QuickCraftType::Clone => stack.max_stack_size(),
    }
}

/// Packs a validated distribution mode and header into a click button.
fn quick_craft_mask_typed(header: i32, kind: QuickCraftType) -> i32 {
    (header & 3) | ((kind.raw() & 3) << 2)
}

/// Whether `stack` may be quick-replaced into `slot` — empty, or the same item
/// with room. Mirrors vanilla `canItemQuickReplace`.
#[must_use]
pub fn can_item_quick_replace(
    slot: Option<&ItemStack>,
    stack: &ItemStack,
    ignore_size: bool,
) -> bool {
    match slot {
        None => true,
        Some(existing) => {
            if ItemStack::is_same_item_same_components(stack, existing) {
                let extra = if ignore_size { 0 } else { stack.count() };
                existing.count() + extra <= stack.max_stack_size()
            } else {
                false
            }
        }
    }
}

/// An ergonomic, named click. Each constructor maps to a raw
/// [`Menu::do_click`] call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Click {
    /// Menu slot index, or [`OUTSIDE_SLOT`].
    pub slot: i32,
    /// Raw button number.
    pub button: i32,
    /// Click mode.
    pub input: ContainerInput,
}

impl Click {
    /// Left-click a slot (pick up whole / place whole / swap).
    #[must_use]
    pub fn left(slot: usize) -> Self {
        Self {
            slot: slot as i32,
            button: 0,
            input: ContainerInput::Pickup,
        }
    }

    /// Right-click a slot (pick up half / place one).
    #[must_use]
    pub fn right(slot: usize) -> Self {
        Self {
            slot: slot as i32,
            button: 1,
            input: ContainerInput::Pickup,
        }
    }

    /// Left-click outside the window (drop the whole cursor).
    #[must_use]
    pub fn drop_cursor() -> Self {
        Self {
            slot: OUTSIDE_SLOT,
            button: 0,
            input: ContainerInput::Pickup,
        }
    }

    /// Right-click outside the window (drop one from the cursor).
    #[must_use]
    pub fn drop_cursor_one() -> Self {
        Self {
            slot: OUTSIDE_SLOT,
            button: 1,
            input: ContainerInput::Pickup,
        }
    }

    /// Shift-click a slot (quick move).
    #[must_use]
    pub fn shift(slot: usize) -> Self {
        Self {
            slot: slot as i32,
            button: 0,
            input: ContainerInput::QuickMove,
        }
    }

    /// Number-key swap between a slot and hotbar index `hotbar` (0–8).
    #[must_use]
    pub fn hotbar_swap(slot: usize, hotbar: u8) -> Self {
        Self {
            slot: slot as i32,
            button: i32::from(hotbar),
            input: ContainerInput::Swap,
        }
    }

    /// Off-hand key swap (`F` by default) between a slot and the off-hand.
    #[must_use]
    pub fn offhand_swap(slot: usize) -> Self {
        Self {
            slot: slot as i32,
            button: 40,
            input: ContainerInput::Swap,
        }
    }

    /// Middle-click clone (creative only).
    #[must_use]
    pub fn clone_slot(slot: usize) -> Self {
        Self {
            slot: slot as i32,
            button: 0,
            input: ContainerInput::Clone,
        }
    }

    /// Drop one item from a slot (`Q`).
    #[must_use]
    pub fn drop_one(slot: usize) -> Self {
        Self {
            slot: slot as i32,
            button: 0,
            input: ContainerInput::Throw,
        }
    }

    /// Drop a whole stack from a slot (`Ctrl+Q`).
    #[must_use]
    pub fn drop_stack(slot: usize) -> Self {
        Self {
            slot: slot as i32,
            button: 1,
            input: ContainerInput::Throw,
        }
    }

    /// Double-click a slot to gather matching items.
    #[must_use]
    pub fn double(slot: usize) -> Self {
        Self {
            slot: slot as i32,
            button: 0,
            input: ContainerInput::PickupAll,
        }
    }

    /// Applies this click to a menu.
    pub fn apply(self, menu: &mut Menu, ctx: PlayerCtx) -> ClickOutcome {
        menu.do_click(self.slot, self.button, self.input, ctx)
    }
}
