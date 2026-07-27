//! Long-lived live session gate: the highest-value live test in the crate.
//!
//! Every other live gate in this crate connects, does one thing, asserts, and
//! disconnects. They are all *shorter than a real server's time-to-failure*, so
//! they are structurally incapable of observing any property governed by a
//! counter that accumulates across a session. This gate is the opposite: it
//! joins the real 26.2 server and stays alive for several wall-clock minutes,
//! moving continuously the whole time, and asserts the session **survives and
//! stays healthy**.
//!
//! Three confirmed silent-kill counters motivate it (all read out of vanilla's
//! source, none hypothetical):
//!   1. Chunk delivery halts after 10 unacknowledged batches
//!      (`PlayerChunkSender.MAX_UNACKNOWLEDGED_BATCHES`) unless the client sends
//!      `chunk_batch_received`. The v770 adapter now acks batches, so healthy
//!      streaming continues past that cliff — this gate proves it does, and the
//!      `suppressing_chunk_ack_starves_streaming` negative control proves the
//!      assertion is falsifiable by turning the ack back off. (Finding worth
//!      recording: the *spawn bubble* alone streams several hundred columns —
//!      ~20+ acked batches — so it already exercises the cliff; long-distance
//!      travel is not required to pass batch 10, contrary to the naive intuition.)
//!   2. Kicked at 4096 pending signed chats. **Unreachable in a test** (you
//!      cannot make a server emit 4096 signed chats to you here); this gate does
//!      NOT claim to cover it. Stated so no one mistakes green here for coverage.
//!   3. Kicked for `flying` after ~80 airborne ticks with the server believing
//!      us unsupported. That is a physics-parity hazard that lives in the
//!      `on_ground` flag we transmit, separate from the simulation. The
//!      `airborne_with_on_ground_true_is_kicked_for_flying` negative control
//!      forces exactly that mistake and proves the server converts it to a kick,
//!      which is what the positive gate's survival property rules out.
//!
//! ## What this gate does and does not prove
//!
//! It asserts *properties*, not packets: we are still connected, the spawn bubble
//! streamed past the 10-batch cliff (measured as cumulative distinct chunk columns
//! seen, which is eviction-proof), the server never rubber-bands us (corrective
//! `TeleportPlayer` count stays bounded — a server-authored signal, not our own
//! read-model echo), keep-alives are answered, we stayed active for the whole run,
//! and no `DISCONNECT` arrives. On any disconnect it classifies *which* of
//! vanilla's twelve reasons fired, because a bare "connection closed" after a
//! multi-minute run tells us nothing.
//!
//! It does NOT prove long-distance travel. On this *shared* server the spawn area
//! carries other agents' builds; a headless client that walks a long straight line
//! eventually rams an obstruction and the server clamps it with an unbounded
//! rubber-band storm unrelated to session survival. So movement is confined to a
//! runway proven walkable with `block_at` and the client oscillates within it —
//! this proves *sustained activity and a working per-tick movement send-path*, not
//! net distance.
//!
//! It also does NOT prove server-confirmed displacement. `handle.position()` is
//! the driver's optimistic local prediction; the only server truth this test reads
//! is the *absence* of corrective teleports and the *absence* of a disconnect.
//! Full server-acknowledged displacement is impl-physics's second-observer gate
//! (`live_second_observer.rs`), deliberately not duplicated here. (RCON is not an
//! option for this oracle: on this host `127.0.0.1:25575` is the entity-oracle
//! container serving a *different* world on `:25567`, not the `:25565` server
//! under test — using it would be a silent wrong-server oracle.)
//!
//! Gated behind the `live-v770` feature AND `#[ignore]`. Per the live-test
//! convention a missing precondition is a FAILURE, not a silent skip: if the
//! server is unreachable the test panics loudly with the address to start, and
//! if built in debug it panics telling you to use `--release` (a debug build's
//! chunk decode starves the driver task and the server rubber-band-storms a
//! moving client — see `require_release_build`). Run with:
//!
//! ```text
//! cargo test --release -p lodestone-client --features live-v770 --test live_session -- --ignored --nocapture
//! ```
#![cfg(feature = "live-v770")]

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use lodestone_client::{
    BlockPos, ChunkPos, ClientAction, ClientBuilder, ClientEvent, ClientHandle, ConnectionState,
    Directive, LoginProfile, Rotation, ServerAddress, SessionOutcome, Vec3, VersionAdapter,
};
use lodestone_model::AdapterError;
use lodestone_world::WorldSink;
use uuid::Uuid;

mod common;
use common::unique_username;

const SERVER_HOST: &str = "127.0.0.1";
const SERVER_PORT: u16 = 25565;
/// v770 family protocol; obtained via the registry so the client never names a
/// concrete version crate.
const PROTOCOL: i32 = 776;

/// One simulated client tick.
const TICK: Duration = Duration::from_millis(50);
/// Horizontal blocks advanced per tick. 0.1 * 20 Hz = 2 b/s — a deliberately
/// unhurried walk that stays comfortably inside the server's movement validator.
const STEP: f64 = 0.1;
/// Version-free state id of `minecraft:air` on the live 26.2 superflat server
/// (confirmed empirically by the sibling physics gate: `block_at` returns
/// `Some(0)` for air above the surface and a non-zero solid id below the feet).
const AIR_STATE_ID: u32 = 0;
/// After the join teleport the server ignores our movement until its
/// `clientLoadedTimeoutTimer` (~60 ticks) expires; we cannot send `player_loaded`
/// through the public API, so we wait this long before moving. Moving inside the
/// window is silently rejected and rubber-banded (mirrors the physics gate).
const LOAD_SETTLE: Duration = Duration::from_secs(5);
/// How far (blocks) each way we probe for a clean, walkable runway from spawn.
const RUNWAY_PROBE: i32 = 24;
/// Minimum total clean runway required to accept a spawn; below this we retry.
const MIN_RUNWAY: f64 = 6.0;
/// Spawn attempts before giving up (inherited corpse or fully obstructed lane).
const MAX_JOIN_ATTEMPTS: usize = 8;
/// How often the walk loop records a sample.
const SAMPLE_EVERY: Duration = Duration::from_secs(2);

/// Is the block column at `(x, y, z)` walkable — a loaded solid underfoot and
/// loaded air at the feet and head? A pure public-API check via `block_at`, so
/// the gate needs no RCON. `None` (chunk not loaded) counts as not clean.
fn column_is_walkable(handle: &ClientHandle, x: i32, y: i32, z: i32) -> bool {
    let below = handle.block_at(BlockPos::new(x, y - 1, z));
    let feet = handle.block_at(BlockPos::new(x, y, z));
    let head = handle.block_at(BlockPos::new(x, y + 1, z));
    matches!(
        (below, feet, head),
        (Some(b), Some(AIR_STATE_ID), Some(AIR_STATE_ID)) if b != AIR_STATE_ID
    )
}

/// Probes a clean walkable runway along the X axis through `start`, returning
/// the furthest clean `x` in the `-X` and `+X` directions (as absolute block
/// coordinates). Movement is confined to this interval so we never ram an
/// obstruction — which the server clamps with an unbounded rubber-band storm.
fn clean_runway(handle: &ClientHandle, start: Vec3) -> (i32, i32) {
    let fy = start.y.floor() as i32;
    let fz = start.z.floor() as i32;
    let fx = start.x.floor() as i32;
    let mut lo = fx;
    let mut hi = fx;
    for d in 1..=RUNWAY_PROBE {
        if column_is_walkable(handle, fx + d, fy, fz) && hi == fx + d - 1 {
            hi = fx + d;
        }
        if column_is_walkable(handle, fx - d, fy, fz) && lo == fx - d + 1 {
            lo = fx - d;
        }
    }
    (lo, hi)
}

// ---------------------------------------------------------------------------
// Version-free adapter decorator
// ---------------------------------------------------------------------------

/// What the [`FilterAdapter`] does to the inner adapter's directives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    /// Transparent passthrough.
    PassThrough,
    /// Drop the chunk-batch-received ack so the server starves us after 10
    /// unacknowledged batches (the negative control for streaming health).
    SuppressBatchAck,
}

/// A test-only [`VersionAdapter`] wrapper that can suppress the chunk-batch ack
/// **without any version-specific knowledge**.
///
/// The discriminator is purely structural on the model's `Directive` enum. In
/// `Play`, the v770 adapter emits exactly two shapes of `Directive::Send` from
/// `handle_packet`: a teleport-accept, which is *always* bundled with an
/// `Emit(TeleportPlayer)`, and the chunk-batch ack, which travels *alone* (no
/// `Emit`). So "in `Play`, a `handle_packet` batch whose only content is
/// `Send`(s) with no accompanying `Emit`" uniquely identifies flow-control acks.
/// This reasons about `Directive` variants only — never a packet id — so it
/// stays inside the version-free contract and cannot drift when packet ids
/// change. It is gated to `Play` so Configuration-phase keep-alive sends are
/// untouched.
#[derive(Debug)]
struct FilterAdapter {
    inner: Box<dyn VersionAdapter>,
    filter: Filter,
}

impl FilterAdapter {
    fn wrap(inner: Box<dyn VersionAdapter>, filter: Filter) -> Box<dyn VersionAdapter> {
        Box::new(Self { inner, filter })
    }
}

impl VersionAdapter for FilterAdapter {
    fn protocol_version(&self) -> i32 {
        self.inner.protocol_version()
    }

    fn minecraft_versions(&self) -> &'static [&'static str] {
        self.inner.minecraft_versions()
    }

    fn supports(&self, protocol: i32) -> bool {
        self.inner.supports(protocol)
    }

    fn begin_login(
        &self,
        profile: &LoginProfile,
        server: &ServerAddress,
    ) -> Result<Vec<Directive>, AdapterError> {
        self.inner.begin_login(profile, server)
    }

    fn handle_packet(
        &self,
        world: &mut dyn WorldSink,
        state: ConnectionState,
        packet_id: i32,
        payload: &[u8],
    ) -> Result<Vec<Directive>, AdapterError> {
        let out = self.inner.handle_packet(world, state, packet_id, payload)?;
        if self.filter == Filter::SuppressBatchAck && state == ConnectionState::Play {
            let has_emit = out.iter().any(|d| matches!(d, Directive::Emit(_)));
            if !has_emit {
                // No Emit in this Play batch => any Send is flow-control (the
                // chunk-batch ack). Strip only the Send(s); keep SetState etc.
                return Ok(out
                    .into_iter()
                    .filter(|d| !matches!(d, Directive::Send { .. }))
                    .collect());
            }
        }
        Ok(out)
    }

    fn encode_action(
        &self,
        state: ConnectionState,
        action: &ClientAction,
    ) -> Result<Option<(i32, Vec<u8>)>, AdapterError> {
        self.inner.encode_action(state, action)
    }

    fn build_encryption_response(
        &self,
        encrypted_secret: &[u8],
        encrypted_token: &[u8],
    ) -> Result<Directive, AdapterError> {
        self.inner
            .build_encryption_response(encrypted_secret, encrypted_token)
    }
}

// ---------------------------------------------------------------------------
// Kick-reason classification
// ---------------------------------------------------------------------------

/// The complete set of vanilla disconnect reasons relevant to long-session
/// survival, matched against the plain rendering of the disconnect `Text`
/// (which falls back to the raw translate key, e.g.
/// `multiplayer.disconnect.flying`). More specific tokens are tested first so
/// `invalid_player_movement` is never shadowed by a looser match. Returns the
/// matched category, or `"unrecognised"` with the raw text preserved by the
/// caller.
fn classify_disconnect(reason_plain: &str) -> &'static str {
    const KNOWN: &[&str] = &[
        "too_many_pending_chats",
        "chat_validation_failed",
        "invalid_player_movement",
        "invalid_vehicle_movement",
        "invalid_entity_attacked",
        "unexpected_query_response",
        "illegal_characters",
        "flying",
        "idling",
        "timeout",
        "spam",
        "generic",
    ];
    for key in KNOWN {
        if reason_plain.contains(key) {
            return key;
        }
    }
    "unrecognised"
}

// ---------------------------------------------------------------------------
// Shared session observation
// ---------------------------------------------------------------------------

/// Server-authored signals folded off the event stream by the drain task.
#[derive(Debug, Default)]
struct Observed {
    /// Set once the server sends an explicit `Disconnect`.
    disconnected: AtomicBool,
    /// The plain-text disconnect reason, if any.
    reason: Mutex<Option<String>>,
    /// Count of `TeleportPlayer` events. Placement teleports arrive before the
    /// walk; anything after the walk starts is a *corrective* teleport — the
    /// server rubber-banding us because it disagrees with our position.
    teleports: AtomicUsize,
    /// Count of keep-alives the server asked us for (our driver answers them).
    keepalives: AtomicUsize,
}

/// Spawns a task that drains the event stream (so the bounded channel never
/// backpressures the driver) and records server-authored signals.
fn spawn_drain(
    mut events: lodestone_client::EventStream,
) -> (Arc<Observed>, tokio::task::JoinHandle<()>) {
    let observed = Arc::new(Observed::default());
    let obs = Arc::clone(&observed);
    let task = tokio::spawn(async move {
        while let Some(event) = events.recv().await {
            match event {
                ClientEvent::TeleportPlayer { .. } => {
                    obs.teleports.fetch_add(1, Ordering::Relaxed);
                }
                ClientEvent::KeepAlive { .. } => {
                    obs.keepalives.fetch_add(1, Ordering::Relaxed);
                }
                ClientEvent::Disconnect { reason } => {
                    *obs.reason.lock().unwrap() = Some(reason.to_plain_string());
                    obs.disconnected.store(true, Ordering::Relaxed);
                    // Keep draining until the channel closes; do not break.
                }
                _ => {}
            }
        }
    });
    (observed, task)
}

/// One sample taken during a walk.
#[derive(Debug, Clone, Copy)]
struct Sample {
    elapsed: f64,
    /// Cumulative distinct chunk columns seen up to this point.
    distinct_chunks: usize,
    /// Absolute `TeleportPlayer` count at this instant.
    teleports: usize,
}

/// The result of oscillating within a clean runway while sampling session health.
struct WalkReport {
    samples: Vec<Sample>,
    /// Cumulative horizontal path length walked (sum of per-tick advances). This
    /// is the real "we kept moving for minutes" signal; net displacement is
    /// deliberately bounded because we stay inside a verified-clean runway.
    path_length: f64,
    /// True if the session ended (server disconnect or transport close) before
    /// the requested duration elapsed.
    ended_early: bool,
}

impl WalkReport {
    fn first(&self) -> Sample {
        *self.samples.first().expect("at least one sample")
    }
    fn last(&self) -> Sample {
        *self.samples.last().expect("at least one sample")
    }
    /// The sample nearest `frac` of the way through (0.0..=1.0).
    fn at_frac(&self, frac: f64) -> Sample {
        let idx = ((self.samples.len() as f64 - 1.0) * frac).round() as usize;
        self.samples[idx.min(self.samples.len() - 1)]
    }
}

/// Oscillates back and forth along the X axis *within a pre-verified clean
/// runway* `[lo, hi]` for `duration`, sending one `move_to` per tick and
/// sampling session health.
///
/// Why oscillate instead of walk in a straight line? On this shared 26.2 server
/// the spawn area contains other agents' builds. A headless client that walks a
/// long straight line eventually rams an obstruction, and the server clamps that
/// with an *unbounded* rubber-band storm (continuous corrective teleports, frozen
/// streaming) that never clears — which ate the distance budget and made a
/// straight-line gate flaky for reasons unrelated to session survival. Confining
/// movement to columns we proved walkable with `block_at` keeps every move valid,
/// so the run exercises sustained per-tick movement, keep-alives and the movement
/// send-path for the full multi-minute duration without a spurious storm. The
/// tradeoff, stated honestly: this proves *sustained activity*, not *long-distance
/// travel* — net displacement is bounded by the runway.
async fn oscillate_and_sample(
    handle: &ClientHandle,
    observed: &Observed,
    duration: Duration,
    start: Vec3,
    lo: i32,
    hi: i32,
) -> WalkReport {
    // Confine feet to the verified-clean interval, inset half a block from each
    // end so we never step onto the first obstructed (or unloaded) column.
    let min_x = lo as f64 + 0.5;
    let max_x = hi as f64 + 0.5;
    let mut dir: f64 = if (max_x - start.x) >= (start.x - min_x) {
        1.0
    } else {
        -1.0
    };

    let mut seen: HashSet<ChunkPos> = handle.loaded_chunks().into_iter().collect();
    let mut samples = Vec::new();
    let begin = Instant::now();
    let mut next_sample = Duration::ZERO;
    let mut ended_early = false;
    let mut last_teleports = observed.teleports.load(Ordering::Relaxed);
    let mut path_length = 0.0_f64;
    let mut prev_x = handle.position().map(|p| p.x).unwrap_or(start.x);

    loop {
        let elapsed = begin.elapsed();
        if elapsed >= duration {
            break;
        }
        if handle.is_finished() || observed.disconnected.load(Ordering::Relaxed) {
            ended_early = true;
            break;
        }

        // Considerate tick controller: read the driver's *current* knowledge
        // (which already folds any server correction). If the server just
        // corrected us, re-affirm its position instead of fighting it; otherwise
        // advance one step in the current direction, reversing at the runway
        // ends. `move_to` folds an optimistic local prediction — fine, because
        // the signals we assert on (corrective teleports, disconnect, chunk
        // batches) are all server-authored, not our own read-model echo.
        let teleports_now = observed.teleports.load(Ordering::Relaxed);
        let being_corrected = teleports_now != last_teleports;
        last_teleports = teleports_now;

        let base = handle.position().unwrap_or(start);
        if being_corrected {
            let _ = handle.move_to(base, Rotation::new(-90.0, 0.0), true);
            prev_x = base.x;
        } else {
            let mut nx = base.x + dir * STEP;
            if nx >= max_x {
                nx = max_x;
                dir = -1.0;
            } else if nx <= min_x {
                nx = min_x;
                dir = 1.0;
            }
            let yaw = if dir > 0.0 { -90.0 } else { 90.0 };
            let _ = handle.move_to(
                Vec3::new(nx, start.y, start.z),
                Rotation::new(yaw, 0.0),
                true,
            );
            path_length += (nx - prev_x).abs();
            prev_x = nx;
        }

        if elapsed >= next_sample {
            seen.extend(handle.loaded_chunks());
            samples.push(Sample {
                elapsed: elapsed.as_secs_f64(),
                distinct_chunks: seen.len(),
                teleports: teleports_now,
            });
            next_sample = elapsed + SAMPLE_EVERY;
        }

        tokio::time::sleep(TICK).await;
    }

    // Final sample.
    seen.extend(handle.loaded_chunks());
    samples.push(Sample {
        elapsed: begin.elapsed().as_secs_f64(),
        distinct_chunks: seen.len(),
        teleports: observed.teleports.load(Ordering::Relaxed),
    });

    WalkReport {
        samples,
        path_length,
        ended_early,
    }
}

/// Fails loudly if built without optimizations. A debug build's chunk decode is
/// slow enough to starve the single driver task under sustained streaming: while
/// the driver decodes a batch it cannot process our movement or send the batch
/// ack promptly, so the server rubber-bands a continuously-moving client
/// (observed: a permanent ~20 corrections/second storm in debug, versus a brief
/// spawn-chunk hold then clean movement in release). The gate is only meaningful
/// against a realistically-fast client, so require a release build rather than
/// let it fail for the wrong reason. Per the live-test convention this is a
/// failure with the fix, not a silent skip.
fn require_release_build() {
    if cfg!(debug_assertions) {
        panic!(
            "this live gate requires a release build — a debug build's chunk decode starves the \
             driver task and the server rubber-band-storms a moving client. Re-run with \
             `--release`, e.g. `cargo test --release -p lodestone-client --features live-v770 \
             --test live_session -- --ignored --nocapture`"
        );
    }
}

/// Everything a live test needs after a clean, settled join.
struct Session {
    handle: ClientHandle,
    observed: Arc<Observed>,
    drain: tokio::task::JoinHandle<()>,
    /// Server-placed spawn position (post-settle).
    start: Vec3,
    /// Verified-clean walkable runway `[lo, hi]` (block x) through `start`.
    runway: (i32, i32),
    /// Positive health read during the corpse guard.
    health: f32,
}

impl Session {
    async fn close(mut self) -> SessionOutcome {
        self.handle.shutdown();
        let outcome = self.handle.join().await;
        self.drain.abort();
        outcome
    }
}

/// Opens one live connection through the registry adapter (optionally wrapped)
/// and reaches Play (Login + spawn placement). Fails loudly with the exact
/// address to start if the server is unreachable — a missing precondition is a
/// failure, not a skip. Each call mints a fresh unique username so join retries
/// never collide.
async fn connect_once(
    filter: Filter,
) -> (ClientHandle, Arc<Observed>, tokio::task::JoinHandle<()>) {
    let server = ServerAddress {
        host: SERVER_HOST.into(),
        port: SERVER_PORT,
    };
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let mut adapter = lodestone_registry::adapter_for_protocol(PROTOCOL)
        .expect("v770 family compiled into the registry via the live-v770 feature");
    if filter != Filter::PassThrough {
        adapter = FilterAdapter::wrap(adapter, filter);
    }

    let (handle, events) = ClientBuilder::new(server, profile, adapter)
        .connect()
        .await
        .unwrap_or_else(|e| {
            panic!(
                "could not connect to the live 26.2 server on {SERVER_HOST}:{SERVER_PORT} \
                 (start it, offline mode, flat world): {e:?}"
            )
        });

    let (observed, drain) = spawn_drain(events);

    handle
        .wait_for_login(Duration::from_secs(30))
        .await
        .expect("should reach Play (Login event) on the live server");
    handle
        .wait_for_spawn(Duration::from_secs(30))
        .await
        .expect("server should place us (spawn position) on the live server");

    (handle, observed, drain)
}

/// Joins and returns a session that is ready to move: alive (not an inherited
/// corpse), past the server's `clientLoadedTimeoutTimer` (so movement is no
/// longer silently rejected), and standing in a verified-clean runway.
///
/// Retries the whole join (fresh connection + username) up to `MAX_JOIN_ATTEMPTS`
/// times if the spawn is a corpse or has no usable clean runway — this shared
/// 26.2 server accumulates other agents' builds near spawn, so a given spawn may
/// be obstructed. This mirrors the sibling physics gate's clean-lane retry.
async fn join_clean_settled(filter: Filter) -> Session {
    let mut last_reason = String::from("(no attempt ran)");
    for attempt in 1..=MAX_JOIN_ATTEMPTS {
        let (mut handle, observed, drain) = connect_once(filter).await;

        // Corpse guard: an inherited dead player blacks out chunks and would make
        // every downstream assertion meaningless. Health arrives just after
        // spawn, so poll for it rather than asserting immediately.
        let deadline = Instant::now() + Duration::from_secs(10);
        while handle.health().is_none() && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        let health = handle.health().unwrap_or(0.0);
        if health <= 0.0 {
            last_reason = format!("attempt {attempt}: inherited corpse (health {health})");
            handle.shutdown();
            let _ = handle.join().await;
            drain.abort();
            continue;
        }

        // Wait out the server's client-load window before moving; moving inside
        // it is silently rejected and rubber-banded.
        tokio::time::sleep(LOAD_SETTLE).await;

        let Some(start) = handle.position() else {
            last_reason = format!("attempt {attempt}: no position after settle");
            handle.shutdown();
            let _ = handle.join().await;
            drain.abort();
            continue;
        };

        let (lo, hi) = clean_runway(&handle, start);
        if (hi - lo) as f64 >= MIN_RUNWAY {
            return Session {
                handle,
                observed,
                drain,
                start,
                runway: (lo, hi),
                health,
            };
        }

        last_reason = format!(
            "attempt {attempt}: clean runway only {} blocks at spawn ({:.1},{:.1},{:.1}) — obstructed",
            hi - lo,
            start.x,
            start.y,
            start.z
        );
        handle.shutdown();
        let _ = handle.join().await;
        drain.abort();
    }
    panic!(
        "could not find a clean, alive spawn on {SERVER_HOST}:{SERVER_PORT} in \
         {MAX_JOIN_ATTEMPTS} attempts (shared server, spawn obstructed by other agents' builds). \
         Last: {last_reason}"
    );
}

/// Panics with the classified kick reason if the server disconnected us.
fn assert_not_kicked(observed: &Observed, context: &str) {
    if observed.disconnected.load(Ordering::Relaxed) {
        let raw = observed
            .reason
            .lock()
            .unwrap()
            .clone()
            .unwrap_or_else(|| "<no reason text>".to_owned());
        panic!(
            "{context}: server disconnected us — category `{}`, raw reason: {raw:?}",
            classify_disconnect(&raw)
        );
    }
}

// ---------------------------------------------------------------------------
// Positive gate
// ---------------------------------------------------------------------------

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "long-running (~3 min) live gate; requires a Minecraft 26.2 server on 127.0.0.1:25565 (offline mode, flat world)"]
async fn bot_survives_extended_session() {
    require_release_build();
    // Committed default is genuinely long (~3 min) so the session outlives the
    // server's accumulating counters. Overridable for local iteration only.
    let secs: u64 = std::env::var("LODESTONE_LIVE_SESSION_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(180);
    let duration = Duration::from_secs(secs);

    let session = join_clean_settled(Filter::PassThrough).await;
    let (lo, hi) = session.runway;
    let health = session.health;

    let report = oscillate_and_sample(
        &session.handle,
        &session.observed,
        duration,
        session.start,
        lo,
        hi,
    )
    .await;

    // If the session died mid-run, surface *which* of the twelve reasons.
    if report.ended_early {
        assert_not_kicked(&session.observed, "extended session ended early");
        panic!(
            "session ended early after ~{:.0}s with no server disconnect reason — transport close",
            report.last().elapsed
        );
    }
    assert_not_kicked(&session.observed, "extended session");

    // --- Property 1: still connected after the full run. ---
    assert!(
        !session.handle.is_finished(),
        "driver reports finished after the full {secs}s run despite no disconnect"
    );

    // --- Property 2: the spawn bubble streamed far past the 10-batch cliff. ---
    // Chunk columns are counted cumulatively (eviction-proof). Ten *unacknowledged*
    // batches can deliver at most ~160 columns before the server halts streaming
    // permanently (`MAX_UNACKNOWLEDGED_BATCHES = 10`, decremented only by our
    // `chunk_batch_received` ack). The join alone streams several hundred columns,
    // which is only possible if the client kept acking batches well past the tenth
    // — reaching 200+ columns needs ~13+ acked batches. Note (a finding worth
    // recording): the *spawn bubble* already exercises this cliff; long-distance
    // travel is not required to pass batch 10, contrary to the naive intuition.
    // The `suppressing_chunk_ack_starves_streaming` control proves this assertion
    // is falsifiable by turning the ack back off (it then plateaus near the cliff).
    let first = report.first();
    let last = report.last();
    assert!(
        first.distinct_chunks > 200,
        "only {} distinct chunk columns after the join settle — at/under the ~160 an *unacked* \
         10-batch stream can deliver, so batch acking may not be working past the cliff",
        first.distinct_chunks
    );
    assert!(
        last.distinct_chunks >= first.distinct_chunks,
        "cumulative distinct chunks went backwards ({} -> {}), which is impossible for a set — \
         a bookkeeping bug would make this property meaningless",
        first.distinct_chunks,
        last.distinct_chunks
    );

    // --- Property 3: once settled, the server does not rubber-band us. ---
    // A corrective `TeleportPlayer` while we are moving = the server disagreeing
    // with our transmitted position (server-authored, not our own read-model
    // echo). We move only through columns proven walkable via `block_at`, so a
    // valid client should draw ~zero corrections in the back half. A client whose
    // transmitted `on_ground`/position kept disagreeing would keep drawing them
    // here and be on course for an `invalid_player_movement` kick.
    let mid = report.at_frac(0.5);
    let steady_corrective = last.teleports.saturating_sub(mid.teleports);
    let early_corrective = mid.teleports.saturating_sub(first.teleports);
    assert!(
        steady_corrective <= 5,
        "server sent {steady_corrective} corrective teleports in the back half of the run \
         (after {early_corrective} earlier) — it keeps disagreeing with our position, which \
         accumulates toward an invalid-movement kick",
    );

    // --- Property 4: we stayed *active* for the whole run (not standing still,
    // which would risk `idling` and prove nothing about the movement send-path).
    // We assert cumulative path length, not net displacement: movement is confined
    // to a verified-clean runway, so we oscillate rather than travel far. ~2 b/s
    // minus considerate holds over `secs` seconds; the 0.5 b/s floor is generous
    // and scales so a shortened dev run and the committed default are both valid. ---
    let min_path = secs as f64 * 0.5;
    assert!(
        report.path_length > min_path,
        "only walked {:.1} blocks of cumulative path in {secs}s (expected > {min_path:.0}) — \
         not enough sustained movement to exercise the per-tick send-path or dodge `idling`",
        report.path_length
    );

    // --- Property 5: keep-alives kept flowing (the driver's auto-responder is
    // what stops `disconnect.timeout`). Over minutes we must see several. ---
    let keepalives = session.observed.keepalives.load(Ordering::Relaxed);
    assert!(
        keepalives >= 3,
        "only {keepalives} keep-alives in {secs}s — the server sends one roughly every 15s, so \
         too few means either a very short run or a stalled read loop"
    );

    eprintln!(
        "REPORT: survived {:.0}s. still_connected=true, spawn_bubble={} distinct chunk columns \
         (>200 => ~13+ acked batches, past the 10-batch cliff), final_distinct={}, \
         path_walked={:.1} blocks (oscillating in a {}-block clean runway), \
         corrective_teleports early={early_corrective} steady(back half)={steady_corrective}, \
         keepalives={keepalives}, health={health}. \
         Proves: still connected after minutes, chunk streaming survived the 10-batch cliff, \
         keep-alives answered, movement send-path drove sustained valid moves, server did not \
         rubber-band us once settled. Does NOT prove: long-distance travel (movement is confined \
         to a verified-clean runway to avoid this shared server's spawn obstructions), \
         server-confirmed *displacement* (position is local prediction; see the physics second-\
         observer gate), or the 4096-pending-chat kick (unreachable in a test).",
        report.last().elapsed,
        first.distinct_chunks,
        last.distinct_chunks,
        report.path_length,
        hi - lo,
    );

    // Clean local shutdown, and observe why the session ended.
    let outcome = session.close().await;
    assert!(
        matches!(outcome, SessionOutcome::LocalClose),
        "expected a clean LocalClose after shutdown(), got {outcome:?}"
    );
    assert!(outcome.is_clean(), "session outcome not clean: {outcome:?}");
}

// ---------------------------------------------------------------------------
// Negative control 1 (required): suppress the chunk-batch ack.
// ---------------------------------------------------------------------------

/// Proves the positive gate's "chunks keep arriving" assertion is falsifiable:
/// the *same* walk, with only the chunk-batch ack suppressed, must plateau. If
/// this test also grew chunks late, Property 2 above would be vacuous.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live negative control; requires a Minecraft 26.2 server on 127.0.0.1:25565 (offline mode, flat world)"]
async fn suppressing_chunk_ack_starves_streaming() {
    require_release_build();
    // Long enough to exhaust the 10-batch buffer and show a flat tail while
    // still moving (so the plateau isn't an artefact of standing still).
    let duration = Duration::from_secs(60);

    let session = join_clean_settled(Filter::SuppressBatchAck).await;
    let (lo, hi) = session.runway;

    let report = oscillate_and_sample(
        &session.handle,
        &session.observed,
        duration,
        session.start,
        lo,
        hi,
    )
    .await;

    // Starvation is not itself a kick (the server just stops sending), so we
    // should remain connected the whole time. If we were kicked, that's a
    // different failure and we want its reason.
    assert_not_kicked(&session.observed, "ack-suppressed session");
    assert!(
        !report.ended_early,
        "ack-suppressed session ended early unexpectedly"
    );

    // We must have genuinely kept moving, or a plateau proves nothing.
    assert!(
        report.path_length > 20.0,
        "only walked {:.1} blocks of path — a plateau would be meaningless if we stood still",
        report.path_length
    );
    // Some chunks must have loaded (the buffered batches) or the connection was
    // dead, not starved.
    let first = report.first();
    let last = report.last();
    assert!(
        first.distinct_chunks > 0,
        "no chunks loaded at all under ack suppression — connection dead, not starved"
    );

    // The core assertion: streaming plateaus at the cliff. With the ack
    // suppressed the server delivers at most the ~10-batch buffer (~160 columns)
    // and then halts, so the count must stay *well under* the 200+ the positive
    // gate requires, and must not grow over the second half despite continued
    // movement. If this test also reached 200+ or kept growing, the positive
    // gate's chunk assertion would be vacuous.
    assert!(
        last.distinct_chunks < 200,
        "with the batch ack suppressed, streaming still reached {} distinct columns (>= the 200 \
         the positive gate treats as proof of acking past the cliff) — the ack is not actually \
         gating streaming, so the positive gate's chunk assertion would be vacuous",
        last.distinct_chunks
    );
    let mid = report.at_frac(0.5);
    assert_eq!(
        last.distinct_chunks, mid.distinct_chunks,
        "with the batch ack suppressed, distinct chunks still grew {} -> {} over the second half \
         while walking {:.1} blocks of path — the ack is not actually gating streaming",
        mid.distinct_chunks, last.distinct_chunks, report.path_length
    );

    eprintln!(
        "REPORT (negative control): ack suppressed -> streaming plateaued at {} distinct columns \
         (mid {} @ {:.0}s == last {} @ {:.0}s, < the positive gate's 200 floor) despite walking \
         {:.1} blocks of path. Confirms the positive gate's chunk assertion is falsifiable: the \
         chunk-batch ack is load-bearing for streaming past the 10-batch cliff.",
        last.distinct_chunks,
        mid.distinct_chunks,
        mid.elapsed,
        last.distinct_chunks,
        last.elapsed,
        report.path_length,
    );

    let _ = session.close().await;
}

// ---------------------------------------------------------------------------
// Negative control 2 (physics parity): airborne with on_ground = true.
// ---------------------------------------------------------------------------

/// Forces the exact mistake the positive gate's survival rules out: sending
/// `on_ground = true` while genuinely airborne. Vanilla's `aboveGroundTickCount`
/// converts that into a `flying` kick after ~80 ticks. This proves the survival
/// property is a real, server-enforced constraint and not merely "the server
/// happened not to disconnect us".
///
/// This is the sharpest test of the physics-parity claim available through the
/// public API: the simulation is bit-exact against the JVM, but the `on_ground`
/// *flag we transmit* is a separate decision — if it is ever wrong in the
/// airborne direction, this counter fires ~4s later and nothing else we run
/// would notice.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live negative control (physics parity); requires a Minecraft 26.2 server on 127.0.0.1:25565 (offline mode, flat world)"]
async fn airborne_with_on_ground_true_is_kicked_for_flying() {
    require_release_build();
    let session = join_clean_settled(Filter::PassThrough).await;
    let start = session.start;
    let rotation = Rotation::new(0.0, 0.0);

    // Reach a clearly-airborne hover height in a few ticks (small per-tick deltas
    // so the move isn't rejected as teleport-like `invalid_player_movement`), then
    // hold that position *stationary* while transmitting `on_ground = false`. A
    // stationary airborne position the server computes as unsupported is exactly
    // the fly-hack signature that drives `aboveGroundTickCount` to the
    // `getMaximumFlyingTicks` limit (~80 ticks ≈ 4s) and a `flying` kick. (A
    // gradual *ascent* with on_ground=true instead gets continuously rubber-
    // banded, which resets the airborne state and never accumulates — verified
    // empirically.)
    let hover_y = start.y + 1.5;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut y = start.y;
    while Instant::now() < deadline {
        if session.observed.disconnected.load(Ordering::Relaxed) || session.handle.is_finished() {
            break;
        }
        if y < hover_y {
            y = (y + 0.3).min(hover_y);
        }
        let _ = session
            .handle
            .move_to(Vec3::new(start.x, y, start.z), rotation, false);
        tokio::time::sleep(TICK).await;
    }

    let disconnected = session.observed.disconnected.load(Ordering::Relaxed);
    let raw = session.observed.reason.lock().unwrap().clone();
    let category = raw.as_deref().map(classify_disconnect);

    eprintln!(
        "REPORT (negative control): airborne + on_ground=true -> disconnected={disconnected}, \
         category={category:?}, raw={raw:?}"
    );

    assert!(
        disconnected,
        "hovered airborne for up to 20s with on_ground=true but was never disconnected — the \
         server's flying detector did not fire, so this control cannot certify the positive gate's \
         survival property. (If the server config disables the flying check, this must be fixed, \
         not skipped.)"
    );
    assert_eq!(
        category,
        Some("flying"),
        "expected a `flying` kick from sending on_ground=true while airborne, got category \
         {category:?} (raw {raw:?})"
    );

    let _ = session.close().await;
}
