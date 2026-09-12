use super::*;

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
