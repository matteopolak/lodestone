//! Real-join captures for the two protocols this era gained, and the
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
//! 498's and 578's packet shapes came from `minecraft-data` — a
//! cross-check-grade source, not an authority — and in one load-bearing place
//! it is simply **wrong**: it models 1.14.4's `map_chunk` with no biome field
//! at all, when a full column ends with 256 big-endian ints of them inside
//! the section buffer. A capture is the authority: bytes a real 1.14.4 server
//! actually sent. `decode(encode(x)) == x` cannot distinguish a correct port
//! from two symmetric misunderstandings; a recorded body can, and did.
//!
//! The replay is deliberately not a byte round-trip. It asserts *values* the
//! capture's own bytes pin down — the join packet's dimension and game mode,
//! and the flat world's own floor read back out of `lodestone-world` — because
//! those are the places where a wrong-but-well-formed decode lands.
//!
//! # Recording
//!
//! ```text
//! ./scripts/live-oracles/legacy.sh 1.14.4
//! cargo test -p lodestone-v1-14 --test capture_join -- --ignored --nocapture record_1_14_4
//! ```
//!
//! Repeat for `1.15.2`. Each brings its own container up on its own port, so
//! the two can be recorded in any order.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestone_data::block_states;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, GameMode, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_v1_14::{PROTOCOL_1_14_4, PROTOCOL_1_15_2, PROTOCOL_1_16_5};
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
}

const MEMBERS: &[EraMember] = &[
    EraMember {
        minecraft: "1.14.4",
        protocol: PROTOCOL_1_14_4,
        game_port: 25586,
    },
    EraMember {
        minecraft: "1.15.2",
        protocol: PROTOCOL_1_15_2,
        game_port: 25588,
    },
];

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

/// Resolves a packet name to its id in one protocol's own clientbound table.
///
/// Read from the generated tables rather than written down: every id in this
/// era moves at least once, so a literal here would be a claim about which
/// protocol is being talked about rather than a fact about it.
fn clientbound_id(protocol: i32, name: &str) -> i32 {
    let entries = match protocol {
        PROTOCOL_1_14_4 => lodestone_v1_14::packet_ids_498::play::clientbound::ENTRIES,
        PROTOCOL_1_15_2 => lodestone_v1_14::packet_ids_578::play::clientbound::ENTRIES,
        _ => lodestone_v1_14::packet_ids::play::clientbound::ENTRIES,
    };
    entries
        .iter()
        .find(|(entry, _)| *entry == name)
        .map(|(_, id)| *id)
        .unwrap_or_else(|| panic!("protocol {protocol} carries no {name}"))
}

/// The canonical 26.2 state id for a block with no properties.
///
/// Resolved out of `lodestone_data::block_states` — jar-derived, and nothing
/// to do with this crate's own tables — so an expected value below originates
/// outside the code under test on both sides: the *bytes* come from a real
/// server, the *meaning* from Mojang's own 26.2 registry.
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

// ---------------------------------------------------------------------------
// Replay — hermetic, runs in the default `cargo test`.
// ---------------------------------------------------------------------------

/// What replaying one capture through an adapter produced.
struct ReplayOutcome {
    events: Vec<ClientEvent>,
    errors: Vec<String>,
    packets: usize,
    world: World,
}

fn replay_through(protocol: i32, minecraft: &str) -> ReplayOutcome {
    let adapter = lodestone_v1_14::adapter_for(protocol);
    let mut world = World::new();
    let mut events = Vec::new();
    let mut errors = Vec::new();
    let packets = read_capture(minecraft);
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

fn replay(member: &EraMember) -> ReplayOutcome {
    replay_through(member.protocol, member.minecraft)
}

/// The shared body of the two replay tests.
///
/// Every assertion here is against a value the *server* chose, recovered from
/// bytes it sent: the flat oracle world's overworld dimension, its default
/// survival game mode, and the flat preset's own floor read back out of the
/// world store. A decode that is well-formed but wrong fails these; a byte
/// round-trip would not.
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
        "protocol {} carries the dimension as a signed integer, so `0` must \
         reach the model as the overworld and not as an invented world name",
        member.protocol
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

    let keep_alives = outcome
        .events
        .iter()
        .filter(|event| matches!(event, ClientEvent::KeepAlive { .. }))
        .count();
    assert!(
        keep_alives > 0,
        "protocol {} capture has no keep_alive — that packet's id moves within \
         this era (32 at 498, 33 at 578, 31 at 754), so it is one of the \
         packets a shared id table would silently misroute",
        member.protocol
    );
}

/// The flat preset's own floor, read back out of `lodestone-world`.
///
/// This is the assertion that actually exercises the era's three chunk
/// differences at once — where the biome array sits, whether the section
/// indices straddle a 64-bit boundary, and which block-state table the wire
/// ids are translated through. Every one of those going wrong produces a
/// world that is populated but wrong, not an error.
///
/// The expected values come from the 26.2 registry, not from this crate: a
/// column whose y=0 plane is uniformly canonical bedrock and whose y=3 plane
/// is uniformly canonical grass is a claim about `lodestone-data`'s numbering
/// that only a correct decode *and* a correct canonical table can satisfy.
fn assert_flat_world_floor(member: &EraMember) {
    let bedrock = canonical_state("minecraft:bedrock", &[]);
    let grass = canonical_state("minecraft:grass_block", &[("snowy", "false")]);
    assert_ne!(
        bedrock, grass,
        "the two probes must be distinguishable in the canonical space"
    );

    let outcome = replay(member);
    let mut checked = 0usize;
    for loaded in outcome.world.values() {
        let column = &loaded.column;
        checked += 1;
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, 0, z) == bedrock)),
            "protocol {}: y=0 is not uniformly canonical bedrock ({bedrock}) — a \
             transposed decode, the wrong long packing, or the wrong protocol's \
             block-state table",
            member.protocol
        );
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, 3, z) == grass)),
            "protocol {}: y=3 is not uniformly canonical grass_block ({grass}) — \
             most likely a section-relative Y offset error, which the y=0 check \
             alone cannot see",
            member.protocol
        );
    }
    assert!(
        checked > 0,
        "protocol {} capture put no columns into the world store",
        member.protocol
    );
}

#[test]
fn committed_1_14_4_capture_replays_through_the_498_adapter() {
    assert_capture_replays_cleanly(&MEMBERS[0]);
}

#[test]
fn committed_1_15_2_capture_replays_through_the_578_adapter() {
    assert_capture_replays_cleanly(&MEMBERS[1]);
}

#[test]
fn the_1_14_4_capture_lands_the_flat_preset_floor_in_canonical_ids() {
    assert_flat_world_floor(&MEMBERS[0]);
}

#[test]
fn the_1_15_2_capture_lands_the_flat_preset_floor_in_canonical_ids() {
    assert_flat_world_floor(&MEMBERS[1]);
}

/// **The negative control.** One packet, one id, two protocols.
///
/// `update_health` is clientbound id **72** at 498 and **73** at 754; id 72 at
/// 754 is `experience`. A 754 adapter handed 498's captured `update_health`
/// bytes must not report health.
///
/// Watched failing first, and this is what the failure looked like: before
/// this era merge `adapter_for` ignored its argument and always returned a
/// 754 adapter, so "the 498 adapter" *was* this wrong arm. Replaying the
/// 1.14.4 capture through it produces zero `Login` events, zero chunk columns
/// and ten decode errors — see
/// [`the_1_14_4_capture_does_not_replay_as_1_16_5`], which is the same
/// observation kept as a standing test.
///
/// **What the misroute is not, in this era.** It does not produce a plausible
/// wrong event. `experience` is `f32`, VarInt, VarInt and `update_health` is
/// `f32`, VarInt, `f32`, so the shorter read leaves three bytes over and the
/// adapter's exact decode rejects it. That is a property worth stating rather
/// than assuming: measured across all 28 captured packets, every id that
/// names a different packet at 754 either errors or lands on an ignored id,
/// and none of them emits a wrong gameplay event.
/// [`misrouting_between_protocols_is_never_a_plausible_wrong_event`] holds
/// that line — it is what would catch a future lenient decode turning this
/// era's loud failures into quiet ones.
#[test]
fn update_health_does_not_decode_as_the_other_protocols_packet() {
    let health_id_498 = clientbound_id(PROTOCOL_1_14_4, "minecraft:update_health");
    let health_id_754 = clientbound_id(PROTOCOL_1_16_5, "minecraft:update_health");
    assert_eq!((health_id_498, health_id_754), (72, 73));
    assert_eq!(
        clientbound_id(PROTOCOL_1_16_5, "minecraft:experience"),
        health_id_498,
        "the control only discriminates because 754 puts a *different* packet \
         at 498's update_health id"
    );

    let body = read_capture("1.14.4")
        .into_iter()
        .find(|packet| packet.state == ConnectionState::Play && packet.id == health_id_498)
        .expect("the 1.14.4 capture carries an update_health")
        .payload;

    // Right: the 498 adapter reads its own id as health.
    let right = lodestone_v1_14::adapter_for(PROTOCOL_1_14_4);
    let mut world = World::new();
    let events = right
        .handle_packet(&mut world, ConnectionState::Play, health_id_498, &body)
        .expect("498 decodes its own update_health");
    assert!(
        events.iter().any(|directive| matches!(
            directive,
            Directive::Emit(ClientEvent::HealthChanged { .. })
        )),
        "the 498 adapter must read id {health_id_498} as update_health"
    );

    // Wrong: the 754 adapter reads the same id as experience, and rejects the
    // three bytes it does not account for.
    let wrong = lodestone_v1_14::adapter_for(PROTOCOL_1_16_5);
    let mut world = World::new();
    let result = wrong.handle_packet(&mut world, ConnectionState::Play, health_id_498, &body);
    match result {
        Err(err) => assert!(
            err.to_string().contains("3 trailing bytes"),
            "expected the shorter `experience` read to leave three bytes over, got {err}"
        ),
        Ok(directives) => panic!(
            "the 754 adapter accepted 498's update_health bytes and emitted \
             {directives:?} — a misroute that no longer fails loudly, which is \
             the regression this control exists for"
        ),
    }
}

/// The general form of the control above, over the whole capture.
///
/// For every play packet in the 1.14.4 capture whose id names a **different**
/// packet at 754, the 754 adapter must not emit a gameplay event: it either
/// rejects the body or the id is one it deliberately ignores. Silently
/// dropping a packet is bad; silently emitting the *wrong* one is the failure
/// class that reaches the screen as plausible nonsense, and this is the line
/// between them.
///
/// The ids that genuinely agree across 498 and 754 are exempted by name
/// rather than by outcome — `difficulty`, `held_item_slot`, `world_border`,
/// `update_view_position` and `update_time` sit at the same number in both,
/// so a correct event from them is correct, not a coincidence.
#[test]
fn misrouting_between_protocols_is_never_a_plausible_wrong_event() {
    let wrong = lodestone_v1_14::adapter_for(PROTOCOL_1_16_5);
    let mut agreed = 0usize;
    let mut misrouted = 0usize;

    for packet in read_capture("1.14.4") {
        if packet.state != ConnectionState::Play {
            continue;
        }
        let name_498 = lodestone_v1_14::packet_ids_498::play::clientbound::ENTRIES
            .iter()
            .find(|(_, id)| *id == packet.id)
            .map(|(name, _)| *name);
        let name_754 = lodestone_v1_14::packet_ids::play::clientbound::ENTRIES
            .iter()
            .find(|(_, id)| *id == packet.id)
            .map(|(name, _)| *name);
        if name_498 == name_754 {
            agreed += 1;
            continue;
        }
        misrouted += 1;

        let mut world = World::new();
        if let Ok(directives) =
            wrong.handle_packet(&mut world, ConnectionState::Play, packet.id, &packet.payload)
        {
            let emitted: Vec<&ClientEvent> = directives
                .iter()
                .filter_map(|directive| match directive {
                    Directive::Emit(event) => Some(event),
                    _ => None,
                })
                .collect();
            assert!(
                emitted.is_empty(),
                "id {} is {:?} at 498 and {:?} at 754, and the 754 adapter turned \
                 498's bytes into {emitted:?} — a plausible wrong event, which is \
                 exactly what an era crate's per-protocol id tables exist to \
                 prevent",
                packet.id,
                name_498,
                name_754
            );
        }
    }

    // A control that exercised nothing would pass silently, so require both
    // buckets to be non-empty and pin the split measured off this capture.
    assert_eq!(
        (agreed, misrouted),
        (7, 19),
        "the capture's split between ids that agree across 498/754 and ids that \
         do not has moved; re-derive it rather than adjusting the numbers"
    );
}

/// Whole-capture form of the same control: 1.14.4's bytes fed to the 754
/// adapter must not come out as a clean join.
///
/// Broader and blunter than the packet-level control above, and it covers the
/// chunk framing, which is where this era's real risk is: 498 puts a column's
/// biomes *inside* the section buffer and packs section indices so a value may
/// cross a 64-bit boundary, and 754 does neither.
#[test]
fn the_1_14_4_capture_does_not_replay_as_1_16_5() {
    let outcome = replay_through(PROTOCOL_1_16_5, "1.14.4");
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
        (logins == 0 && chunks == 0) || !outcome.errors.is_empty(),
        "the 754 adapter read 1.14.4's capture as a clean join ({logins} Login \
         events, {chunks} chunks, {} errors) — then the two protocols' tables do \
         not actually differ and this era merge is untested",
        outcome.errors.len()
    );
    // Pin the shape of the failure, not just its existence: this is the
    // measurement that stands in for "watched the control fail first", since
    // before the era merge `adapter_for(498)` returned exactly this adapter.
    assert_eq!(
        (logins, chunks, outcome.errors.len()),
        (0, 0, 10),
        "the pre-merge failure mode has changed shape; re-derive it rather than \
         adjusting the numbers"
    );
}

/// 1.15.2's capture against 498's adapter. Narrower than the pair above,
/// because 498 and 578 agree on every packet *name* and differ only in ids
/// and three shapes — which is exactly the case where a single shared table
/// would look like it worked.
#[test]
fn the_1_15_2_capture_does_not_replay_as_1_14_4() {
    let outcome = replay_through(PROTOCOL_1_14_4, "1.15.2");
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
        (logins == 0 && chunks == 0) || !outcome.errors.is_empty(),
        "the 498 adapter read 1.15.2's capture as a clean join ({logins} Login \
         events, {chunks} chunks, {} errors) — 1.15 shifted every clientbound id \
         above 7 by one and added a seed hash to `login`, so this must not pass",
        outcome.errors.len()
    );
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
/// paletted decode, the biome placement and the `ensure_empty` trailing-byte
/// check.
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
    use lodestone_testsupport::unique_username;
    use std::time::Instant;

    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: member.game_port,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: uuid::Uuid::new_v4(),
    };
    let adapter = lodestone_v1_14::adapter_for(member.protocol);
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

    let map_chunk_id = clientbound_id(member.protocol, "minecraft:map_chunk");
    let mut recorded: Vec<CapturedPacket> = Vec::new();
    let mut seen_per_id: std::collections::BTreeMap<i32, usize> = std::collections::BTreeMap::new();
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
            let seen = seen_per_id.entry(packet_id).or_default();
            let cap = if state == ConnectionState::Play && packet_id == map_chunk_id {
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
        }
    })
    .await;

    assert!(
        reached_play,
        "never reached Play against {}",
        member.minecraft
    );
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
         {health} health updates)",
        path.display(),
        recorded.len()
    );
}

#[tokio::test]
#[ignore = "records against a live 1.14.4 server: ./scripts/live-oracles/legacy.sh 1.14.4"]
async fn record_1_14_4() {
    record(&MEMBERS[0]).await;
}

#[tokio::test]
#[ignore = "records against a live 1.15.2 server: ./scripts/live-oracles/legacy.sh 1.15.2"]
async fn record_1_15_2() {
    record(&MEMBERS[1]).await;
}

