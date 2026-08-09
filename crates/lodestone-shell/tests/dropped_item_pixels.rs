//! Pixel gate: a **dropped item entity** must reach the screen, bob, and spin.
//!
//! The reported defect is "blocks don't drop anything yet". The rendering half
//! of that had a precise cause: an item entity *is* tracked — `add_entity` gives
//! it type path `item`, `NetClient::entity_snapshots` lowers it, and
//! `EntityInterpolator` interpolates it like any mob — but
//! `EntityModelSet::resolve` has no `entity_models` corpus entry named `item`
//! and never will (an item entity is an item model, not a cuboid part rig), so
//! it was dropped on the floor of `prepare_entities` and drew nothing.
//!
//! # Why the frame counters cannot be the gate
//!
//! `drops=` in the debug overlay is `Sim::mesh_drops` — *chunk columns that
//! failed to mesh* — and `0` there is the **healthy** reading, not evidence of a
//! missing drop. `entities=` is `RenderStats::entities_drawn`, which counts only
//! what the instanced entity pipeline drew, and an item entity never appears
//! there however well this path works. So a counter reading cannot distinguish
//! "dropped items render" from "no dropped item was ever present" — the trap
//! that the same overlay set for the particle work. This gate therefore
//! **causes** the drop itself and reads pixels.
//!
//! # What is asserted
//!
//! One `EntityDraw` with `type_path: "item"` and `item: Some(minecraft:stone)`,
//! two blocks in front of the camera, rendered through the real
//! [`RenderState::render`] — the same call `app.rs` makes, with no extra
//! argument. Then:
//!
//! * **subject**: a substantial, localised cluster of non-sky pixels where the
//!   drop is, sized like a 0.25-scaled cube at that distance;
//! * **opposite corner**: exactly 0, so the count is a drawn object and not a
//!   full-screen tint;
//! * **control A — no item entity at all**: exactly 0 lit, the executed proof
//!   that this pass is what puts the pixels there;
//! * **control B — the same item entity with `item: None`** (today's live state,
//!   because nothing decodes an item entity's stack yet): exactly 0 lit, so the
//!   subject's pixels are attributable to the stack reaching the renderer and
//!   not merely to an entity being present;
//! * **bobbing, observed firing**: two ages half a bob period apart must move
//!   the drawn item vertically, and the *same* age twice must differ by exactly
//!   0 pixels. Both halves are needed: "differs" alone passes for a renderer
//!   that is simply non-deterministic.
//!
//! The vertical measurement is a centroid, which is exact here for a reason
//! worth stating: the spin is a rotation about **Y**, so it cannot change any
//! vertex's `y`. Anything that moves the centroid vertically between two frames
//! is the bob and only the bob.
//!
//! Fail-closed like its siblings: a missing GPU or a missing `client.jar` is a
//! failure, never a skip.
//!
//! # The second gate in this file: a thrown projectile
//!
//! `a_thrown_snowball_reaches_pixels_through_the_real_render_call` is the *island*
//! check for [`thrown-projectiles.md`](../../../docs/thrown-projectiles.md).
//! `lodestone-render`'s `thrown_and_held_item_pixels` proves the pose math and the
//! pipeline; it cannot prove that `RenderState::render` ever reaches
//! `merge_thrown_item`, which is exactly the failure this repo has shipped eleven
//! times. This one drives the same `render` call `app.rs` makes, with an
//! `EntityDraw` whose `type_path` is `"snowball"` and — deliberately — whose
//! `item` is `None`, because `extract_entity_draws` populates `EntityDraw::item`
//! **only** for `type_path == "item"` today. So it exercises the registration
//! table's default-item fallback, which is the path a live frame actually takes.
//!
//! ```text
//! cargo test -p lodestone-shell --test dropped_item_pixels -- --ignored --nocapture
//! ```

use lodestone::entities::{EntityDraw, ITEM_ENTITY_TYPE_PATH};
use lodestone::gpu::RenderState;
use lodestone::resources::BlockResources;
use lodestone_assets::ResourceLocation;
use lodestone_render::{
    AnimInput, BlockModels, Camera, GpuContext, HeadlessTarget, RenderTarget,
    entity::{ITEM_BOB_TICKS_PER_RADIAN, item_bob_offset},
};

const W: u32 = 320;
const H: u32 = 240;

/// The item under test: a full opaque cube whose faces are one sprite, so its
/// silhouette is the posed cube's and nothing else.
const ITEM: &str = "minecraft:stone";

/// The dropped entity's id. Fixed so the bob/spin phase is reproducible.
const DROP_ID: i32 = 4242;

/// Where the drop sits, two blocks in front of the camera.
const DROP_POS: glam::Vec3 = glam::Vec3::new(0.0, 0.0, 2.0);

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

fn drop_draw(item: Option<ResourceLocation>, age_ticks: f32) -> EntityDraw {
    EntityDraw {
        // A dropped item is not a living entity, so it never reddens — vanilla's
        // overlay is `LivingEntityRenderer`'s, and an item entity is drawn
        // through the model pipeline instead (issue #98).
        hurt: false,
        block_state: None,
        id: DROP_ID,
        type_path: ITEM_ENTITY_TYPE_PATH.to_owned(),
        item,
        feet: DROP_POS,
        yaw: 0.0,
        head_yaw: 0.0,
        pitch: 0.0,
        scale: 1.0,
        anim: AnimInput {
            age_ticks,
            ..AnimInput::REST
        },
        // A dropped item entity carries no equipment; this gate is about the
        // item's own ground pose, not a held-item layer.
        equipment: Vec::new(),
        equipment_dye: Vec::new(),
        equipment_trim: Vec::new(),
        // Not a sheep, and a single-item stack: `count` above 1 asks for
        // vanilla's 1-5 jittered copies, which would widen the silhouette this
        // gate measures. `1` keeps it a single-copy measurement.'
        wool: None,
        count: 1,
        foil: false,
        name_tag: None,
        // An item entity is not a living one; nothing can be using it.
        item_use: None,
        creeper_swelling: 0.0,
        // A dropped item entity carries no `EntityFlags` lookup in this
        // hand-built fixture (issue #434).
        on_fire: false,
        // Not a player, so no skin can apply.
        player_skin: None,
        // Not an experience orb, so the orb billboard pass never claims it.
        experience_orb_value: None,
    }
}

/// Pixels whose colour differs from `reference`'s at the same offset by more
/// than a rounding wobble, with their bounding box and vertical centroid.
struct Diff {
    count: usize,
    min_x: u32,
    max_x: u32,
    min_y: u32,
    max_y: u32,
    centroid_y: f32,
}

fn diff(subject: &[u8], reference: &[u8]) -> Diff {
    let mut count = 0usize;
    let (mut min_x, mut max_x, mut min_y, mut max_y) = (W, 0u32, H, 0u32);
    let mut sum_y = 0f64;
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
        sum_y += f64::from(y);
    }
    Diff {
        count,
        min_x,
        max_x,
        min_y,
        max_y,
        centroid_y: if count == 0 {
            f32::NAN
        } else {
            (sum_y / count as f64) as f32
        },
    }
}

/// Differing pixels inside the top-left quarter — the corner opposite a drop
/// drawn at screen centre.
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
fn a_dropped_item_reaches_pixels_and_bobs() {
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
    {
        let models: &BlockModels = atlas
            .models()
            .expect("the vanilla load must attach baked block models");
        assert!(
            models.item(&item).is_some(),
            "{ITEM} must have baked 3-D geometry; without it this gate would be \
             measuring the absence of an item rather than the absence of a draw"
        );
    }

    let mut target = HeadlessTarget::new(device, W, H, format);
    let state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
    let cam = camera();

    let mut shoot = |draws: &[EntityDraw]| -> (Vec<u8>, usize) {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, draws);
        (target.read_texels(device, queue), stats.item_drops_drawn)
    };

    // Control A: an empty world, nothing but sky. Every later count is measured
    // against this, so "lit" means "this pass drew here".
    let (empty, empty_drops) = shoot(&[]);

    // Control B: the item entity is present and tracked, but no stack has been
    // reported for it — which is what a live session looks like today, because
    // the 26.2 adapter rejects the ITEM_STACK metadata serializer outright.
    let (no_stack, no_stack_drops) = shoot(&[drop_draw(None, 0.0)]);

    // Subject: the same entity, now carrying stone.
    let bob_offset = item_bob_offset(DROP_ID);
    // Two ages half a bob period apart, so `sin` is at its two extremes and the
    // item is drawn as high and as low as it ever goes.
    let half_period = std::f32::consts::PI * ITEM_BOB_TICKS_PER_RADIAN;
    // Wrapped into the first positive period: an entity's age is never negative,
    // and a gate that only holds for ages a real drop cannot have is not a gate.
    let age_high = (std::f32::consts::FRAC_PI_2 - bob_offset)
        .rem_euclid(std::f32::consts::TAU)
        * ITEM_BOB_TICKS_PER_RADIAN;
    let age_low = age_high + half_period;
    assert!(
        age_high >= 0.0,
        "the sampled ages must be ones a real drop can reach"
    );

    let (high, high_drops) = shoot(&[drop_draw(Some(item.clone()), age_high)]);
    let (low, low_drops) = shoot(&[drop_draw(Some(item.clone()), age_low)]);
    // The determinism control: the very same phase, rendered again.
    let (high_again, _) = shoot(&[drop_draw(Some(item.clone()), age_high)]);

    let d_high = diff(&high, &empty);
    let d_low = diff(&low, &empty);
    let d_no_stack = diff(&no_stack, &empty);
    let corner = diff_in_far_corner(&high, &empty);
    let d_repeat = diff(&high_again, &high);

    eprintln!("=== dropped-item pixel gate ===");
    eprintln!("item                        = {ITEM}");
    eprintln!("drop position               = {DROP_POS:?}");
    eprintln!("bob phase (id {DROP_ID})       = {bob_offset:.4} rad");
    eprintln!("age at bob max / min        = {age_high:.2} / {age_low:.2} ticks");
    eprintln!("stats.item_drops_drawn      = empty {empty_drops}, no-stack {no_stack_drops}, high {high_drops}, low {low_drops}");
    eprintln!(
        "lit px, stone drop (high)   = {} (bbox x {}..{}, y {}..{}, centroid y {:.2})",
        d_high.count, d_high.min_x, d_high.max_x, d_high.min_y, d_high.max_y, d_high.centroid_y
    );
    eprintln!(
        "lit px, stone drop (low)    = {} (centroid y {:.2})",
        d_low.count, d_low.centroid_y
    );
    eprintln!("lit px, item entity w/o stack = {}", d_no_stack.count);
    eprintln!("lit px, far corner          = {corner}");
    eprintln!("lit px, same phase twice    = {}", d_repeat.count);

    // --- the load-bearing positive -------------------------------------
    //
    // A 0.25-scaled cube 2 blocks from a 60° vertical-FOV camera over 240 px
    // subtends 0.25 / (2 * 2 * tan(30°)) * 240 = 26 px per edge, so its
    // silhouette is between 26^2 = 676 px (face-on) and 26 * 26 * sqrt(2)
    // ~ 956 px (corner-on) plus the visible top face. The band below is wide
    // enough to admit any spin angle and tight enough that a drop rendered at
    // the *gui* scale (0.625, i.e. 2.5x linear and ~6x the area) fails it.
    assert!(
        (400..=2600).contains(&d_high.count),
        "a dropped stone must cover roughly 700-1500 px at this distance; got {}. \
         Far below means the pass drew nothing or only a sliver; far above means \
         the item is posed at the wrong scale (the gui transform's 0.625 rather \
         than ground's 0.25 is the likely culprit)",
        d_high.count
    );
    assert_eq!(
        high_drops, 1,
        "exactly one drop should have been meshed, stats said {high_drops}"
    );

    // Localisation: the drop is a small object at screen centre.
    assert!(
        d_high.min_x > W / 4 && d_high.max_x < 3 * W / 4,
        "the drop must be a localised object near screen centre, not a smear: \
         x {}..{} of {W}",
        d_high.min_x,
        d_high.max_x
    );
    assert_eq!(
        corner, 0,
        "the corner opposite the drop must be untouched; {corner} differing px \
         there means the count above is measuring a full-screen change"
    );

    // --- the executed negative controls --------------------------------
    assert_eq!(
        empty_drops, 0,
        "a frame with no entities cannot have drawn a drop"
    );
    assert_eq!(
        d_no_stack.count, 0,
        "an item entity whose stack has not been reported must draw nothing \
         (vanilla returns early on an empty stack); {} px says a placeholder is \
         being substituted",
        d_no_stack.count
    );
    assert_eq!(
        no_stack_drops, 0,
        "the drop counter must not count an item entity with no model to draw"
    );

    // --- bobbing, both halves ------------------------------------------
    assert_eq!(
        d_repeat.count, 0,
        "the same bob phase rendered twice must be pixel-identical; {} differing \
         px means the frame is non-deterministic and the 'phases differ' check \
         below proves nothing",
        d_repeat.count
    );
    // The bob spans 0.0..=0.2 blocks, which at 104 px per block here is ~21 px
    // of travel between the two extremes. Screen y grows downward, so the
    // higher-bob frame must have the *smaller* centroid.
    let travel = d_low.centroid_y - d_high.centroid_y;
    assert!(
        travel > 8.0,
        "half a bob period must visibly raise the item: centroid moved {travel:.2} px \
         (expected ~20, upward). A value near 0 means the age never reaches the \
         bob, and a negative one means the sine is inverted"
    );
}

/// The **island check** for thrown projectiles: `RenderState::render` — the same
/// call `app.rs` makes — must reach `merge_thrown_item` and put a snowball on
/// screen, with no argument the shell does not already pass.
///
/// # Why `item: None` is the interesting case, not a weakened one
///
/// `extract_entity_draws` fills `EntityDraw::item` only when
/// `type_path == ITEM_ENTITY_TYPE_PATH`, so **a live snowball arrives with
/// `item: None`** even though `ItemStacks` holds its stack (`fold_snapshots` inserts
/// for any entity type). The renderer therefore falls back to
/// `thrown_item_for(type_path).item`, and that fallback is the *only* path a real
/// frame takes today. A gate that helpfully supplied `Some(minecraft:snowball)`
/// would test a branch nothing reaches and pass while the live client drew nothing.
///
/// # Controls, both executed
///
/// * **the same snowball behind the camera.** `projectiles_drawn` must be `0` and the
///   frame identical to the empty one — the proof that the subject's pixels come from
///   *this* entity at *that* position, and that the frustum cull is live rather than
///   dead code. This control is deliberately independent of the entity-model corpus.
/// * **a `pig` at the same position** must produce `projectiles_drawn == 0`: a type
///   absent from `thrown_item_for` must never be billboarded. **No pixel assertion on
///   this one**, and the reason is a mistake worth recording: the first version of
///   this gate asserted the pig frame was identical to the empty one and it failed at
///   **10254** differing pixels, because `pig` *does* have a corpus model and the
///   entity pass drew it correctly. "An unregistered type draws nothing" is false in
///   general; the counter is what discriminates, not the pixels.
#[test]
#[ignore = "requires a GPU adapter and the vanilla client.jar"]
fn a_thrown_snowball_reaches_pixels_through_the_real_render_call() {
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
    // The default item of the `snowball` entity must have baked geometry, or this
    // gate would be measuring the absence of an item rather than the absence of a
    // draw. `minecraft:snowball` is a flat sprite, so this is also a live check that
    // the extruded-slab stream reaches `BlockModels::item`.
    let thrown = lodestone_render::entity::thrown_item_for("snowball")
        .expect("snowball is a ThrownItemRenderer type");
    let default_item: ResourceLocation = thrown.item.parse().expect("valid item id");
    {
        let models: &BlockModels = atlas
            .models()
            .expect("the vanilla load must attach baked block models");
        assert!(
            models.item(&default_item).is_some(),
            "{} must have baked geometry (an extruded sprite slab); without it this \
             gate measures the absence of an item, not the absence of a draw",
            thrown.item
        );
    }

    let mut target = HeadlessTarget::new(device, W, H, format);
    let state = RenderState::new(device, queue, format, W, H, Some(atlas.as_ref()));
    let cam = camera();

    let mut shoot = |draws: &[EntityDraw], cam: &Camera| -> (Vec<u8>, usize) {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), cam, None, draws);
        (target.read_texels(device, queue), stats.projectiles_drawn)
    };

    let projectile = |type_path: &str| EntityDraw {
        hurt: false,
        block_state: None,
        id: DROP_ID + 1,
        type_path: type_path.to_owned(),
        // Exactly what `extract_entity_draws` produces for a non-`item` entity.
        item: None,
        feet: DROP_POS,
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
        name_tag: None,
        item_use: None,
        creeper_swelling: 0.0,
        on_fire: false,
        // Not a player, so no skin can apply.
        player_skin: None,
        // Not an experience orb, so the orb billboard pass never claims it.
        experience_orb_value: None,
    };
    // The same camera, turned to put the projectile squarely behind it.
    let away = Camera {
        yaw: 180.0,
        ..camera()
    };

    let (empty, empty_count) = shoot(&[], &cam);
    let (behind, behind_count) = shoot(&[projectile("snowball")], &away);
    let (_, unregistered_count) = shoot(&[projectile("pig")], &cam);
    let (subject, subject_count) = shoot(&[projectile("snowball")], &cam);

    let d_subject = diff(&subject, &empty);
    let corner = diff_in_far_corner(&subject, &empty);
    // The behind-the-camera frame is compared against its *own* baseline, since a
    // 180° turn changes the sky gradient and not just the projectile.
    let (empty_away, _) = shoot(&[], &away);
    let d_behind = diff(&behind, &empty_away);

    eprintln!(
        "projectiles_drawn = empty {empty_count}, behind {behind_count}, pig \
         {unregistered_count}, snowball {subject_count}"
    );
    eprintln!(
        "lit px            = behind {}, snowball {}",
        d_behind.count, d_subject.count
    );
    eprintln!(
        "snowball box      = x {}..{}, y {}..{}",
        d_subject.min_x, d_subject.max_x, d_subject.min_y, d_subject.max_y
    );
    eprintln!("far-corner px     = {corner}");

    assert_eq!(
        empty_count, 0,
        "a frame with no entities cannot have drawn a projectile"
    );
    assert_eq!(
        unregistered_count, 0,
        "`pig` is not a ThrownItemRenderer type and must not be billboarded"
    );
    assert_eq!(
        behind_count, 0,
        "a projectile behind the camera must be frustum-culled, not meshed"
    );
    assert_eq!(
        d_behind.count, 0,
        "the same snowball behind the camera changed {} px, so the subject's pixels \
         are not attributable to the projectile being in front of the camera",
        d_behind.count
    );
    assert_eq!(
        subject_count, 1,
        "exactly one projectile should have been meshed. Zero means \
         `prepare_item_geometry` never called `merge_thrown_item` — the island shape: \
         the renderer is complete and nothing invokes it"
    );
    assert!(
        d_subject.count > 100,
        "a snowball two blocks away should cover a real run of pixels; only {} differ \
         from the empty frame. A count of 1 with zero pixels is the mesh being built \
         and then clipped or culled away",
        d_subject.count
    );
    assert_eq!(
        corner, 0,
        "the corner opposite the projectile must be untouched; {corner} differing px \
         there means the count above is measuring a full-screen change"
    );
}
