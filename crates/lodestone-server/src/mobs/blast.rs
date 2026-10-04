//! A blast queued from outside the entity simulation, such as a respawn anchor
//! used outside the Nether.

use lodestone_model::Vec3;

use super::{Detonation, MobSim};

impl MobSim<'_> {
    /// Damages every mob in range of a `power` blast at `centre` and queues the
    /// block half for the tick loop's detonation drain, which destroys blocks,
    /// rolls drops, sends the explosion packet and, when `fire` is set, lights
    /// fires in the crater.
    pub(crate) fn queue_blast(&mut self, centre: Vec3, power: f32, fire: bool) {
        self.explode(centre, power, lodestone_entity::DamageFlags::default());
        self.pending_detonations.push(Detonation { centre, radius: power, fire });
    }
}
