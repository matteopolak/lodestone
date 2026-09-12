//! Consumer-level liveness proof for a slow chunk-boundary generation job.
//!
//! A client moves from chunk `0` to chunk `5`, which enqueues the newly visible
//! column `(8, 0)` for the real integrated view stream. That column is also in
//! the moved player's authoritative tick area. The source holds it behind a
//! gate. While it is held, the authoritative server tick clock
//! must advance, a button pressed before the move must be accepted by the play
//! consumer, and a subsequent client packet must still reach that consumer.
//!
//! The gate has an independent bounded release. That is the negative control:
//! if a future implementation awaits generation on the runtime's current
//! thread, the test cannot observe the tick or packet effects before the gate
//! auto-releases, and the explicit "still held" assertion fails.

#[path = "support/worldgen_liveness.rs"]
mod support;

use std::collections::HashSet;
use std::time::{Duration, Instant};

use lodestone_core::{Reader, Writer};
use lodestone_net::{Connection, Transport};
use lodestone_server::{ChunkSource, IntegratedServer};

use support::{
    ActionProbe, BLOCK_UPDATE, BLOCKED_COLUMN, BUTTON_OFF, BUTTON_ON, BUTTON_POS, CHUNK,
    CHUNK_BATCH_FINISHED, CHUNK_BATCH_START, CLIENT_ACTION, FINISH_CONFIGURATION, GenerationGate,
    HANDSHAKE, LivenessProtocol, LivenessWorld, LOGIN_ACKNOWLEDGED, LOGIN_START, LOGIN_SUCCESS,
    MOVE_PLAYER, USE_BUTTON,
};

const VIEW_RADIUS: i32 = 4;

async fn join_and_read_view<T: Transport>(
    client: &mut Connection<T>,
    expected_radius: i32,
) -> HashSet<(i32, i32)> {
    client
        .write_packet(HANDSHAKE, &[2])
        .await
        .expect("handshake");
    let mut login = Writer::default();
    login.string("WorldgenLiveness");
    client
        .write_packet(LOGIN_START, login.as_slice())
        .await
        .expect("login start");

    let (id, _) = client
        .read_packet_timeout(Duration::from_secs(10))
        .await
        .expect("login success read")
        .expect("login success packet");
    assert_eq!(id, LOGIN_SUCCESS);
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login acknowledgement");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("configuration finish");

    let side = usize::try_from(expected_radius * 2 + 1).expect("non-negative view side");
    let expected_count = side * side;
    let mut chunks = HashSet::with_capacity(expected_count);
    let mut saw_batch_finished = false;
    let deadline = Instant::now() + Duration::from_secs(30);
    while chunks.len() < expected_count || !saw_batch_finished {
        assert!(Instant::now() < deadline, "initial view did not finish in 30 seconds");
        let (id, payload) = client
            .read_packet_timeout(Duration::from_secs(10))
            .await
            .expect("join packet read")
            .expect("join packet");
        match id {
            CHUNK_BATCH_START => assert!(payload.is_empty(), "batch start has no body"),
            CHUNK => {
                let mut reader = Reader::new(&payload);
                let cx = reader.var_i32().expect("chunk x");
                let cz = reader.var_i32().expect("chunk z");
                assert!(reader.remaining() == 0, "stand-in chunk has trailing bytes");
                assert!(chunks.insert((cx, cz)), "duplicate chunk ({cx}, {cz})");
            }
            CHUNK_BATCH_FINISHED => {
                let mut reader = Reader::new(&payload);
                let batch_size = reader.var_i32().expect("batch size");
                assert!(batch_size > 0, "empty chunk batch");
                assert_eq!(reader.remaining(), 0, "batch marker has trailing bytes");
                saw_batch_finished = true;
            }
            _ => {}
        }
    }

    let expected: HashSet<_> = (-expected_radius..=expected_radius)
        .flat_map(|cx| (-expected_radius..=expected_radius).map(move |cz| (cx, cz)))
        .collect();
    assert_eq!(chunks, expected, "initial view must be the complete square");
    chunks
}

fn read_block_update(payload: &[u8]) -> (i32, i32, i32, String) {
    let mut reader = Reader::new(payload);
    let x = reader.i32().expect("block update x");
    let y = reader.i32().expect("block update y");
    let z = reader.i32().expect("block update z");
    let state = reader.string(256).expect("block update state");
    assert_eq!(reader.remaining(), 0, "block update has trailing bytes");
    (x, y, z, state)
}

async fn wait_for_button_state<T: Transport>(
    client: &mut Connection<T>,
    wanted: &str,
) {
    // The final assertion may sit behind the deferred join stream's remaining
    // chunk frames after the generation gate opens. This is a wire-observation
    // timeout, not the liveness budget; the latter is enforced separately below.
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(Instant::now() < deadline, "timed out waiting for button state {wanted}");
        let packet = match client
            .read_packet_timeout(Duration::from_millis(50))
            .await
        {
            Ok(packet) => packet,
            Err(_) => continue,
        };
        let Some((id, payload)) = packet else {
            panic!("server closed while waiting for button state {wanted}");
        };
        if id == BLOCK_UPDATE {
            let (x, y, z, state) = read_block_update(&payload);
            if (x, y, z) == BUTTON_POS {
                assert_eq!(state, wanted, "authoritative button state");
                return;
            }
        }
    }
}

async fn wait_for_generation_start<T: Transport>(
    client: &mut Connection<T>,
    gate: &GenerationGate,
) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while !gate.started() {
        assert!(
            Instant::now() < deadline,
            "the moved view never requested blocked column {BLOCKED_COLUMN:?}"
        );
        // Reading while the held worker emits its neighbouring columns keeps
        // the in-memory transport flowing, and the bounded timeout also gives
        // the current-thread runtime a chance to poll the gate worker.
        let _ = client.read_packet_timeout(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "current_thread")]
async fn integrated_ticks_and_play_packets_continue_during_held_worldgen() {
    let gate = GenerationGate::new(Duration::from_secs(5));
    let world = LivenessWorld::new(gate.clone());
    let actions = ActionProbe::default();
    let protocol = LivenessProtocol::new(actions.clone());
    let (server, client_io) = IntegratedServer::open_in_memory_with_mobs(
        protocol,
        world.clone(),
        (0..=0, 0..=0),
        (0, 0),
        0,
        VIEW_RADIUS,
    );
    // Keep the liveness witness focused on the generation handoff rather than
    // natural-spawn/light work over the deliberately broad test view. The
    // production tick loop remains the same; these rules make the expected
    // 20-TPS control deterministic on a shared CI host.
    server
        .world_state()
        .set_rule("spawn_mobs", "false")
        .expect("disable natural spawning for liveness control");
    server
        .world_state()
        .set_rule("random_tick_speed", "0")
        .expect("disable random ticks for liveness control");
    let mut client = Connection::new(client_io);

    let initial_view = join_and_read_view(&mut client, VIEW_RADIUS).await;
    assert_eq!(initial_view.len(), 81, "join precondition must be complete");
    assert_eq!(world.state(BUTTON_POS), BUTTON_OFF);

    // Move to the button's retained/ticked column before clicking it. The
    // later movement to chunk 5 is the held-generation trigger; using the
    // initial spawn position here would make the button unreachable after the
    // view recentres and would turn a valid deferred release into a test of
    // cold-column retry behavior.
    let mut button_movement = Writer::default();
    button_movement.f64(33.0);
    button_movement.f64(64.0);
    button_movement.f64(0.0);
    button_movement.bool(true);
    client
        .write_packet(MOVE_PLAYER, button_movement.as_slice())
        .await
        .expect("move to retained button column");

    let initial_tick = server
        .server_tick_count()
        .expect("integrated server exposes the ECS tick witness");
    let initial_stats = server
        .tick_stats()
        .expect("integrated server exposes tick statistics");

    // A real play action changes a button and queues its delayed release in the
    // server's scheduled-block-tick feed. This is the producer-side precondition
    // for the independent tick assertion below.
    client
        .write_packet(USE_BUTTON, &[])
        .await
        .expect("button click");
    wait_for_button_state(&mut client, BUTTON_ON).await;
    assert_eq!(world.state(BUTTON_POS), BUTTON_ON);
    assert_eq!(
        actions.count(),
        2,
        "the button click and preceding move reached the play decoder"
    );

    // Chunk 8 is outside the initial [−4, 4] view. Crossing into chunk 5 adds
    // the x=8 strip, and the tick area's radius-3 square reaches x=8 too. This
    // is what makes the gate a negative control for a tick loop that would
    // accidentally generate a cold tick-area column inline.
    let mut movement = Writer::default();
    movement.f64(80.0);
    movement.f64(64.0);
    movement.f64(0.0);
    movement.bool(true);
    client
        .write_packet(MOVE_PLAYER, movement.as_slice())
        .await
        .expect("cross-chunk movement");
    wait_for_generation_start(&mut client, &gate).await;
    assert!(
        !gate.auto_released(),
        "negative control: movement reached a held generation job without the fallback timeout"
    );

    // The action is sent after generation has definitely started. The play
    // decoder must still consume it while the moved-view stream waits on the
    // worker; a loop that awaited the source inline cannot reach this count.
    client
        .write_packet(CLIENT_ACTION, &[])
        .await
        .expect("client action during generation");
    let action_deadline = Instant::now() + Duration::from_secs(2);
    while actions.client_action_count() < 1 {
        assert!(
            Instant::now() < action_deadline,
            "client action was not decoded while generation was held (count={})",
            actions.client_action_count()
        );
        assert!(
            !gate.released(),
            "generation released before the client action was observed"
        );
        let _ = client.read_packet_timeout(Duration::from_millis(20)).await;
    }

    // Both public tick witnesses must advance while the gate remains held. The
    // button's delayed release is checked after the gate is released below;
    // keeping that assertion out of this bounded window avoids conflating the
    // tick-liveness witness with the button's normal twenty-tick hold period.
    let tick_deadline = Instant::now() + Duration::from_secs(4);
    loop {
        assert!(
            Instant::now() < tick_deadline,
            "authoritative ticking stalled during held generation: tick_count={}, server_tick_count={}, button={}, scheduled={}, stats={:?}",
            server.tick_stats().expect("tick stats").tick_count,
            server.server_tick_count().expect("server tick witness"),
            world.state(BUTTON_POS),
            server
                .tick_stats()
                .expect("tick stats")
                .owner_work
                .scheduled_block_ticks,
            server.tick_stats().expect("tick stats")
        );
        assert!(
            !gate.released(),
            "negative control: generation auto-released before the tick effects were observed"
        );
        let stats = server.tick_stats().expect("tick stats");
        if server.server_tick_count().expect("server tick witness") >= initial_tick + 5
            && stats.tick_count >= initial_stats.tick_count + 5
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    gate.release();
    let completion_deadline = Instant::now() + Duration::from_secs(2);
    while !gate.completed() {
        assert!(Instant::now() < completion_deadline, "generation worker did not finish");
        tokio::time::sleep(Duration::from_millis(5)).await;
    }

    drop(client);
    server.shutdown().await;
}

/// Instrumentation control: a synchronous source call on the current-thread
/// runtime really does hold the runtime until the gate's independent timeout.
/// The production test above is intentionally stricter: it runs the same source
/// through the integrated server and requires useful tick and packet effects
/// before this fallback release is allowed to happen.
#[tokio::test(flavor = "current_thread")]
async fn inline_generation_control_is_observably_blocking() {
    let gate = GenerationGate::new(Duration::from_millis(100));
    let world = LivenessWorld::new(gate.clone());
    let started = Instant::now();
    let worker = tokio::spawn(async move {
        let _ = world.column(BLOCKED_COLUMN.0, BLOCKED_COLUMN.1);
    });
    worker.await.expect("inline control worker");
    assert!(gate.started(), "control never entered the generation gate");
    assert!(
        gate.auto_released(),
        "control must use the bounded fallback release"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(90),
        "control returned before the deliberately blocking interval"
    );
}
