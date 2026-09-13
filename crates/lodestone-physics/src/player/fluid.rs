/// One tick of in-water movement (vanilla's own travel → in-fluid travel →
/// in-water travel dispatch), plus the in-water parts of its per-tick
/// player update that precede it.
///
/// In order: the baseline per-tick flow-current push, vanilla's own
/// sneak-to-sink (`-0.04F`), the velocity snap-to-zero prologue, the
/// shallow-vs-deep jump decision ([`apply_fluid_jump`]), vanilla's own swim
/// look-descent (blending vertical velocity toward the look angle), the
/// slow-down/input-speed terms (sprint, Depth Strider, Dolphin's Grace), the
/// collision move, the ladder clamp, the `multiply(slowDown, 0.8F, slowDown)`
/// drag, buoyancy ([`fluid_falling_adjusted_movement`]) and finally
/// [`jump_out_of_fluid`].
///
/// # Not modelled
///
/// * Bubble-column impulses are **not** here, and never were vanilla's to
///   put here: vanilla's own bubble-column block effect is reached from its
///   per-tick block-effects pass, which its per-tick player update calls
///   *after* travel. They live one level up, in [`apply_bubble_column`],
///   beside the other block-inside effect this crate models
///   ([`update_stuck_multiplier`]).
/// * The Depth Strider (water-movement-efficiency) attribute has no source
///   in this repo — see [`PlayerState::water_movement_efficiency`]. The
///   arithmetic is here; the value is `0.0`.
pub fn tick_water(
    state: &mut PlayerState,
    input: MovementInput,
    fluid: &FluidState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    tick_water_among_entities(state, input, fluid, view, profile, &[]);
}

fn tick_water_among_entities(
    state: &mut PlayerState,
    input: MovementInput,
    fluid: &FluidState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    nearby: &[crate::push::NearbyEntity],
) {
    match profile.fluid_model {
        FluidModel::Modern => {}
        // Structural seam: 1.8 fluid handling is a different branch (no swimming
        // pose, no falling-adjusted clamp). Not modelled yet — fail loudly rather
        // than run modern water math for a 1.8 profile.
        FluidModel::Legacy1_8 => {
            unimplemented!("1.8 fluid movement is not implemented yet")
        }
    }
    // --- base tick: vanilla's own fluid-interaction water reset ----------------
    // This function is only reached when the per-tick fluid summary already says
    // `in_water()` (see `travel_and_check_inside_blocks`'s dispatch), so the
    // condition is unconditionally true here — matching vanilla, where the same
    // predicate decides both the reset and the in-fluid travel dispatch.
    state.fall_distance = 0.0;
    // --- base tick: vanilla's own fluid current push ---------------------------
    // Vanilla applies the flow current in its own base tick, before its own
    // AI step / travel step within the same tick, so it lands here (ahead of the snap-to-zero prologue)
    // and its result is what the prologue and the accel step then see.
    apply_fluid_push(
        state,
        view,
        crate::fluid::FluidKind::Water,
        profile.water_push_scale,
        profile,
    );
    // --- vanilla's own sneak-to-sink impulse (-0.04F) --------------------------
    // Vanilla's own client-side per-tick update runs this *before* the rest
    // of its per-tick update, so it lands ahead of the snap-to-zero prologue
    // below. (The placement is numerically inert at this magnitude — `0.04`
    // clears the `0.003` collapse from either side — but it is where vanilla
    // puts it, and a smaller future sink impulse would not be inert.) This
    // is the deliberate-sink half of "sinking versus swimming": without it
    // the only way down is to release jump and wait for buoyancy.
    if input.sneak {
        state.velocity = state
            .velocity
            .add(Vec3d::new(0.0, -f64::from(0.04f32), 0.0));
    }
    // --- AI-step prologue: velocity snap-to-zero (identical to the air path) ----
    decrement_no_jump_delay(state);
    state.velocity = snap_small_velocity(state.velocity);

    let (xxa, zza) = set_sprint_and_modify_input(state, input, profile);

    // --- AI-step jump: shallow water jumps, deep water swims up -----------------
    apply_fluid_jump(state, input, fluid, view, profile);

    // --- vanilla's own swim look-descent ---------------------------------------
    // Runs in vanilla's own player travel step, which wraps its base travel
    // (travel → in-fluid travel → in-water travel, i.e. the rest of this
    // function) — so it modifies its own velocity's Y component *before* the
    // in-water physics ever sees it, which is why this sits ahead of the
    // is-falling flag / old-Y capture below rather than after it.
    //
    // While swimming, blend vertical velocity toward the look direction's Y
    // component (steeper multiplier `0.085` looking notably down, `< -0.2`;
    // `0.06` otherwise) whenever looking level-or-down, holding jump, or still
    // submerged at head height — so releasing jump and looking up lets a
    // swimmer coast to the surface instead of being pulled back down, but
    // looking down (or still being underwater) glides them lower. Both
    // constants are read directly from the jar, not from any secondhand
    // recollection of them.
    if state.swimming {
        let look_angle_y = calculate_view_vector(state.pitch, state.yaw).y;
        let multiplier = if look_angle_y < -0.2 { 0.085 } else { 0.06 };
        // The block roughly at head height, floored to a block position;
        // "any fluid present" is the check.
        let head_submerged = view
            .fluid_at(
                mth::floor(state.position.x),
                mth::floor(state.position.y + 1.0 - 0.1),
                mth::floor(state.position.z),
            )
            .is_some();
        if look_angle_y <= 0.0 || input.jump || head_submerged {
            let vy = state.velocity.y;
            state.velocity = Vec3d::new(
                state.velocity.x,
                vy + (look_angle_y - vy) * multiplier,
                state.velocity.z,
            );
        }
    }

    // --- vanilla's own in-fluid / in-water travel ------------------------------
    // The is-falling flag and old-Y position are read at the top of
    // vanilla's own in-fluid travel, i.e. *after* the jump block above has
    // already altered velocity.
    let is_falling = state.velocity.y <= 0.0;
    let old_y = state.position.y;
    let base_gravity = effective_gravity(
        f64::from(profile.gravity),
        is_falling,
        state.effects.slow_falling,
    );

    // Vanilla's own in-water travel, in vanilla's order: the sprint/walk base, then
    // the Depth Strider lerp, then Dolphin's Grace *overriding* the result. The
    // order is observable — Grace wins outright, but the Strider term still moves
    // `speed` even when Grace has flattened the slow-down value.
    let mut slow_down = if state.sprinting {
        profile.water_sprint_slow_down
    } else {
        profile.water_slow_down
    };
    let mut speed = profile.fluid_input_speed;
    let mut water_walker = state.water_movement_efficiency;
    if !state.on_ground {
        water_walker *= 0.5f32;
    }
    if water_walker > 0.0 {
        slow_down += (0.546_000_06f32 - slow_down) * water_walker;
        speed += (effective_speed(profile, state) - speed) * water_walker;
    }
    if state.effects.dolphins_grace {
        slow_down = 0.96f32;
    }

    let accel = input_vector(xxa, zza, speed, state.yaw);
    state.velocity = state.velocity.add(accel);
    do_move(state, view, profile, input.sneak, input.sneak, nearby);

    // Vanilla's own fall-damage bookkeeping call. `in_water` is `true` — the
    // reset above already zeroed `fall_distance` for this whole tick, and
    // vanilla's own `!isInWater()` guard would block any accumulation here
    // too, so this is only reachable for its grounded-reset half (e.g.
    // touching a submerged floor).
    accumulate_fall_distance(state, state.position.y - old_y, true);

    // `if (horizontalCollision && onClimbable()) movement = (x, 0.2, z)` — a ladder
    // still lifts you while submerged, and it does so *before* the water drag.
    let mut movement = state.velocity;
    if state.horizontal_collision && on_climbable(state, view) {
        movement = Vec3d::new(movement.x, 0.2, movement.z);
    }

    let movement = movement.multiply_each(
        f64::from(slow_down),
        f64::from(0.8f32),
        f64::from(slow_down),
    );
    state.velocity =
        fluid_falling_adjusted_movement(base_gravity, is_falling, state.sprinting, movement);
    jump_out_of_fluid(state, old_y, view, profile);
}

/// One tick of movement while submerged in lava (vanilla's own in-lava
/// travel).
///
/// Lava is a *different branch* from water, not a retuned one: input speed is a
/// flat `0.02F`, and gravity is applied as an extra `-baseGravity/4` term
/// regardless of depth. What differs by depth is the post-move velocity scale:
///
/// * **deep** (`!isInShallowFluid(LAVA)`) ⇒ a flat `scale(0.5)` on all three
///   axes, with no buoyant falling-adjustment at all;
/// * **shallow** (`isInShallowFluid(LAVA)`, i.e. `lava_height <=
///   `[`fluid_jump_threshold`]) ⇒ `multiply(0.5, 0.8, 0.5)` (a *different* Y
///   factor from deep's implicit `0.5`) followed by
///   [`fluid_falling_adjusted_movement`] — the same buoyant slow-descent water
///   always gets, which deep lava never does.
///
/// The predicate and both arms were ported from the jar directly (not from a
/// summary): vanilla's own "is in shallow fluid" check is `getFluidHeight(tag)
/// <= getFluidJumpThreshold()`, already used by [`apply_fluid_jump`] for the
/// jump decision, so this reuses the same [`FluidState::lava_height`] /
/// [`fluid_jump_threshold`] inputs rather than adding a parallel predicate.
///
/// The fall-distance accumulator does **not** participate in this predicate
/// or either arm — the is-falling flag here is the velocity's own Y
/// component `<= 0.0`, not a
/// fall-distance comparison, despite the "fall-distance or depth comparison"
/// pattern this file's sibling gravity code might suggest. Confirmed by
/// reading vanilla's own in-fluid travel, in-lava travel and fluid-falling
/// adjusted-movement functions directly: none of the three references the
/// fall-distance field.
pub fn tick_lava(
    state: &mut PlayerState,
    input: MovementInput,
    fluid: &FluidState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    tick_lava_among_entities(state, input, fluid, view, profile, &[]);
}

fn tick_lava_among_entities(
    state: &mut PlayerState,
    input: MovementInput,
    fluid: &FluidState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    nearby: &[crate::push::NearbyEntity],
) {
    match profile.fluid_model {
        FluidModel::Modern => {}
        FluidModel::Legacy1_8 => {
            unimplemented!("1.8 fluid movement is not implemented yet")
        }
    }
    // Vanilla's own per-tick lava halving: `if (isInLava()) fallDistance *= 0.5;`.
    // This function is only reached when the per-tick fluid summary already says
    // `in_lava()`, matching vanilla's own "in lava" predicate deciding both this
    // halving and the in-lava travel dispatch.
    state.fall_distance *= 0.5;
    // Base-tick fluid current push (see `tick_water`); lava uses its own scale.
    apply_fluid_push(
        state,
        view,
        crate::fluid::FluidKind::Lava,
        profile.lava_push_scale,
        profile,
    );
    decrement_no_jump_delay(state);
    state.velocity = snap_small_velocity(state.velocity);

    let (xxa, zza) = set_sprint_and_modify_input(state, input, profile);

    // Vanilla's own jump block: in *shallow* lava while on the ground you
    // jump out normally; only deep lava gets the in-liquid jump's +0.04 (see
    // `apply_fluid_jump`).
    apply_fluid_jump(state, input, fluid, view, profile);

    // The is-falling flag / base-gravity value are read here, at the top of
    // vanilla's own in-fluid travel, i.e. after the jump block above has already altered
    // velocity but before the relative-move step adds this tick's input
    // acceleration. Vanilla's own effective-gravity check folds in the Slow
    // Falling clamp exactly as `tick_water` computes it a few lines above;
    // lava shares the same in-fluid travel call site, so it must apply the
    // same clamp.
    let is_falling = state.velocity.y <= 0.0;
    let base_gravity = effective_gravity(
        f64::from(profile.gravity),
        is_falling,
        state.effects.slow_falling,
    );
    let old_y = state.position.y;

    // Vanilla's own relative-move(0.02) → move → shallow/deep branch →
    // -baseGravity/4.
    let accel = input_vector(xxa, zza, profile.fluid_input_speed, state.yaw);
    state.velocity = state.velocity.add(accel);
    do_move(state, view, profile, input.sneak, input.sneak, nearby);

    // Vanilla's own fall-damage bookkeeping call. Not water on this path.
    accumulate_fall_distance(state, state.position.y - old_y, false);

    // Vanilla's own "is in shallow lava" check: shallow gets the same
    // buoyant falling-adjustment water always gets, on top of a
    // Y-asymmetric `multiply(0.5, 0.8, 0.5)`; deep gets a flat `scale(0.5)`
    // with no adjustment at all. Reuses the same threshold/height inputs
    // `apply_fluid_jump` already reads for the jump decision.
    let threshold = fluid_jump_threshold(state.pose.eye_height());
    let shallow_lava = fluid.lava_height <= threshold;
    if shallow_lava {
        let movement = state.velocity.multiply_each(0.5, f64::from(0.8f32), 0.5);
        state.velocity =
            fluid_falling_adjusted_movement(base_gravity, is_falling, state.sprinting, movement);
    } else {
        state.velocity = state.velocity.scale(0.5);
    }
    if base_gravity != 0.0 {
        state.velocity = state
            .velocity
            .add(Vec3d::new(0.0, -base_gravity / 4.0, 0.0));
    }
    jump_out_of_fluid(state, old_y, view, profile);
}
