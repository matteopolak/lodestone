//! `MobSim`'s dropped-item slice — spawn, per-tick merge and the item query/
//! pickup API. Moved out of `mobs/mod.rs` verbatim as part of the `mobs.rs`
//! file split (see `docs/plans/crate-and-file-splits.md`).

use lodestone_entity::item_entity::{ItemLifecycle, ItemMotion};
use lodestone_model::{ResourceKey, Vec3};
use uuid::Uuid;

use super::{ItemState, MobSim};

/// Horizontal reach of `mergeWithNeighbours`' search: the item's own half-width
/// on both boxes plus vanilla's `inflate(0.5, …, 0.5)`.
const ITEM_MERGE_REACH_XZ: f64 = 0.125 + 0.5 + 0.125;

/// Vertical reach of the same search. Vanilla inflates y by **`0.0`**, so this is
/// nothing but the two 0.25-tall boxes overlapping — see
/// [`MobSim::merge_neighbouring_items`].
const ITEM_MERGE_REACH_Y: f64 = 0.25;

impl<'w> MobSim<'w> {
    /// Registers a dropped item entity at `position` with fall velocity
    /// `velocity` and lifecycle `lifecycle` (typically
    /// [`ItemLifecycle::newly_dropped`]) so [`tick`](Self::tick) advances its
    /// age/pickup-delay every server tick (and removes it on despawn) — the
    /// missing driver issue #215 found: `ItemEntityRegistry`'s lifecycle had
    /// no production consumer, only the client-side fall dynamics
    /// (`ItemMotion`) reached anything, and purely for rendering.
    ///
    /// Returns the assigned entity id. Deciding *pickup* on player-overlap and
    /// merging adjacent stacks (via [`ItemEntityRegistry::merge`]) are the
    /// caller's job once it has player positions to test against — this
    /// closes the "nothing ticks the lifecycle at all" island, not the full
    /// pickup feature.
    pub fn spawn_item(
        &mut self,
        item: ResourceKey,
        position: Vec3,
        velocity: Vec3,
        lifecycle: ItemLifecycle,
    ) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        self.items.spawn(id, lifecycle);
        self.item_state.insert(
            id,
            ItemState {
                uuid: Uuid::new_v4(),
                item,
                motion: ItemMotion::new(position, velocity),
            },
        );
        id
    }

    /// Removes a tracked dropped item (pickup or manual despawn).
    ///
    /// Returns whether an item was actually tracked under `id`.
    pub fn remove_item(&mut self, id: i32) -> bool {
        self.item_state.remove(&id);
        self.items.remove(id).is_some()
    }

    /// The number of tracked dropped items.
    #[must_use]
    pub fn item_count(&self) -> usize {
        self.item_state.len()
    }

    /// The current position of a tracked dropped item, if any.
    #[must_use]
    pub fn item_position(&self, id: i32) -> Option<Vec3> {
        self.item_state.get(&id).map(|s| s.motion.position)
    }

    /// Every tracked dropped item as `(item id, count)`, in arbitrary order —
    /// the pair a caller needs to ask "what did that death drop".
    #[must_use]
    pub fn dropped_items(&self) -> Vec<(String, u8)> {
        self.item_state
            .iter()
            .map(|(id, state)| {
                let count = self.items.get(*id).map_or(0, |lifecycle| lifecycle.count);
                (state.item.to_string(), count)
            })
            .collect()
    }

    /// The current age/pickup-delay/count lifecycle of a tracked dropped
    /// item, if any.
    #[must_use]
    pub fn item_lifecycle(&self, id: i32) -> Option<&ItemLifecycle> {
        self.items.get(id)
    }

    /// Shrinks a tracked dropped item to `count`, for a **partial** pickup.
    ///
    /// Vanilla's `ItemEntity.playerTouch` hands the entity's own `ItemStack` to
    /// `Inventory.add`, which shrinks it in place; the entity is discarded only
    /// when the stack ends up empty. So a player with one free slot walking over
    /// a stack of 40 when 30 fit banks 30 and leaves an entity holding 10 —
    /// *not* nothing, and not the whole 40.
    ///
    /// Returns whether an item was tracked under `id`. A `count` of `0` is left
    /// to the caller to turn into a [`remove_item`](Self::remove_item); this
    /// setter does not implicitly delete, so "shrink to zero" cannot silently
    /// leak a zero-count entity that streams forever.
    ///
    /// Implemented as a remove-and-respawn **at the same id** rather than a
    /// mutating setter on [`ItemEntityRegistry`], which exposes none: that type
    /// lives in `lodestone-entity` and re-registering preserves the network id,
    /// so a client mid-`ADD_ENTITY` does not see the stack become a different
    /// entity. `age` and `pickup_delay` are carried over deliberately — a
    /// partial pickup must not reset the despawn clock, or a stack a full player
    /// keeps brushing past would live forever.
    pub fn set_item_count(&mut self, id: i32, count: u8) -> bool {
        let Some(tracked) = self.items.remove(id) else {
            return false;
        };
        self.items.spawn(
            id,
            ItemLifecycle {
                count,
                ..tracked.lifecycle
            },
        );
        true
    }

    /// Merges dropped items that have drifted together — `ItemEntity.tick`'s
    /// `mergeWithNeighbours()` (`ItemEntity.java`), the other consumer
    /// [`ItemEntityRegistry::merge`] was missing.
    ///
    /// Vanilla's search box is `getBoundingBox().inflate(0.5, 0.0, 0.5)`, and the
    /// **`0.0` vertical inflation is the load-bearing part**: two stacks side by
    /// side merge, two stacks a block apart vertically never do, however close
    /// they are horizontally. Since both boxes are the item's own 0.25 cube that
    /// works out to a horizontal reach of `0.125 + 0.5 + 0.125 = 0.75` and a
    /// vertical overlap of `|dy| < 0.25`. Using one isotropic radius here would
    /// silently merge a drop with one sitting on the block below it.
    ///
    /// Mergeability itself is [`ItemLifecycle::is_mergable`] (vanilla's
    /// `isMergable`: not the never-pickup sentinel, not infinite-age, under the
    /// despawn age, and not already a full stack) plus the same-item test, which
    /// lives here because [`ItemEntityRegistry`] is deliberately identity-free.
    // `pub(super)`, not private: `tick_with_terrain` (mod.rs, the core tick
    // loop) calls this every tick, and mod.rs is this file's *parent* module —
    // the one direction a plain `fn` here cannot reach.
    pub(super) fn merge_neighbouring_items(&mut self) {
        // Snapshot to a sorted id list first: merging mutates both registries,
        // and iteration order over a `HashMap` would otherwise make which of
        // three touching stacks absorbs the others vary run to run.
        let mut ids: Vec<i32> = self.item_state.keys().copied().collect();
        ids.sort_unstable();
        for i in 0..ids.len() {
            let to_id = ids[i];
            for j in (i + 1)..ids.len() {
                let from_id = ids[j];
                let (Some(to), Some(from)) =
                    (self.item_state.get(&to_id), self.item_state.get(&from_id))
                else {
                    continue;
                };
                if to.item != from.item {
                    continue;
                }
                let mergable = |id: i32| {
                    self.items
                        .get(id)
                        .is_some_and(ItemLifecycle::is_mergable)
                };
                if !mergable(to_id) || !mergable(from_id) {
                    continue;
                }
                let a = to.motion.position;
                let b = from.motion.position;
                if (a.x - b.x).abs() >= ITEM_MERGE_REACH_XZ
                    || (a.z - b.z).abs() >= ITEM_MERGE_REACH_XZ
                    || (a.y - b.y).abs() >= ITEM_MERGE_REACH_Y
                {
                    continue;
                }
                if self.items.merge(to_id, from_id) && self.items.get(from_id).is_none() {
                    // The source was fully absorbed, so its wire identity must go
                    // too — otherwise `snapshots()` keeps streaming a stack the
                    // lifecycle registry has already forgotten, and the client
                    // sees a permanent ghost item that never despawns.
                    self.item_state.remove(&from_id);
                }
            }
        }
    }

    /// Every dropped item a player standing at `player_feet` may collect right
    /// now, as `(entity id, item, count)` — issue #337's pickup half.
    ///
    /// Two filters, and both are vanilla:
    ///
    /// * [`crate::block_drops::is_within_pickup_range`] is `Player.aiStep`'s
    ///   inflated-AABB intersection, not a radius (see its own doc comment).
    /// * [`ItemLifecycle::can_be_picked_up`] is `ItemEntity.playerTouch`'s
    ///   `this.pickupDelay == 0` guard. A freshly popped block drop carries
    ///   [`crate::block_drops::DEFAULT_PICKUP_DELAY`] (10 ticks), so an item
    ///   is **not** collectable on the tick it spawns — a pickup gate that
    ///   asserts immediately reads that as a broken feature. Advance the tick
    ///   clock first.
    ///
    /// Read-only: the caller decides what it can actually fit and then calls
    /// [`remove_item`](Self::remove_item) for the ones it took. Splitting the
    /// query from the removal is what lets a connection roll back cleanly when
    /// its inventory is full — vanilla's `playerTouch` likewise only removes
    /// the entity once `getInventory().add(...)` succeeded.
    #[must_use]
    pub fn items_within_pickup_range(&self, player_feet: Vec3) -> Vec<(i32, ResourceKey, u8)> {
        let mut collectable: Vec<(i32, ResourceKey, u8)> = self
            .item_state
            .iter()
            .filter(|(id, state)| {
                crate::block_drops::is_within_pickup_range(player_feet, state.motion.position)
                    && self
                        .items
                        .get(**id)
                        .is_some_and(ItemLifecycle::can_be_picked_up)
            })
            .map(|(&id, state)| {
                let count = self.items.get(id).map_or(1, |lifecycle| lifecycle.count);
                (id, state.item.clone(), count)
            })
            .collect();
        // `item_state` is a `HashMap`, so its iteration order is unspecified and
        // varies run to run. Sorting by id makes a multi-item pickup deterministic
        // — without this, which of two overlapping drops lands in the selected
        // hotbar slot first is a coin flip, and a test asserting slot contents
        // would be intermittently red for reasons that look nothing like the
        // cause.
        collectable.sort_by_key(|&(id, _, _)| id);
        collectable
    }
}
