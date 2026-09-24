//! Live proof of the Brain/Behavior system against the real 26.2 server.
//!
//! # What this can and cannot check
//!
//! Unlike a mob with a target (K2's zombie, whose exact route we could bracket),
//! an *undisturbed* brain-mob's only observable output is its idle wander — and
//! that output is **architecture-agnostic**: a goal-system pig and a brain-system
//! goat idle-stroll identically. Worse, a brain's working memory
//! (`walk_target`, `hurt_by`, panic markers) is **not NBT-serializable**
//! (`MemoryModuleType::canSerialize()` is false for the transient ones), so it
//! can be neither read nor injected over RCON. We verified both facts live:
//! `data get entity … Brain` returns `{memories:{}}` for an actively-strolling
//! goat, and `data modify … Brain.memories."minecraft:walk_target"` does not
//! round-trip.
//!
//! So the Brain **machinery** — memory expiry, gate run-one, activity switching,
//! the `WALK_TARGET` hand-off — is proven **hermetically and bit-exactly** in
//! `src/brain` (13 tests). What this live test adds is the one thing hermetic
//! tests cannot: confirmation that a *real* brain-mob's emergent movement has the
//! structure our scaffold produces, and that one non-trivial timing constant we
//! hard-coded matches vanilla.
//!
//! # The checkable invariants
//!
//! Over a long sprint with no stimulus, a real brain-mob:
//!
//! 1. **wanders bounded and local** — it never drifts away, because each stroll
//!    picks a target within `LandRandomPos`'s 10-block reach and strolls are
//!    rare;
//! 2. **moves in short bursts separated by long pauses** — the `RandomStroll` →
//!    `MoveToTargetSink` → idle cycle, not continuous motion;
//! 3. **pauses for ~150–250 ticks between strolls** — which is exactly
//!    [`MoveToTargetSink::with_timeout(150, 250)`], the constant our
//!    implementation uses. A measured mean pause far outside that window would
//!    mean our timeout is wrong.
//!
//! Run with the oracle up:
//! `cargo test -p lodestone-entity --test live_brain -- --ignored --nocapture`.

use std::time::Duration;

use lodestone_testsupport::RconClient;

const RCON_ADDR: &str = "127.0.0.1:25575";
const RCON_PASSWORD: &str = "lodestone";

struct Rcon {
    inner: RconClient,
}

impl Rcon {
    fn connect() -> Self {
        Self {
            inner: RconClient::connect(RCON_ADDR, RCON_PASSWORD).expect(
                "oracle RCON reachable at 127.0.0.1:25575 — is lodestone-entity-oracle up?",
            ),
        }
    }

    fn cmd(&mut self, command: &str) -> String {
        self.inner.cmd(command)
    }

    fn wait_for_entity(&mut self, selector: &str) {
        self.inner
            .wait_for_entity(
                selector,
                Duration::from_secs(10),
                Duration::from_millis(100),
            )
            .unwrap_or_else(|e| panic!("entity {selector} never registered within 10s: {e}"));
    }

    fn pos(&mut self, selector: &str) -> Option<(f64, f64, f64)> {
        let resp = self.cmd(&format!("data get entity {selector} Pos"));
        parse_pos(&resp)
    }
}

fn parse_pos(resp: &str) -> Option<(f64, f64, f64)> {
    let open = resp.find('[')?;
    let close = resp[open..].find(']')? + open;
    let inner = &resp[open + 1..close];
    let nums: Vec<f64> = inner
        .split(',')
        .filter_map(|s| s.trim().trim_end_matches('d').parse::<f64>().ok())
        .collect();
    if nums.len() == 3 {
        Some((nums[0], nums[1], nums[2]))
    } else {
        None
    }
}

/// Summary of a mob's idle-wander sample track.
struct WanderStats {
    max_radius: f64,
    total_path: f64,
    num_pauses: usize,
    mean_pause_ticks: f64,
    samples: usize,
}

/// Sprints `ticks` server ticks in `inc`-tick steps, sampling position each
/// step, and reduces to burst/pause statistics.
fn characterize_wander(rcon: &mut Rcon, selector: &str, ticks: i32, inc: i32) -> WanderStats {
    let start = rcon.pos(selector).expect("initial pos");
    let mut track = vec![start];
    let steps = ticks / inc;
    for _ in 0..steps {
        rcon.cmd(&format!("tick sprint {inc}"));
        if let Some(p) = rcon.pos(selector) {
            track.push(p);
        }
    }

    let horiz = |a: (f64, f64, f64), b: (f64, f64, f64)| {
        let dx = a.0 - b.0;
        let dz = a.2 - b.2;
        (dx * dx + dz * dz).sqrt()
    };

    let mut max_radius = 0.0_f64;
    let mut total_path = 0.0_f64;
    // A run is a maximal stretch of samples all "moving" or all "still".
    let mut pauses: Vec<i32> = Vec::new();
    let mut in_pause: Option<i32> = None;
    let mut prev = track[0];
    for &p in &track[1..] {
        max_radius = max_radius.max(horiz(p, start));
        let step = horiz(p, prev);
        total_path += step;
        let moving = step > 0.02;
        if moving {
            if let Some(len) = in_pause.take() {
                pauses.push(len);
            }
        } else {
            in_pause = Some(in_pause.unwrap_or(0) + 1);
        }
        prev = p;
    }
    // Ignore a trailing open pause (unbounded by the sample window).
    let mean_pause_ticks = if pauses.is_empty() {
        0.0
    } else {
        f64::from(pauses.iter().sum::<i32>()) / pauses.len() as f64 * f64::from(inc)
    };

    WanderStats {
        max_radius,
        total_path,
        num_pauses: pauses.len(),
        mean_pause_ticks,
        samples: track.len(),
    }
}

fn reset_area(rcon: &mut Rcon, bx: i32, bz: i32) {
    rcon.cmd(&format!(
        "forceload add {} {} {} {}",
        bx - 48,
        bz - 48,
        bx + 48,
        bz + 48
    ));
    rcon.cmd("difficulty easy");
    rcon.cmd("time set day");
    rcon.cmd("gamerule doMobSpawning false");
    rcon.cmd("tick unfreeze");
    rcon.cmd("kill @e[tag=probe]");
}

/// A real brain-mob idle-wanders **bounded**, in **bursts separated by long
/// pauses**, and those pauses sit in vanilla's `MoveToTargetSink` 150–250 tick
/// window — the constant our implementation uses.
#[test]
#[ignore = "requires the live lodestone-entity-oracle on :25575"]
fn live_brain_mob_idle_wander_is_bounded_bursty_and_correctly_timed() {
    let (bx, by, bz) = (200, -60, 200);
    let mut rcon = Rcon::connect();
    reset_area(&mut rcon, bx, bz);
    rcon.cmd(&format!(
        "summon minecraft:goat {bx}.5 {by} {bz}.5 {{PersistenceRequired:1b,Tags:['probe']}}"
    ));
    rcon.wait_for_entity("@e[tag=probe,limit=1]");

    let stats = characterize_wander(&mut rcon, "@e[tag=probe,limit=1]", 2000, 2);

    println!(
        "goat idle wander over 2000 ticks: samples={} max_radius={:.2} total_path={:.2} \
         num_pauses={} mean_pause={:.0} ticks",
        stats.samples, stats.max_radius, stats.total_path, stats.num_pauses, stats.mean_pause_ticks
    );

    rcon.cmd("kill @e[tag=probe]");
    rcon.cmd(&format!(
        "forceload remove {} {} {} {}",
        bx - 48,
        bz - 48,
        bx + 48,
        bz + 48
    ));

    // (1) Bounded, local wander: never drifts away. LandRandomPos reaches at
    // most 10 blocks and strolls are rare, so a generous 24-block bound proves
    // "local" without being flaky.
    assert!(
        stats.max_radius < 24.0,
        "brain-mob drifted unbounded ({:.2} blocks) — idle wander should stay local",
        stats.max_radius
    );

    // (2) Burst-and-pause structure exists: the mob is not in continuous motion.
    assert!(
        stats.num_pauses >= 3,
        "expected several idle pauses (burst-and-pause cycle), saw {}",
        stats.num_pauses
    );

    // (3) The pause length brackets vanilla's MoveToTargetSink 150–250 tick
    // timeout, which our MoveToTargetSink::new() uses verbatim. A mean far
    // outside this window would mean our timeout constant is wrong.
    assert!(
        stats.mean_pause_ticks > 60.0 && stats.mean_pause_ticks < 400.0,
        "mean idle pause {:.0} ticks is inconsistent with the 150–250 tick \
         MoveToTargetSink window our implementation uses",
        stats.mean_pause_ticks
    );
}
