//! Live **second-observer parity gate**: two independent clients on one server.
//! Client **A** walks a commanded distance through `lodestone-client`'s public
//! API; client **B** joins separately and observes A's *entity* through the same
//! public API (`handle.entity(id)`, fed by the driver's read-model from the
//! server's `ADD_ENTITY` / `MOVE_ENTITY_POS` / `TELEPORT_ENTITY` broadcasts).
//! The gate asserts B's observed displacement of A matches A's commanded walk.
//!
//! Gated behind the `live-v770` feature AND `#[ignore]`. Run against a real
//! server (offline mode, flat world) on `127.0.0.1:25565` with:
//!
//! ```text
//! cargo test -p lodestone-client --features live-v770 --test live_second_observer -- --ignored --nocapture
//! ```
//!
//! # Why this gate exists (§12.70)
//!
//! The sibling `live_physics_bot.rs` walks a bot through the public API and reads
//! `handle.position()` back — but the driver folds each outgoing
//! `ClientAction::Move` into an **optimistic local prediction**, so that
//! read-back is substantially *our own commanded target*, not server-confirmed
//! displacement. Agreement between a component and its own forecast is not
//! evidence, and it looks exactly like evidence. (The white-box
//! `crates/protocol/v770/tests/live_physics.rs` gate closes the *arithmetic*
//! claim with the server certifying it via zero corrective teleports — that
//! result is untouched by this and remains the strongest physics evidence.)
//!
//! This gate is the first movement assertion in the project where **the observer
//! is not the mover**. B's knowledge of A's position can *only* have come from
//! the server broadcasting A's validated movement to B — A's local prediction is
//! invisible to B. So B seeing A displace by the commanded distance is the server
//! itself certifying that the walk really happened in the shared world.
//!
//! # Controls (every assertion of a presence/absence is paired, §12.53)
//!
//!   * **Positive:** A walks +X ~4 blocks; B's independently-observed
//!     displacement of A must agree with A's own walk *and* clear an absolute
//!     floor, so a B that merely reports A's spawn position forever fails.
//!   * **Negative:** *before* the walk, while A is verified at rest, A sends
//!     nothing for ~1.5 s and B must observe **no** displacement of A. Without
//!     this, a detector that always reports motion (or reports A's live-updating
//!     position as "movement") would pass the positive case trivially.
//!   * **Agreement / interference gate:** B's server-fed observation of A must
//!     match A's *own* reported position while at rest (start and end of the
//!     walk). On this shared survival superflat a mob can knock A around —
//!     frequently via a velocity packet that A's local model never applies, so
//!     the corrective-teleport counter stays flat while the server (and thus B)
//!     sees A move. Divergence between B's view and A's own position is the
//!     universal signal that the measured interval was contaminated; such a run
//!     is discarded and retried rather than mis-reported as a parity failure.
//!     A is matched in B's read-model by the exact offline UUID the server
//!     assigns it, so identification is immune to that position noise.
#![cfg(feature = "live-v770")]

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use lodestone_client::{
    BlockPos, ClientBuilder, ClientEvent, ClientHandle, EntityView, EventStream, LoginProfile,
    ServerAddress, Vec3,
};
use tokio::task::JoinHandle;
use uuid::Uuid;

mod common;
use common::unique_username;

/// Vanilla ground walking speed: ~0.215 blocks/tick.
const STEP_PER_TICK: f64 = 0.215;
/// One server tick; movement is emitted at this cadence like a real client.
const TICK: Duration = Duration::from_millis(50);
/// Horizontal blocks A walks.
const WALK_BLOCKS: f64 = 4.0;
/// Arrival tolerance for A's commanded target.
const ARRIVE_TOL: f64 = 0.35;
/// Max distance between B's server-fed observation of A and A's *own* reported
/// position while A is at rest. When they agree A is genuinely undisturbed and
/// the server's truth equals A's local model; when a mob knocks A (often via a
/// velocity packet A's local model never applies, so the corrective-teleport
/// counter stays flat) they diverge — this is the universal interference signal
/// the teleport counter alone misses.
const AGREE_TOL: f64 = 0.75;
/// Version-free state id of `minecraft:air` (confirmed against live 26.2).
const AIR_STATE_ID: u32 = 0;
/// Bounded join retries for A: mc262 is a *shared* superflat server whose spawn
/// terrain siblings modify, so a random spawn occasionally lands over an
/// obstructed lane where a constant-Y walk is correctly rejected every tick.
const MAX_JOIN_ATTEMPTS: usize = 8;

fn horizontal_dist(a: Vec3, b: Vec3) -> f64 {
    let dx = a.x - b.x;
    let dz = a.z - b.z;
    (dx * dx + dz * dz).sqrt()
}

fn dist3(a: Vec3, b: Vec3) -> f64 {
    let dx = a.x - b.x;
    let dy = a.y - b.y;
    let dz = a.z - b.z;
    (dx * dx + dy * dy + dz * dz).sqrt()
}

/// Minimal MD5 (RFC 1321). Used only to reproduce Java's
/// `UUID.nameUUIDFromBytes`, which the server uses to derive an offline player's
/// UUID from its name when `online-mode=false`. Standalone so the test pulls in
/// no new dependency; this is a generic, public-domain algorithm and is not
/// derived from Minecraft source in any way.
fn md5(input: &[u8]) -> [u8; 16] {
    #[rustfmt::skip]
    const S: [u32; 64] = [
        7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22,
        5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9, 14, 20,
        4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23,
        6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
    ];
    #[rustfmt::skip]
    const K: [u32; 64] = [
        0xd76aa478, 0xe8c7b756, 0x242070db, 0xc1bdceee,
        0xf57c0faf, 0x4787c62a, 0xa8304613, 0xfd469501,
        0x698098d8, 0x8b44f7af, 0xffff5bb1, 0x895cd7be,
        0x6b901122, 0xfd987193, 0xa679438e, 0x49b40821,
        0xf61e2562, 0xc040b340, 0x265e5a51, 0xe9b6c7aa,
        0xd62f105d, 0x02441453, 0xd8a1e681, 0xe7d3fbc8,
        0x21e1cde6, 0xc33707d6, 0xf4d50d87, 0x455a14ed,
        0xa9e3e905, 0xfcefa3f8, 0x676f02d9, 0x8d2a4c8a,
        0xfffa3942, 0x8771f681, 0x6d9d6122, 0xfde5380c,
        0xa4beea44, 0x4bdecfa9, 0xf6bb4b60, 0xbebfbc70,
        0x289b7ec6, 0xeaa127fa, 0xd4ef3085, 0x04881d05,
        0xd9d4d039, 0xe6db99e5, 0x1fa27cf8, 0xc4ac5665,
        0xf4292244, 0x432aff97, 0xab9423a7, 0xfc93a039,
        0x655b59c3, 0x8f0ccc92, 0xffeff47d, 0x85845dd1,
        0x6fa87e4f, 0xfe2ce6e0, 0xa3014314, 0x4e0811a1,
        0xf7537e82, 0xbd3af235, 0x2ad7d2bb, 0xeb86d391,
    ];
    let (mut a0, mut b0, mut c0, mut d0) =
        (0x67452301u32, 0xefcdab89u32, 0x98badcfeu32, 0x10325476u32);

    let mut msg = input.to_vec();
    let bit_len = (input.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_le_bytes());

    for chunk in msg.chunks_exact(64) {
        let mut m = [0u32; 16];
        for (i, word) in m.iter_mut().enumerate() {
            *word = u32::from_le_bytes([
                chunk[i * 4],
                chunk[i * 4 + 1],
                chunk[i * 4 + 2],
                chunk[i * 4 + 3],
            ]);
        }
        let (mut a, mut b, mut c, mut d) = (a0, b0, c0, d0);
        for i in 0..64 {
            let (f, g) = match i {
                0..=15 => ((b & c) | (!b & d), i),
                16..=31 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                32..=47 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(K[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        a0 = a0.wrapping_add(a);
        b0 = b0.wrapping_add(b);
        c0 = c0.wrapping_add(c);
        d0 = d0.wrapping_add(d);
    }

    let mut out = [0u8; 16];
    out[0..4].copy_from_slice(&a0.to_le_bytes());
    out[4..8].copy_from_slice(&b0.to_le_bytes());
    out[8..12].copy_from_slice(&c0.to_le_bytes());
    out[12..16].copy_from_slice(&d0.to_le_bytes());
    out
}

/// Reproduce `UUID.nameUUIDFromBytes(("OfflinePlayer:"+name).getBytes(UTF_8))`,
/// the UUID the server assigns an offline-mode player's entity and broadcasts to
/// every other client. B matches A by *this* UUID rather than by position, which
/// is immune to the mob knockback / other-player position noise a shared
/// survival superflat introduces.
fn offline_uuid(name: &str) -> Uuid {
    let mut hash = md5(format!("OfflinePlayer:{name}").as_bytes());
    hash[6] = (hash[6] & 0x0f) | 0x30; // set version to 3 (name-based, MD5)
    hash[8] = (hash[8] & 0x3f) | 0x80; // set IETF variant
    Uuid::from_bytes(hash)
}

#[test]
fn offline_uuid_matches_vanilla_derivation() {
    // Reference values computed independently (Java `UUID.nameUUIDFromBytes` /
    // Python `hashlib.md5` + RFC 4122 v3 bit-twiddling). If our MD5 or the
    // version/variant masking drifts, B would match the wrong (or no) entity and
    // the live gate would fail confusingly — this catches it offline.
    assert_eq!(
        offline_uuid("Notch"),
        Uuid::parse_str("b50ad385-829d-3141-a216-7e7d7539ba7f").unwrap()
    );
    assert_eq!(
        offline_uuid("abc123"),
        Uuid::parse_str("4062f8b7-64b0-384d-8ad1-4206c09391ad").unwrap()
    );
}

/// Server-truth correction signal for A (the only one the public API exposes).
#[derive(Default)]
struct Corrections {
    teleports: AtomicUsize,
    disconnected: AtomicBool,
}

/// Is A's commanded +X walk lane walkable? For every block column the walk
/// crosses, the block below the feet must be a loaded solid and the feet/head
/// blocks must be loaded air. Pure public-API (`block_at`), so no RCON needed.
fn lane_is_clean(handle: &ClientHandle, start: Vec3, blocks: f64) -> bool {
    let fx = start.x.floor() as i32;
    let fy = start.y.floor() as i32;
    let fz = start.z.floor() as i32;
    let last = blocks.ceil() as i32 + 1;
    for dx in 0..=last {
        let below = handle.block_at(BlockPos::new(fx + dx, fy - 1, fz));
        let feet = handle.block_at(BlockPos::new(fx + dx, fy, fz));
        let head = handle.block_at(BlockPos::new(fx + dx, fy + 1, fz));
        match (below, feet, head) {
            (Some(b), Some(AIR_STATE_ID), Some(AIR_STATE_ID)) if b != AIR_STATE_ID => {}
            _ => return false,
        }
    }
    true
}

/// A fully joined, settled walker whose commanded lane is verified clean.
struct Walker {
    handle: ClientHandle,
    drain: JoinHandle<()>,
    corrections: Arc<Corrections>,
    start: Vec3,
    /// The UUID the server assigned this walker's entity (offline derivation),
    /// used by B to identify A unambiguously.
    offline_uuid: Uuid,
}

/// Connect A, reach Play, load terrain, wait out the client-load validation
/// window, and verify the +X walk lane is clean. `None` = unsuitable spawn,
/// caller should retry with a fresh join.
async fn join_walker(server: &ServerAddress) -> Option<Walker> {
    let username = unique_username();
    let offline_uuid = offline_uuid(&username);
    let profile = LoginProfile {
        username,
        uuid: Uuid::new_v4(),
    };
    let adapter = lodestone_registry::adapter_for_protocol(776)
        .expect("v770 family compiled into the registry via the live-v770 feature");

    let (handle, mut events) = ClientBuilder::new(server.clone(), profile, adapter)
        .connect()
        .await
        .expect("connect walker to live server");

    let corrections = Arc::new(Corrections::default());
    let drain = {
        let corrections = Arc::clone(&corrections);
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                match event {
                    ClientEvent::TeleportPlayer { .. } => {
                        corrections.teleports.fetch_add(1, Ordering::SeqCst);
                    }
                    ClientEvent::Disconnect { .. } => {
                        corrections.disconnected.store(true, Ordering::SeqCst);
                        break;
                    }
                    _ => {}
                }
            }
        })
    };

    async fn abort(mut handle: ClientHandle, drain: JoinHandle<()>) {
        handle.shutdown();
        let _ = handle.join().await;
        drain.abort();
    }

    handle
        .wait_for_login(Duration::from_secs(30))
        .await
        .expect("walker should reach Play");
    if handle
        .wait_for_chunks(1, Duration::from_secs(30))
        .await
        .is_err()
        || !handle.health().is_some_and(|h| h > 0.0)
    {
        eprintln!("walker: no chunks / inherited corpse; retrying");
        abort(handle, drain).await;
        return None;
    }
    if handle
        .wait_for(Duration::from_secs(10), |h| h.position().is_some())
        .await
        .is_err()
    {
        eprintln!("walker: server never placed us; retrying");
        abort(handle, drain).await;
        return None;
    }
    // The driver auto-sends `player_loaded` on the placement teleport waited on
    // just above, zeroing the server's client-load timer — so the walker's
    // movement is validated immediately with no window left to wait out. (The
    // observer's own settle and the entity-propagation waits downstream are
    // separate conditions and are unaffected.)
    if corrections.disconnected.load(Ordering::SeqCst) {
        eprintln!("walker: disconnected before walking; retrying");
        abort(handle, drain).await;
        return None;
    }
    let start = handle
        .position()
        .expect("walker position known after settle");
    if !lane_is_clean(&handle, start, WALK_BLOCKS) {
        eprintln!(
            "walker: spawn ({:.1},{:.1},{:.1}) has an obstructed +X lane; retrying",
            start.x, start.y, start.z
        );
        abort(handle, drain).await;
        return None;
    }
    Some(Walker {
        handle,
        drain,
        corrections,
        start,
        offline_uuid,
    })
}

/// An observer client that discards its own events (keeping the driver flowing
/// so the read-model stays current) but tracks disconnects.
struct Observer {
    handle: ClientHandle,
    drain: JoinHandle<()>,
    disconnected: Arc<AtomicBool>,
}

/// Connect B, reach Play, load terrain, and settle. B does not move, so its own
/// spawn terrain is irrelevant; it exists only to watch A.
async fn join_observer(server: &ServerAddress) -> Observer {
    let profile = LoginProfile {
        username: unique_username(),
        uuid: Uuid::new_v4(),
    };
    let adapter = lodestone_registry::adapter_for_protocol(776)
        .expect("v770 family compiled into the registry via the live-v770 feature");

    let (handle, mut events): (ClientHandle, EventStream) =
        ClientBuilder::new(server.clone(), profile, adapter)
            .connect()
            .await
            .expect("connect observer to live server");

    // The read-model is updated by the driver *before* each event is forwarded,
    // so we only need to drain (and discard) the stream to prevent the bounded
    // channel from back-pressuring the driver. Entity tracking is read via
    // `handle.entity(id)`, i.e. the production read-model, not this loop.
    let disconnected = Arc::new(AtomicBool::new(false));
    let drain = {
        let disconnected = Arc::clone(&disconnected);
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if matches!(event, ClientEvent::Disconnect { .. }) {
                    disconnected.store(true, Ordering::SeqCst);
                    break;
                }
            }
        })
    };

    handle
        .wait_for_login(Duration::from_secs(30))
        .await
        .expect("observer should reach Play");
    handle
        .wait_for_chunks(1, Duration::from_secs(30))
        .await
        .expect("observer got no chunks (inherited corpse?)");
    assert!(
        handle.health().is_some_and(|h| h > 0.0),
        "observer health not positive (inherited corpse?)"
    );
    tokio::time::sleep(Duration::from_secs(5)).await;

    Observer {
        handle,
        drain,
        disconnected,
    }
}

/// Find A's entity in B's public read-model by the **offline UUID** the server
/// assigned it. This is exact and position-independent: mob knockback, other
/// players, or A being shoved between polls cannot cause a mis-identification
/// (the failure mode of a nearest-position match on a shared survival server).
fn find_walker_entity(observer: &ClientHandle, uuid: Uuid) -> Option<EntityView> {
    observer
        .entities()
        .into_iter()
        .find(|e| e.uuid == Some(uuid))
}

/// Poll B until it stops observing A move (position stable across a few polls),
/// so we measure after the last broadcast movement packet has landed.
async fn observed_position_settled(observer: &ClientHandle, entity_id: i32) -> Vec3 {
    let mut last = observer
        .entity(entity_id)
        .map(|e| e.position)
        .unwrap_or(Vec3::new(0.0, 0.0, 0.0));
    let mut stable = 0;
    for _ in 0..40 {
        tokio::time::sleep(TICK).await;
        let now = observer
            .entity(entity_id)
            .map(|e| e.position)
            .unwrap_or(last);
        if dist3(now, last) < 1e-6 {
            stable += 1;
            if stable >= 4 {
                return now;
            }
        } else {
            stable = 0;
        }
        last = now;
    }
    last
}

/// Bounded scenario retries. mc262 is a *shared* survival superflat
/// (`difficulty=easy`) that spawns hostile mobs near the origin; a mob can knock
/// the (necessarily stationary) walker around during setup — often via a
/// velocity packet that leaves the walker's local model, and therefore the
/// corrective-teleport counter, untouched while the server still moves it. That
/// is environmental, not a physics fault, so any scenario in which the walker is
/// disturbed during the measured window — detected by a corrective teleport, an
/// over-stride, or B's server-fed view diverging from the walker's own position
/// (the universal signal) — is discarded and retried. Physics divergence is
/// already covered by `live_physics.rs` (white-box, zero corrections over 100
/// ticks) and `live_physics_bot.rs`; this gate's novel claim is the
/// observer↔mover parity, so treating walker-instability as interference here
/// does not weaken any physics guarantee.
const MAX_SCENARIO_ATTEMPTS: usize = 20;

/// The successful measurement of one clean scenario.
struct Report {
    walker_id: i32,
    ticks: u32,
    walker_displacement: f64,
    observed_displacement: f64,
    parity_gap: f64,
    idle_observed_drift: f64,
    entity_type: String,
}

/// Outcome of attempting one full scenario.
enum Outcome {
    /// A clean, fully-asserted run.
    Ok(Report),
    /// Environmental interference (mob knockback / shared-server churn); retry.
    Interference(String),
}

/// Run one complete scenario: join A on a clean lane, join B, have B locate A's
/// entity **by A's exact offline UUID**, run the no-input negative control while
/// A is verified at rest, then walk A while B watches. Any disturbance of the
/// walker during the measured window — corrective teleport, over-stride, or B's
/// server-fed view diverging from A's own position — returns `Interference`
/// (retry). A parity mismatch while the walker is *undisturbed* is a hard
/// `panic!` — a real observer/mover pipeline bug, not interference. Both clients
/// are shut down before returning on every path.
async fn run_scenario(server: &ServerAddress) -> Outcome {
    // --- Client A on a verified-clean walk lane (retry lane variance). ---
    let mut walker = None;
    for _ in 0..MAX_JOIN_ATTEMPTS {
        if let Some(w) = join_walker(server).await {
            walker = Some(w);
            break;
        }
    }
    let Walker {
        handle,
        drain: a_drain,
        corrections,
        start,
        offline_uuid: walker_uuid,
    } = match walker {
        Some(w) => w,
        None => return Outcome::Interference("walker found no clean lane this attempt".into()),
    };

    // --- Client B: an independent observer, joined after A exists. ---
    let Observer {
        handle: obs,
        drain: b_drain,
        disconnected: obs_disc,
    } = join_observer(server).await;

    // Helper to tear both clients down before returning.
    async fn teardown(
        mut a: ClientHandle,
        a_drain: JoinHandle<()>,
        mut b: ClientHandle,
        b_drain: JoinHandle<()>,
    ) {
        a.shutdown();
        let _ = a.join().await;
        a_drain.abort();
        b.shutdown();
        let _ = b.join().await;
        b_drain.abort();
    }

    // Baseline: A must stay un-teleported from here through the measurement.
    let a_tp_baseline = corrections.teleports.load(Ordering::SeqCst);

    // Locate A's entity in B's read-model by A's server-assigned offline UUID.
    // This is exact and position-independent, so mob knockback cannot cause a
    // mis-match. A teleport of A during discovery is still interference (its
    // pre-walk position would be unstable), so we bail on that.
    let mut walker_view = None;
    let find_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < find_deadline {
        if corrections.teleports.load(Ordering::SeqCst) != a_tp_baseline {
            teardown(handle, a_drain, obs, b_drain).await;
            return Outcome::Interference("walker teleported during entity discovery".into());
        }
        if let Some(v) = find_walker_entity(&obs, walker_uuid) {
            walker_view = Some(v);
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let walker_view = match walker_view {
        Some(v) => v,
        None => {
            // Could be a real dispatch regression, but on a mob-heavy shared
            // server it is more often B being knocked out of A's range before
            // the server broadcasts A to it. Retry; if it never resolves,
            // MAX_SCENARIO_ATTEMPTS exhausts and fails loudly.
            let players_seen = obs
                .entities()
                .into_iter()
                .filter(|e| e.entity_type.to_string() == "minecraft:player")
                .count();
            teardown(handle, a_drain, obs, b_drain).await;
            return Outcome::Interference(format!(
                "observer did not see the walker's entity by UUID ({players_seen} players in range)"
            ));
        }
    };
    let walker_id = walker_view.entity_id;
    let entity_type = walker_view.entity_type.to_string();
    // Matched by exact server-assigned UUID, so this is unambiguously A's entity
    // regardless of where mobs may have pushed it. A teleport between the match
    // and here (mob kill → respawn) is interference, not a wrong-entity bug.
    if corrections.teleports.load(Ordering::SeqCst) != a_tp_baseline {
        teardown(handle, a_drain, obs, b_drain).await;
        return Outcome::Interference("walker teleported just after entity discovery".into());
    }
    eprintln!(
        "observer sees walker: id={walker_id} type={entity_type} uuid={walker_uuid} \
         obs_pos={:?} (walker's own pos {start:?})",
        walker_view.position
    );

    // --- Re-anchor to A's *live* position. ---
    // B's ~5 s join + discovery gives mobs time to nudge A, so measuring from a
    // stale settle-time `start` (and aiming a stale target) would be wrong.
    // Re-verify the lane at the live anchor; an obstructed lane here is
    // interference (A drifted onto bad terrain), not a physics fault.
    let walk_start = handle
        .position()
        .expect("walker position known before walk");
    if !lane_is_clean(&handle, walk_start, WALK_BLOCKS) {
        teardown(handle, a_drain, obs, b_drain).await;
        return Outcome::Interference("walk-start lane obstructed (walker drifted)".into());
    }

    // --- Agreement gate: A must be genuinely at rest and undisturbed. ---
    // B's settled observation of A must match A's own reported position. If they
    // disagree, a mob has knocked A (possibly via a velocity packet that leaves
    // A's local model — and thus the teleport counter — untouched), so no clean
    // measurement is possible this attempt.
    let observed_baseline = observed_position_settled(&obs, walker_id).await;
    let rest_gap = dist3(observed_baseline, walk_start);
    if rest_gap > AGREE_TOL {
        teardown(handle, a_drain, obs, b_drain).await;
        return Outcome::Interference(format!(
            "walker disturbed pre-walk: B sees A at {observed_baseline:?} but A reports \
             {walk_start:?} (gap {rest_gap:.3} > {AGREE_TOL}) — mob knockback, retrying"
        ));
    }

    // --- Negative control (measured while A is verified at rest). ---
    // A sends nothing for ~1.5 s; B must observe no displacement of A. Because
    // the agreement gate just proved A is undisturbed, a non-trivial observed
    // drift here is genuine phantom motion in the detector — a real bug — so the
    // drift bound is a hard assertion. But re-check agreement afterwards: if a
    // mob shoved A *during* the idle window (agreement breaks, or a teleport
    // fires), B legitimately saw motion and the control is unmeasurable → retry.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let idle_tp = corrections.teleports.load(Ordering::SeqCst) != a_tp_baseline;
    let idle_observed = observed_position_settled(&obs, walker_id).await;
    let idle_own = handle.position().unwrap_or(walk_start);
    let idle_drift = horizontal_dist(idle_observed, observed_baseline);
    if idle_tp || dist3(idle_observed, idle_own) > AGREE_TOL {
        teardown(handle, a_drain, obs, b_drain).await;
        return Outcome::Interference("walker knocked during the no-input control".into());
    }
    let idle_observed_drift = idle_drift;
    assert!(
        idle_observed_drift <= 0.1,
        "observer saw the (undisturbed) walker drift {idle_observed_drift:.4} blocks while it \
         stood still — the displacement detector reports phantom motion, so any positive result \
         is not trustworthy"
    );

    // --- Positive: A walks +X through the public API; B watches. ---
    let target = Vec3::new(walk_start.x + WALK_BLOCKS, walk_start.y, walk_start.z);
    let mut prev = walk_start;
    let mut ticks = 0u32;
    let max_ticks = 200u32;
    loop {
        let pos = handle
            .position()
            .expect("walker position known while walking");
        if horizontal_dist(pos, target) <= ARRIVE_TOL {
            break;
        }
        if ticks >= max_ticks {
            teardown(handle, a_drain, obs, b_drain).await;
            return Outcome::Interference("walk did not converge (walker perturbed)".into());
        }
        handle
            .step_toward(target, STEP_PER_TICK)
            .expect("step_toward through the public API");
        tokio::time::sleep(TICK).await;
        ticks += 1;
        // A corrective teleport mid-walk on this shared server is mob knockback,
        // not a physics fault (that is asserted zero in the physics gates). Treat
        // it as interference and retry the scenario.
        if corrections.teleports.load(Ordering::SeqCst) != a_tp_baseline
            || obs_disc.load(Ordering::SeqCst)
            || corrections.disconnected.load(Ordering::SeqCst)
        {
            teardown(handle, a_drain, obs, b_drain).await;
            return Outcome::Interference("walker teleported or a client dropped mid-walk".into());
        }
        let now = handle
            .position()
            .expect("walker position known while walking");
        let step = horizontal_dist(now, prev);
        // Per-tick over-stride guard. A step larger than one stride means A's
        // local position jumped. The teleport-count check above races server
        // knockback (the corrective packet can land between the two reads on this
        // mob-heavy shared server), so an over-stride here is treated as
        // interference, not a hard failure: the movement *primitive* teleporting
        // is already asserted impossible by `live_physics.rs` (100 ticks, zero
        // corrections, white-box) and `live_physics_bot.rs`. This gate's job is
        // the observer↔mover parity, so it declines to adjudicate a physics fault
        // it cannot cleanly distinguish from knockback, and retries instead.
        if step > STEP_PER_TICK + 0.05 {
            teardown(handle, a_drain, obs, b_drain).await;
            return Outcome::Interference(format!(
                "walker advanced {step:.4} in one tick (> one stride) — knockback race, retrying"
            ));
        }
        prev = now;
    }
    let a_end = handle.position().expect("walker position known after walk");
    let walker_displacement = horizontal_dist(a_end, walk_start);

    // Let the last broadcast movement packets reach B, then read A's position
    // from B's public read-model. If a mob knocked A out of B's view distance
    // (or killed it) mid-walk, B no longer tracks the entity — a clean
    // interference case, reported as such rather than as a spurious huge gap.
    if obs.entity(walker_id).is_none() {
        teardown(handle, a_drain, obs, b_drain).await;
        return Outcome::Interference(
            "observer lost the walker entity mid-walk (knocked out of range / removed)".into(),
        );
    }
    let observed_after_walk = observed_position_settled(&obs, walker_id).await;
    let observed_displacement = horizontal_dist(observed_after_walk, observed_baseline);
    if corrections.teleports.load(Ordering::SeqCst) != a_tp_baseline
        || obs_disc.load(Ordering::SeqCst)
    {
        teardown(handle, a_drain, obs, b_drain).await;
        return Outcome::Interference(
            "walker teleported or observer dropped after the walk".into(),
        );
    }
    // A mob could have knocked A during the walk via velocity (no teleport, no
    // over-stride) — that shows up as B's settled view of A diverging from A's
    // own end position. If so the measured interval was contaminated → retry.
    let end_gap = dist3(observed_after_walk, a_end);
    if end_gap > AGREE_TOL {
        teardown(handle, a_drain, obs, b_drain).await;
        return Outcome::Interference(format!(
            "walker disturbed during walk: B sees A at {observed_after_walk:?} but A ended at \
             {a_end:?} (gap {end_gap:.3} > {AGREE_TOL}) — mob knockback, retrying"
        ));
    }

    // Absolute floor: B independently saw A cross most of the commanded distance.
    // A detector that only reports A's spawn (the trivial failure) lands near 0.
    // With the walker verified undisturbed (agreement held at both ends), a short
    // observation is a real parity failure, so this is a hard assertion.
    assert!(
        observed_displacement >= WALK_BLOCKS - 1.0,
        "observer saw the (undisturbed) walker move only {observed_displacement:.4} blocks; a \
         commanded {WALK_BLOCKS}-block walk should be observed as ~{walker_displacement:.4}. The \
         server did not broadcast the walk, or B's entity tracking is not following it."
    );
    // Parity: B's independent observation agrees with A's own walk. The server
    // sits between observer and mover, so this cannot pass on A's prediction.
    let parity_gap = (observed_displacement - walker_displacement).abs();
    assert!(
        parity_gap <= 0.5,
        "observer measured {observed_displacement:.4} blocks of walker displacement but the \
         walker's own walk was {walker_displacement:.4} (gap {parity_gap:.4} > 0.5) — the \
         observed and commanded motions disagree"
    );

    teardown(handle, a_drain, obs, b_drain).await;
    Outcome::Ok(Report {
        walker_id,
        ticks,
        walker_displacement,
        observed_displacement,
        parity_gap,
        idle_observed_drift,
        entity_type,
    })
}

#[tokio::test]
#[ignore = "requires a live Minecraft server on 127.0.0.1:25565"]
async fn observer_confirms_walker_displacement() {
    let server = ServerAddress {
        host: "127.0.0.1".into(),
        port: 25565,
    };

    let mut report = None;
    for attempt in 1..=MAX_SCENARIO_ATTEMPTS {
        eprintln!("scenario attempt {attempt}/{MAX_SCENARIO_ATTEMPTS}");
        match run_scenario(&server).await {
            Outcome::Ok(r) => {
                report = Some(r);
                break;
            }
            Outcome::Interference(why) => {
                eprintln!("  attempt {attempt} discarded (interference): {why}");
            }
        }
    }

    let report = report.unwrap_or_else(|| {
        panic!(
            "no clean scenario in {MAX_SCENARIO_ATTEMPTS} attempts — the walker was perturbed \
             (mob knockback / shared-server churn) or the observer could not keep the walker in \
             range every time. This is not a parity failure (a real mismatch panics with the \
             walker stable), but the gate could not obtain a clean measurement window. Re-run; \
             if it never succeeds, the entity-spawn/move dispatch to a second observer may have \
             regressed — check `live_oracle` and the ADD_ENTITY/MOVE_ENTITY_POS handlers."
        )
    });

    eprintln!(
        "=== LIVE SECOND-OBSERVER GATE REPORT ===\n\
         walker entity id (in B) : {} ({})\n\
         walk ticks              : {}\n\
         walker own displacement : {:.4} blocks (local prediction)\n\
         OBSERVER displacement   : {:.4} blocks (server-broadcast, B != A)\n\
         parity gap              : {:.4} blocks (<= 0.5)\n\
         negative control drift  : {:.4} blocks (A idle, B observes)",
        report.walker_id,
        report.entity_type,
        report.ticks,
        report.walker_displacement,
        report.observed_displacement,
        report.parity_gap,
        report.idle_observed_drift,
    );
}
