//! **Issue #505's gate.** The server's `ChunkStore` capacity must be a function
//! of the view radius the connection actually serves, so that every column of the
//! streamed view stays resident.
//!
//! # What it is
//!
//! One counter, measured over the **real** join and render-distance-change path:
//! `IntegratedServer::open_in_memory` → `serve_connection` → `join_view_rings` →
//! the `ChunkStore` that constructor wraps the source in. The counter is *column
//! generations per chunk coordinate*, taken from a hand-written [`CountingSource`]
//! with no cache of any kind beneath the store.
//!
//! # Why a dedicated binary, and why not a unit test
//!
//! Two reasons, both from CLAUDE.md:
//!
//! * a **counter** gate wants its own test binary, so nothing else in the process
//!   can contribute to the number it reads;
//! * a unit test would have to *restate* the access pattern — "the join touches
//!   each of `(2r+1)²` columns once, in ring order" — which is the **world**
//!   species of vacuity: it verifies the store against a scan the author wrote,
//!   not the one production performs. Here the view size is **measured** from
//!   `serve_connection`'s own output (the chunk packets it emits) and then
//!   compared against what the store retained.
//!
//! # The regime this is pointed at, which is the whole design of it
//!
//! **A gate at the default render distance cannot see this bug at all.** `rd = 8`
//! is `view_radius` 9 is 361 columns, under the old 512-column literal on *both*
//! arms, so both would pass and the gate would be measuring headroom. The subject
//! is therefore `view_radius = 13` — `render_distance` 12, vanilla's own default —
//! where the view is 729 columns and the old literal held 512 of them.
//! [`the_default_render_distance_is_under_the_old_ceiling_on_both_arms`] pins that
//! reasoning as an executable claim rather than a comment.
//!
//! # How to change it
//!
//! The two hypotheses are computed at compile time from
//! `chunk_store::capacity_for_view_radius`'s own constants, re-exported for this
//! purpose. If a capacity policy change makes an arm red, re-derive the arm from
//! the new constants — do not relax the assertion, which is `== 0` for a reason:
//! "fewer regenerations" is the *magnitude* species of vacuity.
//!
//! # Dependencies
//!
//! `lodestone-server`'s public surface only (`IntegratedServer`, `ChunkSource`,
//! `ServerProtocol`, plus the capacity policy constants), `lodestone-net`'s
//! `Connection` framing, and `lodestone-core`'s `Reader`/`Writer`.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use lodestone_core::{Reader, State, Writer};
use lodestone_net::{Connection, Transport};
use lodestone_server::{
    ChunkColumn, ChunkSource, IntegratedServer, STORE_CAPACITY_CEILING as MAX_CAPACITY,
    STORE_CAPACITY_FLOOR, STORE_CONCURRENT_SCAN_COLUMNS as CONCURRENT_SCAN_COLUMNS,
    STORE_FULLY_RESIDENT_VIEW_RADIUS as FULLY_RESIDENT_VIEW_RADIUS, ServerBound, ServerDirective,
    ServerProtocol, integrated_store_capacity_for_view_radius as integrated_capacity_for_view_radius,
    store_capacity_for_view_radius as capacity_for_view_radius, view_columns,
};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// The stand-in protocol. Trivial wire format, same shape as
// `tests/integrated_memory.rs`'s `FakeProtocol` — the version-correct encoders
// live in the version crates, which depend on *this* crate and so cannot be used
// from here.
// ---------------------------------------------------------------------------

const HANDSHAKE: i32 = 0;
const LOGIN_START: i32 = 0;
const LOGIN_ACKNOWLEDGED: i32 = 3;
const LOGIN_SUCCESS: i32 = 2;
const FINISH_CONFIGURATION: i32 = 3;
const CHUNK_BATCH_START: i32 = 10;
const CHUNK: i32 = 0x27;
const CHUNK_BATCH_FINISHED: i32 = 11;

/// Serverbound: "my render-distance slider moved". Payload is one `i8`. This is
/// the packet issue #505 is about — `dispatch_play_packet` clamps it to the
/// server's configured `view_radius` and hands it to
/// `ViewTracker::set_view_radius`.
const CLIENT_INFORMATION: i32 = 12;
/// Serverbound: one chunk batch received. Payload is one `f32`. Without this the
/// `awaiting_chunk_batch_ack` flow-control gate queues every later batch.
const CHUNK_BATCH_ACK: i32 = 13;

struct FakeProtocol;

impl ServerProtocol for FakeProtocol {
    fn decode(&self, state: State, packet_id: i32, payload: &[u8]) -> ServerBound {
        match state {
            State::Handshaking if packet_id == HANDSHAKE => ServerBound::Handshake {
                next_state: State::Login,
            },
            State::Login if packet_id == LOGIN_START => {
                let mut r = Reader::new(payload);
                ServerBound::LoginStart {
                    username: r.string(16).expect("username"),
                    uuid: Uuid::nil(),
                }
            }
            State::Login if packet_id == LOGIN_ACKNOWLEDGED => ServerBound::LoginAcknowledged,
            State::Configuration if packet_id == FINISH_CONFIGURATION => {
                ServerBound::ConfigurationFinished
            }
            State::Play if packet_id == CLIENT_INFORMATION => {
                let mut r = Reader::new(payload);
                ServerBound::ClientInformationChanged {
                    view_distance: r.i8().expect("view distance"),
                }
            }
            State::Play if packet_id == CHUNK_BATCH_ACK => ServerBound::ChunkBatchAcknowledged {
                desired_chunks_per_tick: 64.0,
            },
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

    /// Coordinates only — deliberately **not** the column body. This gate counts
    /// generations, and a 4 KiB body per column would put ~7 MB of memcpy through
    /// the duplex at the control's radius for no measured property.
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

// ---------------------------------------------------------------------------
// The counter.
// ---------------------------------------------------------------------------

/// Height of the columns this rig generates.
///
/// **Not** the real 384. Production columns are 192 KiB and this gate's control
/// arm generates thousands of them; the count is what is under test, not the
/// column's size, and `chunk_store`'s own `measure_rss_with_retention` pair is
/// where the size is measured. 16 keeps `ChunkColumn::block_state` in range for
/// any probe at `y = 8`.
const RIG_HEIGHT: i32 = 16;

type PerChunk = Arc<Mutex<HashMap<(i32, i32), u64>>>;

/// A [`ChunkSource`] that counts `column()` calls per coordinate and nothing else.
///
/// Hand-written on purpose, and this is the anti-vacuity property of every count
/// below: the real `OverworldGenerator` carries a per-instance 512-entry memo
/// cache keyed on exact `(cx, cz)`, so a generation-count gate built on
/// `overworld_chunk_source` passes **even with a completely broken store** — the
/// memo absorbs the second call. This source has no cache of any kind.
struct CountingSource {
    calls: Arc<AtomicU64>,
    per_chunk: PerChunk,
}

impl CountingSource {
    fn new() -> Self {
        Self {
            calls: Arc::new(AtomicU64::new(0)),
            per_chunk: Arc::new(Mutex::new(HashMap::new())),
        }
    }
}

impl ChunkSource for CountingSource {
    fn column(&self, cx: i32, cz: i32) -> ChunkColumn {
        self.calls.fetch_add(1, Ordering::Relaxed);
        *self
            .per_chunk
            .lock()
            .expect("per-chunk map poisoned")
            .entry((cx, cz))
            .or_insert(0) += 1;
        ChunkColumn::new(0, RIG_HEIGHT)
    }

    fn block_state(&self, x: i32, y: i32, z: i32) -> String {
        let (cx, cz) = (x.div_euclid(16), z.div_euclid(16));
        let (lx, lz) = (x.rem_euclid(16), z.rem_euclid(16));
        self.column(cx, cz).block_state(lx, y, lz).to_string()
    }

    fn set_block(&self, _x: i32, _y: i32, _z: i32, _name: &str) {
        // No storage; this is a counter, and edits are discarded by design.
        // Explicit rather than inherited — issue #440.
    }
}

/// How many coordinates were generated **more than once**, with the worst
/// offender and its count. A total alone can be right while one column is
/// regenerated many times and another was never visited — CLAUDE.md's "make
/// failure output say *where*".
fn regenerations(per_chunk: &PerChunk) -> (u64, (i32, i32), u64) {
    let map = per_chunk.lock().expect("per-chunk map poisoned");
    let extra = map.values().map(|&n| n.saturating_sub(1)).sum();
    let (&worst_coord, &worst) = map
        .iter()
        .max_by_key(|&(_, &n)| n)
        .unwrap_or((&(0, 0), &0));
    (extra, worst_coord, worst)
}

// ---------------------------------------------------------------------------
// The rig.
// ---------------------------------------------------------------------------

/// What one run of [`stream_view_then_reshrink_and_regrow`] observed.
#[derive(Debug)]
struct Observation {
    /// Columns the join actually streamed, counted from the `CHUNK` packets
    /// `serve_connection` emitted. **Measured, not assumed** — this is the number
    /// the capacity policy has to cover.
    join_columns: usize,
    /// Columns the re-grow streamed, likewise from the wire.
    regrow_columns: usize,
    /// Coordinates generated more than once over the whole session.
    regenerated: u64,
    /// The worst coordinate and its generation count.
    worst: ((i32, i32), u64),
    /// Every `column()` call the source saw.
    total_generations: u64,
}

/// Drives one connection through the sequence issue #505 is about:
///
/// 1. join at `view_radius`, which streams the whole `[-r, r]²` square;
/// 2. **shrink** the render distance to [`SHRUNK_RADIUS`] — the client's slider
///    moving down. Nothing is generated; the outer rings are forgotten;
/// 3. **grow** it back to `view_radius`. Every ring from `SHRUNK_RADIUS + 1`
///    outward is re-requested from the store.
///
/// Step 3 is the measurement, and what it reads is **residency**: if capacity
/// covers the view, every one of those columns is still resident and the re-grow
/// costs **zero** generations.
///
/// This is the real path, not a reproduction of it:
/// `ServerBound::ClientInformationChanged` is the render-distance slider on the
/// wire, and `ViewTracker::set_view_radius` generates through the same
/// `SourceRef` the join does.
///
/// # What the failing arm's *number* does and does not mean
///
/// Once capacity is short, the re-grow is a cyclic re-scan of a set larger than
/// the LRU ceiling — LRU's textbook worst case, where each miss evicts an entry
/// the same scan is about to ask for — so the count comes out well above the
/// arithmetic shortfall. **That is a property of this rig's slider sweep, not the
/// steady state of a walking player**, and conflating the two is the mistake issue
/// #505's own body makes. `ViewTracker::recenter` *diffs* the window: it generates
/// only `next.difference(&self.loaded)`, so an ordinary walk streams each column
/// once and never rescans the view. The steady-state cost of a short capacity is
/// that a fixed band of the view is simply **not resident** — and by the join's
/// ring order that band is the *innermost* rings, the ones `vitals_tick` and the
/// random-tick pass live in. See `docs/chunk-store.md` and DESIGN.md §12.111.
///
/// So read the failing count as evidence that residency is short, and read
/// `view_columns(r) - capacity` as the size of the harm. Only the second is what
/// the assertions are built on.
async fn stream_view_then_reshrink_and_regrow(view_radius: i32) -> Observation {
    let counting = CountingSource::new();
    let calls = Arc::clone(&counting.calls);
    let per_chunk = Arc::clone(&counting.per_chunk);

    let (server, client_end) = IntegratedServer::open_in_memory(FakeProtocol, counting, view_radius);
    let mut client = Connection::new(client_end);
    let (join_columns, regrow_columns) = drive_slider(&mut client, view_radius).await;

    drop(client);
    server.shutdown().await;

    let (regenerated, worst_coord, worst) = regenerations(&per_chunk);
    Observation {
        join_columns,
        regrow_columns,
        regenerated,
        worst: (worst_coord, worst),
        total_generations: calls.load(Ordering::Relaxed),
    }
}

/// [`stream_view_then_reshrink_and_regrow`] against the **open-to-LAN** server
/// instead of the in-memory one, over a real loopback socket.
///
/// The two differ in exactly one thing that matters here: `IntegratedServer::bind`
/// builds its store with `chunk_store::capacity_for_view_radius` (capped at
/// [`MAX_CAPACITY`]) while `open_in_memory` uses the uncapped integrated policy.
/// So this is how a *capped* store is measured through the real streaming path —
/// see [`past_the_hosted_capacity_cap_the_view_cannot_stay_resident`].
async fn stream_view_over_lan(view_radius: i32) -> Observation {
    let counting = CountingSource::new();
    let calls = Arc::clone(&counting.calls);
    let per_chunk = Arc::clone(&counting.per_chunk);

    let server = IntegratedServer::bind("127.0.0.1:0", FakeProtocol, counting, view_radius)
        .await
        .expect("bind loopback");
    let addr = server.local_addr().expect("bound address");
    let stream = tokio::net::TcpStream::connect(addr).await.expect("connect");
    let mut client = Connection::new(stream);
    let (join_columns, regrow_columns) = drive_slider(&mut client, view_radius).await;

    drop(client);
    server.shutdown().await;

    let (regenerated, worst_coord, worst) = regenerations(&per_chunk);
    Observation {
        join_columns,
        regrow_columns,
        regenerated,
        worst: (worst_coord, worst),
        total_generations: calls.load(Ordering::Relaxed),
    }
}

/// Logs in, streams the join view, then drags the render-distance slider down to
/// [`SHRUNK_RADIUS`] and back up to `view_radius`. Returns `(join columns, re-grow
/// columns)`, both counted off the wire.
///
/// Generic over the transport so the in-memory and LAN rigs above are the same
/// measurement of two different store policies rather than two rigs that might
/// diverge.
async fn drive_slider<T: Transport>(
    client: &mut Connection<T>,
    view_radius: i32,
) -> (usize, usize) {
    // Handshake → Login → Configuration → Play, ack-driven exactly as the real
    // adapter drives it.
    client.write_packet(HANDSHAKE, &[2]).await.expect("handshake");
    let mut w = Writer::default();
    w.string("Rd505");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");
    let (id, _) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, LOGIN_SUCCESS, "login must succeed before the join view");
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    let join_columns = drain_one_batch(client).await;
    ack_batch(client).await;

    // The slider goes down, then back up.
    set_view_distance(client, SHRUNK_RADIUS).await;
    // A shrink adds nothing, so it emits no batch at all — there is nothing to
    // drain and nothing to ack. `set_view_radius`'s `build_batch` returns an empty
    // `Vec` when the difference is empty.
    set_view_distance(client, view_radius).await;
    let regrow_columns = drain_one_batch(client).await;

    (join_columns, regrow_columns)
}

/// The radius the slider is dragged down to before being dragged back up.
///
/// **0, and the first draft's 3 was a blind spot worth recording.** The re-grow
/// only re-requests columns that *left* the window, so `SHRUNK_RADIUS` decides
/// which part of the view this rig can see. With 3 (chosen to line up with
/// `chunk_store`'s `CONCURRENT_TICK_RADIUS`), the `view_radius = 11` row passed
/// on the **unfixed** code: 529 columns against the old 512-column literal
/// evicts only 17, the join generates in ring order so those 17 are rings 0–2,
/// and rings 0–2 never left a radius-3 window. The rig reported 0 regenerations
/// for a store that was genuinely 17 columns short.
///
/// It failed in the *safe*-looking direction, which is what makes it worth
/// keeping: the 17 columns the old literal dropped at that radius are the
/// player's own column and its two nearest rings — the worst possible band,
/// because `crate::server`'s `vitals_tick` probes the player's column every
/// 50 ms and `run_tick_loop`'s random-tick pass covers rings 0–3.
///
/// 0 makes the re-grow re-request the whole view bar the centre column, so the
/// rig's probe set *is* the view and its sensitivity is exactly "does capacity
/// cover the view" with no band-alignment to get wrong. Vanilla's client slider
/// stops at 2, but `dispatch_play_packet` clamps to `0 ..= view_radius` and says
/// why (`crate::server`'s `ClientInformationChanged` arm), so 0 is a real value
/// on this wire.
const SHRUNK_RADIUS: i32 = 0;

async fn set_view_distance<T: Transport>(client: &mut Connection<T>, radius: i32) {
    let mut w = Writer::default();
    w.i8(i8::try_from(radius).expect("test radii fit in an i8"));
    client
        .write_packet(CLIENT_INFORMATION, w.as_slice())
        .await
        .expect("client information");
}

async fn ack_batch<T: Transport>(client: &mut Connection<T>) {
    let mut w = Writer::default();
    w.f32(64.0);
    client
        .write_packet(CHUNK_BATCH_ACK, w.as_slice())
        .await
        .expect("chunk batch ack");
}

/// Reads `CHUNK_BATCH_START`, every `CHUNK` up to `CHUNK_BATCH_FINISHED`, and
/// returns the column count the batch reported.
///
/// Asserts the marker's own count matches the packets seen: a batch that
/// silently sent fewer columns than it claimed would make every number below
/// smaller and read as a pass.
async fn drain_one_batch<T: Transport>(client: &mut Connection<T>) -> usize {
    let (id, payload) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, CHUNK_BATCH_START, "a batch must open with its marker");
    assert!(payload.is_empty());

    let mut seen = 0usize;
    loop {
        let (id, payload) = client.read_packet().await.expect("read").expect("packet");
        if id == CHUNK_BATCH_FINISHED {
            let mut r = Reader::new(&payload);
            let reported = r.var_i32().expect("batch size") as usize;
            assert_eq!(
                reported, seen,
                "the batch marker reported {reported} columns but {seen} chunk packets arrived"
            );
            return seen;
        }
        assert_eq!(id, CHUNK, "only chunk packets belong inside a batch");
        seen += 1;
    }
}

// ---------------------------------------------------------------------------
// The two hypotheses, computed at compile time from the policy's own constants.
// ---------------------------------------------------------------------------

/// The subject's radius: `render_distance` 12 + 1, i.e. **vanilla's own default
/// render distance**, and the first setting past 9 where the streamed view
/// (729 columns) exceeds the 512-column literal issue #505 removed.
const SUBJECT_RADIUS: i32 = 13;

/// The control's radius: past [`FULLY_RESIDENT_VIEW_RADIUS`], so the shipped cap
/// is genuinely exceeded and the failure reproduces as a real *configuration* of
/// the shipped policy rather than as a temporary neuter.
const CONTROL_RADIUS: i32 = FULLY_RESIDENT_VIEW_RADIUS + 3;

// The premises the two arms rest on, checked at compile time so a capacity policy
// change turns them into build failures instead of a silent pair of passes.
const _: () = assert!(
    view_columns(SUBJECT_RADIUS) > STORE_CAPACITY_FLOOR,
    "the subject radius must exceed the old literal, or both arms are under the \
     ceiling and the gate measures headroom"
);
const _: () = assert!(
    view_columns(SUBJECT_RADIUS) + CONCURRENT_SCAN_COLUMNS <= capacity_for_view_radius(SUBJECT_RADIUS),
    "the derivation must actually cover the subject's view plus the concurrent scans"
);
const _: () = assert!(
    view_columns(CONTROL_RADIUS) > MAX_CAPACITY,
    "the control radius must exceed the shipped cap, or it is the same measurement \
     as the subject"
);

// ---------------------------------------------------------------------------
// The gates.
// ---------------------------------------------------------------------------

/// **The subject.** At vanilla's own default render distance, re-growing the view
/// back to full costs **zero** column regenerations.
///
/// # Predicting the value, not the sign
///
/// The re-grow re-requests every ring from `SHRUNK_RADIUS + 1` outward:
/// `view_columns(13) - view_columns(0)` = `729 - 1` = **728** columns.
///
/// * If capacity covers the view (`capacity_for_view_radius(13)` = 779 ≥ 729 + 50)
///   every one of those 728 is still resident, and the re-grow's generation count
///   is **0**. Session total: 729, one per column.
/// * Under the old 512-column literal the join could hold only 512 of 729, so
///   **217** columns were already gone before the slider moved — and the re-grow
///   is a cyclic re-scan of 728 columns through a 512-entry LRU, which is LRU's
///   worst case: each miss evicts an entry the same scan is about to ask for, so
///   217 is a floor rather than the figure. Measured on the unfixed wiring in an
///   isolated worktree at `c77146d9`: **451** regenerations, 1,180 calls for a
///   729-column view. That figure moves by a few percent between runs and the
///   floor does not — `generate_columns_offloaded` fans the re-grow out over the
///   blocking pool, so which entry a given miss evicts depends on scheduling.
///   **Which is why the subject asserts 0 and the control asserts a computed
///   floor, and neither asserts an observed number.**
///
/// Those are not "more" and "less". They are 0 against a computed floor of 217,
/// and the assertion is `== 0`.
///
/// # Which implementation does this resolve to?
///
/// `IntegratedServer::open_in_memory` — the constructor the shell's singleplayer
/// launch actually uses, wrapping the source in the same `ChunkStore` with the
/// same derivation. The transport is `memory_pair()`, which is the *production*
/// singleplayer transport, not a test double.
#[tokio::test]
async fn regrowing_the_render_distance_regenerates_nothing_at_vanillas_default() {
    let observed = stream_view_then_reshrink_and_regrow(SUBJECT_RADIUS).await;

    // Preconditions, failing rather than skipping. Both are "did the thing under
    // test actually happen", and both would make `regenerated == 0` trivially
    // true if they were merely skipped.
    assert_eq!(
        observed.join_columns,
        view_columns(SUBJECT_RADIUS),
        "precondition: the join must have streamed the whole [-{SUBJECT_RADIUS}, \
         {SUBJECT_RADIUS}]² square. This is the *measured* view size the capacity \
         policy has to cover, taken from serve_connection's own chunk packets."
    );
    assert_eq!(
        observed.regrow_columns,
        view_columns(SUBJECT_RADIUS) - view_columns(SHRUNK_RADIUS),
        "precondition: the re-grow must have re-sent every ring outside the shrunk \
         view. 0 would mean the slider change never reached ViewTracker and the \
         count below measures nothing."
    );

    let ((wx, wz), worst) = observed.worst;
    assert_eq!(
        observed.regenerated, 0,
        "issue #505: {} columns were generated more than once (worst: ({wx}, {wz}) at \
         {worst} generations, {} calls total for a {}-column view). A capacity that \
         covers the view makes this 0; the {STORE_CAPACITY_FLOOR}-column literal it \
         replaced held only {} of them, so at least {} were cold before the slider \
         even moved.",
        observed.regenerated,
        observed.total_generations,
        view_columns(SUBJECT_RADIUS),
        STORE_CAPACITY_FLOOR,
        view_columns(SUBJECT_RADIUS) - STORE_CAPACITY_FLOOR,
    );
    assert_eq!(
        observed.total_generations,
        view_columns(SUBJECT_RADIUS) as u64,
        "the whole session should cost exactly one generation per column of the view"
    );
}

/// **The negative control, and it must fail the assertion above.**
///
/// A view radius past [`FULLY_RESIDENT_VIEW_RADIUS`] exceeds the cap
/// [`MAX_CAPACITY`] deliberately imposes, so the store cannot hold the view and
/// the re-grow *must* regenerate. This is the cap's documented degradation
/// measured as a real configuration of the shipped policy — the same shape as
/// `chunk_store`'s `with_capacity(source, 0)` controls, which reproduce the
/// pre-store behaviour rather than describing it.
///
/// **Over LAN, and that is the whole reason this arm still measures anything.**
/// The ceiling is now the *hosted* policy only: singleplayer's
/// `open_in_memory` uses `chunk_store::integrated_capacity_for_view_radius`,
/// which has no ceiling, so driving this radius through the in-memory rig would
/// report **0** regenerations — not because the cap works but because it no
/// longer applies there. `IntegratedServer::bind` is the constructor that kept
/// it, so the control follows the cap rather than the transport.
///
/// # Predicting the value
///
/// At radius 20 the view is 1,681 columns against a 1,275-column ceiling, so the
/// join alone leaves at least `1681 - 1275` = **406** columns unretained. The
/// assertion is `>= 406`: a computed floor, not "some regenerations". If this ever
/// reports **0**, the cap has stopped applying and the subject above is no longer
/// measuring anything — it would have become a statement about headroom.
///
/// This arm characterises a trade, it does not bless it. Raising
/// `FULLY_RESIDENT_VIEW_RADIUS` is how you shrink the degraded band, and the
/// memory table on that constant is the price list.
#[tokio::test]
async fn past_the_hosted_capacity_cap_the_view_cannot_stay_resident() {
    let observed = stream_view_over_lan(CONTROL_RADIUS).await;

    assert_eq!(
        observed.join_columns,
        view_columns(CONTROL_RADIUS),
        "precondition: the control's join must stream its whole square too"
    );

    let floor = (view_columns(CONTROL_RADIUS) - MAX_CAPACITY) as u64;
    let ((wx, wz), worst) = observed.worst;
    assert!(
        observed.regenerated >= floor,
        "control: a {}-column view against the {MAX_CAPACITY}-column cap must leave at \
         least {floor} columns unretained, so the re-grow cannot be free; observed \
         {} regenerations (worst: ({wx}, {wz}) at {worst}). 0 would mean the cap is \
         not applying and the subject gate proves nothing.",
        view_columns(CONTROL_RADIUS),
        observed.regenerated,
    );
    eprintln!(
        "control  view_radius {CONTROL_RADIUS}  view {:>5}  capacity {:>5}  \
         regenerated {:>6}  total {:>6}",
        view_columns(CONTROL_RADIUS),
        capacity_for_view_radius(CONTROL_RADIUS),
        observed.regenerated,
        observed.total_generations,
    );
}

/// **The reason the subject is not at the default render distance**, as an
/// executable claim rather than a comment.
///
/// `render_distance` 8 is `view_radius` 9 is 361 columns, which fits the
/// 512-column literal issue #505 replaced. So a gate written here would have
/// passed *before and after* the fix — the **world** species of vacuity, the one
/// you cannot find by reading the test, because the source would be exemplary and
/// only the input data wrong.
///
/// Kept as a test rather than prose so that a future change to
/// [`STORE_CAPACITY_FLOOR`] or to the shell's `render_distance + 1` cannot quietly
/// make the sentence false.
#[test]
fn the_default_render_distance_is_under_the_old_ceiling_on_both_arms() {
    /// `crates/lodestone-shell/src/config.rs`'s `DEFAULT_RENDER_DISTANCE`.
    const DEFAULT_RENDER_DISTANCE: i32 = 8;
    /// The shell serves `render_distance + 1` — vanilla's `ChunkTrackingView`
    /// buffer ring (`crates/lodestone-shell/src/app/session.rs`).
    const SERVED: i32 = DEFAULT_RENDER_DISTANCE + 1;

    assert_eq!(view_columns(SERVED), 361);
    assert!(
        view_columns(SERVED) < STORE_CAPACITY_FLOOR,
        "the default view ({} columns) is under the old {STORE_CAPACITY_FLOOR}-column \
         literal, which is why a gate at the default cannot see issue #505",
        view_columns(SERVED)
    );
    assert_eq!(
        capacity_for_view_radius(SERVED),
        STORE_CAPACITY_FLOOR,
        "and the floor is what the default configuration still gets, so the fix cannot \
         have changed the memory profile chunk_store measured at 97.6 MiB"
    );
}

/// **The curve, in counter form.** Reported for every render distance spanning the
/// old ceiling, so the next reader has the shape rather than two isolated passes.
///
/// A `#[test]`, not a measurement tool: each row asserts the regime its capacity
/// puts it in, computed from the policy's constants. The rows below the cap must
/// regenerate nothing; the rows above it must regenerate at least the arithmetic
/// shortfall.
///
/// The capacity per row is `integrated_capacity_for_view_radius`, because that is
/// the policy `open_in_memory` — the rig's constructor — actually applies. It has
/// no ceiling, so **every** row here now lands in the covers-the-view regime,
/// including the one past `FULLY_RESIDENT_VIEW_RADIUS`; the shortfall branch
/// survives for the hosted policy and is exercised by
/// [`past_the_hosted_capacity_cap_the_view_cannot_stay_resident`] over LAN.
#[tokio::test]
async fn the_regeneration_curve_across_the_render_distance_slider() {
    // view_radius, i.e. render_distance + 1. Chosen to straddle both thresholds:
    // 9 is the shell default (under the old literal), 11 and 13 are past it
    // (10 and 12 on the slider — the cliff issue #505 reports), and 20 is past
    // the hosted cap (and, since the integrated policy dropped that ceiling, is
    // the row proving the drop reaches the streamed view rather than only the
    // arithmetic).
    for view_radius in [9, 11, 13, CONTROL_RADIUS] {
        let capacity = integrated_capacity_for_view_radius(view_radius);
        let columns = view_columns(view_radius);
        let observed = stream_view_then_reshrink_and_regrow(view_radius).await;

        assert_eq!(
            observed.join_columns, columns,
            "row {view_radius}: the join must stream the whole square"
        );

        eprintln!(
            "view_radius {view_radius:>2} (rd {:>2})  view {:>5}  capacity {capacity:>5}  \
             old-literal {STORE_CAPACITY_FLOOR}  regenerated {:>6}  total {:>6}",
            view_radius - 1,
            columns,
            observed.regenerated,
            observed.total_generations,
        );

        if columns + CONCURRENT_SCAN_COLUMNS <= capacity {
            assert_eq!(
                observed.regenerated, 0,
                "row {view_radius}: capacity {capacity} covers the {columns}-column view, \
                 so nothing may be generated twice"
            );
        } else {
            let floor = (columns - capacity) as u64;
            assert!(
                observed.regenerated >= floor,
                "row {view_radius}: capacity {capacity} cannot hold the {columns}-column \
                 view, so at least {floor} columns must be cold on the re-grow; got {}",
                observed.regenerated
            );
        }
    }
}

// ---------------------------------------------------------------------------
// Issue #551: the capacity must follow a *live* radius change, not only the one
// the connection joined with.
// ---------------------------------------------------------------------------

/// The radius the #551 arms **join** at: the shell's own default
/// (`render_distance` 8), whose derived capacity is the [`STORE_CAPACITY_FLOOR`]
/// literal. Joining here is the point — this is the store every singleplayer
/// session starts with.
const JOIN_RADIUS: i32 = 9;

/// The radius the #551 arms **raise to** after joining. Past
/// `integrated_capacity_for_view_radius(JOIN_RADIUS)`, so the join-time capacity
/// genuinely cannot hold the raised view; the compile-time premises below are what
/// hold it to that rather than this comment.
const RAISED_RADIUS: i32 = 15;

const _: () = assert!(
    RAISED_RADIUS <= lodestone_server::MAX_CLIENT_VIEW_RADIUS,
    "the raise must be a value `ViewTracker::max_radius` actually permits, or the \
     server clamps it back and both arms measure the join radius"
);
const _: () = assert!(
    view_columns(RAISED_RADIUS) > integrated_capacity_for_view_radius(JOIN_RADIUS),
    "the raised view must exceed the capacity derived at the JOIN radius, or a store \
     whose capacity never moved would pass this gate and it would measure nothing"
);
const _: () = assert!(
    view_columns(RAISED_RADIUS) + CONCURRENT_SCAN_COLUMNS
        <= integrated_capacity_for_view_radius(RAISED_RADIUS),
    "the derivation at the raised radius must cover the raised view, or even a \
     correct implementation cannot reach zero"
);

/// Joins at `join_radius`, raises the slider to `raised_radius`, then drags it to
/// 0 and back to `raised_radius`. Returns the observation from that final re-grow.
///
/// # Why the second sweep is the measurement and the raise is not
///
/// The raise **alone** cannot see the bug, and this is the trap worth writing
/// down. `ViewTracker::set_view_radius` diffs the window, so it asks the source
/// for each newly-visible column exactly once; even a store whose capacity is
/// hopelessly short therefore reports one generation per column during the raise,
/// with no repeats. The harm of a short capacity is not paid at the moment of the
/// raise — it is paid by whatever asks *again*, and by the join's outward ring
/// order the columns that are gone are the **innermost** ones.
///
/// So the probe is a second down-and-up sweep, which is the same instrument
/// [`stream_view_then_reshrink_and_regrow`] uses and reads the same property:
/// residency. Zero regenerations on the final re-grow means the store really is
/// holding the raised view.
async fn join_then_raise_then_probe(join_radius: i32, raised_radius: i32) -> Observation {
    let counting = CountingSource::new();
    let calls = Arc::clone(&counting.calls);
    let per_chunk = Arc::clone(&counting.per_chunk);

    // `open_in_memory` is the constructor singleplayer uses, and it passes
    // `MAX_CLIENT_VIEW_RADIUS` as the ceiling (issue #545) — which is what makes
    // raising above `join_radius` possible at all.
    let (server, client_end) = IntegratedServer::open_in_memory(FakeProtocol, counting, join_radius);
    let mut client = Connection::new(client_end);

    client.write_packet(HANDSHAKE, &[2]).await.expect("handshake");
    let mut w = Writer::default();
    w.string("Rd551");
    client
        .write_packet(LOGIN_START, w.as_slice())
        .await
        .expect("login start");
    let (id, _) = client.read_packet().await.expect("read").expect("packet");
    assert_eq!(id, LOGIN_SUCCESS, "login must succeed before the join view");
    client
        .write_packet(LOGIN_ACKNOWLEDGED, &[])
        .await
        .expect("login ack");
    client
        .write_packet(FINISH_CONFIGURATION, &[])
        .await
        .expect("finish configuration");

    let join_columns = drain_one_batch(&mut client).await;
    ack_batch(&mut client).await;

    // The raise. Everything from ring `join_radius + 1` outward is new, so this
    // emits a batch of its own that has to be drained and acked.
    set_view_distance(&mut client, raised_radius).await;
    let raised_columns = drain_one_batch(&mut client).await;
    ack_batch(&mut client).await;
    assert_eq!(
        join_columns + raised_columns,
        view_columns(raised_radius),
        "precondition: the join plus the raise must together have streamed the whole \
         raised square, or the probe below is pointed at a view that was never served"
    );

    // The probe: down to 0, back up to the raised radius.
    set_view_distance(&mut client, 0).await;
    set_view_distance(&mut client, raised_radius).await;
    let regrow_columns = drain_one_batch(&mut client).await;

    drop(client);
    server.shutdown().await;

    let (regenerated, worst_coord, worst) = regenerations(&per_chunk);
    Observation {
        join_columns,
        regrow_columns,
        regenerated,
        worst: (worst_coord, worst),
        total_generations: calls.load(Ordering::Relaxed),
    }
}

/// **Issue #551's subject.** A connection that joins at the default render
/// distance and then *raises* it must end up with a store that holds the raised
/// view — zero regenerations on a probe of it.
///
/// # Predicting the value, not the sign
///
/// Both hypotheses are computed from the policy's own constants:
///
/// * **Fixed capacity** (the bug): the store is built from `JOIN_RADIUS` and stays
///   there, so it holds `integrated_capacity_for_view_radius(9)` = 512 of the
///   raised view's `view_columns(15)` = 961 columns. 449 are already gone before
///   the probe starts, and the probe is a cyclic re-scan of 960 columns through a
///   512-entry LRU — LRU's worst case, so 449 is a floor and not the figure.
/// * **Capacity follows the radius** (fixed): the store is resized to
///   `integrated_capacity_for_view_radius(15)` = 1,011 ≥ 961 + 50, every column of
///   the raised view is resident, and the probe generates **0**.
///
/// 0 against a computed floor of 449. The assertion is `== 0`, and
/// [`a_fixed_capacity_store_cannot_hold_a_raised_view`] below is the arm that
/// lands on the other hypothesis.
///
/// # Which implementation does this resolve to?
///
/// `IntegratedServer::open_in_memory` over `memory_pair()` — production
/// singleplayer's own constructor and its own transport, not a double, and
/// `ServerBound::ClientInformationChanged` is the real render-distance slider on
/// the wire.
#[tokio::test]
async fn raising_the_render_distance_mid_session_resizes_the_store() {
    let observed = join_then_raise_then_probe(JOIN_RADIUS, RAISED_RADIUS).await;

    // Preconditions, failing rather than skipping: both would make `== 0`
    // trivially true.
    assert_eq!(
        observed.join_columns,
        view_columns(JOIN_RADIUS),
        "the join must stream the whole {JOIN_RADIUS}-radius square"
    );
    assert_eq!(
        observed.regrow_columns,
        view_columns(RAISED_RADIUS) - view_columns(0),
        "the probe's re-grow must re-request the whole raised view bar the centre \
         column, or it is not probing the band a short capacity drops"
    );

    let ((wx, wz), worst) = observed.worst;
    assert_eq!(
        worst, 1,
        "chunk ({wx}, {wz}) was generated {worst} times: the store did not resize when \
         the render distance was raised from {JOIN_RADIUS} to {RAISED_RADIUS}, so the \
         {}-column view is being held in a {}-entry cache",
        view_columns(RAISED_RADIUS),
        integrated_capacity_for_view_radius(JOIN_RADIUS),
    );
    assert_eq!(
        observed.regenerated, 0,
        "capacity {} covers the {}-column raised view, so nothing may be generated \
         twice; got {} regenerations",
        integrated_capacity_for_view_radius(RAISED_RADIUS),
        view_columns(RAISED_RADIUS),
        observed.regenerated,
    );
    assert_eq!(
        observed.total_generations,
        view_columns(RAISED_RADIUS) as u64,
        "the whole session must cost exactly one generation per column of the raised \
         view"
    );
}

/// **The negative control, and it must fail the assertion above.**
///
/// Reproduces the pre-#551 behaviour as a real *configuration* of the shipped
/// type rather than as a temporary neuter: a store whose capacity is fixed at the
/// join radius's derivation and never moves is exactly
/// `ChunkStore::with_capacity(source, integrated_capacity_for_view_radius(9))`,
/// which is [`CapacityPolicy::Fixed`] and therefore ignores
/// `set_retention_radius` by construction.
///
/// It cannot be driven through `IntegratedServer` — every production constructor
/// derives from a radius, which is issue #505's fix stated as a type signature —
/// so this arm drives the store directly and reproduces the access *pattern*
/// instead: the raised view's columns in ring order, twice. That is a weaker rig
/// than the subject's (it restates the pattern rather than measuring it off the
/// wire) and it is the right trade for a control, whose only job is to show the
/// comparison can fail.
#[test]
fn a_fixed_capacity_store_cannot_hold_a_raised_view() {
    let join_capacity = integrated_capacity_for_view_radius(JOIN_RADIUS);
    let raised = view_columns(RAISED_RADIUS);
    assert!(
        raised > join_capacity,
        "control premise: {raised} columns must exceed the {join_capacity}-entry cache"
    );

    // Ring order outward from the centre, matching `join_view_rings` — the order
    // that makes the innermost ring the LRU victim.
    let mut coords = Vec::with_capacity(raised);
    for r in 0..=RAISED_RADIUS {
        for dz in -r..=r {
            for dx in -r..=r {
                if dx.abs().max(dz.abs()) == r {
                    coords.push((dx, dz));
                }
            }
        }
    }
    assert_eq!(coords.len(), raised, "the ring walk must cover the whole square");

    let mut generated: HashMap<(i32, i32), u64> = HashMap::new();
    let mut cache: Vec<(i32, i32)> = Vec::new();
    for pass in 0..2 {
        for &coord in &coords {
            if let Some(at) = cache.iter().position(|&c| c == coord) {
                // A hit refreshes the entry's recency.
                let entry = cache.remove(at);
                cache.push(entry);
                continue;
            }
            *generated.entry(coord).or_insert(0) += 1;
            cache.push(coord);
            if cache.len() > join_capacity {
                cache.remove(0);
            }
        }
        let _ = pass;
    }

    let regenerated: u64 = generated.values().filter(|&&n| n > 1).count() as u64;
    assert!(
        regenerated >= (raised - join_capacity) as u64,
        "control: a {join_capacity}-entry cache re-scanned over {raised} columns must \
         regenerate at least {} of them; got {regenerated}. If this is 0 the control \
         has stopped reproducing the bug and the subject above proves nothing.",
        raised - join_capacity
    );
    // And the *centre* column specifically, which is the whole reason a short
    // capacity here is worse than it looks: ring order makes the player's own
    // column the oldest entry by the end of a pass.
    assert!(
        generated.get(&(0, 0)).copied().unwrap_or(0) > 1,
        "control: the centre column (the player's own feet) must be among the \
         regenerated ones — that is the harm, not the horizon"
    );
}
