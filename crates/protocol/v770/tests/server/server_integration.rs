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
//! only thing not yet real is *terrain content*: [`WorldgenChunkSource`]
//! point-samples the density-router `final_density` only (no surface rules,
//! caves, or ores — see that type's doc comment), which is why the assertion
//! below is block-for-block against an independent instance of the same
//! source rather than against known vanilla terrain.
//!
//! What would have to break for this to fail: any wire-layout mismatch between
//! [`V770ServerProtocol`]'s encoders and [`V770Adapter`]'s decoders — a
//! misplaced field, a wrong palette threshold, a missing shortcount — surfaces
//! as either a decode error (the adapter's `ensure_empty`/`decode_full`
//! discipline) or a lost/incorrect chunk. The non-vacuity guard additionally
//! fails if the terrain is empty air, so "joined but the world is blank"
//! cannot pass.

use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestone_client::{
    BlockPos, ChatKind, ClientBuilder, ClientEvent, LoginProfile, ServerAddress,
};
use lodestone_server::{ChunkSource, IntegratedServer, WorldgenChunkSource};
use lodestone_v770::{V770ServerProtocol, adapter};
use lodestone_worldgen::density::{Builder, Density, NoiseParams, Resolver};
use serde_json::Value;
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

// ---------------------------------------------------------------------------
// Worldgen wiring (mirrors `lodestone-server`'s own
// `tests/client_integration.rs`, which this test is the real-protocol
// counterpart of).
// ---------------------------------------------------------------------------

struct FsResolver {
    root: PathBuf,
}

impl FsResolver {
    fn read(&self, kind: &str, id: &str) -> Value {
        let name = id.strip_prefix("minecraft:").unwrap_or(id);
        let path = self.root.join(kind).join(format!("{name}.json"));
        let text = std::fs::read_to_string(&path).expect("read worldgen json");
        serde_json::from_str(&text).expect("parse worldgen json")
    }
}

impl Resolver for FsResolver {
    fn density_function(&self, id: &str) -> Value {
        self.read("density_function", id)
    }
    fn noise(&self, id: &str) -> NoiseParams {
        let v = self.read("noise", id);
        NoiseParams {
            first_octave: v["firstOctave"].as_i64().unwrap() as i32,
            amplitudes: v["amplitudes"]
                .as_array()
                .unwrap()
                .iter()
                .map(|a| a.as_f64().unwrap())
                .collect(),
        }
    }
}

fn overworld_final_density(seed: i64, root: &Path) -> Density {
    let resolver = FsResolver {
        root: root.to_path_buf(),
    };
    let settings: Value = serde_json::from_str(
        &std::fs::read_to_string(root.join("noise_settings/overworld.json")).unwrap(),
    )
    .unwrap();
    let builder = Builder::new(seed, &resolver);
    builder.build(&settings["noise_router"]["final_density"])
}

#[tokio::test]
async fn real_client_and_real_v770_protocol_reach_play_with_worldgen_chunks() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../lodestone-worldgen/tests/support/worldgen_data");
    let seed = 42_i64;
    // Must match `ChunkShape::overworld_1_21()` exactly: the client hardcodes
    // this shape by dimension name (`ChunkShape::for_dimension`), so a column
    // built to any other vertical extent would misalign the client's decode.
    let min_y = -64;
    let height = 384; // 24 sections
    let view_radius = 0; // single chunk (0,0) — keep the point-sampled cost small

    let final_density = overworld_final_density(seed, &root);
    let source = WorldgenChunkSource::new(final_density.clone(), min_y, height);
    let reference = WorldgenChunkSource::new(final_density, min_y, height);

    // Start the integrated server in-process with the *real* v770 protocol;
    // get the client's transport end.
    let (server, client_io) =
        IntegratedServer::open_in_memory(V770ServerProtocol, source, view_radius);

    // The *real* client, running the *real* v770 adapter, drives the other end.
    let (handle, mut events) =
        ClientBuilder::new(address(), profile(), Box::new(adapter())).connect_with(client_io);

    // Wait for the chunk to arrive (poll; never assert immediately). A full
    // 384-tall column point-samples the overworld density router per block —
    // several times more expensive than the 96-tall stand-in fixture — hence
    // the generous deadline.
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
    // Non-vacuity: the seeded router must have produced terrain, so this is a
    // real block-content comparison and not "correctly delivered nothing".
    assert!(
        solid > 0,
        "worldgen produced no solid blocks — vacuous check"
    );

    println!(
        "real client + real V770ServerProtocol reached Play; chunks={}, blocks_checked={checked}, solid={solid}",
        handle.loaded_chunk_count()
    );

    // `welcome_message` exercises the newly landed `lodestone_core` NBT writer
    // for real: the server encodes a `system_chat` packet with a network-form
    // NBT text component, and the real client's `V770Adapter` decodes it back
    // via `read_network_nbt`/`plain_text_from_nbt_component` — proving the
    // writer/reader round-trip through the actual wire, not a unit test of
    // either side alone.
    let chat_deadline = std::time::Instant::now() + Duration::from_secs(30);
    let mut seen_welcome = false;
    while std::time::Instant::now() < chat_deadline {
        let Some(event) = tokio::time::timeout(Duration::from_millis(500), events.recv())
            .await
            .ok()
            .flatten()
        else {
            continue;
        };
        if let ClientEvent::Chat { text, kind, .. } = event {
            assert_eq!(
                kind,
                ChatKind::System,
                "welcome message should be System chat, not overlay"
            );
            assert_eq!(
                text.to_plain_string(),
                "Welcome to Lodestone",
                "welcome message text mismatch"
            );
            seen_welcome = true;
            break;
        }
    }
    assert!(
        seen_welcome,
        "never received the post-join system_chat welcome message within 30s"
    );

    drop(handle);
    drop(events);
    server.shutdown().await;
}
