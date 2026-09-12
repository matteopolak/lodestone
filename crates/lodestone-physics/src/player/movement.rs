/// Vanilla's own player speed: the movement-speed attribute cast to `float`.
///
/// Walking is the base `0.1F` (widened to `double` when stored, then cast back
/// to `float`); sprinting applies the `+0.3` multiply-on-total modifier in
/// `double` before the final `float` cast. Reproduced exactly here.
#[must_use]
fn player_speed(profile: &PhysicsProfile, sprinting: bool) -> f32 {
    let base = f64::from(profile.base_movement_speed); // 0.1F widened
    if sprinting {
        (base * (1.0 + f64::from(profile.sprint_speed_modifier))) as f32
    } else {
        base as f32
    }
}

/// Vanilla's own client-side input modifier, for the modern (1.21+) input
/// pipeline.
///
/// **Version note:** the square-movement normalization is a *structural*
/// difference between modern and legacy clients, not a scalar — see
/// [`PhysicsProfile`] docs. This implements the modern form; a 1.8 client
/// would use a different mapping.
#[must_use]
fn modify_input(
    model: InputModel,
    strafe: f32,
    forward: f32,
    using_item: Option<UseEffects>,
    sneak: bool,
    sneak_factor: f32,
) -> (f32, f32) {
    match model {
        InputModel::UnitSquareProjection => {
            modify_input_unit_square(strafe, forward, using_item, sneak, sneak_factor)
        }
        // Structural seam: 1.8 used its own "move flying" step (normalise by
        // max(1, magnitude), no unit-square projection). Deliberately not modelled yet — failing
        // loudly here is correct, because silently running the modern transform
        // would produce wrong-but-plausible 1.8 movement. Blocked on the 1.8
        // client restructure + a 1.8 JVM oracle.
        InputModel::LegacyMoveFlying => {
            unimplemented!("1.8 moveFlying input pipeline is not implemented yet")
        }
    }
}

fn modify_input_unit_square(
    strafe: f32,
    forward: f32,
    using_item: Option<UseEffects>,
    sneak: bool,
    sneak_factor: f32,
) -> (f32, f32) {
    if strafe * strafe + forward * forward == 0.0 {
        return (strafe, forward);
    }
    let mut sx = strafe * 0.98;
    let mut sy = forward * 0.98;
    // Vanilla's own input modifier: while using an item (and not a
    // passenger), scale the input by the item's use-speed multiplier —
    // between the `0.98` scale above and the sneak scale below. The
    // "not a passenger" conjunct is dropped: this crate has no riding state
    // (see `PlayerState::on_ground`'s own note on the same gap).
    if let Some(effects) = using_item {
        sx *= effects.speed_multiplier;
        sy *= effects.speed_multiplier;
    }
    if sneak {
        sx *= sneak_factor;
        sy *= sneak_factor;
    }
    // modifyInputSpeedForSquareMovement
    let length = (sx * sx + sy * sy).sqrt();
    if length <= 0.0 {
        return (sx, sy);
    }
    let dir_x = sx / length;
    let dir_y = sy / length;
    let ax = dir_x.abs();
    let ay = dir_y.abs();
    let tan = if ay > ax { ax / ay } else { ay / ax };
    let dist_to_unit_square = (1.0 + tan * tan).sqrt();
    let modified_length = (length * dist_to_unit_square).min(1.0);
    (dir_x * modified_length, dir_y * modified_length)
}

/// Vanilla's own relative-input-to-velocity step: the yaw-rotated,
/// speed-scaled acceleration that gets added to velocity each tick.
///
/// This is entity-agnostic and public so a mob loop can produce its per-tick
/// velocity *contribution* the same way the player pipeline does — vanilla
/// drives both players and mobs through the same relative-movement step, and
/// re-deriving this yaw rotation by hand is a divergence surface (the
/// quantized sin/cos table and the exact `f32`/`f64` widths must match). Feed
/// the result (plus gravity) into [`crate::entity::move_entity`] as
/// `motion.velocity`.
///
/// Scope: `strafe`/`forward` are the horizontal relative-movement axes; the
/// vertical relative component is fixed at `0`, which covers walking mobs. A
/// fully-general relative-movement step with a vertical input (a swimming or
/// flying mob) can be added when one is wired.
pub fn input_vector(strafe: f32, forward: f32, speed: f32, yaw: f32) -> Vec3d {
    let input = Vec3d::new(f64::from(strafe), 0.0, f64::from(forward));
    let length_sqr = input.length_sqr();
    if length_sqr < 1.0E-7 {
        return Vec3d::ZERO;
    }
    let scaled = if length_sqr > 1.0 {
        input.normalize()
    } else {
        input
    }
    .scale(f64::from(speed));
    let rad = yaw * (core::f32::consts::PI / 180.0);
    let sin = f64::from(mth::sin(f64::from(rad)));
    let cos = f64::from(mth::cos(f64::from(rad)));
    Vec3d::new(
        scaled.x * cos - scaled.z * sin,
        scaled.y,
        scaled.z * cos + scaled.x * sin,
    )
}

/// Vanilla's own "block below that affects my movement" lookup.
///
/// For the common case (no fence/wall special-casing) this is the block at
/// `(floor(x), floor(y - 0.500001), floor(z))`.
pub(crate) fn friction_block(position: Vec3d) -> (i32, i32, i32) {
    let x = mth::floor(position.x);
    let y = mth::floor(position.y - f64::from(0.500001f32));
    let z = mth::floor(position.z);
    (x, y, z)
}

/// The player's per-tick call into the shared entity move core
/// ([`move_entity`]). Restricted, as vanilla's own self-mover-type move step
/// is, to the parts that affect a player's reported position: collide,
/// commit position, update collision flags, restitute movement after
/// collisions, and apply the block speed factor.
///
/// This is a thin wrapper: it lifts the player's motion into an [`EntityMotion`],
/// supplies the player's current-pose hitbox ([`PlayerState::dimensions`]) and a
/// [`MoveContext`] (Slow Falling, and `suppress_bounce` = the player sneaking,
/// which both zeroes the base entity restitution and vetoes the block-bounce
/// branch), runs the shared core, and writes the result back. A mob loop would
/// call [`move_entity`] directly with its own dimensions and velocity — the
/// arithmetic is identical, which is the whole point of the shared core.
///
/// `suppress_bounce` and `staying_on_ground_surface` are **both** the sneak
/// key in vanilla, but they are separate parameters here on purpose: they
/// are separate virtual methods serving unrelated rules, our elytra path
/// already disagrees about the first (passing `false`), and collapsing them
/// would make the edge back-off silently inherit whatever that path decided
/// about bouncing.
fn do_move(
    state: &mut PlayerState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    suppress_bounce: bool,
    staying_on_ground_surface: bool,
    nearby: &[crate::push::NearbyEntity],
) {
    let mut motion = EntityMotion {
        position: state.position,
        velocity: state.velocity,
        on_ground: state.on_ground,
        horizontal_collision: state.horizontal_collision,
        stuck_speed_multiplier: state.stuck_speed_multiplier,
    };
    let ctx = MoveContext {
        slow_falling: state.effects.slow_falling,
        suppress_bounce,
        // The same two `Player` overrides `tick_air`'s `AirTravelContext` applies,
        // kept in step deliberately: this is the fluid/elytra move wrapper, and a
        // gate that held on one path and not the other would be a divergence
        // between travel modes rather than against vanilla.
        edge_back_off: if state.flying {
            EdgeBackOff::Entity
        } else {
            EdgeBackOff::Player {
                staying_on_ground_surface,
                fall_distance: state.fall_distance,
            }
        },
        // Vanilla's own block-speed-factor gate: not flying and not gliding.
        // The elytra half is what this reaches in practice — `do_move`'s
        // fluid callers are unreachable while flying, because the dispatch
        // suppressor sends a flying player to `tick_air` instead.
        suppress_block_speed_factor: state.flying || state.fall_flying,
    };
    move_entity_with_nearby(&mut motion, state.dimensions(), view, profile, ctx, nearby);
    state.position = motion.position;
    state.velocity = motion.velocity;
    state.on_ground = motion.on_ground;
    state.horizontal_collision = motion.horizontal_collision;
    state.stuck_speed_multiplier = motion.stuck_speed_multiplier;
}

/// Vanilla's own post-collision velocity rewrite that zeroes blocked axes
/// and produces slime/bed bounces.
///
/// `current` is the pre-collision velocity (still `== delta` here);
/// `resolved` is the movement actually achieved. A living entity has zero
/// base bounciness, so horizontal wall bounces never happen for a player;
/// the only live branch is the vertical land-bounce off a bouncy block.
#[allow(clippy::too_many_arguments)]
pub(crate) fn restitute_movement_after_collisions(
    current: Vec3d,
    resolved: Vec3d,
    x_collision: bool,
    z_collision: bool,
    vertical_collision: bool,
    vertical_collision_below: bool,
    position: Vec3d,
    slow_falling: bool,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    suppress_bounce: bool,
) -> Vec3d {
    // restitution starts at vanilla's own base entity bounciness (0.0 for a
    // player), or 0 while sneaking.
    let mut restitution: f64 = 0.0;
    let mut vx = current.x;
    let mut vy = current.y;
    let mut vz = current.z;
    if x_collision {
        vx = -current.x * restitution;
    }
    if z_collision {
        vz = -current.z * restitution;
    }

    if vertical_collision {
        if vertical_collision_below {
            // Block at vanilla's own legacy on-pos lookup (0.2 below), from
            // the post-move pos.
            let ex = mth::floor(position.x);
            let ey = mth::floor(position.y - f64::from(0.2f32));
            let ez = mth::floor(position.z);
            let block_bounciness = f64::from(view.bounce_restitution(ex, ey, ez));
            let effective_gravity =
                effective_gravity(f64::from(profile.gravity), current.y <= 0.0, slow_falling);
            // `!(-current.y < effGravity)`: only a fast-enough landing bounces (a
            // resting entity does not jitter). Kept as vanilla's negated `<` rather
            // than `>=` so the NaN edge matches its float expression exactly.
            #[allow(clippy::neg_cmp_op_on_partial_ord)]
            let fast_enough = !(-current.y < effective_gravity);
            restitution = if fast_enough && !suppress_bounce {
                restitution.max(block_bounciness)
            } else {
                0.0
            };
        }

        let (gravity_compensation, effective_drag) = if restitution > 0.0 {
            let portion_with_movement = resolved.y / current.y;
            let effective_gravity =
                effective_gravity(f64::from(profile.gravity), current.y <= 0.0, slow_falling);
            (
                portion_with_movement * effective_gravity,
                mth::lerp_f64(portion_with_movement, 1.0, f64::from(0.98f32)),
            )
        } else {
            (0.0, 1.0)
        };
        vy = (gravity_compensation - current.y) * effective_drag * restitution;
    }

    Vec3d::new(vx, vy, vz)
}

/// Vanilla's own approximate-equality check: `abs(b - a) < 1.0E-5F`.
pub(crate) fn mth_equal(a: f64, b: f64) -> bool {
    (b - a).abs() < f64::from(1.0e-5f32)
}

