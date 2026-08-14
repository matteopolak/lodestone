//! The "what consumes this" gate: [`EditSession`] has its own hermetic unit
//! tests in `src/session.rs`, but none of them go through a real `GameTick`
//! schedule or a real [`ChunkWorldWrite`]/[`ChunkWorld`] resource pair the
//! way a shipped plugin actually would. This test builds a real `bevy_ecs`
//! `App` with `lodestone_ecs::CorePlugin` + [`WorldEditPlugin`], queues a
//! [`FillRequest`] the way a chat-command handler would, ticks the schedule,
//! and asserts the change landed in the store through the *read* handle
//! (`ChunkWorld`) — the same handle a real mesher would observe — not just
//! through the write handle the plugin itself holds.

use lodestone_ecs::app::App;
use lodestone_ecs::{ChunkWorld, ChunkWorldWrite, GameTick};
use lodestone_worldedit::{FillRequest, FillRequests, Selection, WorldEditPlugin};
use lodestone_world::{ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World};

fn flat_world(radius: i32) -> World {
    let mut world = World::new();
    for cx in -radius..=radius {
        for cz in -radius..=radius {
            let column = ChunkColumn::new(
                -64,
                24,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                0,
                0,
            );
            let light = ColumnLight::new(24);
            world.load(
                ChunkPos::new(cx, cz),
                LoadedChunk::new(column, light, Heightmaps::new(), Vec::new()),
            );
        }
    }
    world
}

fn app_with_a_flat_world(radius: i32) -> (App, ChunkWorld) {
    let mut app = App::new();
    app.add_plugins((lodestone_ecs::CorePlugin, WorldEditPlugin));

    let write = ChunkWorldWrite::new(flat_world(radius));
    let read = write.read_handle();
    app.insert_resource(write);
    app.insert_resource(read.clone());
    (app, read)
}

#[test]
fn a_queued_fill_request_reaches_the_store_through_a_real_tick() {
    let (mut app, chunk_world) = app_with_a_flat_world(1);

    assert_eq!(
        chunk_world.read().block_state_at(2, 60, 2),
        Some(0),
        "control: air before the request is processed"
    );

    app.world_mut().resource_mut::<FillRequests>().0.push(FillRequest {
        session_key: 1,
        selection: Selection::new([0, 60, 0], [3, 60, 3]),
        state: 7,
        physics: false,
    });

    // Nothing applies until the schedule actually runs — proves the request
    // is genuinely drained by a system, not applied eagerly by pushing it.
    assert_eq!(chunk_world.read().block_state_at(2, 60, 2), Some(0));

    app.world_mut().run_schedule(GameTick);

    assert_eq!(
        chunk_world.read().block_state_at(2, 60, 2),
        Some(7),
        "the fill must be visible through the READ handle — the same one a \
         real mesher/read-only system would hold — not only through the \
         write handle the plugin's own EditSession keeps"
    );
    assert_eq!(chunk_world.read().block_state_at(0, 60, 0), Some(7));
    assert_eq!(
        chunk_world.read().block_state_at(4, 60, 0),
        Some(0),
        "just outside the requested selection must be untouched"
    );

    // The request queue itself must have been drained, not merely read.
    assert!(app.world().resource::<FillRequests>().0.is_empty());
}

#[test]
fn two_different_session_keys_get_independent_undo_histories() {
    let (mut app, chunk_world) = app_with_a_flat_world(1);

    for (key, state) in [(1, 5u32), (2, 6u32)] {
        app.world_mut().resource_mut::<FillRequests>().0.push(FillRequest {
            session_key: key,
            selection: Selection::new([0, 60, 0], [0, 60, 0]),
            state,
            physics: false,
        });
        app.world_mut().run_schedule(GameTick);
    }
    // Player 2's fill happened after player 1's, on the same cell, so the
    // real state is player 2's — but each has their own undo stack, unlike
    // a design with one shared EditSession.
    assert_eq!(chunk_world.read().block_state_at(0, 60, 0), Some(6));

    let sessions = &app
        .world()
        .resource::<lodestone_worldedit::EditSessions>()
        .0;
    assert_eq!(sessions.get(&1).unwrap().undo_depth(), 1);
    assert_eq!(sessions.get(&2).unwrap().undo_depth(), 1);
}
