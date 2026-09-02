//! Pixel gate: a mob wearing an elytra must draw **more** silhouette than the
//! same mob with an empty chest slot — driven through the real
//! [`RenderState::render`] path, the same call `app.rs`'s frame loop makes.
//!
//! `lodestone-render`'s own `elytra_wings.rs` proves the mesh, the mirror
//! identity and the pose branch, and says so in its own module doc: it never
//! calls `RenderState::prepare_elytra`, so it is a closed loop with respect to
//! this crate and stays green whether or not anything in `lodestone-shell`
//! reaches the geometry. It stayed green for exactly that reason while an
//! elytra reached **zero** pixels and — because `prepare_cape` suppresses the
//! cape for an elytra wearer — cost that wearer their cape as well.
//!
//! # Which half this verifies, and which it does not
//!
//! It verifies the **draw**: `EntityDraw::equipment` -> `prepare_elytra` ->
//! `ElytraDrawBatch` -> pixels, through the production pipeline, texture and
//! camera. It installs its own `EntityDraw`, so it says nothing about the
//! producer — whether the wire's equipment packet actually lands
//! `(EquipmentSlot::Chest, minecraft:elytra)` in that field is
//! `crates/protocol/v770`'s question and no assertion here can see it.
//!
//! # The metric, and where the expected figure comes from
//!
//! Not the silhouette delta its armour and wool siblings use. Armour inflates
//! a part that is already drawn, so the only *new* pixels are a thin ring and
//! counting sky is the whole measurement; the wings are new geometry hung off
//! the back and drawn largely **over** the torso, so a silhouette delta
//! measures the small unoccluded fringe and throws away most of the evidence.
//! What is measured here is the number of pixels that **changed** between the
//! two frames, which is exactly the elytra layer's own screen coverage.
//!
//! The expected figure is **bracketed on both sides**, and both bounds are
//! projected mechanically through the *same* [`Camera::view_projection`] this
//! test's own render call uses, from the real baked wing quads
//! ([`ElytraMesh::load`], not a remembered `ElytraModel` box literal) posed by
//! the real [`elytra_wing_transform`] on the real resolved `"body"` matrix.
//! Each wing is a closed convex box, so summing the projected (shoelace) area
//! of its front-facing quads gives its exact screen silhouette — and summing
//! the *back*-facing ones gives the same number by construction, which is the
//! control on this test's own arithmetic and is asserted before either bound
//! is trusted.
//!
//! * **Upper bound.** The coverage cannot exceed that sum: the two wings
//!   overlap each other on screen (their rects overlap by more than half
//!   their width), and a pixel the wing paints in a colour the torso already
//!   had does not count as changed. Both errors run the same way. The 10%
//!   slack allowed on top is for rasterisation, which can claim up to about
//!   half a perimeter of edge pixels beyond an analytic area.
//! * **Lower bound.** A generous fraction of the same figure, under-counted
//!   for exactly those two effects.
//! * **Location, not just magnitude.** Every changed pixel must lie inside
//!   the union of the two wings' own projected rects. That is the assertion a
//!   frame-wide tint, a fogging regression or a mis-transformed instance
//!   fails and a bare count cannot see, and its failure prints the offending
//!   bounding box.
//!
//! [`RenderStats::elytra_wings_drawn`] is asserted exactly alongside: **2** on
//! the subject (one per wing — an odd number would be the "half the elytra is
//! missing" bake failure `ElytraMesh::load`'s own gate can produce), 0 on the
//! control. That counter is corroboration and **not** the evidence: the
//! neuter below left it reading 2 while nothing drew, because it is
//! incremented in `prepare_elytra`, one layer above the draw.
//!
//! # The neuter, observed rather than described
//!
//! Disabling the elytra arm of `gpu/frame.rs`'s draw loop — the exact defect
//! this feature shipped with — took the measurement from 4710 changed pixels
//! to **0**, and the gate failed on the lower bound. Reinstating it restored
//! 4710 against a 7044 px analytic silhouette (67%).
//!
//! # The negative control
//!
//! The **same** mob, same pose, same camera, with `equipment: Vec::new()`.
//! `prepare_elytra` runs unconditionally next to `prepare_entities` regardless
//! of what the mob wears, so the control exercises the same machinery and
//! finds nothing to draw — any measured difference is attributable to the
//! elytra itself, not to whether the elytra *pass* ran.
//!
//! Fail-closed, like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip — the wings have no synthetic-texture fallback for a
//! wearer without a cape, so a missing pack would otherwise read as a quiet,
//! indistinguishable-from-passing zero.
//!
//! ```text
//! cargo test -p lodestone-shell --test elytra_wings_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::EntityDraw;
use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_assets::ResourceLocation;
use lodestone_model::event::EquipmentSlot;
use lodestone_render::{
    AnimInput, Camera, ElytraMesh, EntityModelSet, GpuContext, HeadlessTarget, RenderTarget,
    elytra_rest_rotations, elytra_wing_transform,
};

const W: u32 = 320;
const H: u32 = 240;

/// The bytes the sky clear actually lands on in this readback — see
/// `armour_pixels.rs`'s identically-named helper for why this conversion is
/// derived from [`SKY_COLOR`] rather than typed twice.
fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}

/// Pixels whose colour differs between two frames of the same scene, with the
/// bounding box of that set — "what did the elytra layer touch, and where".
///
/// The box is what makes a failure diagnosable rather than a bare number: a
/// degenerate or frame-sized box says the difference is not the wings, which
/// no count can tell you. Returned in pixel coordinates, origin top-left,
/// matching [`project`]'s convention so the two are directly comparable.
fn changed_pixels(a: &[u8], b: &[u8], w: u32) -> (usize, Option<(f32, f32, f32, f32)>) {
    let mut count = 0usize;
    let (mut lo_x, mut hi_x, mut lo_y, mut hi_y) = (
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
    );
    for (i, (pa, pb)) in a.chunks_exact(4).zip(b.chunks_exact(4)).enumerate() {
        let d = (i32::from(pa[0]) - i32::from(pb[0])).abs()
            + (i32::from(pa[1]) - i32::from(pb[1])).abs()
            + (i32::from(pa[2]) - i32::from(pb[2])).abs();
        if d <= 12 {
            continue;
        }
        count += 1;
        let x = (i as u32 % w) as f32;
        let y = (i as u32 / w) as f32;
        lo_x = lo_x.min(x);
        hi_x = hi_x.max(x);
        lo_y = lo_y.min(y);
        hi_y = hi_y.max(y);
    }
    let bbox = (count > 0).then_some((lo_x, lo_y, hi_x, hi_y));
    (count, bbox)
}

/// Project a world-space point to a pixel coordinate on a `w`x`h` target,
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
fn a_mob_wearing_an_elytra_draws_wings_the_same_mob_bare_does_not() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let state = RenderState::new(device, queue, format, W, H, None);

    // Same fixture shape as `armour_pixels.rs` — camera at the origin, mob a
    // few blocks away, `AnimInput::REST` so no walk cycle moves the body part
    // the analytic projection below is composed onto.
    //
    // `yaw: 0.0` puts the wearer's **back** to the camera, which is where the
    // wings are, and that choice is load-bearing rather than cosmetic: at
    // `yaw: 180.0` the same scene measured 974 changed pixels against a 4907
    // px analytic silhouette, because the torso occludes the wings and only
    // their outer fringe survives the depth test — every changed pixel there
    // was a *new* one, none a repaint. The pass draws the wings either way;
    // it is the bracket that needs a view where most of the geometry is
    // actually visible.
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

    let subject = EntityDraw {
        hurt: false,
        block_state: None,
        item_frame_rotation: 0,
        id: 1,
        // A zombie rather than a player: `WingsLayer` sits on
        // `HumanoidMobRenderer` as well as `AvatarRenderer`, so a mob in an
        // elytra really does grow wings in vanilla — and using one keeps this
        // gate off the remote-skin fetch path entirely.
        type_path: std::sync::Arc::from("zombie"),
        item: None,
        item_model: None,
        item_skin: None,
        main_arm_left: false,
        equipment: vec![(
            EquipmentSlot::Chest,
            ResourceLocation::parse("minecraft:elytra").unwrap(),
        )],
        equipment_skin: Vec::new(),
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
        // Not a player, so no cape URL can override the jar elytra sheet.
        player_skin: None,
        variant_sheet: None,
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
        painting: None,
        firework: None,
        projectile_owner: None,
    };
    // The negative control: identical in every respect except the chest slot.
    let control = EntityDraw {
        id: 2,
        equipment: Vec::new(),
        ..subject.clone()
    };

    let mut shoot = |draw: &EntityDraw| -> (Vec<u8>, lodestone::gpu::RenderStats) {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(
            device,
            queue,
            frame.view(),
            &camera,
            None,
            std::slice::from_ref(draw),
        );
        (target.read_texels(device, queue), stats)
    };

    let (subject_px, subject_stats) = shoot(&subject);
    let (control_px, control_stats) = shoot(&control);

    let sky = sky_bytes();
    let (changed, changed_bbox) = changed_pixels(&subject_px, &control_px, W);

    // --- The analytic bracket, from real baked geometry. ---------------------
    let models = EntityModelSet::load();
    let instance = models
        .resolve(
            &subject.type_path,
            subject.feet,
            subject.yaw,
            subject.scale,
            &subject.anim,
        )
        .expect("zombie resolves in the real corpus");
    let wearer = models.get(instance.model).expect("zombie mesh present");
    let view_proj = camera.view_projection();
    let mesh = ElytraMesh::load();
    let (x_rot, y_rot, z_rot) = elytra_rest_rotations();

    // The exact same composition `prepare_elytra` builds, per wing: the
    // wearer's resolved `"body"` matrix times `elytra_wing_transform`. Each
    // wing is a closed convex box, so its screen silhouette is exactly the
    // summed projected area of its front-facing quads — and equally exactly
    // the summed area of its back-facing ones, which is the control on this
    // arithmetic asserted below. `push_part_quads` emits four vertices per
    // quad in order, so the part's vertices chunk cleanly into quads.
    let mut front_area = 0.0f32;
    let mut back_area = 0.0f32;
    let (mut union_lo_x, mut union_hi_x, mut union_lo_y, mut union_hi_y) = (
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
    );
    let mut wings_projected = 0usize;
    for (wing, range, body_index) in mesh.attach(&wearer.skeleton) {
        let body_m = instance.part_transforms[body_index];
        let m = body_m * elytra_wing_transform(wing, x_rot, y_rot, z_rot, false);
        let verts = &mesh.vertices
            [range.vertex_start as usize..(range.vertex_start + range.vertex_count) as usize];
        assert!(
            !verts.is_empty() && verts.len() % 4 == 0,
            "the {wing:?} wing must carry whole baked quads to derive an expected figure \
             from; got {} vertices",
            verts.len()
        );
        let (mut lo_x, mut hi_x, mut lo_y, mut hi_y) = (
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        );
        for quad in verts.chunks_exact(4) {
            let pts: Vec<(f32, f32)> = quad
                .iter()
                .map(|v| {
                    let p = m.transform_point3(glam::Vec3::from(v.position));
                    project(view_proj, p, W, H)
                })
                .collect();
            // Shoelace over the four projected corners. The sign is the
            // winding, i.e. which way the face points once projected.
            let mut area2 = 0.0f32;
            for i in 0..4 {
                let (x0, y0) = pts[i];
                let (x1, y1) = pts[(i + 1) % 4];
                area2 += x0 * y1 - x1 * y0;
            }
            if area2 > 0.0 {
                front_area += area2 * 0.5;
            } else {
                back_area += -area2 * 0.5;
            }
            for &(x, y) in &pts {
                lo_x = lo_x.min(x);
                hi_x = hi_x.max(x);
                lo_y = lo_y.min(y);
                hi_y = hi_y.max(y);
            }
        }
        union_lo_x = union_lo_x.min(lo_x);
        union_hi_x = union_hi_x.max(hi_x);
        union_lo_y = union_lo_y.min(lo_y);
        union_hi_y = union_hi_y.max(hi_y);
        wings_projected += 1;
        eprintln!(
            "{wing:?} wing screen rect = ({lo_x:.1}, {lo_y:.1})..({hi_x:.1}, {hi_y:.1})"
        );
    }

    // Sanity on this test's own arithmetic before trusting the bracket built
    // from it. A closed convex box projects the same silhouette from the
    // front as from the back, so these two independently-summed halves must
    // agree — a mismatch is a mistake here, not a claim about the renderer.
    assert_eq!(
        wings_projected, 2,
        "both wings must attach to the zombie rig for the bracket to mean anything"
    );
    assert!(
        front_area > 1.0 && (front_area - back_area).abs() < front_area * 0.05,
        "the analytic projection itself is broken (front_area={front_area:.2}, \
         back_area={back_area:.2}): a closed box's front and back faces project the same \
         silhouette, so these must agree — a mistake in this test's own math, not a claim \
         about the renderer"
    );
    let silhouette = front_area;

    eprintln!("=== elytra pixel gate ===");
    eprintln!(
        "subject (elytra) non-sky px = {}",
        subject_px
            .chunks_exact(4)
            .filter(|px| {
                let d = (i32::from(px[0]) - i32::from(sky[0])).abs()
                    + (i32::from(px[1]) - i32::from(sky[1])).abs()
                    + (i32::from(px[2]) - i32::from(sky[2])).abs();
                d > 60
            })
            .count()
    );
    eprintln!("changed px (subject vs control) = {changed}");
    eprintln!("changed bbox                    = {changed_bbox:?}");
    eprintln!(
        "analytic wing silhouette        = {silhouette:.1} px \
         (union rect ({union_lo_x:.1}, {union_lo_y:.1})..({union_hi_x:.1}, {union_hi_y:.1}))"
    );
    eprintln!(
        "subject elytra_wings_drawn      = {}",
        subject_stats.elytra_wings_drawn
    );
    eprintln!(
        "control elytra_wings_drawn      = {}",
        control_stats.elytra_wings_drawn
    );

    // The exact, non-approximate corroboration: two wings, one per
    // `ElytraMesh::attach` entry, and none at all with an empty chest slot.
    assert_eq!(
        subject_stats.elytra_wings_drawn, 2,
        "an elytra is two wings; elytra_wings_drawn={} means the attach chain did not run \
         (no vanilla pack? see the #[ignore] reason), and an odd number means the bake \
         produced only one of the two",
        subject_stats.elytra_wings_drawn
    );
    assert_eq!(
        control_stats.elytra_wings_drawn, 0,
        "the bare control has an empty chest slot, but elytra_wings_drawn={}",
        control_stats.elytra_wings_drawn
    );

    // The load-bearing pixel assertions: magnitude, both sides.
    let floor = (silhouette * 0.4).max(1.0);
    assert!(
        changed as f32 > floor,
        "the elytra layer should repaint most of its own projected area; got \
         changed={changed} px, expected more than {floor:.1} (40% of the analytic wing \
         silhouette {silhouette:.1}). Zero, or far below, means the wings are not reaching \
         pixels at all — which is exactly the state this gate was written for."
    );
    let ceiling = silhouette * 1.1;
    assert!(
        (changed as f32) < ceiling,
        "the elytra layer changed {changed} px, more than its own geometry can cover \
         ({ceiling:.1} = the analytic silhouette {silhouette:.1} plus 10% rasterisation \
         slack): something outside the wings is repainting between the two frames."
    );

    // And location, which the count cannot see: nothing outside the wings'
    // own projected extent may have changed. One pixel of slack per side for
    // the rasteriser's edge coverage.
    let (bb_lo_x, bb_lo_y, bb_hi_x, bb_hi_y) =
        changed_bbox.expect("changed pixels exist, so a bounding box does");
    assert!(
        bb_lo_x >= union_lo_x - 1.0
            && bb_hi_x <= union_hi_x + 1.0
            && bb_lo_y >= union_lo_y - 1.0
            && bb_hi_y <= union_hi_y + 1.0,
        "pixels changed outside the wings' own projected extent: changed bbox \
         ({bb_lo_x:.1}, {bb_lo_y:.1})..({bb_hi_x:.1}, {bb_hi_y:.1}) is not inside the wing \
         union ({union_lo_x:.1}, {union_lo_y:.1})..({union_hi_x:.1}, {union_hi_y:.1}). \
         The elytra layer is painting somewhere the elytra is not."
    );
}
