//! Client-side item/projectile integration and collision-aware motion ticks.

use super::*;

/// A dropped item's collision hitbox: vanilla's own item entity type is
/// sized 0.25×0.25, and `ItemEntity` (not a `LivingEntity`) never overrides
/// vanilla's own max-up-step accessor, whose base implementation returns `0.0F` — items do
/// not auto-step at all.
const ITEM_DIMENSIONS: EntityDimensions = EntityDimensions::new(0.25, 0.25, 0.0);

/// Vanilla's own item below-position lookup — the block an item
/// reads ground friction from. This overrides the base entity's
/// on-pos lookup at a 0.500001 offset with one at 0.999999: an item's 0.25-tall
/// hitbox sits *inside* the block it rests on, not straddling the block
/// below, so the friction sample has to reach almost a full block down, not
/// half of one. Reproduced here (rather than reused from
/// `lodestone_physics::player::friction_block`) because that helper is
/// `pub(crate)` to the physics crate *and* bakes in the different,
/// generic-entity `0.500001F` offset — the wrong constant for an item even if
/// it were reachable.
fn item_friction_block(position: Vec3d) -> (i32, i32, i32) {
    (
        mth::floor(position.x),
        mth::floor(position.y - f64::from(0.999_999_f32)),
        mth::floor(position.z),
    )
}

/// One tick of a dropped item's *own* client-run physics, run against a real
/// [`CollisionView`] instead of [`ItemMotion::tick`]'s bare `position +=
/// velocity`. Without this an airborne item only ever gets a floor from the
/// server's own once-a-second correction (vanilla's own item entity type's
/// 20-tick update interval) and visibly sinks through terrain in between — the
/// gap the module docs on [`ItemPhysics`] call out.
///
/// Mirrors vanilla's own item-entity tick's real order, traced against
/// the 26.2 decompile:
/// gravity is subtracted from `velocity.y` *before* the move
/// (vanilla's own apply-gravity); [`move_entity`] is vanilla's own
/// generic self-move step — the single shared collider
/// this crate must not fork, per the module's own architecture note — and it
/// both commits the collided position and derives this tick's authoritative
/// `on_ground` (not last tick's stale server-reported value, which is what
/// `on_ground` held before this fix); drag is then applied to the
/// *post-collision* velocity exactly as vanilla's own item-entity tick re-reads
/// its own velocity accessor after the move step, using the real block friction under
/// the item via [`CollisionView::friction`] rather than [`ItemMotion`]'s
/// unqueried constant; and the `-0.5` landing bounce follows drag, matching
/// vanilla's tail end of the branch exactly.
///
/// `profile` supplies only the land-bounce threshold's gravity term inside
/// [`move_entity`] (relevant solely if the item ever rests on a bouncy block
/// like slime) — items have their own gravity/drag constants
/// ([`ITEM_GRAVITY`]/[`ITEM_AIR_DRAG`]) applied explicitly here, so passing
/// the player's [`PhysicsProfile`] does not mix the two up for the fall
/// itself, only for that one rare edge case.
///
/// **This stays a plain function, called by a system rather than being one**,
/// for the same reason `docs/bevy-migration.md` §8 keeps `lodestone-physics` a
/// library: it is the vanilla-constant carrier, and its per-tick trace is what
/// the tests below pin.
pub(crate) fn step_item_physics(
    sim: &mut ItemMotion,
    view: &dyn CollisionView,
    profile: &PhysicsProfile,
) {
    // Vanilla's own item-entity apply-gravity, before the move.
    sim.velocity.y -= ITEM_GRAVITY;

    let mut motion = EntityMotion {
        position: to_physics_vec3d(sim.position),
        velocity: to_physics_vec3d(sim.velocity),
        on_ground: sim.on_ground,
        horizontal_collision: false,
        stuck_speed_multiplier: Vec3d::ZERO,
    };
    // The shared collision core, not a second one — `MoveContext::default()`
    // matches an item: never Slow Falling, never bounce-suppressing (that
    // flag is the sneaking-player case, `LivingEntity`-only).
    move_entity(
        &mut motion,
        ITEM_DIMENSIONS,
        view,
        profile,
        MoveContext::default(),
    );

    // Drag on the post-collision velocity, exactly as vanilla's own
    // item-entity tick scales its own velocity accessor after the move step returns.
    let mut ground_friction = ITEM_AIR_DRAG;
    if motion.on_ground {
        let (fx, fy, fz) = item_friction_block(motion.position);
        ground_friction *= f64::from(view.friction(fx, fy, fz));
    }
    motion.velocity.x *= ground_friction;
    motion.velocity.z *= ground_friction;
    motion.velocity.y *= ITEM_AIR_DRAG;
    if motion.on_ground && motion.velocity.y < 0.0 {
        motion.velocity.y *= -0.5;
    }

    sim.position = from_physics_vec3d(motion.position);
    sim.velocity = from_physics_vec3d(motion.velocity);
    sim.on_ground = motion.on_ground;
}

/// A [`CollisionView`] with no collision boxes anywhere — open air forever.
///
/// Backs [`EntityInterpolator::update`]'s no-world-known default so every
/// existing caller (tests, and any future offline/no-net path) keeps the
/// pre-collision free-fall behaviour unchanged. The live path does not use
/// this: since §4.1(c) `crate::sim::Sim` inserts an [`ItemCollision`] resource
/// built from the player's loaded chunks before each `GameTick` run.
#[derive(Debug)]
pub(crate) struct OpenAir;

impl CollisionView for OpenAir {
    fn collision_boxes(&self, _x: i32, _y: i32, _z: i32, _out: &mut Vec<lodestone_physics::Aabb>) {}
}

/// [`OpenAir`] as a [`CollisionSource`]: it owns nothing, so the borrow
/// [`CollisionSource::with_view`] hands out is trivially satisfied by lending
/// back the same zero-sized value.
impl CollisionSource for OpenAir {
    fn with_view(&self, f: &mut dyn FnMut(&dyn CollisionView)) {
        f(self);
    }
}

/// `GameTick` / `TickSet::Physics`: one 20 Hz step of every dropped item's own
/// ballistic physics, re-anchoring each one's render ease onto the freshly
/// simulated point.
///
/// # Why this took until now to become a system
///
/// A `bevy_ecs` system reads its inputs from `Resource`s, and a `Resource`
/// must be `'static`. Before
/// [`lodestone_ecs::player::CollisionSource`] existed, the collision geometry
/// reached this function as a borrowed `&dyn CollisionView` whose owner was a
/// local in `Sim::update_entities` (`WorldCollision::new(&self.world)` borrows
/// the chunk world outright) — there was no safe way to put that borrow in a
/// resource, and the workspace denies `unsafe_code`.
///
/// `CollisionSource` inverts it: the *trait object* is `'static` because an
/// implementor owns whatever it borrows from, and only the `&dyn
/// CollisionView` handed to [`CollisionSource::with_view`]'s callback is
/// short-lived. [`EntityInterpolator::update_with_view`] inserts a
/// [`PlayerCollision`] (holding that `Arc<dyn CollisionSource>` in its `View`
/// variant) as a resource before running the tick loop, which is what makes
/// this reachable from `GameTick` at all.
///
/// The absence of [`ItemPhysics`] is still the switch that keeps every other
/// entity type on the pure position ease — the query below only ever matches
/// entities that have all four components, which [`spawn_track`] inserts
/// atomically.
pub fn tick_item_physics(
    collision: Res<ItemCollision>,
    profile: Res<Profile>,
    mut items: Query<(
        &mut ItemPhysics,
        &mut InterpFrom,
        &mut InterpTo,
        &mut InterpClock,
    )>,
) {
    // `NoWorld`/`Pending` mean there is nothing to collide against yet — leave
    // every item's simulation exactly where it was rather than free-falling it
    // through geometry we cannot query.
    let PlayerCollision::View(source) = &collision.0 else {
        return;
    };
    let profile = &profile.0;
    source.with_view(&mut |view| {
        for (mut physics, mut from, mut to, mut clock) in &mut items {
            // Paused while the last *server* report says the item is resting;
            // the floor within a tick is real collision, not a frozen flag.
            if physics.grounded {
                continue;
            }
            step_item_physics(&mut physics.sim, view, profile);
            let simulated = to_glam_vec3(physics.sim.position);

            // Re-anchor exactly like a fresh authoritative snapshot would: ease
            // from wherever this frame is currently drawn toward the freshly
            // simulated point, so the simulation reads as continuous motion
            // rather than a series of per-tick snaps.
            let drawn = render_feet(&from, &to, &clock);
            from.feet = drawn;
            to.feet = simulated;
            clock.t = 0.0;
        }
    });
}

/// Vanilla's own arrow-entity tick recomputes its entity rotations from the
/// current delta
/// movement every tick. These are projectile angles (`atan2(x, z)` and positive
/// pitch while rising), not the player's camera yaw/pitch convention.
fn projectile_angles(velocity: lodestone_model::Vec3) -> (f32, f32) {
    let horizontal = velocity.x.hypot(velocity.z);
    (
        velocity.x.atan2(velocity.z).to_degrees() as f32,
        velocity.y.atan2(horizontal).to_degrees() as f32,
    )
}

/// `GameTick` / `TickSet::Physics`: one vanilla arrow-entity tick step for
/// each tracked arrow, spectral arrow, or trident. Their network update interval
/// is twenty ticks, so this keeps the render pose moving between corrections.
pub fn tick_projectile_physics(
    mut projectiles: Query<(
        &mut ProjectilePhysics,
        &mut InterpFrom,
        &mut InterpTo,
        &mut InterpClock,
    )>,
) {
    for (mut physics, mut from, mut to, mut clock) in &mut projectiles {
        if physics.grounded {
            continue;
        }
        physics.sim.tick();
        let simulated = to_glam_vec3(physics.sim.position());
        let (yaw, pitch) = projectile_angles(physics.sim.velocity());

        let drawn = render_feet(&from, &to, &clock);
        from.feet = drawn;
        from.yaw = render_yaw(&from, &to, &clock);
        from.head_yaw = render_head_yaw(&from, &to, &clock);
        from.pitch = render_pitch(&from, &to, &clock);
        to.feet = simulated;
        to.yaw = yaw;
        to.head_yaw = yaw;
        to.pitch = pitch;
        clock.t = 0.0;
    }
}

/// Seeds a fresh [`ItemPhysics`] from an item entity's first-seen (or freshly
/// re-anchored) snapshot. A missing velocity seeds zero — gravity still applies
/// to it, it just has nothing to arc with, which is exactly the discriminating
/// behaviour the hermetic tests below pin.
pub(super) fn new_item_physics(snap: &EntityFacts) -> ItemPhysics {
    let mut sim = ItemMotion::new(
        to_model_vec3(snap.feet),
        snap.velocity.map(to_model_vec3).unwrap_or_default(),
    );
    sim.on_ground = snap.on_ground;
    ItemPhysics {
        sim,
        last_reported: snap.feet,
        grounded: snap.on_ground,
    }
}

/// Seeds the local projectile simulation from the first authoritative report.
pub(super) fn new_projectile_physics(snap: &EntityFacts) -> ProjectilePhysics {
    let position = to_model_vec3(snap.feet);
    let velocity = snap.velocity.map(to_model_vec3).unwrap_or_default();
    let sim = if is_accelerating_projectile(snap.entity_type) {
        ProjectileMotion::Accelerating(AcceleratingProjectile::new(
            position,
            velocity,
            snap.projectile_power
                .unwrap_or_else(|| default_projectile_power(snap.entity_type)),
            projectile_inertia(snap.entity_type),
        ))
    } else {
        ProjectileMotion::Ballistic(Projectile::arrow(position, velocity))
    };
    ProjectilePhysics {
        sim,
        last_reported: snap.feet,
        last_reported_velocity: snap.velocity,
        last_reported_power: snap.projectile_power,
        grounded: snap.on_ground,
    }
}
