//! Dropped item entities: their fall dynamics and — the version-free part that
//! actually matters — their **lifecycle** (age, despawn, pickup delay, merge).
//!
//! Vanilla's numbers, all from `ItemEntity`:
//!   * despawn at `age >= 6000` ticks (5 minutes),
//!   * `age == -32768` is the sentinel for *never despawn* (`INFINITE_LIFETIME`),
//!   * `pickupDelay == 32767` is the sentinel for *never pick up*, any other
//!     positive value counts down one per tick and blocks pickup while nonzero,
//!   * two stacks merge when they are the same item, both mergable, and their
//!     combined count fits one stack (`maxStackSize`, 64 for most items),
//!   * fall dynamics: gravity `0.04`, air drag `0.98`, horizontal ground
//!     friction `0.98 * block_friction`, and a `-0.5` vertical bounce on landing.
//!
//! Pickup, inventory insertion and world collision belong to the integrated
//! server / world crates; this models only the entity's own per-tick evolution
//! and the pure predicates a server needs to decide despawn/merge.

use lodestone_model::Vec3;

/// The sentinel `age` marking an item that never despawns (`INFINITE_LIFETIME`).
pub const INFINITE_LIFETIME_AGE: i16 = -32768;
/// The age at which a normal item despawns.
pub const DESPAWN_AGE: i32 = 6000;
/// The sentinel `pickupDelay` marking an item that can never be picked up.
pub const NEVER_PICKUP_DELAY: i16 = 32767;
/// Default max stack size for items that do not override it.
pub const DEFAULT_MAX_STACK_SIZE: u8 = 64;
/// Item entity gravity per tick.
pub const ITEM_GRAVITY: f64 = 0.04;
/// Item entity air drag per tick (base `Entity.getAirDrag`).
pub const ITEM_AIR_DRAG: f64 = 0.98;
/// Default block friction (most blocks) that scales horizontal ground drag.
pub const DEFAULT_BLOCK_FRICTION: f64 = 0.6;

/// The mutable lifecycle counters of a dropped item, independent of the item's
/// identity (which the caller tracks) so this stays version-free.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ItemLifecycle {
    /// Ticks lived. Counts up each tick unless it is [`INFINITE_LIFETIME_AGE`].
    pub age: i16,
    /// Ticks until pickup is allowed. Counts down unless [`NEVER_PICKUP_DELAY`].
    pub pickup_delay: i16,
    /// Current stack count.
    pub count: u8,
    /// This item's maximum stack size.
    pub max_stack_size: u8,
}

impl Default for ItemLifecycle {
    fn default() -> Self {
        Self {
            age: 0,
            pickup_delay: 0,
            count: 1,
            max_stack_size: DEFAULT_MAX_STACK_SIZE,
        }
    }
}

impl ItemLifecycle {
    /// A freshly dropped stack with the default 0.5 s (10-tick) pickup delay a
    /// natural drop uses (`setDefaultPickUpDelay`).
    #[must_use]
    pub fn newly_dropped(count: u8, max_stack_size: u8) -> Self {
        Self {
            age: 0,
            pickup_delay: 10,
            count,
            max_stack_size,
        }
    }

    /// Advances one tick: decrements a finite pickup delay, then increments a
    /// finite age. Mirrors the counter updates in `ItemEntity.tick`.
    pub fn tick(&mut self) {
        if self.pickup_delay > 0 && self.pickup_delay != NEVER_PICKUP_DELAY {
            self.pickup_delay -= 1;
        }
        if self.age != INFINITE_LIFETIME_AGE {
            self.age = self.age.saturating_add(1);
        }
    }

    /// Whether the item should be removed this tick (`age >= 6000`). Infinite
    /// items never despawn.
    #[must_use]
    pub fn should_despawn(&self) -> bool {
        self.age != INFINITE_LIFETIME_AGE && i32::from(self.age) >= DESPAWN_AGE
    }

    /// Whether a player may pick the item up now: delay elapsed and not the
    /// never-pickup sentinel.
    #[must_use]
    pub fn can_be_picked_up(&self) -> bool {
        self.pickup_delay == 0
    }

    /// Whether this stack can participate in a merge (`isMergable`): alive, not
    /// never-pickup, not infinite-age, not yet despawned, and not already a full
    /// stack.
    #[must_use]
    pub fn is_mergable(&self) -> bool {
        self.pickup_delay != NEVER_PICKUP_DELAY
            && self.age != INFINITE_LIFETIME_AGE
            && i32::from(self.age) < DESPAWN_AGE
            && self.count < self.max_stack_size
    }
}

/// Marks an item so it never despawns.
#[must_use]
pub fn make_infinite(mut lifecycle: ItemLifecycle) -> ItemLifecycle {
    lifecycle.age = INFINITE_LIFETIME_AGE;
    lifecycle
}

/// Attempts to merge `from` into `to`, both assumed to be the same item type.
///
/// Returns `Some((new_to_count, new_from_count))` when a merge is possible,
/// transferring as much as fits into `to` (capped at `to.max_stack_size`), and
/// carrying over `pickup_delay` as `max(to, from)` exactly like
/// `ItemEntity.merge`. Returns `None` when either side is not mergable. When the
/// returned `from` count is `0`, the source entity should be discarded.
#[must_use]
pub fn try_merge(
    to: &ItemLifecycle,
    from: &ItemLifecycle,
) -> Option<(ItemLifecycle, ItemLifecycle)> {
    if !to.is_mergable() || !from.is_mergable() {
        return None;
    }
    let space = to.max_stack_size.saturating_sub(to.count);
    if space == 0 {
        return None;
    }
    let moved = space.min(from.count);
    let mut new_to = *to;
    let mut new_from = *from;
    new_to.count += moved;
    new_from.count -= moved;
    let delay = to.pickup_delay.max(from.pickup_delay);
    new_to.pickup_delay = delay;
    new_from.pickup_delay = delay;
    Some((new_to, new_from))
}

/// The falling/rolling dynamics of an item entity, kept separate from its
/// lifecycle so a caller can model position independently.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ItemMotion {
    /// Current position.
    pub position: Vec3,
    /// Current velocity in blocks per tick.
    pub velocity: Vec3,
    /// Whether the item rests on the ground this tick (selects ground friction
    /// and enables the landing bounce).
    pub on_ground: bool,
    /// Friction of the block underneath (only used when `on_ground`).
    pub block_friction: f64,
}

impl ItemMotion {
    /// A new airborne item at `position` with velocity `velocity`.
    #[must_use]
    pub fn new(position: Vec3, velocity: Vec3) -> Self {
        Self {
            position,
            velocity,
            on_ground: false,
            block_friction: DEFAULT_BLOCK_FRICTION,
        }
    }

    /// Advances one tick of free/rolling motion: gravity, translate, then the
    /// split horizontal/vertical drag (`ItemEntity.tick`, airborne branch). This
    /// models the entity's own motion; block collision that would zero a
    /// component is the world crate's job and is expressed here through
    /// `on_ground`.
    pub fn tick(&mut self) {
        self.velocity.y -= ITEM_GRAVITY;
        self.position += self.velocity;

        let mut horizontal_drag = ITEM_AIR_DRAG;
        if self.on_ground {
            horizontal_drag *= self.block_friction;
        }
        self.velocity.x *= horizontal_drag;
        self.velocity.z *= horizontal_drag;
        self.velocity.y *= ITEM_AIR_DRAG;

        if self.on_ground && self.velocity.y < 0.0 {
            self.velocity.y *= -0.5;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_counts_up_and_despawns_at_6000() {
        let mut l = ItemLifecycle {
            age: 5998,
            ..Default::default()
        };
        l.tick(); // 5999
        assert!(!l.should_despawn());
        l.tick(); // 6000
        assert!(l.should_despawn(), "age {} should despawn", l.age);
    }

    #[test]
    fn infinite_item_never_ages_or_despawns() {
        let mut l = make_infinite(ItemLifecycle::default());
        for _ in 0..10 {
            l.tick();
        }
        assert_eq!(l.age, INFINITE_LIFETIME_AGE);
        assert!(!l.should_despawn());
        assert!(!l.is_mergable(), "infinite items are not mergable");
    }

    #[test]
    fn pickup_delay_counts_down_and_gates_pickup() {
        let mut l = ItemLifecycle::newly_dropped(1, 64);
        assert!(!l.can_be_picked_up(), "delay 10 blocks pickup");
        for _ in 0..10 {
            l.tick();
        }
        assert_eq!(l.pickup_delay, 0);
        assert!(l.can_be_picked_up());
    }

    #[test]
    fn never_pickup_sentinel_never_decrements() {
        let mut l = ItemLifecycle {
            pickup_delay: NEVER_PICKUP_DELAY,
            ..Default::default()
        };
        l.tick();
        assert_eq!(l.pickup_delay, NEVER_PICKUP_DELAY);
        assert!(!l.can_be_picked_up());
        assert!(!l.is_mergable());
    }

    #[test]
    fn merge_transfers_only_what_fits_and_takes_max_delay() {
        let to = ItemLifecycle {
            age: 100,
            pickup_delay: 5,
            count: 60,
            max_stack_size: 64,
        };
        let from = ItemLifecycle {
            age: 200,
            pickup_delay: 8,
            count: 10,
            max_stack_size: 64,
        };
        let (new_to, new_from) = try_merge(&to, &from).expect("both mergable");
        // Only 4 fit into `to` (64-60); 6 remain in `from`.
        assert_eq!(new_to.count, 64);
        assert_eq!(new_from.count, 6);
        // Pickup delay carried as max(5, 8).
        assert_eq!(new_to.pickup_delay, 8);
        assert_eq!(new_from.pickup_delay, 8);
    }

    #[test]
    fn full_stack_is_not_mergable() {
        let full = ItemLifecycle {
            count: 64,
            max_stack_size: 64,
            ..Default::default()
        };
        assert!(!full.is_mergable());
        let other = ItemLifecycle::default();
        assert!(try_merge(&full, &other).is_none());
    }

    #[test]
    fn item_falls_under_gravity_and_bounces_on_landing() {
        let mut m = ItemMotion::new(Vec3::new(0.0, 70.0, 0.0), Vec3::new(0.0, 0.0, 0.0));
        m.tick();
        // vy = (0 - 0.04) * 0.98 = -0.0392.
        assert!(
            (m.velocity.y - (-0.0392)).abs() < 1e-12,
            "vy {}",
            m.velocity.y
        );
        // On landing with downward velocity, vy is halved and reversed.
        let mut landed = ItemMotion::new(Vec3::new(0.0, 64.0, 0.0), Vec3::new(0.0, -0.2, 0.0));
        landed.on_ground = true;
        landed.tick();
        assert!(
            landed.velocity.y > 0.0,
            "bounce should invert vy: {}",
            landed.velocity.y
        );
    }

    #[test]
    fn ground_friction_slows_horizontal_more_than_air() {
        let mut air = ItemMotion::new(Vec3::new(0.0, 64.0, 0.0), Vec3::new(0.5, 0.0, 0.0));
        let mut ground = ItemMotion::new(Vec3::new(0.0, 64.0, 0.0), Vec3::new(0.5, 0.0, 0.0));
        ground.on_ground = true;
        air.tick();
        ground.tick();
        assert!(ground.velocity.x < air.velocity.x);
        // ground: 0.5 * (0.98 * 0.6); air: 0.5 * 0.98.
        assert!((ground.velocity.x - 0.5 * 0.98 * 0.6).abs() < 1e-12);
    }
}
