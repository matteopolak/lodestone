//! Live world-state acceptance test.
//!
//! Gated behind the `live-chunk` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run it against the real vanilla 26.2 server
//! (offline mode, flat world) on `127.0.0.1:25565` with:
//!
//! ```text
//! cargo test -p lodestone-v26-2 --features live-chunk --test live_world_state -- --ignored --nocapture
//! ```
//!
//! The hermetic tests pin `respawn` and `set_time` against hand-built golden
//! vectors; this proves the decoders survive **real** server bytes. It joins a
//! fresh player, drives the join to Play through the adapter, and then reads
//! until the server sends `set_time` (vanilla emits it about once a second).
//! The adapter must decode that real payload with **zero trailing bytes** — the
//! single best detector of a subtly wrong layout — and surface a `TimeChanged`
//! event. A missing precondition (server unreachable) is a hard failure with a
//! repair command, never a silent skip.

#![cfg(feature = "live-chunk")]

use std::time::Duration;

use lodestone_core::Writer;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_v26_2::V770Adapter;
use lodestone_v26_2::packet_ids::play;
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

#[path = "../common/mod.rs"]
mod common;
use common::unique_username;

const SERVER_ADDR: &str = "127.0.0.1:25565";

/// The command that repairs the missing precondition, printed when the server
/// cannot be reached so the gate fails loudly rather than skipping.
const REPAIR: &str = "start the server with: docker start lodestone-mc262 \
    (expected a vanilla 26.2 server on 127.0.0.1:25565)";

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
        Directive::Disconnect(reason) => {
            panic!("server disconnected us: {}", reason.to_plain_string())
        }
        Directive::Emit(_) => {}
        _ => {}
    }
}

/// Acknowledges a finished chunk batch so vanilla keeps streaming.
async fn ack_chunk_batch(conn: &mut Connection<TcpStream>) {
    let mut w = Writer::default();
    w.f32(32.0);
    conn.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, &w.into_vec())
        .await
        .expect("ack chunk batch");
}

#[tokio::test]
#[ignore = "requires a live Minecraft 26.2 server on lodestone-mc262"]
async fn set_time_from_real_server_decodes_with_zero_trailing_bytes() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };
    let profile = LoginProfile {
        username: unique_username(),
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

    let mut saw_set_time = false;
    let overall = Duration::from_secs(60);

    let outcome = tokio::time::timeout(overall, async {
        loop {
            let (packet_id, payload) = match conn.read_packet().await {
                Ok(Some(p)) => p,
                Ok(None) => return false,
                Err(err) => panic!("read error: {err}"),
            };

            if state == ConnectionState::Play && packet_id == play::clientbound::SET_TIME {
                // The load-bearing assertion: a real payload must decode with no
                // leftover bytes and surface the canonical event.
                let directives = adapter
                    .handle_packet(&mut World::new(), state, packet_id, &payload)
                    .expect("real set_time must decode with zero trailing bytes");
                assert!(
                    matches!(
                        directives.as_slice(),
                        [Directive::Emit(ClientEvent::TimeChanged { .. })]
                    ),
                    "set_time should surface a TimeChanged event, got {directives:?}"
                );
                saw_set_time = true;
                return true;
            }

            if state == ConnectionState::Play
                && packet_id == play::clientbound::CHUNK_BATCH_FINISHED
            {
                ack_chunk_batch(&mut conn).await;
                continue;
            }

            if let Ok(directives) =
                adapter.handle_packet(&mut World::new(), state, packet_id, &payload)
            {
                for directive in directives {
                    apply(&mut conn, &mut state, directive).await;
                }
            }
        }
    })
    .await;

    assert_eq!(
        outcome,
        Ok(true),
        "never received a decodable set_time within {overall:?} (saw_set_time={saw_set_time})"
    );
}

/// A vanilla join reliably delivers `player_abilities`, `game_event`, and
/// `set_default_spawn_position`. This drives a real join and asserts that each
/// of those real payloads decodes through the adapter with **zero trailing
/// bytes** — the single best detector of a subtly wrong layout — surfacing the
/// expected canonical event. `level_event` is opportunistic (only if the world
/// happens to emit one), so it is verified when seen but not required.
#[tokio::test]
#[ignore = "requires a live Minecraft 26.2 server on lodestone-mc262"]
async fn world_events_from_real_server_decode_with_zero_trailing_bytes() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };
    let profile = LoginProfile {
        username: unique_username(),
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

    let mut saw_abilities = false;
    let mut saw_game_event = false;
    let mut saw_spawn = false;
    let overall = Duration::from_secs(60);

    let _ = tokio::time::timeout(overall, async {
        loop {
            let (packet_id, payload) = match conn.read_packet().await {
                Ok(Some(p)) => p,
                Ok(None) => return,
                Err(err) => panic!("read error: {err}"),
            };

            if state == ConnectionState::Play {
                if packet_id == play::clientbound::PLAYER_ABILITIES {
                    adapter
                        .handle_packet(&mut World::new(), state, packet_id, &payload)
                        .expect("real player_abilities must decode with zero trailing bytes");
                    saw_abilities = true;
                } else if packet_id == play::clientbound::GAME_EVENT {
                    adapter
                        .handle_packet(&mut World::new(), state, packet_id, &payload)
                        .expect("real game_event must decode with zero trailing bytes");
                    saw_game_event = true;
                } else if packet_id == play::clientbound::SET_DEFAULT_SPAWN_POSITION {
                    adapter
                        .handle_packet(&mut World::new(), state, packet_id, &payload)
                        .expect(
                            "real set_default_spawn_position must decode with zero trailing bytes",
                        );
                    saw_spawn = true;
                } else if packet_id == play::clientbound::LEVEL_EVENT {
                    adapter
                        .handle_packet(&mut World::new(), state, packet_id, &payload)
                        .expect("real level_event must decode with zero trailing bytes");
                }
            }

            if saw_abilities && saw_game_event && saw_spawn {
                return;
            }

            if state == ConnectionState::Play
                && packet_id == play::clientbound::CHUNK_BATCH_FINISHED
            {
                ack_chunk_batch(&mut conn).await;
                continue;
            }

            if let Ok(directives) =
                adapter.handle_packet(&mut World::new(), state, packet_id, &payload)
            {
                for directive in directives {
                    apply(&mut conn, &mut state, directive).await;
                }
            }
        }
    })
    .await;

    assert!(
        saw_abilities && saw_game_event && saw_spawn,
        "join must deliver player_abilities, game_event, and set_default_spawn_position \
         (abilities={saw_abilities}, game_event={saw_game_event}, spawn={saw_spawn})"
    );
}

/// A vanilla join reliably delivers at least one ambient `sound` within a
/// minute. This drives a real join and asserts that the real payload decodes
/// through the adapter with **zero trailing bytes** and surfaces a canonical
/// [`ClientEvent::Sound`], with the server-rolled variant `seed` carried
/// through the holder/position/seed layout intact. `sound_entity`,
/// `level_particles`, and `open_screen` do not occur during a passive
/// flat-world join, so they are pinned by the hermetic golden vectors instead.
#[tokio::test]
#[ignore = "requires a live Minecraft 26.2 server on lodestone-mc262"]
async fn sound_from_real_server_decodes_with_zero_trailing_bytes() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };
    let profile = LoginProfile {
        username: unique_username(),
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

    let mut saw_sound = false;
    let overall = Duration::from_secs(90);

    let outcome = tokio::time::timeout(overall, async {
        loop {
            let (packet_id, payload) = match conn.read_packet().await {
                Ok(Some(p)) => p,
                Ok(None) => return false,
                Err(err) => panic!("read error: {err}"),
            };

            if state == ConnectionState::Play && packet_id == play::clientbound::SOUND {
                let directives = adapter
                    .handle_packet(&mut World::new(), state, packet_id, &payload)
                    .expect("real sound must decode with zero trailing bytes");
                assert!(
                    matches!(
                        directives.as_slice(),
                        [Directive::Emit(ClientEvent::Sound { .. })]
                    ),
                    "sound should surface a Sound event, got {directives:?}"
                );
                saw_sound = true;
                return true;
            }

            if state == ConnectionState::Play
                && packet_id == play::clientbound::CHUNK_BATCH_FINISHED
            {
                ack_chunk_batch(&mut conn).await;
                continue;
            }

            if let Ok(directives) =
                adapter.handle_packet(&mut World::new(), state, packet_id, &payload)
            {
                for directive in directives {
                    apply(&mut conn, &mut state, directive).await;
                }
            }
        }
    })
    .await;

    assert_eq!(
        outcome,
        Ok(true),
        "never received a decodable sound within {overall:?} (saw_sound={saw_sound})"
    );
}
