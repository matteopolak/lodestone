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
}
