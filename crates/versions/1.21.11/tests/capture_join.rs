//! A real-join capture for protocol 774, and the hermetic replay that consumes
//! it.
//!
//! # What this is
//!
//! Two halves that never run together:
//!
//! * a **recorder**, `#[ignore]`d, that joins a real vanilla server started by
//!   `scripts/live-oracles/mc-1-21-11.sh`, records every clientbound packet it
//!   receives (state, id, body) to `tests/captures/join_1_21_11.txt`, and
//!   commits nothing itself;
//! * **replay** tests that run in the default `cargo test`, read the committed
//!   capture, and drive every recorded packet through the real 774 adapter.
//!
//! # Why the split matters here in particular
//!
//! The packet *ids* for this era come from the jar's own packet report, so
//! they are not in question. The packet *shapes* come from `minecraft-data` —
//! a cross-check-grade source, not an authority — and five of them are silent
//! rather than loud when wrong:
//!
//! * **The configuration phase.** The join packet names its dimension by a
//!   registry *index*, and the registry arrives earlier. A recording that
//!   never reaches Play is the only evidence the whole choreography is right.
//! * **The chunk column.** This era's heightmap block is a *typed array* — a
//!   count, then a `(kind, long array)` per entry — where the era below sends
//!   a single named-NBT compound. Both are followed by the section buffer's
//!   own length prefix, so reading the wrong one consumes a plausible number
//!   of bytes.
//! * **`add_entity`'s velocity.** It both moved — from the packet's tail to
//!   just after the position — and changed shape, from three fixed `i16`s to a
//!   packed variable-length form that is *one* byte for a stationary entity.
//!   Neither change errors on its own: the reordering consumes the same bytes
//!   for the same values, and the shape change silently eats the five bytes
//!   after it. See [`add_entity_carries_its_velocity_before_its_angles`] and
//!   the asymmetric summon the recorder performs to tell the orders apart, and
//!   [`every_recorded_spawn_velocity_is_one_tick_of_gravity`] for the
//!   quantisation.
//! * **`player_info_update`'s tail.** The two actions this era adds are a bool
//!   and a varint, and for the values a server sends they are one byte each.
//!   See [`the_player_info_tail_is_list_order_then_hat`].
//! * **`forget_level_chunk`.** Its two coordinates are **z then x**. A swap is
//!   invisible in a square view distance.
//!
//! `decode(encode(x)) == x` is satisfied by two symmetric misunderstandings in
//! every one of those. A recorded body is the authority: bytes a real server
//! actually sent.
//!
//! # One thing the recorder has to do that a decoder does not
//!
//! Answering a teleport with its id is necessary but not sufficient. Until the
//! client also *reports a position of its own* at the new location, the server
//! treats it as still in transit: it unloads every column the client had and
//! then sends nothing further, indefinitely. That is why the recorder replies
//! to an absolute reposition with a movement packet, and it is the difference
//! between a recording that carries the far columns this file's unload check
//! needs and one that waits three minutes for columns that will never come.
//!
//! # Recording
//!
//! ```text
//! ./scripts/live-oracles/mc-1-21-11.sh
//! cargo test -p lodestone-v1-21-11 --test capture_join -- --ignored --nocapture record_1_21_11
//! ```

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use lodestone_data::block_states;
use lodestone_model::{
    ClientEvent, ConnectionState, Directive, GameMode, LoginProfile, ServerAddress, VersionAdapter,
};
use lodestone_v1_21_11::PROTOCOL_1_21_11;
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

/// The recorded server: its Minecraft version, protocol, oracle ports, and the
/// vertical window its own vanilla flat overworld has.
///
/// The ports match `scripts/live-oracles/mc-1-21-11.sh`'s own values; that
/// script is the single place they are defined and this is the single place
/// they are read.
struct Oracle {
    minecraft: &'static str,
    protocol: i32,
    game_port: u16,
    /// RCON port, used by the recorder to teleport the joined player far
    /// enough to force column unloads — see [`UNLOAD_PROBE_X`] — and to summon
    /// the probe entity.
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
    minecraft: "1.21.11",
    protocol: PROTOCOL_1_21_11,
    game_port: 25604,
    rcon_port: 25605,
    floor_y: -64,
    section_count: 24,
};

/// The yaw the recorder summons its probe entity with, in degrees.
///
/// Chosen so the wire's signed-byte angle is a value that cannot coincide with
/// anything else in the packet: `90 / 360 * 256 = 64`, i.e. the single byte
/// `0x40`, while every velocity component and both other angles are zero. The
/// two candidate field orders therefore disagree observably — see
/// [`add_entity_carries_its_velocity_before_its_angles`]. A round number like
/// `0` or `180` would make both orders agree.
const PROBE_YAW_DEGREES: i32 = 90;

/// That yaw as this era's signed-byte angle. Written as the arithmetic rather
/// than as `64` so the relationship is checkable.
const PROBE_YAW_BYTE: i8 = ((PROBE_YAW_DEGREES * 256) / 360) as i8;

/// Where the recorder summons the probe entity, in world coordinates.
///
/// Written with explicit fractions because a command coordinate given as a
/// bare integer is a *block* coordinate and lands the entity at that block's
/// horizontal centre; spelling the halves out makes the summoned position and
/// the position asserted below the same literal rather than two values related
/// by a convention. `y` is the floor plus four, so the probe stands clear of
/// the flat preset's blocks.
const PROBE_X: f64 = 3.5;
/// See [`PROBE_X`].
const PROBE_Z: f64 = 0.5;

/// The probe's expected position, which is decoded *before* the fields whose
/// order is in question and is therefore the right way to find it in a
/// capture.
fn probe_position(oracle: &Oracle) -> (f64, f64, f64) {
    (PROBE_X, f64::from(oracle.floor_y + 4), PROBE_Z)
}

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
    lodestone_v1_21_11::packet_ids::play::clientbound::ENTRIES
        .iter()
        .find(|(entry, _)| *entry == name)
        .map(|(_, id)| *id)
        .unwrap_or_else(|| panic!("protocol 774 carries no {name}"))
}

/// The canonical 26.2 state id for a block with the given properties.
///
/// Resolved out of `lodestone_data::block_states` — jar-derived, and nothing to
/// do with this crate's own tables — so an expected value below originates
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

mod replay;
mod recording;
