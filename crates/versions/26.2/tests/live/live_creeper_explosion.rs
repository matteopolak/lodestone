//! Live capture + acceptance gate for a creeper's fuse metadata and its
//! detonation sound.
//!
//! Gated behind the `live-creeper-explosion` feature AND `#[ignore]`, so the
//! default `cargo test` stays hermetic. Run it against the real vanilla 26.2
//! survival server (`lodestone-survival`, game `127.0.0.1:25565`, RCON
//! `:25566`, password `lodestone`) with:
//!
//! ```text
//! cargo test -p lodestone-v26-2 --features live-creeper-explosion \
//!     --test live_creeper_explosion -- --ignored --nocapture
//! ```
//!
//! # What this gate is for
//!
//! Two separate wire surfaces this repo had never decoded before: a
//! creeper's `DATA_SWELL_DIR`/`DATA_IS_POWERED`/`DATA_IS_IGNITED` metadata
//! (issue: live player report, "no swelling, no white flash"), and the
//! `explode` packet's `explosionSound` field (same report, "hiss but no
//! explosion sound"). This joins the real server, summons a pre-ignited
//! creeper, waits for it to detonate, and captures the **raw
//! `set_entity_data` and `explode` payloads the server authored** — then
//! feeds exactly those bytes through the public adapter seam and asserts
//! what comes out. Per `CLAUDE.md`'s evidence standard, this is deliberately
//! a *second*, independent check alongside `sound_particle_screen.rs`'s
//! hand-assembled fixtures: those are transcribed from the decompiled wire
//! spec, this one is what a real server actually sends.
//!
//! Per §12.52 this fails rather than skips when it cannot run.

#![cfg(feature = "live-creeper-explosion")]

use std::time::Duration;

use lodestone_core::{Reader, Writer};
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress, SoundCategory,
    VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_testsupport::{RconClient, unique_username};
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

const SERVER_ADDR: &str = "127.0.0.1:25565";
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";

const REPAIR: &str = "recreate the survival oracle with: ./scripts/live-oracles/survival.sh \
    (expected a vanilla 26.2 survival server on 127.0.0.1:25565, RCON :25566)";

async fn apply(conn: &mut Connection<TcpStream>, state: &mut ConnectionState, directive: Directive) {
    match directive {
        Directive::Send { packet_id, payload } => {
            conn.write_packet(packet_id, &payload).await.expect("write packet");
        }
        Directive::SetState(next) => *state = next,
        Directive::SetCompression(threshold) => conn.set_compression(threshold),
        Directive::Disconnect(reason) => {
            panic!("server disconnected us: {}", reason.to_plain_string())
        }
        _ => {}
    }
}

async fn ack_chunk_batch(conn: &mut Connection<TcpStream>) {
    let mut w = Writer::default();
    w.f32(32.0);
    conn.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, &w.into_vec())
        .await
        .expect("ack chunk batch");
}

/// Everything captured from one detonation: every raw `set_entity_data`
/// payload the creeper itself sent (in arrival order — its fuse can be synced
/// across more than one packet), and the raw `explode` payload.
struct Capture {
    creeper_metadata: Vec<Vec<u8>>,
    explode: Vec<u8>,
}

#[tokio::test]
#[ignore = "requires the live 26.2 survival oracle (scripts/live-oracles/survival.sh)"]
async fn a_pre_ignited_creeper_syncs_its_fuse_and_detonates_with_sound() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };
    let username = unique_username();
    let profile = LoginProfile {
        username: username.clone(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V770Adapter::new();

    let mut conn = match Connection::connect(SERVER_ADDR).await {
        Ok(conn) => conn,
        Err(err) => panic!("could not reach {SERVER_ADDR}: {err}. {REPAIR}"),
    };
    let mut state = ConnectionState::Handshaking;
    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    let mut world = World::new();
    let joined = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let (packet_id, payload) = match conn.read_packet().await {
                Ok(Some(p)) => p,
                Ok(None) => return false,
                Err(err) => panic!("read error during join: {err}"),
            };
            if state == ConnectionState::Play && packet_id == play::clientbound::CHUNK_BATCH_FINISHED {
                ack_chunk_batch(&mut conn).await;
                return true;
            }
            if let Ok(directives) = adapter.handle_packet(&mut world, state, packet_id, &payload) {
                for directive in directives {
                    apply(&mut conn, &mut state, directive).await;
                }
            }
        }
    })
    .await
    .expect("timed out joining the world");
    assert!(joined, "connection closed before reaching Play");

    let mut rcon = RconClient::connect(RCON_ADDR, RCON_PASSWORD)
        .unwrap_or_else(|err| panic!("could not reach RCON {RCON_ADDR}: {err}. {REPAIR}"));

    // `ignited:1b` calls `vanilla's own creeper's own ignite()` at load (`vanilla's own creeper's own java`),
    // so the very first tick primes the fuse without waiting on proximity AI.
    // `NoAI:1b` only disables the goal selector — `tick()`'s fuse integration
    // is unconditional, so this does not slow detonation, it just keeps the
    // creeper from wandering off mid-capture.
    let summon = format!(
        "execute at {username} run summon minecraft:creeper ~ ~2 ~ {{ignited:1b,NoAI:1b}}"
    );
    let reply = rcon.cmd(&summon);
    println!("rcon summon reply: {reply}");
    let lower = reply.to_lowercase();
    assert!(
        !lower.contains("incorrect") && !lower.contains("unknown") && !lower.contains("expected"),
        "server rejected the summon, so the SNBT is wrong: {reply}"
    );

    let mut creeper_id: Option<i32> = None;
    let mut capture = Capture {
        creeper_metadata: Vec::new(),
        explode: Vec::new(),
    };

    // 30 ticks at 20/s is 1.5s to detonation; give it generous headroom for a
    // shared, possibly loaded, oracle host.
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let (packet_id, payload) = match conn.read_packet().await {
                Ok(Some(p)) => p,
                Ok(None) => panic!("connection closed before the creeper detonated"),
                Err(err) => panic!("read error after summon: {err}"),
            };
            if state == ConnectionState::Play && packet_id == play::clientbound::CHUNK_BATCH_FINISHED {
                ack_chunk_batch(&mut conn).await;
                continue;
            }
            // Stash raw bytes *before* the adapter runs, so the captured
            // evidence is independent of the code under test.
            if state == ConnectionState::Play && packet_id == play::clientbound::SET_ENTITY_DATA {
                let mut peek = Reader::new(&payload);
                if let (Some(target), Ok(id)) = (creeper_id, peek.var_i32())
                    && id == target
                {
                    capture.creeper_metadata.push(payload.clone());
                }
            }
            if state == ConnectionState::Play && packet_id == play::clientbound::EXPLODE {
                capture.explode = payload.clone();
            }
            let directives = adapter
                .handle_packet(&mut world, state, packet_id, &payload)
                .unwrap_or_else(|err| {
                    panic!("adapter failed to decode a real server packet (id {packet_id}): {err}")
                });
            for directive in &directives {
                if let Directive::Emit(ClientEvent::EntitySpawned {
                    entity_id,
                    entity_type,
                    ..
                }) = directive
                    && entity_type.to_string() == "minecraft:creeper"
                {
                    creeper_id = Some(*entity_id);
                }
            }
            for directive in directives {
                apply(&mut conn, &mut state, directive).await;
            }
            if !capture.explode.is_empty() {
                return;
            }
        }
    })
    .await
    .expect("timed out waiting for the creeper to detonate");

    let creeper_id = creeper_id.expect("must have seen the creeper's ADD_ENTITY");
    println!(
        "captured {} set_entity_data payload(s) for creeper {creeper_id}, plus one explode \
         payload ({} bytes)",
        capture.creeper_metadata.len(),
        capture.explode.len()
    );
    assert!(
        !capture.creeper_metadata.is_empty(),
        "a pre-ignited creeper must sync at least one metadata update before detonating"
    );

    // Replay every captured `set_entity_data` and fold, exactly as a live
    // client would — `ignited` and `swell_dir` may arrive on the first tick's
    // packet, or split across a couple of ticks depending on scheduling.
    let mut swell_dir = None;
    let mut powered = None;
    let mut ignited = None;
    for raw in &capture.creeper_metadata {
        let directives = adapter
            .handle_packet(&mut World::new(), ConnectionState::Play, play::clientbound::SET_ENTITY_DATA, raw)
            .expect("replaying a real set_entity_data must not error");
        for directive in directives {
            if let Directive::Emit(ClientEvent::EntityMetadataUpdated { metadata, .. }) = directive {
                if let Some(v) = metadata.creeper_swell_dir {
                    swell_dir = Some(v);
                }
                if let Some(v) = metadata.creeper_powered {
                    powered = Some(v);
                }
                if let Some(v) = metadata.creeper_ignited {
                    ignited = Some(v);
                }
            }
        }
    }
    assert_eq!(
        ignited,
        Some(true),
        "a creeper summoned with ignited:1b must report DATA_IS_IGNITED true; got {ignited:?} \
         from {} captured payload(s)",
        capture.creeper_metadata.len()
    );
    assert_eq!(
        swell_dir,
        Some(1),
        "an ignited creeper's tick() sets swellDir to 1 on its very first tick \
         (vanilla's own creeper's own java); got {swell_dir:?}"
    );
    // `powered` stays at vanilla's default (`false`), which `SynchedEntityData`
    // never puts on the wire at all for an un-struck creeper — so `None` (never
    // mentioned) is the expected live capture, not `Some(false)`. Either reading
    // is "not charged"; only `Some(true)` would be wrong here.
    assert_ne!(powered, Some(true), "this creeper was never lightning-struck");

    // Replay the explode packet through the same public seam.
    let directives = adapter
        .handle_packet(
            &mut World::new(),
            ConnectionState::Play,
            play::clientbound::EXPLODE,
            &capture.explode,
        )
        .expect("replaying a real explode packet must not error");
    // A leading `Particles` directive (the shockwave/smoke
    // visual) now precedes the `Sound` directive this test was already
    // pinning.
    assert_eq!(
        directives.len(),
        2,
        "one Particles directive, then one Sound directive, from a real explode packet"
    );
    assert!(
        matches!(
            &directives[0],
            Directive::Emit(ClientEvent::Particles { .. })
        ),
        "expected a Particles directive first, got {:?}",
        directives[0]
    );
    let Directive::Emit(ClientEvent::Sound {
        sound, category, volume, pitch, ..
    }) = &directives[1]
    else {
        panic!("expected a Sound directive second, got {:?}", directives[1]);
    };
    println!("decoded explosion sound: {sound} category={category:?} volume={volume} pitch={pitch}");
    assert_eq!(
        sound.to_string(),
        "minecraft:entity.generic.explode",
        "an un-powered creeper's detonation must use the plain generic-explode sound"
    );
    assert_eq!(*category, SoundCategory::Block);
    assert_eq!(*volume, 4.0);
    assert!((0.56..=0.84).contains(pitch), "pitch {pitch} outside vanilla's rolled band");
}
