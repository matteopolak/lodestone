
/// Which edge-back-off override the entity being moved has.
///
/// Vanilla's own edge-back-off hook is a virtual method whose **base
/// implementation is the identity** — return the delta unchanged. The
/// player is the only override in the tree: it is the sneak-at-a-ledge
/// back-off that stops you walking off a drop while shift is held. Modelling
/// the override as a variant rather than a bare `bool` makes a mob
/// *structurally* unable to acquire player-only behaviour, which is the same
/// property vanilla gets from the class hierarchy.
///
/// [`Self::Entity`] is the [`Default`], so [`crate::MoveContext::default()`] — what
/// a mob or a dropped item passes — is inert by construction.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum EdgeBackOff {
    /// Vanilla's own base implementation: return the delta unchanged. Every
    /// non-player entity.
    #[default]
    Entity,
    /// Vanilla's own sneak-at-a-ledge back-off.
    Player {
        /// Vanilla's own "staying on ground surface" check, which is
        /// exactly the raw shift key — not the crouch *pose* and not the
        /// separate "is crouching" check.
        ///
        /// The two ends of the wire read the same boolean from different
        /// places and it matters that they agree: the client reads its own
        /// input key-presses directly, while the server reads the shared
        /// sneak flag set from the movement-input packet. A driver that
        /// sneaks *locally* without sending the input packet manufactures
        /// the very disagreement this rule exists to prevent, only inverted
        /// — see `docs/edge-back-off.md`.
        staying_on_ground_surface: bool,
        /// Vanilla's own fall-distance accumulator (a `double` since 26.2).
        ///
        /// **An input this crate does not maintain**, like
        /// [`PlayerState::water_movement_efficiency`]. It is read by exactly
        /// one place — vanilla's own "above ground" check's airborne branch
        /// — and only when `on_ground` is `false`.
        fall_distance: f64,
    },
}

/// Vanilla's own sneak-at-a-ledge back-off, called from inside its move
/// step on the *candidate* delta before collision resolution.
///
/// # Why this is a desync rule and not a feel rule
///
/// The server replays the movement we claim through its own move step using
/// the player mover type — one of the two mover types this rule's own gate
/// admits. It then compares the replay's result against the position we
/// claimed and teleports us back if the moved distance exceeds `0.0625`,
/// i.e. **0.25 blocks in a single packet with no accumulator**. Because the
/// intervening Y-distance clamp zeroes Y unconditionally, that comparison is
/// **purely horizontal** — and this rule modifies exactly and only the
/// horizontal components. A client that sneaks near a ledge without
/// modelling it claims a position past where the server's own replay puts
/// it, and gets corrected.
///
/// # The gate, transcribed
///
/// ```text
/// !this.abilities.flying
///   && !(delta.y > 0.0)
///   && (moverType == MoverType.SELF || moverType == MoverType.PLAYER)
///   && this.isStayingOnGroundSurface()
///   && this.isAboveGround(maxDownStep)
/// ```
///
/// Two of the five are satisfied by construction here and so are not parameters:
///
/// * **not flying** — this crate does not model creative flight at all
///   (airborne travel applies gravity unconditionally), so a flying driver
///   does not route through [`tick`]. This is the same standing argument
///   [`update_swimming`] makes for vanilla's own swim-state update's flying
///   override.
/// * **the mover type** — [`crate::move_entity`] *is* vanilla's own
///   self-mover-type move. The excluded types are the piston mover (which
///   this crate has no equivalent of) and the mover types used for
///   vehicle/knockback pushes.
///
/// Vanilla's own "max down step" is its own resolved max-up-step value,
/// i.e. the resolved **step-height attribute**, whose ranged-attribute default is
/// `0.6` — *not* a literal. It arrives as [`EntityDimensions::step_height`],
/// which is documented as the post-modifier value, so a step-height modifier
/// is honoured for free. Hard-coding `0.6` would agree today and silently
/// diverge the moment anything modifies the attribute.
///
/// # The stepping loop
///
/// Vanilla does **not** clamp the delta once; it walks it toward zero in `0.05`
/// increments, re-probing after each step, and stops at the first candidate the
/// probe says will not fall. There are **three** loops, and X and Z are
/// *independent first, then joint*:
///
/// 1. X alone, probing a fall check against `(deltaX, 0.0)`.
/// 2. Z alone, probing a fall check against `(0.0, deltaZ)`, starting from
///    the original `delta.z` (**not** from anything loop 1 produced).
/// 3. X and Z **together**, probing a fall check against `(deltaX, deltaZ)`
///    and stepping both, starting from whatever loops 1 and 2 left behind.
///
/// The third loop is what handles an outside corner, where neither the pure-X nor
/// the pure-Z move leaves the ledge but the diagonal does. It also differs
/// structurally from the first two: those `break` the instant a component is
/// zeroed, whereas the diagonal loop zeroes one component and keeps stepping the
/// other in the same iteration, exiting only when the `!= 0.0` guards fail.
///
/// The step magnitude is fixed at `signum(delta) * 0.05` **computed once**,
/// before any stepping, so it never changes sign mid-loop. `Y` is passed through
/// untouched.
///
/// # What the caller must know
///
/// This rewrites the *local* candidate delta only. Vanilla never writes its
/// own velocity field here, so the vector `restitute_movement_after_collisions`
/// later reads keeps its **un-backed-off** value. A player pressed against a
/// ledge by the back-off therefore keeps accumulating horizontal velocity:
/// the back-off is invisible to the X/Z collision flags too, because those
/// compare against the *backed-off* delta, so a fully-cancelled component
/// reads as "no collision" and is never zeroed. That is vanilla behaviour
/// and it is observable the moment you release shift.
#[must_use]
pub(crate) fn maybe_back_off_from_edge(
    delta: Vec3d,
    back_off: EdgeBackOff,
    bounding_box: Aabb,
    on_ground: bool,
    step_height: f32,
    view: &dyn CollisionView,
) -> Vec3d {
    let EdgeBackOff::Player {
        staying_on_ground_surface,
        fall_distance,
    } = back_off
    else {
        return delta;
    };

    // Vanilla's own max-up-step value — kept at `f32` width, and widened at
    // each use exactly where vanilla's implicit promotion happens.
    let max_down_step = step_height;

    // Vanilla's gate, in vanilla's order and shape: the whole body sits inside one
    // `if`, and the method falls through to `return delta` when it does not hold.
    // `!(delta.y > 0.0)` is kept as written rather than folded to `delta.y <= 0.0`
    // so a NaN Y takes the same branch it does in Java.
    #[allow(
        clippy::neg_cmp_op_on_partial_ord,
        reason = "transcribed from `!(delta.y > 0.0)`; the NaN branch differs from `<=`"
    )]
    if !(delta.y > 0.0)
        && staying_on_ground_surface
        && is_above_ground(bounding_box, on_ground, fall_distance, max_down_step, view)
    {
        let mut delta_x = delta.x;
        let mut delta_z = delta.z;
        // Vanilla's own sign function, computed **once** so the step cannot
        // change sign mid-loop — its zero case differs from Rust's, see
        // [`mth::java_signum`].
        let step_x = mth::java_signum(delta_x) * 0.05;
        let step_z = mth::java_signum(delta_z) * 0.05;
        let min_height = f64::from(max_down_step);

        // Loop 1: X alone.
        while delta_x != 0.0 && can_fall_at_least(bounding_box, delta_x, 0.0, min_height, view) {
            if delta_x.abs() <= 0.05 {
                delta_x = 0.0;
                break;
            }
            delta_x -= step_x;
        }

        // Loop 2: Z alone, from the *original* `delta.z`.
        while delta_z != 0.0 && can_fall_at_least(bounding_box, 0.0, delta_z, min_height, view) {
            if delta_z.abs() <= 0.05 {
                delta_z = 0.0;
                break;
            }
            delta_z -= step_z;
        }

        // Loop 3: both together — the outside-corner case. No `break`: one
        // component may be zeroed while the other keeps stepping in the same
        // iteration, and the `!= 0.0` guards are what end it.
        while delta_x != 0.0
            && delta_z != 0.0
            && can_fall_at_least(bounding_box, delta_x, delta_z, min_height, view)
        {
            if delta_x.abs() <= 0.05 {
                delta_x = 0.0;
            } else {
                delta_x -= step_x;
            }

            if delta_z.abs() <= 0.05 {
                delta_z = 0.0;
            } else {
                delta_z -= step_z;
            }
        }

        return Vec3d::new(delta_x, delta.y, delta_z);
    }

    delta
}

/// Vanilla's own "above ground" check.
///
/// ```text
/// this.onGround() || this.fallDistance < maxDownStep
///                    && !this.canFallAtLeast(0.0, 0.0, maxDownStep - this.fallDistance)
/// ```
///
/// Note the **shrinking** probe depth in the airborne branch: the further you have
/// already fallen, the less additional drop is required to disqualify the
/// back-off. So `fall_distance` errs in a known direction — supplying `0.0` for a
/// genuinely-falling entity probes the *full* step height, which is a strictly
/// weaker fall check, so the gate opens *more* often than vanilla's. That
/// only reaches the airborne branch: while `on_ground` is set the value is
/// unread, and the server resets the fall-distance accumulator to `0.0` on
/// every grounded tick, so the grounded case — the entire bridging /
/// sneak-placing use case — is exact with the default.
#[must_use]
fn is_above_ground(
    bounding_box: Aabb,
    on_ground: bool,
    fall_distance: f64,
    max_down_step: f32,
    view: &dyn CollisionView,
) -> bool {
    on_ground
        || fall_distance < f64::from(max_down_step)
            && !can_fall_at_least(
                bounding_box,
                0.0,
                0.0,
                f64::from(max_down_step) - fall_distance,
                view,
            )
}

/// Vanilla's own "can fall at least" check — whether a box offset by
/// `(dx, dz)` and hanging `min_height` below the feet would meet nothing.
///
/// The probe box is **not** uniformly shrunk, and getting this wrong is the
/// difference between stopping at a ledge and stopping a whole box-width early:
///
/// ```text
/// minX + 1.0E-7 + deltaX  ..  maxX - 1.0E-7 + deltaX   // inset on both sides
/// minY - minHeight - 1.0E-7  ..  minY                  // grown *downward*, top at the feet
/// minZ + 1.0E-7 + deltaZ  ..  maxZ - 1.0E-7 + deltaZ   // inset on both sides
/// ```
///
/// Horizontally it is inset (so merely *touching* a neighbouring column does not
/// count); vertically the bottom is pushed `1e-7` further **down** — an expansion,
/// not a shrink — while the top sits exactly at the feet plane. Because overlap is
/// the strict `min < max` test, the block you are standing on still registers
/// (its top face is at the box's own minimum Y, and `min_y - h - 1e-7 < min_y`
/// holds), which is what makes standing on solid ground report "cannot fall".
///
/// The consequence for the caller is that this is a **whole-box** test: it only
/// reports true once the entire inset footprint clears every collider. A sneaking
/// player therefore walks until their box is flush with the ledge and their
/// footprint has left the supporting block, not until their centre crosses it.
///
/// **Scope.** Vanilla's own no-collision check combines a block check, an
/// entity check and a world-border check. [`no_collision`] is the **block
/// half only** — this crate has no entity list and no world border — so a
/// box that vanilla would consider blocked by another entity or by the
/// border reads as free here. Same documented limitation as the fluid
/// hop-out check.
#[must_use]
fn can_fall_at_least(
    bounding_box: Aabb,
    delta_x: f64,
    delta_z: f64,
    min_height: f64,
    view: &dyn CollisionView,
) -> bool {
    // Left-to-right association preserved: `(min + 1e-7) + delta`, and
    // `(minY - minHeight) - 1e-7`.
    no_collision(
        view,
        Aabb::new(
            bounding_box.min_x + 1.0e-7 + delta_x,
            bounding_box.min_y - min_height - 1.0e-7,
            bounding_box.min_z + 1.0e-7 + delta_z,
            bounding_box.max_x - 1.0e-7 + delta_x,
            bounding_box.min_y,
            bounding_box.max_z - 1.0e-7 + delta_z,
        ),
    )
}
