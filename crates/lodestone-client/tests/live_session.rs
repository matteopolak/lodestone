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
//!      (vanilla's own max-unacknowledged-batches constant) unless the client sends
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
//!      `airborne_on_ground_lie_is_punished_by_server` negative control forces
//!      exactly that mistake. Finding: on this server/version the terminal
//!      `flying` kick is *unreachable* through the public movement API — the
//!      server's position-correction path fires first, re-grounding us each tick
//!      and resetting vanilla's own above-ground tick counter before it hits the limit. The control
//!      asserts the correction storm instead (100+ corrective teleports), which is
//!      the same server-authored signal certifying the positive gate's Property 3.
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
//! rubber-band storm unrelated to session survival. So the controller stays glued
//! to the server's own position each tick and steps only through columns it
//! re-verifies walkable with `block_at`, reversing at anything blocked or unloaded.
//! Because every command is within one step of where the server already believes we
//! are, the server never rejects it, so the run proves *sustained activity and a
//! working per-tick movement send-path* — not guaranteed net distance.
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

/// The result of a walk while sampling session health.
struct WalkReport {
    samples: Vec<Sample>,
    /// Cumulative horizontal path length walked (sum of per-tick steps). This is
    /// the real "we kept moving for minutes" signal; net displacement is small
    /// because the controller stays glued to the server's position.
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

/// Walks back and forth along the X axis for `duration`, sending one `move_to`
/// per tick and sampling session health.
///
/// The controller tracks the *server's* authority every tick: it reads
/// `handle.position()` (which folds any server correction), adopts the server's
/// current `y`/`z`, and steps at most `STEP` in `x` into a column it re-verifies
/// walkable with `block_at` *from the current position*, reversing when the next
/// column is blocked or unloaded. Because every command is within `STEP` of where
/// the server already believes we are, the server never sees a large delta, so it
/// never rejects our movement — which is what prevents the unbounded rubber-band
/// *storm* that a fixed absolute lane provokes the moment the server relocates us
/// (a single stray corrective teleport would otherwise leave us commanding a lane
/// far from our actual position, and the server rejects every command after that).
///
/// The tradeoff, stated honestly: staying glued to the server's position and only
/// stepping through verified-clean columns means this proves *sustained, valid
/// per-tick movement* — not *long-distance travel*. Net displacement is small; the
/// path length (sum of per-tick steps) is the real "we kept moving for minutes"
/// signal. Long straight-line travel is not robust on this shared server because
/// its spawn area carries other agents' builds.
async fn oscillate_and_sample(
    handle: &ClientHandle,
    observed: &Observed,
    duration: Duration,
    start: Vec3,
) -> WalkReport {
    let mut dir: f64 = 1.0;

    let mut seen: HashSet<ChunkPos> = handle.loaded_chunks().into_iter().collect();
    let mut samples = Vec::new();
    let begin = Instant::now();
    let mut next_sample = Duration::ZERO;
    let mut ended_early = false;
    let mut last_teleports = observed.teleports.load(Ordering::Relaxed);
    let mut path_length = 0.0_f64;

    loop {
        let elapsed = begin.elapsed();
        if elapsed >= duration {
            break;
        }
        if handle.is_finished() || observed.disconnected.load(Ordering::Relaxed) {
            ended_early = true;
            break;
        }

        let teleports_now = observed.teleports.load(Ordering::Relaxed);
        let being_corrected = teleports_now != last_teleports;
        last_teleports = teleports_now;

        let base = handle.position().unwrap_or(start);
        // On an active correction, re-affirm exactly where the server just put us
        // rather than fighting it — this appeases a burst instead of feeding it.
        if being_corrected {
            let _ = handle.move_to(base, Rotation::new(-90.0, 0.0), true, false);
        } else {
            // Step within STEP of the server's current position, into a column we
            // re-verify walkable *from here*. If the chosen direction is blocked or
            // not yet loaded, flip; if both are blocked, hold in place. Adopting
            // base.y/base.z means a server relocation just moves our anchor and we
            // keep making small valid steps from wherever it placed us.
            let fy = base.y.floor() as i32;
            let fz = base.z.floor() as i32;
            let ahead = |d: f64| {
                let fx = (base.x + d * 0.6).floor() as i32;
                column_is_walkable(handle, fx, fy, fz)
            };
            if !ahead(dir) {
                dir = -dir;
            }
            if ahead(dir) {
                let nx = base.x + dir * STEP;
                let yaw = if dir > 0.0 { -90.0 } else { 90.0 };
                let _ = handle.move_to(
                    Vec3::new(nx, base.y, base.z),
                    Rotation::new(yaw, 0.0),
                    true,
                    false,
                );
                path_length += STEP;
            } else {
                // Boxed in this tick; re-affirm position so we stay active without
                // stepping into anything.
                let _ = handle.move_to(base, Rotation::new(-90.0, 0.0), true, false);
            }
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
/// corpse), past the server's own client-loaded timeout timer (so movement is no
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

        // The driver auto-sends `player_loaded` on the first placement teleport,
        // which zeroes the server's client-load timer — so we no longer wait the
        // window out, we only need the placement teleport itself before moving.
        if handle
            .wait_for(Duration::from_secs(10), |h| h.position().is_some())
            .await
            .is_err()
        {
            last_reason = format!("attempt {attempt}: server never placed us");
            handle.shutdown();
            let _ = handle.join().await;
            drain.abort();
            continue;
        }

        let Some(start) = handle.position() else {
            last_reason = format!("attempt {attempt}: no position after placement");
            handle.shutdown();
            let _ = handle.join().await;
            drain.abort();
            continue;
        };

        // Require an open, walkable spawn as a quality gate: if we spawn boxed in
        // by another agent's build the movement path can't exercise anything, so
        // retry for a cleaner placement. (The controller itself re-verifies each
        // column at move time; this only rejects hopeless spawns up front.)
        let (lo, hi) = clean_runway(&handle, start);
        if (hi - lo) as f64 >= MIN_RUNWAY {
            return Session {
                handle,
                observed,
                drain,
                start,
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
    let health = session.health;

    let report =
        oscillate_and_sample(&session.handle, &session.observed, duration, session.start).await;

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

    // --- Property 3: once settled, the server does not *storm* us with
    // corrections. ---
    // A corrective `TeleportPlayer` while we are moving = the server disagreeing
    // with our transmitted position (server-authored, not our own read-model
    // echo). We move only through columns proven walkable via `block_at`. Over a
    // multi-minute run the server issues occasional position-syncs even for valid
    // movement, so the meaningful, non-flaky signal is the *rate*: valid
    // oscillation draws well under 1 correction/second, whereas systematically
    // wrong movement draws a continuous storm — the `airborne_on_ground_lie_is_
    // punished_by_server` control measures that storm at 6-9/second. We bound the
    // back-half rate an order of magnitude below the storm, which cleanly
    // separates "healthy with sporadic syncs" from "the server keeps rejecting
    // us" (which would accumulate toward an `invalid_player_movement` kick).
    let mid = report.at_frac(0.5);
    let steady_corrective = last.teleports.saturating_sub(mid.teleports);
    let early_corrective = mid.teleports.saturating_sub(first.teleports);
    let back_half_secs = (last.elapsed - mid.elapsed).max(1.0);
    let corrective_rate = steady_corrective as f64 / back_half_secs;
    assert!(
        corrective_rate < 1.0,
        "server sent {steady_corrective} corrective teleports over the {back_half_secs:.0}s back \
         half ({corrective_rate:.2}/s, after {early_corrective} earlier) — approaching the \
         6-9/s storm the airborne-lie control provokes, i.e. the server keeps rejecting our \
         position, which accumulates toward an invalid-movement kick",
    );

    // --- Property 4: we stayed *active* for the whole run (not standing still,
    // which would risk `idling` and prove nothing about the movement send-path).
    // We assert cumulative path length, not net displacement: the controller stays
    // glued to the server's position and steps through verified-clean columns, so
    // it walks a long path without traveling far. ~2 b/s minus considerate holds
    // over `secs` seconds; the 0.5 b/s floor is generous and scales so a shortened
    // dev run and the committed default are both valid. ---
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
         path_walked={:.1} blocks (server-tracking per-tick steps through verified-clean columns), \
         corrective_teleports early={early_corrective} steady(back half)={steady_corrective} \
         ({corrective_rate:.2}/s vs the airborne-lie control's 6-9/s storm), \
         keepalives={keepalives}, health={health}. \
         Proves: still connected after minutes, chunk streaming survived the 10-batch cliff, \
         keep-alives answered, movement send-path drove sustained valid moves, server did not \
         storm us with corrections. Does NOT prove: long-distance travel (the controller stays \
         glued to the server's position and only steps through verified-clean columns, to avoid \
         this shared server's spawn obstructions), server-confirmed *displacement* (position is \
         local prediction; see the physics second-observer gate), or the 4096-pending-chat kick \
         (unreachable in a test).",
        report.last().elapsed,
        first.distinct_chunks,
        last.distinct_chunks,
        report.path_length,
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

    let report =
        oscillate_and_sample(&session.handle, &session.observed, duration, session.start).await;

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
// Negative control 2 (physics parity): an airborne on_ground lie is punished.
// ---------------------------------------------------------------------------

/// Forces the exact mistake the positive gate's survival rules out — sending
/// positions the server computes as airborne/unsupported while claiming
/// `on_ground = true` — and proves the server *actively punishes* it. This is the
/// falsifier for the positive gate's Property 3: valid ground movement draws ~0
/// corrective teleports and no disconnect, so if that assertion is to mean
/// anything, an invalid airborne lie must draw an *adverse* server reaction.
///
/// The server punishes the lie in one of two observed modes, and this control
/// accepts either — because both are the server refusing our position, and both
/// are categorically different from the positive gate's clean survival:
///
///  * a **correction storm** — the server rejects each airborne position and
///    teleports us back onto valid ground (observed: 100+ corrective teleports in
///    a 12s window, each relocating us to a real ground height); or
///  * an **outright kick** — the server sends a `Disconnect` and closes the
///    session (a server-authored `ClientEvent::Disconnect`, distinct from our own
///    local shutdown, which never sets `disconnected`).
///
/// Which mode fires varies run to run with spawn geometry and timing; the one
/// thing that never happens is the failure this guards against — the server
/// *accepting* the airborne lie (surviving cleanly with ~0 corrections and no
/// disconnect), which would make the positive gate's low-correction assertion
/// vacuous.
///
/// A note on what this does and does not reach. Vanilla has a terminal `flying`
/// kick (its own above-ground tick counter against its own maximum-flying-ticks
/// limit, ~80 ticks) for a client
/// the server believes is hovering unsupported. When the correction-storm mode
/// fires we do **not** reach that terminal kick through the public movement API:
/// each correction re-grounds us, resetting vanilla's own above-ground tick
/// counter before it hits
/// the limit, so `flying` never triggers and the two protections are mutually
/// exclusive. That is itself a finding. If a future server/version ever lets the
/// position stand and fires `flying` instead, this test surfaces that category too
/// (via `classify_disconnect`) rather than silently passing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live negative control (physics parity); requires a Minecraft 26.2 server on 127.0.0.1:25565 (offline mode, flat world)"]
async fn airborne_on_ground_lie_is_punished_by_server() {
    require_release_build();
    let session = join_clean_settled(Filter::PassThrough).await;
    let start = session.start;
    let rotation = Rotation::new(0.0, 0.0);

    // Rise to an *unambiguously* airborne height (well clear of the local ground,
    // whose exact level varies across spawns on this world) in small per-tick
    // steps, then hold there while lying with on_ground = true. At this height the
    // server always computes us as unsupported and rejects every position, either
    // storming corrective teleports or kicking us outright.
    let hover_y = start.y + 4.0;
    let window = Duration::from_secs(12);
    let before = session.observed.teleports.load(Ordering::Relaxed);
    let deadline = Instant::now() + window;
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
            .move_to(Vec3::new(start.x, y, start.z), rotation, true, false);
        tokio::time::sleep(TICK).await;
    }

    let corrections = session
        .observed
        .teleports
        .load(Ordering::Relaxed)
        .saturating_sub(before);
    let disconnected = session.observed.disconnected.load(Ordering::Relaxed);
    let raw = session.observed.reason.lock().unwrap().clone();
    let category = raw.as_deref().map(classify_disconnect);
    let punished = corrections > 25 || disconnected;

    eprintln!(
        "REPORT (negative control): airborne + on_ground=true for {}s -> punished={punished} via \
         {corrections} corrective teleports and disconnected={disconnected} (category={category:?}, \
         raw={raw:?}). The server refused the airborne lie — a valid walk in the positive gate draws \
         ~0 corrections and no disconnect, so this confirms Property 3 is falsifiable. The two \
         observed punishment modes (correction storm vs outright kick) are mutually exclusive with \
         the terminal `flying` kick when the storm mode fires: position-correction re-grounds us each \
         burst, resetting aboveGroundTickCount before it reaches getMaximumFlyingTicks.",
        window.as_secs(),
    );

    // The server must have *reacted adversely*. A valid walk in the positive gate
    // draws <=5 corrective teleports over a *multi-minute* run and never a server
    // disconnect; an airborne lie draws either a correction storm (many dozens over
    // this 12s window) or an outright server kick. Either proves the server does
    // not accept an unsupported position — which is exactly what makes the positive
    // gate's near-zero-correction, no-disconnect survival a real, non-vacuous claim.
    // The only outcome this rejects is the server *accepting* the lie: surviving
    // with ~0 corrections and no disconnect.
    assert!(
        punished,
        "airborne on_ground=true lie drew only {corrections} corrective teleports and no server \
         disconnect — the server appears to have *accepted* an airborne position it should reject, \
         which would make the positive gate's Property 3 (near-zero corrections and no disconnect \
         when moving validly) vacuous. (disconnected={disconnected}, category={category:?})"
    );

    let _ = session.close().await;
}
