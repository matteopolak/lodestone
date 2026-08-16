//! Ballistic (non-mob) projectile trajectories.
//!
//! Projectiles move by a fixed three-step integration each server tick — apply
//! gravity, apply drag (a scalar "inertia" multiply on the whole velocity), and
//! translate by the velocity — but **the order of those steps and the constants
//! differ by projectile family**, and getting the order wrong produces a
//! trajectory that looks plausible and drifts a few centimetres per tick until,
//! forty ticks later, the arrow lands in the wrong block.
//!
//! Two families cover everything here:
//!
//! * **Throwables** (snowball, egg, ender pearl, potion, experience bottle):
//!   gravity `0.03`, air inertia `0.99`, water inertia `0.8`, integrated
//!   **gravity → drag → move** ([`ThrowableProjectile.tick`]).
//! * **Arrows** (arrow, spectral arrow, trident): gravity `0.05`, air inertia
//!   `0.99`, water inertia `0.6`, integrated **move → drag → gravity**
//!   ([`AbstractArrow.tick`]). Note the different order *and* that in water the
//!   drag is applied **before** the move, not after — the [`Projectile::tick`]
//!   here models the common in-air path exactly and the in-water path to the
//!   same constants.
//!
//! The maths is short, exact and free of server-side RNG, which makes it the one
//! part of the non-mob entity layer that can be verified **bit-for-bit against
//! the live server**: summon an arrow with a known `Motion`, read its `Pos` from
//! NBT each tick, and compare (see `tests/live_projectile.rs`).

use lodestone_model::Vec3;

/// The scalar velocity multiplier ("inertia") applied each tick, split by medium.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DragProfile {
    /// Multiplier applied in air.
    pub air: f64,
    /// Multiplier applied while submerged in a fluid.
    pub water: f64,
}

/// The order in which a projectile family applies its per-tick steps. The two
/// vanilla families disagree, so the order is data, not a hardcoded sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntegrationOrder {
    /// Throwables: subtract gravity, scale by drag, then translate.
    GravityDragMove,
    /// Arrows: translate, scale by drag, then subtract gravity.
    MoveDragGravity,
}

/// A ballistic projectile with no steering. One [`Projectile::tick`] advances it
/// exactly one server tick.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Projectile {
    /// Current position.
    pub position: Vec3,
    /// Current velocity (vanilla `deltaMovement`), in blocks per tick.
    pub velocity: Vec3,
    /// Downward acceleration applied each tick.
    pub gravity: f64,
    /// The drag multiplier, split by medium.
    pub drag: DragProfile,
    /// Whether the projectile is currently submerged (selects `drag.water`).
    pub in_water: bool,
    /// The family's step ordering.
    pub order: IntegrationOrder,
}

impl Projectile {
    /// A throwable projectile (snowball / egg / ender pearl / thrown potion):
    /// gravity `0.03`, air drag `0.99`, water drag `0.8`, gravity-first order.
    #[must_use]
    pub fn throwable(position: Vec3, velocity: Vec3) -> Self {
        Self {
            position,
            velocity,
            gravity: 0.03,
            drag: DragProfile {
                air: 0.99,
                water: 0.8,
            },
            in_water: false,
            order: IntegrationOrder::GravityDragMove,
        }
    }

    /// A snowball. Alias for [`Projectile::throwable`].
    #[must_use]
    pub fn snowball(position: Vec3, velocity: Vec3) -> Self {
        Self::throwable(position, velocity)
    }

    /// An ender pearl. Same ballistics as any throwable.
    #[must_use]
    pub fn ender_pearl(position: Vec3, velocity: Vec3) -> Self {
        Self::throwable(position, velocity)
    }

    /// An arrow / spectral arrow / trident: gravity `0.05`, air drag `0.99`,
    /// water drag `0.6`, move-first order.
    #[must_use]
    pub fn arrow(position: Vec3, velocity: Vec3) -> Self {
        Self {
            position,
            velocity,
            gravity: 0.05,
            drag: DragProfile {
                air: 0.99,
                water: 0.6,
            },
            in_water: false,
            order: IntegrationOrder::MoveDragGravity,
        }
    }

    fn drag_now(&self) -> f64 {
        if self.in_water {
            self.drag.water
        } else {
            self.drag.air
        }
    }

    fn apply_gravity(&mut self) {
        self.velocity.y -= self.gravity;
    }

    fn apply_drag(&mut self) {
        self.velocity = self.velocity.scale(self.drag_now());
    }

    fn apply_move(&mut self) {
        self.position += self.velocity;
    }

    /// Advances one server tick, mutating [`position`](Self::position) and
    /// [`velocity`](Self::velocity) in place, honouring the family's step order.
    pub fn tick(&mut self) {
        match self.order {
            IntegrationOrder::GravityDragMove => {
                self.apply_gravity();
                self.apply_drag();
                self.apply_move();
            }
            IntegrationOrder::MoveDragGravity => {
                self.apply_move();
                self.apply_drag();
                self.apply_gravity();
            }
        }
    }

    /// Advances `n` server ticks.
    pub fn tick_n(&mut self, n: u32) {
        for _ in 0..n {
            self.tick();
        }
    }

    /// The horizontal (xz) speed this tick, in blocks per tick.
    #[must_use]
    pub fn horizontal_speed(&self) -> f64 {
        self.velocity.x.hypot(self.velocity.z)
    }
}

// ---------------------------------------------------------------------------
// Launch: turning a facing and a charge into an initial velocity
// ---------------------------------------------------------------------------

/// `BowItem.MAX_DRAW_DURATION`, the ticks of draw at which a bow reaches full
/// power.
pub const BOW_MAX_DRAW_TICKS: i32 = 20;

/// `BowItem.releaseUsing`'s `pow * 3.0F` — the multiplier from normalised bow
/// power to blocks per tick.
pub const BOW_ARROW_SPEED: f64 = 3.0;

/// `BowItem.releaseUsing`'s `if (pow < 0.1) return false` — below this the shot is
/// not taken at all.
pub const BOW_MIN_POWER: f64 = 0.1;

/// `SnowballItem.PROJECTILE_SHOOT_POWER` / `EggItem` / `EnderpearlItem` — all
/// three throw at `1.5` blocks per tick, with no charge.
pub const THROWABLE_SHOOT_POWER: f64 = 1.5;

/// `ThrowablePotionItem.PROJECTILE_SHOOT_POWER` — a thrown potion is slower than
/// a snowball, and is the one throwable with a non-zero pitch offset.
pub const POTION_SHOOT_POWER: f64 = 0.5;

/// `ThrowablePotionItem`'s `spawnProjectileFromRotation(..., -20.0F, 0.5F, 1.0F)`
/// pitch offset, which is what makes a thrown potion arc upward out of the hand
/// instead of travelling flat.
pub const POTION_PITCH_OFFSET: f64 = -20.0;

/// Vanilla `BowItem.getPowerForTime`: `pow = t / 20; pow = (pow² + 2·pow) / 3;`
/// clamped above at `1.0`.
///
/// **Not linear**, and that is the whole character of a bow: the curve is
/// deliberately slow at the start and fast at the end, so a half-second draw is
/// worth far less than half a shot. At `10` ticks a linear reading would give
/// `0.5` and this gives `0.4166…`; the difference is a whole point of impact
/// damage after `ceil`.
#[must_use]
pub fn bow_power_for_time(ticks_held: i32) -> f64 {
    let pow = f64::from(ticks_held.max(0)) / f64::from(BOW_MAX_DRAW_TICKS);
    ((pow * pow + pow * 2.0) / 3.0).min(1.0)
}

/// Vanilla `Projectile.shootFromRotation` composed with `getMovementToShoot`: the
/// initial velocity for a projectile launched by an entity facing
/// `(yaw, pitch)` in degrees, at `power` blocks per tick.
///
/// `pitch_offset` is vanilla's `yOffset`, applied to the **vertical component
/// only** — `0.0` for everything except a thrown potion's `-20.0`. Applying it to
/// the horizontal components as well (the obvious mis-read, since it looks like a
/// rotation) would turn a potion's upward arc into a sideways one.
///
/// The inaccuracy term is deliberately absent: vanilla adds
/// `random.triangle(0.0, 0.0172275 * uncertainty)` per axis before scaling, which
/// needs `RandomSource.triangle`'s exact distribution *and* its exact draw order
/// to reproduce. A deterministic launch is also what lets a gate predict the value
/// rather than assert a direction. This is the same disclosed simplification
/// `ai::mob::ProjectileLaunch::aimed` already carries, stated here too because
/// this is the player's path and nothing forces a reader through that one.
#[must_use]
pub fn launch_velocity(yaw: f64, pitch: f64, pitch_offset: f64, power: f64) -> Vec3 {
    let yaw_rad = yaw.to_radians();
    let pitch_rad = pitch.to_radians();
    let offset_pitch_rad = (pitch + pitch_offset).to_radians();
    let dx = -yaw_rad.sin() * pitch_rad.cos();
    let dy = -offset_pitch_rad.sin();
    let dz = yaw_rad.cos() * pitch_rad.cos();
    // `getMovementToShoot` normalises before scaling. With `pitch_offset == 0.0`
    // the triple is already unit-length, but with the potion's `-20.0` it is not,
    // so the normalise is load-bearing rather than defensive.
    let len = (dx * dx + dy * dy + dz * dz).sqrt();
    if len < 1e-9 {
        return Vec3::default();
    }
    Vec3::new(
        dx / len * power,
        dy / len * power,
        dz / len * power,
    )
}

// ---------------------------------------------------------------------------
// Impact: the half `ProjectileRegistry`'s doc comment used to hand to the caller
// ---------------------------------------------------------------------------

/// The entity-hitbox inflation a projectile uses for its impact test, vanilla's
/// `ProjectileUtil.computeMargin`: `clamp((tickCount - 2) / 20, 0, 0.3)`.
///
/// **The first two ticks have a margin of exactly zero, and that is the point.**
/// A projectile is spawned inside its shooter's own box; a fixed `0.3` inflation
/// from tick zero makes an arrow strike the archer's chest on the tick it is
/// created. The ramp is what lets a projectile clear its owner before its hitbox
/// grows, and it reaches full size at tick `8`, not immediately.
#[must_use]
pub fn hitbox_margin(ticks_alive: u32) -> f64 {
    let raw = (f64::from(ticks_alive) - 2.0) / 20.0;
    raw.clamp(0.0, 0.3)
}

/// Where along a segment a projectile enters an axis-aligned box, as a parameter
/// in `0.0..=1.0`, or `None` if the segment misses it — vanilla's `AABB.clip`.
///
/// The exact slab intersection rather than a sampled walk, because a sampled walk
/// cannot distinguish "passed through a 0.6-wide mob at 3 blocks per tick" from
/// "missed", which is exactly the case an arrow is in: at typical bow speed the
/// travel per tick is five times the target's width, so any sample spacing coarse
/// enough to be cheap steps straight over the target.
///
/// A segment that *starts* inside the box returns `Some(0.0)`, matching vanilla's
/// containment case.
#[must_use]
pub fn clip_aabb(from: Vec3, delta: Vec3, min: Vec3, max: Vec3) -> Option<f64> {
    if from.x >= min.x
        && from.x <= max.x
        && from.y >= min.y
        && from.y <= max.y
        && from.z >= min.z
        && from.z <= max.z
    {
        return Some(0.0);
    }
    let mut enter = 0.0_f64;
    let mut exit = 1.0_f64;
    // Per-axis slab test. The parallel case (`d` near zero) is decided by
    // position alone: a ray parallel to a slab either lies inside it for the
    // whole segment or never enters it, and dividing by the zero would otherwise
    // produce an infinity that silently compares as "inside".
    for (o, d, lo, hi) in [
        (from.x, delta.x, min.x, max.x),
        (from.y, delta.y, min.y, max.y),
        (from.z, delta.z, min.z, max.z),
    ] {
        if d.abs() < 1e-12 {
            if o < lo || o > hi {
                return None;
            }
            continue;
        }
        let t1 = (lo - o) / d;
        let t2 = (hi - o) / d;
        let (near, far) = if t1 <= t2 { (t1, t2) } else { (t2, t1) };
        enter = enter.max(near);
        exit = exit.min(far);
        if enter > exit {
            return None;
        }
    }
    (enter <= 1.0 && exit >= 0.0).then_some(enter)
}

/// What a projectile does to the entity it strikes.
///
/// Deliberately not "a damage number": a snowball deals `0` to almost everything
/// and is still a hit that consumes the projectile, and a small fireball's real
/// effect is as much the five seconds of fire as the five points of damage. A
/// bare `f32` would have collapsed both of those into "no impact".
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImpactEffect {
    /// Raw damage handed to the reduction pipeline, before armour.
    pub damage: f32,
    /// Seconds of fire the hit sets, `0.0` for everything but a fireball.
    pub ignite_seconds: f32,
    /// Whether the projectile is consumed by hitting an entity at all. Every
    /// vanilla projectile modelled here is (piercing arrows are not modelled).
    pub consumed: bool,
}

/// `AbstractArrow.ARROW_BASE_DAMAGE` — the `baseDamage` field's initialiser.
pub const ARROW_BASE_DAMAGE: f64 = 2.0;

/// `TridentItem.BASE_DAMAGE`, the `baseDamage` a thrown trident carries. Flat,
/// not tier-derived, and **not** the same as the arrow's.
pub const TRIDENT_BASE_DAMAGE: f64 = 8.0;

/// `SmallFireball.onHitEntity`'s `hurtServer(..., 5.0F)`.
pub const SMALL_FIREBALL_DAMAGE: f32 = 5.0;

/// `SmallFireball.onHitEntity`'s `igniteForSeconds(5.0F)`.
pub const SMALL_FIREBALL_IGNITE_SECONDS: f32 = 5.0;

/// `WitherSkull.onHitEntity`'s damage when the shooter is a living owner —
/// the only case this crate's production skull spawns ever hit (see
/// `lodestone_server::wither`'s own doc for the no-owner `5.0F` case, which
/// this table does not carry because nothing here resolves "does this
/// tracked projectile have an owner" — that is `MobSim`'s own
/// `ProjectileMeta::owner`, resolved by the caller, not this version-free
/// table).
pub const WITHER_SKULL_DAMAGE: f32 = 8.0;

/// `LargeFireball.onHitEntity`'s `hurtServer(..., 6.0F)` — the ghast's own
/// fireball, registry path `fireball` (see
/// `crate::ai::mob::ProjectileKind::LargeFireball`'s own doc for why that is
/// not `large_fireball`). The unconditional impact explosion
/// (`LargeFireball.onHit`'s `level().explode(...)`, both on a block and on an
/// entity) needs the target's own liveness the same way the wither skull's
/// does, so `MobSim`'s impact pass applies that half — this table only carries
/// the direct hit.
pub const LARGE_FIREBALL_DAMAGE: f32 = 6.0;

/// An arrow-family impact's damage, `AbstractArrow.onHitEntity`:
/// `Mth.ceil(Mth.clamp(speed * baseDamage, 0, Integer.MAX_VALUE))`.
///
/// **Speed-scaled and then rounded up to a whole number.** Both halves matter:
/// an arrow that has slowed to a drift does proportionally less damage, and the
/// `ceil` means even a nearly-stationary arrow that connects deals at least `1`.
/// A formula that dropped the `ceil` would have a spent arrow deal `0.4`, which
/// the i-frame gate then treats as a landed hit for a fractional loss — visibly
/// wrong in a way "the arrow does damage" would not catch.
#[must_use]
pub fn arrow_impact_damage(speed: f64, base_damage: f64) -> f32 {
    let scaled = (speed * base_damage).clamp(0.0, f64::from(i32::MAX));
    scaled.ceil() as f32
}

/// The impact effect for a projectile entity path (`arrow`, `snowball`, …)
/// travelling at `speed` blocks per tick.
///
/// Keyed by the bare registry path rather than an enum because that is the
/// identity the host already carries for a spawned projectile, and because
/// `ai::roster::ranged::projectile_entity_type` already speaks exactly these
/// strings — one vocabulary, not two.
///
/// An unrecognised path yields a harmless zero-damage consumed hit rather than
/// `None`: a projectile this table does not know still has to stop somewhere, and
/// leaving it flying forever is the worse failure.
#[must_use]
pub fn impact_effect(path: &str, speed: f64) -> ImpactEffect {
    let path = path.strip_prefix("minecraft:").unwrap_or(path);
    let none = ImpactEffect {
        damage: 0.0,
        ignite_seconds: 0.0,
        consumed: true,
    };
    match path {
        "arrow" | "spectral_arrow" => ImpactEffect {
            damage: arrow_impact_damage(speed, ARROW_BASE_DAMAGE),
            ..none
        },
        "trident" => ImpactEffect {
            damage: arrow_impact_damage(speed, TRIDENT_BASE_DAMAGE),
            ..none
        },
        "small_fireball" => ImpactEffect {
            damage: SMALL_FIREBALL_DAMAGE,
            ignite_seconds: SMALL_FIREBALL_IGNITE_SECONDS,
            consumed: true,
        },
        // `Snowball.onHitEntity`: `entity instanceof Blaze ? 3 : 0`. The blaze
        // special case needs the *target's* type, which this function is not
        // given, so the host applies it — see `MobSim`'s impact pass. Zero here
        // is the general case, not a stand-in for the whole rule.
        "snowball" | "egg" | "ender_pearl" | "experience_bottle" | "splash_potion"
        | "lingering_potion" => none,
        // `WitherSkull.onHitEntity`'s damage; the impact-blast/wither-effect
        // halves need the target's own liveness/owner, so `MobSim`'s impact
        // pass applies those, matching the snowball-vs-blaze precedent above.
        "wither_skull" => ImpactEffect {
            damage: WITHER_SKULL_DAMAGE,
            ..none
        },
        "fireball" => ImpactEffect {
            damage: LARGE_FIREBALL_DAMAGE,
            ..none
        },
        _ => none,
    }
}

/// `Snowball.onHitEntity`'s blaze-only damage.
pub const SNOWBALL_BLAZE_DAMAGE: f32 = 3.0;

/// One projectile the [`ProjectileRegistry`] is advancing, keyed by its
/// network entity id (an `i32`, matching `SimMob`'s numbering convention in
/// `lodestone-server`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackedProjectile {
    /// The entity id this projectile was spawned with.
    pub id: i32,
    /// The projectile's own ballistic state.
    pub projectile: Projectile,
    /// Ticks since this projectile was registered.
    pub ticks_alive: u32,
}

/// The live set of ballistic projectiles a driver advances once per server
/// tick — the seam that was missing. [`Projectile::tick`] correctly
/// integrates one projectile's motion, but nothing owned a *collection* of
/// them across ticks: `grep`ping the whole tree outside this crate for
/// `projectile::Projectile` returned nothing.
///
/// A caller (typically an integrated server's per-tick loop, alongside
/// `MobSim::tick`) owns one of these: [`spawn`](Self::spawn) a projectile
/// when a launch action creates one, call [`tick`](Self::tick) once per
/// server tick, and [`remove`](Self::remove) it on impact or despawn.
///
/// **Impact resolution is no longer "the caller's job" in the sense of "nobody
/// does it".** `tick` still only advances motion — the *search* for what a
/// projectile hit needs the world and entity set this crate deliberately does not
/// depend on — but the geometry and the damage arithmetic live here, as
/// [`clip_aabb`], [`hitbox_margin`] and [`impact_effect`], so a host supplies the
/// candidate list and nothing more. `lodestone-server`'s `MobSim` is that host:
/// its per-tick impact pass runs before this `tick`, exactly as vanilla's
/// `AbstractArrow.tick` tests the segment it is about to travel rather than the
/// one it just travelled.
#[derive(Debug, Default, Clone)]
pub struct ProjectileRegistry {
    entries: Vec<TrackedProjectile>,
}

impl ProjectileRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers `projectile` under `id`, replacing any existing entry with
    /// the same id.
    pub fn spawn(&mut self, id: i32, projectile: Projectile) {
        self.entries.retain(|e| e.id != id);
        self.entries.push(TrackedProjectile {
            id,
            projectile,
            ticks_alive: 0,
        });
    }

    /// Removes and returns the tracked projectile with `id`, if any (e.g. on
    /// impact).
    pub fn remove(&mut self, id: i32) -> Option<TrackedProjectile> {
        let idx = self.entries.iter().position(|e| e.id == id)?;
        Some(self.entries.remove(idx))
    }

    /// The current ballistic state of `id`, if tracked.
    #[must_use]
    pub fn get(&self, id: i32) -> Option<&Projectile> {
        self.entries.iter().find(|e| e.id == id).map(|e| &e.projectile)
    }

    /// Marks whether `id`'s projectile is currently submerged, selecting
    /// [`DragProfile::water`] starting next tick. The caller (world
    /// collision) owns this decision; the registry only stores it. Returns
    /// `false` if `id` is not tracked.
    pub fn set_in_water(&mut self, id: i32, in_water: bool) -> bool {
        let Some(e) = self.entries.iter_mut().find(|e| e.id == id) else {
            return false;
        };
        e.projectile.in_water = in_water;
        true
    }

    /// Advances every tracked projectile exactly one server tick.
    pub fn tick(&mut self) {
        for e in &mut self.entries {
            e.projectile.tick();
            e.ticks_alive += 1;
        }
    }

    /// Number of tracked projectiles.
    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether no projectiles are tracked.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterates the tracked projectiles in registration order.
    pub fn iter(&self) -> impl Iterator<Item = &TrackedProjectile> {
        self.entries.iter()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(x: f64, y: f64, z: f64) -> Vec3 {
        Vec3::new(x, y, z)
    }

    #[test]
    fn throwable_applies_gravity_then_drag_then_move() {
        // A snowball dropped from rest: v0 = 0.
        let mut p = Projectile::throwable(v(0.0, 100.0, 0.0), v(0.0, 0.0, 0.0));
        p.tick();
        // vy = (0 - 0.03) * 0.99 = -0.0297; pos.y = 100 + (-0.0297).
        assert!(
            (p.velocity.y - (-0.0297)).abs() < 1e-12,
            "vy {}",
            p.velocity.y
        );
        assert!((p.position.y - (100.0 - 0.0297)).abs() < 1e-12);
    }

    #[test]
    fn arrow_applies_move_then_drag_then_gravity() {
        // An arrow fired flat at 3 bpt in +x from rest vertically.
        let mut p = Projectile::arrow(v(0.0, 64.0, 0.0), v(3.0, 0.0, 0.0));
        p.tick();
        // move first: x = 0 + 3 = 3, y unchanged this tick.
        assert!((p.position.x - 3.0).abs() < 1e-12, "x {}", p.position.x);
        assert!((p.position.y - 64.0).abs() < 1e-12, "y {}", p.position.y);
        // then drag: vx = 3*0.99 = 2.97; then gravity: vy = 0 - 0.05.
        assert!((p.velocity.x - 2.97).abs() < 1e-12, "vx {}", p.velocity.x);
        assert!(
            (p.velocity.y - (-0.05)).abs() < 1e-12,
            "vy {}",
            p.velocity.y
        );
    }

    #[test]
    fn order_matters_families_diverge_from_identical_start() {
        let start = v(0.0, 64.0, 0.0);
        let vel = v(2.0, 0.5, 0.0);
        let mut thrown = Projectile::throwable(start, vel);
        let mut arrow = Projectile::arrow(start, vel);
        thrown.gravity = 0.05; // same gravity to isolate ordering
        thrown.tick();
        arrow.tick();
        // Same constants, different order -> different first-tick position.
        assert!(
            (thrown.position.x - arrow.position.x).abs() > 1e-6
                || (thrown.position.y - arrow.position.y).abs() > 1e-6,
            "orders should diverge: {:?} vs {:?}",
            thrown.position,
            arrow.position
        );
    }

    #[test]
    fn water_drag_slows_faster_than_air() {
        let mut air = Projectile::arrow(v(0.0, 64.0, 0.0), v(4.0, 0.0, 0.0));
        let mut water = Projectile::arrow(v(0.0, 64.0, 0.0), v(4.0, 0.0, 0.0));
        water.in_water = true;
        air.tick();
        water.tick();
        // Water inertia 0.6 < air 0.99, so the submerged arrow is slower.
        assert!(water.velocity.x < air.velocity.x);
        assert!((water.velocity.x - 4.0 * 0.6).abs() < 1e-12);
    }

    #[test]
    fn thrown_projectile_falls_in_a_parabola() {
        // Fire level; after many ticks it must be lower and slower horizontally.
        let mut p = Projectile::snowball(v(0.0, 64.0, 0.0), v(1.5, 0.0, 0.0));
        let x0 = p.horizontal_speed();
        p.tick_n(40);
        assert!(p.position.y < 64.0, "should have fallen");
        assert!(p.horizontal_speed() < x0, "horizontal speed should decay");
        assert!(p.velocity.y < 0.0, "should be moving downward");
    }

    // -- ProjectileRegistry: the per-tick driver ----------------------------

    #[test]
    fn registry_tick_advances_every_tracked_projectile_through_one_call() {
        // Two different families registered under distinct ids. Driving them
        // exclusively through `ProjectileRegistry::tick` (never calling
        // `Projectile::tick` directly here) must land on exactly the same
        // state as ticking equivalent standalone instances n times — proving
        // the *registry* is what advances multiple heterogeneous entries,
        // not just the underlying integrator.
        let mut reg = ProjectileRegistry::new();
        reg.spawn(1, Projectile::arrow(v(0.0, 64.0, 0.0), v(3.0, 0.0, 0.0)));
        reg.spawn(2, Projectile::snowball(v(0.0, 64.0, 0.0), v(1.5, 0.0, 0.0)));
        assert_eq!(reg.len(), 2);

        for _ in 0..10 {
            reg.tick();
        }

        let mut expected_arrow = Projectile::arrow(v(0.0, 64.0, 0.0), v(3.0, 0.0, 0.0));
        expected_arrow.tick_n(10);
        let mut expected_snowball = Projectile::snowball(v(0.0, 64.0, 0.0), v(1.5, 0.0, 0.0));
        expected_snowball.tick_n(10);

        assert_eq!(reg.get(1), Some(&expected_arrow));
        assert_eq!(reg.get(2), Some(&expected_snowball));

        for e in reg.iter() {
            assert_eq!(e.ticks_alive, 10);
        }
    }

    #[test]
    fn registry_set_in_water_changes_subsequent_ticks() {
        let mut reg = ProjectileRegistry::new();
        reg.spawn(1, Projectile::arrow(v(0.0, 64.0, 0.0), v(4.0, 0.0, 0.0)));
        assert!(reg.set_in_water(1, true));
        assert!(!reg.set_in_water(99, true), "unknown id");
        reg.tick();
        // Water inertia 0.6 applies via the registry-stored flag, matching
        // the standalone `water_drag_slows_faster_than_air` expectation.
        assert!((reg.get(1).unwrap().velocity.x - 4.0 * 0.6).abs() < 1e-12);
    }

    // -- launch --------------------------------------------------------

    /// The bow curve at both ends and in the middle, with the linear hypothesis
    /// evaluated at an input where the two actually differ.
    ///
    /// At `20` ticks both give `1.0`, so a full draw is exactly the input that
    /// cannot distinguish them — which is why the mid-draw case is the one that
    /// carries the assertion. At `10` ticks: correct `(0.25 + 1.0) / 3 = 0.41666…`,
    /// linear `0.5`. Scaled by `BOW_ARROW_SPEED` those are `1.25` and `1.5`
    /// blocks/tick, whose `ceil(speed * 2.0)` impact damages are `3` and `3` — the
    /// same, so the *damage* is not the discriminator either. The power itself is.
    #[test]
    fn the_bow_curve_is_quadratic_not_linear() {
        assert!((bow_power_for_time(20) - 1.0).abs() < 1e-12);
        assert!((bow_power_for_time(40) - 1.0).abs() < 1e-12, "clamped at 1.0");
        assert!(bow_power_for_time(0).abs() < 1e-12);

        let mid = bow_power_for_time(10);
        assert!((mid - 0.416_666_666_666_666_6).abs() < 1e-12, "{mid}");
        assert!(
            (mid - 0.5).abs() > 0.08,
            "the linear hypothesis must be excluded by magnitude, got {mid}"
        );
        // Below the release threshold at a flick of the button: 3 ticks gives
        // (0.0225 + 0.3) / 3 = 0.1075, which is *above* 0.1 — so the "too weak to
        // fire" window is only the first two ticks, not the first several. Worth
        // pinning, because guessing that window is how a bow ends up unable to
        // fire at all.
        assert!(bow_power_for_time(1) < BOW_MIN_POWER, "{}", bow_power_for_time(1));
        assert!(bow_power_for_time(2) < BOW_MIN_POWER, "{}", bow_power_for_time(2));
        assert!(bow_power_for_time(3) > BOW_MIN_POWER, "{}", bow_power_for_time(3));
    }

    /// Launch direction, checked against hand-evaluated trigonometry rather than
    /// against this function run twice.
    ///
    /// Minecraft yaw `0` faces **+z** and yaw `-90` faces **+x** (the `-sin(yaw)`
    /// / `+cos(yaw)` pair above). Getting that convention backwards produces a
    /// launch that looks plausible and fires 90 degrees off, so both axes are
    /// pinned, and pitch `-90` (straight up) is checked separately because it is
    /// the case where the horizontal components must vanish exactly.
    #[test]
    fn launch_direction_follows_minecrafts_yaw_convention() {
        let p = 3.0;
        let north = launch_velocity(180.0, 0.0, 0.0, p);
        assert!(north.z < -2.99, "yaw 180 faces -z: {north:?}");
        let south = launch_velocity(0.0, 0.0, 0.0, p);
        assert!(south.z > 2.99, "yaw 0 faces +z: {south:?}");
        let east = launch_velocity(-90.0, 0.0, 0.0, p);
        assert!(east.x > 2.99, "yaw -90 faces +x: {east:?}");
        let west = launch_velocity(90.0, 0.0, 0.0, p);
        assert!(west.x < -2.99, "yaw 90 faces -x: {west:?}");

        let up = launch_velocity(0.0, -90.0, 0.0, p);
        assert!((up.y - p).abs() < 1e-9, "pitch -90 is straight up: {up:?}");
        assert!(up.x.abs() < 1e-9 && up.z.abs() < 1e-9, "no horizontal: {up:?}");

        // Speed is exactly `power` for any facing, because the direction is
        // normalised before scaling.
        for (yaw, pitch) in [(0.0, 0.0), (37.0, -14.0), (-123.0, 61.0)] {
            let v = launch_velocity(yaw, pitch, 0.0, p);
            let speed = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt();
            assert!((speed - p).abs() < 1e-9, "yaw {yaw} pitch {pitch}: {speed}");
        }
    }

    /// The potion's `-20.0` pitch offset lifts the throw without turning it, and
    /// the normalise afterwards keeps the speed at exactly `power`.
    ///
    /// The wrong reading — treating the offset as a rotation of the whole vector —
    /// would leave the horizontal direction changed. Asserted against the
    /// no-offset throw at the same facing.
    #[test]
    fn the_potion_pitch_offset_lifts_the_throw_without_turning_it() {
        let flat = launch_velocity(0.0, 0.0, 0.0, POTION_SHOOT_POWER);
        let lobbed = launch_velocity(0.0, 0.0, POTION_PITCH_OFFSET, POTION_SHOOT_POWER);
        assert!(lobbed.y > flat.y, "the offset must raise the arc");
        assert!(lobbed.x.abs() < 1e-9, "and must not introduce sideways drift");
        assert!(lobbed.z > 0.0, "still travelling forward: {lobbed:?}");
        let speed = (lobbed.x * lobbed.x + lobbed.y * lobbed.y + lobbed.z * lobbed.z).sqrt();
        assert!(
            (speed - POTION_SHOOT_POWER).abs() < 1e-9,
            "normalise must run: {speed}"
        );
        // The exact vertical component, derived here from the two outside
        // constants rather than read off a run of the function.
        //
        // **Not `sin(20°) * power`**, which is the plausible round answer and is
        // wrong by 5%: the pre-normalise triple is `(0, sin(20°), 1)`, whose length
        // is `sqrt(1 + sin²(20°)) = 1.0569`, and the normalise divides by it. The
        // first version of this assertion predicted `0.1710` and measured
        // `0.16181` — the normalise is exactly what that 5% is.
        let s = 20.0_f64.to_radians().sin();
        let expected_y = s / (1.0 + s * s).sqrt() * POTION_SHOOT_POWER;
        assert!(
            (lobbed.y - expected_y).abs() < 1e-12,
            "vertical component {} vs derived {expected_y}",
            lobbed.y
        );
        assert!(
            (s * POTION_SHOOT_POWER - expected_y).abs() > 0.008,
            "the un-normalised hypothesis must be excluded, not coincide"
        );
    }

    // -- impact geometry and damage ------------------------------------

    /// Two properties a sampled walk cannot supply, and this crate already has a
    /// sampled walk to compare against: `RayView::is_clear` steps at quarter-block
    /// spacing, which is sound for *blocks* (a cell is a full block wide) and
    /// unsound for a hitbox narrower than the step.
    ///
    /// The first property is **exactness**. A bow arrow travels 3.0 blocks per
    /// tick, so a mob 0.6 wide spanning x in `1.7..=2.3` is entered at exactly
    /// `t = 1.7 / 3.0`. A sampled walk can only report the first sample inside —
    /// `2.25 / 3.0` here, off by nearly a fifth of the segment. That error is not
    /// cosmetic: it is what decides whether a block hit at `t = 0.7` or this
    /// entity hit comes first.
    ///
    /// The second is **not stepping over the target at all**. The narrow box below
    /// is not guessed at a plausible width — it is derived from the sample grid
    /// inside the test, so the claim "no sample lands inside" is measured rather
    /// than asserted from arithmetic done in a doc comment.
    #[test]
    fn clip_is_exact_where_a_sampled_walk_is_approximate_or_blind() {
        let from = v(0.0, 0.0, 0.0);
        let delta = v(3.0, 0.0, 0.0);

        let hit = clip_aabb(from, delta, v(1.7, -1.0, -1.0), v(2.3, 1.0, 1.0))
            .expect("a 0.6-wide mob straight ahead is hit");
        assert!((hit - 1.7 / 3.0).abs() < 1e-9, "entry parameter {hit}");
        // The sampled alternative's answer, computed: the first quarter-block
        // sample inside the same box.
        let first_sample_inside = (0..=12)
            .map(|i| f64::from(i) * 0.25)
            .find(|x| (1.7..=2.3).contains(x))
            .expect("some sample lands in a 0.6-wide box");
        // Not a round number, and worth measuring rather than predicting: the
        // first sample inside is 1.75, not the 2.25 a first guess suggests, so
        // the sampled answer is only 0.0167 of the segment late. Small — but four
        // million times the tolerance the exact assertion above holds to, which
        // is the sense in which the two implementations are distinguishable here.
        let sampled_error = (hit - first_sample_inside / 3.0).abs();
        assert!(
            sampled_error > 1e-3,
            "the sampled entry must not coincide with the exact one, or exactness is \
             not what this test is measuring (error {sampled_error})"
        );
        assert!(
            sampled_error < 0.25 / 3.0 + 1e-9,
            "a quarter-block walk cannot be late by more than one step (error {sampled_error})"
        );

        // A hitbox narrower than the sample spacing, placed between two samples.
        let (lo, hi) = (1.80, 1.95);
        let any_sample_inside = (0..=12)
            .map(|i| f64::from(i) * 0.25)
            .any(|x| (lo..=hi).contains(&x));
        assert!(
            !any_sample_inside,
            "the control is vacuous: a sampled walk would also have found this box"
        );
        let narrow = clip_aabb(from, delta, v(lo, -1.0, -1.0), v(hi, 1.0, 1.0));
        assert!(
            narrow.is_some_and(|t| (t - lo / 3.0).abs() < 1e-9),
            "the slab clip must find a box every sample steps over: {narrow:?}"
        );
    }

    #[test]
    fn clip_rejects_a_miss_and_a_segment_that_stops_short() {
        let from = v(0.0, 0.0, 0.0);
        // Off to one side in z.
        assert_eq!(
            clip_aabb(from, v(3.0, 0.0, 0.0), v(1.0, -1.0, 5.0), v(2.0, 1.0, 6.0)),
            None
        );
        // Directly ahead but beyond the end of the segment: t would be 4.0.
        assert_eq!(
            clip_aabb(from, v(1.0, 0.0, 0.0), v(4.0, -1.0, -1.0), v(5.0, 1.0, 1.0)),
            None
        );
        // Starting inside is an immediate hit.
        assert_eq!(
            clip_aabb(v(1.5, 0.0, 0.0), v(3.0, 0.0, 0.0), v(1.0, -1.0, -1.0), v(2.0, 1.0, 1.0)),
            Some(0.0)
        );
    }

    /// The margin ramp, and specifically that it is **zero** for the first two
    /// ticks. A constant `0.3` is the plausible wrong reading of
    /// `computeMargin`, and it differs at exactly the ticks that decide whether
    /// an arrow hits the archer who fired it.
    #[test]
    fn the_hitbox_margin_starts_at_zero_and_saturates_at_tick_eight() {
        assert_eq!(hitbox_margin(0), 0.0);
        assert_eq!(hitbox_margin(2), 0.0);
        assert!((hitbox_margin(3) - 0.05).abs() < 1e-12, "{}", hitbox_margin(3));
        assert!((hitbox_margin(8) - 0.3).abs() < 1e-12);
        assert!((hitbox_margin(400) - 0.3).abs() < 1e-12, "clamped at 0.3");
    }

    /// A full-charge bow arrow deals **6**, and the two wrong formulas are each
    /// excluded by number rather than by direction.
    ///
    /// `BowItem.releaseUsing` shoots at `pow * 3.0` with `pow == 1.0`, so the
    /// arrow's speed at launch is `3.0`; `baseDamage` is `2.0`; so
    /// `ceil(3.0 * 2.0) == 6`. Dropping the speed scale gives `2`, and using the
    /// trident's `8.0` base instead gives `24`.
    #[test]
    fn a_full_charge_bow_arrow_deals_six() {
        let dealt = arrow_impact_damage(3.0, ARROW_BASE_DAMAGE);
        assert!((dealt - 6.0).abs() < 1e-6, "got {dealt}");
        assert!(
            (dealt - ARROW_BASE_DAMAGE as f32).abs() > 3.0,
            "the speed scale must be applied"
        );
        assert!(
            (dealt - 24.0).abs() > 17.0,
            "the arrow base is 2.0, not the trident's 8.0"
        );
    }

    /// The `ceil` is load-bearing at the slow end: an arrow that has decayed to
    /// 0.2 blocks per tick would deal `0.4` without it, and `1` with it.
    #[test]
    fn a_spent_arrow_still_deals_a_whole_point() {
        let dealt = arrow_impact_damage(0.2, ARROW_BASE_DAMAGE);
        assert!((dealt - 1.0).abs() < 1e-6, "got {dealt}");
        assert!(
            (dealt - 0.4).abs() > 0.5,
            "a truncating formula would deal a fraction of a heart"
        );
    }

    /// The per-projectile table, each entry against its own jar citation. The
    /// zero-damage entries are the interesting ones: a snowball is a real hit
    /// that consumes the projectile and deals nothing, which is different from
    /// not hitting.
    #[test]
    fn the_impact_table_separates_a_harmless_hit_from_no_hit() {
        let arrow = impact_effect("arrow", 3.0);
        assert!((arrow.damage - 6.0).abs() < 1e-6);
        assert!(arrow.consumed);
        assert!(arrow.ignite_seconds.abs() < 1e-6, "an arrow is not on fire");

        // A trident at the drowned's own launch speed of 1.6 (`trident_attack`):
        // ceil(1.6 * 8.0) = 13.
        let trident = impact_effect("trident", 1.6);
        assert!((trident.damage - 13.0).abs() < 1e-6, "got {}", trident.damage);

        let fireball = impact_effect("minecraft:small_fireball", 0.1);
        assert!((fireball.damage - 5.0).abs() < 1e-6, "flat, not speed-scaled");
        assert!((fireball.ignite_seconds - 5.0).abs() < 1e-6);

        let snowball = impact_effect("snowball", 1.5);
        assert!(snowball.damage.abs() < 1e-6, "harmless to a non-blaze");
        assert!(snowball.consumed, "but still consumed by the hit");

        // Namespaced and bare resolve identically, and an unknown projectile is
        // a harmless consumed hit rather than an eternal flyer.
        assert_eq!(impact_effect("minecraft:arrow", 3.0), impact_effect("arrow", 3.0));
        let unknown = impact_effect("wind_charge", 1.0);
        assert!(unknown.consumed && unknown.damage.abs() < 1e-6);
    }

    #[test]
    fn registry_remove_stops_further_ticking() {
        let mut reg = ProjectileRegistry::new();
        reg.spawn(1, Projectile::arrow(v(0.0, 64.0, 0.0), v(1.0, 0.0, 0.0)));
        let removed = reg.remove(1).expect("was tracked");
        assert_eq!(removed.id, 1);
        assert!(reg.is_empty());
        reg.tick(); // must not panic on an empty registry
        assert!(reg.get(1).is_none());
        assert!(reg.remove(1).is_none(), "already removed");
    }
}
