//! Live 1.12.2 legacy-`id:meta` -> canonical-26.2-state parity check.
//!
//! `tests/live_chunk.rs` proves the chunk *decode* (byte layout, straddling
//! unpack) is correct against a real server, using the world-independent
//! bedrock floor. This file goes one step further and checks
//! `crate::canonical`'s **bridging** decisions — the rename/property fixups
//! documented in `docs/protocol-340-canonical-bridge.md` — against blocks a
//! real vanilla 1.12.2 server was told (via RCON, using ITS OWN legacy
//! `minecraft:<name> <meta>` command syntax — this server's `/setblock`
//! rejects a bare numeric id, confirmed directly rather than assumed) to
//! place, for a representative slice of the families that needed
//! non-trivial handling.
//!
//! # Why this is stronger evidence than the hermetic tests
//!
//! Per `CLAUDE.md`'s evidence standards, `decode(encode(x)) == x` (or, here,
//! "does the adapter agree with the table it was built from") proves
//! nothing about whether the bridging is *correct*, only that it is
//! self-consistent. This test's expected values are instead either (a) the
//! literal legacy `id`/`meta` handed to vanilla's own `/setblock`, confirmed
//! present by vanilla's own `/testforblock` before the client ever decodes
//! anything, or (b) hand-derived from documented 1.12.2/26.2 mechanics (e.g.
//! "cauldron level N splits to `water_cauldron` with `level=N`" is exactly
//! what `crate::canonical::bridge` implements, stated independently here
//! rather than by calling that function) — never by calling
//! `crate::canonical::resolve` itself and asserting agreement with itself.
//!
//! Gated behind the same `live-chunk` feature and `#[ignore]` as
//! `tests/live_chunk.rs` (it needs the same live server, plus its RCON port).
//! Run with:
//!
//! ```text
//! cargo test -p lodestone-v340 --features live-chunk --test live_canonical -- --ignored --nocapture
//! ```
#![cfg(feature = "live-chunk")]

use std::time::Duration;

use lodestone_data::block_states;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_testsupport::{AsyncRconClient, unique_username};
use lodestone_v340::V340Adapter;
use lodestone_world::{ChunkPos, World};
use tokio::net::TcpStream;
use uuid::Uuid;

/// Reads the block-state id at absolute world `(x, y, z)`, or `None` if the
/// owning column is not loaded. `World` exposes no direct random-access
/// reader (only `values()`/`iter()`, `set_block`, and the load/merge
/// write path — see `lodestone_world::World`'s docs), so this mirrors what
/// `set_block` does to find the owning column, read-only.
fn get_block(world: &World, x: i32, y: i32, z: i32) -> Option<u32> {
    let pos = ChunkPos::from_block(x, z);
    let chunk = world.iter().find(|(p, _)| **p == pos)?.1;
    let local_x = (x & 15) as usize;
    let local_z = (z & 15) as usize;
    Some(chunk.column.get_block(local_x, y, local_z))
}

fn server_port() -> u16 {
    std::env::var("LODESTONE_V340_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25568)
}

fn rcon_port() -> u16 {
    std::env::var("LODESTONE_V340_RCON_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25569)
}

fn rcon_password() -> String {
    std::env::var("LODESTONE_V340_RCON_PASSWORD").unwrap_or_else(|_| "lodestone".to_owned())
}

/// Applies one login directive against the live connection, capturing the
/// first `TeleportPlayer` position along the way (the join position, needed
/// to place test blocks somewhere the client will actually load).
async fn apply(
    conn: &mut Connection<TcpStream>,
    state: &mut ConnectionState,
    directive: Directive,
    pos: &mut Option<(i32, i32, i32)>,
) {
    match directive {
        Directive::Send { packet_id, payload } => {
            conn.write_packet(packet_id, &payload)
                .await
                .expect("write packet");
        }
        Directive::SetState(next) => *state = next,
        Directive::SetCompression(threshold) => conn.set_compression(threshold),
        Directive::Emit(ClientEvent::TeleportPlayer { pos: p, .. }) => {
            pos.get_or_insert((p.x.floor() as i32, p.y.floor() as i32, p.z.floor() as i32));
        }
        Directive::Emit(_) => {}
        Directive::Disconnect(reason) => {
            panic!("server disconnected us: {}", reason.to_plain_string());
        }
        _ => {}
    }
}

/// Joins with `username`, runs until the first position update (or a short
/// timeout with at least one chunk loaded), and returns the loaded `World`
/// plus the join position (only meaningful on the first call for a fresh
/// player file — see `main`).
async fn join_and_collect(
    port: u16,
    username: &str,
) -> (World, Option<(i32, i32, i32)>, ConnectionState) {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port,
    };
    let profile = LoginProfile {
        username: username.to_owned(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V340Adapter::new();
    let mut world = World::new();
    let mut conn = Connection::connect(("127.0.0.1", port))
        .await
        .expect("connect to live 1.12.2 server");
    let mut state = ConnectionState::Handshaking;
    let mut pos = None;

    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive, &mut pos).await;
    }

    let mut chunk_loaded = 0usize;
    let overall = Duration::from_secs(30);
    let read_timeout = Duration::from_secs(5);

    let _ = tokio::time::timeout(overall, async {
        loop {
            // Stop once we have a position and a handful of chunks — plenty
            // to have loaded the column our test blocks live in.
            if pos.is_some() && chunk_loaded >= 20 {
                break;
            }
            let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
            let (packet_id, payload) = match read {
                Err(_) => break,
                Ok(Ok(Some(packet))) => packet,
                Ok(Ok(None)) => break,
                Ok(Err(err)) => panic!("read error: {err}"),
            };
            match adapter.handle_packet(&mut world, state, packet_id, &payload) {
                Ok(directives) => {
                    for directive in directives {
                        if let Directive::Emit(ClientEvent::ChunkLoaded { .. }) = &directive {
                            chunk_loaded += 1;
                        }
                        apply(&mut conn, &mut state, directive, &mut pos).await;
                    }
                }
                Err(err) if state == ConnectionState::Play => {
                    panic!("packet {packet_id}: {err}");
                }
                Err(_) => {}
            }
        }
    })
    .await;

    (world, pos, state)
}

/// One legacy `(old_block_id, meta)` placed via RCON and the canonical
/// `(name, properties)` it is independently expected to decode to. See the
/// module docs for how `properties` was derived without calling
/// `crate::canonical` itself.
struct Expectation {
    old_block_id: u8,
    meta: u8,
    /// 1.12.2's own legacy block name, as its `/setblock`/`/testforblock`
    /// commands require it — this server's command parser rejects a bare
    /// numeric id for both (confirmed directly: `setblock x y z 1 0`
    /// silently no-ops with "There is no such block with name minecraft:1",
    /// where a weaker substring check on the response would have missed it).
    legacy_command_name: &'static str,
    name: &'static str,
    /// `None` means "check identity (name) only" — used for families whose
    /// remaining properties are a deliberate, unknowable-from-`id:meta`
    /// default (note block, trapdoor `powered`) rather than something this
    /// test can independently derive.
    properties: Option<&'static [(&'static str, &'static str)]>,
}

const EXPECTATIONS: &[Expectation] = &[
    // Plain resolved block, no bridging at all.
    Expectation {
        old_block_id: 1,
        meta: 0,
        legacy_command_name: "stone",
        name: "minecraft:stone",
        properties: Some(&[]),
    },
    // Bark -> wood rename (jar-confirmed intermediate spelling) plus the
    // axis=y property default for the all-bark variant.
    Expectation {
        old_block_id: 17,
        meta: 12,
        legacy_command_name: "log",
        name: "minecraft:oak_wood",
        properties: Some(&[("axis", "y")]),
    },
    // Leaves: decayable=false (meta 4, per the committed flattening table)
    // independently means persistent=true under the bridge's own rule
    // (persistent = !decayable) -- stated here, not computed by calling the
    // bridge.
    Expectation {
        old_block_id: 18,
        meta: 4,
        legacy_command_name: "leaves",
        name: "minecraft:oak_leaves",
        properties: Some(&[("distance", "7"), ("persistent", "true"), ("waterlogged", "false")]),
    },
    // Note block: identity only (instrument/note/powered are an
    // unknowable-from-meta default, not something a live check can confirm).
    Expectation {
        old_block_id: 25,
        meta: 0,
        legacy_command_name: "noteblock",
        name: "minecraft:note_block",
        properties: None,
    },
    // Trapdoor: identity only, for the same reason (`powered` is a fixed
    // default, not derivable from `id:meta`).
    Expectation {
        old_block_id: 96,
        meta: 0,
        legacy_command_name: "trapdoor",
        name: "minecraft:oak_trapdoor",
        properties: None,
    },
    // Cauldron level=0: stays `cauldron`, empty properties (the identity
    // *keeps* rather than splits).
    Expectation {
        old_block_id: 118,
        meta: 0,
        legacy_command_name: "cauldron",
        name: "minecraft:cauldron",
        properties: Some(&[]),
    },
    // Cauldron level=2: identity *splits* to `water_cauldron` with
    // `level=2` -- the meta value passes straight through as the property
    // value, independently obvious from what the bridge is documented to
    // do, not from calling it.
    Expectation {
        old_block_id: 118,
        meta: 2,
        legacy_command_name: "cauldron",
        name: "minecraft:water_cauldron",
        properties: Some(&[("level", "2")]),
    },
    // Cobblestone wall: name unchanged (only the connection encoding
    // changed elsewhere in the registry); identity check only here since
    // the placed block has no neighbours to connect to.
    Expectation {
        old_block_id: 139,
        meta: 0,
        legacy_command_name: "cobblestone_wall",
        name: "minecraft:cobblestone_wall",
        properties: None,
    },
    // Mossy variant of the same id (meta 1) -- confirms the meta->identity
    // split still works, not just meta 0.
    Expectation {
        old_block_id: 139,
        meta: 1,
        legacy_command_name: "cobblestone_wall",
        name: "minecraft:mossy_cobblestone_wall",
        properties: None,
    },
];

#[tokio::test]
#[ignore = "requires a live 1.12.2 server on 127.0.0.1:25568 with RCON on 25569"]
async fn legacy_families_resolve_to_expected_canonical_states_on_live_server() {
    let port = server_port();
    let username = unique_username();

    // ---- Pass 1: scout the join position, then disconnect. ----
    let (_world, pos, _state) = join_and_collect(port, &username).await;
    let (px, py, pz) = pos.expect("server never sent a position packet");
    eprintln!("scouted join position: ({px}, {py}, {pz})");

    // ---- Place every expectation's block via RCON, well above any terrain
    //      (y=200, inside the 0..256 build range but above anything a
    //      default 1.12.2 world generates), spread along x so each has its
    //      own cell with no neighbours to confuse the connection-dependent
    //      families (walls). Confirm each placement with vanilla's own
    //      `/testforblock` before trusting it. ----
    let mut rcon = AsyncRconClient::connect(("127.0.0.1", rcon_port()), &rcon_password())
        .await
        .expect("connect RCON");
    let base_x = px;
    let y = 200;
    let z = pz;
    for (i, expectation) in EXPECTATIONS.iter().enumerate() {
        let x = base_x + 2 * i as i32;
        // Vanilla's own `/setblock`/`/testforblock` command parser rejects a
        // bare numeric block id on this server ("There is no such block with
        // name minecraft:1") -- confirmed directly, not assumed; see
        // `Expectation::legacy_command_name`'s doc. The full
        // `minecraft:<legacy name>` form is what actually works.
        let block = format!("minecraft:{}", expectation.legacy_command_name);
        let set = rcon
            .command(&format!(
                "setblock {x} {y} {z} {block} {} replace",
                expectation.meta
            ))
            .await
            .expect("RCON setblock");
        assert!(
            set.to_lowercase().contains("block placed"),
            "setblock for legacy ({}, {}) [{block}] at ({x},{y},{z}) did not report success: {set}",
            expectation.old_block_id,
            expectation.meta,
        );
        let confirm = rcon
            .command(&format!("testforblock {x} {y} {z} {block} {}", expectation.meta))
            .await
            .expect("RCON testforblock");
        assert!(
            confirm.to_lowercase().contains("successfully"),
            "server does not agree legacy ({}, {}) [{block}] is at ({x},{y},{z}) after placement: {confirm}",
            expectation.old_block_id,
            expectation.meta,
        );
    }

    // ---- Pass 2: rejoin (same username -> same player file -> same
    //      position) and decode the column that now contains every placed
    //      block through the real adapter. ----
    let (world, _pos2, state) = join_and_collect(port, &username).await;
    assert_eq!(state, ConnectionState::Play, "never reached Play on rejoin");

    let mut failures = Vec::new();
    for (i, expectation) in EXPECTATIONS.iter().enumerate() {
        let x = base_x + 2 * i as i32;
        let Some(state_id) = get_block(&world, x, y, z) else {
            failures.push(format!(
                "({x},{y},{z}) legacy ({}, {}): not in a loaded chunk",
                expectation.old_block_id, expectation.meta
            ));
            continue;
        };
        let decoded_name = block_states::block_name(state_id);
        let decoded_properties = block_states::properties(state_id);
        if decoded_name != Some(expectation.name) {
            failures.push(format!(
                "({x},{y},{z}) legacy ({}, {}): expected name {}, decoded {:?} (state {state_id})",
                expectation.old_block_id, expectation.meta, expectation.name, decoded_name,
            ));
            continue;
        }
        if let Some(expected_properties) = expectation.properties
            && decoded_properties != Some(expected_properties)
        {
            failures.push(format!(
                "({x},{y},{z}) legacy ({}, {}) resolved to the right name but wrong properties: \
                 expected {expected_properties:?}, decoded {decoded_properties:?} (state {state_id})",
                expectation.old_block_id, expectation.meta,
            ));
        }
    }

    eprintln!("\n=== LIVE 1.12.2 CANONICAL BRIDGE PARITY REPORT ===");
    eprintln!("expectations checked: {}", EXPECTATIONS.len());
    eprintln!("failures            : {}", failures.len());
    for failure in &failures {
        eprintln!("  FAIL: {failure}");
    }
    eprintln!("===================================================\n");

    assert!(
        failures.is_empty(),
        "{} of {} legacy block(s) placed by a real 1.12.2 server did not decode to their \
         expected canonical state: {failures:#?}",
        failures.len(),
        EXPECTATIONS.len(),
    );
}
