//! Pixel gate: a firework rocket must draw its item model, must draw it
//! **differently** when it was fired from a crossbow, and must draw **nothing**
//! when it is riding a gliding player — all through the real
//! [`RenderState::render`] path.
//!
//! `cargo xtask world-coverage` reported `firework_rocket` as stranded — named
//! only in a shadow-radius table, reaching no geometry — which is the state
//! this gate was written against.
//!
//! # Which half this verifies, and which it does not
//!
//! It verifies the **draw**: `EntityDraw::type_path` + `EntityDraw::item` +
//! `EntityDraw::firework` -> `merge_firework_rocket` -> pixels. It installs its
//! own `EntityDraw`, so it says nothing about the producer — that the wire's
//! `DATA_SHOT_AT_ANGLE` and `DATA_ATTACHED_TO_TARGET` really land in that field
//! is `crates/protocol/v770`'s question, and the decode gates there cover it
//! separately.
//!
//! # Three arms, and why the first alone is not enough
//!
//! **It draws.** Against an entity of the same type with no item geometry
//! reachable at all, the rocket's own pixels are the difference. That is the
//! arm that fails if the type check never fires.
//!
//! **The angle bit changes the pose.** A crossbow-fired rocket is spun out of
//! the camera plane by three fixed rotations, so it must not render
//! identically to a plain one. Coverage cannot see this: both draw the same
//! item, at the same place, at the same size — only the pose differs, and a
//! build that ignored the bit would pass every coverage check while drawing
//! every rocket flat.
//!
//! **An attached rocket draws nothing.** `FireworkRocketEntity.shouldRender`
//! returns false for the elytra boost, so this is not an optimisation — a
//! rocket sprite hanging inside a gliding player is a visible defect, and it is
//! the defect this arm would have shipped.
//!
//! # Measured
//!
//! Plain **821** changed pixels against the empty frame, angled **140**, and
//! the two differ by **865**. The angled figure being *smaller* is the correct
//! result rather than a defect: the three rotations tip the flat item sprite
//! about the camera's own X axis, so a camera looking straight at the rocket
//! sees it nearly edge-on. Attached is **0** on both the counter and the
//! pixels.
//!
//! The neuter was observed rather than described: disabling the type-check
//! branch in `prepare_item_geometry` took every arm to 0 and the gate failed on
//! the first.
//!
//! Fail-closed: no GPU adapter or no `client.jar` is a failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test firework_rocket_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::EntityDraw;
use lodestone::gpu::RenderState;
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_ecs::entity::FireworkFlags;
use lodestone_render::{AnimInput, BlockModels, Camera, GpuContext, HeadlessTarget, RenderTarget};

/// The item a rocket falls back to when the wire reported no stack —
/// `FireworkRocketEntity.getDefaultItem()`, and what this gate draws.
const ITEM: &str = "minecraft:firework_rocket";

const W: u32 = 320;
const H: u32 = 240;

/// Pixels differing between two frames of the same scene. See
/// `elytra_wings_pixels.rs`'s sibling for the shape.
fn changed_pixels(a: &[u8], b: &[u8]) -> usize {
    a.chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(pa, pb)| {
            let d = (i32::from(pa[0]) - i32::from(pb[0])).abs()
                + (i32::from(pa[1]) - i32::from(pb[1])).abs()
                + (i32::from(pa[2]) - i32::from(pb[2])).abs();
            d > 12
        })
        .count()
}

fn rocket(id: i32, type_path: &str, flags: Option<FireworkFlags>, at: glam::Vec3) -> EntityDraw {
    EntityDraw {
        hurt: false,
        block_state: None,
        item_frame_rotation: 0,
        painting: None,
        firework: flags,
        projectile_owner: None,
        id,
        type_path: std::sync::Arc::from(type_path),
        // Left `None` deliberately: the draw path falls back to
        // `FireworkRocketEntity.getDefaultItem()`, which is what a rocket whose
        // item field was never marked dirty genuinely draws as, so this is the
        // *common* case rather than a degraded one.
        item: None,
        item_model: None,
        item_skin: None,
        main_arm_left: false,
        equipment: Vec::new(),
        equipment_skin: Vec::new(),
        equipment_dye: Vec::new(),
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
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_firework_rocket_draws_angles_with_its_bit_and_vanishes_when_attached() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;

    // The item path needs a real baked atlas: `RenderState::new(.., None)`
    // installs no `ModelRenderer` and `prepare_item_geometry` then returns
    // before any of this. That is not a hypothetical — this gate was written
    // with `None` first and reported a flat zero from every arm, which reads
    // exactly like a dead draw path.
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "GPU gate opted in but the vanilla pack did not load; set LODESTONE_ASSETS \
             to a pack root with client.jar + generated/reports/blocks.json. Banner: {:?}",
            resources.banner
        )
    });
    {
        // The precondition its sibling `dropped_item_pixels.rs` documents:
        // without this the gate could be measuring the absence of an *item*
        // rather than the absence of a *draw*.
        let item: ResourceLocation = ITEM.parse().expect("valid item id");
        let models: &BlockModels = atlas
            .models()
            .expect("the vanilla load must attach baked block models");
        assert!(
            models.item(&item).is_some(),
            "{ITEM} must have baked geometry for this gate to mean anything"
        );
    }

    let mut target = HeadlessTarget::new(device, W, H, format);
    let state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));

    // Close enough that a 1-block item model covers a real number of pixels.
    let at = glam::Vec3::new(0.0, 1.0, 1.5);
    let camera = Camera {
        position: glam::Vec3::new(0.0, 1.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    let plain = rocket(1, "firework_rocket", None, at);
    let angled = rocket(
        2,
        "firework_rocket",
        Some(FireworkFlags {
            attached: false,
            shot_at_angle: true,
        }),
        at,
    );
    let attached = rocket(
        3,
        "firework_rocket",
        Some(FireworkFlags {
            attached: true,
            shot_at_angle: false,
        }),
        at,
    );
    // The empty reference frame. An entity type nothing in the world draws and
    // that resolves to no rig, no item and no billboard — so this frame is the
    // scene without the rocket, and every difference below is the rocket.
    let empty = rocket(4, "area_effect_cloud", None, at);

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

    let (plain_px, plain_stats) = shoot(&plain);
    let (angled_px, angled_stats) = shoot(&angled);
    let (attached_px, attached_stats) = shoot(&attached);
    let (empty_px, _) = shoot(&empty);

    let plain_coverage = changed_pixels(&plain_px, &empty_px);
    let angled_coverage = changed_pixels(&angled_px, &empty_px);
    let attached_coverage = changed_pixels(&attached_px, &empty_px);
    let pose_difference = changed_pixels(&plain_px, &angled_px);

    eprintln!("=== firework rocket pixel gate ===");
    eprintln!("plain    coverage = {plain_coverage} px, projectiles_drawn = {}", plain_stats.projectiles_drawn);
    eprintln!("angled   coverage = {angled_coverage} px, projectiles_drawn = {}", angled_stats.projectiles_drawn);
    eprintln!("attached coverage = {attached_coverage} px, projectiles_drawn = {}", attached_stats.projectiles_drawn);
    eprintln!("plain vs angled   = {pose_difference} px");

    // Arm 1: it draws at all. The counter and the pixels together, because
    // either alone has a known failure mode — the counter is incremented one
    // layer above the draw, and a coverage number cannot say *what* drew.
    assert_eq!(
        plain_stats.projectiles_drawn, 1,
        "one rocket is in view; projectiles_drawn={} means the type check never fired \
         (no vanilla pack? see the #[ignore] reason)",
        plain_stats.projectiles_drawn
    );
    assert!(
        plain_coverage > 200,
        "a firework rocket a block and a half from the camera should cover a real number \
         of pixels; got {plain_coverage}. Zero means it is not reaching pixels at all — \
         which is exactly the state this gate was written for."
    );

    // Arm 2: the angle bit is read. Same item, same place, same size — only
    // the pose differs, so coverage is blind to this and only a frame-to-frame
    // comparison can see it.
    assert_eq!(angled_stats.projectiles_drawn, 1, "an angled rocket still draws");
    assert!(
        pose_difference > plain_coverage / 4,
        "a crossbow-fired rocket is spun onto its flight axis by three fixed rotations \
         and must not render like a plain one; only {pose_difference} px differ against a \
         plain coverage of {plain_coverage}. A build that ignored DATA_SHOT_AT_ANGLE would \
         pass every coverage check and fail exactly here."
    );

    // Arm 3: the attached suppression. `FireworkRocketEntity.shouldRender`
    // returns false for the elytra boost — a rocket sprite hanging inside a
    // gliding player is the defect this arm exists to stop.
    assert_eq!(
        attached_stats.projectiles_drawn, 0,
        "a rocket attached to a gliding player must not be submitted at all, but \
         projectiles_drawn={}",
        attached_stats.projectiles_drawn
    );
    assert_eq!(
        attached_coverage, 0,
        "a rocket attached to a gliding player must draw nothing; {attached_coverage} px \
         changed against the empty frame"
    );
}
