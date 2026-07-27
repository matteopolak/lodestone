//! Hermetic proof that the integrated server serves worldgen chunks to a client
//! over the **same** in-memory [`Connection`]/[`Transport`] path used for TCP.
//!
//! A `memory_pair()` duplex connects a client end to a server end. The server
//! runs [`serve_connection`] with a [`WorldgenChunkSource`] (backed by the
//! density-function router) and a small stand-in [`ServerProtocol`]. The client
//! drives handshake + login start through [`Connection::write_packet`], then
//! reads packets back through [`Connection::read_packet`] — the real framing and
//! codec — and verifies every received chunk's solidity matches the terrain the
//! source generates.
//!
//! The stand-in protocol here uses a trivial wire format, not vanilla 26.2's:
//! the version-correct client-bound encoders (`join_game`, registry data,
//! `level_chunk_with_light`) live in the version crate and are a reported seam
//! (the client stack is decode-only today). What this test *does* prove is the
//! full server plumbing: transport → codec → login state machine → worldgen →
//! client decode, with zero framing errors, over the shared seam.

use lodestone_core::{Reader, State, Writer};
use lodestone_net::{Connection, memory_pair};
use lodestone_server::{
    ChunkColumn, ChunkSource, ServerBound, ServerDirective, ServerProtocol, WorldgenChunkSource,
    serve_connection,
};
use lodestone_worldgen::density::{Builder, Density, NoiseParams, Resolver};
use serde_json::Value;
use std::path::{Path, PathBuf};

const LOGIN_START: i32 = 0;
const HANDSHAKE: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const CHUNK: i32 = 0x27;

/// A stand-in protocol with a trivial, self-describing wire format.
struct FakeProtocol;

impl FakeProtocol {
    fn encode_column(cx: i32, cz: i32, col: &ChunkColumn) -> Vec<u8> {
        let mut w = Writer::default();
        w.var_i32(cx);
        w.var_i32(cz);
        w.var_i32(col.min_y);
        w.var_i32(col.height);
        for y in col.min_y..col.min_y + col.height {
            for z in 0..16 {
                for x in 0..16 {
                    w.u8(u8::from(col.is_solid(x, y, z)));
                }
            }
        }
        w.as_slice().to_vec()
    }
}

impl ServerProtocol for FakeProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == LOGIN_START => {
                let mut r = Reader::new(payload);
                let username = r.string(16).expect("username");
                ServerBound::LoginStart { username }
            }
            _ => ServerBound::Ignored,
        }
    }

    fn login_sequence(&self, username: &str) -> Vec<ServerDirective> {
        let mut w = Writer::default();
        w.string(username);
        vec![
            ServerDirective::Send {
                packet_id: LOGIN_SUCCESS,
                payload: w.as_slice().to_vec(),
            },
            ServerDirective::SetState(State::Play),
        ]
    }

    fn encode_chunk(&self, cx: i32, cz: i32, column: &ChunkColumn) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CHUNK,
            payload: Self::encode_column(cx, cz, column),
        }
    }
}

/// Filesystem resolver over the worldgen crate's checked-in vanilla JSON.
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
    // Build first, then drop the borrow before returning the owned Density.
    let builder = Builder::new(seed, &resolver);
    builder.build(&settings["noise_router"]["final_density"])
}

#[tokio::test]
async fn integrated_server_streams_worldgen_chunks_over_memory_transport() {
    // The worldgen crate owns the staged vanilla data (plan §3 puts it in the
    // version crate; it is staged as fixtures this session).
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../lodestone-worldgen/tests/support/worldgen_data");
    let seed = 42_i64;
    let view_radius = 0; // single chunk keeps this hermetic test fast

    let final_density = overworld_final_density(seed, &root);
    // The router math is per-point, so a shorter column still exercises the full
    // density tree; we cap the height to keep debug-mode sampling quick.
    let sample_min_y = -64;
    let sample_height = 96;
    let source = WorldgenChunkSource::new(final_density.clone(), sample_min_y, sample_height);

    // An independent reference source the client checks received chunks against.
    let reference = WorldgenChunkSource::new(final_density, sample_min_y, sample_height);

    let (client_end, server_end) = memory_pair();

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, view_radius)
            .await
            .expect("serve")
    });

    let mut client = Connection::new(client_end);

    // Handshake: one byte selecting the Login next-state.
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    // Login start: the username.
    let mut w = Writer::default();
    w.string("SinglePlayer");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");

    // First play packet is login success carrying the echoed username.
    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, LOGIN_SUCCESS);
    let mut r = Reader::new(&payload);
    assert_eq!(r.string(16).unwrap(), "SinglePlayer");

    // Then 9 chunk packets, each verified block-for-block against the reference.
    let expected_chunks = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    let mut received = 0;
    let mut total_solid = 0usize;
    for _ in 0..expected_chunks {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        assert_eq!(id, CHUNK);
        let mut r = Reader::new(&payload);
        let cx = r.var_i32().unwrap();
        let cz = r.var_i32().unwrap();
        let min_y = r.var_i32().unwrap();
        let height = r.var_i32().unwrap();
        assert_eq!((min_y, height), (sample_min_y, sample_height));

        let expected = reference.column(cx, cz);
        for y in min_y..min_y + height {
            for z in 0..16 {
                for x in 0..16 {
                    let solid = r.u8().unwrap() == 1;
                    assert_eq!(
                        solid,
                        expected.is_solid(x, y, z),
                        "mismatch at chunk ({cx},{cz}) block ({x},{y},{z})"
                    );
                    if solid {
                        total_solid += 1;
                    }
                }
            }
        }
        assert_eq!(r.remaining(), 0, "trailing bytes in chunk ({cx},{cz})");
        received += 1;
    }

    let summary = server.await.expect("join");
    assert_eq!(summary.chunks_sent, expected_chunks);
    assert_eq!(received, expected_chunks);
    assert_eq!(summary.username, "SinglePlayer");
    // The seeded overworld router must actually produce terrain, not empty air.
    assert!(total_solid > 0, "worldgen produced no solid blocks");
    println!("served {received} chunks over in-memory transport; {total_solid} solid blocks total");
}
