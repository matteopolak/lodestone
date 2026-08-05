//! Live navigation oracle: compare a *real* server zombie's route to our
//! `PathFinder` on a matching grid.
//!
//! `#[ignore]`d — needs the isolated `lodestone-entity-oracle` Docker server with
//! RCON (ports 25567 game / 25575 rcon, password `lodestone`). Never touch the
//! shared `lodestone-mc262` / `lodestone-mc189`.
//!
//! ```text
//! cargo test -p lodestone-entity --test live_navigation -- --ignored --nocapture
//! ```
//!
//! # Why this is possible without a connected client
//!
//! The received wisdom was "mobs don't tick unless a player is connected". That
//! is *wrong for the debug tick commands*: `tick sprint N` runs N full server
//! ticks — entity AI included — with no player online. (`tick freeze` + `tick
//! step` does **not** tick mob AI; only `tick sprint` does.) That is the entire
//! reason this oracle exists. We drive the world purely over RCON at op 4.
//!
//! # Why exact position matching is impossible (and what we check instead)
//!
//! Vanilla AI has three unobservable-seed entropy sources here:
//!   * `NearestAttackableTargetGoal` acquires on a random tick interval,
//!   * `PathNavigation` recomputes on a jittered schedule,
//!   * `RandomStrollGoal` injects noise before a target is locked, and the A*
//!     tie-break picks a detour side we cannot predict.
//!
//! So we do **not** assert the zombie's coordinates equal ours. We assert the
//! *invariants both implementations must share* — reachability and that the
//! detour goes around the wall — and we **report the divergence numbers**
//! (max lateral deviation, detour side, tick budget) rather than asserting them.
//!
//! # The fence trick (LOS over the top, unjumpable collision)
//!
//! A zombie only paths to a villager it can *see* (`canSee` raytraces eye→eye),
//! so the obstacle must not block line of sight — but it must block the *path*.
//! A 1-block-tall solid wall fails: mobs **jump** 1-block obstacles (jump height
//! ≈1.25 > 1.0), so the zombie hops straight over it (verified live — it walked
//! through at constant z). A **fence** is the sweet spot: its collision height is
//! **1.5** (the physics fact from `impl-physics`), which exceeds both the 0.6
//! auto-step and the ~1.25 jump, so it cannot be crossed; yet the eye→eye ray at
//! height ≈1.74 passes *over* the 1.5 collision top, so LOS — and target
//! acquisition — still hold. Verified live: the zombie detours around the fence
//! end (max|z|≈4.5) and reaches the villager.
//!
//! # The `Invulnerable` trap (why the lure must be mortal)
//!
//! An `Invulnerable:1b` villager is **never targeted** — vanilla's
//! `TargetingConditions` rejects invulnerable entities, so the zombie just
//! random-strolls and never paths anywhere. (This cost this session a false
//! failure: the "detour" test froze at the origin.) The lure is therefore
//! `NoAI:1b` (stationary, still targetable) and *not* invulnerable; the zombie
//! reaches melee range — and we break out — long before it can kill it.

use lodestone_entity::ai::goals::MeleeAttackGoal;
use lodestone_entity::ai::{GoalSelector, MobController, NavigatingMob};
use lodestone_entity::pathfinding::{
    Aabb, MobShape, PathFinder, PathParams, PathStart, PathType, PathWorld,
};
use lodestone_model::{BlockPos, Vec3};
use lodestone_testsupport::RconClient;
use std::collections::HashSet;
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

const RCON_ADDR: &str = "127.0.0.1:25575";
const RCON_PASSWORD: &str = "lodestone";

/// Serialises the two live tests: both drive the *single* shared oracle by
/// globally killing entities and refilling the arena, so running them in
/// parallel (cargo's default) lets one wipe the other's freshly-summoned mob.
/// A process-wide lock makes them run one at a time regardless of `--test-threads`.
/// Poison is ignored on purpose — a panic in one test must not cascade-fail the
/// next; the arena is reset at the start of each test anyway.
static ORACLE_LOCK: Mutex<()> = Mutex::new(());

fn oracle_guard() -> MutexGuard<'static, ()> {
    ORACLE_LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

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

    /// Reads an entity's `[x, y, z]` position from `data get entity … Pos`.
    fn pos(&mut self, selector: &str) -> Option<(f64, f64, f64)> {
        let resp = self.cmd(&format!("data get entity {selector} Pos"));
        parse_pos(&resp)
    }
}

/// Parses `"... [0.5d, -60.0d, 0.0d]"` into three f64s.
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

// --- Our hermetic side: a grid world matching the RCON arena. -----------------

/// Flat ground at `ground_top`, with a set of fence wall blocks (collision 1.5).
struct ArenaWorld {
    ground_top: i32,
    walls: HashSet<(i32, i32, i32)>,
}

impl ArenaWorld {
    fn is_ground(&self, _x: i32, y: i32, _z: i32) -> bool {
        y <= self.ground_top
    }

    fn is_wall(&self, x: i32, y: i32, z: i32) -> bool {
        self.walls.contains(&(x, y, z))
    }

    fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        self.is_wall(x, y, z) || self.is_ground(x, y, z)
    }
}

impl PathWorld for ArenaWorld {
    fn min_y(&self) -> i32 {
        -64
    }

    fn base_path_type(&self, x: i32, y: i32, z: i32) -> PathType {
        if self.is_solid(x, y, z) {
            PathType::Blocked
        } else {
            PathType::Open
        }
    }

    fn collision_top(&self, x: i32, y: i32, z: i32) -> f64 {
        // A fence stands 1.5 high — above both the 0.6 step and the ~1.25 jump —
        // so a mob cannot cross it. Full ground blocks are the usual 1.0.
        if self.is_wall(x, y, z) {
            1.5
        } else if self.is_ground(x, y, z) {
            1.0
        } else {
            0.0
        }
    }

    fn collides(&self, aabb: Aabb) -> bool {
        let x0 = aabb.min_x.floor() as i32;
        let x1 = (aabb.max_x - 1e-7).floor() as i32;
        let y0 = aabb.min_y.floor() as i32;
        let y1 = (aabb.max_y - 1e-7).floor() as i32;
        let z0 = aabb.min_z.floor() as i32;
        let z1 = (aabb.max_z - 1e-7).floor() as i32;
        for x in x0..=x1 {
            for y in y0..=y1 {
                for z in z0..=z1 {
                    if self.is_solid(x, y, z) {
                        return true;
                    }
                }
            }
        }
        false
    }

    fn is_water(&self, _x: i32, _y: i32, _z: i32) -> bool {
        false
    }
}

/// Resets the arena to a clean flat area and pins the test chunks loaded.
fn reset_arena(rcon: &mut Rcon) {
    rcon.cmd("tick unfreeze");
    rcon.cmd("gamerule doMobSpawning false");
    rcon.cmd("gamerule doDaylightCycle false");
    rcon.cmd("gamerule doWeatherCycle false");
    rcon.cmd("gamerule randomTickSpeed 0");
    rcon.cmd("difficulty easy"); // hostile AI needs at least easy (not peaceful)
    rcon.cmd("time set midnight"); // keep the zombie from burning in daylight
    rcon.cmd("forceload add -4 -8 16 8");
    rcon.cmd("kill @e[type=zombie]");
    rcon.cmd("kill @e[type=villager]");
    // Clear anything above the surface (surface top face at y=-60).
    rcon.cmd("fill -4 -60 -8 16 -55 8 air");
}

#[test]
#[ignore = "needs the isolated lodestone-entity-oracle Docker server with RCON"]
fn live_zombie_reaches_villager_on_open_ground() {
    let _oracle = oracle_guard();
    let mut rcon = Rcon::connect();
    reset_arena(&mut rcon);

    // Lure at x=10, chaser at x=0, clear line of sight, no obstacles.
    rcon.cmd("summon villager 10 -60 0 {NoAI:1b,PersistenceRequired:1b}");
    rcon.cmd("summon zombie 0 -60 0 {PersistenceRequired:1b,IsBaby:0b}");
    rcon.wait_for_entity("@e[type=villager,limit=1]");
    rcon.wait_for_entity("@e[type=zombie,limit=1]");

    let villager = rcon
        .pos("@e[type=villager,limit=1]")
        .expect("villager pos parses");

    let mut route = Vec::new();
    let mut prev_x = 0.0f64;
    let mut ticks = 0u32;
    let mut reached = false;
    let step = 4u32;
    while ticks < 200 {
        rcon.cmd(&format!("tick sprint {step}"));
        ticks += step;
        // tick sprint returns immediately; give the run a beat to finish.
        std::thread::sleep(Duration::from_millis(60));
        if let Some((x, y, z)) = rcon.pos("@e[type=zombie,limit=1]") {
            route.push((x, y, z));
            prev_x = x;
            let dx = villager.0 - x;
            let dz = villager.2 - z;
            if (dx * dx + dz * dz).sqrt() < 2.0 {
                reached = true;
                break;
            }
        } else {
            break; // zombie gone (shouldn't happen: PersistenceRequired)
        }
    }

    let net_progress = prev_x; // started at x=0
    eprintln!("[open-ground] ticks={ticks} net_x_progress={net_progress:.2} reached={reached}");
    eprintln!("[open-ground] route: {route:?}");
    if let (Some(first), Some(last)) = (route.first(), route.last()) {
        let speed = (last.0 - first.0).abs() / f64::from(ticks.max(1));
        eprintln!("[open-ground] mean ground speed ≈ {speed:.3} blocks/tick");
    }

    assert!(
        net_progress > 4.0,
        "zombie made no meaningful progress toward the villager (x={net_progress:.2}); \
         did it acquire the target? (needs difficulty>=easy and LOS)"
    );
    assert!(
        reached,
        "zombie never reached melee range of the villager within {ticks} ticks (x={net_progress:.2})"
    );

    // Our side: the trivial straight path must also reach the target cell.
    let world = ArenaWorld {
        ground_top: -61,
        walls: HashSet::new(),
    };
    let mob = MobShape::land(0.6, 1.95); // adult zombie
    let path = PathFinder::new(4000)
        .find_path(
            &world,
            &mob,
            PathStart::grounded(0.5, -60.0, 0.5),
            &[BlockPos::new(10, -60, 0)],
            params(),
        )
        .expect("our pathfinder finds the open-ground route");
    let end = path.nodes().last().expect("non-empty path");
    eprintln!(
        "[open-ground] our path: {} nodes, endpoint ({},{},{})",
        path.nodes().len(),
        end.x,
        end.y,
        end.z
    );
    assert!(
        (10 - end.x).abs() <= 1 && end.z.abs() <= 1,
        "our path stops within reach of the villager cell (10,0); a mob paths \
         *beside* its target, got ({},{})",
        end.x,
        end.z
    );
}

#[test]
#[ignore = "needs the isolated lodestone-entity-oracle Docker server with RCON"]
fn live_zombie_detours_around_wall() {
    let _oracle = oracle_guard();
    let mut rcon = Rcon::connect();
    reset_arena(&mut rcon);

    // Fence wall across x=5, columns z=-3..=3. Its 1.5 collision is unjumpable
    // (>1.25 jump, >0.6 step) so the zombie must go around an end (z<=-4 or
    // z>=4); LOS passes over the 1.5 top so the target is still acquired.
    rcon.cmd("fill 5 -60 -3 5 -60 3 oak_fence");
    rcon.cmd("summon villager 10 -60 0 {NoAI:1b,PersistenceRequired:1b}");
    rcon.cmd("summon zombie 0 -60 0 {PersistenceRequired:1b,IsBaby:0b}");
    rcon.wait_for_entity("@e[type=villager,limit=1]");
    rcon.wait_for_entity("@e[type=zombie,limit=1]");

    let villager = rcon
        .pos("@e[type=villager,limit=1]")
        .expect("villager pos parses");

    let mut route = Vec::new();
    let mut ticks = 0u32;
    let mut reached = false;
    let step = 4u32;
    while ticks < 300 {
        rcon.cmd(&format!("tick sprint {step}"));
        ticks += step;
        std::thread::sleep(Duration::from_millis(60));
        if let Some((x, y, z)) = rcon.pos("@e[type=zombie,limit=1]") {
            route.push((x, y, z));
            let dx = villager.0 - x;
            let dz = villager.2 - z;
            if (dx * dx + dz * dz).sqrt() < 2.0 {
                reached = true;
                break;
            }
        } else {
            break;
        }
    }

    // Real-zombie detour metrics.
    let real_max_abs_z = route.iter().map(|p| p.2.abs()).fold(0.0f64, f64::max);
    let real_side = route
        .iter()
        .max_by(|a, b| a.2.abs().partial_cmp(&b.2.abs()).unwrap())
        .map(|p| if p.2 >= 0.0 { "+z" } else { "-z" })
        .unwrap_or("?");
    let final_x = route.last().map(|p| p.0).unwrap_or(0.0);
    eprintln!(
        "[detour] ticks={ticks} reached={reached} final_x={final_x:.2} \
         real_max|z|={real_max_abs_z:.2} real_side={real_side}"
    );
    eprintln!("[detour] route: {route:?}");

    assert!(
        reached,
        "zombie never reached the villager past the wall within {ticks} ticks \
         (final_x={final_x:.2}, max|z|={real_max_abs_z:.2}) — check LOS over the fence"
    );
    assert!(
        real_max_abs_z >= 3.5,
        "zombie did not detour around the wall end (max|z|={real_max_abs_z:.2}); \
         to pass x=5 it must reach a column beyond z=±3"
    );

    // Our side: same arena, same mob, real A*.
    let mut walls = HashSet::new();
    for z in -3..=3 {
        walls.insert((5, -60, z));
    }
    let world = ArenaWorld {
        ground_top: -61,
        walls,
    };
    let mob = MobShape::land(0.6, 1.95);
    let path = PathFinder::new(8000)
        .find_path(
            &world,
            &mob,
            PathStart::grounded(0.5, -60.0, 0.5),
            &[BlockPos::new(10, -60, 0)],
            params(),
        )
        .expect("our pathfinder finds a detour around the wall");
    let nodes = path.nodes();
    let end = nodes.last().expect("non-empty path");
    // Where does our path cross the wall plane x=5?
    let crossing = nodes.iter().find(|n| n.x == 5);
    let our_max_abs_z = nodes.iter().map(|n| n.z.abs()).max().unwrap_or(0);
    let our_side = crossing
        .map(|n| if n.z >= 0 { "+z" } else { "-z" })
        .unwrap_or("?");
    eprintln!(
        "[detour] our path: {} nodes, endpoint ({},{},{}), crosses x=5 at z={:?}, our_max|z|={our_max_abs_z}, our_side={our_side}",
        nodes.len(),
        end.x,
        end.y,
        end.z,
        crossing.map(|n| n.z),
    );

    assert!(
        (10 - end.x).abs() <= 1 && end.z.abs() <= 1,
        "our detour path stops within reach of the villager cell (10,0), got ({},{})",
        end.x,
        end.z
    );
    assert!(
        our_max_abs_z >= 4,
        "our path must route through a column beyond the wall (|z|>=4), got {our_max_abs_z}"
    );

    // Full-stack: the *goal-driven* mob (GoalSelector → MeleeAttackGoal → real
    // A* through the MobController seam) must also detour, not just the bare
    // PathFinder. This is the connectedness check — it proves the same route
    // emerges when the pathfinder is reached through the goal scheduler the way
    // a live mob reaches it, not called directly by the test.
    let mut goal_mob = NavigatingMob::new(
        &world,
        MobShape::land(0.6, 1.95),
        Vec3::new(0.5, -60.0, 0.5),
        0.25,
        8000,
        0,
    );
    goal_mob.set_attack_target(Some(Vec3::new(10.5, -60.0, 0.5)));
    let mut ai = GoalSelector::new();
    ai.add(1, Box::new(MeleeAttackGoal::new(1.0, 2.0)));
    let mut goal_max_abs_z = 0.0f64;
    let mut goal_reached = false;
    for _ in 0..2000 {
        goal_mob.tick(&mut ai);
        let p = goal_mob.position();
        goal_max_abs_z = goal_max_abs_z.max(p.z.abs());
        let dx = 10.5 - p.x;
        let dz = 0.5 - p.z;
        if (dx * dx + dz * dz).sqrt() < 1.5 {
            goal_reached = true;
            break;
        }
        if goal_mob.is_stuck() {
            break;
        }
    }
    eprintln!(
        "[detour] goal-driven mob: reached={goal_reached} searches={} max|z|={goal_max_abs_z:.2}",
        goal_mob.path_searches()
    );
    assert!(
        goal_reached,
        "goal-driven mob never reached the target past the wall (max|z|={goal_max_abs_z:.2})"
    );
    assert!(
        goal_max_abs_z >= 4.0,
        "goal-driven mob must detour the wall end (|z|>=4), got max|z|={goal_max_abs_z:.2}"
    );

    // Divergence report (not asserted — the detour *side* is unseedable entropy).
    eprintln!(
        "[detour] DIVERGENCE: real max|z|={real_max_abs_z:.2} ({real_side}) vs \
         our max|z|={our_max_abs_z} ({our_side}); Δmax|z|={:.2}; side_agreement={}",
        (real_max_abs_z - f64::from(our_max_abs_z)).abs(),
        real_side == our_side,
    );
}

fn params() -> PathParams {
    PathParams {
        max_path_length: 200.0,
        reach_range: 1,
        visited_multiplier: 1.0,
    }
}
