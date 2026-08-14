//! Pixel gate: a **remote skeleton** must visibly draw its bow once the server
//! reports it aggressive, and an otherwise-identical skeleton that never gets the
//! flag must not move a pixel.
//!
//! # Why this cannot be a unit test, and why that fix's gate could not catch it
//!
//! That fix landed the whole bow-draw *pose* — `Skeleton::pose_arms_for_item`, the
//! `ArmPose` vocabulary, `AnimInput::arm_pose` — and proved it reaches pixels with
//! `lodestone-render`'s `bow_draw_pose_pixels.rs`. That gate sets `arm_pose`
//! **directly on `AnimInput`** and renders. It is a good gate and it is
//! structurally blind to this bug: it starts *downstream* of the decision about
//! which mobs get the pose.
//!
//! The decision was wrong for every mob. That fix selected the pose from
//! `LivingEntity`'s **using-item** bit, which is the mechanism a *player* uses.
//! Vanilla's `AbstractSkeletonRenderer.getArmPose` reads `Mob.isAggressive()`
//! instead (`AbstractSkeletonRenderer.java:38`), and a skeleton's ranged attack
//! goal calls `performRangedAttack` without ever entering the item-use state — so
//! the using-item bit is `false` for the entire life of every skeleton that has
//! ever shot at anyone. Correct pose, correct plumbing, zero mobs.
//!
//! So this gate starts at a **`ClientEvent`** and ends at **texels**: the mob-flags
//! metadata byte goes into `IngestQueue`, through the production `IngestPlugin` +
//! `EntityInterpPlugin` pair, through the real `extract_entity_draws`, into
//! `RenderState::render` — `app.rs`'s own frame call. Nothing is hand-assembled.
//!
//! # What is measured
//!
//! Locations, never a fraction (`CLAUDE.md`: a percentage cannot separate a
//! uniform-but-wrong frame from a localised blob, and every failure here prints a
//! box):
//!
//! * the **silhouette width**, which must grow, because two arms rotating from
//!   hanging-down to forward-horizontal extend a *broadside* profile;
//! * the **bounding box of the changed pixels**, which must lie inside the mob's
//!   own rect and *begin* in its upper half — at the shoulders, not the feet;
//! * the **sole row**, which must not move, because posing arms is not a
//!   translation.
//!
//! The mob is broadside (`BODY_YAW = 90`) deliberately. Face-on, arms rotating
//! forward move almost entirely into the depth buffer and a *working* pose reads
//! as a dead one.
//!
//! ## One bound that is deliberately not restated from that fix
//!
//! That fix's gate first asserted "nothing below the waist differs" and that assertion
//! **failed on a working pose**: an arm is 12 texels long and hangs *downward*, so
//! rotating it forward vacates every row it occupied, a full arm's length below
//! the shoulder. The premise was false before the feature existed. Both vertical
//! bounds here are derived from the measured silhouette rather than from a
//! constant, for exactly that reason.
//!
//! # Three controls, and what each one would catch
//!
//! 1. **The not-aggressive skeleton** (id 2) — identical spawn, identical bow,
//!    identical elapsed ticks, identical camera; the only difference is that no
//!    mob-flags event ever names it. Its `arm_pose` must stay `Empty` and its
//!    frames must be byte-identical. Without this, "the frames differ" is also
//!    satisfied by a non-deterministic pipeline or by anything else that ticked.
//! 2. **The aggressive zombie** (id 3) — same bow, same aggressive byte, and its
//!    `arm_pose` must stay `Empty`, because `AbstractZombieRenderer` has no such
//!    override and vanilla shows a bow-holding zombie the ordinary undead arms.
//!    This is the *specificity* control: a gate that fired on the zombie too would
//!    be measuring "aggressive reached `AnimInput`", not "the skeleton override
//!    ran".
//! 3. **...and the same zombie must still move**, because `aggressive` is *also*
//!    `animateZombieArms`' arm-drop parameter (`-PI/1.5` aggressive vs `-PI/2.25`
//!    not — `AnimationUtils.java:74`), which was a second island: `AnimInput`
//!    carried the field, `Skeleton::animate_zombie_arms` consumed it, and every
//!    call site in the shell passed a hardcoded `false`. This half is what proves
//!    control 1's byte-identity is caused by *pose selection* rather than by the
//!    flag never arriving at all.
//!
//! # The premise: what else paints in this rect
//!
//! `CLAUDE.md`'s canonical false control asserted a frame "clears uniformly" and
//! failed at 3.5% on the **first-person bare arm**, which the hand pass draws
//! whenever `third_person_body_drawn` is false. This gate calls
//! `RenderState::render` with `None` for the player view and no held item, and
//! reads back a frame containing sky plus one mob. Each capture additionally
//! asserts `RenderStats::entities_drawn == 1`, so a frame that lost the mob fails
//! loudly instead of reading as "the pose changed nothing".
//!
//! No vanilla `client.jar` is needed: rigs come from `EntityModelSet::load()`'s
//! baked-in corpus (`RenderState::new(.., None)`), like `armour_pixels.rs`. The
//! only `#[ignore]` reason is the GPU adapter, and once opted in a missing adapter
//! is a **failure**, never a skip.
//!
//! ```text
//! cargo test -p lodestone-shell --test aggressive_bow_pose_pixels -- --ignored --nocapture
//! ```

use bevy_ecs::world::World;
use lodestone::entities::{EntityDraw, EntityInterpPlugin, extracted_entity_draws, fold_entities};
use lodestone::gpu::RenderState;
use lodestone_ecs::app::App;
use lodestone_ecs::ingest::{IngestPlugin, IngestQueue};
use lodestone_ecs::{Extract, GameTick, NetIngest};
use lodestone_model::event::EquipmentSlot;
use lodestone_model::{
    ClientEvent, EntityEquipment, EntityMetadataUpdate, ItemStack, Rotation, Vec3 as ModelVec3,
};
use lodestone_render::{ArmPose, Camera, GpuContext, HeadlessTarget, RenderTarget};

const W: u32 = 320;
const H: u32 = 240;

/// Broadside. See the module docs: face-on, a correct pose is nearly invisible in
/// silhouette, so this constant is load-bearing rather than cosmetic.
const BODY_YAW: f32 = 90.0;

/// `Mob.DATA_MOB_FLAGS_ID`'s aggressive bit (`Mob.java:1324`, `val | 4`).
const AGGRESSIVE_BIT: u8 = 0x04;

/// Minimum extra silhouette width the draw must add, in pixels.
///
/// Sized the same way that fix's sibling gate sizes its own: two arms swinging from
/// vertical to forward-horizontal each project about an arm's length (12 texels ≈
/// 0.75 blocks), which at this camera is tens of pixels. Six is far above
/// rasterisation jitter and far below the real effect — it separates "the pose
/// ran" from "it did not" and deliberately does not pin a projection.
const MIN_WIDTH_GAIN_PX: u32 = 6;

/// How far the sole row may drift. Posing arms is not a translation, so the right
/// answer is zero; one pixel covers edge rasterisation on a foot's flat bottom.
const MAX_SOLE_DRIFT_PX: u32 = 1;

fn camera() -> Camera {
    Camera {
        position: glam::Vec3::new(0.0, 1.0, 0.0),
        yaw: 0.0,
        pitch: 0.0,
        fov_y_degrees: 60.0,
        aspect: W as f32 / H as f32,
        near: 0.05,
        far: Camera::far_for_render_distance(8, 0),
    }
}

/// Three tracked mobs at the same spot, each **holding a bow in the main hand**.
///
/// The bow is the world-species guard. `arm_pose_for` reads
/// `main_hand_holds_bow`, so a fixture whose bow is in `OffHand`, or in no slot,
/// exercises none of the code under test and fails looking exactly like broken
/// wiring. It is asserted below rather than assumed.
fn world_with_bow_carrying_mobs(feet: glam::Vec3) -> World {
    let mut app = App::new();
    app.add_plugins((IngestPlugin, EntityInterpPlugin));
    let mut world = std::mem::take(app.world_mut());

    // ids 1 and 2 are skeletons (subject, not-aggressive control); id 3 is a
    // zombie (the specificity control).
    for (id, kind) in [
        (1, "minecraft:skeleton"),
        (2, "minecraft:skeleton"),
        (3, "minecraft:zombie"),
    ] {
        world
            .resource_mut::<IngestQueue>()
            .push(ClientEvent::EntitySpawned {
                entity_id: id,
                uuid: None,
                entity_type: kind.parse().expect("valid entity type key"),
                pos: ModelVec3::new(f64::from(feet.x), f64::from(feet.y), f64::from(feet.z)),
                rotation: Rotation::new(BODY_YAW, 0.0),
                velocity: None,
            });
        // The bow, through the real `SET_EQUIPMENT` ingest path (that fix:
        // there is no `EntitySnapshot` any more to hand this to directly —
        // `resolve_entity_facts` reads the `Equipment` component ingest wrote,
        // so the fixture has to write it the same way ingest does).
        world
            .resource_mut::<IngestQueue>()
            .push(ClientEvent::EntityEquipmentUpdated {
                entity_id: id,
                equipment: vec![EntityEquipment {
                    slot: EquipmentSlot::MainHand,
                    item: Some(ItemStack::new(
                        "minecraft:bow".parse().expect("valid item key"),
                        1,
                    )),
                }],
            });
        world.run_schedule(NetIngest);
    }

    // The fold now reads `EntityKind`/`Position`/`Rotation`/
    // `HeadYaw`/`Equipment` directly off the ingest entities just spawned
    // above, rather than a hand-built `EntitySnapshot` — same as
    // `Sim::fold_entities` does live.
    fold_entities(&mut world);
    world
}

/// The mob-flags metadata event, as the adapter emits it for a `Mob`.
fn aggressive_event(entity_id: i32) -> ClientEvent {
    ClientEvent::EntityMetadataUpdated {
        entity_id,
        metadata: EntityMetadataUpdate {
            mob_flags: Some(AGGRESSIVE_BIT),
            ..EntityMetadataUpdate::default()
        },
    }
}

fn draw_for(world: &World, id: i32) -> EntityDraw {
    extracted_entity_draws(world)
        .into_iter()
        .find(|d| d.id == id)
        .unwrap_or_else(|| panic!("entity {id} not among the extracted draws"))
}

/// An inclusive pixel box. Printed on every failure so a reader learns *where*.
#[derive(Debug, Clone, Copy, PartialEq)]
struct Box {
    top: u32,
    bottom: u32,
    left: u32,
    right: u32,
    area: u32,
}

impl Box {
    fn width(self) -> u32 {
        self.right + 1 - self.left
    }

    fn height(self) -> u32 {
        self.bottom + 1 - self.top
    }
}

impl std::fmt::Display for Box {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "rows {}..={} cols {}..={} ({}x{}, {} px)",
            self.top,
            self.bottom,
            self.left,
            self.right,
            self.width(),
            self.height(),
            self.area
        )
    }
}

fn box_of(mut hit: impl FnMut(usize) -> bool) -> Option<Box> {
    let (mut top, mut bottom) = (H, 0u32);
    let (mut left, mut right) = (W, 0u32);
    let mut area = 0u32;
    for y in 0..H {
        for x in 0..W {
            let i = ((y * W + x) * 4) as usize;
            if hit(i) {
                top = top.min(y);
                bottom = bottom.max(y);
                left = left.min(x);
                right = right.max(x);
                area += 1;
            }
        }
    }
    (area > 0).then_some(Box {
        top,
        bottom,
        left,
        right,
        area,
    })
}

/// The mob's silhouette: every pixel that differs from an **entity-free frame**
/// rendered through the identical path.
///
/// # This started as "every pixel that is not the clear colour", and that was wrong
///
/// The first form read the frame's top-left corner as "the sky" and called
/// everything unlike it silhouette. It reported the subject as `rows 65..=239 cols
/// 146..=319` — the full lower-right quadrant, clipped against two borders — and
/// tripped `assert_unclipped` on a *working* pose.
///
/// The cause is `CLAUDE.md`'s "ask what else already paints here": the sky is a
/// **gradient** (`docs/sky-and-air-bubbles.md`), so the corner pixel is not the
/// colour of the rest of the sky and most of the frame is legitimately unlike it.
/// The premise was false before this gate existed, and it failed in the
/// safe-looking direction — an assertion firing, on correct rendering.
///
/// Differencing against a real entity-free frame needs no assumption about what
/// the background is, which is why it is the reference rather than any constant.
fn silhouette(frame: &[u8], background: &[u8]) -> Box {
    changed(frame, background).expect("no mob silhouette at all — the frame matches empty sky")
}

/// Bounding box of the pixels that differ between two frames, or `None` when they
/// are identical within rounding noise.
fn changed(a: &[u8], b: &[u8]) -> Option<Box> {
    box_of(|i| {
        let d = (i32::from(a[i]) - i32::from(b[i])).abs()
            + (i32::from(a[i + 1]) - i32::from(b[i + 1])).abs()
            + (i32::from(a[i + 2]) - i32::from(b[i + 2])).abs();
        d > 20
    })
}

fn assert_unclipped(label: &str, b: Box) {
    assert!(
        b.top > 0 && b.bottom < H - 1 && b.left > 0 && b.right < W - 1,
        "{label}: silhouette is {b} in a {W}x{H} frame — clipped, so its width measures the \
         viewport rather than the arm pose"
    );
}

#[test]
#[ignore = "requires a GPU adapter; run explicitly to prove the aggressive bow draw reaches pixels"]
fn an_aggressive_skeleton_draws_its_bow_and_a_calm_one_does_not() {
    let ctx = GpuContext::new_headless_blocking().expect(
        "headless GPU gate opted in via --ignored but no wgpu adapter is available; run on a \
         host with a GPU — do NOT treat a skip as a pass",
    );
    let device = ctx.device();
    let queue = ctx.queue();
    let format = wgpu::TextureFormat::Rgba8Unorm;
    let mut target = HeadlessTarget::new(device, W, H, format);
    let state = RenderState::new(device, queue, format, W, H, None);
    let cam = camera();

    let feet = glam::Vec3::new(0.0, 0.0, 4.0);
    let mut world = world_with_bow_carrying_mobs(feet);
    world.run_schedule(Extract);

    let subject_rest = draw_for(&world, 1);
    let control_rest = draw_for(&world, 2);
    let zombie_rest = draw_for(&world, 3);

    // The world-species premise, asserted rather than assumed: the mob really is
    // holding a bow, in the main hand, *in the equipment the draw reads*.
    for (label, draw) in [
        ("subject", &subject_rest),
        ("control", &control_rest),
        ("zombie", &zombie_rest),
    ] {
        assert!(
            draw.equipment
                .iter()
                .any(|(slot, item)| *slot == EquipmentSlot::MainHand && item.path() == "bow"),
            "{label}: the fixture's main hand does not hold a bow in EntityDraw::equipment — \
             this gate would then be measuring nothing, and would fail looking exactly like \
             broken wiring"
        );
    }
    for (label, draw) in [
        ("subject", &subject_rest),
        ("control", &control_rest),
        ("zombie", &zombie_rest),
    ] {
        assert_eq!(
            draw.anim.arm_pose,
            ArmPose::Empty,
            "{label}: before any mob-flags event a bow-carrying mob must be in the rest pose \
             — a mob that already holds the draw would make the measurement below vacuous"
        );
        assert!(
            !draw.anim.aggressive,
            "{label}: aggressive must start false, before any metadata arrives"
        );
    }

    // The one real event, twice: only ids 1 and 3 are ever reported aggressive.
    for id in [1, 3] {
        world.resource_mut::<IngestQueue>().push(aggressive_event(id));
    }
    world.run_schedule(NetIngest);
    // A couple of ticks, so the control is *not* separated from the subject by
    // "one side ticked and the other did not".
    for _ in 0..2 {
        world.run_schedule(GameTick);
    }
    world.run_schedule(Extract);

    let subject_angry = draw_for(&world, 1);
    let control_calm = draw_for(&world, 2);
    let zombie_angry = draw_for(&world, 3);

    eprintln!("=== AGGRESSIVE BOW POSE PIXEL GATE (broadside, {W}x{H}) ===");
    eprintln!(
        "subject : aggressive={} arm_pose={:?}",
        subject_angry.anim.aggressive, subject_angry.anim.arm_pose
    );
    eprintln!(
        "control : aggressive={} arm_pose={:?}",
        control_calm.anim.aggressive, control_calm.anim.arm_pose
    );
    eprintln!(
        "zombie  : aggressive={} arm_pose={:?}",
        zombie_angry.anim.aggressive, zombie_angry.anim.arm_pose
    );

    // --- The state assertions, ahead of any pixel ------------------------------
    assert!(
        subject_angry.anim.aggressive,
        "the mob-flags byte did not reach AnimInput at all. Check `ingest::handles_event` \
         claims EntityMetadataUpdated, that `apply_entity_metadata` folds `mob_flags` into \
         `MobState`, and that `extract_entity_draws` bridges it through `EntityIndex`"
    );
    assert_eq!(
        subject_angry.anim.arm_pose,
        ArmPose::BowAndArrow,
        "an aggressive skeleton holding a bow must select BOW_AND_ARROW \
         (AbstractSkeletonRenderer.java:38)"
    );
    assert!(
        !control_calm.anim.aggressive,
        "control 1: a skeleton no metadata event ever named became aggressive — the ingest \
         system is not filtering by entity id, so every measurement here is worthless"
    );
    assert_eq!(control_calm.anim.arm_pose, ArmPose::Empty);
    // Control 3's two halves. The flag *did* arrive...
    assert!(
        zombie_angry.anim.aggressive,
        "control 3: the zombie never became aggressive, so control 2's byte-identity below \
         could be explained by the flag not arriving rather than by pose selection"
    );
    // ...and the pose still did not, because the override is per-renderer.
    assert_eq!(
        zombie_angry.anim.arm_pose,
        ArmPose::Empty,
        "control 2 (specificity): an aggressive bow-holding *zombie* must NOT get \
         BOW_AND_ARROW — AbstractZombieRenderer has no such override. Firing here means the \
         rule is 'any aggressive mob draws', which vanilla never shows"
    );

    // --- Pixels ---------------------------------------------------------------
    let mut shoot_n = |draws: &[EntityDraw], expected: usize| -> Vec<u8> {
        let frame = target.acquire().expect("headless acquire");
        let stats = state.render(device, queue, frame.view(), &cam, None, draws);
        assert_eq!(
            stats.entities_drawn as usize, expected,
            "entities_drawn={} but {expected} was expected — the mob failed to reach the \
             entity pipeline, so this gate would measure the absence of an entity rather \
             than the absence of a pose",
            stats.entities_drawn
        );
        target.read_texels(device, queue)
    };
    // The silhouette reference: the identical camera and pass with no entity at
    // all. Everything the background paints (the sky gradient especially) is in
    // here, so no assumption about it is needed anywhere below.
    let background_px = shoot_n(&[], 0);
    let mut shoot = move |draw: &EntityDraw| -> Vec<u8> {
        shoot_n(std::slice::from_ref(draw), 1)
    };

    let subject_rest_px = shoot(&subject_rest);
    let subject_angry_px = shoot(&subject_angry);
    let control_rest_px = shoot(&control_rest);
    let control_calm_px = shoot(&control_calm);
    let zombie_rest_px = shoot(&zombie_rest);
    let zombie_angry_px = shoot(&zombie_angry);

    let rest_box = silhouette(&subject_rest_px, &background_px);
    let angry_box = silhouette(&subject_angry_px, &background_px);
    let zombie_rest_box = silhouette(&zombie_rest_px, &background_px);
    let zombie_angry_box = silhouette(&zombie_angry_px, &background_px);
    let moved = changed(&subject_rest_px, &subject_angry_px);
    let control_moved = changed(&control_rest_px, &control_calm_px);
    let zombie_moved = changed(&zombie_rest_px, &zombie_angry_px);

    eprintln!("rest silhouette : {rest_box}");
    eprintln!("bow  silhouette : {angry_box}");
    eprintln!("zombie rest sil : {zombie_rest_box}");
    eprintln!("zombie angry sil: {zombie_angry_box}");
    eprintln!(
        "changed by pose : {}",
        moved.map_or("NOTHING".to_string(), |b| b.to_string())
    );
    eprintln!(
        "calm control    : {}",
        control_moved.map_or("identical (correct)".to_string(), |b| b.to_string())
    );
    eprintln!(
        "zombie arm lift : {}",
        zombie_moved.map_or("NOTHING".to_string(), |b| b.to_string())
    );

    assert_unclipped("rest", rest_box);
    assert_unclipped("bow", angry_box);

    // (a) The pose reached pixels at all. This is the assertion that fails on the
    //     build this change replaced, where `arm_pose_for` had no aggressive input
    //     and no mob could ever select the draw.
    let moved = moved.unwrap_or_else(|| {
        panic!(
            "the aggressive bow pose changed ZERO pixels. rest {rest_box}, aggressive \
             {angry_box} — issue #379 exactly: correct pose, correct plumbing, no mob"
        )
    });

    // (b) ...inside the mob's own rect. The changed box may reach *past* the
    //     resting silhouette — that is the point, the arms swing out — so it is
    //     compared against the union of the two.
    let union_left = rest_box.left.min(angry_box.left);
    let union_right = rest_box.right.max(angry_box.right);
    let union_top = rest_box.top.min(angry_box.top);
    let union_bottom = rest_box.bottom.max(angry_box.bottom);
    assert!(
        moved.left >= union_left
            && moved.right <= union_right
            && moved.top >= union_top
            && moved.bottom <= union_bottom,
        "the changed box {moved} is not inside the mob's own rect \
         (rows {union_top}..={union_bottom} cols {union_left}..={union_right}) — something \
         other than this mob moved"
    );

    // (c) ...and it *begins* at the shoulders. Bounds derived from the measured
    //     silhouette, never from a restated waist constant — see the module docs
    //     on that fix's false premise. Only the box's TOP is bounded: an arm hangs
    //     downward, so rotating it forward legitimately vacates rows a full arm's
    //     length below the shoulder.
    let midline = union_top + (union_bottom - union_top) / 2;
    assert!(
        moved.top < midline,
        "the changed box {moved} starts at row {} — below the mob's own midline ({midline}) — \
         so whatever moved is not the arms",
        moved.top
    );

    // (d) The broadside profile widened, which is the pose's defining projection.
    assert!(
        angry_box.width() >= rest_box.width() + MIN_WIDTH_GAIN_PX,
        "the broadside silhouette grew from {} px to {} px, less than the {MIN_WIDTH_GAIN_PX} \
         px two forward-rotated arms must add (rest {rest_box}, aggressive {angry_box})",
        rest_box.width(),
        angry_box.width()
    );

    // (e) ...and the mob did not simply move. Posing arms is not a translation.
    assert!(
        rest_box.bottom.abs_diff(angry_box.bottom) <= MAX_SOLE_DRIFT_PX,
        "the sole row moved from {} to {} — the mob translated rather than posed",
        rest_box.bottom,
        angry_box.bottom
    );

    // --- Control 1: the calm skeleton must be byte-identical ------------------
    assert!(
        control_moved.is_none(),
        "control: a skeleton that never received the mob-flags byte moved {} — it went \
         through the identical spawn, bow, ticks, camera and render, so anything that moved \
         it also moved the subject and the measurement above is not attributable to the flag",
        control_moved.unwrap()
    );

    // --- Control 3: the zombie's arms must still lift -------------------------
    let zombie_moved = zombie_moved.unwrap_or_else(|| {
        panic!(
            "the aggressive zombie is byte-identical. `aggressive` feeds \
             `animate_zombie_arms`' arm drop (-PI/1.5 vs -PI/2.25), so this means the flag is \
             reaching `AnimInput` and being ignored — and it also means control 1's \
             byte-identity proves nothing about pose selection"
        )
    });
    assert!(
        zombie_moved.top < midline,
        "the zombie's change {zombie_moved} is below the midline ({midline}); the arm drop is \
         an upper-body rotation"
    );
    // The two effects must be distinguishable *in kind*, not merely both non-zero
    // — this is `CLAUDE.md`'s magnitude species, and the first form of this
    // assertion was an area ordering (`zombie_moved.area < moved.area`) whose
    // premise was simply false: the measured run had the zombie changing 829 px
    // against the skeleton's 564. Area is not the discriminator, because a zombie's
    // arms already point forward and rotating them *up* sweeps a wide high arc.
    //
    // The broadside **width gain** is, and it is directional: a bow draw rotates
    // arms from hanging-down to forward-horizontal, which extends the profile; an
    // aggressive arm lift rotates already-forward arms further up, which does not.
    let skeleton_gain = angry_box.width() as i64 - rest_box.width() as i64;
    let zombie_gain = zombie_angry_box.width() as i64 - zombie_rest_box.width() as i64;
    eprintln!("width gain      : skeleton {skeleton_gain:+} px, zombie {zombie_gain:+} px");
    assert!(
        zombie_gain < skeleton_gain,
        "the zombie's broadside profile grew {zombie_gain:+} px and the skeleton's \
         {skeleton_gain:+} px. The bow draw must be the one that widens the silhouette; if the \
         zombie widens as much, it is getting BOW_AND_ARROW too and the arm_pose assertion \
         above is not measuring what it claims"
    );
}
