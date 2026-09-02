//! The "what consumes this" gate for issues #132/#134/#136: a real
//! `IntegratedServer`, serving a `PluginChunkSource` obtained purely through
//! `DimensionRegistry::chunk_source` (never by constructing the generator's
//! own type directly), joined by a real, wire-decoding `lodestone-client`
//! running the real [`V770Adapter`](lodestone_v26_2::adapter::V770Adapter).
//!
//! This is deliberately not a test that calls
//! [`lodestone_void_world::CheckerboardVoidGenerator::generate`] and asserts
//! on the returned grid — `src/lib.rs`'s own unit tests already do that, and
//! CLAUDE.md's "closed loop" trap is exactly a test whose assertion and
//! whose subject are both authored by the same code path. Here the
//! assertion is `ClientHandle::block_at`, decoded off packets a real
//! `V770ServerProtocol` encoded from real generated/edited chunk data — the
//! same route a player's own screen would receive it through.
//!
//! What would have to break for this to fail: the registry not handing back
//! a working `ChunkSource` (issue #134), the generator's checkerboard or its
//! generation-time structure placement not reaching the served chunk (issue
//! #132/#136), or `place_structure_live` not persisting a live edit the
//! server actually serves (issue #136's other half).

use std::sync::Arc;
use std::time::Duration;

use lodestone_client::{ClientBuilder, LoginProfile, ServerAddress};
use lodestone_model::BlockPos;
use lodestone_server::plugin_dimension::DimensionRegistry;
use lodestone_server::{IntegratedServer, NoEntities};
use lodestone_v26_2::V770ServerProtocol;
use lodestone_v26_2::adapter;
use lodestone_void_world::{DIMENSION_KEY, FLOOR_Y};
use uuid::Uuid;

fn profile() -> LoginProfile {
    LoginProfile {
        username: "SinglePlayer".into(),
        uuid: Uuid::new_v4(),
    }
}

fn address() -> ServerAddress {
    ServerAddress {
        host: "memory".into(),
        port: 0,
    }
}

/// Resolves a canonical block-state string to the numeric id
/// `ClientHandle::block_at` reports — the same resolver
/// `lodestone-server`'s own chunk encoding uses, per `chunk.rs`'s
/// `resolve_palette_state_id` doc comment ("do not copy this logic — call
/// it").
fn state_id(name: &str) -> u32 {
    lodestone_data::block_states::state_id(name)
        .unwrap_or_else(|| panic!("'{name}' has no known block-state id"))
}

#[tokio::test]
async fn a_real_client_observes_the_registered_dimensions_terrain_and_both_structure_placements() {
    let registry = DimensionRegistry::new();
    lodestone_void_world::register(&registry);
    let source = registry
        .chunk_source(DIMENSION_KEY)
        .expect("just registered under DIMENSION_KEY");

    // Issue #136's live-placement half, exercised *before* anyone joins:
    // paste a marker into the world through the real `ChunkSource` the
    // registry handed back, not by reaching into the generator.
    let marker_at = [5, FLOOR_Y + 1, 5];
    let written = lodestone_void_world::place_marker_live(&*source, marker_at);
    assert_eq!(written, 1, "control: the live marker template must write exactly one block");
    assert_eq!(
        source.block_state(marker_at[0], marker_at[1], marker_at[2]),
        "minecraft:emerald_block",
        "control: the live-placed marker must read back through the source before anyone joins"
    );

    let view_radius = 2;
    let (server, client_io) = IntegratedServer::open_in_memory_with_entities(
        V770ServerProtocol,
        Arc::clone(&source),
        NoEntities,
        view_radius,
    );

    let (handle, _events) =
        ClientBuilder::new(address(), profile(), Box::new(adapter())).connect_with(client_io);

    let deadline = std::time::Instant::now() + Duration::from_secs(60);
    while handle.loaded_chunk_count() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "client never received a chunk within 60s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The checkerboard floor, decoded off the real wire (issue #132).
    assert_eq!(
        handle.block_at(BlockPos::new(0, FLOOR_Y, 0)),
        Some(state_id("minecraft:stone")),
        "checkerboard: (0,0) sums even, expected stone"
    );
    assert_eq!(
        handle.block_at(BlockPos::new(1, FLOOR_Y, 0)),
        Some(state_id("minecraft:glass")),
        "checkerboard: (1,0) sums odd, expected glass"
    );
    assert_eq!(
        handle.block_at(BlockPos::new(0, FLOOR_Y, 1)),
        Some(state_id("minecraft:glass")),
        "checkerboard: (0,1) sums odd, expected glass"
    );

    // The generation-time landmark structure (issue #136, first half).
    assert_eq!(
        handle.block_at(BlockPos::new(0, FLOOR_Y + 1, 0)),
        Some(state_id("minecraft:gold_block")),
        "landmark platform corner"
    );
    assert_eq!(
        handle.block_at(BlockPos::new(1, FLOOR_Y + 2, 1)),
        Some(state_id("minecraft:beacon")),
        "landmark centre beacon, one row above the gold platform"
    );
    // Outside the landmark's footprint, at generation time, is untouched —
    // the checkerboard floor's own glass/stone alternation, not gold.
    assert_eq!(
        handle.block_at(BlockPos::new(5, FLOOR_Y + 1, 5)),
        Some(state_id("minecraft:emerald_block")),
        "the live-placed marker (issue #136, second half), reached through the SAME served \
         chunk as the generation-time landmark above"
    );
    assert_eq!(
        handle.block_at(BlockPos::new(6, FLOOR_Y + 1, 5)),
        Some(state_id("minecraft:air")),
        "just outside the marker's footprint must not have been touched by the live paste"
    );

    drop(server);
}
