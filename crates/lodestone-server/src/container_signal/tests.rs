//! Comparator output of block entities. Every expected value is worked by hand
//! from the fill rule `floor(14 * total / slots) + (total > 0)`, where `total`
//! sums `count / min(99, item limit)`; none is read back from the code.
//!
//! The tick-loop tests run the production loop with a comparator beside a
//! chest: a comparator that reads the right output proves the read, and one
//! that follows a hopper proves the change is noticed without any scan.

use std::sync::{Arc, Mutex};

use lodestone_core::Nbt;
use lodestone_data::block::Block;
use lodestone_data::block_states::{StateId, air_state};
use lodestone_model::{BlockPos, ItemStack};

use super::*;
use crate::block_entities::{BlockEntity, BlockEntityHandle, BlockEntityKind, CONTAINER_3X3_SIZE, LecternData};
use crate::chunk::{ChunkColumn, ChunkSource};
use crate::mobs::{ChunkWorld, LiveMobSource, MobHandle};
use crate::tick::{BlockTickFeed, ExplosionFeed, TICK_PERIOD, TickClock, run_tick_loop};
use crate::{redstone, redstone_diode};

fn stack(item: &str, count: u32) -> ItemStack {
    ItemStack::new(item.parse().unwrap(), count)
}

fn opaque(id: &str, fields: Vec<(&str, Nbt)>) -> BlockEntity {
    BlockEntity::Opaque {
        id: BlockEntityKind::from_name(id),
        nbt: Nbt::Compound(fields.into_iter().map(|(k, v)| (k.to_owned(), v)).collect()),
    }
}

fn item_nbt(id: &str, count: i32) -> Nbt {
    Nbt::Compound(vec![("id".to_owned(), Nbt::String(id.to_owned())), ("count".to_owned(), Nbt::Int(count))])
}

fn read(entity: BlockEntity) -> Option<u8> {
    entity_signal(&entity)
}

fn container(id: &str, size: usize, fill: &[(usize, ItemStack)]) -> BlockEntity {
    let mut entity = BlockEntity::container_of_size(id, size);
    for (slot, item) in fill {
        entity.set_container_slot(*slot, Some(item.clone()));
    }
    entity
}

// -- The fill rule, by block entity ------------------------------------------------

#[test]
fn a_chest_reads_its_fill_fraction() {
    // 1 / 64 / 27 = 0.00058; times 14 floors to 0, plus one for any item.
    assert_eq!(read(container("minecraft:chest", 27, &[(0, stack("minecraft:cobblestone", 1))])), Some(1));
    // 14 of 27 slots full: 14 / 27 = 0.5185; times 14 = 7.26 floors to 7, plus one.
    let fourteen: Vec<_> = (0..14).map(|slot| (slot, stack("minecraft:cobblestone", 64))).collect();
    assert_eq!(read(container("minecraft:chest", 27, &fourteen)), Some(8));
    // 13 of 27: 13 / 27 * 14 = 6.74 floors to 6, plus one.
    assert_eq!(read(container("minecraft:chest", 27, &fourteen[..13])), Some(7));
    // Empty reads zero, full reads fifteen.
    assert_eq!(read(container("minecraft:chest", 27, &[])), Some(0));
    let full: Vec<_> = (0..27).map(|slot| (slot, stack("minecraft:cobblestone", 64))).collect();
    assert_eq!(read(container("minecraft:chest", 27, &full)), Some(15));
}

#[test]
fn an_items_own_stack_limit_scales_the_share() {
    // 27 slots of 16 ender pearls are full (16 / 16 each): 15. Measured against 64
    // instead they would be a quarter full, 27 * 0.25 / 27 * 14 = 3.5, reading 4.
    let pearls: Vec<_> = (0..27).map(|slot| (slot, stack("minecraft:ender_pearl", 16))).collect();
    assert_eq!(read(container("minecraft:barrel", 27, &pearls)), Some(15));
    // 8 pearls in one slot: 8 / 16 = 0.5; / 27 * 14 = 0.26 floors to 0, plus one.
    assert_eq!(read(container("minecraft:barrel", 27, &[(0, stack("minecraft:ender_pearl", 8))])), Some(1));
}

#[test]
fn small_containers_divide_by_their_own_size() {
    // Dispenser, 9 slots, one full stack: 1 / 9 * 14 = 1.56 floors to 1, plus one.
    assert_eq!(
        read(container("minecraft:dispenser", CONTAINER_3X3_SIZE, &[(4, stack("minecraft:cobblestone", 64))])),
        Some(2)
    );
    // Hopper, 5 slots, one full stack: 1 / 5 * 14 = 2.8 floors to 2, plus one; five stacks: 15.
    let mut hopper = BlockEntity::Hopper(crate::hopper::Hopper::new());
    hopper.set_container_slot(0, Some(stack("minecraft:cobblestone", 64)));
    assert_eq!(read(hopper.clone()), Some(3));
    for slot in 1..5 {
        hopper.set_container_slot(slot, Some(stack("minecraft:cobblestone", 64)));
    }
    assert_eq!(read(hopper), Some(15));
}

#[test]
fn a_furnace_reads_its_three_slots() {
    use crate::furnace::{Furnace, FurnaceKind};
    let furnace = Furnace::restore(
        FurnaceKind::Furnace,
        None,
        Some(stack("minecraft:coal", 64)),
        None,
        0,
        0,
        0,
        0,
        Default::default(),
    );
    // One full stack of three: 1 / 3 * 14 = 4.67 floors to 4, plus one.
    assert_eq!(read(BlockEntity::Furnace(furnace)), Some(5));
}

#[test]
fn a_brewing_stand_counts_unstackable_bottles_whole() {
    use crate::brewing::{Bottle, BottleKind, BrewingStand};
    let water = || Bottle::from_potion_name(BottleKind::Potion, "minecraft:water");
    let stand = BrewingStand::restore([water(), water(), water()], None, None, 0, 0, None);
    // Three bottles of five slots: 3 / 5 * 14 = 8.4 floors to 8, plus one.
    assert_eq!(read(BlockEntity::BrewingStand(stand)), Some(9));
}

#[test]
fn a_crafter_counts_slots_that_hold_an_item_or_are_disabled() {
    let mut crafter = BlockEntity::crafter();
    crafter.set_container_slot(0, Some(stack("minecraft:stick", 1)));
    crafter.set_container_slot(1, Some(stack("minecraft:stick", 1)));
    crafter.set_container_slot(2, Some(stack("minecraft:stick", 1)));
    assert_eq!(read(crafter.clone()), Some(3));
    let BlockEntity::Crafter { disabled, .. } = &mut crafter else { unreachable!() };
    disabled[7] = true;
    disabled[8] = true;
    assert_eq!(read(crafter), Some(5));
}

#[test]
fn a_lectern_reads_its_page_progress_plus_one_for_the_book() {
    let book = |pages: usize, page: i32| {
        let mut stack = stack("minecraft:writable_book", 1);
        stack.components.writable_book_content = Some(vec![String::new(); pages]);
        BlockEntity::Lectern(LecternData { book: Some(stack), page })
    };
    assert_eq!(read(BlockEntity::lectern()), Some(0), "no book");
    assert_eq!(read(book(5, 0)), Some(1), "first page: floor(0) + 1");
    assert_eq!(read(book(5, 2)), Some(8), "page 2 of 0..4: 0.5 * 14 = 7, plus one");
    assert_eq!(read(book(5, 4)), Some(15), "last page: 14 + 1");
    assert_eq!(read(book(1, 0)), Some(15), "a one-page book is already at its end");
}

#[test]
fn a_command_block_reads_its_last_success_count() {
    let mut data = crate::command_block::CommandBlockData::new();
    data.success_count = 7;
    assert_eq!(read(BlockEntity::CommandBlock(data)), Some(7));
}

#[test]
fn containers_kept_as_save_data_read_by_the_same_rules() {
    // A jukebox reads its disc's own value: cat 2, pigstep 13, 5 is the loudest at 15.
    for (disc, expected) in [("cat", 2), ("pigstep", 13), ("5", 15), ("13", 1)] {
        let jukebox = opaque("minecraft:jukebox", vec![("RecordItem", item_nbt(&format!("minecraft:music_disc_{disc}"), 1))]);
        assert_eq!(read(jukebox), Some(expected), "{disc}");
    }
    assert_eq!(read(opaque("minecraft:jukebox", vec![])), Some(0));
    // A chiseled bookshelf reads the last touched slot plus one.
    assert_eq!(read(opaque("minecraft:chiseled_bookshelf", vec![("last_interacted_slot", Nbt::Int(3))])), Some(4));
    assert_eq!(read(opaque("minecraft:chiseled_bookshelf", vec![("last_interacted_slot", Nbt::Int(-1))])), Some(0));
    // A decorated pot is a one-slot fill container: 32 / 64 = 0.5; 0.5 * 14 = 7, plus one.
    assert_eq!(read(opaque("minecraft:decorated_pot", vec![("item", item_nbt("minecraft:cobblestone", 32))])), Some(8));
    // A shulker box is 27 slots: one slot of 8 ender pearls is 0.5 / 27 * 14 = 0.26 -> 1.
    let items = Nbt::List {
        element_type: lodestone_core::NbtTag::Compound,
        elements: vec![Nbt::Compound(vec![
            ("Slot".to_owned(), Nbt::Byte(0)),
            ("id".to_owned(), Nbt::String("minecraft:ender_pearl".to_owned())),
            ("count".to_owned(), Nbt::Int(8)),
        ])],
    };
    assert_eq!(read(opaque("minecraft:shulker_box", vec![("Items", items)])), Some(1));
}

#[test]
fn only_blocks_with_an_output_take_over_the_comparator_input() {
    assert!(has_analog_output(Block::Chest) && has_analog_output(Block::WhiteShulkerBox) && has_analog_output(Block::OakShelf));
    assert!(!has_analog_output(Block::Stone) && !has_analog_output(Block::EnderChest));
    assert!(carries_analog(&BlockEntity::container("minecraft:chest")));
    assert!(!carries_analog(&BlockEntity::Comparator { output: 3 }));
}

// -- Chests read the world around them -----------------------------------------------

fn chest_state(spec: &str) -> StateId {
    StateId::from_state_str(spec).unwrap()
}

fn registry_with(entries: Vec<(BlockPos, BlockEntity)>) -> crate::block_entities::BlockEntityRegistry {
    let mut registry = crate::block_entities::BlockEntityRegistry::new();
    for (pos, entity) in entries {
        registry.insert(pos, entity);
    }
    registry
}

#[test]
fn a_double_chest_is_one_inventory_and_a_covered_half_reads_zero() {
    let left = BlockPos::new(4, 1, 8);
    let right = BlockPos::new(4, 1, 9);
    let world = |covered: Option<BlockPos>| {
        move |p: BlockPos| {
            if p == left {
                chest_state("minecraft:chest[facing=east,type=left]")
            } else if p == right {
                chest_state("minecraft:chest[facing=east,type=right]")
            } else if Some(p) == covered.map(|c| BlockPos::new(c.x, c.y + 1, c.z)) {
                Block::Stone.default_state()
            } else {
                air_state()
            }
        }
    };
    let full: Vec<_> = (0..27).map(|slot| (slot, stack("minecraft:cobblestone", 64))).collect();
    let registry = registry_with(vec![
        (left, container("minecraft:chest", 27, &full)),
        (right, container("minecraft:chest", 27, &[])),
    ]);
    let read_from = |covered, at| signal(&registry, &world(covered), at, world(covered)(at), Direction::West);
    // 27 of 54: 0.5 * 14 = 7, plus one, from either half.
    assert_eq!(read_from(None, left), Some(8));
    assert_eq!(read_from(None, right), Some(8));
    // A conductor over either half makes the whole chest unopenable, so it reads zero.
    assert_eq!(read_from(Some(left), left), Some(0));
    assert_eq!(read_from(Some(right), left), Some(0));
    // The single-chest control: the same full chest, alone, is 27 / 27 -> 15.
    let single = registry_with(vec![(left, container("minecraft:chest", 27, &full))]);
    let alone = |p: BlockPos| if p == left { chest_state("minecraft:chest[facing=east,type=single]") } else { air_state() };
    assert_eq!(signal(&single, &alone, left, alone(left), Direction::West), Some(15));
}

#[test]
fn a_chest_partner_is_found_from_either_half() {
    let left = BlockPos::new(4, 1, 8);
    let state = chest_state("minecraft:chest[facing=east,type=left]");
    assert_eq!(chest_partner(left, state), Some(BlockPos::new(4, 1, 9)));
    let state = chest_state("minecraft:chest[facing=east,type=right]");
    assert_eq!(chest_partner(BlockPos::new(4, 1, 9), state), Some(left));
    assert_eq!(chest_partner(left, chest_state("minecraft:chest[facing=east,type=single]")), None);
}

#[test]
fn dirty_tracking_notices_a_change_and_only_a_change() {
    let at = BlockPos::new(4, 1, 8);
    let mut registry = crate::block_entities::BlockEntityRegistry::new();
    assert!(registry.take_analog_dirty().is_empty());
    registry.insert(at, BlockEntity::container("minecraft:chest"));
    registry.insert(BlockPos::new(0, 1, 0), BlockEntity::Comparator { output: 0 });
    assert_eq!(registry.take_analog_dirty(), vec![at], "a comparator has no output of its own");
    assert!(registry.take_analog_dirty().is_empty(), "taking clears");
    // A read leaves it alone; a mutable borrow marks it.
    let _ = registry.get(at);
    assert!(registry.take_analog_dirty().is_empty());
    let _ = registry.get_mut(at);
    assert_eq!(registry.take_analog_dirty(), vec![at]);
    registry.destroy_at(at, StateId::AIR);
    assert_eq!(registry.take_analog_dirty(), vec![at], "a removed container still wakes its comparators");
    // Ticking an idle container never marks it.
    registry.insert(at, BlockEntity::container("minecraft:chest"));
    let _ = registry.take_analog_dirty();
    let _ = registry.tick_all();
    assert!(registry.take_analog_dirty().is_empty());
}

// -- The production tick loop ----------------------------------------------------------

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;
const ROW_Z: i32 = 8;
const COMPARATOR: BlockPos = BlockPos { x: 3, y: 1, z: ROW_Z };
const CHEST: BlockPos = BlockPos { x: 4, y: 1, z: ROW_Z };
const PARTNER: BlockPos = BlockPos { x: 4, y: 1, z: ROW_Z + 1 };
const HOPPER: BlockPos = BlockPos { x: 4, y: 2, z: ROW_Z };

struct Rig(Mutex<ChunkColumn>);

impl Rig {
    fn new(blocks: &[(BlockPos, &str)]) -> Arc<Self> {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for x in 0..16 {
            for z in 0..16 {
                column.set_block_id(x, 0, z, Block::Stone.default_state());
            }
        }
        // A comparator reading east, its output free of any side input.
        column.set_block_id(
            COMPARATOR.x,
            COMPARATOR.y,
            COMPARATOR.z,
            redstone_diode::set_comparator_state(crate::neighbor_update::Direction::East, false, false, 0).state,
        );
        for (pos, spec) in blocks {
            column.set_block_id(pos.x, pos.y, pos.z, chest_state(spec));
        }
        Arc::new(Self(Mutex::new(column)))
    }
}

impl ChunkSource for Rig {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        if (cx, cz) == (0, 0) { self.0.lock().unwrap().clone() } else { ChunkColumn::new(MIN_Y, HEIGHT) }
    }
    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        if (x.div_euclid(16), z.div_euclid(16)) == (0, 0) {
            self.0.lock().unwrap().block_state_id(x.rem_euclid(16), y, z.rem_euclid(16))
        } else {
            air_state()
        }
    }
    fn biome_state_at(&self, _x: i32, _y: i32, _z: i32) -> String {
        "minecraft:plains".to_owned()
    }
    fn set_block(&self, x: i32, y: i32, z: i32, state: StateId) {
        if (x.div_euclid(16), z.div_euclid(16)) == (0, 0) {
            self.0.lock().unwrap().set_block_id(x.rem_euclid(16), y, z.rem_euclid(16), state);
        }
    }
}

struct Loop {
    block_entities: BlockEntityHandle,
    rig: Arc<Rig>,
}

impl Loop {
    async fn start(rig: Arc<Rig>) -> Self {
        let block_entities = BlockEntityHandle::default();
        tokio::spawn(run_tick_loop(
            MobHandle::new(ChunkWorld::new(MIN_Y, HEIGHT)),
            LiveMobSource::default(),
            block_entities.clone(),
            Arc::new(TickClock::new()),
            Arc::clone(&rig),
            BlockTickFeed::default(),
            (0..=0, 0..=0),
            ExplosionFeed::default(),
            crate::region_source::ScheduledTickHandle::default(),
            crate::tick_area::TickFollow::default(),
        ));
        tokio::task::yield_now().await;
        Self { block_entities, rig }
    }

    async fn ticks(&self, n: u64) {
        for _ in 0..n {
            tokio::time::advance(TICK_PERIOD).await;
            tokio::task::yield_now().await;
        }
    }

    /// Edits the container the way a player's click does: through the registry's
    /// mutable borrow, with no word to the comparator.
    fn edit(&self, at: BlockPos, f: impl FnOnce(&mut BlockEntity)) {
        self.block_entities.with(|registry| f(registry.get_mut(at).expect("a container is registered there")));
    }

    fn output(&self) -> u8 {
        self.block_entities.with(|registry| registry.comparator_output(COMPARATOR)).unwrap_or(0)
    }

    fn powered(&self) -> bool {
        redstone::diode_powered(self.rig.block_state_id(COMPARATOR.x, COMPARATOR.y, COMPARATOR.z))
    }
}

const SINGLE_CHEST: &str = "minecraft:chest[facing=east,type=single]";

/// The comparator follows a chest as stacks go in: one item is 1, a full chest 15.
#[tokio::test(start_paused = true)]
async fn a_comparator_beside_a_chest_follows_the_inserted_stacks() {
    let lp = Loop::start(Rig::new(&[(CHEST, SINGLE_CHEST)])).await;
    lp.block_entities.with(|registry| registry.insert(CHEST, BlockEntity::container("minecraft:chest")));
    lp.ticks(6).await;
    assert_eq!(lp.output(), 0, "an empty chest reads 0");
    assert!(!lp.powered());

    lp.edit(CHEST, |chest| chest.set_container_slot(0, Some(stack("minecraft:cobblestone", 1))));
    lp.ticks(6).await;
    assert_eq!(lp.output(), 1, "one item reads 1");
    assert!(lp.powered());

    // 14 full stacks of 27 slots: floor(14 / 27 * 14) + 1 = 8.
    lp.edit(CHEST, |chest| {
        for slot in 0..14 {
            chest.set_container_slot(slot, Some(stack("minecraft:cobblestone", 64)));
        }
    });
    lp.ticks(6).await;
    assert_eq!(lp.output(), 8);

    lp.edit(CHEST, |chest| {
        for slot in 0..27 {
            chest.set_container_slot(slot, Some(stack("minecraft:cobblestone", 64)));
        }
    });
    lp.ticks(6).await;
    assert_eq!(lp.output(), 15, "a full chest reads 15");

    lp.edit(CHEST, |chest| {
        for slot in 0..27 {
            chest.set_container_slot(slot, None);
        }
    });
    lp.ticks(6).await;
    assert_eq!(lp.output(), 0);
    assert!(!lp.powered());
}

/// A double chest is one inventory: 27 full stacks across 54 slots reads 8 until
/// the second half fills too, and the comparator at one half hears the other.
#[tokio::test(start_paused = true)]
async fn a_comparator_beside_a_double_chest_reads_both_halves() {
    let lp = Loop::start(Rig::new(&[
        (CHEST, "minecraft:chest[facing=east,type=left]"),
        (PARTNER, "minecraft:chest[facing=east,type=right]"),
    ]))
    .await;
    lp.block_entities.with(|registry| {
        registry.insert(CHEST, BlockEntity::container("minecraft:chest"));
        registry.insert(PARTNER, BlockEntity::container("minecraft:chest"));
    });
    lp.ticks(6).await;
    assert_eq!(lp.output(), 0);

    // Fill the half the comparator does not touch: 27 / 54 -> floor(7) + 1 = 8.
    lp.edit(PARTNER, |chest| {
        for slot in 0..27 {
            chest.set_container_slot(slot, Some(stack("minecraft:cobblestone", 64)));
        }
    });
    lp.ticks(6).await;
    assert_eq!(lp.output(), 8, "the far half counts, and its comparator-less side woke the near one");

    lp.edit(CHEST, |chest| {
        for slot in 0..27 {
            chest.set_container_slot(slot, Some(stack("minecraft:cobblestone", 64)));
        }
    });
    lp.ticks(6).await;
    assert_eq!(lp.output(), 15, "both halves full");
}

/// A hopper above the chest pushes one item; the chest's reading crosses a step
/// and the comparator follows with nothing but the hopper's own tick to cause it.
///
/// 123 cobblestone is 64/64 + 59/64 = 1.9219 stacks; / 27 * 14 = 0.9965 floors
/// to 0, plus one = 1. 124 is 1.9375 / 27 * 14 = 1.0046 floors to 1, plus one = 2.
#[tokio::test(start_paused = true)]
async fn a_hopper_feeding_a_chest_moves_the_comparator_without_polling() {
    let lp = Loop::start(Rig::new(&[(CHEST, SINGLE_CHEST), (HOPPER, "minecraft:hopper[enabled=true,facing=down]")])).await;
    lp.block_entities.with(|registry| {
        let mut chest = BlockEntity::container("minecraft:chest");
        chest.set_container_slot(0, Some(stack("minecraft:cobblestone", 64)));
        chest.set_container_slot(1, Some(stack("minecraft:cobblestone", 59)));
        registry.insert(CHEST, chest);
        // The hopper holds no item yet, so nothing moves.
        registry.insert(HOPPER, BlockEntity::Hopper(crate::hopper::Hopper::new()));
    });
    lp.ticks(10).await;
    assert_eq!(lp.output(), 1, "123 items read 1");

    // One item in the hopper: it pushes it into the chest on its own.
    lp.edit(HOPPER, |hopper| hopper.set_container_slot(0, Some(stack("minecraft:cobblestone", 1))));
    lp.ticks(12).await;
    let held = lp.block_entities.with(|registry| registry.get(CHEST).map(BlockEntity::container_slots));
    let items: u32 = held.unwrap().iter().flatten().map(|s| s.count).sum();
    assert_eq!(items, 124, "the hopper moved its item into the chest");
    assert_eq!(lp.output(), 2, "124 items read 2");
}
