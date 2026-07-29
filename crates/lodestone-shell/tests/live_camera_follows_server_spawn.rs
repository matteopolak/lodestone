//! Live regression gate for the "standing on invisible blocks" bug: on a server
//! whose spawn is far from the world origin, the shell camera must end up **over
//! the world the server actually streamed**, not stranded at the demo spawn.
//!
//! ## The bug this reproduces
//!
//! The shell runs its own physics starting at the demo spawn (`~0,71,0`) and
//! streams an optimistic position packet every tick from there. It never adopted
//! the server's authoritative placement. On a server whose spawn is far from the
//! origin (seed `lodestone` spawns at roughly `-237,-217`) the server ignores our
//! bogus "I'm at the origin" claim, keeps us at the real spawn, and streams chunks
//! **there** — hundreds of blocks from the camera. The camera then renders that
//! distant terrain ("far away looks fine") while sitting over the *unmeshed* demo
//! platform at the origin ("standing on invisible blocks; collision is present").
//!
//! ## Why the existing gate missed it
//!
//! `live_world_mesh.rs` meshes an explicitly-chosen column directly and never
//! drives the real join path through `Sim`, so it never observes where the camera
//! is relative to the streamed world. This gate drives the **real** path:
//! `Sim::new` → `attach_net` → `step()` pumping `poll_net`, exactly as the windowed
//! app does — and asserts the camera invariant that the streamed world guarantees.
//!
//! ## The invariant
//!
//! A vanilla server always keeps the player's *own* chunk column loaded (it streams
//! chunks centred on the player). So once the camera holds the server-authoritative
//! position, **the camera's chunk column is a member of the loaded set**. Pre-fix
//! the camera is pinned at the origin demo spawn while every loaded chunk is ~15
//! chunks away, so that membership fails — and the printed gap (~328 blocks) is the
//! bug made numeric. We deliberately push the world spawn far from the origin over
//! RCON so this is deterministic rather than luck.
//!
//! Gated behind `--features live` **and** `#[ignore]`. Per §12.52 it **fails**
//! rather than skips when it cannot run — no server, no RCON, or vanilla assets
//! missing is a failure with a fix hint, because a skip here reads exactly like a
//! pass.
//!
//! ```text
//! cargo test -p lodestone-shell --features live \
//!   --test live_camera_follows_server_spawn -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::config::{Config, Mode};
use lodestone::net::NetClient;
use lodestone::sim::Sim;
use lodestone_testsupport::RconClient;

const HOST: &str = "127.0.0.1";
/// The survival 26.2 oracle: game on `:25565`, RCON on `:25566`. Named only as a
/// protocol *number* — the shell never names a version — resolved through the
/// registry by the `live` feature.
const PORT: u16 = 25565;
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL: i32 = 776;

/// A deterministically far-from-origin world spawn. Any point whose chunk is many
/// chunks from `(0,0)` exposes the bug; this matches the region the user actually
/// hit on seed `lodestone`.
const SPAWN_X: i32 = -237;
const SPAWN_Y: i32 = 100;
const SPAWN_Z: i32 = -217;

fn chunk_of(block: f64) -> i32 {
    (block.floor() as i32).div_euclid(16)
}

#[test]
#[ignore = "requires the survival 26.2 oracle on :25565 (+ RCON :25566), the vanilla assets under .cache/mc/26.2, and `--features live`"]
fn camera_sits_over_the_streamed_world_not_the_demo_spawn() {
    // The vanilla atlas must load, or `Sim` takes the demo path (no live meshing,
    // no live render_live behaviour) and this gate would not exercise the bug.
    // Fail loud with the fix hint rather than pass vacuously.
    let probe = Sim::new(live_config());
    assert!(
        probe.vanilla_atlas().is_some(),
        "vanilla assets did not load, so Sim would run the demo path instead of the \
         live server world. Banner: {:?}. Fix: put a vanilla pack at .cache/mc/26.2 \
         (client.jar + generated/reports/blocks.json) or set LODESTONE_ASSETS.",
        probe.asset_banner()
    );
    drop(probe);

    // Push the world spawn far from the origin so a fresh join is placed there
    // deterministically — the whole point is a spawn that is *not* the origin the
    // shell defaults to.
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

    // Drive the *real* join path: exactly what the windowed app does each frame.
    let mut sim = Sim::new(live_config());
    let demo_spawn = sim.player().position;
    sim.attach_net(NetClient::connect(HOST.into(), PORT, PROTOCOL));

    let deadline = Instant::now() + Duration::from_secs(60);
    let mut loaded_seen = false;
    while Instant::now() < deadline {
        sim.step(1.0 / 20.0);
        let _ = sim.drain_meshes();
        let _ = sim.drain_removals();
        if let Some(net) = sim.net()
            && net.world_dimensions().is_some()
            && !net.loaded_chunks().is_empty()
        {
            loaded_seen = true;
            // Give the placement teleport + a couple of chunk bursts time to land
            // and be consumed by poll_net.
            if Instant::now() > deadline - Duration::from_secs(50) {
                std::thread::sleep(Duration::from_millis(500));
            }
        }
        std::thread::sleep(Duration::from_millis(20));
        // Stop early once we have a healthy world *and* the camera has moved off
        // the demo spawn (the fix landed) — or keep going to the deadline so a
        // pre-fix run gathers a full chunk set for a meaningful diagnostic.
        if loaded_seen && sim.player().position != demo_spawn {
            // One more settle pass so the loaded set is representative.
            std::thread::sleep(Duration::from_millis(500));
            break;
        }
    }

    let net = sim.net().expect("net attached");
    let loaded = net.loaded_chunks();
    assert!(
        loaded_seen && !loaded.is_empty(),
        "client never streamed any chunks within 60s. Fix: start the survival 26.2 oracle \
         on :25565 and run with `--features live`."
    );

    // Centre of the loaded region, for the diagnostic gap.
    let (mut minx, mut maxx, mut minz, mut maxz) = (i32::MAX, i32::MIN, i32::MAX, i32::MIN);
    for c in &loaded {
        minx = minx.min(c.x);
        maxx = maxx.max(c.x);
        minz = minz.min(c.z);
        maxz = maxz.max(c.z);
    }
    let world_cx = ((minx + maxx) as f64 / 2.0) * 16.0;
    let world_cz = ((minz + maxz) as f64 / 2.0) * 16.0;

    let cam = sim.player().position;
    let cam_chunk = (chunk_of(cam.x), chunk_of(cam.z));
    let gap = ((cam.x - world_cx).powi(2) + (cam.z - world_cz).powi(2)).sqrt();

    eprintln!("demo spawn (pre-sync camera) = {demo_spawn:?}");
    eprintln!("camera (sim.player().position) = {cam:?}  → chunk {cam_chunk:?}");
    eprintln!(
        "loaded chunk X range [{minx}..{maxx}], Z range [{minz}..{maxz}] \
         (centre world ~({world_cx:.0}, {world_cz:.0}), {} columns)",
        loaded.len()
    );
    eprintln!("camera↔world-centre gap = {gap:.0} blocks");

    // Sanity: the far spawn must actually have moved the streamed world off the
    // origin, or the test could pass vacuously (camera at origin *is* over the
    // world). This asserts the reproduction conditions held.
    assert!(
        world_cx.abs() > 64.0 || world_cz.abs() > 64.0,
        "the streamed world is centred near the origin ({world_cx:.0}, {world_cz:.0}); the far \
         setworldspawn did not take, so this run cannot distinguish the bug. Fix: ensure RCON \
         setworldspawn far from origin succeeded (and clear stale playerdata if the join reused \
         an origin position)."
    );

    // THE INVARIANT. A vanilla server always keeps the player's own chunk loaded,
    // so a correctly-placed camera's chunk is a member of the loaded set. Pre-fix
    // the camera is pinned at the origin demo spawn while every loaded chunk is
    // ~15 chunks away → this membership fails and the gap above reads ~328 blocks.
    let camera_chunk_loaded = loaded
        .iter()
        .any(|c| c.x == cam_chunk.0 && c.z == cam_chunk.1);
    assert!(
        camera_chunk_loaded,
        "camera is over chunk {cam_chunk:?}, which the server never streamed — the camera is \
         stranded off the world (gap {gap:.0} blocks). The shell did not adopt the server's \
         authoritative spawn: it is still sitting at the demo spawn {demo_spawn:?} while the \
         world streamed around ({world_cx:.0}, {world_cz:.0}). This is the 'standing on \
         invisible blocks' bug."
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
