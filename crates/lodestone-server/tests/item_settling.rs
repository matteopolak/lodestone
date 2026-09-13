//! Dropped items land, stop, and merge.
//!
//! # What was broken
//!
//! `ItemMotion::tick` is the entity's own motion — gravity, translate, drag — and
//! its doc comment has always said that "block collision that would zero a
//! component is the world crate's job and is expressed here through `on_ground`".
//! Nothing ever did that job. `ItemMotion::new` set `on_ground: false` and no code
//! path anywhere wrote it again, so every dropped item accelerated downward
//! forever, fell straight through the terrain, and kept being streamed to the
//! client until its 6000-tick despawn.
//!
//! Merging was the same defect wearing a different hat, which is why this had two
//! symptoms and one fix. `MobSim::merge_neighbouring_items` requires
//! `|dy| < 0.25` — vanilla's search box inflates y by exactly `0.0` — and two
//! stacks dropped even *one tick* apart fall at permanently different speeds. The
//! vertical test could therefore never pass for anything but two items that
//! spawned on the same tick, which is not how a player drops things.
//!
//! # Where the expected values come from
//!
//! Arithmetic over vanilla's own constants rather than our own output:
//! `ITEM_GRAVITY` is `0.04`, `ITEM_AIR_DRAG` is `0.98`, and a resting item's
//! bottom face sits on a block boundary, so an item settling on a floor whose top
//! block is at `y` must come to rest at exactly `y + 1.0`. That is an integer, and
//! the gates assert it exactly.
//!
//! **`y + 1.0` is only right for a full cube, and that was the second half of the
//! defect.** The settling pass took a `Fn(i32, i32, i32) -> bool` oracle and
//! hardcoded the rest height to the top of the cell, so every non-cube surface was
//! wrong. The heights below were read out of `lodestone-data`'s generated
//! collision-shape table — a dump from the real 26.2 server, so an outside source —
//! in a separate step, and then written here as literals rather than recomputed from
//! the same table the implementation reads:
//!
//! | surface block (bare name, as the fixture places it) | true collision top | the boolean's `+1.0` |
//! |---|---|---|
//! | `minecraft:short_grass` | **0.0** (no collision boxes at all) | 1.0 |
//! | `minecraft:oak_slab` | 0.5 | 1.0 |
//! | `minecraft:dirt_path` | 0.9375 | 1.0 |
//! | `minecraft:oak_fence` | **1.5** (uncapped) | 1.0 |
//!
//! Every one of those differs from `1.0`, which is the point: a candidate where the
//! two hypotheses coincide measures only that the code runs. `oak_leaves` was
//! evaluated and **rejected** for exactly that reason — its collision top is
//! `1.0`, so a "surely leaves are not solid" intuition would have produced a gate
//! that passes either way. `oak_fence` is kept because it fails in the *opposite*
//! direction: the old behaviour rested the item too **low**, so a gate that only
//! ever checked for floating would miss it.
//!
//! The fixture's own resolution is asserted separately in
//! [`the_surface_fixtures_resolve_to_the_shapes_the_gates_assume`], because these
//! are *bare* names and the state they resolve to is a property of the census, not
//! of this file.
//!
//! # Controls
//!
//! * [`an_item_over_a_void_column_never_settles_and_is_discarded`] — the same
//!   physics with no floor. It proves the settling is a response to the *terrain*
//!   and not an unconditional clamp, which would make `on_ground` meaningless.
//! * [`two_falling_items_do_not_merge_mid_air`] — the precondition behind the
//!   merge gate. Two items in free fall must **not** merge, so the merge gate is
//!   measuring settling and not a merge rule that ignores position.
//! * [`the_fixture_floor_is_where_the_gates_assume`] — the fixture itself.

use lodestone_entity::item_entity::{ITEM_GRAVITY, ItemLifecycle};
use lodestone_model::{ResourceKey, Vec3};
use lodestone_server::{ChunkColumn, ChunkWorld, MobSim};
use std::str::FromStr;

/// World Y of the floor's top solid block. An item resting on it sits at
/// `FLOOR_TOP_Y + 1`.
const FLOOR_TOP_Y: i32 = 64;

const MIN_Y: i32 = -64;
const HEIGHT: i32 = 384;

/// A single chunk column with a solid floor filling `MIN_Y..=FLOOR_TOP_Y` over
/// local `x` in `0..8`, and **nothing at all** over local `x` in `8..16`.
///
/// The empty half is not decoration: it is the void column the negative control
/// falls down, and having it in the *same* world as the floor means both arms run
/// against one fixture rather than two, so a difference between them cannot be a
/// difference between fixtures.
fn world_with_floor_and_void() -> ChunkWorld {
    let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
    for x in 0..8 {
        for z in 0..16 {
            for y in MIN_Y..=FLOOR_TOP_Y {
                column.set_block(x, y, z, "minecraft:stone");
            }
        }
    }
    ChunkWorld::from_columns([((0, 0), column)])
}

fn diamond() -> ResourceKey {
    ResourceKey::from_str("minecraft:diamond").expect("valid key")
}

/// A stack of `count` diamonds, past its pickup delay so it is mergable — the
/// state a block drop is in a few ticks after it pops.
fn mergable_stack(count: u8) -> ItemLifecycle {
    ItemLifecycle {
        age: 20,
        pickup_delay: 0,
        count,
        max_stack_size: 64,
    }
}

/// The fixture really is what every gate below assumes.
#[test]
fn the_fixture_floor_is_where_the_gates_assume() {
    let world = world_with_floor_and_void();
    assert!(
        world.is_solid(2, FLOOR_TOP_Y, 2),
        "the floor's top block must be solid"
    );
    assert!(
        !world.is_solid(2, FLOOR_TOP_Y + 1, 2),
        "and the cell above it must be free, or an item could not rest there"
    );
    assert!(
        !world.is_solid(12, FLOOR_TOP_Y, 12),
        "the void half must have no floor, or the negative control proves nothing"
    );
    assert!(
        !world.is_solid(12, MIN_Y, 12),
        "the void half must be empty all the way down"
    );
}

/// **The headline.** An item dropped in mid-air comes to rest on the floor, at
/// exactly one block above it, and stays there.
///
/// Predicted exactly, from outside constants: the floor's top block is at
/// `FLOOR_TOP_Y`, so a resting item's bottom face is at `FLOOR_TOP_Y + 1 = 65.0`.
/// A "the item is lower than it started" assertion would pass against the broken
/// implementation, which is the whole point of asserting the value.
#[test]
fn a_dropped_item_settles_on_the_floor_and_stays() {
    let world = world_with_floor_and_void();
    let mut sim = MobSim::new(&world);
    let drop_y = f64::from(FLOOR_TOP_Y) + 20.0;
    let id = sim.spawn_item(
        diamond(),
        Vec3::new(2.5, drop_y, 2.5),
        Vec3::new(0.0, 0.0, 0.0),
        mergable_stack(1),
    );

    // 20 blocks under 0.04 gravity is well inside 200 ticks; the exact count does
    // not matter because the assertion is on the resting position, not on when it
    // is reached.
    for _ in 0..200 {
        sim.tick();
    }

    let resting = f64::from(FLOOR_TOP_Y + 1);
    let position = sim
        .item_position(id)
        .expect("the item must still exist — it landed, it did not despawn");
    assert!(
        (position.y - resting).abs() < 1.0e-9,
        "the item must rest at exactly {resting} (one block above the floor's top \
         block at {FLOOR_TOP_Y}); it is at {}, which for a value far below the floor \
         means it fell straight through the terrain",
        position.y
    );
    assert!(
        (position.x - 2.5).abs() < 1.0e-9 && (position.z - 2.5).abs() < 1.0e-9,
        "and it must not have drifted horizontally: it was dropped with zero \
         horizontal velocity"
    );

    // Still there after another 200 ticks: settled, not merely passing through.
    for _ in 0..200 {
        sim.tick();
    }
    let later = sim.item_position(id).expect("still resting");
    assert!(
        (later.y - resting).abs() < 1.0e-9,
        "the item must still be at {resting} 200 ticks later, not sinking at \
         {ITEM_GRAVITY} per tick; it is at {}",
        later.y
    );
}

/// **The negative control.** The identical drop over the void half of the same
/// world must keep falling and eventually be discarded.
///
/// This is what rules out an unconditional clamp: an implementation that pinned
/// every item to `FLOOR_TOP_Y + 1` regardless of terrain would pass the gate above
/// and fail here.
#[test]
fn an_item_over_a_void_column_never_settles_and_is_discarded() {
    let world = world_with_floor_and_void();
    let mut sim = MobSim::new(&world);
    let drop_y = f64::from(FLOOR_TOP_Y) + 20.0;
    let id = sim.spawn_item(
        diamond(),
        Vec3::new(12.5, drop_y, 12.5),
        Vec3::new(0.0, 0.0, 0.0),
        mergable_stack(1),
    );

    // A short run: still falling, and demonstrably below where the floor would be.
    for _ in 0..100 {
        sim.tick();
    }
    let mid = sim
        .item_position(id)
        .expect("100 ticks is not enough to leave the world");
    assert!(
        mid.y < f64::from(FLOOR_TOP_Y),
        "with no floor the item must be below {FLOOR_TOP_Y} by now; it is at {}",
        mid.y
    );

    // A long run: past `min_y - 64`, so `Entity.checkBelowWorld`'s discard fires.
    // Without it an escaped item is ticked and streamed for its full 6000-tick
    // life at ever-increasing depth.
    for _ in 0..2000 {
        sim.tick();
    }
    assert_eq!(
        sim.item_position(id),
        None,
        "an item that has fallen past min_y - 64 must be discarded"
    );
    assert_eq!(
        sim.item_count(),
        0,
        "and its wire identity must go with it, or the client keeps a ghost"
    );
}

/// **The merge half of item settling.** Two stacks of the same item dropped at *different
/// times* over the same block merge once they have both settled.
///
/// "Different times" is the load-bearing part. Two items spawned on the same tick
/// fall in lockstep, so their `dy` stays `0` and they would merge even under the
/// broken implementation — a gate written that way would have passed throughout.
/// Here the second is dropped 30 ticks after the first, which under the old code
/// left them permanently `dy` apart and unmergable.
///
/// Predicted exactly: `20 + 12 = 32` in one stack, one entity, and it is resting
/// at `FLOOR_TOP_Y + 1`.
#[test]
fn two_stacks_dropped_at_different_times_merge_once_settled() {
    let world = world_with_floor_and_void();
    let mut sim = MobSim::new(&world);
    let drop_y = f64::from(FLOOR_TOP_Y) + 10.0;

    let first = sim.spawn_item(
        diamond(),
        Vec3::new(2.5, drop_y, 2.5),
        Vec3::new(0.0, 0.0, 0.0),
        mergable_stack(20),
    );
    for _ in 0..30 {
        sim.tick();
    }
    let second = sim.spawn_item(
        diamond(),
        Vec3::new(2.5, drop_y, 2.5),
        Vec3::new(0.0, 0.0, 0.0),
        mergable_stack(12),
    );
    assert_ne!(first, second);
    assert_eq!(sim.item_count(), 2, "precondition: two separate entities");

    // Precondition: the two are genuinely at different heights right now, which is
    // the state that used to persist forever.
    let gap = (sim.item_position(first).expect("first").y
        - sim.item_position(second).expect("second").y)
        .abs();
    assert!(
        gap > 0.25,
        "precondition: the two stacks must currently be further apart than the \
         0.25 merge reach ({gap}), or this gate is not exercising the defect"
    );

    for _ in 0..300 {
        sim.tick();
    }

    assert_eq!(
        sim.item_count(),
        1,
        "the two settled stacks must have merged into one entity"
    );
    let survivor = sim
        .item_lifecycle(first)
        .or_else(|| sim.item_lifecycle(second))
        .expect("one of the two ids must survive the merge");
    assert_eq!(
        survivor.count, 32,
        "20 + 12 = 32 diamonds, and both fit in one 64-stack"
    );

    let position = sim
        .item_position(first)
        .or_else(|| sim.item_position(second))
        .expect("the survivor has a position");
    assert!(
        (position.y - f64::from(FLOOR_TOP_Y + 1)).abs() < 1.0e-9,
        "and the survivor is resting on the floor, not merged in mid-air at {}",
        position.y
    );
}

/// **Control for the merge gate**: two items still in free fall must not merge.
///
/// Without this, `two_stacks_dropped_at_different_times_merge_once_settled` would
/// pass against a merge rule that ignored position entirely — which would make
/// every same-item drop in the world collapse into one stack regardless of
/// distance.
#[test]
fn two_falling_items_do_not_merge_mid_air() {
    let world = world_with_floor_and_void();
    let mut sim = MobSim::new(&world);
    // Over the **void** half, so nothing can settle and the two stay airborne for
    // the whole run.
    let high = f64::from(FLOOR_TOP_Y) + 30.0;
    sim.spawn_item(
        diamond(),
        Vec3::new(12.5, high, 12.5),
        Vec3::new(0.0, 0.0, 0.0),
        mergable_stack(20),
    );
    for _ in 0..30 {
        sim.tick();
    }
    sim.spawn_item(
        diamond(),
        Vec3::new(12.5, high, 12.5),
        Vec3::new(0.0, 0.0, 0.0),
        mergable_stack(12),
    );

    for _ in 0..40 {
        sim.tick();
    }
    assert_eq!(
        sim.item_count(),
        2,
        "two items 30 ticks apart in free fall are far more than 0.25 apart \
         vertically and must remain two entities"
    );
}

/// A stack landing on the floor with real horizontal velocity comes to rest
/// *somewhere on the floor*, not below it — the case a vertical-only collision
/// resolution is most likely to get wrong.
///
/// Deliberately not an exact-position assertion: the horizontal decay is a
/// geometric series in `ITEM_AIR_DRAG` and the block friction, and pinning the
/// landing spot would be asserting our own integration order rather than a vanilla
/// fact. What *is* asserted exactly is the resting height, which is a property of
/// the terrain, plus that the item stayed inside the floored half of the world.
#[test]
fn an_item_thrown_sideways_still_comes_to_rest_on_the_floor() {
    let world = world_with_floor_and_void();
    let mut sim = MobSim::new(&world);
    let id = sim.spawn_item(
        diamond(),
        Vec3::new(2.5, f64::from(FLOOR_TOP_Y) + 5.0, 2.5),
        // Toward +z, which stays inside the floored half (x < 8) for any distance.
        Vec3::new(0.0, 0.0, 0.3),
        mergable_stack(1),
    );

    for _ in 0..400 {
        sim.tick();
    }

    let position = sim.item_position(id).expect("the item must still exist");
    assert!(
        (position.y - f64::from(FLOOR_TOP_Y + 1)).abs() < 1.0e-9,
        "resting height is a property of the terrain and must be exact: expected \
         {}, got {}",
        FLOOR_TOP_Y + 1,
        position.y
    );
    assert!(
        position.z > 2.5,
        "premise: it really did travel in +z ({}), so this is a moving landing and \
         not the stationary case already covered",
        position.z
    );
    assert!(
        position.x > 0.0 && position.x < 8.0,
        "and it stayed over the floored half of the fixture ({})",
        position.x
    );
}

// --- The live-terrain oracle -------------------------------------------------
//
// Every gate above builds a `ChunkWorld` fixture and drives `MobSim::tick`, whose
// oracle is that same fixture. They are all green, and they were green while
// dropped items phased through the ground in the actual game — the *world* species
// of vacuous test in CLAUDE.md, where the flaw is in the input data and cannot be
// found by reading the test. Their fixture world **is** the whole world they care
// about, so they structurally cannot exercise the thing that was broken.
//
// What was broken: `MobSim`'s `ChunkWorld` is a static snapshot of `mob_area` —
// 7×7 columns, taken once by `MobHandle::reseed` when the world opens. Outside
// those columns `ChunkWorld::is_solid` answers `false` for every cell, because the
// column is absent rather than empty. So an item dropped anywhere else accelerated
// downward forever and was discarded at `min_y - 64`.
//
// The gates below therefore drop an item at coordinates **outside any snapshot**,
// which is the discriminating input: the two hypotheses ("settle against the live
// world" and "settle against the snapshot") differ there by the entire fall, not
// by a tolerance. At a coordinate the snapshot *does* cover they agree, so a gate
// placed there would measure only that the code runs.

/// A chunk far outside any plausible `mob_area` around the origin.
const FAR_CHUNK: (i32, i32) = (100, 100);

/// Block coordinates inside [`FAR_CHUNK`], at the centre of a cell.
const FAR_X: f64 = 100.0 * 16.0 + 2.5;
const FAR_Z: f64 = 100.0 * 16.0 + 2.5;

/// The live world, as the tick loop sees it: one floored column at
/// [`FAR_CHUNK`], reachable only through the solidity closure and **not** present
/// in the sim's snapshot.
fn far_floor() -> ChunkWorld {
    let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
    for x in 0..16 {
        for z in 0..16 {
            for y in MIN_Y..=FLOOR_TOP_Y {
                column.set_block(x, y, z, "minecraft:stone");
            }
        }
    }
    ChunkWorld::from_columns([(FAR_CHUNK, column)])
}

/// The fixtures really are what the two gates below assume: the live world has a
/// floor at the far coordinate and the sim's snapshot has nothing there.
///
/// Without this the pair could both be measuring an empty world.
#[test]
fn the_far_fixtures_disagree_exactly_where_the_gates_need_them_to() {
    let live = far_floor();
    let snapshot = ChunkWorld::new(MIN_Y, HEIGHT);
    let (bx, bz) = (FAR_X.floor() as i32, FAR_Z.floor() as i32);
    assert!(
        live.is_solid(bx, FLOOR_TOP_Y, bz),
        "the LIVE world must have a floor at the far coordinate"
    );
    assert!(
        !live.is_solid(bx, FLOOR_TOP_Y + 1, bz),
        "and free space above it, or nothing could rest there"
    );
    assert!(
        !snapshot.is_solid(bx, FLOOR_TOP_Y, bz),
        "the SNAPSHOT must have nothing there — that absence is the bug the \
         headline gate below reproduces"
    );
}

/// **The headline.** An item dropped outside the sim's snapshot settles on the
/// live world's floor, at exactly one block above it.
///
/// The expected value comes from the terrain, not from our integrator: the floor's
/// top block is at `FLOOR_TOP_Y`, a resting item's bottom face sits on a block
/// boundary, so it must rest at exactly `FLOOR_TOP_Y + 1`. The drop is 20 blocks,
/// so the wrong hypothesis does not merely land low — it never lands at all and
/// the item is gone. Those two answers cannot be confused by a tolerance.
#[test]
fn an_item_outside_the_snapshot_settles_on_the_live_world() {
    let live = far_floor();
    let snapshot = ChunkWorld::new(MIN_Y, HEIGHT);
    let mut sim = MobSim::new(&snapshot);
    let id = sim.spawn_item(
        diamond(),
        Vec3::new(FAR_X, f64::from(FLOOR_TOP_Y) + 20.0, FAR_Z),
        Vec3::new(0.0, 0.0, 0.0),
        mergable_stack(1),
    );

    for _ in 0..200 {
        sim.tick_with_terrain(&|x, y, z| live.block_state(x, y, z).to_owned());
    }

    let resting = f64::from(FLOOR_TOP_Y + 1);
    let position = sim.item_position(id).expect(
        "the item must still exist: settling against the live world is what stops \
         it falling past min_y - 64 and being discarded",
    );
    assert!(
        (position.y - resting).abs() < 1.0e-9,
        "the item must rest at exactly {resting}, one block above the live floor \
         at {FLOOR_TOP_Y}; it is at {}",
        position.y
    );

    // Still there 200 ticks later: settled, not passing through.
    for _ in 0..200 {
        sim.tick_with_terrain(&|x, y, z| live.block_state(x, y, z).to_owned());
    }
    assert!(
        sim.item_position(id)
            .is_some_and(|p| (p.y - resting).abs() < 1.0e-9),
        "and it must still be resting there, not sinking at {ITEM_GRAVITY} per tick"
    );
}

// ---------------------------------------------------------------------------
// Non-cube surfaces: the half the boolean oracle could not express.
// ---------------------------------------------------------------------------

/// Each surface block, the local `x` the fixture places it at, and the rest height
/// an item settling on it must reach.
///
/// The heights are **literals transcribed from `lodestone-data`'s shape table**, not
/// recomputed here — see this file's header for the table and for why each one is
/// discriminating. `FLOOR_TOP_Y + 1` is the top of the stone floor, i.e. where the
/// surface block itself sits, so a surface with collision top `t` rests an item at
/// `FLOOR_TOP_Y + 1 + t`.
const SURFACES: &[(&str, i32, f64)] = &[
    // No collision boxes at all: the item falls straight through and lands on the
    // stone. This is the case with the visible symptom — almost any grassy surface
    // has a plant on it, so almost every dropped item floated a block up.
    ("minecraft:short_grass", 1, 0.0),
    ("minecraft:oak_slab", 3, 0.5),
    ("minecraft:dirt_path", 5, 0.9375),
    // The one that fails the *other* way: 1.5 is above the cell, so the old
    // behaviour rested the item too low, inside the fence post.
    ("minecraft:oak_fence", 7, 1.5),
];

/// A stone floor filling `MIN_Y..=FLOOR_TOP_Y` across the whole column, with each
/// [`SURFACES`] block sitting on top of it at its own local `x`.
///
/// One fixture for all four arms, so a difference between them cannot be a
/// difference between worlds — the same discipline
/// [`world_with_floor_and_void`] already uses for its floor/void pair.
fn world_with_surfaces() -> ChunkWorld {
    let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
    for x in 0..16 {
        for z in 0..16 {
            for y in MIN_Y..=FLOOR_TOP_Y {
                column.set_block(x, y, z, "minecraft:stone");
            }
        }
    }
    for &(name, x, _) in SURFACES {
        column.set_block(x, FLOOR_TOP_Y + 1, 8, name);
    }
    ChunkWorld::from_columns([((0, 0), column)])
}

/// The fixture places what it thinks it places, and each surface resolves to a
/// state whose collision top is the literal the gate below asserts against.
///
/// **A precondition, not a duplicate of the gate.** These are *bare* block names
/// (`minecraft:oak_slab`, not `minecraft:oak_slab[type=bottom,waterlogged=false]`),
/// and which state a bare name resolves to is a property of the census rather than of
/// this file — a default that moved would silently retarget every arm below at a
/// different shape, and the rest-height assertions would then be measuring something
/// nobody chose. It reads the table because the fixture's *identity* is the claim
/// here; the rest heights stay literals.
#[test]
fn the_surface_fixtures_resolve_to_the_shapes_the_gates_assume() {
    let world = world_with_surfaces();
    for &(name, x, expected_top) in SURFACES {
        assert_eq!(
            world.block_state(x, FLOOR_TOP_Y + 1, 8),
            name,
            "the fixture must actually hold {name} at x={x}"
        );
        let id = lodestone_data::block_states::state_id(name)
            .unwrap_or_else(|| panic!("{name} must be in the 26.2 block-state census"));
        let state = lodestone_data::block_states::StateId::new(id).expect("fixture state validates");
        let boxes = lodestone_data::collision_shapes::collision_boxes(state);
        let top = boxes
            .iter()
            .map(|b| f64::from(b.max[1]))
            .fold(0.0_f64, f64::max);
        assert!(
            (top - expected_top).abs() < 1.0e-9,
            "{name} resolves to state {id} with collision top {top}, but the gate \
             asserts a rest height derived from {expected_top}. Either the census \
             moved or the bare name now resolves to a different default state — \
             re-read the table before touching the expected height."
        );
        assert!(
            (top - 1.0).abs() > 1.0e-9,
            "{name} has collision top {top}, which equals the full-cube hypothesis: \
             this arm cannot distinguish the two implementations and must be replaced \
             with a block whose shape differs from a full cube"
        );
    }
    assert!(
        world.is_solid(1, FLOOR_TOP_Y, 8),
        "and the stone floor must be under all of them"
    );
}

/// **The headline for this half.** An item dropped onto each non-cube surface comes
/// to rest at the top of that surface's *real* collision shape.
///
/// Two hypotheses per arm, both computed from outside constants:
///
/// * **full cube** (the old boolean oracle): `FLOOR_TOP_Y + 2.0` for every arm, since
///   the surface block occupies the cell at `FLOOR_TOP_Y + 1` and the item was pinned
///   to the top of whatever cell it was in.
/// * **real shape**: `FLOOR_TOP_Y + 1 + top`, with `top` from the table.
///
/// The assertion lands on the second and the failure message names the first, so a
/// regression reads as "this is the full-cube bug" rather than as an unexplained
/// number. `short_grass` and `oak_fence` are a full block and a half block away from
/// the wrong answer respectively; `dirt_path` is only `0.0625` away, which is why the
/// tolerance is `1.0e-9` and not something forgiving.
///
/// **Every arm is reported, not just the first.** A `for` loop with an `assert!`
/// inside stops at arm one, so a control run could only ever demonstrate one arm and
/// the other three would be arguments rather than observations. Collecting the
/// mismatches and asserting the collection is empty is the same "make failure name
/// where" discipline a bounding box serves in the pixel gates.
#[test]
fn an_item_rests_on_the_real_collision_shape_of_each_surface() {
    let full_cube_hypothesis = f64::from(FLOOR_TOP_Y + 2);
    let mut mismatches: Vec<String> = Vec::new();

    for &(name, x, top) in SURFACES {
        let world = world_with_surfaces();
        let mut sim = MobSim::new(&world);
        let expected = f64::from(FLOOR_TOP_Y + 1) + top;
        let id = sim.spawn_item(
            diamond(),
            Vec3::new(f64::from(x) + 0.5, f64::from(FLOOR_TOP_Y) + 20.0, 8.5),
            Vec3::new(0.0, 0.0, 0.0),
            mergable_stack(1),
        );

        for _ in 0..300 {
            sim.tick();
        }

        let Some(position) = sim.item_position(id) else {
            mismatches.push(format!(
                "{name}: the item left the world, so the sweep found no collision at all"
            ));
            continue;
        };
        if (position.y - expected).abs() >= 1.0e-9 {
            mismatches.push(format!(
                "{name}: rests at {} but must rest at {expected} (stone top {} plus the \
                 shape's own {top}); the full-cube hypothesis predicts \
                 {full_cube_hypothesis}",
                position.y,
                FLOOR_TOP_Y + 1,
            ));
            continue;
        }

        // Settled, not passing through: 300 more ticks must not move it.
        for _ in 0..300 {
            sim.tick();
        }
        match sim.item_position(id) {
            Some(later) if (later.y - expected).abs() < 1.0e-9 => {}
            Some(later) => mismatches.push(format!(
                "{name}: landed at {expected} but crept to {} over 300 further ticks",
                later.y
            )),
            None => mismatches.push(format!(
                "{name}: landed at {expected} and then left the world"
            )),
        }
    }

    assert!(
        mismatches.is_empty(),
        "{} of {} surfaces settled at the wrong height:\n  {}",
        mismatches.len(),
        SURFACES.len(),
        mismatches.join("\n  "),
    );
}

/// **Horizontal collision, which the point test could not see at all.**
///
/// The old pass tested one column at the item's own centre and resolved vertically
/// only. Its doc comment argued that was safe because an item's horizontal velocity
/// decays before it can cross a wall — true for a slow item, and false for a thrown
/// one, which is what this drops.
///
/// The wall is stone, so this is *not* a shape claim: it is the claim that the
/// horizontal axes are resolved at all. An item launched hard at a wall one block
/// away must end up on the near side of it, and the near face is at `x = 3.0`, so with
/// a half-width of `0.125` the furthest its centre can reach is `2.875`. Under the old
/// behaviour it passed straight through and kept going, so the two hypotheses are
/// "bounded by 2.875" and "unbounded" — no tolerance can confuse them.
#[test]
fn a_thrown_item_stops_against_a_wall_instead_of_passing_through_it() {
    let mut column = ChunkColumn::new(MIN_Y, HEIGHT);
    for x in 0..16 {
        for z in 0..16 {
            for y in MIN_Y..=FLOOR_TOP_Y {
                column.set_block(x, y, z, "minecraft:stone");
            }
        }
    }
    // A wall two cells thick at local x = 3..4, so even a fast item cannot be on the
    // far side of it by rounding.
    for y in FLOOR_TOP_Y + 1..=FLOOR_TOP_Y + 3 {
        for z in 0..16 {
            column.set_block(3, y, z, "minecraft:stone");
            column.set_block(4, y, z, "minecraft:stone");
        }
    }
    let world = ChunkWorld::from_columns([((0, 0), column)]);
    let mut sim = MobSim::new(&world);
    let id = sim.spawn_item(
        diamond(),
        Vec3::new(1.5, f64::from(FLOOR_TOP_Y) + 1.0, 8.5),
        // Hard enough that a single tick would clear the wall outright without a
        // sweep — which is also what makes this a test of the sweep and not of drag.
        Vec3::new(1.2, 0.0, 0.0),
        mergable_stack(1),
    );

    for _ in 0..200 {
        sim.tick();
    }

    let position = sim
        .item_position(id)
        .expect("the item must still exist — it hit a wall, it did not leave the world");
    assert!(
        position.x <= 2.875 + 1.0e-9,
        "the item must stop on the near side of the wall whose face is at x = 3.0: \
         with a 0.25-wide hitbox its centre cannot pass 2.875. It is at {}, which \
         beyond the wall means horizontal movement is not being resolved at all",
        position.x
    );
    assert!(
        position.x > 1.5,
        "and it must actually have travelled — an item that never moved would satisfy \
         the bound above without testing anything"
    );
}

/// **The cost of the sweep, bounded as a counter and asserted for linearity.**
///
/// Swept collision against real per-state shapes is strictly more work per item than
/// one boolean lookup was, and nothing bounds how many items sit on a floor. The
/// per-item number is not the interesting one — this repo has already shipped a
/// latency defect whose per-unit cost was fine and whose single unserviced window was
/// not — so what this asserts is that the *tick* cost grows **linearly** with item
/// count and not faster.
///
/// Two arms, and the expectation is derived rather than observed: items are settled
/// independently in one pass, so probes-per-item must be the same at 1 item and at 64.
/// A quadratic pass (each item consulting the others) would show 64× the per-item
/// count at 64 items. The bound is 3×, which is nowhere near 64 and leaves room for
/// the differing sweep lengths of items at slightly different speeds.
///
/// **Measured: 36 probes per item at 1 item and 2,304 at 64 — exactly 36 both times.**
/// So a floor holding 200 dropped items costs ~7,200 cell probes in the settling pass
/// of one tick, each a `String` from the oracle plus a name→id lookup plus an O(1)
/// rodata index. That is the number to re-read if the item tick ever shows up in the
/// tick loop's own overrun accounting; the second bound below is what catches a
/// per-item regression, since linearity alone would be satisfied by a pass that got
/// ten times more expensive uniformly.
#[test]
fn the_settling_sweep_costs_a_constant_number_of_probes_per_item() {
    let world = world_with_surfaces();

    let probes_for = |count: i32| -> u64 {
        let mut sim = MobSim::new(&world);
        for i in 0..count {
            sim.spawn_item(
                diamond(),
                // Spread over the floor so they do not merge into one entity, which
                // would make the 64-item arm secretly a 1-item arm.
                Vec3::new(
                    f64::from(i % 8) + 0.5,
                    f64::from(FLOOR_TOP_Y) + 4.0 + f64::from(i),
                    f64::from(i / 8) + 0.5,
                ),
                Vec3::new(0.0, 0.0, 0.0),
                mergable_stack(1),
            );
        }
        // One tick, measured: the counter is per-tick, so this is the cost of a tick
        // holding `count` items.
        sim.tick();
        assert_eq!(
            sim.item_count(),
            count as usize,
            "precondition: all {count} items must still be live, or this arm is \
             measuring fewer items than it thinks"
        );
        sim.items_settled_probe_count()
    };

    let one = probes_for(1);
    let many = probes_for(64);
    assert!(
        one > 0,
        "precondition: settling one item must probe at least one cell, or the counter \
         is not wired and this gate measures nothing"
    );
    let per_item_one = one as f64;
    let per_item_many = many as f64 / 64.0;
    assert!(
        per_item_many <= per_item_one * 3.0,
        "probes per item must not grow with item count: {per_item_one} at 1 item \
         against {per_item_many} at 64 ({many} total). A quadratic settling pass would \
         show about {} per item here",
        per_item_one * 64.0
    );
    // The absolute bound, because linearity alone is satisfied by a pass that became
    // uniformly ten times more expensive. 36 was measured; 150 is generous enough not
    // to be a tripwire on an unrelated sweep-length change and tight enough to catch
    // an order of magnitude.
    assert!(
        per_item_one <= 150.0,
        "one item's settling sweep probed {per_item_one} cells; it measured 36 when \
         this was written, so an order-of-magnitude rise means the sweep is spanning \
         far more cells than the item's own box plus its movement"
    );
}

/// **The control that proves the gate above measures the fix.** The identical
/// drop, driven through [`MobSim::tick`] — whose oracle is the snapshot — must
/// phase straight through and be discarded.
///
/// This is the bug as the player met it, pinned as a test. Without this arm the
/// headline gate is satisfied by any implementation that settles items at all,
/// including the broken one, because nothing would establish that the snapshot
/// arm behaves differently at this coordinate.
#[test]
fn the_same_drop_through_the_snapshot_oracle_still_falls_out_of_the_world() {
    let snapshot = ChunkWorld::new(MIN_Y, HEIGHT);
    let mut sim = MobSim::new(&snapshot);
    let id = sim.spawn_item(
        diamond(),
        Vec3::new(FAR_X, f64::from(FLOOR_TOP_Y) + 20.0, FAR_Z),
        Vec3::new(0.0, 0.0, 0.0),
        mergable_stack(1),
    );

    for _ in 0..100 {
        sim.tick();
    }
    let mid = sim
        .item_position(id)
        .expect("100 ticks is not enough to leave the world");
    assert!(
        mid.y < f64::from(FLOOR_TOP_Y),
        "with the snapshot as its oracle the item must already be below the floor \
         height {FLOOR_TOP_Y}; it is at {}",
        mid.y
    );

    for _ in 0..2000 {
        sim.tick();
    }
    assert_eq!(
        sim.item_position(id),
        None,
        "and it must eventually fall past min_y - 64 and be discarded — this is \
         exactly what the player saw, and what the live-terrain oracle fixes"
    );
}
