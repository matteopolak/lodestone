//! A LAN-hosted world must actually tick — exactly **once** per tick, no
//! matter how many players are connected.
//!
//! # Observables
//!
//! `IntegratedServer::bind` owns one `tick::run_tick_loop` for the world. The
//! test checks that the loop exists and advances through
//! `IntegratedServer::tick_stats()`, then compares tick-count deltas with zero
//! and two connected clients. The bound is between the one-loop and
//! one-loop-per-client predictions.
//!
//! A hand-written counting `ChunkSource` provides a structural check that the
//! loop visits its tick area. It is intentionally separate from the rate
//! measurement: cached columns may be read once while the tick counter advances
//! on every period, so source visits cannot serve as a rate proxy.
//!
//! The probe chunk lies inside the tick area and outside each connection's view.
//! That keeps its count attributable to world ticking rather than terrain
//! streaming. The source returns an all-air column, so random ticks do not add
//! unrelated feed traffic to this gate.
//!
//! # Measurement units
//!
//! Every reported count is a **delta** across a window opened and closed by this
//! test. `TickStats` belongs to one `IntegratedServer`, so deltas distinguish
//! progress during each measurement from ticks that occurred earlier.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use lodestone_core::{Reader, State, Writer};
use lodestone_net::Connection;
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, ServerBound, ServerDirective, ServerProtocol,
};
use uuid::Uuid;

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_SUCCESS: i32 = 2;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const FINISH_CONFIGURATION: i32 = 3;
const CHUNK_BATCH_START: i32 = 10;
const CHUNK: i32 = 0x27;
const CHUNK_BATCH_FINISHED: i32 = 11;

/// `run_tick_loop`'s real period (`tick::TICK_PERIOD`). Not importable — it is
/// `pub(crate)` — so it is restated here, which is fine because it is also the
/// number this gate's expectations are *derived from* rather than compared to.
const TICK_PERIOD_MILLIS: u128 = 50;

/// `integrated::LAN_TICK_RADIUS`, likewise `pub(crate)`. The tick loop visits
/// every chunk in `[-r, r]²` once per tick, so the probe chunk below must sit
/// inside this square.
const LAN_TICK_RADIUS: i32 = 2;

/// `tick::INITIAL_RANDOM_TICK_DEFERRAL_TICKS`, likewise `pub(crate)` and so
/// restated here.
///
/// **This is why the probe count needs a wait rather than a window.** The
/// random-tick pass is the only thing in `run_tick_loop` that calls
/// `world.column()`, and the deferral skips it while `game_tick <=` this value
/// — so a
/// gate that samples the probe after a fixed 1.5 s window read **0** and failed
/// its own structural control, which is exactly what it is for. Restated rather
/// than inferred from the observed count: the number a gate compares against
/// must come from the production knob, not from what the run happened to do.
const INITIAL_RANDOM_TICK_DEFERRAL_TICKS: u64 = 40;

/// Counts `ChunkSource::column` calls, split into a total and a single probe
/// chunk.
///
/// The probe chunk is what makes the count attributable. Connection tasks also
/// generate columns (for chunk streaming), so a total-only counter would mix
/// "the world ticked" with "a player was sent terrain". The probe chunk is
/// chosen inside the tick area but **outside** every connection's view, so its
/// count is purely the tick loop's.
#[derive(Clone, Default)]
struct TickProbe {
    probe_chunk_calls: Arc<AtomicU64>,
    total_calls: Arc<AtomicU64>,
}

impl TickProbe {
    fn probe(&self) -> u64 {
        self.probe_chunk_calls.load(Ordering::Relaxed)
    }

    fn total(&self) -> u64 {
        self.total_calls.load(Ordering::Relaxed)
    }
}

struct CountingChunkSource {
    probe: TickProbe,
    probe_at: (i32, i32),
}

impl ChunkSource for CountingChunkSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.probe.total_calls.fetch_add(1, Ordering::Relaxed);
        if (cx, cz) == self.probe_at {
            self.probe.probe_chunk_calls.fetch_add(1, Ordering::Relaxed);
        }
        // All air, deliberately: a random tick over an air column produces no
        // block change, so nothing is published into the tick loop's feeds and
        // this gate measures the loop's *rate* without also exercising the
        // fan-out relay (which `lan_join_still_works_with_the_tick_loop_running`
        // covers).
        ChunkColumn::new(-64, 32)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this gate
        // counts generations, not reads.
        let cx = x.div_euclid(16);
        let cz = z.div_euclid(16);
        let lx = x.rem_euclid(16);
        let lz = z.rem_euclid(16);
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn biome_state_at(&self, x: i32, y: i32, z: i32) -> String {
        // The column-regenerating form (correct, just not cheap); this gate
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

/// The seven non-defaulted `ServerProtocol` methods, with a private wire
/// vocabulary — the same shape `tests/integrated_memory.rs` uses.
struct FakeProtocol;

impl ServerProtocol for FakeProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match (state, packet_id) {
            (State::Handshaking, HANDSHAKE) => ServerBound::Handshake {
                next_state: State::Login,
            },
            (State::Login, LOGIN_START) => {
                let mut r = Reader::new(payload);
                ServerBound::LoginStart {
                    username: r.string(16).expect("username"),
                    uuid: Uuid::nil(),
                }
            }
            (State::Login, LOGIN_ACKNOWLEDGED) => ServerBound::LoginAcknowledged,
            (State::Configuration, FINISH_CONFIGURATION) => ServerBound::ConfigurationFinished,
            _ => ServerBound::Ignored,
        }
    }

    fn login_success(&self, username: &str, _uuid: Uuid) -> Vec<ServerDirective> {
        let mut w = Writer::default();
        w.string(username);
        vec![ServerDirective::Send {
            packet_id: LOGIN_SUCCESS,
            payload: w.as_slice().to_vec(),
        }]
    }

    fn begin_configuration(&self) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_play(&self, _view_radius: i32) -> Vec<ServerDirective> {
        Vec::new()
    }

    fn begin_chunk_batch(&self) -> ServerDirective {
        ServerDirective::Send {
            packet_id: CHUNK_BATCH_START,
            payload: Vec::new(),
        }
    }

    fn encode_chunk(&self, cx: i32, cz: i32, _column: &ChunkColumn) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(cx);
        w.var_i32(cz);
        ServerDirective::Send {
            packet_id: CHUNK,
            payload: w.as_slice().to_vec(),
        }
    }

    fn end_chunk_batch(&self, batch_size: i32) -> ServerDirective {
        let mut w = Writer::default();
        w.var_i32(batch_size);
        ServerDirective::Send {
            packet_id: CHUNK_BATCH_FINISHED,
            payload: w.as_slice().to_vec(),
        }
    }
}

/// Binds a LAN server over loopback on an OS-assigned port, with a counting
/// source whose probe chunk sits inside the tick area and outside a
/// `view_radius: 0` view.
async fn bind_lan(view_radius: i32) -> (IntegratedServer, TickProbe, std::net::SocketAddr) {
    let probe = TickProbe::default();
    let probe_at = (LAN_TICK_RADIUS, LAN_TICK_RADIUS);
    // Precondition, asserted rather than assumed: the probe must be somewhere
    // the tick loop visits and a connection's view does not. If either half
    // stopped holding, every count below would silently measure the wrong
    // thing.
    assert!(
        probe_at.0.abs() <= LAN_TICK_RADIUS && probe_at.1.abs() <= LAN_TICK_RADIUS,
        "probe chunk {probe_at:?} must lie inside the tick area"
    );
    assert!(
        probe_at.0.abs() > view_radius || probe_at.1.abs() > view_radius,
        "probe chunk {probe_at:?} must lie outside a view_radius={view_radius} view, or its \
         count would mix chunk streaming into the tick-rate measurement"
    );

    let source = CountingChunkSource {
        probe: probe.clone(),
        probe_at,
    };
    let server = IntegratedServer::bind("127.0.0.1:0", FakeProtocol, source, view_radius)
        .await
        .expect("bind loopback");
    let addr = server.local_addr().expect("a bound server has an address");
    (server, probe, addr)
}

/// Measures how many ticks the world advanced over a real wall-clock window,
/// returning `(delta, elapsed)`.
///
/// A **delta** across a window this function opens and closes, never an
/// absolute: `TickClock` accumulates over the whole life of an
/// `IntegratedServer`, so an absolute reading cannot distinguish "ticking now"
/// from "ticked earlier and stopped". Two of these bracketing two different
/// connection counts is what makes the ratio below meaningful.
/// Blocks until the world tick loop has completed at least one random-tick pass,
/// i.e. until its tick counter is past [`INITIAL_RANDOM_TICK_DEFERRAL_TICKS`].
///
/// Waits on the **tick counter**, not on wall-clock: 40 ticks is 2.0 s only at a
/// nominal 20 TPS, but a fixed-duration sleep is not a reliable substitute:
/// scheduler load can delay the loop and make the probe count look like a
/// source-path failure. Polling the counter keeps the wait tied to the event
/// that enables the structural check.
///
/// The timeout fails rather than skips, and reports the counter it reached, so a
/// genuinely stalled loop is distinguishable from a slow one.
async fn await_past_random_tick_deferral(server: &IntegratedServer) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let ticks = server
            .tick_stats()
            .expect("tick_stats() must stay Some for the life of the handle")
            .tick_count;
        if ticks > INITIAL_RANDOM_TICK_DEFERRAL_TICKS {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "the world tick loop reached only {ticks} ticks in 30s and must pass \
             INITIAL_RANDOM_TICK_DEFERRAL_TICKS ({INITIAL_RANDOM_TICK_DEFERRAL_TICKS}) before \
             the probe count means anything"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

async fn ticks_delta_over(server: &IntegratedServer, window: Duration) -> (u64, Duration) {
    let before = server
        .tick_stats()
        .expect("tick_stats() must stay Some for the life of the handle")
        .tick_count;
    let started = Instant::now();
    tokio::time::sleep(window).await;
    let elapsed = started.elapsed();
    let after = server
        .tick_stats()
        .expect("tick_stats() must stay Some for the life of the handle")
        .tick_count;
    (after - before, elapsed)
}

/// Both LAN tick invariants: the path advances, and it advances exactly once.
///
/// # Why `multi_thread` here, when `chunk.rs`'s generation-blocking gate
/// insists on `current_thread`
///
/// The two gates measure opposite things and want opposite runtimes, so this is
/// worth stating rather than leaving as an inconsistency for the next reader.
///
/// That other gate asks *"does generation block the runtime?"*. Its whole
/// mechanism is that one thread cannot do two things, so a second worker
/// thread would let its negative control pass and make it vacuous —
/// `current_thread` is load-bearing there, and it asserts the flavour.
///
/// This gate asks *"how many tick loops are there?"*, which is a property of
/// the count, not of thread scheduling. On `current_thread` the synchronous
/// work in the tick area competes with this test body for the one thread and
/// can starve its measurement window. A worker pool isolates the scheduler
/// while a duplicated loop still doubles the tick count, so the assertion
/// remains about loop multiplicity.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lan_bind_runs_exactly_one_world_tick_loop() {
    let (server, probe, addr) = bind_lan(0).await;

    // ---- Control 1: a LAN-bound server must expose its tick loop. ----
    // A handle without a loop returns `None` from `tick_stats()`, so this
    // precondition distinguishes a missing loop from one that is merely slow.
    let start_stats = server
        .tick_stats()
        .expect("#439: a LAN-bound server must have a world tick loop, so tick_stats() is Some");

    // ---- Assertion 1: the world actually advances, with zero players. ----
    // Zero players is deliberate: a tick loop that only ran while someone was
    // connected would still be wrong (hoppers near spawn must keep moving), and
    // it would also mean the count below was really measuring connection work.
    // 1.5 s rather than a few hundred ms: on a loaded machine the loop manages
    // under 3 TPS (see the table below), so a shorter window yields counts of
    // 2-3 and a ratio computed from those is noise.
    let window = Duration::from_millis(1500);
    // The loop defers its first random-tick pass, and that pass is
    // the only thing that reaches `ChunkSource::column`. Get past it before the
    // probe is read, or the structural control below measures a deferral rather than the
    // source path. Ordered before the rate window so the window itself does not
    // have to be long enough to cover the deferral.
    await_past_random_tick_deferral(&server).await;
    let (solo_delta, solo_elapsed) = ticks_delta_over(&server, window).await;

    assert!(
        solo_delta > 0,
        "#439: TickStats::ticks did not advance over {solo_elapsed:?} — the loop exists but is \
         not running"
    );
    // ---- Control 2: the loop really does iterate its tick area, through the
    // source. ----
    // Cached columns make this a one-shot structural check rather than a rate
    // instrument. Exactly one visit to the probe verifies that the tick area
    // reaches the source through the cache; a `> 0` check alone would also pass
    // if the loop bypassed caching and regenerated every tick.
    assert_eq!(
        probe.probe(),
        1,
        "the tick loop must have visited the probe chunk exactly once: 0 means it never \
         iterated its tick area (block entities, scheduled/fluid ticks and random ticks all \
         frozen), and more than 1 over {solo_elapsed:?} means #289's `ChunkStore` is not in \
         the source path and every column is being regenerated per tick. The #481 deferral \
         is already past by construction — see `await_past_random_tick_deferral` — so 0 here \
         is a real freeze, not a wait"
    );
    // `start_stats` exists only for Control 1's `expect`; the rate above is a
    // bracketed delta, per `ticks_delta_over`'s doc comment.
    let _ = start_stats;

    // Report the achieved rate against the nominal 20 Hz, but do **not** assert
    // on it. Absolute-rate assertions are sensitive to machine load and say
    // nothing about loop multiplicity; the comparison below uses two windows
    // from the same process instead.
    let nominal = (solo_elapsed.as_millis() / TICK_PERIOD_MILLIS) as u64;
    eprintln!(
        "solo: {solo_delta} ticks over {solo_elapsed:?} (nominal 20 Hz would be {nominal})"
    );

    // A floor so the ratio below is not computed from noise. This is a
    // precondition on the *measurement*, and it fails rather than skips.
    assert!(
        solo_delta >= 2,
        "only {solo_delta} ticks over {solo_elapsed:?} — too few to compare a second \
         measurement against; lengthen the window"
    );

    // ---- Assertion 2: two connections, still ONE loop. ----
    // Raw TCP, no protocol handshake, on purpose: the wrong implementation this
    // catches spawns its loop in the accept arm, which fires the moment the
    // socket is accepted and before any packet is read. Two accepted sockets is
    // therefore the minimum that distinguishes it.
    let conn_a = tokio::net::TcpStream::connect(addr)
        .await
        .expect("first LAN client connects");
    let conn_b = tokio::net::TcpStream::connect(addr)
        .await
        .expect("second LAN client connects");
    // Give the accept loop room to actually accept both before measuring.
    tokio::time::sleep(Duration::from_millis(100)).await;

    let (duo_delta, duo_elapsed) = ticks_delta_over(&server, window).await;
    eprintln!("duo: {duo_delta} ticks over {duo_elapsed:?}");

    // The comparison is a **ratio against the solo measurement taken moments
    // ago in this same process**, not against a theoretical 20 Hz. That is what
    // makes it immune to build profile and machine speed while still separating
    // the two hypotheses cleanly: one world loop predicts duo ≈ solo, one loop
    // per connection predicts duo ≈ 2 × solo. Rates rather than raw counts,
    // because the two windows are not exactly equal in wall-clock.
    let solo_rate = solo_delta as f64 / solo_elapsed.as_secs_f64();
    let duo_rate = duo_delta as f64 / duo_elapsed.as_secs_f64();
    eprintln!("solo_rate={solo_rate:.2}/s duo_rate={duo_rate:.2}/s ratio={:.2}", duo_rate / solo_rate);

    // The load-bearing bound. 1.5× sits strictly between the two hypotheses, so
    // this fails on a per-connection tick loop and passes on a per-world one.
    // Without this assertion, a loop spawned per connection could satisfy the
    // other checks while advancing the world at the wrong speed.
    assert!(
        duo_rate <= solo_rate * 1.5,
        "#439: the world advanced at {duo_rate:.2} ticks/s with 2 connections versus \
         {solo_rate:.2}/s with none. One world tick loop predicts ≈{solo_rate:.2}; one loop *per \
         connection* predicts ≈{:.2}. This is at or past the halfway mark, so the tick loop looks \
         per-connection — the world would run at N× speed with N players",
        solo_rate * 2.0
    );
    assert!(
        duo_rate >= solo_rate * 0.5,
        "#439: the world advanced at only {duo_rate:.2} ticks/s with 2 connections versus \
         {solo_rate:.2}/s with none — connecting players must not stall the world tick"
    );

    drop(conn_a);
    drop(conn_b);
    server.shutdown().await;
}

/// The other half of "nothing is done until something changes on screen": the
/// tick loop must not have broken LAN serving, and a LAN client must still
/// receive its terrain.
///
/// This also exercises the non-blocking generation and per-connection feed pair
/// used by `IntegratedServer::bind`'s connection service; neither is touched by
/// the rate gate above.
///
/// `multi_thread` for the same reason as the gate above.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn lan_join_still_works_with_the_tick_loop_running() {
    let view_radius = 1;
    let probe = TickProbe::default();
    let source = CountingChunkSource {
        probe: probe.clone(),
        // Outside a radius-1 view, inside the tick area.
        probe_at: (LAN_TICK_RADIUS, LAN_TICK_RADIUS),
    };
    let server = IntegratedServer::bind("127.0.0.1:0", FakeProtocol, source, view_radius)
        .await
        .expect("bind loopback");
    let addr = server.local_addr().expect("bound address");

    let stream = tokio::net::TcpStream::connect(addr)
        .await
        .expect("LAN client connects");
    let mut client = Connection::new(stream);

    client.write_packet(HANDSHAKE, &[2]).await.expect("handshake");
    let mut w = Writer::default();
    w.string("LanTicker");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");

    let (id, payload) = client
        .read_packet()
        .await
        .expect("read login success")
        .expect("login success present");
    assert_eq!(id, LOGIN_SUCCESS, "expected LOGIN_SUCCESS, got {id}");
    assert_eq!(
        Reader::new(&payload).string(16).expect("username"),
        "LanTicker"
    );

    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    let (id, _) = client
        .read_packet()
        .await
        .expect("read batch start")
        .expect("batch start present");
    assert_eq!(id, CHUNK_BATCH_START, "expected CHUNK_BATCH_START, got {id}");

    let expected_chunks = ((2 * view_radius + 1) * (2 * view_radius + 1)) as usize;
    for i in 0..expected_chunks {
        let (id, _) = client
            .read_packet()
            .await
            .expect("read chunk")
            .expect("chunk present");
        assert_eq!(id, CHUNK, "expected CHUNK at index {i}, got {id}");
    }

    let (id, payload) = client
        .read_packet()
        .await
        .expect("read batch finished")
        .expect("batch finished present");
    assert_eq!(
        id, CHUNK_BATCH_FINISHED,
        "expected CHUNK_BATCH_FINISHED, got {id}"
    );
    assert_eq!(
        Reader::new(&payload).var_i32().expect("batch size"),
        expected_chunks as i32,
        "the LAN join burst must still send the whole view"
    );

    // And the world is ticking alongside the connection — the two are not
    // mutually exclusive, which is the entire point of this file.
    //
    // The sleep is not padding: a join over loopback completes in a couple of
    // milliseconds, so at 50 ms per tick the loop can legitimately not have run
    // even once by this point. Asserting straight after the join measured
    // exactly that and failed — a precondition-species mistake in the gate, not
    // a defect in the loop.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        probe.probe() > 0,
        "the tick loop must keep running while a LAN client is connected"
    );
    assert!(
        probe.total() > probe.probe(),
        "the join burst must have generated columns of its own, distinct from the tick loop's"
    );

    drop(client);
    server.shutdown().await;
}
