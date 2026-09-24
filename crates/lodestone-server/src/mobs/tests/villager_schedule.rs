use super::*;

/// A flat, walkable floor wide enough that pathfinding across it never
/// runs off the edge — the same shape `a_grazing_mob_hands_its_eat_to_the_driver`
/// already establishes for a real multi-tick walk.
fn flat_world() -> ChunkWorld {
    let mut world = ChunkWorld::new(-64, 384);
    for x in -32..=32 {
        for z in -32..=32 {
            world.set_block(x, -1, z, "minecraft:grass_block");
        }
    }
    world
}

fn spawn_villager(sim: &mut MobSim<'_>, pos: Vec3) -> i32 {
    sim.spawn_species("minecraft:villager".parse().expect("valid key"), pos).id()
}

fn horizontal_distance(a: Vec3, b: Vec3) -> f64 {
    (a.x - b.x).hypot(a.z - b.z)
}

/// **The headline case.** A villager spawns 20 blocks from a composter
/// (a real `farmer` job site), stays idle at a day time before `WORK`
/// starts (`2000`, `VILLAGER_SCHEDULE`'s own keyframe), then the clock
/// enters the `WORK` window and the villager visibly closes most of the
/// distance to its claimed workstation over real ticks — not merely
/// "the position changed", a magnitude check against the starting gap,
/// so a villager that only ever random-strolls (and might coincidentally
/// drift a block or two toward the composter) cannot pass this by luck.
#[test]
fn a_villager_walks_to_its_claimed_workstation_once_work_begins() {
    let mut world = flat_world();
    // Inside `villager::SEARCH_RADIUS` (16 blocks) so the villager's
    // bounded job search can actually find it, and far enough past
    // `WalkToPoi`'s own 9-block close-enough radius that "arrived" and
    // "started here" are unambiguously different distances.
    let composter = BlockPos::new(15, 0, 0);
    world.set_block(composter.x, composter.y, composter.z, "minecraft:composter");

    let mut sim = MobSim::new(&world);
    let id = spawn_villager(&mut sim, Vec3::new(0.5, 0.0, 0.5));

    // Before `WORK` (schedule keyframe `2000`): let the villager claim
    // the workstation (job search is unthrottled on its first tick) but
    // never let the clock enter `WORK`, so any position drift here is
    // attributable only to `IDLE`'s own random stroll, not to this
    // schedule.
    for _ in 0..5 {
        sim.set_day_time(500);
        sim.tick();
    }
    assert_eq!(
        sim.get(id).expect("just spawned").workstation(),
        Some(composter),
        "the villager must have claimed the only nearby composter before WORK ever starts"
    );

    let workstation_center = Vec3::new(
        f64::from(composter.x) + 0.5,
        f64::from(composter.y) + 0.5,
        f64::from(composter.z) + 0.5,
    );
    let initial_distance = horizontal_distance(sim.get(id).expect("spawned").position(), workstation_center);

    // Now enter the WORK window and let the schedule + WalkToPoi close
    // the gap over real ticks.
    for _ in 0..400 {
        sim.set_day_time(3000);
        sim.tick();
    }
    let final_distance = horizontal_distance(sim.get(id).expect("spawned").position(), workstation_center);

    // Two predictions, not just "it got closer": `WalkToPoi::new(JOB_SITE,
    // …, 9)` stops issuing a fresh walk target once within 9 blocks
    // (`MoveToTargetSink::reached`'s own `+ 0.5` tolerance), so a working
    // villager should end up **near that exact radius**, not merely
    // "somewhat closer" — which a lucky IDLE stroll could also produce.
    assert!(
        final_distance <= 10.5,
        "a villager working its claimed job site should stop within WalkToPoi's own \
         9-block close-enough radius (plus MoveToTargetSink's 0.5 tolerance): \
         started {initial_distance:.1} blocks away, ended {final_distance:.1}"
    );
    assert!(
        initial_distance - final_distance > 4.0,
        "WORK must walk the villager measurably closer to its claimed workstation: \
         started {initial_distance:.1} blocks away, ended {final_distance:.1}"
    );
}

/// [`a_villager_walks_to_its_claimed_workstation_once_work_begins`]'s own
/// sibling for `MEET`/bells rather than `WORK`/workstations — proving the
/// third POI (the one this session's own `BellClaims` adds) reaches the
/// identical real chain: claim -> schedule -> `WalkToPoi` -> a real
/// position change.
#[test]
fn a_villager_walks_to_its_claimed_bell_once_meet_begins() {
    let mut world = flat_world();
    let bell = BlockPos::new(15, 0, 0);
    world.set_block(bell.x, bell.y, bell.z, "minecraft:bell[attachment=floor,facing=south]");

    let mut sim = MobSim::new(&world);
    let id = spawn_villager(&mut sim, Vec3::new(0.5, 0.0, 0.5));

    // Before `MEET` (schedule keyframe `9000`): let the villager claim
    // the bell but keep the clock in `IDLE`'s own window.
    for _ in 0..5 {
        sim.set_day_time(500);
        sim.tick();
    }
    assert_eq!(
        sim.get(id).expect("just spawned").meeting_point(),
        Some(bell),
        "the villager must have claimed the only nearby bell before MEET ever starts"
    );

    let bell_center = Vec3::new(f64::from(bell.x) + 0.5, f64::from(bell.y) + 0.5, f64::from(bell.z) + 0.5);
    let initial_distance = horizontal_distance(sim.get(id).expect("spawned").position(), bell_center);

    for _ in 0..400 {
        sim.set_day_time(9500);
        sim.tick();
    }
    let final_distance = horizontal_distance(sim.get(id).expect("spawned").position(), bell_center);

    // `WalkToPoi::new(MEETING_POINT, …, 6)` — a tighter close-enough
    // radius than the job site's `9`, which is the meeting-point walk
    // target's range.
    assert!(
        final_distance <= 7.5,
        "a villager meeting at its claimed bell should stop within WalkToPoi's own \
         6-block close-enough radius (plus MoveToTargetSink's 0.5 tolerance): \
         started {initial_distance:.1} blocks away, ended {final_distance:.1}"
    );
    assert!(
        initial_distance - final_distance > 4.0,
        "MEET must walk the villager measurably closer to its claimed bell: \
         started {initial_distance:.1} blocks away, ended {final_distance:.1}"
    );
}

/// The schedule's own negative control: a villager with **no** claimed
/// job site (nothing nearby to claim) never becomes `WORK`-eligible —
/// the generic villager activity requirement that a job-site memory be
/// present — so it stays wherever `IDLE`'s random stroll leaves
/// it: never *reliably* walking toward a fixed faraway point regardless
/// of the clock. Asserted as "never claims a workstation", the
/// discriminating fact this control actually has available deterministically
/// (a stroll's own endpoint is randomised and not itself a safe assertion).
#[test]
fn a_villager_with_no_nearby_job_site_never_claims_one_regardless_of_the_clock() {
    let world = flat_world();
    let mut sim = MobSim::new(&world);
    let id = spawn_villager(&mut sim, Vec3::new(0.5, 0.0, 0.5));

    for _ in 0..200 {
        sim.set_day_time(3000);
        sim.tick();
    }

    assert_eq!(
        sim.get(id).expect("spawned").workstation(),
        None,
        "with no workstation block anywhere nearby there is nothing to claim, \
         so WORK can never become eligible no matter how long the clock sits in its window"
    );
}
