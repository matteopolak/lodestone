/// **Positive control**: a client that stops responding after joining —
/// connected, but never echoing anything back — must actually be
/// disconnected once the keep-alive challenge goes unanswered, not merely
/// "would be" in theory.
#[tokio::test(start_paused = true)]
async fn silent_client_is_disconnected_after_keep_alive_timeout() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Ghost", 1).await;
    // `client` is deliberately held open (not dropped, not read from, not
    // written to) from here — a genuine stall, not a clean disconnect, which
    // `serve_connection` already handles as a normal `Ok`.

    let result = server.await.expect("server task panicked");
    assert!(
        matches!(result, Err(ServerError::KeepAliveTimeout)),
        "expected KeepAliveTimeout, got {result:?}"
    );

    drop(client);
}

/// **Negative control**, run against the exact same mechanism: a client that
/// answers every keep-alive challenge must stay connected across several
/// intervals — not just long enough to look alive once.
#[tokio::test(start_paused = true)]
async fn responsive_client_survives_multiple_keep_alive_intervals() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    drive_login_and_join(&mut client, "Alive", 1).await;

    // Answer four consecutive keep-alive challenges — comfortably more than
    // one interval's worth — echoing each id straight back, exactly as
    // `lodestone-client`'s default automatic `KeepAlivePolicy` does
    // (`crates/lodestone-client/src/driver.rs`'s `ClientEvent::KeepAlive`
    // arm). Anything else received (the periodic time broadcast) is drained
    // and ignored.
    let mut answered = 0;
    while answered < 4 {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        if id == KEEP_ALIVE_S2C {
            client
                .write_packet(KEEP_ALIVE_C2S, &payload)
                .await
                .expect("echo keep-alive");
            answered += 1;
        }
    }

    // Closing now must still be a clean `Ok` — the same mechanism that fired
    // `KeepAliveTimeout` in the sibling test above did not fire here.
    drop(client);
    let result = server.await.expect("server task panicked");
    assert!(
        matches!(result, Ok(_)),
        "expected a clean close after answered keep-alives, got {result:?}"
    );
}

/// The join-time full clock sync anchors at tick 0, and the periodic
/// broadcasts that follow carry a strictly increasing `game_time` with no
/// anchor — proving both halves of `ServerProtocol::encode_set_time`'s
/// contract are actually driven, not just implemented.
#[tokio::test(start_paused = true)]
async fn time_of_day_anchors_at_join_then_broadcasts_periodically() {
    let (client_end, server_end) = memory_pair();
    let source = AirSource;

    let server = tokio::spawn(async move {
        let mut conn = Connection::new(server_end);
        serve_connection(&mut conn, &FakeProtocol, &source, &NoEntities, 0, &BlockEntityHandle::default(), &MobHandle::default())
            .await
    });

    let mut client = Connection::new(client_end);
    // `drive_login_and_join` already asserts the join-time `SET_TIME_S2C`
    // arrives before any chunk; re-derive its payload here to check the
    // anchor's actual value rather than only its position in the sequence.
    client.write_packet(HANDSHAKE, &[2]).await.unwrap();
    let mut w = Writer::default();
    w.string("Clockwatcher");
    client.write_packet(LOGIN_START, w.as_slice()).await.unwrap();
    client.read_packet().await.unwrap().unwrap(); // LOGIN_SUCCESS
    client.write_packet(LOGIN_ACKNOWLEDGED, &[]).await.unwrap();
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .unwrap();

    let (id, payload) = client.read_packet().await.unwrap().unwrap();
    assert_eq!(id, SET_TIME_S2C);
    let mut r = Reader::new(&payload);
    assert_eq!(r.i64().unwrap(), 0, "join-time game_time must be 0");
    assert!(
        r.bool().unwrap(),
        "join-time sync must carry a day/night anchor"
    );
    assert_eq!(r.i64().unwrap(), 0, "join-time anchor must be tick 0");

    // Drain the initial chunk batch (1 column, view_radius 0) to reach the
    // steady state.
    client.read_packet().await.unwrap().unwrap(); // CHUNK_BATCH_START
    client.read_packet().await.unwrap().unwrap(); // the one CHUNK
    client.read_packet().await.unwrap().unwrap(); // CHUNK_BATCH_FINISHED

    // Collect the periodic broadcasts. Each
    // one reports the *world's* clock, and this server has no tick loop, so that
    // clock is zero and stays zero. Three broadcasts arrive, proving the 1-second
    // `TIME_SYNC_INTERVAL` timer fires repeatedly, and all three carry the same
    // value.
    //
    // A *rising* value would indicate that the broadcast uses connection elapsed
    // time instead of the world's clock. The assertion is intentionally exact:
    // an elapsed-time source would make `game_time` climb and fail here.
    for broadcast in 0..3 {
        let (id, payload) = client.read_packet().await.unwrap().unwrap();
        assert_eq!(id, SET_TIME_S2C);
        let mut r = Reader::new(&payload);
        assert_eq!(
            r.i64().unwrap(),
            0,
            "broadcast {broadcast}: no tick loop means no world ticks, so game_time \
             stays 0 — a climbing value is elapsed-since-join, issue #323's bug"
        );
        assert!(
            r.bool().unwrap(),
            "the day/night anchor is now always sent: an empty clock map means \
             'keep your own anchor', and a client keeps advancing that, so a frozen \
             server clock would still show a moving sun"
        );
        assert_eq!(r.i64().unwrap(), 0, "and the anchor is the world's day_time");
    }

    drop(client);
    let _ = server.await.unwrap();
}
/// View streaming is unobserved if the player never actually crosses a chunk
/// boundary. This moves through three states — no change, a jump far enough
/// that the current and new windows share nothing, then a one-column shift — and
/// asserts on **which** columns were sent and dropped each time, not just a
/// count.
#[tokio::test(start_paused = true)]
async fn player_moved_streams_view_across_several_chunk_boundaries() {
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
    drive_login_and_join(&mut client, "Walker", 9).await;

    // The chunk-batch flow-control gate (`ServerBound::
    // ChunkBatchAcknowledged`) now holds a *second* batch until the first is
    // acknowledged — see `chunk_batch_is_held_until_acknowledged_then_flushed`
    // below for a test of that gate itself. This test is about the view diff
    // shape, not flow control, so it acks promptly after every batch,
    // exactly like a real client's automatic reply does — without this ack
    // the jump/shift batches below would be silently queued instead of sent.
    send_chunk_batch_received(&mut client, 10.0).await;

    // Same-chunk movement must touch nothing: the view is recomputed only when
    // the two-dimensional chunk position actually changes.
    send_player_moved(&mut client, 1.0, 64.0, 1.0).await;
    let noop = drain_available(&mut client).await;
    assert!(
        noop.is_empty(),
        "same-chunk movement must not touch the view: {noop:?}"
    );

    // A jump to chunk (10, 0): far enough that the initial 3x3 (centered (0,0))
    // and new 3x3 (centered (10,0)) windows share no columns at all.
    send_player_moved(&mut client, 160.0, 64.0, 0.0).await;
    let jump = drain_available(&mut client).await;
    let (center, forgotten, added) = split_view_directives(&jump);
    assert_eq!(center, Some((10, 0)));
    assert_eq!(forgotten, square(0, 0, view_radius));
    assert_eq!(added, square(10, 0, view_radius));
    send_chunk_batch_received(&mut client, 10.0).await;

    // One more chunk to the right: a partial diff. Exactly the trailing
    // (x = 9) column leaves, exactly the new (x = 12) column enters, and the
    // two shared columns (x = 10, 11) are touched by neither.
    send_player_moved(&mut client, 176.0, 64.0, 0.0).await;
    let shift = drain_available(&mut client).await;
    let (center2, forgotten2, added2) = split_view_directives(&shift);
    assert_eq!(center2, Some((11, 0)));
    assert_eq!(
        forgotten2,
        HashSet::from([(9, -1), (9, 0), (9, 1)]),
        "expected exactly the trailing column to be forgotten"
    );
    assert_eq!(
        added2,
        HashSet::from([(12, -1), (12, 0), (12, 1)]),
        "expected exactly the new column to be sent"
    );

    drop(client);
    let _ = server.await.unwrap();
}

/// **"If the player moves it should properly generate the closer chunks first."**
///
/// A move's newly-visible columns must reach the wire ordered by distance from the
/// player's *new* column, not lexicographically by `(cx, cz)`.
///
/// # The two hypotheses, and why this jump was chosen
///
/// The subject is a diagonal jump from chunk `(0, 0)` to `(2, 2)` at
/// `view_radius = 3`, which is the smallest move whose added set contains columns
/// at *different* distances — a straight one-axis step adds a single strip, all of
/// it equidistant, and could not tell the two orderings apart. The added set is
/// every column of `[-1, 5]²` outside `[-3, 3]²`, whose distances from `(2, 2)`
/// are 2 and 3.
///
/// | ordering | first column sent |
/// |---|---|
/// | lexicographic (`sort_unstable`) | `(4, -1)` — distance **3**, a corner behind the player |
/// | distance-first (`join_scheduler::view_order_key`) | distance **2** |
///
/// So the assertion is on the first column's distance *and* on monotonicity: the
/// first alone would be satisfied by a shuffle, and monotonicity alone would be
/// satisfied by a set that happened to be equidistant.
#[tokio::test]
async fn a_move_streams_the_new_columns_nearest_first() {
    let view_radius = 3;
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
    drive_login_and_join(&mut client, "Diagonal", 49).await;
    send_chunk_batch_received(&mut client, 10.0).await;

    // Block (40, 64, 40) is chunk (2, 2).
    send_player_moved(&mut client, 40.0, 64.0, 40.0).await;
    let moved = drain_available(&mut client).await;
    let added: Vec<(i32, i32)> = moved
        .iter()
        .filter(|(id, _)| *id == CHUNK)
        .map(|(_, payload)| {
            let mut r = Reader::new(payload);
            (r.var_i32().unwrap(), r.var_i32().unwrap())
        })
        .collect();
    assert!(
        !added.is_empty(),
        "a two-chunk diagonal jump must send new columns"
    );

    let distance = |(cx, cz): (i32, i32)| (cx - 2).abs().max((cz - 2).abs());
    assert_eq!(
        distance(added[0]),
        2,
        "the first column of a move must be one of the nearest ones; got {:?} at distance {}. \
         Lexicographic order would send (4, -1) at distance 3 first",
        added[0],
        distance(added[0])
    );
    let mut previous = 0;
    for &coord in &added {
        let d = distance(coord);
        assert!(
            d >= previous,
            "{coord:?} at distance {d} follows a column at distance {previous}: a move's batch \
             must be non-decreasing in distance from the player's new column"
        );
        previous = d;
    }

    drop(client);
    let _ = server.await.unwrap();
}
