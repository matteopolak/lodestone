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
//!    quads from live server columns — the proof the demo→vanilla classifier swap
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
//! oracle (`:25570`, RCON `:25571`) we RCON a sealed stone room whose interior is
//! fully enclosed, so the server relights it to sky `0`; the surrounding flat
//! ground stays under open sky (`15`). Both land in the same sampled column, so
//! the meshes carry the full gradient. Building the shadow (rather than hunting
//! for one) is the same reason the entity/container gates use this oracle: it is
//! the target where we can *cause* a known world arrangement over RCON.
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
use lodestone_testsupport::RconClient;

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

/// The sealed room is built inside chunk `(6, 6)`, the flat oracle's spawn chunk
/// (player spawns at ~`(96.5, 80, 96.5)`), so it is resident the moment the
/// client logs in. Coordinates stay within `x,z ∈ [96, 111]` so the whole box
/// lives in that one column.
const ROOM_CHUNK: (i32, i32) = (6, 6);

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

    // Build a sealed stone room over RCON so its interior is a guaranteed sky-`0`
    // shadow, then let the server relight before the client connects. A
    // one-block-thick shell fully encloses the interior, so no skylight leaks in.
    let mut rcon = RconClient::connect(RCON_ADDR, RCON_PASSWORD).unwrap_or_else(|e| {
        panic!(
            "cannot reach RCON at {RCON_ADDR}: {e}. Fix: start the flat creative 26.2 oracle \
             (game :25570, RCON :25571) and run with `--features live`."
        )
    });
    // Solid outer box, then hollow interior: interior x101..109, y79..96, z101..109.
    let r1 = rcon.cmd("fill 100 78 100 110 97 110 minecraft:stone");
    let r2 = rcon.cmd("fill 101 79 101 109 96 109 minecraft:air");
    let probe_solid = rcon.cmd("execute if block 100 88 100 minecraft:stone");
    let probe_air = rcon.cmd("execute if block 105 88 105 minecraft:air");
    let spawn = rcon.cmd("data get entity @p Pos");
    eprintln!("fill stone -> {r1:?}\nfill air -> {r2:?}\nprobe wall -> {probe_solid:?}\nprobe interior -> {probe_air:?}\nplayer pos -> {spawn:?}");
    // Give the server a moment to propagate the sky-light removal into the box.
    std::thread::sleep(Duration::from_millis(750));

    // Connect and wait until the client holds the room's column AND knows the
    // column geometry (min_y / section_count) needed to place sections.
    let net = NetClient::connect(HOST.into(), PORT, PROTOCOL);
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut logged_in = false;
    let mut last_err: Option<String> = None;
    while Instant::now() < deadline {
        for u in net.poll() {
            match u {
                NetUpdate::LoggedIn { .. } => logged_in = true,
                NetUpdate::Error(e) => last_err = Some(e),
                NetUpdate::Disconnected(r) => last_err = Some(format!("disconnected: {r}")),
                _ => {}
            }
        }
        let has_room = net
            .loaded_chunks()
            .iter()
            .any(|c| (c.x, c.z) == ROOM_CHUNK);
        if logged_in && net.world_dimensions().is_some() && has_room {
            break;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    // Drain a final burst so the room column and its light are fully resident.
    std::thread::sleep(Duration::from_millis(500));
    for _ in net.poll() {}

    assert!(
        logged_in,
        "never logged in to {HOST}:{PORT} within 60s (last event: {last_err:?}). \
         Fix: start the flat creative 26.2 oracle and run with `--features live`."
    );
    let dims = net.world_dimensions().expect(
        "logged in but the client never reported world dimensions — the column geometry seam \
         (world_dimensions) returned None; cannot place live sections.",
    );
    assert!(
        net.loaded_chunks()
            .iter()
            .any(|c| (c.x, c.z) == ROOM_CHUNK),
        "the room column {ROOM_CHUNK:?} never became resident; cannot mesh the constructed shadow."
    );

    let section_count = dims.section_count();

    // Mesh every section of the room column, exactly as `mark_column_dirty` does
    // on the render path. The sealed interior contributes shadowed faces; the
    // surrounding flat ground and the box's own sky-lit roof contribute bright
    // faces — the whole gradient in one column.
    let mut snapshots = Vec::new();
    for si in 0..section_count {
        let key = SectionKey {
            cx: ROOM_CHUNK.0,
            cz: ROOM_CHUNK.1,
            si,
            min_y: dims.min_y,
        };
        if let Some(snap) = snapshot_section_live(&net, key, section_count) {
            snapshots.push(snap);
        }
    }
    assert!(
        !snapshots.is_empty(),
        "no section meshed in the room column — snapshot_section_live returned None for every \
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
         interior to relight to ~0. Did the fill land, and did the server propagate the skylight \
         removal before the client streamed the column?"
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
    let control_min = control_meshes.iter().map(min_vertex_sky).min().unwrap_or(255);
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

    eprintln!(
        "live world mesh gate OK: {total_quads} quads; live sky [{live_min}..{live_max}] (real \
         shadow), full-bright control flat at {control_max} (cannot tell shadow from open sky)"
    );
}
