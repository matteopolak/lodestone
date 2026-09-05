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
use lodestone_game::maps::MapId;
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

const ITEM: &str = "minecraft:filled_map";
const SUBJECT_ID: i32 = 9201;
const TEST_MAP_ID: i32 = 17;

fn test_map_id() -> MapId {
    MapId::new(TEST_MAP_ID).expect("the fixture map id is non-negative")
}

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
            test_map_id(),
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
            test_map_id(),
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
            test_map_id(),
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
            test_map_id(),
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

// ---------------------------------------------------------------------------
// The wall arm
// ---------------------------------------------------------------------------
//
// Every arm above renders against an **empty world**: no attachment wall, no
// terrain, nothing in the depth buffer at all. A framed map's whole physical
// separation from the surface behind it is `1.01 / 128` of a block — 7.9 mm —
// so a fixture with nothing behind the quad cannot observe the one contest the
// live defect is about. This is the shared-fixture blind spot `CLAUDE.md`
// describes: no gate here is badly written, the corpus simply never puts the
// subject in the state the feature exists for.
//
// `world_text_over_geometry_pixels.rs` found the same gap for sign text and is
// the template for this arm.
//
// Two fixture origins, because the live report is from a server whose build is
// thousands of blocks from the origin and every gate here sits at `(0, 0)`:
// `map_quad_mesh` bakes **absolute** world positions into `f32` vertices while
// terrain reaches the same shader through a section-relative origin, so the two
// only share a quantisation grid near zero.

use lodestone::mesher::{
    SectionGeometry, SectionKey, mesh_snapshot_models, snapshot_section, snapshot_visibility,
};
use lodestone_render::ModelMesh;
use lodestone_world::{
    ChunkColumn, ChunkPos, ColumnLight, Heightmaps, LoadedChunk, PaletteKind, World,
};

/// Vertical extent of the fixture world, in sections from `y = 0`.
const WALL_SECTION_COUNT: usize = 8;
const WALL_MIN_Y: i32 = 0;
/// Solid ground below the frame, so a downward-looking camera sees terrain
/// rather than sky and a lost map cannot be confused with a clipped one.
const WALL_GROUND_Y: i32 = 64;

/// A game tick past `SECTION_FADE_DURATION_SECS`. A section uploaded before any
/// `update_animation` resolves `section_visibility == 0` and renders as flat fog
/// colour, which leaves its depth intact but makes every colour claim vacuous.
const WALL_FADED_IN_TICK: u64 = 40;

/// The **air** block an item frame hangs in, for each fixture origin. The
/// second is the DemocracyCraft build the live trace was captured in, to the
/// nearest block — the axis every existing gate holds at zero.
const WALL_ORIGINS: [(&str, [i32; 3]); 2] = [
    ("near origin", [0, 66, 0]),
    ("live coordinates", [1970, 73, 3811]),
];

fn wall_state_id(state: &str) -> u32 {
    lodestone_data::block_states::state_id(state)
        .unwrap_or_else(|| panic!("{state} is not in the 26.2 block-state table"))
}

/// Ground, plus — when `with_wall` — the stone the frame is attached to.
///
/// The frame's yaw is `0`, so `item_frame_facing_step` is `+z` and the
/// attachment block is the one at `-z` from `block`. An invisible frame's map
/// plane lands `1.01 / 128` outside that block's face; a visible one lands the
/// same distance in front of its own body's front plate, which is itself `1/16`
/// clear of the wall.
fn wall_world(block: [i32; 3], with_wall: bool, sky_light: u8) -> World {
    let air = wall_state_id("minecraft:air");
    let stone = wall_state_id("minecraft:stone");
    let (cx0, cz0) = (block[0] >> 4, block[2] >> 4);
    let mut world = World::new();
    for cx in cx0 - 1..=cx0 + 1 {
        for cz in cz0 - 1..=cz0 + 1 {
            let column = ChunkColumn::new(
                WALL_MIN_Y,
                WALL_SECTION_COUNT,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                air,
                0,
            );
            let mut light = ColumnLight::new(WALL_SECTION_COUNT);
            for i in 0..light.light_section_count() {
                *light.sky_mut(i) = lodestone_world::LightData::Uniform(sky_light);
                *light.block_mut(i) = lodestone_world::LightData::Uniform(0);
            }
            world.load(
                ChunkPos::new(cx, cz),
                LoadedChunk::new(column, light, Heightmaps::new(), Vec::new()),
            );
        }
    }
    let (x0, z0) = (cx0 * 16, cz0 * 16);
    let ground = world.fill_region(
        [x0 - 16, WALL_MIN_Y, z0 - 16],
        [x0 + 31, WALL_GROUND_Y, z0 + 31],
        stone,
    );
    assert!(ground > 0, "fixture: the ground must actually write blocks");
    if with_wall {
        let z = block[2] - 1;
        let filled = world.fill_region(
            [block[0] - 8, WALL_GROUND_Y + 1, z],
            [block[0] + 7, WALL_GROUND_Y + 12, z],
            stone,
        );
        assert!(filled > 0, "fixture: the attachment wall must write blocks");
    }
    world
}

fn wall_upload(
    world: &World,
    block: [i32; 3],
    models: &lodestone_render::BlockModels,
    state: &mut RenderState,
    device: &wgpu::Device,
    queue: &wgpu::Queue,
) {
    let (cx0, cz0) = (block[0] >> 4, block[2] >> 4);
    let mut uploaded = 0usize;
    for cx in cx0 - 1..=cx0 + 1 {
        for cz in cz0 - 1..=cz0 + 1 {
            for si in 0..WALL_SECTION_COUNT {
                let key = SectionKey { cx, cz, si, min_y: WALL_MIN_Y };
                let Some(snap) = snapshot_section(world, key) else {
                    continue;
                };
                let opaque = mesh_snapshot_models(&snap, models, false);
                let visibility = snapshot_visibility(&snap, models);
                state.upload_section(
                    device,
                    queue,
                    key,
                    &SectionGeometry::Model {
                        opaque,
                        water: ModelMesh::default(),
                        translucent_blocks: ModelMesh::default(),
                        visibility,
                    },
                );
                uploaded += 1;
            }
        }
    }
    assert!(uploaded > 0, "fixture: some sections must have uploaded");
    state.update_animation(queue, WALL_FADED_IN_TICK);
}

/// A camera `distance` blocks from the frame's attachment-block centre, turned
/// `degrees` around the wall's own up axis away from the frame normal.
///
/// Obliquity is an **angle**, not a sideways offset in blocks: the live defect
/// is reported as view-dependent, and only an angle means the same entry
/// describes the same view at every range. The eye is lifted a fixed tenth of
/// the distance so the sweep is never exactly in the frame's own plane.
fn wall_camera(block: [i32; 3], distance: f32, degrees: f32) -> Camera {
    let outward = lodestone_render::entity::item_frame_facing_step(0.0, 0.0);
    let right = outward.cross(glam::Vec3::Y).normalize();
    let target = glam::Vec3::new(
        block[0] as f32 + 0.5,
        block[1] as f32 + 0.5,
        block[2] as f32 + 0.5,
    );
    let radians = degrees.to_radians();
    let offset = (outward * radians.cos() + right * radians.sin() + glam::Vec3::Y * 0.1)
        .normalize()
        * distance;
    let position = target + offset;
    let direction = (target - position).normalize();
    Camera {
        position,
        yaw: (-direction.x).atan2(direction.z).to_degrees(),
        pitch: (-direction.y).asin().to_degrees(),
        // The live capture's own field of view, not this file's 60: a wider
        // lens both shrinks the subject and changes the projected depth slope
        // the polygon offset is measured against.
        fov_y_degrees: 110.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(12, 0),
    }
}

/// `(name, distance in blocks, degrees off the frame normal)`. The obliquities
/// go to `85°` because the live trace's own screen-centre frames sat around
/// `78°` off normal, where the map's `7.9 mm` normal separation collapses to
/// under `2 mm` measured along the view direction.
const WALL_VIEWS: [(&str, f32, f32); 9] = [
    ("2m head-on", 2.0, 0.0),
    ("2m 45deg", 2.0, 45.0),
    ("2m 75deg", 2.0, 75.0),
    ("2m 85deg", 2.0, 85.0),
    ("8m head-on", 8.0, 0.0),
    ("8m 45deg", 8.0, 45.0),
    ("8m 75deg", 8.0, 75.0),
    ("24m head-on", 24.0, 0.0),
    ("24m 45deg", 24.0, 45.0),
];

/// Pixel gate: a framed map must survive the depth test **against the wall it
/// hangs on and against its own frame body**, at every obliquity and range, at
/// the origin and at real server coordinates.
///
/// The measurement is per configuration: ink is the pixel count where a painted
/// map differs from the very same scene rendered before any `MapSource` was
/// installed. The wall-free world is the positive control — the same scene with
/// the attachment block removed, so its ink is what the map draws when nothing
/// can contest it, and the walled ink must match.
///
/// Both frame kinds are measured because they contest different surfaces: a
/// visible `item_frame`'s map sits `1.01 / 128` in front of its own body's
/// front plate, while an invisible `glow_item_frame` has no body at all and its
/// map sits the same distance outside the attachment block's face.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_framed_map_survives_the_depth_test_against_its_attachment_wall() {
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
    let models = atlas.models().expect("vanilla atlas must carry baked models");
    let item: ResourceLocation = ITEM.parse().expect("valid item id");

    let mut target = HeadlessTarget::new(device, W, H, format);

    let kinds: [(&str, &str, bool); 2] = [
        ("visible item_frame", "item_frame", false),
        ("invisible glow_item_frame", "glow_item_frame", true),
    ];

    // One `RenderState` per (origin, world): a `MapSource` cannot be withdrawn
    // once installed, so every no-map reference shot has to be taken before the
    // source goes in.
    let mut ink_for = |block: [i32; 3], with_wall: bool| -> Vec<usize> {
        let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
        state.set_third_person_body_source(|| {
            Some(lodestone::gpu::ThirdPersonBodyState {
                player_skin: None,
                feet: glam::Vec3::new(0.0, -10_000.0, 0.0),
                body_yaw_deg: 0.0,
                anim: AnimInput::default(),
                scale: 1.0,
                swim_amount: 0.0,
                slim: false,
                equipment: Vec::new(),
                equipment_skin: Vec::new(),
            })
        });
        wall_upload(
            &wall_world(block, with_wall, 15),
            block,
            models,
            &mut state,
            device,
            queue,
        );

        let frame_draw = |type_path: &str, invisible: bool| {
            let mut draw = blank_draw(SUBJECT_ID, type_path, 0.0);
            // The wire carries a hanging entity's own position, which
            // `ItemFrame.createBoundingBox` puts `0.46875` back along its
            // facing from the attachment block's centre. Feed the renderer that
            // rather than the block, so the `floor()` in `item_frame_space` is
            // exercised the way production exercises it.
            draw.feet = glam::Vec3::new(
                block[0] as f32 + 0.5,
                block[1] as f32 + 0.5,
                block[2] as f32 + 0.5,
            ) - lodestone_render::entity::item_frame_facing_step(0.0, 0.0) * 0.46875;
            draw.item = Some(item.clone());
            draw.invisible = invisible;
            draw
        };

        let mut shoot = |state: &RenderState, cam: &Camera, draw: &EntityDraw| -> Vec<u8> {
            let frame = target.acquire().expect("headless acquire");
            state.render(device, queue, frame.view(), cam, None, std::slice::from_ref(draw));
            target.read_texels(device, queue)
        };

        let mut references = Vec::new();
        for (_, type_path, invisible) in kinds {
            let draw = frame_draw(type_path, invisible);
            for (_, distance, degrees) in WALL_VIEWS {
                references.push(shoot(&state, &wall_camera(block, distance, degrees), &draw));
            }
        }

        let painted = std::sync::Arc::new(grass_grid());
        state.set_map_source(move |_, _| {
            Some(MapPicture::new(
                test_map_id(),
                1,
                std::sync::Arc::clone(&painted),
            ))
        });

        let mut ink = Vec::new();
        let mut index = 0usize;
        for (_, type_path, invisible) in kinds {
            let draw = frame_draw(type_path, invisible);
            for (_, distance, degrees) in WALL_VIEWS {
                let shot = shoot(&state, &wall_camera(block, distance, degrees), &draw);
                ink.push(diff(&shot, &references[index]).count);
                index += 1;
            }
        }
        ink
    };

    eprintln!("=== framed map vs attachment wall ===");
    let mut failures: Vec<String> = Vec::new();
    for (origin, block) in WALL_ORIGINS {
        let free = ink_for(block, false);
        let walled = ink_for(block, true);
        let mut index = 0usize;
        for (kind, _, _) in kinds {
            for (view, ..) in WALL_VIEWS {
                let (no_wall, wall) = (free[index], walled[index]);
                eprintln!(
                    "{origin:<17} {kind:<26} {view:<11} no-wall {no_wall:>6} px   walled {wall:>6} px"
                );
                if no_wall == 0 {
                    failures.push(format!(
                        "{origin} / {kind} / {view}: the wall-free control drew no map at \
                         all, so the walled measurement beside it is vacuous"
                    ));
                } else if wall * 20 < no_wall * 19 {
                    failures.push(format!(
                        "{origin} / {kind} / {view}: the map lost {} of {no_wall} px to the \
                         geometry behind it ({wall} px survived)",
                        no_wall - wall
                    ));
                }
                index += 1;
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Pixel gate: a **glow** item frame lights its own map, a plain one does not.
///
/// This is the wiring half of `gpu/maps.rs`'s `framed_map_light`. That helper
/// has a unit test of its own, which proves the arithmetic and nothing about
/// whether the framed-map producer calls it — the exact island shape this
/// subsystem has already shipped once, where the frame's registry-path doc
/// described a full-bright content light that only the framed-*item* path
/// implemented.
///
/// The scene is deliberately unlit (`sky = 0`, `block = 0`) with the map hung
/// on a real stone wall, because that is the only input where the two branches
/// differ: under sky 15 both draw the same bright picture and the gate is
/// vacuous. The plain frame is therefore not a decoration — it is the negative
/// control, and reverting the producer to the sampled light makes the two
/// arms equal, which is what this assertion measures.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn only_a_glow_framed_map_lights_itself_in_an_unlit_room() {
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
    let models = atlas.models().expect("vanilla atlas must carry baked models");
    let item: ResourceLocation = ITEM.parse().expect("valid item id");
    let block = WALL_ORIGINS[0].1;

    let mut target = HeadlessTarget::new(device, W, H, format);
    let mut state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
    state.set_third_person_body_source(|| {
        Some(lodestone::gpu::ThirdPersonBodyState {
            player_skin: None,
            feet: glam::Vec3::new(0.0, -10_000.0, 0.0),
            body_yaw_deg: 0.0,
            anim: AnimInput::default(),
            scale: 1.0,
            swim_amount: 0.0,
            slim: false,
            equipment: Vec::new(),
            equipment_skin: Vec::new(),
        })
    });
    // Without this the renderer's entity light source is unset, and an unset
    // source answers full bright everywhere — under which both arms below draw
    // the same bright picture and the gate measures nothing. That is the
    // fixture's own vacuity trap and it fired on the first run.
    state.set_entity_light_source(|_| Some(0x00));
    wall_upload(
        &wall_world(block, true, 0),
        block,
        models,
        &mut state,
        device,
        queue,
    );
    let painted = std::sync::Arc::new(grass_grid());
    state.set_map_source(move |_, _| {
        Some(MapPicture::new(
            test_map_id(),
            1,
            std::sync::Arc::clone(&painted),
        ))
    });

    let camera = wall_camera(block, 2.0, 0.0);
    // A patch well inside the map's own silhouette. At two blocks under this
    // fixture's 110-degree lens the picture is ~34 px across, so a 12 px box on
    // the frame's projected centre cannot reach the wall behind it.
    let centre = glam::Vec3::new(
        block[0] as f32 + 0.5,
        block[1] as f32 + 0.5,
        block[2] as f32 + 0.5,
    );
    let clip = camera.view_projection() * centre.extend(1.0);
    assert!(clip.w > 0.0, "fixture: the frame must be in front of the camera");
    let cx = ((clip.x / clip.w * 0.5 + 0.5) * W as f32) as i32;
    let cy = ((1.0 - (clip.y / clip.w * 0.5 + 0.5)) * H as f32) as i32;

    let mut mean_for = |type_path: &str| -> f64 {
        let mut draw = blank_draw(SUBJECT_ID, type_path, 0.0);
        draw.feet = centre
            - lodestone_render::entity::item_frame_facing_step(0.0, 0.0) * 0.46875;
        draw.item = Some(item.clone());
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(
            device,
            queue,
            frame.view(),
            &camera,
            None,
            std::slice::from_ref(&draw),
        );
        assert!(
            stats.filled_maps_drawn > 0,
            "{type_path}: the map must reach the draw before its brightness means anything"
        );
        let px = target.read_texels(device, queue);
        let mut total = 0f64;
        let mut count = 0usize;
        for y in cy - 6..cy + 6 {
            for x in cx - 6..cx + 6 {
                let i = (y as usize * W as usize + x as usize) * 4;
                total += f64::from(px[i]) + f64::from(px[i + 1]) + f64::from(px[i + 2]);
                count += 3;
            }
        }
        total / count as f64
    };

    let plain = mean_for("item_frame");
    let glow = mean_for("glow_item_frame");
    eprintln!("=== unlit framed map brightness ===");
    eprintln!("plain item_frame      mean channel {plain:.1}");
    eprintln!("glow_item_frame       mean channel {glow:.1}");
    assert!(
        glow > plain * 2.0,
        "a glow frame must light its own map: glow {glow:.1} against plain {plain:.1} \
         (equal means the producer is sampling world light for both)"
    );
}

// ---------------------------------------------------------------------------
// The map *board*: many invisible framed maps on one wall, seen from a camera
// that turns in place.
// ---------------------------------------------------------------------------

/// The live report this arm exists for is a wall-sized grid of `filled_map`s in
/// invisible item frames that is **cut off along a straight, screen-parallel
/// line**, with the line moving as the camera turns and *not* moving as the
/// camera walks. Three live switch runs bound the mechanism before this fixture
/// was written: `LODESTONE_MAP_DISABLE_DEPTH_TEST=1` removes the defect
/// entirely, `LODESTONE_MAP_DISABLE_DEPTH_WRITE=1` changes nothing (so the
/// tiles are not occluding each other), and `LODESTONE_MAP_DISABLE_DEPTH_BIAS=1`
/// makes it *worse* — the whole board goes. So the picture is losing a depth
/// comparison against the wall behind it, and the polygon offset is the only
/// thing keeping any of it.
///
/// Every other gate in this file renders **one** frame from a **translating**
/// camera. Those are the two axes the report is about, so this one holds the
/// eye still and turns it, over a board wide enough to run past the screen edge
/// at the narrow field of view and not at the wide one.
const BOARD_HALF_COLUMNS: i32 = 17;
const BOARD_HALF_ROWS: i32 = 9;
/// The frames' own air blocks: the DemocracyCraft build's coordinates, lifted
/// clear of `WALL_GROUND_Y` so the board is not half buried in the floor.
const BOARD_BLOCK: [i32; 3] = [1970, 76, 3811];

/// The board gate renders at its **own** framebuffer size, read from
/// `LODESTONE_BOARD_GATE_SIZE=<width>x<height>` and defaulting to this file's
/// `W`/`H`.
///
/// Resolution is not cosmetic here. The map's only guaranteed margin over the
/// wall is a polygon offset whose slope term is `slope_scale * m`, where `m` is
/// the depth gradient **per pixel**; doubling the framebuffer width halves `m`
/// and so halves that half of the rescue. Every other gate in this file renders
/// at 320x240, which gives the map roughly eight times the slope rescue a
/// 2560-wide display gives it — so a fixture pinned to 320x240 is measuring a
/// more forgiving device than the one the report came from.
fn board_size() -> (u32, u32) {
    std::env::var("LODESTONE_BOARD_GATE_SIZE")
        .ok()
        .and_then(|spec| {
            let (w, h) = spec.split_once('x')?;
            Some((w.trim().parse().ok()?, h.trim().parse().ok()?))
        })
        .unwrap_or((W, H))
}

/// Ground, plus — when `with_wall` — the whole slab of stone the board hangs
/// on, one block behind the frames' own air column.
fn board_world(with_wall: bool) -> World {
    let air = wall_state_id("minecraft:air");
    let stone = wall_state_id("minecraft:stone");
    let (cx0, cz0) = (BOARD_BLOCK[0] >> 4, BOARD_BLOCK[2] >> 4);
    let mut world = World::new();
    for cx in cx0 - 1..=cx0 + 1 {
        for cz in cz0 - 1..=cz0 + 1 {
            let column = ChunkColumn::new(
                WALL_MIN_Y,
                WALL_SECTION_COUNT,
                PaletteKind::block_states(),
                PaletteKind::biomes(),
                air,
                0,
            );
            let mut light = ColumnLight::new(WALL_SECTION_COUNT);
            for i in 0..light.light_section_count() {
                *light.sky_mut(i) = lodestone_world::LightData::Uniform(15);
                *light.block_mut(i) = lodestone_world::LightData::Uniform(0);
            }
            world.load(
                ChunkPos::new(cx, cz),
                LoadedChunk::new(column, light, Heightmaps::new(), Vec::new()),
            );
        }
    }
    let (x0, z0) = (cx0 * 16, cz0 * 16);
    let ground = world.fill_region(
        [x0 - 16, WALL_MIN_Y, z0 - 16],
        [x0 + 31, WALL_GROUND_Y, z0 + 31],
        stone,
    );
    assert!(ground > 0, "fixture: the ground must actually write blocks");
    if with_wall {
        let filled = world.fill_region(
            [
                BOARD_BLOCK[0] - BOARD_HALF_COLUMNS - 1,
                BOARD_BLOCK[1] - BOARD_HALF_ROWS - 1,
                BOARD_BLOCK[2] - 1,
            ],
            [
                BOARD_BLOCK[0] + BOARD_HALF_COLUMNS + 1,
                BOARD_BLOCK[1] + BOARD_HALF_ROWS + 1,
                BOARD_BLOCK[2] - 1,
            ],
            stone,
        );
        assert!(filled > 0, "fixture: the board's wall must write blocks");
    }
    world
}

/// One invisible framed map per cell of the board, all facing `+z`.
///
/// The wire position is the hanging entity's own centre — `ItemFrame
/// .createBoundingBox` puts that `0.46875` back along the facing from the
/// attachment block centre — so the renderer's `floor()` is exercised the way
/// production exercises it, on every one of them.
fn board_draws(item: &ResourceLocation) -> Vec<EntityDraw> {
    let facing = lodestone_render::entity::item_frame_facing_step(0.0, 0.0);
    let mut draws = Vec::new();
    let mut id = SUBJECT_ID;
    for dy in -BOARD_HALF_ROWS..=BOARD_HALF_ROWS {
        for dx in -BOARD_HALF_COLUMNS..=BOARD_HALF_COLUMNS {
            let mut draw = blank_draw(id, "item_frame", 0.0);
            draw.feet = glam::Vec3::new(
                (BOARD_BLOCK[0] + dx) as f32 + 0.5,
                (BOARD_BLOCK[1] + dy) as f32 + 0.5,
                BOARD_BLOCK[2] as f32 + 0.5,
            ) - facing * 0.46875;
            draw.item = Some(item.clone());
            draw.invisible = true;
            draws.push(draw);
            id += 1;
        }
    }
    draws
}

/// An eye a fixed `distance` out from the board's centre, **turned** `turn`
/// degrees in place rather than moved sideways.
///
/// Turning in place is the whole point: the live report says translation does
/// nothing and rotation does everything, and a camera that orbits (which is what
/// `wall_camera` does) confounds the two.
fn board_camera(distance: f32, turn: f32, fov: f32, aspect: f32) -> Camera {
    let centre = glam::Vec3::new(
        BOARD_BLOCK[0] as f32 + 0.5,
        BOARD_BLOCK[1] as f32 + 0.5,
        BOARD_BLOCK[2] as f32 + 0.5,
    );
    let outward = lodestone_render::entity::item_frame_facing_step(0.0, 0.0);
    Camera {
        position: centre + outward * distance,
        // Facing `+z` means the room is at `+z` and the eye looks back along
        // `-z`, which is camera yaw 180 in this client's vanilla-derived
        // convention. `turn` is added to it, so the eye pivots about itself.
        yaw: 180.0 + turn,
        pitch: 0.0,
        fov_y_degrees: fov,
        aspect,
        near: 0.05,
        far: Camera::far_for_render_distance(12, 0),
    }
}

/// `(name, distance, turn degrees, fov)`.
///
/// The distance axis runs well past every other gate in this file (which stops
/// at 24 m) because the map's world clearance from the wall buys a number of
/// representable depth values that falls as `1 / distance^2`: 1661 ULP at 2 m
/// against 1 ULP at 64 m, per `docs/coplanar-overlay-depth.md`. The turn axis
/// stays small on purpose — a wall seen head-on has a window-space depth
/// gradient of zero, so the slope-scaled half of the polygon offset contributes
/// **nothing** there, and a `0`-degree row is the worst case rather than the
/// easiest one.
const BOARD_VIEWS: [(&str, f32, f32, f32); 20] = [
    ("12m turn 0 fov 70", 12.0, 0.0, 70.0),
    ("12m turn 0 fov 110", 12.0, 0.0, 110.0),
    ("12m turn 5 fov 70", 12.0, 5.0, 70.0),
    ("12m turn 15 fov 70", 12.0, 15.0, 70.0),
    ("12m turn 15 fov 110", 12.0, 15.0, 110.0),
    ("12m turn 30 fov 70", 12.0, 30.0, 70.0),
    ("12m turn 30 fov 110", 12.0, 30.0, 110.0),
    ("32m turn 0 fov 70", 32.0, 0.0, 70.0),
    ("32m turn 0 fov 110", 32.0, 0.0, 110.0),
    ("32m turn 5 fov 70", 32.0, 5.0, 70.0),
    ("32m turn 15 fov 70", 32.0, 15.0, 70.0),
    ("32m turn 15 fov 110", 32.0, 15.0, 110.0),
    ("32m turn 30 fov 70", 32.0, 30.0, 70.0),
    ("32m turn 30 fov 110", 32.0, 30.0, 110.0),
    ("64m turn 0 fov 70", 64.0, 0.0, 70.0),
    ("64m turn 0 fov 110", 64.0, 0.0, 110.0),
    ("64m turn 5 fov 70", 64.0, 5.0, 70.0),
    ("64m turn 15 fov 70", 64.0, 15.0, 70.0),
    ("96m turn 0 fov 70", 96.0, 0.0, 70.0),
    ("96m turn 0 fov 110", 96.0, 0.0, 110.0),
];

/// Which pixels a painted board covers that the very same scene without a map
/// source does not.
fn ink_mask(subject: &[u8], reference: &[u8]) -> Vec<bool> {
    subject
        .chunks_exact(4)
        .zip(reference.chunks_exact(4))
        .map(|(a, b)| {
            let d = (i32::from(a[0]) - i32::from(b[0])).abs()
                + (i32::from(a[1]) - i32::from(b[1])).abs()
                + (i32::from(a[2]) - i32::from(b[2])).abs();
            d > 8
        })
        .collect()
}

fn mask_count(mask: &[bool]) -> usize {
    mask.iter().filter(|set| **set).count()
}

/// `(min_x, min_y, max_x, max_y)` of a mask, or `None` when it is empty.
///
/// Printed on every failure: a lost region that is a straight-edged band has a
/// box flush against one screen edge, and a scattered z-fight does not. A
/// verdict about *where* is the only thing that separates the two.
fn mask_box(mask: &[bool], width: u32) -> Option<(u32, u32, u32, u32)> {
    let (mut min_x, mut min_y, mut max_x, mut max_y) = (u32::MAX, u32::MAX, 0u32, 0u32);
    let mut any = false;
    for (i, set) in mask.iter().enumerate() {
        if !set {
            continue;
        }
        any = true;
        let (x, y) = ((i as u32) % width, (i as u32) / width);
        min_x = min_x.min(x);
        min_y = min_y.min(y);
        max_x = max_x.max(x);
        max_y = max_y.max(y);
    }
    any.then_some((min_x, min_y, max_x, max_y))
}

/// Pixel gate: a **board** of invisible framed maps must not lose pixels to the
/// wall it hangs on, at any field of view, with the camera turning in place.
///
/// The measurement per configuration is the board's own ink — the pixels a
/// painted board covers that the identical scene with no map source does not —
/// taken twice: once with the attachment wall present and once with it removed.
/// The wall-free arm is the positive control, so a configuration whose control
/// is zero is reported as vacuous rather than as a pass.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_board_of_framed_maps_survives_the_depth_test_while_the_camera_turns() {
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
    let models = atlas.models().expect("vanilla atlas must carry baked models");
    let item: ResourceLocation = ITEM.parse().expect("valid item id");
    let draws = board_draws(&item);

    let (bw, bh) = board_size();
    let aspect = bw as f32 / bh as f32;
    eprintln!("board gate framebuffer: {bw}x{bh}");
    let mut target = HeadlessTarget::new(device, bw, bh, format);

    let mut masks_for = |with_wall: bool| -> Vec<Vec<bool>> {
        let mut state = RenderState::new(device, queue, format, bw, bh, Some(atlas.as_ref()));
        state.set_third_person_body_source(|| {
            Some(lodestone::gpu::ThirdPersonBodyState {
                player_skin: None,
                feet: glam::Vec3::new(0.0, -10_000.0, 0.0),
                body_yaw_deg: 0.0,
                anim: AnimInput::default(),
                scale: 1.0,
                swim_amount: 0.0,
                slim: false,
                equipment: Vec::new(),
                equipment_skin: Vec::new(),
            })
        });
        wall_upload(
            &board_world(with_wall),
            BOARD_BLOCK,
            models,
            &mut state,
            device,
            queue,
        );

        let mut shoot = |state: &RenderState, cam: &Camera| -> Vec<u8> {
            let frame = target.acquire().expect("headless acquire");
            state.render(device, queue, frame.view(), cam, None, &draws);
            target.read_texels(device, queue)
        };

        let references: Vec<Vec<u8>> = BOARD_VIEWS
            .iter()
            .map(|(_, distance, turn, fov)| {
                shoot(&state, &board_camera(*distance, *turn, *fov, aspect))
            })
            .collect();

        let painted = std::sync::Arc::new(grass_grid());
        state.set_map_source(move |_, _| {
            Some(MapPicture::new(
                test_map_id(),
                1,
                std::sync::Arc::clone(&painted),
            ))
        });

        BOARD_VIEWS
            .iter()
            .enumerate()
            .map(|(index, (_, distance, turn, fov))| {
                let shot = shoot(&state, &board_camera(*distance, *turn, *fov, aspect));
                ink_mask(&shot, &references[index])
            })
            .collect()
    };

    let free = masks_for(false);
    let walled = masks_for(true);

    eprintln!("=== map board vs its attachment wall, camera turning in place ===");
    let mut failures: Vec<String> = Vec::new();
    for (index, (view, ..)) in BOARD_VIEWS.iter().enumerate() {
        let no_wall = mask_count(&free[index]);
        let with_wall = mask_count(&walled[index]);
        let lost: Vec<bool> = free[index]
            .iter()
            .zip(walled[index].iter())
            .map(|(f, w)| *f && !*w)
            .collect();
        let lost_count = mask_count(&lost);
        eprintln!(
            "{view:<20} no-wall {no_wall:>6} px   walled {with_wall:>6} px   lost {lost_count:>6} px   \
             box {:?}",
            mask_box(&lost, bw)
        );
        if no_wall == 0 {
            failures.push(format!(
                "{view}: the wall-free control drew no board at all, so the walled \
                 measurement beside it is vacuous"
            ));
        } else if lost_count * 100 > no_wall * 2 {
            failures.push(format!(
                "{view}: the board lost {lost_count} of {no_wall} px to the wall behind \
                 it, in {:?} (x0, y0, x1, y1)",
                mask_box(&lost, bw)
            ));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
