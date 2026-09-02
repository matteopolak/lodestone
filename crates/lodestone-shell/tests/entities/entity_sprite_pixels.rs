//! Pixel gate: the three entity types `cargo xtask world-coverage` reported as
//! stranded — `dragon_fireball`, `fishing_bobber` and `ominous_item_spawner` —
//! must each reach real pixels through the real [`RenderState::render`] path,
//! and the fishing bobber must also draw its line back to the caster.
//!
//! All three were "named in draw code, no geometry": present in `SHADOW_RADII`,
//! decoded off the wire, drawing nothing. That is the state this gate was
//! written against, and the coverage census went 148 drawn / 7 stranded to 151
//! drawn / 4 stranded when they landed.
//!
//! # Which half this verifies, and which it does not
//!
//! It verifies the **draw**: `EntityDraw` → `prepare_entity_sprites` /
//! `fishing_line_vertices` / `merge_ominous_spawner_item` → pixels. Each arm
//! installs its own `EntityDraw`, so it says **nothing** about the producer.
//! That the wire's `ADD_ENTITY` Object Data really lands in
//! `EntityDraw::projectile_owner`, and that a spawner's `DATA_ITEM` really
//! lands in `EntityDraw::item`, are `crates/versions/26.2`'s and
//! `entities::extract_entity_draws`' questions; the ECS ingest gate in
//! `lodestone-ecs` covers the first hop of the owner chain separately.
//!
//! # The arms, and why the obvious one alone is not enough
//!
//! **Each sprite draws.** Against the same scene with the subject replaced by
//! an entity nothing draws, the subject's own pixels are the difference. This
//! is the arm that fails if the type check never fires.
//!
//! **The fishing line is separate from its bobber.** The billboard and the line
//! go through two different pipelines, and while this was being built they
//! failed independently — a bobber with no line looks like a working feature.
//! So the line is measured as the difference between two frames that differ
//! *only* in `EntityDraw::projectile_owner`, with the segment counter asserted
//! at exactly sixteen. Coverage alone is blind to this: the bobber covers the
//! same pixels either way.
//!
//! **The spawner's grow-in ramp is predicted, not merely observed to change.**
//! A half-grown item is a uniform half-scale of a full-grown one, so its
//! silhouette should cover roughly a quarter of the area. Asserting only "the
//! two differ" passes under a build that ignored `ageInTicks` and jittered the
//! cluster differently; asserting a *ratio* does not. The competing hypothesis
//! — no ramp at all, i.e. full size from tick zero — is computed alongside and
//! the measurement has to land on one.
//!
//! Fail-closed: no GPU adapter or no `client.jar` is a failure, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test entity_sprite_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::EntityDraw;
use lodestone::gpu::{RenderState, RenderStats};
use lodestone::resources::BlockResources;
use lodestone_render::{AnimInput, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// The item an ominous item spawner is holding in this scene. A solid block
/// model rather than a flat sprite, so the cluster takes
/// `submitMultipleFromCount`'s three-axis jitter branch and the silhouette is a
/// real area rather than a fan of coplanar cards.
const SPAWNER_ITEM: &str = "minecraft:diamond_block";

/// An entity type that draws nothing at all: no `entity_models` rig, no sprite
/// row, no item. Used as the reference frame so every difference below is the
/// subject and not the harness.
const DRAWS_NOTHING: &str = "area_effect_cloud";

/// Pixels differing between two frames of the same scene, on the same
/// threshold every sibling gate in this directory uses.
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

fn draw_at(id: i32, type_path: &str, at: glam::Vec3) -> EntityDraw {
    EntityDraw {
        id,
        type_path: std::sync::Arc::from(type_path),
        item: None,
        item_model: None,
        item_skin: None,
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_skin: Vec::new(),
        equipment_trim: Vec::new(),
        wool: None,
        count: 1,
        foil: false,
        item_dyed_color: None,
        item_potion_color: None,
        feet: at,
        yaw: 0.0,
        head_yaw: 0.0,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput::REST,
        block_state: None,
        item_frame_rotation: 0,
        painting: None,
        firework: None,
        projectile_owner: None,
        name_tag: None,
        hurt: false,
        death_time: 0.0,
        item_use: None,
        main_arm_left: false,
        creeper_swelling: 0.0,
        swim_amount: 0.0,
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
fn the_three_stranded_entity_types_each_reach_pixels() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; \
         run on a host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;

    // The spawner arm needs a real baked atlas — `RenderState::new(.., None)`
    // installs no `ModelRenderer` and `prepare_item_geometry` returns before
    // reaching the spawner branch at all, which reads exactly like a dead draw
    // path. The two sprite arms need the pack for a different reason: their
    // sheets are read straight out of `client.jar`.
    let resources = BlockResources::load(true);
    let atlas = resources.vanilla_atlas.clone().unwrap_or_else(|| {
        panic!(
            "GPU gate opted in but the vanilla pack did not load; set LODESTONE_ASSETS \
             to a pack root with client.jar + generated/reports/blocks.json. Banner: {:?}",
            resources.banner
        )
    });
    {
        // The precondition its siblings document: without this the spawner arm
        // could be measuring the absence of an *item* rather than of a *draw*.
        let item: lodestone_assets::ResourceLocation =
            SPAWNER_ITEM.parse().expect("valid item id");
        let models = atlas
            .models()
            .expect("the vanilla load must attach baked block models");
        assert!(
            models.item(&item).is_some(),
            "{SPAWNER_ITEM} must have baked geometry for this gate to mean anything"
        );
    }

    let mut target = HeadlessTarget::new(device, W, H, format);
    let state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));

    let camera = Camera {
        position: glam::Vec3::new(0.0, 1.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    };

    let mut shoot = |draws: &[EntityDraw]| -> (Vec<u8>, RenderStats) {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &camera, None, draws);
        (target.read_texels(device, queue), stats)
    };

    // ---- the reference frame -------------------------------------------
    //
    // One entity nothing draws, at the same place as every subject below, so
    // the scene (sky, fog, clear colour) is identical and each difference is
    // the subject alone.
    let subject_at = glam::Vec3::new(0.0, 1.0, 3.0);
    let (empty_px, empty_stats) = shoot(&[draw_at(90, DRAWS_NOTHING, subject_at)]);
    assert_eq!(
        empty_stats.entity_sprites_drawn, 0,
        "the reference entity must not itself be a sprite, or every difference below \
         is measured against the wrong frame"
    );

    // ---- arm 1: the dragon fireball ------------------------------------
    let (fireball_px, fireball_stats) =
        shoot(&[draw_at(1, "dragon_fireball", subject_at)]);
    let fireball_coverage = changed_pixels(&fireball_px, &empty_px);

    // ---- arm 2: the fishing bobber, with and without its line ----------
    //
    // The owner is an entity that draws nothing, so the *only* thing it
    // contributes to the frame is the line's far endpoint. Offset sideways and
    // down so the line crosses real screen area rather than collapsing onto
    // the bobber.
    let owner_id = 7;
    let owner = draw_at(owner_id, DRAWS_NOTHING, glam::Vec3::new(1.6, 0.2, 3.0));
    let bobber_no_line = draw_at(2, "fishing_bobber", subject_at);
    let mut bobber_with_line = bobber_no_line.clone();
    bobber_with_line.projectile_owner = Some(owner_id);

    let (bobber_px, bobber_stats) = shoot(&[bobber_no_line.clone(), owner.clone()]);
    let (line_px, line_stats) = shoot(&[bobber_with_line, owner]);
    let bobber_coverage = changed_pixels(&bobber_px, &empty_px);
    let line_only = changed_pixels(&line_px, &bobber_px);

    // ---- arm 3: the ominous item spawner, at three ages -----------------
    let spawner = |id: i32, age: f32| {
        let mut d = draw_at(id, "ominous_item_spawner", subject_at);
        d.item = Some(SPAWNER_ITEM.parse().expect("valid item id"));
        d.anim = AnimInput {
            age_ticks: age,
            ..AnimInput::REST
        };
        d
    };
    // Age 0 is exactly the grow-in ramp's zero, so nothing should be meshed at
    // all. Age 25 is half-grown; age 200 is long past the 50-tick ramp.
    let (fresh_px, fresh_stats) = shoot(&[spawner(3, 0.0)]);
    let (half_px, half_stats) = shoot(&[spawner(4, 25.0)]);
    let (grown_px, grown_stats) = shoot(&[spawner(5, 200.0)]);
    let fresh_coverage = changed_pixels(&fresh_px, &empty_px);
    let half_coverage = changed_pixels(&half_px, &empty_px);
    let grown_coverage = changed_pixels(&grown_px, &empty_px);

    eprintln!("=== stranded entity sprite pixel gate ===");
    eprintln!(
        "dragon_fireball       coverage = {fireball_coverage} px, entity_sprites_drawn = {}",
        fireball_stats.entity_sprites_drawn
    );
    eprintln!(
        "fishing_bobber        coverage = {bobber_coverage} px, entity_sprites_drawn = {}, \
         segments = {}",
        bobber_stats.entity_sprites_drawn, bobber_stats.fishing_line_segments
    );
    eprintln!(
        "fishing line only     = {line_only} px, segments = {}",
        line_stats.fishing_line_segments
    );
    eprintln!(
        "ominous spawner age 0   coverage = {fresh_coverage} px, copies = {}",
        fresh_stats.ominous_spawner_items_drawn
    );
    eprintln!(
        "ominous spawner age 25  coverage = {half_coverage} px, copies = {}",
        half_stats.ominous_spawner_items_drawn
    );
    eprintln!(
        "ominous spawner age 200 coverage = {grown_coverage} px, copies = {}",
        grown_stats.ominous_spawner_items_drawn
    );

    // ---- arm 1's assertions ---------------------------------------------
    //
    // The counter and the pixels together: the counter alone is incremented a
    // layer above the draw call, and a coverage number alone cannot say what
    // drew.
    assert_eq!(
        fireball_stats.entity_sprites_drawn, 1,
        "one dragon fireball is in view; entity_sprites_drawn={} means the sprite table \
         lookup never fired, or the pack carries no dragon_fireball.png",
        fireball_stats.entity_sprites_drawn
    );
    assert!(
        fireball_coverage > 400,
        "a dragon fireball drawn at 2x scale three blocks from the camera should cover a \
         real number of pixels; got {fireball_coverage}. Zero is the state this gate was \
         written for."
    );

    // ---- arm 2's assertions ---------------------------------------------
    assert_eq!(
        bobber_stats.entity_sprites_drawn, 1,
        "one fishing bobber is in view"
    );
    assert!(
        bobber_coverage > 20,
        "a fishing bobber drawn at 0.5x scale three blocks from the camera should cover a \
         real number of pixels; got {bobber_coverage}"
    );
    // No owner, no line — and this is the control that makes the next
    // assertion mean something, because a line that drew unconditionally would
    // put the same pixels in both frames and the difference would be zero for
    // the *wrong* reason.
    assert_eq!(
        bobber_stats.fishing_line_segments, 0,
        "a bobber whose spawn packet carried no owner has nowhere to anchor a line; \
         got {} segments",
        bobber_stats.fishing_line_segments
    );
    assert_eq!(
        line_stats.fishing_line_segments,
        lodestone_render::entity_sprite::FISHING_LINE_STEPS,
        "vanilla's `int steps = 16` — a different count means the curve is being \
         sampled somewhere other than `fishing_line_points`"
    );
    assert!(
        line_only > 100,
        "the line spans about 1.9 blocks of screen at 2.5 logical pixels wide and must \
         cover real area; only {line_only} px differ between a bobber with an owner and \
         one without. Zero means the segments reached the counter and not the frame — \
         which is exactly the split this gate keeps separate from the billboard's."
    );

    // ---- arm 3's assertions ---------------------------------------------
    //
    // Age 0: the ramp is exactly zero, so nothing is meshed. Not an
    // optimisation — a zero-scale matrix collapses every vertex onto one point,
    // and a build that skipped the guard would draw a degenerate sliver.
    assert_eq!(
        fresh_stats.ominous_spawner_items_drawn, 0,
        "at age 0 the grow-in scale is exactly 0 and nothing should be meshed; got {} copies",
        fresh_stats.ominous_spawner_items_drawn
    );
    assert_eq!(
        fresh_coverage, 0,
        "and nothing should reach the frame either; got {fresh_coverage} px"
    );
    assert!(
        grown_stats.ominous_spawner_items_drawn >= 1,
        "a grown spawner meshes one to five copies of its stack; got {}",
        grown_stats.ominous_spawner_items_drawn
    );
    assert!(
        grown_coverage > 200,
        "a full-size diamond block three blocks from the camera should cover a real \
         number of pixels; got {grown_coverage}"
    );
    // The ramp, predicted rather than observed. A uniform 0.5x scale halves
    // both silhouette axes, so the covered area should be near a quarter — call
    // it 0.15..0.45 of the grown coverage, which brackets the real projection's
    // perspective term without admitting either wrong hypothesis. Those two are
    // computed here so the assertion has to land on one:
    //
    //   * no ramp at all  -> half_coverage == grown_coverage, ratio 1.0
    //   * ramp inverted   -> half is *larger* than grown, ratio > 1.0
    #[expect(clippy::cast_precision_loss, reason = "pixel counts, far below 2^24")]
    let ratio = half_coverage as f32 / grown_coverage.max(1) as f32;
    eprintln!("ominous spawner half/grown area ratio = {ratio:.3} (quarter-area predicted)");
    assert!(
        (0.15..0.45).contains(&ratio),
        "a half-grown spawner item is a uniform 0.5x scale, so its silhouette should \
         cover about a quarter of the grown one's area; measured {ratio:.3} \
         ({half_coverage} / {grown_coverage}). A ratio near 1.0 means `ageInTicks` never \
         reaches the scale at all."
    );
}
