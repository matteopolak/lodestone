//! Pixel gate: a lightning bolt must draw its procedural geometry through the
//! real [`RenderState::render`] path, and must draw a **different** bolt for a
//! different entity.
//!
//! `cargo xtask world-coverage` reported `lightning_bolt` as stranded — named
//! only in a shadow-radius table, reaching no geometry. The *sky flash* has
//! existed the whole time (`lodestone_render::weather`), which is exactly what
//! made this easy to miss: a storm looked like it was doing something.
//!
//! # Which half this verifies, and which it does not
//!
//! It verifies the **draw**: an `EntityDraw` whose `type_path` is
//! `lightning_bolt` -> `LightningBoltRenderer::prepare` -> the additive
//! pipeline -> pixels. It installs its own `EntityDraw`.
//!
//! Unusually for a gate in this family, that covers rather more than usual:
//! a bolt has **no wire data at all** beyond its spawn and position (vanilla's
//! `LightningBolt` declares no synched fields and its seed is a local
//! `nextLong()`), so there is no metadata producer this could be blind to. The
//! one producer-side thing it cannot see is whether the entity reaches the
//! draw list at all.
//!
//! # Three arms
//!
//! **It draws, and by the right amount.** The vertex counter must land on
//! exactly `BOLT_VERTICES` — a multiple check, not a "greater than zero": the
//! fixed-capacity buffer's failure mode is clipping a bolt mid-walk, which
//! only an exact count can catch.
//!
//! **The pixels are additive.** Every changed pixel must be *brighter* than the
//! sky it replaced, in every channel. This is the arm that fails if the blend
//! state were a stock `ALPHA_BLENDING`: the bolt's own colour is a dim
//! blue-grey `(0.45, 0.45, 0.5)` at alpha `0.3`, so alpha-blending it over a
//! brighter sky would *darken* those pixels while still covering the same
//! silhouette. A coverage-only check cannot tell the two apart.
//!
//! **Two bolts differ.** Different entity ids seed different walks, so two
//! bolts at the same position must not render identically — the check that the
//! seed reaches the geometry rather than a constant being walked.
//!
//! Fail-closed: no GPU adapter is a failure, never a skip. Unlike its siblings
//! this needs **no vanilla pack** — a bolt is untextured — so a jar-less run is
//! not an excuse here either.
//!
//! ```text
//! cargo test -p lodestone-shell --test lightning_bolt_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::EntityDraw;
use lodestone::gpu::{RenderState, SKY_COLOR};
use lodestone_render::lightning_bolt::BOLT_VERTICES;
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

fn sky_bytes() -> [u8; 3] {
    SKY_COLOR.map(|c| (c * 255.0).round() as u8)
}

fn bolt(id: i32, type_path: &str, at: glam::Vec3) -> EntityDraw {
    EntityDraw {
        hurt: false,
        block_state: None,
        item_frame_rotation: 0,
        painting: None,
        firework: None,
        projectile_owner: None,
        id,
        type_path: std::sync::Arc::from(type_path),
        item: None,
        item_model: None,
        main_arm_left: false,
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_skin: Vec::new(),
        equipment_trim: Vec::new(),
        feet: at,
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
        player_skin: None,
        variant_sheet: None,
        experience_orb_value: None,
        cape_sway: (0.0, 0.0, 0.0),
    }
}

#[test]
#[ignore = "requires a GPU adapter"]
fn a_lightning_bolt_draws_additively_and_two_bolts_differ() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);

    let state = RenderState::new(device, queue, format, W, H, None);

    // A bolt is 128 blocks tall and wanders +-5 blocks a level, so the camera
    // stands well back and looks up to get a real length of trunk on screen.
    // The strike point is the entity position, i.e. the *bottom* of the walk.
    let strike = glam::Vec3::new(0.0, 0.0, 40.0);
    let camera = Camera {
        position: glam::Vec3::new(0.0, 2.0, 0.0),
        yaw: 0.0,
        pitch: -35.0,
        fov_y_degrees: 70.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(16, 0),
    };

    let subject = bolt(1, "lightning_bolt", strike);
    // A second bolt at the same place with a different id: a different seed,
    // and so a different walk.
    let other = bolt(2, "lightning_bolt", strike);
    // The empty reference frame — a type with no rig, no item and no pass.
    let empty = bolt(3, "area_effect_cloud", strike);

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
    let (other_px, _) = shoot(&other);
    let (empty_px, empty_stats) = shoot(&empty);

    let sky = sky_bytes();
    // Every pixel the bolt touched, split by whether it got brighter or darker
    // than the frame without it. The split is the whole of the additive arm.
    let mut brighter = 0usize;
    let mut darker = 0usize;
    for (a, b) in subject_px.chunks_exact(4).zip(empty_px.chunks_exact(4)) {
        let delta: [i32; 3] = [
            i32::from(a[0]) - i32::from(b[0]),
            i32::from(a[1]) - i32::from(b[1]),
            i32::from(a[2]) - i32::from(b[2]),
        ];
        if delta.iter().all(|d| d.abs() <= 2) {
            continue;
        }
        if delta.iter().all(|&d| d >= 0) {
            brighter += 1;
        } else {
            darker += 1;
        }
    }
    let between_bolts = subject_px
        .chunks_exact(4)
        .zip(other_px.chunks_exact(4))
        .filter(|(a, b)| {
            (i32::from(a[0]) - i32::from(b[0])).abs()
                + (i32::from(a[1]) - i32::from(b[1])).abs()
                + (i32::from(a[2]) - i32::from(b[2])).abs()
                > 6
        })
        .count();

    eprintln!("=== lightning bolt pixel gate ===");
    eprintln!("sky bytes                = {sky:?}");
    eprintln!("brighter px              = {brighter}");
    eprintln!("darker px                = {darker}");
    eprintln!("bolt 1 vs bolt 2         = {between_bolts} px");
    eprintln!(
        "lightning_bolt_vertices  = {} (one bolt is {BOLT_VERTICES})",
        subject_stats.lightning_bolt_vertices
    );
    eprintln!(
        "empty-frame vertices     = {}",
        empty_stats.lightning_bolt_vertices
    );

    // Arm 1: the walk reached the buffer whole. An exact count, because the
    // fixed-capacity buffer's own failure mode is clipping a bolt part-way and
    // a `> 0` check would call that healthy.
    assert_eq!(
        subject_stats.lightning_bolt_vertices, BOLT_VERTICES,
        "one bolt is exactly {BOLT_VERTICES} vertices; {} means the walk was clipped or \
         never ran",
        subject_stats.lightning_bolt_vertices
    );
    assert_eq!(
        empty_stats.lightning_bolt_vertices, 0,
        "an entity that is not a bolt must contribute no bolt geometry"
    );

    // Arm 2: additive. This is the assertion a coverage count cannot make —
    // the bolt's colour is darker than the sky, so alpha blending would cover
    // the same pixels and darken them.
    assert!(
        brighter > 200,
        "a bolt should brighten a real number of pixels; only {brighter} did (and {darker} \
         got darker). Zero of both means it is not reaching pixels at all."
    );
    assert_eq!(
        darker, 0,
        "{darker} pixels got *darker* where the bolt drew. The pass blends \
         (SRC_ALPHA, ONE) — additive — so every touched pixel must brighten; a stock \
         ALPHA_BLENDING would darken them, because the bolt's own (0.45, 0.45, 0.5) is \
         dimmer than this sky."
    );

    // Arm 3: the seed reaches the geometry. Two bolts at one position with
    // different ids walk differently, so they must not render alike.
    assert!(
        between_bolts > brighter / 4,
        "two bolts with different entity ids must walk differently, but only \
         {between_bolts} px differ against {brighter} lit. A constant seed — or a seed \
         that does not reach the walk — looks exactly like this."
    );
}
