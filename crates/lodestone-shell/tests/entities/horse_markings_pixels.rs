//! Pixel gate: a horse's markings overlay reaches the screen as a second,
//! translucent pass over its coat, driven through the real
//! [`RenderState::render`] path.
//!
//! # The metric
//!
//! A black horse is dark everywhere the overlay does not paint, and the white
//! markings sheet is the only bright art in the scene. The subject (black coat
//! plus the "white" markings sheet) is compared pixel-for-pixel with the same
//! horse drawn with no overlay; pixels that got **much brighter** are the
//! markings. The count and the bounding box of those pixels are printed, and
//! the box must sit inside the horse's own silhouette.
//!
//! # The controls
//!
//! * No overlay at all: nothing brightens (the detector reads zero on an
//!   unchanged scene, so the subject's count is the overlay's doing).
//! * The **black-dots** markings sheet over the black coat: black on black, so
//!   it must not brighten anything. This one proves the brightening is the white
//!   art specifically, not "any second pass lightens the horse".
//! * The pack's texture ordinals: `horse_markings_sheet` maps the wire's
//!   second byte to a sheet, so the ordinal-to-sheet table is asserted against
//!   the five vanilla paths directly.
//!
//! Fail-closed like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test entities horse_markings -- --ignored --nocapture
//! ```

use lodestone::entities::{EntityDraw, EntityOverlay};
use lodestone::gpu::RenderState;
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

fn brightness(px: &[u8]) -> i32 {
    i32::from(px[0]) + i32::from(px[1]) + i32::from(px[2])
}

/// Pixels of `after` much brighter than the same pixel of `before`, with their
/// bounding box `(min_x, min_y, max_x, max_y)`.
fn brightened(before: &[u8], after: &[u8]) -> (usize, Option<(u32, u32, u32, u32)>) {
    let mut count = 0;
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    for (i, (b, a)) in before.chunks_exact(4).zip(after.chunks_exact(4)).enumerate() {
        if brightness(a) - brightness(b) > 150 {
            count += 1;
            let (x, y) = (i as u32 % W, i as u32 / W);
            bbox = Some(match bbox {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }
    (count, bbox)
}

/// Pixels whose colour differs at all from the background-free control frame,
/// as a silhouette proxy: anything not equal to the empty-scene frame.
fn silhouette_bbox(empty: &[u8], frame: &[u8]) -> (u32, u32, u32, u32) {
    let mut bbox: Option<(u32, u32, u32, u32)> = None;
    for (i, (e, f)) in empty.chunks_exact(4).zip(frame.chunks_exact(4)).enumerate() {
        if (brightness(e) - brightness(f)).abs() > 30 {
            let (x, y) = (i as u32 % W, i as u32 / W);
            bbox = Some(match bbox {
                None => (x, y, x, y),
                Some((x0, y0, x1, y1)) => (x0.min(x), y0.min(y), x1.max(x), y1.max(y)),
            });
        }
    }
    bbox.expect("the horse must draw something")
}

#[test]
fn the_markings_ordinal_selects_the_vanilla_overlay_sheet() {
    use lodestone_model::EntityVariant as Wire;
    let sheet = |model: &str, markings: u8| {
        lodestone_render::horse_markings_sheet(model, &Wire::Horse { color: 0, markings })
    };
    assert_eq!(sheet("horse", 0), None, "ordinal 0 is no overlay");
    assert_eq!(sheet("horse", 1), Some("entity/horse/horse_markings_white"));
    assert_eq!(sheet("horse", 2), Some("entity/horse/horse_markings_whitefield"));
    assert_eq!(sheet("horse", 3), Some("entity/horse/horse_markings_whitedots"));
    assert_eq!(sheet("horse", 4), Some("entity/horse/horse_markings_blackdots"));
    assert_eq!(sheet("horse", 5), None, "an ordinal outside the five markings draws nothing");
    // Only the plain horse has a markings layer.
    assert_eq!(sheet("donkey", 1), None);
    assert_eq!(sheet("zombie_horse", 1), None);
    // The coat comes from the low byte, selecting the matching base sheet.
    let coat = |color: u8| {
        lodestone_render::entity_variant_sheet_for("horse", &Wire::Horse { color, markings: 0 }, false)
    };
    assert_eq!(coat(0), Some("entity/horse/horse_white"));
    assert_eq!(coat(4), Some("entity/horse/horse_black"));
    assert_eq!(coat(6), Some("entity/horse/horse_darkbrown"));
    assert_eq!(coat(7), None);
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_horses_markings_overlay_brightens_a_black_coat_where_the_art_is_white() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let state = RenderState::new_headless(device, queue, format, W, H, None);

    let camera = Camera {
        position: glam::Vec3::new(0.0, 1.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };
    let feet = glam::Vec3::new(0.0, 0.0, 4.0);

    let subject = EntityDraw {
        hurt: false,
        id: 1,
        type_path: std::sync::Arc::from("horse"),
        named_cosmetics: Default::default(),
        item: None,
        item_model: None,
        item_skin: None,
        main_arm_left: false,
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_skin: Vec::new(),
        equipment_trim: Vec::new(),
        feet,
        yaw: 90.0,
        head_yaw: 90.0,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput::REST,
        wool: None,
        block_state: None,
        item_frame_rotation: 0,
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
        // Not a player, so no skin can apply.
        player_skin: None,
        // Not an experience orb, so the orb billboard pass never claims it.
        // A black coat, so the white markings are the only bright art.
        variant_sheet: Some("entity/horse/horse_black"),
        overlay_sheet: Some(EntityOverlay {
            sheet: "entity/horse/horse_markings_white",
            tint: [255; 3],
        }),
        eyes_sheet: None,
        experience_orb_value: None,
        tnt_fuse: None,
        cape_sway: (0.0, 0.0, 0.0),
        painting: None,
        firework: None,
        projectile_owner: None,
    };
    let none = EntityDraw {
        id: 2,
        overlay_sheet: None,
        eyes_sheet: None,
        ..subject.clone()
    };
    let black_dots = EntityDraw {
        id: 3,
        overlay_sheet: Some(EntityOverlay {
            sheet: "entity/horse/horse_markings_blackdots",
            tint: [255; 3],
        }),
        eyes_sheet: None,
        ..subject.clone()
    };

    let mut shoot = |draws: &[EntityDraw]| -> Vec<u8> {
        let frame = target.acquire().expect("headless acquire");
        state.render(device, queue, frame.view(), &camera, None, draws);
        target.read_texels(device, queue)
    };
    let empty = shoot(&[]);
    let base = shoot(std::slice::from_ref(&none));
    let marked = shoot(std::slice::from_ref(&subject));
    let dotted = shoot(std::slice::from_ref(&black_dots));
    let repeat = shoot(std::slice::from_ref(&none));

    let (silhouette_x0, silhouette_y0, silhouette_x1, silhouette_y1) = silhouette_bbox(&empty, &base);
    let (marked_count, marked_box) = brightened(&base, &marked);
    let (dotted_count, _) = brightened(&base, &dotted);
    let (repeat_count, _) = brightened(&base, &repeat);

    eprintln!("=== horse markings pixel gate ===");
    eprintln!("horse silhouette bbox = ({silhouette_x0},{silhouette_y0})-({silhouette_x1},{silhouette_y1})");
    eprintln!("white markings brightened px = {marked_count}, bbox {marked_box:?}");
    eprintln!("black-dots control brightened px = {dotted_count}");
    eprintln!("no-overlay repeat brightened px  = {repeat_count}");

    assert_eq!(
        repeat_count, 0,
        "the detector must read zero on an unchanged scene; {repeat_count} px moved between two \
         identical frames, so the metric itself is unsound"
    );
    assert!(
        dotted_count < 20,
        "black markings over a black coat must not brighten the horse ({dotted_count} px did): \
         the brightening would then be a generic second-pass effect, not the white art"
    );
    assert!(
        marked_count > 100,
        "the white markings overlay should brighten a real patch of the black horse; only \
         {marked_count} px did (no overlay pass, or the overlay sheet is not bound)"
    );
    let (x0, y0, x1, y1) = marked_box.expect("count > 0 implies a box");
    assert!(
        x0 >= silhouette_x0 && y0 >= silhouette_y0 && x1 <= silhouette_x1 && y1 <= silhouette_y1,
        "the markings box ({x0},{y0})-({x1},{y1}) must lie inside the horse's own silhouette \
         ({silhouette_x0},{silhouette_y0})-({silhouette_x1},{silhouette_y1})"
    );
}
