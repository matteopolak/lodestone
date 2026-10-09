//! End-to-end: the **real** `lodestone-client`, running the real
//! [`V770Adapter`], connects to `lodestone-server` in-process through the real
//! [`V770ServerProtocol`] and receives worldgen chunks over the real vanilla
//! 26.2 wire format — asserted **block-for-block**.
//!
//! This is the reported gate for the `ServerProtocol` seam: unlike
//! `lodestone-server`'s own `tests/client_integration.rs` (which pairs a real
//! client with a `StandInProtocol`/`StandInAdapter` speaking a trivial fake
//! wire format), every packet exchanged here is the actual protocol-776
//! encoding — paletted `level_chunk_with_light` sections, the real
//! login/configuration/play state machine, the real join-game packet. The
//! only thing not real is *terrain content*: [`RidgeSource`] is stone below a
//! surface whose height varies with both `x` and `z`, so a transposed, mirrored
//! or vertically shifted section fails the block-for-block assertion, which is
//! made against an independent instance of the same source rather than
//! against vanilla terrain.
//!
//! What would have to break for this to fail: any wire-layout mismatch between
//! [`V770ServerProtocol`]'s encoders and [`V770Adapter`]'s decoders — a
//! misplaced field, a wrong palette threshold, a missing shortcount — surfaces
//! as either a decode error (the adapter's `ensure_empty`/`decode_full`
//! discipline) or a lost/incorrect chunk. The non-vacuity guard additionally
//! fails if the terrain is empty air, so "joined but the world is blank"
//! cannot pass.

use std::time::Duration;

use lodestone_client::{
    BlockPos, ClientBuilder, ClientEvent, LoginProfile, ServerAddress,
};
use lodestone_data::block_states::StateId;
use lodestone_server::{ChunkColumn, ChunkSource, IntegratedServer};
use lodestone_v26_2::{V770ServerProtocol, adapter};
use uuid::Uuid;

// Block-state ids this test checks against, resolved the same way
// `server_protocol.rs` resolves them at runtime (by name, not a bare literal)
// so a regenerated table cannot silently desync the expectation from the
// implementation.
fn stone_id() -> u32 {
    (0..)
        .find(|&id| lodestone_data::block_states::block_name(id) == Some("minecraft:stone"))
        .expect("generated block-state table has no `minecraft:stone` entry")
}
const AIR_ID: u32 = 0;

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

/// Stone below a surface at `y = (7x + 13z) mod 23 - 11`, air above: terrain
/// with no symmetry in `x`, `z` or `y`, so a block landing in the wrong cell
/// changes the comparison. It retains no edits.
struct RidgeSource {
    min_y: i32,
    height: i32,
}

impl RidgeSource {
    fn surface(x: i32, z: i32) -> i32 {
        (7 * x + 13 * z).rem_euclid(23) - 11
    }

    fn solid(&self, x: i32, y: i32, z: i32) -> bool {
        y >= self.min_y && y < self.min_y + self.height && y < Self::surface(x, z)
    }
}

impl ChunkSource for RidgeSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(self.min_y, self.height);
        for lx in 0..16 {
            for lz in 0..16 {
                for y in self.min_y..Self::surface(cx * 16 + lx, cz * 16 + lz) {
                    col.set_solid(lx, y, lz, true);
                }
            }
        }
        col
    }

    fn block_state_id(&self, x: i32, y: i32, z: i32) -> StateId {
        let block = if self.solid(x, y, z) {
            lodestone_data::block::Block::Stone
        } else {
            lodestone_data::block::Block::Air
        };
        block.default_state()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        self.column(x.div_euclid(16), z.div_euclid(16))
            .biome_state_at(x.rem_euclid(16), y, z.rem_euclid(16))
            .to_string()
    }

    fn set_block(&self, x: i32, y: i32, z: i32, _state: StateId) {
        panic!("RidgeSource retains no edits; cannot set ({x}, {y}, {z})");
    }
}

#[tokio::test]
async fn real_client_and_real_v770_protocol_reach_play_with_worldgen_chunks() {
    // Must match `ChunkShape::overworld_1_21()` exactly: the client hardcodes
    // this shape by dimension name (`ChunkShape::for_dimension`), so a column
    // built to any other vertical extent would misalign the client's decode.
    let min_y = -64;
    let height = 384; // 24 sections
    let view_radius = 0; // single chunk (0,0)

    let source = RidgeSource { min_y, height };
    let reference = RidgeSource { min_y, height };

    // Start the integrated server in-process with the *real* v26-2 protocol;
    // get the client's transport end.
    let (server, client_io) =
        IntegratedServer::open_in_memory(V770ServerProtocol, source, view_radius);

    // The *real* client, running the *real* v26-2 adapter, drives the other end.
    let (handle, mut events) =
        ClientBuilder::new(address(), profile(), Box::new(adapter())).connect_with(client_io);

    // Wait for the chunk to arrive (poll; never assert immediately), with a
    // deadline generous enough for a loaded debug build.
    let start = std::time::Instant::now();
    let deadline = start + Duration::from_secs(180);
    while handle.loaded_chunk_count() == 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "client never received a chunk within 180s"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(handle.loaded_chunk_count(), 1, "exactly one chunk expected");
    // `SetHealth`, sent as part of the join sequence, is already fully
    // decoded and folded by the client (`ClientEvent::HealthChanged` ->
    // `PlayerSnapshot::health`) — connecting it costs one derived-struct send.
    assert_eq!(
        handle.health(),
        Some(20.0),
        "join sequence should report full health"
    );

    // Block-for-block: every block the client decoded from the real
    // `level_chunk_with_light` wire format must equal what worldgen generated
    // on the server, mapped through the same stone/air coding
    // `server_protocol.rs` uses.
    let stone = stone_id();
    let expected = reference.column(0, 0);
    let mut checked = 0usize;
    let mut solid = 0usize;
    for y in min_y..min_y + height {
        for z in 0..16 {
            for x in 0..16 {
                let want = if expected.is_solid(x, y, z) {
                    stone
                } else {
                    AIR_ID
                };
                let got = handle.block_at(BlockPos::new(x, y, z));
                assert_eq!(
                    got,
                    Some(want),
                    "block mismatch at ({x},{y},{z}): client={got:?} worldgen={want}"
                );
                checked += 1;
                if want == stone {
                    solid += 1;
                }
            }
        }
    }

    assert_eq!(checked, 16 * 16 * height as usize);
    // Non-vacuity, predicted from the surface formula rather than read back
    // from the source: each of chunk (0,0)'s 256 columns is solid from -64 up
    // to its surface, so the count is the sum of `surface + 64`.
    let predicted: usize = (0..16)
        .flat_map(|x| (0..16).map(move |z| ((7 * x + 13 * z) % 23 - 11 + 64) as usize))
        .sum();
    assert_eq!(solid, predicted, "solid block count");

    println!(
        "real client + real V770ServerProtocol reached Play; chunks={}, blocks_checked={checked}, solid={solid}",
        handle.loaded_chunk_count()
    );

    // A join must not synthesize chat. Keep reading through the other initial
    // events for a bounded quiet window so this checks the production client /
    // server path rather than merely inspecting the first event after spawn.
    let chat_deadline = std::time::Instant::now() + Duration::from_secs(2);
    let mut join_chats = Vec::new();
    while std::time::Instant::now() < chat_deadline {
        let remaining = chat_deadline.saturating_duration_since(std::time::Instant::now());
        let Some(event) = tokio::time::timeout(remaining, events.recv())
            .await
            .ok()
            .flatten()
        else {
            break;
        };
        if let ClientEvent::Chat { text, .. } = event {
            join_chats.push(text.to_plain_string());
        }
    }
    assert!(
        join_chats.is_empty(),
        "joining must not emit an automatic system-chat line; saw {join_chats:?}"
    );

    drop(handle);
    drop(events);
    server.shutdown().await;
}
