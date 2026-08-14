//! Hopper block entity: the container-to-container item transfer engine
//! (issue #250).
//!
//! # Where the truth comes from
//!
//! `.cache/mc/26.2/src/net/minecraft/world/level/block/entity/
//! HopperBlockEntity.java`.
//!
//! * `HOPPER_CONTAINER_SIZE = 5` (`:33`) — [`HOPPER_SIZE`].
//! * The cooldown is **not** the declared `MOVE_ITEM_SPEED = 8` constant
//!   (`:32`) — that field is declared but never referenced anywhere in the
//!   class (checked directly, not assumed); the two call sites that actually
//!   set a transfer cooldown hardcode the literal `8`
//!   (`tryMoveItems`: `entity.setCooldown(8)`, `:124`; `tryMoveInItem`:
//!   `hopperBlockEntity.setCooldown(8 - skipTickCount)`, `:340`). This
//!   module names its own [`TRANSFER_COOLDOWN_TICKS`] constant rather than
//!   reusing that dead field, restated with the citation.
//! * `pushItemsTick` (`:97-104`): `cooldownTime--` happens **every** tick,
//!   unconditionally; only once `!isOnCooldown()` (`cooldownTime <= 0`) does
//!   it reset to exactly `0` and attempt a transfer. This is why a hopper
//!   with nothing to do retries every single tick (cooldown pinned at `0`,
//!   never drifting negative) while one that just moved an item waits the
//!   full 8 ticks before its next attempt — [`Hopper::tick`] mirrors both
//!   halves.
//! * `tryMoveItems` (`:106-131`): gated on not-on-cooldown **and** the
//!   block's `ENABLED` state (the redstone-lock rule — vanilla keys this off
//!   `HopperBlock.checkPoweredState`'s `!level.hasNeighborSignal(pos)`, kept
//!   on the block state rather than the block entity, so [`Hopper::tick`]
//!   takes `enabled` as a caller-supplied argument rather than owning it).
//!   Ejects first (if non-empty), *then* independently attempts a suck (if
//!   not full) — both can succeed in the same ready tick, and either one
//!   resets the cooldown to 8.
//! * `ejectItems`/`tryTakeInItemFromSlot` (`:143-172`, `:248-265`) both move
//!   **exactly one item** per attempt (`removeItem(slot, 1)`), trying the
//!   source's slots in order and stopping at the first slot that
//!   successfully moves anything — not a whole-stack transfer, and not a
//!   scan that keeps going after one success.
//! * `tryMoveInItem` (`:314-348`): if the destination slot is empty, the
//!   **whole** incoming stack (already capped at 1 by the callers above)
//!   lands directly; only when the destination slot already holds a
//!   same-item-same-components stack does the merge/grow math run, capped at
//!   [`MAX_STACK_SIZE`].
//! * Chained-hopper tick-skip (`:333-341`): when a transfer fills a
//!   previously-*empty* destination hopper, its cooldown is set to
//!   `8 - skipTickCount`, where `skipTickCount = 1` only if the source is
//!   also a hopper that has *already ticked this same game tick*
//!   (`tickedGameTime`) — not modeled here (see "not modeled" below).
//!
//! ## What this module does not model
//!
//! * **Face restrictions** (`WorldlyContainer.getSlotsForFace`/
//!   `canPlaceItemThroughFace`/`canTakeItemThroughFace`) — e.g. a furnace
//!   only accepting fuel through its side slot, or a brewing stand only
//!   handing back an empty bottle through the bottom. [`try_move_one_item`]
//!   operates on a flat slot slice with no face/slot-kind concept, since
//!   this crate has no real container-kind registry yet to restrict against
//!   (the same gap `docs/server-inventory.md` already notes for the player
//!   inventory's own container model). A caller wiring a specific container
//!   kind (furnace fuel slot, composter) is expected to pre-filter which
//!   slots it hands to this function.
//! * **Item-entity suction and hopper-to-world drop.** `suckInItems`'s
//!   loose-`ItemEntity` branch and any world-drop-on-full-eject path need an
//!   item-entity registry this crate does not have yet (per the issue's own
//!   scope note: "the last needs the item-entity issue in Phase A"). Also
//!   confirmed directly: `ejectItems` returns `false` outright when there is
//!   no attached container (`:145-147`) — there is **no** fallback
//!   `dropItemStack` anywhere in this file, so "eject into empty air" was
//!   never a thing to model in the first place, only "eject into nothing"
//!   (a no-op), which [`Hopper::tick`] already produces for free when the
//!   caller passes `below: None`.
//! * **The chained-hopper tick-skip** (`skipTickCount`, above) — needs
//!   cross-hopper same-tick ordering this crate's tick loop does not
//!   establish yet; [`Hopper::tick`] always uses the full 8-tick cooldown.
//! * **Hopper minecarts** (`MinecartHopper.java`) — a different entity
//!   entirely (no cooldown field, `isGridAligned() == false`); out of scope
//!   for a block entity issue.

use lodestone_model::ItemStack;

/// `HopperBlockEntity.HOPPER_CONTAINER_SIZE` (`:33`).
pub const HOPPER_SIZE: usize = 5;

/// The literal `8` both cooldown-setting call sites use (see the module doc
/// comment for why this is *not* `MOVE_ITEM_SPEED`).
pub const TRANSFER_COOLDOWN_TICKS: i32 = 8;

/// The assumed per-slot stack cap for merge/full checks — see
/// [`crate::furnace::MAX_STACK_SIZE`]'s doc comment for the same
/// "every item this crate models stacks to 64" simplification.
pub const MAX_STACK_SIZE: u32 = 64;

/// What happened on one [`Hopper::tick`] call.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HopperTick {
    /// One item moved from this hopper into `below`.
    pub ejected: bool,
    /// One item moved from `above` into this hopper.
    pub sucked: bool,
}

impl HopperTick {
    #[must_use]
    pub fn changed(&self) -> bool {
        self.ejected || self.sucked
    }
}

/// A hopper's own 5-slot buffer and transfer cooldown timer.
#[derive(Debug, Clone, PartialEq)]
pub struct Hopper {
    items: [Option<ItemStack>; HOPPER_SIZE],
    /// Ticks remaining before the next transfer attempt; `<= 0` means
    /// "ready this tick" (vanilla's `cooldownTime`, which idles at exactly
    /// `0` rather than drifting negative — see [`Hopper::tick`]).
    cooldown: i32,
}

impl Default for Hopper {
    fn default() -> Self {
        Self {
            items: [const { None }; HOPPER_SIZE],
            // Vanilla's `NO_COOLDOWN_TIME = -1` initial value (`:35,38`) —
            // not on cooldown, ready on the very first tick.
            cooldown: -1,
        }
    }
}

impl Hopper {
    /// A freshly placed, empty hopper, ready to attempt a transfer on its
    /// very first tick.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Rebuilds a hopper from persisted state — see [`Furnace::restore`]'s
    /// own doc comment for why this is one total constructor rather than a
    /// setter per field.
    ///
    /// `cooldown` is vanilla's `TransferCooldown`, which idles at `0` on disk
    /// even though a *freshly placed* hopper starts at `NO_COOLDOWN_TIME =
    /// -1`. Both mean "ready", so loading a vanilla-written `0` is correct
    /// and not a lost tick.
    #[must_use]
    pub fn restore(items: [Option<ItemStack>; HOPPER_SIZE], cooldown: i32) -> Self {
        Self { items, cooldown }
    }

    /// Ticks remaining before the next transfer attempt — vanilla's
    /// `TransferCooldown`, for persistence.
    #[must_use]
    pub fn cooldown(&self) -> i32 {
        self.cooldown
    }

    #[must_use]
    pub fn slots(&self) -> &[Option<ItemStack>; HOPPER_SIZE] {
        &self.items
    }

    /// A mutable view of the same 5 slots as a flat slice — what
    /// [`crate::block_entities::BlockEntityRegistry::tick_hopper`] hands to
    /// *another* hopper's [`tick`](Self::tick) call as its `above`/`below`
    /// container, mirroring vanilla's `Container` interface every adjacent
    /// container implements generically. Kept `pub(crate)` rather than fully
    /// public: it exposes the raw array with no bounds/count invariants
    /// enforced, appropriate for the one in-crate caller that already owns
    /// the whole `Hopper` and is porting vanilla's own slot-by-slot access.
    pub(crate) fn slots_mut(&mut self) -> &mut [Option<ItemStack>] {
        &mut self.items
    }

    pub fn set_slot(&mut self, index: usize, item: Option<ItemStack>) {
        if let Some(slot) = self.items.get_mut(index) {
            *slot = item;
        }
    }

    #[must_use]
    pub fn is_on_cooldown(&self) -> bool {
        self.cooldown > 0
    }

    fn is_empty(&self) -> bool {
        self.items.iter().all(Option::is_none)
    }

    fn is_full(&self) -> bool {
        is_full(&self.items)
    }

    /// Advances by exactly one server tick. `enabled` is the block's
    /// `ENABLED` state (the redstone-lock rule — `true` means *not*
    /// powered, matching vanilla's `!level.hasNeighborSignal(pos)`).
    /// `below`/`above` are the adjacent containers' slot arrays, if any
    /// exist there at all (`None` mirrors vanilla's `getAttachedContainer`/
    /// `getSourceContainer` returning `null` — a no-op, not an error).
    ///
    /// See the module doc comment for the face-restriction and
    /// item-entity scope cuts this does not model.
    pub fn tick(
        &mut self,
        enabled: bool,
        mut below: Option<&mut [Option<ItemStack>]>,
        mut above: Option<&mut [Option<ItemStack>]>,
    ) -> HopperTick {
        // `pushItemsTick`: the decrement is unconditional, every tick.
        self.cooldown -= 1;

        if self.is_on_cooldown() {
            return HopperTick::default();
        }
        // `entity.setCooldown(0)` — pin at exactly 0 rather than drifting
        // further negative while idle.
        self.cooldown = 0;

        let mut out = HopperTick::default();
        if !enabled {
            return out;
        }

        if !self.is_empty() {
            if let Some(below) = below.as_deref_mut() {
                out.ejected = try_move_one_item(&mut self.items, below);
            }
        }
        if !self.is_full() {
            if let Some(above) = above.as_deref_mut() {
                out.sucked = try_move_one_item(above, &mut self.items);
            }
        }

        if out.changed() {
            self.cooldown = TRANSFER_COOLDOWN_TICKS;
        }
        out
    }
}

fn is_full(slots: &[Option<ItemStack>]) -> bool {
    slots.iter().all(|s| matches!(s, Some(stack) if stack.count >= MAX_STACK_SIZE))
}

fn same_item_same_components(a: &ItemStack, b: &ItemStack) -> bool {
    a.item == b.item && a.components == b.components
}

/// Moves **at most one item** from the first eligible non-empty slot in
/// `from` into `to`, mirroring `HopperBlockEntity.addItem`/`tryMoveInItem`
/// (`:282-348`): scans `from` in order, and for the first non-empty slot,
/// tries every slot in `to` in order — landing in the first empty slot
/// outright, or merging one item into the first slot holding the same item
/// with room under [`MAX_STACK_SIZE`]. Returns `true` the moment one item
/// actually moves (matching vanilla's "stop at the first successful slot"
/// behaviour — this is not a bulk transfer).
pub fn try_move_one_item(from: &mut [Option<ItemStack>], to: &mut [Option<ItemStack>]) -> bool {
    for from_slot in from.iter_mut() {
        let Some(source) = from_slot else { continue };

        for dest in to.iter_mut() {
            match dest {
                None => {
                    let mut moved = source.clone();
                    moved.count = 1;
                    *dest = Some(moved);
                    source.count -= 1;
                    if source.count == 0 {
                        *from_slot = None;
                    }
                    return true;
                }
                Some(existing) => {
                    if existing.count < MAX_STACK_SIZE && same_item_same_components(existing, source) {
                        existing.count += 1;
                        source.count -= 1;
                        if source.count == 0 {
                            *from_slot = None;
                        }
                        return true;
                    }
                }
            }
        }
    }
    false
}

/// The dropper-push counterpart to [`try_move_one_item`]: `HopperBlockEntity
/// .addItem`/`tryMoveInItem`, but for a caller that has already chosen its one
/// source item externally rather than scanning a `from` array in slot order.
///
/// `DropperBlock.dispenseFrom` fixes the source slot with
/// `DispenserBlockEntity.getRandomSlot` *before* the container check ever
/// runs (`crate::redstone_dispenser`'s own module doc explains why that
/// ordering makes [`try_move_one_item`] not a drop-in reuse here), then calls
/// `HopperBlockEntity.addItem(blockEntity, into, itemStack.copyWithCount(1),
/// direction.getOpposite())` — always exactly one item. This mirrors that:
/// tries every slot in `to` in order, landing in the first empty slot
/// outright or merging into the first slot holding the same item with room
/// under [`MAX_STACK_SIZE`], and returns `None` once `item` is fully placed.
/// `Some(item)` (unchanged, since a single item cannot partially land) means
/// no slot accepted it — vanilla's own remainder, which `DropperBlock
/// .dispenseFrom` reads as "leave the source stack exactly as it was", **not**
/// as a cue to fall back to a toss (see this module's own doc comment on the
/// face-restriction gap this shares with [`try_move_one_item`]).
#[must_use]
pub fn try_move_item_into(mut item: ItemStack, to: &mut [Option<ItemStack>]) -> Option<ItemStack> {
    for dest in to.iter_mut() {
        match dest {
            None => {
                *dest = Some(item);
                return None;
            }
            Some(existing) => {
                if existing.count < MAX_STACK_SIZE && same_item_same_components(existing, &item) {
                    let space = MAX_STACK_SIZE - existing.count;
                    let moved = item.count.min(space);
                    if moved > 0 {
                        existing.count += moved;
                        item.count -= moved;
                        if item.count == 0 {
                            return None;
                        }
                    }
                }
            }
        }
    }
    Some(item)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(item: &str, count: u32) -> ItemStack {
        ItemStack::new(item.parse().expect("valid resource key"), count)
    }

    #[test]
    fn fresh_hopper_is_empty_and_not_on_cooldown() {
        let h = Hopper::new();
        assert!(!h.is_on_cooldown());
        assert!(h.slots().iter().all(Option::is_none));
    }

    #[test]
    fn try_move_one_item_lands_in_the_first_empty_slot() {
        let mut from = [Some(stack("minecraft:coal", 5)), None];
        let mut to = [None, None, None];
        assert!(try_move_one_item(&mut from, &mut to));
        assert_eq!(to[0], Some(stack("minecraft:coal", 1)));
        assert_eq!(from[0], Some(stack("minecraft:coal", 4)), "exactly one item moved, not the whole stack");
    }

    #[test]
    fn try_move_one_item_merges_into_a_matching_partial_stack() {
        let mut from = [Some(stack("minecraft:coal", 5))];
        let mut to = [Some(stack("minecraft:coal", 10))];
        assert!(try_move_one_item(&mut from, &mut to));
        assert_eq!(to[0], Some(stack("minecraft:coal", 11)));
        assert_eq!(from[0], Some(stack("minecraft:coal", 4)));
    }

    /// **Control**: a destination already at [`MAX_STACK_SIZE`] must not
    /// accept a merge, and a destination holding a *different* item must
    /// not accept one either — proves the scan actually keeps looking (and
    /// eventually fails) rather than merging into the first slot regardless.
    #[test]
    fn try_move_one_item_skips_full_or_mismatched_slots() {
        let mut from = [Some(stack("minecraft:coal", 1))];
        let mut to = [
            Some(stack("minecraft:coal", MAX_STACK_SIZE)),
            Some(stack("minecraft:charcoal", 1)),
        ];
        assert!(!try_move_one_item(&mut from, &mut to), "no eligible destination slot exists");
        assert_eq!(from[0], Some(stack("minecraft:coal", 1)), "nothing must be taken on failure");
        assert_eq!(to[0], Some(stack("minecraft:coal", MAX_STACK_SIZE)));
        assert_eq!(to[1], Some(stack("minecraft:charcoal", 1)));
    }

    #[test]
    fn try_move_one_item_from_an_empty_source_does_nothing() {
        let mut from = [None, None];
        let mut to = [None];
        assert!(!try_move_one_item(&mut from, &mut to));
        assert_eq!(to[0], None);
    }

    /// [`try_move_item_into`]'s happy path: an externally-chosen item lands in
    /// the first empty slot and the caller is told nothing is left over.
    #[test]
    fn try_move_item_into_lands_in_the_first_empty_slot() {
        let mut to = [Some(stack("minecraft:charcoal", 1)), None, None];
        let leftover = try_move_item_into(stack("minecraft:coal", 1), &mut to);
        assert_eq!(leftover, None);
        assert_eq!(to[1], Some(stack("minecraft:coal", 1)), "skips the mismatched first slot");
        assert_eq!(to[0], Some(stack("minecraft:charcoal", 1)), "untouched");
    }

    /// A destination with a matching partial stack merges rather than using a
    /// later empty slot — proves the scan does not stop at "any" slot.
    #[test]
    fn try_move_item_into_merges_into_a_matching_partial_stack() {
        let mut to = [Some(stack("minecraft:coal", 10)), None];
        let leftover = try_move_item_into(stack("minecraft:coal", 1), &mut to);
        assert_eq!(leftover, None);
        assert_eq!(to[0], Some(stack("minecraft:coal", 11)));
        assert_eq!(to[1], None, "the empty slot must not be used when a merge succeeds first");
    }

    /// **Control, the discriminating "full or absent" pair's full half**: every
    /// slot full or mismatched hands the whole item straight back, unchanged —
    /// this is what `DropperBlock.dispenseFrom` reads as "leave the source
    /// slot exactly as it was", not as a cue to toss.
    #[test]
    fn try_move_item_into_a_full_container_returns_the_item_untouched() {
        let mut to = [
            Some(stack("minecraft:coal", MAX_STACK_SIZE)),
            Some(stack("minecraft:charcoal", 3)),
        ];
        let leftover = try_move_item_into(stack("minecraft:coal", 1), &mut to);
        assert_eq!(leftover, Some(stack("minecraft:coal", 1)), "nothing accepted it");
        assert_eq!(to[0], Some(stack("minecraft:coal", MAX_STACK_SIZE)), "untouched");
        assert_eq!(to[1], Some(stack("minecraft:charcoal", 3)), "untouched");
    }

    /// An empty `to` (the "absent container" half of the discriminating pair
    /// is a caller-side check — see `crate::redstone_dispenser`'s own doc
    /// comment — but an empty slice is the degenerate case of "nothing
    /// accepts it" this function itself must still get right).
    #[test]
    fn try_move_item_into_an_empty_slice_returns_the_item() {
        let mut to: [Option<ItemStack>; 0] = [];
        assert_eq!(try_move_item_into(stack("minecraft:coal", 1), &mut to), Some(stack("minecraft:coal", 1)));
    }

    /// A hopper above an empty chest, with one item, sucks it down on the
    /// very first tick (fresh hopper starts ready, not on cooldown).
    #[test]
    fn sucks_one_item_from_above_on_first_ready_tick() {
        let mut h = Hopper::new();
        let mut above = [Some(stack("minecraft:diamond", 3)), None, None];
        let tick = h.tick(true, None, Some(&mut above));
        assert!(tick.sucked);
        assert!(!tick.ejected);
        assert_eq!(h.slots()[0], Some(stack("minecraft:diamond", 1)));
        assert_eq!(above[0], Some(stack("minecraft:diamond", 2)));
    }

    /// The exact cooldown cadence: after a successful transfer, the next
    /// attempt is not tick 1 later but exactly [`TRANSFER_COOLDOWN_TICKS`]
    /// (8) ticks later — not "eventually", the precise tick.
    #[test]
    fn next_transfer_attempt_is_exactly_8_ticks_after_a_success() {
        let mut h = Hopper::new();
        let mut above = [Some(stack("minecraft:diamond", 10))];

        let first = h.tick(true, None, Some(&mut above));
        assert!(first.sucked, "expected the fresh hopper's first tick to succeed");

        // Ticks 2..=8 (7 ticks) must all be on cooldown and move nothing.
        for t in 2..=8 {
            let tick = h.tick(true, None, Some(&mut above));
            assert!(!tick.changed(), "unexpected transfer at tick {t}");
        }
        assert_eq!(above[0], Some(stack("minecraft:diamond", 9)), "still only 1 item moved total");

        // Tick 9 is exactly 8 ticks after tick 1 — ready again.
        let tick = h.tick(true, None, Some(&mut above));
        assert!(tick.sucked, "expected the next attempt at exactly tick 9");
        assert_eq!(above[0], Some(stack("minecraft:diamond", 8)));
    }

    /// **Control**: with nothing to move and nothing adjacent, a hopper
    /// stays ready every tick (cooldown pinned at 0, never drifting so far
    /// negative that it would take multiple ticks to "recover") — proves
    /// the idle case is really a no-op every tick, not merely never tested.
    #[test]
    fn idle_hopper_with_nothing_to_move_never_goes_on_cooldown() {
        let mut h = Hopper::new();
        for t in 0..20 {
            assert!(!h.is_on_cooldown(), "unexpectedly on cooldown at tick {t}");
            let tick = h.tick(true, None, None);
            assert!(!tick.changed());
        }
    }

    #[test]
    fn ejects_one_item_into_a_container_below() {
        let mut h = Hopper::new();
        h.set_slot(0, Some(stack("minecraft:redstone", 4)));
        let mut below = [None, None];

        let tick = h.tick(true, Some(&mut below), None);
        assert!(tick.ejected);
        assert_eq!(below[0], Some(stack("minecraft:redstone", 1)));
        assert_eq!(h.slots()[0], Some(stack("minecraft:redstone", 3)));
    }

    /// A hopper both ejects into `below` *and* sucks from `above` in the
    /// same ready tick — they are independent attempts, not
    /// mutually exclusive (`tryMoveItems` ORs the two outcomes together).
    #[test]
    fn ejects_and_sucks_in_the_same_tick() {
        let mut h = Hopper::new();
        h.set_slot(0, Some(stack("minecraft:redstone", 1)));
        let mut below = [None];
        let mut above = [Some(stack("minecraft:diamond", 1))];

        let tick = h.tick(true, Some(&mut below), Some(&mut above));
        assert!(tick.ejected);
        assert!(tick.sucked);
        assert_eq!(below[0], Some(stack("minecraft:redstone", 1)));
        assert_eq!(h.slots()[0], Some(stack("minecraft:diamond", 1)));
        assert_eq!(above[0], None);
    }

    /// **Control**: a disabled (redstone-locked) hopper must never
    /// transfer, even with an eligible source and destination and no
    /// cooldown — proves `enabled` really gates, not merely "nothing else
    /// happened to be blocking it" in the other tests.
    #[test]
    fn disabled_hopper_never_transfers() {
        let mut h = Hopper::new();
        let mut above = [Some(stack("minecraft:diamond", 1))];
        for t in 0..20 {
            let tick = h.tick(false, None, Some(&mut above));
            assert!(!tick.changed(), "disabled hopper transferred at tick {t}");
        }
        assert_eq!(above[0], Some(stack("minecraft:diamond", 1)));
    }

    /// **Control**: the cooldown still ticks down even while disabled
    /// (`pushItemsTick`'s decrement is unconditional) — re-enabling must not
    /// find a hopper artificially still "on cooldown" from before it was
    /// disabled.
    #[test]
    fn cooldown_still_decrements_while_disabled() {
        let mut h = Hopper::new();
        let mut above = [Some(stack("minecraft:diamond", 5))];
        assert!(h.tick(true, None, Some(&mut above)).sucked);
        assert!(h.is_on_cooldown());

        // Disabled for the next several ticks — cooldown must still be
        // draining underneath, not frozen at 8.
        for _ in 0..7 {
            h.tick(false, None, Some(&mut above));
        }
        // Re-enable on what would have been the 8th tick since the success.
        let tick = h.tick(true, None, Some(&mut above));
        assert!(tick.sucked, "cooldown must have fully drained despite being disabled meanwhile");
    }

    #[test]
    fn full_hopper_still_ejects_but_does_not_suck() {
        let mut h = Hopper::new();
        for i in 0..HOPPER_SIZE {
            h.set_slot(i, Some(stack("minecraft:cobblestone", MAX_STACK_SIZE)));
        }
        // `below` has room, so the eject half of the attempt can succeed;
        // `above` offers a diamond the (still-full-after-ejecting-one)
        // hopper must not accept.
        let mut below = [None];
        let mut above = [Some(stack("minecraft:diamond", 1))];

        let tick = h.tick(true, Some(&mut below), Some(&mut above));
        assert!(tick.ejected, "a full hopper must still be able to eject");
        assert_eq!(below[0], Some(stack("minecraft:cobblestone", 1)));
        assert!(!tick.sucked, "a full hopper must not suck in more items");
        assert_eq!(above[0], Some(stack("minecraft:diamond", 1)), "untouched");
    }

    /// **Control**: the same scenario but with `below` also full — now
    /// neither half of the attempt has anywhere to land, proving `ejected`
    /// in the test above was really driven by `below` having room rather
    /// than always being true regardless of the destination.
    #[test]
    fn full_hopper_with_full_destination_does_nothing() {
        let mut h = Hopper::new();
        for i in 0..HOPPER_SIZE {
            h.set_slot(i, Some(stack("minecraft:cobblestone", MAX_STACK_SIZE)));
        }
        let mut below = [Some(stack("minecraft:cobblestone", MAX_STACK_SIZE))];
        let mut above = [Some(stack("minecraft:diamond", 1))];

        let tick = h.tick(true, Some(&mut below), Some(&mut above));
        assert!(!tick.ejected);
        assert!(!tick.sucked);
    }
}
