//! Pixel gate: a **filled map hanging in an item frame** reaches pixels, on a
//! wall facing each of the four horizontal directions.
//!
//! # What this covers, and what it deliberately does not
//!
//! This gate installs its own colour grid through
//! [`RenderState::set_map_source`] and builds its own [`EntityDraw`]. It
//! therefore proves the **draw**: that a framed `minecraft:filled_map` becomes a
//! textured quad in front of the frame's own body, and that the quad's front
//! face points out of the wall rather than into it.
//!
//! It proves nothing at all about the two production hops above it — whether
//! `MAP_ITEM_DATA` reaches `SessionMaps`, and whether `Sim::map_source` hands
//! the renderer a non-empty grid. Those are the caller's half of the chain and
//! this fixture bypasses both by construction. Read a pass here as "the renderer
//! draws a map it is given", never as "a map renders".
//!
//! # Why all four yaws, and not one
//!
//! `Ry(yaw)`'s local `+z` and the frame's real `Direction`
//! ([`lodestone_render::entity::item_frame_facing_step`]) **agree at yaw 0 and
//! 180 and are opposite at 90 and 270**. A pose built from the first expression
//! therefore produces a quad whose front face points *into* the wall on every
//! east- and west-facing frame, which the model pipeline's back-face culling
//! discards outright — invisible, with the wider `item_frame_map` body still
//! drawing around the hole. Yaw 90 and 270 are the only inputs that separate the
//! two readings, and this file's siblings had never used one: the unit test in
//! `gpu/maps.rs` asserts the *translation* at all four and says nothing about the
//! orientation.
//!
//! # The arms
//!
//! | arm | what it separates |
//! |---|---|
//! | bare frame → framed map, source installed, at each of four yaws | the map producer paints, whichever wall the frame is on |
//! | framed map with **no source installed** | the diff is the *map*, not the wider `item_frame_map` body |
//! | framed map with an **all-`MapColor.NONE`** grid | a transparent grid draws nothing — the exact shape of "a bigger frame with nothing in it" |
//!
//! The last two are executed negative controls: both must land on the no-source
//! frame's own pixels exactly, and the painted arm must not.
//!
//! Fail-closed like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test framed_map_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::EntityDraw;
use lodestone::gpu::{MapPicture, RenderState};
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

const ITEM: &str = "minecraft:filled_map";
const SUBJECT_ID: i32 = 9201;
const TEST_MAP_ID: i32 = 17;

/// Where the frame entity sits. `ItemFrame.createBoundingBox` puts that behind
/// the centre of the block it hangs in; the render chain steps `0.46875` forward
/// along the frame's own `Direction` to recover the frame's plane.
const SUBJECT_POS: glam::Vec3 = glam::Vec3::new(0.0, 0.25, 0.0);

/// The renderer reconstructs a hanging entity's draw origin from the integer
/// attachment block carried by the spawn packet. Cameras in this fixture must
/// aim at that reconstructed centre rather than at [`SUBJECT_POS`] itself;
/// otherwise an intact map lands half a block off-axis and the localisation
/// assertion mistakes the fixture's bad aim for a stretched picture.
fn subject_centre() -> glam::Vec3 {
    SUBJECT_POS.floor() + glam::Vec3::splat(0.5)
}

/// The four horizontal `ItemFrame.setDirection` yaws — `get2DDataValue() * 90`,
/// i.e. south, west, north, east. All four, because two of them are the only
/// inputs that can see a front face pointing the wrong way.
const YAWS: [f32; 4] = [0.0, 90.0, 180.0, 270.0];

/// A camera two blocks out along the frame's own facing, looking back at it.
///
/// The yaw is `frame_yaw + 180`: this client's camera yaw follows vanilla's, so
/// `0` looks along `+z`, and the frame's outward direction is
/// `(-sin yaw, 0, cos yaw)` — solving `(-sin cam, cos cam) = -(-sin yaw, cos yaw)`
/// gives `cam = yaw + 180`. Derived rather than tabulated so a fifth yaw needs no
/// new constant.
fn camera_for(frame_yaw: f32) -> Camera {
    let out = lodestone_render::entity::item_frame_facing_step(frame_yaw, 0.0);
    let centre = subject_centre();
    Camera {
        position: centre + out * 2.0,
        yaw: frame_yaw + 180.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

/// A camera in the frame's room-facing hemisphere, offset sideways and upward
/// from the head-on proof in [`camera_for`].  A map is a single cullable quad:
/// if its winding is wrong, the head-on arm can still look plausible while an
/// oblique arm vanishes as a complete rectangle rather than showing a depth
/// fight.  Keep the distance fixed so a failure distinguishes facing/culling
/// from the frame body's depth-layer transform.
fn oblique_camera_for(frame_yaw: f32, side: f32, up: f32) -> Camera {
    let outward = lodestone_render::entity::item_frame_facing_step(frame_yaw, 0.0);
    let right = outward.cross(glam::Vec3::Y).normalize();
    let target = subject_centre();
    let position = target + outward * 2.0 + right * side + glam::Vec3::Y * up;
    let direction = (target - position).normalize();
    Camera {
        position,
        yaw: (-direction.x).atan2(direction.z).to_degrees(),
        pitch: (-direction.y).asin().to_degrees(),
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

fn blank_draw(id: i32, type_path: &str, yaw: f32) -> EntityDraw {
    EntityDraw {
        hurt: false,
        block_state: None,
        item_frame_rotation: 0,
        id,
        type_path: std::sync::Arc::from(type_path),
        item: None,
        item_model: None,
        item_skin: None,
        main_arm_left: false,
        feet: SUBJECT_POS,
        yaw,
        head_yaw: yaw,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput::REST,
        equipment: Vec::new(),
        equipment_skin: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_trim: Vec::new(),
        wool: None,
        count: 1,
        foil: false,
        item_dyed_color: None,
        item_potion_color: None,
        name_tag: None,
        item_use: None,
        creeper_swelling: 0.0,
        swim_amount: 0.0,
        death_time: 0.0,
        on_fire: false,
        invisible: false,
        armor_stand: None,
        player_skin: None,
        variant_sheet: None,
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
        painting: None,
        firework: None,
        projectile_owner: None,
    }
}

/// A 128×128 grid of a single **opaque** palette entry — `GRASS` at `HIGH`
/// brightness, packed `1 << 2 | 2`. Deliberately not `0`: id `0` is
/// `MapColor.NONE`, which resolves to alpha `0` and is discarded by the model
/// shader's cutout, so a grid of zeroes could not tell a working draw from a
/// missing one. That is the transparent arm's job, not this one's.
fn grass_grid() -> Vec<u8> {
    vec![0b0000_0110u8; 128 * 128]
}

struct Diff {
    count: usize,
    min_x: u32,
    max_x: u32,
    min_y: u32,
    max_y: u32,
}

fn diff(subject: &[u8], reference: &[u8]) -> Diff {
    let mut count = 0usize;
    let (mut min_x, mut max_x, mut min_y, mut max_y) = (W, 0u32, H, 0u32);
    for (i, (a, b)) in subject
        .chunks_exact(4)
        .zip(reference.chunks_exact(4))
        .enumerate()
    {
        let d = (i32::from(a[0]) - i32::from(b[0])).abs()
            + (i32::from(a[1]) - i32::from(b[1])).abs()
            + (i32::from(a[2]) - i32::from(b[2])).abs();
        if d <= 8 {
            continue;
        }
        let x = (i as u32) % W;
        let y = (i as u32) / W;
        count += 1;
        min_x = min_x.min(x);
        max_x = max_x.max(x);
        min_y = min_y.min(y);
        max_y = max_y.max(y);
    }
    Diff {
        count,
        min_x,
        max_x,
        min_y,
        max_y,
    }
}

struct Shot {
    px: Vec<u8>,
    bodies: usize,
    maps: usize,
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_filled_map_in_an_item_frame_reaches_pixels_on_every_wall() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;

    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "GPU gate opted in but the vanilla pack did not load; set LODESTONE_ASSETS \
             to a pack root with client.jar + generated/reports/blocks.json. Banner: {:?}",
            resources.banner
        )
    });
    let item: ResourceLocation = ITEM.parse().expect("valid item id");

    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));

    let mut shoot = |state: &RenderState, cam: &Camera, draws: &[EntityDraw]| -> Shot {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), cam, None, draws);
        Shot {
            px: target.read_texels(device, queue),
            bodies: stats.item_frame_bodies_drawn,
            maps: stats.filled_maps_drawn,
        }
    };

    // Collected rather than asserted inside the loop: an `assert!` in a loop
    // proves exactly one arm and leaves the rest as arguments instead of
    // observations, which is precisely the thing that hides "three of the four
    // walls are also broken".
    let mut failures: Vec<String> = Vec::new();
    eprintln!("=== framed map pixel gate ===");

    let framed = |yaw: f32| {
        let mut draw = blank_draw(SUBJECT_ID, "item_frame", yaw);
        draw.item = Some(item.clone());
        draw
    };

    // Three passes rather than three shots per yaw, because a `MapSource` is
    // installed and never withdrawn: the no-source reference has to be taken
    // while none has ever been installed. Each pass renders the same four
    // entities from the same four cameras, so the only thing that varies between
    // passes is the picture.
    let no_source: Vec<Shot> = YAWS
        .iter()
        .map(|yaw| shoot(&state, &camera_for(*yaw), std::slice::from_ref(&framed(*yaw))))
        .collect();

    let transparent_pixels = std::sync::Arc::new(vec![0u8; 128 * 128]);
    state.set_map_source(move |_, _| {
        Some(MapPicture::new(
            TEST_MAP_ID,
            0,
            std::sync::Arc::clone(&transparent_pixels),
        ))
    });
    let transparent: Vec<Shot> = YAWS
        .iter()
        .map(|yaw| shoot(&state, &camera_for(*yaw), std::slice::from_ref(&framed(*yaw))))
        .collect();

    let painted_pixels = std::sync::Arc::new(grass_grid());
    state.set_map_source(move |_, _| {
        Some(MapPicture::new(
            TEST_MAP_ID,
            1,
            std::sync::Arc::clone(&painted_pixels),
        ))
    });
    let painted: Vec<Shot> = YAWS
        .iter()
        .map(|yaw| shoot(&state, &camera_for(*yaw), std::slice::from_ref(&framed(*yaw))))
        .collect();

    for (index, yaw) in YAWS.iter().copied().enumerate() {
        let no_source = &no_source[index];
        let transparent = &transparent[index];
        let painted = &painted[index];
        let d_painted = diff(&painted.px, &no_source.px);
        let d_transparent = diff(&transparent.px, &no_source.px);

        eprintln!(
            "yaw {yaw:>5}: bodies {}/{}/{}, maps {}/{}/{}, painted-vs-nosource {} px \
             (bbox x {}..{}, y {}..{}), transparent-vs-nosource {} px",
            no_source.bodies,
            transparent.bodies,
            painted.bodies,
            no_source.maps,
            transparent.maps,
            painted.maps,
            d_painted.count,
            d_painted.min_x,
            d_painted.max_x,
            d_painted.min_y,
            d_painted.max_y,
            d_transparent.count,
        );

        if no_source.bodies != 1 {
            failures.push(format!(
                "yaw {yaw}: the wider `item_frame_map` body must draw whether or not a \
                 picture does — that is the half the owner sees as \"a bigger item \
                 frame\"; got {}",
                no_source.bodies
            ));
        }
        if no_source.maps != 0 {
            failures.push(format!(
                "yaw {yaw}: with no map source installed nothing can be drawn, stats \
                 said {}",
                no_source.maps
            ));
        }
        if painted.maps != 1 {
            failures.push(format!(
                "yaw {yaw}: one framed map quad should have been meshed and drawn, \
                 stats said {}",
                painted.maps
            ));
        }
        if d_painted.count <= 200 {
            failures.push(format!(
                "yaw {yaw}: a one-block picture two blocks away is roughly a 50 px \
                 square; only {} px differ from the same frame with no picture, which \
                 is exactly what \"a bigger item frame with nothing in it\" looks like",
                d_painted.count
            ));
        }
        if d_painted.count > 0 && !(d_painted.min_x > W / 4 && d_painted.max_x < 3 * W / 4) {
            failures.push(format!(
                "yaw {yaw}: the picture must be a localised object near screen centre, \
                 not a smear: x {}..{} of {W}",
                d_painted.min_x, d_painted.max_x
            ));
        }
        if d_transparent.count != 0 {
            failures.push(format!(
                "yaw {yaw}: a grid of `MapColor.NONE` is alpha 0 and the model shader's \
                 cutout discards it, so it must land on the no-source frame exactly; \
                 {} px differ",
                d_transparent.count
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "{} of the framed-map arms failed:\n  {}",
        failures.len(),
        failures.join("\n  ")
    );
}

/// A framed map remains visible from every sampled room-facing oblique view.
///
/// This is deliberately separate from the all-four-wall head-on gate above:
/// that gate proves the four frame transforms, while this one catches the
/// distinct symptom where the map's entire quad is rejected by back-face cull
/// or depth before any of its pixels can reach the target.  The empty-frame
/// comparison makes the assertion about the picture instead of the surrounding
/// `item_frame_map` body.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_framed_map_remains_visible_from_oblique_room_facing_views() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "GPU gate opted in but the vanilla pack did not load; set LODESTONE_ASSETS \
             to a pack root with client.jar + generated/reports/blocks.json. Banner: {:?}",
            resources.banner
        )
    });
    let item: ResourceLocation = ITEM.parse().expect("valid item id");
    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));

    let opaque_pixels = std::sync::Arc::new(grass_grid());
    state.set_map_source(move |_, _| {
        Some(MapPicture::new(
            TEST_MAP_ID,
            0,
            std::sync::Arc::clone(&opaque_pixels),
        ))
    });

    let mut failures = Vec::new();
    for yaw in YAWS {
        let mut framed = blank_draw(SUBJECT_ID, "item_frame", yaw);
        framed.item = Some(item.clone());
        let empty = blank_draw(SUBJECT_ID, "item_frame", yaw);
        for (side, up) in [
            (-0.75, 0.0),
            (0.75, 0.0),
            (0.0, -0.6),
            (0.0, 0.6),
            // These 42° and 54° arms make the depth plane almost edge-on
            // without crossing behind it.  A whole-quad disappearance here
            // is not the ordinary two-triangle z-fight the close arms expose.
            (-1.8, 0.0),
            (1.8, 0.0),
            (-2.8, 0.0),
            (2.8, 0.0),
            (-6.0, 0.0),
            (6.0, 0.0),
        ] {
            let camera = oblique_camera_for(yaw, side, up);
            let frame = target.acquire().expect("headless acquire");
            let empty_px = state
                .render(device, queue, frame.view(), &camera, None, std::slice::from_ref(&empty));
            let empty_px = (target.read_texels(device, queue), empty_px);
            let frame = target.acquire().expect("headless acquire");
            let painted = state.render(
                device,
                queue,
                frame.view(),
                &camera,
                None,
                std::slice::from_ref(&framed),
            );
            let delta = diff(&target.read_texels(device, queue), &empty_px.0);
            if painted.filled_maps_drawn != 1 || delta.count <= 16 {
                failures.push(format!(
                    "yaw {yaw}, side {side}, up {up}: map draw count {}, map-vs-empty-frame {} pixels",
                    painted.filled_maps_drawn, delta.count
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "a room-facing map quad must survive every oblique view; failures:\n{}",
        failures.join("\n")
    );
}

/// Changing only FOV must not make a submitted invisible-frame map disappear.
///
/// This pins the server-scale coordinates and narrow-FOV edge geometry from the
/// live report. The camera pose never changes between arms, so a failure cannot
/// be explained by crossing behind the frame or by a different broad-phase
/// result. Comparing against the same invisible frame without a picture makes
/// this a pixel assertion rather than the weaker submitted-draw counter.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn invisible_framed_map_at_large_coordinates_survives_fixed_pose_fov_changes() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8UnormSrgb;
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "GPU gate opted in but the vanilla pack did not load; set LODESTONE_ASSETS \
             to a pack root with client.jar + generated/reports/blocks.json. Banner: {:?}",
            resources.banner
        )
    });
    let item: ResourceLocation = ITEM.parse().expect("valid item id");
    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
    let opaque_pixels = std::sync::Arc::new(grass_grid());
    state.set_map_source(move |_, _| {
        Some(MapPicture::new(
            TEST_MAP_ID,
            0,
            std::sync::Arc::clone(&opaque_pixels),
        ))
    });

    let frame_pos = glam::Vec3::new(1965.0, 73.0, 3806.0);
    let mut empty = blank_draw(SUBJECT_ID, "glow_item_frame", 90.0);
    empty.feet = frame_pos;
    empty.invisible = true;
    let mut painted = empty.clone();
    painted.item = Some(item);
    let camera_position = glam::Vec3::new(1962.5, 73.5, 3806.5);
    let mut failures = Vec::new();

    for fov_y_degrees in [30.0, 64.0, 110.0] {
        // The map centre is 15 degrees off-axis: near the narrow arm's edge,
        // but still partly inside it. Only FOV changes between these shots.
        let camera = Camera {
            position: camera_position,
            yaw: -75.0,
            pitch: 0.0,
            fov_y_degrees,
            aspect: W as f32 / H as f32,
            near: 0.05,
            far: Camera::far_for_render_distance(8, 0),
        };
        let frame = target.acquire().expect("headless acquire");
        state.render(
            device,
            queue,
            frame.view(),
            &camera,
            None,
            std::slice::from_ref(&empty),
        );
        let empty_px = target.read_texels(device, queue);
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(
            device,
            queue,
            frame.view(),
            &camera,
            None,
            std::slice::from_ref(&painted),
        );
        let delta = diff(&target.read_texels(device, queue), &empty_px);
        eprintln!(
            "FOV {fov_y_degrees}: submitted maps {}, map-vs-empty {} px (bbox x {}..{}, y {}..{})",
            stats.filled_maps_drawn,
            delta.count,
            delta.min_x,
            delta.max_x,
            delta.min_y,
            delta.max_y,
        );
        if stats.filled_maps_drawn != 1 || delta.count <= 16 {
            failures.push(format!(
                "FOV {fov_y_degrees}: submitted {}, map-vs-empty {} pixels",
                stats.filled_maps_drawn, delta.count
            ));
        }
    }

    assert!(
        failures.is_empty(),
        "fixed-pose invisible map disappeared as only FOV changed:\n{}",
        failures.join("\n")
    );
}
