//! A minecart's saved fields: the NBT a cart writes and reads back.
//!
//! The modelled state (container slots, furnace fuel and push, TNT fuse, the
//! flip flag) is encoded here under the reference names; everything else the
//! incoming record carried (display block, a command or spawner cart's data, a
//! loot table) rides in `TrackedMinecart::carried` verbatim. [`OWNED`] is the
//! dividing line: a listed field is never carried, so a stale copy never sits
//! beside the live value.

use lodestone_core::Nbt;
use lodestone_physics::Vec3d;

use super::minecart::MinecartKind;
use super::{ItemLifecycle, MobSim, SpawnRng, TrackedMinecart};

/// Fields a cart decodes onto its own state.
const OWNED: &[&str] = &["Items", "PushX", "PushZ", "Fuel", "fuse", "FlippedRotation"];

/// Fields the save path attaches itself from live state.
const WRITTEN_ELSEWHERE: &[&str] = &["Tags", "Passengers"];

impl TrackedMinecart {
    /// The fields past the common header, in the reference writer's order.
    pub(super) fn saved_fields(&self) -> Vec<(String, Nbt)> {
        let mut out: Vec<(String, Nbt)> = self.carried.clone();
        let has_loot_table = out.iter().any(|(name, _)| name == "LootTable");
        match self.kind {
            MinecartKind::Chest | MinecartKind::Hopper if !has_loot_table => {
                out.push(("Items".to_owned(), crate::chunk_nbt::items_to_nbt(&self.slots)));
            }
            MinecartKind::Furnace => {
                out.push(("PushX".to_owned(), Nbt::Double(self.push.x)));
                out.push(("PushZ".to_owned(), Nbt::Double(self.push.z)));
                out.push(("Fuel".to_owned(), Nbt::Short(self.fuel.clamp(0, i32::from(i16::MAX)) as i16)));
            }
            MinecartKind::Tnt => out.push(("fuse".to_owned(), Nbt::Int(self.fuse))),
            _ => {}
        }
        out.push(("FlippedRotation".to_owned(), Nbt::Byte(i8::from(self.flipped))));
        out
    }

    /// Applies a record's fields: owned ones onto the cart, the rest carried.
    pub(super) fn restore_fields(&mut self, fields: &[(String, Nbt)]) {
        let value = |name: &str| fields.iter().find(|(n, _)| n == name).map(|(_, v)| v);
        self.carried = fields
            .iter()
            .filter(|(name, _)| !OWNED.contains(&name.as_str()) && !WRITTEN_ELSEWHERE.contains(&name.as_str()))
            .cloned()
            .collect();
        let size = self.kind.container_size();
        if size > 0 {
            self.slots = crate::chunk_nbt::items_from_nbt(value("Items"), size);
        }
        if self.kind.is_furnace() {
            let push = |name: &str| match value(name) {
                Some(Nbt::Double(d)) => *d,
                _ => 0.0,
            };
            self.push = Vec3d::new(push("PushX"), 0.0, push("PushZ"));
            self.fuel = match value("Fuel") {
                Some(Nbt::Short(f)) => i32::from(*f),
                _ => 0,
            };
        }
        if self.kind == MinecartKind::Tnt {
            self.fuse = match value("fuse") {
                Some(Nbt::Int(f)) => *f,
                _ => -1,
            };
        }
        self.flipped = matches!(value("FlippedRotation"), Some(Nbt::Byte(b)) if *b != 0);
    }
}

impl MobSim<'_> {
    /// Drops every stack in cart `id`'s slots at its position, the way a
    /// broken container spills.
    pub(super) fn spill_minecart_contents(&mut self, id: i32) {
        let Some(cart) = self.minecarts.get_mut(&id) else { return };
        let at = lodestone_model::Vec3::new(cart.motion.position.x, cart.motion.position.y, cart.motion.position.z);
        let stacks: Vec<_> = cart.slots.iter_mut().filter_map(Option::take).collect();
        let mut rng = SpawnRng::new(
            (self.tick_count as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15) ^ (at.x.to_bits() ^ at.z.to_bits().rotate_left(31)),
        );
        for stack in stacks {
            let velocity = crate::block_drops::dropped_item_velocity(&mut rng);
            let count = u8::try_from(stack.count).unwrap_or(u8::MAX);
            self.spawn_item(
                &stack,
                at,
                velocity,
                ItemLifecycle::newly_dropped(count, lodestone_entity::item_entity::DEFAULT_MAX_STACK_SIZE),
            );
        }
    }
}
