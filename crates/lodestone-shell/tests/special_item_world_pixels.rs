//! Pixel gate: a `minecraft:shield` — an item with **no baked block-item
//! geometry** — reaches pixels as a **dropped stack** and as a **framed
//! item**, the two `gpu/entity_passes.rs` surfaces the shield un-mirroring
//! fix (`5c99876e`) touched but never measured (see that commit's own diff:
//! `dropped_special_item`/`held_special_item`/`framed_special_item` all
//! changed their `resolve_special_item` call from an owned `Option` to a
//! borrowed transformation **chain**).
//!
//! # Why this file exists at all, not just a colour check
//!
//! `special_item_rig` — the `(kind, item_path) -> (rig, sheet)` resolver
//! these three surfaces share — carried **no `minecraft:shield` arm**
//! before this session: every other special kind (`chest`, `shulker_box`,
//! `head`) had one, `shield` fell through `_ => None`, and its own doc said
//! outright that a shield "is drawn, but not through this resolver" and
//! named only the first-person hand and the GUI icon as the two call sites
//! that actually draw one. **A dropped or framed shield therefore drew
//! nothing at all** — not "mirrored", not "undyed", *nothing* — which is a
//! different and worse defect than the one `5c99876e` fixed, and squarely
//! within what "add the missing gates" for these two surfaces asked for. The
//! resolver now carries a `"minecraft:shield"` arm (always the no-pattern
//! sheet, since neither surface threads `minecraft:base_color`/
//! `minecraft:banner_patterns` through `EntityDraw` — the same *documented,
//! bounded* shortfall this file's sibling surfaces already carry: a dropped
//! stack never multiplies past one copy, a framed item's own rotation is
//! undecoded). `lodestone-render`'s own `block_entity::tests::
//! special_item_tests::shield_resolves_to_the_no_pattern_rig_and_sheet` is
//! the unit-level half of that; this file is the pixel-level half, on the
//! real `RenderState::render` call `app.rs` makes.
//!
//! # Why this can't reuse the hand/GUI gates' colour discriminator
//!
//! Neither `EntityDraw` (the dropped case) nor the item-frame case carries a
//! shield's `base_color`/`banner_patterns` at all, so both always draw the
//! plain no-pattern sheet — there is no dyed variant to compare against here.
//! The un-mirroring fix's own geometry composition
//! (`compose_special_node_transform`, fed the same parsed transformation
//! chain `resolve_special_item` always has) is exactly what
//! `hotbar_special_item_pixels.rs` and `first_person_shield_hand_pixels.rs`
//! already exercise and measure by colour; this file's job is narrower and
//! different — proving the *wiring* (`special_item_rig` now naming a shield
//! arm, `special_item_instances` actually reaching it) puts a real,
//! localised object on screen on these two surfaces, mirroring
//! `dropped_item_pixels.rs`'s own executed-negative-control shape (empty
//! scene, far corner, a same-type entity with no item).
//!
//! Fail-closed like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test special_item_world_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::{EntityDraw, ITEM_ENTITY_TYPE_PATH};
use lodestone::gpu::RenderState;
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

const ITEM: &str = "minecraft:shield";
const SUBJECT_ID: i32 = 9001;
const SUBJECT_POS: glam::Vec3 = glam::Vec3::new(0.0, 0.0, 2.0);

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

/// A blank template with every field a neutral/off value — filled in by each
/// closure below. Mirrors `dropped_item_pixels.rs`'s own `drop_draw`/
/// `projectile` builders, kept as one shared template here since this file's
/// two subjects (dropped, framed) differ only in `type_path`/`item`/pose.
fn blank_draw(id: i32, type_path: &str) -> EntityDraw {
    EntityDraw {
        hurt: false,
        block_state: None,
        id,
        type_path: std::sync::Arc::from(type_path),
        item: None,
        main_arm_left: false,
        feet: SUBJECT_POS,
        yaw: 0.0,
        head_yaw: 0.0,
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
    }
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
    for (i, (a, b)) in subject.chunks_exact(4).zip(reference.chunks_exact(4)).enumerate() {
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
    Diff { count, min_x, max_x, min_y, max_y }
}

/// Differing pixels inside the top-left quarter — the corner opposite a
/// subject drawn near screen centre. Mirrors `dropped_item_pixels.rs`'s own.
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

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_dropped_shield_reaches_pixels() {
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

    let mut shoot = |draws: &[EntityDraw]| -> (Vec<u8>, usize, usize) {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, draws);
        (
            target.read_texels(device, queue),
            stats.item_drops_drawn,
            stats.special_item_drops_drawn,
        )
    };

    let (empty, empty_baked, empty_special) = shoot(&[]);

    // Control: the same item-entity id and position, but no stack — the
    // "tracked but unreported" state `dropped_item_pixels.rs` also controls
    // for, reused here since `dropped_special_item` gates on `draw.item`
    // exactly the same way `prepare_item_geometry` does.
    let mut no_stack = blank_draw(SUBJECT_ID, ITEM_ENTITY_TYPE_PATH);
    no_stack.item = None;
    let (no_stack_px, no_stack_baked, no_stack_special) = shoot(&[no_stack]);

    let mut subject = blank_draw(SUBJECT_ID, ITEM_ENTITY_TYPE_PATH);
    subject.item = Some(item.clone());
    let (subject_px, subject_baked, subject_special) = shoot(&[subject]);

    let d_subject = diff(&subject_px, &empty);
    let d_no_stack = diff(&no_stack_px, &empty);
    let corner = diff_in_far_corner(&subject_px, &empty);

    eprintln!("=== dropped shield pixel gate ===");
    eprintln!("stats.item_drops_drawn         = empty {empty_baked}, no-stack {no_stack_baked}, shield {subject_baked}");
    eprintln!("stats.special_item_drops_drawn = empty {empty_special}, no-stack {no_stack_special}, shield {subject_special}");
    eprintln!(
        "lit px, dropped shield = {} (bbox x {}..{}, y {}..{})",
        d_subject.count, d_subject.min_x, d_subject.max_x, d_subject.min_y, d_subject.max_y
    );
    eprintln!("lit px, no-stack control = {}", d_no_stack.count);
    eprintln!("lit px, far corner       = {corner}");

    assert_eq!(
        subject_special, 1,
        "exactly one dropped shield should have been meshed through the special-item \
         path, stats said {subject_special} — `special_item_rig(\"minecraft:shield\", …)` \
         either regressed back to `None` or `special_item_instances` never reached it"
    );
    assert_eq!(
        subject_baked, 0,
        "a shield has no baked block-item geometry; it must not also be counted by \
         the ordinary baked-drop pass ({subject_baked}), or the two paths are double-\
         resolving the same stack"
    );
    assert!(
        d_subject.count > 50,
        "a dropped shield two blocks away should cover a real run of pixels; only {} \
         differ from the empty frame — near-zero means the instance resolved but the \
         mesh, sheet or transform is degenerate",
        d_subject.count
    );
    assert!(
        d_subject.min_x > W / 4 && d_subject.max_x < 3 * W / 4,
        "the drop must be a localised object near screen centre, not a smear: x {}..{} \
         of {W}",
        d_subject.min_x,
        d_subject.max_x
    );
    assert_eq!(
        corner, 0,
        "the corner opposite the drop must be untouched; {corner} differing px there \
         means the count above is measuring a full-screen change"
    );

    // --- executed negative controls -------------------------------------
    assert_eq!(empty_special, 0, "a frame with no entities cannot have drawn a special-item drop");
    assert_eq!(
        d_no_stack.count, 0,
        "an item entity whose stack has not been reported must draw nothing; {} px \
         says a placeholder is being substituted",
        d_no_stack.count
    );
    assert_eq!(no_stack_special, 0, "the special-item drop counter must not count an entity with no reported stack");
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_framed_shield_reaches_pixels() {
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

    let mut shoot = |draws: &[EntityDraw]| -> (Vec<u8>, usize) {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, draws);
        (target.read_texels(device, queue), stats.special_item_frames_drawn)
    };

    let (empty, empty_frames) = shoot(&[]);

    // Control: a real `item_frame` entity, empty (no item hung in it) — the
    // same "entity present, nothing to draw" shape as the dropped gate's own
    // no-stack control.
    let mut empty_frame = blank_draw(SUBJECT_ID, "item_frame");
    empty_frame.item = None;
    let (empty_frame_px, empty_frame_count) = shoot(&[empty_frame]);

    let mut subject = blank_draw(SUBJECT_ID, "item_frame");
    subject.item = Some(item.clone());
    let (subject_px, subject_count) = shoot(&[subject]);

    let d_subject = diff(&subject_px, &empty);
    let d_empty_frame = diff(&empty_frame_px, &empty);
    let corner = diff_in_far_corner(&subject_px, &empty);

    eprintln!("=== framed shield pixel gate ===");
    eprintln!("stats.special_item_frames_drawn = empty {empty_frames}, empty-frame {empty_frame_count}, shield {subject_count}");
    eprintln!(
        "lit px, framed shield = {} (bbox x {}..{}, y {}..{})",
        d_subject.count, d_subject.min_x, d_subject.max_x, d_subject.min_y, d_subject.max_y
    );
    eprintln!("lit px, empty-frame control = {}", d_empty_frame.count);
    eprintln!("lit px, far corner          = {corner}");

    assert_eq!(
        subject_count, 1,
        "exactly one framed shield should have been meshed, stats said {subject_count} \
         — `special_item_rig(\"minecraft:shield\", …)` either regressed back to `None` \
         or `special_item_instances` never reached the item-frame branch"
    );
    assert!(
        d_subject.count > 30,
        "a framed shield two blocks away should cover a real run of pixels; only {} \
         differ from the empty frame",
        d_subject.count
    );
    assert_eq!(
        corner, 0,
        "the corner opposite the frame must be untouched; {corner} differing px there \
         means the count above is measuring a full-screen change"
    );

    // --- executed negative controls -------------------------------------
    assert_eq!(empty_frames, 0, "a frame with no entities cannot have drawn a framed item");
    assert_eq!(
        empty_frame_count, 0,
        "an item_frame entity with no item hung in it must not draw a shield"
    );
    assert_eq!(
        d_empty_frame.count, 0,
        "an empty item frame must be pixel-identical to no entity at all through this \
         pass (the frame's own block-entity mesh, if any, is a different pipeline); {} \
         px says something drew for an item that was never there",
        d_empty_frame.count
    );
}
