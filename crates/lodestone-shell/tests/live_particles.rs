//! Live regression gate for **server-sent particles reaching the emitter**,
//! against the survival 26.2 oracle (`lodestone-survival`, game :25565, RCON
//! :25566).
//!
//! ## The bug this reproduces
//!
//! `LEVEL_PARTICLES` decoded all the way to `ClientEvent::Particles`
//! (`crates/protocol/v770/src/adapter.rs`) and then went nowhere:
//! `grep -rn "ClientEvent::Particles" crates/lodestone-shell/src/` returned
//! zero hits. `/particle minecraft:flame` over RCON acknowledged on the
//! server and drew nothing client-side; the HUD's `particles=D/A+Unres`
//! counter stayed at `0/0+0` forever, indistinguishable from an idle client
//! that was never sent anything.
//!
//! So this gate does not read the counter at rest — it **causes** particles
//! (RCON `/particle`) and asserts the *caused* frame, the same discipline
//! `live_dig_place.rs` uses for the desync it reproduces.
//!
//! Gated behind `--features live` **and** `#[ignore]`: it **fails** rather
//! than skips when it cannot run (no server, no RCON, missing vanilla
//! assets), because a skip here reads like a pass.
//!
//! ```text
//! cargo test -p lodestone-shell --features live \
//!   --test live_particles -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::config::{Config, Mode};
use lodestone::net::NetClient;
use lodestone::sim::{SessionPhase, Sim};
use lodestone_testsupport::RconClient;

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25565;
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL: i32 = 776;
const ASPECT: f32 = 16.0 / 9.0;

#[test]
#[ignore = "requires the survival 26.2 oracle on :25565 (+ RCON :25566), the vanilla assets under .cache/mc/26.2, and `--features live`"]
fn server_particles_reach_the_emitter() {
    // The vanilla atlas must load or `Sim` takes the demo path, in which case
    // the particle sheets have no UVs and everything would report
    // unresolved regardless of whether the routing works. Fail loud rather
    // than pass vacuously.
    let probe = Sim::new(live_config());
    assert!(
        probe.vanilla_atlas().is_some(),
        "vanilla assets did not load, so Sim would run the demo path instead of the live \
         server world. Banner: {:?}.",
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

    let mut sim = Sim::new(live_config());
    let demo_spawn = sim.player().position;
    sim.attach_net(NetClient::connect(HOST.into(), PORT, PROTOCOL));

    // Drive the real join path until the server has placed us and chunks stream.
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut placed = false;
    while Instant::now() < deadline {
        pump(&mut sim);
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
    for _ in 0..80 {
        pump(&mut sim);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        sim.session_phase(),
        &SessionPhase::Connected,
        "expected a live Connected session before asserting on server-sent particles"
    );

    // Baseline: an idle client (nothing sent yet) must report the exact
    // 0/0+0 counter the briefing warns is indistinguishable from "the route
    // is missing" — recorded so the *caused* reading below has something to
    // contrast against, not asserted as a pass/fail condition on its own.
    let cam = sim.camera(ASPECT);
    let idle = sim.extract_particles(&cam);
    eprintln!(
        "[baseline · idle] particles={}/{}+{}unres",
        idle.drawn, idle.alive, idle.unresolved
    );

    // Cause particles: three of the exact types the briefing drove over RCON
    // and observed doing nothing (`flame`, `smoke`, `crit`), at the player's
    // current position so they land well inside the render cutoff.
    let px = sim.player().position.x;
    let py = sim.player().position.y + 1.0;
    let pz = sim.player().position.z;
    for kind in ["minecraft:flame", "minecraft:smoke", "minecraft:crit"] {
        let reply = rcon.cmd(&format!(
            "particle {kind} {px} {py} {pz} 0.2 0.2 0.2 0.01 20 force"
        ));
        eprintln!("[rcon] /particle {kind} -> {reply:?}");
    }

    // Poll for a few seconds — the burst is client-side-immediate once the
    // packet arrives, but give the network round trip room.
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut caused = None;
    while Instant::now() < deadline {
        pump(&mut sim);
        let cam = sim.camera(ASPECT);
        let frame = sim.extract_particles(&cam);
        if frame.alive > 0 {
            caused = Some(frame);
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    let frame = caused.unwrap_or_else(|| {
        panic!(
            "no particles appeared within 10s of the RCON /particle commands \
             (idle baseline was {}/{}+{}unres) — the ClientEvent::Particles route is \
             not reaching the emitter.",
            idle.drawn, idle.alive, idle.unresolved
        )
    });
    eprintln!(
        "[caused · after /particle flame+smoke+crit] particles={}/{}+{}unres",
        frame.drawn, frame.alive, frame.unresolved
    );
    assert!(
        frame.alive > 0,
        "expected live particles after the RCON burst, got alive=0"
    );
    assert!(
        frame.drawn > 0,
        "particles are alive but none drew — sprite resolution is broken even though routing \
         works (alive={}, unresolved={})",
        frame.alive,
        frame.unresolved
    );
}

/// Step the sim one tick and drain its frame outputs, the way the app loop does.
fn pump(sim: &mut Sim) {
    sim.step(1.0 / 20.0);
    let _ = sim.drain_meshes();
    let _ = sim.drain_removals();
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
