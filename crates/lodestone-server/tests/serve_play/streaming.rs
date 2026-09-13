/// **The player-feed integration gate.**
///
/// Every other gate for the perception feed calls `MobSim::set_players`
/// *itself*, so all of them would pass with no producer anywhere — which is
/// exactly the state `nearest_player`/`temptation` start in: seam
/// present, feed present, nothing calling it. This test never touches
/// `set_players`. It drives a real `PLAYER_MOVED` packet through
/// `serve_connection` and asserts the perception arrived, so it fails if the one
/// line in `dispatch_play_packet`'s `PlayerMoved` arm is ever removed.
///
/// Note the `MobHandle` is a real one over a real `ChunkWorld` holding a real
/// mob, not the `MobHandle::default()` every other test in this file uses — the
/// default is an empty sim, which cannot show a mob's perception changing.
#[tokio::test(start_paused = true)]
async fn a_player_moved_packet_feeds_mob_perception_through_the_real_connection() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    // Flat floor with its surface at y=0, and one cow standing on it.
    let mut world = ChunkWorld::new(-4, 24);
    for x in -8..=8 {
        for z in -8..=8 {
            world.set_solid(x, -1, z, true);
        }
    }
    let mobs = MobHandle::new(world);
    let cow_id = mobs.with(|sim| {
        sim.spawn_species(
            lodestone_model::ResourceKey::from_str("minecraft:cow").expect("valid key"),
            lodestone_model::Vec3::new(0.0, 0.0, 0.0),
        )
        .id()
    });

    // Control, before any movement packet: the sim knows of no players, so the
    // cow's perception is empty. Without this the assertions below could be
    // satisfied by a value that was always there.
    mobs.with(MobSim::tick);
    assert_eq!(
        mobs.with(|sim| sim.players().len()),
        0,
        "precondition: no players known before a PLAYER_MOVED arrives"
    );
    assert_eq!(
        mobs.with(|sim| sim.get(cow_id).expect("alive").nearest_player()),
        None,
        "precondition: the cow must perceive no player yet — this is the state \
         LookAtPlayerGoal and TemptGoal were permanently stuck in"
    );

    let conn_mobs = mobs.clone();
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &conn_mobs,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Perceived", 1).await;

    // Put wheat in the selected hotbar slot, then move. Wheat because
    // `cow_food` is exactly `[wheat]`
    // (`.cache/mc/26.2/src/data/minecraft/tags/item/cow_food.json`), so this
    // proves the *held item* crossed the connection too, not just the position.
    // Slot 36 is the first hotbar slot in the player's own menu indexing.
    let wheat = ItemStack::new(
        lodestone_model::ResourceKey::from_str("minecraft:wheat").expect("valid key"),
        1,
    );
    send_creative_slot(&mut client, 36, Some(&wheat)).await;
    send_player_moved(&mut client, 4.0, 0.0, 0.0).await;
    let _ = drain_available(&mut client).await;

    // The producer runs inside the packet handler, so the value is present
    // without needing a tick; a tick is what pushes it into the mob.
    assert_eq!(
        mobs.with(|sim| sim.players().len()),
        1,
        "a PLAYER_MOVED packet must register the player with the mob sim"
    );
    mobs.with(MobSim::tick);

    let cow_sees = mobs.with(|sim| {
        let cow = sim.get(cow_id).expect("alive");
        (cow.nearest_player(), cow.temptation())
    });
    assert_eq!(
        cow_sees.0,
        Some(lodestone_model::Vec3::new(4.0, 0.0, 0.0)),
        "the cow must perceive the player at the position the packet carried"
    );
    assert_eq!(
        cow_sees.1,
        Some(lodestone_model::Vec3::new(4.0, 0.0, 0.0)),
        "and must be tempted, because the held wheat crossed the wire into \
         PlayerPerception::held_item — if this is None but nearest_player is \
         Some, the position is being fed and the inventory read is not"
    );

    drop(client);
    let _ = server.await;
}

// ---------------------------------------------------------------------------
// The join view must arrive nearest-first, encoded as it is
// generated — not raster-order from the far corner after all of it exists.
// ---------------------------------------------------------------------------

/// A column source that counts how many columns have been generated so far, over
/// terrain a world spawn can actually be found in.
///
/// The count is the load-bearing half of this gate. Ordering alone is not
/// enough: generating all 361 columns and *then* encoding them nearest-first
/// would satisfy every ordering assertion while leaving time-to-first-chunk
/// unchanged. So [`ProbeProto`] stamps this counter into each
/// chunk packet, and the assertion is about **how much had been generated when
/// the player's own column reached the wire**.
///
/// # Why there is a floor, and why that is not a convenience
///
/// A bare-air source exercises an invalid-origin path: the spawn *search*
/// finds no solid block anywhere, so every one of the spiral's 121 candidates is
/// invalid and the search walks the whole ±5-chunk box
/// before a single chunk is encoded — measured at **123** columns before the first
/// encode (1 fallback query + 121 spiral + ring 0), which reads exactly like the
/// "generate everything first" ordering, which is not the join path under test.
///
/// So the air fixture was a *world*-species vacuity in the making: it exercised
/// the pathological invalid-origin path, not the one a joining player takes. A
/// solid layer at `y = 8` makes the origin chunk a valid spawn candidate, which is
/// what every real world presents, and the bound in
/// [`check_proximity_stream`] is then about the ring loop again rather than about
/// the spawn search.
struct CountingAirSource {
    generated: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// The Y of [`CountingAirSource`]'s floor. Inside the column's `0..16` extent,
/// and clear of the top so `get_level_respawn_pos`'s downward scan reaches it
/// through air rather than being aborted by a fluid.
const FLOOR_Y: i32 = 8;

impl ChunkSource for CountingAirSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        self.generated
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut column = ChunkColumn::new(0, 16);
        for lx in 0..16 {
            for lz in 0..16 {
                column.set_block(lx, FLOOR_Y, lz, "minecraft:stone");
            }
        }
        column
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this fixture
        // counts generations, not reads.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this fixture
        // counts generations, not reads.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    // No storage: this fixture serves fresh columns and edits are discarded by
    // design (an edit a test needs to survive goes through a source with real
    // retention). Explicit rather than inherited.
    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; edits are discarded by design.
    }
}

/// [`FakeProtocol`] with `encode_chunk` appending the
/// generation counter's current value to the packet, so the test can read
/// "columns generated by the time this chunk was encoded" straight off the wire
/// rather than inferring it.
///
/// Every other method forwards to `FakeProtocol` explicitly rather than relying
/// on the trait defaults — fifteen of them have defaults that silently answer
/// `ServerDirective::None`, which is exactly the failure mode
/// `protocol.rs`'s own boxed-protocol test exists to catch.
struct ProbeProto {
    generated: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ServerProtocol for ProbeProto {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        FakeProtocol.decode(state, packet_id, payload)
    }
    fn login_success(&self, username: &str, uuid: Uuid) -> Vec<ServerDirective> {
        FakeProtocol.login_success(username, uuid)
    }
    fn begin_configuration(&self) -> Vec<ServerDirective> {
        FakeProtocol.begin_configuration()
    }
    fn begin_play(&self, view_radius: i32) -> Vec<ServerDirective> {
        FakeProtocol.begin_play(view_radius)
    }
    fn begin_chunk_batch(&self) -> ServerDirective {
        FakeProtocol.begin_chunk_batch()
    }
    fn encode_chunk(&self, cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(cx);
        w.var_i32(cz);
        w.var_i32(
            self.generated
                .load(std::sync::atomic::Ordering::SeqCst)
                .try_into()
                .expect("generation count fits in i32"),
        );
        ServerDirective::Send {
            packet_id: CHUNK,
            payload: w.as_slice().to_vec(),
        }
    }
    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        FakeProtocol.end_chunk_batch(batch_size)
    }
    fn encode_keep_alive(&self, id: i64) -> ServerDirective {
        FakeProtocol.encode_keep_alive(id)
    }
    fn encode_set_time(&self, game_time: i64, day_time: Option<i64>) -> ServerDirective {
        FakeProtocol.encode_set_time(game_time, day_time)
    }
    fn encode_chunk_cache_center(&self, cx: i32, cz: i32) -> ServerDirective {
        FakeProtocol.encode_chunk_cache_center(cx, cz)
    }
    fn encode_forget_chunk(&self, cx: i32, cz: i32) -> ServerDirective {
        FakeProtocol.encode_forget_chunk(cx, cz)
    }
    fn encode_air_supply_update(&self, air: i32) -> ServerDirective {
        FakeProtocol.encode_air_supply_update(air)
    }
    fn encode_set_health(&self, health: f32, food: i32, saturation: f32) -> ServerDirective {
        FakeProtocol.encode_set_health(health, food, saturation)
    }
    fn encode_change_difficulty(&self, difficulty: Difficulty, locked: bool) -> ServerDirective {
        FakeProtocol.encode_change_difficulty(difficulty, locked)
    }
    fn encode_game_rule_values(&self, entries: &[(String, String)]) -> ServerDirective {
        FakeProtocol.encode_game_rule_values(entries)
    }
}

/// Chebyshev (chess-king) distance from the join centre, `(0, 0)`.
fn chebyshev(cx: i32, cz: i32) -> i32 {
    cx.abs().max(cz.abs())
}

/// Reads the whole join view off the wire, in wire order, tolerating everything
/// else the server sends while it is doing so.
///
/// Returns `(observed, batch_sizes)` — `observed` is
/// `(cx, cz, columns_generated_when_encoded)` for [`check_proximity_stream`], and
/// `batch_sizes` is what each `CHUNK_BATCH_FINISHED` marker *reported*.
///
/// # Why this reads until the complete view is present
///
/// The innermost rings go out inline and the rest streams from `serve_play` beside
/// everything else it
/// sends, in batches of `JOIN_STREAM_BATCH_COLUMNS`. So the two things a
/// positional read could assume — contiguous chunk packets and one begin/end pair
/// around the whole view — do not hold for this streaming protocol.
///
/// What still has to hold is asserted here rather than dropped:
///
/// * every chunk arrives **inside** an open batch (a stray chunk outside a
///   begin/end pair would break a real client's flow-control accounting);
/// * each marker's reported size equals the columns actually inside that batch;
/// * the chunk *order* is preserved, which the caller checks with the same
///   [`check_proximity_stream`] used by the ordering control.
async fn collect_join_chunks<T: Transport>(
    client: &mut Connection<T>,
    expected: usize,
) -> (Vec<(i32, i32, usize)>, Vec<i32>) {
    let mut observed = Vec::with_capacity(expected);
    let mut batch_sizes = Vec::new();
    let mut in_batch = false;
    let mut counted = 0usize;
    while observed.len() < expected {
        let (id, payload) = client
            .read_packet()
            .await
            .expect("read")
            .expect("the server must not close mid-view");
        if id == CHUNK_BATCH_START {
            assert!(!in_batch, "a batch opened inside another batch");
            in_batch = true;
            counted = 0;
        } else if id == CHUNK {
            assert!(in_batch, "a chunk arrived outside a begin/end batch pair");
            let mut r = Reader::new(&payload);
            let cx = r.var_i32().unwrap();
            let cz = r.var_i32().unwrap();
            let at = r.var_i32().unwrap() as usize;
            observed.push((cx, cz, at));
            counted += 1;
        } else if id == CHUNK_BATCH_FINISHED {
            assert!(in_batch, "a batch closed without opening");
            let reported = Reader::new(&payload).var_i32().unwrap();
            assert_eq!(
                reported as usize, counted,
                "the batch marker reported {reported} columns but {counted} chunk packets \
                 arrived in it"
            );
            batch_sizes.push(reported);
            in_batch = false;
        }
    }
    // The tail marker for the batch the last chunk landed in.
    if in_batch {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        assert_eq!(
            id, CHUNK_BATCH_FINISHED,
            "the last chunk's batch must be closed"
        );
        let reported = Reader::new(&payload).var_i32().unwrap();
        assert_eq!(reported as usize, counted);
        batch_sizes.push(reported);
    }
    (observed, batch_sizes)
}

/// The detector, factored out so the **same** code judges the real join and the
/// synthesised incorrect sequence below.
///
/// `observed` is `(cx, cz, columns_generated_when_this_was_encoded)` in wire
/// order. Returns the first violation as an `Err`, so a failure names *which*
/// entry broke *which* rule rather than reporting a bare fraction.
///
/// Three rules, each aimed at one of the three compounding orderings:
///
/// 1. the player's own column `(0, 0)` is encoded **first**;
/// 2. Chebyshev distance from the centre never decreases — terrain grows
///    outward from the player instead of inward from a corner;
/// 3. the first chunk was encoded after **at most two columns** of
///    generation — the "generate everything, then encode" half.
///
/// Rule 3's bound is `2`, not `1`, and the two are itemised rather than rounded:
///
/// | column | why |
/// |---|---|
/// | 1 | the world spawn search resolves the origin column's surface before the batch opens, and reuses that one column for the spiral's `(0, 0)` candidate — `world_spawn::a_valid_origin_column_is_generated_exactly_once` |
/// | 2 | ring 0 asks the source for the same column again; the fixture has no `ChunkStore`, so it is a second generation |
///
/// So 2 is the player's own column plus one infra query, not the full view, and
/// the wrong hypothesis is 361.
///
/// Two near-miss sequences are worth keeping because both fail in the
/// *safe*-looking direction — a number just over the bound reads as a mild
/// ordering violation:
///
/// * generating `(0, 0)` twice makes the honest figure 3 and this bound
///   unreachable;
/// * with an all-air fixture the search finds no valid spawn anywhere and walks
///   all 121 spiral candidates first, for a figure of **123** — which looks like
///   full-view ordering and is not. [`CountingAirSource`] has a floor for exactly
///   that reason.
fn check_proximity_stream(observed: &[(i32, i32, usize)], view_radius: i32) -> Result<(), String> {
    let expected_total = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    if observed.len() != expected_total {
        return Err(format!(
            "expected {expected_total} columns, got {}",
            observed.len()
        ));
    }

    let (cx, cz, generated_at_first) = observed[0];
    if (cx, cz) != (0, 0) {
        return Err(format!(
            "the player's own column must be encoded first; got ({cx}, {cz}) at Chebyshev \
             distance {}",
            chebyshev(cx, cz)
        ));
    }
    if generated_at_first > 2 {
        return Err(format!(
            "the first chunk must be encoded after at most 2 columns of generation \
             (1 for the spawn-surface query plus ring 0); \
             {generated_at_first} columns had already been generated, meaning the whole view \
             is generated before anything is encoded"
        ));
    }

    let mut previous = 0;
    for &(cx, cz, _) in observed {
        let distance = chebyshev(cx, cz);
        if distance < previous {
            return Err(format!(
                "wire order must be non-decreasing in Chebyshev distance from the centre; \
                 ({cx}, {cz}) at distance {distance} follows a column at distance {previous}"
            ));
        }
        previous = distance;
    }

    Ok(())
}

/// **The join-ordering gate.** A real join must stream the view outward from the
/// player's own column, encoding each ring as it is generated.
///
/// `view_radius = 9` deliberately — the shell's own singleplayer value, so this
/// measures the 361-column configuration a player actually joins with rather
/// than a convenient small one.
#[tokio::test]
async fn join_streams_the_view_outward_from_the_players_own_column() {
    let view_radius = 9;
    let expected_chunks = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    let generated = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let (client_end, server_end) = memory_pair();
    let source = CountingAirSource {
        generated: std::sync::Arc::clone(&generated),
    };
    let proto = ProbeProto {
        generated: std::sync::Arc::clone(&generated),
    };

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &proto,
            &source,
            &NoEntities,
            view_radius,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
    });

    let mut client = Connection::new(client_end);
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut w = Writer::default();
    w.string("Spiral");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");
    client.read_packet().await.unwrap().unwrap(); // LOGIN_SUCCESS
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    let (id, _payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, SET_TIME_S2C);

    let (observed, batch_sizes) = collect_join_chunks(&mut client, expected_chunks).await;
    assert_eq!(
        batch_sizes.iter().sum::<i32>(),
        expected_chunks as i32,
        "the batch markers must account for exactly the whole view — no column may be sent \
         outside one, and none counted twice"
    );

    check_proximity_stream(&observed, view_radius).expect("join must stream nearest-first");

    // The set is unchanged, not merely reordered: every column of the square
    // exactly once. A ring enumeration that dropped or duplicated a column
    // would still satisfy the ordering rules above.
    let sent: HashSet<(i32, i32)> = observed.iter().map(|&(cx, cz, _)| (cx, cz)).collect();
    assert_eq!(
        sent,
        square(0, 0, view_radius),
        "the ring walk must cover the same square the raster walk did, with no gaps or repeats"
    );

    drop(client);
    let _ = server.await;
}

/// **The control, and it must fail the assertion above.**
///
/// This synthesises the incorrect sequence — raster order from
/// `(-view_radius, -view_radius)`, with the whole view already generated before
/// the first encode — and requires [`check_proximity_stream`] to reject it.
///
/// It is written out as a literal `cz`-outer/`cx`-inner walk rather than
/// described, because that walk generates the full `Vec`, waits for every column,
/// and only then begins encoding. A detector that passes this is measuring
/// nothing, so each specific violation is checked independently below.
#[test]
fn control_the_old_raster_order_fails_the_proximity_assertion() {
    let view_radius = 9;
    let raster: Vec<(i32, i32)> = (-view_radius..=view_radius)
        .flat_map(|cz| (-view_radius..=view_radius).map(move |cx| (cx, cz)))
        .collect();
    let total = raster.len();
    assert_eq!(total, 361, "the shell's own join view is 361 columns");

    // Every column reports the *full* view as already generated, because that
    // is what "generate all 361, then encode" means.
    let observed: Vec<(i32, i32, usize)> = raster
        .iter()
        .map(|&(cx, cz)| (cx, cz, total))
        .collect();

    let verdict = check_proximity_stream(&observed, view_radius);
    let message = verdict.expect_err(
        "the pre-#453 raster order must be rejected; if this passes, the detector in \
         check_proximity_stream is vacuous and the gate beside it proves nothing",
    );
    assert!(
        message.contains("must be encoded first"),
        "the control must be caught on the player's-own-column rule first, got: {message}"
    );

    // The distance rule must fire independently of the first-column rule, so a
    // Relaxing one rule cannot silently disarm the other. Rotate the
    // raster walk so it *starts* at (0, 0) and check it is still rejected.
    let centre = raster
        .iter()
        .position(|&c| c == (0, 0))
        .expect("the centre column is in the view");
    assert_eq!(
        centre, 180,
        "the player's own column really was item ~180 of 361 in raster order"
    );
    let mut rotated: Vec<(i32, i32, usize)> = vec![(0, 0, 1)];
    rotated.extend(
        raster
            .iter()
            .filter(|&&c| c != (0, 0))
            .map(|&(cx, cz)| (cx, cz, total)),
    );
    let message = check_proximity_stream(&rotated, view_radius)
        .expect_err("raster order after the centre is still not distance-ordered");
    assert!(
        message.contains("non-decreasing in Chebyshev distance"),
        "the distance rule must be what rejects this one, got: {message}"
    );
}

/// **The same three rules, over the arm production actually runs.**
///
/// [`join_streams_the_view_outward_from_the_players_own_column`] calls
/// `serve_connection`, which resolves to `SourceRef::Borrowed`. Every production
/// caller in `crate::integrated` resolves to `SourceRef::Shared`, and the ring loop
/// **branches on that**: `Borrowed` awaits one `generate_columns_parallel` per ring,
/// while `Shared` spawns each of the ring's columns into the blocking pool
/// individually and awaits the handles in ring order. Two different loop bodies,
/// one of them untested: a source can be correct while the test exercises the
/// other transport-resolution branch.
///
/// `serve_connection_shared` is `pub(crate)` and deliberately not re-exported, so
/// this reaches the arm the way the shell does: through the public
/// `IntegratedServer::bind`, over a real loopback socket.
///
/// # Two differences from the `Borrowed` gate, both deliberate
///
/// `bind` wraps the source in a [`crate::ChunkStore`], so ring 0 is a **cache hit**
/// on the column the world-spawn search already generated and
/// `generated_at_first` is `1` rather than `2`. [`check_proximity_stream`]'s bound
/// is `<= 2`, which covers both; asserting `1` here would be pinning the store's
/// presence, which `chunk_store.rs` already owns.
///
/// `bind` also spawns `run_tick_loop` over a 5×5 tick area, which would inflate the
/// counter — except that the first random-tick pass is deferred for 40 ticks
/// (2.0 s), and a 361-column air view served from a store completes long before
/// that. Rule 3 reads only `observed[0]`, so even a slow run cannot be corrupted by
/// tick-loop generation that happens after the first encode.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_shared_arm_streams_the_view_outward_too() {
    let view_radius = 9;
    let expected_chunks = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    let generated = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let server = lodestone_server::IntegratedServer::bind(
        "127.0.0.1:0",
        ProbeProto {
            generated: std::sync::Arc::clone(&generated),
        },
        CountingAirSource {
            generated: std::sync::Arc::clone(&generated),
        },
        view_radius,
    )
    .await
    .expect("bind loopback");
    let addr = server.local_addr().expect("a bound server has an address");

    let mut client = Connection::new(
        tokio::net::TcpStream::connect(addr)
            .await
            .expect("client connects"),
    );
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut w = Writer::default();
    w.string("SharedSpiral");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");
    client.read_packet().await.unwrap().unwrap(); // LOGIN_SUCCESS
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    let (id, _) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, SET_TIME_S2C);

    let (observed, batch_sizes) = collect_join_chunks(&mut client, expected_chunks).await;
    assert_eq!(
        batch_sizes.iter().sum::<i32>(),
        expected_chunks as i32,
        "the batch markers must account for exactly the whole view on this arm too"
    );

    check_proximity_stream(&observed, view_radius)
        .expect("the Shared arm must stream nearest-first as well");

    let sent: HashSet<(i32, i32)> = observed.iter().map(|&(cx, cz, _)| (cx, cz)).collect();
    assert_eq!(
        sent,
        square(0, 0, view_radius),
        "the Shared arm's per-column spawn_blocking fan-out must cover the same square, \
         with no gaps or repeats — awaiting the handles out of ring order would show up here"
    );

    drop(client);
    server.shutdown().await;
}

/// **The join responsiveness gate.** A play packet must be serviced while the
/// join view is still streaming, so interaction does not wait for the full burst.
///
/// A play packet sent the instant the client finishes configuration must be
/// *serviced* — replied to — while the join view is still streaming. This uses
/// the production arm (`SourceRef::Shared`, over a real loopback socket).
///
/// # Why a counter and not a stopwatch
///
/// This is a latency claim, and a wall-clock one would be attributed to the wrong
/// cause: this machine's timings reproduce to ~10.8% and several agents run
/// concurrently. So the instrument is **how many of the view's 361 chunk packets
/// had arrived when the reply did**, which is a property of the ordering rather
/// than of the machine:
///
/// | hypothesis | count |
/// |---|---|
/// | the join burst blocks the play loop | **361** — the reply cannot precede the last chunk, because the loop that produces it has not started |
/// | the burst is deferred past `JOIN_PRESTREAM_RADIUS` | **12–24**, measured over three runs — the nine pre-streamed columns plus however many of the deferred stream `select!` emitted before it happened to poll the socket read first |
///
/// The bound is 40 — comfortably above the second and nowhere near the first, so
/// it cannot be satisfied by a scheduler that merely reordered the burst. It is a
/// *range* rather than a single number because `select!` picks between a ready
/// column and a ready packet at random, which is exactly the property that stops
/// either starving the other; the floor of 9 is the deterministic part. And the
/// view still has to arrive **whole and in order** afterwards, which the tail of
/// this test asserts with the same [`check_proximity_stream`] the two ordering
/// gates use: dropping the rest of the view would otherwise pass.
#[tokio::test]
async fn a_play_packet_is_serviced_before_the_last_join_chunk() {
    let view_radius = 9;
    let expected_chunks = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    let generated = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let server = lodestone_server::IntegratedServer::bind(
        "127.0.0.1:0",
        ProbeProto {
            generated: std::sync::Arc::clone(&generated),
        },
        CountingAirSource {
            generated: std::sync::Arc::clone(&generated),
        },
        view_radius,
    )
    .await
    .expect("bind loopback");
    let addr = server.local_addr().expect("a bound server has an address");

    let mut client = Connection::new(
        tokio::net::TcpStream::connect(addr)
            .await
            .expect("client connects"),
    );
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut w = Writer::default();
    w.string("Impatient");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");
    client.read_packet().await.unwrap().unwrap(); // LOGIN_SUCCESS
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");
    // The interaction, written before a single chunk has been read: a difficulty
    // change, whose reply is a `change_difficulty` broadcast
    // (`apply_difficulty_change`). Any play packet with an observable reply would
    // do — this one is the cheapest in this file's stand-in vocabulary and needs
    // no world state to succeed.
    client
        .write_packet(CHANGE_DIFFICULTY_C2S, &[3])
        .await
        .expect("difficulty change");

    let mut chunks_before_reply = None;
    let mut observed = Vec::with_capacity(expected_chunks);
    while observed.len() < expected_chunks {
        let (id, payload) = client
            .read_packet()
            .await
            .expect("read")
            .expect("the server must not close mid-view");
        if id == CHUNK {
            let mut r = Reader::new(&payload);
            let cx = r.var_i32().unwrap();
            let cz = r.var_i32().unwrap();
            let at = r.var_i32().unwrap() as usize;
            observed.push((cx, cz, at));
        } else if id == CHANGE_DIFFICULTY_S2C && chunks_before_reply.is_none() {
            chunks_before_reply = Some(observed.len());
        }
    }

    let at = chunks_before_reply.expect(
        "the difficulty change was never answered: the play loop either never ran or the \
         packet was consumed without a reply",
    );
    assert!(
        at < 40,
        "the play packet was answered only after {at} of {expected_chunks} join chunks. \
         Under the defect this is exactly {expected_chunks} (the play loop cannot run until the \
         burst finishes); with the burst deferred it measured 12-24"
    );

    // …and the deferred remainder still arrives, whole and in order.
    check_proximity_stream(&observed, view_radius)
        .expect("deferring the burst must not change what the client receives, or in what order");
    let sent: HashSet<(i32, i32)> = observed.iter().map(|&(cx, cz, _)| (cx, cz)).collect();
    assert_eq!(
        sent,
        square(0, 0, view_radius),
        "every column of the view must still arrive exactly once"
    );

    drop(client);
    server.shutdown().await;
}

/// **The same instrument, pointed at a *move* instead of a join.**
///
/// [`a_play_packet_is_serviced_before_the_last_join_chunk`] covers the join burst.
/// Steady-state recentering has the same scheduling requirement: generating and
/// encoding every newly-visible column inline would occupy the connection task for
/// the whole strip. **That is a `world`-species blind spot in the join gate rather
/// than a missing assertion in it** — a join-only gate cannot reach the recenter
/// path.
///
/// # The counter, and why this jump
///
/// The subject is a jump far enough that the new window shares nothing with the old,
/// so the added set is a whole `(2r + 1)²` square — the same 361 columns the join
/// sends, which is what makes the two hypotheses as far apart as they can be:
///
/// | hypothesis | chunks of the strip that precede the reply |
/// |---|---|
/// | the move generates the strip inline | **361** — the loop cannot read the next packet until the last column is encoded |
/// | the strip is enqueued and streamed | a handful — the socket read and the column stream interleave |
///
/// The bound is 40, matching the join gate's, and it is nowhere near 361. A
/// one-axis step would not do: its added set is 19 columns, close enough to the
/// bound that a passing run would not distinguish the two orderings.
///
/// Ordering is `select!`'s random choice between a ready column and a ready packet,
/// which is the property that stops either starving the other — so this asserts a
/// *bound*, and separately asserts the strip still arrives whole, because dropped
/// newly-visible columns would otherwise sail through.
#[tokio::test]
async fn a_play_packet_is_serviced_before_the_last_chunk_of_a_move() {
    let view_radius = 9;
    let square_columns = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    let generated = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));

    let server = lodestone_server::IntegratedServer::bind(
        "127.0.0.1:0",
        ProbeProto {
            generated: std::sync::Arc::clone(&generated),
        },
        CountingAirSource {
            generated: std::sync::Arc::clone(&generated),
        },
        view_radius,
    )
    .await
    .expect("bind loopback");
    let addr = server.local_addr().expect("a bound server has an address");

    let mut client = Connection::new(
        tokio::net::TcpStream::connect(addr)
            .await
            .expect("client connects"),
    );
    client.write_packet(HANDSHAKE, &[2]).await.expect("hs");
    let mut w = Writer::default();
    w.string("Strider");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");
    client.read_packet().await.unwrap().unwrap(); // LOGIN_SUCCESS
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    let (id, _) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, SET_TIME_S2C);

    // The join first: this gate is about what happens *after* it, so the strip
    // counted below cannot be contaminated by columns the join still owed.
    let (join_observed, _) = collect_join_chunks(&mut client, square_columns).await;
    assert_eq!(
        join_observed.len(),
        square_columns,
        "precondition: the join view must be fully drained before the move, or the \
         count below mixes join columns into the strip"
    );
    let mut ack = Writer::default();
    ack.f32(10.0);
    client
        .write_packet(CHUNK_BATCH_RECEIVED_C2S, ack.as_slice())
        .await
        .expect("ack the join");

    // The jump — chunk (60, 60), whose radius-9 window shares no column with the
    // one centred on the spawn chunk — followed immediately by an interaction whose
    // reply is observable. Written back to back, before a single byte of the reply
    // or the strip is read, so the server sees both in its buffer at once and the
    // ordering it produces is the thing under test.
    let mut moved = Writer::default();
    moved.f64(60.0 * 16.0 + 8.0);
    moved.f64(64.0);
    moved.f64(60.0 * 16.0 + 8.0);
    client
        .write_packet(PLAYER_MOVED_C2S, moved.as_slice())
        .await
        .expect("send move");
    client
        .write_packet(CHANGE_DIFFICULTY_C2S, &[3])
        .await
        .expect("difficulty change");

    let mut chunks_before_reply = None;
    let mut strip: Vec<(i32, i32)> = Vec::with_capacity(square_columns);
    while strip.len() < square_columns {
        let (id, payload) = client
            .read_packet()
            .await
            .expect("read")
            .expect("the server must not close mid-strip");
        if id == CHUNK {
            let mut r = Reader::new(&payload);
            strip.push((r.var_i32().unwrap(), r.var_i32().unwrap()));
        } else if id == CHANGE_DIFFICULTY_S2C && chunks_before_reply.is_none() {
            chunks_before_reply = Some(strip.len());
        }
    }

    let at = chunks_before_reply.expect(
        "the difficulty change was never answered: the move consumed the connection task \
         for the whole strip, or the packet was dropped",
    );
    assert!(
        at < 40,
        "the play packet sent behind a chunk-boundary crossing was answered only after \
         {at} of {square_columns} newly-visible columns. Under the defect this is exactly \
         {square_columns}: `ViewTracker::recenter` awaited the whole strip inside \
         `dispatch_play_packet`, so the loop could not read the next packet at all"
    );

    let sent: HashSet<(i32, i32)> = strip.into_iter().collect();
    assert_eq!(
        sent,
        square(60, 60, view_radius),
        "streaming the strip must not change *which* columns the client receives: \
         exactly the new window, once each"
    );

    drop(client);
    server.shutdown().await;
}

/// `docs/plans/world-state.md` W1, gate (c)'s serve half:
/// `serve_play`'s `container_sync_tick` arm must actually drain the
/// [`WeatherFeed`] and turn each transition into an `encode_game_event`
/// broadcast — the piece with no inbound packet driving it, exactly like the
/// block-tick and explosion drains it sits beside. The feed is published to
/// directly here (the loop that fills it in production is gated in `crate::tick`'s
/// own module); a failure means the drain is missing or miswired, not the
/// weather machine.
#[tokio::test(start_paused = true)]
async fn weather_feed_transitions_reach_the_client_as_game_event_bytes() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;
    let weather = WeatherFeed::default();
    // A second handle for the server task, so this test can keep publishing
    // into the feed after the spawn (a `WeatherFeed` is an `Arc`-backed
    // shared buffer — cloning the handle is not cloning the events).
    let server_weather = weather.clone();

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection_with_mob_events(
            &mut conn,
            &FakeProtocol,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
            &BlockTickFeed::default(),
            &ExplosionFeed::default(),
            &server_weather,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Stormwatcher", 1).await;

    // Publish a rain ramp, a thunder ramp, then a rain flip, and wait one
    // `CONTAINER_SYNC_INTERVAL` (50 ms — the timer the drain rides) for the
    // arm to fire. `start_paused` freezes the clock except for explicit
    // advances, so one 50 ms advance is exactly one interval tick and the
    // three events all drain on it, in publish order.
    weather.publish(WeatherEvent::RainLevelChanged(0.5));
    weather.publish(WeatherEvent::ThunderLevelChanged(0.0));
    weather.publish(WeatherEvent::StartRaining);
    tokio::time::advance(Duration::from_millis(50)).await;
    tokio::task::yield_now().await;

    // The three transitions arrive as three `GAME_EVENT_S2C` frames, each
    // carrying the exact `(event, param)` pair the real v770 `GAME_EVENT`
    // packet would carry (ids 7/8/1) — asserted on bytes, not on "a packet
    // arrived".
    let mut seen = Vec::new();
    while seen.len() < 3 {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        assert_eq!(id, GAME_EVENT_S2C, "expected only game events, got id {id}");
        let mut r = Reader::new(&payload);
        seen.push((r.u8().expect("event id"), r.f32().expect("param")));
    }
    assert_eq!(
        seen,
        vec![(7, 0.5), (8, 0.0), (1, 0.0)],
        "the drain must forward each transition in publish order with its wire pair"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}
