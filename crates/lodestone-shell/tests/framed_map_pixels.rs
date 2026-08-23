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
use lodestone::gpu::RenderState;
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

const ITEM: &str = "minecraft:filled_map";
const SUBJECT_ID: i32 = 9201;

/// Where the frame entity sits. `ItemFrame.createBoundingBox` puts that behind
/// the centre of the block it hangs in; the render chain steps `0.46875` forward
/// along the frame's own `Direction` to recover the frame's plane.
const SUBJECT_POS: glam::Vec3 = glam::Vec3::new(0.0, 0.25, 0.0);

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
    Camera {
        position: SUBJECT_POS + out * 2.0,
        yaw: frame_yaw + 180.0,
        pitch: 0.0,
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
        main_arm_left: false,
        feet: SUBJECT_POS,
        yaw,
        head_yaw: yaw,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput::REST,
        equipment: Vec::new(),
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

    state.set_map_source(|_| Some(vec![0u8; 128 * 128]));
    let transparent: Vec<Shot> = YAWS
        .iter()
        .map(|yaw| shoot(&state, &camera_for(*yaw), std::slice::from_ref(&framed(*yaw))))
        .collect();

    state.set_map_source(|_| Some(grass_grid()));
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
