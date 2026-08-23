//! Pixel gate: an **item frame** reaches pixels — the frame's own body, and an
//! ordinary item hanging in it.
//!
//! # Why a pixel gate and not a model-level one
//!
//! "Renders nothing" is precisely the failure a model-level assertion cannot
//! see, and this subsystem has already demonstrated that a green non-pixel test
//! coexists with an empty screen: `stats.special_item_frames_drawn` has counted
//! framed *chests* correctly for as long as it has existed, while the frame
//! around them drew nothing at all and every ordinary item in one drew nothing
//! either. The frame body has no entity rig by design — vanilla resolves it
//! through `BlockModelResolver`/`BlockStateDefinitions.getItemFrameFakeState`,
//! so `entity_models.rs` deliberately omits it and `model_for_type` answers
//! `None` — which meant nothing anywhere was its producer.
//!
//! Note also that a non-zero draw counter is **not** evidence anything reached
//! the framebuffer (this repo has measured a harness that submitted 15,104 quads
//! and read back a byte-identical frame). Only the executed controls below are:
//! every arm compares against a rendered reference and every count is asserted
//! against a rendered alternative, never against a constructed expectation.
//!
//! # The arms, and what each one separates
//!
//! | arm | what it can see that the others cannot |
//! |---|---|
//! | empty scene → frame with no item | the **body** producer exists at all |
//! | frame with no item → frame holding a diamond | the **ordinary item** producer, which is a different pipeline from the body's |
//! | visible frame → invisible frame | the two are genuinely separate producers rather than one draw counted twice |
//! | `item_frame` → `glow_item_frame` | the glow variant is routed, not silently folded onto the plain one |
//!
//! The invisible arm is the load-bearing control: `ItemFrameRenderer` clears
//! `state.frameModel` for an invisible frame and still draws its contents, so a
//! single producer covering both would have to report the same two counters for
//! both — and does not.
//!
//! Fail-closed like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test item_frame_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::EntityDraw;
use lodestone::gpu::RenderState;
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// A flat-sprite item with no `minecraft:special` form, so it can only reach the
/// screen through the *ordinary* framed-item path this file is gating —
/// `special_item_instances`' item-frame branch resolves `None` for it and the
/// `special_item_frames_drawn` counter stays at zero, which is asserted below.
const ITEM: &str = "minecraft:diamond";
const SUBJECT_ID: i32 = 9101;

/// The frame's entity position. `ItemFrame.createBoundingBox` puts that
/// **behind** the centre of the block it hangs in, so this is not the block
/// centre — the render chain steps forward `0.46875` to recover it.
const SUBJECT_POS: glam::Vec3 = glam::Vec3::new(0.0, 0.25, 2.0);

/// Yaw 180 is a **north**-facing frame, i.e. one whose front is toward `-z` —
/// toward the camera, which sits at `z = 0` looking along `+z`. Yaw 0 would put
/// the frame's back plate toward the camera and hide the item behind it
/// entirely, which is a real thing to get wrong and would read as "the item does
/// not draw".
const SUBJECT_YAW: f32 = 180.0;

fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 0.25, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

/// A blank template with every field a neutral/off value, mirroring
/// `special_item_world_pixels.rs`'s own.
fn blank_draw(id: i32, type_path: &str) -> EntityDraw {
    EntityDraw {
        hurt: false,
        block_state: None,
        item_frame_rotation: 0,
        id,
        type_path: std::sync::Arc::from(type_path),
        item: None,
        main_arm_left: false,
        feet: SUBJECT_POS,
        yaw: SUBJECT_YAW,
        head_yaw: SUBJECT_YAW,
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
        firework: None,
        projectile_owner: None,
    }
}

struct Diff {
    count: usize,
    min_x: u32,
    max_x: u32,
    min_y: u32,
    max_y: u32,
}

/// Differing pixels between two frames, **with a bounding box** — a fraction on
/// its own cannot tell a localised object from a uniform full-frame shift, and
/// this repo has been misled by exactly that.
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

/// Differing pixels inside the top-left third — the corner opposite a subject
/// drawn near screen centre.
fn diff_in_far_corner(subject: &[u8], reference: &[u8]) -> usize {
    let mut n = 0usize;
    for y in 0..H / 3 {
        for x in 0..W / 3 {
            let i = ((y * W + x) * 4) as usize;
            let d = (i32::from(subject[i]) - i32::from(reference[i])).abs()
                + (i32::from(subject[i + 1]) - i32::from(reference[i + 1])).abs()
                + (i32::from(subject[i + 2]) - i32::from(reference[i + 2])).abs();
            if d > 8 {
                n += 1;
            }
        }
    }
    n
}

/// What one render of a draw list produced: the texels plus the three counters
/// that between them say which producer ran.
struct Shot {
    px: Vec<u8>,
    bodies: usize,
    items: usize,
    specials: usize,
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn an_item_frame_and_the_item_in_it_reach_pixels() {
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
    let state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
    let cam = camera();

    let mut shoot = |draws: &[EntityDraw]| -> Shot {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, draws);
        Shot {
            px: target.read_texels(device, queue),
            bodies: stats.item_frame_bodies_drawn,
            items: stats.item_frame_items_drawn,
            specials: stats.special_item_frames_drawn,
        }
    };

    let empty = shoot(&[]);

    // Arm 1: a real frame entity carrying nothing. Vanilla draws the border and
    // back plate for an empty frame, so this must not be pixel-identical to an
    // empty scene — which is exactly what it *was* until the body got a producer.
    let bare = shoot(&[blank_draw(SUBJECT_ID, "item_frame")]);

    // Arm 2: the same frame holding an ordinary item.
    let mut held_draw = blank_draw(SUBJECT_ID, "item_frame");
    held_draw.item = Some(item.clone());
    let held = shoot(&[held_draw]);

    // Arm 3: invisible, holding the same item — no body, contents still drawn.
    let mut hidden_draw = blank_draw(SUBJECT_ID, "item_frame");
    hidden_draw.item = Some(item.clone());
    hidden_draw.invisible = true;
    let hidden = shoot(&[hidden_draw]);

    // Arm 4: the glowing variant, empty, so only the body is in play.
    let glow = shoot(&[blank_draw(SUBJECT_ID, "glow_item_frame")]);

    let d_bare = diff(&bare.px, &empty.px);
    let d_held_vs_bare = diff(&held.px, &bare.px);
    let d_hidden = diff(&hidden.px, &empty.px);
    let d_glow = diff(&glow.px, &empty.px);
    let corner = diff_in_far_corner(&bare.px, &empty.px);

    eprintln!("=== item frame pixel gate ===");
    eprintln!(
        "counters (bodies/items/specials): empty {}/{}/{}, bare {}/{}/{}, held {}/{}/{}, invisible {}/{}/{}, glow {}/{}/{}",
        empty.bodies, empty.items, empty.specials,
        bare.bodies, bare.items, bare.specials,
        held.bodies, held.items, held.specials,
        hidden.bodies, hidden.items, hidden.specials,
        glow.bodies, glow.items, glow.specials,
    );
    eprintln!(
        "lit px, bare frame vs empty scene = {} (bbox x {}..{}, y {}..{})",
        d_bare.count, d_bare.min_x, d_bare.max_x, d_bare.min_y, d_bare.max_y
    );
    eprintln!(
        "lit px, held item vs bare frame   = {} (bbox x {}..{}, y {}..{})",
        d_held_vs_bare.count,
        d_held_vs_bare.min_x,
        d_held_vs_bare.max_x,
        d_held_vs_bare.min_y,
        d_held_vs_bare.max_y
    );
    eprintln!("lit px, invisible frame vs empty  = {}", d_hidden.count);
    eprintln!("lit px, glow frame vs empty       = {}", d_glow.count);
    eprintln!("lit px, far corner                = {corner}");

    // --- the frame's own body -------------------------------------------
    assert_eq!(
        bare.bodies, 1,
        "exactly one item-frame body should have been meshed, stats said {}",
        bare.bodies
    );
    assert!(
        d_bare.count > 200,
        "an item frame two blocks away is roughly a 50 px square of border and back \
         plate; only {} px differ from the empty scene, which is what \"the frame does \
         not render\" looks like",
        d_bare.count
    );
    assert!(
        d_bare.min_x > W / 4 && d_bare.max_x < 3 * W / 4,
        "the frame must be a localised object near screen centre, not a smear: \
         x {}..{} of {W}",
        d_bare.min_x,
        d_bare.max_x
    );
    assert_eq!(
        corner, 0,
        "the corner opposite the frame must be untouched; {corner} differing px there \
         means the count above is measuring a full-screen change rather than a frame"
    );

    // --- the ordinary item in it ----------------------------------------
    assert_eq!(
        held.items, 1,
        "exactly one ordinary framed item should have been meshed, stats said {}",
        held.items
    );
    assert_eq!(
        held.specials, 0,
        "a diamond has no `minecraft:special` form, so the block-entity-rig path must \
         not also claim it ({}) — two producers resolving one stack would draw it twice",
        held.specials
    );
    assert!(
        d_held_vs_bare.count > 50,
        "the item must change pixels *on top of* the frame that was already there; \
         only {} differ from the bare frame, which is the state where the frame draws \
         and its contents do not",
        d_held_vs_bare.count
    );
    assert!(
        d_held_vs_bare.min_x > W / 4 && d_held_vs_bare.max_x < 3 * W / 4,
        "the item must sit inside the frame, not beside it: x {}..{} of {W}",
        d_held_vs_bare.min_x,
        d_held_vs_bare.max_x
    );

    // --- executed negative controls --------------------------------------
    assert_eq!(
        (empty.bodies, empty.items, empty.specials),
        (0, 0, 0),
        "a frame with no entities cannot have drawn anything"
    );
    assert_eq!(
        bare.items, 0,
        "a frame carrying no stack must not draw a framed item ({})",
        bare.items
    );
    // The invisible arm: `ItemFrameRenderer` clears `state.frameModel` when
    // `state.isInvisible`, so the body must vanish while the contents stay. This
    // is what proves the two counters are two producers and not one draw counted
    // twice — a single producer could not answer differently here.
    assert_eq!(
        (hidden.bodies, hidden.items),
        (0, 1),
        "an invisible frame must draw its contents and not its body; got {}/{}",
        hidden.bodies,
        hidden.items
    );
    assert!(
        d_hidden.count > 0 && d_hidden.count < d_bare.count,
        "an invisible frame holding an item must still put *something* on screen, and \
         less of it than a visible empty frame does: {} px against the bare frame's {}",
        d_hidden.count,
        d_bare.count
    );
    // The glow variant is routed rather than silently folded onto the plain one:
    // it draws its own body, and a different `#back` sprite plus the block-light
    // floor of 5 make the two frames non-identical.
    assert_eq!(
        glow.bodies, 1,
        "a glow_item_frame must draw a body too, stats said {}",
        glow.bodies
    );
    assert!(
        d_glow.count > 200,
        "a glow item frame must reach pixels like the plain one; got {}",
        d_glow.count
    );
    assert!(
        diff(&glow.px, &bare.px).count > 0,
        "the glow variant resolved to a byte-identical frame — `item_frame_quads` is \
         ignoring its `glow` argument, or both slots baked the same model"
    );
}
