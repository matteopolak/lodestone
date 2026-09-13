/// **The hunger integration gate**: a sprinting player's exhaustion
/// reaches the wire as a falling `saturation`, then a falling `food`, on the real
/// `serve_connection` path — not just inside `crate::food`'s own unit tests, which
/// would be entirely green with nothing calling them.
///
/// # The prediction, derived rather than observed
///
/// One 250-block sprinting step charges
/// `EXHAUSTION_SPRINT_PER_BLOCK * round(250 * 100) * 0.01 = 0.1 * 25000 * 0.01 =
/// 25.0` exhaustion. The tick then spends `4.0` per tick while exhaustion is
/// **strictly** above `4.0`, taking one saturation point each time and then one food
/// point once saturation is gone:
///
/// | tick | exhaustion after | saturation | food |
/// |---|---|---|---|
/// | 1 | 21.0 | 4.0 | 20 |
/// | 5 | 5.0 | 0.0 | 20 |
/// | 6 | 1.0 | 0.0 | **19** |
/// | 7 | 1.0 — not above 4.0 | 0.0 | 19 |
///
/// So the final wire state is exactly `food = 19, saturation = 0.0`, and it settles
/// there. Six drops, not seven: `1.0` is not greater than `4.0`.
///
/// Natural regeneration is turned off, because the player is at full health here and
/// a regeneration arm would otherwise start competing for the same exhaustion as
/// soon as anything hurt them — the same reason the drowning gate above turns it off.
#[tokio::test(start_paused = true)]
async fn a_sprinting_player_loses_saturation_then_food_on_the_wire() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Sprinter", 1).await;
    send_game_rule(&mut client, "natural_health_regeneration", "false").await;

    // Establish a starting position, then sprint 250 blocks in z from it. The
    // exhaustion charge needs the *previous* position, so the first move is the
    // baseline and the second is the one that costs.
    send_player_input(&mut client, true, false, false).await;
    send_player_moved(&mut client, 8.0, 8.0, 8.0).await;
    send_player_moved(&mut client, 8.0, 8.0, 258.0).await;

    // Collect `(food, saturation)` from every `SetHealth` until the predicted final
    // state arrives, bounded so a failure reports what it saw rather than hanging.
    let mut seen: Vec<(i32, f32)> = Vec::new();
    for _ in 0..4_000 {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        if id != SET_HEALTH_S2C {
            continue;
        }
        let mut r = Reader::new(&payload);
        let health = r.f32().expect("health");
        let food = r.var_i32().expect("food");
        let saturation = r.f32().expect("saturation");
        assert_eq!(health, 20.0, "nothing here should damage the player");
        seen.push((food, saturation));
        if (food, saturation) == (19, 0.0) {
            break;
        }
    }

    assert!(
        seen.contains(&(20, 4.0)),
        "the first drop must cost *saturation*, not food — if the first update is \
         (19, …) then exhaustion is decrementing food directly and hunger depletes \
         five times too fast. Saw: {seen:?}"
    );
    assert_eq!(
        seen.last().copied(),
        Some((19, 0.0)),
        "25.0 exhaustion is exactly six 4.0 drops: five of saturation then one of \
         food. Saw: {seen:?}"
    );
    assert!(
        !seen.contains(&(18, 0.0)),
        "a seventh drop would mean the threshold test is `>=` rather than `>`, since \
         1.0 exhaustion is left over. Saw: {seen:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// **The control**, and the reason the gate above is about *sprinting* rather than
/// about "moving costs food": the identical 250-block step with the sprint flag off
/// must cost **nothing at all**. Walking contributes a literal `0.0F` multiplier.
///
/// Asserted as an absence, so it needs the detector to be known-working — and it is,
/// by construction: the gate above uses the same harness, the same source and the
/// same read loop, and does see updates. Here a bounded read must see none.
#[tokio::test(start_paused = true)]
async fn the_same_step_while_walking_costs_no_hunger_at_all() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Walker", 1).await;
    send_game_rule(&mut client, "natural_health_regeneration", "false").await;

    // No `send_player_input`, so `sprinting` stays at its `false` default.
    send_player_moved(&mut client, 8.0, 8.0, 8.0).await;
    send_player_moved(&mut client, 8.0, 8.0, 258.0).await;

    // The window is **measured, not assumed**. This reads until the server closes the
    // connection (it eventually does: nothing here answers a keep-alive, so the
    // keep-alive timeout fires), and counts the packets that did arrive. A control
    // asserting an absence is only as good as the evidence something *would* have
    // shown up, so the packet count is asserted too — an empty stream would
    // otherwise pass this vacuously.
    let mut updates: Vec<(i32, f32)> = Vec::new();
    let mut observed = 0usize;
    for _ in 0..4_000 {
        let Ok(Some((id, payload))) = client.read_packet().await else {
            break;
        };
        observed += 1;
        if id != SET_HEALTH_S2C {
            continue;
        }
        let mut r = Reader::new(&payload);
        let _health = r.f32().expect("health");
        updates.push((
            r.var_i32().expect("food"),
            r.f32().expect("saturation"),
        ));
    }
    assert!(
        observed > 20,
        "the window must actually span some server traffic, or this control measures \
         nothing; saw {observed} packets"
    );
    assert!(
        updates.is_empty(),
        "walking is a 0.0F multiply in vanilla, so nothing may change: {updates:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// A world of lava, for the burning gate. Mirrors [`WaterSource`]'s shape exactly.
struct LavaSource;

impl ChunkSource for LavaSource {
    fn column(&self, _cx: i32, _cz: i32) -> ChunkColumn {
        let mut col = ChunkColumn::new(0, 16);
        for x in 0..16 {
            for z in 0..16 {
                for y in 0..16 {
                    col.set_block(x, y, z, "minecraft:lava");
                }
            }
        }
        col
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).biome_state_at(lx, y, lz).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {}
}

/// **The burning integration gate**: a player standing in lava takes
/// lava's damage on the real `serve_connection` path, and dies to it.
///
/// # The prediction, derived from the constants
///
/// Contact with lava deals **`4.0` per tick**, and the generic burn tick's own
/// `1.0` is suppressed while the player remains in lava. From full health the
/// sequence on the wire is
/// `16.0, 12.0, 8.0, 4.0, 0.0` — **five ticks to death**, and every value a multiple
/// of 4.
///
/// The wrong hypotheses, both separated:
///
/// | hypothesis | first health value |
/// |---|---|
/// | lava alone (correct) | **16.0** |
/// | lava plus the unguarded burn tick | 15.0 |
/// | burn tick alone | 19.0 |
///
/// `natural_health_regeneration` is off so nothing heals between hits, which would
/// otherwise make the sequence depend on the race rather than on the damage.
#[tokio::test(start_paused = true)]
async fn a_player_standing_in_lava_burns_at_four_damage_per_tick() {
    let (client_end, server_end) = memory_pair();
    let source = LavaSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Diver", 1).await;
    send_game_rule(&mut client, "natural_health_regeneration", "false").await;
    send_player_moved(&mut client, 8.0, 8.0, 8.0).await;

    let mut healths: Vec<f32> = Vec::new();
    for _ in 0..2_000 {
        let Ok(Some((id, payload))) = client.read_packet().await else {
            break;
        };
        if id != SET_HEALTH_S2C {
            continue;
        }
        let mut r = Reader::new(&payload);
        let health = r.f32().expect("health");
        if healths.last().copied() != Some(health) {
            healths.push(health);
        }
        if health <= 0.0 {
            break;
        }
    }

    assert_eq!(
        healths.first().copied(),
        Some(16.0),
        "lava is 4.0 per tick and the burn tick's own 1.0 is suppressed while in it — \
         15.0 would mean the !isInLava guard is missing, 19.0 that lava's contact \
         damage is. Saw: {healths:?}"
    );
    assert_eq!(
        healths,
        vec![16.0, 12.0, 8.0, 4.0, 0.0],
        "five ticks of 4.0 from full health, every value a multiple of 4: {healths:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// **The control**: the identical run in an all-air world must produce no health
/// update at all. Without it, the gate above is satisfied by anything that damages a
/// player on a timer.
///
/// The window is measured by packet count for the reason the walking control gives.
#[tokio::test(start_paused = true)]
async fn a_player_standing_in_air_never_burns() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Bystander", 1).await;
    send_game_rule(&mut client, "natural_health_regeneration", "false").await;
    send_player_moved(&mut client, 8.0, 8.0, 8.0).await;

    let mut updates: Vec<f32> = Vec::new();
    let mut observed = 0usize;
    for _ in 0..2_000 {
        let Ok(Some((id, payload))) = client.read_packet().await else {
            break;
        };
        observed += 1;
        if id != SET_HEALTH_S2C {
            continue;
        }
        let mut r = Reader::new(&payload);
        updates.push(r.f32().expect("health"));
    }
    assert!(
        observed > 20,
        "the window must span real traffic or this control measures nothing; saw \
         {observed} packets"
    );
    assert!(
        updates.is_empty(),
        "an air world must not burn anyone: {updates:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// **The boat-dismount gate.**
///
/// The input carries separate sprint (`0x40`) and shift (`0x20`) flags. This
/// drives the real `dispatch_play_packet` path, where the shift flag reaches
/// `MobSim::dismount_rider`, and asserts both the simulation state and the wire.
///
/// Mounting goes through the real `MobSim` API rather than a wire-level
/// `INTERACT_ENTITY` (this file's stand-in protocol has no decode arm for that
/// packet) — only the dismount half is under test.
///
/// The passenger check is a **level** check run every tick, not an edge. The
/// gate's precondition (`boarded` while *not* sneaking) keeps this test focused
/// on dismounting and leaves boarding behavior to `mount_vehicle`.
#[tokio::test(start_paused = true)]
async fn sneaking_dismounts_a_boat_on_the_wire() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;
    let mobs = MobHandle::default();
    let mobs_for_server = mobs.clone();

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &mobs_for_server,
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Sailor", 1).await;

    let boat = mobs.with(|sim| {
        sim.spawn_vehicle(
            "minecraft:oak_boat".parse().expect("valid key"),
            Vec3::new(8.0, 8.0, 8.0),
            0.0,
        )
    });
    let boarded = mobs.with(|sim| sim.mount_vehicle(boat, LOCAL_PLAYER_ENTITY_ID, false));
    assert!(
        boarded,
        "precondition: mounting must succeed while not sneaking"
    );
    assert_eq!(
        mobs.with(|sim| sim.vehicle_ridden_by(LOCAL_PLAYER_ENTITY_ID)),
        Some(boat),
        "precondition: the player is aboard before the sneak packet is sent"
    );
    let _ = drain_available(&mut client).await;

    // A real `PLAYER_INPUT` with `shift: true`, through
    // the real dispatch path.
    send_player_input(&mut client, false, true, false).await;

    let packets = drain_available(&mut client).await;
    let payload = packets
        .iter()
        .find(|(id, _)| *id == SET_PASSENGERS_S2C)
        .map(|(_, payload)| payload.clone())
        .unwrap_or_else(|| {
            panic!(
                "sneaking while aboard must send SET_PASSENGERS — the only channel a \
                 client learns it dismounted through; got packet ids {:?}",
                packets.iter().map(|(id, _)| *id).collect::<Vec<_>>()
            )
        });
    let mut r = Reader::new(&payload);
    let vehicle_id = r.var_i32().expect("vehicle id");
    let count = r.var_i32().expect("passenger count");
    assert_eq!(vehicle_id, boat, "must name the boat the player just left");
    assert_eq!(
        count, 0,
        "dismounting sends the vehicle's whole (now empty) passenger list, not a delta"
    );

    let teleport = packets
        .iter()
        .find(|(id, _)| *id == PLAYER_POSITION_S2C)
        .map(|(_, payload)| payload)
        .unwrap_or_else(|| {
            panic!(
                "dismounting must authoritatively place the player; got packet ids {:?}",
                packets.iter().map(|(id, _)| *id).collect::<Vec<_>>()
            )
        });
    let mut r = Reader::new(teleport);
    let placed = (
        r.f64().expect("dismount x"),
        r.f64().expect("dismount y"),
        r.f64().expect("dismount z"),
    );
    let yaw = r.f32().expect("dismount yaw");
    let pitch = r.f32().expect("dismount pitch");
    assert_eq!(
        placed,
        (8.0, 8.5625, 8.0),
        "an all-air world has no safe side floor, so vanilla falls back to the boat deck"
    );
    assert_eq!((yaw, pitch), (0.0, 0.0));

    assert_eq!(
        mobs.with(|sim| sim.vehicle_ridden_by(LOCAL_PLAYER_ENTITY_ID)),
        None,
        "the sim itself must actually vacate the boat, not just announce it on the wire"
    );

    drop(client);
    let _ = server.await.expect("server task panicked");
}
