//! Real-join captures for protocol 404 and its two neighbours, and the
//! hermetic replay that consumes them.
//!
//! # What this is
//!
//! Three halves that never run together:
//!
//! * a **recorder**, `#[ignore]`d, that joins a real 1.13.2 server started by
//!   `scripts/live-oracles/legacy.sh 1.13.2`, records every clientbound packet
//!   it receives (state, id, body) to `tests/captures/join_1_13_2.txt`, and
//!   commits nothing itself;
//! * two **neighbour recorders**, also `#[ignore]`d, that do the same against
//!   a 1.12.2 and a 1.14.4 server using a **hand-written handshake** and no
//!   adapter at all — this crate cannot depend on another version crate, and
//!   does not need to: a handshake is a protocol VarInt, a host string, a port
//!   and a next-state VarInt in every one of these protocols, and login
//!   `set_compression`/`success` are ids 3 and 2 in all of them;
//! * **replay** tests that run in the default `cargo test`, read the committed
//!   captures, and drive every recorded packet through the real 404 adapter.
//!
//! # Why the neighbour captures exist
//!
//! 1.13.2 is a one-member era, so the "same crate, wrong protocol" control
//! every other era crate here uses has nothing to misroute *to*. What it has
//! instead is two era boundaries, and the neighbour captures are what makes
//! those boundaries falsifiable: bytes a real 1.12.2 server and a real 1.14.4
//! server actually sent, fed to the 404 adapter, which must not read either as
//! a clean join. Without them, "1.13.2 does not belong to either neighbouring
//! era" would be a claim about `minecraft-data` rather than about the wire.
//!
//! # Why the split matters
//!
//! 404's packet shapes came from `minecraft-data` — a cross-check-grade
//! source, not an authority — and for the chunk packet it is not merely
//! incomplete but silent: its 1.13.2 `map_chunk` names neither the per-section
//! light arrays nor the biome tail, both of which are really there.
//! `decode(encode(x)) == x` cannot distinguish a correct port from two
//! symmetric misunderstandings; a recorded body can.
//!
//! The replay is deliberately not a byte round-trip. It asserts *values* the
//! capture's own bytes pin down — the join packet's dimension and game mode,
//! and the flat world's own floor read back out of `lodestone-world` — because
//! those are the places where a wrong-but-well-formed decode lands.
//!
//! # Recording
//!
//! ```text
//! ./scripts/live-oracles/legacy.sh 1.13.2
//! cargo test -p lodestone-v1-13 --test capture_join -- --ignored --nocapture record_1_13_2
//! ```
//!
//! and, for the two boundary controls:
//!
//! ```text
//! ./scripts/live-oracles/legacy.sh 1.12.2
//! cargo test -p lodestone-v1-13 --test capture_join -- --ignored --nocapture record_neighbour_1_12_2
//! ./scripts/live-oracles/legacy.sh 1.14.4
//! cargo test -p lodestone-v1-13 --test capture_join -- --ignored --nocapture record_neighbour_1_14_4
//! ```

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestone_data::block_states;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, GameMode, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_v1_13::PROTOCOL_1_13_2;
use lodestone_world::World;

/// One clientbound packet as recorded off the wire.
struct CapturedPacket {
    /// Connection state the client was in when this packet arrived.
    state: ConnectionState,
    /// Raw packet id, as the recording protocol's own table numbers it.
    id: i32,
    /// Decompressed packet body, without the id varint.
    payload: Vec<u8>,
}

/// One oracle this file records against: its Minecraft version, the protocol
/// its server speaks, and the game port `scripts/live-oracles/legacy.sh`
/// publishes for it.
///
/// Ports match that script's table; the script is the single place they are
/// defined and this is the single place they are read.
struct Oracle {
    minecraft: &'static str,
    protocol: i32,
    game_port: u16,
}

/// The era itself.
const SELF_ORACLE: Oracle = Oracle {
    minecraft: "1.13.2",
    protocol: PROTOCOL_1_13_2,
    game_port: 25590,
};

/// The era below: 1.12.2, the last pre-Flattening release.
const NEIGHBOUR_BELOW: Oracle = Oracle {
    minecraft: "1.12.2",
    protocol: 340,
    game_port: 25568,
};

/// The era above: 1.14.4, which moved light out of the chunk packet and
/// repacked the block `position`.
const NEIGHBOUR_ABOVE: Oracle = Oracle {
    minecraft: "1.14.4",
    protocol: 498,
    game_port: 25586,
};

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

/// Resolves a packet name to its id in 404's own clientbound table.
///
/// Read from the generated table rather than written down, so an assertion
/// about dispatch is not also an assertion about the table's contents.
fn clientbound_id(name: &str) -> i32 {
    lodestone_v1_13::packet_ids::play::clientbound::ENTRIES
        .iter()
        .find(|(entry, _)| *entry == name)
        .map(|(_, id)| *id)
        .unwrap_or_else(|| panic!("protocol 404 carries no {name}"))
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

/// What replaying one capture through the 404 adapter produced.
struct ReplayOutcome {
    events: Vec<ClientEvent>,
    errors: Vec<String>,
    packets: usize,
    world: World,
}

fn replay(minecraft: &str) -> ReplayOutcome {
    let adapter = lodestone_v1_13::adapter_for(PROTOCOL_1_13_2);
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

/// Every assertion here is against a value the *server* chose, recovered from
/// bytes it sent: the flat oracle world's overworld dimension, its default
/// survival game mode, and the chunk columns it streamed. A decode that is
/// well-formed but wrong fails these; a byte round-trip would not.
#[test]
fn the_committed_1_13_2_capture_replays_cleanly() {
    let outcome = replay(SELF_ORACLE.minecraft);

    assert!(
        outcome.errors.is_empty(),
        "404 replay produced decode errors: {:?}",
        outcome.errors
    );
    assert!(
        outcome.packets >= 20,
        "the capture is too short to be a real join ({} packets)",
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
        .expect("the capture has no Login event");
    assert_eq!(
        login.0,
        GameMode::Survival,
        "the oracle world is a default (survival) flat world"
    );
    assert_eq!(
        login.1.to_string(),
        "minecraft:overworld",
        "404 carries the dimension as a signed integer, so `0` must reach the \
         model as the overworld and not as an invented world name"
    );

    let chunks = outcome
        .events
        .iter()
        .filter(|event| matches!(event, ClientEvent::ChunkLoaded { .. }))
        .count();
    assert!(
        chunks > 0,
        "the capture decoded no chunk columns — a map_chunk that fails \
         `ensure_empty` is reported as an error above, so zero here means the \
         recording never reached Play"
    );

    let keep_alives = outcome
        .events
        .iter()
        .filter(|event| matches!(event, ClientEvent::KeepAlive { .. }))
        .count();
    assert!(
        keep_alives > 0,
        "the capture has no keep_alive — that packet's id differs on both \
         sides of this era (0x1f at 340, 0x21 at 404, 0x20 at 498), so it is \
         one of the packets a neighbouring table would misroute"
    );
}

/// The flat preset's own floor, read back out of `lodestone-world`.
///
/// This is the assertion that exercises the era's whole chunk framing at
/// once — that light is read from *inside* each section rather than from a
/// separate packet, that the biome tail is 256 big-endian ints at the end of
/// the buffer, that the section indices straddle 64-bit boundaries, and that
/// the wire state ids are translated through 1.13.2's own block-state table.
/// Every one of those going wrong produces a world that is populated but
/// wrong, not an error.
///
/// The expected values come from the 26.2 registry, not from this crate; the
/// floor itself comes from the 1.13.2 server, which answers
/// `execute if block 160 0 0 minecraft:bedrock` / `160 3 0
/// minecraft:grass_block` with "Test passed" and every other candidate with
/// "Test failed".
#[test]
fn the_capture_lands_the_flat_presets_floor_in_canonical_ids() {
    let bedrock = canonical_state("minecraft:bedrock", &[]);
    let grass = canonical_state("minecraft:grass_block", &[("snowy", "false")]);
    let dirt = canonical_state("minecraft:dirt", &[]);
    assert!(
        bedrock != grass && grass != dirt && bedrock != dirt,
        "the three probes must be distinguishable in the canonical space"
    );

    let outcome = replay(SELF_ORACLE.minecraft);
    let mut checked = 0usize;
    for loaded in outcome.world.values() {
        let column = &loaded.column;
        checked += 1;
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, 0, z) == bedrock)),
            "y=0 is not uniformly canonical bedrock ({bedrock}) — a transposed \
             decode, the wrong long packing, or an untranslated wire state id"
        );
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, 1, z) == dirt)),
            "y=1 is not uniformly canonical dirt ({dirt})"
        );
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, 3, z) == grass)),
            "y=3 is not uniformly canonical grass_block ({grass}) — most likely \
             a section-relative Y offset error, which the y=0 check alone \
             cannot see"
        );
    }
    assert!(checked > 0, "the capture put no columns into the world store");
}

/// Light arrives **inside** `map_chunk` at 404, so a decoded column must
/// carry it.
///
/// The 1.14 era's equivalent columns come out with empty light by
/// construction, because there light is a separate packet. Here an empty
/// column light means the section loop silently skipped the two 2,048-byte
/// arrays — which cannot happen without the buffer's own length check
/// failing, so this is the positive half of that pair rather than a
/// restatement of it.
#[test]
fn decoded_columns_carry_the_light_that_travels_inside_map_chunk() {
    let outcome = replay(SELF_ORACLE.minecraft);
    let mut lit = 0usize;
    for loaded in outcome.world.values() {
        // The flat preset's floor is opaque, so the sky light directly under
        // it must be zero and the sky light above it full — a claim about
        // values the server computed, not merely about array presence.
        assert_eq!(
            loaded.light.section_sky_light(0, 0, 1, 0),
            Some(0),
            "sky light inside the flat floor must be 0"
        );
        assert_eq!(
            loaded.light.section_sky_light(0, 0, 5, 0),
            Some(15),
            "sky light above the flat floor must be full"
        );
        lit += 1;
    }
    assert!(lit > 0, "the capture put no columns into the world store");
}

// ---------------------------------------------------------------------------
// The two era-boundary controls.
// ---------------------------------------------------------------------------

/// **The negative control, lower boundary.** A real 1.12.2 join fed to the
/// 404 adapter must not come out as a clean join.
///
/// This is the pre-Flattening side: `set_slot`'s and `window_items`' slot
/// encoding, the entity-metadata type table and the entity registry all
/// change at 1.13, and every clientbound id from `nbt_query_response` (0x1d)
/// upward shifts because 1.13 inserted six packets into the middle of the
/// table.
#[test]
fn a_real_1_12_2_join_does_not_replay_as_1_13_2() {
    assert_neighbour_is_rejected(&NEIGHBOUR_BELOW);
}

/// **The negative control, upper boundary.** A real 1.14.4 join fed to the
/// 404 adapter must not come out as a clean join either.
#[test]
fn a_real_1_14_4_join_does_not_replay_as_1_13_2() {
    assert_neighbour_is_rejected(&NEIGHBOUR_ABOVE);
}

/// Shared body of the two boundary controls.
///
/// The claim is deliberately narrow and checkable: the neighbour's bytes must
/// not produce both a `Login` event and a decoded chunk column. Anything
/// looser would pass on an adapter that had quietly become lenient.
fn assert_neighbour_is_rejected(oracle: &Oracle) {
    let outcome = replay(oracle.minecraft);
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
        chunks == 0,
        "the 404 adapter decoded {chunks} chunk column(s) out of a real {} join \
         — then the two protocols' chunk framings do not actually differ and \
         this era boundary is untested",
        oracle.minecraft
    );
    assert!(
        !(logins > 0 && outcome.errors.is_empty()),
        "the 404 adapter read {}'s capture as a clean join ({logins} Login \
         events, {} errors)",
        oracle.minecraft,
        outcome.errors.len()
    );
}

/// The general form of the two controls above, and the place this era's
/// honest answer is recorded rather than predicted.
///
/// For every play packet in a neighbour's capture whose id names a
/// **different** packet at 404, what does the 404 adapter actually do? The
/// 1.14 era measured its own answer as "nothing plausible ever comes out" --
/// every misroute there errors or lands on an ignored id. **That is not true
/// here, and pretending otherwise would be the more comfortable claim rather
/// than the measured one.**
///
/// Measured, from the two committed captures: **seven** misroutes on each
/// side emit a real, well-formed, wrong gameplay event -- 7 of 25 across the
/// lower boundary and 7 of 50 across the upper one. A 1.12.2 `abilities` body
/// read as 404's `open_sign_entity` becomes a `SignEditorOpened` at block
/// (62771, 819, 20827340); a 1.12.2 `update_health` read as `entity_velocity`
/// becomes a velocity for entity 65; and a 1.14.4 **`map_chunk`** read as
/// 404's `keep_alive` becomes a `KeepAlive` with id 4294967298, because the
/// keep-alive arm reads eight bytes off the front of a 30-kilobyte column and
/// stops. Nothing errors, nothing is dropped, and nothing about any of those
/// events says it came from the wrong protocol.
///
/// The reason is structural, not a defect in any one arm: most of the packets
/// this crate translates are short, fixed-width and unvalidated beyond their
/// length, so a body of the right size decodes into whichever struct the id
/// selects. The 1.14 era's stronger property came from its packets happening
/// to differ in length at the ids that collide, not from a check it has and
/// this crate lacks.
///
/// So the guarantee this crate can actually offer is the whole-stream one the
/// two tests above assert -- a neighbour's join never comes out as a clean
/// join -- and **not** a per-packet one. Recording the split here is what
/// keeps that distinction from being quietly forgotten: the numbers are
/// asserted, so a change on either side surfaces as a mismatch to re-derive
/// rather than as a silently weaker control.
#[test]
fn misrouting_across_an_era_boundary_is_measured_not_assumed() {
    let mut measured = Vec::new();
    for oracle in [&NEIGHBOUR_BELOW, &NEIGHBOUR_ABOVE] {
        let adapter = lodestone_v1_13::adapter_for(PROTOCOL_1_13_2);
        let mut agreed = 0usize;
        let mut misrouted = 0usize;
        let mut emitted_from_misroute = 0usize;

        for packet in read_capture(oracle.minecraft) {
            if packet.state != ConnectionState::Play {
                continue;
            }
            let name_404 = lodestone_v1_13::packet_ids::play::clientbound::ENTRIES
                .iter()
                .find(|(_, id)| *id == packet.id)
                .map(|(name, _)| *name);
            let name_neighbour = neighbour_clientbound_name(oracle, packet.id);
            if name_404 == name_neighbour {
                agreed += 1;
                continue;
            }
            misrouted += 1;

            let mut world = World::new();
            if let Ok(directives) =
                adapter.handle_packet(&mut world, ConnectionState::Play, packet.id, &packet.payload)
            {
                let events: Vec<&ClientEvent> = directives
                    .iter()
                    .filter_map(|directive| match directive {
                        Directive::Emit(event) => Some(event),
                        _ => None,
                    })
                    .collect();
                if !events.is_empty() {
                    emitted_from_misroute += 1;
                    eprintln!(
                        "{} id {} is {name_neighbour:?} there and {name_404:?} at 404, \
                         and produced {events:?}",
                        oracle.minecraft, packet.id
                    );
                }
            }
        }
        measured.push((agreed, misrouted, emitted_from_misroute));
    }

    assert_eq!(
        (measured[0], measured[1]),
        (NEIGHBOUR_BELOW_SPLIT, NEIGHBOUR_ABOVE_SPLIT),
        "the (agreed, misrouted, misroute-emitted-an-event) split has moved for one \
         of the two neighbours; re-derive it rather than adjusting the numbers"
    );

    // A control that exercised nothing would pass silently: require both
    // captures to actually contain misroutable ids.
    assert!(
        measured[0].1 > 0 && measured[1].1 > 0,
        "neither capture carried an id that names a different packet at 404, so \
         this measured nothing"
    );
}

/// Measured split for the 1.12.2 capture: play packets whose id names the same
/// packet at 404, play packets whose id names a different one, and how many of
/// the latter the 404 adapter turned into a gameplay event anyway.
///
/// One id in twenty-six agrees. 1.13 inserted six clientbound packets into the
/// middle of the table, so almost nothing above `nbt_query_response` keeps its
/// number -- which is also why this boundary produces the most misroutes per
/// packet of the two.
const NEIGHBOUR_BELOW_SPLIT: (usize, usize, usize) = (1, 25, 7);

/// The same three numbers for the 1.14.4 capture. Twenty-one of its
/// seventy-one play packets sit at an id 404 gives the same name, because
/// 1.14's own insertions are further up the table than 1.13's were; the
/// remaining fifty are misroutes, and seven of those still come out as
/// events.
const NEIGHBOUR_ABOVE_SPLIT: (usize, usize, usize) = (21, 50, 7);

/// The neighbour's own clientbound name for a wire id, read out of its
/// `minecraft-data` table rather than out of any crate.
///
/// This crate cannot depend on `lodestone-v1-9` or `lodestone-v1-14` (the
/// isolation lint forbids a version crate depending on a version crate), and
/// should not: what a control needs is the *neighbour protocol's* naming, and
/// that is a fact about `minecraft-data`, not about a sibling crate. The two
/// tables below are committed alongside the captures they explain.
fn neighbour_clientbound_name(oracle: &Oracle, id: i32) -> Option<&'static str> {
    let table: &[(&'static str, i32)] = match oracle.protocol {
        340 => &NEIGHBOUR_BELOW_NAMES,
        _ => &NEIGHBOUR_ABOVE_NAMES,
    };
    table
        .iter()
        .find(|(_, entry)| *entry == id)
        .map(|(name, _)| *name)
}

include!("captures/neighbour_names.rs");

// ---------------------------------------------------------------------------
// Recorders — `#[ignore]`d; each needs a live server.
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
/// paletted decode, the inline light, the biome tail and the `ensure_empty`
/// trailing-byte check.
const MAX_CHUNK_BODIES: usize = 1;

/// Login-state clientbound `set_compression`. The same id in 340, 404 and
/// 498, which is what lets the neighbour recorder below drive a login without
/// any of those protocols' id tables.
const LOGIN_SET_COMPRESSION: i32 = 3;

/// Login-state clientbound `success`, likewise identical in all three.
const LOGIN_SUCCESS: i32 = 2;

/// Writes one capture file.
fn write_capture(oracle: &Oracle, recorded: &[CapturedPacket], note: &str) {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# lodestone clientbound join capture -- Minecraft {} (protocol {})",
        oracle.minecraft, oracle.protocol
    );
    let _ = writeln!(
        out,
        "# recorded by tests/capture_join.rs against ./scripts/live-oracles/legacy.sh {}",
        oracle.minecraft
    );
    let _ = writeln!(out, "# {note}");
    let _ = writeln!(out, "# <state> <packet id> <body, hex>");
    for packet in recorded {
        let _ = writeln!(
            out,
            "{} {} {}",
            state_name(packet.state),
            packet.id,
            to_hex(&packet.payload)
        );
    }
    let path = capture_path(oracle.minecraft);
    std::fs::create_dir_all(captures_dir()).expect("create captures dir");
    std::fs::write(&path, out).expect("write capture");
    eprintln!("wrote {} ({} packets)", path.display(), recorded.len());
}

/// Drives one real 404 join through this crate's own adapter and writes the
/// capture.
///
/// Records every packet id the wire produced, including the ones this family
/// does not translate: a capture is evidence about the wire, and trimming it
/// to the packets already handled would make it agree with the port by
/// construction. The replay above tolerates untranslated packets because the
/// dispatch table returns no directives for an `IGNORED` id.
async fn record_self() {
    use lodestone_net::Connection;
    use lodestone_testsupport::unique_username;
    use std::time::Instant;

    let oracle = &SELF_ORACLE;
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: oracle.game_port,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: uuid::Uuid::new_v4(),
    };
    let adapter = lodestone_v1_13::adapter_for(oracle.protocol);
    let mut world = World::new();

    let mut conn = Connection::connect(("127.0.0.1", oracle.game_port))
        .await
        .unwrap_or_else(|err| {
            panic!(
                "connect to the {} oracle on :{} ({err}) -- start it with \
                 ./scripts/live-oracles/legacy.sh {}",
                oracle.minecraft, oracle.game_port, oracle.minecraft
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

    let map_chunk_id = clientbound_id("minecraft:map_chunk");
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

    assert!(reached_play, "never reached Play against {}", oracle.minecraft);
    assert!(chunks > 0, "no chunks decoded from {}", oracle.minecraft);
    write_capture(
        oracle,
        &recorded,
        "real server bytes; the outside oracle for this protocol's ids and shapes.",
    );
}

/// Encodes a VarInt the way every protocol in this range does.
fn var_int(mut value: i32) -> Vec<u8> {
    let mut out = Vec::new();
    let mut bits = value as u32;
    loop {
        let byte = (bits & 0x7f) as u8;
        bits >>= 7;
        value = bits as i32;
        if value == 0 {
            out.push(byte);
            return out;
        }
        out.push(byte | 0x80);
    }
}

/// Encodes a length-prefixed UTF-8 string.
fn wire_string(text: &str) -> Vec<u8> {
    let mut out = var_int(
        i32::try_from(text.len()).expect("a test username or hostname is short"),
    );
    out.extend_from_slice(text.as_bytes());
    out
}

/// Drives a real join against a **neighbouring** protocol's server with a
/// hand-written handshake, and records what it sends.
///
/// No adapter is involved on either side: the point of the capture is that
/// the bytes are the neighbour's, and the only protocol knowledge needed to
/// obtain them is the handshake's four fields plus two login packet ids that
/// are the same number in 340, 404 and 498. That keeps the control free of
/// any dependency on another version crate, which the isolation lint forbids
/// and which would in any case make the control agree with a sibling port
/// rather than with the wire.
async fn record_neighbour(oracle: &Oracle) {
    use lodestone_net::Connection;
    use lodestone_testsupport::unique_username;
    use std::time::Instant;

    let username = unique_username();
    let mut conn = Connection::connect(("127.0.0.1", oracle.game_port))
        .await
        .unwrap_or_else(|err| {
            panic!(
                "connect to the {} oracle on :{} ({err}) -- start it with \
                 ./scripts/live-oracles/legacy.sh {}",
                oracle.minecraft, oracle.game_port, oracle.minecraft
            )
        });

    let mut handshake = var_int(oracle.protocol);
    handshake.extend(wire_string("127.0.0.1"));
    handshake.extend_from_slice(&oracle.game_port.to_be_bytes());
    handshake.extend(var_int(2));
    conn.write_packet(0, &handshake).await.expect("handshake");
    conn.write_packet(0, &wire_string(&username))
        .await
        .expect("login start");

    let mut state = ConnectionState::Login;
    let mut recorded: Vec<CapturedPacket> = Vec::new();
    let mut seen_per_id: std::collections::BTreeMap<i32, usize> = std::collections::BTreeMap::new();
    let started = Instant::now();
    let _ = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if started.elapsed() > Duration::from_secs(80) {
                break;
            }
            let read = tokio::time::timeout(Duration::from_secs(8), conn.read_packet()).await;
            let (packet_id, payload) = match read {
                Err(_) | Ok(Ok(None)) => break,
                Ok(Ok(Some(packet))) => packet,
                Ok(Err(err)) => panic!("read error: {err}"),
            };
            if state == ConnectionState::Login {
                if packet_id == LOGIN_SET_COMPRESSION {
                    let threshold = decode_leading_var_int(&payload);
                    conn.set_compression(threshold);
                    continue;
                }
                if packet_id == LOGIN_SUCCESS {
                    state = ConnectionState::Play;
                    continue;
                }
            }
            let seen = seen_per_id.entry(packet_id).or_default();
            if *seen < MAX_BODIES_PER_ID && payload.len() < 200_000 {
                *seen += 1;
                recorded.push(CapturedPacket {
                    state,
                    id: packet_id,
                    payload,
                });
            }
        }
    })
    .await;

    assert_eq!(
        state,
        ConnectionState::Play,
        "never reached Play against {} -- the server rejected the handshake",
        oracle.minecraft
    );
    write_capture(
        oracle,
        &recorded,
        "a NEIGHBOURING protocol's real bytes, kept as the era-boundary control.",
    );
}

/// Reads a leading VarInt out of a raw body.
fn decode_leading_var_int(payload: &[u8]) -> i32 {
    let mut value = 0i32;
    for (index, byte) in payload.iter().enumerate().take(5) {
        value |= i32::from(byte & 0x7f) << (7 * index);
        if byte & 0x80 == 0 {
            break;
        }
    }
    value
}

#[tokio::test]
#[ignore = "records against a live 1.13.2 server: ./scripts/live-oracles/legacy.sh 1.13.2"]
async fn record_1_13_2() {
    record_self().await;
}

#[tokio::test]
#[ignore = "records against a live 1.12.2 server: ./scripts/live-oracles/legacy.sh 1.12.2"]
async fn record_neighbour_1_12_2() {
    record_neighbour(&NEIGHBOUR_BELOW).await;
}

#[tokio::test]
#[ignore = "records against a live 1.14.4 server: ./scripts/live-oracles/legacy.sh 1.14.4"]
async fn record_neighbour_1_14_4() {
    record_neighbour(&NEIGHBOUR_ABOVE).await;
}
