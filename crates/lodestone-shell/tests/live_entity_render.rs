//! Live end-to-end gate: a **server-sent** mob must reach pixels through the
//! shell's real wiring.
//!
//! `lodestone-render`'s `entity_gate` proves the *pipeline* draws a mob, but it
//! feeds a synthetic pig it constructs in-process — nothing there proves a mob
//! the **server** spawned survives the whole shell chain:
//!
//! ```text
//! live ADD_ENTITY → ClientHandle::entities() → NetClient::entity_snapshots()
//!   → EntityInterpolator → EntityDraw → RenderState::render → GPU pixels
//! ```
//!
//! This test closes that gap. It connects the shell's own [`NetClient`] to the
//! live vanilla-26.2 oracle, summons a pig at the player's feet over RCON, polls
//! until that pig crosses the public client API, aims a camera at where the
//! server actually put it, renders one frame through the exact call the live
//! frame loop makes, and reads the pixels back.
//!
//! Gated behind the `live` feature (which compiles the v770 family into the
//! registry) **and** `#[ignore]`, so the default `cargo test` stays hermetic and
//! version-free. Run it explicitly:
//!
//! ```text
//! cargo test -p lodestone-shell --features live --test live_entity_render -- --ignored --nocapture
//! ```
//!
//! Per §12.52 this test **fails** rather than skips when it cannot run — no GPU
//! adapter, no server, or no RCON is a failure, because a skip here reads exactly
//! like a pass and this is the only thing that proves the wiring end to end.
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::entities::{EntityDraw, EntityInterpolator};
use lodestone::gpu::RenderState;
use lodestone::net::NetClient;
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};
use lodestone_testsupport::RconClient;

const GAME_HOST: &str = "127.0.0.1";
/// The purpose-built summon+observe oracle: game on :25567, RCON on :25575.
/// It is a real vanilla-26.2 server (protocol 776), the one target where we can
/// both *place* a known mob and *watch* it arrive over the public API. The
/// mc262 server on :25565 has no reachable RCON, so a mob cannot be summoned at
/// a known position there.
const GAME_PORT: u16 = 25567;
const RCON_ADDR: &str = "127.0.0.1:25575";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL_26_2: i32 = 776;

const PROBE_TAG: &str = "shellrenderprobe";

/// Parse a `data get entity ... Pos` RCON response's `[x, y, z]` list.
fn parse_list3(resp: &str) -> Option<(f64, f64, f64)> {
    let open = resp.find('[')?;
    let close = resp[open..].find(']')? + open;
    let inner = &resp[open + 1..close];
    let nums: Vec<f64> = inner
        .split(',')
        .filter_map(|s| s.trim().trim_end_matches('d').parse::<f64>().ok())
        .collect();
    (nums.len() == 3).then(|| (nums[0], nums[1], nums[2]))
}

/// Yaw/pitch (degrees) that aim the camera from `eye` at `target`, inverting the
/// render camera's convention `forward = (-sin y·cos p, -sin p, cos y·cos p)`.
fn look_at(eye: glam::Vec3, target: glam::Vec3) -> (f32, f32) {
    let d = (target - eye).normalize();
    let pitch = (-d.y).asin().to_degrees();
    let yaw = (-d.x).atan2(d.z).to_degrees();
    (yaw, pitch)
}

#[test]
#[ignore = "requires the live vanilla-26.2 oracle on :25567 (+ RCON :25575) and a GPU adapter"]
fn server_sent_mob_reaches_pixels_through_shell() {
    // --- GPU first: no adapter is a failure, not a skip (§12.52). ------------
    let ctx = GpuContext::new_headless_blocking().expect(
        "no wgpu adapter. This #[ignore]d gate is an explicit request for the full live+GPU \
         path — run it on a host with an adapter (or a software one), don't 'skip': a silent \
         pass here would assert that a mob renders when nothing was ever drawn.",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let (w, h) = (320u32, 240u32);
    let mut target = HeadlessTarget::new(device, w, h, format);

    // --- Connect the shell's own net client to the live oracle. --------------
    let net = NetClient::connect(GAME_HOST.to_owned(), GAME_PORT, PROTOCOL_26_2);

    // Wait until the bot is actually in the world (chunks streaming). The net
    // thread drives independently; draining poll() keeps its update channel from
    // growing unbounded while we wait.
    let ready_deadline = Instant::now() + Duration::from_secs(20);
    let mut in_world = false;
    while Instant::now() < ready_deadline {
        let _ = net.poll();
        if !net.loaded_chunks().is_empty() || !net.entity_snapshots().is_empty() {
            in_world = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        in_world,
        "the shell's NetClient never reached the world on {GAME_HOST}:{GAME_PORT} — connection \
         or login fault (is the vanilla-26.2 oracle up?), not the render path"
    );

    // --- Summon a pig at the player's feet over RCON. ------------------------
    let (px, py, pz) = {
        let mut r = RconClient::connect(RCON_ADDR, RCON_PASSWORD).expect(
            "oracle RCON reachable/authenticated at 127.0.0.1:25575 — is the vanilla-26.2 \
             oracle up? A missing RCON is a harness failure, not a passing render path.",
        );
        // `@p` is the nearest player to the command origin; with our single bot
        // joined it resolves to us. v770 does not emit TeleportPlayer, so the
        // read-model's position never populates on 26.2 — RCON is the only way
        // to learn where the server actually put the player.
        let pos = r.cmd("data get entity @p Pos");
        let (px, py, pz) = parse_list3(&pos)
            .expect("player Pos readable via RCON after join — otherwise the bot never spawned");

        // Force-load the column so the pig can never fall out of a ticking chunk
        // mid-observation, clear any stale probe, then summon.
        r.cmd(&format!(
            "forceload add {} {}",
            px.floor() as i64,
            pz.floor() as i64
        ));
        r.cmd(&format!("kill @e[type=pig,tag={PROBE_TAG}]"));
        r.cmd(&format!(
            "summon pig {px:.3} {py:.3} {pz:.3} {{Tags:[\"{PROBE_TAG}\"],NoAI:1b}}"
        ));
        // `tick sprint` advances the entity systems that emit the tracking
        // packet; `tick step` does NOT on these servers. (It also silently
        // consumes a tick — a phantom +1 offset — but we read position from the
        // API afterward, so that doesn't affect this gate.)
        r.cmd("tick sprint 5");
        (px, py, pz)
    };

    // --- Poll the shell's entity path until the summoned pig crosses it. -----
    // The oracle overworld holds ambient pigs, so select the pig *nearest the
    // summon point* (our probe is at the player's feet, distance ~0; ambient
    // pigs are chunks away) and require it to actually be there — picking any
    // pig would be a flaky false positive.
    let mut pig: Option<EntityDraw> = None;
    let mut interp = EntityInterpolator::new();
    let find_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < find_deadline {
        let _ = net.poll();
        let snaps = net.entity_snapshots();
        // Advance the real interpolation seam with a full tick so draws() lands
        // on the current server pose rather than an easing midpoint.
        interp.update(&snaps, 1.0);
        let nearest = interp
            .draws()
            .into_iter()
            .filter(|d| d.type_path.contains("pig"))
            .min_by(|a, b| {
                let da = (f64::from(a.feet.x) - px).powi(2) + (f64::from(a.feet.z) - pz).powi(2);
                let db = (f64::from(b.feet.x) - px).powi(2) + (f64::from(b.feet.z) - pz).powi(2);
                da.total_cmp(&db)
            });
        if let Some(d) = nearest
            && (f64::from(d.feet.x) - px).abs() < 1.5
            && (f64::from(d.feet.z) - pz).abs() < 1.5
        {
            pig = Some(d);
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    let pig = pig.unwrap_or_else(|| {
        // Clean up before failing so a leaked probe can't poison later runs.
        if let Ok(mut r) = RconClient::connect(RCON_ADDR, RCON_PASSWORD) {
            r.cmd(&format!("kill @e[type=pig,tag={PROBE_TAG}]"));
        }
        panic!(
            "the summoned pig never crossed the shell's entity path (net → entity_snapshots → \
             interpolator → EntityDraw) within the timeout. The server registered it (RCON \
             summon returned), so this is a real gap in the shell/client entity wiring, not a \
             harness fault."
        );
    });

    // --- Aim a camera at where the server actually placed the pig. -----------
    // Pig body centre sits a little above its feet; view it from three blocks
    // north at body height so it fills the frame's middle.
    let pig_centre = pig.feet + glam::Vec3::new(0.0, 0.6, 0.0);
    let eye = pig.feet + glam::Vec3::new(0.0, 0.9, -3.0);
    let (yaw, pitch) = look_at(eye, pig_centre);
    let camera = Camera {
        position: eye,
        yaw,
        pitch,
        fov_y_degrees: 60.0,
        aspect: w as f32 / h as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    // --- Render one frame through the exact call the live loop makes. --------
    // No terrain is uploaded, so the background is the sky clear colour and any
    // non-sky pixel is the pig. That is deliberate: this gate proves the *mob*
    // path, and a clean background makes "did a server-sent mob reach pixels"
    // an unambiguous yes/no.
    let state = RenderState::new(device, queue, format, w, h);
    let draws = vec![pig.clone()];
    let frame = target.acquire().expect("headless acquire");
    let stats = state.render(device, queue, frame.view(), &camera, None, &draws);
    let pixels = target.read_texels(device, queue);

    // Clean up the probe now that we have our pixels; a leaked NoAI pig would
    // accumulate across runs.
    if let Ok(mut r) = RconClient::connect(RCON_ADDR, RCON_PASSWORD) {
        r.cmd(&format!("kill @e[type=pig,tag={PROBE_TAG}]"));
    }

    assert_eq!(
        stats.entities_drawn, 1,
        "exactly the server-sent pig should draw (drawn={}, culled={})",
        stats.entities_drawn, stats.entities_culled
    );

    let sky = [135u8, 181, 235];
    let is_mob = |px: &[u8]| -> bool {
        let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
            + (i32::from(px[1]) - i32::from(sky[1])).abs()
            + (i32::from(px[2]) - i32::from(sky[2])).abs();
        d > 60
    };

    let mut mob_px = 0usize;
    let mut centre_px = 0usize;
    let mut corner_px = 0usize;
    for (i, px) in pixels.chunks_exact(4).enumerate() {
        let x = (i as u32) % w;
        let y = (i as u32) / w;
        if is_mob(px) {
            mob_px += 1;
            if x >= w / 4 && x < 3 * w / 4 && y >= h / 4 && y < 3 * h / 4 {
                centre_px += 1;
            }
            if (x < w / 8 || x >= 7 * w / 8) && (y < h / 8 || y >= 7 * h / 8) {
                corner_px += 1;
            }
        }
    }
    let coverage = mob_px as f64 / f64::from(w * h);

    eprintln!("=== shell live entity render ===");
    eprintln!(
        "pig feet (server) = ({:.2}, {:.2}, {:.2})",
        pig.feet.x, pig.feet.y, pig.feet.z
    );
    eprintln!("summon point      = ({px:.2}, {py:.2}, {pz:.2})");
    eprintln!("entities drawn    = {}", stats.entities_drawn);
    eprintln!("mob coverage      = {:.2}%", coverage * 100.0);
    eprintln!("centre mob px     = {centre_px}");
    eprintln!("corner mob px     = {corner_px}");

    // The pig must reach a real run of pixels in the centre, and the corners
    // must stay sky — a full-frame or corner-smeared result would mean a broken
    // clear or a mob glued to the near plane, not a mob at a world position.
    assert!(
        mob_px > 200,
        "a server-sent pig should reach pixels, only {mob_px} non-sky px ({:.2}%)",
        coverage * 100.0
    );
    assert!(
        coverage < 0.6,
        "the pig should not fill the frame ({:.1}% non-sky) — a broken clear or a near-plane mob",
        coverage * 100.0
    );
    assert!(
        centre_px > 100,
        "the pig should sit in the centre where the camera is aimed, only {centre_px} centre px"
    );
    assert_eq!(
        corner_px, 0,
        "the frame corners should stay sky (no terrain uploaded), but {corner_px} corner px read as mob"
    );

    drop(net);
}
