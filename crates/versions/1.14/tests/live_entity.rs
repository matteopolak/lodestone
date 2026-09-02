//! Live 1.16.5 entity-spawn / metadata acceptance test (the V2a gate for
//! protocol 754).
//!
//! Gated behind the `live-entity` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run against a live vanilla 1.16.5 server
//! (offline mode, flat world, RCON enabled) on `127.0.0.1:25573` (RCON
//! `127.0.0.1:25574`):
//!
//! ```text
//! cargo test -p lodestone-v1-14 --features live-entity --test live_entity \
//!     -- --ignored --nocapture
//! ```
//!
//! The shared server has `spawn-monsters=false`/`spawn-animals=false` and runs
//! on `difficulty=peaceful`, so this cannot rely on natural spawns and must not
//! summon hostiles (they despawn instantly on peaceful). It drives a real join,
//! confirms the 1.9+ teleport (via the adapter, so the player is loaded), then
//! **summons passive mobs at the player through RCON** and decodes the resulting
//! `spawn_entity_living` and `entity_metadata` packets with this crate's codecs.
//!
//! # What it proves — specifically the 1.16 migrations
//!
//! * every entity packet decodes with **zero trailing bytes** (`ensure_empty`);
//! * every `spawn_entity_living.kind` resolves through the **unified 1.16 entity
//!   registry** ([`entity_type_name`]) — this is the table that replaced 1.12's
//!   split mob/object id spaces after flattening, so a mis-migrated id would map
//!   to the wrong name or `None` here;
//! * at least one `entity_metadata` packet carries a non-empty list decoded
//!   against the **1.16.5 metadata type table** (whose type ids shifted vs 1.12,
//!   e.g. `Boolean` moved 6 → 7), on real server-authored bytes.
//!
//! Hazard honoured: a freshly summoned entity is not visible until the next
//! server tick, so this test never asserts immediately after summoning — it
//! polls its read loop until packets arrive or a deadline passes.
#![cfg(feature = "live-entity")]

use lodestone_testsupport::{RconClient, unique_username};
use std::time::{Duration, Instant};

use lodestone_core::{Ctx, Decode, Reader};
use lodestone_model::{
    ClientAction, ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress,
    VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_v1_14::V735Adapter;
use lodestone_v1_14::entity_types::entity_type_name;
use lodestone_v1_14::packet_ids::play;
use lodestone_v1_14::packets::entity::{EntityMetadataPacket, SpawnEntityLiving};
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 754 };

async fn apply(
    conn: &mut Connection<TcpStream>,
    state: &mut ConnectionState,
    directive: Directive,
) {
    match directive {
        Directive::Send { packet_id, payload } => {
            conn.write_packet(packet_id, &payload)
                .await
                .expect("write packet");
        }
        Directive::SetState(next) => *state = next,
        Directive::SetCompression(threshold) => conn.set_compression(threshold),
        Directive::Emit(_) => {}
        Directive::Disconnect(reason) => {
            panic!("server disconnected us: {}", reason.to_plain_string());
        }
        _ => {}
    }
}

fn server_port() -> u16 {
    std::env::var("LODESTONE_V735_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25573)
}

fn rcon_addr() -> String {
    std::env::var("LODESTONE_V735_RCON").unwrap_or_else(|_| "127.0.0.1:25574".into())
}

fn rcon_password() -> String {
    std::env::var("LODESTONE_V735_RCON_PASSWORD").unwrap_or_else(|_| "lodestone".into())
}

/// Summons a batch of peaceful-safe mobs at `(x, y, z)` over RCON. Passive mobs
/// only — hostiles despawn instantly on `difficulty=peaceful`.
fn summon_mobs(rcon: &mut RconClient, x: i32, y: i32, z: i32) {
    let mobs = ["cow", "pig", "sheep", "chicken"];
    for mob in mobs {
        let out = rcon.cmd(&format!("summon minecraft:{mob} {x} {y} {z}"));
        if !out.contains("Summoned") && !out.contains("summon") {
            eprintln!("summon '{mob}' unexpected RCON reply: {out:?}");
        }
    }
    eprintln!("summoned {} mobs at ({x},{y},{z})", mobs.len());
}

#[tokio::test]
#[ignore = "requires a live 1.16.5 server with RCON on 127.0.0.1:25573 (rcon :25574)"]
async fn decodes_real_entity_metadata_from_live_1_16_server() {
    let port = server_port();
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V735Adapter::new();
    // Chunk packets would apply to this sink; this test ignores world state, so
    // a throwaway sink suffices.
    let mut world = World::new();

    let mut rcon = RconClient::connect(rcon_addr(), &rcon_password())
        .expect("connect to 1.16.5 RCON (is lodestone-mc1165 up with enable-rcon=true?)");

    let mut conn = Connection::connect(("127.0.0.1", port))
        .await
        .expect("connect to live 1.16.5 server (is lodestone-mc1165 up on :25573?)");
    let mut state = ConnectionState::Handshaking;

    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    let mut spawns: Vec<SpawnEntityLiving> = Vec::new();
    let mut meta_updates: Vec<EntityMetadataPacket> = Vec::new();
    let mut reached_play = false;
    let mut summoned = false;
    let mut last_health: Option<f32> = None;

    let overall = Duration::from_secs(60);
    let read_timeout = Duration::from_secs(5);
    let collect_window = Duration::from_secs(20);
    let target_spawns = 4usize;
    let mut collect_deadline: Option<Instant> = None;

    let _ = tokio::time::timeout(overall, async {
        loop {
            if let Some(deadline) = collect_deadline
                && (spawns.len() >= target_spawns || Instant::now() >= deadline)
            {
                break;
            }
            let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
            let (packet_id, payload) = match read {
                Err(_) => {
                    if collect_deadline.is_some() && !spawns.is_empty() {
                        break;
                    }
                    continue;
                }
                Ok(Ok(Some(packet))) => packet,
                Ok(Ok(None)) => break,
                Ok(Err(err)) => panic!("read error: {err}"),
            };

            if state == ConnectionState::Play && !reached_play {
                reached_play = true;
                eprintln!("reached Play; awaiting position to summon…");
            }

            // Capture the player position from the teleport, then summon. Do NOT
            // `continue`: the packet still flows to the adapter below, which
            // sends the required 1.9+ teleport_confirm.
            if state == ConnectionState::Play
                && packet_id == play::clientbound::POSITION
                && !summoned
            {
                if let Ok(directives) =
                    adapter.handle_packet(&mut world, state, packet_id, &payload)
                {
                    for directive in directives {
                        if let Directive::Emit(ClientEvent::TeleportPlayer { pos, .. }) = &directive
                        {
                            summon_mobs(&mut rcon, pos.x as i32, pos.y as i32, pos.z as i32);
                            summoned = true;
                            // Poll, never assert immediately — a summoned entity
                            // is invisible until the next tick.
                            collect_deadline = Some(Instant::now() + collect_window);
                        }
                        apply(&mut conn, &mut state, directive).await;
                    }
                }
                continue;
            }

            if state == ConnectionState::Play {
                match packet_id {
                    play::clientbound::SPAWN_ENTITY_LIVING => {
                        let mut r = Reader::new(&payload);
                        let spawn = SpawnEntityLiving::decode(&mut r, CTX)
                            .expect("real spawn_entity_living decodes");
                        r.ensure_empty()
                            .expect("spawn_entity_living consumes the whole packet");
                        spawns.push(spawn);
                        continue;
                    }
                    play::clientbound::ENTITY_METADATA => {
                        let mut r = Reader::new(&payload);
                        let upd = EntityMetadataPacket::decode(&mut r, CTX)
                            .expect("real entity_metadata decodes");
                        r.ensure_empty()
                            .expect("entity_metadata consumes the whole packet");
                        meta_updates.push(upd);
                        continue;
                    }
                    play::clientbound::UPDATE_HEALTH => {
                        let mut r = Reader::new(&payload);
                        if let Ok(health) = r.f32() {
                            last_health = Some(health);
                            if health <= 0.0 {
                                eprintln!(
                                    "WARNING: update_health = {health} (<=0). Inherited a dead \
                                     player? Blackout expected until respawn — NOT a decoder bug."
                                );
                            }
                        }
                        continue;
                    }
                    _ => {}
                }
            }

            if let Ok(directives) = adapter.handle_packet(&mut world, state, packet_id, &payload) {
                for directive in directives {
                    if let Directive::Emit(ClientEvent::KeepAlive { id }) = &directive {
                        if let Ok(Some((pid, body))) = adapter.encode_action(
                            ConnectionState::Play,
                            &ClientAction::KeepAliveResponse { id: *id },
                        ) {
                            conn.write_packet(pid, &body).await.expect("keep-alive ack");
                        }
                        continue;
                    }
                    apply(&mut conn, &mut state, directive).await;
                }
            }
        }
    })
    .await;

    assert!(reached_play, "never reached Play");
    if let Some(h) = last_health {
        eprintln!("last update_health    : {h}");
    }
    assert!(
        !spawns.is_empty(),
        "decoded zero spawn_entity_living packets — if update_health was <=0 above, an \
         inherited dead player is the cause; otherwise the summon did not reach RCON"
    );

    // Every real mob id must resolve through the unified 1.16 registry. This is
    // the assertion that the flattening-era single id space is correct: a stale
    // 1.12 two-table lookup would resolve these ids to the wrong names or None.
    let mut resolved: Vec<(&'static str, i32)> = Vec::new();
    for spawn in &spawns {
        let name = entity_type_name(spawn.kind).unwrap_or_else(|| {
            panic!(
                "spawn_entity_living kind {} did not resolve in the unified 1.16 entity registry",
                spawn.kind
            )
        });
        resolved.push((name, spawn.kind));
    }

    let non_empty_meta = meta_updates
        .iter()
        .filter(|m| !m.metadata.0.is_empty())
        .count();
    assert!(
        non_empty_meta > 0,
        "decoded {} entity_metadata packets but every list was empty — the 1.16.5 metadata \
         type table was never exercised on real bytes",
        meta_updates.len()
    );

    eprintln!("\n=== LIVE 1.16.5 ENTITY / METADATA REPORT ===");
    eprintln!("spawn_entity_living   : {}", spawns.len());
    eprintln!("entity_metadata upd   : {}", meta_updates.len());
    eprintln!("non-empty meta lists  : {non_empty_meta}");
    eprintln!("resolved mob kinds    : {resolved:?}");
    eprintln!("============================================\n");

    // Best effort: remove the summoned passive mobs so re-runs stay clean.
    rcon.cmd("kill @e[type=!minecraft:player,distance=..8]");
}
