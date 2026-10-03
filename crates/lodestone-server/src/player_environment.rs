//! Resident terrain sampling for the connection's pose and underwater eye.

use std::cell::Cell;

use lodestone_data::{block::Block, block_states::StateId, collision_shapes};
use lodestone_physics::{
    Aabb, CollisionView, FluidCell, FluidKind, MovementInput, PlayerState, Pose, Vec3d,
    compute_fluid_state, update_player_pose,
};

/// The pose history needed when movement itself is client-authoritative.
pub(crate) struct PlayerEnvironment {
    pose: Pose,
    swimming: bool,
}

impl Default for PlayerEnvironment {
    fn default() -> Self {
        Self { pose: Pose::Standing, swimming: false }
    }
}

impl PlayerEnvironment {
    /// Derives the current pose and samples its eye from retained terrain.
    /// An unavailable probe defers the whole sample and preserves pose history.
    pub(crate) fn eye_in_water(
        &mut self,
        position: Vec3d,
        sprinting: bool,
        sneaking: bool,
        flying: bool,
        block_state: &dyn Fn(i32, i32, i32) -> Option<StateId>,
    ) -> Option<bool> {
        let view = ResidentEnvironment { block_state, unavailable: Cell::new(false) };
        let mut player = PlayerState::at(position, 0.0).with_pose(self.pose);
        player.flying = flying;
        let previous_fluid = compute_fluid_state(
            self.pose.dimensions().bounding_box(position),
            position,
            self.pose.eye_height(),
            &view,
        );
        player.swimming = !flying
            && lodestone_physics::player::update_swimming(
                self.swimming,
                sprinting,
                &previous_fluid,
                &view,
                position,
            );
        update_player_pose(
            &mut player,
            MovementInput { sneak: sneaking, sprint: sprinting, ..MovementInput::NONE },
            &view,
            &[],
        );
        let fluid = compute_fluid_state(
            player.pose.dimensions().bounding_box(position),
            position,
            player.pose.eye_height(),
            &view,
        );
        if view.unavailable.get() {
            return None;
        }
        self.pose = player.pose;
        self.swimming = player.swimming;
        Some(fluid.eye_in_water)
    }
}

struct ResidentEnvironment<'a> {
    block_state: &'a dyn Fn(i32, i32, i32) -> Option<StateId>,
    unavailable: Cell<bool>,
}

impl ResidentEnvironment<'_> {
    fn state(&self, x: i32, y: i32, z: i32) -> StateId {
        (self.block_state)(x, y, z).unwrap_or_else(|| {
            self.unavailable.set(true);
            Block::Air.default_state()
        })
    }
}

impl CollisionView for ResidentEnvironment<'_> {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        let state = self.state(x, y, z);
        let (x, y, z) = (f64::from(x), f64::from(y), f64::from(z));
        for shape in collision_shapes::collision_boxes(state) {
            out.push(Aabb::new(
                x + f64::from(shape.min[0]),
                y + f64::from(shape.min[1]),
                z + f64::from(shape.min[2]),
                x + f64::from(shape.max[0]),
                y + f64::from(shape.max[1]),
                z + f64::from(shape.max[2]),
            ));
        }
    }

    fn fluid_at(&self, x: i32, y: i32, z: i32) -> Option<FluidCell> {
        let fluid = crate::fluid::fluid_state_of_id(self.state(x, y, z))?;
        Some(FluidCell {
            kind: match fluid.kind {
                crate::fluid::FluidKind::Water => FluidKind::Water,
                crate::fluid::FluidKind::Lava => FluidKind::Lava,
            },
            amount: fluid.amount,
            falling: fluid.falling,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vitals::{MAX_HEALTH, PlayerVitals};

    fn pool(_x: i32, y: i32, _z: i32) -> Option<StateId> {
        Some(if (0..=1).contains(&y) {
            Block::Water.default_state()
        } else {
            Block::Air.default_state()
        })
    }

    fn covered_pool(x: i32, y: i32, z: i32) -> Option<StateId> {
        if y == 2 { Some(Block::Stone.default_state()) } else { pool(x, y, z) }
    }

    #[test]
    fn swimming_up_to_an_underwater_ceiling_keeps_depleting_air() {
        let mut environment = PlayerEnvironment::default();
        let mut vitals = PlayerVitals::restored(MAX_HEALTH, 83);
        // The source surface is at 1 + 8/9; the swimming eye at feet 1.4
        // is 1.8, while a standing-eye probe would read air above the roof.
        for (feet_y, expected_air) in [(0.2, 82), (0.8, 81), (1.39, 80), (1.4, 79)] {
            let submerged = environment
                .eye_in_water(Vec3d::new(0.5, feet_y, 0.5), true, false, false, &covered_pool)
                .expect("resident pool");
            assert!(submerged, "submerged eye at feet y={feet_y}");
            assert_eq!(environment.pose, Pose::Swimming);
            assert_eq!(vitals.tick(submerged).air_changed, Some(expected_air));
        }
        assert_eq!(covered_pool(0, 3, 0), Some(Block::Air.default_state()));
    }

    #[test]
    fn crawling_underwater_uses_the_fitted_pose_even_without_sprinting() {
        let mut environment = PlayerEnvironment::default();
        let submerged = environment
            .eye_in_water(Vec3d::new(0.5, 1.4, 0.5), false, false, false, &covered_pool)
            .expect("resident pool");
        assert_eq!(environment.pose, Pose::Swimming);
        assert!(!environment.swimming);
        let mut vitals = PlayerVitals::restored(MAX_HEALTH, 83);
        assert_eq!(vitals.tick(submerged).air_changed, Some(82));
    }

    #[test]
    fn standing_and_crouching_straddle_the_same_water_surface() {
        for (sneaking, expected_pose, expected_air) in [
            (false, Pose::Standing, 87),
            (true, Pose::Crouching, 82),
        ] {
            let mut environment = PlayerEnvironment::default();
            let submerged = environment
                .eye_in_water(Vec3d::new(0.5, 0.3, 0.5), false, sneaking, false, &pool)
                .expect("resident pool");
            assert_eq!(environment.pose, expected_pose);
            let mut vitals = PlayerVitals::restored(MAX_HEALTH, 83);
            assert_eq!(vitals.tick(submerged).air_changed, Some(expected_air));
        }
    }

    #[test]
    fn a_swimmer_refills_air_when_the_eye_really_surfaces() {
        let mut environment = PlayerEnvironment::default();
        assert_eq!(
            environment.eye_in_water(Vec3d::new(0.5, 0.2, 0.5), true, false, false, &pool),
            Some(true),
        );
        let submerged = environment
            .eye_in_water(Vec3d::new(0.5, 1.6, 0.5), true, false, false, &pool)
            .expect("resident pool");
        assert!(environment.swimming, "sprint-swimming persists while the box touches water");
        assert_eq!(environment.pose, Pose::Swimming);
        let mut vitals = PlayerVitals::restored(MAX_HEALTH, 83);
        assert_eq!(vitals.tick(submerged).air_changed, Some(87));
    }

    #[test]
    fn flying_clears_the_swim_flag_and_restores_the_upright_eye() {
        let mut environment = PlayerEnvironment::default();
        assert_eq!(
            environment.eye_in_water(Vec3d::new(0.5, 0.2, 0.5), true, false, false, &pool),
            Some(true),
        );
        assert_eq!(
            environment.eye_in_water(Vec3d::new(0.5, 0.3, 0.5), true, true, true, &pool),
            Some(false),
        );
        assert!(!environment.swimming);
        assert_eq!(environment.pose, Pose::Standing);
    }

    #[test]
    fn an_unavailable_resident_probe_preserves_pose_history() {
        let mut environment = PlayerEnvironment::default();
        assert_eq!(
            environment.eye_in_water(Vec3d::new(0.5, 0.2, 0.5), true, false, false, &pool),
            Some(true),
        );
        let missing = |x, y, z| if y == 2 { None } else { pool(x, y, z) };
        assert_eq!(
            environment.eye_in_water(Vec3d::new(0.5, 0.3, 0.5), false, false, false, &missing),
            None,
        );
        assert!(environment.swimming);
        assert_eq!(environment.pose, Pose::Swimming);
    }
}
