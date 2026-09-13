//! ECS-to-render extraction, including synthetic pickup-flight draws.

use super::*;

/// Resolve exact custom-name cosmetics at the entity-to-draw boundary.
///
/// Upside-down transforms are supported for built-in living entities with a
/// model-backed draw. Non-living and sprite-only entities intentionally remain
/// unchanged because their renderers do not share the living model transform.
#[must_use]
pub(crate) fn named_entity_cosmetics(
    entity_type: Option<lodestone_data::entity_type::EntityType>,
    custom_name: Option<&str>,
) -> NamedEntityCosmetics {
    let Some(name) = custom_name else {
        return NamedEntityCosmetics::default();
    };
    let upside_down = matches!(name, "Dinnerbone" | "Grumm")
        && entity_type.is_some_and(|entity_type| {
            // These model-backed living types intentionally do not appear in
            // the crowd-push census because they override that behaviour.
            lodestone_data::entity_census::is_living(entity_type)
                || matches!(
                    entity_type,
                    EntityType::ArmorStand | EntityType::Bat | EntityType::Parrot
                )
        });
    let rainbow_wool = name == "jeb_" && entity_type == Some(EntityType::Sheep);
    NamedEntityCosmetics {
        upside_down,
        rainbow_wool,
    }
}

// The item-pickup fly-to-collector animation
// ---------------------------------------------------------------------------

/// Vanilla's own item-pickup-particle lifetime — the pickup flight lasts **3 ticks** (150 ms).
///
/// Read from the 26.2 decompile:
/// `protected static final int LIFE_TIME = 3;`, and `tick()` removes the particle
/// on the tick `life` reaches it.
const PICKUP_LIFE_TICKS: f32 = 3.0;

/// The height above the collector's feet the item flies *to*, as a fraction of
/// the collector's eye height.
///
/// Vanilla's own pickup-particle position update targets
/// `(target.getY() + target.getEyeY()) / 2.0`, and its own eye-Y accessor is
/// `position.y + eyeHeight` — an **absolute** Y, not an
/// offset. So the midpoint is `y + eyeHeight / 2`, i.e. this constant times the
/// eye height above the feet. Reading `getEyeY()` as a relative offset instead
/// would target `y + (y + 1.62)/2`, which for a player at y = 64 is 32 blocks
/// below the floor.
const PICKUP_TARGET_EYE_FRACTION: f32 = 0.5;

/// A **remote** collector's assumed eye height, for the
/// [`PICKUP_TARGET_EYE_FRACTION`] midpoint.
///
/// `lodestone_physics::player::DEFAULT_EYE_HEIGHT` is `Avatar.DEFAULT_EYE_HEIGHT`
/// (`1.62`). The local player's own live `PhysicsState` eye height is used instead
/// when the collector *is* us (it tracks the swimming/crawling pose), so this
/// constant only covers other players and mobs — a fox or an allay picking
/// something up aims 0.81 blocks up rather than at its own smaller midpoint. An
/// approximation, and the only one in this animation: the render-side track set
/// carries no per-entity eye height, and inventing one from [`RenderScale`] would
/// be a guess dressed as a measurement.
const REMOTE_COLLECTOR_EYE_HEIGHT: f32 = lodestone_physics::player::DEFAULT_EYE_HEIGHT;

/// One in-flight item-pickup animation: a **frozen copy** of a collected item,
/// travelling from where the item was drawn to the entity that collected it.
///
/// # Why a copy, and not the item entity retargeted
///
/// This is the part that is easy to get backwards. Vanilla does **not** keep the
/// item entity alive and lerp it: its own take-item-entity packet handling
/// extracts the item's render state (`extractEntity(from, 1.0F)`), hands that
/// *copy* to a new pickup particle, and then calls
/// its own remove-entity with a discarded reason in the
/// same breath. The entity is gone before the animation starts; what flies is a
/// snapshot.
///
/// That is also why this is a resource rather than a component: by the time
/// `fold_entities` next runs, the server has stopped reporting the item and the
/// render track is pruned. An animation hung off the track would be despawned
/// with it, one frame in.
#[derive(Debug, Clone, PartialEq)]
pub struct PickupAnimation {
    /// The collected item entity's id, kept only as the bob-phase key
    /// [`lodestone_render::entity::item_bob_offset`] hashes — the same phase the
    /// item had before it was picked up, so the copy does not visibly re-roll.
    pub item_entity_id: EntityNetworkId,
    /// Which item model to draw.
    pub item: ResourceLocation,
    /// The player-head profile skin selected when this render-state snapshot was
    /// captured. `None` keeps the special renderer's vanilla Steve fallback.
    pub item_skin: Option<Arc<str>>,
    /// The collected stack size, carried for parity with [`EntityDraw::count`].
    pub count: u32,
    /// Whether the collected stack was enchanted, so the flying copy glints too.
    pub foil: bool,
    /// Mirrors [`TrackedStack::dyed_color`], captured at the same instant as
    /// [`Self::foil`] so the flying copy's tint cannot disagree with its glint.
    pub dyed_color: Option<u32>,
    /// Mirrors [`TrackedStack::potion_color`].
    pub potion_color: Option<u32>,
    /// The item's render scale at capture.
    pub scale: f32,
    /// Where the item was **drawn** when the pickup arrived — not its last
    /// reported position. `ItemPickupParticle` is constructed from the extracted
    /// render state, which is the interpolated pose, so this is the same quantity.
    pub start: Vec3,
    /// Frozen `ageInTicks`, so the bob/spin stop the instant the copy is taken —
    /// vanilla's render state is extracted once and never re-extracted.
    pub age_ticks: f32,
    /// The collecting entity's id. **Any** entity, not just the local player:
    /// vanilla animates a mob's pickup too.
    pub collector_id: EntityNetworkId,
    /// Whole ticks elapsed, `0..PICKUP_LIFE_TICKS`.
    pub life: f32,
}

/// Every item-pickup animation currently in flight.
///
/// Started by [`begin_item_pickup`] (from `Sim::poll_net`, off
/// `ClientEvent::ItemPickup`), advanced by [`tick_pickup_animations`] at 20 Hz,
/// and drawn by [`extract_pickup_draws`] — which is the answer to "what consumes
/// this": it appends an ordinary [`EntityDraw`] per animation, so the flight goes
/// through the *existing* dropped-item geometry path
/// (`RenderState::prepare_item_geometry`) with no new pipeline.
#[derive(Resource, Debug, Default)]
pub struct PickupAnimations(pub(super) Vec<PickupAnimation>);

impl PickupAnimations {
    /// How many animations are in flight.
    #[must_use]
    pub fn len(&self) -> usize {
        self.0.len()
    }

    /// Whether nothing is in flight.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// The in-flight animations, for a test or a debug overlay.
    #[must_use]
    pub fn animations(&self) -> &[PickupAnimation] {
        &self.0
    }
}

/// Vanilla's own pickup-particle-group instance easing: the age fraction
/// (life plus partial tick, divided by the 3-tick lifetime) is squared before
/// it is used to lerp the item's position toward the collector.
///
/// So the interpolant is **quadratic in the age fraction** — an ease-*in*: the
/// item leaves slowly and arrives fast. A linear lerp is the obvious wrong
/// reading and is visibly different at the midpoint: at `life + partial = 1.5`
/// the correct fraction is `0.25`, a linear one gives `0.5`.
#[must_use]
pub(super) fn pickup_progress(life: f32, partial_tick: f32) -> f32 {
    let t = ((life + partial_tick) / PICKUP_LIFE_TICKS).clamp(0.0, 1.0);
    t * t
}

/// Start a pickup animation for `item_entity_id` flying to `collector_id`.
///
/// Returns `false` — and starts nothing — when the item was not tracked on the
/// render side, either because its stack was never reported or because the track
/// has already been pruned. Drawing a flight from a made-up start point would be
/// worse than drawing none, and "no animation" is the established behavior
/// for an untracked or stackless item.
///
/// **Must be called before the frame's `fold_entities`.** `Sim::poll_net` runs
/// ahead of `Sim::fold_entities`, so the track the server has stopped reporting
/// is still present here and gone one call later — that ordering is the whole
/// reason this is a function called from `poll_net` rather than a system.
pub fn begin_item_pickup(
    world: &mut World,
    item_entity_id: EntityNetworkId,
    collector_id: EntityNetworkId,
) -> bool {
    let Some(stack) = world
        .resource::<ItemStacks>()
        .0
        .get(&item_entity_id)
        .cloned()
    else {
        return false;
    };
    let Some(entity) = world
        .resource::<TrackIndex>()
        .0
        .get(&item_entity_id)
        .copied()
    else {
        return false;
    };
    let Some((start, age_ticks, scale)) = world.get_entity(entity).ok().and_then(|entity| {
        let from = entity.get::<InterpFrom>()?;
        let to = entity.get::<InterpTo>()?;
        let clock = entity.get::<InterpClock>()?;
        let scale = entity.get::<RenderScale>()?;
        Some((render_feet(from, to, clock), clock.age, scale.0))
    }) else {
        return false;
    };
    world
        .resource_mut::<PickupAnimations>()
        .0
        .push(PickupAnimation {
            item_entity_id,
            item: stack.render_definition().clone(),
            item_skin: stack.skin,
            count: stack.count,
            foil: stack.foil,
            dyed_color: stack.dyed_color,
            potion_color: stack.potion_color,
            scale,
            start,
            age_ticks,
            collector_id,
            life: 0.0,
        });
    true
}

/// `GameTick` / `TickSet::Animate`: one 20 Hz step of every pickup flight.
///
/// Vanilla's own pickup-particle tick increments `life` and removes the
/// particle once it reaches 3 — so an
/// animation is drawn on ticks 0, 1 and 2 and gone on 3. Advancing this per
/// *frame* would make the flight last 3 frames (50 ms at 60 fps), the same
/// frame-rate-dependence `Sim::step`'s note on `chest_lids.tick()` records.
pub fn tick_pickup_animations(mut pickups: ResMut<PickupAnimations>) {
    for pickup in &mut pickups.0 {
        pickup.life += 1.0;
    }
    pickups.0.retain(|pickup| pickup.life < PICKUP_LIFE_TICKS);
}

/// `Extract` / `ExtractSet::Entities`, **after** [`extract_entity_draws`]:
/// append one [`EntityDraw`] per in-flight pickup at its interpolated position.
///
/// This is the consumer that makes the animation reach pixels. It emits a draw
/// whose `type_path` is [`ITEM_ENTITY_TYPE_PATH`], the same one a live dropped item
/// emits, so
/// `RenderState::prepare_item_geometry` picks it up with no change at all on the
/// GPU side — the flight is an existing draw at a new position, not a new pass.
///
/// Ordering matters twice over: [`extract_entity_draws`] **clears**
/// [`ExtractedDraws`], so running before it would have every pickup wiped in the
/// same frame it was written — a green-unit-test island of exactly the shape
/// `CLAUDE.md` §1 describes.
pub fn extract_pickup_draws(
    clock: Res<lodestone_ecs::FrameClock>,
    pickups: Res<PickupAnimations>,
    index: Res<TrackIndex>,
    poses: Query<(&InterpFrom, &InterpTo, &InterpClock)>,
    locals: Query<(&MinecraftEntityId, &PhysicsState), With<LocalPlayer>>,
    mut out: ResMut<ExtractedDraws>,
) {
    if pickups.0.is_empty() {
        return;
    }
    let partial_tick = clock.interp_alpha.clamp(0.0, 1.0);
    for pickup in &pickups.0 {
        let Some(target) = collector_target(pickup.collector_id, &index, &poses, &locals) else {
            continue;
        };
        let feet = pickup
            .start
            .lerp(target, pickup_progress(pickup.life, partial_tick));
        out.0.push(EntityDraw {
            id: pickup.item_entity_id.raw(),
            type_path: Arc::from(ITEM_ENTITY_TYPE_PATH),
            named_cosmetics: NamedEntityCosmetics::default(),
            item: Some(pickup.item.clone()),
            item_model: None,
            item_skin: pickup.item_skin.clone(),
            count: pickup.count,
            foil: pickup.foil,
            item_dyed_color: pickup.dyed_color,
            item_potion_color: pickup.potion_color,
            equipment: Vec::new(),
            equipment_dye: Vec::new(),
            equipment_skin: Vec::new(),
            equipment_trim: Vec::new(),
            wool: None,
            // A pickup animation is always a dropped item, never a falling block
            // and never an experience orb — the XP an orb pays goes straight to
            // the bar, so there is no flight animation for one to reuse.
            block_state: None,
            item_frame_rotation: 0,
            experience_orb_value: None,
            tnt_fuse: None,
            cape_sway: (0.0, 0.0, 0.0),
            painting: None,
            firework: None,
            // A pickup animation is a dropped item in flight, never a
            // projectile — nothing cast it, so there is no owner to anchor to.
            projectile_owner: None,
            feet,
            yaw: 0.0,
            head_yaw: 0.0,
            pitch: 0.0,
            scale: pickup.scale,
            anim: AnimInput {
                // Frozen at capture, like the extracted render state it stands
                // in for. Everything else in `AnimInput` is meaningless for an
                // item model, which has no skeleton.
                age_ticks: pickup.age_ticks,
                ..AnimInput::default()
            },
            name_tag: None,
            hurt: false,
            // An item entity is not a `LivingEntity`, so it has no `deathTime` to
            // topple over — vanilla's fall-over lives on `LivingEntityRenderer`, and
            // an item's renderer never calls `setupRotations` at all.
            death_time: 0.0,
            // A flying pickup is an item entity, not a living one: nothing can be
            // using it, so its variant resolves in `DisplaySlot::Ground` alone.
            item_use: None,
            // An item entity is not a `Mob` and has no main arm at all.
            main_arm_left: false,
            // An item entity is never a creeper.
            creeper_swelling: 0.0,
            // An item entity never swims — vanilla's own swim-amount update is a
            // `LivingEntity` behaviour and `ItemEntity` is not one.
            swim_amount: 0.0,
            // A pickup-flight animation is synthetic (this pass's own item, not
            // a tracked entity with `EntityFlags`) and vanishes in 3 ticks — not
            // worth threading a real flag lookup through for.
            on_fire: false,
            // Same reasoning as `on_fire`: a synthetic pickup-flight item has
            // no `EntityFlags` to read invisible off, and vanishes too fast
            // to matter if it did.
            invisible: false,
            // An item entity is never an armour stand.
            armor_stand: None,
            // An item entity is never a player either.
            player_skin: None,
            variant_sheet: None,
        });
    }
}

/// Where a pickup flies *to*: the collector's `(x, y + eyeHeight/2, z)`.
///
/// Resolved fresh every frame rather than captured at pickup time, because
/// vanilla's own pickup-particle position update re-reads `target.getX()/getY()` on every
/// tick — a pickup while walking must chase the collector, not aim at where they
/// used to be.
///
/// Two sources, in order, and the local player needs the second one: it has no
/// [`RenderKind`]/[`InterpTo`] render track at all (that absence is deliberate —
/// see `lodestone_ecs::ingest::apply_local_player_login` on why a self-model
/// stays off the render path), so resolving only through [`TrackIndex`] would
/// silently animate nothing for **every pickup the player makes**, which is all
/// of them that matter.
pub(super) fn collector_target(
    collector_id: EntityNetworkId,
    index: &TrackIndex,
    poses: &Query<(&InterpFrom, &InterpTo, &InterpClock)>,
    locals: &Query<(&MinecraftEntityId, &PhysicsState), With<LocalPlayer>>,
) -> Option<Vec3> {
    for (id, state) in locals {
        if EntityNetworkId::from_raw(id.0) == collector_id {
            let p = state.0.position;
            return Some(Vec3::new(
                p.x as f32,
                p.y as f32 + state.0.eye_height * PICKUP_TARGET_EYE_FRACTION,
                p.z as f32,
            ));
        }
    }
    let entity = index.0.get(&collector_id).copied()?;
    let (from, to, clock) = poses.get(entity).ok()?;
    let feet = render_feet(from, to, clock);
    Some(feet + Vec3::Y * (REMOTE_COLLECTOR_EYE_HEIGHT * PICKUP_TARGET_EYE_FRACTION))
}

// ---------------------------------------------------------------------------

pub fn extract_entity_draws(
    (clock, controlled): (
        Res<lodestone_ecs::FrameClock>,
        Option<Res<ControlledVehicle>>,
    ),
    stacks: Res<ItemStacks>,
    // `AttackSwing` lives on the *ingest* entity (`lodestone_ecs::ingest::
    // apply_entity_animation` resolves `EntityAnimation` through `EntityIndex`),
    // not on the render entity this query's tuple is drawn from — `entity_id`
    // is the only key the two families share, so `index` is the bridge. See
    // `render_anim`'s doc for why this is not folded through `EntityFacts`
    // like every other field here.
    index: Res<EntityIndex>,
    swings: Query<&AttackSwing>,
    // `HurtTime` lives on the ingest entity too (`apply_entity_damaged` /
    // `apply_entity_hurt_animation` resolve it through the same `EntityIndex`),
    // so it is bridged the same way `AttackSwing` is rather than folded through
    // `EntityFacts` — see `EntityDraw::hurt`.
    hurts: Query<&HurtTime>,
    // `DeathTime` lives on the ingest entity too and is bridged through the same
    // `EntityIndex` as `HurtTime`. Reading both values keeps the red overlay and
    // the death animation driven by their respective countdowns.
    deaths: Query<&DeathTime>,
    // `ItemUse` lives on the ingest entity too and is bridged through the same
    // `EntityIndex` as `AttackSwing` and `HurtTime`. It turns the metadata bit
    // into the item-use arm pose in `arm_pose_for`.
    item_uses: Query<&ItemUse>,
    // `EntityFlags` lives on the ingest entity too and is bridged through the
    // same `EntityIndex`. Bit `0x01` drives the mob-fire billboard in
    // `EntityDraw::on_fire`.
    flags: Query<&EntityFlags>,
    // `MobState` lives on the ingest entity too and is bridged through the same
    // `EntityIndex`. Its aggressive bit selects a drawn bow and raised arms in
    // `arm_pose_for`.
    mob_states: Query<&MobState>,
    // `Pose` lives on the ingest entity too (`apply_entity_metadata` inserts it
    // from the pose accessor at index 6 — a *separate* field from the shared-flags
    // byte `EntityFlags` carries), bridged the same way. It was folded and read by
    // nothing: this is the query that turns it into a sneaking player's crouch.
    //
    // **Deliberately not `EntityFlags & 0x02`.** Vanilla's own is-crouching check
    // tests the crouching pose, and the shift-key bit is
    // vanilla's own is-shift-key-down/is-discrete checks — which is what the
    // *nametag* see-through gate below reads, correctly, because
    // vanilla's own should-show-name check really does ask its own is-discrete
    // check. Two
    // questions, two fields; a shift-key-down player whose standing box does not
    // fit is `SWIMMING`, not `CROUCHING`.
    poses: Query<&Pose>,
    // Custom names live on the ingest entity, not the render track. Keep the
    // raw text here so name-selected cosmetics do not depend on whether the
    // separate visibility gate produced a nameplate.
    custom_names: Query<&CustomName>,
    // `FallingBlockState` lives on the ingest entity too
    // (`apply_falling_block_state` inserts it from the spawn packet's Object Data
    // field through the same `EntityIndex`), bridged the same way. It is what turns
    // one VarInt into a drawn block — see `EntityDraw::block_state`, and note it is
    // the *only* thing a client is ever told about which block is falling.
    falling_blocks: Query<&FallingBlockState>,
    // `Tamed` lives on the ingest entity too and is bridged through the same
    // `EntityIndex` as `variants`. It supplies the tame bit that
    // `variant_sheet` uses to select the tamed texture.
    //
    // Paired with `vehicles` in one tuple parameter rather than two separate
    // ones: `bevy_ecs`'s `SystemParam` tuple impl tops out at 16 top-level
    // parameters, and this function was already at that ceiling before
    // `vehicles` needed to be added — nesting a tuple-of-`SystemParam` here
    // is itself one `SystemParam`, so it stays under the limit without
    // touching any of the other fifteen.
    //
    // `Vehicle` lives on the ingest entity too (`ingest::apply_entity_passengers`
    // folds it from `SET_PASSENGERS`'s reverse edge), bridged the same way
    // `tameds` is. Its mere *presence* is the sit-pose switch: a rider's own
    // network id never appears on its own entity, only the vehicle's it
    // names, so "is riding" is "does this component exist" for any tracked
    // (non-local) entity. See `AnimInput::is_passenger`.
    // `ArmorStandFlags` lives on the ingest entity too (`ingest::
    // apply_entity_metadata` folds `MetadataClass::ArmorStand`'s byte into
    // it), bridged the same way `tameds`/`vehicles` are — nested into this
    // tuple rather than added as a seventeenth top-level parameter, for the
    // same `SystemParam`-arity reason `vehicles` was.
    // `ItemFrameRotation` lives on the ingest entity too (`ingest::
    // apply_entity_metadata` folds index 10's `INT` into it, gated on the adapter
    // having established the entity is a frame), bridged the same way
    // `tameds`/`vehicles`/`armor_stands` are — and nested into this same tuple
    // rather than added as a seventeenth top-level parameter, for the identical
    // `SystemParam`-arity reason. Adding it at the top level really does fail to
    // compile, with an `in_set` "method not found" error a hundred lines away
    // from the parameter that caused it.
    // `ArmorStandPose` lives on the ingest entity too (`ingest::
    // apply_entity_metadata` merges indices 16-21 into it), bridged the same
    // way `armor_stands` beside it is and nested here for the same
    // `SystemParam`-arity reason.
    //
    // Read as an `Option` whose **absence still means a pose**: every armour
    // stand overwrites the humanoid walk cycle in vanilla, posed or not, so a
    // missing component resolves to `ArmorStandPose::VANILLA_DEFAULT` rather
    // than to "no pose". See `ARMOR_STAND_TYPE_PATH`.
    (
        orb_values,
        tnt_fuses,
        variants,
        tameds,
        vehicles,
        armor_stands,
        item_frame_rotations,
        armor_stand_poses,
        painting_variants,
        firework_flags,
        projectile_owners,
        vehicle_hurts,
        player_model_customizations,
        velocities,
    ): (
        Query<&ExperienceOrbValue>,
        Query<&TntFuse>,
        Query<&lodestone_ecs::entity::Variant>,
        Query<&lodestone_ecs::entity::Tamed>,
        Query<&lodestone_ecs::entity::Vehicle>,
        Query<&lodestone_ecs::entity::ArmorStandFlags>,
        Query<&ItemFrameRotation>,
        Query<&lodestone_ecs::entity::ArmorStandPose>,
        // `PaintingVariant` lives on the ingest entity too
        // (`lodestone_ecs::ingest::apply_entity_metadata` inserts it from the
        // painting-variant serializer), bridged the same way and nested here
        // for the same `SystemParam`-arity reason as its neighbours.
        Query<&lodestone_ecs::entity::PaintingVariant>,
        // `FireworkFlags` lives on the ingest entity too, bridged the same way
        // and nested here for the same `SystemParam`-arity reason.
        Query<&lodestone_ecs::entity::FireworkFlags>,
        // `ProjectileOwner` lives on the ingest entity too
        // (`lodestone_ecs::ingest::apply_projectile_owner` inserts it from the
        // spawn packet's Object Data field, the same field `FallingBlockState`
        // reads under a different type's interpretation), bridged the same way
        // and nested here for the same `SystemParam`-arity reason.
        Query<&lodestone_ecs::entity::ProjectileOwner>,
        // `VehicleHurt` lives on the ingest entity too
        // (`lodestone_ecs::ingest::apply_entity_metadata` merges
        // `VehicleEntity`'s hurt/hurt-dir/damage triple into it), bridged the
        // same way and nested here for the same `SystemParam`-arity reason.
        Query<&lodestone_ecs::entity::VehicleHurt>,
        // Player model-layer customization is metadata on the ingest entity,
        // bridged through the same index as the other remote-entity state.
        Query<&PlayerModelCustomization>,
        // Spawn velocity supplies the motion direction needed by the
        // fall-flying wing target.
        Query<&Velocity>,
    ),
    tracks: Query<(
        &MinecraftEntityId,
        &RenderKind,
        &RenderScale,
        &InterpFrom,
        &InterpTo,
        &InterpClock,
        &WalkAnim,
        &RenderEquipment,
        (&RenderEquipmentDye, &RenderEquipmentSkin),
        &RenderEquipmentTrim,
        &RenderWool,
        &RenderNameTag,
        &RenderPlayerSkin,
        // Nested into one tuple slot, not three top-level ones, for the same
        // `SystemParam`/`WorldQuery` tuple-arity reason `(tameds, vehicles,
        // armor_stands)` above is nested — this tuple was already at fourteen
        // top-level items before `CapeLag` needed to join it.
        (
            // `Option`, not `&CreeperFuse` bare: present only on creepers,
            // same "absence is the switch" shape `ItemPhysics` uses elsewhere
            // in this module. Every non-creeper entity matches `None` here at
            // zero cost.
            Option<&CreeperFuse>,
            // Bare, not `Option`: `spawn_track` inserts this on every track
            // entity unconditionally — see [`SwimRamp`]'s own doc for why it
            // is not gated by `RenderKind` the way `CreeperFuse` is.
            &SwimRamp,
            // Bare too, same reason and the same unconditional
            // `spawn_track` insert — see [`CapeLag`]'s own doc.
            &CapeLag,
        ),
    )>,
    mut out: ResMut<ExtractedDraws>,
) {
    // The one accumulator's residual, published by `FrameClock::end_frame` before
    // `Extract` runs. This used to be `TickAccum`, the interpolator `World`'s own
    // second accumulator; it is now the *same* number the camera interpolates the
    // player with, which is the point of §4.1(c).
    let partial_tick = clock.interp_alpha.clamp(0.0, 1.0);
    out.0.clear();
    for (
        id,
        kind,
        scale,
        from,
        to,
        clock,
        walk,
        equipment,
        (equipment_dye, equipment_skin),
        equipment_trim,
        wool,
        name_tag,
        player_skin,
        (fuse, swim, cape_lag),
    ) in &tracks
    {
        let controlled_pose =
            controlled_vehicle_render_pose(controlled.as_deref(), id.0, partial_tick);
        let drawn_feet = controlled_pose.map_or_else(
            || render_feet(from, to, clock),
            |pose| {
                Vec3::new(
                    pose.position.x as f32,
                    pose.position.y as f32,
                    pose.position.z as f32,
                )
            },
        );
        let drawn_yaw =
            controlled_pose.map_or_else(|| render_yaw(from, to, clock), |pose| pose.yaw);
        let drawn_pitch =
            controlled_pose.map_or_else(|| render_pitch(from, to, clock), |pose| pose.pitch);
        // One lookup, not two: `item` and `count` both come from the same
        // recorded stack, and a drop with no stack yet must not manufacture a
        // count out of nowhere.
        //
        // **Not narrowed to `ITEM_ENTITY_TYPE_PATH`, and that narrowing is what
        // made three consumers dead code.** `ItemStacks` is only ever written
        // for an entity whose server metadata actually carried the `ITEM_STACK`
        // serializer, so keying it on the entity *type* as well added nothing
        // and silently answered `None` for every other claimant of that same
        // serializer: an item frame's contents (`ItemFrame.DATA_ITEM`), a
        // framed filled map, and a thrown projectile's real stack. Each of
        // those consumers is written, tested and reachable — and each is
        // guarded by its own entity-type check at the draw site, so widening
        // here cannot make anything draw twice. Every gate for them built its
        // own `EntityDraw` with `item: Some(..)` by hand and so could not see
        // the producer refusing to supply one; `live_framed_item_wire.rs` is
        // the gate that obtains this value the way production does.
        let stack = stacks.0.get(&EntityNetworkId::from_raw(id.0));
        // `0.0` for an id with no ingest entity (shouldn't happen — a render
        // track only exists once the entity has been spawned) or one that has
        // never swung (`AttackSwing` absent, like `HurtTime`).
        let swing_progress = index
            .get(id.0)
            .and_then(|entity| swings.get(entity).ok())
            .map_or(0.0, |swing| swing.attack_anim_lerp(partial_tick));
        // `deathTime + partialTicks` while dying, `0.0` while alive — vanilla's
        // own living-entity render-state extraction:
        // the death time is the tick count plus the partial tick while dying, or
        // zero while alive.
        //
        // The ternary is not decoration: `DeathTime` is inserted at **zero** on the
        // tick death is announced (see its own doc), and a bare `+ partial_tick`
        // would make that first tick report a fractional death time, starting the
        // fall-over — and the red overlay's `deathTime > 0` half — mid-frame instead
        // of on the tick boundary. Absent `DeathTime` is "alive", so this reads
        // `0.0` for every living entity at no cost.
        let death_time = index
            .get(id.0)
            .and_then(|entity| deaths.get(entity).ok())
            .map_or(0.0, |death| {
                if death.0 > 0 {
                    death.0 as f32 + partial_tick
                } else {
                    0.0
                }
            });
        // hurt-time or death-time positive: vanilla's own red-overlay gate in full.
        // `false` for an entity that has never been hit (`HurtTime` absent, like
        // `AttackSwing`) — and also for one whose countdown has aged out, since
        // `tick_hurt_time` leaves the component in place at zero rather than
        // removing it.
        //
        // The `deathTime` half is what this field's doc used to name as its one
        // known divergence: on `hurtTime` alone the overlay ends ten ticks after the
        // killing blow, so a mob went red, turned its normal colour again, and only
        // *then* fell over. The disjunction is why vanilla's tint carries all the
        // way through the fall-over — the two counters run in opposite directions
        // and overlap by design.
        let hurt = index
            .get(id.0)
            .and_then(|entity| hurts.get(entity).ok())
            .is_some_and(|hurt| hurt.0 > 0)
            || death_time > 0.0;
        // The using-item state behind the bow/crossbow arm pose. `None` for an
        // entity that has never reported the byte (`ItemUse` absent, like
        // `AttackSwing`), which `arm_pose_for` reads as "not using anything".
        // `Mob.isAggressive()`. `false` for an entity that has never reported the
        // mob-flags byte (`MobState` absent) — which includes every non-`Mob`
        // entity permanently, because the adapter withholds index 15 for those.
        let aggressive = index
            .get(id.0)
            .and_then(|entity| mob_states.get(entity).ok())
            .is_some_and(|state| state.aggressive);
        // `Mob.getMainArm() == LEFT` — same bridge as `aggressive` above, off
        // the same `MobState` component, `false` for the same absent case.
        let main_arm_left = index
            .get(id.0)
            .and_then(|entity| mob_states.get(entity).ok())
            .is_some_and(|state| state.left_handed);
        // One lookup, two consumers: the arm pose below and `EntityDraw::item_use`,
        // which the held-item pass resolves the item's own definition tree against.
        // Reading it twice would let the two disagree about the same tick.
        let item_use = index
            .get(id.0)
            .and_then(|entity| item_uses.get(entity).ok())
            .map(|item_use| *item_use);
        let arm_pose = arm_pose_for(
            &kind.path,
            &equipment.0,
            item_use,
            aggressive,
            main_arm_left,
        );
        // Vanilla's own is-crouching check. `false` for an entity that has never reported
        // the pose accessor (`Pose` absent) — which is every entity that has
        // never left `STANDING`, since the server only sends metadata that
        // differs from the default.
        let crouching = index
            .get(id.0)
            .and_then(|entity| poses.get(entity).ok())
            .is_some_and(|pose| pose.0 == lodestone_model::EntityPose::Crouching);
        // Vanilla's own is-passenger check. `false` for an entity that has never been
        // named as a rider by `SET_PASSENGERS` (`Vehicle` absent) — see
        // `vehicles`'s own doc above. This is the local-player-*excluded* half
        // of the sit pose: the local player never has an ingest entity of its
        // own to carry `Vehicle`, and gets the same bit from
        // `lodestone_ecs::session::Riding` instead — see `Sim::body_anim`.
        let is_passenger = index
            .get(id.0)
            .and_then(|entity| vehicles.get(entity).ok())
            .is_some();
        let entity_flags = index.get(id.0).and_then(|entity| flags.get(entity).ok());
        let fall_flying = entity_flags.is_some_and(|flags| {
            lodestone_entity::metadata::SharedEntityFlags::from_bits(flags.0 as i8).fall_flying()
        }) || index
            .get(id.0)
            .and_then(|entity| poses.get(entity).ok())
            .is_some_and(|pose| pose.0 == lodestone_model::EntityPose::FallFlying);
        let motion = index
            .get(id.0)
            .and_then(|entity| velocities.get(entity).ok())
            .map(|velocity| to_glam_vec3(velocity.0))
            .unwrap_or(Vec3::ZERO);
        let cape_visible = index
            .get(id.0)
            .and_then(|entity| player_model_customizations.get(entity).ok())
            .map_or(true, |customization| customization.cape_shown());
        // `0.0` (and hence a bit-identical `pose_swelling` to `pose`, per that
        // function's own doc) for every non-creeper — `fuse` is `None` — and
        // for a creeper whose fuse has never moved off idle. Vanilla's own
        // per-partial-tick swelling accessor: `lerp(partialTick, oldSwell, swell) /
        // (maxSwell - 2)`, `maxSwell` fixed at 30 client-side (see
        // `CREEPER_MAX_SWELL_TICKS`'s doc).
        let creeper_swelling = fuse.map_or(0.0, |fuse| {
            let old = fuse.old_swell as f32;
            let new = fuse.swell as f32;
            (old + (new - old) * partial_tick) / (CREEPER_MAX_SWELL_TICKS as f32 - 2.0)
        });
        // `Mth.lerp(partialTick, swimAmountO, swimAmount)` — see [`SwimRamp`]
        // for why this is integrated here rather than read off the wire.
        let swim_amount = swim.old + (swim.current - swim.old) * partial_tick;
        // Vanilla's own avatar-renderer cape-state extraction, given this frame's interpolated
        // lagged cloak position (against the *drawn* feet, exactly as
        // vanilla's own `Mth.lerp(partialTicks, entity.xo, entity.getX())`
        // resolves against the same partial tick every other interpolated
        // field here does) and this frame's body yaw. `walk.walk.position_lerp`
        // stands in for vanilla's own client-avatar-state interpolated-walk-distance accessor — a
        // different accumulator in vanilla, but the same shape (a
        // monotonic walk-cycle distance that drives the flap's footstep-synced
        // wobble), and the only one already tracked on this component.
        let cape_lag_pos = Vec3::new(
            cape_lag.cloak_o.x + (cape_lag.cloak.x - cape_lag.cloak_o.x) * partial_tick,
            cape_lag.cloak_o.y + (cape_lag.cloak.y - cape_lag.cloak_o.y) * partial_tick,
            cape_lag.cloak_o.z + (cape_lag.cloak.z - cape_lag.cloak_o.z) * partial_tick,
        );
        let cape_bob = cape_lag.bob_o + (cape_lag.bob - cape_lag.bob_o) * partial_tick;
        let cape_sway_value = cape_sway(
            cape_lag_pos - drawn_feet,
            drawn_yaw,
            cape_bob,
            walk.walk.position_lerp(partial_tick),
        );
        // Bit `0x01` of the shared-flags byte. `false` for an entity that has
        // never reported the byte at all (`EntityFlags` absent, like
        // `HurtTime`/`AttackSwing`) — see `EntityDraw::on_fire`.
        let on_fire = index
            .get(id.0)
            .and_then(|entity| flags.get(entity).ok())
            .is_some_and(|flags| flags.0 & 0x01 != 0);
        // Bit `0x20` of the same shared-flags byte `on_fire` reads bit `0x01`
        // of. `false` for an entity that has never reported the byte, exactly
        // like `on_fire` — see `EntityDraw::invisible`.
        let invisible = index
            .get(id.0)
            .and_then(|entity| flags.get(entity).ok())
            .is_some_and(|flags| flags.0 & 0x20 != 0);
        let custom_name = index
            .get(id.0)
            .and_then(|entity| custom_names.get(entity).ok())
            .and_then(|name| name.0.as_ref())
            .map(Text::to_plain_string);
        let named_cosmetics = named_entity_cosmetics(kind.entity_type, custom_name.as_deref());
        // The sheep's ordinary white/unsheared metadata is a wire default and
        // may therefore be absent. A rainbow-named sheep still owns a wool
        // layer in that state; only an explicitly reported sheared variant
        // suppresses it at the GPU draw boundary.
        let wool = wool.0.or_else(|| {
            named_cosmetics
                .rainbow_wool
                .then_some(SheepWool {
                    color: 0,
                    sheared: false,
                })
        });
        // An armour stand's own client-flags byte, bridged off the ingest
        // entity through `index` exactly as `on_fire`/`invisible` above are.
        // `None` for every entity that is not an `ArmorStand` (the adapter
        // withholds the byte for those) and for one that has never reported
        // it yet — see `EntityDraw::armor_stand`.
        let armor_stand = index
            .get(id.0)
            .and_then(|entity| armor_stands.get(entity).ok())
            .copied();
        // The stand's six part rotations. Gated on the **type**, not on the
        // component: vanilla's own armor-stand armor-model animation setup assigns all six
        // over the humanoid base pass unconditionally, so a stand that has never
        // reported a pose still overwrites the walk cycle — with
        // `ArmorStand`'s own `defineId` defaults, which is what
        // `ArmorStandPose::default()` carries (and it is not the zero pose).
        //
        // Reading the component alone would leave every unposed stand animating
        // as a walking humanoid, which is the whole defect this chain closes:
        // a stand moved by a contraption swings its arms, and `merge_held_items`
        // hangs its item off that same swinging arm.
        let armor_stand_pose = (kind.entity_type == Some(EntityType::ArmorStand)).then(|| {
            index
                .get(id.0)
                .and_then(|entity| armor_stand_poses.get(entity).ok())
                .map_or_else(lodestone_model::ArmorStandPose::default, |pose| pose.0)
        });
        // The imitated block state of a falling block. Bridged off the ingest
        // entity through `index` exactly as `on_fire` above and `hurt` below are,
        // because `lodestone_ecs::ingest::apply_falling_block_state` inserts the
        // component *there* and not on the render entity this query is drawn from.
        // `None` for every entity that is not a falling block, which is the switch
        // the moving-block pass keys on.
        let block_state = index
            .get(id.0)
            .and_then(|entity| falling_blocks.get(entity).ok())
            .map(|state| state.0);
        // An experience orb's XP value, bridged off the ingest entity like
        // `block_state` above. `None` for every entity that is not an orb — the
        // adapter withholds index 8's `INT` for those — which is the switch
        // `prepare_orbs` keys on. An orb whose value has not arrived yet is still
        // drawn, at sprite cell 0; see `EntityDraw::experience_orb_value`.
        let experience_orb_value = if kind.entity_type == Some(EntityType::ExperienceOrb) {
            Some(
                index
                    .get(id.0)
                    .and_then(|entity| orb_values.get(entity).ok())
                    .map_or(0, |value| value.0),
            )
        } else {
            None
        };
        let tnt_fuse = if kind.entity_type == Some(EntityType::Tnt) {
            index
                .get(id.0)
                .and_then(|entity| tnt_fuses.get(entity).ok())
                .map(|fuse| fuse.0 as f32 - partial_tick + 1.0)
        } else {
            None
        };
        // An item frame's in-plane rotation, bridged off the ingest entity like
        // `block_state` and `experience_orb_value` above. `0` — vanilla's own
        // accessor default — for every entity that is not a frame and for a frame
        // that has not reported one, because unlike a block state there is no
        // "absent" case a consumer would draw differently.
        let item_frame_rotation = index
            .get(id.0)
            .and_then(|entity| item_frame_rotations.get(entity).ok())
            .map_or(0, |rotation| rotation.0);
        // A vehicle's rocking state, bridged off the ingest entity the same way.
        // `BoatHurt::REST` — not a zeroed struct — for everything that has never
        // reported one: the direction's identity is `1`, and a `0` there would
        // multiply the whole roll away, which is a still boat rather than a
        // missing one.
        //
        // Interpolated here rather than at the draw site, because vanilla's
        // own abstract-boat-renderer render-state extraction subtracts the partial tick
        // from *both* the clock and the damage (the damage clamped at zero, the
        // clock deliberately not — a negative clock is the renderer's own "not
        // hurt" test).
        let boat_hurt = index
            .get(id.0)
            .and_then(|entity| vehicle_hurts.get(entity).ok())
            .map_or(lodestone_render::entity_anim::BoatHurt::REST, |hurt| {
                lodestone_render::entity_anim::BoatHurt {
                    time: hurt.time as f32 - partial_tick,
                    dir: hurt.dir as f32,
                    damage: (hurt.damage - partial_tick).max(0.0),
                }
            });
        // Which painting is hung, bridged off the ingest entity the same way,
        // and narrowed to a static table name here rather than at the draw
        // site: an unrecognised data-pack variant becomes `None` at this
        // boundary, so nothing downstream has to decide what to draw for one.
        let painting = index
            .get(id.0)
            .and_then(|entity| painting_variants.get(entity).ok())
            .and_then(|variant| {
                lodestone_render::painting::painting_variant_name(&variant.0.to_string())
            });
        // A firework rocket's flags, bridged off the ingest entity the same
        // way. Left as an `Option` rather than defaulted here because the draw
        // site has to distinguish "not a firework" from "a firework at its
        // defaults" — and it does that by the type path, not by this field.
        let firework = index
            .get(id.0)
            .and_then(|entity| firework_flags.get(entity).ok())
            .copied();
        // Who cast this projectile, bridged off the ingest entity the same way.
        // `None` for every entity type the adapter has not established an owner
        // reading for — today everything but a fishing bobber — which is the
        // switch the fishing-line pass keys on.
        let projectile_owner = index
            .get(id.0)
            .and_then(|entity| projectile_owners.get(entity).ok())
            .map(|owner| owner.0);
        // The variant sheet, bridged off the ingest entity's `Variant` exactly as
        // `block_state` and `experience_orb_value` above are, and for their reason:
        // `lodestone_ecs::ingest::apply_entity_metadata` inserts `Variant` *there*,
        // not on the render track this query is drawn from. Bridged rather than
        // narrowed into a fifteenth render component — the same choice
        // `experience_orb_value` documents one block up.
        //
        // `kind.0` and not `model_type_path()`: the variant axis belongs to the
        // *species* corpus entry, and the only case where the two differ is a
        // player's slim rig, which has no variant axis at all.
        //
        // `tamed` is bridged off the ingest entity exactly like `variant` above,
        // and defaults to `false` (the wild sheet) for an entity whose `Tamed`
        // has never been reported — the honest default for anything that is not
        // a wolf/cat/parrot/ocelot, and for one of those that really is wild.
        let tamed = index
            .get(id.0)
            .and_then(|entity| tameds.get(entity).ok())
            .is_some_and(|tamed| tamed.0);
        // A player's built-in identity sheet takes this channel first. The two
        // can never contend — a player has no `Variant`, and nothing with a
        // variant axis carries a `player_skin` — so the `or_else` is an
        // ordering statement rather than a precedence rule, and it keeps one
        // field meaning "the sheet this entity binds instead of its model's".
        //
        // This is what makes `DefaultPlayerSkin`'s hash pick visible at all.
        // Until it existed the pick's `.model` chose the rig and its `.texture`
        // was dropped, so all eighteen identities drew the pack's two plain
        // sheets: every skinless player was Steve or Alex.
        let variant_sheet = player_skin
            .0
            .as_ref()
            .map(|skin| skin.default_sheet)
            .or_else(|| {
                index
                    .get(id.0)
                    .and_then(|entity| variants.get(entity).ok())
                    .and_then(|variant| {
                        lodestone_render::entity_variant_sheet_for(&kind.path, &variant.0, tamed)
                    })
            });
        out.0.push(EntityDraw {
            id: id.0,
            type_path: Arc::clone(&kind.path),
            named_cosmetics,
            variant_sheet,
            // Only item entities use the selected definition on this scoped
            // world-item path. Frames and projectile stacks retain their base
            // ids until their own component-complete render-state work lands.
            item: stack.map(|s| {
                if kind.entity_type == Some(EntityType::Item) {
                    s.render_definition().clone()
                } else {
                    s.id.clone()
                }
            }),
            item_model: stack.and_then(|s| s.item_model.clone()),
            item_skin: stack.and_then(|s| s.skin.clone()),
            count: stack.map_or(1, |s| s.count),
            foil: stack.is_some_and(|s| s.foil),
            item_dyed_color: stack.and_then(|s| s.dyed_color),
            item_potion_color: stack.and_then(|s| s.potion_color),
            equipment: equipment.0.clone(),
            equipment_dye: equipment_dye.0.clone(),
            equipment_skin: equipment_skin.0.clone(),
            equipment_trim: equipment_trim.0.clone(),
            wool,
            block_state,
            item_frame_rotation,
            painting,
            firework,
            projectile_owner,
            feet: drawn_feet,
            yaw: drawn_yaw,
            head_yaw: render_head_yaw(from, to, clock),
            pitch: drawn_pitch,
            scale: scale.0,
            anim: render_anim(
                from,
                to,
                clock,
                walk,
                partial_tick,
                swing_progress,
                arm_pose,
                aggressive,
                crouching,
                is_passenger,
                swim_amount,
                armor_stand_pose,
                boat_hurt,
                cape_visible,
                fall_flying,
                motion,
            ),
            name_tag: name_tag.0.clone(),
            hurt,
            death_time,
            item_use,
            main_arm_left,
            creeper_swelling,
            swim_amount,
            on_fire,
            invisible,
            armor_stand,
            player_skin: player_skin.0.clone(),
            experience_orb_value,
            tnt_fuse,
            cape_sway: cape_sway_value,
        });
    }
}
