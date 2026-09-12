/// Advances the player by exactly one tick of on-land (non-fluid) movement.
///
/// Fluid, ladder, and elytra handling live in dedicated entry points; this is
/// the common walking/sprinting/jumping/falling path that dominates real play.
pub fn tick_air(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    tick_air_among_entities(state, input, view, profile, &[]);
}

fn tick_air_among_entities(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    nearby: &[crate::push::NearbyEntity],
) {
    // Vanilla's own auto-jump spend: a decision made by [`update_auto_jump`]
    // at the end of the *previous* tick's move is spent here as a forced
    // jump, one tick later, via the same synthetic jump-key deferral vanilla
    // uses. The forced flag flows through the ordinary jump block below (and
    // into `AirTravelContext`'s `jumping`, which is exactly what vanilla's
    // own deferral does).
    let mut input = input;
    if state.auto_jump_time > 0 {
        state.auto_jump_time -= 1;
        input.jump = true;
    }

    // --- AI-step prologue: velocity snap-to-zero -------------------------------
    decrement_no_jump_delay(state);
    state.velocity = snap_small_velocity(state.velocity);

    // --- input transformation (client-side) -----------------------------------
    let (xxa, zza) = set_sprint_and_modify_input(state, input, profile);

    // --- jump -----------------------------------------------------------------
    // Vanilla's own jump block is `if (this.jumping &&
    // this.isAffectedByFluids())` — and the player's fluid-affected check is
    // `!abilities.flying`. So a **flying player never jumps from the ground
    // at all**; the `else` arm runs and clears its own no-jump-delay flag.
    //
    // This conjunct is easy to miss because the fluid-affected check reads
    // like it should only guard the *fluid* sub-branches, and because
    // vanilla's own creative-flight feel is unaffected in the common case
    // (you are rarely on the ground while flying). It matters at ground level:
    // without it, holding jump while hovering just above the floor would
    // add the ground-jump impulse's `0.42` *on top of* the flying-speed
    // times 3 vertical impulse the driver applies, and launch the player at
    // three times vanilla's climb rate.
    //
    // The one-shot hop vanilla *does* perform when flight is **engaged**
    // while standing (its own per-tick client update: if flying and on the
    // ground, jump from the ground) is the driver's, not this function's —
    // it fires on the toggle edge, before the rest of its per-tick update.
    let affected_by_fluids = !state.flying;
    if affected_by_fluids && input.jump && state.on_ground && state.no_jump_delay == 0 {
        jump_from_ground(state, view, profile);
        state.no_jump_delay = 10;
    } else if !affected_by_fluids || !input.jump {
        state.no_jump_delay = 0;
    }

    // Vanilla's own on-climbable fall-distance reset — evaluated once,
    // pre-move, and reused (`travel_in_air` below re-derives the same
    // `climbing` test for its own velocity clamp; both read the pre-move
    // position, so the two checks agree). Only vanilla's own airborne
    // travel step reaches this reset, so this is `tick_air`-only, matching
    // vanilla.
    //
    // Vanilla's own "on climbable" check is `abilities.flying ? false :
    // super.onClimbable()`, so flight detaches the player from ladders
    // entirely — both this reset and `travel_in_air`'s clamp/steady-climb.
    if on_climbable(state, view) {
        state.fall_distance = 0.0;
    }

    // --- travelInAir ----------------------------------------------------------
    // The gravity + drag + collision core is the entity-agnostic `travel_in_air`
    // seam (shared with mobs); the player supplies only the transformed input,
    // its own effective speed, and its per-situation flags. Thread the
    // player's motion state through `EntityMotion` and back so the
    // arithmetic is byte-identical.
    let old_x = state.position.x;
    let old_y = state.position.y;
    let old_z = state.position.z;
    let mut motion = EntityMotion {
        position: state.position,
        velocity: state.velocity,
        on_ground: state.on_ground,
        horizontal_collision: state.horizontal_collision,
        stuck_speed_multiplier: state.stuck_speed_multiplier,
    };
    let ctx = AirTravelContext {
        yaw: state.yaw,
        jumping: input.jump,
        levitation: state.effects.levitation,
        slow_falling: state.effects.slow_falling,
        suppress_ladder_slide: input.sneak,
        suppress_bounce: input.sneak,
        omnidirectional_air_mover: false,
        discard_friction: false,
        // Vanilla's own sneak-at-a-ledge back-off — a player always has the
        // override; the shift key and the fall distance are what decide
        // whether it does anything.
        //
        // …except while flying: the override's own first conjunct is not
        // flying, so a flying player gets the *base* implementation, which
        // is the identity. This is the conjunct `EdgeBackOff`'s doc records
        // as "satisfied by construction here" because this crate had no
        // flight — that argument has now expired, so the gate is real
        // rather than vacuous, and a sneaking player can fly off a ledge.
        edge_back_off: if state.flying {
            EdgeBackOff::Entity
        } else {
            EdgeBackOff::Player {
                staying_on_ground_surface: input.sneak,
                fall_distance: state.fall_distance,
            }
        },
        // Vanilla's own flying-speed accessor — sprint- and flight-dependent,
        // so it cannot be a profile constant. This is also what fixes the
        // non-flying sprint-jump case, which was reading a flat `0.02`.
        flying_speed: Some(player_flying_speed(state, profile)),
        // Vanilla's own block-speed-factor — suppressed while flying **or**
        // gliding.
        suppress_block_speed_factor: state.flying || state.fall_flying,
        // Vanilla's own "on climbable" check.
        suppress_climbable: state.flying,
    };
    travel_in_air_among_entities(
        &mut motion,
        state.dimensions(),
        (xxa, zza),
        effective_speed(profile, state),
        ctx,
        view,
        profile,
        nearby,
    );
    state.position = motion.position;
    state.velocity = motion.velocity;
    state.on_ground = motion.on_ground;
    state.horizontal_collision = motion.horizontal_collision;
    state.stuck_speed_multiplier = motion.stuck_speed_multiplier;

    // Vanilla's own fall-damage bookkeeping call, made from inside its move
    // step — not in water on this path (see `accumulate_fall_distance`'s
    // doc).
    accumulate_fall_distance(state, state.position.y - old_y, false);

    // Vanilla's own client move step calls its auto-jump detector after the
    // base move completes — the detector sees the *actual* post-collision
    // delta (cast to `float`), not the pre-collision intent, and arms the
    // one-tick-deferred jump spent at the top of the next tick. This is a
    // client-only steering effect: the server only ever sees the resulting
    // jump, whose velocity the crate already reproduces bit-exactly.
    update_auto_jump(
        state,
        input,
        view,
        profile,
        (state.position.x - old_x) as f32,
        (state.position.z - old_z) as f32,
    );
}

/// Vanilla's own pre-move clamp applied while on a ladder/vine.
///
/// The clamp bounds are the **`float`** literals `-0.15F`/`0.15F`, promoted
/// to `double` for the clamp. `(double)0.15F` is `0.15000000596046448`,
/// *not* `0.15`, so the widened bound is observable at the last ULP — we
/// widen through `f32` exactly like vanilla rather than writing `0.15_f64`.
/// The sneak-hold (`yd = 0` when descending) applies to ladders/vines but
/// not scaffolding.
pub(crate) fn handle_on_climbable(delta: Vec3d, sneaking: bool) -> Vec3d {
    let bound = f64::from(0.15f32);
    let xd = mth::clamp_f64(delta.x, -bound, bound);
    let zd = mth::clamp_f64(delta.z, -bound, bound);
    let mut yd = delta.y.max(-bound);
    if yd < 0.0 && sneaking {
        yd = 0.0;
    }
    Vec3d::new(xd, yd, zd)
}

/// Vanilla's own effective-gravity check — Slow Falling reduces gravity to
/// `min(gravity, 0.01)` while descending; otherwise it is the base gravity.
///
/// The `0.01` is a `double` literal and `min` uses the pre-move delta-Y sign
/// (velocity Y `<= 0.0`). In fluids this is what makes the `-0.003` clamp
/// reachable: it shifts `baseGravity/16` off the `0.005` that makes the
/// clamp dead at default gravity.
#[must_use]
pub(crate) fn effective_gravity(base_gravity: f64, falling: bool, slow_falling: bool) -> f64 {
    if falling && slow_falling {
        base_gravity.min(0.01)
    } else {
        base_gravity
    }
}

/// Vanilla's own fluid-falling adjusted-movement step.
///
/// Applies the buoyant slow-descent: normally `y - baseGravity/16`, but when
/// already sinking near terminal it clamps to `-0.003` (the famous slow-sink).
/// When sprinting, gravity is not applied at all (vanilla returns `movement`).
#[must_use]
fn fluid_falling_adjusted_movement(
    base_gravity: f64,
    is_falling: bool,
    sprinting: bool,
    movement: Vec3d,
) -> Vec3d {
    if base_gravity != 0.0 && !sprinting {
        let gravity_step = base_gravity / 16.0;
        let yd = if is_falling
            && (movement.y - 0.005).abs() >= 0.003
            && (movement.y - gravity_step).abs() < 0.003
        {
            -0.003
        } else {
            movement.y - gravity_step
        };
        Vec3d::new(movement.x, yd, movement.z)
    } else {
        movement
    }
}

/// Vanilla's own fluid-jump threshold — `getEyeHeight() < 0.4 ? 0.0 : 0.4`.
///
/// The pose feeds back into movement here: the swimming pose's eye height is
/// **exactly** `0.4`, and `0.4 < 0.4` is false, so a swimming player keeps
/// the `0.4` threshold. Only a pose shorter than that (no vanilla player
/// pose is) collapses the threshold to zero.
#[must_use]
fn fluid_jump_threshold(eye_height: f32) -> f64 {
    if eye_height < 0.4 { 0.0 } else { 0.4 }
}

/// Vanilla's own "on climbable" check, reduced to the block test this
/// engine models: the CLIMBABLE tag on the block at the feet block
/// position. Vanilla's own player override is `abilities.flying ? false :
/// super.onClimbable()` — flight detaches the player from ladders and
/// vines.
fn on_climbable(state: &PlayerState, view: &dyn CollisionView) -> bool {
    !state.flying && on_climbable_here(state, view)
}

#[allow(clippy::doc_markdown)]
fn on_climbable_here(state: &PlayerState, view: &dyn CollisionView) -> bool {
    view.is_climbable(
        mth::floor(state.position.x),
        mth::floor(state.position.y),
        mth::floor(state.position.z),
    )
}

/// Vanilla's own fall-damage bookkeeping, restricted to the accumulation
/// and the grounded reset — the living-entity override adds only landing
/// particles and a server/on-changed-block call before delegating to this
/// via the base implementation, neither of which affects position or the
/// fall-distance accumulator itself.
///
/// `ya` is the actual Y position delta vanilla's own move step achieved,
/// *not* the pre-move velocity. Callers pass `state.position.y - old_y`,
/// captured immediately before the move.
///
/// `in_water` is vanilla's own "in water" flag — callers pass a constant
/// matching which travel path they are in (only [`tick_water`] can have it
/// `true`; the dispatch in [`travel_and_check_inside_blocks`] guarantees the
/// other three paths are only reached when it is `false`).
///
/// **That constant is an approximation, and this is the subsystem's one
/// known divergence.** Vanilla's "in water" check is *not* frozen for the
/// tick: it reads a cached flag, and its own fluid-interaction update
/// rewrites that cache from **two** call sites — its baseline per-tick
/// update (pre-travel, the one this crate's dispatch reproduces) and its
/// fall-damage check, which runs *inside* the move step against the
/// **post-move** position, only while not already in water. So on the tick
/// a fall first enters water vanilla resets mid-move and then skips the
/// accumulation below, ending that tick at exactly `0.0`, whereas this crate
/// is still on the `tick_air` path and accumulates the descent.
///
/// The divergence is bounded to that single tick and **cannot move the player**:
/// this call happens at the *end* of the move, after vanilla's own
/// sneak-at-a-ledge back-off has already read the old value, and the next
/// tick's dispatch re-derives the summary from the same post-move position
/// vanilla used, so [`tick_water`]'s reset lands before any gate reads it. It is
/// therefore observable only to an external reader between ticks (a future
/// fall-damage predictor). Closing it would cost a second
/// [`crate::fluid_state::compute_fluid_state`] on every air tick.
/// `tests/fall_distance.rs`'s
/// `water_entry_tick_is_the_one_known_divergence_and_it_lasts_exactly_one_tick`
/// pins both halves of that claim.
///
/// The `(float)` cast is vanilla's, not an approximation: the fall-distance
/// accumulator is a `double` field but the tick's `ya` is truncated to
/// `float` precision *before* the subtraction.
fn accumulate_fall_distance(state: &mut PlayerState, ya: f64, in_water: bool) {
    if !in_water && ya < 0.0 {
        state.fall_distance -= f64::from(ya as f32);
    }
    if state.on_ground {
        state.fall_distance = 0.0;
    }
}

/// Vanilla's own fall-distance-accumulation clamp — clamps the accumulator
/// to at most `1.0` while not descending fast. Called only from vanilla's
/// own fall-flying update, itself only reached while gliding, ahead of the
/// Slow Falling/Levitation reset and travel — so this reads
/// `state.velocity` as it stood at the *end of the previous* tick, exactly
/// as vanilla's pre-travel placement does.
fn check_fall_distance_accumulation(state: &mut PlayerState) {
    if state.velocity.y > -0.5 && state.fall_distance > 1.0 {
        state.fall_distance = 1.0;
    }
}

/// Vanilla's own **jump** block for an entity standing in fluid.
///
/// This is the sinking-vs-swimming decision, and it is *not* "jump means `+0.04`
/// in water". Vanilla compares the fluid's **height** against
/// [`fluid_jump_threshold`]:
///
/// * shallow enough (`onGround && !(height > threshold)`, or not in water at all)
///   ⇒ an ordinary ground jump — you jump out of a puddle normally;
/// * otherwise ⇒ vanilla's own in-liquid jump, the `+0.04F` swim-up impulse.
///
/// Modelling this needs a real fluid height, which is why the summary is passed
/// in rather than re-derived from the coarse presence booleans: with a
/// [`CollisionView::fluid_at`]-capable world the height is exact, and without one
/// a present cell reads as full (`1.0`), which is above the `0.4` threshold and so
/// lands on the swim-up branch — the pre-existing behaviour.
fn apply_fluid_jump(
    state: &mut PlayerState,
    input: MovementInput,
    fluid: &FluidState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    if !input.jump {
        state.no_jump_delay = 0;
        return;
    }
    // `isInLava() ? getFluidHeight(LAVA) : getFluidHeight(WATER)`.
    let in_lava = fluid.in_lava();
    let fluid_height = if in_lava {
        fluid.lava_height
    } else {
        fluid.water_height
    };
    let in_water_and_has_height = fluid.in_water() && fluid_height > 0.0;
    // Vanilla's own eye height is *always* derived from the pose's own
    // dimensions — one record, no way for the two to disagree. Read it from
    // the pose rather than from [`PlayerState::eye_height`] so an
    // out-of-band write to that field cannot make the box and the eye
    // disagree here either.
    let threshold = fluid_jump_threshold(state.pose.eye_height());
    // The outer test is vanilla's `!(fluidHeight > threshold)` and the two inner
    // ones are its `<=` — transcribed as written rather than normalised, because the
    // two forms differ on NaN and the source is the specification here.
    #[allow(clippy::neg_cmp_op_on_partial_ord)]
    let not_above_threshold = !(fluid_height > threshold);
    // `isInShallowFluid(LAVA)`.
    let shallow_lava = fluid.lava_height <= threshold;
    let jump_in_liquid = |state: &mut PlayerState| {
        state.velocity = state.velocity.add(Vec3d::new(0.0, f64::from(0.04f32), 0.0));
    };

    if !in_water_and_has_height || (state.on_ground && not_above_threshold) {
        if in_lava && !(state.on_ground && shallow_lava) {
            jump_in_liquid(state);
        } else if (state.on_ground || (in_water_and_has_height && fluid_height <= threshold))
            && state.no_jump_delay == 0
        {
            jump_from_ground(state, view, profile);
            state.no_jump_delay = 10;
        }
    } else {
        jump_in_liquid(state);
    }
}

/// Vanilla's own jump-out-of-fluid hop — the hop that carries a swimmer
/// *out* of the water onto the ledge they just swam into.
///
/// Runs at the end of both fluid travel branches. When the tick's move collided
/// horizontally and the box would be **free of blocks and of liquid** if lifted to
/// `movement.y + 0.6 - y + oldY`, vertical velocity is replaced by a flat `0.3F`.
/// Without it a player pressed against a shoreline swims into the wall forever;
/// this is the single most visible piece of water movement after buoyancy.
///
/// Vanilla's own "is free" check is its own "no collision and contains no
/// liquid" check — the liquid half is what stops it firing repeatedly while
/// still submerged.
fn jump_out_of_fluid(
    state: &mut PlayerState,
    old_y: f64,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    if !state.horizontal_collision {
        return;
    }
    let movement = state.velocity;
    let lift = movement.y + f64::from(0.6f32) - state.position.y + old_y;
    let probe = state
        .bounding_box(profile)
        .moved(movement.x, lift, movement.z);
    let free = crate::collision::no_collision(view, probe)
        && !crate::collision::contains_any_liquid(view, probe);
    if free {
        state.velocity = Vec3d::new(movement.x, f64::from(0.3f32), movement.z);
    }
}
