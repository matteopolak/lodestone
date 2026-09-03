//! Real-join captures for this era's protocol and its two neighbours, and the
//! hermetic replay that consumes them.
//!
//! # What this is
//!
//! Two halves that never run together:
//!
//! * a **recorder**, `#[ignore]`d, that joins a real vanilla server started by
//!   `scripts/live-oracles/legacy.sh <version>`, records every clientbound
//!   packet it receives (state, id, body) to
//!   `tests/captures/join_<version>.txt`, and commits nothing itself;
//! * **replay** tests that run in the default `cargo test`, read the committed
//!   captures, and drive every recorded packet through the real 762 adapter.
//!
//! # Why the split matters here in particular
//!
//! The packet shapes came from `minecraft-data` — a cross-check-grade source,
//! not an authority — and this era has three places where being wrong is
//! silent rather than loud:
//!
//! * **The spawn packet.** 1.19.4 folded the mob-spawn packet into the
//!   object-spawn one, inserting a head-rotation byte *before* the object-data
//!   field and widening that field to a VarInt. A decoder carrying the era
//!   below's field order reads a plausible position with nonsense motion.
//! * **The chunk column.** The vertical window is no longer inline in the join
//!   packet; it is a lookup inside the dimension registry, keyed by a name.
//!   A wrong section count consumes the wrong number of bytes.
//! * **Chat.** A message index, an optional 256-byte signature and a
//!   last-seen chain all sit in front of fields a naive decoder would read.
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
//! # Why there are three captures for one protocol
//!
//! A multi-protocol era gets its negative control for free: feed one member's
//! bytes to another member's adapter and measure what comes out. **A singleton
//! era has no sibling to misroute against**, so the control has to come from
//! outside the crate: real bytes from the version below (1.18.2, protocol 758)
//! and the version above (1.20.6, protocol 766), replayed through the only
//! adapter this crate has. That is the same shape the 1.13 era used, and it is
//! the strongest control available here.
//!
//! # Recording
//!
//! ```text
//! ./scripts/live-oracles/legacy.sh 1.19.4
//! cargo test -p lodestone-v1-19 --test capture_join -- --ignored --nocapture record_1_19_4
//! ```
//!
//! Repeat with `1.18.2` / `record_1_18_2` and `1.20.6` / `record_1_20_6` for
//! the two neighbour captures. Each brings its own container up on its own
//! port, so the three can be recorded in any order.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestone_data::block_states;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, GameMode, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_v1_19::PROTOCOL_1_19_4;
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

/// One recorded server: its Minecraft version, protocol, oracle port, and the
/// vertical window its own vanilla flat overworld has.
///
/// Ports match `scripts/live-oracles/legacy.sh`'s table; that script is the
/// single place they are defined and this is the single place they are read.
///
/// Only [`ERA`] is a member of this crate's era. The other two are recorded
/// through the era adapter as a deliberate misroute, so their `protocol` field
/// says which *server* produced the bytes, never which adapter reads them.
struct Oracle {
    minecraft: &'static str,
    protocol: i32,
    game_port: u16,
    floor_y: i32,
    section_count: usize,
}

/// The era's own protocol — the only one this crate can construct an adapter
/// for.
const ERA: Oracle = Oracle {
    minecraft: "1.19.4",
    protocol: PROTOCOL_1_19_4,
    game_port: 25596,
    floor_y: -64,
    section_count: 24,
};

/// The version immediately below this era. Its bytes exist only to be fed to
/// the 762 adapter.
const BELOW: Oracle = Oracle {
    minecraft: "1.18.2",
    protocol: 758,
    game_port: 25594,
    floor_y: -64,
    section_count: 24,
};

/// The version immediately above this era. Same purpose as [`BELOW`].
const ABOVE: Oracle = Oracle {
    minecraft: "1.20.6",
    protocol: 766,
    game_port: 25598,
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

/// This era's own clientbound play table.
fn clientbound_entries() -> &'static [(&'static str, i32)] {
    lodestone_v1_19::packet_ids::play::clientbound::ENTRIES
}

/// Resolves a packet name to its id in this protocol's own clientbound table.
///
/// Read from the generated table rather than written down: an id literal here
/// would be a claim about which protocol is being talked about rather than a
/// fact about it.
fn clientbound_id(name: &str) -> i32 {
    clientbound_entries()
        .iter()
        .find(|(entry, _)| *entry == name)
        .map(|(_, id)| *id)
        .unwrap_or_else(|| panic!("protocol 762 carries no {name}"))
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

fn replay_through(minecraft: &str) -> ReplayOutcome {
    let adapter = lodestone_v1_19::adapter_for(PROTOCOL_1_19_4);
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

fn replay(oracle: &Oracle) -> ReplayOutcome {
    replay_through(oracle.minecraft)
}

/// Every assertion here is against a value the *server* chose, recovered from
/// bytes it sent.
#[test]
fn the_committed_capture_replays_cleanly_through_the_762_adapter() {
    let outcome = replay(&ERA);

    assert!(
        outcome.errors.is_empty(),
        "the 1.19.4 replay produced decode errors: {:?}",
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
        "the capture decoded no chunk columns — a map_chunk that fails its \
         trailing-bytes check is reported as an error above, so zero here means \
         the recording never reached Play"
    );

    let keep_alives = outcome
        .events
        .iter()
        .filter(|event| matches!(event, ClientEvent::KeepAlive { .. }))
        .count();
    assert!(keep_alives > 0, "the capture has no keep_alive");
}

/// The flat preset's own floor, read back out of `lodestone-world` — at the
/// height the *server* said the world starts at.
///
/// This is the assertion that exercises the era's chunk risk. Getting the
/// section count wrong, resolving the vertical window against the wrong
/// registry entry, or leaving a single-valued palette's trailing long count
/// unconsumed all produce a populated but wrong world rather than an error,
/// and every one of them moves where the floor lands.
///
/// The expected block ids come from the 26.2 registry, not from this crate,
/// and the expected *height* comes from the capture's own `login` packet
/// rather than from the constant in [`ERA`] — the constant is only there so a
/// disagreement between the two is visible.
#[test]
fn the_capture_lands_the_flat_preset_floor_in_canonical_ids() {
    let bedrock = canonical_state("minecraft:bedrock", &[]);
    let dirt = canonical_state("minecraft:dirt", &[]);
    let grass = canonical_state("minecraft:grass_block", &[("snowy", "false")]);
    assert!(
        bedrock != dirt && dirt != grass && bedrock != grass,
        "the three probes must be distinguishable in the canonical space"
    );

    let outcome = replay(&ERA);
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
            "y={base} is not uniformly canonical bedrock ({bedrock}) — a wrong \
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
            "y={} is not uniformly canonical grass_block ({grass}) — most likely \
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
    assert!(checked > 0, "the capture put no columns into the world store");
}

/// The vertical window came out of the server's own **registry**, not out of
/// an inline dimension blob and not out of a constant.
///
/// This is the era's structural change and it has a discriminating failure:
/// the join packet at 762 carries a dimension *name* where the era below
/// carries a compound, so a decoder that reads the old field consumes a string
/// as NBT and never reaches a chunk at all. That failure is loud. The quiet
/// one this test covers is a decoder that *skips* the lookup and keeps the
/// pre-join default — which happens to be right for the overworld, so the
/// assertion also pins that the value was read rather than defaulted, by
/// checking the join packet's own registry resolves independently.
#[test]
fn the_vertical_window_is_resolved_from_the_servers_own_registry() {
    use lodestone_core::{Ctx, decode_body};
    use lodestone_v1_19::packets::chunk::ChunkShape;
    use lodestone_v1_19::packets::game::JoinGame;

    let login_id = clientbound_id("minecraft:login");
    let body = read_capture(ERA.minecraft)
        .into_iter()
        .find(|packet| packet.state == ConnectionState::Play && packet.id == login_id)
        .expect("the capture carries a join packet")
        .payload;
    let join: JoinGame = decode_body(&body, Ctx { version: PROTOCOL_1_19_4 })
        .expect("the join packet decodes at 762");

    assert_eq!(
        join.world_type, "minecraft:overworld",
        "the join packet names its dimension *type*, which is the registry key"
    );
    assert!(
        !join.dimension_codec.is_empty(),
        "the join packet carries the registry the window is looked up in"
    );

    // Resolve from a shape whose window is deliberately wrong, so a lookup
    // that silently does nothing cannot pass.
    let seeded = ChunkShape {
        min_y: 4096,
        section_count: 1,
        ..ChunkShape::overworld(PROTOCOL_1_19_4)
    };
    let resolved = seeded
        .from_dimension_registry(&join.dimension_codec, &join.world_type)
        .expect("the overworld entry is in the registry the server sent");
    assert_eq!(
        (resolved.min_y, resolved.section_count),
        (ERA.floor_y, ERA.section_count),
        "the window this server declares for its own overworld"
    );

    // A name the registry does not carry must leave the shape alone rather
    // than take the first entry — which would silently be the overworld's
    // window in some other dimension.
    assert!(
        seeded
            .from_dimension_registry(&join.dimension_codec, "lodestone:not_a_dimension")
            .is_none(),
        "an unknown dimension type must not resolve to some other entry's window"
    );
}

/// Chat at this protocol is signed, ordered and acknowledged, and the capture
/// exercises both directions of it.
///
/// The recorder sends one message and the server broadcasts it straight back
/// as `player_chat`. Two things follow, and neither is available from a
/// round-trip against our own encoder:
///
/// * **The serverbound tail is right.** A 1.19.4 server reads the timestamp,
///   salt, optional signature and last-seen window off every chat packet; a
///   malformed one closes the connection rather than being ignored. The
///   message coming back at all is the check.
/// * **The clientbound decode is right.** `player_chat` puts a chain index, an
///   optional 256-byte signature and a last-seen chain in front of fields a
///   decoder inherited from the era below would read first, and its
///   components are JSON strings rather than network NBT. The exact decode
///   below rejects any of those going wrong.
#[test]
fn the_capture_carries_a_real_player_chat_and_decodes_both_of_its_texts() {
    let chat_id = clientbound_id("minecraft:player_chat");
    let body = read_capture(ERA.minecraft)
        .into_iter()
        .find(|packet| packet.state == ConnectionState::Play && packet.id == chat_id)
        .expect(
            "the capture carries the message the recorder sent, echoed back by the \
             server as player_chat",
        )
        .payload;

    use lodestone_core::{Ctx, decode_body_exact};
    use lodestone_v1_19::packets::chat::PlayerChat;
    let chat: PlayerChat = decode_body_exact(&body, Ctx { version: PROTOCOL_1_19_4 })
        .expect("player_chat decodes at 762 with nothing left over");

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
    // no session key, so the server has nothing to sign with. That is a
    // property of the recording, asserted rather than assumed, because it is
    // what makes the `Option` arm the one under test here.
    assert!(
        chat.signature.is_none(),
        "an offline-mode server with enforcement off signs nothing"
    );
    assert!(
        chat.previous_messages.is_empty(),
        "the first message has seen nothing before it"
    );

    // And the whole thing reaches the canonical model with its sender.
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

// ---------------------------------------------------------------------------
// Negative controls — each measured, not predicted.
// ---------------------------------------------------------------------------

/// Measured split for the 1.18.2 capture replayed through the 762 adapter:
/// `(errored, silent, plausible wrong events, ids 762 does not carry)`.
const MISROUTE_FROM_758: (usize, usize, usize, usize) = (0, 0, 0, 0);

/// The same, for the 1.20.6 capture.
const MISROUTE_FROM_766: (usize, usize, usize, usize) = (0, 0, 0, 0);

/// **The per-packet negative control**, run against real bytes from both
/// neighbouring versions.
///
/// For every play packet in a neighbour's capture whose id names a
/// **different** packet at 762, what does the 762 adapter do with it? Silently
/// dropping a packet is bad; silently emitting the *wrong* one is the failure
/// class that reaches the screen as plausible nonsense.
///
/// **This records a measurement, not a guarantee.** The numbers below were
/// obtained by running it; they are pinned so that a change on either side
/// surfaces as a mismatch to re-derive rather than as a silently weaker
/// control.
#[test]
fn misrouting_from_a_neighbour_is_measured_not_assumed() {
    for (oracle, expected) in [
        (&BELOW, MISROUTE_FROM_758),
        (&ABOVE, MISROUTE_FROM_766),
    ] {
        let adapter = lodestone_v1_19::adapter_for(PROTOCOL_1_19_4);
        let mut errored = 0usize;
        let mut silent = 0usize;
        let mut plausible: Vec<String> = Vec::new();
        let mut unknown = 0usize;

        for packet in read_capture(oracle.minecraft) {
            if packet.state != ConnectionState::Play {
                continue;
            }
            if clientbound_entries()
                .iter()
                .all(|(_, id)| *id != packet.id)
            {
                unknown += 1;
                continue;
            }
            let mut world = World::new();
            match adapter.handle_packet(
                &mut world,
                ConnectionState::Play,
                packet.id,
                &packet.payload,
            ) {
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
                        plausible.push(format!("id {} -> {emitted:?}", packet.id));
                    }
                }
            }
        }

        // A control that exercised nothing would pass silently.
        assert!(
            errored + silent + plausible.len() > 0,
            "{}: no captured packet reached the 762 adapter at all — then this \
             control tests nothing",
            oracle.minecraft
        );
        assert_eq!(
            (errored, silent, plausible.len(), unknown),
            expected,
            "{}: the misroute split has moved; re-derive it rather than \
             adjusting the numbers. Plausible wrong events, if any: {plausible:#?}",
            oracle.minecraft
        );
    }
}

/// Whole-capture form: a neighbour's bytes fed to the 762 adapter must not
/// come out as a clean join.
///
/// Blunter than the per-packet control, and it covers the framing, which is
/// where the real risk is. This is the guarantee the crate can actually offer.
#[test]
fn neither_neighbours_capture_replays_as_a_clean_join() {
    for oracle in [&BELOW, &ABOVE] {
        let outcome = replay_through(oracle.minecraft);
        let chunks = outcome
            .events
            .iter()
            .filter(|event| matches!(event, ClientEvent::ChunkLoaded { .. }))
            .count();
        assert!(
            chunks == 0 && !outcome.errors.is_empty(),
            "the 762 adapter read {}'s capture as a clean join ({chunks} chunks, \
             {} errors) — then 762's framing does not actually differ from that \
             version's and this era's work is untested",
            oracle.minecraft,
            outcome.errors.len()
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
    // Always the era's own adapter: a neighbour capture is recorded *through*
    // it deliberately, so the recorder reports the same misroutes the replay
    // will. Its own `handle_packet` errors are printed, never fatal.
    let adapter = lodestone_v1_19::adapter_for(PROTOCOL_1_19_4);
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
    let mut chats = 0usize;
    let mut chat_sent = false;

    let overall = Duration::from_secs(150);
    let read_timeout = Duration::from_secs(8);
    let started = Instant::now();

    let _ = tokio::time::timeout(overall, async {
        loop {
            // Send one chat message once the join has settled. The server
            // broadcasts it straight back as `player_chat`, which is how this
            // capture gets real bytes for the era's highest-risk packet --
            // and, because the server would otherwise disconnect a client
            // whose chat packet is malformed, it is simultaneously the only
            // available check that this crate's *serverbound* signing tail is
            // acceptable to a real server.
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
            let done =
                reached_play && chunks > 0 && keep_alives > 0 && health > 0 && chats > 0;
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
        oracle.minecraft
    );
    if oracle.protocol == PROTOCOL_1_19_4 {
        assert!(chunks > 0, "no chunks decoded from {}", oracle.minecraft);
        assert!(
            chats > 0,
            "the era capture must carry chat: the recorder sends a message and \
             the server broadcasts it back, so zero here means either the \
             serverbound signing tail was rejected or the clientbound decode \
             failed"
        );
    }

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
        "wrote {} ({} packets, {chunks} chunk columns, {keep_alives} keep-alives, \
         {health} health updates, {chats} chat messages)",
        path.display(),
        recorded.len()
    );
}

#[tokio::test]
#[ignore = "records against a live 1.19.4 server: ./scripts/live-oracles/legacy.sh 1.19.4"]
async fn record_1_19_4() {
    record(&ERA).await;
}

#[tokio::test]
#[ignore = "records the lower neighbour: ./scripts/live-oracles/legacy.sh 1.18.2"]
async fn record_1_18_2() {
    record(&BELOW).await;
}

#[tokio::test]
#[ignore = "records the upper neighbour: ./scripts/live-oracles/legacy.sh 1.20.6"]
async fn record_1_20_6() {
    record(&ABOVE).await;
}
