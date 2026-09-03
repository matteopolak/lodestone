//! The real-join capture for protocol 5, and the hermetic replay that
//! consumes it.
//!
//! # What this is
//!
//! Two halves that never run together:
//!
//! * a **recorder**, `#[ignore]`d, that joins a real server started by
//!   `scripts/live-oracles/legacy.sh 1.7.10`, records every clientbound packet
//!   it receives to `tests/captures/join_1_7_10.txt`, and commits nothing
//!   itself;
//! * a **replay** that runs in the default `cargo test`, reads the committed
//!   capture, and drives every recorded packet through the real adapter.
//!
//! # Why the split matters here in particular
//!
//! This era's packet shapes came from `minecraft-data` — cross-check grade in
//! this repo, never an authority — and protocol 5 predates Mojang's own data
//! generator by six years, so there is no first-party dump to fall back on. A
//! capture is the only authority available: bytes a real server actually sent.
//! `decode(encode(x)) == x` cannot tell a correct port from two symmetric
//! misunderstandings, and this era has three places where that failure mode is
//! wide open — the chunk arrays' grouping, the three position widths, and the
//! item stack's gzip-behind-an-`i16` NBT.
//!
//! The replay is deliberately not a byte round-trip. It asserts *values* the
//! capture's own bytes pin down, and the expected values come from outside
//! this crate: the flat oracle world's dimension and game mode are the
//! server's choices, and the floor blocks are read back through
//! `lodestone-data`'s jar-derived 26.2 registry.
//!
//! # Recording
//!
//! ```text
//! ./scripts/live-oracles/legacy.sh 1.7.10
//! cargo test -p lodestone-v1-7 --test capture_join -- --ignored --nocapture record_1_7_10
//! ```

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestone_data::block_states;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, GameMode, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_world::World;

/// Minecraft version the committed capture was recorded against.
const MINECRAFT: &str = "1.7.10";

/// Game port `scripts/live-oracles/legacy.sh` gives this version. That script
/// is the single place the port is defined and this is the single place it is
/// read.
const GAME_PORT: u16 = 25600;

/// One clientbound packet as recorded off the wire.
struct CapturedPacket {
    /// Connection state the client was in when this packet arrived.
    state: ConnectionState,
    /// Raw packet id, as this protocol's own table numbers it.
    id: i32,
    /// Packet body, without the id varint.
    payload: Vec<u8>,
}

fn captures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("captures")
}

fn capture_path() -> PathBuf {
    captures_dir().join(format!("join_{}.txt", MINECRAFT.replace('.', "_")))
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

/// Reads the committed capture.
fn read_capture() -> Vec<CapturedPacket> {
    let path = capture_path();
    let text = std::fs::read_to_string(&path)
        .unwrap_or_else(|err| panic!("read {}: {err}", path.display()));
    let mut packets = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
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
        packets.push(CapturedPacket { state, id, payload });
    }
    packets
}

/// The canonical 26.2 state id for a named block with the given properties.
///
/// Resolved out of `lodestone_data::block_states` — jar-derived, and nothing
/// to do with this crate's own tables — so an expected value below originates
/// outside the code under test on both sides: the *bytes* come from a real
/// server and the *meaning* from Mojang's own 26.2 registry.
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
        .unwrap_or_else(|| panic!("the 26.2 registry has no {name} with {properties:?}"))
}

// ---------------------------------------------------------------------------
// Replay — hermetic, runs in the default `cargo test`.
// ---------------------------------------------------------------------------

/// What replaying the capture through the adapter produced.
struct ReplayOutcome {
    events: Vec<ClientEvent>,
    errors: Vec<String>,
    packets: usize,
    world: World,
}

fn replay() -> ReplayOutcome {
    let adapter = lodestone_v1_7::adapter_for(lodestone_v1_7::PROTOCOL);
    let mut world = World::new();
    let mut events = Vec::new();
    let mut errors = Vec::new();
    let packets = read_capture();
    let count = packets.len();

    for packet in packets {
        match adapter.handle_packet(&mut world, packet.state, packet.id, &packet.payload) {
            Ok(directives) => {
                for directive in directives {
                    if let Directive::Emit(event) = directive {
                        events.push(event);
                    }
                }
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

#[test]
fn the_recorded_join_replays_without_a_decode_error() {
    let outcome = replay();
    assert!(
        outcome.errors.is_empty(),
        "the protocol 5 replay produced decode errors: {:?}",
        outcome.errors
    );
    assert!(
        outcome.packets >= 20,
        "the capture is too short to be a real join ({} packets)",
        outcome.packets
    );
}

#[test]
fn the_join_packet_carries_the_dimension_and_mode_the_server_chose() {
    let outcome = replay();
    let (game_mode, dimension) = outcome
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
        .expect("the capture has no Login event");
    assert_eq!(
        game_mode,
        GameMode::Survival,
        "the oracle world is a default (survival) flat world"
    );
    assert_eq!(
        dimension.to_string(),
        "minecraft:overworld",
        "this era carries the join dimension as a signed *byte*, so 0 must reach the model as \
         the overworld rather than as an invented world name"
    );
}

/// The chunk path is the era's hardest, and this is where a well-formed but
/// wrong decode lands.
#[test]
fn the_bulk_chunk_stream_decodes_into_loaded_columns() {
    let outcome = replay();
    let chunks = outcome
        .events
        .iter()
        .filter(|event| matches!(event, ClientEvent::ChunkLoaded { .. }))
        .count();
    assert!(
        chunks > 0,
        "no column decoded — a bulk packet whose predicted inflated length disagrees with the \
         real one is reported as an error by the other test, so zero here means the recording \
         never reached the terrain"
    );
    // Every bulk packet in the capture carries five columns, so a decoder that
    // read one column and stopped would still pass a bare "> 0" check.
    assert!(
        chunks >= 5,
        "a bulk packet in this era carries several columns behind one zlib stream; {chunks} \
         column(s) means the per-column loop stopped early"
    );
}

/// The flat preset's own floor, read back out of `lodestone-world`.
///
/// This is the assertion that exercises the era's chunk differences at once:
/// whether the block-type, metadata and light arrays are grouped per array
/// rather than per section, whether the composite is assembled with the
/// metadata in the low nibble, and whether the biome footer is consumed. Every
/// one of those going wrong produces a populated but wrong world, not an
/// error.
#[test]
fn the_flat_worlds_floor_reads_back_as_canonical_blocks() {
    let bedrock = canonical_state("minecraft:bedrock", &[]);
    let grass = canonical_state("minecraft:grass_block", &[("snowy", "false")]);
    let dirt = canonical_state("minecraft:dirt", &[]);
    assert_ne!(
        bedrock, grass,
        "the probes must be distinguishable in the canonical space"
    );
    assert_ne!(dirt, grass, "the probes must be distinguishable");

    let outcome = replay();
    let mut checked = 0usize;
    for loaded in outcome.world.values() {
        let column = &loaded.column;
        checked += 1;
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, 0, z) == bedrock)),
            "y=0 is not uniformly canonical bedrock ({bedrock}) — a transposed index, the wrong \
             array order, or the wrong composite assembly"
        );
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, 3, z) == grass)),
            "y=3 is not uniformly canonical grass_block ({grass}) — most likely a section-relative \
             y offset error, which the y=0 check alone cannot see"
        );
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, 1, z) == dirt)),
            "y=1 is not uniformly canonical dirt ({dirt})"
        );
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, 4, z) != grass)),
            "y=4 is grass, so the floor was written one layer too high"
        );
    }
    assert!(
        checked > 0,
        "the replay loaded no column into the world at all"
    );
}

/// The player-list packet carries no UUID, so the adapter derives one. This
/// pins that derivation against the UUID the *server* independently computed
/// for the same name and sent in its own login-success packet — two different
/// packets in the same recording, so neither side of the comparison comes from
/// this crate.
#[test]
fn the_derived_player_uuid_matches_the_servers_own_login_success() {
    use lodestone_core::{Ctx, Decode, Reader};

    let packets = read_capture();
    let success = packets
        .iter()
        .find(|packet| {
            packet.state == ConnectionState::Login
                && packet.id == lodestone_v1_7::packet_ids::login::clientbound::SUCCESS
        })
        .expect("the capture has no login success packet");
    let mut reader = Reader::new(&success.payload);
    let profile = lodestone_v1_7::packets::login::LoginSuccess::decode(
        &mut reader,
        Ctx {
            version: lodestone_v1_7::PROTOCOL,
        },
    )
    .expect("login success decodes at protocol 5");

    let server_uuid: uuid::Uuid = profile.uuid.parse().expect("the server sent a dashed UUID");
    assert_eq!(
        lodestone_v1_7::adapter::offline_player_uuid(&profile.username),
        server_uuid,
        "the adapter's offline-mode derivation for {:?} disagrees with the one the server itself \
         computed and sent; every player-list entry in this era is keyed on that derivation",
        profile.username
    );
}

// ---------------------------------------------------------------------------
// Recorder — `#[ignore]`d; needs a live server.
// ---------------------------------------------------------------------------

/// How many bodies of any one packet id a capture keeps.
///
/// A join sends hundreds of block changes and columns; the hundredth adds
/// nothing a reviewer or a replay can use, and a multi-megabyte committed file
/// is a burden on every later checkout. Every *distinct* id the wire produced
/// is still represented, which is the property the capture is evidence for.
const MAX_BODIES_PER_ID: usize = 3;

/// A chunk packet is two orders of magnitude larger than any other here, so it
/// gets its own tighter cap.
const MAX_CHUNK_BODIES: usize = 2;

/// Drives one real join and writes the capture.
///
/// Records every packet id the wire produced, including the ones this family
/// does not translate: a capture is evidence about the wire, and trimming it to
/// the packets already handled would make it agree with the port by
/// construction. The replay tolerates untranslated packets because the
/// dispatch table returns no directives for an `IGNORED` id.
#[tokio::test]
#[ignore = "needs the 1.7.10 live oracle from scripts/live-oracles/legacy.sh"]
async fn record_1_7_10() {
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
            // There is no compression packet at protocol 5, so unlike a later
            // era this loop never sees a threshold to install.
            _ => {}
        }
    }

    let chunk_ids = [
        lodestone_v1_7::packet_ids::play::clientbound::MAP_CHUNK,
        lodestone_v1_7::packet_ids::play::clientbound::MAP_CHUNK_BULK,
    ];
    let mut recorded: Vec<CapturedPacket> = Vec::new();
    let mut seen_per_id: std::collections::BTreeMap<(&str, i32), usize> =
        std::collections::BTreeMap::new();
    let mut reached_play = false;
    let mut chunks = 0usize;
    let mut keep_alives = 0usize;
    let mut health = 0usize;

    let overall = Duration::from_secs(120);
    let read_timeout = Duration::from_secs(8);
    let started = Instant::now();

    let _ = tokio::time::timeout(overall, async {
        loop {
            let done = reached_play && chunks > 0 && keep_alives > 0 && health > 0;
            if done || started.elapsed() > Duration::from_secs(110) {
                break;
            }
            let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
            let (packet_id, payload) = match read {
                Err(_) | Ok(Ok(None)) => break,
                Ok(Ok(Some(packet))) => packet,
                Ok(Err(err)) => panic!("read error: {err}"),
            };
            let seen = seen_per_id.entry((state_name(state), packet_id)).or_default();
            let cap = if state == ConnectionState::Play && chunk_ids.contains(&packet_id) {
                MAX_CHUNK_BODIES
            } else {
                MAX_BODIES_PER_ID
            };
            if *seen < cap {
                *seen += 1;
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
                            Directive::SetState(next) => {
                                state = *next;
                                reached_play |= *next == ConnectionState::Play;
                            }
                            _ => {}
                        }
                    }
                }
                // A packet this family does not translate is still recorded
                // above; a decode error here is information for the operator,
                // not a reason to abandon the recording.
                Err(err) => eprintln!("note: id {packet_id} did not translate: {err}"),
            }
        }
    })
    .await;

    assert!(reached_play, "the recording never reached the play state");
    assert!(chunks > 0, "the recording captured no chunk column");

    let mut out = String::new();
    out.push_str("# lodestone clientbound join capture -- Minecraft 1.7.10 (protocol 5)\n");
    out.push_str(
        "# recorded against ./scripts/live-oracles/legacy.sh 1.7.10, flat overworld\n",
    );
    out.push_str("# real server bytes; the outside oracle for this protocol's ids and shapes.\n");
    out.push_str("# <state> <packet id> <body, hex>\n");
    for packet in &recorded {
        let _ = writeln!(
            out,
            "{} {} {}",
            state_name(packet.state),
            packet.id,
            to_hex(&packet.payload)
        );
    }
    let path = capture_path();
    std::fs::write(&path, out).unwrap_or_else(|err| panic!("write {}: {err}", path.display()));
    eprintln!(
        "wrote {} packets to {}; review and commit it",
        recorded.len(),
        path.display()
    );
}
