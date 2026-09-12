//! Frame-rate-independent entity pose easing and animation input assembly.

use super::*;

pub(super) fn alpha(clock: &InterpClock) -> f32 {
    (clock.t / clock.window).clamp(0.0, 1.0)
}

/// The currently-drawn position: [`InterpFrom`] eased toward [`InterpTo`].
pub(super) fn render_feet(from: &InterpFrom, to: &InterpTo, clock: &InterpClock) -> Vec3 {
    from.feet.lerp(to.feet, alpha(clock))
}

/// Samples a locally controlled vehicle between its two fixed-tick endpoints.
///
/// `alpha` is the shared driver's accumulator residual. This function is pure:
/// calling it once or a hundred times between ticks cannot advance physics.
pub(super) fn sample_vehicle_pose(
    previous: VehicleRenderPose,
    current: VehicleRenderPose,
    alpha: f32,
) -> VehicleRenderPose {
    let alpha = alpha.clamp(0.0, 1.0);
    VehicleRenderPose {
        position: previous.position.add(
            current
                .position
                .subtract(previous.position)
                .scale(f64::from(alpha)),
        ),
        yaw: lerp_angle(previous.yaw, current.yaw, alpha).rem_euclid(360.0),
        pitch: previous.pitch + (current.pitch - previous.pitch) * alpha,
    }
}

/// Returns this frame's fixed-tick sample when `id` is the vehicle the local
/// client controls. Remote/uncontrolled entities deliberately return `None` and
/// continue through the generic network interpolation track.
pub(super) fn controlled_vehicle_render_pose(
    controlled: Option<&ControlledVehicle>,
    id: i32,
    alpha: f32,
) -> Option<VehicleRenderPose> {
    let held = controlled?.0.as_ref()?;
    (held.server_id == id).then(|| sample_vehicle_pose(held.previous, held.current_pose(), alpha))
}

/// The local player's seat position **this frame**, derived from the
/// vehicle's own per-frame draw pose. For a controlled vehicle this is
/// [`sample_vehicle_pose`] over fixed-tick history; for every other vehicle it
/// is [`render_feet`]/[`render_yaw`] over the network track. The mesh extractor
/// makes the same choice, so the seat cannot run on a second clock.
///
/// `vehicle_network_id` is [`lodestone_ecs::session::Riding`]'s payload;
/// `own_network_id` is [`lodestone_ecs::session::ServerEntityId`]'s, used
/// only to resolve which of the vehicle's [`lodestone_ecs::entity::Passengers`]
/// seats is ours — the same lookup
/// `lodestone_ecs::player::pin_passenger_to_vehicle` does, repeated here
/// because that function computes a *tick-boundary* seat and this one needs a
/// *per-frame* one, off a different position input.
///
/// # Declines rather than guesses, mirroring `pin_passenger_to_vehicle`
///
/// `None` when any link is missing: the vehicle has no [`EntityIndex`] entry
/// yet (not spawned client-side), no render source (controlled pose history or
/// [`InterpFrom`]/[`InterpTo`]/[`InterpClock`]), no
/// [`lodestone_ecs::entity::EntityKind`], or [`lodestone_ecs::VersionData`]
/// holds no adapter or no facts for its type. The caller falls back to the
/// tick-boundary seat in every such case, so declining here never strands the
/// player somewhere invented.
pub(crate) fn riding_render_seat(
    world: &World,
    vehicle_network_id: i32,
    own_network_id: Option<i32>,
) -> Option<Vec3> {
    let index = world.get_resource::<EntityIndex>()?;
    let vehicle = index.get(vehicle_network_id)?;
    let kind = world.get::<lodestone_ecs::entity::EntityKind>(vehicle)?;
    let version = world.get_resource::<lodestone_ecs::VersionData>()?;
    let facts = version.entity_facts(&kind.0)?;
    let passengers = world.get::<lodestone_ecs::entity::Passengers>(vehicle);
    // Vanilla's own default passenger-attachment-point lookup indexes the
    // vehicle's passenger list to find this seat,
    // the same degenerate-case-agrees reasoning `pin_passenger_to_vehicle`'s
    // own doc gives for defaulting to seat 0.
    let seat_index = own_network_id
        .and_then(|own| passengers.and_then(|list| list.0.iter().position(|id| *id == own)))
        .unwrap_or(0);

    let controlled_pose = world
        .get_resource::<lodestone_ecs::FrameClock>()
        .and_then(|frame| {
            controlled_vehicle_render_pose(
                world.get_resource::<ControlledVehicle>(),
                vehicle_network_id,
                frame.interp_alpha,
            )
        });
    let (feet, yaw) = if let Some(pose) = controlled_pose {
        (
            Vec3::new(
                pose.position.x as f32,
                pose.position.y as f32,
                pose.position.z as f32,
            ),
            pose.yaw,
        )
    } else {
        let from = world.get::<InterpFrom>(vehicle)?;
        let to = world.get::<InterpTo>(vehicle)?;
        let clock = world.get::<InterpClock>(vehicle)?;
        (render_feet(from, to, clock), render_yaw(from, to, clock))
    };
    let seat = lodestone_ecs::riding::player_seat_position(
        Vec3d::new(f64::from(feet.x), f64::from(feet.y), f64::from(feet.z)),
        yaw,
        kind.0.path(),
        facts.dimensions.height,
        seat_index,
    );
    Some(Vec3::new(seat.x as f32, seat.y as f32, seat.z as f32))
}

/// The currently-drawn body yaw, taking the shortest arc so a wrap across 360°
/// (e.g. 350°→10°) turns +20° rather than −340°.
pub(super) fn render_yaw(from: &InterpFrom, to: &InterpTo, clock: &InterpClock) -> f32 {
    lerp_angle(from.yaw, to.yaw, alpha(clock))
}

/// The currently-drawn head yaw, shortest-arc like the body yaw.
pub(super) fn render_head_yaw(from: &InterpFrom, to: &InterpTo, clock: &InterpClock) -> f32 {
    lerp_angle(from.head_yaw, to.head_yaw, alpha(clock))
}

/// The currently-drawn head pitch. Pitch is bounded to ±90° and never wraps, so
/// a plain linear ease is correct.
pub(super) fn render_pitch(from: &InterpFrom, to: &InterpTo, clock: &InterpClock) -> f32 {
    from.pitch + (to.pitch - from.pitch) * alpha(clock)
}

/// The animation drive for this frame.
///
/// `partial_tick` is the fraction through the current 50 ms tick, used for the
/// walk cycle. The head yaw is clamped to the body and then expressed
/// *relative* to it; passing the absolute value would spin every mob's head
/// with its body.
///
/// `swing_progress` is the entity's interpolated `attack_anim` for this frame —
/// `0.0` for an entity that has never swung, otherwise
/// [`lodestone_ecs::entity::AttackSwing::attack_anim_lerp`] resolved by the
/// caller through [`EntityIndex`] (see [`extract_entity_draws`]). The local
/// player's swing is *not* this path: it is not a tracked network entity and
/// goes through `Sim::body_pose`/`Sim::hand_swing_progress` instead.
/// `aggressive` is bit `0x04` of the mob-flags byte, folded into [`MobState`]
/// by `ingest::apply_entity_metadata` and resolved by the caller through
/// [`EntityIndex`] the same way `swing_progress` is. It selects the raised-arm
/// pose for mobs whose renderer supports that stance.
///
/// `crouching` is the [`Pose`] component at metadata index 6, **not** the
/// shift-key bit of the shared-flags byte; see the `poses` query in
/// [`extract_entity_draws`] for why the two are not interchangeable. It drives
/// the humanoid crouch branch, so a sneaking remote player hunches exactly as
/// the local self-avatar does.
///
/// `swim_amount` is the interpolated swim amount for this frame — the same
/// [`SwimRamp`]-integrated value [`extract_entity_draws`]
/// already computes and carries on [`EntityDraw::swim_amount`] for the
/// whole-body prone rotation. This is the second, independent consumer: it
/// drives the humanoid swim branch — the arm-over-arm stroke and leg kick —
/// which reads it from `AnimInput` rather than `EntityDraw`.
pub(super) fn render_anim(
    from: &InterpFrom,
    to: &InterpTo,
    clock: &InterpClock,
    walk: &WalkAnim,
    partial_tick: f32,
    swing_progress: f32,
    arm_pose: ArmPoseChoice,
    aggressive: bool,
    crouching: bool,
    is_passenger: bool,
    swim_amount: f32,
    armor_stand_pose: Option<lodestone_model::ArmorStandPose>,
    boat_hurt: lodestone_render::entity_anim::BoatHurt,
) -> AnimInput {
    let body = render_yaw(from, to, clock);
    let head = clamp_head_to_body(body, render_head_yaw(from, to, clock), MAX_HEAD_YAW);
    AnimInput {
        head_yaw_deg: wrap_degrees(head - body),
        head_pitch_deg: render_pitch(from, to, clock),
        limb_swing: walk.walk.position_lerp(partial_tick),
        limb_swing_amount: walk.walk.speed_lerp(partial_tick),
        attack_anim: swing_progress,
        age_ticks: clock.age,
        aggressive,
        arm_pose: arm_pose.pose,
        arm_pose_left_hand: arm_pose.left_hand,
        crouching,
        is_passenger,
        swim_amount,
        armor_stand_pose,
        boat_hurt,
    }
}

/// Vanilla's namespace. Matched explicitly rather than ignored: a resource pack
/// or mod item at `mypack:bow` is a *different* item and must not inherit the
/// bow's arm pose from its path alone.
const VANILLA: &str = "minecraft";
/// The `minecraft:bow` item path, matched by identity because the arm pose is a
/// per-item special case in vanilla too (vanilla's own bow use-animation).
const BOW_PATH: &str = "bow";
/// The `minecraft:crossbow` item path.
const CROSSBOW_PATH: &str = "crossbow";
/// The `minecraft:air` item path — vanilla's own is-empty check's other half. A slot
/// holding air is an *empty* hand, so it must not raise an arm.
const AIR_PATH: &str = "air";

/// Vanilla's own crossbow charge-duration accessor with **no Quick Charge**:
/// `25 - 5 * level`, at level 0.
///
/// The enchantment level is not modelled. Reading it would mean resolving a
/// stack's `minecraft:enchantments` list against the enchantment registry, and
/// while [`lodestone_model::ItemComponents::enchantments`] does carry the list,
/// the render-side equipment set is narrowed to bare item ids
/// ([`RenderEquipment`]) long before it reaches here — an enchanted crossbow
/// therefore charges visually slower than it really does, and finishes its wind
/// animation late. Recorded rather than fixed because widening `RenderEquipment`
/// to full stacks is a larger change than the pose it would serve.
///
/// **Shared with the item *model* path** rather than restated: the same number
/// divides `minecraft:crossbow/pull`, which picks `item/crossbow_pulling_0/1/2`.
/// Two copies would let a crossbow's arms and its model disagree about how far
/// along the same wind is, on the same frame. An alias rather than a re-import so
/// the derivation stays documented at the place the arm pose reads it.
const CROSSBOW_CHARGE_TICKS: f32 = lodestone_render::CROSSBOW_CHARGE_TICKS;

/// Which arm pose an entity's arms take, and in which hand.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(super) struct ArmPoseChoice {
    pub(super) pose: ArmPose,
    pub(super) left_hand: bool,
}

/// Chooses the arm pose from the item in the used hand and how long it has been
/// used — vanilla's own avatar and abstract-skeleton arm-pose selection,
/// reduced to the poses [`ArmPose`] models.
///
/// # Bow vs crossbow: two different triggers, and only one is the using-item bit
///
/// * **Bow** — vanilla's own bow use-animation, gated purely on
///   `getUsedItemHand() == hand && getUseItemRemainingTicks() > 0`. Our
///   [`ItemUse`] flag is exactly that gate.
/// * **Crossbow charge** — vanilla's own crossbow use-animation, same gate, plus the wind
///   fraction from the tick counter.
/// # The mob trigger is a *different flag*, and it is checked first
///
/// Arm poses are selected per renderer, and a renderer-specific aggressive-bow
/// override takes precedence over the generic item-use path. A ranged mob does
/// not enter the item-use state, so relying on `item_use` alone would leave its
/// bow pose unreachable.
///
/// The override is keyed on the entity type by
/// [`mob_draws_bow_when_aggressive`], because it is genuinely per-renderer: an
/// aggressive *zombie* holding a bow gets no such pose, and an aggressive
/// pillager's poses come from a different model family entirely.
///
/// * **Crossbow hold** — **not** an in-use pose at all. Vanilla checks
///   `!swinging && is(CROSSBOW) &&` its own is-charged check, where
///   that check reads the stack's `minecraft:charged_projectiles` component.
///   That component is **not modelled** by this build's item codec
///   ([`lodestone_model::ItemComponents`] has no field for it, and an
///   unrecognised component sets `has_unmodeled` and halts the patch decode), so
///   a charged crossbow is indistinguishable here from an empty one and this
///   function can never return [`ArmPose::CrossbowHold`]. The pose *math* is
///   implemented and tested in `lodestone-render`; what is missing is the wire
///   fact that selects it. Deliberately left as a gap rather than approximated:
///   guessing "charged" from anything else available would make every crossbow in
///   the world hold the shooting pose permanently, which is more wrong, more
///   often, than the resting pose it gets today.
///
/// # `ArmPose::Item` for a merely-held item, and why only an avatar gets it
///
/// Vanilla's own avatar-arm-pose selection's final rule — spears get their own
/// pose, everything else non-empty gets the plain item pose — runs for
/// **any non-empty hand**, in use or not — but that rule is in
/// its own avatar-only arm-pose selection, and **a humanoid mob never reaches it**.
/// Its own humanoid-mob-renderer arm-pose selection, the base every mob override delegates to, ends
/// with spears posed and everything else empty instead. So a player holding a sword raises the arm and a zombie
/// holding the same sword does not, and [`renderer_is_avatar`] is what separates
/// them. An earlier reading of this file had it that *every* armed mob raises an arm
/// in vanilla; that was a transcription of the right method for the wrong renderer,
/// and acting on it would have posed every armed zombie, skeleton and armour stand.
///
/// Because the fallthrough is now avatar-only, it changes **no** mob silhouette, and
/// neither bow-pose pixel gate (both of which use a skeleton subject and a zombie
/// control) needed re-baselining.
pub(super) fn arm_pose_for(
    type_path: &str,
    equipment: &[(EquipmentSlot, ResourceLocation)],
    item_use: Option<ItemUse>,
    aggressive: bool,
    main_arm_left: bool,
) -> ArmPoseChoice {
    // Vanilla's own abstract-skeleton arm-pose selection's override, ahead of
    // the base using-item rule exactly as vanilla's `? :` puts it ahead of the
    // base case.
    //
    // `getMainArm() == arm` is vanilla's left-handed fork. The bow always sits
    // in the main *hand* (`main_hand_holds_bow` only ever looks at
    // `EquipmentSlot::MainHand`), so the physical arm that draws it is simply
    // `main_arm_left` — a left-handed skeleton draws with its left arm.
    if aggressive && mob_draws_bow_when_aggressive(type_path) && main_hand_holds_bow(equipment) {
        return ArmPoseChoice {
            pose: ArmPose::BowAndArrow,
            left_hand: main_arm_left,
        };
    }
    if let Some(choice) = in_use_arm_pose(equipment, item_use, main_arm_left) {
        return choice;
    }
    held_item_arm_pose(type_path, equipment, main_arm_left)
}

/// Vanilla's own avatar-arm-pose selection's using-item `if` chain — the poses that need the
/// item to be *in use*, ahead of the merely-held fallthrough.
///
/// `None` means "no in-use pose applies", which is a different answer from
/// `Some(Empty)`: it lets [`held_item_arm_pose`] have its turn. Extracted so the
/// composition of the two halves has a name, because that seam is where the
/// interesting mistakes live — the in-use half alone was shipped once, and reading
/// it in isolation is what made "vanilla is wider" look like a one-line widening of
/// *this* function rather than a per-renderer question.
pub(super) fn in_use_arm_pose(
    equipment: &[(EquipmentSlot, ResourceLocation)],
    item_use: Option<ItemUse>,
    main_arm_left: bool,
) -> Option<ArmPoseChoice> {
    let item_use = item_use?;
    if !item_use.using {
        return None;
    }
    let slot = if item_use.off_hand {
        EquipmentSlot::OffHand
    } else {
        EquipmentSlot::MainHand
    };
    // Using something we were never told about. Falling through rather than
    // guessing: equipment and metadata are separate packets and either can arrive
    // first.
    let (_, held) = equipment.iter().find(|(s, _)| *s == slot)?;
    if held.namespace() != VANILLA {
        return None;
    }
    let pose = match held.path() {
        BOW_PATH => ArmPose::BowAndArrow,
        CROSSBOW_PATH => ArmPose::CrossbowCharge {
            progress: item_use.ticks as f32 / CROSSBOW_CHARGE_TICKS,
        },
        // Vanilla's own use-animation chain, reduced. **For eating and drinking
        // this is not an approximation — it is what vanilla does.**
        // Its own eat and drink use-animations are deliberately absent from that
        // chain, so a consuming entity takes the plain held-item raise and the
        // whole distinctive eating motion lives in
        // vanilla's own held-item eat-transform, first person only.
        //
        // For the poses `ArmPose` still does not model — `BLOCK` (a raised shield),
        // `SPYGLASS`, `TOOT_HORN`, `BRUSH`, `THROW_TRIDENT`, `SPEAR` — this is a
        // *closer* wrong answer than `Empty`, not a right one: vanilla reaches each
        // of those before the fallthrough. `Item` at least puts the arm up, which is
        // the half those poses share; arms hanging at the sides is the reading that
        // looks like the feature is off.
        _ => ArmPose::Item,
    };
    Some(ArmPoseChoice {
        pose,
        // The *physical* arm, not the equipment slot: for a right-handed mob
        // (the common case) the off hand is the left arm and this is just
        // `item_use.off_hand`, but a left-handed mob's off hand is its right
        // arm — hence the XOR against `main_arm_left` rather than the bare
        // slot bit.
        left_hand: item_use.off_hand != main_arm_left,
    })
}

/// Vanilla's own avatar-arm-pose selection's tail: any non-empty hand gets `ArmPose.ITEM`,
/// whether the item is in use or not.
///
/// **Avatar renderers only** — see [`renderer_is_avatar`] for the measurement. A
/// humanoid *mob* ends at its own humanoid-mob-renderer arm-pose selection's `EMPTY`, so its arms
/// hang, which is what this build already did and what vanilla does.
///
/// Vanilla poses each arm from its own hand and can raise **both** at once
/// (`getMainArm() == arm ? mainHandPose : offHandPose`). [`ArmPoseChoice`] carries
/// one pose and one hand, so the main hand wins when both are full; the off hand is
/// reached only when the main hand is empty, which is the case that would otherwise
/// pose the wrong arm.
pub(super) fn held_item_arm_pose(
    type_path: &str,
    equipment: &[(EquipmentSlot, ResourceLocation)],
    main_arm_left: bool,
) -> ArmPoseChoice {
    if !renderer_is_avatar(type_path) {
        return ArmPoseChoice::default();
    }
    for (slot, off_hand) in [
        (EquipmentSlot::MainHand, false),
        (EquipmentSlot::OffHand, true),
    ] {
        if hand_is_occupied(equipment, slot) {
            return ArmPoseChoice {
                pose: ArmPose::Item,
                // Physical arm, same XOR as `in_use_arm_pose`.
                left_hand: off_hand != main_arm_left,
            };
        }
    }
    ArmPoseChoice::default()
}

/// `!itemInHand.isEmpty()` for one hand.
///
/// [`RenderEquipment`] only carries occupied slots — the narrowing drops an explicit
/// clear rather than keeping it as a present-but-empty entry — so presence is most of
/// the answer. `minecraft:air` is rejected as well: vanilla's own is-empty check is
/// air-or-zero-count, and a server that sends air explicitly must not raise an arm.
pub(super) fn hand_is_occupied(
    equipment: &[(EquipmentSlot, ResourceLocation)],
    slot: EquipmentSlot,
) -> bool {
    equipment
        .iter()
        .any(|(s, item)| *s == slot && !(item.namespace() == VANILLA && item.path() == AIR_PATH))
}

/// Vanilla's own main-hand-item-is-bow check — the *item identity* half of the
/// skeleton override.
///
/// Split out so the "does the fixture's mob actually hold a bow in the equipment
/// the draw reads" question has one place to be true. That is the world species of
/// vacuous test: a gate whose skeleton has a bow in `OffHand`, or in no slot at
/// all, exercises none of this and still reads as a wiring failure.
pub(super) fn main_hand_holds_bow(equipment: &[(EquipmentSlot, ResourceLocation)]) -> bool {
    equipment.iter().any(|(slot, item)| {
        *slot == EquipmentSlot::MainHand && item.namespace() == VANILLA && item.path() == BOW_PATH
    })
}

/// Wraps degrees into `(-180, 180]`, like `Mth.wrapDegrees`.
pub(super) fn wrap_degrees(deg: f32) -> f32 {
    angle_diff(deg, 0.0)
}

/// Narrows a snapshot's per-slot equipment to the slots that actually hold an
/// item — dropping both the never-reported slots (absent already) and the
/// explicitly-empty ones, which draw nothing either way.
pub(super) fn occupied_equipment(
    equipment: &[(EquipmentSlot, Option<ResourceLocation>)],
) -> Vec<(EquipmentSlot, ResourceLocation)> {
    equipment
        .iter()
        .filter_map(|(slot, item)| item.clone().map(|id| (*slot, id)))
        .collect()
}

// ---------------------------------------------------------------------------

/// `Update` / `FrameSet::Interpolate`: advance every ease clock and age by this
/// frame's [`FrameDelta`].
///
/// Runs **before** the 20 Hz tick loop, so a snapshot that resets `t` to 0 this
/// frame starts its new window from exactly the pose that was on screen.
pub fn advance_interp_clocks(delta: Res<FrameDelta>, mut clocks: Query<&mut InterpClock>) {
    for mut clock in &mut clocks {
        clock.t = (clock.t + delta.0).min(clock.window);
        clock.age += delta.0 * TICKS_PER_SECOND;
    }
}

/// `GameTick` / `TickSet::Animate`: one 20 Hz step of every entity's walk cycle.
///
/// Measures the per-tick travel off the *drawn* position, which is what vanilla
/// measures — see the module docs on why the interpolation gap is wrong by
/// [`INTERP_STEPS`]. It needs no separate "has it stopped?" rule: a mob that
/// stops stops moving its drawn position, so the distance goes to zero and the
/// amplitude decays on its own.
pub fn tick_walk_animation(
    mut tracks: Query<(
        &InterpFrom,
        &InterpTo,
        &InterpClock,
        &RenderScale,
        &mut WalkAnim,
    )>,
) {
    for (from, to, clock, scale, mut walk) in &mut tracks {
        let now = render_feet(from, to, clock);
        let distance = (now - walk.last_feet).with_y(0.0).length();
        walk.last_feet = now;
        let limb_scale = if scale.0 < 1.0 {
            BABY_LIMB_SCALE
        } else {
            ADULT_LIMB_SCALE
        };
        walk.walk.update(
            walk_target_speed(distance),
            LIMB_SWING_SMOOTHING,
            limb_scale,
        );
    }
}
