//! Lowering the player pose to the outbound movement action.
//!
//! This is the single most important thing to share: it is the one place the
//! controller touches the wire. If the browser lowered `PlayerState` to a
//! [`ClientAction`] by its own route, the movement fork would survive every
//! other refactor in exactly the spot that determines what the server sees — so
//! both native and browser call *this* function.
//!
//! Kept pure: the only interesting part is the coordinate mapping (physics
//! `Vec3d`/yaw/pitch → version-free model [`Vec3`]/[`Rotation`]), so it is
//! unit-testable without a server or a physics tick. The version adapter lowers
//! the resulting [`ClientAction`] into whatever concrete packet the negotiated
//! protocol wants; the controller never names one.

use lodestone_client::{ClientAction, Rotation, Vec3};
use lodestone_physics::PlayerState;

/// Map the current player pose to the outbound movement action.
#[must_use]
pub fn move_action(player: &PlayerState) -> ClientAction {
    ClientAction::Move {
        pos: Vec3::new(player.position.x, player.position.y, player.position.z),
        rotation: Rotation::new(player.yaw, player.pitch),
        on_ground: player.on_ground,
        horizontal_collision: player.horizontal_collision,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_physics::{PlayerState, Vec3d};

    #[test]
    fn move_action_maps_player_pose_to_model_types() {
        let mut player = PlayerState::at(Vec3d::new(1.5, 64.0, -2.5), 90.0);
        player.pitch = -10.0;
        player.on_ground = true;
        player.horizontal_collision = true;
        assert_eq!(
            move_action(&player),
            ClientAction::Move {
                pos: Vec3::new(1.5, 64.0, -2.5),
                rotation: Rotation::new(90.0, -10.0),
                on_ground: true,
                horizontal_collision: true,
            }
        );
    }
}
