//! Live 1.8.9 entity-metadata acceptance test (the V2a gate for protocol 47).
//!
//! Gated behind the `live-entity` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run against the real vanilla 1.8.9 server
//! (offline mode, flat world, `spawn-monsters=true`) on `127.0.0.1:25566`:
//!
//! ```text
//! cargo test -p lodestone-v1-8 --features live-entity -- --ignored live_entity
//! ```
//!
//! The 1.8 test world spawns real hostile mobs (they routinely kill the test
//! player — see the death-trap note below), so this test does not need console
//! access: it drives a real join at the packet level over
//! [`lodestone_net::Connection`], then **passively decodes the
//! `spawn_entity_living` and `entity_metadata` packets the server pushes for
//! naturally-spawned entities**, asserting real properties against real bytes:
//!
//! * every entity packet decodes with **zero trailing bytes** (`ensure_empty`)
//!   — the single best detector of a subtly wrong metadata terminator or a
//!   miscounted type value;
//! * at least one `spawn_entity_living` is decoded and its embedded metadata
//!   list parses;
//! * window-family packets the server volunteers on join (notably the
//!   clientbound `held_item_slot`) are decoded too, giving live coverage of the
//!   V2b codecs without needing a container to open.
//!
//! It lives in the version crate (not `lodestone-client`) because it names this
//! crate's concrete packet types.
#![cfg(feature = "live-entity")]

use lodestone_testsupport::unique_username;
use std::time::{Duration, Instant};

use lodestone_core::{Ctx, Decode, Reader};

const CTX: Ctx = Ctx { version: 47 };
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_v1_8::V47Adapter;
use lodestone_v1_8::packet_ids::play;
use lodestone_v1_8::packets::entity::{EntityMetadataPacket, SpawnEntityLiving};
use lodestone_v1_8::packets::metadata::MetadataValue;
use lodestone_v1_8::packets::window::HeldItemSlot;
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

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
    std::env::var("LODESTONE_V47_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25566)
}

/// A short human description of a metadata value, for the report.
fn describe(value: &MetadataValue) -> String {
    match value {
        MetadataValue::Byte(v) => format!("byte({v})"),
        MetadataValue::Short(v) => format!("short({v})"),
        MetadataValue::Int(v) => format!("int({v})"),
        MetadataValue::Float(v) => format!("float({v})"),
        MetadataValue::String(v) => format!("string({v:?})"),
        MetadataValue::Slot(_) => "slot".into(),
        MetadataValue::Position { x, y, z } => format!("position({x},{y},{z})"),
        MetadataValue::Rotation { .. } => "rotation".into(),
    }
}

#[tokio::test]
#[ignore = "requires a live 1.8.9 server on 127.0.0.1:25566"]
async fn decodes_real_entity_metadata_from_live_1_8_server() {
    let port = server_port();
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V47Adapter::new();
    // v1-9/v1-8 route packets through the adapter; chunk packets would apply to
    // this sink. This test ignores world state, so a throwaway sink suffices.
    let mut world = World::new();

    let mut conn = Connection::connect(("127.0.0.1", port))
        .await
        .expect("connect to live 1.8.9 server");
    let mut state = ConnectionState::Handshaking;

    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    let mut spawns: Vec<SpawnEntityLiving> = Vec::new();
    let mut meta_updates: Vec<EntityMetadataPacket> = Vec::new();
    let mut held_item_slots: Vec<HeldItemSlot> = Vec::new();
    let mut window_items_count = 0usize;
    let mut set_slot_count = 0usize;
    let mut reached_play = false;
    let mut last_health: Option<f32> = None;

    let overall = Duration::from_secs(60);
    let read_timeout = Duration::from_secs(5);
    let collect_window = Duration::from_secs(25);
    let target_spawns = 5usize;
    let started = Instant::now();

    let _ = tokio::time::timeout(overall, async {
        loop {
            if reached_play
                && (spawns.len() >= target_spawns || started.elapsed() >= collect_window)
                && !spawns.is_empty()
            {
                break;
            }
            let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
            let (packet_id, payload) = match read {
                Err(_) => {
                    if reached_play && !spawns.is_empty() {
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
                eprintln!("reached Play; collecting entity + window packets…");
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
                    play::clientbound::HELD_ITEM_SLOT => {
                        let mut r = Reader::new(&payload);
                        let hel =
                            HeldItemSlot::decode(&mut r, CTX).expect("real held_item_slot decodes");
                        r.ensure_empty()
                            .expect("held_item_slot consumes the packet");
                        held_item_slots.push(hel);
                        continue;
                    }
                    play::clientbound::WINDOW_ITEMS => {
                        window_items_count += 1;
                        continue;
                    }
                    play::clientbound::SET_SLOT => {
                        set_slot_count += 1;
                        continue;
                    }
                    play::clientbound::UPDATE_HEALTH => {
                        let mut r = Reader::new(&payload);
                        if let Ok(health) = r.f32() {
                            last_health = Some(health);
                            if health <= 0.0 {
                                eprintln!(
                                    "WARNING: set_health = {health} (<=0). Inherited a dead \
                                     player? Entity/chunk blackout expected until respawn — NOT a \
                                     decoder bug."
                                );
                            }
                        }
                        continue;
                    }
                    _ => {}
                }
            }

            // Keep the connection alive so the collect window can elapse.
            if let Ok(directives) = adapter.handle_packet(&mut world, state, packet_id, &payload) {
                for directive in directives {
                    if let Directive::Emit(ClientEvent::KeepAlive { id }) = &directive {
                        if let Ok(Some((pid, body))) = adapter.encode_action(
                            ConnectionState::Play,
                            &lodestone_model::ClientAction::KeepAliveResponse { id: *id },
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
        eprintln!("last set_health       : {h}");
    }
    assert!(
        !spawns.is_empty(),
        "decoded zero spawn_entity_living packets — if set_health was <=0 above, an \
         inherited dead player is the cause, not the decoder; otherwise no mob spawned in \
         the collect window (try re-running or lengthening it)"
    );

    // Every spawned mob's embedded metadata parsed (proved by ensure_empty above);
    // count the entries so a silently-empty list can't masquerade as success.
    let total_spawn_entries: usize = spawns.iter().map(|s| s.metadata.0.len()).sum();
    assert!(
        total_spawn_entries > 0,
        "spawn packets decoded but every metadata list was empty — suspicious"
    );

    // The clientbound held_item_slot is sent on join in 1.8, giving live V2b
    // coverage; assert we saw and decoded at least one.
    assert!(
        !held_item_slots.is_empty(),
        "expected a clientbound held_item_slot on join (1.8 always sends one)"
    );

    eprintln!("\n=== LIVE 1.8.9 ENTITY-METADATA REPORT ===");
    eprintln!("spawn_entity_living   : {}", spawns.len());
    eprintln!("entity_metadata upd   : {}", meta_updates.len());
    eprintln!("total metadata entries: {total_spawn_entries}");
    eprintln!(
        "held_item_slot (join) : {} (slot {:?})",
        held_item_slots.len(),
        held_item_slots.first().map(|h| h.slot)
    );
    eprintln!("window_items          : {window_items_count}");
    eprintln!("set_slot              : {set_slot_count}");
    eprintln!("trailing bytes/packet : 0 (ensure_empty passed on every entity packet)");
    if let Some(sample) = spawns.first() {
        eprintln!(
            "sample mob            : type={} at ({},{},{}) with {} metadata entries",
            sample.kind,
            sample.x / 32,
            sample.y / 32,
            sample.z / 32,
            sample.metadata.0.len()
        );
        for entry in sample.metadata.0.iter().take(4) {
            eprintln!("   key {:>2} = {}", entry.key, describe(&entry.value));
        }
    }
    eprintln!("=========================================\n");
}

/// The V5 seam gate: entity spawns and moves must cross the **public**
/// `ClientEvent` API through `handle_packet`, not merely decode locally.
///
/// The distinction is the same one that made the chunk gate meaningful: the
/// existing test above decodes `spawn_entity_living` bytes directly, which only
/// proves the codec. This test feeds *every* Play packet through
/// [`VersionAdapter::handle_packet`] and asserts on the `ClientEvent`s the
/// adapter emits — so a spawn that decodes but is never dispatched (the
/// "correct decoder the adapter never calls" failure this project keeps hitting)
/// fails here.
///
/// Anti-vacuity guards built in deliberately:
/// * at least one `EntitySpawned` must carry a **resolved, non-empty**
///   `entity_type` `ResourceKey` (an unresolved id would have errored in the
///   adapter, so reaching the assertion already proves resolution);
/// * a **negative control** feeds a truncated `spawn_entity_living` payload
///   through the real `handle_packet` and requires a clean `Err`, proving the
///   dispatch path rejects malformed bytes rather than panicking or silently
///   emitting a bogus event.
#[tokio::test]
#[ignore = "requires a live 1.8.9 server on 127.0.0.1:25566"]
async fn entity_events_cross_the_public_api_via_handle_packet() {
    let port = server_port();
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V47Adapter::new();
    let mut world = World::new();

    let mut conn = Connection::connect(("127.0.0.1", port))
        .await
        .expect("connect to live 1.8.9 server");
    let mut state = ConnectionState::Handshaking;

    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    // Negative control: the real dispatch path must reject a truncated spawn
    // with a clean Err (not a panic, not a phantom event). A single byte is a
    // valid varint entity-id start but nowhere near a full spawn_entity_living.
    let truncated = [0x01u8];
    let neg = adapter.handle_packet(
        &mut world,
        ConnectionState::Play,
        play::clientbound::SPAWN_ENTITY_LIVING,
        &truncated,
    );
    assert!(
        neg.is_err(),
        "truncated spawn_entity_living must be rejected by handle_packet, got {neg:?}"
    );

    let mut spawned: Vec<ClientEvent> = Vec::new();
    let mut moved: usize = 0;
    let mut removed: usize = 0;
    let mut reached_play = false;

    let overall = Duration::from_secs(60);
    let read_timeout = Duration::from_secs(5);
    let collect_window = Duration::from_secs(25);
    let target_spawns = 3usize;
    let started = Instant::now();

    let _ = tokio::time::timeout(overall, async {
        loop {
            if reached_play
                && (spawned.len() >= target_spawns || started.elapsed() >= collect_window)
                && !spawned.is_empty()
            {
                break;
            }
            let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
            let (packet_id, payload) = match read {
                Err(_) => {
                    if reached_play && !spawned.is_empty() {
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
                eprintln!("reached Play; routing all packets through handle_packet…");
            }

            // Route every packet through the real adapter entry point. Spawn
            // packets that decode to an unknown mob id would surface as Err
            // here; we let those propagate rather than swallowing them, so a
            // resolution regression can't hide.
            match adapter.handle_packet(&mut world, state, packet_id, &payload) {
                Ok(directives) => {
                    for directive in directives {
                        match &directive {
                            Directive::Emit(event @ ClientEvent::EntitySpawned { .. }) => {
                                spawned.push(event.clone());
                            }
                            Directive::Emit(ClientEvent::EntityMoved { .. }) => moved += 1,
                            Directive::Emit(ClientEvent::EntityRemoved { .. }) => removed += 1,
                            Directive::Emit(ClientEvent::KeepAlive { id }) => {
                                if let Ok(Some((pid, body))) = adapter.encode_action(
                                    ConnectionState::Play,
                                    &lodestone_model::ClientAction::KeepAliveResponse { id: *id },
                                ) {
                                    conn.write_packet(pid, &body).await.expect("keep-alive ack");
                                }
                                continue;
                            }
                            _ => {}
                        }
                        apply(&mut conn, &mut state, directive).await;
                    }
                }
                Err(err) => panic!(
                    "handle_packet errored on live packet id {packet_id}: {err} — a real \
                     server packet the adapter mis-decodes is a bug, not a skip"
                ),
            }
        }
    })
    .await;

    assert!(reached_play, "never reached Play");
    assert!(
        !spawned.is_empty(),
        "no EntitySpawned crossed the public API — if the world is empty try tw-style \
         spawn-monsters, otherwise the spawn dispatch branch is not wired"
    );

    // Anti-vacuity: prove the events carry resolved, non-empty type keys and
    // real coordinates, so "correctly spawned nothing" cannot pass.
    let mut resolved = 0usize;
    for ev in &spawned {
        if let ClientEvent::EntitySpawned {
            entity_type, pos, ..
        } = ev
        {
            assert!(
                !entity_type.namespace().is_empty() && !entity_type.path().is_empty(),
                "EntitySpawned carried an unresolved entity_type: {entity_type:?}"
            );
            assert!(
                pos.x.is_finite() && pos.y.is_finite() && pos.z.is_finite(),
                "EntitySpawned carried non-finite coordinates"
            );
            resolved += 1;
        }
    }
    assert!(resolved > 0, "no resolved EntitySpawned events");

    eprintln!("\n=== LIVE 1.8.9 ENTITY-EVENT SEAM REPORT (handle_packet) ===");
    eprintln!("EntitySpawned (via API): {}", spawned.len());
    eprintln!("EntityMoved   (via API): {moved}");
    eprintln!("EntityRemoved (via API): {removed}");
    if let Some(ClientEvent::EntitySpawned {
        entity_type, pos, ..
    }) = spawned.first()
    {
        eprintln!(
            "sample spawn           : {}:{} at ({:.1},{:.1},{:.1})",
            entity_type.namespace(),
            entity_type.path(),
            pos.x,
            pos.y,
            pos.z
        );
    }
    eprintln!("negative control       : truncated spawn rejected by handle_packet ✓");
    eprintln!("==========================================================\n");
}
