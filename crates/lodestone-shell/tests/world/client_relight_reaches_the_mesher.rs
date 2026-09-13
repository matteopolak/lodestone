//! The island check for the client's own relight: is
//! [`mesher::relight_changed_blocks`] actually *registered*, does it actually run
//! the light engine, and does its result actually reach a mesh job?
//!
//! # Why this file exists separately from the engine's own gates
//!
//! `lodestone-world`'s `tests/client_relight.rs` proves the light engine computes
//! the right numbers. That is a **closed loop** with respect to the shell: the
//! engine can be entirely correct and still reach zero pixels, because nothing in
//! that crate says who calls it. The dominant defect class in this repo is exactly
//! that shape, and it has nine confirmed instances.
//!
//! So this file asserts the three links the engine's own suite structurally cannot
//! see:
//!
//! 1. `TerrainPlugin` registers the system in the `Update` schedule `Sim::step`
//!    runs. A system that is correct, in the right set, and never registered
//!    compiles and passes every unit test.
//! 2. Running that schedule drains `World`'s pending-relight queue, so the write
//!    path and the drain agree about who owns it.
//! 3. The sections the relight reports become **real mesh jobs**, not just an
//!    emptied set. Light that changes and dirties no mesh changes nothing on
//!    screen.
//!
//! # Which id space this exercises
//!
//! The demo palette, via `DemoLightProps` — `ColumnSource::Complete`, the
//! non-vanilla arm of the props fork in `relight_changed_blocks`. That is
//! deliberate and it is the honest scope: the wiring is id-space agnostic, and a
//! hermetic harness cannot supply the vanilla atlas that would select the other
//! arm. The 26.2 props table's own correctness is `lodestone-data`'s gate, not
//! this one's.

use lodestone_ecs::app::App;
use lodestone_ecs::ecs::world::World as EcsWorld;
use lodestone_ecs::{ChunkWorld, ChunkWorldWrite, Update};
use lodestone::blocks::{DemoClassifier, DemoLightProps, ShellClassifier, id};
use lodestone::mesher::{MeshScheduler, TerrainMesh, TerrainPlugin};
use lodestone_world::{
    ChunkColumn, ChunkPos, Heightmaps, LightData, LoadedChunk, NibbleArray, PaletteKind, World,
    compute_column_light,
};

const MIN_Y: i32 = -64;
const SECTION_COUNT: usize = 24;
/// A solid roof, so the cell under it is lit sideways rather than from above — the
/// only shape that separates a real relight from a vertical flood.
const CEILING_Y: i32 = -40;
/// The subject: a lone stone block under the roof.
const BREAK: [i32; 3] = [4, CEILING_Y - 2, 4];

/// One column: stone floor, stone roof with a 3×3 skylight, air between, and a lone
/// stone block at [`BREAK`].
fn scene_column() -> ChunkColumn {
    let mut c = ChunkColumn::new(
        MIN_Y,
        SECTION_COUNT,
        PaletteKind::block_states(),
        PaletteKind::biomes(),
        id::AIR,
        0,
    );
    for z in 0..16usize {
        for x in 0..16usize {
            c.set_block(x, MIN_Y, z, id::STONE);
            if !((7..=9).contains(&x) && (7..=9).contains(&z)) {
                c.set_block(x, CEILING_Y, z, id::STONE);
            }
        }
    }
    c.set_block(BREAK[0] as usize, BREAK[1], BREAK[2] as usize, id::STONE);
    c
}

/// A 3×3 of loaded columns carrying [`scene_column`] and its computed light, so the
/// relight's box has real neighbours rather than barriers.
fn scene_world() -> World {
    let mut world = World::new();
    let column = scene_column();
    for cx in -1..=1 {
        for cz in -1..=1 {
            world.load(
                ChunkPos::new(cx, cz),
                LoadedChunk::new(
                    column.clone(),
                    compute_column_light(&column, &DemoLightProps),
                    Heightmaps::new(),
                    Vec::new(),
                ),
            );
        }
    }
    world
}

/// An `App` with only [`TerrainPlugin`] plus the two store handles and a mesh pool —
/// the smallest configuration in which the relight system can run at all.
fn harness() -> (EcsWorld, ChunkWorldWrite) {
    let mut app = App::new();
    app.add_plugins(TerrainPlugin);
    let mut world = std::mem::take(app.world_mut());

    let write = ChunkWorldWrite::new(scene_world());
    let store: ChunkWorld = write.read_handle();
    world.insert_resource(store);
    world.insert_resource(write.clone());
    world.insert_resource(TerrainMesh::new(MeshScheduler::new(
        2,
        ShellClassifier::Demo(DemoClassifier),
    )));
    (world, write)
}

/// Stored sky light at a centre-column cell, with the `Missing` ⇒ 15 convention the
/// mesher's `SkyDefault::Full` and the relight both follow.
fn stored_sky(write: &ChunkWorldWrite, at: [i32; 3]) -> u8 {
    let world = write.read();
    let chunk = world.get(ChunkPos::new(0, 0)).expect("centre loaded");
    let ls = usize::try_from((at[1] - MIN_Y).div_euclid(16) + 1).expect("in range");
    let nibble = NibbleArray::index(
        at[0] as usize,
        (at[1] - MIN_Y).rem_euclid(16) as usize,
        at[2] as usize,
    );
    match chunk.light.sky(ls) {
        LightData::Missing => 15,
        other => other.get(nibble).unwrap_or(0),
    }
}

/// Break [`BREAK`] through the same `WorldSink` seam a `block_update` packet uses, so
/// the queue is populated by the production write path rather than by the test.
fn break_through_the_write_path(write: &ChunkWorldWrite) {
    let mut world = write.write();
    world.set_block(BREAK[0], BREAK[1], BREAK[2], id::AIR);
}

/// The premise, asserted rather than assumed: the subject sits under a roof with only
/// lateral light, so its post-relight value is *partial*. In an open-sky scene both a
/// correct relight and a naive vertical flood answer 15 and this file would prove
/// nothing.
#[test]
fn the_scene_lights_the_subject_from_the_side() {
    let (_world, write) = harness();
    let above = stored_sky(&write, [BREAK[0], BREAK[1] + 1, BREAK[2]]);
    assert!(
        (1..=14).contains(&above),
        "the cell above the subject holds {above}, not a partial value — the scene is \
         open to the sky or fully sealed and cannot separate the hypotheses"
    );
    assert_eq!(
        stored_sky(&write, BREAK),
        0,
        "an opaque cell stores sky light 0; this is the value the mesher samples into \
         the hole the instant the block becomes air"
    );
}

/// The three links, in one run: the system is registered, the drain happens, and the
/// result becomes mesh work.
#[test]
fn the_update_schedule_relights_a_broken_block_and_re_meshes_it() {
    let (mut world, write) = harness();
    // Start from a clean slate so `pending_meshes` below can only count work this
    // relight caused.
    let _ = world
        .resource_mut::<TerrainMesh>()
        .scheduler
        .drain_blocking(0);
    break_through_the_write_path(&write);

    // The write itself must not have relit anything — the queue-then-drain design is
    // the whole reason a `/fill` does not stall a frame.
    assert_eq!(
        stored_sky(&write, BREAK),
        0,
        "set_block relit on the spot; the batching design is being bypassed"
    );

    world.run_schedule(Update);

    let lit = stored_sky(&write, BREAK);
    assert!(
        (1..=14).contains(&lit),
        "after one Update the broken cell holds {lit}: 0 means the system is not \
         registered or never drained the queue, 15 means it flooded sky light \
         vertically through a solid roof"
    );
    let pending = world.resource::<TerrainMesh>().scheduler.pending();
    {
        let terrain = world.resource::<TerrainMesh>();
        eprintln!(
            "pending={pending} removals={} drops={} deferred={} still_queued={:?}",
            terrain.pending_removals.len(),
            terrain.drops,
            terrain.deferred,
            terrain.light_dirty_sections
        );
    }
    assert!(
        pending > 0,
        "the relight changed light and submitted no mesh job, so it reaches zero \
         pixels — the island defect this file exists for"
    );
    assert!(
        world
            .resource::<TerrainMesh>()
            .light_dirty_sections
            .is_empty(),
        "one break reports fewer sections than the per-frame budget, so the queue \
         must be empty after a single Update"
    );
}

/// **The control.** Without the drain the same cell stays black, which is the bug as
/// reported. Running the schedule is the only difference between this and the gate
/// above, so a green gate cannot be the fixture lighting the cell by itself.
#[test]
fn without_the_update_schedule_the_broken_cell_stays_black() {
    let (_world, write) = harness();
    break_through_the_write_path(&write);
    assert_eq!(
        stored_sky(&write, BREAK),
        0,
        "nothing has drained the relight queue, so the broken cell must still hold \
         the light of the solid block that was there — if it does not, something \
         other than the Update schedule is relighting and the gate above is vacuous"
    );
}
