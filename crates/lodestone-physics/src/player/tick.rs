/// Advances the player one tick: dispatches to the fluid/elytra/air travel
/// path exactly as vanilla's own travel dispatch, records any stuck-in-block
/// multiplier for the next tick to consume (vanilla's own "check inside
/// blocks" sweep), and finally re-decides the **pose** through vanilla's
/// fit gate ([`crate::pose::update_player_pose`]).
///
/// The pose runs last because vanilla's own pose update is the last
/// statement of its own per-tick player update, after the base per-tick
/// update has done all the moving. So this tick's movement used the pose
/// decided at the end of the *previous* tick, and the fit gate probes the
/// post-move position.
pub fn tick(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    travel_and_check_inside_blocks(state, input, view, profile, &[]);
    // Vanilla's own pose update with no entity snapshot: the block half of
    // the fit gate. See `tick_among_entities` for the full predicate.
    update_player_pose(state, input, view, &[]);
}

/// Everything vanilla's own base per-tick update does to a player's motion
/// — its fluid/swim summary, then travel — up to but excluding the pose
/// decision.
///
/// Split out so [`tick`] and [`tick_among_entities`] can share it while
/// keeping vanilla's ordering: the crowd-push pass is the end of its
/// per-tick update, *inside* the base per-tick update, and therefore
/// **before** the pose update. That order is observable, because the
/// push's pair test reads vanilla's own bounding-box accessor — which the pose sizes.
fn travel_and_check_inside_blocks(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    nearby: &[crate::push::NearbyEntity],
) {
    // The position *before* this tick's travel dispatch moves it — the "from"
    // half of vanilla's `checkInsideBlocks(from, to, …)` segment, needed by
    // [`update_stuck_multiplier`] and [`update_freezing`] to sweep the tick's
    // movement instead of sampling only where it ends. Captured here, ahead of
    // every branch below, because every one of them can write `state.position`.
    let pre_move_position = state.position;

    // Vanilla's own per-tick base update computes the fluid summary from the
    // *pre-move* box, before travel reads its own in-water/in-lava flags. Do the
    // same: one source of truth for
    // eye/box submersion, recorded on the state for the swimming pose and for the
    // shell's fog / overlay / ambient-sound consumers.
    //
    // **Both the box and the eye come from the pose**, never from
    // [`PlayerState::eye_height`]. They are one `EntityDimensions` record in
    // vanilla and cannot disagree; deriving both here means an out-of-band write to
    // that field cannot desynchronise them either. `tests/pose_dimensions.rs`
    // measures what the disagreement would cost: a `0.6` box with a `1.62` eye
    // reports dry eyes twenty blocks under water, because this sweep is bounded by
    // the box and never visits the eye's cell.
    let fluid = compute_fluid_state(
        state.bounding_box(profile),
        state.position,
        state.pose.eye_height(),
        view,
    );
    state.eye_in_water = fluid.eye_in_water;
    state.eye_in_lava = fluid.eye_in_lava;
    // Vanilla's own player-swim-state override forces `setSwimming(false)`
    // while flying instead of delegating to the base swim-state update, so a
    // player
    // who dives, starts sprint-swimming and then takes off drops the swim pose on
    // the very next tick rather than flying around with a `0.4` eye height.
    state.swimming = !state.flying
        && update_swimming(
            state.swimming,
            state.sprinting,
            &fluid,
            view,
            state.position,
        );
    // Vanilla's own swim-amount update — see its doc for why this sits here,
    // between the swim-state update and the travel dispatch below.
    update_swim_amount(state);

    // Vanilla's own per-tick update calls its own update-fall-flying step
    // whenever fall-flying is active, which is
    // `check_fall_distance_accumulation`'s only call site for a player.
    // Runs before the Slow Falling/Levitation check and before the travel
    // step, on the velocity as it stood at the end of the *previous* tick —
    // see `check_fall_distance_accumulation`'s doc.
    if state.fall_flying {
        check_fall_distance_accumulation(state);
    }
    // Vanilla's own per-tick update resets the fall-distance accumulator
    // whenever Slow Falling or Levitation is active, unconditionally before
    // the travel dispatch below, regardless of which path it picks.
    if state.effects.slow_falling || state.effects.levitation.is_some() {
        state.fall_distance = 0.0;
    }

    // Vanilla's own per-tick player update resets the fall-distance
    // accumulator while flying and not a passenger — **before** the rest of
    // its per-tick update, so before the travel dispatch below, alongside
    // the Slow Falling reset rather than after the move.
    if state.flying {
        state.fall_distance = 0.0;
    }

    // Vanilla's own player travel step captures `getDeltaMovement().y`
    // *before* delegating to its base travel and then **overwrites** the
    // post-travel Y with `originalMovementY * 0.6`.
    //
    // # Why the capture reads through `snap_small_velocity`
    //
    // Vanilla's read happens inside its own travel step, which its per-tick
    // update calls *after* its own velocity snap-to-zero prologue — and that
    // prologue lives inside our per-path `tick_air`/`tick_elytra`, not out
    // here. Applying the snap non-destructively to derive the capture is
    // **exactly** vanilla's value, because the only other thing the per-tick
    // update writes to velocity between the snap and travel is the ground
    // jump, and that whole block is gated on vanilla's own fluid-affected
    // check — `!flying` for a player. So while flying nothing else can
    // intervene, and `flying` is the only case this value is read in.
    //
    // Getting it wrong here is quiet rather than loud: capturing *before* the snap
    // leaves a flying player at rest with a residual Y of `1.8e-3 * 0.6^n` that
    // decays geometrically but never reaches zero, is invisible in position
    // (the snap re-zeroes it every tick before the move), and then shows up as a
    // ~0.001 offset the first time they press jump. `tests/travel_seam.rs`'s
    // `flight_y_overwrite_reads_the_post_snap_velocity` pins it.
    let original_movement_y = state.flying.then(|| snap_small_velocity(state.velocity).y);

    // Vanilla's own travel dispatch, with its own fluid-travel gate's
    // fluid-affected conjunct — which the player overrides to `!flying`.
    //
    // **This suppressor is what makes flight a wrapper rather than a fourth
    // travel mode**, and it is the single strongest discriminator between the two
    // shapes: a player flying through water takes vanilla's own airborne
    // travel step, so they keep moving at flight speed with no `0.8` water
    // slow-down, no buoyancy and no sneak-to-sink. Note it does **not** gate
    // the elytra arm: vanilla's own "can glide" check is "not flying and the
    // base can-glide check", which stops a flying player *starting* a glide,
    // but the fall-flying flag is a server-owned entity-data bit and
    // vanilla's dispatch still honours it if both are somehow set.
    let affected_by_fluids = !state.flying;
    if affected_by_fluids && fluid.in_water() {
        tick_water_among_entities(state, input, &fluid, view, profile, nearby);
    } else if affected_by_fluids && fluid.in_lava() {
        tick_lava_among_entities(state, input, &fluid, view, profile, nearby);
    } else if state.fall_flying {
        tick_elytra_among_entities(state, input, view, profile, nearby);
    } else {
        tick_air_among_entities(state, input, view, profile, nearby);
    }
    // The Y overwrite, closing vanilla's own player travel step. `with(Direction.Axis.Y, …)`
    // — the X and Z the dispatch produced are kept untouched. There is **no**
    // horizontal drag term in this branch, however much "0.6" looks like one.
    if let Some(y) = original_movement_y {
        state.velocity.y = y * 0.6;
    }
    // Vanilla's own per-tick "apply effects from blocks" pass, immediately
    // after the `travel()` dispatch above and before the crowd-push pass.
    // Both calls below are per-block "entity inside" effects reached from
    // that one sweep, which is why they sit together here.
    //
    // Their order is unobservable: vanilla visits each cell once and a cell is
    // either a bubble column or a stuck-in-block, never both, so the two never see
    // the same cell. Beyond that they touch disjoint state — `apply_bubble_column`
    // writes `velocity.y` and `update_stuck_multiplier` writes
    // `stuck_speed_multiplier`, neither reads what the other writes, and the only
    // field they share is `fall_distance`, which both only ever set to `0.0`.
    //
    // Both are `!flying` for a player: vanilla's own player overrides for
    // both the above/inside bubble-column handlers and its own
    // "make stuck in block" override skip the base behaviour entirely while
    // flying. So a flying player is neither lifted by a soul-sand column nor
    // grabbed by a cobweb.
    if !state.flying {
        apply_bubble_column(state, view, profile);
        update_stuck_multiplier(state, view, profile, pre_move_position);
    }
    // Vanilla's own per-tick freezing block, run unconditionally — **not**
    // inside the `!flying` gate above. Vanilla's own "make stuck in block"
    // player override is what suppresses the two calls above while flying;
    // nothing overrides the powder-snow freeze effect, which applies to
    // every living entity standing in the block regardless of
    // `abilities.flying`. A creative-flying player drifting through powder
    // snow still accumulates `frozen_ticks`, even with no stuck-multiplier
    // drag.
    update_freezing(state, view, profile, pre_move_position);

    // Vanilla's own per-tick "push" block: `if (autoSpinAttackTicks > 0)
    // autoSpinAttackTicks--;`, unconditional — no `!flying` gate in vanilla
    // either, so a riptide launch that carries a player into creative
    // flight still counts down normally. The entity-hit sweep that runs
    // alongside it is not modelled — see
    // [`PlayerState::auto_spin_attack_ticks`]'s scope note.
    if state.auto_spin_attack_ticks > 0 {
        state.auto_spin_attack_ticks -= 1;
    }
}

/// [`tick`] followed by one pass of vanilla's own crowd-push pass against
/// `nearby`.
///
/// This is the whole of vanilla's own per-tick ordering for entity
/// interaction: travel first, the crowd push last. So the impulse a
/// neighbour delivers this tick is integrated on the **next** one, and
/// this tick's collision sweep never sees it.
///
/// Passing an empty `nearby` is bit-for-bit [`tick`]: movement takes the ordinary
/// block-only entry point and [`crate::push::apply_entity_push`] returns
/// immediately. A mixed snapshot may carry both hard colliders (boats, shulkers,
/// eligible happy ghasts) and living crowd-push producers; the two independent
/// flags on [`crate::push::NearbyEntity`] keep those mechanisms separate.
pub fn tick_among_entities(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    nearby: &[crate::push::NearbyEntity],
    self_flags: crate::push::PushSelf,
) {
    crate::trace::player_tick_start(state, input);
    travel_and_check_inside_blocks(state, input, view, profile, nearby);
    crate::push::apply_entity_push(state, view, profile, nearby, self_flags);
    // The pose comes *after* the push, because the crowd-push pass is the
    // tail of vanilla's own per-tick update (inside its base per-tick
    // update) and the pose update is the tail of its own player-tick
    // update. `nearby` also supplies the entity term of the fit gate —
    // vacuous unless one of them is a boat, a shulker or a happy ghast.
    update_player_pose(state, input, view, nearby);
    crate::trace::player_tick_end(state);
}

/// Vanilla's own swim-state update — the sprint-swimming pose state machine.
///
/// Entering requires being **under water** (eye submerged) *and* the block at the
/// feet holding water; once swimming, it is sustained merely by sprinting while
/// **in** water (box touching water), so you keep swimming as you break the
/// surface. Passenger/vehicle state is not modelled here (this engine has none),
/// matching the `!isPassenger()` guard being vacuously true.
///
/// Vanilla's own player override adds one override: a *flying* player is
/// never swimming. This engine has no flight, so a driver with a
/// free-fly/creative-flight mode must clear [`PlayerState::swimming`] itself while
/// flying rather than relying on this function — it is only reached from [`tick`],
/// which a flying driver does not call.
fn update_swimming(
    swimming: bool,
    sprinting: bool,
    fluid: &FluidState,
    view: &dyn CollisionView,
    position: Vec3d,
) -> bool {
    if swimming {
        sprinting && fluid.in_water()
    } else {
        sprinting && fluid.under_water() && water_at_block(view, position)
    }
}

/// Vanilla's own per-block fluid-state check for water — water at the
/// entity's own block position (`floor` of each coordinate), fine-then-coarse
/// like the rest of the fluid state.
fn water_at_block(view: &dyn CollisionView, position: Vec3d) -> bool {
    let bx = mth::floor(position.x);
    let by = mth::floor(position.y);
    let bz = mth::floor(position.z);
    match view.fluid_at(bx, by, bz) {
        Some(cell) => cell.kind == crate::fluid::FluidKind::Water,
        None => view.is_water(bx, by, bz),
    }
}

/// Vanilla's own swim-amount update — advances the `0..1` swim-pose ramp by
/// a fixed per-tick step (`0.09F`) toward `1.0` while `swimming`, or back
/// toward `0.0` otherwise, clamping at both ends.
///
/// Called immediately after [`update_swimming`] decides this tick's swim flag,
/// mirroring vanilla's exact call order: its own per-tick update calls its
/// swim-amount update right after the base per-tick update (where the
/// swim-state update lives) and *before* the rest of its per-tick update
/// (where `travel` — and therefore the look-descent in [`tick_water`] —
/// runs). So this ramp always reflects `swimming` as of the **start** of
/// the current tick, one step ahead of the travel branch that consumes
/// `swimming` directly.
fn update_swim_amount(state: &mut PlayerState) {
    const SWIM_AMOUNT_PER_TICK: f32 = 0.09;
    state.swim_amount_o = state.swim_amount;
    state.swim_amount = if state.swimming {
        (state.swim_amount + SWIM_AMOUNT_PER_TICK).min(1.0)
    } else {
        (state.swim_amount - SWIM_AMOUNT_PER_TICK).max(0.0)
    };
}

/// The player's effective walk speed for vanilla's own friction-influenced
/// speed step, i.e. vanilla's own speed accessor. Uses the injected
/// attribute value when present
/// (sprint + Speed/Slowness already folded in by the entity layer), reproducing
/// the `(float)` cast; otherwise computes the standalone base+sprint value.
fn effective_speed(profile: &PhysicsProfile, state: &PlayerState) -> f32 {
    match state.movement_speed {
        Some(v) => v as f32,
        None => player_speed(profile, state.sprinting),
    }
}

/// Player-shaped wrapper over [`friction_influenced_speed_value`] retained for
/// the speed unit tests, which assert against a `PlayerState`.
#[cfg(test)]
fn friction_influenced_speed(
    profile: &PhysicsProfile,
    state: &PlayerState,
    block_friction: f32,
) -> f32 {
    friction_influenced_speed_value(
        effective_speed(profile, state),
        block_friction,
        state.on_ground,
        player_flying_speed(state, profile),
        profile,
    )
}

/// Vanilla's own friction-influenced speed step, entity-agnostic core.
/// `speed` is the caller's own speed accessor — the player's movement-speed
/// attribute or a mob's AI-supplied speed. On the ground the
/// `0.21600002F / friction^3` factor rescales it (all in `float`); airborne
/// it is discarded entirely for `flying_speed`, the caller's already-resolved
/// flying speed. `speed` therefore only reaches the result on the ground,
/// exactly as vanilla.
///
/// **`flying_speed` is a parameter, not `profile.flying_speed`, because
/// vanilla's own flying-speed accessor is virtual and the player override is
/// stateful** — it depends on the flying ability flag, its own flying-speed
/// value and vanilla's own "is sprinting" check. Reading a profile constant
/// here silently implemented
/// only the "not flying, not sprinting" corner of it; see
/// [`player_flying_speed`] and `PhysicsProfile::airborne_sprint_speed`.
#[must_use]
pub(crate) fn friction_influenced_speed_value(
    speed: f32,
    block_friction: f32,
    on_ground: bool,
    flying_speed: f32,
    profile: &PhysicsProfile,
) -> f32 {
    if on_ground {
        if block_friction > 0.6 {
            let cubed = block_friction * block_friction * block_friction;
            speed * (profile.ground_accel / cubed)
        } else {
            speed
        }
    } else {
        flying_speed
    }
}

/// Vanilla's own flying-speed accessor — the airborne input speed vanilla's
/// own friction-influenced speed step substitutes for its own speed
/// accessor.
///
/// ```text
/// if (this.abilities.flying && !this.isPassenger()) {
///    return this.isSprinting() ? this.abilities.getFlyingSpeed() * 2.0F : this.abilities.getFlyingSpeed();
/// } else {
///    return this.isSprinting() ? 0.025999999F : 0.02F;
/// }
/// ```
///
/// **Four values, not one.** Both arms are sprint-dependent, which is the part
/// that was missing here entirely: the airborne branch used to return a flat
/// `0.02F`, so a sprint-**jump** — every sprint-jump, with no creative flight
/// anywhere in sight — accelerated 30% short of vanilla. The literal is
/// `0.025999999F` and not `0.026F`; see
/// [`PhysicsProfile::airborne_sprint_speed`].
///
/// `!isPassenger()` is vacuously true: this engine has no riding state (the same
/// standing argument [`PlayerState::on_ground`] makes). A driver that adds
/// vehicles must suppress flight for a passenger itself — vanilla falls to the
/// *non*-flying arm there, it does not merely skip the doubling.
#[must_use]
pub fn player_flying_speed(state: &PlayerState, profile: &PhysicsProfile) -> f32 {
    if state.flying {
        if state.sprinting {
            state.flying_speed * 2.0
        } else {
            state.flying_speed
        }
    } else if state.sprinting {
        profile.airborne_sprint_speed
    } else {
        profile.flying_speed
    }
}
