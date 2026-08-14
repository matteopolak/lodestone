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
    /// A plain item container with no simulation of its own — a chest, trapped
    /// chest or barrel. `id` is the block-entity type key, `slots` its
    /// [`CONTAINER_9X3_SIZE`] inventory.
    ///
    /// It has no `tick`: vanilla's `ChestBlockEntity.lidAnimateTick` is client-side
    /// animation only, and nothing about its contents changes on its own.
    Container {
        /// The `minecraft:block_entity_type` key (`minecraft:chest`, …).
        id: String,
        /// The container's own slots, in menu order.
        slots: Vec<Option<ItemStack>>,
    },
    /// A block entity this crate has no simulation for (spawner, vault, …).
    /// The vanilla id and the full NBT compound are preserved verbatim so the entity
    /// round-trips through a save/load cycle unchanged.
    Opaque { id: String, nbt: Nbt },
}

/// Slot count of vanilla's `generic_9x3` menu — a chest, trapped chest or
/// barrel (`ChestBlockEntity`'s `NonNullList.withSize(27, …)`).
pub const CONTAINER_9X3_SIZE: usize = 27;

/// Slot count of vanilla's `generic_3x3` menu — a dispenser or dropper
/// (`DispenserBlockEntity`'s `NonNullList.withSize(9, …)`). Issue #320: this
/// gives both blocks real, persistent storage the same way the three
/// `generic_9x3` blocks already have it; `crate::redstone_dispenser`'s own
/// module doc names the remaining gap (nothing yet threads a live container
/// into the scheduled fire tick).
pub const CONTAINER_3X3_SIZE: usize = 9;

/// The block-entity type key for a container block, or `None` if that block is
/// not one of the plain-inventory containers this crate models (`generic_9x3`
/// or `generic_3x3` — see [`BlockEntity::container`]/[`container_of_size`]).
///
/// Keyed on the block *name* with no properties, so a caller holding a canonical
/// state string must split it first.
#[must_use]
pub fn container_type_for_block(block: &str) -> Option<&'static str> {
    match block {
        "minecraft:chest" => Some("minecraft:chest"),
        "minecraft:trapped_chest" => Some("minecraft:trapped_chest"),
        "minecraft:barrel" => Some("minecraft:barrel"),
        "minecraft:dispenser" => Some("minecraft:dispenser"),
        "minecraft:dropper" => Some("minecraft:dropper"),
        _ => None,
    }
}

impl BlockEntity {
    /// A fresh, empty `generic_9x3` container of type `id`.
    #[must_use]
    pub fn container(id: &str) -> Self {
        BlockEntity::Container {
            id: id.to_owned(),
            slots: vec![None; CONTAINER_9X3_SIZE],
        }
    }

    /// A fresh, empty container of type `id` with `size` slots — the
    /// dispenser/dropper `generic_3x3` counterpart to [`Self::container`],
    /// which is fixed at [`CONTAINER_9X3_SIZE`].
    #[must_use]
    pub fn container_of_size(id: &str, size: usize) -> Self {
        BlockEntity::Container {
            id: id.to_owned(),
            slots: vec![None; size],
        }
    }
}

impl BlockEntity {
    /// This entity's `minecraft:block_entity_type` registry key — the `id` field
    /// vanilla writes into the chunk NBT, and the key a protocol crate resolves
    /// to the VarInt type id the chunk packet's block-entity array carries
    /// (issue #520).
    ///
    /// Deliberately *not* [`menu_name`](Self::menu_name): those two agree for
    /// the furnace family and the hopper by coincidence, and disagree for every
    /// variant with no container screen — a composter has a real block-entity
    /// type and no menu at all.
    #[must_use]
    pub fn type_id(&self) -> &str {
        match self {
            BlockEntity::Composter(_) => "minecraft:composter",
            BlockEntity::Furnace(f) => match f.kind() {
                FurnaceKind::Furnace => "minecraft:furnace",
                FurnaceKind::Smoker => "minecraft:smoker",
                FurnaceKind::BlastFurnace => "minecraft:blast_furnace",
            },
            BlockEntity::Hopper(_) => "minecraft:hopper",
            BlockEntity::BrewingStand(_) => "minecraft:brewing_stand",
            BlockEntity::Container { id, .. } | BlockEntity::Opaque { id, .. } => id,
        }
    }

    /// A mutable view of this entity's flat item-slot array, if it has one
    /// shaped that way — today, only [`Hopper`]. See the module doc comment's
    /// "hopper adjacency" scope note for why every other variant answers
    /// `None` rather than something partial/misleading.
    fn hopper_slots_mut(&mut self) -> Option<&mut [Option<ItemStack>]> {
        match self {
            BlockEntity::Hopper(h) => Some(h.slots_mut()),
            // A chest/barrel *is* a flat slot array, so hopper adjacency into
            // one works — the "no real container at the adjacent position"
            // scope note in the module doc no longer covers these three.
            BlockEntity::Container { slots, .. } => Some(slots),
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
            // Issue #320: a dispenser/dropper's `generic_3x3` (9 slots) is
            // narrower than a chest/trapped-chest/barrel's `generic_9x3` (27) —
            // the two `Container` shapes share one Rust variant but not one
            // vanilla menu, so this reads the id rather than assuming.
            BlockEntity::Container { id, slots } => Some(if slots.len() == CONTAINER_3X3_SIZE {
                debug_assert!(id == "minecraft:dispenser" || id == "minecraft:dropper", "a 9-slot container must be a dispenser or dropper: {id}");
                "minecraft:generic_3x3"
            } else {
                "minecraft:generic_9x3"
            }),
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
            BlockEntity::Container { slots, .. } => slots.clone(),
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
            BlockEntity::Container { slots, .. } => {
                if let Some(cell) = slots.get_mut(slot) {
                    *cell = item;
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
            | BlockEntity::Container { .. } | BlockEntity::Opaque { .. } => {
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
            BlockEntity::Container { .. } | BlockEntity::Opaque { .. } => {}
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
        "minecraft:chest" => Some(("minecraft:chest", BlockEntity::container("minecraft:chest"))),
        "minecraft:trapped_chest" => Some((
            "minecraft:trapped_chest",
            BlockEntity::container("minecraft:trapped_chest"),
        )),
        "minecraft:barrel" => Some(("minecraft:barrel", BlockEntity::container("minecraft:barrel"))),
        "minecraft:dispenser" => Some((
            "minecraft:dispenser",
            BlockEntity::container_of_size("minecraft:dispenser", CONTAINER_3X3_SIZE),
        )),
        "minecraft:dropper" => Some((
            "minecraft:dropper",
            BlockEntity::container_of_size("minecraft:dropper", CONTAINER_3X3_SIZE),
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
        self.tick_all_with_hopper_lock(&|_| true, &|_| true);
    }

    /// [`tick_all`](Self::tick_all), with each hopper's redstone lock supplied
    /// by the caller (issue #321) **and the scan bounded by chunk residency**
    /// (issue #504).
    ///
    /// `enabled` receives a hopper's position and answers whether it may
    /// transfer this tick — `false` while redstone-powered. The caller reads it
    /// off the block state, which is where vanilla keeps it
    /// (`HopperBlock.ENABLED`, maintained by `checkPoweredState`); this registry
    /// has no world access and deliberately does not compute it.
    ///
    /// `is_loaded` receives every entity's position (not only a hopper's) and
    /// answers whether its *chunk* is currently loaded. A position it rejects
    /// is skipped entirely — `enabled` is never called for it, and neither is
    /// [`BlockEntity::tick_non_hopper`]. **This is a deliberate behavioural
    /// choice, not merely a cost optimisation**: vanilla ticks block entities
    /// from each loaded chunk's own tick list, not from one registry that
    /// remembers every position a player has ever visited
    /// (`BlockEntityRegistry` has no eviction — see the module doc). Before
    /// this, `is_loaded` did not exist and every registered entity ticked
    /// forever regardless of whether anyone was near it; the measured cost of
    /// that was up to 610 synchronous column *generations* per tick once the
    /// registry outgrew `ChunkStore`'s capacity (`chunk_store.rs`'s own test
    /// module has the counters). The correct fix is to stop asking the
    /// question for a position nothing has loaded, not to make asking it
    /// cheaper — a `false`-returning `is_loaded` reproduces vanilla exactly (a
    /// furnace far from every player does not advance), whereas a
    /// residency-aware-but-still-polled read would leave a hopper's `enabled`
    /// flag stuck at whatever it last observed, which is not a state vanilla
    /// ever has a hopper sit in.
    ///
    /// `tick_all` remains as the unlocked, always-loaded shorthand for the
    /// several tests and call sites that hold no world, so this is an
    /// addition rather than a signature change beyond adding `is_loaded`.
    /// **`crate::tick::run_tick_loop` is the one production caller that must
    /// use this one** — it is the only place holding both a `ChunkSource` and
    /// this registry, and a hopper ticked through the shorthand can never be
    /// locked or bounded.
    pub fn tick_all_with_hopper_lock(
        &mut self,
        is_loaded: &dyn Fn(BlockPos) -> bool,
        enabled: &dyn Fn(BlockPos) -> bool,
    ) {
        let positions: Vec<BlockPos> = self.entities.keys().copied().collect();
        for pos in positions {
            if !is_loaded(pos) {
                continue;
            }
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

/// Renders a canonical block-state string as the `{Name, Properties}` compound
/// `BlockState.CODEC` reads — the shape any NBT field holding a block state takes.
///
/// `Properties` is omitted entirely for a state with none, matching vanilla's own
/// codec (an empty map would still decode, but writing one where vanilla writes
/// nothing is a gratuitous divergence in a payload we may one day compare byte for
/// byte against a capture). Every property value is a `String`, including numeric
/// and boolean ones — `Properties` is a map of the property's *serialized name*,
/// never its typed value.
#[must_use]
pub fn block_state_nbt(state: &str) -> Nbt {
    let (name, properties) = match state.split_once('[') {
        Some((name, rest)) => (name, rest.strip_suffix(']').unwrap_or(rest)),
        None => (state, ""),
    };
    let mut fields = vec![("Name".to_string(), Nbt::String(name.to_string()))];
    let pairs: Vec<(String, Nbt)> = properties
        .split(',')
        .filter(|pair| !pair.is_empty())
        .filter_map(|pair| pair.split_once('='))
        .map(|(key, value)| (key.to_string(), Nbt::String(value.to_string())))
        .collect();
    if !pairs.is_empty() {
        fields.push(("Properties".to_string(), Nbt::Compound(pairs)));
    }
    Nbt::Compound(fields)
}

/// One in-flight moving piston's network NBT — `PistonMovingBlockEntity`'s
/// `getUpdateTag`, which is `saveCustomOnly`, i.e. exactly `saveAdditional`'s five
/// fields with no `id`/`x`/`y`/`z`.
///
/// Ported from `saveAdditional`, not from the field declarations, and the two
/// differ in a way that matters:
///
/// * **`facing` is a `Byte`**, because `Direction.LEGACY_ID_CODEC` is
///   `Codec.BYTE` over `get3DDataValue`. Written as an `Int` it decodes as absent
///   and every piston silently defaults to `DOWN`.
/// * **`progress` is `progressO`**, the value at the *start* of the tick, so a
///   freshly created entity reports [`crate::piston::PISTON_INITIAL_PROGRESS`] and
///   not the `0.5` the server's own first tick would already have reached. A client
///   ramps from this seed itself; sending the advanced value halves the animation.
/// * `extending` and `source` are `putBoolean`, which is a `Byte` in NBT.
#[must_use]
pub fn moving_piston_nbt(entity: &crate::piston::MovingBlockEntity) -> Nbt {
    Nbt::Compound(vec![
        (
            "blockState".to_string(),
            block_state_nbt(&entity.moved_state),
        ),
        ("facing".to_string(), Nbt::Byte(entity.facing_3d_value())),
        (
            "progress".to_string(),
            Nbt::Float(crate::piston::PISTON_INITIAL_PROGRESS),
        ),
        ("extending".to_string(), Nbt::Byte(i8::from(entity.extending))),
        ("source".to_string(), Nbt::Byte(i8::from(entity.source))),
    ])
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

    /// Issue #320: a dispenser and dropper each get a real 9-slot
    /// `generic_3x3` container — distinct from a chest's 27-slot
    /// `generic_9x3`, which the `menu_name`/`container_slots` split by size
    /// must not collapse into one shape.
    #[test]
    fn dispenser_and_dropper_get_a_nine_slot_generic_3x3_container() {
        let (block, entity) = block_entity_for_item("minecraft:dispenser").expect("dispenser");
        assert_eq!(block, "minecraft:dispenser");
        assert_eq!(entity.menu_name(), Some("minecraft:generic_3x3"));
        assert_eq!(entity.container_slots().len(), CONTAINER_3X3_SIZE);

        let (block, entity) = block_entity_for_item("minecraft:dropper").expect("dropper");
        assert_eq!(block, "minecraft:dropper");
        assert_eq!(entity.menu_name(), Some("minecraft:generic_3x3"));
        assert_eq!(entity.container_slots().len(), CONTAINER_3X3_SIZE);

        // The existing chest path must still answer the wider menu — the
        // control that the id-based split above did not just make every
        // container 9 slots.
        let (_, chest) = block_entity_for_item("minecraft:chest").expect("chest");
        assert_eq!(chest.menu_name(), Some("minecraft:generic_9x3"));
        assert_eq!(chest.container_slots().len(), CONTAINER_9X3_SIZE);

        assert_eq!(container_type_for_block("minecraft:dispenser"), Some("minecraft:dispenser"));
        assert_eq!(container_type_for_block("minecraft:dropper"), Some("minecraft:dropper"));
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

    /// Issue #504's fix, isolated from any world/store machinery: a position
    /// `is_loaded` rejects must not tick at all — not the hopper path, not
    /// the plain `tick_non_hopper` path.
    #[test]
    fn tick_all_with_hopper_lock_skips_a_position_the_loaded_predicate_rejects() {
        let mut reg = BlockEntityRegistry::new();
        let pos = BlockPos::new(0, 70, 0);
        let mut furnace = Furnace::new(FurnaceKind::Furnace);
        furnace.set_fuel(Some(stack("minecraft:coal", 1)));
        furnace.set_input(Some(stack("minecraft:iron_ore", 1)));
        reg.insert(pos, BlockEntity::Furnace(furnace));

        reg.tick_all_with_hopper_lock(&|_| false, &|_| true);

        let Some(BlockEntity::Furnace(f)) = reg.get(pos) else {
            panic!("furnace must still be registered — `is_loaded` skips the tick, not the entity");
        };
        assert!(
            !f.is_lit(),
            "a furnace whose chunk `is_loaded` reports unloaded must not advance — this is \
             the vanilla behaviour the fix chooses (see `tick_all_with_hopper_lock`'s own doc \
             comment): a block entity outside every loaded chunk simply does not tick."
        );

        // Control: the same furnace, same fuel and ore, with the predicate
        // flipped to `true` — proves the furnace above was skipped *because*
        // of the predicate, not because it can never light (which would make
        // the assertion above vacuous).
        let mut reg = BlockEntityRegistry::new();
        let mut furnace = Furnace::new(FurnaceKind::Furnace);
        furnace.set_fuel(Some(stack("minecraft:coal", 1)));
        furnace.set_input(Some(stack("minecraft:iron_ore", 1)));
        reg.insert(pos, BlockEntity::Furnace(furnace));
        reg.tick_all_with_hopper_lock(&|_| true, &|_| true);
        let Some(BlockEntity::Furnace(f)) = reg.get(pos) else {
            panic!("furnace must still be registered");
        };
        assert!(f.is_lit(), "control: the identical furnace must light when `is_loaded` says yes");
    }

    /// The other half of issue #504: for a hopper specifically, `enabled` —
    /// the closure that in production reaches `world.block_state` and is what
    /// used to generate a whole column per probe — must never even be
    /// *called* for a position `is_loaded` rejects. This is the control that
    /// the fix bounds the expensive call itself, not just its visible effect:
    /// counting invocations rather than inferring "did it tick" from state.
    #[test]
    fn tick_all_with_hopper_lock_never_calls_enabled_for_an_unloaded_hopper() {
        use std::sync::atomic::{AtomicU64, Ordering};

        let mut reg = BlockEntityRegistry::new();
        let pos = BlockPos::new(5, 10, 5);
        reg.insert(pos, BlockEntity::Hopper(Hopper::new()));

        let enabled_calls = AtomicU64::new(0);
        reg.tick_all_with_hopper_lock(&|_| false, &|_| {
            enabled_calls.fetch_add(1, Ordering::Relaxed);
            true
        });
        assert_eq!(
            enabled_calls.load(Ordering::Relaxed),
            0,
            "`enabled` — production's `world.block_state` probe — must not be called at all \
             for a position `is_loaded` rejects. A nonzero count here is exactly issue #504's \
             defect: a per-hopper world read on every tick regardless of residency."
        );

        // Control: flip `is_loaded` to `true` and confirm the same closure
        // now *is* called exactly once — proving the zero above is the gate
        // firing, not a hopper that was never going to call it anyway (e.g.
        // if the position were somehow not recognised as a hopper).
        let enabled_calls = AtomicU64::new(0);
        reg.tick_all_with_hopper_lock(&|_| true, &|_| {
            enabled_calls.fetch_add(1, Ordering::Relaxed);
            true
        });
        assert_eq!(
            enabled_calls.load(Ordering::Relaxed),
            1,
            "control: a loaded hopper must still have its `enabled` closure called exactly once"
        );
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
    /// [`moving_piston_nbt`] against `PistonMovingBlockEntity.saveAdditional` read
    /// as a record definition: five fields, and the **tag types** are the half a
    /// name-only comparison cannot see.
    ///
    /// `facing` written as an `Nbt::Int` is the specific failure this pins. A
    /// client reads it with `getBooleanOr`-style tolerance for the flags but a
    /// strict `Nbt::Byte` match for the direction, so an `Int` decodes as *absent*
    /// and every piston in the world silently animates toward `DOWN` — a clean
    /// parse, no error, wrong geometry. Same shape as the `Age` short/int collision
    /// this repo already paid for.
    #[test]
    fn moving_piston_nbt_matches_the_vanilla_update_tag() {
        let entity = crate::piston::MovingBlockEntity {
            moved_state: "minecraft:piston_head[facing=east,short=false,type=sticky]".to_string(),
            direction: crate::neighbor_update::Direction::East,
            // Distinct on purpose: equal flags would let a transposition of the two
            // adjacent booleans through unnoticed.
            extending: true,
            source: false,
        };
        let Nbt::Compound(fields) = moving_piston_nbt(&entity) else {
            panic!("the update tag must be a compound");
        };
        let names: Vec<&str> = fields.iter().map(|(name, _)| name.as_str()).collect();
        assert_eq!(
            names,
            vec!["blockState", "facing", "progress", "extending", "source"],
            "exactly `saveAdditional`'s five fields, and no id/x/y/z — \
             `getUpdateTag` is `saveCustomOnly`"
        );
        let field = |key: &str| {
            fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value.clone())
                .expect("field present")
        };
        // `Direction.LEGACY_ID_CODEC` is `Codec.BYTE` over `get3DDataValue`, and
        // EAST is 5.
        assert_eq!(field("facing"), Nbt::Byte(5));
        assert_eq!(field("progress"), Nbt::Float(0.0), "`progressO`, not `progress`");
        assert_eq!(field("extending"), Nbt::Byte(1));
        assert_eq!(field("source"), Nbt::Byte(0));
        // `BlockState.CODEC`: a `Name` string plus a `Properties` map whose values
        // are all strings, including the boolean `short`.
        assert_eq!(
            field("blockState"),
            Nbt::Compound(vec![
                (
                    "Name".to_string(),
                    Nbt::String("minecraft:piston_head".to_string())
                ),
                (
                    "Properties".to_string(),
                    Nbt::Compound(vec![
                        ("facing".to_string(), Nbt::String("east".to_string())),
                        ("short".to_string(), Nbt::String("false".to_string())),
                        ("type".to_string(), Nbt::String("sticky".to_string())),
                    ])
                ),
            ])
        );
        // A state with no properties omits `Properties` entirely, as vanilla's
        // codec does.
        assert_eq!(
            block_state_nbt("minecraft:stone"),
            Nbt::Compound(vec![(
                "Name".to_string(),
                Nbt::String("minecraft:stone".to_string())
            )])
        );
    }

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
