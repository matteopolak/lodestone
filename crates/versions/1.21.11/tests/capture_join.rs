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
    let unload_id = clientbound_id("minecraft:forget_level_chunk");
    let adapter = lodestone_v1_21_11::adapter_for(PROTOCOL_1_21_11);
    let mut world = World::new();
    let mut events = Vec::new();
    let mut errors = Vec::new();
    let packets = read_capture(oracle.minecraft);
    let count = packets.len();

    for packet in packets {
        if stop_at_first_unload && packet.state == ConnectionState::Play && packet.id == unload_id {
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
fn the_committed_capture_replays_cleanly_through_the_774_adapter() {
    let outcome = replay(&ERA);

    assert!(
        outcome.errors.is_empty(),
        "the 1.21.11 replay produced decode errors: {:?}",
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
        "the capture decoded no chunk columns -- a level_chunk_with_light that \
         fails its trailing-bytes check is reported as an error above, so zero \
         here means the recording never reached Play"
    );
}

/// The configuration phase is real, and it is where the dimension registry
/// arrives.
#[test]
fn the_capture_carries_the_configuration_phase_and_its_dimension_registry() {
    use lodestone_core::{Ctx, decode_body};
    use lodestone_v1_21_11::packets::configuration::RegistryData;

    let packets = read_capture(ERA.minecraft);
    let config: Vec<&CapturedPacket> = packets
        .iter()
        .filter(|packet| packet.state == ConnectionState::Configuration)
        .collect();
    assert!(
        !config.is_empty(),
        "a 774 join passes through a configuration state; an empty one means \
         the login acknowledgement never landed"
    );

    let registry_id = lodestone_v1_21_11::packet_ids::configuration::clientbound::REGISTRY_DATA;
    let dimension = config
        .iter()
        .filter(|packet| packet.id == registry_id)
        .map(|packet| {
            decode_body::<RegistryData>(
                &packet.payload,
                Ctx {
                    version: PROTOCOL_1_21_11,
                },
            )
            .expect("registry_data decodes at 774")
        })
        .find(|data| data.registry == "minecraft:dimension_type")
        .expect("the configuration phase delivers the dimension-type registry");

    // Every entry carries a payload, which is what makes the vertical window
    // resolvable — and is a consequence of this client claiming no known packs.
    // An elided entry would arrive with `data: None`.
    assert!(
        !dimension.entries.is_empty(),
        "the dimension-type registry is not empty"
    );
    assert!(
        dimension.entries.iter().all(|entry| entry.data.is_some()),
        "claiming no known packs is what makes the server send every entry's \
         payload; an elided entry leaves the column unframeable"
    );
    assert_eq!(
        dimension.entries[0].id, "minecraft:overworld",
        "index 0 is the entry the captured join packet named"
    );
}

/// The vertical window the adapter ends up with is the server's, not the
/// fallback — and the era's typed heightmap array is read correctly.
///
/// The check that matters is not that the number is 24 (the fallback is also
/// 24) but that the column parses to its last byte. The typed heightmap array
/// sits immediately before the section buffer's own length prefix, so reading
/// the era below's single named-NBT compound instead consumes a plausible
/// number of bytes and then fails *here*, at the trailing-bytes check, rather
/// than producing a short column.
#[test]
fn the_decoded_column_has_the_section_count_the_servers_registry_declares() {
    use lodestone_core::Reader;
    use lodestone_v1_21_11::packets::chunk::{ChunkShape, LevelChunk};

    let chunk_id = clientbound_id("minecraft:level_chunk_with_light");
    let body = read_capture(ERA.minecraft)
        .into_iter()
        .find(|packet| packet.state == ConnectionState::Play && packet.id == chunk_id)
        .expect("the capture carries a chunk column")
        .payload;

    let shape = ChunkShape::overworld(PROTOCOL_1_21_11);
    let mut reader = Reader::new(&body);
    let data = LevelChunk::decode(&mut reader, &shape).expect("the column decodes");
    assert_eq!(data.column.section_count(), ERA.section_count);
    assert_eq!(
        reader.remaining(),
        0,
        "the light payload after the section buffer must parse to the packet's \
         last byte -- a wrong heightmap or section count shows up here, not as \
         a short column"
    );
    assert_eq!(
        data.fallback.out_of_range, 0,
        "every wire state in a vanilla flat column is inside this era's own \
         state range"
    );
}

/// The flat preset's own floor, read back out of `lodestone-world` — at the
/// height the *server* said the world starts at.
///
/// The expected block ids come from Mojang's own 26.2 registry rather than from
/// this crate's table, so both sides of the comparison originate outside the
/// code under test: the bytes from a real server, the meaning from the
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

/// `add_entity` carries its velocity **before** its angle bytes.
///
/// This is the assertion the recorder's asymmetric summon exists for. The two
/// candidate orders — this era's, and the one the 1.20.6 era uses — consume
/// exactly the same bytes, so nothing errors either way and a round trip
/// against this crate's own encoder proves nothing.
///
/// The recorder summons a stationary entity with a yaw of
/// [`PROBE_YAW_DEGREES`], whose wire byte is [`PROBE_YAW_BYTE`], and zero
/// everywhere else. Under this era's order the decode reports zero velocity and
/// that yaw; under the era below's it reports a velocity whose x component is
/// that same number and a yaw of zero. Both are asserted, so the test fails
/// whichever way round it is wrong.
#[test]
fn add_entity_carries_its_velocity_before_its_angles() {
    use lodestone_core::{Ctx, decode_body};
    use lodestone_v1_21_11::packets::entity::AddEntity;

    assert_eq!(
        PROBE_YAW_BYTE, 64,
        "the probe angle must be a byte no other field in the packet is"
    );

    let add_id = clientbound_id("minecraft:add_entity");
    let spawns: Vec<AddEntity> = read_capture(ERA.minecraft)
        .into_iter()
        .filter(|packet| packet.state == ConnectionState::Play && packet.id == add_id)
        .map(|packet| {
            decode_body(
                &packet.payload,
                Ctx {
                    version: PROTOCOL_1_21_11,
                },
            )
            .expect("add_entity decodes at 774")
        })
        .collect();
    assert!(
        !spawns.is_empty(),
        "the capture carries no add_entity, so it cannot speak to the field \
         order -- re-record it"
    );

    // Selected by position, which is decoded before any of the fields in
    // question, so a wrong reading of those fields shows up as a wrong value
    // rather than as a probe that cannot be found.
    let (px, py, pz) = probe_position(&ERA);
    let probe = spawns
        .iter()
        .find(|spawn| spawn.x == px && spawn.y == py && spawn.z == pz)
        .unwrap_or_else(|| {
            panic!(
                "no recorded spawn sits at the probe's position ({px}, {py}, \
                 {pz}); recorded positions: {:?}",
                spawns
                    .iter()
                    .map(|spawn| (spawn.x, spawn.y, spawn.z))
                    .collect::<Vec<_>>()
            )
        });
    assert!(
        probe.velocity.is_zero(),
        "the probe was summoned with no gravity and no motion, so its velocity \
         is the wire's one-byte zero form; {:?} means the bytes after the \
         position were read as something else",
        probe.velocity
    );
    assert_eq!(
        probe.yaw, PROBE_YAW_BYTE,
        "the probe was summoned at {PROBE_YAW_DEGREES} degrees. A yaw of 0 with \
         a non-zero velocity is the era-below reading; a yaw of 0 with a zero \
         velocity means the angles were read from the wrong offset entirely"
    );
    assert_eq!(
        probe.head_pitch, PROBE_YAW_BYTE,
        "an armour stand's head angle follows its body, so it repeats the yaw \
         byte -- and a decoder off by one byte here would report one of them \
         shifted"
    );
    assert_eq!(probe.pitch, 0, "the probe was summoned with a zero pitch");
    assert_eq!(
        probe.object_data, 0,
        "an armour stand carries no type-specific spawn data"
    );
}

/// Every non-zero velocity a real server sent at spawn decodes to exactly one
/// tick of gravity in `y`.
///
/// This is the outside arithmetic that pins the packed velocity's bit layout,
/// and it is not available from this crate at all. Vanilla's falling-entity
/// integration — unchanged since 1.8, and independently implemented in
/// `lodestone-physics` — applies gravity and then vertical air drag, giving
/// `-0.08 * 0.98 = -0.0784` block/tick for an entity in its first tick of
/// fall. Every mob the recorder saw spawn was standing on the flat preset's
/// floor and reports exactly that.
///
/// Why it discriminates: the three components sit at bit offsets 3, 18 and 33
/// of a 48-bit word whose low bits carry a shared magnitude. Reading the
/// components in the wrong order, at the wrong offsets, or with the magnitude
/// mis-decoded all leave `y` somewhere else entirely, and there is no
/// symmetric misunderstanding available — the number comes from the physics,
/// not from the codec.
#[test]
fn every_recorded_spawn_velocity_is_one_tick_of_gravity() {
    use lodestone_core::{Ctx, decode_body};
    use lodestone_v1_21_11::packets::entity::AddEntity;

    /// One tick of vanilla fall: base gravity, then vertical air drag.
    const GRAVITY_TICK: f64 = -0.08 * 0.98;
    /// The packed form quantises `[-1, 1]` over 32766 steps, so a single step
    /// at magnitude 1 is this wide. The assertion below is inside one step and
    /// outside two, which is what makes it a prediction rather than a
    /// direction.
    const STEP: f64 = 2.0 / 32766.0;

    let add_id = clientbound_id("minecraft:add_entity");
    let velocities: Vec<_> = read_capture(ERA.minecraft)
        .into_iter()
        .filter(|packet| packet.state == ConnectionState::Play && packet.id == add_id)
        .map(|packet| {
            decode_body::<AddEntity>(
                &packet.payload,
                Ctx {
                    version: PROTOCOL_1_21_11,
                },
            )
            .expect("add_entity decodes at 774")
            .velocity
        })
        .filter(|velocity| !velocity.is_zero())
        .collect();
    assert!(
        !velocities.is_empty(),
        "every recorded spawn had a zero velocity, so the packed form's bit \
         layout is untested -- the capture needs a spawn of something subject \
         to gravity"
    );
    for velocity in &velocities {
        let error = (velocity.y - GRAVITY_TICK).abs();
        assert!(
            error < STEP,
            "a recorded spawn velocity's y is {} , which is {error} away from \
             one tick of gravity ({GRAVITY_TICK}) -- more than the {STEP} \
             quantisation step, so the packed layout is being read wrongly",
            velocity.y
        );
        assert!(
            velocity.x.abs() < 1.0 && velocity.z.abs() < 1.0,
            "a horizontal component of {velocity:?} exceeds a block per tick, \
             which no walking mob does"
        );
    }
}

/// `player_info_update`'s two new actions are **not** read in bit order: the
/// list-order priority comes before the hat flag.
///
/// The two are a bool and a varint, one byte each for the values a server
/// sends, so swapping them costs no length and raises no error.
/// `minecraft-data` lists the fields in one order and assigns their bits in
/// the other, so a real-bytes check is necessary rather than merely nice.
///
/// The discriminator is a session whose skin flags turn the hat **on**: a
/// vanilla server then sends priority `0` and hat `true`, so the two bytes are
/// `[0x00][0x01]` and the two readings disagree about which is which. The
/// second half of the test is the control — it decodes the same recorded bytes
/// the other way round and requires the result to contradict, so a passing
/// first half cannot be a detector that would accept either order.
#[test]
fn the_player_info_tail_is_list_order_then_hat() {
    use lodestone_core::{Ctx, decode_body_exact};
    use lodestone_v1_21_11::packets::player_info::{PlayerInfoUpdate, action};

    let info_id = clientbound_id("minecraft:player_info_update");
    let updates: Vec<PlayerInfoUpdate> = read_capture(ERA.minecraft)
        .into_iter()
        .filter(|packet| packet.state == ConnectionState::Play && packet.id == info_id)
        .map(|packet| {
            decode_body_exact(
                &packet.payload,
                Ctx {
                    version: PROTOCOL_1_21_11,
                },
            )
            .expect("player_info_update decodes at 774 with nothing left over")
        })
        .collect();
    assert!(
        !updates.is_empty(),
        "the capture carries no player_info_update -- re-record it"
    );

    // Both bits *and* an entry: a real server sends action-carrying updates
    // with an empty entry list (a batch it had nothing left to say about), and
    // those describe no player at all.
    let entry = updates
        .iter()
        .filter(|update| {
            update.has_action(action::UPDATE_HAT) && update.has_action(action::UPDATE_LIST_ORDER)
        })
        .find_map(|update| update.entries.first())
        .unwrap_or_else(|| {
            panic!(
                "no recorded update both sets this era's two new action bits and \
                 describes a player, so the tail order is untested. Recorded \
                 (mask, entries): {:?}",
                updates
                    .iter()
                    .map(|u| (u.actions, u.entries.len()))
                    .collect::<Vec<_>>()
            )
        });
    // The recorder announces every skin part, hat included, so the server
    // reports the hat as shown, and a vanilla server assigns no list priority.
    assert_eq!(
        entry.show_hat,
        Some(true),
        "the recorder's client information turns the hat layer on; `false` here \
         means the bool was read from the priority byte"
    );
    assert_eq!(
        entry.list_order,
        Some(0),
        "a vanilla server assigns no list priority; `1` here means the varint \
         was read from the hat byte"
    );

    // The control. Nothing above proves the assertions can *fail*: if the two
    // recorded bytes were equal, both readings would satisfy them. Read the
    // same tail the other way round and require the opposite pair, so the two
    // orders are demonstrably distinguishable in these exact bytes.
    let tail = recorded_player_info_tail();
    assert_eq!(
        tail,
        [0x00, 0x01],
        "the discriminator rests on the two tail bytes differing; {tail:?} \
         cannot tell the orders apart and the capture needs re-recording with a \
         hat-enabled session"
    );
}

/// The last two bytes of the recorded `player_info_update` entry that carries
/// both of this era's new actions.
///
/// Extracted positionally rather than through the decoder, so the control
/// above is independent of the reading it is checking.
fn recorded_player_info_tail() -> [u8; 2] {
    use lodestone_v1_21_11::packets::player_info::action;

    let info_id = clientbound_id("minecraft:player_info_update");
    let both = (1u8 << action::UPDATE_HAT) | (1u8 << action::UPDATE_LIST_ORDER);
    let body = read_capture(ERA.minecraft)
        .into_iter()
        .find(|packet| {
            packet.state == ConnectionState::Play
                && packet.id == info_id
                && packet.payload.first().is_some_and(|mask| mask & both == both)
                && packet.payload.len() > 20
        })
        .expect("a recorded update sets both new actions and describes a player")
        .payload;
    let tail = &body[body.len() - 2..];
    [tail[0], tail[1]]
}

/// `login_start` at 774 carries a **required** profile UUID.
#[test]
fn the_login_packet_carries_a_required_profile_uuid() {
    use lodestone_core::{Ctx, encode_body};
    use lodestone_v1_21_11::packets::login::LoginStart;

    let start = LoginStart {
        username: "lodestone".to_owned(),
        uuid: uuid::Uuid::nil(),
    };
    let bytes = encode_body(
        &start,
        Ctx {
            version: PROTOCOL_1_21_11,
        },
    )
    .expect("login_start encodes");
    // 1 length byte + 9 name bytes + 16 uuid bytes.
    assert_eq!(
        bytes.len(),
        26,
        "774 appends sixteen raw uuid bytes after the name: {bytes:?}"
    );
    assert_eq!(&bytes[..10], b"\x09lodestone");
    assert!(
        bytes[10..].iter().all(|byte| *byte == 0),
        "the nil uuid is sixteen zero bytes, with no presence byte in front"
    );
}

/// `forget_level_chunk` carries **z before x**, measured rather than
/// described.
///
/// A square view distance makes a swapped coordinate pair invisible: every
/// column the server drops when a player stands still has `|x|` and `|z|` in
/// the same range. The recorder therefore moves the player [`UNLOAD_PROBE_X`]
/// blocks along **+x only** and back, so the far columns it then drops have a
/// large chunk x and a near-zero chunk z. Reading the pair the other way round
/// reports the two numbers swapped, which this test rejects.
#[test]
fn forget_level_chunk_reads_z_before_x() {
    use lodestone_core::{Ctx, decode_body_exact};
    use lodestone_v1_21_11::packets::chunk::ForgetLevelChunk;

    let unload_id = clientbound_id("minecraft:forget_level_chunk");
    let bodies: Vec<Vec<u8>> = read_capture(ERA.minecraft)
        .into_iter()
        .filter(|packet| packet.state == ConnectionState::Play && packet.id == unload_id)
        .map(|packet| packet.payload)
        .collect();
    assert!(
        !bodies.is_empty(),
        "the capture carries no forget_level_chunk, so it cannot speak to the \
         field order -- re-record it"
    );

    let probe_chunk_x = UNLOAD_PROBE_X / 16;
    let mut far = 0usize;
    let mut misordered = 0usize;
    for body in &bodies {
        let unload: ForgetLevelChunk = decode_body_exact(
            body,
            Ctx {
                version: PROTOCOL_1_21_11,
            },
        )
        .expect("forget_level_chunk is two plain ints");
        if (unload.chunk_x - probe_chunk_x).abs() <= 16 && unload.chunk_z.abs() <= 16 {
            far += 1;
        }
        if (unload.chunk_z - probe_chunk_x).abs() <= 16 && unload.chunk_x.abs() <= 16 {
            misordered += 1;
        }
    }
    assert_eq!(
        misordered, 0,
        "an unload body put the probe's x displacement in chunk_z, which is what \
         a swapped field order looks like"
    );
    assert!(
        far > 0,
        "no unload names a column near chunk x = {probe_chunk_x}; the capture's \
         unloads are all near spawn and the order claim is untested"
    );
}

/// A real `player_chat` decodes, and both of its texts reach the model.
///
/// Three things follow from the message coming back at all, none available
/// from a round trip against this crate's own encoder:
///
/// * **The serverbound tail is right, checksum byte included.** The server
///   reads a timestamp, a salt, an optional signature, a last-seen window and
///   then one checksum byte off every chat packet; a malformed one closes the
///   connection rather than being ignored.
/// * **The leading global index is right.** It is this era's addition and it
///   sits before the sender uuid, so a decoder inherited from the era below
///   reads a counter byte as the first byte of a uuid.
/// * **The chat type is a registry-entry holder.** The wire value is `id + 1`,
///   and the exact decode below rejects the raw-id reading — which would leave
///   a trailing byte.
#[test]
fn the_capture_carries_a_real_player_chat_and_reaches_the_model_with_its_sender() {
    use lodestone_core::{Ctx, decode_body_exact};
    use lodestone_v1_21_11::packets::chat::PlayerChat;

    let chat_id = clientbound_id("minecraft:player_chat");
    let body = read_capture(ERA.minecraft)
        .into_iter()
        .find(|packet| packet.state == ConnectionState::Play && packet.id == chat_id)
        .expect("the capture carries the message the recorder sent, echoed back by the server")
        .payload;

    let chat: PlayerChat = decode_body_exact(
        &body,
        Ctx {
            version: PROTOCOL_1_21_11,
        },
    )
    .expect("player_chat decodes at 774 with nothing left over");
    assert_eq!(
        chat.plain_message, "lodestone capture probe",
        "the signed body is the exact text this client sent"
    );
    assert_eq!(
        chat.index, 0,
        "the first message in a session opens the sender's signing chain at 0"
    );
    assert!(
        chat.global_index >= 0,
        "the per-connection counter is non-negative ({})",
        chat.global_index
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
    assert_eq!(
        chat.chat_type.0, 0,
        "the holder's wire value is `id + 1`, so the plain chat format decodes \
         to registry id 0; a raw-id reading would report 1 and leave the \
         packet's tail one field short"
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
    assert_eq!(
        chat_event.2.global_index, chat.global_index,
        "this era has a real server-global index, so the event carries it \
         rather than the per-sender chain index the era below has to substitute"
    );
    assert!(
        chat_event.2.was_shown,
        "an unfiltered message must be marked shown"
    );
}

/// The metadata serializer table is right about real bytes.
///
/// This era renumbered the table again (the copper-golem and weathering
/// serializers were inserted, and one older entry was dropped), and a wrong
/// number does not fail: it reads the next field's bytes as some other type and
/// either succeeds with nonsense or reports a corrupted stream several fields
/// later. Every recorded `set_entity_data` body is decoded to its terminator
/// here, which is the check the wire can actually give.
#[test]
fn every_recorded_entity_metadata_body_decodes_to_its_terminator() {
    use lodestone_core::{Ctx, decode_body_exact};
    use lodestone_v1_21_11::packets::entity::EntityMetadataPacket;

    let metadata_id = clientbound_id("minecraft:set_entity_data");
    let bodies: Vec<Vec<u8>> = read_capture(ERA.minecraft)
        .into_iter()
        .filter(|packet| packet.state == ConnectionState::Play && packet.id == metadata_id)
        .map(|packet| packet.payload)
        .collect();
    assert!(
        !bodies.is_empty(),
        "the capture carries no set_entity_data, so the serializer table is \
         untested against real bytes"
    );
    let mut entries = 0usize;
    for body in &bodies {
        let packet: EntityMetadataPacket = decode_body_exact(
            body,
            Ctx {
                version: PROTOCOL_1_21_11,
            },
        )
        .expect("set_entity_data decodes at 774 up to its 0xff terminator");
        entries += packet.metadata.0.len();
    }
    assert!(
        entries > 0,
        "every recorded metadata packet was empty, so no serializer was exercised"
    );
}

// ---------------------------------------------------------------------------
// Recorder — `#[ignore]`d; needs a live server.
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

/// `forget_level_chunk` bodies kept. Eight bytes each, and the ones that
/// matter are the far columns the second teleport drops.
const MAX_UNLOAD_BODIES: usize = 64;

/// `add_entity` bodies kept. The probe entity is summoned last, so a cap of
/// three would keep whatever spawned near the player first.
const MAX_SPAWN_BODIES: usize = 32;

/// `player_info_update` bodies kept, for the same reason: the update carrying
/// this era's two new action bits is not necessarily the first one.
const MAX_PLAYER_INFO_BODIES: usize = 16;

/// RCON password `scripts/live-oracles/mc-1-21-11.sh` sets on the oracle.
const RCON_PASSWORD: &str = "lodestone";

/// How far in **+x only** the recorder teleports the joined player.
///
/// The point of the asymmetry: at 1000 blocks the far columns sit around chunk
/// x = 62 with chunk z still near 0, so a `forget_level_chunk` body read in the
/// wrong field order reports (z = 62, x = 0) instead of (z = 0, x = 62). A
/// square view distance makes a swapped pair invisible in every other
/// situation, which is why the probe has to be a long move along one axis.
const UNLOAD_PROBE_X: i32 = 1000;

/// Whether an unload body names a column far enough out that only the
/// recorder's own displacement can explain it — read without deciding which of
/// the two coordinates is which, so this can gate the evidence for that very
/// question.
fn is_far_unload(payload: &[u8]) -> bool {
    if payload.len() < 8 {
        return false;
    }
    let first = i32::from_be_bytes(payload[0..4].try_into().expect("four bytes"));
    let second = i32::from_be_bytes(payload[4..8].try_into().expect("four bytes"));
    first.abs() > 32 || second.abs() > 32
}

/// The chunk x a column packet opens with. Two leading big-endian `i32`s are
/// the one part of this packet nothing disputes.
fn leading_chunk_x(payload: &[u8]) -> Option<i32> {
    payload
        .get(0..4)
        .map(|bytes| i32::from_be_bytes(bytes.try_into().expect("four bytes")))
}

/// Whether a spawn body carries this exact position, matched as the 24 raw
/// bytes of three big-endian `f64`s.
fn body_has_position(payload: &[u8], (x, y, z): (f64, f64, f64)) -> bool {
    let mut needle = [0u8; 24];
    needle[0..8].copy_from_slice(&x.to_be_bytes());
    needle[8..16].copy_from_slice(&y.to_be_bytes());
    needle[16..24].copy_from_slice(&z.to_be_bytes());
    payload.windows(needle.len()).any(|window| window == needle)
}

/// Reads a leading length-prefixed UTF-8 string off a packet body, or `None`
/// when the body does not start with one.
fn leading_string(payload: &[u8]) -> Option<String> {
    let mut reader = lodestone_core::Reader::new(payload);
    reader.string(32_767).ok()
}

/// Drives one real join and writes the capture.
///
/// Records every packet id the wire produced, including the ones this family
/// does not translate: a capture is evidence about the wire, and trimming it to
/// the packets already handled would make it agree with the port by
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
    let adapter = lodestone_v1_21_11::adapter_for(PROTOCOL_1_21_11);
    let mut world = World::new();

    let mut conn = Connection::connect(("127.0.0.1", oracle.game_port))
        .await
        .unwrap_or_else(|err| {
            panic!(
                "connect to the {} oracle on :{} ({err}) -- start it with \
                 ./scripts/live-oracles/mc-1-21-11.sh",
                oracle.minecraft, oracle.game_port
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

    let chunk_id = clientbound_id("minecraft:level_chunk_with_light");
    let registry_data_id =
        lodestone_v1_21_11::packet_ids::configuration::clientbound::REGISTRY_DATA;
    let unload_chunk_id = clientbound_id("minecraft:forget_level_chunk");
    let add_entity_id = clientbound_id("minecraft:add_entity");
    let player_info_id = clientbound_id("minecraft:player_info_update");
    let player_chat_id = clientbound_id("minecraft:player_chat");
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
    let mut spawns = 0usize;
    // Wire-level counters, kept separately from the decoded ones above.
    //
    // Every gate that decides what the recorder does next reads these, because
    // the recorder must not depend on the code it is gathering evidence about:
    // a family whose column decoder is wrong would otherwise never reach the
    // teleport that produces the unload bodies, and the capture would be
    // missing exactly the packets needed to diagnose it.
    let mut play_packets = 0usize;
    let mut raw_unloads = 0usize;
    let mut raw_chats = 0usize;
    // Spawns and unloads seen *after* the probe that makes them
    // discriminating, kept apart from the totals: a mob wandering into view
    // near spawn is an `add_entity` too, and a column dropped on the way *out*
    // is an unload too, so a gate on the totals lets the recorder finish
    // before the packets it went to the trouble of provoking have arrived.
    // That is exactly how this recorder first produced a capture that
    // satisfied every completeness count and settled neither field order.
    // Whether the client still owes the server a "terrain loaded"
    // announcement, which a joining player sends once its columns are in.
    let mut owes_loaded = true;
    // The position the server last teleported this client to, and whether that
    // teleport has been answered with a movement packet.
    //
    // Echoing the teleport id is necessary but **not sufficient**: until the
    // client also reports a position of its own at the new location, the
    // server treats it as still in transit and sends it no further columns. A
    // recorder that only confirms watches all its columns unload and then
    // receives nothing at all, for as long as it is willing to wait — which is
    // how this one first spent three minutes proving nothing.
    let mut owed_move: Option<(lodestone_model::Vec3, lodestone_model::Rotation)> = None;
    let mut probe_spawned = false;
    let mut far_chunk_seen = false;
    let mut discriminating_unloads = 0usize;
    let mut teleported_out = false;
    let mut teleported_back = false;
    let mut summoned = false;

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
            // bytes for `player_chat` — and, because the server would otherwise
            // disconnect a client whose chat packet is malformed, it is
            // simultaneously the only available check that this crate's
            // *serverbound* acknowledgement tail and checksum byte are
            // acceptable to a real server.
            if reached_play && play_packets > 200 && owes_loaded {
                owes_loaded = false;
                if let Ok(Some((id, body))) = adapter
                    .encode_action(ConnectionState::Play, &lodestone_model::ClientAction::PlayerLoaded)
                {
                    conn.write_packet(id, &body).await.expect("player loaded");
                }
            }
            if reached_play && play_packets > 200 && !owes_loaded && !chat_sent {
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
            // moves the player away from spawn and the second brings them home;
            // it is the *second* that unloads the far columns, and only those
            // have the asymmetric coordinates the order check needs.
            if raw_chats > 0 && !teleported_out {
                teleported_out = true;
                owes_loaded = true;
                eprintln!("probe: teleporting out to x = {UNLOAD_PROBE_X}");
                let _ = rcon
                    .command(&format!(
                        "tp {} {UNLOAD_PROBE_X} {} 0",
                        profile.username,
                        oracle.floor_y + 4
                    ))
                    .await;
            } else if teleported_out && !teleported_back && far_chunk_seen {
                teleported_back = true;
                owes_loaded = true;
                eprintln!("probe: teleporting home");
                let _ = rcon
                    .command(&format!(
                        "tp {} 0 {} 0",
                        profile.username,
                        oracle.floor_y + 4
                    ))
                    .await;
            } else if teleported_back && !summoned {
                // The spawn-order probe: a stationary entity three blocks from
                // the player with a yaw whose wire byte is a value no other
                // field of the packet carries. See
                // `add_entity_carries_its_velocity_before_its_angles`.
                summoned = true;
                eprintln!("probe: summoning the spawn-order probe");
                let (px, py, pz) = probe_position(oracle);
                let _ = rcon
                    .command(&format!(
                        "summon minecraft:armor_stand {px} {py} {pz} \
                         {{Rotation:[{PROBE_YAW_DEGREES}f,0f],NoGravity:1b}}"
                    ))
                    .await;
            }
            if let Some((pos, rotation)) = owed_move.take()
                && let Ok(Some((id, body))) = adapter.encode_action(
                    ConnectionState::Play,
                    &lodestone_model::ClientAction::Move {
                        pos,
                        rotation,
                        on_ground: true,
                        horizontal_collision: false,
                    },
                )
            {
                conn.write_packet(id, &body).await.expect("move");
            }
            let done = reached_play
                && keep_alives > 0
                && health > 0
                && teleported_back
                && summoned
                && probe_spawned
                && discriminating_unloads > 4;
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
            let in_play = state == ConnectionState::Play;
            if in_play {
                play_packets += 1;
                if packet_id == unload_chunk_id {
                    raw_unloads += 1;
                    // "Discriminating" without assuming which coordinate is
                    // which: a body naming a column this far out can only have
                    // come from the outbound leg's displacement, whichever of
                    // its two fields carries it.
                    if teleported_back && is_far_unload(&payload) {
                        discriminating_unloads += 1;
                    }
                } else if packet_id == add_entity_id {
                    // Found by the 24 position bytes, which precede every field
                    // whose order the capture exists to settle — so the
                    // recorder's completeness gate does not rest on the reading
                    // it is gathering evidence for.
                    probe_spawned |= body_has_position(&payload, probe_position(oracle));
                } else if packet_id == player_chat_id {
                    raw_chats += 1;
                } else if packet_id == chunk_id
                    && !far_chunk_seen
                    && let Some(x) = leading_chunk_x(&payload)
                {
                    far_chunk_seen = (x - UNLOAD_PROBE_X / 16).abs() <= 8;
                    if far_chunk_seen {
                        eprintln!("probe: the far columns arrived (chunk x {x})");
                    }
                }
            }
            let cap = if in_play && packet_id == chunk_id {
                MAX_CHUNK_BODIES
            } else if in_play && packet_id == unload_chunk_id {
                // Only the columns dropped on the way *back* have the
                // asymmetric coordinates the order check needs; the ones
                // dropped on the way out are the spawn area, whose chunk x and
                // z are both near zero and therefore identical under either
                // reading. Capping without this filter fills the budget with
                // exactly the bodies that cannot discriminate.
                if teleported_back && is_far_unload(&payload) {
                    MAX_UNLOAD_BODIES
                } else {
                    0
                }
            } else if in_play && packet_id == add_entity_id {
                MAX_SPAWN_BODIES
            } else if in_play && packet_id == player_info_id {
                MAX_PLAYER_INFO_BODIES
            } else {
                MAX_BODIES_PER_ID
            };
            // One configuration-state packet is not interchangeable with the
            // next of the same id: the server sends a `registry_data` per
            // registry, and exactly one of them — the dimension types — is what
            // makes a column framable. A plain per-id cap keeps whichever three
            // arrive first, which is a property of the server's iteration order
            // rather than of the wire. Peek the leading registry name and always
            // keep that one.
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
                            Directive::Emit(ClientEvent::EntitySpawned { .. }) => spawns += 1,
                            Directive::Emit(ClientEvent::TeleportPlayer {
                                pos,
                                rotation,
                                flags,
                            }) => {
                                // Only an absolute reposition needs answering;
                                // a relative correction leaves the client
                                // where it already reported itself to be.
                                if !flags.relative_x && !flags.relative_z {
                                    owed_move = Some((*pos, *rotation));
                                }
                            }
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

    // The capture is written before the assertions below, deliberately: a
    // recording that reached the wire and then failed a completeness check is
    // exactly when its bytes are most useful, and a recorder that discards them
    // on the way out forces another three-minute join to see them. The
    // assertions still fail the test, so a short capture is never mistaken for a
    // good one.
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# lodestone clientbound join capture -- Minecraft {} (protocol {})",
        oracle.minecraft, oracle.protocol
    );
    let _ = writeln!(
        out,
        "# recorded by tests/capture_join.rs against ./scripts/live-oracles/mc-1-21-11.sh"
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
         {keep_alives} keep-alives, {health} health updates, {chats} chat \
         messages, {spawns} entity spawns)",
        path.display(),
        recorded.len()
    );

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
        "the recorder teleported the player {UNLOAD_PROBE_X} blocks along +x and \
         back and saw no column unloads, so the capture cannot speak to \
         forget_level_chunk's field order"
    );
    assert!(
        chats > 0,
        "the capture must carry chat: the recorder sends a message and the \
         server broadcasts it back, so zero here means either the serverbound \
         acknowledgement tail was rejected or the clientbound decode failed"
    );
    assert!(
        probe_spawned,
        "no add_entity named the probe's position {:?}, so the capture cannot \
         speak to that packet's field order ({spawns} spawns seen in total)",
        probe_position(oracle)
    );
    assert!(
        discriminating_unloads > 0,
        "every recorded unload names a column near the origin, so all of them \
         are symmetric under a coordinate swap and the field order is untested \
         ({unloads} unloads in total)"
    );

}

#[test]
fn recorded_player_info_preserves_its_supplied_uuid() {
    let expected = uuid::Uuid::parse_str("682587bf-c8e6-3145-8ed2-55846b34a7d7")
        .expect("fixture UUID is valid");
    let outcome = replay(&ERA);
    assert!(outcome.events.iter().any(|event| matches!(
        event,
        ClientEvent::PlayerListUpdate { entries }
            if entries.iter().any(|entry| entry.uuid == Some(expected))
    )), "the UUID supplied by the recorded player-info packet must reach the canonical event");
}

#[tokio::test]
#[ignore = "records against a live 1.21.11 server: ./scripts/live-oracles/mc-1-21-11.sh"]
async fn record_1_21_11() {
    record(&ERA).await;
}
