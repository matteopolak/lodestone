//! Real-join captures for the three protocols this era gained, and the
//! hermetic replay that consumes them.
//!
//! # What this is
//!
//! Two halves that never run together:
//!
//! * a **recorder**, `#[ignore]`d, that joins a real vanilla server started by
//!   `scripts/live-oracles/legacy.sh <version>`, records every clientbound
//!   packet it receives (state, id, body) to `tests/captures/join_<version>.txt`,
//!   and commits nothing itself;
//! * a **replay** test per protocol that runs in the default `cargo test`,
//!   reads the committed capture, and drives every recorded packet through the
//!   real adapter for that protocol.
//!
//! # Why the split matters
//!
//! The three protocols added here have no Mojang data generator (that arrives
//! in 1.13) and no decompiled source on this host, so their packet ids and
//! shapes came from `minecraft-data` — a cross-check-grade source, not an
//! authority. A capture is the authority: bytes a real 1.9.4 server actually
//! sent. `decode(encode(x)) == x` cannot distinguish a correct port from two
//! symmetric misunderstandings; a recorded body can.
//!
//! The replay is deliberately not a byte round-trip. It asserts *values* the
//! capture's own bytes pin down — the join packet's dimension and game mode,
//! the keep-alive id width, the quantised sound pitch — because those are the
//! places where a wrong-but-well-formed decode lands.
//!
//! # Recording
//!
//! ```text
//! ./scripts/live-oracles/legacy.sh 1.9.4
//! cargo test -p lodestone-v1-9 --test capture_join -- --ignored --nocapture record_1_9_4
//! ```
//!
//! Repeat for `1.10.2` and `1.11.2`. Each brings its own container up on its
//! own port, so all three can be recorded in any order.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestone_model::{
    ClientEvent, ConnectionState, Directive, GameMode, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_v1_9::V340Adapter;
use lodestone_world::World;

/// One clientbound packet as recorded off the wire.
struct CapturedPacket {
    /// Connection state the client was in when this packet arrived.
    state: ConnectionState,
    /// Raw packet id, as the protocol's own table numbers it.
    id: i32,
    /// Decompressed packet body, without the id varint.
    payload: Vec<u8>,
}

/// One era member: its Minecraft version, protocol, and oracle ports.
///
/// Ports match `scripts/live-oracles/legacy.sh`'s table; that script is the
/// single place they are defined and this is the single place they are read.
struct EraMember {
    minecraft: &'static str,
    protocol: i32,
    game_port: u16,
    rcon_port: u16,
}

const MEMBERS: &[EraMember] = &[
    EraMember {
        minecraft: "1.9.4",
        protocol: 110,
        game_port: 25580,
        rcon_port: 25581,
    },
    EraMember {
        minecraft: "1.10.2",
        protocol: 210,
        game_port: 25582,
        rcon_port: 25583,
    },
    EraMember {
        minecraft: "1.11.2",
        protocol: 316,
        game_port: 25584,
        rcon_port: 25585,
    },
];

/// Pitch values the recorder asks the server to play, chosen so the
/// quantisation scale is identifiable from the resulting bytes rather than
/// assumed: at scale 63 they land on 94 and 31, at 62 on 93 and 31, and at 64
/// on 96 and 32 — so the *pair* separates all three, which neither value does
/// alone. Both are inside vanilla's `0.5..=2.0` clamp.
const RECORDED_PITCHES: [f32; 2] = [1.5, 0.5];

fn captures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/captures")
}

fn capture_path(minecraft: &str) -> PathBuf {
    captures_dir().join(format!("join_{}.txt", minecraft.replace('.', "_")))
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

/// Reads a committed capture.
fn read_capture(minecraft: &str) -> Vec<CapturedPacket> {
    let path = capture_path(minecraft);
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

// ---------------------------------------------------------------------------
// Replay — hermetic, runs in the default `cargo test`.
// ---------------------------------------------------------------------------

/// What replaying one capture through its own adapter produced.
struct ReplayOutcome {
    events: Vec<ClientEvent>,
    errors: Vec<String>,
    packets: usize,
}

fn replay(member: &EraMember) -> ReplayOutcome {
    let adapter = lodestone_v1_9::adapter_for(member.protocol);
    let mut world = World::new();
    let mut events = Vec::new();
    let mut errors = Vec::new();
    let packets = read_capture(member.minecraft);
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
    }
}

/// The shared body of the three replay tests.
///
/// Every assertion here is against a value the *server* chose, recovered from
/// bytes it sent: the flat oracle world's overworld dimension and its default
/// survival game mode, the presence of real chunk columns, and a keep-alive id
/// whose width differs across this era. A decode that is well-formed but wrong
/// fails these; a byte round-trip would not.
fn assert_capture_replays_cleanly(member: &EraMember) {
    let outcome = replay(member);

    assert!(
        outcome.errors.is_empty(),
        "protocol {} replay produced decode errors: {:?}",
        member.protocol,
        outcome.errors
    );
    assert!(
        outcome.packets >= 20,
        "protocol {} capture is too short to be a real join ({} packets)",
        member.protocol,
        outcome.packets
    );

    let login = outcome
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
        .unwrap_or_else(|| panic!("protocol {} capture has no Login event", member.protocol));
    assert_eq!(
        login.0,
        GameMode::Survival,
        "the oracle world is a default (survival) flat world"
    );
    assert_eq!(
        login.1.to_string(),
        "minecraft:overworld",
        "the oracle spawn is in the overworld"
    );

    let chunks = outcome
        .events
        .iter()
        .filter(|event| matches!(event, ClientEvent::ChunkLoaded { .. }))
        .count();
    assert!(
        chunks > 0,
        "protocol {} capture decoded no chunk columns — a map_chunk that fails \
         `ensure_empty` is reported as an error above, so zero here means the \
         recording never reached Play",
        member.protocol
    );

    let keep_alives: Vec<i64> = outcome
        .events
        .iter()
        .filter_map(|event| match event {
            ClientEvent::KeepAlive { id } => Some(*id),
            _ => None,
        })
        .collect();
    assert!(
        !keep_alives.is_empty(),
        "protocol {} capture has no keep_alive — the id is a VarInt in this era \
         and an i64 from 1.12, so this is the packet a shared struct would \
         misread",
        member.protocol
    );
}

#[test]
fn committed_1_9_4_capture_replays_through_the_110_adapter() {
    assert_capture_replays_cleanly(&MEMBERS[0]);
}

#[test]
fn committed_1_10_2_capture_replays_through_the_210_adapter() {
    assert_capture_replays_cleanly(&MEMBERS[1]);
}

#[test]
fn committed_1_11_2_capture_replays_through_the_316_adapter() {
    assert_capture_replays_cleanly(&MEMBERS[2]);
}

/// Negative control for all three replays: feed 1.9.4's captured bytes to the
/// **340** adapter, which is the mistake this whole era merge exists to make
/// impossible. It must not quietly produce the same join.
///
/// This is the check that proves the three tests above are load-bearing rather
/// than passing by construction.
#[test]
fn the_1_9_4_capture_does_not_replay_as_1_12_2() {
    let adapter = V340Adapter::new();
    assert_eq!(adapter.protocol_version(), 340);
    let mut world = World::new();
    let mut login_events = 0usize;
    let mut errors = 0usize;

    for packet in read_capture("1.9.4") {
        match adapter.handle_packet(&mut world, packet.state, packet.id, &packet.payload) {
            Ok(directives) => {
                login_events += directives
                    .iter()
                    .filter(|directive| {
                        matches!(directive, Directive::Emit(ClientEvent::Login { .. }))
                    })
                    .count();
            }
            Err(_) => errors += 1,
        }
    }

    assert!(
        login_events == 0 || errors > 0,
        "the 340 adapter read 1.9.4's capture as a clean join ({login_events} Login \
         events, {errors} errors) — then the two protocols' tables do not \
         actually differ and this era merge is untested"
    );
}

/// The quantised sound pitch, measured rather than recalled.
///
/// 1.9.4 carries a sound packet's pitch as one byte; 1.10 widened it to a
/// float. The capture holds two sound packets the recorder asked for at known
/// multipliers, so the scale is a property of bytes the server wrote.
///
/// The assertion is that the decoded multiplier recovers the commanded one to
/// within a single quantisation step. That is what separates the candidate
/// scales: at 63 the 1.5 request decodes to 1.492 (0.008 off, inside one step
/// of 0.0159); at 64 it decodes to 1.469 (0.031 off, outside).
#[test]
fn the_1_9_4_capture_pins_the_legacy_sound_pitch_scale() {
    let outcome = replay(&MEMBERS[0]);
    let pitches: Vec<f32> = outcome
        .events
        .iter()
        .filter_map(|event| match event {
            ClientEvent::Sound { pitch, .. } => Some(*pitch),
            _ => None,
        })
        .collect();

    assert!(
        pitches.len() >= RECORDED_PITCHES.len(),
        "the 1.9.4 capture must carry one sound packet per recorded pitch; got {pitches:?}"
    );

    let step = 1.0 / lodestone_v1_9::packets::game::LEGACY_SOUND_PITCH_SCALE;
    for commanded in RECORDED_PITCHES {
        let closest = pitches
            .iter()
            .copied()
            .min_by(|a, b| {
                (a - commanded)
                    .abs()
                    .partial_cmp(&(b - commanded).abs())
                    .expect("pitches are finite")
            })
            .expect("at least one sound pitch");
        assert!(
            (closest - commanded).abs() <= step,
            "commanded pitch {commanded} decoded as {closest}, which is more than one \
             quantisation step ({step}) away — the byte scale is not \
             {}",
            lodestone_v1_9::packets::game::LEGACY_SOUND_PITCH_SCALE
        );
    }
}

/// The differential that makes the pitch split real rather than asserted.
///
/// The same RCON command, at the same two multipliers, against all three
/// oracles. 1.9.4 must come back *quantised* — never exactly 1.5, because one
/// byte cannot hold it — and 1.10.2 and 1.11.2 must come back **exact**,
/// because they carry a float. A single shared struct cannot satisfy both
/// halves: reading the 1.9.4 body as a float consumes bytes past the end of
/// the packet, and reading the 1.10.2 body as a byte leaves three behind.
#[test]
fn the_captures_separate_the_byte_pitch_era_from_the_float_pitch_era() {
    let commanded = RECORDED_PITCHES[0];

    let quantised = replay(&MEMBERS[0])
        .events
        .iter()
        .filter_map(|event| match event {
            ClientEvent::Sound { pitch, .. } => Some(*pitch),
            _ => None,
        })
        .find(|pitch| (pitch - commanded).abs() < 0.1)
        .expect("the 1.9.4 capture carries the 1.5 sound");
    assert!(
        (quantised - commanded).abs() > f32::EPSILON,
        "1.9.4's pitch byte cannot represent {commanded} exactly, yet the replay \
         produced it — the byte path is not being taken"
    );

    for member in &MEMBERS[1..] {
        let exact = replay(member)
            .events
            .iter()
            .filter_map(|event| match event {
                ClientEvent::Sound { pitch, .. } => Some(*pitch),
                _ => None,
            })
            .find(|pitch| (pitch - commanded).abs() < 0.1)
            .unwrap_or_else(|| {
                panic!("the {} capture carries the 1.5 sound", member.minecraft)
            });
        assert_eq!(
            exact, commanded,
            "protocol {} carries the pitch as a float, so it must round-trip \
             exactly",
            member.protocol
        );
    }
}

// ---------------------------------------------------------------------------
// Recorder — `#[ignore]`d; needs a live server.
// ---------------------------------------------------------------------------

/// How many bodies of any one packet id a capture keeps.
///
/// A join sends thousands of `rel_entity_move`s and hundreds of columns; the
/// hundredth adds nothing a reviewer or a replay can use, and a multi-megabyte
/// committed file is a burden on every later checkout. Every *distinct* id the
/// wire produced is still represented, which is the property the capture is
/// evidence for.
const MAX_BODIES_PER_ID: usize = 3;

/// `map_chunk` is two orders of magnitude larger than any other packet here,
/// so it gets its own, tighter cap. One real column is enough to exercise the
/// paletted decode and the `ensure_empty` trailing-byte check.
const MAX_CHUNK_BODIES: usize = 1;

/// Drives one real join and writes the capture.
///
/// Records every packet id the wire produced, including the ones this family
/// does not translate: a capture is evidence about the wire, and trimming it
/// to the packets we already handle would make it agree with the port by
/// construction. The replay above tolerates untranslated packets because the
/// adapter's dispatch table returns no directives for an `IGNORED` id.
async fn record(member: &EraMember) {
    use lodestone_net::Connection;
    use lodestone_testsupport::{RconClient, unique_username};
    use std::time::Instant;

    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: member.game_port,
    };
    let username = unique_username();
    let profile = LoginProfile {
        username: username.clone(),
        uuid: uuid::Uuid::new_v4(),
    };
    let adapter = lodestone_v1_9::adapter_for(member.protocol);
    let mut world = World::new();

    let mut conn = Connection::connect(("127.0.0.1", member.game_port))
        .await
        .unwrap_or_else(|err| {
            panic!(
                "connect to the {} oracle on :{} ({err}) -- start it with \
                 ./scripts/live-oracles/legacy.sh {}",
                member.minecraft, member.game_port, member.minecraft
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
            Directive::SetCompression(threshold) => conn.set_compression(threshold),
            _ => {}
        }
    }

    let mut recorded: Vec<CapturedPacket> = Vec::new();
    let mut seen_per_id: std::collections::BTreeMap<i32, usize> = std::collections::BTreeMap::new();
    let mut player_pos: Option<lodestone_model::Vec3> = None;
    let mut reached_play = false;
    let mut asked_for_sounds = false;
    let mut sounds_seen = 0usize;
    let mut chunks = 0usize;
    let mut keep_alives = 0usize;

    let overall = Duration::from_secs(90);
    let read_timeout = Duration::from_secs(8);
    let started = Instant::now();

    let _ = tokio::time::timeout(overall, async {
        loop {
            let done = reached_play
                && chunks > 0
                && keep_alives > 0
                && sounds_seen >= RECORDED_PITCHES.len();
            if done || started.elapsed() > Duration::from_secs(80) {
                break;
            }
            let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
            let (packet_id, payload) = match read {
                Err(_) | Ok(Ok(None)) => break,
                Ok(Ok(Some(packet))) => packet,
                Ok(Err(err)) => panic!("read error: {err}"),
            };
            let seen = seen_per_id.entry(packet_id).or_default();
            let cap = if state == ConnectionState::Play
                && packet_id == adapter_map_chunk_id(member.protocol)
            {
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
                            Directive::Emit(ClientEvent::TeleportPlayer { pos, .. }) => {
                                player_pos = Some(*pos);
                            }
                            Directive::Emit(ClientEvent::Sound { .. }) => sounds_seen += 1,
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
                            Directive::SetCompression(threshold) => {
                                conn.set_compression(*threshold);
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

            if let Some(pos) = player_pos
                && reached_play
                && chunks > 0
                && !asked_for_sounds
            {
                asked_for_sounds = true;
                // The coordinates must be the *player's*, not the console's:
                // a sound played at the world origin is out of audible range
                // of a spawn that is not there, and the server then sends no
                // packet at all. That is why the first recording of this
                // capture came back with zero sounds.
                let mut rcon = RconClient::connect(("127.0.0.1", member.rcon_port), "lodestone")
                    .expect("connect RCON");
                for pitch in RECORDED_PITCHES {
                    let response = rcon
                        .command(&format!(
                            "playsound minecraft:entity.experience_orb.pickup master \
                             {username} {} {} {} 1 {pitch}",
                            pos.x, pos.y, pos.z
                        ))
                        .expect("rcon playsound");
                    eprintln!("playsound {pitch}: {}", response.trim());
                }
            }
        }
    })
    .await;

    assert!(reached_play, "never reached Play against {}", member.minecraft);
    assert!(chunks > 0, "no chunks decoded from {}", member.minecraft);

    let mut out = String::new();
    let _ = writeln!(
        out,
        "# lodestone clientbound join capture -- Minecraft {} (protocol {})",
        member.minecraft, member.protocol
    );
    let _ = writeln!(
        out,
        "# recorded by tests/capture_join.rs against ./scripts/live-oracles/legacy.sh {}",
        member.minecraft
    );
    let _ = writeln!(
        out,
        "# real server bytes; the outside oracle for this protocol's ids and shapes."
    );
    let _ = writeln!(out, "# <state> <packet id> <body, hex>");
    for packet in &recorded {
        let _ = writeln!(
            out,
            "{} {} {}",
            state_name(packet.state),
            packet.id,
            to_hex(&packet.payload)
        );
    }
    let path = capture_path(member.minecraft);
    std::fs::create_dir_all(captures_dir()).expect("create captures dir");
    std::fs::write(&path, out).expect("write capture");
    eprintln!(
        "wrote {} ({} packets, {chunks} chunk columns, {keep_alives} keep-alives, \
         {sounds_seen} sounds)",
        path.display(),
        recorded.len()
    );
}

/// `map_chunk`'s id in one protocol's table, for the size cap above.
///
/// Read from the generated tables rather than written down: the id is 32 in
/// every protocol in this era, but that is a fact about the tables, not a
/// licence to hardcode it here.
fn adapter_map_chunk_id(protocol: i32) -> i32 {
    let entries = match protocol {
        110 => lodestone_v1_9::packet_ids_110::play::clientbound::ENTRIES,
        210 => lodestone_v1_9::packet_ids_210::play::clientbound::ENTRIES,
        316 => lodestone_v1_9::packet_ids_316::play::clientbound::ENTRIES,
        _ => lodestone_v1_9::packet_ids::play::clientbound::ENTRIES,
    };
    entries
        .iter()
        .find(|(name, _)| *name == "minecraft:map_chunk")
        .map(|(_, id)| *id)
        .expect("every protocol in this era carries map_chunk")
}

#[tokio::test]
#[ignore = "records against a live 1.9.4 server: ./scripts/live-oracles/legacy.sh 1.9.4"]
async fn record_1_9_4() {
    record(&MEMBERS[0]).await;
}

#[tokio::test]
#[ignore = "records against a live 1.10.2 server: ./scripts/live-oracles/legacy.sh 1.10.2"]
async fn record_1_10_2() {
    record(&MEMBERS[1]).await;
}

#[tokio::test]
#[ignore = "records against a live 1.11.2 server: ./scripts/live-oracles/legacy.sh 1.11.2"]
async fn record_1_11_2() {
    record(&MEMBERS[2]).await;
}
