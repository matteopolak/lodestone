//! Live regression gate for the shield/bow "island pair" fix, against the
//! survival 26.2 oracle (`lodestone-survival`, game :25565, RCON :25566).
//!
//! ## The bug this reproduces
//!
//! Two independent, zero-ambiguity defects made the shield and the bow
//! functionally dead in combat (see `docs/combat.md`'s "The shield/bow island
//! pair" section for the full jar-sourced trace):
//!
//! 1. `ClientAction::ReleaseUseItem` was encoded by all four protocol
//!    adapters with **zero producers** anywhere in `lodestone-shell` — no
//!    input arm ever sent it, on mouse or keyboard.
//! 2. `Sim::use_item_live` returned without sending anything whenever the
//!    crosshair was over *any* entity (the common combat case: aiming a bow
//!    at a mob) or over nothing at all, instead of falling through to the
//!    generic use-item send vanilla's own `Minecraft.startUseItem` reaches.
//!
//! This gate drives the real production path end to end against a real
//! server: aim a drawn bow at a live (summoned) entity, hold, release, and
//! assert an arrow actually leaves the bow — the one observation neither
//! defect's hermetic unit test can make, because both are about *reaching
//! the server*, and the server is the only authority on whether a bow fired.
//!
//! Gated behind `--features live` **and** `#[ignore]`: it **fails** rather
//! than skips when it cannot run, because a skip here reads like a pass.
//!
//! ```text
//! cargo test -p lodestone-shell --features live \
//!   --test live_use_item_release -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::config::{Config, Mode};
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
fn a_bow_drawn_at_an_entity_and_released_fires_an_arrow() {
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
    sim.connect(HOST.into(), PORT, PROTOCOL);

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
        SessionPhase::Connected,
        "expected a live Connected session before interacting"
    );

    // Op every online player (bypasses spawn protection near world spawn, the
    // same trap `live_dig_place.rs` documents) and pin full health so a
    // wandering mob cannot end the session mid-gate.
    for name in online_players(&mut rcon) {
        rcon.cmd(&format!("op {name}"));
        rcon.cmd(&format!("gamemode survival {name}"));
        rcon.cmd(&format!("effect give {name} minecraft:resistance 999999 255 true"));
        rcon.cmd(&format!("effect give {name} minecraft:regeneration 999999 9 true"));
    }
    for _ in 0..20 {
        pump(&mut sim);
        std::thread::sleep(Duration::from_millis(20));
    }

    let px = sim.player().position.x.floor() as i32;
    let py = sim.player().position.y.floor() as i32;
    let pz = sim.player().position.z.floor() as i32;
    rcon.cmd(&format!(
        "forceload add {} {} {} {}",
        px - 6,
        pz - 6,
        px + 6,
        pz + 6
    ));

    // Give the bow, seeded directly (the selected-slot default is unreliable
    // across joins, the same reasoning `live_dig_place.rs` uses for its
    // placement invariant).
    rcon.cmd("item replace entity @a weapon.mainhand with minecraft:bow");
    for _ in 0..10 {
        pump(&mut sim);
        std::thread::sleep(Duration::from_millis(20));
    }

    // Clear a small air pocket two blocks in front of and one above the
    // player so the entity ray is not blocked by terrain, then summon a
    // stationary (NoAI, not Invulnerable — an invulnerable entity is also
    // untargetable, CLAUDE.md's own caution) pig inside it, well within
    // `ENTITY_REACH` (3.0 blocks).
    let target_level = py + 1;
    let target = [px, target_level, pz + 2];
    for dy in 0..=1 {
        rcon.cmd(&format!(
            "setblock {} {} {} minecraft:air",
            target[0],
            target[1] + dy,
            target[2]
        ));
    }
    rcon.cmd(&format!(
        "summon minecraft:pig {} {} {} {{NoAI:1b,Silent:1b,PersistenceRequired:1b}}",
        target[0], target[1], target[2]
    ));
    let pig_selector = format!(
        "@e[type=minecraft:pig,x={},y={},z={},distance=..1,limit=1]",
        target[0], target[1], target[2]
    );
    rcon.wait_for_entity(&pig_selector, Duration::from_secs(10), Duration::from_millis(100))
        .expect("summoned pig must become selector-visible (server-side)");

    // Confirm no arrow exists yet, before this gate could create one.
    assert!(
        !arrow_present_near(&mut rcon, [px, py, pz], 12),
        "an arrow already exists near the player before the gate ran — test area not clean"
    );

    // Aim at the pig and poll until the client's own entity ray resolves it
    // — a freshly summoned entity is not immediately visible to *our*
    // client either (it has to stream in over the connection, a second,
    // independent latency from the RCON-side visibility already waited for
    // above).
    let pig_point = [target[0] as f64 + 0.5, target[1] as f64 + 0.3, target[2] as f64 + 0.5];
    let entity_deadline = Instant::now() + Duration::from_secs(20);
    let mut targeted = false;
    while Instant::now() < entity_deadline {
        aim_at(&mut sim, pig_point);
        for _ in 0..3 {
            pump(&mut sim);
            sim.update_target(ASPECT);
            std::thread::sleep(Duration::from_millis(20));
        }
        if sim.entity_target().is_some() {
            targeted = true;
            break;
        }
    }
    assert!(
        targeted,
        "the client never targeted the summoned pig via its own entity ray — cannot exercise \
         the entity-target fallthrough this gate is for"
    );

    // ---- THE FIX UNDER TEST -------------------------------------------
    // Press: `interact_entity` (PASS on a plain pig — it has no special
    // right-click behaviour) must fall through to the generic use-item send
    // that starts the bow draw (Finding 2). Hold for a real charge, then
    // release: `Sim::end_use` must send `ReleaseUseItem` (Finding 1), which
    // is what lets the server's own `releaseUsingItem`/bow logic fire the
    // arrow.
    sim.use_item();
    for _ in 0..24 {
        pump(&mut sim);
        std::thread::sleep(Duration::from_millis(20));
    }
    sim.end_use();

    let fired = {
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut ok = false;
        while Instant::now() < deadline {
            pump(&mut sim);
            if arrow_present_near(&mut rcon, [px, py, pz], 12) {
                ok = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        ok
    };
    eprintln!("[invariant · bow release] arrow present near player: {fired}");
    assert!(
        fired,
        "drawing and releasing a bow aimed at a live entity did not spawn a server-side arrow \
         — the release-input arm or the use_item_live fallthrough is not reaching the server"
    );

    rcon.cmd(&format!("kill {pig_selector}"));
    rcon.cmd("kill @e[type=minecraft:arrow]");
    rcon.cmd(&format!(
        "forceload remove {} {} {} {}",
        px - 6,
        pz - 6,
        px + 6,
        pz + 6
    ));
}

/// Step the sim one tick and drain its frame outputs, the way the app loop
/// does.
fn pump(sim: &mut Sim) {
    sim.step(1.0 / 20.0);
    let _ = sim.drain_meshes();
    let _ = sim.drain_removals();
}

/// Set the player's yaw/pitch to look from the eye toward a world point,
/// using vanilla's forward-vector convention (yaw 0 = south/+Z, pitch 90 =
/// down) — identical to `live_dig_place.rs`'s helper of the same name.
fn aim_at(sim: &mut Sim, point: [f64; 3]) {
    let eye = [
        sim.player().position.x,
        sim.player().position.y + 1.62,
        sim.player().position.z,
    ];
    let dx = point[0] - eye[0];
    let dy = point[1] - eye[1];
    let dz = point[2] - eye[2];
    let len = (dx * dx + dy * dy + dz * dz).sqrt().max(1e-6);
    sim.player_mut(|p| {
        p.yaw = (-dx).atan2(dz).to_degrees() as f32;
        p.pitch = (-dy / len).asin().to_degrees() as f32;
    });
}

/// Whether any `minecraft:arrow` entity exists within `radius` blocks of
/// `pos` — `execute positioned … if entity …` with no `run`, so the server's
/// own bare pass/fail response (`"Test passed"`/`"Test failed"`) is the
/// oracle, the same idiom `live_dig_place.rs`'s `is_block` uses for blocks.
fn arrow_present_near(rcon: &mut RconClient, pos: [i32; 3], radius: i32) -> bool {
    let resp = rcon.cmd(&format!(
        "execute positioned {} {} {} if entity @e[type=minecraft:arrow,distance=..{radius}]",
        pos[0], pos[1], pos[2]
    ));
    resp.contains("Test passed")
}

/// The players the server currently reports online, parsed from `/list`.
fn online_players(rcon: &mut RconClient) -> Vec<String> {
    let reply = rcon.cmd("list");
    match reply.split_once(':') {
        Some((_, names)) => names
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        None => Vec::new(),
    }
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
