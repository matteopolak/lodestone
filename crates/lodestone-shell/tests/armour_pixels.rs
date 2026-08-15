//! Pixel gate: a fully-armoured mob must draw **more** silhouette than the
//! same bare mob — the honest gap `docs/armour-rendering.md` names by name:
//! "nothing asserts armour pixels. The `prepare_armour` -> draw hop is
//! verified only by construction."
//!
//! `lodestone-shell::gpu`'s own
//! `a_fully_armoured_zombie_resolves_layers_on_real_wearer_parts` proves the
//! *resolution* chain (equipment -> `ArmourSlot` -> attach points on the real
//! wearer skeleton) — but it never calls [`RenderState::render`], so it is a
//! closed loop exactly like every other island this repo has shipped: it
//! stays green whether or not `prepare_armour`'s batches ever reach a pixel.
//! This gate drives the real [`RenderState::render`] path, the same call
//! `app.rs`'s frame loop makes, and reads the framebuffer back.
//!
//! # The metric, and where the expected figure comes from
//!
//! Armour is drawn **over** the same body, at the same screen position, so
//! almost every armour pixel recolours a pixel the bare body already lit —
//! only the outward *inflation* (`docs/armour-rendering.md`'s two-inflation
//! table) creates genuinely new, previously-sky pixels, in a thin ring around
//! the silhouette. So the gate's measurement is exactly that ring:
//! `(armoured non-sky pixel count) - (bare non-sky pixel count)`.
//!
//! The expected figure is **not** measured once and pasted back. It is
//! projected mechanically, through the *same* [`Camera::view_projection`]
//! this test's own render call uses, from two facts that are themselves real
//! rather than hand-typed:
//!
//! * the **chest** part's real baked vertex extents (`EntityMesh::vertices`
//!   sliced by its `PartRange`, from the *actual* [`EntityModelSet::load`]
//!   corpus this test resolves "zombie" against — not a remembered
//!   `HumanoidModel.java` box literal);
//! * [`ArmourSlot::Chest::inflation`] — the same public, tested constant
//!   `prepare_armour`'s real mesh bake reads (`OUTER_ARMOUR_INFLATION`,
//!   vanilla's `CubeDeformation(1.0F)`).
//!
//! That gives a real, mechanically-derived "picture-frame" ring area for the
//! **chest cube alone** (`2*Δwidth*height + 2*Δheight*width`, the standard
//! first-order frame-area expansion). It is a deliberate **lower bound**, not
//! a tight band like `hotbar_block_item_pixels.rs`'s analytic figure: legs,
//! feet and head each contribute their own additional ring and are not
//! counted, and part of the projected chest ring can be occluded by the arms
//! in front of it. So the gate asserts the observed delta clears a modest
//! *fraction* of the chest-only estimate, not the estimate itself — real
//! geometry, honestly under-counted, rather than a number typed once and
//! never re-derived.
//!
//! # The negative control
//!
//! The control is not "armour never attempted" — `prepare_armour` runs
//! unconditionally next to `prepare_entities` regardless of what the mob
//! wears, so a mob with no equipment already exercises that machinery and
//! finds nothing to draw. The control here is exactly that: the **same**
//! zombie, same pose, same camera, with `equipment: Vec::new()`. Any measured
//! difference is attributable to the equipped armour itself, not to whether
//! the armour *pass* ran.
//!
//! [`RenderStats::armour_layers_drawn`] is asserted too, as an exact,
//! non-approximate corroboration: 4 diamond pieces (one layer each, unlike
//! leather's two) on the subject, 0 on the control.
//!
//! Fail-closed, like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip — armour has no synthetic-texture fallback
//! (`docs/armour-rendering.md`), so a missing pack would otherwise read as a
//! quiet, indistinguishable-from-passing zero.
//!
//! ```text
//! cargo test -p lodestone-shell --test armour_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::EntityDraw;
use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_assets::ResourceLocation;
use lodestone_assets::equipment::ArmourSlot;
use lodestone_model::event::EquipmentSlot;
use lodestone_render::{AnimInput, Camera, EntityModelSet, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// The bytes the sky clear actually lands on in this readback. Derived from
/// the same [`SKY_COLOR`] the render pipeline clears with, on the same
/// `Rgba8Unorm` (linear, no gamma encode on write) target
/// `entity_renders_to_pixels_through_shell_path` uses — see that constant's
/// own doc for why a second, independently-typed copy of this conversion
/// went stale twice before.
fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}

/// Non-sky pixels in `pixels` — "how much of the frame is mob".
fn non_sky_count(pixels: &[u8], sky: [u8; 3]) -> usize {
    pixels
        .chunks_exact(4)
        .filter(|px| {
            let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
                + (i32::from(px[1]) - i32::from(sky[1])).abs()
                + (i32::from(px[2]) - i32::from(sky[2])).abs();
            d > 60
        })
        .count()
}

/// Project a world-space point through `view_proj` (a full camera
/// view-projection matrix, with the perspective divide this needs and a
/// part transform does not) to a pixel coordinate on a `w`x`h` target,
/// matching wgpu's NDC-to-framebuffer convention (`y` flipped, origin
/// top-left).
fn project(view_proj: glam::Mat4, world: glam::Vec3, w: u32, h: u32) -> (f32, f32) {
    let clip = view_proj * glam::Vec4::new(world.x, world.y, world.z, 1.0);
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;
    (
        (ndc_x * 0.5 + 0.5) * w as f32,
        (1.0 - (ndc_y * 0.5 + 0.5)) * h as f32,
    )
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_fully_armoured_zombie_draws_more_silhouette_than_a_bare_one() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let state = RenderState::new(device, queue, format, W, H, None);

    // Same fixture shape as `entity_renders_to_pixels_through_shell_path`
    // (camera at the origin, mob a few blocks south) so a regression in the
    // shared entity path shows up there too. `yaw: 0.0`, `AnimInput::REST`:
    // no walk cycle, no arm swing, so the analytic chest projection below
    // matches the drawn pose exactly rather than an animated one.
    let feet = glam::Vec3::new(0.0, 0.0, 4.0);
    let camera = Camera {
        position: glam::Vec3::new(0.0, 1.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    let armour_equipment = vec![
        (
            EquipmentSlot::Head,
            ResourceLocation::parse("minecraft:diamond_helmet").unwrap(),
        ),
        (
            EquipmentSlot::Chest,
            ResourceLocation::parse("minecraft:diamond_chestplate").unwrap(),
        ),
        (
            EquipmentSlot::Legs,
            ResourceLocation::parse("minecraft:diamond_leggings").unwrap(),
        ),
        (
            EquipmentSlot::Feet,
            ResourceLocation::parse("minecraft:diamond_boots").unwrap(),
        ),
    ];

    let subject = EntityDraw {
        hurt: false,
        block_state: None,
        id: 1,
        type_path: "zombie".to_owned(),
        item: None,
        equipment: armour_equipment,
        equipment_dye: Vec::new(),
        equipment_trim: Vec::new(),
        feet,
        yaw: 0.0,
        head_yaw: 0.0,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput::REST,
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
        // Not a player, so no skin can apply.
        player_skin: None,
        // A zombie has no variant texture axis, so the model sheet applies.
        variant_sheet: None,
        // Not an experience orb, so the orb billboard pass never claims it.
        experience_orb_value: None,
    };
    // The negative control: identical in every respect except equipment.
    let control = EntityDraw {
        id: 2,
        equipment: Vec::new(),
        ..subject.clone()
    };

    let mut shoot = |draw: &EntityDraw| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, std::slice::from_ref(draw));
        (target.read_texels(device, queue), stats)
    };

    let (subject_px, subject_stats) = shoot(&subject);
    let (control_px, control_stats) = shoot(&control);

    let sky = sky_bytes();
    let subject_count = non_sky_count(&subject_px, sky);
    let control_count = non_sky_count(&control_px, sky);
    let delta = subject_count as isize - control_count as isize;

    // --- The analytic lower bound, from real baked geometry. -----------------
    let models = EntityModelSet::load();
    let instance = models
        .resolve(&subject.type_path, subject.feet, subject.yaw, subject.scale, &subject.anim)
        .expect("zombie resolves in the real corpus");
    let wearer = models.get(instance.model).expect("zombie mesh present");
    let body_idx = wearer
        .skeleton
        .index_of("body")
        .expect("the humanoid rig has a body part");
    let body_range = wearer.parts[body_idx];
    let body_verts = &wearer.vertices[body_range.vertex_start as usize
        ..(body_range.vertex_start + body_range.vertex_count) as usize];
    assert!(
        !body_verts.is_empty(),
        "the body part must carry real baked vertices to derive an expected figure from"
    );
    let (mut min_x, mut max_x, mut min_y, mut max_y) =
        (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
    for v in body_verts {
        min_x = min_x.min(v.position[0]);
        max_x = max_x.max(v.position[0]);
        min_y = min_y.min(v.position[1]);
        max_y = max_y.max(v.position[1]);
    }
    let body_m = instance.part_transforms[body_idx];
    let view_proj = camera.view_projection();
    // `ArmourSlot::Chest::inflation()` is `OUTER_ARMOUR_INFLATION` — one
    // vanilla "model unit" (`CubeDeformation(1.0F)`), i.e. 1/16 block, the
    // same real value `prepare_armour`'s mesh bake reads.
    let inflation = ArmourSlot::Chest.inflation() / 16.0;

    let px = |x: f32, y: f32| project(view_proj, body_m.transform_point3(glam::Vec3::new(x, y, 0.0)), W, H);
    let (right_bare, _) = px(max_x, (min_y + max_y) * 0.5);
    let (left_bare, _) = px(min_x, (min_y + max_y) * 0.5);
    let (right_arm, _) = px(max_x + inflation, (min_y + max_y) * 0.5);
    let (left_arm, _) = px(min_x - inflation, (min_y + max_y) * 0.5);
    let (_, top_bare) = px((min_x + max_x) * 0.5, max_y);
    let (_, bottom_bare) = px((min_x + max_x) * 0.5, min_y);
    let (_, top_arm) = px((min_x + max_x) * 0.5, max_y + inflation);
    let (_, bottom_arm) = px((min_x + max_x) * 0.5, min_y - inflation);

    let width_px = (right_bare - left_bare).abs();
    let height_px = (bottom_bare - top_bare).abs();
    let width_growth_px = (right_arm - left_arm).abs() - width_px;
    let height_growth_px = (bottom_arm - top_arm).abs() - height_px;

    // Sanity on the projection itself, before trusting the ring formula built
    // from it: the armoured chest must project *wider* and *taller* than the
    // bare one, or a sign error here — not a render defect — would be the
    // thing under test.
    assert!(
        width_growth_px > 0.0 && height_growth_px > 0.0,
        "the analytic projection itself is broken (width_growth={width_growth_px:.2}, \
         height_growth={height_growth_px:.2}, both should be positive) — a sign error in \
         this test's own math, not a claim about the renderer"
    );

    // First-order picture-frame ring area: outer area minus inner area,
    // dropping the second-order Δw·Δh corner term.
    let chest_ring_estimate = 2.0 * width_growth_px * height_px + 2.0 * height_growth_px * width_px;

    eprintln!("=== armour pixel gate ===");
    eprintln!("subject (armoured) non-sky px = {subject_count}");
    eprintln!("control (bare)     non-sky px = {control_count}");
    eprintln!("delta                          = {delta}");
    eprintln!(
        "chest-only ring estimate       = {chest_ring_estimate:.1} px (lower bound; \
         legs/feet/head not counted)"
    );
    eprintln!(
        "subject armour_layers_drawn    = {}",
        subject_stats.armour_layers_drawn
    );
    eprintln!(
        "control armour_layers_drawn    = {}",
        control_stats.armour_layers_drawn
    );

    // The exact, non-approximate corroboration: one layer per diamond piece,
    // zero when nothing is equipped.
    assert_eq!(
        subject_stats.armour_layers_drawn, 4,
        "a full diamond set is 4 single-layer pieces; armour_layers_drawn={} means the \
         resolution chain did not run (no vanilla pack? see the #[ignore] reason)",
        subject_stats.armour_layers_drawn
    );
    assert_eq!(
        control_stats.armour_layers_drawn, 0,
        "the bare control must equip nothing, but armour_layers_drawn={}",
        control_stats.armour_layers_drawn
    );

    // The load-bearing pixel assertion. A generous fraction of the chest-only
    // estimate, not the estimate itself: legs/feet/head each add their own
    // ring on top, and part of the chest ring can sit behind the arms.
    let floor = (chest_ring_estimate * 0.2).max(1.0);
    assert!(
        delta as f32 > floor,
        "the armoured zombie should draw a visibly larger silhouette than the bare one \
         (a new, previously-sky ring from the outer inflation); got delta={delta}, expected \
         more than {floor:.1} px (20% of the chest-only analytic estimate {chest_ring_estimate:.1}). \
         Far below (or negative) means armour is not reaching pixels."
    );
    // A broad sanity ceiling, not a tight band: armour cannot plausibly add
    // more new silhouette than the bare mob's own.
    assert!(
        (delta as usize) < control_count.max(1) * 3,
        "the armour delta ({delta}) is implausibly large next to the bare mob's own \
         silhouette ({control_count}) — likely a broken control rather than a real armour effect"
    );

    assert!(
        control_count > 200,
        "the bare mob itself should reach a substantial run of pixels ({control_count}); \
         if this is near zero the whole entity path is broken, not just armour"
    );
}

