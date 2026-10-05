//! Per-connection play-state helpers: teleport acknowledgement tracking and the client movement record.

use super::*;

/// Administrative serverbound actions use permission level `2`, matching the
/// built-in `/gamemode`, `/gamerule`, and `/difficulty` command gates. This
/// constant covers the dedicated packets that perform the same actions without
/// going through a slash command.
pub(super) const COMMANDS_GAMEMASTER_LEVEL: u8 = 2;

/// The one outstanding player-position correction for an acknowledgement-aware
/// connection. A newer correction supersedes an older one, so an overdue reply
/// cannot reopen movement after the server has already moved the player again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct TeleportAcknowledgements {
    pub(super) next_id: i32,
    pub(super) pending_id: Option<i32>,
}

impl TeleportAcknowledgements {
    pub(super) fn after_initial(initial_id: i32) -> Self {
        Self {
            next_id: initial_id.wrapping_add(1),
            pending_id: Some(initial_id),
        }
    }

    pub(super) fn issue(&mut self) -> i32 {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        self.pending_id = Some(id);
        id
    }

    pub(super) fn accepts(&mut self, id: i32) -> bool {
        if self.pending_id == Some(id) {
            self.pending_id = None;
            true
        } else {
            false
        }
    }

    pub(super) fn is_pending(&self) -> bool {
        self.pending_id.is_some()
    }
}

pub(super) fn issue_teleport_id(teleports: &mut Option<TeleportAcknowledgements>) -> i32 {
    teleports.as_mut().map_or(0, TeleportAcknowledgements::issue)
}

/// The movement sample a client has reported for its current local tick.
///
/// Position packets carry a delta only indirectly: the server derives it from
/// two absolute positions. The empty tick-end marker is the delimiter that
/// tells us when an absent position packet means zero movement rather than
/// "keep the previous sample". This is per connection because another
/// player's movement cannot affect this player's projectile launch.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(super) struct ClientMovement {
    pub(super) delta: Vec3,
    pub(super) on_ground: bool,
    pub(super) received_this_tick: bool,
}

impl Default for ClientMovement {
    fn default() -> Self {
        Self {
            delta: Vec3::new(0.0, 0.0, 0.0),
            on_ground: true,
            received_this_tick: false,
        }
    }
}

impl ClientMovement {
    /// Records the latest player-position sample in this client tick.
    pub(super) fn observe(&mut self, delta: Vec3, on_ground: bool) {
        self.delta = delta;
        self.on_ground = on_ground;
        self.received_this_tick = true;
    }

    /// Ends the client's local tick, zeroing only a tick with no movement.
    pub(super) fn finish_tick(&mut self) {
        if !self.received_this_tick {
            self.delta = Vec3::new(0.0, 0.0, 0.0);
        }
        self.received_this_tick = false;
    }

    /// Adds the source's latest movement to a launched projectile.
    ///
    /// Grounded sources contribute horizontal velocity only. This is the
    /// launch rule the protocol's movement boundary protects: a following
    /// idle tick must not leave a projectile with stale horizontal momentum.
    pub(super) fn add_to_launch(self, velocity: Vec3) -> Vec3 {
        Vec3::new(
            velocity.x + self.delta.x,
            velocity.y + if self.on_ground { 0.0 } else { self.delta.y },
            velocity.z + self.delta.z,
        )
    }
}

#[cfg(test)]
mod tests;
