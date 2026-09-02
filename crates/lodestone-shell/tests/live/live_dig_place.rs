//! Live regression gate for **dig and place reaching the server**, against the
//! survival 26.2 oracle (`lodestone-survival`, game :25565, RCON :25566).
//!
//! ## The bug this reproduces
//!
//! The shell's `break_block`/`place_block` were demo-world code: they edited the
//! shell's *offline* world and sent **nothing** to the server. Wired to the mouse
//! on the live path, a left-click deleted a block only in our copy (the server
//! restored it on the next chunk update) and a right-click placed a hardcoded
//! stone that did not exist server-side. This gate asserts **server-side truth
//! over RCON** — not our optimistic client world, which is exactly the thing that
//! was lying.
//!
//! Two prerequisites made the desync invisible even after routing the packets:
//!   1. The v26-2 adapter applied server `block_update`/`section_blocks_update` to
//!      the client world but emitted **no re-mesh event**, so a server-confirmed
//!      break/place was applied-but-never-drawn (the "chunk only renders when I
//!      break something" symptom). Fixed alongside this gate.
//!   2. Live targeting raycast the *demo* world; it now targets the server world.
//!
//! ## Structure — negative control first, then the invariant
//!
//! For both operations the **negative control** drives the old demo path
//! (`place_block` / `break_block`) and asserts the server is **unchanged** — the
//! cleanest possible demonstration of the desync. The **invariant** drives the
//! new server-routed path (`use_item` / held `begin_attack`) and asserts the
//! server world actually changed.
//!
//! Gated behind `--features live` **and** `#[ignore]`: it **fails** rather than
//! skips when it cannot run (no server, no RCON, missing vanilla assets), because
//! a skip here reads like a pass.
//!
//! ```text
//! cargo test -p lodestone-shell --features live \
//!   --test live_dig_place -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::config::{Config, Mode};
use lodestone::sim::{SessionPhase, Sim};
use lodestone_testsupport::{RconClient, unique_username};

const HOST: &str = "127.0.0.1";
const PORT: u16 = 25565;
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL: i32 = 776;
const ASPECT: f32 = 16.0 / 9.0;

/// Plains land spawn, the same walkable spawn the collision gate uses.
const SPAWN_X: i32 = -45;
const SPAWN_Y: i32 = 72;
const SPAWN_Z: i32 = -377;

#[test]
#[ignore = "requires the survival 26.2 oracle on :25565 (+ RCON :25566), the vanilla assets under .cache/mc/26.2, and `--features live`"]
fn dig_and_place_reach_the_server() {
    // The vanilla atlas must load or `Sim` takes the demo path and never reaches
    // a live server. Fail loud rather than pass vacuously.
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
    let reply = rcon.cmd(&format!("setworldspawn {SPAWN_X} {SPAWN_Y} {SPAWN_Z}"));
    assert!(
        reply.to_lowercase().contains("set the world spawn"),
        "RCON setworldspawn did not take: {reply:?}"
    );

    let mut sim = Sim::new(live_config());
    let demo_spawn = sim.player().position;
    // §4.1(c): `Sim::connect` threads the shell\'s one `World` into the
    // client, so the session fold lands where the HUD accessors read.
    // `connect_as`, not `connect`: a live gate needs a fresh identity per run
    // (a shared offline name is a shared player file, and a dead player is held
    // on the death screen, which sends no chunks). `connect` is the *stable*
    // persisted offline identity, which is production's job, not a gate's.
    sim.connect_as(HOST.into(), PORT, PROTOCOL, unique_username());

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
    // Settle so we are alive and grounded, and past the server's ~3s client-load
    // gate that drops player actions before `hasClientLoaded()`.
    for _ in 0..80 {
        pump(&mut sim);
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(
        sim.session_phase(),
        SessionPhase::Connected,
        "expected a live Connected session before interacting"
    );

    // Bypass spawn protection: our client spawns within ~10 blocks of world
    // spawn (-45,72,-377), inside the 16-block protection radius, so a non-op
    // player's placements and breaks are silently refused by the server (which
    // is exactly what masked the fix on the first pass). Op every online player
    // — ours is among them — and pin it survival + damage-proof so a wandering
    // spider can't kill it out from under the gate.
    for name in online_players(&mut rcon) {
        rcon.cmd(&format!("op {name}"));
        rcon.cmd(&format!("gamemode survival {name}"));
        for eff in [
            "minecraft:resistance 999999 255 true",
            "minecraft:regeneration 999999 9 true",
            "minecraft:fire_resistance 999999 0 true",
        ] {
            rcon.cmd(&format!("effect give {name} {eff}"));
        }
    }
    for _ in 0..20 {
        pump(&mut sim);
        std::thread::sleep(Duration::from_millis(20));
    }

    let px = sim.player().position.x.floor() as i32;
    let py = sim.player().position.y.floor() as i32;
    let pz = sim.player().position.z.floor() as i32;
    // Keep the working area active so RCON `execute if block` sees live state.
    rcon.cmd(&format!(
        "forceload add {} {} {} {}",
        px - 4,
        pz - 4,
        px + 6,
        pz + 6
    ));

    // ---- PLACE (player stationary first, before any dig moves them) -----------
    // Build a deterministic eye-level wall to click on, with an air corridor in
    // front so the ray reaches its west face. The place cell C sits between the
    // player and the wall.
    let level = py + 1;
    let wall = [px + 3, level, pz];
    let place_cell = [px + 2, level, pz];
    rcon.cmd(&format!(
        "setblock {} {} {} minecraft:air",
        px + 1,
        level,
        pz
    ));
    rcon.cmd(&format!(
        "setblock {} {} {} minecraft:air",
        place_cell[0], place_cell[1], place_cell[2]
    ));
    rcon.cmd(&format!(
        "setblock {} {} {} minecraft:stone",
        wall[0], wall[1], wall[2]
    ));
    // Aim at the wall's west-face centre and confirm we actually target it, so a
    // camera-convention drift fails loudly instead of digging the wrong block.
    aim_at(
        &mut sim,
        [wall[0] as f64, wall[1] as f64 + 0.5, wall[2] as f64 + 0.5],
    );
    settle_target(&mut sim);
    let hit = sim
        .target()
        .expect("the wall block should be targeted after aiming at it");
    assert_eq!(
        hit.block, wall,
        "aimed at the wall but targeted {:?} (expected {wall:?}) — camera/raycast drift",
        hit.block
    );
    assert_eq!(
        hit.place_position(),
        place_cell,
        "the targeted face does not place into the prepared air cell"
    );

    // Negative control: the old demo place edits our copy only and sends nothing.
    assert!(
        is_block(&mut rcon, place_cell, "minecraft:air"),
        "place cell should start as air"
    );
    sim.place_block();
    for _ in 0..40 {
        pump(&mut sim);
        std::thread::sleep(Duration::from_millis(20));
    }
    let server_unchanged = is_block(&mut rcon, place_cell, "minecraft:air");
    eprintln!(
        "[negative control · demo place] server still air at {place_cell:?}: {server_unchanged}"
    );
    assert!(
        server_unchanged,
        "the demo place path changed the server world at {place_cell:?} — it must be local-only \
         (this negative control demonstrates the desync)."
    );

    // Invariant: routed placement puts a real block on the server.
    // The server places whatever is in the player's mainhand, so seed it
    // directly (the selected-slot default is unreliable across joins).
    rcon.cmd("item replace entity @a weapon.mainhand with minecraft:stone 64");
    for _ in 0..10 {
        pump(&mut sim);
        std::thread::sleep(Duration::from_millis(20));
    }
    // Retry through the server's ~60-tick client-load gate, which silently drops
    // `use_item_on` until `hasClientLoaded()`. Re-aim and re-send each attempt.
    let placed_ok = {
        let deadline = Instant::now() + Duration::from_secs(30);
        let mut ok = false;
        while Instant::now() < deadline {
            aim_at(
                &mut sim,
                [wall[0] as f64, wall[1] as f64 + 0.5, wall[2] as f64 + 0.5],
            );
            settle_target(&mut sim);
            sim.use_item();
            for _ in 0..8 {
                pump(&mut sim);
                std::thread::sleep(Duration::from_millis(25));
            }
            if is_block(&mut rcon, place_cell, "minecraft:stone") {
                ok = true;
                break;
            }
        }
        ok
    };
    eprintln!("[invariant · routed place] server has stone at {place_cell:?}: {placed_ok}");
    assert!(
        placed_ok,
        "routed placement did not reach the server: {place_cell:?} is not stone server-side. \
         The shell must send `use_item_on` and the server must place the selected slot."
    );

    // ---- DIG (eye-level side block in a cleared pocket, the proven geometry) ---
    // Mine a reachable eye-level block two east, in a cleared air pocket, rather
    // than the block underfoot: the player never stands on the target, the ray
    // is horizontal (no risk of clipping the floor), and dirt breaks by hand in
    // well under a second so the invariant does not hinge on stone's multi-second
    // bare-hand timing.
    let dig_level = py + 1;
    let dig_block = [px + 2, dig_level, pz];
    // Clear the block between the eye and the target so the ray reaches it, then
    // set the target to dirt (overwriting whatever the place step left there).
    rcon.cmd(&format!(
        "setblock {} {} {} minecraft:air",
        px + 1,
        dig_level,
        pz
    ));
    rcon.cmd(&format!(
        "setblock {} {} {} minecraft:dirt",
        dig_block[0], dig_block[1], dig_block[2]
    ));
    aim_at(
        &mut sim,
        [
            dig_block[0] as f64,
            dig_block[1] as f64 + 0.5,
            dig_block[2] as f64 + 0.5,
        ],
    );
    settle_target(&mut sim);
    let dig_hit = sim
        .target()
        .expect("the dig block should be targeted after aiming at it");
    assert_eq!(
        dig_hit.block, dig_block,
        "aimed at the dig block but targeted {:?} (expected {dig_block:?})",
        dig_hit.block
    );

    // Negative control: the old demo break edits our copy only.
    assert!(
        is_block(&mut rcon, dig_block, "minecraft:dirt"),
        "dig block should be dirt before the control break"
    );
    sim.break_block();
    for _ in 0..40 {
        pump(&mut sim);
        std::thread::sleep(Duration::from_millis(20));
    }
    let dig_unchanged = is_block(&mut rcon, dig_block, "minecraft:dirt");
    eprintln!(
        "[negative control · demo break] server still dirt at {dig_block:?}: {dig_unchanged}"
    );
    assert!(
        dig_unchanged,
        "the demo break path changed the server world at {dig_block:?} — it must be local-only \
         (this negative control demonstrates the desync)."
    );

    // Invariant: hold-to-mine breaks the block server-side on the server's timer.
    // Re-aim (the control break cleared the target) and hold the attack.
    aim_at(
        &mut sim,
        [
            dig_block[0] as f64,
            dig_block[1] as f64 + 0.5,
            dig_block[2] as f64 + 0.5,
        ],
    );
    settle_target(&mut sim);
    sim.begin_attack();
    let broke_ok = poll_until(&mut rcon, dig_block, "minecraft:air", &mut sim);
    sim.end_attack();
    eprintln!("[invariant · routed dig] server air at {dig_block:?}: {broke_ok}");
    assert!(
        broke_ok,
        "routed mining did not break the block server-side: {dig_block:?} is not air. The shell \
         must hold `START_DESTROY_BLOCK` so the server's destroy timer breaks it."
    );

    rcon.cmd(&format!(
        "forceload remove {} {} {} {}",
        px - 4,
        pz - 4,
        px + 6,
        pz + 6
    ));
}

/// Step the sim one tick and drain its frame outputs, the way the app loop does.
fn pump(sim: &mut Sim) {
    sim.step(1.0 / 20.0);
    let _ = sim.drain_meshes();
    let _ = sim.drain_removals();
}

/// Recompute the view target across a few frames so a freshly-streamed column is
/// picked up before we read `sim.target()`.
fn settle_target(sim: &mut Sim) {
    for _ in 0..5 {
        pump(sim);
        sim.update_target(ASPECT);
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Set the player's yaw/pitch to look from the eye toward a world point, using
/// vanilla's forward-vector convention (yaw 0 = south/+Z, pitch 90 = down).
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

/// Whether the server reports `block` exactly at `pos` (`execute if block`).
fn is_block(rcon: &mut RconClient, pos: [i32; 3], block: &str) -> bool {
    let resp = rcon.cmd(&format!(
        "execute if block {} {} {} {block}",
        pos[0], pos[1], pos[2]
    ));
    resp.contains("Test passed")
}

/// Poll server truth for up to ~12s while continuing to drive the sim, so a
/// held dig keeps its packets flowing and a placement ack lands.
fn poll_until(rcon: &mut RconClient, pos: [i32; 3], block: &str, sim: &mut Sim) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        pump(sim);
        assert!(
            sim.net().is_some() && !matches!(sim.session_phase(), SessionPhase::Ended(_)),
            "the live connection dropped mid-dig (phase={:?}); the dig cannot reach the server.",
            sim.session_phase()
        );
        if is_block(rcon, pos, block) {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
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
