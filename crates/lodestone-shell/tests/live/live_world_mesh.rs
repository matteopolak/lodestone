//! Live end-to-end gate for the milestone that matters: a real vanilla-26.2
//! server's streamed world must **mesh into non-trivial, correctly-lit geometry**
//! through the shell's own live path — the same chain the windowed client walks:
//!
//! ```text
//! live chunk packets → NetClient::sections_and_light_at
//!   → mesher::snapshot_section_live → mesh_snapshot(vanilla classifier)
//!   → GPU-ready quads with per-vertex sky/block light
//! ```
//!
//! It is deliberately **two-stage**, because either half alone passes vacuously:
//!
//! 1. **Geometry is non-trivial.** An empty/air world meshes to zero quads and is
//!    trivially "not full-bright", so a lighting-only gate would pass on a world
//!    that draws nothing. We first assert a real coverage threshold of merged
//!    quads from live server chunks — the proof the demo→vanilla classifier swap
//!    actually connected the island (with the demo palette every vanilla state id
//!    classifies to air, so this count would be ~0).
//! 2. **Lighting is real.** Only then do we assert that a shadowed cell meshes
//!    **measurably darker** than an open-sky one, and that the full-bright control
//!    ([`SectionSnapshot::full_bright_control`], the retired `UniformLight`
//!    bridge) **cannot tell them apart** — the exact assertion the pre-light path
//!    fails. "It still draws" proves nothing here: full-bright and correct light
//!    both emit the same geometry.
//!
//! To make stage 2 **deterministic** rather than a race against which chunks a
//! cave-spawn streams first, we build the shadow ourselves: on the flat creative
//! oracle (`:25570`, RCON `:25571`) we connect the shell's client first (the flat
//! oracle unloads chunks when empty, so a player must be online before RCON can
//! edit the world), read the player's spawn column, then RCON a sealed stone room
//! inside it. The server relights the fully-enclosed interior to sky `0` and
//! streams the block+light edits to the connected client (the v770 adapter applies
//! `BLOCK_UPDATE`/`SECTION_BLOCKS_UPDATE` + `LIGHT_UPDATE` live). The surrounding
//! flat ground stays under open sky (`15`), so the meshed column carries the full
//! gradient. Building the shadow (rather than hunting for one) is the same reason
//! the entity/container gates use this oracle: it is the target where we can
//! *cause* a known world arrangement over RCON.
//!
//! Gated behind `--features live` **and** `#[ignore]`. Per §12.52 it **fails**
//! rather than skips when it cannot run — no server, no RCON, or vanilla assets
//! missing is a failure with a fix hint, because a skip here reads exactly like a
//! pass and this is the only thing that proves the live render path end to end.
//!
//! ```text
//! cargo test -p lodestone-shell --features live --test live_world_mesh -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::blocks::ShellClassifier;
use lodestone::mesher::{SectionKey, mesh_snapshot, snapshot_section_live};
use lodestone::net::{NetClient, NetUpdate};
use lodestone::resources::BlockResources;
use lodestone_testsupport::{RconClient, unique_username};

const HOST: &str = "127.0.0.1";
/// The flat creative 26.2 oracle: game on `:25570`, RCON on `:25571`. Named only
/// as a protocol *number* — the shell never names a version — resolved through
/// the registry by the `live` feature.
const PORT: u16 = 25570;
const RCON_ADDR: &str = "127.0.0.1:25571";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL: i32 = 776;

/// One full section face is `16 * 16 = 256` quads; requiring more than that
/// proves the live column produced substantially more than a single flat face,
/// i.e. real terrain, not a blackout or a single stray block.
const MIN_LIVE_QUADS: usize = 256;

/// Highest per-vertex sky-light byte a mesh emitted (`0..=255`, scaled from the
/// stored `0..=15`).
fn max_vertex_sky(mesh: &lodestone_render::Mesh) -> u8 {
    mesh.vertices
        .iter()
        .map(|v| v.unpack().sky_light)
        .max()
        .unwrap_or(0)
}

/// Lowest per-vertex sky-light byte a mesh emitted.
fn min_vertex_sky(mesh: &lodestone_render::Mesh) -> u8 {
    mesh.vertices
        .iter()
        .map(|v| v.unpack().sky_light)
        .min()
        .unwrap_or(255)
}

/// Parse `x` and `z` out of an RCON `data get entity @p Pos` reply, whose tail is
/// `[<x>d, <y>d, <z>d]`.
fn parse_xz(reply: &str) -> Option<(f64, f64)> {
    let start = reply.find('[')?;
    let end = reply[start..].find(']')? + start;
    let nums: Vec<f64> = reply[start + 1..end]
        .split(',')
        .filter_map(|p| p.trim().trim_end_matches('d').parse().ok())
        .collect();
    (nums.len() >= 3).then(|| (nums[0], nums[2]))
}

/// Connect-and-wait helper: block until the client logs in and reports column
/// geometry, or fail loudly.
fn wait_logged_in(net: &NetClient, label: &str) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut logged_in = false;
    let mut last_err: Option<String> = None;
    while Instant::now() < deadline {
        for u in net.poll() {
            match u {
                NetUpdate::LoggedIn { .. } => logged_in = true,
                NetUpdate::Error(e) => last_err = Some(e),
                NetUpdate::Disconnected(r) => {
                    last_err = Some(format!("disconnected: {}", r.to_plain_string()))
                }
                _ => {}
            }
        }
        if logged_in && net.world_dimensions().is_some() && !net.loaded_chunks().is_empty() {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!(
        "{label} client never logged in to {HOST}:{PORT} within 60s (last event: {last_err:?}). \
         Fix: start the flat creative 26.2 oracle and run with `--features live`."
    );
}

#[test]
#[ignore = "requires the flat creative 26.2 oracle on :25570 (+ RCON :25571), the vanilla assets under .cache/mc/26.2, and `--features live`"]
fn live_world_meshes_into_lit_geometry_and_the_bridge_cannot_tell() {
    // Stage 0: the vanilla classifier + atlas must actually load. On the demo
    // palette every vanilla state id classifies to air, so the whole gate would
    // pass vacuously (zero geometry). Fail loud with the fix.
    let resources = BlockResources::load(true);
    assert!(
        resources.vanilla_atlas.is_some(),
        "vanilla assets did not load, so the live world would mesh with the demo palette \
         (every vanilla id → air). Banner: {:?}. Fix: put a vanilla pack at .cache/mc/26.2 \
         (client.jar + generated/reports/blocks.json) or set LODESTONE_ASSETS.",
        resources.banner
    );
    let classifier: ShellClassifier = resources.classifier;

    // A player must be online before RCON can find the spawn column and before
    // the flat oracle keeps chunks resident. Connect a *scout* client, learn where
    // it spawned, then build the room in its column.
    // `connect_as`, not `connect`: a live gate needs a fresh identity per run
    // (a shared offline name is a shared player file, and a dead player is held
    // on the death screen, which sends no chunks). `connect` is the *stable*
    // persisted offline identity, which is production's job, not a gate's.
    let scout = NetClient::connect_as(HOST.into(), PORT, PROTOCOL, None, unique_username());
    wait_logged_in(&scout, "scout");

    let mut rcon = RconClient::connect(RCON_ADDR, RCON_PASSWORD).unwrap_or_else(|e| {
        panic!(
            "cannot reach RCON at {RCON_ADDR}: {e}. Fix: start the flat creative 26.2 oracle \
             (game :25570, RCON :25571) and run with `--features live`."
        )
    });
    let pos_reply = rcon.cmd("data get entity @p Pos");
    let (px, pz) = parse_xz(&pos_reply)
        .unwrap_or_else(|| panic!("could not parse player position from RCON: {pos_reply:?}"));
    let (scx, scz) = (
        (px.floor() as i32).div_euclid(16),
        (pz.floor() as i32).div_euclid(16),
    );
    let room_chunk = (scx, scz);

    // Force-load the room column so the server keeps it resident **and fully lit**
    // independent of any player, then build a sealed stone room inside it. A
    // one-block-thick shell fully encloses the interior, so no skylight leaks in
    // and the server relights the pocket to sky 0. Coordinates stay inside the
    // chunk's 16-wide footprint.
    let (x0, z0) = (scx * 16 + 2, scz * 16 + 2);
    let (y_lo, y_hi) = (78, 97);
    let r_force = rcon.cmd(&format!(
        "forceload add {} {} {} {}",
        scx * 16,
        scz * 16,
        scx * 16 + 15,
        scz * 16 + 15
    ));
    let stone = format!(
        "fill {x0} {y_lo} {z0} {} {y_hi} {} minecraft:stone",
        x0 + 11,
        z0 + 11
    );
    let air = format!(
        "fill {} {} {} {} {} {} minecraft:air",
        x0 + 1,
        y_lo + 1,
        z0 + 1,
        x0 + 10,
        y_hi - 1,
        z0 + 10
    );
    let r_stone = rcon.cmd(&stone);
    let r_air = rcon.cmd(&air);
    eprintln!(
        "room in chunk {room_chunk:?} (x0={x0} z0={z0}): forceload={r_force:?} stone={r_stone:?} \
         air={r_air:?}"
    );
    assert!(
        !r_stone.contains("not loaded") && !r_stone.contains("No entity"),
        "fill did not land ({r_stone:?}); the spawn chunk was not editable — is a player online?"
    );

    // Give the server a moment to run its lighting engine over the new geometry.
    std::thread::sleep(Duration::from_secs(2));

    // Connect a **fresh** client so the room column streams down as a full
    // chunk-data packet carrying the server's now-seam-complete light — far more
    // reliable than depending on incremental LIGHT_UPDATE deltas reaching the
    // scout. The scout stays connected to keep the area active while B streams.
    // `connect_as`, not `connect`: a live gate needs a fresh identity per run
    // (a shared offline name is a shared player file, and a dead player is held
    // on the death screen, which sends no chunks). `connect` is the *stable*
    // persisted offline identity, which is production's job, not a gate's.
    let net = NetClient::connect_as(HOST.into(), PORT, PROTOCOL, None, unique_username());
    wait_logged_in(&net, "reader");
    let dims = net.world_dimensions().expect(
        "logged in but the client never reported world dimensions — the column geometry seam \
         (world_dimensions) returned None; cannot place live sections.",
    );
    let chunk_deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < chunk_deadline {
        for _ in net.poll() {}
        if net.loaded_chunks().iter().any(|c| (c.x, c.z) == room_chunk) {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    assert!(
        net.loaded_chunks().iter().any(|c| (c.x, c.z) == room_chunk),
        "the room column {room_chunk:?} never streamed to the reader client within 30s; cannot \
         mesh the constructed shadow."
    );
    // Drain a little more so late light packets for the column settle in.
    let settle = Instant::now() + Duration::from_secs(2);
    while Instant::now() < settle {
        for _ in net.poll() {}
        std::thread::sleep(Duration::from_millis(100));
    }

    let section_count = dims.section_count();

    // Mesh every section of the room column, exactly as `mark_column_dirty` does
    // on the render path. The sealed interior contributes shadowed faces; the
    // surrounding flat ground and the box's own sky-lit roof contribute bright
    // faces — the whole gradient in one column.
    let mut snapshots = Vec::new();
    for si in 0..section_count {
        let key = SectionKey {
            cx: room_chunk.0,
            cz: room_chunk.1,
            si,
            min_y: dims.min_y,
        };
        // `any()` rather than `ready()`: this gate measures *light*, and it is
        // deliberately indifferent to whether the column's horizontal
        // neighbourhood has finished arriving (that fix's deferral). A section
        // held back from the screen still carries the server's real light, which
        // is the only thing asserted below — and gating on `ready()` here would
        // make the gate's population depend on chunk-arrival order.
        if let Some(snap) = snapshot_section_live(&net, key, section_count).any() {
            snapshots.push(snap);
        }
    }
    assert!(
        !snapshots.is_empty(),
        "no section meshed in the room column — snapshot_section_live found nothing for every \
         section (blackout, or every centre section read as all-air)."
    );

    // ---- Stage 1: non-trivial geometry from live server chunks. ----
    let live_meshes: Vec<lodestone_render::Mesh> = snapshots
        .iter()
        .map(|s| mesh_snapshot(s, &classifier))
        .collect();
    let total_quads: usize = live_meshes.iter().map(|m| m.quad_count()).sum();
    assert!(
        total_quads > MIN_LIVE_QUADS,
        "live world meshed only {total_quads} quads across {} sections — below the \
         {MIN_LIVE_QUADS}-quad coverage floor. With the vanilla classifier connected this should \
         be thousands; ~0 means vanilla ids are still classifying to air (the demo→vanilla swap \
         did not take).",
        snapshots.len()
    );

    // ---- Stage 2: lighting is real, and the full-bright bridge is not. ----
    let live_max = live_meshes.iter().map(max_vertex_sky).max().unwrap_or(0);
    let live_min = live_meshes.iter().map(min_vertex_sky).min().unwrap_or(255);
    let live_delta = live_max.saturating_sub(live_min);
    eprintln!(
        "live world mesh: {} lit sections, {total_quads} quads; sky light [{live_min}..{live_max}]",
        snapshots.len()
    );

    assert!(
        live_max > 200,
        "no sky-lit faces (max sky {live_max}); expected the flat oracle's open ground / box roof \
         to reach full sky brightness. Is this the flat creative oracle?"
    );
    assert!(
        live_min < 64,
        "the sealed room did not read as shadow (min sky {live_min}); expected the fully-enclosed \
         interior to relight to ~0. Did the fill land, and did the client apply the server's \
         block + light updates before we meshed?"
    );
    assert!(
        live_delta >= 128,
        "live vertex sky light spans only {live_delta} ({live_min}..{live_max}) — the shadowed \
         interior and the open-sky ground should differ by most of the 0..255 range. A near-flat \
         field means the light seam collapsed to full-bright."
    );

    // Control: re-mesh the SAME snapshots with all light stripped, forcing
    // `mesh_snapshot` onto the retired full-bright bridge. That path renders every
    // face at the same full brightness, so it CANNOT reproduce the gradient — the
    // `min < max` assertion fails for it. This is what proves the live gate
    // measures light, not merely geometry.
    let control_meshes: Vec<lodestone_render::Mesh> = snapshots
        .iter()
        .map(|s| mesh_snapshot(&s.full_bright_control(), &classifier))
        .collect();
    let control_max = control_meshes.iter().map(max_vertex_sky).max().unwrap_or(0);
    let control_min = control_meshes
        .iter()
        .map(min_vertex_sky)
        .min()
        .unwrap_or(255);
    assert_eq!(
        control_min, control_max,
        "the full-bright control produced a gradient (min {control_min} != max {control_max}); it \
         must render every face identically, otherwise it isn't a valid full-bright control."
    );
    assert_eq!(
        control_max, 255,
        "the full-bright control should render every face at full sky brightness (255), got \
         {control_max}."
    );

    // Clean up the structure and the force-load so re-runs start from flat ground.
    rcon.cmd(&format!(
        "fill {x0} {y_lo} {z0} {} {y_hi} {} minecraft:air",
        x0 + 11,
        z0 + 11
    ));
    rcon.cmd(&format!("forceload remove {} {}", scx * 16, scz * 16));

    eprintln!(
        "live world mesh gate OK: {total_quads} quads; live sky [{live_min}..{live_max}] (real \
         shadow), full-bright control flat at {control_max} (cannot tell shadow from open sky)"
    );
}
