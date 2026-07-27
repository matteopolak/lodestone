//! Long-lived live session gate: the highest-value live test in the crate.
//!
//! Every other live gate in this crate connects, does one thing, asserts, and
//! disconnects. They are all *shorter than a real server's time-to-failure*, so
//! they are structurally incapable of observing any property governed by a
//! counter that accumulates across a session. This gate is the opposite: it
//! joins the real 26.2 server and stays alive for several wall-clock minutes,
//! walking a real distance, and asserts the session **survives and stays
//! healthy** the whole time.
//!
//! Three confirmed silent-kill counters motivate it (all read out of vanilla's
//! source, none hypothetical):
//!   1. Chunk delivery halts after 10 unacknowledged batches
//!      (`PlayerChunkSender.MAX_UNACKNOWLEDGED_BATCHES`) unless the client sends
//!      `chunk_batch_received`. The v770 adapter now acks batches, so healthy
//!      streaming continues past that cliff — this gate proves it does, and the
//!      `suppressing_chunk_ack_starves_streaming` negative control proves the
//!      assertion is falsifiable by turning the ack back off.
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
//! It asserts *properties*, not packets: we are still connected, chunks are
//! still arriving after the 10-batch cliff (measured as cumulative distinct
//! chunk columns seen, which is eviction-proof), the server never rubber-bands
//! us (corrective `TeleportPlayer` count stays bounded — a server-authored
//! signal, not our own read-model echo), and no `DISCONNECT` arrives. On any
//! disconnect it classifies *which* of vanilla's twelve reasons fired, because a
//! bare "connection closed" after a multi-minute run tells us nothing.
//!
//! It does NOT prove server-confirmed displacement. `handle.position()` is the
//! driver's optimistic local prediction; the only server truth this test reads
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
    ChunkPos, ClientAction, ClientBuilder, ClientEvent, ConnectionState, Directive, LoginProfile,
    Rotation, ServerAddress, SessionOutcome, Vec3, VersionAdapter,
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
/// unhurried walk. Faster than this (vanilla walk is ~4.3 b/s) can outrun the
/// server's chunk streaming under heavy initial decode load and trigger a
/// rubber-band storm; 2 b/s stays comfortably inside the streamed frontier while
/// still covering real distance over a multi-minute run.
fn step_per_tick() -> f64 {
    std::env::var("LODESTONE_LIVE_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(0.1)
}
/// How often the walk loop records a sample.
const SAMPLE_EVERY: Duration = Duration::from_secs(2);

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

/// The result of driving a straight walk while sampling session health.
struct WalkReport {
    samples: Vec<Sample>,
    /// Horizontal distance the local prediction advanced.
    distance: f64,
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

/// Drives a straight `+X` walk at constant ground `Y` for `duration`, sending
/// one `move_to` per tick (the correct per-tick controller primitive: fire-and-
/// forget, `on_ground = true` on the flat world) and sampling session health.
/// Stops early and reports it if the session ends.
async fn walk_and_sample(
    handle: &lodestone_client::ClientHandle,
    observed: &Observed,
    duration: Duration,
) -> WalkReport {
    let start = handle
        .position()
        .expect("server must place us with a TeleportPlayer before we walk");
    // Face +X (east) so the look direction matches travel; not required by the
    // server but keeps the movement ordinary.
    let rotation = Rotation::new(-90.0, 0.0);

    let mut seen: HashSet<ChunkPos> = handle.loaded_chunks().into_iter().collect();
    let step = step_per_tick();
    let mut samples = Vec::new();
    let begin = Instant::now();
    let mut next_sample = Duration::ZERO;
    let mut ended_early = false;
    let mut last_teleports = observed.teleports.load(Ordering::Relaxed);

    loop {
        let elapsed = begin.elapsed();
        if elapsed >= duration {
            break;
        }
        if handle.is_finished() || observed.disconnected.load(Ordering::Relaxed) {
            ended_early = true;
            break;
        }

        // A considerate tick controller: read the driver's *current* knowledge
        // (which already folds any server correction), and advance only when the
        // server is not actively correcting us. While the server is holding us —
        // during the initial spawn-chunk load, or if streaming stalls — a real
        // client cannot outrun the loaded frontier, so we re-affirm the
        // server's position instead of fighting it. Once corrections stop we
        // resume advancing. This keeps moves valid regardless of build profile
        // (a debug build's slower chunk decode otherwise starves the driver and
        // provokes a rubber-band storm). move_to folds an optimistic local
        // prediction, which is fine — the health signals we assert on are
        // server-authored.
        let teleports_now = observed.teleports.load(Ordering::Relaxed);
        let being_corrected = teleports_now != last_teleports;
        last_teleports = teleports_now;

        let base = handle.position().unwrap_or(start);
        let target = if being_corrected {
            base
        } else {
            Vec3::new(base.x + step, base.y, base.z)
        };
        let _ = handle.move_to(target, rotation, true);

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

    let distance = handle.position().map(|p| p.x - start.x).unwrap_or(0.0);
    WalkReport {
        samples,
        distance,
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

/// Builds a live connection through the registry adapter (optionally wrapped),
/// reaches Play, and returns everything the caller needs. Fails loudly with the
/// exact address to start if the server is unreachable — a missing precondition
/// is a failure, not a skip.
async fn connect_and_reach_play(
    filter: Filter,
) -> (
    lodestone_client::ClientHandle,
    Arc<Observed>,
    tokio::task::JoinHandle<()>,
) {
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

    let (mut handle, observed, drain) = connect_and_reach_play(Filter::PassThrough).await;

    // Corpse guard: an inherited dead player blacks out chunks and would make
    // every downstream assertion meaningless. Health arrives just after spawn,
    // so poll for it rather than asserting immediately.
    let deadline = Instant::now() + Duration::from_secs(10);
    while handle.health().is_none() && Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let health = handle.health();
    assert!(
        health.is_some_and(|h| h > 0.0),
        "health {health:?} not positive (0.0 => inherited corpse); session gate would be vacuous"
    );

    let report = walk_and_sample(&handle, &observed, duration).await;

    eprintln!(
        "DEBUG timeline: distance={:.1} ended_early={}",
        report.distance, report.ended_early
    );
    for s in &report.samples {
        eprintln!(
            "DEBUG  t={:>5.1}s distinct_chunks={:>4} teleports={}",
            s.elapsed, s.distinct_chunks, s.teleports
        );
    }

    // If the session died mid-walk, surface *which* of the twelve reasons.
    if report.ended_early {
        assert_not_kicked(&observed, "extended session ended early");
        panic!(
            "session ended early after ~{:.0}s with no server disconnect reason — transport close",
            report.last().elapsed
        );
    }
    assert_not_kicked(&observed, "extended session");

    // --- Property 1: still connected. ---
    assert!(
        !handle.is_finished(),
        "driver reports finished after the full {secs}s run despite no disconnect"
    );

    // --- Property 2: chunks arrive far past the 10-batch cliff. ---
    // Measured as cumulative distinct chunk columns seen (eviction-proof). Ten
    // *unacknowledged* batches can deliver at most ~160 columns before the
    // server halts streaming permanently; reaching several hundred is only
    // possible if the client keeps acking batches (≈16 columns/batch, so 200+
    // columns needs ~13+ acked batches). We also require growth to *continue*
    // after the initial ~10-batch window, so a stream that delivered a big
    // bubble and then stalled at the cliff still fails. The
    // `suppressing_chunk_ack_starves_streaming` control proves this is
    // falsifiable by turning the ack back off.
    let first = report.first();
    // The sample nearest 6s: by here an unacked stream would already be at its
    // ~10-batch ceiling, so any growth past this point required acking.
    let after_buffer = report
        .samples
        .iter()
        .find(|s| s.elapsed >= 6.0)
        .copied()
        .unwrap_or(first);
    let last = report.last();
    assert!(
        last.distinct_chunks > 200,
        "only {} distinct chunk columns after {secs}s — at/under the ~160 an *unacked* 10-batch \
         stream can deliver, so batch acking may not be working",
        last.distinct_chunks
    );
    assert!(
        last.distinct_chunks > after_buffer.distinct_chunks + 20,
        "chunk streaming did not continue past the 10-batch window: {} at {:.0}s -> {} at {:.0}s. \
         Growth stalling here is exactly the cliff the batch ack exists to prevent",
        after_buffer.distinct_chunks,
        after_buffer.elapsed,
        last.distinct_chunks,
        last.elapsed
    );

    // --- Property 3: once settled, the server never rubber-bands us. ---
    // A corrective `TeleportPlayer` after we start moving = the server
    // disagreeing with our position (server-authored, not our own read-model
    // echo). A brief settle burst at the start of movement is expected as the
    // client converges onto valid physics; the meaningful property is that
    // corrections then *stop*. We measure corrections in the back half of the
    // walk, which for a valid straight walk must be ~zero — a client whose
    // transmitted state kept disagreeing would keep drawing them here and be on
    // course for an `invalid_player_movement` kick.
    let settle = report.at_frac(0.5);
    let steady_corrective = last.teleports.saturating_sub(settle.teleports);
    let settle_burst = settle.teleports.saturating_sub(first.teleports);
    assert!(
        steady_corrective <= 2,
        "server sent {steady_corrective} corrective teleports in the back half of the walk \
         (after a {settle_burst}-teleport settle) — it keeps disagreeing with our position",
    );

    // --- Property 4: no disconnect (already asserted), and we covered real
    // distance so the survival wasn't achieved by standing still (which would
    // dodge chunk streaming and risk `idling`). Threshold scales with the run
    // length (net ~0.5 b/s after the initial spawn hold), so a shortened
    // dev run and the committed multi-minute default are both meaningful. ---
    let min_distance = secs as f64 * 0.5;
    assert!(
        report.distance > min_distance,
        "only advanced {:.1} blocks in {secs}s (expected > {min_distance:.0}) — not enough \
         sustained travel to stress chunk streaming",
        report.distance
    );

    eprintln!(
        "REPORT: survived {:.0}s, distance={:.1} blocks, distinct_chunks {}->{} \
         (>{} at {:.0}s, i.e. past the ~10-batch cliff), settle_burst={settle_burst} then \
         steady_corrective_teleports={steady_corrective}, keepalives={}, health={health:?}. \
         Proves: still connected, chunks arriving past the 10-batch cliff, server did not rubber-band \
         us once settled. Does NOT prove: server-confirmed displacement (position is local prediction; \
         see live_second_observer.rs) or the 4096-pending-chat kick (unreachable in a test).",
        report.last().elapsed,
        report.distance,
        first.distinct_chunks,
        last.distinct_chunks,
        after_buffer.distinct_chunks,
        after_buffer.elapsed,
        observed.keepalives.load(Ordering::Relaxed),
    );

    // Clean local shutdown, and observe why the session ended.
    handle.shutdown();
    let outcome = handle.join().await;
    assert!(
        matches!(outcome, SessionOutcome::LocalClose),
        "expected a clean LocalClose after shutdown(), got {outcome:?}"
    );
    assert!(outcome.is_clean(), "session outcome not clean: {outcome:?}");

    drain.abort();
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
    // still walking into fresh territory.
    let duration = Duration::from_secs(60);

    let (mut handle, observed, drain) = connect_and_reach_play(Filter::SuppressBatchAck).await;

    let report = walk_and_sample(&handle, &observed, duration).await;

    // Starvation is not itself a kick (the server just stops sending), so we
    // should remain connected the whole time. If we were kicked, that's a
    // different failure and we want its reason.
    assert_not_kicked(&observed, "ack-suppressed session");
    assert!(
        !report.ended_early,
        "ack-suppressed session ended early unexpectedly"
    );

    // We must have genuinely walked, or a plateau proves nothing.
    assert!(
        report.distance > 80.0,
        "only advanced {:.1} blocks — plateau would be meaningless",
        report.distance
    );
    // Some chunks must have loaded (the buffered batches) or the connection was
    // dead, not starved.
    let first = report.first();
    assert!(
        first.distinct_chunks > 0,
        "no chunks loaded at all under ack suppression — connection dead, not starved"
    );

    // The core assertion: streaming plateaus. Compare the midpoint (after the
    // buffer is spent) to the end — despite continuous forward travel, no new
    // chunk columns arrive.
    let mid = report.at_frac(0.5);
    let last = report.last();
    assert_eq!(
        last.distinct_chunks, mid.distinct_chunks,
        "with the batch ack suppressed, distinct chunks still grew {} -> {} over the second half \
         while walking {:.1} blocks — the ack is not actually gating streaming, so the positive \
         gate's chunk assertion would be vacuous",
        mid.distinct_chunks, last.distinct_chunks, report.distance
    );

    eprintln!(
        "REPORT (negative control): ack suppressed -> distinct chunks plateaued at {} \
         (mid {} @ {:.0}s == last {} @ {:.0}s) despite walking {:.1} blocks. \
         Confirms the positive gate's chunk-growth assertion is falsifiable.",
        last.distinct_chunks,
        mid.distinct_chunks,
        mid.elapsed,
        last.distinct_chunks,
        last.elapsed,
        report.distance,
    );

    handle.shutdown();
    let _ = handle.join().await;
    drain.abort();
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
    let (mut handle, observed, drain) = connect_and_reach_play(Filter::PassThrough).await;

    let start = handle
        .position()
        .expect("server must place us before we ascend");
    let rotation = Rotation::new(0.0, 0.0);

    // Gentle ascent so the server reads sustained *flying* rather than a
    // teleport-like jump that would trip invalid_player_movement instead. ~0.1
    // block/tick up to +3 blocks, then hover there (resend the airborne position
    // with on_ground = true) until kicked or the timeout.
    let hover_y = start.y + 3.0;
    let deadline = Instant::now() + Duration::from_secs(20);
    let mut y = start.y;
    while Instant::now() < deadline {
        if observed.disconnected.load(Ordering::Relaxed) || handle.is_finished() {
            break;
        }
        if y < hover_y {
            y = (y + 0.1).min(hover_y);
        }
        let _ = handle.move_to(Vec3::new(start.x, y, start.z), rotation, true);
        tokio::time::sleep(TICK).await;
    }

    let disconnected = observed.disconnected.load(Ordering::Relaxed);
    let raw = observed.reason.lock().unwrap().clone();
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

    handle.shutdown();
    let _ = handle.join().await;
    drain.abort();
}
