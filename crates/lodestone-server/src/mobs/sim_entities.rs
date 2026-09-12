//! Entity lookup, riding, and position access for [`super::MobSim`].

use super::*;

impl<'w> MobSim<'w> {
    pub fn len(&self) -> usize {
        self.mobs.len()
    }

    /// Whether the simulation has no mobs.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.mobs.is_empty()
    }

    /// A mob by id, if present.
    #[must_use]
    pub fn get(&self, id: i32) -> Option<&SimMob<'w>> {
        self.mobs.iter().find(|m| m.id == id)
    }

    /// A mob by id, mutably, if present.
    pub fn get_mut(&mut self, id: i32) -> Option<&mut SimMob<'w>> {
        self.mobs.iter_mut().find(|m| m.id == id)
    }

    /// The **player entity id** riding `id`, if `id` names a mounted mob.
    ///
    /// The mob-mounted-by-player half of the passenger model — the twin of
    /// [`vehicle_rider`](Self::vehicle_rider) (boats) and the minecart
    /// equivalent in [`mobs::minecart`](crate::mobs::minecart), for a mount
    /// that keeps its own `SimMob` identity and goal AI rather than living in
    /// a separate AI-less map.
    #[must_use]
    pub fn mob_rider(&self, id: i32) -> Option<i32> {
        self.get(id).and_then(|m| m.rider)
    }

    /// The mob `player_entity_id` is riding, if any.
    #[must_use]
    pub fn mob_ridden_by(&self, player_entity_id: i32) -> Option<i32> {
        self.mobs
            .iter()
            .find(|m| m.rider == Some(player_entity_id))
            .map(|m| m.id)
    }

    /// Vanilla's own generic "start riding" call for a mounted mob — its own
    /// horse-family mount interaction's
    /// occupancy half. Mirrors [`mount_vehicle`](Self::mount_vehicle)'s shape
    /// exactly; the difference is what it operates on, a full [`SimMob`]
    /// rather than an AI-less [`TrackedVehicle`].
    ///
    /// Species-specific eligibility — tamed, not a baby, no sneak-click, a
    /// saddle if the species requires one — is deliberately **not** checked
    /// here, the same division `mount_vehicle` draws with
    /// `using_secondary_action`: those are `interact_horse`'s (or a future
    /// per-species interact arm's) job, because they differ by species and
    /// this method's only responsibility is the universal occupancy rule.
    ///
    /// Refuses when `id` is not a live mob or already carries a *different*
    /// rider. A player already riding something else — mob or vehicle — is
    /// left untouched here: unlike vehicles, this crate has exactly one
    /// producer of mob mounts today ([`MobSim::interact`]'s horse-family arm),
    /// so cross-kind dismount-first is the caller's job until a second
    /// producer exists, matching this method's own "one map's worry" scope.
    ///
    /// Returns `true` when the player is now aboard — the caller's cue to
    /// send `SET_PASSENGERS`.
    pub fn mount_mob(&mut self, id: i32, player_entity_id: i32) -> bool {
        let Some(mob) = self.get(id) else {
            return false;
        };
        if mob.rider.is_some_and(|rider| rider != player_entity_id) {
            return false;
        }
        if let Some(previous) = self.mob_ridden_by(player_entity_id) {
            if previous != id {
                if let Some(old) = self.get_mut(previous) {
                    old.rider = None;
                }
            }
        }
        if let Some(mob) = self.get_mut(id) {
            mob.rider = Some(player_entity_id);
        }
        true
    }

    /// Vanilla's own generic "stop riding" call for whatever mob `player_entity_id` is aboard,
    /// returning the mob it left. Called on an explicit dismount as well as
    /// on disconnect: a mount whose rider vanished must resume its own goal
    /// AI ([`tick`](Self::tick) skips a ridden mob's goal tick entirely — see
    /// that skip's own comment), or it stands frozen forever exactly as an
    /// unhealed boat would.
    pub fn dismount_mob(&mut self, player_entity_id: i32) -> Option<i32> {
        let id = self.mob_ridden_by(player_entity_id)?;
        if let Some(mob) = self.get_mut(id) {
            mob.rider = None;
        }
        Some(id)
    }

    /// Vanilla's own camel rider-jump handler/rider-jump executor — the rider-triggered half
    /// of camel dash. Called from `crate::server`'s `ServerBound::PlayerInput`
    /// consumer on every received `jump: true` (see that call site's own
    /// comment for why a received packet already is the rising edge).
    ///
    /// Refuses when `player_entity_id` rides no mob, the mob it rides is
    /// not a camel, or `SimMob::camel_dash_cooldown` has not yet reached
    /// zero (vanilla's own rider-jump handler's own cooldown-at-or-below-zero
    /// gate). Two of
    /// vanilla's three gates are not checked at all — see
    /// `ServerBound::PlayerInput`'s consumer for why (no saddle-equip
    /// model, no `onGround` for a client-authoritative mount).
    ///
    /// Sets `camel_dash_cooldown` to [`CAMEL_DASH_COOLDOWN_TICKS`], which
    /// both gates the next dash and — through
    /// [`SimMob::camel_is_dashing`] — makes the next [`snapshots`](Self::snapshots)
    /// diff carry the dash flag as `true` to every other connected viewer. The
    /// actual position impulse (vanilla's own rider-jump executor's velocity
    /// add) is not
    /// applied here: this crate has no server-side ridden-mob physics at
    /// all (`lodestone_physics::vehicle`'s module doc — a mounted camel is
    /// exactly as client-authoritative as a horse or a boat), so the visible
    /// leap itself is the rider's own client's job, not this seam's.
    ///
    /// Returns whether a dash actually started, so a caller that wants to
    /// know (a future sound/particle producer) can tell a real trigger from
    /// a no-op jump press.
    pub fn trigger_camel_dash(&mut self, player_entity_id: i32) -> bool {
        let Some(id) = self.mob_ridden_by(player_entity_id) else {
            return false;
        };
        let Some(mob) = self.get_mut(id) else {
            return false;
        };
        if mob.entity_type.path() != "camel" || mob.camel_dash_cooldown > 0 {
            return false;
        }
        mob.camel_dash_cooldown = CAMEL_DASH_COOLDOWN_TICKS;
        true
    }

    /// Accepts a client-authoritative move for the mob `player_entity_id` is
    /// riding — the mob-mount twin of
    /// [`apply_vehicle_move`](Self::apply_vehicle_move), and intended for the
    /// same `VehicleMoved` wire packet: vanilla's client is authoritative over
    /// its own ridden entity's position regardless of whether that entity is
    /// a boat or a horse (vanilla's own player-specific "is client
    /// authoritative" override does not
    /// distinguish them), so the two share one packet and should share one
    /// dispatch, trying this after (or instead of)
    /// [`apply_vehicle_move`](Self::apply_vehicle_move) depending on which one
    /// the rider is actually aboard.
    ///
    /// Returns `false` if the player rides no mob (including "rides a
    /// vehicle instead", which is not this method's map to touch).
    pub fn apply_mob_move(&mut self, player_entity_id: i32, position: Vec3, yaw: f32) -> bool {
        let Some(id) = self.mob_ridden_by(player_entity_id) else {
            return false;
        };
        let Some(mob) = self.get_mut(id) else {
            return false;
        };
        mob.mob.set_position(position).set_body_yaw(yaw);
        true
    }

    /// The world this sim's mobs path over. Exposed so a caller holding only
    /// a `&mut MobSim` (e.g. [`MobHandle::with`]) can still reach terrain —
    /// see [`seed_demo_mobs`]'s use of this to resolve spawn-surface Y
    /// without a second, separately-threaded `&ChunkWorld` parameter.
    #[must_use]
    pub(crate) fn world(&self) -> &'w ChunkWorld {
        self.world
    }

    /// The position of the mob with `id`, if present.
    #[must_use]
    pub fn position(&self, id: i32) -> Option<Vec3> {
        self.get(id).map(SimMob::position)
    }

    /// Iterates the live mobs.
    pub fn iter(&self) -> impl Iterator<Item = &SimMob<'w>> {
        self.mobs.iter()
    }
}
