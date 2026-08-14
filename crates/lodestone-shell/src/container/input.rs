//! The GUI-side press/drag/release protocol: mouse and key gestures turned into
//! the [`Click`]s `Menus::click` expects.
//!
//! Split out of `container.rs` verbatim.

use lodestone_game::click::{Click, ContainerInput, drag_header, drag_type, quick_craft_mask};
use lodestone_game::item::ItemStack;
use lodestone_game::menu::{Menu, OUTSIDE_SLOT};

use super::layout::MenuHit;

/// Which mouse button a menu gesture used.
///
/// `Pick` is vanilla's `keyPickItem` (middle-click by default), which only does
/// anything with infinite materials.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuButton {
    /// Primary / left.
    Left,
    /// Secondary / right.
    Right,
    /// Pick-block (middle by default).
    Pick,
}

impl MenuButton {
    /// The raw button number vanilla puts in the packet for an ordinary click.
    fn number(self) -> i32 {
        match self {
            Self::Left | Self::Pick => 0,
            Self::Right => 1,
        }
    }

    /// The drag distribution type this button paints with
    /// ([`drag_type`](lodestone_game::click::drag_type)).
    fn drag_kind(self) -> i32 {
        match self {
            Self::Left => drag_type::EVEN,
            Self::Right => drag_type::ONE,
            Self::Pick => drag_type::CLONE,
        }
    }
}

/// A **keyboard** action an open container screen turns into a click, from
/// vanilla `AbstractContainerScreen.keyPressed` (`:495-501`).
///
/// The hotbar/off-hand `SWAP` half of that method (`checkHotbarKeyPressed`,
/// `:506-522`) is deliberately *not* here: it already goes out through
/// `app.rs`'s `KeyOutcome::ContainerSwap` (commit `43692c5`) with vanilla's own
/// two state guards. What was missing is everything below that call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKey {
    /// `options.keyPickItem` — middle-click's keyboard twin.
    PickItem,
    /// `options.keyDrop` (`Q` by default). `ctrl` selects drop-**stack** over
    /// drop-one, which is the *only* thing the modifier changes.
    Drop {
        /// Whether Control was held (`event.hasControlDown()`).
        ctrl: bool,
    },
}

/// What the caller must tell the input machine about the menu at gesture time.
#[derive(Debug, Clone, Copy)]
pub struct MenuContext {
    /// Whether the cursor (carried stack) currently holds something. Read it off
    /// the *predicted* menu: `menu.carried().is_some()`.
    pub cursor_loaded: bool,
    /// Whether the player has infinite materials (creative), which enables
    /// pick-block cloning and the stack-per-slot drag type.
    pub creative: bool,
}

/// The GUI-side press/drag/release protocol, turning mouse events into the
/// [`Click`]s `Menus::click` expects.
///
/// This is the piece between [`hit_test`] and
/// [`Menus::click`](lodestone_game::menus::Menus::click), and it exists as a state
/// machine rather than a `fn(hit) -> Click` because **vanilla does not send a
/// click on mouse-down when the cursor is loaded**. Read
/// `AbstractContainerScreen.mouseClicked`: with a non-empty carried stack it only
/// sets `isQuickCrafting` and sends *nothing*; the packet goes out on
/// `mouseReleased`, as either a plain `PICKUP` (if the mouse never moved onto a
/// slot) or the `QUICK_CRAFT` start/add…/end sequence (if it did). A naive
/// press-to-`PICKUP` mapper looks right for every single-slot interaction and
/// silently loses the entire paint-drag gesture — the "distribute one item per
/// slot" right-drag most players use to fill a crafting grid.
///
/// The empty-cursor half *is* sent on press (`PICKUP` / `QUICK_MOVE` / `CLONE`),
/// and vanilla's `skipNextRelease` then suppresses the release, which is what
/// [`skip_next_release`](Self::press) models.
///
/// Ordering contract: [`press`](Self::press), then zero or more
/// [`dragged`](Self::dragged), then [`release`](Self::release). `dragged` never
/// emits — vanilla accumulates painted slots and sends the whole sequence from
/// `quickCraftToSlots` at release.
#[derive(Debug, Clone, Default)]
pub struct MenuInput {
    /// The button that armed a paint-drag, and the slots painted so far.
    drag: Option<(MenuButton, Vec<usize>)>,
    /// Set when the press already sent a click, so the release must not send one.
    skip_next_release: bool,
    /// Slot the previous press landed on, for double-click detection.
    last_slot: Option<usize>,
    /// The pending release should gather (`PICKUP_ALL`) instead.
    double_click: bool,
    /// Mirrors vanilla `AbstractContainerScreen.lastQuickMoved`: the stack
    /// held by the slot a `QUICK_MOVE` click was just sent for, or `None` for
    /// vanilla's `ItemStack.EMPTY`. Set at both the sites vanilla sets it —
    /// `:312` in [`press`](Self::press) and `:426` in [`release`](Self::release)
    /// — and read by the shift+double-click gather in `release`, which moves
    /// every slot matching *this* stack rather than gathering onto the
    /// cursor.
    last_quick_moved: Option<ItemStack>,
}

impl MenuInput {
    /// A fresh input machine with nothing armed.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a paint-drag is currently armed.
    ///
    /// This used to say "while this is true the screen should draw the drag
    /// preview rather than a hover highlight." **That is not what vanilla does.**
    /// `extractSlotHighlightBack`/`Front` (`AbstractContainerScreen.java:153-163`)
    /// are gated on `hoveredSlot != null && hoveredSlot.isHighlightable()` and on
    /// nothing else — not on `isQuickCrafting` — so the highlight and the drag
    /// preview are drawn *together* mid-drag, and `build_inner` does the same.
    /// The two are independent, and treating them as exclusive would have made
    /// the highlight vanish the moment the player picked anything up.
    #[must_use]
    pub fn is_dragging(&self) -> bool {
        self.drag.is_some()
    }

    /// Mouse-button press. Returns the clicks to send **now** (empty for the
    /// loaded-cursor case, which sends on release).
    ///
    /// `is_repeat` is the platform's double-click flag; combined with hitting the
    /// same slot twice it arms the gather that fires on release.
    ///
    /// `menu` is read only to capture `last_quick_moved` off the slot about to
    /// be quick-moved (vanilla `AbstractContainerScreen.java:312`) — it does
    /// not otherwise change what this method sends.
    pub fn press(
        &mut self,
        hit: MenuHit,
        button: MenuButton,
        shift: bool,
        ctx: MenuContext,
        is_repeat: bool,
        menu: &Menu,
    ) -> Vec<Click> {
        let cloning = button == MenuButton::Pick && ctx.creative;
        let slot_hit = match hit {
            MenuHit::Slot(i) => Some(i),
            _ => None,
        };
        self.double_click = is_repeat && slot_hit.is_some() && self.last_slot == slot_hit;
        self.last_slot = slot_hit;
        self.skip_next_release = false;
        self.drag = None;

        // A press with `Pick` and no infinite materials is vanilla's hotbar-rebind
        // path, which sends no container click at all.
        if button == MenuButton::Pick && !cloning {
            return Vec::new();
        }

        // Inside the panel but not over a slot: vanilla's `slotId` stays -1 and the
        // whole branch is skipped. Deliberately *not* a drop.
        let slot = match hit {
            MenuHit::Slot(i) => i as i32,
            MenuHit::Outside => OUTSIDE_SLOT,
            MenuHit::Panel => return Vec::new(),
        };

        if ctx.cursor_loaded {
            // Arm a paint-drag and send nothing; the release decides.
            self.drag = Some((button, Vec::new()));
            return Vec::new();
        }

        self.skip_next_release = true;
        // `quickKey` in vanilla: a shift-click on a real slot. Captured before
        // the `if` chain below because vanilla's own assignment
        // (`AbstractContainerScreen.java:312`) happens as a side effect of
        // computing this same condition, and `cloning` takes priority over it
        // there (the two are mutually exclusive `if`/`else` arms, not just
        // independent conditions).
        let quick_key = !cloning && shift && slot != OUTSIDE_SLOT;
        if quick_key {
            // An empty slot records vanilla's `ItemStack.EMPTY`, modelled here
            // as `None`.
            self.last_quick_moved = match hit {
                MenuHit::Slot(i) => menu.slot_item(i).cloned(),
                _ => None,
            };
        }
        let input = if cloning {
            ContainerInput::Clone
        } else if quick_key {
            ContainerInput::QuickMove
        } else if slot == OUTSIDE_SLOT {
            // Vanilla sends THROW at -999 here. The server no-ops it (its THROW
            // branch requires `slotIndex >= 0`), but sending what vanilla sends
            // keeps the packet stream identical rather than merely equivalent.
            ContainerInput::Throw
        } else {
            ContainerInput::Pickup
        };
        vec![Click {
            slot,
            button: button.number(),
            input,
        }]
    }

    /// The cursor moved to `hit` with the button still down. Records a painted
    /// slot; never emits.
    ///
    /// Mirrors vanilla `AbstractContainerScreen.mouseDragged` (`:361-370`),
    /// whose paint site is gated on `shouldAddSlotToQuickCraft` (`:554-561`):
    ///
    /// ```java
    /// return this.isQuickCrafting
    ///    && !carried.isEmpty()
    ///    && (carried.getCount() > this.quickCraftSlots.size() || this.quickCraftingType == 2)
    ///    && AbstractContainerMenu.canItemQuickReplace(slot, carried, true)
    ///    && slot.mayPlace(carried)
    ///    && this.menu.canDragTo(slot);
    /// ```
    ///
    /// # This filter is load-bearing, and its absence was that fix part 1
    ///
    /// This used to record **every** slot the pointer crossed, on the argument
    /// that filtering belongs to `Menu::do_click`'s own `can_drag_place`, which
    /// both sides run, so "painting liberally cannot desynchronise". The desync
    /// half of that was true. What it missed is that the *emptiness* of the
    /// painted set is what decides which packet the release sends at all:
    ///
    /// * painted non-empty → `QUICK_CRAFT` start/add…/end;
    /// * painted empty → a plain [`ContainerInput::Pickup`].
    ///
    /// So a click on a slot the drag may not paint — most visibly a **crafting
    /// result**, whose [`SlotKind::Output`](lodestone_game::container::SlotKind)
    /// fails `mayPlace` — recorded the slot here, sent the drag sequence, and
    /// the machine then dropped the `ADD` at `can_drag_place` and committed
    /// nothing at `END`. The plain `PICKUP` that vanilla would have sent, and
    /// with it `Menu::do_pickup`'s cursor-merge arm (vanilla
    /// `AbstractContainerMenu`'s matching arm), never went out. The reported
    /// symptom was "taking from a crafting output onto a matching cursor does
    /// nothing"; the arm that does the merge was present and correct the whole
    /// time, one layer below the one that was broken.
    ///
    /// Note this needs the mouse to have moved at least once during the click,
    /// which is why it read as intermittent from play rather than absolute.
    ///
    /// `canDragTo` is `true` for every menu this client models (vanilla
    /// overrides it only in `HorseInventoryMenu`), so it is not restated here.
    pub fn dragged(&mut self, hit: MenuHit, menu: &Menu) {
        let MenuHit::Slot(i) = hit else {
            return;
        };
        let Some((button, slots)) = self.drag.as_mut() else {
            return;
        };
        let Some(carried) = menu.carried() else {
            return;
        };
        // All three conditions come from `Menu::can_drag_place_at` — the *same*
        // function the machine's own `ADD` arm uses — rather than being restated
        // here. That is what makes the screen's paint set and the machine's
        // provably identical, which the drag preview depends on: the screen's set
        // size is the divisor for the previewed split and the machine's is the
        // divisor for the real distribution. See that method's doc comment.
        if !menu.can_drag_place_at(i, carried, button.drag_kind(), slots.len() as i32) {
            return;
        }
        if !slots.contains(&i) {
            slots.push(i);
        }
    }

    /// The in-progress paint, for the on-screen preview: the drag type
    /// ([`drag_type`]) and the slots painted so far, in paint order.
    ///
    /// This is vanilla's `quickCraftSlots` / `quickCraftingType` pair, read by
    /// `AbstractContainerScreen.extractSlot` (`:202-222`) to draw the provisional
    /// per-cell stack. `None` when no drag is armed.
    #[must_use]
    pub fn drag_paint(&self) -> Option<(i32, &[usize])> {
        self.drag
            .as_ref()
            .map(|(button, slots)| (button.drag_kind(), slots.as_slice()))
    }

    /// Mouse-button release. Returns the clicks to send.
    ///
    /// `menu` gates the double-click gather branch and (for the shift variant)
    /// supplies the slots to sweep — see [`gather_shift_matches`](Self::gather_shift_matches)
    /// — and also captures `last_quick_moved` for the plain shift-click path,
    /// at the second of the two sites vanilla sets it
    /// (`AbstractContainerScreen.java:426`; the first is [`press`](Self::press)).
    pub fn release(
        &mut self,
        hit: MenuHit,
        button: MenuButton,
        shift: bool,
        ctx: MenuContext,
        menu: &Menu,
    ) -> Vec<Click> {
        let drag = self.drag.take();
        let gather = std::mem::take(&mut self.double_click);
        let skip = std::mem::take(&mut self.skip_next_release);

        // A release on a different button than the one that armed the drag cancels
        // it outright (vanilla returns early and swallows the next release too).
        if drag.as_ref().is_some_and(|(armed, _)| *armed != button) {
            self.skip_next_release = true;
            return Vec::new();
        }

        if gather && button == MenuButton::Left {
            if let MenuHit::Slot(i) = hit {
                // `AbstractContainerScreen.java:387`: the whole gather branch
                // (both this and the shift variant below) is gated on
                // `menu.canTakeItemForPickAll(ItemStack.EMPTY, slot)`. Every
                // result-bearing menu overrides that to exclude its own
                // result container (`Menu::can_take_for_pick_all` in
                // lodestone-game — private, so recomputed here from what the
                // shell already has; its server-side effect is covered by
                // `pickup_all_never_drains_the_crafting_result` in
                // `lodestone-game`). This is **not** a desync fix: a real
                // server honours a PICKUP_ALL/QUICK_MOVE aimed at the result
                // slot regardless, since `Menu::do_click` has no such gate —
                // skipping the packet here only suppresses non-vanilla client
                // UX, matching double-clicking a crafting result silently
                // sending nothing, as it does in the real game.
                let allowed = menu.craft_layout().is_none_or(|l| i != l.result_slot);
                if allowed {
                    return if shift {
                        self.gather_shift_matches(menu, i)
                    } else {
                        vec![Click::double(i)]
                    };
                }
                // Not allowed: fall through to the ordinary release handling
                // below, exactly as vanilla's `if` failing falls into its
                // `else` — the gather is skipped, not replaced with nothing.
            }
        }
        if skip {
            return Vec::new();
        }

        let painted = drag.map(|(_, slots)| slots).unwrap_or_default();
        if !painted.is_empty() {
            let kind = button.drag_kind();
            let mut clicks = Vec::with_capacity(painted.len() + 2);
            clicks.push(quick_craft(OUTSIDE_SLOT, drag_header::START, kind));
            for i in painted {
                clicks.push(quick_craft(i as i32, drag_header::ADD, kind));
            }
            clicks.push(quick_craft(OUTSIDE_SLOT, drag_header::END, kind));
            return clicks;
        }

        if !ctx.cursor_loaded {
            return Vec::new();
        }
        let slot = match hit {
            MenuHit::Slot(i) => i as i32,
            MenuHit::Outside => OUTSIDE_SLOT,
            MenuHit::Panel => return Vec::new(),
        };
        let clone_click = button == MenuButton::Pick && ctx.creative;
        // `AbstractContainerScreen.java:426`: the second `lastQuickMoved`
        // site, inside the (non-clone) loaded-cursor release path — mirrored
        // in `press` for the empty-cursor press path.
        let quick_key = !clone_click && shift && slot != OUTSIDE_SLOT;
        if quick_key {
            self.last_quick_moved = match hit {
                MenuHit::Slot(i) => menu.slot_item(i).cloned(),
                _ => None,
            };
        }
        let input = if clone_click {
            ContainerInput::Clone
        } else if quick_key {
            ContainerInput::QuickMove
        } else {
            ContainerInput::Pickup
        };
        vec![Click {
            slot,
            button: button.number(),
            input,
        }]
    }

    /// A key was pressed while a container screen is open. Returns the clicks to
    /// send, which is empty for every key that is not one of [`MenuKey`]'s or
    /// whose state guard fails.
    ///
    /// # This closed an island, not a missing branch
    ///
    /// `Click::drop_one`/`Click::drop_stack` (`click.rs`), `do_throw` and its
    /// `can_drop` gate were all implemented and tested by that fix's audit —
    /// and had **zero producers anywhere outside `crates/protocol/`**, the exact
    /// shape of `ClientAction::SetFlying`. `ContainerInput::Throw` was reachable
    /// only at [`OUTSIDE_SLOT`] (releasing a loaded cursor off the panel), which
    /// `Menu::do_click`'s own `slotIndex >= 0` guard makes a no-op — so the
    /// machine's whole `THROW`-from-a-slot branch could not run in the real
    /// game. `Q` inside an inventory did nothing.
    ///
    /// Vanilla, `AbstractContainerScreen.keyPressed` (`:495-501`):
    ///
    /// ```java
    /// if (this.hoveredSlot != null && this.hoveredSlot.hasItem()) {
    ///    if (this.minecraft.options.keyPickItem.matches(event)) {
    ///       this.slotClicked(this.hoveredSlot, this.hoveredSlot.index, 0, ContainerInput.CLONE);
    ///    } else if (this.minecraft.options.keyDrop.matches(event)) {
    ///       this.slotClicked(this.hoveredSlot, this.hoveredSlot.index, event.hasControlDown() ? 1 : 0, ContainerInput.THROW);
    ///    }
    /// }
    /// ```
    ///
    /// Three things about that are easy to get wrong, and all three are
    /// transcribed rather than reasoned about:
    ///
    /// * **The gate is "the slot has an item", not "the cursor is empty."**
    ///   Unlike `checkHotbarKeyPressed` (`:507`), this branch does not consult
    ///   the carried stack at all — `doClick`'s own `getCarried().isEmpty()`
    ///   guard (`AbstractContainerMenu.java:513`) is what makes a loaded-cursor
    ///   `THROW` a no-op, one layer down. Adding a `cursor_loaded` check here
    ///   would suppress a packet vanilla sends, which is a desync in the
    ///   direction nothing corrects: the server would never see it.
    /// * **`PickItem` is *not* gated on infinite materials here**, where
    ///   [`press`](Self::press) gates its middle-click equivalent on
    ///   `ctx.creative`. The two are not inconsistent: `mouseClicked` (`:285`)
    ///   uses `hasInfiniteMaterials` to decide *which mouse button means clone*,
    ///   while the permission itself lives in `doClick`'s CLONE arm
    ///   (`AbstractContainerMenu.java:508`, `&& player.hasInfiniteMaterials()`).
    ///   A key has no such ambiguity, so vanilla sends it in survival too and
    ///   lets the menu drop it.
    /// * **`else if`, not two `if`s.** A key bound to both actions clones only.
    ///
    /// `ctx` is taken for symmetry with the mouse entry points and is currently
    /// unread, which is the point of the two bullets above; taking it keeps the
    /// signature stable if a future vanilla version adds a state guard here.
    pub fn key_pressed(
        &self,
        hit: MenuHit,
        key: MenuKey,
        _ctx: MenuContext,
        menu: &Menu,
    ) -> Vec<Click> {
        let MenuHit::Slot(i) = hit else {
            return Vec::new();
        };
        // `hoveredSlot.hasItem()`. An empty slot is not a drop target and not a
        // clone source, so both arms are skipped rather than sent-and-ignored.
        if menu.slot_item(i).is_none() {
            return Vec::new();
        }
        vec![match key {
            MenuKey::PickItem => Click::clone_slot(i),
            MenuKey::Drop { ctrl: false } => Click::drop_one(i),
            MenuKey::Drop { ctrl: true } => Click::drop_stack(i),
        }]
    }

    /// `AbstractContainerScreen.java:388-398`: shift+double-click does not
    /// gather onto the cursor — it sends one `QUICK_MOVE` per slot that is in
    /// the **same backing container** as the double-clicked slot, may be
    /// picked up, is non-empty, and matches `last_quick_moved`
    /// (`target.mayPickup(player) && target.hasItem() && target.container ==
    /// slot.container && canItemQuickReplace(target, lastQuickMoved, true)`).
    ///
    /// `target.container == slot.container` compares the **backing
    /// container** (`Slot::container`, an index into `Menu`'s container
    /// list), not the menu — getting this wrong would let a shift+double-click
    /// in a chest sweep the player's own inventory, or vice versa, since both
    /// live in the same `Menu`.
    ///
    /// `canItemQuickReplace(target, lastQuickMoved, true)` is called here only
    /// once `target.hasItem()` is already known true, at which point its
    /// `ignoreSize` argument (`true`) drops the remaining size check
    /// entirely, so it reduces to `isSameItemSameComponents(lastQuickMoved,
    /// target.getItem())`.
    fn gather_shift_matches(&self, menu: &Menu, origin: usize) -> Vec<Click> {
        let Some(last) = self.last_quick_moved.as_ref() else {
            return Vec::new();
        };
        let Some(origin_container) = menu.slot(origin).map(|s| s.container) else {
            return Vec::new();
        };
        let mut clicks = Vec::new();
        for target in 0..menu.slot_count() {
            if menu.slot(target).is_none_or(|s| s.container != origin_container) {
                continue;
            }
            if !menu.may_pickup(target) {
                continue;
            }
            let Some(target_item) = menu.slot_item(target) else {
                continue;
            };
            if !ItemStack::is_same_item_same_components(target_item, last) {
                continue;
            }
            clicks.push(Click::shift(target));
        }
        clicks
    }
}

fn quick_craft(slot: i32, header: i32, kind: i32) -> Click {
    Click {
        slot,
        button: quick_craft_mask(header, kind),
        input: ContainerInput::QuickCraft,
    }
}
