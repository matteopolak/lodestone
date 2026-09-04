//! Real-join captures for this era's two protocols, and the hermetic replay
//! that consumes them.
//!
//! # What this is
//!
//! Two halves that never run together:
//!
//! * a **recorder**, `#[ignore]`d, that joins a real vanilla server started by
//!   `scripts/live-oracles/legacy.sh <version>`, records every clientbound
//!   packet it receives (state, id, body) to
//!   `tests/captures/join_<version>.txt`, and commits nothing itself;
//! * a **replay** test per protocol that runs in the default `cargo test`,
//!   reads the committed capture, and drives every recorded packet through the
//!   real adapter for that protocol.
//!
//! # Why the split matters here in particular
//!
//! Both protocols' packet shapes came from `minecraft-data` — a
//! cross-check-grade source, not an authority — and this era's chunk packet is
//! the one place in it where being wrong is silent. A section count taken from
//! the wrong place, a biome array read where none is written, or the trailing
//! VarInt of a single-valued palette left unconsumed all leave the buffer
//! misaligned rather than raising anything, and `decode(encode(x)) == x` is
//! satisfied by two symmetric misunderstandings. A recorded body is the
//! authority: bytes a real server actually sent.
//!
//! The replay is deliberately not a byte round-trip. It asserts *values* the
//! capture's own bytes pin down — the join packet's dimension and game mode,
//! the vertical window the server's own dimension entry declares, and the flat
//! world's floor read back out of `lodestone-world`.
//!
//! # Recording
//!
//! ```text
//! ./scripts/live-oracles/legacy.sh 1.17.1
//! cargo test -p lodestone-v1-17 --test capture_join -- --ignored --nocapture record_1_17_1
//! ```
//!
//! Repeat for `1.18.2`. Each brings its own container up on its own port, so
//! the two can be recorded in any order.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestone_data::block_states;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, GameMode, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_v1_17::{PROTOCOL_1_17_1, PROTOCOL_1_18_2};
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

/// One era member: its Minecraft version, protocol, oracle port, and the
/// vertical window its own vanilla flat overworld has.
///
/// Ports match `scripts/live-oracles/legacy.sh`'s table; that script is the
/// single place they are defined and this is the single place they are read.
///
/// `floor_y` is **not** a constant of the format — it is the release's own
/// overworld floor, which is the whole point of this era: 1.17 keeps the
/// historical `y = 0` and 1.18 moves it to `y = -64`. Both are asserted
/// against the server's own dimension entry below rather than merely written
/// here.
struct EraMember {
    minecraft: &'static str,
    protocol: i32,
    game_port: u16,
    floor_y: i32,
    section_count: usize,
}

const MEMBERS: &[EraMember] = &[
    EraMember {
        minecraft: "1.17.1",
        protocol: PROTOCOL_1_17_1,
        game_port: 25592,
        floor_y: 0,
        section_count: 16,
    },
    EraMember {
        minecraft: "1.18.2",
        protocol: PROTOCOL_1_18_2,
        game_port: 25594,
        floor_y: -64,
        section_count: 24,
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

/// One protocol's own clientbound play table.
fn clientbound_entries(protocol: i32) -> &'static [(&'static str, i32)] {
    if protocol == PROTOCOL_1_17_1 {
        lodestone_v1_17::packet_ids::play::clientbound::ENTRIES
    } else {
        lodestone_v1_17::packet_ids_758::play::clientbound::ENTRIES
    }
}

/// Resolves a packet name to its id in one protocol's own clientbound table.
///
/// Read from the generated tables rather than written down: fifteen ids move
/// across this era, so a literal here would be a claim about which protocol is
/// being talked about rather than a fact about it.
fn clientbound_id(protocol: i32, name: &str) -> i32 {
    clientbound_entries(protocol)
        .iter()
        .find(|(entry, _)| *entry == name)
        .map(|(_, id)| *id)
        .unwrap_or_else(|| panic!("protocol {protocol} carries no {name}"))
}

/// The canonical 26.2 state id for a block with the given properties.
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
    let adapter = lodestone_v1_17::adapter_for(protocol);
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
/// bytes it sent.
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
        "protocol {}'s join packet names the world it joined",
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
        "protocol {} capture has no keep_alive",
        member.protocol
    );
}

/// The flat preset's own floor, read back out of `lodestone-world` — at the
/// height the *server* said the world starts at.
///
/// This is the assertion that exercises the era's real risk. Getting the
/// section count wrong, reading the biome array in the wrong place, or leaving
/// a single-valued palette's trailing long count unconsumed all produce a
/// populated but wrong world rather than an error, and every one of them moves
/// where the floor lands.
///
/// The expected block ids come from the 26.2 registry, not from this crate,
/// and the expected *height* comes from the capture's own `login` packet
/// rather than from the constant in [`MEMBERS`] — the constant is only there
/// so a disagreement between the two is visible.
fn assert_flat_world_floor(member: &EraMember) {
    let bedrock = canonical_state("minecraft:bedrock", &[]);
    let dirt = canonical_state("minecraft:dirt", &[]);
    let grass = canonical_state("minecraft:grass_block", &[("snowy", "false")]);
    assert!(
        bedrock != dirt && dirt != grass && bedrock != grass,
        "the three probes must be distinguishable in the canonical space"
    );

    let outcome = replay(member);
    let mut checked = 0usize;
    for loaded in outcome.world.values() {
        let column = &loaded.column;
        assert_eq!(
            column.min_y(),
            member.floor_y,
            "protocol {}: the column's floor came from the server's own \
             dimension entry and disagrees with this era member's recorded \
             window",
            member.protocol
        );
        assert_eq!(
            column.section_count(),
            member.section_count,
            "protocol {}: section count",
            member.protocol
        );
        checked += 1;
        let base = member.floor_y;
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, base, z) == bedrock)),
            "protocol {}: y={base} is not uniformly canonical bedrock ({bedrock}) — \
             a wrong section count, the wrong vertical window, or the wrong \
             block-state table",
            member.protocol
        );
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, base + 1, z) == dirt)),
            "protocol {}: y={} is not uniformly canonical dirt ({dirt})",
            member.protocol,
            base + 1
        );
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, base + 3, z) == grass)),
            "protocol {}: y={} is not uniformly canonical grass_block ({grass}) — \
             most likely a section-relative Y offset error, which the floor \
             check alone cannot see",
            member.protocol,
            base + 3
        );
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, base + 4, z) != grass)),
            "protocol {}: y={} is grass too, so the floor is not four blocks \
             deep and the probe above is not discriminating",
            member.protocol,
            base + 4
        );
    }
    assert!(
        checked > 0,
        "protocol {} capture put no columns into the world store",
        member.protocol
    );
}

#[test]
fn committed_1_17_1_capture_replays_through_the_756_adapter() {
    assert_capture_replays_cleanly(&MEMBERS[0]);
}

#[test]
fn committed_1_18_2_capture_replays_through_the_758_adapter() {
    assert_capture_replays_cleanly(&MEMBERS[1]);
}

#[test]
fn the_1_17_1_capture_lands_the_flat_preset_floor_in_canonical_ids() {
    assert_flat_world_floor(&MEMBERS[0]);
}

#[test]
fn the_1_18_2_capture_lands_the_flat_preset_floor_in_canonical_ids() {
    assert_flat_world_floor(&MEMBERS[1]);
}

/// The era's defining claim, read straight off the wire: the two releases put
/// the world in different places, and the crate learns that from the server
/// rather than from a constant.
///
/// A single hardcoded sixteen-section column would satisfy the 1.17.1 half of
/// this and fail the 1.18.2 half, which is exactly the split that makes it
/// worth asserting.
#[test]
fn the_two_captures_declare_different_vertical_windows() {
    let windows: Vec<(i32, usize)> = MEMBERS
        .iter()
        .map(|member| {
            let outcome = replay(member);
            let column = outcome
                .world
                .values()
                .next()
                .unwrap_or_else(|| panic!("{} capture loaded no column", member.minecraft));
            (column.column.min_y(), column.column.section_count())
        })
        .collect();
    assert_eq!(
        windows,
        vec![(0, 16), (-64, 24)],
        "1.17.1's overworld is y=0..256 and 1.18.2's is y=-64..320; both come \
         from each server's own dimension entry"
    );
}

// ---------------------------------------------------------------------------
// Negative controls — each measured, not predicted.
// ---------------------------------------------------------------------------

/// **The packet-level negative control.** One packet, one id, two protocols.
///
/// `update_time` is clientbound id **88** at 756. 1.18 inserted
/// `simulation_distance` at that number and pushed everything above it up by
/// one, so id 88 at 758 is `set_title_subtitle` — a packet this crate also
/// handles, which is what makes the pair discriminating in both directions.
///
/// The outcome below was **measured** by running this control, not predicted.
#[test]
fn update_time_does_not_decode_as_the_other_protocols_packet() {
    let time_756 = clientbound_id(PROTOCOL_1_17_1, "minecraft:update_time");
    let time_758 = clientbound_id(PROTOCOL_1_18_2, "minecraft:update_time");
    assert_eq!((time_756, time_758), (88, 89));
    assert_eq!(
        clientbound_id(PROTOCOL_1_18_2, "minecraft:set_title_subtitle"),
        time_756,
        "the control only discriminates because 758 puts a *different* packet \
         at 756's update_time id"
    );

    let body = read_capture("1.17.1")
        .into_iter()
        .find(|packet| packet.state == ConnectionState::Play && packet.id == time_756)
        .expect("the 1.17.1 capture carries an update_time")
        .payload;
    assert_eq!(body.len(), 16, "update_time is two i64s");

    // Right: the 756 adapter reads its own id as a time update.
    let right = lodestone_v1_17::adapter_for(PROTOCOL_1_17_1);
    let mut world = World::new();
    let directives = right
        .handle_packet(&mut world, ConnectionState::Play, time_756, &body)
        .expect("756 decodes its own update_time");
    assert!(
        directives.iter().any(|directive| matches!(
            directive,
            Directive::Emit(ClientEvent::TimeChanged { .. })
        )),
        "the 756 adapter must read id {time_756} as update_time, got {directives:?}"
    );

    // Wrong: the 758 adapter reads the same id as `set_title_subtitle`, whose
    // body is one length-prefixed JSON string. Measured: the world age's own
    // leading byte is read as that length, so the read is short and the
    // adapter's exact decode rejects what is left.
    let wrong = lodestone_v1_17::adapter_for(PROTOCOL_1_18_2);
    let mut world = World::new();
    let err = wrong
        .handle_packet(&mut world, ConnectionState::Play, time_756, &body)
        .expect_err(
            "the 758 adapter accepted 756's update_time bytes — a misroute that \
             no longer fails loudly, which is the regression this control exists for",
        );
    assert!(
        err.to_string().contains("trailing bytes"),
        "expected the shorter string read to leave bytes over, got {err}"
    );
}

/// The general form of the control above, over the whole capture.
///
/// For every play packet in the 1.17.1 capture whose id names a **different**
/// packet at 758, what does the 758 adapter do with it? Silently dropping a
/// packet is bad; silently emitting the *wrong* one is the failure class that
/// reaches the screen as plausible nonsense.
///
/// **This test records a measurement, not a guarantee**, and the measurement
/// is the weaker of the two answers the earlier eras gave. Four captured ids
/// name a different packet at 758, and one of them **does** produce a real,
/// well-formed, wrong gameplay event: id 101 is `declare_recipes` at 756 and
/// `entity_effect` at 758, so a recipe list read as an effect becomes
/// `MobEffectApplied` for entity 1058 — levitation, amplifier 109, 105 ticks.
/// Nothing about that is red anywhere.
///
/// So the guarantee this crate can offer is the **whole-stream** one, which
/// [`the_1_17_1_capture_does_not_replay_as_1_18_2`] asserts directly, and not
/// a per-packet one. The split is pinned so that a change on either side
/// surfaces as a mismatch to re-derive rather than as a silently weaker
/// control.
#[test]
fn misrouting_between_protocols_is_measured_not_assumed() {
    let wrong = lodestone_v1_17::adapter_for(PROTOCOL_1_18_2);
    let mut agreed = 0usize;
    let mut errored = 0usize;
    let mut silent = 0usize;
    let mut plausible: Vec<String> = Vec::new();

    for packet in read_capture("1.17.1") {
        if packet.state != ConnectionState::Play {
            continue;
        }
        let name_756 = clientbound_entries(PROTOCOL_1_17_1)
            .iter()
            .find(|(_, id)| *id == packet.id)
            .map(|(name, _)| *name);
        let name_758 = clientbound_entries(PROTOCOL_1_18_2)
            .iter()
            .find(|(_, id)| *id == packet.id)
            .map(|(name, _)| *name);
        if name_756 == name_758 {
            agreed += 1;
            continue;
        }

        let mut world = World::new();
        match wrong.handle_packet(&mut world, ConnectionState::Play, packet.id, &packet.payload) {
            Err(_) => errored += 1,
            Ok(directives) => {
                let emitted: Vec<String> = directives
                    .iter()
                    .filter_map(|directive| match directive {
                        Directive::Emit(event) => Some(format!("{event:?}")),
                        _ => None,
                    })
                    .collect();
                if emitted.is_empty() {
                    silent += 1;
                } else {
                    plausible.push(format!(
                        "id {} is {name_756:?} at 756 and {name_758:?} at 758 -> {emitted:?}",
                        packet.id
                    ));
                }
            }
        }
    }

    // A control that exercised nothing would pass silently, so require the
    // misroutable bucket to be non-empty.
    assert!(
        errored + silent + plausible.len() > 0,
        "no captured packet id names a different packet at 758 — then the two \
         tables do not differ and this control tests nothing"
    );
    assert_eq!(
        (agreed, errored, silent, plausible.len()),
        (26, 1, 2, 1),
        "the capture's misroute split has moved; re-derive it rather than \
         adjusting the numbers. Plausible wrong events, if any: {plausible:?}"
    );
    assert!(
        plausible[0].contains("MobEffectApplied"),
        "the one plausible wrong event this capture produces is a recipe list \
         read as an effect; got {plausible:?}"
    );
}

/// Whole-capture form: 1.17.1's bytes fed to the 758 adapter must not come out
/// as a clean join.
///
/// Blunter than the packet-level control, and it covers the chunk framing,
/// which is where this era's real risk is: 758 has no section mask and no
/// column biome array where 756 has both, so a 758 decoder handed a 756 column
/// reads the mask's long count as a heightmap tag.
#[test]
fn the_1_17_1_capture_does_not_replay_as_1_18_2() {
    let outcome = replay_through(PROTOCOL_1_18_2, "1.17.1");
    let chunks = outcome
        .events
        .iter()
        .filter(|event| matches!(event, ClientEvent::ChunkLoaded { .. }))
        .count();
    assert!(
        chunks == 0 && !outcome.errors.is_empty(),
        "the 758 adapter read 1.17.1's capture as a clean join ({chunks} chunks, \
         {} errors) — then the two protocols' framings do not actually differ \
         and this era's chunk work is untested",
        outcome.errors.len()
    );
}

/// The mirror: 1.18.2's bytes fed to the 756 adapter.
#[test]
fn the_1_18_2_capture_does_not_replay_as_1_17_1() {
    let outcome = replay_through(PROTOCOL_1_17_1, "1.18.2");
    let chunks = outcome
        .events
        .iter()
        .filter(|event| matches!(event, ClientEvent::ChunkLoaded { .. }))
        .count();
    assert!(
        chunks == 0 && !outcome.errors.is_empty(),
        "the 756 adapter read 1.18.2's capture as a clean join ({chunks} chunks, \
         {} errors)",
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

/// `map_chunk` is two orders of magnitude larger than any other packet here —
/// and larger again at 758, which folds the light payload in — so it gets its
/// own, tighter cap.
const MAX_CHUNK_BODIES: usize = 1;

/// Drives one real join and writes the capture.
///
/// Records every packet id the wire produced, including the ones this family
/// does not translate: a capture is evidence about the wire, and trimming it
/// to the packets we already handle would make it agree with the port by
/// construction.
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
    let adapter = lodestone_v1_17::adapter_for(member.protocol);
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

    let overall = Duration::from_secs(150);
    let read_timeout = Duration::from_secs(8);
    let started = Instant::now();

    let _ = tokio::time::timeout(overall, async {
        loop {
            let done = reached_play && chunks > 0 && keep_alives > 0 && health > 0;
            if done || started.elapsed() > Duration::from_secs(140) {
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

#[test]
fn recorded_player_info_preserves_its_supplied_uuid() {
    let expected = uuid::Uuid::parse_str("967e200d-6bcf-3a2f-85c7-7090b36d2e83")
        .expect("fixture UUID is valid");
    let outcome = replay(&MEMBERS[0]);
    assert!(outcome.events.iter().any(|event| matches!(
        event,
        ClientEvent::PlayerListUpdate { entries }
            if entries.iter().any(|entry| entry.uuid == Some(expected))
    )), "the UUID supplied by the recorded player-info packet must reach the canonical event");
}

#[tokio::test]
#[ignore = "records against a live 1.17.1 server: ./scripts/live-oracles/legacy.sh 1.17.1"]
async fn record_1_17_1() {
    record(&MEMBERS[0]).await;
}

#[tokio::test]
#[ignore = "records against a live 1.18.2 server: ./scripts/live-oracles/legacy.sh 1.18.2"]
async fn record_1_18_2() {
    record(&MEMBERS[1]).await;
}
