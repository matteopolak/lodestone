use super::common::*;

/// Reads packets until a [`SET_HEALTH_S2C`] arrives, collecting every
/// [`AIR_SUPPLY_S2C`] value seen along the way (in order) and discarding
/// anything else (keep-alive, time-sync noise interleaved by the same
/// `tokio::select!` loop). Returns `(air_values, health_after)`.
/// Reads packets, discarding anything whose id is not `target_id`, until one
/// matches — the same "skip keep-alive/time-sync noise" tolerance
/// [`read_until_health_update`] already establishes, generalised to any
/// single target id. Needed because the two-directive respawn confirmation
/// (`SET_HEALTH_S2C` then `AIR_SUPPLY_S2C`) can have a stray periodic
/// broadcast queued immediately ahead of it: `tokio::select!` may pick the
/// drowning tick that reaches exactly `0.0` health and the 1-second
/// time-sync tick as *separate* loop iterations at the same virtual instant,
/// so a `SET_TIME_S2C` the server already queued before the client ever
/// sends the respawn command can still be sitting unread in the pipe —
/// asserting on the very next packet without skipping past it is exactly the
/// kind of interleaving-noise race `read_until_health_update`'s own doc
/// comment already accounts for.
async fn read_until(client: &mut Connection<DuplexStream>, target_id: i32) -> Vec<u8> {
    loop {
        let (id, payload) = client
            .read_packet_timeout(Duration::from_secs(5))
            .await
            .expect("read")
            .expect("packet");
        if id == target_id {
            return payload;
        }
    }
}

async fn read_until_health_update(client: &mut Connection<DuplexStream>) -> (Vec<i32>, f32) {
    let mut air_values = Vec::new();
    loop {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        let mut r = Reader::new(&payload);
        if id == AIR_SUPPLY_S2C {
            air_values.push(r.var_i32().expect("air value"));
        } else if id == SET_HEALTH_S2C {
            return (air_values, r.f32().expect("health value"));
        }
        // else: keep-alive / time-sync noise, ignored.
    }
}

/// **Subject**: a player whose eye is submerged the whole time must lose air
/// on the exact cadence (`crate::vitals`'s module doc comment) and take the
/// first drowning hit at exactly
/// tick 320 (300 ticks = 15s to empty from full, then 20 more ticks = 1s to
/// cross the `<= -20` threshold) — not some rounder or approximated number.
/// [`WaterSource`] fills the *entire* column, so the player is genuinely
/// submerged throughout; a test that never exposes the drowning condition would
/// prove nothing because a player who never gets wet cannot exercise the gate.
///
/// This test spans 320 vitals ticks (16s of virtual time) to the first hit,
/// then a further 20 ticks (1s) to the second — 340 real tick-cadence steps
/// in total, all resolved by `tokio`'s paused-clock auto-advance in a
/// fraction of a second of wall time, the same mechanism the keep-alive
/// tests above already rely on for their 15s+ spans. This is deliberately
/// **not** a short window: a test that only ran a handful of ticks would pass
/// even if the cadence were wrong, since nothing would yet distinguish "1 tick"
/// from "20 ticks" from "300 ticks". Spanning past two full hits is what
/// proves the cadence repeats rather than being a one-off.
#[tokio::test(start_paused = true)]
async fn submerged_player_loses_air_and_takes_drowning_damage_on_vanilla_cadence() {
    let (client_end, server_end) = memory_pair();
    let source = WaterSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Diver", 1).await;

    // **Natural regeneration off, and this is load-bearing rather than tidying.**
    // With hunger integration active, a hurt player with a full food bar heals on the fast
    // regeneration arm every 10 ticks — so the very next `SetHealth` after the first
    // drowning hit is a *heal*, not the second hit, and this gate's "the next health
    // update is the second hit" premise stopped holding. Turning the rule off keeps
    // the test measuring the drowning cadence rather than the race between drowning
    // and regeneration, which `crate::food`'s own gates cover.
    send_game_rule(&mut client, "natural_health_regeneration", "false").await;

    // Any position inside chunk (0, 0) with y in [0, 16) is submerged: the
    // entire `WaterSource` column is water, feet at y = 8 puts the eye
    // (8 + 1.62 = 9.62, floored to block y = 9) in water too.
    send_player_moved(&mut client, 8.0, 8.0, 8.0).await;

    let (air_values, health_after_first_hit) = read_until_health_update(&mut client).await;

    // Expected sequence, derived the same way `PlayerVitals::tick` computes
    // it rather than restated as a magic literal: 319 decrements from 300
    // (299, 298, ..., 0, -1, ..., -19), then the 320th tick resets to 0 on
    // crossing the damage threshold.
    let mut expected = Vec::new();
    let mut air = 300;
    for _ in 0..319 {
        air -= 1;
        expected.push(air);
    }
    expected.push(0);

    assert_eq!(
        air_values, expected,
        "air must count down by exactly 1/tick, resetting to 0 only on the hit"
    );
    assert_eq!(
        health_after_first_hit, 18.0,
        "first drowning hit must deal exactly 2.0 damage (20.0 -> 18.0)"
    );

    // The countdown re-arms identically: the second hit must land exactly
    // 20 ticks later, not immediately and not some other interval.
    let (air_values2, health_after_second_hit) = read_until_health_update(&mut client).await;
    let mut expected2 = Vec::new();
    let mut air2 = 0;
    for _ in 0..19 {
        air2 -= 1;
        expected2.push(air2);
    }
    expected2.push(0);

    assert_eq!(air_values2, expected2, "the re-armed countdown must also take exactly 20 ticks");
    assert_eq!(
        health_after_second_hit, 16.0,
        "second drowning hit must also deal exactly 2.0 damage (18.0 -> 16.0)"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// **Control**: a player who is never submerged (an all-air world, matching
/// `AirSource`) must receive **zero** air-supply or health updates, even
/// across a window (20s) comfortably longer than the 16s the subject test
/// above takes to reach its first drowning hit. This is the control that proves
/// the submersion test actually
/// gates the tick — not merely that the subject test happened to show
/// numbers going down, which alone would not rule out air draining
/// regardless of water.
#[tokio::test(start_paused = true)]
async fn dry_player_keeps_full_air_and_takes_no_damage() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Dry", 1).await;
    send_player_moved(&mut client, 8.0, 64.0, 8.0).await;

    tokio::time::sleep(Duration::from_secs(20)).await;

    let packets = drain_available(&mut client).await;
    let stray: Vec<_> = packets
        .iter()
        .filter(|(id, _)| *id == AIR_SUPPLY_S2C || *id == SET_HEALTH_S2C)
        .collect();
    assert!(
        stray.is_empty(),
        "a dry player must never receive an air-supply or health update: {stray:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// The actual consumer, exercised through the real scheduling loop
/// (`dispatch_play_packet`/`apply_difficulty_change`) rather than only at a
/// protocol decode/encode layer. A `ServerBound::DifficultyChanged` sent over a real
/// connection must produce exactly one confirmation back, carrying the
/// requested difficulty — proof `WorldAdminState` is real, connected state
/// and not a struct nothing calls into.
#[tokio::test(start_paused = true)]
async fn difficulty_change_is_confirmed_back_to_the_connection() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Op", 1).await;

    let mut w = Writer::default();
    w.u8(3); // Hard
    client
        .write_packet(CHANGE_DIFFICULTY_C2S, w.as_slice())
        .await
        .expect("send change_difficulty");

    let (id, payload) = client
        .read_packet_timeout(Duration::from_secs(5))
        .await
        .expect("read")
        .expect("confirmation packet");
    assert_eq!(id, CHANGE_DIFFICULTY_S2C);
    let mut r = Reader::new(&payload);
    assert_eq!(r.u8().expect("difficulty"), 3, "confirmed difficulty must be Hard");
    assert!(
        !r.bool().expect("locked"),
        "difficulty was never locked in this test"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// **The flow-control gate itself**: a second chunk
/// batch must not be sent while the first is still unacknowledged — it is
/// queued instead — and must flush the moment the acknowledgement arrives.
/// `recenter` must queue a fresh batch while an earlier batch is outstanding,
/// then flush it after the acknowledgement arrives. This keeps the connection
/// from sending overlapping batches.
#[tokio::test(start_paused = true)]
async fn chunk_batch_is_held_until_acknowledged_then_flushed() {
    let view_radius = 1; // 3x3 = 9 columns
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &source,
            &NoEntities,
            view_radius,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
    });

    let mut client = Connection::new(client_end);
    // Deliberately does NOT ack the initial join batch — that unacknowledged
    // batch is exactly what should gate the next one.
    drive_login_and_join(&mut client, "Queued", 9).await;

    send_player_moved(&mut client, 160.0, 64.0, 0.0).await;
    let held = drain_available(&mut client).await;
    let (center, forgotten, added) = split_view_directives(&held);
    assert_eq!(center, Some((10, 0)), "the cache-center update is never gated");
    assert_eq!(forgotten, square(0, 0, view_radius), "forgets are never gated either");
    assert!(
        added.is_empty(),
        "the new columns must be queued, not sent, while the join batch is unacknowledged: {added:?}"
    );

    // Acknowledge the (still-outstanding) join batch. This is also the
    // signal that flushes the queued jump batch.
    send_chunk_batch_received(&mut client, 10.0).await;
    let flushed = drain_available(&mut client).await;
    let (center2, forgotten2, added2) = split_view_directives(&flushed);
    assert!(center2.is_none(), "the cache-center update already went out; must not repeat");
    assert!(forgotten2.is_empty(), "the forgets already went out; must not repeat");
    assert_eq!(
        added2,
        square(10, 0, view_radius),
        "acknowledging must flush exactly the queued batch"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// The actual consumer for `SET_CREATIVE_MODE_SLOT`: a write to menu
/// slot 9 (main storage — native index 9 too, see
/// `PlayerInventory::apply_menu_slot_change`'s own table) must land in the
/// real `PlayerInventory` the connection closes with, not just decode
/// cleanly. No confirmation packet is expected either — see
/// `ServerBound::CreativeModeSlotSet`'s own doc comment for why no response is
/// sent: the client predicts this write locally.
#[tokio::test(start_paused = true)]
async fn creative_mode_slot_write_lands_in_the_real_inventory() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Creator", 1).await;

    let stack = ItemStack::new("minecraft:diamond_block".parse().expect("valid resource key"), 12);
    send_creative_slot(&mut client, 9, Some(&stack)).await;
    let stray = drain_available(&mut client).await;
    assert!(
        stray.is_empty(),
        "a creative-slot write must not itself produce a reply: {stray:?}"
    );

    drop(client);
    let summary = server.await.unwrap().expect("clean close");
    assert_eq!(
        summary.inventory.native(9),
        Some(&stack),
        "menu slot 9 (main storage) must land at native index 9"
    );
}

/// The play-state `ServerBound::PingRequest` arm in
/// `dispatch_play_packet` must call `encode_pong_response`; this is the
/// dispatch-and-consumer half of the behavior. The stand-in wire shape is
/// deliberately simple, so the assertion does not depend on adapter details.
///
/// The time value is echoed unchanged — asserted by value, not just by
/// "a reply arrived", so a consumer that answered with the wrong field (or a
/// constant) cannot pass.
#[tokio::test(start_paused = true)]
async fn ping_request_gets_a_pong_response_echoing_the_time() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Pinger", 1).await;

    send_ping_request(&mut client, 0x0102_0304_0506_0708).await;
    let reply = drain_available(&mut client).await;
    let pongs: Vec<i64> = reply
        .iter()
        .filter(|(id, _)| *id == PONG_RESPONSE_S2C)
        .map(|(_, payload)| {
            let mut r = Reader::new(payload);
            r.i64().expect("pong time")
        })
        .collect();
    assert_eq!(
        pongs,
        vec![0x0102_0304_0506_0708],
        "exactly one pong, echoing the ping's own time unchanged: {reply:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// A valid `pong` has no reply or stored state, but it must be consumed by the
/// live Play dispatcher rather than merged into the malformed/unmodelled
/// `Ignored` bucket. The following ping request is the observable control: it
/// receives its normal reply only if that same connection stayed alive after
/// the no-op acknowledgement.
#[tokio::test(start_paused = true)]
async fn pong_is_consumed_without_ending_the_play_connection() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;
    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &source,
            &NoEntities,
            0,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "PongWatcher", 1).await;

    send_pong(&mut client, 0x0102_0304).await;
    let after_pong = drain_available(&mut client).await;
    assert!(
        after_pong.is_empty(),
        "the acknowledgement itself must not emit a packet: {after_pong:?}"
    );

    send_ping_request(&mut client, 0x1122_3344_5566_7788).await;
    let reply = drain_available(&mut client).await;
    let pongs: Vec<i64> = reply
        .iter()
        .filter(|(id, _)| *id == PONG_RESPONSE_S2C)
        .map(|(_, payload)| Reader::new(payload).i64().expect("pong time"))
        .collect();
    assert_eq!(
        pongs,
        vec![0x1122_3344_5566_7788],
        "the acknowledgement has no reply, and the next Play packet remains live: {reply:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// **Control**: menu slot 0 (the crafting-result slot) has no native index
/// at all — `PlayerInventory::apply_menu_slot_change`'s own table drops it,
/// exactly as it already does for a real `CONTAINER_CLICK`. Proves the
/// creative-slot consumer really does route through that table rather than
/// writing every wire slot verbatim into some parallel array.
#[tokio::test(start_paused = true)]
async fn creative_mode_slot_write_to_the_crafting_output_is_dropped() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Crafter", 1).await;

    let stack = ItemStack::new("minecraft:diamond_block".parse().expect("valid resource key"), 1);
    send_creative_slot(&mut client, 0, Some(&stack)).await;
    let _ = drain_available(&mut client).await;

    drop(client);
    let summary = server.await.unwrap().expect("clean close");
    for i in 0..lodestone_server::PLAYER_NATIVE_SIZE {
        assert!(
            summary.inventory.native(i).is_none(),
            "slot 0 must not land anywhere in the native inventory, but native {i} is occupied"
        );
    }
}

/// The `PERFORM_RESPAWN` consumer: once a player has actually died
/// (drowned to exactly `0.0` health, the same cadence
/// `submerged_player_loses_air_and_takes_drowning_damage_on_vanilla_cadence`
/// pins — 10 hits of 2.0 damage from 20.0, at tick 320 then every 20 ticks
/// after), a respawn request must refill both health and air on the real
/// connection, matching `PlayerVitals::respawn`'s own unit test.
#[tokio::test(start_paused = true)]
async fn respawn_after_death_refills_health_and_air() {
    let (client_end, server_end) = memory_pair();
    let source = WaterSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Drowned", 1).await;
    send_player_moved(&mut client, 8.0, 8.0, 8.0).await;

    // Drive the exact same cadence the drowning test above pins, until
    // health actually reaches zero (10 hits of 2.0 from 20.0).
    let health_after_death = loop {
        let (_air, health) = read_until_health_update(&mut client).await;
        if health <= 0.0 {
            break health;
        }
    };
    assert_eq!(health_after_death, 0.0, "expected exactly 10 hits to reach 0.0 health");

    send_client_command(&mut client, 0).await; // PERFORM_RESPAWN

    let payload = read_until(&mut client, SET_HEALTH_S2C).await;
    let mut r = Reader::new(&payload);
    assert_eq!(r.f32().expect("health"), 20.0, "respawn must restore full health");

    let payload2 = read_until(&mut client, AIR_SUPPLY_S2C).await;
    let mut r2 = Reader::new(&payload2);
    assert_eq!(r2.var_i32().expect("air"), 300, "respawn must restore full air");

    drop(client);
    let _ = server.await.unwrap();
}

/// **Control**: a respawn request from a player who is not dead must be a
/// no-op, proving the health/air refill is gated on death, not unconditional.
#[tokio::test(start_paused = true)]
async fn respawn_request_while_alive_is_a_no_op() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Alive", 1).await;

    send_client_command(&mut client, 0).await;
    let stray: Vec<_> = drain_available(&mut client)
        .await
        .into_iter()
        .filter(|(id, _)| *id == SET_HEALTH_S2C || *id == AIR_SUPPLY_S2C)
        .collect();
    assert!(
        stray.is_empty(),
        "a live player's respawn request must produce no health/air packet: {stray:?}"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// The other `client_command` ordinal: requesting current game-rule
/// values must reply — even with zero rules ever set — proving
/// `apply_client_command` actually calls through to
/// `ServerProtocol::encode_game_rule_values` rather than only doing so when
/// there happens to be something to report.
#[tokio::test(start_paused = true)]
async fn request_game_rule_values_replies_even_with_no_rules_set() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Curious", 1).await;

    send_client_command(&mut client, 2).await; // REQUEST_GAMERULE_VALUES

    let (id, payload) = client
        .read_packet_timeout(Duration::from_secs(5))
        .await
        .expect("read")
        .expect("game rule values reply");
    assert_eq!(id, GAME_RULE_VALUES_S2C);
    let mut r = Reader::new(&payload);
    assert_eq!(r.var_i32().expect("entry count"), 0, "no game rule was ever set");

    drop(client);
    let _ = server.await.unwrap();
}

/// **The game-rule path end to end**: `SET_GAME_RULE` writes into the world's typed store,
/// and an unknown key is *rejected* rather than stored.
///
/// The spelling is the load-bearing half: `random_tick_speed` is the accepted
/// rule key. The test ensures the typed store validates that spelling
/// and does not echo an unknown key back to the client.
///
/// Both directions are asserted from the same connection: the reply to the valid
/// rule carries it, and the reply to the invalid one is empty.
#[tokio::test(start_paused = true)]
async fn a_set_game_rule_is_validated_and_a_renamed_key_is_refused() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "RuleSetter", 1).await;

    // The real 26.2 identifier: accepted, and confirmed back.
    send_game_rule(&mut client, "random_tick_speed", "7").await;
    let (id, payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, GAME_RULE_VALUES_S2C);
    let mut r = Reader::new(&payload);
    assert_eq!(r.var_i32().expect("entry count"), 1);
    assert_eq!(r.string(64).expect("key"), "random_tick_speed");
    assert_eq!(r.string(64).expect("value"), "7");

    // The pre-26.2 spelling: refused, so the confirmation is empty.
    send_game_rule(&mut client, "randomTickSpeed", "9").await;
    let (id, payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, GAME_RULE_VALUES_S2C);
    let mut r = Reader::new(&payload);
    assert_eq!(
        r.var_i32().expect("entry count"),
        0,
        "a renamed key is not a rule this server knows, and storing it would be \
         confirmed-and-never-read"
    );

    // And the store still holds only the valid one.
    send_client_command(&mut client, 2).await;
    let (id, payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, GAME_RULE_VALUES_S2C);
    let mut r = Reader::new(&payload);
    assert_eq!(r.var_i32().expect("entry count"), 1);
    assert_eq!(r.string(64).expect("key"), "random_tick_speed");
    assert_eq!(r.string(64).expect("value"), "7");

    drop(client);
    let _ = server.await.unwrap();
}

/// The `CLIENT_INFORMATION` consumer: a settings change mid-session
/// must resize the streamed view around the connection's own tracked
/// center — shrinking forgets exactly the outer ring, and growing back
/// (clamped at the server's own configured cap, not the client's raw
/// request) re-sends exactly that same ring.
#[tokio::test(start_paused = true)]
async fn client_information_view_distance_resizes_the_streamed_view() {
    let view_radius = 2; // 5x5 = 25 columns — this connection's configured cap
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(
            &mut conn,
            &FakeProtocol,
            &source,
            &NoEntities,
            view_radius,
            &BlockEntityHandle::default(),
            &MobHandle::default(),
        )
        .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Settings", 25).await;
    // Clear the join batch's own outstanding ack first — this test is about
    // the resize diff, not the flow-control gate (see
    // `chunk_batch_is_held_until_acknowledged_then_flushed` for that).
    send_chunk_batch_received(&mut client, 10.0).await;

    let full = square(0, 0, 2);
    let inner = square(0, 0, 1);
    let ring: HashSet<(i32, i32)> = full.difference(&inner).copied().collect();
    assert_eq!(ring.len(), 16, "sanity: the 5x5 minus 3x3 ring is 16 columns");

    // Shrink to 1: never gated at all (there is nothing to add, only
    // forgets — see `send_view_update`'s own doc comment for why forgets
    // bypass the ack gate entirely).
    send_client_information(&mut client, 1).await;
    let shrunk = drain_available(&mut client).await;
    let (center, forgotten, added) = split_view_directives(&shrunk);
    assert!(center.is_none(), "a settings change never moves the tracked center");
    assert_eq!(forgotten, ring, "shrinking must forget exactly the outer ring");
    assert!(added.is_empty(), "shrinking must never add a column");

    // Grow back past the server's own cap (10 requested, 2 configured) —
    // must clamp to the cap, not the raw requested value.
    send_client_information(&mut client, 10).await;
    let grown = drain_available(&mut client).await;
    let (center2, forgotten2, added2) = split_view_directives(&grown);
    assert!(center2.is_none());
    assert!(forgotten2.is_empty(), "growing must never forget a column");
    assert_eq!(
        added2, ring,
        "clamped growth must re-send exactly the same ring, not the raw requested radius"
    );

    drop(client);
    let _ = server.await.unwrap();
}

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
