//! The [`BlockPos`]-keyed registry that gives the four block-entity
//! simulations (`composter`/`furnace`/`hopper`/`brewing`, `docs/block-entities.md`)
//! somewhere to live in a running world — the first of that doc's three named
//! gaps.
//!
//! Before this module, all four types were plain values a test could
//! construct and tick, but nothing kept one alive at a world position:
//! `docs/block-entities.md`'s own words, "there is no `HashMap<BlockPos, T>`
//! anywhere in this crate's chunk model." [`BlockEntityRegistry`] is that map,
//! an enum ([`BlockEntity`]) over the four existing types rather than four
//! separate maps — matching the doc's own suggestion.
//!
//! [`BlockEntityHandle`] is the `Arc<Mutex<_>>`-backed, `Clone`, shareable
//! handle a caller threads between a connection's own task (which needs to
//! *insert* a fresh entry on placement — see `crate::server`'s
//! `apply_use_item_on`) and a background tick-loop task (which needs to
//! *advance* every entry once a tick) — the exact shape
//! [`crate::mobs::LiveMobSource`] already established for the mob simulation,
//! reused here rather than reinvented.
//!
//! # What is deliberately not modeled yet
//!
//! * ~~**No redstone/power model.**~~ **Fixed (issue #321).** This claim was
//!   true when written and is not any more, which is why it is struck through
//!   rather than deleted: `crate::redstone::best_neighbor_signal` is the
//!   `hasNeighborSignal` equivalent, and
//!   [`BlockEntityRegistry::tick_all_with_hopper_lock`] takes each hopper's
//!   `enabled` from the caller. `crate::random_tick` maintains that property on
//!   the block state, as `HopperBlock.checkPoweredState` does. Note the
//!   plain [`tick_all`](BlockEntityRegistry::tick_all) shorthand still ticks
//!   every hopper unlocked, so a production caller that holds a world must use
//!   the locking form.
//! * **Hopper adjacency only resolves another hopper.** A real container
//!   (chest, furnace slots) at the adjacent position is not something this
//!   crate can hand a hopper today: [`Hopper::tick`](crate::hopper::Hopper::tick)
//!   wants a flat `&mut [Option<ItemStack>]` slice, and only [`Hopper`] itself
//!   exposes one — a furnace's input/fuel/output are three separate typed
//!   fields, and there is no chest block entity in this crate at all. So
//!   [`BlockEntity::hopper_slots_mut`] answers `None` for every non-hopper
//!   variant, which is an honest "no adjacency support yet," not a bug: a
//!   hopper next to a furnace simply never transfers, same as a hopper next
//!   to open air. Extending this needs a `Container`-shaped seam over the
//!   furnace's three slots, deliberately not built here.
//! * **No visual sync.** Ticking a furnace lit/unlit or a composter to
//!   ready does not write anything back through [`crate::chunk::ChunkSource`]
//!   — the block state a client is streamed does not reflect
//!   [`Furnace::is_lit`](crate::furnace::Furnace::is_lit) or
//!   [`Composter::is_ready`](crate::composter::Composter::is_ready). That is
//!   a real, separate gap (closing it needs `ChunkSource::set_block`, which
//!   this module has no handle to), not attempted here — this module's job
//!   is *simulating*, not *rendering*.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use lodestone_core::Nbt;
use lodestone_model::{BlockPos, ItemStack};

use crate::brewing::BrewingStand;
use crate::composter::Composter;
use crate::furnace::{Furnace, FurnaceKind};
use crate::hopper::Hopper;

/// One of the four block-entity kinds this crate simulates, at rest in the
/// [`BlockEntityRegistry`].
#[derive(Debug, Clone, PartialEq)]
pub enum BlockEntity {
    /// `minecraft:composter`.
    Composter(Composter),
    /// `minecraft:furnace`/`minecraft:smoker`/`minecraft:blast_furnace`
    /// (the [`FurnaceKind`] inside [`Furnace`] distinguishes them).
    Furnace(Furnace),
    /// `minecraft:hopper`.
    Hopper(Hopper),
    /// `minecraft:brewing_stand`.
    BrewingStand(BrewingStand),
    /// A block entity this crate has no simulation for (chest, spawner, vault, …).
    /// The vanilla id and the full NBT compound are preserved verbatim so the entity
    /// round-trips through a save/load cycle unchanged.
    Opaque { id: String, nbt: Nbt },
}

impl BlockEntity {
    /// A mutable view of this entity's flat item-slot array, if it has one
    /// shaped that way — today, only [`Hopper`]. See the module doc comment's
    /// "hopper adjacency" scope note for why every other variant answers
    /// `None` rather than something partial/misleading.
    fn hopper_slots_mut(&mut self) -> Option<&mut [Option<ItemStack>]> {
        match self {
            BlockEntity::Hopper(h) => Some(h.slots_mut()),
            BlockEntity::Composter(_) | BlockEntity::Furnace(_) | BlockEntity::BrewingStand(_)
            | BlockEntity::Opaque { .. } => {
                None
            }
        }
    }

    /// The vanilla `minecraft:*` menu identifier this entity's container
    /// screen opens as, or `None` if it has no menu screen at all.
    ///
    /// Two of the four kinds answer `None`, for two different reasons —
    /// stated here rather than left to be discovered by a missing
    /// `open_screen`:
    /// * **[`Composter`] has no vanilla menu at all.** `ComposterBlock`'s own
    ///   `useItemOn`/`useWithoutItem` add or empty one item per click
    ///   directly against the block entity — there is no `AbstractContainerMenu`
    ///   subclass for it anywhere in `.cache/mc/26.2/src/net/minecraft/world/inventory/`,
    ///   unlike every other block entity here. A right-click on a composter is
    ///   therefore never a "screen" question at all; it needs its own
    ///   serverbound handling, not this module's.
    /// * **[`BrewingStand`] has a real vanilla menu (`BrewingStandMenu`,
    ///   5 slots: 3 potion bottles + ingredient + fuel) but this crate cannot
    ///   open it yet**, because its slots are not [`ItemStack`]s —
    ///   `docs/block-entities.md`'s second named gap: "the brewing stand's
    ///   `Bottle` is not a real `ItemStack`" (no potion-contents component
    ///   anywhere in `lodestone_model::ItemComponents`). Every wire encoder
    ///   this module feeds ([`container_slots`](Self::container_slots),
    ///   [`CONTAINER_SET_CONTENT`]) speaks `Option<ItemStack>` only, so a
    ///   brewing stand has nothing valid to put in that list — opening one
    ///   would need either a real potion-contents model or a second,
    ///   bottle-shaped wire path this crate does not have. Real, deliberate
    ///   scope cut, not an oversight.
    #[must_use]
    pub fn menu_name(&self) -> Option<&'static str> {
        match self {
            BlockEntity::Furnace(f) => Some(match f.kind() {
                FurnaceKind::Furnace => "minecraft:furnace",
                FurnaceKind::Smoker => "minecraft:smoker",
                FurnaceKind::BlastFurnace => "minecraft:blast_furnace",
            }),
            BlockEntity::Hopper(_) => Some("minecraft:hopper"),
            BlockEntity::Composter(_) | BlockEntity::BrewingStand(_) | BlockEntity::Opaque { .. } => None,
        }
    }

    /// This entity's own container slots, in vanilla menu order — the
    /// furnace's `[input, fuel, output]` (`AbstractFurnaceMenu.java:63-65`,
    /// `INGREDIENT_SLOT`/`FUEL_SLOT`/`RESULT_SLOT` = `0`/`1`/`2`) or the
    /// hopper's 5 flat slots (`HopperMenu.java:23-24`). Empty for a variant
    /// with [`menu_name`](Self::menu_name) `None` — nothing should ever call
    /// this for one, but an empty list is the honest answer if something
    /// does, not a panic.
    #[must_use]
    pub fn container_slots(&self) -> Vec<Option<ItemStack>> {
        match self {
            BlockEntity::Furnace(f) => vec![f.input().cloned(), f.fuel().cloned(), f.output().cloned()],
            BlockEntity::Hopper(h) => h.slots().to_vec(),
            BlockEntity::Composter(_) | BlockEntity::BrewingStand(_) | BlockEntity::Opaque { .. } => Vec::new(),
        }
    }

    /// Writes one container slot by its position in
    /// [`container_slots`](Self::container_slots)'s own ordering — the
    /// counterpart a `container_click` consumer needs to apply the client's
    /// predicted diff verbatim (`docs/server-inventory.md`'s established
    /// scope: this crate applies the click's own diff rather than re-running
    /// vanilla's `doClick` state machine server-side). An out-of-range
    /// `slot` is a silent no-op, matching `PlayerInventory::set_native`'s own
    /// convention for the identical "malformed index" case.
    pub fn set_container_slot(&mut self, slot: usize, item: Option<ItemStack>) {
        match self {
            BlockEntity::Furnace(f) => match slot {
                0 => f.set_input(item),
                1 => f.set_fuel(item),
                2 => f.set_output(item),
                _ => {}
            },
            BlockEntity::Hopper(h) => {
                if slot < h.slots().len() {
                    h.set_slot(slot, item);
                }
            }
            BlockEntity::Composter(_) | BlockEntity::BrewingStand(_) | BlockEntity::Opaque { .. } => {}
        }
    }

    /// This entity's menu-local `container_set_data` properties, in vanilla
    /// property-index order — the furnace's four burn/cook timers
    /// (`Furnace::container_data`'s own doc comment cites
    /// `AbstractFurnaceBlockEntity`'s `ContainerData` at `:66-104`). Empty for
    /// every other kind: the hopper's `HopperMenu` has no `ContainerData` at
    /// all (`HopperMenu.java` never calls `addDataSlots`), and composter/
    /// brewing-stand are excluded for the same reasons
    /// [`menu_name`](Self::menu_name) gives.
    #[must_use]
    pub fn data_properties(&self) -> Vec<i32> {
        match self {
            BlockEntity::Furnace(f) => (0..4).map(|i| f.container_data(i)).collect(),
            BlockEntity::Hopper(_) | BlockEntity::Composter(_) | BlockEntity::BrewingStand(_)
            | BlockEntity::Opaque { .. } => {
                Vec::new()
            }
        }
    }

    /// Advances this entity by exactly one tick, for every variant *except*
    /// [`Hopper`] — a hopper's tick needs its two adjacent registry entries,
    /// which only [`BlockEntityRegistry::tick_hopper`] (holding `&mut self`
    /// over the whole map) can resolve, so it is deliberately excluded here
    /// and ticked separately.
    fn tick_non_hopper(&mut self) {
        match self {
            BlockEntity::Composter(c) => {
                c.tick();
            }
            BlockEntity::Furnace(f) => {
                f.tick();
            }
            BlockEntity::BrewingStand(b) => {
                b.tick();
            }
            BlockEntity::Hopper(_) => {
                debug_assert!(false, "hoppers are ticked via tick_hopper, not this path");
            }
            BlockEntity::Opaque { .. } => {}
        }
    }
}

/// Resolves the fresh [`BlockEntity`] a *placement* of `item` (a full item
/// id, e.g. `"minecraft:furnace"`, matching [`ItemStack::item`]'s `Display` —
/// the same string vocabulary [`crate::composter::compostable_chance`] and
/// [`crate::furnace::base_burn_duration`] already key on) should create,
/// alongside the canonical block-state string to write through
/// [`crate::chunk::ChunkSource::set_block`] for it. `None` for any item that
/// is not one of the four block-entity blocks this crate models — the
/// caller's cue to fall back to its existing plain-block placement.
///
/// Every block name here is the item's own name with no properties (no
/// `facing=`/`lit=` — this crate's placement already has no per-block
/// orientation rules, see `docs/block-edit.md`'s scope note; this is the same
/// simplification, not a new one).
#[must_use]
pub fn block_entity_for_item(item: &str) -> Option<(&'static str, BlockEntity)> {
    match item {
        "minecraft:furnace" => Some((
            "minecraft:furnace",
            BlockEntity::Furnace(Furnace::new(FurnaceKind::Furnace)),
        )),
        "minecraft:smoker" => Some((
            "minecraft:smoker",
            BlockEntity::Furnace(Furnace::new(FurnaceKind::Smoker)),
        )),
        "minecraft:blast_furnace" => Some((
            "minecraft:blast_furnace",
            BlockEntity::Furnace(Furnace::new(FurnaceKind::BlastFurnace)),
        )),
        "minecraft:composter" => Some(("minecraft:composter", BlockEntity::Composter(Composter::new()))),
        "minecraft:hopper" => Some(("minecraft:hopper", BlockEntity::Hopper(Hopper::new()))),
        "minecraft:brewing_stand" => Some((
            "minecraft:brewing_stand",
            BlockEntity::BrewingStand(BrewingStand::new()),
        )),
        _ => None,
    }
}

/// A [`BlockPos`]-keyed map of live [`BlockEntity`] values — the world's own
/// set of ticking furnaces/composters/hoppers/brewing stands. See the module
/// doc comment for what this closes and what it still does not.
#[derive(Debug, Default)]
pub struct BlockEntityRegistry {
    entities: HashMap<BlockPos, BlockEntity>,
}

impl BlockEntityRegistry {
    /// An empty registry — the state of a freshly started world before any
    /// placement.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Every block entity with its position, in arbitrary order.
    ///
    /// **Non-destructive, unlike every other route into the map**, which is the
    /// whole point: world saving has to read the entire registry without
    /// disturbing the live simulation. Added for #468's remaining half — until
    /// this existed, the save path could not see a single block entity, so
    /// `chunk_nbt.rs` wrote an empty `block_entities` list for every chunk and
    /// a saved chest came back empty.
    pub fn iter(&self) -> impl Iterator<Item = (&BlockPos, &BlockEntity)> {
        self.entities.iter()
    }

    /// How many block entities are currently registered. Mostly for tests
    /// and diagnostics.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entities.len()
    }

    /// Whether the registry holds nothing at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entities.is_empty()
    }

    /// Inserts (or overwrites) the entity at `pos`.
    pub fn insert(&mut self, pos: BlockPos, entity: BlockEntity) {
        self.entities.insert(pos, entity);
    }

    /// Removes and returns the entity at `pos`, if any — e.g. once block
    /// breaking learns to consult this registry (not done by this landing;
    /// `crate::server`'s `apply_block_action` does not call this yet).
    pub fn remove(&mut self, pos: BlockPos) -> Option<BlockEntity> {
        self.entities.remove(&pos)
    }

    /// A read-only view of the entity at `pos`, if any.
    #[must_use]
    pub fn get(&self, pos: BlockPos) -> Option<&BlockEntity> {
        self.entities.get(&pos)
    }

    /// A mutable view of the entity at `pos`, if any.
    pub fn get_mut(&mut self, pos: BlockPos) -> Option<&mut BlockEntity> {
        self.entities.get_mut(&pos)
    }

    /// Advances every registered entity by exactly one tick.
    ///
    /// Positions are snapshotted up front (`Vec<BlockPos>`, not a live
    /// iterator over `self.entities`) because [`tick_hopper`](Self::tick_hopper)
    /// needs to mutate the map (remove-then-reinsert three entries) while a
    /// plain `HashMap` iterator would forbid mutating the map it is walking.
    /// The snapshot cannot observe an entity a tick *added* mid-pass (nothing
    /// here adds one — only placement does, and placement never runs
    /// concurrently with a tick since both hold the same registry lock, see
    /// [`BlockEntityHandle`]), so this is a complete, order-independent pass
    /// over exactly what existed when the tick started.
    pub fn tick_all(&mut self) {
        self.tick_all_with_hopper_lock(&|_| true);
    }

    /// [`tick_all`](Self::tick_all), with each hopper's redstone lock supplied
    /// by the caller (issue #321).
    ///
    /// `enabled` receives a hopper's position and answers whether it may
    /// transfer this tick — `false` while redstone-powered. The caller reads it
    /// off the block state, which is where vanilla keeps it
    /// (`HopperBlock.ENABLED`, maintained by `checkPoweredState`); this registry
    /// has no world access and deliberately does not compute it.
    ///
    /// `tick_all` remains as the unlocked shorthand for the several tests and
    /// call sites that hold no world, so this is an addition rather than a
    /// signature change. **`crate::tick::run_tick_loop` is the one production
    /// caller that must use this one** — it is the only place holding both a
    /// `ChunkSource` and this registry, and a hopper ticked through the
    /// shorthand can never be locked.
    pub fn tick_all_with_hopper_lock(&mut self, enabled: &dyn Fn(BlockPos) -> bool) {
        let positions: Vec<BlockPos> = self.entities.keys().copied().collect();
        for pos in positions {
            let is_hopper = matches!(self.entities.get(&pos), Some(BlockEntity::Hopper(_)));
            if is_hopper {
                self.tick_hopper(pos, enabled(pos));
            } else if let Some(entity) = self.entities.get_mut(&pos) {
                entity.tick_non_hopper();
            }
        }
    }

    /// Ticks the hopper at `pos` against its `above`/`below` neighbours,
    /// mirroring [`Hopper::tick`](crate::hopper::Hopper::tick)'s own
    /// `above`/`below` parameters — this is the "way to ask what's the
    /// container (if any) at world position P" `docs/block-entities.md`
    /// named as the missing piece, answered here for the one container shape
    /// ([`Hopper`]) this crate actually has slots for (see the module doc
    /// comment's scope note).
    ///
    /// Implemented as remove-tick-reinsert rather than three simultaneous
    /// `get_mut` calls: a single `HashMap` cannot hand out more than one live
    /// mutable borrow at a time (`pos`, `pos.y-1`, and `pos.y+1` could even
    /// collide if `pos` were malformed, though `y±1` never equals `y`), so
    /// this sidesteps the borrow entirely rather than reaching for unstable
    /// `get_many_mut`.
    fn tick_hopper(&mut self, pos: BlockPos, enabled: bool) {
        let Some(BlockEntity::Hopper(mut hopper)) = self.entities.remove(&pos) else {
            return;
        };

        let below_pos = BlockPos::new(pos.x, pos.y - 1, pos.z);
        let above_pos = BlockPos::new(pos.x, pos.y + 1, pos.z);
        let mut below = self.entities.remove(&below_pos);
        let mut above = self.entities.remove(&above_pos);

        // Issue #321: the redstone lock. `enabled` is read from the block state
        // the caller supplies rather than recomputed here, because the block
        // state *is* vanilla's source of truth for it —
        // `HopperBlock.checkPoweredState` (`HopperBlock.java:125-130`) writes
        // `ENABLED` on every neighbour change and on placement, and
        // `HopperBlockEntity` then simply obeys it. This registry has no world
        // access to compute `hasNeighborSignal` itself, and needs none.
        hopper.tick(
            enabled,
            below.as_mut().and_then(BlockEntity::hopper_slots_mut),
            above.as_mut().and_then(BlockEntity::hopper_slots_mut),
        );

        self.entities.insert(pos, BlockEntity::Hopper(hopper));
        if let Some(entity) = below {
            self.entities.insert(below_pos, entity);
        }
        if let Some(entity) = above {
            self.entities.insert(above_pos, entity);
        }
    }
}

/// A shared, mutation-capable handle onto one [`BlockEntityRegistry`] —
/// `Arc<Mutex<_>>`-backed and cheaply `Clone`, the same shape
/// [`crate::mobs::LiveMobSource`] already established for sharing the mob
/// simulation between a connection's task and a background tick-loop task.
/// See the module doc comment for why a registry needs this at all (a
/// connection inserts on placement; a tick loop, where one is spawned,
/// advances every entry independently of any one connection's traffic).
#[derive(Debug, Clone, Default)]
pub struct BlockEntityHandle(Arc<Mutex<BlockEntityRegistry>>);

impl BlockEntityHandle {
    /// A handle onto a fresh, empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Runs `f` against the locked registry, returning its result. The whole
    /// point of funnelling every access through this one method (rather than
    /// exposing the `Mutex` or a `lock()` accessor) is that no caller can
    /// forget to handle a poisoned lock inconsistently — see the `expect`
    /// below, matching [`crate::mobs::LiveMobSource`]'s own
    /// "poisoned lock is a bug, not a recoverable condition" precedent.
    pub fn with<R>(&self, f: impl FnOnce(&mut BlockEntityRegistry) -> R) -> R {
        let mut guard = self.0.lock().expect("block entity registry lock poisoned");
        f(&mut guard)
    }
}

/// Native tick-loop driver, the direct analogue of
/// [`crate::mobs::run_mob_tick_loop`] for block entities: owns nothing but
/// the handle, ticks every [`BLOCK_ENTITY_TICK_INTERVAL`], forever, until the
/// task is aborted (by [`crate::IntegratedServer`]'s shutdown/drop, exactly
/// like the mob tick task).
///
/// # Superseded as of issue #284 — no longer what production spawns
///
/// Same situation as [`crate::mobs::run_mob_tick_loop`], its analogue: production
/// (`crate::IntegratedServer::open_in_memory_with_mobs`) used to spawn this
/// function side-by-side with that one, and now spawns
/// [`crate::tick::run_tick_loop`] instead — one loop, ticking both, with MSPT/
/// TPS/overrun accounting (issue #285). This function is unchanged and still
/// covered by its own test below; see `crate::tick`'s module doc for why one
/// loop replaced two.
///
/// Native only, like `run_mob_tick_loop` — `tokio::time::interval` is
/// unavailable on `wasm32` (see that function's own doc comment for the
/// established reasoning this repeats).
#[cfg(not(target_arch = "wasm32"))]
// Same reasoning as `run_mob_tick_loop`'s own `#[allow(dead_code)]`: no
// caller left outside this file's own `#[cfg(test)]` module since #284.
#[allow(dead_code)]
pub(crate) async fn run_block_entity_tick_loop(handle: BlockEntityHandle) {
    // 50ms, matching vanilla's 20 TPS and this crate's other tick intervals
    // (`server.rs`'s `VITALS_TICK_INTERVAL`, `mobs.rs`'s `MOB_TICK_INTERVAL`) —
    // kept as a local constant for the same reason those two are: this task
    // has no reason to share a literal with either.
    const BLOCK_ENTITY_TICK_INTERVAL: std::time::Duration = std::time::Duration::from_millis(50);
    let mut tick = tokio::time::interval(BLOCK_ENTITY_TICK_INTERVAL);
    loop {
        tick.tick().await;
        handle.with(BlockEntityRegistry::tick_all);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stack(item: &str, count: u32) -> ItemStack {
        ItemStack::new(item.parse().expect("valid resource key"), count)
    }

    #[test]
    fn block_entity_for_item_resolves_all_four_kinds_and_rejects_a_plain_block() {
        let (block, entity) = block_entity_for_item("minecraft:furnace").expect("furnace");
        assert_eq!(block, "minecraft:furnace");
        assert!(matches!(entity, BlockEntity::Furnace(f) if f.kind() == FurnaceKind::Furnace));

        let (block, entity) = block_entity_for_item("minecraft:smoker").expect("smoker");
        assert_eq!(block, "minecraft:smoker");
        assert!(matches!(entity, BlockEntity::Furnace(f) if f.kind() == FurnaceKind::Smoker));

        let (block, entity) = block_entity_for_item("minecraft:blast_furnace").expect("blast furnace");
        assert_eq!(block, "minecraft:blast_furnace");
        assert!(matches!(entity, BlockEntity::Furnace(f) if f.kind() == FurnaceKind::BlastFurnace));

        let (block, entity) = block_entity_for_item("minecraft:composter").expect("composter");
        assert_eq!(block, "minecraft:composter");
        assert!(matches!(entity, BlockEntity::Composter(_)));

        let (block, entity) = block_entity_for_item("minecraft:hopper").expect("hopper");
        assert_eq!(block, "minecraft:hopper");
        assert!(matches!(entity, BlockEntity::Hopper(_)));

        let (block, entity) = block_entity_for_item("minecraft:brewing_stand").expect("brewing stand");
        assert_eq!(block, "minecraft:brewing_stand");
        assert!(matches!(entity, BlockEntity::BrewingStand(_)));

        assert!(
            block_entity_for_item("minecraft:stone").is_none(),
            "a plain block must not resolve to a block entity"
        );
    }

    /// A furnace's menu identity/slots/data round trip through the generic
    /// [`BlockEntity`] accessors exactly as [`Furnace`]'s own API reports
    /// them — the wiring layer (`crate::server`) never reads a [`Furnace`]
    /// directly, so this is the control that the indirection is faithful.
    #[test]
    fn furnace_menu_accessors_mirror_the_furnace_directly() {
        let mut furnace = Furnace::new(FurnaceKind::Furnace);
        furnace.set_input(Some(stack("minecraft:iron_ore", 1)));
        furnace.set_fuel(Some(stack("minecraft:coal", 1)));
        let mut entity = BlockEntity::Furnace(furnace);

        assert_eq!(entity.menu_name(), Some("minecraft:furnace"));
        assert_eq!(
            entity.container_slots(),
            vec![Some(stack("minecraft:iron_ore", 1)), Some(stack("minecraft:coal", 1)), None]
        );
        // Furnace::container_data(0..3) for a freshly set, not-yet-ticked
        // furnace: unlit (0), no lit-duration recorded yet (0), no cook
        // progress (0), but `set_input` already computed the recipe's total
        // cook time (200 for iron ore, matching `furnace.rs`'s own
        // `DEFAULT_COOK_TIME`/recipe-table citation) into index 3.
        assert_eq!(entity.data_properties(), vec![0, 0, 0, 200]);

        entity.set_container_slot(2, Some(stack("minecraft:iron_ingot", 1)));
        assert_eq!(entity.container_slots()[2], Some(stack("minecraft:iron_ingot", 1)));

        let smoker = BlockEntity::Furnace(Furnace::new(FurnaceKind::Smoker));
        assert_eq!(smoker.menu_name(), Some("minecraft:smoker"));
        let blast = BlockEntity::Furnace(Furnace::new(FurnaceKind::BlastFurnace));
        assert_eq!(blast.menu_name(), Some("minecraft:blast_furnace"));
    }

    /// A hopper's 5 flat slots round-trip through the same generic
    /// accessors, and it has no menu-local data properties at all — the
    /// control that `data_properties` genuinely varies by kind rather than
    /// always answering the furnace's 4.
    #[test]
    fn hopper_menu_accessors_mirror_the_hopper_directly() {
        let mut hopper = Hopper::new();
        hopper.set_slot(0, Some(stack("minecraft:diamond", 3)));
        let mut entity = BlockEntity::Hopper(hopper);

        assert_eq!(entity.menu_name(), Some("minecraft:hopper"));
        assert_eq!(entity.container_slots().len(), 5);
        assert_eq!(entity.container_slots()[0], Some(stack("minecraft:diamond", 3)));
        assert!(entity.data_properties().is_empty());

        entity.set_container_slot(1, Some(stack("minecraft:emerald", 1)));
        assert_eq!(entity.container_slots()[1], Some(stack("minecraft:emerald", 1)));
    }

    /// **Control**: composter and brewing stand have no menu at all
    /// (see [`BlockEntity::menu_name`]'s own doc comment for why each is
    /// excluded, for two different reasons) — proving the generic accessors
    /// answer "nothing to open" rather than silently fabricating a menu.
    #[test]
    fn composter_and_brewing_stand_have_no_menu() {
        let composter = BlockEntity::Composter(Composter::new());
        assert_eq!(composter.menu_name(), None);
        assert!(composter.container_slots().is_empty());
        assert!(composter.data_properties().is_empty());

        let brewing = BlockEntity::BrewingStand(BrewingStand::new());
        assert_eq!(brewing.menu_name(), None);
        assert!(brewing.container_slots().is_empty());
        assert!(brewing.data_properties().is_empty());
    }

    #[test]
    fn registry_insert_get_remove_round_trip() {
        let mut reg = BlockEntityRegistry::new();
        let pos = BlockPos::new(1, 64, 1);
        assert!(reg.get(pos).is_none());

        reg.insert(pos, BlockEntity::Composter(Composter::new()));
        assert!(matches!(reg.get(pos), Some(BlockEntity::Composter(_))));
        assert_eq!(reg.len(), 1);

        assert!(matches!(reg.remove(pos), Some(BlockEntity::Composter(_))));
        assert!(reg.get(pos).is_none());
        assert!(reg.is_empty());
    }

    /// A furnace ticked through the registry behaves exactly like one ticked
    /// directly — the registry is a location, not a reimplementation. Loaded
    /// with fuel and ore, it lights on the first `tick_all` and reports the
    /// same `lit_changed` transition [`Furnace::tick`]'s own unit tests
    /// already pin.
    #[test]
    fn tick_all_advances_a_registered_furnace() {
        let mut reg = BlockEntityRegistry::new();
        let pos = BlockPos::new(0, 70, 0);
        let mut furnace = Furnace::new(FurnaceKind::Furnace);
        furnace.set_fuel(Some(stack("minecraft:coal", 1)));
        furnace.set_input(Some(stack("minecraft:iron_ore", 1)));
        reg.insert(pos, BlockEntity::Furnace(furnace));

        reg.tick_all();

        let Some(BlockEntity::Furnace(f)) = reg.get(pos) else {
            panic!("furnace must still be registered after a tick");
        };
        assert!(f.is_lit(), "fuel + ingredient must light the furnace on its first tick");
    }

    /// Two hoppers stacked (`below` sits directly under `above`) move
    /// **two** items on the first tick, not one — this is vanilla's real
    /// "double hopper" throughput, not a bug: each hopper's own
    /// [`Hopper::tick`](crate::hopper::Hopper::tick) independently attempts
    /// *both* a push (into its own `below`) and a pull (from its own
    /// `above`), so within one `tick_all` pass, `below`'s tick pulls one
    /// item up from `above`, and **separately** `above`'s own tick pushes
    /// one item down into `below` — two independent successful transfers in
    /// the same tick, exactly the mechanic vanilla hopper clocks/sorters
    /// exploit for 2x throughput. `hopper.rs`'s own
    /// `sucks_one_item_from_above_on_first_ready_tick` already proves the
    /// *single*-hopper pull in isolation; this proves the pair survives
    /// going through the registry's remove/tick/reinsert path with **both**
    /// neighbours present simultaneously (the harder case: `tick_hopper`
    /// must resolve `above` correctly even while `below` has *also* been
    /// removed from the map for the duration of the call) and predicts the
    /// combined result rather than just one direction.
    #[test]
    fn tick_all_moves_two_items_between_a_stacked_hopper_pair_on_the_first_tick() {
        let mut reg = BlockEntityRegistry::new();
        let below_pos = BlockPos::new(5, 10, 5);
        let above_pos = BlockPos::new(5, 11, 5);

        let mut above = Hopper::new();
        above.set_slot(0, Some(stack("minecraft:diamond", 3)));
        reg.insert(below_pos, BlockEntity::Hopper(Hopper::new()));
        reg.insert(above_pos, BlockEntity::Hopper(above));

        reg.tick_all();

        // `below`'s own tick pulled one item from `above` (3 -> 2 there,
        // landing in `below`'s empty slot 0), then `above`'s own tick pushed
        // one more of its remaining items down into `below` (2 -> 1 there,
        // merging into `below`'s slot 0) — two transfers, one net item each
        // direction's own tick contributed.
        let Some(BlockEntity::Hopper(below)) = reg.get(below_pos) else {
            panic!("below hopper must still be registered");
        };
        assert_eq!(below.slots()[0], Some(stack("minecraft:diamond", 2)));

        let Some(BlockEntity::Hopper(above)) = reg.get(above_pos) else {
            panic!("above hopper must still be registered");
        };
        assert_eq!(above.slots()[0], Some(stack("minecraft:diamond", 1)));
    }

    /// **Control**: a hopper with nothing above and nothing below must not
    /// panic and must not change — proves `tick_hopper`'s `None` handling
    /// (empty neighbour slots) is real, not merely untested.
    #[test]
    fn tick_all_leaves_an_isolated_hopper_unchanged_other_than_its_cooldown() {
        let mut reg = BlockEntityRegistry::new();
        let pos = BlockPos::new(0, 0, 0);
        reg.insert(pos, BlockEntity::Hopper(Hopper::new()));

        reg.tick_all();

        let Some(BlockEntity::Hopper(h)) = reg.get(pos) else {
            panic!("hopper must still be registered");
        };
        assert!(h.slots().iter().all(Option::is_none), "no neighbours means nothing to move");
    }

    #[test]
    fn handle_with_mutates_the_shared_registry() {
        let handle = BlockEntityHandle::new();
        let pos = BlockPos::new(2, 2, 2);
        handle.with(|reg| reg.insert(pos, BlockEntity::Composter(Composter::new())));

        let clone = handle.clone();
        let present = clone.with(|reg| reg.get(pos).is_some());
        assert!(present, "a clone of the handle must see the same registry");
    }

    /// The actual background driver [`crate::IntegratedServer::open_in_memory_with_mobs`]
    /// spawns, not just [`BlockEntityRegistry::tick_all`] called synchronously
    /// — this is what proves a furnace really ticks *in a running task over
    /// time*, the missing half `docs/block-entities.md` named ("no tick loop
    /// drives them"). Loaded with one coal (1600-tick burn duration — far
    /// more than needed) and one iron ore (a 200-tick smelt,
    /// `furnace.rs`'s own `iron_ore_smelts_into_one_ingot_at_exactly_tick_200`
    /// unit test pins the same number), it must light on the very first real
    /// tick the loop performs and hold a real iron ingot in its output slot
    /// well before 200 ticks' worth of virtual time (10s at 50ms/tick) have
    /// elapsed — predicting the value (a real ingot), not just that
    /// *something* eventually changed.
    ///
    /// `#[tokio::test(start_paused = true)]`: the same precedent
    /// `tests/serve_play.rs` already established for this crate — real
    /// `tokio::time::interval`s, virtual clock, resolves in a fraction of a
    /// second of actual wall time.
    #[tokio::test(start_paused = true)]
    async fn run_block_entity_tick_loop_actually_advances_a_registered_furnace_over_time() {
        let handle = BlockEntityHandle::new();
        let pos = BlockPos::new(0, 70, 0);
        let mut furnace = Furnace::new(FurnaceKind::Furnace);
        furnace.set_fuel(Some(stack("minecraft:coal", 1)));
        furnace.set_input(Some(stack("minecraft:iron_ore", 1)));
        handle.with(|reg| reg.insert(pos, BlockEntity::Furnace(furnace)));

        // Detached on purpose: the loop never returns, so nothing here should
        // (or could) join it. It is torn down along with the test's runtime.
        tokio::spawn(run_block_entity_tick_loop(handle.clone()));

        // Let the spawned task actually run at least once before checking —
        // `tokio::time::interval`'s first `tick()` resolves immediately, so
        // this is enough for the loop to have performed its first real tick.
        tokio::task::yield_now().await;
        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
        let lit_after_first_tick = handle.with(|reg| match reg.get(pos) {
            Some(BlockEntity::Furnace(f)) => f.is_lit(),
            _ => false,
        });
        assert!(
            lit_after_first_tick,
            "the running background task must have lit the furnace on its first real tick"
        );

        // 200 ticks * 50ms = 10s of tick-loop time to fully smelt one iron
        // ore; sleeping past that is only virtual time under the paused
        // clock, not real wall time.
        tokio::time::sleep(std::time::Duration::from_millis(10_100)).await;
        let output = handle.with(|reg| match reg.get(pos) {
            Some(BlockEntity::Furnace(f)) => f.output().cloned(),
            _ => None,
        });
        assert_eq!(
            output,
            Some(stack("minecraft:iron_ingot", 1)),
            "the background tick loop must have smelted the iron ore into an ingot by \
             10s of virtual time"
        );
    }
}
