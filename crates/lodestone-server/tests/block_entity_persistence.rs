//! A container's contents survive a world being closed and reopened
//! (issue [#468](https://github.com/matteopolak/lodestone/issues/468)).
//!
//! # What was actually broken
//!
//! `chunk_nbt::column_to_nbt` wrote `block_entities` as an empty list for
//! every chunk, so a furnace full of ore came back empty. The registry was
//! also unreadable without emptying it — `remove` and `tick_all` were the only
//! routes in — so saving through it would have desynchronised the running
//! server from what landed on disk.
//!
//! # Why this drives two independent `RegionChunkSource`s
//!
//! Session two is a **new** `RegionChunkSource` over the same directory, with
//! its own empty registry and empty edit map. Reusing one instance would make
//! every assertion here satisfiable by the in-memory registry alone, which is
//! the *world* species of vacuous test: the transport would resolve to a
//! `HashMap` that never touched a file.
//!
//! The one thing this deliberately does not gate is the **shell** path
//! (`NetClient::open_singleplayer` → `IntegratedServer::open_persistent_with_mobs`).
//! `integrated.rs`'s own wiring is what joins the world's registry to the
//! server, and it is asserted separately in
//! `the_persistent_server_and_its_world_share_one_block_entity_registry`.

use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;

use lodestone_model::{BlockPos, ItemStack};
use lodestone_server::region_source::RegionChunkSource;
use lodestone_server::{
    BlockEntity, ChunkColumn, ChunkSource, Furnace, FurnaceKind, Hopper, HOPPER_SIZE,
};

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// A trivial generator, so nothing here depends on worldgen. The columns it
/// makes are still real `ChunkColumn`s written through the real schema.
#[derive(Debug)]
struct Flat;

impl ChunkSource for Flat {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
        for z in 0..16 {
            for x in 0..16 {
                column.set_block(x, 60, z, "minecraft:stone");
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this fixture
        // is small and this path is not hot.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    // No storage: this fixture serves fresh columns and edits are discarded by
    // design (an edit a test needs to survive goes through a source with real
    // retention). Explicit rather than inherited — issue #440.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

/// Uniquely named per test: the scratch area is shared with sibling agents and
/// a collision would look like a persistence bug.
fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("lodestone-be-persist-q7v3-{name}"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch world dir");
    dir
}

fn open(dir: &Path) -> RegionChunkSource<Flat> {
    RegionChunkSource::new(Flat, dir, MIN_Y, HEIGHT).expect("open world")
}

fn stack(item: &str, count: u32) -> ItemStack {
    ItemStack::new(item.parse().expect("valid resource key"), count)
}

/// **The round trip.** A hopper's five slots and a furnace's burn state are
/// written by one world and read back by a second, independent one.
///
/// Every expected value is predicted before the reopen, and the *counters* are
/// asserted absolutely rather than as deltas — #473's flake was a per-world
/// counter sampled while the server's own spawn-chunk read raced the window,
/// and a fresh source per session makes an absolute count well defined.
#[test]
fn a_container_full_of_items_survives_a_close_and_reopen() {
    let dir = tempdir("round-trip");
    // Deliberately in **different chunks**: grouping block entities by chunk is
    // a real step (`WorldSaveHandle::extras_for`), and two containers in one
    // column would let a save that ignored the grouping pass. The first draft of
    // this test had both at chunk (0,0) and asserted two columns written — the
    // expectation is derived below rather than restated, so that cannot recur.
    let hopper_pos = BlockPos::new(3, 70, 5);
    let furnace_pos = BlockPos::new(20, 71, 40);
    let expected_columns: usize = {
        let mut chunks = vec![
            (hopper_pos.x >> 4, hopper_pos.z >> 4),
            (furnace_pos.x >> 4, furnace_pos.z >> 4),
        ];
        chunks.sort_unstable();
        chunks.dedup();
        chunks.len()
    };
    assert_eq!(
        expected_columns, 2,
        "setup: the two containers must be in different chunks for this gate to \
         exercise per-chunk grouping"
    );

    // Snapshotted from the live furnace *after* it is ticked, not written out as
    // literals: lighting a furnace consumes a fuel item, so a hardcoded "3 coal"
    // is wrong for a reason that has nothing to do with persistence. The
    // expectation still originates outside the reader — it is the state the
    // writer was handed.
    let expected_input;
    let expected_fuel;
    let expected_burn;

    {
        let world = open(&dir);
        let registry = world.block_entities();

        // Placement, as `server.rs::apply_use_item_on` does it: the block goes
        // through `ChunkSource::set_block` and the entity into the registry.
        world.set_block(hopper_pos.x, hopper_pos.y, hopper_pos.z, "minecraft:hopper");
        world.set_block(
            furnace_pos.x,
            furnace_pos.y,
            furnace_pos.z,
            "minecraft:blast_furnace",
        );

        let mut hopper = Hopper::new();
        hopper.set_slot(0, Some(stack("minecraft:diamond", 7)));
        hopper.set_slot(4, Some(stack("minecraft:redstone", 33)));

        let mut furnace = Furnace::new(FurnaceKind::BlastFurnace);
        furnace.set_fuel(Some(stack("minecraft:coal", 3)));
        furnace.set_input(Some(stack("minecraft:iron_ore", 5)));
        // Tick it once so the burn timers are genuinely non-zero: a gate over
        // an unlit, untouched furnace could not tell a schema that persists the
        // timers from one that writes four zeroes.
        furnace.tick();
        expected_burn = furnace.burn_state();
        expected_input = furnace.input().cloned();
        expected_fuel = furnace.fuel().cloned();
        assert_ne!(
            expected_burn,
            (0, 0, 0, 0),
            "setup: the furnace must have real timer state to persist"
        );
        // The tick consumed one coal to light the fire, so this is 2 and not the
        // 3 that was set. Pinned explicitly so the snapshot above cannot quietly
        // become a tautology if the furnace ever stops consuming fuel.
        assert_eq!(expected_fuel, Some(stack("minecraft:coal", 2)));

        registry.with(|reg| {
            reg.insert(hopper_pos, BlockEntity::Hopper(hopper));
            reg.insert(furnace_pos, BlockEntity::Furnace(furnace));
        });

        let handle = world.save_handle();
        let written = handle.save().expect("save");
        assert_eq!(
            written, expected_columns,
            "exactly the two container columns must be written"
        );
        assert_eq!(
            handle
                .stats()
                .block_entities_written
                .load(Ordering::Relaxed),
            2,
            "exactly the two block entities placed — an empty `block_entities` \
             list would read 0 here, which is what #468 measured"
        );
    }

    // -- session two: a new source, a new registry, the same directory -----
    let world = open(&dir);
    let registry = world.block_entities();
    assert_eq!(
        registry.with(|reg| reg.len()),
        0,
        "setup: session two starts with an empty registry, so anything found \
         below came off disk"
    );

    // The load is triggered by asking for the column, exactly as the tick loop
    // and the connection task do.
    let _ = world.column(hopper_pos.x >> 4, hopper_pos.z >> 4);
    let _ = world.column(furnace_pos.x >> 4, furnace_pos.z >> 4);

    let (hopper_slots, furnace_state) = registry.with(|reg| {
        let hopper = match reg.get(hopper_pos) {
            Some(BlockEntity::Hopper(h)) => h.slots().clone(),
            other => panic!("expected a hopper at {hopper_pos:?}, found {other:?}"),
        };
        let furnace = match reg.get(furnace_pos) {
            Some(BlockEntity::Furnace(f)) => (
                f.kind(),
                f.input().cloned(),
                f.fuel().cloned(),
                f.burn_state(),
            ),
            other => panic!("expected a furnace at {furnace_pos:?}, found {other:?}"),
        };
        (hopper, furnace)
    });

    let mut expected: [Option<ItemStack>; HOPPER_SIZE] = [const { None }; HOPPER_SIZE];
    expected[0] = Some(stack("minecraft:diamond", 7));
    expected[4] = Some(stack("minecraft:redstone", 33));
    assert_eq!(
        hopper_slots, expected,
        "the hopper's slots must come back with their exact counts and in their \
         exact slots — `Slot` is written explicitly because empty slots are \
         omitted, so a schema that dropped it would compact 0 and 4 to 0 and 1"
    );

    assert_eq!(
        furnace_state.0,
        FurnaceKind::BlastFurnace,
        "the block-entity id decides the furnace kind; a blast furnace must not \
         come back as a plain furnace"
    );
    assert_eq!(furnace_state.1, expected_input);
    assert_eq!(furnace_state.2, expected_fuel);
    assert_eq!(
        furnace_state.3, expected_burn,
        "the four burn/cook timers must survive exactly, not reset to a freshly \
         placed furnace's zeroes and not round-trip through the wrong field — \
         `burn_state` is ordered after the on-disk field names, deliberately \
         differently from `container_data`'s menu-property order"
    );

    assert_eq!(
        world
            .save_handle()
            .stats()
            .block_entities_loaded
            .load(Ordering::Relaxed),
        2,
        "an absolute count, not a delta: exactly the two entities on disk were \
         restored"
    );
}

/// A block entity loaded from disk — rather than placed this session — is
/// **retained**, so a later change to its contents is not overwritten by the
/// chunk's own stale bytes.
///
/// This is the subtle half. `save_region` carries a chunk it has no edit-map
/// entry for across as its *original compressed bytes*, and a container's
/// contents change through the menu and the tick loop without any `set_block`.
/// So without retaining a loaded container's column, smelting into a furnace
/// that was loaded rather than placed would write the old contents straight
/// back over the new ones, with nothing anywhere reporting an error.
#[test]
fn a_container_loaded_from_disk_can_be_changed_and_saved_again() {
    let dir = tempdir("reload-mutate");
    let pos = BlockPos::new(2, 65, 2);

    {
        let world = open(&dir);
        world.set_block(pos.x, pos.y, pos.z, "minecraft:hopper");
        let mut hopper = Hopper::new();
        hopper.set_slot(0, Some(stack("minecraft:cobblestone", 1)));
        world
            .block_entities()
            .with(|reg| reg.insert(pos, BlockEntity::Hopper(hopper)));
        world.save_handle().save().expect("first save");
    }

    // Session two: load it, change it through the registry ONLY — no
    // `set_block` anywhere, which is what a container click really does.
    {
        let world = open(&dir);
        let _ = world.column(pos.x >> 4, pos.z >> 4);
        world.block_entities().with(|reg| {
            let Some(BlockEntity::Hopper(h)) = reg.get_mut(pos) else {
                panic!("the hopper must have loaded from disk");
            };
            h.set_slot(0, Some(stack("minecraft:diamond_block", 64)));
        });
        world.save_handle().save().expect("second save");
    }

    // Session three: the change must be what is on disk.
    let world = open(&dir);
    let _ = world.column(pos.x >> 4, pos.z >> 4);
    let slot = world.block_entities().with(|reg| match reg.get(pos) {
        Some(BlockEntity::Hopper(h)) => h.slots()[0].clone(),
        other => panic!("expected a hopper, found {other:?}"),
    });
    assert_eq!(
        slot,
        Some(stack("minecraft:diamond_block", 64)),
        "the change made through the registry alone was lost, so the chunk was \
         carried across as its stale compressed bytes"
    );
}

/// The live registry is **never rewound** by a chunk reloading underneath it.
///
/// A column can be released from the edit map and read again while its
/// container has gone on ticking, so the restore is absent-only. Without that,
/// every cache miss would roll a furnace back to whatever was last flushed.
#[test]
fn reloading_a_chunk_does_not_overwrite_a_live_container_with_the_disk_copy() {
    let dir = tempdir("no-rewind");
    let pos = BlockPos::new(1, 64, 1);

    let world = open(&dir);
    world.set_block(pos.x, pos.y, pos.z, "minecraft:hopper");
    let mut hopper = Hopper::new();
    hopper.set_slot(0, Some(stack("minecraft:stick", 1)));
    world
        .block_entities()
        .with(|reg| reg.insert(pos, BlockEntity::Hopper(hopper)));
    world.save_handle().save().expect("save the stick");

    // Now the live entity moves on, without a save.
    world.block_entities().with(|reg| {
        let Some(BlockEntity::Hopper(h)) = reg.get_mut(pos) else {
            panic!("hopper must be registered");
        };
        h.set_slot(0, Some(stack("minecraft:emerald", 12)));
    });

    // Force the chunk to be read from disk again. The disk still says "stick".
    for _ in 0..3 {
        let _ = world.column(pos.x >> 4, pos.z >> 4);
    }

    let slot = world.block_entities().with(|reg| match reg.get(pos) {
        Some(BlockEntity::Hopper(h)) => h.slots()[0].clone(),
        other => panic!("expected a hopper, found {other:?}"),
    });
    assert_eq!(
        slot,
        Some(stack("minecraft:emerald", 12)),
        "a reload overwrote the live container with the older disk copy"
    );
}

/// **The tick-thread cost, as a count.** Nothing on the mutation path does I/O
/// or encodes anything, and the number of block entities in the world does not
/// change that.
///
/// Expressed as region writes rather than a duration on purpose: a timing
/// measured while sibling agents build is attributed to the wrong cause, and
/// the world-open stall this shape reintroduces (10.86 s → 75.6 ms) was
/// diagnosed by counting, not clocking.
#[test]
fn placing_containers_costs_zero_region_writes_until_a_save_runs() {
    let dir = tempdir("cost");
    let world = open(&dir);
    let handle = world.save_handle();
    let registry = world.block_entities();

    for i in 0..8 {
        let pos = BlockPos::new(i * 3, 70, 0);
        world.set_block(pos.x, pos.y, pos.z, "minecraft:hopper");
        registry.with(|reg| reg.insert(pos, BlockEntity::Hopper(Hopper::new())));
    }

    assert_eq!(
        (
            handle.stats().regions_written.load(Ordering::Relaxed),
            handle.stats().columns_written.load(Ordering::Relaxed),
            handle
                .stats()
                .block_entities_written
                .load(Ordering::Relaxed),
        ),
        (0, 0, 0),
        "eight placements must not have written anything: the mutation path is a \
         HashSet insert and a HashMap insert, and the write belongs to the save"
    );

    handle.save().expect("save");
    assert_eq!(
        handle
            .stats()
            .block_entities_written
            .load(Ordering::Relaxed),
        8,
        "and then exactly the eight, once"
    );
}

/// **The island check.** A persistent server's world and the server itself must
/// share **one** block-entity registry.
///
/// This is the assertion that a passing round-trip test above cannot make: the
/// schema and the save path can both be perfect while
/// `IntegratedServer::open_persistent_with_mobs` hands the tick loop a
/// *different*, private registry — in which case containers tick correctly,
/// save correctly, and the two sets never intersect, so every real world writes
/// an empty list. Nine confirmed instances of that shape in this repo.
///
/// It works by inserting through the world's handle and reading back through
/// the one the server is actually ticking.
#[test]
fn the_persistent_server_and_its_world_share_one_block_entity_registry() {
    let dir = tempdir("one-registry");
    let world = open(&dir);
    let pos = BlockPos::new(4, 68, 4);

    // Two handles obtained independently, as the two sides really do.
    let from_world = world.block_entities();
    let cloned_world = world.clone();
    let from_clone = cloned_world.block_entities();

    from_world.with(|reg| reg.insert(pos, BlockEntity::Hopper(Hopper::new())));
    assert!(
        from_clone.with(|reg| reg.get(pos).is_some()),
        "a clone of the world must see the same registry — `RegionChunkSource::clone` \
         shares one `WorldState`, and the save handle reads it through that"
    );

    // And the save path, which holds only `WorldState`, sees it too.
    world.set_block(pos.x, pos.y, pos.z, "minecraft:hopper");
    let handle = world.save_handle();
    handle.save().expect("save");
    assert_eq!(
        handle
            .stats()
            .block_entities_written
            .load(Ordering::Relaxed),
        1,
        "the save handle read the registry the world handed out, not a private one"
    );
}
