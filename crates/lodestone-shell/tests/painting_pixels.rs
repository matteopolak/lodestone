//! Pixel gate: a painting must draw its **own variant's** sprite on the wall,
//! driven through the real [`RenderState::render`] path.
//!
//! `cargo xtask world-coverage` reported `painting` as stranded — named only in
//! a shadow-radius table, reaching no geometry — which is the state this gate
//! was written against.
//!
//! # Which half this verifies, and which it does not
//!
//! It verifies the **draw**: `EntityDraw::painting` + `EntityDraw::yaw` ->
//! `prepare_paintings` -> `PaintingDrawBatch` -> pixels, through the production
//! pipeline, mesh bake, texture load and camera. It installs its own
//! `EntityDraw`, so it says nothing about the producer — that the wire's
//! painting-variant metadata really lands in that field is
//! `crates/protocol/v770`'s question, and no assertion here can see it.
//!
//! # Two assertions, because one of them is not enough
//!
//! **Coverage** is the obvious one: a painting is opaque and hung against the
//! sky here, so the pixels that change between "variant known" and "variant
//! unknown" are exactly the painting's own silhouette. That is bracketed
//! against the front face's analytic projected area — the four corners of the
//! real baked quad, pushed through the real
//! [`painting_matrix`](lodestone_render::painting::painting_matrix) and the same
//! [`Camera::view_projection`] the render call uses.
//!
//! **Discrimination** is the one that matters more, and coverage cannot see it:
//! two variants *of the same size* must render **differently**. Every 4x4
//! painting shares one mesh and one shape, so a pass that bound one shared
//! sheet — or bound the back tile to the front face, or looked the texture up
//! by shape instead of by variant — would pass every coverage check ever
//! written while drawing `pointer` for all 51 variants. This is also what makes
//! the fixture's orientation self-checking: the two variants differ only on the
//! **front** face, so if the camera were looking at the back the assertion
//! would fail rather than quietly measure the shared tile.
//!
//! [`RenderStats::paintings_drawn`] is asserted exactly alongside: 1 on each
//! subject, 0 on the control.
//!
//! # The negative control
//!
//! The same entity with `painting: None` — which is not a synthetic state: it
//! is what a data-pack-added variant resolves to, and it is what *every*
//! painting resolved to before this pass existed. `prepare_paintings` runs
//! unconditionally next to `prepare_entities` either way, so the control
//! exercises the same machinery and finds nothing to draw.
//!
//! Fail-closed, like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip — paintings have no synthetic-texture fallback, so a
//! missing pack would otherwise read as a quiet, indistinguishable-from-passing
//! zero.
//!
//! ```text
//! cargo test -p lodestone-shell --test painting_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::EntityDraw;
use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{
    AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget,
    painting::{painting_matrix, painting_mesh, painting_size},
};

const W: u32 = 320;
const H: u32 = 240;

/// The two 4x4 variants the discrimination assertion compares. Same shape, so
/// they share a mesh and a batch key on everything except the texture — which
/// is precisely the axis under test.
const SUBJECT: &str = "pointer";
const OTHER: &str = "pigscene";

/// Pixels whose colour differs between two frames of the same scene, with the
/// bounding box of that set. See `elytra_wings_pixels.rs`'s identical helper
/// for why the box matters as much as the count.
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
    ((count), (count > 0).then_some((lo_x, lo_y, hi_x, hi_y)))
}

/// Project a world-space point to a pixel coordinate, wgpu's NDC convention.
fn project(view_proj: glam::Mat4, world: glam::Vec3, w: u32, h: u32) -> (f32, f32) {
    let clip = view_proj * glam::Vec4::new(world.x, world.y, world.z, 1.0);
    let ndc_x = clip.x / clip.w;
    let ndc_y = clip.y / clip.w;
    (
        (ndc_x * 0.5 + 0.5) * w as f32,
        (1.0 - (ndc_y * 0.5 + 0.5)) * h as f32,
    )
}

fn painting_draw(id: i32, variant: Option<&'static str>, centre: glam::Vec3) -> EntityDraw {
    EntityDraw {
        hurt: false,
        block_state: None,
        item_frame_rotation: 0,
        painting: variant,
        id,
        type_path: std::sync::Arc::from("painting"),
        item: None,
        main_arm_left: false,
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_trim: Vec::new(),
        // A painting's wire position is the slab's **centre**, not a mob's
        // feet — `Painting.calculateBoundingBox` places it there.
        feet: centre,
        // 180 puts the painting's *front* face toward a camera looking down
        // +Z: `painting_matrix` applies vanilla's `180 - yaw`, so this is the
        // identity rotation and the front (local -Z) stays at -Z. The
        // discrimination assertion below is what checks this, rather than a
        // comment claiming it.
        yaw: 180.0,
        head_yaw: 180.0,
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
        player_skin: None,
        variant_sheet: None,
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
    }
}

#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_painting_draws_its_own_variant_and_an_unknown_one_draws_nothing() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let state = RenderState::new(device, queue, format, W, H, None);

    // Far enough back that a 4x4 painting fits the frame with sky around it, so
    // the coverage measurement is the painting's silhouette rather than a crop
    // of it.
    let centre = glam::Vec3::new(0.0, 1.0, 8.0);
    let camera = Camera {
        position: glam::Vec3::new(0.0, 1.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    let subject = painting_draw(1, Some(SUBJECT), centre);
    let other = painting_draw(2, Some(OTHER), centre);
    // The negative control, and not a synthetic one: `None` is what a
    // data-pack variant resolves to, and what every painting resolved to
    // before this pass existed.
    let control = painting_draw(3, None, centre);

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
    let (other_px, other_stats) = shoot(&other);
    let (control_px, control_stats) = shoot(&control);

    let (changed, changed_bbox) = changed_pixels(&subject_px, &control_px, W);
    let (between_variants, _) = changed_pixels(&subject_px, &other_px, W);

    // --- The analytic expectation, from the real baked quad. -----------------
    let size = painting_size(SUBJECT).expect("the subject is a table variant");
    let mesh = painting_mesh(size.width, size.height);
    let matrix = painting_matrix(centre, subject.yaw);
    let view_proj = camera.view_projection();
    // The front mesh's own vertices, projected: the shoelace area over its
    // convex hull is the painting's screen silhouette, since the whole front
    // face is one flat rectangle however finely it is subdivided into cells.
    let corners: Vec<(f32, f32)> = {
        let verts = &mesh.front.0;
        assert!(
            !verts.is_empty(),
            "the front mesh must carry real baked vertices to derive an expected figure from"
        );
        let (mut lo_x, mut hi_x, mut lo_y, mut hi_y) = (
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
        );
        for v in verts {
            let p = matrix.transform_point3(glam::Vec3::from(v.position));
            let (sx, sy) = project(view_proj, p, W, H);
            lo_x = lo_x.min(sx);
            hi_x = hi_x.max(sx);
            lo_y = lo_y.min(sy);
            hi_y = hi_y.max(sy);
        }
        vec![(lo_x, lo_y), (hi_x, hi_y)]
    };
    let (lo_x, lo_y) = corners[0];
    let (hi_x, hi_y) = corners[1];
    // The painting faces the camera square-on, so its screen rect *is* its
    // silhouette: no rotation to under-count, unlike the elytra's wings.
    let expected_area = (hi_x - lo_x) * (hi_y - lo_y);

    eprintln!("=== painting pixel gate ===");
    eprintln!("subject variant                = {SUBJECT} ({}x{})", size.width, size.height);
    eprintln!("changed px (subject vs none)   = {changed}");
    eprintln!("changed px ({SUBJECT} vs {OTHER}) = {between_variants}");
    eprintln!("changed bbox                   = {changed_bbox:?}");
    eprintln!(
        "analytic front rect            = ({lo_x:.1}, {lo_y:.1})..({hi_x:.1}, {hi_y:.1}), \
         area {expected_area:.1} px"
    );
    eprintln!("subject paintings_drawn        = {}", subject_stats.paintings_drawn);
    eprintln!("control paintings_drawn        = {}", control_stats.paintings_drawn);
    let _ = SKY_COLOR;

    assert_eq!(
        subject_stats.paintings_drawn, 1,
        "one painting is in view; paintings_drawn={} means the pass did not reach it (no \
         vanilla pack? see the #[ignore] reason)",
        subject_stats.paintings_drawn
    );
    assert_eq!(
        other_stats.paintings_drawn, 1,
        "the second variant is the same shape and must also draw; got {}",
        other_stats.paintings_drawn
    );
    assert_eq!(
        control_stats.paintings_drawn, 0,
        "a painting with no resolved variant has no size and must draw nothing, but \
         paintings_drawn={}",
        control_stats.paintings_drawn
    );

    // Coverage. A painting is opaque against sky here, so the changed set is
    // its silhouette: the observed count should land close under the analytic
    // rect (the rect is exact; only edge coverage and any texel that happens to
    // match the sky within tolerance pull it down).
    assert!(
        expected_area > 100.0,
        "the analytic projection itself is broken (area={expected_area:.1}) — a mistake in \
         this test's own math, not a claim about the renderer"
    );
    let floor = expected_area * 0.7;
    let ceiling = expected_area * 1.1;
    assert!(
        (changed as f32) > floor && (changed as f32) < ceiling,
        "the painting should cover its own projected rect; got changed={changed} px, \
         expected between {floor:.1} and {ceiling:.1} (the analytic front-face rect is \
         {expected_area:.1}). Zero, or far below, means paintings are not reaching pixels — \
         which is exactly the state this gate was written for."
    );

    // Discrimination — the assertion coverage cannot make. Two variants of the
    // same size share a mesh, a shape and a batch key; only the bound texture
    // differs, so a shared-sheet or shape-keyed lookup shows up here and
    // nowhere else. A generous floor: two paintings can agree on plenty of
    // texels, but not on most of them.
    let discrimination_floor = expected_area * 0.25;
    assert!(
        (between_variants as f32) > discrimination_floor,
        "{SUBJECT} and {OTHER} are both 4x4 and must not render alike; only \
         {between_variants} px differ, expected more than {discrimination_floor:.1}. Either \
         the front texture is looked up by shape rather than by variant, or the front face \
         is not the one facing the camera and this is measuring the shared back tile."
    );
}
