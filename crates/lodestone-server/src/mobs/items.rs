//! `MobSim`'s dropped-item slice — spawn, per-tick merge and the item query/
//! pickup API. Moved out of `mobs/mod.rs` verbatim as part of the `mobs.rs`
//! file split (see `docs/plans/crate-and-file-splits.md`).

use lodestone_entity::item_entity::{ItemLifecycle, ItemMotion};
use std::sync::Arc;

use lodestone_model::{ItemComponents, ItemStack, ResourceKey, Vec3};
use uuid::Uuid;

use super::{ItemState, MobSim};

/// Horizontal reach of `mergeWithNeighbours`' search: the item's own half-width
/// on both boxes plus vanilla's `inflate(0.5, …, 0.5)`.
const ITEM_MERGE_REACH_XZ: f64 = 0.125 + 0.5 + 0.125;

/// Vertical reach of the same search. Vanilla inflates y by **`0.0`**, so this is
/// nothing but the two 0.25-tall boxes overlapping — see
/// [`MobSim::merge_neighbouring_items`].
const ITEM_MERGE_REACH_Y: f64 = 0.25;

/// What a dropped item entity holds, apart from its count (which lives on its
/// lifecycle): the item and, when it has any, its components. A bare
/// [`ResourceKey`] converts to a plain stack; an [`ItemStack`] keeps its
/// components, so an enchanted sword thrown out stays enchanted.
#[derive(Debug, Clone, PartialEq)]
pub struct DroppedItem {
    pub item: ResourceKey,
    pub components: Option<Arc<ItemComponents>>,
}

impl From<ResourceKey> for DroppedItem {
    fn from(item: ResourceKey) -> Self {
        Self { item, components: None }
    }
}

impl From<&ItemStack> for DroppedItem {
    fn from(stack: &ItemStack) -> Self {
        let components = (stack.components != ItemComponents::default()).then(|| Arc::new(stack.components.clone()));
        Self { item: stack.item.clone(), components }
    }
}

impl From<ItemStack> for DroppedItem {
    fn from(stack: ItemStack) -> Self {
        Self::from(&stack)
    }
}

impl<'w> MobSim<'w> {
    /// Registers a dropped item entity at `position` with fall velocity
    /// `velocity` and lifecycle `lifecycle` (typically
    /// [`ItemLifecycle::newly_dropped`]). [`tick`](Self::tick) advances its
    /// age and pickup delay every server tick, removes it on despawn, and
    /// updates the item state sent to the client.
    ///
    /// Returns the assigned entity id. Player-overlap pickup and merging
    /// adjacent stacks (via [`ItemEntityRegistry::merge`]) are separate
    /// operations that require player positions.
    pub fn spawn_item(
        &mut self,
        item: impl Into<DroppedItem>,
        position: Vec3,
        velocity: Vec3,
        lifecycle: ItemLifecycle,
    ) -> i32 {
        let DroppedItem { item, components } = item.into();
        let id = self.next_id;
        self.next_id += 1;
        self.items.spawn(id, lifecycle);
        self.item_state.insert(
            id,
            ItemState {
                uuid: Uuid::new_v4(),
                item,
                components,
                motion: ItemMotion::new(position, velocity),
                owner: super::ItemTickOwner::for_position(position),
            },
        );
        id
    }

    /// Removes a tracked dropped item (pickup or manual despawn).
    ///
    /// Returns whether an item was actually tracked under `id`.
    pub fn remove_item(&mut self, id: i32) -> bool {
        self.item_state.remove(&id);
        self.item_handoff.forget_entity(id);
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
    #[cfg(test)]
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
    /// A pickup hands the entity's `ItemStack` to the inventory, which shrinks
    /// it in place; the entity is discarded only
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

    /// Merges dropped items that have drifted together — vanilla's own
    /// per-tick merge-with-neighbours call, the other consumer
    /// [`ItemEntityRegistry::merge`] was missing.
    ///
    /// Vanilla's search box is get bounding box's get bounding box, and the
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
                // Vanilla merges only stacks that are the same item with the
                // same components: two differently enchanted swords stay apart.
                if to.item != from.item || to.components != from.components {
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
                    self.item_handoff.forget_entity(from_id);
                }
            }
        }
    }

    /// Every dropped item a player standing at `player_feet` may collect, as
    /// `(entity id, stack)` — the pickup half.
    ///
    /// Two filters, and both are vanilla:
    ///
    /// * [`crate::block_drops::is_within_pickup_range`] is Player's ai step's
    ///   inflated-AABB intersection, not a radius (see its own doc comment).
    /// * [`ItemLifecycle::can_be_picked_up`] checks `pickup_delay == 0`. A
    ///   freshly popped block drop carries
    ///   [`crate::block_drops::DEFAULT_PICKUP_DELAY`] (10 ticks), so an item
    ///   is **not** collectable on the tick it spawns — a pickup gate that
    ///   asserts immediately reads that as a broken feature. Advance the tick
    ///   clock first.
    ///
    /// Read-only: the caller decides what it can actually fit and then calls
    /// [`remove_item`](Self::remove_item) for the ones it took. Splitting the
    /// query from the removal is what lets a connection roll back cleanly when
    /// its inventory is full — remove the entity only after the inventory
    /// accepts the complete stack.
    #[must_use]
    pub fn items_within_pickup_range(&self, player_feet: Vec3) -> Vec<(i32, ItemStack)> {
        let mut collectable: Vec<(i32, ItemStack)> = self
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
                (id, state.stack(count))
            })
            .collect();
        // `item_state` is a `HashMap`, so its iteration order is unspecified and
        // varies run to run. Sorting by id makes a multi-item pickup deterministic
        // — without this, which of two overlapping drops lands in the selected
        // hotbar slot first is a coin flip, and a test asserting slot contents
        // would be intermittently red for reasons that look nothing like the
        // cause.
        collectable.sort_by_key(|(id, _)| *id);
        collectable
    }
}

impl ItemState {
    /// The stack this entity holds, at the lifecycle's `count`.
    pub(super) fn stack(&self, count: u8) -> ItemStack {
        let mut stack = ItemStack::new(self.item.clone(), u32::from(count));
        if let Some(components) = &self.components {
            stack.components = (**components).clone();
        }
        stack
    }
}

#[cfg(test)]
mod tests {
    use lodestone_entity::item_entity::ItemLifecycle;
    use lodestone_model::{ItemEnchantment, ItemStack, Vec3};

    use crate::mobs::MobSim;

    fn sharp_sword() -> ItemStack {
        let mut sword = ItemStack::new("minecraft:diamond_sword".parse().unwrap(), 1);
        sword.components.enchantments = vec![ItemEnchantment {
            id: crate::enchantment_data::id_of("minecraft:sharpness").unwrap(),
            level: 5,
        }];
        sword
    }

    fn lifecycle() -> ItemLifecycle {
        ItemLifecycle { pickup_delay: 0, ..ItemLifecycle::newly_dropped(1, 64) }
    }

    /// A thrown enchanted sword lands beside a plain one: they stay two
    /// entities, the client is told about the enchantment, and the pickup
    /// hands back the enchanted stack, not a plain sword.
    #[test]
    fn a_dropped_stack_keeps_its_components() {
        let world = crate::ChunkWorld::new(-64, 384);
        let mut sim = MobSim::new(&world);
        let at = Vec3::new(0.5, 64.0, 0.5);
        let sharp = sim.spawn_item(&sharp_sword(), at, Vec3::default(), lifecycle());
        let plain = sim.spawn_item(
            "minecraft:diamond_sword".parse::<lodestone_model::ResourceKey>().unwrap(),
            Vec3::new(0.6, 64.0, 0.5),
            Vec3::default(),
            lifecycle(),
        );
        sim.merge_neighbouring_items();
        assert_eq!(sim.item_count(), 2, "differently enchanted swords never merge");

        let shown = sim
            .snapshots()
            .into_iter()
            .find(|snapshot| snapshot.id == sharp)
            .expect("the sword is streamed");
        assert!(matches!(
            shown.metadata.as_slice(),
            [crate::protocol::MetadataField::Item { components: Some(components), .. }]
                if components.enchantments == sharp_sword().components.enchantments
        ));

        let picked = sim.items_within_pickup_range(at);
        let by_id = |id| picked.iter().find(|(entity, _)| *entity == id).map(|(_, stack)| stack.clone());
        assert_eq!(by_id(sharp), Some(sharp_sword()));
        assert_eq!(by_id(plain).map(|stack| stack.components), Some(Default::default()));
    }

    /// Saving and reloading a dropped stack keeps its components.
    #[test]
    fn a_saved_dropped_stack_reloads_with_its_components() {
        let world = crate::ChunkWorld::new(-64, 384);
        let mut sim = MobSim::new(&world);
        sim.spawn_item(&sharp_sword(), Vec3::new(0.5, 64.0, 0.5), Vec3::default(), lifecycle());
        let saved: Vec<_> = sim
            .saved_entities()
            .into_iter()
            .map(|entity| crate::entity_record::SavedEntity::from_nbt(&entity.to_nbt()).expect("reads back"))
            .collect();
        let mut reloaded = MobSim::new(&world);
        assert_eq!(reloaded.restore_saved(&saved), 1);
        let stacks: Vec<ItemStack> = reloaded
            .items_within_pickup_range(Vec3::new(0.5, 64.0, 0.5))
            .into_iter()
            .map(|(_, stack)| stack)
            .collect();
        assert_eq!(stacks, vec![sharp_sword()]);
    }
}
