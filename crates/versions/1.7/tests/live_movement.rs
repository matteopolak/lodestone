//! Live gate: does a real protocol 5 server actually accept this client's
//! movement, and does it unload the columns left behind?
//!
//! # Why this needs a live server
//!
//! Three separate defects in this crate's movement path were invisible to
//! every hermetic test, because a protocol 5 server's response to all three is
//! to silently stop accepting movement — no error, no disconnect, nothing in
//! its log. `tests/movement.rs` pins the shapes now that they are known, but
//! nothing short of a real server would have found them, and nothing short of
//! a real server proves they stay found: a fourth mistake of the same class
//! would pass every hermetic test in this crate.
//!
//! The discriminator is chosen so that a held player and a moving one look
//! nothing alike, rather than differing by a little:
//!
//! | outcome over a 320-block walk | held player | accepted movement |
//! |---|---|---|
//! | server re-sends its own position | 65-70 times | once |
//! | chunk columns loaded | 445 (the join burst, and no more) | 759 |
//! | chunk unloads for columns left behind | 0 | 420 |
//!
//! Those are measured numbers from both arms, not estimates: the left column
//! is what this crate produced before the fixes and the right is what it
//! produces now.
//!
//! # Running
//!
//! ```text
//! ./scripts/live-oracles/legacy.sh 1.7.10
//! cargo test -p lodestone-v1-7 --test live_movement -- --ignored --nocapture
//! ```

use std::time::{Duration, Instant};

use lodestone_model::{
    ClientAction, ClientEvent, ConnectionState, Directive, LoginProfile, Rotation, ServerAddress,
    Vec3, VersionAdapter,
};
use lodestone_world::World;

/// Minecraft version this gate targets.
const MINECRAFT: &str = "1.7.10";

/// Game port `scripts/live-oracles/legacy.sh` gives this version.
const GAME_PORT: u16 = 25602;

/// Feet y of a default flat world's surface: bedrock, two dirt, grass, so the
/// first free layer is 4. Walking below it puts the player inside blocks,
/// which the server rejects for a reason that has nothing to do with the wire.
const SURFACE_Y: f64 = 4.0;

/// Blocks travelled per movement packet.
///
/// Roughly one tick of walking. Small enough that the server has no reason to
/// call it too fast, large enough to clear the no-movement epsilon.
const STEP: f64 = 0.2;

/// Movement packets sent, so the walk covers 320 blocks — well past the
/// default 10-chunk view distance in one direction, which is what forces both
/// new columns ahead and unloads behind.
const STEPS: i32 = 1600;

/// Ceiling on how many times the server may re-send its own position.
///
/// A player whose movement is accepted sees the join teleport and nothing
/// more; a held one saw 65 to 70 over this same walk. Ten leaves room for an
/// unlucky extra correction without coming close to the failing arm.
const MAX_SERVER_CORRECTIONS: usize = 10;

#[tokio::test]
#[ignore = "needs the 1.7.10 live oracle from scripts/live-oracles/legacy.sh"]
async fn a_real_server_accepts_this_clients_walk_and_unloads_behind_it() {
    use lodestone_net::Connection;
    use lodestone_testsupport::unique_username;

    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: GAME_PORT,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: uuid::Uuid::new_v4(),
    };
    let adapter = lodestone_v1_7::adapter_for(lodestone_v1_7::PROTOCOL);
    let mut world = World::new();

    let mut conn = Connection::connect(("127.0.0.1", GAME_PORT))
        .await
        .unwrap_or_else(|err| {
            panic!(
                "connect to the {MINECRAFT} oracle on :{GAME_PORT} ({err}) -- start it with \
                 ./scripts/live-oracles/legacy.sh {MINECRAFT}"
            )
        });

    let mut state = ConnectionState::Handshaking;
    for directive in adapter.begin_login(&profile, &server).expect("begin login") {
        match directive {
            Directive::Send { packet_id, payload } => {
                conn.write_packet(packet_id, &payload)
                    .await
                    .expect("write packet");
            }
            Directive::SetState(next) => state = next,
            _ => {}
        }
    }

    let mut spawn: Option<Vec3> = None;
    let mut corrections = 0usize;
    let mut loaded = 0usize;
    let mut unloaded = 0usize;
    let mut steps = 0i32;
    let started = Instant::now();

    let _ = tokio::time::timeout(Duration::from_secs(240), async {
        loop {
            if steps >= STEPS || started.elapsed() > Duration::from_secs(220) {
                break;
            }
            let read = tokio::time::timeout(Duration::from_millis(50), conn.read_packet()).await;
            match read {
                // A read timeout is the normal case: the walk is driven below,
                // not by incoming traffic.
                Err(_) => {}
                Ok(Ok(None)) => break,
                Ok(Err(err)) => panic!("read error: {err}"),
                Ok(Ok(Some((packet_id, payload)))) => {
                    let directives = match adapter.handle_packet(&mut world, state, packet_id, &payload)
                    {
                        Ok(directives) => directives,
                        // An untranslated packet is not what this gate is
                        // about; the join replay covers decoding.
                        Err(_) => continue,
                    };
                    for directive in directives {
                        match &directive {
                            Directive::Emit(ClientEvent::TeleportPlayer { pos, .. }) => {
                                corrections += 1;
                                if spawn.is_none() {
                                    spawn = Some(*pos);
                                }
                            }
                            Directive::Emit(ClientEvent::ChunkLoaded { .. }) => loaded += 1,
                            Directive::Emit(ClientEvent::ChunkUnloaded { .. }) => unloaded += 1,
                            Directive::Emit(ClientEvent::KeepAlive { id }) => {
                                if let Ok(Some((id, body))) = adapter.encode_action(
                                    ConnectionState::Play,
                                    &ClientAction::KeepAliveResponse { id: *id },
                                ) {
                                    conn.write_packet(id, &body).await.expect("keep-alive ack");
                                }
                            }
                            Directive::Send { packet_id, payload } => {
                                conn.write_packet(*packet_id, payload)
                                    .await
                                    .expect("write packet");
                            }
                            Directive::SetState(next) => state = *next,
                            _ => {}
                        }
                    }
                }
            }

            // Walk east along the surface, one step per loop.
            if let Some(origin) = spawn {
                steps += 1;
                let pos = Vec3::new(origin.x + f64::from(steps) * STEP, SURFACE_Y, origin.z);
                if let Some((packet_id, body)) = adapter
                    .encode_action(
                        ConnectionState::Play,
                        &ClientAction::Move {
                            pos,
                            rotation: Rotation { yaw: 90.0, pitch: 0.0 },
                            on_ground: true,
                            horizontal_collision: false,
                        },
                    )
                    .expect("encode a move")
                {
                    conn.write_packet(packet_id, &body).await.expect("move");
                }
            }
        }
    })
    .await;

    assert!(spawn.is_some(), "the walk never reached the play state");
    assert_eq!(steps, STEPS, "the walk did not finish inside its window");
    eprintln!(
        "walked {} blocks: {corrections} server correction(s), {loaded} column(s) loaded, \
         {unloaded} unloaded",
        f64::from(STEPS) * STEP
    );

    assert!(
        corrections <= MAX_SERVER_CORRECTIONS,
        "the server corrected our position {corrections} times, which is the signature of a \
         player it is holding rather than one it is moving"
    );
    // The join burst alone is a few hundred columns, so this threshold does
    // not by itself prove travel; the unload count below is what does.
    assert!(
        loaded > 400,
        "only {loaded} column(s) loaded, so the world was never streamed at all"
    );
    assert!(
        unloaded > 0,
        "no column was ever unloaded, so the server never saw the player leave one behind -- \
         which is what an ignored movement packet looks like from here"
    );
}
