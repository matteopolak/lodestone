use super::*;

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
                                ..
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

#[tokio::test]
#[ignore = "records against a live 1.21.11 server: ./scripts/live-oracles/mc-1-21-11.sh"]
async fn record_1_21_11() {
    record(&ERA).await;
}
