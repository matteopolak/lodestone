//! A real 1.8.9 join capture and its hermetic replay.
//!
//! The ignored recorder talks to the vanilla 1.8.9 oracle started by
//! `scripts/live-oracles/legacy.sh`.  The committed replay keeps the wire
//! bytes as the authority for packet ids, chunk framing, and field order;
//! the tests assert server-chosen values rather than merely round-tripping
//! Lodestone's own codecs.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestone_data::block_states;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, GameMode, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_v1_8::packet_ids::play;
use lodestone_v1_8::{PROTOCOL, adapter_for};
use lodestone_world::{ChunkPos, World};

const MINECRAFT: &str = "1.8.9";
const GAME_PORT: u16 = 25566;

struct CapturedPacket {
    state: ConnectionState,
    id: i32,
    payload: Vec<u8>,
}

struct ReplayOutcome {
    events: Vec<ClientEvent>,
    errors: Vec<String>,
    packets: usize,
    world: World,
}

fn captures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/captures")
}

fn capture_path() -> PathBuf {
    captures_dir().join("join_1_8_9.txt")
}

fn state_name(state: ConnectionState) -> &'static str {
    match state {
        ConnectionState::Handshaking => "handshaking",
        ConnectionState::Status => "status",
        ConnectionState::Login => "login",
        ConnectionState::Configuration => "configuration",
        ConnectionState::Play => "play",
    }
}

fn state_from_name(name: &str) -> ConnectionState {
    match name {
        "login" => ConnectionState::Login,
        "play" => ConnectionState::Play,
        other => panic!("capture names an unexpected state {other:?}"),
    }
}

fn to_hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        let _ = write!(out, "{byte:02x}");
    }
    out
}

fn from_hex(text: &str) -> Vec<u8> {
    assert!(text.len() % 2 == 0, "capture payload has an odd hex length");
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&text[i..i + 2], 16).expect("capture payload is hex"))
        .collect()
}

fn parse_capture(text: &str) -> Vec<CapturedPacket> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut parts = line.split(' ');
            let state = state_from_name(parts.next().expect("capture line has a state"));
            let id: i32 = parts
                .next()
                .expect("capture line has an id")
                .parse()
                .expect("capture id is an integer");
            let payload = from_hex(parts.next().unwrap_or(""));
            assert!(parts.next().is_none(), "capture line has trailing fields");
            Some(CapturedPacket { state, id, payload })
        })
        .collect()
}

fn read_capture() -> Vec<CapturedPacket> {
    let path = capture_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    parse_capture(&text)
}

fn first_map_chunk_pos(packets: &[CapturedPacket]) -> ChunkPos {
    let packet = packets
        .iter()
        .find(|packet| {
            packet.state == ConnectionState::Play && packet.id == play::clientbound::MAP_CHUNK
        })
        .expect("capture has no data-bearing map_chunk");
    let x = i32::from_be_bytes(packet.payload[0..4].try_into().expect("map_chunk x"));
    let z = i32::from_be_bytes(packet.payload[4..8].try_into().expect("map_chunk z"));
    ChunkPos::new(x, z)
}

fn replay_capture(packets: Vec<CapturedPacket>) -> ReplayOutcome {
    let adapter = adapter_for(PROTOCOL);
    let mut world = World::new();
    let mut events = Vec::new();
    let mut errors = Vec::new();
    let count = packets.len();

    for packet in packets {
        match adapter.handle_packet(&mut world, packet.state, packet.id, &packet.payload) {
            Ok(directives) => {
                events.extend(
                    directives
                        .into_iter()
                        .filter_map(|directive| match directive {
                            Directive::Emit(event) => Some(event),
                            _ => None,
                        }),
                )
            }
            Err(err) => errors.push(format!("state {:?} id {}: {err}", packet.state, packet.id)),
        }
    }

    ReplayOutcome {
        events,
        errors,
        packets: count,
        world,
    }
}

fn canonical_state(name: &str, properties: &[(&str, &str)]) -> u32 {
    (0..block_states::STATE_COUNT)
        .find(|&id| {
            block_states::block_name(id) == Some(name)
                && block_states::properties(id).is_some_and(|props| {
                    props.len() == properties.len()
                        && props
                            .iter()
                            .zip(properties.iter())
                            .all(|(a, b)| a.0 == b.0 && a.1 == b.1)
                })
        })
        .unwrap_or_else(|| panic!("26.2 registry has no {name} with {properties:?}"))
}

#[test]
fn the_committed_1_8_9_capture_replays_as_a_clean_join() {
    let outcome = replay_capture(read_capture());
    assert!(
        outcome.errors.is_empty(),
        "protocol 47 replay produced decode errors: {:?}",
        outcome.errors
    );
    assert!(
        outcome.packets >= 20,
        "capture is too short to be a real join"
    );

    let (mode, dimension) = outcome
        .events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Login {
                game_mode,
                dimension,
                ..
            } => Some((*game_mode, dimension.clone())),
            _ => None,
        })
        .expect("capture has no Login event");
    assert_eq!(mode, GameMode::Survival);
    assert_eq!(dimension.to_string(), "minecraft:overworld");
    assert!(
        outcome
            .events
            .iter()
            .any(|event| matches!(event, ClientEvent::ChunkLoaded { .. })),
        "capture decoded no chunk columns"
    );
    assert!(
        outcome
            .events
            .iter()
            .any(|event| matches!(event, ClientEvent::KeepAlive { .. })),
        "capture has no keep_alive"
    );
}

#[test]
fn the_capture_decodes_the_flat_oracles_floor_in_canonical_ids() {
    let bedrock = canonical_state("minecraft:bedrock", &[]);
    let dirt = canonical_state("minecraft:dirt", &[]);
    let grass = canonical_state("minecraft:grass_block", &[("snowy", "false")]);
    let packets = read_capture();
    let primary_pos = first_map_chunk_pos(&packets);
    let outcome = replay_capture(packets);
    // The single `map_chunk` body is the first target in the real capture;
    // assert that exact server-authored column rather than making a claim
    // about later bulk columns whose lifecycle may include partial updates.
    let column = &outcome
        .world
        .get(primary_pos)
        .expect("capture did not load its primary map_chunk")
        .column;
    assert!((0..16).all(|x| (0..16).all(|z| column.get_block(x, 0, z) == bedrock)));
    assert!((0..16).all(|x| (0..16).all(|z| column.get_block(x, 1, z) == dirt)));
    assert!((0..16).all(|x| (0..16).all(|z| column.get_block(x, 3, z) == grass)));
    assert!((0..16).all(|x| (0..16).all(|z| column.get_block(x, 4, z) != grass)));
}

/// The adjacent-family captures are real server bytes already committed by
/// their own replay tests. Feeding either through protocol 47 must not produce
/// a clean join; this guards the family boundary without inventing bytes in a
/// v1.8 test.
#[test]
fn neighbouring_family_captures_do_not_replay_as_protocol_47() {
    let neighbours = [
        (
            "1.7.10",
            include_str!("../../1.7/tests/captures/join_1_7_10.txt"),
        ),
        (
            "1.9.4",
            include_str!("../../1.9/tests/captures/join_1_9_4.txt"),
        ),
    ];

    for (version, text) in neighbours {
        let outcome = replay_capture(parse_capture(text));
        let logins = outcome
            .events
            .iter()
            .filter(|event| matches!(event, ClientEvent::Login { .. }))
            .count();
        let chunks = outcome
            .events
            .iter()
            .filter(|event| matches!(event, ClientEvent::ChunkLoaded { .. }))
            .count();
        assert!(
            !outcome.errors.is_empty() || logins == 0 || chunks == 0,
            "the {version} capture replayed as a clean protocol-47 join"
        );
    }
}

const MAX_BODIES_PER_ID: usize = 3;
const MAX_CHUNK_BODIES: usize = 2;

/// Records every distinct clientbound id the oracle sends, with a small cap
/// per id. The cap keeps the fixture reviewable while retaining the complete
/// id set and enough chunk bodies to exercise the decoder repeatedly.
#[tokio::test]
#[ignore = "needs the 1.8.9 live oracle from scripts/live-oracles/legacy.sh"]
async fn record_1_8_9() {
    use lodestone_net::Connection;
    use lodestone_testsupport::unique_username;
    use std::time::Instant;

    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: GAME_PORT,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: uuid::Uuid::new_v4(),
    };
    let adapter = adapter_for(PROTOCOL);
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

    let chunk_ids = [
        play::clientbound::MAP_CHUNK,
        play::clientbound::MAP_CHUNK_BULK,
    ];
    let mut recorded = Vec::new();
    let mut seen = std::collections::BTreeMap::<(&str, i32), usize>::new();
    let mut reached_play = false;
    let mut chunks = 0;
    let mut keep_alives = 0;
    let mut health = 0;
    let started = Instant::now();

    let _ = tokio::time::timeout(Duration::from_secs(120), async {
        loop {
            if reached_play && chunks > 0 && keep_alives > 0 && health > 0 {
                break;
            }
            if started.elapsed() > Duration::from_secs(110) {
                break;
            }
            let read = tokio::time::timeout(Duration::from_secs(8), conn.read_packet()).await;
            let (packet_id, payload) = match read {
                Err(_) | Ok(Ok(None)) => break,
                Ok(Ok(Some(packet))) => packet,
                Ok(Err(err)) => panic!("read error: {err}"),
            };
            let count = seen.entry((state_name(state), packet_id)).or_default();
            let cap = if state == ConnectionState::Play && chunk_ids.contains(&packet_id) {
                MAX_CHUNK_BODIES
            } else {
                MAX_BODIES_PER_ID
            };
            if *count < cap {
                *count += 1;
                recorded.push(CapturedPacket {
                    state,
                    id: packet_id,
                    payload: payload.clone(),
                });
            }

            match adapter.handle_packet(&mut world, state, packet_id, &payload) {
                Ok(directives) => {
                    for directive in directives {
                        match &directive {
                            Directive::Emit(ClientEvent::ChunkLoaded { .. }) => chunks += 1,
                            Directive::Emit(ClientEvent::HealthChanged { .. }) => health += 1,
                            Directive::Emit(ClientEvent::KeepAlive { id }) => {
                                keep_alives += 1;
                                if let Ok(Some((id, body))) = adapter.encode_action(
                                    ConnectionState::Play,
                                    &lodestone_model::ClientAction::KeepAliveResponse { id: *id },
                                ) {
                                    conn.write_packet(id, &body).await.expect("keep-alive ack");
                                }
                            }
                            Directive::Send { packet_id, payload } => {
                                conn.write_packet(*packet_id, payload)
                                    .await
                                    .expect("write packet");
                            }
                            Directive::SetCompression(threshold) => {
                                conn.set_compression(*threshold)
                            }
                            Directive::SetState(next) => {
                                state = *next;
                                reached_play |= *next == ConnectionState::Play;
                            }
                            _ => {}
                        }
                    }
                }
                Err(err) => eprintln!("note: id {packet_id} did not translate: {err}"),
            }
        }
    })
    .await;

    assert!(reached_play, "recording never reached play");
    assert!(chunks > 0, "recording captured no chunk column");

    let mut out = String::new();
    writeln!(
        out,
        "# lodestone clientbound join capture -- Minecraft {MINECRAFT} (protocol {PROTOCOL})"
    )
    .unwrap();
    writeln!(
        out,
        "# recorded by this test against scripts/live-oracles/legacy.sh {MINECRAFT}, flat overworld"
    )
    .unwrap();
    out.push_str("# real server bytes; the outside oracle for this protocol's ids and shapes.\n");
    out.push_str("# <state> <packet id> <body, hex>\n");
    for packet in &recorded {
        writeln!(
            out,
            "{} {} {}",
            state_name(packet.state),
            packet.id,
            to_hex(&packet.payload)
        )
        .unwrap();
    }
    let path = capture_path();
    std::fs::create_dir_all(captures_dir()).expect("create captures directory");
    std::fs::write(&path, out).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
    eprintln!(
        "wrote {} packets to {}; review and commit it",
        recorded.len(),
        path.display()
    );
}
