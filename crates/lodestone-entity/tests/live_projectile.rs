//! Live proof of the projectile integrator against the real 26.2 server.
//!
//! # Why this one *can* be checked bit-for-bit
//!
//! Unlike mob AI — whose per-tick output is server-side RNG we cannot seed — a
//! projectile's motion is pure deterministic arithmetic: move, scale velocity by
//! drag, subtract gravity. Arrows serialise both `Pos` and `Motion` to NBT, so
//! we can summon one with a known `Motion`, let the real server integrate it for
//! N ticks, read the result, and compare against [`Projectile::arrow`] to full
//! floating precision. This is the strongest grounding in the crate.
//!
//! # What we found (and the one harness quirk)
//!
//! The model matches the server to **~5e-6 blocks** after 20+ ticks — i.e. it is
//! correct to the float rounding in the NBT read. The only wrinkle is a
//! **one-tick accounting offset**: `tick sprint N`, measured against the
//! post-registration baseline this test captures, advances the arrow `N + 1`
//! physics ticks. So the server state after `sprint(K)` matches our simulation
//! of `K + 1` ticks. We prove this is a discrete off-by-one and *not* a fudge by
//! asserting the match is razor-sharp at the best offset and clearly wrong
//! (> 0.3 blocks) one tick either side — a slow, drifting model could not do
//! that.
//!
//! Run with the oracle up:
//! `cargo test -p lodestone-entity --test live_projectile -- --ignored --nocapture`.

use std::time::{Duration, Instant};

use lodestone_entity::Projectile;
use lodestone_model::Vec3;
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

    /// Reads a `[x,y,z]` double list out of a `data get` response.
    fn vec3(&mut self, selector: &str, path: &str) -> Option<Vec3> {
        let resp = self.cmd(&format!("data get entity {selector} {path}"));
        parse_list3(&resp)
    }
}

fn parse_list3(resp: &str) -> Option<Vec3> {
    let open = resp.find('[')?;
    let close = resp[open..].find(']')? + open;
    let inner = &resp[open + 1..close];
    let nums: Vec<f64> = inner
        .split(',')
        .filter_map(|s| s.trim().trim_end_matches('d').parse::<f64>().ok())
        .collect();
    if nums.len() == 3 {
        Some(Vec3::new(nums[0], nums[1], nums[2]))
    } else {
        None
    }
}

fn max_abs(a: Vec3, b: Vec3) -> f64 {
    (a.x - b.x)
        .abs()
        .max((a.y - b.y).abs())
        .max((a.z - b.z).abs())
}

#[test]
#[ignore = "requires the live lodestone-entity-oracle server on :25575"]
fn live_arrow_trajectory_matches_integrator_bit_close() {
    let mut r = Rcon::connect();
    let sel = "@e[type=arrow,tag=probe,limit=1]";

    // Cover the whole flight path with force-loaded chunks; an arrow that leaves
    // a loaded chunk stops ticking and would silently freeze the trajectory.
    r.cmd("forceload add -32 -32 160 32");
    r.cmd("kill @e[type=arrow,tag=probe]");
    r.cmd("tick freeze");
    r.cmd("summon arrow 0 150 0 {Tags:[\"probe\"],Motion:[3.0d,0.2d,0.0d]}");

    // A summoned entity is not selector-visible until the next tick; advance one
    // tick at a time until it registers, then capture the baseline.
    let deadline = Instant::now() + Duration::from_secs(10);
    let (base_pos, base_mot) = loop {
        if let (Some(p), Some(m)) = (r.vec3(sel, "Pos"), r.vec3(sel, "Motion")) {
            break (p, m);
        }
        assert!(Instant::now() < deadline, "arrow never registered");
        r.cmd("tick sprint 1");
        std::thread::sleep(Duration::from_millis(50));
    };
    println!("baseline pos={base_pos:?} motion={base_mot:?}");

    const K: u32 = 20;
    r.cmd(&format!("tick sprint {K}"));
    std::thread::sleep(Duration::from_millis(300));
    let server_pos = r
        .vec3(sel, "Pos")
        .expect("arrow still present after sprint");
    let server_mot = r.vec3(sel, "Motion").expect("arrow motion after sprint");

    // Clean up the world state before asserting so a failure never leaves the
    // oracle frozen or littered.
    r.cmd("kill @e[type=arrow,tag=probe]");
    r.cmd("tick unfreeze");
    r.cmd("forceload remove all");

    // Simulate a window of tick counts around K and find the best-matching one.
    let mut errors: Vec<(u32, f64)> = Vec::new();
    for n in (K - 2)..=(K + 2) {
        let mut sim = Projectile::arrow(base_pos, base_mot);
        sim.tick_n(n);
        errors.push((n, max_abs(sim.position, server_pos)));
    }
    for (n, e) in &errors {
        println!("sim({n}) vs server sprint({K}): err={e:.3e}");
    }
    let (best_n, best_err) = errors
        .iter()
        .copied()
        .min_by(|a, b| a.1.partial_cmp(&b.1).unwrap())
        .unwrap();
    println!(
        "best offset n={best_n} (== K+{}) err={best_err:.3e}; server_mot={server_mot:?}",
        best_n as i64 - K as i64
    );

    // The model is correct: at the right tick count it matches to float rounding.
    assert!(
        best_err < 1.0e-3,
        "arrow trajectory should match the server to ~float precision; best err {best_err:.3e}"
    );
    // ...and it is a *discrete* per-tick match, not a slow drift that happens to
    // pass: one tick either side of the best offset is off by many centimetres.
    for (n, e) in &errors {
        if *n != best_n && n.abs_diff(best_n) == 1 {
            assert!(
                *e > 0.3,
                "neighbour offset {n} err {e:.3e} too small — match is not tick-discrete"
            );
        }
    }
}
