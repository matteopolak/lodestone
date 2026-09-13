//! Live regression gate: holding "forward" against a real vanilla survival
//! server, through the shell's own `Sim`, must not draw a corrective
//! `TeleportPlayer` — the rubber-band the repo owner reported ("it seems like
//! physics triggered some kind of anticheat as i couldnt move (it would
//! rubberband me back)").
//!
//! ## Why this gate, and not the existing ones
//!
//! `live_stands_on_server_ground.rs` proves the player *settles* on the
//! server's ground and can *jump* without a net displacement, and
//! `crates/lodestone-client/tests/live_physics_bot.rs` proves a bot can walk
//! through the public `ClientHandle` API on a flat, verified-clean lane with
//! zero corrections. Neither drives **sustained horizontal walking through the
//! real `Sim`** (the code path the shipped game actually runs — real ECS
//! schedule, real `InputState`, real ground the survival world happens to have
//! underfoot, bumps and all). A physics disagreement that only shows up once
//! the player leaves a synthetic flat plane, or only through the shell's own
//! `TickSet::Physics` → `TickSet::Send` wiring, is exactly what those two gates
//! cannot see.
//!
//! ## The discriminating claim
//!
//! Walking is not "we send movement packets" (true even under the bug: a
//! client that never adopts a correction still sends *something* every tick).
//! It is: **after the server places us, holding forward for several seconds
//! draws zero additional `TeleportPlayer` corrections**, and if the walk ever
//! is corrected, the very next packets are computed from the corrected
//! position (`Sim`'s own `player.position`, which `net_apply.rs`'s
//! `NetUpdate::Teleport` arm snaps onto the server's authoritative pose) —
//! never from a stale, pre-correction position. `teleport_count` bounded across
//! the walk is the falsifiable half of that claim: a client that ignores or
//! mis-applies a correction re-diverges immediately and keeps drawing new
//! ones, so the count would climb instead of holding flat.
//!
//! Gated behind `--features live` **and** `#[ignore]`.
//!
//! ```text
//! cargo test -p lodestone-shell --features live \
//!   --test live_walk_on_server_ground -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::config::{Config, Mode};
use lodestone::sim::Sim;
use lodestone_controller::Action;
use lodestone_testsupport::{RconClient, unique_username};

const HOST: &str = "127.0.0.1";
/// The survival 26.2 oracle: game on `:25565`, RCON on `:25566`.
const PORT: u16 = 25565;
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL: i32 = 776;

/// Same plains spot `live_stands_on_server_ground.rs` uses — already confirmed
/// walkable ground at y~69-70, deterministic across runs via `setworldspawn`.
const SPAWN_X: i32 = -45;
const SPAWN_Y: i32 = 72;
const SPAWN_Z: i32 = -377;

/// Real terrain, not a flat lane: enough ticks to cross several blocks in each
/// of four directions, so a bump, a slab, or a one-block step (any of which
/// could expose an edge-back-off or collision-resolution disagreement a flat
/// plane cannot) has a real chance of being walked over.
const WALK_TICKS_PER_LEG: usize = 60;

#[test]
#[ignore = "requires the survival 26.2 oracle on :25565 (+ RCON :25566), the vanilla assets under .cache/mc/26.2, and `--features live`"]
fn walking_on_real_terrain_draws_no_corrective_teleport() {
    let probe = Sim::new(live_config());
    assert!(
        probe.vanilla_atlas().is_some(),
        "vanilla assets did not load, so Sim would run the demo path instead of the \
         live server world. Banner: {:?}. Fix: put a vanilla pack at .cache/mc/26.2 \
         (client.jar + generated/reports/blocks.json) or set LODESTONE_ASSETS.",
        probe.asset_banner()
    );
    drop(probe);

    let mut rcon = RconClient::connect(RCON_ADDR, RCON_PASSWORD).unwrap_or_else(|e| {
        panic!(
            "cannot reach RCON at {RCON_ADDR}: {e}. Fix: start the survival 26.2 oracle \
             (game :25565, RCON :25566) with `./scripts/live-oracles/survival.sh` and run \
             with `--features live`."
        )
    });
    let reply = rcon.cmd(&format!("setworldspawn {SPAWN_X} {SPAWN_Y} {SPAWN_Z}"));
    assert!(
        reply.to_lowercase().contains("set the world spawn"),
        "RCON setworldspawn did not take: {reply:?}"
    );

    let mut sim = Sim::new(live_config());
    sim.connect_as(HOST.into(), PORT, PROTOCOL, unique_username());

    // Phase 1: drive until the server has placed us and chunks are streaming.
    let demo_spawn = sim.player().position;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut placed = false;
    while Instant::now() < deadline {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        if let Some(net) = sim.net()
            && net.world_dimensions().is_some()
            && !net.loaded_chunks().is_empty()
            && sim.player().position != demo_spawn
        {
            placed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(
        placed,
        "server never placed the player within 60s (still at demo spawn {demo_spawn:?}). \
         Fix: start the survival 26.2 oracle on :25565 and run with `--features live`."
    );

    // Let the placement teleport + first chunk burst settle before walking.
    std::thread::sleep(Duration::from_millis(500));
    for _ in 0..10 {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        std::thread::sleep(Duration::from_millis(20));
    }

    let start = sim.player().position;
    let teleports_before_walk = sim.teleport_count;
    eprintln!(
        "settled at ({:.3},{:.3},{:.3}), on_ground={}, teleport_count={}",
        start.x,
        start.y,
        start.z,
        sim.player().on_ground,
        teleports_before_walk
    );

    // Phase 2: walk. Four legs (forward, right, back, left) so a wall in one
    // direction does not stall the whole gate — each leg is independently a
    // real, sustained walk against real terrain.
    let legs = [Action::Forward, Action::Right, Action::Back, Action::Left];
    let mut positions = vec![start];
    for leg in legs {
        sim.input_mut(|i| i.set(leg, true));
        for _ in 0..WALK_TICKS_PER_LEG {
            sim.step(1.0 / 20.0);
            let _ = sim.drain_meshes();
            let _ = sim.drain_removals();
            std::thread::sleep(Duration::from_millis(20));
        }
        sim.input_mut(|i| i.set(leg, false));
        // A couple of settle ticks between legs so a direction change is not
        // itself read as part of the next leg's displacement measurement.
        for _ in 0..5 {
            sim.step(1.0 / 20.0);
            let _ = sim.drain_meshes();
            let _ = sim.drain_removals();
            std::thread::sleep(Duration::from_millis(20));
        }
        let p = sim.player().position;
        eprintln!(
            "after {leg:?}: ({:.3},{:.3},{:.3}), on_ground={}, teleport_count={}",
            p.x,
            p.y,
            p.z,
            sim.player().on_ground,
            sim.teleport_count
        );
        positions.push(p);
    }

    let teleports_after_walk = sim.teleport_count;
    let end = sim.player().position;
    let total_horizontal: f64 = positions
        .windows(2)
        .map(|w| {
            let dx = w[1].x - w[0].x;
            let dz = w[1].z - w[0].z;
            (dx * dx + dz * dz).sqrt()
        })
        .sum();

    eprintln!(
        "=== LIVE SIM WALK GATE REPORT ===\n\
         start                : ({:.3},{:.3},{:.3})\n\
         end                  : ({:.3},{:.3},{:.3})\n\
         total horizontal path: {total_horizontal:.3} blocks\n\
         teleports before/after walk: {teleports_before_walk}/{teleports_after_walk}",
        start.x, start.y, start.z, end.x, end.y, end.z,
    );

    // The discriminating assertion: the server issued zero corrective
    // teleports while we held movement keys. A client that fails to adopt (or
    // mis-adopts) a correction would instead show this counter climbing —
    // every subsequent packet re-diverges from the corrected truth and draws
    // another correction, the rubber-band the repo owner reported.
    assert_eq!(
        teleports_after_walk, teleports_before_walk,
        "server issued {} corrective teleport(s) while walking on real survival \
         terrain through the shell's own Sim — physics diverged from vanilla \
         (or a prior correction was not adopted, so every following packet kept \
         re-diverging). Positions observed per leg: {positions:?}",
        teleports_after_walk - teleports_before_walk
    );
    // A sanity floor: some of the four legs must have actually moved us,
    // or this gate would vacuously pass with a client that never sends
    // movement at all (the same class of vacuous-test failure `CLAUDE.md`
    // warns about for "we send movement packets" alone).
    assert!(
        total_horizontal > 1.0,
        "walked only {total_horizontal:.3} blocks total across four directions — \
         movement did not really leave the client, so the zero-corrections result \
         above is not trustworthy. (Real terrain can legitimately block one or two \
         legs; all four failing to move is not terrain, it is the client not \
         walking.)"
    );
}

fn live_config() -> Config {
    Config {
        mode: Mode::Window,
        host: HOST.into(),
        port: PORT,
        protocol: PROTOCOL,
        connect_in_window: true,
        render_distance: 8,
        ..Config::default()
    }
}
