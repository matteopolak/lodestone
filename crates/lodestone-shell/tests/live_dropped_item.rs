//! Live gate: an item entity the **server** spawned must reach the shell's
//! entity path, must arrive knowing *which item it is*, and must reach pixels.
//!
//! ```text
//! /summon item → ADD_ENTITY + SET_ENTITY_DATA
//!   → v770 metadata decode (index 8, SER_ITEM_STACK)
//!   → EntityMetadataUpdate::item → EntityView::item
//!   → NetClient::entities()  (type_path == "item", item == Some(..))
//!   → apply_view (ingest components) → EntityInterpolator → EntityDraw
//!   → RenderState::render → GPU pixels
//! ```
//!
//! # Nothing here is faked
//!
//! This test used to call [`EntityInterpolator::set_item_stack`] by hand,
//! standing in for a metadata decode the 26.2 adapter refused to do. That decode
//! now exists and the whole chain above is wired, so the hand-supplied identity
//! is gone: the item id asserted below is the one that came off the wire. If the
//! chain regresses anywhere — adapter, read-model fold, snapshot lowering, or
//! the interpolator — this reads `None` and fails, which is exactly what it did
//! before the chain was closed.
//!
//! # Two items, because they answer different questions
//!
//! * **`minecraft:diamond_block`** — a full block item, so it bakes to real 3-D
//!   geometry and its silhouette is the least ambiguous thing available. This is
//!   the one the pixel assertions use.
//! * **`minecraft:diamond`** — a flat `item/generated` sprite, which reaches
//!   `BlockModels::items` through a *different* baking path: `IconPart::Sprite`'s
//!   layer stack extruded into vanilla's thin slab by
//!   `extruded_sprite_geometry`, rather than `IconPart::Model`'s baked cuboids.
//!   Both land in the same map under the same key, so the drop pass cannot tell
//!   them apart — which is exactly the property worth pinning, because it was
//!   *not* true before `9980a96` and this assertion used to read
//!   `sprite_drops == 0`. Keeping both items here is what makes a regression in
//!   either baking path localise to one of them.
//!
//! Per §12.52 this fails rather than skips when it cannot run.
//!
//! ```text
//! cargo test -p lodestone-shell --features live --test live_dropped_item -- --ignored --nocapture
//! ```
#![cfg(feature = "live")]

use std::time::{Duration, Instant};

use lodestone::net::NetClient;
use lodestone::entities::{EntityDraw, EntityInterpolator, ITEM_ENTITY_TYPE_PATH};
use lodestone::gpu::RenderState;
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_client::EntityView;
use lodestone_ecs::entity::{
    CreeperSwellDir, CustomName, CustomNameVisible, DisplayItem, Equipment, EntityFlags,
    EntityIndex, EntityKind, EntityUuid, HeadYaw, MinecraftEntityId, OnGround, Position, Rotation,
    Variant, Velocity,
};
use lodestone_model::Reported;
use lodestone_render::{Camera, GpuContext, HeadlessTarget, RenderTarget};
use lodestone_testsupport::{RconClient, unique_username};

/// Translates a raw, version-free [`EntityView`] into the ingest components
/// the live path itself reads (`entities.rs`'s `resolve_entity_facts`),
/// upserting `view.entity_id`'s ingest entity in `world` — that fix's
/// replacement for feeding a hand-built `EntitySnapshot` straight to a
/// now-deleted `fold_entity_snapshots`. Identical to
/// `live_entity_render.rs`'s own `apply_view` — see that copy's doc for why
/// this is duplicated rather than shared: [`EntityInterpolator::new`]
/// installs no `IngestPlugin`, so there is no production "apply this whole
/// view" entry point either file could call instead.
fn apply_view(world: &mut bevy_ecs::world::World, view: &EntityView) {
    let entity = match world.resource::<EntityIndex>().get(view.entity_id) {
        Some(existing) => existing,
        None => {
            let entity = world.spawn(MinecraftEntityId(view.entity_id)).id();
            world.resource_mut::<EntityIndex>().insert(view.entity_id, entity);
            entity
        }
    };
    let mut e = world.entity_mut(entity);
    e.insert((
        EntityKind(view.entity_type.clone()),
        Position(view.position),
        Rotation(view.rotation),
        HeadYaw(view.head_yaw),
        OnGround(view.on_ground),
        Equipment(view.equipment.clone()),
    ));
    if let Some(uuid) = view.uuid {
        e.insert(EntityUuid(uuid));
    }
    if let Some(v) = view.velocity {
        e.insert(Velocity(v));
    }
    if let Reported::Reported(item) = &view.item {
        e.insert(DisplayItem(item.clone()));
    }
    if let Some(variant) = &view.variant {
        e.insert(Variant(variant.clone()));
    }
    if let Some(dir) = view.creeper_swell_dir {
        e.insert(CreeperSwellDir(dir));
    }
    if let Reported::Reported(name) = &view.custom_name {
        e.insert(CustomName(name.clone()));
    }
    if let Some(visible) = view.custom_name_visible {
        e.insert(CustomNameVisible(visible));
    }
    if let Some(flags) = view.flags {
        e.insert(EntityFlags(flags));
    }
}

const GAME_HOST: &str = "127.0.0.1";
const GAME_PORT: u16 = 25565;
const RCON_ADDR: &str = "127.0.0.1:25566";
const RCON_PASSWORD: &str = "lodestone";
const PROTOCOL_26_2: i32 = 776;

/// A full block item, so the drawn geometry is a solid cube rather than a flat
/// sprite standing on edge — the biggest, least ambiguous silhouette available.
const BLOCK_ITEM: &str = "minecraft:diamond_block";
/// A flat `item/generated` item, to measure the other stream.
const SPRITE_ITEM: &str = "minecraft:diamond";

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

fn rcon() -> RconClient {
    RconClient::connect(RCON_ADDR, RCON_PASSWORD).expect(
        "oracle RCON reachable at 127.0.0.1:25566 — a missing RCON is a harness \
         failure, not a passing render path",
    )
}

/// Clear the ground and drop one `item` on the player, returning where.
///
/// The summon uses the *player's own* position read back over RCON rather than
/// `~ ~ ~`: a bare relative summon resolves against the console's origin, which
/// puts the drop outside the bot's view distance, and an entity the client never
/// tracks sends no metadata at all.
///
/// `PickupDelay` 32767 is the never-pick-up sentinel (`ItemLifecycle`'s
/// `NEVER_PICKUP_DELAY`): without it the bot standing on the item collects it
/// within a tick and there is nothing left to read. `Age` −32768 is
/// `INFINITE_LIFETIME`, so a slow poll cannot lose it to a despawn.
fn summon_drop(item: &str) -> (f64, f64, f64) {
    let mut r = rcon();
    let (px, py, pz) =
        parse_list3(&r.cmd("data get entity @p Pos")).expect("player Pos readable via RCON");
    r.cmd(&format!(
        "forceload add {} {}",
        px.floor() as i64,
        pz.floor() as i64
    ));
    r.cmd("kill @e[type=item]");
    r.cmd(&format!(
        "summon item {px:.3} {:.3} {pz:.3} \
         {{Item:{{id:\"{item}\",count:1}},PickupDelay:32767s,Age:-32768s}}",
        py + 1.0
    ));
    r.cmd("tick sprint 20");
    (px, py, pz)
}

/// Poll the live client until an item entity shows up within two blocks of
/// `(px, pz)`, ignoring `exclude` (the previous phase's drop, which may still be
/// in flight when the next one is summoned).
fn wait_for_drop(
    net: &NetClient,
    interp: &mut EntityInterpolator,
    px: f64,
    pz: f64,
    exclude: Option<i32>,
) -> Option<EntityDraw> {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let _ = net.poll();
        for view in net.entities() {
            apply_view(interp.world_mut(), &view);
        }
        interp.update(1.0);
        let nearest = interp
            .draws()
            .into_iter()
            .filter(|d| d.type_path.as_ref() == ITEM_ENTITY_TYPE_PATH && Some(d.id) != exclude)
            .min_by(|a, b| {
                let da = (f64::from(a.feet.x) - px).powi(2) + (f64::from(a.feet.z) - pz).powi(2);
                let db = (f64::from(b.feet.x) - px).powi(2) + (f64::from(b.feet.z) - pz).powi(2);
                da.total_cmp(&db)
            });
        // The identity rides a *separate* packet from the spawn, so a drop can
        // be tracked a poll or two before its stack lands. Waiting for the item
        // is what makes the assertion below about the decode rather than about
        // poll timing — the timeout still fails if it never arrives.
        if let Some(d) = nearest
            && (f64::from(d.feet.x) - px).abs() < 2.0
            && (f64::from(d.feet.z) - pz).abs() < 2.0
            && d.item.is_some()
        {
            return Some(d);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    None
}

#[test]
#[ignore = "requires the live vanilla-26.2 oracle on :25565 (+ RCON :25566), a GPU adapter and client.jar"]
fn a_server_spawned_drop_knows_which_item_it_is_and_reaches_pixels() {
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
    let block_item: ResourceLocation = BLOCK_ITEM.parse().expect("valid item id");
    let sprite_item: ResourceLocation = SPRITE_ITEM.parse().expect("valid item id");

    // --- connect ---------------------------------------------------------
    // `connect_as`, not `connect`: a live gate needs a fresh identity per run
    // (a shared offline name is a shared player file, and a dead player is held
    // on the death screen, which sends no chunks). `connect` is the *stable*
    // persisted offline identity, which is production's job, not a gate's.
    let net = NetClient::connect_as(GAME_HOST.to_owned(), GAME_PORT, PROTOCOL_26_2, None, unique_username());
    let ready = Instant::now() + Duration::from_secs(25);
    let mut in_world = false;
    while Instant::now() < ready {
        let _ = net.poll();
        if !net.loaded_chunks().is_empty() || !net.entities().is_empty() {
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

    let cleanup = || {
        if let Ok(mut r) = RconClient::connect(RCON_ADDR, RCON_PASSWORD) {
            r.cmd("kill @e[type=item]");
        }
    };

    // --- phase 1: a 3-D-modelled drop ------------------------------------
    let (px, py, pz) = summon_drop(BLOCK_ITEM);
    let mut interp = EntityInterpolator::new();
    let drop = wait_for_drop(&net, &mut interp, px, pz, None).unwrap_or_else(|| {
        cleanup();
        panic!(
            "the summoned {BLOCK_ITEM} never crossed the shell's entity path carrying an \
             item id within the timeout. The server accepted the summon, so this is a gap \
             in the entity/metadata wiring upstream of the renderer."
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

    assert_eq!(drop.type_path.as_ref(), ITEM_ENTITY_TYPE_PATH);
    // The whole point of the gate: the id came off the wire, nothing here
    // supplied it. Before the metadata chain was closed this read `None`.
    assert_eq!(
        drop.item,
        Some(block_item.clone()),
        "the drop must arrive knowing it is a {BLOCK_ITEM} — a `None` here means the \
         ITEM_STACK metadata never reached EntityDraw, and a different id means it \
         reached it wrong"
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

    // Control: the *same* server-sent entity at the same server-reported
    // position with its identity taken away. It must draw nothing, which is what
    // makes the pixel count below attributable to the decoded item and not to
    // the terrain, the sky, or the drop merely existing.
    let blank = EntityDraw {
        item: None,
        ..drop.clone()
    };
    let (control, control_drops) = shoot(std::slice::from_ref(&blank));
    let (subject, subject_drops) = shoot(std::slice::from_ref(&drop));

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
        "a server-spawned {BLOCK_ITEM} drop should cover a real run of pixels at 1.2 \
         blocks; only {lit} differ from the no-stack control"
    );
    assert_eq!(
        corner, 0,
        "the corner opposite the drop must be untouched; {corner} differing px \
         there means the count above is not measuring a localised object"
    );

    // --- phase 2: a flat sprite item -------------------------------------
    // Same chain, same assertions about identity; the difference is what the
    // renderer can do with it.
    let (px, _, pz) = summon_drop(SPRITE_ITEM);
    let sprite_drop =
        wait_for_drop(&net, &mut interp, px, pz, Some(drop.id)).unwrap_or_else(|| {
            cleanup();
            panic!("the summoned {SPRITE_ITEM} never arrived with an item id");
        });
    eprintln!("sprite drop.item   = {:?}", sprite_drop.item);
    assert_eq!(
        sprite_drop.item,
        Some(sprite_item),
        "a flat sprite item's identity decodes exactly like a block item's"
    );

    let (_, sprite_drops) = shoot(std::slice::from_ref(&sprite_drop));
    eprintln!("sprite item_drops_drawn = {sprite_drops}");
    assert_eq!(
        sprite_drops, 1,
        "a flat `item/generated` sprite must mesh exactly like a block item does: \
         `BlockModels::build` extrudes its layer stack into vanilla's thin slab \
         (`extruded_sprite_geometry`) and inserts it into the *same* item map, so \
         the drop pass never learns which of the two baking paths produced the \
         geometry. Zero here means the extrusion loop found no stitched layer for \
         {SPRITE_ITEM} — check `BlockModels::item_bake_misses`, not this chain."
    );
    // Deliberately **no pixel assertion for the sprite drop**: the camera above is
    // aimed at the block item's summon position, and the two are summoned at
    // different coordinates, so the sprite drop is not in this frame at all. Adding
    // a "differing pixels > 0" check here would be a *world*-species vacuous test —
    // pointed at a scene that structurally cannot contain the subject. The pixel
    // evidence for the extruded slab lives in `lodestone-render`'s
    // `sprite_drop_pixels`, which renders it and correlates the silhouette against
    // the sprite's own alpha profile read out of the atlas.

    cleanup();
    drop_net(net);
}

/// Explicit teardown so the net thread is closed before the test returns.
fn drop_net(net: NetClient) {
    std::mem::drop(net);
}
