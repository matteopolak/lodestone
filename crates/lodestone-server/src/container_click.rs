//! Server-side `doClick`: the menu state machine that *derives* the result of a
//! container click instead of believing the client's claimed slot diff.
//!
//! # What it is
//!
//! A port of vanilla's `AbstractContainerMenu.doClick`
//! (`.cache/mc/26.2/src/net/minecraft/world/inventory/AbstractContainerMenu.java:334-540`)
//! over a flat, menu-ordered slot vector.
//!
//! Before this module `apply_container_clicked` applied the client's own
//! `changed_slots` map: the client had already run `doClick` locally, and the
//! server stored whatever it said each slot now contained. Issue #529 closed the
//! *crafting result* half of that (a claimed result was dropped and the server's
//! own value pushed back), and left the general hole open in writing: **a client
//! could mint any item in any ordinary slot by sending a container diff naming
//! it.** This closes it, in the only way that actually closes it — by re-running
//! the click from the button input.
//!
//! # How it works
//!
//! [`MenuLayout`] says what each menu index *is* ([`SlotKind`]) for the three menu
//! shapes this crate opens: the player screen (window `0`), a block-entity
//! container, and a crafting table. [`do_click`] then runs vanilla's state machine
//! over a `Vec<Option<ItemStack>>` in that ordering, plus a [`ClickState`] holding
//! the cursor and the in-progress drag.
//!
//! The caller's job is the two ends: build the slot vector from its real backing
//! stores, and write the result back. `crate::server::apply_container_clicked` does
//! both, and routes a grid write through [`crate::crafting::CraftingState::set_input`]
//! so the result slot is re-derived rather than copied.
//!
//! **The client's `changed_slots` map is not read at all any more.** It is compared
//! against the derived state purely to decide whether a correcting
//! `container_set_content` is worth sending — so an honest client sees no extra
//! traffic and a lying one is corrected on the same packet.
//!
//! # How to change it
//!
//! Adding a menu shape means a [`MenuKind`] variant, its [`MenuLayout`]
//! constructor, and its arm in [`MenuLayout::quick_move_targets`] (vanilla's
//! per-menu `quickMoveStack`). Nothing else in this module is menu-specific.
//!
//! ## Gotchas
//!
//! * **The result slot is take-only and taking it consumes the grid.** Vanilla does
//!   this in `ResultSlot.onTake` → `CraftingContainer.removeItem`; here it is
//!   [`take_result`]. Before the server derived clicks, `apply_container_clicked`
//!   deliberately did *not* consume, because the client's diff already carried the
//!   shrunk cells — that comment is now wrong and the consume is required.
//! * **Take-only is not un-clickable, and the difference was a shipped bug.**
//!   `ResultSlot.mayPlace` is `false` and `ResultSlot.onTake` is what decrements the
//!   grid: a click *on* the result is how you craft. [`do_click`] always modelled the
//!   take; what was missing is that the result slot is **live inside one click**.
//!   Vanilla's `QUICK_MOVE` arm loops `quickMoveStack` *while the clicked slot still
//!   holds the same item* (`AbstractContainerMenu.java:423-427`), and that loop only
//!   terminates because `slotsChanged` refills slot `0` between iterations — which is
//!   how shift-clicking a result crafts until the grid empties. So a caller that owns
//!   a recipe corpus passes it to [`do_click_with`]; [`do_click`] itself keeps the
//!   recipe-free behaviour (one craft per click) for callers that have none.
//! * **`may_place` on an armour slot is a real restriction**, checked against
//!   `lodestone_data::item_prototypes`' `equip_slot` (vanilla's `ArmorSlot.mayPlace`).
//!   Allowing anything there lets a boot go on your head, which reads as a
//!   rendering bug.
//! * **Max stack size is per item**, from the same prototype table. Defaulting to
//!   64 would let the server itself derive a 64-stack of swords — not minting, but
//!   the same duplication with extra steps.
//! * **What is deliberately *not* modelled**: `tryItemClickBehaviourOverride`
//!   (bundles), `canDropItems`, the tutorial hooks, and the drop-into-the-world
//!   *entity* (a `Throw` or an outside-click yields its stacks in [`do_click`]'s
//!   return value and the caller decides what to do with them). Also unmodelled:
//!   creative-mode `Clone` is gated on the caller's `creative` flag, matching
//!   `player.hasInfiniteMaterials()`.
//!
//! # Dependencies
//!
//! `lodestone_data::item_prototypes` (stack caps, equipment slots),
//! `lodestone-model` for [`ItemStack`]. No protocol, no packet id.

use lodestone_model::{EquipmentSlot, ItemStack};

use crate::inventory::{OFFHAND_NATIVE, PLAYER_NATIVE_SIZE};

/// What a menu index addresses in the server's own state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SlotKind {
    /// A [`crate::inventory::PlayerInventory`] native index.
    Player(usize),
    /// An index into the open block entity's own container slots.
    Container(usize),
    /// A crafting grid cell.
    Grid(usize),
    /// The crafting result — server-derived, take-only.
    Result,
}

/// Which menu shape a layout describes. Selects the quick-move routing, which is
/// the only genuinely per-menu behaviour in vanilla's `doClick` family.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuKind {
    /// `InventoryMenu` — window `0`.
    Player,
    /// A `generic_9x3`/furnace/hopper style menu with `size` own slots.
    Container {
        /// Number of the block entity's own slots, before the player tail.
        size: usize,
    },
    /// `CraftingMenu` — a crafting table's 3×3.
    CraftingTable,
    /// `ItemCombinerMenu`'s three positionless-scratch shapes (workstation
    /// economy, issues #253-#255): `inputs` grid cells then one take-only
    /// result, exactly `ItemCombinerMenu`'s own
    /// `getInventorySlotStart() == resultSlot + 1` (`docs/container-cost-screens.md`
    /// already documents this for the client-side layout; this is the same
    /// shape for the server's own slot algebra).
    ItemCombiner {
        /// `2` for the anvil/grindstone, `3` for the smithing table.
        inputs: usize,
        /// Which station's `may_place`/quick-move/take rules apply.
        station: Station,
    },
    /// `EnchantmentMenu`'s two slots: item (any, capped to a stack of one) and
    /// lapis. **No result slot** — unlike the other three, nothing is *taken*
    /// here; the item slot is enchanted in place. Positionless scratch space,
    /// same story as [`ItemCombiner`](Self::ItemCombiner).
    Enchanting,
}

/// Which workstation an [`MenuKind::ItemCombiner`] is — the anvil and
/// grindstone share an `inputs: 2` shape but differ in every rule that
/// matters (`may_place`, and — in [`take_result`] — how a take consumes the
/// input cells), so the shape alone cannot tell them apart.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Station {
    Anvil,
    Grindstone,
    Smithing,
}

/// The menu-index → [`SlotKind`] table for one open menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MenuLayout {
    kind: MenuKind,
    slots: Vec<SlotKind>,
}

impl MenuLayout {
    /// `InventoryMenu`'s 46 slots: result `0`, the 2×2 grid `1..=4`, armour
    /// `5..=8` (head→feet), main storage `9..=35`, hotbar `36..=44`, off-hand `45`.
    #[must_use]
    pub fn player() -> Self {
        let mut slots = vec![SlotKind::Result];
        slots.extend((0..4).map(SlotKind::Grid));
        // Armour, head first: natives 39, 38, 37, 36.
        slots.extend([39, 38, 37, 36].map(SlotKind::Player));
        slots.extend((9..36).map(SlotKind::Player));
        slots.extend((0..9).map(SlotKind::Player));
        slots.push(SlotKind::Player(OFFHAND_NATIVE));
        Self {
            kind: MenuKind::Player,
            slots,
        }
    }

    /// A block-entity menu: `size` own slots, then the standard 27 main-storage +
    /// 9 hotbar tail every `addStandardInventorySlots` menu appends. Never armour
    /// or off-hand — only `InventoryMenu` exposes those.
    #[must_use]
    pub fn container(size: usize) -> Self {
        let mut slots: Vec<SlotKind> = (0..size).map(SlotKind::Container).collect();
        slots.extend((9..36).map(SlotKind::Player));
        slots.extend((0..9).map(SlotKind::Player));
        Self {
            kind: MenuKind::Container { size },
            slots,
        }
    }

    /// `CraftingMenu`'s 46 slots: result `0`, the 3×3 grid `1..=9`, main storage
    /// `10..=36`, hotbar `37..=45`.
    #[must_use]
    pub fn crafting_table() -> Self {
        let mut slots = vec![SlotKind::Result];
        slots.extend((0..9).map(SlotKind::Grid));
        slots.extend((9..36).map(SlotKind::Player));
        slots.extend((0..9).map(SlotKind::Player));
        Self {
            kind: MenuKind::CraftingTable,
            slots,
        }
    }

    /// One `ItemCombinerMenu` shape: `station`'s own input-cell count grid
    /// cells (`Anvil`/`Grindstone` 2, `Smithing` 3), then one take-only
    /// result, then the standard 27+9 player tail
    /// (`addStandardInventorySlots(inventory, 8, 84)`, identical for all
    /// three — the anvil/grindstone/smithing/enchanting screens all place the
    /// player section at the same `y = 84`, per `docs/container-cost-screens.md`).
    #[must_use]
    pub fn item_combiner(station: Station) -> Self {
        let inputs = match station {
            Station::Anvil | Station::Grindstone => 2,
            Station::Smithing => 3,
        };
        let mut slots: Vec<SlotKind> = (0..inputs).map(SlotKind::Grid).collect();
        slots.push(SlotKind::Result);
        slots.extend((9..36).map(SlotKind::Player));
        slots.extend((0..9).map(SlotKind::Player));
        Self {
            kind: MenuKind::ItemCombiner { inputs, station },
            slots,
        }
    }

    /// `EnchantmentMenu`'s two slots (`15,47` item, `35,47` lapis) then the
    /// standard 27+9 player tail. See [`MenuKind::Enchanting`]'s own doc for
    /// why there is no result slot.
    #[must_use]
    pub fn enchanting_table() -> Self {
        let mut slots = vec![SlotKind::Grid(0), SlotKind::Grid(1)];
        slots.extend((9..36).map(SlotKind::Player));
        slots.extend((0..9).map(SlotKind::Player));
        Self {
            kind: MenuKind::Enchanting,
            slots,
        }
    }

    /// Total menu slots.
    #[must_use]
    pub fn len(&self) -> usize {
        self.slots.len()
    }

    /// Whether this layout has no slots (never true for the three constructors).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    /// What menu index `index` addresses.
    #[must_use]
    pub fn kind_of(&self, index: usize) -> Option<SlotKind> {
        self.slots.get(index).copied()
    }

    /// Every `(menu index, kind)` pair, in menu order.
    pub fn iter(&self) -> impl Iterator<Item = (usize, SlotKind)> + '_ {
        self.slots.iter().copied().enumerate()
    }

    /// `Slot.mayPlace` — whether `item` may be *put into* menu index `index`.
    ///
    /// Three real restrictions: the result slot takes nothing, an armour slot takes
    /// only an item whose `Equippable.slot()` is that armour slot
    /// (`ArmorSlot.mayPlace`), and the off-hand takes anything.
    #[must_use]
    fn may_place(&self, index: usize, item: &ItemStack) -> bool {
        match self.kind_of(index) {
            None | Some(SlotKind::Result) => false,
            Some(SlotKind::Player(native)) => match armour_slot_for_native(native) {
                Some(slot) => equip_slot_of(item) == Some(slot),
                None => true,
            },
            Some(SlotKind::Grid(cell)) => match self.kind {
                MenuKind::ItemCombiner { station, .. } => item_combiner_may_place(station, cell, item),
                // `EnchantmentMenu`'s lapis slot: `itemStack.is(Items.LAPIS_LAZULI)`.
                MenuKind::Enchanting if cell == 1 => item.item.to_string() == "minecraft:lapis_lazuli",
                _ => true,
            },
            Some(SlotKind::Container(_)) => true,
        }
    }

    /// `Slot.getMaxStackSize(stack)` — the per-item cap, from
    /// `lodestone_data::item_prototypes`.
    ///
    /// A menu with a smaller cap than the item's own (vanilla's furnace-fuel slot
    /// has none; the *brewing* stand does) is not modelled: no menu this crate
    /// opens narrows it.
    #[must_use]
    fn max_stack_size(&self, index: usize, item: &ItemStack) -> u32 {
        // `EnchantmentMenu`'s item slot overrides `getMaxStackSize()` to `1`
        // regardless of the item's own cap — the table only ever enchants one
        // item at a time.
        if self.kind == MenuKind::Enchanting && self.kind_of(index) == Some(SlotKind::Grid(0)) {
            return 1;
        }
        max_stack_size(item)
    }

    /// The `[start, end)` ranges a shift-click from `index` moves into, in the
    /// order tried, each with whether the scan runs backwards.
    ///
    /// One arm per menu, transcribed from that menu's own `quickMoveStack`. The
    /// `InventoryMenu` armour-equip and off-hand branches are included: they are
    /// what makes shift-clicking a helmet wear it.
    #[must_use]
    fn quick_move_targets(&self, index: usize, item: &ItemStack) -> Vec<(usize, usize, bool)> {
        match self.kind {
            // `InventoryMenu.quickMoveStack`.
            MenuKind::Player => {
                let equip = equip_slot_of(item);
                if index == 0 {
                    return vec![(9, 45, true)];
                }
                if (1..9).contains(&index) {
                    return vec![(9, 45, false)];
                }
                // `8 - eqSlot.getIndex()` — the armour slot for this item, if it is
                // armour and that slot is empty.
                if let Some(target) = equip.and_then(armour_menu_slot) {
                    return vec![(target, target + 1, false), (9, 45, false)];
                }
                if equip == Some(EquipmentSlot::OffHand) {
                    return vec![(45, 46, false), (9, 45, false)];
                }
                if (9..36).contains(&index) {
                    return vec![(36, 45, false)];
                }
                if (36..45).contains(&index) {
                    return vec![(9, 36, false)];
                }
                vec![(9, 45, false)]
            }
            // `ChestMenu`/`AbstractFurnaceMenu`/`HopperMenu`: own slots one way,
            // the player tail the other.
            MenuKind::Container { size } => {
                if index < size {
                    vec![(size, self.slots.len(), true)]
                } else {
                    vec![(0, size, false)]
                }
            }
            // `CraftingMenu.quickMoveStack`.
            MenuKind::CraftingTable => {
                if index == 0 {
                    return vec![(10, 46, true)];
                }
                if (10..46).contains(&index) {
                    // Vanilla tries the grid first, then the *other* player
                    // section — hotbar from storage, storage from hotbar.
                    return if index < 37 {
                        vec![(1, 10, false), (37, 46, false)]
                    } else {
                        vec![(1, 10, false), (10, 37, false)]
                    };
                }
                vec![(10, 46, false)]
            }
            // `ItemCombinerMenu.quickMoveStack`: result shifts out backwards into
            // the player tail, a grid cell shifts forward into the tail, and a
            // tail slot tries the input cells first (`canMoveIntoInputSlots`,
            // approximated as always-attempted — the real gate is still
            // `may_place` at the destination, so an ineligible item simply fails
            // to move rather than skipping the attempt) before falling back to
            // the usual storage<->hotbar shuffle.
            MenuKind::ItemCombiner { inputs, .. } => {
                let result = inputs;
                let tail_start = inputs + 1;
                let tail_end = self.slots.len();
                if index == result {
                    return vec![(tail_start, tail_end, true)];
                }
                if index < result {
                    return vec![(tail_start, tail_end, false)];
                }
                let hotbar_start = tail_end - 9;
                if index < hotbar_start {
                    vec![(0, result, false), (hotbar_start, tail_end, false)]
                } else {
                    vec![(0, result, false), (tail_start, hotbar_start, false)]
                }
            }
            // `EnchantmentMenu.quickMoveStack`: either input slot shifts out
            // backwards into the tail; lapis from the tail goes straight to slot
            // 1; anything else tries slot 0 first (capped to one item by
            // `max_stack_size`'s own override above).
            MenuKind::Enchanting => {
                if index < 2 {
                    return vec![(2, self.slots.len(), true)];
                }
                if item.item.to_string() == "minecraft:lapis_lazuli" {
                    return vec![(1, 2, false)];
                }
                vec![(0, 1, false), (2, self.slots.len(), false)]
            }
        }
    }
}

/// `Slot.mayPlace` for one of [`MenuKind::ItemCombiner`]'s input cells —
/// [`AnvilMenu`], [`GrindstoneMenu`] and [`SmithingMenu`]'s own
/// `createInputSlotDefinitions`/anonymous `Slot` overrides.
fn item_combiner_may_place(station: Station, cell: usize, item: &ItemStack) -> bool {
    match station {
        // `AnvilMenu.createInputSlotDefinitions`: both slots accept anything.
        Station::Anvil => true,
        // `GrindstoneMenu`'s two anonymous slots:
        // `itemStack.isDamageableItem() || EnchantmentHelper.hasAnyEnchantments(itemStack)`.
        Station::Grindstone => is_damageable(item) || !item.components.enchantments.is_empty(),
        // `SmithingMenu.createInputSlotDefinitions`: one `RecipePropertySet` test
        // per slot index.
        Station::Smithing => {
            let name = item.item.to_string();
            match cell {
                0 => crate::smithing::is_template(&name),
                1 => crate::smithing::is_base(&name),
                _ => crate::smithing::is_addition(&name),
            }
        }
    }
}

/// `ItemStack.isDamageableItem()` — has a `minecraft:max_damage` prototype,
/// via the same census [`max_stack_size`] already reads.
fn is_damageable(item: &ItemStack) -> bool {
    item.components.max_damage.is_some()
        || lodestone_data::item_prototypes::prototype(&item.item.to_string()).is_some_and(|p| p.max_damage.is_some())
}

/// The armour [`EquipmentSlot`] a player native index is, if any.
fn armour_slot_for_native(native: usize) -> Option<EquipmentSlot> {
    match native {
        36 => Some(EquipmentSlot::Feet),
        37 => Some(EquipmentSlot::Legs),
        38 => Some(EquipmentSlot::Chest),
        39 => Some(EquipmentSlot::Head),
        _ => None,
    }
}

/// The window-`0` menu index an armour [`EquipmentSlot`] occupies — vanilla's
/// `8 - eqSlot.getIndex()`.
fn armour_menu_slot(slot: EquipmentSlot) -> Option<usize> {
    match slot {
        EquipmentSlot::Feet => Some(8),
        EquipmentSlot::Legs => Some(7),
        EquipmentSlot::Chest => Some(6),
        EquipmentSlot::Head => Some(5),
        _ => None,
    }
}

fn equip_slot_of(item: &ItemStack) -> Option<EquipmentSlot> {
    lodestone_data::item_prototypes::prototype(&item.item.to_string()).and_then(|p| p.equip_slot)
}

/// `ItemStack.getMaxStackSize()` — the item's own component override if it carries
/// one, otherwise the jar-dumped prototype, otherwise 64.
#[must_use]
pub fn max_stack_size(item: &ItemStack) -> u32 {
    if let Some(override_size) = item.components.max_stack_size {
        return override_size.max(1);
    }
    lodestone_data::item_prototypes::prototype(&item.item.to_string())
        .map_or(64, |p| u32::from(p.max_stack_size))
        .max(1)
}

/// Whether two stacks are the same item with the same components —
/// `ItemStack.isSameItemSameComponents`.
fn same(a: &ItemStack, b: &ItemStack) -> bool {
    a.item == b.item && a.components == b.components
}

/// One inbound click, straight off the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Click {
    /// The clicked menu slot. `-999` is vanilla's "outside the window".
    pub slot: i32,
    /// `buttonNum`: mouse button, hotbar index, or the drag header/type mask.
    pub button: i8,
    /// `ContainerInput`'s ordinal: 0 pickup, 1 quick-move, 2 swap, 3 clone,
    /// 4 throw, 5 quick-craft, 6 pickup-all.
    pub click_type: i32,
}

/// Vanilla's "outside the window" slot index (`AbstractContainerMenu.SLOT_CLICKED_OUTSIDE`).
pub const SLOT_OUTSIDE: i32 = -999;

/// The in-progress `QUICK_CRAFT` drag: vanilla's `quickcraftStatus`/
/// `quickcraftType`/`quickcraftSlots` triple, which is per-menu state a single
/// click packet cannot carry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Drag {
    /// `0` = idle/started, `1` = collecting slots, `2` = ending.
    status: i32,
    /// `0` = even split (left drag), `1` = one each (right drag), `2` = fill
    /// (creative middle drag).
    kind: i32,
    /// Menu indices collected so far.
    slots: Vec<usize>,
}

/// The per-connection menu state a click packet does not carry: the cursor stack
/// and the in-progress drag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ClickState {
    /// Vanilla's `AbstractContainerMenu.carried` — the stack on the cursor.
    pub carried: Option<ItemStack>,
    drag: Drag,
}

impl ClickState {
    /// Clears the cursor and any in-progress drag — what closing a menu does.
    pub fn reset(&mut self) {
        self.carried = None;
        self.drag = Drag::default();
    }
}

/// A menu's recipe corpus: grid cells (row-major, by [`SlotKind::Grid`] cell index)
/// to the result they produce.
///
/// `lodestone-server`'s is `crate::crafting::derive_result` bound to the open grid's
/// dimensions. This module deliberately holds no corpus of its own — it is the click
/// state machine, and a second matcher here could disagree with
/// [`crate::crafting::CraftingState`]'s.
pub type ResultRecipe<'a> = &'a dyn Fn(&[Option<ItemStack>]) -> Option<ItemStack>;

/// Upper bound on [`quick_move`]'s repeat rounds — vanilla's `while` loop has none,
/// relying on the grid draining. A malformed [`ResultRecipe`] that refilled the
/// result without consuming anything would spin forever, and a server that hangs on
/// one click is worse than one that crafts 64 times.
const QUICK_MOVE_ROUNDS: usize = 512;

/// Runs one click against `slots` (menu-ordered) and `state`, with **no recipe
/// corpus**: the result slot is taken and the grid consumed exactly once, and the
/// result slot itself is left as the caller wrote it.
///
/// Returns the stacks that left the menu into the world (a `Throw`, or a click
/// outside the window with a full cursor) — vanilla's `player.drop(...)` calls,
/// which this module has no world to make.
///
/// `creative` is `player.hasInfiniteMaterials()`, which gates `Clone`.
pub fn do_click(
    layout: &MenuLayout,
    slots: &mut [Option<ItemStack>],
    state: &mut ClickState,
    click: Click,
    creative: bool,
) -> Vec<ItemStack> {
    do_click_with(layout, slots, state, click, creative, None)
}

/// [`do_click`], with the menu's own recipe corpus so the result slot is **live for
/// the duration of the click** — vanilla's `slotsChanged` →
/// `CraftingMenu.slotChangedCraftingGrid` hook.
///
/// Two behaviours need it, and neither is reachable without it:
///
/// * a shift-click on the result crafts **repeatedly** until the grid empties or the
///   inventory fills, because vanilla's `QUICK_MOVE` loop tests the *refilled* slot;
/// * the `slots` this returns carry the result the grid now produces, so a caller
///   comparing against the client's prediction sees the same value the client will.
pub fn do_click_with(
    layout: &MenuLayout,
    slots: &mut [Option<ItemStack>],
    state: &mut ClickState,
    click: Click,
    creative: bool,
    recipe: Option<ResultRecipe<'_>>,
) -> Vec<ItemStack> {
    let mut dropped = Vec::new();
    let index = usize::try_from(click.slot).ok();

    match click.click_type {
        // QUICK_CRAFT — the drag state machine.
        5 => {
            let expected = state.drag.status;
            state.drag.status = i32::from(click.button) & 3;
            if (expected != 1 || state.drag.status != 2) && expected != state.drag.status {
                state.drag = Drag::default();
            } else if state.carried.is_none() {
                state.drag = Drag::default();
            } else if state.drag.status == 0 {
                state.drag.kind = (i32::from(click.button) >> 2) & 3;
                if state.drag.kind == 2 && !creative {
                    state.drag = Drag::default();
                } else {
                    state.drag.status = 1;
                    state.drag.slots.clear();
                }
            } else if state.drag.status == 1 {
                let carried = state.carried.clone().expect("checked non-empty above");
                if let Some(index) = index {
                    let quick_replaceable = can_item_quick_replace(slots.get(index), &carried);
                    if quick_replaceable
                        && layout.may_place(index, &carried)
                        && (state.drag.kind == 2 || carried.count > state.drag.slots.len() as u32)
                        && !state.drag.slots.contains(&index)
                    {
                        state.drag.slots.push(index);
                    }
                }
            } else if state.drag.status == 2 {
                finish_drag(layout, slots, state, recipe);
                state.drag = Drag::default();
            } else {
                state.drag = Drag::default();
            }
        }
        // Any non-drag click aborts an in-progress drag, exactly as vanilla's
        // `else if (this.quickcraftStatus != 0)` arm does — and does nothing else.
        _ if state.drag.status != 0 => {
            state.drag = Drag::default();
        }
        // PICKUP and QUICK_MOVE share vanilla's arm because both are gated on
        // `buttonNum == 0 || buttonNum == 1`.
        0 | 1 if click.button == 0 || click.button == 1 => {
            let primary = click.button == 0;
            if click.slot == SLOT_OUTSIDE {
                if let Some(carried) = state.carried.clone() {
                    if primary {
                        dropped.push(carried);
                        state.carried = None;
                    } else if let Some(one) = split_one(&mut state.carried) {
                        dropped.push(one);
                    }
                }
            } else if let Some(index) = index.filter(|i| *i < layout.len()) {
                if click.click_type == 1 {
                    quick_move(layout, slots, index, &mut dropped, recipe);
                } else {
                    pickup(layout, slots, state, index, primary, &mut dropped, recipe);
                }
            }
        }
        // SWAP — a hotbar number key (`0..9`) or the off-hand key (`40`).
        2 if (0..9).contains(&click.button) || click.button == 40 => {
            if let Some(index) = index.filter(|i| *i < layout.len()) {
                swap(layout, slots, index, click.button, &mut dropped, recipe);
            }
        }
        // CLONE — creative middle-click.
        3 if creative && state.carried.is_none() => {
            if let Some(index) = index.filter(|i| *i < layout.len()) {
                if let Some(existing) = slots[index].clone() {
                    let mut cloned = existing;
                    cloned.count = max_stack_size(&cloned);
                    state.carried = Some(cloned);
                }
            }
        }
        // THROW — Q (one) or ctrl-Q (the whole stack), cursor must be empty.
        4 if state.carried.is_none() => {
            if let Some(index) = index.filter(|i| *i < layout.len()) {
                let whole = click.button == 1;
                if let Some(taken) =
                    take_from(layout, slots, index, if whole { u32::MAX } else { 1 }, recipe)
                {
                    dropped.push(taken);
                }
            }
        }
        // PICKUP_ALL — double-click gather into the cursor.
        6 => {
            if let Some(index) = index.filter(|i| *i < layout.len()) {
                pickup_all(layout, slots, state, index, click.button == 0, recipe);
            }
        }
        _ => {}
    }

    // `slotsChanged`: every arm above can have written a grid cell (a place, a drag,
    // a swap, a quick-move *into* the grid), and vanilla re-derives the result on any
    // of them — not only on a take. Doing it once here rather than in each arm is why
    // no arm has to remember to.
    resync_result(layout, slots, recipe);

    dropped
}

/// The menu index of the result slot, if this layout has one.
fn result_index(layout: &MenuLayout) -> Option<usize> {
    layout
        .slots
        .iter()
        .position(|kind| *kind == SlotKind::Result)
}

/// The grid cells in `slots`, in [`SlotKind::Grid`] cell order — the argument a
/// [`ResultRecipe`] takes.
fn grid_cells(layout: &MenuLayout, slots: &[Option<ItemStack>]) -> Vec<Option<ItemStack>> {
    let mut cells: Vec<Option<ItemStack>> = Vec::new();
    for (index, kind) in layout.iter() {
        if let SlotKind::Grid(cell) = kind {
            if cells.len() <= cell {
                cells.resize(cell + 1, None);
            }
            cells[cell] = slots.get(index).cloned().flatten();
        }
    }
    cells
}

/// Re-derives the result slot from the grid — `CraftingMenu.slotChangedCraftingGrid`.
///
/// A `None` recipe leaves the result slot **exactly as it is**, which is the
/// recipe-free [`do_click`] contract: a caller with no corpus has already written
/// whatever it believes the result to be, and clearing it here would silently
/// contradict that.
fn resync_result(
    layout: &MenuLayout,
    slots: &mut [Option<ItemStack>],
    recipe: Option<ResultRecipe<'_>>,
) {
    let Some(recipe) = recipe else { return };
    let Some(index) = result_index(layout) else {
        return;
    };
    let cells = grid_cells(layout, slots);
    if cells.is_empty() {
        return;
    }
    if let Some(slot) = slots.get_mut(index) {
        *slot = recipe(&cells);
    }
}

/// `AbstractContainerMenu.canItemQuickReplace(slot, stack, true)`.
fn can_item_quick_replace(slot: Option<&Option<ItemStack>>, item: &ItemStack) -> bool {
    match slot.and_then(Option::as_ref) {
        None => true,
        Some(existing) => same(existing, item) && existing.count <= max_stack_size(item),
    }
}

/// `AbstractContainerMenu.getQuickCraftPlaceCount`.
fn quick_craft_place_count(slot_count: usize, kind: i32, item: &ItemStack) -> u32 {
    match kind {
        0 => item.count / slot_count.max(1) as u32,
        1 => 1,
        2 => max_stack_size(item),
        _ => item.count,
    }
}

/// The `quickcraftStatus == 2` branch: distribute the cursor over the collected
/// slots, then keep whatever is left on the cursor.
fn finish_drag(
    layout: &MenuLayout,
    slots: &mut [Option<ItemStack>],
    state: &mut ClickState,
    recipe: Option<ResultRecipe<'_>>,
) {
    if state.drag.slots.is_empty() {
        return;
    }
    let Some(source) = state.carried.clone() else {
        return;
    };
    // A one-slot drag degrades to an ordinary click, vanilla's own shortcut.
    if state.drag.slots.len() == 1 {
        let index = state.drag.slots[0];
        let primary = state.drag.kind == 0;
        let mut dropped = Vec::new();
        state.drag = Drag::default();
        pickup(layout, slots, state, index, primary, &mut dropped, recipe);
        return;
    }

    let mut remaining = source.count;
    let collected = state.drag.slots.clone();
    for index in collected {
        let Some(carried) = state.carried.as_ref() else { break };
        if !can_item_quick_replace(slots.get(index), carried) || !layout.may_place(index, carried) {
            continue;
        }
        if state.drag.kind != 2 && carried.count < state.drag.slots.len() as u32 {
            continue;
        }
        let held = slots[index].as_ref().map_or(0, |s| s.count);
        let cap = max_stack_size(&source).min(layout.max_stack_size(index, &source));
        let new_count =
            (quick_craft_place_count(state.drag.slots.len(), state.drag.kind, &source) + held).min(cap);
        remaining = remaining.saturating_sub(new_count - held);
        let mut placed = source.clone();
        placed.count = new_count;
        slots[index] = Some(placed);
    }
    state.carried = if remaining == 0 {
        None
    } else {
        let mut left = source;
        left.count = remaining;
        Some(left)
    };
}

/// `PICKUP`: the four-way cursor/slot interaction.
fn pickup(
    layout: &MenuLayout,
    slots: &mut [Option<ItemStack>],
    state: &mut ClickState,
    index: usize,
    primary: bool,
    dropped: &mut Vec<ItemStack>,
    recipe: Option<ResultRecipe<'_>>,
) {
    let clicked = slots[index].clone();
    let carried = state.carried.clone();

    match (clicked, carried) {
        // Empty slot, full cursor: insert.
        (None, Some(carried)) => {
            let amount = if primary { carried.count } else { 1 };
            state.carried = safe_insert(layout, slots, index, carried, amount);
        }
        // Full slot, empty cursor: take (all, or half rounded up).
        (Some(clicked), None) => {
            let amount = if primary {
                clicked.count
            } else {
                clicked.count.div_ceil(2)
            };
            if let Some(taken) = take_from(layout, slots, index, amount, recipe) {
                state.carried = Some(taken);
            }
        }
        // Both full.
        (Some(clicked), Some(carried)) => {
            if layout.may_place(index, &carried) {
                if same(&clicked, &carried) {
                    // Merge cursor into slot.
                    let amount = if primary { carried.count } else { 1 };
                    state.carried = safe_insert(layout, slots, index, carried, amount);
                } else if carried.count <= layout.max_stack_size(index, &carried) {
                    // Straight swap.
                    slots[index] = Some(carried);
                    state.carried = Some(clicked);
                }
            } else if same(&clicked, &carried) {
                // A take-only slot holding the same item tops the cursor up —
                // this is how a crafting result stacks onto a partial cursor.
                let room = max_stack_size(&carried).saturating_sub(carried.count);
                if room > 0 {
                    if let Some(taken) =
                        take_from(layout, slots, index, clicked.count.min(room), recipe)
                    {
                        let mut grown = carried;
                        grown.count += taken.count;
                        state.carried = Some(grown);
                    }
                }
            }
        }
        (None, None) => {}
    }
    let _ = dropped;
}

/// `Slot.safeInsert(stack, amount)` — inserts up to `amount`, returns the
/// remainder (`None` when all of it went in).
fn safe_insert(
    layout: &MenuLayout,
    slots: &mut [Option<ItemStack>],
    index: usize,
    mut stack: ItemStack,
    amount: u32,
) -> Option<ItemStack> {
    if !layout.may_place(index, &stack) {
        return Some(stack);
    }
    let cap = max_stack_size(&stack).min(layout.max_stack_size(index, &stack));
    let held = match slots[index].as_ref() {
        Some(existing) if !same(existing, &stack) => return Some(stack),
        Some(existing) => existing.count,
        None => 0,
    };
    let room = cap.saturating_sub(held);
    let moved = amount.min(stack.count).min(room);
    if moved == 0 {
        return Some(stack);
    }
    match slots[index].as_mut() {
        Some(existing) => existing.count += moved,
        None => {
            let mut fresh = stack.clone();
            fresh.count = moved;
            slots[index] = Some(fresh);
        }
    }
    stack.count -= moved;
    if stack.count == 0 { None } else { Some(stack) }
}

/// `Slot.safeTake(amount, …)` — removes up to `amount` from `index`.
///
/// **The result slot's take consumes the grid**, which the caller learns by the
/// grid cells in `slots` having shrunk (`ResultSlot.onTake` →
/// `CraftingContainer.removeItem`). Nothing else about a take is special.
fn take_from(
    layout: &MenuLayout,
    slots: &mut [Option<ItemStack>],
    index: usize,
    amount: u32,
    recipe: Option<ResultRecipe<'_>>,
) -> Option<ItemStack> {
    let existing = slots[index].clone()?;
    let taken = amount.min(existing.count);
    if taken == 0 {
        return None;
    }
    if taken >= existing.count {
        slots[index] = None;
    } else if let Some(remaining) = slots[index].as_mut() {
        remaining.count -= taken;
    }
    if layout.kind_of(index) == Some(SlotKind::Result) {
        take_result(layout, slots, recipe);
    }
    let mut out = existing;
    out.count = taken;
    Some(out)
}

/// `ResultSlot.onTake` — how much of each input cell one take consumes, then
/// the result slot is re-derived from what is left (`slotsChanged`).
///
/// The re-derivation is *here* rather than only at the end of the click because
/// [`quick_move`]'s repeat loop reads it: the refilled result is what tells it to
/// craft again.
///
/// Three shapes, one per family: crafting/smithing consume exactly one of
/// every grid cell (`CraftingMenu`'s own grid, `SmithingMenu.onTake`'s three
/// `shrinkStackInSlot` calls); the grindstone always fully clears both input
/// cells regardless of what was consumed (`GrindstoneMenu`'s result slot
/// `onTake`, unconditional `setItem(0/1, EMPTY)`); the anvil is the one
/// genuinely bespoke case — cell 0 (input) is always cleared, cell 1
/// (addition) is either partially shrunk by `repairItemCountCost`, cleared, or
/// left untouched for a pure rename (`AnvilMenu.onTake`). The anvil branch
/// re-derives that shape from [`crate::anvil::compute`] with `creative: true`
/// purely to read its consumption fields — safe because creative can only
/// ever *widen* which combination produces a result, so a result that reached
/// this take (however it was gated) is reproduced identically.
fn take_result(
    layout: &MenuLayout,
    slots: &mut [Option<ItemStack>],
    recipe: Option<ResultRecipe<'_>>,
) {
    match layout.kind {
        MenuKind::ItemCombiner { station: Station::Grindstone, .. } => {
            for (index, kind) in layout.slots.iter().copied().enumerate() {
                if matches!(kind, SlotKind::Grid(_)) {
                    slots[index] = None;
                }
            }
        }
        MenuKind::ItemCombiner { station: Station::Anvil, .. } => {
            let cells = grid_cells(layout, slots);
            let input = cells.first().and_then(Option::as_ref);
            let addition = cells.get(1).and_then(Option::as_ref);
            let outcome = crate::anvil::compute(input, addition, None, true);
            for (index, kind) in layout.slots.iter().copied().enumerate() {
                let SlotKind::Grid(cell) = kind else { continue };
                match cell {
                    0 => slots[index] = None,
                    1 => {
                        if outcome.repair_item_count_cost > 0 {
                            match slots[index].as_mut() {
                                Some(stack) if stack.count > outcome.repair_item_count_cost => {
                                    stack.count -= outcome.repair_item_count_cost;
                                }
                                _ => slots[index] = None,
                            }
                        } else if !outcome.only_renaming {
                            slots[index] = None;
                        }
                        // Pure rename with nothing consumed: addition slot (if any)
                        // is left exactly as it was, matching vanilla.
                    }
                    _ => {}
                }
            }
        }
        _ => {
            for (index, kind) in layout.slots.iter().copied().enumerate() {
                if !matches!(kind, SlotKind::Grid(_)) {
                    continue;
                }
                let Some(cell) = slots[index].as_mut() else { continue };
                if cell.count <= 1 {
                    slots[index] = None;
                } else {
                    cell.count -= 1;
                }
            }
        }
    }
    resync_result(layout, slots, recipe);
}

/// `QUICK_MOVE`: `quickMoveStack`, then vanilla's own **repeat loop**.
///
/// `AbstractContainerMenu.doClick`'s `QUICK_MOVE` arm is
///
/// ```java
/// ItemStack clicked = this.quickMoveStack(player, slotIndex);
/// while (!clicked.isEmpty() && ItemStack.isSameItem(this.getSlot(slotIndex).getItem(), clicked)) {
///    clicked = this.quickMoveStack(player, slotIndex);
/// }
/// ```
///
/// For every slot but a crafting result that loop runs once — the slot is empty or
/// unchanged the second time round. For the **result** slot it is the whole of
/// "shift-click crafts until the grid runs out": each round consumes one of every
/// grid cell, `slotsChanged` refills the result with the same item, and the condition
/// holds again. It ends when the grid can no longer produce that item (nothing to
/// refill with) or the inventory has no room (nothing moves, so `quickMoveStack`
/// returns EMPTY).
///
/// Without a [`ResultRecipe`] the result slot never refills and this is one craft,
/// which is [`do_click`]'s documented recipe-free behaviour.
fn quick_move(
    layout: &MenuLayout,
    slots: &mut [Option<ItemStack>],
    index: usize,
    dropped: &mut Vec<ItemStack>,
    recipe: Option<ResultRecipe<'_>>,
) {
    let is_result = layout.kind_of(index) == Some(SlotKind::Result);
    for _ in 0..QUICK_MOVE_ROUNDS {
        let Some(source) = slots[index].clone() else {
            return;
        };
        let mut stack = source.clone();
        let targets = layout.quick_move_targets(index, &source);
        for (start, end, backwards) in targets {
            move_stack_to(layout, slots, &mut stack, start, end, backwards, index);
            if stack.count == 0 {
                break;
            }
        }
        if stack.count == source.count {
            // Nothing moved — vanilla returns EMPTY and leaves the slot alone.
            return;
        }
        if stack.count == 0 {
            slots[index] = None;
        } else if let Some(existing) = slots[index].as_mut() {
            existing.count = stack.count;
        }
        if !is_result {
            return;
        }
        take_result(layout, slots, recipe);
        // Vanilla drops whatever would not fit rather than leaving it in the result
        // slot (`if (slotIndex == 0) player.drop(stack, false)`). It does **not**
        // clear the slot: `onTake` has already refilled it with the next result, and
        // the dropped stack is the old object. With no recipe there is nothing to
        // refill with, so the leftover would linger as a phantom result and is
        // cleared instead.
        if stack.count > 0 {
            dropped.push(stack);
            if recipe.is_none() {
                slots[index] = None;
            }
            return;
        }
        // Vanilla's `while`: the grid refilled the result with the same item, so
        // craft again.
        match slots[index].as_ref() {
            Some(next) if same(next, &source) => {}
            _ => return,
        }
    }
}

/// `moveItemStackTo(stack, start, end, backwards)` — merge pass then place pass.
#[allow(clippy::too_many_arguments)]
fn move_stack_to(
    layout: &MenuLayout,
    slots: &mut [Option<ItemStack>],
    stack: &mut ItemStack,
    start: usize,
    end: usize,
    backwards: bool,
    skip: usize,
) {
    let end = end.min(slots.len());
    if start >= end {
        return;
    }
    let order: Vec<usize> = if backwards {
        (start..end).rev().collect()
    } else {
        (start..end).collect()
    };

    if max_stack_size(stack) > 1 {
        for &index in &order {
            if stack.count == 0 || index == skip {
                continue;
            }
            let cap = layout.max_stack_size(index, stack).min(max_stack_size(stack));
            let Some(target) = slots[index].as_mut() else { continue };
            if !same(target, stack) {
                continue;
            }
            let total = target.count + stack.count;
            if total <= cap {
                target.count = total;
                stack.count = 0;
            } else if target.count < cap {
                stack.count -= cap - target.count;
                target.count = cap;
            }
        }
    }
    if stack.count == 0 {
        return;
    }
    for &index in &order {
        if index == skip || slots[index].is_some() || !layout.may_place(index, stack) {
            continue;
        }
        let cap = layout.max_stack_size(index, stack).min(max_stack_size(stack));
        let moved = stack.count.min(cap);
        let mut placed = stack.clone();
        placed.count = moved;
        slots[index] = Some(placed);
        stack.count -= moved;
        break;
    }
}

/// `SWAP`: exchange the clicked slot with a hotbar native (`0..9`) or the
/// off-hand (`40`).
fn swap(
    layout: &MenuLayout,
    slots: &mut [Option<ItemStack>],
    index: usize,
    button: i8,
    dropped: &mut Vec<ItemStack>,
    recipe: Option<ResultRecipe<'_>>,
) {
    let native = if button == 40 {
        OFFHAND_NATIVE
    } else {
        usize::try_from(button).unwrap_or(0)
    };
    if native >= PLAYER_NATIVE_SIZE {
        return;
    }
    // The hotbar/off-hand slot's own menu index in *this* layout. Vanilla reaches
    // straight into `Inventory`; here every menu already exposes the hotbar, so the
    // exchange stays inside the slot vector and the caller's write-back handles it.
    let Some(source_index) = layout
        .iter()
        .find(|(_, kind)| *kind == SlotKind::Player(native))
        .map(|(index, _)| index)
    else {
        return;
    };
    if source_index == index {
        return;
    }
    let source = slots[source_index].clone();
    let target = slots[index].clone();
    match (source, target) {
        (None, None) => {}
        (None, Some(target)) => {
            if layout.kind_of(index) == Some(SlotKind::Result) {
                // A result can be swapped *out* but the take must consume the grid.
                slots[index] = None;
                take_result(layout, slots, recipe);
            } else {
                slots[index] = None;
            }
            slots[source_index] = Some(target);
        }
        (Some(source), None) => {
            if !layout.may_place(index, &source) {
                return;
            }
            let cap = layout.max_stack_size(index, &source).min(max_stack_size(&source));
            if source.count > cap {
                let mut placed = source.clone();
                placed.count = cap;
                slots[index] = Some(placed);
                if let Some(remaining) = slots[source_index].as_mut() {
                    remaining.count -= cap;
                }
            } else {
                slots[source_index] = None;
                slots[index] = Some(source);
            }
        }
        (Some(source), Some(target)) => {
            if !layout.may_place(index, &source) {
                return;
            }
            let cap = layout.max_stack_size(index, &source).min(max_stack_size(&source));
            if source.count > cap {
                let mut placed = source.clone();
                placed.count = cap;
                slots[index] = Some(placed);
                if let Some(remaining) = slots[source_index].as_mut() {
                    remaining.count -= cap;
                }
                // Vanilla tries `inventory.add` then drops; this module has no
                // inventory-wide add, so the displaced stack goes to the world —
                // the same visible outcome for a full inventory.
                dropped.push(target);
            } else {
                slots[source_index] = Some(target);
                slots[index] = Some(source);
            }
        }
    }
}

/// `PICKUP_ALL`: two passes gathering matching stacks into the cursor, partial
/// stacks first.
fn pickup_all(
    layout: &MenuLayout,
    slots: &mut [Option<ItemStack>],
    state: &mut ClickState,
    index: usize,
    forwards: bool,
    recipe: Option<ResultRecipe<'_>>,
) {
    let Some(mut carried) = state.carried.clone() else {
        return;
    };
    // Vanilla only gathers when the clicked slot cannot be picked from — i.e. the
    // double-click landed on an empty slot or a take-only one.
    if slots[index].is_some() && layout.kind_of(index) != Some(SlotKind::Result) {
        return;
    }
    let cap = max_stack_size(&carried);
    let order: Vec<usize> = if forwards {
        (0..slots.len()).collect()
    } else {
        (0..slots.len()).rev().collect()
    };
    for pass in 0..2 {
        for &target in &order {
            if carried.count >= cap {
                break;
            }
            if layout.kind_of(target) == Some(SlotKind::Result) {
                continue;
            }
            let Some(existing) = slots[target].clone() else { continue };
            if !same(&existing, &carried) {
                continue;
            }
            // Pass 0 skips full stacks, so partials are consolidated first.
            if pass == 0 && existing.count == max_stack_size(&existing) {
                continue;
            }
            let room = cap - carried.count;
            if let Some(taken) = take_from(layout, slots, target, existing.count.min(room), recipe) {
                carried.count += taken.count;
            }
        }
    }
    state.carried = Some(carried);
}

/// Splits one item off the cursor, clearing it when that was the last.
fn split_one(carried: &mut Option<ItemStack>) -> Option<ItemStack> {
    let stack = carried.as_mut()?;
    let mut one = stack.clone();
    one.count = 1;
    if stack.count <= 1 {
        *carried = None;
    } else {
        stack.count -= 1;
    }
    Some(one)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(name: &str, count: u32) -> ItemStack {
        ItemStack::new(name.parse().expect("valid key"), count)
    }

    fn click(slot: i32, button: i8, click_type: i32) -> Click {
        Click {
            slot,
            button,
            click_type,
        }
    }

    /// **The security property, stated directly.** A click carries only a slot, a
    /// button and a click type; there is no channel through which an item can be
    /// named. So no sequence of clicks against an empty menu can produce an item.
    #[test]
    fn no_click_against_an_empty_menu_can_produce_an_item() {
        let layout = MenuLayout::player();
        let mut slots = vec![None; layout.len()];
        let mut state = ClickState::default();
        for click_type in 0..7 {
            for button in [-1i8, 0, 1, 2, 8, 40, 64] {
                for slot in [-999i32, -1, 0, 1, 5, 9, 36, 45, 46, 9999] {
                    do_click(
                        &layout,
                        &mut slots,
                        &mut state,
                        click(slot, button, click_type),
                        true,
                    );
                }
            }
        }
        assert!(
            slots.iter().all(Option::is_none),
            "an empty menu stayed empty: {slots:?}"
        );
        assert!(state.carried.is_none(), "the cursor stayed empty");
    }

    /// Left-click picks the whole stack up, right-click puts one down — and the
    /// counts are exact, not merely "something moved".
    #[test]
    fn pickup_takes_all_then_places_one() {
        let layout = MenuLayout::player();
        let mut slots = vec![None; layout.len()];
        slots[9] = Some(stack("minecraft:cobblestone", 7));
        let mut state = ClickState::default();

        do_click(&layout, &mut slots, &mut state, click(9, 0, 0), false);
        assert_eq!(slots[9], None);
        assert_eq!(state.carried.as_ref().map(|s| s.count), Some(7));

        do_click(&layout, &mut slots, &mut state, click(10, 1, 0), false);
        assert_eq!(slots[10].as_ref().map(|s| s.count), Some(1));
        assert_eq!(state.carried.as_ref().map(|s| s.count), Some(6));

        // Right-click on a full slot with an empty cursor takes half, rounded up.
        state.carried = None;
        slots[11] = Some(stack("minecraft:cobblestone", 7));
        do_click(&layout, &mut slots, &mut state, click(11, 1, 0), false);
        assert_eq!(state.carried.as_ref().map(|s| s.count), Some(4));
        assert_eq!(slots[11].as_ref().map(|s| s.count), Some(3));
    }

    /// A per-item stack cap, from `lodestone_data`'s jar dump rather than a
    /// constant 64: two swords never become one stack of two.
    #[test]
    fn the_stack_cap_is_per_item() {
        assert_eq!(max_stack_size(&stack("minecraft:cobblestone", 1)), 64);
        assert_eq!(max_stack_size(&stack("minecraft:diamond_sword", 1)), 1);
        assert_eq!(max_stack_size(&stack("minecraft:ender_pearl", 1)), 16);

        let layout = MenuLayout::player();
        let mut slots = vec![None; layout.len()];
        slots[9] = Some(stack("minecraft:diamond_sword", 1));
        let mut state = ClickState::default();
        state.carried = Some(stack("minecraft:diamond_sword", 1));
        // Same item, but the slot is already at its cap of 1, so this is a swap
        // rather than a merge — and the cursor still holds exactly one.
        do_click(&layout, &mut slots, &mut state, click(9, 0, 0), false);
        assert_eq!(slots[9].as_ref().map(|s| s.count), Some(1));
        assert_eq!(state.carried.as_ref().map(|s| s.count), Some(1));
    }

    /// An armour slot takes only its own armour piece — `ArmorSlot.mayPlace`.
    #[test]
    fn an_armour_slot_refuses_the_wrong_piece() {
        let layout = MenuLayout::player();
        let mut slots = vec![None; layout.len()];
        let mut state = ClickState::default();

        // Menu slot 5 is the head. Boots must not go there.
        state.carried = Some(stack("minecraft:diamond_boots", 1));
        do_click(&layout, &mut slots, &mut state, click(5, 0, 0), false);
        assert_eq!(slots[5], None, "boots must not fit the helmet slot");
        assert!(state.carried.is_some(), "and the cursor keeps them");

        state.carried = Some(stack("minecraft:diamond_helmet", 1));
        do_click(&layout, &mut slots, &mut state, click(5, 0, 0), false);
        assert!(slots[5].is_some(), "a helmet does fit the helmet slot");
        assert!(state.carried.is_none());
    }

    /// Shift-clicking moves storage to hotbar and back, and merges before it
    /// places — the two-pass `moveItemStackTo`.
    #[test]
    fn quick_move_merges_before_it_places() {
        let layout = MenuLayout::player();
        let mut slots = vec![None; layout.len()];
        // Storage slot 9 holds 10 cobble; hotbar slot 36 already holds 60.
        slots[9] = Some(stack("minecraft:cobblestone", 10));
        slots[36] = Some(stack("minecraft:cobblestone", 60));
        let mut state = ClickState::default();

        do_click(&layout, &mut slots, &mut state, click(9, 0, 1), false);
        assert_eq!(slots[36].as_ref().map(|s| s.count), Some(64), "topped up first");
        assert_eq!(slots[37].as_ref().map(|s| s.count), Some(6), "the rest opened a slot");
        assert_eq!(slots[9], None);
    }

    /// Taking a crafting result consumes one of every grid cell, and the result
    /// slot itself is never writable.
    #[test]
    fn taking_the_result_consumes_the_grid_and_the_result_is_not_writable() {
        let layout = MenuLayout::player();
        let mut slots = vec![None; layout.len()];
        for cell in 1..=4 {
            slots[cell] = Some(stack("minecraft:oak_planks", 3));
        }
        slots[0] = Some(stack("minecraft:crafting_table", 1));
        let mut state = ClickState::default();

        do_click(&layout, &mut slots, &mut state, click(0, 0, 0), false);
        assert_eq!(state.carried.as_ref().map(|s| s.count), Some(1));
        for cell in 1..=4 {
            assert_eq!(
                slots[cell].as_ref().map(|s| s.count),
                Some(2),
                "grid cell {cell} must have shrunk by exactly one"
            );
        }

        // And nothing can be *placed* into the result.
        state.carried = Some(stack("minecraft:diamond_block", 64));
        do_click(&layout, &mut slots, &mut state, click(0, 0, 0), false);
        assert_eq!(state.carried.as_ref().map(|s| s.count), Some(64));
        assert_eq!(slots[0], None);
    }

    /// A left-drag over three empty slots splits the cursor evenly and keeps the
    /// remainder — the three-packet `QUICK_CRAFT` sequence.
    #[test]
    fn a_left_drag_splits_the_cursor_evenly() {
        let layout = MenuLayout::container(27);
        let mut slots = vec![None; layout.len()];
        let mut state = ClickState::default();
        state.carried = Some(stack("minecraft:cobblestone", 7));

        // header 0 (start), type 0 (even) -> button = (0 << 2) | 0
        do_click(&layout, &mut slots, &mut state, click(-999, 0, 5), false);
        for slot in [0i32, 1, 2] {
            // header 1 (add slot) -> button = (0 << 2) | 1
            do_click(&layout, &mut slots, &mut state, click(slot, 1, 5), false);
        }
        // header 2 (end) -> button = (0 << 2) | 2
        do_click(&layout, &mut slots, &mut state, click(-999, 2, 5), false);

        for slot in 0..3 {
            assert_eq!(
                slots[slot].as_ref().map(|s| s.count),
                Some(2),
                "7 over 3 slots is 2 each"
            );
        }
        assert_eq!(
            state.carried.as_ref().map(|s| s.count),
            Some(1),
            "and the remainder stays on the cursor"
        );
    }

    /// A double-click gathers matching partial stacks into the cursor before full
    /// ones, and stops at the cap.
    #[test]
    fn pickup_all_gathers_partials_first() {
        let layout = MenuLayout::container(27);
        let mut slots = vec![None; layout.len()];
        slots[0] = Some(stack("minecraft:cobblestone", 64));
        slots[1] = Some(stack("minecraft:cobblestone", 10));
        slots[2] = Some(stack("minecraft:cobblestone", 20));
        let mut state = ClickState::default();
        state.carried = Some(stack("minecraft:cobblestone", 1));

        do_click(&layout, &mut slots, &mut state, click(26, 0, 6), false);
        assert_eq!(state.carried.as_ref().map(|s| s.count), Some(64));
        // The two partials went first: 1 + 10 + 20 = 31, then 33 off the full one.
        assert_eq!(slots[1], None);
        assert_eq!(slots[2], None);
        assert_eq!(slots[0].as_ref().map(|s| s.count), Some(31));
    }

    /// Throwing drops from the slot without touching the cursor, and `button == 1`
    /// throws the whole stack.
    #[test]
    fn throw_drops_one_or_all() {
        let layout = MenuLayout::container(27);
        let mut slots = vec![None; layout.len()];
        slots[0] = Some(stack("minecraft:cobblestone", 5));
        let mut state = ClickState::default();

        let dropped = do_click(&layout, &mut slots, &mut state, click(0, 0, 4), false);
        assert_eq!(dropped.iter().map(|s| s.count).sum::<u32>(), 1);
        assert_eq!(slots[0].as_ref().map(|s| s.count), Some(4));

        let dropped = do_click(&layout, &mut slots, &mut state, click(0, 1, 4), false);
        assert_eq!(dropped.iter().map(|s| s.count).sum::<u32>(), 4);
        assert_eq!(slots[0], None);
    }

    /// `Clone` is creative-only. In survival it does nothing at all — the arm that
    /// would otherwise be a mint-anything button for any client claiming a middle
    /// click.
    #[test]
    fn clone_is_refused_outside_creative() {
        let layout = MenuLayout::container(27);
        let mut slots = vec![None; layout.len()];
        slots[0] = Some(stack("minecraft:cobblestone", 1));
        let mut state = ClickState::default();

        do_click(&layout, &mut slots, &mut state, click(0, 2, 3), false);
        assert!(state.carried.is_none(), "survival must not clone");

        do_click(&layout, &mut slots, &mut state, click(0, 2, 3), true);
        assert_eq!(
            state.carried.as_ref().map(|s| s.count),
            Some(64),
            "creative clones a full stack"
        );
    }

    /// A number-key swap exchanges the clicked slot with that hotbar slot, in both
    /// directions.
    #[test]
    fn swap_exchanges_with_the_named_hotbar_slot() {
        let layout = MenuLayout::container(27);
        let mut slots = vec![None; layout.len()];
        slots[0] = Some(stack("minecraft:cobblestone", 5));
        // Hotbar native 0 is the first slot after the 27 storage rows.
        let hotbar = 27 + 27;
        slots[hotbar] = Some(stack("minecraft:torch", 3));
        let mut state = ClickState::default();

        do_click(&layout, &mut slots, &mut state, click(0, 0, 2), false);
        assert_eq!(slots[0].as_ref().map(|s| s.item.to_string()), Some("minecraft:torch".into()));
        assert_eq!(
            slots[hotbar].as_ref().map(|s| s.item.to_string()),
            Some("minecraft:cobblestone".into())
        );
    }
}
