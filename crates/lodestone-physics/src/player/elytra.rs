/// Advances the player by one tick, dispatching to the water, lava, or air
/// path exactly as vanilla's own travel dispatch does: water takes
/// precedence over lava, and both over air.
/// Vanilla's own look-direction unit vector.
///
/// The trig comes from vanilla's own **quantized sine/cosine table**
/// (`float`), and each component is a `float` product widened to `double` on
/// construction. The degrees→radians factor is `(float)(Math.PI / 180.0)` —
/// the division happens in `double` *then* narrows to `float`, which is a
/// different bit pattern from the input path's `(float)Math.PI / 180.0F`; we
/// mirror the exact form.
fn calculate_view_vector(pitch: f32, yaw: f32) -> Vec3d {
    let deg_to_rad = (core::f64::consts::PI / 180.0) as f32;
    let real_x_rot = pitch * deg_to_rad;
    let real_y_rot = -yaw * deg_to_rad;
    let y_cos = mth::cos(f64::from(real_y_rot));
    let y_sin = mth::sin(f64::from(real_y_rot));
    let x_cos = mth::cos(f64::from(real_x_rot));
    let x_sin = mth::sin(f64::from(real_x_rot));
    Vec3d::new(
        f64::from(y_sin * x_cos),
        f64::from(-x_sin),
        f64::from(y_cos * x_cos),
    )
}

/// Vanilla's own elytra glide update.
///
/// Preserves vanilla's exact operation order and its two distinct trig
/// sources: the look vector uses vanilla's own quantized table (`float`),
/// while the lift-force value and the nose-up lift use the real-valued
/// (`double`) `cos`/`sin`. The final drag is `multiply(0.99F, 0.98F, 0.99F)` with each
/// `float` widened to `double`.
fn update_fall_flying_movement(
    state: &PlayerState,
    profile: &PhysicsProfile,
    movement: Vec3d,
) -> Vec3d {
    let look = calculate_view_vector(state.pitch, state.yaw);
    let lean_angle = state.pitch * ((core::f64::consts::PI / 180.0) as f32);
    let look_hor_len = (look.x * look.x + look.z * look.z).sqrt();
    let move_hor_len = (movement.x * movement.x + movement.z * movement.z).sqrt();
    let gravity = effective_gravity(
        f64::from(profile.gravity),
        movement.y <= 0.0,
        state.effects.slow_falling,
    );
    // liftForce = square(cos(leanAngle)) — real double cos, not the
    // quantized table.
    let cos_lean = f64::from(lean_angle).cos();
    let lift_force = mth::square_f64(cos_lean);

    let mut mx = movement.x;
    let mut my = movement.y;
    let mut mz = movement.z;

    my += gravity * (-1.0 + lift_force * 0.75);

    if my < 0.0 && look_hor_len > 0.0 {
        let convert = my * -0.1 * lift_force;
        mx += look.x * convert / look_hor_len;
        my += convert;
        mz += look.z * convert / look_hor_len;
    }

    if lean_angle < 0.0 && look_hor_len > 0.0 {
        // -sin(leanAngle): vanilla's own quantized table again, negated and
        // widened.
        let convert = move_hor_len * f64::from(-mth::sin(f64::from(lean_angle))) * 0.04;
        mx += -look.x * convert / look_hor_len;
        my += convert * 3.2;
        mz += -look.z * convert / look_hor_len;
    }

    if look_hor_len > 0.0 {
        mx += (look.x / look_hor_len * move_hor_len - mx) * 0.1;
        mz += (look.z / look_hor_len * move_hor_len - mz) * 0.1;
    }

    Vec3d::new(
        mx * f64::from(0.99f32),
        my * f64::from(0.98f32),
        mz * f64::from(0.99f32),
    )
}

/// Vanilla's own travel-fall-flying (client path) — one tick of elytra flight.
///
/// Direction comes purely from the look angle; WASD `input` is ignored while
/// gliding (except that landing on a climbable hands control back to
/// [`tick_air`] and ends the glide, mirroring vanilla). The AI-step small-
/// velocity collapse runs first, exactly as for the other travel modes.
pub fn tick_elytra(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    tick_elytra_among_entities(state, input, view, profile, &[]);
}

fn tick_elytra_among_entities(
    state: &mut PlayerState,
    input: MovementInput,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    nearby: &[crate::push::NearbyEntity],
) {
    // onClimbable: vanilla stops fall-flying and reverts to the walking path.
    if view.is_climbable(
        mth::floor(state.position.x),
        mth::floor(state.position.y),
        mth::floor(state.position.z),
    ) {
        state.fall_flying = false;
        tick_air_among_entities(state, input, view, profile, nearby);
        return;
    }

    decrement_no_jump_delay(state);

    // AI-step velocity collapse (players use the horizontal-distance test).
    let collapsed = snap_small_velocity(state.velocity);

    state.velocity = update_fall_flying_movement(state, profile, collapsed);
    let old_y = state.position.y;
    do_move(state, view, profile, false, input.sneak, nearby);

    // Vanilla's own fall-damage bookkeeping call. Not water on this path.
    accumulate_fall_distance(state, state.position.y - old_y, false);
}

/// Vanilla's own firework-rocket glide-boost impulse — the
/// elytra speed boost from using a firework rocket while gliding.
///
/// # What this function is, and is not, responsible for
///
/// Vanilla applies this every tick a firework rocket entity is **attached**
/// to a fall-flying player. The rocket is its own entity, ticked
/// independently by the level's normal entity loop — spawning it on right-
/// click, tracking the attachment, and its own `life` counter (which
/// decides how many ticks the boost lasts before the rocket explodes) are
/// entity/item state this crate has no model of and does not attempt here.
/// A driver must spawn/track the rocket (or an equivalent per-use counter)
/// and call this once per tick for as long as vanilla's attached rocket
/// would still be ticking, with [`PlayerState::fall_flying`] already `true`
/// — this function does not check it, the same way [`tick_elytra`] is only
/// ever reached through its caller's own `fall_flying` dispatch.
///
/// **Ordering vanilla does not pin either.** The rocket ticks as an ordinary
/// entity in the level's entity-iteration order, which is not defined relative
/// to the player's own tick — so there is no "before or after `tick_elytra`"
/// answer to reproduce; call this whenever the driver's own entity-tick order
/// places the rocket.
///
/// What *is* physics, reproduced exactly: `movement.add(lookAngle * 0.1 +
/// (lookAngle * 1.5 - movement) * 0.5)`, component-wise, all in `double`
/// (no `float` narrowing anywhere in the source line). The look-angle vector
/// is vanilla's own look-angle accessor, which is [`calculate_view_vector`]
/// at the player's current pitch/yaw.
pub fn apply_firework_boost(state: &mut PlayerState) {
    let look = calculate_view_vector(state.pitch, state.yaw);
    let m = state.velocity;
    state.velocity = m.add(Vec3d::new(
        look.x * 0.1 + (look.x * 1.5 - m.x) * 0.5,
        look.y * 0.1 + (look.y * 1.5 - m.y) * 0.5,
        look.z * 0.1 + (look.z * 1.5 - m.z) * 0.5,
    ));
}

/// Vanilla's own box-collided-along-vector check, specialised to one unit
/// cell and to a **boolean** result: vanilla returns the hit point
/// (`Optional<Vec3>`), but every caller here only ever asks "did it collide" —
/// vanilla's own "collided with shape moving from" check immediately reduces
/// it to `.isPresent()`-shaped logic, and that is the only vanilla caller
/// relevant to the blocks this module's swept scan cares about (every
/// stuck-multiplier and powder-snow cell presents a solid-block shape to
/// vanilla's own "get entity-inside collision shape" query, so its own
/// "inside block" step reduces to exactly this segment test).
///
/// `from`/`to` are the entity's **box centres** at the start and end of the
/// move — equal to vanilla's own box-collided-along-vector's own
/// `this.getCenter()` / `from.add(vector)`, since `vector = to_pos - from_pos`
/// and the box is rigidly attached to the entity position, so translating
/// the centre by the position delta is the same point as the centre of a
/// box built at the new position. `half` is the entity's own half-extents
/// (`getXsize() * 0.5` etc.).
///
/// Implemented as the standard box-slab test rather than porting vanilla's
/// own face-by-face walk: the two compute the same boolean (intersect /
/// no-intersect) over the same inflated box, and no caller here reads
/// *which* face or *where* the hit point was — the one thing vanilla's own
/// walk computes that this does not.
fn segment_hits_cell(from: Vec3d, to: Vec3d, half: Vec3d, x: i32, y: i32, z: i32) -> bool {
    // `inflate(size * 0.5 - 1.0E-7)`.
    const EPS: f64 = 1.0e-7;
    let min_x = f64::from(x) - (half.x - EPS);
    let max_x = f64::from(x) + 1.0 + (half.x - EPS);
    let min_y = f64::from(y) - (half.y - EPS);
    let max_y = f64::from(y) + 1.0 + (half.y - EPS);
    let min_z = f64::from(z) - (half.z - EPS);
    let max_z = f64::from(z) + 1.0 + (half.z - EPS);

    let d = to.subtract(from);
    let mut t_min = 0.0f64;
    let mut t_max = 1.0f64;
    for (d_a, from_a, lo, hi) in [
        (d.x, from.x, min_x, max_x),
        (d.y, from.y, min_y, max_y),
        (d.z, from.z, min_z, max_z),
    ] {
        if d_a.abs() < EPS {
            // A stationary (or single-axis-flat) segment: the boolean test
            // degenerates to plain containment on this axis, which subsumes
            // vanilla's separate `inflated.contains(to) || inflated.contains(from)`
            // pre-check — no need to special-case `from == to` above this loop.
            if from_a < lo || from_a > hi {
                return false;
            }
        } else {
            let inv = 1.0 / d_a;
            let (t1, t2) = {
                let a = (lo - from_a) * inv;
                let b = (hi - from_a) * inv;
                if a <= b { (a, b) } else { (b, a) }
            };
            t_min = t_min.max(t1);
            t_max = t_max.min(t2);
            if t_min > t_max {
                return false;
            }
        }
    }
    true
}

/// Enumerates the cells vanilla's own "check inside blocks" sweep visits,
/// for every consumer of that one vanilla sweep in this module
/// ([`update_stuck_multiplier`], [`update_freezing`]).
///
/// Vanilla walks a 3D DDA from `from` to `to` (its own "for each block
/// intersected between" walk, capped at 16 iterations) and tests each
/// visited cell with [`segment_hits_cell`]-equivalent logic. Porting the DDA
/// itself buys nothing a
/// caller here needs — every block these two functions ask about
/// (`stuck_multiplier`, `is_powder_snow`) is a single-cell yes/no answer, order-
/// independent for the uniform volumes they occupy (see
/// [`update_stuck_multiplier`]'s "last one wins" note). So this scans the
/// **union** of the pre- and post-move bounding boxes instead: a conservative
/// superset of the cells the DDA would reach (same start box, same end box,
/// every intermediate cell along a straight segment between two boxes is
/// spatially between them), narrowed to the cells the entity's box *actually*
/// swept through via [`segment_hits_cell`] rather than accepted just for being
/// in that superset's bounds.
///
/// This is what closes the defect the resting-box-only version had: a mover
/// fast enough to pass *through* a thin stuck-in-block layer or powder-snow
/// pocket without ever resting inside it is now caught, because the swept
/// centre-to-centre segment crosses the cell even though neither endpoint box
/// does.
fn for_each_swept_cell(old_bb: Aabb, new_bb: Aabb, mut f: impl FnMut(i32, i32, i32)) {
    let half = Vec3d::new(
        (new_bb.max_x - new_bb.min_x) * 0.5,
        (new_bb.max_y - new_bb.min_y) * 0.5,
        (new_bb.max_z - new_bb.min_z) * 0.5,
    );
    let from_center = Vec3d::new(
        (old_bb.min_x + old_bb.max_x) * 0.5,
        (old_bb.min_y + old_bb.max_y) * 0.5,
        (old_bb.min_z + old_bb.max_z) * 0.5,
    );
    let to_center = Vec3d::new(
        (new_bb.min_x + new_bb.max_x) * 0.5,
        (new_bb.min_y + new_bb.max_y) * 0.5,
        (new_bb.min_z + new_bb.max_z) * 0.5,
    );
    // Same `1.0e-5` deflation as the resting-box version this replaces, and as
    // `apply_bubble_column` still uses independently — see that function's
    // "Divergences" note for why the constant is shared but the enumeration no
    // longer is.
    let min_x = mth::floor(old_bb.min_x.min(new_bb.min_x) + 1.0e-5);
    let max_x = mth::floor(old_bb.max_x.max(new_bb.max_x) - 1.0e-5);
    let min_y = mth::floor(old_bb.min_y.min(new_bb.min_y) + 1.0e-5);
    let max_y = mth::floor(old_bb.max_y.max(new_bb.max_y) - 1.0e-5);
    let min_z = mth::floor(old_bb.min_z.min(new_bb.min_z) + 1.0e-5);
    let max_z = mth::floor(old_bb.max_z.max(new_bb.max_z) - 1.0e-5);
    for x in min_x..=max_x {
        for y in min_y..=max_y {
            for z in min_z..=max_z {
                if segment_hits_cell(from_center, to_center, half, x, y, z) {
                    f(x, y, z);
                }
            }
        }
    }
}

/// Vanilla's own "check inside blocks" sweep → per-block "entity inside"
/// effect → "make stuck in block": after the tick's movement, record the
/// stuck-speed multiplier of whatever block the swept movement segment
/// intersects, for the *next* move to consume. This is what produces the
/// observable one-tick lag between entering a cobweb and being grabbed by
/// it.
///
/// **Sweeps the segment, via [`for_each_swept_cell`], rather than sampling only
/// the final resting position.** The old version floored/ceiled the
/// post-move box alone, so a mover fast enough to pass through a cobweb or
/// powder-snow cell without ending inside it took no impulse at all — the
/// resting-box-only approximation the `apply_bubble_column` "Divergences" note
/// used to describe as deliberately shared between the two. It no longer is;
/// see that note.
///
/// Blocks are *assigned* in vanilla, not accumulated, so the last intersected
/// block wins; for the uniform volumes these blocks form, iteration order is
/// immaterial.
fn update_stuck_multiplier(
    state: &mut PlayerState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    pre_move_position: Vec3d,
) {
    let new_bb = state.bounding_box(profile);
    let old_bb = state.dimensions().bounding_box(pre_move_position);
    let mut found = Vec3d::ZERO;
    for_each_swept_cell(old_bb, new_bb, |x, y, z| {
        if let Some(m) = view.stuck_multiplier(x, y, z) {
            found = m;
        }
    });
    // Vanilla's own "make stuck in block" step resets the fall-distance
    // accumulator and sets the stuck-speed multiplier — the reset rides
    // along with every call that finds a stuck-triggering block (cobweb,
    // powder snow, sweet berry bush, honey), which is every tick vanilla's
    // own per-block "entity inside" effect sees one, not just the first.
    if found != Vec3d::ZERO {
        state.fall_distance = 0.0;
    }
    state.stuck_speed_multiplier = found;
}

/// Vanilla's own per-tick freezing block plus the increment half of its
/// powder-snow "entity inside" freeze effect, reached via the same "check
/// inside blocks" sweep [`update_stuck_multiplier`] models:
///
/// ```text
/// freeze effect, applied once per tick the swept segment finds powder snow:
///   mark "in powder snow";
///   if not freeze-immune, advance the frozen-ticks counter toward its cap;
///
/// end-of-tick, unconditional:
///   if not in powder snow, or freeze-immune, decay the frozen-ticks counter by 2;
/// ```
///
/// Collapsed into one function because this crate has no per-tick "flag set
/// earlier, read later" bookkeeping to spare: `is_in_powder_snow` is computed
/// and consumed in the same call, which is equivalent since nothing else reads
/// it between the two vanilla call sites.
///
/// Vanilla's own "can freeze" check is its own "not tagged freeze-immune"
/// check; the player entity type is never a member (only certain mobs —
/// striders, blazes, snow golems and others — are), so it is unconditionally
/// `true` for every [`PlayerState`] and not modelled as a field.
///
/// **What this does not do: apply freeze damage.** Vanilla's own trigger —
/// every 40 ticks, when fully frozen and able to freeze — needs an
/// absolute server tick count this
/// crate has no notion of — every other timer here is a countdown
/// ([`PlayerState::no_jump_delay`], [`PlayerState::fall_distance`]'s resets),
/// never a count against a global clock, and this crate applies no damage
/// anywhere (fall damage is server-owned too; see [`PlayerState::fall_distance`]'s
/// doc). [`PlayerState::should_apply_freeze_damage`] exposes the *predicate* —
/// correct, tested, and ready for a driver that does own a tick counter — so
/// the only missing piece is the counter itself, not the rule.
fn update_freezing(
    state: &mut PlayerState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
    pre_move_position: Vec3d,
) {
    let new_bb = state.bounding_box(profile);
    let old_bb = state.dimensions().bounding_box(pre_move_position);
    let mut in_powder_snow = false;
    for_each_swept_cell(old_bb, new_bb, |x, y, z| {
        if view.is_powder_snow(x, y, z) {
            in_powder_snow = true;
        }
    });
    state.frozen_ticks = if in_powder_snow {
        (state.frozen_ticks + 1).min(PlayerState::TICKS_REQUIRED_TO_FREEZE)
    } else {
        state.frozen_ticks.saturating_sub(2)
    };
}

/// Vanilla's own bubble-column block-inside effect → its own inside/above
/// bubble-column impulses — the soul-sand lift and the magma-block drain.
///
/// # The four constants, and the branch that picks between them
///
/// Vanilla decides *capped or not* per cell, in its own bubble-column
/// block-inside effect: the cell **above** this one is inspected, and
/// if its collision shape is empty **and** its fluid state is empty — i.e. it is
/// open air — the entity is "above" the column and gets the stronger, surface-
/// launch pair. Anything else (more column, water, a solid lid) is the "inside"
/// pair.
///
/// | | `drag=false` (soul sand, up) | `drag=true` (magma, down) |
/// |---|---|---|
/// | inside | `min(0.7, vy + 0.06)` | `max(-0.3, vy - 0.03)` |
/// | above (air over the cell) | `min(1.8, vy + 0.1)` | `max(-0.9, vy - 0.03)` |
///
/// All four are `double` arithmetic on the vector's Y component against
/// `double` literals, so there is no `f32` narrowing anywhere in this
/// function.
///
/// Note the asymmetry in the drag-down column: the **step** is `-0.03` in both
/// rows and only the *clamp* differs (`-0.3` inside, `-0.9` above). It is the
/// push-up column whose step changes too (`+0.06` → `+0.1`). Transcribing this as
/// "the above case is three times stronger" would be wrong in one of the two
/// columns, which is why the table is written out rather than summarised.
///
/// # One impulse *per cell*, not per tick
///
/// This is the part that surprises. Vanilla's own "check inside blocks"
/// sweep visits every block the movement intersects, dedupes by *position*
/// (its own visited-blocks set), and calls
/// its own per-block "entity inside" effect on each — and vanilla's own
/// bubble-column block applies its impulse **immediately**, inside that
/// callback, rather than deferring it to the effect-applier the way fire
/// and freezing do. So a standing player
/// (a `1.8`-high box) inside a column spans two cells and takes **two** impulses
/// every tick. The clamp is what keeps that from diverging: repeated
/// `min(0.7, vy + 0.06)` converges on `0.7` instead of running away. Applying the
/// impulse once per tick would reach the same terminal velocity but climb to it at
/// half the rate, which is exactly the kind of sub-tick divergence the server's
/// movement check accumulates.
///
/// # The fall-distance reset applies on `inside` only
///
/// Vanilla's own inside-bubble-column handler ends with its own fall-distance
/// reset; its own above-bubble-column handler ends
/// with sending bubble-column particles and **no** reset. The asymmetry is
/// reproduced here faithfully, but **no test pins it, because it is currently
/// unobservable and cannot honestly be made observable.** A bubble column *is* a
/// water source cell, so any player this function
/// finds in one has already been through [`tick_water`], which zeroes
/// `fall_distance` unconditionally at its top. The only world that would separate
/// the two is a bubble column in a cell that does not hold water — which vanilla
/// cannot represent, so a test fixture asserting it would be the *world* species of
/// vacuous test: green, and measuring a scene the game cannot produce. The
/// asymmetry is coded correctly so that it is already right if the classification
/// ever changes; it is deliberately unproven until something can see it.
///
/// # Divergences, deliberate
///
/// * **Cell enumeration is the post-move box, not the swept path — no longer
///   shared with [`update_stuck_multiplier`].** Vanilla walks its own
///   "for each block intersected between" walk, so a player moving fast enough
///   to pass *through* a column cell without ending inside it still takes that
///   cell's impulse. This function still enumerates the cells of the post-move
///   bounding box only. **This used to be "exactly the approximation
///   `update_stuck_multiplier` makes for the same vanilla call", deliberately kept
///   consistent; it no longer is.** A later fix gave `update_stuck_multiplier`
///   (and [`update_freezing`]) a real swept-segment test via
///   [`for_each_swept_cell`], because a fast mover grazing a cobweb or a thin
///   powder-snow layer without resting inside it is the exact defect a resting-box
///   sample cannot see — and bubble columns were left as they were, since that
///   fix was scoped to those two and the case this function serves (a player
///   *riding* a column, displacement at most `0.7`/tick, well under a cell) does
///   not need it. So this is now a **known, not a deliberately mirrored**,
///   approximation — worth revisiting with the same fix if a launched-through-a-
///   column case ever needs it.
/// * **The deflation constant is `1.0e-5`, not vanilla's widened `1.0E-5F`.**
///   Vanilla deflates the target box by a *float* literal, which widens to
///   `1.0000000116860974e-5`. This function uses the `f64` `1.0e-5` that
///   [`update_stuck_multiplier`] and [`for_each_swept_cell`] also use, for the
///   same reason: they all model the same vanilla sweep and must not gratuitously
///   disagree on the constant, even though the enumeration itself has now
///   diverged. The difference from vanilla's widened float can only change an
///   answer when a box edge lies within `1.2e-13` of a cell boundary.
/// * **The player's `!abilities.flying` gate is not applied.** Both overrides
///   skip the impulse entirely for a flying player. This
///   crate has no abilities state, so the conjunct is vacuously true — see
///   [`tick`]'s note. A driver with a flight mode must not route a flying player
///   through [`tick`].
fn apply_bubble_column(
    state: &mut PlayerState,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    let bb = state.bounding_box(profile);
    // Same cell enumeration as `update_stuck_multiplier` — see this function's
    // "Divergences" note for why the two are deliberately identical.
    let min_x = mth::floor(bb.min_x + 1.0e-5);
    let max_x = mth::floor(bb.max_x - 1.0e-5);
    let min_y = mth::floor(bb.min_y + 1.0e-5);
    let max_y = mth::floor(bb.max_y - 1.0e-5);
    let min_z = mth::floor(bb.min_z + 1.0e-5);
    let max_z = mth::floor(bb.max_z - 1.0e-5);
    for x in min_x..=max_x {
        for y in min_y..=max_y {
            for z in min_z..=max_z {
                let Some(drag_down) = view.bubble_column(x, y, z) else {
                    continue;
                };
                let vy = state.velocity.y;
                if cell_is_open_air(view, x, y + 1, z) {
                    // Vanilla's own above-bubble-column handler. The particle
                    // burst is server-side only and carries no motion, so
                    // nothing is owed here; note it does *not* reset fall
                    // distance.
                    state.velocity.y = if drag_down {
                        (vy - 0.03).max(-0.9)
                    } else {
                        (vy + 0.1).min(1.8)
                    };
                } else {
                    // Vanilla's own inside-bubble-column handler.
                    state.velocity.y = if drag_down {
                        (vy - 0.03).max(-0.3)
                    } else {
                        (vy + 0.06).min(0.7)
                    };
                    state.fall_distance = 0.0;
                }
            }
        }
    }
}

/// Vanilla's own bubble-column "nothing above" test:
/// `stateAbove.getCollisionShape(…).isEmpty() && stateAbove.getFluidState().isEmpty()`.
///
/// The fluid half is what makes the common cases fall out correctly without any
/// special-casing: a bubble column's own fluid state is a **water source**,
/// and this engine classifies the block as water for the same reason
/// (`docs/fluid-classification.md`'s `UNCONDITIONAL_WATER_BLOCKS`), so a cell
/// with more column above it reports `false` here and takes the *inside*
/// branch. Only the cell at the very top of a column, with real air over
/// it, reports `true`.
///
/// **One vanilla quirk deliberately not reproduced.** Vanilla calls
/// `stateAbove.getCollisionShape(level, pos)` — passing `pos`, the *lower* cell,
/// while asking the *upper* cell's state. It is a genuine mismatch in the game's
/// own source, not a transcription slip here, and it is unobservable for every
/// block whose shape does not vary with position. This seam is position-keyed and
/// cannot express the quirk anyway.
fn cell_is_open_air(view: &dyn CollisionView, x: i32, y: i32, z: i32) -> bool {
    let mut boxes = Vec::new();
    view.collision_boxes(x, y, z, &mut boxes);
    boxes.is_empty() && !view.is_water(x, y, z) && !view.is_lava(x, y, z)
}
