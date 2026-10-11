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

    /// A player boards the mob `id` by interaction; refused when it is not a
    /// live mob or already has a rider. Species eligibility (tamed, saddled, not
    /// sneaking) is the interact arm's job; this is the universal occupancy rule.
    pub fn mount_mob(&mut self, id: i32, player_entity_id: i32) -> bool {
        self.get(id).is_some() && self.board(id, player_entity_id, true, false)
    }

    /// Vanilla's own camel rider-jump handler/rider-jump executor — the rider-triggered half
    /// of camel dash. Called from `crate::server`'s `ServerBound::PlayerInput`
    /// consumer on every received `jump: true` (see that call site's own
    /// comment for why a received packet already is the rising edge).
    ///
    /// Refuses when `player_entity_id` rides no mob, the mob it rides is
    /// not a camel, or `SimMob::camel_dash_cooldown` has not yet reached
    /// zero (vanilla's rider-jump handler's cooldown-at-or-below-zero
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
        let Some(id) = self.vehicle_of(player_entity_id).filter(|&id| self.get(id).is_some()) else {
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
        let Some(id) = self
            .vehicle_of(player_entity_id)
            .filter(|&id| self.get(id).is_some() && self.controlling_player(id) == Some(player_entity_id))
        else {
            return false;
        };
        let Some(mob) = self.get_mut(id) else {
            return false;
        };
        mob.mob.set_position(position).set_body_yaw(yaw);
        true
    }

    /// The position of the mob with `id`, if present.
    #[must_use]
    pub fn position(&self, id: i32) -> Option<Vec3> {
        self.get(id).map(SimMob::position)
    }

    /// Whether a mob that keeps a player from resting stands within
    /// `horizontal` blocks of `centre` on x and z and `vertical` on y: any
    /// monster, except the zombified piglin, which only counts while angry at
    /// the player (anger is not queried here, so it never counts).
    #[must_use]
    pub fn monster_prevents_rest(&self, centre: Vec3, horizontal: f64, vertical: f64) -> bool {
        self.mobs.iter().any(|m| {
            let at = m.position();
            m.category == MobCategory::Monster
                && m.entity_type.path() != "zombified_piglin"
                && (at.x - centre.x).abs() <= horizontal
                && (at.z - centre.z).abs() <= horizontal
                && (at.y - centre.y).abs() <= vertical
        })
    }

    /// Iterates the live mobs.
    pub fn iter(&self) -> impl Iterator<Item = &SimMob<'w>> {
        self.mobs.iter()
    }
}
