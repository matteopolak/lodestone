//! The one damage entry for boats, rafts and minecarts.
//!
//! A hit rocks the hull (hurt time, a flipped direction, damage x 10 as the
//! rock amplitude) and breaks the vehicle once the accumulated amplitude passes
//! [`BREAK_DAMAGE`], or at once for a creative-mode source. A broken vehicle
//! drops its item, a container cart spills its slots first, and its riders
//! lose their seats. A creative break removes the vehicle with no drops at all.
//! Melee, projectiles and explosions all arrive through [`MobSim::hurt_vehicle`].

use lodestone_model::{ItemStack, Vec3};

use super::MobSim;
use super::minecart::MinecartKind;

/// Ticks the hull rocks after a hit. The client's roll formula reads the
/// counter twice (inside its sine and as a linear falloff), so this sets both
/// the duration and the number of swings.
const HURT_TICKS: i32 = 10;

/// Accumulated damage x 10 above which a vehicle breaks.
const BREAK_DAMAGE: f32 = 40.0;

/// Horizontal speed squared at or above which a TNT cart that breaks goes off
/// instead of dropping its item.
const TNT_CART_ARMING_SPEED_SQR: f64 = 0.01;

/// The rock state every vehicle shares on the wire: entity-data indices 8
/// (hurt time), 9 (hurt direction) and 10 (damage).
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct VehicleHurt {
    /// Ticks of rocking remaining.
    pub time: i32,
    /// `+1` or `-1`. The registered default is `1`, not `0`: the client
    /// multiplies the whole roll by it, so zero draws a still hull.
    pub dir: i32,
    /// Accumulated damage x 10, decayed by one per tick.
    pub damage: f32,
}

impl VehicleHurt {
    pub(super) const REST: Self = Self { time: 0, dir: 1, damage: 0.0 };

    /// Counts the rock down by one tick.
    pub(super) fn tick(&mut self) {
        if self.time > 0 {
            self.time -= 1;
        }
        if self.damage > 0.0 {
            self.damage -= 1.0;
        }
    }

    /// Tips the hull the other way and restarts the rock. The negation is what
    /// makes consecutive hits swing alternately instead of stuttering.
    fn rock(&mut self, amount: f32) {
        self.dir = -self.dir;
        self.time = HURT_TICKS;
        self.damage += amount * 10.0;
    }
}

/// Who or what hurt a vehicle.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum VehicleSource {
    /// A player's melee hit.
    Player { creative: bool },
    /// A projectile; `creative_shooter` is whether its owner is a creative player.
    Projectile { creative_shooter: bool },
    /// A blast. A mob-caused one is ignored while mob griefing is off.
    Explosion { by_mob: bool },
}

impl VehicleSource {
    fn creative(self) -> bool {
        matches!(
            self,
            Self::Player { creative: true } | Self::Projectile { creative_shooter: true }
        )
    }

    /// Whether the source lights a TNT cart: any explosion does.
    fn ignites_tnt(self) -> bool {
        matches!(self, Self::Explosion { .. })
    }
}

/// What a hit did to a vehicle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VehicleHurtOutcome {
    /// `id` is not a vehicle, or the source cannot hurt it.
    Ignored,
    /// The hull rocked and survives.
    Rocked,
    /// The vehicle broke: dropped its item, or primed if it is a TNT cart that
    /// detonates instead.
    Broken,
    /// A creative source removed it without drops.
    Discarded,
}

impl MobSim<'_> {
    /// Whether `id` is a boat, raft or minecart.
    #[must_use]
    pub fn is_vehicle(&self, id: i32) -> bool {
        self.vehicles.contains_key(&id) || self.minecarts.contains_key(&id)
    }

    /// Applies `amount` damage from `source` to vehicle `id`.
    pub fn hurt_vehicle(&mut self, id: i32, amount: f32, source: VehicleSource) -> VehicleHurtOutcome {
        if matches!(source, VehicleSource::Explosion { by_mob: true }) && !self.mob_griefing {
            return VehicleHurtOutcome::Ignored;
        }
        let hurt = if let Some(boat) = self.vehicles.get_mut(&id) {
            &mut boat.hurt
        } else if let Some(cart) = self.minecarts.get_mut(&id) {
            &mut cart.hurt
        } else {
            return VehicleHurtOutcome::Ignored;
        };
        hurt.rock(amount);
        let over = hurt.damage > BREAK_DAMAGE;
        let tnt_lit = source.ignites_tnt() && self.minecarts.get(&id).is_some_and(|c| c.kind == MinecartKind::Tnt);
        if (!source.creative() && over) || tnt_lit {
            self.break_vehicle(id, source.ignites_tnt());
            VehicleHurtOutcome::Broken
        } else if source.creative() {
            self.discard_vehicle(id);
            VehicleHurtOutcome::Discarded
        } else {
            VehicleHurtOutcome::Rocked
        }
    }

    /// Breaks vehicle `id` the way damage does: its item drops (with the
    /// `entity_drops` rule), a container cart spills first, and a TNT cart that
    /// was lit or is moving fast enough primes instead.
    fn break_vehicle(&mut self, id: i32, lit: bool) {
        if let Some(cart) = self.minecarts.get(&id)
            && cart.kind == MinecartKind::Tnt
        {
            let speed_sqr = cart.motion.velocity.x * cart.motion.velocity.x
                + cart.motion.velocity.z * cart.motion.velocity.z;
            if lit || speed_sqr >= TNT_CART_ARMING_SPEED_SQR {
                if cart.fuse < 0 {
                    let fuse = self.tnt_rng.next_int(20) + self.tnt_rng.next_int(20);
                    if let Some(cart) = self.minecarts.get_mut(&id) {
                        cart.fuse = fuse;
                    }
                }
                return;
            }
        }
        self.eject_riders(id);
        if let Some(boat) = self.vehicles.remove(&id) {
            if self.entity_drops {
                let at = Vec3::new(boat.motion.position.x, boat.motion.position.y, boat.motion.position.z);
                self.drop_vehicle_item(ItemStack::new(boat.entity_type, 1), at);
            }
        } else {
            self.break_minecart(id, self.entity_drops);
        }
    }

    /// Removes vehicle `id` with no drops and no spill.
    fn discard_vehicle(&mut self, id: i32) {
        self.eject_riders(id);
        if self.vehicles.remove(&id).is_none() {
            self.break_minecart(id, false);
        }
    }

    /// Drops one vehicle item at `at`.
    pub(super) fn drop_vehicle_item(&mut self, stack: ItemStack, at: Vec3) {
        self.spawn_item(
            &stack,
            at,
            Vec3::new(0.0, 0.2, 0.0),
            lodestone_entity::item_entity::ItemLifecycle::newly_dropped(
                1,
                lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE,
            ),
        );
    }
}

impl MobSim<'_> {
    /// Hurts every vehicle in reach of a blast, scaled by distance and by how
    /// much of its box the blast can see, exactly as a mob is.
    pub(super) fn blast_vehicles(&mut self, centre: Vec3, radius: f32, by_mob: bool) {
        use lodestone_physics::EntityDimensions;

        use super::{ExplosionAabb, entity_damage, seen_percent};

        let boat_dims = EntityDimensions::new(crate::boat::BOAT_WIDTH as f32, crate::boat::BOAT_HEIGHT as f32, 0.0);
        let mut ids: Vec<(i32, EntityDimensions, Vec3)> = self
            .vehicles
            .iter()
            .map(|(&id, v)| (id, boat_dims, v.motion.position))
            .chain(
                self.minecarts
                    .iter()
                    .map(|(&id, c)| (id, super::minecart::MINECART_DIMENSIONS, c.motion.position)),
            )
            .map(|(id, dims, p)| (id, dims, Vec3::new(p.x, p.y, p.z)))
            .collect();
        ids.sort_unstable_by_key(|(id, ..)| *id);
        for (id, dims, position) in ids {
            let box_ = ExplosionAabb::from_size(position, f64::from(dims.width), f64::from(dims.height));
            let exposure = seen_percent(centre, box_, self.world);
            if exposure <= 0.0 {
                continue;
            }
            let box_centre = Vec3::new(
                (box_.min.x + box_.max.x) / 2.0,
                (box_.min.y + box_.max.y) / 2.0,
                (box_.min.z + box_.max.z) / 2.0,
            );
            let damage = entity_damage(radius, (box_centre - centre).length(), exposure);
            if damage > 0.0 {
                self.hurt_vehicle(id, damage, VehicleSource::Explosion { by_mob });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mobs::ChunkWorld;

    fn sim() -> MobSim<'static> {
        let world: &'static ChunkWorld = Box::leak(Box::new(ChunkWorld::new(-64, 384)));
        MobSim::new(world)
    }

    fn at() -> Vec3 {
        Vec3::new(0.5, 70.0, 0.5)
    }

    fn player() -> VehicleSource {
        VehicleSource::Player { creative: false }
    }

    fn dropped(sim: &MobSim<'_>) -> Vec<(String, u8)> {
        let mut items = sim.dropped_items();
        items.sort();
        items
    }

    /// A broken chest cart spills its slots and then drops itself; a creative
    /// break, the control, spills and drops nothing.
    #[test]
    fn a_broken_container_cart_spills_its_slots_and_a_creative_break_spills_none() {
        for (creative, expect_drops) in [(false, true), (true, false)] {
            let mut sim = sim();
            let id = sim.spawn_minecart(MinecartKind::Chest, at());
            let cobble = ItemStack::new("minecraft:cobblestone".parse().unwrap(), 3);
            sim.minecarts.get_mut(&id).unwrap().slots[0] = Some(cobble);
            let source = VehicleSource::Player { creative };
            sim.hurt_vehicle(id, 7.0, source);
            assert!(!sim.is_vehicle(id));
            let expected = if expect_drops {
                vec![("minecraft:chest_minecart".to_owned(), 1), ("minecraft:cobblestone".to_owned(), 3)]
            } else {
                Vec::new()
            };
            assert_eq!(dropped(&sim), expected, "creative = {creative}");
        }
    }

    /// The `entity_drops` rule off keeps both the item and the spill.
    #[test]
    fn the_entity_drops_rule_gates_the_item_and_the_spill() {
        let mut sim = sim();
        sim.set_entity_drops(false);
        let id = sim.spawn_minecart(MinecartKind::Chest, at());
        sim.minecarts.get_mut(&id).unwrap().slots[0] = Some(ItemStack::new("minecraft:dirt".parse().unwrap(), 1));
        sim.hurt_vehicle(id, 7.0, player());
        assert!(!sim.is_vehicle(id));
        assert!(dropped(&sim).is_empty());
    }

    /// A rider is lifted off when the hull breaks.
    #[test]
    fn breaking_a_boat_dismounts_its_riders() {
        let mut sim = sim();
        let boat = sim.spawn_vehicle("minecraft:oak_boat".parse().unwrap(), at(), 0.0);
        assert!(sim.mount_vehicle(boat, 77, false));
        assert_eq!(sim.hurt_vehicle(boat, 7.0, player()), VehicleHurtOutcome::Broken);
        assert_eq!(sim.vehicle_of(77), None);
    }

    /// Damage accumulates across hits (and decays one per tick): 3 x 10 + 2 x 10
    /// is 50 > 40, while 3 x 10 alone is not.
    #[test]
    fn damage_accumulates_across_hits_before_the_break() {
        let mut sim = sim();
        let id = sim.spawn_minecart(MinecartKind::Plain, at());
        assert_eq!(sim.hurt_vehicle(id, 3.0, player()), VehicleHurtOutcome::Rocked);
        assert_eq!(sim.hurt_vehicle(id, 2.0, player()), VehicleHurtOutcome::Broken);
        assert_eq!(dropped(&sim), vec![("minecraft:minecart".to_owned(), 1)]);
    }

    /// A TNT cart broken by an explosion primes instead of dropping, and a slow
    /// plain break drops the item; the fuse is the sum of two rolls below 20.
    #[test]
    fn a_tnt_cart_lit_by_a_blast_primes_and_a_slow_break_drops() {
        let mut sim = sim();
        let lit = sim.spawn_minecart(MinecartKind::Tnt, at());
        sim.hurt_vehicle(lit, 0.5, VehicleSource::Explosion { by_mob: false });
        assert!(sim.is_vehicle(lit), "a lit cart stays until the fuse ends");
        let fuse = sim.minecart_fuse(lit);
        assert!((0..=38).contains(&fuse), "fuse {fuse} is two rolls below 20");
        assert!(dropped(&sim).is_empty());

        let slow = sim.spawn_minecart(MinecartKind::Tnt, at());
        sim.hurt_vehicle(slow, 7.0, player());
        assert!(!sim.is_vehicle(slow));
        assert_eq!(dropped(&sim), vec![("minecraft:tnt_minecart".to_owned(), 1)]);
    }

    /// A mob-caused blast is ignored while mob griefing is off.
    #[test]
    fn a_mob_blast_spares_a_vehicle_while_mob_griefing_is_off() {
        let mut sim = sim();
        sim.set_mob_griefing(false);
        let id = sim.spawn_minecart(MinecartKind::Plain, at());
        assert_eq!(sim.hurt_vehicle(id, 9.0, VehicleSource::Explosion { by_mob: true }), VehicleHurtOutcome::Ignored);
        assert_eq!(sim.hurt_vehicle(id, 9.0, VehicleSource::Explosion { by_mob: false }), VehicleHurtOutcome::Broken);
    }
}
