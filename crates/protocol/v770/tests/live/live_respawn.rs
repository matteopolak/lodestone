//! Live respawn acceptance test (T1 gate).
//!
//! Gated behind the `live-chunk` feature AND `#[ignore]`, so the default
//! `cargo test` stays hermetic. Run it against the real vanilla 26.2 server
//! (offline mode, flat world) on `127.0.0.1:25565` with:
//!
//! ```text
//! cargo test -p lodestone-v770 --features live-chunk --test live_respawn -- --ignored --nocapture
//! ```
//!
//! This is the acceptance gate for death/respawn handling. A headless client
//! that dies and stays dead is useless: the vanilla server holds a dead player
//! on the death screen and streams **zero chunks** until it receives
//! `client_command(perform_respawn)`. This test drives that whole cycle against
//! the live server at the packet level:
//!
//! 1. join a fresh player (unique name) and read the initial chunk stream;
//! 2. kill the player via RCON (`kill <name>`), triggering the real
//!    `set_health` (health 0) and `player_combat_kill` packets;
//! 3. on the death notification, send `client_command(perform_respawn)` through
//!    the v770 adapter's `ClientAction::Respawn` path — the exact code the
//!    high-level driver runs when [`RespawnPolicy::Automatic`] is set;
//! 4. assert the server **resumes** streaming chunks afterwards — the one
//!    property that proves we actually left the death screen.
//!
//! RCON is reached by `container exec lodestone-mc262 perl …` speaking the RCON
//! protocol to the server's in-container listener (the port is not published to
//! the host). The suite enabling RCON is documented in the task report.
//! `container exec` takes the same `<container-id> <arguments>` shape Docker's
//! does, so this is a straight CLI-name swap — no change to the perl or its
//! invocation.

#![cfg(feature = "live-chunk")]

use std::time::Duration;

use lodestone_core::{Reader, Writer};
use lodestone_model::{
    ClientAction, ClientEvent, ConnectionState, Directive, LoginProfile, ServerAddress,
    VersionAdapter,
};
use lodestone_net::Connection;
use lodestone_v770::V770Adapter;
use lodestone_v770::packet_ids::play;
use lodestone_world::World;
use tokio::net::TcpStream;
use uuid::Uuid;

#[path = "../common/mod.rs"]
mod common;
use common::unique_username;

/// A tiny RCON client run inside the server container (the RCON port is not
/// published to the host, so we reach it via `container exec`). Reads password
/// and command from `@ARGV`, prints the command response.
const RCON_PERL: &str = r#"
use IO::Socket::INET;
my ($pw,$cmd)=@ARGV;
my $s=IO::Socket::INET->new(PeerAddr=>"127.0.0.1",PeerPort=>25575,Proto=>"tcp") or die "conn:$!";
sub pkt{my($id,$t,$b)=@_; my $p=pack("VV",$id,$t).$b."\0\0"; pack("V",length($p)).$p}
sub rd{my $l; read($s,$l,4); $l=unpack("V",$l); my $d; read($s,$d,$l); my($id,$t)=unpack("VV",substr($d,0,8)); my $b=substr($d,8); $b=~s/\0+$//; ($id,$t,$b)}
print $s pkt(1,3,$pw); my($id)=rd(); die "auth failed" if $id==-1;
print $s pkt(2,2,$cmd); my($i,$t,$b)=rd(); print "$b";
"#;

async fn rcon(command: &str) -> String {
    let output = std::process::Command::new("container")
        .args([
            "exec",
            "lodestone-mc262",
            "perl",
            "-e",
            RCON_PERL,
            "--",
            "lodestone",
            command,
        ])
        .output()
        .expect("run container exec perl rcon");
    assert!(
        output.status.success(),
        "rcon `{command}` failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

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

/// Acknowledges a finished chunk batch so vanilla sends the next one; without
/// this only the first batch ever arrives.
async fn ack_chunk_batch(conn: &mut Connection<TcpStream>) {
    let mut w = Writer::default();
    w.f32(32.0);
    conn.write_packet(play::serverbound::CHUNK_BATCH_RECEIVED, &w.into_vec())
        .await
        .expect("ack chunk batch");
}

#[tokio::test]
#[ignore = "requires a live Minecraft server with RCON on lodestone-mc262"]
async fn client_respawns_and_resumes_chunks_after_death() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };
    let name = unique_username();
    let profile = LoginProfile {
        username: name.clone(),
        uuid: Uuid::new_v4(),
    };
    let adapter = V770Adapter::new();

    let mut conn = Connection::connect("127.0.0.1:25565")
        .await
        .expect("connect to live server");
    let mut state = ConnectionState::Handshaking;
    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        apply(&mut conn, &mut state, directive).await;
    }

    // Phase state.
    let mut chunks_before = 0usize;
    let mut chunks_after = 0usize;
    let mut killed = false;
    let mut respawn_sent = false;
    let mut saw_death = false;
    let mut saw_zero_health = false;

    let read_timeout = Duration::from_secs(5);
    let overall = Duration::from_secs(90);

    let result = tokio::time::timeout(overall, async {
        loop {
            // Once we're in Play with an initial chunk view, kill the player.
            if state == ConnectionState::Play && !killed && chunks_before >= 20 {
                let out = rcon(&format!("kill {name}")).await;
                eprintln!("rcon kill -> {out:?}");
                killed = true;
            }

            // Stop once we've proven chunks resumed after respawn.
            if respawn_sent && chunks_after >= 20 {
                return true;
            }

            let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
            let (packet_id, payload) = match read {
                Err(_) => {
                    // The server goes quiet on the death screen; if we haven't
                    // respawned yet that's expected, keep looping to allow the
                    // kill/respawn to progress. Otherwise treat as done.
                    if respawn_sent {
                        return chunks_after > 0;
                    }
                    continue;
                }
                Ok(Ok(Some(p))) => p,
                Ok(Ok(None)) => return false, // clean EOF
                Ok(Err(err)) => panic!("read error: {err}"),
            };

            if state != ConnectionState::Play {
                if let Ok(directives) =
                    adapter.handle_packet(&mut World::new(), state, packet_id, &payload)
                {
                    for directive in directives {
                        apply(&mut conn, &mut state, directive).await;
                    }
                }
                continue;
            }

            match packet_id {
                id if id == play::clientbound::LEVEL_CHUNK_WITH_LIGHT => {
                    if respawn_sent {
                        chunks_after += 1;
                    } else {
                        chunks_before += 1;
                    }
                }
                id if id == play::clientbound::CHUNK_BATCH_FINISHED => {
                    ack_chunk_batch(&mut conn).await;
                }
                id if id == play::clientbound::SET_HEALTH => {
                    // health is the leading big-endian f32.
                    let mut r = Reader::new(&payload);
                    let health = r.f32().expect("set_health f32");
                    eprintln!("set_health -> {health}");
                    if health == 0.0 {
                        saw_zero_health = true;
                    }
                }
                id if id == play::clientbound::PLAYER_COMBAT_KILL => {
                    saw_death = true;
                    // Confirm the adapter surfaces a Death event for this packet.
                    let directives = adapter
                        .handle_packet(&mut World::new(), state, packet_id, &payload)
                        .expect("handle combat_kill");
                    assert!(
                        matches!(
                            directives.as_slice(),
                            [Directive::Emit(ClientEvent::Death { .. })]
                        ),
                        "combat_kill should surface a Death event, got {directives:?}"
                    );
                    // Send perform_respawn through the real action path.
                    if let Some((pid, body)) = adapter
                        .encode_action(state, &ClientAction::Respawn)
                        .expect("encode respawn")
                    {
                        conn.write_packet(pid, &body).await.expect("write respawn");
                        respawn_sent = true;
                        chunks_after = 0;
                        eprintln!("sent perform_respawn (packet {pid})");
                    }
                }
                _ => {
                    if let Ok(directives) =
                        adapter.handle_packet(&mut World::new(), state, packet_id, &payload)
                    {
                        for directive in directives {
                            apply(&mut conn, &mut state, directive).await;
                        }
                    }
                }
            }
        }
    })
    .await;

    eprintln!("\n=== LIVE RESPAWN REPORT ===");
    eprintln!("player name              : {name}");
    eprintln!("chunks before death      : {chunks_before}");
    eprintln!("saw set_health 0.0       : {saw_zero_health}");
    eprintln!("saw combat_kill (Death)  : {saw_death}");
    eprintln!("sent perform_respawn     : {respawn_sent}");
    eprintln!("chunks after respawn     : {chunks_after}");
    eprintln!("===========================\n");

    assert!(chunks_before > 0, "never received chunks before death");
    assert!(saw_death, "server never sent the combat_kill death packet");
    assert!(respawn_sent, "client never sent perform_respawn");
    assert_eq!(
        result,
        Ok(true),
        "chunks did not resume after respawn (still stuck on death screen?)"
    );
    assert!(
        chunks_after > 0,
        "server streamed no chunks after respawn — respawn did not take effect"
    );
}
