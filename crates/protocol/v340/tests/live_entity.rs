//! Live 1.12.2 entity-metadata acceptance test (the V2a gate for protocol 340).
//!
//! Gated behind the `live-entity` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run against a live vanilla 1.12.2 server
//! (offline mode, flat world) that this test can summon mobs on via a console
//! FIFO — by default the throwaway `lodestone-tw1122` container on
//! `127.0.0.1:25569`:
//!
//! ```text
//! cargo test -p lodestone-v340 --features live-entity -- --ignored live_entity
//! ```
//!
//! The shared `lodestone-mc1122` server on `:25568` has `spawn-monsters=false`
//! and `spawn-animals=false` (and must not be restarted), so unlike the 1.8
//! test this one cannot rely on natural spawns. Instead it drives a real join,
//! confirms the 1.9+ teleport (via the adapter, so the player is loaded), then
//! **summons mobs at the player's position through the server console** and
//! decodes the resulting `spawn_entity_living` / `entity_metadata` packets.
//!
//! Hazard honoured: a freshly summoned entity is not visible until the next
//! server tick, so this test never asserts immediately after summoning — it
//! polls its normal read loop until packets arrive or a deadline passes.
//!
//! Assertions mirror the 1.8 gate: zero trailing bytes on every entity packet
//! (`ensure_empty`), at least one `spawn_entity_living` with a non-empty
//! metadata list, and live decode of the join-time `held_item_slot`.
#![cfg(feature = "live-entity")]

use lodestone_testsupport::unique_username;
use std::process::Command;
use std::time::{Duration, Instant};

use lodestone_core::{Ctx, Decode, Reader};
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_v340::V340Adapter;
use lodestone_v340::packet_ids::play;
use lodestone_v340::packets::entity::{EntityMetadataPacket, SpawnEntityLiving};
use lodestone_v340::packets::game::ClientboundPositionLook;
use lodestone_v340::packets::metadata::MetadataValue;
use lodestone_v340::packets::window::HeldItemSlot;
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

const CTX: Ctx = Ctx { version: 340 };

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
    std::env::var("LODESTONE_V340_PORT")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(25569)
}

fn summon_container() -> String {
    std::env::var("LODESTONE_V340_CONTAINER").unwrap_or_else(|_| "lodestone-tw1122".into())
}

fn console_path() -> String {
    std::env::var("LODESTONE_V340_CONSOLE").unwrap_or_else(|_| "/w/console".into())
}

/// Summons a batch of mobs at `(x, y, z)` through the server console FIFO.
/// Best-effort: failures are reported but do not fail the test (a naturally
/// spawned mob or a re-run can still satisfy the assertions).
fn summon_mobs(x: i32, y: i32, z: i32) {
    let container = summon_container();
    let console = console_path();
    let mobs = [
        "minecraft:cow",
        "minecraft:sheep",
        "minecraft:zombie",
        "minecraft:creeper",
    ];
    for mob in mobs {
        // Summon slightly spread out so they are distinct entities in view.
        let cmd = format!("summon {mob} {x} {y} {z}\n");
        let redirect = format!("printf %s '{cmd}' > {console}");
        let status = Command::new("container")
            .args(["exec", &container, "sh", "-c", &redirect])
            .status();
        match status {
            Ok(s) if s.success() => {}
            other => eprintln!("summon '{mob}' best-effort failed: {other:?}"),
        }
    }
    eprintln!("summoned {} mobs at ({x},{y},{z})", mobs.len());
}

fn describe(value: &MetadataValue) -> String {
    match value {
        MetadataValue::Byte(v) => format!("byte({v})"),
        MetadataValue::VarInt(v) => format!("varint({v})"),
        MetadataValue::Float(v) => format!("float({v})"),
        MetadataValue::String(v) => format!("string({v:?})"),
        MetadataValue::Chat(v) => format!("chat({v:?})"),
        MetadataValue::Slot(_) => "slot".into(),
        MetadataValue::Bool(v) => format!("bool({v})"),
        MetadataValue::Rotation { .. } => "rotation".into(),
        MetadataValue::Position(_) => "position".into(),
        MetadataValue::OptPosition(o) => format!("opt_position({})", o.is_some()),
        MetadataValue::Direction(v) => format!("direction({v})"),
        MetadataValue::OptUuid(o) => format!("opt_uuid({})", o.is_some()),
        MetadataValue::BlockId(v) => format!("block_id({v})"),
        MetadataValue::Nbt(o) => format!("nbt({})", o.is_some()),
    }
}

#[tokio::test]
#[ignore = "requires a live 1.12.2 server with console FIFO on 127.0.0.1:25569"]
async fn decodes_real_entity_metadata_from_live_1_12_server() {
    let port = server_port();
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V340Adapter::new();
    // v340/v47 route packets through the adapter; chunk packets would apply to
    // this sink. This test ignores world state, so a throwaway sink suffices.
    let mut world = World::new();

    let mut conn = Connection::connect(("127.0.0.1", port))
        .await
        .expect("connect to live 1.12.2 server");
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

            // Capture the player position from the (first) teleport, then summon.
            // We do NOT `continue`: the packet still flows to the adapter below,
            // which sends the required 1.9+ teleport_confirm.
            if state == ConnectionState::Play
                && packet_id == play::clientbound::POSITION
                && !summoned
                && let Ok(pos) = {
                    let mut r = Reader::new(&payload);
                    ClientboundPositionLook::decode(&mut r, CTX)
                }
            {
                summon_mobs(pos.x as i32, pos.y as i32, pos.z as i32);
                summoned = true;
                // Start the collect window only after summoning (poll, never
                // assert immediately — a summoned entity is invisible until the
                // next tick).
                collect_deadline = Some(Instant::now() + collect_window);
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
         inherited dead player is the cause; otherwise the summon did not reach the console \
         (check the throwaway container name/port)"
    );

    let total_spawn_entries: usize = spawns.iter().map(|s| s.metadata.0.len()).sum();
    assert!(
        total_spawn_entries > 0,
        "spawn packets decoded but every metadata list was empty — suspicious"
    );
    assert!(
        !held_item_slots.is_empty(),
        "expected a clientbound held_item_slot on join"
    );

    eprintln!("\n=== LIVE 1.12.2 ENTITY-METADATA REPORT ===");
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
            "sample mob            : type={} at ({:.1},{:.1},{:.1}) with {} metadata entries",
            sample.kind,
            sample.x,
            sample.y,
            sample.z,
            sample.metadata.0.len()
        );
        for entry in sample.metadata.0.iter().take(5) {
            eprintln!("   key {:>2} = {}", entry.key, describe(&entry.value));
        }
    }
    eprintln!("==========================================\n");
}

/// The V5 seam gate for protocol 340: entity spawns and moves must cross the
/// **public** `ClientEvent` API through `handle_packet`, not merely decode.
///
/// Same discipline as the v47 gate: the test above decodes
/// `spawn_entity_living` bytes directly (a codec proof); this one routes every
/// Play packet through [`VersionAdapter::handle_packet`] and asserts on the
/// emitted `ClientEvent`s, so a spawn that decodes but is never dispatched
/// fails here. Anti-vacuity: resolved non-empty `entity_type` keys, finite
/// coordinates, and a negative control that requires a truncated spawn to be
/// rejected by the real dispatch path.
#[tokio::test]
#[ignore = "requires a live 1.12.2 server with console FIFO on 127.0.0.1:25569"]
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
    let adapter = V340Adapter::new();
    let mut world = World::new();

    let mut conn = Connection::connect(("127.0.0.1", port))
        .await
        .expect("connect to live 1.12.2 server");
    let mut state = ConnectionState::Handshaking;

    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    // Negative control: the real dispatch path must reject a truncated spawn.
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
    let mut summoned = false;

    let overall = Duration::from_secs(60);
    let read_timeout = Duration::from_secs(5);
    let collect_window = Duration::from_secs(20);
    let target_spawns = 3usize;
    let mut collect_deadline: Option<Instant> = None;

    let _ = tokio::time::timeout(overall, async {
        loop {
            if let Some(deadline) = collect_deadline
                && (spawned.len() >= target_spawns || Instant::now() >= deadline)
                && !spawned.is_empty()
            {
                break;
            }
            let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
            let (packet_id, payload) = match read {
                Err(_) => {
                    if collect_deadline.is_some() && !spawned.is_empty() {
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

            // Capture player position from the first teleport, then summon.
            // Note we do NOT short-circuit: the same POSITION packet still
            // flows through handle_packet below, which sends teleport_confirm.
            if state == ConnectionState::Play
                && packet_id == play::clientbound::POSITION
                && !summoned
                && let Ok(pos) = {
                    let mut r = Reader::new(&payload);
                    ClientboundPositionLook::decode(&mut r, CTX)
                }
            {
                summon_mobs(pos.x as i32, pos.y as i32, pos.z as i32);
                summoned = true;
                // Poll, never assert immediately: a summoned entity is invisible
                // until the next tick.
                collect_deadline = Some(Instant::now() + collect_window);
            }

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
        "no EntitySpawned crossed the public API — summon may have failed (check the \
         console FIFO / container) or the spawn dispatch branch is not wired"
    );

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

    eprintln!("\n=== LIVE 1.12.2 ENTITY-EVENT SEAM REPORT (handle_packet) ===");
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
    eprintln!("===========================================================\n");
}
