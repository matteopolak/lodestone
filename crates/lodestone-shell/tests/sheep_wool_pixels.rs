//! Pixel gate: a woolly sheep must draw **more** silhouette than the same
//! sheep sheared (issue #53) — driven through the real
//! [`RenderState::render`] path, the same call `app.rs`'s frame loop makes.
//!
//! `lodestone-render`'s own `sheep_wool_pixels.rs` proves the mesh/tint/pose
//! machinery works by reimplementing `ArmourMesh::attach`'s discipline
//! locally against public API — it could not do otherwise, since it does not
//! own `lodestone-render/src/entity.rs`, where `WoolMesh` now lives. That
//! test is a closed loop with respect to this crate: it never calls
//! `RenderState::prepare_wool`, so it cannot see whether anything in
//! `lodestone-shell` actually reaches it. Per `CLAUDE.md`'s dominant defect
//! class — a subsystem built, tested, and reaching zero pixels because
//! nothing calls it — this gate drives the real shell path instead.
//!
//! # The metric
//!
//! Same technique as `armour_pixels.rs`'s sibling gate: wool draws **over**
//! the same sheep body at the same screen position, so almost every wool
//! pixel recolours a pixel the bare body already lit. Only the outward
//! *inflation* creates new, previously-sky pixels, in a thin ring around the
//! silhouette. The body part alone is projected — `sheep_wool_model`'s own
//! `+1.75` (model units) body inflation, the largest of the four — through
//! the *same* [`Camera::view_projection`] this test's render call uses, from
//! the **real** baked body vertices (`EntityModelSet::load` corpus, not a
//! remembered literal), giving a real, mechanically-derived lower bound
//! rather than a number typed once and never re-derived. Head and legs each
//! add their own ring on top and are not counted, so the observed delta is
//! asserted against a generous *fraction* of the body-only estimate, not the
//! estimate itself.
//!
//! # The negative control
//!
//! The briefing's own suggested pair: a **sheared** sheep against a
//! **woolly** one, same pose, same camera, differing only in
//! `EntityDraw::wool.sheared`. `prepare_wool` runs unconditionally next to
//! `prepare_entities` regardless of shear state (mirroring how
//! `prepare_armour` runs unconditionally regardless of equipment), so the
//! control exercises the same machinery and finds nothing to draw — any
//! measured difference is attributable to the wool itself, not to whether the
//! wool *pass* ran. [`RenderStats::wool_layers_drawn`] corroborates exactly:
//! 1 on the subject, 0 on the sheared control.
//!
//! Fail-closed, like its siblings: no GPU adapter or no `client.jar` is a
//! failure, never a skip — wool has no synthetic-texture fallback (mirroring
//! armour), so a missing pack would otherwise read as a quiet,
//! indistinguishable-from-passing zero.
//!
//! ```text
//! cargo test -p lodestone-shell --test sheep_wool_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::{EntityDraw, SheepWool};
use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::{AnimInput, Camera, EntityModelSet, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// See `armour_pixels.rs`'s identically-named constant for why this
/// conversion is worth its own doc rather than a bare literal.
fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}

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
fn a_woolly_sheep_draws_more_silhouette_than_a_sheared_one() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let state = RenderState::new(device, queue, format, W, H, None);

    // Side-on, matching `lodestone-render`'s own sheep wool gate, so the
    // body's growth reads as a silhouette change rather than mostly depth.
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
        id: 1,
        type_path: "sheep".to_owned(),
        item: None,
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        feet,
        yaw: 90.0,
        head_yaw: 90.0,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput::REST,
        wool: Some(SheepWool {
            color: 0,
            sheared: false,
        }),
        count: 1,
        name_tag: None,
        item_use: None,
        creeper_swelling: 0.0,
        on_fire: false,
    };
    // The negative control: identical in every respect except shear state —
    // the briefing's own suggested pair.
    let control = EntityDraw {
        id: 2,
        wool: Some(SheepWool {
            color: 0,
            sheared: true,
        }),
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
        .expect("sheep resolves in the real corpus");
    let wearer = models.get(instance.model).expect("sheep mesh present");
    let body_idx = wearer
        .skeleton
        .index_of("body")
        .expect("the sheep rig has a body part");
    let body_range = wearer.parts[body_idx];
    let body_verts = &wearer.vertices[body_range.vertex_start as usize
        ..(body_range.vertex_start + body_range.vertex_count) as usize];
    assert!(
        !body_verts.is_empty(),
        "the body part must carry real baked vertices to derive an expected figure from"
    );
    let (mut min_x, mut max_x, mut min_y, mut max_y, mut min_z, mut max_z) = (
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
    );
    for v in body_verts {
        min_x = min_x.min(v.position[0]);
        max_x = max_x.max(v.position[0]);
        min_y = min_y.min(v.position[1]);
        max_y = max_y.max(v.position[1]);
        min_z = min_z.min(v.position[2]);
        max_z = max_z.max(v.position[2]);
    }
    let body_m = instance.part_transforms[body_idx];
    let view_proj = camera.view_projection();
    // `sheep_wool_model`'s own body inflation: `+1.75` model units
    // (`cube(...).grown(1.75)` in `entity_models.rs`), the largest of the
    // four wool parts (head `+0.6`, legs `+0.5`) and therefore the safest
    // single-part lower bound. `CubeDeformation` grows a box symmetrically in
    // all three local axes, not just the two the camera happens to see as
    // "width"/"height" — the sheep body part carries its own 90° pivot
    // rotation (`PartPose::offset_and_rotation(.., PI / 2.0, 0.0, 0.0)`), so
    // local X/Y/Z do **not** line up with screen X/Y the way a part with an
    // identity pivot (e.g. the zombie chest `armour_pixels.rs` projects)
    // would. Projecting all 8 corners of the bare and grown boxes and taking
    // each one's own on-screen bounding rect sidesteps that entirely: it
    // needs no assumption about which local axis is "lateral" after the
    // part's own rotation.
    let inflation = 1.75 / 16.0;

    let screen_bbox = |dx: f32, dy: f32, dz: f32| -> (f32, f32, f32, f32) {
        let (mut lo_x, mut hi_x, mut lo_y, mut hi_y) =
            (f32::INFINITY, f32::NEG_INFINITY, f32::INFINITY, f32::NEG_INFINITY);
        for &x in &[min_x - dx, max_x + dx] {
            for &y in &[min_y - dy, max_y + dy] {
                for &z in &[min_z - dz, max_z + dz] {
                    let (sx, sy) = project(view_proj, body_m.transform_point3(glam::Vec3::new(x, y, z)), W, H);
                    lo_x = lo_x.min(sx);
                    hi_x = hi_x.max(sx);
                    lo_y = lo_y.min(sy);
                    hi_y = hi_y.max(sy);
                }
            }
        }
        (lo_x, hi_x, lo_y, hi_y)
    };

    let (bare_lo_x, bare_hi_x, bare_lo_y, bare_hi_y) = screen_bbox(0.0, 0.0, 0.0);
    let (wool_lo_x, wool_hi_x, wool_lo_y, wool_hi_y) = screen_bbox(inflation, inflation, inflation);

    let width_px = bare_hi_x - bare_lo_x;
    let height_px = bare_hi_y - bare_lo_y;
    let width_growth_px = (wool_hi_x - wool_lo_x) - width_px;
    let height_growth_px = (wool_hi_y - wool_lo_y) - height_px;

    assert!(
        width_growth_px > 0.0 && height_growth_px > 0.0,
        "the analytic projection itself is broken (width_growth={width_growth_px:.2}, \
         height_growth={height_growth_px:.2}, both should be positive) — a sign error in \
         this test's own math, not a claim about the renderer"
    );

    let body_ring_estimate = 2.0 * width_growth_px * height_px + 2.0 * height_growth_px * width_px;

    eprintln!("=== sheep wool pixel gate ===");
    eprintln!("subject (woolly) non-sky px = {subject_count}");
    eprintln!("control (sheared) non-sky px = {control_count}");
    eprintln!("delta                        = {delta}");
    eprintln!(
        "body-only ring estimate      = {body_ring_estimate:.1} px (lower bound; head/legs \
         not counted)"
    );
    eprintln!("subject wool_layers_drawn    = {}", subject_stats.wool_layers_drawn);
    eprintln!("control wool_layers_drawn    = {}", control_stats.wool_layers_drawn);

    // Exact, non-approximate corroboration: one wool layer on the woolly
    // subject, none on the sheared control — vanilla's own gate applied at
    // exactly the point that draws the mesh.
    assert_eq!(
        subject_stats.wool_layers_drawn, 1,
        "the woolly subject should draw exactly one wool layer; wool_layers_drawn={} means \
         the resolution chain did not run (no vanilla pack? see the #[ignore] reason)",
        subject_stats.wool_layers_drawn
    );
    assert_eq!(
        control_stats.wool_layers_drawn, 0,
        "the sheared control must draw no wool layer at all (vanilla's own gate), but \
         wool_layers_drawn={}",
        control_stats.wool_layers_drawn
    );

    let floor = (body_ring_estimate * 0.2).max(1.0);
    assert!(
        delta as f32 > floor,
        "the woolly sheep should draw a visibly larger silhouette than the sheared one (a \
         new, previously-sky ring from the outer inflation); got delta={delta}, expected more \
         than {floor:.1} px (20% of the body-only analytic estimate {body_ring_estimate:.1}). \
         Far below (or negative) means wool is not reaching pixels."
    );
    assert!(
        (delta as usize) < control_count.max(1) * 3,
        "the wool delta ({delta}) is implausibly large next to the sheared sheep's own \
         silhouette ({control_count}) — likely a broken control rather than a real wool effect"
    );
    assert!(
        control_count > 100,
        "the sheared sheep itself should reach a substantial run of pixels ({control_count}); \
         if this is near zero the whole entity path is broken, not just wool"
    );
}
