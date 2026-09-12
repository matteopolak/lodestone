/// Vanilla's own ground-jump impulse (including the sprint boost), and
/// vanilla's own trident-release riptide impulse.
///
/// # What this function is, and is not, responsible for
///
/// Vanilla reaches this arithmetic only after three gates this function does
/// **not** evaluate, because none of them is physics state: the trident's
/// resolved spin-attack strength being above zero (equipment/enchantment
/// data), the item-use hold time reaching its throw threshold (`10` ticks —
/// item-use duration, owned by the interaction layer), and being in water or
/// rain (fluid presence **or** weather — this crate's
/// [`CollisionView::is_water`] answers the first half but has no weather
/// concept for the second). A driver must evaluate all three itself and call
/// this only once, on the release edge, with `strength` already resolved to
/// the enchantment's value (`> 0.0`).
///
/// What *is* physics, and what this function does:
///
/// 1. **The impulse.** `xd/yd/zd` from yaw/pitch via the quantized sine
///    table (bit-exact — see [`mth`]), normalised and scaled by `strength`,
///    added to velocity via vanilla's own plain velocity-add.
/// 2. **The spin-attack state.** Vanilla's own 20-tick, power-`8.0F` spin
///    attack arming reduces, for this crate, to setting
///    [`PlayerState::auto_spin_attack_ticks`] to `20` — the damage/held-item
///    half is not modelled (see that field's doc).
/// 3. **The on-ground pop-up.** If on the ground, vanilla issues a real
///    collision-resolving move straight up by `1.1999999` (so a riptide
///    launched under a low ceiling stops at the ceiling), reproduced via
///    [`move_entity`] with a synthetic, zeroed `stuck_speed_multiplier`: this
///    is a one-off supplementary move outside the tick's own, so nothing
///    tick-pending should be consumed by it, and vanilla's own raw move call
///    here is likewise independent of its travel step's own stuck-multiplier
///    handling.
pub fn apply_riptide(
    state: &mut PlayerState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    strength: f32,
) {
    let yaw_rad = f64::from(state.yaw) * (core::f64::consts::PI / 180.0);
    let pitch_rad = f64::from(state.pitch) * (core::f64::consts::PI / 180.0);
    let mut xd = -mth::sin(yaw_rad) * mth::cos(pitch_rad);
    let mut yd = -mth::sin(pitch_rad);
    let mut zd = mth::cos(yaw_rad) * mth::cos(pitch_rad);
    let dist = (xd * xd + yd * yd + zd * zd).sqrt();
    xd *= strength / dist;
    yd *= strength / dist;
    zd *= strength / dist;
    state.velocity = state
        .velocity
        .add(Vec3d::new(f64::from(xd), f64::from(yd), f64::from(zd)));

    state.auto_spin_attack_ticks = 20;

    if state.on_ground {
        let mut motion = EntityMotion {
            position: state.position,
            velocity: Vec3d::new(0.0, 1.199_999_9, 0.0),
            on_ground: state.on_ground,
            horizontal_collision: state.horizontal_collision,
            stuck_speed_multiplier: Vec3d::ZERO,
        };
        move_entity(
            &mut motion,
            state.dimensions(),
            view,
            profile,
            MoveContext::default(),
        );
        state.position = motion.position;
    }
}

/// The strength `apply_riptide` wants for one level of Riptide, straight out of
/// the enchantment's own data file.
///
/// `data/minecraft/enchantment/riptide.json` declares
/// `minecraft:trident_spin_attack_strength` as `{"type": "minecraft:add",
/// "value": {"type": "minecraft:linear", "base": 1.5,
/// "per_level_above_first": 0.75}}`, and vanilla's own enchantment-effect
/// resolution sums that effect over the stack's enchantments — so a single
/// Riptide N contributes exactly
/// `1.5 + 0.75 * (N - 1)`: **1.5 / 2.25 / 3.0** for I / II / III.
///
/// Read from the data file rather than from a recollected constant: the
/// per-level term is `0.75`, not the `0.5` a `1.5, 2.0, 2.5` ladder would
/// imply, and the difference at Riptide III is a full block per tick.
///
/// `level == 0` (no Riptide) is `0.0`, which is exactly the `> 0.0F` test
/// vanilla's own release-using logic gates the whole launch on.
#[must_use]
pub fn riptide_spin_attack_strength(level: u32) -> f32 {
    if level == 0 {
        return 0.0;
    }
    1.5f32 + 0.75f32 * (level as f32 - 1.0)
}

/// Vanilla's own "can glide" check, together with the player's own override
/// for it.
///
/// `!flying && !onGround && !isPassenger && !hasEffect(LEVITATION)` plus "some
/// equipment slot holds a glider". The last conjunct is **not** physics state —
/// vanilla walks every equipment slot looking for a glider component — so
/// the caller resolves it and passes the answer in, the same division of
/// labour [`apply_riptide`] uses for the enchantment gate.
///
/// This engine has no riding state on [`PlayerState`], so the `!isPassenger()`
/// conjunct is the caller's too (the ECS driver holds it as a component); it is
/// vacuous here.
#[must_use]
pub fn can_glide(state: &PlayerState, glider_equipped: bool) -> bool {
    !state.flying
        && !state.on_ground
        && state.effects.levitation.is_none()
        && glider_equipped
}

/// Vanilla's own start-fall-flying attempt — the
/// **client-predicted** start of an elytra glide.
///
/// ```text
/// if (!this.isFallFlying() && this.canGlide() && !this.isInWater()) {
///    this.startFallFlying();   // setSharedFlag(7, true)
///    return true;
/// }
/// ```
///
/// Returns whether the glide started, which is exactly the bit vanilla's
/// own per-tick client update turns into one start-fall-flying player
/// command — so a driver should send that command if and only if this
/// returned `true`.
///
/// **The start is client-authoritative and the stop is not.** Vanilla's client
/// sets the shared flag itself here and only then tells the server; the
/// *end* of a glide is decided server-side by vanilla's own fall-flying
/// update's "can no longer glide" branch and synced back as entity data. A
/// client that models the start but not the stop glides forever once it
/// lands, so a driver must also run [`update_fall_flying`] each tick.
pub fn try_start_fall_flying(
    state: &mut PlayerState,
    glider_equipped: bool,
    in_water: bool,
) -> bool {
    if state.fall_flying || !can_glide(state, glider_equipped) || in_water {
        return false;
    }
    state.fall_flying = true;
    true
}

/// The half of vanilla's own fall-flying update that ends a glide.
///
/// Vanilla's own version of this check runs only when **not** on the client,
/// and inside that guard clears its own shared "gliding" flag once its own
/// "can glide" check fails.
///
/// Vanilla runs this **server-side only** and syncs the cleared flag; we run
/// it on the client because this client has no server that tracks glide
/// state, a gap left by an unfinished server-side glide tracker. Predicting
/// the stop locally is the safe direction: the dominant trigger is the
/// on-ground flag, and a client that keeps `fall_flying` set after touching
/// down routes every subsequent tick through [`tick_elytra`] and can never
/// walk again.
///
/// The elytra-durability and glide game-event halves of vanilla's own
/// update are deliberately not modelled — both are server-only effects on
/// server-owned item state.
pub fn update_fall_flying(state: &mut PlayerState, glider_equipped: bool) {
    if state.fall_flying && !can_glide(state, glider_equipped) {
        state.fall_flying = false;
    }
}

fn jump_from_ground(state: &mut PlayerState, view: &dyn CollisionView, profile: &PhysicsProfile) {
    // Vanilla's own jump-power expression: jump strength * block-jump
    // factor + jump-boost power. The block-jump-factor product and the
    // boost are separate terms in one float expression; honey reduces the
    // former (0.5), Jump Boost adds the latter.
    let block_jump_factor = block_jump_factor(state.position, view);
    let jump_power =
        profile.jump_power * block_jump_factor + jump_boost_power(state.effects.jump_boost);
    if jump_power <= 1.0e-5 {
        return;
    }
    let v = state.velocity;
    state.velocity = Vec3d::new(v.x, f64::from(jump_power).max(v.y), v.z);
    if state.sprinting {
        let angle = state.yaw * (core::f32::consts::PI / 180.0);
        let boost = profile.sprint_jump_boost;
        state.velocity = state.velocity.add(Vec3d::new(
            f64::from(-mth::sin(f64::from(angle))) * boost,
            0.0,
            f64::from(mth::cos(f64::from(angle))) * boost,
        ));
    }
}

/// Vanilla's own jump-boost power: `0.1F * (amp + 1)` as a `float`, or `0`.
fn jump_boost_power(jump_boost: Option<u32>) -> f32 {
    match jump_boost {
        Some(amp) => 0.1f32 * (amp as f32 + 1.0f32),
        None => 0.0f32,
    }
}

/// Vanilla's own block-jump-factor lookup: the jump factor of the block at
/// the feet, or the block below when the feet block is neutral (`== 1.0`).
/// Honey is `0.5`.
fn block_jump_factor(position: Vec3d, view: &dyn CollisionView) -> f32 {
    let here_x = mth::floor(position.x);
    let here_y = mth::floor(position.y);
    let here_z = mth::floor(position.z);
    let here = view.jump_factor(here_x, here_y, here_z);
    if here == 1.0 {
        let (bx, by, bz) = friction_block(position);
        view.jump_factor(bx, by, bz)
    } else {
        here
    }
}

/// Vanilla's own auto-jump gate, checked before any auto-jump detection.
///
/// The "staying on ground surface" check is just the raw shift key, so a
/// sneaking player never auto-jumps. The "is moving" check reads the
/// *input* move-vector, not the actual movement — the detector still fires
/// for a standing player pressing forward into a step. This engine has no
/// riding state, so the `!isPassenger()` conjunct is vacuous.
fn can_auto_jump(state: &PlayerState, input: MovementInput, view: &dyn CollisionView) -> bool {
    let (mv_x, mv_y) = normalized_move_vector(input);
    state.auto_jump_enabled
        && state.auto_jump_time <= 0
        && state.on_ground
        && !input.sneak
        && mv_x * mv_x + mv_y * mv_y > 0.0
        && block_jump_factor(state.position, view) >= 1.0
}

/// Vanilla's own client-side auto-jump detector. Called at the end of its
/// move step with the **actual** x/z deltas the sweep produced
/// (post-collision, cast to `float`).
///
/// When it decides the player is about to walk into a stepable obstacle (rise
/// strictly greater than `0.5` and at most the jump height, clear headroom, and
/// the movement generally facing the player) it arms
/// [`PlayerState::auto_jump_time`], which the next tick's prologue spends as a
/// forced jump — the one-tick deferral vanilla's own client spells as a
/// synthetic jump-key input.
///
/// This is **client-only** steering: the server never sees the detector, only
/// the resulting jump, so an exact port matters for *feel* (and for the
/// reference JVM oracle), not for anti-cheat.
fn update_auto_jump(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    xa: f32,
    za: f32,
) {
    if !can_auto_jump(state, input, view) {
        return;
    }

    // `position()` at this point is the post-move feet position (move() has
    // completed); the look-ahead is anchored there.
    let move_begin = state.position;
    let mut move_diff = Vec3d::new(f64::from(xa), 0.0, f64::from(za));
    let move_end = move_begin.add(move_diff);
    let current_speed = effective_speed(profile, state);
    let mut move_dist_sq = move_diff.length_sqr() as f32;

    // The player barely moved this tick (standing still, or fully blocked): fall
    // back to the *intended* input vector — vanilla's own normalised
    // strafe/forward move vector, rotated into world space by the yaw.
    if move_dist_sq <= 0.001 {
        let (mv_x, mv_y) = normalized_move_vector(input);
        let input_xa = current_speed * mv_x;
        let input_za = current_speed * mv_y;
        let angle = state.yaw * (core::f32::consts::PI / 180.0);
        let sin = mth::sin(f64::from(angle));
        let cos = mth::cos(f64::from(angle));
        move_diff = Vec3d::new(
            f64::from(input_xa * cos - input_za * sin),
            move_diff.y,
            f64::from(input_za * cos + input_xa * sin),
        );
        move_dist_sq = move_diff.length_sqr() as f32;
        if move_dist_sq <= 0.001 {
            return;
        }
    }

    // Vanilla's own fast inverse-sqrt approximation, not a plain `1/sqrt`.
    let move_dist_inverted = mth::inv_sqrt_f32(move_dist_sq);
    let move_dir = move_diff.scale(f64::from(move_dist_inverted));

    // Vanilla's own "forward" vector: direction-from-rotation(pitch, yaw).
    let facing = forward_direction(state.pitch, state.yaw);
    let facing_dot = (facing.x * move_dir.x + facing.z * move_dir.z) as f32;
    if facing_dot < -0.15 {
        return;
    }

    // The block at the box's top and the cell one above: both must be free
    // of collision for the jump to have headroom.
    let bb = state.bounding_box(profile);
    let cx = mth::floor(state.position.x);
    let cz = mth::floor(state.position.z);
    let mut ceiling_y = mth::floor(bb.max_y);
    if !block_has_no_collision(view, cx, ceiling_y, cz)
        || !block_has_no_collision(view, cx, ceiling_y + 1, cz)
    {
        return;
    }

    // `1.2F` plus `(amplifier + 1) * 0.75F` per Jump Boost level.
    let mut jump_height = 1.2f32;
    if let Some(amp) = state.effects.jump_boost {
        jump_height += (amp as f32 + 1.0) * 0.75;
    }

    // Vanilla's own max of (currentSpeed * 7.0F) and (1.0F / moveDistInverted)
    // — both floats widened to double for the max; the result stays a double.
    let look_ahead_dist =
        f64::from(current_speed * 7.0f32).max(f64::from(1.0f32 / move_dist_inverted));

    let seg_begin = move_begin;
    let seg_end = move_end.add(move_dir.scale(look_ahead_dist));
    let dims = state.dimensions();
    let player_width = dims.width;
    let player_height = dims.height;

    // Vanilla's own box construction: from segBegin to segEnd offset by
    // player height, inflated by player width on X/Z — the constructor
    // normalises min/max per coordinate.
    let far = seg_end.add(Vec3d::new(0.0, f64::from(player_height), 0.0));
    let test_box = Aabb::new(
        move_begin.x.min(far.x) - f64::from(player_width),
        move_begin.y.min(far.y),
        move_begin.z.min(far.z) - f64::from(player_width),
        move_begin.x.max(far.x) + f64::from(player_width),
        move_begin.y.max(far.y),
        move_begin.z.max(far.z) + f64::from(player_width),
    );

    // The probe segments are the swept line raised by `0.51F` (just above a
    // stair's low face, so a low step is *seen* but its near face is not
    // mistaken for a tall wall), offset left/right by `rightDir = moveDir × up`,
    // scaled by `width * 0.5F`.
    let seg_begin = seg_begin.add(Vec3d::new(0.0, f64::from(0.51f32), 0.0));
    let seg_end = seg_end.add(Vec3d::new(0.0, f64::from(0.51f32), 0.0));
    let right_dir = Vec3d::new(-move_dir.z, 0.0, move_dir.x);
    let right_offset = right_dir.scale(f64::from(player_width * 0.5));
    let left_begin = seg_begin.subtract(right_offset);
    let left_end = seg_end.subtract(right_offset);
    let right_begin = seg_begin.add(right_offset);
    let right_end = seg_end.add(right_offset);

    // Vanilla's own collision-gathering step: every block collision box the
    // swept box touches, over its own cursor range (`floor(box ± 1.0E-7) ∓ 1
    // .. floor(box ± 1.0E-7) ± 1`). A box only counts if it intersects either
    // probe segment.
    let mut boxes: Vec<Aabb> = Vec::new();
    let x0 = mth::floor(test_box.min_x - 1.0e-7) - 1;
    let x1 = mth::floor(test_box.max_x + 1.0e-7) + 1;
    let y0 = mth::floor(test_box.min_y - 1.0e-7) - 1;
    let y1 = mth::floor(test_box.max_y + 1.0e-7) + 1;
    let z0 = mth::floor(test_box.min_z - 1.0e-7) - 1;
    let z1 = mth::floor(test_box.max_z + 1.0e-7) + 1;
    for x in x0..=x1 {
        for y in y0..=y1 {
            for z in z0..=z1 {
                view.collision_boxes(x, y, z, &mut boxes);
            }
        }
    }

    // Java's `Float.MIN_VALUE` sentinel is the smallest *positive* subnormal
    // (`1.0E-45`), not the most-negative float — Rust's `f32::MIN` is the wrong
    // sentinel and would compare unequal to every real height.
    let no_obstacle = f32::from_bits(1);
    let mut obstacle_height = no_obstacle;

    for box_ in &boxes {
        if segment_intersects(box_, left_begin, left_end)
            || segment_intersects(box_, right_begin, right_end)
        {
            obstacle_height = box_.max_y as f32;
            // Vanilla's own box-centre: lerp(0.5, min, max) per axis, then
            // floored to a block position.
            let center = Vec3d::new(
                mth::lerp_f64(0.5, box_.min_x, box_.max_x),
                mth::lerp_f64(0.5, box_.min_y, box_.max_y),
                mth::lerp_f64(0.5, box_.min_z, box_.max_z),
            );
            let obstacle_x = mth::floor(center.x);
            let obstacle_y = mth::floor(center.y);
            let obstacle_z = mth::floor(center.z);

            // `for (int steps = 1; steps < jumpHeight; steps++)` — an int
            // compared against a float, so the body runs once for the default
            // `1.2` and twice only when Jump Boost pushes the height past `2`.
            for steps in 1..(jump_height.ceil() as i32) {
                let above_y = obstacle_y + steps;
                if !block_has_no_collision(view, obstacle_x, above_y, obstacle_z) {
                    // Vanilla's own block-local top (uncapped) plus the
                    // block's own y, in float math.
                    obstacle_height =
                        view.collision_top(obstacle_x, above_y, obstacle_z) as f32 + above_y as f32;
                    if f64::from(obstacle_height) - state.position.y > f64::from(jump_height) {
                        return;
                    }
                }
                if steps > 1 {
                    ceiling_y += 1;
                    if !block_has_no_collision(view, cx, ceiling_y, cz) {
                        return;
                    }
                }
            }
            break;
        }
    }

    if obstacle_height != no_obstacle {
        // `(float)(obstacleHeight - getY())` then the double-sided gate
        // `!(ydelta <= 0.5F) && !(ydelta > jumpHeight)`.
        let ydelta = (f64::from(obstacle_height) - state.position.y) as f32;
        if ydelta > 0.5 && ydelta <= jump_height {
            state.auto_jump_time = 1;
        }
    }
}

/// Vanilla's own input move-vector: `(leftImpulse, forwardImpulse)`
/// normalised with a `1.0E-4F` zero-threshold. `x` is the strafe axis and
/// `y` the forward axis, matching `MovementInput`'s `strafe`/`forward`.
fn normalized_move_vector(input: MovementInput) -> (f32, f32) {
    let dist = (input.strafe * input.strafe + input.forward * input.forward).sqrt();
    if dist < 1.0e-4f32 {
        (0.0, 0.0)
    } else {
        (input.strafe / dist, input.forward / dist)
    }
}

/// Vanilla's own "forward" vector: direction-from-rotation(pitch, yaw). Only
/// `x` and `z` are read by the auto-jump dot product, but the whole
/// expression is reproduced so the bits match the reference — the `-PI` yaw
/// term and the `-cos(pitch)` x scale are real float arithmetic, not
/// simplifiable.
fn forward_direction(pitch: f32, yaw: f32) -> Vec3d {
    let y_arg = -yaw * (core::f32::consts::PI / 180.0) - core::f32::consts::PI;
    let x_arg = -pitch * (core::f32::consts::PI / 180.0);
    let y_cos = mth::cos(f64::from(y_arg));
    let y_sin = mth::sin(f64::from(y_arg));
    let x_cos = -mth::cos(f64::from(x_arg));
    let x_sin = mth::sin(f64::from(x_arg));
    Vec3d::new(
        f64::from(y_sin * x_cos),
        f64::from(x_sin),
        f64::from(y_cos * x_cos),
    )
}

/// Vanilla's own per-cell empty-collision-shape check — true when the
/// block appends no collision boxes (air, most plants).
fn block_has_no_collision(view: &dyn CollisionView, x: i32, y: i32, z: i32) -> bool {
    let mut boxes = Vec::new();
    view.collision_boxes(x, y, z, &mut boxes);
    boxes.is_empty()
}

/// Vanilla's own segment-vs-box intersection check — the min/max of each
/// coordinate pair, then the strict overlap test (a flush contact is *not*
/// an intersection).
fn segment_intersects(box_: &Aabb, first: Vec3d, second: Vec3d) -> bool {
    let min_x = first.x.min(second.x);
    let min_y = first.y.min(second.y);
    let min_z = first.z.min(second.z);
    let max_x = first.x.max(second.x);
    let max_y = first.y.max(second.y);
    let max_z = first.z.max(second.z);
    box_.min_x < max_x
        && box_.max_x > min_x
        && box_.min_y < max_y
        && box_.max_y > min_y
        && box_.min_z < max_z
        && box_.max_z > min_z
}

/// Vanilla's own no-jump-delay countdown, run at the top of every travel
/// path before the velocity snap-to-zero.
fn decrement_no_jump_delay(state: &mut PlayerState) {
    if state.no_jump_delay > 0 {
        state.no_jump_delay -= 1;
    }
}

/// Vanilla's own velocity snap-to-zero prologue: the horizontal components
/// collapse to zero below `9.0e-6` (a squared distance) and the vertical
/// component collapses below `0.003`. Byte-identical across every travel path
/// (air, water, lava, elytra), so it is factored out once rather than
/// reproduced per path.
fn snap_small_velocity(v: Vec3d) -> Vec3d {
    let mut dx = v.x;
    let mut dy = v.y;
    let mut dz = v.z;
    if v.horizontal_distance_sqr() < 9.0e-6 {
        dx = 0.0;
        dz = 0.0;
    }
    if v.y.abs() < 0.003 {
        dy = 0.0;
    }
    Vec3d::new(dx, dy, dz)
}

/// The sprint-flag write plus the client-side input transform shared by the
/// air, water and lava travel paths.
///
/// The slowdown reads the pose established by the preceding tick, not this
/// tick's raw shift bit. The client computes its moving-slowly state before it
/// refreshes keyboard input, then applies that state during travel. Keeping
/// those values distinct makes both shift edges one tick delayed: the press
/// gets one final full-speed input sample and the release gets one final
/// slowed sample. The raw bit still drives edge back-off, bounce suppression,
/// water descent, and the pose selected at the end of this tick.
fn set_sprint_and_modify_input(
    state: &mut PlayerState,
    input: MovementInput,
    profile: &PhysicsProfile,
) -> (f32, f32) {
    state.sprinting = input.sprint;
    // The ordinary walking/airborne case. Visual crawling also counts as
    // moving slowly, but needs the separate in-water predicate owned by the
    // fluid dispatch; do not infer it from `state.swimming`, which is the
    // sprint-swim state rather than the in-water state.
    let moving_slowly = state.pose == Pose::Crouching;
    modify_input(
        profile.input_model,
        input.strafe,
        input.forward,
        input.using_item,
        moving_slowly,
        profile.sneaking_speed,
    )
}
