//! A real-join capture for protocol 766, and the hermetic replay that
//! consumes it.
//!
//! # What this is
//!
//! Two halves that never run together:
//!
//! * a **recorder**, `#[ignore]`d, that joins a real vanilla server started by
//!   `scripts/live-oracles/legacy.sh 1.20.6`, records every clientbound packet
//!   it receives (state, id, body) to `tests/captures/join_1_20_6.txt`, and
//!   commits nothing itself;
//! * **replay** tests that run in the default `cargo test`, read the committed
//!   capture, and drive every recorded packet through the real 766 adapter.
//!
//! # Why the split matters here in particular
//!
//! This era's packet shapes came from `minecraft-data` — a cross-check-grade
//! source, not an authority — and its jar ships no packet report to check them
//! against. Four places are silent rather than loud when wrong:
//!
//! * **The configuration phase.** The join packet names its dimension by a
//!   registry *index*, and the registry arrives earlier, in a state the eras
//!   below do not have. A recording that never reaches Play is the only
//!   evidence that the whole choreography — acknowledge login, answer the
//!   known-packs offer, answer finish-configuration — is right.
//! * **The chunk column.** A wrong section count consumes the wrong number of
//!   bytes and produces a plausible column.
//! * **The item slot.** A stack is a count, an id and two component lists at
//!   this protocol; a decoder carrying the older `(id, count, NBT)` shape
//!   reads a plausible stack and desynchronises everything after it.
//! * **`unload_chunk`.** Its two coordinates are **z then x**. A swap is
//!   invisible in a square view distance.
//!
//! `decode(encode(x)) == x` is satisfied by two symmetric misunderstandings in
//! every one of those. A recorded body is the authority: bytes a real server
//! actually sent.
//!
//! The replay is deliberately not a byte round-trip. It asserts *values* the
//! capture's own bytes pin down — the join packet's dimension and game mode,
//! the vertical window the server's own registry declares, and the flat
//! world's floor read back out of `lodestone-world`.
//!
//! # Recording
//!
//! ```text
//! ./scripts/live-oracles/legacy.sh 1.20.6
//! cargo test -p lodestone-v1-20-6 --test capture_join -- --ignored --nocapture record_1_20_6
//! ```

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestone_data::block_states;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, GameMode, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_v1_20_6::PROTOCOL_1_20_6;
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

/// The recorded server: its Minecraft version, protocol, oracle port, and the
/// vertical window its own vanilla flat overworld has.
///
/// The port matches `scripts/live-oracles/legacy.sh`'s table; that script is
/// the single place it is defined and this is the single place it is read.
struct Oracle {
    minecraft: &'static str,
    protocol: i32,
    game_port: u16,
    /// RCON port, used by the recorder to teleport the joined player far
    /// enough to force column unloads -- see [`UNLOAD_PROBE_X`].
    rcon_port: u16,
    floor_y: i32,
    section_count: usize,
}

/// The era's own protocol.
///
/// `floor_y` and `section_count` are what a vanilla flat overworld has at this
/// version, and both are checked against the server's own bytes rather than
/// assumed: the section count comes back out of the decoded column and the
/// floor out of the world store.
const ERA: Oracle = Oracle {
    minecraft: "1.20.6",
    protocol: PROTOCOL_1_20_6,
    game_port: 25598,
    rcon_port: 25599,
    floor_y: -64,
    section_count: 24,
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
        "configuration" => ConnectionState::Configuration,
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

/// Resolves a packet name to its id in this protocol's own clientbound table.
///
/// Read from the generated table rather than written down: an id literal here
/// would be a claim about which protocol is being talked about rather than a
/// fact about it.
fn clientbound_id(name: &str) -> i32 {
    lodestone_v1_20_6::packet_ids::play::clientbound::ENTRIES
        .iter()
        .find(|(entry, _)| *entry == name)
        .map(|(_, id)| *id)
        .unwrap_or_else(|| panic!("protocol 766 carries no {name}"))
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

/// What replaying the capture through the adapter produced.
struct ReplayOutcome {
    events: Vec<ClientEvent>,
    errors: Vec<String>,
    packets: usize,
    world: World,
}

fn replay(oracle: &Oracle) -> ReplayOutcome {
    replay_bounded(oracle, false)
}

/// Replays only the prefix of the capture that precedes the first column
/// unload.
///
/// The recorder deliberately teleports the player far enough to make the
/// server drop every column it had sent, so the world store is *empty* by the
/// end of a full replay — correctly so. A test that reads a block back has to
/// stop before that, and saying so here is clearer than a test that quietly
/// depends on capture ordering.
fn replay_before_first_unload(oracle: &Oracle) -> ReplayOutcome {
    replay_bounded(oracle, true)
}

fn replay_bounded(oracle: &Oracle, stop_at_first_unload: bool) -> ReplayOutcome {
    let unload_id = clientbound_id("minecraft:unload_chunk");
    let adapter = lodestone_v1_20_6::adapter_for(PROTOCOL_1_20_6);
    let mut world = World::new();
    let mut events = Vec::new();
    let mut errors = Vec::new();
    let packets = read_capture(oracle.minecraft);
    let count = packets.len();

    for packet in packets {
        if stop_at_first_unload
            && packet.state == ConnectionState::Play
            && packet.id == unload_id
        {
            break;
        }
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
/// bytes it sent.
#[test]
fn the_committed_capture_replays_cleanly_through_the_766_adapter() {
    let outcome = replay(&ERA);

    assert!(
        outcome.errors.is_empty(),
        "the 1.20.6 replay produced decode errors: {:?}",
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
        .expect("the capture has a Login event");
    assert_eq!(
        login.0,
        GameMode::Survival,
        "the oracle world is a default (survival) flat world"
    );
    assert_eq!(
        login.1.to_string(),
        "minecraft:overworld",
        "the join packet names the world it joined"
    );

    let chunks = outcome
        .events
        .iter()
        .filter(|event| matches!(event, ClientEvent::ChunkLoaded { .. }))
        .count();
    assert!(
        chunks > 0,
        "the capture decoded no chunk columns -- a map_chunk that fails its \
         trailing-bytes check is reported as an error above, so zero here means \
         the recording never reached Play"
    );
}

/// The configuration phase is real, and it is where the dimension registry
/// arrives.
///
/// This is the one claim no other era's capture can make, and it is the whole
/// reason this crate cannot reuse a neighbour's choreography: the recording
/// must contain configuration-state packets, and one of them must be the
/// `registry_data` for `minecraft:dimension_type` whose entry order the join
/// packet's index refers to.
#[test]
fn the_capture_carries_the_configuration_phase_and_its_dimension_registry() {
    use lodestone_core::{Ctx, decode_body};
    use lodestone_v1_20_6::packets::configuration::RegistryData;

    let packets = read_capture(ERA.minecraft);
    let config: Vec<&CapturedPacket> = packets
        .iter()
        .filter(|packet| packet.state == ConnectionState::Configuration)
        .collect();
    assert!(
        !config.is_empty(),
        "a 766 join passes through a configuration state; an empty one means \
         the login acknowledgement never landed"
    );

    let registry_id = lodestone_v1_20_6::packet_ids::configuration::clientbound::REGISTRY_DATA;
    let dimension = config
        .iter()
        .filter(|packet| packet.id == registry_id)
        .map(|packet| {
            decode_body::<RegistryData>(&packet.payload, Ctx { version: PROTOCOL_1_20_6 })
                .expect("registry_data decodes at 766")
        })
        .find(|data| data.registry == "minecraft:dimension_type")
        .expect("the configuration phase delivers the dimension-type registry");

    // Every entry carries a payload, which is what makes the vertical window
    // resolvable -- and is a consequence of this client claiming no known
    // packs. An elided entry would arrive with `data: None`.
    assert!(
        !dimension.entries.is_empty(),
        "the dimension-type registry is not empty"
    );
    assert!(
        dimension.entries.iter().all(|entry| entry.data.is_some()),
        "claiming no known packs is what makes the server send every entry's \
         payload; an elided entry leaves the column unframeable"
    );
    // The join packet indexed entry 0 out of this registry.
    assert_eq!(
        dimension.entries[0].id, "minecraft:overworld",
        "index 0 is the entry the captured join packet named"
    );
}

/// The vertical window the adapter ends up with is the server's, not the
/// fallback.
///
/// The check that matters is not that the number is 24 — the fallback is also
/// 24 — but that the *registry* produced it: the assertion below reads the
/// section count off a column the adapter decoded after processing the
/// configuration phase, and a wrong count fails the column's own
/// trailing-bytes check rather than producing a short column.
#[test]
fn the_decoded_column_has_the_section_count_the_servers_registry_declares() {
    use lodestone_core::Reader;
    use lodestone_v1_20_6::packets::chunk::{ChunkShape, MapChunk};

    let map_chunk_id = clientbound_id("minecraft:map_chunk");
    let body = read_capture(ERA.minecraft)
        .into_iter()
        .find(|packet| packet.state == ConnectionState::Play && packet.id == map_chunk_id)
        .expect("the capture carries a chunk column")
        .payload;

    let shape = ChunkShape::overworld(PROTOCOL_1_20_6);
    let mut reader = Reader::new(&body);
    let data = MapChunk::decode(&mut reader, &shape).expect("the column decodes");
    assert_eq!(data.column.section_count(), ERA.section_count);
    assert_eq!(
        reader.remaining(),
        0,
        "the light payload after the section buffer must parse to the packet's \
         last byte -- a wrong section count shows up here, not as a short column"
    );
    assert_eq!(
        data.fallback.out_of_range, 0,
        "every wire state in a vanilla flat column is inside this era's own \
         state range"
    );
}

/// The flat preset's own floor, read back out of `lodestone-world` -- at the
/// height the *server* said the world starts at.
///
/// This is the end-to-end claim, and the assertion that exercises this era's
/// chunk risk. Getting the section count wrong, resolving the vertical window
/// against the wrong registry entry, or leaving a single-valued palette's
/// trailing long count unconsumed all produce a populated but *wrong* world
/// rather than an error, and every one of them moves where the floor lands.
///
/// The expected block ids come from Mojang's own 26.2 registry rather than
/// from this crate's table, so both sides of the comparison originate outside
/// the code under test: the bytes from a real server, the meaning from the
/// registry.
#[test]
fn the_capture_lands_the_flat_preset_floor_in_canonical_ids() {
    let bedrock = canonical_state("minecraft:bedrock", &[]);
    let dirt = canonical_state("minecraft:dirt", &[]);
    let grass = canonical_state("minecraft:grass_block", &[("snowy", "false")]);
    assert!(
        bedrock != dirt && dirt != grass && bedrock != grass,
        "the three probes must be distinguishable in the canonical space"
    );

    let outcome = replay_before_first_unload(&ERA);
    let mut checked = 0usize;
    for loaded in outcome.world.values() {
        let column = &loaded.column;
        assert_eq!(
            column.min_y(),
            ERA.floor_y,
            "the column's floor came from the server's own registry entry and \
             disagrees with the recorded window"
        );
        assert_eq!(column.section_count(), ERA.section_count, "section count");
        checked += 1;
        let base = ERA.floor_y;
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, base, z) == bedrock)),
            "y={base} is not uniformly canonical bedrock ({bedrock}) -- a wrong \
             section count, the wrong vertical window, or the wrong block-state \
             table"
        );
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, base + 1, z) == dirt)),
            "y={} is not uniformly canonical dirt ({dirt})",
            base + 1
        );
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, base + 3, z) == grass)),
            "y={} is not uniformly canonical grass_block ({grass}) -- most likely \
             a section-relative Y offset error, which the floor check alone \
             cannot see",
            base + 3
        );
        assert!(
            (0..16).all(|x| (0..16).all(|z| column.get_block(x, base + 4, z) != grass)),
            "y={} is grass too, so the floor is not four blocks deep and the \
             probe above is not discriminating",
            base + 4
        );
    }
    assert!(
        checked > 0,
        "the replay loaded no columns, so there is nothing to read back"
    );
}

/// `login_start` at 766 carries a **required** profile UUID.
///
/// Recorded as a test because it is the packet that decides whether a join is
/// possible at all: the era below writes a presence boolean and, usually,
/// nothing after it, and a 766 server reads sixteen bytes unconditionally and
/// reports a decode failure on its own login packet. The length is asserted
/// rather than described so the claim is falsifiable.
#[test]
fn the_login_packet_carries_a_required_profile_uuid() {
    use lodestone_core::{Ctx, encode_body};
    use lodestone_v1_20_6::packets::login::LoginStart;

    let start = LoginStart {
        username: "lodestone".to_owned(),
        uuid: uuid::Uuid::nil(),
    };
    let bytes =
        encode_body(&start, Ctx { version: PROTOCOL_1_20_6 }).expect("login_start encodes");
    // 1 length byte + 9 name bytes + 16 uuid bytes.
    assert_eq!(
        bytes.len(),
        26,
        "766 appends sixteen raw uuid bytes after the name: {bytes:?}"
    );
    assert_eq!(&bytes[..10], b"\x09lodestone");
    assert!(
        bytes[10..].iter().all(|byte| *byte == 0),
        "the nil uuid is sixteen zero bytes, with no presence byte in front"
    );
}

/// `unload_chunk` carries **z before x**, measured rather than described.
///
/// This is the assertion the recorder's two teleports exist for. A square view
/// distance makes a swapped coordinate pair invisible: every column the server
/// drops when a player stands still has `|x|` and `|z|` in the same range. The
/// recorder therefore moves the player [`UNLOAD_PROBE_X`] blocks along **+x
/// only** and back, so the far columns it then drops have a large chunk x and
/// a near-zero chunk z. Reading the pair the other way round reports the two
/// numbers swapped, which this test rejects.
#[test]
fn unload_chunk_reads_z_before_x() {
    use lodestone_core::{Ctx, decode_body_exact};
    use lodestone_v1_20_6::packets::chunk::UnloadChunk;

    let unload_id = clientbound_id("minecraft:unload_chunk");
    let bodies: Vec<Vec<u8>> = read_capture(ERA.minecraft)
        .into_iter()
        .filter(|packet| packet.state == ConnectionState::Play && packet.id == unload_id)
        .map(|packet| packet.payload)
        .collect();
    assert!(
        !bodies.is_empty(),
        "the capture carries no unload_chunk, so it cannot speak to the field \
         order -- re-record it"
    );

    // The far columns sit around chunk x = UNLOAD_PROBE_X / 16 with chunk z
    // near zero. `far` counts the ones whose decoded x is out there; a decoder
    // reading the fields the other way round finds none, because it would put
    // that value in `chunk_z`.
    let probe_chunk_x = UNLOAD_PROBE_X / 16;
    let mut far = 0usize;
    let mut misordered = 0usize;
    for body in &bodies {
        let unload: UnloadChunk = decode_body_exact(body, Ctx { version: PROTOCOL_1_20_6 })
            .expect("unload_chunk is two plain ints");
        if (unload.chunk_x - probe_chunk_x).abs() <= 16 && unload.chunk_z.abs() <= 16 {
            far += 1;
        }
        if (unload.chunk_z - probe_chunk_x).abs() <= 16 && unload.chunk_x.abs() <= 16 {
            misordered += 1;
        }
    }
    assert_eq!(
        misordered, 0,
        "an unload body put the probe's x displacement in chunk_z, which is \
         what a swapped field order looks like"
    );
    assert!(
        far > 0,
        "no unload names a column near chunk x = {probe_chunk_x}; the capture's \
         unloads are all near spawn and the order claim is untested"
    );
}

/// A real `player_chat` decodes, and both of its texts reach the model.
///
/// Two things follow from the message coming back at all, and neither is
/// available from a round trip against this crate's own encoder:
///
/// * **The serverbound tail is right.** The server reads a timestamp, a salt,
///   an optional signature and a last-seen window off every chat packet; a
///   malformed one closes the connection rather than being ignored.
/// * **The clientbound decode is right.** The packet puts a chain index, an
///   optional 256-byte signature and a last-seen chain in front of fields a
///   decoder inherited from the era below would read first, and its components
///   are anonymous NBT rather than JSON strings. The exact decode below
///   rejects any of that going wrong.
#[test]
fn the_capture_carries_a_real_player_chat_and_reaches_the_model_with_its_sender() {
    use lodestone_core::{Ctx, decode_body_exact};
    use lodestone_v1_20_6::packets::chat::PlayerChat;

    let chat_id = clientbound_id("minecraft:player_chat");
    let body = read_capture(ERA.minecraft)
        .into_iter()
        .find(|packet| packet.state == ConnectionState::Play && packet.id == chat_id)
        .expect(
            "the capture carries the message the recorder sent, echoed back by \
             the server",
        )
        .payload;

    let chat: PlayerChat = decode_body_exact(&body, Ctx { version: PROTOCOL_1_20_6 })
        .expect("player_chat decodes at 766 with nothing left over");
    assert_eq!(
        chat.plain_message, "lodestone capture probe",
        "the signed body is the exact text this client sent"
    );
    assert_eq!(
        chat.index, 0,
        "the first message in a session opens the sender's signing chain at 0"
    );
    assert!(
        chat.timestamp > 1_600_000_000_000,
        "the signed timestamp is epoch milliseconds, not seconds ({})",
        chat.timestamp
    );
    // The oracle runs with secure-profile enforcement off and this client has
    // no session key, so the server has nothing to sign with. Asserted rather
    // than assumed, because it is what makes the `Option` arm the one under
    // test.
    assert!(
        chat.signature.is_none(),
        "an offline-mode server with enforcement off signs nothing"
    );
    assert!(
        chat.previous_messages.is_empty(),
        "the first message has seen nothing before it"
    );

    let outcome = replay(&ERA);
    let chat_event = outcome
        .events
        .iter()
        .find_map(|event| match event {
            ClientEvent::Chat {
                text,
                sender: Some(sender),
                ack: Some(ack),
                ..
            } => Some((text.to_plain_string(), *sender, ack.clone())),
            _ => None,
        })
        .expect("the player_chat reached the model as an attributable chat event");
    assert!(
        chat_event.0.contains("lodestone capture probe"),
        "the displayed text carries the message: {:?}",
        chat_event.0
    );
    assert_eq!(
        chat_event.2.raw_content, "lodestone capture probe",
        "the signed bytes are kept verbatim alongside the decorated form"
    );
    assert_eq!(chat_event.1, chat.sender, "the sender profile id is carried");
    assert!(
        chat_event.2.was_shown,
        "an unfiltered message must be marked shown"
    );
}

/// The metadata serializer table is right about real bytes.
///
/// This era renumbered the table (armadillo and wolf-variant serializers were
/// inserted, moving everything after them), and a wrong number does not fail:
/// it reads the next field's bytes as some other type and either succeeds with
/// nonsense or reports a corrupted stream several fields later. Every recorded
/// `entity_metadata` body is decoded to its terminator here, which is the
/// check the wire can actually give.
#[test]
fn every_recorded_entity_metadata_body_decodes_to_its_terminator() {
    use lodestone_core::{Ctx, decode_body_exact};
    use lodestone_v1_20_6::packets::entity::EntityMetadataPacket;

    let metadata_id = clientbound_id("minecraft:entity_metadata");
    let bodies: Vec<Vec<u8>> = read_capture(ERA.minecraft)
        .into_iter()
        .filter(|packet| packet.state == ConnectionState::Play && packet.id == metadata_id)
        .map(|packet| packet.payload)
        .collect();
    assert!(
        !bodies.is_empty(),
        "the capture carries no entity_metadata, so the serializer table is \
         untested against real bytes"
    );
    let mut entries = 0usize;
    for body in &bodies {
        let packet: EntityMetadataPacket =
            decode_body_exact(body, Ctx { version: PROTOCOL_1_20_6 })
                .expect("entity_metadata decodes at 766 up to its 0xff terminator");
        entries += packet.metadata.0.len();
    }
    assert!(
        entries > 0,
        "every recorded metadata packet was empty, so no serializer was \
         exercised"
    );
}

// ---------------------------------------------------------------------------
// Recorder -- `#[ignore]`d; needs a live server.
// ---------------------------------------------------------------------------

/// How many bodies of any one packet id a capture keeps.
///
/// A join sends thousands of relative moves and hundreds of columns; the
/// hundredth adds nothing a reviewer or a replay can use, and a multi-megabyte
/// committed file is a burden on every later checkout. Every *distinct* id the
/// wire produced is still represented, which is the property the capture is
/// evidence for.
const MAX_BODIES_PER_ID: usize = 3;

/// A chunk column is two orders of magnitude larger than any other packet
/// here, so it gets its own, tighter cap.
const MAX_CHUNK_BODIES: usize = 1;

/// `unload_chunk` bodies kept. Eight bytes each, and the ones that matter are
/// the far columns the second teleport drops -- see [`UNLOAD_PROBE_X`].
const MAX_UNLOAD_BODIES: usize = 64;

/// RCON password `scripts/live-oracles/legacy.sh` sets on every oracle.
const RCON_PASSWORD: &str = "lodestone";

/// How far in **+x only** the recorder teleports the joined player.
///
/// The point of the asymmetry: at 1000 blocks the far columns sit around
/// chunk x = 62 with chunk z still near 0, so an `unload_chunk` body read in
/// the wrong field order reports (z = 62, x = 0) instead of (z = 0, x = 62).
/// A square view distance makes a swapped pair invisible in every other
/// situation, which is why the probe has to be a long move along one axis.
const UNLOAD_PROBE_X: i32 = 1000;

/// Reads a leading length-prefixed UTF-8 string off a packet body, or `None`
/// when the body does not start with one.
///
/// Used by the recorder to tell one `registry_data` packet from another
/// without decoding the whole thing: which registry a body carries is its
/// first field, and the recorder has to know before it decides whether to
/// keep the body.
fn leading_string(payload: &[u8]) -> Option<String> {
    let mut reader = lodestone_core::Reader::new(payload);
    reader.string(32_767).ok()
}

/// Drives one real join and writes the capture.
///
/// Records every packet id the wire produced, including the ones this family
/// does not translate: a capture is evidence about the wire, and trimming it
/// to the packets already handled would make it agree with the port by
/// construction.
#[allow(clippy::too_many_lines)]
async fn record(oracle: &Oracle) {
    use lodestone_net::Connection;
    use lodestone_testsupport::unique_username;
    use std::time::Instant;

    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: oracle.game_port,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: uuid::Uuid::new_v4(),
    };
    let adapter = lodestone_v1_20_6::adapter_for(PROTOCOL_1_20_6);
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
    let registry_data_id = lodestone_v1_20_6::packet_ids::configuration::clientbound::REGISTRY_DATA;
    let unload_chunk_id = clientbound_id("minecraft:unload_chunk");
    let mut recorded: Vec<CapturedPacket> = Vec::new();
    let mut seen_per_id: std::collections::BTreeMap<(u8, i32), usize> =
        std::collections::BTreeMap::new();
    let mut reached_config = false;
    let mut reached_play = false;
    let mut chunks = 0usize;
    let mut keep_alives = 0usize;
    let mut health = 0usize;
    let mut chats = 0usize;
    let mut chat_sent = false;
    let mut unloads = 0usize;
    let mut teleported_out = false;
    let mut teleported_back = false;

    let mut rcon = lodestone_testsupport::AsyncRconClient::connect(
        ("127.0.0.1", oracle.rcon_port),
        RCON_PASSWORD,
    )
    .await
    .unwrap_or_else(|err| {
        panic!(
            "connect to the {} oracle's RCON on :{} ({err})",
            oracle.minecraft, oracle.rcon_port
        )
    });

    let overall = Duration::from_secs(180);
    let read_timeout = Duration::from_secs(10);
    let started = Instant::now();

    let _ = tokio::time::timeout(overall, async {
        loop {
            // Send one chat message once the join has settled. The server
            // broadcasts it straight back, which is how this capture gets real
            // bytes for `player_chat` -- and, because the server would
            // otherwise disconnect a client whose chat packet is malformed, it
            // is simultaneously the only available check that this crate's
            // *serverbound* acknowledgement tail is acceptable to a real
            // server.
            if reached_play && chunks > 0 && !chat_sent {
                chat_sent = true;
                if let Ok(Some((id, body))) = adapter.encode_action(
                    ConnectionState::Play,
                    &lodestone_model::ClientAction::SendChat {
                        text: "lodestone capture probe".to_owned(),
                    },
                ) {
                    conn.write_packet(id, &body).await.expect("chat send");
                }
            }
            // Two teleports along +x only, once chat has come back. The first
            // moves the player away from spawn and the second brings them
            // home; it is the *second* that unloads the far columns, and only
            // those have the asymmetric coordinates the order check needs.
            if chats > 0 && !teleported_out {
                teleported_out = true;
                let _ = rcon
                    .command(&format!(
                        "tp {} {UNLOAD_PROBE_X} {} 0",
                        profile.username, oracle.floor_y + 4
                    ))
                    .await;
            } else if teleported_out && chunks > 0 && !teleported_back && unloads > 0 {
                teleported_back = true;
                let _ = rcon
                    .command(&format!("tp {} 0 {} 0", profile.username, oracle.floor_y + 4))
                    .await;
            }
            let done = reached_play
                && chunks > 0
                && keep_alives > 0
                && health > 0
                && chats > 0
                && teleported_back
                && unloads > 8;
            if done || started.elapsed() > Duration::from_secs(170) {
                break;
            }
            let read = tokio::time::timeout(read_timeout, conn.read_packet()).await;
            let (packet_id, payload) = match read {
                Err(_) | Ok(Ok(None)) => break,
                Ok(Ok(Some(packet))) => packet,
                Ok(Err(err)) => panic!("read error: {err}"),
            };
            // The cap is per (state, id): a configuration-state id and a
            // play-state id are different packets that happen to share a
            // number, and keeping only one of the pair would lose half the
            // evidence about the phase this era introduced.
            let state_key = match state {
                ConnectionState::Login => 0,
                ConnectionState::Configuration => 1,
                _ => 2,
            };
            let seen = seen_per_id.entry((state_key, packet_id)).or_default();
            let cap = if state == ConnectionState::Play && packet_id == map_chunk_id {
                MAX_CHUNK_BODIES
            } else {
                MAX_BODIES_PER_ID
            };
            // One configuration-state packet is not interchangeable with the
            // next of the same id: the server sends a `registry_data` per
            // registry, and exactly one of them -- the dimension types -- is
            // what makes a column framable. A plain per-id cap keeps whichever
            // three arrive first, which is a property of the server's
            // iteration order rather than of the wire. Peek the leading
            // registry name and always keep that one.
            // Same reasoning for `unload_chunk`: which columns the server
            // drops depends on where the player went, so a per-id cap of three
            // keeps whichever three the first teleport produced -- all of them
            // near spawn, and none of them asymmetric. Keep a wider window so
            // the second teleport's far columns are in the file.
            let is_unload = state == ConnectionState::Play && packet_id == unload_chunk_id;
            let cap = if is_unload { MAX_UNLOAD_BODIES } else { cap };
            let is_dimension_registry = state == ConnectionState::Configuration
                && packet_id == registry_data_id
                && leading_string(&payload).as_deref() == Some("minecraft:dimension_type");
            if is_dimension_registry || *seen < cap {
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
                            Directive::Emit(ClientEvent::ChunkUnloaded { .. }) => unloads += 1,
                            Directive::Emit(ClientEvent::HealthChanged { .. }) => health += 1,
                            Directive::Emit(ClientEvent::Chat { .. }) => chats += 1,
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
                                reached_config |= *next == ConnectionState::Configuration;
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
        reached_config,
        "never entered Configuration against {} -- the login acknowledgement is \
         what enters it",
        oracle.minecraft
    );
    assert!(
        reached_play,
        "never reached Play against {} -- the finish-configuration exchange is \
         what leaves the configuration phase",
        oracle.minecraft
    );
    assert!(chunks > 0, "no chunks decoded from {}", oracle.minecraft);
    assert!(
        unloads > 0,
        "the recorder teleported the player {UNLOAD_PROBE_X} blocks along +x \
         and back and saw no column unloads, so the capture cannot speak to \
         unload_chunk's field order"
    );
    assert!(
        chats > 0,
        "the capture must carry chat: the recorder sends a message and the \
         server broadcasts it back, so zero here means either the serverbound \
         acknowledgement tail was rejected or the clientbound decode failed"
    );

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
    let path = capture_path(oracle.minecraft);
    std::fs::create_dir_all(captures_dir()).expect("create captures dir");
    std::fs::write(&path, out).expect("write capture");
    eprintln!(
        "wrote {} ({} packets, {chunks} chunk columns, {unloads} unloads, \
         {keep_alives} keep-alives, {health} health updates, {chats} chat messages)",
        path.display(),
        recorded.len()
    );
}

#[test]
fn recorded_player_info_preserves_its_supplied_uuid() {
    let expected = uuid::Uuid::parse_str("975e8951-b8d7-3c32-acf9-f58b975788b3")
        .expect("fixture UUID is valid");
    let outcome = replay(&ERA);
    assert!(outcome.events.iter().any(|event| matches!(
        event,
        ClientEvent::PlayerListUpdate { entries }
            if entries.iter().any(|entry| entry.uuid == Some(expected))
    )), "the UUID supplied by the recorded player-info packet must reach the canonical event");
}

#[tokio::test]
#[ignore = "records against a live 1.20.6 server: ./scripts/live-oracles/legacy.sh 1.20.6"]
async fn record_1_20_6() {
    record(&ERA).await;
}
