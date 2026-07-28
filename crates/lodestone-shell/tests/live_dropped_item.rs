//! Live gate: an item entity the **server** spawned must reach the shell's
//! entity path, and must reach pixels once its stack is known.
//!
//! ```text
//! /summon item → ADD_ENTITY → ClientHandle::entities()
//!   → NetClient::entity_snapshots()  (type_path == "item")
//!   → EntityInterpolator → EntityDraw → RenderState::render → GPU pixels
//! ```
//!
//! # The one link this test supplies by hand, and why
//!
//! Everything above is real except the item's **identity**. A dropped item
//! carries its stack in entity metadata index 8 under the `ITEM_STACK`
//! serializer, and the 26.2 adapter rejects that serializer outright
//! (`protocol/v770/src/packets/metadata.rs`:
//! `SER_ITEM_STACK | SER_PARTICLE | … => return Err(unknown_serializer(…))`,
//! commented "complex, self-describing payloads mobs never emit" — true of mobs,
//! false of item entities, which emit exactly this and nothing else). A failed
//! metadata decode raises no event at all, so nothing downstream — not
//! `EntityMetadataUpdate`, not `EntityView`, not `EntitySnapshot` — has anywhere
//! to put an item id.
//!
//! So this test calls [`EntityInterpolator::set_item_stack`] with the item it
//! just summoned, standing in for that decode. That is deliberately the *only*
//! thing it fakes, and it is stated here rather than hidden, because it makes
//! the test's result precise: **everything except the metadata decode works
//! against a real server.** When the adapter learns the serializer, deleting the
//! `set_item_stack` line below must leave this test green.
//!
//! The first half is not faked at all, and is asserted separately: a server-sent
//! item entity really does arrive as a tracked `EntityDraw` with type path
//! `item`. That is the half that says "the drop exists but is invisible" rather
//! than "the drop never arrives".
//!
//! Per §12.52 this fails rather than skips when it cannot run.
//!
//! ```text
//! cargo test -p lodestone-shell --features live --test live_dropped_item -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::entities::{EntityDraw, EntityInterpolator, ITEM_ENTITY_TYPE_PATH};
use lodestone::gpu::RenderState;
use lodestone::net::NetClient;
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};
use lodestone_testsupport::RconClient;

const GAME_HOST: &str = "127.0.0.1";
const GAME_PORT: u16 = 25565;
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL_26_2: i32 = 776;

/// A full block item, so the drawn geometry is a solid cube rather than a flat
/// sprite standing on edge — the biggest, least ambiguous silhouette available.
const ITEM: &str = "minecraft:diamond_block";

const W: u32 = 320;
const H: u32 = 240;

/// Parse a `data get entity … Pos` response's `[x, y, z]` list.
fn parse_list3(resp: &str) -> Option<(f64, f64, f64)> {
    let open = resp.find('[')?;
    let close = resp[open..].find(']')? + open;
    let nums: Vec<f64> = resp[open + 1..close]
        .split(',')
        .filter_map(|s| s.trim().trim_end_matches('d').parse::<f64>().ok())
        .collect();
    (nums.len() == 3).then(|| (nums[0], nums[1], nums[2]))
}

/// Yaw/pitch (degrees) aiming from `eye` at `target`, inverting the render
/// camera's `forward = (-sin y·cos p, -sin p, cos y·cos p)`.
fn look_at(eye: glam::Vec3, target: glam::Vec3) -> (f32, f32) {
    let d = (target - eye).normalize();
    ((-d.x).atan2(d.z).to_degrees(), (-d.y).asin().to_degrees())
}

#[test]
#[ignore = "requires the live vanilla-26.2 oracle on :25565 (+ RCON :25566), a GPU adapter and client.jar"]
fn a_server_spawned_drop_is_tracked_and_reaches_pixels() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "no wgpu adapter. This #[ignore]d gate is an explicit request for the full \
         live+GPU path — run it on a host with an adapter, don't 'skip'.",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "the vanilla pack must load for item geometry to exist. Banner: {:?}",
            resources.banner
        )
    });
    let item: ResourceLocation = ITEM.parse().expect("valid item id");

    // --- connect ---------------------------------------------------------
    let net = NetClient::connect(GAME_HOST.to_owned(), GAME_PORT, PROTOCOL_26_2);
    let ready = Instant::now() + Duration::from_secs(25);
    let mut in_world = false;
    while Instant::now() < ready {
        let _ = net.poll();
        if !net.loaded_chunks().is_empty() || !net.entity_snapshots().is_empty() {
            in_world = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    assert!(
        in_world,
        "the shell's NetClient never reached the world on {GAME_HOST}:{GAME_PORT} — \
         a connection fault, not a render one"
    );

    // --- cause a drop ----------------------------------------------------
    let (px, py, pz) = {
        let mut r = RconClient::connect(RCON_ADDR, RCON_PASSWORD).expect(
            "oracle RCON reachable at 127.0.0.1:25566 — a missing RCON is a harness \
             failure, not a passing render path",
        );
        let (px, py, pz) = parse_list3(&r.cmd("data get entity @p Pos"))
            .expect("player Pos readable via RCON after join");
        r.cmd(&format!(
            "forceload add {} {}",
            px.floor() as i64,
            pz.floor() as i64
        ));
        r.cmd("kill @e[type=item]");
        // PickupDelay 32767 is the never-pick-up sentinel (`ItemLifecycle`'s
        // NEVER_PICKUP_DELAY): without it the bot standing on the item collects
        // it within a tick and there is nothing left to render. Age -32768 is
        // INFINITE_LIFETIME so a slow poll cannot lose it to a despawn.
        r.cmd(&format!(
            "summon item {px:.3} {:.3} {pz:.3} \
             {{Item:{{id:\"{ITEM}\",count:1}},PickupDelay:32767s,Age:-32768s}}",
            py + 1.0
        ));
        r.cmd("tick sprint 20");
        (px, py, pz)
    };

    // --- observe it crossing the shell's entity path ---------------------
    let mut interp = EntityInterpolator::new();
    let mut drop: Option<EntityDraw> = None;
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut ever_saw_item_snapshot = false;
    while Instant::now() < deadline {
        let _ = net.poll();
        let snaps = net.entity_snapshots();
        ever_saw_item_snapshot |= snaps
            .iter()
            .any(|s| s.type_path == ITEM_ENTITY_TYPE_PATH);
        interp.update(&snaps, 1.0);
        let nearest = interp
            .draws()
            .into_iter()
            .filter(|d| d.type_path == ITEM_ENTITY_TYPE_PATH)
            .min_by(|a, b| {
                let da = (f64::from(a.feet.x) - px).powi(2) + (f64::from(a.feet.z) - pz).powi(2);
                let db = (f64::from(b.feet.x) - px).powi(2) + (f64::from(b.feet.z) - pz).powi(2);
                da.total_cmp(&db)
            });
        if let Some(d) = nearest
            && (f64::from(d.feet.x) - px).abs() < 2.0
            && (f64::from(d.feet.z) - pz).abs() < 2.0
        {
            drop = Some(d);
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }

    let cleanup = || {
        if let Ok(mut r) = RconClient::connect(RCON_ADDR, RCON_PASSWORD) {
            r.cmd("kill @e[type=item]");
        }
    };

    let drop = drop.unwrap_or_else(|| {
        cleanup();
        panic!(
            "the summoned item entity never crossed the shell's entity path within the \
             timeout (saw an item-typed snapshot at some point: {ever_saw_item_snapshot}). \
             The server accepted the summon, so this is a gap in the entity wiring \
             upstream of the renderer."
        );
    });

    eprintln!("=== live dropped-item gate ===");
    eprintln!("summon point       = ({px:.2}, {py:.2}, {pz:.2})");
    eprintln!(
        "drop entity        = id {} at ({:.2}, {:.2}, {:.2})",
        drop.id, drop.feet.x, drop.feet.y, drop.feet.z
    );
    eprintln!("drop.item (decoded) = {:?}", drop.item);
    eprintln!("age_ticks          = {:.2}", drop.anim.age_ticks);

    // The unfaked half. Both halves of this are load bearing: the entity is
    // tracked (so the drop is not lost upstream), *and* nothing knows what it
    // is (so the invisibility is exactly the metadata gap and not something
    // else).
    assert_eq!(drop.type_path, ITEM_ENTITY_TYPE_PATH);
    assert_eq!(
        drop.item, None,
        "the adapter is not expected to decode the ITEM_STACK metadata serializer \
         yet — if this now reads Some(..), the decode has landed and the \
         set_item_stack call below should be deleted"
    );

    // --- render it -------------------------------------------------------
    let state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
    // The item sits roughly `bob + lift` above its reported position; aim at a
    // point a little above the feet from close range, since a dropped block is
    // only a quarter of a block across.
    let centre = drop.feet + glam::Vec3::new(0.0, 0.2, 0.0);
    let eye = centre + glam::Vec3::new(0.0, 0.35, -1.2);
    let (yaw, pitch) = look_at(eye, centre);
    let camera = Camera {
        position: eye,
        yaw,
        pitch,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    let mut shoot = |draws: &[EntityDraw]| {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, draws);
        (target.read_texels(device, queue), stats.item_drops_drawn)
    };

    // Control, first: the drop exactly as the live client has it — tracked, but
    // with no stack. It must draw nothing.
    let (control, control_drops) = shoot(std::slice::from_ref(&drop));

    // Subject: the same server-sent entity, at the same server-reported
    // position, with the stack the summon used supplied by hand.
    interp.set_item_stack(drop.id, item.clone());
    let with_stack = interp
        .draws()
        .into_iter()
        .find(|d| d.id == drop.id)
        .expect("the drop must still be tracked");
    assert_eq!(
        with_stack.item,
        Some(item.clone()),
        "set_item_stack must reach the draw"
    );
    let (subject, subject_drops) = shoot(std::slice::from_ref(&with_stack));

    cleanup();

    let mut lit = 0usize;
    let mut corner = 0usize;
    for (i, (a, b)) in subject
        .chunks_exact(4)
        .zip(control.chunks_exact(4))
        .enumerate()
    {
        let d = (i32::from(a[0]) - i32::from(b[0])).abs()
            + (i32::from(a[1]) - i32::from(b[1])).abs()
            + (i32::from(a[2]) - i32::from(b[2])).abs();
        if d <= 8 {
            continue;
        }
        lit += 1;
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        if x < W / 4 && y < H / 4 {
            corner += 1;
        }
    }

    eprintln!("item_drops_drawn   = control {control_drops}, subject {subject_drops}");
    eprintln!("lit px vs control  = {lit}");
    eprintln!("lit px, far corner = {corner}");

    assert_eq!(
        control_drops, 0,
        "an item entity with no reported stack must draw nothing"
    );
    assert_eq!(subject_drops, 1, "exactly one drop should have been meshed");
    assert!(
        lit > 300,
        "a server-spawned {ITEM} drop should cover a real run of pixels at 1.2 \
         blocks; only {lit} differ from the no-stack control"
    );
    assert_eq!(
        corner, 0,
        "the corner opposite the drop must be untouched; {corner} differing px \
         there means the count above is not measuring a localised object"
    );

    drop_net(net);
}

/// Explicit teardown so the net thread is closed before the test returns.
fn drop_net(net: NetClient) {
    std::mem::drop(net);
}
