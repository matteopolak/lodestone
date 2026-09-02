//! `MobSim`'s end-crystal slice — spawn, destruction, and the query API a
//! dragon needs to find its nearest healer. Follows the same split
//! `tnt.rs`/`vehicles.rs` established: [`super::TrackedCrystal`] lives in
//! `mobs/mod.rs`, the behaviour lives here.
//!
//! A port of vanilla's own end-crystal entity,
//! deliberately narrow: this codebase has no obsidian pillars anywhere (see
//! `crate::dragon::fight`'s module doc), so a crystal here is a plain
//! stationary point wherever a caller spawns one — it never checks whether
//! it is standing on a spike, and there is no "caged" variant.
//!
//! # What is deliberately not ported
//!
//! * **`handlePortal`/nether-portal interaction** and **igniting the block
//!   below it on fire** (`EndCrystal.tick`'s `BaseFireBlock.getState`
//!   write) — both are single-tick side effects on the *world*, and this
//!   struct tracks no block-state oracle to write through (the same class of
//!   cut `tnt.rs`'s own doc names for fluid current).
//! * **`beamTarget` has a wire field (`MetadataField::CrystalBeamTarget`) but
//!   no producer.** This crate has no obsidian pillars anywhere and no
//!   respawn sequence wired to a real crystal (`crate::dragon::fight`'s own
//!   module doc), so every crystal streams `CrystalBeamTarget(None)` — a real,
//!   disclosed gap, not a silent stub. `CrystalShowBottom` **is** a real,
//!   wired field now (always `true`, since a caged crystal is never spawned
//!   here either) — see `push_end_crystal_snapshots`'s own comment.

use lodestone_model::{ResourceKey, Rotation, Vec3};
use uuid::Uuid;

use super::{Detonation, MobSim, TrackedCrystal};

/// `EndCrystal.hurtServer`'s explosion power — the `6.0F` literal passed to
/// `level.explode(...)`.
pub const EXPLOSION_POWER: f32 = 6.0;

/// The entity-type key every end crystal streams as, following
/// `tnt::tnt_entity_type`'s own convention (a named constructor rather than a
/// numeric lookup, so a wrong key fails loudly instead of silently encoding
/// as a different vanilla entity).
pub(super) fn end_crystal_entity_type() -> ResourceKey {
    "minecraft:end_crystal"
        .parse()
        .expect("`minecraft:end_crystal` is a valid resource key")
}

impl<'w> MobSim<'w> {
    /// `new EndCrystal(level, x, y, z)` — spawns a stationary crystal at
    /// `pos`. Returns the new entity's network id.
    pub fn spawn_end_crystal(&mut self, pos: Vec3) -> i32 {
        let id = self.next_id;
        self.next_id += 1;
        self.crystals.insert(
            id,
            TrackedCrystal {
                uuid: Uuid::new_v4(),
                position: pos,
            },
        );
        id
    }

    /// The number of live end crystals — `dragon::fight`'s `alive_crystals`
    /// input, and `EnderDragonFight.updateCrystalCount`'s network-visible
    /// result (minus the spike-bounding-box scoping that function does and
    /// this sim cannot, per the module doc: every crystal here counts,
    /// there being nowhere else for one to be).
    #[must_use]
    pub fn end_crystal_count(&self) -> usize {
        self.crystals.len()
    }

    /// A live end crystal's position, if any.
    #[must_use]
    pub fn end_crystal_position(&self, id: i32) -> Option<Vec3> {
        self.crystals.get(&id).map(|c| c.position)
    }

    /// The id and position of every live end crystal — what
    /// `EnderDragon.checkCrystals`'s rescan
    /// (`level().getEntitiesOfClass(EndCrystal.class, ...)`) reads before
    /// picking the nearest. Unfiltered by distance: the caller (`dragon.rs`'s
    /// `tick_dragons`) does the nearest-of search itself, matching how
    /// `checkCrystals` folds the scan and the pick in one pass.
    #[must_use]
    pub fn end_crystals(&self) -> Vec<(i32, Vec3)> {
        self.crystals.iter().map(|(&id, c)| (id, c.position)).collect()
    }

    /// `EndCrystal.hurtServer`/`kill` — destroys the crystal and queues its
    /// explosion (entity damage/knockback via [`MobSim::explode`], and the
    /// block half via [`MobSim::pending_detonations`], the same two-call
    /// handoff `tick_tnt` uses for an identical reason). Returns the
    /// crystal's position at the moment of destruction (the blast centre),
    /// or `None` if `id` was not a live crystal.
    ///
    /// Vanilla's `hurtServer` skips the explosion entirely when the
    /// destroying `DamageSource` `is(DamageTypeTags.IS_EXPLOSION)` (so a
    /// chained blast from a neighbouring crystal does not double-explode).
    /// That distinction is not modelled here — every destruction explodes —
    /// which over-explodes a chain reaction relative to vanilla; a caller
    /// that already knows it is mid-explosion should not call this a second
    /// time for a crystal caught in its own blast radius.
    pub fn destroy_end_crystal(&mut self, id: i32) -> Option<Vec3> {
        let crystal = self.crystals.remove(&id)?;
        self.explode(crystal.position, EXPLOSION_POWER, lodestone_entity::DamageFlags::default());
        self.pending_detonations.push(Detonation {
            centre: crystal.position,
            radius: EXPLOSION_POWER,
        });
        Some(crystal.position)
    }

    /// Appends every live end crystal's [`crate::protocol::EntitySnapshot`]
    /// to `out` — the crystal half of [`MobSim::snapshots`]'s sidecar loops.
    /// Kept as its own method (rather than inlined in `snapshots`, unlike
    /// most of the other sidecar loops) because `snapshots` lives in
    /// `mobs/mod.rs` and calling out to a per-kind method here is how this
    /// file adds a wire-visible entity kind without an edit to that already
    /// very long function beyond the one call.
    pub(super) fn push_end_crystal_snapshots(&self, out: &mut Vec<crate::protocol::EntitySnapshot>) {
        let mut ids: Vec<i32> = self.crystals.keys().copied().collect();
        ids.sort_unstable();
        for id in ids {
            let Some(c) = self.crystals.get(&id) else {
                continue;
            };
            out.push(crate::protocol::EntitySnapshot {
                id,
                uuid: c.uuid,
                entity_type: end_crystal_entity_type(),
                position: c.position,
                // `EndCrystal` never rotates.
                rotation: Rotation::new(0.0, 0.0),
                head_yaw: 0.0,
                velocity: Vec3::new(0.0, 0.0, 0.0),
                // `CrystalShowBottom(true)` is real and unconditional — every
                // crystal here draws its base, since a caged crystal is never
                // spawned (no obsidian pillars anywhere, see this module's
                // doc). `CrystalBeamTarget(None)` is real on the wire but has
                // no `Some`-producing caller yet — see this module's doc for
                // exactly why.
                metadata: vec![
                    crate::protocol::MetadataField::CrystalShowBottom(true),
                    crate::protocol::MetadataField::CrystalBeamTarget(None),
                ],
                object_data: 0,
                leash_link: None,
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::ChunkWorld;

    fn sim() -> MobSim<'static> {
        let world: &'static ChunkWorld = Box::leak(Box::new(ChunkWorld::new(-64, 384)));
        MobSim::new(world)
    }

    #[test]
    fn a_spawned_crystal_is_counted_and_streamed() {
        let mut sim = sim();
        let id = sim.spawn_end_crystal(Vec3::new(0.5, 80.0, 0.5));
        assert_eq!(sim.end_crystal_count(), 1);
        assert_eq!(sim.end_crystal_position(id), Some(Vec3::new(0.5, 80.0, 0.5)));
        let snap = sim
            .snapshots()
            .into_iter()
            .find(|s| s.id == id)
            .expect("a live crystal must be streamed, or it reaches zero pixels");
        assert_eq!(snap.entity_type, end_crystal_entity_type());
    }

    #[test]
    fn destroying_a_crystal_removes_it_and_queues_the_blast() {
        let mut sim = sim();
        let id = sim.spawn_end_crystal(Vec3::new(4.0, 90.0, -4.0));
        let centre = sim.destroy_end_crystal(id);
        assert_eq!(centre, Some(Vec3::new(4.0, 90.0, -4.0)));
        assert_eq!(sim.end_crystal_count(), 0);
        let detonations = sim.take_detonations();
        assert_eq!(detonations.len(), 1);
        assert_eq!(detonations[0].radius, EXPLOSION_POWER);
        assert_eq!(detonations[0].centre, Vec3::new(4.0, 90.0, -4.0));
    }

    #[test]
    fn destroying_an_absent_crystal_is_a_harmless_none() {
        let mut sim = sim();
        assert_eq!(sim.destroy_end_crystal(999), None);
        assert!(sim.take_detonations().is_empty());
    }

    /// The gate the issue asked for: destroy a crystal through the *real*
    /// production entry point, `MobSim::attack_from_player`, not through
    /// `destroy_end_crystal` directly. Before the `self.crystals` branch
    /// existed, `attack_from_player` reached neither `self.attack` (only
    /// reads `self.mobs`) nor `destroy_end_crystal` for a crystal target id,
    /// so this call returned `None` and the crystal was still listed in
    /// `end_crystals()` afterward — the exact island the isolated
    /// `destroy_end_crystal` unit tests above could not see.
    #[test]
    fn a_crystal_is_destroyed_through_attack_from_player() {
        let mut sim = sim();
        let id = sim.spawn_end_crystal(Vec3::new(2.0, 80.0, 2.0));
        let outcome = sim.attack_from_player(
            id,
            None,
            Vec3::new(2.0, 80.0, 3.0),
            6.0,
            lodestone_entity::DamageFlags::default(),
            0.0,
        );
        assert!(outcome.is_some_and(|o| o.killed), "the crystal hit must report killed");
        assert_eq!(sim.end_crystal_count(), 0, "the crystal must actually be gone");
        assert!(sim.end_crystals().is_empty());
        assert_eq!(sim.take_detonations().len(), 1, "destroying it still queues its blast");
    }

    #[test]
    fn end_crystals_lists_every_live_one() {
        let mut sim = sim();
        let a = sim.spawn_end_crystal(Vec3::new(1.0, 80.0, 0.0));
        let b = sim.spawn_end_crystal(Vec3::new(-1.0, 80.0, 0.0));
        let mut ids: Vec<i32> = sim.end_crystals().into_iter().map(|(id, _)| id).collect();
        ids.sort_unstable();
        let mut expected = [a, b];
        expected.sort_unstable();
        assert_eq!(ids, expected);
    }
}
