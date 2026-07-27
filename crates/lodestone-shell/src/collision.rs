//! Adapts a generated [`lodestone_world::World`] to the physics engine's
//! [`CollisionView`], so the shell drives the **real** bit-exact movement code
//! in `lodestone-physics` rather than any ad-hoc integrator.
//!
//! The mapping is deliberately coarse but honest: every solid block is a unit
//! cube, water reports `is_water` (so the engine runs its swim path), and
//! everything else is empty. That is all the demo world contains.

use lodestone_physics::{Aabb, CollisionView};
use lodestone_world::{ChunkPos, World};

use crate::blocks::id;

/// A [`CollisionView`] over a borrowed [`World`].
#[derive(Debug)]
pub struct WorldCollision<'a> {
    world: &'a World,
}

impl<'a> WorldCollision<'a> {
    /// Wrap a world.
    #[must_use]
    pub fn new(world: &'a World) -> Self {
        Self { world }
    }

    /// Block-state id at world coordinates, or [`id::AIR`] outside loaded chunks.
    #[must_use]
    pub fn block_at(&self, x: i32, y: i32, z: i32) -> u32 {
        let pos = ChunkPos {
            x: x.div_euclid(16),
            z: z.div_euclid(16),
        };
        let Some(chunk) = self.world.get(pos) else {
            return id::AIR;
        };
        let col = &chunk.column;
        if y < col.min_y() || y >= col.max_y() {
            return id::AIR;
        }
        col.get_block(x.rem_euclid(16) as usize, y, z.rem_euclid(16) as usize)
    }

    /// Whether the block at these coordinates is a full-cube collider.
    #[must_use]
    pub fn is_solid(&self, x: i32, y: i32, z: i32) -> bool {
        let b = self.block_at(x, y, z);
        b != id::AIR && b != id::WATER
    }
}

impl CollisionView for WorldCollision<'_> {
    fn collision_boxes(&self, x: i32, y: i32, z: i32, out: &mut Vec<Aabb>) {
        if self.is_solid(x, y, z) {
            out.push(Aabb::new(
                f64::from(x),
                f64::from(y),
                f64::from(z),
                f64::from(x) + 1.0,
                f64::from(y) + 1.0,
                f64::from(z) + 1.0,
            ));
        }
    }

    fn is_water(&self, x: i32, y: i32, z: i32) -> bool {
        self.block_at(x, y, z) == id::WATER
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lodestone_physics::{MovementInput, PhysicsProfile, PlayerState, Vec3d, tick};

    #[test]
    fn solid_ground_reports_a_box() {
        let world = crate::worldgen::generate(0);
        let view = WorldCollision::new(&world);
        let s = crate::worldgen::surface_height(0, 0);
        let mut boxes = Vec::new();
        view.collision_boxes(0, s, 0, &mut boxes);
        assert_eq!(boxes.len(), 1, "surface block is a collider");
        assert!(boxes[0].max_y - boxes[0].min_y == 1.0);
    }

    #[test]
    fn air_above_surface_is_empty() {
        let world = crate::worldgen::generate(0);
        let view = WorldCollision::new(&world);
        let s = crate::worldgen::surface_height(0, 0);
        let mut boxes = Vec::new();
        view.collision_boxes(0, s + 5, 0, &mut boxes);
        assert!(boxes.is_empty());
    }

    #[test]
    fn player_falls_and_lands_on_the_floor() {
        let world = crate::worldgen::generate(0);
        let view = WorldCollision::new(&world);
        let profile = PhysicsProfile::mc_1_21();
        let s = crate::worldgen::surface_height(0, 0);

        // Drop the player 6 blocks above the surface, feet-centred.
        let start = Vec3d::new(0.5, f64::from(s) + 7.0, 0.5);
        let mut state = PlayerState::at(start, 0.0);
        let input = MovementInput {
            forward: 0.0,
            strafe: 0.0,
            jump: false,
            sneak: false,
            sprint: false,
        };

        for _ in 0..80 {
            tick(&mut state, input, &view, &profile);
        }

        assert!(state.on_ground, "player should have landed");
        // Feet should rest on top of the surface block (y = s + 1).
        let expected = f64::from(s) + 1.0;
        assert!(
            (state.position.y - expected).abs() < 0.05,
            "feet at {} expected ~{expected}",
            state.position.y
        );
    }
}
