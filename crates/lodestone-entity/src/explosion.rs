//! Explosions: the ray-sampled *exposure* model, the entity damage formula and
//! the knockback-power scalar.
//!
//! Vanilla's blast damage is not a simple radius falloff — an entity's exposure
//! is measured by firing a grid of rays from sample points across its bounding
//! box toward the blast centre and counting how many reach it unobstructed
//! (`ServerExplosion.getSeenPercent`). The sample density depends on the box
//! size — a bigger entity is sampled at *more* points (the step is
//! `1/(size*2+1)`) — so reproducing the exact grid is what makes the exposure
//! fraction match. Given that fraction, the damage is a closed form
//! (`ExplosionDamageCalculator.getEntityDamageAmount`).
//!
//! The ray-vs-block test is the version crate's collision world, so this takes a
//! [`RayView`] seam (mirroring `lodestone-physics`'s `CollisionView`) rather than
//! depending on a version crate. The **knockback impulse** it computes is a
//! scalar power only; turning it into a velocity push is `impl-physics`'s job —
//! coordinate through the project owner instead of applying it here.
//!
//! # Explosion knockback is NOT purely server-authoritative (E5)
//!
//! §12.31 settled that attack knockback needs no client model: the server sets
//! the entity's velocity and the client's integrator just consumes the
//! `SET_ENTITY_MOTION` it is handed. That generalisation **holds for non-player
//! entities under a blast** — `ServerExplosion.hurtEntities` calls
//! `entity.push(knockback)` server-side and the change rides the normal velocity
//! packet — but it **breaks for the local player**, which is client-predicted.
//! Verified in 26.2 source: the server records each hit player's vector into
//! `hitPlayers` and ships it as `ClientboundExplodePacket.playerKnockback`
//! (`Optional<Vec3>`); the client applies it *itself* with
//! `player.addDeltaMovement(knockback)` — an **additive** impulse to current
//! velocity, not a replacement (contrast attack knockback, which partially
//! overwrites horizontal velocity). So the seam for player explosion knockback
//! is: decode the explode packet's `playerKnockback` and hand that exact vector
//! to physics to *add*; the client does **not** recompute it from the blast (the
//! server already did, using the distinct `EXPLOSION_KNOCKBACK_RESISTANCE`
//! attribute — not attack's `KNOCKBACK_RESISTANCE`). [`knockback_power`] here is
//! the primitive for the non-player / predictive case.

use lodestone_model::Vec3;

/// An axis-aligned bounding box in world space, min/max corners.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Aabb {
    /// Minimum corner.
    pub min: Vec3,
    /// Maximum corner.
    pub max: Vec3,
}

impl Aabb {
    /// A box from its min and max corners.
    #[must_use]
    pub fn new(min: Vec3, max: Vec3) -> Self {
        Self { min, max }
    }

    /// A box centred on `centre` with the given full `width` (x/z) and `height`.
    #[must_use]
    pub fn from_size(centre: Vec3, width: f64, height: f64) -> Self {
        let hw = width / 2.0;
        Self {
            min: Vec3::new(centre.x - hw, centre.y, centre.z - hw),
            max: Vec3::new(centre.x + hw, centre.y + height, centre.z + hw),
        }
    }
}

/// The seam a caller supplies so the exposure sampler can ask whether a straight
/// line from a sample point to the blast centre is blocked by solid collision.
/// Implemented against the version crate's collision world.
pub trait RayView {
    /// Returns `true` if the segment from `from` to `to` is unobstructed by any
    /// block with collision (vanilla's own explosion-clip collision context, fluids ignored).
    fn is_clear(&self, from: Vec3, to: Vec3) -> bool;
}

/// A [`RayView`] where nothing obstructs — every ray reaches the centre. Useful
/// for open-air exposure (the fraction is then `1.0`) and for tests.
#[derive(Debug, Clone, Copy, Default)]
pub struct OpenAir;

impl RayView for OpenAir {
    fn is_clear(&self, _from: Vec3, _to: Vec3) -> bool {
        true
    }
}

/// The fraction of an entity exposed to a blast, `getSeenPercent`: fires the
/// box-size-dependent grid of rays and returns hits/total. Returns `0.0` for a
/// degenerate box.
#[must_use]
pub fn seen_percent(centre: Vec3, box_: Aabb, view: &impl RayView) -> f32 {
    let dx = box_.max.x - box_.min.x;
    let dy = box_.max.y - box_.min.y;
    let dz = box_.max.z - box_.min.z;
    let xs = 1.0 / (dx * 2.0 + 1.0);
    let ys = 1.0 / (dy * 2.0 + 1.0);
    let zs = 1.0 / (dz * 2.0 + 1.0);
    if xs < 0.0 || ys < 0.0 || zs < 0.0 {
        return 0.0;
    }
    let x_offset = (1.0 - (1.0 / xs).floor() * xs) / 2.0;
    let z_offset = (1.0 - (1.0 / zs).floor() * zs) / 2.0;

    let mut hits = 0u32;
    let mut count = 0u32;
    let mut xx = 0.0;
    while xx <= 1.0 {
        let mut yy = 0.0;
        while yy <= 1.0 {
            let mut zz = 0.0;
            while zz <= 1.0 {
                let x = lerp(xx, box_.min.x, box_.max.x);
                let y = lerp(yy, box_.min.y, box_.max.y);
                let z = lerp(zz, box_.min.z, box_.max.z);
                let from = Vec3::new(x + x_offset, y, z + z_offset);
                if view.is_clear(from, centre) {
                    hits += 1;
                }
                count += 1;
                zz += zs;
            }
            yy += ys;
        }
        xx += xs;
    }
    if count == 0 {
        0.0
    } else {
        hits as f32 / count as f32
    }
}

fn lerp(t: f64, a: f64, b: f64) -> f64 {
    a + t * (b - a)
}

/// The blast damage an entity takes, `getEntityDamageAmount`. `radius` is the
/// explosion radius (TNT is `4.0`), `distance` is the entity's distance from the
/// centre and `exposure` its [`seen_percent`].
#[must_use]
pub fn entity_damage(radius: f32, distance: f64, exposure: f32) -> f32 {
    let double_radius = f64::from(radius) * 2.0;
    let dist = distance / double_radius;
    if dist > 1.0 {
        return 0.0;
    }
    let pow = (1.0 - dist) * f64::from(exposure);
    ((pow * pow + pow) / 2.0 * 7.0 * double_radius + 1.0) as f32
}

/// The scalar knockback power `(1 - dist) * exposure * multiplier * (1 - kbRes)`
/// from `ServerExplosion.hurtEntities`. `impl-physics` multiplies this by the
/// normalised direction to get the velocity push — do not apply it here.
#[must_use]
pub fn knockback_power(
    radius: f32,
    distance: f64,
    exposure: f32,
    multiplier: f32,
    knockback_resistance: f64,
) -> f64 {
    let double_radius = f64::from(radius) * 2.0;
    let dist = distance / double_radius;
    if dist > 1.0 {
        return 0.0;
    }
    (1.0 - dist) * f64::from(exposure) * f64::from(multiplier) * (1.0 - knockback_resistance)
}

/// The direction a blast pushes an entity: the unit vector from the centre to
/// the entity's reference point (eye position for most, feet for primed TNT).
#[must_use]
pub fn knockback_direction(centre: Vec3, entity_reference: Vec3) -> Vec3 {
    (entity_reference - centre).normalize()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A view that blocks every ray, so exposure is 0.
    struct Blocked;
    impl RayView for Blocked {
        fn is_clear(&self, _from: Vec3, _to: Vec3) -> bool {
            false
        }
    }

    #[test]
    fn open_air_exposure_is_one() {
        let box_ = Aabb::from_size(Vec3::new(0.0, 0.0, 0.0), 0.6, 1.8);
        let e = seen_percent(Vec3::new(5.0, 0.0, 0.0), box_, &OpenAir);
        assert!(
            (e - 1.0).abs() < 1e-6,
            "open air should be fully exposed: {e}"
        );
    }

    #[test]
    fn fully_blocked_exposure_is_zero() {
        let box_ = Aabb::from_size(Vec3::new(0.0, 0.0, 0.0), 0.6, 1.8);
        let e = seen_percent(Vec3::new(5.0, 0.0, 0.0), box_, &Blocked);
        assert!(e.abs() < 1e-6, "fully blocked should be zero: {e}");
    }

    #[test]
    fn tnt_point_blank_full_exposure_matches_formula() {
        // TNT radius 4, entity at distance 1, exposure 1:
        // doubleR=8, dist=1/8=0.125, pow=0.875,
        // (0.875^2+0.875)/2 *7*8 +1 = (0.765625+0.875)/2*56+1
        // = 0.8203125*56+1 = 45.9375+1 = 46.9375.
        let d = entity_damage(4.0, 1.0, 1.0);
        assert!((d - 46.9375).abs() < 1e-3, "got {d}");
    }

    #[test]
    fn damage_is_zero_beyond_double_radius() {
        // distance > 2*radius => no damage.
        assert_eq!(entity_damage(4.0, 8.01, 1.0), 0.0);
    }

    #[test]
    fn damage_falls_off_with_distance() {
        let near = entity_damage(4.0, 1.0, 1.0);
        let far = entity_damage(4.0, 6.0, 1.0);
        assert!(far < near, "closer should hurt more: {near} vs {far}");
        assert!(far >= 1.0, "in-range damage has the +1 floor: {far}");
    }

    #[test]
    fn zero_exposure_gives_floor_damage_only() {
        // pow=0 -> (0+0)/2*... +1 = 1.0.
        let d = entity_damage(4.0, 1.0, 0.0);
        assert!((d - 1.0).abs() < 1e-4, "got {d}");
    }

    #[test]
    fn knockback_power_scales_and_resists() {
        // radius 4, dist 1 (=>0.125 fraction), exposure 1, mult 1, kbRes 0:
        // (1-0.125)*1*1*1 = 0.875.
        let p = knockback_power(4.0, 1.0, 1.0, 1.0, 0.0);
        assert!((p - 0.875).abs() < 1e-6, "got {p}");
        // Full knockback resistance nullifies it.
        assert_eq!(knockback_power(4.0, 1.0, 1.0, 1.0, 1.0), 0.0);
    }

    #[test]
    fn knockback_direction_points_away_from_centre() {
        let dir = knockback_direction(Vec3::new(0.0, 0.0, 0.0), Vec3::new(3.0, 0.0, 0.0));
        assert!((dir.x - 1.0).abs() < 1e-9);
        assert!(dir.y.abs() < 1e-9 && dir.z.abs() < 1e-9);
    }

    #[test]
    fn box_size_changes_the_sample_grid_density() {
        // The sample step is 1/(size*2+1), so a bigger box is sampled at more
        // points, not fewer. This pins that the grid density tracks box size
        // (the detail that makes exposure match vanilla for wide entities).
        struct Counter {
            n: std::cell::Cell<u32>,
        }
        impl RayView for Counter {
            fn is_clear(&self, _f: Vec3, _t: Vec3) -> bool {
                self.n.set(self.n.get() + 1);
                true
            }
        }
        let small = Counter {
            n: std::cell::Cell::new(0),
        };
        let big = Counter {
            n: std::cell::Cell::new(0),
        };
        let _ = seen_percent(
            Vec3::new(10.0, 0.0, 0.0),
            Aabb::from_size(Vec3::new(0.0, 0.0, 0.0), 1.0, 1.0),
            &small,
        );
        let _ = seen_percent(
            Vec3::new(10.0, 0.0, 0.0),
            Aabb::from_size(Vec3::new(0.0, 0.0, 0.0), 4.0, 4.0),
            &big,
        );
        // 1x1x1 -> 4 points/axis (64); 4x4x4 -> 9 points/axis (729).
        assert_eq!(small.n.get(), 64);
        assert_eq!(big.n.get(), 729);
        assert!(big.n.get() > small.n.get());
    }
}
